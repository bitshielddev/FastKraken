use crate::{
    Tab,
    commands::Target,
    notice::{Failure, Outcome},
    push_to,
    repository::{Repository, checked},
    view::{self, Ui},
};
use gpui::{Context, SharedString, Window};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
};

pub(crate) const OFFER: &str = "Open pull request";

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, networking, operating};
    fn branch_of<'a>(ui: &'a Ui, target: &'a Target) -> Option<&'a OsStr> {
        match target {
            Target::Branch(name) => Some(name),
            Target::Repo if !ui.detached && ui.head.is_some() && !ui.branch.is_empty() => Some(OsStr::new(&ui.branch)),
            _ => None,
        }
    }
    fn dropped(ui: &Ui, target: &Target) -> Option<(OsString, Onto, String)> {
        let Target::Pair { dragged, onto } = target else { return None };
        let Target::Branch(name) = dragged.as_ref() else { return None };
        match onto.as_ref() {
            Target::RemoteBranch(full) => {
                ui.refs.oid_of(onto)?;
                let (remote, there) = crate::branch::split_remote(&ui.refs.remotes, full)?;
                let base = there.into_string().ok()?;
                let onto = Onto { remote: remote.to_owned(), base: Some(base) };
                Some((name.clone(), onto, full.to_string_lossy().into_owned()))
            }
            Target::Remote(remote) if ui.refs.remotes.contains(remote) => {
                Some((name.clone(), Onto { remote: remote.clone(), base: None }, remote.clone()))
            }
            _ => None,
        }
    }
    fn available(ui: &Ui, request: &Request) -> Availability {
        match request.push {
            Some(_) => Availability::unless(networking(ui).or_else(|| operating(ui))),
            None => Availability::Enabled,
        }
    }
    &[
        Command::new(
            "branch.pull-request",
            Group::Sync,
            |ui, target| match branch_of(ui, target).and_then(|branch| resolve(ui, branch, None)) {
                Some(request) if request.push.is_some() => "Push and open pull request…".into(),
                Some(request) => format!("Open pull request for {}…", request.branch.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| match branch_of(ui, target).filter(|branch| crate::pulls::open_of_local(ui, branch).is_none()).and_then(|branch| resolve(ui, branch, None)) {
                Some(request) => available(ui, &request),
                None => Availability::Hidden,
            },
            |ui, target| {
                let branch = branch_of(ui, target)?.to_owned();
                Some(view::Action::PullRequest(Action::Open { branch, onto: None }))
            },
        )
        .words(&["pull request", "pr", "merge request", "start a pull request", "compare", "github", "gitlab", "bitbucket"])
        .icon("globe"),
        Command::new(
            "drop.pull-request",
            Group::Remotes,
            |ui, target| {
                let Some((branch, onto, shown)) = dropped(ui, target) else { return String::new() };
                match resolve(ui, &branch, Some(&onto)) {
                    Some(request) if request.push.is_some() => {
                        format!("Push {} and open pull request to {shown}…", branch.to_string_lossy())
                    }
                    Some(_) => format!("Open pull request from {} to {shown}…", branch.to_string_lossy()),
                    None => String::new(),
                }
            },
            |ui, target, _| {
                let found = dropped(ui, target).and_then(|(branch, onto, _)| resolve(ui, &branch, Some(&onto)));
                match found {
                    Some(request) => available(ui, &request),
                    None => Availability::Hidden,
                }
            },
            |ui, target| {
                let (branch, onto, _) = dropped(ui, target)?;
                Some(view::Action::PullRequest(Action::Open { branch, onto: Some(onto) }))
            },
        )
        .icon("globe"),
    ]
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Onto {
    pub(crate) remote: String,
    pub(crate) base: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open { branch: OsString, onto: Option<Onto> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Push {
    pub(crate) track: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) branch: OsString,
    pub(crate) tip: String,
    pub(crate) remote: String,
    pub(crate) there: String,
    pub(crate) push: Option<Push>,
    pub(crate) page: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Host {
    GitHub,
    GitLab,
    Bitbucket,
    Other,
}

fn host_kind(host: &str) -> Host {
    match host {
        "github.com" => Host::GitHub,
        "gitlab.com" => Host::GitLab,
        "bitbucket.org" => Host::Bitbucket,
        host if host.starts_with("github.") => Host::GitHub,
        host if host.starts_with("gitlab.") => Host::GitLab,
        _ => Host::Other,
    }
}

fn split(page: &str) -> Option<(&str, &str)> {
    page.strip_prefix("https://")?.split_once('/').filter(|(host, path)| !host.is_empty() && !path.is_empty())
}

fn escape(text: &str, keep_slash: bool) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => escaped.push(byte as char),
            b'/' if keep_slash => escaped.push('/'),
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}

pub(crate) fn page(base_page: &str, head_page: &str, base: &str, head: &str) -> Option<String> {
    let (host, path) = split(head_page)?;
    let (base_host, base_path) = split(base_page)?;
    let same = base_page == head_page;
    Some(match host_kind(host) {
        Host::GitHub if same || base_host != host => {
            format!("{head_page}/compare/{}...{}?expand=1", escape(base, true), escape(head, true))
        }
        Host::GitHub => {
            let (owner, repo) = path.split_once('/')?;
            let parent_repo = base_path.rsplit('/').next()?;
            let from = match repo == parent_repo {
                true => format!("{}:{}", escape(owner, false), escape(head, true)),
                false => format!("{}:{}:{}", escape(owner, false), escape(repo, false), escape(head, true)),
            };
            format!("{base_page}/compare/{}...{from}?expand=1", escape(base, true))
        }
        Host::GitLab => {
            let mut url = format!("{head_page}/-/merge_requests/new?merge_request[source_branch]={}", escape(head, false));
            if same {
                url.push_str(&format!("&merge_request[target_branch]={}", escape(base, false)));
            }
            url
        }
        Host::Bitbucket => {
            let mut url = format!("{head_page}/pull-requests/new?source={}", escape(head, false));
            if same {
                url.push_str(&format!("&dest={}", escape(base, false)));
            }
            url
        }
        Host::Other => head_page.to_owned(),
    })
}

pub(crate) fn web(ui: &Ui, remote: &str) -> Option<String> {
    let addresses = ui.refs.addresses.get(remote)?;
    let push = addresses.separate_push().ok().flatten();
    addresses.single().and_then(crate::address::web_address).or_else(|| push.and_then(crate::address::web_address))
}

fn listed(ui: &Ui, remote: &str, branch: &str) -> Option<String> {
    let full = OsString::from(format!("{remote}/{branch}"));
    ui.refs.oid_of(&Target::RemoteBranch(full)).map(str::to_owned)
}

pub(crate) fn default_branch(ui: &Ui, remote: &str) -> String {
    if let Some(found) = ui.refs.defaults.get(remote) {
        return found.clone();
    }
    match ["main", "master"].into_iter().find(|name| listed(ui, remote, name).is_some()) {
        Some(name) => name.to_owned(),
        None => "main".to_owned(),
    }
}

fn home(ui: &Ui, tracked: Option<&str>) -> Option<String> {
    let remotes = &ui.refs.remotes;
    tracked
        .map(str::to_owned)
        .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
        .or_else(|| remotes.first().cloned())
}

fn parent(ui: &Ui, remote: &str, head_page: &str) -> Option<String> {
    if remote == "upstream" || !ui.refs.remotes.iter().any(|name| name == "upstream") {
        return None;
    }
    let page = web(ui, "upstream")?;
    let same_host = split(&page).map(|(host, _)| host) == split(head_page).map(|(host, _)| host);
    (same_host && page != head_page).then(|| "upstream".to_owned())
}

pub(crate) fn resolve(ui: &Ui, branch: &OsStr, onto: Option<&Onto>) -> Option<Request> {
    let item = ui.refs.local(branch)?;
    let tip = item.oid.clone()?;
    let local = branch.to_str()?;
    let upstream = item
        .upstream
        .as_deref()
        .and_then(|upstream| crate::branch::split_remote(&ui.refs.remotes, OsStr::new(upstream)));
    let tracked = match &upstream {
        Some((remote, there)) => {
            let there = there.to_str()?;
            (there == local || default_branch(ui, remote) != there).then(|| (remote.to_string(), there.to_owned()))
        }
        None => None,
    };
    let (remote, there, push) = match tracked {
        Some((remote, there)) => {
            let push = (item.gone || item.ahead > 0).then_some(Push { track: false });
            (remote, there, push)
        }
        None => {
            let remote = home(ui, upstream.as_ref().map(|(remote, _)| *remote))?;
            let pushed = listed(ui, &remote, local).as_deref() == Some(tip.as_str());
            let push = (!pushed).then_some(Push { track: item.upstream.is_none() });
            (remote, local.to_owned(), push)
        }
    };
    let head_page = web(ui, &remote)?;
    let (base_page, base) = match onto {
        Some(Onto { remote: into, base }) => {
            let base = base.clone().unwrap_or_else(|| default_branch(ui, into));
            (web(ui, into)?, base)
        }
        None => {
            let into = parent(ui, &remote, &head_page).unwrap_or_else(|| remote.clone());
            (web(ui, &into)?, default_branch(ui, &into))
        }
    };
    if base_page == head_page && base == there {
        return None;
    }
    let page = page(&base_page, &head_page, &base, &there)?;
    Some(Request { branch: branch.to_owned(), tip, remote, there, push, page })
}

pub(crate) fn offer(ui: &Ui, branch: &OsStr, remote: &str, destination: &OsStr) -> Option<(SharedString, view::Action)> {
    if let Some((_, pull)) = crate::pulls::open_of_local(ui, branch) {
        let open = view::Action::Handoff(crate::handoff::Action::OpenUrl(pull.page.clone()));
        return Some((format!("View pull request #{}", pull.number).into(), open));
    }
    let request = resolve(ui, branch, None)?;
    if request.remote != remote || OsStr::new(&request.there) != destination {
        return None;
    }
    Some((OFFER.into(), view::Action::PullRequest(Action::Open { branch: branch.to_owned(), onto: None })))
}

pub(crate) fn offer_upstream(ui: &Ui, branch: &OsStr) -> Option<(SharedString, view::Action)> {
    let upstream = ui.refs.local(branch)?.upstream.as_deref()?;
    let (remote, there) = crate::branch::split_remote(&ui.refs.remotes, OsStr::new(upstream))?;
    offer(ui, branch, remote, &there)
}

pub(crate) fn offered(mut outcome: Outcome, offer: Option<(SharedString, view::Action)>) -> Outcome {
    if let Some((label, action)) = offer {
        if !outcome.actions.iter().any(|(shown, _)| *shown == label) {
            outcome = outcome.offer(label, action);
        }
    }
    outcome
}

pub(crate) fn defaults(repo: &Repository) -> Result<BTreeMap<String, String>, String> {
    let listed = checked(repo.read_command().args(["for-each-ref", "--format=%(refname)%00%(symref)", "refs/remotes"]))?;
    let mut found = BTreeMap::new();
    for line in listed.split(|&byte| byte == b'\n') {
        let Some(at) = line.iter().position(|&byte| byte == 0) else { continue };
        let (Ok(name), Ok(target)) = (std::str::from_utf8(&line[..at]), std::str::from_utf8(&line[at + 1..])) else {
            continue;
        };
        let Some(remote) = name.strip_prefix("refs/remotes/").and_then(|rest| rest.strip_suffix("/HEAD")) else {
            continue;
        };
        let prefix = format!("refs/remotes/{remote}/");
        if let Some(branch) = target.strip_prefix(&prefix).filter(|branch| !branch.is_empty() && *branch != "HEAD") {
            found.insert(remote.to_owned(), branch.to_owned());
        }
    }
    Ok(found)
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() || tab.ui.dialog_open() {
        return;
    }
    match action {
        Action::Open { branch, onto } => {
            tab.ui.menu = None;
            let Some(request) = resolve(&tab.ui, &branch, onto.as_ref()) else {
                tab.refuse(format!("{} has no pull request page to open", branch.to_string_lossy()), cx);
                return;
            };
            start(tab, request, cx);
        }
    }
    cx.notify();
}

fn start(tab: &mut Tab, request: Request, cx: &mut Context<Tab>) {
    let Request { branch, tip, remote, there, push, page } = request;
    let Some(push) = push else {
        crate::handoff::open_url(cx, &page);
        return;
    };
    if let Some(reason) = tab.ui.operation.refusal().or_else(|| tab.ui.busy().then(|| "Wait for the current operation to finish".to_owned())) {
        tab.refuse(reason, cx);
        return;
    }
    tab.run_network(
        cx,
        crate::jobs::Spec::push(Some(remote.clone())),
        "Pushing the branch…",
        "Pushed Successfully",
        false,
        move |repo| {
            let destination = OsString::from(&there);
            push_to::send_tracked(&repo, &branch, &tip, &remote, &destination, push.track)
                .map(|sent| sent.outcome(&branch, &remote, &destination))
                .map_err(|why| Failure::from(format!("Push Failed — {why}")))
        },
        move |_, cx| crate::handoff::open_url(cx, &page),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    #[test]
    fn each_host_opens_its_own_new_pull_request_page() {
        let github = "https://github.com/cedar/trail";
        assert_eq!(page(github, github, "main", "ridge").as_deref(), Some("https://github.com/cedar/trail/compare/main...ridge?expand=1"));
        assert_eq!(
            page(github, github, "release/2", "feature/bridge #2").as_deref(),
            Some("https://github.com/cedar/trail/compare/release/2...feature/bridge%20%232?expand=1")
        );
        assert_eq!(
            page("https://github.com/team/trail", github, "trunk", "ridge").as_deref(),
            Some("https://github.com/team/trail/compare/trunk...cedar:ridge?expand=1")
        );
        assert_eq!(
            page("https://github.com/team/trail", "https://github.com/cedar/trail-fork", "trunk", "ridge").as_deref(),
            Some("https://github.com/team/trail/compare/trunk...cedar:trail-fork:ridge?expand=1")
        );
        let lab = "https://gitlab.com/group/sub/trail";
        assert_eq!(
            page(lab, lab, "main", "feature/x").as_deref(),
            Some("https://gitlab.com/group/sub/trail/-/merge_requests/new?merge_request[source_branch]=feature%2Fx&merge_request[target_branch]=main")
        );
        assert_eq!(
            page("https://gitlab.com/group/trail", lab, "main", "ridge").as_deref(),
            Some("https://gitlab.com/group/sub/trail/-/merge_requests/new?merge_request[source_branch]=ridge")
        );
        let bucket = "https://bitbucket.org/team/trail";
        assert_eq!(page(bucket, bucket, "main", "ridge").as_deref(), Some("https://bitbucket.org/team/trail/pull-requests/new?source=ridge&dest=main"));
        let other = "https://git.example.invalid:8443/team/trail";
        assert_eq!(page(other, other, "main", "ridge").as_deref(), Some(other));
        assert_eq!(page("https://github.example.invalid/team/trail", "https://github.example.invalid/team/trail", "main", "x").as_deref(), Some("https://github.example.invalid/team/trail/compare/main...x?expand=1"));
        assert_eq!(page("not a page", github, "main", "ridge"), None);
        for url in [page(github, github, "main", "a b").unwrap(), page(lab, lab, "main", "a&b=c").unwrap()] {
            assert!(crate::handoff::openable(&url), "{url}");
            assert!(!url.contains(' ') && !url.contains("a&b"), "{url}");
        }
    }

    #[test]
    fn a_remote_s_default_branch_is_read_from_its_head_and_nothing_changes() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        let dir = tempfile::tempdir().unwrap();
        for name in ["origin", "team/fork", "bare"] {
            let url = dir.path().join(name.replace('/', "-")).to_str().unwrap().to_owned();
            f.git(&["init", "-q", "--bare", "-b", "trunk", &url]);
            f.git(&["remote", "add", name, &url]);
            f.git(&["push", "-q", name, "main:refs/heads/trunk", "main:refs/heads/develop"]);
            f.git(&["fetch", "-q", name]);
        }
        f.git(&["remote", "set-head", "origin", "trunk"]);
        f.git(&["remote", "set-head", "team/fork", "develop"]);
        f.git(&["update-ref", "-d", "refs/remotes/bare/HEAD"]);
        let before = f.git(&["for-each-ref"]);
        let found = defaults(&f.repo).unwrap();
        assert_eq!(found.get("origin").map(String::as_str), Some("trunk"));
        assert_eq!(found.get("team/fork").map(String::as_str), Some("develop"));
        assert_eq!(found.get("bare"), None);
        assert_eq!(f.git(&["for-each-ref"]), before);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
    }

    fn published(f: &Fixture, bare: &str) -> String {
        let listed = String::from_utf8(f.git(&["ls-remote", "--", bare, "refs/heads/ridge"])).unwrap();
        listed.split('\t').next().unwrap_or_default().to_owned()
    }

    #[test]
    fn pushing_for_a_pull_request_publishes_the_branch_and_tracks_it() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git").to_str().unwrap().to_owned();
        f.git(&["init", "-q", "--bare", "-b", "main", &bare]);
        f.git(&["remote", "add", "origin", &bare]);
        f.git(&["push", "-q", "-u", "origin", "main"]);
        f.git(&["switch", "-qc", "ridge"]);
        fs::write(f.dir.join("ridge.txt"), "Climb\n").unwrap();
        f.git(&["add", "--", "ridge.txt"]);
        f.git(&["commit", "-qm", "Climb the ridge"]);
        let tip = String::from_utf8(f.git(&["rev-parse", "ridge"])).unwrap().trim().to_owned();
        let index = f.git(&["ls-files", "--stage"]);
        let sent = push_to::send_tracked(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("ridge"), true).unwrap();
        assert_eq!(sent, push_to::Sent::Created);
        assert_eq!(published(&f, &bare), tip);
        assert_eq!(f.git(&["config", "branch.ridge.remote"]), b"origin\n");
        assert_eq!(f.git(&["config", "branch.ridge.merge"]), b"refs/heads/ridge\n");
        assert_eq!(f.git(&["rev-parse", "refs/remotes/origin/ridge"]), format!("{tip}\n").as_bytes());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/ridge\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert!(f.git(&["status", "--porcelain"]).is_empty());

        f.git(&["switch", "-qc", "spur", "main"]);
        f.git(&["config", "branch.spur.remote", "origin"]);
        f.git(&["config", "branch.spur.merge", "refs/heads/main"]);
        let main = String::from_utf8(f.git(&["rev-parse", "main"])).unwrap().trim().to_owned();
        assert_eq!(push_to::send_tracked(&f.repo, OsStr::new("spur"), &main, "origin", OsStr::new("spur"), false).unwrap(), push_to::Sent::Created);
        assert_eq!(f.git(&["config", "branch.spur.merge"]), b"refs/heads/main\n");
    }

    #[test]
    fn a_branch_that_moved_since_the_menu_opened_is_not_pushed() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git").to_str().unwrap().to_owned();
        f.git(&["init", "-q", "--bare", "-b", "main", &bare]);
        f.git(&["remote", "add", "origin", &bare]);
        f.git(&["switch", "-qc", "ridge"]);
        let seen = String::from_utf8(f.git(&["rev-parse", "ridge"])).unwrap().trim().to_owned();
        fs::write(f.dir.join("ridge.txt"), "Climb\n").unwrap();
        f.git(&["add", "--", "ridge.txt"]);
        f.git(&["commit", "-qm", "Climb the ridge"]);
        let refused = push_to::send_tracked(&f.repo, OsStr::new("ridge"), &seen, "origin", OsStr::new("ridge"), true);
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(published(&f, &bare), "");
        assert!(f.repo.command().args(["config", "branch.ridge.remote"]).output().unwrap().stdout.is_empty());
        f.git(&["remote", "remove", "origin"]);
        let tip = String::from_utf8(f.git(&["rev-parse", "ridge"])).unwrap().trim().to_owned();
        assert!(push_to::send_tracked(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("ridge"), true).is_err());
        assert!(f.repo.command().args(["config", "branch.ridge.remote"]).output().unwrap().stdout.is_empty());
    }
}
