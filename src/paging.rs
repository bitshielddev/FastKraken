use crate::{
    PAGE,
    graph_view::GraphView,
    repository::{Commit, Repository, git_error},
    stash::HistoryItem,
    theme::ROW,
    view::{Selection, Ui},
};
use gpui::{point};
use crate::theme::px;
use std::{
    collections::HashSet,
    io::{self, BufRead, BufReader, Read},
    process::{Child, ChildStdout, Command, Output, Stdio},
    sync::atomic::AtomicBool,
    thread::{self, JoinHandle},
    time::{Instant, SystemTime},
};

pub(crate) const KEPT: usize = 5_000;
pub(crate) const LOADING: &str = "Loading more commits…";
const FIELDS: usize = 9;

pub(crate) struct HistoryStream {
    command: Command,
    child: Child,
    reader: BufReader<ChildStdout>,
    errors: Option<JoinHandle<Vec<u8>>>,
    started: Instant,
    at: SystemTime,
    ahead: Option<Commit>,
    loaded: usize,
    done: bool,
}

impl HistoryStream {
    pub(crate) fn open(repo: &Repository, view: &GraphView) -> Result<Self, String> {
        let mut command = repo.log_command(view.bodies);
        command
            .args(view.history_args())
            .arg("--")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (started, at) = (Instant::now(), SystemTime::now());
        let mut child = command.spawn().map_err(|e| format!("Run bundled Git: {e}"))?;
        let errors = child.stderr.take().map(|mut stderr| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = stderr.read_to_end(&mut bytes);
                bytes
            })
        });
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Git gave no history to read.".to_owned());
        };
        Ok(Self {
            command,
            child,
            reader: BufReader::with_capacity(1 << 16, stdout),
            errors,
            started,
            at,
            ahead: None,
            loaded: 0,
            done: false,
        })
    }

    pub(crate) fn next(&mut self, count: usize) -> Result<Vec<Commit>, String> {
        let mut rows = Vec::with_capacity(count.min(KEPT));
        while rows.len() < count {
            let row = match self.ahead.take() {
                Some(row) => row,
                None => match self.record()? {
                    Some(row) => row,
                    None => break,
                },
            };
            rows.push(row);
        }
        if self.ahead.is_none() {
            self.ahead = self.record()?;
        }
        self.loaded += rows.len();
        Ok(rows)
    }

    pub(crate) fn more(&self) -> bool {
        self.ahead.is_some()
    }

    pub(crate) fn loaded(&self) -> usize {
        self.loaded
    }

    fn record(&mut self) -> Result<Option<Commit>, String> {
        if self.done {
            return Ok(None);
        }
        let mut fields: [String; FIELDS] = Default::default();
        for (index, field) in fields.iter_mut().enumerate() {
            let mut bytes = Vec::new();
            let read = self
                .reader
                .read_until(0, &mut bytes)
                .map_err(|e| format!("Read the history from Git: {e}"))?;
            if read == 0 {
                self.finish()?;
                return match index {
                    0 => Ok(None),
                    _ => Err("Invalid history record".to_owned()),
                };
            }
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            *field = String::from_utf8_lossy(&bytes).into_owned();
        }
        let [oid, parents, author, email, date, time, refs, subject, body] = fields;
        Ok(Some(Commit {
            oid,
            parents: parents.split_whitespace().map(String::from).collect(),
            author,
            email,
            date,
            time: time.parse().unwrap_or(0),
            refs,
            subject,
            body: crate::repository::first_line(&body),
        }))
    }

    fn finish(&mut self) -> Result<(), String> {
        self.done = true;
        let status = self.child.wait().map_err(|e| e.to_string())?;
        let stderr = self.errors.take().and_then(|errors| errors.join().ok()).unwrap_or_default();
        let output = Output { status, stdout: Vec::new(), stderr };
        crate::activity::record(&self.command, self.started, self.at, &Ok(output.clone()));
        if status.success() { Ok(()) } else { Err(git_error(&output.stderr, status)) }
    }
}

impl Drop for HistoryStream {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stopped = io::Error::new(io::ErrorKind::Interrupted, "cancelled");
        crate::activity::record(&self.command, self.started, self.at, &Err(stopped));
    }
}

pub(crate) fn read(
    repo: &Repository,
    view: &GraphView,
    stream: Option<HistoryStream>,
    skip: usize,
    rows: usize,
    wanted: &[String],
    anchor: Option<&str>,
) -> (Option<HistoryStream>, Result<(Vec<Commit>, bool), String>) {
    let mut stream = match stream {
        Some(stream) => stream,
        None => match HistoryStream::open(repo, view) {
            Ok(stream) => stream,
            Err(error) => return (None, Err(error)),
        },
    };
    let mut page = || {
        if skip > stream.loaded() {
            stream.next(skip - stream.loaded())?;
        }
        let mut page = stream.next(rows)?;
        let found = |page: &[Commit]| wanted.iter().all(|oid| page.iter().any(|row| &row.oid == oid));
        if stream.more() && !found(&page) {
            page.extend(stream.next(PAGE)?);
        }
        if let Some(oid) = anchor.filter(|oid| stream.more() && !page.iter().any(|row| row.oid == *oid)) {
            let sought = HashSet::from([oid.to_owned()]);
            let positions = repo.positions(&view.history_args(), &sought, &AtomicBool::new(false));
            if let Some(&position) = positions.ok().as_ref().and_then(|positions| positions.get(oid)) {
                let through = position + 1 + PAGE;
                if through > stream.loaded() {
                    page.extend(stream.next(through - stream.loaded())?);
                }
            }
        }
        Ok(page)
    };
    match page() {
        Ok(page) => {
            let more = stream.more();
            (Some(stream), Ok((page, more)))
        }
        Err(error) => (None, Err(error)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Commit(String),
    Stash(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    key: Key,
    item: usize,
    pending: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reload {
    pub(crate) rows: usize,
    pub(crate) wanted: Vec<String>,
    pub(crate) anchor: Option<String>,
    pub(crate) place: Option<Place>,
}

pub(crate) fn rows(loaded: usize, deepest: Option<usize>) -> usize {
    let rows = if loaded <= KEPT { loaded } else { deepest.map_or(KEPT, |row| KEPT.max(row + 1 + PAGE)) };
    rows.max(PAGE)
}

pub(crate) fn anchor(top: f32, height: f32, items: usize, selected: Option<usize>) -> Option<usize> {
    if items == 0 || top < 1. {
        return None;
    }
    let at = |item: usize| item as f32 * ROW - top;
    if let Some(item) = selected.filter(|&item| item < items && at(item) + ROW > 0. && at(item) < height) {
        return Some(item);
    }
    Some(((top / ROW).floor() as usize).min(items - 1))
}

fn key(ui: &Ui, item: usize) -> Option<Key> {
    if item >= ui.history_items() {
        return None;
    }
    Some(match ui.stash.item(item) {
        HistoryItem::Commit(index) => Key::Commit(ui.history.get(index)?.oid.clone()),
        HistoryItem::Stash { stash, .. } => Key::Stash(ui.stash.entries.get(stash)?.oid.clone()),
    })
}

fn item_of(ui: &Ui, key: &Key) -> Option<usize> {
    match key {
        Key::Commit(oid) => ui.history.iter().position(|row| &row.oid == oid).map(|index| ui.stash.commit_row(index)),
        Key::Stash(oid) => ui.stash.stash_row(oid),
    }
}

pub(crate) fn reload(ui: &Ui) -> Reload {
    let items = ui.history_items();
    let selected = ui.selected_history_row();
    let (top, height, pending) = {
        let scroll = ui.history_scroll.0.borrow();
        (
            -f32::from(scroll.base_handle.offset().y),
            f32::from(scroll.base_handle.bounds().size.height),
            scroll.deferred_scroll_to_item.map(|deferred| deferred.item_index),
        )
    };
    let place = match pending {
        Some(item) if item > 0 => Some((item, true)),
        Some(_) => None,
        None => anchor(top, height, items, selected).map(|item| (item, false)),
    }
    .and_then(|(item, pending)| Some(Place { key: key(ui, item)?, item, pending }));
    let mut wanted = Vec::new();
    if let Some(Selection::Commit(oid)) = &ui.selection {
        if selected.is_some() {
            wanted.push(oid.clone());
        }
    }
    let anchor = match place.as_ref().map(|place| &place.key) {
        Some(Key::Commit(oid)) => Some(oid.clone()),
        Some(Key::Stash(oid)) => ui.stash.base_of(oid).map(str::to_owned),
        None => None,
    };
    wanted.extend(anchor.clone());
    wanted.dedup();
    let bottom = (height > 0. && items > 0)
        .then(|| (((top.max(0.) + height) / ROW).ceil() as usize).saturating_sub(1).min(items - 1));
    let deepest = [selected, place.as_ref().map(|place| place.item), bottom].into_iter().flatten().max();
    Reload { rows: rows(ui.history.len(), deepest), wanted, anchor, place }
}

pub(crate) fn restore(ui: &Ui, place: Option<Place>) {
    let Some(place) = place else {
        return;
    };
    let Some(item) = item_of(ui, &place.key) else {
        return;
    };
    let mut scroll = ui.history_scroll.0.borrow_mut();
    if let Some(deferred) = scroll.deferred_scroll_to_item.as_mut().filter(|_| place.pending) {
        deferred.item_index = item;
        return;
    }
    let offset = scroll.base_handle.offset();
    let moved = (item as f32 - place.item as f32) * ROW;
    scroll.base_handle.set_offset(point(offset.x, (offset.y - px(moved)).min(px(0.))));
}

pub(crate) fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(crate) fn tail(ui: &Ui) -> Option<String> {
    if ui.history.is_empty() {
        return None;
    }
    if ui.has_more {
        return Some(LOADING.to_owned());
    }
    if !ui.history_complete {
        return None;
    }
    let count = ui.history.len();
    let start = if ui.sparse.shallow { "History is shallow" } else { "Start of history" };
    Some(format!("{start} · {} {}", grouped(count), if count == 1 { "commit" } else { "commits" }))
}

pub(crate) fn items(ui: &Ui) -> usize {
    ui.history_items() + usize::from(tail(ui).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn commit(f: &Fixture, name: &str, subject: &str) {
        std::fs::write(f.dir.join(name), format!("{subject}\n")).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", subject]);
    }

    fn linear(f: &Fixture, count: usize) {
        let mut stream = String::new();
        for index in 0..count {
            let subject = format!("Step {index}");
            stream.push_str(&format!(
                "commit refs/heads/main\ncommitter Cedar <cedar@example.invalid> {} +0000\ndata {}\n{subject}\n",
                1_800_000_000 + index,
                subject.len() + 1
            ));
            if index == 0 {
                stream.push_str("from refs/heads/main^0\n");
            }
            stream.push('\n');
        }
        let mut child = f
            .repo
            .command()
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), stream.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn subjects(rows: &[Commit]) -> Vec<String> {
        rows.iter().map(|row| row.subject.clone()).collect()
    }

    #[test]
    fn a_stream_reads_the_same_history_page_by_page_as_one_log() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        f.git(&["switch", "-qc", "side"]);
        commit(&f, "b.txt", "Side work");
        f.git(&["switch", "-q", "main"]);
        linear(&f, 4);
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        f.git(&["tag", "-a", "-m", "Ready", "v1"]);
        let whole = f.repo.history(0, 100).unwrap();
        assert_eq!(whole.len(), 7);
        let mut stream = HistoryStream::open(&f.repo, &GraphView::default()).unwrap();
        let mut paged = Vec::new();
        for count in [3, 1, 2] {
            paged.extend(stream.next(count).unwrap());
            assert!(stream.more());
        }
        paged.extend(stream.next(5).unwrap());
        assert!(!stream.more());
        assert_eq!(stream.loaded(), 7);
        assert_eq!(paged, whole);
        assert!(paged[0].refs.contains("tag: v1"));
        assert_eq!(paged[0].parents.len(), 2);
        assert!(stream.next(10).unwrap().is_empty());
    }

    #[test]
    fn the_first_line_of_each_body_is_read_only_when_body_preview_is_on() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        f.git(&["commit", "--allow-empty", "-q", "-m", "Explained", "-m", "\n  Why it changed\nMore detail", "-m", "Later"]);
        f.git(&["commit", "--allow-empty", "-qm", "Plain"]);
        let mut quiet = HistoryStream::open(&f.repo, &GraphView::default()).unwrap();
        let rows = quiet.next(10).unwrap();
        assert_eq!(subjects(&rows), ["Plain", "Explained", "Root"]);
        assert!(rows.iter().all(|row| row.body.is_empty()));
        assert_eq!(rows, f.repo.history(0, 10).unwrap());
        let mut shown = HistoryStream::open(&f.repo, &GraphView { bodies: true, ..GraphView::default() }).unwrap();
        let rows = shown.next(10).unwrap();
        assert_eq!(subjects(&rows), ["Plain", "Explained", "Root"]);
        assert_eq!(rows.iter().map(|row| row.body.as_str()).collect::<Vec<_>>(), ["", "Why it changed", ""]);
        assert!(!shown.more());
        assert_eq!(crate::repository::first_line("\n \n  First \nSecond\n"), "First");
    }

    #[test]
    fn a_page_that_ends_exactly_at_the_root_says_nothing_more_is_left() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        linear(&f, 3);
        let mut stream = HistoryStream::open(&f.repo, &GraphView::default()).unwrap();
        assert_eq!(subjects(&stream.next(4).unwrap()), ["Step 2", "Step 1", "Step 0", "Root"]);
        assert!(!stream.more());
    }

    #[test]
    fn the_stream_lists_what_the_view_lists_and_stops_git_when_dropped() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        f.git(&["switch", "-qc", "hidden"]);
        commit(&f, "h.txt", "Hidden work");
        f.git(&["switch", "-q", "main"]);
        linear(&f, 2);
        let view = GraphView { hidden: ["refs/heads/hidden".to_owned()].into(), ..GraphView::default() };
        let mut stream = HistoryStream::open(&f.repo, &view).unwrap();
        assert_eq!(subjects(&stream.next(10).unwrap()), ["Step 1", "Step 0", "Root"]);
        let mut open = HistoryStream::open(&f.repo, &GraphView::default()).unwrap();
        assert_eq!(open.next(1).unwrap().len(), 1);
        let pid = open.child.id() as i32;
        drop(open);
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    }

    #[test]
    fn a_reload_reads_past_the_kept_rows_when_a_wanted_commit_moved_down() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        linear(&f, PAGE + 20);
        let wanted = String::from_utf8(f.git(&["rev-parse", &format!("HEAD~{}", PAGE)])).unwrap().trim().to_owned();
        let (stream, page) = read(&f.repo, &GraphView::default(), None, 0, PAGE, &[wanted.clone()], None);
        let (page, more) = page.unwrap();
        assert_eq!(page.len(), PAGE + 21);
        assert!(!more);
        assert!(page.iter().any(|row| row.oid == wanted));
        assert_eq!(stream.unwrap().loaded(), PAGE + 21);
        let (stream, first) = read(&f.repo, &GraphView::default(), None, 0, PAGE, &[], None);
        assert_eq!(first.unwrap().0.len(), PAGE);
        let (_, rest) = read(&f.repo, &GraphView::default(), None, PAGE, 5, &[], None);
        let (rest, more) = rest.unwrap();
        assert_eq!(rest.len(), 5);
        assert!(more);
        let (_, next) = read(&f.repo, &GraphView::default(), stream, PAGE, 5, &[], None);
        assert_eq!(next.unwrap().0, rest);
    }

    #[test]
    fn a_reload_finds_the_kept_place_however_far_new_commits_pushed_it_down() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        linear(&f, PAGE * 4);
        let place = String::from_utf8(f.git(&["rev-parse", &format!("HEAD~{}", PAGE * 3)])).unwrap().trim().to_owned();
        let (_, page) = read(&f.repo, &GraphView::default(), None, 0, PAGE, &[place.clone()], Some(&place));
        let (page, more) = page.unwrap();
        assert_eq!(page.iter().position(|row| row.oid == place), Some(PAGE * 3));
        assert_eq!(page.len(), PAGE * 4 + 1);
        assert!(!more);
        let gone = "0".repeat(40);
        let (_, page) = read(&f.repo, &GraphView::default(), None, 0, PAGE, &[gone.clone()], Some(&gone));
        let (page, more) = page.unwrap();
        assert_eq!(page.len(), PAGE * 2);
        assert!(more);
    }

    #[test]
    fn a_git_failure_is_reported_and_drops_the_stream() {
        let f = Fixture::new();
        commit(&f, "a.txt", "Root");
        std::fs::remove_dir_all(f.repo.common_dir().join("objects")).unwrap();
        let (stream, result) = read(&f.repo, &GraphView::default(), None, 0, 5, &[], None);
        assert!(stream.is_none());
        assert!(result.is_err());
    }

    #[test]
    fn up_to_five_thousand_rows_are_reread_and_past_that_one_page_beyond_the_deepest_place() {
        assert_eq!(rows(0, None), PAGE);
        assert_eq!(rows(120, Some(30)), PAGE);
        assert_eq!(rows(1_750, Some(1_700)), 1_750);
        assert_eq!(rows(KEPT, Some(4_999)), KEPT);
        assert_eq!(rows(9_000, None), KEPT);
        assert_eq!(rows(9_000, Some(40)), KEPT);
        assert_eq!(rows(9_000, Some(8_000)), 8_000 + 1 + PAGE);
        assert_eq!(rows(50_000, Some(49_900)), 49_900 + 1 + PAGE);
    }

    #[test]
    fn the_place_is_the_selected_row_when_visible_and_otherwise_the_top_row() {
        let height = ROW * 10.;
        assert_eq!(anchor(0., height, 500, Some(300)), None);
        assert_eq!(anchor(ROW * 100., height, 0, None), None);
        assert_eq!(anchor(ROW * 100. + 5., height, 500, Some(104)), Some(104));
        assert_eq!(anchor(ROW * 100. + 5., height, 500, Some(100)), Some(100));
        assert_eq!(anchor(ROW * 100. + 5., height, 500, Some(99)), Some(100));
        assert_eq!(anchor(ROW * 100. + 5., height, 500, Some(111)), Some(100));
        assert_eq!(anchor(ROW * 100. + 5., height, 500, None), Some(100));
        assert_eq!(anchor(ROW * 600., height, 500, None), Some(499));
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(12_345), "12,345");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
