mod backend;
use crate::{
    Tab, kit,
    theme::*,
    view::{self, Desk},
};
pub(crate) use backend::{Choice, LIMIT, Merged, Side, marker, marker_width};
use backend::{Choices, Document, LinePair, Place, Preview, ResultRow};
use crate::operation::{Kind, Snapshot};
use gpui::{
    Context, Div, FontWeight, ScrollHandle, ScrollStrategy, UniformListScrollHandle, Window, div, prelude::*,
    px, rgb, uniform_list,
};
use crate::{
    diff_select::Column,
    diff_text::Face,
    find::Pane,
    syntax::Spans,
};
use std::{
    collections::HashMap,
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Mark, Submenu, Target, busy, on_repo, operating};
    use view::Ui;
    fn merge_into(ui: &Ui, own: impl FnOnce() -> Option<String>) -> Availability {
        Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
            (ui.head.is_none() || ui.detached).then(|| "Check out a branch to merge into".to_owned())
        }).or_else(own))
    }
    fn on_branch(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        let Target::Branch(name) = target else {
            return Availability::Hidden;
        };
        merge_into(ui, || (*name == *ui.branch).then(|| format!("{} is the checked-out branch", ui.branch)))
    }
    fn on_remote(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        match target {
            Target::RemoteBranch(_) if ui.refs.oid_of(target).is_some() => merge_into(ui, || None),
            _ => Availability::Hidden,
        }
    }
    fn on_tag(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        match target {
            Target::Tag(_) if ui.refs.oid_of(target).is_some() => merge_into(ui, || None),
            _ => Availability::Hidden,
        }
    }
    fn on_commit(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        match target {
            Target::Commit(oid) => merge_into(ui, || {
                (ui.head.as_deref() == Some(oid.as_str())).then(|| format!("This commit is the tip of {}", ui.branch))
            }),
            _ => Availability::Hidden,
        }
    }
    fn title<const M: usize>(_: &Ui, _: &Target) -> String {
        Mode::ALL[M].title().to_owned()
    }
    fn run<const M: usize>(ui: &Ui, target: &Target) -> Option<view::Action> {
        let source = Source::of(target)?;
        ui.refs.oid_of(target).or(crate::commands::commit(target))?;
        Some(view::Action::Merge(Action::Start(source, Mode::ALL[M])))
    }
    fn mark<const M: usize>(ui: &Ui, _: &Target) -> Option<Mark> {
        let mode = Mode::ALL[M];
        Some(if ui.merge.mode == mode { Mark::Default } else { Mark::Offer(view::Action::Merge(Action::SetDefault(mode))) })
    }
    fn default_title<const M: usize>(_: &Ui, _: &Target) -> String {
        format!("Default merge: {}", Mode::ALL[M].title())
    }
    fn default_when<const M: usize>(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless((ui.merge.mode == Mode::ALL[M]).then(|| "This is already how merges run here".to_owned()))
    }
    fn set_default<const M: usize>(_: &Ui, _: &Target) -> Option<view::Action> {
        Some(view::Action::Merge(Action::SetDefault(Mode::ALL[M])))
    }
    fn resolving(ui: &Ui, target: &Target, _: &gpui::App) -> Availability {
        if *target == Target::Repo && ui.merge.editing().is_some() && view::centre(ui) == view::Centre::Resolver {
            Availability::Enabled
        } else {
            Availability::Hidden
        }
    }
    const BRANCH: Submenu = Submenu {
        id: "branch.merge",
        title: |ui, target| match target {
            Target::Branch(name) if *name == *ui.branch => format!("Merge into {}", ui.branch),
            Target::Branch(name) => format!("Merge {} into {}", name.to_string_lossy(), ui.branch),
            _ => String::new(),
        },
        icon: "merge",
    };
    const REMOTE: Submenu = Submenu {
        id: "branch.merge-remote",
        title: |ui, target| match target {
            Target::RemoteBranch(name) => format!("Merge {} into {}", name.to_string_lossy(), ui.branch),
            _ => String::new(),
        },
        icon: "merge",
    };
    const TAG: Submenu = Submenu {
        id: "tag.merge",
        title: |ui, target| match target {
            Target::Tag(name) => format!("Merge {} into {}", name.to_string_lossy(), ui.branch),
            _ => String::new(),
        },
        icon: "merge",
    };
    const COMMIT: Submenu = Submenu {
        id: "commit.merge",
        title: |ui, _| format!("Merge this commit into {}", ui.branch),
        icon: "merge",
    };
    const WORDS: [&[&str]; 4] = [
        &["integrate", "fast forward"],
        &["integrate", "no-ff", "merge commit"],
        &["integrate", "ff-only"],
        &["integrate", "squash merge", "combine commits"],
    ];
    &[
        Command::new("branch.merge-ff", Group::Switch, title::<0>, on_branch, run::<0>).under(&BRANCH).mark(mark::<0>).words(WORDS[0]).icon("merge"),
        Command::new("branch.merge-no-ff", Group::Switch, title::<1>, on_branch, run::<1>).under(&BRANCH).mark(mark::<1>).words(WORDS[1]).icon("merge"),
        Command::new("branch.merge-ff-only", Group::Switch, title::<2>, on_branch, run::<2>).under(&BRANCH).mark(mark::<2>).words(WORDS[2]).icon("merge"),
        Command::new("branch.merge-squash", Group::Switch, title::<3>, on_branch, run::<3>).under(&BRANCH).mark(mark::<3>).words(WORDS[3]).icon("merge"),
        Command::new("branch.merge-remote-ff", Group::Switch, title::<0>, on_remote, run::<0>).under(&REMOTE).mark(mark::<0>).words(WORDS[0]).icon("merge"),
        Command::new("branch.merge-remote-no-ff", Group::Switch, title::<1>, on_remote, run::<1>).under(&REMOTE).mark(mark::<1>).words(WORDS[1]).icon("merge"),
        Command::new("branch.merge-remote-ff-only", Group::Switch, title::<2>, on_remote, run::<2>).under(&REMOTE).mark(mark::<2>).words(WORDS[2]).icon("merge"),
        Command::new("branch.merge-remote-squash", Group::Switch, title::<3>, on_remote, run::<3>).under(&REMOTE).mark(mark::<3>).words(WORDS[3]).icon("merge"),
        Command::new("tag.merge-ff", Group::Switch, title::<0>, on_tag, run::<0>).under(&TAG).mark(mark::<0>).words(WORDS[0]).icon("merge"),
        Command::new("tag.merge-no-ff", Group::Switch, title::<1>, on_tag, run::<1>).under(&TAG).mark(mark::<1>).words(WORDS[1]).icon("merge"),
        Command::new("tag.merge-ff-only", Group::Switch, title::<2>, on_tag, run::<2>).under(&TAG).mark(mark::<2>).words(WORDS[2]).icon("merge"),
        Command::new("tag.merge-squash", Group::Switch, title::<3>, on_tag, run::<3>).under(&TAG).mark(mark::<3>).words(WORDS[3]).icon("merge"),
        Command::new("commit.merge-ff", Group::Switch, title::<0>, on_commit, run::<0>).under(&COMMIT).mark(mark::<0>).words(WORDS[0]).icon("merge"),
        Command::new("commit.merge-no-ff", Group::Switch, title::<1>, on_commit, run::<1>).under(&COMMIT).mark(mark::<1>).words(WORDS[1]).icon("merge"),
        Command::new("commit.merge-ff-only", Group::Switch, title::<2>, on_commit, run::<2>).under(&COMMIT).mark(mark::<2>).words(WORDS[2]).icon("merge"),
        Command::new("commit.merge-squash", Group::Switch, title::<3>, on_commit, run::<3>).under(&COMMIT).mark(mark::<3>).words(WORDS[3]).icon("merge"),
        Command::new("repo.merge-default-ff", Group::Switch, default_title::<0>, default_when::<0>, set_default::<0>).words(&["merge mode", "fast forward"]),
        Command::new("repo.merge-default-no-ff", Group::Switch, default_title::<1>, default_when::<1>, set_default::<1>).words(&["merge mode", "no-ff", "merge commit"]),
        Command::new("repo.merge-default-ff-only", Group::Switch, default_title::<2>, default_when::<2>, set_default::<2>).words(&["merge mode", "ff-only"]),
        Command::new("repo.merge-default-squash", Group::Switch, default_title::<3>, default_when::<3>, set_default::<3>).words(&["merge mode", "squash merge"]),
        Command::new(
            "conflict.whole-file",
            Group::View,
            |ui, _| if ui.merge.whole_file { "Show only the conflict in the resolver" } else { "Show whole files in the resolver" }.into(),
            resolving,
            |ui, _| Some(view::Action::Merge(Action::WholeFile(!ui.merge.whole_file))),
        )
        .words(&["merge tool", "full file", "context", "conflict"]),
        Command::new(
            "conflict.show-original",
            Group::View,
            |ui, _| if ui.merge.original { "Hide the original lines" } else { "Show the original lines of the conflict" }.into(),
            resolving,
            |ui, _| Some(view::Action::Merge(Action::ShowOriginal(!ui.merge.original))),
        )
        .words(&["ancestor", "base", "merge base", "conflict"]),
    ]
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    #[default]
    FastForwardIfPossible,
    NoFastForward,
    FastForwardOnly,
    Squash,
}

impl Mode {
    pub(crate) const ALL: [Mode; 4] = [Mode::FastForwardIfPossible, Mode::NoFastForward, Mode::FastForwardOnly, Mode::Squash];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Mode::FastForwardIfPossible => "Fast-forward if possible",
            Mode::NoFastForward => "Always create a merge commit",
            Mode::FastForwardOnly => "Fast-forward only",
            Mode::Squash => "Squash into one change",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Branch(OsString),
    RemoteBranch(OsString),
    Tag(OsString),
    Commit(String),
}

impl Source {
    pub(crate) fn of(target: &crate::commands::Target) -> Option<Self> {
        use crate::commands::Target;
        Some(match target {
            Target::Branch(name) => Self::Branch(name.clone()),
            Target::RemoteBranch(name) => Self::RemoteBranch(name.clone()),
            Target::Tag(name) => Self::Tag(name.clone()),
            Target::Commit(oid) => Self::Commit(oid.clone()),
            _ => return None,
        })
    }

    pub(crate) fn full(&self) -> OsString {
        let (prefix, name) = match self {
            Self::Branch(name) => ("refs/heads/", name),
            Self::RemoteBranch(name) => ("refs/remotes/", name),
            Self::Tag(name) => ("refs/tags/", name),
            Self::Commit(oid) => return OsString::from(oid),
        };
        let mut full = OsString::from(prefix);
        full.push(name);
        full
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Branch(name) | Self::RemoteBranch(name) | Self::Tag(name) => name.to_string_lossy().into_owned(),
            Self::Commit(oid) => crate::notice::short(oid).to_owned(),
        }
    }

    pub(crate) fn shown(&self, ui: &view::Ui) -> Option<String> {
        use crate::commands::Target;
        let target = match self {
            Self::Branch(name) => Target::Branch(name.clone()),
            Self::RemoteBranch(name) => Target::RemoteBranch(name.clone()),
            Self::Tag(name) => Target::Tag(name.clone()),
            Self::Commit(oid) => return Some(oid.clone()),
        };
        ui.refs.oid_of(&target).map(str::to_owned)
    }
}

pub(crate) fn outcome(merged: &Merged, current: &str, source: &str) -> crate::notice::Outcome {
    let title = match merged {
        Merged::Nothing => "Already up to date".to_owned(),
        Merged::Squashed(_) => format!("Staged {source} as one change"),
        _ => format!("Merged {source} into {current}"),
    };
    crate::notice::Outcome::titled(title, merged.sentence(current, source))
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Start(Source, Mode),
    SetDefault(Mode),
    Open(PathBuf),
    Hunk(usize),
    Choose(usize, Side),
    ChooseAll(Side),
    ChooseLine(usize, usize, Choice),
    ToggleLine(usize, usize, Side),
    WholeFile(bool),
    ShowOriginal(bool),
    Save,
    Close,
}

struct Editor {
    document: Arc<Document>,
    choices: Choices,
    hunk: usize,
    preview: Preview,
    unresolved: usize,
    widest: usize,
    tabs: usize,
    revision: u64,
    colours: HashMap<usize, Spans>,
    coloured: bool,
}

const SHOWN: usize = 2048;

fn line_of(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

pub(crate) fn shown_length(code: &str) -> usize {
    if code.len() <= SHOWN { code.len() } else { code.floor_char_boundary(SHOWN) }
}

fn display_line(line: &str) -> String {
    let line = line_of(line);
    let cut = shown_length(line);
    if cut == line.len() { line.to_owned() } else { format!("{}…", &line[..cut]) }
}

impl Editor {
    fn new(document: Document) -> Self {
        let choices = document.new_choices();
        let preview = document.preview(&choices);
        let unresolved = document.unresolved(&choices);
        let widest = document.source.lines().map(|line| line.chars().count().min(SHOWN)).max().unwrap_or(0);
        let tabs = document.source.lines().map(|line| line.matches('\t').count()).max().unwrap_or(0);
        Self {
            document: Arc::new(document),
            choices,
            hunk: 0,
            preview,
            unresolved,
            widest,
            tabs,
            revision: 0,
            colours: HashMap::new(),
            coloured: false,
        }
    }

    fn width(&self, face: &Face) -> f32 {
        (self.widest + self.tabs * face.tab.saturating_sub(1)) as f32 * face.size as f32 * 0.62
    }

    fn colour_of(&self, range: &Range<usize>) -> Option<&Spans> {
        self.colours.get(&range.start)
    }

    fn choose(&mut self, index: usize, side: Side) {
        if index == self.hunk {
            self.choices[index].fill(Some(Choice::One(side)));
            self.refresh_preview();
        }
    }

    fn choose_all(&mut self, side: Side) {
        for hunk in &mut self.choices {
            hunk.fill(Some(Choice::One(side)));
        }
        self.refresh_preview();
    }

    fn all_on(&self, side: Side) -> bool {
        self.document.hunks.iter().zip(&self.choices).all(|(hunk, choices)| {
            hunk.iter().zip(choices).all(|(line, choice)| line.shared || *choice == Some(Choice::One(side)))
        })
    }

    fn choose_line(&mut self, hunk: usize, line: usize, choice: Option<Choice>) -> bool {
        if self.document.hunks.get(hunk).and_then(|lines| lines.get(line)).is_none_or(|line| line.shared) {
            return false;
        }
        self.choices[hunk][line] = choice;
        self.refresh_preview();
        true
    }

    fn refresh_preview(&mut self) {
        self.preview = self.document.preview(&self.choices);
        self.unresolved = self.document.unresolved(&self.choices);
        self.revision += 1;
    }
}

pub(crate) struct State {
    pub(crate) mode: Mode,
    pub(crate) whole_file: bool,
    pub(crate) original: bool,
    editor: Option<Editor>,
    document_ticket: u64,
    colouring: Option<Arc<AtomicBool>>,
    synced: std::cell::Cell<Option<f32>>,
    source_scroll: UniformListScrollHandle,
    result_scroll: UniformListScrollHandle,
    original_scroll: UniformListScrollHandle,
    source_pan: ScrollHandle,
    result_pan: ScrollHandle,
    original_pan: ScrollHandle,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            whole_file: false,
            original: false,
            editor: None,
            document_ticket: 0,
            colouring: None,
            synced: std::cell::Cell::new(None),
            source_scroll: UniformListScrollHandle::new(),
            result_scroll: UniformListScrollHandle::new(),
            original_scroll: UniformListScrollHandle::new(),
            source_pan: ScrollHandle::new(),
            result_pan: ScrollHandle::new(),
            original_pan: ScrollHandle::new(),
        }
    }
}

fn source_height(face: &Face) -> f32 {
    face.line().max(DIFF_LINE)
}

fn result_height(face: &Face) -> f32 {
    face.line().max(CONTROL_SMALL + 4.)
}

const ORIGINAL_ROWS: usize = 8;

impl State {
    pub(crate) fn editing(&self) -> Option<&Path> {
        self.editor
            .as_ref()
            .map(|editor| editor.document.path.as_path())
    }
    pub(crate) fn preview(&self) -> Vec<String> {
        self.editor.as_ref().map_or_else(Vec::new, |editor| {
            editor.preview.rows.iter().filter_map(|row| match row {
                ResultRow::Text { range, .. } => Some(display_line(&editor.document.source[range.clone()])),
                ResultRow::Unresolved { .. } => None,
            }).collect()
        })
    }

    fn shown_rows(&self) -> Range<usize> {
        let Some(editor) = self.editor.as_ref() else { return 0..0 };
        let document = &editor.document;
        if self.whole_file {
            return 0..document.rows.len();
        }
        let start = document.hunk_rows.get(editor.hunk).copied().unwrap_or(0);
        start..start + document.hunks.get(editor.hunk).map_or(0, Vec::len)
    }

    fn source_row(&self, hunk: usize, line: usize) -> usize {
        match self.editor.as_ref() {
            Some(editor) if self.whole_file => editor.document.hunk_rows.get(hunk).copied().unwrap_or(0) + line,
            _ => line,
        }
    }

    fn showing_original(&self) -> bool {
        self.original && self.editor.is_some()
    }

    pub(crate) fn pane_rows(&self, pane: Pane) -> usize {
        let Some(editor) = self.editor.as_ref() else { return 0 };
        match pane {
            Pane::Current | Pane::Incoming => self.shown_rows().len(),
            Pane::Original if self.original => editor.document.original.lines(editor.hunk).len(),
            Pane::Result => editor.preview.rows.len(),
            Pane::Original | Pane::Diff => 0,
        }
    }

    fn source_range(&self, row: usize, side: Side) -> Option<Range<usize>> {
        let editor = self.editor.as_ref()?;
        let shown = self.shown_rows();
        let index = shown.start + row;
        let row = editor.document.rows.get(index).filter(|_| index < shown.end)?;
        editor.document.on(row, side)
    }

    fn result_range(&self, row: usize) -> Option<Range<usize>> {
        match self.editor.as_ref()?.preview.rows.get(row)? {
            ResultRow::Text { range, .. } => Some(range.clone()),
            ResultRow::Unresolved { .. } => None,
        }
    }

    pub(crate) fn pane_line(&self, pane: Pane, row: usize) -> Option<&str> {
        let editor = self.editor.as_ref()?;
        let source = &editor.document.source;
        match pane {
            Pane::Current => self.source_range(row, Side::Current).map(|range| line_of(&source[range])),
            Pane::Incoming => self.source_range(row, Side::Incoming).map(|range| line_of(&source[range])),
            Pane::Original if self.original => editor.document.original.lines(editor.hunk).get(row).map(String::as_str),
            Pane::Result => self.result_range(row).map(|range| line_of(&source[range])),
            Pane::Original | Pane::Diff => None,
        }
    }

    pub(crate) fn text_key(&self, pane: Pane) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let editor = self.editor.as_ref()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (Arc::as_ptr(&editor.document) as usize, self.document_ticket, pane).hash(&mut hasher);
        match pane {
            Pane::Current | Pane::Incoming => (self.whole_file, if self.whole_file { 0 } else { editor.hunk }).hash(&mut hasher),
            Pane::Original => (self.original, editor.hunk).hash(&mut hasher),
            Pane::Result => editor.revision.hash(&mut hasher),
            Pane::Diff => return None,
        }
        Some(hasher.finish())
    }

    pub(crate) fn track(&self, pane: Pane, face: &Face) -> (&UniformListScrollHandle, usize, f32) {
        match pane {
            Pane::Result => (&self.result_scroll, self.pane_rows(pane), result_height(face)),
            Pane::Original => (&self.original_scroll, self.pane_rows(pane), face.line()),
            _ => (&self.source_scroll, self.pane_rows(pane), source_height(face)),
        }
    }

    pub(crate) fn find_key(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let editor = self.editor.as_ref()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (Arc::as_ptr(&editor.document) as usize, editor.hunk, editor.revision, self.document_ticket).hash(&mut hasher);
        (self.whole_file, self.original).hash(&mut hasher);
        Some(hasher.finish())
    }

    pub(crate) fn find_lines(&self, mut visit: impl FnMut(Pane, usize, &str)) {
        for pane in [Pane::Current, Pane::Incoming, Pane::Original, Pane::Result] {
            for row in 0..self.pane_rows(pane) {
                if let Some(text) = self.pane_line(pane, row) {
                    visit(pane, row, text);
                }
            }
        }
    }

    pub(crate) fn reveal(&self, pane: Pane, row: usize) {
        match pane {
            Pane::Result => self.result_scroll.scroll_to_item(row, ScrollStrategy::Center),
            Pane::Original => self.original_scroll.scroll_to_item(row, ScrollStrategy::Center),
            _ => self.source_scroll.scroll_to_item(row, ScrollStrategy::Center),
        }
    }

    fn show_hunk(&mut self, index: usize) {
        if let Some(editor) = self.editor.as_mut()
            && let Some(&start) = editor.preview.hunk_starts.get(index)
        {
            editor.hunk = index;
            let first = if self.whole_file {
                editor.document.hunk_rows.get(index).map_or(0, |row| row.saturating_sub(2))
            } else {
                0
            };
            self.source_scroll.scroll_to_item_strict(first, ScrollStrategy::Top);
            self.result_scroll.scroll_to_item_strict(start, ScrollStrategy::Top);
            self.original_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        }
    }

    fn stop_colouring(&mut self) {
        if let Some(cancel) = self.colouring.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn forget(&mut self) {
        self.stop_colouring();
        self.synced.set(None);
        self.editor = None;
        self.document_ticket += 1;
    }

    pub(crate) fn follow(&mut self, changed: bool, snapshot: Option<&Snapshot>) {
        if changed
            || self.editing().is_some_and(|path| !snapshot.is_some_and(|s| s.has_conflict(path)))
        {
            self.forget();
        }
    }
}

pub(crate) fn start(
    tab: &mut Tab,
    source: Source,
    mode: Mode,
    expected: String,
    seen: crate::guard::Seen,
    stash: bool,
    cx: &mut Context<Tab>,
) {
    let current = tab.ui.branch.clone();
    start_on(tab, current, source, mode, expected, seen, stash, cx);
}

pub(crate) fn start_on(
    tab: &mut Tab,
    current: String,
    source: Source,
    mode: Mode,
    expected: String,
    seen: crate::guard::Seen,
    stash: bool,
    cx: &mut Context<Tab>,
) {
    if let Some(reason) = crate::commands::operating(&tab.ui) {
        tab.ui.notices.show(crate::notice::Notice::new(crate::view::Tone::Info, reason, ""));
        cx.notify();
        return;
    }
    let label = source.label();
    let squash = mode == Mode::Squash;
    if !squash {
        tab.ui.operation.started = Some((Kind::Merge, expected.clone(), label.clone()));
    }
    tab.ui.refs.list_open = false;
    let kept = crate::autostash::Kept::default();
    let stopped = kept.clone();
    let staged = Arc::new(std::sync::Mutex::new(None::<String>));
    let squashing = Arc::new(std::sync::Mutex::new(None::<crate::operation::Picking>));
    let (staged_job, squashing_job) = (staged.clone(), squashing.clone());
    tab.run_operation_or(
        cx,
        if squash { "Squashing…" } else { "Merging…" },
        "Merge completed",
        !squash,
        move |repo| {
            let retry = || crate::autostash::Retry::Merge {
                source: source.clone(),
                mode,
                expected: expected.clone(),
                seen: seen.clone(),
            };
            let merge = |autostash: bool| {
                backend::start(&repo, &source, &expected, &seen, mode, autostash).map_err(|err| {
                    let refused = err.starts_with(backend::NO_FAST_FORWARD);
                    let failure = crate::notice::Failure::from(err);
                    if refused {
                        let again = view::Action::Merge(Action::Start(source.clone(), Mode::NoFastForward));
                        failure.offer("Merge with a merge commit", again)
                    } else {
                        failure
                    }
                })
            };
            let done = |merged: &Merged| outcome(merged, &current, &label).title;
            let message = |repo: &crate::repository::Repository| {
                format!("Squash merge {label} into {current}\n\n{}", backend::squash_message(repo))
            };
            let (merged, stashed) = match crate::autostash::by_git(&repo, stash, &kept, retry, merge, done) {
                Ok(done) => done,
                Err(failure) => {
                    if squash && failure.text.starts_with(backend::SQUASH_CONFLICT) {
                        *squashing_job.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::operation::Picking {
                            oid: expected.clone(),
                            label: label.clone(),
                            message: message(&repo),
                            squash: true,
                        });
                    }
                    return Err(failure);
                }
            };
            let mut said = outcome(&merged, &current, &label).undone_as(format!("Merge {label} into {current}"));
            if matches!(merged, Merged::Squashed(_)) {
                *staged_job.lock().unwrap_or_else(|e| e.into_inner()) = Some(message(&repo));
            }
            if stashed && crate::autostash::pending(&repo).is_some() {
                said.detail.push_str(" Your local changes were stashed first, and Git puts them back when you commit.");
            } else if stashed {
                said.detail.push_str(" Your local changes were stashed first and put back.");
            }
            Ok::<_, crate::notice::Failure>(said)
        },
        move |tab, cx| {
            let message = staged.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(message) = message {
                crate::operation::draft_message(&tab.ui, &message, cx);
                tab.select(view::Selection::Changes, cx);
            }
        },
        move |tab, _| {
            stopped.remember(&mut tab.ui);
            if let Some(picking) = squashing.lock().unwrap_or_else(|e| e.into_inner()).take() {
                tab.ui.operation.picking = Some(picking);
            }
        },
    );
}

fn set_default(tab: &mut Tab, mode: Mode, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    tab.ui.merge.mode = mode;
    let common = repo.common_dir().to_owned();
    crate::settings::Memory::update(cx, |memory| memory.change_repo(&common, |view| view.merge = Some(mode)));
    tab.ui.notices.show(crate::notice::Notice::new(
        crate::view::Tone::Info,
        format!("Default merge: {}", mode.title()),
        "A double-click on a Merge item runs it in this repository.",
    ));
    cx.notify();
}

pub(crate) fn restore(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    tab.ui.merge.mode =
        crate::settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.merge).unwrap_or_default();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() && !matches!(action, Action::WholeFile(_) | Action::ShowOriginal(_)) {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.merge;
    let snapshot = tab.ui.operation.snapshot();
    match action {
        Action::Start(source, mode) => {
            let Some(expected) = source.shown(&tab.ui) else {
                return;
            };
            if tab.ui.head.is_none() || tab.ui.detached {
                tab.error("Merge Failed — Check out a branch to merge into.".to_owned(), cx);
                return;
            }
            let seen = crate::guard::Seen::of(&tab.ui);
            let stash = tab.ui.autostash.always;
            start(tab, source, mode, expected, seen, stash, cx);
        }
        Action::SetDefault(mode) => set_default(tab, mode, cx),
        Action::Open(path) => {
            let found = snapshot.and_then(|s| s.conflict(&path).map(|c| (c.shape.fits_resolver(), s.id.clone())));
            let Some((fits, id)) = found else {
                return;
            };
            if !fits {
                state.forget();
                crate::conflict::show(&mut tab.ui, path, None);
                cx.notify();
                return;
            }
            state.forget();
            let ticket = state.document_ticket;
            let epoch = tab.epoch;
            tab.ui.conflict.close();
            Tab::work(
                cx,
                move || {
                    Document::open(&repo, id.clone(), path.clone())
                        .map(Editor::new)
                        .map_err(|err| (err, repo.expect_operation(&id).is_ok(), path))
                },
                move |tab, result, cx| {
                    if tab.epoch != epoch || tab.ui.merge.document_ticket != ticket {
                        return;
                    }
                    match result {
                        Ok(editor)
                            if tab.ui.operation.snapshot().is_some_and(|s| {
                                s.id == editor.document.id && s.has_conflict(&editor.document.path)
                            }) =>
                        {
                            let state = &mut tab.ui.merge;
                            state.source_scroll = UniformListScrollHandle::new();
                            state.result_scroll = UniformListScrollHandle::new();
                            state.source_pan = ScrollHandle::new();
                            state.result_pan = ScrollHandle::new();
                            state.original_scroll = UniformListScrollHandle::new();
                            state.original_pan = ScrollHandle::new();
                            state.editor = Some(editor);
                            state.show_hunk(0);
                            colour(tab, cx);
                        }
                        Ok(_) => {}
                        Err((err, same, path))
                            if same && tab.ui.operation.snapshot().is_some_and(|s| s.has_conflict(&path)) =>
                        {
                            crate::conflict::show(&mut tab.ui, path, Some(err));
                        }
                        Err((err, _, _)) => tab.error(err, cx),
                    }
                    cx.notify();
                },
            );
        }
        Action::Hunk(index) => {
            state.show_hunk(index);
        }
        Action::Close => state.forget(),
        Action::WholeFile(on) => {
            state.whole_file = on;
            if let Some(hunk) = state.editor.as_ref().map(|editor| editor.hunk) {
                state.show_hunk(hunk);
            }
        }
        Action::ShowOriginal(on) => {
            state.original = on;
            state.original_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        }
        Action::Choose(index, side) => {
            if let Some(editor) = state.editor.as_mut() {
                editor.choose(index, side)
            }
        }
        Action::ChooseAll(side) => {
            if let Some(editor) = state.editor.as_mut() {
                editor.choose_all(side)
            }
        }
        Action::ChooseLine(hunk, line, choice) => {
            if let Some(editor) = state.editor.as_mut()
                && editor.choose_line(hunk, line, Some(choice))
                && editor.hunk != hunk
            {
                editor.hunk = hunk;
                let row = state.source_row(hunk, line);
                state.source_scroll.scroll_to_item(row, ScrollStrategy::Center);
            }
        }
        Action::ToggleLine(hunk, line, side) => {
            if let Some(editor) = state.editor.as_mut()
                && let Some(&choice) = editor.choices.get(hunk).and_then(|lines| lines.get(line))
            {
                let next = match choice {
                    None => Some(Choice::One(side)),
                    Some(Choice::One(chosen)) if chosen == side => None,
                    Some(Choice::One(_)) => Some(Choice::Both),
                    Some(Choice::Both) => Some(Choice::One(match side {
                        Side::Current => Side::Incoming,
                        Side::Incoming => Side::Current,
                    })),
                };
                if editor.choose_line(hunk, line, next) {
                    editor.hunk = hunk;
                }
            }
        }
        Action::Save => {
            let Some(editor) = state.editor.as_ref() else {
                return;
            };
            if editor.unresolved > 0 {
                return;
            }
            let document = Arc::clone(&editor.document);
            let choices = editor.choices.clone();
            tab.run_operation_then(
                cx,
                "Saving the merge result…",
                "Merge output saved",
                false,
                move |repo| document.save(&repo, &choices),
                |tab, _| tab.ui.merge.forget(),
            );
        }
    }
    cx.notify();
}

fn coloured(document: &Document, cancel: &AtomicBool) -> HashMap<usize, Spans> {
    let mut colours = HashMap::new();
    for side in [Side::Incoming, Side::Current] {
        let (text, starts) = document.side_text(side);
        let Some(lines) = crate::syntax::colour_lines(&document.path, &text, cancel) else { continue };
        colours.extend(starts.into_iter().zip(lines).filter(|(_, spans)| !spans.is_empty()));
    }
    colours
}

pub(crate) fn colour(tab: &mut Tab, cx: &mut Context<Tab>) {
    let on = tab.ui.syntax.on;
    let state = &mut tab.ui.merge;
    state.stop_colouring();
    let Some(editor) = state.editor.as_mut() else { return };
    if !on {
        editor.colours.clear();
        editor.coloured = false;
        return;
    }
    if editor.coloured {
        return;
    }
    let document = Arc::clone(&editor.document);
    let cancel = Arc::new(AtomicBool::new(false));
    state.colouring = Some(cancel.clone());
    let ticket = state.document_ticket;
    Tab::work(
        cx,
        {
            let (document, cancel) = (document.clone(), cancel.clone());
            move || coloured(&document, &cancel)
        },
        move |tab, colours, cx| {
            let state = &mut tab.ui.merge;
            if cancel.load(Ordering::Relaxed) || state.document_ticket != ticket {
                return;
            }
            state.colouring = None;
            if let Some(editor) = state.editor.as_mut().filter(|editor| Arc::ptr_eq(&editor.document, &document)) {
                editor.colours = colours;
                editor.coloured = true;
                cx.notify();
            }
        },
    );
}

fn button<T: Desk>(id: impl Into<gpui::ElementId>, label: &str) -> kit::Btn {
    let _ = std::marker::PhantomData::<T>;
    kit::btn(id, label.to_owned())
}

fn side_color(side: Side) -> u32 {
    match side {
        Side::Current => palette().accent,
        Side::Incoming => palette().remote,
    }
}

fn line_number(face: &Face, number: Option<usize>) -> Div {
    div()
        .w(px(face.number(true)))
        .flex_shrink_0()
        .pr_2()
        .flex()
        .justify_end()
        .font_family(face.family.clone())
        .text_size(face.small())
        .text_color(rgb(palette().text_off))
        .when_some(number.filter(|_| face.numbers), |d, n| d.child(n.to_string()))
}

fn runs(ui: &view::Ui, pane: Pane, row: usize, code: &str, syntax: Option<&Spans>) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
    let chosen: Vec<Range<usize>> = crate::diff_select::span(ui, row, Column::Resolver(pane)).into_iter().collect();
    let (found, focus) = crate::find::marks(ui, pane, row);
    let marks = view::DiffMarks {
        chosen: &chosen,
        found: &found,
        focus: &focus,
        words: &[],
        changed: palette().text_selection,
        syntax: syntax.map_or(&[][..], Vec::as_slice),
    };
    view::diff_clipped(marks.runs(), &(0..shown_length(code)))
}

fn line_text<T: Desk>(
    ui: &view::Ui,
    id: (&'static str, usize),
    pane: Pane,
    row: usize,
    pan: &ScrollHandle,
    syntax: Option<&Spans>,
    cx: &Context<T>,
) -> impl IntoElement {
    let face = &ui.diff_text.face;
    let code = ui.merge.pane_line(pane, row).unwrap_or_default();
    let highlights = runs(ui, pane, row, code, syntax);
    let cut = shown_length(code);
    let shown = if cut == code.len() { code.to_owned() } else { format!("{}…", &code[..cut]) };
    let width = ui.merge.editor.as_ref().map_or(0., |editor| editor.width(face));
    let drawn = view::diff_drawn(face, &shown, highlights).min_w(px(width));
    let mut text = div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_x_scroll()
        .track_scroll(pan)
        .child(crate::diff_select::cell(ui, row, 0, Column::Resolver(pane), drawn, cx));
    text.style().restrict_scroll_to_axis = Some(true);
    text
}

fn conflict_strip(shown: Option<bool>) -> Div {
    div()
        .w(px(3.))
        .h_full()
        .flex_shrink_0()
        .when_some(shown, |d, shown| d.bg(if shown { rgb(palette().conflict) } else { soft(palette().conflict, 0x60) }))
}

fn source_cell<T: Desk>(app: &T, row: usize, side: Side, cx: &Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.merge;
    let editor = state.editor.as_ref().expect("source rows require an editor");
    let face = &ui.diff_text.face;
    let index = state.shown_rows().start + row;
    let shown_row = &editor.document.rows[index];
    let (button_id, text_id, tip, pane) = match side {
        Side::Current => ("merge-pick-current", "merge-current-text", "Toggle current line", Pane::Current),
        Side::Incoming => ("merge-pick-incoming", "merge-incoming-text", "Toggle incoming line", Pane::Incoming),
    };
    let cell = div()
        .h_full()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .when(side == Side::Incoming, |d| d.border_l_1().border_color(rgb(palette().line)));
    let range = editor.document.on(shown_row, side);
    let syntax = range.as_ref().and_then(|range| editor.colour_of(range));
    let text = |d: Div| d.child(line_text(ui, (text_id, row), pane, row, &state.source_pan, syntax, cx));
    let &Place::Conflict { hunk, line } = &shown_row.place else {
        return text(cell.child(conflict_strip(None)).child(line_number(face, shown_row.number(side))).child(div().w(px(23.)).flex_shrink_0()));
    };
    let pair: &LinePair = &editor.document.hunks[hunk][line];
    let choice = editor.choices[hunk][line];
    let selected = choice.is_some_and(|choice| choice.includes(side));
    let color = side_color(side);
    let action = view::Action::Merge(Action::ToggleLine(hunk, line, side));
    let enabled = !ui.busy();
    let cell = cell
        .when(!pair.shared, |d| d.bg(soft(color, if selected { 0x24 } else { 0x0d })))
        .child(conflict_strip(Some(hunk == editor.hunk)))
        .child(line_number(face, shown_row.number(side)))
        .child(
            div()
                .w(px(23.))
                .flex_shrink_0()
                .when(!pair.shared, |d| {
                    d.child(
                        div()
                            .id((button_id, row))
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(kit::clear())
                            .tooltip(kit::tip(if range.is_some() { tip } else { "Use this side's deletion" }))
                            .child(kit::checkbox(selected))
                            .when(enabled, |d| {
                                let d = d.cursor_pointer().focusable().focus(|s| s.border_color(rgb(palette().accent)));
                                kit::pressable(d, action, cx)
                            }),
                    )
                }),
        );
    if range.is_some() { text(cell) } else { cell.child(kit::muted("No line")) }
}

fn follow_scroll(state: &State, editor: &Editor, face: &Face) {
    if !state.whole_file {
        state.synced.set(None);
        return;
    }
    let top = -f32::from(state.source_scroll.0.borrow().base_handle.offset().y);
    let seen = state.synced.replace(Some(top));
    if seen.is_none_or(|seen| (seen - top).abs() < 0.5) {
        return;
    }
    let height = source_height(face);
    let first = (top / height).floor().max(0.);
    let Some(&row) = editor.preview.sources.get(first as usize) else { return };
    let result = state.result_scroll.0.borrow();
    let mut offset = result.base_handle.offset();
    offset.y = px(-(row as f32 + top / height - first) * result_height(face));
    result.base_handle.set_offset(offset);
}

fn source_rows<T: Desk>(state: &State, face: &Face, cx: &mut Context<T>) -> impl IntoElement {
    let height = source_height(face);
    let mut rows = uniform_list(
        "merge-source-lines",
        state.shown_rows().len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            range.map(|line| {
                div()
                    .h(px(height))
                    .w_full()
                    .flex()
                    .child(source_cell(app, line, Side::Current, cx))
                    .child(source_cell(app, line, Side::Incoming, cx))
            }).collect::<Vec<_>>()
        }),
    )
    .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::FitList)
    .track_scroll(state.source_scroll.clone())
    .flex_1()
    .min_h_0();
    rows.style().restrict_scroll_to_axis = Some(true);
    rows
}

fn result_rows<T: Desk>(state: &State, editor: &Editor, face: &Face, cx: &mut Context<T>) -> impl IntoElement {
    let height = result_height(face);
    let mut rows = uniform_list(
        "merge-result-lines",
        editor.preview.rows.len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let state = &ui.merge;
            let editor = state.editor.as_ref().expect("result rows require an editor");
            let face = &ui.diff_text.face;
            range.map(|i| {
                let base = div().id(("merge-result", i))
                    .h(px(height))
                    .w_full()
                    .flex()
                    .items_center()
                    .overflow_hidden();
                match &editor.preview.rows[i] {
                    ResultRow::Text { range, side } => base
                        .when_some(*side, |d, side| d.bg(soft(side_color(side), 0x15)))
                        .child(line_number(face, Some(i + 1)))
                        .child(line_text(ui, ("merge-result-text", i), Pane::Result, i, &state.result_pan, editor.colour_of(range), cx)),
                    ResultRow::Unresolved { hunk, line } => base
                        .px_2()
                        .gap(px(4.))
                        .bg(soft(palette().conflict, 0x18))
                        .child(kit::fit(format!("Line {} unresolved", i + 1)).size(TEXT_SMALL).color(palette().conflict).grow())
                        .children([
                            ("result-current", "Use current", Choice::One(Side::Current)),
                            ("result-incoming", "Use incoming", Choice::One(Side::Incoming)),
                            ("result-both", "Use both", Choice::Both),
                        ].into_iter().map(|(id, label, choice)| {
                            button::<T>(id, label).small().enabled(!ui.busy())
                                .on(view::Action::Merge(Action::ChooseLine(*hunk, *line, choice)), cx)
                        })),
                }
            }).collect::<Vec<_>>()
        }),
    )
    .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::FitList)
    .track_scroll(state.result_scroll.clone())
    .flex_1()
    .min_h_0();
    rows.style().restrict_scroll_to_axis = Some(true);
    rows
}

fn original_pane<T: Desk>(state: &State, editor: &Editor, face: &Face, cx: &mut Context<T>) -> Div {
    let original = &editor.document.original;
    let count = original.lines(editor.hunk).len();
    let heading = div()
        .h(px(30.))
        .flex_shrink_0()
        .pl_2()
        .pr_1()
        .flex()
        .items_center()
        .gap(px(5.))
        .bg(rgb(palette().panel))
        .border_t_1()
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(rgb(palette().text_3)))
        .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).font_weight(FontWeight::SEMIBOLD).child("Original"))
        .child(
            kit::fit(format!("Common ancestor of conflict {}", editor.hunk + 1))
                .size(TEXT_SMALL)
                .color(palette().text_3)
                .grow(),
        )
        .child(kit::icon_btn("close-original", "x", "Hide the original", view::Action::Merge(Action::ShowOriginal(false)), cx));
    let pane = div().flex().flex_col().flex_shrink_0().child(heading);
    if let Some(reason) = original.missing(editor.hunk) {
        return pane.child(div().px_3().py_2().child(kit::muted(reason)));
    }
    let line = face.line();
    let mut rows = uniform_list(
        "merge-original-lines",
        count,
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            range.map(|i| {
                div()
                    .h(px(line))
                    .w_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .bg(soft(palette().text_3, 0x10))
                    .child(div().w(px(8.)).flex_shrink_0())
                    .child(line_text(ui, ("merge-original-text", i), Pane::Original, i, &ui.merge.original_pan, None, cx))
            }).collect::<Vec<_>>()
        }),
    )
    .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::FitList)
    .track_scroll(state.original_scroll.clone())
    .h(px(count.min(ORIGINAL_ROWS) as f32 * line))
    .flex_shrink_0();
    rows.style().restrict_scroll_to_axis = Some(true);
    pane.child(
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .child(rows)
            .child(kit::scrollbar("merge-original-scrollbar", &state.original_scroll))
            .child(kit::scrollbar("merge-original-pan", &state.original_pan).horizontal().within(&state.original_scroll)),
    )
}

pub(crate) fn resolver<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.merge;
    let (Some(snapshot), Some(editor)) = (ui.operation.snapshot(), state.editor.as_ref()) else {
        return div();
    };
    let ready = !ui.busy();
    let face = &ui.diff_text.face;
    follow_scroll(state, editor, face);
    let i = editor.hunk;
    let total = editor.document.hunks.len();
    let open = editor.unresolved;
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
        .child(
            kit::fit(editor.document.path.to_string_lossy().into_owned())
                .size(12.5)
                .weight(FontWeight::SEMIBOLD)
                .cut(kit::Cut::Start)
                .grow(),
        )
        .child(
            button::<T>("save-merge-output", "Mark resolved")
                .small()
                .primary()
                .enabled(ready && open == 0)
                .on(view::Action::Merge(Action::Save), cx),
        )
        .child(kit::icon_btn(
            "close-conflict",
            "x",
            "Back to the graph (Esc)",
            view::Action::Merge(Action::Close),
            cx,
        ));
    let tools = div()
        .h(px(32.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap_1()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::muted(format!("Conflict {} of {total}", i + 1)).flex_shrink_0().mr_1())
        .when(total > 1, |d| {
            d.child(
                button::<T>("previous-conflict", "")
                    .small()
                    .ghost()
                    .icon("chevron-up")
                    .enabled(i > 0)
                    .tip("Previous conflict")
                    .on(view::Action::Merge(Action::Hunk(i.saturating_sub(1))), cx),
            )
            .child(
                button::<T>("next-conflict", "")
                    .small()
                    .ghost()
                    .icon("chevron-down")
                    .enabled(i + 1 < total)
                    .tip("Next conflict")
                    .on(view::Action::Merge(Action::Hunk(i + 1)), cx),
            )
        })
        .child(div().w(px(1.)).h(px(16.)).mx_1().flex_shrink_0().bg(rgb(palette().line)))
        .child(
            kit::btn("merge-whole-file", "Whole file")
                .small()
                .ghost()
                .selected(state.whole_file)
                .tip("Show each side as the whole file, with its conflicts marked")
                .on(view::Action::Merge(Action::WholeFile(!state.whole_file)), cx),
        )
        .child(
            kit::btn("merge-show-original", "Show original")
                .small()
                .ghost()
                .selected(state.original)
                .tip("Show the common ancestor's lines for this conflict")
                .on(view::Action::Merge(Action::ShowOriginal(!state.original)), cx),
        )
        .child(kit::grow())
        .children([
            (Side::Current, snapshot.ours_name(), snapshot.ours(), "use-all-current"),
            (Side::Incoming, snapshot.theirs_name(), snapshot.theirs(), "use-all-incoming"),
        ].into_iter().map(|(side, name, full, id)| {
            kit::btn(id, format!("Use {name} for all"))
                .small()
                .enabled(ready)
                .selected(editor.all_on(side))
                .tip(format!("Use {full} for every conflict in this file"))
                .on(view::Action::Merge(Action::ChooseAll(side)), cx)
        }));
    let headings = div().flex().flex_shrink_0().children([
        (Side::Current, "Current", snapshot.ours(), "use-current", "Use all current lines in this conflict"),
        (Side::Incoming, "Incoming", snapshot.theirs(), "use-incoming", "Use all incoming lines in this conflict"),
    ].into_iter().map(|(side, name, branch, id, tip)| {
        let chosen = editor.document.hunks[i].iter().zip(&editor.choices[i])
            .all(|(line, choice)| line.shared || *choice == Some(Choice::One(side)));
        div()
            .h(px(30.))
            .flex_1()
            .min_w_0()
            .pl_2()
            .pr_2()
            .flex()
            .items_center()
            .gap(px(5.))
            .bg(rgb(palette().panel))
            .border_b_1()
            .border_color(rgb(palette().line))
            .when(side == Side::Incoming, |d| d.border_l_1())
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(rgb(side_color(side))))
            .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).font_weight(FontWeight::SEMIBOLD).child(name))
            .child(kit::fit(branch).size(TEXT_SMALL).color(palette().text_3).grow())
            .child(button::<T>(id, "Use all").small().enabled(ready).selected(chosen).tip(tip)
                .on(view::Action::Merge(Action::Choose(i, side)), cx))
    }));
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(tools)
        .children(crate::find::bar(app, crate::find::Surface::Resolver, cx))
        .child(headings)
        .child(source_rows(state, face, cx))
        .child(kit::scrollbar("merge-source-scrollbar", &state.source_scroll))
        .child(kit::scrollbar("merge-source-pan", &state.source_pan).horizontal().within(&state.source_scroll))
        .when(state.showing_original(), |d| d.child(original_pane(state, editor, face, cx)))
        .child(
            div()
                .h(px(30.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .bg(rgb(palette().panel))
                .border_t_1()
                .border_b_1()
                .border_color(rgb(palette().line))
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Result"),
                )
                .child(kit::grow())
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(if open == 0 { palette().added } else { palette().conflict }))
                        .child(match open {
                            0 => "All lines resolved".to_owned(),
                            1 => "1 line unresolved".to_owned(),
                            n => format!("{n} lines unresolved"),
                        }),
                ),
        )
        .child(result_rows(state, editor, face, cx))
        .child(kit::scrollbar("merge-result-scrollbar", &state.result_scroll))
        .child(kit::scrollbar("merge-result-pan", &state.result_pan).horizontal().within(&state.result_scroll))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{snapshot, tests::{commit, stops}};
    use crate::repository::tests::Fixture;

    #[test]
    fn find_covers_both_sides_of_the_shown_conflict_and_the_result_and_follows_choices() {
        use crate::find::{Pane, resolver_hits};
        let f = Fixture::new();
        let base = (0..32).map(|i| format!("line {i}\n")).collect::<String>();
        commit(&f, "trail.txt", &base, "Base");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "trail.txt", &base.replace("line 4\n", "Ridge waypoint\n").replace("line 22\n", "Detour waypoint\n"), "Side");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.txt", &base.replace("line 4\n", "Willow bridge\n").replace("line 22\n", "Main Waypoint\n"), "Main");
        stops(&f, &["merge", "side"]);
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        let mut state = State { editor: Some(Editor::new(Document::open(&f.repo, id, "trail.txt".into()).unwrap())), ..State::default() };
        let panes = |state: &State, query: &str| resolver_hits(state, query).into_iter().map(|hit| (hit.pane, hit.row)).collect::<Vec<_>>();
        assert_eq!(panes(&state, "waypoint"), [(Pane::Incoming, 0)]);
        assert_eq!(panes(&state, "line 3"), [(Pane::Result, 3), (Pane::Result, 30), (Pane::Result, 31)]);
        let key = state.find_key();
        state.show_hunk(1);
        assert_ne!(state.find_key(), key);
        assert_eq!(panes(&state, "waypoint"), [(Pane::Current, 0), (Pane::Incoming, 0)]);
        assert_eq!(panes(&state, "Waypoint"), [(Pane::Current, 0)]);
        let key = state.find_key();
        state.editor.as_mut().unwrap().choose_all(Side::Incoming);
        assert_ne!(state.find_key(), key);
        let found = resolver_hits(&state, "waypoint");
        let results: Vec<String> = state.preview().into_iter().enumerate()
            .filter(|(row, _)| found.iter().any(|hit| hit.pane == Pane::Result && hit.row == *row))
            .map(|(_, line)| line)
            .collect();
        assert_eq!(results, ["Ridge waypoint", "Detour waypoint"]);
        state.editor = None;
        assert!(resolver_hits(&state, "waypoint").is_empty() && state.find_key().is_none());
    }

    fn waypoints() -> (Fixture, State) {
        let f = Fixture::new();
        let base = (0..32).map(|i| format!("line {i}\n")).collect::<String>();
        commit(&f, "trail.txt", &base, "Base");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "trail.txt", &base.replace("line 4\n", "Ridge waypoint\n").replace("line 22\n", "Detour waypoint\n"), "Side");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.txt", &base.replace("line 4\n", "Willow bridge\n").replace("line 22\n", "Main Waypoint\n"), "Main");
        stops(&f, &["merge", "side"]);
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        let state = State { editor: Some(Editor::new(Document::open(&f.repo, id, "trail.txt".into()).unwrap())), ..State::default() };
        (f, state)
    }

    #[test]
    fn whole_files_show_every_line_of_both_sides_and_find_reaches_every_conflict() {
        use crate::find::{Pane, resolver_hits};
        let (_f, mut state) = waypoints();
        assert_eq!(state.pane_rows(Pane::Current), 1);
        let hunk_key = state.text_key(Pane::Current);
        state.whole_file = true;
        assert_ne!(state.text_key(Pane::Current), hunk_key);
        assert_eq!((state.pane_rows(Pane::Current), state.pane_rows(Pane::Incoming)), (32, 32));
        assert_eq!(state.pane_line(Pane::Current, 3), Some("line 3"));
        assert_eq!(state.pane_line(Pane::Current, 4), Some("Willow bridge"));
        assert_eq!(state.pane_line(Pane::Incoming, 4), Some("Ridge waypoint"));
        assert_eq!(state.pane_line(Pane::Incoming, 31), Some("line 31"));
        assert_eq!(state.pane_line(Pane::Incoming, 32), None);
        let editor = state.editor.as_ref().unwrap();
        assert_eq!(editor.document.rows[22].number(Side::Current), Some(23));
        assert_eq!(state.source_row(1, 0), 22);
        let panes = |state: &State, query: &str| resolver_hits(state, query).into_iter().map(|hit| (hit.pane, hit.row)).collect::<Vec<_>>();
        assert_eq!(panes(&state, "waypoint"), [(Pane::Current, 22), (Pane::Incoming, 4), (Pane::Incoming, 22)]);
        let (key, whole_key) = (state.find_key(), state.text_key(Pane::Current));
        state.show_hunk(1);
        assert_eq!(state.editor.as_ref().unwrap().hunk, 1);
        assert_eq!(state.text_key(Pane::Current), whole_key);
        assert_ne!(state.find_key(), key);
        assert_eq!(panes(&state, "waypoint"), [(Pane::Current, 22), (Pane::Incoming, 4), (Pane::Incoming, 22)]);
        let copied = crate::diff_select::text(
            &state,
            Column::Resolver(Pane::Current),
            crate::diff_select::Spot { row: 3, at: 5 },
            crate::diff_select::Spot { row: 5, at: 4 },
        );
        assert_eq!(copied, "3\nWillow bridge\nline");
        state.editor.as_mut().unwrap().choose_all(Side::Incoming);
        let everything = crate::diff_select::everything(&state, Column::Resolver(Pane::Result)).unwrap();
        let result = crate::diff_select::text(&state, Column::Resolver(Pane::Result), everything.0, everything.1);
        assert_eq!(result.lines().nth(4), Some("Ridge waypoint"));
        assert_eq!(result.lines().count(), 32);
    }

    #[test]
    fn the_original_pane_lists_the_common_ancestor_lines_of_the_shown_conflict() {
        use crate::find::{Pane, resolver_hits};
        let (_f, mut state) = waypoints();
        assert_eq!(state.pane_rows(Pane::Original), 0);
        assert!(!state.showing_original());
        state.original = true;
        assert!(state.showing_original());
        assert_eq!(state.pane_rows(Pane::Original), 1);
        assert_eq!(state.pane_line(Pane::Original, 0), Some("line 4"));
        let hits = resolver_hits(&state, "line 4").into_iter().map(|hit| (hit.pane, hit.row)).collect::<Vec<_>>();
        assert_eq!(hits, [(Pane::Original, 0)]);
        let key = state.text_key(Pane::Original);
        state.show_hunk(1);
        assert_ne!(state.text_key(Pane::Original), key);
        assert_eq!(state.pane_line(Pane::Original, 0), Some("line 22"));
        state.original = false;
        assert_eq!(state.pane_line(Pane::Original, 0), None);
        assert!(resolver_hits(&state, "line 22").is_empty());
    }

    #[test]
    fn both_sides_are_coloured_as_whole_files_so_a_comment_above_a_conflict_reaches_it() {
        let f = Fixture::new();
        let base = "/*\n trail notes\n line a\n*/\nfn main() {}\n";
        commit(&f, "trail.rs", base, "Base");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "trail.rs", &base.replace(" line a", " ridge pass"), "Side");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.rs", &base.replace(" line a", " willow bridge"), "Main");
        stops(&f, &["merge", "side"]);
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        let editor = Editor::new(Document::open(&f.repo, id, "trail.rs".into()).unwrap());
        let colours = coloured(&editor.document, &AtomicBool::new(false));
        let pair = &editor.document.hunks[0][0];
        for side in [Side::Current, Side::Incoming] {
            let (_, range) = pair.on(side).unwrap();
            let spans = colours.get(&range.start).expect("the conflict line is coloured");
            assert!(spans.iter().all(|(_, token)| *token == crate::syntax::Token::Comment), "{spans:?}");
        }
        let last = editor.document.rows.last().unwrap();
        let range = editor.document.on(last, Side::Current).unwrap();
        assert!(colours.get(&range.start).unwrap().iter().any(|(_, token)| *token == crate::syntax::Token::Keyword));
        assert!(coloured(&editor.document, &AtomicBool::new(true)).is_empty());
    }

    #[test]
    fn one_side_can_be_used_for_every_conflict_in_a_file() {
        let f = Fixture::new();
        let base = (0..32).map(|i| format!("line {i}\n")).collect::<String>();
        commit(&f, "trail.txt", &base, "Base");
        f.git(&["switch", "-qc", "side"]);
        let theirs = base.replace("line 4\n", "Ridge pass\n").replace("line 22\n", "Detour waypoint\n");
        commit(&f, "trail.txt", &theirs, "Side");
        f.git(&["switch", "-q", "main"]);
        let ours = base.replace("line 4\n", "Willow bridge\n").replace("line 22\n", "Main waypoint\n");
        commit(&f, "trail.txt", &ours, "Main");
        stops(&f, &["merge", "side"]);
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        let mut editor = Editor::new(Document::open(&f.repo, id, "trail.txt".into()).unwrap());
        assert_eq!(editor.document.hunks.len(), 2);
        assert!(!editor.all_on(Side::Incoming));
        editor.choose_all(Side::Incoming);
        assert!(editor.all_on(Side::Incoming) && !editor.all_on(Side::Current));
        assert_eq!(editor.unresolved, 0);
        assert_eq!(editor.document.output(&editor.choices), theirs);
        editor.choose_all(Side::Current);
        assert_eq!(editor.document.output(&editor.choices), ours);
        editor.document.save(&f.repo, &editor.choices).unwrap();
        assert_eq!(f.git(&["show", ":trail.txt"]), ours.as_bytes());
    }

    #[test]
    fn a_source_names_its_full_ref_and_reads_as_the_user_chose_it() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let cases = [
            (Source::Branch("feature/x".into()), "refs/heads/feature/x", "feature/x"),
            (Source::RemoteBranch("team/fork/main".into()), "refs/remotes/team/fork/main", "team/fork/main"),
            (Source::Tag("v1.2".into()), "refs/tags/v1.2", "v1.2"),
            (Source::Commit(oid.into()), oid, "0123456"),
        ];
        for (source, full, label) in cases {
            assert_eq!((source.full(), source.label()), (OsString::from(full), label.to_owned()));
        }
    }
}
