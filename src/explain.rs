use crate::{
    Tab,
    notice::{Failure, Notice, Offer, Outcome},
    prompt::{self, Answer, Spec},
    repository::Repository,
    view::{self, Tone, Ui},
};
use gpui::{Context, SharedString, Window};
use std::path::{Path, PathBuf};

const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";

#[derive(Clone, Debug, Default)]
pub(crate) struct About {
    pub remote: Option<String>,
    pub url: Option<String>,
    pub again: Option<view::Action>,
    pub push: bool,
    pub local: bool,
    pub root: Option<PathBuf>,
    pub github: Option<crate::connect::Hint>,
    pub cloning: bool,
}

impl About {
    pub(crate) fn new(remote: Option<String>, again: Option<view::Action>) -> Self {
        Self { remote, url: None, again: again.filter(repeatable), push: false, local: false, root: None, github: None, cloning: false }
    }

    pub(crate) fn explain(mut self, repo: &Repository, failure: Failure) -> Failure {
        self.root.get_or_insert_with(|| repo.root.clone());
        if failure.plain.is_none() && !self.local {
            let said = everything(&failure);
            let cause = classify(&said);
            if self.url.is_none() && cause.is_some() && host_in(&said).is_none() {
                self.url = self.remote.as_deref().and_then(|remote| address(repo, remote, self.push));
            }
            if cause == Some(Cause::SignIn) && self.github.is_none() {
                self.github = crate::connect::hint(repo, self.remote.as_deref(), &said);
            }
        }
        explain(failure, &self)
    }
}

fn address(repo: &Repository, remote: &str, push: bool) -> Option<String> {
    let read = |key: String| {
        let bytes = crate::repository::checked(repo.read_command().args(["config", "--get", &key])).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        text.lines().next().map(str::to_owned).filter(|line| !line.is_empty())
    };
    push.then(|| read(format!("remote.{remote}.pushurl"))).flatten().or_else(|| read(format!("remote.{remote}.url")))
}

pub(crate) fn repeatable(action: &view::Action) -> bool {
    use crate::sync::Action as S;
    use view::Action as A;
    match action {
        A::Sync(action) => matches!(
            action,
            S::FetchAll | S::FetchRemote(_) | S::Push | S::Pull | S::Button | S::PullWith(_) | S::PushBranch { .. } | S::PullBranch { .. } | S::FastForward { .. }
        ),
        A::Tag(action) => matches!(action, crate::tag::Action::Push { .. }),
        A::Refs(action) => matches!(action, crate::refs::Action::Switch(_)),
        A::Clone(action) => matches!(action, crate::clone::Action::Again(_)),
        A::Commit | A::CommitAll | A::StageAll | A::UnstageAll | A::StagePath(_) | A::UnstagePath(_) => true,
        A::Picked(action) => action.stages(),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    Lapsed,
    SignIn,
    PublicKey,
    NoRepository,
    Unreachable,
    Stalled,
    Certificate,
    HostKeyChanged { host: Option<String>, file: Option<PathBuf> },
    UnknownHost,
    KeyOpen { key: String },
    KeyUnreadable { key: String },
    KeyMissing { key: String },
    Protected { branch: Option<String> },
    Declined { lines: Vec<String> },
    InTheWay { files: Vec<String>, untracked: bool },
    Locked { lock: Option<String> },
    Dubious { command: Option<String> },
    Signing { missing: Option<String> },
    Identity,
    NotFound { hook: Option<String>, command: Option<String> },
    NoRemote,
    HookSkipped { hook: String, path: String },
    InWorktree { branch: Option<String>, path: Option<String> },
}

impl Cause {
    fn network(&self) -> bool {
        matches!(
            self,
            Cause::Lapsed
                | Cause::SignIn
                | Cause::PublicKey
                | Cause::NoRepository
                | Cause::Unreachable
                | Cause::Stalled
                | Cause::Certificate
                | Cause::HostKeyChanged { .. }
                | Cause::UnknownHost
                | Cause::KeyOpen { .. }
                | Cause::KeyUnreadable { .. }
                | Cause::KeyMissing { .. }
                | Cause::Protected { .. }
                | Cause::Declined { .. }
        )
    }
}

pub(crate) fn classify(said: &str) -> Option<Cause> {
    let lower = said.to_lowercase();
    let has = |pattern: &str| lower.contains(&pattern.to_lowercase());
    Some(if has("would be overwritten by") {
        let (files, untracked) = in_the_way(said);
        Cause::InTheWay { files, untracked }
    } else if has(".lock': File exists") {
        Cause::Locked { lock: between(said, "Unable to create '", "': File exists") }
    } else if has("detected dubious ownership") {
        Cause::Dubious { command: trust_command(said) }
    } else if has("Please tell me who you are")
        || has("auto-detection is disabled")
        || has("unable to auto-detect email address")
        || has("empty ident name")
    {
        Cause::Identity
    } else if has("gpg failed to sign the data") || has("failed to write commit object") && has("cannot run ") {
        Cause::Signing { missing: between(said, "cannot run ", ": No such file or directory") }
    } else if let Some((hook, command)) = not_found(said) {
        Cause::NotFound { hook, command }
    } else if has("FastKraken credentials: ") && has(crate::auth::LAPSED) {
        Cause::Lapsed
    } else if has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        Cause::HostKeyChanged { host: changed_host(said), file: known_hosts_in(said) }
    } else if has("UNPROTECTED PRIVATE KEY FILE") {
        Cause::KeyOpen { key: between(said, "for '", "' are too open").unwrap_or_default() }
    } else if has("Load key \"") && has("invalid format") {
        Cause::KeyUnreadable { key: between(said, "Load key \"", "\"").unwrap_or_default() }
    } else if has("no such identity: ") {
        Cause::KeyMissing { key: between(said, "no such identity: ", ": No such file").unwrap_or_default() }
    } else if has("Host key verification failed") || has("and you have requested strict checking") {
        Cause::UnknownHost
    } else if has("Permission denied (publickey") {
        Cause::PublicKey
    } else if has("SSL certificate problem") || has("server certificate verification failed") {
        Cause::Certificate
    } else if has("Operation too slow") {
        Cause::Stalled
    } else if [
        "Could not resolve host",
        "Could not resolve hostname",
        "Couldn't connect to server",
        "Failed to connect to",
        "Connection refused",
        "timed out",
        "Network is unreachable",
        "No route to host",
    ]
    .iter()
    .any(|pattern| has(pattern))
    {
        Cause::Unreachable
    } else if [
        "Authentication failed",
        "could not read Username",
        "could not read Password",
        "terminal prompts disabled",
        "The requested URL returned error: 401",
        "The requested URL returned error: 403",
        "HTTP Basic: Access denied",
    ]
    .iter()
    .any(|pattern| has(pattern))
    {
        Cause::SignIn
    } else if has("Repository not found")
        || has("does not appear to be a git repository")
        || has("repository '") && (has("' not found") || has("' does not exist"))
    {
        Cause::NoRepository
    } else if has("protected branch") && (has("remote rejected") || has("declined") || has("not allowed")) {
        Cause::Protected { branch: rejected_branch(said) }
    } else if has("hook declined") {
        Cause::Declined { lines: remote_lines(said) }
    } else if has("' is already used by worktree at '") || has("' is already checked out at '") {
        let markers = ["' is already used by worktree at '", "' is already checked out at '"];
        let path = markers.iter().find_map(|marker| between(said, marker, "'"));
        let branch = markers.iter().find_map(|marker| {
            let before = &said[..said.find(marker)?];
            let named = &before[before.rfind('\'')? + 1..];
            (!named.is_empty() && !named.contains('\n')).then(|| named.to_owned())
        });
        Cause::InWorktree { branch, path }
    } else if has("No such remote") || has("No configured push destination") || has("No remotes are configured") {
        Cause::NoRemote
    } else if let Some((hook, path)) = skipped_hooks(said).into_iter().next() {
        Cause::HookSkipped { hook, path }
    } else {
        return None;
    })
}

const HOOKS: &[&str] = &[
    "applypatch-msg", "pre-applypatch", "post-applypatch", "pre-commit", "pre-merge-commit",
    "prepare-commit-msg", "commit-msg", "post-commit", "pre-rebase", "post-checkout", "post-merge",
    "pre-push", "pre-receive", "update", "proc-receive", "post-receive", "post-update",
    "reference-transaction", "push-to-checkout", "pre-auto-gc", "post-rewrite", "sendemail-validate",
    "fsmonitor-watchman", "post-index-change",
];

fn hook_named(path: &str) -> Option<String> {
    let name = Path::new(path.trim()).file_name()?.to_str()?;
    HOOKS.contains(&name).then(|| name.to_owned())
}

pub(crate) fn skipped_hooks(said: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in said.lines() {
        let Some(path) = between(line, "The '", "' hook was ignored because it's not set as executable") else {
            continue;
        };
        if let Some(hook) = hook_named(&path) {
            if !found.iter().any(|(_, seen)| *seen == path) {
                found.push((hook, path));
            }
        }
    }
    found
}

fn not_found(said: &str) -> Option<(Option<String>, Option<String>)> {
    if said.contains("configured for Git LFS but 'git-lfs' was not found") {
        return Some((between(said, "deleting the '", "' file").and_then(|hook| hook_named(&hook)), Some("git-lfs".to_owned())));
    }
    let program = |command: &str| {
        let command = command.trim();
        let server = ["git-upload-pack", "git-receive-pack", "git-upload-archive"].iter().any(|name| command.ends_with(name));
        (!command.is_empty() && !server && !command.contains(|c: char| c.is_whitespace() || "'\"`$;".contains(c)))
            .then(|| command.to_owned())
    };
    let place = |place: &str| {
        let file = place.split(": ").next().unwrap_or(place);
        let file = match file.rsplit_once(':') {
            Some((file, line)) if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) => file,
            _ => file,
        };
        hook_named(file)
    };
    for line in said.lines() {
        let line = line.rsplit_once(" — ").map_or(line, |(_, said)| said).trim();
        if line.starts_with("remote:") {
            continue;
        }
        let found = if let Some((at, command)) = line.split_once(": command not found: ") {
            program(command).map(|command| (place(at), Some(command)))
        } else if let Some(rest) = line.strip_suffix(": command not found").or_else(|| line.strip_suffix(": not found")) {
            rest.rsplit_once(": ").and_then(|(at, command)| program(command).map(|command| (place(at), Some(command))))
        } else if let Some(command) = line.strip_prefix("env: ").and_then(|rest| rest.strip_suffix(": No such file or directory")) {
            program(command).map(|command| (None, Some(command)))
        } else if let Some(path) = between(line, "cannot exec '", "': No such file or directory") {
            hook_named(&path).map(|hook| (Some(hook), None))
        } else {
            None
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn trust_command(said: &str) -> Option<String> {
    let marker = "detected dubious ownership in repository at '";
    let line = said.lines().find(|line| line.contains(marker))?;
    let folder = line.split_once(marker)?.1.trim_end().strip_suffix('\'')?;
    folder.starts_with('/').then(|| format!("git config --global --add safe.directory {}", quoted(folder)))
}

fn between(said: &str, start: &str, end: &str) -> Option<String> {
    let at = said.find(start)? + start.len();
    let rest = &said[at..];
    let stop = rest.find(end)?;
    let found = &rest[..stop];
    (!found.contains('\n') && !found.is_empty()).then(|| found.to_owned())
}

fn changed_host(said: &str) -> Option<String> {
    let lower = said.to_lowercase();
    let at = lower.find("host key for ")? + "host key for ".len();
    let rest = &said[at..];
    let host = rest.split_whitespace().next()?;
    rest.contains(" has changed").then(|| host.to_owned())
}

fn known_hosts_in(said: &str) -> Option<PathBuf> {
    let line = said.lines().find(|line| line.trim_start().starts_with("Offending ") && line.contains(" in "))?;
    let (_, place) = line.rsplit_once(" in ")?;
    let place = place.trim();
    let file = match place.rsplit_once(':') {
        Some((file, number)) if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) => file,
        _ => place,
    };
    (!file.is_empty()).then(|| PathBuf::from(file))
}

fn rejected_branch(said: &str) -> Option<String> {
    for line in said.lines() {
        let line = line.trim().trim_start_matches('!').trim();
        if let Some(rest) = line.strip_prefix("[remote rejected]") {
            if let Some((_, to)) = rest.split_once("->") {
                let branch = to.trim().split(" (").next().unwrap_or_default().trim();
                if !branch.is_empty() {
                    return Some(branch.to_owned());
                }
            }
        }
        if let Some((branch, _)) = line.split_once(": [remote rejected]") {
            return Some(branch.trim().to_owned());
        }
    }
    None
}

fn remote_lines(said: &str) -> Vec<String> {
    said.split(['\n', '\r'])
        .filter(|line| !crate::jobs::is_progress(line))
        .filter_map(|line| line.trim_start().strip_prefix("remote:"))
        .map(|line| crate::activity::redact(line.trim()))
        .filter(|line| !line.is_empty())
        .take(6)
        .collect()
}

fn in_the_way(said: &str) -> (Vec<String>, bool) {
    let mut files = Vec::new();
    let mut untracked = false;
    let mut listing = false;
    for line in said.lines() {
        if line.contains("would be overwritten by") {
            listing = true;
            untracked |= line.contains("untracked working tree files");
            continue;
        }
        if !listing {
            continue;
        }
        if line.starts_with(['\t', ' ']) && !line.trim().is_empty() {
            files.push(line.trim().to_owned());
        } else {
            listing = false;
        }
    }
    files.dedup();
    (files, untracked)
}

pub(crate) fn host_in(said: &str) -> Option<String> {
    let word = |after: &str| -> Option<String> {
        let at = said.find(after)? + after.len();
        let host = said[at..]
            .split(|c: char| c.is_whitespace() || c == ':' || c == '\'')
            .next()?
            .trim_end_matches(['.', ',']);
        (!host.is_empty()).then(|| host.to_owned())
    };
    if let Some(host) = changed_host(said) {
        return Some(host);
    }
    for after in [
        "Could not resolve host: ",
        "Could not resolve hostname ",
        "Failed to connect to ",
        "connect to host ",
        "host key is known for ",
    ] {
        if let Some(host) = word(after) {
            return Some(host);
        }
    }
    if let Some(line) = said.lines().find(|line| line.contains(": Permission denied")) {
        let who = line.split(": Permission denied").next().unwrap_or_default().trim();
        if let Some((_, host)) = who.rsplit_once('@') {
            if !host.is_empty() && !host.contains(char::is_whitespace) {
                return Some(host.to_owned());
            }
        }
    }
    for start in ["unable to access '", "Authentication failed for '", "repository '", "for '"] {
        if let Some(url) = between(said, start, "'") {
            if let Some(host) = host_of(&url) {
                return Some(host);
            }
        }
    }
    None
}

pub(crate) fn host_of(address: &str) -> Option<String> {
    if address.contains("://") {
        let url = url::Url::parse(address).ok()?;
        return url.host_str().filter(|host| !host.is_empty()).map(str::to_owned);
    }
    let colon = address.find(':')?;
    if address[..colon].contains('/') {
        return None;
    }
    let host = address[..colon].rsplit_once('@').map_or(&address[..colon], |(_, host)| host);
    (!host.is_empty()).then(|| host.to_owned())
}

fn everything(failure: &Failure) -> String {
    match &failure.output {
        Some(output) => format!("{}\n{output}", failure.text),
        None => failure.text.clone(),
    }
}

fn heading(text: &str) -> (Option<&str>, &str) {
    match text.split_once(" — ") {
        Some((title, rest)) if !title.contains('\n') && title.len() <= 40 => (Some(title), rest),
        _ => (None, text),
    }
}

fn sentence(title: &str) -> String {
    let mut words = title.split(' ');
    let first = words.next().unwrap_or_default().to_owned();
    let rest: Vec<String> = words.map(str::to_lowercase).collect();
    let mut said = std::iter::once(first).chain(rest).collect::<Vec<_>>().join(" ");
    said.push('.');
    said
}

fn name_of(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

fn listed(files: &[String]) -> String {
    match files {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, c, rest @ ..] => format!("{a}, {b}, {c} and {} more", rest.len()),
    }
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

pub(crate) fn key_settings(host: &str) -> Option<&'static str> {
    match host {
        "github.com" => Some("https://github.com/settings/keys"),
        "gitlab.com" => Some("https://gitlab.com/-/user_settings/ssh_keys"),
        "bitbucket.org" => Some("https://bitbucket.org/account/settings/ssh-keys/"),
        _ => None,
    }
}

fn words(cause: &Cause, host: &str, remote: &str, about: &About) -> (String, String, Vec<Offer>) {
    let retry = || about.again.clone().map(|action| (SharedString::from("Retry"), action)).into_iter().collect::<Vec<_>>();
    let copy = |label: &'static str, text: String| vec![(SharedString::from(label), view::Action::CopyText { text, what: "command" })];
    match cause {
        Cause::Lapsed => (
            "Your GitHub sign-in has lapsed".to_owned(),
            "Reconnect GitHub, then try again.".to_owned(),
            vec![reconnect(about)],
        ),
        Cause::SignIn => {
            let mut offers: Vec<Offer> = about.again.clone().map(|action| (SharedString::from("Try again"), action)).into_iter().collect();
            match &about.github {
                Some(crate::connect::Hint::Unbound { remote, url }) => offers.push((
                    format!("Use your GitHub connection for {remote}").into(),
                    view::Action::Connect(crate::connect::Action::Use(crate::connect::Bind {
                        remote: Some(remote.clone()),
                        url: Some(url.clone()),
                        again: about.again.clone().map(Box::new),
                    })),
                )),
                Some(crate::connect::Hint::Bound) => offers.push(reconnect(about)),
                None => {}
            }
            (
                format!("Sign-in to {host} failed"),
                match &about.github {
                    Some(crate::connect::Hint::Bound) => "GitHub did not accept your GitHub connection's sign-in. Reconnect GitHub.".to_owned(),
                    _ => "Check the account, and the password or token saved for it.".to_owned(),
                },
                offers,
            )
        }
        Cause::PublicKey => (
            format!("{host} did not accept any of your SSH keys"),
            "Add your public key to your account there, or check which key ssh offers it.".to_owned(),
            key_settings(host)
                .map(|url| (SharedString::from("Open SSH key settings"), view::Action::Handoff(crate::handoff::Action::OpenUrl(url.to_owned()))))
                .into_iter()
                .collect(),
        ),
        Cause::NoRepository if about.cloning => (
            "There is no repository you can reach at that address".to_owned(),
            "Check the address, and that this account has access.".to_owned(),
            about.again.iter().filter_map(|again| match again {
                view::Action::Clone(crate::clone::Action::Again(request)) => {
                    Some(("Change the address…".into(), view::Action::Clone(crate::clone::Action::Edit(request.clone()))))
                }
                _ => None,
            })
            .collect(),
        ),
        Cause::NoRepository => (
            format!("{remote} does not point at a repository you can reach"),
            "Check the address, and that this account has access.".to_owned(),
            match &about.remote {
                Some(name) => vec![("Edit address…".into(), view::Action::Address(crate::address::Action::Edit(name.clone())))],
                None => vec![("Manage remotes…".into(), view::Action::Publish(crate::publish::Action::Menu))],
            },
        ),
        Cause::Unreachable => (format!("Could not reach {host}"), "Check your connection.".to_owned(), retry()),
        Cause::Stalled => (
            format!("The connection to {host} stalled"),
            "Nothing arrived for 30 seconds. Check your connection.".to_owned(),
            retry(),
        ),
        Cause::Certificate => (
            format!("{host}'s certificate could not be verified"),
            "Check this Mac's date and time. A network that inspects secure connections can also cause this.".to_owned(),
            Vec::new(),
        ),
        Cause::HostKeyChanged { host: named, file } => {
            let named = named.clone().unwrap_or_else(|| host.to_owned());
            let file = file.clone().unwrap_or_else(default_known_hosts);
            (
                format!("{host}'s identity has changed since you last connected"),
                "This can mean the server was reinstalled, or that something is intercepting the connection.".to_owned(),
                vec![("Remove the saved key…".into(), view::Action::Explain(Action::ForgetHostKey { host: named, file }))],
            )
        }
        Cause::UnknownHost => (
            format!("{host} is not a known host"),
            "ssh has no saved key for it, so it did not connect. Connect again to review its key and trust it.".to_owned(),
            about.again.clone().map(|action| (SharedString::from("Connect and review"), action)).into_iter().collect(),
        ),
        Cause::KeyOpen { key } => (
            format!("Your SSH key {} can be read by other users", name_of(key)),
            "ssh does not use a key like that. Make it private, then try again.".to_owned(),
            if key.is_empty() { Vec::new() } else { copy("Copy the command", format!("chmod 600 {}", quoted(key))) },
        ),
        Cause::KeyUnreadable { key } => (
            format!("ssh could not read your key {}", name_of(key)),
            "It may be damaged, or not a private key this ssh can use.".to_owned(),
            Vec::new(),
        ),
        Cause::KeyMissing { key } => (
            format!("The SSH key {key} does not exist"),
            "Check the IdentityFile lines in your ssh configuration.".to_owned(),
            Vec::new(),
        ),
        Cause::Protected { branch } => (
            format!("{remote} does not allow pushing to {} directly", branch.as_deref().unwrap_or("this branch")),
            "Push to another branch and open a pull request instead.".to_owned(),
            Vec::new(),
        ),
        Cause::Declined { lines } => (
            format!("{remote} refused the push"),
            if lines.is_empty() { "A hook on the server declined it.".to_owned() } else { lines.join("\n") },
            Vec::new(),
        ),
        Cause::InTheWay { files: changed, untracked } => {
            let count = if changed.is_empty() { "some files".to_owned() } else { files(changed.len()) };
            let list = if changed.is_empty() { String::new() } else { format!("{}. ", listed(changed)) };
            if *untracked {
                (
                    format!("Untracked files are in the way: {count}"),
                    format!("{list}Move or delete them, or commit them first."),
                    Vec::new(),
                )
            } else {
                (format!("Your changes to {count} are in the way"), format!("{list}Commit or stash them first."), Vec::new())
            }
        }
        Cause::Locked { lock } => (
            "Another Git process is using this repository".to_owned(),
            match lock {
                Some(lock) => format!("Wait for it to finish and retry. If no Git is running, delete {lock}."),
                None => "Wait for it to finish and retry.".to_owned(),
            },
            retry(),
        ),
        Cause::Dubious { command } => (
            "Git does not trust this folder because another user owns it".to_owned(),
            match command {
                Some(_) => "If you trust it, run the command Git suggests in Terminal.".to_owned(),
                None => "If you trust it, add it to Git's safe.directory setting.".to_owned(),
            },
            command.clone().map(|command| copy("Copy the command that trusts it", command)).unwrap_or_default(),
        ),
        Cause::Identity => (
            "Git needs your name and email".to_owned(),
            "Set the name and email your commits carry, then try again.".to_owned(),
            vec![("Set name and email…".into(), view::Action::Identity(crate::identity::Action::Edit))],
        ),
        Cause::Signing { missing: None } => (
            "The commit could not be signed".to_owned(),
            "GPG could not sign it with your key. The activity log has what it said.".to_owned(),
            Vec::new(),
        ),
        Cause::Signing { missing: Some(program) } => (
            "The commit could not be signed".to_owned(),
            format!("Git could not find {program}, which signs your commits. {ON_THE_PATH}"),
            retry(),
        ),
        Cause::NotFound { hook, command: Some(command) } if command == "git-lfs" => (
            format!("{} could not find Git LFS", hook.as_deref().unwrap_or("Git")),
            format!("{} {ON_THE_PATH}", crate::lfs::NOT_INSTALLED),
            std::iter::once((
                SharedString::from(crate::lfs::INSTALL_LABEL),
                view::Action::Handoff(crate::handoff::Action::OpenUrl(crate::lfs::INSTALL_PAGE.to_owned())),
            ))
            .chain(retry())
            .collect(),
        ),
        Cause::NotFound { hook, command: Some(command) } => (
            format!("{} could not find {command}", hook.as_deref().unwrap_or("Git")),
            format!("{command} is not in any folder on the PATH your login shell sets up. {ON_THE_PATH}"),
            retry(),
        ),
        Cause::NotFound { hook, command: None } => (
            format!("{} could not start", hook.as_deref().unwrap_or("A hook")),
            "The program its first line names is not installed, or not where that line says.".to_owned(),
            Vec::new(),
        ),
        Cause::NoRemote => (
            "This repository has no remote".to_owned(),
            "Add one to fetch, pull and push.".to_owned(),
            vec![("Add remote…".into(), view::Action::Publish(crate::publish::Action::Add))],
        ),
        Cause::InWorktree { branch, path } => {
            let folder = path.as_deref().and_then(|path| Path::new(path).file_name()).map(|name| name.to_string_lossy().into_owned());
            let title = match (branch, &folder) {
                (Some(branch), Some(folder)) => format!("{branch} is checked out in worktree {folder}"),
                (Some(branch), None) => format!("{branch} is checked out in another worktree"),
                (None, _) => "That branch is checked out in another worktree".to_owned(),
            };
            let offers = path
                .iter()
                .map(|path| (SharedString::from("Open that worktree"), view::Action::Worktree(crate::worktree::Action::Open(PathBuf::from(path)))))
                .collect();
            (title, "Git keeps a branch in one worktree at a time.".to_owned(), offers)
        }
        Cause::HookSkipped { hook, path } => (
            format!("{hook} is not executable, so Git skipped it"),
            "Make it executable so that Git runs it.".to_owned(),
            copy("Copy the command", chmod(&[in_repository(path, about.root.as_deref())])),
        ),
    }
}

fn reconnect(about: &About) -> Offer {
    (
        SharedString::from("Reconnect GitHub…"),
        view::Action::Connect(crate::connect::Action::Reconnect { again: about.again.clone().map(Box::new) }),
    )
}

const ON_THE_PATH: &str = "Install it and retry. If it is installed, add its folder to PATH in your shell's login files, then reopen FastKraken.";

fn in_repository(path: &str, root: Option<&Path>) -> PathBuf {
    match root {
        Some(root) => root.join(path),
        None => PathBuf::from(path),
    }
}

fn chmod(paths: &[PathBuf]) -> String {
    let paths: Vec<String> = paths.iter().map(|path| quoted(&path.to_string_lossy())).collect();
    format!("chmod +x {}", paths.join(" "))
}

pub(crate) fn skipped(mut outcome: Outcome, hooks: &[PathBuf]) -> Outcome {
    if hooks.is_empty() {
        return outcome;
    }
    let names: Vec<String> = hooks.iter().map(|hook| name_of(&hook.to_string_lossy())).collect();
    let note = match names.as_slice() {
        [one] => format!("{one} is not executable, so Git skipped it."),
        _ => format!("{} are not executable, so Git skipped them.", listed(&names)),
    };
    outcome.detail = if outcome.detail.is_empty() { note } else { format!("{}\n{note}", outcome.detail) };
    outcome.offer("Copy the command", view::Action::CopyText { text: chmod(hooks), what: "command" })
}

fn quoted(path: &str) -> String {
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

pub(crate) fn explain(mut failure: Failure, about: &About) -> Failure {
    if failure.plain.is_some() {
        return failure;
    }
    let said = everything(&failure);
    let Some(cause) = classify(&said).filter(|cause| !(about.local && cause.network())) else { return failure };
    let host = host_in(&said)
        .or_else(|| about.url.as_deref().and_then(host_of))
        .or_else(|| about.remote.clone())
        .unwrap_or_else(|| "the remote".to_owned());
    let remote = about
        .remote
        .clone()
        .or_else(|| between(&said, "'", "' does not appear to be a git repository").filter(|name| !name.contains(['/', ':'])))
        .unwrap_or_else(|| "The remote".to_owned());
    let (title, rest, offers) = words(&cause, &host, &remote, about);
    let (lead, gits) = heading(&failure.text);
    let (title, detail) = match (&cause, lead) {
        (Cause::HookSkipped { .. }, _) => {
            let (own, said) = crate::notice::parts(&failure.text);
            let note = format!("{title}. {rest}");
            (own, if said.is_empty() { note } else { format!("{said}\n{note}") })
        }
        (_, Some(lead)) if rest.contains('\n') => (title, format!("{}\n{rest}", sentence(lead))),
        (_, Some(lead)) => (title, format!("{} {rest}", sentence(lead))),
        (_, None) => (title, rest),
    };
    if failure.output.is_none() {
        failure.output = Some(gits.trim().to_owned());
    }
    failure.output = failure.output.map(|output| crate::activity::redact(&output));
    for offer in offers {
        if !failure.actions.iter().any(|(label, _)| *label == offer.0) {
            failure.actions.push(offer);
        }
    }
    failure.plain = Some((crate::activity::redact(&title), crate::activity::redact(&detail)));
    failure
}

pub(crate) fn notice(message: String) -> Notice {
    explain(Failure::from(message), &About::default()).notice()
}

pub(crate) fn message(message: String) -> String {
    let explained = explain(Failure::from(message.clone()), &About::default());
    match (explained.plain, explained.output) {
        (Some((title, detail)), Some(output)) => format!("{title}. {detail}\n{output}"),
        (Some((title, detail)), None) => format!("{title}. {detail}"),
        _ => message,
    }
}


#[derive(Clone, Debug)]
pub(crate) enum Action {
    ForgetHostKey { host: String, file: PathBuf },
}

#[derive(Debug)]
pub(crate) enum Ask {
    ForgetHostKey { host: String, file: PathBuf },
}

fn default_known_hosts() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".ssh/known_hosts")
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::ForgetHostKey { host, file } => {
            if tab.ui.prompt.open() {
                return;
            }
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Explain(Ask::ForgetHostKey { host, file }), cx);
        }
    }
    cx.notify();
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(ask)
}

fn spec(ask: &Ask) -> Spec {
    match ask {
        Ask::ForgetHostKey { host, file } => Spec::new(format!("Remove the saved key for {host}?"), "Remove saved key")
            .line(format!(
                "{host} no longer has the key saved for it in {}. This can mean the server was reinstalled, or that something is intercepting the connection.",
                file.display()
            ))
            .line("Remove it only if you know why the key changed, for example because the host announced it. The next connection asks you to confirm the new key.")
            .line(format!("A copy of the file as it is now is kept as {}.old.", file.display()))
            .danger(),
    }
}

pub(crate) fn confirm(_: &mut Tab, ask: Ask, _: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::ForgetHostKey { host, file } => {
            Tab::work(
                cx,
                move || forget_host_key(&host, &file).map(|()| (host, file)),
                |tab, result, cx| {
                    let notice = match result {
                        Ok((host, file)) => Notice::new(
                            Tone::Success,
                            format!("Removed the saved key for {host}"),
                            format!("The old file is kept as {}.old. The next connection asks you to confirm the new key.", file.display()),
                        ),
                        Err(message) => Notice::new(Tone::Error, "The key was not removed", message),
                    };
                    tab.ui.notices.show(notice.logged(tab.trail.entry(false)));
                    cx.notify();
                },
            );
        }
    }
}

pub(crate) fn forget_host_key(host: &str, file: &Path) -> Result<(), String> {
    if host.is_empty() || host.starts_with('-') || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("That host name cannot be passed to ssh-keygen.".to_owned());
    }
    if cfg!(test) || crate::acceptance::replaying() {
        let temp = std::env::temp_dir().canonicalize().map_err(|e| e.to_string())?;
        if !file.canonicalize().is_ok_and(|file| file.starts_with(&temp)) {
            return Err(format!("{} is outside the test's folder.", file.display()));
        }
    }
    let found = crate::repository::run(std::process::Command::new(SSH_KEYGEN).arg("-F").arg(host).arg("-f").arg(file))
        .map_err(|e| format!("Run ssh-keygen: {e}"))?;
    if !found.status.success() || found.stdout.is_empty() {
        return Err(format!("{} has no saved key for {host} any more, so nothing was changed.", file.display()));
    }
    let removed = crate::repository::run(std::process::Command::new(SSH_KEYGEN).arg("-R").arg(host).arg("-f").arg(file))
        .map_err(|e| format!("Run ssh-keygen: {e}"))?;
    if !removed.status.success() {
        let said = String::from_utf8_lossy(&removed.stderr).trim().to_owned();
        return Err(if said.is_empty() { format!("ssh-keygen exited with {}", removed.status) } else { said });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    const REFUSED_HTTP: &str = "fatal: unable to access 'http://127.0.0.1:9/x.git/': Failed to connect to 127.0.0.1 port 9 after 0 ms: Couldn't connect to server";
    const REFUSED_SSH: &str = "ssh: connect to host 127.0.0.1 port 9: Connection refused\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.";
    const UNRESOLVED_HTTPS: &str = "fatal: unable to access 'https://git.example.invalid/team/app.git/': Could not resolve host: git.example.invalid";
    const UNRESOLVED_SSH: &str = "ssh: Could not resolve hostname git.example.invalid: nodename nor servname provided, or not known\nfatal: Could not read from remote repository.";
    const TIMED_OUT: &str = "ssh: connect to host github.com port 22: Operation timed out\nfatal: Could not read from remote repository.";
    const UNREACHABLE: &str = "ssh: connect to host 10.0.0.9 port 22: Network is unreachable\nfatal: Could not read from remote repository.";
    const TOO_SLOW: &str = "error: RPC failed; curl 28 Operation too slow. Less than 1000 bytes/sec transferred the last 30 seconds\nfatal: expected 'packfile'";
    const NO_USERNAME: &str = "fatal: could not read Username for 'http://127.0.0.1:18573': terminal prompts disabled";
    const AUTH_FAILED: &str = "remote: no\nfatal: Authentication failed for 'http://127.0.0.1:18573/401/x.git/'";
    const FORBIDDEN: &str = "remote: no\nfatal: unable to access 'http://127.0.0.1:18573/403/x.git/': The requested URL returned error: 403";
    const NOT_FOUND: &str = "remote: Repository not found.\nfatal: repository 'http://127.0.0.1:18573/404/x.git/' not found";
    const NOT_A_REPOSITORY: &str = "fatal: '/srv/gone.git' does not appear to be a git repository\nfatal: Could not read from remote repository.";
    const NO_CLONE_SOURCE: &str = "Cloning into 'gone'...\nfatal: repository '/srv/gone.git' does not exist";
    const PUBLICKEY: &str = "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.";
    const CERTIFICATE: &str = "fatal: unable to access 'https://git.example.invalid/app.git/': SSL certificate problem: self signed certificate in certificate chain";
    const CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nIT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\nSomeone could be eavesdropping on you right now (man-in-the-middle attack)!\nIt is also possible that a host key has just been changed.\nThe fingerprint for the ED25519 key sent by the remote host is\nSHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.\nPlease contact your system administrator.\nAdd correct host key in /Users/me/.ssh/known_hosts to get rid of this message.\nOffending ED25519 key in /Users/me/.ssh/known_hosts:3\nHost key for [git.example.invalid]:2222 has changed and you have requested strict checking.\nHost key verification failed.\nfatal: Could not read from remote repository.";
    const UNKNOWN: &str = "No ED25519 host key is known for git.example.invalid and you have requested strict checking.\nHost key verification failed.\nfatal: Could not read from remote repository.";
    const KEY_OPEN: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@         WARNING: UNPROTECTED PRIVATE KEY FILE!          @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nPermissions 0644 for '/Users/me/.ssh/id_ed25519' are too open.\nIt is required that your private key files are NOT accessible by others.\nThis private key will be ignored.\nLoad key \"/Users/me/.ssh/id_ed25519\": bad permissions\ngit@github.com: Permission denied (publickey).";
    const KEY_FORMAT: &str = "Load key \"/Users/me/.ssh/id_work\": invalid format\ngit@gitlab.com: Permission denied (publickey).";
    const KEY_MISSING: &str = "no such identity: /Users/me/.ssh/id_gone: No such file or directory\ngit@github.com: Permission denied (publickey).";
    const PROTECTED: &str = "remote: error: GH006: Protected branch update failed for refs/heads/main.\nremote: error: Changes must be made through a pull request.\nTo github.com:team/app.git\n ! [remote rejected] main -> main (protected branch hook declined)\nerror: failed to push some refs to 'github.com:team/app.git'";
    const DECLINED: &str = "main: [remote rejected] (pre-receive hook declined)\nremote: Commit 1a2b3c4 is missing a ticket number.\nremote: Push rejected by policy.\nTo /srv/app.git\n ! [remote rejected] main -> main (pre-receive hook declined)";
    const IN_THE_WAY: &str = "error: Your local changes to the following files would be overwritten by checkout:\n\troute.txt\n\tnotes/camp.txt\nPlease commit your changes or stash them before you switch branches.\nAborting";
    const UNTRACKED: &str = "error: The following untracked working tree files would be overwritten by merge:\n\tcamp.txt\nPlease move or remove them before you merge.\nAborting";
    const LOCKED: &str = "fatal: Unable to create '/work/app/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository, e.g.\nan editor opened by 'git commit'. Please make sure all processes\nare terminated then try again. If it still fails, a git process\nmay have crashed in this repository earlier:\nremove the file manually to continue.";
    const DUBIOUS: &str = "fatal: detected dubious ownership in repository at '/Volumes/Shared/app'\n'/Volumes/Shared/app' is owned by:\n\tBUILTIN/Administrators (S-1-5-32-544)\nbut the current user is:\n\tme\nTo add an exception for this directory, call:\n\n\tgit config --global --add safe.directory /Volumes/Shared/app";
    const GPG: &str = "error: gpg failed to sign the data\nfatal: failed to write commit object";
    const NO_IDENTITY: &str = "Author identity unknown\n\n*** Please tell me who you are.\n\nRun\n\n  git config --global user.email \"you@example.com\"\n  git config --global user.name \"Your Name\"\n\nto set your account's default identity.\nOmit --global to set the identity only in this repository.\n\nfatal: no email was given and auto-detection is disabled";
    const NO_EMAIL_DETECTED: &str = "fatal: unable to auto-detect email address (got 'me@Mac.(none)')";
    const EMPTY_NAME: &str = "fatal: empty ident name (for <aspen@example.invalid>) not allowed";
    const NO_PUSH_DESTINATION: &str = "fatal: No configured push destination.\nEither specify the URL from the command-line or configure a remote repository using\n\n    git remote add <name> <url>";
    const NO_SUCH_REMOTE: &str = "error: No such remote 'upstream'";
    const SKIPPED: &str = "hint: The '.git/hooks/pre-commit' hook was ignored because it's not set as executable.\nhint: You can disable this warning with `git config set advice.ignoredHook false`.\nOn branch main\nnothing to commit, working tree clean";
    const SKIPPED_ALONE: &str = "hint: The '.git/hooks/pre-commit' hook was ignored because it's not set as executable.\nhint: You can disable this warning with `git config set advice.ignoredHook false`.";
    const HOOK_SH: &str = ".git/hooks/pre-commit: line 2: npx: command not found";
    const HOOK_ZSH: &str = ".git/hooks/pre-commit:2: command not found: npx";
    const HOOK_DASH: &str = "> lint-staged\n.husky/pre-commit: 2: npx: not found\nhusky - pre-commit script failed (code 127)";
    const HOOK_EXEC: &str = ".git/hooks/commit-msg: line 2: exec: nonexist-tool: not found";
    const HOOK_ENV: &str = "env: node: No such file or directory";
    const HOOK_SHEBANG: &str = "fatal: cannot exec '.git/hooks/pre-commit': No such file or directory";
    const LFS_HOOK: &str = "\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. If you no longer wish to use Git LFS, remove this hook by deleting the 'pre-push' file in the hooks directory (set by 'core.hookspath'; usually '.git/hooks').\n\nerror: failed to push some refs to '../b.git'";
    const LFS_FILTER: &str = "git-lfs filter-process: git-lfs: command not found\nerror: could not read greeting from subprocess 'git-lfs filter-process'\nerror: initialization for subprocess 'git-lfs filter-process' failed\nfatal: x.bin: clean filter 'lfs' failed";
    const LFS_SMUDGE: &str = "git-lfs smudge -- 'a.bin': git-lfs: command not found\nerror: external filter 'git-lfs smudge -- %f' failed 127\nerror: external filter 'git-lfs smudge -- %f' failed\nfatal: a.bin: smudge filter lfs failed";
    const SSH_COMMAND: &str = "sh: ssh-with-agent: command not found\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.";
    const GPG_MISSING: &str = "error: cannot run gpg: No such file or directory\nerror: gpg failed to sign the data:\n(no gpg output)\nfatal: failed to write commit object";
    const SSH_SIGNER_MISSING: &str = "error: cannot run nokeygen: No such file or directory\nerror: \nfatal: failed to write commit object";
    const SERVER_WITHOUT_GIT: &str = "bash: git-upload-pack: command not found\nfatal: Could not read from remote repository.";

    fn about(remote: &str) -> About {
        About { remote: Some(remote.to_owned()), ..About::default() }
    }

    fn shown(heading: &str, said: &str, about: &About) -> Notice {
        explain(Failure::from(format!("{heading} — {said}")), about).notice()
    }

    fn offers(notice: &Notice) -> Vec<String> {
        notice.actions.iter().map(|(label, _)| label.to_string()).collect()
    }

    #[test]
    fn each_pattern_in_the_table_names_its_cause() {
        for (said, cause) in [
            (REFUSED_HTTP, Cause::Unreachable),
            (REFUSED_SSH, Cause::Unreachable),
            (UNRESOLVED_HTTPS, Cause::Unreachable),
            (UNRESOLVED_SSH, Cause::Unreachable),
            (TIMED_OUT, Cause::Unreachable),
            (UNREACHABLE, Cause::Unreachable),
            (TOO_SLOW, Cause::Stalled),
            (NO_USERNAME, Cause::SignIn),
            (AUTH_FAILED, Cause::SignIn),
            (FORBIDDEN, Cause::SignIn),
            (NOT_FOUND, Cause::NoRepository),
            (NOT_A_REPOSITORY, Cause::NoRepository),
            (NO_CLONE_SOURCE, Cause::NoRepository),
            (PUBLICKEY, Cause::PublicKey),
            (CERTIFICATE, Cause::Certificate),
            (
                CHANGED,
                Cause::HostKeyChanged {
                    host: Some("[git.example.invalid]:2222".into()),
                    file: Some("/Users/me/.ssh/known_hosts".into()),
                },
            ),
            (UNKNOWN, Cause::UnknownHost),
            (KEY_OPEN, Cause::KeyOpen { key: "/Users/me/.ssh/id_ed25519".into() }),
            (KEY_FORMAT, Cause::KeyUnreadable { key: "/Users/me/.ssh/id_work".into() }),
            (KEY_MISSING, Cause::KeyMissing { key: "/Users/me/.ssh/id_gone".into() }),
            (PROTECTED, Cause::Protected { branch: Some("main".into()) }),
            (
                DECLINED,
                Cause::Declined { lines: vec!["Commit 1a2b3c4 is missing a ticket number.".into(), "Push rejected by policy.".into()] },
            ),
            (IN_THE_WAY, Cause::InTheWay { files: vec!["route.txt".into(), "notes/camp.txt".into()], untracked: false }),
            (UNTRACKED, Cause::InTheWay { files: vec!["camp.txt".into()], untracked: true }),
            (LOCKED, Cause::Locked { lock: Some("/work/app/.git/index.lock".into()) }),
            (DUBIOUS, Cause::Dubious { command: Some("git config --global --add safe.directory /Volumes/Shared/app".into()) }),
            (GPG, Cause::Signing { missing: None }),
            (NO_IDENTITY, Cause::Identity),
            (NO_EMAIL_DETECTED, Cause::Identity),
            (EMPTY_NAME, Cause::Identity),
            (GPG_MISSING, Cause::Signing { missing: Some("gpg".into()) }),
            (SSH_SIGNER_MISSING, Cause::Signing { missing: Some("nokeygen".into()) }),
            (SKIPPED, Cause::HookSkipped { hook: "pre-commit".into(), path: ".git/hooks/pre-commit".into() }),
            (HOOK_SH, Cause::NotFound { hook: Some("pre-commit".into()), command: Some("npx".into()) }),
            (HOOK_ZSH, Cause::NotFound { hook: Some("pre-commit".into()), command: Some("npx".into()) }),
            (HOOK_DASH, Cause::NotFound { hook: Some("pre-commit".into()), command: Some("npx".into()) }),
            (HOOK_EXEC, Cause::NotFound { hook: Some("commit-msg".into()), command: Some("nonexist-tool".into()) }),
            (HOOK_ENV, Cause::NotFound { hook: None, command: Some("node".into()) }),
            (HOOK_SHEBANG, Cause::NotFound { hook: Some("pre-commit".into()), command: None }),
            (LFS_HOOK, Cause::NotFound { hook: Some("pre-push".into()), command: Some("git-lfs".into()) }),
            (LFS_FILTER, Cause::NotFound { hook: None, command: Some("git-lfs".into()) }),
            (LFS_SMUDGE, Cause::NotFound { hook: None, command: Some("git-lfs".into()) }),
            (SSH_COMMAND, Cause::NotFound { hook: None, command: Some("ssh-with-agent".into()) }),
            (NO_PUSH_DESTINATION, Cause::NoRemote),
            (NO_SUCH_REMOTE, Cause::NoRemote),
            (IN_WORKTREE, Cause::InWorktree { branch: Some("ridge".into()), path: Some("/work/ridge-tree".into()) }),
            (CHECKED_OUT_AT, Cause::InWorktree { branch: Some("ridge".into()), path: Some("/work/ridge-tree".into()) }),
        ] {
            assert_eq!(classify(said), Some(cause), "{said}");
        }
        assert_eq!(classify("fatal: Not possible to fast-forward, aborting."), None);
    }

    const IN_WORKTREE: &str = "fatal: 'ridge' is already used by worktree at '/work/ridge-tree'";
    const CHECKED_OUT_AT: &str = "fatal: 'ridge' is already checked out at '/work/ridge-tree'";

    #[test]
    fn a_branch_used_by_another_worktree_offers_to_open_it() {
        let notice = shown("Switch Failed", IN_WORKTREE, &About { local: true, ..About::default() });
        assert_eq!(notice.title, "ridge is checked out in worktree ridge-tree");
        assert_eq!(notice.detail, "Switch failed. Git keeps a branch in one worktree at a time.");
        assert_eq!(offers(&notice), ["Open that worktree"]);
        assert!(matches!(&notice.actions[0].1, view::Action::Worktree(crate::worktree::Action::Open(path)) if path == Path::new("/work/ridge-tree")));
    }

    #[test]
    fn a_missing_name_and_email_offers_the_form() {
        let notice = shown("Stash Failed", NO_IDENTITY, &About { local: true, ..About::default() });
        assert_eq!(notice.title, "Git needs your name and email");
        assert_eq!(notice.detail, "Stash failed. Set the name and email your commits carry, then try again.");
        assert_eq!(offers(&notice), ["Set name and email…"]);
        assert!(matches!(notice.actions[0].1, view::Action::Identity(crate::identity::Action::Edit)));
    }

    #[test]
    fn a_clone_from_nowhere_offers_to_change_the_address_not_a_remote() {
        let request = crate::clone::Request {
            address: "/srv/gone.git".into(),
            parent: "/srv/clones".into(),
            name: "gone".into(),
            ..Default::default()
        };
        let cloning = About {
            url: Some(request.address.clone()),
            again: Some(view::Action::Clone(crate::clone::Action::Again(request))),
            cloning: true,
            ..About::default()
        };
        let notice = shown("Clone Failed", NO_CLONE_SOURCE, &cloning);
        assert_eq!(notice.title, "There is no repository you can reach at that address");
        assert_eq!(notice.detail, "Clone failed. Check the address, and that this account has access.");
        assert_eq!(offers(&notice), ["Change the address…"]);
        let unreachable = shown("Clone Failed", REFUSED_HTTP, &cloning);
        assert_eq!((unreachable.title.as_str(), offers(&unreachable)), ("Could not reach 127.0.0.1", vec!["Retry".to_owned()]));
    }

    #[test]
    fn the_host_comes_from_gits_words_or_else_the_remotes_address() {
        assert_eq!(host_in(REFUSED_HTTP).as_deref(), Some("127.0.0.1"));
        assert_eq!(host_in(REFUSED_SSH).as_deref(), Some("127.0.0.1"));
        assert_eq!(host_in(UNRESOLVED_HTTPS).as_deref(), Some("git.example.invalid"));
        assert_eq!(host_in(UNRESOLVED_SSH).as_deref(), Some("git.example.invalid"));
        assert_eq!(host_in(NO_USERNAME).as_deref(), Some("127.0.0.1"));
        assert_eq!(host_in(PUBLICKEY).as_deref(), Some("github.com"));
        assert_eq!(host_in(UNKNOWN).as_deref(), Some("git.example.invalid"));
        assert_eq!(host_in(CHANGED).as_deref(), Some("[git.example.invalid]:2222"));
        assert_eq!(host_in("Host key verification failed."), None);
        for (address, host) in [
            ("https://scout@dev.example.invalid/team/app.git", Some("dev.example.invalid")),
            ("ssh://git@git.example.invalid:2222/team/app.git", Some("git.example.invalid")),
            ("git@github.com:team/app.git", Some("github.com")),
            ("github.com:team/app.git", Some("github.com")),
            ("/srv/git/app.git", None),
            ("./relative/app.git", None),
        ] {
            assert_eq!(host_of(address).as_deref(), host, "{address}");
        }
        let unnamed = About { url: Some("git@git.example.invalid:team/app.git".into()), ..about("origin") };
        let notice = shown("Fetch Failed", "Host key verification failed.\nfatal: Could not read from remote repository.", &unnamed);
        assert_eq!(notice.title, "git.example.invalid is not a known host");
        assert_eq!(shown("Fetch Failed", "Host key verification failed.", &about("origin")).title, "origin is not a known host");
    }

    #[test]
    fn an_explained_failure_has_a_title_a_sentence_one_step_and_gits_words() {
        let again = About { again: Some(view::Action::Sync(crate::sync::Action::FetchAll)), ..about("origin") };
        let notice = shown("Fetch Failed", REFUSED_HTTP, &again);
        assert_eq!(notice.tone, Tone::Error);
        assert_eq!(notice.title, "Could not reach 127.0.0.1");
        assert_eq!(notice.detail, "Fetch failed. Check your connection.");
        assert_eq!(offers(&notice), ["Retry"]);
        assert!(matches!(notice.actions[0].1, view::Action::Sync(crate::sync::Action::FetchAll)));
        assert_eq!(notice.output.as_deref(), Some(REFUSED_HTTP));
        assert!(shown("Fetch Failed", REFUSED_HTTP, &about("origin")).actions.is_empty());
        let unreachable = shown("Fetch Failed", NOT_FOUND, &about("origin"));
        assert!(matches!(&unreachable.actions[0].1, view::Action::Address(crate::address::Action::Edit(name)) if name == "origin"));
        let unnamed = About { remote: None, ..about("origin") };
        assert_eq!(offers(&shown("Fetch Failed", NOT_FOUND, &unnamed)), ["Manage remotes…"]);

        let table = [
            ("Push Failed", AUTH_FAILED, "Sign-in to 127.0.0.1 failed", Vec::<&str>::new()),
            ("Push Failed", PUBLICKEY, "github.com did not accept any of your SSH keys", vec!["Open SSH key settings"]),
            ("Fetch Failed", NOT_FOUND, "origin does not point at a repository you can reach", vec!["Edit address…"]),
            ("Fetch Failed", CERTIFICATE, "git.example.invalid's certificate could not be verified", vec![]),
            (
                "Fetch Failed",
                CHANGED,
                "[git.example.invalid]:2222's identity has changed since you last connected",
                vec!["Remove the saved key…"],
            ),
            ("Pull Failed", UNKNOWN, "git.example.invalid is not a known host", vec![]),
            ("Push Failed", KEY_OPEN, "Your SSH key id_ed25519 can be read by other users", vec!["Copy the command"]),
            ("Push Failed", KEY_FORMAT, "ssh could not read your key id_work", vec![]),
            ("Push Failed", KEY_MISSING, "The SSH key /Users/me/.ssh/id_gone does not exist", vec![]),
            ("Push Failed", PROTECTED, "origin does not allow pushing to main directly", vec![]),
            ("Push Failed", DECLINED, "origin refused the push", vec![]),
            ("Switch Failed", IN_THE_WAY, "Your changes to 2 files are in the way", vec![]),
            ("Commit Failed", LOCKED, "Another Git process is using this repository", vec![]),
            ("Open Failed", DUBIOUS, "Git does not trust this folder because another user owns it", vec!["Copy the command that trusts it"]),
            ("Commit Failed", GPG, "The commit could not be signed", vec![]),
            ("Push Failed", NO_PUSH_DESTINATION, "This repository has no remote", vec!["Add remote…"]),
        ];
        for (heading, said, title, steps) in table {
            let notice = shown(heading, said, &about("origin"));
            assert_eq!(notice.title, title, "{said}");
            assert_eq!(offers(&notice), steps, "{said}");
            assert!(notice.detail.starts_with(&sentence(heading)), "{}", notice.detail);
            assert_eq!(notice.output.as_deref(), Some(said), "Git's words stay for Show Git output");
        }

        let key = shown("Push Failed", PUBLICKEY, &about("origin"));
        assert!(matches!(&key.actions[0].1, view::Action::Handoff(crate::handoff::Action::OpenUrl(url)) if url == "https://github.com/settings/keys"));
        let changed = shown("Fetch Failed", CHANGED, &about("origin"));
        assert!(matches!(
            &changed.actions[0].1,
            view::Action::Explain(Action::ForgetHostKey { host, file })
                if host == "[git.example.invalid]:2222" && file == Path::new("/Users/me/.ssh/known_hosts")
        ));
        let trust = shown("Open Failed", DUBIOUS, &About::default());
        assert!(matches!(
            &trust.actions[0].1,
            view::Action::CopyText { text, .. } if text == "git config --global --add safe.directory /Volumes/Shared/app"
        ));
        let open = shown("Push Failed", KEY_OPEN, &about("origin"));
        assert!(matches!(&open.actions[0].1, view::Action::CopyText { text, .. } if text == "chmod 600 /Users/me/.ssh/id_ed25519"));
        let declined = shown("Push Failed", DECLINED, &about("origin"));
        assert_eq!(declined.detail, "Push failed.\nCommit 1a2b3c4 is missing a ticket number.\nPush rejected by policy.");
        assert_eq!(
            shown("Switch Failed", IN_THE_WAY, &About::default()).detail,
            "Switch failed. route.txt and notes/camp.txt. Commit or stash them first."
        );
        assert_eq!(shown("Pull Failed", UNTRACKED, &About::default()).title, "Untracked files are in the way: 1 file");
        let commit = About { again: Some(view::Action::Commit), ..About::default() };
        let locked = shown("Commit Failed", LOCKED, &commit);
        assert_eq!(offers(&locked), ["Retry"]);
        assert!(locked.detail.contains("delete /work/app/.git/index.lock"), "{}", locked.detail);
        let raw = explain(Failure::from(IN_THE_WAY.to_owned()), &About::default()).notice();
        assert_eq!((raw.title.as_str(), raw.detail.as_str()), ("Your changes to 2 files are in the way", "route.txt and notes/camp.txt. Commit or stash them first."));
    }

    #[test]
    fn a_step_already_offered_stays_and_an_explanation_is_given_once() {
        let offered = Failure::from(format!("Switch Failed — {IN_THE_WAY}"))
            .offer(crate::autostash::RETRY, view::Action::Commit);
        let once = explain(offered, &About::default());
        let plain = once.plain.clone();
        let twice = explain(once, &about("upstream"));
        assert_eq!(twice.plain, plain);
        let notice = twice.notice();
        assert_eq!(offers(&notice), [crate::autostash::RETRY]);
        assert_eq!(notice.title, "Your changes to 2 files are in the way");
        let kept = explain(Failure::from("Push Failed — The push failed.".to_owned()).output(PROTECTED), &about("origin")).notice();
        assert_eq!(kept.title, "origin does not allow pushing to main directly");
        assert_eq!(kept.output.as_deref(), Some(PROTECTED));
    }

    #[test]
    fn a_failure_already_in_plain_words_is_left_as_it_is() {
        let raw = "topic: [remote rejected] (protected branch hook declined)";
        let worded = Failure::from("Delete Failed — origin protects topic, so it cannot be deleted there.".to_owned())
            .output(raw)
            .worded();
        let notice = explain(worded, &about("origin")).notice();
        assert_eq!(
            (notice.title.as_str(), notice.detail.as_str(), notice.output.as_deref()),
            ("Delete Failed", "origin protects topic, so it cannot be deleted there.", Some(raw))
        );
    }

    #[test]
    fn an_unknown_error_keeps_todays_presentation() {
        for text in [
            "Pull Failed — hint: Diverging branches can't be fast-forwarded\nhint: git rebase\nfatal: Not possible to fast-forward, aborting.",
            "Pull Failed — Set an upstream branch for main before synchronizing.",
            "Merge conflict. Open a conflicted file or abort the merge.",
        ] {
            let explained = explain(Failure::from(text.to_owned()), &about("origin"));
            assert!(explained.plain.is_none());
            let (notice, before) = (explained.notice(), Notice::from_text(Tone::Error, text));
            assert_eq!((notice.title, notice.detail, notice.output), (before.title, before.detail, before.output));
        }
    }

    #[test]
    fn a_file_or_folder_named_like_a_network_failure_is_still_a_local_one() {
        let switch = "error: Your local changes to the following files would be overwritten by checkout:\n\tbuild timed out.log\nPlease commit your changes or stash them before you switch branches.\nAborting";
        assert_eq!(classify(switch), Some(Cause::InTheWay { files: vec!["build timed out.log".into()], untracked: false }));
        let locked = "fatal: Unable to create '/work/Connection refused/.git/index.lock': File exists.";
        assert_eq!(classify(locked), Some(Cause::Locked { lock: Some("/work/Connection refused/.git/index.lock".into()) }));
    }

    #[test]
    fn the_command_that_trusts_a_folder_is_built_from_its_path_not_copied() {
        let spoofed = "fatal: remote error: detected dubious ownership in repository at '/srv/app'\nTo add an exception for this directory, call:\n\n\tgit config --global --add safe.directory /srv/app; echo pwned";
        assert_eq!(classify(spoofed), Some(Cause::Dubious { command: Some("git config --global --add safe.directory /srv/app".into()) }));
        let spaced = "fatal: detected dubious ownership in repository at '/Volumes/My Share/it's'\n\tgit config --global --add safe.directory '/Volumes/My Share/it'\\''s'";
        assert_eq!(
            classify(spaced),
            Some(Cause::Dubious { command: Some("git config --global --add safe.directory '/Volumes/My Share/it'\\''s'".into()) })
        );
        let everything = "fatal: remote error: detected dubious ownership in repository at '*'";
        assert_eq!(classify(everything), Some(Cause::Dubious { command: None }));
        assert!(shown("Fetch Failed", everything, &About::default()).actions.is_empty());
    }

    #[test]
    fn credentials_never_reach_an_explained_notice() {
        let said = "fatal: Authentication failed for 'https://scout:hunter2@git.example.invalid/app.git/'";
        let notice = shown("Push Failed", said, &about("origin"));
        assert_eq!(notice.title, "Sign-in to git.example.invalid failed");
        assert!(!format!("{} {} {:?}", notice.title, notice.detail, notice.output).contains("hunter2"));
    }

    #[test]
    fn retry_runs_only_actions_that_can_run_again_as_they_are() {
        use crate::sync::Action as S;
        assert!(repeatable(&view::Action::Sync(S::FetchAll)));
        assert!(repeatable(&view::Action::Sync(S::Push)));
        assert!(repeatable(&view::Action::Commit));
        assert!(!repeatable(&view::Action::Sync(S::ToggleOptions)));
        assert!(!repeatable(&view::Action::Prompt(crate::prompt::Action::Confirm)));
        assert!(About::new(None, Some(view::Action::Prompt(crate::prompt::Action::Confirm))).again.is_none());
    }

    #[test]
    fn a_job_names_the_host_from_its_remotes_address() {
        let f = Fixture::new();
        f.git(&["remote", "add", "origin", "ssh://git@git.example.invalid:2222/team/app.git"]);
        let failure = Failure::from("Fetch Failed — Host key verification failed.".to_owned());
        let notice = About::new(Some("origin".into()), None).explain(&f.repo, failure).notice();
        assert_eq!(notice.title, "git.example.invalid is not a known host");
        let failure = Failure::from("Fetch Failed — Host key verification failed.".to_owned());
        assert_eq!(About::new(Some("gone".into()), None).explain(&f.repo, failure).notice().title, "gone is not a known host");
        f.git(&["config", "remote.origin.pushurl", "git@push.example.invalid:team/app.git"]);
        let failure = Failure::from("Push Failed — Host key verification failed.".to_owned());
        let pushing = About { push: true, ..About::new(Some("origin".into()), None) };
        assert_eq!(pushing.explain(&f.repo, failure).notice().title, "push.example.invalid is not a known host");
        let failure = Failure::from("Fetch Failed — Host key verification failed.".to_owned());
        assert_eq!(About::new(Some("origin".into()), None).explain(&f.repo, failure).notice().title, "git.example.invalid is not a known host");
    }

    #[test]
    fn a_github_sign_in_offers_the_connection_and_a_lapsed_one_reconnects() {
        const GITHUB_REFUSED: &str = "remote: Invalid username or token. Password authentication is not supported for Git operations.\nfatal: Authentication failed for 'https://github.com/TrailTeam/ledger.git/'";
        const LAPSED: &str = "FastKraken credentials: The GitHub sign-in has lapsed. Reconnect GitHub.\nfatal: credential helper '!exec '/Applications/FastKraken.app/Contents/MacOS/fastkraken' --git-credential 'https://github.com/TrailTeam/ledger.git'' told us to quit";
        let _apart = crate::auth::tests::Apart::new();
        let f = Fixture::new();
        let url = "https://github.com/TrailTeam/ledger.git";
        f.git(&["remote", "add", "origin", url]);
        f.git(&["remote", "add", "local", "http://127.0.0.1:9/x.git"]);
        let fetch = Some(view::Action::Sync(crate::sync::Action::FetchAll));
        let explained = |remote: Option<&str>, said: &str| {
            let failure = Failure::from(format!("Fetch Failed — {said}"));
            About::new(remote.map(str::to_owned), fetch.clone()).explain(&f.repo, failure).notice()
        };
        let unbound = explained(Some("origin"), GITHUB_REFUSED);
        assert_eq!(unbound.title, "Sign-in to github.com failed");
        assert_eq!(offers(&unbound), ["Try again", "Use your GitHub connection for origin"]);
        let view::Action::Connect(crate::connect::Action::Use(bind)) = &unbound.actions[1].1 else { panic!() };
        assert_eq!((bind.remote.as_deref(), bind.url.as_deref()), (Some("origin"), Some(url)));
        assert!(matches!(bind.again.as_deref(), Some(view::Action::Sync(crate::sync::Action::FetchAll))));
        assert_eq!(offers(&explained(None, GITHUB_REFUSED)), ["Try again", "Use your GitHub connection for origin"]);
        assert_eq!(offers(&explained(Some("local"), AUTH_FAILED)), ["Try again"]);

        let client = crate::github::Client::for_test("http://127.0.0.1:9/");
        crate::auth::bind(&f.repo, "origin", url, &client, 11).unwrap();
        let bound = explained(Some("origin"), GITHUB_REFUSED);
        assert_eq!(offers(&bound), ["Try again", "Reconnect GitHub…"]);
        assert!(bound.detail.contains("Reconnect GitHub"));

        assert_eq!(classify(LAPSED), Some(Cause::Lapsed));
        let lapsed = explained(Some("origin"), LAPSED);
        assert_eq!(lapsed.title, "Your GitHub sign-in has lapsed");
        assert_eq!(offers(&lapsed), ["Reconnect GitHub…"]);
        let view::Action::Connect(crate::connect::Action::Reconnect { again }) = &lapsed.actions[0].1 else { panic!() };
        assert!(again.is_some());
        assert!(lapsed.output.as_deref().is_some_and(|output| output.contains("told us to quit")));
    }

    #[test]
    fn a_real_fetch_from_a_port_nothing_listens_on_is_explained_and_moves_nothing() {
        let f = Fixture::new();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        f.git(&["remote", "add", "origin", &format!("http://127.0.0.1:{port}/app.git")]);
        let refs = f.git(&["for-each-ref"]);
        let mut fetch = f.repo.network_command().unwrap();
        let said = crate::repository::checked(fetch.args(["-c", "credential.helper=", "fetch", "origin"])).unwrap_err();
        let again = Some(view::Action::Sync(crate::sync::Action::FetchAll));
        let failure = Failure::from(format!("Fetch Failed — {said}"));
        let notice = About::new(Some("origin".into()), again).explain(&f.repo, failure).notice();
        assert_eq!(notice.title, "Could not reach 127.0.0.1");
        assert_eq!(offers(&notice), ["Retry"]);
        assert!(notice.output.as_deref().is_some_and(|output| output.contains(&format!("port {port}"))), "{:?}", notice.output);
        assert_eq!(f.git(&["for-each-ref"]), refs);
    }

    #[test]
    fn a_hook_or_helper_that_cannot_run_is_named_with_what_it_could_not_find() {
        let commit = About { again: Some(view::Action::Commit), local: true, root: Some("/work/app".into()), ..About::default() };
        let missing = shown("Commit Failed", HOOK_SH, &commit);
        assert_eq!(missing.title, "pre-commit could not find npx");
        assert!(missing.detail.starts_with("Commit failed. npx is not in any folder on the PATH your login shell sets up."), "{}", missing.detail);
        assert_eq!(offers(&missing), ["Retry"]);
        assert_eq!(missing.output.as_deref(), Some(HOOK_SH));
        assert_eq!(shown("Push Failed", LFS_HOOK, &about("origin")).title, "pre-push could not find Git LFS");
        let filter = shown("Stage Failed", LFS_FILTER, &About { again: Some(view::Action::StageAll), ..About::default() });
        assert_eq!(filter.title, "Git could not find Git LFS");
        assert!(filter.detail.contains(crate::lfs::NOT_INSTALLED), "{}", filter.detail);
        assert_eq!(offers(&filter), [crate::lfs::INSTALL_LABEL, "Retry"]);
        assert!(matches!(&filter.actions[0].1, view::Action::Handoff(crate::handoff::Action::OpenUrl(url)) if url == crate::lfs::INSTALL_PAGE));
        let checkout = shown("Switch Failed", LFS_SMUDGE, &About::default());
        assert_eq!((checkout.title.as_str(), offers(&checkout)), ("Git could not find Git LFS", vec![crate::lfs::INSTALL_LABEL.to_owned()]));
        assert_eq!(shown("Fetch Failed", SSH_COMMAND, &about("origin")).title, "Git could not find ssh-with-agent");
        assert_eq!(shown("Commit Failed", HOOK_ENV, &commit).title, "Git could not find node");
        let shebang = shown("Commit Failed", HOOK_SHEBANG, &commit);
        assert_eq!(shebang.title, "pre-commit could not start");
        assert!(shebang.actions.is_empty());
        let gpg = shown("Commit Failed", GPG_MISSING, &commit);
        assert_eq!(gpg.title, "The commit could not be signed");
        assert!(gpg.detail.contains("Git could not find gpg, which signs your commits."), "{}", gpg.detail);
        assert_ne!(classify(SERVER_WITHOUT_GIT).map(|cause| matches!(cause, Cause::NotFound { .. })), Some(true));
        let skipped = shown("Commit Failed", SKIPPED, &commit);
        assert_eq!(skipped.title, "Commit Failed");
        assert_eq!(
            skipped.detail,
            "On branch main nothing to commit, working tree clean\npre-commit is not executable, so Git skipped it. Make it executable so that Git runs it."
        );
        assert!(matches!(&skipped.actions[..], [(label, view::Action::CopyText { text, .. })]
            if label.as_ref() == "Copy the command" && text == "chmod +x /work/app/.git/hooks/pre-commit"));
        assert_eq!(skipped.output.as_deref(), Some(SKIPPED));
    }

    #[test]
    fn a_failure_beside_a_skipped_hook_still_says_why_it_failed() {
        let said = format!("{SKIPPED_ALONE}\nCommit messages must start with a ticket number");
        let commit = About { again: Some(view::Action::Commit), local: true, root: Some("/work/app".into()), ..About::default() };
        let notice = shown("Commit Failed", &said, &commit);
        assert_eq!(notice.title, "Commit Failed");
        assert!(notice.detail.starts_with("Commit messages must start with a ticket number\n"), "{}", notice.detail);
        assert!(notice.detail.ends_with("pre-commit is not executable, so Git skipped it. Make it executable so that Git runs it."), "{}", notice.detail);
        assert_eq!(offers(&notice), ["Copy the command"]);
    }

    #[test]
    fn a_skipped_hook_is_never_the_explanation_when_something_else_is() {
        let both = format!("{SKIPPED}\n{LOCKED}");
        assert!(matches!(classify(&both), Some(Cause::Locked { .. })));
        let both = format!("{SKIPPED}\n{HOOK_SH}");
        assert!(matches!(classify(&both), Some(Cause::NotFound { .. })));
    }

    #[test]
    fn what_a_hook_prints_about_a_network_does_not_explain_a_local_write() {
        let said = "tests: could not connect: Connection refused\n2 tests failed";
        let commit = About { again: Some(view::Action::Commit), local: true, ..About::default() };
        let notice = shown("Commit Failed", said, &commit);
        let before = Notice::from_text(Tone::Error, &format!("Commit Failed — {said}"));
        assert_eq!((notice.title, notice.actions.len()), (before.title, 0));
        assert_eq!(shown("Fetch Failed", said, &about("origin")).title, "Could not reach origin");
        let both = format!("{said}\n{HOOK_ZSH}");
        assert_eq!(shown("Commit Failed", &both, &commit).title, "pre-commit could not find npx");
    }

    #[test]
    fn a_commit_that_skipped_a_hook_says_so_and_commits() {
        let f = Fixture::new();
        let hooks = f.dir.join(".git/skipped-hooks");
        fs::create_dir(&hooks).unwrap();
        fs::write(hooks.join("pre-commit"), "#!/bin/sh\nexit 1\n").unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        fs::write(f.dir.join("camp.txt"), "tent\n").unwrap();
        f.git(&["add", "camp.txt"]);
        let skipped = f.repo.commit("Pitch the tent").unwrap().skipped;
        assert_eq!(skipped, [hooks.join("pre-commit")]);
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Pitch the tent\n");
        let outcome = skipped_outcome(&skipped);
        assert_eq!(outcome.detail, "Pitch the tent\npre-commit is not executable, so Git skipped it.");
        assert!(matches!(&outcome.actions[..], [(_, view::Action::CopyText { text, .. })] if *text == format!("chmod +x {}", quoted(&hooks.join("pre-commit").to_string_lossy()))));
        fs::set_permissions(hooks.join("pre-commit"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        fs::write(f.dir.join("camp.txt"), "tarp\n").unwrap();
        let tip = f.git(&["rev-parse", "HEAD"]);
        let head = String::from_utf8(tip.clone()).unwrap();
        assert!(f.repo.commit_all(Some(head.trim()), Some(std::ffi::OsStr::new("refs/heads/main")), "Swap").is_err());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
        fs::set_permissions(hooks.join("pre-commit"), std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
        fs::remove_file(hooks.join("pre-commit")).unwrap();
        assert!(f.repo.commit_all(Some(head.trim()), Some(std::ffi::OsStr::new("refs/heads/main")), "Swap").unwrap().skipped.is_empty());
        assert_eq!(skipped_outcome(&[]).detail, "Pitch the tent");
    }

    fn skipped_outcome(hooks: &[PathBuf]) -> Outcome {
        skipped(Outcome::titled("Changes committed", "Pitch the tent"), hooks)
    }

    #[test]
    fn a_hook_that_cannot_find_its_program_stops_the_commit_and_is_named() {
        let f = Fixture::new();
        let hooks = f.dir.join(".git/missing-hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\nfastkraken-no-such-linter --fix\n").unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        fs::write(f.dir.join("camp.txt"), "tent\n").unwrap();
        f.git(&["add", "camp.txt"]);
        let said = f.repo.commit("Pitch the tent").unwrap_err().text;
        assert!(f.repo.status().unwrap().head.is_none(), "nothing was committed");
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), b"camp.txt\n", "the index is as it was");
        let commit = About { again: Some(view::Action::Commit), local: true, ..About::default() };
        let notice = commit.explain(&f.repo, Failure::from(format!("Commit Failed — {said}"))).notice();
        assert_eq!(notice.title, "pre-commit could not find fastkraken-no-such-linter");
        assert_eq!(offers(&notice), ["Retry"]);
    }

    #[test]
    fn hooks_run_with_the_bundled_git_first_and_then_the_environments_path() {
        let f = Fixture::new();
        let hooks = f.dir.join(".git/path-hooks");
        fs::create_dir(&hooks).unwrap();
        let seen = f.dir.join(".git/seen-path");
        let hook = hooks.join("pre-commit");
        fs::write(&hook, format!("#!/bin/sh\nprintf '%s' \"$PATH\" > '{}'\n", seen.display())).unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        fs::write(f.dir.join("camp.txt"), "tent\n").unwrap();
        f.git(&["add", "camp.txt"]);
        f.repo.commit("Pitch the tent").unwrap();
        let path = fs::read_to_string(&seen).unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        let runtime = std::env::var_os("FASTKRAKEN_GIT_ROOT").map(PathBuf::from).unwrap().canonicalize().unwrap();
        let given = dirs.iter().position(|dir| *dir == runtime.join("bin")).expect("the bundled Git's bin");
        assert_eq!(&dirs[given + 1..], crate::environment::host_dirs(), "{path}");
    }

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINhevt3r6VlryoT9qGlrR2tgCE7NfNYZeA5H/NtZ8Ai6";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIJs5d6wjK7Zo/eUimzVpMUzkn63seGzZJNxbDoo/dH6";

    #[test]
    fn removing_a_changed_key_takes_only_that_hosts_keys_and_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        let before = format!("other.invalid {KEY_A}\ntalus.invalid {KEY_B}\n[talus.invalid]:2222 {KEY_A}\n");
        fs::write(&file, &before).unwrap();
        forget_host_key("talus.invalid", &file).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            format!("other.invalid {KEY_A}\n[talus.invalid]:2222 {KEY_A}\n")
        );
        assert_eq!(fs::read_to_string(dir.path().join("known_hosts.old")).unwrap(), before);
        forget_host_key("[talus.invalid]:2222", &file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("other.invalid {KEY_A}\n"));
    }

    #[test]
    fn removing_a_key_is_refused_once_the_file_no_longer_has_one_and_for_odd_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        fs::write(&file, format!("talus.invalid {KEY_B}\n")).unwrap();
        fs::write(&file, format!("other.invalid {KEY_A}\n")).unwrap();
        let refused = forget_host_key("talus.invalid", &file).unwrap_err();
        assert!(refused.contains("has no saved key for talus.invalid any more"), "{refused}");
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("other.invalid {KEY_A}\n"));
        assert!(!dir.path().join("known_hosts.old").exists());
        for host in ["", "-oProxyCommand=x", "two words"] {
            assert!(forget_host_key(host, &file).is_err(), "{host:?}");
        }
        let outside = forget_host_key("talus.invalid", Path::new("/etc/ssh/ssh_known_hosts")).unwrap_err();
        assert!(outside.contains("outside the test's folder"), "{outside}");
    }

    #[test]
    fn the_removal_prompt_is_a_danger_prompt_naming_the_host_and_file() {
        let spec = spec(&Ask::ForgetHostKey { host: "talus.invalid".into(), file: "/tmp/known_hosts".into() });
        assert!(spec.danger);
        assert_eq!(spec.title, "Remove the saved key for talus.invalid?");
        assert_eq!(spec.confirm.as_ref(), "Remove saved key");
        assert!(spec.lines[0].contains("/tmp/known_hosts"));
        assert!(spec.lines[2].contains("/tmp/known_hosts.old"));
    }
}
