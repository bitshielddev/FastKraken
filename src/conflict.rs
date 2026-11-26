mod backend;

use crate::{
    Tab,
    commands::{self, Target},
    kit,
    merge::{self, Side},
    notice::{Notice, Outcome},
    operation::{Conflict, Id, Shape, Snapshot, Version},
    prompt,
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use backend::named;
pub(crate) use backend::{Shown, Tooled, merge_in_tool};
use gpui::{Context, Div, FontWeight, MouseButton, MouseDownEvent, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::path::{Path, PathBuf};

pub(crate) const COMMANDS: &[commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy};
    fn conflict<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a Snapshot, &'a Conflict)> {
        let Target::Conflict(path) = target else { return None };
        let snapshot = ui.operation.snapshot().filter(|_| ui.operation.active())?;
        Some((snapshot, snapshot.conflict(path)?))
    }
    fn when(ui: &Ui, target: &Target, applies: impl FnOnce(&Snapshot, &Conflict) -> bool) -> Availability {
        match conflict(ui, target) {
            Some((snapshot, conflict)) if applies(snapshot, conflict) => Availability::unless(busy(ui)),
            _ => Availability::Hidden,
        }
    }
    fn path(target: &Target) -> Option<PathBuf> {
        match target {
            Target::Conflict(path) => Some(path.clone()),
            _ => None,
        }
    }
    fn take(side: Side) -> impl Fn(&Snapshot, &Conflict) -> bool {
        move |_, conflict| !conflict.shape.deleted() && conflict.side(side).is_some()
    }
    &[
        Command::new(
            "conflict.open",
            Group::View,
            |ui, target| match conflict(ui, target) {
                Some((_, conflict)) if !conflict.shape.fits_resolver() => "Choose a version".into(),
                _ => "Open in resolver".into(),
            },
            |ui, target, _| when(ui, target, |_, _| true),
            |_, target| path(target).map(|path| view::Action::Merge(merge::Action::Open(path))),
        )
        .icon("merge"),
        Command::new(
            "conflict.take-ours",
            Group::Changes,
            |ui, target| conflict(ui, target).map_or_else(String::new, |(s, _)| format!("Take {}", s.ours_name())),
            |ui, target, _| when(ui, target, take(Side::Current)),
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Take(path, Side::Current))),
        )
        .words(&["take current", "use current", "ours", "whole file"])
        .icon("check"),
        Command::new(
            "conflict.take-theirs",
            Group::Changes,
            |ui, target| conflict(ui, target).map_or_else(String::new, |(s, _)| format!("Take {}", s.theirs_name())),
            |ui, target, _| when(ui, target, take(Side::Incoming)),
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Take(path, Side::Incoming))),
        )
        .words(&["take incoming", "use incoming", "theirs", "whole file"])
        .icon("check"),
        Command::new(
            "conflict.keep",
            Group::Changes,
            |_, _| "Keep the changed file".into(),
            |ui, target, _| {
                when(ui, target, |_, c| {
                    c.shape.deleted() && (c.side(Side::Current).is_some() || c.side(Side::Incoming).is_some())
                })
            },
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Keep(path))),
        )
        .words(&["modified", "deleted", "keep modified version"])
        .icon("check"),
        Command::new(
            "conflict.delete",
            Group::Changes,
            |_, _| "Delete the file".into(),
            |ui, target, _| {
                when(ui, target, |_, c| c.side(Side::Current).is_none() || c.side(Side::Incoming).is_none())
            },
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Delete(path))),
        )
        .words(&["git rm", "remove", "deleted"])
        .icon("minus"),
        Command::new(
            "conflict.mark-resolved",
            Group::Changes,
            |_, _| "Mark resolved".into(),
            |ui, target, _| {
                when(ui, target, |_, c| !c.shape.deleted() && c.shape != Shape::Submodule)
            },
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Resolve(path))),
        )
        .words(&["git add", "resolved", "edited elsewhere"])
        .icon("ok"),
        Command::new(
            "conflict.mark-all-resolved",
            Group::Changes,
            |_, _| "Mark all resolved".into(),
            |ui, target, _| {
                let Some(snapshot) = ui.operation.snapshot().filter(|_| ui.operation.active() && *target == Target::Repo) else {
                    return Availability::Hidden;
                };
                if snapshot.conflicts.is_empty() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| {
                    markable(snapshot).is_empty().then(|| "Take a side for each file instead".to_owned())
                }))
            },
            |_, _| Some(view::Action::Conflict(Action::ResolveAll)),
        )
        .words(&["resolve all", "mark resolved", "git add", "conflicts"])
        .icon("ok"),
        Command::new(
            "conflict.unresolve",
            Group::Reverse,
            |_, _| "Mark as conflicted again…".into(),
            |ui, target, _| {
                let resolved = match target {
                    Target::Conflict(path) => ui.operation.active() && ui.operation.resolved().contains(path),
                    _ => false,
                };
                if resolved { Availability::unless(busy(ui)) } else { Availability::Hidden }
            },
            |_, target| path(target).map(|path| view::Action::Conflict(Action::Unresolve(path))),
        )
        .words(&["mark conflicted", "unresolve", "redo", "checkout -m"])
        .icon("undo")
        .danger(),
    ]
};

const ROW: &[&str] = &[
    "conflict.take-ours",
    "conflict.take-theirs",
    "conflict.keep",
    "conflict.delete",
    "conflict.mark-resolved",
];

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Take(PathBuf, Side),
    Keep(PathBuf),
    Delete(PathBuf),
    Resolve(PathBuf),
    ResolveAll,
    Unresolve(PathBuf),
    Close,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Markers(Shown),
    MarkersInAll { shown: Vec<Shown>, marked: Vec<PathBuf> },
    Unresolve { id: Id, path: PathBuf },
}

fn markable(snapshot: &Snapshot) -> Vec<&Conflict> {
    snapshot.conflicts.iter().filter(|c| !c.shape.deleted() && c.shape != Shape::Submodule).collect()
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

fn mark_all(tab: &mut Tab, shown: Vec<Shown>, check: bool, left: usize, cx: &mut Context<Tab>) {
    let title = format!("Marked {} resolved", files(shown.len()));
    let detail = match left {
        0 => String::new(),
        1 => "1 file is still in conflict: take a side for it.".to_owned(),
        n => format!("{n} files are still in conflict: take a side for each."),
    };
    resolve(tab, cx, "Marking the files resolved…", title, detail, move |repo| {
        backend::mark_all_resolved(repo, &shown, check)
    });
}

#[derive(Default)]
pub(crate) struct State {
    open: Option<PathBuf>,
    note: Option<String>,
    ticket: u64,
}

impl State {
    pub(crate) fn showing(&self) -> Option<&Path> {
        self.open.as_deref()
    }

    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub(crate) fn close(&mut self) {
        self.open = None;
        self.note = None;
    }

    pub(crate) fn follow(&mut self, snapshot: Option<&Snapshot>) {
        if self.open.as_deref().is_some_and(|path| !snapshot.is_some_and(|s| s.has_conflict(path))) {
            self.close();
        }
    }
}

pub(crate) fn show(ui: &mut Ui, path: PathBuf, note: Option<String>) {
    ui.conflict.open = Some(path);
    ui.conflict.note = note;
}

pub(crate) fn open_path(ui: &Ui) -> Option<&Path> {
    match view::centre(ui) {
        view::Centre::Resolver => ui.merge.editing(),
        view::Centre::Chooser => ui.conflict.showing(),
        _ => None,
    }
}

fn refuse(tab: &mut Tab, reason: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
    cx.notify();
}

fn resolve(
    tab: &mut Tab,
    cx: &mut Context<Tab>,
    progress: &'static str,
    title: String,
    detail: String,
    job: impl FnOnce(&crate::repository::Repository) -> Result<(), String> + Send + 'static,
) {
    tab.run_operation_then(
        cx,
        progress,
        "Resolved",
        false,
        move |repo| job(&repo).map(|()| Outcome::titled(title, detail)),
        |_, _| {},
    );
}

fn mark(tab: &mut Tab, shown: Shown, check: bool, cx: &mut Context<Tab>) {
    let title = format!("Marked {} resolved", named(&shown.path));
    resolve(tab, cx, "Marking the file resolved…", title, String::new(), move |repo| {
        backend::mark_resolved(repo, &shown, check)
    });
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() {
        return;
    }
    if let Action::Close = action {
        tab.ui.conflict.close();
        cx.notify();
        return;
    }
    let Some(snapshot) = tab.ui.operation.snapshot().filter(|_| tab.ui.operation.active()).cloned() else {
        return refuse(tab, "Nothing is in progress".to_owned(), cx);
    };
    let conflicted = |path: &Path| snapshot.conflict(path).ok_or_else(|| format!("{} is not in conflict", named(path)));
    match action {
        Action::Take(path, side) => {
            let conflict = match conflicted(&path) {
                Ok(conflict) => conflict,
                Err(reason) => return refuse(tab, reason, cx),
            };
            let label = match side {
                Side::Current => snapshot.ours(),
                Side::Incoming => snapshot.theirs(),
            };
            if conflict.side(side).is_none() {
                return refuse(tab, format!("{label} deleted {}. Delete the file instead", named(&path)), cx);
            }
            let shown = Shown::of(&snapshot, conflict);
            let title = format!("Resolved {}", named(&path));
            resolve(tab, cx, "Taking one side…", title, format!("Took the version from {label}."), move |repo| {
                backend::take(repo, &shown, side)
            });
        }
        Action::Keep(path) => {
            let conflict = match conflicted(&path) {
                Ok(conflict) => conflict,
                Err(reason) => return refuse(tab, reason, cx),
            };
            let shown = Shown::of(&snapshot, conflict);
            let title = format!("Resolved {}", named(&path));
            resolve(tab, cx, "Keeping the changed file…", title, "Kept the changed file.".to_owned(), move |repo| {
                backend::keep(repo, &shown)
            });
        }
        Action::Delete(path) => {
            let conflict = match conflicted(&path) {
                Ok(conflict) => conflict,
                Err(reason) => return refuse(tab, reason, cx),
            };
            let shown = Shown::of(&snapshot, conflict);
            let title = format!("Resolved {}", named(&path));
            resolve(tab, cx, "Deleting the file…", title, "Deleted the file.".to_owned(), move |repo| {
                backend::delete(repo, &shown)
            });
        }
        Action::Resolve(path) => {
            let conflict = match conflicted(&path) {
                Ok(conflict) => conflict,
                Err(reason) => return refuse(tab, reason, cx),
            };
            let shown = Shown::of(&snapshot, conflict);
            if !conflict.shape.may_hold_markers() {
                return mark(tab, shown, false, cx);
            }
            let Some(repo) = tab.repo.clone() else { return };
            tab.ui.conflict.ticket += 1;
            let (ticket, epoch) = (tab.ui.conflict.ticket, tab.epoch);
            Tab::work(
                cx,
                move || backend::has_markers(&repo, &path),
                move |tab, found, cx| {
                    if tab.epoch != epoch || tab.ui.conflict.ticket != ticket || tab.ui.busy() || tab.ui.dialog_open() {
                        return;
                    }
                    match found {
                        Ok(true) => prompt::ask(&mut tab.ui, prompt::Kind::Conflict(Ask::Markers(shown)), cx),
                        Ok(false) => mark(tab, shown, true, cx),
                        Err(err) => tab.error(err, cx),
                    }
                    cx.notify();
                },
            );
        }
        Action::ResolveAll => {
            let chosen = markable(&snapshot);
            if chosen.is_empty() {
                return refuse(tab, "No conflicted file can be marked resolved as it is".to_owned(), cx);
            }
            let left = snapshot.conflicts.len() - chosen.len();
            let shown: Vec<Shown> = chosen.iter().map(|conflict| Shown::of(&snapshot, conflict)).collect();
            let scanned: Vec<PathBuf> =
                chosen.iter().filter(|c| c.shape.may_hold_markers()).map(|c| c.path.clone()).collect();
            let Some(repo) = tab.repo.clone() else { return };
            tab.ui.conflict.ticket += 1;
            let (ticket, epoch) = (tab.ui.conflict.ticket, tab.epoch);
            Tab::work(
                cx,
                move || -> Result<Vec<PathBuf>, String> {
                    let mut marked = Vec::new();
                    for path in scanned {
                        if backend::has_markers(&repo, &path)? {
                            marked.push(path);
                        }
                    }
                    Ok(marked)
                },
                move |tab, found, cx| {
                    if tab.epoch != epoch || tab.ui.conflict.ticket != ticket || tab.ui.busy() || tab.ui.dialog_open() {
                        return;
                    }
                    match found {
                        Ok(marked) if marked.is_empty() => mark_all(tab, shown, true, left, cx),
                        Ok(marked) => {
                            let ask = Ask::MarkersInAll { shown, marked };
                            prompt::ask(&mut tab.ui, prompt::Kind::Conflict(ask), cx)
                        }
                        Err(err) => tab.error(err, cx),
                    }
                    cx.notify();
                },
            );
        }
        Action::Unresolve(path) => {
            if !snapshot.resolved.contains(&path) {
                return refuse(tab, format!("{} is not resolved", named(&path)), cx);
            }
            let ask = Ask::Unresolve { id: snapshot.id.clone(), path };
            prompt::ask(&mut tab.ui, prompt::Kind::Conflict(ask), cx);
        }
        Action::Close => {}
    }
    cx.notify();
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> prompt::Spec {
    match ask {
        Ask::Markers(shown) => {
            let name = named(&shown.path);
            prompt::Spec::new(format!("Mark {name} resolved anyway?"), "Mark resolved")
                .line("This file still contains conflict markers.")
                .line("Lines that start with <<<<<<<, ======= or >>>>>>> will be staged as they are.")
        }
        Ask::MarkersInAll { shown, marked } => {
            let mut spec = prompt::Spec::new(format!("Mark {} resolved anyway?", files(shown.len())), "Mark all resolved");
            spec = spec.line(match marked.len() {
                1 => "This file still contains conflict markers:".to_owned(),
                n => format!("These {n} files still contain conflict markers:"),
            });
            for path in marked.iter().take(crate::picked::NAMED) {
                spec = spec.line(named(path));
            }
            if marked.len() > crate::picked::NAMED {
                spec = spec.line(format!("and {} more", marked.len() - crate::picked::NAMED));
            }
            spec.line("Lines that start with <<<<<<<, ======= or >>>>>>> will be staged as they are.")
        }
        Ask::Unresolve { path, .. } => {
            let name = named(path);
            prompt::Spec::new(format!("Mark {name} as conflicted again?"), "Mark conflicted")
                .line(format!("Your resolution of {name} is discarded."))
                .line("The file goes back to how Git left it, in conflict, so you can resolve it again.")
                .danger()
        }
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, _: prompt::Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Markers(shown) => mark(tab, shown, false, cx),
        Ask::MarkersInAll { shown, .. } => {
            let left = tab.ui.operation.snapshot().map_or(0, |s| s.conflicts.len().saturating_sub(shown.len()));
            mark_all(tab, shown, false, left, cx)
        }
        Ask::Unresolve { id, path } => {
            let title = format!("{} is in conflict again", named(&path));
            tab.run_operation_then(
                cx,
                "Marking the file conflicted…",
                "In conflict again",
                false,
                move |repo| backend::unresolve(&repo, &id, &path).map(|()| Outcome::titled(title, String::new())),
                |_, _| {},
            );
        }
    }
}

fn file_name(path: &Path) -> (String, String) {
    (
        path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned(),
        path.parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default(),
    )
}

fn kind_word(shape: Shape) -> Option<&'static str> {
    Some(match shape {
        Shape::Text | Shape::BothAdded => return None,
        Shape::Binary => "binary",
        Shape::TooLarge => "over 1 MiB",
        Shape::DeletedByUs | Shape::DeletedByThem => "deleted on one side",
        Shape::Symlink => "symlink",
        Shape::Submodule => "submodule",
    })
}

pub(crate) fn size(bytes: u64) -> String {
    match bytes {
        0 => "Empty".to_owned(),
        1 => "1 byte".to_owned(),
        n if n < 1000 => format!("{n} bytes"),
        n if n < 1_000_000 => format!("{:.1} KB", n as f64 / 1000.),
        n if n < 1_000_000_000 => format!("{:.1} MB", n as f64 / 1_000_000.),
        n => format!("{:.1} GB", n as f64 / 1_000_000_000.),
    }
}

fn holds(version: Option<&Version>) -> String {
    let Some(version) = version else {
        return "Deleted".to_owned();
    };
    match version.mode.as_str() {
        "160000" => format!("Submodule at commit {}", crate::notice::short(&version.oid)),
        "120000" => "Symbolic link".to_owned(),
        _ => version.size.map_or_else(|| "A file".to_owned(), size),
    }
}

fn tip_for(id: &str, snapshot: &Snapshot) -> Option<String> {
    Some(match id {
        "conflict.take-ours" => format!("Resolve with the whole file as {} has it", snapshot.ours()),
        "conflict.take-theirs" => format!("Resolve with the whole file as {} has it", snapshot.theirs()),
        "conflict.keep" => "Resolve by keeping the file as the side that changed it has it".to_owned(),
        "conflict.delete" => "Resolve by deleting the file, as one side did".to_owned(),
        "conflict.mark-resolved" => "Stage the file as it is in the working tree".to_owned(),
        "conflict.unresolve" => "Discard the resolution and put the file back in conflict".to_owned(),
        _ => return None,
    })
}

fn command_button<T: Desk>(entry: commands::Entry, tip: Option<String>, cx: &Context<T>) -> gpui::Stateful<Div> {
    let mut button = kit::btn(entry.id, entry.title).small().enabled(entry.action.is_some());
    if let Some(tip) = entry.disabled.or(tip) {
        button = button.tip(tip);
    }
    match entry.action {
        Some(action) => button.on(action, cx),
        None => button.on(view::Action::Dismiss, cx),
    }
}

pub(crate) fn conflict_row<T: Desk>(app: &T, index: usize, conflict: &Conflict, cx: &mut Context<T>) -> gpui::Stateful<Div> {
    let ui = app.ui();
    let (name, folder) = file_name(&conflict.path);
    let path = conflict.path.clone();
    let target = Target::Conflict(path.clone());
    let menu = target.clone();
    let open = ui.merge.editing() == Some(conflict.path.as_path()) || ui.conflict.showing() == Some(conflict.path.as_path());
    let head = kit::row(("merge-open", index), open)
        .h(whole_px(FILE_ROW))
        .pl(px(10.))
        .pr_3()
        .gap_2()
        .child(kit::badge("Conflicted"))
        .child(kit::fit(name).size(12.5).cut(kit::Cut::Middle))
        .child(kit::fit(folder).size(11.5).color(palette().text_3).cut(kit::Cut::Start).grow())
        .when_some(kind_word(conflict.shape), |d, word| {
            d.child(div().flex_shrink_0().text_size(px(TEXT_TINY)).text_color(rgb(palette().text_3)).child(word))
        })
        .on_click(cx.listener(move |app, _, w, cx| {
            if !app.ui().busy() {
                app.dispatch(view::Action::Merge(merge::Action::Open(path.clone())), w, cx)
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = menu.clone();
                app.dispatch(view::Action::Menu(Some(view::Menu::Object { target, at: event.position })), w, cx);
            }),
        );
    let snapshot = ui.operation.snapshot();
    let buttons = commands::listed(ui, &target, ROW, cx).into_iter().map(|entry| {
        let tip = snapshot.and_then(|s| tip_for(entry.id, s));
        command_button(entry, tip, cx)
    });
    div()
        .id(("conflict", index))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .child(head)
        .child(div().pl(px(34.)).pr_3().pb(px(6.)).flex().flex_wrap().gap_1().children(buttons))
}

pub(crate) fn resolved_row<T: Desk>(app: &T, index: usize, path: &Path, cx: &mut Context<T>) -> gpui::Stateful<Div> {
    let ui = app.ui();
    let (name, folder) = file_name(path);
    let target = Target::Conflict(path.to_path_buf());
    let menu = target.clone();
    let again = commands::listed(ui, &target, &["conflict.unresolve"], cx).into_iter().map(|entry| {
        let tip = entry.disabled.clone().unwrap_or_else(|| entry.title.clone());
        let button = kit::btn(entry.id, "").small().ghost().icon("undo").tip(tip).enabled(entry.action.is_some());
        match entry.action {
            Some(action) => button.on(action, cx),
            None => button.on(view::Action::Dismiss, cx),
        }
    });
    div()
        .id(("resolved", index))
        .h(whole_px(FILE_ROW))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .child(kit::badge("Modified"))
        .child(kit::fit(name).size(12.5).cut(kit::Cut::Middle))
        .child(kit::fit(folder).size(11.5).color(palette().text_3).cut(kit::Cut::Start).grow())
        .child(kit::icon("check").size(px(13.)).text_color(rgb(palette().added)))
        .children(again)
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = menu.clone();
                app.dispatch(view::Action::Menu(Some(view::Menu::Object { target, at: event.position })), w, cx);
            }),
        )
}

pub(crate) fn sentence(snapshot: &Snapshot, conflict: &Conflict) -> String {
    let name = named(&conflict.path);
    let deleted = |changed: String, gone: String| format!("{name} was changed on {changed} and deleted on {gone}.");
    if conflict.side(Side::Current).is_none() && conflict.side(Side::Incoming).is_none() {
        return format!("Both sides deleted {name}, or moved it to different names.");
    }
    match conflict.shape {
        Shape::DeletedByUs => deleted(snapshot.theirs(), snapshot.ours()),
        Shape::DeletedByThem => deleted(snapshot.ours(), snapshot.theirs()),
        shape => {
            let what = match shape {
                Shape::Binary => ", a binary file",
                Shape::TooLarge => ", a file over 1 MiB",
                Shape::Symlink => ", a symbolic link",
                Shape::Submodule => ", a submodule",
                _ => "",
            };
            match (conflict.side(Side::Current), conflict.side(Side::Incoming)) {
                (None, _) => deleted(snapshot.theirs(), snapshot.ours()),
                (_, None) => deleted(snapshot.ours(), snapshot.theirs()),
                _ => format!("Both sides changed {name}{what}. Choose the version to keep."),
            }
        }
    }
}

pub(crate) fn chooser<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let (Some(snapshot), Some(path)) = (ui.operation.snapshot(), ui.conflict.showing()) else {
        return div();
    };
    let Some(conflict) = snapshot.conflict(path) else {
        return div();
    };
    let target = Target::Conflict(path.to_path_buf());
    let mut entries = commands::listed(ui, &target, ROW, cx);
    let mut take = |id: &str| entries.iter().position(|entry| entry.id == id).map(|at| entries.remove(at));
    let header = div()
        .h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::fit(named(path)).size(12.5).weight(FontWeight::SEMIBOLD).cut(kit::Cut::Start))
        .children(kind_word(conflict.shape).map(|word| kit::muted(word).flex_shrink_0()))
        .child(kit::grow())
        .child(kit::icon_btn("close-chooser", "x", "Back to the graph (Esc)", view::Action::Conflict(Action::Close), cx));
    let mut cards = Vec::new();
    for (index, (side, name, full)) in [
        (Side::Current, "Current", snapshot.ours()),
        (Side::Incoming, "Incoming", snapshot.theirs()),
    ]
    .into_iter()
    .enumerate()
    {
        let version = conflict.side(side);
        let id = match (version.is_some(), conflict.shape.deleted(), side) {
            (false, _, _) => "conflict.delete",
            (true, true, _) => "conflict.keep",
            (true, false, Side::Current) => "conflict.take-ours",
            (true, false, Side::Incoming) => "conflict.take-theirs",
        };
        let button = take(id).map(|entry| command_button(entry, tip_for(id, snapshot), cx));
        let dot = match side {
            Side::Current => palette().accent,
            Side::Incoming => palette().remote,
        };
        cards.push(
            div()
                .flex_1()
                .min_w_0()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(palette().line_strong))
                .bg(rgb(palette().raised))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(rgb(dot)))
                        .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).font_weight(FontWeight::SEMIBOLD).child(name))
                        .child(kit::fit(full).size(TEXT_SMALL).color(palette().text_3).grow()),
                )
                .children(crate::image_diff::chooser_card(ui, index))
                .child(div().text_size(px(TEXT_BASE)).text_color(rgb(palette().text_2)).child(holds(version)))
                .children(button),
        );
    }
    let resolved = take("conflict.mark-resolved")
        .map(|entry| command_button(entry, tip_for("conflict.mark-resolved", snapshot), cx));
    let body = div()
        .flex_1()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_size(px(TEXT_BASE)).child(sentence(snapshot, conflict)))
        .when_some(ui.conflict.note(), |d, note| {
            d.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(palette().text_2))
                    .child(format!("The line resolver cannot open this file: {note}")),
            )
        })
        .child(div().flex().gap_3().children(cards))
        .when_some(resolved, |d, button| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kit::muted("Or edit it in another app, then"))
                    .child(button),
            )
        });
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(palette().canvas))
        .child(header)
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_finder_writes_them() {
        assert_eq!(size(0), "Empty");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(12_345), "12.3 KB");
        assert_eq!(size(4_200_000), "4.2 MB");
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let version = |mode: &str| Version { mode: mode.into(), oid: oid.into(), size: Some(10) };
        assert_eq!(holds(None), "Deleted");
        assert_eq!(holds(Some(&version("100644"))), "10 bytes");
        assert_eq!(holds(Some(&version("120000"))), "Symbolic link");
        assert_eq!(holds(Some(&version("160000"))), "Submodule at commit 0123456");
    }

    #[test]
    fn a_file_deleted_on_both_sides_is_not_said_to_be_changed() {
        use crate::operation::{snapshot, tests::{commit, stops}};
        let f = crate::repository::tests::Fixture::new();
        commit(&f, "trail.txt", &(1..=50).map(|i| format!("{i}\n")).collect::<String>(), "Base");
        f.git(&["switch", "-qc", "side"]);
        f.git(&["mv", "trail.txt", "ridge.txt"]);
        f.git(&["commit", "-qm", "Side"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["mv", "trail.txt", "river.txt"]);
        f.git(&["commit", "-qm", "Main"]);
        stops(&f, &["merge", "side"]);
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        let said = |path: &str| sentence(&snapshot, snapshot.conflict(Path::new(path)).unwrap());
        assert_eq!(said("trail.txt"), "Both sides deleted trail.txt, or moved it to different names.");
        assert_eq!(said("river.txt"), "river.txt was changed on main and deleted on side.");
    }
}
