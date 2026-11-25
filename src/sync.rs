mod backend;

use crate::repository::Run;
use crate::{
    Tab, jobs, kit,
    notice::{self, Failure, Outcome},
    prompt::{self, Answer, Spec},
    remotes::{PushError, Rejection},
    repository::Repository,
    theme::*,
    view::{self, Desk},
};
use gpui::{Context, Div, SharedString, Window, div, prelude::*, px, rgb};
use std::ffi::{OsStr, OsString};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Target, busy, networking, on_repo, operating};
    use view::Ui;
    fn attached(ui: &Ui, detached: &str) -> Option<String> {
        if ui.detached {
            return Some(detached.to_owned());
        }
        ui.head.is_none().then(|| "Check out a local branch with a commit".to_owned())
    }
    fn no_upstream(ui: &Ui, to: &str) -> Option<String> {
        ui.upstream.is_none().then(|| format!("This branch has no upstream to {to}"))
    }
    fn fetch(ui: &Ui, target: &Target) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(networking(ui).or_else(|| operating(ui)).or_else(|| {
            ui.refs
                .remotes
                .is_empty()
                .then(|| "This repository has no remote to fetch from".to_owned())
        }))
    }
    fn pull(ui: &Ui, target: &Target) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(
            networking(ui)
                .or_else(|| operating(ui))
                .or_else(|| attached(ui, DETACHED_PULL))
                .or_else(|| no_upstream(ui, "pull from")),
        )
    }
    fn push(ui: &Ui, target: &Target) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(
            networking(ui)
                .or_else(|| operating(ui))
                .or_else(|| attached(ui, DETACHED_PUSH))
                .or_else(|| no_upstream(ui, "push to")),
        )
    }
    fn branch<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a crate::refs::NamedRef, bool)> {
        let Target::Branch(name) = target else { return None };
        if ui.refs.remotes.is_empty() {
            return None;
        }
        let item = ui.refs.local(name).filter(|item| item.oid.is_some())?;
        Some((item, !ui.detached && ui.branch.as_bytes() == name.as_encoded_bytes()))
    }
    fn as_toolbar(ui: &Ui, id: &str, cx: &gpui::App) -> Availability {
        match crate::commands::find(id) {
            Some(command) => (command.when)(ui, &Target::Repo, cx),
            None => Availability::Hidden,
        }
    }
    fn unset(item: &crate::refs::NamedRef) -> Option<String> {
        item.upstream.is_none().then(|| format!("Set an upstream for {} first", item.name.to_string_lossy()))
    }
    &[
        Command::new(
            "repo.fetch-all",
            Group::Sync,
            |_, _| "Fetch all remotes".into(),
            |ui, target, _| fetch(ui, target),
            |_, _| Some(view::Action::Sync(Action::FetchAll)),
        )
        .words(&["download", "update remotes"])
        .icon("fetch")
        .keys("cmd-l")
        .bar(Bar::Repository),
        Command::new(
            "remote.fetch",
            Group::Sync,
            |_, target| match target {
                Target::Remote(name) => format!("Fetch {name}"),
                _ => String::new(),
            },
            |ui, target, _| match target {
                Target::Remote(_) => Availability::unless(networking(ui).or_else(|| operating(ui))),
                _ => Availability::Hidden,
            },
            |_, target| match target {
                Target::Remote(name) => Some(view::Action::Sync(Action::FetchRemote(name.clone()))),
                _ => None,
            },
        )
        .words(&["download", "update remote"])
        .icon("fetch"),
        Command::new(
            "repo.fetch-prune",
            Group::Sync,
            |_, _| PRUNE.into(),
            |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
            |_, _| Some(view::Action::Sync(Action::TogglePrune)),
        )
        .words(&["prune", "auto-prune", "deleted branches", "stale remote branches"])
        .icon("fetch"),
        Command::new(
            "repo.pull",
            Group::Sync,
            |ui, _| match ui.sync.strategy() {
                Strategy::FastForwardIfPossible => "Pull".into(),
                strategy => strategy.title().into(),
            },
            |ui, target, _| pull(ui, target),
            |_, _| Some(view::Action::Sync(Action::Pull)),
        )
        .words(&["download", "update", "merge remote"])
        .icon("arrow-down")
        .keys("cmd-shift-l")
        .bar(Bar::Repository),
        Command::new(
            "repo.pull-ff",
            Group::Sync,
            |_, _| Strategy::FastForwardIfPossible.title().into(),
            |ui, target, _| pull(ui, target),
            |_, _| Some(view::Action::Sync(Action::PullWith(Strategy::FastForwardIfPossible))),
        )
        .words(&["merge remote", "pull merge"])
        .icon("arrow-down"),
        Command::new(
            "repo.pull-ff-only",
            Group::Sync,
            |_, _| Strategy::FastForwardOnly.title().into(),
            |ui, target, _| pull(ui, target),
            |_, _| Some(view::Action::Sync(Action::PullWith(Strategy::FastForwardOnly))),
        )
        .words(&["fast forward", "pull ff only"])
        .icon("arrow-down"),
        Command::new(
            "repo.pull-rebase",
            Group::Sync,
            |_, _| Strategy::Rebase.title().into(),
            |ui, target, _| pull(ui, target),
            |_, _| Some(view::Action::Sync(Action::PullWith(Strategy::Rebase))),
        )
        .words(&["rebase", "pull rebase", "linear history"])
        .icon("arrow-down"),
        Command::new(
            "repo.push",
            Group::Sync,
            |_, _| "Push".into(),
            |ui, target, _| push(ui, target),
            |_, _| Some(view::Action::Sync(Action::Push)),
        )
        .words(&["upload", "send"])
        .icon("arrow-up")
        .keys("cmd-shift-k")
        .bar(Bar::Repository),
        Command::new(
            "branch.push",
            Group::Sync,
            |ui, target| match branch(ui, target) {
                Some((item, _)) => match &item.upstream {
                    Some(upstream) => format!("Push {} to {upstream}", item.name.to_string_lossy()),
                    None => format!("Push {}", item.name.to_string_lossy()),
                },
                None => String::new(),
            },
            |ui, target, cx| {
                let Some((item, current)) = branch(ui, target) else {
                    return Availability::Hidden;
                };
                if current {
                    return as_toolbar(ui, "repo.push", cx);
                }
                Availability::unless(networking(ui).or_else(|| operating(ui)).or_else(|| unset(item)).or_else(|| {
                    let upstream = item.upstream.as_deref().unwrap_or_default();
                    (!item.gone && item.ahead == 0).then(|| {
                        format!("{} has no commits that {upstream} lacks", item.name.to_string_lossy())
                    })
                }))
            },
            |ui, target| {
                let (item, current) = branch(ui, target)?;
                if current {
                    return Some(view::Action::Sync(Action::Push));
                }
                let tip = item.oid.clone()?;
                Some(view::Action::Sync(Action::PushBranch { name: item.name.clone(), tip }))
            },
        )
        .words(&["upload branch", "send branch"])
        .icon("arrow-up"),
        Command::new(
            "branch.pull",
            Group::Sync,
            |ui, target| match branch(ui, target) {
                Some((item, _)) => match &item.upstream {
                    Some(upstream) => format!("Pull {} from {upstream}", item.name.to_string_lossy()),
                    None => format!("Pull {}", item.name.to_string_lossy()),
                },
                None => String::new(),
            },
            |ui, target, cx| {
                let Some((item, current)) = branch(ui, target) else {
                    return Availability::Hidden;
                };
                if current {
                    return as_toolbar(ui, "repo.pull", cx);
                }
                Availability::unless(networking(ui).or_else(|| operating(ui)).or_else(|| unset(item)).or_else(|| {
                    let (name, upstream) = (item.name.to_string_lossy(), item.upstream.as_deref().unwrap_or_default());
                    if item.gone {
                        Some(format!("{upstream} no longer exists"))
                    } else if item.ahead > 0 && item.behind > 0 {
                        Some(format!("{name} and {upstream} have both moved on. Check out {name} to merge them"))
                    } else {
                        None
                    }
                }))
            },
            |ui, target| {
                let (item, current) = branch(ui, target)?;
                if current {
                    return Some(view::Action::Sync(Action::Pull));
                }
                let tip = item.oid.clone()?;
                Some(view::Action::Sync(Action::PullBranch { name: item.name.clone(), tip }))
            },
        )
        .words(&["download branch", "update branch"])
        .icon("arrow-down"),
        Command::new(
            "branch.fast-forward",
            Group::Sync,
            |ui, target| match branch(ui, target).and_then(|(item, _)| Some((item, item.upstream.as_deref()?))) {
                Some((item, upstream)) => format!("Fast-forward {} to {upstream}", item.name.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| {
                let Some((item, false)) = branch(ui, target) else {
                    return Availability::Hidden;
                };
                let Some(upstream) = item.upstream.as_deref().filter(|_| !item.gone && item.behind > 0) else {
                    return Availability::Hidden;
                };
                if ui.refs.oid_of(&Target::RemoteBranch(upstream.into())).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    (item.ahead > 0).then(|| {
                        format!(
                            "{} has commits that {upstream} does not, so it cannot be fast-forwarded",
                            item.name.to_string_lossy()
                        )
                    })
                }))
            },
            |ui, target| {
                let (item, _) = branch(ui, target)?;
                let upstream = item.upstream.clone()?;
                let to = ui.refs.oid_of(&Target::RemoteBranch(upstream.clone().into()))?.to_owned();
                let tip = item.oid.clone()?;
                Some(view::Action::Sync(Action::FastForward { name: item.name.clone(), tip, upstream, to }))
            },
        )
        .words(&["update branch", "fast forward", "catch up"])
        .icon("arrow-down"),
    ]
};

pub(crate) const PRUNE: &str = "Remove branches that were deleted on the remote";

pub(crate) const DETACHED_PULL: &str = "You are not on a branch. Check out a branch to pull into it";
pub(crate) const DETACHED_PUSH: &str = "You are not on a branch. Create a branch here to push your commits";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Strategy {
    #[default]
    FastForwardIfPossible,
    FastForwardOnly,
    Rebase,
}

impl Strategy {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Strategy::FastForwardIfPossible => "Pull (fast-forward if possible)",
            Strategy::FastForwardOnly => "Pull (fast-forward only)",
            Strategy::Rebase => "Pull (rebase)",
        }
    }

    fn button(self) -> &'static str {
        match self {
            Strategy::FastForwardIfPossible => "Pull",
            Strategy::FastForwardOnly => "Pull (ff only)",
            Strategy::Rebase => "Pull (rebase)",
        }
    }

}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct PullDefault {
    pub(crate) fetch: bool,
    #[serde(deserialize_with = "crate::storage::lenient")]
    pub(crate) strategy: Strategy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Fetch,
    Pull(Strategy),
}

impl PullDefault {
    fn is(self, mode: Mode) -> bool {
        match mode {
            Mode::Fetch => self.fetch,
            Mode::Pull(strategy) => !self.fetch && self.strategy == strategy,
        }
    }

    fn with(self, mode: Mode) -> Self {
        match mode {
            Mode::Fetch => Self { fetch: true, ..self },
            Mode::Pull(strategy) => Self { fetch: false, strategy },
        }
    }
}

pub(crate) struct State {
    options_open: bool,
    default: PullDefault,
    pub(crate) prune: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new(PullDefault::default(), true)
    }
}

impl State {
    pub(crate) fn new(default: PullDefault, prune: bool) -> Self {
        Self {
            options_open: false,
            default,
            prune,
        }
    }

    pub(crate) fn options_open(&self) -> bool {
        self.options_open
    }

    pub(crate) fn strategy(&self) -> Strategy {
        self.default.strategy
    }

    pub(crate) fn chosen(&self) -> PullDefault {
        self.default
    }

    pub(crate) fn button(&self) -> &'static str {
        if self.default.fetch { "Fetch" } else { self.default.strategy.button() }
    }
}

pub(crate) fn configured(repo: &Repository) -> Strategy {
    let read = |args: &[&str]| {
        repo.read_command()
            .args(["config", "get"])
            .args(args)
            .run()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_ascii_lowercase())
    };
    if read(&["pull.ff"]).as_deref() == Some("only") {
        return Strategy::FastForwardOnly;
    }
    let rebase = match read(&["pull.rebase"]) {
        Some(value) if value.is_empty() => read(&["--type=bool", "pull.rebase"]),
        value => value,
    };
    match rebase.as_deref() {
        Some("true" | "yes" | "on" | "1" | "merges" | "m" | "interactive" | "i") => Strategy::Rebase,
        _ => Strategy::FastForwardIfPossible,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Push,
    Pull,
    FetchAll,
    FetchRemote(String),
    TogglePrune,
    ToggleOptions,
    Button,
    PullWith(Strategy),
    SetDefault(Mode),
    Dismiss,
    PushBranch { name: OsString, tip: String },
    PullBranch { name: OsString, tip: String },
    FastForward { name: OsString, tip: String, upstream: String, to: String },
    PullInto(OsString),
    ForcePush { name: OsString, checked_out: bool },
    RestoreForced { remote: String, remote_ref: OsString, tracking: OsString, old: String, new: String },
}

#[derive(Clone, Debug)]
pub(crate) struct Forced {
    branch: String,
    tip: String,
    current: bool,
    remote: String,
    remote_ref: OsString,
    tracking_ref: OsString,
    expect: String,
    replaced: usize,
    subjects: Vec<(String, String)>,
    sent: usize,
}

impl Forced {
    fn upstream(&self) -> String {
        crate::guard::display(&self.tracking_ref)
    }
}

#[derive(Debug)]
pub(crate) enum Ask {
    Behind { name: OsString, tip: String, upstream: String, ahead: usize, behind: usize },
    Force(Forced),
}

const FORCE: usize = 0;

pub(crate) fn describe(ask: &Ask, _: &view::Ui) -> Spec {
    match ask {
        Ask::Behind { name, upstream, ahead, behind, .. } => behind_spec(name, upstream, *ahead, *behind),
        Ask::Force(forced) => force_spec(forced),
    }
}

fn behind_spec(name: &OsStr, upstream: &str, ahead: usize, behind: usize) -> Spec {
    let shown = name.to_string_lossy();
    Spec::new(format!("{shown} is behind {upstream}"), "Pull")
        .line(format!(
            "{upstream} has {} that {shown} does not. Pull to bring them in, then push.",
            notice::commits(behind)
        ))
        .line(match ahead {
            0 => format!("Or force push to take them off {upstream}."),
            n => format!("Or force push to replace them with your {}.", notice::commits(n)),
        })
        .choice("Force push…", true)
}

fn force_spec(forced: &Forced) -> Spec {
    let upstream = forced.upstream();
    let them = if forced.replaced == 1 { "it" } else { "them" };
    let what = match forced.sent {
        0 => format!("This takes {} off {upstream}.", notice::commits(forced.replaced)),
        sent => format!("This replaces {} on {upstream} with your {sent}.", notice::commits(forced.replaced)),
    };
    let mut spec = Spec::new(format!("Force push {} to {upstream}?", forced.branch), "Force push")
        .line(format!("{what} Anyone who already pulled {them} will have to recover by hand."));
    for (short, subject) in &forced.subjects {
        spec = spec.line(format!("{short}  {subject}"));
    }
    match forced.replaced.saturating_sub(forced.subjects.len()) {
        0 => {}
        1 => spec = spec.line("and 1 more commit"),
        n => spec = spec.line(format!("and {n} more commits")),
    }
    spec.line(format!(
        "It goes ahead only while {} still has {} at {}, where it was when you last fetched.",
        forced.remote,
        crate::guard::display(&forced.remote_ref),
        notice::short(&forced.expect)
    ))
    .danger()
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Behind { name, tip, .. } => match answer.choice {
            None => pull_into(tab, &name, Some(tip), cx),
            Some(FORCE) => force(tab, name, true, cx),
            Some(_) => {}
        },
        Ask::Force(forced) => {
            tab.run_network(
                cx,
                jobs::Spec::push(Some(forced.remote.clone())),
                "Force pushing…",
                "Force pushed",
                false,
                move |repo| {
                    let (detail, replaced, said) = backend::force_push(&repo, &forced).map_err(|err| force_refused(err, &forced))?;
                    if !replaced {
                        return Ok(told(Outcome::titled(format!("Pushed {}", forced.branch), detail), said));
                    }
                    let restore = Action::RestoreForced {
                        remote: forced.remote.clone(),
                        remote_ref: forced.remote_ref.clone(),
                        tracking: forced.tracking_ref.clone(),
                        old: forced.expect.clone(),
                        new: forced.tip.clone(),
                    };
                    Ok::<_, Failure>(told(
                        Outcome::titled(format!("Force pushed {}", forced.branch), detail)
                            .offer(format!("Restore {}", forced.upstream()), view::Action::Sync(restore))
                            .sticky(),
                        said,
                    ))
                },
                |_, _| {},
            );
        }
    }
}

fn force_refused(error: PushError, forced: &Forced) -> Failure {
    let upstream = forced.upstream();
    let PushError::Refused(report) = error else {
        return Failure::from(format!("Force Push Failed — {}", String::from(error)));
    };
    let raw = String::from(PushError::Refused(report.clone()));
    let branch = crate::guard::display(&forced.remote_ref);
    let text = match report.rejection().map(|(_, why)| why) {
        Some(Rejection::Stale) => {
            return Failure::from(format!("Force Push Failed — {upstream} changed since your last fetch."))
                .offer("Fetch", view::Action::Sync(Action::FetchAll))
                .output(raw);
        }
        Some(Rejection::Remote(reason)) if format!("{reason}\n{raw}").to_lowercase().contains("protected") => {
            format!("{} protects {branch}, so it cannot be force pushed.", forced.remote)
        }
        Some(Rejection::Remote(reason)) => format!("{} refused to replace {branch}: {reason}.", forced.remote),
        _ => return Failure::from(format!("Force Push Failed — {}", String::from(PushError::Refused(report)))).output(raw),
    };
    Failure::from(format!("Force Push Failed — {text}")).output(raw).worded()
}

fn restore_refused(error: PushError, shown: &str) -> Failure {
    match error {
        PushError::Refused(report) if matches!(report.rejection(), Some((_, Rejection::Stale))) => {
            Failure::from(format!("Restore Failed — {shown} changed since the force push. Fetch and look again."))
                .offer("Fetch", view::Action::Sync(Action::FetchAll))
                .output(String::from(PushError::Refused(report)))
        }
        err => Failure::from(format!("Restore Failed — {}", String::from(err))),
    }
}

fn refused_push(repo: &Repository, branch: &str, head: &str, current: bool, error: PushError) -> Failure {
    if !rejected(&error) {
        return Failure::from(format!("Push Failed — {}", String::from(error)));
    }
    let said = String::from(error);
    let text = match backend::behind_remote(repo, branch, head, current) {
        Ok((upstream, behind)) if behind > 0 => {
            format!("Push Rejected — {upstream} has {} that {branch} does not.", notice::commits(behind))
        }
        _ => format!("Push Rejected — The remote branch has commits that {branch} does not."),
    };
    let name = OsString::from(branch);
    let mut failure = Failure::from(text).output(said);
    if current {
        failure = failure.offer("Pull", view::Action::Sync(Action::PullInto(name.clone())));
    }
    failure.offer("Force push…", view::Action::Sync(Action::ForcePush { name, checked_out: current }))
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let writes = matches!(action, Action::FastForward { .. } | Action::ForcePush { .. });
    if tab.repo.is_none() && !matches!(action, Action::TogglePrune) || writes && tab.ui.busy() {
        return;
    }
    view::focus(&tab.ui, window);
    match action {
        Action::PushBranch { name, tip } => {
            let Some(branch) = utf8(tab, &name, cx) else { return };
            let remote = remote_of(tab, tab.ui.refs.local(&name).and_then(|item| item.upstream.as_deref()));
            tab.run_network(
                cx,
                jobs::Spec::push(remote),
                "Pushing the branch…",
                "Pushed Successfully",
                false,
                move |repo| {
                    backend::push_named(&repo, &branch, &tip)
                        .map(|(detail, said)| told(crate::notice::Outcome::titled(format!("Pushed {branch}"), detail), said))
                        .map_err(|err| refused_push(&repo, &branch, &tip, false, err))
                },
                |_, _| {},
            );
        }
        Action::PullBranch { name, tip } => {
            let Some(branch) = utf8(tab, &name, cx) else { return };
            let remote = remote_of(tab, tab.ui.refs.local(&name).and_then(|item| item.upstream.as_deref()));
            let (fetching, seen) = (branch.clone(), tip.clone());
            tab.run_pull(
                cx,
                jobs::Spec::pull(remote),
                "Pulling the branch…",
                "Pulled Successfully",
                false,
                move |repo| {
                    backend::pull_named_fetch(&repo, &fetching, &seen).map_err(|err| Failure::from(format!("Pull Failed — {err}")))
                },
                move |repo, fetched, _| {
                    backend::pull_named_integrate(&repo, &branch, &tip, fetched)
                        .map(|(title, detail)| crate::notice::Outcome::titled(title, detail))
                        .map_err(|err| format!("Pull Failed — {err}"))
                },
                |_, _| {},
                |_, _| {},
            );
        }
        Action::FastForward { name, tip, upstream, to } => {
            let Some(branch) = utf8(tab, &name, cx) else { return };
            let tracking = OsString::from(format!("refs/remotes/{upstream}"));
            tab.run_operation_then(
                cx,
                "Fast-forwarding the branch…",
                "Fast-forwarded",
                false,
                move |repo| {
                    backend::fast_forward(&repo, &branch, &tip, &tracking, &to)
                        .map(|(title, detail)| crate::notice::Outcome::titled(title, detail))
                        .map_err(|err| format!("Fast-forward Failed — {err}"))
                },
                |_, _| {},
            );
        }
        Action::ToggleOptions | Action::Dismiss => {
            tab.ui.sync.options_open =
                matches!(action, Action::ToggleOptions) && !tab.ui.sync.options_open;
            cx.notify();
            return;
        }
        Action::FetchAll => fetch_all(tab, cx),
        Action::FetchRemote(name) => fetch_remote(tab, name, cx),
        Action::TogglePrune => {
            toggle_prune(tab, cx);
            return;
        }
        Action::Push => push_current(tab, cx),
        Action::Pull => pull_current(tab, None, tab.ui.sync.strategy(), cx),
        Action::Button if tab.ui.sync.default.fetch => fetch_all(tab, cx),
        Action::Button => pull_current(tab, None, tab.ui.sync.strategy(), cx),
        Action::PullWith(strategy) => pull_current(tab, None, strategy, cx),
        Action::SetDefault(mode) => {
            set_default(tab, mode, cx);
            return;
        }
        Action::PullInto(name) => pull_into(tab, &name, None, cx),
        Action::ForcePush { name, checked_out } => {
            if checked_out && !checked_out_is(tab, &name) {
                let shown = name.to_string_lossy();
                let now = now_checked_out(tab);
                tab.error(
                    format!("Force Push Failed — This was offered for {shown}, but {now} now. Switch back to {shown} to force push it."),
                    cx,
                );
                return;
            }
            force(tab, name, checked_out, cx);
        }
        Action::RestoreForced { remote, remote_ref, tracking, old, new } => {
            let shown = crate::guard::display(&tracking);
            tab.run_network(
                cx,
                jobs::Spec::push(Some(remote.clone())),
                "Restoring the remote branch…",
                "Remote branch restored",
                false,
                move |repo| {
                    backend::put_back(&repo, &remote, &remote_ref, &tracking, &old, &new)
                        .map(|detail| Outcome::titled(format!("Restored {shown}"), detail))
                        .map_err(|err| restore_refused(err, &shown))
                },
                |_, _| {},
            );
        }
    }
    tab.ui.sync.options_open = false;
    cx.notify();
}

fn remote_of(tab: &Tab, upstream: Option<&str>) -> Option<String> {
    let (remote, _) = crate::branch::split_remote(&tab.ui.refs.remotes, OsStr::new(upstream?))?;
    Some(remote.to_owned())
}

fn fetch_all(tab: &mut Tab, cx: &mut Context<Tab>) {
    let remotes = &tab.ui.refs.remotes;
    let remote = (remotes.len() == 1).then(|| remotes[0].clone());
    let prune = tab.ui.sync.prune;
    tab.run_network_or(
        cx,
        jobs::Spec::fetch(remote),
        "Fetching all remotes…",
        "Fetched Successfully",
        false,
        move |repo| {
            backend::fetch_all(&repo, prune)
                .map(crate::notice::Outcome::detail)
                .map_err(|err| format!("Fetch Failed — {err}"))
        },
        |tab, _| crate::autofetch::by_hand(tab, true),
        |tab, _| crate::autofetch::by_hand(tab, false),
    );
}

pub(crate) fn fetch_every(repo: &Repository, prune: bool) -> Result<String, String> {
    backend::fetch_all(repo, prune)
}

pub(crate) fn fetch_remote(tab: &mut Tab, name: String, cx: &mut Context<Tab>) {
    if crate::remotes::validate_name(&name).is_err() {
        tab.error(format!("Fetch Failed — {name} is not a remote FastKraken can fetch."), cx);
        return;
    }
    let prune = tab.ui.sync.prune;
    let only = tab.ui.refs.remotes == [name.clone()];
    let progress = format!("Fetching {name}…");
    tab.run_network_or(
        cx,
        jobs::Spec::fetch(Some(name.clone())),
        &progress,
        "Fetched Successfully",
        false,
        move |repo| {
            backend::fetch_remote(&repo, &name, prune)
                .map(|detail| crate::notice::Outcome::detail(format!("From {name}: {detail}")))
                .map_err(|err| format!("Fetch Failed — {err}"))
        },
        move |tab, _| crate::autofetch::by_hand(tab, only),
        |tab, _| crate::autofetch::by_hand(tab, false),
    );
}

fn toggle_prune(tab: &mut Tab, cx: &mut Context<Tab>) {
    let prune = !tab.ui.sync.prune;
    tab.ui.sync.prune = prune;
    crate::settings::Settings::update(cx, |settings| settings.prune_on_fetch = prune);
    let (title, detail) = if prune {
        ("Fetch removes deleted remote branches", "A branch deleted on the remote loses its remote-tracking branch here. Local branches stay.")
    } else {
        ("Fetch keeps deleted remote branches", "Remote-tracking branches stay after their branch is deleted on the remote.")
    };
    tab.ui.notices.show(notice::Notice::new(view::Tone::Info, title, detail));
    cx.notify();
}

fn set_default(tab: &mut Tab, mode: Mode, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let default = tab.ui.sync.default.with(mode);
    tab.ui.sync.default = default;
    let common = repo.common_dir().to_owned();
    crate::settings::Memory::update(cx, |memory| memory.change_repo(&common, |view| view.pull = Some(default)));
    cx.notify();
}

pub(crate) fn restore(tab: &mut Tab, configured: Strategy, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    let remembered = crate::settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.pull);
    let default = remembered.unwrap_or(PullDefault { fetch: false, strategy: configured });
    tab.ui.sync = State::new(default, crate::settings::settings(cx).prune_on_fetch);
    if remembered.is_none() {
        crate::settings::Memory::update(cx, |memory| memory.change_repo(&common, |view| view.pull = Some(default)));
    }
}

fn utf8(tab: &mut Tab, name: &std::ffi::OsStr, cx: &mut Context<Tab>) -> Option<String> {
    let text = name.to_str().map(str::to_owned);
    if text.is_none() {
        tab.error("FastKraken can only sync a branch whose name is valid UTF-8.".to_owned(), cx);
    }
    text
}

fn checked_out_is(tab: &Tab, name: &OsStr) -> bool {
    !tab.ui.detached && tab.ui.branch.as_bytes() == name.as_encoded_bytes()
}

fn now_checked_out(tab: &Tab) -> String {
    if tab.ui.detached || tab.ui.branch.is_empty() {
        "HEAD is detached".to_owned()
    } else {
        format!("{} is checked out", tab.ui.branch)
    }
}

fn pull_ready(tab: &mut Tab, cx: &mut Context<Tab>) -> Option<(String, String)> {
    let Some(head) = tab.ui.head.clone().filter(|_| !tab.ui.detached) else {
        tab.error("Pull Failed — Check out a local branch with a commit.".to_owned(), cx);
        return None;
    };
    if let Some(reason) = tab.ui.operation.refusal() {
        tab.error(format!("Pull Failed — {reason}."), cx);
        return None;
    }
    Some((tab.ui.branch.clone(), head))
}

fn pull_current(tab: &mut Tab, seen: Option<String>, strategy: Strategy, cx: &mut Context<Tab>) {
    let stash = tab.ui.autostash.always;
    pull_with(tab, seen, strategy, stash, cx);
}

pub(crate) fn pull_stashing(tab: &mut Tab, head: String, strategy: Strategy, cx: &mut Context<Tab>) {
    pull_with(tab, Some(head), strategy, true, cx);
}

fn pull_with(tab: &mut Tab, seen: Option<String>, strategy: Strategy, stash: bool, cx: &mut Context<Tab>) {
    let Some((branch, head)) = pull_ready(tab, cx) else { return };
    let head = seen.unwrap_or(head);
    let remote = remote_of(tab, tab.ui.upstream.as_deref());
    let started = std::sync::Arc::new(std::sync::Mutex::new(None));
    let rebasing = started.clone();
    let kept = crate::autostash::Kept::default();
    let stopped = kept.clone();
    let (fetching, seen) = (branch.clone(), head.clone());
    tab.run_pull(
        cx,
        jobs::Spec::pull(remote),
        "Pulling from upstream…",
        "Pulled Successfully",
        true,
        move |repo| {
            backend::pull_fetch(&repo, &fetching, &seen)
                .map_err(|err| Failure { text: format!("Pull Failed — {}", err.text), ..err })
        },
        move |repo, fetched, moved| {
            backend::pull_integrate(&repo, &branch, &head, moved, fetched, strategy, &rebasing, stash, &kept)
                .map(crate::notice::Outcome::detail)
                .map_err(|err| match err.text.starts_with("Pulled from ") {
                    true => err,
                    false => Failure { text: format!("Pull Failed — {}", err.text), ..err },
                })
        },
        |tab, _| tab.follow = crate::Follow::ChangesOrHead,
        move |tab, _| {
            if let Some((oid, label)) = started.lock().unwrap_or_else(|e| e.into_inner()).take() {
                tab.ui.operation.started = Some((crate::operation::Kind::Rebase, oid, label));
            }
            stopped.remember(&mut tab.ui);
        },
    );
}

fn pull_into(tab: &mut Tab, name: &OsStr, seen: Option<String>, cx: &mut Context<Tab>) {
    if let Some(reason) = tab.ui.operation.refusal() {
        tab.error(format!("Pull Failed — {reason}."), cx);
        return;
    }
    if !checked_out_is(tab, name) {
        let shown = name.to_string_lossy();
        let now = now_checked_out(tab);
        tab.error(format!("Pull Failed — This Pull was for {shown}, but {now} now. Switch back to {shown} to pull it."), cx);
        return;
    }
    pull_current(tab, seen, tab.ui.sync.strategy(), cx);
}

fn push_current(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(head) = tab.ui.head.clone().filter(|_| !tab.ui.detached) else {
        tab.error("Push Failed — Check out a local branch with a commit.".to_owned(), cx);
        return;
    };
    let branch = tab.ui.branch.clone();
    push_tip(tab, branch, head, cx);
}

pub(crate) fn push_tip(tab: &mut Tab, branch: String, head: String, cx: &mut Context<Tab>) {
    let behind = tab.ui.refs.current(&branch).and_then(|item| {
        let upstream = item.upstream.clone().filter(|_| !item.gone && item.behind > 0)?;
        Some((upstream, item.ahead, item.behind))
    });
    if let Some((upstream, ahead, behind)) = behind {
        tab.ui.menu = None;
        let ask = Ask::Behind { name: branch.into(), tip: head, upstream, ahead, behind };
        prompt::ask(&mut tab.ui, prompt::Kind::Sync(ask), cx);
        return;
    }
    let remote = remote_of(tab, tab.ui.upstream.as_deref());
    tab.run_network(
        cx,
        jobs::Spec::push(remote),
        "Pushing to upstream…",
        "Pushed Successfully",
        false,
        move |repo| {
            backend::push(&repo, &branch, &head)
                .map(|(detail, said)| told(crate::notice::Outcome::detail(detail), said))
                .map_err(|err| refused_push(&repo, &branch, &head, true, err))
        },
        |_, _| {},
    );
}

fn force(tab: &mut Tab, name: OsString, current: bool, cx: &mut Context<Tab>) {
    if let Some(reason) = tab.ui.operation.refusal() {
        tab.error(format!("Force Push Failed — {reason}."), cx);
        return;
    }
    let (Some(repo), Some(branch)) = (tab.repo.clone(), utf8(tab, &name, cx)) else { return };
    let Some(tip) = tab.ui.refs.local(&name).and_then(|item| item.oid.clone()) else {
        tab.error(format!("Force Push Failed — {branch} no longer exists."), cx);
        return;
    };
    tab.ui.menu = None;
    let epoch = tab.epoch;
    let seen = tip.clone();
    Tab::work(
        cx,
        move || backend::force_preview(&repo, &branch, &seen, current),
        move |tab, preview, cx| {
            if tab.epoch != epoch
                || tab.ui.refs.local(&name).and_then(|item| item.oid.as_deref()) != Some(tip.as_str())
                || tab.ui.busy()
                || tab.ui.dialog_open()
            {
                return;
            }
            match preview {
                Ok(forced) if forced.replaced == 0 => {
                    let upstream = forced.upstream();
                    tab.ui.notices.show(notice::Notice::new(
                        view::Tone::Info,
                        "Nothing to force",
                        format!("{upstream} has no commits that {} lacks. Push it instead.", forced.branch),
                    ));
                }
                Ok(forced) => prompt::ask(&mut tab.ui, prompt::Kind::Sync(Ask::Force(forced)), cx),
                Err(error) => tab.error(format!("Force Push Failed — {error}"), cx),
            }
            cx.notify();
        },
    );
}

pub(crate) fn told(mut outcome: Outcome, said: crate::remotes::Said) -> Outcome {
    if said.lines.is_empty() {
        return outcome;
    }
    let lines = said.lines.join("\n");
    outcome.detail = if outcome.detail.is_empty() { lines } else { format!("{}\n{lines}", outcome.detail) };
    outcome = outcome.output(said.output);
    match said.link {
        Some(link) => outcome.offer("Open", view::Action::Handoff(crate::handoff::Action::OpenUrl(link))).sticky(),
        None => outcome,
    }
}

fn rejected(error: &crate::remotes::PushError) -> bool {
    use crate::remotes::Rejection;
    matches!(error.rejection(), Some(Rejection::NonFastForward | Rejection::FetchFirst))
}

pub(crate) fn fetch_tip(ui: &view::Ui) -> String {
    match ui.jobs.refusal() {
        Some(reason) => reason,
        None if !ui.refs.remotes.is_empty() => {
            format!("{}\n{}", crate::commands::tip_with_keys("Fetch all remotes", "repo.fetch-all"), crate::autofetch::line(ui))
        }
        None => "This repository has no remote to fetch from".to_owned(),
    }
}

pub(crate) fn toolbar<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let attached = ui.head.is_some() && !ui.detached;
    let running = ui.jobs.refusal();
    let idle = running.is_none() && !ui.operation.active();
    let has_remote = !ui.refs.remotes.is_empty();
    let (ahead, behind) = ui
        .refs
        .current(&ui.branch)
        .map_or((0, 0), |branch| (branch.ahead, branch.behind));
    let divider = || div().w(px(1.)).h(px(CONTROL)).bg(rgb(LINE_STRONG));
    let fetch_tip = || fetch_tip(ui);
    let fetch = kit::btn("sync-fetch", "Fetch")
        .ghost()
        .icon("fetch")
        .enabled(idle && has_remote)
        .tip(fetch_tip());
    let pull = kit::btn("sync-pull", ui.sync.button()).ghost().count(Some(behind));
    let pull = if ui.sync.chosen().fetch {
        pull.icon("fetch").enabled(idle && has_remote).tip(fetch_tip())
    } else {
        pull.icon("arrow-down")
            .enabled(idle && attached && ui.upstream.is_some())
            .tip(if let Some(reason) = &running {
                reason.clone()
            } else if ui.detached {
                DETACHED_PULL.to_owned()
            } else if ui.upstream.is_none() {
                "This branch has no upstream to pull from".to_owned()
            } else {
                crate::commands::tip_with_keys(ui.sync.chosen().strategy.title(), "repo.pull")
            })
    };
    let push = if ui.detached {
        kit::btn("sync-push", "Push")
            .ghost()
            .icon("arrow-up")
            .enabled(false)
            .tip(DETACHED_PUSH)
            .on(view::Action::Sync(Action::Push), cx)
    } else if ui.upstream.is_some() {
        kit::btn("sync-push", "Push")
            .ghost()
            .icon("arrow-up")
            .count(Some(ahead))
            .enabled(idle && attached)
            .tip(running.clone().unwrap_or_else(|| crate::commands::tip_with_keys("Push", "repo.push")))
            .on(view::Action::Sync(Action::Push), cx)
    } else if has_remote {
        kit::btn("sync-push", "Publish branch")
            .ghost()
            .icon("arrow-up")
            .enabled(idle && !ui.busy())
            .on(view::Action::Publish(crate::publish::Action::PushBranch), cx)
    } else {
        kit::btn("sync-push", "Publish…")
            .ghost()
            .icon("arrow-up")
            .enabled(idle && !ui.busy())
            .on(view::Action::Publish(crate::publish::Action::Publish), cx)
    };
    div()
        .relative()
        .flex()
        .items_center()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE_STRONG))
        .bg(rgb(RAISED))
        .child(fetch.on(view::Action::Sync(Action::FetchAll), cx))
        .child(divider())
        .child(pull.on(view::Action::Sync(Action::Button), cx))
        .child(
            kit::btn("sync-options", "")
                .ghost()
                .icon("chevron-down")
                .selected(ui.sync.options_open())
                .enabled(idle)
                .tip("Pull options")
                .on(view::Action::Sync(Action::ToggleOptions), cx)
                .px(px(5.)),
        )
        .child(divider())
        .child(push)
        .when(ui.sync.options_open(), |d| {
            d.child(kit::popover(options(app, cx), false))
        })
        .when(ui.publish.branch_open(), |d| {
            d.child(kit::popover(crate::publish::branch_form(app, cx), false))
        })
}

pub(crate) const PULL_MENU: [(&str, Mode); 4] = [
    ("repo.fetch-all", Mode::Fetch),
    ("repo.pull-ff", Mode::Pull(Strategy::FastForwardIfPossible)),
    ("repo.pull-ff-only", Mode::Pull(Strategy::FastForwardOnly)),
    ("repo.pull-rebase", Mode::Pull(Strategy::Rebase)),
];

pub(crate) fn options<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let default = ui.sync.chosen();
    let mut menu = kit::float().w(px(340.)).py_1().child(
        div().px_3().pt_1().pb_1().child(kit::label(
            ui.upstream
                .as_deref()
                .map(|name| format!("Fetch, or pull from {name}"))
                .unwrap_or_else(|| "No upstream configured".to_owned()),
        )),
    );
    for (index, (id, mode)) in PULL_MENU.into_iter().enumerate() {
        let Some(entry) = crate::commands::find(id).and_then(|command| crate::commands::entry(command, ui, &crate::commands::Target::Repo, cx))
        else {
            continue;
        };
        if index == 1 {
            menu = menu.child(kit::separator());
        }
        let chosen = default.is(mode);
        let item = kit::item(SharedString::from(format!("sync-{id}")), entry.title)
            .check(chosen)
            .enabled(entry.action.is_some())
            .tip(entry.disabled.map(Into::into));
        let item = match entry.action {
            Some(action) => item.on(action, cx),
            None => item.inert(),
        };
        let mark = if chosen {
            div().flex_shrink_0().px_2().child(kit::muted("Default").text_size(px(TEXT_TINY))).into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .px_2()
                .child(kit::link(
                    SharedString::from(format!("sync-default-{id}")),
                    "Set as default",
                    view::Action::Sync(Action::SetDefault(mode)),
                    cx,
                ))
                .into_any_element()
        };
        menu = menu.child(div().flex().items_center().child(div().flex_1().min_w_0().child(item)).child(mark));
    }
    menu = menu.child(
        div()
            .px_3()
            .pt_1()
            .pb_2()
            .child(kit::muted("A row runs once. Set as default makes it what the Pull button does in this repository.").text_size(px(11.5))),
    );
    if let Some(entry) = crate::commands::find("repo.fetch-prune")
        .and_then(|command| crate::commands::entry(command, ui, &crate::commands::Target::Repo, cx))
    {
        let item = kit::item("sync-prune", entry.title).check(ui.sync.prune);
        let item = match entry.action {
            Some(action) => item.on(action, cx),
            None => item.inert(),
        };
        menu = menu.child(kit::separator()).child(item);
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_notice_carries_what_the_server_said_and_opens_its_link() {
        let said = crate::remotes::Said {
            lines: vec!["Create a pull request for 'topic' by visiting:".into(), "https://example.invalid/o/r/pull/new/topic".into()],
            link: Some("https://example.invalid/o/r/pull/new/topic".into()),
            output: "remote: Create a pull request for 'topic' by visiting:\nremote:   https://example.invalid/o/r/pull/new/topic".into(),
        };
        let outcome = told(Outcome::detail("Created origin/topic."), said.clone());
        assert_eq!(
            outcome.detail,
            "Created origin/topic.\nCreate a pull request for 'topic' by visiting:\nhttps://example.invalid/o/r/pull/new/topic"
        );
        assert!(outcome.sticky);
        assert_eq!(outcome.output.as_deref(), Some(said.output.as_str()));
        assert!(matches!(
            outcome.actions.as_slice(),
            [(label, view::Action::Handoff(crate::handoff::Action::OpenUrl(url)))] if label.as_ref() == "Open" && url == "https://example.invalid/o/r/pull/new/topic"
        ));
        let plain = told(Outcome::detail("1 commit to origin/main."), crate::remotes::Said { link: None, ..said });
        assert!(!plain.sticky && plain.actions.is_empty());
        let quiet = told(Outcome::detail("1 commit to origin/main."), Default::default());
        assert_eq!((quiet.detail.as_str(), quiet.output, quiet.sticky), ("1 commit to origin/main.", None, false));
    }

    fn forced(replaced: usize, sent: usize, subjects: &[(&str, &str)]) -> Forced {
        Forced {
            branch: "main".into(),
            tip: "1".repeat(40),
            current: true,
            remote: "origin".into(),
            remote_ref: "refs/heads/main".into(),
            tracking_ref: "refs/remotes/origin/main".into(),
            expect: "abcdef1".to_owned() + &"2".repeat(33),
            replaced,
            subjects: subjects.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            sent,
        }
    }

    #[test]
    fn the_force_push_prompt_is_red_and_says_what_leaves_the_remote() {
        let spec = force_spec(&forced(1, 1, &[("abc1234", "Hang the gate")]));
        assert_eq!(spec.title, "Force push main to origin/main?");
        assert_eq!(spec.confirm.as_ref(), "Force push");
        assert!(spec.danger);
        assert_eq!(
            spec.lines,
            [
                "This replaces 1 commit on origin/main with your 1. Anyone who already pulled it will have to recover by hand.",
                "abc1234  Hang the gate",
                "It goes ahead only while origin still has main at abcdef1, where it was when you last fetched.",
            ]
        );
        let many: Vec<(String, String)> = (0..5).map(|i| (format!("c{i}"), format!("Step {i}"))).collect();
        let listed: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let spec = force_spec(&forced(7, 0, &listed));
        assert_eq!(spec.lines[0], "This takes 7 commits off origin/main. Anyone who already pulled them will have to recover by hand.");
        assert_eq!(spec.lines[6], "and 2 more commits");
    }

    #[test]
    fn a_repository_first_pulls_the_way_its_configuration_says() {
        let f = crate::repository::tests::Fixture::new();
        assert_eq!(configured(&f.repo), Strategy::FastForwardIfPossible);
        for (rebase, ff, expected) in [
            (Some("true"), None, Strategy::Rebase),
            (Some("merges"), None, Strategy::Rebase),
            (Some("interactive"), Some("true"), Strategy::Rebase),
            (Some("false"), None, Strategy::FastForwardIfPossible),
            (None, Some("only"), Strategy::FastForwardOnly),
            (None, Some("false"), Strategy::FastForwardIfPossible),
            (Some("true"), Some("only"), Strategy::FastForwardOnly),
        ] {
            for (key, value) in [("pull.rebase", rebase), ("pull.ff", ff)] {
                match value {
                    Some(value) => drop(f.git(&["config", key, value])),
                    None => drop(crate::repository::checked(f.repo.command().args(["config", "--unset-all", key]))),
                }
            }
            assert_eq!(configured(&f.repo), expected, "pull.rebase={rebase:?} pull.ff={ff:?}");
        }
        for key in ["pull.ff", "pull.rebase"] {
            drop(crate::repository::checked(f.repo.command().args(["config", "--unset-all", key])));
        }
        let config = f.repo.git_dir().join("config");
        let plain = std::fs::read_to_string(&config).unwrap();
        for (line, expected) in [("\trebase\n", Strategy::Rebase), ("\trebase =\n", Strategy::FastForwardIfPossible)] {
            std::fs::write(&config, format!("{plain}[pull]\n{line}")).unwrap();
            assert_eq!(configured(&f.repo), expected, "{line:?}");
            std::fs::write(&config, &plain).unwrap();
        }
    }

    #[test]
    fn the_pull_button_follows_the_default_and_fetch_keeps_the_way_to_pull() {
        let mut state = State::default();
        assert_eq!((state.button(), state.strategy()), ("Pull", Strategy::FastForwardIfPossible));
        assert!(state.chosen().is(Mode::Pull(Strategy::FastForwardIfPossible)));
        state.default = state.default.with(Mode::Pull(Strategy::Rebase));
        assert_eq!((state.button(), state.strategy()), ("Pull (rebase)", Strategy::Rebase));
        state.default = state.default.with(Mode::Fetch);
        assert_eq!((state.button(), state.strategy()), ("Fetch", Strategy::Rebase));
        assert!(state.chosen().is(Mode::Fetch) && !state.chosen().is(Mode::Pull(Strategy::Rebase)));
        state.default = state.default.with(Mode::Pull(Strategy::FastForwardOnly));
        assert_eq!(state.button(), "Pull (ff only)");
        let titles: Vec<_> = PULL_MENU.iter().map(|(id, _)| crate::commands::find(id).map(|c| c.id)).collect();
        assert_eq!(titles, [Some("repo.fetch-all"), Some("repo.pull-ff"), Some("repo.pull-ff-only"), Some("repo.pull-rebase")]);
        let stored = serde_json::to_value(PullDefault { fetch: true, strategy: Strategy::Rebase }).unwrap();
        assert_eq!(stored, serde_json::json!({"fetch": true, "strategy": "rebase"}));
    }

    #[test]
    fn push_behind_its_upstream_offers_pull_first_and_force_push_in_red() {
        let spec = behind_spec(OsStr::new("main"), "origin/main", 1, 2);
        assert_eq!(spec.title, "main is behind origin/main");
        assert_eq!(spec.confirm.as_ref(), "Pull");
        assert!(!spec.danger);
        assert_eq!(
            spec.lines,
            [
                "origin/main has 2 commits that main does not. Pull to bring them in, then push.",
                "Or force push to replace them with your 1 commit.",
            ]
        );
        assert_eq!(spec.choices.len(), 1);
        assert_eq!((spec.choices[FORCE].label.as_ref(), spec.choices[FORCE].danger), ("Force push…", true));
        assert!(matches!(prompt::first(&spec), prompt::Stop::Confirm));
        assert!(matches!(prompt::on_return(&spec, Some(prompt::Stop::Choice(FORCE))), prompt::Action::Cancel));
        let spec = behind_spec(OsStr::new("main"), "origin/main", 0, 1);
        assert_eq!(spec.lines[1], "Or force push to take them off origin/main.");
    }
}
