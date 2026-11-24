use super::*;
use crate::sync::{Action as SyncAction, Strategy};

struct UpstreamFixture {
    work: PathBuf,
    origin: PathBuf,
    peer: PathBuf,
    base: String,
}

impl UpstreamFixture {
    fn new(
        dir: &Path,
        case: &str,
        name: &str,
        file: &str,
        text: &str,
        subject: &str,
    ) -> Result<Self> {
        let sandbox = dir.join(case);
        fs::create_dir(&sandbox)?;
        let work = init(&sandbox, name)?;
        record(&work, file, text, subject)?;
        let base = oid(&work, "HEAD")?;
        git(
            &sandbox,
            &["init", "--bare", "-q", "-b", "main", "origin.git"],
        )?;
        git(&work, &["remote", "add", "origin", "../origin.git"])?;
        git(&work, &["push", "-qu", "origin", "main"])?;
        git(
            &sandbox,
            &[
                "clone",
                "-q",
                "--no-local",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Cedar Peer",
                "-c",
                "user.email=peer@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "origin.git",
                "peer",
            ],
        )?;
        Ok(Self {
            work,
            base,
            origin: sandbox.join("origin.git"),
            peer: sandbox.join("peer"),
        })
    }

    fn check_refs(&self, local: &str, tracking: &str, bare: &str) -> Result<()> {
        git_is(&self.work, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(
            &self.work,
            &["rev-parse", "refs/heads/main", "refs/remotes/origin/main"],
            &format!("{local}\n{tracking}\n"),
        )?;
        git_is(
            &self.origin,
            &["rev-parse", "refs/heads/main"],
            &format!("{bare}\n"),
        )
    }

    fn peer_update(&self, file: &str, text: &str, subject: &str) -> Result<String> {
        git(&self.peer, &["pull", "-q", "--ff-only"])?;
        record(&self.peer, file, text, subject)?;
        git(&self.peer, &["push", "-q", "origin", "main"])?;
        oid(&self.peer, "HEAD")
    }
}

fn graph_labels(tab: &Tab, local: &str, tracking: &str) -> bool {
    let at = |oid: &str, label: &str| {
        tab.ui
            .history
            .iter()
            .any(|row| row.oid == oid && row.refs.split(", ").any(|decoration| decoration == label))
    };
    tab.ui.branch == "main"
        && tab.ui.upstream.as_deref() == Some("origin/main")
        && tab.ui.head.as_deref() == Some(local)
        && refs::show_labels(&tab.ui.refs)
        && at(local, "HEAD -> main")
        && at(tracking, "origin/main")
}

fn selected_tip(tab: &Tab, oid: &str) -> bool {
    matches!(&tab.ui.selection, Some(Selection::Commit(selected)) if selected == oid)
}

impl Replay<'_> {
    async fn sync_options(&mut self) -> Result<()> {
        self.action(Action::Sync(SyncAction::ToggleOptions))?;
        self.until("pull options visible", |tab| tab.ui.sync.options_open())
            .await
    }

    async fn open_upstream(
        &mut self,
        fixture: &UpstreamFixture,
        local: &str,
        tracking: &str,
    ) -> Result<()> {
        self.open(&fixture.work)?;
        self.until(
            "clean working tree with local and upstream graph labels",
            |tab| {
                graph_labels(tab, local, tracking)
                    && selected_tip(tab, local)
                    && files(tab, &[], &[])
            },
        )
        .await
    }

    pub(super) async fn upstream(&mut self, dir: &Path) -> Result<()> {
        self.start("OBS14");
        let cedar = UpstreamFixture::new(
            dir,
            "upstream-observed",
            "Cedar",
            "trail.txt",
            TRAIL,
            "Add trail description",
        )?;
        git(&cedar.work, &["switch", "-qc", "feature/map"])?;
        record(
            &cedar.work,
            "map.txt",
            "Follow the creek east.\n",
            "Add creek map",
        )?;
        let map = oid(&cedar.work, "HEAD")?;
        git(&cedar.work, &["switch", "-q", "main"])?;
        record(&cedar.work, "notes.txt", NOTE, NOTE_COMMIT)?;
        let notes = oid(&cedar.work, "HEAD")?;
        git(
            &cedar.work,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge creek map",
                "feature/map",
            ],
        )?;
        let merged = oid(&cedar.work, "HEAD")?;
        let parents = format!("{notes} {map}\n");
        git_is(
            &cedar.work,
            &["show", "-s", "--format=%P", "HEAD"],
            &parents,
        )?;
        cedar.check_refs(&merged, &cedar.base, &cedar.base)?;
        self.open_upstream(&cedar, &merged, &cedar.base).await?;
        self.action(Action::Sync(SyncAction::Push))?;
        self.until(
            "push confirmation and both graph labels on the merge",
            |tab| {
                tab.ui.notice_text() == "Pushed Successfully — 3 commits to origin/main."
                    && graph_labels(tab, &merged, &merged)
                    && selected_tip(tab, &merged)
                    && files(tab, &[], &[])
            },
        )
        .await?;
        cedar.check_refs(&merged, &merged, &merged)?;
        git_is(
            &cedar.origin,
            &["show", "-s", "--format=%P", "main"],
            &parents,
        )?;
        self.pass();

        self.start("OBS15");
        const SIGNPOST: &str = "East: footbridge\n";
        let signpost = cedar.peer_update("signpost.txt", SIGNPOST, "Add east signpost")?;
        cedar.check_refs(&merged, &merged, &signpost)?;
        let index = git(&cedar.work, &["ls-files", "--stage", "-z"])?;
        let local_log = git(&cedar.work, &["reflog", "--format=%H", "main"])?;
        self.select_commit(&map).await?;
        self.sync_options().await?;
        self.action(Action::Sync(SyncAction::FetchAll))?;
        self.until(
            "remote graph label advances; local merge and inspected map stay in place",
            |tab| {
                !tab.ui.sync.options_open()
                    && tab.ui.notice_text() == "Fetched Successfully — 1 branch updated."
                    && graph_labels(tab, &merged, &signpost)
                    && selected_tip(tab, &map)
                    && files(tab, &[], &[])
            },
        )
        .await?;
        cedar.check_refs(&merged, &signpost, &signpost)?;
        git_is(&cedar.work, &["ls-files", "--stage", "-z"], &index)?;
        git_is(&cedar.work, &["reflog", "--format=%H", "main"], &local_log)?;
        git_is(&cedar.work, &["status", "--porcelain=v1"], "")?;
        if cedar.work.join("signpost.txt").exists() {
            return Err("Fetch materialized the incoming file in the worktree".into());
        }
        self.pass();

        self.start("OBS16");
        self.until("fast-forward-if-possible strategy selected", |tab| {
            tab.ui.sync.strategy() == Strategy::FastForwardIfPossible
        })
        .await?;
        self.action(Action::Sync(SyncAction::Pull))?;
        self.until(
            "pull selects the signpost commit and shows its added file",
            |tab| {
                tab.ui.notice_text() == "Pulled Successfully — From origin/main: fast-forwarded main by 1 commit."
                    && graph_labels(tab, &signpost, &signpost)
                    && selected_tip(tab, &signpost)
                    && tab.ui.inspection.subject.as_deref() == Some("Add east signpost")
                    && committed_file(tab, "signpost.txt", "Added")
                    && files(tab, &[], &[])
            },
        )
        .await?;
        cedar.check_refs(&signpost, &signpost, &signpost)?;
        git_is(
            &cedar.work,
            &["show", "-s", "--format=%P", "HEAD"],
            &format!("{merged}\n"),
        )?;
        git_is(&cedar.work, &["show", ":signpost.txt"], SIGNPOST)?;
        if fs::read_to_string(cedar.work.join("signpost.txt"))? != SIGNPOST {
            return Err("Pull did not install the incoming signpost in the worktree".into());
        }
        self.pass();

        self.start("EXT14");
        let ff = UpstreamFixture::new(
            dir,
            "upstream-ff",
            "Notebook",
            "notes.txt",
            "Draft\n",
            "Start notebook",
        )?;
        let review = ff.peer_update("notes.txt", "Reviewed\n", "Review notebook")?;
        ff.check_refs(&ff.base, &ff.base, &review)?;
        self.open_upstream(&ff, &ff.base, &ff.base).await?;
        self.sync_options().await?;
        self.action(Action::Sync(SyncAction::PullWith(
            Strategy::FastForwardOnly,
        )))?;
        self.until(
            "fast-forward-only pull puts both labels on the peer commit",
            |tab| {
                !tab.ui.sync.options_open()
                    && tab.ui.sync.strategy() == Strategy::FastForwardIfPossible
                    && tab.ui.notice_text() == "Pulled Successfully — From origin/main: fast-forwarded main by 1 commit."
                    && graph_labels(tab, &review, &review)
                    && selected_tip(tab, &review)
                    && tab.ui.history.iter().any(|r| {
                        r.oid == review
                            && r.subject == "Review notebook"
                            && r.parents.as_slice() == std::slice::from_ref(&ff.base)
                    })
                    && files(tab, &[], &[])
            },
        )
        .await?;
        ff.check_refs(&review, &review, &review)?;
        git_is(
            &ff.work,
            &["show", "-s", "--format=%P", "HEAD"],
            &format!("{}\n", ff.base),
        )?;
        git_is(&ff.work, &["status", "--porcelain=v1"], "")?;
        self.pass();

        self.start("EXT15");
        let split = UpstreamFixture::new(
            dir,
            "upstream-split",
            "Notebook",
            "notes.txt",
            "Draft\n",
            "Start notebook",
        )?;
        record(
            &split.work,
            "notes.txt",
            "Local revision\n",
            "Local revision",
        )?;
        let local = oid(&split.work, "HEAD")?;
        let remote = split.peer_update("notes.txt", "Remote revision\n", "Remote revision")?;
        git(&split.work, &["fetch", "-q", "origin"])?;
        split.check_refs(&local, &remote, &remote)?;
        git_is(
            &split.work,
            &["rev-list", "--left-right", "--count", "main...origin/main"],
            "1\t1\n",
        )?;
        let index = git(&split.work, &["ls-files", "--stage", "-z"])?;
        let branch_log = git(&split.work, &["reflog", "--format=%H", "main"])?;
        let tracking_log = git(&split.work, &["reflog", "--format=%H", "origin/main"])?;
        self.open_upstream(&split, &local, &remote).await?;
        self.sync_options().await?;
        self.action(Action::Sync(SyncAction::PullWith(
            Strategy::FastForwardOnly,
        )))?;
        self.until(
            "failed pull leaves the diverged labels and local selection intact",
            |tab| {
                tab.ui.notice_text().starts_with("Pull Failed")
                    && graph_labels(tab, &local, &remote)
                    && selected_tip(tab, &local)
                    && !tab.ui.operation.active()
                    && files(tab, &[], &[])
            },
        )
        .await?;
        split.check_refs(&local, &remote, &remote)?;
        git_is(&split.work, &["reflog", "--format=%H", "main"], &branch_log)?;
        git_is(
            &split.work,
            &["reflog", "--format=%H", "origin/main"],
            &tracking_log,
        )?;
        git_is(&split.work, &["ls-files", "--stage", "-z"], &index)?;
        git_is(&split.work, &["status", "--porcelain=v1"], "")?;
        if fs::read_to_string(split.work.join("notes.txt"))? != "Local revision\n"
            || split.work.join(".git/MERGE_HEAD").exists()
        {
            return Err("A refused fast-forward changed the local file or started a merge".into());
        }
        self.pass();
        Ok(())
    }
}
