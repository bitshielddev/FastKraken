pub(crate) mod backend;

use crate::repository::Run;
use crate::{
    Tab, kit,
    notice::{self, Notice},
    prompt,
    repository::{Repository, checked, raw_path, validate_oid},
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use gpui::{Context, Div, FontWeight, Window, div, prelude::*, px, rgb};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;

const MARKER_LIMIT: u64 = 64 * 1024;

pub(crate) const UNCOMMITTED: &str = "Finish without committing";

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy, on_repo};
    fn during(ui: &Ui, target: &crate::commands::Target, applies: impl FnOnce(&Snapshot) -> bool) -> Availability {
        match ui.operation.snapshot() {
            Some(snapshot) if on_repo(ui, target) && ui.operation.active() && applies(snapshot) => {
                Availability::unless(busy(ui))
            }
            _ => Availability::Hidden,
        }
    }
    &[
        Command::new(
            "repo.abort-operation",
            Group::Reverse,
            |ui, _| format!("{}…", ui.operation.abort_label().unwrap_or("Abort")),
            |ui, target, _| during(ui, target, |s| s.abort_label().is_some()),
            |_, _| Some(view::Action::Operation(Action::Abort)),
        )
        .words(&["cancel merge", "cancel rebase", "abort merge", "abort rebase", "abort cherry-pick", "abort stash"])
        .icon("x")
        .danger(),
        Command::new(
            "repo.skip-commit",
            Group::Reverse,
            |_, _| "Skip commit".into(),
            |ui, target, _| during(ui, target, |s| s.can_skip),
            |_, _| Some(view::Action::Operation(Action::Skip)),
        )
        .words(&["rebase --skip", "cherry-pick --skip"])
        .icon("chevron-right"),
        Command::new(
            "repo.keep-empty",
            Group::Changes,
            |_, _| "Keep as an empty commit".into(),
            |ui, target, _| during(ui, target, |s| s.empty && s.message.is_some() && !s.uncommitted),
            |_, _| Some(view::Action::Operation(Action::KeepEmpty)),
        )
        .words(&["allow empty"])
        .icon("commit"),
        Command::new(
            "repo.view-conflicts",
            Group::View,
            |_, _| "View conflicts".into(),
            |ui, target, _| {
                if on_repo(ui, target) && ui.operation.active() {
                    Availability::Enabled
                } else {
                    Availability::Hidden
                }
            },
            |_, _| Some(view_conflicts()),
        )
        .words(&["merge", "rebase", "resolve"])
        .icon("alert"),
        Command::new(
            "repo.end-bisect",
            Group::Reverse,
            |_, _| "End bisect…".into(),
            |ui, target, _| {
                if on_repo(ui, target) && ui.operation.kind() == Some(Kind::Bisect) {
                    Availability::unless(busy(ui))
                } else {
                    Availability::Hidden
                }
            },
            |_, _| Some(view::Action::Operation(Action::EndBisect)),
        )
        .words(&["bisect reset", "stop bisecting"])
        .icon("x"),
    ]
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Patches,
    Applied,
    Bisect,
}

impl Kind {
    pub(crate) fn refusal(self) -> &'static str {
        match self {
            Self::Merge => "Finish or abort the merge first",
            Self::Rebase => "Finish or abort the rebase first",
            Self::CherryPick => "Finish or abort the cherry-pick first",
            Self::Revert => "Finish or abort the revert first",
            Self::Patches => "Finish or abort the patch series being applied first",
            Self::Applied => "Resolve the conflicted files first",
            Self::Bisect => "End the bisect first",
        }
    }

    pub(crate) fn holds(self) -> bool {
        self != Self::Bisect
    }

    fn can_skip(self) -> bool {
        matches!(self, Self::Rebase | Self::CherryPick | Self::Revert | Self::Patches)
    }

    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Patches => "patch series",
            Self::Applied => "conflicted change",
            Self::Bisect => "bisect",
        }
    }

    pub(crate) fn continue_label(self) -> Option<&'static str> {
        Some(match self {
            Self::Merge => "Commit merge",
            Self::Rebase => "Continue rebase",
            Self::CherryPick => "Continue cherry-pick",
            Self::Revert => "Commit revert",
            Self::Patches => "Continue applying patches",
            Self::Applied | Self::Bisect => return None,
        })
    }

    pub(crate) fn abort_label(self) -> Option<&'static str> {
        Some(match self {
            Self::Merge => "Abort merge",
            Self::Rebase => "Abort rebase",
            Self::CherryPick => ABORT_PICK,
            Self::Revert => "Abort revert",
            Self::Patches => "Abort patches",
            Self::Applied | Self::Bisect => return None,
        })
    }

    fn continuing(self) -> &'static str {
        match self {
            Self::Merge => "Committing the merge…",
            Self::Rebase => "Continuing the rebase…",
            Self::CherryPick => "Continuing the cherry-pick…",
            Self::Revert => "Committing the revert…",
            _ => "Continuing…",
        }
    }

    fn aborting(self) -> &'static str {
        match self {
            Self::Merge => "Aborting the merge…",
            Self::Rebase => "Aborting the rebase…",
            Self::CherryPick => "Aborting the cherry-pick…",
            Self::Revert => "Aborting the revert…",
            Self::Applied => "Aborting the stash apply…",
            _ => "Aborting…",
        }
    }

    fn markers(self, git_dir: &Path) -> &'static [&'static str] {
        match self {
            Self::Merge => &["MERGE_HEAD"],
            Self::Rebase if git_dir.join("rebase-merge").is_dir() => &[
                "rebase-merge/head-name",
                "rebase-merge/onto",
                "rebase-merge/msgnum",
                "rebase-merge/end",
            ],
            Self::Rebase => &["rebase-apply/head-name", "rebase-apply/onto", "rebase-apply/next", "rebase-apply/last"],
            Self::Patches => &["rebase-apply/next", "rebase-apply/last"],
            Self::CherryPick => &["CHERRY_PICK_HEAD", "sequencer/todo"],
            Self::Revert => &["REVERT_HEAD", "sequencer/todo"],
            Self::Applied => &[],
            Self::Bisect => &["BISECT_LOG"],
        }
    }
}

fn exists(git_dir: &Path, name: &str) -> Result<bool> {
    git_dir.join(name).try_exists().map_err(|e| format!("Read the operation state: {e}"))
}

fn read(git_dir: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let file = match fs::File::open(git_dir.join(name)) {
        Ok(file) => file,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(None),
        Err(e) => return Err(format!("Read the operation state: {e}")),
    };
    let mut bytes = Vec::new();
    file.take(MARKER_LIMIT).read_to_end(&mut bytes).map_err(|e| format!("Read the operation state: {e}"))?;
    Ok(Some(bytes))
}

fn read_line(git_dir: &Path, name: &str) -> Result<Option<String>> {
    Ok(read(git_dir, name)?.map(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        text.lines().next().unwrap_or_default().trim().to_owned()
    }))
}

pub(crate) fn detect(git_dir: &Path, unmerged: impl FnOnce() -> Result<bool>) -> Result<Option<Kind>> {
    if exists(git_dir, "rebase-merge")? {
        return Ok(Some(Kind::Rebase));
    }
    if exists(git_dir, "rebase-apply")? {
        return Ok(Some(if exists(git_dir, "rebase-apply/applying")? { Kind::Patches } else { Kind::Rebase }));
    }
    for (marker, kind) in [
        ("CHERRY_PICK_HEAD", Kind::CherryPick),
        ("REVERT_HEAD", Kind::Revert),
        ("MERGE_HEAD", Kind::Merge),
    ] {
        if exists(git_dir, marker)? {
            return Ok(Some(kind));
        }
    }
    if exists(git_dir, "sequencer")? {
        let next = read_line(git_dir, "sequencer/todo")?.unwrap_or_default();
        let revert = next.split_whitespace().next() == Some("revert");
        return Ok(Some(if revert { Kind::Revert } else { Kind::CherryPick }));
    }
    if unmerged()? {
        return Ok(Some(Kind::Applied));
    }
    if exists(git_dir, "BISECT_LOG")? {
        return Ok(Some(Kind::Bisect));
    }
    Ok(None)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Id {
    kind: Kind,
    markers: Vec<Option<Vec<u8>>>,
    head: Option<String>,
    branch: Option<Vec<u8>>,
}

impl Id {
    #[cfg(test)]
    pub(crate) fn blank() -> Self {
        Self { kind: Kind::Merge, markers: Vec::new(), head: None, branch: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Named {
    pub oid: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stashing {
    pub oid: String,
    pub title: String,
    pub pop: bool,
}

pub(crate) const ABORT_STASH: &str = "Abort stash apply";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picking {
    pub oid: String,
    pub label: String,
    pub message: String,
    pub squash: bool,
}

pub(crate) const ABORT_PICK: &str = "Abort cherry-pick";
pub(crate) const ABORT_SQUASH: &str = "Abort squash merge";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub path: PathBuf,
    pub shape: Shape,
    pub stages: Stages,
}

pub(crate) type Stages = [Option<Version>; 3];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub mode: String,
    pub oid: String,
    pub size: Option<u64>,
}

impl Conflict {
    pub(crate) fn side(&self, side: crate::merge::Side) -> Option<&Version> {
        match side {
            crate::merge::Side::Current => self.stages[1].as_ref(),
            crate::merge::Side::Incoming => self.stages[2].as_ref(),
        }
    }
}

impl Shape {
    pub(crate) fn fits_resolver(self) -> bool {
        matches!(self, Self::Text | Self::BothAdded)
    }

    pub(crate) fn may_hold_markers(self) -> bool {
        matches!(self, Self::Text | Self::BothAdded | Self::TooLarge)
    }

    pub(crate) fn deleted(self) -> bool {
        matches!(self, Self::DeletedByUs | Self::DeletedByThem)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Text,
    Binary,
    TooLarge,
    DeletedByUs,
    DeletedByThem,
    BothAdded,
    Symlink,
    Submodule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub kind: Kind,
    pub id: Id,
    pub branch: String,
    pub source: Option<Named>,
    pub commit: Option<Named>,
    pub step: Option<(usize, usize)>,
    pub conflicts: Vec<Conflict>,
    pub resolved: Vec<PathBuf>,
    pub message: Option<String>,
    pub can_skip: bool,
    pub empty: bool,
    pub queued: bool,
    pub uncommitted: bool,
    pub stash: Option<Stashing>,
    pub picked: Option<Picking>,
}

impl Snapshot {
    pub(crate) fn ours(&self) -> String {
        match self.kind {
            Kind::Rebase => format!("{}, with the commits replayed so far", self.target()),
            Kind::Applied => "your files".to_owned(),
            _ => self.branch.clone(),
        }
    }

    pub(crate) fn theirs(&self) -> String {
        let commit = self.commit.as_ref().map(|c| c.label.as_str());
        match self.kind {
            Kind::Merge => self.target(),
            Kind::Rebase => commit.map_or("your commit".to_owned(), |c| format!("your commit {c}")),
            Kind::CherryPick => commit.map_or("the picked commit".to_owned(), |c| format!("picked commit {c}")),
            Kind::Revert => commit.map_or("the reverted commit".to_owned(), |c| format!("undoing {c}")),
            Kind::Patches => "the patch being applied".to_owned(),
            Kind::Applied if self.stash.is_some() => "stashed changes".to_owned(),
            Kind::Applied => match &self.picked {
                Some(picked) if picked.squash => format!("squashed {}", picked.label),
                Some(picked) => format!("picked commit {}", picked.label),
                None => "applied change".to_owned(),
            },
            Kind::Bisect => String::new(),
        }
    }

    pub(crate) fn ours_name(&self) -> String {
        match self.kind {
            Kind::Rebase => self.target(),
            Kind::Applied => self.ours(),
            _ => self.branch.clone(),
        }
    }

    pub(crate) fn theirs_name(&self) -> String {
        let short = self.commit.as_ref().map(|c| notice::short(&c.oid).to_owned());
        match self.kind {
            Kind::Merge => self.target(),
            Kind::Rebase => short.unwrap_or_else(|| "your commit".to_owned()),
            Kind::CherryPick => short.unwrap_or_else(|| "the picked commit".to_owned()),
            Kind::Revert => short.map_or_else(|| "the revert".to_owned(), |short| format!("undoing {short}")),
            Kind::Patches => "the patch".to_owned(),
            Kind::Applied if self.stash.is_none() && self.picked.is_some() => self.picked.as_ref().map_or_else(String::new, |picked| {
                if picked.squash { picked.label.clone() } else { notice::short(&picked.oid).to_owned() }
            }),
            Kind::Applied | Kind::Bisect => self.theirs(),
        }
    }

    pub(crate) fn conflict(&self, path: &Path) -> Option<&Conflict> {
        self.conflicts.iter().find(|c| c.path == path)
    }

    fn target(&self) -> String {
        self.source.as_ref().map_or_else(|| "its target".to_owned(), |s| s.label.clone())
    }

    pub(crate) fn continue_label(&self) -> Option<&'static str> {
        if self.uncommitted {
            return Some(UNCOMMITTED);
        }
        self.kind.continue_label()
    }

    pub(crate) fn abort_label(&self) -> Option<&'static str> {
        match self.kind {
            Kind::Applied if self.stash.is_some() => Some(ABORT_STASH),
            Kind::Applied => self.picked.as_ref().map(|picked| if picked.squash { ABORT_SQUASH } else { ABORT_PICK }),
            kind => kind.abort_label(),
        }
    }

    pub(crate) fn headline(&self) -> String {
        let commit = || self.commit.as_ref().map_or_else(|| "a commit".to_owned(), |c| c.label.clone());
        match self.kind {
            Kind::Merge => format!("Merging {} into {}", self.target(), self.branch),
            Kind::Rebase => format!("Rebasing {} onto {}", self.branch, self.target()),
            Kind::CherryPick => format!("Cherry-picking {} onto {}", commit(), self.branch),
            Kind::Revert if self.uncommitted => format!("Reverting {} on {}, not committed", commit(), self.branch),
            Kind::Revert => format!("Reverting {} on {}", commit(), self.branch),
            Kind::Patches => format!("Applying patches to {}", self.branch),
            Kind::Applied => match (&self.stash, &self.picked) {
                (Some(stash), _) => format!("Applying stash {}", stash.title),
                (None, Some(picked)) if picked.squash => format!("Squashing {} into {}, not committed", picked.label, self.branch),
                (None, Some(picked)) => format!("Cherry-picking {} onto {}, not committed", picked.label, self.branch),
                (None, None) => "Unresolved conflicts".to_owned(),
            },
            Kind::Bisect => "A bisect is in progress".to_owned(),
        }
    }

    pub(crate) fn details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some((step, of)) = self.step {
            let unit = if self.kind == Kind::Patches { "patch" } else { "commit" };
            details.push(format!("{unit} {step} of {of}"));
        }
        match self.conflicts.len() {
            _ if self.kind == Kind::Bisect => {}
            0 if self.kind == Kind::Merge || !self.resolved.is_empty() => {
                details.push("every conflict is resolved".to_owned())
            }
            0 => {}
            1 => details.push("1 file needs resolving".to_owned()),
            n => details.push(format!("{n} files need resolving")),
        }
        details
    }

    pub(crate) fn summary(&self) -> String {
        std::iter::once(self.headline()).chain(self.details()).collect::<Vec<_>>().join(" · ")
    }

    pub(crate) fn conflicted(&self) -> Vec<&Path> {
        self.conflicts.iter().map(|c| c.path.as_path()).collect()
    }

    pub(crate) fn has_conflict(&self, path: &Path) -> bool {
        self.conflicts.iter().any(|c| c.path == path)
    }
}

fn revision(repo: &Repository, name: &str) -> Result<Option<String>> {
    let output = repo
        .read_command()
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{name}^{{commit}}"))
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim_end().to_owned();
    validate_oid(&oid)?;
    Ok(Some(oid))
}

pub(crate) fn branch(repo: &Repository) -> Result<Option<Vec<u8>>> {
    let output = repo
        .read_command()
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .run()
        .map_err(|e| e.to_string())?;
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout).to_vec())),
        Some(1) => Ok(None),
        _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

fn short_branch(full: &[u8]) -> String {
    String::from_utf8_lossy(full.strip_prefix(b"refs/heads/").unwrap_or(full)).into_owned()
}

pub(crate) fn name_of(repo: &Repository, oid: &str) -> Result<String> {
    Ok(ref_name(repo, oid)?.unwrap_or_else(|| oid[..8].to_owned()))
}

pub(crate) fn ref_name(repo: &Repository, oid: &str) -> Result<Option<String>> {
    validate_oid(oid)?;
    let names = checked(repo.read_command().args([
        "for-each-ref",
        "--format=%(refname)",
        "--points-at",
        oid,
        "refs/heads/",
        "refs/remotes/",
        "refs/tags/",
    ]))?;
    let names: Vec<&[u8]> = names.split(|&b| b == b'\n').filter(|name| !name.is_empty()).collect();
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        let found: Vec<&[u8]> = names
            .iter()
            .filter_map(|name| name.strip_prefix(prefix.as_bytes()))
            .filter(|short| !short.ends_with(b"/HEAD"))
            .collect();
        match found.as_slice() {
            [] => continue,
            [one] => return Ok(Some(String::from_utf8_lossy(one).into_owned())),
            _ => break,
        }
    }
    Ok(None)
}

fn described(repo: &Repository, oid: String) -> Result<Named> {
    validate_oid(&oid)?;
    let subject = checked(repo.read_command().args(["log", "--no-walk", "--format=%s", &oid, "--"]))?;
    let subject = String::from_utf8_lossy(&subject).trim().to_owned();
    let label = if subject.is_empty() {
        notice::short(&oid).to_owned()
    } else {
        format!("{} {subject}", notice::short(&oid))
    };
    Ok(Named { oid, label })
}

fn marked_commit(repo: &Repository, line: Option<String>) -> Result<Option<String>> {
    let Some(name) = line.filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_hexdigit())) else {
        return Ok(None);
    };
    revision(repo, &name)
}

impl Repository {
    pub(crate) fn operation(&self) -> Result<Option<Kind>> {
        detect(self.git_dir(), || {
            Ok(!checked(self.read_command().args(["ls-files", "--unmerged", "-z"]))?.is_empty())
        })
    }

    fn operation_id(&self, kind: Kind) -> Result<Id> {
        let git_dir = self.git_dir();
        Ok(Id {
            kind,
            markers: kind.markers(git_dir).iter().map(|name| read(git_dir, name)).collect::<Result<_>>()?,
            head: revision(self, "HEAD")?,
            branch: branch(self)?,
        })
    }

    pub(crate) fn expect_operation(&self, expected: &Id) -> Result<()> {
        if current_id(self)?.as_ref() != Some(expected) {
            return Err("The operation in progress changed. Look at it again before continuing.".into());
        }
        Ok(())
    }
}

pub(crate) fn current_id(repo: &Repository) -> Result<Option<Id>> {
    repo.operation()?.map(|kind| repo.operation_id(kind)).transpose()
}

fn stages(repo: &Repository, mode: &str) -> Result<Vec<u8>> {
    checked(repo.read_command().args(["ls-files", mode, "-z", "--"]))
}

pub(crate) fn stage_paths(records: &[u8]) -> Result<BTreeSet<PathBuf>> {
    records
        .split(|&c| c == 0)
        .filter(|r| !r.is_empty())
        .map(|r| {
            let tab = r.iter().position(|&b| b == b'\t').ok_or("Invalid Git index record")?;
            Ok(raw_path(&r[tab + 1..]))
        })
        .collect()
}

pub(crate) fn with_input(command: &mut std::process::Command, input: Vec<u8>) -> Result<Vec<u8>> {
    let output = crate::repository::run_with_input(command, input).map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output.stdout)
}

fn sizes(repo: &Repository, oids: &BTreeSet<String>) -> Result<BTreeMap<String, u64>> {
    if oids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut input = Vec::new();
    for oid in oids {
        validate_oid(oid)?;
        input.extend_from_slice(oid.as_bytes());
        input.push(b'\n');
    }
    let output = with_input(repo.read_command().args(["cat-file", "--batch-check=%(objectname) %(objectsize)"]), input)?;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| {
            let (oid, size) = line.split_once(' ')?;
            Some((oid.to_owned(), size.parse().ok()?))
        })
        .collect())
}

fn looks_binary(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else { return false };
    if !meta.file_type().is_file() {
        return false;
    }
    let mut start = Vec::new();
    fs::File::open(path).and_then(|file| file.take(8000).read_to_end(&mut start)).is_ok() && start.contains(&0)
}

pub(crate) fn parse_stages(records: &[u8]) -> Result<BTreeMap<PathBuf, [Option<(String, String)>; 3]>> {
    let mut paths: BTreeMap<PathBuf, [Option<(String, String)>; 3]> = BTreeMap::new();
    for record in records.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let tab = record.iter().position(|&b| b == b'\t').ok_or("Invalid Git index record")?;
        let fields: Vec<&[u8]> = record[..tab].split(|&b| b == b' ').collect();
        let [mode, oid, stage] = fields.as_slice() else {
            return Err("Invalid Git index record".into());
        };
        let stage = match *stage {
            b"1" => 0,
            b"2" => 1,
            b"3" => 2,
            _ => return Err("Invalid Git index record".into()),
        };
        if mode.is_empty() || !mode.iter().all(u8::is_ascii_digit) {
            return Err("Invalid Git index record".into());
        }
        let oid = String::from_utf8_lossy(oid).into_owned();
        validate_oid(&oid)?;
        let mode = String::from_utf8_lossy(mode).into_owned();
        paths.entry(raw_path(&record[tab + 1..])).or_default()[stage] = Some((mode, oid));
    }
    Ok(paths)
}

fn conflicts(repo: &Repository) -> Result<Vec<Conflict>> {
    let paths = parse_stages(&stages(repo, "--unmerged")?)?;
    let oids = paths.values().flatten().flatten().map(|(_, oid)| oid.clone()).collect();
    let sizes = sizes(repo, &oids)?;
    Ok(paths
        .into_iter()
        .map(|(path, stages)| {
            let has = |mode: &str| stages.iter().flatten().any(|(m, _)| m == mode);
            let shape = if has("160000") {
                Shape::Submodule
            } else if has("120000") {
                Shape::Symlink
            } else if stages[1].is_none() {
                Shape::DeletedByUs
            } else if stages[2].is_none() {
                Shape::DeletedByThem
            } else if stages.iter().flatten().any(|(_, oid)| sizes.get(oid).is_some_and(|&size| size > crate::merge::LIMIT as u64)) {
                Shape::TooLarge
            } else if looks_binary(&repo.root.join(&path)) {
                Shape::Binary
            } else if stages[0].is_none() {
                Shape::BothAdded
            } else {
                Shape::Text
            };
            let stages = stages.map(|stage| {
                stage.map(|(mode, oid)| Version { size: sizes.get(&oid).copied(), mode, oid })
            });
            Conflict { path, shape, stages }
        })
        .collect())
}

fn stopped_commit(repo: &Repository, kind: Kind) -> Result<Option<Named>> {
    let git_dir = repo.git_dir();
    let oid = match kind {
        Kind::Rebase => match revision(repo, "REBASE_HEAD")? {
            Some(oid) => Some(oid),
            None => marked_commit(repo, read_line(git_dir, "rebase-merge/stopped-sha")?)?,
        },
        Kind::CherryPick | Kind::Revert => {
            let head = if kind == Kind::CherryPick { "CHERRY_PICK_HEAD" } else { "REVERT_HEAD" };
            match marked_commit(repo, read_line(git_dir, head)?)? {
                Some(oid) => Some(oid),
                None => {
                    let next = read_line(git_dir, "sequencer/todo")?.unwrap_or_default();
                    marked_commit(repo, next.split_whitespace().nth(1).map(str::to_owned))?
                }
            }
        }
        _ => None,
    };
    oid.map(|oid| described(repo, oid)).transpose()
}

fn number(git_dir: &Path, name: &str) -> Result<Option<usize>> {
    Ok(read_line(git_dir, name)?.and_then(|line| line.parse().ok()))
}

pub(crate) fn snapshot(repo: &Repository) -> Result<Option<Snapshot>> {
    let Some(kind) = repo.operation()? else {
        return Ok(None);
    };
    let id = repo.operation_id(kind)?;
    let git_dir = repo.git_dir();
    let short_head = || id.head.as_deref().map_or_else(|| "HEAD".to_owned(), |head| head[..8].to_owned());
    let current = id.branch.as_deref().map_or_else(short_head, short_branch);
    let (branch, source, step) = match kind {
        Kind::Rebase => {
            let dir = if exists(git_dir, "rebase-merge")? { "rebase-merge" } else { "rebase-apply" };
            let head_name = read_line(git_dir, &format!("{dir}/head-name"))?.unwrap_or_default();
            let branch = match head_name.strip_prefix("refs/heads/") {
                Some(name) => name.to_owned(),
                None => match read_line(git_dir, &format!("{dir}/orig-head"))?.as_deref().and_then(|orig| orig.get(..8)) {
                    Some(short) => short.to_owned(),
                    None => current,
                },
            };
            let source = match marked_commit(repo, read_line(git_dir, &format!("{dir}/onto"))?)? {
                Some(onto) => Some(Named { label: name_of(repo, &onto)?, oid: onto }),
                None => None,
            };
            let (at, of) = if dir == "rebase-merge" { ("msgnum", "end") } else { ("next", "last") };
            let step = number(git_dir, &format!("{dir}/{at}"))?.zip(number(git_dir, &format!("{dir}/{of}"))?);
            (branch, source, step)
        }
        Kind::Patches => {
            let step = number(git_dir, "rebase-apply/next")?.zip(number(git_dir, "rebase-apply/last")?);
            (current, None, step)
        }
        Kind::Merge => {
            let incoming = read_line(git_dir, "MERGE_HEAD")?.unwrap_or_default();
            if incoming.is_empty() {
                return Err("Git recorded an empty merge head.".into());
            }
            validate_oid(&incoming)?;
            let source = Named { label: name_of(repo, &incoming)?, oid: incoming };
            (current, Some(source), None)
        }
        Kind::Bisect => {
            let start = read_line(git_dir, "BISECT_START")?.unwrap_or_default();
            (if start.is_empty() { current } else { start }, None, None)
        }
        _ => (current, None, None),
    };
    let conflicts = conflicts(repo)?;
    let conflicted: BTreeSet<&Path> = conflicts.iter().map(|c| c.path.as_path()).collect();
    let resolved = stage_paths(&stages(repo, "--resolve-undo")?)?
        .into_iter()
        .filter(|path| !conflicted.contains(path.as_path()))
        .collect();
    let pending = match kind {
        Kind::Merge => true,
        Kind::CherryPick => exists(git_dir, "CHERRY_PICK_HEAD")?,
        Kind::Revert => exists(git_dir, "REVERT_HEAD")?,
        Kind::Rebase => exists(git_dir, "rebase-merge")?,
        _ => false,
    };
    let message = match read(git_dir, "MERGE_MSG")? {
        Some(bytes) if pending => Some(strip_comments(repo, &bytes)?),
        None if pending && kind != Kind::Rebase => Some(String::new()),
        _ => None,
    };
    let empty = matches!(kind, Kind::CherryPick | Kind::Revert)
        && pending
        && conflicts.is_empty()
        && staged_nothing(repo)?;
    Ok(Some(Snapshot {
        kind,
        commit: stopped_commit(repo, kind)?,
        id,
        branch,
        source,
        step,
        conflicts,
        resolved,
        message,
        can_skip: kind.can_skip(),
        empty,
        queued: exists(git_dir, "sequencer")?,
        uncommitted: false,
        stash: None,
        picked: None,
    }))
}

fn strip_comments(repo: &Repository, message: &[u8]) -> Result<String> {
    let output = with_input(repo.read_command().args(["stripspace", "--strip-comments"]), message.to_vec())?;
    Ok(String::from_utf8_lossy(&output).trim_end().to_owned())
}

pub(crate) fn staged_nothing(repo: &Repository) -> Result<bool> {
    let status = repo
        .read_command()
        .args(["diff-index", "--cached", "--quiet", "HEAD", "--"])
        .run()
        .map(|output| output.status)
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("Git exited with {status} comparing the index with HEAD")),
    }
}

pub(crate) fn end_bisect(repo: &Repository, expected: &Id) -> Result<notice::Outcome> {
    if expected.kind != Kind::Bisect {
        return Err("No bisect is in progress.".into());
    }
    repo.expect_operation(expected)?;
    let old = crate::branch::detached_head(repo);
    checked(repo.command().args(["bisect", "reset"]))?;
    let mut outcome = crate::branch::left_on_no_branch(repo, old.as_deref(), notice::Outcome::default());
    outcome.detail = outcome.detail.trim_start().to_owned();
    Ok(outcome)
}

#[derive(Default)]
pub(crate) struct State {
    marked: Option<Kind>,
    snapshot: Option<Snapshot>,
    ticket: u64,
    pub(crate) started: Option<(Kind, String, String)>,
    pub(crate) uncommitted: Option<String>,
    pub(crate) stash: Option<Stashing>,
    pub(crate) picking: Option<Picking>,
    message_for: Option<Id>,
}

impl State {
    pub(crate) fn kind(&self) -> Option<Kind> {
        self.marked
    }

    pub(crate) fn active(&self) -> bool {
        self.marked.is_some_and(Kind::holds)
    }

    pub(crate) fn refusal(&self) -> Option<String> {
        self.marked.filter(|kind| kind.holds()).map(|kind| kind.refusal().to_owned())
    }

    pub(crate) fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub(crate) fn conflicted(&self) -> Vec<&Path> {
        self.snapshot.as_ref().map_or_else(Vec::new, Snapshot::conflicted)
    }

    pub(crate) fn resolved(&self) -> &[PathBuf] {
        self.snapshot.as_ref().map_or(&[], |s| s.resolved.as_slice())
    }

    pub(crate) fn can_finish(&self) -> bool {
        self.blocked().is_none()
    }

    pub(crate) fn blocked(&self) -> Option<String> {
        let Some(snapshot) = self.snapshot.as_ref().filter(|s| s.kind.continue_label().is_some()) else {
            return Some(match self.marked {
                Some(kind) if kind.holds() && kind.continue_label().is_none() => kind.refusal().to_owned(),
                Some(kind) if kind.holds() => "Wait for the operation to load".to_owned(),
                _ => "Nothing is in progress".to_owned(),
            });
        };
        match snapshot.conflicts.len() {
            0 if snapshot.empty => Some("This commit is now empty. Skip it, or keep it as an empty commit".to_owned()),
            0 => None,
            1 => Some("Resolve 1 file first".to_owned()),
            n => Some(format!("Resolve {n} files first")),
        }
    }

    pub(crate) fn abort_label(&self) -> Option<&'static str> {
        let kind = self.marked.filter(|kind| kind.holds())?;
        match self.snapshot.as_ref().filter(|s| s.kind == kind) {
            Some(snapshot) => snapshot.abort_label(),
            None => kind.abort_label(),
        }
    }

    pub(crate) fn continue_label(&self) -> Option<&'static str> {
        let kind = self.marked.filter(|kind| kind.holds())?;
        match self.snapshot.as_ref().filter(|s| s.kind == kind) {
            Some(snapshot) => snapshot.continue_label(),
            None => kind.continue_label(),
        }
    }

    pub(crate) fn has_message(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.message.is_some())
    }

    pub(crate) fn summary(&self) -> Option<String> {
        self.snapshot.as_ref().map(Snapshot::summary)
    }

    pub(crate) fn mark(&mut self, kind: Option<Kind>) -> bool {
        self.marked = kind;
        let applied = |kind: Option<Kind>| kind == Some(Kind::Applied);
        if !applied(kind) && !applied(self.snapshot.as_ref().map(|s| s.kind)) {
            self.stash = None;
            self.picking = None;
        }
        self.snapshot.as_ref().map(|s| s.kind) != kind
    }

    pub(crate) fn update(&mut self, mut snapshot: Option<Snapshot>) -> bool {
        match (&mut snapshot, &self.started) {
            (Some(snapshot), Some((kind, oid, label)))
                if snapshot.kind == *kind && snapshot.source.as_ref().is_some_and(|s| s.oid == *oid) =>
            {
                if let Some(source) = snapshot.source.as_mut() {
                    source.label = label.clone();
                }
            }
            (None, _) => self.started = None,
            _ => {}
        }
        match &mut snapshot {
            Some(snapshot) if snapshot.kind == Kind::Revert => {
                let reverted = snapshot.commit.as_ref().map(|c| &c.oid);
                snapshot.uncommitted = self.uncommitted.is_some() && reverted == self.uncommitted.as_ref();
            }
            Some(_) => {}
            None => self.uncommitted = None,
        }
        match &mut snapshot {
            Some(snapshot) if snapshot.kind == Kind::Applied => {
                snapshot.stash = self.stash.clone();
                snapshot.picked = self.picking.clone();
            }
            _ => {
                self.stash = None;
                self.picking = None;
            }
        }
        let changed = self.snapshot.as_ref().map(|s| &s.id) != snapshot.as_ref().map(|s| &s.id);
        self.marked = snapshot.as_ref().map(|s| s.kind);
        self.snapshot = snapshot;
        changed
    }
}

pub(crate) fn set(ui: &mut Ui, snapshot: Option<Snapshot>) {
    let changed = ui.operation.update(snapshot);
    ui.merge.follow(changed, ui.operation.snapshot());
    ui.conflict.follow(ui.operation.snapshot());
}

pub(crate) fn prefill(ui: &mut Ui, cx: &mut gpui::App) {
    let state = &mut ui.operation;
    let shown = state.snapshot.as_ref().and_then(|s| s.message.as_ref().map(|message| (&s.id, message)));
    let Some((id, message)) = shown else {
        state.message_for = None;
        return;
    };
    if state.message_for.as_ref() == Some(id) {
        return;
    }
    state.message_for = Some(id.clone());
    let message = message.clone();
    ui.operation_message.update(cx, |field, cx| field.set_text(&message, cx));
}

pub(crate) fn owns_panel(ui: &Ui) -> bool {
    ui.operation.active() && !matches!(ui.selection, Some(view::Selection::Commit(_) | view::Selection::Stash(_)))
}

pub(crate) fn shows_message(ui: &Ui) -> bool {
    owns_panel(ui) && ui.operation.snapshot().is_some_and(|s| s.message.is_some() && !s.empty)
}

pub(crate) fn reminder(ui: &Ui) -> Option<String> {
    if !ui.operation.active() || owns_panel(ui) {
        return None;
    }
    let snapshot = ui.operation.snapshot()?;
    Some(match snapshot.conflicts.len() {
        0 => snapshot.headline(),
        1 => "1 conflict in the working tree".to_owned(),
        n => format!("{n} conflicts in the working tree"),
    })
}

pub(crate) fn view_conflicts() -> view::Action {
    view::Action::Select(view::Selection::Changes)
}

pub(crate) fn row_text(ui: &Ui) -> Option<(String, String)> {
    if !ui.operation.active() {
        return None;
    }
    let Some(snapshot) = ui.operation.snapshot() else {
        let kind = ui.operation.kind()?;
        return Some((format!("Loading the {}…", kind.noun()), String::new()));
    };
    let mut details = snapshot.details();
    if !snapshot.resolved.is_empty() && !snapshot.conflicts.is_empty() {
        details.push(format!("{} resolved", snapshot.resolved.len()));
    }
    Some((snapshot.headline(), details.join(" · ")))
}

pub(crate) fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.operation.ticket += 1;
    let ticket = tab.ui.operation.ticket;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || -> Result<(Option<Snapshot>, bool, bool)> {
            let now = snapshot(&repo)?;
            let resolved = now.is_none() && !stages(&repo, "--resolve-undo")?.is_empty();
            let staged = resolved && !staged_nothing(&repo)?;
            Ok((now, resolved, staged))
        },
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.operation.ticket != ticket || tab.ui.busy() {
                return;
            }
            match result {
                Ok((snapshot, resolved, staged)) => {
                    let kind = |s: Option<&Snapshot>| s.map(|s| s.kind).filter(|kind| kind.holds());
                    let began = kind(snapshot.as_ref()).is_some() && kind(tab.ui.operation.snapshot()) != kind(snapshot.as_ref());
                    let ended = match tab.ui.operation.snapshot() {
                        Some(shown) if shown.kind == Kind::Applied && snapshot.is_none() && resolved => {
                            Some((shown.stash.clone(), shown.picked.clone(), shown.branch.clone()))
                        }
                        _ => None,
                    };
                    set(&mut tab.ui, snapshot);
                    match ended {
                        Some((Some(stash), _, _)) => {
                            tab.ui.notices.show(stash_resolved(&stash));
                        }
                        Some((None, Some(picked), branch)) if picked.squash => {
                            tab.ui.notices.show(squash_resolved(&picked, &branch, staged));
                            if staged {
                                draft_message(&tab.ui, &picked.message, cx);
                            }
                        }
                        Some((None, Some(picked), branch)) => {
                            tab.ui.notices.show(pick_resolved(&picked, &branch, staged));
                            if staged {
                                draft_message(&tab.ui, &picked.message, cx);
                            }
                        }
                        _ => {}
                    }
                    if began && !tab.ui.working() {
                        tab.ui.diff_open = false;
                        tab.select(view::Selection::Changes, cx);
                    }
                }
                Err(err) => tab.error(err, cx),
            }
            cx.notify();
        },
    );
}

pub(crate) fn stash_resolved(stash: &Stashing) -> Notice {
    if stash.pop {
        Notice::new(
            Tone::Success,
            format!("Stash {} applied", stash.title),
            "Every conflict is resolved. The stash was kept because it did not apply cleanly.",
        )
        .offer("Drop the stash now", view::Action::Stash(crate::stash::Action::DropNow(stash.oid.clone())))
        .sticky()
    } else {
        Notice::new(
            Tone::Success,
            format!("Stash {} applied", stash.title),
            "Every conflict is resolved. The stash is still in the list.",
        )
    }
}

pub(crate) fn pick_resolved(picked: &Picking, branch: &str, staged: bool) -> Notice {
    if !staged {
        return Notice::new(
            Tone::Success,
            format!("{branch} already has these changes"),
            format!("Every conflict is resolved, and picking {} changed nothing on {branch}, so nothing is staged.", picked.label),
        );
    }
    Notice::new(
        Tone::Success,
        "Cherry-pick staged",
        format!("Every conflict is resolved. The changes from {} are staged on {branch}. Commit them when you are ready.", picked.label),
    )
}

pub(crate) fn squash_resolved(squashed: &Picking, branch: &str, staged: bool) -> Notice {
    if !staged {
        return Notice::new(
            Tone::Success,
            format!("{branch} already has these changes"),
            format!("Every conflict is resolved, and squashing {} changed nothing on {branch}, so nothing is staged.", squashed.label),
        );
    }
    Notice::new(
        Tone::Success,
        "Squash merge staged",
        format!(
            "Every conflict is resolved. The changes from {} are staged on {branch} as one change. Commit them when you are ready.",
            squashed.label
        ),
    )
}

pub(crate) fn allows(kind: Option<Kind>, action: &view::Action) -> std::result::Result<(), &'static str> {
    use view::Action as A;
    let Some(kind) = kind.filter(|kind| kind.holds()) else {
        return Ok(());
    };
    let allowed = match action {
        A::Open | A::Refresh | A::Operation(_) | A::Merge(_) | A::Conflict(_) | A::Prompt(_) => true,
        A::Select(_) | A::Inspect(_) | A::Search(_) | A::More | A::OpenDiff | A::CloseDiff | A::DiffMode(_) => true,
        A::Dismiss
        | A::Notice(_)
        | A::CopyText { .. }
        | A::Menu(_)
        | A::Handoff(_)
        | A::Jobs(_)
        | A::Activity(_)
        | A::Explain(_)
        | A::Credentials(_)
        | A::Connect(_)
        | A::Clone(_)
        | A::Init(_)
        | A::Start(_)
        | A::Preferences(_)
        | A::Identity(_)
        | A::AutoFetch(_)
        | A::Tools(_)
        | A::Tabs(_)
        | A::Cli(_)
        | A::Help(_)
        | A::Palette(_)
        | A::ToggleSidebar
        | A::TogglePanel
        | A::Run(_)
        | A::Activate(_)
        | A::MenuKey(_)
        | A::MenuHover(_)
        | A::Navigate(_) => true,
        A::Refs(action) => {
            use crate::refs::Action as R;
            matches!(action, R::Inspect(..) | R::ToggleSection(_) | R::ToggleFolder(_) | R::ToggleList | R::Escape)
        }
        A::Branch(action) => matches!(action, crate::branch::Action::Restore { .. }),
        A::Tag(action) => matches!(action, crate::tag::Action::Restore { .. }),
        A::Tracking(action) => matches!(action, crate::tracking::Action::RestoreRemote { .. }),
        A::Stash(action) => matches!(action, crate::stash::Action::Restore { .. } | crate::stash::Action::Dismiss),
        A::Sync(action) => {
            use crate::sync::Action as S;
            matches!(action, S::RestoreForced { .. } | S::Dismiss | S::TogglePrune | S::SetDefault(_))
        }
        A::Publish(action) => {
            use crate::publish::Action as P;
            matches!(
                action,
                P::Escape
                    | P::Connect
                    | P::Disconnect
                    | P::CopyCode
                    | P::OpenVerification
                    | P::RefreshOwners
                    | P::Install
                    | P::EditConfig
                    | P::SaveConfig
                    | P::RegisterApp
                    | P::UseBuiltInApp
            )
        }
        A::Autostash(action) => matches!(action, crate::autostash::Action::Toggle),
        A::Picked(action) => action.picks(),
        A::ViewChanges
        | A::StageAll
        | A::UnstageAll
        | A::Reverse(_)
        | A::Pick(_)
        | A::Rebase(_)
        | A::StageHunk(_)
        | A::UnstageHunk(_)
        | A::StageFile
        | A::StagePath(_)
        | A::UnstagePath(_)
        | A::Commit
        | A::CommitAll
        | A::FocusSummary
        | A::ToggleAmend
        | A::AskDiscard(_)
        | A::Discard(_)
        | A::Ignore(_)
        | A::Address(_) => false,
    };
    if allowed { Ok(()) } else { Err(kind.refusal()) }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Continue,
    Skip,
    KeepEmpty,
    Abort,
    EndBisect,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Abort(Snapshot),
    EndBisect { id: Id, back_to: String },
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> prompt::Spec {
    match ask {
        Ask::Abort(shown) => abort_spec(shown),
        Ask::EndBisect { back_to, .. } => prompt::Spec::new("End the bisect?", "End bisect")
            .line(format!("HEAD goes back to {back_to}, where the bisect started, as git bisect reset does.")),
    }
}

fn abort_spec(shown: &Snapshot) -> prompt::Spec {
    if let (Kind::Applied, Some(stash)) = (shown.kind, &shown.stash) {
        let spec = prompt::Spec::new(format!("Abort applying stash {}?", stash.title), ABORT_STASH)
            .line("The files the stash changed go back to how they were before it was applied, including those you have already resolved. Your other edits are kept.")
            .line("Untracked files it restored are removed, unless you have changed them since.")
            .line("The stash stays in the list, so you can apply it again later.");
        return match shown.resolved.len() {
            0 => spec,
            1 => spec.line("The resolution of 1 file will be lost."),
            n => spec.line(format!("The resolutions of {n} files will be lost.")),
        }
        .danger();
    }
    if let (Kind::Applied, Some(picked)) = (shown.kind, &shown.picked) {
        let spec = if picked.squash {
            prompt::Spec::new(format!("Abort the squash merge of {} into {}?", picked.label, shown.branch), ABORT_SQUASH)
                .line("The files it changed go back to how they were before the squash merge, including those you have already resolved. Your other edits are kept.")
        } else {
            prompt::Spec::new(format!("Abort the cherry-pick of {} onto {}?", picked.label, shown.branch), ABORT_PICK)
                .line("The files it changed go back to how they were before the cherry-pick, including those you have already resolved. Your other edits are kept.")
        }
            .line(format!("{} stays where it is; nothing was committed.", shown.branch));
        return match shown.resolved.len() {
            0 => spec,
            1 => spec.line("The resolution of 1 file will be lost."),
            n => spec.line(format!("The resolutions of {n} files will be lost.")),
        }
        .danger();
    }
    let noun = shown.kind.noun();
    let title = match shown.kind {
        Kind::Merge => format!("Abort the merge of {} into {}?", shown.theirs(), shown.branch),
        Kind::Rebase => format!("Abort the rebase of {} onto {}?", shown.branch, shown.target()),
        Kind::CherryPick | Kind::Revert => {
            let commit = shown.commit.as_ref().map_or_else(|| "a commit".to_owned(), |c| c.label.clone());
            let on = if shown.kind == Kind::CherryPick { "onto" } else { "on" };
            format!("Abort the {noun} of {commit} {on} {}?", shown.branch)
        }
        _ => format!("Abort the {noun} on {}?", shown.branch),
    };
    let mut spec = prompt::Spec::new(title, shown.abort_label().unwrap_or("Abort"))
        .line(format!("Files you have already resolved go back to how they were before the {noun}."));
    match shown.kind {
        Kind::Rebase => {
            spec = spec.line(format!(
                "{} goes back to where it was before the rebase, and the commits replayed so far are dropped.",
                shown.branch
            ))
        }
        Kind::CherryPick | Kind::Revert | Kind::Patches if shown.queued || shown.step.is_some() => {
            spec = spec.line(format!("{} goes back to where it was before the {noun} started.", shown.branch))
        }
        _ => {}
    }
    match shown.resolved.len() {
        0 => spec,
        1 => spec.line("The resolution of 1 file will be lost."),
        n => spec.line(format!("The resolutions of {n} files will be lost.")),
    }
    .danger()
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, _: prompt::Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Abort(shown) => {
            let (kept, stopped) = kept();
            tab.run_operation_or(
                cx,
                if shown.kind == Kind::Applied && shown.stash.is_none() && shown.picked.as_ref().is_some_and(|p| p.squash) {
                    "Aborting the squash merge…"
                } else if shown.kind == Kind::Applied && shown.stash.is_none() && shown.picked.is_some() {
                    Kind::CherryPick.aborting()
                } else {
                    shown.kind.aborting()
                },
                "Aborted",
                true,
                move |repo| put_back(&repo, &kept, |repo| backend::abort(repo, &shown)),
                |tab, _| set(&mut tab.ui, None),
                move |tab, _| stopped.remember(&mut tab.ui),
            )
        }
        Ask::EndBisect { id, .. } => tab.run_operation_then(
            cx,
            "Ending the bisect…",
            "Bisect ended",
            true,
            move |repo| end_bisect(&repo, &id),
            |tab, _| set(&mut tab.ui, None),
        ),
    }
}

fn kept() -> (crate::autostash::Kept, crate::autostash::Kept) {
    let kept = crate::autostash::Kept::default();
    (kept.clone(), kept)
}

fn put_back(
    repo: &Repository,
    kept: &crate::autostash::Kept,
    step: impl FnOnce(&Repository) -> std::result::Result<crate::notice::Outcome, crate::notice::Failure>,
) -> std::result::Result<crate::notice::Outcome, crate::notice::Failure> {
    let pending = crate::autostash::pending(repo);
    let outcome = step(repo)?;
    crate::autostash::settled(repo, pending, kept, outcome)
}

pub(crate) fn draft_message(ui: &Ui, message: &str, cx: &mut gpui::App) {
    if message.trim().is_empty() || !ui.draft(cx).message().is_empty() {
        return;
    }
    ui.set_draft(&view::CommitDraft::from_message(message), cx);
}

fn ends(shown: &Snapshot) -> bool {
    match shown.kind {
        Kind::Merge => true,
        Kind::Revert if shown.uncommitted => false,
        Kind::CherryPick | Kind::Revert => !shown.queued,
        _ => false,
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() {
        return;
    }
    let shown = tab.ui.operation.snapshot().filter(|s| s.kind.holds() || matches!(action, Action::EndBisect)).cloned();
    let Some(shown) = shown else {
        let what = if matches!(action, Action::EndBisect) { "No bisect is in progress" } else { "Nothing is in progress" };
        tab.ui.notices.show(Notice::new(Tone::Info, what, ""));
        cx.notify();
        return;
    };
    let refuse = |tab: &mut Tab, reason: String, cx: &mut Context<Tab>| {
        tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
        cx.notify();
    };
    let message = shown.message.is_some().then(|| tab.ui.operation_message.read(cx).text().to_owned());
    match action {
        Action::Continue => {
            if let Some(reason) = tab.ui.operation.blocked() {
                return refuse(tab, reason, cx);
            }
            if !shown.uncommitted && message.as_deref().is_some_and(|m| m.trim().is_empty()) {
                return refuse(tab, "Write a message for the commit first".to_owned(), cx);
            }
            let select = ends(&shown);
            let staged = shown.uncommitted.then(|| message.clone().unwrap_or_default());
            let (kept, stopped) = kept();
            tab.run_operation_or(
                cx,
                shown.kind.continuing(),
                "Done",
                select,
                move |repo| put_back(&repo, &kept, |repo| backend::finish(repo, &shown, message)),
                move |tab, cx| {
                    set(&mut tab.ui, None);
                    if let Some(message) = staged {
                        draft_message(&tab.ui, &message, cx);
                        tab.select(view::Selection::Changes, cx);
                    }
                },
                move |tab, _| stopped.remember(&mut tab.ui),
            );
        }
        Action::Skip => {
            if !shown.can_skip {
                return refuse(tab, format!("A {} has no commit to skip", shown.kind.noun()), cx);
            }
            let select = ends(&shown);
            let (kept, stopped) = kept();
            tab.run_operation_or(
                cx,
                "Skipping the commit…",
                "Skipped",
                select,
                move |repo| put_back(&repo, &kept, |repo| backend::skip(repo, &shown)),
                |tab, _| set(&mut tab.ui, None),
                move |tab, _| stopped.remember(&mut tab.ui),
            );
        }
        Action::KeepEmpty => {
            if shown.uncommitted {
                return refuse(tab, "This revert was started without committing. Skip it or abort it".to_owned(), cx);
            }
            if !shown.empty || message.as_deref().is_none_or(|m| m.trim().is_empty()) {
                return refuse(tab, "Only a pick or a revert that came out empty can be kept empty".to_owned(), cx);
            }
            let select = ends(&shown);
            tab.run_operation_then(
                cx,
                "Committing the empty change…",
                "Kept",
                select,
                move |repo| backend::keep_empty(&repo, &shown, message),
                |tab, _| set(&mut tab.ui, None),
            );
        }
        Action::Abort => {
            if shown.abort_label().is_none() {
                return refuse(tab, shown.kind.refusal().to_owned(), cx);
            }
            prompt::ask(&mut tab.ui, prompt::Kind::Operation(Ask::Abort(shown)), cx);
        }
        Action::EndBisect => {
            if shown.kind != Kind::Bisect {
                return refuse(tab, "No bisect is in progress".to_owned(), cx);
            }
            let ask = Ask::EndBisect { id: shown.id.clone(), back_to: shown.branch.clone() };
            prompt::ask(&mut tab.ui, prompt::Kind::Operation(ask), cx);
        }
    }
    cx.notify();
}

pub(crate) fn banner<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(snapshot) = ui.operation.snapshot() else {
        return div();
    };
    let action = match snapshot.kind {
        Kind::Bisect => Some(("end-bisect", "End bisect", view::Action::Operation(Action::EndBisect), false)),
        _ => snapshot.abort_label().map(|label| ("abort-operation", label, view::Action::Operation(Action::Abort), true)),
    };
    let details = snapshot.details();
    div()
        .h(px(40.))
        .flex_shrink_0()
        .pl(px(14.))
        .pr(px(10.))
        .flex()
        .items_center()
        .gap_2()
        .bg(soft(CONFLICT, 0x18))
        .border_b_1()
        .border_color(soft(CONFLICT, 0x48))
        .text_size(px(12.5))
        .child(kit::icon("alert").text_color(rgb(CONFLICT)))
        .child(kit::fit(snapshot.headline()).size(12.5).weight(FontWeight::SEMIBOLD))
        .child(
            kit::fit(details.iter().map(|detail| format!("· {detail}")).collect::<Vec<_>>().join(" "))
                .size(12.5)
                .color(TEXT_2)
                .grow(),
        )
        .when_some(action, |d, (id, label, action, danger)| {
            let button = kit::btn(id, label).small().enabled(!ui.busy());
            d.child(if danger { button.danger() } else { button }.on(action, cx))
        })
}

pub(crate) fn reminder_strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let text = reminder(app.ui())?;
    Some(
        div()
            .h(px(36.))
            .flex_shrink_0()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(soft(CONFLICT, 0x18))
            .border_b_1()
            .border_color(soft(CONFLICT, 0x48))
            .child(kit::icon("alert").size(px(13.)).text_color(rgb(CONFLICT)))
            .child(kit::fit(text).size(TEXT_SMALL).grow())
            .child(kit::btn("view-conflicts", "View conflicts").small().on(view_conflicts(), cx)),
    )
}

fn mark_all_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> Option<gpui::Stateful<Div>> {
    let entry = crate::commands::find("conflict.mark-all-resolved")
        .and_then(|command| crate::commands::entry(command, ui, &crate::commands::Target::Repo, cx))?;
    let action = entry.action?;
    Some(
        kit::btn("mark-all-resolved", entry.title)
            .small()
            .ghost()
            .tip(crate::commands::tip_with_keys("Stage every conflicted file as it is now", entry.id))
            .on(action, cx),
    )
}

pub(crate) fn side_panel<T: Desk>(app: &T, width: gpui::Pixels, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.operation;
    let ready = !ui.busy();
    let snapshot = state.snapshot();
    let kind = state.kind();
    let heading = |title: &'static str, count: usize| {
        div()
            .h(px(FILE_ROW))
            .flex_shrink_0()
            .px_3()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(kit::label(title))
            .child(kit::label(count.to_string()).font_weight(FontWeight::MEDIUM))
    };
    let conflicted = state.conflicted();
    let mut list = div()
        .id("merge-files")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .pt_1()
        .child(heading("Conflicts", conflicted.len()).when_some(mark_all_button(ui, cx), |row, button| {
            row.child(kit::grow()).child(button)
        }));
    for (i, conflict) in snapshot.map_or(&[][..], |s| s.conflicts.as_slice()).iter().enumerate() {
        list = list.child(crate::conflict::conflict_row(app, i, conflict, cx));
    }
    list = list.child(heading("Resolved", state.resolved().len()).mt_2());
    for (i, path) in state.resolved().iter().enumerate() {
        list = list.child(crate::conflict::resolved_row(app, i, path, cx));
    }
    let footer = div()
        .p_3()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .border_t_1()
        .border_color(rgb(LINE));
    let note = |text: String, token: u32| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.5))
            .text_color(rgb(token))
            .child(kit::icon("alert").size(px(12.)).text_color(rgb(token)))
            .child(text)
    };
    let abort = state.abort_label().map(|label| {
        kit::btn("abort-operation-panel", label)
            .danger()
            .wide()
            .enabled(ready)
            .on(view::Action::Operation(Action::Abort), cx)
    });
    let footer = match (kind, snapshot) {
        (Some(Kind::Applied), snapshot) => {
            let text = match (snapshot.and_then(|s| s.stash.as_ref()), snapshot.and_then(|s| s.picked.as_ref())) {
                (Some(stash), _) if stash.pop => {
                    "Resolve each file. The stash was kept because it did not apply cleanly; once the last file is resolved you can drop it."
                }
                (Some(_), _) => "Resolve each file. The conflicts end when the last one is resolved, and the stash stays in the list.",
                (None, Some(picked)) if picked.squash => {
                    "Not committed: resolve each file, and the squashed changes stay staged, with a message for them in the commit box."
                }
                (None, Some(_)) => {
                    "Not committed: resolve each file, and the picked changes stay staged, with the commit's message in the commit box."
                }
                (None, None) => "Resolve each file. The conflicts end when the last one is resolved.",
            };
            footer
                .child(div().text_size(px(TEXT_SMALL)).text_color(rgb(TEXT_2)).child(text))
                .children(abort)
        }
        (Some(_), Some(snapshot)) if snapshot.empty => {
            let commit = snapshot.commit.as_ref().map_or("This commit", |c| c.label.as_str());
            footer
                .child(note(
                    format!("{commit} is now empty, possibly because of how its conflicts were resolved."),
                    CONFLICT,
                ))
                .child(
                    kit::btn("skip-commit", "Skip commit")
                        .primary()
                        .wide()
                        .enabled(ready)
                        .on(view::Action::Operation(Action::Skip), cx),
                )
                .when(snapshot.message.is_some() && !snapshot.uncommitted, |d| {
                    d.child(
                        kit::btn("keep-empty", "Keep as an empty commit")
                            .wide()
                            .enabled(ready)
                            .on(view::Action::Operation(Action::KeepEmpty), cx),
                    )
                })
                .children(abort)
        }
        (Some(kind), snapshot) => {
            let left = conflicted.len();
            let label = state.continue_label().unwrap_or("Continue");
            let blocked = state.blocked();
            let can_skip = snapshot.is_some_and(|s| s.can_skip);
            let uncommitted = snapshot.is_some_and(|s| s.uncommitted);
            footer
                .when(uncommitted, |d| {
                    d.child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(rgb(TEXT_2))
                            .child("Not committed: finishing leaves the result staged, with this message in the commit box."),
                    )
                })
                .when(state.has_message(), |d| {
                    d.child(kit::label("Commit message")).child(ui.operation_message.clone())
                })
                .when(left > 0, |d| {
                    let files = if left == 1 { "1 file".to_owned() } else { format!("{left} files") };
                    let verb = if kind == Kind::Merge || kind == Kind::Revert { "finish" } else { "continue" };
                    d.child(note(format!("Resolve {files} to {verb} the {}", kind.noun()), CONFLICT))
                })
                .child({
                    let button = kit::btn("continue-operation", label)
                        .primary()
                        .wide()
                        .hint(if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl ↩" })
                        .enabled(ready && blocked.is_none());
                    match blocked {
                        Some(reason) => button.tip(reason),
                        None => button,
                    }
                    .on(view::Action::Operation(Action::Continue), cx)
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .when(can_skip, |d| {
                            d.child(
                                div().flex_1().child(
                                    kit::btn("skip-commit", "Skip commit")
                                        .wide()
                                        .enabled(ready)
                                        .on(view::Action::Operation(Action::Skip), cx),
                                ),
                            )
                        })
                        .when_some(abort, |d, abort| d.child(div().flex_1().child(abort))),
                )
        }
        (None, _) => footer,
    };
    div()
        .w(width)
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(rgb(PANEL))
        .child(list)
        .child(footer)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    pub(crate) fn oid(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim_end().to_owned()
    }

    pub(crate) fn write(f: &Fixture, path: &str, text: &str) {
        fs::write(f.dir.join(path), text).unwrap();
    }

    pub(crate) fn commit(f: &Fixture, path: &str, text: &str, subject: &str) -> String {
        write(f, path, text);
        f.git(&["add", "--", path]);
        f.git(&["commit", "-qm", subject]);
        oid(f, "HEAD")
    }

    pub(crate) fn stops(f: &Fixture, args: &[&str]) {
        let output = f.repo.command().env("GIT_EDITOR", "true").args(args).output().unwrap();
        assert!(!output.status.success(), "git {args:?} did not stop");
    }

    pub(crate) fn diverged() -> Fixture {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "map.txt", "creek\n", "Map the creek");
        commit(&f, "trail.txt", "feature side\n", "Climb the ridge");
        commit(&f, "map.txt", "creek and ridge\n", "Mark the ridge");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.txt", "main side\n", "Bridge the river");
        f
    }

    fn text(path: &str) -> (PathBuf, Shape) {
        (path.into(), Shape::Text)
    }

    #[test]
    fn a_rebase_started_in_a_terminal_is_named_with_its_step_and_both_sides() {
        let f = diverged();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        let ridge = oid(&f, "feature~1");
        stops(&f, &["rebase", "main"]);
        assert_eq!(f.repo.operation().unwrap(), Some(Kind::Rebase));
        assert_eq!(f.repo.status().unwrap().operation, Some(Kind::Rebase));
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(s.kind, Kind::Rebase);
        assert_eq!(s.branch, "feature");
        assert_eq!(s.source, Some(Named { oid: main, label: "main".into() }));
        assert_eq!(s.commit, Some(Named { label: format!("{} Climb the ridge", &ridge[..7]), oid: ridge.clone() }));
        assert_eq!(s.step, Some((2, 3)));
        assert_eq!(s.conflicts.iter().map(|c| (c.path.clone(), c.shape)).collect::<Vec<_>>(), [text("trail.txt")]);
        assert!(s.resolved.is_empty());
        assert!(s.can_skip);
        assert_eq!(s.ours(), "main, with the commits replayed so far");
        assert_eq!(s.theirs(), format!("your commit {} Climb the ridge", &ridge[..7]));
        assert_eq!(s.summary(), "Rebasing feature onto main · commit 2 of 3 · 1 file needs resolving");

        write(&f, "trail.txt", "both sides\n");
        f.git(&["add", "trail.txt"]);
        let staged = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((staged.conflicts.len(), staged.resolved.as_slice()), (0, [PathBuf::from("trail.txt")].as_slice()));
        assert_eq!(staged.summary(), "Rebasing feature onto main · commit 2 of 3 · every conflict is resolved");
        f.git(&["rebase", "--abort"]);
        assert_eq!(snapshot(&f.repo).unwrap(), None);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
    }

    #[test]
    fn the_older_rebase_backend_and_a_patch_series_are_told_apart() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "--apply", "main"]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str(), s.step), (Kind::Rebase, "feature", Some((2, 3))));
        assert_eq!(s.source.as_ref().map(|s| s.label.as_str()), Some("main"));
        f.git(&["rebase", "--abort"]);

        let patches = f.dir.join("patches");
        f.git(&["format-patch", "-q", "-3", "-o", patches.to_str().unwrap(), "feature"]);
        f.git(&["switch", "-q", "main"]);
        let mut files: Vec<_> = fs::read_dir(&patches).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        let mut am = vec!["am".to_owned(), "-3".to_owned()];
        am.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
        stops(&f, &am.iter().map(String::as_str).collect::<Vec<_>>());
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str(), s.step), (Kind::Patches, "main", Some((2, 3))));
        assert_eq!(s.summary(), "Applying patches to main · patch 2 of 3 · 1 file needs resolving");
        assert_eq!(f.repo.expect_no_operation().unwrap_err(), "Finish or abort the patch series being applied first.");
        f.git(&["am", "--abort"]);
        assert_eq!(f.repo.operation().unwrap(), None);
    }

    #[test]
    fn a_cherry_pick_and_a_sequence_between_its_stops_are_recognised() {
        let f = diverged();
        let ridge = oid(&f, "feature~1");
        stops(&f, &["cherry-pick", &ridge]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(s.kind, Kind::CherryPick);
        assert_eq!(s.branch, "main");
        assert_eq!(s.commit.as_ref().map(|c| c.oid.as_str()), Some(ridge.as_str()));
        assert_eq!(s.ours(), "main");
        assert_eq!(s.theirs(), format!("picked commit {} Climb the ridge", &ridge[..7]));
        assert_eq!(s.summary(), format!("Cherry-picking {} Climb the ridge onto main · 1 file needs resolving", &ridge[..7]));
        assert!(s.message.as_deref().is_some_and(|m| m.starts_with("Climb the ridge")));
        f.git(&["cherry-pick", "--abort"]);

        stops(&f, &["cherry-pick", &ridge, "feature"]);
        write(&f, "trail.txt", "both\n");
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-q", "--no-edit"]);
        assert!(!f.repo.git_dir().join("CHERRY_PICK_HEAD").exists());
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(s.kind, Kind::CherryPick);
        assert_eq!(f.repo.expect_no_operation().unwrap_err(), "Finish or abort the cherry-pick first.");
        f.git(&["cherry-pick", "--quit"]);
        assert_eq!(f.repo.operation().unwrap(), None);
    }

    #[test]
    fn a_revert_is_named_by_what_it_undoes() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        let second = commit(&f, "trail.txt", "second\n", "Second");
        commit(&f, "trail.txt", "third\n", "Third");
        stops(&f, &["revert", "--no-edit", &second]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str()), (Kind::Revert, "main"));
        assert_eq!(s.theirs(), format!("undoing {} Second", &second[..7]));
        assert_eq!(s.headline(), format!("Reverting {} Second on main", &second[..7]));
        assert_eq!(f.repo.status().unwrap().operation, Some(Kind::Revert));
        f.git(&["revert", "--abort"]);
        assert_eq!(snapshot(&f.repo).unwrap(), None);
    }

    #[test]
    fn a_merge_is_named_by_the_branch_it_brings_in() {
        let f = diverged();
        stops(&f, &["merge", "feature"]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str(), s.step, s.can_skip), (Kind::Merge, "main", None, false));
        assert_eq!(s.source, Some(Named { oid: oid(&f, "feature"), label: "feature".into() }));
        assert_eq!((s.ours(), s.theirs()), ("main".to_owned(), "feature".to_owned()));
        assert_eq!(s.summary(), "Merging feature into main · 1 file needs resolving");
        f.git(&["merge", "--abort"]);
    }

    #[test]
    fn a_conflicted_stash_apply_leaves_no_marker_and_is_still_recognised() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        write(&f, "trail.txt", "stashed\n");
        f.git(&["stash", "push", "-q"]);
        commit(&f, "trail.txt", "moved on\n", "Move on");
        stops(&f, &["stash", "apply"]);
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(s.kind, Kind::Applied);
        assert_eq!((s.ours(), s.theirs()), ("your files".to_owned(), "applied change".to_owned()));
        assert_eq!(s.summary(), "Unresolved conflicts · 1 file needs resolving");
        assert_eq!(f.repo.ensure_index_writable().unwrap_err(), "Resolve the conflicted files first.");
        f.git(&["add", "trail.txt"]);
        assert_eq!(snapshot(&f.repo).unwrap(), None);
    }

    #[test]
    fn a_bisect_is_shown_holds_nothing_and_ends_only_as_shown() {
        let f = Fixture::new();
        let base = commit(&f, "trail.txt", "1\n", "One");
        commit(&f, "trail.txt", "2\n", "Two");
        commit(&f, "trail.txt", "3\n", "Three");
        f.git(&["bisect", "start", "HEAD", &base]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str()), (Kind::Bisect, "main"));
        assert_eq!(s.summary(), "A bisect is in progress");
        f.repo.expect_no_operation().unwrap();
        f.repo.ensure_index_writable().unwrap();
        assert_eq!(allows(Some(Kind::Bisect), &view::Action::StageAll), Ok(()));
        f.git(&["bisect", "good"]);
        let err = end_bisect(&f.repo, &s.id).unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert!(f.repo.git_dir().join("BISECT_LOG").exists());
        let now = snapshot(&f.repo).unwrap().unwrap();
        end_bisect(&f.repo, &now.id).unwrap();
        assert!(!f.repo.git_dir().join("BISECT_LOG").exists());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(snapshot(&f.repo).unwrap(), None);
        assert!(end_bisect(&f.repo, &now.id).is_err());
    }

    #[test]
    fn a_rebase_of_no_branch_with_an_unreadable_orig_head_is_named_by_head() {
        let f = Fixture::new();
        let head = commit(&f, "trail.txt", "base\n", "Base");
        let dir = f.repo.git_dir().join("rebase-merge");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("head-name"), "detached HEAD\n").unwrap();
        fs::write(dir.join("orig-head"), "aéééé\n").unwrap();
        let s = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((s.kind, s.branch.as_str()), (Kind::Rebase, "main"));
        fs::write(dir.join("orig-head"), format!("{head}\n")).unwrap();
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().branch, &head[..8]);
    }

    #[test]
    fn ending_a_bisect_says_what_it_left_on_no_branch() {
        let f = Fixture::new();
        let base = commit(&f, "trail.txt", "1\n", "One");
        commit(&f, "trail.txt", "2\n", "Two");
        commit(&f, "trail.txt", "3\n", "Three");
        f.git(&["bisect", "start", "HEAD", &base]);
        let made = commit(&f, "trail.txt", "fix\n", "Try a fix");
        let s = snapshot(&f.repo).unwrap().unwrap();
        let outcome = end_bisect(&f.repo, &s.id).unwrap();
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(outcome.detail, format!("You left 1 commit behind on no branch, ending at {}.", &made[..7]));
        let [(keep, view::Action::Refs(crate::refs::Action::NewAt(crate::refs::Kind::Branch, kept)))] =
            outcome.actions.as_slice()
        else {
            panic!("Ending the bisect offered {:?}", outcome.actions);
        };
        assert_eq!((keep.as_ref(), kept.as_str()), (format!("Create branch at {}…", &made[..7]).as_str(), made.as_str()));
        assert!(outcome.sticky);
        f.git(&["bisect", "start", "HEAD", &base]);
        let s = snapshot(&f.repo).unwrap().unwrap();
        let outcome = end_bisect(&f.repo, &s.id).unwrap();
        assert_eq!((outcome.detail.as_str(), outcome.actions.len(), outcome.sticky), ("", 0, false));
    }

    #[test]
    fn an_operation_that_moved_on_is_refused() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        write(&f, "map.txt", "main map\n");
        f.git(&["add", "map.txt"]);
        f.git(&["commit", "-qm", "Map on feature"]);
        f.git(&["switch", "-q", "main"]);
        write(&f, "map.txt", "other map\n");
        f.git(&["add", "map.txt"]);
        f.git(&["commit", "-qm", "Map on main"]);
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "main"]);
        let first = snapshot(&f.repo).unwrap().unwrap();
        f.repo.expect_operation(&first.id).unwrap();
        stops(&f, &["rebase", "--skip"]);
        let next = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(next.kind, Kind::Rebase);
        assert_ne!(next.step, first.step);
        let err = f.repo.expect_operation(&first.id).unwrap_err();
        assert_eq!(err, "The operation in progress changed. Look at it again before continuing.");
        f.git(&["rebase", "--abort"]);
        assert!(f.repo.expect_operation(&next.id).is_err());
    }

    #[test]
    fn conflicts_are_shaped_by_their_stages_size_and_contents() {
        let f = Fixture::new();
        commit(&f, "gone-here.txt", "base\n", "Base");
        commit(&f, "gone-there.txt", "base\n", "Base two");
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0base").unwrap();
        f.git(&["add", "image.bin"]);
        commit(&f, "trail.txt", "base\n", "Base three");
        f.git(&["switch", "-qc", "side"]);
        f.git(&["rm", "-q", "gone-here.txt"]);
        write(&f, "gone-there.txt", "changed there\n");
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0side").unwrap();
        write(&f, "both.txt", "added on side\n");
        write(&f, "trail.txt", "side\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "Side"]);
        f.git(&["switch", "-q", "main"]);
        write(&f, "gone-here.txt", "changed here\n");
        f.git(&["rm", "-q", "gone-there.txt"]);
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0main").unwrap();
        write(&f, "both.txt", "added on main\n");
        write(&f, "trail.txt", "main\n");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "Main"]);
        stops(&f, &["merge", "side"]);
        let shapes: Vec<_> = snapshot(&f.repo).unwrap().unwrap().conflicts.into_iter().map(|c| (c.path, c.shape)).collect();
        assert_eq!(
            shapes,
            [
                ("both.txt".into(), Shape::BothAdded),
                ("gone-here.txt".into(), Shape::DeletedByThem),
                ("gone-there.txt".into(), Shape::DeletedByUs),
                ("image.bin".into(), Shape::Binary),
                ("trail.txt".into(), Shape::Text),
            ]
        );
    }

    #[test]
    fn the_guard_lets_through_only_what_cannot_disturb_an_operation() {
        use view::Action as A;
        let refused = "Finish or abort the rebase first";
        for action in [
            A::StageAll,
            A::Commit,
            A::CommitAll,
            A::Sync(crate::sync::Action::FetchAll),
            A::Sync(crate::sync::Action::Pull),
            A::Sync(crate::sync::Action::PullInto("main".into())),
            A::Sync(crate::sync::Action::ForcePush { name: "main".into(), checked_out: true }),
            A::Refs(crate::refs::Action::Switch("topic".into())),
            A::Refs(crate::refs::Action::NewAt(crate::refs::Kind::Branch, "0".repeat(40))),
            A::Stash(crate::stash::Action::Save),
            A::Reverse(crate::reverse::Action::ResetToUpstream),
        ] {
            assert_eq!(allows(Some(Kind::Rebase), &action), Err(refused), "{action:?}");
            assert_eq!(allows(None, &action), Ok(()), "{action:?}");
            assert_eq!(allows(Some(Kind::Bisect), &action), Ok(()), "{action:?}");
        }
        assert_eq!(allows(Some(Kind::Applied), &A::StageAll), Err("Resolve the conflicted files first"));
        for action in [
            A::Refresh,
            A::More,
            A::Merge(crate::merge::Action::Close),
            A::Operation(Action::Abort),
            A::Inspect(crate::inspection::Action::OpenFile("trail.txt".into())),
            A::Search(crate::search::Action::Open),
            A::Operation(Action::EndBisect),
            A::Select(view::Selection::Changes),
            A::CloseDiff,
            A::Dismiss,
            A::Refs(crate::refs::Action::ToggleList),
            A::Branch(crate::branch::Action::Restore { name: "x".into(), oid: "0".repeat(40), tracking: None }),
            A::Stash(crate::stash::Action::Restore { oid: "0".repeat(40), message: String::new() }),
            A::Jobs(crate::jobs::Action::Cancel(1)),
        ] {
            assert_eq!(allows(Some(Kind::Rebase), &action), Ok(()), "{action:?}");
        }
    }

    #[test]
    fn status_marks_every_kind_from_its_markers() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        let dir = f.repo.git_dir().to_owned();
        let none = || Ok(false);
        assert_eq!(detect(&dir, none).unwrap(), None);
        assert_eq!(detect(&dir, || Ok(true)).unwrap(), Some(Kind::Applied));
        for (marker, kind) in [
            ("BISECT_LOG", Kind::Bisect),
            ("MERGE_HEAD", Kind::Merge),
            ("REVERT_HEAD", Kind::Revert),
            ("CHERRY_PICK_HEAD", Kind::CherryPick),
        ] {
            fs::write(dir.join(marker), "x\n").unwrap();
            assert_eq!(detect(&dir, none).unwrap(), Some(kind), "{marker}");
        }
        fs::create_dir(dir.join("rebase-apply")).unwrap();
        assert_eq!(detect(&dir, none).unwrap(), Some(Kind::Rebase));
        fs::write(dir.join("rebase-apply/applying"), "").unwrap();
        assert_eq!(detect(&dir, none).unwrap(), Some(Kind::Patches));
        fs::create_dir(dir.join("rebase-merge")).unwrap();
        assert_eq!(detect(&dir, none).unwrap(), Some(Kind::Rebase));
        for name in ["rebase-merge", "rebase-apply"] {
            fs::remove_dir_all(dir.join(name)).unwrap();
        }
        for name in ["CHERRY_PICK_HEAD", "REVERT_HEAD", "MERGE_HEAD", "BISECT_LOG"] {
            fs::remove_file(dir.join(name)).unwrap();
        }
        fs::create_dir(dir.join("sequencer")).unwrap();
        fs::write(dir.join("sequencer/todo"), "revert 1234567 Second\n").unwrap();
        assert_eq!(detect(&dir, none).unwrap(), Some(Kind::Revert));
        fs::write(dir.join("sequencer/todo"), "pick 1234567 Second\n").unwrap();
        assert_eq!(detect(&dir, none).unwrap(), Some(Kind::CherryPick));
    }

    #[test]
    fn continue_waits_for_every_file_and_abort_says_what_is_lost() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "main"]);
        let mut state = State::default();
        assert_eq!(state.blocked().as_deref(), Some("Nothing is in progress"));
        state.mark(Some(Kind::Rebase));
        assert_eq!(state.blocked().as_deref(), Some("Wait for the operation to load"));
        state.update(snapshot(&f.repo).unwrap());
        assert_eq!(state.continue_label(), Some("Continue rebase"));
        assert_eq!(state.blocked().as_deref(), Some("Resolve 1 file first"));
        assert!(!state.can_finish() && state.has_message());
        let spec = abort_spec(state.snapshot().unwrap());
        assert_eq!((spec.title.as_str(), spec.confirm.as_ref(), spec.danger), ("Abort the rebase of feature onto main?", "Abort rebase", true));
        assert_eq!(
            spec.lines,
            [
                "Files you have already resolved go back to how they were before the rebase.",
                "feature goes back to where it was before the rebase, and the commits replayed so far are dropped.",
            ]
        );

        write(&f, "trail.txt", "both\n");
        f.git(&["add", "trail.txt"]);
        state.update(snapshot(&f.repo).unwrap());
        assert_eq!(state.blocked(), None);
        assert!(state.can_finish());
        assert_eq!(abort_spec(state.snapshot().unwrap()).lines.last().unwrap(), "The resolution of 1 file will be lost.");
        f.git(&["rebase", "--abort"]);

        commit(&f, "trail.txt", "base\n", "Back to base");
        write(&f, "trail.txt", "stashed\n");
        f.git(&["stash", "push", "-q"]);
        commit(&f, "trail.txt", "moved on\n", "Move on");
        stops(&f, &["stash", "apply"]);
        state.update(snapshot(&f.repo).unwrap());
        assert_eq!(state.continue_label(), None);
        assert_eq!(state.blocked().as_deref(), Some("Resolve the conflicted files first"));
        assert_eq!(Kind::Applied.abort_label(), None);
    }

    #[test]
    fn a_name_chosen_here_names_the_stopped_merge() {
        let f = diverged();
        let feature = oid(&f, "feature");
        f.git(&["branch", "copy", &feature]);
        stops(&f, &["merge", "feature"]);
        let mut state = State::default();
        state.update(snapshot(&f.repo).unwrap());
        assert_eq!(state.snapshot().unwrap().theirs(), &feature[..8]);
        state.started = Some((Kind::Merge, feature.clone(), "feature".into()));
        assert!(!state.update(snapshot(&f.repo).unwrap()));
        assert_eq!(state.snapshot().unwrap().theirs(), "feature");
        state.started = Some((Kind::Rebase, feature.clone(), "elsewhere".into()));
        state.update(snapshot(&f.repo).unwrap());
        assert_eq!(state.snapshot().unwrap().theirs(), &feature[..8]);
        assert!(state.active());
        assert_eq!(state.refusal().as_deref(), Some("Finish or abort the merge first"));
        assert!(state.update(None));
        assert!(state.started.is_none() && !state.active() && state.kind().is_none());
        assert!(state.mark(Some(Kind::Applied)));
        assert!(state.active());
    }

    fn merge_with_autostash(edited: &str) -> Fixture {
        let f = Fixture::new();
        commit(&f, "route.txt", "Start\nFord\nSummit\n", "Plan the route");
        commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "route.txt", "Start north\nFord\nSummit\n", "Start north");
        commit(&f, "trail.txt", "Trail via the ferry\n", "Route the ferry");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "trail.txt", "Trail via the bridge\n", "Route the bridge");
        write(&f, "route.txt", edited);
        stops(&f, &["merge", "--no-edit", "--autostash", "feature"]);
        write(&f, "trail.txt", "Trail via both\n");
        f.git(&["add", "--", "trail.txt"]);
        f
    }

    #[test]
    fn finishing_a_merge_git_stashed_for_puts_the_changes_back_or_names_the_stash() {
        let f = merge_with_autostash("Start\nFord\nSummit east\n");
        assert!(crate::autostash::pending(&f.repo).is_some());
        let shown = snapshot(&f.repo).unwrap().unwrap();
        let kept = crate::autostash::Kept::default();
        let done = put_back(&f.repo, &kept, |repo| backend::finish(repo, &shown, Some("Merge feature".into()))).ok().expect("commits");
        assert_eq!(done.title, "Merge committed");
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(kept.take().is_none());

        let f = merge_with_autostash("Start south\nFord\nSummit\n");
        let shown = snapshot(&f.repo).unwrap().unwrap();
        let left = put_back(&f.repo, &kept, |repo| {
            backend::finish(repo, &shown, Some("Merge feature".into())).map(|done| done.offer("Next", view::Action::Refresh))
        })
        .err()
        .expect("the edit conflicts");
        assert_eq!(left.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>(), ["Next"]);
        assert_eq!(
            left.text,
            "Merge committed — Your changes did not go back cleanly. 1 file needs resolving, and they stay in the stash \"autostash\" until you drop it."
        );
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Merge feature\n");
        let stash = kept.take().expect("the stash is named");
        assert_eq!((stash.oid, stash.title.as_str()), (oid(&f, "refs/stash"), "autostash"));
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().kind, Kind::Applied);
        let f = merge_with_autostash("Start\nFord\nSummit east\n");
        let shown = snapshot(&f.repo).unwrap().unwrap();
        let aborted = put_back(&f.repo, &kept, |repo| backend::abort(repo, &shown)).ok().expect("aborts");
        assert_eq!(aborted.title, "Merge aborted");
        assert!(snapshot(&f.repo).unwrap().is_none() && kept.take().is_none());
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
    }
}
