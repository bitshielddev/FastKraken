use crate::{
    jobs::Link,
    notice::{Failure, Outcome},
    repository::{Repository, Run},
    view::Tone,
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub(crate) const COMMIT: &[&str] = &["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit"];
pub(crate) const AMEND: &[&str] = &["pre-commit", "prepare-commit-msg", "commit-msg", "post-commit", "post-rewrite"];

const HOOKED: &[&str] = &["am", "checkout", "cherry-pick", "commit", "merge", "pull", "push", "rebase", "revert", "switch"];
const POLL: Duration = Duration::from_millis(40);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    pub hook: String,
    pub code: Option<i32>,
}

impl Ran {
    fn failed(&self) -> bool {
        self.code.is_some_and(|code| code != 0)
    }
}

impl Repository {
    pub(crate) fn runnable_hooks(&self, events: &[&str]) -> Vec<String> {
        events
            .iter()
            .filter(|event| {
                self.read_command()
                    .args(["hook", "list", "-z", "--", event])
                    .run()
                    .is_ok_and(|output| output.status.success() && output.stdout.iter().any(|&b| b != 0))
            })
            .map(|event| (*event).to_owned())
            .collect()
    }
}

pub(crate) fn may_run(command: &Command) -> bool {
    subcommand(command).is_some_and(|name| HOOKED.contains(&name))
}

fn subcommand(command: &Command) -> Option<&str> {
    let mut args = command.get_args();
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "-C" {
            args.next();
            continue;
        }
        let arg = arg.to_str()?;
        if !arg.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

fn exits_with_post_hooks(command: &Command) -> bool {
    matches!(subcommand(command), Some("checkout" | "switch"))
}

pub(crate) struct Trace {
    path: tempfile::TempPath,
    link: Option<Link>,
    post_hooks_count: bool,
    stop: Arc<AtomicBool>,
    watcher: Option<thread::JoinHandle<()>>,
}

impl Trace {
    pub(crate) fn attach(command: &mut Command, link: Option<&Link>) -> Option<Self> {
        let path = tempfile::Builder::new().prefix("fastkraken-hooks-").suffix(".json").tempfile().ok()?.into_temp_path();
        let post_hooks_count = exits_with_post_hooks(command);
        command.env("GIT_TRACE2_EVENT", &path);
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = link.cloned().map(|link| {
            let (file, stop) = (path.to_path_buf(), stop.clone());
            thread::spawn(move || watch(&file, &link, &stop))
        });
        Some(Self { path, link: link.cloned(), post_hooks_count, stop, watcher })
    }

    pub(crate) fn finish(mut self) -> Vec<Ran> {
        self.halt();
        let mut events = Events::default();
        let mut bytes = Vec::new();
        if File::open(&self.path).and_then(|mut file| file.read_to_end(&mut bytes)).is_ok() {
            events.feed(&bytes);
            events.feed(b"\n");
        }
        let ran = events.ran();
        if let Some(link) = &self.link {
            let counted: Vec<Ran> =
                ran.iter().filter(|ran| self.post_hooks_count || !ran.hook.starts_with("post-")).cloned().collect();
            link.hooks_ran(&counted);
        }
        ran
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(watcher) = self.watcher.take() {
            watcher.thread().unpark();
            let _ = watcher.join();
        }
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        self.halt();
    }
}

fn watch(path: &Path, link: &Link, stop: &AtomicBool) {
    let mut events = Events::default();
    let mut offset = 0;
    loop {
        let last = stop.load(Ordering::SeqCst);
        if let Ok(mut file) = File::open(path) {
            let mut bytes = Vec::new();
            if file.seek(SeekFrom::Start(offset)).is_ok() && file.read_to_end(&mut bytes).is_ok() {
                offset += bytes.len() as u64;
                for event in events.feed(&bytes) {
                    match event {
                        Event::Started(hook) => link.hook_started(&hook),
                        Event::Ended => link.hook_ended(),
                    }
                }
            }
        }
        if last {
            return;
        }
        thread::park_timeout(POLL);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Started(String),
    Ended,
}

#[derive(Default)]
struct Events {
    partial: Vec<u8>,
    started: BTreeMap<u64, usize>,
    ran: Vec<Ran>,
}

impl Events {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.partial.extend_from_slice(bytes);
        let mut seen = Vec::new();
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            if let Some(event) = self.line(&line) {
                seen.push(event);
            }
        }
        seen
    }

    fn line(&mut self, line: &[u8]) -> Option<Event> {
        let value: serde_json::Value = serde_json::from_slice(line).ok()?;
        if value.get("sid")?.as_str()?.contains('/') {
            return None;
        }
        let child = value.get("child_id")?.as_u64()?;
        match value.get("event")?.as_str()? {
            "child_start" if value.get("child_class")?.as_str()? == "hook" => {
                let hook = value.get("hook_name")?.as_str()?.to_owned();
                self.started.insert(child, self.ran.len());
                self.ran.push(Ran { hook: hook.clone(), code: None });
                Some(Event::Started(hook))
            }
            "child_exit" => {
                let index = self.started.remove(&child)?;
                self.ran[index].code = value.get("code").and_then(serde_json::Value::as_i64).map(|code| code as i32);
                Some(Event::Ended)
            }
            _ => None,
        }
    }

    fn ran(self) -> Vec<Ran> {
        self.ran
    }
}

pub(crate) struct Stop<'a> {
    pub noun: &'a str,
    pub nothing: &'a str,
}

pub(crate) const COMMITTING: Stop<'static> = Stop { noun: "commit", nothing: "No commit was made." };
pub(crate) const PUSHING: Stop<'static> = Stop { noun: "push", nothing: "Nothing was pushed." };

pub(crate) fn stopped(stop: &Stop, ran: Option<&[Ran]>, present: &[String], said: &str) -> Option<Failure> {
    if matches!(crate::explain::classify(said), Some(crate::explain::Cause::NotFound { .. })) {
        return None;
    }
    let (named, code) = match ran {
        Some(ran) => {
            let failed = ran.iter().find(|ran| ran.failed())?;
            (failed.hook.clone(), failed.code)
        }
        None if !present.is_empty() && crate::explain::classify(said).is_none() => (either(present), None),
        None => return None,
    };
    let detail = match code {
        Some(code) => format!("{} It exited with code {code}.", stop.nothing),
        None => stop.nothing.to_owned(),
    };
    let failure = Failure::from(format!("{named} stopped the {}", stop.noun)).output(crate::activity::redact(said)).expanded();
    Some(Failure { plain: Some((format!("{named} stopped the {}", stop.noun), detail)), ..failure })
}

pub(crate) fn named(failure: Failure, ran: &[Ran], stop: Option<&Stop>) -> Failure {
    if failure.plain.is_some() {
        return failure;
    }
    let Some(failed) = ran.iter().find(|ran| ran.failed()) else { return failure };
    let said = failure.output.clone().unwrap_or_else(|| failure.text.clone());
    if matches!(crate::explain::classify(&said), Some(crate::explain::Cause::NotFound { .. })) {
        return failure;
    }
    let code = failed.code.unwrap_or_default();
    let (title, detail) = match stop {
        Some(stop) if !failed.hook.starts_with("post-") => (
            format!("{} stopped the {}", failed.hook, stop.noun),
            format!("{} It exited with code {code}.", stop.nothing),
        ),
        _ if failed.hook.starts_with("post-") => {
            (format!("{} failed", failed.hook), format!("It exited with code {code} after Git had finished."))
        }
        _ => (format!("{} failed", failed.hook), format!("It exited with code {code}.")),
    };
    let output = crate::activity::redact(&failure.output.clone().unwrap_or_else(|| without_title(&failure.text)));
    Failure { plain: Some((title, detail)), ..failure.output(output).expanded() }
}

fn without_title(text: &str) -> String {
    match text.split_once(" — ") {
        Some((title, rest)) if !title.contains('\n') => rest.to_owned(),
        _ => text.to_owned(),
    }
}

fn either(hooks: &[String]) -> String {
    match hooks {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn both(hooks: &[String]) -> String {
    match hooks {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn cancelled(staged: bool) -> Failure {
    let detail = if staged { "No commit was made. Your changes stay staged." } else { "No commit was made." };
    Failure { plain: Some(("Commit cancelled".to_owned(), detail.to_owned())), tone: Tone::Info, ..Failure::from(String::new()) }
}

#[derive(Debug, Default)]
pub(crate) struct Committed {
    pub skipped: Vec<PathBuf>,
    pub hooks: Vec<String>,
    pub printed: String,
    pub stopped: Option<String>,
}

impl Committed {
    pub(crate) fn printed_lines(&self) -> usize {
        self.printed.lines().filter(|line| !line.trim().is_empty()).count()
    }
}

pub(crate) fn printed(said: &str) -> String {
    let said = crate::activity::redact(said);
    said.lines()
        .filter(|line| !line.trim_start().starts_with("hint:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub(crate) fn hooks_that_ran(ran: Option<&[Ran]>, present: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for hook in ran.map(|ran| ran.iter().map(|ran| ran.hook.clone()).collect()).unwrap_or_else(|| present.to_vec()) {
        if !names.contains(&hook) {
            names.push(hook);
        }
    }
    names
}

pub(crate) fn told(mut outcome: Outcome, committed: &Committed) -> Outcome {
    let mut notes = Vec::new();
    if let Some(hook) = &committed.stopped {
        notes.push(format!("{hook} was stopped before it finished."));
    }
    let lines = committed.printed_lines();
    if lines > 0 && !committed.hooks.is_empty() {
        let count = if lines == 1 { "1 line".to_owned() } else { format!("{lines} lines") };
        notes.push(format!("{} printed {count}.", both(&committed.hooks)));
        outcome = outcome.output(committed.printed.clone());
    }
    for note in notes {
        outcome.detail = if outcome.detail.is_empty() { note } else { format!("{}\n{note}", outcome.detail) };
    }
    crate::explain::skipped(outcome, &committed.skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    fn hook(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn hooked(f: &Fixture) -> PathBuf {
        let hooks = f.dir.join(".git/trial-hooks");
        fs::create_dir(&hooks).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        hooks
    }

    fn staged(f: &Fixture, name: &str, text: &str) {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
    }

    fn head(f: &Fixture) -> Option<String> {
        f.repo.status().unwrap().head
    }

    fn gone(pid: libc::pid_t) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn pid(path: &Path) -> libc::pid_t {
        fs::read_to_string(path).unwrap().trim().parse().unwrap()
    }

    fn wait_for(what: &str, ready: impl Fn() -> bool) {
        let start = Instant::now();
        while !ready() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn only_hooks_that_git_would_run_are_listed_from_the_hooks_path_and_configuration() {
        let f = Fixture::new();
        assert!(f.repo.runnable_hooks(COMMIT).is_empty());
        let hooks = hooked(&f);
        hook(&hooks, "pre-commit", "exit 0\n");
        let idle = hook(&hooks, "commit-msg", "exit 0\n");
        fs::set_permissions(&idle, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(hooks.join("pre-commit.sample"), "#!/bin/sh\n").unwrap();
        f.git(&["config", "hook.notes.event", "post-commit"]);
        f.git(&["config", "hook.notes.command", "true"]);
        assert_eq!(f.repo.runnable_hooks(COMMIT), ["pre-commit", "post-commit"]);
        assert_eq!(f.repo.runnable_hooks(&["pre-push"]), Vec::<String>::new());
        f.git(&["config", "--unset", "core.hooksPath"]);
        let default = f.dir.join(".git/hooks");
        fs::create_dir_all(&default).unwrap();
        hook(&default, "prepare-commit-msg", "exit 0\n");
        assert_eq!(f.repo.runnable_hooks(COMMIT), ["prepare-commit-msg", "post-commit"]);
    }

    #[test]
    fn commands_that_can_run_hooks_are_told_apart_from_ones_that_cannot() {
        let f = Fixture::new();
        let mut commit = f.repo.command();
        commit.args(["-c", "user.useConfigOnly=true", "commit", "-m", "push"]);
        assert!(may_run(&commit));
        let mut status = f.repo.read_command();
        status.args(["-c", "commit.x=1", "status", "--porcelain=v2"]);
        assert!(!may_run(&status));
        let mut push = f.repo.command();
        push.arg("-c").arg("remote.origin.mirror=false").args(["push", "--porcelain"]);
        assert!(may_run(&push));
        let mut list = f.repo.read_command();
        list.args(["hook", "list", "pre-commit"]);
        assert!(!may_run(&list));
    }

    #[test]
    fn trace_events_name_each_hook_and_its_exit_code_and_ignore_nested_git() {
        let mut events = Events::default();
        let lines = concat!(
            r#"{"event":"version","sid":"20261124T1-H1-P1","evt":"4"}"#, "\n",
            r#"{"event":"child_start","sid":"20261124T1-H1-P1","child_id":0,"child_class":"hook","hook_name":"pre-commit","argv":[".git/hooks/pre-commit"]}"#, "\n",
            r#"{"event":"child_start","sid":"20261124T1-H1-P1/20261124T2-H1-P2","child_id":0,"child_class":"hook","hook_name":"post-checkout"}"#, "\n",
            r#"{"event":"child_exit","sid":"20261124T1-H1-P1","child_id":0,"pid":9,"code":0}"#, "\n",
            r#"{"event":"child_start","sid":"20261124T1-H1-P1","child_id":1,"child_class":"?","argv":["git","maintenance"]}"#, "\n",
            r#"{"event":"child_exit","sid":"20261124T1-H1-P1","child_id":1,"pid":10,"code":0}"#, "\n",
            r#"{"event":"child_start","sid":"20261124T1-H1-P1","child_id":2,"child_class":"hook","hook_name":"commit-msg"}"#, "\n",
        );
        let (first, rest) = lines.split_at(70);
        assert!(events.feed(first.as_bytes()).is_empty());
        assert_eq!(
            events.feed(rest.as_bytes()),
            [Event::Started("pre-commit".into()), Event::Ended, Event::Started("commit-msg".into())]
        );
        assert_eq!(events.feed(br#"{"event":"child_exit","sid":"20261124T1-H1-P1","child_id":2,"pid":11,"code":3}"#), []);
        assert_eq!(events.feed(b"\nnot json\n"), [Event::Ended]);
        assert_eq!(
            events.ran(),
            [Ran { hook: "pre-commit".into(), code: Some(0) }, Ran { hook: "commit-msg".into(), code: Some(3) }]
        );
    }

    #[test]
    fn a_failing_hook_is_named_keeps_all_its_output_and_leaves_head_and_index_alone() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        let tip = head(&f);
        let hooks = hooked(&f);
        hook(&hooks, "pre-commit", "i=1\nwhile [ $i -le 200 ]; do echo \"lint line $i\"; i=$((i+1)); done\nexit 1\n");
        staged(&f, "camp.txt", "tarp\n");
        let failure = f.repo.commit("Swap the tent").unwrap_err();
        let notice = failure.notice();
        assert_eq!((notice.tone, notice.title.as_str()), (Tone::Error, "pre-commit stopped the commit"));
        assert_eq!(notice.detail, "No commit was made. It exited with code 1.");
        assert!(notice.expanded);
        let output = notice.output.unwrap();
        assert_eq!(output.lines().count(), 200);
        assert!(output.starts_with("lint line 1\n") && output.ends_with("lint line 200"), "{output}");
        assert_eq!(head(&f), tip);
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), b"camp.txt\n");
        assert!(!f.dir.join(".git/index.lock").exists());

        fs::remove_file(hooks.join("pre-commit")).unwrap();
        hook(&hooks, "pre-commit", "echo fine\n");
        hook(&hooks, "commit-msg", "exit 4\n");
        let failure = f.repo.commit("Swap the tent").unwrap_err().notice();
        assert_eq!(failure.title, "commit-msg stopped the commit");
        assert_eq!(failure.detail, "No commit was made. It exited with code 4.");
        assert_eq!(failure.output.as_deref(), Some("fine"));
        assert_eq!(head(&f), tip);

        fs::remove_file(hooks.join("commit-msg")).unwrap();
        let nothing = f.repo.commit_all(tip.as_deref(), Some(std::ffi::OsStr::new("refs/heads/main")), "Again");
        assert!(nothing.is_ok());
        let moved = head(&f);
        assert_ne!(moved, tip);
        let empty = f.repo.commit("Nothing left").unwrap_err().notice();
        assert_ne!(empty.title, "pre-commit stopped the commit", "a commit with nothing staged is not the hook's doing");
        assert_eq!(head(&f), moved);
    }

    #[test]
    fn a_hook_that_prints_and_passes_is_counted_and_its_output_kept() {
        let f = Fixture::new();
        let hooks = hooked(&f);
        hook(&hooks, "pre-commit", "echo 'lint: 3 files look fine'\necho 'tests: 12 passed' >&2\n");
        hook(&hooks, "post-commit", "echo 'notified the team'\n");
        staged(&f, "camp.txt", "tent\n");
        let committed = f.repo.commit("Pitch the tent").unwrap();
        assert_eq!(committed.hooks, ["pre-commit", "post-commit"]);
        assert_eq!(committed.printed, "lint: 3 files look fine\ntests: 12 passed\nnotified the team");
        let outcome = told(Outcome::titled("Changes committed", "Pitch the tent"), &committed);
        assert_eq!(outcome.detail, "Pitch the tent\npre-commit and post-commit printed 3 lines.");
        assert_eq!(outcome.output.as_deref(), Some(committed.printed.as_str()));
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Pitch the tent\n");

        fs::remove_file(hooks.join("post-commit")).unwrap();
        hook(&hooks, "pre-commit", "exit 0\n");
        staged(&f, "camp.txt", "tarp\n");
        let quiet = f.repo.commit("Swap").unwrap();
        assert_eq!((quiet.hooks.as_slice(), quiet.printed.as_str()), (&["pre-commit".to_owned()][..], ""));
        let outcome = told(Outcome::titled("Changes committed", "Swap"), &quiet);
        assert_eq!((outcome.detail.as_str(), outcome.output), ("Swap", None));
    }

    #[test]
    fn cancelling_a_slow_hook_stops_its_whole_process_group_and_makes_no_commit() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        let tip = head(&f);
        let hooks = hooked(&f);
        let (own, child) = (f.dir.join(".git/hook.pid"), f.dir.join(".git/sleep.pid"));
        hook(
            &hooks,
            "pre-commit",
            &format!(
                "echo 'checking the tent'\n/bin/sleep 60 &\necho $! > '{child}'\necho $$ > '{own}'\nwait\n",
                own = own.display(),
                child = child.display()
            ),
        );
        staged(&f, "camp.txt", "tarp\n");
        let link = Link::new(false);
        let worker = {
            let (repo, link) = (f.repo.clone(), link.clone());
            thread::spawn(move || link.enter(|| repo.commit("Swap the tent")))
        };
        wait_for("the hook to start", || own.exists() && child.exists() && link.hook().is_some_and(|(hook, _)| hook == "pre-commit"));
        wait_for("the hook's line", || link.latest().as_deref() == Some("checking the tent"));
        let (hook_pid, sleep_pid) = (pid(&own), pid(&child));
        assert_eq!(unsafe { libc::getpgid(hook_pid) }, unsafe { libc::getpgid(sleep_pid) });
        assert_ne!(unsafe { libc::getpgid(hook_pid) }, unsafe { libc::getpgid(0) }, "the hook has a group of its own");
        assert!(link.cancellable());
        assert!(link.cancel());
        let failure = worker.join().unwrap().unwrap_err().notice();
        assert_eq!((failure.tone, failure.title.as_str(), failure.detail.as_str()), (Tone::Info, "Commit cancelled", "No commit was made."));
        assert!(gone(hook_pid), "the hook is still running");
        assert!(gone(sleep_pid), "the hook's child is still running");
        assert_eq!(head(&f), tip);
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), b"camp.txt\n");
        assert!(!f.dir.join(".git/index.lock").exists());
        assert!(!link.cancellable() && !link.cancelled());
    }

    #[test]
    fn a_commit_cannot_be_cancelled_once_post_commit_runs_and_commit_all_keeps_its_staging() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        let tip = head(&f);
        let hooks = hooked(&f);
        let (started, release) = (f.dir.join(".git/post.started"), f.dir.join(".git/post.release"));
        hook(
            &hooks,
            "post-commit",
            &format!(
                ": > '{started}'\nwhile [ ! -e '{release}' ]; do /bin/sleep 0.02; done\n",
                started = started.display(),
                release = release.display()
            ),
        );
        staged(&f, "camp.txt", "tarp\n");
        let link = Link::new(false);
        let worker = {
            let (repo, link) = (f.repo.clone(), link.clone());
            thread::spawn(move || link.enter(|| repo.commit("Swap the tent")))
        };
        wait_for("post-commit to start", || started.exists() && link.hook().is_some_and(|(hook, _)| hook == "post-commit"));
        assert!(!link.cancel(), "the commit was already made");
        fs::write(&release, "").unwrap();
        let committed = worker.join().unwrap().unwrap();
        assert_eq!(committed.stopped, None);
        assert_ne!(head(&f), tip);
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Swap the tent\n");
        fs::remove_file(hooks.join("post-commit")).unwrap();

        let (own, gate) = (f.dir.join(".git/all.pid"), f.dir.join(".git/all.gate"));
        hook(&hooks, "pre-commit", &format!("echo $$ > '{}'\nwhile [ ! -e '{}' ]; do /bin/sleep 0.02; done\n", own.display(), gate.display()));
        fs::write(f.dir.join("camp.txt"), "canvas\n").unwrap();
        fs::write(f.dir.join("stove.txt"), "gas\n").unwrap();
        let moved = head(&f);
        let link = Link::new(false);
        let worker = {
            let (repo, link, moved) = (f.repo.clone(), link.clone(), moved.clone());
            thread::spawn(move || link.enter(|| repo.commit_all(moved.as_deref(), Some(std::ffi::OsStr::new("refs/heads/main")), "Pack")))
        };
        wait_for("the hook to start", || own.exists() && link.cancellable());
        let hook_pid = pid(&own);
        assert!(link.cancel());
        let failure = worker.join().unwrap().unwrap_err().notice();
        assert_eq!((failure.title.as_str(), failure.detail.as_str()), ("Commit cancelled", "No commit was made. Your changes stay staged."));
        assert!(gone(hook_pid));
        assert_eq!(head(&f), moved);
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), b"camp.txt\nstove.txt\n");
    }

    #[test]
    fn a_hook_that_cannot_find_its_program_keeps_that_explanation() {
        let f = Fixture::new();
        let hooks = hooked(&f);
        hook(&hooks, "pre-commit", "fastkraken-no-such-linter --fix\n");
        staged(&f, "camp.txt", "tent\n");
        let failure = f.repo.commit("Pitch the tent").unwrap_err();
        assert!(failure.plain.is_none());
        assert!(failure.text.contains("fastkraken-no-such-linter"), "{}", failure.text);
        assert!(head(&f).is_none());
    }

    #[test]
    fn a_failed_hook_names_the_failure_of_any_other_command() {
        let ran = |hook: &str, code| vec![Ran { hook: hook.to_owned(), code: Some(code) }];
        let push = named(
            Failure::from("Push Failed — error: failed to push some refs to '../trail.git'".to_owned()),
            &ran("pre-push", 1),
            Some(&PUSHING),
        )
        .notice();
        assert_eq!((push.title.as_str(), push.detail.as_str()), ("pre-push stopped the push", "Nothing was pushed. It exited with code 1."));
        assert_eq!(push.output.as_deref(), Some("error: failed to push some refs to '../trail.git'"));
        assert!(push.expanded);
        let checkout = named(Failure::from("Switch Failed — oops".to_owned()), &ran("post-checkout", 2), None).notice();
        assert_eq!((checkout.title.as_str(), checkout.detail.as_str()), ("post-checkout failed", "It exited with code 2 after Git had finished."));
        let rebase = named(Failure::from("The pre-rebase hook refused to rebase.".to_owned()), &ran("pre-rebase", 1), None).notice();
        assert_eq!((rebase.title.as_str(), rebase.detail.as_str()), ("pre-rebase failed", "It exited with code 1."));
        let fine = named(Failure::from("Push Failed — rejected".to_owned()), &ran("pre-push", 0), Some(&PUSHING)).notice();
        assert_eq!(fine.title, "Push Failed");
        let lfs = "This repository is configured for Git LFS but 'git-lfs' was not found on your path. If you no longer wish to use Git LFS, remove this hook by deleting the 'pre-push' file in the hooks directory (set by 'core.hookspath'; usually '.git/hooks').";
        assert!(named(Failure::from(format!("Push Failed — {lfs}")), &ran("pre-push", 2), Some(&PUSHING)).plain.is_none());
        let unknown = stopped(&COMMITTING, None, &["pre-commit".into(), "commit-msg".into()], "lint failed").unwrap().notice();
        assert_eq!((unknown.title.as_str(), unknown.detail.as_str()), ("pre-commit or commit-msg stopped the commit", "No commit was made."));
    }

    #[test]
    fn switching_and_rebasing_record_the_hooks_they_ran_for_the_failure_notice() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        f.git(&["branch", "side"]);
        staged(&f, "stove.txt", "gas\n");
        f.repo.commit("Light the stove").unwrap();
        let hooks = hooked(&f);
        hook(&hooks, "post-checkout", "echo 'refreshing the map'\nexit 2\n");
        hook(&hooks, "pre-rebase", "echo 'not on this branch'\nexit 1\n");
        let link = Link::new(false);
        let switched = link.enter(|| crate::repository::checked(f.repo.command().args(["switch", "-q", "side"])));
        assert!(switched.is_err());
        assert_eq!(f.git(&["branch", "--show-current"]), b"side\n", "the switch itself happened");
        let failure = named(Failure::from(switched.unwrap_err()), &link.ran(), None).notice();
        assert_eq!((failure.title.as_str(), failure.detail.as_str()), ("post-checkout failed", "It exited with code 2 after Git had finished."));
        assert_eq!(failure.output.as_deref(), Some("refreshing the map"));
        let tip = head(&f);
        let link = Link::new(false);
        let rebased = link.enter(|| crate::repository::checked(f.repo.command().args(["rebase", "main"])));
        assert!(rebased.is_err());
        assert_eq!(link.ran(), [Ran { hook: "pre-rebase".into(), code: Some(1) }]);
        assert_eq!(head(&f), tip);
        assert!(!f.dir.join(".git/rebase-merge").exists());
        let untraced = Link::new(false);
        untraced.enter(|| crate::repository::checked(f.repo.read_command().args(["status", "--porcelain=v2"]))).unwrap();
        assert!(untraced.ran().is_empty());
    }

    #[test]
    fn a_hook_whose_exit_git_ignores_is_not_blamed_for_a_later_failure() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        f.git(&["switch", "-qc", "side"]);
        staged(&f, "camp.txt", "tarp\n");
        f.repo.commit("Swap for a tarp").unwrap();
        f.git(&["switch", "-q", "main"]);
        staged(&f, "camp.txt", "canvas\n");
        f.repo.commit("Swap for canvas").unwrap();
        let main = String::from_utf8(f.git(&["rev-parse", "main"])).unwrap().trim().to_owned();
        f.git(&["switch", "-q", "side"]);
        let hooks = hooked(&f);
        hook(&hooks, "post-checkout", "echo 'refreshing the map'\nexit 1\n");
        hook(&hooks, "post-commit", "exit 1\n");
        let link = Link::new(false);
        let conflict = link.enter(|| crate::rebase::replay(&f.repo, &main, false, None, false)).unwrap_err();
        assert!(conflict.text.starts_with("The rebase stopped on a conflict"), "{}", conflict.text);
        let shown = named(conflict, &link.ran(), None).notice();
        assert!(shown.title.starts_with("The rebase stopped on a conflict"), "{}", shown.title);
        f.git(&["rebase", "--abort"]);

        let link = Link::new(false);
        staged(&f, "stove.txt", "gas\n");
        link.enter(|| f.repo.commit("Light the stove")).unwrap();
        let later = named(Failure::from("Changes committed — Git could not put your local changes back".to_owned()), &link.ran(), None);
        assert!(later.plain.is_none());

        let link = Link::new(false);
        let switched = link.enter(|| crate::repository::checked(f.repo.command().args(["switch", "-q", "main"]))).unwrap_err();
        let failure = named(Failure::from(switched), &link.ran(), None).notice();
        assert_eq!(failure.title, "post-checkout failed");
    }

    #[test]
    fn a_push_shows_what_its_pre_push_hook_prints_and_records_its_exit_code() {
        let f = Fixture::new();
        staged(&f, "camp.txt", "tent\n");
        f.repo.commit("Pitch the tent").unwrap();
        let bare = f.dir.join(".git/trail.git");
        f.git(&["init", "-q", "--bare", bare.to_str().unwrap()]);
        f.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        let hooks = hooked(&f);
        let gate = f.dir.join(".git/push.gate");
        hook(&hooks, "pre-push", &format!("echo 'pre-push: checking'\nwhile [ ! -e '{}' ]; do /bin/sleep 0.02; done\nexit 1\n", gate.display()));
        let link = Link::new(true);
        let worker = {
            let (repo, link) = (f.repo.clone(), link.clone());
            thread::spawn(move || link.enter(|| crate::repository::checked(repo.command().args(["push", "-q", "origin", "main"]))))
        };
        wait_for("the hook's line", || {
            link.hook().is_some_and(|(hook, _)| hook == "pre-push") && link.latest().as_deref() == Some("pre-push: checking")
        });
        fs::write(&gate, "").unwrap();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(link.ran(), [Ran { hook: "pre-push".into(), code: Some(1) }]);
        assert!(link.hook().is_none());
        let pushed = f.repo.command().arg("--git-dir").arg(&bare).args(["rev-parse", "-q", "--verify", "refs/heads/main"]).output().unwrap();
        assert!(pushed.stdout.is_empty());
    }
}
