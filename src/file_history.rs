use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Side, Target},
    compare::{Names, Rev},
    jobs::Link,
    repository::{Repository, Run, changed_status, checked, paired_status, raw_path, validate_oid, validate_path},
    theme::*,
    view::{self, Desk, Menu, Selection, Ui},
};
use gpui::{
    Context, Div, FontWeight, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, prelude::*,
    relative, rgb, uniform_list,
};
use crate::theme::px;
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

const PAGE: usize = 64;
const LIST_WIDTH: f32 = 340.;
const ENTRY_ROW: f32 = 46.;
pub(crate) const NO_COMMITS: &str = "No commits changed this path.";
pub(crate) const END: &str = "End of history";
const NEVER_COMMITTED: &str = "This file has never been committed";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    Head,
    Commit(String),
    BaseOf(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    pub(crate) oid: String,
    pub(crate) subject: String,
    pub(crate) author: String,
    pub(crate) email: String,
    pub(crate) date: String,
    pub(crate) time: i64,
    pub(crate) path: PathBuf,
    pub(crate) from: Option<PathBuf>,
    pub(crate) status: &'static str,
}

impl Revision {
    pub(crate) fn moved(&self) -> Option<String> {
        let from = self.from.as_ref()?;
        let how = if self.status == "Copied" { "copied" } else { "renamed" };
        Some(format!("{how} from {}", from.display()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Back {
    selection: Option<Selection>,
    path: Option<PathBuf>,
    diff_open: bool,
    names: Option<Names>,
}

impl Back {
    fn of(ui: &Ui) -> Self {
        let path = match &ui.selection {
            Some(Selection::Commit(_)) => ui.inspection.path.clone(),
            Some(Selection::Stash(_)) => ui.stash.open_path().map(Path::to_path_buf),
            Some(Selection::Compare { .. }) => ui.compare.path.clone(),
            _ => None,
        };
        Self { selection: ui.selection.clone(), path, diff_open: ui.diff_open, names: ui.compare.names.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) path: PathBuf,
    pub(crate) shown: PathBuf,
    pub(crate) start: Start,
    back: Back,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(Request),
    OpenHere,
    Choose(usize),
    Step(isize),
    More,
    ShowInGraph(String),
    Close,
}

struct Open {
    request: Request,
    from: Option<String>,
    entries: Vec<Revision>,
    chosen: Option<usize>,
    loading: bool,
    more: bool,
    failed: Option<String>,
    stepping: bool,
    link: Option<Link>,
    scroll: UniformListScrollHandle,
}

#[derive(Default)]
pub(crate) struct State {
    open: Option<Open>,
    ticket: u64,
}

impl State {
    pub(crate) fn open(&self) -> bool {
        self.open.is_some()
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.open.as_ref().map(|open| open.request.shown.as_path())
    }

    pub(crate) fn start(&self) -> Option<&str> {
        self.open.as_ref()?.from.as_deref()
    }

    pub(crate) fn entries(&self) -> &[Revision] {
        self.open.as_ref().map_or(&[], |open| open.entries.as_slice())
    }

    pub(crate) fn chosen(&self) -> Option<&Revision> {
        let open = self.open.as_ref()?;
        open.entries.get(open.chosen?)
    }

    pub(crate) fn chosen_index(&self) -> Option<usize> {
        self.open.as_ref()?.chosen
    }

    pub(crate) fn loading(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.loading)
    }

    pub(crate) fn more(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.more)
    }

    pub(crate) fn entry(&self, oid: &str) -> Option<&Revision> {
        self.entries().iter().find(|entry| entry.oid == oid)
    }

    fn stop(&mut self) {
        if let Some(link) = self.open.as_mut().and_then(|open| open.link.take()) {
            link.cancel();
        }
    }

    fn close(&mut self) -> Option<Back> {
        self.stop();
        self.ticket += 1;
        self.open.take().map(|open| open.request.back)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Repository {
    pub(crate) fn file_revisions(
        &self,
        start: &Start,
        path: &Path,
        offset: usize,
        limit: usize,
    ) -> Result<(Option<String>, Vec<Revision>), String> {
        validate_path(path)?;
        let Some(from) = self.history_start(start)? else {
            return Ok((None, Vec::new()));
        };
        let output = checked(
            self.read_command()
                .args([
                    "log",
                    "--topo-order",
                    "--follow",
                    "--name-status",
                    "--diff-merges=first-parent",
                    "--no-show-signature",
                    "--no-notes",
                    "--no-color",
                    "--format=%H%x00%s%x00%an%x00%ae%x00%as%x00%at",
                    "-z",
                ])
                .arg(format!("--max-count={}", offset + limit))
                .args([from.as_str(), "--"])
                .arg(path),
        )?;
        let mut revisions = parse(&output, path)?;
        revisions.drain(..offset.min(revisions.len()));
        Ok((Some(from), revisions))
    }

    fn history_start(&self, start: &Start) -> Result<Option<String>, String> {
        let revision = match start {
            Start::Commit(oid) => {
                validate_oid(oid)?;
                return Ok(Some(oid.clone()));
            }
            Start::Head => "HEAD^{commit}".to_owned(),
            Start::BaseOf(oid) => {
                validate_oid(oid)?;
                format!("{oid}^1^{{commit}}")
            }
        };
        let output = self
            .read_command()
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options", &revision])
            .run()
            .map_err(|error| format!("Run bundled Git: {error}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let oid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        validate_oid(&oid)?;
        Ok(Some(oid))
    }
}

pub(crate) fn parse(output: &[u8], path: &Path) -> Result<Vec<Revision>, String> {
    let mut revisions = Vec::new();
    let Some(records) = output.strip_suffix(b"\0") else {
        return if output.is_empty() { Ok(revisions) } else { Err("Incomplete file history record".into()) };
    };
    let mut fields = records.split(|&b| b == 0).peekable();
    let mut named = path.to_owned();
    while let Some(oid) = fields.next() {
        let oid = String::from_utf8_lossy(oid.strip_prefix(b"\n").unwrap_or(oid)).into_owned();
        validate_oid(&oid).map_err(|_| "Invalid file history record".to_owned())?;
        let mut text = |missing: &'static str| {
            fields.next().map(|field| String::from_utf8_lossy(field).into_owned()).ok_or(missing)
        };
        let subject = text("Missing file history subject")?;
        let author = text("Missing file history author")?;
        let email = text("Missing file history email")?;
        let date = text("Missing file history date")?;
        let time = text("Missing file history time")?.trim().parse::<i64>().unwrap_or(0);
        let code = fields.peek().map(|code| code.strip_prefix(b"\n").unwrap_or(code));
        let mut status = "Modified";
        let mut from = None;
        if let Some((code, word)) = code.and_then(|code| Some((code, changed_status(code)?))) {
            let paired = paired_status(code);
            status = word;
            fields.next();
            let mut name = || fields.next().map(raw_path).ok_or("Missing file history path");
            let old = if paired { Some(name()?) } else { None };
            named = name()?;
            from = old.filter(|old| *old != named);
        }
        revisions.push(Revision { oid, subject, author, email, date, time, path: named.clone(), from, status });
    }
    Ok(revisions)
}

fn staged_origin(ui: &Ui, path: &Path, staged: bool) -> Option<PathBuf> {
    ui.files.iter().find(|row| row.path == path && row.staged == staged).and_then(|row| row.from.clone())
}

fn working_request(ui: &Ui, path: &Path, staged: bool) -> Option<Request> {
    let followed = staged_origin(ui, path, staged).unwrap_or_else(|| path.to_owned());
    Some(Request { path: followed, shown: path.to_owned(), start: Start::Head, back: Back::of(ui) })
}

pub(crate) fn request(ui: &Ui, path: &Path, side: &Side) -> Option<Request> {
    let start = match side {
        Side::Unstaged => return working_request(ui, path, false),
        Side::Staged => return working_request(ui, path, true),
        Side::Commit(oid) => Start::Commit(oid.clone()),
        Side::Stash(oid) => {
            let followed = ui.stash.from(oid, path).unwrap_or_else(|| path.to_owned());
            return Some(Request { path: followed, shown: path.to_owned(), start: Start::BaseOf(oid.clone()), back: Back::of(ui) });
        }
        Side::Compare(Rev::Commit(oid)) => Start::Commit(oid.clone()),
        Side::Compare(Rev::WorkingTree) => {
            let followed = staged_origin(ui, path, true).unwrap_or_else(|| path.to_owned());
            return Some(Request { path: followed, shown: path.to_owned(), start: Start::Head, back: Back::of(ui) });
        }
    };
    Some(Request { path: path.to_owned(), shown: path.to_owned(), start, back: Back::of(ui) })
}

pub(crate) fn here(ui: &Ui) -> Option<Request> {
    match ui.selection.as_ref()? {
        Selection::File { path, staged } => working_request(ui, path, *staged),
        Selection::Commit(oid) => request(ui, ui.inspection.path.as_deref()?, &Side::Commit(oid.clone())),
        Selection::Stash(oid) => request(ui, ui.stash.open_path()?, &Side::Stash(oid.clone())),
        Selection::Compare { to, .. } => request(ui, ui.compare.path.as_deref()?, &Side::Compare(to.clone())),
        Selection::Changes | Selection::Commits => None,
    }
}

fn unavailable(ui: &Ui, path: &Path, side: &Side) -> Option<String> {
    match side {
        Side::Unstaged | Side::Staged => {
            let staged = *side == Side::Staged;
            let row = ui.files.iter().find(|row| row.path == path && row.staged == staged);
            if ui.head.is_none() {
                Some("There are no commits yet".to_owned())
            } else if row.is_some_and(|row| row.status == "Untracked") {
                Some(NEVER_COMMITTED.to_owned())
            } else {
                None
            }
        }
        Side::Commit(_) | Side::Stash(_) | Side::Compare(_) => None,
    }
}

pub(crate) fn refused_here(ui: &Ui) -> Option<String> {
    let Some(Selection::File { path, staged }) = &ui.selection else { return None };
    unavailable(ui, path, if *staged { &Side::Staged } else { &Side::Unstaged })
}

pub(crate) fn offered_here(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::FileHistory || here(ui).is_some()
}

pub(crate) fn holding(ui: &Ui, selection: &Selection) -> Option<PathBuf> {
    let entry = ui.file_history.chosen()?;
    (*selection == Selection::Commit(entry.oid.clone())).then(|| entry.path.clone())
}

fn keeps(ui: &Ui, selection: &Selection) -> bool {
    holding(ui, selection).is_some() || (ui.file_history.chosen().is_none() && ui.selection.as_ref() == Some(selection))
}

pub(crate) fn selected(tab: &mut Tab, selection: &Selection) {
    if tab.ui.file_history.open() && !keeps(&tab.ui, selection) {
        tab.ui.file_history.close();
    }
}

pub(crate) fn leave(tab: &mut Tab) {
    tab.ui.file_history.close();
}

pub(crate) fn shows_diff(ui: &Ui) -> bool {
    ui.file_history
        .chosen()
        .is_some_and(|entry| ui.selection.as_ref() == Some(&Selection::Commit(entry.oid.clone())))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "file.history",
        Group::View,
        |_, _| "File history".into(),
        |ui, target, _| match target {
            Target::File { path, side } => Availability::unless(unavailable(ui, path, side)),
            _ => Availability::Hidden,
        },
        |ui, target| match target {
            Target::File { path, side } => request(ui, path, side).map(|request| view::Action::FileHistory(Action::Open(request))),
            _ => None,
        },
    )
    .words(&["log", "revisions", "commits that changed this file", "follow renames"])
    .icon("history"),
    Command::new(
        "revision.show-in-graph",
        Group::View,
        |_, _| "Show in graph".into(),
        |ui, target, _| match target {
            Target::Revision { oid, .. } if ui.file_history.entry(oid).is_some() => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Revision { oid, .. } => Some(view::Action::FileHistory(Action::ShowInGraph(oid.clone()))),
            _ => None,
        },
    )
    .words(&["reveal", "jump to commit", "view on graph"])
    .icon("crosshair"),
    Command::new(
        "revision.copy-sha",
        Group::Copy,
        |_, _| "Copy SHA".into(),
        |ui, target, _| match target {
            Target::Revision { oid, .. } if ui.file_history.entry(oid).is_some() => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Revision { oid, .. } => Some(crate::inspection::copy_sha(oid)),
            _ => None,
        },
    )
    .words(&["hash", "id", "clipboard"])
    .icon("copy"),
];

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(request) => open(tab, request, cx),
        Action::OpenHere => {
            if refused_here(&tab.ui).is_none()
                && let Some(request) = here(&tab.ui)
            {
                open(tab, request, cx);
            }
        }
        Action::Choose(index) => choose(tab, index, cx),
        Action::Step(delta) => step(tab, delta, cx),
        Action::More => {
            if tab.ui.file_history.more() && !tab.ui.file_history.loading() {
                load(tab, cx);
            }
        }
        Action::ShowInGraph(oid) => {
            if tab.ui.file_history.entry(&oid).is_none() {
                return;
            }
            tab.ui.file_history.close();
            tab.ui.diff_open = false;
            crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
        }
        Action::Close => close(tab, cx),
    }
    cx.notify();
}

fn open(tab: &mut Tab, mut request: Request, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    let state = &mut tab.ui.file_history;
    if let Some(open) = &state.open {
        request.back = open.request.back.clone();
    }
    state.close();
    state.open = Some(Open {
        request,
        from: None,
        entries: Vec::new(),
        chosen: None,
        loading: false,
        more: false,
        failed: None,
        stepping: false,
        link: None,
        scroll: UniformListScrollHandle::new(),
    });
    load(tab, cx);
}

fn load(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.file_history;
    let ticket = state.ticket;
    let Some(open) = state.open.as_mut() else { return };
    let offset = open.entries.len();
    let start = open.from.clone().map_or_else(|| open.request.start.clone(), Start::Commit);
    let path = open.request.path.clone();
    let link = Link::new(true);
    open.link = Some(link.clone());
    open.loading = true;
    open.failed = None;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || link.enter(|| repo.file_revisions(&start, &path, offset, PAGE + 1)),
        move |tab, result, cx| {
            let state = &mut tab.ui.file_history;
            if tab.epoch != epoch || state.ticket != ticket {
                return;
            }
            let Some(open) = state.open.as_mut() else { return };
            open.loading = false;
            open.link = None;
            match result {
                Ok((from, mut rows)) => {
                    if open.from.is_none() {
                        open.from = from;
                    }
                    open.more = rows.len() > PAGE;
                    rows.truncate(PAGE);
                    open.entries.extend(rows);
                    let stepping = std::mem::take(&mut open.stepping);
                    match open.chosen {
                        None if !open.entries.is_empty() => choose(tab, 0, cx),
                        Some(_) if stepping => step(tab, 1, cx),
                        _ => {}
                    }
                }
                Err(error) => {
                    open.stepping = false;
                    open.failed = Some(error);
                }
            }
            cx.notify();
        },
    );
}

fn choose(tab: &mut Tab, index: usize, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.file_history.open.as_mut() else { return };
    let Some(entry) = open.entries.get(index).cloned() else { return };
    open.chosen = Some(index);
    open.scroll.scroll_to_item(index, ScrollStrategy::Center);
    let selection = Selection::Commit(entry.oid);
    let showing = tab.ui.selection.as_ref() == Some(&selection)
        && tab.ui.diff_open
        && tab.ui.inspection.path.as_ref() == Some(&entry.path);
    if !showing {
        tab.choose(selection, cx);
    }
}

fn step(tab: &mut Tab, delta: isize, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.file_history.open.as_mut() else { return };
    let count = open.entries.len();
    if count == 0 {
        return;
    }
    let at = open.chosen.unwrap_or(0);
    let wanted = at.saturating_add_signed(delta);
    if wanted >= count && delta > 0 && open.more {
        if !open.loading {
            open.stepping = true;
            load(tab, cx);
        }
        return;
    }
    let index = wanted.min(count - 1);
    if open.chosen != Some(index) {
        choose(tab, index, cx);
    }
}

fn close(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(back) = tab.ui.file_history.close() else { return };
    let Some(selection) = back.selection else {
        tab.ui.diff_open = false;
        return;
    };
    match selection {
        Selection::File { path, staged } => {
            if tab.ui.files.iter().any(|row| row.path == path && row.staged == staged) {
                tab.choose(Selection::File { path, staged }, cx);
            } else {
                tab.choose(Selection::Changes, cx);
            }
        }
        Selection::Commit(oid) => {
            let selection = Selection::Commit(oid);
            let same = tab.ui.selection.as_ref() == Some(&selection) && tab.ui.inspection.path == back.path;
            if !same {
                tab.choose(selection, cx);
            }
            match back.path.filter(|_| back.diff_open) {
                Some(path) if !same => crate::inspection::open_when_loaded(tab, path, cx),
                Some(_) => {}
                None => tab.ui.diff_open = false,
            }
        }
        Selection::Compare { from, to, since_fork } => {
            let selection = Selection::Compare { from, to, since_fork };
            tab.ui.compare.clear_pick();
            tab.choose(selection.clone(), cx);
            if tab.ui.selection.as_ref() == Some(&selection) {
                tab.ui.compare.names = back.names;
                if let Some(path) = back.path.filter(|_| back.diff_open) {
                    crate::compare::open_when_loaded(tab, path, cx);
                }
            }
        }
        Selection::Stash(oid) => {
            let selection = Selection::Stash(oid);
            tab.choose(selection.clone(), cx);
            if let Some(path) = back.path.filter(|_| back.diff_open && tab.ui.selection.as_ref() == Some(&selection)) {
                crate::stash::open_when_loaded(tab, path, cx);
            }
        }
        selection => tab.choose(selection, cx),
    }
}

fn message(ui: &Ui) -> Option<String> {
    let open = ui.file_history.open.as_ref()?;
    if !open.entries.is_empty() {
        return None;
    }
    Some(match &open.failed {
        Some(error) => format!("The history could not be read. {error}"),
        None if open.loading => "Loading file history…".to_owned(),
        None => NO_COMMITS.to_owned(),
    })
}

pub(crate) fn button<T: Desk>(ui: &Ui, cx: &Context<T>) -> impl IntoElement + use<T> {
    let listing = view::centre(ui) == view::Centre::FileHistory;
    let refused = if listing { None } else { refused_here(ui) };
    let tip = match &refused {
        Some(reason) => reason.clone(),
        None if listing => "Back to where you were (Esc)".to_owned(),
        None => "Every commit that changed this file".to_owned(),
    };
    kit::btn("file-history", "History")
        .small()
        .ghost()
        .selected(listing)
        .enabled(refused.is_none())
        .tip(tip)
        .on(view::Action::FileHistory(if listing { Action::Close } else { Action::OpenHere }), cx)
}

fn entry_row<T: Desk>(index: usize, entry: &Revision, chosen: bool, now: i64, cx: &mut Context<T>) -> gpui::Stateful<Div> {
    let short: String = entry.oid.chars().take(7).collect();
    let when = view::when(entry.time, &entry.date, now);
    let time = entry.time;
    let target = Target::Revision { oid: entry.oid.clone(), path: entry.path.clone() };
    kit::row(("file-revision", index), chosen)
        .h(px(ENTRY_ROW))
        .pl_3()
        .pr_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(kit::fit(entry.subject.clone()).weight(FontWeight::MEDIUM).grow())
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(MONO)
                                .text_size(px(11.5))
                                .text_color(rgb(palette().text_3))
                                .child(short),
                        ),
                )
                .child(
                    div()
                        .id(("file-revision-when", index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .tooltip(kit::tip_with(move || view::exact_time(time)))
                        .child(kit::fit(format!("{} · {when}", entry.author)).size(11.5).color(palette().text_3).grow())
                        .when_some(entry.moved(), |d, moved| {
                            d.child(kit::fit(moved).size(11.5).color(palette().text_2).cut(kit::Cut::Start))
                        }),
                ),
        )
        .on_click(cx.listener(move |app, _, w, cx| {
            app.dispatch(view::Action::FileHistory(Action::Choose(index)), w, cx)
        }))
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                let target = target.clone();
                app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
            }),
        )
}

fn list<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(open) = ui.file_history.open.as_ref() else { return div() };
    let header = div()
        .min_h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::icon("history").size(px(14.)).flex_shrink_0().text_color(rgb(palette().text_3)))
        .child(kit::fit(open.request.shown.to_string_lossy().into_owned()).size(12.5).weight(FontWeight::SEMIBOLD).cut(kit::Cut::Start).grow())
        .child(kit::icon_btn("close-file-history", "x", "Back to where you were (Esc)", view::Action::FileHistory(Action::Close), cx));
    let column = div()
        .w(px(LIST_WIDTH))
        .max_w(relative(0.45))
        .flex_shrink_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(palette().canvas))
        .border_r_1()
        .border_color(rgb(palette().line))
        .child(header);
    if let Some(text) = message(ui) {
        return column.child(div().p_4().child(kit::muted(text)));
    }
    let count = open.entries.len();
    let entries = uniform_list(
        SharedString::from(format!("file-revisions-{}", open.request.shown.display())),
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let state = &app.ui().file_history;
            let chosen = state.chosen_index();
            let now = view::now();
            range
                .filter_map(|i| {
                    let entry = state.entries().get(i)?;
                    Some(entry_row(i, entry, chosen == Some(i), now, cx))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(open.scroll.clone())
    .flex_1()
    .min_h_0();
    let footer = div().flex_shrink_0().px_3().py_2().flex().items_center().border_t_1().border_color(rgb(palette().line));
    let footer = if open.more {
        footer.justify_end().child(
            kit::btn("older-file-history", "Load older")
                .small()
                .enabled(!open.loading)
                .on(view::Action::FileHistory(Action::More), cx),
        )
    } else if let Some(error) = &open.failed {
        footer.child(kit::muted(format!("The history could not be read. {error}")))
    } else {
        footer.child(kit::muted(END))
    };
    column
        .child(entries)
        .child(kit::scrollbar("file-revisions-scrollbar", &open.scroll))
        .child(footer)
}

pub(crate) fn render<T: Desk>(app: &T, window: &Window, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let pane = if shows_diff(ui) {
        view::diff_pane(app, window, cx)
    } else {
        div().flex_1().min_w_0().min_h_0().flex().items_center().justify_center().child(kit::muted(
            if ui.file_history.entries().is_empty() { "" } else { "Loading diff…" },
        ))
    };
    div().flex_1().min_w_0().min_h_0().flex().child(list(app, cx)).child(pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn write(f: &Fixture, path: &str, content: &str) {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(f.dir.join(parent)).unwrap();
        }
        fs::write(f.dir.join(path), content).unwrap();
        f.repo.stage_file(Path::new(path)).unwrap();
    }

    fn head(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn untouched(f: &Fixture) -> Vec<Vec<u8>> {
        vec![
            f.git(&["for-each-ref"]),
            f.git(&["ls-files", "-s"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
        ]
    }

    const TRAIL: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    fn moved_notes(f: &Fixture) -> Vec<String> {
        write(f, "notes.txt", TRAIL);
        f.repo.commit("Start notes").unwrap();
        write(f, "notes.txt", &TRAIL.replace("two", "TWO"));
        write(f, "other.txt", "other\n");
        f.repo.commit("Edit notes").unwrap();
        fs::create_dir(f.dir.join("docs")).unwrap();
        f.git(&["mv", "notes.txt", "docs/notes é.md"]);
        write(f, "docs/notes é.md", &TRAIL.replace("two", "TWO").replace("nine", "NINE"));
        f.repo.commit("Move notes").unwrap();
        write(f, "docs/notes é.md", &TRAIL.replace("two", "TWO").replace("nine", "NINE").replace("ten", "TEN"));
        f.repo.commit("Edit moved notes").unwrap();
        String::from_utf8(f.git(&["log", "--format=%H"])).unwrap().lines().map(str::to_owned).collect()
    }

    #[test]
    fn file_history_crosses_the_rename_names_the_path_at_each_commit_and_says_where_it_came_from() {
        let f = Fixture::new();
        let commits = moved_notes(&f);
        let new = Path::new("docs/notes é.md");
        let before = untouched(&f);
        let (from, history) = f.repo.file_revisions(&Start::Commit(commits[0].clone()), new, 0, 64).unwrap();
        assert_eq!(from.as_deref(), Some(commits[0].as_str()));
        let rows: Vec<(&str, &Path, Option<String>)> =
            history.iter().map(|row| (row.subject.as_str(), row.path.as_path(), row.moved())).collect();
        assert_eq!(
            rows,
            [
                ("Edit moved notes", new, None),
                ("Move notes", new, Some("renamed from notes.txt".to_owned())),
                ("Edit notes", Path::new("notes.txt"), None),
                ("Start notes", Path::new("notes.txt"), None),
            ]
        );
        assert_eq!(history.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>(), commits);
        assert_eq!(history.iter().map(|row| row.status).collect::<Vec<_>>(), ["Modified", "Renamed", "Modified", "Added"]);
        assert!(history.iter().all(|row| row.author == "Cedar" && row.email == "cedar@example.invalid"));
        let today = String::from_utf8(f.git(&["log", "-1", "--format=%as%n%at"])).unwrap();
        let (date, time) = today.trim().split_once('\n').unwrap();
        assert_eq!((history[0].date.as_str(), history[0].time), (date, time.parse::<i64>().unwrap()));
        let (_, page) = f.repo.file_revisions(&Start::Commit(commits[0].clone()), new, 2, 1).unwrap();
        assert_eq!(page, history[2..3]);
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn a_working_tree_file_starts_at_head_and_a_stashed_one_at_the_commit_it_was_stashed_on() {
        let f = Fixture::new();
        let commits = moved_notes(&f);
        let new = Path::new("docs/notes é.md");
        fs::write(f.dir.join(new), "edited in my files\n").unwrap();
        let (from, history) = f.repo.file_revisions(&Start::Head, new, 0, 64).unwrap();
        assert_eq!(from.as_deref(), Some(commits[0].as_str()));
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].oid, commits[0]);
        f.git(&["stash", "push", "-q"]);
        let stash = String::from_utf8(f.git(&["rev-parse", "refs/stash"])).unwrap().trim().to_owned();
        write(&f, "docs/notes é.md", "after the stash\n");
        f.repo.commit("After the stash").unwrap();
        let (from, history) = f.repo.file_revisions(&Start::BaseOf(stash), new, 0, 64).unwrap();
        assert_eq!(from.as_deref(), Some(commits[0].as_str()));
        assert_eq!(history[0].subject, "Edit moved notes");
        let (from, history) = f.repo.file_revisions(&Start::Head, new, 0, 64).unwrap();
        assert_eq!(from, Some(head(&f)));
        assert_eq!(history[0].subject, "After the stash");
    }

    #[test]
    fn an_empty_repository_has_no_history_and_bad_starts_are_refused() {
        let f = Fixture::new();
        assert_eq!(f.repo.file_revisions(&Start::Head, Path::new("a.txt"), 0, 64).unwrap(), (None, Vec::new()));
        assert!(f.repo.file_revisions(&Start::Commit("HEAD".into()), Path::new("a.txt"), 0, 64).is_err());
        assert!(f.repo.file_revisions(&Start::BaseOf("main".into()), Path::new("a.txt"), 0, 64).is_err());
        assert!(f.repo.file_revisions(&Start::Head, Path::new("../a.txt"), 0, 64).is_err());
        write(&f, "a.txt", "a\n");
        f.repo.commit("Add a").unwrap();
        assert_eq!(f.repo.file_revisions(&Start::Head, Path::new("missing.txt"), 0, 64).unwrap(), (Some(head(&f)), Vec::new()));
    }

    #[test]
    fn file_history_includes_a_merge_through_its_first_parent_and_reads_paths_with_newlines() {
        let output = [
            "a".repeat(40).as_bytes(),
            b"\0Merge side\0Ann\0ann@example.invalid\02026-01-02\01767312000\0\nM\0new\nname\0",
            "b".repeat(40).as_bytes(),
            b"\0Move\0Ann\0ann@example.invalid\02026-01-01\01767225600\0\nR100\0old\0new\nname\0",
            "c".repeat(40).as_bytes(),
            b"\0Start\0Bo\0bo@example.invalid\02025-12-31\01767139200\0\nA\0old\0",
        ]
        .concat();
        let rows = parse(&output, Path::new("new\nname")).unwrap();
        let paths: Vec<&Path> = rows.iter().map(|row| row.path.as_path()).collect();
        assert_eq!(paths, [Path::new("new\nname"), Path::new("new\nname"), Path::new("old")]);
        assert_eq!(rows[1].subject, "Move");
        assert_eq!(rows[1].from.as_deref(), Some(Path::new("old")));
        assert_eq!(rows[2].author, "Bo");
        assert_eq!(rows[2].time, 1767139200);
        assert!(parse(b"zz\0x\0", Path::new("a")).is_err());

        let f = Fixture::new();
        write(&f, "f.txt", "1\n2\n3\n4\n5\n6\n");
        f.repo.commit("Start").unwrap();
        f.git(&["switch", "-qc", "side"]);
        write(&f, "f.txt", "1\n2\n3\n4\n5\n6\nside\n");
        f.repo.commit("Side").unwrap();
        f.git(&["switch", "-q", "main"]);
        write(&f, "f.txt", "one\n2\n3\n4\n5\n6\n");
        f.repo.commit("Main").unwrap();
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.git(&["mv", "f.txt", "g.txt"]);
        f.repo.commit("Move").unwrap();
        let (_, rows) = f.repo.file_revisions(&Start::Head, Path::new("g.txt"), 0, 64).unwrap();
        let subjects: Vec<String> = rows.into_iter().map(|row| row.subject).collect();
        assert_eq!(subjects, ["Move", "Merge branch 'side'", "Side", "Main", "Start"]);
    }

    fn opened(path: &str, start: Start, link: &Link) -> State {
        State {
            open: Some(Open {
                request: Request {
                    path: path.into(),
                    shown: path.into(),
                    start,
                    back: Back { selection: None, path: None, diff_open: false, names: None },
                },
                from: None,
                entries: Vec::new(),
                chosen: None,
                loading: true,
                more: false,
                failed: None,
                stepping: false,
                link: Some(link.clone()),
                scroll: UniformListScrollHandle::new(),
            }),
            ticket: 0,
        }
    }

    fn running(name: &str) -> bool {
        std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", name])
            .output()
            .is_ok_and(|output| !output.stdout.is_empty())
    }

    #[test]
    fn closing_the_view_stops_the_git_process_that_reads_the_history() {
        let f = Fixture::new();
        let name = format!("probe-{}-{}.txt", std::process::id(), f.dir.file_name().unwrap().to_string_lossy());
        write(&f, &name, "probe\n");
        f.repo.commit("Add the probe").unwrap();
        let tip = head(&f);
        let alternates = f.dir.join(".git/objects/info/alternates");
        fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        let fifo = std::ffi::CString::new(alternates.to_string_lossy().into_owned()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let link = Link::new(true);
        let mut state = opened(&name, Start::Commit(tip.clone()), &link);
        let reader = {
            let (repo, link, path) = (f.repo.clone(), link.clone(), PathBuf::from(&name));
            std::thread::spawn(move || link.enter(|| repo.file_revisions(&Start::Commit(tip), &path, 0, PAGE + 1)))
        };
        let waited = std::time::Instant::now();
        while !running(&name) {
            assert!(waited.elapsed() < std::time::Duration::from_secs(10), "git log never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(running(&name));
        let closed = std::time::Instant::now();
        assert!(state.close().is_some());
        assert!(link.cancelled());
        let result = reader.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(closed.elapsed() < std::time::Duration::from_secs(3));
        assert!(!running(&name), "git log is still running after the view closed");
        assert!(!state.open());
        fs::remove_file(&alternates).unwrap();
    }

    #[test]
    fn dropping_an_open_history_cancels_its_read() {
        let link = Link::new(true);
        let state = opened("a.txt", Start::Head, &link);
        assert!(!link.cancelled());
        drop(state);
        assert!(link.cancelled());
    }
}
