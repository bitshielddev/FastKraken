use crate::repository::Run;
use crate::{
    Tab,
    autostash::{self, Retry},
    commands::{self, Availability, Command, Group, on_repo},
    guard::display,
    notice::{self, Failure, Outcome},
    repository::{Repository, checked, raw_path, validate_oid},
    view::{self, Ui},
};
use gpui::{App, Context, Window};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(crate) const HIDDEN: &str = "--exclude=refs/fastkraken/*";
pub(crate) const UNDECORATED: &str = "--decorate-refs-exclude=refs/fastkraken/*";
const UNDO: &str = "refs/fastkraken/undo/";
pub(crate) const MAIN_WORKTREE: &str = "main";
const LINKED: &str = "worktrees/";
pub(crate) const LIMIT: usize = 50;
const NOTHING_TO_UNDO: &str = "Nothing to undo";
const NOTHING_TO_REDO: &str = "Nothing to redo";
pub(crate) const PUSHED: &str = "A push cannot be undone";
const STASHED: &str = "Saving a stash cannot be undone. Pop the stash to bring the changes back.";
pub(crate) const POPPED: &str = "Popping a stash cannot be undone";
pub(crate) const PARTLY: &str = "Discarding part of a file cannot be undone";

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastkraken_desk, no_json)]
pub(crate) struct Unavailable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    Soft,
    Mixed,
    Keep,
}

impl Mode {
    fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Keep => "--keep",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Place {
    Branch(String),
    Detached(String),
}

impl Place {
    fn shown(&self) -> String {
        match self {
            Self::Branch(full) => display(OsStr::new(full)),
            Self::Detached(oid) => notice::short(oid).to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Upstream {
    pub(crate) remote: String,
    pub(crate) merge: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Discarded {
    All,
    Tracked,
    Files(Vec<Vec<u8>>),
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Change {
    MoveBranch { reference: String, before: String, after: String, mode: Mode },
    Checkout { from: Place, to: Place },
    DeleteRef { reference: String, oid: String, upstream: Option<Upstream> },
    CreateRef { reference: String, oid: String, upstream: Option<Upstream> },
    RenameBranch { old: String, new: String, oid: String },
    Discard { saved: String, discarded: Discarded },
    DropStash { oid: String, message: String },
    NotUndoable(String),
}

impl Change {
    pub(crate) fn discarded_files(saved: String, paths: &[PathBuf]) -> Self {
        let paths = paths.iter().map(|path| path.as_os_str().as_bytes().to_vec()).collect();
        Self::Discard { saved, discarded: Discarded::Files(paths) }
    }

    fn objects(&self) -> Vec<&str> {
        match self {
            Self::MoveBranch { before, after, .. } => vec![before, after],
            Self::Checkout { from, to } => [from, to]
                .into_iter()
                .filter_map(|place| match place {
                    Place::Detached(oid) => Some(oid.as_str()),
                    Place::Branch(_) => None,
                })
                .collect(),
            Self::DeleteRef { oid, .. } | Self::CreateRef { oid, .. } | Self::RenameBranch { oid, .. } => vec![oid],
            Self::Discard { saved, .. } => vec![saved],
            Self::DropStash { oid, .. } => vec![oid],
            Self::NotUndoable(_) => Vec::new(),
        }
    }

    fn title(&self) -> String {
        let named = |reference: &str| {
            let kind = if reference.starts_with("refs/tags/") { "tag" } else { "branch" };
            format!("{kind} {}", display(OsStr::new(reference)))
        };
        match self {
            Self::MoveBranch { reference, after, .. } => {
                format!("Move {} to {}", shown_ref(reference), notice::short(after))
            }
            Self::Checkout { from, to } => format!("Switch from {} to {}", from.shown(), to.shown()),
            Self::DeleteRef { reference, .. } => format!("Delete {}", named(reference)),
            Self::CreateRef { reference, .. } => format!("Create {}", named(reference)),
            Self::RenameBranch { old, new, .. } => format!("Rename {} to {}", shown_ref(old), shown_ref(new)),
            Self::Discard { .. } => "Discard changes".to_owned(),
            Self::DropStash { message, .. } => format!("Drop stash {}", crate::stash::named(message).0),
            Self::NotUndoable(reason) => match reason.as_str() {
                PUSHED => "Push",
                STASHED => "Save stash",
                POPPED => "Pop stash",
                PARTLY => "Discard part of a file",
                _ => "Last action",
            }
            .to_owned(),
        }
    }
}

fn shown_ref(reference: &str) -> String {
    if reference == "HEAD" { "HEAD".to_owned() } else { display(OsStr::new(reference)) }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct Entry {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) changes: Vec<Change>,
    pub(crate) at: i64,
    pub(crate) head: Option<Place>,
    pub(crate) was: Option<Place>,
}

impl Entry {
    fn refusal(&self) -> Option<&str> {
        self.changes.iter().find_map(|change| match change {
            Change::NotUndoable(reason) => Some(reason.as_str()),
            _ => None,
        })
    }

    fn objects(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.changes
            .iter()
            .flat_map(Change::objects)
            .filter(|oid| seen.insert(oid.to_string()))
            .map(str::to_owned)
            .collect()
    }

    fn touches_files(&self) -> bool {
        self.changes.iter().any(|change| match change {
            Change::Checkout { .. } | Change::Discard { .. } => true,
            Change::MoveBranch { reference, .. } => {
                reference == "HEAD" || self.head.as_ref() == Some(&Place::Branch(reference.clone()))
            }
            _ => false,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Stored {
    pub(crate) undo: Vec<Entry>,
    pub(crate) redo: Vec<Entry>,
    pub(crate) next: u64,
}

impl Stored {
    pub(crate) fn is_empty(&self) -> bool {
        self.undo.is_empty() && self.redo.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    pub(crate) title: Option<String>,
    pub(crate) mode: Option<Mode>,
    pub(crate) changes: Vec<Change>,
    pub(crate) refused: Option<String>,
    pub(crate) offered: bool,
}

impl Outcome {
    pub(crate) fn undone_as(mut self, title: impl Into<String>) -> Self {
        self.journal.title = Some(title.into());
        self
    }

    pub(crate) fn moving(mut self, mode: Mode) -> Self {
        self.journal.mode = Some(mode);
        self
    }

    pub(crate) fn changed(mut self, change: Change) -> Self {
        self.journal.changes.push(change);
        self
    }

    pub(crate) fn offering_undo(mut self) -> Self {
        self.journal.offered = true;
        self
    }

    pub(crate) fn not_undoable(mut self, reason: impl Into<String>) -> Self {
        self.journal.refused = Some(reason.into());
        self
    }
}

pub(crate) fn titled(result: Result<Outcome, Failure>, title: String) -> Result<Outcome, Failure> {
    match result {
        Ok(outcome) => Ok(outcome.undone_as(title)),
        Err(mut failure) => {
            failure.journal.title = Some(title);
            Err(failure)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Refs {
    head: Option<Place>,
    refs: BTreeMap<String, (String, Option<Upstream>)>,
    stashes: Vec<(String, String)>,
    busy: bool,
}

impl Refs {
    pub(crate) fn read(repo: &Repository) -> Result<Self, String> {
        let listed = checked(repo.read_command().args([
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/heads",
            "refs/tags",
        ]))?;
        let upstreams = upstreams(repo)?;
        let mut refs = BTreeMap::new();
        for line in listed.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
            let Some(at) = line.iter().position(|&b| b == 0) else { continue };
            let (Ok(name), Ok(oid)) = (std::str::from_utf8(&line[..at]), std::str::from_utf8(&line[at + 1..])) else {
                continue;
            };
            if validate_oid(oid).is_err() {
                continue;
            }
            let upstream = branch_name(name).and_then(|branch| upstreams.get(branch)).cloned();
            refs.insert(name.to_owned(), (oid.to_owned(), upstream));
        }
        let stashes = repo.stashes()?.into_iter().map(|entry| (entry.oid, entry.message)).collect();
        let busy = crate::operation::detect(repo.git_dir(), || Ok(false))?.is_some();
        Ok(Self { head: head_place(repo)?, refs, stashes, busy })
    }
}

fn upstreams(repo: &Repository) -> Result<BTreeMap<String, Upstream>, String> {
    let output = repo
        .read_command()
        .args(["config", "--local", "--null", "--get-regexp", "^branch\\..*\\.(remote|merge)$"])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    let mut remotes = BTreeMap::new();
    let mut merges = BTreeMap::new();
    for record in output.stdout.split(|&b| b == 0).filter(|record| !record.is_empty()) {
        let text = String::from_utf8_lossy(record);
        let Some((key, value)) = text.split_once('\n') else { continue };
        let Some(key) = key.strip_prefix("branch.") else { continue };
        if let Some(branch) = key.strip_suffix(".remote") {
            remotes.insert(branch.to_owned(), value.to_owned());
        } else if let Some(branch) = key.strip_suffix(".merge") {
            merges.insert(branch.to_owned(), value.to_owned());
        }
    }
    Ok(remotes
        .into_iter()
        .filter_map(|(branch, remote)| {
            let merge = merges.remove(&branch)?;
            Some((branch, Upstream { remote, merge }))
        })
        .collect())
}

fn head_place(repo: &Repository) -> Result<Option<Place>, String> {
    let symbolic = repo
        .read_command()
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if symbolic.status.success() {
        let name = String::from_utf8_lossy(&symbolic.stdout).trim_end().to_owned();
        return Ok(Some(Place::Branch(name)));
    }
    let tip = repo
        .read_command()
        .args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    let oid = String::from_utf8_lossy(&tip.stdout).trim_end().to_owned();
    Ok((tip.status.success() && validate_oid(&oid).is_ok()).then_some(Place::Detached(oid)))
}

pub(crate) fn derive(before: &Refs, after: &Refs, report: &Report) -> Vec<Change> {
    let mut created = Vec::new();
    let mut moved = Vec::new();
    let mut deleted = Vec::new();
    let names: BTreeSet<&String> = before.refs.keys().chain(after.refs.keys()).collect();
    for name in names {
        match (before.refs.get(name), after.refs.get(name)) {
            (None, Some((oid, upstream))) => {
                created.push(Change::CreateRef { reference: name.clone(), oid: oid.clone(), upstream: upstream.clone() })
            }
            (Some((oid, upstream)), None) => {
                deleted.push(Change::DeleteRef { reference: name.clone(), oid: oid.clone(), upstream: upstream.clone() })
            }
            (Some((was, _)), Some((now, _))) if was != now => {
                let current = after.head.as_ref() == Some(&Place::Branch(name.clone()));
                let mode = if current { report.mode.unwrap_or(Mode::Keep) } else { Mode::Keep };
                moved.push(Change::MoveBranch { reference: name.clone(), before: was.clone(), after: now.clone(), mode });
            }
            _ => {}
        }
    }
    let mut checkout = None;
    match (&before.head, &after.head) {
        (Some(Place::Detached(was)), Some(Place::Detached(now))) if was != now && report.mode.is_some() => {
            moved.push(Change::MoveBranch {
                reference: "HEAD".to_owned(),
                before: was.clone(),
                after: now.clone(),
                mode: report.mode.unwrap_or(Mode::Keep),
            });
        }
        (Some(from), Some(to)) if from != to => checkout = Some(Change::Checkout { from: from.clone(), to: to.clone() }),
        _ => {}
    }
    if let ([Change::DeleteRef { reference: old, oid: was, .. }], [Change::CreateRef { reference: new, oid: now, .. }]) =
        (deleted.as_slice(), created.as_slice())
    {
        let renamed_head = match &checkout {
            Some(Change::Checkout { from: Place::Branch(from), to: Place::Branch(to) }) => from == old && to == new,
            None => true,
            _ => false,
        };
        if was == now && old.starts_with("refs/heads/") && new.starts_with("refs/heads/") && renamed_head {
            let rename = Change::RenameBranch { old: old.clone(), new: new.clone(), oid: was.clone() };
            let mut changes = vec![rename];
            changes.extend(moved);
            return changes;
        }
    }
    let mut changes = created;
    changes.extend(checkout);
    changes.extend(moved);
    changes.extend(deleted);
    let dropped = before
        .stashes
        .iter()
        .filter(|(oid, _)| !after.stashes.iter().any(|(now, _)| now == oid))
        .map(|(oid, message)| Change::DropStash { oid: oid.clone(), message: message.clone() });
    changes.extend(dropped);
    let saved = after.stashes.iter().any(|(oid, _)| !before.stashes.iter().any(|(was, _)| was == oid));
    if changes.is_empty() && saved && report.changes.is_empty() {
        changes.push(Change::NotUndoable(STASHED.to_owned()));
    }
    changes
}

fn generic_title(changes: &[Change]) -> String {
    match changes {
        [] => String::new(),
        [one] => one.title(),
        [first, second] => format!("{} and {}", first.title(), autostash::lowered(&second.title())),
        [first, rest @ ..] => format!("{} and {} more changes", first.title(), rest.len()),
    }
}

pub(crate) struct Watch {
    id: u64,
    refs: bool,
    pending: Option<Pending>,
    released: Vec<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct Pending {
    refs: Refs,
    title: Option<String>,
    mode: Option<Mode>,
}

pub(crate) enum Next {
    Keep,
    Set(Pending),
    Clear,
}

pub(crate) struct Watched {
    next: Next,
    entry: Option<Entry>,
    released: Vec<u64>,
}

impl Watched {
    pub(crate) fn recorded(&self) -> bool {
        self.entry.is_some()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs() as i64)
}

pub(crate) fn watched(
    repo: &Repository,
    watch: Option<Watch>,
    run: impl FnOnce() -> Result<Outcome, Failure>,
) -> (Result<Outcome, Failure>, Option<Watched>) {
    let Some(watch) = watch else {
        return (run(), None);
    };
    let before = if watch.refs { Refs::read(repo).ok() } else { None };
    let result = run();
    let _ = release(repo, &watch.released);
    let report = match &result {
        Ok(outcome) => outcome.journal.clone(),
        Err(failure) => failure.journal.clone(),
    };
    let mut watched = Watched { next: Next::Keep, entry: None, released: watch.released.clone() };
    let mut changes = report.changes.clone();
    let mut title = report.title.clone();
    let (mut was, mut head) = (None, None);
    if let Some(before) = before {
        let Ok(after) = Refs::read(repo) else { return (result, Some(watched)) };
        if after.busy {
            if watch.pending.is_none() && !before.busy {
                watched.next = Next::Set(Pending { refs: before, title: report.title, mode: report.mode });
            }
            return (result, Some(watched));
        }
        if watch.pending.is_some() {
            watched.next = Next::Clear;
        }
        let (base, report, finished) = match watch.pending.filter(|_| before.busy) {
            Some(pending) => {
                let report = Report { mode: report.mode.or(pending.mode), ..report.clone() };
                title = title.or(pending.title);
                (pending.refs, report, true)
            }
            None => (before, report.clone(), false),
        };
        if !base.busy {
            for change in derive(&base, &after, &report) {
                let moved = matches!(change, Change::MoveBranch { .. } | Change::Checkout { .. });
                if (moved || !finished) && !changes.contains(&change) {
                    changes.push(change);
                }
            }
        }
        was = base.head.clone();
        head = after.head.clone();
    } else if !changes.is_empty() || report.refused.is_some() {
        head = head_place(repo).ok().flatten();
        was = head.clone();
    }
    if let Some(reason) = report.refused {
        changes = vec![Change::NotUndoable(reason)];
    }
    if changes.is_empty() {
        return (result, Some(watched));
    }
    let title = title.unwrap_or_else(|| generic_title(&changes));
    let entry = Entry { id: watch.id, title, changes, at: now(), head, was };
    if keep(repo, entry.id, &entry.objects()).is_ok() {
        watched.entry = Some(entry);
    }
    (result, Some(watched))
}

pub(crate) fn space(repo: &Repository) -> String {
    space_of(repo.git_dir(), repo.common_dir())
}

fn space_of(git_dir: &Path, common: &Path) -> String {
    if git_dir == common {
        return MAIN_WORKTREE.to_owned();
    }
    let linked = git_dir.parent() == Some(common.join("worktrees").as_path());
    match git_dir.file_name().and_then(OsStr::to_str) {
        Some(name) if linked && plain_component(name) => format!("{LINKED}{name}"),
        _ => format!("other/{:016x}", crate::instance::fnv(git_dir.as_os_str().as_bytes())),
    }
}

fn plain_component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || !b.is_ascii() || matches!(b, b'.' | b'_' | b'-'))
}

fn root(repo: &Repository) -> String {
    format!("{UNDO}{}/", space(repo))
}

fn private(root: &str, id: u64, index: usize) -> String {
    format!("{root}{id}/{index}")
}

pub(crate) fn keep(repo: &Repository, id: u64, oids: &[String]) -> Result<(), String> {
    let root = root(repo);
    let mut input = Vec::new();
    for (index, oid) in oids.iter().enumerate() {
        validate_oid(oid)?;
        input.extend_from_slice(format!("update {} {oid}\n", private(&root, id, index)).as_bytes());
    }
    if input.is_empty() {
        return Ok(());
    }
    crate::operation::with_input(repo.command().args(["update-ref", "--stdin"]), input).map(|_| ())
}

fn every_private(repo: &Repository) -> Result<Vec<(String, String)>, String> {
    let listed = checked(repo.read_command().args(["for-each-ref", "--format=%(refname)%00%(objectname)", UNDO]))?;
    Ok(String::from_utf8_lossy(&listed)
        .lines()
        .filter_map(|line| {
            let (name, oid) = line.split_once('\0')?;
            Some((name.to_owned(), oid.to_owned()))
        })
        .collect())
}

pub(crate) fn held(repo: &Repository) -> Result<Vec<(u64, String)>, String> {
    let root = root(repo);
    Ok(every_private(repo)?
        .into_iter()
        .filter_map(|(name, _)| {
            let id = name.strip_prefix(root.as_str())?.split('/').next()?.parse().ok()?;
            Some((id, name))
        })
        .collect())
}

fn delete_held(repo: &Repository, doomed: impl Fn(u64) -> bool) -> Result<(), String> {
    let mut input = Vec::new();
    for (id, name) in held(repo)? {
        if doomed(id) {
            input.extend_from_slice(format!("delete {name}\n").as_bytes());
        }
    }
    if input.is_empty() {
        return Ok(());
    }
    crate::operation::with_input(repo.command().args(["update-ref", "--stdin"]), input).map(|_| ())
}

pub(crate) fn release(repo: &Repository, ids: &[u64]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    delete_held(repo, |id| ids.contains(&id))
}

pub(crate) fn sweep(repo: &Repository, named: &BTreeSet<u64>) -> Result<Vec<String>, String> {
    let space = space(repo);
    let root = format!("{UNDO}{space}/");
    let worktrees = repo.common_dir().join("worktrees");
    let mut input = Vec::new();
    let mut gone = Vec::new();
    for (name, oid) in every_private(repo)? {
        let Some(rest) = name.strip_prefix(UNDO) else { continue };
        let first = rest.split('/').next().unwrap_or_default();
        if let Some(own) = name.strip_prefix(root.as_str()) {
            let id = own.split('/').next().and_then(|id| id.parse::<u64>().ok());
            if id.is_none_or(|id| !named.contains(&id)) {
                input.extend_from_slice(format!("delete {name}\n").as_bytes());
            }
        } else if let Ok(id) = first.parse::<u64>() {
            if space != MAIN_WORKTREE {
                continue;
            }
            if named.contains(&id) && validate_oid(&oid).is_ok() {
                input.extend_from_slice(format!("update {root}{rest} {oid}\n").as_bytes());
            }
            input.extend_from_slice(format!("delete {name}\n").as_bytes());
        } else if let Some(linked) = rest.strip_prefix(LINKED) {
            let worktree = linked.split('/').next().unwrap_or_default();
            if !worktree.is_empty() && !worktrees.join(worktree).is_dir() {
                input.extend_from_slice(format!("delete {name}\n").as_bytes());
                let left = format!("{LINKED}{worktree}");
                if !gone.contains(&left) {
                    gone.push(left);
                }
            }
        }
    }
    if !input.is_empty() {
        crate::operation::with_input(repo.command().args(["update-ref", "--stdin"]), input)?;
    }
    Ok(gone)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Way {
    Undo,
    Redo,
}

impl Way {
    fn verb(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Undo(Option<u64>),
    Redo(Option<u64>),
}

#[derive(Default)]
pub(crate) struct State {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    next: u64,
    pending: Option<Pending>,
    released: Vec<u64>,
}

impl State {
    pub(crate) fn from_stored(stored: Stored) -> Self {
        let next = stored.undo.iter().chain(&stored.redo).map(|entry| entry.id + 1).max().unwrap_or(0).max(stored.next);
        Self { undo: stored.undo, redo: stored.redo, next, pending: None, released: Vec::new() }
    }

    pub(crate) fn stored(&self) -> Option<Stored> {
        let stored = Stored { undo: self.undo.clone(), redo: self.redo.clone(), next: self.next };
        (!stored.is_empty()).then_some(stored)
    }

    pub(crate) fn named(&self) -> BTreeSet<u64> {
        self.undo.iter().chain(&self.redo).map(|entry| entry.id).collect()
    }

    pub(crate) fn last(&self, way: Way) -> Option<&Entry> {
        match way {
            Way::Undo => self.undo.last(),
            Way::Redo => self.redo.last(),
        }
    }

    #[cfg(test)]
    pub(crate) fn undo_list(&self) -> &[Entry] {
        &self.undo
    }

    #[cfg(test)]
    pub(crate) fn redo_list(&self) -> &[Entry] {
        &self.redo
    }

    fn watch(&mut self, refs: bool) -> Watch {
        let id = self.next;
        self.next += 1;
        Watch { id, refs, pending: self.pending.clone(), released: std::mem::take(&mut self.released) }
    }

    fn settle(&mut self, watched: Watched) -> Option<&Entry> {
        self.released.retain(|id| !watched.released.contains(id));
        match watched.next {
            Next::Keep => {}
            Next::Set(pending) => self.pending = Some(pending),
            Next::Clear => self.pending = None,
        }
        let entry = watched.entry?;
        self.push(entry);
        self.undo.last()
    }

    pub(crate) fn push(&mut self, entry: Entry) {
        self.next = self.next.max(entry.id + 1);
        self.released.extend(self.redo.drain(..).map(|entry| entry.id));
        self.undo.push(entry);
        let excess = self.undo.len().saturating_sub(LIMIT);
        self.released.extend(self.undo.drain(..excess).map(|entry| entry.id));
    }

    fn replayed(&mut self, way: Way, entry: Entry) {
        let (from, to) = match way {
            Way::Undo => (&mut self.undo, &mut self.redo),
            Way::Redo => (&mut self.redo, &mut self.undo),
        };
        from.retain(|kept| kept.id != entry.id);
        to.push(entry);
    }

    #[cfg(test)]
    pub(crate) fn released(&self) -> &[u64] {
        &self.released
    }
}

pub(crate) fn availability(ui: &Ui, way: Way) -> Availability {
    if ui.repo.is_empty() {
        return Availability::Hidden;
    }
    let Some(entry) = ui.journal.last(way) else {
        return Availability::Disabled(match way {
            Way::Undo => NOTHING_TO_UNDO,
            Way::Redo => NOTHING_TO_REDO,
        }
        .to_owned());
    };
    if let Some(reason) = entry.refusal() {
        return Availability::Disabled(reason.to_owned());
    }
    if let Some(reason) = commands::busy(ui) {
        return Availability::Disabled(reason);
    }
    if entry.touches_files() {
        if let Some(reason) = commands::operating(ui) {
            return Availability::Disabled(reason);
        }
    }
    Availability::Enabled
}

pub(crate) fn title(ui: &Ui, way: Way) -> String {
    match ui.journal.last(way) {
        Some(entry) => format!("{} {}", way.verb(), entry.title),
        None => way.verb().to_owned(),
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "edit.undo",
        Group::Reverse,
        |ui, _| title(ui, Way::Undo),
        |ui, target, _| if on_repo(ui, target) { availability(ui, Way::Undo) } else { Availability::Hidden },
        |_, _| Some(view::Action::Journal(Action::Undo(None))),
    )
    .keys("cmd-z")
    .words(&["undo", "take back", "last action", "reflog"]),
    Command::new(
        "edit.redo",
        Group::Reverse,
        |ui, _| title(ui, Way::Redo),
        |ui, target, _| if on_repo(ui, target) { availability(ui, Way::Redo) } else { Availability::Hidden },
        |_, _| Some(view::Action::Journal(Action::Redo(None))),
    )
    .keys("cmd-shift-z")
    .words(&["redo", "do again"]),
];

pub(crate) fn menu_rows(ui: &Ui) -> [(String, bool); 2] {
    [Way::Undo, Way::Redo].map(|way| match availability(ui, way) {
        Availability::Enabled => (title(ui, way), true),
        Availability::Disabled(reason) if reason != NOTHING_TO_UNDO && reason != NOTHING_TO_REDO => (reason, false),
        _ => (way.verb().to_owned(), false),
    })
}

pub(crate) fn bar_titles(ui: &Ui, field: bool) -> Vec<String> {
    let mut titles = vec![format!("field:{field}")];
    titles.extend(menu_rows(ui).into_iter().map(|(title, enabled)| format!("{title}\0{enabled}")));
    titles
}

pub(crate) fn bar_items(ui: &Ui, field: bool) -> Vec<gpui::MenuItem> {
    use gpui::{MenuItem, OsAction};
    if field {
        return vec![
            MenuItem::os_action("Undo", crate::view::commit_input::UndoText, OsAction::Undo),
            MenuItem::os_action("Redo", crate::view::commit_input::RedoText, OsAction::Redo),
        ];
    }
    menu_rows(ui)
        .into_iter()
        .zip(["edit.undo", "edit.redo"])
        .map(|((title, enabled), id)| {
            if enabled { MenuItem::action(title, commands::Run { id }) } else { MenuItem::action(title, Unavailable) }
        })
        .collect()
}

pub(crate) fn begin(ui: &mut Ui, refs: bool) -> Watch {
    ui.journal.watch(refs)
}

pub(crate) fn record(
    tab: &mut Tab,
    watched: Option<Watched>,
    result: &mut Result<Outcome, Failure>,
    cx: &mut Context<Tab>,
) {
    let Some(watched) = watched else { return };
    let changed = watched.entry.is_some() || !watched.released.is_empty();
    let offer = tab.ui.journal.settle(watched).filter(|entry| entry.refusal().is_none()).map(|entry| entry.id);
    if let Some(id) = offer {
        let undo = ("Undo".into(), view::Action::Journal(Action::Undo(Some(id))));
        match result {
            Ok(outcome) if outcome.journal.offered => outcome.actions.insert(0, undo),
            Ok(outcome) => outcome.actions.push(undo),
            Err(failure) if failure.journal.offered => failure.actions.insert(0, undo),
            _ => {}
        }
    }
    if changed {
        save(tab, cx);
    }
}

pub(crate) fn refuse(tab: &mut Tab, title: String, reason: &str, cx: &mut Context<Tab>) {
    let id = tab.ui.journal.next;
    let entry = Entry {
        id,
        title,
        changes: vec![Change::NotUndoable(reason.to_owned())],
        at: now(),
        head: None,
        was: None,
    };
    tab.ui.journal.push(entry);
    save(tab, cx);
}

fn save(tab: &Tab, cx: &mut App) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    let space = space(repo);
    let stored = tab.ui.journal.stored();
    crate::settings::Memory::update(cx, |memory| {
        memory.change_repo(&common, |view| {
            if space == MAIN_WORKTREE {
                view.journal = None;
            }
            match stored {
                Some(stored) => view.journals.insert(space, stored),
                None => view.journals.remove(&space),
            };
        })
    });
}

pub(crate) fn stored_for(view: &crate::settings::RepoView, space: &str) -> Option<Stored> {
    view.journals.get(space).cloned().or_else(|| (space == MAIN_WORKTREE).then(|| view.journal.clone()).flatten())
}

pub(crate) fn restore(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let space = space(&repo);
    let view = crate::settings::memory(cx).and_then(|memory| memory.repo(repo.common_dir()));
    let legacy = view.is_some_and(|view| space == MAIN_WORKTREE && view.journal.is_some());
    let stored = view.and_then(|view| stored_for(view, &space)).unwrap_or_default();
    tab.ui.journal = State::from_stored(stored);
    let named = tab.ui.journal.named();
    let epoch = tab.epoch;
    let id = tab.ui.jobs.start_write("Tidying the undo history…", crate::jobs::Link::new(false), None);
    Tab::work(
        cx,
        move || sweep(&repo, &named),
        move |tab, swept, cx| {
            if tab.epoch != epoch {
                return;
            }
            if tab.ui.jobs.write.as_ref().is_some_and(|job| job.id == id) {
                tab.ui.jobs.end_write();
            }
            match swept {
                Ok(gone) if legacy || !gone.is_empty() => {
                    if let Some(repo) = &tab.repo {
                        let common = repo.common_dir().to_owned();
                        crate::settings::Memory::update(cx, |memory| {
                            memory.change_repo(&common, |view| view.journals.retain(|space, _| !gone.contains(space)))
                        });
                    }
                    if legacy {
                        save(tab, cx);
                    }
                }
                _ => {}
            }
            let refs_changed = std::mem::take(&mut tab.refs_dirty);
            let status_changed = std::mem::take(&mut tab.status_dirty);
            if refs_changed {
                tab.refresh(true, cx);
            } else if status_changed {
                tab.load_status(cx);
            }
            crate::operation::refresh(tab, cx);
            tab.resume(cx);
            cx.notify();
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Undo(id) => run(tab, Way::Undo, id, false, cx),
        Action::Redo(id) => run(tab, Way::Redo, id, false, cx),
    }
}

pub(crate) fn run(tab: &mut Tab, way: Way, id: Option<u64>, stash: bool, cx: &mut Context<Tab>) {
    let Some(entry) = tab.ui.journal.last(way).cloned() else {
        tab.ui.say(view::Tone::Info, match way {
            Way::Undo => NOTHING_TO_UNDO,
            Way::Redo => NOTHING_TO_REDO,
        });
        cx.notify();
        return;
    };
    let elsewhere = |entries: &[Entry]| id.is_some_and(|id| entries.iter().any(|kept| kept.id == id));
    let done = match way {
        Way::Undo => elsewhere(&tab.ui.journal.redo),
        Way::Redo => elsewhere(&tab.ui.journal.undo),
    };
    if done {
        let reason = match way {
            Way::Undo => "That was already undone",
            Way::Redo => "That was already redone",
        };
        tab.ui.say(view::Tone::Info, reason);
        cx.notify();
        return;
    }
    if id.is_some_and(|id| id != entry.id) {
        let reason = match way {
            Way::Undo => format!("{} came after it. Undo that first.", entry.title),
            Way::Redo => "Something else was undone or done since, so this cannot be redone from here.".to_owned(),
        };
        tab.ui.say(view::Tone::Info, reason);
        cx.notify();
        return;
    }
    if let Availability::Disabled(reason) = availability(&tab.ui, way) {
        tab.ui.say(view::Tone::Info, reason);
        cx.notify();
        return;
    }
    let (progress, notice) = match way {
        Way::Undo => ("Undoing…", "Undone"),
        Way::Redo => ("Redoing…", "Redone"),
    };
    let landed = Arc::new(Mutex::new(None));
    let replayed = landed.clone();
    let kept = autostash::Kept::default();
    let left = kept.clone();
    tab.replaying = true;
    tab.run_operation_or(
        cx,
        progress,
        notice,
        true,
        move |repo| {
            let doing = match way {
                Way::Undo => "undoing",
                Way::Redo => "redoing",
            };
            let how = autostash::Around { doing: doing.to_owned(), stash, kept: &kept };
            let id = entry.id;
            autostash::around(
                &repo,
                how,
                || Retry::Journal { redo: way == Way::Redo, id },
                |repo| {
                    let done = replay(repo, &entry, way)?;
                    let outcome = told(&entry, way, &done.detail);
                    *landed.lock().unwrap_or_else(|e| e.into_inner()) = Some(done.entry);
                    Ok(outcome)
                },
                |_| None,
            )
        },
        move |tab, cx| {
            if let Some(entry) = replayed.lock().unwrap_or_else(|e| e.into_inner()).take() {
                tab.ui.journal.replayed(way, entry);
                save(tab, cx);
            }
        },
        move |tab, _| left.remember(&mut tab.ui),
    );
}

fn told(entry: &Entry, way: Way, detail: &str) -> Outcome {
    let (title, offer, action) = match way {
        Way::Undo => (format!("Undid: {}", entry.title), "Redo", Action::Redo(Some(entry.id))),
        Way::Redo => (format!("Redid: {}", entry.title), "Undo", Action::Undo(Some(entry.id))),
    };
    Outcome::titled(title, detail).offer(offer, view::Action::Journal(action))
}

#[derive(Debug)]
pub(crate) struct Replayed {
    pub(crate) entry: Entry,
    pub(crate) detail: String,
}

fn changed_since(what: &str, way: Way) -> Failure {
    Failure::from(format!("Something else changed {what} since. {} cannot be applied safely.", way.verb()))
}

fn current(repo: &Repository, reference: &str) -> Result<Option<String>, String> {
    if reference == "HEAD" {
        return Ok(match head_place(repo)? {
            Some(Place::Detached(oid)) => Some(oid),
            _ => None,
        });
    }
    read_one(repo, reference)
}

fn read_one(repo: &Repository, reference: &str) -> Result<Option<String>, String> {
    let listed = checked(repo.read_command().args(["for-each-ref", "--format=%(refname)%00%(objectname)", "--"]).arg(reference))?;
    Ok(listed.split(|&b| b == b'\n').find_map(|line| {
        let at = line.iter().position(|&b| b == 0)?;
        (&line[..at] == reference.as_bytes()).then(|| String::from_utf8_lossy(&line[at + 1..]).into_owned())
    }))
}

fn check(repo: &Repository, entry: &Entry, way: Way) -> Result<(), Failure> {
    if entry.touches_files() {
        let expected = match way {
            Way::Undo => &entry.head,
            Way::Redo => &entry.was,
        };
        if head_place(repo)? != *expected {
            return Err(changed_since("the checked-out branch", way));
        }
    }
    let stashes: Vec<String> = repo.stashes()?.into_iter().map(|entry| entry.oid).collect();
    for change in &entry.changes {
        match (change, way) {
            (Change::NotUndoable(reason), _) => return Err(Failure::from(reason.clone())),
            (Change::MoveBranch { reference, before, after, .. }, _) => {
                let expected = if way == Way::Undo { after } else { before };
                if current(repo, reference)?.as_ref() != Some(expected) {
                    return Err(changed_since(&shown_ref(reference), way));
                }
            }
            (Change::DeleteRef { reference, oid, .. }, Way::Redo) | (Change::CreateRef { reference, oid, .. }, Way::Undo) => {
                if current(repo, reference)?.as_ref() != Some(oid) {
                    return Err(changed_since(&shown_ref(reference), way));
                }
            }
            (Change::DeleteRef { reference, .. }, Way::Undo) | (Change::CreateRef { reference, .. }, Way::Redo) => {
                if current(repo, reference)?.is_some() {
                    return Err(changed_since(&shown_ref(reference), way));
                }
            }
            (Change::RenameBranch { old, new, oid }, _) => {
                let (there, gone) = if way == Way::Undo { (new, old) } else { (old, new) };
                if current(repo, there)?.as_ref() != Some(oid) || current(repo, gone)?.is_some() {
                    return Err(changed_since(&shown_ref(there), way));
                }
            }
            (Change::DropStash { oid, .. }, _) => {
                if stashes.contains(oid) == (way == Way::Undo) {
                    return Err(changed_since("the stash list", way));
                }
            }
            (Change::Checkout { .. } | Change::Discard { .. }, _) => {}
        }
    }
    Ok(())
}

pub(crate) fn replay(repo: &Repository, entry: &Entry, way: Way) -> Result<Replayed, Failure> {
    check(repo, entry, way)?;
    let mut entry = entry.clone();
    let mut said = Vec::new();
    let order: Vec<usize> = match way {
        Way::Undo => (0..entry.changes.len()).rev().collect(),
        Way::Redo => (0..entry.changes.len()).collect(),
    };
    let mut applied = Vec::new();
    for (step, index) in order.into_iter().enumerate() {
        let change = entry.changes[index].clone();
        let result = match way {
            Way::Undo => undo_one(repo, &entry, &change),
            Way::Redo => redo_one(repo, &change),
        };
        match result {
            Ok((sentence, replaced)) => {
                if let Some(replaced) = replaced {
                    entry.changes[index] = replaced;
                }
                said.extend(sentence);
                applied.push(index);
            }
            Err(failure) if step == 0 => return Err(failure),
            Err(failure) if put_back(repo, &entry, way, &applied) => return Err(failure),
            Err(failure) => {
                let _ = keep(repo, entry.id, &entry.objects());
                let text = format!(
                    "{} was only partly applied — {} {}",
                    way.verb(),
                    said.join(" "),
                    failure.text.trim()
                );
                return Err(Failure { text, ..failure });
            }
        }
    }
    if way == Way::Redo {
        let _ = release(repo, &[entry.id]);
        keep(repo, entry.id, &entry.objects())?;
    }
    Ok(Replayed { entry, detail: said.join(" ") })
}

fn put_back(repo: &Repository, entry: &Entry, way: Way, applied: &[usize]) -> bool {
    let restored_files = applied.iter().any(|&index| matches!(entry.changes[index], Change::Discard { .. }));
    if way == Way::Undo && restored_files {
        return false;
    }
    applied.iter().rev().all(|&index| {
        let change = &entry.changes[index];
        match way {
            Way::Undo => redo_one(repo, change).is_ok(),
            Way::Redo => undo_one(repo, entry, change).is_ok(),
        }
    })
}

fn branch_name(reference: &str) -> Option<&str> {
    reference.strip_prefix("refs/heads/")
}

fn set_upstream(repo: &Repository, reference: &str, upstream: &Option<Upstream>) -> Result<(), String> {
    let (Some(name), Some(upstream)) = (branch_name(reference), upstream) else { return Ok(()) };
    for (field, value) in [("remote", &upstream.remote), ("merge", &upstream.merge)] {
        if value.starts_with('-') {
            return Err("The branch's upstream setting cannot be restored.".into());
        }
        checked(repo.command().args(["config", "--local"]).arg(format!("branch.{name}.{field}")).arg(value))?;
    }
    Ok(())
}

fn forget_upstream(repo: &Repository, reference: &str) {
    if let Some(name) = branch_name(reference) {
        let _ = repo
            .command()
            .args(["config", "--local", "--remove-section", "--"])
            .arg(format!("branch.{name}"))
            .run();
    }
}

fn create(repo: &Repository, reference: &str, oid: &str, upstream: &Option<Upstream>) -> Result<(), Failure> {
    validate_oid(oid)?;
    checked(repo.command().args(["update-ref", "-m", "FastKraken: undo", "--"]).args([reference, oid, ""]))?;
    set_upstream(repo, reference, upstream)?;
    Ok(())
}

fn in_another_worktree(repo: &Repository, reference: &str) -> Result<(), Failure> {
    if branch_name(reference).is_none() {
        return Ok(());
    }
    let listed = checked(repo.read_command().args(["for-each-ref", "--format=%(refname)%00%(worktreepath)", "--"]).arg(reference))?;
    let elsewhere = listed.split(|&b| b == b'\n').find_map(|line| {
        let at = line.iter().position(|&b| b == 0)?;
        let path = &line[at + 1..];
        (&line[..at] == reference.as_bytes() && !path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path)))
    });
    match elsewhere {
        Some(path) if !crate::repository::same_place(&path, &repo.root) => {
            let folder = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
            Err(Failure::from(format!(
                "{} is checked out in worktree {folder}, so it cannot be changed from here. Open that worktree to change it there.",
                shown_ref(reference)
            )))
        }
        _ => Ok(()),
    }
}

fn delete(repo: &Repository, reference: &str, oid: &str, may_be_current: bool) -> Result<(), Failure> {
    validate_oid(oid)?;
    if !may_be_current && head_place(repo)? == Some(Place::Branch(reference.to_owned())) {
        return Err(Failure::from(format!("{} is checked out, so it cannot be removed.", shown_ref(reference))));
    }
    in_another_worktree(repo, reference)?;
    checked(repo.command().args(["update-ref", "-d", "--"]).args([reference, oid]))?;
    forget_upstream(repo, reference);
    Ok(())
}

fn move_to(repo: &Repository, reference: &str, from: &str, to: &str, mode: Mode) -> Result<(), Failure> {
    validate_oid(from)?;
    validate_oid(to)?;
    let checked_out = reference == "HEAD" || head_place(repo)? == Some(Place::Branch(reference.to_owned()));
    if checked_out {
        checked(repo.command().args(["reset", "--quiet", mode.flag(), to, "--"]))?;
    } else {
        in_another_worktree(repo, reference)?;
        checked(repo.command().args(["update-ref", "-m", "FastKraken: undo", "--"]).args([reference, to, from]))?;
    }
    Ok(())
}

fn switch(repo: &Repository, place: &Place) -> Result<(), Failure> {
    let mut command = repo.command();
    match place {
        Place::Branch(full) => {
            let name = branch_name(full).ok_or_else(|| "Only a local branch can be checked out again.".to_owned())?;
            command.args(["switch", "--quiet", "--no-guess", "--"]).arg(name);
        }
        Place::Detached(oid) => {
            validate_oid(oid)?;
            command.args(["switch", "--quiet", "--detach", oid, "--"]);
        }
    }
    checked(&mut command)?;
    Ok(())
}

fn still_upstream(repo: &Repository, reference: &str, gone: &str) -> Option<String> {
    branch_name(reference)?;
    let listed = checked(
        repo.read_command().args(["for-each-ref", "--format=%(upstream)%00%(upstream:short)", "--"]).arg(reference),
    )
    .ok()?;
    let listed = String::from_utf8_lossy(&listed).trim().to_owned();
    let (full, shown) = listed.split_once('\0')?;
    if full.is_empty() {
        return None;
    }
    let tip = repo.resolve(OsStr::new(full)).ok()?;
    repo.is_ancestor(gone, &tip).ok()?.then(|| {
        format!("The remote still has it: {shown} includes {}, so pushing would need a force push.", notice::short(gone))
    })
}

type Step = Result<(Option<String>, Option<Change>), Failure>;

fn undo_one(repo: &Repository, entry: &Entry, change: &Change) -> Step {
    let said = match change {
        Change::MoveBranch { reference, before, after, mode } => {
            move_to(repo, reference, after, before, *mode)?;
            let mut said = format!("{} is back at {}.", shown_ref(reference), notice::short(before));
            match mode {
                Mode::Soft if repo.is_ancestor(before, after).unwrap_or(false) => said.push_str(" The undone changes are staged."),
                Mode::Mixed => said.push_str(" The undone changes are in your files, unstaged."),
                _ => {}
            }
            if let Some(remote) = still_upstream(repo, reference, after) {
                said.push(' ');
                said.push_str(&remote);
            }
            said
        }
        Change::Checkout { from, .. } => {
            switch(repo, from)?;
            format!("Back on {}.", from.shown())
        }
        Change::DeleteRef { reference, oid, upstream } => {
            create(repo, reference, oid, upstream)?;
            format!("{} is back at {}.", shown_ref(reference), notice::short(oid))
        }
        Change::CreateRef { reference, oid, .. } => {
            let unborn = entry.was.as_ref() == Some(&Place::Branch(reference.clone()));
            delete(repo, reference, oid, unborn)?;
            format!("{} is gone again.", shown_ref(reference))
        }
        Change::RenameBranch { old, new, .. } => {
            rename(repo, new, old)?;
            format!("{} is called {} again.", shown_ref(new), shown_ref(old))
        }
        Change::Discard { saved, discarded } => {
            match discarded {
                Discarded::All | Discarded::Tracked => {
                    repo.restore_all(saved)?;
                }
                Discarded::Files(paths) => {
                    repo.restore_files(saved, &paths.iter().map(|path| raw_path(path)).collect::<Vec<_>>())?;
                }
            }
            "The discarded changes are back.".to_owned()
        }
        Change::DropStash { oid, message } => {
            repo.store_stash(oid, message)?;
            "The stash is back as stash@{0}.".to_owned()
        }
        Change::NotUndoable(reason) => return Err(Failure::from(reason.clone())),
    };
    Ok((Some(said), None))
}

fn rename(repo: &Repository, from: &str, to: &str) -> Result<(), Failure> {
    let (Some(from), Some(to)) = (branch_name(from), branch_name(to)) else {
        return Err(Failure::from("Only a local branch can be renamed back.".to_owned()));
    };
    checked(repo.command().args(["branch", "-m", "--", from, to]))?;
    Ok(())
}

fn redo_one(repo: &Repository, change: &Change) -> Step {
    let said = match change {
        Change::MoveBranch { reference, before, after, mode } => {
            move_to(repo, reference, before, after, *mode)?;
            format!("{} is at {} again.", shown_ref(reference), notice::short(after))
        }
        Change::Checkout { to, .. } => {
            switch(repo, to)?;
            format!("On {} again.", to.shown())
        }
        Change::DeleteRef { reference, oid, .. } => {
            delete(repo, reference, oid, false)?;
            format!("{} is deleted again.", shown_ref(reference))
        }
        Change::CreateRef { reference, oid, upstream } => {
            create(repo, reference, oid, upstream)?;
            format!("{} is back at {}.", shown_ref(reference), notice::short(oid))
        }
        Change::RenameBranch { old, new, .. } => {
            rename(repo, old, new)?;
            format!("{} is called {} again.", shown_ref(old), shown_ref(new))
        }
        Change::Discard { discarded, .. } => {
            let saved = match discarded {
                Discarded::All => repo.stash_away()?,
                Discarded::Tracked => {
                    let saved = repo.save_tracked()?.ok_or_else(|| "There are no changes to discard again.".to_owned())?;
                    checked(repo.command().args(["reset", "--quiet", "--hard", "--no-recurse-submodules", "HEAD", "--"]))?;
                    saved
                }
                Discarded::Files(paths) => {
                    let paths: Vec<PathBuf> = paths.iter().map(|path| raw_path(path)).collect();
                    repo.discard_again(&paths)?
                }
            };
            let replaced = Change::Discard { saved, discarded: discarded.clone() };
            return Ok((Some("The changes are discarded again.".to_owned()), Some(replaced)));
        }
        Change::DropStash { oid, .. } => {
            let (position, entries) = repo.stash_position(oid)?;
            repo.drop_at(oid, position, entries.len())?;
            "The stash is dropped again.".to_owned()
        }
        Change::NotUndoable(reason) => return Err(Failure::from(reason.clone())),
    };
    Ok((Some(said), None))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn entry_of(repo: &Repository, job: impl FnOnce(&Repository) -> Result<Outcome, Failure>) -> Entry {
        let mut state = State::default();
        let watch = state.watch(true);
        let (result, seen) = watched(repo, Some(watch), || job(repo));
        result.expect("the job ran");
        state.settle(seen.unwrap()).cloned().expect("an entry")
    }
    use crate::operation::tests::{commit, oid, write};
    use crate::repository::tests::Fixture;
    use std::fs;

    fn snapshot(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            f.git(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags", "refs/stash"]),
            f.git(&["ls-files", "-s"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
        )
    }

    fn files(f: &Fixture) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for entry in fs::read_dir(&f.dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != ".git" {
                found.insert(name, fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        found
    }

    struct Journal {
        state: State,
    }

    impl Journal {
        fn new() -> Self {
            Self { state: State::default() }
        }

        fn run(&mut self, f: &Fixture, report: Report, job: impl FnOnce(&Fixture)) -> Entry {
            let watch = self.state.watch(true);
            let (_, watched) = watched(&f.repo, Some(watch), || {
                job(f);
                Ok(Outcome { journal: report, ..Outcome::default() })
            });
            self.state.settle(watched.unwrap()).cloned().expect("an entry")
        }

        fn undo(&mut self, f: &Fixture) -> Result<Replayed, Failure> {
            let entry = self.state.last(Way::Undo).cloned().unwrap();
            let done = replay(&f.repo, &entry, Way::Undo)?;
            self.state.replayed(Way::Undo, done.entry.clone());
            Ok(done)
        }

        fn redo(&mut self, f: &Fixture) -> Result<Replayed, Failure> {
            let entry = self.state.last(Way::Redo).cloned().unwrap();
            let done = replay(&f.repo, &entry, Way::Redo)?;
            self.state.replayed(Way::Redo, done.entry.clone());
            Ok(done)
        }
    }

    fn soft(title: &str) -> Report {
        Report { title: Some(title.to_owned()), mode: Some(Mode::Soft), ..Report::default() }
    }

    fn base() -> Fixture {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f
    }

    #[test]
    fn undoing_a_commit_stages_its_changes_and_redo_puts_the_commit_back() {
        let f = base();
        let parent = oid(&f, "HEAD");
        write(&f, "camp.txt", "Camp\n");
        f.git(&["add", "camp.txt"]);
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let entry = journal.run(&f, soft("Commit 'Pitch the camp'"), |f| {
            f.git(&["commit", "-qm", "Pitch the camp"]);
        });
        let committed = oid(&f, "HEAD");
        assert_eq!(entry.title, "Commit 'Pitch the camp'");
        assert_eq!(
            entry.changes,
            [Change::MoveBranch { reference: "refs/heads/main".into(), before: parent.clone(), after: committed.clone(), mode: Mode::Soft }]
        );
        let done = journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        assert_eq!(done.detail, format!("main is back at {}. The undone changes are staged.", notice::short(&parent)));
        journal.redo(&f).unwrap();
        assert_eq!(oid(&f, "HEAD"), committed);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
    }

    #[test]
    fn a_mixed_reset_is_undone_with_a_mixed_reset() {
        let f = base();
        let start = oid(&f, "HEAD");
        let tip = commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let report = Report { mode: Some(Mode::Mixed), ..Report::default() };
        let entry = journal.run(&f, report, |f| {
            f.git(&["reset", "-q", "--mixed", &start]);
        });
        assert_eq!(entry.title, format!("Move main to {}", notice::short(&start)));
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"?? camp.txt\n");
        journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        journal.redo(&f).unwrap();
        assert_eq!(oid(&f, "HEAD"), start);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"?? camp.txt\n");
        let _ = tip;
    }

    #[test]
    fn a_merge_is_undone_with_a_keep_reset_that_refuses_to_overwrite_local_edits() {
        let f = base();
        f.git(&["switch", "-qc", "ridge"]);
        commit(&f, "ridge.txt", "Ridge\n", "Climb the ridge");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "lake.txt", "Lake\n", "Map the lake");
        let before = snapshot(&f);
        let tip = oid(&f, "main");
        let mut journal = Journal::new();
        journal.run(&f, Report::default(), |f| {
            f.git(&["merge", "-q", "--no-edit", "ridge"]);
        });
        let merged = oid(&f, "main");
        write(&f, "ridge.txt", "Ridge, edited\n");
        let entry = journal.state.last(Way::Undo).cloned().unwrap();
        let refused = replay(&f.repo, &entry, Way::Undo).unwrap_err();
        assert!(autostash::in_the_way(&refused), "{refused:?}");
        assert_eq!(oid(&f, "main"), merged);
        f.git(&["checkout", "--", "ridge.txt"]);
        journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        assert_eq!(oid(&f, "main"), tip);
        journal.redo(&f).unwrap();
        assert_eq!(oid(&f, "main"), merged);
    }

    #[test]
    fn a_hard_reset_saves_the_edits_it_throws_away_and_undo_brings_both_back() {
        let f = base();
        let start = oid(&f, "HEAD");
        commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        write(&f, "trail.txt", "Trail, edited\n");
        write(&f, "camp.txt", "Camp, staged\n");
        f.git(&["add", "camp.txt"]);
        let before = (snapshot(&f), files(&f));
        let mut journal = Journal::new();
        let saved = f.repo.save_tracked().unwrap().unwrap();
        let report = Report {
            mode: Some(Mode::Keep),
            changes: vec![Change::Discard { saved, discarded: Discarded::Tracked }],
            ..Report::default()
        };
        journal.run(&f, report, |f| {
            f.git(&["reset", "-q", "--hard", &start]);
        });
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        journal.undo(&f).unwrap();
        assert_eq!((snapshot(&f), files(&f)), before);
        journal.redo(&f).unwrap();
        assert_eq!(oid(&f, "HEAD"), start);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        journal.undo(&f).unwrap();
        assert_eq!((snapshot(&f), files(&f)), before);
    }

    #[test]
    fn an_undo_stopped_by_an_edit_in_the_way_moves_nothing_and_can_be_tried_again() {
        let f = base();
        let start = oid(&f, "HEAD");
        let tip = commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        write(&f, "trail.txt", "Trail, edited\n");
        let mut journal = Journal::new();
        let saved = f.repo.save_tracked().unwrap().unwrap();
        let report = Report {
            mode: Some(Mode::Keep),
            changes: vec![Change::Discard { saved, discarded: Discarded::Tracked }],
            ..Report::default()
        };
        journal.run(&f, report, |f| {
            f.git(&["reset", "-q", "--hard", &start]);
        });
        write(&f, "trail.txt", "Trail, edited again\n");
        let before = snapshot(&f);
        let refused = journal.undo(&f).unwrap_err();
        assert!(autostash::in_the_way(&refused), "{}", refused.text);
        assert_eq!(snapshot(&f), before);
        assert_eq!(oid(&f, "HEAD"), start);
        f.git(&["checkout", "--", "trail.txt"]);
        journal.undo(&f).unwrap();
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail, edited\n");
    }

    #[test]
    fn a_branch_switch_is_undone_by_switching_back() {
        let f = base();
        f.git(&["branch", "ridge"]);
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["switch", "-q", "ridge"]);
        });
        assert_eq!(entry.title, "Switch from main to ridge");
        journal.undo(&f).unwrap();
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(snapshot(&f), before);
        journal.redo(&f).unwrap();
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/ridge\n");
    }

    #[test]
    fn a_deleted_branch_comes_back_with_its_upstream_and_redo_deletes_it_again() {
        let f = base();
        f.git(&["switch", "-qc", "ridge"]);
        let tip = commit(&f, "ridge.txt", "Ridge\n", "Climb the ridge");
        f.git(&["switch", "-q", "main"]);
        f.git(&["config", "branch.ridge.remote", "origin"]);
        f.git(&["config", "branch.ridge.merge", "refs/heads/ridge"]);
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "-q", "-D", "ridge"]);
        });
        assert_eq!(entry.title, "Delete branch ridge");
        let held: Vec<String> = held(&f.repo).unwrap().into_iter().map(|(_, name)| name).collect();
        assert_eq!(held, [private(&root(&f.repo), entry.id, 0)]);
        assert!(held[0].starts_with("refs/fastkraken/undo/main/"), "{held:?}");
        assert_eq!(oid(&f, &held[0]), tip);
        journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        assert_eq!(f.git(&["config", "branch.ridge.merge"]), b"refs/heads/ridge\n");
        assert_eq!(f.git(&["config", "branch.ridge.remote"]), b"origin\n");
        journal.redo(&f).unwrap();
        assert!(f.git(&["for-each-ref", "refs/heads/ridge"]).is_empty());
        assert!(f.repo.command().args(["config", "branch.ridge.remote"]).output().unwrap().stdout.is_empty());
    }

    #[test]
    fn a_created_branch_and_tag_are_removed_only_while_still_at_their_commit() {
        let f = base();
        let start = oid(&f, "HEAD");
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "ridge"]);
            f.git(&["tag", "-a", "-m", "Camp", "v1"]);
        });
        assert_eq!(entry.title, "Create branch ridge and create tag v1");
        let before = snapshot(&f);
        journal.undo(&f).unwrap();
        assert!(f.git(&["for-each-ref", "refs/heads/ridge", "refs/tags/v1"]).is_empty());
        journal.redo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        assert_eq!(f.git(&["cat-file", "-t", "v1"]), b"tag\n");
        commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        f.git(&["branch", "-f", "ridge", "HEAD"]);
        let refused = journal.undo(&f).unwrap_err();
        assert_eq!(refused.text, "Something else changed ridge since. Undo cannot be applied safely.");
        assert_ne!(oid(&f, "ridge"), start);
        assert_eq!(f.git(&["cat-file", "-t", "v1"]), b"tag\n");
    }

    #[test]
    fn a_rename_is_undone_by_renaming_back() {
        let f = base();
        f.git(&["branch", "ridge"]);
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "-m", "ridge", "summit"]);
        });
        assert_eq!(entry.title, "Rename ridge to summit");
        journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        journal.redo(&f).unwrap();
        assert!(!f.git(&["for-each-ref", "refs/heads/summit"]).is_empty());
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "-m", "main", "trunk"]);
        });
        assert_eq!(entry.title, "Rename main to trunk");
        journal.undo(&f).unwrap();
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
    }

    #[test]
    fn a_discard_all_comes_back_with_its_untracked_file() {
        let f = base();
        write(&f, "trail.txt", "Trail, edited\n");
        write(&f, "camp.txt", "Camp\n");
        let before = (snapshot(&f), files(&f));
        let mut journal = Journal::new();
        let watch = journal.state.watch(true);
        let (_, seen) = watched(&f.repo, Some(watch), || {
            let saved = f.repo.stash_away().unwrap();
            Ok(Outcome::default().changed(Change::Discard { saved, discarded: Discarded::All }))
        });
        journal.state.settle(seen.unwrap()).unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert!(f.git(&["stash", "list"]).is_empty());
        journal.undo(&f).unwrap();
        assert_eq!((snapshot(&f), files(&f)), before);
        journal.redo(&f).unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        let Some(Change::Discard { saved, .. }) = journal.state.last(Way::Undo).unwrap().changes.first().cloned() else { panic!() };
        let entry = journal.state.last(Way::Undo).unwrap().id;
        assert!(held(&f.repo).unwrap().iter().any(|(id, name)| *id == entry && oid(&f, name) == saved));
        journal.undo(&f).unwrap();
        assert_eq!((snapshot(&f), files(&f)), before);
    }

    #[test]
    fn a_discard_of_picked_files_is_restored_and_refused_once_a_file_changed() {
        let f = base();
        commit(&f, "lake.txt", "Lake\n", "Map the lake");
        write(&f, "trail.txt", "Trail, edited\n");
        write(&f, "lake.txt", "Lake, edited\n");
        let before = (snapshot(&f), files(&f));
        let paths = vec![PathBuf::from("trail.txt"), PathBuf::from("lake.txt")];
        let mut journal = Journal::new();
        let watch = journal.state.watch(false);
        let (_, seen) = watched(&f.repo, Some(watch), || {
            let saved = f.repo.discard_files(&paths, &BTreeSet::new()).unwrap();
            Ok(Outcome::default().changed(Change::discarded_files(saved, &paths)))
        });
        journal.state.settle(seen.unwrap()).unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        journal.undo(&f).unwrap();
        assert_eq!((snapshot(&f), files(&f)), before);
        journal.redo(&f).unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        write(&f, "lake.txt", "Lake, edited elsewhere\n");
        let refused = journal.undo(&f).unwrap_err();
        assert_eq!(
            refused.text,
            "lake.txt changed since the discard, so Undo would overwrite it. Commit, stash or discard it first."
        );
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail\n");
    }

    #[test]
    fn a_dropped_stash_is_stored_again_and_redo_drops_it() {
        let f = base();
        write(&f, "trail.txt", "Trail, stashed\n");
        f.git(&["stash", "push", "-q", "-m", "Bench plans"]);
        let stash = oid(&f, "stash@{0}");
        let before = snapshot(&f);
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["stash", "drop", "-q"]);
        });
        assert_eq!(entry.title, "Drop stash Bench plans");
        journal.undo(&f).unwrap();
        assert_eq!(snapshot(&f), before);
        assert_eq!(f.git(&["stash", "list", "--format=%gs"]), b"On main: Bench plans\n");
        journal.redo(&f).unwrap();
        assert!(f.git(&["stash", "list"]).is_empty());
        assert_eq!(held(&f.repo).unwrap().len(), 1);
        assert_eq!(oid(&f, &held(&f.repo).unwrap()[0].1), stash);
    }

    #[test]
    fn undo_is_refused_after_an_outside_change_and_nothing_moves() {
        let f = base();
        write(&f, "camp.txt", "Camp\n");
        f.git(&["add", "camp.txt"]);
        let mut journal = Journal::new();
        journal.run(&f, soft("Commit 'Pitch the camp'"), |f| {
            f.git(&["commit", "-qm", "Pitch the camp"]);
        });
        let outside = commit(&f, "lake.txt", "Lake\n", "Map the lake");
        let before = snapshot(&f);
        let refused = journal.undo(&f).unwrap_err();
        assert_eq!(refused.text, "Something else changed main since. Undo cannot be applied safely.");
        assert_eq!(snapshot(&f), before);
        assert_eq!(oid(&f, "HEAD"), outside);
        f.git(&["branch", "ridge"]);
        f.git(&["switch", "-q", "ridge"]);
        f.git(&["reset", "-q", "--hard", "HEAD^"]);
        let refused = journal.undo(&f).unwrap_err();
        assert_eq!(refused.text, "Something else changed the checked-out branch since. Undo cannot be applied safely.");
    }

    #[test]
    fn a_push_cannot_be_undone() {
        let f = base();
        let mut state = State::default();
        let entry = Entry { id: 0, title: "Push main".into(), changes: vec![Change::NotUndoable(PUSHED.into())], at: 0, head: None, was: None };
        state.push(entry.clone());
        assert_eq!(state.last(Way::Undo).unwrap().refusal(), Some(PUSHED));
        let refused = replay(&f.repo, &entry, Way::Undo).unwrap_err();
        assert_eq!(refused.text, PUSHED);
        let watch = state.watch(true);
        let (_, seen) = watched(&f.repo, Some(watch), || Ok(Outcome::default().not_undoable(PUSHED)));
        let recorded = state.settle(seen.unwrap()).unwrap();
        assert_eq!(recorded.changes, [Change::NotUndoable(PUSHED.into())]);
    }

    #[test]
    fn a_rebase_that_stops_is_recorded_once_when_it_finishes() {
        let f = base();
        f.git(&["switch", "-qc", "ridge"]);
        commit(&f, "trail.txt", "Trail north\n", "Turn north");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.txt", "Trail south\n", "Turn south");
        f.git(&["switch", "-q", "ridge"]);
        let tip = oid(&f, "ridge");
        let mut state = State::default();
        let watch = state.watch(true);
        let (_, seen) = watched(&f.repo, Some(watch), || {
            let _ = f.repo.command().args(["rebase", "--merge", "main"]).output();
            Ok(Outcome::default())
        });
        assert!(state.settle(seen.unwrap()).is_none());
        assert!(state.pending.is_some());
        write(&f, "trail.txt", "Trail south, then north\n");
        f.git(&["add", "trail.txt"]);
        let watch = state.watch(true);
        let (_, seen) = watched(&f.repo, Some(watch), || {
            f.repo.command().env("GIT_EDITOR", "true").args(["rebase", "--continue"]).output().unwrap();
            Ok(Outcome::default())
        });
        let entry = state.settle(seen.unwrap()).cloned().unwrap();
        assert!(state.pending.is_none());
        let rebased = oid(&f, "ridge");
        assert_eq!(
            entry.changes,
            [Change::MoveBranch { reference: "refs/heads/ridge".into(), before: tip.clone(), after: rebased, mode: Mode::Keep }]
        );
        replay(&f.repo, &entry, Way::Undo).unwrap();
        assert_eq!(oid(&f, "ridge"), tip);
    }

    #[test]
    fn private_refs_stay_out_of_history_and_decorations() {
        let f = base();
        let kept = commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        f.git(&["reset", "-q", "--hard", "HEAD^"]);
        keep(&f.repo, 7, &[kept.clone()]).unwrap();
        let listed = f.repo.history(0, 10).unwrap();
        assert!(listed.iter().all(|row| row.oid != kept), "{listed:?}");
        let found = f.repo.search_history("Pitch", 10).unwrap();
        assert!(found.is_empty());
        let mut log = f.repo.log_command(false);
        log.arg(&kept).arg("--max-count=1");
        let decorated = checked(&mut log).unwrap();
        assert!(!String::from_utf8_lossy(&decorated).contains("fastkraken"));
        assert_eq!(held(&f.repo).unwrap(), [(7, "refs/fastkraken/undo/main/7/0".to_owned())]);
    }

    #[test]
    fn entries_past_fifty_and_cleared_redo_entries_release_their_refs() {
        let f = base();
        let mut state = State::default();
        for n in 0..=LIMIT {
            let watch = state.watch(true);
            let name = format!("trail-{n}");
            let (_, seen) = watched(&f.repo, Some(watch), || {
                f.git(&["branch", &name]);
                Ok(Outcome::default())
            });
            state.settle(seen.unwrap()).unwrap();
        }
        assert_eq!(state.undo_list().len(), LIMIT);
        assert_eq!(state.released(), [0]);
        assert_eq!(held(&f.repo).unwrap().len(), LIMIT + 1);
        let entry = state.last(Way::Undo).cloned().unwrap();
        let done = replay(&f.repo, &entry, Way::Undo).unwrap();
        state.replayed(Way::Undo, done.entry);
        let watch = state.watch(true);
        assert!(state.released().is_empty());
        let (_, seen) = watched(&f.repo, Some(watch), || {
            f.git(&["branch", "late"]);
            Ok(Outcome::default())
        });
        state.settle(seen.unwrap()).unwrap();
        assert!(state.redo_list().is_empty());
        assert_eq!(state.released(), [entry.id]);
        release(&f.repo, &state.released).unwrap();
        let ids: BTreeSet<u64> = held(&f.repo).unwrap().into_iter().map(|(id, _)| id).collect();
        assert!(!ids.contains(&0) && ids.contains(&1) && !ids.contains(&entry.id));
        assert!(sweep(&f.repo, &BTreeSet::from([2])).unwrap().is_empty());
        assert_eq!(held(&f.repo).unwrap(), [(2, private(&root(&f.repo), 2, 0))]);
    }

    #[test]
    fn undoing_a_commit_already_pushed_says_the_remote_still_has_it() {
        let f = base();
        write(&f, "camp.txt", "Camp\n");
        f.git(&["add", "camp.txt"]);
        let mut journal = Journal::new();
        journal.run(&f, soft("Commit 'Pitch the camp'"), |f| {
            f.git(&["commit", "-qm", "Pitch the camp"]);
        });
        let tip = oid(&f, "HEAD");
        f.git(&["update-ref", "refs/remotes/origin/main", &tip]);
        f.git(&["config", "branch.main.remote", "origin"]);
        f.git(&["config", "branch.main.merge", "refs/heads/main"]);
        f.git(&["config", "remote.origin.url", "/nowhere"]);
        f.git(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
        let done = journal.undo(&f).unwrap();
        assert!(
            done.detail.ends_with(&format!("The remote still has it: origin/main includes {}, so pushing would need a force push.", notice::short(&tip))),
            "{}",
            done.detail
        );
    }

    #[test]
    fn a_stash_save_alone_is_recorded_as_not_undoable() {
        let f = base();
        write(&f, "trail.txt", "Trail, stashed\n");
        let mut journal = Journal::new();
        let entry = journal.run(&f, Report::default(), |f| {
            f.git(&["stash", "push", "-q"]);
        });
        assert_eq!(entry.refusal(), Some(STASHED));
    }

    #[test]
    fn stored_journals_round_trip_and_keep_counting() {
        let entry = Entry {
            id: 4,
            title: "Commit 'Pitch the camp'".into(),
            changes: vec![
                Change::Discard { saved: "a".repeat(40), discarded: Discarded::Files(vec![b"camp.txt".to_vec()]) },
                Change::Checkout { from: Place::Branch("refs/heads/main".into()), to: Place::Detached("b".repeat(40)) },
            ],
            at: 1,
            head: Some(Place::Branch("refs/heads/main".into())),
            was: None,
        };
        let stored = Stored { undo: vec![entry], redo: Vec::new(), next: 2 };
        let text = serde_json::to_string(&stored).unwrap();
        let back: Stored = serde_json::from_str(&text).unwrap();
        assert_eq!(back, stored);
        let state = State::from_stored(back);
        assert_eq!(state.next, 5);
        assert_eq!(state.named(), BTreeSet::from([4]));
    }

    fn private_refs(f: &Fixture) -> Vec<String> {
        String::from_utf8(f.git(&["for-each-ref", "--format=%(refname) %(objectname)", UNDO])).unwrap().lines().map(str::to_owned).collect()
    }

    fn linked_worktree(f: &Fixture, folder: &str, branch: &str) -> (PathBuf, Repository) {
        let path = f.dir.join(folder);
        f.git(&["worktree", "add", "-q", "-b", branch, path.to_str().unwrap()]);
        let repo = f.open(&path);
        (path, repo)
    }

    fn deleted_in(repo: &Repository, state: &mut State, branch: &str) -> Entry {
        let watch = state.watch(true);
        let (result, seen) = watched(repo, Some(watch), || {
            checked(repo.command().args(["branch", "-q", "-D", branch]))?;
            Ok(Outcome::default())
        });
        result.unwrap();
        state.settle(seen.unwrap()).cloned().unwrap()
    }

    #[test]
    fn two_worktrees_keep_their_own_undo_refs_and_each_sweep_spares_the_other() {
        let f = base();
        let (_, linked) = linked_worktree(&f, "ridge-tree", "ridge");
        assert_eq!(space(&f.repo), MAIN_WORKTREE);
        assert_eq!(space(&linked), "worktrees/ridge-tree");
        let lake = commit(&f, "lake.txt", "Lake\n", "Map the lake");
        f.git(&["branch", "lake"]);
        f.git(&["reset", "-q", "--hard", "HEAD^"]);
        checked(linked.command().args(["branch", "pond", "main"])).unwrap();
        let pond = oid(&f, "pond");
        let (mut here, mut there) = (State::default(), State::default());
        let main_entry = deleted_in(&f.repo, &mut here, "lake");
        let linked_entry = deleted_in(&linked, &mut there, "pond");
        assert_eq!((main_entry.id, linked_entry.id), (0, 0));
        assert_eq!(
            private_refs(&f),
            [format!("refs/fastkraken/undo/main/0/0 {lake}"), format!("refs/fastkraken/undo/worktrees/ridge-tree/0/0 {pond}")]
        );
        assert_eq!(held(&linked).unwrap(), [(0, "refs/fastkraken/undo/worktrees/ridge-tree/0/0".to_owned())]);
        assert!(sweep(&linked, &there.named()).unwrap().is_empty());
        assert!(sweep(&f.repo, &here.named()).unwrap().is_empty());
        assert_eq!(private_refs(&f).len(), 2);
        replay(&linked, &linked_entry, Way::Undo).unwrap();
        replay(&f.repo, &main_entry, Way::Undo).unwrap();
        assert_eq!((oid(&f, "lake"), oid(&f, "pond")), (lake, pond));
        assert!(sweep(&linked, &BTreeSet::new()).unwrap().is_empty());
        assert_eq!(private_refs(&f), [format!("refs/fastkraken/undo/main/0/0 {}", oid(&f, "lake"))]);
    }

    #[test]
    fn a_removed_worktree_loses_its_undo_refs_at_the_next_sweep() {
        let f = base();
        let (path, linked) = linked_worktree(&f, "ridge-tree", "ridge");
        let tip = oid(&f, "HEAD");
        keep(&linked, 3, &[tip.clone()]).unwrap();
        keep(&f.repo, 3, &[tip.clone()]).unwrap();
        assert_eq!(sweep(&f.repo, &BTreeSet::from([3])).unwrap(), Vec::<String>::new());
        assert_eq!(private_refs(&f).len(), 2);
        f.git(&["worktree", "remove", "--force", path.to_str().unwrap()]);
        assert_eq!(sweep(&f.repo, &BTreeSet::from([3])).unwrap(), ["worktrees/ridge-tree"]);
        assert_eq!(private_refs(&f), [format!("refs/fastkraken/undo/main/3/0 {tip}")]);
    }

    #[test]
    fn a_removed_worktree_with_an_accented_name_loses_its_undo_refs_too() {
        let f = base();
        let (path, linked) = linked_worktree(&f, "café-tree", "café");
        assert_eq!(space(&linked), "worktrees/café-tree");
        let tip = oid(&f, "HEAD");
        keep(&linked, 2, &[tip.clone()]).unwrap();
        assert_eq!(held(&linked).unwrap(), [(2, "refs/fastkraken/undo/worktrees/café-tree/2/0".to_owned())]);
        assert!(sweep(&f.repo, &BTreeSet::new()).unwrap().is_empty());
        assert_eq!(private_refs(&f).len(), 1);
        f.git(&["worktree", "remove", "--force", path.to_str().unwrap()]);
        assert_eq!(sweep(&f.repo, &BTreeSet::new()).unwrap(), ["worktrees/café-tree"]);
        assert!(private_refs(&f).is_empty());
    }

    #[test]
    fn undo_refs_from_before_worktrees_were_kept_apart_move_to_the_main_worktree() {
        let f = base();
        let (_, linked) = linked_worktree(&f, "ridge-tree", "ridge");
        let tip = oid(&f, "HEAD");
        for name in ["refs/fastkraken/undo/3/0", "refs/fastkraken/undo/3/1", "refs/fastkraken/undo/4/0"] {
            f.git(&["update-ref", name, &tip]);
        }
        assert!(sweep(&linked, &BTreeSet::new()).unwrap().is_empty());
        assert_eq!(private_refs(&f).len(), 3);
        assert!(held(&f.repo).unwrap().is_empty());
        assert!(sweep(&f.repo, &BTreeSet::from([3])).unwrap().is_empty());
        assert_eq!(
            private_refs(&f),
            [format!("refs/fastkraken/undo/main/3/0 {tip}"), format!("refs/fastkraken/undo/main/3/1 {tip}")]
        );
        let entry = Entry { id: 3, title: "Commit".into(), changes: Vec::new(), at: 0, head: None, was: None };
        let legacy = Stored { undo: vec![entry], redo: Vec::new(), next: 4 };
        let mut view = crate::settings::RepoView { journal: Some(legacy.clone()), ..Default::default() };
        assert_eq!(stored_for(&view, MAIN_WORKTREE), Some(legacy.clone()));
        assert_eq!(stored_for(&view, "worktrees/ridge-tree"), None);
        view.journals.insert(MAIN_WORKTREE.to_owned(), Stored::default());
        assert_eq!(stored_for(&view, MAIN_WORKTREE), Some(Stored::default()));
        let text = serde_json::to_string(&view).unwrap();
        let back: crate::settings::RepoView = serde_json::from_str(&text).unwrap();
        assert_eq!(back.journals, view.journals);
    }

    #[test]
    fn a_worktree_folder_with_an_unusual_name_gets_a_hashed_namespace() {
        let common = Path::new("/trail/.git");
        assert_eq!(space_of(common, common), "main");
        assert_eq!(space_of(Path::new("/trail/.git/worktrees/ridge-2"), common), "worktrees/ridge-2");
        let odd = space_of(Path::new("/trail/.git/worktrees/ridge tree"), common);
        assert!(odd.starts_with("other/") && odd.len() == "other/".len() + 16, "{odd}");
        assert_ne!(odd, space_of(Path::new("/trail/.git/worktrees/ridge  tree"), common));
        assert!(space_of(Path::new("/elsewhere/gitdir"), common).starts_with("other/"));
        assert!(!plain_component("x.lock") && !plain_component(".hidden") && !plain_component("a..b") && plain_component("a.b_c-d"));
    }

    #[test]
    fn undo_will_not_move_or_delete_a_branch_checked_out_in_another_worktree() {
        let f = base();
        let start = oid(&f, "HEAD");
        f.git(&["branch", "ridge"]);
        let tip = commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        let mut journal = Journal::new();
        journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "late"]);
        });
        let moved = journal.run(&f, Report::default(), |f| {
            f.git(&["branch", "-f", "ridge", "main"]);
        });
        assert_eq!(moved.changes, [Change::MoveBranch { reference: "refs/heads/ridge".into(), before: start.clone(), after: tip.clone(), mode: Mode::Keep }]);
        let path = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", path.to_str().unwrap(), "ridge"]);
        let refused = journal.undo(&f).unwrap_err();
        assert_eq!(
            refused.text,
            "ridge is checked out in worktree ridge-tree, so it cannot be changed from here. Open that worktree to change it there."
        );
        assert_eq!(oid(&f, "ridge"), tip);
        f.git(&["worktree", "remove", "--force", path.to_str().unwrap()]);
        journal.undo(&f).unwrap();
        assert_eq!(oid(&f, "ridge"), start);
        let late = f.dir.join("late-tree");
        f.git(&["worktree", "add", "-q", late.to_str().unwrap(), "late"]);
        let refused = journal.undo(&f).unwrap_err();
        assert!(refused.text.starts_with("late is checked out in worktree late-tree"), "{}", refused.text);
        assert_eq!(oid(&f, "late"), tip);
    }
}
