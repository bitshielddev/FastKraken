use super::hosting_mock::{Hosting, Installed, Pull, TOKEN, listed, repository, run};
use super::*;
use crate::{
    commands::{self, Target},
    github::{PullState, Tokens},
    notice, pulls, sync,
};
use crate::{commit_options, preferences, prompt, reveal, signing::{self, Grade, Signature}};
use std::{ffi::OsStr, os::unix::fs::PermissionsExt, process::Stdio};

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
const SUMMIT_TRAIL: &str = "summit-trail";
const CEDAR_SUMMIT: &str = "https://github.com/cedar/summit-trail";
const FERN_SUMMIT: &str = "https://github.com/fern/summit-trail";
const TEAM_SUMMIT: &str = "https://github.com/trailteam/summit-trail";

const SIGNPOST: &str = "Signpost";
const FERRY_TOOL: &str = "Ferry-tool";
const KEYGEN: &str = "/usr/bin/ssh-keygen";
const SIGNER: &str = "cedar@example.invalid";
const LANTERN_PASSPHRASE: &str = "ridge lantern";
const UNLOCK_LANTERN: &str = "Unlock SSH key lantern";
const CAIRN_PASSPHRASE: &str = "cairn above the tree line";
const AGENT: &str = "/usr/bin/ssh-agent";

struct Agent(std::process::Child);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn agent_at(socket: &Path) -> Result<Agent> {
    let child = std::process::Command::new(AGENT)
        .arg("-D")
        .arg("-a")
        .arg(socket)
        .env_remove("SSH_AUTH_SOCK")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let agent = Agent(child);
    let started = Instant::now();
    while !socket.exists() {
        if started.elapsed() > Duration::from_secs(5) {
            return Err("The sandbox's ssh-agent did not start".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(agent)
}

fn agent_listed(socket: &Path) -> Result<String> {
    let output = std::process::Command::new("/usr/bin/ssh-add").arg("-L").env("SSH_AUTH_SOCK", socket).stdin(Stdio::null()).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

struct Signpost {
    root: PathBuf,
    keys: PathBuf,
    allowed: PathBuf,
    link: PathBuf,
    raised: String,
    painted: String,
}

fn keygen(args: &[&str], key: &Path, askpass: Option<&Path>) -> Result<String> {
    let mut command = std::process::Command::new(KEYGEN);
    command.args(args).arg(key).env_remove("SSH_AUTH_SOCK").stdin(Stdio::null());
    if let Some(askpass) = askpass {
        command.env("SSH_ASKPASS", askpass).env("SSH_ASKPASS_REQUIRE", "force");
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!("ssh-keygen {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "A sandbox path is not UTF-8".into())
}

fn signpost(dir: &Path) -> Result<Signpost> {
    let sandbox = super::sandbox().ok_or("The replay has no sandbox")?;
    let bin = sandbox.join("bin");
    fs::create_dir_all(&bin)?;
    let link = bin.join("ssh-keygen");
    std::os::unix::fs::symlink(KEYGEN, &link)?;
    let keys = dir.join("signing-keys");
    fs::create_dir(&keys)?;
    for name in ["cedar", "stranger"] {
        keygen(&["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"], &keys.join(name), None)?;
    }
    let maker = keys.join("make-passphrase.sh");
    super::gap04::script(&maker, &format!("echo '{LANTERN_PASSPHRASE}'\n"))?;
    keygen(&["-q", "-t", "ed25519", "-C", "lantern", "-f"], &keys.join("lantern"), Some(&maker))?;
    fs::remove_file(&maker)?;
    let allowed = keys.join("allowed_signers");
    let mut listed = String::new();
    for name in ["cedar", "lantern"] {
        listed.push_str(&format!("{SIGNER} {}\n", fs::read_to_string(keys.join(format!("{name}.pub")))?.trim()));
    }
    fs::write(&allowed, listed)?;
    let root = init(dir, SIGNPOST)?;
    for (key, value) in [
        ("gpg.format", "ssh"),
        ("user.signingKey", utf8(&keys.join("cedar.pub"))?),
        ("gpg.ssh.allowedSignersFile", utf8(&allowed)?),
    ] {
        git(&root, &["config", "--local", key, value])?;
    }
    fs::write(root.join("signpost.txt"), TRAIL)?;
    git(&root, &["add", "--", "signpost.txt"])?;
    git(&root, &["commit", "-S", "-qm", "Raise the signpost"])?;
    git(&root, &["tag", "-s", "-m", "Signpost v1", "v1"])?;
    let raised = oid(&root, "HEAD")?;
    record(&root, "signpost.txt", &format!("{TRAIL}Paint: yellow\n"), "Paint the signpost")?;
    let painted = oid(&root, "HEAD")?;
    Ok(Signpost { root, keys, allowed, link, raised, painted })
}

fn signed_object(root: &Path, kind: &str, revision: &str) -> Result<bool> {
    let raw = git(root, &["cat-file", kind, revision])?;
    Ok(if kind == "commit" {
        signing::header_signature(raw.as_bytes()).is_some()
    } else {
        raw.contains("-----BEGIN SSH SIGNATURE-----")
    })
}

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
        self.check_out_a_pull_request_from_a_fork(dir, &hosting, &badges).await?;
        self.clone_from_the_github_list(dir, &hosting).await?;
        let signpost = self.a_signed_commit_shows_as_verified(dir).await?;
        self.a_commit_signed_by_an_unknown_key(&signpost).await?;
        self.sign_one_commit_from_the_commit_box(&signpost).await?;
        self.generate_an_ssh_key(&signpost).await?;
        self.open_a_conflict_in_a_merge_tool(dir).await
    }

    async fn until_tab_total(&mut self, description: &str, count: usize, front: usize) -> Result<()> {
        let started = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            let seen = self.tab.tabs(self.cx)?;
            if seen == (count, front) {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description}: tabs and front are {seen:?}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    fn filter_list(&mut self, text: &str) -> Result<Vec<String>> {
        let text = text.to_owned();
        self.tab.update(self.cx, |tab, _, cx| tab.ui.cloning.filter.update(cx, |field, cx| field.set_text(&text, cx)))?;
        Ok(self.tab.read_with(self.cx, |tab, cx| tab.ui.cloning.shown(cx).iter().map(|repository| repository.full_name()).collect())?)
    }

    fn clone_request(&mut self) -> Result<crate::clone::Request> {
        Ok(self.tab.read_with(self.cx, |tab, cx| tab.ui.cloning.request(cx))?)
    }

    async fn idle(&mut self, description: &str, ready: impl Fn(&Tab) -> bool) -> Result<()> {
        self.until(description, |tab| !tab.ui.busy() && tab.ui.jobs.network.is_none() && !tab.ui.forks.looking && ready(tab)).await
    }

    async fn told(&mut self, title: &str, detail: &str) -> Result<()> {
        let (_, said, _, _) = self.notice_appears(title).await?;
        if said != detail {
            return Err(format!("The notice {title:?} says {said:?}, not {detail:?}").into());
        }
        Ok(())
    }

    async fn clone_from_the_github_list(&mut self, dir: &Path, hosting: &Hosting) -> Result<()> {
        use crate::clone::{Action as C, Listing, Source};
        self.start("GAP1206");
        let source = init(dir, "Summit-trail-source")?;
        record(&source, "trail.txt", TRAIL, "Start the summit trail")?;
        git(&source, &["branch", "-m", "main", "trunk"])?;
        let bare = |owner: &str| -> Result<String> {
            let path = dir.join(format!("summit-{owner}.git"));
            fs::create_dir(&path)?;
            git(&path, &["init", "-q", "--bare", "-b", "trunk"])?;
            Ok(path.to_str().ok_or("The bare repository's path is not UTF-8")?.to_owned())
        };
        let team = bare("trailteam")?;
        git(&source, &["push", "-q", &team, "trunk"])?;
        let team_tip = oid(&source, "trunk")?;
        record(&source, "summit.txt", "Summit\n", "Mark the summit")?;
        let summit = oid(&source, "trunk")?;
        let cedar = bare("cedar")?;
        git(&source, &["push", "-q", &cedar, "trunk"])?;
        let fern = bare("fern")?;
        git(&source, &["push", "-q", &fern, "trunk"])?;
        git(&source, &["switch", "-qc", "cairn"])?;
        record(&source, "cairn.txt", "Cairn\n", "Build a cairn")?;
        let cairn = oid(&source, "cairn")?;
        git(&source, &["push", "-q", &fern, "cairn"])?;
        let global = global_config(dir);
        let kept = fs::read_to_string(&global)?;
        let mut rules = kept.clone();
        for (page, path) in [(CEDAR_SUMMIT, &cedar), (FERN_SUMMIT, &fern), (TEAM_SUMMIT, &team)] {
            rules += &format!("[url \"{path}\"]\n\tinsteadOf = {page}.git\n[url \"{page}.git\"]\n\tpushInsteadOf = {page}.git\n");
        }
        fs::write(&global, rules)?;
        hosting.change(|state| {
            state.user = Some((11, "fern".into()));
            state.installations = vec![
                Installed {
                    id: 501,
                    owner: "cedar".into(),
                    repositories: vec![listed("cedar/summit-trail", false, "2026-11-25T08:00:00Z"), listed("cedar/badge-trail", true, "2026-11-20T10:00:00Z")],
                },
                Installed {
                    id: 502,
                    owner: "fern".into(),
                    repositories: vec![listed("fern/summit-trail", false, "2026-11-23T12:30:00Z"), listed("fern/field-notes", true, "2026-11-26T07:00:00Z")],
                },
            ];
            for (full_name, parent) in [("cedar/summit-trail", Some("trailteam/summit-trail")), ("fern/summit-trail", Some("cedar/summit-trail")), ("trailteam/summit-trail", None)] {
                state.repositories.insert(full_name.into(), repository(full_name, parent));
            }
        });
        let clones = dir.join("github-clones");
        fs::create_dir(&clones)?;
        let asked = hosting.asked().len();

        self.action(Action::Clone(C::Browse))?;
        self.until("the GitHub list in the clone dialog", |tab| {
            tab.ui.cloning.open() && tab.ui.cloning.source() == Source::GitHub && matches!(tab.ui.cloning.listing(), Listing::Ready(_))
        })
        .await?;
        let (rows, account) = self.tab.read_with(self.cx, |tab, cx| {
            let at = crate::github::epoch_of("2026-11-26T09:00:00Z").unwrap_or_default();
            let rows: Vec<(String, bool, Option<String>)> =
                tab.ui.cloning.shown(cx).iter().map(|r| (r.full_name(), r.private, crate::clone::pushed(r, at))).collect();
            let account = match tab.ui.cloning.listing() {
                Listing::Ready(account) => Some((account.account, account.login.clone(), account.install.clone())),
                _ => None,
            };
            (rows, account)
        })?;
        let wanted = [
            ("fern/field-notes", true, "Pushed 2 hours ago"),
            ("cedar/summit-trail", false, "Pushed yesterday"),
            ("fern/summit-trail", false, "Pushed 23 Nov"),
            ("cedar/badge-trail", true, "Pushed 20 Nov"),
        ];
        let rows_match = rows.len() == wanted.len()
            && rows.iter().zip(wanted).all(|((name, private, pushed), (want, secret, when))| name == want && *private == secret && pushed.as_deref() == Some(when));
        if !rows_match {
            return Err(format!("The GitHub list shows {rows:?}").into());
        }
        if account != Some((11, "fern".to_owned(), INSTALL.to_owned())) {
            return Err(format!("The list was read for {account:?}").into());
        }
        let paths: Vec<String> = hosting.asked()[asked..].iter().map(|ask| ask.path.clone()).collect();
        for path in ["/user", "/user/installations?per_page=100&page=1", "/user/installations/501/repositories?per_page=100&page=1", "/user/installations/502/repositories?per_page=100&page=1"] {
            if !paths.iter().any(|asked| asked == path) {
                return Err(format!("Listing the repositories never asked for {path}: {paths:?}").into());
            }
        }
        let opened = crate::handoff::opened().len();
        let choose = self.tab.read_with(self.cx, |tab, _| match tab.ui.cloning.listing() {
            Listing::Ready(account) => Some(crate::clone::choose(account)),
            _ => None,
        })?;
        self.action(choose.ok_or("The GitHub list has no Choose repositories… link")?)?;
        self.opened_after(opened, INSTALL).await?;
        if self.filter_list("summit")? != ["cedar/summit-trail", "fern/summit-trail"] {
            return Err("Filtering by summit does not leave the two summit trails".into());
        }
        if !self.filter_list("no-such-trail")?.is_empty() {
            return Err("A filter that matches nothing still lists repositories".into());
        }
        self.filter_list("")?;
        self.action(Action::Clone(C::Pick("fern/summit-trail".into())))?;
        let picked = self.clone_request()?;
        if picked.address != format!("{FERN_SUMMIT}.git") || picked.name != SUMMIT_TRAIL || picked.account != Some(11) {
            return Err(format!("Choosing fern/summit-trail fills {picked:?}").into());
        }
        let typed = format!("{CEDAR_SUMMIT}.git");
        self.tab.update(self.cx, |tab, _, cx| tab.ui.cloning.address.update(cx, |field, cx| field.set_text(&typed, cx)))?;
        if self.clone_request()?.account.is_some() {
            return Err("A typed address is still cloned with the account of the chosen row".into());
        }
        if self.filter_list("CEDAR/S")? != ["cedar/summit-trail"] {
            return Err("The filter does not ignore case".into());
        }
        self.action(Action::Clone(C::PickFirst))?;
        self.action(Action::Clone(C::SetParent(clones.clone())))?;
        let destination = clones.join(SUMMIT_TRAIL);
        let request = self.clone_request()?;
        if request.address != typed || request.account != Some(11) || request.destination() != destination || request.problem().is_some() {
            return Err(format!("After choosing cedar/summit-trail the dialog would clone {request:?}").into());
        }

        self.action(Action::Clone(C::Submit))?;
        self.until_tab_total("the clone in a second tab", 2, 1).await?;
        self.idle("summit-trail open with origin and its history", |tab| {
            tab.ui.repo == SUMMIT_TRAIL
                && tab.ui.branch == "trunk"
                && tab.ui.history.iter().any(|row| row.subject == "Mark the summit")
                && tab.ui.refs.remotes.iter().any(|remote| remote == "origin")
        })
        .await?;
        let parent = clones.display().to_string();
        self.told("Cloned summit-trail", &format!("Into {parent}. origin uses your GitHub connection.")).await?;
        git_is(&destination, &["config", "--get-all", "remote.origin.url"], &format!("{typed}\n"))?;
        git_is(&destination, &["rev-parse", "HEAD"], &format!("{summit}\n"))?;
        git_is(&destination, &["status", "--porcelain"], "")?;
        let repo = crate::repository::Repository::open(&destination)?;
        if crate::auth::bound(&repo, "origin")?.as_deref() != Some(typed.as_str()) {
            return Err("The clone's origin is not bound to the GitHub account".into());
        }
        let settings: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.join("state/github.json"))?)?;
        if !settings["cloning"].as_array().is_some_and(Vec::is_empty) {
            return Err(format!("The clone left its one-time access behind: {}", settings["cloning"]).into());
        }

        let origin = Target::Remote("origin".into());
        let opened = crate::handoff::opened().len();
        self.take_row(&origin, "remote.fork-on-github", "Fork on GitHub…").await?;
        self.opened_after(opened, &format!("{CEDAR_SUMMIT}/fork")).await?;
        self.take_row(&origin, "remote.add-my-fork", "Add my fork as a remote").await?;
        self.told("Added fern", &format!("fern is {FERN_SUMMIT}.git and uses your GitHub connection. Fetching it now.")).await?;
        self.idle("fern fetched", |tab| tab.ui.refs.oid_of(&Target::RemoteBranch("fern/cairn".into())) == Some(cairn.as_str())).await?;
        git_is(&destination, &["config", "--get-all", "remote.fern.url"], &format!("{FERN_SUMMIT}.git\n"))?;
        git_is(&destination, &["rev-parse", "refs/remotes/fern/cairn"], &format!("{cairn}\n"))?;
        if crate::auth::bound(&repo, "fern")?.as_deref() != Some(format!("{FERN_SUMMIT}.git").as_str()) {
            return Err("fern is not bound to the GitHub account".into());
        }
        self.take_row(&origin, "remote.add-parent", "Add the parent repository as a remote").await?;
        self.told("Added upstream", &format!("upstream is {TEAM_SUMMIT}.git. Fetching it now.")).await?;
        self.idle("upstream fetched", |tab| tab.ui.refs.oid_of(&Target::RemoteBranch("upstream/trunk".into())) == Some(team_tip.as_str())).await?;
        git_is(&destination, &["config", "--get-all", "remote.upstream.url"], &format!("{TEAM_SUMMIT}.git\n"))?;
        if crate::auth::bound(&repo, "upstream")?.is_some() {
            return Err("The parent repository was bound to the account".into());
        }
        let remotes = git(&destination, &["remote"])?;
        self.take_row(&Target::Remote("fern".into()), "remote.add-parent", "Add the parent repository as a remote").await?;
        self.told("origin already is the parent repository", "origin is cedar/summit-trail. Nothing was changed.").await?;
        self.idle("the parent looked up", |_| true).await?;
        self.take_row(&origin, "remote.add-my-fork", "Add my fork as a remote").await?;
        self.told("Your fork is already a remote", "fern is fern/summit-trail. Nothing was changed.").await?;
        self.idle("the fork looked up", |_| true).await?;
        let upstream = Target::Remote("upstream".into());
        self.take_row(&upstream, "remote.add-my-fork", "Add my fork as a remote").await?;
        self.told("fern/summit-trail is not a fork of trailteam/summit-trail", "Nothing was added.").await?;
        self.idle("the other fork looked up", |_| true).await?;
        let opened = crate::handoff::opened().len();
        self.take_row(&upstream, "remote.fork-on-github", "Fork on GitHub…").await?;
        self.opened_after(opened, &format!("{TEAM_SUMMIT}/fork")).await?;
        git_is(&destination, &["remote"], &remotes)?;
        git_is(&destination, &["rev-parse", "trunk"], &format!("{summit}\n"))?;
        git_is(&destination, &["status", "--porcelain"], "")?;

        self.key("cmd-w")?;
        self.until_tab_total("one tab again", 1, 0).await?;
        self.until("Badge-trail in front", |tab| tab.ui.repo == BADGE_TRAIL).await?;
        self.action(Action::Clone(C::Open))?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.cloning.listing() != &Listing::Idle)? {
            return Err("Opening the clone dialog again still shows the earlier GitHub list".into());
        }
        let asked = hosting.asked().len();
        self.action(Action::Clone(C::Source(Source::GitHub)))?;
        self.until("the GitHub list read again", |tab| matches!(tab.ui.cloning.listing(), Listing::Ready(_))).await?;
        if !hosting.asked()[asked..].iter().any(|ask| ask.path == "/user") {
            return Err("The GitHub tab did not read the list again".into());
        }
        self.action(Action::Clone(C::Close))?;
        fs::write(&global, kept)?;
        self.pass();
        Ok(())
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

    async fn signature_checked(&mut self, oid: &str) -> Result<(Option<Signature>, Vec<(String, Signature)>)> {
        let wanted = oid.to_owned();
        self.until(&format!("the signature of {oid} checked"), move |tab| {
            tab.ui.selection == Some(Selection::Commit(wanted.clone())) && tab.ui.signing.checked(&wanted)
        })
        .await?;
        let wanted = oid.to_owned();
        Ok(self.tab.read_with(self.cx, move |tab, _| (tab.ui.signing.of(&wanted).cloned(), tab.ui.signing.tags.clone()))?)
    }

    async fn reveal_signature(&mut self, oid: &str) -> Result<(Option<Signature>, Vec<(String, Signature)>)> {
        self.action(Action::Reveal(reveal::Action::Commit(oid.to_owned())))?;
        self.signature_checked(oid).await
    }

    async fn check_again(&mut self, oid: &str) -> Result<Signature> {
        self.action(Action::Signing(signing::Action::Recheck))?;
        let (commit, _) = self.signature_checked(oid).await?;
        commit.ok_or_else(|| format!("Checking {oid} again found no signature").into())
    }

    async fn a_signed_commit_shows_as_verified(&mut self, dir: &Path) -> Result<Signpost> {
        self.start("GAP1207");
        let signpost = signpost(dir)?;
        let root = signpost.root.clone();
        let before = snapshot(&root)?;
        let printed = keygen(&["-lf"], &signpost.keys.join("cedar.pub"), None)?;
        let fingerprint = printed.split_whitespace().nth(1).ok_or("ssh-keygen -lf printed no fingerprint")?.to_owned();
        self.open(&root)?;
        let painted = signpost.painted.clone();
        self.until("Signpost open on Paint the signpost", |tab| {
            tab.ui.repo == SIGNPOST && tab.ui.head.as_deref() == Some(painted.as_str())
        })
        .await?;
        let (commit, tags) = self.reveal_signature(&signpost.raised).await?;
        let Some(commit) = commit else {
            return Err("Raise the signpost shows no signature badge".into());
        };
        if (commit.grade, commit.word(), commit.detail().as_str()) != (Grade::Verified, "Verified", SIGNER) {
            return Err(format!("Raise the signpost shows {commit:?}").into());
        }
        let tip = commit.tip();
        for line in [
            format!("Verified, signed by {SIGNER}"),
            "SSH signature".to_owned(),
            format!("Signer: {SIGNER}"),
            format!("Fingerprint: {fingerprint}"),
        ] {
            if !tip.lines().any(|shown| shown == line) {
                return Err(format!("The badge's tooltip lacks {line:?}: {tip:?}").into());
            }
        }
        let tagged: Vec<(&str, Grade)> = tags.iter().map(|(name, signature)| (name.as_str(), signature.grade)).collect();
        if tagged != [("v1", Grade::Verified)] || tags[0].1.word() != "Verified" {
            return Err(format!("The tags at Raise the signpost show {tags:?}").into());
        }
        git_is(&root, &["log", "-1", "--format=%G? %GS", &signpost.raised], &format!("G {SIGNER}\n"))?;
        let again = self.check_again(&signpost.raised).await?;
        if again.grade != Grade::Verified {
            return Err(format!("Checking again shows {again:?}").into());
        }
        let (unsigned, tags) = self.reveal_signature(&signpost.painted).await?;
        if unsigned.is_some() || !tags.is_empty() {
            return Err(format!("Paint the signpost shows a badge: {unsigned:?} {tags:?}").into());
        }
        if snapshot(&root)? != before {
            return Err("Checking signatures changed Signpost".into());
        }
        self.pass();
        Ok(signpost)
    }

    async fn a_commit_signed_by_an_unknown_key(&mut self, signpost: &Signpost) -> Result<()> {
        self.start("GAP1208");
        let root = &signpost.root;
        let stranger = format!("user.signingKey={}", utf8(&signpost.keys.join("stranger.pub"))?);
        git(root, &["-c", &stranger, "commit", "--allow-empty", "-S", "-qm", "Visit the signpost"])?;
        let visited = oid(root, "HEAD")?;
        self.action(Action::Refresh)?;
        let tip = visited.clone();
        self.until("Signpost on Visit the signpost", |tab| tab.ui.head.as_deref() == Some(tip.as_str())).await?;
        let (commit, _) = self.reveal_signature(&visited).await?;
        let Some(commit) = commit else {
            return Err("Visit the signpost shows no signature badge".into());
        };
        let reason = "The key is not one of the allowed signers in gpg.ssh.allowedSignersFile.";
        if (commit.grade, commit.word(), commit.detail().as_str()) != (Grade::Unknown, "Signed", "cannot be checked here")
            || !commit.tip().lines().any(|line| line == "Signed; cannot be checked here")
            || !commit.tip().lines().any(|line| line == reason)
        {
            return Err(format!("Visit the signpost shows {commit:?}").into());
        }
        git_is(root, &["log", "-1", "--format=%G?", &visited], "U\n")?;

        git(root, &["config", "--local", "--unset", "gpg.ssh.allowedSignersFile"])?;
        self.reveal_signature(&signpost.raised).await?;
        let unchecked = self.check_again(&signpost.raised).await?;
        let said = unchecked.reason.clone().unwrap_or_default();
        if unchecked.sentence() != "Signed; cannot be checked here"
            || !said.starts_with("Git has no allowed signers file to check SSH signatures against")
        {
            return Err(format!("Without an allowed signers file Raise the signpost shows {unchecked:?}").into());
        }
        git(root, &["config", "--local", "gpg.ssh.allowedSignersFile", utf8(&signpost.allowed)?])?;
        let restored = self.check_again(&signpost.raised).await?;
        if restored.grade != Grade::Verified {
            return Err(format!("With the allowed signers file back Raise the signpost shows {restored:?}").into());
        }
        self.pass();
        Ok(())
    }

    fn signing_now(&mut self) -> Result<(bool, Option<bool>, bool)> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            (commit_options::signs(&tab.ui), tab.ui.commit_options.sign_chosen, signing::commit_chip(&tab.ui).is_some())
        })?)
    }

    async fn stage_for_commit(&mut self, root: &Path, name: &str, summary: &str) -> Result<()> {
        fs::write(root.join(name), format!("{name}\n"))?;
        git(root, &["add", "--", name])?;
        self.action(Action::Refresh)?;
        self.until(&format!("{name} staged"), |tab| tab.ui.file_ranges().1.len() == 1).await?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui.input.update(cx, |field, cx| field.set_text(summary, cx));
        })?;
        Ok(())
    }

    async fn unlock_lantern(&mut self, waiting: bool) -> Result<()> {
        self.during("the lantern key's passphrase asked for", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Askpass(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == UNLOCK_LANTERN)
        })
        .await?;
        let (masked, progress) = self.tab.read_with(self.cx, |tab, _| {
            let masked = prompt::shown(&tab.ui).and_then(|spec| spec.field.as_ref().map(|field| (field.label, field.masked)));
            let progress = tab.ui.jobs.write.as_ref().and_then(|job| job.notice).and_then(|id| tab.ui.notices.get(id)).map(|notice| notice.detail.clone());
            (masked, progress)
        })?;
        if masked != Some(("Passphrase", true)) || waiting && progress.as_deref() != Some("Waiting for your answer…") {
            return Err(format!("The unlock prompt has the field {masked:?} and the job says {progress:?}").into());
        }
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            let Some(field) = tab.ui.prompt.field().cloned() else { return false };
            field.update(cx, |field, cx| field.replace_text_in_range(None, LANTERN_PASSPHRASE, window, cx));
            field.read(cx).text() == LANTERN_PASSPHRASE
        })?;
        if !typed {
            return Err("The unlock prompt did not take the passphrase".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.during("the unlock prompt closed", |tab| !tab.ui.prompt.open()).await
    }

    async fn committed_as(&mut self, root: &Path, before: &str, summary: &str) -> Result<String> {
        let (from, wanted) = (before.to_owned(), summary.to_owned());
        self.until(&format!("{summary} committed"), move |tab| {
            tab.ui.head.as_deref().is_some_and(|head| head != from) && tab.ui.notices.newest().is_some_and(|notice| notice.detail == wanted)
        })
        .await?;
        git_is(root, &["log", "-1", "--format=%s"], &format!("{summary}\n"))?;
        oid(root, "HEAD")
    }

    async fn sign_one_commit_from_the_commit_box(&mut self, signpost: &Signpost) -> Result<()> {
        self.start("GAP1209");
        let root = &signpost.root;
        let lantern = signpost.keys.join("lantern.pub");
        git(root, &["config", "--local", "user.signingKey", utf8(&lantern)?])?;
        self.stage_for_commit(root, "lantern.txt", "Light the lantern").await?;
        let wanted = lantern.display().to_string();
        self.until("the lantern key read from the configuration", move |tab| {
            signing::config(&tab.ui).key.as_deref() == Some(wanted.as_str()) && !signing::config(&tab.ui).commits
        })
        .await?;
        if self.signing_now()? != (false, None, false) {
            return Err(format!("With commit.gpgSign off the commit box shows {:?}", self.signing_now()?).into());
        }
        self.action(Action::CommitOptions(commit_options::Action::ToggleOpen))?;
        self.action(Action::CommitOptions(commit_options::Action::ToggleSign))?;
        let described = self.tab.read_with(self.cx, |tab, _| signing::config(&tab.ui).described())?;
        if self.signing_now()? != (true, Some(true), true) || !described.contains("lantern.pub") {
            return Err(format!("After ticking Sign this commit the box shows {:?}, {described:?}", self.signing_now()?).into());
        }
        let asked = crate::askpass::shown_so_far();
        let before = oid(root, "HEAD")?;
        self.action(Action::Commit)?;
        self.unlock_lantern(true).await?;
        let lit = self.committed_as(root, &before, "Light the lantern").await?;
        git_is(root, &["log", "-1", "--format=%G? %GS", &lit], &format!("G {SIGNER}\n"))?;
        if self.signing_now()? != (false, None, false) {
            return Err(format!("After the signed commit the box shows {:?}", self.signing_now()?).into());
        }
        if let Some(entry) = crate::activity::entries().iter().find(|entry| entry.command_line().contains(LANTERN_PASSPHRASE)) {
            return Err(format!("A command had the passphrase in its arguments: {}", entry.command_line()).into());
        }

        let asked = asked + 1;
        self.stage_for_commit(root, "notes.txt", "Note the lantern").await?;
        self.action(Action::Commit)?;
        let noted = self.committed_as(root, &lit, "Note the lantern").await?;
        if signed_object(root, "commit", &noted)? || crate::askpass::shown_so_far() != asked {
            return Err("Note the lantern was signed, or a prompt appeared".into());
        }

        git(root, &["config", "--local", "commit.gpgSign", "true"])?;
        self.action(Action::Refresh)?;
        self.until("commit.gpgSign read as on", |tab| signing::config(&tab.ui).commits).await?;
        if self.signing_now()? != (true, None, true) {
            return Err(format!("With commit.gpgSign on the box shows {:?}", self.signing_now()?).into());
        }
        self.action(Action::CommitOptions(commit_options::Action::ToggleSign))?;
        if self.signing_now()? != (false, Some(false), false) {
            return Err(format!("After unticking Sign this commit the box shows {:?}", self.signing_now()?).into());
        }
        self.stage_for_commit(root, "route.txt", "Mark the route").await?;
        self.action(Action::Commit)?;
        let routed = self.committed_as(root, &noted, "Mark the route").await?;
        if signed_object(root, "commit", &routed)? || crate::askpass::shown_so_far() != asked {
            return Err("Mark the route was signed, or a prompt appeared".into());
        }
        if self.signing_now()? != (true, None, true) {
            return Err(format!("After the unsigned commit the box shows {:?}", self.signing_now()?).into());
        }

        git(root, &["config", "--local", "tag.gpgSign", "true"])?;
        self.action(Action::Refresh)?;
        self.until("tag.gpgSign read as on", |tab| signing::config(&tab.ui).tags).await?;
        self.ref_action(refs::Action::NewAt(refs::Kind::Tag, lit.clone()))?;
        self.type_ref("v2")?;
        let held = self.tab.read_with(self.cx, |tab, cx| (tab.ui.refs.draft_sign(), tab.ui.refs.sign_problem(cx)))?;
        if held != (Some(true), Some(refs::SIGNED_NEEDS_MESSAGE)) {
            return Err(format!("The new tag dialog shows {held:?}").into());
        }
        self.ref_action(refs::Action::Submit)?;
        if !git(root, &["tag", "--list", "v2"])?.is_empty() {
            return Err("A signed tag was created with no message".into());
        }
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.refs.message.update(cx, |field, cx| field.replace_text_in_range(None, "Lantern release", window, cx));
            tab.ui.refs.sign_problem(cx).is_none()
        })?;
        if !typed {
            return Err("The tag message did not clear the hold".into());
        }
        self.ref_action(refs::Action::Submit)?;
        self.unlock_lantern(false).await?;
        let (detail, _) = self.noticed("Tag created").await?;
        if detail != format!("v2 at {}, signed", crate::notice::short(&lit)) {
            return Err(format!("The tag notice says {detail:?}").into());
        }
        if !signed_object(root, "tag", "refs/tags/v2")? {
            return Err("Tag v2 has no SSH signature".into());
        }
        let (_, tags) = self.reveal_signature(&lit).await?;
        let tagged: Vec<(&str, Grade, &str)> = tags.iter().map(|(name, signature)| (name.as_str(), signature.grade, signature.signer.as_str())).collect();
        if tagged != [("v2", Grade::Verified, SIGNER)] {
            return Err(format!("Light the lantern lists the tags {tags:?}").into());
        }
        fs::remove_file(&signpost.link)?;
        git_is(root, &["status", "--porcelain"], "")?;
        self.pass();
        Ok(())
    }

    fn typed_in_prompt(&mut self, first: Option<&str>, second: Option<&str>) -> Result<bool> {
        let (first, second) = (first.map(str::to_owned), second.map(str::to_owned));
        Ok(self.tab.update(self.cx, |tab, window, cx| {
            let mut typed = true;
            for (field, text) in [(tab.ui.prompt.field().cloned(), first), (tab.ui.prompt.second().cloned(), second)] {
                let Some(text) = text else { continue };
                let Some(field) = field else { return false };
                field.update(cx, |field, cx| field.replace_text_in_range(None, &text, window, cx));
                typed &= field.read(cx).text() == text;
            }
            typed
        })?)
    }

    async fn generate_an_ssh_key(&mut self, signpost: &Signpost) -> Result<()> {
        use crate::keys::{self, Action as K};
        self.start("GAP1210");
        let root = &signpost.root;
        let sandbox = super::sandbox().ok_or("The replay has no sandbox")?;
        let folder = self.tab.read_with(self.cx, |tab, _| tab.ui.keys.places.folder.clone())?;
        if folder != sandbox.join("home/.ssh") || self.tab.read_with(self.cx, |tab, _| tab.ui.keys.places.keychain)? {
            return Err(format!("The SSH keys are looked for in {}, outside the sandbox, or with the Keychain", folder.display()).into());
        }
        let bin = sandbox.join("bin");
        let wrapper = bin.join("ssh-keygen");
        let arguments = sandbox.join("ssh-keygen-arguments.log");
        fs::create_dir_all(&bin)?;
        super::gap04::script(&wrapper, &format!("printf '%s\\n' \"$*\" >> '{}'\nexec {KEYGEN} \"$@\"\n", arguments.display()))?;
        let socket = sandbox.join("agent.sock");
        let agent = agent_at(&socket)?;
        self.tab.update(self.cx, |tab, _, _| tab.ui.keys.places.agent = Some(socket.clone()))?;

        self.action(Action::Preferences(preferences::Action::Open(Some(preferences::Section::Ssh))))?;
        self.until("the SSH keys read", |tab| {
            tab.ui.keys.settled() && matches!(&tab.ui.keys.ssh, Some(Ok(keys)) if keys.is_empty()) && matches!(&tab.ui.keys.agent, Some(Ok(keys)) if keys.is_empty())
        })
        .await?;
        let email = self.tab.read_with(self.cx, |tab, _| tab.ui.identity.effective().and_then(|values| values.email.clone()))?.ok_or("Signpost has no email")?;
        let asked = crate::askpass::shown_so_far();
        self.action(Action::Keys(K::Generate))?;
        let spec = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| {
            (spec.title.clone(), spec.lines.clone(), spec.field.as_ref().map(|f| (f.label, f.masked)), spec.second.as_ref().map(|f| (f.label, f.masked)))
        }))?;
        let label = format!("Its label is {email}.");
        match &spec {
            Some((title, lines, Some(("Name", false)), Some(("Passphrase", true)))) if title == "Generate an SSH key" && lines.contains(&label) => {}
            other => return Err(format!("Generate key… asks {other:?}").into()),
        }
        let refused = self.tab.read_with(self.cx, |tab, _| {
            prompt::shown(&tab.ui).and_then(|spec| spec.field.map(|field| ((field.validate)("has space", &tab.ui), (field.validate)("trail", &tab.ui))))
        })?;
        if !matches!(&refused, Some((Some(_), None))) {
            return Err(format!("The name field checks give {refused:?}").into());
        }
        if !self.typed_in_prompt(Some("trail"), Some(CAIRN_PASSPHRASE))? {
            return Err("The generate prompt did not take the name and passphrase".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let again = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| (spec.title.clone(), spec.field.map(|field| field.masked))))?;
        if again != Some(("Type the passphrase again".to_owned(), Some(true))) {
            return Err(format!("After the passphrase the prompt is {again:?}").into());
        }
        if !self.typed_in_prompt(Some(CAIRN_PASSPHRASE), None)? {
            return Err("The repeat prompt did not take the passphrase".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let (detail, offers) = self.noticed("SSH key generated").await?;
        let private = folder.join("id_ed25519_trail");
        let public = folder.join("id_ed25519_trail.pub");
        if detail != format!("id_ed25519_trail is in {}, with a passphrase. Copy its public key to your host.", folder.display()) || offers != ["Copy public key"] {
            return Err(format!("The key notice says {detail:?} and offers {offers:?}").into());
        }
        if !private.is_file() || !public.is_file() || fs::metadata(&private)?.permissions().mode() & 0o777 != 0o600 {
            return Err("The key files are missing or the private key is readable by others".into());
        }
        if crate::askpass::shown_so_far() != asked {
            return Err("ssh-keygen's passphrase questions were shown instead of answered by the app".into());
        }
        let logged = fs::read_to_string(&arguments).map_err(|error| format!("ssh-keygen left no arguments at {}: {error}", arguments.display()))?;
        let wanted = format!("-q -t ed25519 -C {email} -f {}", private.display());
        if !logged.lines().any(|line| line == wanted) || logged.contains(CAIRN_PASSPHRASE) {
            return Err(format!("ssh-keygen ran with the arguments {logged:?}").into());
        }
        if let Some(entry) = crate::activity::entries().iter().find(|entry| entry.command_line().contains(CAIRN_PASSPHRASE)) {
            return Err(format!("A command had the passphrase in its arguments: {}", entry.command_line()).into());
        }
        let printed = keygen(&["-l", "-f"], &public, None)?;
        let fingerprint = printed.split_whitespace().nth(1).ok_or("ssh-keygen -lf printed no fingerprint")?.to_owned();
        self.until("the new key listed", |tab| tab.ui.keys.settled() && !tab.ui.keys.keys().is_empty()).await?;
        let listed = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.keys.keys().iter().map(|key| (key.name(), key.printed.kind.clone(), key.printed.fingerprint.clone(), key.printed.comment.clone())).collect::<Vec<_>>()
        })?;
        if listed != [("id_ed25519_trail".to_owned(), "ED25519".to_owned(), fingerprint.clone(), email.clone())] {
            return Err(format!("The SSH page lists {listed:?}").into());
        }
        if keygen(&["-y", "-P", "", "-f"], &private, None).is_ok() {
            return Err("The new key opens without its passphrase".into());
        }
        let opener = sandbox.join("open-cairn.sh");
        super::gap04::script(&opener, &format!("echo '{CAIRN_PASSPHRASE}'\n"))?;
        let opened = keygen(&["-y", "-f"], &private, Some(&opener));
        fs::remove_file(&opener)?;
        let text = fs::read_to_string(&public)?;
        let body: Vec<&str> = text.split_whitespace().take(2).collect();
        if opened?.split_whitespace().take(2).collect::<Vec<_>>() != body {
            return Err("The passphrase does not open the new key".into());
        }

        let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
        let copy = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.items().iter().find(|n| n.title == "SSH key generated").and_then(|n| n.actions.first().map(|(_, action)| action.clone()))
        })?;
        self.action(copy.ok_or("The key notice has no copy offer")?)?;
        let copied = self.cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
        self.cx.update(|cx| cx.write_to_clipboard(saved.unwrap_or_else(|| gpui::ClipboardItem::new_string(String::new()))))?;
        if copied.as_deref() != Some(text.trim()) {
            return Err(format!("Copy public key copied {copied:?}").into());
        }

        self.action(Action::Keys(K::AddToAgent(private.clone())))?;
        self.during("the new key's passphrase asked for by ssh-add", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Askpass(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == "Unlock SSH key id_ed25519_trail")
        })
        .await?;
        if !self.typed_in_prompt(Some(CAIRN_PASSPHRASE), None)? {
            return Err("The unlock prompt did not take the passphrase".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let (added, _) = self.noticed("Key added to the agent").await?;
        if added != "id_ed25519_trail is unlocked for ssh until the agent stops." {
            return Err(format!("The agent notice says {added:?}").into());
        }
        if !agent_listed(&socket)?.lines().any(|line| line.split_whitespace().take(2).collect::<Vec<_>>() == body) {
            return Err("The sandbox agent does not hold the new key".into());
        }
        self.until("the agent's keys read again", |tab| tab.ui.keys.settled() && tab.ui.keys.agent_keys().len() == 1).await?;
        let titled = self.tab.read_with(self.cx, |tab, _| tab.ui.keys.agent_keys().first().map(|key| key.title()))?;
        if titled.as_deref() != Some(email.as_str()) {
            return Err(format!("The agent row reads {titled:?}").into());
        }
        let added_with = crate::activity::entries().iter().rev().find(|entry| entry.command_line().contains("ssh-add") && entry.command_line().contains("id_ed25519_trail")).map(|entry| entry.command_line());
        match added_with {
            Some(line) if !line.contains("apple-use-keychain") && !line.contains(CAIRN_PASSPHRASE) => {}
            other => return Err(format!("ssh-add ran as {other:?}").into()),
        }

        let before = snapshot(root)?;
        self.action(Action::Keys(K::UseForRepository(Some(private.clone()))))?;
        let wanted = keys::ssh_command_for(&private);
        let expected = wanted.clone();
        self.until("the key chosen for Signpost", move |tab| tab.ui.keys.settled() && matches!(&tab.ui.keys.command, Some(Ok(Some(command))) if *command == expected)).await?;
        git_is(root, &["config", "get", "--local", "core.sshCommand"], &format!("{wanted}\n"))?;
        if !wanted.ends_with(" -o IdentitiesOnly=yes") || !wanted.starts_with("ssh -i ") {
            return Err(format!("The SSH command is {wanted:?}").into());
        }
        git(root, &["config", "--local", "core.sshCommand", "ssh -i /elsewhere"])?;
        self.action(Action::Keys(K::UseForRepository(None)))?;
        self.until("the changed SSH command refused", |tab| tab.ui.keys.settled() && tab.ui.keys.error.as_deref().is_some_and(|error| error.contains("meanwhile"))).await?;
        git_is(root, &["config", "get", "--local", "core.sshCommand"], "ssh -i /elsewhere\n")?;
        self.until("the SSH command read again", |tab| matches!(&tab.ui.keys.command, Some(Ok(Some(command))) if command == "ssh -i /elsewhere")).await?;
        self.action(Action::Keys(K::UseForRepository(None)))?;
        self.until("the SSH command removed", |tab| tab.ui.keys.settled() && matches!(&tab.ui.keys.command, Some(Ok(None)))).await?;
        if git(root, &["config", "--local", "--get-all", "core.sshCommand"]).is_ok() {
            return Err("Stop using it left core.sshCommand".into());
        }

        let opened = crate::handoff::opened().len();
        self.action(Action::Handoff(crate::handoff::Action::OpenUrl(keys::Host::GitHub.page().to_owned())))?;
        if crate::handoff::opened().get(opened).map(String::as_str) != Some("https://github.com/settings/ssh/new") {
            return Err(format!("Adding a key on GitHub opened {:?}", crate::handoff::opened().get(opened)).into());
        }

        self.action(Action::Preferences(preferences::Action::Show(preferences::Section::Signing)))?;
        self.until("the signing settings read", |tab| tab.ui.keys.settled() && matches!(&tab.ui.keys.signing, Some(Ok(_)))).await?;
        let scope = self.tab.read_with(self.cx, |tab, _| tab.ui.keys.scope)?;
        if scope != crate::identity::Scope::Local {
            return Err(format!("Signpost's own signing settings show for {scope:?}").into());
        }
        let chosen = public.to_string_lossy().into_owned();
        self.action(Action::Keys(K::UseSigningKey(chosen.clone())))?;
        let expected = chosen.clone();
        self.until("the new key chosen for signing", move |tab| {
            tab.ui.keys.settled() && signing::config(&tab.ui).key.as_deref() == Some(expected.as_str())
        })
        .await?;
        git_is(root, &["config", "get", "--local", "user.signingkey"], &format!("{chosen}\n"))?;

        self.action(Action::Keys(K::Generate))?;
        let taken = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).and_then(|spec| spec.field.and_then(|field| (field.validate)("trail", &tab.ui))))?;
        if taken != Some(format!("id_ed25519_trail already exists in {}.", folder.display())) {
            return Err(format!("A second key named trail is checked as {taken:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        drop(agent);
        fs::remove_file(&wrapper)?;
        self.action(Action::Preferences(preferences::Action::Close))?;
        if snapshot(root)? != before {
            return Err("Choosing an SSH key changed Signpost's refs or files".into());
        }
        self.pass();
        Ok(())
    }

    fn tool_row(&mut self, path: &str) -> Result<(String, Option<String>, Option<Action>)> {
        let target = Target::Conflict(path.into());
        self.menu_row(&target, "file.diff-tool")?.ok_or_else(|| format!("The menu of {path} has no merge tool row").into())
    }

    async fn looked_for_tools(&mut self, found: bool) -> Result<()> {
        self.action(Action::Tools(crate::tools::Action::Look))?;
        self.until("the tools looked for again", move |tab| {
            crate::tools::detected().is_some_and(|detected| detected.comparers.iter().any(|tool| tool.tool.id == "smerge") == found)
                && tab.ui.difftool.configured.is_some()
        })
        .await
    }

    async fn open_a_conflict_in_a_merge_tool(&mut self, dir: &Path) -> Result<()> {
        use crate::difftool::{Action as D, Configured, Mode, Pick};
        self.start("GAP1211");
        let sandbox = super::sandbox().ok_or("The replay has no sandbox")?;
        let global = super::global_config(&sandbox);
        let global_before = fs::read(&global)?;
        let root = init(dir, FERRY_TOOL)?;
        record(&root, "ridge.txt", "Camp below the ridge\n", "Camp below the ridge")?;
        record(&root, "camp.txt", "Tents\n", "Pitch the tents")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        fs::write(root.join("ridge.txt"), "Camp on the ridge\n")?;
        fs::write(root.join("camp.txt"), "Tents and tarps\n")?;
        git(&root, &["commit", "-qam", "Move camp up the ridge"])?;
        git(&root, &["switch", "-q", "main"])?;
        fs::write(root.join("ridge.txt"), "Camp by the lake\n")?;
        fs::write(root.join("camp.txt"), "Tents and stoves\n")?;
        git(&root, &["commit", "-qam", "Move camp to the lake"])?;
        let local_before = fs::read(root.join(".git/config"))?;
        let ridge = oid(&root, "feature/ridge")?;
        self.open(&root)?;
        self.until("Ferry-tool open on main", |tab| tab.ui.branch == "main" && tab.ui.history.iter().any(|row| row.oid == ridge)).await?;
        self.begin_merge("feature/ridge").await?;
        self.until("the merge stopped on two conflicts", |tab| {
            let conflicted = tab.ui.operation.conflicted();
            tab.ui.operation.active() && conflicted.len() == 2 && conflicted.contains(&Path::new("ridge.txt"))
        })
        .await?;

        self.looked_for_tools(false).await?;
        let unread = self.tab.read_with(self.cx, |tab, _| tab.ui.difftool.configured.clone())?;
        if unread != Some(Ok(Configured::default())) {
            return Err(format!("The sandbox's Git has a diff or merge tool set: {unread:?}").into());
        }
        let (title, disabled, action) = self.tool_row("ridge.txt")?;
        let Some(action) = action.filter(|_| title == "Open in merge tool" && disabled.is_none()) else {
            return Err(format!("The conflict's menu offers {title:?}, disabled {disabled:?}").into());
        };
        self.action(action)?;
        self.until("Preferences open at External tools", |tab| {
            tab.ui.preferences.shows_tools() && tab.ui.notices.items().iter().any(|n| n.title == "Choose a merge tool first")
        })
        .await?;
        let said = self.notice_titled("Choose a merge tool first")?.map(|(_, detail, _, _)| detail);
        if said.as_deref() != Some("FastKraken found no merge tool it knows on this Mac, and Git has none set.") {
            return Err(format!("With no merge tool the notice says {said:?}").into());
        }

        let bin = sandbox.join("bin");
        let tool = bin.join("smerge");
        let arguments = sandbox.join("smerge-arguments.log");
        fs::create_dir_all(&bin)?;
        super::gap04::script(
            &tool,
            &format!(
                "printf '%s\\n' \"$*\" >> '{}'\n[ \"$1\" = mergetool ] || exit 2\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; *) shift;; esac; done\n[ -n \"$out\" ] || exit 0\n/bin/sleep 2\nprintf 'Settled %s\\n' \"$out\" > \"$out\"\n",
                arguments.display()
            ),
        )?;
        self.looked_for_tools(true).await?;
        let wanted = tool.clone();
        let shown = self.tab.read_with(self.cx, |tab, _| {
            let found = crate::tools::detected()?;
            match tab.ui.difftool.pick(Mode::Merge, &found)? {
                Pick::Found(found) => Some(found.place().to_owned()),
                _ => None,
            }
        })?;
        if shown != Some(wanted) {
            return Err(format!("External tools shows the merge tool at {shown:?}").into());
        }
        self.action(Action::DiffTool(D::Choose(Mode::Merge, "smerge".into())))?;
        let saved = self.cx.update(|cx| crate::settings::settings(cx).merge_tool)?;
        if saved.as_deref() != Some("smerge") {
            return Err(format!("The chosen merge tool was saved as {saved:?}").into());
        }
        self.action(Action::Preferences(preferences::Action::Close))?;

        let (title, disabled, action) = self.tool_row("ridge.txt")?;
        let Some(action) = action.filter(|_| title == "Open in Sublime Merge" && disabled.is_none()) else {
            return Err(format!("After choosing, the conflict's menu offers {title:?}, disabled {disabled:?}").into());
        };
        self.action(action)?;
        self.during("Sublime Merge open on ridge.txt without blocking the window", |tab| {
            tab.ui.difftool.is_open(Mode::Merge, Path::new("ridge.txt")) && !tab.ui.busy() && tab.ui.jobs.network.is_none()
        })
        .await?;
        let (_, disabled, _) = self.tool_row("ridge.txt")?;
        if disabled.as_deref() != Some("ridge.txt is already open in the merge tool") {
            return Err(format!("While the tool is open, the row reads disabled {disabled:?}").into());
        }
        self.until("ridge.txt moved to Resolved", |tab| {
            !tab.ui.difftool.is_open(Mode::Merge, Path::new("ridge.txt"))
                && tab.ui.operation.resolved().iter().any(|path| path == Path::new("ridge.txt"))
                && tab.ui.operation.conflicted() == [Path::new("camp.txt")]
        })
        .await?;
        let (detail, _) = self.noticed("Resolved ridge.txt").await?;
        if detail != "Sublime Merge saved the merged file, and it is marked resolved." {
            return Err(format!("The resolved notice says {detail:?}").into());
        }
        git_is(&root, &["ls-files", "--unmerged", "--", "ridge.txt"], "")?;
        git_is(&root, &["show", ":ridge.txt"], "Settled ridge.txt\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all", "--ignored"], "UU camp.txt\nM  ridge.txt\n")?;
        if root.join("ridge.txt.orig").exists() {
            return Err("The merge tool left ridge.txt.orig".into());
        }
        let logged = fs::read_to_string(&arguments)?;
        let words: Vec<&str> = logged.trim_end().split(' ').collect();
        let temporary = |word: &str, side: &str| word.contains(&format!("ridge_{side}_")) && !Path::new(word).starts_with(&root);
        if words.len() != 6
            || words[0] != "mergetool"
            || !temporary(words[1], "BASE")
            || !temporary(words[2], "LOCAL")
            || !temporary(words[3], "REMOTE")
            || words[4..] != ["-o", "ridge.txt"]
        {
            return Err(format!("Sublime Merge was run as {logged:?}").into());
        }

        self.action(Action::Merge(crate::merge::Action::Open("camp.txt".into())))?;
        self.until("camp.txt in the resolver", |tab| {
            crate::view::centre(&tab.ui) == crate::view::Centre::Resolver && tab.ui.merge.editing() == Some(Path::new("camp.txt"))
        })
        .await?;
        self.key("cmd-d")?;
        self.until("camp.txt resolved from Cmd-D", |tab| {
            !tab.ui.difftool.is_open(Mode::Merge, Path::new("camp.txt"))
                && tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved().len() == 2
                && tab.ui.merge.editing().is_none()
        })
        .await?;
        git_is(&root, &["show", ":camp.txt"], "Settled camp.txt\n")?;
        git_is(&root, &["status", "--porcelain=v1", "--untracked-files=all", "--ignored"], "M  camp.txt\nM  ridge.txt\n")?;
        if fs::read_to_string(&arguments)?.lines().count() != 2 {
            return Err("Cmd-D did not run the merge tool exactly once more".into());
        }
        self.action(Action::Select(file("ridge.txt", true)))?;
        self.until("ridge.txt selected under Staged", |tab| tab.ui.selection == Some(file("ridge.txt", true))).await?;
        self.key("cmd-d")?;
        self.until("the staged change sent to the diff tool", |_| {
            fs::read_to_string(&arguments).is_ok_and(|logged| logged.lines().count() == 3)
        })
        .await?;
        self.until("the diff tool closed", |tab| !tab.ui.difftool.is_open(Mode::Diff, Path::new("ridge.txt"))).await?;
        let compared = fs::read_to_string(&arguments)?;
        let third: Vec<&str> = compared.lines().nth(2).unwrap_or_default().split(' ').collect();
        if third.len() != 3 || third[0] != "mergetool" || !third[1].contains("ridge") || !third[2].contains("ridge") {
            return Err(format!("Cmd-D on a staged file ran the diff tool as {compared:?}").into());
        }
        let failed = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.items().iter().find(|n| n.tone == crate::view::Tone::Error && n.title.contains("did not")).map(|n| n.title.clone()))?;
        if let Some(title) = failed {
            return Err(format!("The diff tool reported {title:?}").into());
        }
        if fs::read(&global)? != global_before || fs::read(root.join(".git/config"))? != local_before {
            return Err("Opening the merge tool changed a Git configuration file".into());
        }

        fs::remove_file(&tool)?;
        fs::remove_file(&arguments)?;
        self.looked_for_tools(false).await?;
        self.pass();
        Ok(())
    }
}
