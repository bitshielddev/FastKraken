use crate::{
    Tab, jobs, kit,
    browse::Kind,
    commands::{Availability, Command, Group, busy, networking, on_repo, operating},
    compare::Rev,
    notice::{Failure, Outcome},
    preferences::{self as settings_page, Section},
    repository::{Repository, Run, checked, git_error, run_with_input, validate_path},
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use gpui::{Context, Div, UniformListScrollHandle, div, prelude::*, px, rgb, uniform_list};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

pub(crate) const STASH_MESSAGE: &str = "FastKraken: before applying sparse checkout";
pub(crate) const CHANGED: &str = "The sparse checkout was changed outside FastKraken. Look at the folders again before applying.";
pub(crate) const NO_COMMITS: &str = "There are no commits yet to choose folders from.";
pub(crate) const COMPLETE: &str = "This repository already has its full history.";
pub(crate) const NO_REMOTE: &str = "This repository has no remote to fetch its history from";
pub(crate) const SWITCH_TITLE: &str = "Check out only some folders";
const SWITCH_HELP: &str = "Top-level files are always checked out. Folders left unticked stay in the repository and its history, but not in this working copy.";
const LEFT: &str = "were left despite sparse patterns";
const TREE_ROW: f32 = 26.;
const TREE_ROWS_SHOWN: usize = 11;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Cone(Vec<PathBuf>),
    Patterns(Vec<String>),
}

impl Mode {
    pub(crate) fn on(&self) -> bool {
        !matches!(self, Mode::Off)
    }

    pub(crate) fn folders(&self) -> &[PathBuf] {
        match self {
            Mode::Cone(folders) => folders,
            _ => &[],
        }
    }

    pub(crate) fn summary(&self) -> Option<String> {
        match self {
            Mode::Off => None,
            Mode::Cone(folders) if folders.is_empty() => Some("Sparse: top-level files only".to_owned()),
            Mode::Cone(folders) => Some(format!("Sparse: {}", counted(folders.len(), "folder"))),
            Mode::Patterns(patterns) => Some(format!("Sparse: {}", counted(patterns.len(), "pattern"))),
        }
    }
}

fn counted(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    pub(crate) mode: Mode,
    pub(crate) shallow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tick {
    Off,
    On,
    Within,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Folder { path: PathBuf, name: String, depth: usize, unfolded: bool, tick: Tick, inside: usize },
    Note { depth: usize, text: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) folders: Vec<PathBuf>,
    pub(crate) expected: Mode,
    pub(crate) stash: bool,
    pub(crate) reapply: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Enable,
    Apply,
    Reapply,
    Disable,
    Retry(Change),
    Tick(PathBuf),
    Unfold(PathBuf),
    Revert,
    FetchHistory,
}

impl Action {
    pub(crate) fn during_operation(&self) -> bool {
        matches!(self, Action::Tick(_) | Action::Unfold(_) | Action::Revert | Action::FetchHistory)
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) mode: Option<Mode>,
    pub(crate) shallow: bool,
    pub(crate) problem: Option<String>,
    ticket: u64,
    tree: HashMap<PathBuf, Result<Vec<PathBuf>, String>>,
    tree_at: Option<String>,
    unfolded: BTreeSet<PathBuf>,
    picked: Option<BTreeSet<PathBuf>>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
}

impl State {
    pub(crate) fn on(&self) -> bool {
        self.mode.as_ref().is_some_and(Mode::on)
    }

    pub(crate) fn summary(&self) -> Option<String> {
        self.mode.as_ref().and_then(Mode::summary)
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn chosen(&self) -> BTreeSet<PathBuf> {
        match &self.picked {
            Some(picked) => picked.clone(),
            None => self.mode.as_ref().map(|mode| mode.folders().iter().cloned().collect()).unwrap_or_default(),
        }
    }

    pub(crate) fn edited(&self) -> bool {
        self.picked.is_some()
    }

    fn current(&self) -> BTreeSet<PathBuf> {
        self.mode.as_ref().map(|mode| mode.folders().iter().cloned().collect()).unwrap_or_default()
    }

    pub(crate) fn tick(&mut self, path: &Path) {
        let mut chosen = self.chosen();
        if !chosen.remove(path) {
            if chosen.iter().any(|folder| path.starts_with(folder)) {
                return;
            }
            chosen.retain(|folder| !folder.starts_with(path));
            chosen.insert(path.to_owned());
        }
        self.picked = (chosen != self.current()).then_some(chosen);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let chosen = self.chosen();
        let mut rows = Vec::new();
        self.walk(Path::new(""), 0, &chosen, &mut rows);
        self.rows = rows;
    }

    fn walk(&self, folder: &Path, depth: usize, chosen: &BTreeSet<PathBuf>, rows: &mut Vec<Row>) {
        let children = match self.tree.get(folder) {
            None => return rows.push(Row::Note { depth, text: "Reading the folders…".to_owned() }),
            Some(Err(error)) => return rows.push(Row::Note { depth, text: error.clone() }),
            Some(Ok(children)) => children,
        };
        if children.is_empty() {
            let text = if depth == 0 { "The checked-out commit has only top-level files." } else { "No folders inside" };
            return rows.push(Row::Note { depth, text: text.to_owned() });
        }
        for child in children {
            let tick = if chosen.contains(child) {
                Tick::On
            } else if chosen.iter().any(|picked| child.starts_with(picked)) {
                Tick::Within
            } else {
                Tick::Off
            };
            let inside = chosen.iter().filter(|picked| picked.starts_with(child) && *picked != child).count();
            let unfolded = self.unfolded.contains(child);
            let name = child.file_name().unwrap_or(child.as_os_str()).to_string_lossy().into_owned();
            rows.push(Row::Folder { path: child.clone(), name, depth, unfolded, tick, inside });
            if unfolded {
                self.walk(child, depth + 1, chosen, rows);
            }
        }
    }

    fn settle(&mut self, reading: Reading) {
        let folders: BTreeSet<PathBuf> = reading.mode.folders().iter().cloned().collect();
        if matches!(reading.mode, Mode::Patterns(_)) || self.picked.as_ref() == Some(&folders) {
            self.picked = None;
        }
        self.mode = Some(reading.mode);
        self.shallow = reading.shallow;
        self.problem = None;
        self.rebuild();
    }
}

pub(crate) fn mode(repo: &Repository) -> Result<Mode, String> {
    let output = repo
        .read_command()
        .args(["config", "--type=bool", "--get-regexp", r"^core\.sparsecheckout(cone)?$"])
        .run()
        .map_err(|error| format!("Run bundled Git: {error}"))?;
    let (mut on, mut cone) = (false, false);
    match output.status.code() {
        Some(0) => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                match line.split_once(' ') {
                    Some(("core.sparsecheckout", value)) => on = value == "true",
                    Some(("core.sparsecheckoutcone", value)) => cone = value == "true",
                    _ => {}
                }
            }
        }
        Some(1) => return Ok(Mode::Off),
        _ => return Err(git_error(&output.stderr, output.status)),
    }
    if !on {
        return Ok(Mode::Off);
    }
    let listed = checked(repo.read_command().args(["-c", "core.quotePath=true", "sparse-checkout", "list"]))?;
    let lines = listed.split(|&byte| byte == b'\n').filter(|line| !line.is_empty());
    if cone {
        let mut folders: Vec<PathBuf> = lines.map(listed_path).collect();
        folders.sort();
        folders.dedup();
        Ok(Mode::Cone(folders))
    } else {
        Ok(Mode::Patterns(lines.map(|line| String::from_utf8_lossy(line).into_owned()).collect()))
    }
}

fn listed_path(line: &[u8]) -> PathBuf {
    let bytes = crate::diff_select::unquoted(line).unwrap_or_else(|| line.to_vec());
    PathBuf::from(OsString::from_vec(bytes))
}

pub(crate) fn shallow(repo: &Repository) -> Result<bool, String> {
    let said = checked(repo.read_command().args(["rev-parse", "--is-shallow-repository"]))?;
    Ok(String::from_utf8_lossy(&said).trim() == "true")
}

pub(crate) fn read(repo: &Repository) -> Result<Reading, String> {
    Ok(Reading { mode: mode(repo)?, shallow: shallow(repo)? })
}

pub(crate) fn in_cone(path: &Path, folders: &[PathBuf]) -> bool {
    let parent = path.parent().unwrap_or(Path::new(""));
    parent.as_os_str().is_empty() || folders.iter().any(|folder| path.starts_with(folder) || folder.starts_with(parent))
}

pub(crate) fn outside(repo: &Repository, folders: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let status = repo.status()?;
    let mut found: Vec<PathBuf> = status
        .staged
        .iter()
        .chain(&status.unstaged)
        .filter(|file| file.status != "Untracked")
        .filter(|file| !in_cone(&file.path, folders) || file.from.as_deref().is_some_and(|from| !in_cone(from, folders)))
        .map(|file| file.path.clone())
        .collect();
    found.sort();
    found.dedup();
    Ok(found)
}

pub(crate) fn quoted(folder: &Path) -> Result<Vec<u8>, String> {
    validate_path(folder)?;
    let text = folder.to_str().ok_or_else(|| format!("{} is not a name FastKraken can pass to Git.", folder.display()))?;
    let trimmed = |letter: char| matches!(letter, ' ' | '\t' | '\n' | '\r');
    if text.starts_with(trimmed) || text.ends_with(trimmed) {
        return Err(format!("\"{text}\" starts or ends with a space, which Git drops from sparse checkout folders. Rename the folder to check it out on its own."));
    }
    if !text.chars().any(|letter| letter == '"' || letter == '\\' || letter.is_control()) {
        return Ok(text.as_bytes().to_vec());
    }
    let mut out = vec![b'"'];
    for byte in text.bytes() {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            byte if byte < 0x20 || byte == 0x7f => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
            byte => out.push(byte),
        }
    }
    out.push(b'"');
    Ok(out)
}

fn shown_paths(paths: &[PathBuf]) -> String {
    let mut named: Vec<String> = paths.iter().take(3).map(|path| path.display().to_string()).collect();
    if paths.len() > 3 {
        named.push(format!("{} more", paths.len() - 3));
    }
    match named.as_slice() {
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        [] => String::new(),
    }
}

fn files_have(count: usize) -> String {
    match count {
        1 => "1 file outside the chosen folders has".to_owned(),
        n => format!("{n} files outside the chosen folders have"),
    }
}

fn left_by_git(stderr: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(stderr);
    let Some((_, rest)) = text.split_once(LEFT) else { return Vec::new() };
    rest.lines()
        .skip(1)
        .take_while(|line| line.starts_with('\t'))
        .map(|line| listed_path(line.trim_start_matches('\t').as_bytes()))
        .collect()
}

fn folders_text(folders: &[PathBuf]) -> String {
    match folders {
        [] => "Only the top-level files are checked out.".to_owned(),
        [one] => format!("Only {} and the top-level files are checked out.", one.display()),
        many => format!("Only {} and the top-level files are checked out.", counted(many.len(), "folder")),
    }
}

pub(crate) fn apply(repo: &Repository, change: &Change) -> Result<Outcome, Failure> {
    repo.expect_no_operation()?;
    if repo.resolve(OsStr::new("HEAD")).is_err() {
        return Err(NO_COMMITS.to_owned().into());
    }
    let now = mode(repo)?;
    if now != change.expected {
        return Err(CHANGED.to_owned().into());
    }
    if change.reapply && !matches!(now, Mode::Cone(_)) {
        return Err("Sparse checkout is not on for folders, so there is nothing to reapply.".to_owned().into());
    }
    let mut input = Vec::new();
    for folder in &change.folders {
        input.extend(quoted(folder)?);
        input.push(b'\n');
    }
    let in_the_way = outside(repo, &change.folders)?;
    let stashed = !in_the_way.is_empty();
    if stashed && !change.stash {
        let text = format!(
            "{} uncommitted changes: {}. Git would leave them in place, so their folders would stay. Commit or stash them first.",
            files_have(in_the_way.len()),
            shown_paths(&in_the_way)
        );
        let retry = Change { stash: true, ..change.clone() };
        return Err(Failure::from(text).offer("Stash them and apply", view::Action::Sparse(Action::Retry(retry))));
    }
    if stashed {
        repo.save_stash(&in_the_way, STASH_MESSAGE)?;
    }
    let kept = |text: String| {
        if stashed {
            format!("{text}\nYour changes outside the chosen folders are in the stash \"{STASH_MESSAGE}\".")
        } else {
            text
        }
    };
    let output = if change.reapply {
        repo.command().args(["sparse-checkout", "reapply"]).run()
    } else {
        run_with_input(repo.command().args(["sparse-checkout", "set", "--cone", "--stdin"]), input)
    }
    .map_err(|error| kept(format!("Run bundled Git: {error}")))?;
    if !output.status.success() {
        return Err(kept(git_error(&output.stderr, output.status)).into());
    }
    let wanted: BTreeSet<&PathBuf> = change.folders.iter().collect();
    match mode(repo)? {
        Mode::Cone(folders) if folders.iter().collect::<BTreeSet<_>>() == wanted => {}
        _ => return Err(kept("Git ran, but the sparse checkout does not list the chosen folders.".to_owned()).into()),
    }
    let title = if change.reapply {
        "Sparse checkout reapplied"
    } else if change.expected.on() {
        "Sparse checkout applied"
    } else {
        "Sparse checkout on"
    };
    let mut detail = folders_text(&change.folders);
    if stashed {
        detail.push_str(&format!(
            " Your changes to {} outside them are in the stash \"{STASH_MESSAGE}\". Apply it once their folders are checked out again.",
            counted(in_the_way.len(), "file")
        ));
    }
    let left = left_by_git(&output.stderr);
    let mut outcome = Outcome::titled(title, detail);
    if !left.is_empty() {
        outcome.detail.push_str(&format!(
            " Git left {} with changes in place: {}. Commit or stash them, then reapply.",
            counted(left.len(), "file"),
            shown_paths(&left)
        ));
        outcome = outcome.sticky();
    }
    if stashed {
        outcome = outcome.sticky();
    }
    Ok(outcome)
}

pub(crate) fn disable(repo: &Repository, expected: &Mode) -> Result<Outcome, String> {
    repo.expect_no_operation()?;
    if mode(repo)? != *expected {
        return Err(CHANGED.to_owned());
    }
    if !expected.on() {
        return Err("Sparse checkout is already off.".to_owned());
    }
    checked(repo.command().args(["sparse-checkout", "disable"]))?;
    if mode(repo)? != Mode::Off {
        return Err("Git ran, but sparse checkout is still on.".to_owned());
    }
    Ok(Outcome::titled("Sparse checkout off", "Every file of the checked-out commit is in the working copy again."))
}

fn commits(repo: &Repository) -> usize {
    checked(repo.read_command().args(["rev-list", "--count", "HEAD", "--"]))
        .ok()
        .and_then(|said| String::from_utf8_lossy(&said).trim().parse().ok())
        .unwrap_or(0)
}

pub(crate) fn fetch_history(repo: &Repository, remote: &str) -> Result<Outcome, String> {
    crate::remotes::validate_name(remote)?;
    if !shallow(repo)? {
        return Err(COMPLETE.to_owned());
    }
    if !String::from_utf8_lossy(&checked(repo.read_command().arg("remote"))?).lines().any(|listed| listed == remote) {
        return Err(format!("{remote} is not a remote of this repository any more."));
    }
    let before = commits(repo);
    checked(repo.network_command()?.args([
        "fetch",
        "--unshallow",
        "--progress",
        "--no-prune",
        "--no-prune-tags",
        "--no-recurse-submodules",
        "--",
        remote,
    ]))?;
    if shallow(repo)? {
        return Err(format!("Git fetched from {remote}, but the history is still shallow. Fetch from the remote it was cloned from."));
    }
    let after = commits(repo);
    let gained = after.saturating_sub(before);
    Ok(Outcome::titled(
        "Fetched the full history",
        format!(
            "From {remote}. The checked-out branch now has {} of history, {} more than before.",
            counted(after, "commit"),
            crate::paging::grouped(gained)
        ),
    ))
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.sparse;
    state.ticket += 1;
    let (ticket, epoch) = (state.ticket, tab.epoch);
    Tab::work(
        cx,
        move || read(&repo),
        move |tab, reading, cx| {
            if tab.epoch != epoch || tab.ui.sparse.ticket != ticket {
                return;
            }
            match reading {
                Ok(reading) => tab.ui.sparse.settle(reading),
                Err(problem) => tab.ui.sparse.problem = Some(problem),
            }
            if tree_wanted(&tab.ui) {
                read_tree(tab, cx);
            }
            cx.notify();
        },
    );
}

pub(crate) fn tree_wanted(ui: &Ui) -> bool {
    ui.preferences.open() && ui.preferences.section() == Section::Repository
}

pub(crate) fn read_tree(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(head) = tab.ui.head.clone() else { return };
    let state = &mut tab.ui.sparse;
    if state.tree_at.as_deref() == Some(head.as_str()) {
        return;
    }
    state.tree.clear();
    state.tree_at = Some(head);
    let folders: Vec<PathBuf> = std::iter::once(PathBuf::new()).chain(state.unfolded.iter().cloned()).collect();
    state.rebuild();
    load(tab, folders, cx);
}

fn load(tab: &mut Tab, folders: Vec<PathBuf>, cx: &mut Context<Tab>) {
    let (Some(repo), Some(head)) = (tab.repo.clone(), tab.ui.sparse.tree_at.clone()) else { return };
    let epoch = tab.epoch;
    let at = head.clone();
    Tab::work(
        cx,
        move || {
            let rev = Rev::Commit(at);
            folders
                .into_iter()
                .map(|folder| {
                    let listed = repo.browse_folder(&rev, &folder).map(|entries| {
                        entries.into_iter().filter(|entry| entry.kind == Kind::Folder).map(|entry| entry.path).collect()
                    });
                    (folder, listed)
                })
                .collect::<Vec<_>>()
        },
        move |tab, listed, cx| {
            if tab.epoch != epoch || tab.ui.sparse.tree_at.as_deref() != Some(head.as_str()) {
                return;
            }
            tab.ui.sparse.tree.extend(listed);
            tab.ui.sparse.rebuild();
            cx.notify();
        },
    );
}

fn write_refusal(ui: &Ui) -> Option<String> {
    if ui.head.is_none() {
        return Some(NO_COMMITS.trim_end_matches('.').to_owned());
    }
    if ui.sparse.mode.is_none() {
        return Some("Wait for the sparse checkout to be read".to_owned());
    }
    operating(ui).or_else(|| busy(ui))
}

pub(crate) fn history_remote(ui: &Ui) -> Option<String> {
    let remotes = &ui.refs.remotes;
    ui.upstream
        .as_deref()
        .and_then(|upstream| crate::branch::split_remote(remotes, OsStr::new(upstream)))
        .map(|(remote, _)| remote.to_owned())
        .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
        .or_else(|| remotes.first().cloned())
}

fn fetch_refusal(ui: &Ui) -> Option<String> {
    networking(ui).or_else(|| history_remote(ui).is_none().then(|| NO_REMOTE.to_owned()))
}

fn refused(tab: &mut Tab, reason: Option<String>, cx: &mut Context<Tab>) -> bool {
    let Some(reason) = reason else { return false };
    tab.ui.menu = None;
    tab.ui.say(Tone::Info, reason);
    cx.notify();
    true
}

fn run(tab: &mut Tab, change: Change, cx: &mut Context<Tab>) {
    let progress = if change.reapply { "Reapplying the sparse checkout…" } else { "Applying the sparse checkout…" };
    let mut applied = change.folders.clone();
    applied.sort();
    let job = move |repo: Repository| apply(&repo, &change);
    tab.run_operation_then(cx, progress, "Sparse checkout applied", false, job, move |tab, _| {
        let state = &mut tab.ui.sparse;
        state.mode = Some(Mode::Cone(applied));
        state.picked = None;
        state.rebuild();
    });
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::Tick(path) => tab.ui.sparse.tick(&path),
        Action::Unfold(path) => {
            let state = &mut tab.ui.sparse;
            if !state.unfolded.remove(&path) {
                state.unfolded.insert(path.clone());
                if !state.tree.contains_key(&path) {
                    load(tab, vec![path], cx);
                }
            }
            tab.ui.sparse.rebuild();
        }
        Action::Revert => {
            tab.ui.sparse.picked = None;
            tab.ui.sparse.rebuild();
        }
        Action::Enable | Action::Apply => {
            if refused(tab, write_refusal(&tab.ui), cx) {
                return;
            }
            let Some(expected) = tab.ui.sparse.mode.clone() else { return };
            if matches!(action, Action::Enable) && expected.on() {
                return;
            }
            if matches!(expected, Mode::Patterns(_)) {
                refused(tab, Some("This sparse checkout uses patterns. Turn it off before choosing folders".to_owned()), cx);
                return;
            }
            let folders = tab.ui.sparse.chosen().into_iter().collect();
            run(tab, Change { folders, expected, stash: false, reapply: false }, cx);
        }
        Action::Reapply => {
            if refused(tab, write_refusal(&tab.ui), cx) {
                return;
            }
            let Some(expected @ Mode::Cone(_)) = tab.ui.sparse.mode.clone() else { return };
            let folders = expected.folders().to_vec();
            run(tab, Change { folders, expected, stash: false, reapply: true }, cx);
        }
        Action::Retry(change) => {
            if refused(tab, write_refusal(&tab.ui), cx) {
                return;
            }
            run(tab, Change { stash: true, ..change }, cx);
        }
        Action::Disable => {
            if refused(tab, write_refusal(&tab.ui), cx) {
                return;
            }
            let Some(expected) = tab.ui.sparse.mode.clone().filter(Mode::on) else { return };
            let job = move |repo: Repository| disable(&repo, &expected);
            tab.run_operation_then(cx, "Turning sparse checkout off…", "Sparse checkout off", false, job, |tab, _| {
                let state = &mut tab.ui.sparse;
                state.mode = Some(Mode::Off);
                state.picked = None;
                state.rebuild();
            });
        }
        Action::FetchHistory => {
            if !tab.ui.sparse.shallow {
                refused(tab, Some(COMPLETE.trim_end_matches('.').to_owned()), cx);
                return;
            }
            if refused(tab, fetch_refusal(&tab.ui), cx) {
                return;
            }
            let Some(remote) = history_remote(&tab.ui) else { return };
            let progress = format!("Fetching the full history from {remote}…");
            let spec = jobs::Spec::fetch(Some(remote.clone()));
            let job = move |repo: Repository| fetch_history(&repo, &remote);
            tab.run_network(cx, spec, &progress, "Fetched the full history", false, job, |_, _| {});
        }
    }
    cx.notify();
}

fn settings_action() -> view::Action {
    view::Action::Preferences(settings_page::Action::Open(Some(Section::Repository)))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "sparse.status",
        Group::Settings,
        |ui, _| ui.sparse.summary().unwrap_or_else(|| "Sparse checkout".to_owned()),
        |ui, target, _| if on_repo(ui, target) && ui.sparse.on() { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(settings_action()),
    )
    .words(&["sparse checkout", "missing folders", "cone", "choose folders"])
    .icon("folder"),
    Command::new(
        "sparse.settings",
        Group::Settings,
        |_, _| "Sparse checkout…".into(),
        |ui, target, _| if on_repo(ui, target) && !ui.sparse.on() { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(settings_action()),
    )
    .words(&["git sparse-checkout", "check out some folders", "monorepo", "cone"])
    .icon("folder"),
    Command::new(
        "repo.fetch-history",
        Group::Sync,
        |_, _| "Fetch full history".into(),
        |ui, target, _| {
            if !on_repo(ui, target) || !ui.sparse.shallow {
                return Availability::Hidden;
            }
            Availability::unless(fetch_refusal(ui))
        },
        |_, _| Some(view::Action::Sparse(Action::FetchHistory)),
    )
    .words(&["git fetch --unshallow", "shallow clone", "deepen", "whole history"])
    .icon("fetch"),
];

pub(crate) fn fetch_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> Option<gpui::Stateful<Div>> {
    if !ui.sparse.shallow || ui.has_more || !ui.history_complete {
        return None;
    }
    let refusal = fetch_refusal(ui);
    Some(
        kit::btn("fetch-full-history", "Fetch full history")
            .small()
            .icon("fetch")
            .enabled(refusal.is_none())
            .tip(refusal.unwrap_or_else(|| "git fetch --unshallow".to_owned()))
            .on(view::Action::Sparse(Action::FetchHistory), cx),
    )
}

fn status_text(state: &State) -> String {
    let mut text = match state.mode.as_ref() {
        Some(Mode::Cone(folders)) => folders_text(folders),
        _ => "Every file is checked out.".to_owned(),
    };
    if state.edited() {
        let chosen = state.chosen();
        text.push_str(&format!(
            " Ticked: {}. {} to check them out.",
            if chosen.is_empty() { "no folders".to_owned() } else { counted(chosen.len(), "folder") },
            if state.on() { "Apply" } else { "Turn it on" }
        ));
    }
    text
}

fn tree_row<T: Desk>(index: usize, row: &Row, enabled: bool, cx: &mut Context<T>) -> gpui::AnyElement {
    let indent = |depth: usize| px(8. + depth as f32 * 16.);
    match row {
        Row::Note { depth, text } => div()
            .w_full()
            .h(px(TREE_ROW))
            .flex()
            .items_center()
            .pl(indent(*depth) + px(20.))
            .child(kit::fit(text.clone()).size(TEXT_SMALL).color(palette().text_3).grow())
            .into_any_element(),
        Row::Folder { path, name, depth, unfolded, tick, inside } => {
            let chevron = div()
                .id(("sparse-chevron", index))
                .size(px(16.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(kit::icon(if *unfolded { "chevron-down" } else { "chevron-right" }).size(px(12.)).text_color(rgb(palette().text_3)));
            let chevron = kit::pressable(chevron, view::Action::Sparse(Action::Unfold(path.clone())), cx);
            let within = *tick == Tick::Within;
            let pick = div()
                .id(("sparse-tick", index))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .when(enabled && !within, |d| d.cursor_pointer().focusable().focus(|s| s.bg(rgb(palette().hover))))
                .child(div().when(within, |d| d.opacity(0.5)).child(kit::checkbox(*tick != Tick::Off)))
                .child(kit::icon("folder").size(px(13.)).text_color(rgb(palette().text_3)))
                .child(kit::fit(name.clone()).size(12.5).color(if *tick == Tick::Off { palette().text_2 } else { palette().text }).grow())
                .when(*inside > 0, |d| d.child(kit::muted(format!("{} inside", counted(*inside, "folder")))))
                .when(within, |d| d.tooltip(kit::tip("Inside a ticked folder, so it is checked out too")));
            let pick = if enabled && !within { kit::pressable(pick, view::Action::Sparse(Action::Tick(path.clone())), cx) } else { pick };
            div()
                .id(("sparse-folder", index))
                .w_full()
                .h(px(TREE_ROW))
                .flex()
                .items_center()
                .gap_1()
                .pl(indent(*depth))
                .pr_2()
                .child(chevron)
                .child(pick)
                .into_any_element()
        }
    }
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.sparse;
    let body = div().flex().flex_col().child(settings_page::group("SPARSE CHECKOUT"));
    let Some(mode) = state.mode.as_ref() else {
        let text = state.problem.clone().unwrap_or_else(|| "Reading the sparse checkout…".to_owned());
        return body.child(div().py_2().child(kit::muted(text)));
    };
    let refusal = write_refusal(ui);
    if let Mode::Patterns(patterns) = mode {
        return body.child(settings_page::setting(
            "Sparse checkout with patterns",
            Some(format!("It was set up elsewhere with {}, which FastKraken does not edit. Turn it off to check out every file.", counted(patterns.len(), "pattern")).into()),
            kit::btn("preferences-sparse-off", "Turn off").small().enabled(refusal.is_none()).on(view::Action::Sparse(Action::Disable), cx),
        ));
    }
    if ui.head.is_none() {
        return body.child(div().py_2().child(kit::muted(NO_COMMITS)));
    }
    let on = mode.on();
    let switch = if on { Action::Disable } else { Action::Enable };
    let count = state.rows.len();
    let height = px(count.clamp(1, TREE_ROWS_SHOWN) as f32 * TREE_ROW);
    let enabled = refusal.is_none();
    let list = uniform_list(
        "sparse-folders",
        count,
        cx.processor(move |app: &mut T, range: std::ops::Range<usize>, _, cx| {
            let rows: Vec<(usize, Row)> = range.filter_map(|index| Some((index, app.ui().sparse.rows.get(index)?.clone()))).collect();
            rows.into_iter().map(|(index, row)| tree_row(index, &row, enabled, cx)).collect::<Vec<_>>()
        }),
    )
    .track_scroll(state.scroll.clone())
    .w_full()
    .h(height);
    let changed = state.edited();
    body.child(settings_page::tick("preferences-sparse", on, SWITCH_TITLE, SWITCH_HELP, view::Action::Sparse(switch), cx))
        .child(div().pt_2().pb_1().child(kit::muted(status_text(state))))
        .child(div().pt_2().pb_1().child(kit::label("FOLDERS")))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(palette().line))
                .bg(rgb(palette().canvas))
                .py_1()
                .child(list),
        )
        .child(
            div()
                .pt_3()
                .flex()
                .gap_2()
                .when(on, |d| {
                    d.child(
                        kit::btn("preferences-sparse-apply", "Apply")
                            .small()
                            .primary()
                            .enabled(enabled && changed)
                            .tip(refusal.clone().unwrap_or_else(|| "git sparse-checkout set --cone".to_owned()))
                            .on(view::Action::Sparse(Action::Apply), cx),
                    )
                    .child(
                        kit::btn("preferences-sparse-reapply", "Reapply")
                            .small()
                            .enabled(enabled)
                            .tip("Check out the chosen folders again, after files left in place are committed or stashed")
                            .on(view::Action::Sparse(Action::Reapply), cx),
                    )
                })
                .when(changed, |d| {
                    d.child(kit::btn("preferences-sparse-revert", "Clear changes").small().on(view::Action::Sparse(Action::Revert), cx))
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn commit(f: &Fixture, files: &[(&str, &str)], subject: &str) {
        for (path, text) in files {
            let path = f.dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        f.git(&["add", "--all"]);
        f.git(&["commit", "-qm", subject]);
    }

    fn monorepo(f: &Fixture) {
        commit(
            f,
            &[
                ("readme.txt", "Trail guide\n"),
                ("maps/ridge/route.txt", "Ridge route\n"),
                ("maps/lake.txt", "Lake map\n"),
                ("camp/tent.txt", "Tent\n"),
                ("gear/rope.txt", "Rope\n"),
            ],
            "Lay out the trail",
        );
    }

    fn text(f: &Fixture, args: &[&str]) -> String {
        String::from_utf8(f.git(args)).unwrap()
    }

    fn present(f: &Fixture) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![f.dir.clone()];
        while let Some(folder) = stack.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(path.strip_prefix(&f.dir).unwrap().display().to_string());
                }
            }
        }
        found.sort();
        found
    }

    fn change(folders: &[&str], expected: Mode) -> Change {
        Change { folders: folders.iter().map(PathBuf::from).collect(), expected, stash: false, reapply: false }
    }

    #[test]
    fn a_folder_is_in_the_cone_with_the_files_beside_its_parents() {
        let folders = [PathBuf::from("maps/ridge")];
        for (path, inside) in [
            ("readme.txt", true),
            ("maps/lake.txt", true),
            ("maps/ridge/route.txt", true),
            ("maps/ridge/deep/stone.txt", true),
            ("maps/valley/river.txt", false),
            ("camp/tent.txt", false),
            ("maps-old/lake.txt", false),
        ] {
            assert_eq!(in_cone(Path::new(path), &folders), inside, "{path}");
        }
        assert!(in_cone(Path::new("readme.txt"), &[]));
        assert!(!in_cone(Path::new("camp/tent.txt"), &[]));
    }

    #[test]
    fn turning_sparse_checkout_on_for_one_folder_leaves_it_and_the_top_level_files() {
        let f = Fixture::new();
        monorepo(&f);
        let head = text(&f, &["rev-parse", "HEAD"]);
        let index = text(&f, &["ls-files", "--stage"]);
        assert_eq!(read(&f.repo).unwrap(), Reading { mode: Mode::Off, shallow: false });
        let outcome = apply(&f.repo, &change(&["maps"], Mode::Off)).unwrap();
        assert_eq!(outcome.title, "Sparse checkout on");
        assert_eq!(outcome.detail, "Only maps and the top-level files are checked out.");
        assert_eq!(present(&f), ["maps/lake.txt", "maps/ridge/route.txt", "readme.txt"]);
        assert_eq!(mode(&f.repo).unwrap(), Mode::Cone(vec![PathBuf::from("maps")]));
        assert_eq!(text(&f, &["sparse-checkout", "list"]), "maps\n");
        assert_eq!(text(&f, &["rev-parse", "HEAD"]), head);
        assert_eq!(text(&f, &["ls-files", "--stage"]), index);
        assert_eq!(text(&f, &["status", "--porcelain"]), "");
        let narrower = apply(&f.repo, &change(&["camp", "maps/ridge"], Mode::Cone(vec!["maps".into()]))).unwrap();
        assert_eq!(narrower.title, "Sparse checkout applied");
        assert_eq!(narrower.detail, "Only 2 folders and the top-level files are checked out.");
        assert_eq!(present(&f), ["camp/tent.txt", "maps/lake.txt", "maps/ridge/route.txt", "readme.txt"]);
        let none = apply(&f.repo, &change(&[], mode(&f.repo).unwrap())).unwrap();
        assert_eq!(none.detail, "Only the top-level files are checked out.");
        assert_eq!(present(&f), ["readme.txt"]);
        assert_eq!(Mode::Cone(Vec::new()).summary().as_deref(), Some("Sparse: top-level files only"));
        let off = disable(&f.repo, &Mode::Cone(Vec::new())).unwrap();
        assert_eq!(off.title, "Sparse checkout off");
        assert_eq!(present(&f).len(), 5);
        assert_eq!(mode(&f.repo).unwrap(), Mode::Off);
        assert_eq!(text(&f, &["ls-files", "--stage"]), index);
    }

    #[test]
    fn a_sparse_checkout_changed_elsewhere_is_refused_before_git_runs() {
        let f = Fixture::new();
        monorepo(&f);
        f.git(&["sparse-checkout", "set", "--cone", "gear"]);
        let shown = Mode::Cone(vec![PathBuf::from("maps")]);
        assert_eq!(apply(&f.repo, &change(&["camp"], shown.clone())).unwrap_err().text, CHANGED);
        assert_eq!(disable(&f.repo, &shown).unwrap_err(), CHANGED);
        assert_eq!(apply(&f.repo, &change(&["camp"], Mode::Off)).unwrap_err().text, CHANGED);
        assert_eq!(present(&f), ["gear/rope.txt", "readme.txt"]);
        assert_eq!(text(&f, &["sparse-checkout", "list"]), "gear\n");
        f.git(&["sparse-checkout", "set", "--no-cone", "/*.txt"]);
        assert_eq!(mode(&f.repo).unwrap(), Mode::Patterns(vec!["/*.txt".to_owned()]));
        assert_eq!(Mode::Patterns(vec!["/*.txt".to_owned()]).summary().as_deref(), Some("Sparse: 1 pattern"));
        let reapply = Change { reapply: true, ..change(&[], Mode::Patterns(vec!["/*.txt".to_owned()])) };
        assert!(apply(&f.repo, &reapply).unwrap_err().text.contains("nothing to reapply"));
        let empty = Fixture::new();
        assert_eq!(apply(&empty.repo, &change(&["maps"], Mode::Off)).unwrap_err().text, NO_COMMITS);
    }

    #[test]
    fn changes_outside_the_chosen_folders_are_refused_then_stashed_on_request() {
        let f = Fixture::new();
        monorepo(&f);
        fs::write(f.dir.join("camp/tent.txt"), "Tent, patched\n").unwrap();
        fs::write(f.dir.join("gear/rope.txt"), "Rope, frayed\n").unwrap();
        f.git(&["add", "gear/rope.txt"]);
        fs::write(f.dir.join("maps/lake.txt"), "Lake map, redrawn\n").unwrap();
        fs::write(f.dir.join("camp/new.txt"), "Untracked\n").unwrap();
        assert_eq!(outside(&f.repo, &[PathBuf::from("maps")]).unwrap(), [PathBuf::from("camp/tent.txt"), PathBuf::from("gear/rope.txt")]);
        let refused = apply(&f.repo, &change(&["maps"], Mode::Off)).unwrap_err();
        assert_eq!(
            refused.text,
            "2 files outside the chosen folders have uncommitted changes: camp/tent.txt and gear/rope.txt. Git would leave them in place, so their folders would stay. Commit or stash them first."
        );
        assert_eq!(refused.actions.len(), 1);
        assert_eq!(refused.actions[0].0.as_ref(), "Stash them and apply");
        let view::Action::Sparse(Action::Retry(retry)) = refused.actions[0].1.clone() else { panic!("{:?}", refused.actions) };
        assert!(retry.stash);
        assert_eq!(mode(&f.repo).unwrap(), Mode::Off);
        assert_eq!(text(&f, &["stash", "list"]), "");
        let applied = apply(&f.repo, &retry).unwrap();
        assert!(applied.sticky);
        assert!(applied.detail.contains(&format!("Your changes to 2 files outside them are in the stash \"{STASH_MESSAGE}\"")), "{}", applied.detail);
        assert_eq!(text(&f, &["stash", "list", "--format=%s"]), format!("On main: {STASH_MESSAGE}\n"));
        assert_eq!(text(&f, &["stash", "show", "--name-only", "--include-untracked", "stash@{0}"]), "camp/tent.txt\ngear/rope.txt\n");
        assert_eq!(text(&f, &["status", "--porcelain", "--untracked-files=no"]), " M maps/lake.txt\n");
        assert!(!f.dir.join("gear").exists());
        assert_eq!(fs::read_to_string(f.dir.join("camp/new.txt")).unwrap(), "Untracked\n");
        assert!(!f.dir.join("camp/tent.txt").exists());
        assert_eq!(mode(&f.repo).unwrap(), Mode::Cone(vec![PathBuf::from("maps")]));
    }

    #[test]
    fn files_git_leaves_in_place_are_named_and_reapplied_later() {
        let said = b"warning: The following paths are not up to date and were left despite sparse patterns:\n\tcamp/tent.txt\n\t\"gear/r\\303\\266pe.txt\"\n\nAfter fixing the above paths, you may want to run `git sparse-checkout reapply`.\n";
        assert_eq!(left_by_git(said), [PathBuf::from("camp/tent.txt"), PathBuf::from("gear/r\u{f6}pe.txt")]);
        assert!(left_by_git(b"").is_empty());
        let f = Fixture::new();
        monorepo(&f);
        fs::write(f.dir.join("camp/tent.txt"), "Tent, mended\n").unwrap();
        let output = f.repo.command().args(["sparse-checkout", "set", "--cone", "maps"]).output().unwrap();
        assert!(output.status.success());
        assert_eq!(left_by_git(&output.stderr), [PathBuf::from("camp/tent.txt")]);
        f.git(&["checkout", "--", "camp/tent.txt"]);
        assert_eq!(present(&f), ["camp/tent.txt", "maps/lake.txt", "maps/ridge/route.txt", "readme.txt"]);
        let cone = Mode::Cone(vec![PathBuf::from("maps")]);
        let reapply = Change { reapply: true, ..change(&["maps"], cone.clone()) };
        let outcome = apply(&f.repo, &reapply).unwrap();
        assert_eq!(outcome.title, "Sparse checkout reapplied");
        assert!(!outcome.sticky);
        assert_eq!(present(&f), ["maps/lake.txt", "maps/ridge/route.txt", "readme.txt"]);
        assert_eq!(mode(&f.repo).unwrap(), cone);
    }

    #[test]
    fn odd_folder_names_are_quoted_for_git() {
        assert_eq!(quoted(Path::new("maps/ridge")).unwrap(), b"maps/ridge");
        assert_eq!(quoted(Path::new("say \"hi\"")).unwrap(), b"\"say \\\"hi\\\"\"");
        assert_eq!(quoted(Path::new("line\nbreak")).unwrap(), b"\"line\\nbreak\"");
        assert!(quoted(Path::new("../up")).is_err());
        let f = Fixture::new();
        commit(&f, &[("readme.txt", "Guide\n"), ("say \"hi\"/a.txt", "A\n"), ("back\\slash/b.txt", "B\n"), ("plain/c.txt", "C\n")], "Odd names");
        let folders = vec![PathBuf::from("back\\slash"), PathBuf::from("say \"hi\"")];
        apply(&f.repo, &Change { folders: folders.clone(), expected: Mode::Off, stash: false, reapply: false }).unwrap();
        assert_eq!(mode(&f.repo).unwrap(), Mode::Cone(folders));
        assert!(!f.dir.join("plain").exists());
        assert!(f.dir.join("say \"hi\"/a.txt").exists());
    }

    #[test]
    fn a_folder_name_git_would_trim_is_refused_before_git_runs() {
        let f = Fixture::new();
        commit(&f, &[("readme.txt", "Guide\n"), ("trail /a.txt", "A\n"), (" lead/b.txt", "B\n"), ("plain/c.txt", "C\n")], "Spaced names");
        let before = present(&f);
        for name in ["trail ", " lead"] {
            let refused = apply(&f.repo, &change(&[name, "plain"], Mode::Off)).unwrap_err();
            assert!(refused.text.contains("starts or ends with a space"), "{name:?}: {}", refused.text);
            assert_eq!(mode(&f.repo).unwrap(), Mode::Off);
            assert_eq!(present(&f), before);
        }
        assert!(quoted(Path::new("maps/ridge route")).is_ok());
        assert!(quoted(Path::new("maps\t")).is_err());
    }

    #[test]
    fn the_folder_tree_ticks_a_folder_and_everything_inside_it() {
        let mut state = State { mode: Some(Mode::Cone(vec![PathBuf::from("maps/ridge")])), ..State::default() };
        state.tree.insert(PathBuf::new(), Ok(vec!["camp".into(), "maps".into()]));
        state.tree.insert(PathBuf::from("maps"), Ok(vec!["maps/ridge".into(), "maps/valley".into()]));
        state.unfolded.insert(PathBuf::from("maps"));
        state.rebuild();
        let ticks: Vec<(String, Tick, usize)> = state
            .rows()
            .iter()
            .filter_map(|row| match row {
                Row::Folder { name, tick, inside, .. } => Some((name.clone(), *tick, *inside)),
                Row::Note { .. } => None,
            })
            .collect();
        assert_eq!(ticks, [("camp".into(), Tick::Off, 0), ("maps".into(), Tick::Off, 1), ("ridge".into(), Tick::On, 0), ("valley".into(), Tick::Off, 0)]);
        assert!(!state.edited());
        state.tick(Path::new("maps"));
        assert_eq!(state.chosen(), BTreeSet::from([PathBuf::from("maps")]));
        assert!(state.edited());
        assert!(state.rows().iter().any(|row| matches!(row, Row::Folder { name, tick: Tick::Within, .. } if name == "ridge")));
        state.tick(Path::new("maps/valley"));
        assert_eq!(state.chosen(), BTreeSet::from([PathBuf::from("maps")]));
        state.tick(Path::new("maps"));
        state.tick(Path::new("maps/ridge"));
        assert!(!state.edited());
        state.tick(Path::new("camp"));
        state.settle(Reading { mode: Mode::Cone(vec!["camp".into(), "maps/ridge".into()]), shallow: true });
        assert!(!state.edited());
        assert!(state.shallow);
        assert_eq!(state.summary().as_deref(), Some("Sparse: 2 folders"));
        state.tree.remove(&PathBuf::from("maps"));
        state.rebuild();
        assert!(state.rows().iter().any(|row| matches!(row, Row::Note { depth: 1, text } if text == "Reading the folders…")));
    }

    #[test]
    fn a_shallow_clone_fetches_its_full_history_and_is_refused_once_it_has_it() {
        let source = Fixture::new();
        for index in 1..=5 {
            commit(&source, &[("log.txt", &format!("Day {index}\n"))], &format!("Day {index}"));
        }
        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("shallow");
        let address = format!("file://{}", source.dir.display());
        f_clone(dir.path(), &source, &["clone", "-q", "--depth=2", &address, clone.to_str().unwrap()]);
        let repo = source.open(&clone);
        assert!(shallow(&repo).unwrap());
        assert_eq!(commits(&repo), 2);
        let heads = |repo: &Repository| checked(repo.read_command().args(["for-each-ref", "--format=%(refname) %(objectname)"])).unwrap();
        let before = heads(&repo);
        assert!(fetch_history(&repo, "-x").is_err());
        assert_eq!(fetch_history(&repo, "upstream").unwrap_err(), "upstream is not a remote of this repository any more.");
        let outcome = fetch_history(&repo, "origin").unwrap();
        assert_eq!(outcome.title, "Fetched the full history");
        assert_eq!(outcome.detail, "From origin. The checked-out branch now has 5 commits of history, 3 more than before.");
        assert!(!shallow(&repo).unwrap());
        assert!(!clone.join(".git/shallow").exists());
        assert_eq!(commits(&repo), 5);
        assert_eq!(heads(&repo), before);
        assert_eq!(fetch_history(&repo, "origin").unwrap_err(), COMPLETE);
    }

    fn f_clone(dir: &Path, source: &Fixture, args: &[&str]) {
        let output = source.repo.command().current_dir(dir).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn the_end_of_a_shallow_history_says_so() {
        assert_eq!(counted(1, "commit"), "1 commit");
        assert_eq!(counted(3, "folder"), "3 folders");
        assert_eq!(Mode::Off.summary(), None);
        assert_eq!(Mode::Cone(vec!["a".into()]).summary().as_deref(), Some("Sparse: 1 folder"));
        assert_eq!(shown_paths(&["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]), "a, b, c and 2 more");
    }
}
