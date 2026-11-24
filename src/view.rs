#[path = "commit_input.rs"]
mod commit_input;
#[path = "diff_view.rs"]
mod diff_view;
#[path = "graph.rs"]
mod graph;

use crate::{kit, navigate::Action as Move, stash::HistoryItem, theme::*};
pub use commit_input::CommitInput;
pub(crate) use diff_view::{DiffLine, Layout as DiffLayout, Mode as DiffMode};
use gpui::{
    App, ClipboardItem, Context, CursorStyle, Div, DragMoveEvent, Entity, FocusHandle, Focusable,
    FontWeight, KeyBinding, MouseButton, MouseDownEvent, Pixels, Point, ScrollHandle, ScrollStrategy,
    UniformListScrollHandle, Window, actions, anchored, deferred, div, prelude::*, px, rgb,
    uniform_list,
};
use graph::Graph;
use std::{
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub type HistoryRow = crate::repository::Commit;

const SIDEBAR_WIDTH: f32 = 200.;
const PANEL_WIDTH: f32 = 300.;
const SIDEBAR_MIN: f32 = 146.;
const PANEL_MIN: f32 = 240.;
const SIDEBAR_MAX: f32 = 420.;
const PANEL_MAX: f32 = 580.;
const CENTRE_MIN: f32 = 290.;
const DIVIDER_WIDTH: f32 = 5.;
const WIDE_CENTRE: f32 = 640.;
const HEADINGS: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct ColumnWidths {
    sidebar: f32,
    panel: f32,
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self { sidebar: SIDEBAR_WIDTH, panel: PANEL_WIDTH }
    }
}

impl ColumnWidths {
    #[cfg(test)]
    pub(crate) fn new(sidebar: f32, panel: f32) -> Self {
        Self { sidebar, panel }
    }

    pub(crate) fn sanitized(self) -> Self {
        let clamp = |value: f32, min: f32, max: f32, default: f32| {
            if value.is_finite() { value.clamp(min, max) } else { default }
        };
        Self {
            sidebar: clamp(self.sidebar, SIDEBAR_MIN, SIDEBAR_MAX, SIDEBAR_WIDTH),
            panel: clamp(self.panel, PANEL_MIN, PANEL_MAX, PANEL_WIDTH),
        }
    }

    fn budget(viewport: f32) -> f32 {
        (viewport - CENTRE_MIN - DIVIDER_WIDTH * 2.).max(SIDEBAR_MIN + PANEL_MIN)
    }

    fn fit(self, viewport: f32) -> Self {
        let extra = self.sidebar - SIDEBAR_MIN + self.panel - PANEL_MIN;
        let available = Self::budget(viewport) - SIDEBAR_MIN - PANEL_MIN;
        if extra <= available {
            return self;
        }
        Self {
            sidebar: SIDEBAR_MIN + (self.sidebar - SIDEBAR_MIN) * available / extra,
            panel: PANEL_MIN + (self.panel - PANEL_MIN) * available / extra,
        }
    }

    fn dragged(self, column: Column, viewport: f32, x: f32) -> Self {
        let mut widths = self.fit(viewport);
        let budget = Self::budget(viewport);
        match column {
            Column::Sidebar => {
                widths.sidebar = (x - DIVIDER_WIDTH / 2.)
                    .clamp(SIDEBAR_MIN, (budget - widths.panel).max(SIDEBAR_MIN).min(SIDEBAR_MAX));
            }
            Column::Panel => {
                widths.panel = (viewport - x - DIVIDER_WIDTH / 2.)
                    .clamp(PANEL_MIN, (budget - widths.sidebar).max(PANEL_MIN).min(PANEL_MAX));
            }
        }
        widths
    }

    pub(crate) fn layout(self, viewport: f32, shown: Shown) -> (f32, f32, f32) {
        let sidebar = if shown.sidebar { self.sidebar } else { 0. };
        let panel = if shown.panel { self.panel } else { 0. };
        let dividers = DIVIDER_WIDTH * (shown.sidebar as u8 + shown.panel as u8) as f32;
        (sidebar, viewport - sidebar - panel - dividers, panel)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub sidebar: bool,
    pub panel: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Column {
    Sidebar,
    Panel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Changes,
    Commit(String),
    Stash(String),
    File {
        path: PathBuf,
        staged: bool,
    },
}

#[derive(Clone, Debug)]
pub struct FileRow {
    pub path: PathBuf,
    pub staged: bool,
    pub status: &'static str,
}

#[derive(Clone, Debug)]
pub enum Menu {
    Repo,
    Remotes {
        at: Point<Pixels>,
    },
    CommitActions,
    Object {
        target: crate::commands::Target,
        at: Point<Pixels>,
    },
    Labels {
        oid: String,
        at: Point<Pixels>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum Surface {
    Menu(Menu),
    Branches,
    PullOptions,
    PushBranch,
    StashOptions,
}

impl Surface {
    pub(crate) fn reopened_by(&self, action: &Action) -> bool {
        match (self, action) {
            (Self::Menu(open), Action::Menu(Some(menu))) => match (open, menu) {
                (Menu::Repo, Menu::Repo)
                | (Menu::CommitActions, Menu::CommitActions)
                | (Menu::Remotes { .. }, Menu::Remotes { .. }) => true,
                (Menu::Labels { oid: open, .. }, Menu::Labels { oid, .. }) => open == oid,
                _ => false,
            },
            (Self::Branches, Action::Refs(crate::refs::Action::ToggleList))
            | (Self::PullOptions, Action::Sync(crate::sync::Action::ToggleOptions))
            | (Self::PushBranch, Action::Publish(crate::publish::Action::PushBranch))
            | (Self::StashOptions, Action::Stash(crate::stash::Action::ToggleOptions)) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Discard {
    File(PathBuf),
    Hunk(usize),
}

#[derive(Debug)]
pub(crate) enum DiscardAsk {
    File {
        path: PathBuf,
        untracked: bool,
    },
    Hunk {
        path: PathBuf,
        index: usize,
        diff: std::sync::Arc<crate::repository::Diff>,
    },
    Files {
        paths: Vec<PathBuf>,
        untracked: std::collections::BTreeSet<PathBuf>,
    },
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn describe_discard(ask: &DiscardAsk) -> crate::prompt::Spec {
    use crate::prompt::Spec;
    match ask {
        DiscardAsk::File { path, untracked: true } => {
            Spec::new(format!("Discard changes to {}?", base_name(path)), "Discard file")
                .line("This file is not tracked, so discarding deletes it. This cannot be undone.")
        }
        DiscardAsk::File { path, untracked: false } => {
            Spec::new(format!("Discard changes to {}?", base_name(path)), "Discard changes").line(
                "Unstaged edits to this file will be lost. Staged changes are kept. This cannot be undone.",
            )
        }
        DiscardAsk::Hunk { path, index, diff } => {
            let at = diff
                .hunks
                .get(*index)
                .map(|hunk| format!(" at line {}", hunk.start_line))
                .unwrap_or_default();
            Spec::new(format!("Discard this hunk{at} of {}?", base_name(path)), "Discard hunk")
                .line("These lines return to their staged content. This cannot be undone.")
        }
        DiscardAsk::Files { paths, untracked } => crate::picked::describe_discard(paths, untracked),
    }
    .danger()
}

pub use crate::notice::Tone;


pub(crate) const REPO_MENU: &[&str] =
    &["repo.open", "repo.close", "repo.refresh", "repo.reveal", "repo.terminal", "repo.editor", "repo.labels"];

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Side, Target, busy, operating, on_repo};
    fn worktree(target: &Target, staged: bool) -> Option<&PathBuf> {
        match target {
            Target::File { path, side: Side::Staged } if staged => Some(path),
            Target::File { path, side: Side::Unstaged } if !staged => Some(path),
            _ => None,
        }
    }
    fn picked(target: &Target, staged: bool) -> Option<&Vec<PathBuf>> {
        match target {
            Target::Files { paths, side: Side::Staged } if staged => Some(paths),
            Target::Files { paths, side: Side::Unstaged } if !staged => Some(paths),
            _ => None,
        }
    }
    fn plural(target: &Target, staged: bool, one: &str, many: &str) -> String {
        match picked(target, staged) {
            Some(paths) => many.replace("{n}", &paths.len().to_string()),
            None => one.to_owned(),
        }
    }
    fn index_edit(ui: &Ui) -> Option<String> {
        busy(ui).or_else(|| ui.operation.refusal())
    }
    fn file(ui: &Ui, target: &Target, staged: bool) -> Availability {
        if worktree(target, staged).is_some() || picked(target, staged).is_some() {
            Availability::unless(index_edit(ui))
        } else {
            Availability::Hidden
        }
    }
    fn all_of(ui: &Ui, target: &Target, staged: bool) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        let count = if staged { ui.file_ranges().1.len() } else { ui.file_ranges().0.len() };
        Availability::unless(index_edit(ui).or_else(|| {
            (count == 0).then(|| if staged { "Nothing is staged" } else { "Nothing to stage" }.to_owned())
        }))
    }
    fn dialog(ui: &Ui) -> Option<String> {
        ui.dialog_open().then(|| "Close the open dialog first".to_owned())
    }
    fn uncommittable(ui: &Ui, cx: &App) -> Option<String> {
        index_edit(ui).or_else(|| operating(ui)).or_else(|| dialog(ui)).or_else(|| ui.identity.reason()).or_else(|| {
            ui.input
                .read(cx)
                .text()
                .trim()
                .is_empty()
                .then(|| "Write a summary for the commit".to_owned())
        })
    }
    fn commit_all(ui: &Ui, target: &Target, cx: &App) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(uncommittable(ui, cx).or_else(|| {
            if ui.amend.is_some() {
                Some("Turn off Amend to stage everything and commit".to_owned())
            } else if ui.files.is_empty() {
                Some("There are no changes to commit".to_owned())
            } else {
                None
            }
        }))
    }
    fn commit(ui: &Ui, target: &Target, cx: &App) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        if ui.operation.continue_label().is_some() {
            return Availability::unless(busy(ui).or_else(|| dialog(ui)).or_else(|| ui.operation.blocked()));
        }
        if ui.can_commit(cx) {
            return Availability::Enabled;
        }
        let reason = uncommittable(ui, cx)
            .or_else(|| match &ui.amend {
                Some(amend) if amend.original.is_none() => {
                    Some("Wait for the last commit's message to load".to_owned())
                }
                Some(_) => Some("The last commit changed since amend was chosen".to_owned()),
                None => Some("Stage a change to commit".to_owned()),
            });
        Availability::unless(reason)
    }
    &[
        Command::new(
            "repo.refresh",
            Group::Repository,
            |_, _| "Refresh".into(),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::unless(busy(ui)) } else { Availability::Hidden }
            },
            |_, _| Some(Action::Refresh),
        )
        .words(&["reload", "rescan"])
        .icon("fetch")
        .keys("cmd-r")
        .bar(Bar::File),
        Command::new(
            "repo.stage-all",
            Group::Changes,
            |_, _| "Stage all".into(),
            |ui, target, _| all_of(ui, target, false),
            |_, _| Some(Action::StageAll),
        )
        .icon("plus")
        .keys("cmd-shift-s")
        .bar(Bar::Repository),
        Command::new(
            "repo.unstage-all",
            Group::Changes,
            |_, _| "Unstage all".into(),
            |ui, target, _| all_of(ui, target, true),
            |_, _| Some(Action::UnstageAll),
        )
        .icon("minus")
        .keys("cmd-shift-u")
        .bar(Bar::Repository),
        Command::new(
            "repo.commit",
            Group::Changes,
            |ui, _| match ui.operation.continue_label() {
                Some(label) => label.into(),
                None if ui.amend.is_some() => "Amend commit".into(),
                None => "Commit".into(),
            },
            commit,
            |ui, _| {
                Some(match ui.operation.continue_label() {
                    Some(_) => Action::Operation(crate::operation::Action::Continue),
                    None => Action::Commit,
                })
            },
        )
        .icon("commit")
        .keys("cmd-enter")
        .bar(Bar::Repository),
        Command::new(
            "repo.commit-all",
            Group::Changes,
            |_, _| "Stage all and commit".into(),
            commit_all,
            |_, _| Some(Action::CommitAll),
        )
        .icon("commit")
        .keys("cmd-shift-enter")
        .bar(Bar::Repository),
        Command::new(
            "repo.focus-summary",
            Group::Changes,
            |_, _| "Write the commit summary".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(operating(ui).or_else(|| dialog(ui)))
            },
            |_, _| Some(Action::FocusSummary),
        )
        .words(&["commit message"])
        .icon("commit")
        .keys("cmd-shift-m"),
        Command::new(
            "file.stage",
            Group::Changes,
            |_, target| plural(target, false, "Stage file", "Stage {n} files"),
            |ui, target, _| file(ui, target, false),
            |_, target| {
                worktree(target, false).map(|path| Action::StagePath(path.clone())).or_else(|| {
                    picked(target, false).map(|paths| Action::Picked(crate::picked::Action::Stage(paths.clone())))
                })
            },
        )
        .icon("plus")
        .keys("s"),
        Command::new(
            "file.unstage",
            Group::Changes,
            |_, target| plural(target, true, "Unstage file", "Unstage {n} files"),
            |ui, target, _| file(ui, target, true),
            |_, target| {
                worktree(target, true).map(|path| Action::UnstagePath(path.clone())).or_else(|| {
                    picked(target, true).map(|paths| Action::Picked(crate::picked::Action::Unstage(paths.clone())))
                })
            },
        )
        .icon("minus")
        .keys("u"),
        Command::new(
            "file.discard",
            Group::Changes,
            |_, target| plural(target, false, "Discard changes…", "Discard {n} files…"),
            |ui, target, _| file(ui, target, false),
            |_, target| {
                worktree(target, false).map(|path| Action::AskDiscard(Discard::File(path.clone()))).or_else(|| {
                    picked(target, false).map(|paths| Action::Picked(crate::picked::Action::AskDiscard(paths.clone())))
                })
            },
        )
        .words(&["revert", "throw away"])
        .icon("undo")
        .danger(),
        Command::new(
            "view.sidebar",
            Group::View,
            |ui, _| if ui.sidebar_hidden { "Show sidebar" } else { "Hide sidebar" }.into(),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(Action::ToggleSidebar),
        )
        .words(&["branches", "left"])
        .keys("cmd-j")
        .bar(Bar::View),
        Command::new(
            "view.panel",
            Group::View,
            |ui, _| if ui.panel_hidden { "Show details panel" } else { "Hide details panel" }.into(),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(Action::TogglePanel),
        )
        .words(&["details", "files", "right"])
        .keys("cmd-k")
        .bar(Bar::View),
        Command::new(
            "view.diff-mode",
            Group::View,
            |ui, _| match ui.diff_mode {
                DiffMode::Unified => "Show diffs side by side",
                DiffMode::Split => "Show diffs inline",
            }
            .into(),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |ui, _| {
                Some(Action::DiffMode(match ui.diff_mode {
                    DiffMode::Unified => DiffMode::Split,
                    DiffMode::Split => DiffMode::Unified,
                }))
            },
        )
        .words(&["split", "unified", "side by side"])
        .bar(Bar::View),
    ]
};

#[derive(Clone, Debug)]
pub enum Action {
    Open,
    Refresh,
    ViewChanges,
    StageAll,
    UnstageAll,
    Inspect(crate::inspection::Action),
    Refs(crate::refs::Action),
    Branch(crate::branch::Action),
    Tag(crate::tag::Action),
    Tracking(crate::tracking::Action),
    Search(crate::search::Action),
    Stash(crate::stash::Action),
    Sync(crate::sync::Action),
    Publish(crate::publish::Action),
    Merge(crate::merge::Action),
    Operation(crate::operation::Action),
    Conflict(crate::conflict::Action),
    Reverse(crate::reverse::Action),
    Pick(crate::pick::Action),
    Picked(crate::picked::Action),
    Rebase(crate::rebase::Action),
    Autostash(crate::autostash::Action),
    Explain(crate::explain::Action),
    Select(Selection),
    More,
    StageHunk(usize),
    UnstageHunk(usize),
    StageFile,
    StagePath(PathBuf),
    UnstagePath(PathBuf),
    Commit,
    CommitAll,
    FocusSummary,
    ToggleSidebar,
    TogglePanel,
    Help(crate::help::Action),
    Palette(crate::palette::Action),
    ToggleAmend,
    AskDiscard(Discard),
    Prompt(crate::prompt::Action),
    OpenDiff,
    CloseDiff,
    DiffMode(DiffMode),
    Menu(Option<Menu>),
    Dismiss,
    Notice(crate::notice::Action),
    CopyText {
        text: String,
        what: &'static str,
    },
    Run(&'static str),
    Activate(crate::commands::Target),
    MenuKey(crate::commands::Key),
    MenuHover(crate::commands::Cursor),
    Navigate(crate::navigate::Action),
    Handoff(crate::handoff::Action),
    Jobs(crate::jobs::Action),
    Credentials(crate::credentials::Action),
    Connect(crate::connect::Action),
    Address(crate::address::Action),
    Activity(crate::activity::Action),
    Clone(crate::clone::Action),
    Init(crate::init::Action),
    Start(crate::start::Action),
    Preferences(crate::preferences::Action),
    Identity(crate::identity::Action),
    AutoFetch(crate::autofetch::Action),
    Tabs(crate::tabs::Action),
    Tools(crate::tools::Action),
    Cli(crate::cli::Action),
}

#[derive(Debug, PartialEq, Eq)]
pub struct CommitDraft {
    pub summary: String,
    pub description: String,
}

impl CommitDraft {
    pub fn from_message(message: &str) -> Self {
        let message = message.trim_end_matches('\n');
        let (summary, body) = message.split_once('\n').unwrap_or((message, ""));
        Self {
            summary: summary.to_owned(),
            description: body.strip_prefix('\n').unwrap_or(body).to_owned(),
        }
    }

    pub fn message(&self) -> String {
        let summary = self.summary.trim();
        let description = self.description.trim_matches('\n');
        if description.trim().is_empty() {
            summary.to_owned()
        } else {
            format!("{summary}\n\n{description}")
        }
    }
}

pub struct AmendDraft {
    pub head: String,
    pub reference: Option<OsString>,
    pub previous: CommitDraft,
    pub original: Option<CommitDraft>,
}

pub struct Ui {
    pub repo: String,
    pub root: PathBuf,
    pub branch: String,
    pub upstream: Option<String>,
    pub head: Option<String>,
    pub head_ref: Option<OsString>,
    pub detached: bool,
    pub history: Vec<HistoryRow>,
    pub selection: Option<Selection>,
    pub(crate) picked: Option<crate::picked::Picked<PathBuf>>,
    pub files: Vec<FileRow>,
    pub changed_paths: usize,
    pub operation: crate::operation::State,
    pub inspection: crate::inspection::State,
    pub refs: crate::refs::State,
    pub search: crate::search::State,
    pub stash: crate::stash::State,
    pub autostash: crate::autostash::State,
    pub sync: crate::sync::State,
    pub publish: crate::publish::State,
    pub(crate) connect: crate::connect::State,
    pub merge: crate::merge::State,
    pub conflict: crate::conflict::State,
    pub diff_title: String,
    pub diff: Vec<DiffLine>,
    pub diff_open: bool,
    pub notices: crate::notice::State,
    pub opening: Option<String>,
    pub loading_diff: bool,
    pub menu: Option<Menu>,
    pub menu_cursor: crate::commands::Cursor,
    pub prompt: crate::prompt::State,
    pub help: crate::help::State,
    pub palette: crate::palette::State,
    pub sidebar_hidden: bool,
    pub panel_hidden: bool,
    pub(crate) title_shown: Option<String>,
    pub(crate) bar_shown: Vec<String>,
    pub jobs: crate::jobs::State,
    pub activity: crate::activity::State,
    pub cloning: crate::clone::State,
    pub init: crate::init::State,
    pub(crate) start: crate::start::State,
    pub(crate) preferences: crate::preferences::State,
    pub(crate) identity: crate::identity::State,
    pub(crate) autofetch: crate::autofetch::State,
    pub(crate) tools: crate::tools::State,
    pub has_more: bool,
    pub input: Entity<CommitInput>,
    pub description: Entity<CommitInput>,
    pub operation_message: Entity<CommitInput>,
    pub amend: Option<AmendDraft>,
    pub history_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    pub(crate) focus_on_open: bool,
    pub(crate) file_scroll: UniformListScrollHandle,
    pub(crate) diff_scroll: UniformListScrollHandle,
    pub(crate) navigate: crate::navigate::State,
    pub(crate) modal_focus: FocusHandle,
    modal_drawn: bool,
    focus_lost: Option<gpui::Subscription>,
    pub(crate) closed_by_press: Option<Surface>,
    columns: ColumnWidths,
    diff_layout: DiffLayout,
    pub(crate) diff_mode: DiffMode,
    split_pan: ScrollHandle,
    requested_at: Option<usize>,
    graph: Graph,
}

impl Ui {
    pub fn new(cx: &mut Context<crate::Tab>) -> Self {
        let settings = crate::settings::settings(cx);
        let columns = crate::settings::memory(cx)
            .and_then(|memory| memory.columns)
            .unwrap_or_default();
        let input = cx.new(CommitInput::new);
        let description = cx.new(CommitInput::description);
        let operation_message = cx.new(|cx| CommitInput::multiline(cx, "Commit message"));
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        cx.observe(&description, |_, _, cx| cx.notify()).detach();
        cx.observe(&operation_message, |_, _, cx| cx.notify()).detach();
        Self {
            repo: String::new(),
            root: PathBuf::new(),
            branch: String::new(),
            upstream: None,
            head: None,
            head_ref: None,
            detached: false,
            history: vec![],
            selection: None,
            picked: None,
            files: vec![],
            changed_paths: 0,
            operation: Default::default(),
            inspection: crate::inspection::State::default(),
            refs: crate::refs::State::new(settings.show_ref_labels, cx),
            search: crate::search::State::new(cx),
            stash: Default::default(),
            autostash: crate::autostash::State::new(settings.stash_in_the_way),
            sync: crate::sync::State::new(Default::default(), settings.prune_on_fetch),
            publish: crate::publish::State::new(cx),
            connect: Default::default(),
            merge: Default::default(),
            conflict: Default::default(),
            diff_title: String::new(),
            diff: vec![],
            diff_open: false,
            notices: Default::default(),
            opening: None,
            loading_diff: false,
            menu: None,
            menu_cursor: Default::default(),
            prompt: crate::prompt::State::new(cx),
            help: crate::help::State::new(cx),
            palette: crate::palette::State::new(cx),
            sidebar_hidden: settings.sidebar_hidden,
            panel_hidden: settings.panel_hidden,
            title_shown: None,
            bar_shown: Vec::new(),
            jobs: Default::default(),
            activity: Default::default(),
            cloning: crate::clone::State::new(cx),
            init: crate::init::State::new(cx),
            start: crate::start::State::new(cx),
            preferences: crate::preferences::State::new(cx),
            identity: crate::identity::State::new(cx),
            autofetch: Default::default(),
            tools: crate::tools::State::new(cx),
            has_more: false,
            input,
            description,
            operation_message,
            amend: None,
            focus: cx.focus_handle(),
            focus_on_open: true,
            graph: Graph::default(),
            diff_layout: DiffLayout::default(),
            diff_mode: settings.diff_mode,
            split_pan: ScrollHandle::new(),
            requested_at: None,
            history_scroll: UniformListScrollHandle::new(),
            file_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            navigate: Default::default(),
            modal_focus: cx.focus_handle(),
            modal_drawn: false,
            focus_lost: None,
            closed_by_press: None,
            columns,
        }
    }

    pub fn replace_history(&mut self, rows: Vec<HistoryRow>) {
        self.requested_at = None;
        self.graph = Graph::default();
        self.history.clear();
        self.append_history(rows);
    }

    pub fn append_history(&mut self, rows: Vec<HistoryRow>) {
        for row in &rows {
            self.graph.push(&row.oid, &row.parents);
        }
        self.stash.place(&rows, self.history.len());
        self.history.extend(rows);
    }

    pub(crate) fn history_items(&self) -> usize {
        self.history.len() + self.stash.placed()
    }

    pub(crate) fn selected_history_row(&self) -> Option<usize> {
        match self.selection.as_ref()? {
            Selection::Commit(oid) => self.history.iter()
                .position(|row| &row.oid == oid)
                .map(|commit| self.stash.commit_row(commit)),
            Selection::Stash(oid) => self.stash.stash_row(oid),
            _ => None,
        }
    }

    pub fn scroll_to_commit(&self, oid: &str) {
        if let Some(index) = self.history.iter().position(|commit| commit.oid == oid) {
            self.history_scroll.scroll_to_item(self.stash.commit_row(index), ScrollStrategy::Center);
        }
    }

    pub fn replace_diff(&mut self, lines: Vec<DiffLine>, layout: DiffLayout) {
        self.diff = lines;
        self.diff_layout = layout;
        self.split_pan.set_offset(Point::default());
        self.diff_scroll = UniformListScrollHandle::new();
    }

    pub fn draft(&self, cx: &App) -> CommitDraft {
        CommitDraft {
            summary: self.input.read(cx).text().to_owned(),
            description: self.description.read(cx).text().to_owned(),
        }
    }

    pub fn set_draft(&self, draft: &CommitDraft, cx: &mut App) {
        self.input.update(cx, |input, cx| input.set_text(&draft.summary, cx));
        self.description.update(cx, |input, cx| input.set_text(&draft.description, cx));
    }

    pub fn checked_out_branch(&self) -> Option<&str> {
        (!self.detached).then_some(self.branch.as_str())
    }

    fn amend_target_changed(&self, amend: &AmendDraft) -> bool {
        self.head.as_deref() != Some(amend.head.as_str())
            || self.head_ref != amend.reference
    }

    pub fn can_commit(&self, cx: &App) -> bool {
        self.can_edit_index()
            && !self.prompt.open()
            && self.refs.draft.is_none()
            && !self.operation.active()
            && !self.publish.open()
            && self.identity.reason().is_none()
            && !self.input.read(cx).text().trim().is_empty()
            && match &self.amend {
                Some(amend) => amend.original.is_some() && !self.amend_target_changed(amend),
                None => !self.file_ranges().1.is_empty(),
            }
    }

    pub(crate) fn dialog_open(&self) -> bool {
        self.prompt.open() || self.publish.open() || self.refs.draft.is_some() || self.cloning.open() || self.init.open()
    }

    pub(crate) fn window_title(&self) -> String {
        if self.repo.is_empty() {
            return "FastKraken".to_owned();
        }
        if self.branch.is_empty() {
            return self.repo.clone();
        }
        let branch = match (self.detached, &self.head) {
            (true, Some(head)) => format!("detached at {}", &head[..head.len().min(7)]),
            _ => self.branch.clone(),
        };
        format!("{} — {}", self.repo, branch)
    }

    pub fn can_commit_all(&self, cx: &App) -> bool {
        self.can_edit_index()
            && !self.dialog_open()
            && !self.operation.active()
            && self.amend.is_none()
            && self.identity.reason().is_none()
            && !self.files.is_empty()
            && !self.input.read(cx).text().trim().is_empty()
    }

    pub(crate) fn busy(&self) -> bool {
        self.jobs.write.is_some()
    }

    pub(crate) fn can_edit_index(&self) -> bool {
        !self.busy() && !self.operation.active()
    }

    pub fn changed_count(&self) -> usize {
        self.changed_paths
    }

    pub fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.notices.say(tone, &text.into());
    }

    pub fn notice_text(&self) -> String {
        self.notices.text()
    }

    pub(crate) fn modal_open(&self) -> bool {
        (self.publish.open() && !self.publish.branch_open())
            || self.refs.draft.is_some()
            || self.cloning.open()
            || self.init.open()
            || self.help.open()
            || self.palette.open()
    }

    pub(crate) fn open_surface(&self) -> Option<Surface> {
        if let Some(menu) = &self.menu {
            Some(Surface::Menu(menu.clone()))
        } else if self.refs.list_open {
            Some(Surface::Branches)
        } else if self.sync.options_open() {
            Some(Surface::PullOptions)
        } else if self.publish.branch_open() {
            Some(Surface::PushBranch)
        } else if self.stash.options_open() {
            Some(Surface::StashOptions)
        } else {
            None
        }
    }

    pub fn transient_open(&self) -> bool {
        self.menu.is_some()
            || self.refs.list_open
            || self.sync.options_open()
            || self.publish.branch_open()
            || self.stash.options_open()
    }

    pub(crate) fn file_ranges(&self) -> (Range<usize>, Range<usize>) {
        let first_staged = self.files.first().is_some_and(|f| f.staged);
        let mid = self.files.partition_point(|f| f.staged == first_staged);
        if first_staged {
            (mid..self.files.len(), 0..mid)
        } else {
            (0..mid, mid..self.files.len())
        }
    }

    pub(crate) fn shown(&self) -> Shown {
        Shown { sidebar: !self.sidebar_hidden, panel: !self.panel_hidden }
    }

    pub(crate) fn columns_drawn(&self, viewport: f32) -> (f32, f32, f32) {
        self.columns.fit(viewport).layout(viewport, self.shown())
    }

    pub(crate) fn column_widths(&self) -> (f32, f32) {
        (self.columns.sidebar, self.columns.panel)
    }

    pub(crate) fn working(&self) -> bool {
        matches!(
            self.selection,
            Some(Selection::Changes | Selection::File { .. })
        )
    }
}

pub fn focus(ui: &Ui, window: &mut Window) {
    window.focus(&ui.focus);
}

pub trait Desk: Render {
    fn ui(&self) -> &Ui;
    fn ui_mut(&mut self) -> &mut Ui;
    fn dispatch(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>);
}

actions!(
    fastkraken_desk,
    [
        QuitApp,
        HideApp,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        FullScreen,
        Undo,
        Redo,
        ChooseSearchResult,
        NextFocus,
        PreviousFocus,
        ActivateControl,
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        MovePageUp,
        MovePageDown,
        MoveHome,
        MoveEnd,
        MoveFirst,
        MoveLast,
        FollowParent,
        FollowChild,
        ChooseRow,
        FocusDesk
    ]
);

pub fn bindings(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    cx.on_action(|_: &QuitApp, cx| cx.quit());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    let desk = Some("Desk");
    let lists = Some("Desk && !CommitField");
    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-q"), QuitApp, None),
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, desk),
        KeyBinding::new("ctrl-cmd-f", FullScreen, desk),
        KeyBinding::new("tab", NextFocus, desk),
        KeyBinding::new("shift-tab", PreviousFocus, desk),
        KeyBinding::new("escape", FocusDesk, desk),
        KeyBinding::new("up", MoveUp, lists),
        KeyBinding::new("k", MoveUp, lists),
        KeyBinding::new("down", MoveDown, lists),
        KeyBinding::new("j", MoveDown, lists),
        KeyBinding::new("left", MoveLeft, lists),
        KeyBinding::new("h", MoveLeft, lists),
        KeyBinding::new("right", MoveRight, lists),
        KeyBinding::new("l", MoveRight, lists),
        KeyBinding::new("pageup", MovePageUp, lists),
        KeyBinding::new("pagedown", MovePageDown, lists),
        KeyBinding::new("home", MoveHome, lists),
        KeyBinding::new("end", MoveEnd, lists),
        KeyBinding::new("cmd-up", MoveFirst, lists),
        KeyBinding::new("cmd-down", MoveLast, lists),
        KeyBinding::new("shift-down", FollowParent, lists),
        KeyBinding::new("shift-j", FollowParent, lists),
        KeyBinding::new("shift-up", FollowChild, lists),
        KeyBinding::new("shift-k", FollowChild, lists),
        KeyBinding::new("enter", ChooseRow, lists),
        KeyBinding::new("up", MoveUp, Some("BranchSwitcher > CommitField")),
        KeyBinding::new("down", MoveDown, Some("BranchSwitcher > CommitField")),
        KeyBinding::new("enter", ChooseRow, Some("BranchSwitcher > CommitField")),
        KeyBinding::new("up", MoveUp, Some("CommandPalette > CommitField")),
        KeyBinding::new("down", MoveDown, Some("CommandPalette > CommitField")),
        KeyBinding::new("pageup", MovePageUp, Some("CommandPalette > CommitField")),
        KeyBinding::new("pagedown", MovePageDown, Some("CommandPalette > CommitField")),
        KeyBinding::new("enter", ChooseRow, Some("CommandPalette > CommitField")),
        KeyBinding::new("up", MoveUp, Some("StartScreen > CommitField")),
        KeyBinding::new("down", MoveDown, Some("StartScreen > CommitField")),
        KeyBinding::new("pageup", MovePageUp, Some("StartScreen > CommitField")),
        KeyBinding::new("pagedown", MovePageDown, Some("StartScreen > CommitField")),
        KeyBinding::new("enter", ChooseRow, Some("StartScreen > CommitField")),
        KeyBinding::new("up", MoveUp, Some("HistorySearch > CommitField")),
        KeyBinding::new("down", MoveDown, Some("HistorySearch > CommitField")),
        KeyBinding::new("enter", ChooseSearchResult, Some("HistorySearch > CommitField")),
        KeyBinding::new("enter", ActivateControl, Some("DeskButton")),
        KeyBinding::new("space", ActivateControl, Some("DeskButton")),
    ]);
    cx.bind_keys(crate::commands::bindings());
    cx.bind_keys((0..crate::tabs::NUMBERED).map(|index| {
        KeyBinding::new(&format!("{modifier}-{}", index + 1), crate::tabs::ShowTab { index }, desk)
    }));
    commit_input::bindings(cx);
    crate::prompt::bindings(cx);
}

pub(crate) const OTHER_KEYS: &[(&str, &str)] = &[
    ("cmd-1", "Show the first tab; Cmd-2 to Cmd-9 show the others"),
    ("up", "Previous row"),
    ("down", "Next row"),
    ("k", "Previous row"),
    ("j", "Next row"),
    ("left", "Move to the sidebar, or from the files back to the graph"),
    ("right", "Move from the sidebar to the graph, or to the files"),
    ("h", "Move left between the sidebar, graph and files"),
    ("l", "Move right between the sidebar, graph and files"),
    ("enter", "Open the highlighted file or row"),
    ("pageup", "Up a page, or scroll the open file up"),
    ("pagedown", "Down a page, or scroll the open file down"),
    ("home", "First row, or the top of the open file"),
    ("end", "Last row, or the bottom of the open file"),
    ("cmd-up", "First row"),
    ("cmd-down", "Last row"),
    ("shift-down", "Follow the commit's first parent, or pick the next file too"),
    ("shift-up", "Back along the first parent, or pick the previous file too"),
    ("tab", "Next control"),
    ("shift-tab", "Previous control"),
    ("escape", "Close what is open"),
    ("cmd-m", "Minimize the window"),
    ("ctrl-cmd-f", "Enter or leave full screen"),
    ("cmd-h", "Hide FastKraken"),
    ("alt-cmd-h", "Hide other apps"),
    ("cmd-q", "Quit FastKraken"),
];

pub(crate) fn menu_bar(ui: &Ui, recents: &[(String, PathBuf)], cx: &App) -> Vec<gpui::Menu> {
    use crate::commands::{Bar, bar_index, bar_items};
    use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
    let mut app = bar_items(ui, Bar::App);
    app.extend([
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide FastKraken", HideApp),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit FastKraken", QuitApp),
    ]);
    let mut edit = vec![
        MenuItem::os_action("Undo", Undo, OsAction::Undo),
        MenuItem::os_action("Redo", Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", commit_input::CutSummary, OsAction::Cut),
        MenuItem::os_action("Copy", commit_input::CopySummary, OsAction::Copy),
        MenuItem::os_action("Paste", commit_input::PasteSummary, OsAction::Paste),
        MenuItem::os_action("Select All", commit_input::SelectSummary, OsAction::SelectAll),
        MenuItem::separator(),
    ];
    edit.extend(bar_items(ui, Bar::Edit));
    let mut view = bar_items(ui, Bar::View);
    view.extend([MenuItem::separator(), MenuItem::action("Enter Full Screen", FullScreen)]);
    let mut file = bar_items(ui, Bar::File);
    let at = bar_index(Bar::File, "repo.open").map_or(file.len(), |at| (at + 1).min(file.len()));
    file.insert(at, crate::start::bar_menu(recents));
    vec![
        Menu { name: "FastKraken".into(), items: app },
        Menu { name: "File".into(), items: file },
        Menu { name: "Edit".into(), items: edit },
        Menu { name: "View".into(), items: view },
        Menu { name: "Repository".into(), items: bar_items(ui, Bar::Repository) },
        Menu { name: "Branch".into(), items: bar_items(ui, Bar::Branch) },
        Menu { name: "Window".into(), items: window_items(ui, cx) },
        Menu { name: "Help".into(), items: bar_items(ui, Bar::Help) },
    ]
}

fn window_items(ui: &Ui, cx: &App) -> Vec<gpui::MenuItem> {
    use gpui::MenuItem;
    let mut items = vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom), MenuItem::separator()];
    items.extend(crate::commands::bar_items(ui, crate::commands::Bar::Window));
    let tabs = crate::tabs::bar_rows(cx);
    if !tabs.is_empty() {
        items.push(MenuItem::separator());
        items.extend(tabs);
    }
    items
}

fn sync_chrome<T: Desk>(app: &mut T, window: &mut Window, cx: &mut Context<T>) {
    let title = app.ui().window_title();
    if app.ui().title_shown.as_ref() != Some(&title) {
        window.set_window_title(&title);
        app.ui_mut().title_shown = Some(title);
    }
    let recents = crate::start::others(app.ui(), cx, crate::start::IN_BAR);
    let mut titles = crate::commands::bar_titles(app.ui());
    titles.extend(recents.iter().map(|(title, path)| format!("{title}\0{}", path.display())));
    titles.extend(crate::tabs::bar_titles(cx));
    if app.ui().bar_shown != titles {
        let menus = menu_bar(app.ui(), &recents, cx);
        cx.set_menus(menus);
        app.ui_mut().bar_shown = titles;
    }
}

fn file_selection(f: &FileRow) -> Selection {
    Selection::File {
        path: f.path.clone(),
        staged: f.staged,
    }
}

fn select<T: Desk>(app: &mut T, selected: Selection, w: &mut Window, cx: &mut Context<T>) {
    focus(app.ui(), w);
    app.dispatch(Action::Select(selected), w, cx);
}

pub(crate) fn press_started(ui: &mut Ui) {
    ui.closed_by_press = None;
}

pub(crate) fn pressed_outside<T: Desk>(app: &mut T, w: &mut Window, cx: &mut Context<T>) {
    let closed = app.ui().open_surface();
    app.ui_mut().closed_by_press = closed;
    app.dispatch(Action::Dismiss, w, cx);
}

fn navigate<T: Desk>(app: &mut T, action: Move, w: &mut Window, cx: &mut Context<T>) {
    app.dispatch(Action::Navigate(action), w, cx);
}

fn trap(ui: &Ui, forward: bool, window: &mut Window, cx: &App) {
    let started = window.focused(cx);
    for _ in 0..512 {
        if forward {
            window.focus_next();
        } else {
            window.focus_prev();
        }
        if ui.modal_focus.contains_focused(window, cx) {
            return;
        }
        if window.focused(cx) == started {
            break;
        }
    }
    window.focus(&ui.modal_focus);
}


pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn year_of(time: i64) -> i64 {
    let days = time.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    year_of_era + era * 400 + i64::from(month >= 10)
}

pub(crate) fn date_of(time: i64) -> String {
    let days = time.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub(crate) fn when(time: i64, date: &str, now: i64) -> String {
    let age = now - time;
    if time > 0 && (0..172_800).contains(&age) {
        return match age {
            0..60 => "now".into(),
            60..3_600 => format!("{} min ago", age / 60),
            3_600..7_200 => "1 hour ago".into(),
            7_200..86_400 => format!("{} hours ago", age / 3_600),
            _ => "yesterday".into(),
        };
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = date.split('-').map(|part| part.parse::<i64>().unwrap_or(0));
    let (year, month, day) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let Some(name) = MONTHS
        .get((month - 1).max(0) as usize)
        .filter(|_| day > 0)
    else {
        return date.to_owned();
    };
    if year == year_of(now) {
        format!("{day} {name}")
    } else {
        format!("{day} {name} {year}")
    }
}


fn held(child: impl IntoElement) -> Div {
    div()
        .relative()
        .flex_shrink_0()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
}

fn toolbar<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let open = !ui.repo.is_empty();
    let bar = div()
        .h(px(TOOLBAR))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(if cfg!(target_os = "macos") { 78. } else { 12. }))
        .pr(px(10.))
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count > 1 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        });
    if !open {
        return bar;
    }
    let repo_menu = matches!(ui.menu, Some(Menu::Repo));
    bar.child(
        held(
            kit::btn("repo-menu", ui.repo.clone())
                .ghost()
                .icon("folder")
                .trailing("chevron-down")
                .selected(repo_menu)
                .on(Action::Menu((!repo_menu).then_some(Menu::Repo)), cx),
        )
        .when(repo_menu, |d| {
            let entries = crate::commands::entries_of(ui, &Menu::Repo, cx);
            d.child(kit::popover(crate::commands::surface(entries, 300., ui.menu_cursor, cx), false))
        }),
    )
    .when(!ui.branch.is_empty(), |d| {
        let ready = !ui.operation.active() && !ui.busy();
        d.child(div().text_color(rgb(TEXT_OFF)).child("/")).child(
            held(
                kit::btn("branch-selector", ui.branch.clone())
                    .ghost()
                    .icon("branch")
                    .trailing("chevron-down")
                    .selected(ui.refs.list_open)
                    .enabled(ready)
                    .on(Action::Refs(crate::refs::Action::ToggleList), cx),
            )
            .when(ui.refs.list_open, |d| {
                d.child(kit::popover(crate::refs::switcher(app, cx), false))
            }),
        )
    })
    .child(
        div()
            .w(px(1.))
            .h(px(18.))
            .mx_1()
            .flex_shrink_0()
            .bg(rgb(LINE_STRONG)),
    )
    .child(held(crate::sync::toolbar(app, cx)))
    .child(kit::grow())
    .child(held(crate::stash::toolbar(app, cx)))
    .child(held(crate::search::toolbar(app, cx)))
}


pub(crate) struct Label {
    pub chip: kit::Chip,
    pub name: String,
    pub target: Option<crate::commands::Target>,
}

pub(crate) const SHOWN_LABELS: usize = 3;

pub(crate) fn labels(remotes: &[String], decorations: &str) -> Vec<Label> {
    use crate::commands::Target;
    let mut labels: Vec<(u8, Label)> = decorations
        .split(", ")
        .filter_map(|label| {
            let (order, chip, name) = if let Some(name) = label.strip_prefix("HEAD -> ") {
                (0, kit::Chip::Head, name)
            } else if label == "HEAD" {
                let head = Label { chip: kit::Chip::Head, name: "HEAD".into(), target: None };
                return Some((0, head));
            } else if let Some(name) = label.strip_prefix("tag: ") {
                (3, kit::Chip::Tag, name)
            } else if label.is_empty() || label.ends_with("/HEAD") {
                return None;
            } else if crate::refs::remote_ref(remotes, label) {
                (2, kit::Chip::Remote, label)
            } else {
                (1, kit::Chip::Local, label)
            };
            let target = match chip {
                kit::Chip::Remote => Target::RemoteBranch(name.into()),
                kit::Chip::Tag => Target::Tag(name.into()),
                _ => Target::Branch(name.into()),
            };
            Some((order, Label { chip, name: name.to_owned(), target: Some(target) }))
        })
        .collect();
    labels.sort_by_key(|label| label.0);
    labels.into_iter().map(|(_, label)| label).collect()
}

pub(crate) fn label_click(oid: &str, target: &crate::commands::Target, click_count: usize) -> Action {
    if click_count > 1 {
        Action::Activate(target.clone())
    } else {
        Action::Select(Selection::Commit(oid.to_owned()))
    }
}

pub(crate) fn label_menu(target: crate::commands::Target, at: Point<Pixels>) -> Action {
    Action::Menu(Some(Menu::Object { target, at }))
}

fn live_label<T: Desk>(
    chip: gpui::Stateful<Div>,
    oid: String,
    target: crate::commands::Target,
    cx: &Context<T>,
) -> gpui::Stateful<Div> {
    let menu_target = target.clone();
    chip.cursor_pointer()
        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
            cx.stop_propagation();
            focus(app.ui(), w);
            app.dispatch(label_click(&oid, &target, event.click_count()), w, cx);
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                cx.stop_propagation();
                app.dispatch(label_menu(menu_target.clone(), event.position), w, cx);
            }),
        )
}

fn show_hidden<T: Desk>(app: &mut T, oid: &str, at: Point<Pixels>, w: &mut Window, cx: &mut Context<T>) {
    cx.stop_propagation();
    select(app, Selection::Commit(oid.to_owned()), w, cx);
    app.dispatch(Action::Menu(Some(Menu::Labels { oid: oid.to_owned(), at })), w, cx);
}

fn ref_chips<T: Desk>(ui: &Ui, oid: &str, decorations: &str, cx: &Context<T>) -> Div {
    let labels = labels(&ui.refs.remotes, decorations);
    let hidden = labels.len().saturating_sub(SHOWN_LABELS);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .children(labels.into_iter().take(SHOWN_LABELS).enumerate().map(|(i, label)| {
            let branch = match &label.target {
                Some(crate::commands::Target::Branch(_)) => ui.refs.branch(&label.name),
                _ => None,
            };
            let distance = branch.and_then(crate::refs::distance);
            let text = match &distance {
                Some(distance) => format!("{} {distance}", label.name),
                None => label.name,
            };
            let chip = kit::chip(label.chip, text);
            let Some(target) = label.target else {
                return chip.into_any_element();
            };
            let tag = match &target {
                crate::commands::Target::Tag(name) => ui.refs.tag(name).map(crate::tag::tooltip),
                _ => None,
            };
            let live = live_label(chip.id(("label", i)), oid.to_owned(), target, cx);
            match (branch.filter(|item| item.upstream.is_some()), tag) {
                (Some(item), _) => live.tooltip(kit::tip(crate::tracking::tooltip(item))).into_any_element(),
                (None, Some(tip)) => live.tooltip(kit::tip(tip)).into_any_element(),
                (None, None) => live.into_any_element(),
            }
        }))
        .when(hidden > 0, |d| {
            let (left, right) = (oid.to_owned(), oid.to_owned());
            d.child(
                kit::chip(kit::Chip::Plain, format!("+{hidden}"))
                    .id("more-labels")
                    .cursor_pointer()
                    .tooltip(kit::tip(if hidden == 1 {
                        "1 more label".to_owned()
                    } else {
                        format!("{hidden} more labels")
                    }))
                    .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                        show_hidden(app, &left, event.position(), w, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                            show_hidden(app, &right, event.position, w, cx)
                        }),
                    ),
            )
        })
}

fn history_metadata(author: &str, date: &str, time: i64, oid: &str, now: i64, wide: bool, selected: bool) -> Div {
    let color = rgb(if selected { TEXT_2 } else { TEXT_3 });
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .child(
            div()
                .w(px(if wide { 140. } else { 28. }))
                .flex_shrink_0()
                .pr_2()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::avatar(author))
                .when(wide, |d| d.child(kit::fit(author.to_owned()).size(12.5).color(TEXT_2).grow())),
        )
        .child(
            div()
                .w(px(84.))
                .flex_shrink_0()
                .text_size(px(TEXT_SMALL))
                .text_color(color)
                .whitespace_nowrap()
                .child(when(time, date, now)),
        )
        .when(wide, |d| {
            d.child(
                div()
                    .w(px(66.))
                    .flex_shrink_0()
                    .font_family(MONO)
                    .text_size(px(11.5))
                    .text_color(color)
                    .child(oid.get(..7).unwrap_or(oid).to_owned()),
            )
        })
}

fn history<T: Desk>(app: &T, centre_width: f32, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let wide = centre_width >= WIDE_CENTRE;
    let author_width = px(if wide { 140. } else { 28. });
    let graph_width = px(ui.graph.width());
    let moment = now();
    let headings = div()
        .h(px(HEADINGS))
        .flex_shrink_0()
        .flex()
        .items_center()
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(div().w(graph_width).flex_shrink_0())
        .child(div().flex_1().child(kit::label("Description")))
        .child(
            div()
                .w(author_width)
                .flex_shrink_0()
                .when(wide, |d| d.child(kit::label("Author"))),
        )
        .child(div().w(px(84.)).flex_shrink_0().child(kit::label("Date")))
        .when(wide, |d| {
            d.child(div().w(px(66.)).flex_shrink_0().child(kit::label("SHA")))
        });
    let list = uniform_list(
        "history",
        ui.history_items(),
        cx.processor(move |app, range: Range<usize>, window, cx| {
            let size = app.ui().history.len();
            if app.ui().has_more
                && range.end.saturating_add(18) >= app.ui().history_items()
                && app.ui().requested_at != Some(size)
            {
                app.ui_mut().requested_at = Some(size);
                cx.defer_in(window, |app, w, cx| app.dispatch(Action::More, w, cx));
            }
            let ui = app.ui();
            range
                .map(|index| {
                    let i = match ui.stash.item(index) {
                        HistoryItem::Commit(i) => i,
                        HistoryItem::Stash { stash, base, first_at_base } => {
                            let entry = &ui.stash.entries[stash];
                            let oid = entry.oid.clone();
                            let active = matches!(&ui.selection, Some(Selection::Stash(id)) if id == &oid);
                            return kit::row(("stash-graph", stash), active)
                                .h(px(ROW))
                                .child(ui.graph.stash_element(base, first_at_base, active))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .pr(px(14.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(kit::chip(kit::Chip::Stash, "stash"))
                                        .child(kit::fit(entry.title.clone()).color(TEXT_2).grow()),
                                )
                                .child(history_metadata(&entry.author, &entry.date, entry.time, &entry.oid, moment, wide, active))
                                .on_click(cx.listener({
                                    let oid = oid.clone();
                                    move |app, _, w, cx| select(app, Selection::Stash(oid.clone()), w, cx)
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                        let target = crate::commands::Target::Stash(oid.clone());
                                        app.dispatch(
                                            Action::Menu(Some(Menu::Object { target, at: event.position })),
                                            w,
                                            cx,
                                        );
                                    }),
                                );
                        }
                    };
                    let row = &ui.history[i];
                    let (oid, menu_oid) = (row.oid.clone(), row.oid.clone());
                    let active = matches!(&ui.selection, Some(Selection::Commit(id)) if id == &oid);
                    let checked_out = ui.head.as_deref() == Some(row.oid.as_str());
                    let merge = row.parents.len() > 1;
                    kit::row(("commit", i), active)
                        .h(px(ROW))
                        .on_click(cx.listener(move |app, _, w, cx| {
                            select(app, Selection::Commit(oid.clone()), w, cx)
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                select(app, Selection::Commit(menu_oid.clone()), w, cx);
                                let target = crate::commands::Target::Commit(menu_oid.clone());
                                app.dispatch(
                                    Action::Menu(Some(Menu::Object { target, at: event.position })),
                                    w,
                                    cx,
                                );
                            }),
                        )
                        .child(ui.graph.element(i, checked_out, active, ui.stash.at_commit(i)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pr(px(14.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .when(
                                    crate::refs::show_labels(&ui.refs) && !row.refs.is_empty(),
                                    |d| d.child(ref_chips(ui, &row.oid, &row.refs, cx)),
                                )
                                .child(
                                    kit::fit(row.subject.clone())
                                        .color(if merge { TEXT_2 } else { TEXT })
                                        .grow(),
                                ),
                        )
                        .child(history_metadata(&row.author, &row.date, row.time, &row.oid, moment, wide, active))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.history_scroll.clone())
    .flex_1()
    .min_h_0();

    let (unstaged, staged) = ui.file_ranges();
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(headings)
        .child({
            let conflicted = ui.operation.active();
            let (title, summary) = crate::operation::row_text(ui).unwrap_or_else(|| {
                if ui.changed_count() == 0 {
                    ("Working tree".to_owned(), "Working tree clean".to_owned())
                } else {
                    ("Uncommitted changes".to_owned(), format!("{} unstaged · {} staged", unstaged.len(), staged.len()))
                }
            });
            kit::row("working-directory-row", ui.working())
                    .h(px(ROW))
                    .child(
                        div()
                            .w(graph_width)
                            .flex_shrink_0()
                            .pl(px(graph::LEFT - 6.5))
                            .child(
                                div()
                                    .size(px(9.))
                                    .rounded_full()
                                    .border_1()
                                    .border_dashed()
                                    .border_color(rgb(if conflicted { CONFLICT } else { ACCENT })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(if conflicted {
                                kit::any(kit::fit(title).color(CONFLICT).weight(FontWeight::MEDIUM))
                            } else {
                                kit::any(div().flex_shrink_0().child(title))
                            })
                            .child(kit::fit(summary).size(TEXT_SMALL).color(TEXT_3).grow()),
                    )
                    .on_click(cx.listener(|app, _, w, cx| select(app, Selection::Changes, w, cx)))
        })
        .when(!unborn(ui), |d| d.child(list))
        .when(unborn(ui), |d| {
            d.child(
                div()
                    .id("no-commits")
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(div().text_size(px(TEXT_TITLE)).font_weight(FontWeight::SEMIBOLD).child(NO_COMMITS.0))
                    .child(div().text_color(rgb(TEXT_2)).child(NO_COMMITS.1)),
            )
        })
}

pub(crate) const NO_COMMITS: (&str, &str) = ("No commits yet.", "Stage your files and make the first commit.");

pub(crate) fn unborn(ui: &Ui) -> bool {
    !ui.repo.is_empty() && no_commits(&ui.branch, ui.head.as_deref(), ui.history.len())
}

fn no_commits(branch: &str, head: Option<&str>, history: usize) -> bool {
    !branch.is_empty() && head.is_none() && history == 0
}


fn changes<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let (unstaged, staged) = ui.file_ranges();
    let listed = ui.files.len();
    let staged_count = staged.len();
    let amending = ui.amend.is_some();
    let loading = ui.amend.as_ref().is_some_and(|amend| amend.original.is_none());
    let amend_busy = amending && ui.busy();
    let ready = ui.can_commit(cx);
    let files = uniform_list(
        "worktree",
        listed + 2,
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let editable = ui.can_edit_index();
            let pick = crate::picked::files(ui);
            range
                .map(|i| {
                    if i == 0 || i == unstaged.len() + 1 {
                        let (name, count, id, label, action, command) = if i == 0 {
                            (
                                "Unstaged",
                                unstaged.len(),
                                "stage-all",
                                "Stage all",
                                Action::StageAll,
                                "repo.stage-all",
                            )
                        } else {
                            (
                                "Staged",
                                staged.len(),
                                "unstage-all",
                                "Unstage all",
                                Action::UnstageAll,
                                "repo.unstage-all",
                            )
                        };
                        return div()
                            .h(px(FILE_ROW))
                            .w_full()
                            .pl_3()
                            .pr_2()
                            .flex()
                            .gap(px(6.))
                            .items_center()
                            .child(kit::label(name))
                            .child(kit::label(count.to_string()).font_weight(FontWeight::MEDIUM))
                            .child(kit::grow())
                            .when(count > 0, |row| {
                                row.child(
                                    kit::btn(id, label)
                                        .small()
                                        .ghost()
                                        .enabled(editable)
                                        .tip(crate::commands::tip_with_keys(label, command))
                                        .on(action, cx),
                                )
                            })
                            .into_any_element();
                    }
                    let index = if i < unstaged.len() + 1 {
                        unstaged.start + i - 1
                    } else {
                        staged.start + i - unstaged.len() - 2
                    };
                    let f = &ui.files[index];
                    let choice = file_selection(f);
                    let side = if f.staged { crate::commands::Side::Staged } else { crate::commands::Side::Unstaged };
                    let group = pick
                        .as_ref()
                        .filter(|(paths, picked)| *picked == side && paths.contains(&f.path))
                        .map(|(paths, _)| paths.clone());
                    let target = match &group {
                        Some(paths) => crate::commands::Target::Files { paths: paths.clone(), side },
                        None => crate::commands::Target::File { path: f.path.clone(), side },
                    };
                    let selected = ui.selection.as_ref() == Some(&choice) || group.is_some();
                    let (discard_action, stage_action, unstage_action, discard_tip, stage_tip, unstage_tip) = match &group {
                        Some(paths) => (
                            Action::Picked(crate::picked::Action::AskDiscard(paths.clone())),
                            Action::Picked(crate::picked::Action::Stage(paths.clone())),
                            Action::Picked(crate::picked::Action::Unstage(paths.clone())),
                            format!("Discard {} files…", paths.len()),
                            crate::picked::file_tip("Stage", paths.len(), "S"),
                            crate::picked::file_tip("Unstage", paths.len(), "U"),
                        ),
                        None => (
                            Action::AskDiscard(Discard::File(f.path.clone())),
                            Action::StagePath(f.path.clone()),
                            Action::UnstagePath(f.path.clone()),
                            "Discard changes…".to_owned(),
                            "Stage file (S)".to_owned(),
                            "Unstage file (U)".to_owned(),
                        ),
                    };
                    let name = f
                        .path
                        .file_name()
                        .unwrap_or(f.path.as_os_str())
                        .to_string_lossy()
                        .into_owned();
                    let folder = f
                        .path
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let actions = div()
                        .flex()
                        .flex_shrink_0()
                        .gap(px(2.))
                        .when(!selected, |d| {
                            d.invisible().group_hover("file-row", |s| s.visible())
                        })
                        .when(editable && !f.staged, |d| {
                            d.child(kit::icon_btn(("discard", index), "undo", discard_tip, discard_action, cx))
                                .child(kit::icon_btn(("stage", index), "plus", stage_tip, stage_action, cx))
                        })
                        .when(editable && f.staged, |d| {
                            d.child(kit::icon_btn(("unstage", index), "minus", unstage_tip, unstage_action, cx))
                        });
                    let (path, staged) = (f.path.clone(), f.staged);
                    let menu_choice = choice.clone();
                    let picked_row = group.is_some();
                    kit::row(("file", index), selected)
                        .group("file-row")
                        .h(px(FILE_ROW))
                        .pl(px(10.))
                        .pr_2()
                        .gap_2()
                        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                            let modifiers = event.modifiers();
                            let how = if modifiers.platform {
                                Some(crate::picked::How::Toggle)
                            } else if modifiers.shift {
                                Some(crate::picked::How::Range)
                            } else {
                                None
                            };
                            match how {
                                Some(how) => {
                                    focus(app.ui(), w);
                                    let path = path.clone();
                                    app.dispatch(Action::Picked(crate::picked::Action::Click { path, staged, how }), w, cx)
                                }
                                None => select(app, choice.clone(), w, cx),
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                if !picked_row && app.ui().selection.as_ref() != Some(&menu_choice) {
                                    select(app, menu_choice.clone(), w, cx);
                                }
                                let target = target.clone();
                                app.dispatch(
                                    Action::Menu(Some(Menu::Object { target, at: event.position })),
                                    w,
                                    cx,
                                )
                            }),
                        )
                        .child(kit::badge(f.status))
                        .child(kit::fit(name).size(12.5).cut(kit::Cut::Middle))
                        .child(
                            kit::fit(folder)
                                .size(11.5)
                                .color(TEXT_3)
                                .cut(kit::Cut::Start)
                                .grow(),
                        )
                        .child(actions)
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.file_scroll.clone())
    .flex_1()
    .min_h_0();

    let reason = ui.identity.reason().or_else(|| {
        if loading {
            Some("Loading the last commit message")
        } else if ui.amend.as_ref().is_some_and(|amend| ui.amend_target_changed(amend)) {
            Some("The checkout changed. Turn Amend off and back on to use the new tip.")
        } else if !amending && staged_count == 0 {
            Some("Stage a change to commit it")
        } else if ui.input.read(cx).text().trim().is_empty() {
            Some("Write a summary to commit")
        } else {
            None
        }
        .map(str::to_owned)
    });
    let label = if amending {
        match staged_count {
            0 => "Amend last commit".to_owned(),
            1 => "Amend with 1 staged file".to_owned(),
            n => format!("Amend with {n} staged files"),
        }
    } else {
        match (staged_count, ui.checked_out_branch()) {
            (0, _) => "Commit".to_owned(),
            (1, Some(branch)) => format!("Commit 1 file to {branch}"),
            (n, Some(branch)) => format!("Commit {n} files to {branch}"),
            (1, None) => "Commit 1 file".to_owned(),
            (n, None) => format!("Commit {n} files"),
        }
    };
    let mut commit = kit::btn("commit-index", label)
        .primary()
        .wide()
        .enabled(ready)
        .hint(if cfg!(target_os = "macos") {
            "⌘↵"
        } else {
            "Ctrl ↵"
        });
    if let Some(reason) = reason {
        commit = commit.tip(reason);
    }
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .pt_1()
        .when_some(detached_warning(ui), |d, warning| d.child(detached_note(app, warning, cx)))
        .child(files)
        .child(
            div()
                .p_3()
                .flex_shrink_0()
                .border_t_1()
                .border_color(rgb(LINE))
                .flex()
                .flex_col()
                .gap_2()
                .when(loading || amend_busy, |d| {
                    d.child(div().h(px(106.)).child(kit::muted(if loading {
                        "Loading commit message…"
                    } else {
                        "Working…"
                    })))
                })
                .when(!loading && !amend_busy, |d| d.child(ui.input.clone()).child(ui.description.clone()))
                .child(
                    div()
                        .id("amend-last-commit")
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(TEXT_2))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(kit::clear())
                        .child(kit::checkbox(amending))
                        .child("Amend last commit")
                        .tooltip(kit::tip("Replace the last commit with this message and any staged changes."))
                        .when(ui.can_edit_index() && ui.head.is_some(), |d| {
                            let d = d.cursor_pointer().focusable().focus(|s| s.border_color(rgb(ACCENT)));
                            kit::pressable(d, Action::ToggleAmend, cx)
                        })
                        .when(!ui.can_edit_index() || ui.head.is_none(), |d| d.opacity(0.42)),
                )
                .when_some(amend_warning(ui), |d, warning| {
                    d.child(
                        div()
                            .id("amend-pushed")
                            .text_size(px(TEXT_SMALL))
                            .text_color(rgb(MODIFIED))
                            .child(warning),
                    )
                })
                .children(crate::identity::form(ui, cx))
                .child(commit.on(Action::Commit, cx))
                .children(crate::identity::committing_as(ui, cx)),
        )
}

pub(crate) fn amend_warning(ui: &Ui) -> Option<String> {
    if ui.amend.is_none() || ui.detached {
        return None;
    }
    let item = ui.refs.current(&ui.branch)?;
    let upstream = item.upstream.as_deref().filter(|_| !item.gone && item.ahead == 0)?;
    Some(format!("This commit is already on {upstream}. Amending it means a force push."))
}

pub(crate) const DETACHED_WARNING: &str = "You are not on a branch. A commit made here is easy to lose.";

pub(crate) fn detached_warning(ui: &Ui) -> Option<&'static str> {
    (ui.detached && ui.head.is_some()).then_some(DETACHED_WARNING)
}

fn detached_note<T: Desk>(app: &T, warning: &'static str, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let command = crate::commands::find("repo.new-branch");
    let entry = command.and_then(|command| crate::commands::entry(command, ui, &crate::commands::Target::Repo, cx));
    let (action, reason) = entry.map_or((None, None), |entry| (entry.action, entry.disabled));
    let tip = reason.unwrap_or_else(|| {
        crate::commands::tip_with_keys("Start a branch at this commit and switch to it", "repo.new-branch")
    });
    let enabled = action.is_some();
    let button = kit::btn("detached-branch", "Create branch here…")
        .small()
        .enabled(enabled)
        .tip(tip)
        .on(action.unwrap_or(Action::Refresh), cx);
    div()
        .mx_3()
        .mt_1()
        .mb_2()
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE_STRONG))
        .bg(rgb(RAISED))
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(kit::icon("alert").size(px(14.)).mt(px(1.)).flex_shrink_0().text_color(rgb(MODIFIED)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(TEXT_2))
                        .child(warning),
                ),
        )
        .child(div().pl(px(22.)).flex().child(button))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Centre {
    Activity,
    Resolver,
    Chooser,
    Diff,
    Search,
    History,
}

pub(crate) fn centre(ui: &Ui) -> Centre {
    let operating = ui.operation.active();
    let browsing = !crate::operation::owns_panel(ui);
    if ui.activity.open() {
        Centre::Activity
    } else if ui.diff_open && browsing {
        Centre::Diff
    } else if operating && ui.merge.editing().is_some() {
        Centre::Resolver
    } else if operating && ui.conflict.showing().is_some() {
        Centre::Chooser
    } else if ui.search.visible() {
        Centre::Search
    } else {
        Centre::History
    }
}

fn panel<T: Desk>(app: &T, width: Pixels, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let shell = div()
        .w(width)
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(PANEL))
        .children(crate::operation::reminder_strip(app, cx));
    match &ui.selection {
        Some(Selection::Changes | Selection::File { .. }) => shell.child(changes(app, cx)),
        Some(Selection::Commit(_)) => shell.child(crate::inspection::panel(app, cx)),
        Some(Selection::Stash(_)) => shell.child(crate::stash::details(app, cx)),
        None => shell.child(
            div()
                .p_4()
                .child(kit::muted("Select a commit to see its details.")),
        ),
    }
}


fn context_menu<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement> {
    use crate::commands::{self, Target};
    let ui = app.ui();
    let open = ui.menu.as_ref()?;
    let (at, width) = match open {
        Menu::Remotes { at } => (*at, 230.),
        Menu::Object { target, at } => (
            *at,
            match target {
                Target::Commit(_) => 270.,
                Target::Remote(_) => 186.,
                Target::Stash(_) => 200.,
                _ => 260.,
            },
        ),
        Menu::Labels { at, .. } => (*at, 220.),
        Menu::Repo | Menu::CommitActions => return None,
    };
    let surface = commands::surface(commands::entries_of(ui, open, cx), width, ui.menu_cursor, cx);
    Some(
        deferred(
            anchored()
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(surface),
        )
        .with_priority(1),
    )
}


struct DividerDrag;

impl Render for DividerDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub(crate) fn resize_column<T: Desk>(
    app: &mut T,
    column: Column,
    x: Pixels,
    window: &Window,
    cx: &mut Context<T>,
) {
    let viewport = f32::from(window.viewport_size().width);
    let before = app.ui().columns.fit(viewport);
    let after = before.dragged(column, viewport, f32::from(x));
    if before != after {
        app.ui_mut().columns = after;
        crate::settings::Memory::update(cx, |memory| memory.columns = Some(after));
        cx.notify();
    }
}

fn divider<T: Desk>(column: Column, cx: &Context<T>) -> impl IntoElement + use<T> {
    let tab = cx.weak_entity();
    div()
        .id(match column {
            Column::Sidebar => "sidebar-divider",
            Column::Panel => "details-divider",
        })
        .w(px(DIVIDER_WIDTH))
        .h_full()
        .flex_shrink_0()
        .flex()
        .justify_center()
        .bg(rgb(PANEL))
        .cursor(CursorStyle::ResizeLeftRight)
        .hover(|style| style.bg(rgb(LINE_STRONG)))
        .child(div().w(px(1.)).h_full().bg(rgb(LINE)))
        .on_drag(column, move |column, _, window, cx| {
            let _ = tab.update(cx, |app, cx| {
                resize_column(app, *column, window.mouse_position().x, window, cx);
            });
            cx.new(|_| DividerDrag)
        })
}

pub fn render<T: Desk>(
    app: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    if app.ui().focus_on_open {
        focus(app.ui(), window);
        app.ui_mut().focus_on_open = false;
    }
    crate::prompt::take_focus(app.ui_mut(), window, cx);
    if crate::start::wants_focus(app.ui(), cx) {
        crate::start::take_focus(app.ui_mut(), window, cx);
    } else if !crate::start::filter_shown(app.ui(), cx) {
        crate::start::rearm(app.ui_mut());
    }
    if app.ui().focus_lost.is_none() {
        let watch = cx.on_focus_lost(window, |app, window, cx| {
            if crate::tabs::in_front(cx.entity_id(), cx) {
                focus(app.ui(), window)
            }
        });
        app.ui_mut().focus_lost = Some(watch);
    }
    {
        let modal = app.ui().modal_open() && !app.ui().prompt.open();
        let ui = app.ui();
        if modal && ui.modal_drawn && !ui.modal_focus.contains_focused(window, cx) {
            window.focus(&ui.modal_focus);
        }
        if !modal && ui.modal_drawn && ui.modal_focus.contains_focused(window, cx) {
            focus(ui, window);
        }
        app.ui_mut().modal_drawn = modal;
    }
    crate::refs::rebuild_rows(app.ui_mut(), cx);
    crate::operation::prefill(app.ui_mut(), cx);
    sync_chrome(app, window, cx);
    {
        let ui = app.ui();
        let summary = ui.working() && !ui.panel_hidden && !ui.operation.active();
        let message = !ui.panel_hidden && crate::operation::shows_message(ui);
        let gone = (!message && ui.operation_message.focus_handle(cx).is_focused(window))
            || (!summary
            && (ui.input.focus_handle(cx).is_focused(window)
                || ui.description.focus_handle(cx).is_focused(window)))
            || (!ui.refs.list_open && ui.refs.find.focus_handle(cx).is_focused(window))
            || (ui.refs.draft.is_none()
                && (ui.refs.input.focus_handle(cx).is_focused(window)
                    || ui.refs.message.focus_handle(cx).is_focused(window)))
            || (!ui.stash.options_open()
                && ui.stash.message().is_some_and(|field| field.focus_handle(cx).is_focused(window)))
            || (!ui.cloning.open()
                && (ui.cloning.address.focus_handle(cx).is_focused(window)
                    || ui.cloning.name.focus_handle(cx).is_focused(window)))
            || (!ui.init.open()
                && (ui.init.name.focus_handle(cx).is_focused(window)
                    || ui.init.branch.focus_handle(cx).is_focused(window)))
            || (!crate::start::filter_shown(ui, cx) && ui.start.filter.focus_handle(cx).is_focused(window))
            || crate::preferences::field_focused(ui, window, cx)
            || crate::identity::panel_field_focused(ui, window, cx);
        if gone {
            focus(ui, window);
        }
    }
    let ui = app.ui();
    let open = !ui.repo.is_empty();
    let root = div()
        .id("desk")
        .track_focus(&ui.focus)
        .key_context("Desk")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(rgb(CANVAS))
        .text_color(rgb(TEXT))
        .text_size(px(TEXT_BASE))
        .on_drag_move(cx.listener(|app, event: &DragMoveEvent<Column>, w, cx| {
            let column = *event.drag(cx);
            resize_column(app, column, event.event.position.x, w, cx);
        }))
        .on_action(cx.listener(|app, run: &crate::commands::Run, w, cx| {
            app.dispatch(Action::Run(run.id), w, cx)
        }))
        .on_action(cx.listener(|app, run: &crate::commands::Choose, w, cx| {
            app.dispatch(Action::Run(run.id), w, cx)
        }))
        .on_action(cx.listener(|app, _: &commit_input::SelectSummary, w, cx| {
            app.dispatch(Action::Run("file.pick-all"), w, cx)
        }))
        .on_action(cx.listener(|app, open: &crate::start::OpenRecent, w, cx| {
            if app.ui().dialog_open() {
                app.ui_mut().notices.show(crate::notice::Notice::new(Tone::Info, "Close the open dialog first", ""));
                cx.notify();
            } else {
                app.dispatch(Action::Start(crate::start::Action::Open(open.path.clone())), w, cx);
            }
        }))
        .on_action(|_: &Minimize, w, _| w.minimize_window())
        .on_action(|_: &Zoom, w, _| w.zoom_window())
        .on_action(|_: &FullScreen, w, _| w.toggle_fullscreen())
        .on_action(cx.listener(|app, _: &ChooseSearchResult, w, cx| {
            app.dispatch(Action::Search(crate::search::Action::Pick(None)), w, cx);
        }))
        .on_action(cx.listener(|app, _: &NextFocus, w, cx| {
            if app.ui().prompt.open() {
                app.dispatch(Action::Prompt(crate::prompt::Action::Next), w, cx);
            } else if app.ui().modal_open() {
                trap(app.ui(), true, w, cx);
            } else {
                w.focus_next();
            }
        }))
        .on_action(cx.listener(|app, _: &PreviousFocus, w, cx| {
            if app.ui().prompt.open() {
                app.dispatch(Action::Prompt(crate::prompt::Action::Previous), w, cx);
            } else if app.ui().modal_open() {
                trap(app.ui(), false, w, cx);
            } else {
                w.focus_prev();
            }
        }))
        .capture_any_mouse_down(cx.listener(|app, _, _, _| press_started(app.ui_mut())))
        .capture_key_down(cx.listener(|app, _, _, _| app.ui_mut().closed_by_press = None))
        .on_action(cx.listener(|app, _: &MoveUp, w, cx| navigate(app, Move::Step(-1), w, cx)))
        .on_action(cx.listener(|app, _: &MoveDown, w, cx| navigate(app, Move::Step(1), w, cx)))
        .on_action(cx.listener(|app, _: &MoveLeft, w, cx| navigate(app, Move::Left, w, cx)))
        .on_action(cx.listener(|app, _: &MoveRight, w, cx| navigate(app, Move::Right, w, cx)))
        .on_action(cx.listener(|app, _: &MovePageUp, w, cx| navigate(app, Move::Page(-1), w, cx)))
        .on_action(cx.listener(|app, _: &MovePageDown, w, cx| navigate(app, Move::Page(1), w, cx)))
        .on_action(cx.listener(|app, _: &MoveHome, w, cx| navigate(app, Move::Home, w, cx)))
        .on_action(cx.listener(|app, _: &MoveEnd, w, cx| navigate(app, Move::End, w, cx)))
        .on_action(cx.listener(|app, _: &MoveFirst, w, cx| navigate(app, Move::First, w, cx)))
        .on_action(cx.listener(|app, _: &MoveLast, w, cx| navigate(app, Move::Last, w, cx)))
        .on_action(cx.listener(|app, _: &FollowParent, w, cx| navigate(app, Move::Parent, w, cx)))
        .on_action(cx.listener(|app, _: &FollowChild, w, cx| navigate(app, Move::Child, w, cx)))
        .on_action(cx.listener(|app, _: &ChooseRow, w, cx| navigate(app, Move::Choose, w, cx)))
        .on_action(cx.listener(|app, show: &crate::tabs::ShowTab, w, cx| {
            app.dispatch(Action::Tabs(crate::tabs::Action::Show(show.index)), w, cx)
        }))
        .on_action(cx.listener(|app, _: &FocusDesk, w, cx| {
            let ui = app.ui();
            if ui.prompt.open() {
                app.dispatch(Action::Prompt(crate::prompt::Action::Cancel), w, cx);
            } else if ui.palette.open() {
                app.dispatch(Action::Palette(crate::palette::Action::Close), w, cx);
            } else if ui.help.open() {
                app.dispatch(Action::Help(crate::help::Action::Close), w, cx);
            } else if ui.cloning.open() {
                app.dispatch(Action::Clone(crate::clone::Action::Close), w, cx);
            } else if ui.init.open() {
                app.dispatch(Action::Init(crate::init::Action::Close), w, cx);
            } else if ui.publish.open() {
                app.dispatch(Action::Publish(crate::publish::Action::Escape), w, cx);
            } else if ui.menu.is_some() && ui.menu_cursor.open {
                app.dispatch(Action::MenuKey(crate::commands::Key::Left), w, cx);
            } else if ui.menu.is_some() {
                app.dispatch(Action::Dismiss, w, cx);
            } else if ui.sync.options_open() && ui.refs.draft.is_none() {
                app.dispatch(Action::Sync(crate::sync::Action::Dismiss), w, cx);
            } else if ui.stash.options_open() && ui.refs.draft.is_none() {
                app.dispatch(Action::Stash(crate::stash::Action::Dismiss), w, cx);
            } else if ui.refs.draft.is_some() || ui.refs.list_open {
                app.dispatch(Action::Refs(crate::refs::Action::Escape), w, cx);
            } else if ui.preferences.open() {
                app.dispatch(Action::Preferences(crate::preferences::Action::Close), w, cx);
            } else if ui.activity.open() {
                app.dispatch(Action::Activity(crate::activity::Action::Close), w, cx);
            } else if centre(ui) == Centre::Diff {
                app.dispatch(Action::CloseDiff, w, cx);
            } else if ui.merge.editing().is_some() {
                app.dispatch(Action::Merge(crate::merge::Action::Close), w, cx);
            } else if centre(ui) == Centre::Chooser {
                app.dispatch(Action::Conflict(crate::conflict::Action::Close), w, cx);
            } else if ui.search.open() {
                app.dispatch(Action::Search(crate::search::Action::Close), w, cx);
            } else if crate::start::clear_filter(ui, cx) {
            } else if !ui.refs.filter.read(cx).text().is_empty() {
                ui.refs.filter.clone().update(cx, |filter, cx| filter.clear(cx));
                focus(app.ui(), w);
            } else if ui.input.focus_handle(cx).is_focused(w)
                || ui.description.focus_handle(cx).is_focused(w)
                || ui.refs.filter.focus_handle(cx).is_focused(w)
            {
                focus(ui, w);
            } else if let Some(id) = ui.notices.dismissable() {
                app.dispatch(Action::Notice(crate::notice::Action::Dismiss(id)), w, cx);
            } else {
                app.dispatch(Action::Refs(crate::refs::Action::Escape), w, cx);
            }
        }))
        .child(toolbar(app, cx))
        .children(crate::tabs::render(app, cx));

    let body = if ui.preferences.open() {
        crate::preferences::page(app, cx)
    } else if let Some(path) = ui.opening.clone().filter(|_| !open) {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kit::spinner("opening-spinner"))
                    .child(
                        div()
                            .text_size(px(TEXT_TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Opening repository…"),
                    ),
            )
            .child(kit::muted(path))
    } else if !open {
        crate::start::screen(app, cx)
    } else {
        let viewport = f32::from(window.viewport_size().width);
        let widths = ui.columns.fit(viewport);
        let shown = ui.shown();
        let sidebar = div()
            .w(px(widths.sidebar))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(PANEL))
            .child(div().p_2().child(ui.refs.filter.clone()))
            .child(crate::refs::sidebar(app, cx));
        let centre = match centre(ui) {
            Centre::Activity => crate::activity::render(app, cx),
            Centre::Resolver => crate::merge::resolver(app, cx),
            Centre::Chooser => crate::conflict::chooser(app, cx),
            Centre::Diff => diff_view::render(app, window, cx),
            Centre::Search => crate::search::results(app, cx),
            Centre::History => history(app, ui.columns_drawn(viewport).1, cx),
        };
        let columns = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .child(centre)
            .when(shown.panel, |d| {
                d.child(divider(Column::Panel, cx)).child(if crate::operation::owns_panel(ui) {
                    crate::operation::side_panel(app, px(widths.panel), cx)
                } else {
                    panel(app, px(widths.panel), cx)
                })
            });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .when(shown.sidebar, |d| d.child(sidebar).child(divider(Column::Sidebar, cx)))
            .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .when(ui.operation.kind().is_some(), |d| d.child(crate::operation::banner(app, cx)))
                .child(columns),
        )
    };

    let backdrop = ui.transient_open().then(|| {
        deferred(
            div()
                .id("backdrop")
                .absolute()
                .inset_0()
                .on_any_mouse_down(cx.listener(|app, _, w, cx| pressed_outside(app, w, cx)))
                .on_scroll_wheel(cx.listener(|app, _, w, cx| {
                    app.dispatch(Action::Dismiss, w, cx);
                })),
        )
    });
    root.child(body)
        .children(backdrop)
        .children(context_menu(app, cx))
        .children(crate::refs::dialog(app, cx))
        .children(crate::clone::dialog(app, cx))
        .children(crate::init::dialog(app, cx))
        .children(crate::publish::dialog(app, cx))
        .children(crate::help::render(app, cx))
        .children(crate::palette::render(app, cx))
        .children(crate::prompt::render(app, window.capslock().on, cx))
        .children(crate::notice::stack(app, cx))
}

pub(crate) fn copy(text: String, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Target;

    #[test]
    fn a_hidden_column_gives_its_width_and_divider_to_the_centre() {
        let widths = ColumnWidths::new(200., 300.);
        let both = Shown { sidebar: true, panel: true };
        assert_eq!(widths.layout(1180., both), (200., 670., 300.));
        let (_, centre, panel) = widths.layout(1180., Shown { sidebar: false, ..both });
        assert_eq!((centre, panel), (670. + 200. + DIVIDER_WIDTH, 300.));
        let (sidebar, centre, _) = widths.layout(1180., Shown { panel: false, ..both });
        assert_eq!((sidebar, centre), (200., 670. + 300. + DIVIDER_WIDTH));
        assert_eq!(widths.layout(1180., Shown { sidebar: false, panel: false }), (0., 1180., 0.));
    }

    #[test]
    fn graph_labels_stand_for_their_refs_in_display_order() {
        let remotes = ["origin".to_owned(), "up".to_owned()];
        let found = labels(
            &remotes,
            "tag: v1, origin/main, HEAD -> main, origin/HEAD, feature/map, up/feature/map, origin-old/x",
        );
        let shown: Vec<_> = found.iter().map(|label| (label.name.as_str(), label.target.clone())).collect();
        assert_eq!(
            shown,
            [
                ("main", Some(Target::Branch("main".into()))),
                ("feature/map", Some(Target::Branch("feature/map".into()))),
                ("origin-old/x", Some(Target::Branch("origin-old/x".into()))),
                ("origin/main", Some(Target::RemoteBranch("origin/main".into()))),
                ("up/feature/map", Some(Target::RemoteBranch("up/feature/map".into()))),
                ("v1", Some(Target::Tag("v1".into()))),
            ]
        );
        assert!(found[0].chip == kit::Chip::Head);
        assert!(found[3].chip == kit::Chip::Remote);
        let detached = labels(&remotes, "HEAD, tag: v2");
        assert!(detached[0].chip == kit::Chip::Head && detached[0].target.is_none());
        assert_eq!(detached[1].target, Some(Target::Tag("v2".into())));
        assert!(labels(&remotes, "").is_empty());
    }

    #[test]
    fn a_label_click_selects_its_commit_and_a_double_click_runs_its_default() {
        let oid = "a".repeat(40);
        let branch = Target::Branch("feature/map".into());
        assert!(matches!(label_click(&oid, &branch, 1), Action::Select(Selection::Commit(id)) if id == oid));
        assert!(matches!(label_click(&oid, &branch, 2), Action::Activate(target) if target == branch));
        let remote = Target::RemoteBranch("origin/main".into());
        assert!(matches!(
            label_menu(remote.clone(), Point::default()),
            Action::Menu(Some(Menu::Object { target, .. })) if target == remote
        ));
    }

    #[test]
    fn only_a_repository_with_no_commit_on_any_ref_has_no_commits() {
        use crate::repository::tests::Fixture;
        let f = Fixture::new();
        let status = f.repo.status().unwrap();
        let history = f.repo.history(0, 10).unwrap();
        assert_eq!((status.branch.as_str(), status.head.as_deref(), history.len()), ("main", None, 0));
        assert!(no_commits(&status.branch, status.head.as_deref(), history.len()));
        assert!(!no_commits("", None, 0));
        std::fs::write(f.dir.join("trail.txt"), "Trail\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start the trail"]);
        f.git(&["switch", "-q", "--orphan", "fresh"]);
        let status = f.repo.status().unwrap();
        let history = f.repo.history(0, 10).unwrap();
        assert_eq!((status.branch.as_str(), status.head.as_deref(), history.len()), ("fresh", None, 1));
        assert!(!no_commits(&status.branch, status.head.as_deref(), history.len()));
        f.git(&["switch", "-q", "main"]);
        let status = f.repo.status().unwrap();
        assert!(status.head.is_some() && !no_commits(&status.branch, status.head.as_deref(), 1));
    }

    #[test]
    fn dates_are_relative_today_then_short_then_dated() {
        let now = 1_795_000_000;
        assert_eq!(when(now - 20, "2026-11-18", now), "now");
        assert_eq!(when(now - 300, "2026-11-18", now), "5 min ago");
        assert_eq!(when(now - 3 * 3_600, "2026-11-18", now), "3 hours ago");
        assert_eq!(when(now - 30 * 3_600, "2026-11-17", now), "yesterday");
        assert_eq!(when(now - 40 * 86_400, "2026-10-09", now), "9 Oct");
        assert_eq!(when(now - 400 * 86_400, "2025-10-14", now), "14 Oct 2025");
        assert_eq!(year_of(now), 2026);
        assert_eq!(date_of(now), "2026-11-18");
        assert_eq!(date_of(1_767_225_599), "2025-12-31");
        assert_eq!(date_of(1_767_225_600), "2026-01-01");
        assert_eq!(date_of(951_782_400), "2000-02-29");
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(when(now - 40 * 86_400, &date_of(now - 40 * 86_400), now), "9 Oct");
        assert_eq!(year_of(1_767_225_599), 2025);
        assert_eq!(year_of(1_767_225_600), 2026);
    }
}
