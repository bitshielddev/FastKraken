use crate::{
    Tab,
    commands::{self, Availability, Command, Group, Side, Target},
    kit,
    settings::Settings,
    theme::*,
    view::{self, CommitInput, Desk, Menu, Selection, Ui, commit_input::FieldCommand},
};
use gpui::{Context, Div, Entity, FontWeight, Focusable, Stateful, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt,
    ops::{Deref, Range},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const FILTER_FROM: usize = 15;
const INDENT: f32 = 12.;
const CHEVRON: f32 = 16.;
const ROW_START: f32 = 10.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    #[default]
    List,
    Tree,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Sort {
    #[default]
    Path,
    Name,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Panel {
    Working,
    Commit,
    Stash,
    Compare,
}

impl Panel {
    fn slot(self) -> usize {
        self as usize
    }

    pub(crate) fn lists(self) -> &'static [Which] {
        match self {
            Self::Working => &[Which::Unstaged, Which::Staged],
            Self::Commit => &[Which::Commit],
            Self::Stash => &[Which::Stash],
            Self::Compare => &[Which::Compare],
        }
    }

    pub(crate) fn of(ui: &Ui) -> Option<Self> {
        match &ui.selection {
            Some(Selection::Changes | Selection::File { .. }) => Some(Self::Working),
            Some(Selection::Commit(_)) => Some(Self::Commit),
            Some(Selection::Stash(_)) => Some(Self::Stash),
            Some(Selection::Compare { .. }) => Some(Self::Compare),
            Some(Selection::Commits) | None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Which {
    Unstaged,
    Staged,
    Commit,
    Stash,
    Compare,
}

impl Which {
    fn slot(self) -> usize {
        self as usize
    }

    pub(crate) fn panel(self) -> Panel {
        match self {
            Self::Unstaged | Self::Staged => Panel::Working,
            Self::Commit => Panel::Commit,
            Self::Stash => Panel::Stash,
            Self::Compare => Panel::Compare,
        }
    }

    pub(crate) fn of_side(side: &Side) -> Self {
        match side {
            Side::Unstaged => Self::Unstaged,
            Side::Staged => Self::Staged,
            Side::Commit(_) => Self::Commit,
            Side::Stash(_) => Self::Stash,
            Side::Compare(_) => Self::Compare,
        }
    }

    pub(crate) fn side(self, ui: &Ui) -> Option<Side> {
        match (self, &ui.selection) {
            (Self::Unstaged, _) => Some(Side::Unstaged),
            (Self::Staged, _) => Some(Side::Staged),
            (Self::Commit, Some(Selection::Commit(oid))) => Some(Side::Commit(oid.clone())),
            (Self::Stash, Some(Selection::Stash(oid))) => Some(Side::Stash(oid.clone())),
            (Self::Compare, Some(Selection::Compare { to, .. })) => Some(Side::Compare(to.clone())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    File { index: usize, depth: usize },
    Folder { path: PathBuf, name: String, depth: usize, count: usize, folded: bool },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Listed<'a> {
    pub(crate) path: &'a Path,
    pub(crate) status: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape<'a> {
    pub(crate) mode: Mode,
    pub(crate) sort: Sort,
    pub(crate) filter: &'a str,
    pub(crate) folded: &'a BTreeSet<PathBuf>,
}

fn needle(filter: &str) -> String {
    filter.trim().to_lowercase()
}

fn matches(path: &Path, needle: &str) -> bool {
    needle.is_empty() || path.to_string_lossy().to_lowercase().contains(needle)
}

fn status_rank(status: &str) -> u8 {
    match status {
        "Conflicted" => 0,
        "Modified" => 1,
        "Added" => 2,
        "Untracked" => 3,
        "Renamed" => 4,
        "Copied" => 5,
        "Type changed" => 6,
        "Deleted" => 7,
        _ => 8,
    }
}

fn name_of(path: &Path) -> &[u8] {
    path.file_name().unwrap_or(path.as_os_str()).as_bytes()
}

fn folded_name(path: &Path) -> String {
    String::from_utf8_lossy(name_of(path)).to_lowercase()
}

fn order(files: &[Listed], kept: &mut [usize], sort: Sort, tree: bool) {
    let bytes = |index: usize| files[index].path.as_os_str().as_bytes();
    match (sort, tree) {
        (Sort::Path, false) => kept.sort_by(|a, b| bytes(*a).cmp(bytes(*b))),
        (Sort::Path, true) => kept.sort_by(|a, b| name_of(files[*a].path).cmp(name_of(files[*b].path))),
        (Sort::Name, _) => kept.sort_by_cached_key(|index| (folded_name(files[*index].path), bytes(*index).to_vec())),
        (Sort::Status, _) => kept.sort_by_cached_key(|index| (status_rank(files[*index].status), bytes(*index).to_vec())),
    }
}

#[derive(Default)]
struct Node {
    folders: BTreeMap<OsString, Node>,
    files: Vec<usize>,
    total: usize,
}

impl Node {
    fn insert(&mut self, path: &Path, index: usize) {
        let mut node = self;
        node.total += 1;
        if let Some(parent) = path.parent() {
            for part in parent.iter() {
                node = node.folders.entry(part.to_owned()).or_default();
                node.total += 1;
            }
        }
        node.files.push(index);
    }

    fn emit(&self, files: &[Listed], shape: &Shape, at: &Path, depth: usize, rows: &mut Vec<Row>) {
        let mut folders: Vec<(&OsString, &Node)> = self.folders.iter().collect();
        folders.sort_by_cached_key(|(name, _)| (name.to_string_lossy().to_lowercase(), name.as_bytes().to_vec()));
        for (first, mut node) in folders {
            let mut path = at.join(first);
            let mut name = first.to_string_lossy().into_owned();
            while node.files.is_empty() && node.folders.len() == 1 {
                let Some((next, child)) = node.folders.iter().next() else { break };
                path.push(next);
                name.push('/');
                name.push_str(&next.to_string_lossy());
                node = child;
            }
            let folded = shape.folded.contains(&path);
            rows.push(Row::Folder { path: path.clone(), name, depth, count: node.total, folded });
            if !folded {
                node.emit(files, shape, &path, depth + 1, rows);
            }
        }
        let mut own = self.files.clone();
        order(files, &mut own, shape.sort, true);
        rows.extend(own.into_iter().map(|index| Row::File { index, depth }));
    }
}

fn arrange_counted(files: &[Listed], shape: &Shape) -> (Vec<Row>, usize) {
    let needle = needle(shape.filter);
    let mut kept: Vec<usize> = (0..files.len()).filter(|index| matches(files[*index].path, &needle)).collect();
    let matching = kept.len();
    let rows = match shape.mode {
        Mode::List => {
            order(files, &mut kept, shape.sort, false);
            kept.into_iter().map(|index| Row::File { index, depth: 0 }).collect()
        }
        Mode::Tree => {
            let mut root = Node::default();
            for index in kept {
                root.insert(files[index].path, index);
            }
            let mut rows = Vec::new();
            root.emit(files, shape, Path::new(""), 0, &mut rows);
            rows
        }
    };
    (rows, matching)
}

pub(crate) fn arrange(files: &[Listed], shape: &Shape) -> Vec<Row> {
    arrange_counted(files, shape).0
}

pub(crate) fn folders(files: &[Listed], filter: &str) -> Vec<PathBuf> {
    let none = BTreeSet::new();
    let shape = Shape { mode: Mode::Tree, sort: Sort::Path, filter, folded: &none };
    arrange(files, &shape)
        .into_iter()
        .filter_map(|row| match row {
            Row::Folder { path, .. } => Some(path),
            Row::File { .. } => None,
        })
        .collect()
}

pub(crate) fn within(files: &[Listed], filter: &str, folder: &Path) -> Vec<PathBuf> {
    let needle = needle(filter);
    let mut kept: Vec<usize> = (0..files.len())
        .filter(|index| files[*index].path.starts_with(folder) && files[*index].path != folder)
        .filter(|index| matches(files[*index].path, &needle))
        .collect();
    order(files, &mut kept, Sort::Path, false);
    kept.into_iter().map(|index| files[index].path.to_path_buf()).collect()
}

static STAMPS: AtomicU64 = AtomicU64::new(1);

pub struct Files<T> {
    items: Vec<T>,
    stamp: u64,
}

impl<T> Files<T> {
    pub(crate) fn set(&mut self, items: Vec<T>) {
        self.items = items;
        self.stamp = STAMPS.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn clear(&mut self) {
        self.set(Vec::new());
    }

    pub(crate) fn stamp(&self) -> u64 {
        self.stamp
    }
}

impl<T> Default for Files<T> {
    fn default() -> Self {
        Self { items: Vec::new(), stamp: STAMPS.fetch_add(1, Ordering::Relaxed) }
    }
}

impl<T> Deref for Files<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T: fmt::Debug> fmt::Debug for Files<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.items.fmt(f)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Key {
    files: u64,
    folds: u64,
    mode: Mode,
    sort: Sort,
    needle: String,
}

impl Key {
    fn new(files: u64, folds: u64, shape: &Shape) -> Self {
        Self { files, folds, mode: shape.mode, sort: shape.sort, needle: needle(shape.filter) }
    }
}

struct Arranged {
    key: Key,
    rows: Rc<Vec<Row>>,
    matching: usize,
}

#[derive(Default)]
struct Cache(RefCell<[Option<Arranged>; 5]>);

impl Cache {
    fn get<'a>(&self, which: Which, key: Key, shape: &Shape, files: impl FnOnce() -> Vec<Listed<'a>>) -> (Rc<Vec<Row>>, usize) {
        let mut cache = self.0.borrow_mut();
        let slot = &mut cache[which.slot()];
        if let Some(kept) = slot.as_ref().filter(|kept| kept.key == key) {
            return (kept.rows.clone(), kept.matching);
        }
        let (rows, matching) = arrange_counted(&files(), shape);
        let rows = Rc::new(rows);
        *slot = Some(Arranged { key, rows: rows.clone(), matching });
        (rows, matching)
    }
}

pub(crate) struct State {
    pub(crate) mode: Mode,
    pub(crate) sort: Sort,
    inputs: Vec<Entity<CommitInput>>,
    typed: [String; 4],
    asked: [bool; 4],
    folded: [BTreeSet<PathBuf>; 5],
    folds: [u64; 5],
    cache: Cache,
}

impl State {
    pub(crate) fn new(settings: &Settings, cx: &mut Context<Tab>) -> Self {
        let inputs = [Panel::Working, Panel::Commit, Panel::Stash, Panel::Compare]
            .into_iter()
            .map(|panel| {
                let input = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter files"));
                cx.observe(&input, move |tab, input, cx| {
                    let text = input.read(cx).text().to_owned();
                    if tab.ui.file_list.typed[panel.slot()] != text {
                        tab.ui.file_list.typed[panel.slot()] = text;
                        crate::picked::refresh(&mut tab.ui);
                    }
                    cx.notify();
                })
                .detach();
                input
            })
            .collect();
        Self {
            mode: settings.file_list_mode,
            sort: settings.file_list_sort,
            inputs,
            typed: Default::default(),
            asked: [false; 4],
            folded: Default::default(),
            folds: [0; 5],
            cache: Cache::default(),
        }
    }

    pub(crate) fn input(&self, panel: Panel) -> &Entity<CommitInput> {
        &self.inputs[panel.slot()]
    }

    pub(crate) fn filter(&self, panel: Panel) -> &str {
        &self.typed[panel.slot()]
    }

    pub(crate) fn filtering(&self, panel: Panel) -> bool {
        !needle(self.filter(panel)).is_empty()
    }

    pub(crate) fn folded(&self, which: Which) -> &BTreeSet<PathBuf> {
        &self.folded[which.slot()]
    }

    fn folding(&mut self, which: Which) -> &mut BTreeSet<PathBuf> {
        self.folds[which.slot()] += 1;
        &mut self.folded[which.slot()]
    }

    fn shape(&self, which: Which) -> Shape<'_> {
        Shape { mode: self.mode, sort: self.sort, filter: self.filter(which.panel()), folded: self.folded(which) }
    }
}

pub(crate) fn working_range(ui: &Ui, staged: bool) -> Range<usize> {
    let (unstaged, staged_range) = ui.file_ranges();
    if staged { staged_range } else { unstaged }
}

fn changed(files: &[crate::repository::ChangedFile]) -> Vec<Listed<'_>> {
    files.iter().map(|file| Listed { path: &file.path, status: file.status }).collect()
}

pub(crate) fn source(ui: &Ui, which: Which) -> Vec<Listed<'_>> {
    match which {
        Which::Unstaged | Which::Staged => ui.files[working_range(ui, which == Which::Staged)]
            .iter()
            .map(|file| Listed { path: &file.path, status: file.status })
            .collect(),
        Which::Commit => changed(&ui.inspection.files),
        Which::Stash => changed(&ui.stash.files),
        Which::Compare => changed(&ui.compare.files),
    }
}

fn stamp(ui: &Ui, which: Which) -> u64 {
    match which {
        Which::Unstaged | Which::Staged => ui.files.stamp(),
        Which::Commit => ui.inspection.files.stamp(),
        Which::Stash => ui.stash.files.stamp(),
        Which::Compare => ui.compare.files.stamp(),
    }
}

fn listed_len(ui: &Ui, which: Which) -> usize {
    match which {
        Which::Unstaged | Which::Staged => working_range(ui, which == Which::Staged).len(),
        Which::Commit => ui.inspection.files.len(),
        Which::Stash => ui.stash.files.len(),
        Which::Compare => ui.compare.files.len(),
    }
}

fn arranged(ui: &Ui, which: Which) -> (Rc<Vec<Row>>, usize) {
    let state = &ui.file_list;
    let shape = state.shape(which);
    let key = Key::new(stamp(ui, which), state.folds[which.slot()], &shape);
    state.cache.get(which, key, &shape, || source(ui, which))
}

pub(crate) fn rows(ui: &Ui, which: Which) -> Rc<Vec<Row>> {
    arranged(ui, which).0
}

pub(crate) fn shown(ui: &Ui, which: Which) -> Vec<usize> {
    rows(ui, which)
        .iter()
        .filter_map(|row| match row {
            Row::File { index, .. } => Some(*index),
            Row::Folder { .. } => None,
        })
        .collect()
}

pub(crate) fn row_of(ui: &Ui, which: Which, index: usize) -> Option<usize> {
    rows(ui, which).iter().position(|row| matches!(row, Row::File { index: at, .. } if *at == index))
}

pub(crate) fn shown_paths(ui: &Ui, which: Which) -> Vec<PathBuf> {
    let files = source(ui, which);
    shown(ui, which).into_iter().map(|index| files[index].path.to_path_buf()).collect()
}

pub(crate) fn working_order(ui: &Ui) -> Vec<(PathBuf, bool, usize)> {
    let unstaged = rows(ui, Which::Unstaged);
    let staged = rows(ui, Which::Staged);
    let mut order = Vec::new();
    for (staged_side, listed, first) in [(false, &unstaged, 1), (true, &staged, unstaged.len() + 2)] {
        let start = working_range(ui, staged_side).start;
        for (at, row) in listed.iter().enumerate() {
            if let Row::File { index, .. } = row {
                order.push((ui.files[start + index].path.clone(), staged_side, first + at));
            }
        }
    }
    order
}

pub(crate) fn working_row(ui: &Ui, staged: bool, path: &Path) -> Option<usize> {
    working_order(ui).into_iter().find(|(at, side, _)| *side == staged && at == path).map(|(_, _, row)| row)
}

pub(crate) fn under(ui: &Ui, which: Which, folder: &Path) -> Vec<PathBuf> {
    within(&source(ui, which), ui.file_list.filter(which.panel()), folder)
}

pub(crate) fn counted(ui: &Ui, which: Which) -> (usize, usize) {
    (arranged(ui, which).1, listed_len(ui, which))
}

pub(crate) fn total(ui: &Ui, panel: Panel) -> usize {
    panel.lists().iter().map(|which| listed_len(ui, *which)).sum()
}

pub(crate) fn field_shown(ui: &Ui, panel: Panel) -> bool {
    ui.file_list.asked[panel.slot()] || ui.file_list.filtering(panel) || total(ui, panel) > FILTER_FROM
}

pub(crate) fn here(ui: &Ui) -> Option<Panel> {
    if ui.panel_hidden || ui.navigate.zone(ui) != crate::navigate::Zone::Files {
        return None;
    }
    Panel::of(ui).filter(|panel| total(ui, *panel) > 0)
}

pub(crate) fn focused(ui: &Ui, window: &Window, cx: &gpui::App) -> Option<Panel> {
    [Panel::Working, Panel::Commit, Panel::Stash, Panel::Compare]
        .into_iter()
        .find(|panel| ui.file_list.input(*panel).focus_handle(cx).is_focused(window))
}

pub(crate) fn indent(mode: Mode, depth: usize) -> f32 {
    match mode {
        Mode::List => ROW_START,
        Mode::Tree => ROW_START + depth as f32 * INDENT + CHEVRON,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Stage,
    Unstage,
    Discard,
    Stash,
    Ignore,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Show(Mode),
    Sort(Sort),
    Fold { which: Which, path: PathBuf },
    ExpandAll(Panel),
    CollapseAll(Panel),
    Filter(Panel),
    CloseFilter(Panel),
    Folder { which: Which, path: PathBuf, verb: Verb },
}

fn refold(ui: &mut Ui) {
    crate::picked::refresh(ui);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Show(mode) => {
            if tab.ui.file_list.mode != mode {
                tab.ui.file_list.mode = mode;
                Settings::update(cx, |settings| settings.file_list_mode = mode);
                refold(&mut tab.ui);
            }
        }
        Action::Sort(sort) => {
            if tab.ui.file_list.sort != sort {
                tab.ui.file_list.sort = sort;
                Settings::update(cx, |settings| settings.file_list_sort = sort);
            }
        }
        Action::Fold { which, path } => {
            let folded = tab.ui.file_list.folding(which);
            if !folded.remove(&path) {
                folded.insert(path);
            }
            refold(&mut tab.ui);
        }
        Action::ExpandAll(panel) => {
            for which in panel.lists() {
                tab.ui.file_list.folding(*which).clear();
            }
        }
        Action::CollapseAll(panel) => {
            for which in panel.lists() {
                let all: BTreeSet<PathBuf> = folders(&source(&tab.ui, *which), tab.ui.file_list.filter(panel)).into_iter().collect();
                *tab.ui.file_list.folding(*which) = all;
            }
            refold(&mut tab.ui);
        }
        Action::Filter(panel) => {
            tab.ui.file_list.asked[panel.slot()] = true;
            let input = tab.ui.file_list.input(panel).clone();
            window.focus(&input.focus_handle(cx));
            input.update(cx, |input, cx| input.choose(FieldCommand::SelectAll, window, cx));
        }
        Action::CloseFilter(panel) => {
            tab.ui.file_list.asked[panel.slot()] = false;
            let input = tab.ui.file_list.input(panel).clone();
            input.update(cx, |input, cx| input.clear(cx));
            view::focus(&tab.ui, window);
        }
        Action::Folder { which, path, verb } => {
            let paths = under(&tab.ui, which, &path);
            if paths.is_empty() {
                return;
            }
            return tab.dispatch(folder_call(&path, paths, verb), window, cx);
        }
    }
    cx.notify();
}

fn folder_call(folder: &Path, mut paths: Vec<PathBuf>, verb: Verb) -> view::Action {
    match verb {
        Verb::Stage => view::Action::Picked(crate::picked::Action::Stage(paths)),
        Verb::Unstage => view::Action::Picked(crate::picked::Action::Unstage(paths)),
        Verb::Discard if paths.len() == 1 => view::Action::AskDiscard(view::Discard::File(paths.remove(0))),
        Verb::Discard => view::Action::Picked(crate::picked::Action::AskDiscard(paths)),
        Verb::Stash => view::Action::Stash(crate::stash::Action::SaveFiles(paths)),
        Verb::Ignore => view::Action::Ignore(crate::ignore::Action::Ignore {
            paths,
            rule: crate::ignore::Rule::Enclosing(folder.components().count()),
        }),
    }
}

fn panel_of(ui: &Ui, target: &Target) -> Option<Panel> {
    match target {
        Target::FileList(panel) => Some(*panel),
        Target::Repo if !ui.repo.is_empty() => Panel::of(ui),
        _ => None,
    }
}

fn listed_here(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
    if panel_of(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden }
}

fn tree_here(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
    match panel_of(ui, target) {
        Some(_) if ui.file_list.mode == Mode::Tree => Availability::Enabled,
        _ => Availability::Hidden,
    }
}

fn panel_action(ui: &Ui, target: &Target, make: fn(Panel) -> Action) -> Option<view::Action> {
    panel_of(ui, target).map(|panel| view::Action::FileList(make(panel)))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "files.show-list",
        Group::View,
        |_, _| "Show files as a list".into(),
        listed_here,
        |_, _| Some(view::Action::FileList(Action::Show(Mode::List))),
    )
    .tick(|ui, _| ui.file_list.mode == Mode::List)
    .words(&["flat", "paths", "file list"]),
    Command::new(
        "files.show-tree",
        Group::View,
        |_, _| "Show files as a tree".into(),
        listed_here,
        |_, _| Some(view::Action::FileList(Action::Show(Mode::Tree))),
    )
    .tick(|ui, _| ui.file_list.mode == Mode::Tree)
    .words(&["folders", "nested", "file tree", "directory"]),
    Command::new(
        "files.sort-path",
        Group::View,
        |_, _| "Sort files by path".into(),
        listed_here,
        |_, _| Some(view::Action::FileList(Action::Sort(Sort::Path))),
    )
    .tick(|ui, _| ui.file_list.sort == Sort::Path)
    .words(&["order"]),
    Command::new(
        "files.sort-name",
        Group::View,
        |_, _| "Sort files by name".into(),
        listed_here,
        |_, _| Some(view::Action::FileList(Action::Sort(Sort::Name))),
    )
    .tick(|ui, _| ui.file_list.sort == Sort::Name)
    .words(&["order", "filename"]),
    Command::new(
        "files.sort-status",
        Group::View,
        |_, _| "Sort files by status".into(),
        listed_here,
        |_, _| Some(view::Action::FileList(Action::Sort(Sort::Status))),
    )
    .tick(|ui, _| ui.file_list.sort == Sort::Status)
    .words(&["order", "change type"]),
    Command::new(
        "files.expand-all",
        Group::View,
        |_, _| "Expand all folders".into(),
        tree_here,
        |ui, target| panel_action(ui, target, Action::ExpandAll),
    )
    .words(&["unfold", "open folders"]),
    Command::new(
        "files.collapse-all",
        Group::View,
        |_, _| "Collapse all folders".into(),
        tree_here,
        |ui, target| panel_action(ui, target, Action::CollapseAll),
    )
    .words(&["fold", "close folders"]),
    Command::new(
        "files.filter",
        Group::View,
        |_, _| "Filter files…".into(),
        listed_here,
        |ui, target| panel_action(ui, target, Action::Filter),
    )
    .icon("funnel")
    .words(&["find file", "search files", "narrow"]),
];

fn folder<'a>(ui: &Ui, target: &'a Target) -> Option<(Which, &'a PathBuf)> {
    match target {
        Target::Folder { path, side } => {
            let which = Which::of_side(side);
            (which.side(ui).as_ref() == Some(side)).then_some((which, path))
        }
        _ => None,
    }
}

fn index_edit(ui: &Ui) -> Option<String> {
    commands::busy(ui).or_else(|| ui.operation.refusal())
}

fn folder_offered(ui: &Ui, target: &Target, lists: &[Which]) -> Availability {
    match folder(ui, target) {
        Some((which, path)) if lists.contains(&which) => {
            Availability::unless(index_edit(ui).or_else(|| under(ui, which, path).is_empty().then(|| "No files in this folder are listed".to_owned())))
        }
        _ => Availability::Hidden,
    }
}

fn folder_action(ui: &Ui, target: &Target, verb: Verb) -> Option<view::Action> {
    let (which, path) = folder(ui, target)?;
    Some(view::Action::FileList(Action::Folder { which, path: path.clone(), verb }))
}

pub(crate) const FOLDER_COMMANDS: &[Command] = &[
    Command::new(
        "folder.stage",
        Group::Changes,
        |_, _| "Stage folder".into(),
        |ui, target, _| folder_offered(ui, target, &[Which::Unstaged]),
        |ui, target| folder_action(ui, target, Verb::Stage),
    )
    .icon("plus")
    .words(&["add directory"]),
    Command::new(
        "folder.unstage",
        Group::Changes,
        |_, _| "Unstage folder".into(),
        |ui, target, _| folder_offered(ui, target, &[Which::Staged]),
        |ui, target| folder_action(ui, target, Verb::Unstage),
    )
    .icon("minus")
    .words(&["reset directory"]),
    Command::new(
        "folder.discard",
        Group::Changes,
        |_, _| "Discard folder…".into(),
        |ui, target, _| folder_offered(ui, target, &[Which::Unstaged]),
        |ui, target| folder_action(ui, target, Verb::Discard),
    )
    .icon("undo")
    .danger()
    .words(&["revert directory"]),
    Command::new(
        "folder.ignore",
        Group::Changes,
        |_, _| "Ignore folder".into(),
        |ui, target, _| folder_offered(ui, target, &[Which::Unstaged]),
        |ui, target| folder_action(ui, target, Verb::Ignore),
    )
    .icon("eye-off")
    .words(&["gitignore", "exclude directory"]),
    Command::new(
        "folder.stash",
        Group::Stash,
        |_, _| "Stash folder".into(),
        |ui, target, _| folder_offered(ui, target, &[Which::Unstaged, Which::Staged]),
        |ui, target| folder_action(ui, target, Verb::Stash),
    )
    .icon("stash"),
    Command::new(
        "folder.copy-path",
        Group::Copy,
        |_, _| "Copy path".into(),
        |ui, target, _| if folder(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
        |ui, target| {
            folder(ui, target).map(|(_, path)| view::Action::CopyText { text: path.to_string_lossy().into_owned(), what: "path" })
        },
    )
    .icon("copy")
    .words(&["clipboard"]),
];

pub(crate) fn tools<T: Desk>(ui: &Ui, panel: Panel, cx: &Context<T>) -> Div {
    let tree = ui.file_list.mode == Mode::Tree;
    let (icon, tip, next) = if tree {
        ("list", "Show files as a list", Mode::List)
    } else {
        ("list-tree", "Show files as a tree", Mode::Tree)
    };
    let slot = panel.slot();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(2.))
        .child(kit::icon_btn(("files-mode", slot), icon, tip, view::Action::FileList(Action::Show(next)), cx))
        .child(
            div()
                .id(("files-options", slot))
                .size(px(22.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(HOVER)))
                .child(kit::icon("sort"))
                .tooltip(kit::tip("Sort and view options"))
                .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                    cx.stop_propagation();
                    app.dispatch(
                        view::Action::Menu(Some(Menu::Object { target: Target::FileList(panel), at: event.position() })),
                        w,
                        cx,
                    )
                })),
        )
        .when(!field_shown(ui, panel), |d| {
            d.child(kit::icon_btn(
                ("files-filter", slot),
                "funnel",
                "Filter files",
                view::Action::FileList(Action::Filter(panel)),
                cx,
            ))
        })
        .when_some(browsed(ui, panel), |d, rev| {
            d.child(kit::icon_btn(
                ("files-browse", slot),
                "folder",
                "Browse every file",
                view::Action::Browse(crate::browse::Action::Open(rev)),
                cx,
            ))
        })
}

fn browsed(ui: &Ui, panel: Panel) -> Option<crate::compare::Rev> {
    match (panel, &ui.selection) {
        (Panel::Commit, Some(Selection::Commit(oid))) => Some(crate::compare::Rev::Commit(oid.clone())),
        (Panel::Working, _) => Some(crate::compare::Rev::WorkingTree),
        _ => None,
    }
}

pub(crate) fn filter_field<T: Desk>(ui: &Ui, panel: Panel, cx: &Context<T>) -> Option<Div> {
    if !field_shown(ui, panel) {
        return None;
    }
    let typed = !ui.file_list.filter(panel).is_empty();
    let closable = typed || ui.file_list.asked[panel.slot()];
    Some(
        div()
            .px_2()
            .pb(px(6.))
            .flex_shrink_0()
            .relative()
            .child(ui.file_list.input(panel).clone())
            .when(closable, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .bottom(px(6.))
                        .right(px(12.))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .id(("files-filter-clear", panel.slot()))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(HOVER)))
                                .child(kit::icon("x").size(px(11.)).text_color(rgb(TEXT_3)))
                                .tooltip(kit::tip("Clear the filter (Esc)"))
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    cx.stop_propagation();
                                    app.dispatch(view::Action::FileList(Action::CloseFilter(panel)), w, cx)
                                })),
                        ),
                )
            }),
    )
}

pub(crate) fn nothing_matches(ui: &Ui, panel: Panel) -> Option<Div> {
    if !ui.file_list.filtering(panel) || total(ui, panel) == 0 {
        return None;
    }
    let none = panel.lists().iter().all(|which| rows(ui, *which).is_empty());
    none.then(|| {
        div().px_3().py_2().child(kit::muted(format!("No files match “{}”", ui.file_list.filter(panel).trim())))
    })
}

pub(crate) fn count_label(ui: &Ui, which: Which) -> String {
    let (matching, all) = counted(ui, which);
    if ui.file_list.filtering(which.panel()) && matching != all {
        format!("{matching} of {all}")
    } else {
        all.to_string()
    }
}

pub(crate) fn label(id: impl Into<gpui::ElementId>, status: &str, path: &Path, from: Option<&Path>, mode: Mode) -> Stateful<Div> {
    match mode {
        Mode::List => kit::file_label(id, status, path, from),
        Mode::Tree => {
            let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(kit::badge(status))
                .child(kit::fit(name).size(12.5).cut(kit::Cut::Middle).grow())
                .when_some(from, |d, from| d.tooltip(kit::tip(kit::moved_from(status, from))))
        }
    }
}

pub(crate) fn folder_row<T: Desk>(
    id: (&'static str, usize),
    which: Which,
    row: &Row,
    side: Option<Side>,
    actions: Option<Div>,
    cx: &Context<T>,
) -> gpui::AnyElement {
    let Row::Folder { path, name, depth, count, folded } = row else {
        return div().into_any_element();
    };
    let toggle = path.clone();
    let menu_path = path.clone();
    let tip = path.to_string_lossy().into_owned();
    kit::row(id, false)
        .group("folder-row")
        .h(px(FILE_ROW))
        .pl(px(ROW_START + *depth as f32 * INDENT))
        .pr_2()
        .gap(px(5.))
        .child(kit::icon(if *folded { "chevron-right" } else { "chevron-down" }).size(px(12.)).text_color(rgb(TEXT_3)))
        .child(kit::icon("folder").size(px(13.)).text_color(rgb(TEXT_3)))
        .child(
            div()
                .id((id.0, id.1 + 1_000_000))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::fit(name.clone()).size(12.5).color(TEXT_2).cut(kit::Cut::Start).grow())
                .tooltip(kit::tip(tip)),
        )
        .children(actions)
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(TEXT_TINY))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(TEXT_3))
                .child(count.to_string()),
        )
        .on_click(cx.listener(move |app, _, w, cx| {
            app.dispatch(view::Action::FileList(Action::Fold { which, path: toggle.clone() }), w, cx)
        }))
        .when_some(side, |d, side| {
            d.on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                    let target = Target::Folder { path: menu_path.clone(), side: side.clone() };
                    app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
                }),
            )
        })
        .into_any_element()
}

pub(crate) fn folder_actions<T: Desk>(ui: &Ui, which: Which, path: &Path, index: usize, cx: &Context<T>) -> Option<Div> {
    if !ui.can_edit_index() {
        return None;
    }
    let act = |verb| view::Action::FileList(Action::Folder { which, path: path.to_path_buf(), verb });
    let actions = div()
        .flex()
        .flex_shrink_0()
        .gap(px(2.))
        .invisible()
        .group_hover("folder-row", |s| s.visible());
    Some(match which {
        Which::Unstaged => actions
            .child(kit::icon_btn(("discard-folder", index), "undo", "Discard folder…", act(Verb::Discard), cx))
            .child(kit::icon_btn(("stage-folder", index), "plus", "Stage folder", act(Verb::Stage), cx)),
        Which::Staged => actions.child(kit::icon_btn(("unstage-folder", index), "minus", "Unstage folder", act(Verb::Unstage), cx)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn listed<'a>(files: &'a [(PathBuf, &'static str)]) -> Vec<Listed<'a>> {
        files.iter().map(|(path, status)| Listed { path, status }).collect()
    }

    fn files(rows: &[(&str, &'static str)]) -> Vec<(PathBuf, &'static str)> {
        rows.iter().map(|(path, status)| (PathBuf::from(path), *status)).collect()
    }

    fn drawn(files: &[Listed], rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::File { index, depth } => format!("{}{}", "  ".repeat(*depth), files[*index].path.display()),
                Row::Folder { name, depth, count, folded, .. } => {
                    format!("{}{} {name}/ ({count})", "  ".repeat(*depth), if *folded { "+" } else { "-" })
                }
            })
            .collect()
    }

    fn shape<'a>(mode: Mode, sort: Sort, filter: &'a str, folded: &'a BTreeSet<PathBuf>) -> Shape<'a> {
        Shape { mode, sort, filter, folded }
    }

    fn sample() -> Vec<(PathBuf, &'static str)> {
        files(&[
            ("README.md", "Modified"),
            ("src/app/models/user.rs", "Added"),
            ("src/app/models/team.rs", "Modified"),
            ("src/app/view.rs", "Deleted"),
            ("src/lib.rs", "Modified"),
            ("docs/guide/intro/start.md", "Untracked"),
            ("Zeta.txt", "Renamed"),
        ])
    }

    #[test]
    fn a_tree_folds_chains_of_single_folders_into_one_row_and_counts_the_files_under_each() {
        let sample = sample();
        let files = listed(&sample);
        let none = BTreeSet::new();
        let rows = arrange(&files, &shape(Mode::Tree, Sort::Path, "", &none));
        assert_eq!(
            drawn(&files, &rows),
            vec![
                "- docs/guide/intro/ (1)",
                "  docs/guide/intro/start.md",
                "- src/ (4)",
                "  - app/ (3)",
                "    - models/ (2)",
                "      src/app/models/team.rs",
                "      src/app/models/user.rs",
                "    src/app/view.rs",
                "  src/lib.rs",
                "README.md",
                "Zeta.txt",
            ]
        );
        let folded = BTreeSet::from([PathBuf::from("src/app")]);
        let rows = arrange(&files, &shape(Mode::Tree, Sort::Path, "", &folded));
        assert_eq!(
            drawn(&files, &rows),
            vec!["- docs/guide/intro/ (1)", "  docs/guide/intro/start.md", "- src/ (4)", "  + app/ (3)", "  src/lib.rs", "README.md", "Zeta.txt"]
        );
        assert_eq!(
            folders(&files, ""),
            vec![PathBuf::from("docs/guide/intro"), PathBuf::from("src"), PathBuf::from("src/app"), PathBuf::from("src/app/models")]
        );
        let chain = super::tests::files(&[("a/b/c/one.txt", "Added"), ("a/b/c/two.txt", "Added")]);
        let only = listed(&chain);
        assert_eq!(
            drawn(&only, &arrange(&only, &shape(Mode::Tree, Sort::Path, "", &none))),
            vec!["- a/b/c/ (2)", "  a/b/c/one.txt", "  a/b/c/two.txt"]
        );
    }

    #[test]
    fn lists_sort_by_path_by_name_or_by_status_and_the_filter_matches_anywhere_in_the_path() {
        let sample = sample();
        let files = listed(&sample);
        let none = BTreeSet::new();
        let paths = |mode, sort, filter| -> Vec<String> {
            arrange(&files, &shape(mode, sort, filter, &none))
                .iter()
                .filter_map(|row| match row {
                    Row::File { index, .. } => Some(files[*index].path.to_string_lossy().into_owned()),
                    Row::Folder { .. } => None,
                })
                .collect()
        };
        assert_eq!(
            paths(Mode::List, Sort::Path, ""),
            vec!["README.md", "Zeta.txt", "docs/guide/intro/start.md", "src/app/models/team.rs", "src/app/models/user.rs", "src/app/view.rs", "src/lib.rs"]
        );
        assert_eq!(
            paths(Mode::List, Sort::Name, ""),
            vec!["src/lib.rs", "README.md", "docs/guide/intro/start.md", "src/app/models/team.rs", "src/app/models/user.rs", "src/app/view.rs", "Zeta.txt"]
        );
        assert_eq!(
            paths(Mode::List, Sort::Status, ""),
            vec!["README.md", "src/app/models/team.rs", "src/lib.rs", "src/app/models/user.rs", "docs/guide/intro/start.md", "Zeta.txt", "src/app/view.rs"]
        );
        assert_eq!(paths(Mode::List, Sort::Path, " MODELS "), vec!["src/app/models/team.rs", "src/app/models/user.rs"]);
        assert_eq!(paths(Mode::List, Sort::Path, "app/v"), vec!["src/app/view.rs"]);
        assert!(paths(Mode::List, Sort::Path, "nowhere").is_empty());
        let rows = arrange(&files, &shape(Mode::Tree, Sort::Status, "src", &none));
        assert_eq!(
            drawn(&files, &rows),
            vec!["- src/ (4)", "  - app/ (3)", "    - models/ (2)", "      src/app/models/team.rs", "      src/app/models/user.rs", "    src/app/view.rs", "  src/lib.rs"]
        );
        let rows = arrange(&files, &shape(Mode::Tree, Sort::Path, "user", &none));
        assert_eq!(drawn(&files, &rows), vec!["- src/app/models/ (1)", "  src/app/models/user.rs"]);
        assert_eq!(within(&files, "", Path::new("src/app")), vec![
            PathBuf::from("src/app/models/team.rs"),
            PathBuf::from("src/app/models/user.rs"),
            PathBuf::from("src/app/view.rs"),
        ]);
        assert_eq!(within(&files, "user", Path::new("src/app")), vec![PathBuf::from("src/app/models/user.rs")]);
        assert!(within(&files, "", Path::new("src/ap")).is_empty());
    }

    fn reloaded(rows: &[(&str, &'static str)]) -> Vec<crate::repository::ChangedFile> {
        rows.iter().map(|(path, status)| crate::repository::ChangedFile { path: PathBuf::from(path), status, from: None }).collect()
    }

    fn read(cache: &Cache, files: &Files<crate::repository::ChangedFile>, folds: u64, shape: &Shape, built: &std::cell::Cell<usize>) -> (Vec<String>, usize) {
        let (rows, matching) = cache.get(Which::Commit, Key::new(files.stamp(), folds, shape), shape, || {
            built.set(built.get() + 1);
            changed(files)
        });
        (drawn(&changed(files), &rows), matching)
    }

    #[test]
    fn a_reloaded_file_list_is_arranged_again_and_an_unchanged_one_is_not() {
        let mut files = Files::default();
        files.set(reloaded(&[("src/lib.rs", "Modified"), ("README.md", "Added"), ("src/app/view.rs", "Deleted")]));
        let cache = Cache::default();
        let built = std::cell::Cell::new(0);
        let none = BTreeSet::new();
        let list = shape(Mode::List, Sort::Path, "", &none);
        let first = read(&cache, &files, 0, &list, &built);
        assert_eq!(first, (vec!["README.md".to_owned(), "src/app/view.rs".to_owned(), "src/lib.rs".to_owned()], 3));
        assert_eq!(read(&cache, &files, 0, &list, &built), first);
        assert_eq!(built.get(), 1);
        let before = files.stamp();
        files.set(reloaded(&[("src/lib.rs", "Modified"), ("README.md", "Added"), ("src/app/view.rs", "Deleted")]));
        assert_ne!(files.stamp(), before);
        assert_eq!(read(&cache, &files, 0, &list, &built), first);
        assert_eq!(built.get(), 2);
        files.set(reloaded(&[("src/lib.rs", "Modified"), ("notes.txt", "Untracked")]));
        assert_eq!(read(&cache, &files, 0, &list, &built), (vec!["notes.txt".to_owned(), "src/lib.rs".to_owned()], 2));
        files.set(reloaded(&[("src/lib.rs", "Deleted"), ("notes.txt", "Untracked")]));
        let status = shape(Mode::List, Sort::Status, "", &none);
        assert_eq!(read(&cache, &files, 0, &status, &built), (vec!["notes.txt".to_owned(), "src/lib.rs".to_owned()], 2));
        files.set(reloaded(&[("src/lib.rs", "Modified"), ("notes.txt", "Untracked")]));
        assert_eq!(read(&cache, &files, 0, &status, &built), (vec!["src/lib.rs".to_owned(), "notes.txt".to_owned()], 2));
        files.clear();
        assert_eq!(read(&cache, &files, 0, &status, &built), (Vec::new(), 0));
        assert_ne!(Files::<crate::repository::ChangedFile>::default().stamp(), Files::<crate::repository::ChangedFile>::default().stamp());
    }

    #[test]
    fn the_arranged_rows_follow_the_mode_the_sort_the_filter_and_the_folded_folders() {
        let mut files = Files::default();
        files.set(reloaded(&[("src/app/models/user.rs", "Added"), ("src/lib.rs", "Modified"), ("README.md", "Modified")]));
        let cache = Cache::default();
        let built = std::cell::Cell::new(0);
        let none = BTreeSet::new();
        let tree = shape(Mode::Tree, Sort::Path, "", &none);
        assert_eq!(read(&cache, &files, 0, &tree, &built).0, vec!["- src/ (2)", "  - app/models/ (1)", "    src/app/models/user.rs", "  src/lib.rs", "README.md"]);
        let folded = BTreeSet::from([PathBuf::from("src/app/models")]);
        let tree_folded = shape(Mode::Tree, Sort::Path, "", &folded);
        assert_eq!(read(&cache, &files, 1, &tree_folded, &built).0, vec!["- src/ (2)", "  + app/models/ (1)", "  src/lib.rs", "README.md"]);
        assert_eq!(read(&cache, &files, 2, &tree, &built).0, vec!["- src/ (2)", "  - app/models/ (1)", "    src/app/models/user.rs", "  src/lib.rs", "README.md"]);
        let list = shape(Mode::List, Sort::Path, "", &none);
        assert_eq!(read(&cache, &files, 2, &list, &built).0, vec!["README.md", "src/app/models/user.rs", "src/lib.rs"]);
        let name = shape(Mode::List, Sort::Name, "", &none);
        assert_eq!(read(&cache, &files, 2, &name, &built).0, vec!["src/lib.rs", "README.md", "src/app/models/user.rs"]);
        let filtered = shape(Mode::List, Sort::Name, " SRC ", &none);
        assert_eq!(read(&cache, &files, 2, &filtered, &built), (vec!["src/lib.rs".to_owned(), "src/app/models/user.rs".to_owned()], 2));
        let same = shape(Mode::List, Sort::Name, "src", &none);
        let count = built.get();
        assert_eq!(read(&cache, &files, 2, &same, &built).1, 2);
        assert_eq!(built.get(), count);
        let (rows, matching) = read(&cache, &files, 3, &tree_folded, &built);
        assert_eq!(rows, vec!["- src/ (2)", "  + app/models/ (1)", "  src/lib.rs", "README.md"]);
        assert_eq!(matching, 3);
    }

    #[test]
    fn discarding_a_folder_of_one_file_asks_about_that_file_and_a_larger_folder_lists_its_files() {
        let folder = Path::new("docs");
        let one = vec![PathBuf::from("docs/intro.md")];
        match folder_call(folder, one.clone(), Verb::Discard) {
            view::Action::AskDiscard(view::Discard::File(path)) => assert_eq!(path, one[0]),
            other => panic!("one file asks {other:?}"),
        }
        let two = vec![PathBuf::from("docs/intro.md"), PathBuf::from("docs/notes.md")];
        match folder_call(folder, two.clone(), Verb::Discard) {
            view::Action::Picked(crate::picked::Action::AskDiscard(paths)) => assert_eq!(paths, two),
            other => panic!("two files ask {other:?}"),
        }
        match folder_call(Path::new("src/app/models"), one.clone(), Verb::Ignore) {
            view::Action::Ignore(crate::ignore::Action::Ignore { paths, rule }) => {
                assert_eq!((paths, rule), (one.clone(), crate::ignore::Rule::Enclosing(3)))
            }
            other => panic!("ignoring asks {other:?}"),
        }
        match folder_call(folder, one.clone(), Verb::Stage) {
            view::Action::Picked(crate::picked::Action::Stage(paths)) => assert_eq!(paths, one),
            other => panic!("staging asks {other:?}"),
        }
    }

    fn worktree() -> Fixture {
        let f = Fixture::new();
        for path in ["src/app/models/user.rs", "src/app/view.rs", "src/lib.rs", "docs/intro.md", "src/application.rs"] {
            let full = f.dir.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, "first\n").unwrap();
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "Lay out the project"]);
        for path in ["src/app/models/user.rs", "src/app/view.rs", "src/lib.rs", "docs/intro.md", "src/application.rs"] {
            fs::write(f.dir.join(path), "second\n").unwrap();
        }
        fs::write(f.dir.join("src/app/models/team.rs"), "new\n").unwrap();
        f
    }

    fn unstaged(f: &Fixture) -> Vec<(PathBuf, &'static str)> {
        f.repo.status().unwrap().unstaged.into_iter().map(|file| (file.path, file.status)).collect()
    }

    fn staged_names(f: &Fixture) -> String {
        String::from_utf8(f.git(&["diff", "--cached", "--name-only"])).unwrap()
    }

    fn refs(f: &Fixture) -> String {
        String::from_utf8(f.git(&["for-each-ref"])).unwrap()
    }

    #[test]
    fn staging_a_folder_stages_exactly_the_files_under_it_and_unstaging_takes_them_back() {
        let f = worktree();
        let before = refs(&f);
        let status = unstaged(&f);
        let files = listed(&status);
        let under_app = within(&files, "", Path::new("src/app"));
        assert_eq!(
            under_app,
            vec![PathBuf::from("src/app/models/team.rs"), PathBuf::from("src/app/models/user.rs"), PathBuf::from("src/app/view.rs")]
        );
        f.repo.stage_files(&under_app).unwrap();
        assert_eq!(staged_names(&f), "src/app/models/team.rs\nsrc/app/models/user.rs\nsrc/app/view.rs\n");
        assert_eq!(fs::read_to_string(f.dir.join("src/application.rs")).unwrap(), "second\n");
        let staged: Vec<(PathBuf, &'static str)> =
            f.repo.status().unwrap().staged.into_iter().map(|file| (file.path, file.status)).collect();
        let staged = listed(&staged);
        let models = within(&staged, "", Path::new("src/app/models"));
        f.repo.unstage_files(&models).unwrap();
        assert_eq!(staged_names(&f), "src/app/view.rs\n");
        assert_eq!(fs::read_to_string(f.dir.join("src/app/models/team.rs")).unwrap(), "new\n");
        assert_eq!(refs(&f), before);
    }

    #[test]
    fn discarding_and_ignoring_a_folder_touch_only_the_files_under_it_and_refuse_a_changed_repository() {
        let f = worktree();
        let before = refs(&f);
        let status = unstaged(&f);
        let files = listed(&status);
        let models = within(&files, "", Path::new("src/app/models"));
        let untracked: BTreeSet<PathBuf> =
            status.iter().filter(|(_, status)| *status == "Untracked").map(|(path, _)| path.clone()).collect();
        let untracked: BTreeSet<PathBuf> = untracked.into_iter().filter(|path| models.contains(path)).collect();
        f.repo.discard_files(&models, &untracked).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("src/app/models/user.rs")).unwrap(), "first\n");
        assert!(!f.dir.join("src/app/models/team.rs").exists());
        assert_eq!(fs::read_to_string(f.dir.join("src/app/view.rs")).unwrap(), "second\n");
        assert_eq!(staged_names(&f), "");

        fs::create_dir_all(f.dir.join("build/cache")).unwrap();
        fs::write(f.dir.join("build/cache/out.bin"), "out\n").unwrap();
        fs::write(f.dir.join("build/log.txt"), "log\n").unwrap();
        let status = unstaged(&f);
        let files = listed(&status);
        let build = within(&files, "", Path::new("build"));
        assert_eq!(build, vec![PathBuf::from("build/cache/out.bin"), PathBuf::from("build/log.txt")]);
        let depth = Path::new("build").components().count();
        f.repo.ignore(&build, crate::ignore::Rule::Enclosing(depth), &[]).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join(".gitignore")).unwrap(), "/build/\n");
        assert!(!unstaged(&f).iter().any(|(path, _)| path.starts_with("build")));

        let docs = within(&listed(&unstaged(&f)), "", Path::new("docs"));
        assert_eq!(docs, vec![PathBuf::from("docs/intro.md")]);
        fs::write(f.dir.join("docs/notes.md"), "notes\n").unwrap();
        f.git(&["add", "docs/notes.md"]);
        let tracked = vec![PathBuf::from("docs/intro.md")];
        let asked = vec![PathBuf::from("docs/intro.md"), PathBuf::from("docs/notes.md")];
        let refused = f.repo.ignore(&asked, crate::ignore::Rule::Enclosing(1), &tracked).unwrap_err();
        assert!(refused.contains("docs/notes.md is now tracked"), "{refused}");
        assert_eq!(fs::read_to_string(f.dir.join(".gitignore")).unwrap(), "/build/\n");
        assert_eq!(staged_names(&f), "docs/notes.md\n");
        assert_eq!(refs(&f), before);
    }
}
