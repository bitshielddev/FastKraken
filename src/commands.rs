use crate::{
    kit,
    notice::Notice,
    view::{self, Action, Desk, Selection, Tone, Ui},
};
use crate::theme::ROW;
use gpui::{App, Context, Div, ElementId, KeyBinding, SharedString, Window, div, prelude::*, px};
use std::{ffi::OsString, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Unstaged,
    Staged,
    Commit(String),
    Stash(String),
    Compare(crate::compare::Rev),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Repo,
    Commit(String),
    Commits(Vec<String>),
    Branch(OsString),
    Branches(Vec<OsString>),
    RemoteBranch(OsString),
    Tag(OsString),
    Stash(String),
    Remote(String),
    File { path: PathBuf, side: Side },
    Revision { oid: String, path: PathBuf },
    Files { paths: Vec<PathBuf>, side: Side },
    Folder { path: PathBuf, side: Side },
    FileList(crate::file_list::Panel),
    Conflict(PathBuf),
    Code { path: Option<PathBuf>, line: Option<u32>, row: usize, hunk: Option<usize> },
    Lines { path: PathBuf, side: Side, count: usize },
    Columns,
    Section(crate::refs::Section),
    Worktree(PathBuf),
    Submodule(PathBuf),
    PullRequest { remote: String, number: u64 },
    Pair { dragged: Box<Target>, onto: Box<Target> },
}

impl Target {
    pub(crate) fn current(ui: &Ui) -> Self {
        match &ui.selection {
            Some(Selection::File { path, staged }) => Self::File {
                path: path.clone(),
                side: if *staged { Side::Staged } else { Side::Unstaged },
            },
            Some(Selection::Commit(oid)) => match &ui.inspection.path {
                Some(path) if ui.diff_open => Self::File {
                    path: path.clone(),
                    side: Side::Commit(oid.clone()),
                },
                _ => Self::Commit(oid.clone()),
            },
            Some(Selection::Stash(oid)) => match ui.stash.open_path() {
                Some(path) if ui.diff_open => Self::File {
                    path: path.to_path_buf(),
                    side: Side::Stash(oid.clone()),
                },
                _ => Self::Stash(oid.clone()),
            },
            Some(Selection::Compare { .. } | Selection::Commits) => match crate::compare::picked_commits(ui) {
                commits if commits.len() > 1 => Self::Commits(commits.to_vec()),
                _ => Self::Repo,
            },
            Some(Selection::Changes) | None => match crate::conflict::open_path(ui) {
                Some(path) => Self::Conflict(path.to_path_buf()),
                None => Self::Repo,
            },
        }
    }

    pub(crate) fn of_ref(kind: crate::refs::Kind, name: OsString) -> Self {
        match kind {
            crate::refs::Kind::Branch => Self::Branch(name),
            crate::refs::Kind::Remote => Self::RemoteBranch(name),
            crate::refs::Kind::Tag => Self::Tag(name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Hidden,
    Enabled,
    Disabled(String),
}

impl Availability {
    pub(crate) fn unless(reason: Option<String>) -> Self {
        reason.map_or(Self::Enabled, Self::Disabled)
    }
}

pub(crate) fn on_repo(ui: &Ui, target: &Target) -> bool {
    *target == Target::Repo && !ui.repo.is_empty()
}

pub(crate) fn busy(ui: &Ui) -> Option<String> {
    ui.busy().then(|| "Wait for the current operation to finish".to_owned())
}

pub(crate) fn networking(ui: &Ui) -> Option<String> {
    ui.jobs.refusal()
}

pub(crate) fn operating(ui: &Ui) -> Option<String> {
    ui.operation.refusal()
}

pub(crate) fn commit(target: &Target) -> Option<&str> {
    match target {
        Target::Commit(oid) => Some(oid),
        _ => None,
    }
}

pub(crate) fn unselected(ui: &Ui, oid: &str) -> Option<String> {
    (!matches!(&ui.selection, Some(Selection::Commit(id)) if id == oid))
        .then(|| "Select the commit first".to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Tabs,
    Repository,
    Recent,
    Sync,
    Changes,
    Stash,
    Create,
    Switch,
    Reverse,
    Remotes,
    Manage,
    View,
    Open,
    Copy,
    App,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bar {
    App,
    File,
    Edit,
    View,
    Repository,
    Branch,
    Window,
    Help,
}

pub(crate) type TitleFn = fn(&Ui, &Target) -> String;
pub(crate) type WhenFn = fn(&Ui, &Target, &App) -> Availability;
pub(crate) type RunFn = fn(&Ui, &Target) -> Option<Action>;
pub(crate) type MarkFn = fn(&Ui, &Target) -> Option<Mark>;
pub(crate) type TickFn = fn(&Ui, &Target) -> bool;

#[derive(Clone, Debug)]
pub(crate) enum Mark {
    Default,
    Offer(Action),
}

pub(crate) struct Command {
    pub id: &'static str,
    pub title: TitleFn,
    pub icon: Option<&'static str>,
    pub group: Group,
    pub shortcut: Option<&'static str>,
    pub scope: Option<&'static str>,
    pub danger: bool,
    pub bar: Option<Bar>,
    pub words: &'static [&'static str],
    pub when: WhenFn,
    pub run: RunFn,
    pub submenu: Option<&'static Submenu>,
    pub mark: Option<MarkFn>,
    pub tick: Option<TickFn>,
}

pub(crate) struct Submenu {
    pub id: &'static str,
    pub title: TitleFn,
    pub icon: &'static str,
}

impl Command {
    pub(crate) const fn new(id: &'static str, group: Group, title: TitleFn, when: WhenFn, run: RunFn) -> Self {
        Self {
            id,
            title,
            icon: None,
            group,
            shortcut: None,
            scope: None,
            danger: false,
            bar: None,
            words: &[],
            when,
            run,
            submenu: None,
            mark: None,
            tick: None,
        }
    }

    pub(crate) const fn tick(mut self, tick: TickFn) -> Self {
        self.tick = Some(tick);
        self
    }

    pub(crate) const fn mark(mut self, mark: MarkFn) -> Self {
        self.mark = Some(mark);
        self
    }

    pub(crate) const fn under(mut self, submenu: &'static Submenu) -> Self {
        self.submenu = Some(submenu);
        self
    }

    pub(crate) const fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) const fn keys(mut self, shortcut: &'static str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    pub(crate) const fn scope(mut self, scope: &'static str) -> Self {
        self.scope = Some(scope);
        self
    }

    pub(crate) const fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub(crate) const fn bar(mut self, bar: Bar) -> Self {
        self.bar = Some(bar);
        self
    }

    pub(crate) const fn words(mut self, words: &'static [&'static str]) -> Self {
        self.words = words;
        self
    }
}

pub(crate) fn all() -> impl Iterator<Item = &'static Command> {
    [
        crate::tabs::COMMANDS,
        crate::clone::COMMANDS,
        crate::init::COMMANDS,
        crate::start::COMMANDS,
        view::COMMANDS,
        crate::discard::COMMANDS,
        crate::picked::COMMANDS,
        crate::file_list::COMMANDS,
        crate::file_list::FOLDER_COMMANDS,
        crate::ignore::COMMANDS,
        crate::search::COMMANDS,
        crate::find::COMMANDS,
        crate::activity::COMMANDS,
        crate::sync::COMMANDS,
        crate::connect::COMMANDS,
        crate::address::COMMANDS,
        crate::publish::COMMANDS,
        crate::stash::COMMANDS,
        crate::autostash::COMMANDS,
        crate::refs::COMMANDS,
        crate::branch::COMMANDS,
        crate::tag::COMMANDS,
        crate::tracking::COMMANDS,
        crate::merge::COMMANDS,
        crate::pick::COMMANDS,
        crate::rebase::COMMANDS,
        crate::rebase_editor::COMMANDS,
        crate::operation::COMMANDS,
        crate::conflict::COMMANDS,
        crate::reverse::COMMANDS,
        crate::rewrite::COMMANDS,
        crate::recover::COMMANDS,
        crate::inspection::COMMANDS,
        crate::file_history::COMMANDS,
        crate::blame::COMMANDS,
        crate::compare::COMMANDS,
        crate::browse::COMMANDS,
        crate::diff_select::COMMANDS,
        crate::handoff::COMMANDS,
        crate::patch::COMMANDS,
        crate::diff_select::SELECT_COMMANDS,
        crate::syntax::COMMANDS,
        crate::diff_options::COMMANDS,
        crate::diff_text::COMMANDS,
        crate::image_diff::COMMANDS,
        crate::file_view::COMMANDS,
        crate::palette::COMMANDS,
        crate::help::COMMANDS,
        crate::preferences::COMMANDS,
        crate::identity::COMMANDS,
        crate::credentials::COMMANDS,
        crate::cli::COMMANDS,
        crate::columns::COMMANDS,
        crate::graph_view::COMMANDS,
        crate::pin::COMMANDS,
        crate::sidebar::COMMANDS,
        crate::cleanup::COMMANDS,
        crate::push_to::COMMANDS,
        crate::pull_request::COMMANDS,
        crate::pulls::COMMANDS,
        crate::forks::COMMANDS,
        crate::drag::COMMANDS,
        crate::journal::COMMANDS,
        crate::worktree::COMMANDS,
        crate::submodule::COMMANDS,
        crate::lfs::COMMANDS,
        crate::sparse::COMMANDS,
    ]
    .into_iter()
    .flatten()
}

pub(crate) fn find(id: &str) -> Option<&'static Command> {
    all().find(|command| command.id == id)
}

pub(crate) struct Entry {
    pub id: &'static str,
    pub title: String,
    pub icon: Option<&'static str>,
    pub group: Group,
    pub shortcut: Option<&'static str>,
    pub danger: bool,
    pub disabled: Option<String>,
    pub action: Option<Action>,
    pub children: Vec<Entry>,
    pub mark: Option<Mark>,
    pub check: Option<bool>,
    pub note: Option<Note>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    pub tip: Option<String>,
    pub warn: bool,
}

impl Entry {
    pub(crate) fn note(id: &'static str, title: String, note: Note) -> Self {
        Self {
            id,
            title,
            icon: None,
            group: Group::Switch,
            shortcut: None,
            danger: false,
            disabled: None,
            action: None,
            children: Vec::new(),
            mark: None,
            check: None,
            note: Some(note),
        }
    }
}

pub(crate) fn entry(command: &'static Command, ui: &Ui, target: &Target, cx: &App) -> Option<Entry> {
    let disabled = match (command.when)(ui, target, cx) {
        Availability::Hidden => return None,
        Availability::Enabled => None,
        Availability::Disabled(reason) => Some(reason),
    };
    let action = if disabled.is_none() {
        Some((command.run)(ui, target)?)
    } else {
        None
    };
    Some(Entry {
        id: command.id,
        title: (command.title)(ui, target),
        icon: command.icon,
        group: command.group,
        shortcut: command.shortcut,
        danger: command.danger,
        disabled,
        action,
        children: Vec::new(),
        mark: command.mark.and_then(|mark| mark(ui, target)),
        check: command.tick.map(|tick| tick(ui, target)),
        note: None,
    })
}

fn submenu(id: &'static str, title: String, icon: &'static str, group: Group, children: Vec<Entry>) -> Entry {
    Entry {
        id,
        title,
        icon: Some(icon),
        group,
        shortcut: None,
        danger: false,
        disabled: None,
        action: None,
        children,
        mark: None,
        check: None,
        note: None,
    }
}

pub(crate) fn menu(ui: &Ui, target: &Target, cx: &App) -> Vec<Entry> {
    rows(ui, target, cx, false)
}

fn rows(ui: &Ui, target: &Target, cx: &App, flat: bool) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for command in all() {
        let Some(row) = entry(command, ui, target, cx) else { continue };
        let Some(sub) = command.submenu else {
            entries.push(row);
            continue;
        };
        match entries.iter_mut().find(|entry| entry.id == sub.id) {
            Some(parent) => parent.children.push(row),
            None => {
                let title = (sub.title)(ui, target);
                entries.push(submenu(sub.id, title, sub.icon, row.group, vec![row]));
            }
        }
    }
    for parent in entries.iter_mut().filter(|entry| !entry.children.is_empty()) {
        if all().any(|command| command.submenu.is_some_and(|sub| sub.id == parent.id)) {
            parent.disabled = shared_reason(&parent.children);
            parent.action = parent
                .children
                .iter()
                .find(|child| matches!(child.mark, Some(Mark::Default)))
                .and_then(|child| child.action.clone());
        }
    }
    crate::forecast::annotate(ui, target, &mut entries);
    if let Target::Tag(_) = target {
        crate::tag::fan_out(ui, &mut entries, flat);
    }
    if let Target::Commit(oid) = target {
        if let Some(row) = switch_here(ui, oid, cx) {
            entries.insert(0, row);
        }
    }
    if let Target::Section(section) = target {
        entries.extend(listed(ui, &Target::Repo, crate::sidebar::repo_rows(*section), cx));
    }
    for (title, other) in crate::drag::alternatives(ui, target) {
        let children = rows(ui, &other, cx, true);
        if !children.is_empty() {
            entries.push(submenu("drop.also", title, "cloud", Group::View, children));
        }
    }
    entries.sort_by_key(|entry| entry.group);
    crate::pulls::merged_first(ui, target, &mut entries);
    entries
}

fn shared_reason(rows: &[Entry]) -> Option<String> {
    let first = rows.first()?.disabled.clone()?;
    rows.iter().all(|row| row.disabled.as_ref() == Some(&first)).then_some(first)
}

pub(crate) fn full_title(command: &Command, ui: &Ui, target: &Target) -> String {
    let title = (command.title)(ui, target);
    match command.submenu {
        Some(sub) => format!("{} › {title}", (sub.title)(ui, target)),
        None => title,
    }
}

fn switch_here(ui: &Ui, oid: &str, cx: &App) -> Option<Entry> {
    let switch = find("branch.switch")?;
    let mut rows: Vec<(String, Entry)> = ui
        .refs
        .items
        .iter()
        .filter(|item| item.kind == crate::refs::Kind::Branch && item.oid.as_deref() == Some(oid))
        .filter_map(|item| {
            let row = entry(switch, ui, &Target::Branch(item.name.clone()), cx)?;
            Some((item.name.to_string_lossy().into_owned(), row))
        })
        .collect();
    match rows.len() {
        0 => None,
        1 => rows.pop().map(|(_, row)| row),
        _ => {
            let rows = rows
                .into_iter()
                .map(|(name, mut row)| {
                    row.title = name;
                    row
                })
                .collect();
            Some(submenu("commit.switch", "Switch to".into(), "branch", switch.group, rows))
        }
    }
}

fn hidden_labels(ui: &Ui, oid: &str, cx: &App) -> Vec<Entry> {
    let Some(row) = ui.history.iter().find(|row| row.oid == oid) else {
        return Vec::new();
    };
    view::row_labels(ui, &row.refs)
        .into_iter()
        .skip(view::SHOWN_LABELS)
        .flat_map(|label| {
            let remote = label.remote.map(|name| (kit::Chip::Remote, Target::RemoteBranch(name.as_str().into()), name));
            label.target.map(|target| (label.chip, target, label.name)).into_iter().chain(remote)
        })
        .map(|(chip, target, name)| {
            let icon = match chip {
                kit::Chip::Remote => "cloud",
                kit::Chip::Tag => "tag",
                _ => "branch",
            };
            let mut row = submenu("label", name, icon, Group::Switch, rows(ui, &target, cx, true));
            row.action = default_command(&target).map(|_| Action::Activate(target));
            row
        })
        .collect()
}

pub(crate) fn entries_of(ui: &Ui, open: &view::Menu, cx: &App) -> Vec<Entry> {
    use view::Menu;
    match open {
        Menu::Repo => {
            let mut rows = listed(ui, &Target::Repo, view::REPO_MENU, cx);
            let at = rows.iter().position(|row| row.id == "repo.open").map_or(0, |at| at + 1);
            rows.splice(at..at, crate::start::menu_rows(ui, cx));
            rows
        }
        Menu::Remotes { .. } => listed(ui, &Target::Repo, crate::publish::REMOTES_MENU, cx),
        Menu::Changes { .. } => listed(ui, &Target::Repo, view::CHANGES_MENU, cx),
        Menu::CommitActions => match &ui.selection {
            Some(Selection::Commit(oid)) => menu(ui, &Target::Commit(oid.clone()), cx),
            _ => Vec::new(),
        },
        Menu::Object { target, .. } => menu(ui, target, cx),
        Menu::Labels { oid, .. } => hidden_labels(ui, oid, cx),
        Menu::Pair { branch, remote, .. } => paired_labels(ui, branch, remote, cx),
    }
}

fn paired_labels(ui: &Ui, branch: &std::ffi::OsStr, remote: &std::ffi::OsStr, cx: &App) -> Vec<Entry> {
    let mut entries = menu(ui, &Target::Branch(branch.to_owned()), cx);
    let target = Target::RemoteBranch(remote.to_owned());
    let title = remote.to_string_lossy().into_owned();
    let mut row = submenu("pair.remote", title, "cloud", Group::Switch, rows(ui, &target, cx, true));
    row.action = default_command(&target).map(|_| Action::Activate(target));
    if entries.last().is_some_and(|last| last.group == Group::Switch) {
        row.group = Group::View;
    }
    entries.push(row);
    entries
}

pub(crate) fn listed(ui: &Ui, target: &Target, ids: &[&str], cx: &App) -> Vec<Entry> {
    ids.iter()
        .filter_map(|id| find(id))
        .filter_map(|command| entry(command, ui, target, cx))
        .collect()
}

fn breaks(groups: &[Group]) -> Vec<bool> {
    groups
        .iter()
        .enumerate()
        .map(|(i, group)| i > 0 && groups[i - 1] != *group)
        .collect()
}

pub(crate) fn tip_with_keys(text: &str, id: &str) -> String {
    match find(id).and_then(|command| command.shortcut) {
        Some(shortcut) => format!("{text} ({})", keys(shortcut)),
        None => text.to_owned(),
    }
}

pub(crate) fn keys(shortcut: &str) -> String {
    let mut parts: Vec<&str> = shortcut.split('-').collect();
    if shortcut.ends_with("--") {
        parts.retain(|part| !part.is_empty());
        parts.push("-");
    }
    let Some(key) = parts.pop() else {
        return String::new();
    };
    let mut shown = String::new();
    for (modifier, glyph) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")] {
        if parts.contains(&modifier) {
            shown.push_str(glyph);
        }
    }
    shown.push_str(&match key {
        "enter" => "↩".to_owned(),
        "escape" => "⎋".to_owned(),
        "space" => "Space".to_owned(),
        "tab" => "⇥".to_owned(),
        "backspace" => "⌫".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "pageup" => "⇞".to_owned(),
        "pagedown" => "⇟".to_owned(),
        "home" => "↖".to_owned(),
        "end" => "↘".to_owned(),
        key => key.to_uppercase(),
    });
    shown
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub row: Option<usize>,
    pub open: bool,
    pub sub: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
}

fn stepped(at: Option<usize>, len: usize, down: bool) -> usize {
    match (at, down) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => (i + len - 1) % len,
    }
}

fn moved(entries: &[Entry], cursor: Cursor, key: Key) -> (Cursor, Option<(usize, Option<usize>)>) {
    let cursor = if cursor.row.is_some_and(|row| row >= entries.len()) { Cursor::default() } else { cursor };
    let children = cursor.row.and_then(|row| entries.get(row)).map_or(0, |entry| entry.children.len());
    let mut next = cursor;
    match key {
        Key::Up | Key::Down if cursor.open && children > 0 => {
            next.sub = Some(stepped(cursor.sub, children, key == Key::Down));
        }
        Key::Up | Key::Down => {
            next = Cursor {
                row: Some(stepped(cursor.row, entries.len(), key == Key::Down)),
                open: false,
                sub: None,
            };
        }
        Key::Right | Key::Enter if !cursor.open && children > 0 => {
            next.open = true;
            next.sub = Some(0);
        }
        Key::Left => {
            next.open = false;
            next.sub = None;
        }
        Key::Right => {}
        Key::Enter => {
            return match (cursor.row, cursor.open) {
                (Some(row), false) => (cursor, Some((row, None))),
                (Some(row), true) => (cursor, cursor.sub.map(|sub| (row, Some(sub)))),
                (None, _) => (cursor, None),
            };
        }
    }
    (next, None)
}

pub(crate) fn key<T: Desk>(app: &mut T, key: Key, window: &mut Window, cx: &mut Context<T>) {
    let Some(open) = app.ui().menu.clone() else { return };
    let mut entries = entries_of(app.ui(), &open, cx);
    if entries.is_empty() {
        return;
    }
    let (cursor, chosen) = moved(&entries, app.ui().menu_cursor, key);
    app.ui_mut().menu_cursor = cursor;
    cx.notify();
    let Some((row, sub)) = chosen else { return };
    let mut entry = entries.swap_remove(row);
    if let Some(sub) = sub {
        if sub >= entry.children.len() {
            return;
        }
        entry = entry.children.swap_remove(sub);
    }
    if entry.note.is_some() {
        return;
    }
    match entry.action {
        Some(action) => app.dispatch(action, window, cx),
        None => {
            app.ui_mut().menu = None;
            if let Some(reason) = entry.disabled {
                app.ui_mut().notices.show(Notice::new(Tone::Info, reason, ""));
            }
        }
    }
}

const SEPARATOR: f32 = 9.;
const SUBMENU_WIDTH: f32 = 230.;
const MARKED_SUBMENU_WIDTH: f32 = 330.;

fn row<T: Desk>(
    entry: Entry,
    id: ElementId,
    highlighted: bool,
    parent: bool,
    here: Cursor,
    cx: &Context<T>,
) -> gpui::Stateful<Div> {
    if let Some(note) = entry.note {
        let (icon, ink) = if note.warn { ("alert", crate::theme::CONFLICT) } else { (entry.icon.unwrap_or("ok"), crate::theme::TEXT_3) };
        let item = kit::item(id, entry.title).icon(icon).ink(ink).highlighted(highlighted).tip(note.tip.map(SharedString::from));
        return item.inert().on_hover(cx.listener(move |app, hovered: &bool, w, cx| {
            if *hovered && app.ui().menu_cursor != here {
                app.dispatch(Action::MenuHover(here), w, cx);
            }
        }));
    }
    let mut item = kit::item(id, entry.title)
        .enabled(entry.action.is_some() || parent)
        .highlighted(highlighted)
        .tip(entry.disabled.map(SharedString::from));
    if let Some(icon) = entry.icon {
        item = item.icon(icon);
    }
    if let Some(shortcut) = entry.shortcut {
        item = item.meta(keys(shortcut));
    }
    if entry.danger {
        item = item.danger();
    }
    if entry.mark.is_some() {
        item = item.check(matches!(entry.mark, Some(Mark::Default)));
    }
    if let Some(checked) = entry.check {
        item = item.check(checked);
    }
    let (mark, name) = (entry.mark, entry.id);
    let drawn = if parent {
        let double = entry.action;
        item.chevron().inert().on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
            match &double {
                Some(action) if event.click_count() > 1 => app.dispatch(action.clone(), w, cx),
                _ => app.dispatch(Action::MenuHover(here), w, cx),
            }
        }))
    } else {
        match entry.action {
            Some(action) => item.on(action, cx),
            None => item.inert(),
        }
    };
    let drawn = drawn.on_hover(cx.listener(move |app, hovered: &bool, w, cx| {
        if *hovered && app.ui().menu_cursor != here {
            app.dispatch(Action::MenuHover(here), w, cx);
        }
    }));
    let Some(mark) = mark else { return drawn };
    let side = match mark {
        Mark::Default => div().child(kit::muted("Default").text_size(px(crate::theme::TEXT_TINY))),
        Mark::Offer(action) => div().child(kit::link(SharedString::from(format!("{name}-default")), "Set as default", action, cx)),
    };
    div()
        .id(SharedString::from(format!("{name}-marked")))
        .flex()
        .items_center()
        .child(div().flex_1().min_w_0().child(drawn))
        .child(side.flex_shrink_0().px_2())
}

pub(crate) fn surface<T: Desk>(entries: Vec<Entry>, width: f32, cursor: Cursor, cx: &Context<T>) -> Div {
    let groups: Vec<Group> = entries.iter().map(|entry| entry.group).collect();
    let mut menu = kit::float().w(px(width)).py_1();
    let mut top = 0.;
    let mut hanging = None;
    for (index, (mut entry, separated)) in entries.into_iter().zip(breaks(&groups)).enumerate() {
        if separated {
            menu = menu.child(kit::separator());
            top += SEPARATOR;
        }
        let highlighted = cursor.row == Some(index);
        let parent = !entry.children.is_empty();
        if highlighted && cursor.open && parent {
            let marked = entry.children.iter().any(|child| child.mark.is_some()) || entry.id == "drop.also";
            let rows: Vec<_> = entry
                .children
                .drain(..)
                .enumerate()
                .map(|(sub, child)| {
                    let here = Cursor { row: Some(index), open: true, sub: Some(sub) };
                    let id = ElementId::NamedInteger(child.id.into(), sub as u64);
                    row(child, id, cursor.sub == Some(sub), false, here, cx)
                })
                .collect();
            hanging = Some((top, rows, if marked { MARKED_SUBMENU_WIDTH } else { SUBMENU_WIDTH }));
        }
        let here = Cursor { row: Some(index), open: parent, sub: None };
        let id = ElementId::NamedInteger(entry.id.into(), index as u64);
        menu = menu.child(row(entry, id, highlighted, parent, here, cx));
        top += ROW;
    }
    div().flex().items_start().child(menu).when_some(hanging, |d, (top, rows, width)| {
        d.child(
            div()
                .id("submenu")
                .pl(px(2.))
                .pt(px(top))
                .child(kit::float().w(px(width)).py_1().children(rows)),
        )
    })
}

pub(crate) fn default_command(target: &Target) -> Option<&'static str> {
    match target {
        Target::Branch(_) => Some("branch.switch"),
        Target::RemoteBranch(_) => Some("branch.checkout-remote"),
        Target::Tag(_) => Some("tag.checkout"),
        Target::Worktree(_) => Some("worktree.open"),
        Target::Submodule(_) => Some("submodule.open"),
        Target::PullRequest { .. } => Some("pull.checkout"),
        _ => None,
    }
}

pub(crate) fn run_for<T: Desk>(app: &mut T, id: &str, target: Target, window: &mut Window, cx: &mut Context<T>) {
    if !run_on(app, id, &[target], window, cx) {
        app.ui_mut().notices.show(Notice::new(Tone::Info, "That command does not apply here", ""));
        cx.notify();
    }
}

fn run_on<T: Desk>(app: &mut T, id: &str, targets: &[Target], window: &mut Window, cx: &mut Context<T>) -> bool {
    let Some(command) = find(id) else { return false };
    for target in targets {
        match (command.when)(app.ui(), target, cx) {
            Availability::Hidden => continue,
            Availability::Disabled(reason) => {
                app.ui_mut().notices.show(Notice::new(Tone::Info, reason, ""));
                cx.notify();
            }
            Availability::Enabled => {
                if let Some(action) = (command.run)(app.ui(), target) {
                    app.dispatch(action, window, cx);
                }
            }
        }
        return true;
    }
    false
}

pub(crate) fn targets(ui: &Ui) -> Vec<Target> {
    let current = Target::current(ui);
    let mut targets = Vec::new();
    targets.extend(crate::lines::target(ui));
    if let Some((paths, side)) = crate::picked::files(ui) {
        targets.push(Target::Files { paths, side });
    }
    if let Target::File { side: Side::Commit(oid), .. } = &current {
        let commit = Target::Commit(oid.clone());
        targets.push(current);
        targets.push(commit);
    } else if let Target::File { side: Side::Stash(oid), .. } = &current {
        let stash = Target::Stash(oid.clone());
        targets.push(current);
        targets.push(stash);
    } else if current != Target::Repo {
        targets.push(current);
    }
    targets.push(Target::Repo);
    targets
}

pub(crate) fn not_about(ui: &Ui, id: &str) -> String {
    if ui.repo.is_empty() {
        return "Open a repository first".to_owned();
    }
    if let Target::File { path, side } = Target::current(ui) {
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
        let listed = |staged: bool| ui.files.iter().any(|f| f.staged == staged && f.path == path);
        match (id, side) {
            ("file.stage", Side::Staged) if listed(false) => {
                return format!("Select {name} under Unstaged to stage the rest of it");
            }
            ("file.stage", Side::Staged) => return format!("{name} is already staged"),
            ("file.unstage", Side::Unstaged) if listed(true) => {
                return format!("Select {name} under Staged to unstage it");
            }
            ("file.unstage", Side::Unstaged) => return format!("{name} has no staged changes"),
            ("file.pick-all", Side::Unstaged | Side::Staged) => {
                return format!("{name} is the only file in its list");
            }
            _ => {}
        }
    }
    match id.split_once('.').map(|(area, _)| area) {
        Some("file") => "Select a file in the list of changes first".to_owned(),
        Some("commit") => "Select a commit first".to_owned(),
        Some("branch") | Some("ref") => "Select a branch first".to_owned(),
        Some("stash") => "Select a stash first".to_owned(),
        Some("conflict") => "Open a conflicted file first".to_owned(),
        _ => "That command does not apply to the selection".to_owned(),
    }
}

pub(crate) fn invoke<T: Desk>(app: &mut T, id: &str, window: &mut Window, cx: &mut Context<T>) {
    let targets = targets(app.ui());
    if !run_on(app, id, &targets, window, cx) && find(id).is_some() {
        let reason = not_about(app.ui(), id);
        app.ui_mut().notices.show(Notice::new(Tone::Info, reason, ""));
        cx.notify();
    }
}

pub(crate) fn context_for(shortcut: &str) -> &'static str {
    if shortcut.contains("cmd-") || shortcut.contains("ctrl-") {
        "Desk"
    } else {
        "Desk && !CommitField"
    }
}

pub(crate) fn bindings() -> Vec<KeyBinding> {
    let unscoped = all().filter(|command| command.scope.is_none());
    let scoped = all().filter(|command| command.scope.is_some());
    unscoped
        .chain(scoped)
        .filter_map(|command| {
            let shortcut = command.shortcut?;
            let context = match command.scope {
                Some(scope) => format!("Desk && {scope}"),
                None => context_for(shortcut).to_owned(),
            };
            Some(KeyBinding::new(shortcut, Run { id: command.id }, Some(context.as_str())))
        })
        .collect()
}

const MINOR_WORDS: &[&str] =
    &["a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "of", "on", "or", "the", "to", "with"];

pub(crate) fn title_case(title: &str) -> String {
    title
        .split(' ')
        .enumerate()
        .map(|(i, word)| {
            if i > 0 && MINOR_WORDS.contains(&word) {
                return word.to_owned();
            }
            let mut done = false;
            word.chars()
                .map(|c| {
                    if !done && c.is_alphabetic() {
                        done = true;
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c
                    }
                })
                .collect()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

pub(crate) fn in_bar(bar: Bar) -> Vec<&'static Command> {
    let mut commands: Vec<_> = all().filter(|command| command.bar == Some(bar)).collect();
    commands.sort_by_key(|command| command.group);
    commands
}

pub(crate) fn bar_items(ui: &Ui, bar: Bar) -> Vec<gpui::MenuItem> {
    let commands = in_bar(bar);
    let groups: Vec<Group> = commands.iter().map(|command| command.group).collect();
    let mut items = Vec::new();
    for (command, separated) in commands.into_iter().zip(breaks(&groups)) {
        if separated {
            items.push(gpui::MenuItem::separator());
        }
        let title = title_case(&(command.title)(ui, &Target::Repo));
        items.push(if keyed_in_menu(command) || command.shortcut.is_none() {
            gpui::MenuItem::action(title, Run { id: command.id })
        } else {
            gpui::MenuItem::action(title, Choose { id: command.id })
        });
    }
    items
}

pub(crate) fn bar_index(bar: Bar, id: &str) -> Option<usize> {
    let commands = in_bar(bar);
    let groups: Vec<Group> = commands.iter().map(|command| command.group).collect();
    let mut index = 0;
    for (command, separated) in commands.into_iter().zip(breaks(&groups)) {
        if separated {
            index += 1;
        }
        if command.id == id {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub(crate) fn bar_titles(ui: &Ui) -> Vec<String> {
    all()
        .filter(|command| command.bar.is_some())
        .map(|command| (command.title)(ui, &Target::Repo))
        .collect()
}

pub(crate) fn activate<T: Desk>(app: &mut T, target: Target, window: &mut Window, cx: &mut Context<T>) {
    if let Some(id) = default_command(&target) {
        run_on(app, id, &[target], window, cx);
    }
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastkraken_desk, no_json)]
pub(crate) struct Run {
    pub id: &'static str,
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastkraken_desk, no_json)]
pub(crate) struct Choose {
    pub id: &'static str,
}

pub(crate) fn keyed_in_menu(command: &Command) -> bool {
    command
        .shortcut
        .is_some_and(|shortcut| context_for(shortcut) == "Desk" && !shortcut.ends_with("-enter"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_shortcuts_are_unique() {
        let mut ids = HashSet::new();
        let mut shortcuts = HashSet::new();
        for command in all() {
            assert!(ids.insert(command.id), "two commands are called {}", command.id);
            if let Some(shortcut) = command.shortcut {
                assert!(shortcuts.insert((shortcut, command.scope)), "two commands use {shortcut}");
            }
        }
        assert!(ids.len() >= 30, "only {} commands are registered", ids.len());
    }

    #[test]
    fn ids_are_namespaced_words() {
        for command in all() {
            let (area, name) = command.id.split_once('.').unwrap_or_else(|| panic!("{}", command.id));
            for part in [area, name] {
                assert!(
                    !part.is_empty()
                        && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                    "{} is not area.name in lower case",
                    command.id
                );
            }
        }
    }

    #[test]
    fn hand_picked_menus_name_real_commands() {
        for id in view::REPO_MENU.iter().chain(crate::publish::REMOTES_MENU).chain(view::CHANGES_MENU) {
            assert!(find(id).is_some(), "{id} is not in the command table");
        }
    }

    #[test]
    fn shortcuts_read_as_a_mac_menu_shows_them() {
        assert_eq!(keys("cmd-o"), "⌘O");
        assert_eq!(keys("cmd-enter"), "⌘↩");
        assert_eq!(keys("cmd-shift-l"), "⇧⌘L");
        assert_eq!(keys("shift-cmd-k"), "⇧⌘K");
        assert_eq!(keys("alt-o"), "⌥O");
        assert_eq!(keys("cmd-/"), "⌘/");
        assert_eq!(keys("s"), "S");
        assert_eq!(keys("ctrl-cmd-f"), "⌃⌘F");
        assert_eq!(keys("alt-cmd-h"), "⌥⌘H");
        assert_eq!(keys("cmd-shift-enter"), "⇧⌘↩");
        assert_eq!(keys("pagedown"), "⇟");
        assert_eq!(keys("pageup"), "⇞");
        assert_eq!(keys("home"), "↖");
        assert_eq!(keys("end"), "↘");
        assert_eq!(keys("cmd-down"), "⌘↓");
        assert_eq!(keys("shift-up"), "⇧↑");
    }

    #[test]
    fn menu_titles_are_in_title_case() {
        assert_eq!(title_case("Fetch all remotes"), "Fetch All Remotes");
        assert_eq!(title_case("Open repository…"), "Open Repository…");
        assert_eq!(title_case("Reveal in Finder"), "Reveal in Finder");
        assert_eq!(title_case("Stage all and commit"), "Stage All and Commit");
        assert_eq!(title_case("Pull (fast-forward only)"), "Pull (Fast-forward Only)");
        assert_eq!(title_case("Show diffs side by side"), "Show Diffs Side by Side");
        assert_eq!(title_case("About FastKraken"), "About FastKraken");
    }

    #[test]
    fn the_menu_bar_lists_its_commands_by_group() {
        let ids = |bar| in_bar(bar).into_iter().map(|command| command.id).collect::<Vec<_>>();
        assert_eq!(ids(Bar::App), ["app.about", "app.preferences", "app.install-command"]);
        assert_eq!(
            ids(Bar::File),
            ["tab.new", "tab.close", "repo.clone", "repo.init", "repo.open", "repo.close", "repo.refresh", "repo.apply-patch", "repo.reveal", "repo.terminal", "repo.editor"]
        );
        assert_eq!(bar_index(Bar::File, "repo.clone"), Some(3));
        assert_eq!(bar_index(Bar::File, "repo.open"), Some(5));
        assert_eq!(bar_index(Bar::File, "repo.apply-patch"), Some(9));
        assert_eq!(bar_index(Bar::File, "repo.reveal"), Some(11));
        assert_eq!(ids(Bar::Window), ["tab.next", "tab.previous"]);
        assert_eq!(bar_index(Bar::File, "repo.labels"), None);
        assert_eq!(ids(Bar::Edit), ["repo.find", "view.find-next", "view.find-previous"]);
        assert_eq!(ids(Bar::View), ["view.sidebar", "view.panel", "view.diff-mode", "view.activity", "repo.labels", "graph.only-current", "graph.author", "view.filter-sidebar"]);
        assert_eq!(
            ids(Bar::Repository),
            [
                "repo.fetch-all",
                "repo.pull",
                "repo.push",
                "repo.stage-all",
                "repo.unstage-all",
                "repo.commit",
                "repo.commit-all",
                "repo.discard-all",
                "repo.stash",
                "repo.stash-options",
                "repo.pop-stash",
            ]
        );
        assert_eq!(ids(Bar::Branch), ["repo.new-branch", "repo.switch-branch", "branch.rename"]);
        assert_eq!(ids(Bar::Help), ["app.palette", "app.shortcuts"]);
    }

    #[test]
    fn plain_keys_never_reach_a_text_field_or_the_menu_bar() {
        assert_eq!(context_for("cmd-shift-l"), "Desk");
        assert_eq!(context_for("s"), "Desk && !CommitField");
        assert_eq!(context_for("alt-o"), "Desk && !CommitField");
        for id in ["repo.reveal", "repo.terminal"] {
            assert!(!keyed_in_menu(find(id).unwrap()), "{id}");
        }
        for id in ["repo.commit", "repo.commit-all"] {
            assert!(!keyed_in_menu(find(id).unwrap()), "{id}");
        }
        for id in ["repo.fetch-all", "repo.push", "view.sidebar", "app.shortcuts"] {
            assert!(keyed_in_menu(find(id).unwrap()), "{id}");
        }
        assert!(find("file.stage").unwrap().bar.is_none());
        assert_eq!(bindings().len(), all().filter(|command| command.shortcut.is_some()).count());
    }

    #[test]
    fn the_plan_s_shortcuts_run_the_commands_it_names() {
        for (shortcut, id) in [
            ("cmd-b", "repo.new-branch"),
            ("cmd-l", "repo.fetch-all"),
            ("cmd-shift-l", "repo.pull"),
            ("cmd-shift-k", "repo.push"),
            ("cmd-shift-m", "repo.focus-summary"),
            ("s", "file.stage"),
            ("u", "file.unstage"),
            ("cmd-shift-s", "repo.stage-all"),
            ("cmd-shift-u", "repo.unstage-all"),
            ("cmd-enter", "repo.commit"),
            ("cmd-shift-enter", "repo.commit-all"),
            ("cmd-j", "view.sidebar"),
            ("cmd-k", "view.panel"),
            ("cmd-w", "tab.close"),
            ("cmd-t", "tab.new"),
            ("ctrl-tab", "tab.next"),
            ("ctrl-shift-tab", "tab.previous"),
            ("cmd-shift-y", "view.activity"),
            ("cmd-/", "app.shortcuts"),
            ("cmd-p", "app.palette"),
            ("cmd-shift-o", "repo.open-recent"),
            ("alt-o", "repo.reveal"),
            ("alt-t", "repo.terminal"),
            ("cmd-shift-e", "repo.editor"),
        ] {
            assert_eq!(find(id).and_then(|command| command.shortcut), Some(shortcut), "{id}");
        }
    }

    #[test]
    fn separators_fall_between_groups() {
        use Group::*;
        assert_eq!(breaks(&[]), Vec::<bool>::new());
        assert_eq!(
            breaks(&[Create, Create, Reverse, Reverse, Copy]),
            [false, false, true, false, true]
        );
    }

    fn row(id: &'static str, children: Vec<Entry>) -> Entry {
        let mut entry = submenu(id, id.into(), "branch", Group::Switch, children);
        entry.icon = None;
        entry
    }

    #[test]
    fn keys_move_through_a_menu_and_into_its_submenu() {
        let entries = [
            row("first", Vec::new()),
            row("parent", vec![row("one", Vec::new()), row("two", Vec::new())]),
            row("last", Vec::new()),
        ];
        let at = |row, open, sub| Cursor { row, open, sub };
        let press = |cursor, key| moved(&entries, cursor, key);
        assert_eq!(press(Cursor::default(), Key::Down), (at(Some(0), false, None), None));
        assert_eq!(press(Cursor::default(), Key::Up), (at(Some(2), false, None), None));
        assert_eq!(press(at(Some(2), false, None), Key::Down), (at(Some(0), false, None), None));
        assert_eq!(press(at(Some(0), false, None), Key::Right), (at(Some(0), false, None), None));
        assert_eq!(press(at(Some(0), false, None), Key::Enter).1, Some((0, None)));
        let parent = at(Some(1), false, None);
        assert_eq!(press(parent, Key::Right), (at(Some(1), true, Some(0)), None));
        assert_eq!(press(parent, Key::Enter), (at(Some(1), true, Some(0)), None));
        let inside = at(Some(1), true, Some(0));
        assert_eq!(press(inside, Key::Down), (at(Some(1), true, Some(1)), None));
        assert_eq!(press(inside, Key::Up), (at(Some(1), true, Some(1)), None));
        assert_eq!(press(inside, Key::Left), (parent, None));
        assert_eq!(press(at(Some(1), true, Some(1)), Key::Enter).1, Some((1, Some(1))));
        assert_eq!(press(Cursor::default(), Key::Enter).1, None);
        let stale = at(Some(5), true, Some(1));
        assert_eq!(press(stale, Key::Enter), (Cursor::default(), None));
        assert_eq!(press(stale, Key::Down), (at(Some(0), false, None), None));
    }

    #[test]
    fn branches_and_tags_have_a_default_command_and_commits_do_not() {
        assert_eq!(default_command(&Target::Branch("x".into())), Some("branch.switch"));
        assert!(find("branch.switch").is_some());
        assert_eq!(default_command(&Target::RemoteBranch("origin/x".into())), Some("branch.checkout-remote"));
        assert!(find("branch.checkout-remote").is_some());
        assert_eq!(default_command(&Target::Tag("v1".into())), Some("tag.checkout"));
        assert!(find("tag.checkout").is_some());
        for target in [Target::Commit("0".repeat(40)), Target::Repo] {
            assert_eq!(default_command(&target), None, "{target:?}");
        }
    }

    #[test]
    fn sidebar_refs_become_targets_of_their_kind() {
        use crate::refs::Kind;
        let name = || OsString::from("x");
        assert_eq!(Target::of_ref(Kind::Branch, name()), Target::Branch(name()));
        assert_eq!(Target::of_ref(Kind::Remote, name()), Target::RemoteBranch(name()));
        assert_eq!(Target::of_ref(Kind::Tag, name()), Target::Tag(name()));
    }
}
