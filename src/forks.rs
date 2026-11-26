use crate::{
    Tab, auth,
    github::{Error, Repo},
    notice::{Notice, Outcome},
    pulls,
    repository::Repository,
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target, busy, networking, operating};
    fn hosted<'a>(ui: &Ui, target: &'a Target) -> Option<(&'a String, String)> {
        match target {
            Target::Remote(name) => pulls::repo_of(ui, name).map(|full_name| (name, full_name)),
            _ => None,
        }
    }
    fn looking(ui: &Ui, target: &Target) -> Availability {
        if hosted(ui, target).is_none() {
            return Availability::Hidden;
        }
        Availability::unless(
            busy(ui)
                .or_else(|| networking(ui))
                .or_else(|| operating(ui))
                .or_else(|| ui.forks.looking.then(|| "Looking on GitHub already".to_owned())),
        )
    }
    &[
        Command::new(
            "remote.fork-on-github",
            Group::Remotes,
            |_, _| "Fork on GitHub…".into(),
            |ui, target, _| if hosted(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| hosted(ui, target).map(|(_, full_name)| open(fork_page(&full_name))),
        )
        .words(&["fork", "github", "copy of the repository"])
        .icon("external"),
        Command::new(
            "remote.add-my-fork",
            Group::Remotes,
            |_, _| "Add my fork as a remote".into(),
            |ui, target, _| looking(ui, target),
            |ui, target| hosted(ui, target).map(|(name, _)| view::Action::Forks(Action::AddMine(name.clone()))),
        )
        .words(&["fork", "github", "remote", "mine"])
        .icon("plus"),
        Command::new(
            "remote.add-parent",
            Group::Remotes,
            |_, _| "Add the parent repository as a remote".into(),
            |ui, target, _| looking(ui, target),
            |ui, target| hosted(ui, target).map(|(name, _)| view::Action::Forks(Action::AddParent(name.clone()))),
        )
        .words(&["upstream", "fork", "parent", "github", "remote"])
        .icon("plus"),
    ]
};

pub(crate) const CHOOSE: &str = "Choose repositories…";
pub(crate) const FORK: &str = "Fork on GitHub…";

#[derive(Default)]
pub(crate) struct State {
    pub(crate) looking: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    AddMine(String),
    AddParent(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want {
    Mine,
    Parent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Found {
    Fork { full_name: String, account: u64 },
    NoFork { wanted: String, of: String, install: String },
    NotAFork { full_name: String, of: String },
    Own { login: String },
    Parent { full_name: String },
    NoParent { full_name: String },
    Hidden { full_name: String, install: String },
    SignedOut,
}

pub(crate) fn fork_page(full_name: &str) -> String {
    format!("https://github.com/{full_name}/fork")
}

fn open(url: String) -> view::Action {
    view::Action::Handoff(crate::handoff::Action::OpenUrl(url))
}

fn address_of(full_name: &str) -> String {
    format!("https://github.com/{full_name}.git")
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::AddMine(remote) => look(tab, remote, Want::Mine, cx),
        Action::AddParent(remote) => look(tab, remote, Want::Parent, cx),
    }
    cx.notify();
}

fn look(tab: &mut Tab, remote: String, want: Want, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let Some(repo) = tab.repo.clone() else { return };
    let Some(full_name) = pulls::repo_of(&tab.ui, &remote) else {
        tab.refuse(format!("{remote} is not a GitHub repository"), cx);
        return;
    };
    if tab.ui.forks.looking {
        return;
    }
    if tab.ui.busy() {
        tab.refuse("Wait for the current operation to finish".to_owned(), cx);
        return;
    }
    tab.ui.forks.looking = true;
    let epoch = tab.epoch;
    let spinner = tab.ui.notices.progress(match want {
        Want::Mine => "Looking for your fork on GitHub…",
        Want::Parent => "Looking up the parent repository on GitHub…",
    });
    let again = match want {
        Want::Mine => Action::AddMine(remote.clone()),
        Want::Parent => Action::AddParent(remote.clone()),
    };
    Tab::work(
        cx,
        move || {
            let found = match connect(&repo, &remote)? {
                Some(connection) => found(&connection, &full_name, want)?,
                None => Found::SignedOut,
            };
            Ok::<_, String>((found, remote))
        },
        move |tab, result, cx| {
            tab.ui.forks.looking = false;
            if tab.epoch != epoch {
                tab.ui.notices.remove(spinner);
                return;
            }
            match result {
                Ok((found, remote)) => {
                    tab.ui.notices.remove(spinner);
                    settle(tab, &remote, found, again, cx);
                }
                Err(why) => {
                    let title = match want {
                        Want::Mine => "Your fork was not looked up",
                        Want::Parent => "The parent repository was not looked up",
                    };
                    tab.ui.notices.finish(spinner, Notice::new(Tone::Error, title, why));
                }
            }
            cx.notify();
        },
    );
}

fn connect(repo: &Repository, remote: &str) -> Result<Option<auth::Connection>, String> {
    let lapsed = |why: String| if why.starts_with(auth::LAPSED) { Ok(None) } else { Err(why) };
    match auth::connection(repo, remote) {
        Ok(Some(connection)) => Ok(Some(connection)),
        Ok(None) => auth::signed_in().or_else(lapsed),
        Err(why) => lapsed(why),
    }
}

fn found(connection: &auth::Connection, full_name: &str, want: Want) -> Result<Found, String> {
    let (owner, name) = full_name.split_once('/').ok_or("The remote is not a GitHub repository.")?;
    let (client, token) = (&connection.client, connection.token.as_str());
    let install = client.installation_url();
    let failed = |error: Error| match error {
        Error::Unauthorized(_) => Ok(Found::SignedOut),
        error => Err(error.to_string()),
    };
    match want {
        Want::Mine => {
            let login = match client.user(token) {
                Ok((_, login)) => login,
                Err(error) => return failed(error),
            };
            if login.eq_ignore_ascii_case(owner) {
                return Ok(Found::Own { login });
            }
            match client.repository(token, &login, name) {
                Ok(Some(Repo { full_name: fork, parent: Some(parent), .. })) if parent.eq_ignore_ascii_case(full_name) => {
                    Ok(Found::Fork { full_name: fork, account: connection.account })
                }
                Ok(Some(other)) => Ok(Found::NotAFork { full_name: other.full_name, of: full_name.to_owned() }),
                Ok(None) | Err(Error::Denied(_)) => Ok(Found::NoFork { wanted: format!("{login}/{name}"), of: full_name.to_owned(), install }),
                Err(error) => failed(error),
            }
        }
        Want::Parent => match client.repository(token, owner, name) {
            Ok(Some(Repo { parent: Some(parent), .. })) => Ok(Found::Parent { full_name: parent }),
            Ok(Some(repository)) => Ok(Found::NoParent { full_name: repository.full_name }),
            Ok(None) | Err(Error::Denied(_)) => Ok(Found::Hidden { full_name: full_name.to_owned(), install }),
            Err(error) => failed(error),
        },
    }
}

fn remote_at(ui: &Ui, full_name: &str) -> Option<String> {
    let page = format!("https://github.com/{full_name}");
    ui.refs
        .remotes
        .iter()
        .find(|remote| crate::pull_request::web(ui, remote).is_some_and(|web| web.eq_ignore_ascii_case(&page)))
        .cloned()
}

fn free_name(ui: &Ui, names: &[String]) -> Option<String> {
    names
        .iter()
        .find(|name| crate::remotes::validate_name(name).is_ok() && !ui.refs.remotes.iter().any(|remote| remote == *name))
        .cloned()
}

fn settle(tab: &mut Tab, remote: &str, found: Found, again: Action, cx: &mut Context<Tab>) {
    let shown = match found {
        Found::Fork { full_name, account } => {
            if let Some(existing) = remote_at(&tab.ui, &full_name) {
                Notice::new(Tone::Info, "Your fork is already a remote", format!("{existing} is {full_name}. Nothing was changed."))
            } else {
                let login = full_name.split('/').next().unwrap_or_default().to_ascii_lowercase();
                match free_name(&tab.ui, &[login.clone(), format!("{login}-fork")]) {
                    Some(name) => return add(tab, name, address_of(&full_name), Some(account), cx),
                    None => Notice::new(
                        Tone::Info,
                        format!("No free remote name for {full_name}"),
                        format!("Remotes named {login} and {login}-fork exist already. Rename one, then try again."),
                    ),
                }
            }
        }
        Found::Parent { full_name } => {
            if let Some(existing) = remote_at(&tab.ui, &full_name) {
                Notice::new(Tone::Info, format!("{existing} already is the parent repository"), format!("{existing} is {full_name}. Nothing was changed."))
            } else {
                let owner = full_name.split('/').next().unwrap_or_default().to_ascii_lowercase();
                match free_name(&tab.ui, &["upstream".to_owned(), owner.clone()]) {
                    Some(name) => return add(tab, name, address_of(&full_name), None, cx),
                    None => Notice::new(
                        Tone::Info,
                        format!("No free remote name for {full_name}"),
                        format!("Remotes named upstream and {owner} exist already. Rename one, then try again."),
                    ),
                }
            }
        }
        Found::NoFork { wanted, of, install } => Notice::new(
            Tone::Info,
            "FastKraken cannot find your fork",
            format!("There is no fork of {of} at {wanted} that the GitHub connection can see. Fork it on GitHub, or if you have, choose it in the app's repositories."),
        )
        .offer(FORK, open(fork_page(&of)))
        .offer(CHOOSE, open(install)),
        Found::NotAFork { full_name, of } => Notice::new(
            Tone::Info,
            format!("{full_name} is not a fork of {of}"),
            "Nothing was added.",
        ),
        Found::Own { login } => Notice::new(
            Tone::Info,
            format!("{remote} is your own repository"),
            format!("{login} owns it, so there is no fork of it to add."),
        ),
        Found::NoParent { full_name } => Notice::new(
            Tone::Info,
            format!("{full_name} is not a fork"),
            "It has no parent repository on GitHub. Nothing was added.",
        ),
        Found::Hidden { full_name, install } => Notice::new(
            Tone::Info,
            format!("FastKraken cannot see {full_name}"),
            "Install the app on its owner, or choose it in the app's repositories, then try again.",
        )
        .offer(CHOOSE, open(install)),
        Found::SignedOut => Notice::new(Tone::Info, "Connect GitHub first", "Then choose the command again.").offer(
            "Connect GitHub…",
            view::Action::Connect(crate::connect::Action::Reconnect { again: Some(Box::new(view::Action::Forks(again))) }),
        ),
    };
    tab.ui.notices.show(shown);
    cx.notify();
}

fn add(tab: &mut Tab, name: String, url: String, account: Option<u64>, cx: &mut Context<Tab>) {
    if tab.ui.busy() {
        tab.refuse("Wait for the current operation to finish".to_owned(), cx);
        return;
    }
    let added = name.clone();
    tab.run_operation_then(
        cx,
        "Adding the remote…",
        "Remote added",
        false,
        move |repo| {
            pulls::add_fork_remote(&repo, &name, &url)?;
            let bound = account.is_some_and(|account| auth::client().and_then(|client| auth::bind(&repo, &name, &url, &client, account)).is_ok());
            let detail = if bound {
                format!("{name} is {url} and uses your GitHub connection. Fetching it now.")
            } else {
                format!("{name} is {url}. Fetching it now.")
            };
            Ok::<_, String>(Outcome::titled(format!("Added {name}"), detail))
        },
        move |tab, cx| crate::sync::fetch_remote(tab, added, cx),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        github::{
            Client, Tokens,
            tests::{mock, response},
        },
        repository::tests::Fixture,
    };
    use serde_json::json;

    fn signed_in(client: &Client) -> auth::Connection {
        auth::Connection { client: client.clone(), token: "fork-fixture".into(), account: 11 }
    }

    #[test]
    fn your_fork_is_found_by_your_login_and_must_be_a_fork_of_this_repository() {
        let records = mock(
            vec![
                response(200, json!({"id": 11, "login": "fern"})),
                response(200, json!({"full_name": "fern/summit-trail", "private": false, "fork": true, "parent": {"full_name": "Cedar/Summit-Trail"}})),
                response(200, json!({"id": 11, "login": "fern"})),
                response(200, json!({"full_name": "fern/summit-trail", "private": false, "fork": false})),
                response(200, json!({"id": 11, "login": "fern"})),
                response(404, json!({"message": "Not Found"})),
                response(200, json!({"id": 11, "login": "Cedar"})),
                response(401, json!({"message": "Bad credentials"})),
            ],
            |client| {
                let connection = signed_in(&client);
                let look = || found(&connection, "cedar/summit-trail", Want::Mine).unwrap();
                assert_eq!(look(), Found::Fork { full_name: "fern/summit-trail".into(), account: 11 });
                assert_eq!(look(), Found::NotAFork { full_name: "fern/summit-trail".into(), of: "cedar/summit-trail".into() });
                assert_eq!(
                    look(),
                    Found::NoFork { wanted: "fern/summit-trail".into(), of: "cedar/summit-trail".into(), install: client.installation_url() }
                );
                assert_eq!(look(), Found::Own { login: "Cedar".into() });
                assert_eq!(look(), Found::SignedOut);
            },
        );
        assert_eq!(records[0].line, "GET /user HTTP/1.1");
        assert_eq!(records[1].line, "GET /repos/fern/summit-trail HTTP/1.1");
        assert!(records.iter().all(|record| record.line != "GET /repos/Cedar/summit-trail HTTP/1.1"));
        assert_eq!(records.len(), 8);
    }

    #[test]
    fn the_parent_repository_comes_from_the_remote_s_own_record() {
        let records = mock(
            vec![
                response(200, json!({"full_name": "cedar/summit-trail", "private": false, "fork": true, "parent": {"full_name": "trailteam/summit-trail"}})),
                response(200, json!({"full_name": "cedar/summit-trail", "private": false, "fork": false})),
                response(403, json!({"message": "Resource not accessible by integration"})),
            ],
            |client| {
                let connection = signed_in(&client);
                let look = || found(&connection, "cedar/summit-trail", Want::Parent).unwrap();
                assert_eq!(look(), Found::Parent { full_name: "trailteam/summit-trail".into() });
                assert_eq!(look(), Found::NoParent { full_name: "cedar/summit-trail".into() });
                assert_eq!(look(), Found::Hidden { full_name: "cedar/summit-trail".into(), install: client.installation_url() });
            },
        );
        assert!(records.iter().all(|record| record.line == "GET /repos/cedar/summit-trail HTTP/1.1"));
    }

    #[test]
    fn without_a_connection_nothing_is_asked_of_github() {
        let _apart = crate::auth::tests::Apart::new();
        let f = Fixture::new();
        f.git(&["remote", "add", "origin", "git@github.com:cedar/summit-trail.git"]);
        let before = f.git(&["config", "--local", "--list"]);
        let result = connect(&f.repo, "origin");
        assert!(matches!(result, Ok(None) | Err(_)), "{:?}", result.as_ref().map(|c| c.is_some()));
        crate::auth::configure("unconnected-client", "fastkraken-unconnected").unwrap();
        assert!(matches!(connect(&f.repo, "origin"), Ok(None)));
        assert_eq!(f.git(&["config", "--local", "--list"]), before);
        assert_eq!(fork_page("cedar/summit-trail"), "https://github.com/cedar/summit-trail/fork");
        assert_eq!(address_of("fern/summit-trail"), "https://github.com/fern/summit-trail.git");
    }

    #[test]
    fn an_added_fork_is_one_remote_bound_to_the_account_and_a_changed_one_is_refused() {
        let _apart = crate::auth::tests::Apart::new();
        let f = Fixture::new();
        let client = crate::auth::configure("fork-client", "fastkraken-fork").unwrap();
        crate::auth::save(&client, 11, &Tokens { access_token: "fork-fixture".into(), refresh_token: None, expires_at: None }).unwrap();
        let origin = "https://github.com/cedar/summit-trail.git";
        f.git(&["remote", "add", "origin", origin]);
        crate::auth::bind(&f.repo, "origin", origin, &client, 11).unwrap();
        let connection = connect(&f.repo, "origin").unwrap().unwrap();
        assert_eq!((connection.account, connection.token.as_str()), (11, "fork-fixture"));
        let url = address_of("fern/summit-trail");
        let refs = f.git(&["for-each-ref"]);
        pulls::add_fork_remote(&f.repo, "fern", &url).unwrap();
        crate::auth::bind(&f.repo, "fern", &url, &client, 11).unwrap();
        assert_eq!(f.git(&["config", "--get-all", "remote.fern.url"]), format!("{url}\n").as_bytes());
        assert_eq!(crate::auth::bound(&f.repo, "fern").unwrap().as_deref(), Some(url.as_str()));
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
        let refused = pulls::add_fork_remote(&f.repo, "fern", &address_of("fern/other")).unwrap_err();
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(f.git(&["config", "--get-all", "remote.fern.url"]), format!("{url}\n").as_bytes());
        f.git(&["remote", "set-url", "fern", "https://github.com/fern/moved.git"]);
        assert!(crate::auth::bind(&f.repo, "fern", &url, &client, 11).is_err());
    }
}
