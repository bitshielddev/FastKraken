use crate::repository::Run;
use crate::{
    Tab,
    commands::{self, Availability, Command, Group, Side, Target},
    diff_doc::PatchState,
    notice::{self, Notice, Outcome},
    prompt::{self, Spec},
    repository::{Diff, Repository, checked, run_with_input, validate_oid, validate_path},
    view::{self, Selection, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

pub(crate) const DIFFERENT_NOW: &str = "These lines are different now, so the change could not be undone automatically.";
const DELETED: &str = "Deleted";
const ADDED: &str = "Added";
const SUBMODULE: &str = "This hunk moves a submodule to another commit, which cannot be undone in your files.";
const REVIEW: &str = "It shows as a change in your files, ready to review. Nothing was staged.";

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Restore { oid: String, path: PathBuf, before: bool },
    UndoHunk(usize),
}

#[derive(Default)]
pub(crate) struct State {
    looking: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub(crate) oid: String,
    pub(crate) path: PathBuf,
    pub(crate) from: Option<PathBuf>,
    pub(crate) before: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
    pub(crate) mode: u32,
    pub(crate) id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Present {
    Missing,
    Blob { mode: u32, id: String },
    Folder,
    Under(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Working {
    pub(crate) present: Present,
    pub(crate) rows: Vec<(bool, &'static str)>,
    pub(crate) tracked: bool,
}

impl Working {
    fn unsaved(&self) -> bool {
        self.rows.iter().any(|(staged, _)| !staged)
    }

    fn ignored(&self) -> bool {
        !self.tracked && self.rows.is_empty() && matches!(self.present, Present::Blob { .. })
    }

    fn staged(&self) -> bool {
        self.rows.iter().any(|(staged, _)| *staged)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restore {
    pub(crate) oid: String,
    pub(crate) before: bool,
    pub(crate) source: String,
    pub(crate) path: PathBuf,
    pub(crate) named: PathBuf,
    pub(crate) stored: Stored,
    pub(crate) working: Working,
}

impl Restore {
    pub(crate) fn unchanged(&self) -> bool {
        self.working.present == Present::Blob { mode: self.stored.mode, id: self.stored.id.clone() }
    }

    fn version(&self) -> String {
        let short = notice::short(&self.oid);
        if self.before { format!("as it was before {short}") } else { format!("from {short}") }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Undo {
    pub(crate) oid: String,
    pub(crate) path: Option<PathBuf>,
    pub(crate) from: Option<PathBuf>,
    pub(crate) hunk: usize,
    pub(crate) name: String,
}

#[derive(Debug)]
pub(crate) struct Ask(Restore);

fn name(path: &Path) -> String {
    path.display().to_string()
}

fn refusal(ui: &Ui) -> Option<String> {
    commands::busy(ui).or_else(|| commands::operating(ui))
}

fn committed<'a>(ui: &'a Ui, target: &'a Target) -> Option<(&'a String, &'a PathBuf, &'a crate::repository::ChangedFile)> {
    match target {
        Target::File { path, side: Side::Commit(oid) } => ui.inspection.file(oid, path).map(|file| (oid, path, file)),
        _ => None,
    }
}

fn restoring(target: &Target, before: bool) -> Option<view::Action> {
    match target {
        Target::File { path, side: Side::Commit(oid) } => {
            Some(view::Action::Recover(Action::Restore { oid: oid.clone(), path: path.clone(), before }))
        }
        _ => None,
    }
}

fn committed_diff(ui: &Ui) -> Option<&String> {
    match &ui.selection {
        Some(Selection::Commit(oid)) if ui.diff_open && ui.diff.file.is_none() && ui.diff.state == PatchState::Text => Some(oid),
        _ => None,
    }
}

pub(crate) fn undo_offered(ui: &Ui) -> bool {
    committed_diff(ui).is_some() && !ui.diff.options.whole_file && !ui.diff.options.ignore_whitespace
}

fn undo_refusal(ui: &Ui) -> Option<String> {
    refusal(ui)
        .or_else(|| ui.diff.options.ignore_whitespace.then(|| "Show whitespace changes to undo a hunk".to_owned()))
        .or_else(|| ui.diff.options.whole_file.then(|| "Show 3 lines of context to undo a single hunk".to_owned()))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "file.restore-version",
        Group::Reverse,
        |_, _| "Restore this version…".into(),
        |ui, target, _| match committed(ui, target) {
            Some((_, _, file)) => Availability::unless(refusal(ui).or_else(|| {
                (file.status == DELETED)
                    .then(|| "This commit deletes the file. Restore the version before it instead".to_owned())
            })),
            None => Availability::Hidden,
        },
        |_, target| restoring(target, false),
    )
    .words(&["restore file from this commit", "checkout file", "get this version back", "revert file"])
    .icon("history"),
    Command::new(
        "file.restore-before",
        Group::Reverse,
        |_, _| "Restore the version before this commit…".into(),
        |ui, target, _| match committed(ui, target) {
            Some((_, _, file)) => Availability::unless(refusal(ui).or_else(|| {
                (file.status == ADDED).then(|| "The file did not exist before this commit".to_owned())
            })),
            None => Availability::Hidden,
        },
        |_, target| restoring(target, true),
    )
    .words(&["restore previous version", "undo changes to this file", "revert file", "parent version"])
    .icon("history"),
    Command::new(
        "code.undo-hunk",
        Group::Reverse,
        |_, _| "Undo this hunk in my files".into(),
        |ui, target, _| match target {
            Target::Code { hunk: Some(_), .. } if committed_diff(ui).is_some() => Availability::unless(undo_refusal(ui)),
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Code { hunk: Some(hunk), .. } => Some(view::Action::Recover(Action::UndoHunk(*hunk))),
            _ => None,
        },
    )
    .words(&["revert hunk", "apply the inverse of this hunk"])
    .icon("undo"),
];

fn refuse(tab: &mut Tab, reason: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Restore { oid, path, before } => look(tab, oid, path, before, cx),
        Action::UndoHunk(hunk) => undo(tab, hunk, cx),
    }
}

fn look(tab: &mut Tab, oid: String, path: PathBuf, before: bool, cx: &mut Context<Tab>) {
    if refusal(&tab.ui).is_some() || tab.ui.prompt.open() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let Some(file) = tab.ui.inspection.file(&oid, &path) else {
        return refuse(tab, format!("{} is no longer listed in this commit", name(&path)), cx);
    };
    let wanted = Wanted { oid, from: file.from.clone(), path, before };
    tab.ui.menu = None;
    tab.ui.recover.looking += 1;
    let ticket = tab.ui.recover.looking;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || repo.plan_restore(&wanted),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.recover.looking != ticket {
                return;
            }
            match result {
                Ok(restore) if restore.unchanged() => {
                    refuse(tab, format!("{} already matches that version", name(&restore.path)), cx)
                }
                Ok(restore) => {
                    if refusal(&tab.ui).is_none() && !tab.ui.prompt.open() {
                        prompt::ask(&mut tab.ui, prompt::Kind::Recover(Ask(restore)), cx);
                    }
                }
                Err(reason) => refuse(tab, reason, cx),
            }
        },
    );
}

fn undo(tab: &mut Tab, hunk: usize, cx: &mut Context<Tab>) {
    if undo_refusal(&tab.ui).is_some() || tab.ui.prompt.open() {
        return;
    }
    let Some(oid) = committed_diff(&tab.ui).cloned() else { return };
    let Some(diff) = tab.diff.clone() else { return };
    let Some(rows) = tab.ui.diff.hunks.get(hunk) else { return };
    let name = crate::diff_select::file_of(&tab.ui, rows.start).map_or_else(|| "the file".to_owned(), |path| name(&path));
    let path = tab.ui.inspection.path.clone();
    let from = path.as_deref().and_then(|path| tab.ui.inspection.from(&oid, path));
    tab.ui.menu = None;
    let undo = Undo { oid, path, from, hunk, name };
    tab.run_operation_then(
        cx,
        "Undoing the hunk in your files…",
        "Hunk undone",
        false,
        move |repo| repo.undo_commit_hunk(&undo, &diff),
        |_, _| {},
    );
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(&ask.0)
}

fn spec(restore: &Restore) -> Spec {
    let shown = name(&restore.path);
    let mut spec = Spec::new(format!("Restore {shown} {}?", restore.version()), "Restore file");
    if restore.path != restore.named {
        spec = spec.line(format!(
            "Before this commit the file was named {shown}, so it is restored under that name. {} is not touched.",
            name(&restore.named)
        ));
    }
    let working = &restore.working;
    spec = if working.unsaved() {
        spec.line(format!("Your copy of {shown} has changes that are not committed. They will be replaced.")).danger()
    } else if working.ignored() {
        spec.line(format!("{shown} is in your files and Git ignores it. Your copy will be replaced.")).danger()
    } else if working.present == Present::Missing {
        spec.line(format!("{shown} is not in your files now. This brings it back."))
    } else {
        spec.line(format!("That version replaces {shown} in your files."))
    };
    if working.staged() {
        spec = spec.line("Staged changes are not touched.");
    }
    spec.line("The result shows as a change to review. Nothing is staged or committed.")
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, cx: &mut Context<Tab>) {
    let Ask(restore) = ask;
    tab.run_operation_then(
        cx,
        "Restoring the file…",
        "File restored",
        false,
        move |repo| repo.restore_from_history(&restore),
        |_, _| {},
    );
}

fn reverse_patch(header: &[u8], hunk: &[u8]) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = header.split_inclusive(|&b| b == b'\n').collect();
    let whole = lines
        .iter()
        .any(|line| line.starts_with(b"new file mode ") || line.starts_with(b"deleted file mode "));
    let mut patch = Vec::new();
    if whole {
        for line in lines {
            if !line.starts_with(b"old mode ") && !line.starts_with(b"new mode ") {
                patch.extend_from_slice(line);
            }
        }
    } else {
        let new = lines.iter().find_map(|line| line.strip_prefix(b"+++ "))?;
        let new = new.strip_suffix(b"\n").unwrap_or(new);
        let (named, tab): (&[u8], &[u8]) = match new.strip_suffix(b"\t") {
            Some(named) => (named, b"\t"),
            None => (new, b""),
        };
        let old = match named.strip_prefix(b"\"b/") {
            Some(rest) => [b"\"a/".as_slice(), rest].concat(),
            None => [b"a/".as_slice(), named.strip_prefix(b"b/")?].concat(),
        };
        for part in [b"diff --git ".as_slice(), &old, b" ", named, b"\n--- ", &old, tab, b"\n+++ ", named, tab, b"\n"] {
            patch.extend_from_slice(part);
        }
    }
    patch.extend_from_slice(hunk);
    if !patch.ends_with(b"\n") {
        patch.push(b'\n');
    }
    Some(patch)
}

fn moves_a_submodule(header: &[u8]) -> bool {
    header.split(|&b| b == b'\n').any(|line| {
        ["index ", "new file mode ", "deleted file mode ", "old mode ", "new mode "]
            .iter()
            .any(|start| line.starts_with(start.as_bytes()))
            && line.ends_with(b" 160000")
    })
}

fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

impl Repository {
    fn first_parent(&self, oid: &str) -> Result<Option<String>, String> {
        validate_oid(oid)?;
        let output = self
            .read_command()
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(format!("{oid}^1^{{commit}}"))
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let parent = String::from_utf8_lossy(line(&output.stdout)).into_owned();
        validate_oid(&parent)?;
        Ok(Some(parent))
    }

    pub(crate) fn stored_entry(&self, commit: &str, path: &Path) -> Result<Option<Stored>, String> {
        validate_oid(commit)?;
        validate_path(path)?;
        let listed = checked(
            self.read_command()
                .args(["--literal-pathspecs", "ls-tree", "-z", "--full-tree", commit, "--"])
                .arg(path),
        )?;
        let wanted = path.as_os_str().as_bytes();
        for entry in listed.split(|&b| b == 0) {
            let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
            if &entry[tab + 1..] != wanted {
                continue;
            }
            let fields = String::from_utf8_lossy(&entry[..tab]).into_owned();
            let mut fields = fields.split(' ');
            let (Some(mode), Some(kind), Some(id)) = (fields.next(), fields.next(), fields.next()) else {
                return Err("Git listed the file in a form FastKraken does not read.".into());
            };
            if kind != "blob" {
                return Err(format!("{} is not a file at that version, so it cannot be restored.", name(path)));
            }
            let mode = u32::from_str_radix(mode, 8).map_err(|_| "Git listed the file without a mode".to_owned())?;
            validate_oid(id)?;
            return Ok(Some(Stored { mode, id: id.to_owned() }));
        }
        Ok(None)
    }

    pub(crate) fn working_copy(&self, path: &Path) -> Result<Working, String> {
        validate_path(path)?;
        let mut present = None;
        let mut parent = PathBuf::new();
        for part in path.parent().into_iter().flat_map(Path::components) {
            parent.push(part);
            if self.root.join(&parent).symlink_metadata().is_ok_and(|meta| !meta.is_dir()) {
                present = Some(Present::Under(parent.clone()));
                break;
            }
        }
        let present = match present {
            Some(under) => under,
            None => match self.root.join(path).symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Present::Missing,
                Err(error) => return Err(format!("Read {}: {error}", name(path))),
                Ok(meta) if meta.is_dir() => Present::Folder,
                Ok(meta) if meta.file_type().is_symlink() => {
                    let target = std::fs::read_link(self.root.join(path)).map_err(|e| format!("Read {}: {e}", name(path)))?;
                    let output = run_with_input(
                        self.read_command().args(["hash-object", "--no-filters", "--stdin"]),
                        target.as_os_str().as_bytes().to_vec(),
                    )
                    .map_err(|e| format!("Run bundled Git: {e}"))?;
                    if !output.status.success() {
                        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
                    }
                    Present::Blob { mode: 0o120000, id: String::from_utf8_lossy(line(&output.stdout)).into_owned() }
                }
                Ok(meta) => {
                    let hashed = checked(self.read_command().args(["hash-object", "--"]).arg(path))?;
                    let mode = if meta.permissions().mode() & 0o111 != 0 { 0o100755 } else { 0o100644 };
                    Present::Blob { mode, id: String::from_utf8_lossy(line(&hashed)).into_owned() }
                }
            },
        };
        let status = self.status()?;
        let mut rows: Vec<(bool, &'static str)> = [(false, &status.unstaged), (true, &status.staged)]
            .into_iter()
            .flat_map(|(staged, rows)| {
                rows.iter()
                    .filter(|row| row.path == path || row.from.as_deref() == Some(path))
                    .map(move |row| (staged, row.status))
            })
            .collect();
        rows.sort();
        let tracked = checked(self.read_command().args(["--literal-pathspecs", "ls-files", "-z", "--"]).arg(path))?
            .split(|&b| b == 0)
            .any(|entry| entry == path.as_os_str().as_bytes());
        Ok(Working { present, rows, tracked })
    }

    pub(crate) fn plan_restore(&self, wanted: &Wanted) -> Result<Restore, String> {
        validate_oid(&wanted.oid)?;
        validate_path(&wanted.path)?;
        if let Some(from) = &wanted.from {
            validate_path(from)?;
        }
        self.expect_no_operation()?;
        let (source, path) = if wanted.before {
            let path = wanted.from.clone().unwrap_or_else(|| wanted.path.clone());
            let missing = || format!("{} did not exist before this commit.", name(&path));
            let parent = self.first_parent(&wanted.oid)?.ok_or_else(missing)?;
            (parent, path)
        } else {
            (wanted.oid.clone(), wanted.path.clone())
        };
        let stored = self.stored_entry(&source, &path)?.ok_or_else(|| {
            if wanted.before {
                format!("{} did not exist before this commit.", name(&path))
            } else {
                format!("{} is not in this commit, which deletes it.", name(&path))
            }
        })?;
        let working = self.working_copy(&path)?;
        match &working.present {
            Present::Folder => {
                return Err(format!("{} is a folder in your files. Move it aside, then restore the file.", name(&path)));
            }
            Present::Under(parent) => {
                return Err(format!(
                    "{} is a file in your files, where that version has a folder. Move it aside, then restore the file.",
                    name(parent)
                ));
            }
            Present::Missing | Present::Blob { .. } => {}
        }
        Ok(Restore { oid: wanted.oid.clone(), before: wanted.before, source, path, named: wanted.path.clone(), stored, working })
    }

    pub(crate) fn restore_from_history(&self, restore: &Restore) -> Result<Outcome, String> {
        validate_oid(&restore.oid)?;
        self.expect_no_operation()?;
        let shown = name(&restore.path);
        if self.stored_entry(&restore.source, &restore.path)?.as_ref() != Some(&restore.stored) {
            return Err(format!("That version of {shown} is not in the repository any more."));
        }
        if self.working_copy(&restore.path)? != restore.working {
            return Err(format!("{shown} changed since you chose to restore it. Look at it again, then restore it."));
        }
        self.with_paths(&["restore", "--worktree", &format!("--source={}", restore.source)], &[restore.path.clone()])?;
        Ok(Outcome::titled(format!("Restored {shown} {}", restore.version()), REVIEW))
    }

    pub(crate) fn undo_commit_hunk(&self, undo: &Undo, diff: &Diff) -> Result<Outcome, String> {
        validate_oid(&undo.oid)?;
        self.expect_no_operation()?;
        if diff.options.ignore_whitespace {
            return Err("Whitespace changes are hidden in this diff, so its hunks cannot be undone. Show them and try again.".into());
        }
        if diff.options.whole_file {
            return Err("This diff shows the whole file as one hunk. Show 3 lines of context to undo a single hunk.".into());
        }
        let latest = match &undo.path {
            Some(path) => self.commit_path_diff_with(&undo.oid, path, undo.from.as_deref(), diff.options)?,
            None => self.commit_diff_with(&undo.oid, diff.options)?,
        };
        if !latest.same_patch(diff) {
            return Err("The diff shown is not this commit's any more. Open it again, then undo the hunk.".into());
        }
        let (header, hunk) = diff.hunk_parts(undo.hunk).ok_or("This hunk is no longer in the diff.")?;
        if moves_a_submodule(header) {
            return Err(SUBMODULE.into());
        }
        let patch = reverse_patch(header, hunk).ok_or("This hunk is no longer in the diff.")?;
        let output = run_with_input(
            self.command().args(["apply", "--reverse", "--whitespace=nowarn", "-"]),
            patch,
        )
        .map_err(|e| format!("Run bundled Git: {e}"))?;
        if !output.status.success() {
            return Err(DIFFERENT_NOW.into());
        }
        Ok(Outcome::titled(format!("Undid a hunk in {}", undo.name), REVIEW))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn head(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim_end().to_owned()
    }

    fn listed(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "-uall"])).unwrap()
    }

    fn read(f: &Fixture, path: &str) -> String {
        fs::read_to_string(f.dir.join(path)).unwrap()
    }

    fn record(f: &Fixture, path: &str, text: &str, message: &str) -> String {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(f.dir.join(parent)).unwrap();
        }
        fs::write(f.dir.join(path), text).unwrap();
        f.git(&["add", "--", path]);
        f.git(&["commit", "-qm", message]);
        head(f, "HEAD")
    }

    fn wanted(oid: &str, path: &str, before: bool) -> Wanted {
        Wanted { oid: oid.to_owned(), path: path.into(), from: None, before }
    }

    const LOG: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n";

    #[test]
    fn restoring_a_version_writes_only_the_working_file_and_leaves_the_index_and_refs() {
        let f = Fixture::new();
        let first = record(&f, "log.txt", "Day one: calm\n", "Start the log");
        let second = record(&f, "log.txt", "Day one: calm\nDay two: wind\n", "Log day two");
        record(&f, "log.txt", "Day three only\n", "Rewrite the log");
        let tip = head(&f, "HEAD");
        let index = f.git(&["ls-files", "-s"]);

        let restore = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        assert_eq!((restore.source.as_str(), restore.path.as_path()), (first.as_str(), Path::new("log.txt")));
        assert_eq!(restore.working.rows, []);
        assert!(restore.working.tracked && !restore.unchanged());
        let outcome = f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(outcome.title, format!("Restored log.txt from {}", &first[..7]));
        assert_eq!(outcome.detail, REVIEW);
        assert_eq!(read(&f, "log.txt"), "Day one: calm\n");
        assert_eq!(listed(&f), " M log.txt\n");
        assert_eq!(f.git(&["ls-files", "-s"]), index);
        assert_eq!(head(&f, "HEAD"), tip);

        let restore = f.repo.plan_restore(&wanted(&second, "log.txt", true)).unwrap();
        assert_eq!(restore.source, first);
        assert!(restore.unchanged());
        let restore = f.repo.plan_restore(&wanted(&tip, "log.txt", true)).unwrap();
        assert_eq!(restore.source, second);
        assert_eq!(restore.working.rows, [(false, "Modified")]);
        let outcome = f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(outcome.title, format!("Restored log.txt as it was before {}", &tip[..7]));
        assert_eq!(read(&f, "log.txt"), "Day one: calm\nDay two: wind\n");
        assert_eq!(f.git(&["ls-files", "-s"]), index);
        assert_eq!(head(&f, "HEAD"), tip);
    }

    #[test]
    fn restoring_brings_back_a_deleted_file_and_keeps_staged_changes() {
        let f = Fixture::new();
        let made = record(&f, "map/route.txt", "North trail\n", "Draw the route");
        record(&f, "notes.txt", "Notes\n", "Take notes");
        f.git(&["rm", "-q", "-r", "map"]);
        f.git(&["commit", "-qm", "Drop the map"]);
        let dropped = head(&f, "HEAD");

        let err = f.repo.plan_restore(&wanted(&dropped, "map/route.txt", false)).unwrap_err();
        assert_eq!(err, "map/route.txt is not in this commit, which deletes it.");
        let restore = f.repo.plan_restore(&wanted(&dropped, "map/route.txt", true)).unwrap();
        assert_eq!(restore.working.present, Present::Missing);
        assert!(!restore.working.tracked);
        f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(read(&f, "map/route.txt"), "North trail\n");
        assert_eq!(listed(&f), "?? map/route.txt\n");

        let err = f.repo.plan_restore(&wanted(&made, "map/route.txt", true)).unwrap_err();
        assert_eq!(err, "map/route.txt did not exist before this commit.");

        fs::write(f.dir.join("notes.txt"), "Notes, staged\n").unwrap();
        f.git(&["add", "notes.txt"]);
        fs::write(f.dir.join("notes.txt"), "Notes, staged, then edited\n").unwrap();
        let notes = head(&f, "HEAD~1");
        let restore = f.repo.plan_restore(&wanted(&notes, "notes.txt", false)).unwrap();
        assert_eq!(restore.working.rows, [(false, "Modified"), (true, "Modified")]);
        f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(read(&f, "notes.txt"), "Notes\n");
        assert_eq!(f.git(&["show", ":notes.txt"]), b"Notes, staged\n");
    }

    #[test]
    fn restoring_the_version_before_a_rename_uses_the_old_name() {
        let f = Fixture::new();
        record(&f, "old.txt", LOG, "Keep a log");
        f.git(&["mv", "old.txt", "new.txt"]);
        fs::write(f.dir.join("new.txt"), LOG.replace("six", "SIX")).unwrap();
        f.git(&["add", "new.txt"]);
        f.git(&["commit", "-qm", "Rename the log"]);
        let renamed = head(&f, "HEAD");
        let wanted = Wanted { from: Some("old.txt".into()), ..wanted(&renamed, "new.txt", true) };
        let restore = f.repo.plan_restore(&wanted).unwrap();
        assert_eq!((restore.path.as_path(), restore.named.as_path()), (Path::new("old.txt"), Path::new("new.txt")));
        let asked = spec(&restore);
        assert_eq!(asked.title, format!("Restore old.txt as it was before {}?", &renamed[..7]));
        assert_eq!(asked.lines[..2], [
            "Before this commit the file was named old.txt, so it is restored under that name. new.txt is not touched.",
            "old.txt is not in your files now. This brings it back.",
        ]);
        f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(read(&f, "old.txt"), LOG);
        assert_eq!(read(&f, "new.txt"), LOG.replace("six", "SIX"));
        assert_eq!(listed(&f), "?? old.txt\n");
    }

    #[test]
    fn restoring_refuses_a_working_file_that_changed_after_the_prompt_and_folders_in_the_way() {
        let f = Fixture::new();
        let first = record(&f, "log.txt", "First\n", "Start");
        record(&f, "log.txt", "Second\n", "Next");
        let restore = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        fs::write(f.dir.join("log.txt"), "Typed meanwhile\n").unwrap();
        let err = f.repo.restore_from_history(&restore).unwrap_err();
        assert_eq!(err, "log.txt changed since you chose to restore it. Look at it again, then restore it.");
        assert_eq!(read(&f, "log.txt"), "Typed meanwhile\n");

        let restore = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        fs::write(f.dir.join("log.txt"), "Typed again\n").unwrap();
        assert!(f.repo.restore_from_history(&restore).is_err());
        assert_eq!(read(&f, "log.txt"), "Typed again\n");

        let restore = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        f.git(&["add", "log.txt"]);
        assert!(f.repo.restore_from_history(&restore).is_err());
        assert_eq!(f.git(&["show", ":log.txt"]), b"Typed again\n");

        f.git(&["rm", "-q", "--cached", "log.txt"]);
        fs::remove_file(f.dir.join("log.txt")).unwrap();
        fs::create_dir(f.dir.join("log.txt")).unwrap();
        let err = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap_err();
        assert_eq!(err, "log.txt is a folder in your files. Move it aside, then restore the file.");

        let deep = record(&f, "a/b.txt", "Deep\n", "Go deep");
        f.git(&["rm", "-q", "-r", "a"]);
        fs::write(f.dir.join("a"), "a file\n").unwrap();
        let err = f.repo.plan_restore(&wanted(&deep, "a/b.txt", false)).unwrap_err();
        assert!(err.starts_with("a is a file in your files"), "{err}");
        assert_eq!(read(&f, "a"), "a file\n");
    }

    #[test]
    fn restoring_notices_an_ignored_copy_and_an_operation_in_progress() {
        let f = Fixture::new();
        let first = record(&f, "build.log", "Built\n", "Keep the build log");
        f.git(&["rm", "-q", "--cached", "build.log"]);
        record(&f, ".gitignore", "build.log\n", "Ignore the log");
        fs::write(f.dir.join("build.log"), "Local build\n").unwrap();
        let restore = f.repo.plan_restore(&wanted(&first, "build.log", false)).unwrap();
        assert!(restore.working.ignored() && !restore.working.unsaved());
        let spec = spec(&restore);
        assert!(spec.danger);
        assert!(spec.lines.iter().any(|line| line == "build.log is in your files and Git ignores it. Your copy will be replaced."));
        f.repo.restore_from_history(&restore).unwrap();
        assert_eq!(read(&f, "build.log"), "Built\n");

        let head = head(&f, "HEAD");
        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
        assert!(f.repo.plan_restore(&wanted(&first, "build.log", false)).unwrap_err().contains("merge"));
        assert!(f.repo.restore_from_history(&restore).unwrap_err().contains("merge"));
    }

    #[test]
    fn the_prompt_says_when_uncommitted_changes_will_be_replaced() {
        let f = Fixture::new();
        let first = record(&f, "log.txt", "First\n", "Start");
        record(&f, "log.txt", "Second\n", "Next");
        let clean = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        let asked = spec(&clean);
        assert_eq!(asked.title, format!("Restore log.txt from {}?", &first[..7]));
        assert!(!asked.danger);
        assert_eq!(asked.lines, ["That version replaces log.txt in your files.", "The result shows as a change to review. Nothing is staged or committed."]);

        fs::write(f.dir.join("log.txt"), "Edited\n").unwrap();
        let edited = f.repo.plan_restore(&wanted(&first, "log.txt", false)).unwrap();
        let warned = spec(&edited);
        assert!(warned.danger);
        assert_eq!(warned.lines[0], "Your copy of log.txt has changes that are not committed. They will be replaced.");
    }

    #[test]
    fn restoring_keeps_the_mode_and_reads_symlinks() {
        let f = Fixture::new();
        fs::write(f.dir.join("run.sh"), ": go\n").unwrap();
        fs::set_permissions(f.dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("run.sh", f.dir.join("link")).unwrap();
        f.git(&["add", "run.sh", "link"]);
        f.git(&["commit", "-qm", "Add the script"]);
        let made = head(&f, "HEAD");
        let restore = f.repo.plan_restore(&wanted(&made, "run.sh", false)).unwrap();
        assert_eq!(restore.stored.mode, 0o100755);
        assert!(restore.unchanged());
        let link = f.repo.plan_restore(&wanted(&made, "link", false)).unwrap();
        assert_eq!(link.stored.mode, 0o120000);
        assert!(link.unchanged());
        fs::set_permissions(f.dir.join("run.sh"), fs::Permissions::from_mode(0o644)).unwrap();
        let restore = f.repo.plan_restore(&wanted(&made, "run.sh", false)).unwrap();
        assert!(!restore.unchanged());
        f.repo.restore_from_history(&restore).unwrap();
        assert_ne!(fs::metadata(f.dir.join("run.sh")).unwrap().permissions().mode() & 0o111, 0);
        assert_eq!(listed(&f), "");
    }

    fn undo_of(f: &Fixture, oid: &str, path: Option<&str>, hunk: usize) -> (Undo, Diff) {
        let diff = match path {
            Some(path) => f.repo.commit_path_diff(oid, Path::new(path), None).unwrap(),
            None => f.repo.commit_diff(oid).unwrap(),
        };
        let undo = Undo { oid: oid.to_owned(), path: path.map(PathBuf::from), from: None, hunk, name: path.unwrap_or("the file").to_owned() };
        (undo, diff)
    }

    #[test]
    fn undoing_a_hunk_applies_its_inverse_to_the_working_file_only() {
        let f = Fixture::new();
        record(&f, "log.txt", LOG, "Keep a log");
        let edited = LOG.replace("two", "TWO").replace("eleven", "ELEVEN");
        let edit = record(&f, "log.txt", &edited, "Shout two numbers");
        fs::write(f.dir.join("log.txt"), edited.replace("six", "six and a half")).unwrap();
        let index = f.git(&["ls-files", "-s"]);

        let (undo, diff) = undo_of(&f, &edit, Some("log.txt"), 1);
        assert_eq!(diff.hunks.len(), 2);
        let outcome = f.repo.undo_commit_hunk(&undo, &diff).unwrap();
        assert_eq!(outcome.title, "Undid a hunk in log.txt");
        assert_eq!(read(&f, "log.txt"), LOG.replace("two", "TWO").replace("six", "six and a half"));
        assert_eq!(f.git(&["ls-files", "-s"]), index);
        assert_eq!(head(&f, "HEAD"), edit);

        let err = f.repo.undo_commit_hunk(&undo, &diff).unwrap_err();
        assert_eq!(err, DIFFERENT_NOW);
        assert_eq!(read(&f, "log.txt"), LOG.replace("two", "TWO").replace("six", "six and a half"));

        let (undo, diff) = undo_of(&f, &edit, None, 0);
        f.repo.undo_commit_hunk(&undo, &diff).unwrap();
        assert_eq!(read(&f, "log.txt"), LOG.replace("six", "six and a half"));
        assert_eq!(f.git(&["ls-files", "-s"]), index);
    }

    #[test]
    fn undoing_a_hunk_refuses_lines_that_changed_since_and_a_diff_that_is_not_the_commits() {
        let f = Fixture::new();
        record(&f, "log.txt", LOG, "Keep a log");
        let edit = record(&f, "log.txt", &LOG.replace("two", "TWO"), "Shout two");
        let other = record(&f, "log.txt", &LOG.replace("two", "TWO").replace("ten", "TEN"), "Shout ten");
        fs::write(f.dir.join("log.txt"), LOG.replace("two", "2")).unwrap();
        let (undo, diff) = undo_of(&f, &edit, Some("log.txt"), 0);
        assert_eq!(f.repo.undo_commit_hunk(&undo, &diff).unwrap_err(), DIFFERENT_NOW);
        assert_eq!(read(&f, "log.txt"), LOG.replace("two", "2"));

        let (_, stale) = undo_of(&f, &other, Some("log.txt"), 0);
        let err = f.repo.undo_commit_hunk(&undo, &stale).unwrap_err();
        assert!(err.starts_with("The diff shown is not this commit's"), "{err}");
        let whole = f.repo.commit_path_diff_with(&edit, Path::new("log.txt"), None, crate::repository::DiffOptions { whole_file: true, ignore_whitespace: false }).unwrap();
        assert!(f.repo.undo_commit_hunk(&undo, &whole).unwrap_err().contains("whole file"));
        let spaced = f.repo.commit_path_diff_with(&edit, Path::new("log.txt"), None, crate::repository::DiffOptions { whole_file: false, ignore_whitespace: true }).unwrap();
        assert!(f.repo.undo_commit_hunk(&undo, &spaced).unwrap_err().contains("Whitespace"));
        assert_eq!(read(&f, "log.txt"), LOG.replace("two", "2"));
        assert_eq!(listed(&f), " M log.txt\n");
    }

    #[test]
    fn undoing_a_hunk_follows_renames_and_new_and_deleted_files() {
        let f = Fixture::new();
        record(&f, "old name.txt", LOG, "Keep a log");
        f.git(&["mv", "old name.txt", "new name.txt"]);
        fs::write(f.dir.join("new name.txt"), LOG.replace("nine", "NINE")).unwrap();
        f.git(&["add", "new name.txt"]);
        f.git(&["commit", "-qm", "Rename and shout"]);
        let renamed = head(&f, "HEAD");
        let diff = f.repo.commit_path_diff(&renamed, Path::new("new name.txt"), Some(Path::new("old name.txt"))).unwrap();
        let undo = Undo {
            oid: renamed.clone(),
            path: Some("new name.txt".into()),
            from: Some("old name.txt".into()),
            hunk: 0,
            name: "new name.txt".into(),
        };
        f.repo.undo_commit_hunk(&undo, &diff).unwrap();
        assert_eq!(read(&f, "new name.txt"), LOG);
        assert!(!f.dir.join("old name.txt").exists());
        assert_eq!(listed(&f), " M \"new name.txt\"\n");
        f.git(&["checkout", "--", "new name.txt"]);

        let added = record(&f, "camp.txt", "Tent\n", "Pitch camp");
        let (undo, diff) = undo_of(&f, &added, Some("camp.txt"), 0);
        f.repo.undo_commit_hunk(&undo, &diff).unwrap();
        assert!(!f.dir.join("camp.txt").exists());
        assert_eq!(listed(&f), " D camp.txt\n");
        f.git(&["checkout", "--", "camp.txt"]);

        f.git(&["rm", "-q", "camp.txt"]);
        f.git(&["commit", "-qm", "Strike camp"]);
        let struck = head(&f, "HEAD");
        let (undo, diff) = undo_of(&f, &struck, Some("camp.txt"), 0);
        f.repo.undo_commit_hunk(&undo, &diff).unwrap();
        assert_eq!(read(&f, "camp.txt"), "Tent\n");
        assert_eq!(listed(&f), "?? camp.txt\n");
    }

    #[test]
    fn undoing_a_submodule_hunk_is_refused_rather_than_reported_done() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "One"]);
        let one = head(&f, "HEAD");
        f.git(&["commit", "--allow-empty", "-qm", "Two"]);
        let two = head(&f, "HEAD");
        f.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{one},sub")]);
        f.git(&["commit", "-qm", "Add the submodule"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{two},sub")]);
        f.git(&["commit", "-qm", "Move the submodule"]);
        let moved = head(&f, "HEAD");
        fs::create_dir(f.dir.join("sub")).unwrap();
        let (undo, diff) = undo_of(&f, &moved, Some("sub"), 0);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(f.repo.undo_commit_hunk(&undo, &diff).unwrap_err(), SUBMODULE);
        assert_eq!(f.git(&["ls-files", "-s", "sub"]), format!("160000 {two} 0\tsub\n").into_bytes());
        assert!(!moves_a_submodule(b"diff --git a/x 160000 b/x 160000\nindex 1..2 100644\n--- a/x 160000\t\n+++ b/x 160000\t\n"));
    }

    #[test]
    fn the_reverse_patch_names_the_new_path_on_both_sides() {
        let header = b"diff --git a/old b/new\nsimilarity index 90%\nrename from old\nrename to new\nindex 1..2 100644\n--- a/old\n+++ b/new\n";
        let patch = reverse_patch(header, b"@@ -1 +1 @@\n-a\n+b").unwrap();
        assert_eq!(String::from_utf8(patch).unwrap(), "diff --git a/new b/new\n--- a/new\n+++ b/new\n@@ -1 +1 @@\n-a\n+b\n");
        let header = b"diff --git a/sp ace b/sp ace\nold mode 100644\nnew mode 100755\n--- a/sp ace\t\n+++ b/sp ace\t\n";
        let patch = reverse_patch(header, b"@@ -1 +1 @@\n-a\n+b\n").unwrap();
        assert_eq!(String::from_utf8(patch).unwrap(), "diff --git a/sp ace b/sp ace\n--- a/sp ace\t\n+++ b/sp ace\t\n@@ -1 +1 @@\n-a\n+b\n");
        let header = b"diff --git \"a/\\303\\251\" \"b/\\303\\251\"\n--- \"a/\\303\\251\"\n+++ \"b/\\303\\251\"\n";
        let patch = reverse_patch(header, b"@@ -1 +1 @@\n-a\n+b\n").unwrap();
        assert!(String::from_utf8(patch).unwrap().starts_with("diff --git \"a/\\303\\251\" \"b/\\303\\251\"\n--- \"a/\\303\\251\"\n"));
        let header = b"diff --git a/new b/new\nnew file mode 100644\nindex 000..1\n--- /dev/null\n+++ b/new\n";
        let patch = reverse_patch(header, b"@@ -0,0 +1 @@\n+a\n").unwrap();
        assert_eq!(patch, [header.as_slice(), b"@@ -0,0 +1 @@\n+a\n"].concat());
    }
}
