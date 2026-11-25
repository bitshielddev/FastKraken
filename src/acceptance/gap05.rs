use super::gap04::{published, script};
use super::*;
use crate::{
    activity, autofetch,
    clone::{self, Request},
    commands::{self, Target},
    identity::{self, Place, Scope},
    init, jobs,
    notice::{self, Notice, Tone},
    palette,
    preferences::{self, Section},
    start,
};
use gpui::Focusable;
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

impl Replay<'_> {
    pub(super) async fn gap05(&mut self, dir: &Path) -> Result<()> {
        let bare = self.clone_from_bare(dir).await?;
        self.cancel_clone(dir).await?;
        self.clone_into_existing(dir, &bare).await?;
        self.create_with_readme(dir).await?;
        self.open_plain_folder(dir).await?;
        self.deleted_recent(dir).await?;
        self.empty_repository(dir).await?;
        self.change_a_preference().await?;
        self.commit_without_identity(dir).await?;
        self.background_fetch_fails_quietly(dir).await?;
        self.background_fetch_waits_for_a_write(dir).await?;
        self.open_in_editor(dir).await?;
        self.two_tabs(dir).await?;
        self.second_launch(dir).await
    }

    fn notice_texts(&mut self) -> Result<Vec<String>> {
        Ok(self.tab.read_with(self.cx, |tab, _| tab.ui.notices.items().iter().map(Notice::text).collect())?)
    }

    fn fetch_tip(&mut self) -> Result<String> {
        Ok(self.tab.read_with(self.cx, |tab, _| crate::sync::fetch_tip(&tab.ui))?)
    }

    fn autofetch(&mut self) -> Result<(Option<autofetch::Failed>, Option<u64>, bool)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let state = &tab.ui.autofetch;
            let every = autofetch::interval(settings::settings(cx).fetch_minutes(), state.failures).map(|wait| wait.as_secs() / 60);
            (state.failed.clone(), every, state.due)
        })?)
    }

    async fn background_fetch_fails_quietly(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0510");
        let (root, bare, peer) = published(dir, "Alder-gap")?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        self.open_clone(&root).await?;
        self.until("origin listed", |tab| !tab.ui.refs.remotes.is_empty()).await?;
        let tip = self.fetch_tip()?;
        if tip != "Fetch all remotes (⌘L)\nNot fetched yet" {
            return Err(format!("Before any fetch the Fetch button said {tip:?}").into());
        }
        let moved = dir.join("Alder-gap.moved");
        fs::rename(&bare, &moved)?;
        let shown = self.notice_texts()?;
        let asked = crate::askpass::shown_so_far();
        let mark = activity::newest();

        self.action(Action::AutoFetch(autofetch::Action::Tick))?;
        self.until("the automatic fetch failed", |tab| tab.ui.autofetch.failed.is_some()).await?;
        let notices = self.notice_texts()?;
        if notices.iter().any(|text| !shown.contains(text)) || crate::askpass::shown_so_far() != asked {
            return Err(format!("The automatic fetch's failure interrupted: notices {notices:?}, before {shown:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;
        let (failed, every, due) = self.autofetch()?;
        let failed = failed.ok_or("The warning went")?;
        let wanted = "Automatic fetch failed: origin does not point at a repository you can reach. Check the address, and that this account has access.\nFetch to try again. Click to show the command in the activity log.";
        if failed.tip() != wanted || every != Some(10) || due {
            return Err(format!("After the failure the warning read {:?}, the interval {every:?} minutes, waiting {due}", failed.tip()).into());
        }
        let drawn = self.tab.update(self.cx, |tab, _, cx| autofetch::warning(&tab.ui, cx).is_some())?;
        if !drawn {
            return Err("The Remotes heading has no warning".into());
        }
        let entry = failed.entry.ok_or("The warning names no activity log entry")?;
        let logged = activity::find(entry).ok_or("The warning's entry is not in the activity log")?;
        if entry <= mark
            || logged.succeeded()
            || !logged.argv.iter().any(|arg| arg == "fetch")
            || !logged.argv.iter().any(|arg| arg == "--all")
            || !logged.stderr.contains("does not appear to be a git repository")
        {
            return Err(format!("The warning's entry is {logged:?}").into());
        }
        self.action(Action::Activity(activity::Action::Show(entry)))?;
        self.until("the activity log open at the failed fetch", |tab| {
            tab.ui.activity.open() && tab.ui.activity.selected().is_some_and(|selected| selected.id == entry)
        })
        .await?;
        self.action(Action::Activity(activity::Action::Close))?;
        self.until("the activity log closed", |tab| !tab.ui.activity.open()).await?;

        fs::rename(&moved, &bare)?;
        record(&peer, "gap.txt", "Gap: narrow\n", "Mark the gap")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        self.key("cmd-l")?;
        self.until("the fetch by hand", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        let (failed, every, _) = self.autofetch()?;
        if failed.is_some() || every != Some(5) {
            return Err(format!("After the fetch by hand the warning is {failed:?} and the interval {every:?} minutes").into());
        }
        let tip = self.fetch_tip()?;
        if tip != "Fetch all remotes (⌘L)\nLast fetched just now" {
            return Err(format!("After the fetch the Fetch button said {tip:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn background_fetch_waits_for_a_write(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0511");
        let (root, bare, peer) = published(dir, "Aspen-ford")?;
        let before = oid(&root, "refs/remotes/origin/main")?;
        record(&peer, "river.txt", "River: wide\n", "Ford the river")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let incoming = oid(&peer, "HEAD")?;
        let (pid, release) = (dir.join("aspen-ford.pid"), dir.join("aspen-ford.release"));
        let hooks = dir.join("aspen-ford-hooks");
        fs::create_dir(&hooks)?;
        script(
            &hooks.join("pre-commit"),
            &format!(
                "echo $$ > '{pid}'\nwhile [ ! -e '{release}' ]; do [ -e '{pid}' ] || exit 1; /bin/sleep 0.05; done\nexit 0\n",
                pid = pid.display(),
                release = release.display()
            ),
        )?;
        git(&root, &["config", "core.hooksPath", path(&hooks)?])?;
        self.open_clone(&root).await?;
        self.until("origin listed", |tab| !tab.ui.refs.remotes.is_empty()).await?;
        fs::write(root.join("ford.txt"), "Ford: knee deep\n")?;
        self.until("ford.txt listed", |tab| files(tab, &[], &["ford.txt"])).await?;
        self.action(Action::StageAll)?;
        self.until("ford.txt staged", |tab| files(tab, &["ford.txt"], &[])).await?;
        let mark = activity::newest();
        self.type_commit("Note the ford")?;
        self.during("the commit's hook waiting", |tab| tab.ui.busy() && pid.exists()).await?;

        self.action(Action::AutoFetch(autofetch::Action::Tick))?;
        self.cx.background_executor().timer(Duration::from_millis(400)).await;
        let (networking, due) = self.tab.read_with(self.cx, |tab, _| (tab.ui.jobs.network.is_some(), tab.ui.autofetch.due))?;
        let fetches = |since: u64| {
            activity::entries().into_iter().filter(|entry| entry.id > since && entry.argv.iter().any(|arg| arg == "fetch")).collect::<Vec<_>>()
        };
        if networking || !due || !fetches(mark).is_empty() {
            return Err(format!("During the commit a fetch ran: network {networking}, waiting {due}, {:?}", fetches(mark)).into());
        }
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{before}\n"))?;

        fs::write(&release, "")?;
        self.until("the commit and then the fetch", |tab| {
            !tab.ui.autofetch.due && tab.ui.history.iter().any(|row| row.subject == "Note the ford")
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%s"], "Note the ford\n")?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{incoming}\n"))?;
        let notices = self.notice_texts()?;
        if notices.iter().any(|text| text.starts_with("Fetched")) || !notices.iter().any(|text| text.starts_with("Changes committed")) {
            return Err(format!("After the commit the notices are {notices:?}").into());
        }
        let commit = activity::entries()
            .into_iter()
            .find(|entry| entry.id > mark && entry.argv.iter().any(|arg| arg == "commit"))
            .ok_or("The commit is not in the activity log")?;
        let fetched = fetches(mark);
        let [fetch] = fetched.as_slice() else {
            return Err(format!("After the commit the activity log holds the fetches {fetched:?}").into());
        };
        if !fetch.succeeded() || fetch.at < commit.at + commit.duration {
            return Err(format!("The fetch {fetch:?} did not wait for the commit {commit:?}").into());
        }

        let (gate, answer) = (dir.join("aspen-ford-upload-pack"), dir.join("aspen-ford.answer"));
        let served = dir.join("aspen-ford-served.pid");
        script(
            &gate,
            &format!(
                "echo $$ > '{served}'\nwhile [ ! -e '{answer}' ]; do [ -e '{served}' ] || exit 1; /bin/sleep 0.05; done\nexec git upload-pack \"$@\"\n",
                served = served.display(),
                answer = answer.display()
            ),
        )?;
        git(&root, &["config", "remote.origin.uploadpack", path(&gate)?])?;
        record(&peer, "bank.txt", "Bank: muddy\n", "Climb the far bank")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let far = oid(&peer, "HEAD")?;
        let mark = activity::newest();
        self.action(Action::AutoFetch(autofetch::Action::Tick))?;
        self.during("the automatic fetch waiting on origin", |tab| tab.ui.jobs.background().is_some() && served.exists()).await?;
        let (disabled, tip) = self.tab.read_with(self.cx, |tab, cx| {
            let entry = commands::find("repo.fetch-all").and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx));
            (entry.map(|entry| entry.disabled), crate::sync::fetch_tip(&tab.ui))
        })?;
        if disabled != Some(None) || tip != "Fetch all remotes (⌘L)\nFetching in the background…" {
            return Err(format!("While the automatic fetch runs, Fetch all remotes is {disabled:?} with {tip:?}").into());
        }
        let shown = self.notice_texts()?;
        if shown.iter().any(|text| text.contains("Fetching")) {
            return Err(format!("The automatic fetch shows a notice: {shown:?}").into());
        }
        fs::remove_file(&served)?;
        self.key("cmd-l")?;
        self.during("my fetch in its place", |tab| {
            tab.ui.jobs.network.as_ref().is_some_and(|job| job.spec.interactive && job.notice.is_some()) && served.exists()
        })
        .await?;
        let cancelled = activity::entries()
            .into_iter()
            .find(|entry| entry.id > mark && entry.argv.iter().any(|arg| arg == "fetch"))
            .ok_or("The automatic fetch is not in the activity log")?;
        if cancelled.succeeded() {
            return Err(format!("The automatic fetch was not cancelled: {cancelled:?}").into());
        }
        fs::write(&answer, "")?;
        self.until("my fetch done", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Fetched Successfully")
        })
        .await?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main"], &format!("{far}\n"))?;
        let (failed, _, due) = self.autofetch()?;
        if failed.is_some() || due {
            return Err(format!("After my fetch the warning is {failed:?}, waiting {due}").into());
        }
        self.pass();
        Ok(())
    }

    fn start_rows(&mut self) -> Result<Vec<(String, bool)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            start::rows(&tab.ui, cx).into_iter().map(|row| (row.name, row.missing)).collect()
        })?)
    }

    fn recent_names(&mut self) -> Result<Vec<String>> {
        Ok(self.cx.update(|cx| {
            settings::memory(cx).map_or_else(Vec::new, |memory| memory.recents.iter().map(|recent| recent.name.clone()).collect())
        })?)
    }

    fn cloning(&mut self) -> Result<(Request, bool)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let request = tab.ui.cloning.request(cx);
            let ready = request.problem().is_none();
            (request, ready)
        })?)
    }

    async fn until_dialog(&mut self, description: &str, predicate: impl Fn(&Request, bool) -> bool) -> Result<()> {
        let start = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            let (request, ready) = self.cloning()?;
            if predicate(&request, ready) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description} ({request:?}, ready={ready})").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    fn type_address(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.cloning.address.update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx))
        })?;
        Ok(())
    }

    fn fetch_all(&mut self) -> Result<Option<Option<String>>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::find("repo.fetch-all").and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx)).map(|entry| entry.disabled)
        })?)
    }

    async fn clone_from_bare(&mut self, dir: &Path) -> Result<PathBuf> {
        self.start("GAP0501");
        let peer = init(dir, "Juniper-trail-peer")?;
        record(&peer, "trail.txt", TRAIL, "Start the trail")?;
        record(&peer, "camp.txt", "Camp at the ridge\n", "Mark the camp")?;
        let bare = dir.join("Juniper-trail.git");
        git(dir, &["init", "-q", "--bare", "-b", "main", path(&bare)?])?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let clones = dir.join("clones");
        fs::create_dir(&clones)?;
        self.until("the start screen", |tab| tab.repo.is_none() && !tab.ui.cloning.open()).await?;

        self.key("cmd-n")?;
        self.until("the clone dialog", |tab| tab.ui.cloning.open()).await?;
        let (opened, _) = self.cloning()?;
        if !opened.address.is_empty() || !opened.name.is_empty() {
            return Err(format!("The dialog opened filled in: {opened:?}").into());
        }
        self.type_address(path(&bare)?)?;
        self.until_dialog("the folder name from the address", |request, _| request.name == "Juniper-trail").await?;
        self.action(Action::Clone(clone::Action::SetParent(clones.clone())))?;
        let destination = clones.join("Juniper-trail");
        let wanted = destination.clone();
        self.until_dialog("the full path in clones", move |request, ready| request.destination() == wanted && ready).await?;
        let parent = self.tab.read_with(self.cx, |tab, _| tab.ui.cloning.parent().to_owned())?;
        if parent != clones {
            return Err(format!("The dialog clones into {}", parent.display()).into());
        }

        self.action(Action::Clone(clone::Action::Submit))?;
        self.until("Juniper-trail open with both commits", |tab| {
            tab.ui.repo == "Juniper-trail"
                && tab.ui.branch == "main"
                && ["Start the trail", "Mark the camp"].iter().all(|subject| tab.ui.history.iter().any(|row| row.subject == *subject))
                && tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Cloned Juniper-trail")
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.cloning.open())? {
            return Err("The clone dialog is still open".into());
        }
        git_is(&destination, &["rev-parse", "main"], &git(&bare, &["rev-parse", "main"])?)?;
        git_is(&destination, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        git_is(&destination, &["log", "--format=%s"], "Mark the camp\nStart the trail\n")?;
        git_is(&destination, &["status", "--porcelain=v1"], "")?;
        git_is(&destination, &["remote", "get-url", "origin"], &format!("{}\n", path(&bare)?))?;
        let root = fs::canonicalize(&destination)?;
        let first = self.cx.update(|cx| {
            let memory = settings::memory(cx);
            let first = memory.and_then(|memory| memory.recents.first().map(|recent| (recent.path(), recent.name.clone())));
            (first, memory.and_then(|memory| memory.clone_parent()))
        })?;
        match first {
            (Some((recent, name)), Some(remembered)) if recent == root && name == "Juniper-trail" && remembered == clones => {}
            other => return Err(format!("Recents and the clone folder are {other:?}").into()),
        }
        self.action(Action::Run("repo.clone"))?;
        let wanted = clones.clone();
        self.until("the dialog in clones", move |tab| tab.ui.cloning.open() && tab.ui.cloning.parent() == wanted).await?;
        self.action(Action::Clone(clone::Action::Close))?;
        self.until("the dialog closed", |tab| !tab.ui.cloning.open()).await?;
        self.pass();
        Ok(bare)
    }

    async fn cancel_clone(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0502");
        let server = Silent::start()?;
        let clones = dir.join("clones");
        let partial = clones.join("Hollow-creek");
        self.action(Action::Run("repo.clone"))?;
        self.until("the clone dialog", |tab| tab.ui.cloning.open()).await?;
        self.type_address(&format!("http://127.0.0.1:{}/Hollow-creek.git", server.port))?;
        let wanted = partial.clone();
        self.until_dialog("Hollow-creek in clones", move |request, ready| request.destination() == wanted && ready).await?;
        self.action(Action::Clone(clone::Action::Submit))?;
        let made = partial.join(".git");
        self.during("the clone offering Cancel", move |tab| {
            made.exists()
                && tab.ui.jobs.network.as_ref().is_some_and(|job| job.spec.kind == jobs::Kind::Clone)
                && running(tab).is_some_and(|notice| notice.job.is_some())
        })
        .await?;
        let refused = self.fetch_all()?;
        if refused != Some(Some("A clone is already running".to_owned())) {
            return Err(format!("While the clone runs, Fetch is {refused:?}").into());
        }
        let job = self
            .tab
            .read_with(self.cx, |tab, _| running(tab).and_then(|notice| notice.job))?
            .ok_or("The clone's notice has no Cancel button")?;
        let asked = Instant::now();
        self.action(Action::Jobs(jobs::Action::Cancel(job)))?;
        self.until("the clone cancelled", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| {
                notice.tone == Tone::Info && notice.title == "Clone cancelled" && notice.detail == "Nothing it cloned was kept."
            })
        })
        .await?;
        let took = asked.elapsed();
        if took > Duration::from_secs(3) {
            return Err(format!("Cancelling the clone took {took:?}").into());
        }
        if partial.exists() {
            return Err(format!("{} is still there after the cancel", partial.display()).into());
        }
        if self.tab.read_with(self.cx, |tab, _| tab.ui.repo.clone())? != "Juniper-trail" {
            return Err("Juniper-trail is no longer open".into());
        }
        let fetch = self.fetch_all()?;
        if fetch != Some(None) {
            return Err(format!("After the cancel, Fetch is {fetch:?}").into());
        }
        drop(server);
        self.pass();
        Ok(())
    }

    async fn clone_into_existing(&mut self, dir: &Path, bare: &Path) -> Result<()> {
        self.start("GAP0503");
        let clones = dir.join("clones");
        let taken = clones.join("Juniper-trail");
        fs::write(taken.join("notes.txt"), NOTE)?;
        let head = git(&taken, &["rev-parse", "HEAD"])?;
        let ran = clones_into("Juniper-trail");
        if ran != 1 {
            return Err(format!("Git cloned into Juniper-trail {ran} times before").into());
        }
        self.action(Action::Run("repo.clone"))?;
        self.until("the clone dialog", |tab| tab.ui.cloning.open()).await?;
        self.type_address(path(bare)?)?;
        let refusal = format!("{} already exists and is not empty.", taken.display());
        let wanted = refusal.clone();
        self.until_dialog("the folder refused", move |request, ready| request.problem().as_deref() == Some(wanted.as_str()) && !ready).await?;
        self.tab.update(self.cx, |tab, window, cx| window.focus(&tab.ui.cloning.name.focus_handle(cx)))?;
        self.key("enter")?;
        self.until("the dialog still open", |tab| tab.ui.cloning.open() && tab.ui.jobs.network.is_none()).await?;
        if clones_into("Juniper-trail") != ran {
            return Err("Git ran to clone into the folder that exists".into());
        }
        git_is(&taken, &["rev-parse", "HEAD"], &head)?;
        if fs::read_to_string(taken.join("notes.txt"))? != NOTE {
            return Err("notes.txt changed".into());
        }
        self.tab.update(self.cx, |tab, _, cx| tab.ui.cloning.name.update(cx, |field, cx| field.set_text("Juniper-trail-copy", cx)))?;
        let copy = clones.join("Juniper-trail-copy");
        let wanted = copy.clone();
        self.until_dialog("the other name accepted", move |request, ready| request.destination() == wanted && ready).await?;
        self.key("escape")?;
        self.until("the dialog closed", |tab| !tab.ui.cloning.open() && tab.ui.jobs.network.is_none()).await?;
        if copy.exists() || clones_into("Juniper-trail-copy") != 0 {
            return Err("Closing the dialog cloned".into());
        }
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    fn creating(&mut self) -> Result<(init::Request, bool)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let request = tab.ui.init.request(cx);
            let ready = request.problem().is_none();
            (request, ready)
        })?)
    }

    async fn until_creating(&mut self, description: &str, predicate: impl Fn(&init::Request, bool) -> bool) -> Result<()> {
        let start = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            let (request, ready) = self.creating()?;
            if predicate(&request, ready) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description} ({request:?}, ready={ready})").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    fn first_recent(&mut self) -> Result<Option<PathBuf>> {
        Ok(self.cx.update(|cx| settings::memory(cx).and_then(|memory| memory.recents.first().map(|recent| recent.path())))?)
    }

    async fn create_with_readme(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0504");
        let projects = dir.join("projects");
        fs::create_dir(&projects)?;
        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.default_branch = "trunk".into()))?;
        let global = global_config(dir);
        for (key, value) in [("user.name", "Aspen Walker"), ("user.email", "aspen@example.invalid")] {
            git(dir, &["config", "--file", path(&global)?, key, value])?;
        }
        self.until("Juniper-trail open", |tab| tab.ui.repo == "Juniper-trail" && !tab.ui.init.open()).await?;

        self.key("cmd-i")?;
        self.until("the New Repository dialog", |tab| tab.ui.init.open()).await?;
        let (opened, ready) = self.creating()?;
        if opened.mode != init::Mode::New || opened.branch != "trunk" || !opened.readme || !opened.name.is_empty() || ready {
            return Err(format!("The dialog opened as {opened:?}, ready={ready}").into());
        }
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.init.name.update(cx, |field, cx| field.replace_text_in_range(None, "Aspen-ridge", window, cx))
        })?;
        self.action(Action::Init(init::Action::SetParent(projects.clone())))?;
        let destination = projects.join("Aspen-ridge");
        let wanted = destination.clone();
        self.until_creating("the full path in projects", move |request, ready| request.destination() == wanted && ready).await?;
        self.tab.update(self.cx, |tab, window, cx| window.focus(&tab.ui.init.name.focus_handle(cx)))?;
        self.key("enter")?;
        self.until("Aspen-ridge open with its first commit", |tab| {
            tab.ui.repo == "Aspen-ridge"
                && tab.ui.branch == "trunk"
                && tab.ui.history.iter().any(|row| row.subject == init::FIRST_COMMIT)
                && tab.ui.notices.newest().is_some_and(|notice| {
                    notice.tone == Tone::Success && notice.title == "Created Aspen-ridge" && notice.detail == "On trunk, with README.md as its first commit."
                })
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.init.open())? {
            return Err("The New Repository dialog is still open".into());
        }
        git_is(&destination, &["symbolic-ref", "HEAD"], "refs/heads/trunk
")?;
        git_is(&destination, &["rev-list", "--count", "HEAD"], "1
")?;
        git_is(&destination, &["for-each-ref", "--format=%(refname)"], "refs/heads/trunk
")?;
        git_is(&destination, &["log", "--format=%s|%an <%ae>"], "Initial commit|Aspen Walker <aspen@example.invalid>
")?;
        git_is(&destination, &["ls-tree", "-r", "--name-only", "HEAD"], "README.md
")?;
        git_is(&destination, &["show", "HEAD:README.md"], "# Aspen-ridge
")?;
        git_is(&destination, &["status", "--porcelain=v1"], "")?;
        let root = fs::canonicalize(&destination)?;
        if self.first_recent()? != Some(root.clone()) {
            return Err(format!("{} is not the first recent repository", root.display()).into());
        }
        fs::write(&global, "")?;
        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.default_branch = "main".into()))?;
        self.pass();
        Ok(())
    }

    async fn open_plain_folder(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0505");
        let sketches = dir.join("sketches");
        fs::create_dir(&sketches)?;
        fs::write(sketches.join("ideas.txt"), NOTE)?;
        let open = fs::canonicalize(dir.join("projects/Aspen-ridge"))?;
        let recents = self.cx.update(|cx| settings::memory(cx).map(|memory| memory.recents.clone()))?;

        self.open(&sketches)?;
        let title = "sketches is not a Git repository";
        self.until("the folder refused", move |tab| tab.ui.notices.items().iter().any(|notice| notice.title == title)).await?;
        let (offers, shown, root, history) = self.tab.read_with(self.cx, |tab, _| {
            let notice = tab.ui.notices.items().iter().find(|notice| notice.title == title).cloned();
            let offers: Vec<String> = notice.iter().flat_map(|notice| notice.actions.iter().map(|(label, _)| label.to_string())).collect();
            let history = tab.ui.history.iter().any(|row| row.subject == init::FIRST_COMMIT);
            (offers, tab.ui.repo.clone(), tab.repo.as_ref().map(|repo| repo.root.clone()), history)
        })?;
        if offers != ["Create one here", "Choose another…"] {
            return Err(format!("The notice offers {offers:?}").into());
        }
        if shown != "Aspen-ridge" || root.as_ref() != Some(&open) || !history {
            return Err(format!("After the refusal {shown:?} is open at {root:?}, graph loaded: {history}").into());
        }
        git_is(&open, &["rev-list", "--count", "HEAD"], "1
")?;
        if self.cx.update(|cx| settings::memory(cx).map(|memory| memory.recents.clone()))? != recents {
            return Err("The folder that did not open changed the recent repositories".into());
        }
        if sketches.join(".git").exists() {
            return Err("Refusing the folder made a repository in it".into());
        }

        let offered = self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.notices.items().iter().find(|notice| notice.title == title).map(|notice| notice.id))?
            .ok_or("The notice went away")?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the dialog for sketches", |tab| tab.ui.init.open()).await?;
        let (request, ready) = self.creating()?;
        if request.mode != init::Mode::Existing || request.folder != sketches || request.readme || request.branch != "main" || !ready {
            return Err(format!("Create one here opened {request:?}, ready={ready}").into());
        }
        self.action(Action::Init(init::Action::Submit))?;
        self.until("sketches open as a repository", |tab| {
            tab.ui.repo == "sketches"
                && tab.ui.branch == "main"
                && tab.ui.files.iter().any(|file| file.path == Path::new("ideas.txt"))
                && tab.ui.notices.newest().is_some_and(|notice| notice.title == "Created sketches" && notice.detail == "On main, with no commits yet.")
        })
        .await?;
        let root = fs::canonicalize(&sketches)?;
        git_is(&sketches, &["rev-parse", "--show-toplevel"], &format!("{}
", path(&root)?))?;
        git_is(&sketches, &["symbolic-ref", "HEAD"], "refs/heads/main
")?;
        git_is(&sketches, &["for-each-ref"], "")?;
        git_is(&sketches, &["status", "--porcelain=v1"], "?? ideas.txt
")?;
        if fs::read_to_string(sketches.join("ideas.txt"))? != NOTE {
            return Err("ideas.txt changed".into());
        }
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    async fn deleted_recent(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0506");
        let birch = init(dir, "Birch-hollow")?;
        record(&birch, "trail.txt", TRAIL, "Start the hollow trail")?;
        self.until("sketches open", |tab| tab.ui.repo == "sketches").await?;
        self.open(&birch)?;
        self.until("Birch-hollow open", |tab| tab.ui.repo == "Birch-hollow" && tab.ui.history.len() == 1).await?;
        let earlier = ["sketches", "Aspen-ridge", "Juniper-trail"];
        if self.recent_names()? != ["Birch-hollow", "sketches", "Aspen-ridge", "Juniper-trail"] {
            return Err(format!("The recent repositories are {:?}", self.recent_names()?).into());
        }

        self.action(Action::Menu(Some(view::Menu::Repo)))?;
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            commands::entries_of(&tab.ui, &view::Menu::Repo, cx).into_iter().map(|entry| (entry.id, entry.title)).collect::<Vec<_>>()
        })?;
        let listed: Vec<_> = rows.iter().map(|(id, title)| (*id, title.as_str())).collect();
        let wanted = [
            ("repo.open", "Open repository…"),
            ("repo.recent", "sketches"),
            ("repo.recent", "Aspen-ridge"),
            ("repo.recent", "Juniper-trail"),
            ("repo.close", "Close repository"),
        ];
        if listed.len() < wanted.len() || listed[..wanted.len()] != wanted {
            return Err(format!("The repository menu lists {listed:?}").into());
        }
        self.action(Action::Dismiss)?;

        self.key("cmd-shift-o")?;
        self.until("the palette of recent repositories", |tab| tab.ui.palette.open() && tab.ui.palette.mode() == palette::Mode::Recent).await?;
        let (rows, at) = self.palette_rows()?;
        let titles: Vec<_> = rows.iter().map(|row| (row.0, row.1.as_str())).collect();
        if titles != [("repo.recent", "Open sketches"), ("repo.recent", "Open Aspen-ridge"), ("repo.recent", "Open Juniper-trail")] || at != Some(0) {
            return Err(format!("Cmd-Shift-O lists {titles:?}, at {at:?}").into());
        }
        self.type_keys("asp")?;
        let (rows, _) = self.palette_rows()?;
        if rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>() != ["Open Aspen-ridge"] {
            return Err(format!("\"asp\" lists {rows:?}").into());
        }
        self.key("cmd-shift-o")?;
        self.until("the palette closed", |tab| !tab.ui.palette.open()).await?;

        self.action(Action::Run("repo.close"))?;
        self.until("the start screen", |tab| tab.repo.is_none() && tab.ui.repo.is_empty()).await?;
        let last = self.cx.update(|cx| settings::memory(cx).and_then(|memory| memory.last_repository()))?;
        if last.is_some() {
            return Err(format!("After Close the next launch would open {last:?}").into());
        }
        let typing = self.tab.update(self.cx, |tab, window, cx| tab.ui.start.filter.focus_handle(cx).is_focused(window))?;
        if !typing {
            return Err("The start screen did not put the cursor in its filter".into());
        }

        fs::remove_dir_all(&birch)?;
        self.until("the start screen drawn", |tab| tab.repo.is_none()).await?;
        let rows = self.start_rows()?;
        let mut wanted = vec![("Birch-hollow".to_owned(), true)];
        wanted.extend(earlier.iter().map(|name| (name.to_string(), false)));
        if rows != wanted {
            return Err(format!("The start screen lists {rows:?}").into());
        }

        self.type_keys("birch")?;
        if self.start_rows()? != [("Birch-hollow".to_owned(), true)] {
            return Err(format!("\"birch\" lists {:?}", self.start_rows()?).into());
        }
        self.key("enter")?;
        let title = "Birch-hollow no longer exists";
        self.until("the folder said to be gone", move |tab| tab.ui.notices.items().iter().any(|notice| notice.title == title)).await?;
        let (offers, id, open) = self.tab.read_with(self.cx, |tab, _| {
            let notice = tab.ui.notices.items().iter().find(|notice| notice.title == title);
            let offers: Vec<String> = notice.iter().flat_map(|notice| notice.actions.iter().map(|(label, _)| label.to_string())).collect();
            (offers, notice.map(|notice| notice.id), tab.repo.is_some())
        })?;
        if offers != ["Remove from recents", "Choose another…"] || open {
            return Err(format!("The notice offers {offers:?}, a repository open: {open}").into());
        }
        self.until("the start screen drawn again", |tab| tab.repo.is_none()).await?;
        let typing = self.tab.update(self.cx, |tab, window, cx| tab.ui.start.filter.focus_handle(cx).is_focused(window))?;
        if !typing {
            return Err("After the folder was said to be gone the filter no longer had the cursor".into());
        }
        self.action(Action::Notice(notice::Action::Take(id.ok_or("The notice went away")?, 0)))?;
        self.until("Birch-hollow taken off the list", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Removed Birch-hollow from recent repositories")
        })
        .await?;
        if self.recent_names()? != earlier {
            return Err(format!("After removal the recent repositories are {:?}", self.recent_names()?).into());
        }
        self.key("escape")?;
        let rows = self.start_rows()?;
        if rows != earlier.iter().map(|name| (name.to_string(), false)).collect::<Vec<_>>() {
            return Err(format!("After Esc the start screen lists {rows:?}").into());
        }

        self.action(Action::Start(start::Action::Clear))?;
        self.until("the list cleared", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "Cleared the recent repositories")
        })
        .await?;
        if !self.recent_names()?.is_empty() || !self.start_rows()?.is_empty() {
            return Err("Clear left recent repositories".into());
        }
        let undo = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|notice| notice.id))?.ok_or("No notice")?;
        self.action(Action::Notice(notice::Action::Take(undo, 0)))?;
        if self.recent_names()? != earlier {
            return Err(format!("Undo put back {:?}", self.recent_names()?).into());
        }
        if birch.exists() {
            return Err("Something put Birch-hollow back on disk".into());
        }
        self.pass();
        Ok(())
    }

    async fn empty_repository(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0507");
        let sketches = dir.join("sketches");
        git_is(&sketches, &["rev-list", "--all", "--count"], "0\n")?;
        if self.start_rows()?.first().map(|row| row.0.as_str()) != Some("sketches") {
            return Err(format!("The start screen lists {:?}", self.start_rows()?).into());
        }
        self.key("enter")?;
        self.until("sketches open with the working tree selected", |tab| {
            tab.ui.repo == "sketches"
                && tab.ui.branch == "main"
                && tab.ui.selection == Some(Selection::Changes)
                && files(tab, &[], &["ideas.txt"])
        })
        .await?;
        let (empty, working, panel) = self.tab.read_with(self.cx, |tab, _| {
            (view::unborn(&tab.ui), tab.ui.working(), !tab.ui.panel_hidden && tab.ui.history.is_empty())
        })?;
        if !empty || !working || !panel {
            return Err(format!("No-commits message {empty}, working tree selected {working}, panel {panel}").into());
        }
        let (title, detail) = view::NO_COMMITS;
        if (title, detail) != ("No commits yet.", "Stage your files and make the first commit.") {
            return Err(format!("The graph says {title:?} {detail:?}").into());
        }

        self.key("cmd-shift-m")?;
        let typing = self.tab.update(self.cx, |tab, window, cx| tab.ui.input.focus_handle(cx).is_focused(window))?;
        if !typing {
            return Err("Cmd-Shift-M did not reach the commit summary".into());
        }
        let global = global_config(dir);
        for (key, value) in [("user.name", "Aspen Walker"), ("user.email", "aspen@example.invalid")] {
            git(dir, &["config", "--file", path(&global)?, key, value])?;
        }
        self.key("cmd-shift-s")?;
        self.until("ideas.txt staged", |tab| files(tab, &["ideas.txt"], &[])).await?;
        self.type_commit("Sketch the first ideas")?;
        self.until("the first commit in the graph", |tab| {
            tab.ui.history.len() == 1 && tab.ui.history[0].subject == "Sketch the first ideas" && !view::unborn(&tab.ui)
        })
        .await?;
        git_is(&sketches, &["rev-list", "--count", "HEAD"], "1\n")?;
        git_is(&sketches, &["log", "--format=%s|%an"], "Sketch the first ideas|Aspen Walker\n")?;
        git_is(&sketches, &["ls-tree", "-r", "--name-only", "HEAD"], "ideas.txt\n")?;
        git_is(&sketches, &["status", "--porcelain=v1"], "")?;
        fs::write(&global, "")?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    fn preferences_at(&mut self) -> Result<Option<Section>> {
        Ok(self.tab.read_with(self.cx, |tab, _| tab.ui.preferences.open().then(|| tab.ui.preferences.section()))?)
    }

    fn type_default_branch(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| window.focus(&tab.ui.preferences.branch.focus_handle(cx)))?;
        self.key("cmd-a")?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.preferences.branch.update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx))
        })?;
        Ok(())
    }

    async fn change_a_preference(&mut self) -> Result<()> {
        self.start("GAP0508");
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        self.until("sketches open", |tab| tab.ui.repo == "sketches").await?;
        let app: Vec<_> = commands::in_bar(commands::Bar::App).into_iter().map(|command| command.id).collect();
        let keys = commands::find("app.preferences").and_then(|command| command.shortcut);
        if !app.starts_with(&["app.about", "app.preferences"]) || keys != Some("cmd-,") {
            return Err(format!("The FastKraken menu lists {app:?}, and Preferences has the keys {keys:?}").into());
        }
        let (before, title) = self.tab.read_with(self.cx, |tab, cx| {
            let title = commands::find("app.preferences").map(|command| (command.title)(&tab.ui, &Target::Repo));
            (settings::settings(cx), title)
        })?;
        if title.as_deref() != Some("Preferences…") || !before.reopen_last || before.stash_in_the_way || before.fetch_minutes() != 5 {
            return Err(format!("Before: {title:?}, {before:?}").into());
        }

        self.key("cmd-,")?;
        if self.preferences_at()? != Some(Section::General) {
            return Err(format!("Cmd-, opened Preferences at {:?}", self.preferences_at()?).into());
        }
        let (branch, listed, shown) = self.tab.read_with(self.cx, |tab, cx| {
            (
                tab.ui.preferences.branch.read(cx).text().to_owned(),
                preferences::sections(tab.repo.is_some()),
                tab.ui.repo.clone(),
            )
        })?;
        if branch != "main"
            || listed
                != [
                    Section::General,
                    Section::Accounts,
                    Section::Git,
                    Section::Commit,
                    Section::Tools,
                    Section::Shortcuts,
                    Section::Repository,
                ]
            || shown != "sketches"
            || preferences::interval(5) != "Every 5 minutes"
        {
            return Err(format!("Preferences opened with the branch {branch:?} and the sections {listed:?}").into());
        }
        self.key("down")?;
        self.until("Accounts with nothing saved", |tab| {
            tab.ui.preferences.section() == Section::Accounts
                && tab.ui.preferences.saved().is_some_and(|saved| saved.as_ref().is_ok_and(Vec::is_empty))
        })
        .await?;
        self.key("up")?;
        if self.preferences_at()? != Some(Section::General) {
            return Err(format!("Up went to {:?}", self.preferences_at()?).into());
        }
        self.action(Action::Refs(crate::refs::Action::ToggleList))?;
        let (at, listing) = (self.preferences_at()?, self.tab.read_with(self.cx, |tab, _| tab.ui.refs.list_open)?);
        if at.is_some() || !listing {
            return Err(format!("The branch button left Preferences at {at:?}, the switcher open {listing}").into());
        }
        self.key("escape")?;
        self.until("the switcher closed", |tab| !tab.ui.refs.list_open).await?;
        self.key("cmd-,")?;
        self.action(Action::Search(crate::search::Action::Open))?;
        let (at, searching) = (self.preferences_at()?, self.tab.read_with(self.cx, |tab, _| tab.ui.search.open())?);
        if at.is_some() || !searching {
            return Err(format!("The Search button left Preferences at {at:?}, search open {searching}").into());
        }
        self.action(Action::Search(crate::search::Action::Close))?;
        self.key("cmd-,")?;
        if self.preferences_at()? != Some(Section::General) {
            return Err(format!("Cmd-, opened Preferences again at {:?}", self.preferences_at()?).into());
        }

        self.action(Action::Autostash(crate::autostash::Action::Toggle))?;
        let (always, saved) = self.tab.read_with(self.cx, |tab, cx| (tab.ui.autostash.always, settings::settings(cx).stash_in_the_way))?;
        if !always || !saved {
            return Err(format!("After the tick the window stashes {always} and the setting says {saved}").into());
        }
        self.type_default_branch("two..dots")?;
        let (typed, setting) = self.tab.read_with(self.cx, |tab, cx| {
            (tab.ui.preferences.branch.read(cx).text().to_owned(), settings::settings(cx).default_branch)
        })?;
        let problem = preferences::branch_problem(&typed);
        if typed != "two..dots" || problem.as_deref() != Some("Branch names cannot contain two dots in a row") || setting != "main" {
            return Err(format!("Typed {typed:?}: {problem:?}, and the setting is {setting:?}").into());
        }
        self.type_default_branch("trunk")?;
        let setting = self.cx.update(|cx| settings::settings(cx).default_branch)?;
        if setting != "trunk" {
            return Err(format!("After typing trunk the setting is {setting:?}").into());
        }
        self.action(Action::Preferences(preferences::Action::FetchEvery(6)))?;
        self.action(Action::Preferences(preferences::Action::ToggleReopen))?;
        let after = self.cx.update(|cx| settings::settings(cx))?;
        if after.fetch_minutes() != 6 || preferences::interval(6) != "Every 6 minutes" || after.reopen_last {
            return Err(format!("After the stepper and the tick: {after:?}").into());
        }
        self.key("escape")?;
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        let typing = self.tab.update(self.cx, |tab, window, cx| tab.ui.preferences.branch.focus_handle(cx).is_focused(window))?;
        if self.preferences_at()?.is_some() || typing {
            return Err("Esc left Preferences open, or its field with the keys".into());
        }
        self.key("cmd-i")?;
        self.until("the New Repository dialog", |tab| tab.ui.init.open()).await?;
        let (opened, _) = self.creating()?;
        if opened.branch != "trunk" {
            return Err(format!("The New Repository dialog starts on {:?}", opened.branch).into());
        }
        self.key("escape")?;
        self.until("the dialog closed", |tab| !tab.ui.init.open()).await?;

        self.relaunch().await?;
        let written: serde_json::Value = serde_json::from_slice(&fs::read(state.join("settings.json"))?)?;
        let expected = serde_json::json!({"default_branch": "trunk", "fetch_every": 6, "stash_in_the_way": true, "reopen_last": false});
        for (key, value) in expected.as_object().into_iter().flatten() {
            if written.get(key) != Some(value) {
                return Err(format!("settings.json holds {key} = {:?}", written.get(key)).into());
            }
        }
        let last = self.cx.update(|cx| settings::memory(cx).and_then(|memory| memory.last_repository()))?;
        if last.as_ref().and_then(|path| path.file_name()) != Some(std::ffi::OsStr::new("sketches")) {
            return Err(format!("The last repository was {last:?}").into());
        }
        self.until("the start screen after the relaunch", |tab| tab.repo.is_none() && tab.ui.opening.is_none()).await?;
        self.cx.background_executor().timer(Duration::from_millis(300)).await;
        let open = self.tab.read_with(self.cx, |tab, _| (tab.repo.is_some(), tab.ui.opening.clone(), tab.ui.autostash.always))?;
        if open != (false, None, true) {
            return Err(format!("After the relaunch: repository open {}, opening {:?}, stashing {}", open.0, open.1, open.2).into());
        }
        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.default_branch = String::new()))?;
        self.key("cmd-,")?;
        let branch = self.tab.read_with(self.cx, |tab, cx| tab.ui.preferences.branch.read(cx).text().to_owned())?;
        if branch != "main" {
            return Err(format!("An empty default branch reads {branch:?} in Preferences").into());
        }
        let prune = self.cx.update(|cx| settings::settings(cx).prune_on_fetch)?;
        self.action(Action::Sync(crate::sync::Action::TogglePrune))?;
        let ticked = self.tab.read_with(self.cx, |tab, cx| (tab.ui.sync.prune, settings::settings(cx).prune_on_fetch))?;
        if ticked != (!prune, !prune) {
            return Err(format!("With no repository open the prune tick left {ticked:?}, from {prune}").into());
        }
        self.action(Action::Sync(crate::sync::Action::TogglePrune))?;
        self.key("down")?;
        self.until("Accounts read on the start screen", |tab| {
            tab.ui.preferences.section() == Section::Accounts
                && tab.ui.publish.account_shown()
                && !tab.ui.publish.working
                && !tab.ui.publish.open()
                && tab.ui.preferences.saved().is_some()
        })
        .await?;
        let hidden = self.tab.update(self.cx, |tab, window, cx| tab.ui.start.filter.focus_handle(cx).is_focused(window))?;
        if hidden {
            return Err("The start screen's filter kept the keys under Preferences".into());
        }
        self.key("end")?;
        if self.preferences_at()? != Some(Section::Shortcuts) {
            return Err(format!("End went to {:?} with no repository open", self.preferences_at()?).into());
        }
        self.key("escape")?;
        let after = self.tab.read_with(self.cx, |tab, _| (tab.ui.preferences.open(), tab.ui.publish.account_shown()))?;
        if after != (false, false) {
            return Err(format!("After Esc, Preferences open {} and the connection shown {}", after.0, after.1).into());
        }
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        let typing = self.tab.update(self.cx, |tab, window, cx| tab.ui.start.filter.focus_handle(cx).is_focused(window))?;
        if !typing {
            return Err("After Preferences closed the start screen's filter did not have the keys".into());
        }
        self.cx.update(|cx| {
            settings::Settings::update(cx, |settings| {
                settings.default_branch = "main".into();
                settings.fetch_every = None;
                settings.stash_in_the_way = false;
                settings.reopen_last = true;
            });
            settings::flush(cx);
        })?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    fn type_over(&mut self, pick: fn(&Tab) -> gpui::Entity<view::CommitInput>, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| window.focus(&pick(tab).focus_handle(cx)))?;
        self.key("cmd-a")?;
        self.tab.update(self.cx, |tab, window, cx| pick(tab).update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx)))?;
        Ok(())
    }

    fn commit_state(&mut self) -> Result<(Option<String>, bool, bool, Option<String>)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let disabled = commands::find("repo.commit")
                .and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx))
                .and_then(|entry| entry.disabled);
            (disabled, tab.ui.can_commit(cx), tab.ui.identity.form_shown(), tab.ui.identity.committing_as())
        })?)
    }

    async fn commit_without_identity(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0509");
        let global = global_config(dir);
        if !fs::read(&global)?.is_empty() {
            return Err("The sandbox's global configuration is not empty".into());
        }
        let root = dir.join("Larch-hollow");
        fs::create_dir(&root)?;
        git(&root, &["init", "-q", "-b", "main"])?;
        for (key, value) in [("core.hooksPath", "/dev/null"), ("commit.gpgSign", "false"), ("core.autocrlf", "false")] {
            git(&root, &["config", "--local", key, value])?;
        }
        fs::write(root.join("trail.txt"), TRAIL)?;
        git(&root, &["add", "--", "trail.txt"])?;
        git(&root, &["-c", "user.name=Larch Peer", "-c", "user.email=peer@example.invalid", "commit", "-q", "-m", "Start the hollow"])?;
        fs::write(root.join("camp.txt"), "Camp by the larches\n")?;
        self.open(&root)?;
        self.until("Larch-hollow open with camp.txt changed", |tab| {
            tab.ui.repo == "Larch-hollow" && files(tab, &[], &["camp.txt"]) && tab.ui.identity.known().is_some()
        })
        .await?;
        self.action(Action::Select(Selection::Changes))?;
        self.key("cmd-shift-s")?;
        self.until("camp.txt staged", |tab| files(tab, &["camp.txt"], &[])).await?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.update(cx, |field, cx| field.replace_text_in_range(None, "Pitch the camp", window, cx))
        })?;

        let (disabled, ready, form, line) = self.commit_state()?;
        let reason = "Set your name and email to commit";
        if disabled.as_deref() != Some(reason) || ready || !form || line.is_some() {
            return Err(format!("With no identity: Commit disabled {disabled:?}, ready {ready}, form {form}, line {line:?}").into());
        }
        let (scope, name, email) = self.tab.read_with(self.cx, |tab, cx| {
            let state = &tab.ui.identity;
            (state.scope(), state.name.read(cx).text().to_owned(), state.email.read(cx).text().to_owned())
        })?;
        if scope != Scope::Global || !name.is_empty() || !email.is_empty() {
            return Err(format!("The form opened {scope:?} with {name:?} and {email:?}").into());
        }
        self.key("cmd-enter")?;
        self.cx.background_executor().timer(Duration::from_millis(300)).await;
        git_is(&root, &["rev-list", "--count", "HEAD"], "1\n")?;
        git_is(&root, &["diff", "--cached", "--name-only"], "camp.txt\n")?;

        self.type_over(|tab| tab.ui.identity.name.clone(), "Larch Walker")?;
        self.type_over(|tab| tab.ui.identity.email.clone(), "larch")?;
        self.action(Action::Identity(identity::Action::Save(Place::Panel)))?;
        let error = self.tab.read_with(self.cx, |tab, _| tab.ui.identity.error().map(str::to_owned))?;
        if error.as_deref() != Some("Enter an email address, such as you@example.com.") || !fs::read(&global)?.is_empty() {
            return Err(format!("A bad email said {error:?}").into());
        }
        self.type_over(|tab| tab.ui.identity.email.clone(), "larch@example.invalid")?;
        self.key("enter")?;
        self.until("the name and email saved", |tab| {
            !tab.ui.identity.saving()
                && tab.ui.identity.reason().is_none()
                && tab.ui.notices.newest().is_some_and(|notice| {
                    notice.tone == Tone::Success
                        && notice.title == "Name and email saved"
                        && notice.detail == "Commits in every repository carry Larch Walker <larch@example.invalid>."
                })
        })
        .await?;
        let (disabled, ready, form, line) = self.commit_state()?;
        if disabled.is_some() || !ready || form || line.as_deref() != Some("Committing as Larch Walker <larch@example.invalid>") {
            return Err(format!("After saving: Commit disabled {disabled:?}, ready {ready}, form {form}, line {line:?}").into());
        }
        git_is(dir, &["config", "--file", path(&global)?, "--get", "user.name"], "Larch Walker\n")?;
        git_is(dir, &["config", "--file", path(&global)?, "--get", "user.email"], "larch@example.invalid\n")?;
        if git(&root, &["config", "--local", "--list"])?.contains("user.") {
            return Err("The form wrote the repository's own configuration".into());
        }
        self.key("cmd-enter")?;
        self.until("the commit in the graph", |tab| tab.ui.history.first().is_some_and(|row| row.subject == "Pitch the camp")).await?;
        git_is(&root, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"], "Larch Walker <larch@example.invalid>|Larch Walker <larch@example.invalid>\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;

        let before = fs::read(&global)?;
        self.action(Action::Identity(identity::Action::Edit))?;
        let (scope, name, focused) = self.tab.update(self.cx, |tab, window, cx| {
            let state = &tab.ui.identity;
            (state.scope(), state.name.read(cx).text().to_owned(), state.name.focus_handle(cx).is_focused(window))
        })?;
        if scope != Scope::Global || name != "Larch Walker" || !focused || !self.commit_state()?.2 {
            return Err(format!("Committing as opened the form {scope:?} with {name:?}, focused {focused}").into());
        }
        self.action(Action::Identity(identity::Action::SetScope(Scope::Local)))?;
        self.type_over(|tab| tab.ui.identity.name.clone(), "Larch at Work")?;
        self.type_over(|tab| tab.ui.identity.email.clone(), "larch@work.invalid")?;
        self.action(Action::Identity(identity::Action::Save(Place::Panel)))?;
        self.until("this repository's own name and email", |tab| {
            !tab.ui.identity.saving()
                && tab.ui.identity.committing_as().as_deref() == Some("Committing as Larch at Work <larch@work.invalid>")
                && tab.ui.notices.newest().is_some_and(|notice| {
                    notice.title == "Name and email saved"
                        && notice.detail == "Commits in Larch-hollow carry Larch at Work <larch@work.invalid>. Other repositories are unchanged."
                })
        })
        .await?;
        git_is(&root, &["config", "--local", "--get", "user.name"], "Larch at Work\n")?;
        git_is(&root, &["config", "--local", "--get", "user.email"], "larch@work.invalid\n")?;
        if fs::read(&global)? != before {
            return Err("Saving for this repository changed the global configuration".into());
        }

        self.action(Action::Identity(identity::Action::Edit))?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.identity.scope())? != Scope::Local {
            return Err("The form did not start on this repository's own name and email".into());
        }
        git(&root, &["config", "--local", "user.name", "Someone Else"])?;
        self.type_over(|tab| tab.ui.identity.name.clone(), "Larch Again")?;
        self.action(Action::Identity(identity::Action::Save(Place::Panel)))?;
        self.until("the save refused", |tab| !tab.ui.identity.saving() && tab.ui.identity.error().is_some()).await?;
        let error = self.tab.read_with(self.cx, |tab, _| tab.ui.identity.error().map(str::to_owned))?;
        if error.as_deref()
            != Some("This repository's own name and email changed to Someone Else <larch@work.invalid> meanwhile. Check them and save again.")
        {
            return Err(format!("The save that would overwrite said {error:?}").into());
        }
        git_is(&root, &["config", "--local", "--get", "user.name"], "Someone Else\n")?;
        self.action(Action::Identity(identity::Action::Cancel))?;

        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Git)))?;
        self.until("Preferences > Git with the global name and email", |tab| {
            tab.ui.preferences.section() == Section::Git
                && tab.ui.identity.global().is_some_and(|read| {
                    read.as_ref().is_ok_and(|values| values.shown().as_deref() == Some("Larch Walker <larch@example.invalid>"))
                })
        })
        .await?;
        let fields = self.tab.read_with(self.cx, |tab, cx| {
            (tab.ui.identity.global_name.read(cx).text().to_owned(), tab.ui.identity.global_email.read(cx).text().to_owned())
        })?;
        if fields != ("Larch Walker".to_owned(), "larch@example.invalid".to_owned()) {
            return Err(format!("Preferences > Git shows {fields:?}").into());
        }
        self.type_over(|tab| tab.ui.identity.global_name.clone(), "Larch W. Walker")?;
        self.key("enter")?;
        self.until("the global name saved from Preferences", |tab| {
            !tab.ui.identity.saving()
                && tab.ui.identity.global_error().is_none()
                && tab.ui.identity.global().is_some_and(|read| read.as_ref().is_ok_and(|values| values.name.as_deref() == Some("Larch W. Walker")))
        })
        .await?;
        git_is(dir, &["config", "--file", path(&global)?, "--get", "user.name"], "Larch W. Walker\n")?;
        self.key("escape")?;
        if self.preferences_at()?.is_some() {
            return Err("Esc left Preferences open".into());
        }
        fs::write(&global, "")?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    fn launched_since(mark: usize) -> Vec<Vec<String>> {
        crate::tools::launched().into_iter().skip(mark).collect()
    }

    async fn forget_tools(&mut self, dir: &Path) -> Result<()> {
        let _ = fs::remove_file(dir.join("bin/code"));
        let _ = fs::remove_dir_all(dir.join("Applications/Zed.app"));
        self.cx.update(|cx| {
            settings::Settings::update(cx, |s| {
                s.editor = None;
                s.editor_command.clear();
                s.terminal = None;
            })
        })?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui.tools.editor = None;
            tab.ui.tools.terminal = None;
            tab.ui.tools.command.update(cx, |field, cx| field.clear(cx));
        })?;
        self.action(Action::Tools(crate::tools::Action::Look))?;
        self.until("only Terminal found", |_| {
            crate::tools::detected().is_some_and(|found| found.editors.is_empty() && found.terminals.len() == 1)
        })
        .await
    }

    async fn open_in_editor(&mut self, dir: &Path) -> Result<()> {
        use crate::tools::{self, Action as T};
        self.start("GAP0514");
        let root = init(dir, "Juniper")?;
        record(&root, "trail.txt", "Juniper: the first post.\nJuniper: the ford.\nJuniper: the summit.\n", "Start the juniper trail")?;
        fs::write(root.join("trail.txt"), "Juniper: the first post.\nJuniper: the stepping stones.\nJuniper: the summit.\n")?;
        let head = oid(&root, "HEAD")?;
        let top = PathBuf::from(git(&root, &["rev-parse", "--show-toplevel"])?.trim_end());
        let trail = top.join("trail.txt");
        let file_at = |line: u32| format!("{}:{line}", trail.display());
        let (top_text, file_text) = (top.display().to_string(), trail.display().to_string());
        self.open(&root)?;
        self.until("Juniper open with trail.txt changed", |tab| tab.ui.repo == "Juniper" && files(tab, &[], &["trail.txt"]))
            .await?;

        let terminal = dir.join("Applications/Terminal.app").display().to_string();
        let title = self.tab.read_with(self.cx, |tab, _| tools::terminal_title(&tab.ui))?;
        if title != "Open in Terminal" {
            return Err(format!("The terminal command is called {title:?}").into());
        }
        let mark = tools::launched().len();
        self.key("alt-t")?;
        self.until("Terminal opened at Juniper", |_| {
            Self::launched_since(mark) == [vec!["/usr/bin/open".to_owned(), "-a".into(), terminal.clone(), top_text.clone()]]
        })
        .await?;

        let mark = tools::launched().len();
        self.key("cmd-shift-e")?;
        self.until("Preferences at External tools", |tab| {
            tab.ui.preferences.open()
                && tab.ui.preferences.section() == Section::Tools
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Choose an editor first")
        })
        .await?;
        if !Self::launched_since(mark).is_empty() {
            return Err(format!("With no editor, {:?} was launched", Self::launched_since(mark)).into());
        }

        fs::create_dir_all(dir.join("bin"))?;
        let code = dir.join("bin/code");
        fs::write(&code, "#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(&code, <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755))?;
        let zed = dir.join("Applications/Zed.app");
        fs::create_dir_all(zed.join("Contents"))?;
        self.action(Action::Tools(T::Look))?;
        self.until("VS Code and Zed found", |_| {
            tools::detected().is_some_and(|found| found.editors.iter().map(|f| f.tool.id).collect::<Vec<_>>() == ["code", "zed"])
        })
        .await?;
        let titles = self.tab.read_with(self.cx, |tab, _| (tools::repo_title(&tab.ui), tools::file_title(&tab.ui, None)))?;
        if titles != ("Open repository in VS Code".to_owned(), "Open in VS Code".to_owned()) {
            return Err(format!("With VS Code found, the commands are called {titles:?}").into());
        }
        self.key("escape")?;

        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("trail.txt's diff", |tab| patch(tab, "+Juniper: the stepping stones.")).await?;
        let line = self.tab.read_with(self.cx, |tab, _| {
            let row = tab.ui.diff.lines.iter().position(|row| row.kind.is_code() && row.new == Some(2))?;
            crate::diff_select::code_target(&tab.ui, row, 2)
        })?;
        let line = line.ok_or("The diff has no row for line 2")?;
        let rows: Vec<(String, String)> = self
            .menu_rows(&line)?
            .into_iter()
            .filter(|row| row.0.starts_with("file."))
            .map(|row| (row.0.to_owned(), row.1))
            .collect();
        let expected = [
            ("file.edit", "Open in VS Code at line 2"),
            ("file.reveal", "Reveal in Finder"),
            ("file.copy-path", "Copy path"),
            ("file.copy-full-path", "Copy full path"),
        ]
        .map(|(id, title)| (id.to_owned(), title.to_owned()));
        if rows != expected {
            return Err(format!("A diff line's menu lists {rows:?}").into());
        }
        let mark = tools::launched().len();
        self.action(Action::Menu(Some(view::Menu::Object { target: line, at: gpui::Point::default() })))?;
        self.key("down")?;
        self.key("enter")?;
        let code_text = code.display().to_string();
        self.until("VS Code at line 2", |tab| {
            tab.ui.menu.is_none()
                && Self::launched_since(mark) == [vec![code_text.clone(), "--goto".into(), file_at(2)]]
        })
        .await?;

        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Tools)))?;
        self.action(Action::Tools(T::Editor("zed".into())))?;
        let chosen = self.cx.update(|cx| settings::settings(cx).editor)?;
        if chosen.as_deref() != Some("zed") {
            return Err(format!("Choosing Zed kept {chosen:?}").into());
        }
        self.key("escape")?;
        let mark = tools::launched().len();
        self.key("cmd-shift-e")?;
        let zed_text = zed.display().to_string();
        self.until("Zed opened with Juniper", |_| {
            Self::launched_since(mark) == [vec!["/usr/bin/open".to_owned(), "-a".into(), zed_text.clone(), top_text.clone()]]
        })
        .await?;

        self.key("cmd-,")?;
        self.action(Action::Tools(T::Editor(tools::CUSTOM.into())))?;
        self.key("escape")?;
        let mark = tools::launched().len();
        self.key("cmd-shift-e")?;
        self.until("the command asked for", |tab| {
            tab.ui.preferences.open()
                && tab.ui.preferences.section() == Section::Tools
                && tab.ui.notices.newest().is_some_and(|n| {
                    n.title == "Choose an editor first" && n.detail == "Type the command that opens your editor."
                })
        })
        .await?;
        if !Self::launched_since(mark).is_empty() {
            return Err(format!("With no command typed, {:?} was launched", Self::launched_since(mark)).into());
        }
        self.type_over(|tab| tab.ui.tools.command.clone(), "code -n {file}:{line}")?;
        let saved = self.cx.update(|cx| {
            let chosen = settings::settings(cx);
            (chosen.editor, chosen.editor_command)
        })?;
        if saved != (Some(tools::CUSTOM.to_owned()), "code -n {file}:{line}".to_owned()) {
            return Err(format!("The custom command was kept as {saved:?}").into());
        }
        self.key("escape")?;
        let unstaged = Target::File { path: "trail.txt".into(), side: commands::Side::Unstaged };
        let mark = tools::launched().len();
        let run = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &unstaged, &["file.edit"], cx).into_iter().next().map(|entry| (entry.title, entry.action))
        })?;
        let Some((title, Some(edit))) = run else { return Err("Open in code is not offered for trail.txt".into()) };
        if title != "Open in code" {
            return Err(format!("With the custom command, the file's row is {title:?}").into());
        }
        self.action(edit)?;
        self.until("code -n at line 1", |_| {
            Self::launched_since(mark) == [vec![code_text.clone(), "-n".into(), file_at(1)]]
        })
        .await?;

        self.key("cmd-,")?;
        self.type_over(|tab| tab.ui.tools.command.clone(), "nonesuch {file}")?;
        self.key("escape")?;
        let mark = tools::launched().len();
        self.action(Action::Handoff(crate::handoff::Action::EditRepo))?;
        self.until("the failure explained", |tab| {
            tab.ui.notices.newest().is_some_and(|n| {
                n.tone == Tone::Error
                    && n.title == "nonesuch did not open"
                    && n.detail == "nonesuch is not on your search path"
                    && n.actions.iter().any(|(label, action)| {
                        label.as_ref() == "External tools…"
                            && matches!(action, Action::Preferences(preferences::Action::Open(Some(Section::Tools))))
                    })
            })
        })
        .await?;
        if !Self::launched_since(mark).is_empty() {
            return Err(format!("A missing program launched {:?}", Self::launched_since(mark)).into());
        }

        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], " M trail.txt\n")?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        if !file_text.ends_with("/Juniper/trail.txt") {
            return Err(format!("trail.txt was located at {file_text}").into());
        }
        self.forget_tools(dir).await?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    fn tab_strip(&mut self) -> Result<(usize, usize, Vec<String>)> {
        let (count, front) = self.tab.tabs(self.cx)?;
        let titles = self.cx.update(|cx| {
            crate::tabs::strip(cx).map_or_else(Vec::new, |strip| strip.tabs.iter().map(|shown| shown.title.clone()).collect())
        })?;
        Ok((count, front, titles))
    }

    async fn until_tabs(&mut self, description: &str, count: usize, front: usize, titles: &[&str]) -> Result<()> {
        let wanted: Vec<String> = titles.iter().map(|title| title.to_string()).collect();
        let started = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            let strip = self.tab_strip()?;
            if strip == (count, front, wanted.clone()) {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description}: the strip is {strip:?}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    async fn two_tabs(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0512");
        let alder = init(dir, "Alder-tabs")?;
        record(&alder, "trail.txt", TRAIL, "Plant the alder")?;
        let plant = oid(&alder, "HEAD")?;
        record(&alder, "camp.txt", "Camp by the alders\n", "Mark the alder trail")?;
        let alder_tip = oid(&alder, "HEAD")?;
        let alder_bare = dir.join("Alder-tabs.git");
        git(dir, &["init", "-q", "--bare", "-b", "main", path(&alder_bare)?])?;
        git(&alder, &["remote", "add", "origin", path(&alder_bare)?])?;
        let rowan = init(dir, "Rowan-tabs")?;
        record(&rowan, "trail.txt", TRAIL, "Plant the rowan")?;
        let server = Silent::start()?;
        git(&rowan, &["remote", "add", "origin", &format!("http://127.0.0.1:{}/Rowan-tabs.git", server.port)])?;
        let alder_root = fs::canonicalize(&alder)?;
        let rowan_root = fs::canonicalize(&rowan)?;
        let reopen = self.cx.update(|cx| settings::settings(cx).reopen_last)?;
        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.reopen_last = false))?;
        self.relaunch().await?;
        self.until_tabs("one tab", 1, 0, &["New tab"]).await?;
        self.open(&alder)?;
        self.until("Alder-tabs open", |tab| tab.ui.repo == "Alder-tabs" && tab.ui.history.len() == 2).await?;
        self.select_commit(&plant).await?;
        let no_strip = self.tab.update(self.cx, |tab, _, cx| crate::tabs::render(tab, cx).is_none())?;
        if !no_strip {
            return Err("One tab draws a strip".into());
        }

        self.key("cmd-t")?;
        self.until_tabs("a new tab in front", 2, 1, &["Alder-tabs", "New tab"]).await?;
        self.until("the start screen in the new tab", |tab| tab.repo.is_none() && tab.ui.repo.is_empty()).await?;
        if self.window_title()? != "FastKraken" {
            return Err(format!("The new tab's window is called {:?}", self.window_title()?).into());
        }
        let drawn = self.tab.update(self.cx, |tab, _, cx| crate::tabs::render(tab, cx).is_some())?;
        if !drawn {
            return Err("Two tabs draw no strip".into());
        }

        self.action(Action::Start(start::Action::Open(rowan_root.clone())))?;
        self.until("Rowan-tabs open", |tab| tab.ui.repo == "Rowan-tabs" && tab.ui.history.len() == 1).await?;
        self.until_tabs("both repositories in the strip", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        if !self.window_title()?.starts_with("Rowan-tabs") {
            return Err(format!("With Rowan-tabs in front the window is called {:?}", self.window_title()?).into());
        }
        fs::write(rowan.join("trail.txt"), WAYPOINT)?;
        self.until("trail.txt changed in Rowan-tabs", |tab| files(tab, &[], &["trail.txt"])).await?;
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("trail.txt's changes open", |tab| tab.ui.diff_open && patch(tab, "+Waypoint: Willow bridge")).await?;
        self.action(Action::Sync(crate::sync::Action::FetchAll))?;
        self.during("the fetch running in Rowan-tabs", |tab| {
            tab.ui.jobs.network.as_ref().is_some_and(|job| job.spec.kind == jobs::Kind::Fetch && job.spec.interactive)
        })
        .await?;
        let fetching = self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.jobs.network.as_ref().map(|job| job.id))?
            .ok_or("No fetch runs in Rowan-tabs")?;

        self.key("ctrl-shift-tab")?;
        self.until_tabs("Alder-tabs in front", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;
        let wanted = plant.clone();
        self.until("Alder-tabs with its commit selected", move |tab| {
            tab.ui.repo == "Alder-tabs" && tab.ui.selection == Some(Selection::Commit(wanted.clone())) && tab.ui.jobs.idle()
        })
        .await?;
        if self.fetch_all()? != Some(None) {
            return Err(format!("While Rowan-tabs fetches, Fetch in Alder-tabs is {:?}", self.fetch_all()?).into());
        }
        if !self.window_title()?.starts_with("Alder-tabs") {
            return Err(format!("With Alder-tabs in front the window is called {:?}", self.window_title()?).into());
        }
        let behind = self.tab.read_tab(self.cx, 1, |tab, _| tab.ui.jobs.network.as_ref().map(|job| job.id))?;
        if behind != Some(Some(fetching)) {
            return Err(format!("Behind, Rowan-tabs runs {behind:?}, not the fetch {fetching:?}").into());
        }

        record(&rowan, "camp.txt", "Camp under the rowan\n", "Pitch the camp")?;
        let started = Instant::now();
        loop {
            let seen = self.tab.read_tab(self.cx, 1, |tab, _| (tab.stale, tab.ui.history.len()))?;
            match seen {
                Some((Some(true), 1)) => break,
                Some((_, 1)) if started.elapsed() < Duration::from_secs(10) => {}
                other => return Err(format!("Behind, Rowan-tabs is (stale, commits) {other:?}").into()),
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
        self.cx.background_executor().timer(Duration::from_millis(400)).await;
        if self.tab.read_tab(self.cx, 1, |tab, _| tab.ui.history.len())? != Some(1) {
            return Err("Rowan-tabs refreshed while it was behind".into());
        }

        self.key("cmd-2")?;
        self.until_tabs("Rowan-tabs in front", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.during("Rowan-tabs refreshed with its file open and its fetch running", move |tab| {
            tab.ui.repo == "Rowan-tabs"
                && tab.stale.is_none()
                && tab.ui.history.iter().any(|row| row.subject == "Pitch the camp")
                && tab.ui.selection == Some(file("trail.txt", false))
                && tab.ui.diff_open
                && tab.ui.jobs.network.as_ref().map(|job| job.id) == Some(fetching)
        })
        .await?;
        let job = self
            .tab
            .read_with(self.cx, |tab, _| running(tab).and_then(|notice| notice.job))?
            .ok_or("The fetch's notice has no Cancel button")?;
        self.action(Action::Jobs(jobs::Action::Cancel(job)))?;
        self.until("the fetch cancelled", |tab| {
            tab.ui.jobs.idle() && tab.ui.notices.items().iter().any(|notice| notice.title == "Fetch cancelled")
        })
        .await?;
        let alder_notices = self.tab.read_tab(self.cx, 0, |tab, _| tab.ui.notices.items().iter().any(|notice| notice.title == "Fetch cancelled"))?;
        if alder_notices != Some(false) {
            return Err("Alder-tabs shows Rowan-tabs' notice".into());
        }

        self.action(Action::Start(start::Action::Open(alder_root.clone())))?;
        self.until_tabs("Alder-tabs in front again", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;
        let wanted = plant.clone();
        self.until("Alder-tabs as it was", move |tab| tab.ui.selection == Some(Selection::Commit(wanted.clone()))).await?;
        let session = self.cx.update(|cx| settings::memory(cx).map(|memory| (memory.session(), memory.last_repository())))?;
        if session != Some((vec![alder_root.clone(), rowan_root.clone()], Some(alder_root.clone()))) {
            return Err(format!("The session remembered is {session:?}").into());
        }

        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.reopen_last = true))?;
        self.relaunch().await?;
        self.until_tabs("both tabs back", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.until("Alder-tabs reopened", |tab| tab.ui.repo == "Alder-tabs" && tab.ui.history.len() == 2).await?;
        let started = Instant::now();
        while self.tab.read_tab(self.cx, 1, |tab, _| tab.ui.repo.clone())?.as_deref() != Some("Rowan-tabs") {
            if started.elapsed() > Duration::from_secs(10) {
                return Err("Rowan-tabs did not reopen behind".into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }

        self.key("ctrl-tab")?;
        self.until_tabs("Rowan-tabs in front", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.until("Rowan-tabs drawn", |tab| tab.ui.repo == "Rowan-tabs" && tab.ui.history.len() == 2).await?;
        self.key("ctrl-tab")?;
        self.until_tabs("Alder-tabs in front", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;

        self.key("cmd-,")?;
        self.until("Preferences open", |tab| tab.ui.preferences.open()).await?;
        self.key("cmd-w")?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open() && tab.ui.repo == "Alder-tabs").await?;
        self.until_tabs("both tabs still there", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.key("cmd-2")?;
        self.until_tabs("Rowan-tabs in front", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.until("trail.txt still changed", |tab| tab.ui.repo == "Rowan-tabs" && files(tab, &[], &["trail.txt"])).await?;
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("trail.txt's changes open", |tab| tab.ui.diff_open && patch(tab, "+Waypoint: Willow bridge")).await?;
        self.key("cmd-w")?;
        self.until("the changes closed", |tab| !tab.ui.diff_open && tab.ui.repo == "Rowan-tabs").await?;
        self.until_tabs("both tabs still there", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;

        self.action(Action::AskDiscard(crate::view::Discard::File("trail.txt".into())))?;
        self.until("the discard asks first", |tab| matches!(tab.ui.prompt.asked(), Some(crate::prompt::Kind::Discard(_)))).await?;
        self.key("cmd-1")?;
        self.until("Cmd-1 refused behind the question", |tab| {
            tab.ui.prompt.open() && tab.ui.notices.items().iter().any(|notice| notice.title == "Close the open dialog first")
        })
        .await?;
        self.until_tabs("Rowan-tabs still in front", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.cx.update(|cx| crate::tabs::ask(cx, crate::tabs::Request::Handed(alder_root.clone())))?;
        self.until_tabs("Alder-tabs in front of the question", 2, 0, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.key("cmd-2")?;
        self.until_tabs("Rowan-tabs back", 2, 1, &["Alder-tabs", "Rowan-tabs"]).await?;
        self.until("the question still open", |tab| tab.ui.prompt.open()).await?;
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        let focused = self.prompt_focus()?;
        if focused != Some(crate::prompt::Stop::Cancel) {
            return Err(format!("Back in Rowan-tabs, the question's focus is {focused:?}").into());
        }
        self.key("escape")?;
        self.until("the question cancelled", |tab| !tab.ui.prompt.open()).await?;

        self.key("cmd-w")?;
        self.until_tabs("Rowan-tabs closed", 1, 0, &["Alder-tabs"]).await?;
        self.until("Alder-tabs in front", |tab| tab.ui.repo == "Alder-tabs").await?;

        self.key("cmd-w")?;
        self.until("the start screen in the last tab", |tab| tab.repo.is_none() && tab.ui.repo.is_empty()).await?;
        self.until_tabs("one tab", 1, 0, &["New tab"]).await?;
        self.key("cmd-w")?;
        self.until("Cmd-W refused", |tab| tab.ui.notices.items().iter().any(|notice| notice.title == "This is the only tab")).await?;
        self.until_tabs("still one tab", 1, 0, &["New tab"]).await?;

        git_is(&alder, &["rev-parse", "HEAD"], &format!("{alder_tip}\n"))?;
        git_is(&alder, &["status", "--porcelain=v1"], "")?;
        git_is(&alder, &["for-each-ref", "--format=%(refname)", "refs/remotes"], "")?;
        git_is(&rowan, &["log", "--format=%s"], "Pitch the camp\nPlant the rowan\n")?;
        git_is(&rowan, &["status", "--porcelain=v1"], " M trail.txt\n")?;
        git_is(&rowan, &["for-each-ref", "--format=%(refname)", "refs/remotes"], "")?;
        drop(server);
        self.cx.update(|cx| settings::Settings::update(cx, |settings| settings.reopen_last = reopen))?;
        self.pass();
        Ok(())
    }
}

impl Replay<'_> {
    async fn until_notice(&mut self, description: &str, title: &'static str, detail: impl Fn(&str) -> bool) -> Result<String> {
        self.until(description, |tab| tab.ui.notices.items().iter().any(|notice| notice.title == title && detail(&notice.detail)))
            .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.items().iter().rev().find(|notice| notice.title == title).map(|notice| notice.detail.clone())
        })?;
        shown.ok_or_else(|| format!("No notice {title:?}").into())
    }

    async fn second(&mut self, link: &Path, folder: &Path, args: &[&str]) -> Result<(Option<i32>, String)> {
        let mut child = std::process::Command::new(link)
            .args(args)
            .current_dir(folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                let mut said = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    std::io::Read::read_to_string(&mut stderr, &mut said)?;
                }
                return Ok((status.code(), said));
            }
            if started.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} {args:?} in {} was still running after 10 seconds", link.display(), folder.display()).into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    async fn second_launch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0513");
        let spruce = init(dir, "Spruce-cli")?;
        record(&spruce, "trail.txt", TRAIL, "Plant the spruce")?;
        let hemlock = init(dir, "Hemlock-cli")?;
        record(&hemlock, "trail.txt", TRAIL, "Plant the hemlock")?;
        let (spruce_tip, hemlock_tip) = (oid(&spruce, "HEAD")?, oid(&hemlock, "HEAD")?);
        self.until_tabs("one tab on the start screen", 1, 0, &["New tab"]).await?;

        let places = crate::cli::places()?;
        if !places.home.starts_with(dir) || !places.system.starts_with(dir) {
            return Err(format!("The command would go outside the sandbox: {places:?}").into());
        }
        let program = fs::canonicalize(std::env::current_exe()?)?;
        self.action(Action::Run("app.install-command"))?;
        let said = self.until_notice("the command installed", "Installed the fastkraken command", |_| true).await?;
        let local = places.local().join("fastkraken");
        let wanted = "~/.local/bin/fastkraken opens FastKraken. In a terminal, fastkraken opens the folder you are in, and fastkraken <path> another. ~/.local/bin is not on your search path, so a terminal will not find it yet. Add it in your shell's profile: export PATH=\"$HOME/.local/bin:$PATH\"";
        if said != wanted {
            return Err(format!("The notice said {said:?}").into());
        }
        if fs::read_link(&local)? != program {
            return Err(format!("{} leads to {:?}, not {}", local.display(), fs::read_link(&local), program.display()).into());
        }
        if places.system.exists() {
            return Err("Installing made a /usr/local/bin".into());
        }
        fs::create_dir_all(&places.system)?;
        let system = places.system.join("fastkraken");
        let start = format!("{} opens FastKraken.", system.display());
        self.action(Action::Run("app.install-command"))?;
        self.until_notice("the command installed in /usr/local/bin", "Installed the fastkraken command", |detail| detail.starts_with(&start))
            .await?;
        if fs::read_link(&system)? != program {
            return Err(format!("{} does not lead to FastKraken", system.display()).into());
        }

        let state = storage::override_directory().ok_or("The replay has no state folder")?;
        let listener = self.cx.update(|cx| crate::instance::listen(&state, cx))??;
        let socket = listener.path().to_owned();
        if !socket.starts_with(dir) && !socket.starts_with(std::env::temp_dir()) {
            return Err(format!("The socket is at {}", socket.display()).into());
        }
        let shown = crate::instance::activated();

        let (code, said) = self.second(&local, &spruce, &[]).await?;
        if code != Some(0) {
            return Err(format!("fastkraken in Spruce-cli exited {code:?}: {said}").into());
        }
        self.until_tabs("Spruce-cli in the one tab", 1, 0, &["Spruce-cli"]).await?;
        self.until("Spruce-cli open", |tab| tab.ui.repo == "Spruce-cli" && tab.ui.history.len() == 1).await?;

        let (code, said) = self.second(&system, &spruce, &["../Hemlock-cli"]).await?;
        if code != Some(0) {
            return Err(format!("fastkraken ../Hemlock-cli exited {code:?}: {said}").into());
        }
        self.until_tabs("Hemlock-cli in a second tab", 2, 1, &["Spruce-cli", "Hemlock-cli"]).await?;
        self.until("Hemlock-cli open", |tab| tab.ui.repo == "Hemlock-cli" && tab.ui.history.len() == 1).await?;

        let (code, said) = self.second(&local, &spruce, &["."]).await?;
        if code != Some(0) {
            return Err(format!("fastkraken . exited {code:?}: {said}").into());
        }
        self.until_tabs("Spruce-cli in front", 2, 0, &["Spruce-cli", "Hemlock-cli"]).await?;
        if crate::instance::activated() != shown + 3 {
            return Err(format!("FastKraken came to the front {} times for three folders", crate::instance::activated() - shown).into());
        }

        drop(listener);
        if socket.exists() {
            return Err("The socket stayed after the app stopped listening".into());
        }
        let (code, said) = self.second(&local, &hemlock, &[]).await?;
        if code != Some(3) || !said.contains("No FastKraken is running for this sandbox.") {
            return Err(format!("With no FastKraken listening, fastkraken exited {code:?}: {said}").into());
        }
        self.until_tabs("still two tabs", 2, 0, &["Spruce-cli", "Hemlock-cli"]).await?;

        git_is(&spruce, &["rev-parse", "HEAD"], &format!("{spruce_tip}\n"))?;
        git_is(&hemlock, &["rev-parse", "HEAD"], &format!("{hemlock_tip}\n"))?;
        git_is(&spruce, &["status", "--porcelain=v1"], "")?;
        git_is(&hemlock, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }
}

fn clones_into(name: &str) -> usize {
    let end = format!(" {name}");
    activity::entries()
        .iter()
        .filter(|entry| {
            let line = entry.command_line();
            line.contains(" clone --progress -- ") && line.ends_with(&end)
        })
        .count()
}

fn running(tab: &Tab) -> Option<&Notice> {
    let id = tab.ui.jobs.network.as_ref()?.notice?;
    tab.ui.notices.get(id).filter(|notice| notice.tone == Tone::Progress)
}

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "The sandbox path is not UTF-8".into())
}

struct Silent {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Silent {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while !stopping.load(Ordering::SeqCst) {
                if let Ok((stream, _)) = listener.accept() {
                    held.push(stream);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(Self { port, stop })
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
