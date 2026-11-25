mod backend;
use crate::{
    Tab, kit,
    theme::*,
    view::{self, Desk},
};
pub(crate) use backend::{Choice, LIMIT, Merged, Side, marker, marker_width};
use backend::{Choices, Document, LinePair, Preview, ResultRow};
use crate::operation::{Kind, Snapshot};
use gpui::{
    Context, Div, FontWeight, ScrollHandle, ScrollStrategy, UniformListScrollHandle, Window, div, prelude::*,
    px, rgb, uniform_list,
};
use std::{
    ffi::OsString,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
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
    Save,
    Close,
}

struct Editor {
    document: Arc<Document>,
    choices: Choices,
    hunk: usize,
    preview: Preview,
    unresolved: usize,
    width: f32,
}

fn display_line(line: &str) -> String {
    let line = line.strip_suffix('\n').map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));
    if line.len() <= 2048 {
        line.to_owned()
    } else {
        format!("{}…", &line[..line.floor_char_boundary(2048)])
    }
}

impl Editor {
    fn new(document: Document) -> Self {
        let choices = document.new_choices();
        let preview = document.preview(&choices);
        let unresolved = document.unresolved(&choices);
        let width = document.source.lines().map(|line| line.len().min(2048)).max().unwrap_or(0) as f32 * 7.5;
        Self {
            document: Arc::new(document),
            choices,
            hunk: 0,
            preview,
            unresolved,
            width,
        }
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
    }
}

pub(crate) struct State {
    pub(crate) mode: Mode,
    editor: Option<Editor>,
    document_ticket: u64,
    source_scroll: UniformListScrollHandle,
    result_scroll: UniformListScrollHandle,
    source_pan: ScrollHandle,
    result_pan: ScrollHandle,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            editor: None,
            document_ticket: 0,
            source_scroll: UniformListScrollHandle::new(),
            result_scroll: UniformListScrollHandle::new(),
            source_pan: ScrollHandle::new(),
            result_pan: ScrollHandle::new(),
        }
    }
}

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

    fn show_hunk(&mut self, index: usize) {
        if let Some(editor) = self.editor.as_mut()
            && let Some(&start) = editor.preview.hunk_starts.get(index)
        {
            editor.hunk = index;
            self.source_scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
            self.result_scroll.scroll_to_item_strict(start, ScrollStrategy::Top);
        }
    }

    pub(crate) fn follow(&mut self, changed: bool, snapshot: Option<&Snapshot>) {
        if changed
            || self.editing().is_some_and(|path| !snapshot.is_some_and(|s| s.has_conflict(path)))
        {
            self.editor = None;
            self.document_ticket += 1;
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
    if let Some(reason) = tab.ui.operation.refusal() {
        tab.ui.notices.show(crate::notice::Notice::new(crate::view::Tone::Info, reason, ""));
        cx.notify();
        return;
    }
    let current = tab.ui.branch.clone();
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
            let mut said = outcome(&merged, &current, &label);
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
    if tab.ui.busy() {
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
                state.editor = None;
                state.document_ticket += 1;
                crate::conflict::show(&mut tab.ui, path, None);
                cx.notify();
                return;
            }
            state.document_ticket += 1;
            let ticket = state.document_ticket;
            let epoch = tab.epoch;
            state.editor = None;
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
                            state.editor = Some(editor);
                            state.show_hunk(0);
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
        Action::Close => {
            state.editor = None;
            state.document_ticket += 1;
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
                state.source_scroll.scroll_to_item(line, ScrollStrategy::Center);
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
                editor.choose_line(hunk, line, next);
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
                |tab, _| {
                    tab.ui.merge.editor = None;
                    tab.ui.merge.document_ticket += 1;
                },
            );
        }
    }
    cx.notify();
}

fn button<T: Desk>(id: impl Into<gpui::ElementId>, label: &str) -> kit::Btn {
    let _ = std::marker::PhantomData::<T>;
    kit::btn(id, label.to_owned())
}

fn side_color(side: Side) -> u32 {
    match side {
        Side::Current => ACCENT,
        Side::Incoming => REMOTE,
    }
}

fn line_number(number: Option<usize>) -> Div {
    div()
        .w(px(36.))
        .flex_shrink_0()
        .pr_2()
        .flex()
        .justify_end()
        .font_family(MONO)
        .text_size(px(TEXT_TINY))
        .text_color(rgb(TEXT_OFF))
        .when_some(number, |d, n| d.child(n.to_string()))
}

fn line_text(
    id: (&'static str, usize),
    text: &str,
    width: f32,
    pan: &ScrollHandle,
) -> impl IntoElement {
    let mut text = div()
        .id(id)
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .track_scroll(pan)
        .child(
            div()
                .min_w(px(width))
                .font_family(MONO)
                .text_size(px(TEXT_SMALL))
                .whitespace_nowrap()
                .child(display_line(text)),
        );
    text.style().restrict_scroll_to_axis = Some(true);
    text
}

fn source_cell<T: Desk>(app: &T, row: usize, side: Side, cx: &Context<T>) -> Div {
    let state = &app.ui().merge;
    let editor = state.editor.as_ref().expect("source rows require an editor");
    let hunk = editor.hunk;
    let pair: &LinePair = &editor.document.hunks[hunk][row];
    let choice = editor.choices[hunk][row];
    let selected = choice.is_some_and(|choice| choice.includes(side));
    let version = pair.on(side);
    let color = side_color(side);
    let (button_id, text_id, tip) = match side {
        Side::Current => ("merge-pick-current", "merge-current-text", "Toggle current line"),
        Side::Incoming => ("merge-pick-incoming", "merge-incoming-text", "Toggle incoming line"),
    };
    let action = view::Action::Merge(Action::ToggleLine(hunk, row, side));
    let enabled = !app.ui().busy();
    div()
        .h_full()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .when(side == Side::Incoming, |d| d.border_l_1().border_color(rgb(LINE)))
        .when(!pair.shared, |d| d.bg(soft(color, if selected { 0x24 } else { 0x0d })))
        .child(line_number(version.map(|(number, _)| *number)))
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
                            .tooltip(kit::tip(if version.is_some() { tip } else { "Use this side's deletion" }))
                            .child(kit::checkbox(selected))
                            .when(enabled, |d| {
                                let d = d.cursor_pointer().focusable().focus(|s| s.border_color(rgb(ACCENT)));
                                kit::pressable(d, action, cx)
                            }),
                    )
                }),
        )
        .when_some(version, |d, (_, range)| {
            d.child(line_text((text_id, row), &editor.document.source[range.clone()], editor.width, &state.source_pan))
        })
        .when(version.is_none(), |d| d.child(kit::muted("No line")))
}

fn source_rows<T: Desk>(state: &State, editor: &Editor, cx: &mut Context<T>) -> impl IntoElement {
    let mut rows = uniform_list(
        "merge-source-lines",
        editor.document.hunks[editor.hunk].len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            range.map(|line| {
                div()
                    .h(px(DIFF_LINE))
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

fn result_rows<T: Desk>(state: &State, editor: &Editor, cx: &mut Context<T>) -> impl IntoElement {
    let mut rows = uniform_list(
        "merge-result-lines",
        editor.preview.rows.len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let state = &app.ui().merge;
            let editor = state.editor.as_ref().expect("result rows require an editor");
            range.map(|i| {
                let base = div().id(("merge-result", i))
                    .h(px(CONTROL_SMALL + 4.))
                    .w_full()
                    .flex()
                    .items_center()
                    .overflow_hidden();
                match &editor.preview.rows[i] {
                    ResultRow::Text { range, side } => base
                        .when_some(*side, |d, side| d.bg(soft(side_color(side), 0x15)))
                        .child(line_number(Some(i + 1)))
                        .child(line_text(("merge-result-text", i), &editor.document.source[range.clone()], editor.width, &state.result_pan)),
                    ResultRow::Unresolved { hunk, line } => base
                        .px_2()
                        .gap(px(4.))
                        .bg(soft(CONFLICT, 0x18))
                        .child(kit::fit(format!("Line {} unresolved", i + 1)).size(TEXT_SMALL).color(CONFLICT).grow())
                        .children([
                            ("result-current", "Use current", Choice::One(Side::Current)),
                            ("result-incoming", "Use incoming", Choice::One(Side::Incoming)),
                            ("result-both", "Use both", Choice::Both),
                        ].into_iter().map(|(id, label, choice)| {
                            button::<T>(id, label).small().enabled(!app.ui().busy())
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

pub(crate) fn resolver<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.merge;
    let (Some(snapshot), Some(editor)) = (ui.operation.snapshot(), state.editor.as_ref()) else {
        return div();
    };
    let ready = !ui.busy();
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
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(
            kit::fit(editor.document.path.to_string_lossy().into_owned())
                .size(12.5)
                .weight(FontWeight::SEMIBOLD)
                .cut(kit::Cut::Start),
        )
        .child(kit::muted(format!("Conflict {} of {total}", i + 1)).flex_shrink_0())
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
        }))
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
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(LINE))
            .when(side == Side::Incoming, |d| d.border_l_1())
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(rgb(side_color(side))))
            .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).font_weight(FontWeight::SEMIBOLD).child(name))
            .child(kit::fit(branch).size(TEXT_SMALL).color(TEXT_3).grow())
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
        .child(headings)
        .child(source_rows(state, editor, cx))
        .child(kit::scrollbar("merge-source-scrollbar", &state.source_scroll))
        .child(kit::scrollbar("merge-source-pan", &state.source_pan).horizontal().within(&state.source_scroll))
        .child(
            div()
                .h(px(30.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .bg(rgb(PANEL))
                .border_t_1()
                .border_b_1()
                .border_color(rgb(LINE))
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
                        .text_color(rgb(if open == 0 { ADDED } else { CONFLICT }))
                        .child(match open {
                            0 => "All lines resolved".to_owned(),
                            1 => "1 line unresolved".to_owned(),
                            n => format!("{n} lines unresolved"),
                        }),
                ),
        )
        .child(result_rows(state, editor, cx))
        .child(kit::scrollbar("merge-result-scrollbar", &state.result_scroll))
        .child(kit::scrollbar("merge-result-pan", &state.result_pan).horizontal().within(&state.result_scroll))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{snapshot, tests::{commit, stops}};
    use crate::repository::tests::Fixture;

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
