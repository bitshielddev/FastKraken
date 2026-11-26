use super::*;
use crate::{
    commands::{self, Target},
    notice, sync,
};

const PULL_TRAIL: &str = "Pull-trail";
const PUSH_TRAIL: &str = "Push-trail";
const CEDAR_PULL: &str = "https://github.com/cedar/pull-trail";
const TEAM_PULL: &str = "https://github.com/trailteam/pull-trail";
const CEDAR_PUSH: &str = "https://github.com/cedar/push-trail";
const CEDAR_BACKUP: &str = "https://github.com/cedar/push-trail-backup";

fn hosted(dir: &Path, name: &str, remote: &str, page: &str) -> Result<String> {
    let bare = dir.join(format!("{name}-{remote}.git"));
    fs::create_dir(&bare)?;
    git(&bare, &["init", "-q", "--bare", "-b", "trunk"])?;
    let bare = bare.to_str().ok_or("The bare repository's path is not UTF-8")?.to_owned();
    let root = dir.join(name);
    let address = format!("{page}.git");
    git(&root, &["remote", "add", remote, &address])?;
    git(&root, &["config", "--local", &format!("url.{bare}.insteadOf"), &address])?;
    Ok(bare)
}

fn tip_at(root: &Path, bare: &str, name: &str) -> Result<Option<String>> {
    let listed = git(root, &["ls-remote", "--", bare, name])?;
    Ok(listed.lines().find_map(|line| line.split_once('\t').filter(|(_, at)| *at == name).map(|(oid, _)| oid.to_owned())))
}

impl Replay<'_> {
    pub(super) async fn gap12(&mut self, dir: &Path) -> Result<()> {
        self.open_a_pull_request_for_a_pushed_branch(dir).await?;
        self.push_and_open_a_pull_request(dir).await
    }

    fn pull_request_row(&mut self, target: &Target) -> Result<Option<(String, Option<String>, Option<Action>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .find(|entry| entry.id == "branch.pull-request")
                .map(|entry| (entry.title, entry.disabled, entry.action))
        })?)
    }

    async fn take_pull_request(&mut self, target: &Target, title: &str) -> Result<usize> {
        let row = self.pull_request_row(target)?;
        let Some((shown, None, Some(action))) = row else {
            return Err(format!("The menu of {target:?} offers no usable pull request command: {row:?}").into());
        };
        if shown != title {
            return Err(format!("The pull request command of {target:?} reads {shown:?}, not {title:?}").into());
        }
        let before = crate::handoff::opened().len();
        self.action(action)?;
        Ok(before)
    }

    async fn opened_after(&mut self, before: usize, wanted: &str) -> Result<()> {
        self.during(&format!("the page {wanted} opened"), |_| crate::handoff::opened().len() > before).await?;
        let opened = crate::handoff::opened();
        if opened.len() != before + 1 || opened.last().map(String::as_str) != Some(wanted) {
            return Err(format!("Expected only {wanted} to open, the app opened {:?}", &opened[before..]).into());
        }
        Ok(())
    }

    async fn open_a_pull_request_for_a_pushed_branch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1201");
        let root = init(dir, PULL_TRAIL)?;
        record(&root, "trail.txt", TRAIL, "Start the pull trail")?;
        git(&root, &["branch", "-m", "main", "trunk"])?;
        let origin = hosted(dir, PULL_TRAIL, "origin", CEDAR_PULL)?;
        git(&root, &["push", "-q", "-u", "origin", "trunk"])?;
        git(&root, &["remote", "set-head", "origin", "trunk"])?;
        git(&root, &["switch", "-qc", "lookout"])?;
        record(&root, "lookout.txt", "Lookout\n", "Climb to the lookout")?;
        git(&root, &["push", "-q", "-u", "origin", "lookout"])?;
        git(&root, &["switch", "-qc", "ridge", "trunk"])?;
        record(&root, "ridge.txt", "Ridge\n", "Walk the ridge")?;
        git(&root, &["push", "-q", "-u", "origin", "ridge"])?;
        let ridge = oid(&root, "ridge")?;
        let remote_refs = git(&root, &["ls-remote", "--", &origin])?;
        self.open(&root)?;
        self.until("Pull-trail open on ridge with origin's default branch known", |tab| {
            tab.ui.repo == PULL_TRAIL
                && tab.ui.branch == "ridge"
                && tab.ui.refs.oid_of(&Target::RemoteBranch("origin/ridge".into())) == Some(ridge.as_str())
                && tab.ui.refs.oid_of(&Target::RemoteBranch("origin/lookout".into())).is_some()
                && tab.ui.refs.defaults.get("origin").map(String::as_str) == Some("trunk")
        })
        .await?;
        let trunk = Target::Branch("trunk".into());
        if let Some(row) = self.pull_request_row(&trunk)? {
            return Err(format!("trunk, origin's default branch, offers a pull request into itself: {row:?}").into());
        }

        let before = self.take_pull_request(&Target::Branch("ridge".into()), "Open pull request for ridge…").await?;
        self.opened_after(before, &format!("{CEDAR_PULL}/compare/trunk...ridge?expand=1")).await?;
        let before = self.take_pull_request(&Target::Repo, "Open pull request for ridge…").await?;
        self.opened_after(before, &format!("{CEDAR_PULL}/compare/trunk...ridge?expand=1")).await?;

        let rows = self.dropped(Target::Branch("ridge".into()), Target::RemoteBranch("origin/lookout".into()), None).await?;
        let action = Self::pick_dropped(&rows, "Open pull request from ridge to origin/lookout…")?;
        self.action(Action::Menu(None))?;
        let before = crate::handoff::opened().len();
        self.action(action)?;
        self.opened_after(before, &format!("{CEDAR_PULL}/compare/lookout...ridge?expand=1")).await?;

        let parent = hosted(dir, PULL_TRAIL, "upstream", TEAM_PULL)?;
        git(&root, &["push", "-q", &parent, "trunk"])?;
        git(&root, &["fetch", "-q", "upstream"])?;
        git(&root, &["remote", "set-head", "upstream", "trunk"])?;
        self.action(Action::Refresh)?;
        self.until("the parent repository listed as upstream", |tab| {
            tab.ui.refs.defaults.get("upstream").map(String::as_str) == Some("trunk")
                && tab.ui.refs.oid_of(&Target::RemoteBranch("upstream/trunk".into())).is_some()
        })
        .await?;
        let before = self.take_pull_request(&Target::Branch("ridge".into()), "Open pull request for ridge…").await?;
        self.opened_after(before, &format!("{TEAM_PULL}/compare/trunk...cedar:ridge?expand=1")).await?;

        git_is(&root, &["ls-remote", "--", &origin], &remote_refs)?;
        git_is(&root, &["rev-parse", "ridge"], &format!("{ridge}\n"))?;
        git_is(&root, &["status", "--porcelain"], "")?;
        self.pass();
        Ok(())
    }

    async fn push_and_open_a_pull_request(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1202");
        let root = init(dir, PUSH_TRAIL)?;
        record(&root, "trail.txt", TRAIL, "Start the push trail")?;
        git(&root, &["branch", "-m", "main", "trunk"])?;
        let origin = hosted(dir, PUSH_TRAIL, "origin", CEDAR_PUSH)?;
        git(&root, &["push", "-q", "-u", "origin", "trunk"])?;
        git(&root, &["remote", "set-head", "origin", "trunk"])?;
        git(&root, &["switch", "-qc", "spur"])?;
        record(&root, "spur.txt", "Spur\n", "Scout the spur")?;
        let spur = oid(&root, "spur")?;
        self.open(&root)?;
        self.until("Push-trail open on spur with no upstream", |tab| {
            tab.ui.repo == PUSH_TRAIL
                && tab.ui.branch == "spur"
                && tab.ui.upstream.is_none()
                && tab.ui.refs.defaults.get("origin").map(String::as_str) == Some("trunk")
        })
        .await?;
        let page = format!("{CEDAR_PUSH}/compare/trunk...spur?expand=1");
        let before = self.take_pull_request(&Target::Branch("spur".into()), "Push and open pull request…").await?;
        let (detail, _) = self.noticed("Pushed spur").await?;
        if detail != "Created origin/spur." {
            return Err(format!("The push before the pull request says {detail:?}").into());
        }
        self.opened_after(before, &page).await?;
        if tip_at(&root, &origin, "refs/heads/spur")?.as_deref() != Some(spur.as_str()) {
            return Err("origin has no spur at the pushed commit".into());
        }
        git_is(&root, &["config", "branch.spur.remote"], "origin\n")?;
        git_is(&root, &["config", "branch.spur.merge"], "refs/heads/spur\n")?;
        self.until("spur tracking origin/spur", |tab| {
            tab.ui.upstream.as_deref() == Some("origin/spur")
                && tab.ui.refs.oid_of(&Target::RemoteBranch("origin/spur".into())) == Some(spur.as_str())
        })
        .await?;
        let row = self.pull_request_row(&Target::Branch("spur".into()))?.map(|row| row.0);
        if row.as_deref() != Some("Open pull request for spur…") {
            return Err(format!("After the push the command reads {row:?}").into());
        }

        record(&root, "spur.txt", "Spur\nCairn\n", "Build a cairn on the spur")?;
        let cairn = oid(&root, "spur")?;
        self.action(Action::Refresh)?;
        self.until("spur one commit ahead of origin/spur", |tab| {
            tab.ui.refs.local(std::ffi::OsStr::new("spur")).is_some_and(|item| item.oid.as_deref() == Some(cairn.as_str()) && item.ahead == 1)
        })
        .await?;
        let row = self.pull_request_row(&Target::Branch("spur".into()))?.map(|row| row.0);
        if row.as_deref() != Some("Push and open pull request…") {
            return Err(format!("With a commit not pushed the command reads {row:?}").into());
        }
        let opened = crate::handoff::opened().len();
        self.action(Action::Sync(sync::Action::Push))?;
        let (_, offers) = self.noticed("Pushed Successfully").await?;
        let Some(index) = offers.iter().position(|offer| offer == crate::pull_request::OFFER) else {
            return Err(format!("The push notice offers {offers:?}").into());
        };
        if crate::handoff::opened().len() != opened {
            return Err("A plain push opened a page".into());
        }
        if tip_at(&root, &origin, "refs/heads/spur")?.as_deref() != Some(cairn.as_str()) {
            return Err("origin has no spur at the cairn commit".into());
        }
        self.until("origin/spur at the cairn commit", |tab| {
            tab.ui.refs.oid_of(&Target::RemoteBranch("origin/spur".into())) == Some(cairn.as_str())
        })
        .await?;
        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, index)))?;
        self.opened_after(opened, &page).await?;
        git_is(&root, &["ls-remote", "--", &origin, "refs/heads/spur"], &format!("{cairn}\trefs/heads/spur\n"))?;

        let backup = hosted(dir, PUSH_TRAIL, "backup", CEDAR_BACKUP)?;
        record(&root, "spur.txt", "Spur\nCairn\nSummit\n", "Reach the summit of the spur")?;
        let summit = oid(&root, "spur")?;
        self.action(Action::Refresh)?;
        self.until("spur one commit ahead of origin/spur with backup listed", |tab| {
            tab.ui.refs.addresses.contains_key("backup")
                && tab.ui.refs.local(std::ffi::OsStr::new("spur")).is_some_and(|item| item.oid.as_deref() == Some(summit.as_str()) && item.ahead == 1)
        })
        .await?;
        let opened = crate::handoff::opened().len();
        let elsewhere = crate::push_to::Action::Push { branch: "spur".into(), tip: summit.clone(), remote: "backup".into(), destination: "spur".into() };
        self.action(Action::PushTo(elsewhere))?;
        let (detail, offers) = self.noticed("Pushed spur").await?;
        if detail != "Created backup/spur." {
            return Err(format!("The push to backup says {detail:?}").into());
        }
        if offers.iter().any(|offer| offer == crate::pull_request::OFFER) {
            return Err(format!("A push to backup offers a pull request that would push to origin first: {offers:?}").into());
        }
        if tip_at(&root, &backup, "refs/heads/spur")?.as_deref() != Some(summit.as_str()) || crate::handoff::opened().len() != opened {
            return Err("The push to backup did not land, or opened a page".into());
        }
        git_is(&root, &["ls-remote", "--", &origin, "refs/heads/spur"], &format!("{cairn}\trefs/heads/spur\n"))?;
        git_is(&root, &["status", "--porcelain"], "")?;
        self.pass();
        Ok(())
    }
}
