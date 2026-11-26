use crate::{
    Tab, kit,
    autostash::{Kept, Retry},
    commands::{Availability, Command, Group, Target, busy, operating},
    guard::Seen,
    merge::Source,
    notice::{self, Failure, Notice, Outcome},
    operation::Kind as Operation,
    prompt::{self, Answer, Field, Spec},
    repository::{Repository, checked, validate_oid},
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use gpui::{
    Context, Div, ElementId, FontWeight, IntoElement, ParentElement, Pixels, Point, Render, ScrollStrategy, SharedString,
    Stateful, Styled, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use std::{
    ffi::{OsStr, OsString},
    ops::Range,
    os::unix::ffi::OsStrExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const LIMIT: usize = 2000;
pub(crate) const OLDEST: &str = "The oldest commit has nothing before it to fold into";
const NO_RECEIVER: &str = "Each squashed commit needs a kept commit before it to fold into";
const UNCHANGED: &str = "Nothing has changed yet. Set an action or move a commit first";
const KEYED: &str = "RebaseEditor && !CommitField";
const ROW_HEIGHT: f32 = 34.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Pick,
    Reword,
    Squash,
    Drop,
}

impl Choice {
    const ALL: [Choice; 4] = [Choice::Pick, Choice::Reword, Choice::Squash, Choice::Drop];

    fn label(self) -> &'static str {
        match self {
            Choice::Pick => "Pick",
            Choice::Reword => "Reword",
            Choice::Squash => "Squash",
            Choice::Drop => "Drop",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Choice::Pick => "P",
            Choice::Reword => "R",
            Choice::Squash => "S",
            Choice::Drop => "D",
        }
    }

    fn mark(self) -> Option<(&'static str, u32)> {
        match self {
            Choice::Pick => None,
            Choice::Reword => Some(("reworded", REMOTE)),
            Choice::Squash => Some(("squashed", MODIFIED)),
            Choice::Drop => Some(("dropped", DELETED)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) oid: String,
    pub(crate) subject: String,
    pub(crate) author: String,
    pub(crate) time: i64,
    date: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) commit: Commit,
    pub(crate) choice: Choice,
    pub(crate) message: Option<String>,
    original: Option<String>,
}

impl Row {
    fn new(commit: Commit) -> Self {
        Self { commit, choice: Choice::Pick, message: None, original: None }
    }

    fn shown_subject(&self) -> String {
        match (&self.message, self.choice) {
            (Some(message), Choice::Reword) => crate::rewrite::split(message).0,
            _ => self.commit.subject.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ends {
    pub(crate) branch: String,
    full: OsString,
    switch: bool,
    seen: Seen,
    tip: String,
    onto: Source,
    target: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Loaded {
    pub(crate) ends: Ends,
    pub(crate) commits: Vec<Commit>,
    pub(crate) upstream: Option<String>,
    pub(crate) in_place: bool,
}

pub(crate) struct Editor {
    loaded: Loaded,
    rows: Vec<Row>,
    highlighted: usize,
    scroll: UniformListScrollHandle,
    starting: bool,
}

impl Editor {
    fn new(loaded: Loaded) -> Self {
        let rows = loaded.commits.iter().cloned().map(Row::new).collect();
        Self { loaded, rows, highlighted: 0, scroll: UniformListScrollHandle::new(), starting: false }
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn highlighted(&self) -> usize {
        self.highlighted
    }

    pub(crate) fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    pub(crate) fn heading(&self) -> String {
        format!("Rebasing {} onto {}", commits(self.rows.len()), self.loaded.ends.label)
    }

    pub(crate) fn summary(&self) -> String {
        summary(&self.rows, &self.loaded.commits)
    }

    pub(crate) fn start_refusal(&self) -> Option<String> {
        if self.starting {
            return Some("The rebase is starting".to_owned());
        }
        if squash_without_receiver(&self.rows) {
            return Some(NO_RECEIVER.to_owned());
        }
        if self.loaded.in_place && self.summary().is_empty() {
            return Some(UNCHANGED.to_owned());
        }
        None
    }

    fn reorder(&mut self, from: usize, to: usize) -> bool {
        if from >= self.rows.len() || to >= self.rows.len() || from == to {
            return false;
        }
        let row = self.rows.remove(from);
        self.rows.insert(to, row);
        self.highlighted = to;
        true
    }
}

#[derive(Default)]
pub(crate) struct State {
    open: Option<Editor>,
    loading: bool,
}

impl State {
    pub(crate) fn open(&self) -> bool {
        self.open.is_some()
    }

    pub(crate) fn editor(&self) -> Option<&Editor> {
        self.open.as_ref()
    }

    pub(crate) fn loading(&self) -> bool {
        self.loading
    }
}

pub(crate) fn keyed(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::RebaseEditor
        && !ui.dialog_open()
        && !ui.modal_open()
        && !ui.transient_open()
        && !ui.preferences.open()
}

fn commits(count: usize) -> String {
    match count {
        1 => "1 commit".to_owned(),
        n => format!("{n} commits"),
    }
}

pub(crate) fn summary(rows: &[Row], original: &[Commit]) -> String {
    let count = |choice: Choice| rows.iter().filter(|row| row.choice == choice).count();
    let mut parts = Vec::new();
    for (choice, word) in [(Choice::Squash, "squashed"), (Choice::Drop, "dropped"), (Choice::Reword, "reworded")] {
        let n = count(choice);
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let moved = rows.len() != original.len() || rows.iter().zip(original).any(|(row, commit)| row.commit.oid != commit.oid);
    if moved {
        parts.push("order changed".to_owned());
    }
    parts.join(", ")
}

fn squash_without_receiver(rows: &[Row]) -> bool {
    let mut kept = false;
    for row in rows.iter().rev() {
        match row.choice {
            Choice::Drop => {}
            Choice::Squash if !kept => return true,
            Choice::Squash => {}
            Choice::Pick | Choice::Reword => kept = true,
        }
    }
    false
}

pub(crate) fn todo(rows: &[Row]) -> Result<String, String> {
    let mut lines = Vec::with_capacity(rows.len() + 1);
    let mut amend: Option<String> = None;
    let mut kept = false;
    for row in rows.iter().rev() {
        validate_oid(&row.commit.oid)?;
        match row.choice {
            Choice::Drop => {}
            Choice::Squash if !kept => return Err(format!("{NO_RECEIVER}.")),
            Choice::Squash => lines.push(format!("fixup {}", row.commit.oid)),
            Choice::Pick | Choice::Reword => {
                lines.extend(amend.take());
                lines.push(format!("pick {}", row.commit.oid));
                kept = true;
                if let (Choice::Reword, Some(message)) = (row.choice, &row.message) {
                    amend = Some(crate::rewrite::amend_line(message)?);
                }
            }
        }
    }
    lines.extend(amend);
    if lines.is_empty() {
        lines.push("noop".to_owned());
    }
    Ok(lines.join("\n") + "\n")
}

fn listing(repo: &Repository, tip: &str, target: &str) -> Result<Vec<(Commit, usize)>, String> {
    validate_oid(tip)?;
    validate_oid(target)?;
    let output = checked(repo.read_command().args([
        "log",
        "--topo-order",
        "--no-show-signature",
        "--no-notes",
        &format!("--max-count={}", LIMIT + 1),
        "--format=%H%x00%P%x00%an%x00%at%x00%s",
        tip,
        "--not",
        target,
        "--",
    ]))?;
    let mut listed = Vec::new();
    for line in output.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(5, |&b| b == 0);
        let mut next = || String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        let (oid, parents, author, time, subject) = (next(), next(), next(), next(), next());
        validate_oid(&oid)?;
        let time: i64 = time.trim().parse().map_err(|_| "Git listed a commit without its date.".to_owned())?;
        let date = view::date_of(time);
        listed.push((Commit { oid, subject, author, time, date }, parents.split_whitespace().count()));
    }
    Ok(listed)
}

pub(crate) fn load(repo: &Repository, ends: Ends) -> Result<Result<Loaded, String>, String> {
    let (branch, label) = (&ends.branch, &ends.label);
    if repo.is_ancestor(&ends.tip, &ends.target)? {
        return Ok(Err(format!("{label} already has every commit on {branch}, so there is nothing to rebase.")));
    }
    let listed = listing(repo, &ends.tip, &ends.target)?;
    if listed.len() > LIMIT {
        return Ok(Err(format!(
            "{branch} has more than {LIMIT} commits that {label} lacks. The editor takes up to {LIMIT} at a time."
        )));
    }
    if listed.iter().any(|(_, parents)| *parents > 1) {
        return Ok(Err(crate::rewrite::ACROSS_MERGE.to_owned()));
    }
    if let Some((root, _)) = listed.iter().find(|(_, parents)| *parents == 0) {
        if crate::rewrite::shallow_at(repo, &root.oid)? {
            return Ok(Err("This range reaches the edge of a shallow clone. Fetch the full history to rebase it.".to_owned()));
        }
        return Ok(Err(format!(
            "This range reaches the first commit, so {branch} and {label} share no history. Rebasing the first commit is not supported."
        )));
    }
    let commits: Vec<Commit> = listed.into_iter().map(|(commit, _)| commit).collect();
    let Some(oldest) = commits.last() else {
        return Ok(Err(format!("{label} already has every commit on {branch}, so there is nothing to rebase.")));
    };
    let upstream = match crate::rebase::upstream_of(repo, &ends.full)? {
        Some((shown, oid)) if repo.is_ancestor(&oldest.oid, &oid)? => Some(shown),
        _ => None,
    };
    let in_place = repo.is_ancestor(&ends.target, &ends.tip)?;
    Ok(Ok(Loaded { ends, commits, upstream, in_place }))
}

#[derive(Clone, Debug)]
pub(crate) struct Start {
    ends: Ends,
    todo: String,
    summary: String,
    count: usize,
}

impl Start {
    pub(crate) fn of(editor: &Editor) -> Result<Self, String> {
        Ok(Self {
            ends: editor.loaded.ends.clone(),
            todo: todo(&editor.rows)?,
            summary: editor.summary(),
            count: editor.rows.iter().filter(|row| row.choice != Choice::Drop).count(),
        })
    }

    pub(crate) fn apply(&self, repo: &Repository, stash: bool, kept: &Kept) -> Result<Outcome, Failure> {
        let ends = &self.ends;
        validate_oid(&ends.tip)?;
        validate_oid(&ends.target)?;
        let attached = ends.seen.head_ref.as_deref().is_some_and(|name| name.as_bytes().starts_with(b"refs/heads/"));
        if !attached || ends.seen.head.is_none() {
            return Err(Failure::from("Check out a branch first.".to_owned()));
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&ends.seen)?;
        let Some(short) = ends.full.as_bytes().strip_prefix(b"refs/heads/").filter(|short| !short.is_empty()) else {
            return Err(Failure::from("Select a local branch to rebase.".to_owned()));
        };
        if ends.switch {
            repo.expect_ref(&ends.full, &ends.tip)?;
        } else if ends.seen.head_ref.as_deref() != Some(ends.full.as_os_str())
            || ends.seen.head.as_deref() != Some(ends.tip.as_str())
        {
            return Err(Failure::from("The checked-out branch or its tip changed. Reopen the action.".to_owned()));
        }
        crate::rebase::expect_onto(repo, &ends.onto, &ends.target)?;
        let switch = ends.switch.then(|| OsStr::from_bytes(short));
        let (branch, label) = (&ends.branch, &ends.label);
        let finished = format!("Rebased {branch} onto {label}");
        let (head, stashed) = crate::autostash::by_git(
            repo,
            stash,
            kept,
            || Retry::Interactive(Box::new(self.clone())),
            |autostash| crate::rewrite::scripted(repo, Some(&ends.target), switch, &self.todo, autostash),
            |_| finished.clone(),
        )
        .map_err(|mut failure| {
            if failure.text.starts_with(&finished) {
                failure.actions = crate::operation::backend::offer_force_push(repo, Outcome::default()).actions;
            }
            failure
        })?;
        let mut detail = match self.summary.as_str() {
            "" => format!("{} replayed. {branch} is at {}.", capitalised(&commits(self.count)), notice::short(&head)),
            summary => format!("{}. {branch} is at {}.", capitalised(summary), notice::short(&head)),
        };
        if stashed {
            detail.push_str(" Your local changes were stashed first and put back.");
        }
        Ok(crate::operation::backend::offer_force_push(repo, Outcome::titled(finished, detail)))
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Open { branch: Option<OsString>, onto: Source },
    Highlight(usize),
    Step(isize),
    Set(usize, Choice),
    SetHighlighted(Choice),
    Shift(isize),
    MoveRow { from: usize, to: usize },
    Reword(usize),
    RewordHighlighted,
    Reset,
    Cancel,
    Start,
}

impl Action {
    pub(crate) fn writes(&self) -> bool {
        matches!(self, Action::Open { .. } | Action::Start)
    }
}

#[derive(Debug)]
pub(crate) struct Reword {
    oid: String,
    summary: String,
    description: String,
    original: String,
}

fn title(ui: &Ui, name: &str) -> String {
    match crate::rebase::on_branch(ui) {
        true => format!("Interactive rebase {} onto {name}…", ui.branch),
        false => format!("Interactive rebase onto {name}…"),
    }
}

fn current(ui: &Ui) -> Availability {
    Availability::unless(
        busy(ui)
            .or_else(|| operating(ui))
            .or_else(|| (!crate::rebase::on_branch(ui)).then(|| "Check out a branch first".to_owned())),
    )
}

fn onto(ui: &Ui, target: &Target) -> Option<view::Action> {
    let source = Source::of(target)?;
    source.shown(ui)?;
    Some(view::Action::RebaseEditor(Action::Open { branch: None, onto: source }))
}

fn editing(ui: &Ui, target: &Target) -> Availability {
    match target {
        Target::Repo if ui.rebase_editor.open() => Availability::Enabled,
        _ => Availability::Hidden,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "branch.interactive-rebase",
        Group::Switch,
        |ui, target| match target {
            Target::Branch(name) => title(ui, &name.to_string_lossy()),
            _ => String::new(),
        },
        |ui, target, _| match target {
            Target::Branch(name) if !crate::rebase::checked_out(ui, name) && ui.refs.oid_of(target).is_some() => current(ui),
            _ => Availability::Hidden,
        },
        onto,
    )
    .words(&["interactive rebase", "rebase -i", "reorder", "squash", "tidy", "rewrite"])
    .icon("list"),
    Command::new(
        "branch.interactive-rebase-remote",
        Group::Switch,
        |ui, target| match target {
            Target::RemoteBranch(name) => title(ui, &name.to_string_lossy()),
            _ => String::new(),
        },
        |ui, target, _| match target {
            Target::RemoteBranch(_) if ui.refs.oid_of(target).is_some() => current(ui),
            _ => Availability::Hidden,
        },
        onto,
    )
    .words(&["interactive rebase", "rebase -i", "reorder", "squash", "tidy", "rewrite"])
    .icon("list"),
    Command::new(
        "tag.interactive-rebase",
        Group::Switch,
        |ui, target| match target {
            Target::Tag(name) => title(ui, &name.to_string_lossy()),
            _ => String::new(),
        },
        |ui, target, _| match target {
            Target::Tag(_) if ui.refs.oid_of(target).is_some() => current(ui),
            _ => Availability::Hidden,
        },
        onto,
    )
    .words(&["interactive rebase", "rebase -i", "reorder", "squash", "tidy", "rewrite"])
    .icon("list"),
    Command::new(
        "commit.interactive-rebase",
        Group::Switch,
        |ui, _| title(ui, "this commit"),
        |ui, target, _| match target {
            Target::Commit(oid) if ui.head.as_deref() != Some(oid.as_str()) => current(ui),
            _ => Availability::Hidden,
        },
        onto,
    )
    .words(&["interactive rebase", "rebase -i", "children", "reorder", "squash", "tidy", "rewrite"])
    .icon("list"),
    Command::new(
        "rebase-editor.pick",
        Group::Reverse,
        |_, _| "Pick commit".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::SetHighlighted(Choice::Pick))),
    )
    .words(&["pick", "keep"])
    .keys("p")
    .scope(KEYED),
    Command::new(
        "rebase-editor.reword",
        Group::Reverse,
        |_, _| "Reword commit…".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::RewordHighlighted)),
    )
    .words(&["reword", "edit message"])
    .keys("r")
    .scope(KEYED),
    Command::new(
        "rebase-editor.squash",
        Group::Reverse,
        |_, _| "Squash commit".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::SetHighlighted(Choice::Squash))),
    )
    .words(&["squash", "fixup", "fold"])
    .keys("s")
    .scope(KEYED),
    Command::new(
        "rebase-editor.drop",
        Group::Reverse,
        |_, _| "Drop commit".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::SetHighlighted(Choice::Drop))),
    )
    .words(&["drop", "remove"])
    .keys("d")
    .scope(KEYED),
    Command::new(
        "rebase-editor.move-up",
        Group::Reverse,
        |_, _| "Move commit up".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::Shift(-1))),
    )
    .words(&["move", "reorder", "later"])
    .keys("alt-up")
    .scope(KEYED),
    Command::new(
        "rebase-editor.move-down",
        Group::Reverse,
        |_, _| "Move commit down".into(),
        |ui, target, _| editing(ui, target),
        |_, _| Some(view::Action::RebaseEditor(Action::Shift(1))),
    )
    .words(&["move", "reorder", "earlier"])
    .keys("alt-down")
    .scope(KEYED),
];

pub(crate) fn describe_reword(reword: &Reword, _: &Ui) -> Spec {
    let mut spec = Spec::new("Edit commit message", "Update Message")
        .line("The new message is applied when the rebase starts.");
    spec.field = Some(Field {
        label: "Summary",
        placeholder: "Summary",
        initial: reword.summary.clone(),
        masked: false,
        validate: Box::new(|text, _| text.trim().is_empty().then(|| "Enter a summary.".to_owned())),
    });
    spec.second = Some(Field {
        label: "Description",
        placeholder: "Description (optional)",
        initial: reword.description.clone(),
        masked: false,
        validate: Box::new(|_, _| None),
    });
    spec.second_lines = true;
    spec
}

pub(crate) fn confirm_reword(tab: &mut Tab, reword: Reword, answer: Answer, cx: &mut Context<Tab>) {
    if answer.text.trim().is_empty() {
        return;
    }
    let message = crate::rewrite::joined(&answer.text, &answer.second);
    let Some(editor) = tab.ui.rebase_editor.open.as_mut() else { return };
    let Some(at) = editor.rows.iter().position(|row| row.commit.oid == reword.oid) else { return };
    let row = &mut editor.rows[at];
    row.original = Some(reword.original.clone());
    if message == crate::rewrite::joined(&crate::rewrite::split(&reword.original).0, &crate::rewrite::split(&reword.original).1) {
        row.message = None;
        row.choice = Choice::Pick;
    } else {
        row.message = Some(message);
        row.choice = Choice::Reword;
    }
    editor.highlighted = at;
    tab.ui.focus_on_open = true;
    cx.notify();
}

fn ask_reword(tab: &mut Tab, index: usize, cx: &mut Context<Tab>) {
    let Some(editor) = tab.ui.rebase_editor.open.as_ref().filter(|editor| !editor.starting) else { return };
    let Some(row) = editor.rows.get(index) else { return };
    let oid = row.commit.oid.clone();
    if let Some(original) = row.original.clone() {
        let shown = row.message.clone().filter(|_| row.choice == Choice::Reword).unwrap_or_else(|| original.clone());
        let (summary, description) = crate::rewrite::split(&shown);
        if let Some(editor) = tab.ui.rebase_editor.open.as_mut() {
            editor.highlighted = index;
        }
        prompt::ask(&mut tab.ui, prompt::Kind::RebaseMessage(Reword { oid, summary, description, original }), cx);
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    Tab::work(
        cx,
        {
            let oid = oid.clone();
            move || repo.commit_message(&oid)
        },
        move |tab, read, cx| {
            if tab.epoch != epoch || tab.ui.dialog_open() {
                return;
            }
            match read {
                Ok(original) => {
                    let Some(editor) = tab.ui.rebase_editor.open.as_mut() else { return };
                    let Some(at) = editor.rows.iter().position(|row| row.commit.oid == oid) else { return };
                    editor.rows[at].original = Some(original);
                    ask_reword(tab, at, cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

fn open(tab: &mut Tab, branch: Option<OsString>, onto: Source, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let (Some(head), Some(head_ref)) = (ui.head.clone(), ui.head_ref.clone()) else {
        tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
        cx.notify();
        return;
    };
    if !crate::rebase::on_branch(ui) {
        tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
        cx.notify();
        return;
    }
    let current = OsStr::from_bytes(head_ref.as_bytes().strip_prefix(b"refs/heads/").unwrap_or_default()).to_owned();
    let branch = branch.filter(|name| *name != current);
    if matches!(&onto, Source::Branch(name) if Some(name) == branch.as_ref().or(Some(&current))) {
        return;
    }
    let Some(target) = onto.shown(ui) else { return };
    let seen = Seen::of(ui);
    let ends = match branch {
        None => Ends {
            branch: ui.branch.clone(),
            full: head_ref,
            switch: false,
            seen,
            tip: head,
            label: onto.label(),
            onto,
            target,
        },
        Some(name) => {
            let Some(tip) = ui.refs.oid_of(&Target::Branch(name.clone())).map(str::to_owned) else { return };
            Ends {
                branch: name.to_string_lossy().into_owned(),
                full: crate::rebase::branch_ref(&name),
                switch: true,
                seen,
                tip,
                label: onto.label(),
                onto,
                target,
            }
        }
    };
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    tab.ui.rebase_editor.loading = true;
    view::focus(&tab.ui, window);
    let seen = ends.seen.clone();
    Tab::work(
        cx,
        move || load(&repo, ends),
        move |tab, loaded, cx| {
            tab.ui.rebase_editor.loading = false;
            let ui = &tab.ui;
            if tab.epoch != epoch || Seen::of(ui) != seen || ui.busy() || ui.dialog_open() || ui.operation.active() {
                cx.notify();
                return;
            }
            match loaded {
                Ok(Ok(loaded)) => {
                    crate::blame::leave(tab);
                    crate::browse::leave(tab);
                    crate::file_history::leave(tab);
                    tab.ui.activity.close();
                    tab.ui.diff_open = false;
                    tab.ui.menu = None;
                    tab.ui.rebase_editor.open = Some(Editor::new(loaded));
                    tab.ui.focus_on_open = true;
                }
                Ok(Err(limit)) => {
                    tab.ui.notices.show(Notice::new(Tone::Info, "This cannot be rebased interactively", limit));
                }
                Err(error) => tab.error(error, cx),
            }
            cx.notify();
        },
    );
}

pub(crate) fn run(tab: &mut Tab, start: Start, stash: bool, cx: &mut Context<Tab>) {
    if tab.ui.busy() {
        return;
    }
    let (target, label) = (start.ends.target.clone(), start.ends.label.clone());
    tab.ui.operation.started = Some((Operation::Rebase, target, label.clone()));
    if let Some(editor) = tab.ui.rebase_editor.open.as_mut() {
        editor.starting = true;
    }
    let kept = Kept::default();
    let remembered = kept.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_here = stopped.clone();
    let title = format!("Interactive rebase {} onto {label}", start.ends.branch);
    tab.run_operation_or(
        cx,
        "Rebasing…",
        "Rebased",
        true,
        move |repo| {
            let result = start.apply(&repo, stash, &kept);
            if result.is_err() && repo.expect_no_operation().is_err() {
                stopped_here.store(true, Ordering::SeqCst);
            }
            crate::journal::titled(result, title)
        },
        |tab, _| {
            tab.ui.rebase_editor.open = None;
            tab.ui.focus_on_open = true;
        },
        move |tab, _| {
            remembered.remember(&mut tab.ui);
            match stopped.load(Ordering::SeqCst) {
                true => tab.ui.rebase_editor.open = None,
                false => {
                    if let Some(editor) = tab.ui.rebase_editor.open.as_mut() {
                        editor.starting = false;
                    }
                }
            }
        },
    );
}

fn scroll_to(editor: &Editor) {
    editor.scroll.scroll_to_item(editor.highlighted, ScrollStrategy::Center);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if let Action::Open { branch, onto } = action {
        if tab.ui.busy() || tab.ui.dialog_open() || tab.ui.operation.active() || tab.ui.rebase_editor.loading {
            return;
        }
        open(tab, branch, onto, window, cx);
        return;
    }
    if tab.ui.dialog_open() {
        return;
    }
    let Some(editor) = tab.ui.rebase_editor.open.as_mut() else { return };
    let last = editor.rows.len().saturating_sub(1);
    match action {
        Action::Open { .. } => {}
        Action::Cancel => {
            if editor.starting {
                return;
            }
            tab.ui.rebase_editor.open = None;
            view::focus(&tab.ui, window);
        }
        _ if editor.starting => return,
        Action::Highlight(index) => {
            editor.highlighted = index.min(last);
            scroll_to(editor);
        }
        Action::Step(delta) => {
            editor.highlighted = editor.highlighted.saturating_add_signed(delta).min(last);
            scroll_to(editor);
        }
        Action::Set(index, choice) => set(tab, index, choice, cx),
        Action::SetHighlighted(choice) => {
            let index = editor.highlighted;
            set(tab, index, choice, cx);
        }
        Action::Shift(delta) => {
            let from = editor.highlighted;
            let to = from.saturating_add_signed(delta).min(last);
            if editor.reorder(from, to) {
                scroll_to(editor);
            }
        }
        Action::MoveRow { from, to } => {
            if editor.reorder(from, to.min(last)) {
                scroll_to(editor);
            }
        }
        Action::Reword(index) => ask_reword(tab, index, cx),
        Action::RewordHighlighted => {
            let index = editor.highlighted;
            ask_reword(tab, index, cx);
        }
        Action::Reset => {
            editor.rows = editor.loaded.commits.iter().cloned().map(Row::new).collect();
            editor.highlighted = editor.highlighted.min(last);
            scroll_to(editor);
        }
        Action::Start => {
            let refusal = editor.start_refusal();
            let start = Start::of(editor);
            if let Some(reason) = refusal.or_else(|| busy(&tab.ui)).or_else(|| operating(&tab.ui)) {
                tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
                cx.notify();
                return;
            }
            match start {
                Ok(start) => {
                    let stash = tab.ui.autostash.always;
                    run(tab, start, stash, cx);
                }
                Err(error) => tab.error(error, cx),
            }
        }
    }
    cx.notify();
}

fn set(tab: &mut Tab, index: usize, choice: Choice, cx: &mut Context<Tab>) {
    let Some(editor) = tab.ui.rebase_editor.open.as_mut() else { return };
    let last = editor.rows.len().saturating_sub(1);
    if index > last || editor.rows.is_empty() {
        return;
    }
    editor.highlighted = index;
    match choice {
        Choice::Reword => ask_reword(tab, index, cx),
        Choice::Squash if index == last => {
            tab.ui.notices.show(Notice::new(Tone::Info, OLDEST, ""));
        }
        choice => editor.rows[index].choice = choice,
    }
    cx.notify();
}

#[derive(Clone)]
struct Carried {
    from: usize,
    subject: String,
}

struct Preview {
    subject: String,
    grabbed: Point<Pixels>,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let left = (self.grabbed.x - px(8.)).max(px(0.));
        let top = (self.grabbed.y - px(12.)).max(px(0.));
        div().pl(left).pt(top).child(
            div()
                .px_2()
                .py_1()
                .max_w(px(360.))
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(ACCENT))
                .bg(rgb(RAISED))
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(TEXT))
                .opacity(0.92)
                .child(self.subject.clone()),
        )
    }
}

fn choice_control<T: Desk>(index: usize, row: &Row, last: bool, cx: &mut Context<T>) -> Div {
    let mut control = kit::segmented();
    for choice in Choice::ALL {
        let chosen = row.choice == choice;
        let refused = (choice == Choice::Squash && last).then_some(OLDEST);
        let ink = match (chosen, choice) {
            (true, Choice::Drop) => DELETED,
            (true, Choice::Squash) => MODIFIED,
            (true, Choice::Reword) => REMOTE,
            (true, Choice::Pick) => TEXT,
            (false, _) => TEXT_3,
        };
        let tip: SharedString = match refused {
            Some(reason) => reason.into(),
            None => format!("{} ({})", choice.label(), choice.key()).into(),
        };
        let segment = div()
            .id(ElementId::Name(format!("rebase-{}-{index}", choice.label().to_lowercase()).into()))
            .h(px(22.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .text_size(px(TEXT_SMALL))
            .text_color(rgb(ink))
            .border_1()
            .border_color(kit::clear())
            .when(chosen, |d| d.bg(rgb(RAISED)).font_weight(FontWeight::SEMIBOLD))
            .when(refused.is_some(), |d| d.opacity(0.42))
            .tooltip(kit::tip(tip))
            .child(choice.label());
        let segment = match refused {
            Some(_) => segment.on_click(|_, _, cx| cx.stop_propagation()),
            None => {
                let action = view::Action::RebaseEditor(Action::Set(index, choice));
                let activate = action.clone();
                segment
                    .cursor_pointer()
                    .when(!chosen, |d| d.hover(|s| s.text_color(rgb(TEXT_2))))
                    .focusable()
                    .focus(|s| s.border_color(rgb(ACCENT)))
                    .key_context("DeskButton")
                    .tab_stop(true)
                    .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                        cx.stop_propagation();
                        if !event.is_keyboard() {
                            app.dispatch(action.clone(), w, cx)
                        }
                    }))
                    .on_action(cx.listener(move |app, _: &view::ActivateControl, w, cx| {
                        app.dispatch(activate.clone(), w, cx)
                    }))
            }
        };
        control = control.child(segment);
    }
    control
}

fn editor_row<T: Desk>(index: usize, row: &Row, highlighted: bool, last: bool, now: i64, cx: &mut Context<T>) -> Stateful<Div> {
    let commit = &row.commit;
    let short = notice::short(&commit.oid).to_owned();
    let when = view::when(commit.time, &commit.date, now);
    let time = commit.time;
    let dropped = row.choice == Choice::Drop;
    let subject = row.shown_subject();
    let carried = Carried { from: index, subject: subject.clone() };
    let element = kit::row(("rebase-row", index), highlighted)
        .h(px(ROW_HEIGHT))
        .pl_2()
        .pr_3()
        .gap_3()
        .child(choice_control(index, row, last, cx))
        .child(div().flex_shrink_0().font_family(MONO).text_size(px(11.5)).text_color(rgb(TEXT_3)).child(short))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    kit::fit(subject)
                        .weight(FontWeight::MEDIUM)
                        .color(if dropped { TEXT_OFF } else { TEXT })
                        .grow(),
                )
                .when_some(row.choice.mark(), |d, (mark, ink)| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .px(px(6.))
                            .rounded(px(RADIUS_CHIP))
                            .border_1()
                            .border_color(rgb(ink))
                            .text_size(px(TEXT_TINY))
                            .text_color(rgb(ink))
                            .child(mark),
                    )
                }),
        )
        .child(
            div()
                .id(("rebase-row-when", index))
                .flex_shrink_0()
                .w(px(220.))
                .tooltip(kit::tip_with(move || view::exact_time(time)))
                .child(kit::fit(format!("{} · {when}", commit.author)).size(11.5).color(TEXT_3).grow()),
        )
        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
            let action = match event.click_count() {
                1 => Action::Highlight(index),
                _ => Action::Reword(index),
            };
            app.dispatch(view::Action::RebaseEditor(action), w, cx)
        }))
        .on_drag(carried, |carried, grabbed, _, cx| {
            let subject = carried.subject.clone();
            cx.new(|_| Preview { subject, grabbed })
        });
    if !cx.has_active_drag() {
        return element;
    }
    element
        .drag_over::<Carried>(move |style, carried, _, _| match carried.from == index {
            true => style,
            false => style.bg(soft(ACCENT, 0x2e)),
        })
        .on_drop(cx.listener(move |app, carried: &Carried, w, cx| {
            let action = Action::MoveRow { from: carried.from, to: index };
            app.dispatch(view::Action::RebaseEditor(action), w, cx)
        }))
}

pub(crate) fn render<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(editor) = ui.rebase_editor.editor() else { return div().flex_1() };
    let ends = &editor.loaded.ends;
    let header = div()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(kit::icon("list").size(px(14.)).flex_shrink_0().text_color(rgb(TEXT_3)))
                .child(kit::fit(editor.heading()).size(TEXT_TITLE).weight(FontWeight::SEMIBOLD).grow())
                .child(kit::icon_btn("close-rebase-editor", "x", "Cancel (Esc)", view::Action::RebaseEditor(Action::Cancel), cx)),
        )
        .child(kit::muted(
            "Newest at the top. P pick, R reword, S squash, D drop; ⌥↑ and ⌥↓ move the highlighted commit; Return edits its message.",
        ))
        .when(ends.switch, |d| {
            d.child(kit::muted(format!("{} is not checked out. FastKraken switches to it when the rebase starts.", ends.branch)))
        })
        .when_some(editor.loaded.upstream.as_ref(), |d, upstream| {
            d.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(MODIFIED))
                    .child(format!("{upstream} already has some of these commits. You will need to force push afterwards.")),
            )
        });
    let count = editor.rows.len();
    let list = uniform_list(
        SharedString::from("rebase-editor-rows"),
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let Some(editor) = app.ui().rebase_editor.editor() else { return Vec::new() };
            let now = view::now();
            let last = editor.rows.len().saturating_sub(1);
            range
                .filter_map(|i| {
                    let row = editor.rows.get(i)?;
                    Some(editor_row(i, row, editor.highlighted == i, i == last, now, cx))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(editor.scroll.clone())
    .flex_1()
    .min_h_0();
    let summary = editor.summary();
    let refusal = editor.start_refusal().or_else(|| busy(ui)).or_else(|| operating(ui));
    let said = match (summary.as_str(), editor.loaded.in_place) {
        ("", true) => "No changes yet".to_owned(),
        ("", false) => format!("Every commit is replayed onto {} as it is", ends.label),
        (summary, _) => capitalised(summary),
    };
    let mut start = kit::btn("start-rebase", "Start rebase").primary().small().enabled(refusal.is_none());
    if let Some(reason) = &refusal {
        start = start.tip(reason.clone());
    }
    let footer = div()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(PANEL))
        .border_t_1()
        .border_color(rgb(LINE))
        .child(kit::fit(said).size(TEXT_SMALL).color(TEXT_2).grow())
        .child(
            kit::btn("reset-rebase", "Reset")
                .small()
                .ghost()
                .enabled(!editor.starting)
                .tip("Back to Pick for every commit, in the original order")
                .on(view::Action::RebaseEditor(Action::Reset), cx),
        )
        .child(
            kit::btn("cancel-rebase", "Cancel")
                .small()
                .enabled(!editor.starting)
                .on(view::Action::RebaseEditor(Action::Cancel), cx),
        )
        .child(start.on(view::Action::RebaseEditor(Action::Start), cx));
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(CANVAS))
        .child(header)
        .child(list)
        .child(kit::scrollbar("rebase-editor-scrollbar", &editor.scroll))
        .child(footer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::{commit, oid};
    use crate::repository::tests::Fixture;
    use std::fs;

    fn subjects(f: &Fixture, rev: &str) -> Vec<String> {
        String::from_utf8(f.git(&["log", "--format=%s", rev])).unwrap().lines().map(str::to_owned).collect()
    }

    fn status(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "--untracked-files=all"])).unwrap()
    }

    fn refs(f: &Fixture) -> String {
        String::from_utf8(f.git(&["for-each-ref", "--format=%(refname) %(objectname)"])).unwrap()
    }

    fn trail() -> (Fixture, String) {
        let f = Fixture::new();
        commit(&f, "base.txt", "start\n", "Start the trail");
        let base = oid(&f, "HEAD");
        commit(&f, "camp.txt", "tents\n", "Pitch the camp");
        commit(&f, "map.txt", "creek\n", "Map the creek");
        commit(&f, "ridge.txt", "pass\n", "Climb the ridge");
        commit(&f, "lake.txt", "cold\n", "Swim in the lake");
        (f, base)
    }

    fn ends_onto(f: &Fixture, onto: Source) -> Ends {
        let target = match &onto {
            Source::Commit(oid) => oid.clone(),
            source => oid(f, &format!("{}^{{commit}}", source.full().to_string_lossy())),
        };
        let tip = oid(f, "HEAD");
        Ends {
            branch: "main".into(),
            full: "refs/heads/main".into(),
            switch: false,
            seen: Seen { head_ref: Some("refs/heads/main".into()), head: Some(tip.clone()) },
            tip,
            label: onto.label(),
            onto,
            target,
        }
    }

    fn editor(f: &Fixture, onto: Source) -> Editor {
        Editor::new(load(&f.repo, ends_onto(f, onto)).unwrap().expect("the range can be edited"))
    }

    fn started(f: &Fixture, editor: &Editor) -> Result<Outcome, Failure> {
        Start::of(editor).unwrap().apply(&f.repo, false, &Kept::default())
    }

    fn at(editor: &Editor, subject: &str) -> usize {
        editor.rows.iter().position(|row| row.commit.subject == subject).unwrap()
    }

    #[test]
    fn the_editor_lists_the_range_newest_first_with_author_and_date() {
        let (f, base) = trail();
        let editor = editor(&f, Source::Commit(base.clone()));
        let listed: Vec<&str> = editor.rows.iter().map(|row| row.commit.subject.as_str()).collect();
        assert_eq!(listed, ["Swim in the lake", "Climb the ridge", "Map the creek", "Pitch the camp"]);
        assert!(editor.rows.iter().all(|row| row.choice == Choice::Pick && !row.commit.author.is_empty() && row.commit.time > 0));
        assert_eq!(editor.heading(), format!("Rebasing 4 commits onto {}", notice::short(&base)));
        assert!(editor.loaded.in_place && editor.loaded.upstream.is_none());
        assert_eq!(editor.summary(), "");
        assert_eq!(editor.start_refusal().as_deref(), Some(UNCHANGED));
    }

    #[test]
    fn reordering_and_dropping_rewrites_the_branch_in_the_new_order() {
        let (f, base) = trail();
        let mut editor = editor(&f, Source::Commit(base.clone()));
        let ridge = at(&editor, "Climb the ridge");
        editor.highlighted = ridge;
        assert!(editor.reorder(ridge, ridge + 2));
        let map = at(&editor, "Map the creek");
        editor.rows[map].choice = Choice::Drop;
        assert_eq!(editor.summary(), "1 dropped, order changed");
        assert_eq!(editor.start_refusal(), None);
        let lines: Vec<String> = todo(&editor.rows).unwrap().lines().map(str::to_owned).collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.starts_with("pick ")));
        let outcome = started(&f, &editor).ok().expect("the rebase runs");
        assert_eq!(outcome.title, format!("Rebased main onto {}", notice::short(&base)));
        assert!(outcome.detail.starts_with("1 dropped, order changed. main is at "), "{}", outcome.detail);
        assert_eq!(subjects(&f, "main"), ["Swim in the lake", "Pitch the camp", "Climb the ridge", "Start the trail"]);
        assert_eq!(String::from_utf8(f.git(&["ls-tree", "--name-only", "main"])).unwrap(), "base.txt\ncamp.txt\nlake.txt\nridge.txt\n");
        assert_eq!(oid(&f, "main~3"), base);
        assert_eq!(status(&f), "");
        assert!(!f.repo.git_dir().join("rebase-merge").exists());
    }

    #[test]
    fn squash_folds_into_the_commit_below_and_reword_applies_its_message_after() {
        let (f, base) = trail();
        let mut editor = editor(&f, Source::Commit(base.clone()));
        let camp = at(&editor, "Pitch the camp");
        editor.rows[camp].choice = Choice::Reword;
        editor.rows[camp].message = Some("Make camp\n\nTents and a fire\n".into());
        let map = at(&editor, "Map the creek");
        editor.rows[map].choice = Choice::Squash;
        let lake = at(&editor, "Swim in the lake");
        editor.rows[lake].choice = Choice::Squash;
        assert_eq!(editor.summary(), "2 squashed, 1 reworded");
        let listed = todo(&editor.rows).unwrap();
        let words: Vec<&str> = listed.lines().map(|line| line.split(' ').next().unwrap()).collect();
        assert_eq!(words, ["pick", "fixup", "exec", "pick", "fixup"]);
        started(&f, &editor).ok().expect("the rebase runs");
        assert_eq!(subjects(&f, "main"), ["Climb the ridge", "Make camp", "Start the trail"]);
        assert_eq!(String::from_utf8(f.git(&["log", "-1", "--format=%B", "main~1"])).unwrap(), "Make camp\n\nTents and a fire\n\n");
        assert_eq!(String::from_utf8(f.git(&["diff", "--name-only", "main~2", "main~1"])).unwrap(), "camp.txt\nmap.txt\n");
        assert_eq!(String::from_utf8(f.git(&["diff", "--name-only", "main~1", "main"])).unwrap(), "lake.txt\nridge.txt\n");
        assert_eq!(status(&f), "");
    }

    #[test]
    fn the_oldest_commit_cannot_be_squashed_and_a_squash_with_nothing_kept_below_is_refused() {
        let (f, base) = trail();
        let mut editor = editor(&f, Source::Commit(base));
        let last = editor.rows.len() - 1;
        editor.rows[last].choice = Choice::Drop;
        editor.rows[last - 1].choice = Choice::Squash;
        assert_eq!(editor.start_refusal().as_deref(), Some(NO_RECEIVER));
        assert!(todo(&editor.rows).is_err());
        editor.rows[last].choice = Choice::Pick;
        assert_eq!(editor.start_refusal(), None);
        let all_dropped: Vec<Row> = editor.rows.iter().cloned().map(|mut row| {
            row.choice = Choice::Drop;
            row
        }).collect();
        assert_eq!(todo(&all_dropped).unwrap(), "noop\n");
    }

    #[test]
    fn rebasing_onto_another_branch_replays_the_branch_and_counts_what_the_target_lacks() {
        let f = Fixture::new();
        commit(&f, "base.txt", "start\n", "Start the trail");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "bridge.txt", "stone\n", "Bridge the river");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "camp.txt", "tents\n", "Pitch the camp");
        commit(&f, "map.txt", "creek\n", "Map the creek");
        let mut editor = editor(&f, Source::Branch("side".into()));
        assert_eq!(editor.heading(), "Rebasing 2 commits onto side");
        assert!(!editor.loaded.in_place);
        assert_eq!(editor.start_refusal(), None);
        editor.rows[0].choice = Choice::Squash;
        started(&f, &editor).ok().expect("the rebase runs");
        assert_eq!(subjects(&f, "main"), ["Pitch the camp", "Bridge the river", "Start the trail"]);
        assert_eq!(oid(&f, "main~1"), oid(&f, "side"));
        assert_eq!(String::from_utf8(f.git(&["ls-tree", "--name-only", "main"])).unwrap(), "base.txt\nbridge.txt\ncamp.txt\nmap.txt\n");
    }

    #[test]
    fn a_branch_that_is_not_checked_out_is_switched_to_when_the_rebase_starts() {
        let f = Fixture::new();
        commit(&f, "base.txt", "start\n", "Start the trail");
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "camp.txt", "tents\n", "Pitch the camp");
        commit(&f, "map.txt", "creek\n", "Map the creek");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "bridge.txt", "stone\n", "Bridge the river");
        let main = oid(&f, "main");
        let tip = oid(&f, "feature");
        let ends = Ends {
            branch: "feature".into(),
            full: "refs/heads/feature".into(),
            switch: true,
            seen: Seen { head_ref: Some("refs/heads/main".into()), head: Some(main.clone()) },
            tip: tip.clone(),
            onto: Source::Branch("main".into()),
            target: main.clone(),
            label: "main".into(),
        };
        let mut editor = Editor::new(load(&f.repo, ends).unwrap().unwrap());
        assert_eq!(editor.heading(), "Rebasing 2 commits onto main");
        assert!(editor.reorder(0, 1));
        started(&f, &editor).ok().expect("the rebase runs");
        assert_eq!(String::from_utf8(f.git(&["symbolic-ref", "HEAD"])).unwrap(), "refs/heads/feature\n");
        assert_eq!(subjects(&f, "feature"), ["Pitch the camp", "Map the creek", "Bridge the river", "Start the trail"]);
        assert_eq!(oid(&f, "main"), main);
    }

    #[test]
    fn ranges_with_a_merge_or_the_first_commit_or_nothing_to_rebase_are_refused_before_anything_starts() {
        let f = Fixture::new();
        commit(&f, "base.txt", "start\n", "Start the trail");
        let base = oid(&f, "HEAD");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "side.txt", "path\n", "Take the side path");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "map.txt", "creek\n", "Map the creek");
        f.git(&["merge", "-q", "--no-ff", "-m", "Join the side path", "side"]);
        let before = refs(&f);
        let refused = |onto: Source| load(&f.repo, ends_onto(&f, onto)).unwrap().err().expect("the range is refused");
        assert_eq!(refused(Source::Commit(base.clone())), crate::rewrite::ACROSS_MERGE);
        f.git(&["switch", "-q", "--orphan", "fresh"]);
        commit(&f, "fresh.txt", "new\n", "A fresh start");
        let fresh = oid(&f, "HEAD");
        f.git(&["switch", "-q", "main"]);
        let mut ends = ends_onto(&f, Source::Commit(fresh));
        ends.tip = oid(&f, "main~1");
        let unrelated = load(&f.repo, ends).unwrap().err().unwrap();
        assert!(unrelated.contains("share no history"), "{unrelated}");
        let mut ends = ends_onto(&f, Source::Branch("side".into()));
        ends.tip = base.clone();
        let nothing = load(&f.repo, ends).unwrap().err().unwrap();
        assert!(nothing.contains("nothing to rebase"), "{nothing}");
        f.git(&["branch", "-qD", "fresh"]);
        assert_eq!(refs(&f), before);
        assert!(!f.repo.git_dir().join("rebase-merge").exists());
    }

    #[test]
    fn starting_is_refused_when_the_branch_or_the_target_moved_since_the_editor_opened() {
        let (f, base) = trail();
        f.git(&["branch", "camp", &base]);
        let mut editor = editor(&f, Source::Branch("camp".into()));
        editor.rows[0].choice = Choice::Drop;
        commit(&f, "summit.txt", "flag\n", "Reach the summit");
        let tip = oid(&f, "main");
        let err = started(&f, &editor).err().expect("the moved branch is refused");
        assert!(err.text.contains("changed"), "{}", err.text);
        assert_eq!(oid(&f, "main"), tip);
        let mut editor = self::editor(&f, Source::Branch("camp".into()));
        editor.rows[0].choice = Choice::Drop;
        f.git(&["branch", "-qf", "camp", &oid(&f, "main~2")]);
        let err = started(&f, &editor).err().expect("the moved target is refused");
        assert!(err.text.contains("camp changed"), "{}", err.text);
        assert_eq!(oid(&f, "main"), tip);
        assert!(!f.repo.git_dir().join("rebase-merge").exists());
    }

    #[test]
    fn a_rebase_that_conflicts_stops_and_abort_puts_the_tip_back() {
        let f = Fixture::new();
        commit(&f, "base.txt", "start\n", "Start the trail");
        let base = oid(&f, "HEAD");
        commit(&f, "trail.txt", "one\n", "First waypoint");
        commit(&f, "trail.txt", "one\ntwo\n", "Second waypoint");
        let tip = oid(&f, "HEAD");
        let mut editor = editor(&f, Source::Commit(base));
        assert!(editor.reorder(0, 1));
        let err = started(&f, &editor).err().expect("the reordered rebase conflicts");
        assert!(err.text.starts_with("The rebase stopped on a conflict"), "{}", err.text);
        assert!(f.repo.expect_no_operation().is_err());
        f.git(&["rebase", "--abort"]);
        assert_eq!(oid(&f, "main"), tip);
        assert_eq!(status(&f), "");
    }

    #[test]
    fn local_changes_in_the_way_are_offered_a_stash_and_retry_that_puts_them_back() {
        let (f, base) = trail();
        let mut editor = editor(&f, Source::Commit(base));
        editor.rows[1].choice = Choice::Drop;
        fs::write(f.dir.join("lake.txt"), "warm\n").unwrap();
        let err = started(&f, &editor).err().expect("the local change is in the way");
        assert!(err.actions.iter().any(|action| action.0 == crate::autostash::RETRY), "{err:?}");
        let outcome = Start::of(&editor).unwrap().apply(&f.repo, true, &Kept::default()).ok().expect("the retry runs");
        assert!(outcome.detail.ends_with("Your local changes were stashed first and put back."), "{}", outcome.detail);
        assert_eq!(subjects(&f, "main"), ["Swim in the lake", "Map the creek", "Pitch the camp", "Start the trail"]);
        assert_eq!(fs::read_to_string(f.dir.join("lake.txt")).unwrap(), "warm\n");
    }

    #[test]
    fn a_pushed_range_says_a_force_push_is_needed() {
        let (f, base) = trail();
        for (key, value) in [
            ("remote.origin.url", "/nonexistent/origin.git"),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("branch.main.remote", "origin"),
            ("branch.main.merge", "refs/heads/main"),
        ] {
            f.git(&["config", key, value]);
        }
        f.git(&["update-ref", "refs/remotes/origin/main", "main~1"]);
        let editor = editor(&f, Source::Commit(base.clone()));
        assert_eq!(editor.loaded.upstream.as_deref(), Some("origin/main"));
        f.git(&["update-ref", "refs/remotes/origin/main", &base]);
        let editor = self::editor(&f, Source::Commit(base));
        assert_eq!(editor.loaded.upstream, None);
    }

    #[test]
    fn the_journal_records_the_whole_rebase_as_one_move_that_undo_reverses() {
        let (f, base) = trail();
        let tip = oid(&f, "main");
        let mut editor = editor(&f, Source::Commit(base));
        editor.rows[0].choice = Choice::Drop;
        editor.rows[2].choice = Choice::Squash;
        let start = Start::of(&editor).unwrap();
        let entry = crate::journal::tests::entry_of(&f.repo, |repo| start.apply(repo, false, &Kept::default()));
        assert_eq!(entry.changes.len(), 1, "{:?}", entry.changes);
        assert!(matches!(&entry.changes[0], crate::journal::Change::MoveBranch { .. }), "{:?}", entry.changes);
        crate::journal::replay(&f.repo, &entry, crate::journal::Way::Undo).ok().expect("the undo runs");
        assert_eq!(oid(&f, "main"), tip);
        assert_eq!(status(&f), "");
    }

    #[test]
    fn reset_puts_back_every_pick_in_the_original_order_and_reword_keeps_its_message_on_the_row() {
        let (f, base) = trail();
        let mut editor = editor(&f, Source::Commit(base));
        assert!(editor.reorder(0, 3));
        editor.rows[1].choice = Choice::Drop;
        editor.rows[2].choice = Choice::Reword;
        editor.rows[2].message = Some("Cross the creek\n".into());
        assert_eq!(editor.rows[2].shown_subject(), "Cross the creek");
        assert_eq!(editor.summary(), "1 dropped, 1 reworded, order changed");
        editor.rows = editor.loaded.commits.iter().cloned().map(Row::new).collect();
        assert_eq!(editor.summary(), "");
    }
}
