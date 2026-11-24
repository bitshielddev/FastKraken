use crate::{
    Tab,
    notice::Outcome,
    prompt::{self, Answer, Field, Spec},
    remotes::{self, Addresses},
    view::{self, Ui},
};
use gpui::{Context, Window};
use url::Url;

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target, busy, operating};
    fn remote<'a>(ui: &'a Ui, target: &'a Target) -> Option<(&'a String, Option<&'a Addresses>)> {
        match target {
            Target::Remote(name) => Some((name, ui.refs.addresses.get(name))),
            _ => None,
        }
    }
    fn fetch<'a>(ui: &'a Ui, target: &'a Target) -> Option<&'a str> {
        remote(ui, target)?.1?.single()
    }
    fn web<'a>(ui: &'a Ui, target: &'a Target) -> Option<String> {
        web_address(fetch(ui, target)?)
    }
    &[
        Command::new(
            "remote.edit-address",
            Group::Remotes,
            |_, _| "Edit address…".into(),
            |ui, target, _| {
                let Some((name, addresses)) = remote(ui, target) else {
                    return Availability::Hidden;
                };
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| uneditable(name, addresses)).or_else(|| {
                    ui.prompt.open().then(|| "Close the open dialog first".to_owned())
                }))
            },
            |_, target| match target {
                Target::Remote(name) => Some(view::Action::Address(Action::Edit(name.clone()))),
                _ => None,
            },
        )
        .words(&["url", "change url", "set-url", "push url", "repoint"]),
        Command::new(
            "remote.open",
            Group::Open,
            |ui, target| match web(ui, target).as_deref().and_then(host_of) {
                Some(host) => format!("Open on {host}"),
                None => String::new(),
            },
            |ui, target, _| if web(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| Some(view::Action::Handoff(crate::handoff::Action::OpenUrl(web(ui, target)?))),
        )
        .words(&["browser", "website", "view on", "github", "gitlab", "bitbucket"])
        .icon("globe"),
        Command::new(
            "remote.copy-address",
            Group::Copy,
            |_, _| "Copy address".into(),
            |ui, target, _| if fetch(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| {
                Some(view::Action::CopyText { text: remotes::shown(fetch(ui, target)?), what: "address" })
            },
        )
        .words(&["clipboard", "url", "link to remote"])
        .icon("copy"),
    ]
};

fn uneditable(name: &str, addresses: Option<&Addresses>) -> Option<String> {
    let addresses = addresses.filter(|addresses| !addresses.fetch.is_empty());
    match addresses {
        None => Some(format!("{name} has no address to edit")),
        Some(addresses) if addresses.single().is_none() || addresses.separate_push().is_err() => {
            Some(format!("{name} has more than one address. Change it with git remote set-url"))
        }
        Some(_) => None,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Edit(String),
}

#[derive(Debug)]
pub(crate) struct Ask {
    name: String,
    seen: Addresses,
    bound: bool,
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(ask)
}

fn fetch_problem(text: &str) -> Option<String> {
    remotes::validate_url(text.trim()).err()
}

fn push_problem(text: &str) -> Option<String> {
    match text.trim() {
        "" => None,
        text => remotes::validate_url(text).err(),
    }
}

fn spec(ask: &Ask) -> Spec {
    let fetch = ask.seen.single().map(remotes::shown).unwrap_or_default();
    let push = ask.seen.separate_push().ok().flatten().map(remotes::shown).unwrap_or_default();
    let mut spec = Spec::new(format!("Edit the address of {}", ask.name), "Save address")
        .line("Fetches and pulls use the fetch address. Pushes use the push address, or the fetch address when that is left empty.");
    if ask.bound {
        spec = spec.line("Your GitHub connection signs in for the old address only. Use it again for the new one from the remote's menu.");
    }
    spec.field = Some(Field {
        label: "Fetch address",
        placeholder: "Repository URL or local path",
        initial: fetch,
        masked: false,
        validate: Box::new(|text, _| fetch_problem(text)),
    });
    spec.second = Some(Field {
        label: "Push address (optional)",
        placeholder: "The fetch address",
        initial: push,
        masked: false,
        validate: Box::new(|text, _| push_problem(text)),
    });
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    let Ask { name, seen, .. } = ask;
    let fetch = answer.text.trim().to_owned();
    let push = Some(answer.second.trim().to_owned()).filter(|push| !push.is_empty() && *push != fetch);
    tab.run_operation_then(
        cx,
        "Changing the address…",
        "Address changed",
        false,
        move |repo| {
            let changed = remotes::set_address(&repo, &name, &seen, &fetch, push.as_deref())
                .map_err(|error| format!("Address Not Changed — {error}"))?;
            Ok::<_, String>(match (changed, push) {
                (false, _) => Outcome::titled("Nothing changed", format!("{name} already has these addresses.")),
                (true, None) => Outcome::titled(format!("{name} now uses {fetch}"), "Fetches, pulls and pushes go there."),
                (true, Some(push)) => {
                    Outcome::titled(format!("{name} now fetches from {fetch}"), format!("Pushes go to {push}."))
                }
            })
        },
        |_, _| {},
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() || tab.ui.prompt.open() {
        return;
    }
    if tab.ui.busy() {
        tab.refuse("Wait for the current operation to finish".to_owned(), cx);
        return;
    }
    match action {
        Action::Edit(name) => {
            let addresses = tab.ui.refs.addresses.get(&name);
            if let Some(reason) = uneditable(&name, addresses) {
                tab.ui.say(view::Tone::Error, format!("{reason}."));
                cx.notify();
                return;
            }
            let seen = addresses.cloned().unwrap_or_default();
            let bound = signed_in(&seen, &tab.ui.refs.github, &name);
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Address(Ask { name, seen, bound }), cx);
        }
    }
}

fn signed_in(seen: &Addresses, unbound: &[String], name: &str) -> bool {
    seen.single().is_some_and(|url| crate::auth::github_repo(url).is_some())
        && seen.separate_push() == Ok(None)
        && !unbound.iter().any(|remote| remote == name)
}

pub(crate) fn web_address(url: &str) -> Option<String> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (host, path) = match url.split_once("://") {
        Some((scheme, _)) => {
            let parsed = Url::parse(url).ok()?;
            if parsed.query().is_some() || parsed.fragment().is_some() {
                return None;
            }
            let host = parsed.host_str()?.to_ascii_lowercase();
            match (scheme, parsed.port()) {
                ("https", Some(port)) => (format!("{host}:{port}"), parsed.path().to_owned()),
                ("https" | "ssh", _) => (host, parsed.path().to_owned()),
                _ => return None,
            }
        }
        None => {
            let (left, path) = url.split_once(':')?;
            if left.is_empty() || left.contains(['/', '[', ']']) || url.starts_with('/') || url.starts_with('.') {
                return None;
            }
            let host = left.rsplit_once('@').map_or(left, |(_, host)| host);
            (host.to_ascii_lowercase(), format!("/{path}"))
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    if host.is_empty() || path.is_empty() || path.starts_with('~') || path.contains(['?', '#', '\\']) {
        return None;
    }
    let (host, path) = match host.as_str() {
        "ssh.github.com" => ("github.com".to_owned(), path.to_owned()),
        "altssh.gitlab.com" => ("gitlab.com".to_owned(), path.to_owned()),
        "altssh.bitbucket.org" => ("bitbucket.org".to_owned(), path.to_owned()),
        "ssh.dev.azure.com" => {
            let parts: Vec<&str> = path.split('/').collect();
            match parts.as_slice() {
                ["v3", team, project, repo] => ("dev.azure.com".to_owned(), format!("{team}/{project}/_git/{repo}")),
                _ => return None,
            }
        }
        _ => (host, path.to_owned()),
    };
    let address = format!("https://{host}/{path}");
    crate::handoff::openable(&address).then_some(address)
}

fn host_of(address: &str) -> Option<&str> {
    address.strip_prefix("https://")?.split('/').next().filter(|host| !host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    #[test]
    fn hosted_addresses_open_their_repository_page() {
        for (url, page) in [
            ("https://github.com/team/trail.git", "https://github.com/team/trail"),
            ("https://github.com/team/trail", "https://github.com/team/trail"),
            ("https://github.com/team/trail/", "https://github.com/team/trail"),
            ("https://cedar@bitbucket.org/team/trail.git", "https://bitbucket.org/team/trail"),
            ("https://dev-team@dev.azure.com/team/project/_git/trail", "https://dev.azure.com/team/project/_git/trail"),
            ("https://git.example.invalid:8443/team/trail.git", "https://git.example.invalid:8443/team/trail"),
            ("https://GitLab.com/group/sub/trail.git", "https://gitlab.com/group/sub/trail"),
            ("git@github.com:team/trail.git", "https://github.com/team/trail"),
            ("github.com:team/trail", "https://github.com/team/trail"),
            ("ssh://git@github.com/team/trail.git", "https://github.com/team/trail"),
            ("ssh://git@git.example.invalid:2222/team/trail.git", "https://git.example.invalid/team/trail"),
            ("ssh://git@ssh.github.com:443/team/trail.git", "https://github.com/team/trail"),
            ("git@ssh.dev.azure.com:v3/team/project/trail", "https://dev.azure.com/team/project/_git/trail"),
        ] {
            assert_eq!(web_address(url).as_deref(), Some(page), "{url}");
        }
        for url in [
            "/srv/git/trail.git",
            "../peer.git",
            "./peer.git",
            "file:///srv/git/trail.git",
            "git://git.example.invalid/trail.git",
            "http://127.0.0.1:8080/trail.git",
            "https://github.com/",
            "https://github.com/.git",
            "git@github.com:",
            "git@host:~/trail.git",
            "git@[::1]:team/trail.git",
            "https://github.com/team/trail?x=1",
            "git@github.com:team/tr ail.git",
            "git@ssh.dev.azure.com:team/trail",
        ] {
            assert_eq!(web_address(url), None, "{url}");
        }
        assert_eq!(host_of("https://github.com/team/trail"), Some("github.com"));
    }

    #[test]
    fn a_copied_address_never_carries_a_password() {
        assert_eq!(remotes::shown("https://cedar:hidden@git.example.invalid/trail.git"), "https://cedar@git.example.invalid/trail.git");
        assert_eq!(remotes::shown("https://cedar@git.example.invalid/trail.git"), "https://cedar@git.example.invalid/trail.git");
        assert_eq!(remotes::shown("git@github.com:team/trail.git"), "git@github.com:team/trail.git");
        assert_eq!(remotes::shown("https://ghp_hidden0123@github.com/team/trail.git"), "https://github.com/team/trail.git");
    }

    fn addresses(f: &Fixture, name: &str) -> Addresses {
        remotes::addresses(&f.repo).unwrap().remove(name).unwrap_or_default()
    }

    #[test]
    fn editing_an_address_sets_the_fetch_and_push_addresses_and_drops_a_github_binding() {
        let _apart = crate::auth::tests::Apart::new();
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let [first, second, third] = ["first.git", "second.git", "third.git"].map(|name| dir.path().join(name).to_str().unwrap().to_owned());
        f.git(&["remote", "add", "origin", &first]);
        f.git(&["remote", "add", "other", &first]);
        let refs = || f.git(&["for-each-ref"]);
        let before = refs();
        let seen = addresses(&f, "origin");
        assert_eq!(seen, Addresses { fetch: vec![first.clone()], push: vec![] });
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &first, None), Ok(false));
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &first, Some(&first)), Ok(false));
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &second, None), Ok(true));
        assert_eq!(f.git(&["remote", "get-url", "origin"]), format!("{second}\n").as_bytes());
        assert_eq!(f.git(&["remote", "get-url", "--push", "origin"]), format!("{second}\n").as_bytes());
        let seen = addresses(&f, "origin");
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &second, Some(&third)), Ok(true));
        assert_eq!(f.git(&["remote", "get-url", "--push", "origin"]), format!("{third}\n").as_bytes());
        let seen = addresses(&f, "origin");
        assert_eq!(seen.separate_push(), Ok(Some(third.as_str())));
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &first, None), Ok(true));
        assert_eq!(addresses(&f, "origin"), Addresses { fetch: vec![first.clone()], push: vec![] });
        assert_eq!(addresses(&f, "other").fetch, [first.clone()]);
        assert_eq!(refs(), before);
        assert!(f.git(&["status", "--porcelain"]).is_empty());

        let github = "https://github.com/team/trail.git";
        f.git(&["remote", "set-url", "origin", github]);
        let client = crate::github::Client::for_test("http://127.0.0.1:9/");
        crate::auth::bind(&f.repo, "origin", github, &client, 11).unwrap();
        assert_eq!(crate::auth::bound(&f.repo, "origin").unwrap().as_deref(), Some(github));
        let seen = addresses(&f, "origin");
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, "git@github.com:team/trail.git", None), Ok(true));
        assert_eq!(crate::auth::bound(&f.repo, "origin").unwrap(), None);
    }

    #[test]
    fn an_address_that_changed_since_the_prompt_opened_is_not_overwritten() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.git").to_str().unwrap().to_owned();
        let moved = dir.path().join("moved.git").to_str().unwrap().to_owned();
        let wanted = dir.path().join("wanted.git").to_str().unwrap().to_owned();
        f.git(&["remote", "add", "origin", &first]);
        let seen = addresses(&f, "origin");
        f.git(&["remote", "set-url", "origin", &moved]);
        let refused = remotes::set_address(&f.repo, "origin", &seen, &wanted, None).unwrap_err();
        assert_eq!(refused, "origin's address changed since you opened it. Look at it again.");
        assert_eq!(f.git(&["remote", "get-url", "origin"]), format!("{moved}\n").as_bytes());
        let seen = addresses(&f, "origin");
        f.git(&["config", "remote.origin.pushurl", &first]);
        assert!(remotes::set_address(&f.repo, "origin", &seen, &wanted, None).is_err());
        assert_eq!(f.git(&["config", "--get", "remote.origin.pushurl"]), format!("{first}\n").as_bytes());
        let seen = addresses(&f, "origin");
        f.git(&["remote", "remove", "origin"]);
        assert_eq!(remotes::set_address(&f.repo, "origin", &seen, &wanted, None).unwrap_err(), "origin is not a remote of this repository any more.");
        f.git(&["remote", "add", "origin", &first]);
        f.git(&["config", "--add", "remote.origin.url", &moved]);
        let two = addresses(&f, "origin");
        assert!(uneditable("origin", Some(&two)).is_some());
        assert!(remotes::set_address(&f.repo, "origin", &two, &wanted, None).is_err());
        f.git(&["config", "--unset", "remote.origin.url", &moved]);
        let seen = addresses(&f, "origin");
        assert!(remotes::set_address(&f.repo, "origin", &seen, "https://cedar:secret@git.example.invalid/trail.git", None).is_err());
        assert!(remotes::set_address(&f.repo, "--origin", &seen, &wanted, None).is_err());
        assert_eq!(f.git(&["remote", "get-url", "--all", "origin"]), format!("{first}\n").as_bytes());
        assert_eq!(uneditable("origin", None), Some("origin has no address to edit".to_owned()));
    }

    #[test]
    fn the_edit_prompt_says_the_connection_is_dropped_only_for_a_remote_it_signs_in_for() {
        let github = "https://github.com/team/trail.git".to_owned();
        let one = Addresses { fetch: vec![github.clone()], push: vec![] };
        assert!(signed_in(&one, &[], "origin"));
        assert!(!signed_in(&one, &["origin".to_owned()], "origin"));
        assert!(signed_in(&Addresses { fetch: vec![github.clone()], push: vec![github.clone()] }, &[], "origin"));
        assert!(!signed_in(&Addresses { fetch: vec![github], push: vec!["git@github.com:team/trail.git".into()] }, &[], "origin"));
        assert!(!signed_in(&Addresses { fetch: vec!["git@github.com:team/trail.git".into()], push: vec![] }, &[], "origin"));
    }

    #[test]
    fn the_edit_prompt_shows_both_addresses_without_a_password() {
        let ask = Ask {
            name: "origin".into(),
            seen: Addresses { fetch: vec!["https://cedar:hidden@git.example.invalid/trail.git".into()], push: vec!["git@git.example.invalid:trail.git".into()] },
            bound: false,
        };
        let spec = spec(&ask);
        assert_eq!((spec.title.as_str(), spec.confirm.as_ref(), spec.danger), ("Edit the address of origin", "Save address", false));
        let (field, second) = (spec.field.as_ref().unwrap(), spec.second.as_ref().unwrap());
        assert_eq!((field.label, field.initial.as_str()), ("Fetch address", "https://cedar@git.example.invalid/trail.git"));
        assert_eq!((second.label, second.initial.as_str()), ("Push address (optional)", "git@git.example.invalid:trail.git"));
        assert_eq!(push_problem(""), None);
        assert!(push_problem("ext::sh -c touch").is_some());
        assert!(fetch_problem("").is_some());
        assert!(fetch_problem("https://cedar:pw@git.example.invalid/trail.git").is_some());
        assert_eq!(fetch_problem("https://cedar@git.example.invalid/trail.git"), None);
    }
}
