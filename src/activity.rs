use crate::{
    Tab, kit,
    theme::*,
    view::{self, Desk},
};
use gpui::{
    Context, Div, FontWeight, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div,
    prelude::*, rgb, uniform_list,
};
use crate::theme::px;
use std::{
    cell::Cell,
    collections::VecDeque,
    io,
    ops::Range,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) const CAPACITY: usize = 500;
const KEPT: usize = 16 * 1024;
pub(crate) const MASK: &str = "•••";
const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub id: u64,
    pub at: SystemTime,
    pub argv: Vec<String>,
    pub dir: PathBuf,
    pub read_only: bool,
    pub duration: Duration,
    pub exit: Option<i32>,
    pub note: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

impl Entry {
    pub(crate) fn succeeded(&self) -> bool {
        self.exit == Some(0)
    }

    pub(crate) fn command_line(&self) -> String {
        self.argv.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")
    }

    pub(crate) fn summary(&self) -> String {
        let mut args = self.argv.iter().skip(1);
        let mut shown = vec![quote(self.argv.first().map_or("git", String::as_str))];
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--no-pager" | "--no-optional-locks" | "--literal-pathspecs" => {}
                "-c" | "-C" => {
                    args.next();
                }
                _ => {
                    shown.push(quote(arg));
                    shown.extend(args.by_ref().map(|arg| quote(arg)));
                    break;
                }
            }
        }
        shown.join(" ").replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
    }

    pub(crate) fn result(&self) -> String {
        match (&self.note, self.exit) {
            (Some(note), _) => note.clone(),
            (None, Some(code)) => format!("exit {code}"),
            (None, None) => "no exit code".to_owned(),
        }
    }

    pub(crate) fn output(&self) -> String {
        match (self.stdout.trim_end(), self.stderr.trim_end()) {
            ("", err) => err.to_owned(),
            (out, "") => out.to_owned(),
            (out, err) => format!("{out}\n{err}"),
        }
    }

    pub(crate) fn line(&self) -> String {
        let place = if self.dir.as_os_str().is_empty() { String::new() } else { format!(" · in {}", self.dir.display()) };
        format!("{}  {} · {} · {}{place}", clock(self.at), self.summary(), self.result(), took(self.duration))
    }

    fn failed(&self) -> bool {
        !self.succeeded() && (!self.read_only || self.exit != Some(1) || !self.stderr.trim().is_empty())
    }
}

fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,^~{}".contains(c) || c == '•');
    if plain { arg.to_owned() } else { format!("'{}'", arg.replace('\'', "'\\''")) }
}

struct Log {
    entries: VecDeque<Arc<Entry>>,
    newest: u64,
}

impl Log {
    const fn new() -> Self {
        Self { entries: VecDeque::new(), newest: 0 }
    }

    fn push(&mut self, mut entry: Entry) -> u64 {
        self.newest += 1;
        entry.id = self.newest;
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(Arc::new(entry));
        self.newest
    }
}

static LOG: Mutex<Log> = Mutex::new(Log::new());

fn log() -> std::sync::MutexGuard<'static, Log> {
    LOG.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn record(command: &Command, started: Instant, at: SystemTime, result: &io::Result<Output>) -> u64 {
    let entry = entry(command, started, at, result);
    let (read_only, failed) = (entry.read_only, entry.failed());
    let id = log().push(entry);
    TRAIL.with(|trail| {
        if let Some(mut seen) = trail.get() {
            seen.last = Some(id);
            if !read_only {
                seen.wrote = Some(id);
            }
            if failed {
                seen.failed = Some(id);
            }
            trail.set(Some(seen));
        }
    });
    id
}

fn entry(command: &Command, started: Instant, at: SystemTime, result: &io::Result<Output>) -> Entry {
    let argv = arguments(command);
    let read_only = argv.iter().any(|arg| arg == "--no-optional-locks");
    let dir = command
        .get_current_dir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let (exit, note, stdout, stderr) = match result {
        Ok(output) => {
            let note = match (output.status.code(), output.status.signal()) {
                (None, Some(signal)) => Some(format!("ended by signal {signal}")),
                _ => None,
            };
            let kept = |bytes: &[u8]| tail(redact(&String::from_utf8_lossy(bytes)).as_bytes());
            let stdout = if read_only { String::new() } else { kept(&output.stdout) };
            (output.status.code(), note, stdout, kept(&output.stderr))
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => (None, Some("cancelled".to_owned()), String::new(), String::new()),
        Err(error) => (None, Some("did not start".to_owned()), String::new(), redact(&error.to_string())),
    };
    Entry { id: 0, at, argv, dir, read_only, duration: started.elapsed(), exit, note, stdout, stderr }
}

pub(crate) fn newest() -> u64 {
    log().newest
}

pub(crate) fn entries() -> Vec<Arc<Entry>> {
    log().entries.iter().cloned().collect()
}

pub(crate) fn recent(count: usize) -> Vec<Arc<Entry>> {
    let mut tries = 0;
    let log = loop {
        match LOG.try_lock() {
            Ok(log) => break log,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) if tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::TryLockError::WouldBlock) => return Vec::new(),
        }
    };
    let skip = log.entries.len().saturating_sub(count);
    log.entries.iter().skip(skip).cloned().collect()
}

pub(crate) fn find(id: u64) -> Option<Arc<Entry>> {
    log().entries.iter().find(|entry| entry.id == id).cloned()
}

fn arguments(command: &Command) -> Vec<String> {
    let program = Path::new(command.get_program())
        .file_name()
        .map_or_else(|| command.get_program().to_string_lossy(), |name| name.to_string_lossy())
        .into_owned();
    let mut argv = vec![program];
    let mut setting = false;
    for arg in command.get_args() {
        let arg = arg.to_string_lossy();
        let (flag, pair) = match arg.strip_prefix("-c").filter(|pair| !setting && !pair.is_empty() && !arg.starts_with("--")) {
            Some(pair) => ("-c", pair),
            None if setting => ("", arg.as_ref()),
            None => ("", ""),
        };
        let shown = match pair.split_once('=') {
            Some((key, _)) if key.to_ascii_lowercase().starts_with("credential.") => format!("{flag}{}={MASK}", redact(key)),
            _ => redact(&arg),
        };
        setting = !setting && arg == "-c";
        argv.push(shown);
    }
    argv
}

pub(crate) fn redact(text: &str) -> String {
    passwords(&userinfo(text))
}

fn userinfo(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        shown.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || "/?#'\"<>".contains(c))
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(sign) => {
                shown.push_str(MASK);
                shown.push_str(&authority[sign..]);
            }
            None => shown.push_str(authority),
        }
        rest = &tail[end..];
    }
    shown.push_str(rest);
    shown
}

fn passwords(text: &str) -> String {
    const KEY: &str = "password=";
    let lower = text.to_ascii_lowercase();
    let mut shown = String::with_capacity(text.len());
    let (mut from, mut search) = (0, 0);
    while let Some(found) = lower[search..].find(KEY) {
        let start = search + found + KEY.len();
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || "&;'\"".contains(c))
            .map_or(text.len(), |n| start + n);
        shown.push_str(&text[from..start]);
        if end > start {
            shown.push_str(MASK);
        }
        (from, search) = (end, end);
    }
    shown.push_str(&text[from..]);
    shown
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(KEPT)..]).into_owned();
    if bytes.len() <= KEPT {
        return text;
    }
    match text.find('\n') {
        Some(at) if at < 512 => text[at + 1..].to_owned(),
        _ => text.chars().skip(1).collect(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Trail {
    last: Option<u64>,
    wrote: Option<u64>,
    failed: Option<u64>,
}

impl Trail {
    pub(crate) fn entry(&self, failed: bool) -> Option<u64> {
        if failed { self.failed.or(self.wrote) } else { self.wrote }
    }
}

thread_local! {
    static TRAIL: Cell<Option<Trail>> = const { Cell::new(None) };
}

struct Traced(Option<Trail>);

impl Drop for Traced {
    fn drop(&mut self) {
        let outer = self.0;
        TRAIL.with(|trail| {
            let inner = trail.replace(outer);
            if let (Some(mut outer), Some(inner)) = (outer, inner) {
                outer.last = inner.last.or(outer.last);
                outer.wrote = inner.wrote.or(outer.wrote);
                outer.failed = inner.failed.or(outer.failed);
                trail.set(Some(outer));
            }
        });
    }
}

pub(crate) fn traced<R>(work: impl FnOnce() -> R) -> (R, Trail) {
    let outer = TRAIL.with(|trail| trail.replace(Some(Trail::default())));
    let restore = Traced(outer);
    let result = work();
    let trail = TRAIL.with(Cell::get).unwrap_or_default();
    drop(restore);
    (result, trail)
}


#[derive(Clone, Debug)]
pub(crate) enum Action {
    Toggle,
    Close,
    Show(u64),
    Select(u64),
    Step(crate::navigate::Move),
    ToggleReads,
    CopyCommand,
    CopyOutput,
}

#[derive(Default)]
pub(crate) struct State {
    open: bool,
    show_reads: bool,
    selected: Option<u64>,
    shown: Vec<Arc<Entry>>,
    lines: Vec<SharedString>,
    lines_of: Option<u64>,
    seen: u64,
    watching: bool,
    scroll: UniformListScrollHandle,
    output_scroll: UniformListScrollHandle,
}

impl State {
    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn show_reads(&self) -> bool {
        self.show_reads
    }

    pub(crate) fn shown(&self) -> &[Arc<Entry>] {
        &self.shown
    }

    pub(crate) fn selected(&self) -> Option<&Arc<Entry>> {
        self.index().and_then(|index| self.shown.get(index))
    }

    fn index(&self) -> Option<usize> {
        match self.selected {
            Some(id) => self.shown.iter().position(|entry| entry.id == id),
            None => (!self.shown.is_empty()).then_some(0),
        }
    }

    fn sync(&mut self) -> bool {
        let newest = newest();
        if newest == self.seen {
            return false;
        }
        self.seen = newest;
        let reads = self.show_reads;
        self.shown = entries().into_iter().rev().filter(|entry| reads || !entry.read_only).collect();
        if self.selected().map(|entry| entry.id) != self.lines_of {
            self.lines_for_selection();
        }
        true
    }

    fn lines_for_selection(&mut self) {
        let selected = self.selected().cloned();
        self.lines_of = selected.as_ref().map(|entry| entry.id);
        self.lines = selected
            .map(|entry| entry.output().lines().map(|line| SharedString::from(line.to_owned())).collect())
            .unwrap_or_default();
        self.output_scroll = UniformListScrollHandle::default();
    }

    fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.lines_for_selection();
        if let Some(index) = self.index() {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }
}

pub(crate) const COMMANDS: &[crate::commands::Command] = &[crate::commands::Command::new(
    "view.activity",
    crate::commands::Group::View,
    |ui, _| if ui.activity.open { "Hide activity log" } else { "Show activity log" }.into(),
    |ui, target, _| {
        use crate::commands::Availability;
        if crate::commands::on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
    },
    |_, _| Some(view::Action::Activity(Action::Toggle)),
)
.words(&["git commands", "history of commands", "log", "output", "hooks"])
.icon("history")
.keys("cmd-shift-y")
.bar(crate::commands::Bar::View)];

fn watch(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.activity.watching {
        return;
    }
    tab.ui.activity.watching = true;
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let going = tab.update(cx, |tab, cx| {
                let state = &mut tab.ui.activity;
                if !state.open {
                    state.watching = false;
                    return false;
                }
                if state.sync() {
                    cx.notify();
                }
                true
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

fn open(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &mut tab.ui;
    ui.activity.open = true;
    ui.diff_open = false;
    ui.search.hide(cx);
    ui.activity.seen = 0;
    ui.activity.sync();
    view::focus(ui, window);
    watch(tab, cx);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    match action {
        Action::Toggle if tab.ui.activity.open => tab.ui.activity.close(),
        Action::Toggle => {
            open(tab, window, cx);
            let newest = tab.ui.activity.shown.first().map(|entry| entry.id);
            if let Some(id) = newest {
                tab.ui.activity.select(id);
            }
        }
        Action::Close => {
            tab.ui.activity.close();
            view::focus(&tab.ui, window);
        }
        Action::Show(id) => {
            if find(id).is_some_and(|entry| entry.read_only) {
                tab.ui.activity.show_reads = true;
            }
            open(tab, window, cx);
            if tab.ui.activity.shown.iter().any(|entry| entry.id == id) {
                tab.ui.activity.select(id);
            } else if let Some(newest) = tab.ui.activity.shown.first().map(|entry| entry.id) {
                tab.ui.activity.select(newest);
                tab.ui.notices.show(crate::notice::Notice::new(
                    view::Tone::Info,
                    "That command is no longer in the activity log",
                    "The log keeps the last 500 commands.",
                ));
            }
        }
        Action::Select(id) => tab.ui.activity.select(id),
        Action::Step(how) => {
            let state = &mut tab.ui.activity;
            let moved = crate::navigate::moved(state.index(), state.shown.len(), how);
            if let Some(id) = moved.and_then(|index| state.shown.get(index)).map(|entry| entry.id) {
                state.select(id);
            }
        }
        Action::ToggleReads => {
            let state = &mut tab.ui.activity;
            state.show_reads = !state.show_reads;
            state.seen = 0;
            state.sync();
            if state.index().is_none() {
                state.selected = None;
                state.lines_for_selection();
            }
        }
        Action::CopyCommand => {
            if let Some(text) = tab.ui.activity.selected().map(|entry| entry.command_line()) {
                tab.dispatch(view::Action::CopyText { text, what: "command" }, window, cx);
            }
        }
        Action::CopyOutput => {
            if let Some(text) = tab.ui.activity.selected().map(|entry| entry.output()) {
                tab.dispatch(view::Action::CopyText { text, what: "output" }, window, cx);
            }
        }
    }
    cx.notify();
}


pub(crate) fn took(duration: Duration) -> String {
    let ms = duration.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", duration.as_secs_f64())
    } else {
        format!("{} min {} s", ms / 60_000, ms % 60_000 / 1000)
    }
}

fn clock(at: SystemTime) -> String {
    let seconds = at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs()) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub(crate) fn render<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let state = &app.ui().activity;
    let count = state.shown.len();
    let counted = match count {
        1 => "1 command".to_owned(),
        n => format!("{n} commands"),
    };
    let reads = state.show_reads;
    let header = div()
        .min_h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("Activity log"))
        .child(kit::muted(counted))
        .child(kit::grow())
        .child(kit::pressable(
            div()
                .id("activity-reads")
                .flex()
                .items_center()
                .gap(px(6.))
                .px_1()
                .rounded(px(RADIUS_CHIP))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(palette().hover)))
                .focusable()
                .min_h(px(TARGET))
                .focus(kit::ring_clear)
                .child(kit::checkbox(reads))
                .child(div().text_size(px(TEXT_SMALL)).text_color(rgb(palette().text_2)).child("Show reads")),
            view::Action::Activity(Action::ToggleReads),
            cx,
        ))
        .child(kit::icon_btn("close-activity", "x", "Close (Esc)", view::Action::Activity(Action::Close), cx));
    let list = if count == 0 {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(div().text_color(rgb(palette().text_2)).child("No Git commands yet"))
            .child(kit::muted(if reads {
                "Commands appear here as FastKraken runs them."
            } else {
                "Commands that change the repository appear here. Tick Show reads to see the ones that only read it."
            }))
    } else {
        div().flex_1().min_h_0().flex().child(
            uniform_list(
                "activity-entries",
                count,
                cx.processor(|app, range: Range<usize>, _, cx| {
                    let state = &app.ui().activity;
                    let selected = state.selected().map(|entry| entry.id);
                    range
                        .filter_map(|index| state.shown.get(index).map(|entry| (index, entry)))
                        .map(|(index, entry)| {
                            let id = entry.id;
                            let failed = !entry.succeeded();
                            kit::row(("activity-entry", index), selected == Some(id))
                                .h(whole_px(ROW))
                                .px_3()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(58.))
                                        .flex_shrink_0()
                                        .font_family(MONO)
                                        .text_size(px(TEXT_TINY))
                                        .text_color(rgb(palette().text_3))
                                        .child(clock(entry.at)),
                                )
                                .child(kit::fit(entry.summary()).mono().size(TEXT_SMALL).grow())
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(TEXT_TINY))
                                        .text_color(rgb(palette().text_3))
                                        .child(took(entry.duration)),
                                )
                                .child(
                                    div()
                                        .w(px(84.))
                                        .flex_shrink_0()
                                        .flex()
                                        .justify_end()
                                        .text_size(px(TEXT_TINY))
                                        .text_color(rgb(if failed { palette().deleted } else { palette().text_3 }))
                                        .child(entry.result()),
                                )
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    app.dispatch(view::Action::Activity(Action::Select(id)), w, cx);
                                }))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(state.scroll.clone())
            .flex_1()
            .min_h_0(),
        )
        .child(kit::scrollbar("activity-scrollbar", &state.scroll))
    };
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(palette().canvas))
        .child(header)
        .child(list)
        .when_some(state.selected().cloned(), |d, entry| d.child(detail(app, &entry, cx)))
}

fn detail<T: Desk>(app: &T, entry: &Entry, cx: &mut Context<T>) -> Div {
    let state = &app.ui().activity;
    let lines = state.lines.len();
    let place = entry.dir.to_string_lossy().into_owned();
    let facts = format!(
        "{} · {} · {}{}",
        clock(entry.at),
        took(entry.duration),
        entry.result(),
        if entry.read_only { " · read" } else { "" }
    );
    div()
        .h(px(250.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(rgb(palette().line))
        .bg(rgb(palette().panel))
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(MONO)
                        .text_size(px(TEXT_SMALL))
                        .line_clamp(4)
                        .child(entry.command_line()),
                )
                .child(kit::btn("activity-copy-command", "Copy command").small().icon("copy").on(view::Action::Activity(Action::CopyCommand), cx))
                .child(
                    kit::btn("activity-copy-output", "Copy output")
                        .small()
                        .icon("copy")
                        .enabled(lines > 0)
                        .on(view::Action::Activity(Action::CopyOutput), cx),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .text_size(px(TEXT_TINY))
                .text_color(rgb(if entry.succeeded() { palette().text_3 } else { palette().deleted }))
                .child(facts)
                .child(kit::fit(place).size(TEXT_TINY).color(palette().text_3).cut(kit::Cut::Start).grow()),
        )
        .child(if lines == 0 {
            div().flex_1().min_h_0().child(kit::muted("Git printed nothing."))
        } else {
            div().flex_1().min_h_0().flex().rounded(px(RADIUS)).border_1().border_color(rgb(palette().line)).bg(rgb(palette().canvas)).p_1().child(
                uniform_list(
                    "activity-output",
                    lines,
                    cx.processor(|app, range: Range<usize>, _, _| {
                        let lines = &app.ui().activity.lines;
                        range
                            .filter_map(|index| lines.get(index).cloned())
                            .map(|line| {
                                div()
                                    .h(px(16.))
                                    .px_1()
                                    .font_family(MONO)
                                    .text_size(px(TEXT_TINY))
                                    .text_color(rgb(palette().text_2))
                                    .whitespace_nowrap()
                                    .child(line)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(state.output_scroll.clone())
                .flex_1()
                .min_h_0(),
            )
            .child(kit::scrollbar("activity-output-scrollbar", &state.output_scroll))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{checked, tests::Fixture};
    use std::fs;

    fn entry_with(marker: &str) -> Option<Arc<Entry>> {
        entries().into_iter().rev().find(|entry| entry.argv.iter().any(|arg| arg.contains(marker)))
    }

    #[test]
    fn credentials_are_masked_in_arguments_and_output() {
        let mut command = Command::new("/opt/git/bin/git");
        command.args([
            "--no-pager",
            "-c",
            "credential.https://github.com/cedar/ledger.git.helper=!exec '/Apps/FastKraken' --git-credential 'https://github.com/cedar/ledger.git'",
            "-ccredential.helper=store --file=/secret",
            "-c",
            "http.lowSpeedLimit=1000",
            "push",
            "https://scout:hunter2@example.invalid/cedar/ledger.git",
            "--push-option=password=hunter2",
        ]);
        let argv = arguments(&command);
        assert_eq!(
            argv,
            [
                "git",
                "--no-pager",
                "-c",
                "credential.https://github.com/cedar/ledger.git.helper=•••",
                "-ccredential.helper=•••",
                "-c",
                "http.lowSpeedLimit=1000",
                "push",
                "https://•••@example.invalid/cedar/ledger.git",
                "--push-option=password=•••",
            ]
        );
        assert!(argv.iter().all(|arg| !arg.contains("hunter2") && !arg.contains("FastKraken")));
        assert_eq!(
            redact("fatal: unable to access 'https://x-access-token:ghp_abc123@github.com/o/r.git/': 403\nPassword=hunter2&next"),
            "fatal: unable to access 'https://•••@github.com/o/r.git/': 403\nPassword=•••&next"
        );
        assert_eq!(redact("To https://example.invalid/o/r.git"), "To https://example.invalid/o/r.git");
        assert_eq!(redact("git@example.invalid:o/r.git"), "git@example.invalid:o/r.git");
    }

    #[test]
    fn every_command_is_recorded_with_its_folder_duration_and_how_it_ended() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "FastKraken: git for the impatient\n").unwrap();
        f.git(&["add", "trail.txt"]);
        let refs = f.git(&["for-each-ref"]);
        checked(f.repo.command().args(["commit", "-q", "-m", "Mark the activity-ford"])).unwrap();
        let commit = entry_with("Mark the activity-ford").unwrap();
        assert_eq!(&commit.argv[..2], ["git", "--no-pager"]);
        assert_eq!(commit.summary(), "git commit -q -m 'Mark the activity-ford'");
        assert!(commit.command_line().ends_with("commit -q -m 'Mark the activity-ford'"));
        assert_eq!((commit.exit, commit.read_only, commit.note.as_deref()), (Some(0), false, None));
        assert_eq!(commit.dir, f.repo.root);
        assert!(commit.duration > Duration::ZERO);
        assert_eq!(commit.result(), "exit 0");
        assert_ne!(f.git(&["for-each-ref"]), refs);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");

        let _ = checked(f.repo.read_command().args(["rev-parse", "activity-missing"]));
        let read = entry_with("activity-missing").unwrap();
        assert!(read.read_only);
        assert_eq!(read.exit, Some(128));
        assert!(read.stderr.contains("activity-missing"), "{read:?}");
        assert!(read.failed());
        checked(f.repo.read_command().args(["log", "-1", "--format=%s", "--grep=activity-ford"])).unwrap();
        let log = entry_with("--grep=activity-ford").unwrap();
        assert!(log.read_only && log.stdout.is_empty());
        let _ = crate::repository::run(f.repo.read_command().args(["config", "--get", "activity.missing"]));
        let answer = entry_with("activity.missing").unwrap();
        assert_eq!(answer.exit, Some(1));
        assert!(!answer.failed());
    }

    #[test]
    fn a_hooks_output_is_in_its_commits_entry() {
        let f = Fixture::new();
        let hooks = f.dir.join(".git/activity-hooks");
        fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\necho 'lint: 3 files look fine' >&2\n").unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        f.git(&["commit", "--allow-empty", "-q", "-m", "Hooked activity-camp"]);
        let commit = entry_with("Hooked activity-camp").unwrap();
        assert!(commit.stderr.contains("lint: 3 files look fine"), "{commit:?}");
        assert!(commit.output().contains("lint: 3 files look fine"));
    }

    #[test]
    fn the_log_keeps_the_last_five_hundred_and_the_end_of_long_output() {
        let mut log = Log::new();
        let started = Instant::now();
        for n in 0..CAPACITY + 20 {
            let mut command = Command::new("git");
            command.arg(format!("ring-{n}"));
            let output = Output { status: std::process::ExitStatus::from_raw(0), stdout: Vec::new(), stderr: Vec::new() };
            assert_eq!(log.push(entry(&command, started, SystemTime::now(), &Ok(output))), n as u64 + 1);
        }
        assert_eq!((log.entries.len(), log.newest), (CAPACITY, CAPACITY as u64 + 20));
        assert_eq!(log.entries.front().map(|entry| entry.argv[1].as_str()), Some("ring-20"));
        assert_eq!(log.entries.back().map(|entry| (entry.id, entry.argv[1].as_str())), Some((CAPACITY as u64 + 20, "ring-519")));
        assert!(log.entries.iter().zip(log.entries.iter().skip(1)).all(|(older, newer)| older.id + 1 == newer.id));

        let long: Vec<u8> = (0..4000).flat_map(|n| format!("line {n} of the hook\n").into_bytes()).collect();
        let kept = tail(&long);
        assert!(kept.len() <= KEPT);
        assert!(kept.starts_with("line ") && kept.ends_with("line 3999 of the hook\n"), "{}", &kept[..40]);
    }

    #[test]
    fn a_notice_points_at_the_command_that_failed_or_else_the_last_that_wrote() {
        let f = Fixture::new();
        let ((), trail) = traced(|| {
            let _ = checked(f.repo.read_command().args(["config", "--get", "activity.unset"]));
            f.git(&["commit", "--allow-empty", "-q", "-m", "Trail activity-one"]);
        });
        let wrote = entry_with("Trail activity-one").unwrap().id;
        assert_eq!((trail.entry(false), trail.entry(true)), (Some(wrote), Some(wrote)));
        let (_, trail) = traced(|| {
            f.git(&["commit", "--allow-empty", "-q", "-m", "Trail activity-two"]);
            traced(|| checked(f.repo.command().args(["switch", "activity-nowhere"]))).0
        });
        let failed = entry_with("activity-nowhere").unwrap().id;
        assert_eq!(trail.entry(true), Some(failed));
        let (_, trail) = traced(|| checked(f.repo.read_command().args(["rev-parse", "HEAD"])));
        assert_eq!(trail.entry(false), None);
        f.git(&["commit", "--allow-empty", "-q", "-m", "Untraced activity"]);
        assert_eq!(TRAIL.with(Cell::get), None);
    }

    #[test]
    fn the_viewer_lists_newest_first_and_hides_reads_until_asked() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-q", "-m", "Viewer activity-write"]);
        checked(f.repo.read_command().args(["log", "-1", "--format=%s", "--grep=viewer-activity-read"])).unwrap();
        let wrote = entry_with("Viewer activity-write").unwrap().id;
        let read = entry_with("--grep=viewer-activity-read").unwrap().id;
        let mut state = State::default();
        assert!(state.sync());
        assert!(state.shown().iter().all(|entry| !entry.read_only));
        assert!(state.shown().windows(2).all(|pair| pair[0].id > pair[1].id));
        assert!(state.shown().iter().any(|entry| entry.id == wrote));
        assert!(!state.shown().iter().any(|entry| entry.id == read));
        assert_eq!(state.selected().map(|entry| entry.id), state.shown().first().map(|entry| entry.id));
        state.select(wrote);
        f.git(&["commit", "--allow-empty", "-q", "-m", "Viewer activity-later"]);
        assert!(state.sync());
        assert_eq!(state.selected().map(|entry| entry.id), Some(wrote));
        state.show_reads = true;
        state.seen = 0;
        state.sync();
        assert!(state.shown().iter().any(|entry| entry.id == read));
        state.select(read);
        assert!(state.selected().is_some_and(|entry| entry.read_only));
    }

    #[test]
    fn a_credential_cut_by_the_16_kib_limit_is_still_masked() {
        let head = format!("{}https://scout:", "a".repeat(1000));
        let token = "hunter2".repeat(3);
        let rest = format!("@mirror.invalid/r.git{}", "b".repeat(KEPT - 32));
        let stderr = format!("{head}{token}{rest}").into_bytes();
        let output = Output { status: std::process::ExitStatus::from_raw(0), stdout: stderr.clone(), stderr };
        let mut command = Command::new("git");
        command.arg("push");
        let kept = entry(&command, Instant::now(), SystemTime::now(), &Ok(output));
        assert!(kept.stderr.len() <= KEPT && kept.stderr.ends_with('b'));
        assert!(!kept.stderr.contains("hunter2") && !kept.stdout.contains("hunter2"), "{}", &kept.stderr[..80]);
    }

    #[test]
    fn a_listed_command_is_one_line_and_the_output_keeps_its_place() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-q", "-m", "Viewer activity-lines\n\nWith a body\r\nof two lines"]);
        let commit = entry_with("Viewer activity-lines").unwrap();
        assert!(!commit.summary().contains(['\n', '\r']), "{}", commit.summary());
        assert!(commit.summary().ends_with("commit --allow-empty -q -m 'Viewer activity-lines⏎⏎With a body⏎of two lines'"), "{}", commit.summary());
        assert!(commit.command_line().contains("activity-lines\n\nWith a body"));
        let mut state = State::default();
        state.sync();
        state.select(commit.id);
        let scroll = state.output_scroll.clone();
        f.git(&["commit", "--allow-empty", "-q", "-m", "Viewer activity-after"]);
        assert!(state.sync());
        assert_eq!(state.selected().map(|entry| entry.id), Some(commit.id));
        assert!(std::rc::Rc::ptr_eq(&scroll.0, &state.output_scroll.0));
        let after = entry_with("Viewer activity-after").unwrap().id;
        state.select(after);
        assert!(!std::rc::Rc::ptr_eq(&scroll.0, &state.output_scroll.0));
    }

    #[test]
    fn entries_say_how_they_ended() {
        let started = Instant::now();
        let mut command = Command::new("git");
        command.arg("fetch");
        let ended = |result: io::Result<Output>| entry(&command, started, SystemTime::now(), &result);
        let cancelled = ended(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        assert_eq!((cancelled.result(), cancelled.failed()), ("cancelled".to_owned(), true));
        assert_eq!(ended(Err(io::Error::from(io::ErrorKind::NotFound))).result(), "did not start");
        let killed = Output { status: std::process::ExitStatus::from_raw(9), stdout: Vec::new(), stderr: Vec::new() };
        assert_eq!(ended(Ok(killed)).result(), "ended by signal 9");
        assert_eq!(took(Duration::from_millis(12)), "12 ms");
        assert_eq!(took(Duration::from_millis(1400)), "1.4 s");
        assert_eq!(took(Duration::from_secs(125)), "2 min 5 s");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("refs/heads/main:refs/heads/main"), "refs/heads/main:refs/heads/main");
    }
}
