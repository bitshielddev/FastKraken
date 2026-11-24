use crate::repository::Run;
use crate::{
    Tab, kit,
    notice::{self, Failure, Outcome},
    prompt::{self, Answer, Spec},
    repository::{
        ChangedFile, Commit, Diff, Repository, checked, nul_separated, parse_changed_files,
        patch_options, raw_path, read_diff, with_origins, validate_oid, validate_path,
    },
    theme::*,
    view::{self, CommitInput, Desk, Selection, Ui},
};
use gpui::{
    Context, Div, Entity, Focusable, FontWeight, KeyDownEvent, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Side, Target, busy, operating, on_repo};
    fn stash<'a>(ui: &Ui, target: &'a Target) -> Option<&'a String> {
        match target {
            Target::Stash(oid) if ui.stash.entries.iter().any(|entry| &entry.oid == oid) => Some(oid),
            _ => None,
        }
    }
    fn worktree_files(target: &Target) -> Option<Vec<PathBuf>> {
        match target {
            Target::File { path, side: Side::Unstaged | Side::Staged } => Some(vec![path.clone()]),
            Target::Files { paths, side: Side::Unstaged | Side::Staged } => Some(paths.clone()),
            _ => None,
        }
    }
    &[
        Command::new(
            "repo.stash",
            Group::Stash,
            |_, _| "Stash changes".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(refusal(ui, false))
            },
            |_, _| Some(view::Action::Stash(Action::Save)),
        )
        .words(&["save", "shelve"])
        .keys("alt-cmd-s")
        .icon("stash")
        .bar(Bar::Repository),
        Command::new(
            "repo.stash-options",
            Group::Stash,
            |_, _| "Stash with a message…".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)))
            },
            |_, _| Some(view::Action::Stash(Action::ToggleOptions)),
        )
        .words(&["name stash", "stash message", "stash staged only", "staged changes only"])
        .icon("stash")
        .bar(Bar::Repository),
        Command::new(
            "repo.pop-stash",
            Group::Stash,
            |_, _| "Pop latest stash".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(index_edit(ui).or_else(|| operating(ui)).or_else(|| {
                    ui.stash.entries.is_empty().then(|| "There is no stash to pop".to_owned())
                }))
            },
            |ui, _| ui.stash.entries.first().map(|entry| view::Action::Stash(Action::Pop(entry.oid.clone()))),
        )
        .words(&["unstash", "restore stash"])
        .keys("alt-shift-cmd-s")
        .icon("stash")
        .bar(Bar::Repository),
        Command::new(
            "file.stash",
            Group::Stash,
            |_, target| match target {
                Target::Files { paths, .. } => format!("Stash {} files", paths.len()),
                _ => "Stash file".into(),
            },
            |ui, target, _| match worktree_files(target) {
                Some(_) => Availability::unless(index_edit(ui)),
                None => Availability::Hidden,
            },
            |_, target| worktree_files(target).map(|paths| view::Action::Stash(Action::SaveFiles(paths))),
        )
        .icon("stash"),
        Command::new(
            "stash.apply",
            Group::Stash,
            |_, _| "Apply stash".into(),
            |ui, target, _| match stash(ui, target) {
                Some(_) => Availability::unless(index_edit(ui).or_else(|| operating(ui))),
                None => Availability::Hidden,
            },
            |ui, target| stash(ui, target).map(|oid| view::Action::Stash(Action::Apply(oid.clone()))),
        )
        .words(&["restore", "unstash"])
        .icon("stash"),
        Command::new(
            "stash.pop",
            Group::Stash,
            |_, _| "Pop stash".into(),
            |ui, target, _| match stash(ui, target) {
                Some(_) => Availability::unless(index_edit(ui).or_else(|| operating(ui))),
                None => Availability::Hidden,
            },
            |ui, target| stash(ui, target).map(|oid| view::Action::Stash(Action::Pop(oid.clone()))),
        )
        .words(&["restore", "unstash"]),
        Command::new(
            "stash.rename",
            Group::Manage,
            |_, _| "Rename stash…".into(),
            |ui, target, _| match stash(ui, target) {
                Some(_) => Availability::unless(busy(ui).or_else(|| operating(ui))),
                None => Availability::Hidden,
            },
            |ui, target| stash(ui, target).map(|oid| view::Action::Stash(Action::Rename(oid.clone()))),
        )
        .words(&["edit stash message", "name stash"]),
        Command::new(
            "stash.drop",
            Group::Manage,
            |_, _| "Drop stash…".into(),
            |ui, target, _| match stash(ui, target) {
                Some(_) => Availability::unless(busy(ui).or_else(|| operating(ui))),
                None => Availability::Hidden,
            },
            |ui, target| stash(ui, target).map(|oid| view::Action::Stash(Action::Drop(oid.clone()))),
        )
        .words(&["delete stash", "remove stash"])
        .icon("x")
        .danger(),
    ]
};

fn index_edit(ui: &Ui) -> Option<String> {
    crate::commands::busy(ui).or_else(|| ui.operation.refusal())
}

fn refusal(ui: &Ui, staged: bool) -> Option<String> {
    index_edit(ui).or_else(|| crate::commands::operating(ui)).or_else(|| {
        if staged {
            staged_paths(ui)
                .is_empty()
                .then(|| "Nothing is staged. Stage the changes to stash first".to_owned())
        } else {
            (ui.changed_count() == 0).then(|| "Nothing to stash: no uncommitted changes".to_owned())
        }
    })
}

fn staged_paths(ui: &Ui) -> Vec<PathBuf> {
    ui.files.iter().filter(|file| file.staged).map(|file| file.path.clone()).collect()
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub oid: String,
    pub title: String,
    pub message: String,
    pub branch: Option<String>,
    base: Option<String>,
    pub author: String,
    pub date: String,
    pub time: i64,
}

fn named(message: &str) -> (String, Option<String>) {
    if let Some((branch, _)) = message.strip_prefix("WIP on ").and_then(|rest| rest.split_once(": ")) {
        return (format!("WIP on {branch}"), Some(branch.to_owned()));
    }
    if let Some((branch, text)) = message.strip_prefix("On ").and_then(|rest| rest.split_once(": "))
        && !text.trim().is_empty()
    {
        return (text.to_owned(), Some(branch.to_owned()));
    }
    (message.to_owned(), None)
}

fn unnamed(branch: &str, detached: bool) -> String {
    format!("WIP on {}", if detached { "(no branch)" } else { branch })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Untracked {
    pub path: PathBuf,
    pub mode: String,
    pub oid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Push {
    pub message: String,
    pub staged: Option<Vec<PathBuf>>,
}

pub(crate) fn push_changes(cmd: &mut std::process::Command) -> Result<(), String> {
    saved(&checked(cmd.env("LC_ALL", "C"))?)
}

fn push_changes_from(cmd: &mut std::process::Command, pathspecs: Vec<u8>) -> Result<(), String> {
    saved(&crate::operation::with_input(cmd.env("LC_ALL", "C"), pathspecs)?)
}

fn saved(output: &[u8]) -> Result<(), String> {
    if output
        .split(|&byte| byte == b'\n')
        .any(|line| line.starts_with(b"Saved working directory and index state "))
    {
        Ok(())
    } else {
        Err("No changes could be stashed in this repository.".into())
    }
}

impl Repository {
    pub(crate) fn stashes(&self) -> Result<Vec<Entry>, String> {
        let bytes = checked(self.read_command().args([
            "stash",
            "list",
            "--no-color",
            "--no-show-signature",
            "--format=%H%x00%P%x00%an%x00%as%x00%at%x00%gs",
            "-z",
        ]))?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let mut fields = bytes
            .strip_suffix(b"\0")
            .ok_or("Incomplete stash list")?
            .split(|&b| b == 0);
        let mut entries = Vec::new();
        while let Some(hash) = fields.next() {
            let oid = std::str::from_utf8(hash).map_err(|e| e.to_string())?;
            validate_oid(oid)?;
            let parents = fields.next().ok_or("Missing stash parents")?;
            let parents = std::str::from_utf8(parents).map_err(|err| err.to_string())?;
            let base = parents.split_whitespace().next().map(str::to_owned);
            if let Some(base) = &base {
                validate_oid(base)?;
            }
            let mut field = || {
                fields.next().map(String::from_utf8_lossy).ok_or("Incomplete stash entry")
            };
            let author = field()?.into_owned();
            let date = field()?.into_owned();
            let time = field()?.parse().unwrap_or(0);
            let message = String::from_utf8_lossy(fields.next().ok_or("Missing stash title")?).into_owned();
            let (title, branch) = named(&message);
            entries.push(Entry {
                oid: oid.to_owned(),
                title,
                message,
                branch,
                base,
                author,
                date,
                time,
            });
        }
        Ok(entries)
    }

    pub(crate) fn stash_position(&self, oid: &str) -> Result<(usize, Vec<Entry>), String> {
        validate_oid(oid)?;
        let entries = self.stashes()?;
        let position = entries
            .iter()
            .position(|entry| entry.oid == oid)
            .ok_or("This stash is no longer in the stash list.")?;
        if self.resolve(OsStr::new(&format!("stash@{{{position}}}")))? != oid {
            return Err("The stash list changed. Review it and try again.".into());
        }
        Ok((position, entries))
    }

    pub(crate) fn drop_at(&self, oid: &str, position: usize, count: usize) -> Result<(), String> {
        let mut drop = self.command();
        if count == 1 {
            drop.args(["update-ref", "-d", "refs/stash", oid]);
        } else {
            drop.args(["stash", "drop", "--quiet"]).arg(format!("stash@{{{position}}}"));
        }
        checked(&mut drop).map(|_| ())
    }

    fn store_stash(&self, oid: &str, message: &str) -> Result<(), String> {
        validate_oid(oid)?;
        checked(
            self.command()
                .args(["stash", "store", "--quiet"])
                .arg(format!("--message={message}"))
                .arg(oid),
        )
        .map(|_| ())
    }

    pub(crate) fn stash_untracked(&self, oid: &str) -> Result<Vec<Untracked>, String> {
        validate_oid(oid)?;
        let verify = |name: String| {
            self.read_command()
                .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
                .arg(name)
                .run()
                .map_err(|e| format!("Run bundled Git: {e}"))
        };
        let tree = verify(format!("{oid}^3^{{tree}}"))?;
        if !tree.status.success() {
            if !verify(format!("{oid}^{{commit}}"))?.status.success() {
                return Err("The stash's commit is no longer in the repository.".into());
            }
            return Ok(Vec::new());
        }
        let tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();
        validate_oid(&tree)?;
        let listing = checked(self.read_command().args(["ls-tree", "-r", "-z", "--full-tree", &tree]))?;
        listing
            .split(|&b| b == 0)
            .filter(|record| !record.is_empty())
            .map(|record| {
                let tab = record.iter().position(|&b| b == b'\t').ok_or("Invalid Git tree record")?;
                let fields: Vec<&[u8]> = record[..tab].split(|&b| b == b' ').collect();
                let [mode, _, oid] = fields.as_slice() else {
                    return Err("Invalid Git tree record".to_owned());
                };
                let oid = String::from_utf8_lossy(oid).into_owned();
                validate_oid(&oid)?;
                let path = raw_path(&record[tab + 1..]);
                validate_path(&path)?;
                Ok(Untracked { path, mode: String::from_utf8_lossy(mode).into_owned(), oid })
            })
            .collect()
    }

    pub(crate) fn remove_unchanged(&self, files: &[Untracked]) -> Result<usize, String> {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        if files.is_empty() {
            return Ok(0);
        }
        let tracked: BTreeSet<PathBuf> = checked(self.read_command().args(["ls-files", "-z"]))?
            .split(|&b| b == 0)
            .filter(|path| !path.is_empty())
            .map(raw_path)
            .collect();
        let mut regular = Vec::new();
        let mut matched = Vec::new();
        let mut kept = 0;
        let same = |bytes: Vec<u8>, oid: &str| -> Result<bool, String> {
            let hashed =
                crate::operation::with_input(self.read_command().args(["hash-object", "--no-filters", "--stdin"]), bytes)?;
            Ok(String::from_utf8_lossy(&hashed).trim() == oid)
        };
        for file in files.iter().filter(|file| !tracked.contains(&file.path)) {
            let at = self.root.join(&file.path);
            let Ok(meta) = std::fs::symlink_metadata(&at) else { continue };
            let name = file.path.as_os_str().as_bytes();
            let by_name = name.first() != Some(&b'"') && !name.contains(&b'\n') && !name.contains(&b'\r');
            let bytes = match file.mode.as_str() {
                "100644" | "100755" if meta.is_file() && by_name => {
                    regular.push(file);
                    continue;
                }
                "100644" | "100755" if meta.is_file() => std::fs::read(&at).map_err(|e| e.to_string())?,
                "120000" if meta.file_type().is_symlink() => {
                    std::fs::read_link(&at).map_err(|e| e.to_string())?.into_os_string().into_vec()
                }
                _ => {
                    kept += 1;
                    continue;
                }
            };
            if same(bytes, &file.oid)? {
                matched.push(file);
            } else {
                kept += 1;
            }
        }
        if !regular.is_empty() {
            let mut input = Vec::new();
            for file in &regular {
                input.extend_from_slice(file.path.as_os_str().as_bytes());
                input.push(b'\n');
            }
            let hashed =
                crate::operation::with_input(self.read_command().args(["hash-object", "--no-filters", "--stdin-paths"]), input)?;
            let hashed = String::from_utf8_lossy(&hashed);
            let hashes: Vec<&str> = hashed.lines().collect();
            if hashes.len() != regular.len() {
                return Err("Git did not hash every untracked file.".into());
            }
            for (file, hash) in regular.into_iter().zip(hashes) {
                if hash == file.oid {
                    matched.push(file);
                } else {
                    kept += 1;
                }
            }
        }
        for file in matched {
            std::fs::remove_file(self.root.join(&file.path)).map_err(|e| format!("Remove {}: {e}", file.path.display()))?;
            let mut folder = file.path.parent();
            while let Some(dir) = folder.filter(|dir| !dir.as_os_str().is_empty()) {
                if std::fs::remove_dir(self.root.join(dir)).is_err() {
                    break;
                }
                folder = dir.parent();
            }
        }
        Ok(kept)
    }

    fn stash_files(&self, oid: &str) -> Result<Vec<ChangedFile>, String> {
        validate_oid(oid)?;
        let output = checked(self.read_command().args([
            "stash",
            "show",
            "--include-untracked",
            "--no-color",
            "--find-renames",
            "--name-status",
            "-z",
            oid,
        ]))?;
        parse_changed_files(&output)
    }

    pub(crate) fn stash_diff(&self, oid: &str) -> Result<Diff, String> {
        validate_oid(oid)?;
        let mut cmd = self.read_command();
        cmd.args(["stash", "show", "--include-untracked", "--patch"])
            .args(patch_options(true))
            .arg(oid);
        read_diff(cmd, None, false)
    }

    pub(crate) fn stash_push(&self) -> std::process::Command {
        let mut push = self.command();
        for setting in [
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            push.env_remove(setting);
        }
        push.args(["stash", "push"]);
        push
    }

    fn save_stash(&self, paths: &[PathBuf]) -> Result<(), String> {
        let status = self.status()?;
        if let Some(kind) = status.operation.filter(|kind| kind.holds()) {
            return Err(format!("{}.", kind.refusal()));
        }
        if status.changed_paths == 0 {
            return Err("No changes to stash.".into());
        }
        let mut push = self.stash_push();
        push.arg("--include-untracked");
        if paths.is_empty() {
            return push_changes(&mut push);
        }
        let changed: BTreeSet<&Path> =
            status.staged.iter().chain(&status.unstaged).map(|file| file.path.as_path()).collect();
        for path in paths {
            validate_path(path)?;
            if !changed.contains(path.as_path()) {
                return Err(match paths {
                    [_] => "This file no longer has changes to stash.".to_owned(),
                    _ => format!("{} no longer has changes to stash.", path.display()),
                });
            }
        }
        let origins = with_origins(&status.unstaged, &with_origins(&status.staged, paths)).split_off(paths.len());
        let chosen: BTreeSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let pathspecs = nul_separated(paths, b":(top,literal)")?;
        push.args(["--pathspec-from-file=-", "--pathspec-file-nul"]);
        if status.staged.iter().all(|file| chosen.contains(file.path.as_path())) {
            push_changes_from(&mut push, pathspecs)?;
            return match &status.head {
                Some(head) => self.put_back(head, &origins),
                None => Ok(()),
            };
        }

        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let index = scratch.path().join("index");
        let head = status
            .head
            .ok_or("Make the first commit before stashing changes.")?;
        let rows: Vec<PathBuf> = status
            .staged
            .iter()
            .filter(|file| chosen.contains(file.path.as_path()))
            .map(|file| file.path.clone())
            .collect();
        let staged = with_origins(&status.staged, &rows);
        let wanted: BTreeSet<&Path> = staged.iter().map(PathBuf::as_path).collect();
        let stage_records = || -> Result<Vec<Vec<u8>>, String> {
            let listing = checked(self.read_command().args(["ls-files", "--stage", "-z"]))?;
            Ok(listing
                .split(|&b| b == 0)
                .filter(|record| {
                    record
                        .iter()
                        .position(|&b| b == b'\t')
                        .is_some_and(|tab| wanted.contains(raw_path(&record[tab + 1..]).as_path()))
                })
                .map(<[u8]>::to_vec)
                .collect())
        };
        let expected = stage_records()?;
        checked(
            self.command()
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", &head]),
        )?;
        if !staged.is_empty() {
            let tree = checked(self.command().arg("write-tree"))?;
            let tree = std::str::from_utf8(&tree)
                .map_err(|e| e.to_string())?
                .trim_end();
            validate_oid(tree)?;
            let mut scratch_index = self.command();
            scratch_index.env("GIT_INDEX_FILE", &index);
            self.paths_into(scratch_index, &["restore", "--staged", "--source", tree], &staged)?;
        }
        push_changes_from(push.env("GIT_INDEX_FILE", &index), pathspecs)?;
        if !staged.is_empty() {
            if stage_records()? != expected {
                return Err("Stash saved. The selected index entries changed during the operation and were kept.".into());
            }
            self.with_paths(&["reset", "--quiet", &head], &staged).map_err(|err| {
                format!("Stash saved, but the selected index entries could not be reset: {err}")
            })?;
        }
        self.put_back(&head, &origins)
    }

    fn put_back(&self, head: &str, origins: &[PathBuf]) -> Result<(), String> {
        if origins.is_empty() {
            return Ok(());
        }
        let failed = |err: String| format!("Stash saved, but the files the renames came from could not be put back: {err}");
        self.with_paths(&["reset", "--quiet", head], origins).map_err(failed)?;
        let absent: Vec<PathBuf> =
            origins.iter().filter(|path| self.root.join(path).symlink_metadata().is_err()).cloned().collect();
        if !absent.is_empty() {
            self.with_paths(&["restore", "--worktree"], &absent).map_err(failed)?;
        }
        Ok(())
    }

    fn push_stash(&self, push: &Push) -> Result<Outcome, Failure> {
        let status = self.status()?;
        if let Some(kind) = status.operation.filter(|kind| kind.holds()) {
            return Err(Failure::from(format!("{}.", kind.refusal())));
        }
        let message = push.message.trim();
        let mut command = self.stash_push();
        match &push.staged {
            None => {
                if status.changed_paths == 0 {
                    return Err(Failure::from("No changes to stash.".to_owned()));
                }
                command.arg("--include-untracked");
            }
            Some(seen) => {
                let now: BTreeSet<&Path> = status.staged.iter().map(|file| file.path.as_path()).collect();
                if now.is_empty() {
                    return Err(Failure::from("Nothing is staged, so there is nothing to stash on its own.".to_owned()));
                }
                if now != seen.iter().map(PathBuf::as_path).collect() {
                    return Err(Failure::from("The staged files changed. Review them and stash again.".to_owned()));
                }
                command.arg("--staged");
            }
        }
        if !message.is_empty() {
            command.arg(format!("--message={message}"));
        }
        let before = self.resolve(OsStr::new("refs/stash")).ok();
        if let Err(err) = push_changes(&mut command) {
            return Err(self.undo_staged_push(push, before.as_deref(), err));
        }
        let title = if !message.is_empty() {
            format!("Stashed {message}")
        } else if push.staged.is_some() {
            "Staged changes stashed".to_owned()
        } else {
            "Changes stashed".to_owned()
        };
        let detail = if push.staged.is_some() {
            "Only the staged changes. Unstaged edits and untracked files stay in your files."
        } else {
            ""
        };
        Ok(Outcome::titled(title, detail))
    }

    fn undo_staged_push(&self, push: &Push, before: Option<&str>, err: String) -> Failure {
        let after = self.resolve(OsStr::new("refs/stash")).ok();
        let Some(saved) = after.filter(|after| push.staged.is_some() && Some(after.as_str()) != before) else {
            return Failure::from(err);
        };
        let undone = match before {
            None => self.drop_at(&saved, 0, 1),
            Some(_) => match self.resolve(OsStr::new("stash@{0}")) {
                Ok(top) if top == saved => self.drop_at(&saved, 0, 2),
                _ => Err("The stash list changed.".to_owned()),
            },
        };
        let text = match undone {
            Err(why) => format!(
                "Stash Failed — Git saved the stash {} but could not take the staged changes out of your files, and the stash could not be removed: {why}",
                notice::short(&saved)
            ),
            Ok(()) if err.contains("Cannot remove worktree changes") => {
                "Stash Failed — The staged changes overlap unstaged edits in the same lines, so they cannot be stashed on their own. Nothing was stashed.".to_owned()
            }
            Ok(()) => "Stash Failed — Git could not stash only the staged changes. Nothing was stashed.".to_owned(),
        };
        Failure::from(text).output(err)
    }

    pub(crate) fn ready_to_apply(&self, oid: &str) -> Result<(), String> {
        let status = self.status()?;
        if let Some(kind) = status.operation.filter(|kind| kind.holds()) {
            return Err(format!("{}.", kind.refusal()));
        }
        if !status.staged.is_empty() {
            return Err(
                "Staged changes prevent applying this stash. Commit or unstage them first.".into(),
            );
        }
        let mut present = self.stash_untracked(oid)?;
        present.retain(|file| std::fs::symlink_metadata(self.root.join(&file.path)).is_ok());
        match present.as_slice() {
            [] => Ok(()),
            [one] => Err(format!(
                "{} is already in your files, and the stash holds an untracked copy of it. Move it aside, then try again.",
                one.path.display()
            )),
            [first, rest @ ..] => Err(format!(
                "{} and {} are already in your files, and the stash holds untracked copies of them. Move them aside, then try again.",
                first.path.display(),
                files(rest.len()).replace("file", "other file")
            )),
        }
    }

    fn run_apply(&self, oid: &str, pop: bool) -> Result<(), Failure> {
        let output = self
            .command()
            .args(["stash", "apply", oid])
            .stdin(std::process::Stdio::null())
            .run()
            .map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
        printed.push_str(&String::from_utf8_lossy(&output.stderr));
        let printed = printed.trim().to_owned();
        let conflicted = checked(self.read_command().args(["ls-files", "--unmerged", "-z"]))
            .and_then(|records| crate::operation::stage_paths(&records))
            .map_or(0, |paths| paths.len());
        let needs = match conflicted {
            0 => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let text = if stderr.is_empty() { printed.clone() } else { stderr };
                return Err(Failure::from(text).output(printed));
            }
            1 => "1 file needs".to_owned(),
            n => format!("{n} files need"),
        };
        let text = if pop {
            format!("{POP_STOPPED} {needs} resolving, and the stash was kept.")
        } else {
            format!("{APPLY_STOPPED} {needs} resolving. The stash stays in the list.")
        };
        Err(Failure::from(text).output(printed))
    }

    fn apply_stash(&self, oid: &str) -> Result<(), Failure> {
        validate_oid(oid)?;
        if !self.stashes()?.iter().any(|entry| entry.oid == oid) {
            return Err(Failure::from("This stash is no longer in the stash list.".to_owned()));
        }
        self.ready_to_apply(oid)?;
        self.run_apply(oid, false)
    }

    pub(crate) fn pop_stash(&self, expected: &str) -> Result<Outcome, Failure> {
        let (_, entries) = self.stash_position(expected)?;
        let title = entries.iter().find(|entry| entry.oid == expected).map(|entry| entry.title.clone());
        self.ready_to_apply(expected)?;
        self.run_apply(expected, true)?;
        let (position, current) = self
            .stash_position(expected)
            .map_err(|_| "Changes restored. The stash list changed, so the entry was kept.".to_owned())?;
        self.drop_at(expected, position, current.len()).map_err(|err| {
            format!("Changes restored, but the stash could not be dropped: {err}")
        })?;
        Ok(Outcome::titled(format!("Popped stash {}", title.unwrap_or_default()), ""))
    }

    fn drop_stash(&self, oid: &str) -> Result<Outcome, String> {
        let (position, entries) = self.stash_position(oid)?;
        let entry = &entries[position];
        self.drop_at(oid, position, entries.len())?;
        let restore = Action::Restore { oid: oid.to_owned(), message: entry.message.clone() };
        Ok(Outcome::titled(
            format!("Dropped stash {}", entry.title),
            format!("was stash@{{{position}}}, {}", notice::short(oid)),
        )
        .offer("Restore", view::Action::Stash(restore))
        .sticky())
    }

    fn restore_stash(&self, oid: &str, message: &str) -> Result<Outcome, String> {
        validate_oid(oid)?;
        let entries = self.stashes()?;
        if entries.iter().any(|entry| entry.oid == oid) {
            return Err("This stash is already in the stash list.".into());
        }
        self.store_stash(oid, message)?;
        let detail = if entries.is_empty() { "" } else { "It is back as the newest stash, stash@{0}." };
        Ok(Outcome::titled(format!("Restored stash {}", named(message).0), detail))
    }

    fn rename_stash(&self, oid: &str, text: &str) -> Result<Outcome, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Type a message for the stash.".into());
        }
        let (position, entries) = self.stash_position(oid)?;
        let entry = &entries[position];
        let message = match &entry.branch {
            Some(branch) => format!("On {branch}: {text}"),
            None => text.to_owned(),
        };
        if position == 0 {
            self.drop_at(oid, 0, entries.len())?;
            if let Err(err) = self.store_stash(oid, &message) {
                return Err(match self.store_stash(oid, &entry.message) {
                    Ok(()) => format!("The stash could not be renamed, so it keeps its name: {err}"),
                    Err(_) => format!("The stash could not be renamed or put back: {err}. Its object is {oid}."),
                });
            }
        } else {
            self.store_stash(oid, &message)?;
            let below = position + 1;
            match self.resolve(OsStr::new(&format!("stash@{{{below}}}"))) {
                Ok(at) if at == oid => self.drop_at(oid, below, entries.len() + 1).map_err(|err| {
                    format!("The renamed stash is at the top, but the old entry could not be dropped: {err}")
                })?,
                _ => return Err("The renamed stash is at the top. The stash list changed, so the old entry was kept.".into()),
            }
        }
        let detail = if position > 0 { "It moved to the top of the list, as stash@{0}." } else { "" };
        Ok(Outcome::titled(format!("Renamed stash to {text}"), detail))
    }
}

#[derive(Default)]
pub struct State {
    pub entries: Vec<Entry>,
    pub files: Vec<ChangedFile>,
    detail_oid: Option<String>,
    list_ticket: u64,
    placements: Vec<Placement>,
    options_open: bool,
    staged_only: bool,
    message: Option<Entity<CommitInput>>,
}

struct Placement {
    stash: usize,
    base: usize,
    row: usize,
}

pub(crate) enum HistoryItem {
    Commit(usize),
    Stash {
        stash: usize,
        base: usize,
        first_at_base: bool,
    },
}

impl State {
    pub fn title(&self, oid: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|s| s.oid == oid)
            .map(|s| s.title.as_str())
    }

    pub(crate) fn options_open(&self) -> bool {
        self.options_open
    }

    pub(crate) fn staged_only(&self) -> bool {
        self.staged_only
    }

    pub(crate) fn message(&self) -> Option<&Entity<CommitInput>> {
        self.message.as_ref()
    }

    pub fn place(&mut self, commits: &[Commit], offset: usize) {
        if offset == 0 {
            self.placements.clear();
        }
        if self.entries.is_empty() || self.entries.len() == self.placements.len() {
            return;
        }
        let mut by_base = HashMap::<&str, Vec<usize>>::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if let Some(base) = entry.base.as_deref() {
                by_base.entry(base).or_default().push(index);
            }
        }
        for (index, commit) in commits.iter().enumerate() {
            if let Some(stashes) = by_base.remove(commit.oid.as_str()) {
                let base = index + offset;
                for stash in stashes {
                    self.placements.push(Placement {
                        stash,
                        base,
                        row: base + self.placements.len(),
                    });
                }
            }
        }
    }

    pub fn placed(&self) -> usize {
        self.placements.len()
    }

    pub fn item(&self, row: usize) -> HistoryItem {
        match self.placements.binary_search_by_key(&row, |placement| placement.row) {
            Ok(index) => {
                let placement = &self.placements[index];
                HistoryItem::Stash {
                    stash: placement.stash,
                    base: placement.base,
                    first_at_base: index == 0 || self.placements[index - 1].base != placement.base,
                }
            }
            Err(preceding) => HistoryItem::Commit(row - preceding),
        }
    }

    pub fn commit_row(&self, commit: usize) -> usize {
        commit + self.placements.partition_point(|stash| stash.base <= commit)
    }

    pub fn stash_row(&self, oid: &str) -> Option<usize> {
        self.placements.iter().find(|stash| self.entries[stash.stash].oid == oid).map(|stash| stash.row)
    }

    pub fn at_commit(&self, commit: usize) -> bool {
        self.placements.binary_search_by_key(&commit, |stash| stash.base).is_ok()
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Save,
    SaveFiles(Vec<PathBuf>),
    ToggleOptions,
    Dismiss,
    ToggleStaged,
    Submit,
    Pop(String),
    Apply(String),
    Rename(String),
    Drop(String),
    DropNow(String),
    Restore { oid: String, message: String },
}

#[derive(Debug)]
pub(crate) enum Ask {
    Drop {
        oid: String,
        title: String,
        position: usize,
        when: String,
    },
    Rename {
        oid: String,
        title: String,
        position: usize,
    },
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::Drop { oid, title, position, when } => {
            Spec::new(format!("Drop stash {title}?"), "Drop stash")
                .line(format!("stash@{{{position}}}, {}, saved {when}.", notice::short(oid)))
                .line("Its changes leave the stash list. Restore, in the notice that follows, puts it back.")
                .danger()
        }
        Ask::Rename { title, position, .. } => {
            let mut spec = Spec::new(format!("Rename stash {title}"), "Rename");
            if *position > 0 {
                spec = spec.line(format!(
                    "It keeps its changes and moves from stash@{{{position}}} to the top of the list, as stash@{{0}}."
                ));
            }
            let current = title.clone();
            spec.field = Some(prompt::Field {
                label: "Message",
                placeholder: "Stash message",
                initial: title.clone(),
                masked: false,
                validate: Box::new(move |text, _| {
                    let text = text.trim();
                    (text.is_empty() || text == current).then(String::new)
                }),
            });
            spec
        }
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Drop { oid, .. } => tab.run_operation_then(
            cx,
            "Dropping the stash…",
            "Stash dropped",
            false,
            move |repo| repo.drop_stash(&oid),
            |_, _| {},
        ),
        Ask::Rename { oid, .. } => tab.run_operation_then(
            cx,
            "Renaming the stash…",
            "Stash renamed",
            false,
            move |repo| repo.rename_stash(&oid, &answer.text),
            |_, _| {},
        ),
    }
}

pub fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.stash.list_ticket += 1;
    let ticket = tab.ui.stash.list_ticket;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || repo.stashes(),
        move |tab, result, cx| {
            if epoch != tab.epoch || ticket != tab.ui.stash.list_ticket {
                return;
            }
            match result {
                Ok(entries) => {
                    tab.ui.stash.entries = entries;
                    tab.ui.stash.place(&tab.ui.history, 0);
                    if let Some(Selection::Stash(oid)) = &tab.ui.selection {
                        if let Some(title) = tab.ui.stash.title(oid) {
                            tab.ui.diff_title = title.to_owned();
                        } else {
                            selected(tab, None, cx);
                            tab.clear_diff();
                            tab.ui.selection = None;
                            tab.select_head = true;
                            tab.load_status(cx);
                        }
                    }
                }
                Err(err) => tab.error(err, cx),
            }
            cx.notify();
        },
    );
}

pub fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    if oid == tab.ui.stash.detail_oid.as_deref() {
        return;
    }
    tab.ui.stash.files.clear();
    tab.ui.stash.detail_oid = oid.map(str::to_owned);
    let (Some(oid), Some(repo)) = (oid.map(str::to_owned), tab.repo.clone()) else {
        return;
    };
    let epoch = tab.epoch;
    let selected = oid.clone();
    Tab::work(
        cx,
        move || repo.stash_files(&oid),
        move |tab, result, cx| {
            if epoch != tab.epoch || tab.ui.stash.detail_oid.as_deref() != Some(&selected) {
                return;
            }
            match result {
                Ok(files) => tab.ui.stash.files = files,
                Err(err) => {
                    tab.ui.stash.detail_oid = None;
                    tab.error(err, cx);
                }
            }
            cx.notify();
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::ToggleOptions => {
            let open = !tab.ui.stash.options_open && !tab.ui.busy() && !tab.ui.operation.active();
            tab.ui.stash.options_open = open;
            if open {
                let field = tab.ui.stash.message.get_or_insert_with(|| {
                    let input = cx.new(|cx| CommitInput::with_placeholder(cx, "Stash message (optional)"));
                    cx.observe(&input, |_, _, cx| cx.notify()).detach();
                    input
                });
                window.focus(&field.focus_handle(cx));
            } else {
                view::focus(&tab.ui, window);
            }
            cx.notify();
            return;
        }
        Action::Dismiss => {
            tab.ui.stash.options_open = false;
            cx.notify();
            return;
        }
        Action::ToggleStaged => {
            if tab.ui.stash.options_open {
                tab.ui.stash.staged_only = !tab.ui.stash.staged_only;
            }
            cx.notify();
            return;
        }
        Action::Submit => {
            if !tab.ui.stash.options_open || refusal(&tab.ui, tab.ui.stash.staged_only).is_some() {
                return;
            }
            let message = tab.ui.stash.message.as_ref().map(|m| m.read(cx).text().to_owned()).unwrap_or_default();
            let staged = tab.ui.stash.staged_only.then(|| staged_paths(&tab.ui));
            tab.ui.stash.options_open = false;
            view::focus(&tab.ui, window);
            push(tab, Push { message, staged }, cx);
            cx.notify();
            return;
        }
        _ => {}
    }
    tab.ui.stash.options_open = false;
    match action {
        Action::Save | Action::SaveFiles(_) => tab.run_operation_then(
            cx,
            "Stashing changes…",
            "Changes stashed",
            false,
            move |repo| match action {
                Action::SaveFiles(paths) => repo.save_stash(&paths),
                _ => repo.save_stash(&[]),
            },
            |_, _| {},
        ),
        Action::Apply(ref oid) | Action::Pop(ref oid) => {
            let (pop, oid) = (matches!(action, Action::Pop(_)), oid.clone());
            let stashing = remember(tab, &oid, pop);
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            tab.run_operation_or(
                cx,
                if pop { "Popping the stash…" } else { "Applying the stash…" },
                if pop { "Stash popped" } else { "Stash applied" },
                false,
                move |repo| {
                    let result = if pop { repo.pop_stash(&oid) } else { repo.apply_stash(&oid).map(Outcome::from) };
                    result.map_err(|failure| {
                        stop.store(conflicted(&failure), Ordering::Relaxed);
                        offer_unstage(failure)
                    })
                },
                |_, _| {},
                move |tab, _| {
                    if stopped.load(Ordering::Relaxed) {
                        tab.ui.operation.stash = stashing;
                    }
                },
            )
        }
        Action::DropNow(oid) => tab.run_operation_then(
            cx,
            "Dropping the stash…",
            "Stash dropped",
            false,
            move |repo| repo.drop_stash(&oid),
            |_, _| {},
        ),
        Action::Restore { oid, message } => tab.run_operation_then(
            cx,
            "Restoring the stash…",
            "Stash restored",
            false,
            move |repo| repo.restore_stash(&oid, &message),
            |_, _| {},
        ),
        Action::Rename(oid) => ask_about(tab, oid, false, cx),
        Action::Drop(oid) => ask_about(tab, oid, true, cx),
        Action::ToggleOptions | Action::Dismiss | Action::ToggleStaged | Action::Submit => {}
    }
    cx.notify();
}

fn remember(tab: &Tab, oid: &str, pop: bool) -> Option<crate::operation::Stashing> {
    if tab.ui.busy() {
        return None;
    }
    let title = tab.ui.stash.title(oid).map_or_else(|| notice::short(oid).to_owned(), str::to_owned);
    Some(crate::operation::Stashing { oid: oid.to_owned(), title, pop })
}

fn conflicted(failure: &Failure) -> bool {
    [APPLY_STOPPED, POP_STOPPED].iter().any(|start| failure.text.starts_with(start))
}

const APPLY_STOPPED: &str = "Applying the stash stopped on a conflict.";
const POP_STOPPED: &str = "Popping the stash stopped on a conflict.";

fn ask_about(tab: &mut Tab, oid: String, drop: bool, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    let Some(position) = tab.ui.stash.entries.iter().position(|entry| entry.oid == oid) else {
        return;
    };
    let entry = &tab.ui.stash.entries[position];
    let title = entry.title.clone();
    let ask = if drop {
        let when = view::when(entry.time, &entry.date, view::now());
        Ask::Drop { oid, title, position, when }
    } else {
        Ask::Rename { oid, title, position }
    };
    tab.ui.menu = None;
    prompt::ask(&mut tab.ui, prompt::Kind::Stash(ask), cx);
}

fn push(tab: &mut Tab, how: Push, cx: &mut Context<Tab>) {
    let progress = if how.staged.is_some() { "Stashing the staged changes…" } else { "Stashing changes…" };
    tab.run_operation_then(
        cx,
        progress,
        "Changes stashed",
        false,
        move |repo| repo.push_stash(&how),
        |tab, cx| {
            tab.ui.stash.staged_only = false;
            if let Some(field) = &tab.ui.stash.message {
                field.update(cx, |field, cx| field.clear(cx));
            }
        },
    );
}

fn offer_unstage(failure: Failure) -> Failure {
    if failure.text.starts_with("Staged changes prevent applying") {
        failure.offer("Unstage all", view::Action::UnstageAll)
    } else {
        failure
    }
}

pub fn toolbar<T: Desk>(app: &T, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let ui = app.ui();
    let open = ui.stash.options_open();
    let divider = || div().w(px(1.)).h(px(CONTROL)).bg(rgb(LINE_STRONG));
    let reason = refusal(ui, false);
    let mut button = kit::btn("stash-worktree", "Stash")
        .ghost()
        .icon("stash")
        .enabled(reason.is_none());
    button = button.tip(match reason {
        Some(reason) if !ui.busy() => reason,
        _ => crate::commands::tip_with_keys("Stash all changes", "repo.stash"),
    });
    div()
        .relative()
        .flex()
        .items_center()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE_STRONG))
        .bg(rgb(RAISED))
        .child(button.on(view::Action::Stash(Action::Save), cx))
        .child(divider())
        .child(
            kit::btn("stash-options", "")
                .ghost()
                .icon("chevron-down")
                .selected(open)
                .enabled(!ui.busy() && !ui.operation.active())
                .tip("Stash with a message, or pop the latest stash")
                .on(view::Action::Stash(Action::ToggleOptions), cx)
                .px(px(5.)),
        )
        .when(open, |d| d.child(kit::popover(options(app, cx), true)))
}

fn options<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.stash;
    let staged = staged_paths(ui);
    let only = state.staged_only;
    let reason = refusal(ui, only);
    let label = if only {
        match staged.len() {
            0 => "Stash staged changes".to_owned(),
            1 => "Stash 1 staged file".to_owned(),
            n => format!("Stash {n} staged files"),
        }
    } else {
        match ui.changed_count() {
            0 => "Stash changes".to_owned(),
            n => format!("Stash {}", files(n)),
        }
    };
    let mut form = kit::float()
        .w(px(300.))
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Stash changes"),
        );
    if let Some(field) = &state.message {
        form = form.child(kit::field(
            "Message",
            div().child(field.clone()).on_key_down(cx.listener(|app, e: &KeyDownEvent, w, cx| {
                if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
                    app.dispatch(view::Action::Stash(Action::Submit), w, cx);
                    cx.stop_propagation();
                }
            })),
            Some(format!("Without one the stash is called {}.", unnamed(&ui.branch, ui.detached)).into()),
        ));
    }
    let tick = div()
        .id("stash-staged-only")
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_CHIP))
        .text_size(px(12.5))
        .child(kit::checkbox(only))
        .child("Staged changes only");
    form = form.child(if staged.is_empty() && !only {
        tick.text_color(rgb(TEXT_OFF)).child(kit::muted("Nothing is staged").text_size(px(11.5)))
    } else {
        kit::pressable(
            tick.cursor_pointer()
                .text_color(rgb(TEXT_2))
                .focusable()
                .focus(|s| s.text_color(rgb(TEXT))),
            view::Action::Stash(Action::ToggleStaged),
            cx,
        )
    });
    if only {
        form = form.child(kit::muted("Unstaged edits and untracked files stay in your files.").text_size(px(11.5)));
    }
    let mut submit = kit::btn("stash-submit", label).primary().wide().enabled(reason.is_none());
    if let Some(reason) = reason {
        submit = submit.tip(reason);
    }
    form = form.child(submit.on(view::Action::Stash(Action::Submit), cx));
    if let Some(latest) = state.entries.first() {
        form = form.child(
            div()
                .mx(px(-12.))
                .mb(px(-12.))
                .py_1()
                .border_t_1()
                .border_color(rgb(LINE))
                .child(
                    kit::item("stash-pop-latest", "Pop latest stash")
                        .icon("stash")
                        .meta(crate::commands::keys("alt-shift-cmd-s"))
                        .tip(Some(latest.title.clone().into()))
                        .enabled(index_edit(ui).or_else(|| crate::commands::operating(ui)).is_none())
                        .on(view::Action::Stash(Action::Pop(latest.oid.clone())), cx),
                ),
        );
    }
    form
}

pub fn file_button<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let mut row = div();
    if let Some(Selection::File { path, .. }) = &app.ui().selection {
        row = row.child(
            kit::btn("stash-file", "Stash file")
                .small()
                .ghost()
                .enabled(app.ui().can_edit_index())
                .on(view::Action::Stash(Action::SaveFiles(vec![path.clone()])), cx),
        );
    }
    row
}

fn whereabouts(position: usize, entry: &Entry, now: i64) -> String {
    let mut text = format!("stash@{{{position}}}");
    if let Some(branch) = &entry.branch {
        text.push_str(&format!(" · On {branch}"));
    }
    text.push_str(&format!(" · {}", view::when(entry.time, &entry.date, now)));
    text
}

pub fn details<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let Some(Selection::Stash(oid)) = &ui.selection else {
        return div();
    };
    let position = ui.stash.entries.iter().position(|s| &s.oid == oid);
    let writable = ui.can_edit_index() && !ui.operation.active();
    let n = ui.stash.files.len();
    let files = uniform_list(
        "stashed-files",
        n,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let Some(Selection::Stash(stash)) = &app.ui().selection else {
                return Vec::new();
            };
            range
                .filter_map(|idx| {
                    let file = app.ui().stash.files.get(idx)?;
                    let target = crate::commands::Target::File {
                        path: file.path.clone(),
                        side: crate::commands::Side::Stash(stash.clone()),
                    };
                    Some(
                        div()
                            .id(("stashed-file", idx))
                            .h(px(FILE_ROW))
                            .w_full()
                            .pl_3()
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                                    let target = target.clone();
                                    app.dispatch(
                                        view::Action::Menu(Some(view::Menu::Object { target, at: event.position })),
                                        w,
                                        cx,
                                    )
                                }),
                            )
                            .child(kit::file_label(
                                ("stashed-file-label", idx),
                                file.status,
                                &file.path,
                                file.from.as_deref(),
                            )),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .flex_1()
    .min_h_0();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(ui.stash.title(oid).unwrap_or("Stash").to_owned()),
                )
                .when_some(position, |d, i| {
                    let entry = &ui.stash.entries[i];
                    d.child(kit::muted(whereabouts(i, entry, view::now())).font_family(MONO))
                })
                .child(
                    div()
                        .pt_1()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(
                            kit::btn("apply-stash", "Apply")
                                .small()
                                .enabled(writable)
                                .tip("Restore these changes and keep the stash")
                                .on(view::Action::Stash(Action::Apply(oid.clone())), cx),
                        )
                        .child(
                            kit::btn("pop-stash", "Pop")
                                .small()
                                .enabled(writable)
                                .tip("Restore these changes and remove the stash")
                                .on(view::Action::Stash(Action::Pop(oid.clone())), cx),
                        )
                        .child(
                            kit::btn("stash-patch", "View changes")
                                .small()
                                .ghost()
                                .on(view::Action::OpenDiff, cx),
                        )
                        .child(
                            kit::btn("rename-stash", "Rename…")
                                .small()
                                .ghost()
                                .enabled(!ui.busy() && !ui.operation.active())
                                .on(view::Action::Stash(Action::Rename(oid.clone())), cx),
                        )
                        .child(
                            kit::btn("drop-stash", "Drop…")
                                .small()
                                .ghost()
                                .danger()
                                .enabled(!ui.busy() && !ui.operation.active())
                                .tip("Remove this stash from the list")
                                .on(view::Action::Stash(Action::Drop(oid.clone())), cx),
                        ),
                ),
        )
        .child(
            div()
                .h(px(28.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .child(kit::label(self::files(n))),
        )
        .child(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", name])).unwrap().trim_end().to_owned()
    }

    fn exists(f: &Fixture, name: &str) -> bool {
        checked(f.repo.command().args(["rev-parse", "--verify", "--quiet", name])).is_ok()
    }

    fn listed(f: &Fixture) -> String {
        String::from_utf8(f.git(&["stash", "list", "--format=%H %gs"])).unwrap()
    }

    fn stacked(names: &[&str]) -> (Fixture, Vec<String>) {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        let mut oids = Vec::new();
        for name in names {
            fs::write(f.dir.join(name), name).unwrap();
            f.repo.push_stash(&Push { message: (*name).to_owned(), staged: None }).unwrap();
            oids.push(rev(&f, "refs/stash"));
        }
        (f, oids)
    }

    #[test]
    fn stashes_follow_their_first_parent_across_history_pages() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-m", "Base"]);
        for title in ["First at base", "Second at base"] {
            fs::write(f.dir.join("note"), title).unwrap();
            f.git(&["stash", "push", "--include-untracked", "-m", title]);
        }
        f.git(&["commit", "--allow-empty", "-m", "Tip"]);
        fs::write(f.dir.join("note"), "At tip").unwrap();
        f.git(&["stash", "push", "--include-untracked", "-m", "At tip"]);

        let commits = f.repo.history(0, 2).unwrap();
        let mut state = State { entries: f.repo.stashes().unwrap(), ..Default::default() };
        assert_eq!(state.entries[0].base.as_deref(), Some(commits[0].oid.as_str()));
        assert_eq!(state.entries[1].base.as_deref(), Some(commits[1].oid.as_str()));
        state.place(&commits[..1], 0);
        assert_eq!(state.commit_row(0), 1);
        assert_eq!(state.stash_row(&state.entries[1].oid), None);

        state.place(&commits[1..], 1);
        assert_eq!(state.placed(), 3);
        assert!(matches!(state.item(1), HistoryItem::Commit(0)));
        assert!(matches!(state.item(2), HistoryItem::Stash { stash: 1, base: 1, first_at_base: true }));
        assert!(matches!(state.item(3), HistoryItem::Stash { stash: 2, base: 1, first_at_base: false }));
        assert_eq!(state.stash_row(&state.entries[1].oid), Some(2));
        assert_eq!(state.commit_row(1), 4);
        assert!(matches!(state.item(4), HistoryItem::Commit(1)));
    }

    #[test]
    fn a_stash_message_becomes_its_title_and_git_s_default_names_the_branch() {
        assert_eq!(named("On main: Draft the gate"), ("Draft the gate".to_owned(), Some("main".to_owned())));
        assert_eq!(named("WIP on main: abc1234 Start"), ("WIP on main".to_owned(), Some("main".to_owned())));
        assert_eq!(named("On feature/x: On a: b"), ("On a: b".to_owned(), Some("feature/x".to_owned())));
        assert_eq!(named("On (no branch): Field notes").1.as_deref(), Some("(no branch)"));
        assert_eq!(named("autostash"), ("autostash".to_owned(), None));
        assert_eq!(named("On main: "), ("On main: ".to_owned(), None));
    }

    #[test]
    fn the_form_names_the_title_git_gives_an_unnamed_stash() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        for detach in [false, true] {
            if detach {
                f.git(&["switch", "-q", "--detach"]);
            }
            let status = f.repo.status().unwrap();
            fs::write(f.dir.join("notes"), "edit").unwrap();
            f.repo.push_stash(&Push { message: String::new(), staged: None }).unwrap();
            assert_eq!(f.repo.stashes().unwrap()[0].title, unnamed(&status.branch, status.detached));
        }
        assert_eq!(f.repo.stashes().unwrap()[0].title, "WIP on (no branch)");
    }

    #[test]
    fn single_path_preserves_the_index_and_worktree_of_another_partially_staged_file() {
        let f = Fixture::new();
        let path = Path::new(":(glob)*\nnotes");
        let work = f.dir.join(path);
        fs::write(&work, "base\n").unwrap();
        fs::write(f.dir.join("other"), "base\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(&work, "note staged\n").unwrap();
        fs::write(f.dir.join("other"), "other staged\n").unwrap();
        f.git(&["add", "."]);
        fs::write(&work, "note working\n").unwrap();
        fs::write(f.dir.join("other"), "other working\n").unwrap();
        f.repo.save_stash(&[path.to_path_buf()]).unwrap();
        let stash = &f.repo.stashes().unwrap()[0].oid;
        assert_eq!(
            f.repo.stash_files(stash).unwrap(),
            [ChangedFile {
                path: path.into(),
                status: "Modified",
                from: None
            }]
        );
        assert_eq!(f.git(&["show", "stash^2:other"]), b"base\n");
        assert_eq!(
            f.git(&["show", "stash^2::(glob)*\nnotes"]),
            b"note staged\n"
        );
        assert_eq!(f.git(&["show", "stash::(glob)*\nnotes"]), b"note working\n");
        assert_eq!(f.git(&["show", ":0::(glob)*\nnotes"]), b"base\n");
        assert_eq!(fs::read(&work).unwrap(), b"base\n");
        assert_eq!(f.git(&["show", ":other"]), b"other staged\n");
        assert_eq!(fs::read(f.dir.join("other")).unwrap(), b"other working\n");
    }

    #[test]
    fn several_paths_stash_only_those_and_keep_the_staged_changes_of_other_files() {
        let f = Fixture::new();
        for name in ["a.txt", "b c.txt", "other.txt", "left.txt"] {
            fs::write(f.dir.join(name), "base\n").unwrap();
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("a.txt"), "a staged\n").unwrap();
        fs::write(f.dir.join("other.txt"), "other staged\n").unwrap();
        f.git(&["add", "--", "a.txt", "other.txt"]);
        fs::write(f.dir.join("a.txt"), "a working\n").unwrap();
        fs::write(f.dir.join("b c.txt"), "b working\n").unwrap();
        fs::write(f.dir.join("left.txt"), "left working\n").unwrap();
        fs::write(f.dir.join("new é.txt"), "new\n").unwrap();
        let chosen = [PathBuf::from("a.txt"), PathBuf::from("b c.txt"), PathBuf::from("new é.txt")];
        f.repo.save_stash(&chosen).unwrap();

        let stash = f.repo.stashes().unwrap()[0].oid.clone();
        let mut files: Vec<_> = f.repo.stash_files(&stash).unwrap().into_iter().map(|file| file.path).collect();
        files.sort();
        assert_eq!(files, chosen);
        assert_eq!(f.git(&["show", "stash^2:a.txt"]), b"a staged\n");
        assert_eq!(f.git(&["show", "stash^2:other.txt"]), b"base\n");
        assert_eq!(f.git(&["show", "stash:b c.txt"]), b"b working\n");
        assert_eq!(f.git(&["show", "stash^3:new é.txt"]), b"new\n");
        assert_eq!(f.git(&["show", ":a.txt"]), b"base\n");
        assert_eq!(f.git(&["show", ":other.txt"]), b"other staged\n");
        assert_eq!(fs::read(f.dir.join("a.txt")).unwrap(), b"base\n");
        assert_eq!(fs::read(f.dir.join("b c.txt")).unwrap(), b"base\n");
        assert!(!f.dir.join("new é.txt").exists());
        assert_eq!(fs::read(f.dir.join("left.txt")).unwrap(), b"left working\n");

        f.git(&["reset", "-q", "--", "other.txt"]);
        f.git(&["stash", "apply", "-q", "--index", &stash]);
        assert_eq!(f.git(&["show", ":a.txt"]), b"a staged\n");
        assert_eq!(fs::read(f.dir.join("a.txt")).unwrap(), b"a working\n");
        assert_eq!(fs::read(f.dir.join("new é.txt")).unwrap(), b"new\n");
    }

    #[test]
    fn stashing_several_paths_refuses_one_that_has_no_changes_and_stashes_nothing() {
        let f = Fixture::new();
        for name in ["a.txt", "clean.txt"] {
            fs::write(f.dir.join(name), "base\n").unwrap();
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("a.txt"), "edited\n").unwrap();
        let err = f.repo.save_stash(&[PathBuf::from("a.txt"), PathBuf::from("clean.txt")]).unwrap_err();
        assert_eq!(err, "clean.txt no longer has changes to stash.");
        assert!(f.repo.stashes().unwrap().is_empty());
        assert_eq!(fs::read(f.dir.join("a.txt")).unwrap(), b"edited\n");
    }

    #[test]
    fn a_named_stash_keeps_its_message_and_takes_untracked_files() {
        let f = Fixture::new();
        fs::write(f.dir.join("notes"), "base\n").unwrap();
        f.git(&["add", "notes"]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("notes"), "edited\n").unwrap();
        fs::write(f.dir.join("loose"), "new\n").unwrap();
        let outcome = f.repo.push_stash(&Push { message: "  --Draft the gate  ".into(), staged: None }).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Stashed --Draft the gate", ""));
        let entries = f.repo.stashes().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].title.as_str(), entries[0].message.as_str()), ("--Draft the gate", "On main: --Draft the gate"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert_eq!(f.git(&["show", "stash^3:loose"]), b"new\n");
        fs::write(f.dir.join("notes"), "again\n").unwrap();
        let outcome = f.repo.push_stash(&Push { message: " ".into(), staged: None }).unwrap();
        assert_eq!(outcome.title, "Changes stashed");
        assert_eq!(f.repo.stashes().unwrap()[0].title, "WIP on main");
        assert_eq!(f.repo.push_stash(&Push { message: "x".into(), staged: None }).unwrap_err().text, "No changes to stash.");
    }

    #[test]
    fn a_staged_only_stash_leaves_unstaged_edits_and_untracked_files() {
        let f = Fixture::new();
        fs::write(f.dir.join("gate"), (1..=20).map(|n| format!("{n}\n")).collect::<String>()).unwrap();
        fs::write(f.dir.join("other"), "keep\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        let gate = fs::read_to_string(f.dir.join("gate")).unwrap();
        fs::write(f.dir.join("gate"), gate.replacen("1\n", "one\n", 1)).unwrap();
        f.git(&["add", "gate"]);
        fs::write(f.dir.join("gate"), gate.replacen("1\n", "one\n", 1).replace("20\n", "twenty\n")).unwrap();
        fs::write(f.dir.join("other"), "keep\nedit\n").unwrap();
        fs::write(f.dir.join("loose"), "new\n").unwrap();
        let push = Push { message: "Gate".into(), staged: Some(vec!["gate".into()]) };
        let outcome = f.repo.push_stash(&push).unwrap();
        assert_eq!(outcome.title, "Stashed Gate");
        assert_eq!(outcome.detail, "Only the staged changes. Unstaged edits and untracked files stay in your files.");
        assert_eq!(f.repo.stashes().unwrap()[0].title, "Gate");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" M gate\n M other\n?? loose\n");
        let now = fs::read_to_string(f.dir.join("gate")).unwrap();
        assert!(now.starts_with("1\n") && now.ends_with("twenty\n"), "{now}");
        assert_eq!(fs::read(f.dir.join("other")).unwrap(), b"keep\nedit\n");
        assert_eq!(f.git(&["diff", "--stat", "stash^1", "stash", "--", "other"]), b"");
        assert!(String::from_utf8(f.git(&["show", "stash:gate"])).unwrap().starts_with("one\n"));
        assert!(!exists(&f, "stash^3"));
    }

    #[test]
    fn a_staged_only_stash_refuses_when_the_staged_files_changed_or_none_are() {
        let f = Fixture::new();
        fs::write(f.dir.join("gate"), "base\n").unwrap();
        fs::write(f.dir.join("ford"), "base\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("gate"), "staged\n").unwrap();
        fs::write(f.dir.join("ford"), "staged\n").unwrap();
        f.git(&["add", "gate"]);
        let index = f.git(&["ls-files", "--stage"]);
        f.git(&["add", "ford"]);
        let seen = Push { message: String::new(), staged: Some(vec!["gate".into()]) };
        let failure = f.repo.push_stash(&seen).unwrap_err();
        assert_eq!(failure.text, "The staged files changed. Review them and stash again.");
        assert_eq!(listed(&f), "");
        f.git(&["restore", "--staged", "ford"]);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        f.git(&["restore", "--staged", "gate"]);
        let failure = f.repo.push_stash(&Push { message: String::new(), staged: Some(Vec::new()) }).unwrap_err();
        assert_eq!(failure.text, "Nothing is staged, so there is nothing to stash on its own.");
        assert_eq!(listed(&f), "");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" M ford\n M gate\n");
    }

    #[test]
    fn a_staged_only_stash_that_overlaps_unstaged_edits_leaves_everything_as_it_was() {
        let (f, oids) = stacked(&["older"]);
        fs::write(f.dir.join("gate"), "base\n").unwrap();
        f.git(&["add", "gate"]);
        f.git(&["commit", "-qm", "gate"]);
        fs::write(f.dir.join("gate"), "staged\n").unwrap();
        f.git(&["add", "gate"]);
        fs::write(f.dir.join("gate"), "working\n").unwrap();
        let index = f.git(&["ls-files", "--stage"]);
        let list = listed(&f);
        let failure = f.repo.push_stash(&Push { message: "Gate".into(), staged: Some(vec!["gate".into()]) }).unwrap_err();
        assert_eq!(
            failure.text,
            "Stash Failed — The staged changes overlap unstaged edits in the same lines, so they cannot be stashed on their own. Nothing was stashed."
        );
        assert!(failure.output.as_deref().is_some_and(|output| output.contains("Cannot remove worktree changes")));
        assert_eq!(listed(&f), list);
        assert_eq!(rev(&f, "refs/stash"), oids[0]);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(fs::read(f.dir.join("gate")).unwrap(), b"working\n");
        f.git(&["stash", "drop", "-q"]);
        let failure = f.repo.push_stash(&Push { message: String::new(), staged: Some(vec!["gate".into()]) }).unwrap_err();
        assert!(failure.text.ends_with("Nothing was stashed."), "{}", failure.text);
        assert!(!exists(&f, "refs/stash"));
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
    }

    #[test]
    fn apply_uses_saved_object_and_pop_takes_any_stash() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        fs::write(f.dir.join("first"), b"older").unwrap();
        f.repo.save_stash(&[]).unwrap();
        let old = f.repo.stashes().unwrap()[0].oid.clone();
        fs::write(f.dir.join("second"), b"newer").unwrap();
        f.repo.save_stash(&[]).unwrap();
        let list = f.repo.stashes().unwrap();
        assert_eq!(list.len(), 2);
        f.repo.apply_stash(&old).unwrap();
        assert_eq!(fs::read(f.dir.join("first")).unwrap(), b"older");
        assert!(!f.dir.join("second").exists());
        assert_eq!(f.repo.stashes().unwrap(), list);
        f.git(&["clean", "-qfd"]);
        let outcome = f.repo.pop_stash(&old).unwrap();
        assert_eq!(outcome.title, "Popped stash WIP on main");
        assert_eq!(fs::read(f.dir.join("first")).unwrap(), b"older");
        assert_eq!(f.repo.stashes().unwrap(), list[..1]);
        assert!(f.repo.apply_stash(&old).unwrap_err().text.contains("no longer"));
        assert_eq!(f.repo.pop_stash(&old).unwrap_err().text, "This stash is no longer in the stash list.");
    }

    #[test]
    fn popping_each_entry_preserves_the_older_stash_until_it_is_restored() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        for name in ["first", "second"] {
            fs::write(f.dir.join(name), name).unwrap();
            f.repo.save_stash(&[]).unwrap();
        }
        let entries = f.repo.stashes().unwrap();
        assert_eq!(entries.len(), 2);
        f.repo.pop_stash(&entries[0].oid).unwrap();
        assert_eq!(f.repo.stashes().unwrap(), entries[1..]);
        assert_eq!(fs::read(f.dir.join("second")).unwrap(), b"second");
        assert!(!f.dir.join("first").exists());
        f.repo.pop_stash(&entries[1].oid).unwrap();
        assert!(f.repo.stashes().unwrap().is_empty());
        assert_eq!(fs::read(f.dir.join("first")).unwrap(), b"first");
    }

    #[test]
    fn a_position_that_no_longer_names_the_stash_is_refused() {
        let (f, oids) = stacked(&["first", "second", "third"]);
        assert_eq!(f.repo.stash_position(&oids[0]).unwrap().0, 2);
        assert_eq!(f.repo.stash_position(&oids[2]).unwrap().0, 0);
        f.git(&["stash", "drop", "-q", "stash@{1}"]);
        assert_eq!(f.repo.stash_position(&oids[0]).unwrap().0, 1);
        assert_eq!(f.repo.stash_position(&oids[1]).unwrap_err(), "This stash is no longer in the stash list.");
        assert!(f.repo.stash_position("not an object").is_err());
    }

    #[test]
    fn dropping_an_older_stash_keeps_the_others_in_order_and_restore_brings_it_back() {
        let (f, oids) = stacked(&["first", "second", "third"]);
        let head = rev(&f, "HEAD");
        let outcome = f.repo.drop_stash(&oids[1]).unwrap();
        assert_eq!(
            (outcome.title.as_str(), outcome.detail.as_str()),
            ("Dropped stash second", format!("was stash@{{1}}, {}", &oids[1][..7]).as_str())
        );
        assert!(outcome.sticky);
        let offers: Vec<_> = outcome.actions.iter().map(|(label, _)| label.to_string()).collect();
        assert_eq!(offers, ["Restore"]);
        assert_eq!(listed(&f), format!("{} On main: third\n{} On main: first\n", oids[2], oids[0]));
        assert_eq!(rev(&f, "HEAD"), head);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");

        let Some((_, view::Action::Stash(Action::Restore { oid, message }))) = outcome.actions.into_iter().next() else {
            panic!("Restore is not a stash restore");
        };
        assert_eq!((oid.as_str(), message.as_str()), (oids[1].as_str(), "On main: second"));
        let outcome = f.repo.restore_stash(&oid, &message).unwrap();
        assert_eq!(
            (outcome.title.as_str(), outcome.detail.as_str()),
            ("Restored stash second", "It is back as the newest stash, stash@{0}.")
        );
        assert_eq!(
            listed(&f),
            format!("{} On main: second\n{} On main: third\n{} On main: first\n", oids[1], oids[2], oids[0])
        );
        assert_eq!(f.repo.restore_stash(&oid, &message).unwrap_err(), "This stash is already in the stash list.");
        assert!(f.repo.restore_stash(&head, "On main: not a stash").is_err());
        assert_eq!(f.repo.stashes().unwrap().len(), 3);
    }

    #[test]
    fn dropping_the_last_stash_removes_the_ref_and_a_gone_stash_is_refused() {
        let (f, oids) = stacked(&["only"]);
        f.repo.drop_stash(&oids[0]).unwrap();
        assert!(!exists(&f, "refs/stash"));
        assert_eq!(f.repo.drop_stash(&oids[0]).unwrap_err(), "This stash is no longer in the stash list.");
        f.repo.restore_stash(&oids[0], "On main: only").unwrap();
        assert_eq!(listed(&f), format!("{} On main: only\n", oids[0]));
    }

    #[test]
    fn renaming_moves_the_stash_to_the_top_under_its_new_message() {
        let (f, oids) = stacked(&["first", "second", "third"]);
        let outcome = f.repo.rename_stash(&oids[0], "  Field notes ").unwrap();
        assert_eq!(
            (outcome.title.as_str(), outcome.detail.as_str()),
            ("Renamed stash to Field notes", "It moved to the top of the list, as stash@{0}.")
        );
        assert_eq!(
            listed(&f),
            format!("{} On main: Field notes\n{} On main: third\n{} On main: second\n", oids[0], oids[2], oids[1])
        );
        let outcome = f.repo.rename_stash(&oids[0], "Survey notes").unwrap();
        assert_eq!(outcome.detail, "");
        assert_eq!(
            listed(&f),
            format!("{} On main: Survey notes\n{} On main: third\n{} On main: second\n", oids[0], oids[2], oids[1])
        );
        let (f, oids) = stacked(&["only"]);
        f.repo.rename_stash(&oids[0], "Alone").unwrap();
        assert_eq!(listed(&f), format!("{} On main: Alone\n", oids[0]));
        assert_eq!(f.repo.rename_stash(&oids[0], " ").unwrap_err(), "Type a message for the stash.");
        f.git(&["stash", "drop", "-q"]);
        f.git(&["stash", "store", "-m", "autostash", &oids[0]]);
        f.repo.rename_stash(&oids[0], "Kept").unwrap();
        assert_eq!(listed(&f), format!("{} Kept\n", oids[0]));
    }

    #[test]
    fn renaming_a_stash_that_went_or_moved_is_refused() {
        let (f, oids) = stacked(&["first", "second"]);
        f.git(&["stash", "drop", "-q", "stash@{1}"]);
        let list = listed(&f);
        assert_eq!(f.repo.rename_stash(&oids[0], "Gone").unwrap_err(), "This stash is no longer in the stash list.");
        assert_eq!(listed(&f), list);
        assert_eq!(f.repo.drop_stash(&oids[0]).unwrap_err(), "This stash is no longer in the stash list.");
        assert_eq!(listed(&f), list);
    }

    #[test]
    fn dirty_submodule_does_not_report_a_successful_stash() {
        let f = Fixture::new();
        f.git(&["init", "-q", "-b", "main", "vendor"]);
        fs::write(f.dir.join("vendor/notes"), "base").unwrap();
        f.git(&["-C", "vendor", "add", "notes"]);
        f.git(&[
            "-C",
            "vendor",
            "-c",
            "user.name=Cedar",
            "-c",
            "user.email=cedar@example.invalid",
            "commit",
            "-qm",
            "base",
        ]);
        f.git(&["add", "vendor"]);
        f.git(&["commit", "-qm", "vendor"]);
        fs::write(f.dir.join("vendor/notes"), "dirty").unwrap();
        assert_eq!(f.repo.status().unwrap().changed_paths, 1);
        assert!(f.repo.save_stash(&[]).unwrap_err().contains("No changes"));
        assert!(f.repo.stashes().unwrap().is_empty());
        assert_eq!(fs::read(f.dir.join("vendor/notes")).unwrap(), b"dirty");
    }

    fn conflicting_stash() -> (Fixture, String) {
        let f = Fixture::new();
        for (path, text) in [("trail.txt", "base\n"), ("notes.txt", "notes\n"), ("other.txt", "other\n")] {
            fs::write(f.dir.join(path), text).unwrap();
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "Base"]);
        fs::write(f.dir.join("trail.txt"), "stashed\n").unwrap();
        fs::write(f.dir.join("other.txt"), "other, stashed\n").unwrap();
        fs::create_dir(f.dir.join("camp")).unwrap();
        fs::write(f.dir.join("camp/loose.txt"), "loose\n").unwrap();
        f.repo.push_stash(&Push { message: "Trail notes".into(), staged: None }).unwrap();
        let oid = rev(&f, "refs/stash");
        fs::write(f.dir.join("trail.txt"), "moved on\n").unwrap();
        f.git(&["commit", "-qam", "Move on"]);
        fs::write(f.dir.join("notes.txt"), "notes, edited\n").unwrap();
        fs::write(f.dir.join("plan.txt"), "plan\n").unwrap();
        (f, oid)
    }

    fn everything(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<(String, Option<Vec<u8>>)>) {
        let files = ["trail.txt", "notes.txt", "other.txt", "plan.txt", "camp/loose.txt"]
            .into_iter()
            .map(|path| (path.to_owned(), fs::read(f.dir.join(path)).ok()))
            .collect();
        (f.git(&["status", "--porcelain=v1", "--untracked-files=all"]), f.git(&["ls-files", "--stage"]), files)
    }

    fn shown(f: &Fixture, oid: &str, pop: bool) -> crate::operation::Snapshot {
        let mut state = crate::operation::State::default();
        state.stash = Some(crate::operation::Stashing { oid: oid.to_owned(), title: "Trail notes".into(), pop });
        state.mark(f.repo.status().unwrap().operation);
        state.update(crate::operation::snapshot(&f.repo).unwrap());
        state.snapshot().cloned().expect("the apply stopped")
    }

    #[test]
    fn a_conflicted_apply_is_named_by_its_stash_and_aborted_back_to_the_status_before() {
        use crate::operation::{Kind, backend};
        let (f, oid) = conflicting_stash();
        let before = everything(&f);
        let list = listed(&f);
        let failure = f.repo.apply_stash(&oid).unwrap_err();
        assert_eq!(failure.text, "Applying the stash stopped on a conflict. 1 file needs resolving. The stash stays in the list.");
        assert!(failure.output.as_deref().is_some_and(|output| output.contains("CONFLICT")), "{failure:?}");
        assert!(conflicted(&failure));
        assert_eq!(
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            b" M notes.txt\nM  other.txt\nUU trail.txt\n?? camp/loose.txt\n?? plan.txt\n"
        );
        let stopped = shown(&f, &oid, false);
        assert_eq!(stopped.kind, Kind::Applied);
        assert_eq!(stopped.summary(), "Applying stash Trail notes · 1 file needs resolving");
        assert_eq!((stopped.ours(), stopped.theirs(), stopped.theirs_name()), ("your files".into(), "stashed changes".into(), "stashed changes".into()));
        assert_eq!(stopped.abort_label(), Some(crate::operation::ABORT_STASH));

        let unknown = crate::operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((unknown.headline().as_str(), unknown.abort_label()), ("Unresolved conflicts", None));
        assert!(backend::abort(&f.repo, &unknown).unwrap_err().text.starts_with("FastKraken did not start this change"));

        let outcome = backend::abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Stash apply aborted");
        assert_eq!(outcome.detail, "Your files are as they were before Trail notes was applied, and it is still in the stash list.");
        assert_eq!(everything(&f), before);
        assert!(!f.dir.join("camp").exists(), "the folder the stash restored is gone too");
        assert_eq!(listed(&f), list);
        assert_eq!(f.repo.operation().unwrap(), None);
        assert!(f.repo.apply_stash(&oid).unwrap_err().text.starts_with("Applying the stash stopped on a conflict."));
    }

    #[test]
    fn an_abort_keeps_untracked_files_changed_since_the_apply() {
        let (f, oid) = conflicting_stash();
        f.repo.apply_stash(&oid).unwrap_err();
        fs::write(f.dir.join("camp/loose.txt"), "loose, edited after the apply\n").unwrap();
        fs::write(f.dir.join("camp/mine.txt"), "mine\n").unwrap();
        let outcome = crate::operation::backend::abort(&f.repo, &shown(&f, &oid, false)).unwrap();
        assert!(outcome.detail.ends_with(" 1 untracked file it restored was kept, because it has changed since."), "{}", outcome.detail);
        assert_eq!(fs::read(f.dir.join("camp/loose.txt")).unwrap(), b"loose, edited after the apply\n");
        assert_eq!(
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            b" M notes.txt\n?? camp/loose.txt\n?? camp/mine.txt\n?? plan.txt\n"
        );
    }

    #[test]
    fn an_abort_refuses_a_repository_that_moved_on() {
        let (f, oid) = conflicting_stash();
        f.repo.apply_stash(&oid).unwrap_err();
        let stopped = shown(&f, &oid, false);
        fs::write(f.dir.join("trail.txt"), "both\n").unwrap();
        f.git(&["add", "trail.txt"]);
        let resolved = everything(&f);
        let err = crate::operation::backend::abort(&f.repo, &stopped).unwrap_err();
        assert!(err.text.contains("changed"), "{}", err.text);
        assert_eq!(everything(&f), resolved);
        assert!(f.dir.join("camp/loose.txt").exists());
    }

    #[test]
    fn an_abort_removes_restored_files_whose_names_git_reads_specially() {
        let names = ["\"quoted\".txt", "line\nbreak.txt", "carriage\r"];
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "base\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "Base"]);
        fs::write(f.dir.join("trail.txt"), "stashed\n").unwrap();
        for name in names {
            fs::write(f.dir.join(name), format!("{name:?}\n")).unwrap();
        }
        f.repo.push_stash(&Push { message: "Odd names".into(), staged: None }).unwrap();
        let oid = rev(&f, "refs/stash");
        fs::write(f.dir.join("trail.txt"), "moved on\n").unwrap();
        f.git(&["commit", "-qam", "Move on"]);
        let before = f.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
        f.repo.apply_stash(&oid).unwrap_err();
        for name in names {
            assert!(f.dir.join(name).exists(), "{name:?} was restored");
        }
        let outcome = crate::operation::backend::abort(&f.repo, &shown(&f, &oid, false)).unwrap();
        assert_eq!(outcome.detail, "Your files are as they were before Trail notes was applied, and it is still in the stash list.");
        for name in names {
            assert!(!f.dir.join(name).exists(), "{name:?} is still there");
        }
        assert_eq!(f.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]), before);
    }

    #[test]
    fn a_conflicted_pop_keeps_its_stash_until_dropped() {
        let (f, oid) = conflicting_stash();
        let failure = f.repo.pop_stash(&oid).unwrap_err();
        assert_eq!(failure.text, "Popping the stash stopped on a conflict. 1 file needs resolving, and the stash was kept.");
        assert!(conflicted(&failure));
        let again = f.repo.pop_stash(&oid).unwrap_err();
        assert!(!conflicted(&again), "{again:?}");
        assert_eq!(f.repo.stashes().unwrap().iter().map(|e| e.oid.as_str()).collect::<Vec<_>>(), [oid.as_str()]);
        let stopped = shown(&f, &oid, true);
        assert_eq!(stopped.headline(), "Applying stash Trail notes");
        fs::write(f.dir.join("trail.txt"), "moved on, and stashed\n").unwrap();
        f.git(&["add", "trail.txt"]);
        assert_eq!(crate::operation::snapshot(&f.repo).unwrap(), None);
        let notice = crate::operation::stash_resolved(stopped.stash.as_ref().unwrap());
        assert_eq!(notice.title, "Stash Trail notes applied");
        let offered: Vec<_> = notice.actions.iter().map(|(label, action)| (label.to_string(), format!("{action:?}"))).collect();
        assert_eq!(offered, [("Drop the stash now".to_owned(), format!("Stash(DropNow({oid:?}))"))]);
        f.repo.drop_stash(&oid).unwrap();
        assert_eq!(listed(&f), "");
        assert_eq!(
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            b" M notes.txt\nM  other.txt\nM  trail.txt\n?? camp/loose.txt\n?? plan.txt\n"
        );
        let applied = crate::operation::Stashing { oid, title: "Trail notes".into(), pop: false };
        assert!(crate::operation::stash_resolved(&applied).actions.is_empty());
    }

    #[test]
    fn applying_refuses_staged_changes_and_untracked_files_in_the_way() {
        let (f, oid) = conflicting_stash();
        f.git(&["add", "notes.txt"]);
        let before = everything(&f);
        for failure in [f.repo.apply_stash(&oid).unwrap_err(), f.repo.pop_stash(&oid).unwrap_err()] {
            assert_eq!(failure.text, "Staged changes prevent applying this stash. Commit or unstage them first.");
            assert_eq!(failure.actions.len(), 0);
            assert!(!conflicted(&failure));
            assert_eq!(offer_unstage(failure).actions.len(), 1);
        }
        assert_eq!(everything(&f), before);
        f.git(&["restore", "--staged", "notes.txt"]);
        fs::create_dir(f.dir.join("camp")).unwrap();
        fs::write(f.dir.join("camp/loose.txt"), "already here\n").unwrap();
        let before = everything(&f);
        let failure = f.repo.pop_stash(&oid).unwrap_err();
        assert_eq!(
            failure.text,
            "camp/loose.txt is already in your files, and the stash holds an untracked copy of it. Move it aside, then try again."
        );
        assert_eq!(everything(&f), before);
        assert_eq!(f.repo.stashes().unwrap().len(), 1);
    }

    #[test]
    fn failed_pop_keeps_the_entry_and_preexisting_untracked_contents() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        fs::write(f.dir.join("notes"), "saved").unwrap();
        f.repo.save_stash(&[]).unwrap();
        let entries = f.repo.stashes().unwrap();
        fs::write(f.dir.join("notes"), "other").unwrap();
        assert!(f.repo.pop_stash(&entries[0].oid).is_err());
        assert_eq!(f.repo.stashes().unwrap(), entries);
        assert_eq!(fs::read(f.dir.join("notes")).unwrap(), b"other");
    }

    #[test]
    fn stashing_a_staged_rename_takes_both_paths_and_keeps_another_staged_file() {
        let f = Fixture::new();
        fs::write(f.dir.join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        fs::write(f.dir.join("other.txt"), "base\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["mv", "old.txt", "new.txt"]);
        fs::write(f.dir.join("new.txt"), "one\ntwo\nthree\nFOUR\n").unwrap();
        fs::write(f.dir.join("other.txt"), "other staged\n").unwrap();
        f.git(&["add", "."]);
        f.repo.save_stash(&[PathBuf::from("new.txt")]).unwrap();

        let stash = f.repo.stashes().unwrap()[0].oid.clone();
        assert_eq!(
            f.repo.stash_files(&stash).unwrap(),
            [ChangedFile { path: "new.txt".into(), status: "Renamed", from: Some("old.txt".into()) }]
        );
        assert_eq!(f.git(&["diff", "--cached", "--name-only", "-z"]), b"other.txt\0");
        assert_eq!(fs::read(f.dir.join("old.txt")).unwrap(), b"one\ntwo\nthree\nfour\n");
        assert!(!f.dir.join("new.txt").exists());
        assert_eq!(f.git(&["status", "--porcelain", "--", "old.txt", "new.txt"]), b"");
        let diff = f.repo.stash_diff(&stash).unwrap();
        assert!(diff.text.contains("rename from old.txt") && diff.text.contains("+FOUR"));
        assert!(!diff.text.contains("+one"));

        f.git(&["reset", "-q", "--", "other.txt"]);
        f.git(&["checkout", "--", "other.txt"]);
        f.git(&["mv", "old.txt", "moved.txt"]);
        f.repo.save_stash(&[PathBuf::from("moved.txt")]).unwrap();
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
        assert_eq!(fs::read(f.dir.join("old.txt")).unwrap(), b"one\ntwo\nthree\nfour\n");
        let stash = f.repo.stashes().unwrap()[0].oid.clone();
        assert_eq!(
            f.repo.stash_files(&stash).unwrap(),
            [ChangedFile { path: "moved.txt".into(), status: "Renamed", from: Some("old.txt".into()) }]
        );
    }
}
