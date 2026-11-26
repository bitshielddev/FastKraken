use super::*;
use crate::{
    commands::{self, Availability, Target},
    compare::{self, Rev},
    journal,
    notice,
    picked::How,
    operation,
    prompt,
    rebase_editor::{self, Choice},
    rewrite,
    view::Tone,
};
use gpui::Focusable;

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
        self.hide_private_refs(dir).await?;
        self.reword_the_third_commit(dir).await?;
        self.drop_a_commit(dir).await?;
        self.squash_three_commits(dir).await?;
        self.refuse_a_range_with_a_merge(dir).await?;
        self.reorder_and_drop_in_the_editor(dir).await?;
        self.stop_an_edited_rebase_on_a_conflict(dir).await?;
        self.undo_a_whole_edited_rebase(dir).await?;
        self.warn_before_a_conflicting_merge(dir).await
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

    fn message_fields(&mut self) -> Result<Option<(String, String)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let summary = tab.ui.prompt.field()?.read(cx).text().to_owned();
            let description = tab.ui.prompt.second()?.read(cx).text().to_owned();
            Some((summary, description))
        })?)
    }

    fn write_message(&mut self, summary: &str, description: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, _, cx| {
            let fields = (tab.ui.prompt.field().cloned(), tab.ui.prompt.second().cloned());
            if let (Some(first), Some(second)) = fields {
                first.update(cx, |field, cx| field.set_text(summary, cx));
                second.update(cx, |field, cx| field.set_text(description, cx));
            }
        })?;
        Ok(())
    }

    fn type_in_description(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            if let Some(second) = tab.ui.prompt.second().cloned() {
                window.focus(&second.focus_handle(cx));
                second.update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx));
            }
        })?;
        Ok(())
    }

    fn history_of(root: &Path) -> Result<Vec<String>> {
        Ok(git(root, &["log", "--format=%H %T %an <%ae> %ad", "main"])?.lines().map(str::to_owned).collect())
    }

    async fn reword_the_third_commit(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1010");
        let root = init(dir, "Reword-trail")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        record(&root, "ridge.txt", "Pass\n", "Climb the rdige")?;
        record(&root, "lake.txt", "Cold\n", "Swim in the lake")?;
        record(&root, "summit.txt", "Flag\n", "Reach the summit")?;
        let before = Self::history_of(&root)?;
        let third = oid(&root, "main~2")?;
        let untouched = (oid(&root, "main~3")?, oid(&root, "main~4")?);
        self.open(&root)?;
        self.until("Reword-trail open with five commits", |tab| tab.ui.repo == "Reword-trail" && tab.ui.history.len() == 5 && crate::rebase::on_branch(&tab.ui)).await?;
        self.select_commit(&third).await?;
        let button = self.tab.read_with(self.cx, |tab, cx| {
            rewrite::offered(&tab.ui, "commit.reword", std::slice::from_ref(&third), cx)
                .map(|entry| (entry.title, entry.disabled, entry.action.is_some()))
        })?;
        if button != Some(("Edit commit message…".to_owned(), None, true)) {
            return Err(format!("The details panel's Edit button runs {button:?}").into());
        }
        self.choose(&Target::Commit(third.clone()), "commit.reword")?;
        let (lines, danger) = self.asked_about("Edit commit message").await?;
        if danger || lines != ["Rewording this commit message will cause 3 commits to be rebased."] {
            return Err(format!("The reword prompt reads {lines:?}").into());
        }
        let fields = self.message_fields()?;
        if fields != Some(("Climb the rdige".to_owned(), String::new())) {
            return Err(format!("The reword prompt is prefilled with {fields:?}").into());
        }
        self.write_message("Climb the ridge", "Over the pass")?;
        self.type_in_description("")?;
        self.key("enter")?;
        self.type_in_description("At first light")?;
        let fields = self.message_fields()?;
        if fields != Some(("Climb the ridge".to_owned(), "Over the pass\nAt first light".to_owned())) {
            return Err(format!("After Return in the description the prompt holds {fields:?}").into());
        }
        self.key(CMD_RETURN)?;
        let said = self.settled_notice("Commit message updated").await?;
        let tip = oid(&root, "main")?;
        let detail = format!("3 commits were rewritten. main is at {}.", notice::short(&tip));
        if said != (Tone::Success, "Commit message updated".to_owned(), detail, vec!["Undo".to_owned()]) {
            return Err(format!("Rewording said {said:?}").into());
        }
        git_is(&root, &["log", "-1", "--format=%B", "main~2"], "Climb the ridge\n\nOver the pass\nAt first light\n\n")?;
        git_is(&root, &["log", "--format=%s", "main"], "Reach the summit\nSwim in the lake\nClimb the ridge\nMap the creek\nPitch the camp\n")?;
        let after = Self::history_of(&root)?;
        let content = |lines: &[String]| lines.iter().map(|line| line.split_once(' ').map_or("", |(_, rest)| rest).to_owned()).collect::<Vec<_>>();
        if content(&after) != content(&before) {
            return Err(format!("The trees, authors or dates changed: {before:?} became {after:?}").into());
        }
        if (oid(&root, "main~3")?, oid(&root, "main~4")?) != untouched {
            return Err("The commits below the reworded one were rewritten".into());
        }
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() {
            return Err("The rebase was left in progress".into());
        }
        let rows = self.menu_rows_now()?;
        if rows[0] != ("Undo Reword 'Climb the rdige'".to_owned(), true) {
            return Err(format!("After the reword the Edit menu reads {rows:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn drop_a_commit(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1011");
        let root = init(dir, "Drop-trail")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        let map = oid(&root, "HEAD")?;
        record(&root, "ridge.txt", "Pass\n", "Climb the ridge")?;
        let ridge = oid(&root, "HEAD")?;
        record(&root, "lake.txt", "Cold\n", "Swim in the lake")?;
        let tip = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Drop-trail open with four commits", |tab| tab.ui.repo == "Drop-trail" && tab.ui.history.len() == 4 && crate::rebase::on_branch(&tab.ui)).await?;
        self.select_commit(&ridge).await?;
        self.choose(&Target::Commit(ridge.clone()), "commit.drop")?;
        let short = notice::short(&ridge).to_owned();
        let (lines, danger) = self.asked_about(&format!("Drop commit {short}?")).await?;
        let expected = [
            "This commit and its changes leave main:".to_owned(),
            format!("{short} Climb the ridge"),
            "1 later commit will be rebased.".to_owned(),
            "Undo, in the notice that follows, brings them back.".to_owned(),
        ];
        if !danger || lines != expected {
            return Err(format!("The drop prompt reads {lines:?}").into());
        }
        self.key(CMD_RETURN)?;
        let said = self.settled_notice("Dropped 1 commit from main").await?;
        if said.0 != Tone::Success || said.3 != ["Undo"] {
            return Err(format!("Dropping said {said:?}").into());
        }
        git_is(&root, &["log", "--format=%s", "main"], "Swim in the lake\nMap the creek\nPitch the camp\n")?;
        git_is(&root, &["ls-tree", "--name-only", "main"], "camp.txt\nlake.txt\nmap.txt\n")?;
        git_is(&root, &["rev-parse", "main^"], &format!("{map}\n"))?;
        git_is(&root, &["show", "main:lake.txt"], "Cold\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join("ridge.txt").exists() {
            return Err("ridge.txt is still in the working tree".into());
        }
        let rows = self.menu_rows_now()?;
        if rows[0] != ("Undo Drop 'Climb the ridge'".to_owned(), true) {
            return Err(format!("After the drop the Edit menu reads {rows:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-z")?;
        self.settled_notice("Undid: Drop 'Climb the ridge'").await?;
        git_is(&root, &["rev-parse", "main"], &format!("{tip}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        self.pass();
        Ok(())
    }

    async fn squash_three_commits(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1012");
        let root = init(dir, "Squash-trail")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        let camp = oid(&root, "HEAD")?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        let map = oid(&root, "HEAD")?;
        record(&root, "ridge.txt", "Pass\n", "Climb the ridge")?;
        let ridge = oid(&root, "HEAD")?;
        record(&root, "lake.txt", "Cold\n", "Swim in the lake")?;
        let lake = oid(&root, "HEAD")?;
        record(&root, "summit.txt", "Flag\n", "Reach the summit")?;
        let summit = git(&root, &["rev-parse", "main^{tree}"])?;
        self.open(&root)?;
        self.until("Squash-trail open with five commits", |tab| tab.ui.repo == "Squash-trail" && tab.ui.history.len() == 5 && crate::rebase::on_branch(&tab.ui)).await?;
        self.action(Action::Select(Selection::Commit(lake.clone())))?;
        self.until("Swim in the lake selected", |tab| tab.ui.selection == Some(Selection::Commit(lake.clone()))).await?;
        for oid in [&ridge, &map] {
            self.action(Action::Compare(compare::Action::Pick { rev: Rev::Commit(oid.clone()), how: How::Toggle }))?;
        }
        self.until("three commits picked", |tab| tab.ui.compare.count() == 3 && matches!(tab.ui.selection, Some(Selection::Compare { .. }))).await?;
        let button = self.tab.read_with(self.cx, |tab, cx| {
            rewrite::offered(&tab.ui, "commits.squash", compare::picked_commits(&tab.ui), cx)
                .map(|entry| (entry.title, entry.disabled, entry.action.is_some()))
        })?;
        if button != Some(("Squash 3 commits…".to_owned(), None, true)) {
            return Err(format!("The panel's Squash button is {button:?}").into());
        }
        self.choose(&Target::Commits(vec![lake.clone(), ridge.clone(), map.clone()]), "commits.squash")?;
        let (lines, danger) = self.asked_about("Squash 3 commits").await?;
        if danger || lines != ["Their changes become one commit on main, with the message below.", "1 later commit will be rebased."] {
            return Err(format!("The squash prompt reads {lines:?}").into());
        }
        let fields = self.message_fields()?;
        if fields != Some(("Map the creek".to_owned(), "Climb the ridge\n\nSwim in the lake".to_owned())) {
            return Err(format!("The squash prompt is prefilled with {fields:?}").into());
        }
        self.write_message("Cross the valley", "Creek, ridge and lake")?;
        self.key(CMD_RETURN)?;
        let said = self.settled_notice("Squashed 3 commits into one").await?;
        if said.0 != Tone::Success {
            return Err(format!("Squashing said {said:?}").into());
        }
        git_is(&root, &["log", "--format=%s", "main"], "Reach the summit\nCross the valley\nPitch the camp\n")?;
        git_is(&root, &["log", "-1", "--format=%B", "main^"], "Cross the valley\n\nCreek, ridge and lake\n\n")?;
        git_is(&root, &["diff", "--name-only", "main~2", "main^"], "lake.txt\nmap.txt\nridge.txt\n")?;
        git_is(&root, &["rev-parse", "main~2"], &format!("{camp}\n"))?;
        git_is(&root, &["rev-parse", "main^{tree}"], &summit)?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        self.pass();
        Ok(())
    }

    async fn refuse_a_range_with_a_merge(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1013");
        let root = init(dir, "Merge-trail")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        git(&root, &["switch", "-qc", "side"])?;
        record(&root, "side.txt", "Path\n", "Take the side path")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        let map = oid(&root, "HEAD")?;
        git(&root, &["merge", "-q", "--no-ff", "-m", "Join the side path", "side"])?;
        let merge = oid(&root, "HEAD")?;
        record(&root, "summit.txt", "Flag\n", "Reach the summit")?;
        let tip = oid(&root, "HEAD")?;
        let refs = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)"])?;
        self.open(&root)?;
        self.until("Merge-trail open with the merge", |tab| tab.ui.repo == "Merge-trail" && tab.ui.history.iter().any(|row| row.oid == merge)).await?;
        self.select_commit(&map).await?;
        self.choose(&Target::Commit(map.clone()), "commit.reword")?;
        self.until("the limit stated", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.detail == rewrite::ACROSS_MERGE)
        })
        .await?;
        let said = self.said()?;
        let refusal = (Tone::Info, "The message cannot be edited here".to_owned(), rewrite::ACROSS_MERGE.to_owned(), Vec::new());
        if said.as_ref() != Some(&refusal) || self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())? {
            return Err(format!("Rewording below the merge said {said:?}").into());
        }
        self.action(Action::Select(Selection::Commit(tip.clone())))?;
        self.until("Reach the summit selected", |tab| tab.ui.selection == Some(Selection::Commit(tip.clone()))).await?;
        self.action(Action::Compare(compare::Action::Pick { rev: Rev::Commit(merge.clone()), how: How::Toggle }))?;
        self.until("the summit and the merge picked", |tab| tab.ui.compare.count() == 2).await?;
        let picked = Target::Commits(vec![tip.clone(), merge.clone()]);
        let rows: Vec<_> = self.menu_rows(&picked)?.into_iter().filter(|row| row.0.starts_with("commits.squash") || row.0 == "commits.drop").collect();
        let reason = Some(rewrite::ACROSS_MERGE.to_owned());
        let expected = [
            ("commits.squash", "Squash 2 commits…".to_owned(), reason.clone()),
            ("commits.drop", "Drop 2 commits…".to_owned(), reason.clone()),
        ];
        if rows != expected {
            return Err(format!("With a merge picked the menu lists {rows:?}").into());
        }
        git_is(&root, &["for-each-ref", "--format=%(refname) %(objectname)"], &refs)?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() {
            return Err("A rebase was started".into());
        }
        self.pass();
        Ok(())
    }

    fn rebase_editor_now(&mut self) -> Result<Option<(String, Vec<(String, Choice)>, usize, String, Option<String>)>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            let editor = tab.ui.rebase_editor.editor()?;
            let rows = editor.rows().iter().map(|row| (row.commit.subject.clone(), row.choice)).collect();
            Some((editor.heading(), rows, editor.highlighted(), editor.summary(), editor.start_refusal()))
        })?)
    }

    async fn rebase_editor_opened(&mut self, heading: &str) -> Result<Vec<(String, Choice)>> {
        self.until(&format!("the editor {heading:?} open"), |tab| {
            crate::view::centre(&tab.ui) == crate::view::Centre::RebaseEditor
                && tab.ui.rebase_editor.editor().is_some_and(|editor| editor.heading() == heading)
                && !tab.ui.rebase_editor.loading()
        })
        .await?;
        Ok(self.rebase_editor_now()?.map(|(_, rows, ..)| rows).unwrap_or_default())
    }

    fn picks(subjects: &[&str]) -> Vec<(String, Choice)> {
        subjects.iter().map(|subject| ((*subject).to_owned(), Choice::Pick)).collect()
    }

    async fn reorder_and_drop_in_the_editor(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1014");
        let root = init(dir, "Tidy-trail")?;
        record(&root, "base.txt", "Start\n", "Start the trail")?;
        let main = oid(&root, "main")?;
        git(&root, &["switch", "-qc", "feature"])?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        record(&root, "ridge.txt", "Pass\n", "Climb the ridge")?;
        record(&root, "lake.txt", "Cold\n", "Swim in the lake")?;
        self.open(&root)?;
        let base = Target::Branch("main".into());
        self.until("Tidy-trail open on feature with main listed", |tab| {
            tab.ui.repo == "Tidy-trail" && tab.ui.branch == "feature" && tab.ui.refs.oid_of(&base).is_some()
        })
        .await?;
        let rows = self.menu_rows(&base)?;
        let offered = ("branch.interactive-rebase", "Interactive rebase feature onto main…".to_owned(), None);
        if !rows.contains(&offered) {
            return Err(format!("The menu of main lists {rows:?}").into());
        }
        self.choose(&base, "branch.interactive-rebase")?;
        let rows = self.rebase_editor_opened("Rebasing 4 commits onto main").await?;
        if rows != Self::picks(&["Swim in the lake", "Climb the ridge", "Map the creek", "Pitch the camp"]) {
            return Err(format!("The editor lists {rows:?}").into());
        }
        let refusal = self.rebase_editor_now()?.and_then(|(.., refusal)| refusal);
        if refusal.as_deref() != Some("Nothing has changed yet. Set an action or move a commit first") {
            return Err(format!("Before any change Start says {refusal:?}").into());
        }
        let untouched = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)"])?;
        self.focus_desk()?;
        self.key("down")?;
        self.key("alt-down")?;
        self.key("alt-down")?;
        let now = self.rebase_editor_now()?.map(|(_, rows, at, ..)| (rows, at));
        let moved = Self::picks(&["Swim in the lake", "Map the creek", "Pitch the camp", "Climb the ridge"]);
        if now != Some((moved.clone(), 3)) {
            return Err(format!("After Option-Down twice the editor holds {now:?}").into());
        }
        self.key("up")?;
        self.key("up")?;
        self.key("d")?;
        let mut expected = moved;
        expected[1].1 = Choice::Drop;
        let now = self.rebase_editor_now()?;
        let wanted = ("Rebasing 4 commits onto main".to_owned(), expected, 1, "1 dropped, order changed".to_owned(), None);
        if now.as_ref() != Some(&wanted) {
            return Err(format!("After D the editor holds {now:?}").into());
        }
        git_is(&root, &["for-each-ref", "--format=%(refname) %(objectname)"], &untouched)?;
        self.action(Action::RebaseEditor(rebase_editor::Action::Start))?;
        let said = self.settled_notice("Rebased feature onto main").await?;
        let tip = oid(&root, "feature")?;
        let detail = format!("1 dropped, order changed. feature is at {}.", notice::short(&tip));
        if said.0 != Tone::Success || said.2 != detail || !said.3.contains(&"Undo".to_owned()) {
            return Err(format!("Starting said {said:?}").into());
        }
        git_is(&root, &["log", "--format=%s", "main..feature"], "Swim in the lake\nPitch the camp\nClimb the ridge\n")?;
        git_is(&root, &["ls-tree", "--name-only", "feature"], "base.txt\ncamp.txt\nlake.txt\nridge.txt\n")?;
        git_is(&root, &["merge-base", "main", "feature"], &format!("{main}\n"))?;
        git_is(&root, &["rev-parse", "feature~3"], &format!("{main}\n"))?;
        git_is(&root, &["rev-parse", "main"], &format!("{main}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() || self.tab.read_with(self.cx, |tab, _| tab.ui.rebase_editor.open())? {
            return Err("The rebase or the editor was left open".into());
        }
        self.pass();
        Ok(())
    }

    async fn stop_an_edited_rebase_on_a_conflict(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1015");
        let root = init(dir, "Knot-trail")?;
        record(&root, "base.txt", "Start\n", "Start the trail")?;
        let base = oid(&root, "main")?;
        record(&root, "trail.txt", "One\n", "Mark the first waypoint")?;
        record(&root, "trail.txt", "One\nTwo\n", "Mark the second waypoint")?;
        let tip = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Knot-trail open on main with three commits", |tab| {
            tab.ui.repo == "Knot-trail" && tab.ui.history.len() == 3 && tab.ui.head.as_deref() == Some(tip.as_str()) && tab.ui.branch == "main"
        })
        .await?;
        let onto = Target::Commit(base.clone());
        let rows = self.menu_rows(&onto)?;
        if !rows.contains(&("commit.interactive-rebase", "Interactive rebase main onto this commit…".to_owned(), None)) {
            return Err(format!("The menu of the first commit lists {rows:?}").into());
        }
        let short = notice::short(&base).to_owned();
        self.choose(&onto, "commit.interactive-rebase")?;
        self.rebase_editor_opened(&format!("Rebasing 2 commits onto {short}")).await?;
        self.focus_desk()?;
        self.key("alt-down")?;
        let rows = self.rebase_editor_now()?.map(|(_, rows, ..)| rows);
        if rows != Some(Self::picks(&["Mark the first waypoint", "Mark the second waypoint"])) {
            return Err(format!("After Option-Down the editor lists {rows:?}").into());
        }
        self.action(Action::RebaseEditor(rebase_editor::Action::Start))?;
        let banner = format!("Rebasing main onto {short} · commit 1 of 2 · 1 file needs resolving");
        self.until("the rebase stopped on trail.txt", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && operation::owns_panel(&tab.ui)
                && !tab.ui.busy()
        })
        .await?;
        let state = self.tab.read_with(self.cx, |tab, _| {
            (crate::view::centre(&tab.ui), tab.ui.rebase_editor.open(), tab.ui.operation.abort_label(), tab.ui.notices.newest().map(|n| (n.tone, n.title.clone())))
        })?;
        let stopped = "The rebase stopped on a conflict at commit 1 of 2. 1 file needs resolving before it can go on.".to_owned();
        if state != (crate::view::Centre::History, false, Some("Abort rebase"), Some((Tone::Error, stopped))) {
            return Err(format!("After the stop the window shows {state:?}").into());
        }
        self.action(Action::Operation(operation::Action::Abort))?;
        self.until("the abort question", |tab| matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(_)))).await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the rebase aborted", |tab| {
            !tab.ui.operation.active() && tab.ui.notices.newest().is_some_and(|n| n.title == "Rebase aborted")
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "main"], &format!("{tip}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() {
            return Err("The rebase is still in progress after Abort".into());
        }
        self.pass();
        Ok(())
    }

    async fn undo_a_whole_edited_rebase(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1016");
        let root = init(dir, "Undo-rebase")?;
        record(&root, "base.txt", "Start\n", "Start the trail")?;
        git(&root, &["switch", "-qc", "feature"])?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        record(&root, "map.txt", "Creek\n", "Map the creek")?;
        record(&root, "ridge.txt", "Pass\n", "Climb the ridge")?;
        let before = oid(&root, "feature")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "bridge.txt", "Stone\n", "Bridge the river")?;
        let main = oid(&root, "main")?;
        let refs = git(&root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"])?;
        self.open(&root)?;
        let feature = Target::Branch("feature".into());
        self.until("Undo-rebase open on main with feature listed", |tab| {
            tab.ui.repo == "Undo-rebase" && tab.ui.branch == "main" && tab.ui.refs.oid_of(&feature).is_some()
        })
        .await?;
        let dropped = crate::drag::pair(feature.clone(), Target::Branch("main".into()));
        let rows = self.menu_rows(&dropped)?;
        let offered = ("drop.interactive-rebase", "Switch to feature and interactively rebase it onto main…".to_owned(), None);
        if !rows.contains(&offered) {
            return Err(format!("The drop menu lists {rows:?}").into());
        }
        self.choose(&dropped, "drop.interactive-rebase")?;
        let rows = self.rebase_editor_opened("Rebasing 3 commits onto main").await?;
        if rows != Self::picks(&["Climb the ridge", "Map the creek", "Pitch the camp"]) {
            return Err(format!("The editor lists {rows:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        self.focus_desk()?;
        self.key("down")?;
        self.key("s")?;
        self.key("up")?;
        self.key("enter")?;
        self.asked_about("Edit commit message").await?;
        let fields = self.message_fields()?;
        if fields != Some(("Climb the ridge".to_owned(), String::new())) {
            return Err(format!("The message prompt is prefilled with {fields:?}").into());
        }
        self.write_message("Climb to the ridge", "Over the pass")?;
        self.key(CMD_RETURN)?;
        self.until("the reword set", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.rebase_editor.editor().is_some_and(|editor| editor.rows().first().is_some_and(|row| row.choice == Choice::Reword))
        })
        .await?;
        let now = self.rebase_editor_now()?.map(|(_, rows, _, summary, refusal)| (rows, summary, refusal));
        let wanted = (
            vec![
                ("Climb the ridge".to_owned(), Choice::Reword),
                ("Map the creek".to_owned(), Choice::Squash),
                ("Pitch the camp".to_owned(), Choice::Pick),
            ],
            "1 squashed, 1 reworded".to_owned(),
            None,
        );
        if now.as_ref() != Some(&wanted) {
            return Err(format!("Before Start the editor holds {now:?}").into());
        }
        let switching = self.tab.read_with(self.cx, |tab, _| tab.ui.rebase_editor.editor().map(|editor| editor.loaded().upstream.clone()))?;
        if switching != Some(None) {
            return Err(format!("The editor reports an upstream: {switching:?}").into());
        }
        self.action(Action::RebaseEditor(rebase_editor::Action::Start))?;
        self.settled_notice("Rebased feature onto main").await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature\n")?;
        git_is(&root, &["log", "--format=%s", "main..feature"], "Climb to the ridge\nPitch the camp\n")?;
        git_is(&root, &["log", "-1", "--format=%B", "feature"], "Climb to the ridge\n\nOver the pass\n\n")?;
        git_is(&root, &["diff", "--name-only", "main", "feature~1"], "camp.txt\nmap.txt\n")?;
        git_is(&root, &["rev-parse", "feature~2"], &format!("{main}\n"))?;
        let rows = self.menu_rows_now()?;
        if rows[0] != ("Undo Interactive rebase feature onto main".to_owned(), true) {
            return Err(format!("After the rebase the Edit menu reads {rows:?}").into());
        }
        self.focus_desk()?;
        self.key("cmd-z")?;
        self.settled_notice("Undid: Interactive rebase feature onto main").await?;
        git_is(&root, &["rev-parse", "feature"], &format!("{before}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"], &refs)?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        self.pass();
        Ok(())
    }

    fn forecast_rows(&mut self, open: &view::Menu) -> Result<Vec<(String, String, Option<String>, bool)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let mut found = Vec::new();
            for entry in commands::entries_of(&tab.ui, open, cx) {
                for row in entry.children.iter().chain(std::iter::once(&entry)) {
                    if let Some(note) = &row.note {
                        found.push((entry.id.to_owned(), row.title.clone(), note.tip.clone(), note.warn));
                    }
                }
            }
            found
        })?)
    }

    async fn warn_before_a_conflicting_merge(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1017");
        let origin = dir.join("Forecast-origin.git");
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", path])?;
        let root = init(dir, "Forecast")?;
        record(&root, "notes.txt", "Pack\nWalk\nRest\n", "Start the notes")?;
        record(&root, "map.txt", "North\nSouth\n", "Draw the map")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the camp")?;
        git(&root, &["remote", "add", "origin", path])?;
        git(&root, &["switch", "-qc", "ridge"])?;
        record(&root, "notes.txt", "Pack\nClimb\nRest\n", "Climb instead")?;
        record(&root, "map.txt", "North\nRidge\n", "Mark the ridge")?;
        record(&root, "lake.txt", "Swim\n", "Find the lake")?;
        git(&root, &["switch", "-qc", "pond", "main"])?;
        record(&root, "pond.txt", "Still\n", "Find the pond")?;
        git(&root, &["switch", "-qc", "river", "main"])?;
        record(&root, "camp.txt", "Tents by the river\n", "Move the camp to the river")?;
        git(&root, &["push", "-q", "origin", "river:main"])?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["branch", "-qD", "river"])?;
        git(&root, &["fetch", "-q", "origin"])?;
        git(&root, &["branch", "-q", "--set-upstream-to=origin/main", "main"])?;
        record(&root, "notes.txt", "Pack\nSwim\nRest\n", "Swim instead")?;
        record(&root, "map.txt", "North\nCoast\n", "Mark the coast")?;
        record(&root, "camp.txt", "Tents on the hill\n", "Move the camp to the hill")?;
        let main = oid(&root, "main")?;
        let ridge = Target::Branch("ridge".into());
        let pond = Target::Branch("pond".into());
        self.open(&root)?;
        self.until("Forecast open on main, one ahead of and behind origin/main", |tab| {
            tab.ui.repo == "Forecast"
                && tab.ui.branch == "main"
                && tab.ui.refs.oid_of(&pond).is_some()
                && tab.ui.refs.current("main").is_some_and(|item| item.ahead == 3 && item.behind == 1)
        })
        .await?;
        let before = snapshot(&root)?;
        self.until("the Pull button warned about origin/main", |tab| crate::forecast::pull_warning(&tab.ui).is_some()).await?;
        let warning = self.tab.read_with(self.cx, |tab, _| crate::forecast::pull_warning(&tab.ui))?;
        if warning.as_deref() != Some("Merging origin/main into main will conflict in 1 file: camp.txt") {
            return Err(format!("The Pull button warns {warning:?}").into());
        }
        let (ridge_tip, pond_tip) = (oid(&root, "ridge")?, oid(&root, "pond")?);
        let known = |tab: &Tab, theirs: &str| {
            tab.ui.forecast.of(&main, theirs).is_some_and(|forecast| *forecast != crate::forecast::Forecast::Checking)
        };
        let on_ridge = view::Menu::Object { target: ridge.clone(), at: gpui::Point::default() };
        self.action(Action::Menu(Some(on_ridge.clone())))?;
        self.until("the merge forecast for ridge", |tab| known(tab, &ridge_tip)).await?;
        let rows = self.forecast_rows(&on_ridge)?;
        let named = "map.txt, notes.txt".to_owned();
        let wanted = vec![("branch.merge".to_owned(), format!("Will conflict in 2 files: {named}"), Some(named.clone()), true)];
        if rows != wanted {
            return Err(format!("The menu of ridge forecasts {rows:?}").into());
        }
        let children = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &ridge, cx)
                .into_iter()
                .find(|entry| entry.id == "branch.merge")
                .map(|entry| entry.children.into_iter().map(|child| (child.id, child.action.is_some())).collect::<Vec<_>>())
        })?;
        let modes = Some(vec![
            ("branch.merge-ff", true),
            ("branch.merge-no-ff", true),
            ("branch.merge-ff-only", true),
            ("branch.merge-squash", true),
            ("forecast", false),
        ]);
        if children != modes {
            return Err(format!("The merge submenu of ridge holds {children:?}").into());
        }
        self.action(Action::Menu(None))?;
        let on_pond = view::Menu::Object { target: pond.clone(), at: gpui::Point::default() };
        self.action(Action::Menu(Some(on_pond.clone())))?;
        self.until("the merge forecast for pond", |tab| known(tab, &pond_tip)).await?;
        let rows = self.forecast_rows(&on_pond)?;
        if rows != [("branch.merge".to_owned(), "No conflicts expected".to_owned(), None, false)] {
            return Err(format!("The menu of pond forecasts {rows:?}").into());
        }
        self.action(Action::Menu(None))?;
        let main_branch = Target::Branch("main".into());
        self.action(Action::Drag(crate::drag::Action::Drop {
            dragged: ridge.clone(),
            dragged_also: None,
            onto: main_branch.clone(),
            onto_also: None,
            at: gpui::Point::default(),
        }))?;
        self.until("the drop menu open", |tab| tab.ui.menu.is_some()).await?;
        let dropped = self.tab.read_with(self.cx, |tab, _| tab.ui.menu.clone())?.ok_or("The drop menu closed")?;
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            commands::entries_of(&tab.ui, &dropped, cx).into_iter().map(|entry| (entry.id, entry.title)).collect::<Vec<_>>()
        })?;
        let at = rows.iter().position(|row| row.0 == "drop.merge");
        if at.is_none() || rows.get(at.map_or(0, |at| at + 1)) != Some(&("forecast", format!("Will conflict in 2 files: {named}"))) {
            return Err(format!("The drop menu lists {rows:?}").into());
        }
        self.action(Action::Menu(None))?;
        self.choose(&ridge, "branch.rebase-onto")?;
        let (lines, _) = self.asked_about("Rebase main onto ridge?").await?;
        if lines.last() != Some(&format!("May conflict in 2 files: {named}")) {
            return Err(format!("The rebase prompt says {lines:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        self.until("the rebase prompt closed", |tab| !tab.ui.prompt.open()).await?;
        if snapshot(&root)? != before {
            return Err(format!("Forecasting changed Git: {:?}", snapshot(&root)?).into());
        }
        if root.join(".git/MERGE_HEAD").exists() || root.join(".git/rebase-merge").exists() {
            return Err("Forecasting left a merge or a rebase behind".into());
        }
        self.choose_under(&ridge, "branch.merge", "Always create a merge commit")?;
        self.until("the merge stopped on its conflicts", |tab| !tab.ui.busy() && tab.ui.operation.active()).await?;
        git_is(&root, &["diff", "--name-only", "--diff-filter=U"], "map.txt\nnotes.txt\n")?;
        git(&root, &["merge", "--abort"])?;
        git_is(&root, &["rev-parse", "main"], &format!("{main}\n"))?;
        self.pass();
        Ok(())
    }
}
