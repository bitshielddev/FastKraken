use crate::{
    Tab, auth,
    github::{Client, Error, Tokens},
    notice::Notice,
    repository::{Repository, checked},
    view::{self, Tone, Ui},
};
use gpui::Context;
use std::collections::BTreeMap;

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target};
    fn unbound<'a>(ui: &Ui, target: &'a Target) -> Option<&'a String> {
        match target {
            Target::Remote(name) if ui.refs.github.contains(name) => Some(name),
            _ => None,
        }
    }
    &[Command::new(
        "remote.use-github",
        Group::Remotes,
        |_, _| "Use your GitHub connection".into(),
        |ui, target, _| match unbound(ui, target) {
            Some(_) => Availability::unless(ui.connect.checking.is_some().then(|| "Checking GitHub access already".to_owned())),
            None => Availability::Hidden,
        },
        |ui, target| {
            unbound(ui, target).map(|name| view::Action::Connect(Action::Use(Bind { remote: Some(name.clone()), ..Bind::default() })))
        },
    )
    .words(&["github", "sign in", "token", "credentials", "account"])
    .icon("cloud")]
};

#[derive(Clone, Debug, Default)]
pub(crate) struct Bind {
    pub remote: Option<String>,
    pub url: Option<String>,
    pub again: Option<Box<view::Action>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Use(Bind),
    Reconnect { again: Option<Box<view::Action>> },
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) checking: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    Unbound { remote: String, url: String },
    Bound,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    Bound { remote: String, full_name: String, login: String },
    Already { remote: String },
    SignIn { bind: Bind, why: Option<String> },
    NotInstalled { bind: Bind, full_name: String, owner: String, login: String, slug: String, install: String },
}

impl PartialEq for Bind {
    fn eq(&self, other: &Self) -> bool {
        self.remote == other.remote && self.url == other.url
    }
}
impl Eq for Bind {}


fn github_remotes(repo: &Repository) -> Vec<(String, String)> {
    github_among(crate::remotes::addresses(repo).unwrap_or_default())
}

fn github_among(remotes: BTreeMap<String, crate::remotes::Addresses>) -> Vec<(String, String)> {
    remotes
        .into_iter()
        .filter_map(|(name, addresses)| match addresses.fetch.as_slice() {
            [url] if addresses.push.iter().all(|push| push == url) && auth::github_repo(url).is_some() => Some((name, url.clone())),
            _ => None,
        })
        .filter(|(name, _)| crate::remotes::validate_name(name).is_ok())
        .collect()
}

pub(crate) fn unbound(repo: &Repository) -> Vec<String> {
    unbound_among(repo, crate::remotes::addresses(repo).unwrap_or_default())
}

pub(crate) fn unbound_among(repo: &Repository, addresses: BTreeMap<String, crate::remotes::Addresses>) -> Vec<String> {
    let remotes = github_among(addresses);
    if remotes.is_empty() {
        return Vec::new();
    }
    let bound = auth::bindings(repo).unwrap_or_default();
    remotes
        .into_iter()
        .filter(|(name, url)| !bound.iter().any(|(remote, address)| remote == name && address == url))
        .map(|(name, _)| name)
        .collect()
}

pub(crate) fn hint(repo: &Repository, remote: Option<&str>, said: &str) -> Option<Hint> {
    let remotes = github_remotes(repo);
    let quoted = ["Authentication failed for '", "unable to access '", "for '"]
        .iter()
        .find_map(|start| {
            let at = said.find(start)? + start.len();
            let rest = &said[at..];
            Some(rest[..rest.find('\'')?].trim_end_matches('/').to_owned())
        });
    let (name, url) = match remote {
        Some(remote) => remotes.into_iter().find(|(name, _)| name == remote)?,
        None => match quoted.as_deref() {
            Some(quoted) if auth::github_repo(quoted).is_some() => {
                remotes.into_iter().find(|(_, url)| url.trim_end_matches('/').eq_ignore_ascii_case(quoted))?
            }
            Some(quoted) if crate::explain::host_of(quoted).is_some_and(|host| !host.eq_ignore_ascii_case("github.com")) => {
                return None;
            }
            _ if remotes.len() == 1 => remotes.into_iter().next()?,
            _ => return None,
        },
    };
    match auth::bound(repo, &name) {
        Ok(Some(address)) if address == url => Some(Hint::Bound),
        Ok(_) => Some(Hint::Unbound { remote: name, url }),
        Err(_) => None,
    }
}

fn addresses(repo: &Repository, remote: &str, push: bool) -> Result<Vec<String>, String> {
    let mut command = repo.read_command();
    command.args(["remote", "get-url", "--all"]);
    if push {
        command.arg("--push");
    }
    let bytes = checked(command.args(["--", remote])).map_err(|_| format!("{remote} is not a remote of this repository any more."))?;
    Ok(String::from_utf8_lossy(&bytes).lines().map(str::to_owned).collect())
}

fn target(repo: &Repository, bind: &Bind) -> Result<(String, String, String, String), String> {
    let remote = match &bind.remote {
        Some(remote) => remote.clone(),
        None => match unbound(repo).as_slice() {
            [one] => one.clone(),
            [] => return Err("No remote of this repository has a github.com HTTPS address that the connection could sign in for.".into()),
            _ => return Err("Several remotes have github.com addresses. Right-click the one to use in the sidebar and choose Use your GitHub connection.".into()),
        },
    };
    crate::remotes::validate_name(&remote)?;
    crate::remotes::check_transport(repo, &remote)?;
    let fetch = addresses(repo, &remote, false)?;
    let push = addresses(repo, &remote, true)?;
    let url = match (fetch.as_slice(), push.as_slice()) {
        ([fetch], [push]) if fetch == push => fetch.clone(),
        _ => return Err(format!("{remote} fetches from or pushes to more than one address. Give it one address to use the GitHub connection.")),
    };
    if let Some(expected) = &bind.url {
        if *expected != url {
            return Err(format!("{remote} points at {url} now, not {expected}. Nothing was changed."));
        }
    }
    let (owner, name) = auth::github_repo(&url).ok_or_else(|| {
        format!("{remote} is not a github.com HTTPS address, such as https://github.com/owner/repository.git, so the GitHub connection cannot sign in for it.")
    })?;
    Ok((remote, url, owner, name))
}

pub(crate) fn reach(
    repo: &Repository,
    bind: &Bind,
    client: &Client,
    signed_in: Result<Option<(u64, Tokens)>, String>,
) -> Result<Reach, String> {
    let (remote, url, owner, name) = target(repo, bind)?;
    let found = Bind { remote: Some(remote.clone()), url: Some(url.clone()), again: bind.again.clone() };
    if auth::bound(repo, &remote)?.as_deref() == Some(url.as_str()) {
        return Ok(Reach::Already { remote });
    }
    let (account, tokens) = match signed_in {
        Ok(Some(signed_in)) => signed_in,
        Ok(None) => return Ok(Reach::SignIn { bind: found, why: None }),
        Err(why) => return Ok(Reach::SignIn { bind: found, why: Some(why) }),
    };
    let lapsed = |said: String| Ok(Reach::SignIn { bind: found.clone(), why: Some(format!("{}. {said}", auth::LAPSED)) });
    let (id, login) = match client.user(&tokens.access_token) {
        Ok(user) => user,
        Err(Error::Unauthorized(said)) => return lapsed(said),
        Err(error) => return Err(error.to_string()),
    };
    if id != account {
        return Ok(Reach::SignIn { bind: found, why: Some("GitHub returned a different account. Connect again.".into()) });
    }
    let not_installed = |full_name: String| {
        Ok(Reach::NotInstalled {
            bind: found.clone(),
            full_name,
            owner: owner.clone(),
            login: login.clone(),
            slug: client.slug().to_owned(),
            install: client.installation_url(),
        })
    };
    let repository = match client.lookup(&tokens.access_token, &owner, &name) {
        Ok(Some(repository)) => repository,
        Ok(None) => return not_installed(format!("{owner}/{name}")),
        Err(Error::Unauthorized(said)) => return lapsed(said),
        Err(error) => return Err(error.to_string()),
    };
    let owner_now = repository.full_name.split('/').next().unwrap_or(&owner).to_owned();
    match client.installed_on(&tokens.access_token, &owner_now) {
        Ok(true) => {}
        Ok(false) => return not_installed(repository.full_name),
        Err(Error::Unauthorized(said)) => return lapsed(said),
        Err(error) => return Err(error.to_string()),
    }
    target(repo, &found)?;
    auth::bind(repo, &remote, &url, client, account)?;
    Ok(Reach::Bound { remote, full_name: repository.full_name, login })
}


pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::Use(bind) => start(tab, bind, cx),
        Action::Reconnect { again } => {
            tab.ui.menu = None;
            crate::publish::sign_in(tab, crate::publish::After::Retry(again), cx);
        }
    }
    cx.notify();
}

pub(crate) fn start(tab: &mut Tab, bind: Bind, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if tab.ui.connect.checking.is_some() {
        return;
    }
    tab.ui.menu = None;
    let epoch = tab.epoch;
    let spinner = tab.ui.notices.progress("Checking GitHub access…");
    tab.ui.connect.checking = Some(spinner);
    let again = bind.again.clone();
    cx.notify();
    Tab::work(
        cx,
        move || {
            let client = auth::client()?;
            let signed_in = auth::active(&client);
            reach(&repo, &bind, &client, signed_in)
        },
        move |tab, result, cx| {
            tab.ui.connect.checking = None;
            if tab.epoch != epoch {
                tab.ui.notices.remove(spinner);
                return;
            }
            match result {
                Ok(Reach::SignIn { bind, .. }) => {
                    tab.ui.notices.remove(spinner);
                    crate::publish::sign_in(tab, crate::publish::After::Bind(bind), cx);
                }
                Ok(reach) => {
                    let shown = match reach {
                        Reach::Bound { .. } => bound_notice(reach, again),
                        reach => shown(reach),
                    };
                    let shown = shown.logged(tab.trail.entry(false));
                    tab.ui.notices.finish(spinner, shown);
                }
                Err(error) => {
                    let shown = Notice::new(Tone::Error, "The GitHub connection was not used", error).logged(tab.trail.entry(true));
                    tab.ui.notices.finish(spinner, shown);
                }
            }
            crate::refs::refresh(tab, cx);
            cx.notify();
        },
    );
}

pub(crate) fn shown(reach: Reach) -> Notice {
    match reach {
        Reach::Bound { remote, full_name, login } => Notice::new(
            Tone::Success,
            format!("{remote} now uses your GitHub connection"),
            format!("Fetch, pull and push to {full_name} sign in as {login}."),
        ),
        Reach::Already { remote } => Notice::new(
            Tone::Info,
            format!("{remote} already uses your GitHub connection"),
            "Nothing was changed.",
        ),
        Reach::NotInstalled { bind, full_name, owner, login, slug, install } => Notice::new(
            Tone::Error,
            format!("Your GitHub connection cannot reach {full_name}"),
            format!(
                "GitHub account {login} cannot see {full_name}, or cannot push to it, through the {slug} app. The usual cause is that the app is not installed on {owner}: install it there, or give it access to this repository, then try again."
            ),
        )
        .offer("Install the GitHub App…", view::Action::Handoff(crate::handoff::Action::OpenUrl(install)))
        .offer("Try again", view::Action::Connect(Action::Use(bind))),
        Reach::SignIn { why, .. } => Notice::new(Tone::Info, "Connect GitHub first", why.unwrap_or_default()),
    }
}

fn bound_notice(reach: Reach, again: Option<Box<view::Action>>) -> Notice {
    match again {
        Some(again) => shown(reach).offer("Retry", *again),
        None => shown(reach),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        github::tests::{mock, response},
        repository::tests::Fixture,
    };
    use serde_json::json;

    fn tokens() -> Tokens {
        Tokens { access_token: "connect-fixture".into(), refresh_token: None, expires_at: None }
    }

    fn installation(login: &str, contents: &str) -> serde_json::Value {
        json!({"account":{"login":login,"id":5,"type":"Organization"},"permissions":{"contents":contents},"suspended_at":null})
    }

    fn repository(full_name: &str) -> serde_json::Value {
        json!({"id":77,"full_name":full_name,"clone_url":format!("https://github.com/{full_name}.git"),"private":true,"owner":{"id":5}})
    }

    fn remote(f: &Fixture, name: &str, url: &str) {
        f.git(&["remote", "add", name, url]);
    }

    #[test]
    fn only_single_github_https_addresses_are_offered_and_bound_ones_are_not() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        remote(&f, "origin", "https://github.com/TrailTeam/ledger.git");
        remote(&f, "plain", "https://github.com/TrailTeam/notes");
        remote(&f, "ssh", "git@github.com:TrailTeam/ledger.git");
        remote(&f, "named", "https://cedar@github.com/TrailTeam/ledger.git");
        remote(&f, "other", "https://git.example.invalid/TrailTeam/ledger.git");
        remote(&f, "split", "https://github.com/TrailTeam/split.git");
        f.git(&["remote", "set-url", "--push", "split", "https://github.com/TrailTeam/elsewhere.git"]);
        assert_eq!(unbound(&f.repo), ["origin", "plain"]);
        let client = Client::for_test("http://127.0.0.1:9/");
        auth::bind(&f.repo, "origin", "https://github.com/TrailTeam/ledger.git", &client, 11).unwrap();
        assert_eq!(unbound(&f.repo), ["plain"]);

        let said = "fatal: Authentication failed for 'https://github.com/TrailTeam/notes/'";
        assert_eq!(
            hint(&f.repo, None, said),
            Some(Hint::Unbound { remote: "plain".into(), url: "https://github.com/TrailTeam/notes".into() })
        );
        assert_eq!(hint(&f.repo, Some("origin"), "remote: Invalid username or token."), Some(Hint::Bound));
        assert_eq!(hint(&f.repo, Some("other"), said), None);
        assert_eq!(hint(&f.repo, None, "fatal: could not read Username for 'https://github.com': terminal prompts disabled"), None);
    }

    #[test]
    fn a_failed_sign_in_to_another_host_is_not_offered_the_github_connection() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        remote(&f, "origin", "https://github.com/TrailTeam/ledger.git");
        remote(&f, "work", "https://git.example.invalid/team/ledger.git");
        for said in [
            "fatal: Authentication failed for 'https://git.example.invalid/team/ledger.git/'",
            "fatal: could not read Username for 'https://git.example.invalid': terminal prompts disabled",
        ] {
            assert_eq!(hint(&f.repo, None, said), None, "{said}");
        }
        let unbound = Some(Hint::Unbound { remote: "origin".into(), url: "https://github.com/TrailTeam/ledger.git".into() });
        assert_eq!(hint(&f.repo, None, "fatal: could not read Username for 'https://github.com': terminal prompts disabled"), unbound);
        assert_eq!(hint(&f.repo, None, "fatal: Authentication failed"), unbound);
    }

    #[test]
    fn a_reachable_repository_is_bound_and_its_fetches_sign_in_through_the_connection() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        let url = "https://github.com/TrailTeam/ledger.git";
        remote(&f, "origin", url);
        let bind = Bind { remote: Some("origin".into()), url: Some(url.into()), again: None };
        let records = mock(
            vec![
                response(200, json!({"id":11,"login":"Cedar"})),
                response(200, repository("TrailTeam/ledger")),
                response(200, json!({"installations":[installation("trailteam","write")]})),
            ],
            |client| {
                let reach = reach(&f.repo, &bind, &client, Ok(Some((11, tokens())))).unwrap();
                assert_eq!(
                    reach,
                    Reach::Bound { remote: "origin".into(), full_name: "TrailTeam/ledger".into(), login: "Cedar".into() }
                );
            },
        );
        assert_eq!(records[1].line, "GET /repos/TrailTeam/ledger HTTP/1.1");
        assert_eq!(auth::bound(&f.repo, "origin").unwrap().as_deref(), Some(url));
        assert!(unbound(&f.repo).is_empty());
        assert_eq!(f.git(&["remote", "get-url", "origin"]), format!("{url}\n").as_bytes());
        let mut command = f.repo.read_command();
        auth::configure_transport(&f.repo, &mut command).unwrap();
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&format!("credential.{url}.username=x-access-token")));
        let client = Client::for_test("http://127.0.0.1:9/");
        assert_eq!(reach(&f.repo, &bind, &client, Ok(Some((11, tokens())))).unwrap(), Reach::Already { remote: "origin".into() });
    }

    #[test]
    fn a_repository_the_app_cannot_reach_links_to_its_installation_and_binds_nothing() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        remote(&f, "origin", "https://github.com/Stranger/atlas");
        let bind = Bind { remote: Some("origin".into()), ..Bind::default() };
        let check = |responses: Vec<String>| {
            let mut found = None;
            mock(responses, |client| found = Some(reach(&f.repo, &bind, &client, Ok(Some((11, tokens())))).unwrap()));
            found.unwrap()
        };
        let hidden = check(vec![response(200, json!({"id":11,"login":"Cedar"})), response(404, json!({}))]);
        let Reach::NotInstalled { full_name, owner, install, .. } = &hidden else { panic!("{hidden:?}") };
        assert_eq!((full_name.as_str(), owner.as_str()), ("Stranger/atlas", "Stranger"));
        assert!(install.ends_with("/apps/test-app/installations/new"));
        let public = check(vec![
            response(200, json!({"id":11,"login":"Cedar"})),
            response(200, repository("Stranger/atlas")),
            response(200, json!({"installations":[installation("Cedar","write"),installation("Stranger","read")]})),
        ]);
        assert!(matches!(public, Reach::NotInstalled { .. }));
        assert_eq!(auth::bound(&f.repo, "origin").unwrap(), None);
        let notice = shown(public);
        assert_eq!(notice.title, "Your GitHub connection cannot reach Stranger/atlas");
        assert!(notice.detail.contains("not installed on Stranger"));
        let labels: Vec<_> = notice.actions.iter().map(|(label, _)| label.to_string()).collect();
        assert_eq!(labels, ["Install the GitHub App…", "Try again"]);
        assert!(matches!(
            &notice.actions[0].1,
            view::Action::Handoff(crate::handoff::Action::OpenUrl(url)) if url.ends_with("/apps/test-app/installations/new")
        ));
    }

    #[test]
    fn without_a_live_connection_the_sign_in_comes_first() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        remote(&f, "origin", "https://github.com/TrailTeam/ledger.git");
        let bind = Bind { remote: None, ..Bind::default() };
        let client = Client::for_test("http://127.0.0.1:9/");
        let expected = Bind { remote: Some("origin".into()), url: Some("https://github.com/TrailTeam/ledger.git".into()), again: None };
        assert_eq!(reach(&f.repo, &bind, &client, Ok(None)).unwrap(), Reach::SignIn { bind: expected.clone(), why: None });
        let lapsed = reach(&f.repo, &bind, &client, Err("The GitHub sign-in has lapsed. Reconnect GitHub.".into())).unwrap();
        assert!(matches!(lapsed, Reach::SignIn { ref bind, why: Some(_) } if *bind == expected));
        mock(vec![response(401, json!({}))], |client| {
            let revoked = reach(&f.repo, &bind, &client, Ok(Some((11, tokens())))).unwrap();
            assert!(matches!(revoked, Reach::SignIn { why: Some(ref why), .. } if why.starts_with(auth::LAPSED)));
        });
        assert_eq!(auth::bound(&f.repo, "origin").unwrap(), None);
    }

    #[test]
    fn a_remote_that_changed_since_the_offer_is_refused() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        let offered = "https://github.com/TrailTeam/ledger.git";
        remote(&f, "origin", offered);
        f.git(&["remote", "set-url", "origin", "https://github.com/Stranger/ledger.git"]);
        let bind = Bind { remote: Some("origin".into()), url: Some(offered.into()), again: None };
        let client = Client::for_test("http://127.0.0.1:9/");
        let error = reach(&f.repo, &bind, &client, Ok(Some((11, tokens())))).unwrap_err();
        assert_eq!(error, format!("origin points at https://github.com/Stranger/ledger.git now, not {offered}. Nothing was changed."));
        f.git(&["remote", "set-url", "--add", "--push", "origin", offered]);
        f.git(&["remote", "set-url", "--add", "--push", "origin", "https://github.com/Stranger/ledger.git"]);
        let anything = Bind { remote: Some("origin".into()), ..Bind::default() };
        assert!(reach(&f.repo, &anything, &client, Ok(None)).unwrap_err().contains("more than one address"));
        f.git(&["remote", "remove", "origin"]);
        assert!(reach(&f.repo, &anything, &client, Ok(None)).unwrap_err().contains("not a remote"));
        remote(&f, "origin", "git@github.com:TrailTeam/ledger.git");
        assert!(reach(&f.repo, &anything, &client, Ok(None)).unwrap_err().contains("not a github.com HTTPS address"));
        assert_eq!(auth::bound(&f.repo, "origin").unwrap(), None);
        assert_eq!(f.git(&["remote", "get-url", "origin"]), b"git@github.com:TrailTeam/ledger.git\n");
    }
}
