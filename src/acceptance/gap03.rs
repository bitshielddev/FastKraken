use super::*;
use crate::{
    commands::{self, Target},
    merge::{self, Side},
    notice::{self, Notice},
    operation, prompt,
    reverse::{self, Operation},
    sync::{self, Mode, PullDefault, Strategy},
    view::{Centre, Tone},
};

const REFUSED: &str = "Finish or abort the rebase first";

impl Replay<'_> {
    pub(super) async fn gap03(&mut self, dir: &Path) -> Result<()> {
        self.external_rebase(dir).await?;
        self.browse_during_merge(dir).await?;
        self.edit_merge_message(dir).await?;
        self.take_binary_side(dir).await?;
        self.delete_on_one_side(dir).await?;
        self.mark_edited_resolved(dir).await?;
        self.mark_with_markers(dir).await?;
        self.finish_conflicted_revert(dir).await?;
        self.abort_conflicted_revert(dir).await?;
        self.abort_conflicted_stash(dir).await?;
        self.resolve_conflicted_pop(dir).await?;
        self.pick_cleanly(dir).await?;
        self.pick_with_conflict(dir).await?;
        self.pick_what_is_there(dir).await?;
        self.rebase_cleanly(dir).await?;
        self.rebase_and_skip(dir).await?;
        self.rebase_and_abort(dir).await?;
        self.pull_with_rebase(dir).await?;
        self.pull_default_remembered(dir).await?;
        self.stash_and_retry_switch(dir).await?;
        self.squash_merge(dir).await
    }

    async fn ferry_conflict(&mut self, dir: &Path, name: &str) -> Result<(PathBuf, String, String)> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Add the ferry timetable")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ferry landing\n", "Route the trail to the ferry")?;
        let ferry = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let main = oid(&root, "main")?;
        self.open(&root)?;
        self.until("the ferry repository open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.history.iter().any(|row| row.oid == ferry)
        })
        .await?;
        self.begin_merge("feature/ferry").await?;
        self.until("the merge stopped on trail.txt", |tab| {
            tab.ui.operation.active() && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
        })
        .await?;
        Ok((root, main, ferry))
    }

    pub(super) fn focus_desk(&mut self) -> Result<()> {
        self.tab.update(self.cx, |tab, window, _| crate::view::focus(&tab.ui, window))?;
        Ok(())
    }

    async fn browse_during_merge(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0302");
        let (root, main, ferry) = self.ferry_conflict(dir, "Ferry-landing").await?;
        let status = git(&root, &["status", "--porcelain=v1"])?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let row = self.tab.read_with(self.cx, |tab, _| {
            (operation::row_text(&tab.ui), operation::owns_panel(&tab.ui), operation::reminder(&tab.ui))
        })?;
        let expected = (
            Some(("Merging feature/ferry into main".to_owned(), "1 file needs resolving".to_owned())),
            true,
            None,
        );
        if row != expected {
            return Err(format!("During the merge the uncommitted row and panel are {row:?}").into());
        }

        self.action(Action::Select(Selection::Commit(ferry.clone())))?;
        self.until("the ferry commit's details beside the strip", |tab| {
            tab.ui.selection == Some(Selection::Commit(ferry.clone()))
                && tab.ui.inspection.files.len() == 1
                && committed_file(tab, "trail.txt", "Modified")
                && !operation::owns_panel(&tab.ui)
                && operation::reminder(&tab.ui).as_deref() == Some("1 conflict in the working tree")
        })
        .await?;
        let offered = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.view-conflicts"], cx)
                .into_iter()
                .map(|entry| (entry.title, entry.disabled))
                .collect::<Vec<_>>()
        })?;
        if offered != [("View conflicts".to_owned(), None)] {
            return Err(format!("During the merge View conflicts is {offered:?}").into());
        }

        self.focus_desk()?;
        self.key("down")?;
        self.until("the next commit selected from the keyboard", |tab| {
            matches!(&tab.ui.selection, Some(Selection::Commit(oid)) if *oid != ferry)
                && operation::reminder(&tab.ui).is_some()
        })
        .await?;
        self.key("up")?;
        self.until("the ferry commit selected again", |tab| {
            tab.ui.selection == Some(Selection::Commit(ferry.clone())) && tab.ui.inspection.files.len() == 1
        })
        .await?;

        self.action(Action::Inspect(crate::inspection::Action::OpenFile("trail.txt".into())))?;
        self.until("the ferry commit's trail.txt open", |tab| {
            crate::view::centre(&tab.ui) == Centre::Diff
                && tab.ui.diff.lines.iter().any(|line| tab.ui.diff.text(line).contains("Waypoint: Ferry landing"))
        })
        .await?;
        self.focus_desk()?;
        self.key("escape")?;
        self.until("the patch closed and the commit still selected", |tab| {
            crate::view::centre(&tab.ui) == Centre::History
                && tab.ui.selection == Some(Selection::Commit(ferry.clone()))
        })
        .await?;

        self.action(operation::view_conflicts())?;
        self.until("the merge's panel again", |tab| {
            tab.ui.selection == Some(Selection::Changes)
                && operation::owns_panel(&tab.ui)
                && operation::reminder(&tab.ui).is_none()
        })
        .await?;
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("the resolver open", |tab| crate::view::centre(&tab.ui) == Centre::Resolver).await?;
        self.focus_desk()?;
        self.key("escape")?;
        self.until("the resolver closed", |tab| {
            crate::view::centre(&tab.ui) == Centre::History && tab.ui.merge.editing().is_none()
        })
        .await?;

        git_is(&root, &["status", "--porcelain=v1"], &status)?;
        git_is(&root, &["ls-files", "--stage"], &index)?;
        git_is(&root, &["rev-parse", "MERGE_HEAD", "HEAD"], &format!("{ferry}\n{main}\n"))?;
        self.pass();
        Ok(())
    }

    async fn edit_merge_message(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0303");
        let (root, main, ferry) = self.ferry_conflict(dir, "Ferry-message").await?;
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("the resolver open", |tab| tab.ui.merge.editing() == Some(Path::new("trail.txt"))).await?;
        self.action(Action::Merge(merge::Action::Choose(0, Side::Incoming)))?;
        self.action(Action::Merge(merge::Action::Save))?;
        self.until("trail.txt resolved and the merge ready", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.operation.can_finish()
        })
        .await?;
        self.until("the merge message in the box", |tab| {
            operation::shows_message(&tab.ui)
        })
        .await?;
        let prefilled = self.tab.read_with(self.cx, |tab, cx| tab.ui.operation_message.read(cx).text().to_owned())?;
        if prefilled != "Merge branch 'feature/ferry'" {
            return Err(format!("The merge message box holds {prefilled:?}").into());
        }
        let row = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.commit"], cx)
                .into_iter()
                .map(|entry| (entry.title, entry.disabled))
                .collect::<Vec<_>>()
        })?;
        if row != [("Commit merge".to_owned(), None)] {
            return Err(format!("With the merge ready the commit row is {row:?}").into());
        }

        let message = "Merge the ferry route into main\n\nKept the ferry landing in place of the Willow bridge.";
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.operation_message.update(cx, |field, cx| {
                field.set_text("", cx);
                window.focus(&gpui::Focusable::focus_handle(field, cx));
                field.replace_text_in_range(None, message, window, cx);
                field.text() == message
            })
        })?;
        if !typed {
            return Err("The merge message box did not take the text".into());
        }
        self.key(if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" })?;
        self.until("the merge committed with the edited message", |tab| {
            !tab.ui.operation.active()
                && tab.ui.history.iter().any(|row| row.parents == [main.clone(), ferry.clone()])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Merge committed")
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%B", "HEAD"], &format!("{message}\n\n"))?;
        git_is(&root, &["show", "-s", "--format=%P", "HEAD"], &format!("{main} {ferry}\n"))?;
        git_is(&root, &["show", "HEAD:trail.txt"], "FastKraken: git for the impatient\nWaypoint: Ferry landing\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if root.join(".git/MERGE_HEAD").exists() {
            return Err("MERGE_HEAD is still there after the merge was committed".into());
        }
        self.pass();
        Ok(())
    }

    async fn files_conflict(
        &mut self,
        dir: &Path,
        name: &str,
        base: impl Fn(&Path) -> Result<()>,
        main_side: impl Fn(&Path) -> Result<()>,
        ferry_side: impl Fn(&Path) -> Result<()>,
        conflicted: &[&str],
    ) -> Result<PathBuf> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        base(&root)?;
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-qm", "Chart the crossing"])?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        ferry_side(&root)?;
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-qm", "Route the trail to the ferry"])?;
        let ferry = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        main_side(&root)?;
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-qm", WAYPOINT_COMMIT])?;
        self.open(&root)?;
        self.until("the repository open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.history.iter().any(|row| row.oid == ferry)
        })
        .await?;
        self.begin_merge("feature/ferry").await?;
        let expected: Vec<&Path> = conflicted.iter().map(Path::new).collect();
        self.until("the merge stopped on its conflicts", |tab| {
            tab.ui.operation.active() && tab.ui.operation.conflicted() == expected
        })
        .await?;
        Ok(root)
    }

    fn conflict_rows(&mut self, path: &str) -> Result<Vec<(String, String, bool)>> {
        let target = Target::Conflict(path.into());
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &target, cx)
                .into_iter()
                .map(|entry| (entry.id.to_owned(), entry.title, entry.action.is_some()))
                .collect()
        })?)
    }

    fn conflict_command(&mut self, path: &str, id: &str) -> Result<()> {
        let target = Target::Conflict(path.into());
        let action = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &target, &[id], cx).into_iter().next().and_then(|entry| entry.action)
        })?;
        self.action(action.ok_or_else(|| format!("{id} is not enabled for {path}"))?)
    }

    async fn take_binary_side(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0304");
        let chart = |bytes: &'static [u8]| move |root: &Path| -> Result<()> { Ok(fs::write(root.join("chart.png"), bytes)?) };
        let root = self
            .files_conflict(
                dir,
                "Ferry-chart",
                chart(b"\x89PNG\0chart of the crossing"),
                chart(b"\x89PNG\0chart with the bridge"),
                chart(b"\x89PNG\0chart with the ferry"),
                &["chart.png"],
            )
            .await?;
        let stages = git(&root, &["ls-files", "--unmerged", "--", "chart.png"])?;
        self.action(Action::Merge(merge::Action::Open("chart.png".into())))?;
        self.until("chart.png's versions over the graph", |tab| {
            crate::view::centre(&tab.ui) == Centre::Chooser && tab.ui.conflict.showing() == Some(Path::new("chart.png"))
        })
        .await?;
        let said = self.tab.read_with(self.cx, |tab, _| {
            let snapshot = tab.ui.operation.snapshot()?;
            Some(crate::conflict::sentence(snapshot, snapshot.conflict(Path::new("chart.png"))?))
        })?;
        if said.as_deref() != Some("Both sides changed chart.png, a binary file. Choose the version to keep.") {
            return Err(format!("The chooser says {said:?}").into());
        }
        let rows = self.conflict_rows("chart.png")?;
        let expected = [
            ("conflict.take-ours", "Take main"),
            ("conflict.take-theirs", "Take feature/ferry"),
            ("conflict.mark-resolved", "Mark resolved"),
            ("conflict.open", "Choose a version"),
            ("file.edit", "Open in external editor…"),
            ("file.open", "Open with default app"),
            ("file.reveal", "Reveal in Finder"),
            ("file.copy-path", "Copy path"),
            ("file.copy-full-path", "Copy full path"),
        ]
        .map(|(id, title)| (id.to_owned(), title.to_owned(), true));
        if rows != expected {
            return Err(format!("A binary conflict's menu is {rows:?}").into());
        }

        self.conflict_command("chart.png", "conflict.take-theirs")?;
        self.until("chart.png resolved with the ferry's chart", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("chart.png")]
                && crate::view::centre(&tab.ui) == Centre::History
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Resolved chart.png")
        })
        .await?;
        let ferry = oid(&root, "feature/ferry:chart.png")?;
        git_is(&root, &["ls-files", "--stage", "--", "chart.png"], &format!("100644 {ferry} 0\tchart.png\n"))?;
        if fs::read(root.join("chart.png"))? != b"\x89PNG\0chart with the ferry" {
            return Err("chart.png does not hold the ferry's chart".into());
        }
        if !root.join(".git/MERGE_HEAD").exists() {
            return Err("Taking a side ended the merge".into());
        }

        let rows = self.conflict_rows("chart.png")?;
        let unresolve = ("conflict.unresolve".to_owned(), "Mark as conflicted again…".to_owned(), true);
        if rows.first() != Some(&unresolve) || rows.iter().any(|(id, ..)| id.starts_with("conflict.") && *id != unresolve.0) {
            return Err(format!("A resolved file's menu is {rows:?}").into());
        }
        self.conflict_command("chart.png", "conflict.unresolve")?;
        self.until("the question before putting chart.png back", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Conflict(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == "Mark chart.png as conflicted again?" && spec.danger)
        })
        .await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("chart.png in conflict again", |tab| {
            tab.ui.operation.conflicted() == [Path::new("chart.png")] && tab.ui.operation.resolved().is_empty()
        })
        .await?;
        git_is(&root, &["ls-files", "--unmerged", "--", "chart.png"], &stages)?;
        if fs::read(root.join("chart.png"))? != b"\x89PNG\0chart with the bridge" {
            return Err("chart.png did not go back to main's chart".into());
        }
        self.pass();
        Ok(())
    }

    async fn delete_on_one_side(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0305");
        let root = self
            .files_conflict(
                dir,
                "Ferry-timetable",
                |root| Ok(fs::write(root.join("timetable.txt"), "Ferry: Hourly crossings.\n")?),
                |root| Ok(fs::write(root.join("timetable.txt"), "Ferry: Crossings every half hour.\n")?),
                |root| Ok(fs::remove_file(root.join("timetable.txt"))?),
                &["timetable.txt"],
            )
            .await?;
        self.action(Action::Merge(merge::Action::Open("timetable.txt".into())))?;
        self.until("the keep-or-delete choice over the graph", |tab| {
            crate::view::centre(&tab.ui) == Centre::Chooser
        })
        .await?;
        let said = self.tab.read_with(self.cx, |tab, _| {
            let snapshot = tab.ui.operation.snapshot()?;
            Some(crate::conflict::sentence(snapshot, snapshot.conflict(Path::new("timetable.txt"))?))
        })?;
        if said.as_deref() != Some("timetable.txt was changed on main and deleted on feature/ferry.") {
            return Err(format!("The chooser says {said:?}").into());
        }
        let rows = self.conflict_rows("timetable.txt")?;
        let expected = [
            ("conflict.keep", "Keep the changed file"),
            ("conflict.delete", "Delete the file"),
            ("conflict.open", "Choose a version"),
            ("file.edit", "Open in external editor…"),
            ("file.open", "Open with default app"),
            ("file.reveal", "Reveal in Finder"),
            ("file.copy-path", "Copy path"),
            ("file.copy-full-path", "Copy full path"),
        ]
        .map(|(id, title)| (id.to_owned(), title.to_owned(), true));
        if rows != expected {
            return Err(format!("A changed-and-deleted conflict's menu is {rows:?}").into());
        }

        self.conflict_command("timetable.txt", "conflict.delete")?;
        self.until("timetable.txt resolved by deleting it", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("timetable.txt")]
                && tab.ui.operation.can_finish()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Resolved timetable.txt")
        })
        .await?;
        git_is(&root, &["ls-files", "--stage", "--", "timetable.txt"], "")?;
        git_is(&root, &["status", "--porcelain=v1", "--", "timetable.txt"], "D  timetable.txt\n")?;
        if root.join("timetable.txt").exists() {
            return Err("timetable.txt is still in the working tree".into());
        }
        self.pass();
        Ok(())
    }

    async fn mark_edited_resolved(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0306");
        let (root, _, _) = self.ferry_conflict(dir, "Ferry-edited").await?;
        let edited = "FastKraken: git for the impatient\nWaypoint: Willow bridge, then the ferry landing\n";
        fs::write(root.join("trail.txt"), edited)?;
        self.conflict_command("trail.txt", "conflict.mark-resolved")?;
        self.until("trail.txt marked resolved without a question", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Marked trail.txt resolved")
        })
        .await?;
        let asked = self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())?;
        if asked {
            return Err("Marking a file without markers asked a question".into());
        }
        git_is(&root, &["show", ":trail.txt"], edited)?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        self.pass();
        Ok(())
    }

    async fn mark_with_markers(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0307");
        let (root, _, _) = self.ferry_conflict(dir, "Ferry-markers").await?;
        let stages = git(&root, &["ls-files", "--unmerged"])?;
        let markers = fs::read(root.join("trail.txt"))?;
        self.conflict_command("trail.txt", "conflict.mark-resolved")?;
        self.until("the question about the markers left in trail.txt", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Conflict(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.title == "Mark trail.txt resolved anyway?"
                        && spec.lines.first().map(String::as_str) == Some("This file still contains conflict markers.")
                        && spec.confirm.as_ref() == "Mark resolved"
                })
        })
        .await?;
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        self.until("the question gone", |tab| !tab.ui.prompt.open()).await?;
        git_is(&root, &["ls-files", "--unmerged"], &stages)?;
        self.conflict_command("trail.txt", "conflict.mark-resolved")?;
        self.until("the question again", |tab| matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Conflict(_)))).await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("trail.txt marked resolved with its markers", |tab| {
            tab.ui.operation.conflicted().is_empty() && tab.ui.operation.resolved() == [Path::new("trail.txt")]
        })
        .await?;
        if git(&root, &["show", ":trail.txt"])?.as_bytes() != markers.as_slice() {
            return Err("trail.txt was not staged as it was".into());
        }
        self.pass();
        Ok(())
    }

    fn newest_said(&mut self) -> Result<Option<(Tone, String)>> {
        Ok(self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|n| (n.tone, n.title.clone())))?)
    }

    async fn external_rebase(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0301");
        let root = init(dir, "Switchback")?;
        record(&root, "trail.txt", TRAIL, "Start the switchback trail")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "map.txt", "Map: Follow the creek.\n", "Map the creek")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ridge pass\n", "Climb the ridge")?;
        record(&root, "map.txt", "Map: Follow the creek to the ridge.\n", "Mark the ridge")?;
        let ridge = oid(&root, "HEAD~1")?;
        let tip = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        git(&root, &["switch", "-q", "feature/ridge"])?;
        self.open(&root)?;
        self.until("Switchback open on feature/ridge", |tab| {
            tab.ui.branch == "feature/ridge"
                && tab.ui.operation.kind().is_none()
                && tab.ui.history.iter().any(|row| row.subject == "Mark the ridge")
        })
        .await?;
        let offered = self.tab.update(self.cx, |tab, _, cx| {
            let id = tab.ui.notices.show(
                Notice::new(Tone::Success, "Created branch main", "")
                    .offer("Switch to main", Action::Refs(refs::Action::Switch("main".into())))
                    .sticky(),
            );
            cx.notify();
            id
        })?;

        let rebase = repository::bundled_command(&root)?.env("GIT_EDITOR", "true").args(["rebase", "main"]).output()?;
        if rebase.status.success() {
            return Err("The rebase in the terminal did not stop on a conflict".into());
        }
        let banner = "Rebasing feature/ridge onto main · commit 2 of 3 · 1 file needs resolving";
        self.until("the rebase named in the banner", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner)
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && tab.ui.operation.active()
        })
        .await?;
        let sides = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.operation.snapshot().map(|s| (s.ours(), s.theirs()))
        })?;
        let expected = (
            "main, with the commits replayed so far".to_owned(),
            format!("your commit {} Climb the ridge", notice::short(&ridge)),
        );
        if sides.as_ref() != Some(&expected) {
            return Err(format!("The rebase's sides are named {sides:?}").into());
        }
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.stage-all"], cx)
                .into_iter()
                .map(|entry| entry.disabled)
                .collect::<Vec<_>>()
        })?;
        if rows != [Some(REFUSED.to_owned())] {
            return Err(format!("During the rebase Stage all changes is {rows:?}").into());
        }

        let index = git(&root, &["ls-files", "--stage"])?;
        let status = git(&root, &["status", "--porcelain=v1"])?;
        let trail = fs::read(root.join("trail.txt"))?;
        self.action(Action::StageAll)?;
        self.until("the staging refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == REFUSED && n.tone == Tone::Info)
        })
        .await?;
        git_is(&root, &["ls-files", "--stage"], &index)?;
        git_is(&root, &["status", "--porcelain=v1"], &status)?;
        if fs::read(root.join("trail.txt"))? != trail {
            return Err("Refused staging changed trail.txt".into());
        }

        let refusal = self.newest_notice_id()?;
        self.action(Action::Notice(notice::Action::Dismiss(refusal)))?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the offer refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == REFUSED && n.tone == Tone::Info)
        })
        .await?;
        let still = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.get(offered).map(|n| n.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>())
        })?;
        if still != Some(vec!["Switch to main".to_owned()]) {
            return Err(format!("After the refusal the earlier notice offers {still:?}").into());
        }
        git_is(&root, &["ls-files", "--stage"], &index)?;

        git(&root, &["rebase", "--abort"])?;
        self.until("the rebase gone", |tab| {
            tab.ui.operation.kind().is_none()
                && tab.ui.operation.snapshot().is_none()
                && tab.ui.branch == "feature/ridge"
                && tab.ui.head.as_deref() == Some(tip.as_str())
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("main checked out", |tab| tab.ui.branch == "main").await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        let said = self.newest_said()?;
        if said.as_ref().is_some_and(|(_, title)| title == REFUSED) {
            return Err(format!("After the rebase the offer said {said:?}").into());
        }
        self.pass();
        Ok(())
    }

    fn prompt_shown(&mut self) -> Result<Option<(String, String, Vec<String>, Option<(String, bool)>)>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            prompt::shown(&tab.ui).map(|spec| {
                (
                    spec.title,
                    spec.confirm.to_string(),
                    spec.choices.iter().map(|c| c.label.to_string()).collect(),
                    spec.tick.map(|(label, on)| (label.to_string(), on)),
                )
            })
        })?)
    }

    async fn finish_conflicted_revert(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0308");
        let root = init(dir, "Willow-revert")?;
        record(&root, "trail.txt", TRAIL, "Start the trail")?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let waypoint = oid(&root, "HEAD")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Willow bridge, rebuilt\n", "Rebuild the Willow bridge")?;
        let tip = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Willow-revert open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.head.as_deref() == Some(tip.as_str()) && tab.ui.history.iter().any(|row| row.oid == waypoint)
        })
        .await?;
        self.select_commit(&waypoint).await?;
        self.reversal_prompt(reverse::Action::Start(Operation::Revert)).await?;
        let shown = self.prompt_shown()?;
        let expected = (
            "Revert this commit on main?".to_owned(),
            "Revert commit".to_owned(),
            Vec::new(),
            Some(("Commit the revert straight away".to_owned(), true)),
        );
        if shown.as_ref() != Some(&expected) {
            return Err(format!("The revert prompt shows {shown:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;

        let label = format!("{} {WAYPOINT_COMMIT}", notice::short(&waypoint));
        let banner = format!("Reverting {label} on main · 1 file needs resolving");
        self.until("the revert stopped in the operation panel", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && operation::owns_panel(&tab.ui)
                && tab.ui.operation.continue_label() == Some("Commit revert")
                && tab.ui.operation.blocked().as_deref() == Some("Resolve 1 file first")
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.tone == Tone::Error && n.title.starts_with("The revert stopped on a conflict.")
                })
        })
        .await?;
        let abort = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.abort-operation"], cx)
                .into_iter()
                .map(|entry| (entry.title, entry.disabled))
                .collect::<Vec<_>>()
        })?;
        if abort != [("Abort revert…".to_owned(), None)] {
            return Err(format!("During the revert the abort row is {abort:?}").into());
        }

        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("the resolver open", |tab| tab.ui.merge.editing() == Some(Path::new("trail.txt"))).await?;
        self.action(Action::Merge(merge::Action::Choose(0, Side::Incoming)))?;
        self.action(Action::Merge(merge::Action::Save))?;
        self.until("trail.txt resolved and the revert ready", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.operation.can_finish()
                && operation::shows_message(&tab.ui)
        })
        .await?;
        let message = format!("Revert \"{WAYPOINT_COMMIT}\"\n\nThis reverts commit {waypoint}.");
        let prefilled = self.tab.read_with(self.cx, |tab, cx| tab.ui.operation_message.read(cx).text().to_owned())?;
        if prefilled != message {
            return Err(format!("The revert message box holds {prefilled:?}").into());
        }

        self.action(Action::Operation(operation::Action::Continue))?;
        self.until("the revert committed", |tab| {
            !tab.ui.operation.active()
                && tab.ui.history.iter().any(|row| row.parents == [tip.clone()] && row.refs.contains("HEAD -> main"))
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Revert committed")
        })
        .await?;
        git_is(&root, &["show", "-s", "--format=%P", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["log", "-1", "--format=%B", "HEAD"], &format!("{message}\n\n"))?;
        git_is(&root, &["show", "HEAD:trail.txt"], TRAIL)?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        if root.join(".git/REVERT_HEAD").exists() {
            return Err("REVERT_HEAD is still there after the revert was committed".into());
        }
        self.pass();
        Ok(())
    }

    async fn abort_conflicted_revert(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0309");
        let root = init(dir, "Ferry-undo")?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        record(&root, "notes.txt", NOTE, NOTE_COMMIT)?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ferry landing\n", "Route the trail to the ferry")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "bridge.txt", "Bridge: Willow\n", "Build the Willow bridge")?;
        let bridge = oid(&root, "HEAD")?;
        git(&root, &["merge", "-q", "--no-ff", "-m", "Merge branch 'feature/ferry'", "feature/ferry"])?;
        let merge = oid(&root, "HEAD")?;
        record(
            &root,
            "trail.txt",
            "FastKraken: git for the impatient\nWaypoint: Ferry landing, with a pier\n",
            "Add a pier to the ferry landing",
        )?;
        let tip = oid(&root, "HEAD")?;
        fs::write(root.join("notes.txt"), "Remember to mark the bridge and the pier.\n")?;
        fs::write(root.join("plan.txt"), "Plan: a second pier.\n")?;
        self.open(&root)?;
        self.until("Ferry-undo open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.head.as_deref() == Some(tip.as_str()) && tab.ui.history.iter().any(|row| row.oid == merge)
        })
        .await?;
        let before = (
            git(&root, &["status", "--porcelain=v1", "--untracked-files=all"])?,
            git(&root, &["ls-files", "--stage"])?,
            fs::read(root.join("trail.txt"))?,
            fs::read(root.join("notes.txt"))?,
            fs::read(root.join("plan.txt"))?,
        );

        self.select_commit(&merge).await?;
        self.reversal_prompt(reverse::Action::Start(Operation::Revert)).await?;
        let shown = self.prompt_shown()?;
        let expected = (
            "Revert this merge on main?".to_owned(),
            "Undo what feature/ferry brought in".to_owned(),
            vec![format!("Undo what {} brought in", notice::short(&bridge))],
            Some(("Commit the revert straight away".to_owned(), true)),
        );
        if shown.as_ref() != Some(&expected) {
            return Err(format!("The merge revert prompt shows {shown:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Toggle))?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let label = format!("{} Merge branch 'feature/ferry'", notice::short(&merge));
        let banner = format!("Reverting {label} on main, not committed · 1 file needs resolving");
        self.until("the uncommitted revert stopped on trail.txt", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && tab.ui.operation.continue_label() == Some(operation::UNCOMMITTED)
                && operation::owns_panel(&tab.ui)
        })
        .await?;
        git_is(&root, &["rev-parse", "REVERT_HEAD"], &format!("{merge}\n"))?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["ls-files", "--unmerged", "--", "bridge.txt"], "")?;

        self.action(Action::Operation(operation::Action::Abort))?;
        let title = format!("Abort the revert of {label} on main?");
        self.until("the abort question", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.title == title && spec.confirm.as_ref() == "Abort revert" && spec.danger
                })
        })
        .await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the revert aborted", |tab| {
            !tab.ui.operation.active()
                && tab.ui.operation.snapshot().is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Revert aborted")
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        let after = (
            git(&root, &["status", "--porcelain=v1", "--untracked-files=all"])?,
            git(&root, &["ls-files", "--stage"])?,
            fs::read(root.join("trail.txt"))?,
            fs::read(root.join("notes.txt"))?,
            fs::read(root.join("plan.txt"))?,
        );
        if after != before {
            return Err(format!("After the abort the repository is {after:?}, not {before:?}").into());
        }
        for left in ["REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE"] {
            if root.join(".git").join(left).exists() {
                return Err(format!("{left} is still there after the abort").into());
            }
        }

        self.select_commit(&merge).await?;
        self.reversal_prompt(reverse::Action::Start(Operation::Revert)).await?;
        self.action(Action::Prompt(prompt::Action::Toggle))?;
        self.action(Action::Prompt(prompt::Action::Choose(0)))?;
        self.until("the revert left staged", |tab| {
            !tab.ui.operation.active()
                && tab.ui.selection == Some(Selection::Changes)
                && files(tab, &["bridge.txt"], &["notes.txt", "plan.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Revert staged")
        })
        .await?;
        let draft = self.tab.read_with(self.cx, |tab, cx| tab.ui.draft(cx))?;
        let reversing = format!("This reverts commit {merge}, reversing");
        if draft.summary != "Revert \"Merge branch 'feature/ferry'\"" || !draft.description.starts_with(&reversing) {
            return Err(format!("After the uncommitted revert the commit box holds {draft:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "D  bridge.txt\n M notes.txt\n?? plan.txt\n")?;
        if root.join(".git/REVERT_HEAD").exists() || root.join(".git/MERGE_MSG").exists() {
            return Err("The revert left staged is still in progress".into());
        }
        self.pass();
        Ok(())
    }

    async fn stash_conflict(&mut self, dir: &Path, name: &str, title: &str) -> Result<(PathBuf, String)> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        record(&root, "notes.txt", NOTE, NOTE_COMMIT)?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Add the ferry timetable")?;
        fs::write(root.join("trail.txt"), "FastKraken: git for the impatient\nWaypoint: Willow bridge, with a bench\n")?;
        fs::write(root.join("ferry.txt"), "Ferry: Crossings every half hour.\n")?;
        fs::write(root.join("camp.txt"), "Camp: Below the ridge.\n")?;
        git(&root, &["stash", "push", "-q", "--include-untracked", &format!("--message={title}")])?;
        let stash = oid(&root, "refs/stash")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Willow bridge, rebuilt\n", "Rebuild the Willow bridge")?;
        let tip = oid(&root, "HEAD")?;
        fs::write(root.join("notes.txt"), "Remember to mark the bridge and the bench.\n")?;
        fs::write(root.join("plan.txt"), "Plan: a second bench.\n")?;
        self.open(&root)?;
        self.until("the repository open with its stash", |tab| {
            tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(tip.as_str())
                && tab.ui.stash.title(&stash) == Some(title)
                && files(tab, &[], &["notes.txt", "plan.txt"])
        })
        .await?;
        self.action(Action::Select(Selection::Stash(stash.clone())))?;
        self.until("the stash selected", |tab| {
            matches!(&tab.ui.selection, Some(Selection::Stash(oid)) if *oid == stash) && tab.ui.stash.files.len() == 3
        })
        .await?;
        Ok((root, stash))
    }

    fn worktree(root: &Path) -> Result<(String, String, Vec<Option<Vec<u8>>>)> {
        let files = ["trail.txt", "notes.txt", "ferry.txt", "plan.txt", "camp.txt"]
            .iter()
            .map(|name| fs::read(root.join(name)).ok())
            .collect();
        Ok((git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?, git(root, &["ls-files", "--stage"])?, files))
    }

    async fn stash_stopped(&mut self, title: &str, notice: &'static str) -> Result<()> {
        let banner = format!("Applying stash {title} · 1 file needs resolving");
        self.until("the stash apply stopped in the operation panel", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && operation::owns_panel(&tab.ui)
                && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title.starts_with(notice))
        })
        .await?;
        let sides = self.tab.read_with(self.cx, |tab, cx| {
            let snapshot = tab.ui.operation.snapshot().map(|s| (s.ours(), s.theirs()));
            let abort = commands::listed(&tab.ui, &Target::Repo, &["repo.abort-operation"], cx)
                .into_iter()
                .map(|entry| (entry.title, entry.disabled))
                .collect::<Vec<_>>();
            (snapshot, abort)
        })?;
        let expected = (
            Some(("your files".to_owned(), "stashed changes".to_owned())),
            vec![(format!("{}…", operation::ABORT_STASH), None)],
        );
        if sides != expected {
            return Err(format!("During the stash apply the sides and abort row are {sides:?}").into());
        }
        Ok(())
    }

    async fn abort_conflicted_stash(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0310");
        let (root, stash) = self.stash_conflict(dir, "Camp-stash", "Camp plans").await?;
        let before = Self::worktree(&root)?;
        let list = git(&root, &["stash", "list", "--format=%H %gs"])?;

        self.action(Action::Stash(crate::stash::Action::Apply(stash.clone())))?;
        self.stash_stopped("Camp plans", "Applying the stash stopped on a conflict.").await?;
        git_is(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            "M  ferry.txt\n M notes.txt\nUU trail.txt\n?? camp.txt\n?? plan.txt\n",
        )?;

        self.action(Action::Operation(operation::Action::Abort))?;
        self.until("the abort question", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.title == "Abort applying stash Camp plans?"
                        && spec.confirm.as_ref() == operation::ABORT_STASH
                        && spec.danger
                        && spec.lines.iter().any(|line| line == "The stash stays in the list, so you can apply it again later.")
                })
        })
        .await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the stash apply aborted", |tab| {
            !tab.ui.operation.active()
                && tab.ui.operation.snapshot().is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Stash apply aborted")
                && files(tab, &[], &["notes.txt", "plan.txt"])
                && tab.ui.stash.title(&stash) == Some("Camp plans")
        })
        .await?;
        let after = Self::worktree(&root)?;
        if after != before {
            return Err(format!("After the abort the repository is {after:?}, not {before:?}").into());
        }
        git_is(&root, &["stash", "list", "--format=%H %gs"], &list)?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        self.pass();
        Ok(())
    }

    async fn resolve_conflicted_pop(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0311");
        let (root, stash) = self.stash_conflict(dir, "Bench-pop", "Bench plans").await?;
        let bench = "FastKraken: git for the impatient\nWaypoint: Willow bridge, with a bench\n";

        self.action(Action::Stash(crate::stash::Action::Pop(stash.clone())))?;
        self.stash_stopped("Bench plans", "Popping the stash stopped on a conflict.").await?;
        git_is(&root, &["stash", "list", "--format=%H"], &format!("{stash}\n"))?;

        let rows = self.conflict_rows("trail.txt")?;
        if !rows.iter().any(|(id, title, on)| id == "conflict.take-theirs" && title == "Take stashed changes" && *on) {
            return Err(format!("The conflicted row offers {rows:?}").into());
        }
        self.conflict_command("trail.txt", "conflict.take-theirs")?;
        self.until("the conflicts over, with the stash offered for dropping", |tab| {
            !tab.ui.operation.active()
                && tab.ui.operation.snapshot().is_none()
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.title == "Stash Bench plans applied"
                        && n.actions.iter().map(|(label, _)| label.as_ref()).eq(["Drop the stash now"])
                })
                && tab.ui.stash.title(&stash).is_some()
        })
        .await?;
        if fs::read_to_string(root.join("trail.txt"))? != bench {
            return Err("Taking the stashed changes did not give trail.txt the stash's waypoint".into());
        }
        git_is(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
            "M  ferry.txt\n M notes.txt\nM  trail.txt\n?? camp.txt\n?? plan.txt\n",
        )?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        git_is(&root, &["show", ":trail.txt"], bench)?;
        git_is(&root, &["stash", "list", "--format=%H"], &format!("{stash}\n"))?;

        let offered = self.newest_notice_id()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the stash dropped", |tab| {
            tab.ui.stash.entries.is_empty()
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.title == "Dropped stash Bench plans" && n.actions.iter().map(|(label, _)| label.as_ref()).eq(["Restore"])
                })
        })
        .await?;
        git_is(&root, &["stash", "list"], "")?;
        if fs::read_to_string(root.join("camp.txt"))? != "Camp: Below the ridge.\n" {
            return Err("The popped stash's untracked camp.txt is not there".into());
        }
        self.pass();
        Ok(())
    }

    async fn ferry_picks(&mut self, dir: &Path, name: &str) -> Result<(PathBuf, String, String, String)> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Add the ferry timetable")?;
        let timetable = oid(&root, "HEAD")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ferry landing\n", "Route the trail to the ferry")?;
        let route = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let tip = oid(&root, "main")?;
        self.open(&root)?;
        self.until("the ferry repository open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.head.as_deref() == Some(tip.as_str()) && tab.ui.history.iter().any(|row| row.oid == route)
        })
        .await?;
        Ok((root, tip, timetable, route))
    }

    async fn pick_prompt(&mut self, commit: &str) -> Result<()> {
        self.select_commit(commit).await?;
        self.choose(&Target::Commit(commit.to_owned()), "commit.cherry-pick")?;
        self.until("the cherry-pick prompt open", |tab| matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Pick(_)))).await
    }

    fn authored(root: &Path, commit: &str) -> Result<String> {
        git(root, &["log", "-1", "--format=%an <%ae> %ad%n%B", commit])
    }

    async fn pick_cleanly(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0312");
        let (root, tip, timetable, _) = self.ferry_picks(dir, "Ferry-pick").await?;
        let ferry = oid(&root, "feature/ferry")?;
        self.select_commit(&timetable).await?;
        let rows = self.menu_rows(&Target::Commit(timetable.clone()))?;
        let ids: Vec<_> = rows.iter().map(|row| row.0).collect();
        let at = ids.iter().position(|id| *id == "commit.cherry-pick");
        if at.is_none_or(|at| at == 0 || ids[at - 1] != "commit.merge")
            || rows.iter().find(|row| row.0 == "commit.cherry-pick").map(|row| (row.1.as_str(), row.2.clone()))
                != Some(("Cherry-pick onto main…", None))
        {
            return Err(format!("The menu of the ferry timetable lists {rows:?}").into());
        }
        self.select_commit(&tip).await?;
        if self.menu_rows(&Target::Commit(tip.clone()))?.iter().any(|row| row.0 == "commit.cherry-pick") {
            return Err("The menu of main's own tip offers a cherry-pick".into());
        }

        self.pick_prompt(&timetable).await?;
        let shown = self.prompt_shown()?;
        let expected = (
            "Cherry-pick this commit onto main?".to_owned(),
            "Cherry-pick commit".to_owned(),
            Vec::new(),
            Some(("Commit straight away".to_owned(), true)),
        );
        if shown.as_ref() != Some(&expected) {
            return Err(format!("The cherry-pick prompt shows {shown:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the pick committed", |tab| {
            !tab.ui.busy()
                && tab.ui.head.as_deref().is_some_and(|head| head != tip)
                && tab.ui.notices.newest().is_some_and(|n| n.title.starts_with("Cherry-picked "))
        })
        .await?;
        let new = oid(&root, "HEAD")?;
        let title = self.newest_said()?;
        let said = Some((Tone::Success, format!("Cherry-picked {} as {}", notice::short(&timetable), notice::short(&new))));
        if title != said {
            return Err(format!("After the pick the notice is {title:?}").into());
        }
        git_is(&root, &["show", "-s", "--format=%P", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        if git(&root, &["diff", "HEAD^", "HEAD"])? != git(&root, &["diff", &format!("{timetable}^"), &timetable])? {
            return Err("The new commit's patch is not the timetable's".into());
        }
        if Self::authored(&root, "HEAD")? != Self::authored(&root, &timetable)? {
            return Err("The new commit's author or message is not the timetable's".into());
        }
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        git_is(&root, &["rev-parse", "feature/ferry"], &format!("{ferry}\n"))?;
        self.pass();
        Ok(())
    }

    async fn take_picked_waypoint(&mut self) -> Result<()> {
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("the resolver open", |tab| tab.ui.merge.editing() == Some(Path::new("trail.txt"))).await?;
        self.action(Action::Merge(merge::Action::Choose(0, Side::Incoming)))?;
        self.action(Action::Merge(merge::Action::Save))
    }

    async fn pick_with_conflict(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0313");
        let (root, tip, _, route) = self.ferry_picks(dir, "Ferry-conflict").await?;
        self.pick_prompt(&route).await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let label = format!("{} Route the trail to the ferry", notice::short(&route));
        let banner = format!("Cherry-picking {label} onto main · 1 file needs resolving");
        self.until("the pick stopped in the operation panel", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && operation::owns_panel(&tab.ui)
                && tab.ui.operation.continue_label() == Some("Continue cherry-pick")
                && tab.ui.operation.blocked().as_deref() == Some("Resolve 1 file first")
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.tone == Tone::Error && n.title.starts_with("The cherry-pick stopped on a conflict.")
                })
        })
        .await?;
        let abort = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.abort-operation"], cx)
                .into_iter()
                .map(|entry| (entry.title, entry.disabled))
                .collect::<Vec<_>>()
        })?;
        if abort != [("Abort cherry-pick…".to_owned(), None)] {
            return Err(format!("During the pick the abort row is {abort:?}").into());
        }
        self.take_picked_waypoint().await?;
        self.until("trail.txt resolved and the pick ready", |tab| {
            tab.ui.operation.conflicted().is_empty() && tab.ui.operation.can_finish() && operation::shows_message(&tab.ui)
        })
        .await?;
        let prefilled = self.tab.read_with(self.cx, |tab, cx| tab.ui.operation_message.read(cx).text().to_owned())?;
        if prefilled != "Route the trail to the ferry" {
            return Err(format!("The pick's message box holds {prefilled:?}").into());
        }
        self.action(Action::Operation(operation::Action::Continue))?;
        self.until("the pick continued", |tab| {
            !tab.ui.operation.active()
                && !tab.ui.busy()
                && tab.ui.head.as_deref().is_some_and(|head| head != tip)
                && tab.ui.notices.newest().is_some_and(|n| n.title.starts_with("Cherry-picked "))
        })
        .await?;
        let new = oid(&root, "HEAD")?;
        let said = Some((Tone::Success, format!("Cherry-picked {} as {}", notice::short(&route), notice::short(&new))));
        if self.newest_said()? != said {
            return Err(format!("After Continue the notice is {:?}", self.newest_said()?).into());
        }
        git_is(&root, &["show", "-s", "--format=%P", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["show", "HEAD:trail.txt"], "FastKraken: git for the impatient\nWaypoint: Ferry landing\n")?;
        git_is(&root, &["log", "-1", "--format=%an <%ae> %ad%n%s", "HEAD"], &git(&root, &["log", "-1", "--format=%an <%ae> %ad%n%s", &route])?)?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        if root.join(".git/CHERRY_PICK_HEAD").exists() {
            return Err("CHERRY_PICK_HEAD is still there after Continue".into());
        }

        let (root, tip, _, route) = self.ferry_picks(dir, "Ferry-staged").await?;
        self.pick_prompt(&route).await?;
        self.action(Action::Prompt(prompt::Action::Toggle))?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let label = format!("{} Route the trail to the ferry", notice::short(&route));
        let banner = format!("Cherry-picking {label} onto main, not committed · 1 file needs resolving");
        self.until("the uncommitted pick stopped on trail.txt", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.kind() == Some(operation::Kind::Applied)
                && tab.ui.operation.continue_label().is_none()
                && tab.ui.operation.abort_label() == Some(operation::ABORT_PICK)
                && operation::owns_panel(&tab.ui)
        })
        .await?;
        if root.join(".git/CHERRY_PICK_HEAD").exists() {
            return Err("An uncommitted pick left CHERRY_PICK_HEAD".into());
        }
        self.take_picked_waypoint().await?;
        self.until("the uncommitted pick resolved and staged", |tab| {
            !tab.ui.operation.active()
                && files(tab, &["trail.txt"], &[])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Cherry-pick staged")
        })
        .await?;
        let draft = self.tab.read_with(self.cx, |tab, cx| tab.ui.draft(cx))?;
        if draft.summary != "Route the trail to the ferry" {
            return Err(format!("After the uncommitted pick the commit box holds {draft:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "M  trail.txt\n")?;
        git_is(&root, &["show", ":trail.txt"], "FastKraken: git for the impatient\nWaypoint: Ferry landing\n")?;
        self.pass();
        Ok(())
    }

    async fn pick_what_is_there(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0314");
        let root = init(dir, "Ferry-twice")?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        let first = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Add the ferry timetable")?;
        let timetable = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Copy the ferry timetable")?;
        let tip = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Ferry-twice open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.head.as_deref() == Some(tip.as_str()) && tab.ui.history.iter().any(|row| row.oid == timetable)
        })
        .await?;

        self.select_commit(&first).await?;
        if self.menu_rows(&Target::Commit(first.clone()))?.iter().any(|row| row.0 == "commit.cherry-pick") {
            return Err("The menu of main's first commit offers a cherry-pick".into());
        }
        self.action(Action::Pick(crate::pick::Action::Start))?;
        let refused = format!("main already has {}", notice::short(&first));
        self.until("the pick of main's own commit refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.title == refused)
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())? {
            return Err("A prompt opened for a commit main already has".into());
        }

        let before = git(&root, &["ls-files", "--stage"])?;
        for commit_now in [true, false] {
            let earlier = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|n| (n.id, n.count)))?;
            self.pick_prompt(&timetable).await?;
            if !commit_now {
                self.action(Action::Prompt(prompt::Action::Toggle))?;
            }
            self.action(Action::Prompt(prompt::Action::Confirm))?;
            self.until("the pick reported as already there", |tab| {
                !tab.ui.busy()
                    && tab.ui.notices.newest().is_some_and(|n| Some((n.id, n.count)) != earlier)
                    && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Success && n.title == "main already has these changes")
            })
            .await?;
            git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
            git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
            git_is(&root, &["ls-files", "--unmerged"], "")?;
            if git(&root, &["ls-files", "--stage"])? != before {
                return Err(format!("After the pick (commit {commit_now}) the index changed").into());
            }
            for left in ["CHERRY_PICK_HEAD", "MERGE_MSG", "AUTO_MERGE", "sequencer"] {
                if root.join(".git").join(left).exists() {
                    return Err(format!("{left} is left after the pick (commit {commit_now})").into());
                }
            }
            self.until("no operation shown", |tab| !tab.ui.operation.active() && tab.ui.operation.snapshot().is_none()).await?;
        }
        self.pass();
        Ok(())
    }

    async fn ridge(&mut self, dir: &Path, name: &str, clash: bool, on: &str) -> Result<(PathBuf, String, String, String)> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the ridge trail")?;
        let first = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "map.txt", "Map: Follow the creek.\n", "Map the creek")?;
        if clash {
            record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ridge pass\n", "Climb the ridge")?;
            record(&root, "map.txt", "Map: Follow the creek to the ridge.\n", "Mark the ridge")?;
        } else {
            record(&root, "camp.txt", "Camp: Below the ridge.\n", "Pitch camp")?;
        }
        let ridge = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let main = oid(&root, "main")?;
        git(&root, &["switch", "-q", on])?;
        self.open(&root)?;
        let (shown, main_branch) = (Target::Branch("feature/ridge".into()), Target::Branch("main".into()));
        let on = on.to_owned();
        self.until("the ridge repository open", |tab| {
            tab.ui.branch == on
                && tab.ui.history.iter().any(|row| row.oid == ridge)
                && tab.ui.history.iter().any(|row| row.oid == main)
                && tab.ui.refs.oid_of(&shown).is_some()
                && tab.ui.refs.oid_of(&main_branch).is_some()
        })
        .await?;
        Ok((root, ridge, first, main))
    }

    async fn rebase_prompt(&mut self) -> Result<(String, String, Option<(String, bool)>, Vec<String>)> {
        self.until("the rebase prompt open", |tab| matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Rebase(_)))).await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            prompt::shown(&tab.ui).map(|spec| {
                (spec.title, spec.confirm.to_string(), spec.tick.map(|(label, on)| (label.to_string(), on)), spec.lines)
            })
        })?;
        shown.ok_or_else(|| "The rebase prompt closed".into())
    }

    fn replayed(root: &Path, range: &str) -> Result<String> {
        git(root, &["log", "--reverse", "--format=%s%n%an <%ae>", "--patch", "--no-decorate", range])
            .map(|log| log.lines().filter(|line| !line.starts_with("index ")).collect::<Vec<_>>().join("\n"))
    }

    async fn rebase_cleanly(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0315");
        let remote = dir.join("Ridge-rebase-origin.git");
        git(dir, &["init", "-q", "--bare", "-b", "main", remote.to_str().ok_or("Scratch path is not UTF-8")?])?;
        let root = init(dir, "Ridge-rebase")?;
        record(&root, "trail.txt", TRAIL, "Start the ridge trail")?;
        let base = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "map.txt", "Map: Follow the creek.\n", "Map the creek")?;
        record(&root, "camp.txt", "Camp: Below the ridge.\n", "Pitch camp")?;
        let ridge = oid(&root, "HEAD")?;
        git(&root, &["remote", "add", "origin", remote.to_str().ok_or("Scratch path is not UTF-8")?])?;
        git(&root, &["push", "-q", "-u", "origin", "feature/ridge"])?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let main = oid(&root, "main")?;
        git(&root, &["switch", "-q", "feature/ridge"])?;
        let before = Self::replayed(&root, &format!("{base}..{ridge}"))?;
        self.open(&root)?;
        let (ridge_branch, main_branch) = (Target::Branch("feature/ridge".into()), Target::Branch("main".into()));
        self.until("Ridge-rebase open on feature/ridge", |tab| {
            tab.ui.branch == "feature/ridge"
                && tab.ui.history.iter().any(|row| row.oid == main)
                && tab.ui.refs.oid_of(&main_branch).is_some()
                && tab.ui.refs.oid_of(&Target::RemoteBranch("origin/feature/ridge".into())).is_some()
        })
        .await?;
        let rows = self.menu_rows(&main_branch)?;
        let rebases: Vec<_> = rows.iter().filter(|row| row.0.starts_with("branch.rebase")).cloned().collect();
        let expected = [
            ("branch.rebase-onto", "Rebase feature/ridge onto main…".to_owned(), None),
            ("branch.rebase", "Rebase main onto feature/ridge…".to_owned(), None),
        ];
        let after_merge = rows.iter().position(|row| row.0 == "branch.merge").map(|at| at + 1);
        if rebases != expected || after_merge != rows.iter().position(|row| row.0 == "branch.rebase-onto") {
            return Err(format!("The menu of main lists {rows:?}").into());
        }
        if self.menu_rows(&ridge_branch)?.iter().any(|row| row.0.contains("rebase")) {
            return Err("The checked-out branch's own menu offers a rebase".into());
        }

        self.choose(&main_branch, "branch.rebase-onto")?;
        let shown = self.rebase_prompt().await?;
        let expected = (
            "Rebase feature/ridge onto main?".to_owned(),
            "Rebase feature/ridge".to_owned(),
            None,
            vec![
                "2 commits will be rewritten, replayed one by one on top of main.".to_owned(),
                "origin/feature/ridge already has these commits. You will need to force push afterwards.".to_owned(),
            ],
        );
        if shown != expected {
            return Err(format!("The rebase prompt shows {shown:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the rebase finished", |tab| {
            !tab.ui.busy()
                && tab.ui.head.as_deref().is_some_and(|head| head != ridge)
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Rebased feature/ridge onto main")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        let said = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().map(|n| {
                (n.tone, n.detail.clone(), n.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>())
            })
        })?;
        let expected = (
            Tone::Success,
            format!("2 commits were rewritten. feature/ridge is at {}.", notice::short(&head)),
            vec!["Force push…".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("After the rebase the notice is {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["merge-base", "main", "feature/ridge"], &format!("{main}\n"))?;
        git_is(&root, &["rev-list", "--count", "main..feature/ridge"], "2\n")?;
        git_is(&root, &["rev-parse", "main"], &format!("{main}\n"))?;
        let after = Self::replayed(&root, "main..feature/ridge")?;
        if after != before {
            return Err(format!("The rebased commits are {after:?}, not {before:?}").into());
        }
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() || root.join(".git/rebase-apply").exists() {
            return Err("A rebase is still in progress after a clean rebase".into());
        }

        let (root, ridge, _, main) = self.ridge(dir, "Ridge-switch", false, "main").await?;
        let rows = self.menu_rows(&ridge_branch)?;
        if !rows.iter().any(|row| row.0 == "branch.rebase" && row.1 == "Rebase feature/ridge onto main…" && row.2.is_none()) {
            return Err(format!("The menu of feature/ridge lists {rows:?}").into());
        }
        self.choose(&ridge_branch, "branch.rebase")?;
        let (title, _, _, lines) = self.rebase_prompt().await?;
        if title != "Rebase feature/ridge onto main?"
            || !lines.iter().any(|line| line == "feature/ridge is not checked out. FastKraken switches to it first.")
        {
            return Err(format!("The switching rebase prompt shows {title:?} {lines:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the switching rebase finished", |tab| {
            !tab.ui.busy()
                && tab.ui.branch == "feature/ridge"
                && tab.ui.head.as_deref().is_some_and(|head| head != ridge)
                && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Success && n.title == "Rebased feature/ridge onto main")
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["merge-base", "main", "feature/ridge"], &format!("{main}\n"))?;
        git_is(&root, &["rev-list", "--count", "main..feature/ridge"], "2\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        self.pass();
        Ok(())
    }

    async fn rebase_and_skip(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0316");
        let (root, _, first, main) = self.ridge(dir, "Ridge-skip", true, "feature/ridge").await?;
        let short = notice::short(&main).to_owned();
        self.select_commit(&first).await?;
        if self.menu_rows(&Target::Commit(first.clone()))?.iter().any(|row| row.0 == "commit.rebase-onto") {
            return Err("The menu of a commit feature/ridge already has offers a rebase".into());
        }
        self.select_commit(&main).await?;
        let rows = self.menu_rows(&Target::Commit(main.clone()))?;
        if !rows.iter().any(|row| row.0 == "commit.rebase-onto" && row.1 == "Rebase feature/ridge onto this commit…" && row.2.is_none()) {
            return Err(format!("The menu of main's tip lists {rows:?}").into());
        }
        self.choose(&Target::Commit(main.clone()), "commit.rebase-onto")?;
        let (title, _, _, lines) = self.rebase_prompt().await?;
        if title != format!("Rebase feature/ridge onto {short}?")
            || lines.first().map(String::as_str) != Some(format!("3 commits will be rewritten, replayed one by one on top of {short}.").as_str())
        {
            return Err(format!("The rebase prompt shows {title:?} {lines:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let banner = format!("Rebasing feature/ridge onto {short} · commit 2 of 3 · 1 file needs resolving");
        self.until("the rebase stopped in the operation panel", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner.as_str())
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && operation::owns_panel(&tab.ui)
                && tab.ui.operation.continue_label() == Some("Continue rebase")
                && tab.ui.operation.blocked().as_deref() == Some("Resolve 1 file first")
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.tone == Tone::Error
                        && n.title == "The rebase stopped on a conflict at commit 2 of 3. 1 file needs resolving before it can go on."
                })
        })
        .await?;
        self.action(Action::Operation(operation::Action::Skip))?;
        let done = format!("Rebased feature/ridge onto {short}");
        self.until("the rebase finished without the skipped commit", |tab| {
            !tab.ui.operation.active()
                && !tab.ui.busy()
                && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Success && n.title == done)
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["log", "--format=%s", "main..feature/ridge"], "Mark the ridge\nMap the creek\n")?;
        git_is(&root, &["merge-base", "main", "feature/ridge"], &format!("{main}\n"))?;
        git_is(&root, &["show", "feature/ridge:trail.txt"], WAYPOINT)?;
        git_is(&root, &["show", "feature/ridge:map.txt"], "Map: Follow the creek to the ridge.\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "")?;
        if root.join(".git/rebase-merge").exists() {
            return Err("The rebase is still in progress after Skip".into());
        }
        self.pass();
        Ok(())
    }

    async fn rebase_and_abort(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0317");
        let (root, ridge, _, main) = self.ridge(dir, "Ridge-abort", true, "main").await?;
        fs::write(root.join("notes.txt"), "Notes: Pack a lamp.\n")?;
        let ridge_branch = Target::Branch("feature/ridge".into());
        self.choose(&ridge_branch, "branch.rebase")?;
        self.rebase_prompt().await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let banner = "Rebasing feature/ridge onto main · commit 2 of 3 · 1 file needs resolving";
        self.until("the rebase stopped on trail.txt", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner) && tab.ui.operation.abort_label() == Some("Abort rebase")
        })
        .await?;
        self.action(Action::Operation(operation::Action::Abort))?;
        self.until("the abort question", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.title == "Abort the rebase of feature/ridge onto main?" && spec.confirm.as_ref() == "Abort rebase" && spec.danger
                })
        })
        .await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let detail = format!("feature/ridge is back at {}.", notice::short(&ridge));
        self.until("the rebase aborted", |tab| {
            !tab.ui.operation.active()
                && tab.ui.operation.snapshot().is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Rebase aborted" && n.detail == detail)
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["rev-parse", "feature/ridge"], &format!("{ridge}\n"))?;
        git_is(&root, &["rev-parse", "main"], &format!("{main}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "?? notes.txt\n")?;
        git_is(&root, &["ls-files", "--unmerged"], "")?;
        if fs::read_to_string(root.join("trail.txt"))? != "FastKraken: git for the impatient\nWaypoint: Ridge pass\n" {
            return Err("After the abort trail.txt is not feature/ridge's version".into());
        }
        if root.join(".git/rebase-merge").exists() || root.join(".git/REBASE_HEAD").exists() {
            return Err("The rebase is still in progress after Abort".into());
        }
        self.pass();
        Ok(())
    }

    fn shared_remote(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let origin = dir.join(format!("{name}-origin.git"));
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?.to_owned();
        git(dir, &["init", "-q", "--bare", "-b", "main", &path])?;
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the trail")?;
        git(&root, &["remote", "add", "origin", &path])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        let peer = dir.join(format!("{name}-peer"));
        let peer_path = peer.to_str().ok_or("Scratch path is not UTF-8")?;
        git(
            dir,
            &[
                "clone", "-q", "--no-local", "-c", "core.hooksPath=/dev/null", "-c", "user.name=Cedar Peer", "-c",
                "user.email=peer@example.invalid", "-c", "commit.gpgSign=false", &path, peer_path,
            ],
        )?;
        Ok((root, origin, peer))
    }

    fn peer_pushes(peer: &Path, file: &str, text: &str, subject: &str) -> Result<String> {
        git(peer, &["pull", "-q", "--ff-only"])?;
        record(peer, file, text, subject)?;
        git(peer, &["push", "-q", "origin", "main"])?;
        oid(peer, "HEAD")
    }

    async fn open_tracking(&mut self, root: &Path, head: &str) -> Result<()> {
        self.open(root)?;
        let head = head.to_owned();
        self.until("the repository open on main, tracking origin/main", |tab| {
            tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(head.as_str())
                && tab.ui.upstream.as_deref() == Some("origin/main")
                && !tab.ui.refs.remotes.is_empty()
        })
        .await
    }

    fn pull_rows(&mut self) -> Result<Vec<(String, bool)>> {
        self.tab.read_with(self.cx, |tab, cx| {
            sync::PULL_MENU
                .iter()
                .filter_map(|(id, _)| commands::find(id).and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx)))
                .map(|entry| (entry.title, entry.action.is_some()))
                .collect()
        })
        .map_err(Into::into)
    }

    async fn choose_pull_row(&mut self, id: &str) -> Result<()> {
        self.action(Action::Sync(sync::Action::ToggleOptions))?;
        self.until("the pull menu open", |tab| tab.ui.sync.options_open()).await?;
        let action = self.tab.read_with(self.cx, |tab, cx| {
            commands::find(id).and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx)).and_then(|entry| entry.action)
        })?;
        self.action(action.ok_or_else(|| format!("The pull menu's {id} cannot run"))?)
    }

    async fn pull_with_rebase(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0318");
        let (root, origin, peer) = Self::shared_remote(dir, "Pull-rebase")?;
        let base = oid(&root, "HEAD")?;
        let incoming = Self::peer_pushes(&peer, "bridge.txt", "Bridge: Stone arch.\n", "Bridge the river")?;
        record(&root, "map.txt", "Map: Follow the creek.\n", "Map the creek")?;
        record(&root, "camp.txt", "Camp: Below the ridge.\n", "Pitch camp")?;
        let local = oid(&root, "HEAD")?;
        fs::write(root.join("notes.txt"), "Notes: Pack a lamp.\n")?;
        self.open_tracking(&root, &local).await?;
        let rows = self.pull_rows()?;
        let expected: Vec<(String, bool)> = [
            "Fetch all remotes",
            "Pull (fast-forward if possible)",
            "Pull (fast-forward only)",
            "Pull (rebase)",
        ]
        .into_iter()
        .map(|title| (title.to_owned(), true))
        .collect();
        if rows != expected {
            return Err(format!("The pull menu lists {rows:?}").into());
        }
        self.choose_pull_row("repo.pull-rebase").await?;
        self.until("the rebase pull finished", |tab| {
            !tab.ui.sync.options_open() && tab.ui.notice_text().starts_with("Pulled Successfully — ")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        self.until("the rebased tip shown", |tab| tab.ui.head.as_deref() == Some(head.as_str())).await?;
        let said = self.tab.read_with(self.cx, |tab, _| (tab.ui.notice_text(), tab.ui.sync.button(), tab.ui.head.clone()))?;
        let expected = (
            format!(
                "Pulled Successfully — From origin/main: 1 commit brought in, and main's 2 commits replayed on top. main is at {}.",
                notice::short(&head)
            ),
            "Pull",
            Some(head.clone()),
        );
        if said != expected {
            return Err(format!("After Pull (rebase) the window shows {said:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD~2", "origin/main"], &format!("{incoming}\n{incoming}\n"))?;
        git_is(&root, &["rev-list", "--count", "--merges", "HEAD"], "0\n")?;
        git_is(&root, &["log", "--format=%s", &format!("{base}..HEAD")], "Pitch camp\nMap the creek\nBridge the river\n")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "?? notes.txt\n")?;
        git_is(&origin, &["rev-parse", "main"], &format!("{incoming}\n"))?;
        if root.join(".git/rebase-merge").exists() || root.join(".git/MERGE_HEAD").exists() {
            return Err("The pull left an operation in progress".into());
        }

        let clash = Self::peer_pushes(&peer, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ferry landing\n", "Route the trail to the ferry")?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let local = oid(&root, "HEAD")?;
        self.until("the new commit shown", |tab| tab.ui.head.as_deref() == Some(local.as_str())).await?;
        self.choose_pull_row("repo.pull-rebase").await?;
        let banner = "Rebasing main onto origin/main · commit 3 of 3 · 1 file needs resolving";
        self.until("the rebase pull stopped in the operation panel", |tab| {
            tab.ui.operation.summary().as_deref() == Some(banner)
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && tab.ui.notice_text()
                    == "Pull Failed — The rebase stopped on a conflict at commit 3 of 3. 1 file needs resolving before it can go on."
        })
        .await?;
        git_is(&root, &["rev-parse", "origin/main"], &format!("{clash}\n"))?;
        self.action(Action::Operation(operation::Action::Abort))?;
        self.until("the abort question", |tab| matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(_)))).await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let detail = format!("main is back at {}.", notice::short(&local));
        self.until("the rebase pull aborted", |tab| {
            !tab.ui.operation.active() && tab.ui.notices.newest().is_some_and(|n| n.title == "Rebase aborted" && n.detail == detail)
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "main"], &format!("{local}\n"))?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all"], "?? notes.txt\n")?;
        git_is(&root, &["show", "HEAD:trail.txt"], WAYPOINT)?;
        self.pass();
        Ok(())
    }

    fn pull_button(&mut self) -> Result<(&'static str, PullDefault)> {
        self.tab.read_with(self.cx, |tab, _| (tab.ui.sync.button(), tab.ui.sync.chosen())).map_err(Into::into)
    }

    async fn pull_default_remembered(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0319");
        let (config, _, _) = Self::shared_remote(dir, "Pull-config")?;
        git(&config, &["config", "pull.ff", "only"])?;
        let tip = oid(&config, "HEAD")?;
        self.open_tracking(&config, &tip).await?;
        let ff_only = PullDefault { fetch: false, strategy: Strategy::FastForwardOnly };
        if self.pull_button()? != ("Pull (ff only)", ff_only) {
            return Err(format!("With pull.ff=only the Pull button is {:?}", self.pull_button()?).into());
        }
        git(&config, &["config", "pull.ff", "true"])?;
        git(&config, &["config", "pull.rebase", "true"])?;
        self.open_tracking(&config, &tip).await?;
        if self.pull_button()? != ("Pull (ff only)", ff_only) {
            return Err(format!("After the configuration changed the Pull button is {:?}", self.pull_button()?).into());
        }

        let (root, _, peer) = Self::shared_remote(dir, "Pull-default")?;
        let base = oid(&root, "HEAD")?;
        let incoming = Self::peer_pushes(&peer, "bridge.txt", "Bridge: Stone arch.\n", "Bridge the river")?;
        record(&root, "map.txt", "Map: Follow the creek.\n", "Map the creek")?;
        let local = oid(&root, "HEAD")?;
        self.open_tracking(&root, &local).await?;
        if self.pull_button()? != ("Pull", PullDefault::default()) {
            return Err(format!("A repository with no pull configuration starts with {:?}", self.pull_button()?).into());
        }
        self.action(Action::Sync(sync::Action::ToggleOptions))?;
        self.until("the pull menu open", |tab| tab.ui.sync.options_open()).await?;
        self.action(Action::Sync(sync::Action::SetDefault(Mode::Pull(Strategy::Rebase))))?;
        let rebase = PullDefault { fetch: false, strategy: Strategy::Rebase };
        let shown = self.tab.read_with(self.cx, |tab, _| {
            (tab.ui.sync.options_open(), tab.ui.sync.button(), tab.ui.sync.chosen(), tab.ui.busy(), tab.ui.notices.items().len())
        })?;
        if shown != (true, "Pull (rebase)", rebase, false, 0) {
            return Err(format!("Setting the default shows {shown:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD", "origin/main"], &format!("{local}\n{base}\n"))?;
        self.action(Action::Dismiss)?;

        self.relaunch().await?;
        self.until("Pull-default reopened", |tab| {
            tab.ui.repo == "Pull-default" && tab.ui.head.as_deref() == Some(local.as_str()) && tab.ui.upstream.is_some()
        })
        .await?;
        if self.pull_button()? != ("Pull (rebase)", rebase) {
            return Err(format!("After reopening the Pull button is {:?}", self.pull_button()?).into());
        }
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let remembered: serde_json::Value = serde_json::from_slice(&fs::read(state.join("state.json"))?)?;
        let common = git(&root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let key: Vec<u8> = common.trim_end().as_bytes().to_vec();
        let entry = remembered["repositories"]
            .as_array()
            .and_then(|list| list.iter().find(|view| serde_json::from_value::<Vec<u8>>(view["directory"].clone()).ok() == Some(key.clone())));
        if entry.map(|view| &view["pull"]) != Some(&json!({"fetch": false, "strategy": "rebase"})) {
            return Err(format!("state.json remembers {entry:?} for Pull-default").into());
        }
        self.action(Action::Sync(sync::Action::Button))?;
        self.until("the button's rebase pull finished", |tab| tab.ui.notice_text().starts_with("Pulled Successfully — ")).await?;
        let head = oid(&root, "HEAD")?;
        self.until("the rebased tip shown", |tab| tab.ui.head.as_deref() == Some(head.as_str())).await?;
        let said = self.tab.read_with(self.cx, |tab, _| tab.ui.notice_text())?;
        let expected = format!(
            "Pulled Successfully — From origin/main: 1 commit brought in, and main's 1 commit replayed on top. main is at {}.",
            notice::short(&head)
        );
        if said != expected {
            return Err(format!("The button's pull said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD~1"], &format!("{incoming}\n"))?;
        git_is(&root, &["rev-list", "--count", "--merges", "HEAD"], "0\n")?;
        git_is(&root, &["log", "-1", "--format=%s"], "Map the creek\n")?;

        self.action(Action::Sync(sync::Action::ToggleOptions))?;
        self.until("the pull menu open", |tab| tab.ui.sync.options_open()).await?;
        self.action(Action::Sync(sync::Action::SetDefault(Mode::Fetch)))?;
        self.action(Action::Dismiss)?;
        let pull_title = self.tab.read_with(self.cx, |tab, _| {
            commands::find("repo.pull").map(|command| (command.title)(&tab.ui, &Target::Repo))
        })?;
        if self.pull_button()?.0 != "Fetch" || pull_title.as_deref() != Some("Pull (rebase)") {
            return Err(format!("With Fetch as the default the button is {:?} and Pull is {pull_title:?}", self.pull_button()?).into());
        }
        let later = Self::peer_pushes(&peer, "ferry.txt", "Ferry: Hourly.\n", "Add the ferry")?;
        self.action(Action::Sync(sync::Action::Button))?;
        self.until("the button fetched", |tab| tab.ui.notice_text() == "Fetched Successfully — 1 branch updated.").await?;
        git_is(&root, &["rev-parse", "HEAD", "origin/main"], &format!("{head}\n{later}\n"))?;
        self.pass();
        Ok(())
    }

    fn newest_offers(&mut self) -> Result<Option<(String, String, Vec<String>)>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().map(|n| {
                (n.title.clone(), n.detail.clone(), n.actions.iter().map(|(label, _)| label.to_string()).collect())
            })
        })?)
    }

    async fn stash_and_retry_switch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0320");
        let root = init(dir, "Route-switch")?;
        let route = |start: &str, summit: &str| format!("FastKraken: git for the impatient\n{start}\nFord\n{summit}\n");
        record(&root, "route.txt", &route("Start", "Summit"), "Plan the route")?;
        git(&root, &["switch", "-qc", "ridge"])?;
        record(&root, "route.txt", &route("Start north", "Summit"), "Start north")?;
        let ridge = oid(&root, "ridge")?;
        git(&root, &["switch", "-q", "main"])?;
        let main = oid(&root, "main")?;
        fs::write(root.join("route.txt"), route("Start", "Summit east"))?;
        fs::write(root.join("notes.txt"), "Pack water.\n")?;
        self.open(&root)?;
        self.until("Route-switch open on main with ridge listed", |tab| {
            tab.ui.repo == "Route-switch"
                && tab.ui.branch == "main"
                && tab.ui.changed_count() == 2
                && tab.ui.refs.local(std::ffi::OsStr::new("ridge")).is_some()
        })
        .await?;

        self.action(Action::Refs(refs::Action::Switch("ridge".into())))?;
        self.until("the switch refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title.starts_with("Your changes"))
        })
        .await?;
        let refused = self.newest_offers()?;
        let expected = "Your changes to 1 file are in the way";
        let output = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().and_then(|n| n.output.clone()))?;
        if refused.as_ref().is_none_or(|(title, detail, offers)| {
            title != expected
                || detail != "route.txt. Commit or stash them first."
                || offers != &[crate::autostash::RETRY.to_owned()]
        }) || !output.is_some_and(|output| output.contains("would be overwritten by checkout:"))
        {
            return Err(format!("The refused switch said {refused:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{main}\n"))?;
        git_is(&root, &["stash", "list"], "")?;

        let offered = self.newest_notice_id()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the switch retried", |tab| {
            tab.ui.notice_text() == "Stashed your changes, switched to ridge and restored them — Now on ridge."
                && tab.ui.branch == "ridge"
                && tab.ui.head.as_deref() == Some(ridge.as_str())
                && tab.ui.stash.entries.is_empty()
                && tab.ui.changed_count() == 2
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{ridge}\n"))?;
        if fs::read_to_string(root.join("route.txt"))? != route("Start north", "Summit east") {
            return Err("After the retry route.txt lacks one of the two changes".into());
        }
        if fs::read_to_string(root.join("notes.txt"))? != "Pack water.\n" {
            return Err("After the retry the untracked notes.txt is gone".into());
        }
        git_is(&root, &["status", "--porcelain=v1"], " M route.txt\n?? notes.txt\n")?;
        git_is(&root, &["stash", "list"], "")?;

        let title = |ui: &crate::view::Ui| commands::find("repo.stash-in-the-way").map(|command| (command.title)(ui, &Target::Repo));
        let before = self.tab.read_with(self.cx, |tab, _| title(&tab.ui))?;
        self.action(Action::Run("repo.stash-in-the-way"))?;
        self.until("the setting on", |tab| tab.ui.autostash.always).await?;
        let after = self.tab.read_with(self.cx, |tab, _| title(&tab.ui))?;
        if before.as_deref() != Some("Stash automatically when changes are in the way")
            || after.as_deref() != Some("Ask before stashing changes that are in the way")
        {
            return Err(format!("The setting's command read {before:?}, then {after:?}").into());
        }
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        self.cx.update(|cx| crate::settings::flush(cx))?;
        let chosen: serde_json::Value = serde_json::from_slice(&fs::read(state.join("settings.json"))?)?;
        if chosen["stash_in_the_way"] != true {
            return Err(format!("settings.json holds {chosen}").into());
        }
        self.action(Action::Refs(refs::Action::Switch("main".into())))?;
        self.until("back on main without a question", |tab| {
            tab.ui.notice_text() == "Stashed your changes, switched to main and restored them — Now on main."
                && tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(main.as_str())
        })
        .await?;
        if fs::read_to_string(root.join("route.txt"))? != route("Start", "Summit east") {
            return Err("Back on main, route.txt is not main's with the edit".into());
        }
        git_is(&root, &["stash", "list"], "")?;

        self.action(Action::Run("repo.stash-in-the-way"))?;
        self.until("the setting off", |tab| !tab.ui.autostash.always).await?;
        self.cx.update(|cx| crate::settings::flush(cx))?;
        self.pass();
        Ok(())
    }

    fn merge_submenu(&mut self, target: &Target) -> Result<(String, Option<Action>, Vec<(&'static str, String, bool)>)> {
        let found = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx).into_iter().find(|entry| entry.id == "branch.merge").map(|entry| {
                let rows = entry
                    .children
                    .into_iter()
                    .map(|child| (child.id, child.title, matches!(child.mark, Some(commands::Mark::Default))))
                    .collect();
                (entry.title, entry.action, rows)
            })
        })?;
        found.ok_or_else(|| format!("The menu of {target:?} has no Merge submenu").into())
    }

    async fn squash_merge(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0321");
        let root = init(dir, "Squash-ferry")?;
        record(&root, "trail.txt", TRAIL, "Start the ferry trail")?;
        git(&root, &["switch", "-qc", "feature/ferry"])?;
        record(&root, "ferry.txt", "Ferry: Hourly crossings.\n", "Add the ferry timetable")?;
        record(&root, "camp.txt", "Camp: By the landing.\n", "Mark the camp")?;
        let ferry = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let main = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Squash-ferry open on main with feature/ferry listed", |tab| {
            tab.ui.repo == "Squash-ferry"
                && tab.ui.branch == "main"
                && tab.ui.refs.local(std::ffi::OsStr::new("feature/ferry")).is_some()
        })
        .await?;

        let target = Target::Branch("feature/ferry".into());
        let (title, double, rows) = self.merge_submenu(&target)?;
        let expected = vec![
            ("branch.merge-ff", "Fast-forward if possible".to_owned(), true),
            ("branch.merge-no-ff", "Always create a merge commit".to_owned(), false),
            ("branch.merge-ff-only", "Fast-forward only".to_owned(), false),
            ("branch.merge-squash", "Squash into one change".to_owned(), false),
        ];
        if title != "Merge feature/ferry into main" || rows != expected {
            return Err(format!("The Merge submenu is {title:?} with {rows:?}").into());
        }
        if !matches!(double, Some(Action::Merge(merge::Action::Start(_, merge::Mode::FastForwardIfPossible)))) {
            return Err(format!("A double-click on the Merge item runs {double:?}").into());
        }

        self.choose_in(&target, "branch.merge", "branch.merge-squash")?;
        let said = "Staged feature/ferry as one change — The changes from 2 commits are staged on main, with a message in the commit box. Nothing is committed until you commit it.";
        self.until("the squash staged", |tab| {
            tab.ui.notice_text() == said && !tab.ui.busy() && tab.ui.files.iter().filter(|file| file.staged).count() == 2
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{main}\n"))?;
        git_is(&root, &["rev-list", "--count", "HEAD"], "2\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "A  camp.txt\nA  ferry.txt\n")?;
        git_is(&root, &["diff", "--cached", "feature/ferry", "--", "camp.txt", "ferry.txt"], "")?;
        if root.join(".git/MERGE_HEAD").exists() {
            return Err("The squash left a merge in progress".into());
        }
        let (draft, selection, active) =
            self.tab.read_with(self.cx, |tab, cx| (tab.ui.draft(cx), tab.ui.selection.clone(), tab.ui.operation.active()))?;
        if draft.summary != "Squash merge feature/ferry into main"
            || !draft.description.starts_with("Squashed commit of the following:")
            || !draft.description.contains("Add the ferry timetable")
            || !draft.description.contains("Mark the camp")
        {
            return Err(format!("The commit box holds {draft:?}").into());
        }
        if selection != Some(crate::view::Selection::Changes) || active {
            return Err(format!("After the squash the selection is {selection:?}, operation active: {active}").into());
        }

        self.action(Action::Commit)?;
        self.until("the squash committed", |tab| {
            tab.ui.notice_text() == "Changes committed — Squash merge feature/ferry into main"
                && tab.ui.head.as_deref() != Some(main.as_str())
                && tab.ui.files.is_empty()
        })
        .await?;
        git_is(&root, &["show", "-s", "--format=%P%n%s", "HEAD"], &format!("{main}\nSquash merge feature/ferry into main\n"))?;
        git_is(&root, &["diff", "HEAD", "feature/ferry", "--", "camp.txt", "ferry.txt"], "")?;
        if git(&root, &["merge-base", "--is-ancestor", &ferry, "HEAD"]).is_ok() {
            return Err("feature/ferry became part of main's history".into());
        }
        if root.join(".git/SQUASH_MSG").exists() {
            return Err("SQUASH_MSG is still there after the commit".into());
        }

        self.action(Action::Merge(merge::Action::SetDefault(merge::Mode::Squash)))?;
        self.relaunch().await?;
        self.until("Squash-ferry reopened", |tab| tab.ui.repo == "Squash-ferry" && tab.ui.refs.local(std::ffi::OsStr::new("feature/ferry")).is_some())
            .await?;
        let (_, double, rows) = self.merge_submenu(&target)?;
        let ticked: Vec<_> = rows.iter().filter(|row| row.2).map(|row| row.0).collect();
        if ticked != ["branch.merge-squash"]
            || !matches!(double, Some(Action::Merge(merge::Action::Start(_, merge::Mode::Squash))))
        {
            return Err(format!("After reopening the default is {ticked:?}, and a double-click runs {double:?}").into());
        }
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let remembered: serde_json::Value = serde_json::from_slice(&fs::read(state.join("state.json"))?)?;
        let common = git(&root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let key: Vec<u8> = common.trim_end().as_bytes().to_vec();
        let entry = remembered["repositories"]
            .as_array()
            .and_then(|list| list.iter().find(|view| serde_json::from_value::<Vec<u8>>(view["directory"].clone()).ok() == Some(key.clone())));
        if entry.map(|view| &view["merge"]) != Some(&json!("squash")) {
            return Err(format!("state.json remembers {entry:?} for Squash-ferry").into());
        }
        self.pass();
        Ok(())
    }

    fn newest_notice_id(&mut self) -> Result<u64> {
        let id = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|n| n.id))?;
        id.ok_or_else(|| "There is no notice".into())
    }
}
