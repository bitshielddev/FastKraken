use super::*;
use crate::{
    commands::{self, Availability, Target},
    journal,
    notice,
    view::Tone,
};

type Said = (Tone, String, String, Vec<String>);

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };

fn snapshot(root: &Path) -> Result<String> {
    Ok([
        git(root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags", "refs/stash"])?,
        git(root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default(),
        git(root, &["ls-files", "--stage"])?,
        git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?,
    ]
    .join("\n"))
}

fn read(root: &Path, name: &str) -> Result<String> {
    Ok(fs::read_to_string(root.join(name))?)
}

impl Replay<'_> {
    pub(super) async fn gap10(&mut self, dir: &Path) -> Result<()> {
        self.undo_and_redo_a_commit(dir).await?;
        self.undo_a_switch(dir).await?;
        self.undo_a_hard_reset(dir).await?;
        self.undo_a_branch_deletion(dir).await?;
        self.undo_a_discard(dir).await?;
        self.refuse_after_an_outside_change(dir).await?;
        self.refuse_to_undo_a_push(dir).await?;
        self.hide_private_refs(dir).await
    }

    fn menu_rows_now(&mut self) -> Result<[(String, bool); 2]> {
        Ok(self.tab.read_with(self.cx, |tab, _| journal::menu_rows(&tab.ui))?)
    }

    fn undo_entry(&mut self, id: &'static str) -> Result<Option<(String, Option<String>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::find(id)
                .and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx))
                .map(|entry| (entry.title, entry.disabled))
        })?)
    }

    async fn settled_notice(&mut self, title: &str) -> Result<Said> {
        self.until(&format!("the notice {title:?}"), |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == title)
        })
        .await?;
        self.said()?.ok_or_else(|| format!("The notice {title:?} went away").into())
    }

    async fn undo_and_redo_a_commit(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1001");
        let root = init(dir, "Undo-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        let parent = oid(&root, "HEAD")?;
        fs::write(root.join("date.txt"), "parse dates in UTC\n")?;
        git(&root, &["add", "--", "date.txt"])?;
        let staged = snapshot(&root)?;
        self.open(&root)?;
        self.until("Undo-trail open with date.txt staged", |tab| {
            tab.ui.repo == "Undo-trail" && files(tab, &["date.txt"], &[])
        })
        .await?;
        if self.menu_rows_now()?[0] != ("Undo".to_owned(), false) {
            return Err(format!("Before anything is done, the Edit menu reads {:?}", self.menu_rows_now()?).into());
        }
        self.type_commit("Fix the date parser")?;
        self.until("the commit made", |tab| tab.ui.head.as_deref().is_some_and(|head| head != parent) && tab.ui.files.is_empty())
            .await?;
        let committed = oid(&root, "main")?;
        git_is(&root, &["log", "-1", "--format=%s", "main"], "Fix the date parser\n")?;
        let said = self.settled_notice("Changes committed").await?;
        if said.3 != ["Undo"] {
            return Err(format!("The commit notice reads {said:?}").into());
        }
        let rows = self.menu_rows_now()?;
        if rows[0] != ("Undo Commit 'Fix the date parser'".to_owned(), true) || rows[1] != ("Redo".to_owned(), false) {
            return Err(format!("After the commit the Edit menu reads {rows:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-z")?;
        let said = self.settled_notice("Undid: Commit 'Fix the date parser'").await?;
        let detail = format!("main is back at {}. The undone changes are staged.", notice::short(&parent));
        if said != (Tone::Success, "Undid: Commit 'Fix the date parser'".to_owned(), detail, vec!["Redo".to_owned()]) {
            return Err(format!("Undoing the commit said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{parent}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-status"], "A\tdate.txt\n")?;
        if snapshot(&root)? != staged {
            return Err(format!("After the undo Git shows {:?}", snapshot(&root)?).into());
        }
        self.until("date.txt staged again in the panel", |tab| files(tab, &["date.txt"], &[])).await?;
        self.pass();

        self.start("GAP1002");
        let rows = self.menu_rows_now()?;
        if rows[1] != ("Redo Commit 'Fix the date parser'".to_owned(), true) {
            return Err(format!("After the undo the Edit menu reads {rows:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-shift-z")?;
        let said = self.settled_notice("Redid: Commit 'Fix the date parser'").await?;
        if said.3 != ["Undo"] {
            return Err(format!("Redoing the commit said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{committed}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.until("the panel empty again", |tab| tab.ui.files.is_empty() && tab.ui.head.as_deref() == Some(committed.as_str()))
            .await?;
        self.pass();
        Ok(())
    }

    async fn undo_a_switch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1003");
        let root = init(dir, "Switch-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        git(&root, &["branch", "ridge"])?;
        self.open(&root)?;
        let ridge = Target::Branch("ridge".into());
        self.until("Switch-trail open with ridge", |tab| tab.ui.repo == "Switch-trail" && tab.ui.refs.oid_of(&ridge).is_some())
            .await?;
        self.choose(&ridge, "branch.switch")?;
        self.until("ridge checked out", |tab| !tab.ui.busy() && tab.ui.branch == "ridge").await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;
        let entry = self.undo_entry("edit.undo")?;
        if entry != Some(("Undo Switch from main to ridge".to_owned(), None)) {
            return Err(format!("After the switch the undo command is {entry:?}").into());
        }
        self.action(Action::Run("edit.undo"))?;
        let said = self.settled_notice("Undid: Switch from main to ridge").await?;
        if said.2 != "Back on main." {
            return Err(format!("Undoing the switch said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        self.until("main shown as checked out", |tab| tab.ui.branch == "main").await?;
        self.pass();
        Ok(())
    }

    async fn undo_a_hard_reset(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1004");
        let root = init(dir, "Reset-trail")?;
        record(&root, "quarry.txt", "Quarry: open\n", "Start the quarry")?;
        let start = oid(&root, "HEAD")?;
        record(&root, "stone.txt", "Stone: one\n", "Cut the stone")?;
        let tip = oid(&root, "HEAD")?;
        fs::write(root.join("quarry.txt"), "Quarry: flooded\n")?;
        fs::write(root.join("stone.txt"), "Stone: cracked\n")?;
        git(&root, &["add", "--", "stone.txt"])?;
        let before = snapshot(&root)?;
        self.open(&root)?;
        self.until("Reset-trail open with its edits", |tab| {
            tab.ui.repo == "Reset-trail" && tab.ui.history.iter().any(|row| row.oid == start) && files(tab, &["stone.txt"], &["quarry.txt"])
        })
        .await?;
        self.select_commit(&start).await?;
        let commit = Target::Commit(start.clone());
        self.choose_in(&commit, "commit.reset", "commit.reset-hard")?;
        let (lines, danger) = self.asked_about("Reset main and discard all changes?").await?;
        let line = "1 commit leaves main. Changes in 2 files will be discarded. Undo, in the notice that follows, brings them back.";
        if !danger || !lines.iter().any(|shown| shown == line) {
            return Err(format!("The hard reset prompt reads {lines:?}").into());
        }
        self.key(CMD_RETURN)?;
        let short = notice::short(&start).to_owned();
        let said = self.settled_notice(&format!("Reset main to {short}")).await?;
        if said.3 != ["Undo"] {
            return Err(format!("The hard reset notice reads {said:?}").into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{start}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        let said = self.settled_notice(&format!("Undid: Reset main to {short}")).await?;
        if said.0 != Tone::Success || said.3 != ["Redo"] {
            return Err(format!("Undoing the hard reset said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{tip}\n"))?;
        if snapshot(&root)? != before {
            return Err(format!("After the undo Git shows {:?}", snapshot(&root)?).into());
        }
        if read(&root, "quarry.txt")? != "Quarry: flooded\n" || read(&root, "stone.txt")? != "Stone: cracked\n" {
            return Err("The discarded edits did not come back".into());
        }
        git_is(&root, &["show", ":stone.txt"], "Stone: cracked\n")?;
        self.until("the edits listed again", |tab| files(tab, &["stone.txt"], &["quarry.txt"])).await?;
        self.pass();
        Ok(())
    }

    async fn undo_a_branch_deletion(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1005");
        let root = init(dir, "Delete-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "ridge.txt", "Ridge\n", "Climb the ridge")?;
        let tip = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["config", "branch.feature/ridge.remote", "origin"])?;
        git(&root, &["config", "branch.feature/ridge.merge", "refs/heads/feature/ridge"])?;
        let before = snapshot(&root)?;
        self.open(&root)?;
        let ridge = Target::Branch("feature/ridge".into());
        self.until("Delete-trail open with feature/ridge", |tab| tab.ui.repo == "Delete-trail" && tab.ui.refs.oid_of(&ridge).is_some())
            .await?;
        self.choose(&ridge, "branch.delete")?;
        self.asked_about("Delete branch feature/ridge?").await?;
        self.key(CMD_RETURN)?;
        self.settled_notice("Deleted branch feature/ridge").await?;
        git_is(&root, &["for-each-ref", "refs/heads/feature/ridge"], "")?;
        self.focus_desk()?;
        self.key("cmd-z")?;
        let said = self.settled_notice("Undid: Delete branch feature/ridge").await?;
        let detail = format!("feature/ridge is back at {}.", notice::short(&tip));
        if said.2 != detail {
            return Err(format!("Undoing the delete said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/heads/feature/ridge"], &format!("{tip}\n"))?;
        git_is(&root, &["config", "branch.feature/ridge.remote"], "origin\n")?;
        git_is(&root, &["config", "branch.feature/ridge.merge"], "refs/heads/feature/ridge\n")?;
        if snapshot(&root)? != before {
            return Err(format!("After the undo Git shows {:?}", snapshot(&root)?).into());
        }
        self.until("feature/ridge listed again", |tab| tab.ui.refs.oid_of(&ridge) == Some(tip.as_str())).await?;
        self.pass();
        Ok(())
    }

    async fn undo_a_discard(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1006");
        let root = init(dir, "Discard-trail")?;
        record(&root, "camp.txt", "Camp\n", "Pitch the camp")?;
        record(&root, "fire.txt", "Fire\n", "Light the fire")?;
        fs::write(root.join("camp.txt"), "Camp, moved uphill\n")?;
        fs::write(root.join("fire.txt"), "Fire, banked\n")?;
        git(&root, &["add", "--", "fire.txt"])?;
        fs::write(root.join("notes.txt"), "Water is upstream\n")?;
        let before = snapshot(&root)?;
        self.open(&root)?;
        self.until("Discard-trail open with three changes", |tab| {
            tab.ui.repo == "Discard-trail" && files(tab, &["fire.txt"], &["camp.txt", "notes.txt"])
        })
        .await?;
        self.action(Action::Run("repo.discard-all"))?;
        self.asked_about("Discard all changes?").await?;
        self.key(CMD_RETURN)?;
        self.settled_notice("Discarded all changes").await?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        self.until("no changes listed", |tab| tab.ui.files.is_empty()).await?;
        let entry = self.undo_entry("edit.undo")?;
        if entry != Some(("Undo Discard all changes".to_owned(), None)) {
            return Err(format!("After the discard the undo command is {entry:?}").into());
        }
        self.action(Action::Run("edit.undo"))?;
        self.settled_notice("Undid: Discard all changes").await?;
        if snapshot(&root)? != before {
            return Err(format!("After the undo Git shows {:?}", snapshot(&root)?).into());
        }
        if read(&root, "notes.txt")? != "Water is upstream\n" || read(&root, "camp.txt")? != "Camp, moved uphill\n" {
            return Err("The edits and the untracked file did not come back".into());
        }
        git_is(&root, &["show", ":fire.txt"], "Fire, banked\n")?;
        self.until("the three changes listed again", |tab| files(tab, &["fire.txt"], &["camp.txt", "notes.txt"])).await?;
        self.pass();
        Ok(())
    }

    async fn refuse_after_an_outside_change(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1007");
        let root = init(dir, "Outside-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        fs::write(root.join("camp.txt"), "Camp\n")?;
        git(&root, &["add", "--", "camp.txt"])?;
        self.open(&root)?;
        self.until("Outside-trail open with camp.txt staged", |tab| tab.ui.repo == "Outside-trail" && files(tab, &["camp.txt"], &[]))
            .await?;
        self.type_commit("Pitch the camp")?;
        self.until("the camp committed", |tab| tab.ui.files.is_empty() && tab.ui.notices.newest().is_some_and(|n| n.title == "Changes committed"))
            .await?;
        record(&root, "lake.txt", "Lake\n", "Map the lake")?;
        let outside = oid(&root, "main")?;
        self.action(Action::Refresh)?;
        self.until("the outside commit shown", |tab| tab.ui.head.as_deref() == Some(outside.as_str())).await?;
        let before = snapshot(&root)?;
        self.action(Action::Run("edit.undo"))?;
        self.until("the undo refused", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error)
        })
        .await?;
        let said = self.said()?.ok_or("No notice")?;
        if said.1 != "Something else changed main since. Undo cannot be applied safely." {
            return Err(format!("The refused undo said {said:?}").into());
        }
        if snapshot(&root)? != before {
            return Err(format!("The refused undo changed Git: {:?}", snapshot(&root)?).into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{outside}\n"))?;
        self.pass();
        Ok(())
    }

    async fn refuse_to_undo_a_push(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1008");
        let root = init(dir, "Push-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        let origin = dir.join("Push-trail-origin.git");
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", path])?;
        git(&root, &["remote", "add", "origin", path])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        fs::write(root.join("summit.txt"), "Summit\n")?;
        git(&root, &["add", "--", "summit.txt"])?;
        self.open(&root)?;
        self.until("Push-trail open with summit.txt staged", |tab| tab.ui.repo == "Push-trail" && files(tab, &["summit.txt"], &[]))
            .await?;
        self.type_commit("Mark the summit")?;
        self.until("the summit committed", |tab| tab.ui.files.is_empty() && tab.ui.notices.newest().is_some_and(|n| n.title == "Changes committed"))
            .await?;
        let summit = oid(&root, "main")?;
        self.until("main one ahead of origin/main", |tab| tab.ui.refs.branch("main").is_some_and(|item| item.ahead == 1)).await?;
        self.action(Action::Sync(crate::sync::Action::Push))?;
        self.until("main pushed", |tab| !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == "Pushed Successfully")).await?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{summit}\n"))?;
        let rows = self.menu_rows_now()?;
        if rows[0] != (journal::PUSHED.to_owned(), false) {
            return Err(format!("After the push the Edit menu reads {rows:?}").into());
        }
        let row = self.tab.read_with(self.cx, |tab, cx| {
            commands::find("edit.undo").map(|command| (command.when)(&tab.ui, &Target::Repo, cx))
        })?;
        if row != Some(Availability::Disabled(journal::PUSHED.to_owned())) {
            return Err(format!("The palette's undo row is {row:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-z")?;
        self.until("the reason given", |tab| tab.ui.notices.newest().is_some_and(|n| n.title == journal::PUSHED)).await?;
        git_is(&root, &["rev-parse", "main", "refs/remotes/origin/main"], &format!("{summit}\n{summit}\n"))?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{summit}\n"))?;
        self.pass();
        Ok(())
    }

    async fn hide_private_refs(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1009");
        let root = init(dir, "Hidden-trail")?;
        record(&root, "trail.txt", "Trail\n", "Start the trail")?;
        git(&root, &["switch", "-qc", "spur"])?;
        record(&root, "spur.txt", "Spur\n", "Scout the spur")?;
        let scouted = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        self.open(&root)?;
        let spur = Target::Branch("spur".into());
        self.until("Hidden-trail open with spur in the graph", |tab| {
            tab.ui.repo == "Hidden-trail" && tab.ui.history.iter().any(|row| row.oid == scouted) && tab.ui.refs.oid_of(&spur).is_some()
        })
        .await?;
        self.choose(&spur, "branch.delete")?;
        self.asked_about("Delete branch spur?").await?;
        self.key(CMD_RETURN)?;
        self.settled_notice("Deleted branch spur").await?;
        let held = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/fastkraken/"])?;
        if !held.starts_with("refs/fastkraken/undo/") || !held.contains(&scouted) {
            return Err(format!("Git holds {held:?} under refs/fastkraken").into());
        }
        self.until("spur's commit gone from the graph", |tab| !tab.ui.history.iter().any(|row| row.oid == scouted)).await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            let labels = tab.ui.history.iter().any(|row| row.refs.contains("fastkraken"));
            let listed = tab.ui.refs.items.iter().any(|item| item.name.to_string_lossy().contains("fastkraken"));
            let found = tab.repo.as_ref().map(|repo| repo.search_history("Scout", 10).map(|rows| rows.len()));
            (labels, listed, found)
        })?;
        if shown != (false, false, Some(Ok(0))) {
            return Err(format!("With the private ref in place the graph shows {shown:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-z")?;
        self.settled_notice("Undid: Delete branch spur").await?;
        self.until("spur's commit back in the graph", |tab| {
            tab.ui.history.iter().any(|row| row.oid == scouted && row.refs.split(", ").any(|label| label == "spur"))
        })
        .await?;
        self.pass();
        Ok(())
    }
}
