use super::*;
use crate::stash;

impl Replay<'_> {
    async fn inspect_stash(&mut self, saved: &str) -> Result<()> {
        self.until("stash row in graph and sidebar", |tab| {
            tab.ui.stash.title(saved).is_some()
        })
        .await?;
        self.action(Action::Select(Selection::Stash(saved.to_owned())))?;
        self.until("selected stash details and patch", |tab| {
            matches!(&tab.ui.selection, Some(Selection::Stash(s)) if s == saved)
                && !tab.ui.stash.files.is_empty()
                && !tab.ui.loading_diff()
                && tab.diff.is_some()
        })
        .await
    }

    pub(super) async fn stashes(&mut self, dir: &Path) -> Result<()> {
        self.start("OBS07");
        let root = init(dir, "Cedar-stash")?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        fs::write(root.join("notes.txt"), NOTE)?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("only untracked note on main", |tab| {
            tab.ui.branch == "main" && files(tab, &[], &["notes.txt"])
        })
        .await?;
        self.action(Action::Stash(stash::Action::Save))?;
        self.until("clean worktree and one stash", |tab| {
            files(tab, &[], &[]) && tab.ui.stash.entries.len() == 1
        })
        .await?;
        let saved = oid(&root, "refs/stash")?;
        self.inspect_stash(&saved).await?;
        self.until("WIP stash lists one added note", |tab| {
            tab.ui.diff_title == "WIP on main"
                && tab.ui.stash.files.len() == 1
                && tab.ui.stash.files[0].path == Path::new("notes.txt")
                && tab.ui.stash.files[0].status == "Added"
                && patch(tab, "+Remember to mark the bridge.")
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain"], "")?;
        git_is(&root, &["show", &format!("{saved}^3:notes.txt")], NOTE)?;
        self.pass();

        self.start("OBS08");
        self.action(Action::Stash(stash::Action::Pop(saved.clone())))?;
        self.until("pop restores note and removes stash", |tab| {
            tab.ui.stash.entries.is_empty()
                && files(tab, &[], &["notes.txt"])
                && !matches!(tab.ui.selection, Some(Selection::Stash(_)))
                && tab.ui.history.iter().all(|row| row.oid == head)
        })
        .await?;
        git_is(&root, &["stash", "list"], "")?;
        git_is(&root, &["status", "--porcelain"], "?? notes.txt\n")?;
        if fs::read_to_string(root.join("notes.txt"))? != NOTE {
            return Err("Pop changed the note bytes".into());
        }
        self.pass();

        self.start("EXT05");
        let root = init(dir, "Stash-one-file")?;
        record(&root, "notes.txt", "Notebook\n", "Start notebook")?;
        record(&root, "checklist.txt", "Checklist\n", "Add checklist")?;
        fs::write(root.join("notes.txt"), "Notebook\nDraft introduction\n")?;
        fs::write(root.join("checklist.txt"), "Checklist\nInspect crossing\n")?;
        self.open(&root)?;
        self.until("two unstaged paths", |tab| {
            files(tab, &[], &["notes.txt", "checklist.txt"])
        })
        .await?;
        self.action(Action::Select(file("notes.txt", false)))?;
        self.until("working note selected", |tab| {
            patch(tab, "+Draft introduction")
        })
        .await?;
        self.action(Action::Stash(stash::Action::SaveFiles(vec!["notes.txt".into()])))?;
        self.until("only checklist pending and one stash", |tab| {
            files(tab, &[], &["checklist.txt"]) && tab.ui.stash.entries.len() == 1
        })
        .await?;
        let saved = oid(&root, "refs/stash")?;
        self.inspect_stash(&saved).await?;
        self.until("stash details contain only the note", |tab| {
            tab.ui.stash.files.len() == 1
                && tab.ui.stash.files[0].path == Path::new("notes.txt")
                && patch(tab, "+Draft introduction")
                && !patch(tab, "+Inspect crossing")
        })
        .await?;
        git_is(&root, &["diff", "--name-only"], "checklist.txt\n")?;
        git_is(
            &root,
            &["stash", "show", "-u", "--name-status", &saved],
            "M\tnotes.txt\n",
        )?;
        if fs::read_to_string(root.join("checklist.txt"))? != "Checklist\nInspect crossing\n" {
            return Err("Stashing one path changed the other edit".into());
        }
        self.pass();

        self.start("EXT06");
        let root = init(dir, "Apply-stash")?;
        record(&root, "notes.txt", "Notebook\n", "Start notebook")?;
        record(&root, "checklist.txt", "Checklist\n", "Add checklist")?;
        fs::write(root.join("notes.txt"), "Notebook\nDraft introduction\n")?;
        git(&root, &["stash", "push", "-qm", "Draft"])?;
        let saved = oid(&root, "refs/stash")?;
        self.open(&root)?;
        self.inspect_stash(&saved).await?;
        self.until("clean worktree before applying", |tab| {
            files(tab, &[], &[]) && tab.ui.stash.entries.len() == 1
        })
        .await?;
        self.action(Action::Stash(stash::Action::Apply(saved.clone())))?;
        self.until("applied stash remains available", |tab| {
            files(tab, &[], &["notes.txt"])
                && tab.ui.stash.entries.len() == 1
                && tab.ui.stash.title(&saved).is_some()
        })
        .await?;
        self.action(Action::Select(file("notes.txt", false)))?;
        self.until("applied working patch", |tab| {
            patch(tab, "+Draft introduction")
        })
        .await?;
        git_is(
            &root,
            &["stash", "list", "--format=%H"],
            &format!("{saved}\n"),
        )?;
        git_is(&root, &["status", "--porcelain"], " M notes.txt\n")?;
        if fs::read_to_string(root.join("notes.txt"))? != "Notebook\nDraft introduction\n" {
            return Err("Apply changed the saved bytes".into());
        }
        self.pass();

        self.start("EXT07");
        git(&root, &["restore", "--", "notes.txt"])?;
        fs::write(root.join("checklist.txt"), "Checklist\nInspect crossing\n")?;
        git(&root, &["add", "--", "checklist.txt"])?;
        self.until("external staged checklist seen", |tab| {
            files(tab, &["checklist.txt"], &[])
        })
        .await?;
        self.inspect_stash(&saved).await?;
        let refs_before = git(&root, &["show-ref"])?;
        let index_before = git(&root, &["ls-files", "--stage"])?;
        self.action(Action::Stash(stash::Action::Apply(saved.clone())))?;
        self.until("staged changes prevent applying the stash", |tab| {
            tab.ui.notice_text().contains("Staged changes prevent applying")
                && files(tab, &["checklist.txt"], &[])
                && tab.ui.stash.title(&saved).is_some()
        })
        .await?;
        git_is(&root, &["show-ref"], &refs_before)?;
        git_is(&root, &["ls-files", "--stage"], &index_before)?;
        git_is(
            &root,
            &["stash", "list", "--format=%H"],
            &format!("{saved}\n"),
        )?;
        git_is(&root, &["status", "--porcelain"], "M  checklist.txt\n")?;
        if fs::read_to_string(root.join("notes.txt"))? != "Notebook\n"
            || fs::read_to_string(root.join("checklist.txt"))? != "Checklist\nInspect crossing\n"
        {
            return Err("Refused stash apply changed working files".into());
        }
        self.pass();
        Ok(())
    }
}
