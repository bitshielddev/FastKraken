use crate::{
    Tab,
    file_list::{self, Row, Which},
    kit,
    picked::{How, ListId, Picked},
    repository::{
        ChangedFile, Diff, DiffOptions, Repository, checked, parse_changed_files, patch_options, read_diff,
        validate_oid, validate_path,
    },
    theme::*,
    view::{self, Desk, HistoryRow, Menu, Selection, Ui},
};
use gpui::{Context, Div, FontWeight, SharedString, Window, div, prelude::*, px, rgb, uniform_list};
use std::{
    collections::HashSet,
    ops::Range,
    path::{Path, PathBuf},
};

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const EMPTY_TREE_LONG: &str = "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321";
pub(crate) const RUN: usize = 3;
pub(crate) const UNTRACKED_LEFT_OUT: &str = "Untracked files are left out of this comparison.";
pub(crate) const NO_COMBINED_CHANGE: &str =
    "These commits are not one unbroken line of history, so they have no single combined change.";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rev {
    WorkingTree,
    Commit(String),
}

impl Rev {
    pub(crate) fn oid(&self) -> Option<&str> {
        match self {
            Self::Commit(oid) => Some(oid),
            Self::WorkingTree => None,
        }
    }

    fn short(&self) -> String {
        match self {
            Self::Commit(oid) if empty_tree(oid) => "the empty tree".to_owned(),
            Self::Commit(oid) => oid.chars().take(7).collect(),
            Self::WorkingTree => "your files".to_owned(),
        }
    }
}

fn empty_tree(oid: &str) -> bool {
    oid == EMPTY_TREE || oid == EMPTY_TREE_LONG
}

pub(crate) fn empty_tree_like(oid: &str) -> String {
    if oid.len() == EMPTY_TREE_LONG.len() { EMPTY_TREE_LONG } else { EMPTY_TREE }.to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Names {
    pub ours: String,
    pub theirs: String,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) picked: Option<Picked<Rev>>,
    commits: HashSet<String>,
    ordered: Vec<String>,
    merges: bool,
    primary: Option<Rev>,
    pub(crate) names: Option<Names>,
    shown: Option<Selection>,
    pub(crate) files: file_list::Files<ChangedFile>,
    fork: Option<String>,
    loading: bool,
    failed: Option<String>,
    pub(crate) path: Option<PathBuf>,
    opening: Option<PathBuf>,
    ticket: u64,
    pub(crate) scroll: gpui::UniformListScrollHandle,
}

impl State {
    pub(crate) fn picks(&self, oid: &str) -> bool {
        !self.commits.is_empty() && self.commits.contains(oid)
    }

    pub(crate) fn picks_working_tree(&self) -> bool {
        self.picked.as_ref().is_some_and(|picked| picked.contains(&Rev::WorkingTree))
    }

    pub(crate) fn count(&self) -> usize {
        self.commits.len()
    }

    pub(crate) fn primary(&self) -> Option<&Rev> {
        self.primary.as_ref()
    }

    pub(crate) fn loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn fork(&self) -> Option<&str> {
        self.fork.as_deref()
    }

    pub(crate) fn from(&self, path: &Path) -> Option<PathBuf> {
        self.files.iter().find(|file| file.path == path).and_then(|file| file.from.clone())
    }

    pub(crate) fn merges(&self) -> bool {
        self.merges
    }

    fn set_pick(&mut self, picked: Option<Picked<Rev>>, primary: Option<Rev>, history: &[HistoryRow]) {
        self.commits = picked
            .iter()
            .flat_map(|picked| picked.keys.iter())
            .filter_map(|rev| rev.oid().map(str::to_owned))
            .collect();
        self.picked = picked;
        self.primary = primary;
        self.history_cleared();
        self.history_added(history);
    }

    pub(crate) fn clear_pick(&mut self) {
        self.set_pick(None, None, &[]);
    }

    pub(crate) fn history_cleared(&mut self) {
        self.ordered.clear();
        self.merges = false;
    }

    pub(crate) fn history_added(&mut self, rows: &[HistoryRow]) {
        if self.commits.is_empty() {
            return;
        }
        let commits = &self.commits;
        for row in rows.iter().filter(|row| commits.contains(&row.oid)) {
            self.ordered.push(row.oid.clone());
            self.merges |= row.parents.len() > 1;
        }
    }

    fn forget(&mut self) {
        self.ticket += 1;
        self.shown = None;
        self.files.clear();
        self.fork = None;
        self.loading = false;
        self.failed = None;
        self.path = None;
        self.opening = None;
        self.scroll = gpui::UniformListScrollHandle::new();
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Pick { rev: Rev, how: How },
    Show { from: Rev, to: Rev, since_fork: bool, names: Option<Names> },
    SinceFork(bool),
    OpenFile(PathBuf),
    AllChanges,
}

pub(crate) fn how(modifiers: gpui::Modifiers) -> Option<How> {
    if modifiers.platform {
        Some(How::Toggle)
    } else if modifiers.shift {
        Some(How::Range)
    } else {
        None
    }
}

pub(crate) fn rows(history: &[HistoryRow]) -> Vec<Rev> {
    std::iter::once(Rev::WorkingTree).chain(history.iter().map(|row| Rev::Commit(row.oid.clone()))).collect()
}

pub(crate) fn picked_commits(ui: &Ui) -> &[String] {
    &ui.compare.ordered
}

pub(crate) fn run_base(history: &[HistoryRow], ordered: &[Rev]) -> Option<String> {
    if ordered.len() < RUN {
        return None;
    }
    let oids: Vec<&str> = ordered.iter().map(Rev::oid).collect::<Option<_>>()?;
    let wanted: HashSet<&str> = oids.iter().copied().collect();
    let found: Vec<&HistoryRow> = history.iter().filter(|row| wanted.contains(row.oid.as_str())).collect();
    if found.len() != oids.len() {
        return None;
    }
    let chained = found.windows(2).all(|pair| pair[0].parents.len() == 1 && pair[0].parents[0] == pair[1].oid);
    let oldest = found.last()?;
    if !chained || oldest.parents.len() > 1 {
        return None;
    }
    Some(oldest.parents.first().cloned().unwrap_or_else(|| empty_tree_like(&oldest.oid)))
}

pub(crate) fn classify(history: &[HistoryRow], ordered: &[Rev]) -> Option<Selection> {
    match ordered {
        [] => None,
        [one] => Some(match one {
            Rev::WorkingTree => Selection::Changes,
            Rev::Commit(oid) => Selection::Commit(oid.clone()),
        }),
        [newer, older] => Some(Selection::Compare { from: older.clone(), to: newer.clone(), since_fork: false }),
        _ => Some(match run_base(history, ordered) {
            Some(base) => Selection::Compare { from: Rev::Commit(base), to: ordered[0].clone(), since_fork: false },
            None => Selection::Commits,
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Heading {
    pub title: String,
    pub notes: Vec<String>,
}

pub(crate) fn heading(ui: &Ui) -> Option<Heading> {
    let state = &ui.compare;
    describe(ui.selection.as_ref()?, state.names.as_ref(), state.count(), state.picks_working_tree())
}

fn describe(selection: &Selection, names: Option<&Names>, commits: usize, with_files: bool) -> Option<Heading> {
    match selection {
        Selection::Commits => {
            let mut notes = vec![NO_COMBINED_CHANGE.to_owned()];
            if with_files {
                notes.push("Your uncommitted changes are picked too.".to_owned());
            }
            Some(Heading { title: format!("{commits} commits selected"), notes })
        }
        Selection::Compare { from, to, since_fork } => {
            let mut notes = Vec::new();
            let title = match names {
                Some(names) if *to == Rev::WorkingTree => format!("Comparing {} with your files", names.theirs),
                Some(names) => {
                    notes.push(if *since_fork {
                        format!("What {} adds since it left {}.", names.theirs, names.ours)
                    } else {
                        format!("Every difference between the tips of {} and {}.", names.theirs, names.ours)
                    });
                    format!("Comparing {} with {}", names.theirs, names.ours)
                }
                None if commits >= RUN && !with_files => {
                    notes.push("One combined change, from the parent of the oldest to the newest.".to_owned());
                    format!("Changes in {commits} commits")
                }
                None => format!("Comparing {} with {}", from.short(), to.short()),
            };
            if *to == Rev::WorkingTree {
                notes.push(UNTRACKED_LEFT_OUT.to_owned());
            }
            Some(Heading { title, notes })
        }
        _ => None,
    }
}

impl Repository {
    pub(crate) fn compared_ends(&self, from: &Rev, to: &Rev, since_fork: bool) -> Result<(String, Option<String>), String> {
        let Rev::Commit(from) = from else {
            return Err("A comparison starts from a commit.".into());
        };
        validate_oid(from)?;
        let to = match to {
            Rev::Commit(oid) => {
                validate_oid(oid)?;
                Some(oid.clone())
            }
            Rev::WorkingTree => None,
        };
        if !since_fork {
            return Ok((from.clone(), to));
        }
        let Some(tip) = &to else {
            return Err("Compare your files with a commit, not with where they diverged.".into());
        };
        let base = checked(self.read_command().args(["merge-base", from.as_str(), tip.as_str()]))
            .map_err(|_| "These two have no commit in common. Compare the tips instead.".to_owned())?;
        let base = String::from_utf8_lossy(&base).trim().to_owned();
        validate_oid(&base).map_err(|_| "Git did not name a common commit. Compare the tips instead.".to_owned())?;
        Ok((base, to))
    }

    pub(crate) fn compared_files(
        &self,
        from: &Rev,
        to: &Rev,
        since_fork: bool,
    ) -> Result<(String, Vec<ChangedFile>), String> {
        let (from, to) = self.compared_ends(from, to, since_fork)?;
        let output = checked(
            self.read_command()
                .args([
                    "diff",
                    "--name-status",
                    "--find-renames",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-relative",
                    "-z",
                ])
                .arg(&from)
                .args(to.as_ref())
                .arg("--"),
        )?;
        Ok((from, parse_changed_files(&output)?))
    }

    pub(crate) fn compared_patch(
        &self,
        ends: (&Rev, &Rev, bool),
        path: Option<&Path>,
        old: Option<&Path>,
        options: DiffOptions,
    ) -> Result<Diff, String> {
        let (from, to, since_fork) = ends;
        for path in path.iter().chain(old.iter()) {
            validate_path(path)?;
        }
        let (from, to) = self.compared_ends(from, to, since_fork)?;
        let mut command = self.read_command();
        command
            .arg("diff")
            .args(patch_options(true, options))
            .arg(&from)
            .args(to.as_ref())
            .arg("--")
            .args(old)
            .args(path);
        Ok(read_diff(command, None, false)?.shown_with(options))
    }
}

fn select(tab: &mut Tab, selection: Selection, window: &mut Window, cx: &mut Context<Tab>) {
    view::focus(&tab.ui, window);
    tab.dispatch(view::Action::Select(selection), window, cx);
}

fn primary_of(ui: &Ui) -> Option<Rev> {
    match ui.selection.as_ref()? {
        Selection::Commit(oid) => Some(Rev::Commit(oid.clone())),
        Selection::Changes | Selection::File { .. } => Some(Rev::WorkingTree),
        Selection::Compare { .. } | Selection::Commits => ui.compare.primary.clone(),
        Selection::Stash(_) => None,
    }
}

fn apply(tab: &mut Tab, picked: Picked<Rev>, primary: Rev, window: &mut Window, cx: &mut Context<Tab>) {
    let ordered = picked_order(&tab.ui.history, &picked);
    let Some(selection) = classify(&tab.ui.history, &ordered) else { return };
    let several = ordered.len() > 1;
    if !several {
        select(tab, selection, window, cx);
        return;
    }
    tab.ui.compare.names = None;
    if tab.ui.selection.as_ref() != Some(&selection) {
        select(tab, selection.clone(), window, cx);
    }
    if tab.ui.selection.as_ref() == Some(&selection) {
        tab.ui.compare.set_pick(Some(picked), Some(primary), &tab.ui.history);
    }
    cx.notify();
}

fn picked_order(history: &[HistoryRow], picked: &Picked<Rev>) -> Vec<Rev> {
    let mut ordered = Vec::with_capacity(picked.len());
    if picked.contains(&Rev::WorkingTree) {
        ordered.push(Rev::WorkingTree);
    }
    let wanted: HashSet<&str> = picked.keys.iter().filter_map(Rev::oid).collect();
    ordered.extend(history.iter().filter(|row| wanted.contains(row.oid.as_str())).map(|row| Rev::Commit(row.oid.clone())));
    ordered
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Pick { rev, how } => {
            let listed = rows(&tab.ui.history);
            let primary = primary_of(&tab.ui);
            let current = tab.ui.compare.picked.clone().filter(|_| {
                matches!(tab.ui.selection, Some(Selection::Compare { .. } | Selection::Commits))
                    && tab.ui.compare.names.is_none()
            });
            if let Some((picked, primary)) =
                Picked::clicked(current.as_ref(), primary.as_ref(), ListId::History, &listed, &rev, how)
            {
                apply(tab, picked, primary, window, cx);
            }
        }
        Action::Show { from, to, since_fork, names } => {
            tab.ui.compare.clear_pick();
            let selection = Selection::Compare { from, to, since_fork };
            select(tab, selection.clone(), window, cx);
            if tab.ui.selection.as_ref() == Some(&selection) {
                tab.ui.compare.names = names;
            }
            cx.notify();
        }
        Action::SinceFork(on) => {
            let Some(Selection::Compare { from, to, since_fork }) = tab.ui.selection.clone() else { return };
            if since_fork == on || tab.ui.compare.names.is_none() || to == Rev::WorkingTree {
                return;
            }
            let names = tab.ui.compare.names.take();
            dispatch(tab, Action::Show { from, to, since_fork: on, names }, window, cx);
        }
        Action::OpenFile(path) => open_file(tab, path, cx),
        Action::AllChanges => {
            let Some(selection @ Selection::Compare { .. }) = tab.ui.selection.clone() else { return };
            tab.ui.compare.path = None;
            tab.ui.diff_open = true;
            tab.load_selected_patch(selection, None, cx);
        }
    }
}

fn open_file(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(selection @ Selection::Compare { .. }) = tab.ui.selection.clone() else { return };
    if !tab.ui.compare.files.iter().any(|file| file.path == path) {
        return;
    }
    tab.ui.compare.path = Some(path.clone());
    tab.ui.diff_open = true;
    tab.load_selected_patch(selection, Some(path), cx);
}

pub(crate) fn open_when_loaded(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    if tab.ui.compare.loading {
        tab.ui.compare.opening = Some(path);
    } else {
        open_file(tab, path, cx);
    }
}

pub(crate) fn selected(tab: &mut Tab, cx: &mut Context<Tab>) {
    let selection = tab.ui.selection.clone();
    let keeps = matches!(selection, Some(Selection::Compare { .. } | Selection::Commits));
    if !keeps {
        tab.ui.compare.clear_pick();
        tab.ui.compare.names = None;
    }
    if tab.ui.compare.shown.is_some() && tab.ui.compare.shown != selection {
        tab.ui.compare.forget();
    }
    if let Some(Selection::Compare { .. }) = &selection {
        if tab.ui.compare.shown.is_none() {
            load(tab, cx);
        }
    }
}

fn load(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(Selection::Compare { from, to, since_fork }), Some(repo)) = (tab.ui.selection.clone(), tab.repo.clone()) else {
        return;
    };
    let state = &mut tab.ui.compare;
    state.ticket += 1;
    state.loading = true;
    state.failed = None;
    state.shown = tab.ui.selection.clone();
    let (ticket, epoch) = (state.ticket, tab.epoch);
    Tab::work(
        cx,
        move || repo.compared_files(&from, &to, since_fork),
        move |tab, result, cx| {
            let state = &mut tab.ui.compare;
            if tab.epoch != epoch || state.ticket != ticket {
                return;
            }
            state.loading = false;
            let opening = state.opening.take();
            match result {
                Ok((start, files)) => {
                    state.fork = since_fork.then_some(start);
                    let paths = files.iter().map(|file| file.path.clone()).collect();
                    state.files.set(files);
                    if state.path.as_ref().is_some_and(|path| !state.files.iter().any(|file| &file.path == path)) {
                        state.path = None;
                        tab.ui.diff_open = false;
                    }
                    if let Some(source) = crate::lfs::compared_source(&tab.ui) {
                        crate::lfs::listed(tab, source, paths, cx);
                    }
                    if let Some(path) = opening {
                        open_file(tab, path, cx);
                    }
                }
                Err(error) => {
                    state.files.clear();
                    state.fork = None;
                    state.failed = Some(error);
                }
            }
            cx.notify();
        },
    );
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(selection @ Selection::Compare { to: Rev::WorkingTree, .. }) = tab.ui.selection.clone() else { return };
    load(tab, cx);
    if tab.ui.diff_open && !tab.ui.blame.open() {
        let path = tab.ui.compare.path.clone();
        tab.load_selected_patch(selection, path, cx);
    }
}

pub(crate) fn history_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(mut picked) = tab.ui.compare.picked.clone() else { return };
    let listed = rows(&tab.ui.history);
    let before = picked.len();
    if !picked.retain(&listed) {
        tab.ui.compare.clear_pick();
        if tab.ui.selection == Some(Selection::Commits) {
            let landing = tab.ui.history.first().map_or(Selection::Changes, |row| Selection::Commit(row.oid.clone()));
            tab.select(landing, cx);
        }
        return;
    }
    if picked.len() == before {
        return;
    }
    let ordered = picked_order(&tab.ui.history, &picked);
    let Some(selection) = classify(&tab.ui.history, &ordered) else { return };
    let primary = tab.ui.compare.primary.clone().filter(|rev| picked.contains(rev)).unwrap_or_else(|| picked.anchor.clone());
    tab.select(selection, cx);
    if ordered.len() > 1 {
        tab.ui.compare.set_pick(Some(picked), Some(primary), &tab.ui.history);
    }
}

pub(crate) fn context_target(ui: &Ui, oid: &str) -> Option<crate::commands::Target> {
    (ui.compare.picks(oid) && ui.compare.count() > 1).then(|| crate::commands::Target::Commits(picked_commits(ui).to_vec()))
}

pub(crate) fn commits_of(target: &crate::commands::Target) -> Option<&[String]> {
    match target {
        crate::commands::Target::Commits(oids) if oids.len() > 1 => Some(oids),
        _ => None,
    }
}

pub(crate) fn copy_shas(oids: &[String]) -> view::Action {
    view::Action::CopyText { text: oids.join("\n"), what: "SHAs" }
}

fn current_name(ui: &Ui) -> String {
    ui.checked_out_branch().map_or_else(|| "HEAD".to_owned(), str::to_owned)
}

fn ref_tip(ui: &Ui, target: &crate::commands::Target) -> Option<(String, String)> {
    use crate::commands::Target;
    let (kind, name) = match target {
        Target::Branch(name) => (crate::refs::Kind::Branch, name),
        Target::RemoteBranch(name) => (crate::refs::Kind::Remote, name),
        Target::Tag(name) => (crate::refs::Kind::Tag, name),
        _ => return None,
    };
    let item = ui.refs.items.iter().find(|item| item.kind == kind && &item.name == name)?;
    Some((item.name.to_string_lossy().into_owned(), item.oid.clone()?))
}

fn comparable_ref(ui: &Ui, target: &crate::commands::Target) -> Option<(String, String, String)> {
    let head = ui.head.clone()?;
    if let crate::commands::Target::Branch(name) = target {
        if !ui.detached && name.to_string_lossy() == ui.branch {
            return None;
        }
    }
    let (name, oid) = ref_tip(ui, target)?;
    Some((name, oid, head))
}

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target};
    &[
        Command::new(
            "commit.compare-working",
            Group::View,
            |_, _| "Compare with my files".into(),
            |ui, target, _| match target {
                Target::Commit(_) if !ui.repo.is_empty() => Availability::Enabled,
                _ => Availability::Hidden,
            },
            |_, target| match target {
                Target::Commit(oid) => Some(view::Action::Compare(Action::Show {
                    from: Rev::Commit(oid.clone()),
                    to: Rev::WorkingTree,
                    since_fork: false,
                    names: None,
                })),
                _ => None,
            },
        )
        .words(&["diff", "working tree", "working directory", "changes since"])
        .icon("search"),
        Command::new(
            "ref.compare-current",
            Group::View,
            |ui, _| format!("Compare with {}", current_name(ui)),
            |ui, target, _| {
                if comparable_ref(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |ui, target| {
                let (theirs, oid, head) = comparable_ref(ui, target)?;
                Some(view::Action::Compare(Action::Show {
                    from: Rev::Commit(head),
                    to: Rev::Commit(oid),
                    since_fork: true,
                    names: Some(Names { ours: current_name(ui), theirs }),
                }))
            },
        )
        .words(&["diff", "branches", "since diverged", "merge base"])
        .icon("search"),
        Command::new(
            "ref.compare-working",
            Group::View,
            |_, _| "Compare with my files".into(),
            |ui, target, _| {
                if !ui.repo.is_empty() && ref_tip(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |ui, target| {
                let (theirs, oid) = ref_tip(ui, target)?;
                Some(view::Action::Compare(Action::Show {
                    from: Rev::Commit(oid),
                    to: Rev::WorkingTree,
                    since_fork: false,
                    names: Some(Names { ours: "your files".to_owned(), theirs }),
                }))
            },
        )
        .words(&["diff", "working tree", "working directory", "changes since"])
        .icon("search"),
        Command::new(
            "commits.copy-shas",
            Group::Copy,
            |_, target| format!("Copy {} SHAs", commits_of(target).map_or(0, <[String]>::len)),
            |_, target, _| if commits_of(target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |_, target| commits_of(target).map(copy_shas),
        )
        .words(&["hash", "ids", "clipboard", "several"])
        .icon("copy"),
    ]
};

fn side_line(ui: &Ui, label: &'static str, rev: &Rev) -> Option<Div> {
    let Rev::Commit(oid) = rev else { return None };
    if empty_tree(oid) {
        return None;
    }
    let subject = ui.history.iter().find(|row| &row.oid == oid).map(|row| row.subject.clone()).unwrap_or_default();
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(kit::fit(label).size(TEXT_SMALL).color(palette().text_3))
            .child(kit::fit(rev.short()).mono().size(11.5).color(palette().text_2))
            .child(kit::fit(subject).size(TEXT_SMALL).color(palette().text_2).grow()),
    )
}

fn fork_switch<T: Desk>(since_fork: bool, cx: &mut Context<T>) -> Div {
    div()
        .flex()
        .gap(px(4.))
        .child(
            kit::btn("compare-since-fork", "Since they diverged")
                .small()
                .ghost()
                .selected(since_fork)
                .on(view::Action::Compare(Action::SinceFork(true)), cx),
        )
        .child(
            kit::btn("compare-tips", "Tip to tip")
                .small()
                .ghost()
                .selected(!since_fork)
                .on(view::Action::Compare(Action::SinceFork(false)), cx),
        )
}

const REWRITE_BUTTONS: [(&str, &str, &str, &str); 2] =
    [("commits.squash", "squash-picked", "Squash…", "merge"), ("commits.drop", "drop-picked", "Drop…", "trash")];

pub(crate) fn panel<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(heading) = heading(ui) else { return div() };
    let state = &ui.compare;
    let comparing = match &ui.selection {
        Some(Selection::Compare { from, to, since_fork }) => Some((from.clone(), to.clone(), *since_fork)),
        _ => None,
    };
    let summary = div()
        .p_3()
        .flex()
        .flex_col()
        .gap(px(7.))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(
            div()
                .id("compare-title")
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(heading.title.clone()),
        )
        .when_some(comparing.clone(), |d, (from, to, since_fork)| {
            let start = if since_fork { state.fork.clone().map(Rev::Commit) } else { Some(from) };
            d.children(start.and_then(|start| side_line(ui, "from", &start))).children(side_line(ui, "to", &to))
        })
        .children(heading.notes.iter().map(|note| kit::muted(note.clone())))
        .when(state.names.is_some() && comparing.as_ref().is_some_and(|(_, to, _)| *to != Rev::WorkingTree), |d| {
            d.child(fork_switch(comparing.as_ref().is_some_and(|(_, _, since)| *since), cx))
        })
        .when(state.count() > 1, |d| {
            let oids = picked_commits(ui);
            let merges = state.merges();
            let pick = crate::pick::offered(ui, oids, cx);
            d.child(
                div()
                    .pt(px(2.))
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .when_some(pick, |d, entry| {
                        let button = kit::btn("cherry-pick-picked", "Cherry-pick…")
                            .small()
                            .icon("commit")
                            .enabled(entry.disabled.is_none())
                            .tip(entry.disabled.clone().unwrap_or(entry.title));
                        d.child(button.on(entry.action.unwrap_or(view::Action::Dismiss), cx))
                    })
                    .children(REWRITE_BUTTONS.into_iter().filter_map(|(command, id, label, icon)| {
                        crate::rewrite::offered(ui, command, oids, cx).map(|entry| {
                            kit::btn(id, label)
                                .small()
                                .icon(icon)
                                .enabled(entry.disabled.is_none())
                                .tip(entry.disabled.clone().unwrap_or(entry.title))
                                .on(entry.action.unwrap_or(view::Action::Dismiss), cx)
                        })
                    }))
                    .child(
                        kit::btn("copy-picked-shas", format!("Copy {} SHAs", oids.len()))
                            .small()
                            .icon("copy")
                            .on(copy_shas(oids), cx),
                    )
                    .when(!merges, |d| {
                        d.child(
                            kit::btn("save-picked-patch", "Save as patch…")
                                .small()
                                .icon("file")
                                .on(crate::patch::save_commits(oids), cx),
                        )
                    }),
            )
        });
    let body = div().flex_1().min_h_0().flex().flex_col().child(summary);
    if comparing.is_none() {
        return body;
    }
    let count = state.files.len();
    let label = if state.loading {
        "Comparing…".to_owned()
    } else if let Some(failed) = &state.failed {
        failed.clone()
    } else if count == 0 {
        "No differences".to_owned()
    } else if count == 1 {
        "1 file differs".to_owned()
    } else {
        format!("{count} files differ")
    };
    let header = div()
        .h(px(28.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(kit::label(label))
        .child(kit::grow())
        .when(count > 1 && !state.loading, |d| {
            d.child(
                kit::btn("show-compared-files", "View all")
                    .small()
                    .ghost()
                    .on(view::Action::Compare(Action::AllChanges), cx),
            )
        })
        .when(count > 0 && !state.loading, |d| d.child(file_list::tools(ui, file_list::Panel::Compare, cx)));
    let rows = file_list::rows(ui, Which::Compare);
    let files = uniform_list(
        SharedString::from("compared-files"),
        rows.len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let state = &ui.compare;
            let mode = ui.file_list.mode;
            let side = Which::Compare.side(ui);
            let lfs_source = crate::lfs::compared_source(ui);
            range
                .filter_map(|at| {
                    let (i, depth) = match rows.get(at)? {
                        Row::File { index, depth } => (*index, *depth),
                        row => return Some(file_list::folder_row(("compared-folder", at), Which::Compare, row, side.clone(), None, cx)),
                    };
                    let file = state.files.get(i)?;
                    let path = file.path.clone();
                    let open = ui.diff_open && state.path.as_ref() == Some(&file.path);
                    let target = side.clone().map(|side| crate::commands::Target::File { path: file.path.clone(), side });
                    Some(
                        kit::row(("compared-file", i), open)
                            .h(px(FILE_ROW))
                            .pl(px(file_list::indent(mode, depth)))
                            .pr_3()
                            .gap_2()
                            .child(file_list::label(("compared-file-label", i), file.status, &file.path, file.from.as_deref(), mode))
                            .when(lfs_source.as_ref().is_some_and(|source| ui.lfs.marked(source, &file.path)), |d| d.child(crate::lfs::badge()))
                            .on_click(cx.listener(move |app, _, w, cx| {
                                app.dispatch(view::Action::Compare(Action::OpenFile(path.clone())), w, cx)
                            }))
                            .when_some(target, |row, target| {
                                row.on_mouse_down(
                                    gpui::MouseButton::Right,
                                    cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                                        let target = target.clone();
                                        app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
                                    }),
                                )
                            })
                            .into_any_element(),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(state.scroll.clone())
    .flex_1()
    .min_h_0();
    body.child(header)
        .children(file_list::filter_field(ui, file_list::Panel::Compare, cx))
        .children(file_list::nothing_matches(ui, file_list::Panel::Compare))
        .child(files)
        .child(kit::scrollbar("compared-files-scrollbar", &state.scroll))
}

pub(crate) fn menu_for(ui: &Ui, oid: &str, at: gpui::Point<gpui::Pixels>) -> Option<view::Action> {
    let target = context_target(ui, oid)?;
    Some(view::Action::Menu(Some(Menu::Object { target, at })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn row(oid: &str, parents: &[&str]) -> HistoryRow {
        HistoryRow {
            oid: oid.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            subject: String::new(),
            author: String::new(),
            email: String::new(),
            date: String::new(),
            time: 0,
            refs: String::new(),
            body: String::new(),
        }
    }

    fn id(n: u8) -> String {
        format!("{n:02x}").repeat(20)
    }

    fn commit(n: u8) -> Rev {
        Rev::Commit(id(n))
    }

    fn line_with_a_merge() -> Vec<HistoryRow> {
        let (a, b, c, d, e, f) = (id(1), id(2), id(3), id(4), id(5), id(6));
        vec![
            row(&f, &[&e]),
            row(&e, &[&d, &c]),
            row(&d, &[&b]),
            row(&c, &[&b]),
            row(&b, &[&a]),
            row(&a, &[]),
        ]
    }

    fn pick(history: &[HistoryRow], clicks: &[(Rev, How)]) -> (Picked<Rev>, Vec<Rev>) {
        let listed = rows(history);
        let mut current: Option<Picked<Rev>> = None;
        let mut primary = clicks[0].0.clone();
        for (rev, how) in clicks {
            let (next, chosen) = Picked::clicked(current.as_ref(), Some(&primary), ListId::History, &listed, rev, *how).unwrap();
            current = Some(next);
            primary = chosen;
        }
        let picked = current.unwrap();
        let ordered = picked_order(history, &picked);
        (picked, ordered)
    }

    #[test]
    fn two_commits_picked_bottom_up_compare_the_same_way_as_top_down() {
        let history = line_with_a_merge();
        let (_, down) = pick(&history, &[(commit(6), How::Toggle), (commit(2), How::Toggle)]);
        let (_, up) = pick(&history, &[(commit(2), How::Toggle), (commit(6), How::Toggle)]);
        let expected = Selection::Compare { from: commit(2), to: commit(6), since_fork: false };
        assert_eq!(classify(&history, &down), Some(expected.clone()));
        assert_eq!(classify(&history, &up), Some(expected));
        let (_, with_files) = pick(&history, &[(commit(4), How::Toggle), (Rev::WorkingTree, How::Toggle)]);
        assert_eq!(
            classify(&history, &with_files),
            Some(Selection::Compare { from: commit(4), to: Rev::WorkingTree, since_fork: false })
        );
    }

    #[test]
    fn a_run_of_commits_compares_the_oldest_ones_parent_with_the_newest() {
        let history = line_with_a_merge();
        let (_, run) = pick(&history, &[(commit(2), How::Toggle), (commit(4), How::Range)]);
        assert_eq!(run, vec![commit(4), commit(3), commit(2)]);
        assert_eq!(classify(&history, &run), Some(Selection::Commits));
        let (_, chained) = pick(&history, &[(commit(4), How::Toggle), (commit(2), How::Toggle), (commit(1), How::Toggle)]);
        assert_eq!(
            classify(&history, &chained),
            Some(Selection::Compare { from: Rev::Commit(EMPTY_TREE.to_owned()), to: commit(4), since_fork: false })
        );
        let short = vec![row(&id(9), &[&id(8)]), row(&id(8), &[&id(7)]), row(&id(7), &[&id(6)])];
        let (_, top) = pick(&short, &[(commit(9), How::Toggle), (commit(7), How::Range)]);
        assert_eq!(classify(&short, &top), Some(Selection::Compare { from: commit(6), to: commit(9), since_fork: false }));
    }

    #[test]
    fn a_range_through_a_merge_is_not_treated_as_consecutive() {
        let history = line_with_a_merge();
        let (_, through) = pick(&history, &[(commit(6), How::Toggle), (commit(4), How::Range)]);
        assert_eq!(through, vec![commit(6), commit(5), commit(4)]);
        assert_eq!(classify(&history, &through), Some(Selection::Commits));
        let (_, from_merge) = pick(&history, &[(commit(6), How::Toggle), (commit(5), How::Toggle), (commit(4), How::Toggle)]);
        assert_eq!(classify(&history, &from_merge), Some(Selection::Commits));
        let ending_on_merge = vec![row(&id(9), &[&id(8)]), row(&id(8), &[&id(5)]), row(&id(5), &[&id(4), &id(3)])];
        let (_, last) = pick(&ending_on_merge, &[(commit(9), How::Toggle), (commit(5), How::Range)]);
        assert_eq!(classify(&ending_on_merge, &last), Some(Selection::Commits));
        let (_, with_files) = pick(&history, &[(Rev::WorkingTree, How::Toggle), (commit(5), How::Range)]);
        assert_eq!(with_files, vec![Rev::WorkingTree, commit(6), commit(5)]);
        assert_eq!(classify(&history, &with_files), Some(Selection::Commits));
    }

    #[test]
    fn removing_picks_back_down_to_one_row_selects_it_alone() {
        let history = line_with_a_merge();
        let (picked, _) = pick(&history, &[(commit(6), How::Toggle), (commit(4), How::Toggle), (commit(4), How::Toggle)]);
        let ordered = picked_order(&history, &picked);
        assert_eq!(classify(&history, &ordered), Some(Selection::Commit(id(6))));
        let (picked, _) = pick(&history, &[(Rev::WorkingTree, How::Toggle), (commit(4), How::Toggle), (commit(4), How::Toggle)]);
        assert_eq!(classify(&history, &picked_order(&history, &picked)), Some(Selection::Changes));
    }

    fn walked(history: &[HistoryRow], state: &State) -> (Vec<String>, bool) {
        let oids: Vec<String> = history.iter().filter(|row| state.picks(&row.oid)).map(|row| row.oid.clone()).collect();
        let merges = oids.iter().any(|oid| history.iter().any(|row| &row.oid == oid && row.parents.len() > 1));
        (oids, merges)
    }

    fn reload(state: &mut State, history: &[HistoryRow]) {
        state.history_cleared();
        state.history_added(history);
    }

    #[test]
    fn the_ordered_pick_follows_new_picks_reloaded_history_and_more_pages() {
        let history = line_with_a_merge();
        let mut state = State::default();
        let (picked, _) = pick(&history, &[(commit(2), How::Toggle), (commit(5), How::Toggle), (commit(6), How::Toggle)]);
        state.set_pick(Some(picked), Some(commit(6)), &history);
        assert_eq!((state.ordered.clone(), state.merges()), (vec![id(6), id(5), id(2)], true));
        assert_eq!((state.ordered.clone(), state.merges()), walked(&history, &state));

        let without_merge: Vec<HistoryRow> = history.iter().filter(|row| row.oid != id(5)).cloned().collect();
        reload(&mut state, &without_merge);
        assert_eq!((state.ordered.clone(), state.merges()), (vec![id(6), id(2)], false));
        assert_eq!((state.ordered.clone(), state.merges()), walked(&without_merge, &state));

        let mut reordered = history.clone();
        reordered.swap(0, 4);
        reload(&mut state, &reordered);
        assert_eq!(state.ordered, vec![id(2), id(5), id(6)]);
        assert_eq!((state.ordered.clone(), state.merges()), walked(&reordered, &state));

        reload(&mut state, &history[..2]);
        assert_eq!((state.ordered.clone(), state.merges()), (vec![id(6), id(5)], true));
        state.history_added(&history[2..]);
        assert_eq!((state.ordered.clone(), state.merges()), (vec![id(6), id(5), id(2)], true));
        assert_eq!((state.ordered.clone(), state.merges()), walked(&history, &state));

        let (fewer, _) = pick(&history, &[(commit(4), How::Toggle), (commit(3), How::Toggle)]);
        state.set_pick(Some(fewer), Some(commit(4)), &history);
        assert_eq!((state.ordered.clone(), state.merges()), (vec![id(4), id(3)], false));
        state.clear_pick();
        assert_eq!((state.ordered.clone(), state.merges()), (Vec::new(), false));
        reload(&mut state, &history);
        assert!(state.ordered.is_empty());
    }

    #[test]
    fn the_panel_heading_names_what_is_compared_and_what_is_left_out() {
        let (older, newer) = (commit(2), commit(6));
        let pair = Selection::Compare { from: older.clone(), to: newer.clone(), since_fork: false };
        let read = describe(&pair, None, 2, false).unwrap();
        assert_eq!(read.title, "Comparing 0202020 with 0606060");
        assert!(read.notes.is_empty());
        let mine = Selection::Compare { from: older.clone(), to: Rev::WorkingTree, since_fork: false };
        let read = describe(&mine, None, 1, true).unwrap();
        assert_eq!(read.title, "Comparing 0202020 with your files");
        assert_eq!(read.notes, [UNTRACKED_LEFT_OUT]);
        let run = describe(&pair, None, 3, false).unwrap();
        assert_eq!(run.title, "Changes in 3 commits");
        let names = Names { ours: "main".into(), theirs: "ridge".into() };
        let forked = Selection::Compare { from: older, to: newer, since_fork: true };
        let read = describe(&forked, Some(&names), 0, false).unwrap();
        assert_eq!(read.title, "Comparing ridge with main");
        assert_eq!(read.notes, ["What ridge adds since it left main."]);
        let desk = Names { ours: "your files".into(), theirs: "ridge".into() };
        let read = describe(&mine, Some(&desk), 0, false).unwrap();
        assert_eq!(read.title, "Comparing ridge with your files");
        assert_eq!(read.notes, [UNTRACKED_LEFT_OUT]);
        let several = describe(&Selection::Commits, None, 4, true).unwrap();
        assert_eq!(several.title, "4 commits selected");
        assert_eq!(several.notes, [NO_COMBINED_CHANGE, "Your uncommitted changes are picked too."]);
        assert_eq!(describe(&Selection::Changes, None, 0, false), None);
    }

    fn commit_file(f: &Fixture, path: &str, text: &str, message: &str) -> String {
        fs::write(f.dir.join(path), text).unwrap();
        f.git(&["add", "--", path]);
        f.git(&["commit", "-qm", message]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn listed(files: &[ChangedFile]) -> Vec<(String, &'static str)> {
        files.iter().map(|file| (file.path.to_string_lossy().into_owned(), file.status)).collect()
    }

    fn untouched(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (f.git(&["for-each-ref"]), f.git(&["ls-files", "-s"]), f.git(&["status", "--porcelain=v1", "-z"]))
    }

    #[test]
    fn two_commits_list_the_files_git_finds_between_them_and_change_nothing() {
        let f = Fixture::new();
        let first = commit_file(&f, "camp.txt", "camp\n", "Pitch the camp");
        commit_file(&f, "trail.txt", "trail\n", "Mark the trail");
        fs::rename(f.dir.join("camp.txt"), f.dir.join("base.txt")).unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "Rename the camp"]);
        let last = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let before = untouched(&f);
        let files = f.repo.compared_files(&Rev::Commit(first.clone()), &Rev::Commit(last.clone()), false).unwrap().1;
        assert_eq!(listed(&files), vec![("base.txt".to_owned(), "Renamed"), ("trail.txt".to_owned(), "Added")]);
        assert_eq!(files[0].from.as_deref(), Some(Path::new("camp.txt")));
        let ends = (&Rev::Commit(first), &Rev::Commit(last), false);
        let patch = f.repo.compared_patch(ends, Some(Path::new("trail.txt")), None, DiffOptions::default()).unwrap();
        assert!(patch.text.contains("+trail"), "{}", patch.text);
        assert!(!patch.stageable());
        assert_eq!(untouched(&f), before);
        assert!(f.repo.compared_files(&Rev::WorkingTree, &Rev::Commit(id(1)), false).is_err());
        assert!(f.repo.compared_files(&Rev::Commit("HEAD".into()), &Rev::WorkingTree, false).is_err());
    }

    #[test]
    fn a_comparison_with_the_working_tree_lists_edits_and_leaves_untracked_files_out() {
        let f = Fixture::new();
        let base = commit_file(&f, "camp.txt", "camp\n", "Pitch the camp");
        commit_file(&f, "trail.txt", "trail\n", "Mark the trail");
        fs::write(f.dir.join("camp.txt"), "camp by the lake\n").unwrap();
        fs::write(f.dir.join("loose.txt"), "not tracked\n").unwrap();
        let before = untouched(&f);
        let files = f.repo.compared_files(&Rev::Commit(base.clone()), &Rev::WorkingTree, false).unwrap().1;
        assert_eq!(listed(&files), vec![("camp.txt".to_owned(), "Modified"), ("trail.txt".to_owned(), "Added")]);
        let ends = (&Rev::Commit(base), &Rev::WorkingTree, false);
        let patch = f.repo.compared_patch(ends, Some(Path::new("camp.txt")), None, DiffOptions::default()).unwrap();
        assert!(patch.text.contains("+camp by the lake"), "{}", patch.text);
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn since_they_diverged_lists_only_what_the_branch_added() {
        let f = Fixture::new();
        let fork = commit_file(&f, "camp.txt", "camp\n", "Pitch the camp");
        f.git(&["switch", "-qc", "ridge"]);
        let ridge = commit_file(&f, "ridge.txt", "ridge\n", "Survey the ridge");
        f.git(&["switch", "-q", "main"]);
        let main = commit_file(&f, "lake.txt", "lake\n", "Map the lake");
        let before = untouched(&f);
        let (start, since) =
            f.repo.compared_files(&Rev::Commit(main.clone()), &Rev::Commit(ridge.clone()), true).unwrap();
        assert_eq!(start, fork);
        assert_eq!(listed(&since), vec![("ridge.txt".to_owned(), "Added")]);
        let tips = f.repo.compared_files(&Rev::Commit(main.clone()), &Rev::Commit(ridge.clone()), false).unwrap().1;
        assert_eq!(listed(&tips), vec![("lake.txt".to_owned(), "Deleted"), ("ridge.txt".to_owned(), "Added")]);
        assert_eq!(untouched(&f), before);
        f.git(&["switch", "-q", "--orphan", "island"]);
        let island = commit_file(&f, "island.txt", "island\n", "Land on the island");
        let unrelated = f.repo.compared_files(&Rev::Commit(main), &Rev::Commit(island), true).unwrap_err();
        assert!(unrelated.contains("no commit in common"), "{unrelated}");
    }

    #[test]
    fn a_run_from_the_first_commit_compares_with_the_empty_tree() {
        let f = Fixture::new();
        commit_file(&f, "camp.txt", "camp\n", "Pitch the camp");
        let tip = commit_file(&f, "trail.txt", "trail\n", "Mark the trail");
        let files = f.repo.compared_files(&Rev::Commit(EMPTY_TREE.to_owned()), &Rev::Commit(tip), false).unwrap().1;
        assert_eq!(listed(&files), vec![("camp.txt".to_owned(), "Added"), ("trail.txt".to_owned(), "Added")]);
    }
}
