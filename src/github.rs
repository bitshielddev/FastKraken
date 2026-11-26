use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env, fmt,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ureq::{
    Agent, Body,
    http::Response,
    tls::{RootCerts, TlsConfig},
};

const API_VERSION: &str = "2026-03-10";
const SIGN_IN_URL: &str = "https://github.com/login/device";

#[derive(Clone)]
pub(crate) struct Client {
    client_id: String,
    slug: String,
    http: Agent,
    web: String,
    api: String,
}

#[derive(Clone)]
pub(crate) struct DeviceCode {
    device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
}

pub(crate) enum Poll {
    Pending,
    SlowDown,
    Ready(Tokens),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Account {
    pub id: u64,
    pub login: String,
    pub owners: Vec<Owner>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Owner {
    pub id: u64,
    pub login: String,
    pub personal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CreatedRepo {
    pub id: u64,
    pub full_name: String,
    pub clone_url: String,
    pub private: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub state: PullState,
    pub head: String,
    pub head_sha: String,
    pub head_repo: Option<String>,
    pub base: String,
    pub page: String,
    pub avatar: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunState {
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckRun {
    pub name: String,
    pub state: RunState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Listed {
    pub owner: String,
    pub name: String,
    pub private: bool,
    pub pushed: Option<i64>,
}

impl Listed {
    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    pub(crate) fn clone_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Repo {
    pub full_name: String,
    pub private: bool,
    pub parent: Option<String>,
}

const LISTED_PAGES: u32 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Cached<T> {
    Fresh { value: T, etag: Option<String> },
    Unchanged,
}

static REPLAY_ORIGIN: OnceLock<String> = OnceLock::new();

pub(crate) fn serve_replay_from(base: &str) -> Result<(), String> {
    if !crate::acceptance::replaying() {
        return Err("Only an acceptance replay can redirect GitHub.".into());
    }
    let url = url::Url::parse(base).map_err(|e| e.to_string())?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none() || url.path() != "/" {
        return Err("A replay serves GitHub only from 127.0.0.1.".into());
    }
    let origin = url.origin().ascii_serialization();
    match REPLAY_ORIGIN.get() {
        Some(existing) if *existing != origin => Err("The replay already serves GitHub from another address.".into()),
        _ => {
            let _ = REPLAY_ORIGIN.set(origin);
            Ok(())
        }
    }
}

fn replay_origin() -> Option<&'static String> {
    REPLAY_ORIGIN.get().filter(|_| crate::acceptance::replaying())
}

#[derive(Debug)]
pub(crate) enum Error {
    Unauthorized(String),
    Denied(String),
    Collision(String),
    Uncertain(String),
    Other(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unauthorized(s)
            | Self::Denied(s)
            | Self::Collision(s)
            | Self::Uncertain(s)
            | Self::Other(s) => s,
        };
        f.write_str(message)
    }
}
impl std::error::Error for Error {}

struct Reply {
    status: u16,
    json: Value,
    next_page: bool,
    etag: Option<String>,
}

impl Client {
    pub(crate) fn configured() -> Result<Self, String> {
        let id = env::var("FASTKRAKEN_GITHUB_CLIENT_ID")
            .ok()
            .or_else(|| option_env!("FASTKRAKEN_GITHUB_CLIENT_ID").map(str::to_owned));
        let slug = env::var("FASTKRAKEN_GITHUB_APP_SLUG")
            .ok()
            .or_else(|| option_env!("FASTKRAKEN_GITHUB_APP_SLUG").map(str::to_owned));
        Self::new(id.as_deref().unwrap_or(""), slug.as_deref().unwrap_or(""))
    }

    pub(crate) fn new(client_id: &str, slug: &str) -> Result<Self, String> {
        let client_id = client_id.trim();
        let slug = slug.trim();
        if client_id.is_empty()
            || client_id.len() > 128
            || !client_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Set the public client ID from your GitHub App registration.".into());
        }
        if !valid_slug(slug) {
            return Err("Set the GitHub App slug from its public installation URL.".into());
        }
        if let Some(origin) = replay_origin() {
            return Ok(Self {
                client_id: client_id.into(),
                slug: slug.into(),
                http: http_agent(true),
                web: origin.clone(),
                api: origin.clone(),
            });
        }
        Ok(Self {
            client_id: client_id.into(),
            slug: slug.into(),
            http: http_agent(false),
            web: "https://github.com".into(),
            api: "https://api.github.com".into(),
        })
    }

    #[cfg(any(test, feature = "publish-replay"))]
    pub(crate) fn for_test(base: &str) -> Self {
        let url = url::Url::parse(base).expect("test base URL");
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.username().is_empty() && url.password().is_none());
        assert!(url.query().is_none() && url.fragment().is_none() && url.path() == "/");
        let origin = url.origin().ascii_serialization();
        Self {
            client_id: "test-client".into(),
            slug: "test-app".into(),
            http: http_agent(true),
            web: origin.clone(),
            api: origin,
        }
    }

    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }
    pub(crate) fn slug(&self) -> &str {
        &self.slug
    }
    pub(crate) fn installation_url(&self) -> String {
        format!("https://github.com/apps/{}/installations/new", self.slug)
    }

    fn offline(&self) -> Result<(), Error> {
        let local = self.api.starts_with("http://127.0.0.1:") && self.web.starts_with("http://127.0.0.1:");
        if (cfg!(test) || crate::acceptance::replaying()) && !local {
            return Err(Error::Other("Tests and replays never contact GitHub.".into()));
        }
        Ok(())
    }

    fn oauth(&self, path: &str, fields: &[(&str, &str)]) -> Result<Value, Error> {
        self.offline()?;
        let response = self
            .http
            .post(format!("{}{}", self.web, path))
            .header("Accept", "application/json")
            .send_form(fields.iter().copied());
        let reply = read_reply(response, false)?;
        if reply.json.get("error").is_none() && reply.status != 200 {
            return Err(status_error(reply.status, &reply.json));
        }
        Ok(reply.json)
    }

    pub(crate) fn start_device(&self) -> Result<DeviceCode, Error> {
        let body = self.oauth(
            "/login/device/code",
            &[("client_id", self.client_id()), ("scope", "offline_access")],
        )?;
        check_oauth_error(&body)?;
        let verification_uri = body["verification_uri"].as_str();
        if verification_uri != Some(SIGN_IN_URL) {
            return Err(Error::Other(
                "GitHub returned an unexpected verification address.".into(),
            ));
        }
        let code = DeviceCode {
            device_code: secret_value(&body, "device_code")?,
            user_code: secret_value(&body, "user_code")?,
            verification_uri: SIGN_IN_URL.into(),
            expires_in: body["expires_in"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 86400)
                .ok_or_else(invalid_response)?,
            interval: body["interval"].as_u64().unwrap_or(5),
        };
        if code.interval == 0 || code.interval > code.expires_in {
            return Err(invalid_response());
        }
        Ok(code)
    }

    pub(crate) fn poll_device(&self, device: &DeviceCode) -> Result<Poll, Error> {
        let body = self.oauth(
            "/login/oauth/access_token",
            &[
                ("client_id", self.client_id()),
                ("device_code", &device.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        match body["error"].as_str() {
            Some("authorization_pending") => Ok(Poll::Pending),
            Some("slow_down") => Ok(Poll::SlowDown),
            _ => tokens_from_response(&body).map(Poll::Ready),
        }
    }

    pub(crate) fn refresh(&self, refresh_token: &str) -> Result<Tokens, Error> {
        let body = self.oauth(
            "/login/oauth/access_token",
            &[
                ("client_id", self.client_id()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ],
        )?;
        tokens_from_response(&body)
    }

    fn get(&self, token: &str, path: &str) -> Result<Reply, Error> {
        self.get_since(token, path, None)
    }

    fn get_since(&self, token: &str, path: &str, etag: Option<&str>) -> Result<Reply, Error> {
        self.offline()?;
        let mut request = self
            .http
            .get(format!("{}{}", self.api, path))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(etag) = etag.filter(|etag| clean_etag(etag)) {
            request = request.header("If-None-Match", etag);
        }
        read_reply(request.call(), false)
    }

    fn cached(&self, token: &str, path: &str, etag: Option<&str>) -> Result<Cached<Value>, Error> {
        let reply = self.get_since(token, path, etag)?;
        match reply.status {
            304 if etag.is_some() => Ok(Cached::Unchanged),
            200 => Ok(Cached::Fresh { value: reply.json, etag: reply.etag.filter(|etag| clean_etag(etag)) }),
            403 | 404 if !rate_limited(reply.status, &reply.json) => {
                Err(Error::Denied("GitHub did not let FastKraken read this repository.".into()))
            }
            status => Err(status_error(status, &reply.json)),
        }
    }

    pub(crate) fn pull_requests(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        open: bool,
        etag: Option<&str>,
    ) -> Result<Cached<Vec<PullRequest>>, Error> {
        validate_destination(owner, name)?;
        let path = if open {
            format!("/repos/{owner}/{name}/pulls?state=open&per_page=100")
        } else {
            format!("/repos/{owner}/{name}/pulls?state=closed&sort=updated&direction=desc&per_page=50")
        };
        Ok(match self.cached(token, &path, etag)? {
            Cached::Unchanged => Cached::Unchanged,
            Cached::Fresh { value, etag } => {
                let listed = value.as_array().ok_or_else(invalid_response)?;
                let base = format!("{owner}/{name}");
                Cached::Fresh { value: listed.iter().filter_map(|item| pull_from_response(item, &base)).collect(), etag }
            }
        })
    }

    pub(crate) fn check_runs(
        &self,
        token: &str,
        owner: &str,
        name: &str,
        sha: &str,
        etag: Option<&str>,
    ) -> Result<Cached<Vec<CheckRun>>, Error> {
        validate_destination(owner, name)?;
        if sha.len() < 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Other("The pull request's head commit is not a full commit ID.".into()));
        }
        let path = format!("/repos/{owner}/{name}/commits/{sha}/check-runs?per_page=100");
        Ok(match self.cached(token, &path, etag)? {
            Cached::Unchanged => Cached::Unchanged,
            Cached::Fresh { value, etag } => {
                let runs = value["check_runs"].as_array().ok_or_else(invalid_response)?;
                Cached::Fresh { value: runs.iter().filter_map(check_from_response).collect(), etag }
            }
        })
    }

    pub(crate) fn commit_authors(&self, token: &str, owner: &str, name: &str) -> Result<Vec<(String, String)>, Error> {
        validate_destination(owner, name)?;
        let body = success(self.get(token, &format!("/repos/{owner}/{name}/commits?per_page=100"))?, 200)?;
        let listed = body.as_array().ok_or_else(invalid_response)?;
        Ok(listed
            .iter()
            .filter_map(|item| {
                let email = item["commit"]["author"]["email"].as_str().map(crate::photos::key_of).filter(|email| email.contains('@'))?;
                let avatar = item["author"]["avatar_url"].as_str().and_then(crate::photos::github_avatar)?;
                Some((email, avatar))
            })
            .collect())
    }

    pub(crate) fn user(&self, token: &str) -> Result<(u64, String), Error> {
        let body = success(self.get(token, "/user")?, 200)?;
        let id = body["id"].as_u64().ok_or_else(invalid_response)?;
        let login = body["login"]
            .as_str()
            .filter(|s| valid_owner_login(s))
            .ok_or_else(invalid_response)?;
        Ok((id, login.to_owned()))
    }

    pub(crate) fn account(&self, token: &str) -> Result<Account, Error> {
        let (id, login) = self.user(token)?;
        let mut owners = Vec::new();
        for install in self.installations(token)? {
            let perms = &install["permissions"];
            if !install["suspended_at"].is_null()
                || perms["contents"] != "write"
                || (perms["repository_creation"] != "write"
                    && perms["administration"] != "write")
            {
                continue;
            }
            let account = &install["account"];
            let Some(owner_id) = account["id"].as_u64() else {
                continue;
            };
            let Some(owner_login) = account["login"].as_str().filter(|s| valid_owner_login(s))
            else {
                continue;
            };
            let personal = match account["type"].as_str() {
                Some("User") if owner_id == id => true,
                Some("Organization") => false,
                _ => continue,
            };
            owners.push(Owner {
                id: owner_id,
                login: owner_login.into(),
                personal,
            });
        }
        owners.sort_by_cached_key(|owner| (!owner.personal, owner.login.to_ascii_lowercase()));
        owners.dedup_by_key(|owner| owner.id);
        Ok(Account { id, login, owners })
    }

    fn installations(&self, token: &str) -> Result<Vec<Value>, Error> {
        let mut all = Vec::new();
        for page in 1.. {
            let reply = self.get(
                token,
                &format!("/user/installations?per_page=100&page={page}"),
            )?;
            let more = reply.next_page;
            let list = success(reply, 200)?;
            all.extend(
                list["installations"]
                    .as_array()
                    .ok_or_else(invalid_response)?
                    .iter()
                    .cloned(),
            );
            if !more {
                break;
            }
        }
        Ok(all)
    }

    pub(crate) fn repositories(&self, token: &str) -> Result<Vec<Listed>, Error> {
        let mut listed: Vec<Listed> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for install in self.installations(token)? {
            let Some(id) = install["id"].as_u64().filter(|_| install["suspended_at"].is_null()) else {
                continue;
            };
            for page in 1..=LISTED_PAGES {
                let reply = self.get(token, &format!("/user/installations/{id}/repositories?per_page=100&page={page}"))?;
                let more = reply.next_page;
                let body = success(reply, 200)?;
                for repository in body["repositories"].as_array().ok_or_else(invalid_response)?.iter().filter_map(listed_from_response) {
                    if seen.insert(repository.full_name().to_ascii_lowercase()) {
                        listed.push(repository);
                    }
                }
                if !more {
                    break;
                }
            }
        }
        listed.sort_by(|a, b| {
            b.pushed.cmp(&a.pushed).then_with(|| a.full_name().to_ascii_lowercase().cmp(&b.full_name().to_ascii_lowercase()))
        });
        Ok(listed)
    }

    pub(crate) fn repository(&self, token: &str, owner: &str, name: &str) -> Result<Option<Repo>, Error> {
        validate_destination(owner, name)?;
        let reply = self.get(token, &format!("/repos/{owner}/{name}"))?;
        if reply.status == 404 {
            return Ok(None);
        }
        let body = success(reply, 200)?;
        let full_name = full_name(&body["full_name"]).ok_or_else(invalid_response)?;
        if !full_name.eq_ignore_ascii_case(&format!("{owner}/{name}")) {
            return Err(invalid_response());
        }
        let parent = if body["fork"].as_bool() == Some(true) { full_name_of(&body["parent"]) } else { None };
        Ok(Some(Repo { full_name, private: body["private"].as_bool().ok_or_else(invalid_response)?, parent }))
    }

    pub(crate) fn installed_on(&self, token: &str, owner: &str) -> Result<bool, Error> {
        Ok(self.installations(token)?.iter().any(|install| {
            install["suspended_at"].is_null()
                && install["permissions"]["contents"] == "write"
                && install["account"]["login"]
                    .as_str()
                    .is_some_and(|login| login.eq_ignore_ascii_case(owner))
        }))
    }

    pub(crate) fn create(
        &self,
        token: &str,
        owner: &Owner,
        name: &str,
        private: bool,
    ) -> Result<CreatedRepo, Error> {
        validate_destination(&owner.login, name)?;
        self.offline()?;
        let path = if owner.personal {
            "/user/repos".into()
        } else {
            format!("/orgs/{}/repos", owner.login)
        };
        let response = self
            .http
            .post(format!("{}{}", self.api, path))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("Content-Type", "application/json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .send(json!({"name":name, "private":private, "auto_init":false}).to_string());
        let reply = read_reply(response, true)?;
        if reply.status == 408 || reply.status >= 500 {
            return Err(uncertain_create());
        }
        if reply.status != 201 {
            if (200..300).contains(&reply.status) {
                return Err(uncertain_create());
            }
            return Err(status_error(reply.status, &reply.json));
        }
        let repo = repository_from_response(&reply.json, &owner.login, name)
            .ok_or_else(uncertain_create)?;
        if repo.private != private || reply.json["owner"]["id"].as_u64() != Some(owner.id) {
            return Err(Error::Uncertain("GitHub created a repository but the returned owner or visibility differs from the requested destination. Verify it on GitHub before connecting.".into()));
        }
        Ok(repo)
    }

    pub(crate) fn lookup(
        &self,
        token: &str,
        owner: &str,
        name: &str,
    ) -> Result<Option<CreatedRepo>, Error> {
        validate_destination(owner, name)?;
        let reply = self.get(token, &format!("/repos/{owner}/{name}"))?;
        if reply.status == 404 {
            return Ok(None);
        }
        let body = success(reply, 200)?;
        repository_from_response(&body, owner, name)
            .map(Some)
            .ok_or_else(invalid_response)
    }
}

pub(crate) fn http_agent(loopback: bool) -> Agent {
    let builder = Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(concat!("FastKraken/", env!("CARGO_PKG_VERSION")));
    let builder = if loopback {
        builder.proxy(None)
    } else {
        builder
    };
    builder.build().into()
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn valid_owner_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

fn validate_destination(owner: &str, name: &str) -> Result<(), Error> {
    if !valid_owner_login(owner) {
        return Err(Error::Other("Select a valid GitHub owner.".into()));
    }
    validate_repo_name(name).map_err(Error::Other)
}

pub(crate) fn validate_repo_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.len() > 100
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
    {
        return Err("Use 1–100 letters, digits, periods, hyphens or underscores for the GitHub repository name.".into());
    }
    Ok(())
}

fn read_reply(raw: Result<Response<Body>, ureq::Error>, creating: bool) -> Result<Reply, Error> {
    let unavailable = || {
        if creating {
            uncertain_create()
        } else {
            Error::Other(
                "Could not read a response from GitHub. Check your connection and try again."
                    .into(),
            )
        }
    };
    let mut response = raw.map_err(|_| unavailable())?;
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("ETag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let next_page = response
        .headers()
        .get_all("Link")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|header| header.split(','))
        .flat_map(|link| link.split(';').skip(1))
        .any(|attr| attr.trim() == "rel=\"next\"");
    let data = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec();
    let json = data
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let body = if (200..300).contains(&status) {
        json.ok_or_else(unavailable)?
    } else {
        json.unwrap_or(Value::Null)
    };
    Ok(Reply {
        status,
        json: body,
        next_page,
        etag,
    })
}

fn invalid_response() -> Error {
    Error::Other("GitHub returned an incomplete or unexpected response.".into())
}
fn uncertain_create() -> Error {
    Error::Uncertain(
        "GitHub may have created the repository. Check the destination before creating it again."
            .into(),
    )
}
fn success(reply: Reply, status: u16) -> Result<Value, Error> {
    if reply.status == status {
        Ok(reply.json)
    } else {
        Err(status_error(reply.status, &reply.json))
    }
}
fn clean_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 200 && etag.bytes().all(|b| b.is_ascii_graphic())
}

fn rate_limited(status: u16, body: &Value) -> bool {
    status == 429 || body["message"].as_str().is_some_and(|s| s.to_ascii_lowercase().contains("rate limit"))
}

fn plain_text(value: &Value, limit: usize) -> Option<String> {
    let text: String = value.as_str()?.chars().filter(|c| !c.is_control()).take(limit).collect();
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

fn branch_name(value: &Value) -> Option<String> {
    let name = value.as_str()?;
    (!name.is_empty() && name.len() <= 255 && !name.chars().any(|c| c.is_control() || c.is_whitespace())).then(|| name.to_owned())
}

fn full_name(value: &Value) -> Option<String> {
    let (owner, name) = value.as_str()?.split_once('/')?;
    (valid_owner_login(owner) && validate_repo_name(name).is_ok()).then(|| format!("{owner}/{name}"))
}

fn full_name_of(repository: &Value) -> Option<String> {
    full_name(&repository["full_name"])
}

fn listed_from_response(item: &Value) -> Option<Listed> {
    let full = full_name_of(item)?;
    let (owner, name) = full.split_once('/')?;
    Some(Listed {
        owner: owner.to_owned(),
        name: name.to_owned(),
        private: item["private"].as_bool()?,
        pushed: item["pushed_at"].as_str().and_then(epoch_of),
    })
}

pub(crate) fn epoch_of(stamp: &str) -> Option<i64> {
    let bytes = stamp.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> {
        let part = &stamp[from..to];
        part.bytes().all(|b| b.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn pull_from_response(item: &Value, base: &str) -> Option<PullRequest> {
    let number = item["number"].as_u64().filter(|n| *n > 0)?;
    let head_sha = item["head"]["sha"].as_str().filter(|sha| sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let state = match (item["state"].as_str()?, item["draft"].as_bool().unwrap_or(false), item["merged_at"].is_string()) {
        ("open", true, _) => PullState::Draft,
        ("open", false, _) => PullState::Open,
        ("closed", _, true) => PullState::Merged,
        ("closed", _, false) => PullState::Closed,
        _ => return None,
    };
    Some(PullRequest {
        number,
        title: plain_text(&item["title"], 300).unwrap_or_else(|| format!("Pull request #{number}")),
        author: item["user"]["login"].as_str().filter(|login| valid_owner_login(login.trim_end_matches("[bot]"))).unwrap_or("someone").to_owned(),
        state,
        head: branch_name(&item["head"]["ref"])?,
        head_sha: head_sha.to_ascii_lowercase(),
        head_repo: full_name(&item["head"]["repo"]["full_name"]),
        base: branch_name(&item["base"]["ref"])?,
        page: format!("https://github.com/{base}/pull/{number}"),
        avatar: item["user"]["avatar_url"].as_str().and_then(crate::photos::github_avatar),
    })
}

fn check_from_response(item: &Value) -> Option<CheckRun> {
    let name = plain_text(&item["name"], 200)?;
    let state = match (item["status"].as_str()?, item["conclusion"].as_str()) {
        ("completed", Some("success")) => RunState::Passed,
        ("completed", Some("neutral" | "skipped" | "stale")) => RunState::Skipped,
        ("completed", _) => RunState::Failed,
        _ => RunState::Running,
    };
    Some(CheckRun { name, state })
}

fn status_error(status: u16, body: &Value) -> Error {
    match status {
        401 => Error::Unauthorized("The GitHub authorisation has expired or was revoked. Connect GitHub again.".into()),
        403 | 429 if status == 429 || body["message"].as_str().is_some_and(|s| s.to_ascii_lowercase().contains("rate limit")) =>
            Error::Other("GitHub rate-limited this request. Wait before retrying.".into()),
        403 => Error::Denied("GitHub denied this operation. Check the app installation, your organisation’s repository policy, and any required SSO session.".into()),
        422 if body["errors"].as_array().is_some_and(|items| items.iter().any(|e| {
            e["field"] == "name" && (e["code"] == "already_exists" || e["message"].as_str().is_some_and(|m| m.to_ascii_lowercase().contains("already exists")))
        })) => Error::Collision("A repository with that name already exists. Choose another name or connect the existing repository.".into()),
        422 => Error::Other("GitHub rejected the repository settings. Check the name, visibility and your organisation’s required properties.".into()),
        404 => Error::Denied("GitHub could not find the destination or did not grant this account access.".into()),
        _ => Error::Other(format!("GitHub returned HTTP {status}.")),
    }
}

fn clean_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn secret_value(body: &Value, key: &str) -> Result<String, Error> {
    body[key]
        .as_str()
        .filter(|s| clean_secret(s))
        .map(str::to_owned)
        .ok_or_else(invalid_response)
}
fn check_oauth_error(value: &Value) -> Result<(), Error> {
    let Some(error) = value["error"].as_str() else {
        return Ok(());
    };
    Err(match error {
        "access_denied" => Error::Denied("GitHub authorisation was cancelled.".into()),
        "expired_token" | "bad_verification_code" => Error::Unauthorized(
            "The device code expired or is no longer valid. Connect GitHub again.".into(),
        ),
        "bad_refresh_token" | "invalid_grant" => {
            Error::Unauthorized("GitHub authorisation expired. Connect GitHub again.".into())
        }
        "device_flow_disabled" | "incorrect_client_credentials" | "invalid_client" => Error::Other(
            "Check the GitHub App client ID and enable device flow in its registration.".into(),
        ),
        _ => Error::Other("GitHub could not authorise this application.".into()),
    })
}
fn tokens_from_response(body: &Value) -> Result<Tokens, Error> {
    check_oauth_error(body)?;
    if !body["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
    {
        return Err(invalid_response());
    }
    let access_token = secret_value(body, "access_token")?;
    let refresh_token = if body["refresh_token"].is_null() {
        None
    } else {
        Some(secret_value(body, "refresh_token")?)
    };
    let expires_at = if body["expires_in"].is_null() {
        None
    } else {
        let lifetime = body["expires_in"].as_u64().ok_or_else(invalid_response)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Other("Check the system clock before connecting GitHub.".into()))?
            .as_secs();
        Some(now.checked_add(lifetime).ok_or_else(invalid_response)?)
    };
    Ok(Tokens {
        access_token,
        refresh_token,
        expires_at,
    })
}

fn repository_from_response(body: &Value, owner: &str, name: &str) -> Option<CreatedRepo> {
    let id = body["id"].as_u64().filter(|id| *id > 0)?;
    let full_name = body["full_name"].as_str()?;
    if !full_name.eq_ignore_ascii_case(&format!("{owner}/{name}")) {
        return None;
    }
    let clone_url = body["clone_url"].as_str()?;
    if clone_url != format!("https://github.com/{full_name}.git") {
        return None;
    }
    let private = body["private"].as_bool()?;
    if let Some(visibility) = body.get("visibility")
        && visibility != if private { "private" } else { "public" }
    {
        return None;
    }
    Some(CreatedRepo {
        id,
        full_name: full_name.into(),
        clone_url: clone_url.into(),
        private,
    })
}

#[cfg(test)]
pub(crate) mod tests;
