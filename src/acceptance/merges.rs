use super::*;
use crate::merge::{Action as MergeAction, Choice, Side};

impl Replay<'_> {
    pub(super) async fn merges(&mut self, dir: &Path) -> Result<()> {
        self.start("OBS13");
        let clean = init(dir, "Cedar-clean-merge")?;
        record(&clean, "trail.txt", TRAIL, "Start Cedar guide")?;
        git(&clean, &["switch", "-qc", "feature/map"])?;
        record(&clean, "map.txt", "Route: Creek\n", "Add creek map")?;
        let map = oid(&clean, "HEAD")?;
        git(&clean, &["switch", "-q", "main"])?;
        record(&clean, "notes.txt", NOTE, NOTE_COMMIT)?;
        let main = oid(&clean, "main")?;
        self.open(&clean)?;
        self.until("clean merge repository opened", |tab| {
            tab.ui.repo == "Cedar-clean-merge" && tab.ui.branch == "main"
        })
        .await?;
        self.begin_merge("feature/map").await?;
        self.until("clean merge joins both tips and lists map.txt", |tab| {
            !tab.ui.operation.active()
                && tab.ui.history.iter().any(|r| {
                    r.parents == [main.clone(), map.clone()]
                        && tab.ui.selection == Some(Selection::Commit(r.oid.clone()))
                })
                && tab.ui.inspection.files.len() == 1
                && committed_file(tab, "map.txt", "Added")
        })
        .await?;
        git_is(
            &clean,
            &["show", "-s", "--format=%P", "HEAD"],
            &format!("{main} {map}\n"),
        )?;
        git_is(
            &clean,
            &["diff", "--name-status", "HEAD^1", "HEAD"],
            "A\tmap.txt\n",
        )?;
        git_is(&clean, &["status", "--porcelain=v1"], "")?;
        self.pass();

        self.start("OBS17");
        let root = init(dir, "Cedar-detour")?;
        let base = "FastKraken: git for the impatient\nWaypoint: Old ford\n";
        record(&root, "trail.txt", base, "Start Cedar guide")?;
        git(&root, &["switch", "-qc", "feature/detour"])?;
        let incoming = "FastKraken: git for the impatient\nWaypoint: Ridge pass\n";
        record(&root, "trail.txt", incoming, "Record ridge detour")?;
        let theirs = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, "Add east signpost")?;
        let ours = oid(&root, "main")?;
        self.open(&root)?;
        self.until("detour repository opened", |tab| {
            tab.ui.repo == "Cedar-detour" && tab.ui.branch == "main"
        })
        .await?;
        self.begin_merge("feature/detour").await?;
        self.until("merge conflict and trail.txt", |tab| {
            tab.ui.operation.active()
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && !tab.ui.operation.can_finish()
                && tab.ui.notice_text().to_lowercase().contains("merge conflict")
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "UU trail.txt\n")?;
        git_is(&root, &["rev-parse", "MERGE_HEAD"], &format!("{theirs}\n"))?;
        self.pass();

        self.start("OBS18");
        self.action(Action::Operation(crate::operation::Action::Abort))?;
        self.until("Abort merge asks first", |tab| {
            matches!(tab.ui.prompt.asked(), Some(crate::prompt::Kind::Operation(crate::operation::Ask::Abort(_))))
        })
        .await?;
        self.action(Action::Prompt(crate::prompt::Action::Confirm))?;
        self.until(
            "merge controls closed and original tip checked out",
            |tab| {
                !tab.ui.operation.active()
                    && files(tab, &[], &[])
                    && tab.ui.branch == "main"
                    && tab.ui.selection == Some(Selection::Commit(ours.clone()))
            },
        )
        .await?;
        git_is(&root, &["rev-parse", "main"], &format!("{ours}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if fs::read_to_string(root.join("trail.txt"))? != WAYPOINT
            || root.join(".git/MERGE_HEAD").exists()
        {
            return Err("Abort did not restore the original merge state".into());
        }
        self.pass();

        self.start("OBS19");
        self.begin_merge("feature/detour").await?;
        self.until("repeated merge stopped on trail", |tab| {
            tab.ui.operation.conflicted() == [Path::new("trail.txt")]
        })
        .await?;
        self.open(&root)?;
        self.until("reopened merge controls", |tab| {
            tab.ui.operation.conflicted() == [Path::new("trail.txt")]
        })
        .await?;
        self.action(Action::Merge(MergeAction::Open("trail.txt".into())))?;
        self.until("three-way editor has shared output", |tab| {
            tab.ui.merge.editing() == Some(Path::new("trail.txt"))
                && tab.ui.merge.preview() == ["FastKraken: git for the impatient"]
        })
        .await?;
        self.action(Action::Merge(MergeAction::ChooseLine(0, 0, Choice::Both)))?;
        self.until("both waypoints in preview", |tab| {
            tab.ui.merge.preview() == [
                "FastKraken: git for the impatient",
                "Waypoint: Willow bridge",
                "Waypoint: Ridge pass",
            ]
        })
        .await?;
        self.action(Action::Merge(MergeAction::ToggleLine(0, 0, Side::Current)))?;
        self.until("incoming waypoint in preview", |tab| {
            tab.ui.merge.preview() == ["FastKraken: git for the impatient", "Waypoint: Ridge pass"]
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "UU trail.txt\n")?;
        if !fs::read_to_string(root.join("trail.txt"))?.contains("<<<<<<<") {
            return Err("Preview wrote the file before Save".into());
        }
        self.action(Action::Merge(MergeAction::Save))?;
        self.until("resolution staged and merge ready", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.operation.can_finish()
        })
        .await?;
        git_is(&root, &["ls-files", "-u"], "")?;
        git_is(&root, &["show", ":trail.txt"], incoming)?;
        if fs::read_to_string(root.join("trail.txt"))? != incoming {
            return Err("Worktree differs from saved output".into());
        }
        self.pass();

        self.start("OBS20");
        self.action(Action::Operation(crate::operation::Action::Continue))?;
        self.until("selected merge with first-parent trail.txt change", |tab| {
            !tab.ui.operation.active()
                && tab.ui.history.iter().any(|r| {
                    r.parents == [ours.clone(), theirs.clone()]
                        && tab.ui.selection == Some(Selection::Commit(r.oid.clone()))
                })
                && committed_file(tab, "trail.txt", "Modified")
                && files(tab, &[], &[])
        })
        .await?;
        git_is(
            &root,
            &["show", "-s", "--format=%P", "HEAD"],
            &format!("{ours} {theirs}\n"),
        )?;
        git_is(
            &root,
            &["diff", "--name-status", "HEAD^1", "HEAD"],
            "M\ttrail.txt\n",
        )?;
        git_is(&root, &["show", "HEAD:trail.txt"], incoming)?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    pub(super) async fn begin_merge(&mut self, name: &str) -> Result<()> {
        self.ref_list().await?;
        self.until("incoming branch available to merge", |tab| {
            tab.ui
                .refs
                .items
                .iter()
                .any(|r| r.kind == refs::Kind::Branch && r.name == name)
        })
        .await?;
        self.action(Action::Merge(MergeAction::Start(crate::merge::Source::Branch(name.into()), crate::merge::Mode::FastForwardIfPossible)))
    }
}
