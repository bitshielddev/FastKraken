use super::hosting_mock::{Hosting, Pull, TOKEN, run};
use super::*;
use crate::{
    commands::{self, Target},
    github::{PullState, Tokens},
    notice, pulls, sync,
};
use std::ffi::OsStr;

const PULL_TRAIL: &str = "Pull-trail";
const PUSH_TRAIL: &str = "Push-trail";
const CEDAR_PULL: &str = "https://github.com/cedar/pull-trail";
const TEAM_PULL: &str = "https://github.com/trailteam/pull-trail";
const CEDAR_PUSH: &str = "https://github.com/cedar/push-trail";
const CEDAR_BACKUP: &str = "https://github.com/cedar/push-trail-backup";
const BADGE_TRAIL: &str = "Badge-trail";
const CEDAR_BADGE: &str = "https://github.com/cedar/badge-trail";
const HIKER_BADGE: &str = "https://github.com/hiker/badge-trail";
const BADGE_REPO: &str = "cedar/badge-trail";
const INSTALL: &str = "https://github.com/apps/fastkraken-replay/installations/new";
const PULLS_DENIED: &str = "FastKraken needs permission to read pull requests on cedar";
const CHECKS_DENIED: &str = "FastKraken needs permission to read checks on cedar";

struct Badges {
    root: PathBuf,
    origin: String,
}

fn shown(tab: &Tab, target: &Target) -> Option<(String, PullState, String)> {
    let (remote, pull) = pulls::of_target(&tab.ui, target)?;
    Some((pulls::label(pull), pull.state, pulls::tip(&tab.ui, remote, pull)))
}

fn branch(name: &str) -> Target {
    Target::Branch(name.into())
}

fn snapshot(root: &Path) -> Result<String> {
    Ok(git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])? + &git(root, &["status", "--porcelain"])?)
}

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
        self.push_and_open_a_pull_request(dir).await?;
        let hosting = Hosting::start()?;
        let badges = self.a_branch_with_an_open_pull_request_shows_its_number(dir, &hosting).await?;
        self.missing_permission_is_explained(&hosting, &badges).await?;
        self.check_out_a_pull_request_from_a_fork(dir, &hosting, &badges).await
    }

    fn menu_row(&mut self, target: &Target, id: &str) -> Result<Option<(String, Option<String>, Option<Action>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .find(|entry| entry.id == id)
                .map(|entry| (entry.title, entry.disabled, entry.action))
        })?)
    }

    async fn take_row(&mut self, target: &Target, id: &str, title: &str) -> Result<()> {
        let row = self.menu_row(target, id)?;
        let Some((shown, None, Some(action))) = row else {
            return Err(format!("The menu of {target:?} has no usable {id}: {row:?}").into());
        };
        if shown != title {
            return Err(format!("{id} on {target:?} reads {shown:?}, not {title:?}").into());
        }
        self.action(action)
    }

    fn notice_titled(&mut self, title: &str) -> Result<Option<(u64, String, Vec<String>, Vec<Action>)>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.items().iter().find(|n| n.title == title).map(|n| {
                (
                    n.id,
                    n.detail.clone(),
                    n.actions.iter().map(|(label, _)| label.to_string()).collect(),
                    n.actions.iter().map(|(_, action)| action.clone()).collect(),
                )
            })
        })?)
    }

    async fn notice_appears(&mut self, title: &str) -> Result<(u64, String, Vec<String>, Vec<Action>)> {
        self.during(&format!("the notice {title:?}"), |tab| tab.ui.notices.items().iter().any(|n| n.title == title)).await?;
        self.notice_titled(title)?.ok_or_else(|| format!("The notice {title:?} went away").into())
    }

    async fn refreshed(&mut self, hosting: &Hosting) -> Result<usize> {
        let before = hosting.asked().len();
        self.action(Action::Pulls(pulls::Action::Refresh))?;
        self.until("the pull requests read again", |tab| tab.ui.pulls.loaded() && hosting.asked().len() > before).await?;
        Ok(before)
    }

    async fn a_branch_with_an_open_pull_request_shows_its_number(&mut self, dir: &Path, hosting: &Hosting) -> Result<Badges> {
        self.start("GAP1203");
        crate::github::serve_replay_from(&hosting.base())?;
        let client = crate::auth::configure("replay-client", "fastkraken-replay")?;
        crate::auth::save(&client, 11, &Tokens { access_token: TOKEN.into(), refresh_token: None, expires_at: None })?;
        let root = init(dir, BADGE_TRAIL)?;
        record(&root, "trail.txt", TRAIL, "Start the badge trail")?;
        git(&root, &["branch", "-m", "main", "trunk"])?;
        let origin = hosted(dir, BADGE_TRAIL, "origin", CEDAR_BADGE)?;
        git(&root, &["push", "-q", "-u", "origin", "trunk"])?;
        git(&root, &["remote", "set-head", "origin", "trunk"])?;
        for (name, file, subject) in [
            ("lookout", "lookout.txt", "Climb to the lookout"),
            ("switchback", "switchback.txt", "Cut the switchback"),
            ("ridge", "ridge.txt", "Walk the ridge"),
        ] {
            git(&root, &["switch", "-qc", name, "trunk"])?;
            record(&root, file, &format!("{name}\n"), subject)?;
            git(&root, &["push", "-q", "-u", "origin", name])?;
        }
        let (ridge, lookout, switchback) = (oid(&root, "ridge")?, oid(&root, "lookout")?, oid(&root, "switchback")?);
        let rewrite = format!("url.{origin}.insteadOf");
        git(&root, &["config", "--local", "--unset", &rewrite])?;
        crate::auth::bind(&crate::repository::Repository::open(&root)?, "origin", &format!("{CEDAR_BADGE}.git"), &client, 11)?;
        git(&root, &["config", "--local", &rewrite, &format!("{CEDAR_BADGE}.git")])?;
        hosting.change(|state| {
            state.open.insert(
                BADGE_REPO.into(),
                vec![
                    Pull { number: 14, title: "Mark the lookout", author: "fern", head: "lookout", sha: &lookout, repo: BADGE_REPO, state: "draft" }.json(),
                    Pull { number: 12, title: "Walk the ridge to the lookout", author: "cedar", head: "ridge", sha: &ridge, repo: BADGE_REPO, state: "open" }.json(),
                ],
            );
            state.closed.insert(
                BADGE_REPO.into(),
                vec![
                    Pull { number: 9, title: "Cut the switchback", author: "cedar", head: "switchback", sha: &switchback, repo: BADGE_REPO, state: "merged" }.json(),
                    Pull { number: 7, title: "An old spur", author: "fern", head: "old-spur", sha: &switchback, repo: BADGE_REPO, state: "closed" }.json(),
                ],
            );
            state.checks.insert(ridge.clone(), vec![run("build", "completed", Some("success")), run("lint", "completed", Some("failure"))]);
        });
        let before = snapshot(&root)?;
        let origin_refs = git(&root, &["ls-remote", "--", &origin])?;
        self.open(&root)?;
        self.until("Badge-trail open with its pull requests and the checks of ridge", |tab| {
            tab.ui.repo == BADGE_TRAIL
                && tab.ui.branch == "ridge"
                && tab.ui.pulls.loaded()
                && pulls::current(&tab.ui).is_some_and(|(_, pull)| pull.number == 12)
                && tab.ui.pulls.checks.as_ref().is_some_and(|checks| checks.runs.len() == 2)
        })
        .await?;
        let (ridge_badge, lookout_badge, remote_badge, switchback_badge, trunk_badge, rows) = self.tab.read_with(self.cx, |tab, _| {
            let rows: Vec<String> = tab
                .ui
                .refs
                .rows
                .iter()
                .filter_map(|row| match row {
                    crate::refs::SideRow::Header { section: crate::refs::Section::PullRequests, count, .. } => Some(format!("Pull requests {count}")),
                    crate::refs::SideRow::Pull(index) => pulls::listed(&tab.ui).get(*index).map(|(_, pull)| format!("#{} {} {}", pull.number, pull.title, pull.author)),
                    _ => None,
                })
                .collect();
            (
                shown(tab, &branch("ridge")),
                shown(tab, &branch("lookout")),
                shown(tab, &Target::RemoteBranch("origin/ridge".into())),
                shown(tab, &branch("switchback")),
                shown(tab, &branch("trunk")),
                rows,
            )
        })?;
        let Some((label, PullState::Open, tip)) = ridge_badge else {
            return Err(format!("ridge shows {ridge_badge:?}").into());
        };
        let wanted = "#12: Walk the ridge to the lookout\ncedar wants to merge ridge into trunk\nChecks: 1 of 2 checks failed: lint";
        if label != "#12" || tip != wanted {
            return Err(format!("ridge's badge reads {label:?} with the tooltip {tip:?}").into());
        }
        let failed = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.pulls.checks.as_ref().and_then(|checks| pulls::summary(&checks.runs)).map(|(overall, _)| overall)
        })?;
        if failed != Some(pulls::Overall::Failed) {
            return Err(format!("The checks dot of ridge is {failed:?}").into());
        }
        if !matches!(&lookout_badge, Some((label, PullState::Draft, tip)) if label == "#14 Draft" && tip == "#14: Mark the lookout\nDraft by fern: lookout into trunk") {
            return Err(format!("lookout shows {lookout_badge:?}").into());
        }
        if !matches!(&remote_badge, Some((label, PullState::Open, _)) if label == "#12") {
            return Err(format!("origin/ridge shows {remote_badge:?}").into());
        }
        if !matches!(&switchback_badge, Some((label, PullState::Merged, _)) if label == "#9 Merged") {
            return Err(format!("switchback shows {switchback_badge:?}").into());
        }
        if trunk_badge.is_some() {
            return Err(format!("trunk shows {trunk_badge:?}").into());
        }
        if rows != ["Pull requests 2", "#14 Mark the lookout fern", "#12 Walk the ridge to the lookout cedar"] {
            return Err(format!("The Pull requests section lists {rows:?}").into());
        }
        let first = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &branch("switchback"), cx).into_iter().next().map(|entry| (entry.id, entry.title))
        })?;
        if first != Some(("branch.delete", "Delete switchback — merged in #9…".to_owned())) {
            return Err(format!("The menu of switchback starts with {first:?}").into());
        }
        if let Some(row) = self.menu_row(&branch("ridge"), "branch.pull-request")? {
            return Err(format!("ridge, which has a pull request, still offers a new one: {row:?}").into());
        }
        let opened = crate::handoff::opened().len();
        self.take_row(&branch("ridge"), "branch.view-pull-request", "View pull request #12 on GitHub").await?;
        self.opened_after(opened, &format!("{CEDAR_BADGE}/pull/12")).await?;
        let lookout_pull = Target::PullRequest { remote: "origin".into(), number: 14 };
        self.take_row(&lookout_pull, "pull.reveal", "Show the head of #14").await?;
        self.until("the head of #14 selected", |tab| tab.ui.selection == Some(Selection::Commit(lookout.clone()))).await?;
        let opened = crate::handoff::opened().len();
        self.take_row(&lookout_pull, "pull.open", "Open #14 on GitHub").await?;
        self.opened_after(opened, &format!("{CEDAR_BADGE}/pull/14")).await?;

        let asked = self.refreshed(hosting).await?;
        let again = hosting.asked()[asked..].to_vec();
        if !again.iter().any(|ask| ask.path.contains("state=open") && ask.since.is_some() && ask.status == 304) {
            return Err(format!("A second read did not ask with the ETag it was given: {again:?}").into());
        }
        hosting.change(|state| {
            if let Some(listed) = state.open.get_mut(BADGE_REPO) {
                listed[1]["title"] = "Walk the whole ridge".into();
            }
        });
        self.refreshed(hosting).await?;
        self.until("the new title of #12", |tab| {
            pulls::of_local(&tab.ui, OsStr::new("ridge")).is_some_and(|(_, pull)| pull.title == "Walk the whole ridge")
        })
        .await?;
        git_is(&root, &["ls-remote", "--", &origin], &origin_refs)?;
        if snapshot(&root)? != before {
            return Err("Showing pull requests changed the repository's refs or files".into());
        }
        self.pass();
        Ok(Badges { root, origin })
    }

    async fn missing_permission_is_explained(&mut self, hosting: &Hosting, badges: &Badges) -> Result<()> {
        self.start("GAP1204");
        let before = snapshot(&badges.root)?;
        hosting.change(|state| {
            state.denied_pulls.insert(BADGE_REPO.into());
        });
        self.refreshed(hosting).await?;
        let (id, detail, offers, actions) = self.notice_appears(PULLS_DENIED).await?;
        if !detail.contains("Pull requests: read") || offers != ["Review permissions…"] {
            return Err(format!("The permission notice reads {detail:?} and offers {offers:?}").into());
        }
        if !matches!(actions.first(), Some(Action::Handoff(crate::handoff::Action::OpenUrl(url))) if url == INSTALL) {
            return Err(format!("Review permissions… does {actions:?}").into());
        }
        let (gone, notes) = self.tab.read_with(self.cx, |tab, _| {
            let notes: Vec<(String, Option<&str>)> = pulls::notes(&tab.ui).into_iter().map(|(text, offer)| (text, offer.map(|(label, _)| label))).collect();
            (shown(tab, &branch("ridge")).is_none() && pulls::listed(&tab.ui).is_empty(), notes)
        })?;
        if !gone || notes != [(PULLS_DENIED.to_owned(), Some("Review permissions…"))] {
            return Err(format!("With the permission missing the section shows {notes:?} (badges gone: {gone})").into());
        }
        let opened = crate::handoff::opened().len();
        self.action(Action::Notice(notice::Action::Take(id, 0)))?;
        self.opened_after(opened, INSTALL).await?;
        self.action(Action::Notice(notice::Action::Dismiss(id)))?;
        self.refreshed(hosting).await?;
        if let Some(again) = self.notice_titled(PULLS_DENIED)? {
            return Err(format!("The same missing permission was announced twice: {again:?}").into());
        }

        hosting.change(|state| {
            state.denied_pulls.clear();
            state.denied_checks.insert(BADGE_REPO.into());
        });
        self.refreshed(hosting).await?;
        let (_, detail, offers, _) = self.notice_appears(CHECKS_DENIED).await?;
        if !detail.contains("Checks: read") || offers != ["Review permissions…"] {
            return Err(format!("The checks notice reads {detail:?} and offers {offers:?}").into());
        }
        self.until("ridge's badge back with the checks explained", |tab| {
            shown(tab, &branch("ridge")).is_some_and(|(label, _, tip)| label == "#12" && tip.ends_with(CHECKS_DENIED))
        })
        .await?;
        hosting.change(|state| state.denied_checks.clear());
        self.refreshed(hosting).await?;
        self.until("the checks read again and nothing left to explain", |tab| {
            pulls::notes(&tab.ui).is_empty() && tab.ui.pulls.checks.as_ref().is_some_and(|checks| checks.trouble.is_none() && checks.runs.len() == 2)
        })
        .await?;
        if snapshot(&badges.root)? != before {
            return Err("Reading permissions changed the repository".into());
        }
        self.pass();
        Ok(())
    }

    async fn check_out_a_pull_request_from_a_fork(&mut self, dir: &Path, hosting: &Hosting, badges: &Badges) -> Result<()> {
        self.start("GAP1205");
        let root = &badges.root;
        let fork = dir.join(format!("{BADGE_TRAIL}-hiker.git"));
        fs::create_dir(&fork)?;
        git(&fork, &["init", "-q", "--bare", "-b", "trunk"])?;
        let fork = fork.to_str().ok_or("The fork's path is not UTF-8")?.to_owned();
        git(root, &["config", "--local", &format!("url.{fork}.insteadOf"), &format!("{HIKER_BADGE}.git")])?;
        let hiker = dir.join("hiker-badge");
        git(dir, &["clone", "-q", "--", &badges.origin, "hiker-badge"])?;
        for (key, value) in [("user.name", "Hiker"), ("user.email", "hiker@example.invalid"), ("core.hooksPath", "/dev/null"), ("commit.gpgSign", "false")] {
            git(&hiker, &["config", "--local", key, value])?;
        }
        git(&hiker, &["switch", "-qc", "cairn", "origin/trunk"])?;
        record(&hiker, "cairn.txt", "Cairn\n", "Build a cairn")?;
        let cairn = oid(&hiker, "cairn")?;
        git(&hiker, &["push", "-q", &fork, "cairn"])?;
        git(&hiker, &["push", "-q", &badges.origin, "cairn:refs/pull/15/head"])?;
        git(&hiker, &["switch", "-qc", "overlook", "origin/trunk"])?;
        record(&hiker, "overlook.txt", "Overlook\n", "Find the overlook")?;
        let overlook = oid(&hiker, "overlook")?;
        git(&hiker, &["push", "-q", &badges.origin, "overlook"])?;
        hosting.change(|state| {
            if let Some(listed) = state.open.get_mut(BADGE_REPO) {
                listed.insert(0, Pull { number: 16, title: "Find the overlook", author: "fern", head: "overlook", sha: &overlook, repo: BADGE_REPO, state: "open" }.json());
                listed.insert(0, Pull { number: 15, title: "Build a cairn", author: "hiker", head: "cairn", sha: &cairn, repo: "hiker/badge-trail", state: "open" }.json());
            }
        });
        self.refreshed(hosting).await?;
        self.until("#15 and #16 listed", |tab| {
            let numbers: Vec<u64> = pulls::listed(&tab.ui).iter().map(|(_, pull)| pull.number).collect();
            numbers == [16, 15, 14, 12]
        })
        .await?;
        if git(root, &["cat-file", "-t", &cairn]).is_ok() {
            return Err("The fork's commit is already in Badge-trail".into());
        }
        let cairn_pull = Target::PullRequest { remote: "origin".into(), number: 15 };
        self.take_row(&cairn_pull, "pull.reveal", "Show the head of #15").await?;
        let (_, _, offers, _) = self.notice_appears("The head of #15 has not been fetched").await?;
        if offers != ["Check out as pr/15"] {
            return Err(format!("The unfetched head offers {offers:?}").into());
        }
        let origin_refs = git(root, &["ls-remote", "--", &badges.origin])?;
        self.take_row(&cairn_pull, "pull.checkout", "Check out #15 as pr/15").await?;
        let (detail, offers) = self.noticed("Checked out pr/15").await?;
        if !detail.starts_with("pr/15 is a copy of cairn from hiker's fork") || !detail.ends_with("It cannot be pushed back to the fork from here.") {
            return Err(format!("The checkout says {detail:?}").into());
        }
        let Some(index) = offers.iter().position(|offer| offer == pulls::FORK_OFFER) else {
            return Err(format!("The checkout offers {offers:?}").into());
        };
        git_is(root, &["rev-parse", "pr/15"], &format!("{cairn}\n"))?;
        git_is(root, &["symbolic-ref", "HEAD"], "refs/heads/pr/15\n")?;
        git_is(root, &["status", "--porcelain"], "")?;
        git_is(root, &["show", "HEAD:cairn.txt"], "Cairn\n")?;
        if git(root, &["config", "--get", "branch.pr/15.remote"]).is_ok() {
            return Err("pr/15 was given an upstream it cannot push to".into());
        }
        git_is(root, &["ls-remote", "--", &badges.origin], &origin_refs)?;
        self.until("pr/15 checked out with the badge of #15", |tab| {
            tab.ui.branch == "pr/15" && shown(tab, &branch("pr/15")).is_some_and(|(label, _, _)| label == "#15")
        })
        .await?;
        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, index)))?;
        self.until("hiker added and fetched with the badge on hiker/cairn", |tab| {
            tab.ui.refs.oid_of(&Target::RemoteBranch("hiker/cairn".into())) == Some(cairn.as_str())
                && shown(tab, &Target::RemoteBranch("hiker/cairn".into())).is_some_and(|(label, _, _)| label == "#15")
        })
        .await?;
        git_is(root, &["config", "--get", "remote.hiker.url"], &format!("{HIKER_BADGE}.git\n"))?;
        git_is(root, &["rev-parse", "refs/remotes/hiker/cairn"], &format!("{cairn}\n"))?;

        let overlook_pull = Target::PullRequest { remote: "origin".into(), number: 16 };
        self.take_row(&overlook_pull, "pull.checkout", "Check out origin/overlook").await?;
        let (_, _, offers, actions) = self.notice_appears("origin/overlook has not been fetched").await?;
        if offers != ["Fetch origin"] {
            return Err(format!("The unfetched branch offers {offers:?}").into());
        }
        self.action(actions[0].clone())?;
        self.until("origin/overlook fetched", |tab| {
            tab.ui.refs.oid_of(&Target::RemoteBranch("origin/overlook".into())) == Some(overlook.as_str())
        })
        .await?;
        self.take_row(&overlook_pull, "pull.checkout", "Check out origin/overlook").await?;
        self.until("overlook checked out and tracking origin/overlook", |tab| {
            tab.ui.branch == "overlook" && tab.ui.upstream.as_deref() == Some("origin/overlook")
        })
        .await?;
        git_is(root, &["rev-parse", "overlook"], &format!("{overlook}\n"))?;
        git_is(root, &["config", "branch.overlook.merge"], "refs/heads/overlook\n")?;
        git_is(root, &["status", "--porcelain"], "")?;
        self.pass();
        Ok(())
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
