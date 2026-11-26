use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread,
    time::Instant,
};

pub(crate) struct Request {
    pub line: String,
    headers: Vec<String>,
    body: String,
}

pub(crate) fn response(status: u16, body: Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} GitHub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn mock(responses: Vec<String>, run: impl FnOnce(Client)) -> Vec<Request> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let client = Client::for_test(&format!("http://{}", listener.local_addr().unwrap()));
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        responses
            .into_iter()
            .map(|response| {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "expected an HTTP request");
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut headers = Vec::new();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    assert_ne!(reader.read_line(&mut header).unwrap(), 0);
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = header.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push(header.trim().to_owned());
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if !response.is_empty() {
                    stream.write_all(response.as_bytes()).unwrap();
                }
                Request {
                    line: line.trim().into(),
                    headers,
                    body: String::from_utf8(body).unwrap(),
                }
            })
            .collect()
    });
    run(client);
    handle.join().unwrap()
}

#[test]
fn device_flow_and_rotation_send_credentials_only_in_request_bodies() {
    let code = json!({"device_code":"device-fixture","user_code":"TEST-ONLY","verification_uri":SIGN_IN_URL,"expires_in":900,"interval":5});
    let issued = json!({"access_token":"test-access", "token_type":"bearer","expires_in":28800,"refresh_token":"test-refresh"});
    let rotated = json!({"access_token":"new-access", "token_type":"bearer","expires_in":28800,"refresh_token":"new-refresh"});
    let requests = mock(
        vec![
            response(200, code),
            response(200, json!({"error":"authorization_pending"})),
            response(200, json!({"error":"slow_down"})),
            response(200, issued),
            response(200, rotated),
            response(400, json!({"error":"access_denied"})),
            response(400, json!({"error":"bad_refresh_token"})),
        ],
        |client| {
            let device = client.start_device().unwrap();
            assert_eq!(
                (device.user_code.as_str(), device.interval),
                ("TEST-ONLY", 5)
            );
            assert!(matches!(
                client.poll_device(&device).unwrap(),
                Poll::Pending
            ));
            assert!(matches!(
                client.poll_device(&device).unwrap(),
                Poll::SlowDown
            ));
            let Poll::Ready(tokens) = client.poll_device(&device).unwrap() else {
                panic!("expected tokens")
            };
            assert_eq!(tokens.access_token, "test-access");
            assert!(tokens.expires_at.is_some());
            let new = client
                .refresh(tokens.refresh_token.as_ref().unwrap())
                .unwrap();
            assert_eq!(new.refresh_token.as_deref(), Some("new-refresh"));
            assert!(matches!(client.poll_device(&device), Err(Error::Denied(_))));
            assert!(matches!(
                client.refresh("expired"),
                Err(Error::Unauthorized(_))
            ));
        },
    );
    assert_eq!(requests[0].line, "POST /login/device/code HTTP/1.1");
    assert_eq!(
        requests[0].body,
        "client_id=test-client&scope=offline_access"
    );
    for r in &requests[1..] {
        assert_eq!(r.line, "POST /login/oauth/access_token HTTP/1.1");
        assert!(!r.body.contains("client_secret"));
    }
    assert!(requests[1].body.contains("device_code=device-fixture"));
    assert!(requests[4].body.contains("refresh_token=test-refresh"));
}

#[test]
fn owners_include_only_eligible_installations_across_pages() {
    let installation = |id: u64, login: &str, kind: &str, create: &str| {
        json!({
            "account":{"type":kind,"login":login,"id":id},
            "permissions":{"repository_creation":create,"contents":"write"},
            "suspended_at":null,
        })
    };
    let first = response(
        200,
        json!({"installations":[
            installation(2,"First-Org","Organization","write"),
            installation(3,"Other","User","write"),
            installation(4,"Denied","Organization","read"),
        ]}),
    )
    .replace(
        "Content-Type:",
        "Link: <https://example.invalid/wrong>; rel=\"next\"\r\nContent-Type:",
    );
    let mut suspended = installation(5, "Suspended", "Organization", "write");
    suspended["suspended_at"] = json!("2026-01-01");
    let records = mock(
        vec![
            response(200, json!({"id":1,"login":"Me"})),
            first,
            response(
                200,
                json!({"installations":[suspended,installation(1,"Me","User","write")]}),
            ),
        ],
        |client| {
            let account = client.account("auth-fixture").unwrap();
            assert_eq!((account.id, account.login.as_str()), (1, "Me"));
            assert_eq!(
                account
                    .owners
                    .iter()
                    .map(|o| (o.login.as_str(), o.personal))
                    .collect::<Vec<_>>(),
                vec![("Me", true), ("First-Org", false)]
            );
        },
    );
    assert_eq!(
        records[2].line,
        "GET /user/installations?per_page=100&page=2 HTTP/1.1"
    );
    for record in &records {
        assert!(
            record
                .headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case("authorization: Bearer auth-fixture"))
        );
        assert!(
            record
                .headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case("x-github-api-version: 2026-03-10"))
        );
    }
}

fn personal() -> Owner {
    Owner {
        id: 11,
        login: "My-Name".into(),
        personal: true,
    }
}
fn repo(owner: &Owner, private: bool) -> Value {
    let name = format!("{}/.github", owner.login);
    json!({"id":42,"owner":{"id":owner.id},"full_name":name,"private":private,"visibility":if private {"private"}else{"public"},"clone_url":format!("https://github.com/{name}.git")})
}

#[test]
fn creates_explicitly_empty_personal_or_organisation_repository_and_can_lookup() {
    let me = personal();
    let org = Owner {
        id: 13,
        login: "Company".into(),
        personal: false,
    };
    let mut internal_repo = repo(&org, true);
    internal_repo["visibility"] = json!("internal");
    let result = mock(
        vec![
            response(201, repo(&me, true)),
            response(201, repo(&org, false)),
            response(200, repo(&org, false)),
            response(404, json!({})),
            response(200, internal_repo),
        ],
        |client| {
            assert_eq!(client.create("token", &me, ".github", true).unwrap().id, 42);
            let created = client.create("token", &org, ".github", false).unwrap();
            assert_eq!(
                Some(created),
                client.lookup("token", "company", ".github").unwrap()
            );
            assert!(client.lookup("token", "other", "unseen").unwrap().is_none());
            assert!(client.lookup("token", "Company", ".github").is_err());
        },
    );
    assert_eq!(result[0].line, "POST /user/repos HTTP/1.1");
    assert_eq!(result[1].line, "POST /orgs/Company/repos HTTP/1.1");
    for (record, private) in result[..2].iter().zip([true, false]) {
        assert_eq!(
            serde_json::from_str::<Value>(&record.body).unwrap(),
            json!({"name":".github","private":private,"auto_init":false})
        );
    }
    assert_eq!(result[2].line, "GET /repos/company/.github HTTP/1.1");
}

#[test]
fn preserves_denials_collisions_and_uncertain_creation_as_distinct_outcomes() {
    let owner = personal();
    let mut wrong_url = repo(&owner, true);
    wrong_url["clone_url"] = json!("https://example.invalid/My-Name/.github.git");
    let mut wrong_id = repo(&owner, true);
    wrong_id["owner"]["id"] = json!(888);
    mock(
        vec![
            response(403, json!({})),
            response(
                422,
                json!({"errors":[{"field":"name","code":"custom","message":"name already exists on this account"}]}),
            ),
            response(422, json!({"errors":[{"field":"name","code":"invalid"}]})),
            response(201, wrong_url),
            response(201, wrong_id),
            response(201, repo(&owner, false)),
            response(502, Value::Null),
            String::new(),
        ],
        |client| {
            let create = || client.create("token", &owner, ".github", true);
            assert!(matches!(create(), Err(Error::Denied(_))));
            assert!(matches!(create(), Err(Error::Collision(_))));
            assert!(matches!(create(), Err(Error::Other(_))));
            for _ in 0..5 {
                assert!(matches!(create(), Err(Error::Uncertain(_))));
            }
        },
    );
}

#[test]
fn validates_destinations_and_browser_urls_before_using_them() {
    for name in [".github", "repo.01", "fast_kraken-2"] {
        assert!(validate_repo_name(name).is_ok());
    }
    for name in ["", ".", "..", "a/b", "a?b", "hello\nworld", "a b"] {
        assert!(validate_repo_name(name).is_err());
    }
    assert!(Client::new("test-client", "good/other").is_err());
    assert!(
        Client::new("test-client", "good")
            .unwrap()
            .installation_url()
            .ends_with("/apps/good/installations/new")
    );
    mock(
        vec![response(
            200,
            json!({"verification_uri":"https://example.invalid/login/device"}),
        )],
        |client| {
            assert!(matches!(client.start_device(), Err(Error::Other(_))));
        },
    );
}

#[test]
fn an_installation_counts_only_with_write_access_to_contents_on_that_owner() {
    let installation = |login: &str, contents: &str, suspended: bool| {
        json!({
            "account":{"type":"Organization","login":login,"id":9},
            "permissions":{"contents":contents},
            "suspended_at":if suspended { json!("2026-01-01") } else { Value::Null },
        })
    };
    let listing = response(
        200,
        json!({"installations":[
            installation("Readers","read",false),
            installation("Paused","write",true),
            installation("trailteam","write",false),
        ]}),
    );
    mock(vec![listing.clone(), listing.clone(), listing.clone(), listing], |client| {
        assert!(client.installed_on("auth-fixture", "TrailTeam").unwrap());
        assert!(!client.installed_on("auth-fixture", "Readers").unwrap());
        assert!(!client.installed_on("auth-fixture", "Paused").unwrap());
        assert!(!client.installed_on("auth-fixture", "Elsewhere").unwrap());
    });
}

#[test]
fn tests_never_reach_github_itself() {
    let real = Client::new("test-client", "test-app").unwrap();
    for error in [
        real.user("token").err().unwrap(),
        real.start_device().err().unwrap(),
        real.lookup("token", "TrailTeam", "ledger").err().unwrap(),
    ] {
        assert_eq!(error.to_string(), "Tests and replays never contact GitHub.");
    }
}

#[test]
fn the_built_in_registration_is_compiled_in() {
    let id = option_env!("FASTKRAKEN_GITHUB_CLIENT_ID").unwrap_or_default();
    let slug = option_env!("FASTKRAKEN_GITHUB_APP_SLUG").unwrap_or_default();
    let client = Client::new(id, slug).unwrap();
    assert!(client.installation_url().starts_with("https://github.com/apps/"));
}

#[test]
fn pull_requests_are_read_once_and_then_only_when_their_etag_changed() {
    let sha = "b".repeat(40);
    let pull = |number: u64, state: &str, draft: bool, merged: bool| {
        json!({
            "number": number, "title": format!("Walk\u{7}the ridge {number}"), "state": state, "draft": draft,
            "merged_at": if merged { json!("2026-11-01T00:00:00Z") } else { Value::Null },
            "user": {"login": "hiker"}, "html_url": "https://evil.example.invalid/",
            "head": {"ref": "ridge", "sha": sha, "repo": {"full_name": "hiker/trail"}}, "base": {"ref": "trunk"},
        })
    };
    let tagged = response(200, json!([pull(12, "open", false, false), pull(14, "open", true, false), {"number": 3}]))
        .replace("Content-Type:", "ETag: \"open-1\"\r\nContent-Type:");
    let closed = response(200, json!([pull(9, "closed", false, true), pull(8, "closed", false, false)]));
    let records = mock(
        vec![tagged, response(304, Value::Null), closed, response(403, json!({"message":"Resource not accessible by integration"})), response(403, json!({"message":"API rate limit exceeded"}))],
        |client| {
            let Cached::Fresh { value, etag } = client.pull_requests("auth-fixture", "cedar", "trail", true, None).unwrap() else {
                panic!("expected a fresh list")
            };
            assert_eq!(etag.as_deref(), Some("\"open-1\""));
            assert_eq!(value.len(), 2);
            assert_eq!((value[0].number, value[0].state, value[1].state), (12, PullState::Open, PullState::Draft));
            assert_eq!(value[0].title, "Walkthe ridge 12");
            assert_eq!(value[0].page, "https://github.com/cedar/trail/pull/12");
            assert_eq!((value[0].head.as_str(), value[0].head_repo.as_deref(), value[0].base.as_str()), ("ridge", Some("hiker/trail"), "trunk"));
            assert_eq!(client.pull_requests("auth-fixture", "cedar", "trail", true, etag.as_deref()).unwrap(), Cached::Unchanged);
            let Cached::Fresh { value, .. } = client.pull_requests("auth-fixture", "cedar", "trail", false, None).unwrap() else {
                panic!("expected closed pull requests")
            };
            assert_eq!(value.iter().map(|pull| pull.state).collect::<Vec<_>>(), [PullState::Merged, PullState::Closed]);
            assert!(matches!(client.pull_requests("auth-fixture", "cedar", "trail", true, None), Err(Error::Denied(_))));
            assert!(matches!(client.pull_requests("auth-fixture", "cedar", "trail", true, None), Err(Error::Other(said)) if said.contains("rate-limited")));
        },
    );
    assert_eq!(records[0].line, "GET /repos/cedar/trail/pulls?state=open&per_page=100 HTTP/1.1");
    assert!(!records[0].headers.iter().any(|h| h.to_ascii_lowercase().starts_with("if-none-match")));
    assert!(records[1].headers.iter().any(|h| h == "if-none-match: \"open-1\"" || h == "If-None-Match: \"open-1\""));
    assert_eq!(records[2].line, "GET /repos/cedar/trail/pulls?state=closed&sort=updated&direction=desc&per_page=50 HTTP/1.1");
}

#[test]
fn check_runs_are_read_for_one_full_commit() {
    let sha = "c".repeat(40);
    let runs = json!({"total_count": 4, "check_runs": [
        {"name": "build", "status": "completed", "conclusion": "success"},
        {"name": "lint", "status": "completed", "conclusion": "failure"},
        {"name": "docs", "status": "completed", "conclusion": "skipped"},
        {"name": "test", "status": "in_progress", "conclusion": null},
    ]});
    let records = mock(vec![response(200, runs), response(404, json!({}))], |client| {
        let Cached::Fresh { value, .. } = client.check_runs("auth-fixture", "cedar", "trail", &sha, None).unwrap() else {
            panic!("expected check runs")
        };
        let states: Vec<_> = value.iter().map(|run| (run.name.as_str(), run.state)).collect();
        assert_eq!(states, [("build", RunState::Passed), ("lint", RunState::Failed), ("docs", RunState::Skipped), ("test", RunState::Running)]);
        assert!(matches!(client.check_runs("auth-fixture", "cedar", "trail", &sha, None), Err(Error::Denied(_))));
        assert!(client.check_runs("auth-fixture", "cedar", "trail", "main", None).is_err());
    });
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].line, format!("GET /repos/cedar/trail/commits/{sha}/check-runs?per_page=100 HTTP/1.1"));
}

#[test]
fn repositories_are_listed_from_every_installation_newest_push_first() {
    let installation = |id: u64, login: &str, suspended: bool| {
        json!({"id": id, "account": {"login": login, "id": id, "type": "Organization"}, "permissions": {"contents": "write"}, "suspended_at": if suspended { json!("2026-01-01") } else { Value::Null }})
    };
    let listed = |full_name: &str, private: bool, pushed: Option<&str>| json!({"full_name": full_name, "private": private, "pushed_at": pushed});
    let first_page = response(
        200,
        json!({"total_count": 3, "repositories": [
            listed("cedar/summit-trail", false, Some("2026-11-25T08:00:00Z")),
            listed("cedar/badge-trail", true, Some("2026-11-20T10:00:00Z")),
            {"full_name": "cedar/../escape", "private": false},
            {"full_name": "cedar/no-visibility"},
        ]}),
    )
    .replace("Content-Type:", "Link: <https://example.invalid/next>; rel=\"next\"\r\nContent-Type:");
    let records = mock(
        vec![
            response(200, json!({"installations": [installation(501, "cedar", false), installation(502, "frozen", true), installation(503, "fern", false)]})),
            first_page,
            response(200, json!({"repositories": [listed("cedar/old-map", false, None)]})),
            response(200, json!({"repositories": [listed("fern/summit-trail", false, Some("2026-11-24T12:30:00Z")), listed("Cedar/Summit-trail", false, Some("2026-11-26T00:00:00Z"))]})),
        ],
        |client| {
            let repositories = client.repositories("auth-fixture").unwrap();
            let shown: Vec<(String, bool, Option<i64>)> = repositories.iter().map(|r| (r.full_name(), r.private, r.pushed)).collect();
            assert_eq!(
                shown,
                [
                    ("cedar/summit-trail".to_owned(), false, epoch_of("2026-11-25T08:00:00Z")),
                    ("fern/summit-trail".to_owned(), false, epoch_of("2026-11-24T12:30:00Z")),
                    ("cedar/badge-trail".to_owned(), true, epoch_of("2026-11-20T10:00:00Z")),
                    ("cedar/old-map".to_owned(), false, None),
                ]
            );
            assert_eq!(repositories[0].clone_url(), "https://github.com/cedar/summit-trail.git");
        },
    );
    let lines: Vec<&str> = records.iter().map(|record| record.line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "GET /user/installations?per_page=100&page=1 HTTP/1.1",
            "GET /user/installations/501/repositories?per_page=100&page=1 HTTP/1.1",
            "GET /user/installations/501/repositories?per_page=100&page=2 HTTP/1.1",
            "GET /user/installations/503/repositories?per_page=100&page=1 HTTP/1.1",
        ]
    );
}

#[test]
fn a_repository_reports_the_parent_it_was_forked_from() {
    let records = mock(
        vec![
            response(200, json!({"full_name": "fern/summit-trail", "private": false, "fork": true, "parent": {"full_name": "cedar/summit-trail"}})),
            response(200, json!({"full_name": "cedar/summit-trail", "private": true, "fork": false, "parent": {"full_name": "ignored/parent"}})),
            response(404, json!({"message": "Not Found"})),
            response(200, json!({"full_name": "someone/else", "private": false, "fork": false})),
            response(403, json!({"message": "Resource not accessible by integration"})),
        ],
        |client| {
            let fork = client.repository("auth-fixture", "fern", "summit-trail").unwrap().unwrap();
            assert_eq!(fork, Repo { full_name: "fern/summit-trail".into(), private: false, parent: Some("cedar/summit-trail".into()) });
            let original = client.repository("auth-fixture", "cedar", "summit-trail").unwrap().unwrap();
            assert_eq!(original.parent, None);
            assert!(original.private);
            assert_eq!(client.repository("auth-fixture", "hiker", "summit-trail").unwrap(), None);
            assert!(client.repository("auth-fixture", "cedar", "moved").is_err());
            assert!(matches!(client.repository("auth-fixture", "cedar", "locked"), Err(Error::Denied(_))));
            assert!(client.repository("auth-fixture", "cedar", "../x").is_err());
        },
    );
    assert_eq!(records.len(), 5);
    assert_eq!(records[0].line, "GET /repos/fern/summit-trail HTTP/1.1");
}

#[test]
fn a_push_time_is_read_as_seconds_since_1970() {
    assert_eq!(epoch_of("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(epoch_of("2000-03-01T00:00:01Z"), Some(951_868_801));
    let stamp = epoch_of("2026-11-20T10:00:00Z").unwrap();
    assert_eq!(crate::view::date_of(stamp), "2026-11-20");
    assert_eq!(stamp % 86_400, 36_000);
    for stamp in ["", "2026-11-20", "2026-11-20T10:00:00", "2026-13-20T10:00:00Z", "2026-11-20T25:00:00Z", "2026-1a-20T10:00:00Z", "2026-11-20T10:00:00+01:00"] {
        assert_eq!(epoch_of(stamp), None, "{stamp:?}");
    }
}
