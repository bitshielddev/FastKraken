use super::*;
use crate::{prompt, reverse::{self, Operation}};

pub(super) fn reversal_open(tab: &Tab) -> bool {
    matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Reverse(_)))
}

impl Replay<'_> {
    async fn reversal_fixture(
        &mut self,
        dir: &Path,
        name: &str,
    ) -> Result<(PathBuf, String, String)> {
        let repo = init(dir, name)?;
        record(&repo, "notes.txt", "First page\n", "Start notebook")?;
        record(&repo, "checklist.txt", "Review notes\n", "Add checklist")?;
        let tip = oid(&repo, "HEAD")?;
        let base = oid(&repo, "HEAD^")?;
        self.open(&repo)?;
        self.until("clean notebook open on main", |t| {
            t.ui.repo == name
                && t.ui.branch == "main"
                && t.ui.head.as_ref() == Some(&tip)
                && t.ui.history.iter().any(|r| r.oid == base)
                && files(t, &[], &[])
        })
        .await?;
        Ok((repo, base, tip))
    }

    pub(super) async fn reversal_prompt(&mut self, action: reverse::Action) -> Result<()> {
        self.action(Action::Reverse(action))?;
        self.until("history confirmation is open", reversal_open).await
    }

    pub(super) async fn reversals(&mut self, dir: &Path) -> Result<()> {
        self.start("EXT10");
        let (repo, base, tip) = self.reversal_fixture(dir, "Notebook-reset").await?;
        self.select_commit(&base).await?;
        self.reversal_prompt(reverse::Action::Start(Operation::SoftReset))
            .await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until(
            "main moves to Start notebook with the checklist staged",
            |t| {
                !reversal_open(t)
                    && t.ui.selection == Some(Selection::Commit(base.clone()))
                    && t.ui
                        .history
                        .iter()
                        .any(|r| r.oid == base && r.refs.contains("HEAD -> main"))
                    && files(t, &["checklist.txt"], &[])
                    && t.ui
                        .files
                        .iter()
                        .any(|row| row.path == Path::new("checklist.txt") && row.status == "Added")
            },
        )
        .await?;
        git_is(&repo, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        git_is(&repo, &["rev-parse", "main"], &format!("{base}\n"))?;
        git_is(&repo, &["rev-parse", "ORIG_HEAD"], &format!("{tip}\n"))?;
        git_is(&repo, &["status", "--porcelain=v1"], "A  checklist.txt\n")?;
        git_is(&repo, &["show", ":checklist.txt"], "Review notes\n")?;
        self.pass();

        self.start("EXT11");
        let (repo, _, tip) = self.reversal_fixture(dir, "Notebook-revert").await?;
        self.select_commit(&tip).await?;
        self.reversal_prompt(reverse::Action::Start(Operation::Revert)).await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("inverse commit selected with the checklist deleted", |t| {
            !reversal_open(t)
                && t.ui.history.iter().any(|row| {
                    row.parents.as_slice() == std::slice::from_ref(&tip)
                        && row.refs.contains("HEAD -> main")
                        && t.ui.selection == Some(Selection::Commit(row.oid.clone()))
                })
                && committed_file(t, "checklist.txt", "Deleted")
                && files(t, &[], &[])
        })
        .await?;
        git_is(
            &repo,
            &["show", "-s", "--format=%P", "main"],
            &format!("{tip}\n"),
        )?;
        git_is(
            &repo,
            &["diff", "--name-status", "HEAD^", "HEAD"],
            "D\tchecklist.txt\n",
        )?;
        git_is(&repo, &["status", "--porcelain=v1"], "")?;
        self.pass();

        self.start("EXT12");
        let (repo, _, tip) = self.reversal_fixture(dir, "Notebook-cancel").await?;
        self.select_commit(&tip).await?;
        let before = git(&repo, &["reflog", "--format=%H", "main"])?;
        let index = git(&repo, &["ls-files", "--stage"])?;
        self.reversal_prompt(reverse::Action::Start(Operation::Revert)).await?;
        self.key("escape")?;
        self.until(
            "revert dismissed with the original clean tip selected",
            |t| {
                !reversal_open(t)
                    && files(t, &[], &[])
                    && t.ui.branch == "main"
                    && t.ui.selection == Some(Selection::Commit(tip.clone()))
            },
        )
        .await?;
        git_is(&repo, &["rev-parse", "main"], &format!("{tip}\n"))?;
        git_is(&repo, &["reflog", "--format=%H", "main"], &before)?;
        git_is(&repo, &["ls-files", "--stage"], &index)?;
        git_is(&repo, &["status", "--porcelain=v1"], "")?;
        if fs::read(repo.join("checklist.txt"))? != b"Review notes\n" {
            return Err("Cancelling revert changed the working file".into());
        }
        self.pass();
        Ok(())
    }
}
