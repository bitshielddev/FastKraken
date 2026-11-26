use crate::repository::Run;
use crate::{
    Tab,
    commands::{self, Target},
    kit,
    journal::{Change, Discarded},
    notice::{Failure, Notice, Outcome},
    prompt::{self, Spec},
    repository::{Repository, Status, checked, nul_separated, raw_path, validate_oid},
    theme::*,
    view::{self, Desk, DiscardAsk, Tone, Ui},
};
use gpui::{Context, Div, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub(crate) const DISCARDED: &str = "FastKraken: discarded changes";
const UNRECORDED: &str = "Changes discarded before the first commit cannot be brought back";

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
        "Ignored files are not touched. Undo, in the notice that follows, brings everything back."
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
        "Undo brings back their unstaged edits and untracked files.",
    )
    .undone_as(discard_title(&paths))
    .changed(Change::discarded_files(kept, &paths))
    .offering_undo()
    .sticky()
}

fn discard_title(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("Discard changes to {}", one.display()),
        _ => format!("Discard changes to {}", files(paths.len())),
    }
}

pub(crate) fn discard_file(repo: &Repository, path: PathBuf, untracked: bool) -> Result<Outcome, Failure> {
    let untracked_set: BTreeSet<PathBuf> = if untracked { BTreeSet::from([path.clone()]) } else { BTreeSet::new() };
    let kept = repo.keep_files(std::slice::from_ref(&path), &untracked_set)?;
    repo.discard_file(&path, untracked)?;
    let paths = vec![path];
    Ok(Outcome::default().undone_as(discard_title(&paths)).changed(Change::discarded_files(kept, &paths)).offering_undo())
}

pub(crate) fn discard_picked(
    repo: &Repository,
    paths: Vec<PathBuf>,
    untracked: &BTreeSet<PathBuf>,
) -> Result<Outcome, Failure> {
    let kept = repo.keep_files(&paths, untracked)?;
    if let Err(err) = repo.remove_file_changes(&paths, untracked) {
        let mut failure = Failure {
            plain: Some((
                "Some changes could not be discarded".to_owned(),
                "Undo brings back the changes that were discarded.".to_owned(),
            )),
            ..Failure::from(err.clone())
        };
        failure.journal.title = Some(discard_title(&paths));
        failure.journal.changes.push(Change::discarded_files(kept, &paths));
        failure.journal.offered = true;
        return Err(failure.output(err));
    }
    Ok(discarded(kept, paths))
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

    fn unlike_kept<'a>(
        &self,
        changed: Vec<&'a Path>,
        kept: &BTreeSet<PathBuf>,
        blobs: &BTreeMap<PathBuf, String>,
    ) -> Result<Vec<&'a Path>, String> {
        let on_disk = |path: &Path| std::fs::symlink_metadata(self.root.join(path));
        let files: Vec<&Path> =
            changed.iter().copied().filter(|path| on_disk(path).is_ok_and(|meta| meta.is_file())).collect();
        let hashes: Vec<String> = if files.is_empty() {
            Vec::new()
        } else {
            let output = checked(self.read_command().args(["hash-object", "--"]).args(&files))?;
            String::from_utf8_lossy(&output).lines().map(str::to_owned).collect()
        };
        if hashes.len() != files.len() {
            return Err("Git could not read the files to restore.".into());
        }
        let current: BTreeMap<&Path, &str> = files.into_iter().zip(hashes.iter().map(String::as_str)).collect();
        Ok(changed
            .into_iter()
            .filter(|path| match current.get(path) {
                Some(hash) => blobs.get(*path).map(String::as_str) != Some(*hash),
                None => kept.contains(*path) || on_disk(path).is_ok(),
            })
            .collect())
    }

    pub(crate) fn restore_files(&self, oid: &str, paths: &[PathBuf]) -> Result<Outcome, String> {
        self.ensure_index_writable()?;
        self.kept_commit(oid)?;
        if paths.is_empty() {
            return Err("There are no files to restore.".into());
        }
        let status = self.status()?;
        let chosen: BTreeSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let listing = checked(self.read_command().args(["ls-tree", "-r", "-z", "--full-tree", oid]))?;
        let mut kept = BTreeSet::new();
        let mut blobs = BTreeMap::new();
        for record in listing.split(|&b| b == 0).filter(|record| !record.is_empty()) {
            let Some(tab) = record.iter().position(|&b| b == b'\t') else { continue };
            let path = raw_path(&record[tab + 1..]);
            if let [b"100644" | b"100755", b"blob", blob] = record[..tab].split(|&b| b == b' ').collect::<Vec<_>>()[..] {
                blobs.insert(path.clone(), String::from_utf8_lossy(blob).into_owned());
            }
            kept.insert(path);
        }
        let changed: Vec<&Path> =
            status.unstaged.iter().map(|file| file.path.as_path()).filter(|path| chosen.contains(path)).collect();
        let changed = self.unlike_kept(changed, &kept, &blobs)?;
        match changed.iter().collect::<Vec<_>>().as_slice() {
            [] => {}
            [one] => {
                return Err(format!(
                    "{} changed since the discard, so Undo would overwrite it. Commit, stash or discard it first.",
                    one.display()
                ));
            }
            [first, rest @ ..] => {
                return Err(format!(
                    "{} and {} changed since the discard, so Undo would overwrite them. Commit, stash or discard them first.",
                    first.display(),
                    files(rest.len()).replace("file", "other file")
                ));
            }
        }
        let indexed: BTreeSet<PathBuf> = checked(self.read_command().args(["ls-files", "-z"]))?
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .map(raw_path)
            .collect();
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
            )
            .not_undoable(UNRECORDED));
        }
        let kept = self.stash_away()?;
        Ok(Outcome::titled("Discarded all changes", "Undo brings back every change, staged ones included.")
            .undone_as("Discard all changes")
            .changed(Change::Discard { saved: kept, discarded: Discarded::All })
            .offering_undo()
            .sticky())
    }

    pub(crate) fn stash_away(&self) -> Result<String, String> {
        self.ensure_index_writable()?;
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
        Ok(kept)
    }

    pub(crate) fn save_tracked(&self) -> Result<Option<String>, String> {
        let saved = checked(self.command().envs(KEEPER).args(["stash", "create", DISCARDED]))?;
        let saved = String::from_utf8_lossy(&saved).trim().to_owned();
        if saved.is_empty() {
            return Ok(None);
        }
        validate_oid(&saved)?;
        Ok(Some(saved))
    }

    pub(crate) fn discard_again(&self, paths: &[PathBuf]) -> Result<String, String> {
        let status = self.status()?;
        let untracked: BTreeSet<PathBuf> = status
            .unstaged
            .iter()
            .filter(|file| file.status == "Untracked" && paths.contains(&file.path))
            .map(|file| file.path.clone())
            .collect();
        let changed: BTreeSet<&Path> = status.unstaged.iter().map(|file| file.path.as_path()).collect();
        if paths.iter().any(|path| !changed.contains(path.as_path())) {
            return Err("Something else changed the files since. Redo cannot be applied safely.".into());
        }
        let kept = self.keep_files(paths, &untracked)?;
        self.remove_file_changes(paths, &untracked)?;
        Ok(kept)
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
        .h(whole_px(FILE_ROW))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap(px(6.))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::fit(summary(ui)).size(12.).color(palette().text_2).grow())
        .when(!ui.files.is_empty(), |row| row.child(crate::file_list::tools(ui, crate::file_list::Panel::Working, cx)))
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
        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.journal.changes, [Change::discarded_files(kept.clone(), &paths)]);
        assert_eq!(outcome.journal.title.as_deref(), Some("Discard changes to 4 files"));
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
    fn a_discard_that_stops_part_way_is_still_recorded_and_restoring_brings_everything_back() {
        use std::os::unix::fs::PermissionsExt;
        let f = grove();
        fs::create_dir_all(f.dir.join("locked")).unwrap();
        fs::write(f.dir.join("locked/held.txt"), "held\n").unwrap();
        fs::set_permissions(f.dir.join("locked"), fs::Permissions::from_mode(0o555)).unwrap();
        let paths = vec![PathBuf::from("b c.txt"), PathBuf::from("d.txt"), PathBuf::from("locked/held.txt")];
        let untracked = BTreeSet::from([PathBuf::from("locked/held.txt")]);
        let failed = discard_picked(&f.repo, paths.clone(), &untracked);
        fs::set_permissions(f.dir.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        let Err(failure) = failed else { panic!("the discard did not stop") };
        let Some(Change::Discard { saved: oid, discarded: Discarded::Files(offered) }) = failure.journal.changes.first() else {
            panic!("the discard was not recorded: {:?}", failure.journal);
        };
        let offered: Vec<PathBuf> = offered.iter().map(|path| raw_path(path)).collect();
        assert_eq!(offered, paths);
        assert_eq!(read(&f, "b c.txt").unwrap(), b"base\n");
        assert_eq!(read(&f, "locked/held.txt").unwrap(), b"held\n");

        f.repo.restore_files(oid, &paths).unwrap();
        assert_eq!(read(&f, "b c.txt").unwrap(), b"edited\n");
        assert!(read(&f, "d.txt").is_none());
        assert_eq!(read(&f, "locked/held.txt").unwrap(), b"held\n");
        assert_eq!(read(&f, "kept.txt").unwrap(), b"edited too\n");
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
        let Some(Change::Discard { saved: kept, discarded: Discarded::All }) = outcome.journal.changes.first() else {
            panic!("the discard was not recorded: {:?}", outcome.journal);
        };
        assert!(outcome.actions.is_empty());
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
        assert_eq!(outcome.journal.refused.as_deref(), Some(UNRECORDED));
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
        assert!(spec.lines[1].contains("Undo, in the notice that follows"));
        assert_eq!(said(3, Some("main")), "3 changes on main");
        assert_eq!(said(1, Some("main")), "1 change on main");
        assert_eq!(said(0, None), "No changes on a detached HEAD");
    }
}
