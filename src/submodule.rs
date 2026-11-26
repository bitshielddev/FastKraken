use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Side, Target, busy, networking, on_repo, operating},
    jobs,
    notice::{self, Notice, Outcome},
    repository::{Gitlink, Repository, checked, git_error, raw_path, run, validate_oid, validate_path, with_system_tools},
    settings::{self, Memory},
    theme::*,
    view::{self, Desk, Menu, Selection, Tone, Ui},
};
use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, div, prelude::*, px, rgb};
use std::{
    path::{Path, PathBuf},
    process::Command as Process,
};

const LISTED: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Module {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) recorded: Option<String>,
    pub(crate) checked_out: Option<String>,
    pub(crate) populated: bool,
}

impl Module {
    pub(crate) fn shown(&self) -> String {
        self.path.display().to_string()
    }

    pub(crate) fn in_sync(&self) -> bool {
        self.populated && self.recorded.is_some() && self.checked_out == self.recorded
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) oid: String,
    pub(crate) subject: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Details {
    pub(crate) path: PathBuf,
    pub(crate) staged: bool,
    pub(crate) populated: bool,
    pub(crate) from: Option<Commit>,
    pub(crate) to: Option<Commit>,
    pub(crate) ahead: Vec<Commit>,
    pub(crate) ahead_count: usize,
    pub(crate) behind: Vec<Commit>,
    pub(crate) behind_count: usize,
    pub(crate) missing: Vec<String>,
    pub(crate) changes: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct Panel {
    path: PathBuf,
    staged: bool,
    pub(crate) details: Option<Result<Details, String>>,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) list: Vec<Module>,
    pub(crate) behind: Vec<PathBuf>,
    pub(crate) automatic: bool,
    ticket: u64,
    seen: Option<String>,
    moved: Option<(String, String)>,
    panel: Option<Panel>,
    panel_ticket: u64,
}

impl State {
    pub(crate) fn find(&self, path: &Path) -> Option<&Module> {
        self.list.iter().find(|module| module.path == path)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(PathBuf),
    Show(PathBuf),
    Update(PathBuf),
    UpdateAll,
    Dismiss,
    ToggleAutomatic,
}

fn inside(repo: &Repository, path: &Path) -> Process {
    let mut command = repo.read_command();
    command.arg("-C").arg(repo.root.join(path));
    command
}

fn populated(repo: &Repository, path: &Path) -> bool {
    std::fs::symlink_metadata(repo.root.join(path).join(".git")).is_ok()
}

pub(crate) fn declared(repo: &Repository) -> Result<Vec<(String, PathBuf)>, String> {
    let file = repo.root.join(".gitmodules");
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let output = run(
        repo.read_command()
            .args(["config", "-z", "--file"])
            .arg(&file)
            .args(["--get-regexp", r"^submodule\..*\.path$"]),
    )
    .map_err(|error| format!("Run bundled Git: {error}"))?;
    match output.status.code() {
        Some(0) => {}
        Some(1) => return Ok(Vec::new()),
        _ => return Err(git_error(&output.stderr, output.status)),
    }
    let mut found = Vec::new();
    for record in output.stdout.split(|&b| b == 0).filter(|record| !record.is_empty()) {
        let Some(at) = record.iter().position(|&b| b == b'\n') else { continue };
        let Some(name) = record[..at].strip_prefix(b"submodule.").and_then(|key| key.strip_suffix(b".path")) else {
            continue;
        };
        let path = raw_path(&record[at + 1..]);
        if validate_path(&path).is_err() || found.iter().any(|(_, known): &(String, PathBuf)| *known == path) {
            continue;
        }
        found.push((String::from_utf8_lossy(name).into_owned(), path));
    }
    Ok(found)
}

fn gitlinks(output: &[u8]) -> Vec<(PathBuf, String, u8)> {
    output
        .split(|&b| b == 0)
        .filter_map(|record| {
            let tab = record.iter().position(|&b| b == b'\t')?;
            let mut fields = record[..tab].split(|&b| b == b' ');
            if fields.next()? != b"160000" {
                return None;
            }
            let oid = String::from_utf8_lossy(fields.next()?).into_owned();
            let stage = fields.next().and_then(|stage| stage.first().copied()).unwrap_or(b'0');
            Some((raw_path(&record[tab + 1..]), oid, stage))
        })
        .collect()
}

fn recorded_in_index(repo: &Repository, paths: &[PathBuf]) -> Result<Vec<(PathBuf, String, u8)>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let output = checked(repo.read_command().args(["ls-files", "--stage", "-z", "--"]).args(paths))?;
    Ok(gitlinks(&output))
}

fn recorded_in_head(repo: &Repository, path: &Path) -> Result<Option<String>, String> {
    let output = run(repo.read_command().args(["ls-tree", "-z", "HEAD", "--"]).arg(path))
        .map_err(|error| format!("Run bundled Git: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let found = output.stdout.split(|&b| b == 0).find_map(|record| {
        let tab = record.iter().position(|&b| b == b'\t')?;
        let fields: Vec<&[u8]> = record[..tab].split(|&b| b == b' ').collect();
        (fields.len() == 3 && fields[0] == b"160000" && raw_path(&record[tab + 1..]) == path)
            .then(|| String::from_utf8_lossy(fields[2]).into_owned())
    });
    Ok(found)
}

fn checked_out(repo: &Repository, path: &Path) -> Option<String> {
    if !populated(repo, path) {
        return None;
    }
    let output = checked(inside(repo, path).args(["rev-parse", "--verify", "-q", "HEAD"])).ok()?;
    let oid = String::from_utf8_lossy(&output).trim().to_owned();
    validate_oid(&oid).is_ok().then_some(oid)
}

pub(crate) fn list(repo: &Repository) -> Result<Vec<Module>, String> {
    let declared = declared(repo)?;
    let paths: Vec<PathBuf> = declared.iter().map(|(_, path)| path.clone()).collect();
    let recorded = recorded_in_index(repo, &paths)?;
    Ok(declared
        .into_iter()
        .filter_map(|(name, path)| {
            let mut stages = recorded.iter().filter(|(listed, _, _)| *listed == path);
            let first = stages.next()?;
            let recorded = (first.2 == b'0').then(|| first.1.clone());
            let populated = populated(repo, &path);
            let checked_out = checked_out(repo, &path);
            Some(Module { path, name, recorded, checked_out, populated })
        })
        .collect())
}

pub(crate) fn changed_between(repo: &Repository, old: &str, new: &str) -> Result<Vec<PathBuf>, String> {
    validate_oid(old)?;
    validate_oid(new)?;
    let output = checked(repo.read_command().args(["diff-tree", "-r", "-z", "--no-renames", "--raw", old, new]))?;
    let mut found = Vec::new();
    let mut records = output.split(|&b| b == 0);
    while let Some(meta) = records.next() {
        if meta.is_empty() {
            continue;
        }
        let Some(path) = records.next() else { break };
        let mut fields = meta.strip_prefix(b":").unwrap_or(meta).split(|&b| b == b' ');
        if fields.nth(1) == Some(b"160000".as_slice()) {
            found.push(raw_path(path));
        }
    }
    Ok(found)
}

fn subject(repo: &Repository, path: &Path, oid: &str) -> Option<String> {
    let output = checked(inside(repo, path).args(["log", "-1", "--no-show-signature", "--format=%s", oid, "--"])).ok()?;
    Some(String::from_utf8_lossy(&output).trim_end().to_owned())
}

fn present(repo: &Repository, path: &Path, oid: &str) -> bool {
    run(inside(repo, path).args(["cat-file", "-e", &format!("{oid}^{{commit}}")])).is_ok_and(|output| output.status.success())
}

fn between(repo: &Repository, path: &Path, from: &str, to: &str) -> Result<(Vec<Commit>, usize), String> {
    let range = format!("{from}..{to}");
    let count = checked(inside(repo, path).args(["rev-list", "--count", &range, "--"]))?;
    let count = String::from_utf8_lossy(&count).trim().parse().unwrap_or(0);
    if count == 0 {
        return Ok((Vec::new(), 0));
    }
    let listed = checked(inside(repo, path).args([
        "log",
        "--no-show-signature",
        "--no-decorate",
        &format!("-n{LISTED}"),
        "--format=%H%x1f%s",
        &range,
        "--",
    ]))?;
    let commits = String::from_utf8_lossy(&listed)
        .lines()
        .filter_map(|line| {
            let (oid, subject) = line.split_once('\u{1f}')?;
            Some(Commit { oid: oid.to_owned(), subject: Some(subject.to_owned()) })
        })
        .collect();
    Ok((commits, count))
}

pub(crate) fn details(repo: &Repository, path: &Path, staged: bool) -> Result<Details, String> {
    validate_path(path)?;
    let in_index = recorded_in_index(repo, &[path.to_owned()])?
        .into_iter()
        .find(|(listed, _, stage)| listed == path && *stage == b'0')
        .map(|(_, oid, _)| oid);
    let in_head = recorded_in_head(repo, path)?;
    if in_index.is_none() && in_head.is_none() {
        return Err(format!("{} is no longer a submodule here", path.display()));
    }
    let populated = populated(repo, path);
    let (from, to) = if staged { (in_head, in_index) } else { (in_index, checked_out(repo, path)) };
    let mut details = Details { path: path.to_owned(), staged, populated, ..Details::default() };
    let known = |oid: &Option<String>| oid.as_ref().filter(|oid| populated && present(repo, path, oid)).is_some();
    let (from_known, to_known) = (known(&from), known(&to));
    if populated {
        details.missing = [(&from, from_known), (&to, to_known)]
            .into_iter()
            .filter_map(|(oid, known)| oid.clone().filter(|_| !known))
            .collect();
    }
    let commit = |oid: Option<String>, known: bool| {
        oid.map(|oid| Commit { subject: known.then(|| subject(repo, path, &oid)).flatten(), oid })
    };
    if let (Some(old), Some(new), true, true) = (&from, &to, from_known, to_known) {
        (details.ahead, details.ahead_count) = between(repo, path, old, new)?;
        (details.behind, details.behind_count) = between(repo, path, new, old)?;
    }
    details.from = commit(from, from_known);
    details.to = commit(to, to_known);
    if populated && !staged {
        details.changes = crate::worktree::uncommitted(repo, &repo.root.join(path)).ok();
    }
    Ok(details)
}

fn configured(command: &mut Process) -> Option<String> {
    let output = run(command).ok().filter(|output| output.status.success())?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn checks_out(repo: &Repository, module: &Module) -> bool {
    let key = format!("submodule.{}.update", module.name);
    let declared = repo.root.join(".gitmodules");
    configured(repo.read_command().args(["config", "--get", &key]))
        .or_else(|| configured(repo.read_command().args(["config", "--file"]).arg(&declared).args(["--get", &key])))
        .is_none_or(|mode| mode == "checkout")
}

fn left_behind(repo: &Repository, module: &Module) -> Result<usize, String> {
    let Some(head) = module.checked_out.as_deref().filter(|_| module.populated) else { return Ok(0) };
    if !checks_out(repo, module) {
        return Ok(0);
    }
    let mut command = inside(repo, &module.path);
    command.args(["rev-list", "--count", head, "--not", "--branches", "--tags", "--remotes"]);
    if let Some(recorded) = module.recorded.as_deref().filter(|oid| present(repo, &module.path, oid)) {
        command.arg(recorded);
    }
    let output = checked(command.arg("--"))?;
    Ok(String::from_utf8_lossy(&output).trim().parse().unwrap_or(0))
}

fn stranded(module: &Module, count: usize) -> String {
    format!(
        "Submodule {} has {} on no branch. Open it and create a branch there first, or updating leaves {} behind.",
        module.shown(),
        notice::commits(count),
        if count == 1 { "it" } else { "them" }
    )
}

fn fresh(repo: &Repository, path: &Path) -> Result<Module, String> {
    list(repo)?
        .into_iter()
        .find(|module| module.path == path)
        .ok_or_else(|| format!("{} is no longer a submodule here", path.display()))
}

pub(crate) fn update(repo: &Repository, expected: &Module) -> Result<Outcome, String> {
    let found = fresh(repo, &expected.path)?;
    if found != *expected {
        return Err(format!("Submodule {} changed since it was shown. Look again, then update it.", expected.shown()));
    }
    let Some(recorded) = found.recorded.clone() else {
        return Err(format!("Submodule {} has a conflict. Resolve it first.", expected.shown()));
    };
    if found.in_sync() {
        return Err(format!("Submodule {} is already at the recorded commit", expected.shown()));
    }
    let count = left_behind(repo, &found)?;
    if count > 0 {
        return Err(stranded(&found, count));
    }
    checked(with_system_tools(&mut repo.network_command()?).args(["submodule", "update", "--init", "--progress", "--"]).arg(&found.path))?;
    let now = fresh(repo, &found.path)?;
    if now.checked_out.as_deref() != Some(recorded.as_str()) {
        return Err(format!("Git did not move submodule {} to {}", found.shown(), notice::short(&recorded)));
    }
    let title = if found.populated {
        format!("Updated submodule {}", found.shown())
    } else {
        format!("Initialised submodule {}", found.shown())
    };
    let mut detail = format!("It is at {}, the commit this repository records.", notice::short(&recorded));
    if let Some(was) = found.checked_out.as_deref() {
        detail.push_str(&format!(" It was at {}.", notice::short(was)));
    }
    Ok(Outcome::titled(title, detail))
}

pub(crate) fn update_all(repo: &Repository, expected: &[Module]) -> Result<Outcome, String> {
    let found = list(repo)?;
    if found != expected {
        return Err("The submodules changed since they were shown. Look again, then update them.".to_owned());
    }
    let waiting: Vec<&Module> = found.iter().filter(|module| !module.in_sync() && module.recorded.is_some()).collect();
    if waiting.is_empty() {
        return Err("Every submodule is already at the commit this branch expects".to_owned());
    }
    for module in &waiting {
        let count = left_behind(repo, module)?;
        if count > 0 {
            return Err(stranded(module, count));
        }
    }
    checked(with_system_tools(&mut repo.network_command()?).args(["submodule", "update", "--init", "--recursive", "--progress"]))?;
    let now = list(repo)?;
    let updated: Vec<String> = waiting
        .iter()
        .filter(|module| now.iter().any(|after| after.path == module.path && after.in_sync()))
        .map(|module| module.shown())
        .collect();
    let title = match updated.as_slice() {
        [] => return Err("Git updated no submodule".to_owned()),
        [one] => format!("Updated submodule {one}"),
        many => format!("Updated {} submodules", many.len()),
    };
    let left = waiting.len() - updated.len();
    let mut detail = format!("Now at the commits this branch expects: {}.", updated.join(", "));
    if left > 0 {
        detail.push_str(&format!(" {left} could not be updated."));
    }
    Ok(Outcome::titled(title, detail))
}

pub(crate) fn describe(flags: Gitlink, staged: bool) -> String {
    if staged {
        return "new commit staged".to_owned();
    }
    let parts: Vec<&str> = [(flags.moved, "new commits"), (flags.modified, "modified content"), (flags.untracked, "untracked content")]
        .into_iter()
        .filter_map(|(on, text)| on.then_some(text))
        .collect();
    parts.join(", ")
}

fn row_flags(ui: &Ui, path: &Path, staged: bool) -> Option<Gitlink> {
    ui.files.iter().find(|file| file.staged == staged && file.path == path).and_then(|file| file.submodule)
}

pub(crate) fn state_text(ui: &Ui, module: &Module) -> String {
    if !module.populated {
        return "Not initialised".to_owned();
    }
    if module.recorded.is_none() {
        return "Conflicted".to_owned();
    }
    let flags = row_flags(ui, &module.path, false).unwrap_or_default();
    let flags = Gitlink { moved: module.checked_out != module.recorded, ..flags };
    let text = describe(flags, false);
    if text.is_empty() {
        return "In sync".to_owned();
    }
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn module_of<'a>(ui: &'a Ui, target: &Target) -> Option<&'a Module> {
    match target {
        Target::Submodule(path) => ui.submodule.find(path),
        Target::File { path, side: side @ (Side::Unstaged | Side::Staged) } => {
            row_flags(ui, path, *side == Side::Staged)?;
            ui.submodule.find(path)
        }
        _ => None,
    }
}

fn update_refusal(ui: &Ui, module: &Module) -> Option<String> {
    if module.recorded.is_none() {
        return Some("It has a conflict. Resolve it first".to_owned());
    }
    if module.in_sync() {
        return Some("It is at the recorded commit".to_owned());
    }
    networking(ui).or_else(|| operating(ui)).or_else(|| busy(ui))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "submodule.open",
        Group::Switch,
        |_, _| "Open submodule".into(),
        |ui, target, _| match module_of(ui, target) {
            None => Availability::Hidden,
            Some(module) if !module.populated => Availability::Disabled("It is not initialised yet".to_owned()),
            Some(_) => Availability::Enabled,
        },
        |ui, target| module_of(ui, target).map(|module| view::Action::Submodule(Action::Open(module.path.clone()))),
    )
    .icon("external"),
    Command::new(
        "submodule.update",
        Group::Switch,
        |ui, target| match module_of(ui, target) {
            Some(module) if !module.populated => "Initialise".into(),
            _ => "Update to the recorded commit".into(),
        },
        |ui, target, _| match module_of(ui, target) {
            None => Availability::Hidden,
            Some(module) => Availability::unless(update_refusal(ui, module)),
        },
        |ui, target| module_of(ui, target).map(|module| view::Action::Submodule(Action::Update(module.path.clone()))),
    )
    .icon("fetch"),
    Command::new(
        "submodule.update-all",
        Group::Switch,
        |_, _| "Update submodules".into(),
        |ui, target, _| {
            if !on_repo(ui, target) || ui.submodule.list.is_empty() {
                return Availability::Hidden;
            }
            let waiting = ui.submodule.list.iter().any(|module| !module.in_sync() && module.recorded.is_some());
            Availability::unless(
                (!waiting)
                    .then(|| "Every submodule is at its recorded commit".to_owned())
                    .or_else(|| networking(ui))
                    .or_else(|| operating(ui))
                    .or_else(|| busy(ui)),
            )
        },
        |_, _| Some(view::Action::Submodule(Action::UpdateAll)),
    )
    .words(&["submodule update", "git submodule update", "initialise submodules"])
    .icon("fetch"),
];

pub(crate) fn restore(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    tab.ui.submodule.automatic =
        settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.update_submodules).unwrap_or(false);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => open(tab, path, cx),
        Action::Show(path) => show(tab, path, cx),
        Action::Dismiss => {
            tab.ui.submodule.behind.clear();
            cx.notify();
        }
        Action::ToggleAutomatic => {
            let on = !tab.ui.submodule.automatic;
            tab.ui.submodule.automatic = on;
            if let Some(repo) = &tab.repo {
                let common = repo.common_dir().to_owned();
                Memory::update(cx, |memory| memory.change_repo(&common, |view| view.update_submodules = on.then_some(true)));
            }
            cx.notify();
        }
        Action::Update(path) => {
            tab.ui.menu = None;
            let Some(module) = tab.ui.submodule.find(&path).cloned() else {
                tab.ui.say(Tone::Info, "That submodule is no longer listed");
                cx.notify();
                return;
            };
            if let Some(reason) = update_refusal(&tab.ui, &module) {
                tab.ui.say(Tone::Info, reason);
                cx.notify();
                return;
            }
            let progress = if module.populated {
                format!("Updating submodule {}…", module.shown())
            } else {
                format!("Initialising submodule {}…", module.shown())
            };
            tab.run_network_or(
                cx,
                jobs::Spec::submodules(),
                &progress,
                "Submodule updated",
                false,
                move |repo| update(&repo, &module),
                |_, _| {},
                |_, _| {},
            );
        }
        Action::UpdateAll => update_waiting(tab, cx),
    }
}

fn update_waiting(tab: &mut Tab, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let expected = tab.ui.submodule.list.clone();
    if let Some(reason) = networking(&tab.ui).or_else(|| operating(&tab.ui)).or_else(|| busy(&tab.ui)) {
        tab.ui.say(Tone::Info, reason);
        cx.notify();
        return;
    }
    tab.run_network_or(
        cx,
        jobs::Spec::submodules(),
        "Updating submodules…",
        "Submodules updated",
        false,
        move |repo| update_all(&repo, &expected),
        |_, _| {},
        |_, _| {},
    );
}

fn open(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let full = tab.ui.root.join(&path);
    if std::fs::symlink_metadata(full.join(".git")).is_err() {
        let mut notice = Notice::new(
            Tone::Info,
            format!("Submodule {} is not initialised", path.display()),
            "Initialise it to check out its files, then open it.",
        );
        if tab.ui.submodule.find(&path).is_some() {
            notice = notice.offer("Initialise", view::Action::Submodule(Action::Update(path)));
        }
        tab.ui.notices.show(notice);
        cx.notify();
        return;
    }
    if !crate::tabs::open_elsewhere(full.clone(), |_, _| {}, cx) {
        tab.open(full, cx);
    }
}

fn show(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let row = [false, true].into_iter().find(|staged| row_flags(&tab.ui, &path, *staged).is_some());
    match row {
        Some(staged) => tab.choose(Selection::File { path, staged }, cx),
        None => {
            let shown = tab.ui.submodule.find(&path).map(|module| state_text(&tab.ui, module));
            if let Some(state) = shown {
                tab.ui.say(Tone::Info, format!("Submodule {}: {}", path.display(), state.to_lowercase()));
            }
            cx.notify();
        }
    }
}

pub(crate) fn explains_staging(ui: &Ui, path: &Path) -> bool {
    row_flags(ui, path, false).is_some_and(|flags| flags.modified || flags.untracked)
}

pub(crate) fn stage(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(flags) = row_flags(&tab.ui, &path, false) else { return };
    let shown = path.display().to_string();
    let open = view::Action::Submodule(Action::Open(path.clone()));
    if !flags.moved {
        tab.ui.notices.show(
            Notice::new(
                Tone::Info,
                format!("Nothing to stage in submodule {shown}"),
                "Only a submodule's commit is recorded here, not the uncommitted changes inside it. Commit them in the submodule, then stage its new commit.",
            )
            .offer("Open submodule", open),
        );
        cx.notify();
        return;
    }
    tab.apply_operation(
        cx,
        "Updating the staging area…",
        Some("Staged"),
        false,
        move |repo| {
            repo.stage_file(&path)?;
            Ok::<_, String>(
                Outcome::titled(
                    format!("Staged the new commit of submodule {shown}"),
                    "Only its commit is recorded, not the uncommitted changes inside it. Commit those in the submodule first if they belong with it.",
                )
                .offer("Open submodule", open),
            )
        },
        |_, _| {},
    );
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let head = tab.ui.head.clone();
    let wanted = repo.root.join(".gitmodules").is_file() || tab.ui.files.iter().any(|file| file.submodule.is_some());
    let state = &mut tab.ui.submodule;
    if state.seen != head {
        if let (Some(old), Some(new)) = (state.seen.clone(), head.clone()) {
            let first = state.moved.take().map_or(old, |(first, _)| first);
            state.moved = Some((first, new));
        }
        state.seen = head;
    }
    if !wanted && state.list.is_empty() {
        state.moved = None;
        state.behind.clear();
        return;
    }
    state.ticket += 1;
    let (ticket, epoch, moved) = (state.ticket, tab.epoch, state.moved.clone());
    let asked = moved.clone();
    Tab::work(
        cx,
        move || {
            let listed = list(&repo);
            let changed = asked.as_ref().filter(|(old, new)| old != new).map(|(old, new)| changed_between(&repo, old, new));
            (listed, changed)
        },
        move |tab, (listed, changed), cx| {
            if tab.epoch != epoch || tab.ui.submodule.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.submodule;
            if moved.is_some() && state.moved == moved {
                state.moved = None;
            }
            let Ok(listed) = listed else { return };
            state.list = listed;
            let before = state.behind.len();
            if let Some(Ok(paths)) = changed {
                for path in paths {
                    let waiting = state.list.iter().any(|module| module.path == path && !module.in_sync() && module.recorded.is_some());
                    if waiting && !state.behind.contains(&path) {
                        state.behind.push(path);
                    }
                }
            }
            let list = &state.list;
            state.behind.retain(|path| list.iter().any(|module| module.path == *path && !module.in_sync()));
            let arrived = state.behind.len() > before;
            if arrived && state.automatic && networking(&tab.ui).or_else(|| operating(&tab.ui)).or_else(|| busy(&tab.ui)).is_none() {
                update_waiting(tab, cx);
            }
            cx.notify();
        },
    );
}

pub(crate) fn selected(tab: &mut Tab, selection: &Selection, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let (path, staged) = match selection {
        Selection::File { path, staged } if row_flags(&tab.ui, path, *staged).is_some() => (path.clone(), *staged),
        _ => {
            tab.ui.submodule.panel = None;
            return;
        }
    };
    let state = &mut tab.ui.submodule;
    let kept = state.panel.take().filter(|panel| panel.path == path && panel.staged == staged).and_then(|panel| panel.details);
    state.panel = Some(Panel { path: path.clone(), staged, details: kept });
    state.panel_ticket += 1;
    let (ticket, epoch) = (state.panel_ticket, tab.epoch);
    Tab::work(
        cx,
        move || details(&repo, &path, staged),
        move |tab, read, cx| {
            if tab.epoch != epoch || tab.ui.submodule.panel_ticket != ticket {
                return;
            }
            if let Some(panel) = tab.ui.submodule.panel.as_mut() {
                panel.details = Some(read);
            }
            cx.notify();
        },
    );
}

pub(crate) fn showing(ui: &Ui) -> Option<&Panel> {
    let panel = ui.submodule.panel.as_ref()?;
    let chosen = matches!(&ui.selection, Some(Selection::File { path, staged }) if *path == panel.path && *staged == panel.staged);
    (chosen && row_flags(ui, &panel.path, panel.staged).is_some()).then_some(panel)
}

pub(crate) fn loaded(ui: &Ui) -> Option<&Details> {
    showing(ui)?.details.as_ref()?.as_ref().ok()
}

fn commit_line(label: &'static str, commit: Option<&Commit>) -> Div {
    let (oid, subject) = match commit {
        Some(commit) => (
            notice::short(&commit.oid).to_owned(),
            commit.subject.clone().unwrap_or_else(|| "Not in the submodule yet".to_owned()),
        ),
        None => ("—".to_owned(), "None".to_owned()),
    };
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(div().w(px(150.)).flex_shrink_0().child(kit::label(label)))
        .child(kit::fit(oid).size(12.5).mono().color(TEXT_2))
        .child(kit::fit(subject).size(12.5).grow())
}

fn commit_list(title: String, commits: &[Commit], count: usize) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(kit::fit(title).size(12.5).weight(FontWeight::SEMIBOLD))
        .children(commits.iter().map(|commit| {
            div()
                .pl_3()
                .flex()
                .items_center()
                .gap_3()
                .child(kit::fit(notice::short(&commit.oid).to_owned()).size(12.).mono().color(TEXT_3))
                .child(kit::fit(commit.subject.clone().unwrap_or_default()).size(12.5).color(TEXT_2).grow())
        }))
        .when(count > commits.len(), |d| d.child(div().pl_3().child(kit::muted(format!("and {} more", count - commits.len())))))
}

pub(crate) fn ahead_title(details: &Details) -> String {
    let commits = notice::commits(details.ahead_count);
    if details.staged {
        format!("{commits} staged that the last commit does not record")
    } else {
        format!("{commits} checked out that this repository does not record")
    }
}

pub(crate) fn behind_title(details: &Details) -> String {
    let commits = notice::commits(details.behind_count);
    if details.staged {
        format!("{commits} in the last commit that are not staged")
    } else {
        format!("{commits} recorded here that are not checked out")
    }
}

pub(crate) fn panel<T: Desk>(ui: &Ui, panel: &Panel, cx: &Context<T>) -> gpui::Stateful<Div> {
    let path = panel.path.clone();
    let flags = row_flags(ui, &panel.path, panel.staged).unwrap_or_default();
    let module = ui.submodule.find(&panel.path);
    let summary = describe(flags, panel.staged);
    let editable = ui.can_edit_index();
    let body = match &panel.details {
        None => div().flex().items_center().gap_2().child(kit::spinner("submodule-reading")).child(kit::muted("Reading the submodule…")),
        Some(Err(error)) => div().child(kit::muted(error.clone())),
        Some(Ok(details)) => {
            let (from, to) = if details.staged { ("IN THE LAST COMMIT", "STAGED") } else { ("RECORDED HERE", "CHECKED OUT") };
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().flex().flex_col().gap_2().child(commit_line(from, details.from.as_ref())).child(commit_line(to, details.to.as_ref())))
                .when(!details.populated, |d| d.child(kit::muted("It is not initialised, so nothing is checked out.")))
                .when(details.ahead_count > 0, |d| d.child(commit_list(ahead_title(details), &details.ahead, details.ahead_count)))
                .when(details.behind_count > 0, |d| d.child(commit_list(behind_title(details), &details.behind, details.behind_count)))
                .children(details.missing.first().map(|oid| {
                    kit::muted(format!("Commit {} is not in the submodule yet. Updating fetches it.", notice::short(oid)))
                }))
                .children(details.changes.filter(|count| *count > 0).map(|count| {
                    kit::muted(match count {
                        1 => "1 uncommitted change inside it.".to_owned(),
                        n => format!("{n} uncommitted changes inside it."),
                    })
                }))
        }
    };
    let update = module.filter(|_| !panel.staged).map(|module| {
        let label = if module.populated { "Update to the recorded commit" } else { "Initialise" };
        let refusal = update_refusal(ui, module);
        kit::btn("submodule-update", label)
            .small()
            .enabled(refusal.is_none())
            .tip(refusal.unwrap_or_else(|| "git submodule update --init".to_owned()))
            .on(view::Action::Submodule(Action::Update(module.path.clone())), cx)
    });
    let open = module.filter(|module| module.populated).map(|_| {
        kit::btn("submodule-open", "Open submodule").small().icon("external").on(view::Action::Submodule(Action::Open(path.clone())), cx)
    });
    let stage = if panel.staged {
        kit::btn("submodule-unstage", "Unstage").small().enabled(editable).on(view::Action::UnstagePath(path.clone()), cx)
    } else {
        kit::btn("submodule-stage", "Stage").small().enabled(editable).on(view::Action::StagePath(path.clone()), cx)
    };
    div()
        .id("submodule-panel")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(kit::chip(kit::Chip::Plain, "Submodule"))
                .child(kit::fit(panel.path.display().to_string()).size(13.).weight(FontWeight::SEMIBOLD))
                .when(!summary.is_empty(), |d| d.child(kit::fit(summary).size(12.5).color(TEXT_3).grow())),
        )
        .child(body)
        .child(div().flex().flex_wrap().gap_2().children(update).children(open).child(stage))
        .when(!panel.staged && (flags.modified || flags.untracked), |d| {
            d.child(kit::muted("Staging records only the submodule's commit, not the uncommitted changes inside it."))
        })
}

pub(crate) fn row_marks(flags: Gitlink, staged: bool) -> Div {
    let summary = describe(flags, staged);
    div()
        .flex_shrink_0()
        .max_w(px(150.))
        .flex()
        .items_center()
        .gap_1()
        .child(kit::chip(kit::Chip::Plain, "Submodule"))
        .when(!summary.is_empty(), |d| d.child(kit::fit(summary).size(TEXT_TINY).color(TEXT_3)))
}

pub(crate) fn strip_text(count: usize) -> String {
    match count {
        1 => "1 submodule is not at the commit this branch expects.".to_owned(),
        n => format!("{n} submodules are not at the commits this branch expects."),
    }
}

pub(crate) fn strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let ui = app.ui();
    let count = ui.submodule.behind.len();
    if count == 0 {
        return None;
    }
    let refusal = networking(ui).or_else(|| operating(ui)).or_else(|| busy(ui));
    Some(
        div()
            .h(px(36.))
            .flex_shrink_0()
            .pl(px(14.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap_2()
            .bg(soft(MODIFIED, 0x18))
            .border_b_1()
            .border_color(soft(MODIFIED, 0x48))
            .child(kit::icon("alert").size(px(13.)).text_color(rgb(MODIFIED)))
            .child(kit::fit(strip_text(count)).size(TEXT_SMALL).grow())
            .child(
                kit::btn("update-submodules", "Update submodules")
                    .small()
                    .enabled(refusal.is_none())
                    .tip(refusal.unwrap_or_else(|| "git submodule update --init --recursive".to_owned()))
                    .on(view::Action::Submodule(Action::UpdateAll), cx),
            )
            .child(kit::icon_btn("dismiss-submodules", "x", "Dismiss", view::Action::Submodule(Action::Dismiss), cx)),
    )
}

pub(crate) fn side_row<T: Desk>(ui: &Ui, index: usize, row: usize, pointed: bool, cx: &mut Context<T>) -> AnyElement {
    let Some(module) = ui.submodule.list.get(index) else { return div().into_any_element() };
    let state = state_text(ui, module);
    let quiet = module.in_sync() && state == "In sync";
    let (shown, opened, menu_path) = (module.path.clone(), module.path.clone(), module.path.clone());
    let tip = {
        let mut lines = vec![module.shown(), state.clone()];
        if let Some(recorded) = &module.recorded {
            lines.push(format!("Recorded {}", notice::short(recorded)));
        }
        if let Some(checked) = &module.checked_out {
            lines.push(format!("Checked out {}", notice::short(checked)));
        }
        lines.join("\n")
    };
    kit::row(("side-submodule", row), false)
        .h(px(TREE_ROW))
        .pl(px(12.))
        .pr(px(10.))
        .gap(px(6.))
        .child(div().w(px(11.)).flex_shrink_0().flex().justify_center().when(!quiet, |d| {
            d.child(div().size(px(6.)).rounded_full().bg(rgb(MODIFIED)))
        }))
        .child(kit::fit(module.shown()).size(12.5).color(TEXT_2).grow())
        .child(div().max_w(px(120.)).flex_shrink_0().child(kit::fit(state).size(TEXT_SMALL).color(TEXT_3)))
        .when(pointed, |d| d.tooltip(kit::tip(tip)))
        .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
            if event.click_count() > 1 {
                app.dispatch(view::Action::Submodule(Action::Open(opened.clone())), w, cx);
            } else {
                app.dispatch(view::Action::Submodule(Action::Show(shown.clone())), w, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = Target::Submodule(menu_path.clone());
                app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
            }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};
    use std::fs;

    struct Nest {
        f: Fixture,
        library: Fixture,
        _config: GlobalConfig,
        _file: tempfile::NamedTempFile,
    }

    fn commit(f: &Fixture, name: &str, text: &str, subject: &str) -> String {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", subject]);
        head(f)
    }

    fn head(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn at(f: &Fixture, path: &str, args: &[&str]) -> String {
        let dir = f.dir.join(path);
        let mut all = vec!["-C", dir.to_str().unwrap()];
        all.extend_from_slice(args);
        String::from_utf8(f.git(&all)).unwrap().trim().to_owned()
    }

    fn nest() -> Nest {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"[protocol \"file\"]\n\tallow = always\n").unwrap();
        let config = GlobalConfig::set(file.path());
        let library = Fixture::new();
        commit(&library, "map.txt", "Ridge map\n", "Draw the ridge map");
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["submodule", "add", "-q", library.dir.to_str().unwrap(), "maps"]);
        f.git(&["commit", "-qm", "Add the maps"]);
        for (key, value) in [("user.name", "Cedar"), ("user.email", "cedar@example.invalid"), ("commit.gpgSign", "false")] {
            at(&f, "maps", &["config", "--local", key, value]);
        }
        Nest { f, library, _config: config, _file: file }
    }

    fn work_inside(f: &Fixture, name: &str, text: &str, subject: &str) -> String {
        fs::write(f.dir.join("maps").join(name), text).unwrap();
        at(f, "maps", &["add", "--", name]);
        at(f, "maps", &["commit", "-qm", subject]);
        at(f, "maps", &["rev-parse", "HEAD"])
    }

    fn recorded(f: &Fixture) -> String {
        let listed = String::from_utf8(f.git(&["ls-files", "--stage", "--", "maps"])).unwrap();
        listed.split_whitespace().nth(1).unwrap().to_owned()
    }

    fn unstaged(f: &Fixture) -> Vec<(PathBuf, Option<Gitlink>)> {
        f.repo.status().unwrap().unstaged.into_iter().map(|file| (file.path, file.submodule)).collect()
    }

    #[test]
    fn status_reads_new_commits_modified_and_untracked_content_of_a_submodule() {
        let n = nest();
        let f = &n.f;
        assert!(f.repo.status().unwrap().unstaged.is_empty());
        let start = recorded(f);
        work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        assert_eq!(unstaged(f), [(PathBuf::from("maps"), Some(Gitlink { moved: true, modified: false, untracked: false }))]);
        fs::write(f.dir.join("maps/map.txt"), "Ridge map, redrawn\n").unwrap();
        fs::write(f.dir.join("maps/notes.txt"), "Loose notes\n").unwrap();
        assert_eq!(unstaged(f), [(PathBuf::from("maps"), Some(Gitlink { moved: true, modified: true, untracked: true }))]);
        assert_eq!(describe(Gitlink { moved: true, modified: true, untracked: true }, false), "new commits, modified content, untracked content");
        let inside = at(f, "maps", &["rev-parse", "HEAD"]);
        f.repo.stage_file(Path::new("maps")).unwrap();
        assert_eq!(recorded(f), inside);
        assert_eq!(fs::read_to_string(f.dir.join("maps/map.txt")).unwrap(), "Ridge map, redrawn\n");
        assert_eq!(at(f, "maps", &["status", "--porcelain"]), "M map.txt\n?? notes.txt");
        let status = f.repo.status().unwrap();
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.staged[0].submodule, Some(Gitlink { moved: true, modified: false, untracked: false }));
        assert_eq!(status.unstaged[0].submodule, Some(Gitlink { moved: false, modified: true, untracked: true }));
        assert_ne!(recorded(f), start);
        let plain = f.repo.status().unwrap().unstaged.into_iter().filter(|file| file.submodule.is_none()).count();
        assert_eq!(plain, 0);
    }

    #[test]
    fn the_list_reads_each_submodule_and_the_details_list_the_commits_between() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        let listed = list(&f.repo).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, Path::new("maps"));
        assert_eq!(listed[0].name, "maps");
        assert_eq!(listed[0].recorded.as_deref(), Some(start.as_str()));
        assert!(listed[0].in_sync());
        let first = work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        let second = work_inside(f, "lake.txt", "Lake\n", "Mark the lake");
        let listed = list(&f.repo).unwrap();
        assert_eq!(listed[0].checked_out.as_deref(), Some(second.as_str()));
        assert!(!listed[0].in_sync());
        let read = details(&f.repo, Path::new("maps"), false).unwrap();
        assert_eq!(read.from.as_ref().map(|commit| commit.oid.as_str()), Some(start.as_str()));
        assert_eq!(read.from.as_ref().and_then(|commit| commit.subject.as_deref()), Some("Draw the ridge map"));
        assert_eq!(read.to.as_ref().map(|commit| commit.oid.as_str()), Some(second.as_str()));
        assert_eq!(read.ahead_count, 2);
        assert_eq!(read.ahead.iter().map(|commit| commit.oid.as_str()).collect::<Vec<_>>(), [second.as_str(), first.as_str()]);
        assert_eq!(read.behind_count, 0);
        assert_eq!(read.changes, Some(0));
        assert_eq!(ahead_title(&read), "2 commits checked out that this repository does not record");
        at(f, "maps", &["checkout", "-q", &start]);
        let back = details(&f.repo, Path::new("maps"), false).unwrap();
        assert_eq!((back.ahead_count, back.behind_count), (0, 0));
        f.git(&["update-index", "--cacheinfo", &format!("160000,{second},maps")]);
        let staged = details(&f.repo, Path::new("maps"), true).unwrap();
        assert_eq!(staged.from.as_ref().map(|commit| commit.oid.as_str()), Some(start.as_str()));
        assert_eq!(staged.to.as_ref().map(|commit| commit.oid.as_str()), Some(second.as_str()));
        assert_eq!(staged.ahead_count, 2);
        let behind = details(&f.repo, Path::new("maps"), false).unwrap();
        assert_eq!((behind.ahead_count, behind.behind_count), (0, 2));
        assert_eq!(behind_title(&behind), "2 commits recorded here that are not checked out");
        assert!(details(&f.repo, Path::new("trail.txt"), false).is_err());
        assert_eq!(n.library.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    }

    #[test]
    fn updating_a_submodule_moves_it_to_the_recorded_commit_and_leaves_the_repository_alone() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        let refs = f.git(&["for-each-ref"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let moved = work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        let shown = list(&f.repo).unwrap().remove(0);
        let outcome = update(&f.repo, &shown).unwrap();
        assert_eq!(outcome.title, "Updated submodule maps");
        assert!(outcome.detail.contains(&notice::short(&moved).to_owned()));
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), start);
        assert!(f.repo.status().unwrap().unstaged.is_empty());
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert!(at(f, "maps", &["cat-file", "-t", &moved]) == "commit");
    }

    #[test]
    fn updating_refuses_a_submodule_that_changed_since_it_was_shown() {
        let n = nest();
        let f = &n.f;
        work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        let shown = list(&f.repo).unwrap().remove(0);
        let later = work_inside(f, "lake.txt", "Lake\n", "Mark the lake");
        let refused = update(&f.repo, &shown).unwrap_err();
        assert!(refused.contains("changed since it was shown"), "{refused}");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), later);
        let shown = list(&f.repo).unwrap().remove(0);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{later},maps")]);
        assert!(update(&f.repo, &shown).unwrap_err().contains("changed since it was shown"));
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), later);
        assert_eq!(recorded(f), later);
        let in_sync = list(&f.repo).unwrap().remove(0);
        assert!(update(&f.repo, &in_sync).unwrap_err().contains("already at the recorded commit"));
    }

    #[test]
    fn updating_refuses_to_leave_commits_that_are_on_no_branch_behind() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        at(f, "maps", &["checkout", "-q", "--detach"]);
        work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        let loose = work_inside(f, "lake.txt", "Lake\n", "Mark the lake");
        let shown = list(&f.repo).unwrap();
        assert_eq!(left_behind(&f.repo, &shown[0]).unwrap(), 2);
        let refused = update(&f.repo, &shown[0]).unwrap_err();
        assert_eq!(refused, "Submodule maps has 2 commits on no branch. Open it and create a branch there first, or updating leaves them behind.");
        assert_eq!(update_all(&f.repo, &shown).unwrap_err(), refused);
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), loose);
        f.git(&["config", "submodule.maps.update", "rebase"]);
        assert_eq!(left_behind(&f.repo, &shown[0]).unwrap(), 0);
        f.git(&["config", "--unset", "submodule.maps.update"]);
        at(f, "maps", &["branch", "camp"]);
        assert_eq!(left_behind(&f.repo, &shown[0]).unwrap(), 0);
        assert_eq!(update(&f.repo, &shown[0]).unwrap().title, "Updated submodule maps");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), start);
        assert_eq!(at(f, "maps", &["rev-parse", "camp"]), loose);
        at(f, "maps", &["checkout", "-q", "--detach", &loose]);
        at(f, "maps", &["branch", "-qD", "camp"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{loose},maps")]);
        at(f, "maps", &["checkout", "-q", "--detach", "HEAD~1"]);
        let behind = list(&f.repo).unwrap().remove(0);
        assert_eq!(behind.recorded.as_deref(), Some(loose.as_str()));
        assert_eq!(left_behind(&f.repo, &behind).unwrap(), 0);
        assert_eq!(update(&f.repo, &behind).unwrap().title, "Updated submodule maps");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), loose);
    }

    #[test]
    fn a_submodule_that_is_not_initialised_is_listed_and_initialised() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        f.git(&["submodule", "deinit", "-q", "-f", "maps"]);
        let shown = list(&f.repo).unwrap().remove(0);
        assert!(!shown.populated && shown.checked_out.is_none());
        assert!(!shown.in_sync());
        let read = details(&f.repo, Path::new("maps"), false).unwrap();
        assert!(!read.populated && read.to.is_none() && read.ahead_count == 0);
        let outcome = update(&f.repo, &shown).unwrap();
        assert_eq!(outcome.title, "Initialised submodule maps");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), start);
        assert!(f.dir.join("maps/map.txt").is_file());
    }

    #[test]
    fn after_a_pull_the_moved_pointer_is_found_and_every_waiting_submodule_is_updated() {
        let n = nest();
        let f = &n.f;
        let before = head(f);
        let start = recorded(f);
        let newer = commit(&n.library, "lake.txt", "Lake\n", "Mark the lake");
        at(f, "maps", &["fetch", "-q", "origin"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{newer},maps")]);
        f.git(&["commit", "-qm", "Move the maps"]);
        let after = head(f);
        at(f, "maps", &["checkout", "-q", &start]);
        assert_eq!(changed_between(&f.repo, &before, &after).unwrap(), [PathBuf::from("maps")]);
        assert!(changed_between(&f.repo, &before, &before).unwrap().is_empty());
        assert!(changed_between(&f.repo, "not-an-id", &after).is_err());
        let shown = list(&f.repo).unwrap();
        assert!(!shown[0].in_sync());
        let refs = f.git(&["for-each-ref"]);
        let outcome = update_all(&f.repo, &shown).unwrap();
        assert_eq!(outcome.title, "Updated submodule maps");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), newer);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert!(update_all(&f.repo, &list(&f.repo).unwrap()).unwrap_err().contains("already at the commit"));
    }

    #[test]
    fn updating_every_submodule_refuses_a_list_that_changed() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        let newer = commit(&n.library, "lake.txt", "Lake\n", "Mark the lake");
        at(f, "maps", &["fetch", "-q", "origin"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{newer},maps")]);
        let shown = list(&f.repo).unwrap();
        f.git(&["update-index", "--cacheinfo", &format!("160000,{start},maps")]);
        assert!(update_all(&f.repo, &shown).unwrap_err().contains("changed since they were shown"));
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), start);
        assert_eq!(recorded(f), start);
    }

    #[test]
    fn a_folder_without_gitmodules_lists_nothing_and_a_declared_path_must_be_plain() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        assert!(list(&f.repo).unwrap().is_empty());
        fs::write(f.dir.join(".gitmodules"), "[submodule \"odd\"]\n\tpath = ../outside\n[submodule \"maps\"]\n\tpath = maps\n").unwrap();
        assert_eq!(declared(&f.repo).unwrap(), [("maps".to_owned(), PathBuf::from("maps"))]);
        assert!(list(&f.repo).unwrap().is_empty());
    }
}
