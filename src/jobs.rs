use crate::{
    askpass,
    Tab, kit,
    notice::{Notice, Tone},
    theme::*,
    view::{self, Ui},
};
use gpui::{Context, Div, Window, div, prelude::*, px, relative, rgb};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(120);
pub(crate) const SILENCE: Duration = Duration::from_secs(20);

const PHASES: &[&str] = &[
    "Enumerating objects",
    "Counting objects",
    "Compressing objects",
    "Receiving objects",
    "Resolving deltas",
    "Writing objects",
    "Unpacking objects",
    "Checking objects",
    "Checking connectivity",
    "Updating files",
    "Finding sources",
    "Downloading LFS objects",
    "Uploading LFS objects",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    pub phase: String,
    pub percent: Option<u8>,
    pub detail: String,
}

impl Progress {
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let line = line.strip_prefix("remote:").map_or(line, str::trim_start);
        if let Some(rest) = line.strip_prefix("Rebasing (") {
            let (at, of) = rest.strip_suffix(')')?.split_once('/')?;
            let (at, of) = (at.parse::<u32>().ok()?, of.parse::<u32>().ok()?);
            if of == 0 || at > of {
                return None;
            }
            return Some(Self {
                phase: "Rebasing".into(),
                percent: Some((at * 100 / of) as u8),
                detail: format!("commit {at} of {of}"),
            });
        }
        let (phase, rest) = line.split_once(':')?;
        if !PHASES.contains(&phase) {
            return None;
        }
        let rest = rest.trim();
        let rest = rest.strip_suffix("done.").unwrap_or(rest).trim().trim_end_matches(',').trim();
        let (percent, rest) = match rest.split_once('%') {
            Some((number, after)) => match number.trim().parse::<u8>() {
                Ok(number) => (Some(number.min(100)), after.trim()),
                Err(_) => (None, rest),
            },
            None => (None, rest),
        };
        let detail = match rest.strip_prefix('(').and_then(|rest| rest.split_once(')')) {
            Some((count, after)) => match count.split_once('/') {
                Some((at, of)) => format!("{at} of {of}{after}"),
                None => rest.to_owned(),
            },
            None => rest.to_owned(),
        };
        Some(Self { phase: phase.to_owned(), percent, detail: detail.trim().to_owned() })
    }

    pub(crate) fn text(&self) -> String {
        let mut text = self.phase.clone();
        if let Some(percent) = self.percent {
            text.push_str(&format!(" {percent}%"));
        }
        if !self.detail.is_empty() {
            text.push_str(" · ");
            text.push_str(&self.detail);
        }
        text
    }
}

pub(crate) fn is_progress(line: &str) -> bool {
    let trimmed = line.trim();
    let bare = trimmed.strip_prefix("remote:").map_or(trimmed, str::trim_start);
    Progress::parse(line).is_some() || bare.starts_with("Total ") && bare.contains("(delta ")
}

#[derive(Clone)]
pub(crate) struct Link(Arc<Shared>);

struct Shared {
    cancel: AtomicBool,
    cancellable: AtomicBool,
    heard: Mutex<Heard>,
    asking: Option<Arc<askpass::Session>>,
}

struct Heard {
    progress: Option<Progress>,
    at: Instant,
    latest: Option<String>,
    hooks: Hooks,
}

#[derive(Default)]
struct Hooks {
    expected: Vec<String>,
    since: Option<Instant>,
    running: Option<(String, Instant)>,
    started: bool,
    ran: Vec<crate::hooks::Ran>,
}

thread_local! {
    static CURRENT: RefCell<Option<Link>> = const { RefCell::new(None) };
}

struct Entered(Option<Link>);

impl Drop for Entered {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

impl Link {
    pub(crate) fn new(cancellable: bool) -> Self {
        Self::with(cancellable, None)
    }

    pub(crate) fn interactive() -> Self {
        Self::with(true, askpass::Broker::global().map(|broker| broker.session()))
    }

    fn with(cancellable: bool, asking: Option<Arc<askpass::Session>>) -> Self {
        Self(Arc::new(Shared {
            cancel: AtomicBool::new(false),
            cancellable: AtomicBool::new(cancellable),
            heard: Mutex::new(Heard { progress: None, at: Instant::now(), latest: None, hooks: Hooks::default() }),
            asking,
        }))
    }

    pub(crate) fn session(&self) -> Option<&Arc<askpass::Session>> {
        self.0.asking.as_ref()
    }

    pub(crate) fn declined(&self) -> bool {
        self.session().is_some_and(|session| session.declined())
    }

    pub(crate) fn enter<R>(&self, work: impl FnOnce() -> R) -> R {
        let previous = CURRENT.with(|current| current.borrow_mut().replace(self.clone()));
        let _restore = Entered(previous);
        work()
    }

    pub(crate) fn current() -> Option<Self> {
        CURRENT.with(|current| current.borrow().clone())
    }

    pub(crate) fn cancel(&self) -> bool {
        if !self.cancellable() {
            return false;
        }
        self.0.cancel.store(true, Ordering::SeqCst);
        true
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::SeqCst)
    }

    pub(crate) fn cancellable(&self) -> bool {
        self.0.cancellable.load(Ordering::SeqCst)
    }

    pub(crate) fn hold(&self) {
        self.0.cancellable.store(false, Ordering::SeqCst);
    }

    pub(crate) fn release(&self) {
        self.0.cancellable.store(true, Ordering::SeqCst);
    }

    pub(crate) fn settle(&self) -> bool {
        self.hold();
        self.0.cancel.swap(false, Ordering::SeqCst)
    }

    fn heard_mut(&self) -> std::sync::MutexGuard<'_, Heard> {
        self.0.heard.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn heard(&self, line: &str) {
        let progress = Progress::parse(line);
        let mut heard = self.heard_mut();
        heard.at = Instant::now();
        if progress.is_some() {
            heard.progress = progress;
        } else if !is_progress(line) && !line.trim_start().starts_with("hint:") && !line.trim().is_empty() {
            heard.latest = Some(line.trim().to_owned());
        }
    }

    pub(crate) fn printed(&self, bytes: &[u8]) {
        let mut heard = self.heard_mut();
        if heard.hooks.running.is_none() {
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().map(str::trim).filter(|line| !line.is_empty()).last() {
            heard.latest = Some(line.to_owned());
        }
    }

    pub(crate) fn latest(&self) -> Option<String> {
        self.heard_mut().latest.clone()
    }

    pub(crate) fn expect_hooks(&self, hooks: &[String]) {
        let mut heard = self.heard_mut();
        heard.hooks.expected = hooks.to_vec();
        heard.hooks.since = Some(Instant::now());
    }

    pub(crate) fn hook_started(&self, hook: &str) {
        if hook.starts_with("post-") {
            self.hold();
        }
        let mut heard = self.heard_mut();
        heard.at = Instant::now();
        heard.hooks.started = true;
        heard.hooks.running = Some((hook.to_owned(), Instant::now()));
    }

    pub(crate) fn hook_ended(&self) {
        let mut heard = self.heard_mut();
        heard.at = Instant::now();
        heard.hooks.running = None;
    }

    pub(crate) fn hook(&self) -> Option<(String, Instant)> {
        let heard = self.heard_mut();
        let hooks = &heard.hooks;
        hooks.running.clone().or_else(|| {
            let first = hooks.expected.first().filter(|_| !hooks.started)?;
            Some((first.clone(), hooks.since?))
        })
    }

    pub(crate) fn hooks_ran(&self, ran: &[crate::hooks::Ran]) {
        self.heard_mut().hooks.ran.extend_from_slice(ran);
    }

    pub(crate) fn ran(&self) -> Vec<crate::hooks::Ran> {
        self.heard_mut().hooks.ran.clone()
    }

    pub(crate) fn alive(&self) {
        self.heard_mut().at = Instant::now();
    }

    pub(crate) fn snapshot(&self) -> (Option<Progress>, Instant) {
        let heard = self.heard_mut();
        let at = match self.session().map(|session| session.touched()) {
            Some(answered) => heard.at.max(answered),
            None => heard.at,
        };
        (heard.progress.clone(), at)
    }

    pub(crate) fn asking(&self) -> bool {
        self.session().is_some_and(|session| session.waiting())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Write,
    Fetch,
    Push,
    Pull,
    Publish,
    Clone,
    Submodules,
    AddSubmodule,
    Lfs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spec {
    pub kind: Kind,
    pub remote: Option<String>,
    pub writes: bool,
    pub interactive: bool,
}

impl Spec {
    fn of(kind: Kind, remote: Option<String>, writes: bool) -> Self {
        Self { kind, remote, writes, interactive: true }
    }

    pub(crate) fn fetch(remote: Option<String>) -> Self {
        Self::of(Kind::Fetch, remote, false)
    }

    pub(crate) fn push(remote: Option<String>) -> Self {
        Self::of(Kind::Push, remote, false)
    }

    pub(crate) fn pull(remote: Option<String>) -> Self {
        Self::of(Kind::Pull, remote, false)
    }

    pub(crate) fn publish() -> Self {
        Self::of(Kind::Publish, None, true)
    }

    pub(crate) fn submodules() -> Self {
        Self::of(Kind::Submodules, None, true)
    }

    pub(crate) fn adding_submodule() -> Self {
        Self::of(Kind::AddSubmodule, None, true)
    }

    pub(crate) fn lfs() -> Self {
        Self::of(Kind::Lfs, None, true)
    }

    pub(crate) fn clone_from(host: Option<String>) -> Self {
        Self::of(Kind::Clone, host, false)
    }

    pub(crate) fn background(mut self) -> Self {
        self.interactive = false;
        self
    }

    pub(crate) fn writing(mut self) -> Self {
        self.writes = true;
        self
    }

    fn remote(&self) -> &str {
        self.remote.as_deref().unwrap_or("the remote")
    }
}

pub(crate) struct Job {
    pub id: u64,
    pub spec: Spec,
    pub title: String,
    pub link: Link,
    pub notice: Option<u64>,
    pub waiting: bool,
}

#[derive(Default)]
pub(crate) struct State {
    pub write: Option<Job>,
    pub network: Option<Job>,
    next: u64,
    polling: bool,
    pub(crate) writes: u64,
    pub(crate) queued: Option<Kind>,
}

impl State {
    fn job(&mut self, spec: Spec, title: &str, link: Link, notice: Option<u64>) -> Job {
        self.next += 1;
        Job { id: self.next, spec, title: title.to_owned(), link, notice, waiting: false }
    }

    pub(crate) fn start_write(&mut self, title: &str, link: Link, notice: Option<u64>) -> u64 {
        let job = self.job(Spec::of(Kind::Write, None, true).background(), title, link, notice);
        let id = job.id;
        self.write = Some(job);
        id
    }

    pub(crate) fn start_network(&mut self, spec: Spec, title: &str, link: Link, notice: Option<u64>) -> u64 {
        if spec.writes {
            self.start_write(title, link.clone(), None);
        }
        let job = self.job(spec, title, link, notice);
        let id = job.id;
        self.network = Some(job);
        id
    }

    pub(crate) fn end_write(&mut self) -> Option<Job> {
        let job = self.write.take();
        if job.is_some() {
            self.writes += 1;
        }
        job
    }

    pub(crate) fn end_network(&mut self, id: u64) -> Option<Job> {
        let job = self.network.take_if(|job| job.id == id)?;
        if job.spec.writes {
            self.end_write();
        }
        Some(job)
    }

    pub(crate) fn refusal(&self) -> Option<String> {
        let job = self.network.as_ref()?;
        let kind = if job.spec.interactive { job.spec.kind } else { self.queued? };
        Some(
            match kind {
                Kind::Fetch => "A fetch is already running",
                Kind::Push => "A push is already running",
                Kind::Pull => "A pull is already running",
                Kind::Clone => "A clone is already running",
                Kind::Submodules => "Submodules are being updated",
                Kind::AddSubmodule => "A submodule is being added",
                Kind::Lfs => "Git LFS is already at work",
                Kind::Publish | Kind::Write => "Wait for publishing to finish",
            }
            .to_owned(),
        )
    }

    pub(crate) fn idle(&self) -> bool {
        self.write.is_none() && self.network.is_none()
    }

    pub(crate) fn background(&self) -> Option<&Job> {
        self.network.as_ref().filter(|job| !job.spec.interactive)
    }

    pub(crate) fn idle_but_background(&self) -> bool {
        self.write.is_none() && (self.network.is_none() || self.background().is_some() && self.queued.is_none())
    }

    pub(crate) fn cancellable(&self) -> Option<u64> {
        self.network.as_ref().filter(|job| job.link.cancellable() && !job.link.cancelled()).map(|job| job.id)
    }
}

pub(crate) fn cancelled(spec: &Spec) -> Notice {
    match spec.kind {
        Kind::Fetch => Notice::new(Tone::Info, "Fetch cancelled", "Refs that were already updated stay updated."),
        Kind::Push => Notice::new(
            Tone::Info,
            "Push cancelled",
            format!("It may or may not have reached {}. Fetch to check.", spec.remote()),
        )
        .offer("Fetch", view::Action::Sync(crate::sync::Action::FetchAll))
        .sticky(),
        Kind::Pull => Notice::new(Tone::Info, "Pull cancelled", "Nothing was changed locally."),
        Kind::Clone => Notice::new(Tone::Info, "Clone cancelled", "Nothing it cloned was kept."),
        Kind::Submodules => Notice::new(Tone::Info, "Submodule update cancelled", "Submodules that were already updated stay updated."),
        Kind::AddSubmodule => {
            Notice::new(Tone::Info, "Adding the submodule cancelled", "Look at the staged changes to see whether Git added anything.")
        }
        Kind::Lfs => Notice::new(Tone::Info, "Git LFS cancelled", "Files it already downloaded stay downloaded."),
        Kind::Publish | Kind::Write => Notice::new(Tone::Info, "Cancelled", ""),
    }
}

fn shown(job: &Job, now: Instant) -> (String, String, Option<u8>, Option<u64>) {
    let (progress, heard) = job.link.snapshot();
    let asking = job.link.asking();
    let hook = job.link.hook();
    let silent = job.spec.kind != Kind::Write
        && !job.waiting
        && !asking
        && hook.is_none()
        && job.link.cancellable()
        && now.saturating_duration_since(heard) >= SILENCE;
    let title = if silent {
        format!("No response from {} for {} seconds", job.spec.remote(), SILENCE.as_secs())
    } else if let Some((name, _)) = &hook {
        format!("Running {name}…")
    } else {
        job.title.clone()
    };
    let cancelling = job.link.cancelled();
    let detail = if cancelling {
        "Cancelling…".to_owned()
    } else if job.waiting {
        "Fetched. Waiting for the current operation to finish…".to_owned()
    } else if asking {
        "Waiting for your answer…".to_owned()
    } else if let Some((_, since)) = &hook {
        hook_detail(job.link.latest(), now.saturating_duration_since(*since))
    } else {
        progress.as_ref().map(Progress::text).unwrap_or_default()
    };
    let bar = progress.and_then(|progress| progress.percent).filter(|_| !cancelling && !job.waiting && hook.is_none());
    let cancel = (job.link.cancellable() && !cancelling).then_some(job.id);
    (title, detail, bar, cancel)
}

pub(crate) const ELAPSED_AFTER: Duration = Duration::from_secs(3);

fn hook_detail(latest: Option<String>, elapsed: Duration) -> String {
    let mut lines: Vec<String> = latest.into_iter().collect();
    if elapsed >= ELAPSED_AFTER {
        lines.push(format!("Running for {}", elapsed_text(elapsed)));
    }
    lines.join("\n")
}

pub(crate) fn elapsed_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {:02} s", seconds / 60, seconds % 60)
    }
}

pub(crate) fn tick(ui: &mut Ui, now: Instant) -> bool {
    let mut changed = false;
    for job in [ui.jobs.network.as_ref(), ui.jobs.write.as_ref()].into_iter().flatten() {
        let Some(id) = job.notice else { continue };
        let (title, detail, bar, cancel) = shown(job, now);
        changed |= ui.notices.progressed(id, title, detail, bar, cancel);
    }
    changed
}

pub(crate) fn watch(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.jobs.polling {
        return;
    }
    tab.ui.jobs.polling = true;
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let going = tab.update(cx, |tab, cx| {
                if tick(&mut tab.ui, Instant::now()) | askpass::poll(tab, cx) {
                    cx.notify();
                }
                let going = !tab.ui.jobs.idle();
                tab.ui.jobs.polling = going;
                going
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Cancel(u64),
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Cancel(id) => {
            let jobs = [tab.ui.jobs.network.as_ref(), tab.ui.jobs.write.as_ref()];
            let Some(job) = jobs.into_iter().flatten().find(|job| job.id == id) else { return };
            if job.link.cancel() {
                tick(&mut tab.ui, Instant::now());
            }
        }
    }
    cx.notify();
}

pub(crate) fn needs_network(action: &view::Action) -> bool {
    use crate::sync::Action as S;
    match action {
        view::Action::Sync(action) => !matches!(action, S::ToggleOptions | S::Dismiss | S::SetDefault(_) | S::FastForward { .. } | S::TogglePrune),
        view::Action::Tag(action) => matches!(action, crate::tag::Action::Push { .. }),
        view::Action::Tracking(action) => matches!(action, crate::tracking::Action::RestoreRemote { .. }),
        view::Action::Clone(action) => matches!(action, crate::clone::Action::Again(_)),
        _ => false,
    }
}


pub(crate) fn bar(percent: u8) -> Div {
    div()
        .mt(px(4.))
        .h(px(3.))
        .w_full()
        .rounded(px(2.))
        .bg(rgb(LINE_STRONG))
        .child(div().h_full().rounded(px(2.)).bg(rgb(ACCENT)).w(relative(f32::from(percent.min(100)) / 100.)))
}

pub(crate) fn cancel_button<T: view::Desk>(id: u64, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    kit::btn(("job-cancel", id), "Cancel").small().on(view::Action::Jobs(Action::Cancel(id)), cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_give_their_phase_percentage_and_counts() {
        let receiving = Progress::parse("Receiving objects:  45% (450/1000), 1.20 MiB | 600.00 KiB/s").unwrap();
        assert_eq!(receiving.phase, "Receiving objects");
        assert_eq!(receiving.percent, Some(45));
        assert_eq!(receiving.detail, "450 of 1000, 1.20 MiB | 600.00 KiB/s");
        assert_eq!(receiving.text(), "Receiving objects 45% · 450 of 1000, 1.20 MiB | 600.00 KiB/s");
        let counting = Progress::parse("remote: Counting objects: 100% (3/3), done.        ").unwrap();
        assert_eq!((counting.phase.as_str(), counting.percent, counting.detail.as_str()), ("Counting objects", Some(100), "3 of 3"));
        let enumerating = Progress::parse("remote: Enumerating objects: 14, done.").unwrap();
        assert_eq!((enumerating.percent, enumerating.detail.as_str()), (None, "14"));
        let writing = Progress::parse("Writing objects: 100% (3/3), 250 bytes | 250.00 KiB/s, done.").unwrap();
        assert_eq!(writing.detail, "3 of 3, 250 bytes | 250.00 KiB/s");
        let rebasing = Progress::parse("Rebasing (3/7)").unwrap();
        assert_eq!((rebasing.phase.as_str(), rebasing.percent, rebasing.text().as_str()), ("Rebasing", Some(42), "Rebasing 42% · commit 3 of 7"));
        let downloading = Progress::parse("Downloading LFS objects:  50% (1/2), 6.2 MB | 1.3 MB/s").unwrap();
        assert_eq!(downloading.text(), "Downloading LFS objects 50% · 1 of 2, 6.2 MB | 1.3 MB/s");
        for other in [
            "remote: Create a pull request for 'topic' on GitHub by visiting:",
            "fatal: could not read from remote repository.",
            "Rebasing (8/7)",
            "Successfully rebased and updated refs/heads/side.",
            "",
        ] {
            assert_eq!(Progress::parse(other), None, "{other}");
        }
        assert!(is_progress("remote: Total 14 (delta 1), reused 0 (delta 0), pack-reused 0"));
        assert!(is_progress("Resolving deltas: 100% (1/1), done."));
        assert!(!is_progress("To ../bare.git"));
    }

    #[test]
    fn a_job_reports_progress_and_cancels_only_while_it_talks_to_a_remote() {
        let link = Link::new(true);
        assert!(Link::current().is_none());
        link.enter(|| {
            let current = Link::current().unwrap();
            current.heard("Receiving objects:  10% (1/10)");
            current.heard("warning: something else");
        });
        assert!(Link::current().is_none());
        assert_eq!(link.snapshot().0.unwrap().percent, Some(10));
        link.hold();
        assert!(!link.cancel() && !link.cancelled());
        let fetch = Link::new(true);
        assert!(fetch.cancel() && fetch.cancelled());
        assert!(!Link::new(false).cancel());
    }

    #[test]
    fn the_slots_refuse_a_second_network_job_and_say_which_one_runs() {
        let mut jobs = State::default();
        assert!(jobs.idle() && jobs.refusal().is_none());
        let fetch = jobs.start_network(Spec::fetch(Some("origin".into())), "Fetching…", Link::new(true), None);
        assert_eq!(jobs.refusal().as_deref(), Some("A fetch is already running"));
        assert!(jobs.write.is_none());
        assert_eq!(jobs.cancellable(), Some(fetch));
        jobs.end_network(fetch).unwrap();
        assert!(jobs.idle());
        let delete = jobs.start_network(Spec::push(None).writing(), "Deleting…", Link::new(true), None);
        assert!(jobs.write.is_some());
        assert_eq!(jobs.refusal().as_deref(), Some("A push is already running"));
        let writes = jobs.writes;
        assert!(jobs.end_network(delete + 1).is_none());
        jobs.end_network(delete).unwrap();
        assert!(jobs.idle() && jobs.writes == writes + 1);
    }

    #[test]
    fn a_quiet_remote_is_named_after_twenty_seconds_and_cancel_says_what_is_left() {
        let mut jobs = State::default();
        let link = Link::new(true);
        jobs.start_network(Spec::fetch(Some("origin".into())), "Fetching all remotes…", link.clone(), Some(1));
        let job = jobs.network.as_ref().unwrap();
        let (_, at) = link.snapshot();
        let (title, detail, bar, cancel) = shown(job, at + Duration::from_secs(5));
        assert_eq!((title.as_str(), detail.as_str(), bar, cancel), ("Fetching all remotes…", "", None, Some(job.id)));
        let (title, ..) = shown(job, at + SILENCE);
        assert_eq!(title, "No response from origin for 20 seconds");
        link.heard("Receiving objects:  50% (5/10)");
        let (title, detail, bar, _) = shown(job, Instant::now());
        assert_eq!((title.as_str(), detail.as_str(), bar), ("Fetching all remotes…", "Receiving objects 50% · 5 of 10", Some(50)));
        link.cancel();
        let (_, detail, bar, cancel) = shown(job, Instant::now());
        assert_eq!((detail.as_str(), bar, cancel), ("Cancelling…", None, None));

        let fetch = cancelled(&Spec::fetch(None));
        assert_eq!((fetch.title.as_str(), fetch.detail.as_str()), ("Fetch cancelled", "Refs that were already updated stay updated."));
        let push = cancelled(&Spec::push(Some("origin".into())));
        assert_eq!(push.text(), "Push cancelled — It may or may not have reached origin. Fetch to check.");
        assert_eq!(push.actions.len(), 1);
        let pull = cancelled(&Spec::pull(None));
        assert_eq!(pull.text(), "Pull cancelled — Nothing was changed locally.");
        let clone = cancelled(&Spec::clone_from(Some("github.com".into())));
        assert_eq!(clone.text(), "Clone cancelled — Nothing it cloned was kept.");
    }

    #[test]
    fn a_commit_names_the_hook_it_runs_shows_its_latest_line_and_time_and_offers_cancel_until_post_commit() {
        let mut jobs = State::default();
        let link = Link::new(false);
        let id = jobs.start_write("Committing…", link.clone(), Some(1));
        let job = jobs.write.as_ref().unwrap();
        let now = Instant::now();
        assert_eq!(shown(job, now), ("Committing…".to_owned(), String::new(), None, None));
        link.expect_hooks(&["pre-commit".to_owned(), "commit-msg".to_owned()]);
        link.release();
        let (title, detail, _, cancel) = shown(job, Instant::now());
        assert_eq!((title.as_str(), detail.as_str(), cancel), ("Running pre-commit…", "", Some(id)));
        link.hook_started("pre-commit");
        link.heard("hint: The '.git/hooks/commit-msg' hook was ignored because it's not set as executable.");
        link.heard("lint: checking 40 files");
        let (_, since) = link.hook().unwrap();
        assert_eq!(shown(job, since + Duration::from_secs(2)).1, "lint: checking 40 files");
        assert_eq!(shown(job, since + Duration::from_secs(3)).1, "lint: checking 40 files\nRunning for 3 s");
        assert_eq!(shown(job, since + Duration::from_secs(75)).1, "lint: checking 40 files\nRunning for 1 min 15 s");
        link.hook_ended();
        assert_eq!(shown(job, Instant::now()).0, "Committing…");
        link.printed(b"quiet\n");
        assert_eq!(link.latest().as_deref(), Some("lint: checking 40 files"));
        link.hook_started("commit-msg");
        link.printed(b"msg: ok\nmsg: subject fine\n");
        let (title, detail, _, cancel) = shown(job, Instant::now());
        assert_eq!((title.as_str(), detail.as_str(), cancel), ("Running commit-msg…", "msg: subject fine", Some(id)));
        link.hook_ended();
        link.hook_started("post-commit");
        let (title, _, _, cancel) = shown(job, Instant::now());
        assert_eq!((title.as_str(), cancel), ("Running post-commit…", None));
        assert!(!link.cancel());
    }

    #[test]
    fn a_quiet_hook_during_a_push_is_not_called_a_quiet_remote() {
        let mut jobs = State::default();
        let link = Link::new(true);
        jobs.start_network(Spec::push(Some("origin".into())), "Pushing to upstream…", link.clone(), Some(1));
        link.hook_started("pre-push");
        let (_, since) = link.hook().unwrap();
        let job = jobs.network.as_ref().unwrap();
        let (title, detail, _, cancel) = shown(job, since + SILENCE);
        assert_eq!((title.as_str(), detail.as_str(), cancel), ("Running pre-push…", "Running for 20 s", Some(job.id)));
        link.hook_ended();
        assert_eq!(shown(job, Instant::now()).0, "Pushing to upstream…");
    }

    #[test]
    fn a_background_fetch_refuses_nothing_until_a_job_waits_to_take_its_place() {
        let mut jobs = State::default();
        let link = Link::new(true);
        let id = jobs.start_network(Spec::fetch(None).background(), "Fetching in the background…", link, None);
        assert!(jobs.background().is_some_and(|job| job.id == id) && !jobs.idle());
        assert_eq!(jobs.refusal(), None);
        assert!(jobs.idle_but_background());
        jobs.queued = Some(Kind::Push);
        assert_eq!(jobs.refusal().as_deref(), Some("A push is already running"));
        assert!(!jobs.idle_but_background());
        jobs.queued = None;
        jobs.end_network(id).unwrap();
        jobs.start_network(Spec::fetch(None), "Fetching all remotes…", Link::new(true), None);
        assert!(jobs.background().is_none() && !jobs.idle_but_background());
        assert_eq!(jobs.refusal().as_deref(), Some("A fetch is already running"));
    }

    #[test]
    fn a_pull_bringing_in_what_it_fetched_is_never_called_unanswered() {
        let mut jobs = State::default();
        let link = Link::new(true);
        jobs.start_network(Spec::pull(Some("origin".into())), "Pulling from upstream…", link.clone(), Some(1));
        let (_, at) = link.snapshot();
        let job = jobs.network.as_ref().unwrap();
        assert_eq!(shown(job, at + SILENCE).0, "No response from origin for 20 seconds");
        link.hold();
        let (title, _, _, cancel) = shown(job, at + SILENCE * 3);
        assert_eq!((title.as_str(), cancel), ("Pulling from upstream…", None));
    }
}
