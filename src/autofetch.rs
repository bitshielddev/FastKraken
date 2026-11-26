use crate::{
    Tab, activity, explain, jobs, kit,
    notice::Failure,
    repository::Repository,
    settings,
    theme::*,
    view::{self, Desk, Ui},
};
use gpui::{Context, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::time::{Duration, Instant, UNIX_EPOCH};

const CHECK: Duration = Duration::from_secs(15);
pub(crate) const BACKOFF_MINUTES: u32 = 30;
const TITLE: &str = "Fetching in the background…";

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failed {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) entry: Option<u64>,
}

impl Failed {
    pub(crate) fn tip(&self) -> String {
        let detail = self.detail.strip_prefix("Fetch failed.").map_or(self.detail.as_str(), str::trim_start);
        let explained = match (self.title.is_empty(), detail.is_empty()) {
            (_, true) => self.title.clone(),
            (true, false) => detail.to_owned(),
            (false, false) => format!("{}. {detail}", self.title.trim_end_matches('.')),
        };
        let more = if self.entry.is_some() {
            "Fetch to try again. Click to show the command in the activity log."
        } else {
            "Fetch to try again."
        };
        format!("Automatic fetch failed: {explained}\n{more}")
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) fetched: Option<i64>,
    since: Option<Instant>,
    pub(crate) failures: u32,
    pub(crate) failed: Option<Failed>,
    pub(crate) due: bool,
}

impl State {
    fn resumes(&mut self, minutes: u32, holding: bool) -> bool {
        if minutes == 0 {
            self.due = false;
        }
        self.due && !holding
    }
}

pub(crate) fn interval(minutes: u32, failures: u32) -> Option<Duration> {
    if minutes == 0 {
        return None;
    }
    let longest = minutes.max(BACKOFF_MINUTES);
    let doubled = (0..failures.min(16)).fold(minutes, |every, _| every.saturating_mul(2).min(longest));
    Some(Duration::from_secs(u64::from(doubled) * 60))
}

pub(crate) fn held(jobs: &jobs::State, operating: bool) -> bool {
    jobs.write.is_some() || jobs.network.is_some() || operating
}

fn waits(tab: &Tab) -> bool {
    held(&tab.ui.jobs, tab.ui.operation.active()) || tab.waiting.is_some() || tab.queued.is_some() || tab.ui.opening.is_some()
}

pub(crate) fn fetched_at(repo: &Repository) -> Option<i64> {
    [repo.git_dir(), repo.common_dir()]
        .into_iter()
        .filter_map(|dir| dir.join("FETCH_HEAD").metadata().ok()?.modified().ok())
        .max()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64)
}

pub(crate) fn last_fetched(fetched: Option<i64>, now: i64) -> String {
    let Some(fetched) = fetched else { return "Not fetched yet".to_owned() };
    let age = (now - fetched).max(0);
    let (count, unit) = match age {
        0..60 => return "Last fetched just now".to_owned(),
        60..3_600 => (age / 60, "minute"),
        3_600..86_400 => (age / 3_600, "hour"),
        _ => (age / 86_400, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("Last fetched {count} {unit}{plural} ago")
}

pub(crate) fn line(ui: &Ui) -> String {
    if ui.jobs.background().is_some() {
        return TITLE.to_owned();
    }
    last_fetched(ui.autofetch.fetched, view::now())
}

pub(crate) fn arm(tab: &mut Tab, cx: &mut Context<Tab>) {
    tab.ui.autofetch.since = Some(Instant::now());
    if cfg!(test) || crate::acceptance::replaying() || tab.bench.is_some() {
        return;
    }
    let epoch = tab.epoch;
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(CHECK).await;
            let going = tab.update(cx, |tab, cx| {
                if tab.epoch != epoch {
                    return false;
                }
                check(tab, Instant::now(), cx);
                true
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

fn check(tab: &mut Tab, now: Instant, cx: &mut Context<Tab>) {
    if tab.ui.autofetch.due {
        resume(tab, cx);
        return;
    }
    let Some(every) = interval(settings::settings(cx).fetch_minutes(), tab.ui.autofetch.failures) else { return };
    if tab.ui.autofetch.since.is_some_and(|since| now.saturating_duration_since(since) >= every) {
        tick(tab, cx);
    }
}

fn tick(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.repo.is_none() || tab.ui.refs.remotes.is_empty() {
        tab.ui.autofetch.since = Some(Instant::now());
        return;
    }
    if waits(tab) {
        tab.ui.autofetch.due = true;
        return;
    }
    start(tab, cx);
}

pub(crate) fn resume(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (minutes, holding) = (settings::settings(cx).fetch_minutes(), waits(tab));
    if tab.ui.autofetch.resumes(minutes, holding) {
        tick(tab, cx);
    }
}

pub(crate) fn by_hand(tab: &mut Tab, fetched: bool) {
    let state = &mut tab.ui.autofetch;
    state.since = Some(Instant::now());
    state.failures = 0;
    state.failed = None;
    state.due = false;
    if fetched {
        state.fetched = Some(view::now());
    }
}

fn start(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.autofetch.due = false;
    tab.ui.autofetch.since = Some(Instant::now());
    let remotes = &tab.ui.refs.remotes;
    let remote = (remotes.len() == 1).then(|| remotes[0].clone());
    let about = explain::About::new(remote.clone(), None);
    let link = jobs::Link::new(true);
    let id = tab.ui.jobs.start_network(jobs::Spec::fetch(remote).background(), TITLE, link.clone(), None);
    let prune = tab.ui.sync.prune;
    let epoch = tab.epoch;
    let work = move || {
        link.enter(|| {
            crate::sync::fetch_every(&repo, prune)
                .map_err(|error| about.explain(&repo, Failure::from(format!("Fetch Failed — {error}"))))
        })
    };
    Tab::work(cx, work, move |tab, result, cx| {
        if tab.epoch != epoch {
            return;
        }
        let Some(job) = tab.ui.jobs.end_network(id) else { return };
        let entry = tab.trail.entry(true);
        let state = &mut tab.ui.autofetch;
        state.since = Some(Instant::now());
        match result {
            Ok(_) => {
                tab.ui.pulls.due = true;
                let state = &mut tab.ui.autofetch;
                state.fetched = Some(view::now());
                state.failures = 0;
                state.failed = None;
            }
            Err(_) if job.link.cancelled() => {}
            Err(failure) => {
                let shown = failure.notice();
                state.failures = state.failures.saturating_add(1);
                state.failed = Some(Failed { title: shown.title, detail: shown.detail, entry });
            }
        }
        tab.refresh(true, cx);
        tab.ui.jobs.queued = None;
        if let Some(next) = tab.queued.take() {
            next(tab, cx);
        }
        tab.resume(cx);
        cx.notify();
    });
    cx.notify();
}

pub(crate) fn stop(tab: &mut Tab) {
    if let Some(job) = tab.ui.jobs.background() {
        job.link.cancel();
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Tick => tick(tab, cx),
    }
    cx.notify();
}


pub(crate) fn warning<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let failed = ui.autofetch.failed.as_ref()?;
    let entry = failed.entry;
    Some(
        div()
            .id("autofetch-warning")
            .size(px(TARGET))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .hover(|s| s.bg(rgb(palette().hover)))
            .child(kit::icon("alert").size(px(12.)).text_color(rgb(palette().modified)))
            .tooltip(kit::tip(failed.tip()))
            .when_some(entry, |d, entry| {
                d.cursor_pointer().on_click(cx.listener(move |app, _, window, cx| {
                    cx.stop_propagation();
                    app.dispatch(view::Action::Activity(activity::Action::Show(entry)), window, cx)
                }))
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    #[test]
    fn the_interval_doubles_after_failures_up_to_thirty_minutes_and_zero_is_off() {
        let minutes = |every, failures| interval(every, failures).map(|wait| wait.as_secs() / 60);
        assert_eq!(minutes(0, 0), None);
        assert_eq!(minutes(0, 3), None);
        assert_eq!(minutes(5, 0), Some(5));
        assert_eq!(minutes(5, 1), Some(10));
        assert_eq!(minutes(5, 2), Some(20));
        assert_eq!(minutes(5, 3), Some(30));
        assert_eq!(minutes(5, 40), Some(30));
        assert_eq!(minutes(1, 4), Some(16));
        assert_eq!(minutes(45, 0), Some(45));
        assert_eq!(minutes(45, 2), Some(45));
    }

    #[test]
    fn a_tick_during_a_write_or_another_network_job_or_an_operation_waits() {
        let mut jobs = jobs::State::default();
        assert!(!held(&jobs, false));
        assert!(held(&jobs, true), "a merge or rebase in progress holds the fetch");
        jobs.start_write("Committing…", jobs::Link::new(false), None);
        assert!(held(&jobs, false), "a commit holds the fetch");
        jobs.end_write();
        assert!(!held(&jobs, false));
        let push = jobs.start_network(jobs::Spec::push(None), "Pushing…", jobs::Link::new(true), None);
        assert!(held(&jobs, false), "a push holds the fetch");
        jobs.end_network(push);
        let fetch = jobs.start_network(jobs::Spec::fetch(None).background(), TITLE, jobs::Link::new(true), None);
        assert!(held(&jobs, false), "a fetch on the timer does not start twice");
        jobs.end_network(fetch);
        assert!(jobs.idle() && !held(&jobs, false));
    }

    #[test]
    fn a_fetch_that_waited_starts_once_nothing_holds_the_repository_unless_turned_off() {
        let mut state = State { due: true, ..State::default() };
        assert!(!state.resumes(5, true), "a write still holds it");
        assert!(state.due, "it still waits");
        assert!(state.resumes(5, false));
        let mut state = State { due: true, ..State::default() };
        assert!(!state.resumes(0, false));
        assert!(!state.due);
        let mut state = State { due: true, ..State::default() };
        assert!(!state.resumes(0, true));
        assert!(!state.due && !state.resumes(5, false), "turned back on, nothing waits");
        assert!(!State::default().resumes(5, false), "nothing waited");
    }

    #[test]
    fn the_sync_tooltip_says_when_every_remote_was_last_fetched() {
        assert_eq!(last_fetched(None, 1_000), "Not fetched yet");
        assert_eq!(last_fetched(Some(1_000), 1_030), "Last fetched just now");
        assert_eq!(last_fetched(Some(1_000), 900), "Last fetched just now");
        assert_eq!(last_fetched(Some(1_000), 1_060), "Last fetched 1 minute ago");
        assert_eq!(last_fetched(Some(1_000), 1_000 + 3 * 60 + 20), "Last fetched 3 minutes ago");
        assert_eq!(last_fetched(Some(0), 3_600), "Last fetched 1 hour ago");
        assert_eq!(last_fetched(Some(0), 5 * 3_600), "Last fetched 5 hours ago");
        assert_eq!(last_fetched(Some(0), 2 * 86_400 + 5), "Last fetched 2 days ago");
    }

    #[test]
    fn the_warning_gives_the_explanation_in_one_line() {
        let failed = Failed {
            title: "origin does not point at a repository you can reach".into(),
            detail: "Fetch failed. Check the address, and that this account has access.".into(),
            entry: Some(4),
        };
        assert_eq!(
            failed.tip(),
            "Automatic fetch failed: origin does not point at a repository you can reach. Check the address, and that this account has access.\nFetch to try again. Click to show the command in the activity log."
        );
        let plain = Failed { title: "Fetch Failed".into(), detail: String::new(), entry: None };
        assert_eq!(plain.tip(), "Automatic fetch failed: Fetch Failed\nFetch to try again.");
    }

    struct Remote {
        local: Fixture,
        peer: Fixture,
        bare: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Remote {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bare = dir.path().join("origin.git");
            let peer = Fixture::new();
            peer.git(&["commit", "--allow-empty", "-qm", "Start the trail"]);
            peer.git(&["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
            peer.git(&["push", "-q", bare.to_str().unwrap(), "main", "main:refs/heads/spur"]);
            let local = Fixture::new();
            local.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
            local.git(&["fetch", "-q", "origin"]);
            local.git(&["switch", "-q", "-c", "main", "--track", "origin/main"]);
            Self { local, peer, bare, _dir: dir }
        }

        fn at(&self, name: &str) -> Option<String> {
            let output = self.local.repo.command().args(["rev-parse", "--verify", "-q", name]).output().unwrap();
            output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
    }

    fn timed(repo: &Repository, prune: bool) -> Result<String, Failure> {
        let about = explain::About::new(Some("origin".into()), None);
        jobs::Link::new(true).enter(|| {
            crate::sync::fetch_every(repo, prune).map_err(|error| about.explain(repo, Failure::from(format!("Fetch Failed — {error}"))))
        })
    }

    #[test]
    fn a_timed_fetch_moves_remote_branches_prunes_and_leaves_the_branch_index_and_files() {
        let r = Remote::new();
        let head = r.at("HEAD").unwrap();
        fs::write(r.local.dir.join("notes.txt"), "staged\n").unwrap();
        r.local.git(&["add", "notes.txt"]);
        fs::write(r.local.dir.join("notes.txt"), "unstaged\n").unwrap();
        let index = r.local.git(&["ls-files", "--stage", "-z"]);
        assert_eq!(fetched_at(&r.local.repo).map(|_| ()), Some(()), "the first fetch wrote FETCH_HEAD");
        fs::remove_file(r.local.repo.git_dir().join("FETCH_HEAD")).unwrap();
        assert_eq!(fetched_at(&r.local.repo), None);

        r.peer.git(&["commit", "--allow-empty", "-qm", "Mark the lookout"]);
        r.peer.git(&["push", "-q", r.bare.to_str().unwrap(), "main", ":refs/heads/spur"]);
        let incoming = String::from_utf8(r.peer.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let before = view::now();
        assert_eq!(timed(&r.local.repo, true).unwrap(), "1 branch updated, 1 branch removed.");
        assert_eq!(r.at("refs/remotes/origin/main").as_deref(), Some(incoming.as_str()));
        assert_eq!(r.at("refs/remotes/origin/spur"), None, "the prune setting is honoured");
        assert_eq!(r.at("HEAD").as_deref(), Some(head.as_str()), "the branch is not pulled");
        assert_eq!(r.local.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(fs::read_to_string(r.local.dir.join("notes.txt")).unwrap(), "unstaged\n");
        assert!(fetched_at(&r.local.repo).is_some_and(|at| at >= before - 1));

        r.peer.git(&["push", "-q", r.bare.to_str().unwrap(), "main:refs/heads/spur"]);
        timed(&r.local.repo, true).unwrap();
        r.peer.git(&["push", "-q", r.bare.to_str().unwrap(), ":refs/heads/spur"]);
        assert_eq!(timed(&r.local.repo, false).unwrap(), "Everything is up to date.");
        assert!(r.at("refs/remotes/origin/spur").is_some());
    }

    #[test]
    fn a_timed_fetch_from_a_remote_that_went_fails_with_the_explanation_and_changes_nothing() {
        let r = Remote::new();
        let tracking = r.at("refs/remotes/origin/main");
        fs::rename(&r.bare, r.bare.with_extension("moved")).unwrap();
        let (result, trail) = activity::traced(|| timed(&r.local.repo, true));
        let shown = result.unwrap_err().notice();
        assert_eq!(shown.title, "origin does not point at a repository you can reach");
        assert_eq!(shown.detail, "Fetch failed. Check the address, and that this account has access.");
        assert_eq!(r.at("refs/remotes/origin/main"), tracking);
        let entry = activity::find(trail.entry(true).unwrap()).unwrap();
        assert!(!entry.succeeded() && entry.argv.iter().any(|arg| arg == "fetch"), "{entry:?}");
        assert!(entry.stderr.contains("does not appear to be a git repository"), "{}", entry.stderr);
    }
}
