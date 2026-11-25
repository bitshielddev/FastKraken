use super::*;
use crate::{
    activity, askpass,
    commands::{self, Target},
    address, explain, handoff, jobs,
    notice::{self as notices, Notice, Tone},
    prompt::{self, Stop},
    sync,
};
use std::os::unix::fs::PermissionsExt;

impl Replay<'_> {
    pub(super) async fn gap04(&mut self, dir: &Path) -> Result<()> {
        self.commit_during_fetch(dir).await?;
        self.cancel_hanging_fetch(dir).await?;
        self.fetch_progress(dir).await?;
        self.push_in_activity_log(dir).await?;
        self.unreachable_explained(dir).await?;
        self.changed_host_key(dir).await?;
        self.password_remembered(dir).await?;
        self.rejected_password(dir).await?;
        self.background_never_asks(dir).await?;
        self.fetch_one_remote(dir).await?;
        self.edit_address(dir).await?;
        self.prune_on_fetch(dir).await?;
        self.username_in_address(dir).await
    }

    pub(super) async fn failed(&mut self, description: &str, title: &str) -> Result<Notice> {
        let wanted = title.to_owned();
        self.until(description, move |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Error && notice.title == wanted)
        })
        .await?;
        let notice = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().cloned())?;
        notice.ok_or_else(|| "The error notice went".into())
    }

    pub(super) async fn open_clone(&mut self, root: &Path) -> Result<()> {
        self.open(root)?;
        self.until("the clone open on main", |tab| {
            tab.ui.branch == "main" && tab.ui.history.iter().any(|row| row.subject == "Start the trail")
        })
        .await
    }

    async fn commit_during_fetch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0401");
        let (root, bare, peer) = published(dir, "Lookout-ridge")?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        record(&peer, "lookout.txt", "Lookout: north face\n", "Mark the lookout")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        let (pid, release) = (dir.join("lookout-ridge.pid"), dir.join("lookout-ridge.release"));
        let gate = dir.join("lookout-ridge-upload-pack");
        script(
            &gate,
            &format!(
                "echo \"$$ $PPID\" > '{pid}'\nwhile [ ! -e '{release}' ]; do [ -e '{pid}' ] || exit 1; /bin/sleep 0.05; done\nexec git upload-pack \"$@\"\n",
                pid = pid.display(),
                release = release.display()
            ),
        )?;
        git(&root, &["config", "remote.origin.uploadpack", path(&gate)?])?;
        self.open_clone(&root).await?;

        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.during("the fetch waiting on origin", |tab| tab.ui.jobs.network.is_some() && pid.exists()).await?;
        let reasons = self.tab.read_with(self.cx, |tab, cx| {
            ["repo.fetch-all", "repo.pull", "repo.push"].map(|id| {
                commands::find(id).and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx)).and_then(|entry| entry.disabled)
            })
        })?;
        let waiting = Some("A fetch is already running".to_owned());
        if reasons != [waiting.clone(), waiting.clone(), waiting] {
            return Err(format!("While the fetch runs, Fetch, Pull and Push said {reasons:?}").into());
        }
        fs::write(root.join("notes.txt"), NOTE)?;
        self.during("the new note listed", |tab| !tab.ui.busy() && files(tab, &[], &["notes.txt"])).await?;
        self.action(Action::StageAll)?;
        self.during("the note staged", |tab| !tab.ui.busy() && files(tab, &["notes.txt"], &[])).await?;
        let start = oid(&root, "HEAD")?;
        self.type_commit("Note the lookout")?;
        self.during("the commit made while the fetch runs", |tab| {
            !tab.ui.busy() && tab.ui.head.as_deref().is_some_and(|head| head != start) && tab.ui.files.is_empty()
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%s"], "Note the lookout\n")?;
        git_is(&root, &["show", "HEAD:notes.txt"], NOTE)?;
        let fetching = self.tab.read_with(self.cx, |tab, _| tab.ui.jobs.network.is_some())?;
        if !fetching || oid(&root, "refs/remotes/origin/main")? != before {
            return Err("The fetch finished before the commit was made.".into());
        }
        fs::write(&release, "")?;
        self.until("the fetch finished after the commit", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        git_is(&root, &["log", "-1", "--format=%s"], "Note the lookout\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;

        record(&peer, "summit.txt", "Summit: cairn\n", "Mark the summit")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let summit = oid(&peer, "HEAD")?;
        let hooks = dir.join("lookout-ridge-hooks");
        fs::create_dir(&hooks)?;
        let (holding, letting) = (dir.join("lookout-ridge.hook"), dir.join("lookout-ridge.hook-release"));
        script(
            &hooks.join("pre-commit"),
            &format!(
                ": > '{hold}'\nwhile [ ! -e '{let_go}' ]; do [ -e '{hold}' ] || exit 1; /bin/sleep 0.05; done\n",
                hold = holding.display(),
                let_go = letting.display()
            ),
        )?;
        git(&root, &["config", "core.hooksPath", path(&hooks)?])?;
        fs::write(root.join("camp.txt"), "Camp: below the ridge\n")?;
        self.until("the camp note listed", |tab| files(tab, &[], &["camp.txt"])).await?;
        self.action(Action::StageAll)?;
        self.until("the camp note staged", |tab| files(tab, &["camp.txt"], &[])).await?;
        let noted = oid(&root, "HEAD")?;
        self.type_commit("Record the camp")?;
        self.during("the commit held by its hook", |tab| tab.ui.busy() && holding.exists()).await?;
        self.action(Action::Sync(sync::Action::Pull))?;
        self.during("the pull fetched and waiting for the commit", |tab| {
            running(tab).is_some_and(|notice| notice.detail == "Fetched. Waiting for the current operation to finish…" && notice.job.is_none())
        })
        .await?;
        if oid(&root, "refs/remotes/origin/main")? != summit || oid(&root, "HEAD")? != noted {
            return Err("The pull's fetch did not finish before the commit, or something moved main.".into());
        }
        fs::write(&letting, "")?;
        self.until("the pull integrated after the commit", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Pulled Successfully")
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%s", "HEAD^1"], "Record the camp\n")?;
        git_is(&root, &["rev-parse", "HEAD^2"], &format!("{summit}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn cancel_hanging_fetch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0402");
        let (root, bare, peer) = published(dir, "Stalled-pass")?;
        record(&peer, "pass.txt", "Pass: closed for snow\n", "Close the pass")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let pid = dir.join("stalled-pass.pid");
        let hang = dir.join("stalled-pass-upload-pack");
        script(&hang, &format!("echo \"$$ $PPID\" > '{}'\nexec /bin/sleep 600\n", pid.display()))?;
        git(&root, &["config", "remote.origin.uploadpack", path(&hang)?])?;
        self.open_clone(&root).await?;
        let refs = git(&root, &["for-each-ref", "refs/remotes"])?;

        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.during("the hung fetch offering Cancel", |tab| pid.exists() && running(tab).is_some_and(|notice| notice.job.is_some())).await?;
        let processes: Vec<libc::pid_t> = fs::read_to_string(&pid)?
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        let job = self
            .tab
            .read_with(self.cx, |tab, _| running(tab).and_then(|notice| notice.job))?
            .ok_or("The fetch's notice has no Cancel button")?;
        let asked = Instant::now();
        self.action(Action::Jobs(jobs::Action::Cancel(job)))?;
        self.until("the fetch cancelled", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| {
                notice.tone == Tone::Info
                    && notice.title == "Fetch cancelled"
                    && notice.detail == "Refs that were already updated stay updated."
            })
        })
        .await?;
        let took = asked.elapsed();
        if took > Duration::from_secs(3) {
            return Err(format!("Cancelling the fetch took {took:?}").into());
        }
        for process in processes {
            let start = Instant::now();
            while unsafe { libc::kill(process, 0) } == 0 {
                if start.elapsed() > Duration::from_secs(2) {
                    return Err(format!("Process {process} is still running after the cancel").into());
                }
                self.cx.background_executor().timer(Duration::from_millis(20)).await;
            }
        }
        git_is(&root, &["for-each-ref", "refs/remotes"], &refs)?;
        let fetch = self.tab.read_with(self.cx, |tab, cx| {
            commands::find("repo.fetch-all").and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx)).map(|entry| entry.disabled)
        })?;
        if fetch != Some(None) {
            return Err(format!("After the cancel, Fetch is {fetch:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn fetch_progress(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0403");
        let (root, bare, peer) = published(dir, "Signal-hill")?;
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for sheet in 0..48 {
            let bytes: Vec<u8> = (0..8192)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            fs::write(peer.join(format!("sheet-{sheet:02}.bin")), bytes)?;
        }
        git(&peer, &["add", "--", "."])?;
        git(&peer, &["commit", "-qm", "Survey the hill"])?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        let slow = dir.join("signal-hill-upload-pack");
        let chunk = dir.join("signal-hill.chunk");
        script(
            &slow,
            &format!(
                "git upload-pack \"$@\" | while :; do\n  /bin/dd bs=8192 count=1 of='{chunk}' 2>/dev/null\n  [ -s '{chunk}' ] || break\n  /bin/cat '{chunk}'\n  /bin/sleep 0.03\ndone\n",
                chunk = chunk.display()
            ),
        )?;
        git(&root, &["config", "remote.origin.uploadpack", path(&slow)?])?;
        self.open_clone(&root).await?;

        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.during("a phase and a percentage on the fetch's notice", |tab| {
            running(tab).is_some_and(|notice| {
                let phase = ["Counting objects", "Compressing objects", "Receiving objects", "Resolving deltas"]
                    .iter()
                    .any(|phase| notice.detail.starts_with(&format!("{phase} ")));
                notice.title == "Fetching all remotes…" && phase && notice.detail.contains('%') && notice.bar.is_some()
            })
        })
        .await?;
        self.until("the fetch finished", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully")
        })
        .await?;
        let detail = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|notice| notice.detail.clone()))?;
        if detail.as_deref() != Some("1 branch updated.") {
            return Err(format!("The fetch's outcome said {detail:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        fs::remove_file(&chunk)?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    async fn push_in_activity_log(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0404");
        let (root, bare, _) = published(dir, "Cairn-col")?;
        script(
            &bare.join("hooks/post-receive"),
            "echo\necho \"Create a pull request for 'main' by visiting:\"\necho \"     https://example.invalid/cedar/cairn-col/pull/new/main\"\necho \"Mirrored to https://scout:hunter2@mirror.invalid/cairn-col.git\"\n",
        )?;
        record(&root, "cairn.txt", "Cairn: at the col\n", "Build the cairn")?;
        let tip = oid(&root, "HEAD")?;
        self.open_clone(&root).await?;
        self.until("the cairn listed", |tab| tab.ui.head.as_deref() == Some(tip.as_str())).await?;

        self.action(Action::Sync(sync::Action::Push))?;
        self.until("the push finished", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Pushed Successfully")
        })
        .await?;
        git_is(dir, &["--git-dir", path(&bare)?, "rev-parse", "main"], &format!("{tip}\n"))?;
        let link = "https://example.invalid/cedar/cairn-col/pull/new/main";
        let notice = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().cloned())?.ok_or("No notice after the push")?;
        let open = notice.actions.iter().position(|(label, action)| {
            label.as_ref() == "Open"
                && matches!(action, Action::Handoff(handoff::Action::OpenUrl(url)) if url == link)
        });
        if !notice.detail.contains("Create a pull request for 'main' by visiting:")
            || !notice.detail.contains("https://•••@mirror.invalid/cairn-col.git")
            || notice.text().contains("hunter2")
            || notice.output.as_deref().is_some_and(|output| output.contains("hunter2"))
            || open.is_none()
        {
            return Err(format!("The push's notice said {:?}, offering {:?}", notice.detail, notice.actions).into());
        }
        let id = notice.log.ok_or("The push's notice does not offer Show in activity log")?;
        let entry = activity::find(id).ok_or("The push is not in the activity log")?;
        let command = entry.command_line();
        if !(entry.argv.first().map(String::as_str) == Some("git")
            && entry.argv.iter().any(|arg| arg == "push")
            && command.contains("push --porcelain --progress")
            && command.ends_with("-- origin refs/heads/main:refs/heads/main")
            && entry.exit == Some(0)
            && entry.result() == "exit 0"
            && entry.duration > Duration::ZERO
            && !entry.read_only
            && entry.dir == root.canonicalize()?
            && entry.stderr.contains("remote: Mirrored to https://•••@mirror.invalid/cairn-col.git"))
        {
            return Err(format!("The push was logged as {entry:?}").into());
        }
        for logged in activity::entries() {
            if format!("{} {} {}", logged.command_line(), logged.stdout, logged.stderr).contains("hunter2") {
                return Err(format!("A credential reached the activity log: {logged:?}").into());
            }
        }

        self.action(Action::Activity(activity::Action::Show(id)))?;
        self.until("the activity log open at the push", |tab| {
            view::centre(&tab.ui) == view::Centre::Activity && tab.ui.activity.selected().is_some_and(|entry| entry.id == id)
        })
        .await?;
        let listed = self.tab.read_with(self.cx, |tab, _| {
            let shown = tab.ui.activity.shown();
            (tab.ui.activity.show_reads(), shown.iter().any(|entry| entry.read_only), shown.first().map(|entry| entry.id) == Some(id))
        })?;
        if listed != (false, false, true) {
            return Err(format!("Before Show reads, the log listed (reads shown, a read listed, push newest) = {listed:?}").into());
        }
        self.action(Action::Activity(activity::Action::ToggleReads))?;
        self.until("the reads listed", |tab| tab.ui.activity.show_reads() && tab.ui.activity.shown().iter().any(|entry| entry.read_only && entry.summary().starts_with("git status"))).await?;
        self.action(Action::Activity(activity::Action::ToggleReads))?;
        self.until("the reads hidden again", |tab| !tab.ui.activity.shown().iter().any(|entry| entry.read_only)).await?;
        self.action(Action::Activity(activity::Action::Select(id)))?;
        let copied = self.copied(Action::Activity(activity::Action::CopyCommand), "Copied command").await?;
        if copied.as_deref() != Some(command.as_str()) {
            return Err(format!("Copy command copied {copied:?}, not {command:?}").into());
        }

        let index = open.unwrap_or_default();
        self.action(Action::Notice(notices::Action::Take(notice.id, index)))?;
        self.until("the link followed", |_| handoff::opened().iter().any(|url| url == link)).await?;

        self.key("escape")?;
        self.until("the graph back", |tab| view::centre(&tab.ui) == view::Centre::History).await?;
        self.key("cmd-shift-y")?;
        self.until("the log open by its shortcut", |tab| tab.ui.activity.open()).await?;
        self.key("cmd-shift-y")?;
        self.until("the log closed by its shortcut", |tab| !tab.ui.activity.open()).await?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }
}

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };
const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINhevt3r6VlryoT9qGlrR2tgCE7NfNYZeA5H/NtZ8Ai6";
const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIJs5d6wjK7Zo/eUimzVpMUzkn63seGzZJNxbDoo/dH6";

impl Replay<'_> {
    async fn unreachable_explained(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0405");
        let (root, bare, peer) = published(dir, "Scree-slope")?;
        record(&root, "scree.txt", "Scree: loose underfoot\n", "Cross the scree")?;
        let tip = oid(&root, "HEAD")?;
        let ssh = dir.join("scree-slope-ssh");
        script(
            &ssh,
            "printf '%s\\n' 'ssh: Could not resolve hostname scree.invalid: nodename nor servname provided, or not known' >&2\nexit 255\n",
        )?;
        git(&root, &["config", "core.sshCommand", path(&ssh)?])?;
        git(&root, &["config", "remote.origin.pushurl", "ssh://git@scree.invalid/scree-slope.git"])?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        git(&root, &["config", "remote.origin.url", &format!("http://127.0.0.1:{port}/scree-slope.git")])?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        self.open_clone(&root).await?;
        self.until("the new commit listed", |tab| tab.ui.head.as_deref() == Some(tip.as_str())).await?;

        self.action(Action::Sync(sync::Action::Push))?;
        let pushed = self.failed("the push explained", "Could not reach scree.invalid").await?;
        let retry = |notice: &Notice, again: fn(&Action) -> bool| {
            notice.actions.len() == 1 && notice.actions[0].0.as_ref() == "Retry" && again(&notice.actions[0].1)
        };
        if pushed.detail != "Push failed. Check your connection."
            || !retry(&pushed, |action| matches!(action, Action::Sync(sync::Action::Push)))
            || !pushed.output.as_deref().is_some_and(|output| output.contains("Could not resolve hostname scree.invalid"))
            || pushed.log.is_none()
        {
            return Err(format!("The unreachable push said {pushed:?}").into());
        }
        git_is(dir, &["--git-dir", path(&bare)?, "rev-parse", "main"], &format!("{before}\n"))?;
        self.action(Action::Notice(notices::Action::Dismiss(pushed.id)))?;

        self.action(Action::Sync(sync::Action::FetchAll))?;
        let fetched = self.failed("the fetch explained", "Could not reach 127.0.0.1").await?;
        let said = format!("Failed to connect to 127.0.0.1 port {port}");
        if fetched.detail != "Fetch failed. Check your connection."
            || !retry(&fetched, |action| matches!(action, Action::Sync(sync::Action::FetchAll)))
            || !fetched.output.as_deref().is_some_and(|output| output.contains(&said) && output.contains("unable to access"))
            || fetched.log.is_none()
        {
            return Err(format!("The unreachable fetch said {fetched:?}").into());
        }
        self.action(Action::Notice(notices::Action::ToggleOutput(fetched.id)))?;
        self.until("Git's output shown", |tab| tab.ui.notices.get(fetched.id).is_some_and(|notice| notice.expanded)).await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;
        let entry = activity::find(fetched.log.unwrap_or_default()).ok_or("The fetch is not in the activity log")?;
        if !entry.argv.iter().any(|arg| arg == "fetch") || entry.exit == Some(0) || !entry.stderr.contains(&said) {
            return Err(format!("The notice points at {entry:?}").into());
        }

        record(&peer, "slope.txt", "Slope: settled\n", "Settle the slope")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let settled = oid(&peer, "HEAD")?;
        git(&root, &["config", "remote.origin.url", path(&bare)?])?;
        self.action(Action::Notice(notices::Action::Take(fetched.id, 0)))?;
        self.until("the retried fetch finished", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{settled}\n"))?;
        self.pass();
        Ok(())
    }

    async fn changed_host_key(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0406");
        let (root, _, _) = published(dir, "Talus-field")?;
        let known = dir.join("talus-field-known_hosts");
        let saved = format!("other.invalid {KEY_A}\ntalus.invalid {KEY_B}\n");
        fs::write(&known, &saved)?;
        let warning = [
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@",
            "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @",
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@",
            "IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!",
            "Someone could be eavesdropping on you right now (man-in-the-middle attack)!",
            "It is also possible that a host key has just been changed.",
            "The fingerprint for the ED25519 key sent by the remote host is",
            "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.",
            "Please contact your system administrator.",
            &format!("Add correct host key in {} to get rid of this message.", known.display()),
            &format!("Offending ED25519 key in {}:2", known.display()),
            "Host key for talus.invalid has changed and you have requested strict checking.",
            "Host key verification failed.",
        ]
        .map(|line| format!("printf '%s\\n' '{line}' >&2\n"))
        .concat();
        let ssh = dir.join("talus-field-ssh");
        script(&ssh, &format!("{warning}exit 255\n"))?;
        git(&root, &["config", "core.sshCommand", path(&ssh)?])?;
        git(&root, &["config", "remote.origin.url", "ssh://git@talus.invalid/talus-field.git"])?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        self.open_clone(&root).await?;

        let title = "talus.invalid's identity has changed since you last connected";
        let removal = |notice: &Notice| {
            notice.actions.len() == 1
                && notice.actions[0].0.as_ref() == "Remove the saved key…"
                && matches!(&notice.actions[0].1, Action::Explain(explain::Action::ForgetHostKey { host, file })
                    if host == "talus.invalid" && *file == known)
        };
        self.action(Action::Sync(sync::Action::FetchAll))?;
        let warned = self.failed("the changed key explained", title).await?;
        if warned.detail != "Fetch failed. This can mean the server was reinstalled, or that something is intercepting the connection."
            || !removal(&warned)
            || !warned.output.as_deref().is_some_and(|output| output.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"))
        {
            return Err(format!("The changed host key said {warned:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;

        let lines = vec![
            format!(
                "talus.invalid no longer has the key saved for it in {}. This can mean the server was reinstalled, or that something is intercepting the connection.",
                known.display()
            ),
            "Remove it only if you know why the key changed, for example because the host announced it. The next connection asks you to confirm the new key.".to_owned(),
            format!("A copy of the file as it is now is kept as {}.old.", known.display()),
        ];
        let asked = |tab: &Tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Explain(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.danger && spec.title == "Remove the saved key for talus.invalid?" && spec.lines == lines
                })
        };
        self.action(Action::Notice(notices::Action::Take(warned.id, 0)))?;
        self.until("the removal asked", asked).await?;
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The removal prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("the removal cancelled", |tab| !tab.ui.prompt.open()).await?;
        if fs::read_to_string(&known)? != saved || dir.join("talus-field-known_hosts.old").exists() {
            return Err("Cancelling the removal changed the known_hosts file".into());
        }

        self.action(Action::Sync(sync::Action::FetchAll))?;
        let again = self.failed("the changed key explained again", title).await?;
        self.action(Action::Notice(notices::Action::Take(again.id, 0)))?;
        self.until("the removal asked again", asked).await?;
        self.key(CMD_RETURN)?;
        self.until("the key removed", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| {
                notice.tone == Tone::Success && notice.title == "Removed the saved key for talus.invalid"
            })
        })
        .await?;
        if fs::read_to_string(&known)? != format!("other.invalid {KEY_A}\n")
            || fs::read_to_string(dir.join("talus-field-known_hosts.old"))? != saved
        {
            return Err("The removal did not leave only other.invalid's key, with the old file kept".into());
        }

        self.action(Action::Sync(sync::Action::FetchAll))?;
        let stale = self.failed("the changed key explained once more", title).await?;
        self.action(Action::Notice(notices::Action::Take(stale.id, 0)))?;
        self.until("the removal asked once more", asked).await?;
        self.key(CMD_RETURN)?;
        let refused = self.failed("the removal refused", "The key was not removed").await?;
        if !refused.detail.contains("has no saved key for talus.invalid any more, so nothing was changed.") {
            return Err(format!("The stale removal said {refused:?}").into());
        }
        if fs::read_to_string(&known)? != format!("other.invalid {KEY_A}\n") {
            return Err("The refused removal changed the known_hosts file".into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;
        self.pass();
        Ok(())
    }
}

fn running(tab: &Tab) -> Option<&Notice> {
    let id = tab.ui.jobs.network.as_ref()?.notice?;
    tab.ui.notices.get(id).filter(|notice| notice.tone == Tone::Progress)
}

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "The sandbox path is not UTF-8".into())
}

pub(super) fn published(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let peer = init(dir, &format!("{name}-peer"))?;
    record(&peer, "trail.txt", TRAIL, "Start the trail")?;
    let bare = dir.join(format!("{name}.git"));
    git(dir, &["init", "-q", "--bare", "-b", "main", path(&bare)?])?;
    git(&peer, &["push", "-q", path(&bare)?, "main"])?;
    let root = dir.join(name);
    git(dir, &["clone", "-q", path(&bare)?, path(&root)?])?;
    for (key, value) in [
        ("user.name", "Cedar Contributor"),
        ("user.email", "cedar@example.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("commit.gpgSign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(&root, &["config", "--local", key, value])?;
    }
    Ok((root, bare, peer))
}

pub(super) fn script(path: &Path, body: &str) -> Result<()> {
    fs::write(path, format!("#!/bin/sh\n{body}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}


impl Replay<'_> {
    async fn sign_in_asked(&mut self, what: &str, title: &str) -> Result<()> {
        let wanted = title.to_owned();
        self.during(what, move |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Askpass(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == wanted)
                && tab.ui.prompt.field().is_some()
        })
        .await
    }

    async fn sign_in(&mut self, user: &str, password: &str, remember: bool) -> Result<()> {
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            let (Some(field), Some(second)) = (tab.ui.prompt.field().cloned(), tab.ui.prompt.second().cloned()) else {
                return false;
            };
            field.update(cx, |field, cx| field.replace_text_in_range(None, user, window, cx));
            second.update(cx, |field, cx| field.replace_text_in_range(None, password, window, cx));
            field.read(cx).text() == user && second.read(cx).text() == password
        })?;
        if !typed {
            return Err("The sign-in prompt did not take the username and password".into());
        }
        if remember {
            self.action(Action::Prompt(prompt::Action::Toggle))?;
            if !self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.ticked())? {
                return Err("Remember in Keychain did not tick".into());
            }
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.during("the sign-in prompt closed", |tab| !tab.ui.prompt.open()).await
    }

    async fn password_remembered(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0407");
        let (root, bare, peer) = published(dir, "Ridge-hut")?;
        let server = Server::start(dir, "cedar", "ridge-pass")?;
        git(&root, &["config", "remote.origin.url", &server.url("Ridge-hut.git")])?;
        record(&peer, "stove.txt", "Stove: lit\n", "Light the stove")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        self.open_clone(&root).await?;

        let host = server.host();
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.sign_in_asked("the sign-in asked", &format!("Sign in to {host}")).await?;
        let (fields, tick, note, waiting) = self.tab.read_with(self.cx, |tab, _| {
            let spec = prompt::shown(&tab.ui);
            let fields = spec.as_ref().map(|spec| {
                (spec.field.as_ref().map(|f| (f.label, f.masked)), spec.second.as_ref().map(|f| (f.label, f.masked)))
            });
            let tick = spec.as_ref().and_then(|spec| spec.tick.as_ref().map(|(label, ticked)| (label.to_string(), *ticked)));
            let note = spec.as_ref().is_some_and(|spec| spec.note.is_some());
            (fields, tick, note, running(tab).map(|notice| notice.detail.clone()))
        })?;
        if fields != Some((Some(("Username", false)), Some(("Password", true))))
            || tick != Some(("Remember in Keychain".to_owned(), false))
            || note
            || waiting.as_deref() != Some("Waiting for your answer…")
        {
            return Err(format!("The sign-in prompt had fields {fields:?}, tick {tick:?}, a note {note}, and the job said {waiting:?}").into());
        }
        self.sign_in("cedar", "ridge-pass", true).await?;
        self.until("the fetch signed in", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        let seen = server.seen();
        if seen.first() != Some(&None) || !seen.contains(&Some(("cedar".to_owned(), "ridge-pass".to_owned()))) {
            return Err(format!("The server saw {seen:?}").into());
        }
        let saved = crate::credentials::Saved::current()?;
        let entry = crate::credentials::Entry { protocol: "http".into(), host: host.clone(), username: "cedar".into() };
        if saved.find("http", &host, None)? != Some(("cedar".into(), "ridge-pass".into())) || !saved.list()?.contains(&entry) {
            return Err(format!("After the sign-in the saved passwords held {:?}", saved.list()?).into());
        }
        if crate::auth::secret::get(&format!("FastKraken: {host}"), "cedar")?.as_deref() != Some(b"ridge-pass".as_slice()) {
            return Err("The saved password is not FastKraken's item for the host".into());
        }
        self.no_secret("ridge-pass")?;

        record(&peer, "fire.txt", "Fire: banked\n", "Bank the fire")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let banked = oid(&peer, "HEAD")?;
        let shown = askpass::shown_so_far();
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.until("the fetch with the saved password", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully") && !tab.ui.prompt.open()
        })
        .await?;
        if askpass::shown_so_far() != shown {
            return Err("The fetch with a saved password asked for it again".into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{banked}\n"))?;

        self.action(Action::Run("app.forget-credentials"))?;
        let listed = entry.clone();
        self.until("the saved hosts in Preferences", move |tab| {
            tab.ui.preferences.open()
                && tab.ui.preferences.section() == crate::preferences::Section::Accounts
                && tab.ui.preferences.saved().is_some_and(|saved| saved.as_ref().is_ok_and(|entries| entries.contains(&listed)))
        })
        .await?;
        self.action(Action::Credentials(crate::credentials::Action::Forget))?;
        let wanted = entry.clone();
        self.until("the saved hosts listed", move |tab| match tab.ui.prompt.asked() {
            Some(prompt::Kind::Credentials(ask)) => ask.entries().contains(&wanted),
            _ => false,
        })
        .await?;
        let (index, danger) = self.tab.read_with(self.cx, |tab, _| {
            let index = match tab.ui.prompt.asked() {
                Some(prompt::Kind::Credentials(ask)) => ask.entries().iter().position(|listed| *listed == entry),
                _ => None,
            };
            (index, prompt::shown(&tab.ui).is_some_and(|spec| spec.danger))
        })?;
        let index = index.ok_or("The host is not listed")?;
        if !danger || self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("Forget saved credentials did not open as a danger prompt with Cancel focused".into());
        }
        let single = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).is_some_and(|spec| spec.choices.is_empty()))?;
        if single {
            self.key(CMD_RETURN)?;
        } else {
            self.action(Action::Prompt(prompt::Action::Choose(index)))?;
        }
        self.until("the password forgotten", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title.starts_with("Forgot "))
        })
        .await?;
        if saved.find("http", &host, None)?.is_some() || crate::auth::secret::get(&format!("FastKraken: {host}"), "cedar")?.is_some() {
            return Err("Forgetting the host left its password saved".into());
        }
        let gone = entry.clone();
        self.until("Preferences listing the host no more", move |tab| {
            tab.ui.preferences.saved().is_some_and(|saved| saved.as_ref().is_ok_and(|entries| !entries.contains(&gone)))
        })
        .await?;
        self.key("escape")?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.preferences.open())? {
            return Err("Esc did not close Preferences".into());
        }
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.sign_in_asked("the sign-in asked after forgetting", &format!("Sign in to {host}")).await?;
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        self.until("the fetch cancelled", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Cancelled")
        })
        .await?;
        self.pass();
        Ok(())
    }

    fn no_secret(&mut self, secret: &str) -> Result<()> {
        let notices = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.items().iter().map(|notice| format!("{} {:?}", notice.text(), notice.output)).collect::<Vec<_>>()
        })?;
        if notices.iter().any(|text| text.contains(secret)) {
            return Err(format!("A notice holds the password: {notices:?}").into());
        }
        for logged in activity::entries() {
            if format!("{} {} {}", logged.command_line(), logged.stdout, logged.stderr).contains(secret) {
                return Err(format!("The password reached the activity log: {logged:?}").into());
            }
        }
        Ok(())
    }

    async fn rejected_password(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0408");
        let (root, bare, peer) = published(dir, "Moraine-camp")?;
        let server = Server::start(dir, "cedar", "moraine-pass")?;
        git(&root, &["config", "remote.origin.url", &server.url("Moraine-camp.git")])?;
        record(&peer, "tent.txt", "Tent: pitched\n", "Pitch the tent")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        self.open_clone(&root).await?;
        let host = server.host();
        let saved = crate::credentials::Saved::current()?;
        let nothing_saved = |saved: &crate::credentials::Saved| -> Result<bool> {
            Ok(saved.find("http", &host, None)?.is_none() && !saved.list()?.iter().any(|entry| entry.host == host))
        };
        let title = "Sign-in to 127.0.0.1 failed";

        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.sign_in_asked("the sign-in asked", &format!("Sign in to {host}")).await?;
        self.sign_in("cedar", "wrong-pass", true).await?;
        let refused = self.failed("the wrong password refused", title).await?;
        let again = refused.actions.iter().position(|(label, action)| {
            label.as_ref() == "Try again" && matches!(action, Action::Sync(sync::Action::FetchAll))
        });
        if again.is_none() {
            return Err(format!("The refused sign-in offered {:?}", refused.actions).into());
        }
        if !server.seen().contains(&Some(("cedar".to_owned(), "wrong-pass".to_owned()))) || !nothing_saved(&saved)? {
            return Err(format!("After the refusal the server saw {:?} and the saved passwords held {:?}", server.seen(), saved.list()?).into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;
        self.action(Action::Notice(notices::Action::Dismiss(refused.id)))?;

        saved.put(&askpass::Credential { protocol: "http".into(), host: host.clone(), username: "cedar".into(), password: "stale-pass".into() })?;
        let shown = askpass::shown_so_far();
        self.action(Action::Sync(sync::Action::FetchAll))?;
        let stale = self.failed("the stale password refused", title).await?;
        if askpass::shown_so_far() != shown || !server.seen().contains(&Some(("cedar".to_owned(), "stale-pass".to_owned()))) {
            return Err(format!("With a saved password the fetch asked anyway, or did not use it: {:?}", server.seen()).into());
        }
        if !nothing_saved(&saved)? {
            return Err(format!("The refused saved password is still kept: {:?}", saved.list()?).into());
        }

        let index = stale.actions.iter().position(|(label, _)| label.as_ref() == "Try again").ok_or("No Try again")?;
        self.action(Action::Notice(notices::Action::Take(stale.id, index)))?;
        self.sign_in_asked("the sign-in asked again", &format!("Sign in to {host}")).await?;
        self.sign_in("cedar", "moraine-pass", false).await?;
        self.until("the fetch signed in", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        if !nothing_saved(&saved)? {
            return Err(format!("An unticked password was saved: {:?}", saved.list()?).into());
        }
        self.no_secret("moraine-pass")?;

        record(&peer, "stakes.txt", "Stakes: in\n", "Drive the stakes")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.sign_in_asked("the sign-in asked to be cancelled", &format!("Sign in to {host}")).await?;
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        self.until("the fetch cancelled", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Info && notice.title == "Cancelled" && notice.detail.is_empty())
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        self.pass();
        Ok(())
    }

    async fn background_never_asks(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0409");
        let (root, bare, peer) = published(dir, "Cirque-wall")?;
        let server = Server::start(dir, "cedar", "cirque-pass")?;
        git(&root, &["config", "remote.origin.url", &server.url("Cirque-wall.git")])?;
        record(&peer, "wall.txt", "Wall: sheer\n", "Survey the wall")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        self.open_clone(&root).await?;

        self.until("origin listed", |tab| !tab.ui.refs.remotes.is_empty()).await?;
        let shown = askpass::shown_so_far();
        self.action(Action::AutoFetch(crate::autofetch::Action::Tick))?;
        let opened = std::cell::Cell::new(false);
        self.during("the background fetch failed", |tab| {
            opened.set(opened.get() || tab.ui.prompt.open());
            tab.ui.jobs.idle()
                && tab.ui.autofetch.failed.as_ref().is_some_and(|failed| failed.title == "Sign-in to 127.0.0.1 failed")
        })
        .await?;
        if opened.get() || askpass::shown_so_far() != shown {
            return Err("A prompt opened for a background fetch".into());
        }
        let seen = server.seen();
        if seen.is_empty() || seen.iter().any(Option::is_some) {
            return Err(format!("The background fetch sent the server {seen:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;

        let socket = askpass::Broker::global().ok_or("The window is not listening for questions")?.path().to_owned();
        for token in [None, Some("0123456789abcdef0123456789abcdef")] {
            let mut helper = std::process::Command::new(std::env::current_exe()?);
            helper.arg(format!("Password for 'http://cedar@{}': ", server.host())).env(askpass::SOCKET, &socket);
            match token {
                Some(token) => helper.env(askpass::TOKEN, token),
                None => helper.env_remove(askpass::TOKEN),
            };
            let started = Instant::now();
            let output = helper.output()?;
            if output.status.code() != Some(1) || !output.stdout.is_empty() || started.elapsed() > Duration::from_secs(3) {
                return Err(format!("The askpass program with token {token:?} gave {:?} after {:?}", output, started.elapsed()).into());
            }
        }
        if askpass::shown_so_far() != shown {
            return Err("The askpass program without a job's token opened a prompt".into());
        }
        self.pass();
        Ok(())
    }
}

struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Option<(String, String)>>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

impl Server {
    fn start(root: &Path, user: &str, password: &str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (root, wanted) = (root.to_owned(), (user.to_owned(), password.to_owned()));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (root, wanted, seen) = (root.clone(), wanted.clone(), seen.clone());
                    std::thread::spawn(move || {
                        let _ = serve(stream, &root, &wanted, &seen);
                    });
                }
            })
        };
        Ok(Self { port, seen, stop, thread: Some(thread) })
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn url(&self, name: &str) -> String {
        format!("http://{}/{name}", self.host())
    }

    fn seen(&self) -> Vec<Option<(String, String)>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: TcpStream, root: &Path, wanted: &(String, String), seen: &Mutex<Vec<Option<(String, String)>>>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut parts = request.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else { return Ok(()) };
    let mut headers = std::collections::BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let mut body = Vec::new();
    if let Some(length) = headers.get("content-length").and_then(|n| n.parse::<usize>().ok()) {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else if headers.get("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    }
    let given = headers.get("authorization").and_then(|value| value.strip_prefix("Basic ")).and_then(basic);
    seen.lock().unwrap_or_else(|e| e.into_inner()).push(given.clone());
    let mut out = stream;
    if given.as_ref() != Some(wanted) {
        out.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"FastKraken replay\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let (path_info, query) = target.split_once('?').unwrap_or((target, ""));
    let mut backend = repository::bundled_command(root)?;
    backend
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", method)
        .env("PATH_INFO", path_info)
        .env("QUERY_STRING", query)
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", &wanted.0)
        .env("REMOTE_ADDR", "127.0.0.1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for (header, variable) in [("content-type", "CONTENT_TYPE"), ("git-protocol", "GIT_PROTOCOL"), ("content-encoding", "HTTP_CONTENT_ENCODING")] {
        if let Some(value) = headers.get(header) {
            backend.env(variable, value);
        }
    }
    let mut child = backend.spawn()?;
    let mut input = child.stdin.take().ok_or("No input for http-backend")?;
    let writer = std::thread::spawn(move || input.write_all(&body));
    let output = child.wait_with_output()?;
    let _ = writer.join();
    let split = output.stdout.windows(4).position(|w| w == b"\r\n\r\n").map(|at| (at, 4))
        .or_else(|| output.stdout.windows(2).position(|w| w == b"\n\n").map(|at| (at, 2)))
        .ok_or("http-backend gave no headers")?;
    let (head, rest) = (String::from_utf8_lossy(&output.stdout[..split.0]).into_owned(), &output.stdout[split.0 + split.1..]);
    let mut status = "200 OK".to_owned();
    let mut response = String::new();
    for line in head.lines() {
        match line.split_once(':') {
            Some((key, value)) if key.eq_ignore_ascii_case("status") => status = value.trim().to_owned(),
            Some(_) => {
                response.push_str(line.trim_end());
                response.push_str("\r\n");
            }
            None => {}
        }
    }
    out.write_all(format!("HTTP/1.1 {status}\r\n{response}Content-Length: {}\r\nConnection: close\r\n\r\n", rest.len()).as_bytes())?;
    out.write_all(rest)?;
    Ok(())
}

fn basic(encoded: &str) -> Option<(String, String)> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0;
    let mut decoded = Vec::new();
    for byte in encoded.trim().bytes().take_while(|b| *b != b'=') {
        bits = bits << 6 | ALPHABET.iter().position(|c| *c == byte)? as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_owned(), password.to_owned()))
}


impl Replay<'_> {
    async fn succeeded(&mut self, description: &str, title: &str, detail: &str) -> Result<Notice> {
        let (title, detail) = (title.to_owned(), detail.to_owned());
        self.until(description, move |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == title && notice.detail == detail)
        })
        .await?;
        let notice = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().cloned())?;
        notice.ok_or_else(|| "The notice went".into())
    }

    async fn edit_remote_address(&mut self, remote: &str, fetch: &str, push: &str, save: bool) -> Result<()> {
        self.choose(&Target::Remote(remote.to_owned()), "remote.edit-address")?;
        let title = format!("Edit the address of {remote}");
        self.until("the address prompt open", move |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Address(_))) && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == title)
        })
        .await?;
        let typed = self.tab.update(self.cx, |tab, _, cx| {
            let (Some(field), Some(second)) = (tab.ui.prompt.field().cloned(), tab.ui.prompt.second().cloned()) else {
                return false;
            };
            field.update(cx, |field, cx| field.set_text(fetch, cx));
            second.update(cx, |field, cx| field.set_text(push, cx));
            field.read(cx).text() == fetch && second.read(cx).text() == push
        })?;
        if !typed {
            return Err("The address prompt did not take the addresses".into());
        }
        if save {
            self.action(Action::Prompt(prompt::Action::Confirm))?;
            self.until("the address prompt closed", |tab| !tab.ui.prompt.open()).await?;
        }
        Ok(())
    }

    async fn fetch_one_remote(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0410");
        let (root, bare, peer) = published(dir, "Fork-junction")?;
        let upstream = dir.join("Fork-junction-upstream.git");
        git(dir, &["clone", "-q", "--bare", path(&bare)?, path(&upstream)?])?;
        git(&root, &["remote", "add", "upstream", path(&upstream)?])?;
        git(&root, &["fetch", "-q", "upstream"])?;
        record(&peer, "fork.txt", "Fork: left to the col\n", "Mark the fork")?;
        for remote in [&bare, &upstream] {
            git(&peer, &["push", "-q", path(remote)?, "main"])?;
        }
        let (incoming, origin, head) = (oid(&peer, "HEAD")?, oid(&root, "refs/remotes/origin/main")?, oid(&root, "HEAD")?);
        self.open_clone(&root).await?;
        self.until("both remotes listed with their addresses", |tab| {
            tab.ui.refs.remotes == ["origin", "upstream"] && tab.ui.refs.addresses.contains_key("upstream")
        })
        .await?;

        let target = Target::Remote("upstream".into());
        let rows = self.menu_rows(&target)?;
        let listed: Vec<(&str, &str)> = rows.iter().map(|(id, title, _)| (*id, title.as_str())).collect();
        if listed
            != [
                ("remote.fetch", "Fetch upstream"),
                ("remote.edit-address", "Edit address…"),
                ("remote.rename", "Rename remote…"),
                ("remote.remove", "Remove remote…"),
                ("remote.copy-address", "Copy address"),
            ]
            || rows.iter().any(|(_, _, disabled)| disabled.is_some())
        {
            return Err(format!("The menu of upstream listed {rows:?}").into());
        }
        self.choose(&target, "remote.fetch")?;
        let fetched = self.succeeded("the fetch of upstream", "Fetched Successfully", "From upstream: 1 branch updated.").await?;
        git_is(&root, &["rev-parse", "refs/remotes/upstream/main"], &format!("{incoming}\n"))?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{origin}\n"))?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        let entry = activity::find(fetched.log.unwrap_or_default()).ok_or("The fetch is not in the activity log")?;
        let named: Vec<&str> = entry.argv.iter().skip_while(|arg| *arg != "--").skip(1).map(String::as_str).collect();
        if !entry.argv.iter().any(|arg| arg == "fetch") || named != ["upstream"] || entry.exit != Some(0) {
            return Err(format!("The fetch ran {:?}", entry.argv).into());
        }
        self.pass();
        Ok(())
    }

    async fn edit_address(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0411");
        let (root, bare, peer) = published(dir, "Switchback-pass")?;
        let moved = dir.join("Switchback-pass-moved.git");
        let pushes = dir.join("Switchback-pass-push.git");
        git(dir, &["clone", "-q", "--bare", path(&bare)?, path(&moved)?])?;
        git(dir, &["clone", "-q", "--bare", path(&bare)?, path(&pushes)?])?;
        record(&peer, "switchback.txt", "Switchback: seven turns\n", "Cut the switchback")?;
        git(&peer, &["push", "-q", path(&moved)?, "main"])?;
        let cut = oid(&peer, "HEAD")?;
        self.open_clone(&root).await?;
        self.until("origin's address read", |tab| tab.ui.refs.addresses.contains_key("origin")).await?;

        self.edit_remote_address("origin", path(&moved)?, path(&pushes)?, false).await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            prompt::shown(&tab.ui).map(|spec| {
                (spec.confirm.to_string(), spec.field.map(|f| (f.label, f.initial)), spec.second.map(|f| (f.label, f.initial)))
            })
        })?;
        let wanted = (
            "Save address".to_owned(),
            Some(("Fetch address", path(&bare)?.to_owned())),
            Some(("Push address (optional)", String::new())),
        );
        if shown.as_ref() != Some(&wanted) {
            return Err(format!("The address prompt showed {shown:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the address prompt closed", |tab| !tab.ui.prompt.open()).await?;
        self.succeeded("the address changed", &format!("origin now fetches from {}", path(&moved)?), &format!("Pushes go to {}.", path(&pushes)?))
            .await?;
        git_is(&root, &["remote", "get-url", "origin"], &format!("{}\n", path(&moved)?))?;
        git_is(&root, &["remote", "get-url", "--push", "origin"], &format!("{}\n", path(&pushes)?))?;
        self.choose(&Target::Remote("origin".into()), "remote.fetch")?;
        self.succeeded("the fetch from the new address", "Fetched Successfully", "From origin: 1 branch updated.").await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{cut}\n"))?;

        self.until("the new addresses read", |tab| {
            tab.ui.refs.addresses.get("origin").is_some_and(|addresses| addresses.push.len() == 1)
        })
        .await?;
        self.edit_remote_address("origin", "git@github.com:cedar/elsewhere.git", "", false).await?;
        git(&root, &["remote", "set-url", "origin", path(&bare)?])?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let refused = self.failed("the stale address refused", "Address Not Changed").await?;
        if !refused.text().contains("origin's address changed since you opened it") {
            return Err(format!("The stale edit said {refused:?}").into());
        }
        git_is(&root, &["remote", "get-url", "origin"], &format!("{}\n", path(&bare)?))?;
        self.action(Action::Notice(notices::Action::Dismiss(refused.id)))?;

        let hosted = "git@github.com:cedar/switchback-pass.git";
        let wanted = path(&bare)?.to_owned();
        self.until("the address set outside read", move |tab| {
            tab.ui.refs.addresses.get("origin").and_then(|addresses| addresses.single()) == Some(wanted.as_str())
        })
        .await?;
        self.edit_remote_address("origin", hosted, "", true).await?;
        self.succeeded("the hosted address set", &format!("origin now uses {hosted}"), "Fetches, pulls and pushes go there.").await?;
        git_is(&root, &["remote", "get-url", "--push", "origin"], &format!("{hosted}\n"))?;
        if git(&root, &["config", "--get-all", "remote.origin.pushurl"]).is_ok() {
            return Err("A push address was left behind".into());
        }
        self.until("the hosted address read", |tab| {
            tab.ui.refs.addresses.get("origin").and_then(|addresses| addresses.single()) == Some(hosted)
        })
        .await?;
        let target = Target::Remote("origin".into());
        let open = self.menu_rows(&target)?.into_iter().find(|(id, _, _)| *id == "remote.open");
        if open.as_ref().map(|(_, title, disabled)| (title.as_str(), disabled.is_none())) != Some(("Open on github.com", true)) {
            return Err(format!("The menu's open row was {open:?}").into());
        }
        let before = handoff::opened().len();
        self.choose(&target, "remote.open")?;
        let opened = handoff::opened();
        if opened.len() != before + 1 || opened.last().map(String::as_str) != Some("https://github.com/cedar/switchback-pass") {
            return Err(format!("Open on github.com opened {opened:?}").into());
        }
        if address::web_address(hosted).as_deref() != Some("https://github.com/cedar/switchback-pass") {
            return Err("The page address is not the repository's".into());
        }
        let copy = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &target, cx).into_iter().find(|entry| entry.id == "remote.copy-address").and_then(|entry| entry.action)
        })?;
        let copied = self.copied(copy.ok_or("The menu has no Copy address")?, "Copied address").await?;
        if copied.as_deref() != Some(hosted) {
            return Err(format!("Copy address copied {copied:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn prune_on_fetch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0412");
        let (root, bare, peer) = published(dir, "Glacier-tongue")?;
        git(&peer, &["push", "-q", path(&bare)?, "main:refs/heads/old-route", "main:refs/heads/side-route"])?;
        git(&root, &["fetch", "-q", "origin"])?;
        git(&root, &["branch", "-q", "--track", "old-route", "origin/old-route"])?;
        let local = oid(&root, "refs/heads/old-route")?;
        self.open_clone(&root).await?;
        let tick = self.tab.read_with(self.cx, |tab, cx| {
            let entry = commands::find("repo.fetch-prune").and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx));
            (tab.ui.sync.prune, entry.map(|entry| (entry.title, entry.action.is_some())))
        })?;
        if tick != (true, Some((sync::PRUNE.to_owned(), true))) || sync::PRUNE != "Remove branches that were deleted on the remote" {
            return Err(format!("The pull menu's prune tick read {tick:?}").into());
        }

        git(&peer, &["push", "-q", path(&bare)?, ":refs/heads/old-route"])?;
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.succeeded("the pruning fetch", "Fetched Successfully", "1 branch removed.").await?;
        let refs = || git(&root, &["for-each-ref", "--format=%(refname)"]);
        let after = refs()?;
        if after.lines().any(|line| line == "refs/remotes/origin/old-route") || !after.lines().any(|line| line == "refs/remotes/origin/side-route") {
            return Err(format!("After the pruning fetch the refs were {after}").into());
        }
        git_is(&root, &["rev-parse", "refs/heads/old-route"], &format!("{local}\n"))?;

        self.action(Action::Sync(sync::Action::TogglePrune))?;
        self.until("pruning turned off", |tab| !tab.ui.sync.prune && tab.ui.notices.newest().is_some_and(|n| n.title == "Fetch keeps deleted remote branches"))
            .await?;
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        self.cx.update(|cx| crate::settings::flush(cx))?;
        let chosen: serde_json::Value = serde_json::from_slice(&fs::read(state.join("settings.json"))?)?;
        if chosen["prune_on_fetch"] != false {
            return Err(format!("settings.json holds {chosen}").into());
        }
        git(&peer, &["push", "-q", path(&bare)?, ":refs/heads/side-route"])?;
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.succeeded("the fetch that keeps", "Fetched Successfully", "Everything is up to date.").await?;
        git(&root, &["rev-parse", "--verify", "-q", "refs/remotes/origin/side-route"])?;

        self.action(Action::Sync(sync::Action::TogglePrune))?;
        self.until("pruning turned on", |tab| tab.ui.sync.prune).await?;
        self.action(Action::Sync(sync::Action::FetchAll))?;
        self.succeeded("the second pruning fetch", "Fetched Successfully", "1 branch removed.").await?;
        if refs()?.lines().any(|line| line == "refs/remotes/origin/side-route") {
            return Err("origin/side-route was not pruned".into());
        }
        git_is(&root, &["rev-parse", "refs/heads/old-route"], &format!("{local}\n"))?;
        self.pass();
        Ok(())
    }

    async fn username_in_address(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0413");
        let (root, bare, _) = published(dir, "Col-hut")?;
        let server = Server::start(dir, "cedar", "col-pass")?;
        let host = server.host();
        let main = git(dir, &["--git-dir", path(&bare)?, "rev-parse", "main"])?;
        self.open_clone(&root).await?;

        self.action(Action::Publish(crate::publish::Action::Add))?;
        self.until("the Add existing remote form", |tab| tab.ui.publish.open() && !tab.ui.publish.working).await?;
        let fill = |name: &str, url: String| {
            let name = name.to_owned();
            move |tab: &mut Tab, _: &mut gpui::Window, cx: &mut gpui::Context<Tab>| {
                let state = &tab.ui.publish;
                let (field, address) = (state.name.clone(), state.url.clone());
                field.update(cx, |field, cx| field.set_text(&name, cx));
                address.update(cx, |field, cx| field.set_text(&url, cx));
            }
        };
        let secret = format!("http://cedar:col-pass@{host}/Col-hut.git");
        self.tab.update(self.cx, fill("hut", secret))?;
        self.action(Action::Publish(crate::publish::Action::AddRemote))?;
        self.until("the address with a password refused", |tab| {
            tab.ui.publish.message == "Use a repository URL without embedded credentials, a query or a fragment."
        })
        .await?;
        git_is(&root, &["remote"], "origin\n")?;

        let named = format!("http://cedar@{host}/Col-hut.git");
        self.tab.update(self.cx, fill("hut", named.clone()))?;
        self.action(Action::Publish(crate::publish::Action::AddRemote))?;
        self.sign_in_asked("the sign-in for cedar", &format!("Sign in to {host}")).await?;
        git_is(&root, &["remote", "get-url", "hut"], &format!("{named}\n"))?;
        let asked = self.tab.update(self.cx, |tab, _, cx| {
            let spec = prompt::shown(&tab.ui)?;
            let fields = (spec.field.as_ref().map(|f| (f.label, f.masked)), spec.second.is_some());
            let field = tab.ui.prompt.field().cloned()?;
            field.update(cx, |field, cx| field.set_text("col-pass", cx));
            Some((spec.lines, fields))
        })?;
        let line = format!("Git needs the password for cedar at http://{host}.");
        if asked.as_ref() != Some(&(vec![line], (Some(("Password", true)), false))) {
            return Err(format!("The sign-in prompt asked {asked:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.during("the sign-in prompt closed", |tab| !tab.ui.prompt.open()).await?;
        self.succeeded("the new remote fetched", "Fetched Successfully", "From hut: 1 new branch.").await?;
        git_is(&root, &["rev-parse", "refs/remotes/hut/main"], &main)?;
        if !server.seen().contains(&Some(("cedar".to_owned(), "col-pass".to_owned()))) {
            return Err(format!("The server saw {:?}", server.seen()).into());
        }
        self.no_secret("col-pass")?;
        self.action(Action::Publish(crate::publish::Action::Escape))?;
        self.until("the publish form closed", |tab| !tab.ui.publish.open()).await?;
        self.pass();
        Ok(())
    }
}
