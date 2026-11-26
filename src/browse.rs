use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Side, Target},
    compare::Rev,
    diff_doc::{DiffDoc, PatchState},
    file_list::{self, Listed, Shape},
    file_view::{self, Origin, Version},
    jobs::Link,
    repository::{Repository, checked, git_error, raw_path, run_with_input, stream_into, validate_oid, validate_path},
    theme::*,
    view::{self, CommitInput, Desk, DiffLayout, Selection, Ui},
};
use gpui::{
    Context, Div, Entity, FontWeight, Focusable, Pixels, Point, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, div, prelude::*, relative, rgb, uniform_list,
};
use crate::theme::px;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime},
};

pub(crate) const LIMIT: usize = 200_000;
const LIST_WIDTH: f32 = 340.;
const STEP_INDENT: f32 = 12.;
const ROW_START: f32 = 10.;
const CHEVRON: f32 = 16.;
pub(crate) const EMPTY: &str = "There are no files here.";
pub(crate) const NO_COMMITS: &str = "There are no commits yet";
pub(crate) const NEVER_COMMITTED: &str = "This file has never been committed";
pub(crate) const MODULE: &str = "This is a submodule, so its files are not listed here.";
pub(crate) const CAPPED: &str = "Only the first 200,000 paths are searched.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Folder,
    File,
    Link,
    Module,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) kind: Kind,
    pub(crate) ignored: bool,
}

impl Entry {
    fn name(&self) -> String {
        self.path.file_name().unwrap_or(self.path.as_os_str()).to_string_lossy().into_owned()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Entry { path: PathBuf, name: String, kind: Kind, depth: usize, unfolded: bool, ignored: bool, count: Option<usize> },
    Note { depth: usize, text: String, loading: bool },
}

impl Row {
    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Self::Entry { path, .. } => Some(path),
            Self::Note { .. } => None,
        }
    }
}

enum Listing {
    Loading,
    Loaded(Vec<Entry>),
    Failed(String),
}

enum Every {
    Idle,
    Loading,
    Loaded { paths: Arc<Vec<PathBuf>>, capped: bool },
    Failed(String),
}

struct Open {
    rev: Rev,
    subject: Option<String>,
    owner: Option<Selection>,
    diff_open: bool,
    folders: HashMap<PathBuf, Listing>,
    expanded: BTreeSet<PathBuf>,
    folded: BTreeSet<PathBuf>,
    every: Every,
    link: Option<Link>,
    query: String,
    rows: Vec<Row>,
    cursor: Option<PathBuf>,
    chosen: Option<PathBuf>,
    reveal: Option<PathBuf>,
    stamp: Option<(u64, SystemTime)>,
    scroll: UniformListScrollHandle,
}

pub(crate) struct State {
    open: Option<Open>,
    ticket: u64,
    file_ticket: u64,
    placed: Option<u64>,
    pub(crate) filter: Entity<CommitInput>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter every file by path"));
        cx.observe(&filter, |tab, _, cx| typed(tab, cx)).detach();
        Self { open: None, ticket: 0, file_ticket: 0, placed: None, filter }
    }

    pub(crate) fn open(&self) -> bool {
        self.open.is_some()
    }

    pub(crate) fn rev(&self) -> Option<&Rev> {
        self.open.as_ref().map(|open| &open.rev)
    }

    pub(crate) fn rows(&self) -> &[Row] {
        self.open.as_ref().map_or(&[], |open| open.rows.as_slice())
    }

    pub(crate) fn chosen(&self) -> Option<&Path> {
        self.open.as_ref()?.chosen.as_deref()
    }

    pub(crate) fn cursor(&self) -> Option<&Path> {
        self.open.as_ref()?.cursor.as_deref()
    }

    pub(crate) fn query(&self) -> &str {
        self.open.as_ref().map_or("", |open| open.query.as_str())
    }

    pub(crate) fn searching(&self) -> bool {
        self.open.as_ref().is_some_and(|open| matches!(open.every, Every::Loading))
    }

    pub(crate) fn capped(&self) -> bool {
        self.open.as_ref().is_some_and(|open| matches!(open.every, Every::Loaded { capped: true, .. }))
    }

    pub(crate) fn every(&self) -> Option<&[PathBuf]> {
        match &self.open.as_ref()?.every {
            Every::Loaded { paths, .. } => Some(paths.as_slice()),
            _ => None,
        }
    }

    pub(crate) fn settled(&self) -> bool {
        self.open.as_ref().is_some_and(|open| {
            open.folders.values().all(|listing| !matches!(listing, Listing::Loading)) && !matches!(open.every, Every::Loading)
        })
    }

    pub(crate) fn entry(&self, path: &Path) -> Option<&Entry> {
        let open = self.open.as_ref()?;
        let parent = path.parent().unwrap_or(Path::new(""));
        match open.folders.get(parent)? {
            Listing::Loaded(entries) => entries.iter().find(|entry| entry.path == path),
            _ => None,
        }
    }

    fn close(&mut self) -> Option<(Option<Selection>, bool)> {
        self.ticket += 1;
        self.file_ticket += 1;
        self.open.take().map(|open| (open.owner.clone(), open.diff_open))
    }
}

impl Open {
    fn new(rev: Rev, owner: Option<Selection>, diff_open: bool) -> Self {
        Self {
            rev,
            subject: None,
            owner,
            diff_open,
            folders: HashMap::new(),
            expanded: BTreeSet::new(),
            folded: BTreeSet::new(),
            every: Every::Idle,
            link: None,
            query: String::new(),
            rows: Vec::new(),
            cursor: None,
            chosen: None,
            reveal: None,
            stamp: None,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        if let Some(link) = self.link.take() {
            link.cancel();
        }
    }
}

fn order(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    let folder = |entry: &Entry| entry.kind != Kind::Folder;
    let name = |entry: &Entry| entry.name().to_lowercase();
    folder(a).cmp(&folder(b)).then_with(|| name(a).cmp(&name(b))).then_with(|| a.path.cmp(&b.path))
}

fn nul_list(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|&b| b == 0).filter(|record| !record.is_empty())
}

impl Repository {
    pub(crate) fn browse_folder(&self, rev: &Rev, folder: &Path) -> Result<Vec<Entry>, String> {
        if !folder.as_os_str().is_empty() {
            validate_path(folder)?;
        }
        let mut entries = match rev {
            Rev::Commit(oid) => self.tree_folder(oid, folder)?,
            Rev::WorkingTree => self.working_folder(folder)?,
        };
        entries.sort_by(order);
        Ok(entries)
    }

    fn tree_folder(&self, oid: &str, folder: &Path) -> Result<Vec<Entry>, String> {
        validate_oid(oid)?;
        let mut tree = OsString::from(oid);
        if folder.as_os_str().is_empty() {
            tree.push("^{tree}");
        } else {
            tree.push(":");
            tree.push(folder.as_os_str());
        }
        let output = checked(self.read_command().args(["ls-tree", "-z"]).arg(tree))?;
        let mut entries = Vec::new();
        for record in nul_list(&output) {
            let tab = record.iter().position(|&b| b == b'\t').ok_or("Unreadable folder listing")?;
            let mut fields = record[..tab].split(|&b| b == b' ');
            let mode = fields.next().unwrap_or_default();
            let kind = match (fields.next().unwrap_or_default(), mode) {
                (b"tree", _) => Kind::Folder,
                (b"commit", _) => Kind::Module,
                (_, b"120000") => Kind::Link,
                _ => Kind::File,
            };
            entries.push(Entry { path: folder.join(raw_path(&record[tab + 1..])), kind, ignored: false });
        }
        Ok(entries)
    }

    fn working_folder(&self, folder: &Path) -> Result<Vec<Entry>, String> {
        let disk = self.root.join(folder);
        let read = std::fs::read_dir(&disk).map_err(|error| format!("This folder could not be read. {error}"))?;
        let mut entries = Vec::new();
        for item in read {
            let Ok(item) = item else { continue };
            let name = item.file_name();
            if name == ".git" {
                continue;
            }
            let kind = match item.file_type() {
                Ok(kind) if kind.is_symlink() => Kind::Link,
                Ok(kind) if kind.is_dir() && std::fs::symlink_metadata(item.path().join(".git")).is_ok() => Kind::Module,
                Ok(kind) if kind.is_dir() => Kind::Folder,
                Ok(_) => Kind::File,
                Err(_) => continue,
            };
            entries.push(Entry { path: folder.join(&name), kind, ignored: false });
        }
        let ignored = self.ignored_of(&entries)?;
        for entry in &mut entries {
            entry.ignored = ignored.contains(&entry.path);
        }
        Ok(entries)
    }

    fn ignored_of(&self, entries: &[Entry]) -> Result<BTreeSet<PathBuf>, String> {
        if entries.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut input = Vec::new();
        for entry in entries {
            input.extend_from_slice(b"./");
            input.extend_from_slice(entry.path.as_os_str().as_encoded_bytes());
            input.push(0);
        }
        let mut check = self.command();
        check.env_remove("GIT_LITERAL_PATHSPECS").args(["--no-optional-locks", "check-ignore", "--stdin", "-z"]);
        let output = run_with_input(&mut check, input).map_err(|error| format!("Run bundled Git: {error}"))?;
        match output.status.code() {
            Some(0 | 1) => Ok(nul_list(&output.stdout).map(|name| raw_path(name.strip_prefix(b"./").unwrap_or(name))).collect()),
            _ => Err(git_error(&output.stderr, output.status)),
        }
    }

    pub(crate) fn browse_paths(&self, rev: &Rev, link: &Link, limit: usize) -> Result<(Vec<PathBuf>, bool), String> {
        let mut command = self.read_command();
        match rev {
            Rev::Commit(oid) => {
                validate_oid(oid)?;
                command.args(["ls-tree", "-r", "-z", "--name-only", oid.as_str()]);
            }
            Rev::WorkingTree => {
                command.args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--deduplicate"]);
            }
        }
        listed_paths(&mut command, link, limit)
    }

    pub(crate) fn tracked_paths(&self, link: &Link, limit: usize) -> Result<(Vec<PathBuf>, bool), String> {
        listed_paths(self.read_command().args(["ls-files", "-z", "--cached", "--deduplicate"]), link, limit)
    }
}

fn listed_paths(command: &mut std::process::Command, link: &Link, limit: usize) -> Result<(Vec<PathBuf>, bool), String> {
    let kept: Arc<Mutex<Vec<u8>>> = Arc::default();
    let capped = Arc::new(AtomicBool::new(false));
    let (into, full, stop) = (kept.clone(), capped.clone(), link.clone());
    let mut count = 0;
    let (started, at) = (Instant::now(), SystemTime::now());
    let result = stream_into(command, link, move |bytes| {
        if full.load(Ordering::SeqCst) {
            return;
        }
        let mut kept = into.lock().unwrap_or_else(|e| e.into_inner());
        for (at, byte) in bytes.iter().enumerate() {
            if *byte == 0 {
                count += 1;
                if count > limit {
                    kept.extend_from_slice(&bytes[..=at]);
                    full.store(true, Ordering::SeqCst);
                    stop.cancel();
                    return;
                }
            }
        }
        kept.extend_from_slice(bytes);
    });
    crate::activity::record(command, started, at, &result);
    let capped = capped.load(Ordering::SeqCst);
    match result {
        Ok(output) if !output.status.success() => return Err(git_error(&output.stderr, output.status)),
        Ok(_) => {}
        Err(_) if capped => {}
        Err(error) => return Err(format!("Run bundled Git: {error}")),
    }
    let bytes = std::mem::take(&mut *kept.lock().unwrap_or_else(|e| e.into_inner()));
    let mut paths: Vec<PathBuf> = nul_list(&bytes).map(raw_path).collect();
    paths.truncate(limit);
    paths.sort_unstable();
    Ok((paths, capped))
}

impl Repository {
    pub(crate) fn browsed_file(&self, rev: &Rev, path: &Path, owner: &Selection, tab: usize) -> (DiffDoc, DiffLayout) {
        let doc = match self.browsed_origin(rev, path) {
            Err(reason) => file_view::failed(Version::Changed, reason),
            Ok((origin, version)) => match self.file_contents(&origin, file_view::FILE_LIMIT) {
                Ok(contents) => file_view::shape(&DiffDoc::default(), owner, path, &origin, version, contents).0,
                Err(reason) => file_view::failed(version, reason),
            },
        };
        let layout = DiffLayout::new(&doc, tab);
        (doc, layout)
    }

    fn browsed_origin(&self, rev: &Rev, path: &Path) -> Result<(Origin, Version), String> {
        validate_path(path)?;
        match rev {
            Rev::WorkingTree => Ok((Origin::Working(self.root.join(path)), Version::Working)),
            Rev::Commit(oid) => {
                validate_oid(oid)?;
                let name = path.to_str().filter(|name| !name.contains('\n')).ok_or("This file's name cannot be looked up.")?;
                Ok((Origin::Revision(format!("{oid}:{name}")), Version::Changed))
            }
        }
    }
}

fn walk(open: &Open, folder: &Path, depth: usize, rows: &mut Vec<Row>) {
    match open.folders.get(folder) {
        Some(Listing::Loaded(entries)) => {
            if entries.is_empty() {
                rows.push(Row::Note { depth, text: EMPTY.to_owned(), loading: false });
            }
            for entry in entries {
                let unfolded = entry.kind == Kind::Folder && open.expanded.contains(&entry.path);
                rows.push(Row::Entry {
                    path: entry.path.clone(),
                    name: entry.name(),
                    kind: entry.kind,
                    depth,
                    unfolded,
                    ignored: entry.ignored,
                    count: None,
                });
                if unfolded {
                    walk(open, &entry.path, depth + 1, rows);
                }
            }
        }
        Some(Listing::Failed(reason)) => rows.push(Row::Note { depth, text: reason.clone(), loading: false }),
        _ => rows.push(Row::Note { depth, text: "Loading…".to_owned(), loading: true }),
    }
}

pub(crate) fn found(paths: &[PathBuf], query: &str, folded: &BTreeSet<PathBuf>) -> Vec<Row> {
    let listed: Vec<Listed> = paths.iter().map(|path| Listed { path, status: "" }).collect();
    let shape = Shape { mode: file_list::Mode::Tree, sort: file_list::Sort::Path, filter: query, folded };
    let rows: Vec<Row> = file_list::arrange(&listed, &shape)
        .into_iter()
        .map(|row| match row {
            file_list::Row::File { index, depth } => {
                let path = paths[index].clone();
                let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
                Row::Entry { path, name, kind: Kind::File, depth, unfolded: false, ignored: false, count: None }
            }
            file_list::Row::Folder { path, name, depth, count, folded } => {
                Row::Entry { path, name, kind: Kind::Folder, depth, unfolded: !folded, ignored: false, count: Some(count) }
            }
        })
        .collect();
    if rows.is_empty() {
        return vec![Row::Note { depth: 0, text: format!("No files match “{query}”."), loading: false }];
    }
    rows
}

fn rebuild(open: &mut Open) {
    open.rows = if open.query.is_empty() {
        let mut rows = Vec::new();
        walk(open, Path::new(""), 0, &mut rows);
        rows
    } else {
        match &open.every {
            Every::Loaded { paths, .. } => found(paths, &open.query, &open.folded),
            Every::Failed(reason) => vec![Row::Note { depth: 0, text: reason.clone(), loading: false }],
            Every::Idle | Every::Loading => vec![Row::Note { depth: 0, text: "Reading every path…".to_owned(), loading: true }],
        }
    };
    if let Some(wanted) = open.reveal.clone()
        && let Some(at) = open.rows.iter().position(|row| row.path() == Some(wanted.as_path()))
    {
        open.scroll.scroll_to_item(at, ScrollStrategy::Center);
        open.reveal = None;
    }
}

pub(crate) fn showing(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::Browse
}

pub(crate) fn shows_file(ui: &Ui) -> bool {
    ui.browse.chosen().is_some()
}

pub(crate) fn showing_file(ui: &Ui) -> bool {
    showing(ui) && shows_file(ui)
}

fn owner_of(rev: &Rev) -> Selection {
    match rev {
        Rev::Commit(oid) => Selection::Commit(oid.clone()),
        Rev::WorkingTree => Selection::Changes,
    }
}

pub(crate) fn selected(tab: &mut Tab, selection: &Selection, cx: &mut Context<Tab>) -> bool {
    let Some(open) = &tab.ui.browse.open else { return false };
    if tab.ui.file_history.open() {
        return false;
    }
    if open.owner.as_ref() == Some(selection) {
        let holding = tab.ui.selection.as_ref() == Some(selection);
        if holding && tab.ui.browse.placed != Some(tab.diff_ticket) {
            reshow(tab, cx);
        }
        return holding;
    }
    tab.ui.browse.close();
    tab.ui.diff_options.measured = px(0.);
    false
}

pub(crate) fn kept(ui: &Ui) -> bool {
    ui.browse.open.as_ref().is_some_and(|open| open.owner == ui.selection) && !ui.file_history.open()
}

pub(crate) fn leave(tab: &mut Tab) {
    if tab.ui.file_history.open() {
        return;
    }
    if tab.ui.browse.close().is_some() {
        tab.ui.diff_options.measured = px(0.);
    }
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(open), Some(repo)) = (&tab.ui.browse.open, &tab.repo) else { return };
    if open.rev != Rev::WorkingTree {
        return;
    }
    let loaded: Vec<PathBuf> =
        open.folders.iter().filter(|(_, listing)| matches!(listing, Listing::Loaded(_))).map(|(folder, _)| folder.clone()).collect();
    let changed = open.chosen.as_ref().is_some_and(|path| crate::image_diff::stamp(&repo.root.join(path)) != open.stamp);
    let showing = tab.ui.browse.placed == Some(tab.diff_ticket);
    if let Some(open) = tab.ui.browse.open.as_mut()
        && open.query.is_empty()
    {
        open.every = Every::Idle;
    }
    for folder in loaded {
        read_folder(tab, folder, cx);
    }
    if changed && showing && showing_file(&tab.ui) {
        let place = tab.ui.diff_scroll.0.borrow().base_handle.offset();
        load_file(tab, Some(place), cx);
    }
}

pub(crate) fn reshow(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.browse.chosen().is_some() {
        load_file(tab, None, cx);
    } else {
        tab.clear_diff();
        tab.ui.browse.placed = Some(tab.diff_ticket);
    }
}

pub(crate) fn refusal(ui: &Ui) -> Option<String> {
    let path = ui.browse.chosen()?;
    if ui.head.is_none() {
        return Some(NO_COMMITS.to_owned());
    }
    if ui.browse.rev() != Some(&Rev::WorkingTree) {
        return None;
    }
    let untracked = ui.files.iter().any(|row| row.path == path && row.status == "Untracked");
    let ignored = ui.browse.entry(path).is_some_and(|entry| entry.ignored);
    (untracked || ignored).then(|| NEVER_COMMITTED.to_owned())
}

fn side(rev: &Rev) -> Side {
    match rev {
        Rev::Commit(oid) => Side::Commit(oid.clone()),
        Rev::WorkingTree => Side::Unstaged,
    }
}

pub(crate) fn history_request(ui: &Ui) -> Option<crate::file_history::Request> {
    let path = ui.browse.chosen()?;
    crate::file_history::request(ui, path, &side(ui.browse.rev()?))
}

pub(crate) fn blame_request(ui: &Ui) -> Option<crate::blame::Request> {
    let path = ui.browse.chosen()?;
    let at = match ui.browse.rev()? {
        Rev::Commit(oid) => crate::blame::At::Commit(oid.clone()),
        Rev::WorkingTree => crate::blame::At::Working,
    };
    Some(crate::blame::Request::at(ui, path, at))
}

pub(crate) fn placeholder(ui: &Ui) -> Option<String> {
    (showing_file(ui) && ui.diff.state == PatchState::Loading).then(|| file_view::LOADING.to_owned())
}

pub(crate) fn title(ui: &Ui) -> Option<(String, String)> {
    let path = ui.browse.chosen()?;
    let at = match ui.browse.rev()? {
        Rev::Commit(oid) => format!("At {}", oid.chars().take(7).collect::<String>()),
        Rev::WorkingTree => "Working copy".to_owned(),
    };
    Some((path.to_string_lossy().into_owned(), at))
}

pub(crate) fn ignored(ui: &Ui) -> bool {
    ui.browse.chosen().and_then(|path| ui.browse.entry(path)).is_some_and(|entry| entry.ignored)
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "commit.browse-files",
        Group::View,
        |ui, _| if ui.palette.open() { "Browse files at this commit" } else { "Browse files" }.into(),
        |_, target, _| match target {
            Target::Commit(_) => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Commit(oid) => Some(view::Action::Browse(Action::Open(Rev::Commit(oid.clone())))),
            _ => None,
        },
    )
    .words(&["view all files", "file tree", "every file", "tree at this commit", "show files"])
    .icon("folder"),
    Command::new(
        "repo.browse-files",
        Group::View,
        |ui, _| if ui.palette.open() { "Browse working files" } else { "Browse files" }.into(),
        |ui, target, _| {
            if crate::commands::on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
        },
        |_, _| Some(view::Action::Browse(Action::Open(Rev::WorkingTree))),
    )
    .words(&["view all files", "file tree", "every file", "working copy files", "show files"])
    .icon("folder"),
];

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(Rev),
    Reveal(Rev, PathBuf),
    Toggle(PathBuf),
    Choose(PathBuf),
    Step(isize),
    Sideways(bool),
    Activate,
    Unchoose,
    History,
    Blame,
    FocusFilter,
    ClearFilter,
    Back,
    Close,
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(rev) => {
            open(tab, rev, window, cx);
        }
        Action::Reveal(rev, path) => {
            if validate_path(&path).is_err() {
                return;
            }
            if tab.ui.browse.rev() != Some(&rev) || view::centre(&tab.ui) != view::Centre::Browse {
                open(tab, rev, window, cx);
            }
            reveal(tab, path, cx);
        }
        Action::Toggle(path) => toggle(tab, path, cx),
        Action::Choose(path) => choose(tab, path, cx),
        Action::Step(delta) => step(tab, delta, cx),
        Action::Sideways(right) => sideways(tab, right, cx),
        Action::Activate => {
            let Some(row) = cursor_row(&tab.ui).cloned() else { return };
            if let Row::Entry { path, kind, .. } = row {
                match kind {
                    Kind::Folder => toggle(tab, path, cx),
                    Kind::File | Kind::Link => choose(tab, path, cx),
                    Kind::Module => tab.dispatch(view::Action::Submodule(crate::submodule::Action::Open(path)), window, cx),
                }
            }
        }
        Action::Unchoose => unchoose(tab),
        Action::History => {
            if refusal(&tab.ui).is_none()
                && let Some(request) = history_request(&tab.ui)
            {
                tab.dispatch(view::Action::FileHistory(crate::file_history::Action::Open(request)), window, cx);
            }
        }
        Action::Blame => {
            if refusal(&tab.ui).is_none()
                && let Some(request) = blame_request(&tab.ui)
            {
                tab.dispatch(view::Action::Blame(crate::blame::Action::Open(request)), window, cx);
            }
        }
        Action::FocusFilter => {
            if tab.ui.browse.open() {
                window.focus(&tab.ui.browse.filter.focus_handle(cx));
            }
        }
        Action::ClearFilter => {
            tab.ui.browse.filter.clone().update(cx, |field, cx| field.clear(cx));
            view::focus(&tab.ui, window);
        }
        Action::Back => {
            if tab.ui.browse.chosen().is_some() {
                unchoose(tab);
            } else if !tab.ui.browse.query().is_empty() {
                tab.ui.browse.filter.clone().update(cx, |field, cx| field.clear(cx));
                view::focus(&tab.ui, window);
            } else {
                close(tab, window, cx);
            }
        }
        Action::Close => close(tab, window, cx),
    }
    cx.notify();
}

fn open(tab: &mut Tab, rev: Rev, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    if let Rev::Commit(oid) = &rev
        && validate_oid(oid).is_err()
    {
        return;
    }
    crate::blame::leave(tab);
    crate::file_history::leave(tab);
    let wanted = owner_of(&rev);
    if tab.ui.selection.as_ref() != Some(&wanted) {
        tab.choose(wanted, cx);
    }
    let previous = tab.ui.browse.close();
    let diff_open = match previous {
        Some((_, diff_open)) => diff_open,
        None => tab.ui.diff_open,
    };
    tab.ui.diff_open = false;
    tab.clear_diff();
    if let Some(owner) = tab.ui.selection.clone() {
        crate::image_diff::follow(tab, None, crate::image_diff::owner(&owner, None));
    }
    tab.ui.diff_options.measured = px(0.);
    let mut opened = Open::new(rev, tab.ui.selection.clone(), diff_open);
    if let Rev::Commit(oid) = &opened.rev {
        opened.subject = tab
            .ui
            .history
            .iter()
            .find(|commit| commit.oid == *oid)
            .map(|commit| commit.subject.clone())
            .or_else(|| tab.ui.inspection.subject.clone());
    }
    tab.ui.browse.open = Some(opened);
    tab.ui.browse.filter.clone().update(cx, |field, cx| field.clear(cx));
    if let Some(open) = tab.ui.browse.open.as_mut() {
        rebuild(open);
    }
    tab.ui.browse.placed = Some(tab.diff_ticket);
    load_folder(tab, PathBuf::new(), cx);
    view::focus(&tab.ui, window);
}

fn reveal(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    let mut folders = Vec::new();
    let mut above = path.parent();
    while let Some(folder) = above.filter(|folder| !folder.as_os_str().is_empty()) {
        folders.push(folder.to_path_buf());
        above = folder.parent();
    }
    open.expanded.extend(folders.iter().cloned());
    open.reveal = Some(path.clone());
    for folder in folders {
        load_folder(tab, folder, cx);
    }
    if let Some(open) = tab.ui.browse.open.as_mut() {
        rebuild(open);
    }
    choose(tab, path, cx);
}

fn load_folder(tab: &mut Tab, folder: PathBuf, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    if matches!(open.folders.get(&folder), Some(Listing::Loading | Listing::Loaded(_))) {
        return;
    }
    open.folders.insert(folder.clone(), Listing::Loading);
    rebuild(open);
    read_folder(tab, folder, cx);
}

fn read_folder(tab: &mut Tab, folder: PathBuf, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let ticket = tab.ui.browse.ticket;
    let Some(open) = tab.ui.browse.open.as_ref() else { return };
    let rev = open.rev.clone();
    let epoch = tab.epoch;
    let asked = folder.clone();
    Tab::work(
        cx,
        move || repo.browse_folder(&rev, &asked),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.browse.ticket != ticket {
                return;
            }
            let Some(open) = tab.ui.browse.open.as_mut() else { return };
            let listing = match result {
                Ok(entries) => Listing::Loaded(entries),
                Err(reason) => Listing::Failed(reason),
            };
            open.folders.insert(folder, listing);
            rebuild(open);
            cx.notify();
        },
    );
}

fn load_every(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let ticket = tab.ui.browse.ticket;
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    let link = Link::new(true);
    open.link = Some(link.clone());
    open.every = Every::Loading;
    let rev = open.rev.clone();
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || repo.browse_paths(&rev, &link, LIMIT),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.browse.ticket != ticket {
                return;
            }
            let Some(open) = tab.ui.browse.open.as_mut() else { return };
            open.link = None;
            open.every = match result {
                Ok((paths, capped)) => Every::Loaded { paths: Arc::new(paths), capped },
                Err(reason) => Every::Failed(format!("The file list could not be read. {reason}")),
            };
            rebuild(open);
            cx.notify();
        },
    );
}

fn typed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let text = tab.ui.browse.filter.read(cx).text().trim().to_owned();
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    if open.query == text {
        return;
    }
    open.query = text;
    let wanted = !open.query.is_empty() && matches!(open.every, Every::Idle | Every::Failed(_));
    if wanted {
        load_every(tab, cx);
    }
    if let Some(open) = tab.ui.browse.open.as_mut() {
        rebuild(open);
    }
    cx.notify();
}

fn toggle(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    open.cursor = Some(path.clone());
    if !open.query.is_empty() {
        if !open.folded.remove(&path) {
            open.folded.insert(path);
        }
        rebuild(open);
        return;
    }
    if open.expanded.remove(&path) {
        rebuild(open);
        return;
    }
    open.expanded.insert(path.clone());
    if matches!(open.folders.get(&path), Some(Listing::Failed(_))) {
        open.folders.remove(&path);
    }
    rebuild(open);
    load_folder(tab, path, cx);
}

fn choose(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    open.cursor = Some(path.clone());
    if open.chosen.as_ref() == Some(&path) && tab.ui.diff.file.is_some() {
        return;
    }
    open.chosen = Some(path);
    load_file(tab, None, cx);
}

fn unchoose(tab: &mut Tab) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    if open.chosen.take().is_none() {
        return;
    }
    tab.ui.browse.file_ticket += 1;
    tab.clear_diff();
    tab.ui.browse.placed = Some(tab.diff_ticket);
    tab.ui.diff_options.measured = px(0.);
}

fn close(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    let Some((owner, diff_open)) = tab.ui.browse.close() else { return };
    tab.ui.diff_options.measured = px(0.);
    tab.ui.diff_open = diff_open;
    tab.clear_diff();
    if tab.ui.selection == owner {
        crate::diff_options::reload(tab, cx);
    }
    view::focus(&tab.ui, window);
}

fn load_file(tab: &mut Tab, place: Option<Point<Pixels>>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let face_tab = tab.ui.diff_text.face.tab;
    let state = &mut tab.ui.browse;
    state.file_ticket += 1;
    let ticket = state.file_ticket;
    let Some(open) = state.open.as_ref() else { return };
    let Some(path) = open.chosen.clone() else { return };
    let rev = open.rev.clone();
    let owner = open.owner.clone().unwrap_or(Selection::Changes);
    tab.diff_ticket += 1;
    tab.diff = None;
    let mut waiting = DiffDoc::default();
    waiting.state = PatchState::Loading;
    tab.ui.replace_diff(waiting, DiffLayout::default());
    tab.ui.diff_title = path.to_string_lossy().into_owned();
    crate::image_diff::follow(tab, None, crate::image_diff::owner(&owner, None));
    crate::lines::reloaded(&mut tab.ui);
    crate::diff_select::reloaded(&mut tab.ui);
    tab.ui.diff_options.measured = px(0.);
    let epoch = tab.epoch;
    let diff_ticket = tab.diff_ticket;
    tab.ui.browse.placed = Some(diff_ticket);
    Tab::work(
        cx,
        move || repo.browsed_file(&rev, &path, &owner, face_tab),
        move |tab, (doc, layout), cx| {
            if tab.epoch != epoch || tab.ui.browse.file_ticket != ticket || tab.diff_ticket != diff_ticket {
                return;
            }
            let shown_tab = tab.ui.diff_text.face.tab;
            let layout = if shown_tab == face_tab { layout } else { DiffLayout::new(&doc, shown_tab) };
            tab.diff_ticket += 1;
            tab.ui.browse.placed = Some(tab.diff_ticket);
            tab.ui.replace_diff(doc, layout);
            if let Some(place) = place {
                tab.ui.diff_scroll.0.borrow().base_handle.set_offset(place);
            }
            if let Some(open) = tab.ui.browse.open.as_mut() {
                open.stamp = match (&open.rev, &open.chosen, &tab.repo) {
                    (Rev::WorkingTree, Some(path), Some(repo)) => crate::image_diff::stamp(&repo.root.join(path)),
                    _ => None,
                };
            }
            crate::lines::reloaded(&mut tab.ui);
            crate::diff_select::reloaded(&mut tab.ui);
            crate::find::reloaded(&mut tab.ui);
            if tab.ui.syntax.on {
                crate::syntax::start(tab, cx);
            }
            cx.notify();
        },
    );
}

fn cursor_index(ui: &Ui) -> Option<usize> {
    let wanted = ui.browse.cursor().or(ui.browse.chosen())?;
    ui.browse.rows().iter().position(|row| row.path() == Some(wanted))
}

fn cursor_row(ui: &Ui) -> Option<&Row> {
    ui.browse.rows().get(cursor_index(ui)?)
}

fn land(tab: &mut Tab, at: usize, cx: &mut Context<Tab>) {
    let Some(open) = tab.ui.browse.open.as_mut() else { return };
    let Some(Row::Entry { path, kind, .. }) = open.rows.get(at).cloned() else { return };
    open.scroll.scroll_to_item(at, ScrollStrategy::Center);
    open.cursor = Some(path.clone());
    if matches!(kind, Kind::File | Kind::Link) {
        choose(tab, path, cx);
    }
}

fn step(tab: &mut Tab, delta: isize, cx: &mut Context<Tab>) {
    let rows = tab.ui.browse.rows();
    let entries: Vec<usize> = (0..rows.len()).filter(|&at| rows[at].path().is_some()).collect();
    let Some(&last) = entries.last() else { return };
    let now = cursor_index(&tab.ui).and_then(|at| entries.iter().position(|&entry| entry == at));
    let wanted = match (delta, now) {
        (isize::MIN, _) => 0,
        (isize::MAX, _) => entries.len() - 1,
        (_, Some(now)) => now.saturating_add_signed(delta).min(entries.len() - 1),
        (delta, None) if delta < 0 => entries.len() - 1,
        _ => 0,
    };
    let at = entries.get(wanted).copied().unwrap_or(last);
    land(tab, at, cx);
}

fn sideways(tab: &mut Tab, right: bool, cx: &mut Context<Tab>) {
    let Some(at) = cursor_index(&tab.ui) else {
        step(tab, 0, cx);
        return;
    };
    let Some(Row::Entry { path, kind, unfolded, depth, .. }) = tab.ui.browse.rows().get(at).cloned() else { return };
    match (right, kind, unfolded) {
        (true, Kind::Folder, false) => toggle(tab, path, cx),
        (true, Kind::Folder, true) => {
            if tab.ui.browse.rows().get(at + 1).is_some_and(|row| row.path().is_some()) {
                land(tab, at + 1, cx);
            }
        }
        (true, Kind::File | Kind::Link, _) => choose(tab, path, cx),
        (false, Kind::Folder, true) => toggle(tab, path, cx),
        (false, _, _) if depth > 0 => {
            let above = tab.ui.browse.rows()[..at].iter().rposition(|row| matches!(row, Row::Entry { depth: d, kind: Kind::Folder, .. } if *d < depth));
            if let Some(above) = above {
                let Some(open) = tab.ui.browse.open.as_mut() else { return };
                open.cursor = open.rows[above].path().map(Path::to_path_buf);
                open.scroll.scroll_to_item(above, ScrollStrategy::Center);
            }
        }
        _ => {}
    }
}

pub(crate) fn history_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> impl IntoElement + use<T> {
    let refused = refusal(ui);
    kit::btn("browse-history", "History")
        .small()
        .ghost()
        .enabled(refused.is_none())
        .tip(refused.unwrap_or_else(|| "Every commit that changed this file".to_owned()))
        .on(view::Action::Browse(Action::History), cx)
}

pub(crate) fn blame_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> impl IntoElement + use<T> {
    let refused = refusal(ui);
    kit::btn("browse-blame", "Blame")
        .small()
        .ghost()
        .enabled(refused.is_none())
        .tip(refused.unwrap_or_else(|| "Who last changed each line".to_owned()))
        .on(view::Action::Browse(Action::Blame), cx)
}

pub(crate) fn heading(ui: &Ui) -> String {
    let subject = ui.browse.open.as_ref().and_then(|open| open.subject.as_deref());
    match ui.browse.rev() {
        Some(Rev::Commit(oid)) => {
            let short: String = oid.chars().take(7).collect();
            match subject {
                Some(subject) => format!("Files at {short} · {subject}"),
                None => format!("Files at {short}"),
            }
        }
        Some(Rev::WorkingTree) => "Files in your working copy".to_owned(),
        None => String::new(),
    }
}

fn entry_row<T: Desk>(index: usize, row: &Row, chosen: bool, at: bool, cx: &mut Context<T>) -> gpui::AnyElement {
    match row {
        Row::Note { depth, text, loading } => div()
            .h(whole_px(FILE_ROW))
            .flex()
            .items_center()
            .gap_2()
            .pl(px(ROW_START + *depth as f32 * STEP_INDENT + CHEVRON))
            .pr_2()
            .when(*loading, |d| d.child(kit::spinner(("browse-loading", index))))
            .child(kit::fit(text.clone()).size(TEXT_SMALL).color(palette().text_3).grow())
            .into_any_element(),
        Row::Entry { path, name, kind, depth, unfolded, ignored, count } => {
            let icon = match kind {
                Kind::Folder => "folder",
                Kind::Module => "commit",
                Kind::File | Kind::Link => "file",
            };
            let (shown, kind_of, ignored_too) = (path.clone(), *kind, *ignored);
            let tip = move || match kind_of {
                Kind::Module => format!("{}\n{MODULE}\nDouble-click to open it in a new tab.", shown.display()),
                Kind::Link => format!("{}\nA symbolic link", shown.display()),
                _ if ignored_too => format!("{}\nIgnored", shown.display()),
                _ => shown.to_string_lossy().into_owned(),
            };
            let action = match kind {
                Kind::Folder => Some(Action::Toggle(path.clone())),
                Kind::File | Kind::Link => Some(Action::Choose(path.clone())),
                Kind::Module => None,
            };
            let ink = if *ignored || *kind == Kind::Module { palette().text_3 } else if *kind == Kind::Folder { palette().text_2 } else { palette().text };
            let chevron = match kind {
                Kind::Folder => kit::icon(if *unfolded { "chevron-down" } else { "chevron-right" })
                    .size(px(12.))
                    .text_color(rgb(palette().text_3))
                    .into_any_element(),
                _ => div().w(px(12.)).flex_shrink_0().into_any_element(),
            };
            kit::cursor(kit::row(("browse-row", index), chosen), at && !chosen)
                .h(whole_px(FILE_ROW))
                .pl(px(ROW_START + *depth as f32 * STEP_INDENT))
                .pr_2()
                .gap(px(5.))
                .child(chevron)
                .child(kit::icon(icon).size(px(13.)).text_color(rgb(palette().text_3)))
                .child(
                    div()
                        .id(("browse-name", index))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(kit::fit(name.clone()).size(12.5).color(ink).cut(kit::Cut::Middle).grow())
                        .tooltip(kit::tip_with(tip)),
                )
                .when(*ignored, |d| d.child(div().flex_shrink_0().text_size(px(TEXT_TINY)).text_color(rgb(palette().text_3)).child("Ignored")))
                .when_some(*count, |d, count| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_TINY))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(palette().text_3))
                            .child(count.to_string()),
                    )
                })
                .when_some(action, |d, action| {
                    d.on_click(cx.listener(move |app, _, w, cx| app.dispatch(view::Action::Browse(action.clone()), w, cx)))
                })
                .when(*kind == Kind::Module, |d| {
                    let opened = path.clone();
                    d.on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                        if event.click_count() > 1 {
                            app.dispatch(view::Action::Submodule(crate::submodule::Action::Open(opened.clone())), w, cx)
                        }
                    }))
                })
                .into_any_element()
        }
    }
}

fn footer(ui: &Ui) -> Option<String> {
    if ui.browse.query().is_empty() {
        return None;
    }
    if ui.browse.searching() {
        return Some("Reading every path…".to_owned());
    }
    let shown = ui.browse.rows().iter().filter(|row| matches!(row, Row::Entry { kind: Kind::File, .. })).count();
    let found = match shown {
        1 => "1 file matches".to_owned(),
        n => format!("{n} files match"),
    };
    Some(if ui.browse.capped() { format!("{found}. {CAPPED}") } else { found })
}

fn tree<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(open) = ui.browse.open.as_ref() else { return div() };
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
        .child(kit::icon("folder").size(px(14.)).flex_shrink_0().text_color(rgb(palette().text_3)))
        .child(kit::fit(heading(ui)).size(12.5).weight(FontWeight::SEMIBOLD).grow())
        .child(kit::icon_btn("close-browse", "x", "Back to the graph", view::Action::Browse(Action::Close), cx));
    let field = div().px_2().py(px(6.)).flex_shrink_0().border_b_1().border_color(rgb(palette().line)).child(ui.browse.filter.clone());
    let count = open.rows.len();
    let list = uniform_list(
        SharedString::from(format!("browse-rows-{:?}", open.rev)),
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let chosen = ui.browse.chosen().map(Path::to_path_buf);
            let at = cursor_index(ui);
            range
                .filter_map(|i| {
                    let row = ui.browse.rows().get(i)?;
                    let picked = chosen.is_some() && row.path() == chosen.as_deref();
                    Some(entry_row(i, row, picked, at == Some(i), cx))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(open.scroll.clone())
    .flex_1()
    .min_h_0();
    div()
        .w(px(LIST_WIDTH))
        .max_w(relative(0.45))
        .flex_shrink_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(palette().canvas))
        .border_r_1()
        .border_color(rgb(palette().line))
        .child(header)
        .child(field)
        .child(list)
        .child(kit::scrollbar("browse-scrollbar", &open.scroll))
        .when_some(footer(ui), |d, text| {
            d.child(div().flex_shrink_0().px_3().py_2().border_t_1().border_color(rgb(palette().line)).child(kit::muted(text)))
        })
}

pub(crate) fn render<T: Desk>(app: &T, window: &Window, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let pane = if shows_file(ui) {
        view::diff_pane(app, window, cx)
    } else {
        let text = match ui.browse.rev() {
            Some(Rev::Commit(_)) => "Choose a file to see it as it was at this commit.",
            _ => "Choose a file to see it.",
        };
        div().flex_1().min_w_0().min_h_0().flex().items_center().justify_center().child(kit::muted(text))
    };
    div().flex_1().min_w_0().min_h_0().flex().child(tree(app, cx)).child(pane)
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
    }

    fn id(f: &Fixture, spec: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", spec])).unwrap().trim().to_owned()
    }

    fn untouched(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (f.git(&["status", "--porcelain=v1", "-uall", "--ignored"]), f.git(&["ls-files", "-s"]), f.git(&["for-each-ref"]))
    }

    fn listed(entries: &[Entry]) -> Vec<(String, Kind, bool)> {
        entries.iter().map(|entry| (entry.path.to_string_lossy().into_owned(), entry.kind, entry.ignored)).collect()
    }

    #[test]
    fn a_commit_lists_one_folder_at_a_time_with_folders_first_and_keeps_files_deleted_since() {
        let f = Fixture::new();
        write(&f, "notes/old-map.txt", "Old map\n");
        write(&f, "notes/Lake.txt", "Lake\n");
        write(&f, "camp.txt", "Camp\n");
        write(&f, "Zebra/z.txt", "Z\n");
        std::os::unix::fs::symlink("camp.txt", f.dir.join("link")).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        let first = id(&f, "HEAD");
        f.git(&["rm", "-q", "notes/old-map.txt"]);
        f.git(&["commit", "-qm", "second"]);
        let before = untouched(&f);
        let at = Rev::Commit(first.clone());
        let root = f.repo.browse_folder(&at, Path::new("")).unwrap();
        assert_eq!(listed(&root), [
            ("notes".to_owned(), Kind::Folder, false),
            ("Zebra".to_owned(), Kind::Folder, false),
            ("camp.txt".to_owned(), Kind::File, false),
            ("link".to_owned(), Kind::Link, false),
        ]);
        let notes = f.repo.browse_folder(&at, Path::new("notes")).unwrap();
        assert_eq!(listed(&notes), [
            ("notes/Lake.txt".to_owned(), Kind::File, false),
            ("notes/old-map.txt".to_owned(), Kind::File, false),
        ]);
        let now = f.repo.browse_folder(&Rev::Commit(id(&f, "HEAD")), Path::new("notes")).unwrap();
        assert_eq!(listed(&now), [("notes/Lake.txt".to_owned(), Kind::File, false)]);
        let (doc, _) = f.repo.browsed_file(&at, Path::new("notes/old-map.txt"), &Selection::Commit(first.clone()), 4);
        assert_eq!(doc.state, PatchState::Text);
        let shown = doc.file.as_deref().unwrap();
        assert_eq!(shown.contents.as_deref(), Some("Old map\n"));
        assert_eq!(shown.version, Version::Changed);
        assert!(shown.marks.iter().all(Option::is_none) && !shown.working);
        let (missing, _) = f.repo.browsed_file(&Rev::Commit(id(&f, "HEAD")), Path::new("notes/old-map.txt"), &Selection::Changes, 4);
        assert!(matches!(missing.state, PatchState::Unreadable(_)), "{:?}", missing.state);
        assert!(f.repo.browse_folder(&at, Path::new("nowhere")).is_err());
        assert!(f.repo.browse_folder(&at, Path::new("../outside")).is_err());
        assert!(f.repo.browse_folder(&Rev::Commit("HEAD".to_owned()), Path::new("")).is_err());
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn the_working_tree_is_read_from_disk_and_marks_ignored_files_without_changing_anything() {
        let f = Fixture::new();
        write(&f, ".gitignore", "build/\n*.log\n");
        write(&f, "src/main.rs", "fn main() {}\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        write(&f, "build/out.o", "binary\n");
        write(&f, "debug.log", "log\n");
        write(&f, "fresh.txt", "new\n");
        fs::create_dir_all(f.dir.join("vendor/lib/.git")).unwrap();
        let before = untouched(&f);
        let root = f.repo.browse_folder(&Rev::WorkingTree, Path::new("")).unwrap();
        assert_eq!(listed(&root), [
            ("build".to_owned(), Kind::Folder, true),
            ("src".to_owned(), Kind::Folder, false),
            ("vendor".to_owned(), Kind::Folder, false),
            (".gitignore".to_owned(), Kind::File, false),
            ("debug.log".to_owned(), Kind::File, true),
            ("fresh.txt".to_owned(), Kind::File, false),
        ]);
        let build = f.repo.browse_folder(&Rev::WorkingTree, Path::new("build")).unwrap();
        assert_eq!(listed(&build), [("build/out.o".to_owned(), Kind::File, true)]);
        let vendor = f.repo.browse_folder(&Rev::WorkingTree, Path::new("vendor")).unwrap();
        assert_eq!(listed(&vendor), [("vendor/lib".to_owned(), Kind::Module, false)]);
        assert_eq!(untouched(&f), before);
        write(&f, "fresh.txt", "newer\n");
        let (doc, _) = f.repo.browsed_file(&Rev::WorkingTree, Path::new("fresh.txt"), &Selection::Changes, 4);
        let shown = doc.file.as_deref().unwrap();
        assert_eq!(shown.contents.as_deref(), Some("newer\n"));
        assert!(shown.working && shown.version == Version::Working);
        let before = untouched(&f);
        let (paths, capped) = f.repo.browse_paths(&Rev::WorkingTree, &Link::new(true), LIMIT).unwrap();
        assert!(!capped);
        let paths: Vec<String> = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, [".gitignore", "fresh.txt", "src/main.rs"]);
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn a_working_folder_with_names_that_look_like_pathspec_magic_still_lists_and_marks_ignored_files() {
        let f = Fixture::new();
        write(&f, ".gitignore", "*.log\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        write(&f, ":(exclude)odd.txt", "odd\n");
        write(&f, ":!bang.log", "bang\n");
        write(&f, "debug.log", "log\n");
        let root = f.repo.browse_folder(&Rev::WorkingTree, Path::new("")).unwrap();
        assert_eq!(listed(&root), [
            (".gitignore".to_owned(), Kind::File, false),
            (":!bang.log".to_owned(), Kind::File, true),
            (":(exclude)odd.txt".to_owned(), Kind::File, false),
            ("debug.log".to_owned(), Kind::File, true),
        ]);
    }

    #[test]
    fn a_list_exactly_as_long_as_the_limit_is_not_capped() {
        let f = Fixture::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            write(&f, name, "x\n");
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        let (all, capped) = f.repo.tracked_paths(&Link::new(true), 3).unwrap();
        assert_eq!((all.len(), capped), (3, false));
        let (some, capped) = f.repo.tracked_paths(&Link::new(true), 2).unwrap();
        assert_eq!((some.len(), capped), (2, true));
        let head = Rev::Commit(id(&f, "HEAD"));
        let (all, capped) = f.repo.browse_paths(&head, &Link::new(true), 3).unwrap();
        assert_eq!((all.len(), capped), (3, false));
    }

    #[test]
    fn the_tracked_list_leaves_out_untracked_and_ignored_files_and_changes_nothing() {
        let f = Fixture::new();
        write(&f, ".gitignore", "*.log\n");
        write(&f, "src/main.rs", "fn main() {}\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        write(&f, "staged.txt", "staged\n");
        f.git(&["add", "staged.txt"]);
        write(&f, "loose.txt", "loose\n");
        write(&f, "debug.log", "log\n");
        let before = untouched(&f);
        let (paths, capped) = f.repo.tracked_paths(&Link::new(true), LIMIT).unwrap();
        assert!(!capped);
        assert_eq!(paths, [PathBuf::from(".gitignore"), PathBuf::from("src/main.rs"), PathBuf::from("staged.txt")]);
        let (one, capped) = f.repo.tracked_paths(&Link::new(true), 1).unwrap();
        assert!(capped);
        assert_eq!(one, [PathBuf::from(".gitignore")]);
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn the_full_list_is_capped_and_filtered_into_a_tree() {
        let f = Fixture::new();
        for name in ["a/one.txt", "a/two.txt", "b/c/map.txt", "b/c/maple.txt", "top.txt"] {
            write(&f, name, "x\n");
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        let head = Rev::Commit(id(&f, "HEAD"));
        let (all, capped) = f.repo.browse_paths(&head, &Link::new(true), LIMIT).unwrap();
        assert!(!capped);
        assert_eq!(all.len(), 5);
        let (some, capped) = f.repo.browse_paths(&head, &Link::new(true), 2).unwrap();
        assert!(capped);
        assert_eq!(some, [PathBuf::from("a/one.txt"), PathBuf::from("a/two.txt")]);
        let rows = found(&all, "MAP", &BTreeSet::new());
        let drawn: Vec<(String, usize, Option<usize>)> = rows
            .iter()
            .map(|row| match row {
                Row::Entry { name, depth, count, .. } => (name.clone(), *depth, *count),
                Row::Note { text, .. } => (text.clone(), 0, None),
            })
            .collect();
        assert_eq!(drawn, [("b/c".to_owned(), 0, Some(2)), ("map.txt".to_owned(), 1, None), ("maple.txt".to_owned(), 1, None)]);
        let folded = BTreeSet::from([PathBuf::from("b/c")]);
        assert_eq!(found(&all, "map", &folded).len(), 1);
        assert_eq!(found(&all, "nowhere", &BTreeSet::new()), [Row::Note { depth: 0, text: "No files match “nowhere”.".to_owned(), loading: false }]);
    }

    fn running(probe: &str) -> bool {
        std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", probe])
            .output()
            .is_ok_and(|output| !output.stdout.is_empty())
    }

    #[test]
    fn closing_the_browser_stops_the_git_process_that_reads_every_path() {
        let f = Fixture::new();
        write(&f, "camp.txt", "Camp\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        let tip = id(&f, "HEAD");
        let alternates = f.dir.join(".git/objects/info/alternates");
        fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        let fifo = std::ffi::CString::new(alternates.to_string_lossy().into_owned()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let link = Link::new(true);
        let mut open = Open::new(Rev::Commit(tip.clone()), None, false);
        open.every = Every::Loading;
        open.link = Some(link.clone());
        let reader = {
            let (repo, link, rev) = (f.repo.clone(), link.clone(), Rev::Commit(tip.clone()));
            std::thread::spawn(move || repo.browse_paths(&rev, &link, LIMIT))
        };
        let probe = format!("ls-tree -r -z --name-only {tip}");
        let waited = std::time::Instant::now();
        while !running(&probe) {
            assert!(waited.elapsed() < std::time::Duration::from_secs(10), "git ls-tree never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let closed = std::time::Instant::now();
        drop(open);
        assert!(link.cancelled());
        let result = reader.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(closed.elapsed() < std::time::Duration::from_secs(3));
        assert!(!running(&probe), "git ls-tree is still running after the browser closed");
        fs::remove_file(&alternates).unwrap();
    }
}
