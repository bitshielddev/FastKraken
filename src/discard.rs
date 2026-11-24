use crate::repository::Run;
use crate::{
    Tab,
    commands::{self, Target},
    kit,
    notice::{Failure, Notice, Outcome},
    prompt::{self, Spec},
    repository::{Repository, Status, checked, nul_separated, raw_path, validate_oid},
    theme::*,
    view::{self, Desk, DiscardAsk, Tone, Ui},
};
use gpui::{Context, Div, Window, div, prelude::*, px, rgb};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub(crate) const DISCARDED: &str = "FastKraken: discarded changes";

const KEEPER: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "FastKraken"),
    ("GIT_AUTHOR_EMAIL", "fastkraken@localhost"),
    ("GIT_COMMITTER_NAME", "FastKraken"),
    ("GIT_COMMITTER_EMAIL", "fastkraken@localhost"),
];

pub(crate) const COMMANDS: &[commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, busy, on_repo, operating};
    &[Command::new(
        "repo.discard-all",
        Group::Changes,
        |_, _| "Discard all changes…".into(),
        |ui, target, _| {
            if !on_repo(ui, target) {
                return Availability::Hidden;
            }
            Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                ui.files.is_empty().then(|| "There are no changes to discard".to_owned())
            }))
        },
        |_, _| Some(view::Action::Discard(Action::AskAll)),
    )
    .words(&["reset hard all", "throw away everything", "clean working tree"])
    .icon("trash")
    .danger()
    .bar(Bar::Repository)]
};

#[derive(Clone, Debug)]
pub(crate) enum Action {
    AskAll,
    RestoreAll(String),
    RestoreFiles { oid: String, paths: Vec<PathBuf> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub head: Option<String>,
    pub head_ref: Option<OsString>,
    pub rows: Vec<(PathBuf, bool, &'static str)>,
}

impl Seen {
    pub(crate) fn of(ui: &Ui) -> Self {
        let rows = ui.files.iter().map(|file| (file.path.clone(), file.staged, file.status)).collect();
        Self::sorted(ui.head.clone(), ui.head_ref.clone(), rows)
    }

    fn of_status(status: &Status) -> Self {
        let rows = [(false, &status.unstaged), (true, &status.staged)]
            .into_iter()
            .flat_map(|(staged, files)| files.iter().map(move |file| (file.path.clone(), staged, file.status)))
            .collect();
        Self::sorted(status.head.clone(), status.head_ref.clone(), rows)
    }

    fn sorted(head: Option<String>, head_ref: Option<OsString>, mut rows: Vec<(PathBuf, bool, &'static str)>) -> Self {
        rows.sort();
        Self { head, head_ref, rows }
    }

    pub(crate) fn untracked(&self) -> usize {
        self.rows.iter().filter(|(_, _, status)| *status == "Untracked").count()
    }

    pub(crate) fn tracked(&self) -> usize {
        self.rows
            .iter()
            .filter(|(_, _, status)| *status != "Untracked")
            .map(|(path, _, _)| path)
            .collect::<BTreeSet<_>>()
            .len()
    }
}

pub(crate) fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

fn untracked_files(count: usize) -> String {
    match count {
        1 => "1 untracked file".to_owned(),
        n => format!("{n} untracked files"),
    }
}

pub(crate) fn describe(seen: &Seen) -> Spec {
    let (tracked, untracked) = (seen.tracked(), seen.untracked());
    let removed = match (tracked, untracked) {
        (0, n) => format!("{} will be deleted.", untracked_files(n)),
        (n, 0) => format!("Changes to {} will be removed, staged ones included.", files(n)),
        (n, m) => format!(
            "Changes to {} will be removed, staged ones included, and {} will be deleted.",
            files(n),
            untracked_files(m)
        ),
    };
    let after = if seen.head.is_some() {
        "Ignored files are not touched. Restore, in the notice that follows, brings everything back."
    } else {
        "Ignored files are not touched. There is no commit yet to keep the changes in, so this cannot be undone."
    };
    Spec::new("Discard all changes?", "Discard all").line(removed).line(after)
}

pub(crate) fn summary(ui: &Ui) -> String {
    said(ui.changed_count(), ui.checked_out_branch())
}

fn said(count: usize, branch: Option<&str>) -> String {
    let changes = match count {
        0 => "No changes".to_owned(),
        1 => "1 change".to_owned(),
        n => format!("{n} changes"),
    };
    match branch.filter(|branch| !branch.is_empty()) {
        Some(branch) => format!("{changes} on {branch}"),
        None => format!("{changes} on a detached HEAD"),
    }
}

pub(crate) fn discarded(kept: String, paths: Vec<PathBuf>) -> Outcome {
    Outcome::titled(
        format!("Discarded changes to {}", files(paths.len())),
        "Restore brings back their unstaged edits and untracked files.",
    )
    .offer("Restore", view::Action::Discard(Action::RestoreFiles { oid: kept, paths }))
    .sticky()
}

impl Repository {
    pub(crate) fn head_commit(&self) -> Result<Option<String>, String> {
        let output = self
            .read_command()
            .args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        if !output.status.success() {
            return Ok(None);
        }
        let oid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        validate_oid(&oid)?;
        Ok(Some(oid))
    }

    pub(crate) fn snapshot(&self, paths: &[PathBuf]) -> Result<String, String> {
        let input = nul_separated(paths, b"")?;
        let head = self.head_commit()?;
        let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
        let index = scratch.path().join("index");
        let mut start = self.command();
        start.env("GIT_INDEX_FILE", &index).arg("read-tree");
        match &head {
            Some(oid) => start.arg(oid),
            None => start.arg("--empty"),
        };
        checked(&mut start)?;
        crate::operation::with_input(
            self.command().env("GIT_INDEX_FILE", &index).args(["update-index", "--add", "--remove", "-z", "--stdin"]),
            input,
        )?;
        let tree = checked(self.command().env("GIT_INDEX_FILE", &index).arg("write-tree"))?;
        let tree = String::from_utf8_lossy(&tree).trim().to_owned();
        validate_oid(&tree)?;
        let mut commit = self.command();
        commit.envs(KEEPER).args(["commit-tree", "--no-gpg-sign", "-m", DISCARDED]);
        if let Some(head) = &head {
            commit.args(["-p", head]);
        }
        let kept = checked(commit.arg(&tree))?;
        let kept = String::from_utf8_lossy(&kept).trim().to_owned();
        validate_oid(&kept)?;
        Ok(kept)
    }

    fn kept_commit(&self, oid: &str) -> Result<(), String> {
        validate_oid(oid)?;
        let found = self
            .read_command()
            .args(["cat-file", "-e", &format!("{oid}^{{commit}}")])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        if found.status.success() {
            Ok(())
        } else {
            Err("The discarded changes are no longer in the repository: Git has cleaned them up.".into())
        }
    }

    pub(crate) fn restore_files(&self, oid: &str, paths: &[PathBuf]) -> Result<Outcome, String> {
        self.ensure_index_writable()?;
        self.kept_commit(oid)?;
        if paths.is_empty() {
            return Err("There are no files to restore.".into());
        }
        let status = self.status()?;
        let chosen: BTreeSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let changed: BTreeSet<&Path> =
            status.unstaged.iter().map(|file| file.path.as_path()).filter(|path| chosen.contains(path)).collect();
        match changed.iter().collect::<Vec<_>>().as_slice() {
            [] => {}
            [one] => {
                return Err(format!(
                    "{} changed since the discard, so Restore would overwrite it. Commit, stash or discard it first.",
                    one.display()
                ));
            }
            [first, rest @ ..] => {
                return Err(format!(
                    "{} and {} changed since the discard, so Restore would overwrite them. Commit, stash or discard them first.",
                    first.display(),
                    files(rest.len()).replace("file", "other file")
                ));
            }
        }
        let names = |output: Vec<u8>| -> BTreeSet<PathBuf> {
            output.split(|&b| b == 0).filter(|name| !name.is_empty()).map(raw_path).collect()
        };
        let kept = names(checked(self.read_command().args(["ls-tree", "-r", "-z", "--name-only", "--full-tree", oid]))?);
        let indexed = names(checked(self.read_command().args(["ls-files", "-z"]))?);
        let restorable: Vec<PathBuf> =
            paths.iter().filter(|path| kept.contains(*path) || indexed.contains(*path)).cloned().collect();
        if !restorable.is_empty() {
            let source = format!("--source={oid}");
            self.with_paths(&["restore", "--worktree", &source], &restorable)?;
        }
        Ok(Outcome::titled(format!("Restored changes to {}", files(paths.len())), ""))
    }

    pub(crate) fn discard_all(&self, seen: &Seen) -> Result<Outcome, String> {
        self.ensure_index_writable()?;
        let status = self.status()?;
        let now = Seen::of_status(&status);
        if now.head != seen.head || now.head_ref != seen.head_ref {
            return Err("The checked-out branch or its tip changed since you chose Discard all. Look again before discarding.".into());
        }
        if now.rows.is_empty() {
            return Err("There are no changes to discard.".into());
        }
        if now.rows != seen.rows {
            return Err("The changes are not the ones you saw: files changed since you chose Discard all. Look at them again before discarding.".into());
        }
        if status.head.is_none() {
            checked(self.command().args(["rm", "-r", "--cached", "--quiet", "--ignore-unmatch", "--", "."]))?;
            checked(self.command().args(["clean", "-f", "-d", "--quiet", "--", "."]))?;
            return Ok(Outcome::titled(
                "Discarded all changes",
                "There was no commit to keep them in, so they cannot be restored.",
            ));
        }
        let before = self.resolve(OsStr::new("refs/stash")).ok();
        let mut push = self.stash_push();
        push.args(["--include-untracked", &format!("--message={DISCARDED}")]);
        crate::stash::push_changes(&mut push)?;
        let kept = self.resolve(OsStr::new("stash@{0}"))?;
        if Some(&kept) == before.as_ref() {
            return Err("Git did not save the changes, so nothing was discarded.".into());
        }
        let (position, entries) = self
            .stash_position(&kept)
            .map_err(|err| format!("The changes were discarded and kept as a stash, which stays in the list: {err}"))?;
        self.drop_at(&kept, position, entries.len())
            .map_err(|err| format!("The changes were discarded and kept as a stash, which stays in the list: {err}"))?;
        Ok(Outcome::titled("Discarded all changes", "Restore brings back every change, staged ones included.")
            .offer("Restore", view::Action::Discard(Action::RestoreAll(kept)))
            .sticky())
    }

    pub(crate) fn restore_all(&self, oid: &str) -> Result<Outcome, Failure> {
        self.ensure_index_writable()?;
        self.kept_commit(oid)?;
        self.ready_to_apply(oid)?;
        let apply = |index: bool| {
            let mut command = self.command();
            command.args(["stash", "apply", "--quiet"]);
            if index {
                command.arg("--index");
            }
            command.arg(oid).stdin(std::process::Stdio::null()).run().map_err(|e| format!("Run bundled Git: {e}"))
        };
        let output = apply(true)?;
        if output.status.success() {
            return Ok(Outcome::titled("Restored the discarded changes", ""));
        }
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !said.contains("Try without --index") {
            return Err(Failure::from(if said.is_empty() { "Git could not restore the changes.".to_owned() } else { said }));
        }
        let output = apply(false)?;
        if output.status.success() {
            return Ok(Outcome::titled(
                "Restored the discarded changes",
                "The staged changes no longer fit the index, so every change is back as an unstaged edit.",
            ));
        }
        Err(Failure::from(String::from_utf8_lossy(&output.stderr).trim().to_owned()))
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::AskAll => {
            if !tab.ui.can_edit_index() || tab.ui.prompt.open() {
                return;
            }
            if tab.ui.files.is_empty() {
                tab.ui.notices.show(Notice::new(Tone::Info, "There are no changes to discard", ""));
                cx.notify();
                return;
            }
            let seen = Seen::of(&tab.ui);
            prompt::ask(&mut tab.ui, prompt::Kind::Discard(DiscardAsk::All(seen)), cx);
        }
        Action::RestoreAll(oid) => tab.apply_operation(
            cx,
            "Restoring the discarded changes…",
            Some("Changes restored"),
            false,
            move |repo| repo.restore_all(&oid),
            |_, _| {},
        ),
        Action::RestoreFiles { oid, paths } => tab.apply_operation(
            cx,
            "Restoring the discarded changes…",
            Some("Changes restored"),
            false,
            move |repo| repo.restore_files(&oid, &paths),
            |_, _| {},
        ),
    }
}

pub(crate) fn confirm(tab: &mut Tab, seen: Seen, cx: &mut Context<Tab>) {
    tab.apply_operation(
        cx,
        "Discarding all changes…",
        Some("Changes discarded"),
        true,
        move |repo| repo.discard_all(&seen),
        |_, _| {},
    );
}

pub(crate) fn header<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let enabled = commands::find("repo.discard-all")
        .and_then(|command| commands::entry(command, ui, &Target::Repo, cx))
        .is_some_and(|entry| entry.action.is_some());
    let tip = commands::tip_with_keys("Discard all changes…", "repo.discard-all");
    div()
        .h(px(FILE_ROW))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap(px(6.))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(kit::fit(summary(ui)).size(12.).color(TEXT_2).grow())
        .when(enabled, |row| {
            row.child(kit::icon_btn("discard-all", "trash", tip, view::Action::Run("repo.discard-all"), cx))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn read(f: &Fixture, name: &str) -> Option<Vec<u8>> {
        fs::read(f.dir.join(name)).ok()
    }

    fn grove() -> Fixture {
        let f = Fixture::new();
        for name in ["a.txt", "b c.txt", "d.txt", "kept.txt"] {
            fs::write(f.dir.join(name), "base\n").unwrap();
        }
        fs::write(f.dir.join(".gitignore"), "*.log\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("a.txt"), "staged\n").unwrap();
        f.git(&["add", "--", "a.txt"]);
        fs::write(f.dir.join("a.txt"), "staged and more\n").unwrap();
        fs::write(f.dir.join("b c.txt"), "edited\n").unwrap();
        fs::remove_file(f.dir.join("d.txt")).unwrap();
        fs::write(f.dir.join("kept.txt"), "edited too\n").unwrap();
        fs::write(f.dir.join("new é.txt"), "new\n").unwrap();
        fs::create_dir_all(f.dir.join("build/sub")).unwrap();
        fs::write(f.dir.join("build/out.log"), "ignored\n").unwrap();
        fs::write(f.dir.join("build/sub/u.txt"), "untracked\n").unwrap();
        fs::write(f.dir.join("top.log"), "ignored too\n").unwrap();
        f
    }

    fn picks() -> (Vec<PathBuf>, BTreeSet<PathBuf>) {
        let paths = vec![PathBuf::from("a.txt"), PathBuf::from("b c.txt"), PathBuf::from("d.txt"), PathBuf::from("new é.txt")];
        (paths, BTreeSet::from([PathBuf::from("new é.txt")]))
    }

    #[test]
    fn discarding_several_files_keeps_their_edits_where_restore_brings_them_back() {
        let f = grove();
        let head = f.git(&["rev-parse", "HEAD"]);
        let refs = f.git(&["for-each-ref"]);
        let (paths, untracked) = picks();
        let kept = f.repo.discard_files(&paths, &untracked).unwrap();
        assert_eq!(read(&f, "a.txt").unwrap(), b"staged\n");
        assert_eq!(read(&f, "b c.txt").unwrap(), b"base\n");
        assert_eq!(read(&f, "d.txt").unwrap(), b"base\n");
        assert!(read(&f, "new é.txt").is_none());
        assert_eq!(read(&f, "kept.txt").unwrap(), b"edited too\n");
        assert_eq!(f.git(&["show", ":a.txt"]), b"staged\n");
        assert_eq!(f.git(&["for-each-ref"]), refs);

        let outcome = discarded(kept.clone(), paths.clone());
        assert_eq!(outcome.title, "Discarded changes to 4 files");
        assert_eq!(outcome.actions.iter().map(|(label, _)| label.as_ref()).collect::<Vec<_>>(), ["Restore"]);
        let restored = f.repo.restore_files(&kept, &paths).unwrap();
        assert_eq!(restored.title, "Restored changes to 4 files");
        assert_eq!(read(&f, "a.txt").unwrap(), b"staged and more\n");
        assert_eq!(read(&f, "b c.txt").unwrap(), b"edited\n");
        assert!(read(&f, "d.txt").is_none());
        assert_eq!(read(&f, "new é.txt").unwrap(), b"new\n");
        assert_eq!(f.git(&["show", ":a.txt"]), b"staged\n");
        assert_eq!(f.git(&["show", ":d.txt"]), b"base\n");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert!(f.git(&["stash", "list"]).is_empty());
    }

    #[test]
    fn restoring_discarded_files_refuses_a_file_changed_since_and_a_snapshot_that_is_gone() {
        let f = grove();
        let (paths, untracked) = picks();
        let kept = f.repo.discard_files(&paths, &untracked).unwrap();
        fs::write(f.dir.join("b c.txt"), "rewritten\n").unwrap();
        let err = f.repo.restore_files(&kept, &paths).unwrap_err();
        assert!(err.starts_with("b c.txt changed since the discard"), "{err}");
        assert_eq!(read(&f, "b c.txt").unwrap(), b"rewritten\n");
        assert_eq!(read(&f, "a.txt").unwrap(), b"staged\n");
        assert!(read(&f, "new é.txt").is_none());

        fs::write(f.dir.join("b c.txt"), "base\n").unwrap();
        fs::write(f.dir.join("new é.txt"), "someone else\n").unwrap();
        let err = f.repo.restore_files(&kept, &paths).unwrap_err();
        assert!(err.starts_with("new é.txt changed since the discard"), "{err}");
        assert_eq!(read(&f, "new é.txt").unwrap(), b"someone else\n");

        let gone = "0123456789abcdef0123456789abcdef01234567";
        let err = f.repo.restore_files(gone, &paths).unwrap_err();
        assert!(err.contains("no longer in the repository"), "{err}");
        assert!(f.repo.restore_files("HEAD", &paths).is_err());
    }

    #[test]
    fn discarding_everything_keeps_ignored_files_and_restore_brings_back_staged_unstaged_and_untracked_changes() {
        let f = grove();
        let head = f.git(&["rev-parse", "HEAD"]);
        let before = f.git(&["status", "--porcelain=v1", "-uall"]);
        let seen = Seen::of_status(&f.repo.status().unwrap());
        assert_eq!((seen.tracked(), seen.untracked()), (4, 2));
        let outcome = f.repo.discard_all(&seen).unwrap();
        assert_eq!(outcome.title, "Discarded all changes");
        let Some((label, view::Action::Discard(Action::RestoreAll(kept)))) = outcome.actions.first() else {
            panic!("no Restore offered: {:?}", outcome.actions);
        };
        assert_eq!(label.as_ref(), "Restore");
        assert!(f.git(&["status", "--porcelain=v1", "-uall"]).is_empty());
        assert!(f.git(&["stash", "list"]).is_empty());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(read(&f, "build/out.log").unwrap(), b"ignored\n");
        assert_eq!(read(&f, "top.log").unwrap(), b"ignored too\n");
        assert!(read(&f, "build/sub/u.txt").is_none());
        assert_eq!(f.git(&["cat-file", "-t", kept]), b"commit\n");

        f.repo.restore_all(kept).unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1", "-uall"]), before);
        assert_eq!(read(&f, "a.txt").unwrap(), b"staged and more\n");
        assert_eq!(f.git(&["show", ":a.txt"]), b"staged\n");
        assert_eq!(read(&f, "build/sub/u.txt").unwrap(), b"untracked\n");
        assert!(f.git(&["stash", "list"]).is_empty());
    }

    #[test]
    fn discarding_everything_refuses_changes_or_a_tip_that_moved_since_the_prompt() {
        let f = grove();
        let seen = Seen::of_status(&f.repo.status().unwrap());
        fs::write(f.dir.join("late.txt"), "late\n").unwrap();
        let before = f.git(&["status", "--porcelain=v1", "-uall"]);
        let err = f.repo.discard_all(&seen).unwrap_err();
        assert!(err.starts_with("The changes are not the ones you saw"), "{err}");
        assert_eq!(f.git(&["status", "--porcelain=v1", "-uall"]), before);
        assert_eq!(read(&f, "late.txt").unwrap(), b"late\n");

        let seen = Seen::of_status(&f.repo.status().unwrap());
        f.git(&["commit", "-qm", "staged a"]);
        let err = f.repo.discard_all(&seen).unwrap_err();
        assert!(err.contains("its tip changed"), "{err}");
        assert_eq!(read(&f, "b c.txt").unwrap(), b"edited\n");
        assert!(f.git(&["stash", "list"]).is_empty());
    }

    #[test]
    fn discarding_everything_before_the_first_commit_unstages_and_deletes_but_keeps_ignored_files() {
        let f = Fixture::new();
        fs::write(f.dir.join(".gitignore"), "*.log\n").unwrap();
        fs::write(f.dir.join("first.txt"), "first\n").unwrap();
        f.git(&["add", "--", "first.txt"]);
        fs::write(f.dir.join("loose.txt"), "loose\n").unwrap();
        fs::write(f.dir.join("debug.log"), "debug\n").unwrap();
        let seen = Seen::of_status(&f.repo.status().unwrap());
        assert!(describe(&seen).lines[1].contains("cannot be undone"));
        let outcome = f.repo.discard_all(&seen).unwrap();
        assert!(outcome.actions.is_empty());
        assert!(f.git(&["ls-files"]).is_empty());
        assert!(read(&f, "first.txt").is_none());
        assert!(read(&f, "loose.txt").is_none());
        assert!(read(&f, ".gitignore").is_none());
        assert_eq!(read(&f, "debug.log").unwrap(), b"debug\n");
    }

    #[test]
    fn discarding_everything_is_refused_during_a_merge() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "base\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["switch", "-q", "-c", "side"]);
        fs::write(f.dir.join("trail.txt"), "side\n").unwrap();
        f.git(&["commit", "-qam", "side"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("trail.txt"), "main\n").unwrap();
        f.git(&["commit", "-qam", "main"]);
        assert!(!f.repo.command().args(["merge", "-q", "side"]).output().unwrap().status.success());
        let seen = Seen::of_status(&f.repo.status().unwrap());
        assert_eq!(f.repo.discard_all(&seen).unwrap_err(), "Finish or abort the merge first.");
        assert!(f.git(&["ls-files", "-u"]).len() > 0);
    }

    #[test]
    fn the_prompt_counts_files_and_the_header_names_the_branch() {
        let seen = Seen {
            head: Some("0123456789abcdef0123456789abcdef01234567".into()),
            head_ref: None,
            rows: vec![
                (PathBuf::from("a"), false, "Modified"),
                (PathBuf::from("a"), true, "Modified"),
                (PathBuf::from("b"), true, "Added"),
                (PathBuf::from("c"), false, "Untracked"),
            ],
        };
        let spec = describe(&seen);
        assert_eq!((spec.title.as_str(), spec.confirm.as_ref()), ("Discard all changes?", "Discard all"));
        assert_eq!(spec.lines[0], "Changes to 2 files will be removed, staged ones included, and 1 untracked file will be deleted.");
        assert!(spec.lines[1].contains("Restore"));
        assert_eq!(said(3, Some("main")), "3 changes on main");
        assert_eq!(said(1, Some("main")), "1 change on main");
        assert_eq!(said(0, None), "No changes on a detached HEAD");
    }
}
