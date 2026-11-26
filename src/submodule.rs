use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Side, Target, busy, networking, on_repo, operating},
    jobs,
    notice::{self, Notice, Outcome},
    prompt::{self, Answer, Field, Spec},
    remotes,
    repository::{Gitlink, Repository, checked, git_error, raw_path, run, validate_oid, validate_path, with_system_tools},
    settings::{self, Memory},
    theme::*,
    view::{self, Desk, Menu, Selection, Tone, Ui},
};
use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, div, prelude::*, px, rgb};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::Command as Process,
};

const LISTED: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Module {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) url: Option<String>,
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
    pub(crate) uninitialised: Vec<PathBuf>,
    pub(crate) offer: bool,
    offered: bool,
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
    Add,
    ChangeAddress(PathBuf),
    Remove(PathBuf),
    InitialiseAll,
    StopOffering,
    ToggleOffer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Addition {
    pub(crate) address: String,
    pub(crate) path: PathBuf,
    pub(crate) seen: Vec<Module>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removal {
    pub(crate) module: Module,
    pub(crate) changes: usize,
    pub(crate) unpushed: usize,
    pub(crate) kept_for: Option<String>,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Add { seen: Vec<Module> },
    Address(Module),
    Remove(Removal),
}

fn inside(repo: &Repository, path: &Path) -> Process {
    let mut command = repo.read_command();
    command.arg("-C").arg(repo.root.join(path));
    command
}

fn populated(repo: &Repository, path: &Path) -> bool {
    std::fs::symlink_metadata(repo.root.join(path).join(".git")).is_ok()
}

pub(crate) fn declared(repo: &Repository) -> Result<Vec<(String, PathBuf, Option<String>)>, String> {
    let file = repo.root.join(".gitmodules");
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let output = run(
        repo.read_command()
            .args(["config", "-z", "--file"])
            .arg(&file)
            .args(["--get-regexp", r"^submodule\..*\.(path|url)$"]),
    )
    .map_err(|error| format!("Run bundled Git: {error}"))?;
    match output.status.code() {
        Some(0) => {}
        Some(1) => return Ok(Vec::new()),
        _ => return Err(git_error(&output.stderr, output.status)),
    }
    let mut found: Vec<(String, PathBuf, Option<String>)> = Vec::new();
    let mut urls: Vec<(String, String)> = Vec::new();
    for record in output.stdout.split(|&b| b == 0).filter(|record| !record.is_empty()) {
        let Some(at) = record.iter().position(|&b| b == b'\n') else { continue };
        let Some(key) = record[..at].strip_prefix(b"submodule.") else { continue };
        let value = &record[at + 1..];
        if let Some(name) = key.strip_suffix(b".path") {
            let path = raw_path(value);
            if validate_path(&path).is_err() || found.iter().any(|(_, known, _)| *known == path) {
                continue;
            }
            found.push((String::from_utf8_lossy(name).into_owned(), path, None));
        } else if let Some(name) = key.strip_suffix(b".url") {
            urls.push((String::from_utf8_lossy(name).into_owned(), String::from_utf8_lossy(value).into_owned()));
        }
    }
    for (name, url) in urls {
        if let Some(entry) = found.iter_mut().find(|(known, _, _)| *known == name) {
            entry.2 = Some(url);
        }
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
    let paths: Vec<PathBuf> = declared.iter().map(|(_, path, _)| path.clone()).collect();
    let recorded = recorded_in_index(repo, &paths)?;
    Ok(declared
        .into_iter()
        .filter_map(|(name, path, url)| {
            let mut stages = recorded.iter().filter(|(listed, _, _)| *listed == path);
            let first = stages.next()?;
            let recorded = (first.2 == b'0').then(|| first.1.clone());
            let populated = populated(repo, &path);
            let checked_out = checked_out(repo, &path);
            Some(Module { path, name, url, recorded, checked_out, populated })
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

const LOCAL_REFUSED: &str = "Git does not clone submodules from local folders unless your Git configuration allows it with protocol.file.allow. Use the repository's network address, or allow local folders in your own Git configuration if you trust them.";

pub(crate) fn address_problem(text: &str) -> Option<String> {
    match text.trim() {
        "" => Some(String::new()),
        address => remotes::validate_url(address).err(),
    }
}

pub(crate) fn derived_path(address: &str) -> Option<PathBuf> {
    let trimmed = address.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed).trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next()?;
    let path = PathBuf::from(last);
    (validate_path(&path).is_ok() && !last.eq_ignore_ascii_case(".git")).then_some(path)
}

fn names_git(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str().to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
}

pub(crate) fn folder_problem(path: &Path, root: &Path, listed: &[Module]) -> Option<String> {
    if path.is_absolute() {
        return Some("Enter a folder inside this repository, such as libs/maps.".to_owned());
    }
    if validate_path(path).is_err() || path.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Some("Enter the folder without . or .. in it.".to_owned());
    }
    if names_git(path) {
        return Some("Choose a folder that is not named .git.".to_owned());
    }
    for module in listed {
        if module.path == path {
            return Some(format!("{} is already a submodule.", path.display()));
        }
        if path.starts_with(&module.path) {
            return Some(format!("That folder is inside submodule {}.", module.shown()));
        }
        if module.path.starts_with(path) {
            return Some(format!("That folder holds submodule {}.", module.shown()));
        }
    }
    let full = root.join(path);
    match fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("{} cannot be read: {error}.", path.display())),
        Ok(meta) if !meta.is_dir() => Some(format!("{} already exists and is not a folder.", path.display())),
        Ok(_) => match fs::read_dir(&full).map(|mut entries| entries.next().is_none()) {
            Ok(true) => None,
            Ok(false) => Some(format!("{} already exists and is not empty.", path.display())),
            Err(error) => Some(format!("{} cannot be read: {error}.", path.display())),
        },
    }
}

pub(crate) fn path_problem(text: &str, ui: &Ui) -> Option<String> {
    let text = text.trim().trim_end_matches('/');
    if text.is_empty() {
        return None;
    }
    folder_problem(Path::new(text), &ui.root, &ui.submodule.list)
}

fn module_dir(repo: &Repository, name: &str) -> Option<PathBuf> {
    let relative = Path::new(name);
    validate_path(relative).ok()?;
    Some(repo.git_dir().join("modules").join(relative))
}

fn tracked(repo: &Repository, path: &Path) -> Result<bool, String> {
    let output = checked(repo.read_command().args(["ls-files", "-z", "--"]).arg(path))?;
    Ok(!output.is_empty())
}

pub(crate) fn add(repo: &Repository, addition: &Addition) -> Result<Outcome, String> {
    let Addition { address, path, seen } = addition;
    remotes::validate_url(address)?;
    let listed = list(repo)?;
    if listed != *seen {
        return Err("The submodules changed since you chose this. Reopen the action.".to_owned());
    }
    if let Some(problem) = folder_problem(path, &repo.root, &listed) {
        return Err(problem);
    }
    let shown = path.display().to_string();
    if tracked(repo, path)? {
        return Err(format!("{shown} is already in this repository."));
    }
    if let Some(dir) = module_dir(repo, &shown).filter(|dir| fs::symlink_metadata(dir).is_ok()) {
        return Err(format!(
            "Git still keeps the data of an earlier submodule named {shown} in {}. Choose another folder, or delete that data first.",
            dir.display()
        ));
    }
    let mut command = repo.network_command()?;
    with_system_tools(&mut command).args(["submodule", "add", "--progress", "--"]).arg(address).arg(path);
    checked(&mut command).map_err(|error| if error.contains("transport 'file' not allowed") { LOCAL_REFUSED.to_owned() } else { error })?;
    let added = fresh(repo, path)?;
    let at = added.recorded.as_deref().map(notice::short).unwrap_or("its first commit").to_owned();
    Ok(Outcome::titled(
        format!("Added submodule {shown}"),
        format!("It is at {at}, cloned from {}. It is staged with .gitmodules: commit them to record it.", remotes::shown(address)),
    )
    .offer("Open submodule", view::Action::Submodule(Action::Open(path.clone())))
    .undone_as(format!("Add submodule {shown}"))
    .not_undoable(format!("Adding submodule {shown} cannot be undone. Remove it from the Submodules section instead.")))
}

pub(crate) fn set_address(repo: &Repository, expected: &Module, address: &str) -> Result<Outcome, String> {
    remotes::validate_url(address)?;
    let found = fresh(repo, &expected.path)?;
    let shown = found.shown();
    if found.url != expected.url {
        return Err(format!("The address of submodule {shown} changed since you chose this. Reopen the action."));
    }
    if found.url.as_deref() == Some(address) {
        return Ok(Outcome::titled("Nothing changed", format!("Submodule {shown} already uses this address.")));
    }
    checked(with_system_tools(&mut repo.command()).args(["submodule", "--quiet", "set-url", "--"]).arg(&found.path).arg(address))?;
    checked(with_system_tools(&mut repo.command()).args(["submodule", "--quiet", "sync", "--"]).arg(&found.path))?;
    let now = fresh(repo, &found.path)?;
    if now.url.as_deref() != Some(address) {
        return Err(format!("Git did not change the address of submodule {shown}"));
    }
    let mut detail = format!("It fetches from {}", remotes::shown(address));
    if found.populated {
        detail.push_str(", and so does its checkout");
    }
    detail.push_str(". .gitmodules is changed: commit it to share the new address.");
    Ok(Outcome::titled(format!("Changed the address of submodule {shown}"), detail))
}

fn unpushed(repo: &Repository, module: &Module) -> Result<usize, String> {
    if !module.populated || module.checked_out.is_none() {
        return Ok(0);
    }
    let output = checked(inside(repo, &module.path).args(["rev-list", "--count", "HEAD", "--branches", "--not", "--remotes", "--"]))?;
    Ok(String::from_utf8_lossy(&output).trim().parse().unwrap_or(0))
}

fn linked_data(folder: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(folder.join(".git")).ok()?;
    let target = text.strip_prefix("gitdir: ")?.trim_end_matches(['\n', '\r']);
    fs::canonicalize(folder.join(target)).ok()
}

fn used_elsewhere(repo: &Repository, path: &Path, data: Option<&Path>) -> Option<String> {
    let data = fs::canonicalize(data?).ok()?;
    let listed = crate::worktree::list(repo).ok()?;
    listed
        .iter()
        .filter(|worktree| !worktree.current && worktree.prunable.is_none())
        .find(|worktree| linked_data(&worktree.path.join(path)).is_some_and(|linked| linked == data))
        .map(crate::worktree::Worktree::name)
}

pub(crate) fn removal(repo: &Repository, expected: &Module) -> Result<Removal, String> {
    let found = fresh(repo, &expected.path)?;
    if found != *expected {
        return Err(format!("Submodule {} changed since it was shown. Look again, then remove it.", expected.shown()));
    }
    let changes = if found.populated { crate::worktree::uncommitted(repo, &repo.root.join(&found.path))? } else { 0 };
    let unpushed = unpushed(repo, &found)?;
    let kept_for = used_elsewhere(repo, &found.path, module_dir(repo, &found.name).as_deref());
    Ok(Removal { module: found, changes, unpushed, kept_for })
}

fn gitmodules_changed(repo: &Repository) -> Result<bool, String> {
    let output = run(repo.read_command().args(["diff", "--quiet", "--", ".gitmodules"])).map_err(|error| format!("Run bundled Git: {error}"))?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(git_error(&output.stderr, output.status)),
    }
}

fn delete_data(repo: &Repository, dir: &Path) -> Result<(), String> {
    let modules = repo.git_dir().join("modules");
    let meta = match fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{} cannot be read: {error}", dir.display())),
        Ok(meta) => meta,
    };
    if !meta.is_dir() {
        return Err(format!("{} is not a folder, so it was left alone", dir.display()));
    }
    let inside_modules = match (fs::canonicalize(&modules), fs::canonicalize(dir)) {
        (Ok(base), Ok(real)) => real.starts_with(&base) && real != base,
        _ => false,
    };
    if !inside_modules {
        return Err(format!("{} is not inside {}, so it was left alone", dir.display(), modules.display()));
    }
    fs::remove_dir_all(dir).map_err(|error| format!("{} cannot be deleted: {error}", dir.display()))?;
    let mut at = dir.parent();
    while let Some(parent) = at {
        if parent == modules || !parent.starts_with(&modules) || fs::remove_dir(parent).is_err() {
            break;
        }
        at = parent.parent();
    }
    Ok(())
}

pub(crate) fn remove(repo: &Repository, expected: &Removal) -> Result<Outcome, String> {
    let shown = expected.module.shown();
    let now = removal(repo, &expected.module)?;
    if now != *expected {
        return Err(format!("What is inside submodule {shown} changed since you chose to remove it. Reopen the action to see it."));
    }
    if now.module.recorded.is_none() {
        return Err(format!("Submodule {shown} has a conflict. Resolve it first."));
    }
    if gitmodules_changed(repo)? {
        return Err("Stage or undo the changes to .gitmodules first. Removing a submodule changes that file and stages it.".to_owned());
    }
    let path = &now.module.path;
    let data = module_dir(repo, &now.module.name);
    checked(with_system_tools(&mut repo.command()).args(["--literal-pathspecs", "submodule", "--quiet", "deinit", "-f", "--"]).arg(path))?;
    checked(repo.command().args(["--literal-pathspecs", "rm", "-q", "-f", "--"]).arg(path))
        .map_err(|error| format!("Submodule {shown} was emptied, but Git did not remove it: {error}"))?;
    let folder = repo.root.join(path);
    if fs::read_dir(&folder).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(&folder);
    }
    let staged = "The removal and the change to .gitmodules are staged: commit them to record it.";
    let detail = match (&now.kept_for, data) {
        (Some(worktree), _) => {
            format!("Its folder is deleted. Its Git data is kept, because worktree {worktree} still uses it. {staged}")
        }
        (None, Some(dir)) => {
            delete_data(repo, &dir).map_err(|error| format!("Submodule {shown} was removed, but its Git data was not deleted: {error}"))?;
            format!("Its folder and its Git data are deleted. {staged}")
        }
        (None, None) => format!("Its folder is deleted. Its Git data is left alone, because its name is not a plain folder name. {staged}"),
    };
    Ok(Outcome::titled(format!("Removed submodule {shown}"), detail)
        .undone_as(format!("Remove submodule {shown}"))
        .not_undoable(format!("Removing submodule {shown} cannot be undone.")))
}

pub(crate) fn initialise(repo: &Repository, expected: &[PathBuf]) -> Result<Outcome, String> {
    let found = list(repo)?;
    let waiting: Vec<&Module> = expected.iter().filter_map(|path| found.iter().find(|module| module.path == *path)).collect();
    if waiting.len() != expected.len() || waiting.iter().any(|module| module.populated) || expected.is_empty() {
        return Err("The submodules changed since they were shown. Look again, then initialise them.".to_owned());
    }
    if let Some(module) = waiting.iter().find(|module| module.recorded.is_none()) {
        return Err(format!("Submodule {} has a conflict. Resolve it first.", module.shown()));
    }
    let mut command = repo.network_command()?;
    with_system_tools(&mut command).args(["submodule", "update", "--init", "--recursive", "--progress", "--"]).args(expected);
    checked(&mut command).map_err(|error| if error.contains("transport 'file' not allowed") { LOCAL_REFUSED.to_owned() } else { error })?;
    let now = list(repo)?;
    let done: Vec<String> = waiting
        .iter()
        .filter(|module| now.iter().any(|after| after.path == module.path && after.in_sync()))
        .map(|module| module.shown())
        .collect();
    let title = match done.as_slice() {
        [] => return Err("Git initialised no submodule".to_owned()),
        [one] => format!("Initialised submodule {one}"),
        many => format!("Initialised {} submodules", many.len()),
    };
    let mut detail = format!("Checked out at the commits this repository records: {}.", done.join(", "));
    let left = waiting.len() - done.len();
    if left > 0 {
        detail.push_str(&format!(" {left} could not be initialised."));
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
    Command::new(
        "submodule.add",
        Group::Create,
        |_, _| "Add submodule…".into(),
        |ui, target, _| {
            if !on_repo(ui, target) {
                return Availability::Hidden;
            }
            Availability::unless(networking(ui).or_else(|| operating(ui)).or_else(|| busy(ui)))
        },
        |_, _| Some(view::Action::Submodule(Action::Add)),
    )
    .words(&["git submodule add", "new submodule", "add a repository inside"])
    .icon("plus"),
    Command::new(
        "submodule.set-url",
        Group::Manage,
        |_, _| "Change address…".into(),
        |ui, target, _| match target {
            Target::Submodule(path) if ui.submodule.find(path).is_some() => {
                Availability::unless(operating(ui).or_else(|| busy(ui)))
            }
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Submodule(path) => Some(view::Action::Submodule(Action::ChangeAddress(path.clone()))),
            _ => None,
        },
    )
    .words(&["submodule url", "set-url", "edit submodule"])
    .icon("pencil"),
    Command::new(
        "submodule.remove",
        Group::Manage,
        |_, _| "Remove submodule…".into(),
        |ui, target, _| match target {
            Target::Submodule(path) => match ui.submodule.find(path) {
                None => Availability::Hidden,
                Some(module) => Availability::unless(removal_refusal(ui, module)),
            },
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Submodule(path) => Some(view::Action::Submodule(Action::Remove(path.clone()))),
            _ => None,
        },
    )
    .words(&["delete submodule", "git submodule deinit", "git rm submodule"])
    .icon("trash")
    .danger(),
];

fn removal_refusal(ui: &Ui, module: &Module) -> Option<String> {
    if module.recorded.is_none() {
        return Some("It has a conflict. Resolve it first".to_owned());
    }
    operating(ui).or_else(|| networking(ui)).or_else(|| busy(ui))
}

pub(crate) fn restore(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    let remembered = settings::memory(cx).and_then(|memory| memory.repo(&common));
    tab.ui.submodule.automatic = remembered.and_then(|view| view.update_submodules).unwrap_or(false);
    tab.ui.submodule.offer = remembered.and_then(|view| view.offer_initialise).unwrap_or(true);
}

fn remember_offer(tab: &mut Tab, on: bool, cx: &mut Context<Tab>) {
    tab.ui.submodule.offer = on;
    if !on {
        tab.ui.submodule.uninitialised.clear();
    }
    if let Some(repo) = &tab.repo {
        let common = repo.common_dir().to_owned();
        Memory::update(cx, |memory| memory.change_repo(&common, |view| view.offer_initialise = (!on).then_some(false)));
    }
    cx.notify();
}

fn refused(tab: &mut Tab, reason: Option<String>, cx: &mut Context<Tab>) -> bool {
    let Some(reason) = reason else { return false };
    tab.ui.menu = None;
    tab.ui.say(Tone::Info, reason);
    cx.notify();
    true
}

fn listed(tab: &mut Tab, path: &Path, cx: &mut Context<Tab>) -> Option<Module> {
    let found = tab.ui.submodule.find(path).cloned();
    if found.is_none() {
        tab.ui.say(Tone::Info, "That submodule is no longer listed");
        cx.notify();
    }
    found
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => open(tab, path, cx),
        Action::Show(path) => show(tab, path, cx),
        Action::Dismiss => {
            tab.ui.submodule.behind.clear();
            tab.ui.submodule.uninitialised.clear();
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
        Action::StopOffering => remember_offer(tab, false, cx),
        Action::ToggleOffer => {
            let on = !tab.ui.submodule.offer;
            remember_offer(tab, on, cx);
        }
        _ if tab.ui.busy() || tab.ui.dialog_open() => {
            tab.ui.menu = None;
            if tab.ui.busy() {
                tab.ui.say(Tone::Info, "Wait for the current operation to finish");
            }
            cx.notify();
        }
        Action::Add => {
            if refused(tab, networking(&tab.ui).or_else(|| operating(&tab.ui)), cx) {
                return;
            }
            tab.ui.menu = None;
            let seen = tab.ui.submodule.list.clone();
            prompt::ask(&mut tab.ui, prompt::Kind::Submodule(Ask::Add { seen }), cx);
        }
        Action::ChangeAddress(path) => {
            let Some(module) = listed(tab, &path, cx) else { return };
            if refused(tab, operating(&tab.ui), cx) {
                return;
            }
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Submodule(Ask::Address(module)), cx);
        }
        Action::Remove(path) => ask_remove(tab, path, cx),
        Action::InitialiseAll => initialise_waiting(tab, cx),
    }
}

fn ask_remove(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(module) = listed(tab, &path, cx) else { return };
    if refused(tab, removal_refusal(&tab.ui, &module), cx) {
        return;
    }
    if open_in_another_tab(tab, &module, cx) {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.menu = None;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || removal(&repo, &module),
        move |tab, prepared, cx| {
            if tab.epoch != epoch || tab.ui.dialog_open() || tab.ui.busy() {
                return;
            }
            match prepared {
                Ok(removal) => prompt::ask(&mut tab.ui, prompt::Kind::Submodule(Ask::Remove(removal)), cx),
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

fn open_in_another_tab(tab: &mut Tab, module: &Module, cx: &mut Context<Tab>) -> bool {
    let full = tab.ui.root.join(&module.path);
    if !crate::worktree::open_in_another_tab(&full, cx) {
        return false;
    }
    tab.ui.notices.show(Notice::new(
        Tone::Info,
        format!("Submodule {} is open in another tab", module.shown()),
        "Close that tab first, then remove the submodule.",
    ));
    cx.notify();
    true
}

fn initialise_waiting(tab: &mut Tab, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let expected = tab.ui.submodule.uninitialised.clone();
    if expected.is_empty() {
        tab.ui.say(Tone::Info, "Every submodule is initialised");
        cx.notify();
        return;
    }
    if refused(tab, networking(&tab.ui).or_else(|| operating(&tab.ui)).or_else(|| busy(&tab.ui)), cx) {
        return;
    }
    tab.run_network_or(
        cx,
        jobs::Spec::submodules(),
        "Initialising submodules…",
        "Submodules initialised",
        false,
        move |repo| initialise(&repo, &expected),
        |tab, cx| {
            tab.ui.submodule.uninitialised.clear();
            cx.notify();
        },
        |_, _| {},
    );
}

pub(crate) fn describe_ask(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::Add { .. } => {
            let mut spec = Spec::new("Add submodule", "Add submodule")
                .line("A submodule is another repository checked out in a folder of this one. This repository records which of its commits to use.")
                .line("Git clones it now. The new submodule and .gitmodules are then staged for you to commit.");
            spec.field = Some(Field {
                label: "Address",
                placeholder: "Repository URL or local path",
                initial: String::new(),
                masked: false,
                validate: Box::new(|text, _| address_problem(text)),
            });
            spec.second = Some(Field {
                label: "Folder",
                placeholder: "Taken from the address when left empty",
                initial: String::new(),
                masked: false,
                validate: Box::new(path_problem),
            });
            spec
        }
        Ask::Address(module) => {
            let mut spec = Spec::new(format!("Change the address of submodule {}", module.shown()), "Change address")
                .line("Git fetches the submodule's commits from this address. It is changed in .gitmodules, for you to commit, and in this repository's settings.");
            spec.field = Some(Field {
                label: "Address",
                placeholder: "Repository URL or local path",
                initial: module.url.as_deref().map(remotes::shown).unwrap_or_default(),
                masked: false,
                validate: Box::new(|text, _| address_problem(text)),
            });
            spec
        }
        Ask::Remove(removal) => remove_spec(removal),
    }
}

fn uncommitted_changes(count: usize) -> String {
    match count {
        1 => "1 uncommitted change".to_owned(),
        n => format!("{n} uncommitted changes"),
    }
}

pub(crate) fn remove_spec(removal: &Removal) -> Spec {
    let shown = removal.module.shown();
    let mut spec = Spec::new(format!("Remove submodule {shown}?"), "Remove submodule")
        .danger()
        .line(format!(
            "Its folder {shown} and its Git data are deleted, with any commits and changes made in it that are not pushed. They cannot be brought back."
        ));
    match (removal.changes, removal.unpushed) {
        (0, 0) => {}
        (changes, 0) => spec = spec.line(format!("It has {} now.", uncommitted_changes(changes))),
        (0, commits) => spec = spec.line(format!("{} in it {} on no remote.", notice::commits(commits), if commits == 1 { "is" } else { "are" })),
        (changes, commits) => {
            spec = spec.line(format!(
                "It has {} now, and {} in it {} on no remote.",
                uncommitted_changes(changes),
                notice::commits(commits),
                if commits == 1 { "is" } else { "are" }
            ))
        }
    }
    if let Some(worktree) = &removal.kept_for {
        spec = spec.line(format!("Its Git data is kept, because worktree {worktree} still uses it."));
    }
    spec.line("The removal and the change to .gitmodules are staged for you to commit.")
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Add { seen } => {
            let address = answer.text.trim().to_owned();
            let typed = answer.second.trim().trim_end_matches('/');
            let path = if typed.is_empty() { derived_path(&address) } else { Some(PathBuf::from(typed)) };
            let Some(path) = path else {
                return tab.error("Enter a folder for the submodule. It cannot be taken from this address.".to_owned(), cx);
            };
            let progress = format!("Adding submodule {}…", path.display());
            let addition = Addition { address, path, seen };
            tab.run_network_or(
                cx,
                jobs::Spec::adding_submodule(),
                &progress,
                "Submodule added",
                false,
                move |repo| add(&repo, &addition),
                |_, _| {},
                |_, _| {},
            );
        }
        Ask::Address(module) => {
            let address = answer.text.trim().to_owned();
            tab.run_operation_then(
                cx,
                "Changing the address…",
                "Address changed",
                false,
                move |repo| set_address(&repo, &module, &address),
                |_, _| {},
            );
        }
        Ask::Remove(removal) => {
            if open_in_another_tab(tab, &removal.module, cx) {
                return;
            }
            tab.run_operation_then(
                cx,
                "Removing the submodule…",
                "Submodule removed",
                false,
                move |repo| remove(&repo, &removal),
                |_, _| {},
            );
        }
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
            if !state.offered {
                state.offered = true;
                if state.offer {
                    state.uninitialised =
                        state.list.iter().filter(|module| !module.populated && module.recorded.is_some()).map(|module| module.path.clone()).collect();
                }
            }
            let list = &state.list;
            state.uninitialised.retain(|path| list.iter().any(|module| module.path == *path && !module.populated));
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

pub(crate) fn uninitialised_text(count: usize) -> String {
    match count {
        1 => "1 submodule is not initialised, so its folder is empty.".to_owned(),
        n => format!("{n} submodules are not initialised, so their folders are empty."),
    }
}

fn strip_frame(text: String) -> Div {
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
        .child(kit::fit(text).size(TEXT_SMALL).grow())
}

pub(crate) fn strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let ui = app.ui();
    let refusal = networking(ui).or_else(|| operating(ui)).or_else(|| busy(ui));
    let dismiss = kit::icon_btn("dismiss-submodules", "x", "Dismiss", view::Action::Submodule(Action::Dismiss), cx);
    let count = ui.submodule.behind.len();
    if count > 0 {
        return Some(
            strip_frame(strip_text(count))
                .child(
                    kit::btn("update-submodules", "Update submodules")
                        .small()
                        .enabled(refusal.is_none())
                        .tip(refusal.unwrap_or_else(|| "git submodule update --init --recursive".to_owned()))
                        .on(view::Action::Submodule(Action::UpdateAll), cx),
                )
                .child(dismiss),
        );
    }
    let waiting = ui.submodule.uninitialised.len();
    if waiting == 0 {
        return None;
    }
    Some(
        strip_frame(uninitialised_text(waiting))
            .child(
                kit::btn("initialise-submodules", "Initialise submodules")
                    .small()
                    .enabled(refusal.is_none())
                    .tip(refusal.unwrap_or_else(|| "git submodule update --init --recursive".to_owned()))
                    .on(view::Action::Submodule(Action::InitialiseAll), cx),
            )
            .child(
                kit::btn("stop-offering-submodules", "Don't ask again")
                    .small()
                    .tip("Turn this back on in Preferences, under This repository")
                    .on(view::Action::Submodule(Action::StopOffering), cx),
            )
            .child(dismiss),
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
        if let Some(url) = &module.url {
            lines.push(format!("From {}", remotes::shown(url)));
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

    fn staged(f: &Fixture) -> String {
        String::from_utf8(f.git(&["diff", "--cached", "--name-status"])).unwrap()
    }

    fn declared_url(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["config", "--file", ".gitmodules", "--get", &format!("submodule.{name}.url")])).unwrap().trim().to_owned()
    }

    fn library_path(n: &Nest) -> String {
        n.library.dir.to_str().unwrap().to_owned()
    }

    #[test]
    fn a_submodule_is_added_from_an_address_and_staged_with_gitmodules() {
        let n = nest();
        let f = &n.f;
        let head_before = head(f);
        let refs = f.git(&["for-each-ref"]);
        let address = library_path(&n);
        let addition = Addition { address: address.clone(), path: PathBuf::from("vendor/atlas"), seen: list(&f.repo).unwrap() };
        let outcome = add(&f.repo, &addition).unwrap();
        let library_head = head(&n.library);
        assert_eq!(outcome.title, "Added submodule vendor/atlas");
        assert_eq!(
            outcome.detail,
            format!("It is at {}, cloned from {address}. It is staged with .gitmodules: commit them to record it.", notice::short(&library_head))
        );
        assert_eq!(outcome.actions.len(), 1);
        assert_eq!(staged(f), "M\t.gitmodules\nA\tvendor/atlas\n");
        assert_eq!(declared_url(f, "vendor/atlas"), address);
        let listed = String::from_utf8(f.git(&["ls-files", "--stage", "--", "vendor/atlas"])).unwrap();
        assert!(listed.starts_with(&format!("160000 {library_head} 0\t")), "{listed}");
        assert!(f.repo.common_dir().join("modules/vendor/atlas").is_dir());
        assert_eq!(at(f, "vendor/atlas", &["rev-parse", "HEAD"]), library_head);
        assert_eq!(head(f), head_before);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        let now = list(&f.repo).unwrap();
        assert_eq!(now.len(), 2);
        assert_eq!(now[1].url.as_deref(), Some(address.as_str()));
        assert!(now[1].in_sync());
    }

    #[test]
    fn adding_refuses_a_changed_list_a_used_folder_and_an_old_submodules_data() {
        let n = nest();
        let f = &n.f;
        let address = library_path(&n);
        let seen = list(&f.repo).unwrap();
        let before = staged(f);
        let stale = Addition { address: address.clone(), path: PathBuf::from("atlas"), seen: Vec::new() };
        assert_eq!(add(&f.repo, &stale).unwrap_err(), "The submodules changed since you chose this. Reopen the action.");
        let taken = Addition { address: address.clone(), path: PathBuf::from("maps"), seen: seen.clone() };
        assert_eq!(add(&f.repo, &taken).unwrap_err(), "maps is already a submodule.");
        let file = Addition { address: address.clone(), path: PathBuf::from("trail.txt"), seen: seen.clone() };
        assert_eq!(add(&f.repo, &file).unwrap_err(), "trail.txt already exists and is not a folder.");
        fs::create_dir(f.dir.join("notes")).unwrap();
        fs::write(f.dir.join("notes/a.txt"), "A\n").unwrap();
        let full = Addition { address: address.clone(), path: PathBuf::from("notes"), seen: seen.clone() };
        assert_eq!(add(&f.repo, &full).unwrap_err(), "notes already exists and is not empty.");
        fs::create_dir_all(f.repo.common_dir().join("modules/atlas")).unwrap();
        let old = Addition { address: address.clone(), path: PathBuf::from("atlas"), seen: seen.clone() };
        assert!(add(&f.repo, &old).unwrap_err().starts_with("Git still keeps the data of an earlier submodule named atlas in "));
        let odd = Addition { address: "-upload-pack=x".to_owned(), path: PathBuf::from("odd"), seen };
        assert!(add(&f.repo, &odd).is_err());
        assert_eq!(staged(f), before);
        assert!(!f.dir.join("atlas").exists() && !f.dir.join("odd").exists());
    }

    #[test]
    fn adding_from_a_local_folder_is_explained_when_git_does_not_allow_it() {
        let library = Fixture::new();
        commit(&library, "map.txt", "Ridge map\n", "Draw the ridge map");
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        let empty = tempfile::NamedTempFile::new().unwrap();
        let _config = GlobalConfig::set(empty.path());
        let addition = Addition { address: library.dir.to_str().unwrap().to_owned(), path: PathBuf::from("maps"), seen: Vec::new() };
        assert_eq!(add(&f.repo, &addition).unwrap_err(), LOCAL_REFUSED);
        assert_eq!(staged(&f), "");
        assert!(!f.dir.join(".gitmodules").exists());
        assert!(list(&f.repo).unwrap().is_empty());
    }

    #[test]
    fn the_add_prompt_checks_the_address_and_takes_the_folder_from_it() {
        assert_eq!(derived_path("https://example.invalid/team/atlas.git"), Some(PathBuf::from("atlas")));
        assert_eq!(derived_path("git@example.invalid:team/atlas"), Some(PathBuf::from("atlas")));
        assert_eq!(derived_path("/Users/cedar/Maps/"), Some(PathBuf::from("Maps")));
        assert_eq!(derived_path("host:atlas.git"), Some(PathBuf::from("atlas")));
        assert_eq!(derived_path("/"), None);
        assert_eq!(derived_path("https://example.invalid/.."), None);
        assert_eq!(address_problem("   "), Some(String::new()));
        assert_eq!(address_problem("https://example.invalid/team/atlas.git"), None);
        assert!(address_problem("https://token:secret@example.invalid/atlas.git").is_some());
        assert!(address_problem("-oProxyCommand=x").is_some());
        let n = nest();
        let listed = list(&n.f.repo).unwrap();
        let root = &n.f.dir;
        assert_eq!(folder_problem(Path::new("libs/atlas"), root, &listed), None);
        assert_eq!(folder_problem(Path::new("/tmp/atlas"), root, &listed).unwrap(), "Enter a folder inside this repository, such as libs/maps.");
        assert_eq!(folder_problem(Path::new("libs/../atlas"), root, &listed).unwrap(), "Enter the folder without . or .. in it.");
        assert_eq!(folder_problem(Path::new("libs/.git"), root, &listed).unwrap(), "Choose a folder that is not named .git.");
        assert_eq!(folder_problem(Path::new("maps/inner"), root, &listed).unwrap(), "That folder is inside submodule maps.");
        fs::create_dir(root.join("empty")).unwrap();
        assert_eq!(folder_problem(Path::new("empty"), root, &listed), None);
    }

    #[test]
    fn changing_the_address_updates_gitmodules_the_settings_and_the_checkout() {
        let n = nest();
        let f = &n.f;
        let shown = list(&f.repo).unwrap().remove(0);
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let mirror = n.library.dir.with_extension("mirror");
        n.library.git(&["clone", "-q", "--bare", n.library.dir.to_str().unwrap(), mirror.to_str().unwrap()]);
        let address = mirror.to_str().unwrap().to_owned();
        let outcome = set_address(&f.repo, &shown, &address).unwrap();
        assert_eq!(outcome.title, "Changed the address of submodule maps");
        assert_eq!(
            outcome.detail,
            format!("It fetches from {address}, and so does its checkout. .gitmodules is changed: commit it to share the new address.")
        );
        assert_eq!(declared_url(f, "maps"), address);
        assert_eq!(String::from_utf8(f.git(&["config", "--get", "submodule.maps.url"])).unwrap().trim(), address);
        assert_eq!(at(f, "maps", &["remote", "get-url", "origin"]), address);
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain=v1"])).unwrap(), " M .gitmodules\n");
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        let refused = set_address(&f.repo, &shown, "/elsewhere/maps").unwrap_err();
        assert_eq!(refused, "The address of submodule maps changed since you chose this. Reopen the action.");
        assert_eq!(declared_url(f, "maps"), address);
        let now = list(&f.repo).unwrap().remove(0);
        assert_eq!(set_address(&f.repo, &now, &address).unwrap().title, "Nothing changed");
        assert!(set_address(&f.repo, &now, "-bad").is_err());
        let _ = fs::remove_dir_all(&mirror);
    }

    #[test]
    fn removing_a_submodule_stages_its_removal_and_deletes_its_folder_and_git_data() {
        let n = nest();
        let f = &n.f;
        let addition = Addition { address: library_path(&n), path: PathBuf::from("vendor/atlas"), seen: list(&f.repo).unwrap() };
        add(&f.repo, &addition).unwrap();
        f.git(&["commit", "-qm", "Add the atlas"]);
        for (key, value) in [("user.name", "Cedar"), ("user.email", "cedar@example.invalid"), ("commit.gpgSign", "false")] {
            at(f, "vendor/atlas", &["config", "--local", key, value]);
        }
        let head_before = head(f);
        let refs = f.git(&["for-each-ref"]);
        fs::write(f.dir.join("vendor/atlas/lake.txt"), "Lake\n").unwrap();
        at(f, "vendor/atlas", &["add", "--", "lake.txt"]);
        at(f, "vendor/atlas", &["commit", "-qm", "Mark the lake"]);
        fs::write(f.dir.join("vendor/atlas/map.txt"), "Redrawn\n").unwrap();
        let module = list(&f.repo).unwrap().into_iter().find(|module| module.path == Path::new("vendor/atlas")).unwrap();
        let prepared = removal(&f.repo, &module).unwrap();
        assert_eq!((prepared.changes, prepared.unpushed, prepared.kept_for.clone()), (1, 1, None));
        let spec = remove_spec(&prepared);
        assert!(spec.danger);
        assert_eq!(spec.title, "Remove submodule vendor/atlas?");
        assert_eq!(spec.lines[1], "It has 1 uncommitted change now, and 1 commit in it is on no remote.");
        let outcome = remove(&f.repo, &prepared).unwrap();
        assert_eq!(outcome.title, "Removed submodule vendor/atlas");
        assert_eq!(
            outcome.detail,
            "Its folder and its Git data are deleted. The removal and the change to .gitmodules are staged: commit them to record it."
        );
        assert_eq!(staged(f), "M\t.gitmodules\nD\tvendor/atlas\n");
        assert!(f.git(&["ls-files", "--", "vendor/atlas"]).is_empty());
        assert!(!f.dir.join("vendor/atlas").exists());
        let modules = f.repo.common_dir().join("modules");
        assert!(!modules.join("vendor").exists());
        assert!(modules.join("maps").is_dir());
        assert!(!fs::read_to_string(f.dir.join(".gitmodules")).unwrap().contains("atlas"));
        assert_eq!(String::from_utf8(f.git(&["config", "--get-regexp", r"^submodule\..*\.url$"])).unwrap().lines().count(), 1);
        assert_eq!(head(f), head_before);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(list(&f.repo).unwrap().len(), 1);
        assert_eq!(n.library.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    }

    #[test]
    fn removing_refuses_a_submodule_that_changed_or_an_unstaged_gitmodules() {
        let n = nest();
        let f = &n.f;
        let module = list(&f.repo).unwrap().remove(0);
        let prepared = removal(&f.repo, &module).unwrap();
        assert_eq!((prepared.changes, prepared.unpushed), (0, 0));
        fs::write(f.dir.join("maps/map.txt"), "Redrawn\n").unwrap();
        let refused = remove(&f.repo, &prepared).unwrap_err();
        assert_eq!(refused, "What is inside submodule maps changed since you chose to remove it. Reopen the action to see it.");
        at(f, "maps", &["checkout", "--", "map.txt"]);
        work_inside(f, "camp.txt", "Camp\n", "Mark the camp");
        assert!(remove(&f.repo, &prepared).unwrap_err().contains("changed since it was shown"));
        at(f, "maps", &["checkout", "-q", "HEAD~1"]);
        set_address(&f.repo, &module, "/elsewhere/maps").unwrap();
        let moved = list(&f.repo).unwrap().remove(0);
        let again = removal(&f.repo, &moved).unwrap();
        assert_eq!(
            remove(&f.repo, &again).unwrap_err(),
            "Stage or undo the changes to .gitmodules first. Removing a submodule changes that file and stages it."
        );
        assert!(f.dir.join("maps/map.txt").is_file());
        assert!(f.repo.common_dir().join("modules/maps").is_dir());
        assert_eq!(staged(f), "");
    }

    #[test]
    fn each_worktree_removes_only_its_own_submodule_data_and_keeps_data_another_one_points_at() {
        let n = nest();
        let f = &n.f;
        let main_data = f.repo.common_dir().join("modules/maps");
        let other = f.dir.with_extension("other");
        let shared = f.dir.with_extension("shared");
        f.git(&["worktree", "add", "-q", "--detach", other.to_str().unwrap()]);
        f.git(&["worktree", "add", "-q", "--detach", shared.to_str().unwrap()]);
        let opened = f.open(&other);
        checked(with_system_tools(&mut opened.command()).args(["submodule", "update", "--init", "-q"])).unwrap();
        let other_data = opened.git_dir().join("modules/maps");
        assert!(other_data.is_dir() && main_data.is_dir());
        fs::create_dir_all(shared.join("maps")).unwrap();
        fs::write(shared.join("maps/.git"), format!("gitdir: {}\n", main_data.display())).unwrap();
        let module = list(&f.repo).unwrap().remove(0);
        let name = shared.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(removal(&f.repo, &module).unwrap().kept_for.as_deref(), Some(name.as_str()));
        fs::remove_file(shared.join("maps/.git")).unwrap();
        let prepared = removal(&f.repo, &module).unwrap();
        assert_eq!(prepared.kept_for, None);
        let outcome = remove(&f.repo, &prepared).unwrap();
        assert!(outcome.detail.starts_with("Its folder and its Git data are deleted."), "{}", outcome.detail);
        assert!(!main_data.exists());
        assert!(other_data.is_dir());
        assert!(other.join("maps/map.txt").is_file());
        let module = list(&opened).unwrap().remove(0);
        let prepared = removal(&opened, &module).unwrap();
        assert_eq!(prepared.kept_for, None);
        remove(&opened, &prepared).unwrap();
        assert!(!other_data.exists());
        assert!(!other.join("maps").exists());
        let _ = fs::remove_dir_all(&other);
        let _ = fs::remove_dir_all(&shared);
    }

    #[test]
    fn uninitialised_submodules_are_initialised_and_a_changed_list_is_refused() {
        let n = nest();
        let f = &n.f;
        let start = recorded(f);
        f.git(&["submodule", "deinit", "-q", "-f", "maps"]);
        let paths = [PathBuf::from("maps")];
        assert!(initialise(&f.repo, &[]).is_err());
        let outcome = initialise(&f.repo, &paths).unwrap();
        assert_eq!(outcome.title, "Initialised submodule maps");
        assert_eq!(outcome.detail, "Checked out at the commits this repository records: maps.");
        assert_eq!(at(f, "maps", &["rev-parse", "HEAD"]), start);
        assert_eq!(initialise(&f.repo, &paths).unwrap_err(), "The submodules changed since they were shown. Look again, then initialise them.");
        assert!(initialise(&f.repo, &[PathBuf::from("atlas")]).is_err());
    }

    #[test]
    fn a_folder_without_gitmodules_lists_nothing_and_a_declared_path_must_be_plain() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        assert!(list(&f.repo).unwrap().is_empty());
        fs::write(f.dir.join(".gitmodules"), "[submodule \"odd\"]\n\tpath = ../outside\n[submodule \"maps\"]\n\tpath = maps\n").unwrap();
        assert_eq!(declared(&f.repo).unwrap(), [("maps".to_owned(), PathBuf::from("maps"), None)]);
        assert!(list(&f.repo).unwrap().is_empty());
    }
}
