use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Target, busy, on_repo},
    notice::{self, Notice, Outcome},
    prompt::{self, Answer, Field, Note, Spec},
    refs::Kind,
    repository::{Repository, Revisions, checked, raw_path, same_place, validate_oid},
    theme::*,
    view::{self, Desk, Menu, Tone, Ui},
};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, PathPromptOptions, Window, div,
    prelude::*, px, rgb,
};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Worktree {
    pub(crate) path: PathBuf,
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<OsString>,
    pub(crate) locked: Option<String>,
    pub(crate) prunable: Option<String>,
    pub(crate) current: bool,
    pub(crate) main: bool,
}

impl Worktree {
    pub(crate) fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| self.path.display().to_string(), |name| name.to_string_lossy().into_owned())
    }

    pub(crate) fn on(&self) -> String {
        match (&self.branch, &self.head) {
            (Some(branch), _) => branch.to_string_lossy().into_owned(),
            (None, Some(head)) => format!("detached at {}", notice::short(head)),
            (None, None) => "no commits yet".to_owned(),
        }
    }
}

pub(crate) fn parse(output: &[u8], root: &Path) -> Vec<Worktree> {
    let mut found = Vec::new();
    let mut open: Option<(Worktree, bool)> = None;
    let mut records = 0;
    let finish = |open: Option<(Worktree, bool)>, found: &mut Vec<Worktree>| {
        if let Some((worktree, false)) = open {
            found.push(worktree);
        }
    };
    for field in output.split(|&b| b == 0) {
        if field.is_empty() {
            finish(open.take(), &mut found);
            continue;
        }
        let (key, value) = match field.iter().position(|&b| b == b' ') {
            Some(at) => (&field[..at], Some(&field[at + 1..])),
            None => (field, None),
        };
        if key == b"worktree" {
            finish(open.take(), &mut found);
            let Some(path) = value.filter(|path| !path.is_empty()) else { continue };
            let path = raw_path(path);
            let current = same_place(&path, root);
            let main = records == 0;
            records += 1;
            open = Some((Worktree { path, current, main, ..Worktree::default() }, false));
            continue;
        }
        let Some((worktree, bare)) = open.as_mut() else { continue };
        let text = || value.map(|value| String::from_utf8_lossy(value).into_owned()).unwrap_or_default();
        match key {
            b"HEAD" => {
                let oid = text();
                if validate_oid(&oid).is_ok() && oid.bytes().any(|b| b != b'0') {
                    worktree.head = Some(oid);
                }
            }
            b"branch" => {
                worktree.branch = value
                    .and_then(|full| full.strip_prefix(b"refs/heads/"))
                    .filter(|name| !name.is_empty())
                    .map(|name| OsStr::from_bytes(name).to_owned());
            }
            b"bare" => *bare = true,
            b"locked" => worktree.locked = Some(text()),
            b"prunable" => worktree.prunable = Some(text()),
            _ => {}
        }
    }
    finish(open.take(), &mut found);
    found
}

pub(crate) fn list(repo: &Repository) -> Result<Vec<Worktree>, String> {
    let output = checked(repo.read_command().args(["worktree", "list", "--porcelain", "-z"]))?;
    Ok(parse(&output, &repo.root))
}

pub(crate) fn uncommitted(repo: &Repository, path: &Path) -> Result<usize, String> {
    if !path.is_dir() {
        return Err(format!("{} is missing", path.display()));
    }
    let output = checked(
        repo.read_command()
            .arg("-C")
            .arg(path)
            .args(["status", "--porcelain=v2", "--untracked-files=normal", "--ignore-submodules=none"]),
    )?;
    Ok(output.split(|&b| b == b'\n').filter(|line| !line.is_empty() && !line.starts_with(b"#")).count())
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) list: Vec<Worktree>,
    ticket: u64,
    counts: HashMap<PathBuf, Result<usize, String>>,
    asked: bool,
}

impl State {
    pub(crate) fn linked(&self) -> bool {
        self.list.len() > 1
    }

    pub(crate) fn others(&self) -> impl Iterator<Item = &Worktree> {
        self.list.iter().filter(|worktree| !worktree.current)
    }

    pub(crate) fn find(&self, path: &Path) -> Option<&Worktree> {
        self.list.iter().find(|worktree| worktree.path == path)
    }

    pub(crate) fn holding(&self, branch: &OsStr) -> Option<&Worktree> {
        self.others().find(|worktree| worktree.branch.as_deref() == Some(branch))
    }

    pub(crate) fn at<'a>(&'a self, oid: &'a str) -> impl Iterator<Item = &'a Worktree> + 'a {
        self.others().filter(move |worktree| worktree.head.as_deref() == Some(oid))
    }

    pub(crate) fn count(&self, path: &Path) -> Option<&Result<usize, String>> {
        self.counts.get(path)
    }

    pub(crate) fn wanted(&self) -> bool {
        !self.asked && self.others().next().is_some()
    }
}

pub(crate) fn in_use(ui: &Ui, branch: &OsStr) -> Option<String> {
    ui.worktree.holding(branch).map(|worktree| format!("Checked out in worktree {}", worktree.name()))
}

pub(crate) fn changes_line(state: &State, worktree: &Worktree) -> String {
    match state.count(&worktree.path) {
        Some(Ok(0)) => "No uncommitted changes".to_owned(),
        Some(Ok(1)) => "1 uncommitted change".to_owned(),
        Some(Ok(count)) => format!("{count} uncommitted changes"),
        Some(Err(_)) => "Its changes could not be counted".to_owned(),
        None => "Counting uncommitted changes…".to_owned(),
    }
}

pub(crate) fn tip(state: &State, worktree: &Worktree) -> String {
    let mut lines = vec![worktree.path.display().to_string(), format!("On {}", worktree.on())];
    if worktree.current {
        lines.push("Open in this tab".to_owned());
    } else {
        lines.push(changes_line(state, worktree));
    }
    match worktree.locked.as_deref() {
        Some("") => lines.push("Locked".to_owned()),
        Some(reason) => lines.push(format!("Locked: {reason}")),
        None => {}
    }
    if worktree.prunable.is_some() {
        lines.push("Its folder is missing".to_owned());
    }
    lines.join("\n")
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(PathBuf),
    Count,
    New(Start),
    ChooseFolder,
    Remove(PathBuf),
    Lock(PathBuf),
    Unlock(PathBuf),
    Prune,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    Head,
    Branch(OsString),
    Commit(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Base {
    Branch { name: OsString, tip: String, held: Option<String> },
    Commit(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Addition {
    pub(crate) folder: PathBuf,
    pub(crate) base: Base,
    pub(crate) branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removal {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<OsString>,
    pub(crate) changes: usize,
    pub(crate) only: usize,
    pub(crate) subjects: Vec<(String, String)>,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Add { base: Base, folder: PathBuf },
    Remove(Removal),
    Lock { path: PathBuf, name: String },
}

const LISTED: usize = 5;

fn named(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

fn uncommitted_changes(count: usize) -> String {
    match count {
        1 => "1 uncommitted change".to_owned(),
        n => format!("{n} uncommitted changes"),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute())
}

pub(crate) fn typed_folder(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(String::new());
    }
    let path = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            home().ok_or("Your home folder cannot be found, so enter the full path.")?.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    };
    if !path.is_absolute() {
        return Err("Enter the folder's full path, starting with / or ~/.".to_owned());
    }
    if path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
        return Err("Enter the folder's path without . or .. in it.".to_owned());
    }
    Ok(path)
}

fn real(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(found) = fs::canonicalize(at) {
            return rest.iter().rev().fold(found, |path: PathBuf, part: &&OsStr| path.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

pub(crate) fn folder_problem(folder: &Path, worktrees: &[Worktree]) -> Option<String> {
    let wanted = real(folder);
    for worktree in worktrees {
        let inside = folder.starts_with(&worktree.path) || wanted.starts_with(real(&worktree.path));
        if !inside {
            continue;
        }
        return Some(if same_place(folder, &worktree.path) || wanted == real(&worktree.path) {
            format!("That is the folder of worktree {}.", worktree.name())
        } else {
            format!("That folder is inside worktree {}. Choose one outside it.", worktree.name())
        });
    }
    match fs::symlink_metadata(folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("{} cannot be read: {error}.", folder.display())),
        Ok(meta) if !meta.is_dir() => Some(format!("{} already exists and is not a folder.", folder.display())),
        Ok(_) => match fs::read_dir(folder).map(|mut entries| entries.next().is_none()) {
            Ok(true) => None,
            Ok(false) => Some(format!("{} already exists and is not empty.", folder.display())),
            Err(error) => Some(format!("{} cannot be read: {error}.", folder.display())),
        },
    }
}

fn label(base: &Base) -> String {
    match base {
        Base::Branch { name, .. } => name.to_string_lossy().replace('/', "-"),
        Base::Commit(oid) => notice::short(oid).to_owned(),
    }
}

pub(crate) fn default_folder(ui: &Ui, base: &Base) -> PathBuf {
    let main = ui.worktree.list.iter().find(|worktree| worktree.main).map_or_else(|| ui.root.clone(), |worktree| worktree.path.clone());
    let parent = main.parent().map_or_else(|| main.clone(), Path::to_path_buf);
    let stem = format!("{}-{}", named(&main), label(base));
    (1..100)
        .map(|n| if n == 1 { parent.join(&stem) } else { parent.join(format!("{stem}-{n}")) })
        .find(|folder| folder_problem(folder, &ui.worktree.list).is_none())
        .unwrap_or_else(|| parent.join(stem))
}

fn base_of(ui: &Ui, start: Start) -> Result<Base, String> {
    let branch = |name: OsString| -> Result<Base, String> {
        let tip = ui
            .refs
            .local(&name)
            .and_then(|item| item.oid.clone())
            .ok_or_else(|| format!("Branch {} is no longer listed", name.to_string_lossy()))?;
        let shown = name.to_string_lossy().into_owned();
        let held = if !ui.detached && ui.branch == shown {
            Some(format!("{shown} is checked out in this tab"))
        } else {
            ui.worktree.holding(&name).map(|worktree| format!("{shown} is checked out in worktree {}", worktree.name()))
        };
        Ok(Base::Branch { name, tip, held })
    };
    match start {
        Start::Branch(name) => branch(name),
        Start::Commit(oid) => {
            validate_oid(&oid)?;
            Ok(Base::Commit(oid))
        }
        Start::Head => match (&ui.head, ui.detached) {
            (None, _) => Err("Make the first commit before adding a worktree".to_owned()),
            (Some(head), true) => Ok(Base::Commit(head.clone())),
            (Some(_), false) => branch(OsString::from(&ui.branch)),
        },
    }
}

pub(crate) fn describe(ask: &Ask, ui: &Ui) -> Spec {
    match ask {
        Ask::Add { base, folder } => add_spec(base, folder),
        Ask::Remove(removal) => remove_spec(removal, ui.prompt.ticked()),
        Ask::Lock { name, .. } => {
            let mut spec = Spec::new(format!("Lock worktree {name}"), "Lock worktree").line(
                "Git will not prune or remove a locked worktree, for example while its folder is on a disk that is not connected.",
            );
            spec.field = Some(Field {
                label: "Reason",
                placeholder: "Optional",
                initial: String::new(),
                masked: false,
                validate: Box::new(|text, _| text.contains(['\n', '\0']).then(|| "Enter the reason on one line.".to_owned())),
            });
            spec
        }
    }
}

fn add_spec(base: &Base, folder: &Path) -> Spec {
    let (title, tick, ticked, held) = match base {
        Base::Branch { name, held, .. } => {
            let name = name.to_string_lossy();
            (format!("New worktree from {name}"), format!("Create a new branch from {name}"), held.is_some(), held.clone())
        }
        Base::Commit(oid) => {
            (format!("New worktree at {}", notice::short(oid)), "Create a new branch here".to_owned(), true, None)
        }
    };
    let mut spec = Spec::new(title, "Create worktree")
        .line("A worktree is another folder with its own checkout of this repository. It shares the commits and branches.");
    if let Some(held) = &held {
        spec = spec.line(format!("{held}, so the new worktree needs a branch of its own."));
    }
    if matches!(base, Base::Commit(_)) {
        spec = spec.line("Without a new branch, the worktree starts on no branch at this commit.");
    }
    spec.field = Some(Field {
        label: "Folder",
        placeholder: "Full path of a new or empty folder",
        initial: folder.display().to_string(),
        masked: false,
        validate: Box::new(|text, ui| match typed_folder(text) {
            Err(problem) => Some(problem),
            Ok(path) => folder_problem(&path, &ui.worktree.list),
        }),
    });
    spec.note = Some(Note {
        text: "Keep it outside this repository's folders.".to_owned(),
        link: Some(("Choose a folder…".into(), view::Action::Worktree(Action::ChooseFolder))),
    });
    spec.tick = Some((tick.into(), ticked));
    spec.fixed_tick = held.is_some();
    spec.tick_opens_second = true;
    spec.second = Some(Field {
        label: "New branch",
        placeholder: "Name of the new branch",
        initial: String::new(),
        masked: false,
        validate: Box::new(|text, ui| {
            if !ui.prompt.ticked() {
                return None;
            }
            if text.is_empty() {
                return Some(String::new());
            }
            ui.refs.name_problem(Kind::Branch, text)
        }),
    });
    spec
}

fn remove_spec(removal: &Removal, deleting_branch: bool) -> Spec {
    let mut spec = Spec::new(format!("Remove worktree {}?", removal.name), "Remove worktree");
    let folder = removal.path.display();
    spec = match removal.changes {
        0 => spec.line(format!("Its folder {folder} is deleted, with any ignored files in it.")),
        n => spec
            .line(format!("Its folder {folder} is deleted with its {}.", uncommitted_changes(n)))
            .line("They have not been committed, so they cannot be brought back.")
            .danger(),
    };
    let Some(branch) = &removal.branch else { return spec };
    let branch = branch.to_string_lossy();
    spec = spec.line(match removal.only {
        0 => format!("Branch {branch} is kept unless you delete it too. Its commits are also on other branches or tags."),
        1 => format!("Branch {branch} is kept unless you delete it too. 1 commit is only on it:"),
        n => format!("Branch {branch} is kept unless you delete it too. {n} commits are only on it:"),
    });
    for (short, subject) in &removal.subjects {
        spec = spec.line(format!("{short}  {subject}"));
    }
    match removal.only.saturating_sub(removal.subjects.len()) {
        0 => {}
        1 => spec = spec.line("and 1 more commit"),
        n => spec = spec.line(format!("and {n} more commits")),
    }
    spec.tick = Some((format!("Also delete branch {branch}").into(), false));
    if deleting_branch && removal.only > 0 {
        spec = spec.danger();
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Add { base, .. } => {
            let folder = match typed_folder(&answer.text) {
                Ok(folder) => folder,
                Err(problem) => return tab.error(problem, cx),
            };
            let branch = answer.tick.then_some(answer.second);
            let addition = Addition { folder, base, branch };
            tab.run_operation_then(
                cx,
                "Creating the worktree…",
                "Worktree created",
                false,
                move |repo| add(&repo, &addition),
                |_, _| {},
            );
        }
        Ask::Remove(removal) => {
            if open_in_another_tab(&removal.path, cx) {
                return open_elsewhere_refused(tab, &removal.name, cx);
            }
            let delete_branch = answer.tick && removal.branch.is_some();
            tab.run_operation_then(
                cx,
                "Removing the worktree…",
                "Worktree removed",
                false,
                move |repo| remove(&repo, &removal, delete_branch),
                |_, _| {},
            );
        }
        Ask::Lock { path, .. } => {
            let reason = answer.text;
            tab.run_operation_then(
                cx,
                "Locking the worktree…",
                "Worktree locked",
                false,
                move |repo| lock(&repo, &path, &reason),
                |_, _| {},
            );
        }
    }
}

fn fresh(repo: &Repository, path: &Path) -> Result<Worktree, String> {
    list(repo)?
        .into_iter()
        .find(|worktree| worktree.path == path || same_place(&worktree.path, path))
        .ok_or_else(|| format!("Worktree {} is no longer listed. Reopen the action.", named(path)))
}

fn branch_exists(repo: &Repository, name: &str) -> Result<bool, String> {
    let full = crate::branch::full(OsStr::new(name));
    let listed = checked(repo.read_command().args(["for-each-ref", "--format=%(refname)", "--"]).arg(&full))?;
    Ok(listed.split(|&b| b == b'\n').any(|line| line == full.as_bytes()))
}

pub(crate) fn add(repo: &Repository, addition: &Addition) -> Result<Outcome, String> {
    let folder = &addition.folder;
    if !folder.is_absolute() {
        return Err("Enter the folder's full path.".to_owned());
    }
    let listed = list(repo)?;
    if let Some(problem) = folder_problem(folder, &listed) {
        return Err(problem);
    }
    let start = match &addition.base {
        Base::Branch { name, tip, held } => {
            crate::branch::plain(name)?;
            repo.expect_ref(&crate::branch::full(name), tip)?;
            if addition.branch.is_none() {
                let shown = name.to_string_lossy();
                if let Some(holder) = listed.iter().find(|worktree| worktree.branch.as_deref() == Some(name.as_os_str())) {
                    return Err(format!("{shown} is checked out in worktree {}. Reopen the action.", holder.name()));
                }
                if let Some(held) = held {
                    return Err(format!("{held}, so the new worktree needs a branch of its own."));
                }
            }
            tip.clone()
        }
        Base::Commit(oid) => {
            validate_oid(oid)?;
            checked(
                repo.read_command()
                    .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
                    .arg(format!("{oid}^{{commit}}")),
            )
            .map_err(|_| format!("Commit {} is no longer in this repository.", notice::short(oid)))?;
            oid.clone()
        }
    };
    let mut command = repo.command();
    command.args(["worktree", "add", "--no-guess-remote", "--checkout"]);
    match (&addition.branch, &addition.base) {
        (Some(new), _) => {
            if let Some(problem) = crate::refs::name_problem(Kind::Branch, new, []) {
                return Err(problem);
            }
            if branch_exists(repo, new)? {
                return Err(format!("A branch named {new} already exists. Reopen the action and choose another name."));
            }
            command.args(["--no-track", "-b"]).arg(new).arg("--").arg(folder).arg(&start);
        }
        (None, Base::Branch { name, .. }) => {
            command.arg("--").arg(folder).arg(name);
        }
        (None, Base::Commit(oid)) => {
            command.args(["--detach", "--"]).arg(folder).arg(oid);
        }
    }
    checked(&mut command)?;
    let name = named(folder);
    let on = match (&addition.branch, &addition.base) {
        (Some(new), _) => format!("on the new branch {new}"),
        (None, Base::Branch { name, .. }) => format!("on {}", name.to_string_lossy()),
        (None, Base::Commit(oid)) => format!("at {}, on no branch", notice::short(oid)),
    };
    Ok(Outcome::titled(format!("Created worktree {name}"), format!("{}, {on}.", folder.display()))
        .offer("Open in a new tab", view::Action::Worktree(Action::Open(folder.clone())))
        .undone_as(format!("Create worktree {name}"))
        .not_undoable(format!("Creating worktree {name} cannot be undone. Remove it from the Worktrees section instead."))
        .sticky())
}

pub(crate) fn remove(repo: &Repository, removal: &Removal, delete_branch: bool) -> Result<Outcome, String> {
    let name = &removal.name;
    let found = fresh(repo, &removal.path)?;
    if found.main {
        return Err("The main worktree cannot be removed.".to_owned());
    }
    if same_place(&found.path, &repo.root) {
        return Err(format!("Worktree {name} is open in this tab."));
    }
    if found.locked.is_some() {
        return Err(format!("Worktree {name} is locked. Unlock it first."));
    }
    if found.prunable.is_some() {
        return Err(format!("The folder of worktree {name} is missing. Use Remove missing worktrees instead."));
    }
    if found.head != removal.head || found.branch != removal.branch {
        return Err(format!("Worktree {name} changed since you chose it. Reopen the action."));
    }
    let now = uncommitted(repo, &found.path)?;
    if now != removal.changes {
        return Err(match removal.changes {
            0 => format!("Worktree {name} has {} now. Reopen the action to see them.", uncommitted_changes(now)),
            _ => format!("The uncommitted changes in worktree {name} changed since you chose it. Reopen the action."),
        });
    }
    let deleted = match (&removal.branch, &removal.head) {
        (Some(branch), Some(tip)) if delete_branch => {
            crate::branch::plain(branch)?;
            repo.expect_ref(&crate::branch::full(branch), tip)?;
            Some((branch, tip))
        }
        (None, _) | (_, None) if delete_branch => return Err(format!("Worktree {name} is not on a branch.")),
        _ => None,
    };
    let mut command = repo.command();
    command.args(["worktree", "remove"]);
    if removal.changes > 0 {
        command.arg("--force");
    }
    checked(command.arg("--").arg(&found.path))?;
    let Some((branch, tip)) = deleted else {
        let detail = match &removal.branch {
            Some(branch) => format!("Its folder is deleted. Branch {} is kept.", branch.to_string_lossy()),
            None => "Its folder is deleted.".to_owned(),
        };
        return Ok(Outcome::titled(format!("Removed worktree {name}"), detail)
            .undone_as(format!("Remove worktree {name}"))
            .not_undoable(format!("Removing worktree {name} cannot be undone.")));
    };
    let shown = branch.to_string_lossy().into_owned();
    let mut outcome = crate::branch::delete(repo, branch, tip)
        .map_err(|error| format!("Worktree {name} was removed, but branch {shown} was not deleted: {error}"))?;
    outcome.title = format!("Removed worktree {name} and branch {shown}");
    outcome.detail = format!("Undo brings back branch {shown}, not the worktree's folder.");
    Ok(outcome)
}

pub(crate) fn lock(repo: &Repository, path: &Path, reason: &str) -> Result<Outcome, String> {
    let found = fresh(repo, path)?;
    let name = found.name();
    if found.main {
        return Err("The main worktree cannot be locked.".to_owned());
    }
    if found.locked.is_some() {
        return Err(format!("Worktree {name} is already locked. Reopen the action."));
    }
    let reason = reason.trim();
    if reason.contains(['\n', '\0']) {
        return Err("Enter the reason on one line.".to_owned());
    }
    let mut command = repo.command();
    command.args(["worktree", "lock"]);
    if !reason.is_empty() {
        command.arg("--reason").arg(reason);
    }
    checked(command.arg("--").arg(&found.path))?;
    let detail = "Git will not prune or remove it until it is unlocked.";
    Ok(Outcome::titled(
        format!("Locked worktree {name}"),
        if reason.is_empty() { detail.to_owned() } else { format!("{reason}. {detail}") },
    ))
}

pub(crate) fn unlock(repo: &Repository, path: &Path) -> Result<Outcome, String> {
    let found = fresh(repo, path)?;
    let name = found.name();
    if found.locked.is_none() {
        return Err(format!("Worktree {name} is not locked any more. Reopen the action."));
    }
    checked(repo.command().args(["worktree", "unlock", "--"]).arg(&found.path))?;
    Ok(Outcome::titled(format!("Unlocked worktree {name}"), "It can be removed or pruned again."))
}

pub(crate) fn prune(repo: &Repository, missing: &[PathBuf]) -> Result<Outcome, String> {
    if missing.is_empty() {
        return Err("No worktree's folder is missing.".to_owned());
    }
    let listed = list(repo)?;
    let now: Vec<&PathBuf> = listed.iter().filter(|worktree| worktree.prunable.is_some()).map(|worktree| &worktree.path).collect();
    if now.len() != missing.len() || !missing.iter().all(|path| now.contains(&path)) {
        return Err("The missing worktrees changed since you chose this. Reopen the action.".to_owned());
    }
    checked(repo.command().args(["worktree", "prune"]))?;
    let names: Vec<String> = missing.iter().map(|path| named(path)).collect();
    let title = match names.as_slice() {
        [one] => format!("Removed missing worktree {one}"),
        _ => format!("Removed {} missing worktrees", names.len()),
    };
    Ok(Outcome::titled(title, format!("Git no longer lists {}. Branches are kept.", names.join(", "))))
}

fn holder<'a>(ui: &'a Ui, target: &Target) -> Option<&'a Worktree> {
    match target {
        Target::Branch(name) => ui.worktree.holding(name),
        Target::RemoteBranch(name) => {
            let (_, local) = crate::branch::split_remote(&ui.refs.remotes, name)?;
            ui.worktree.holding(&local)
        }
        _ => None,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "worktree.open",
        Group::Switch,
        |_, _| "Open in a new tab".into(),
        |ui, target, _| {
            let Target::Worktree(path) = target else { return Availability::Hidden };
            match ui.worktree.find(path) {
                None => Availability::Hidden,
                Some(worktree) if worktree.current => Availability::Disabled("This worktree is open in this tab".to_owned()),
                Some(worktree) if worktree.prunable.is_some() => Availability::Disabled("Its folder is missing".to_owned()),
                Some(_) => Availability::Enabled,
            }
        },
        |_, target| match target {
            Target::Worktree(path) => Some(view::Action::Worktree(Action::Open(path.clone()))),
            _ => None,
        },
    )
    .icon("folder"),
    Command::new(
        "branch.open-worktree",
        Group::Switch,
        |_, _| "Open that worktree".into(),
        |ui, target, _| match holder(ui, target) {
            None => Availability::Hidden,
            Some(worktree) if worktree.prunable.is_some() => {
                Availability::Disabled(format!("The folder of worktree {} is missing", worktree.name()))
            }
            Some(_) => Availability::Enabled,
        },
        |ui, target| holder(ui, target).map(|worktree| view::Action::Worktree(Action::Open(worktree.path.clone()))),
    )
    .icon("folder"),
    Command::new(
        "worktree.new",
        Group::Create,
        |_, target| match target {
            Target::Branch(name) => format!("New worktree from {}…", name.to_string_lossy()),
            Target::Commit(_) => "New worktree from this commit…".into(),
            _ => "New worktree…".into(),
        },
        |ui, target, _| match target {
            Target::Branch(name) if ui.refs.local(name).is_some() => Availability::unless(busy(ui)),
            Target::Commit(_) if !ui.repo.is_empty() => Availability::unless(busy(ui)),
            Target::Repo if on_repo(ui, target) => Availability::unless(
                busy(ui).or_else(|| ui.head.is_none().then(|| "Make the first commit before adding a worktree".to_owned())),
            ),
            _ => Availability::Hidden,
        },
        |_, target| {
            let start = match target {
                Target::Branch(name) => Start::Branch(name.clone()),
                Target::Commit(oid) => Start::Commit(oid.clone()),
                Target::Repo => Start::Head,
                _ => return None,
            };
            Some(view::Action::Worktree(Action::New(start)))
        },
    )
    .words(&["add worktree", "create worktree", "another folder", "git worktree add"])
    .icon("plus"),
    Command::new(
        "worktree.lock",
        Group::Manage,
        |_, _| "Lock worktree…".into(),
        |ui, target, _| {
            let Target::Worktree(path) = target else { return Availability::Hidden };
            match ui.worktree.find(path) {
                None => Availability::Hidden,
                Some(worktree) if worktree.locked.is_some() => Availability::Hidden,
                Some(worktree) if worktree.main => Availability::Disabled("The main worktree cannot be locked".to_owned()),
                Some(_) => Availability::unless(busy(ui)),
            }
        },
        |_, target| match target {
            Target::Worktree(path) => Some(view::Action::Worktree(Action::Lock(path.clone()))),
            _ => None,
        },
    )
    .icon("lock"),
    Command::new(
        "worktree.unlock",
        Group::Manage,
        |_, _| "Unlock worktree".into(),
        |ui, target, _| {
            let Target::Worktree(path) = target else { return Availability::Hidden };
            match ui.worktree.find(path) {
                Some(worktree) if worktree.locked.is_some() => Availability::unless(busy(ui)),
                _ => Availability::Hidden,
            }
        },
        |_, target| match target {
            Target::Worktree(path) => Some(view::Action::Worktree(Action::Unlock(path.clone()))),
            _ => None,
        },
    )
    .icon("lock"),
    Command::new(
        "worktree.remove",
        Group::Manage,
        |_, _| "Remove worktree…".into(),
        |ui, target, _| {
            let Target::Worktree(path) = target else { return Availability::Hidden };
            match ui.worktree.find(path) {
                None => Availability::Hidden,
                Some(worktree) => Availability::unless(removal_refusal(worktree).or_else(|| busy(ui))),
            }
        },
        |_, target| match target {
            Target::Worktree(path) => Some(view::Action::Worktree(Action::Remove(path.clone()))),
            _ => None,
        },
    )
    .icon("trash")
    .danger(),
    Command::new(
        "worktree.prune",
        Group::Manage,
        |_, _| "Remove missing worktrees".into(),
        |ui, target, _| {
            if !on_repo(ui, target) || !ui.worktree.linked() {
                return Availability::Hidden;
            }
            let missing = ui.worktree.list.iter().any(|worktree| worktree.prunable.is_some());
            Availability::unless(
                busy(ui).or_else(|| (!missing).then(|| "No worktree's folder is missing".to_owned())),
            )
        },
        |_, _| Some(view::Action::Worktree(Action::Prune)),
    )
    .words(&["prune worktrees", "git worktree prune"])
    .icon("trash"),
];

fn removal_refusal(worktree: &Worktree) -> Option<String> {
    if worktree.main {
        Some("The main worktree cannot be removed".to_owned())
    } else if worktree.current {
        Some("This worktree is open in this tab".to_owned())
    } else if worktree.locked.is_some() {
        Some("It is locked. Unlock it first".to_owned())
    } else if worktree.prunable.is_some() {
        Some("Its folder is missing. Use Remove missing worktrees".to_owned())
    } else {
        None
    }
}

pub(crate) fn refused_switch(tab: &mut Tab, branch: &OsStr, cx: &mut Context<Tab>) -> bool {
    let Some(worktree) = tab.ui.worktree.holding(branch) else { return false };
    let notice = Notice::new(
        Tone::Info,
        format!("{} is checked out in worktree {}", branch.to_string_lossy(), worktree.name()),
        "Git keeps a branch in one worktree at a time.",
    )
    .offer("Open that worktree", view::Action::Worktree(Action::Open(worktree.path.clone())));
    tab.ui.notices.show(notice);
    cx.notify();
    true
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => open(tab, path, cx),
        Action::Count => count(tab, cx),
        Action::ChooseFolder => choose_folder(tab, window, cx),
        _ if tab.ui.busy() || tab.ui.dialog_open() => {
            tab.ui.menu = None;
            if tab.ui.busy() {
                tab.ui.say(Tone::Info, "Wait for the current operation to finish");
            }
            cx.notify();
        }
        Action::New(start) => new(tab, start, cx),
        Action::Remove(path) => ask_remove(tab, path, cx),
        Action::Lock(path) => {
            let Some(worktree) = listed(tab, &path, cx) else { return };
            if let Some(reason) = worktree
                .main
                .then(|| "The main worktree cannot be locked".to_owned())
                .or_else(|| worktree.locked.is_some().then(|| format!("Worktree {} is already locked", worktree.name())))
            {
                tab.ui.say(Tone::Info, reason);
                cx.notify();
                return;
            }
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Worktree(Ask::Lock { path: worktree.path.clone(), name: worktree.name() }), cx);
        }
        Action::Unlock(path) => {
            let Some(worktree) = listed(tab, &path, cx) else { return };
            tab.run_operation_then(
                cx,
                "Unlocking the worktree…",
                "Worktree unlocked",
                false,
                move |repo| unlock(&repo, &worktree.path),
                |_, _| {},
            );
        }
        Action::Prune => {
            let missing: Vec<PathBuf> = tab
                .ui
                .worktree
                .list
                .iter()
                .filter(|worktree| worktree.prunable.is_some())
                .map(|worktree| worktree.path.clone())
                .collect();
            if missing.is_empty() {
                tab.ui.say(Tone::Info, "No worktree's folder is missing");
                cx.notify();
                return;
            }
            tab.run_operation_then(
                cx,
                "Removing missing worktrees…",
                "Missing worktrees removed",
                false,
                move |repo| prune(&repo, &missing),
                |_, _| {},
            );
        }
    }
}

fn listed(tab: &mut Tab, path: &Path, cx: &mut Context<Tab>) -> Option<Worktree> {
    let found = tab.ui.worktree.find(path).cloned();
    if found.is_none() {
        tab.ui.say(Tone::Info, "That worktree is no longer listed");
        cx.notify();
    }
    found
}

fn new(tab: &mut Tab, start: Start, cx: &mut Context<Tab>) {
    match base_of(&tab.ui, start) {
        Ok(base) => {
            let folder = default_folder(&tab.ui, &base);
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Worktree(Ask::Add { base, folder }), cx);
        }
        Err(reason) => {
            tab.ui.say(Tone::Info, reason);
            cx.notify();
        }
    }
}

fn choose_folder(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(prompt::Kind::Worktree(Ask::Add { folder, .. })) = tab.ui.prompt.asked() else { return };
    let name = named(folder);
    let picker = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Put the worktree here".into()),
    });
    let handle = cx.weak_entity();
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = picker.await else { return };
            let Some(parent) = paths.into_iter().next() else { return };
            let _ = cx.update(|_, cx| {
                handle.update(cx, |tab, cx| {
                    if !matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Worktree(Ask::Add { .. }))) {
                        return;
                    }
                    if let Some(field) = tab.ui.prompt.field().cloned() {
                        let chosen = parent.join(&name).display().to_string();
                        field.update(cx, |field, cx| field.set_text(&chosen, cx));
                    }
                    prompt::refocus(&mut tab.ui);
                    cx.notify();
                })
            });
        })
        .detach();
}

fn open_in_another_tab(path: &Path, cx: &Context<Tab>) -> bool {
    let me = cx.entity_id();
    crate::tabs::strip(cx).is_some_and(|strip| {
        strip.tabs.iter().any(|shown| shown.id != me && shown.root.as_deref().is_some_and(|root| same_place(root, path)))
    })
}

fn open_elsewhere_refused(tab: &mut Tab, name: &str, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(
        Tone::Info,
        format!("Worktree {name} is open in another tab"),
        "Close that tab first, then remove the worktree.",
    ));
    cx.notify();
}

fn ask_remove(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(worktree) = listed(tab, &path, cx) else { return };
    if let Some(reason) = removal_refusal(&worktree) {
        tab.ui.say(Tone::Info, reason);
        cx.notify();
        return;
    }
    if open_in_another_tab(&worktree.path, cx) {
        return open_elsewhere_refused(tab, &worktree.name(), cx);
    }
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.menu = None;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || {
            let found = fresh(&repo, &worktree.path)?;
            if found.head != worktree.head || found.branch != worktree.branch {
                return Err(format!("Worktree {} changed. Try again.", worktree.name()));
            }
            let changes = uncommitted(&repo, &found.path)?;
            let (only, subjects) = match (&found.branch, &found.head) {
                (Some(branch), Some(tip)) => {
                    let own = Revisions::reachable(tip).only_on(branch);
                    let only = repo.count(&own)?;
                    let subjects = if only == 0 { Vec::new() } else { repo.subjects(&own, LISTED)? };
                    let subjects = subjects
                        .into_iter()
                        .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
                        .collect();
                    (only, subjects)
                }
                _ => (0, Vec::new()),
            };
            Ok::<_, String>(Removal {
                path: worktree.path.clone(),
                name: worktree.name(),
                head: found.head,
                branch: found.branch,
                changes,
                only,
                subjects,
            })
        },
        move |tab, prepared, cx| {
            if tab.epoch != epoch || tab.ui.dialog_open() || tab.ui.busy() {
                return;
            }
            match prepared {
                Ok(removal) => prompt::ask(&mut tab.ui, prompt::Kind::Worktree(Ask::Remove(removal)), cx),
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

fn open(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let listed = &tab.ui.worktree;
    let found = listed.find(&path).or_else(|| listed.list.iter().find(|worktree| same_place(&worktree.path, &path)));
    let Some(worktree) = found.cloned() else {
        tab.ui.say(Tone::Info, "That worktree is no longer listed");
        cx.notify();
        return;
    };
    if worktree.current {
        tab.ui.say(Tone::Info, "This worktree is open in this tab");
        cx.notify();
        return;
    }
    if worktree.prunable.is_some() || !worktree.path.is_dir() {
        tab.ui.notices.show(Notice::new(
            Tone::Error,
            format!("The folder of worktree {} is missing", worktree.name()),
            worktree.path.display().to_string(),
        ));
        cx.notify();
        return;
    }
    if !crate::tabs::open_elsewhere(worktree.path.clone(), |_, _| {}, cx) {
        tab.open(worktree.path, cx);
    }
}

pub(crate) fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    tab.ui.worktree.ticket += 1;
    let ticket = tab.ui.worktree.ticket;
    Tab::work(
        cx,
        move || list(&repo),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.worktree.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.worktree;
            state.list = result.unwrap_or_default();
            state.counts.clear();
            state.asked = false;
            cx.notify();
        },
    );
}

fn count(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.worktree;
    if !state.wanted() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    state.asked = true;
    let paths: Vec<PathBuf> = state.others().map(|worktree| worktree.path.clone()).collect();
    let (epoch, ticket) = (tab.epoch, state.ticket);
    Tab::work(
        cx,
        move || paths.into_iter().map(|path| {
            let counted = uncommitted(&repo, &path);
            (path, counted)
        }).collect::<Vec<_>>(),
        move |tab, counted, cx| {
            if tab.epoch != epoch || tab.ui.worktree.ticket != ticket {
                return;
            }
            tab.ui.worktree.counts.extend(counted);
            cx.notify();
        },
    );
}

pub(crate) fn side_row<T: Desk>(ui: &Ui, index: usize, row: usize, pointed: bool, cx: &mut Context<T>) -> AnyElement {
    let Some(worktree) = ui.worktree.list.get(index) else { return div().into_any_element() };
    let current = worktree.current;
    let (opened, menu_path) = (worktree.path.clone(), worktree.path.clone());
    let head = worktree.head.clone();
    let shown = matches!((&ui.selection, &worktree.head), (Some(view::Selection::Commit(id)), Some(oid)) if id == oid);
    let dirty = if current { ui.changed_count() > 0 } else { matches!(ui.worktree.count(&worktree.path), Some(Ok(count)) if *count > 0) };
    kit::row(("side-worktree", row), false)
        .h(px(TREE_ROW))
        .pl(px(12.))
        .pr(px(10.))
        .gap(px(6.))
        .child(
            div()
                .w(px(11.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .when(current, |d| d.child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))),
        )
        .child(
            kit::fit(worktree.name())
                .size(12.5)
                .color(if current || shown { TEXT } else { TEXT_2 })
                .weight(if current { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .grow(),
        )
        .when(dirty, |d| d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(rgb(MODIFIED))))
        .when(worktree.locked.is_some(), |d| d.child(kit::icon("lock").size(px(10.)).text_color(rgb(TEXT_3))))
        .when(worktree.prunable.is_some(), |d| d.child(kit::label("missing")))
        .child(div().max_w(px(110.)).flex_shrink_0().child(kit::fit(worktree.on()).size(TEXT_SMALL).color(TEXT_3)))
        .when(pointed, |d| d.tooltip(kit::tip(tip(&ui.worktree, worktree))))
        .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
            if event.click_count() > 1 && !current {
                app.dispatch(view::Action::Worktree(Action::Open(opened.clone())), w, cx);
            } else if let Some(oid) = head.clone() {
                app.dispatch(view::Action::Reveal(crate::reveal::Action::Commit(oid)), w, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = Target::Worktree(menu_path.clone());
                app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
            }),
        )
        .into_any_element()
}

pub(crate) fn chips<T: Desk>(ui: &Ui, oid: &str, cx: &Context<T>) -> Vec<AnyElement> {
    ui.worktree
        .at(oid)
        .enumerate()
        .map(|(n, worktree)| {
            let (opened, menu_path) = (worktree.path.clone(), worktree.path.clone());
            kit::chip_marked(kit::Chip::Plain, worktree.name(), &["folder"])
                .id(("worktree-chip", n))
                .cursor_pointer()
                .tooltip(kit::tip(tip(&ui.worktree, worktree)))
                .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
                    if event.click_count() > 1 {
                        cx.stop_propagation();
                        app.dispatch(view::Action::Worktree(Action::Open(opened.clone())), w, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                        cx.stop_propagation();
                        let target = Target::Worktree(menu_path.clone());
                        app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
                    }),
                )
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn commit(f: &Fixture, name: &str, text: &str, subject: &str) -> String {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", subject]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn snapshot(f: &Fixture) -> Vec<u8> {
        [
            f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            f.git(&["ls-files", "--stage"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            f.git(&["worktree", "list", "--porcelain"]),
        ]
        .concat()
    }

    #[test]
    fn worktrees_are_listed_with_their_branch_head_and_flags_and_the_open_one_is_marked() {
        let f = Fixture::new();
        let start = commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["branch", "ridge"]);
        let ridge = f.dir.join("ridge-tree");
        let summit = f.dir.join("summit-tree");
        let gone = f.dir.join("gone-tree");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        f.git(&["worktree", "add", "-q", "--detach", summit.to_str().unwrap(), "HEAD"]);
        f.git(&["worktree", "add", "-q", "-b", "gone", gone.to_str().unwrap()]);
        f.git(&["worktree", "lock", "--reason", "On the backup disk", summit.to_str().unwrap()]);
        fs::remove_dir_all(&gone).unwrap();
        let before = snapshot(&f);
        let listed = list(&f.repo).unwrap();
        assert_eq!(snapshot(&f), before);
        let main = f.dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(listed[0].name(), main);
        assert!(listed[0].current && listed.iter().skip(1).all(|worktree| !worktree.current));
        assert_eq!(listed[0].branch.as_deref(), Some(OsStr::new("main")));
        let named = |name: &str| listed.iter().find(|worktree| worktree.name() == name).cloned().unwrap();
        assert_eq!(listed.len(), 4);
        let (ridge_row, summit_row, gone_row) = (named("ridge-tree"), named("summit-tree"), named("gone-tree"));
        assert_eq!(ridge_row.branch.as_deref(), Some(OsStr::new("ridge")));
        assert_eq!(ridge_row.head.as_deref(), Some(start.as_str()));
        assert!(same_place(&ridge_row.path, &ridge));
        assert_eq!((summit_row.branch.clone(), summit_row.on()), (None, format!("detached at {}", notice::short(&start))));
        assert_eq!(summit_row.locked.as_deref(), Some("On the backup disk"));
        assert!(gone_row.prunable.is_some() && ridge_row.prunable.is_none() && ridge_row.locked.is_none());
        let from_ridge = list(&f.open(&ridge)).unwrap();
        assert!(from_ridge.iter().all(|worktree| worktree.current == (worktree.name() == "ridge-tree")));
    }

    #[test]
    fn a_bare_main_repository_and_an_unborn_worktree_are_read_from_porcelain() {
        let root = Path::new("/trail/ridge");
        let oid = "a".repeat(40);
        let zero = "0".repeat(40);
        let output = format!(
            "worktree /trail/bare.git\0bare\0\0worktree /trail/ridge\0HEAD {oid}\0branch refs/heads/ridge\0locked\0\0worktree /trail/new\0HEAD {zero}\0branch refs/heads/fresh\0prunable gitdir file points to non-existent location\0\0"
        );
        let listed = parse(output.as_bytes(), root);
        assert_eq!(listed.len(), 2);
        assert!(listed[0].current);
        assert_eq!(listed[0].locked.as_deref(), Some(""));
        assert_eq!(listed[1].head, None);
        assert_eq!(listed[1].on(), "fresh");
        assert_eq!(listed[1].prunable.as_deref(), Some("gitdir file points to non-existent location"));
        let state = State { list: listed, ..State::default() };
        assert_eq!(tip(&state, &state.list[0]), "/trail/ridge\nOn ridge\nOpen in this tab\nLocked");
        assert_eq!(tip(&state, &state.list[1]), "/trail/new\nOn fresh\nCounting uncommitted changes…\nIts folder is missing");
        assert_eq!(state.holding(OsStr::new("fresh")).map(Worktree::name).as_deref(), Some("new"));
        assert!(state.holding(OsStr::new("ridge")).is_none());
    }

    #[test]
    fn a_branch_taken_by_another_worktree_after_the_list_was_read_is_refused_by_git_and_explained() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["branch", "ridge"]);
        let state = State { list: list(&f.repo).unwrap(), ..State::default() };
        assert!(state.holding(OsStr::new("ridge")).is_none());
        let ridge = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        let before = snapshot(&f);
        let refused = f.repo.command().args(["switch", "--quiet", "--no-guess", "--", "ridge"]).output().unwrap();
        assert!(!refused.status.success());
        assert_eq!(snapshot(&f), before);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        let Some(crate::explain::Cause::InWorktree { branch, path: Some(path) }) = crate::explain::classify(&said) else {
            panic!("{said}")
        };
        assert_eq!(branch.as_deref(), Some("ridge"));
        let fresh = State { list: list(&f.repo).unwrap(), ..State::default() };
        let holder = fresh.holding(OsStr::new("ridge")).unwrap();
        assert!(same_place(&holder.path, Path::new(&path)));
        assert_eq!(holder.name(), "ridge-tree");
    }

    #[test]
    fn uncommitted_changes_are_counted_in_the_other_worktree_without_touching_it() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        let ridge = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", "-b", "ridge", ridge.to_str().unwrap()]);
        assert_eq!(uncommitted(&f.repo, &ridge).unwrap(), 0);
        fs::write(ridge.join("trail.txt"), "Trail, edited\n").unwrap();
        fs::write(ridge.join("lake.txt"), "Lake\n").unwrap();
        fs::remove_file(ridge.join("camp.txt")).unwrap();
        let linked = f.open(&ridge);
        let before = (snapshot(&f), checked(linked.command().args(["status", "--porcelain=v2"])).unwrap());
        assert_eq!(uncommitted(&f.repo, &ridge).unwrap(), 3);
        assert_eq!(uncommitted(&f.repo, &f.dir).unwrap(), 1);
        assert_eq!((snapshot(&f), checked(linked.command().args(["status", "--porcelain=v2"])).unwrap()), before);
        assert!(uncommitted(&f.repo, &f.dir.join("missing-tree")).is_err());
        let mut state = State { list: list(&f.repo).unwrap(), ..State::default() };
        assert!(state.wanted());
        let other = state.holding(OsStr::new("ridge")).unwrap().clone();
        state.counts.insert(other.path.clone(), Ok(3));
        state.asked = true;
        assert!(!state.wanted());
        assert_eq!(changes_line(&state, &other), "3 uncommitted changes");
        assert_eq!(state.at(other.head.as_deref().unwrap()).count(), 1);
    }

    struct Beside(PathBuf);

    impl Beside {
        fn new(f: &Fixture) -> Self {
            let dir = f.dir.with_extension("trees");
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Beside {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn trail(f: &Fixture) -> String {
        commit(f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["branch", "ridge"]);
        commit(f, "camp.txt", "Camp\n", "Pitch the camp")
    }

    fn tip_of(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().to_owned()
    }

    fn branch_base(f: &Fixture, name: &str) -> Base {
        Base::Branch { name: name.into(), tip: tip_of(f, name), held: None }
    }

    fn records(f: &Fixture) -> Vec<Worktree> {
        list(&f.repo).unwrap()
    }

    fn worktree_at(f: &Fixture, folder: &Path) -> Option<Worktree> {
        records(f).into_iter().find(|worktree| same_place(&worktree.path, folder))
    }

    fn removal(f: &Fixture, folder: &Path) -> Removal {
        let found = worktree_at(f, folder).unwrap();
        Removal {
            path: found.path.clone(),
            name: found.name(),
            head: found.head.clone(),
            branch: found.branch.clone(),
            changes: uncommitted(&f.repo, &found.path).unwrap(),
            only: 0,
            subjects: Vec::new(),
        }
    }

    #[test]
    fn a_worktree_is_added_on_an_existing_branch_a_new_branch_or_no_branch() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        let camp = trail(&f);
        let ridge_tip = tip_of(&f, "ridge");
        f.git(&["config", "branch.autoSetupMerge", "always"]);
        let main_before = (f.git(&["ls-files", "--stage"]), f.git(&["status", "--porcelain=v1"]), f.git(&["symbolic-ref", "HEAD"]));

        let ridge = beside.0.join("trail-ridge");
        let outcome = add(&f.repo, &Addition { folder: ridge.clone(), base: branch_base(&f, "ridge"), branch: None }).unwrap();
        assert_eq!(outcome.title, "Created worktree trail-ridge");
        assert_eq!(outcome.detail, format!("{}, on ridge.", ridge.display()));
        assert!(matches!(&outcome.actions[..], [(label, view::Action::Worktree(Action::Open(path)))] if label.as_ref() == "Open in a new tab" && path == &ridge));
        assert!(outcome.journal.refused.is_some());
        let linked = f.open(&ridge);
        assert_eq!(checked(linked.command().args(["symbolic-ref", "HEAD"])).unwrap(), b"refs/heads/ridge\n");
        assert_eq!(fs::read_to_string(ridge.join("trail.txt")).unwrap(), "Trail\n");
        assert!(!ridge.join("camp.txt").exists());
        assert_eq!(worktree_at(&f, &ridge).unwrap().head.as_deref(), Some(ridge_tip.as_str()));

        let lake = beside.0.join("nested/trail-lake");
        add(&f.repo, &Addition { folder: lake.clone(), base: branch_base(&f, "main"), branch: Some("lake".into()) }).unwrap();
        assert_eq!(tip_of(&f, "lake"), camp);
        assert_eq!(checked(f.open(&lake).command().args(["symbolic-ref", "HEAD"])).unwrap(), b"refs/heads/lake\n");
        assert!(f.repo.command().args(["config", "--get", "branch.lake.merge"]).output().unwrap().stdout.is_empty());

        let empty = beside.0.join("summit");
        fs::create_dir(&empty).unwrap();
        let detached = add(&f.repo, &Addition { folder: empty.clone(), base: Base::Commit(ridge_tip.clone()), branch: None }).unwrap();
        assert_eq!(detached.detail, format!("{}, at {}, on no branch.", empty.display(), notice::short(&ridge_tip)));
        let summit = worktree_at(&f, &empty).unwrap();
        assert_eq!((summit.branch, summit.head.as_deref()), (None, Some(ridge_tip.as_str())));

        assert_eq!(records(&f).len(), 4);
        assert_eq!((f.git(&["ls-files", "--stage"]), f.git(&["status", "--porcelain=v1"]), f.git(&["symbolic-ref", "HEAD"])), main_before);
        assert_eq!(tip_of(&f, "main"), camp);
    }

    #[test]
    fn adding_a_worktree_refuses_a_moved_branch_a_taken_name_a_used_folder_and_a_branch_in_use() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        let shown = branch_base(&f, "ridge");
        let taken = beside.0.join("taken");
        fs::create_dir(&taken).unwrap();
        fs::write(taken.join("note.txt"), "Keep\n").unwrap();
        let before = snapshot(&f);
        let refused = |addition: Addition| add(&f.repo, &addition).unwrap_err();
        let folder = beside.0.join("fresh");
        assert!(refused(Addition { folder: taken.clone(), base: shown.clone(), branch: None }).contains("not empty"));
        assert!(refused(Addition { folder: f.dir.join("inside"), base: shown.clone(), branch: None }).contains("inside worktree"));
        assert!(refused(Addition { folder: folder.clone(), base: shown.clone(), branch: Some("main".into()) }).contains("already exists"));
        assert!(refused(Addition { folder: folder.clone(), base: shown.clone(), branch: Some("bad name".into()) }).contains("spaces"));
        let here = Base::Branch { name: "main".into(), tip: tip_of(&f, "main"), held: Some("main is checked out in this tab".into()) };
        assert!(refused(Addition { folder: folder.clone(), base: here, branch: None }).contains("checked out"));
        assert!(refused(Addition { folder: folder.clone(), base: Base::Commit("0".repeat(40)), branch: None }).contains("no longer"));
        assert_eq!(snapshot(&f), before);
        assert!(!folder.exists());

        f.git(&["update-ref", "refs/heads/ridge", "main"]);
        let moved = snapshot(&f);
        assert!(refused(Addition { folder: folder.clone(), base: shown.clone(), branch: None }).contains("changed since"));
        assert_eq!(snapshot(&f), moved);
        assert!(!folder.exists());

        let elsewhere = beside.0.join("elsewhere");
        f.git(&["worktree", "add", "-q", elsewhere.to_str().unwrap(), "ridge"]);
        let held = snapshot(&f);
        let now = branch_base(&f, "ridge");
        let said = refused(Addition { folder: folder.clone(), base: now, branch: None });
        assert_eq!(said, "ridge is checked out in worktree elsewhere. Reopen the action.");
        assert_eq!(snapshot(&f), held);
        assert!(!folder.exists());
    }

    #[test]
    fn removing_a_worktree_deletes_its_folder_and_record_and_keeps_or_deletes_its_branch() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        let ridge = beside.0.join("ridge");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        let ridge_tip = tip_of(&f, "ridge");
        let main_before = (f.git(&["ls-files", "--stage"]), f.git(&["status", "--porcelain=v1"]));
        let outcome = remove(&f.repo, &removal(&f, &ridge), false).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Removed worktree ridge", "Its folder is deleted. Branch ridge is kept."));
        assert!(outcome.journal.refused.is_some());
        assert!(!ridge.exists());
        assert!(worktree_at(&f, &ridge).is_none() && records(&f).len() == 1);
        assert_eq!(tip_of(&f, "ridge"), ridge_tip);
        assert_eq!((f.git(&["ls-files", "--stage"]), f.git(&["status", "--porcelain=v1"])), main_before);

        let lake = beside.0.join("lake");
        f.git(&["worktree", "add", "-q", "-b", "lake", lake.to_str().unwrap(), "ridge"]);
        let lake_tree = f.open(&lake);
        fs::write(lake.join("lake.txt"), "Lake\n").unwrap();
        checked(lake_tree.command().args(["add", "lake.txt"])).unwrap();
        checked(lake_tree.command().args(["commit", "-qm", "Reach the lake"])).unwrap();
        fs::write(lake.join("trail.txt"), "Trail, flooded\n").unwrap();
        fs::write(lake.join("shore.txt"), "Shore\n").unwrap();
        let lake_tip = tip_of(&f, "lake");
        let dirty = removal(&f, &lake);
        assert_eq!(dirty.changes, 2);
        let outcome = remove(&f.repo, &dirty, true).unwrap();
        assert_eq!(outcome.title, "Removed worktree lake and branch lake");
        assert_eq!(outcome.journal.title.as_deref(), Some("Delete branch lake"));
        assert!(outcome.journal.refused.is_none());
        assert!(outcome.journal.changes.iter().any(|change| matches!(change, crate::journal::Change::DeleteRef { reference, oid, .. } if reference == "refs/heads/lake" && *oid == lake_tip)));
        assert!(!lake.exists());
        assert!(worktree_at(&f, &lake).is_none());
        assert!(f.git(&["for-each-ref", "refs/heads/lake"]).is_empty());
        assert_eq!(tip_of(&f, "ridge"), ridge_tip);
        assert_eq!((f.git(&["ls-files", "--stage"]), f.git(&["status", "--porcelain=v1"])), main_before);
    }

    #[test]
    fn removing_refuses_a_worktree_that_changed_or_cannot_be_removed() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        let ridge = beside.0.join("ridge");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        let clean = removal(&f, &ridge);
        fs::write(ridge.join("trail.txt"), "Trail, edited\n").unwrap();
        let before = snapshot(&f);
        let said = remove(&f.repo, &clean, false).unwrap_err();
        assert_eq!(said, "Worktree ridge has 1 uncommitted change now. Reopen the action to see them.");
        let dirty = removal(&f, &ridge);
        fs::write(ridge.join("more.txt"), "More\n").unwrap();
        assert!(remove(&f.repo, &dirty, false).unwrap_err().contains("changed since"));
        let shown = removal(&f, &ridge);
        checked(f.open(&ridge).command().args(["commit", "-qam", "Edit the trail"])).unwrap();
        assert!(remove(&f.repo, &shown, true).unwrap_err().contains("changed since"));
        let moved = snapshot(&f);
        assert!(ridge.join("more.txt").exists());
        f.git(&["worktree", "lock", "--", ridge.to_str().unwrap()]);
        let locked = removal(&f, &ridge);
        assert!(remove(&f.repo, &locked, false).unwrap_err().contains("is locked"));
        f.git(&["worktree", "unlock", "--", ridge.to_str().unwrap()]);
        assert_eq!(snapshot(&f), moved);
        assert_ne!(before, moved);
        let main = removal(&f, &f.dir);
        assert_eq!(remove(&f.repo, &main, false).unwrap_err(), "The main worktree cannot be removed.");
        let from_ridge = f.open(&ridge);
        let own = Removal { changes: uncommitted(&from_ridge, &ridge).unwrap(), ..removal(&f, &ridge) };
        assert!(remove(&from_ridge, &own, false).unwrap_err().contains("open in this tab"));
        assert!(ridge.join("more.txt").exists() && worktree_at(&f, &ridge).is_some());
        assert_eq!(snapshot(&f), moved);
    }

    #[test]
    fn lock_and_unlock_record_the_reason_and_refuse_a_lock_that_changed() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        let ridge = beside.0.join("ridge");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        let refs = f.git(&["for-each-ref"]);
        let outcome = lock(&f.repo, &ridge, "  On the backup disk ").unwrap();
        assert_eq!(outcome.title, "Locked worktree ridge");
        assert_eq!(outcome.detail, "On the backup disk. Git will not prune or remove it until it is unlocked.");
        assert_eq!(worktree_at(&f, &ridge).unwrap().locked.as_deref(), Some("On the backup disk"));
        assert!(lock(&f.repo, &ridge, "").unwrap_err().contains("already locked"));
        assert!(lock(&f.repo, &f.dir, "").unwrap_err().contains("main worktree"));
        assert_eq!(unlock(&f.repo, &ridge).unwrap().title, "Unlocked worktree ridge");
        assert!(worktree_at(&f, &ridge).unwrap().locked.is_none());
        assert!(unlock(&f.repo, &ridge).unwrap_err().contains("not locked"));
        lock(&f.repo, &ridge, "").unwrap();
        assert_eq!(worktree_at(&f, &ridge).unwrap().locked.as_deref(), Some(""));
        assert!(lock(&f.repo, &beside.0.join("gone"), "").unwrap_err().contains("no longer listed"));
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert!(ridge.join("trail.txt").exists());
    }

    #[test]
    fn missing_worktrees_are_pruned_and_a_changed_list_is_refused() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        let (ridge, lake, kept) = (beside.0.join("ridge"), beside.0.join("lake"), beside.0.join("kept"));
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        f.git(&["worktree", "add", "-q", "-b", "lake", lake.to_str().unwrap()]);
        f.git(&["worktree", "add", "-q", "-b", "kept", kept.to_str().unwrap()]);
        fs::remove_dir_all(&ridge).unwrap();
        let missing: Vec<PathBuf> = records(&f).into_iter().filter(|worktree| worktree.prunable.is_some()).map(|worktree| worktree.path).collect();
        assert_eq!(missing.len(), 1);
        fs::remove_dir_all(&lake).unwrap();
        let before = snapshot(&f);
        assert!(prune(&f.repo, &missing).unwrap_err().contains("changed since"));
        assert!(prune(&f.repo, &[]).is_err());
        assert_eq!(snapshot(&f), before);
        let missing: Vec<PathBuf> = records(&f).into_iter().filter(|worktree| worktree.prunable.is_some()).map(|worktree| worktree.path).collect();
        let refs = f.git(&["for-each-ref"]);
        let outcome = prune(&f.repo, &missing).unwrap();
        assert_eq!(outcome.title, "Removed 2 missing worktrees");
        assert_eq!(outcome.detail, "Git no longer lists lake, ridge. Branches are kept.");
        let left: Vec<String> = records(&f).iter().map(Worktree::name).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"kept".to_owned()));
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert!(kept.join("trail.txt").exists());
    }

    #[test]
    fn a_typed_folder_is_expanded_and_must_be_new_or_empty_and_outside_every_worktree() {
        let f = Fixture::new();
        let beside = Beside::new(&f);
        trail(&f);
        assert_eq!(typed_folder("  "), Err(String::new()));
        assert!(typed_folder("relative/tree").unwrap_err().contains("full path"));
        assert!(typed_folder("/trail/../tree").unwrap_err().contains(".."));
        assert_eq!(typed_folder("/trail/ridge ").unwrap(), PathBuf::from("/trail/ridge"));
        if let Some(home) = home() {
            assert_eq!(typed_folder("~/trees/ridge").unwrap(), home.join("trees/ridge"));
        }
        let listed = records(&f);
        assert!(folder_problem(&beside.0.join("new"), &listed).is_none());
        assert!(folder_problem(&f.dir.join("sub/tree"), &listed).unwrap().contains("inside worktree"));
        assert!(folder_problem(&f.dir, &listed).unwrap().contains("That is the folder"));
        let real_inside = fs::canonicalize(&f.dir).unwrap().join("tree");
        assert!(folder_problem(&real_inside, &listed).unwrap().contains("inside worktree"));
        fs::write(beside.0.join("file"), "x").unwrap();
        assert!(folder_problem(&beside.0.join("file"), &listed).unwrap().contains("not a folder"));
        assert!(folder_problem(&beside.0, &listed).unwrap().contains("not empty"));
    }

    #[test]
    fn the_prompts_say_what_is_lost_and_a_branch_in_use_must_get_a_new_one() {
        let base = Base::Branch { name: "main".into(), tip: "a".repeat(40), held: Some("main is checked out in this tab".into()) };
        let spec = add_spec(&base, Path::new("/trail/Trail-main"));
        assert_eq!(spec.title, "New worktree from main");
        assert!(spec.lines.contains(&"main is checked out in this tab, so the new worktree needs a branch of its own.".to_owned()));
        assert_eq!(spec.tick.as_ref().map(|(label, ticked)| (label.to_string(), *ticked)), Some(("Create a new branch from main".to_owned(), true)));
        assert!(spec.fixed_tick && !spec.danger);
        assert_eq!(spec.field.as_ref().unwrap().initial, "/trail/Trail-main");
        let free = add_spec(&Base::Branch { name: "ridge".into(), tip: "a".repeat(40), held: None }, Path::new("/t"));
        assert!(!free.fixed_tick && free.tick.as_ref().is_some_and(|(_, ticked)| !ticked));
        let at = add_spec(&Base::Commit("b".repeat(40)), Path::new("/t"));
        assert_eq!(at.title, format!("New worktree at {}", notice::short(&"b".repeat(40))));
        assert!(at.tick.as_ref().is_some_and(|(_, ticked)| *ticked) && !at.fixed_tick);

        let mut removal = Removal {
            path: "/trail/ridge".into(),
            name: "ridge".into(),
            head: Some("a".repeat(40)),
            branch: Some("ridge".into()),
            changes: 0,
            only: 0,
            subjects: Vec::new(),
        };
        let clean = remove_spec(&removal, false);
        assert_eq!(clean.title, "Remove worktree ridge?");
        assert!(!clean.danger);
        assert_eq!(clean.lines[0], "Its folder /trail/ridge is deleted, with any ignored files in it.");
        assert_eq!(clean.tick.as_ref().map(|(label, ticked)| (label.to_string(), *ticked)), Some(("Also delete branch ridge".to_owned(), false)));
        removal.changes = 3;
        removal.only = 7;
        removal.subjects = vec![("abc1234".into(), "Climb the ridge".into())];
        let dirty = remove_spec(&removal, false);
        assert!(dirty.danger);
        assert_eq!(dirty.confirm.as_ref(), "Remove worktree");
        assert_eq!(
            dirty.lines,
            [
                "Its folder /trail/ridge is deleted with its 3 uncommitted changes.",
                "They have not been committed, so they cannot be brought back.",
                "Branch ridge is kept unless you delete it too. 7 commits are only on it:",
                "abc1234  Climb the ridge",
                "and 6 more commits",
            ]
        );
        removal.changes = 0;
        assert!(!remove_spec(&removal, false).danger);
        let deleting = remove_spec(&removal, true);
        assert!(deleting.danger);
        assert_eq!(deleting.confirm.as_ref(), "Remove worktree");
        removal.only = 0;
        removal.subjects.clear();
        assert!(!remove_spec(&removal, true).danger);
        removal.branch = None;
        assert!(remove_spec(&removal, false).tick.is_none());
    }
}
