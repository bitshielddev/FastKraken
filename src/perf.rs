use crate::{
    Tab, repository,
    view::{Action, Desk, Selection},
};
use gpui::{App, AppContext, Context, ScrollStrategy, Window};
use serde_json::json;
use std::{
    ffi::OsStr,
    io::Write,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const FILE: &str = "survey.txt";
const SUBJECT: &str = "Measure selected hunk";
const ORIGINAL: &str = "Cedar gate\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nEast gate\n";

fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = repository::bundled_command(dir)?
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let write = child.stdin.take().unwrap().write_all(data);
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    write.map_err(|e| e.to_string())?;
    Ok(output.stdout)
}

pub fn fixture() -> Result<TempDir, String> {
    let dir = tempfile::Builder::new()
        .prefix("fastkraken-measure-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"], &[])?;
    for (key, value) in [
        ("user.name", "FastKraken Fixture"),
        ("user.email", "fixture@example.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("commit.gpgSign", "false"),
        ("gc.auto", "0"),
    ] {
        git(root, &["config", key, value], &[])?;
    }
    let mut stream = Vec::new();
    for i in 0..640 {
        let msg = format!("Survey checkpoint {i:03}\n");
        let line = format!("Checkpoint {i}\n");
        write!(stream,"commit refs/heads/main\nmark :{}\ncommitter FastKraken <fixture@example.invalid> {} +0000\ndata {}\n{}",
            i+1,1700000000+i,msg.len(),msg).unwrap();
        if i == 0 {
            write!(
                stream,
                "M 100644 inline {FILE}\ndata {}\n{ORIGINAL}",
                ORIGINAL.len()
            )
            .unwrap();
        }
        if i == 485 {
            stream.extend_from_slice(b"from :485\nmerge :1000\n");
        }
        writeln!(
            stream,
            "M 100644 inline route.txt\ndata {}\n{}",
            line.len(),
            line
        )
        .unwrap();
        if i == 350 {
            stream.extend_from_slice(b"commit refs/heads/creek\nmark :1000\ncommitter FastKraken <fixture@example.invalid> 1700000351 +0000\ndata 10\nCreek map\nfrom :351\nM 100644 inline creek.txt\ndata 4\nmap\n\n");
        }
    }
    git(root, &["fast-import", "--quiet"], &stream)?;
    git(root, &["reset", "--hard", "-q", "HEAD"], &[])?;
    Ok(dir)
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Open,
    Diff,
    Scroll,
    More,
    ShowPage,
    External,
    SelectFile,
    Stage,
    Commit,
}

pub struct Run {
    started: Instant,
    since: Instant,
    phase: Phase,
    queued: bool,
    sample: usize,
    page_len: usize,
    frames: Vec<f64>,
    edits: Option<TempDir>,
    offscreen: bool,
    hold: bool,
    status_seen: bool,
    opened: bool,
}

impl Run {
    pub fn new(started: Instant, edits: Option<TempDir>, offscreen: bool, hold: bool) -> Self {
        Self {
            started,
            since: started,
            phase: Phase::Open,
            queued: false,
            sample: 0,
            page_len: 0,
            frames: Vec::new(),
            edits,
            offscreen,
            hold,
            status_seen: false,
            opened: false,
        }
    }

    fn phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = Instant::now();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}
fn time(name: &str, start: Instant, offscreen: bool) {
    let metric = if offscreen {
        name.replace("_frame", "_scene")
    } else {
        name.to_owned()
    };
    println!("{}", json!({"metric":metric,"ms":ms(start.elapsed())}));
}

#[derive(Clone, Copy)]
struct Painted {
    history: usize,
    status: bool,
    diff: bool,
    external: bool,
    staged: bool,
    committed: bool,
}

impl Painted {
    fn from_tab(tab: &Tab) -> Self {
        Self {
            history: tab.ui.history.len(),
            status: tab.repo.is_some() && !tab.loading_status && !tab.ui.branch.is_empty(),
            diff: tab.diff.is_some() && !tab.ui.loading_diff,
            external: tab
                .ui
                .files
                .iter()
                .any(|f| !f.staged && f.path == Path::new(FILE)),
            staged: tab
                .ui
                .files
                .iter()
                .any(|f| f.staged && f.path == Path::new(FILE)),
            committed: tab.ui.history.first().is_some_and(|c| c.subject == SUBJECT),
        }
    }
}
pub fn schedule(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(run) = &mut tab.bench else { return };
    if run.offscreen || run.queued {
        return;
    }
    run.queued = true;
    let painted = Painted::from_tab(tab);
    let weak = cx.weak_entity();
    window.on_next_frame(move |window, cx| {
        weak.update(cx, |tab, cx| tick(tab, painted, None, window, cx))
            .ok();
    });
}

pub fn drive_offscreen(handle: crate::tabs::Front, cx: &App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(2))
                .await;
            let start = Instant::now();
            if cx
                .update_window(handle.into(), |_, window, cx| window.draw(cx).clear())
                .is_err()
            {
                return;
            }
            let draw = start.elapsed();
            let active = handle.update(cx, |tab, window, cx| {
                if tab.bench.is_none() {
                    return false;
                }
                tick(tab, Painted::from_tab(tab), Some(draw), window, cx);
                tab.bench.is_some()
            });
            if !matches!(active, Ok(true)) {
                return;
            }
        }
    })
    .detach();
}

fn tick(
    tab: &mut Tab,
    p: Painted,
    draw: Option<Duration>,
    window: &mut Window,
    cx: &mut Context<Tab>,
) {
    let Some(mut r) = tab.bench.take() else {
        return;
    };
    r.queued = false;

    if p.status && !r.status_seen {
        time("open_to_status_frame", r.started, r.offscreen);
        r.status_seen = true;
    }
    let mut action = None;
    let mut finish = false;
    if r.phase == Phase::Open && p.history > 0 && !r.opened {
        time("open_to_history_frame", r.started, r.offscreen);
        r.opened = true;
    }
    match r.phase {
        Phase::Open if r.opened && r.status_seen && p.diff => {
            r.phase(Phase::Diff);
            let i = 1.min(tab.ui.history.len() - 1);
            action = Some(Action::Select(Selection::Commit(
                tab.ui.history[i].oid.clone(),
            )));
        }
        Phase::Diff if p.diff => {
            time("select_to_diff_frame", r.since, r.offscreen);
            r.phase(Phase::Scroll);
            r.sample = 0;
            tab.ui
                .history_scroll
                .scroll_to_item_strict(0, ScrollStrategy::Top);
        }
        Phase::Scroll => {
            if r.sample > 0 {
                r.frames.push(ms(draw.unwrap_or_else(|| r.since.elapsed())));
            }
            r.since = Instant::now();
            r.sample += 1;
            let steps = 120.min(tab.ui.history.len().saturating_sub(25));
            if r.sample <= steps {
                let row = r.sample;
                tab.ui
                    .history_scroll
                    .scroll_to_item_strict(row, ScrollStrategy::Top);
            } else {
                println!(
                    "{}",
                    json!({"metric":if r.offscreen {"scroll_scene_build"} else {"scroll_frame_intervals"},"ms":r.frames})
                );
                if tab.ui.has_more {
                    r.page_len = tab.ui.history.len();
                    r.phase(Phase::More);
                    tab.ui
                        .history_scroll
                        .scroll_to_item_strict(tab.ui.stash.commit_row(r.page_len.saturating_sub(1)), ScrollStrategy::Top);
                    action = Some(Action::More);
                } else if r.edits.is_some() {
                    begin_edit(&mut r);
                } else {
                    finish = true
                }
            }
        }
        Phase::More if p.history > r.page_len => {
            r.phase = Phase::ShowPage;
            tab.ui
                .history_scroll
                .scroll_to_item_strict(tab.ui.stash.commit_row(r.page_len), ScrollStrategy::Top);
            cx.notify();
        }
        Phase::ShowPage => {
            time("load_more_to_frame", r.since, r.offscreen);
            if r.edits.is_some() {
                begin_edit(&mut r);
            } else {
                finish = true
            }
        }
        Phase::External if p.external => {
            time("external_save_to_status_frame", r.since, r.offscreen);
            r.phase(Phase::SelectFile);
            action = Some(Action::Select(Selection::File {
                path: FILE.into(),
                staged: false,
            }));
        }
        Phase::SelectFile if p.diff && tab.diff.as_ref().is_some_and(|d| d.hunks.len() == 2) => {
            r.phase(Phase::Stage);
            action = Some(Action::StageHunk(0));
        }
        Phase::Stage
            if p.staged
                && p.external
                && p.diff
                && !tab.ui.busy()
                && tab
                    .diff
                    .as_ref()
                    .is_some_and(|d| d.hunks.len() == 1 && d.text.contains("+Orchard gate")) =>
        {
            time("stage_to_split_diff_frame", r.since, r.offscreen);
            r.phase(Phase::Commit);
            tab.ui.input.update(cx, |input, cx| input.set_text(SUBJECT, cx));
            action = Some(Action::Commit);
        }
        Phase::Commit if p.committed && p.external && !p.staged && !tab.ui.busy() && p.diff => {
            time("commit_to_history_and_status_frame", r.since, r.offscreen);
            let root = r.edits.as_ref().unwrap().path();
            let head =
                git(root, &["show", "HEAD:survey.txt"], &[]).expect("verify committed bytes");
            let disk = std::fs::read(root.join(FILE)).expect("verify remaining edit");
            assert_eq!(head, ORIGINAL.replace("Cedar", "River").as_bytes());
            assert_eq!(
                disk,
                ORIGINAL
                    .replace("Cedar", "River")
                    .replace("East", "Orchard")
                    .as_bytes()
            );
            assert!(
                git(root, &["diff", "--cached", "--name-only"], &[])
                    .unwrap()
                    .is_empty()
            );
            println!(
                "{}",
                json!({"verification":"selected hunk committed; remaining hunk on disk; index clean"})
            );
            finish = true;
        }
        _ => {}
    }
    if finish {
        println!(
            "{}",
            json!({"result":if r.hold {"idle_ready"} else {"complete"}})
        );
        if !r.hold {
            cx.quit();
        }
        return;
    }
    let scrolling = r.phase == Phase::Scroll;
    tab.bench = Some(r);
    if let Some(action) = action {
        tab.dispatch(action, window, cx);
    }
    if scrolling {
        cx.notify();
    }
}

fn begin_edit(r: &mut Run) {
    r.phase(Phase::External);
    let path = r.edits.as_ref().expect("private fixture").path().join(FILE);
    std::fs::write(
        path,
        ORIGINAL
            .replace("Cedar", "River")
            .replace("East", "Orchard"),
    )
    .expect("write fixture edit");
}
