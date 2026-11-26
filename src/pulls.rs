use crate::{
    Tab, auth, kit,
    commands::Target,
    github::{Cached, CheckRun, Error, PullRequest, PullState, RunState},
    notice::{Notice, Outcome},
    repository::{Repository, Run, checked},
    theme::*,
    view::{self, Desk, Menu, Tone, Ui},
};
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, MouseButton, MouseDownEvent, Stateful, Window, div,
    prelude::*, px, rgb,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    time::{Duration, Instant},
};

pub(crate) const EVERY: Duration = Duration::from_secs(300);
const LOOK: Duration = Duration::from_secs(30);
pub(crate) const FORK_OFFER: &str = "Add the fork as a remote";

#[derive(Clone, Debug, Default)]
struct Listing {
    etag: Option<String>,
    pulls: Vec<PullRequest>,
}

impl Listing {
    fn take(&mut self, cached: Cached<Vec<PullRequest>>) {
        if let Cached::Fresh { value, etag } = cached {
            self.pulls = value;
            self.etag = etag;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Trouble {
    Permission { what: &'static str, owner: String, install: String },
    Lapsed,
    Failed(String),
}

impl Trouble {
    pub(crate) fn title(&self) -> String {
        match self {
            Self::Permission { what, owner, .. } => format!("FastKraken needs permission to read {what} on {owner}"),
            Self::Lapsed => "Reconnect GitHub to see pull requests".to_owned(),
            Self::Failed(_) => "Pull requests could not be read from GitHub".to_owned(),
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Permission { what, owner, .. } => {
                let permission = if *what == "checks" { "Checks: read" } else { "Pull requests: read" };
                format!("The FastKraken GitHub App on {owner} has not been granted {permission}. Review its permissions on GitHub and approve the request, then they show here.")
            }
            Self::Lapsed => format!("{}. Reconnect GitHub, then pull requests show on their branches again.", auth::LAPSED),
            Self::Failed(why) => why.clone(),
        }
    }

    fn offer(&self) -> Option<(&'static str, view::Action)> {
        match self {
            Self::Permission { install, .. } => {
                Some(("Review permissions…", view::Action::Handoff(crate::handoff::Action::OpenUrl(install.clone()))))
            }
            Self::Lapsed => Some((
                "Reconnect GitHub…",
                view::Action::Connect(crate::connect::Action::Reconnect { again: Some(Box::new(view::Action::Pulls(Action::Refresh))) }),
            )),
            Self::Failed(_) => None,
        }
    }

    fn told(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Hosted {
    pub(crate) full_name: String,
    open: Listing,
    closed: Listing,
    pub(crate) trouble: Option<Trouble>,
}

#[derive(Clone, Debug)]
pub(crate) struct Checks {
    remote: String,
    sha: String,
    etag: Option<String>,
    pub(crate) runs: Vec<CheckRun>,
    pub(crate) trouble: Option<Trouble>,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) hosts: BTreeMap<String, Hosted>,
    pub(crate) checks: Option<Checks>,
    pub(crate) bound: bool,
    pub(crate) present: BTreeSet<String>,
    told: BTreeSet<String>,
    ticket: u64,
    checks_ticket: u64,
    fetching: bool,
    pub(crate) due: bool,
    last: Option<Instant>,
}

impl State {
    pub(crate) fn loaded(&self) -> bool {
        self.last.is_some() && !self.fetching
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Refresh,
    Reveal { remote: String, number: u64 },
    Checkout { remote: String, number: u64 },
    AddFork { name: String, url: String },
}

impl Action {
    pub(crate) fn during_operation(&self) -> bool {
        matches!(self, Self::Refresh | Self::Reveal { .. })
    }
}

pub(crate) fn repo_of(ui: &Ui, remote: &str) -> Option<String> {
    let page = crate::pull_request::web(ui, remote)?;
    let path = page.strip_prefix("https://github.com/")?;
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/')).then(|| path.to_owned())
}

pub(crate) fn find_in<'a>(
    hosts: &'a BTreeMap<String, Hosted>,
    repo: &str,
    branch: &str,
    closed_too: bool,
) -> Option<(&'a str, &'a PullRequest)> {
    let matches = |pull: &PullRequest| {
        pull.head == branch && pull.head_repo.as_deref().is_some_and(|head| head.eq_ignore_ascii_case(repo))
    };
    let open = hosts.iter().find_map(|(remote, host)| host.open.pulls.iter().find(|p| matches(p)).map(|p| (remote.as_str(), p)));
    open.or_else(|| {
        if !closed_too {
            return None;
        }
        hosts.iter().find_map(|(remote, host)| host.closed.pulls.iter().find(|p| matches(p)).map(|p| (remote.as_str(), p)))
    })
}

fn numbered(hosts: &BTreeMap<String, Hosted>, number: u64) -> Option<(&str, &PullRequest)> {
    hosts.iter().find_map(|(remote, host)| host.open.pulls.iter().find(|p| p.number == number).map(|p| (remote.as_str(), p)))
}

fn home(ui: &Ui) -> Option<String> {
    let remotes = &ui.refs.remotes;
    remotes.iter().find(|remote| *remote == "origin").or_else(|| remotes.first()).cloned()
}

pub(crate) fn of_local<'a>(ui: &'a Ui, name: &OsStr) -> Option<(&'a str, &'a PullRequest)> {
    if ui.pulls.hosts.is_empty() {
        return None;
    }
    let item = ui.refs.local(name)?;
    let local = name.to_str()?;
    let upstream = item
        .upstream
        .as_deref()
        .and_then(|upstream| crate::branch::split_remote(&ui.refs.remotes, OsStr::new(upstream)));
    let (remote, there) = match upstream {
        Some((remote, there)) => (remote.to_owned(), there.into_string().ok()?),
        None => {
            if let Some(number) = local.strip_prefix("pr/").and_then(|n| n.parse().ok()) {
                if let Some(found) = numbered(&ui.pulls.hosts, number) {
                    return Some(found);
                }
            }
            (home(ui)?, local.to_owned())
        }
    };
    find_in(&ui.pulls.hosts, &repo_of(ui, &remote)?, &there, true)
}

pub(crate) fn of_remote<'a>(ui: &'a Ui, full: &OsStr) -> Option<(&'a str, &'a PullRequest)> {
    if ui.pulls.hosts.is_empty() {
        return None;
    }
    let (remote, there) = crate::branch::split_remote(&ui.refs.remotes, full)?;
    find_in(&ui.pulls.hosts, &repo_of(ui, remote)?, there.to_str()?, false)
}

pub(crate) fn open_of_local<'a>(ui: &'a Ui, name: &OsStr) -> Option<(&'a str, &'a PullRequest)> {
    of_local(ui, name).filter(|(_, pull)| matches!(pull.state, PullState::Open | PullState::Draft))
}

pub(crate) fn of_target<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a str, &'a PullRequest)> {
    match target {
        Target::Branch(name) => of_local(ui, name),
        Target::RemoteBranch(full) => of_remote(ui, full),
        Target::Repo if !ui.detached && !ui.branch.is_empty() => of_local(ui, OsStr::new(&ui.branch)),
        Target::PullRequest { remote, number } => {
            let (name, host) = ui.pulls.hosts.get_key_value(remote)?;
            host.open.pulls.iter().find(|pull| pull.number == *number).map(|pull| (name.as_str(), pull))
        }
        _ => None,
    }
}

pub(crate) fn current(ui: &Ui) -> Option<(&str, &PullRequest)> {
    if ui.detached || ui.branch.is_empty() {
        return None;
    }
    of_local(ui, OsStr::new(&ui.branch)).filter(|(_, pull)| matches!(pull.state, PullState::Open | PullState::Draft))
}

pub(crate) fn listed(ui: &Ui) -> Vec<(&str, &PullRequest)> {
    let mut seen = BTreeSet::new();
    let mut all: Vec<(&str, &PullRequest)> = ui
        .pulls
        .hosts
        .iter()
        .flat_map(|(remote, host)| host.open.pulls.iter().map(move |pull| (remote.as_str(), pull)))
        .filter(|(_, pull)| seen.insert(pull.page.clone()))
        .collect();
    all.sort_by_key(|(_, pull)| std::cmp::Reverse(pull.number));
    all
}

pub(crate) fn shown_in(ui: &Ui, filter: &str) -> Vec<usize> {
    listed(ui)
        .iter()
        .enumerate()
        .filter(|(_, (_, pull))| {
            filter.is_empty()
                || pull.title.to_lowercase().contains(filter)
                || pull.author.to_lowercase().contains(filter)
                || format!("#{}", pull.number).contains(filter)
                || pull.head.to_lowercase().contains(filter)
        })
        .map(|(index, _)| index)
        .collect()
}

pub(crate) fn notes(ui: &Ui) -> Vec<(String, Option<(&'static str, view::Action)>)> {
    let mut found: Vec<(String, Option<(&'static str, view::Action)>)> = Vec::new();
    let troubles = ui.pulls.hosts.values().filter_map(|host| host.trouble.as_ref());
    let checks = ui.pulls.checks.as_ref().and_then(|checks| checks.trouble.as_ref());
    for trouble in troubles.chain(checks) {
        let title = trouble.title();
        if !found.iter().any(|(shown, _)| *shown == title) {
            found.push((title, trouble.offer()));
        }
    }
    if found.is_empty() && ui.pulls.loaded() && listed(ui).is_empty() {
        found.push(("No open pull requests".to_owned(), None));
    }
    found
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overall {
    Running,
    Passed,
    Failed,
}

pub(crate) fn summary(runs: &[CheckRun]) -> Option<(Overall, String)> {
    if runs.is_empty() {
        return None;
    }
    let total = runs.len();
    let failing: Vec<&str> = runs.iter().filter(|run| run.state == RunState::Failed).map(|run| run.name.as_str()).collect();
    let running = runs.iter().filter(|run| run.state == RunState::Running).count();
    let checks = |count: usize| if count == 1 { "check".to_owned() } else { "checks".to_owned() };
    Some(if !failing.is_empty() {
        (Overall::Failed, format!("{} of {total} {} failed: {}", failing.len(), checks(total), failing.join(", ")))
    } else if running > 0 {
        (Overall::Running, format!("{running} of {total} {} running", checks(total)))
    } else {
        (Overall::Passed, format!("All {total} {} passed", checks(total)))
    })
}

pub(crate) fn checks_of<'a>(ui: &'a Ui, remote: &str, pull: &PullRequest) -> Option<&'a Checks> {
    let (current_remote, current) = current(ui)?;
    if current_remote != remote || current.number != pull.number {
        return None;
    }
    ui.pulls.checks.as_ref().filter(|checks| checks.remote == remote && checks.sha == pull.head_sha)
}

pub(crate) fn colour(state: PullState) -> u32 {
    match state {
        PullState::Open => ADDED,
        PullState::Draft => TEXT_3,
        PullState::Merged => TAG,
        PullState::Closed => DELETED,
    }
}

fn dot(overall: Overall) -> u32 {
    match overall {
        Overall::Running => ACCENT,
        Overall::Passed => ADDED,
        Overall::Failed => DELETED,
    }
}

pub(crate) fn label(pull: &PullRequest) -> String {
    match pull.state {
        PullState::Open => format!("#{}", pull.number),
        PullState::Draft => format!("#{} Draft", pull.number),
        PullState::Merged => format!("#{} Merged", pull.number),
        PullState::Closed => format!("#{} Closed", pull.number),
    }
}

pub(crate) fn tip(ui: &Ui, remote: &str, pull: &PullRequest) -> String {
    let said = match pull.state {
        PullState::Open => format!("{} wants to merge {} into {}", pull.author, pull.head, pull.base),
        PullState::Draft => format!("Draft by {}: {} into {}", pull.author, pull.head, pull.base),
        PullState::Merged => format!("{} merged {} into {}", pull.author, pull.head, pull.base),
        PullState::Closed => format!("Closed without merging, by {}", pull.author),
    };
    let mut lines = vec![format!("#{}: {}", pull.number, pull.title), said];
    if let Some(checks) = checks_of(ui, remote, pull) {
        match (&checks.trouble, summary(&checks.runs)) {
            (Some(trouble), _) => lines.push(trouble.title()),
            (None, Some((_, text))) => lines.push(format!("Checks: {text}")),
            (None, None) => {}
        }
    }
    lines.join("\n")
}

pub(crate) fn badge<T: Desk>(ui: &Ui, remote: &str, pull: &PullRequest, id: impl Into<ElementId>, cx: &Context<T>) -> Stateful<Div> {
    let shade = colour(pull.state);
    let overall = checks_of(ui, remote, pull).filter(|checks| checks.trouble.is_none()).and_then(|checks| summary(&checks.runs));
    let target = Target::PullRequest { remote: remote.to_owned(), number: pull.number };
    let open = pull.page.clone();
    div()
        .id(id)
        .h(px(16.))
        .px(px(5.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(3.))
        .rounded(px(RADIUS_CHIP))
        .bg(soft(shade, 0x26))
        .cursor_pointer()
        .child(kit::icon("merge").size(px(9.)).text_color(rgb(shade)))
        .child(div().text_size(px(TEXT_TINY)).text_color(rgb(shade)).whitespace_nowrap().child(label(pull)))
        .when_some(overall, |d, (overall, _)| d.child(div().size(px(6.)).rounded_full().bg(rgb(dot(overall)))))
        .tooltip(kit::tip(tip(ui, remote, pull)))
        .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
            cx.stop_propagation();
            if event.click_count() > 1 {
                app.dispatch(view::Action::Handoff(crate::handoff::Action::OpenUrl(open.clone())), w, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                cx.stop_propagation();
                app.dispatch(view::Action::Menu(Some(Menu::Object { target: target.clone(), at: event.position })), w, cx)
            }),
        )
}

pub(crate) fn branch_badge<T: Desk>(ui: &Ui, target: &Target, id: impl Into<ElementId>, cx: &Context<T>) -> Option<Stateful<Div>> {
    let (remote, pull) = match target {
        Target::Branch(_) | Target::RemoteBranch(_) => of_target(ui, target)?,
        _ => return None,
    };
    Some(badge(ui, remote, pull, id, cx))
}

pub(crate) fn side_row<T: Desk>(ui: &Ui, index: usize, row: usize, pointed: bool, cx: &mut Context<T>) -> AnyElement {
    let all = listed(ui);
    let Some((remote, pull)) = all.get(index).copied() else { return div().into_any_element() };
    let shown = matches!(&ui.selection, Some(view::Selection::Commit(id)) if *id == pull.head_sha);
    let (reveal_remote, checkout_remote, menu_remote) = (remote.to_owned(), remote.to_owned(), remote.to_owned());
    let number = pull.number;
    kit::row(("side-pull", row), false)
        .h(px(TREE_ROW))
        .pl(px(12.))
        .pr(px(10.))
        .gap(px(6.))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(colour(pull.state)))
                .font_weight(FontWeight::MEDIUM)
                .child(format!("#{}", pull.number)),
        )
        .child(kit::fit(pull.title.clone()).size(12.5).color(if shown { TEXT } else { TEXT_2 }).grow())
        .when(pull.state == PullState::Draft, |d| d.child(kit::label("draft")))
        .child(div().max_w(px(90.)).flex_shrink_0().child(kit::fit(pull.author.clone()).size(TEXT_SMALL).color(TEXT_3)))
        .when(pointed, |d| d.tooltip(kit::tip(format!("{}\nClick to show its head commit. Double-click to check it out.", tip(ui, remote, pull)))))
        .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
            let action = if event.click_count() > 1 {
                view::Action::Activate(Target::PullRequest { remote: checkout_remote.clone(), number })
            } else {
                view::Action::Pulls(Action::Reveal { remote: reveal_remote.clone(), number })
            };
            app.dispatch(action, w, cx);
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = Target::PullRequest { remote: menu_remote.clone(), number };
                app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
            }),
        )
        .into_any_element()
}

pub(crate) fn note_row<T: Desk>(ui: &Ui, index: usize, row: usize, cx: &mut Context<T>) -> AnyElement {
    let all = notes(ui);
    let Some((text, offer)) = all.get(index).cloned() else { return div().into_any_element() };
    div()
        .id(("side-pull-note", row))
        .h(px(TREE_ROW))
        .w_full()
        .pl(px(29.))
        .pr(px(10.))
        .flex()
        .items_center()
        .gap_2()
        .child(kit::fit(text.clone()).size(12.).color(TEXT_3).grow())
        .tooltip(kit::tip(text))
        .when_some(offer, |d, (label, action)| {
            d.child(
                div()
                    .id(("side-pull-offer", row))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(rgb(ACCENT))
                    .child(label)
                    .on_click(cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx))),
            )
        })
        .into_any_element()
}

pub(crate) fn row_action(ui: &Ui, index: usize) -> Option<view::Action> {
    let (remote, pull) = listed(ui).get(index).copied()?;
    Some(view::Action::Pulls(Action::Reveal { remote: remote.to_owned(), number: pull.number }))
}

pub(crate) fn note_action(ui: &Ui, index: usize) -> Option<view::Action> {
    notes(ui).into_iter().nth(index).and_then(|(_, offer)| offer.map(|(_, action)| action))
}

pub(crate) fn merged_first(ui: &Ui, target: &Target, entries: &mut Vec<crate::commands::Entry>) {
    let Target::Branch(name) = target else { return };
    let Some((_, pull)) = of_local(ui, name) else { return };
    if pull.state != PullState::Merged {
        return;
    }
    let Some(at) = entries.iter().position(|entry| entry.id == "branch.delete") else { return };
    let mut delete = entries.remove(at);
    delete.title = format!("Delete {} — merged in #{}…", name.to_string_lossy(), pull.number);
    if let Some(first) = entries.first() {
        delete.group = first.group;
    }
    entries.insert(0, delete);
}

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy, on_repo, operating};
    fn pull_of<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a str, &'a PullRequest)> {
        match target {
            Target::PullRequest { .. } => of_target(ui, target),
            _ => None,
        }
    }
    fn on_branch<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a str, &'a PullRequest)> {
        match target {
            Target::Branch(_) | Target::RemoteBranch(_) | Target::Repo => of_target(ui, target),
            _ => None,
        }
    }
    fn checkout_title(ui: &Ui, target: &Target) -> String {
        match pull_of(ui, target).map(|(remote, pull)| (remote, pull, checkout_kind(ui, remote, pull))) {
            Some((remote, pull, Kind::Same)) => format!("Check out {remote}/{}", pull.head),
            Some((_, pull, Kind::Fork)) => format!("Check out #{} as pr/{}", pull.number, pull.number),
            None => String::new(),
        }
    }
    fn checkout_when(ui: &Ui, target: &Target) -> Availability {
        let Some((remote, pull)) = pull_of(ui, target) else { return Availability::Hidden };
        let local = match checkout_kind(ui, remote, pull) {
            Kind::Same => pull.head.clone(),
            Kind::Fork => format!("pr/{}", pull.number),
        };
        Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| ui.jobs.refusal()).or_else(|| {
            (!ui.detached && ui.branch == local)
                .then(|| format!("{local} is already checked out"))
                .or_else(|| crate::worktree::in_use(ui, OsStr::new(&local)))
        }))
    }
    &[
        Command::new(
            "pull.reveal",
            Group::Switch,
            |ui, target| pull_of(ui, target).map_or_else(String::new, |(_, pull)| format!("Show the head of #{}", pull.number)),
            |ui, target, _| if pull_of(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| {
                let (remote, pull) = pull_of(ui, target)?;
                Some(view::Action::Pulls(Action::Reveal { remote: remote.to_owned(), number: pull.number }))
            },
        )
        .icon("crosshair"),
        Command::new(
            "pull.checkout",
            Group::Switch,
            |ui, target| checkout_title(ui, target),
            |ui, target, _| checkout_when(ui, target),
            |ui, target| {
                let (remote, pull) = pull_of(ui, target)?;
                Some(view::Action::Pulls(Action::Checkout { remote: remote.to_owned(), number: pull.number }))
            },
        )
        .words(&["checkout", "pull request", "review"])
        .icon("branch"),
        Command::new(
            "pull.open",
            Group::Open,
            |ui, target| pull_of(ui, target).map_or_else(String::new, |(_, pull)| format!("Open #{} on GitHub", pull.number)),
            |ui, target, _| if pull_of(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| Some(view::Action::Handoff(crate::handoff::Action::OpenUrl(pull_of(ui, target)?.1.page.clone()))),
        )
        .icon("globe"),
        Command::new(
            "pull.copy-link",
            Group::Copy,
            |_, _| "Copy link".into(),
            |ui, target, _| if pull_of(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| Some(view::Action::CopyText { text: pull_of(ui, target)?.1.page.clone(), what: "link" }),
        )
        .icon("copy"),
        Command::new(
            "branch.view-pull-request",
            Group::Sync,
            |ui, target| match on_branch(ui, target) {
                Some((_, pull)) => format!("View pull request #{} on GitHub", pull.number),
                None => String::new(),
            },
            |ui, target, _| if on_branch(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |ui, target| Some(view::Action::Handoff(crate::handoff::Action::OpenUrl(on_branch(ui, target)?.1.page.clone()))),
        )
        .words(&["pull request", "pr", "github", "checks"])
        .icon("globe"),
        Command::new(
            "pulls.refresh",
            Group::View,
            |_, _| "Refresh pull requests".into(),
            |ui, target, _| if on_repo(ui, target) && ui.pulls.bound { Availability::Enabled } else { Availability::Hidden },
            |_, _| Some(view::Action::Pulls(Action::Refresh)),
        )
        .words(&["pull requests", "github", "checks", "reload"])
        .icon("fetch"),
    ]
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Same,
    Fork,
}

pub(crate) fn checkout_kind(ui: &Ui, remote: &str, pull: &PullRequest) -> Kind {
    let base = ui.pulls.hosts.get(remote).map(|host| host.full_name.as_str());
    match (&pull.head_repo, base) {
        (Some(head), Some(base)) if head.eq_ignore_ascii_case(base) => Kind::Same,
        _ => Kind::Fork,
    }
}

pub(crate) fn after_network(ui: &mut Ui, kind: crate::jobs::Kind) {
    use crate::jobs::Kind as K;
    if matches!(kind, K::Fetch | K::Push | K::Pull | K::Publish) {
        ui.pulls.due = true;
    }
}

pub(crate) fn refs_loaded(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &tab.ui.pulls;
    let stale = state.last.is_none_or(|last| last.elapsed() >= EVERY);
    if state.due || stale {
        poll(tab, cx);
        return;
    }
    let wanted = current(&tab.ui).map(|(remote, pull)| (remote.to_owned(), pull.head_sha.clone()));
    let have = tab.ui.pulls.checks.as_ref().map(|checks| (checks.remote.clone(), checks.sha.clone()));
    if wanted != have {
        checks(tab, cx);
    }
    recheck(tab, cx);
}

pub(crate) fn arm(tab: &mut Tab, cx: &mut Context<Tab>) {
    if cfg!(test) || crate::acceptance::replaying() || tab.bench.is_some() {
        return;
    }
    let epoch = tab.epoch;
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(LOOK).await;
            let going = tab.update(cx, |tab, cx| {
                if tab.epoch != epoch {
                    return false;
                }
                let state = &tab.ui.pulls;
                if state.bound && !state.fetching && state.last.is_some_and(|last| last.elapsed() >= EVERY) {
                    poll(tab, cx);
                }
                true
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

struct Gathered {
    remote: String,
    full_name: String,
    found: Result<(Cached<Vec<PullRequest>>, Cached<Vec<PullRequest>>), Trouble>,
}

type Known = BTreeMap<String, (String, Option<String>, Option<String>)>;

fn trouble(error: Error, what: &'static str, owner: &str, install: String) -> Trouble {
    match error {
        Error::Denied(_) => Trouble::Permission { what, owner: owner.to_owned(), install },
        Error::Unauthorized(_) => Trouble::Lapsed,
        error => Trouble::Failed(error.to_string()),
    }
}

fn connection_trouble(error: String) -> Trouble {
    if error.starts_with(auth::LAPSED) { Trouble::Lapsed } else { Trouble::Failed(error) }
}

fn gather(repo: &Repository, known: &Known) -> (bool, Vec<Gathered>) {
    let addresses = crate::remotes::addresses(repo).unwrap_or_default();
    let mut found = Vec::new();
    for (remote, url) in auth::bindings(repo).unwrap_or_default() {
        if addresses.get(&remote).and_then(crate::remotes::Addresses::single) != Some(url.as_str()) {
            continue;
        }
        let Some((owner, name)) = auth::github_repo(&url) else { continue };
        let full_name = format!("{owner}/{name}");
        let (open_etag, closed_etag) = match known.get(&remote) {
            Some((seen, open, closed)) if *seen == full_name => (open.clone(), closed.clone()),
            _ => (None, None),
        };
        let fetched = match auth::connection(repo, &remote) {
            Ok(Some(connection)) => {
                let install = connection.client.installation_url();
                let read = |open: bool, etag: Option<&str>| {
                    connection
                        .client
                        .pull_requests(&connection.token, &owner, &name, open, etag)
                        .map_err(|error| trouble(error, "pull requests", &owner, install.clone()))
                };
                read(true, open_etag.as_deref()).and_then(|open| Ok((open, read(false, closed_etag.as_deref())?)))
            }
            Ok(None) => continue,
            Err(error) => Err(connection_trouble(error)),
        };
        found.push(Gathered { remote, full_name, found: fetched });
    }
    (!found.is_empty(), found)
}

fn heads(hosts: &BTreeMap<String, Hosted>) -> Vec<String> {
    let mut all: Vec<String> = hosts.values().flat_map(|host| host.open.pulls.iter().map(|pull| pull.head_sha.clone())).collect();
    all.sort();
    all.dedup();
    all
}

pub(crate) fn present(repo: &Repository, shas: &[String]) -> BTreeSet<String> {
    if shas.is_empty() {
        return BTreeSet::new();
    }
    let input: Vec<u8> = shas.iter().filter(|sha| crate::repository::validate_oid(sha).is_ok()).flat_map(|sha| format!("{sha}\n").into_bytes()).collect();
    let listed = crate::operation::with_input(repo.read_command().args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]), input);
    listed
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|line| line.strip_suffix(" commit").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn recheck(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let shas = heads(&tab.ui.pulls.hosts);
    if shas.iter().all(|sha| tab.ui.pulls.present.contains(sha)) {
        return;
    }
    let (epoch, ticket) = (tab.epoch, tab.ui.pulls.ticket);
    Tab::work(
        cx,
        move || present(&repo, &shas),
        move |tab, found, cx| {
            if tab.epoch == epoch && tab.ui.pulls.ticket == ticket && !tab.ui.pulls.fetching {
                tab.ui.pulls.present = found;
                cx.notify();
            }
        },
    );
}

fn apply(state: &mut State, bound: bool, gathered: Vec<Gathered>) {
    state.bound = bound;
    let mut hosts = BTreeMap::new();
    for Gathered { remote, full_name, found } in gathered {
        let mut host = state.hosts.remove(&remote).filter(|host| host.full_name == full_name).unwrap_or_default();
        host.full_name = full_name;
        match found {
            Ok((open, closed)) => {
                host.open.take(open);
                host.closed.take(closed);
                host.trouble = None;
            }
            Err(trouble) => {
                if !matches!(trouble, Trouble::Failed(_)) {
                    host.open = Listing::default();
                    host.closed = Listing::default();
                }
                host.trouble = Some(trouble);
            }
        }
        hosts.insert(remote, host);
    }
    state.hosts = hosts;
}

fn tell(tab: &mut Tab) {
    let mut troubles: Vec<(String, Trouble)> = tab
        .ui
        .pulls
        .hosts
        .iter()
        .filter_map(|(remote, host)| host.trouble.clone().map(|trouble| (format!("pulls:{remote}"), trouble)))
        .collect();
    if let Some(checks) = &tab.ui.pulls.checks {
        if let Some(trouble) = checks.trouble.clone() {
            troubles.push((format!("checks:{}", checks.remote), trouble));
        }
    }
    let keys: Vec<String> = troubles.iter().filter(|(_, t)| t.told()).map(|(key, t)| format!("{key}:{}", t.title())).collect();
    tab.ui.pulls.told.retain(|key| keys.contains(key));
    for ((_, trouble), key) in troubles.into_iter().filter(|(_, t)| t.told()).zip(keys) {
        if !tab.ui.pulls.told.insert(key) {
            continue;
        }
        let mut notice = Notice::new(Tone::Info, trouble.title(), trouble.detail());
        if let Some((label, action)) = trouble.offer() {
            notice = notice.offer(label, action);
        }
        tab.ui.notices.show(notice);
    }
}

pub(crate) fn poll(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.pulls;
    if state.fetching {
        state.due = true;
        return;
    }
    state.fetching = true;
    state.due = false;
    state.ticket += 1;
    let ticket = state.ticket;
    let epoch = tab.epoch;
    let known: Known = state
        .hosts
        .iter()
        .map(|(remote, host)| (remote.clone(), (host.full_name.clone(), host.open.etag.clone(), host.closed.etag.clone())))
        .collect();
    let known_heads = heads(&state.hosts);
    Tab::work(
        cx,
        move || {
            let (bound, gathered) = gather(&repo, &known);
            let mut shas = known_heads;
            for each in &gathered {
                if let Ok((Cached::Fresh { value, .. }, _)) = &each.found {
                    shas.extend(value.iter().map(|pull| pull.head_sha.clone()));
                }
            }
            shas.sort();
            shas.dedup();
            let found = present(&repo, &shas);
            (bound, gathered, found)
        },
        move |tab, (bound, gathered, found), cx| {
            if tab.epoch != epoch || tab.ui.pulls.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.pulls;
            state.fetching = false;
            state.last = Some(Instant::now());
            state.present = found;
            apply(state, bound, gathered);
            tell(tab);
            if tab.ui.pulls.due {
                poll(tab, cx);
            } else {
                checks(tab, cx);
            }
            cx.notify();
        },
    );
}

fn checks(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.pulls.checks_ticket += 1;
    let ticket = tab.ui.pulls.checks_ticket;
    let Some((remote, pull)) = current(&tab.ui).map(|(remote, pull)| (remote.to_owned(), pull.clone())) else {
        tab.ui.pulls.checks = None;
        tell(tab);
        return;
    };
    let Some((owner, name)) = tab.ui.pulls.hosts.get(&remote).and_then(|host| host.full_name.split_once('/')).map(|(o, n)| (o.to_owned(), n.to_owned()))
    else {
        return;
    };
    let previous = tab.ui.pulls.checks.as_ref().filter(|checks| checks.remote == remote && checks.sha == pull.head_sha);
    let etag = previous.and_then(|checks| checks.etag.clone());
    let epoch = tab.epoch;
    let (sha, asked) = (pull.head_sha.clone(), remote.clone());
    Tab::work(
        cx,
        move || {
            let connection = auth::connection(&repo, &asked).map_err(connection_trouble)?.ok_or(Trouble::Lapsed)?;
            let install = connection.client.installation_url();
            connection
                .client
                .check_runs(&connection.token, &owner, &name, &sha, etag.as_deref())
                .map_err(|error| trouble(error, "checks", &owner, install))
        },
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.pulls.checks_ticket != ticket {
                return;
            }
            let kept = tab.ui.pulls.checks.take().filter(|checks| checks.remote == remote && checks.sha == pull.head_sha);
            let mut checks = kept.unwrap_or(Checks { remote, sha: pull.head_sha.clone(), etag: None, runs: Vec::new(), trouble: None });
            match result {
                Ok(Cached::Fresh { value, etag }) => {
                    checks.runs = value;
                    checks.etag = etag;
                    checks.trouble = None;
                }
                Ok(Cached::Unchanged) => checks.trouble = None,
                Err(trouble) => {
                    if !matches!(trouble, Trouble::Failed(_)) {
                        checks.runs.clear();
                        checks.etag = None;
                    }
                    checks.trouble = Some(trouble);
                }
            }
            tab.ui.pulls.checks = Some(checks);
            tell(tab);
            cx.notify();
        },
    );
}

#[derive(Clone, Debug)]
pub(crate) struct ForkCheckout {
    pub(crate) remote: String,
    pub(crate) address: String,
    pub(crate) number: u64,
    pub(crate) seen: Option<String>,
}

impl ForkCheckout {
    pub(crate) fn branch(&self) -> String {
        format!("pr/{}", self.number)
    }
}

fn tip_of(repo: &Repository, full: &str) -> Result<Option<String>, String> {
    let output = repo
        .read_command()
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{full}^{{commit}}"))
        .run()
        .map_err(|e| e.to_string())?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

pub(crate) fn check_out_fork(repo: &Repository, checkout: &ForkCheckout) -> Result<String, String> {
    crate::remotes::validate_name(&checkout.remote)?;
    let addresses = crate::remotes::addresses(repo)?;
    let now = addresses.get(&checkout.remote).and_then(crate::remotes::Addresses::single);
    match now {
        Some(address) if address == checkout.address => {}
        Some(address) => {
            return Err(format!("{} points at {address} now, not {}. Nothing was fetched.", checkout.remote, checkout.address));
        }
        None => return Err(format!("{} is not a remote of this repository any more. Nothing was fetched.", checkout.remote)),
    }
    let branch = checkout.branch();
    let full = format!("refs/heads/{branch}");
    if tip_of(repo, &full)? != checkout.seen {
        return Err(format!("{branch} changed since the pull request was shown. Nothing was fetched."));
    }
    let head = repo.read_command().args(["symbolic-ref", "--quiet", "HEAD"]).run().map_err(|e| e.to_string())?;
    if String::from_utf8_lossy(&head.stdout).trim() == full {
        return Err(format!("{branch} is checked out. Switch to another branch first."));
    }
    checked(
        repo.network_command()?
            .args(["fetch", "--no-tags", "--no-write-fetch-head", "--", &checkout.remote])
            .arg(format!("pull/{}/head:{full}", checkout.number)),
    )?;
    checked(repo.command().args(["switch", "--no-guess", "--"]).arg(&branch))?;
    tip_of(repo, &full)?.ok_or_else(|| format!("{branch} was not created."))
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    tab.ui.menu = None;
    match action {
        Action::Refresh => poll(tab, cx),
        Action::Reveal { remote, number } => reveal(tab, remote, number, window, cx),
        Action::Checkout { remote, number } => checkout(tab, remote, number, window, cx),
        Action::AddFork { name, url } => add_fork(tab, name, url, cx),
    }
    cx.notify();
}

fn found(tab: &mut Tab, remote: &str, number: u64, cx: &mut Context<Tab>) -> Option<PullRequest> {
    let pull = of_target(&tab.ui, &Target::PullRequest { remote: remote.to_owned(), number }).map(|(_, pull)| pull.clone());
    if pull.is_none() {
        tab.refuse(format!("Pull request #{number} is no longer open"), cx);
    }
    pull
}

fn reveal(tab: &mut Tab, remote: String, number: u64, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(pull) = found(tab, &remote, number, cx) else { return };
    if tab.ui.pulls.present.contains(&pull.head_sha) {
        tab.dispatch(view::Action::Reveal(crate::reveal::Action::Commit(pull.head_sha.clone())), window, cx);
        return;
    }
    let mut notice = Notice::new(
        Tone::Info,
        format!("The head of #{number} has not been fetched"),
        format!("Its commit {} is not in this repository yet.", crate::notice::short(&pull.head_sha)),
    );
    notice = if checkout_kind(&tab.ui, &remote, &pull) == Kind::Same {
        notice.offer(format!("Fetch {remote}"), view::Action::Sync(crate::sync::Action::FetchRemote(remote.clone())))
    } else {
        notice.offer(format!("Check out as pr/{number}"), view::Action::Pulls(Action::Checkout { remote, number }))
    };
    tab.ui.notices.show(notice);
}

fn checkout(tab: &mut Tab, remote: String, number: u64, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(pull) = found(tab, &remote, number, cx) else { return };
    if let Some(reason) = tab.ui.operation.refusal().or_else(|| tab.ui.busy().then(|| "Wait for the current operation to finish".to_owned())) {
        tab.refuse(reason, cx);
        return;
    }
    if checkout_kind(&tab.ui, &remote, &pull) == Kind::Same {
        let full = OsString::from(format!("{remote}/{}", pull.head));
        if tab.ui.refs.oid_of(&Target::RemoteBranch(full.clone())).is_none() {
            tab.ui.notices.show(
                Notice::new(Tone::Info, format!("{} has not been fetched", full.to_string_lossy()), format!("Fetch {remote} first, then check out #{number}."))
                    .offer(format!("Fetch {remote}"), view::Action::Sync(crate::sync::Action::FetchRemote(remote.clone()))),
            );
            return;
        }
        tab.dispatch(view::Action::Branch(crate::branch::Action::CheckoutRemote(full)), window, cx);
        return;
    }
    let Some(address) = tab.ui.refs.addresses.get(&remote).and_then(crate::remotes::Addresses::single).map(str::to_owned) else {
        tab.refuse(format!("{remote} has no single address to fetch from"), cx);
        return;
    };
    let request = ForkCheckout {
        remote: remote.clone(),
        address,
        number,
        seen: tab.ui.refs.oid_of(&Target::Branch(format!("pr/{number}").into())).map(str::to_owned),
    };
    let fork = pull.head_repo.clone();
    let offer = fork.as_deref().and_then(|full_name| fork_offer(&tab.ui, full_name));
    let owner = fork.as_deref().and_then(|full| full.split_once('/')).map_or_else(|| "a deleted fork".to_owned(), |(owner, _)| format!("{owner}'s fork"));
    let head = pull.head.clone();
    tab.run_network(
        cx,
        crate::jobs::Spec::fetch(Some(remote.clone())).writing(),
        &format!("Fetching pull request #{number}…"),
        "Checked out pull request",
        true,
        move |repo| {
            let branch = request.branch();
            check_out_fork(&repo, &request)
                .map(|tip| {
                    let outcome = Outcome::titled(
                        format!("Checked out {branch}"),
                        format!(
                            "{branch} is a copy of {head} from {owner}, at {}. It cannot be pushed back to the fork from here.",
                            crate::notice::short(&tip)
                        ),
                    );
                    match offer {
                        Some(action) => outcome.offer(FORK_OFFER, action),
                        None => outcome,
                    }
                })
                .map_err(|why| crate::notice::Failure::from(format!("Check Out Failed — {why}")))
        },
        |_, _| {},
    );
}

fn fork_offer(ui: &Ui, full_name: &str) -> Option<view::Action> {
    let url = format!("https://github.com/{full_name}.git");
    let taken = ui.refs.addresses.values().any(|addresses| {
        addresses.fetch.iter().any(|address| crate::address::web_address(address).is_some_and(|page| page.eq_ignore_ascii_case(&format!("https://github.com/{full_name}"))))
    });
    if taken {
        return None;
    }
    let (owner, _) = full_name.split_once('/')?;
    let mut name = owner.to_ascii_lowercase();
    if ui.refs.remotes.iter().any(|remote| *remote == name) {
        name = format!("{name}-fork");
    }
    if ui.refs.remotes.iter().any(|remote| *remote == name) || crate::remotes::validate_name(&name).is_err() {
        return None;
    }
    Some(view::Action::Pulls(Action::AddFork { name, url }))
}

pub(crate) fn add_fork_remote(repo: &Repository, name: &str, url: &str) -> Result<(), String> {
    crate::remotes::validate_name(name)?;
    crate::remotes::validate_url(url)?;
    if crate::remotes::addresses(repo)?.contains_key(name) {
        return Err(format!("A remote named {name} already exists. Nothing was changed."));
    }
    checked(repo.command().args(["remote", "add", "--", name, url]))?;
    match crate::remotes::addresses(repo)?.get(name).and_then(crate::remotes::Addresses::single) {
        Some(added) if added == url => Ok(()),
        _ => Err(format!("{name} was not added with the address {url}.")),
    }
}

fn add_fork(tab: &mut Tab, name: String, url: String, cx: &mut Context<Tab>) {
    if let Some(reason) = tab.ui.busy().then(|| "Wait for the current operation to finish".to_owned()) {
        tab.refuse(reason, cx);
        return;
    }
    let added = name.clone();
    tab.run_operation_then(
        cx,
        "Adding the fork as a remote…",
        "Remote added",
        false,
        move |repo| {
            add_fork_remote(&repo, &name, &url)?;
            Ok::<_, String>(Outcome::titled(format!("Added {name}"), format!("{name} is {url}. Fetching it now.")))
        },
        move |tab, cx| crate::sync::fetch_remote(tab, added, cx),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn pull(number: u64, head: &str, repo: &str, state: PullState) -> PullRequest {
        PullRequest {
            number,
            title: format!("Pull {number}"),
            author: "hiker".into(),
            state,
            head: head.into(),
            head_sha: "a".repeat(40),
            head_repo: Some(repo.into()),
            base: "trunk".into(),
            page: format!("https://github.com/cedar/trail/pull/{number}"),
        }
    }

    fn hosts(open: Vec<PullRequest>, closed: Vec<PullRequest>) -> BTreeMap<String, Hosted> {
        let host = Hosted {
            full_name: "cedar/trail".into(),
            open: Listing { etag: None, pulls: open },
            closed: Listing { etag: None, pulls: closed },
            trouble: None,
        };
        BTreeMap::from([("origin".to_owned(), host)])
    }

    fn commit(f: &Fixture, name: &str, text: &str, subject: &str) -> String {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", subject]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn snapshot(f: &Fixture) -> Vec<u8> {
        [
            f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            f.git(&["ls-files", "--stage"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            f.git(&["symbolic-ref", "HEAD"]),
        ]
        .concat()
    }

    fn configured(f: &Fixture, pattern: &str) -> bool {
        f.repo.read_command().args(["config", "--get-regexp", pattern]).run().unwrap().status.success()
    }

    fn hosted(f: &Fixture) -> (String, String, tempfile::TempDir) {
        let outside = tempfile::Builder::new().prefix("fastkraken-pulls-test").tempdir().unwrap();
        let bare = outside.path().join("origin.git");
        f.git(&["init", "-q", "--bare", bare.to_str().unwrap()]);
        let address = "https://github.com/cedar/trail.git".to_owned();
        f.git(&["remote", "add", "origin", &address]);
        f.git(&["config", &format!("url.{}.insteadOf", bare.display()), &address]);
        f.git(&["push", "-q", "origin", "main"]);
        let fork = commit(f, "fork.txt", "Fork\n", "Mark the fork trail");
        f.git(&["push", "-q", "origin", &format!("{fork}:refs/pull/15/head")]);
        f.git(&["reset", "-q", "--hard", "HEAD~1"]);
        (address, fork, outside)
    }

    #[test]
    fn a_branch_matches_the_pull_request_from_its_own_repository_and_open_ones_come_first() {
        let open = vec![pull(12, "ridge", "cedar/trail", PullState::Open), pull(15, "ridge", "hiker/trail", PullState::Draft)];
        let closed = vec![pull(9, "switchback", "cedar/trail", PullState::Merged), pull(8, "ridge", "cedar/trail", PullState::Closed)];
        let hosts = hosts(open, closed);
        assert_eq!(find_in(&hosts, "Cedar/Trail", "ridge", true).map(|(r, p)| (r, p.number)), Some(("origin", 12)));
        assert_eq!(find_in(&hosts, "hiker/trail", "ridge", true).map(|(_, p)| p.number), Some(15));
        assert_eq!(find_in(&hosts, "cedar/trail", "switchback", true).map(|(_, p)| p.state), Some(PullState::Merged));
        assert_eq!(find_in(&hosts, "cedar/trail", "switchback", false), None);
        assert_eq!(find_in(&hosts, "cedar/trail", "trunk", true), None);
        assert_eq!(numbered(&hosts, 15).map(|(_, p)| p.head.as_str()), Some("ridge"));
        assert_eq!(numbered(&hosts, 9), None);
        assert_eq!(label(&pull(4, "x", "a/b", PullState::Draft)), "#4 Draft");
        assert_eq!(label(&pull(4, "x", "a/b", PullState::Open)), "#4");
        assert_eq!(colour(PullState::Merged), TAG);
    }

    #[test]
    fn checks_add_up_to_running_passed_or_failed_with_the_failing_names() {
        let run = |name: &str, state| CheckRun { name: name.into(), state };
        assert_eq!(summary(&[]), None);
        assert_eq!(summary(&[run("build", RunState::Passed), run("docs", RunState::Skipped)]), Some((Overall::Passed, "All 2 checks passed".into())));
        assert_eq!(summary(&[run("build", RunState::Passed), run("lint", RunState::Running)]), Some((Overall::Running, "1 of 2 checks running".into())));
        assert_eq!(
            summary(&[run("build", RunState::Failed), run("lint", RunState::Running), run("test", RunState::Failed)]),
            Some((Overall::Failed, "2 of 3 checks failed: build, test".into()))
        );
    }

    #[test]
    fn a_pull_request_from_a_fork_is_fetched_into_its_own_branch_and_checked_out() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        let (address, fork, _outside) = hosted(&f);
        let main = String::from_utf8(f.git(&["rev-parse", "main"])).unwrap().trim().to_owned();
        let remotes_before = f.git(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/remotes"]);
        let request = ForkCheckout { remote: "origin".into(), address, number: 15, seen: None };
        assert_eq!(check_out_fork(&f.repo, &request).unwrap(), fork);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/pr/15\n");
        assert_eq!(f.git(&["rev-parse", "pr/15"]), format!("{fork}\n").as_bytes());
        assert_eq!(f.git(&["rev-parse", "main"]), format!("{main}\n").as_bytes());
        assert_eq!(f.git(&["status", "--porcelain=v1", "--untracked-files=all"]), b"");
        assert_eq!(fs::read_to_string(f.dir.join("fork.txt")).unwrap(), "Fork\n");
        assert!(f.git(&["ls-files", "--stage", "--", "fork.txt"]).starts_with(b"100644"));
        assert_eq!(f.git(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/remotes"]), remotes_before);
        assert!(!configured(&f, "^branch\\.pr/15\\."));
    }

    #[test]
    fn adding_the_fork_writes_one_remote_and_refuses_a_name_already_taken() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["remote", "add", "origin", "https://github.com/cedar/trail.git"]);
        f.git(&["config", "url./elsewhere/hiker.git.insteadOf", "https://github.com/hiker/trail.git"]);
        let before = snapshot(&f);
        add_fork_remote(&f.repo, "hiker", "https://github.com/hiker/trail.git").unwrap();
        assert_eq!(f.git(&["config", "--get-all", "remote.hiker.url"]), b"https://github.com/hiker/trail.git\n");
        assert_eq!(f.git(&["config", "--get-all", "remote.hiker.fetch"]), b"+refs/heads/*:refs/remotes/hiker/*\n");
        assert_eq!(snapshot(&f), before);
        let refused = add_fork_remote(&f.repo, "origin", "https://github.com/hiker/trail.git").unwrap_err();
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(f.git(&["config", "--get-all", "remote.origin.url"]), b"https://github.com/cedar/trail.git\n");
        assert!(add_fork_remote(&f.repo, "fern", "ext::sh -c touch").is_err());
        assert!(!configured(&f, "^remote\\.fern\\."));
    }

    #[test]
    fn a_fork_checkout_refuses_a_remote_or_branch_that_changed_since_it_was_shown() {
        let _apart = auth::tests::Apart::new();
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        let (address, _, _outside) = hosted(&f);
        let before = snapshot(&f);
        let moved = ForkCheckout { remote: "origin".into(), address: "https://github.com/cedar/other.git".into(), number: 15, seen: None };
        assert!(check_out_fork(&f.repo, &moved).unwrap_err().contains("points at https://github.com/cedar/trail.git now"));
        let gone = ForkCheckout { remote: "upstream".into(), address: address.clone(), number: 15, seen: None };
        assert!(check_out_fork(&f.repo, &gone).unwrap_err().contains("not a remote"));
        f.git(&["branch", "pr/15"]);
        let before_branch = snapshot(&f);
        let stale = ForkCheckout { remote: "origin".into(), address: address.clone(), number: 15, seen: None };
        assert!(check_out_fork(&f.repo, &stale).unwrap_err().contains("pr/15 changed since"));
        assert_eq!(snapshot(&f), before_branch);
        let tip = String::from_utf8(f.git(&["rev-parse", "pr/15"])).unwrap().trim().to_owned();
        f.git(&["switch", "-q", "pr/15"]);
        let checked_out = snapshot(&f);
        let current = ForkCheckout { remote: "origin".into(), address: address.clone(), number: 15, seen: Some(tip) };
        assert!(check_out_fork(&f.repo, &current).unwrap_err().contains("pr/15 is checked out"));
        assert_eq!(snapshot(&f), checked_out);
        f.git(&["switch", "-q", "main"]);
        f.git(&["branch", "-D", "pr/15"]);
        assert_eq!(snapshot(&f), before);
        assert!(f.git(&["for-each-ref", "refs/heads/pr"]).is_empty());
    }

    #[test]
    fn only_head_commits_already_in_the_repository_count_as_present() {
        let f = Fixture::new();
        let first = commit(&f, "trail.txt", "Trail\n", "Start the trail");
        let tree = String::from_utf8(f.git(&["rev-parse", "HEAD^{tree}"])).unwrap().trim().to_owned();
        let missing = "d".repeat(40);
        let asked = [first.clone(), tree, missing, "main".to_owned(), first.to_ascii_uppercase()];
        assert_eq!(present(&f.repo, &asked), BTreeSet::from([first]));
        assert!(present(&f.repo, &[]).is_empty());
    }
}
