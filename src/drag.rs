use crate::{
    Tab, kit,
    commands::Target,
    guard::Seen,
    merge::{Mode, Source},
    notice::{self, Notice, Outcome},
    prompt::{self, Answer, Spec},
    repository::{Repository, Revisions, checked, validate_oid},
    reverse::Operation,
    theme::*,
    view::{self, Desk, Menu, Selection, Tone, Ui},
};
use gpui::{
    BoxShadow, Context, Div, IntoElement, ParentElement, Pixels, Point, Render, Stateful, StatefulInteractiveElement,
    Styled, Window, div, point, prelude::*, px, rgb,
};
use std::{
    cell::Cell,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    rc::Rc,
};

#[derive(Clone)]
pub(crate) struct Carried {
    target: Target,
    also: Option<Target>,
    oid: Option<String>,
    chip: kit::Chip,
    name: String,
    inert: Rc<Cell<bool>>,
}

impl Carried {
    pub(crate) fn new(target: Target, also: Option<Target>, chip: kit::Chip, name: impl Into<String>) -> Self {
        Self { target, also, oid: None, chip, name: name.into(), inert: Rc::new(Cell::new(false)) }
    }

    pub(crate) fn at(mut self, oid: Option<String>) -> Self {
        self.oid = oid;
        self
    }

    fn ends(&self) -> impl Iterator<Item = &Target> {
        std::iter::once(&self.target).chain(self.also.as_ref())
    }
}

struct Preview {
    chip: kit::Chip,
    name: String,
    inert: bool,
    grabbed: Point<Pixels>,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let left = (self.grabbed.x - px(8.)).max(px(0.));
        let top = (self.grabbed.y - px(9.)).max(px(0.));
        div().pl(left).pt(top).when(!self.inert, |d| {
            d.child(kit::chip(self.chip, self.name.clone()).opacity(0.9).shadow(ring(LINE_STRONG)))
        })
    }
}

fn ring(colour: u32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: rgb(colour).into(), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.) }]
}

pub(crate) fn source<E: StatefulInteractiveElement>(element: E, carried: Carried) -> E {
    element.on_drag(carried, |carried, grabbed, window, cx| {
        let modifiers = window.modifiers();
        let inert = modifiers.platform || modifiers.shift;
        carried.inert.set(inert);
        let (chip, name) = (carried.chip, carried.name.clone());
        cx.new(|_| Preview { chip, name, inert, grabbed })
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Row,
    Chip,
}

pub(crate) fn target<T: Desk>(
    element: Stateful<Div>,
    onto: Target,
    also: Option<Target>,
    look: Look,
    cx: &Context<T>,
) -> Stateful<Div> {
    if !cx.has_active_drag() {
        return element;
    }
    let ends: Vec<Target> = std::iter::once(onto.clone()).chain(also.clone()).collect();
    let checked_ends = ends.clone();
    element
        .drag_over::<Carried>(move |style, carried, _, _| {
            if !accepts(carried, &ends) {
                return style;
            }
            match look {
                Look::Row => style.bg(soft(ACCENT, 0x2e)),
                Look::Chip => style.shadow(ring(ACCENT)),
            }
        })
        .can_drop(move |value, _, _| value.downcast_ref::<Carried>().is_some_and(|carried| accepts(carried, &checked_ends)))
        .on_drop(cx.listener(move |app, carried: &Carried, w, cx| {
            let action = Action::Drop {
                dragged: carried.target.clone(),
                dragged_also: carried.also.clone(),
                onto: onto.clone(),
                onto_also: also.clone(),
                at: w.mouse_position(),
            };
            app.dispatch(view::Action::Drag(action), w, cx);
        }))
}

fn accepts(carried: &Carried, onto: &[Target]) -> bool {
    let own_commit = |onto: &Target| matches!(onto, Target::Commit(oid) if carried.oid.as_ref() == Some(oid));
    !carried.inert.get()
        && carried.ends().any(|dragged| onto.iter().any(|onto| joins(dragged, onto) && !own_commit(onto)))
}

pub(crate) fn joins(dragged: &Target, onto: &Target) -> bool {
    if dragged == onto {
        return false;
    }
    matches!(
        (dragged, onto),
        (Target::Branch(_) | Target::RemoteBranch(_) | Target::Tag(_), Target::Branch(_))
            | (Target::Branch(_), Target::RemoteBranch(_) | Target::Tag(_) | Target::Commit(_) | Target::Remote(_))
    )
}

#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    dropped: Option<Dropped>,
}

#[derive(Clone, Debug)]
struct Dropped {
    dragged: Target,
    dragged_also: Option<Target>,
    onto: Target,
    onto_also: Option<Target>,
    ahead: Vec<(Target, Target)>,
}

impl Dropped {
    fn pairs(&self) -> Vec<(Target, Target)> {
        let dragged = std::iter::once(&self.dragged).chain(self.dragged_also.as_ref());
        dragged
            .flat_map(|dragged| {
                std::iter::once(&self.onto).chain(self.onto_also.as_ref()).map(move |onto| (dragged.clone(), onto.clone()))
            })
            .filter(|(dragged, onto)| joins(dragged, onto))
            .collect()
    }
}

pub(crate) fn pair(dragged: Target, onto: Target) -> Target {
    Target::Pair { dragged: Box::new(dragged), onto: Box::new(onto) }
}

pub(crate) fn alternatives(ui: &Ui, target: &Target) -> Vec<(String, Target)> {
    let (Some(dropped), Some((dragged, onto))) = (&ui.drag.dropped, ends(target)) else {
        return Vec::new();
    };
    if *dragged != dropped.dragged || *onto != dropped.onto {
        return Vec::new();
    }
    let mut found = Vec::new();
    if let Some(also) = &dropped.dragged_also {
        if joins(also, onto) {
            found.push((named(also), pair(also.clone(), onto.clone())));
        }
    }
    if let Some(also) = &dropped.onto_also {
        if joins(dragged, also) {
            found.push((named(also), pair(dragged.clone(), also.clone())));
        }
    }
    found
}

fn ends(target: &Target) -> Option<(&Target, &Target)> {
    match target {
        Target::Pair { dragged, onto } if joins(dragged, onto) => Some((dragged, onto)),
        _ => None,
    }
}

fn oid<'a>(ui: &'a Ui, target: &'a Target) -> Option<&'a str> {
    match target {
        Target::Commit(oid) => Some(oid),
        _ => ui.refs.oid_of(target),
    }
}

pub(crate) fn named(target: &Target) -> String {
    match target {
        Target::Branch(name) | Target::RemoteBranch(name) | Target::Tag(name) => name.to_string_lossy().into_owned(),
        Target::Remote(name) => name.clone(),
        Target::Commit(_) => "this commit".to_owned(),
        _ => String::new(),
    }
}

fn checked_out(ui: &Ui, name: &OsStr) -> bool {
    !ui.detached && ui.head.is_some() && ui.branch.as_bytes() == name.as_bytes()
}

fn local<'a>(ui: &Ui, target: &'a Target) -> Option<&'a OsString> {
    match target {
        Target::Branch(name) if ui.refs.local(name).is_some_and(|item| item.oid.is_some()) => Some(name),
        _ => None,
    }
}

fn merging<'a>(ui: &Ui, target: &'a Target) -> Option<(&'a OsString, &'a Target)> {
    let (dragged, onto) = ends(target)?;
    let (into, from) = match (dragged, onto) {
        (_, Target::Branch(_)) => (local(ui, onto)?, dragged),
        (Target::Branch(_), Target::RemoteBranch(_)) => (local(ui, dragged)?, onto),
        _ => return None,
    };
    let from_oid = oid(ui, from)?;
    (ui.refs.oid_of(&Target::Branch(into.clone())) != Some(from_oid)).then_some((into, from))
}

fn rebasing<'a>(ui: &Ui, target: &'a Target) -> Option<(&'a OsString, &'a Target)> {
    let (dragged, onto) = ends(target)?;
    let branch = local(ui, dragged)?;
    oid(ui, onto)?;
    matches!(onto, Target::Branch(_) | Target::RemoteBranch(_) | Target::Tag(_) | Target::Commit(_)).then_some((branch, onto))
}

fn forwarding<'a>(ui: &Ui, target: &'a Target) -> Option<(&'a OsString, &'a Target)> {
    let (dragged, onto) = ends(target)?;
    let into = local(ui, onto)?;
    let dropped = ui.drag.dropped.as_ref()?;
    dropped.ahead.iter().any(|(d, o)| d == dragged && o == onto).then_some((into, dragged))
}

fn resetting<'a>(ui: &Ui, target: &'a Target) -> Option<(&'a OsString, &'a Target)> {
    let (dragged, onto) = ends(target)?;
    let (branch, to) = match (dragged, onto) {
        (_, Target::Branch(_)) => (local(ui, onto)?, dragged),
        (Target::Branch(_), Target::Commit(_) | Target::Tag(_)) => (local(ui, dragged)?, onto),
        _ => return None,
    };
    let to_oid = oid(ui, to)?;
    (ui.refs.oid_of(&Target::Branch(branch.clone())) != Some(to_oid)).then_some((branch, to))
}

fn pushing<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a crate::refs::NamedRef, String, OsString, String)> {
    let (dragged, onto) = ends(target)?;
    let item = ui.refs.local(local(ui, dragged)?)?;
    match onto {
        Target::RemoteBranch(name) => {
            ui.refs.oid_of(onto)?;
            let (remote, there) = crate::branch::split_remote(&ui.refs.remotes, name)?;
            Some((item, remote.to_owned(), there, name.to_string_lossy().into_owned()))
        }
        Target::Remote(remote) if ui.refs.remotes.contains(remote) => {
            let upstream = item.upstream.as_deref().and_then(|upstream| crate::branch::split_remote(&ui.refs.remotes, OsStr::new(upstream)));
            let there = match upstream {
                Some((on, there)) if on == remote => there,
                _ => item.name.clone(),
            };
            Some((item, remote.clone(), there, remote.clone()))
        }
        _ => None,
    }
}

fn tracking<'a>(ui: &'a Ui, target: &'a Target) -> Option<(&'a crate::refs::NamedRef, &'a OsString)> {
    let (dragged, onto) = ends(target)?;
    let item = ui.refs.local(local(ui, dragged)?)?;
    let Target::RemoteBranch(remote) = onto else { return None };
    ui.refs.oid_of(onto)?;
    crate::branch::split_remote(&ui.refs.remotes, remote)?;
    Some((item, remote))
}

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Submenu, busy, networking, operating};
    fn writing(ui: &Ui) -> Availability {
        Availability::unless(busy(ui).or_else(|| operating(ui)))
    }
    fn attached(ui: &Ui) -> Availability {
        Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
            (ui.detached || ui.head.is_none()).then(|| "Check out a branch first".to_owned())
        }))
    }
    fn reset_when(ui: &Ui, target: &Target) -> Availability {
        match resetting(ui, target) {
            Some((branch, _)) if checked_out(ui, branch) => writing(ui),
            _ => Availability::Hidden,
        }
    }
    fn reset_run(ui: &Ui, target: &Target, operation: Operation) -> Option<view::Action> {
        let (_, to) = resetting(ui, target)?;
        Some(view::Action::Drag(Action::ResetHere { operation, oid: oid(ui, to)?.to_owned() }))
    }
    const RESET: Submenu = Submenu {
        id: "drop.reset",
        title: |ui, target| match resetting(ui, target) {
            Some((branch, to)) => format!("Reset {} to {}", branch.to_string_lossy(), named(to)),
            None => String::new(),
        },
        icon: "history",
    };
    &[
        Command::new(
            "drop.fast-forward",
            Group::Switch,
            |ui, target| match forwarding(ui, target) {
                Some((into, to)) => format!("Fast-forward {} to {}", into.to_string_lossy(), named(to)),
                None => String::new(),
            },
            |ui, target, _| match forwarding(ui, target) {
                Some(_) => writing(ui),
                None => Availability::Hidden,
            },
            |ui, target| {
                let (into, to) = forwarding(ui, target)?;
                let source = Source::of(to)?;
                if checked_out(ui, into) {
                    return Some(view::Action::Merge(crate::merge::Action::Start(source, Mode::FastForwardOnly)));
                }
                Some(view::Action::Sync(crate::sync::Action::Advance {
                    name: into.clone(),
                    tip: ui.refs.oid_of(&Target::Branch(into.clone()))?.to_owned(),
                    to: oid(ui, to)?.to_owned(),
                    source,
                }))
            },
        )
        .icon("arrow-down"),
        Command::new(
            "drop.merge",
            Group::Switch,
            |ui, target| match merging(ui, target) {
                Some((into, from)) if checked_out(ui, into) => format!("Merge {} into {}", named(from), into.to_string_lossy()),
                Some((into, from)) => format!("Switch to {} and merge {}", into.to_string_lossy(), named(from)),
                None => String::new(),
            },
            |ui, target, _| match merging(ui, target) {
                Some(_) => writing(ui),
                None => Availability::Hidden,
            },
            |ui, target| {
                let (into, from) = merging(ui, target)?;
                let source = Source::of(from)?;
                if checked_out(ui, into) {
                    return Some(view::Action::Merge(crate::merge::Action::Start(source, ui.merge.mode)));
                }
                let tip = ui.refs.oid_of(&Target::Branch(into.clone()))?.to_owned();
                Some(view::Action::Drag(Action::SwitchAndMerge { branch: into.clone(), tip, source }))
            },
        )
        .icon("merge"),
        Command::new(
            "drop.rebase",
            Group::Switch,
            |ui, target| match rebasing(ui, target) {
                Some((branch, onto)) if checked_out(ui, branch) => {
                    format!("Rebase {} onto {}…", branch.to_string_lossy(), named(onto))
                }
                Some((branch, onto)) => format!("Switch to {} and rebase it onto {}…", branch.to_string_lossy(), named(onto)),
                None => String::new(),
            },
            |ui, target, _| match rebasing(ui, target) {
                Some(_) => attached(ui),
                None => Availability::Hidden,
            },
            |ui, target| {
                let (branch, onto) = rebasing(ui, target)?;
                let onto = Source::of(onto)?;
                if checked_out(ui, branch) {
                    return Some(view::Action::Rebase(crate::rebase::Action::Onto(onto)));
                }
                Some(view::Action::Rebase(crate::rebase::Action::Pair { branch: branch.clone(), onto }))
            },
        )
        .icon("branch"),
        Command::new(
            "drop.reset-soft",
            Group::Reverse,
            |_, _| "Soft: keep all changes staged…".into(),
            |ui, target, _| reset_when(ui, target),
            |ui, target| reset_run(ui, target, Operation::SoftReset),
        )
        .under(&RESET)
        .icon("history"),
        Command::new(
            "drop.reset-mixed",
            Group::Reverse,
            |_, _| "Mixed: keep the files, clear the staging area…".into(),
            |ui, target, _| reset_when(ui, target),
            |ui, target| reset_run(ui, target, Operation::MixedReset),
        )
        .under(&RESET)
        .icon("history"),
        Command::new(
            "drop.reset-hard",
            Group::Reverse,
            |_, _| "Hard: discard all changes…".into(),
            |ui, target, _| reset_when(ui, target),
            |ui, target| reset_run(ui, target, Operation::HardReset),
        )
        .under(&RESET)
        .icon("history")
        .danger(),
        Command::new(
            "drop.move",
            Group::Reverse,
            |ui, target| match resetting(ui, target) {
                Some((branch, to)) => format!("Reset {} to {}…", branch.to_string_lossy(), named(to)),
                None => String::new(),
            },
            |ui, target, _| match resetting(ui, target) {
                Some((branch, _)) if !checked_out(ui, branch) => writing(ui),
                _ => Availability::Hidden,
            },
            |ui, target| {
                let (branch, to) = resetting(ui, target)?;
                Some(view::Action::Drag(Action::AskMove { branch: branch.clone(), to: to.clone() }))
            },
        )
        .icon("history")
        .danger(),
        Command::new(
            "drop.push",
            Group::Remotes,
            |ui, target| match pushing(ui, target) {
                Some((item, _, _, shown)) => format!("Push {} to {shown}", item.name.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| {
                let Some((item, ..)) = pushing(ui, target) else {
                    return Availability::Hidden;
                };
                let same = ends(target).is_some_and(|(_, onto)| {
                    matches!(onto, Target::RemoteBranch(_)) && ui.refs.oid_of(onto) == item.oid.as_deref()
                });
                Availability::unless(networking(ui).or_else(|| operating(ui)).or_else(|| {
                    same.then(|| format!("It is already at {}", item.name.to_string_lossy()))
                }))
            },
            |ui, target| {
                let (item, remote, destination, _) = pushing(ui, target)?;
                Some(view::Action::PushTo(crate::push_to::Action::Push {
                    branch: item.name.clone(),
                    tip: item.oid.clone()?,
                    remote,
                    destination,
                }))
            },
        )
        .icon("arrow-up"),
        Command::new(
            "drop.upstream",
            Group::Remotes,
            |ui, target| match tracking(ui, target) {
                Some((item, remote)) => format!("Set {} as upstream of {}", remote.to_string_lossy(), item.name.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| {
                let Some((item, remote)) = tracking(ui, target) else {
                    return Availability::Hidden;
                };
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    (item.upstream.as_deref().map(str::as_bytes) == Some(remote.as_bytes()) && !item.gone)
                        .then(|| format!("{} already tracks it", item.name.to_string_lossy()))
                }))
            },
            |ui, target| {
                let (item, remote) = tracking(ui, target)?;
                Some(view::Action::Tracking(crate::tracking::Action::Track {
                    name: item.name.clone(),
                    seen: item.upstream.clone(),
                    chosen: remote.to_string_lossy().into_owned(),
                }))
            },
        )
        .icon("cloud"),
    ]
};

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Drop { dragged: Target, dragged_also: Option<Target>, onto: Target, onto_also: Option<Target>, at: Point<Pixels> },
    SwitchAndMerge { branch: OsString, tip: String, source: Source },
    ResetHere { operation: Operation, oid: String },
    AskMove { branch: OsString, to: Target },
    Restore { branch: OsString, from: String, to: String },
}

#[derive(Debug)]
pub(crate) struct Ask {
    branch: OsString,
    from: String,
    to: String,
    label: String,
    leaving: usize,
    lost: usize,
    listed: Vec<(String, String)>,
}

const LISTED: usize = 5;

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    let branch = ask.branch.to_string_lossy();
    let mut spec = Spec::new(format!("Reset {branch} to {}?", ask.label), format!("Reset {branch}"))
        .line(format!(
            "{branch} moves from {} to {}. It is not checked out, so your files stay as they are.",
            notice::short(&ask.from),
            notice::short(&ask.to)
        ))
        .line(match ask.leaving {
            0 => format!("No commits leave {branch}."),
            1 => format!("1 commit leaves {branch}."),
            n => format!("{n} commits leave {branch}."),
        });
    if ask.lost > 0 {
        spec = spec.line(match ask.lost {
            1 if ask.leaving == 1 => "It is on no other branch or tag, so it will no longer appear in the graph.".to_owned(),
            n if n == ask.leaving => "They are on no other branch or tag, so they will no longer appear in the graph.".to_owned(),
            1 => "1 of them is on no other branch or tag, so it will no longer appear in the graph.".to_owned(),
            n => format!("{n} of them are on no other branch or tag, so they will no longer appear in the graph."),
        });
        for (short, subject) in &ask.listed {
            spec = spec.line(format!("{short}  {subject}"));
        }
        match ask.lost.saturating_sub(ask.listed.len()) {
            0 => {}
            1 => spec = spec.line("and 1 more commit"),
            n => spec = spec.line(format!("and {n} more commits")),
        }
        spec = spec.line("Restore, in the notice that follows, puts the branch back.").danger();
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, _: Answer, cx: &mut Context<Tab>) {
    let Ask { branch, from, to, label, .. } = ask;
    tab.run_operation_then(
        cx,
        "Resetting the branch…",
        "Branch reset",
        false,
        move |repo| move_branch(&repo, &branch, &from, &to, &label),
        |_, _| {},
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() || tab.ui.dialog_open() {
        return;
    }
    if tab.ui.busy() && !matches!(action, Action::Drop { .. }) {
        return;
    }
    match action {
        Action::Drop { dragged, dragged_also, onto, onto_also, at } => {
            view::focus(&tab.ui, window);
            dropped_on(tab, Dropped { dragged, dragged_also, onto, onto_also, ahead: Vec::new() }, at, cx);
        }
        Action::SwitchAndMerge { branch, tip, source } => {
            let Some(expected) = source.shown(&tab.ui) else { return };
            if tab.ui.refs.oid_of(&Target::Branch(branch.clone())) != Some(tip.as_str()) {
                return;
            }
            view::focus(&tab.ui, window);
            let mut head_ref = OsString::from("refs/heads/");
            head_ref.push(&branch);
            let seen = Seen { head_ref: Some(head_ref), head: Some(tip) };
            let stash = tab.ui.autostash.always;
            let mode = tab.ui.merge.mode;
            let into = branch.to_string_lossy().into_owned();
            crate::refs::switch_then(
                tab,
                branch,
                stash,
                move |tab, cx| crate::merge::start_on(tab, into, source, mode, expected, seen, stash, cx),
                cx,
            );
        }
        Action::ResetHere { operation, oid } => {
            if validate_oid(&oid).is_err() {
                return;
            }
            tab.select(Selection::Commit(oid), cx);
            crate::reverse::dispatch(tab, crate::reverse::Action::Start(operation), window, cx);
        }
        Action::AskMove { branch, to } => ask_move(tab, branch, to, cx),
        Action::Restore { branch, from, to } => {
            let label = notice::short(&to).to_owned();
            tab.run_operation_then(
                cx,
                "Restoring the branch…",
                "Branch restored",
                false,
                move |repo| {
                    move_ref(&repo, &branch, &from, &to, &format!("restore: moving back to {label}"))?;
                    let shown = branch.to_string_lossy();
                    Ok::<_, String>(Outcome::titled(format!("Restored {shown}"), format!("{shown} is back at {label}.")))
                },
                |_, _| {},
            );
        }
    }
    cx.notify();
}

fn dropped_on(tab: &mut Tab, mut dropped: Dropped, at: Point<Pixels>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let candidates: Vec<(Target, Target, String, String)> = dropped
        .pairs()
        .into_iter()
        .filter(|(_, onto)| matches!(onto, Target::Branch(_)))
        .filter_map(|(dragged, onto)| {
            let from = oid(&tab.ui, &dragged)?.to_owned();
            let to = oid(&tab.ui, &onto)?.to_owned();
            (from != to).then_some((dragged, onto, from, to))
        })
        .collect();
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || {
            candidates
                .into_iter()
                .filter(|(_, _, from, to)| repo.is_ancestor(to, from).unwrap_or(false))
                .map(|(dragged, onto, _, _)| (dragged, onto))
                .collect::<Vec<_>>()
        },
        move |tab, ahead, cx| {
            if tab.epoch != epoch || tab.ui.dialog_open() || tab.ui.menu.is_some() {
                return;
            }
            dropped.ahead = ahead;
            let target = pair(dropped.dragged.clone(), dropped.onto.clone());
            let (dragged, onto) = (named(&dropped.dragged), named(&dropped.onto));
            tab.ui.drag.dropped = Some(dropped);
            let menu = Menu::Object { target, at };
            if crate::commands::entries_of(&tab.ui, &menu, cx).is_empty() {
                tab.ui.notices.show(Notice::new(Tone::Info, format!("Nothing joins {dragged} and {onto}"), ""));
            } else {
                tab.ui.menu = Some(menu);
                tab.ui.menu_cursor = Default::default();
            }
            cx.notify();
        },
    );
}

fn ask_move(tab: &mut Tab, branch: OsString, to: Target, cx: &mut Context<Tab>) {
    let (Some(from), Some(to_oid)) = (tab.ui.refs.oid_of(&Target::Branch(branch.clone())), oid(&tab.ui, &to)) else {
        return;
    };
    if checked_out(&tab.ui, &branch) {
        return;
    }
    let (from, to_oid, label) = (from.to_owned(), to_oid.to_owned(), named(&to));
    let label = if matches!(to, Target::Commit(_)) { notice::short(&to_oid).to_owned() } else { label };
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (counted_from, counted_to, counted_branch) = (from.clone(), to_oid.clone(), branch.clone());
    Tab::work(
        cx,
        move || counted(&repo, &counted_branch, &counted_from, &counted_to),
        move |tab, counted, cx| {
            let still = tab.ui.refs.oid_of(&Target::Branch(branch.clone())) == Some(from.as_str());
            if tab.epoch != epoch || !still || tab.ui.busy() || tab.ui.dialog_open() {
                return;
            }
            match counted {
                Ok((leaving, lost, listed)) => {
                    let ask = Ask { branch, from, to: to_oid, label, leaving, lost, listed };
                    prompt::ask(&mut tab.ui, prompt::Kind::Drag(ask), cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

fn counted(repo: &Repository, branch: &OsStr, from: &str, to: &str) -> Result<(usize, usize, Vec<(String, String)>), String> {
    validate_oid(from)?;
    validate_oid(to)?;
    let leaving = repo.count(&Revisions::reachable(from).not(to))?;
    let only = Revisions::reachable(from).not(to).only_on(branch);
    let lost = repo.count(&only)?;
    let listed = repo
        .subjects(&only, LISTED.min(lost))?
        .into_iter()
        .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
        .collect();
    Ok((leaving, lost, listed))
}

fn plain(name: &OsStr) -> Result<(), String> {
    if name.is_empty() || name.as_bytes()[0] == b'-' {
        return Err("Select a branch by its name.".into());
    }
    Ok(())
}

pub(crate) fn move_ref(repo: &Repository, branch: &OsStr, from: &str, to: &str, reason: &str) -> Result<(), String> {
    plain(branch)?;
    validate_oid(from)?;
    validate_oid(to)?;
    let shown = branch.to_string_lossy();
    let mut full = OsString::from("refs/heads/");
    full.push(branch);
    let listed = checked(
        repo.read_command()
            .args(["for-each-ref", "--format=%(refname)%00%(objectname)%00%(worktreepath)", "--"])
            .arg(&full),
    )?;
    let found = listed.split(|&b| b == b'\n').find_map(|line| {
        let mut fields = line.split(|&b| b == 0);
        (fields.next()? == full.as_bytes()).then(|| (fields.next().unwrap_or_default().to_vec(), fields.next().unwrap_or_default().to_vec()))
    });
    let Some((at, worktree)) = found else {
        return Err(format!("{shown} no longer exists. Reopen the action."));
    };
    if at != from.as_bytes() {
        return Err(format!("{shown} changed since you chose it. Reopen the action."));
    }
    if !worktree.is_empty() {
        return Err(format!(
            "{shown} is checked out in {}, so it cannot be moved from here.",
            String::from_utf8_lossy(&worktree)
        ));
    }
    if rewritten_elsewhere(repo, &full, branch) {
        return Err(format!("{shown} is being rebased or bisected, so it cannot be moved from here."));
    }
    let mut commit = OsString::from(to);
    commit.push("^{commit}");
    if repo.resolve(&commit).ok().as_deref() != Some(to) {
        return Err("That commit is not in the repository. Refresh and try again.".into());
    }
    checked(repo.command().args(["update-ref", "-m", reason]).arg(&full).args([to, from])).map(|_| ())
}

fn rewritten_elsewhere(repo: &Repository, full: &OsStr, branch: &OsStr) -> bool {
    let common = repo.common_dir();
    let mut git_dirs = vec![common.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        git_dirs.extend(entries.flatten().map(|entry| entry.path()));
    }
    let names = |dir: &std::path::Path, file: &str, wanted: &OsStr| {
        std::fs::read(dir.join(file)).is_ok_and(|text| text.trim_ascii_end() == wanted.as_bytes())
    };
    git_dirs.iter().any(|dir| {
        names(dir, "rebase-merge/head-name", full) || names(dir, "rebase-apply/head-name", full) || names(dir, "BISECT_START", branch)
    })
}

pub(crate) fn move_branch(repo: &Repository, branch: &OsStr, from: &str, to: &str, label: &str) -> Result<Outcome, String> {
    let leaving = repo.count(&Revisions::reachable(from).not(to))?;
    move_ref(repo, branch, from, to, &format!("reset: moving to {label}"))?;
    let shown = branch.to_string_lossy().into_owned();
    let detail = match leaving {
        0 => format!("It moved from {} to {}.", notice::short(from), notice::short(to)),
        n => format!("{} left {shown}. It was at {}.", notice::commits(n), notice::short(from)),
    };
    let restore = Action::Restore { branch: branch.to_owned(), from: to.to_owned(), to: from.to_owned() };
    let outcome = Outcome::titled(format!("Reset {shown} to {label}"), detail).offer("Restore", view::Action::Drag(restore));
    Ok(if leaving > 0 { outcome.sticky() } else { outcome })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{Run, tests::Fixture};
    use std::fs;

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().to_owned()
    }

    fn commit(f: &Fixture, file: &str, text: &str, subject: &str) {
        fs::write(f.dir.join(file), text).unwrap();
        f.git(&["add", "--", file]);
        f.git(&["commit", "-qm", subject]);
    }

    fn camp() -> (Fixture, String, String) {
        let f = Fixture::new();
        commit(&f, "camp.txt", "Base\n", "Pitch the camp");
        let base = rev(&f, "main");
        f.git(&["switch", "-qc", "ridge"]);
        commit(&f, "ridge.txt", "Climb\n", "Climb the ridge");
        commit(&f, "ridge.txt", "Summit\n", "Reach the summit");
        f.git(&["switch", "-q", "main"]);
        let ridge = rev(&f, "ridge");
        (f, base, ridge)
    }

    #[test]
    fn moving_a_branch_that_is_not_checked_out_changes_only_that_ref_and_restore_puts_it_back() {
        let (f, base, ridge) = camp();
        fs::write(f.dir.join("camp.txt"), "Edited\n").unwrap();
        let index = f.git(&["ls-files", "--stage"]);
        let (leaving, lost, listed) = counted(&f.repo, OsStr::new("ridge"), &ridge, &base).unwrap();
        assert_eq!((leaving, lost), (2, 2));
        let subjects: Vec<&str> = listed.iter().map(|(_, subject)| subject.as_str()).collect();
        assert_eq!(subjects, ["Reach the summit", "Climb the ridge"]);
        let outcome = move_branch(&f.repo, OsStr::new("ridge"), &ridge, &base, "main").unwrap();
        assert_eq!(outcome.title, "Reset ridge to main");
        assert_eq!(outcome.detail, format!("2 commits left ridge. It was at {}.", notice::short(&ridge)));
        assert_eq!(rev(&f, "ridge"), base);
        assert_eq!(rev(&f, "main"), base);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(fs::read_to_string(f.dir.join("camp.txt")).unwrap(), "Edited\n");
        let reflog = String::from_utf8(f.git(&["reflog", "-1", "--format=%gs", "ridge"])).unwrap();
        assert_eq!(reflog.trim(), "reset: moving to main");
        move_ref(&f.repo, OsStr::new("ridge"), &base, &ridge, "restore: moving back").unwrap();
        assert_eq!(rev(&f, "ridge"), ridge);
    }

    #[test]
    fn moving_a_branch_refuses_a_moved_tip_a_checked_out_branch_a_missing_commit_and_odd_names() {
        let (f, base, ridge) = camp();
        let err = move_ref(&f.repo, OsStr::new("ridge"), &base, &base, "x").unwrap_err();
        assert_eq!(err, "ridge changed since you chose it. Reopen the action.");
        assert_eq!(rev(&f, "ridge"), ridge);
        let err = move_ref(&f.repo, OsStr::new("main"), &base, &ridge, "x").unwrap_err();
        assert!(err.starts_with("main is checked out in "), "{err}");
        assert_eq!(rev(&f, "main"), base);
        let missing = "0123456789abcdef0123456789abcdef01234567";
        let err = move_ref(&f.repo, OsStr::new("ridge"), &ridge, missing, "x").unwrap_err();
        assert_eq!(err, "That commit is not in the repository. Refresh and try again.");
        assert!(move_ref(&f.repo, OsStr::new("-D"), &ridge, &base, "x").is_err());
        assert!(move_ref(&f.repo, OsStr::new("gone"), &ridge, &base, "x").unwrap_err().contains("no longer exists"));
        let worktree = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", worktree.to_str().unwrap(), "ridge"]);
        let err = move_ref(&f.repo, OsStr::new("ridge"), &ridge, &base, "x").unwrap_err();
        assert!(err.contains("is checked out in") && err.contains("ridge-tree"), "{err}");
        assert_eq!(rev(&f, "ridge"), ridge);
    }

    #[test]
    fn moving_a_branch_refuses_one_that_a_worktree_is_rebasing_or_bisecting() {
        let (f, base, ridge) = camp();
        commit(&f, "camp.txt", "Moved\n", "Move the camp");
        let worktree = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", worktree.to_str().unwrap(), "ridge"]);
        let stopped = f.repo.command().args(["-C", worktree.to_str().unwrap(), "rebase", "-q", "-x", "false", "main"]).run().unwrap();
        assert!(!stopped.status.success());
        let err = move_ref(&f.repo, OsStr::new("ridge"), &ridge, &base, "x").unwrap_err();
        assert_eq!(err, "ridge is being rebased or bisected, so it cannot be moved from here.");
        assert_eq!(rev(&f, "refs/heads/ridge"), ridge);
        f.git(&["-C", worktree.to_str().unwrap(), "rebase", "--abort"]);
        f.git(&["-C", worktree.to_str().unwrap(), "bisect", "start"]);
        f.git(&["-C", worktree.to_str().unwrap(), "bisect", "bad"]);
        f.git(&["-C", worktree.to_str().unwrap(), "bisect", "good", &base]);
        let err = move_ref(&f.repo, OsStr::new("ridge"), &ridge, &base, "x").unwrap_err();
        assert_eq!(err, "ridge is being rebased or bisected, so it cannot be moved from here.");
        assert_eq!(rev(&f, "refs/heads/ridge"), ridge);
        f.git(&["-C", worktree.to_str().unwrap(), "bisect", "reset"]);
        f.git(&["worktree", "remove", "--force", worktree.to_str().unwrap()]);
        move_ref(&f.repo, OsStr::new("ridge"), &ridge, &base, "x").unwrap();
        assert_eq!(rev(&f, "refs/heads/ridge"), base);
    }

    #[test]
    fn commits_kept_elsewhere_are_not_counted_as_lost() {
        let (f, base, ridge) = camp();
        f.git(&["tag", "summit", &ridge]);
        let (leaving, lost, listed) = counted(&f.repo, OsStr::new("ridge"), &ridge, &base).unwrap();
        assert_eq!((leaving, lost, listed.len()), (2, 0, 0));
    }

    #[test]
    fn a_ref_joins_only_the_ends_the_drop_menu_has_entries_for() {
        let branch = || Target::Branch("main".into());
        let other = || Target::Branch("ridge".into());
        let remote = || Target::RemoteBranch("origin/main".into());
        let tag = || Target::Tag("v1".into());
        let commit = || Target::Commit("a".repeat(40));
        assert!(joins(&other(), &branch()));
        assert!(!joins(&branch(), &branch()));
        assert!(joins(&remote(), &branch()));
        assert!(joins(&tag(), &branch()));
        assert!(joins(&branch(), &remote()));
        assert!(joins(&branch(), &commit()));
        assert!(joins(&branch(), &tag()));
        assert!(joins(&branch(), &Target::Remote("origin".into())));
        assert!(!joins(&remote(), &commit()));
        assert!(!joins(&tag(), &remote()));
        assert!(!joins(&remote(), &Target::Remote("origin".into())));
        let carried = Carried::new(branch(), Some(remote()), kit::Chip::Local, "main");
        assert!(accepts(&carried, &[branch()]));
        let commit_of = |oid: &str| Target::Commit(oid.to_owned());
        let carried = carried.at(Some("b".repeat(40)));
        assert!(!accepts(&carried, &[commit_of(&"b".repeat(40))]));
        assert!(accepts(&carried, &[commit_of(&"c".repeat(40))]));
        carried.inert.set(true);
        assert!(!accepts(&carried, &[other()]));
        let dropped = Dropped { dragged: branch(), dragged_also: Some(remote()), onto: other(), onto_also: None, ahead: Vec::new() };
        assert_eq!(dropped.pairs(), [(branch(), other()), (remote(), other())]);
    }
}
