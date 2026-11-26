use crate::{
    Tab, activity, keys, kit,
    commands::{self, Side, Target},
    compare::Rev,
    conflict::{Shown, Tooled},
    notice::Notice,
    operation::Shape,
    repository::{self, Repository, validate_oid, validate_path},
    settings::Settings,
    theme::*,
    tools::{Detected, Found, Known, Lines},
    view::{self, Desk, Selection, Tone, Ui},
};
use gpui::{Context, Div, Window, div, prelude::*, px, rgb};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result<T> = std::result::Result<T, String>;

pub(crate) const GIT: &str = "git";
const OWN: &str = "fastkraken";
const IN_A_TERMINAL: &[&str] = &["vimdiff", "nvimdiff", "emerge"];

pub(crate) struct Tool {
    pub(crate) known: Known,
    diff: &'static str,
    merge: &'static str,
    trusted: bool,
}

const fn known(
    id: &'static str,
    name: &'static str,
    command: Option<&'static str>,
    apps: &'static [&'static str],
    inside: &'static str,
) -> Known {
    Known { id, name, command, apps, inside: Some(inside), lines: Lines::Flag }
}

pub(crate) const TOOLS: &[Tool] = &[
    Tool {
        known: known("filemerge", "FileMerge", None, &["Xcode.app", "Xcode-beta.app"], "Contents/Developer/usr/bin/opendiff"),
        diff: r#""$LOCAL" "$REMOTE" | cat"#,
        merge: r#""$LOCAL" "$REMOTE" -ancestor "$BASE" -merge "$MERGED" | cat"#,
        trusted: false,
    },
    Tool {
        known: known("kaleidoscope", "Kaleidoscope", Some("ksdiff"), &["Kaleidoscope.app"], "Contents/Resources/bin/ksdiff"),
        diff: r#"--partial-changeset --relative-path "$MERGED" -- "$LOCAL" "$REMOTE""#,
        merge: r#"--merge --output "$MERGED" --base "$BASE" -- "$LOCAL" --snapshot "$REMOTE" --snapshot"#,
        trusted: true,
    },
    Tool {
        known: known("bcomp", "Beyond Compare", Some("bcomp"), &["Beyond Compare.app"], "Contents/MacOS/bcomp"),
        diff: r#""$LOCAL" "$REMOTE""#,
        merge: r#""$LOCAL" "$REMOTE" "$BASE" "$MERGED""#,
        trusted: false,
    },
    Tool {
        known: known("smerge", "Sublime Merge", Some("smerge"), &["Sublime Merge.app"], "Contents/SharedSupport/bin/smerge"),
        diff: r#"mergetool "$LOCAL" "$REMOTE""#,
        merge: r#"mergetool "$BASE" "$LOCAL" "$REMOTE" -o "$MERGED""#,
        trusted: false,
    },
    Tool {
        known: known("p4merge", "P4Merge", Some("p4merge"), &["p4merge.app", "P4Merge.app"], "Contents/Resources/launchp4merge"),
        diff: r#""$LOCAL" "$REMOTE""#,
        merge: r#""$BASE" "$REMOTE" "$LOCAL" "$MERGED""#,
        trusted: false,
    },
    Tool {
        known: known("code", "VS Code", Some("code"), &["Visual Studio Code.app"], "Contents/Resources/app/bin/code"),
        diff: r#"--wait --diff "$LOCAL" "$REMOTE""#,
        merge: r#"--wait --merge "$REMOTE" "$LOCAL" "$BASE" "$MERGED""#,
        trusted: false,
    },
];

fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.known.id == id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Diff,
    Merge,
}

impl Mode {
    pub(crate) fn what(self) -> &'static str {
        match self {
            Mode::Diff => "diff tool",
            Mode::Merge => "merge tool",
        }
    }

    fn section(self) -> &'static str {
        match self {
            Mode::Diff => "diff",
            Mode::Merge => "merge",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Found(Found),
    Git(String),
    Gone(&'static Known),
    Unset,
}

pub(crate) fn pick(chosen: Option<&str>, found: &[Found], configured: Option<&str>) -> Option<Pick> {
    let configured = configured.map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
    let automatic = |configured: Option<String>| configured.map(Pick::Git).or_else(|| found.first().cloned().map(Pick::Found));
    match chosen {
        Some(GIT) => Some(configured.map_or(Pick::Unset, Pick::Git)),
        Some(id) => match found.iter().find(|found| found.tool.id == id) {
            Some(found) => Some(Pick::Found(found.clone())),
            None => match tool(id) {
                Some(tool) => Some(Pick::Gone(&tool.known)),
                None => automatic(configured),
            },
        },
        None => automatic(configured),
    }
}

pub(crate) fn runs(mode: Mode, found: &Found) -> Option<String> {
    let tool = tool(found.tool.id)?;
    let program = keys::quoted(&found.place().to_string_lossy());
    Some(match mode {
        Mode::Diff => format!("{program} {}", tool.diff),
        Mode::Merge if tool.trusted => format!("{program} {}", tool.merge),
        Mode::Merge => format!(
            r#"before=$(git hash-object -- "$MERGED"); {program} {} && test "$(git hash-object -- "$MERGED")" != "$before""#,
            tool.merge
        ),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) mode: Mode,
    pub(crate) name: String,
    pub(crate) settings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Choose(String),
    Failed(String),
}

pub(crate) fn call(mode: Mode, pick: Option<&Pick>) -> std::result::Result<Call, Refusal> {
    let mut settings = Vec::new();
    let name = match pick {
        None => {
            return Err(Refusal::Choose(format!(
                "FastKraken found no {} it knows on this Mac, and Git has none set.",
                mode.what()
            )));
        }
        Some(Pick::Unset) => return Err(Refusal::Choose(format!("Git has no {} set.", mode.what()))),
        Some(Pick::Gone(tool)) => return Err(Refusal::Failed(format!("{} is no longer installed.", tool.name))),
        Some(Pick::Git(name)) if IN_A_TERMINAL.iter().any(|terminal| name.starts_with(terminal)) => {
            return Err(Refusal::Choose(format!(
                "Git is set to use {name}, which needs a terminal. Choose a tool that opens its own window."
            )));
        }
        Some(Pick::Git(name)) => name.clone(),
        Some(Pick::Found(found)) => {
            let command = runs(mode, found).ok_or_else(|| Refusal::Failed(format!("{} cannot be run here.", found.tool.name)))?;
            let own = format!("{OWN}-{}", found.tool.id);
            settings.push(format!("{}.tool={own}", mode.section()));
            settings.push(format!("{}tool.{own}.cmd={command}", mode.section()));
            if mode == Mode::Merge {
                settings.push(format!("mergetool.{own}.trustExitCode=true"));
            }
            found.tool.name.to_owned()
        }
    };
    if mode == Mode::Merge {
        settings.push("mergetool.keepBackup=false".to_owned());
        settings.push("mergetool.writeToTemp=true".to_owned());
    }
    Ok(Call { mode, name, settings })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Configured {
    pub(crate) diff: Option<String>,
    pub(crate) merge: Option<String>,
}

impl Configured {
    pub(crate) fn of(&self, mode: Mode) -> Option<&str> {
        match mode {
            Mode::Diff => self.diff.as_deref(),
            Mode::Merge => self.merge.as_deref(),
        }
    }
}

fn value(mut command: Command, key: &str) -> Result<Option<String>> {
    let output = repository::run(command.args(["config", "--get", key])).map_err(|e| format!("Run bundled Git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|name| !name.is_empty())),
        Some(1) => Ok(None),
        _ => Err(repository::git_error(&output.stderr, output.status)),
    }
}

pub(crate) fn configured(repo: Option<&Repository>) -> Result<Configured> {
    let command = || match repo {
        Some(repo) => Ok(repo.read_command()),
        None => repository::bundled_command(Path::new("/")),
    };
    let merge = value(command()?, "merge.tool")?;
    let diff = value(command()?, "diff.tool")?.or_else(|| merge.clone());
    Ok(Configured { diff, merge })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sides {
    Unstaged,
    Staged,
    Commit(String),
    Between { from: Rev, to: Rev, since_fork: bool },
}

fn parent_of(repo: &Repository, oid: &str) -> Result<String> {
    validate_oid(oid)?;
    let exists = repository::run(repo.read_command().args(["rev-parse", "--verify", "--quiet"]).arg(format!("{oid}^{{commit}}")))
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if !exists.status.success() {
        return Err("That commit is no longer in this repository.".to_owned());
    }
    let parent = repository::run(repo.read_command().args(["rev-parse", "--verify", "--quiet"]).arg(format!("{oid}^1")))
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if parent.status.success() {
        Ok(String::from_utf8_lossy(&parent.stdout).trim().to_owned())
    } else {
        Ok(crate::compare::empty_tree_like(oid))
    }
}

pub(crate) fn ends(repo: &Repository, sides: &Sides) -> Result<Vec<String>> {
    Ok(match sides {
        Sides::Unstaged => Vec::new(),
        Sides::Staged => vec!["--cached".to_owned()],
        Sides::Commit(oid) => vec![parent_of(repo, oid)?, oid.clone()],
        Sides::Between { from, to, since_fork } => {
            let (from, to) = repo.compared_ends(from, to, *since_fork)?;
            std::iter::once(from).chain(to).collect()
        }
    })
}

fn named(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn changed(repo: &Repository, ends: &[String], path: &Path) -> Result<bool> {
    let output = repository::run(
        repo.read_command().args(["diff", "--quiet", "--no-ext-diff", "--no-textconv"]).args(ends).arg("--").arg(path),
    )
    .map_err(|e| format!("Run bundled Git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(repository::git_error(&output.stderr, output.status)),
    }
}

pub(crate) fn told(output: &Output) -> Option<String> {
    let text = String::from_utf8_lossy(&output.stderr);
    text.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !(line.starts_with("merge of ") && line.ends_with(" failed"))
                && !line.starts_with("fatal: external diff died")
        })
        .next_back()
        .map(|line| match line.split_once(": line ") {
            Some((_, rest)) => rest.split_once(": ").map_or(rest, |(_, said)| said).to_owned(),
            None => line.to_owned(),
        })
}

pub(crate) fn command_with(repo: &Repository, settings: &[String]) -> Command {
    let mut command = repo.command();
    for setting in settings {
        command.arg("-c").arg(setting);
    }
    command.arg("--literal-pathspecs");
    repository::with_system_tools(&mut command);
    command
}

pub(crate) fn diff(repo: &Repository, call: &Call, ends: &[String], path: &Path) -> Result<()> {
    validate_path(path)?;
    if !changed(repo, ends, path)? {
        return Err(format!("{} has no changes to compare any more.", named(path)));
    }
    let mut command = command_with(repo, &call.settings);
    command.args(["difftool", "--no-prompt", "--trust-exit-code"]).args(ends).arg("--").arg(path);
    let output = repository::run(&mut command).map_err(|e| format!("Run bundled Git: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(told(&output).unwrap_or_else(|| format!("{} stopped with {}", call.name, output.status)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Diff { path: PathBuf, sides: Sides },
    Merge(PathBuf),
    Choose(Mode, String),
}

pub(crate) struct State {
    pub(crate) diff: Option<String>,
    pub(crate) merge: Option<String>,
    pub(crate) configured: Option<Result<Configured>>,
    open: Vec<(Mode, PathBuf)>,
    ticket: u64,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let settings = crate::settings::settings(cx);
        Self { diff: settings.diff_tool, merge: settings.merge_tool, configured: None, open: Vec::new(), ticket: 0 }
    }

    pub(crate) fn chosen(&self, mode: Mode) -> Option<&str> {
        match mode {
            Mode::Diff => self.diff.as_deref(),
            Mode::Merge => self.merge.as_deref(),
        }
    }

    pub(crate) fn is_open(&self, mode: Mode, path: &Path) -> bool {
        self.open.iter().any(|(open, shown)| *open == mode && shown == path)
    }

    fn configured_name(&self, mode: Mode) -> Option<&str> {
        self.configured.as_ref().and_then(|read| read.as_ref().ok()).and_then(|configured| configured.of(mode))
    }

    pub(crate) fn pick(&self, mode: Mode, found: &Detected) -> Option<Pick> {
        pick(self.chosen(mode), &found.comparers, self.configured_name(mode))
    }

    fn close(&mut self, mode: Mode, path: &Path) {
        if let Some(at) = self.open.iter().position(|(open, shown)| *open == mode && shown == path) {
            self.open.remove(at);
        }
    }
}

pub(crate) const COMMANDS: &[commands::Command] = {
    use commands::{Availability, Command, Group};
    &[Command::new(
        "file.diff-tool",
        Group::Open,
        |ui, target| title(ui, aim(ui, target).map_or(Mode::Diff, |(mode, _)| mode)),
        |ui, target, _| match aim(ui, target) {
            None => Availability::Hidden,
            Some((_, Err(reason))) => Availability::Disabled(reason),
            Some((_, Ok(_))) => Availability::Enabled,
        },
        |ui, target| match aim(ui, target) {
            Some((_, Ok(action))) => Some(view::Action::DiffTool(action)),
            _ => None,
        },
    )
    .keys("cmd-d")
    .words(&[
        "external diff",
        "difftool",
        "merge tool",
        "mergetool",
        "filemerge",
        "kaleidoscope",
        "beyond compare",
        "sublime merge",
        "p4merge",
    ])
    .icon("external")]
};

fn title(ui: &Ui, mode: Mode) -> String {
    let chosen = ui.difftool.chosen(mode).and_then(|id| {
        let found = crate::tools::detected()?;
        found.comparers.iter().find(|found| found.tool.id == id).map(|found| found.tool.name)
    });
    match chosen {
        Some(name) => format!("Open in {name}"),
        None => format!("Open in {}", mode.what()),
    }
}

fn aim(ui: &Ui, target: &Target) -> Option<(Mode, std::result::Result<Action, String>)> {
    match target {
        Target::Conflict(path) => merge_aim(ui, path),
        Target::File { path, side } => {
            let sides = match side {
                Side::Unstaged | Side::Staged => {
                    let staged = *side == Side::Staged;
                    let name = named(path);
                    if let Some(row) = ui.files.iter().find(|row| row.staged == staged && row.path == *path) {
                        if row.status == "Conflicted" {
                            return merge_aim(ui, path);
                        }
                        if row.status == "Untracked" {
                            return Some((Mode::Diff, Err(format!("{name} is new, so there is nothing to compare it with"))));
                        }
                        if row.submodule.is_some() {
                            return Some((Mode::Diff, Err(format!("{name} is a submodule, which has no file to compare"))));
                        }
                    }
                    if staged { Sides::Staged } else { Sides::Unstaged }
                }
                Side::Commit(oid) | Side::Stash(oid) => Sides::Commit(oid.clone()),
                Side::Compare(to) => match &ui.selection {
                    Some(Selection::Compare { from, to: shown, since_fork }) if shown == to => {
                        Sides::Between { from: from.clone(), to: to.clone(), since_fork: *since_fork }
                    }
                    _ => return None,
                },
            };
            Some((Mode::Diff, Ok(Action::Diff { path: path.clone(), sides })))
        }
        _ => None,
    }
}

fn merge_aim(ui: &Ui, path: &Path) -> Option<(Mode, std::result::Result<Action, String>)> {
    let snapshot = ui.operation.snapshot().filter(|_| ui.operation.active())?;
    let conflict = snapshot.conflict(path)?;
    let name = named(path);
    let both_gone = conflict.side(crate::merge::Side::Current).is_none() && conflict.side(crate::merge::Side::Incoming).is_none();
    let refusal = match conflict.shape {
        _ if both_gone => Some(format!("Neither side kept {name}, so there is nothing to merge")),
        Shape::Submodule => Some(format!("{name} is a submodule. Choose a version of it instead")),
        Shape::Symlink => Some(format!("{name} is a symbolic link. Choose a version of it instead")),
        Shape::DeletedByUs | Shape::DeletedByThem => Some(format!("{name} was deleted on one side. Keep it or delete it instead")),
        _ => None,
    }
    .or_else(|| commands::busy(ui))
    .or_else(|| ui.difftool.is_open(Mode::Merge, path).then(|| format!("{name} is already open in the merge tool")));
    Some((Mode::Merge, refusal.map_or_else(|| Ok(Action::Merge(path.to_path_buf())), Err)))
}

fn later<R: Send + 'static>(
    window: &mut Window,
    cx: &mut Context<Tab>,
    work: impl FnOnce() -> R + Send + 'static,
    done: impl FnOnce(&mut Tab, R, &mut Window, &mut Context<Tab>) + 'static,
) {
    let task = cx.background_executor().spawn(async move { activity::traced(work) });
    let tab = cx.weak_entity();
    window
        .spawn(cx, async move |cx| {
            let (result, trail) = task.await;
            let _ = cx.update(|window, cx| {
                tab.update(cx, |tab, cx| {
                    let outer = std::mem::replace(&mut tab.trail, trail);
                    done(tab, result, window, cx);
                    tab.trail = outer;
                })
            });
        })
        .detach();
}

fn outside<R: Send + 'static>(
    cx: &mut Context<Tab>,
    work: impl FnOnce() -> R + Send + 'static,
    done: impl FnOnce(&mut Tab, R, &mut Context<Tab>) + 'static,
) {
    let (send, receive) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = send.send_blocking(activity::traced(work));
    });
    cx.spawn(async move |tab, cx| {
        if let Ok((result, trail)) = receive.recv().await {
            let _ = tab.update(cx, |tab, cx| {
                let outer = std::mem::replace(&mut tab.trail, trail);
                done(tab, result, cx);
                tab.trail = outer;
            });
        }
    })
    .detach();
}

pub(crate) fn read(tab: &mut Tab, cx: &mut Context<Tab>) {
    let repo = tab.repo.clone();
    tab.ui.difftool.ticket += 1;
    let ticket = tab.ui.difftool.ticket;
    Tab::work(cx, move || configured(repo.as_ref()), move |tab, read, cx| {
        if tab.ui.difftool.ticket == ticket {
            tab.ui.difftool.configured = Some(read);
            cx.notify();
        }
    });
}

fn refuse(tab: &mut Tab, reason: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
    cx.notify();
}

fn failed(tab: &mut Tab, title: String, error: String, cx: &mut Context<Tab>) {
    let notice = Notice::new(Tone::Error, title, error)
        .offer("External tools…", crate::tools::tools_section())
        .logged(tab.trail.entry(true));
    tab.ui.notices.show(notice);
    cx.notify();
}

struct Prepared {
    call: Call,
    ends: Vec<String>,
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Choose(mode, id) => {
            let chosen = Some(id);
            match mode {
                Mode::Diff => tab.ui.difftool.diff = chosen.clone(),
                Mode::Merge => tab.ui.difftool.merge = chosen.clone(),
            }
            Settings::update(cx, |s| match mode {
                Mode::Diff => s.diff_tool = chosen,
                Mode::Merge => s.merge_tool = chosen,
            });
            cx.notify();
        }
        Action::Diff { path, sides } => open_diff(tab, path, sides, window, cx),
        Action::Merge(path) => open_merge(tab, path, window, cx),
    }
}

fn prepare(
    repo: &Repository,
    mode: Mode,
    chosen: Option<&str>,
    found: &Detected,
    sides: Option<&Sides>,
) -> std::result::Result<Prepared, Refusal> {
    let read = configured(Some(repo)).map_err(Refusal::Failed)?;
    let picked = pick(chosen, &found.comparers, read.of(mode));
    let call = call(mode, picked.as_ref())?;
    let ends = match sides {
        Some(sides) => ends(repo, sides).map_err(Refusal::Failed)?,
        None => Vec::new(),
    };
    Ok(Prepared { call, ends })
}

fn not_ready(tab: &mut Tab, mode: Mode, path: &Path, refusal: Refusal, window: &mut Window, cx: &mut Context<Tab>) {
    match refusal {
        Refusal::Choose(detail) => {
            let what = format!("a {}", mode.what());
            crate::tools::choose(&what, &detail, tab, window, cx);
        }
        Refusal::Failed(error) => {
            let title = format!("{} did not open in the {}", named(path), mode.what());
            failed(tab, title, error, cx);
        }
    }
}

fn open_diff(tab: &mut Tab, path: PathBuf, sides: Sides, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if let Err(reason) = validate_path(&path) {
        return refuse(tab, reason, cx);
    }
    let chosen = tab.ui.difftool.chosen(Mode::Diff).map(str::to_owned);
    let found = crate::tools::current();
    let epoch = tab.epoch;
    later(
        window,
        cx,
        move || prepare(&repo, Mode::Diff, chosen.as_deref(), &found, Some(&sides)).map(|prepared| (repo, prepared)),
        move |tab, prepared, window, cx| {
            if tab.epoch != epoch {
                return;
            }
            let (repo, prepared) = match prepared {
                Ok(prepared) => prepared,
                Err(refusal) => return not_ready(tab, Mode::Diff, &path, refusal, window, cx),
            };
            tab.ui.difftool.open.push((Mode::Diff, path.clone()));
            cx.notify();
            let name = prepared.call.name.clone();
            let shown = path.clone();
            outside(
                cx,
                move || diff(&repo, &prepared.call, &prepared.ends, &shown),
                move |tab, result, cx| {
                    tab.ui.difftool.close(Mode::Diff, &path);
                    cx.notify();
                    if tab.epoch != epoch {
                        return;
                    }
                    if let Err(error) = result {
                        failed(tab, format!("{name} did not show {}", named(&path)), error, cx);
                    }
                },
            );
        },
    );
}

fn open_merge(tab: &mut Tab, path: PathBuf, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if tab.ui.busy() {
        return refuse(tab, "Wait for the current operation to finish".to_owned(), cx);
    }
    let name = named(&path);
    if tab.ui.difftool.is_open(Mode::Merge, &path) {
        return refuse(tab, format!("{name} is already open in the merge tool"), cx);
    }
    let Some(snapshot) = tab.ui.operation.snapshot().filter(|_| tab.ui.operation.active()) else {
        return refuse(tab, "Nothing is in progress".to_owned(), cx);
    };
    let Some(conflict) = snapshot.conflict(&path) else {
        return refuse(tab, format!("{name} is not in conflict"), cx);
    };
    let shown = Shown::of(snapshot, conflict);
    let chosen = tab.ui.difftool.chosen(Mode::Merge).map(str::to_owned);
    let found = crate::tools::current();
    let epoch = tab.epoch;
    later(
        window,
        cx,
        move || prepare(&repo, Mode::Merge, chosen.as_deref(), &found, None).map(|prepared| (repo, prepared)),
        move |tab, prepared, window, cx| {
            if tab.epoch != epoch {
                return;
            }
            let (repo, prepared) = match prepared {
                Ok(prepared) => prepared,
                Err(refusal) => return not_ready(tab, Mode::Merge, &path, refusal, window, cx),
            };
            if tab.ui.difftool.is_open(Mode::Merge, &path) {
                return refuse(tab, format!("{name} is already open in the merge tool"), cx);
            }
            tab.ui.difftool.open.push((Mode::Merge, path.clone()));
            cx.notify();
            let tool = prepared.call.name.clone();
            outside(
                cx,
                move || crate::conflict::merge_in_tool(&repo, &shown, &prepared.call.settings),
                move |tab, result, cx| {
                    tab.ui.difftool.close(Mode::Merge, &path);
                    cx.notify();
                    if tab.epoch != epoch {
                        return;
                    }
                    tab.refresh(false, cx);
                    let (tone, title, detail) = match result {
                        Ok(Tooled::Resolved) => (
                            Tone::Success,
                            format!("Resolved {name}"),
                            format!("{tool} saved the merged file, and it is marked resolved."),
                        ),
                        Ok(Tooled::Unchanged) => (
                            Tone::Info,
                            format!("{name} is still in conflict"),
                            format!("{tool} closed without saving a merged file."),
                        ),
                        Ok(Tooled::Stopped(said)) => return failed(tab, format!("{tool} did not merge {name}"), said, cx),
                        Err(error) => return failed(tab, format!("{name} did not open in the merge tool"), error, cx),
                    };
                    tab.ui.notices.show(Notice::new(tone, title, detail));
                    cx.notify();
                },
            );
        },
    );
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let found = crate::tools::detected();
    let mut page = div().flex().flex_col();
    for (mode, heading) in [(Mode::Diff, "DIFF TOOL"), (Mode::Merge, "MERGE TOOL")] {
        page = page.child(crate::preferences::group(heading));
        let Some(found) = &found else {
            page = page.child(div().py_2().child(kit::muted("Looking for tools…")));
            continue;
        };
        let shown = ui.difftool.pick(mode, found);
        let tag = match mode {
            Mode::Diff => "tools-diff",
            Mode::Merge => "tools-merge",
        };
        if found.comparers.is_empty() {
            page = page.child(div().py_2().child(kit::muted(
                "No diff or merge tool was found on your search path or in Applications. FastKraken looks for FileMerge, Kaleidoscope, Beyond Compare, Sublime Merge, P4Merge and VS Code.",
            )));
        }
        for (index, tool) in found.comparers.iter().enumerate() {
            let on = matches!(&shown, Some(Pick::Found(chosen)) if chosen == tool);
            let place = crate::start::homely(tool.place(), home.as_deref());
            page = page.child(crate::tools::choice(
                (tag, index),
                tool.tool.name.to_owned(),
                Some(place),
                on,
                view::Action::DiffTool(Action::Choose(mode, tool.tool.id.to_owned())),
                cx,
            ));
        }
        if let Some(Pick::Gone(tool)) = &shown {
            page = page.child(crate::tools::choice(
                (tag, found.comparers.len()),
                tool.name.to_owned(),
                Some("Not found any more".to_owned()),
                true,
                view::Action::DiffTool(Action::Choose(mode, tool.id.to_owned())),
                cx,
            ));
        }
        let git_said = match &ui.difftool.configured {
            None => "Reading Git's settings…".to_owned(),
            Some(Err(error)) => error.clone(),
            Some(Ok(configured)) => match configured.of(mode) {
                Some(name) => format!("Git uses {name}"),
                None => format!("Git has no {} set", mode.what()),
            },
        };
        page = page.child(crate::tools::choice(
            (tag, found.comparers.len() + 1),
            "Whatever Git is configured to use".to_owned(),
            Some(git_said),
            matches!(&shown, Some(Pick::Git(_) | Pick::Unset)),
            view::Action::DiffTool(Action::Choose(mode, GIT.to_owned())),
            cx,
        ));
        if let Some(Pick::Found(chosen)) = &shown
            && let Some(command) = runs(mode, chosen)
        {
            page = page.child(
                div()
                    .pt_2()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(TEXT_3))
                    .child(format!("Runs {command}")),
            );
        }
    }
    page.child(div().pt_3().child(kit::muted(
        "Cmd-D sends the selected file's change to the diff tool, or a conflicted file to the merge tool. The tool is passed to Git for that one call, so your Git settings are not changed, and a merge leaves no .orig backup behind.",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{snapshot, tests::{commit, oid, stops, write}};
    use crate::repository::tests::Fixture;
    use crate::tools::How;
    use std::{fs, os::unix::fs::PermissionsExt};

    struct Bench {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        log: PathBuf,
    }

    impl Bench {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("tool bin");
            fs::create_dir_all(&bin).unwrap();
            let log = dir.path().join("tool.log");
            Self { bin, log, _dir: dir }
        }

        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.bin.join(name);
            fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}", self.log.display())).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn found(&self, id: &str, program: &Path) -> Found {
            Found { tool: &tool(id).unwrap().known, how: How::Program(program.to_owned()) }
        }

        fn logged(&self) -> String {
            fs::read_to_string(&self.log).unwrap_or_default()
        }
    }

    fn smerge_resolving(bench: &Bench, text: &str) -> Found {
        let body = format!(
            "[ \"$1\" = mergetool ] || exit 2\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; *) shift;; esac; done\nprintf '{text}' > \"$out\"\n"
        );
        let program = bench.script("smerge", &body);
        bench.found("smerge", &program)
    }

    fn conflicted() -> Fixture {
        let f = Fixture::new();
        commit(&f, "ridge.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "ridge.txt", "side\n", "Side");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "ridge.txt", "main\n", "Main");
        stops(&f, &["merge", "side"]);
        f
    }

    fn shown(f: &Fixture, path: &str) -> Shown {
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        Shown::of(&snapshot, snapshot.conflict(Path::new(path)).unwrap())
    }

    fn unmerged(f: &Fixture, path: &str) -> Vec<u8> {
        f.git(&["ls-files", "--unmerged", "--", path])
    }

    fn untracked(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "--untracked-files=all", "--ignored"])).unwrap()
    }

    fn merge_call(found: &Found) -> Call {
        call(Mode::Merge, Some(&Pick::Found(found.clone()))).unwrap()
    }

    #[test]
    fn a_chosen_merge_tool_writes_the_resolution_which_is_staged_with_no_backup_left() {
        let f = conflicted();
        let bench = Bench::new();
        let found = smerge_resolving(&bench, "agreed\\n");
        let config = fs::read(f.repo.git_dir().join("config")).unwrap();
        let refs = f.git(&["for-each-ref"]);
        f.git(&["config", "--local", "mergetool.keepBackup", "true"]);
        let tooled = crate::conflict::merge_in_tool(&f.repo, &shown(&f, "ridge.txt"), &merge_call(&found).settings).unwrap();
        assert_eq!(tooled, Tooled::Resolved);
        assert!(unmerged(&f, "ridge.txt").is_empty());
        assert_eq!(f.git(&["show", ":ridge.txt"]), b"agreed\n");
        assert_eq!(fs::read_to_string(f.dir.join("ridge.txt")).unwrap(), "agreed\n");
        assert_eq!(untracked(&f), "M  ridge.txt\n");
        assert!(!f.dir.join("ridge.txt.orig").exists());
        assert_eq!(f.git(&["for-each-ref"]), refs);
        f.git(&["config", "--local", "--unset", "mergetool.keepBackup"]);
        assert_eq!(fs::read(f.repo.git_dir().join("config")).unwrap(), config);
        let logged = bench.logged();
        let args: Vec<&str> = logged.trim_end().split(' ').collect();
        assert_eq!((args.len(), args[0], &args[4..]), (6, "mergetool", &["-o", "ridge.txt"][..]), "{logged}");
        assert!(args[1].contains("ridge_BASE_") && args[2].contains("ridge_LOCAL_") && args[3].contains("ridge_REMOTE_"), "{logged}");
        assert!(!args[1].starts_with(&*f.dir.to_string_lossy()), "{logged}");
    }

    #[test]
    fn a_merge_tool_closed_without_saving_leaves_the_conflict() {
        let f = conflicted();
        let bench = Bench::new();
        let program = bench.script("smerge", "exit 0\n");
        let found = bench.found("smerge", &program);
        let before = unmerged(&f, "ridge.txt");
        let tooled = crate::conflict::merge_in_tool(&f.repo, &shown(&f, "ridge.txt"), &merge_call(&found).settings).unwrap();
        assert_eq!(tooled, Tooled::Unchanged);
        assert_eq!(unmerged(&f, "ridge.txt"), before);
        assert_eq!(untracked(&f), "UU ridge.txt\n");
        let gone = bench.found("smerge", &bench.bin.join("missing-smerge"));
        let tooled = crate::conflict::merge_in_tool(&f.repo, &shown(&f, "ridge.txt"), &merge_call(&gone).settings).unwrap();
        match tooled {
            Tooled::Stopped(said) => assert!(said.contains("missing-smerge") && !said.contains("line "), "{said}"),
            other => panic!("A missing tool gave {other:?}"),
        }
        assert_eq!(unmerged(&f, "ridge.txt"), before);
    }

    #[test]
    fn the_merge_tool_is_not_started_on_a_conflict_that_changed() {
        let f = conflicted();
        let bench = Bench::new();
        let found = smerge_resolving(&bench, "agreed\\n");
        let settings = merge_call(&found).settings;
        let stale = shown(&f, "ridge.txt");
        f.git(&["checkout", "--theirs", "--", "ridge.txt"]);
        f.git(&["add", "--", "ridge.txt"]);
        let err = crate::conflict::merge_in_tool(&f.repo, &stale, &settings).unwrap_err();
        assert_eq!(err, "ridge.txt is no longer in conflict.");
        f.git(&["update-index", "--unresolve", "--", "ridge.txt"]);
        let base = oid(&f, ":1:ridge.txt");
        let changed = f.repo.command().args(["update-index", "--index-info"]).stdin(std::process::Stdio::piped()).spawn().map(|mut child| {
            use std::io::Write;
            let line = format!("100644 {base} 3\tridge.txt\n");
            child.stdin.take().unwrap().write_all(line.as_bytes()).unwrap();
            child.wait().unwrap()
        });
        assert!(changed.unwrap().success());
        let err = crate::conflict::merge_in_tool(&f.repo, &stale, &settings).unwrap_err();
        assert_eq!(err, "ridge.txt changed since the window showed it. Look at it again.");
        assert_eq!(bench.logged(), "");
        assert_eq!(fs::read_to_string(f.dir.join("ridge.txt")).unwrap(), "side\n");
    }

    #[test]
    fn git_s_own_merge_tool_is_used_with_the_backup_switched_off() {
        let f = conflicted();
        let bench = Bench::new();
        let program = bench.script("trail-merge", "printf 'by git\\n' > \"$1\"\n");
        f.git(&["config", "--local", "merge.tool", "trailmerge"]);
        let command = format!("{} \"$MERGED\"", keys::quoted(&program.to_string_lossy()));
        f.git(&["config", "--local", "mergetool.trailmerge.cmd", &command]);
        f.git(&["config", "--local", "mergetool.trailmerge.trustExitCode", "true"]);
        f.git(&["config", "--local", "mergetool.keepBackup", "true"]);
        let read = configured(Some(&f.repo)).unwrap();
        assert_eq!(read, Configured { diff: Some("trailmerge".into()), merge: Some("trailmerge".into()) });
        let picked = pick(None, &[], read.of(Mode::Merge));
        assert_eq!(picked, Some(Pick::Git("trailmerge".into())));
        let call = call(Mode::Merge, picked.as_ref()).unwrap();
        assert_eq!(call.settings, ["mergetool.keepBackup=false", "mergetool.writeToTemp=true"]);
        let tooled = crate::conflict::merge_in_tool(&f.repo, &shown(&f, "ridge.txt"), &call.settings).unwrap();
        assert_eq!(tooled, Tooled::Resolved);
        assert_eq!(f.git(&["show", ":ridge.txt"]), b"by git\n");
        assert_eq!(untracked(&f), "M  ridge.txt\n");
        assert_eq!(bench.logged(), "ridge.txt\n");
    }

    fn sides_logged(bench: &Bench) -> Found {
        let body = "[ \"$1\" = mergetool ] || exit 2\nprintf '%s|%s\\n' \"$(cat \"$2\")\" \"$(cat \"$3\")\" >> \"$(dirname \"$0\")/../sides.log\"\n";
        let program = bench.script("smerge", body);
        bench.found("smerge", &program)
    }

    #[test]
    fn the_diff_tool_is_given_both_sides_of_each_kind_of_change() {
        let f = Fixture::new();
        let root = commit(&f, "ridge.txt", "one\n", "One");
        let two = commit(&f, "ridge.txt", "two\n", "Two");
        commit(&f, "ridge.txt", "three\n", "Three");
        write(&f, "ridge.txt", "staged\n");
        f.git(&["add", "--", "ridge.txt"]);
        write(&f, "ridge.txt", "working\n");
        let bench = Bench::new();
        let found = sides_logged(&bench);
        let diff_call = call(Mode::Diff, Some(&Pick::Found(found))).unwrap();
        let config = fs::read(f.repo.git_dir().join("config")).unwrap();
        let index = f.git(&["ls-files", "--stage"]);
        let refs = f.git(&["for-each-ref"]);
        let ridge = Path::new("ridge.txt");
        for sides in [
            Sides::Unstaged,
            Sides::Staged,
            Sides::Commit(two.clone()),
            Sides::Commit(root.clone()),
            Sides::Between { from: Rev::Commit(root.clone()), to: Rev::WorkingTree, since_fork: false },
        ] {
            let ends = ends(&f.repo, &sides).unwrap();
            diff(&f.repo, &diff_call, &ends, ridge).unwrap();
        }
        let sides = fs::read_to_string(bench.bin.join("../sides.log")).unwrap();
        assert_eq!(sides, "staged|working\nthree|staged\none|two\n|one\none|working\n");
        assert_eq!(fs::read(f.repo.git_dir().join("config")).unwrap(), config);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(fs::read_to_string(f.dir.join("ridge.txt")).unwrap(), "working\n");
        assert_eq!(untracked(&f), "MM ridge.txt\n");
    }

    #[test]
    fn a_diff_with_nothing_left_to_compare_is_refused_without_starting_the_tool() {
        let f = Fixture::new();
        commit(&f, "ridge.txt", "one\n", "One");
        write(&f, "ridge.txt", "two\n");
        let bench = Bench::new();
        let found = sides_logged(&bench);
        let diff_call = call(Mode::Diff, Some(&Pick::Found(found))).unwrap();
        f.git(&["add", "--", "ridge.txt"]);
        let err = diff(&f.repo, &diff_call, &[], Path::new("ridge.txt")).unwrap_err();
        assert_eq!(err, "ridge.txt has no changes to compare any more.");
        assert_eq!(bench.logged(), "");
        assert!(diff(&f.repo, &diff_call, &[], Path::new("../ridge.txt")).is_err());
        let gone = Sides::Commit("0".repeat(40));
        assert_eq!(ends(&f.repo, &gone), Err("That commit is no longer in this repository.".to_owned()));
        let failing = bench.script("failing-smerge", "echo 'no window for you' >&2\nexit 3\n");
        let failing = call(Mode::Diff, Some(&Pick::Found(bench.found("smerge", &failing)))).unwrap();
        let err = diff(&f.repo, &failing, &["--cached".to_owned()], Path::new("ridge.txt")).unwrap_err();
        assert!(err.contains("no window for you"), "{err}");
        assert_eq!(untracked(&f), "M  ridge.txt\n");
    }

    #[test]
    fn the_tool_is_the_chosen_one_then_git_s_then_the_first_found() {
        let bench = Bench::new();
        let code = bench.found("code", &bench.bin.join("code"));
        let smerge = bench.found("smerge", &bench.bin.join("smerge"));
        let found = [code.clone(), smerge.clone()];
        assert_eq!(pick(None, &found, None), Some(Pick::Found(code.clone())));
        assert_eq!(pick(None, &found, Some("meld")), Some(Pick::Git("meld".into())));
        assert_eq!(pick(None, &found, Some("  ")), Some(Pick::Found(code.clone())));
        assert_eq!(pick(Some("smerge"), &found, Some("meld")), Some(Pick::Found(smerge)));
        assert_eq!(pick(Some("bcomp"), &found, None), Some(Pick::Gone(&tool("bcomp").unwrap().known)));
        assert_eq!(pick(Some("nonesuch"), &found, None), Some(Pick::Found(code)));
        assert_eq!(pick(Some(GIT), &found, Some("meld")), Some(Pick::Git("meld".into())));
        assert_eq!(pick(Some(GIT), &found, None), Some(Pick::Unset));
        assert_eq!(pick(None, &[], None), None);
    }

    #[test]
    fn a_chosen_tool_is_passed_for_the_one_call_with_its_path_quoted() {
        let program = PathBuf::from("/Applications/Sublime Merge.app/Contents/SharedSupport/bin/smerge");
        let found = Found { tool: &tool("smerge").unwrap().known, how: How::Program(program) };
        let diff = call(Mode::Diff, Some(&Pick::Found(found.clone()))).unwrap();
        assert_eq!(diff.name, "Sublime Merge");
        assert_eq!(
            diff.settings,
            [
                "diff.tool=fastkraken-smerge".to_owned(),
                r#"difftool.fastkraken-smerge.cmd='/Applications/Sublime Merge.app/Contents/SharedSupport/bin/smerge' mergetool "$LOCAL" "$REMOTE""#.to_owned(),
            ]
        );
        let merge = call(Mode::Merge, Some(&Pick::Found(found))).unwrap();
        assert_eq!(merge.settings[0], "merge.tool=fastkraken-smerge");
        assert!(merge.settings[1].starts_with(r#"mergetool.fastkraken-smerge.cmd=before=$(git hash-object -- "$MERGED"); '/Applications/Sublime Merge.app/"#));
        assert_eq!(&merge.settings[2..], ["mergetool.fastkraken-smerge.trustExitCode=true", "mergetool.keepBackup=false", "mergetool.writeToTemp=true"]);
        let kaleidoscope = Found { tool: &tool("kaleidoscope").unwrap().known, how: How::Program("/usr/local/bin/ksdiff".into()) };
        assert_eq!(
            runs(Mode::Merge, &kaleidoscope).unwrap(),
            r#"/usr/local/bin/ksdiff --merge --output "$MERGED" --base "$BASE" -- "$LOCAL" --snapshot "$REMOTE" --snapshot"#
        );
        assert!(matches!(call(Mode::Diff, None), Err(Refusal::Choose(_))));
        assert_eq!(call(Mode::Merge, Some(&Pick::Unset)), Err(Refusal::Choose("Git has no merge tool set.".into())));
        assert!(matches!(call(Mode::Merge, Some(&Pick::Git("vimdiff3".into()))), Err(Refusal::Choose(_))));
        assert_eq!(call(Mode::Diff, Some(&Pick::Gone(&tool("p4merge").unwrap().known))), Err(Refusal::Failed("P4Merge is no longer installed.".into())));
        assert_eq!(call(Mode::Diff, Some(&Pick::Git("meld".into()))).unwrap(), Call { mode: Mode::Diff, name: "meld".into(), settings: Vec::new() });
    }

    #[test]
    fn diff_and_merge_tools_are_found_only_where_there_is_a_program_to_run() {
        let bench = Bench::new();
        let apps = bench.bin.join("../Applications");
        let app = |name: &str| {
            let app = apps.join(name);
            fs::create_dir_all(app.join("Contents")).unwrap();
            app
        };
        let program = |path: PathBuf| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let detect = || crate::tools::detect(&[bench.bin.clone()], &[apps.clone()]).comparers;
        assert!(detect().is_empty());
        let xcode = app("Xcode.app");
        let opendiff = program(xcode.join("Contents/Developer/usr/bin/opendiff"));
        app("Kaleidoscope.app");
        app("Beyond Compare.app");
        let bcomp = program(app("Beyond Compare.app").join("Contents/MacOS/bcomp"));
        let code = program(bench.bin.join("code"));
        let found = detect();
        let ids: Vec<&str> = found.iter().map(|found| found.tool.id).collect();
        assert_eq!(ids, ["filemerge", "bcomp", "code"]);
        assert_eq!(found[0].how, How::Program(opendiff));
        assert_eq!(found[1].how, How::Program(bcomp));
        assert_eq!(found[2].how, How::Program(code));
        let editors: Vec<&str> = crate::tools::detect(&[bench.bin.clone()], &[apps.clone()]).editors.iter().map(|found| found.tool.id).collect();
        assert_eq!(editors, ["code", "xed"]);
    }

    #[test]
    fn git_s_diff_tool_falls_back_to_its_merge_tool() {
        let f = Fixture::new();
        assert_eq!(configured(Some(&f.repo)).unwrap(), Configured::default());
        f.git(&["config", "--local", "merge.tool", "meld"]);
        assert_eq!(configured(Some(&f.repo)).unwrap(), Configured { diff: Some("meld".into()), merge: Some("meld".into()) });
        f.git(&["config", "--local", "diff.tool", "kdiff3"]);
        assert_eq!(configured(Some(&f.repo)).unwrap(), Configured { diff: Some("kdiff3".into()), merge: Some("meld".into()) });
    }
}
