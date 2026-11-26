use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Side, Target},
    compare::Rev,
    diff_doc::{DiffDoc, PatchState},
    diff_text::Face,
    file_view::{self, Origin, Version},
    jobs::Link,
    repository::{Repository, Run, git_error, stream_into, validate_oid, validate_path},
    theme::*,
    view::{self, Desk, DiffLayout, Selection, Ui},
};
use gpui::{Context, Div, FontWeight, Pixels, Point, Stateful, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

pub(crate) const LIMIT: u64 = 2 * 1024 * 1024;
pub(crate) const TOO_LARGE: &str = "This file is larger than 2 MiB, so it is not blamed.";
pub(crate) const NOT_COMMITTED: &str = "Not committed";
pub(crate) const BINARY: &str = "This is a binary file, so it has no lines to blame.";
pub(crate) const HINT: &str = "Click a line's annotation to see the commit that last changed it.";
const NEVER_COMMITTED: &str = "This file has never been committed";
const NO_COMMITS: &str = "There are no commits yet";
const DELETED_IN_COMPARISON: &str = "This file is deleted on the newer side, so it has no lines to blame";
const POLL: Duration = Duration::from_millis(40);
const SEPARATOR_ALPHA: u8 = 0x26;
const CHOSEN_ALPHA: u8 = 0x4c;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum At {
    Working,
    Commit(String),
    ParentOf(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blamed {
    pub(crate) path: PathBuf,
    pub(crate) at: At,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) blamed: Blamed,
    owner: Option<Selection>,
    diff_open: bool,
}

impl Request {
    pub(crate) fn at(ui: &Ui, path: &Path, at: At) -> Self {
        Self { blamed: Blamed { path: path.to_owned(), at }, owner: ui.selection.clone(), diff_open: ui.diff_open }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) oid: String,
    pub(crate) author: String,
    pub(crate) email: String,
    pub(crate) time: i64,
    pub(crate) date: String,
    pub(crate) summary: String,
}

impl Commit {
    pub(crate) fn committed(&self) -> bool {
        !self.oid.bytes().all(|b| b == b'0')
    }

    pub(crate) fn short(&self) -> String {
        self.oid.chars().take(7).collect()
    }

    fn hue(&self) -> usize {
        let prefix = self.oid.get(..8).unwrap_or("0");
        u32::from_str_radix(prefix, 16).unwrap_or(0) as usize % palette().lanes.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) commit: usize,
    pub(crate) start: usize,
    pub(crate) count: usize,
    pub(crate) path: PathBuf,
    pub(crate) previous: Option<(String, PathBuf)>,
    hue: usize,
}

#[derive(Default)]
struct Partial {
    oid: String,
    start: usize,
    count: usize,
    author: Option<String>,
    email: Option<String>,
    time: Option<i64>,
    summary: Option<String>,
    previous: Option<(String, PathBuf)>,
}

#[derive(Default)]
pub(crate) struct Annotations {
    pub(crate) commits: Vec<Arc<Commit>>,
    known: HashMap<String, usize>,
    pub(crate) blocks: Vec<Block>,
    lines: Vec<u32>,
    pending: Vec<u8>,
    partial: Option<Partial>,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn name(bytes: &[u8]) -> Result<PathBuf, String> {
    let raw = if bytes.first() == Some(&b'"') {
        crate::diff_select::unquoted(bytes).ok_or("Unreadable file name in the blame")?
    } else {
        bytes.to_vec()
    };
    Ok(crate::repository::raw_path(&raw))
}

impl Annotations {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<bool, String> {
        self.pending.extend_from_slice(bytes);
        let Some(end) = self.pending.iter().rposition(|&b| b == b'\n') else { return Ok(false) };
        let complete: Vec<u8> = self.pending.drain(..=end).collect();
        let before = self.blocks.len();
        for line in complete.split(|&b| b == b'\n') {
            if !line.is_empty() {
                self.line(line)?;
            }
        }
        let added = self.blocks.len() != before;
        if added {
            self.tint();
        }
        Ok(added)
    }

    pub(crate) fn finished(&self) -> bool {
        self.pending.is_empty() && self.partial.is_none()
    }

    fn line(&mut self, line: &[u8]) -> Result<(), String> {
        let Some(partial) = self.partial.as_mut() else {
            let mut fields = line.split(|&b| b == b' ');
            let oid = text(fields.next().unwrap_or_default());
            validate_oid(&oid).map_err(|_| "Unreadable blame record".to_owned())?;
            let number = |field: Option<&[u8]>| -> Result<usize, String> {
                std::str::from_utf8(field.unwrap_or_default())
                    .ok()
                    .and_then(|field| field.parse().ok())
                    .ok_or_else(|| "Unreadable blame line numbers".to_owned())
            };
            let _original = number(fields.next())?;
            let start = number(fields.next())?.checked_sub(1).ok_or("Unreadable blame line numbers")?;
            let count = number(fields.next())?;
            self.partial = Some(Partial { oid, start, count, ..Partial::default() });
            return Ok(());
        };
        let (key, value) = match line.iter().position(|&b| b == b' ') {
            Some(at) => (&line[..at], &line[at + 1..]),
            None => (line, &[][..]),
        };
        match key {
            b"author" => partial.author = Some(text(value)),
            b"author-mail" => {
                let mail = text(value);
                partial.email = Some(mail.trim_start_matches('<').trim_end_matches('>').to_owned());
            }
            b"author-time" => partial.time = std::str::from_utf8(value).ok().and_then(|time| time.parse().ok()),
            b"summary" => partial.summary = Some(text(value)),
            b"previous" => {
                let at = value.iter().position(|&b| b == b' ').ok_or("Unreadable blame record")?;
                let oid = text(&value[..at]);
                validate_oid(&oid).map_err(|_| "Unreadable blame record".to_owned())?;
                partial.previous = Some((oid, name(&value[at + 1..])?));
            }
            b"filename" => {
                let path = name(value)?;
                let partial = self.partial.take().unwrap_or_default();
                self.finish(partial, path);
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, partial: Partial, path: PathBuf) {
        let commit = match self.known.get(&partial.oid) {
            Some(&index) => index,
            None => {
                let time = partial.time.unwrap_or(0);
                let commit = Commit {
                    oid: partial.oid.clone(),
                    author: partial.author.unwrap_or_default(),
                    email: partial.email.unwrap_or_default(),
                    time,
                    date: view::date_of(time),
                    summary: partial.summary.unwrap_or_default(),
                };
                self.commits.push(Arc::new(commit));
                self.known.insert(partial.oid, self.commits.len() - 1);
                self.commits.len() - 1
            }
        };
        let index = u32::try_from(self.blocks.len() + 1).unwrap_or(u32::MAX);
        let end = partial.start + partial.count;
        if self.lines.len() < end {
            self.lines.resize(end, 0);
        }
        self.lines[partial.start..end].fill(index);
        let hue = self.commits[commit].hue();
        self.blocks.push(Block { commit, start: partial.start, count: partial.count, path, previous: partial.previous, hue });
    }

    fn tint(&mut self) {
        let mut above: Option<(usize, usize)> = None;
        let mut line = 0;
        while line < self.lines.len() {
            let Some(index) = self.block_of(line) else {
                above = None;
                line += 1;
                continue;
            };
            let block = &self.blocks[index];
            let commit = block.commit;
            let mut hue = self.commits[commit].hue();
            if let Some((other, other_hue)) = above
                && other != commit
                && other_hue == hue
            {
                hue = (hue + 1) % palette().lanes.len();
            }
            let next = block.start + block.count.max(1);
            self.blocks[index].hue = hue;
            above = Some((commit, hue));
            line = next.max(line + 1);
        }
    }

    pub(crate) fn block_of(&self, line: usize) -> Option<usize> {
        let index = *self.lines.get(line)?;
        (index > 0).then(|| index as usize - 1)
    }

    pub(crate) fn commit_of(&self, line: usize) -> Option<&Arc<Commit>> {
        self.commits.get(self.blocks.get(self.block_of(line)?)?.commit)
    }

    pub(crate) fn covered(&self) -> usize {
        self.lines.iter().filter(|&&index| index > 0).count()
    }
}

pub(crate) struct Prepared {
    resolved: Option<String>,
    doc: DiffDoc,
    layout: DiffLayout,
    refused: Option<String>,
    stamp: Option<(u64, SystemTime)>,
}

impl Repository {
    fn resolve_commit(&self, revision: &str) -> Result<String, String> {
        let output = self
            .read_command()
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options", revision])
            .run()
            .map_err(|error| format!("Run bundled Git: {error}"))?;
        let oid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || validate_oid(&oid).is_err() {
            return Err("There is no earlier version of this file.".to_owned());
        }
        Ok(oid)
    }

    pub(crate) fn blame_revision(&self, blamed: &Blamed) -> Result<Option<String>, String> {
        validate_path(&blamed.path)?;
        Ok(match &blamed.at {
            At::Working if std::fs::symlink_metadata(self.root.join(&blamed.path)).is_ok_and(|meta| !meta.is_dir()) => None,
            At::Working => Some(self.resolve_commit("HEAD^{commit}")?),
            At::Commit(oid) => {
                validate_oid(oid)?;
                Some(oid.clone())
            }
            At::ParentOf(oid) => {
                validate_oid(oid)?;
                Some(self.resolve_commit(&format!("{oid}^1^{{commit}}"))?)
            }
        })
    }

    pub(crate) fn blame_contents(&self, blamed: &Blamed, tab: usize) -> Result<Prepared, String> {
        let resolved = self.blame_revision(blamed)?;
        let path = &blamed.path;
        let (origin, version, size) = match &resolved {
            None => {
                let disk = self.root.join(path);
                let size = std::fs::symlink_metadata(&disk).map_err(|error| error.to_string())?.len();
                (Origin::Working(disk), Version::Working, size)
            }
            Some(oid) => {
                let name = path.to_str().ok_or("This file's name cannot be looked up.")?;
                let (blob, size) = self.blob_named(&format!("{oid}:{name}"))?;
                (Origin::Stored(blob), Version::Changed, size)
            }
        };
        let stamp = match &origin {
            Origin::Working(disk) => crate::image_diff::stamp(disk),
            _ => None,
        };
        if size > LIMIT {
            let mut doc = DiffDoc::default();
            doc.state = PatchState::Unreadable(TOO_LARGE.to_owned());
            return Ok(Prepared { resolved, doc, layout: DiffLayout::default(), refused: Some(TOO_LARGE.to_owned()), stamp });
        }
        let contents = self.file_contents(&origin, LIMIT as usize)?;
        let (doc, _) = file_view::shape(&DiffDoc::default(), &Selection::Changes, path, &origin, version, contents);
        let refused = matches!(doc.state, PatchState::Binary { .. }).then(|| BINARY.to_owned());
        let layout = DiffLayout::new(&doc, tab);
        Ok(Prepared { resolved, doc, layout, refused, stamp })
    }

    pub(crate) fn blame_lines(
        &self,
        revision: Option<&str>,
        path: &Path,
        link: &Link,
        each: impl FnMut(&[u8]) + Send + 'static,
    ) -> Result<(), String> {
        validate_path(path)?;
        let mut command = self.read_command();
        command.args(["blame", "--incremental"]);
        if let Some(oid) = revision {
            validate_oid(oid)?;
            command.arg(oid);
        }
        command.arg("--").arg(path);
        let (started, at) = (Instant::now(), SystemTime::now());
        let result = stream_into(&mut command, link, each);
        crate::activity::record(&command, started, at, &result);
        let output = result.map_err(|error| format!("Run bundled Git: {error}"))?;
        if !output.status.success() {
            return Err(git_error(&output.stderr, output.status));
        }
        Ok(())
    }
}

struct Open {
    request: Request,
    stack: Vec<Blamed>,
    places: Vec<Point<Pixels>>,
    resolved: Option<String>,
    notes: Annotations,
    feed: Arc<Mutex<Vec<u8>>>,
    loading: bool,
    reading: bool,
    refused: Option<String>,
    failed: Option<String>,
    link: Option<Link>,
    chosen: Option<usize>,
    stamp: Option<(u64, SystemTime)>,
}

#[derive(Default)]
pub(crate) struct State {
    open: Option<Open>,
    ticket: u64,
    pub(crate) bare: bool,
}

impl State {
    pub(crate) fn open(&self) -> bool {
        self.open.is_some()
    }

    pub(crate) fn blamed(&self) -> Option<&Blamed> {
        self.open.as_ref()?.stack.last()
    }

    pub(crate) fn depth(&self) -> usize {
        self.open.as_ref().map_or(0, |open| open.stack.len())
    }

    pub(crate) fn resolved(&self) -> Option<&str> {
        self.open.as_ref()?.resolved.as_deref()
    }

    pub(crate) fn loading(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.loading)
    }

    pub(crate) fn reading(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.reading)
    }

    pub(crate) fn settled(&self) -> bool {
        self.open.as_ref().is_some_and(|open| !open.loading && !open.reading)
    }

    pub(crate) fn refused(&self) -> Option<&str> {
        self.open.as_ref()?.refused.as_deref()
    }

    pub(crate) fn failed(&self) -> Option<&str> {
        self.open.as_ref()?.failed.as_deref()
    }

    pub(crate) fn notes(&self) -> Option<&Annotations> {
        self.open.as_ref().map(|open| &open.notes)
    }

    pub(crate) fn chosen(&self) -> Option<(&Block, &Arc<Commit>)> {
        let open = self.open.as_ref()?;
        let block = open.notes.blocks.get(open.chosen?)?;
        Some((block, open.notes.commits.get(block.commit)?))
    }

    fn stop(&mut self) {
        if let Some(link) = self.open.as_mut().and_then(|open| open.link.take()) {
            link.cancel();
        }
    }

    fn close(&mut self) -> Option<Request> {
        self.stop();
        self.ticket += 1;
        self.open.take().map(|open| open.request)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn showing(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::Blame
}

pub(crate) fn width(ui: &Ui) -> f32 {
    if !ui.blame.open() {
        return 0.;
    }
    let face = &ui.diff_text.face;
    if ui.blame.bare { (face.marker() / 2.).round() } else { (face.size as f32 * 14.).round() }
}

fn unavailable(ui: &Ui, path: &Path, side: &Side) -> Option<String> {
    if ui.head.is_none() {
        return Some(NO_COMMITS.to_owned());
    }
    match side {
        Side::Unstaged | Side::Staged => {
            let staged = *side == Side::Staged;
            let row = ui.files.iter().find(|row| row.path == path && row.staged == staged);
            row.is_some_and(|row| row.status == "Untracked").then(|| NEVER_COMMITTED.to_owned())
        }
        Side::Compare(Rev::Commit(_)) if compared_deleted(ui, path) => Some(DELETED_IN_COMPARISON.to_owned()),
        Side::Commit(_) | Side::Stash(_) | Side::Compare(_) => None,
    }
}

fn compared_deleted(ui: &Ui, path: &Path) -> bool {
    ui.compare.files.iter().any(|file| file.path == path && file.status == "Deleted")
}

fn deleted(file: Option<&crate::repository::ChangedFile>) -> bool {
    file.is_some_and(|file| file.status == "Deleted")
}

pub(crate) fn request(ui: &Ui, path: &Path, side: &Side) -> Option<Request> {
    let at = match side {
        Side::Unstaged | Side::Staged => At::Working,
        Side::Commit(oid) if deleted(ui.inspection.file(oid, path)) => At::ParentOf(oid.clone()),
        Side::Commit(oid) => At::Commit(oid.clone()),
        Side::Stash(oid) if deleted(ui.stash.file(oid, path)) => At::ParentOf(oid.clone()),
        Side::Stash(oid) => At::Commit(oid.clone()),
        Side::Compare(Rev::Commit(_)) if compared_deleted(ui, path) => return None,
        Side::Compare(Rev::Commit(oid)) => At::Commit(oid.clone()),
        Side::Compare(Rev::WorkingTree) => At::Working,
    };
    Some(Request { blamed: Blamed { path: path.to_owned(), at }, owner: ui.selection.clone(), diff_open: ui.diff_open })
}

pub(crate) fn here(ui: &Ui) -> Option<Request> {
    match ui.selection.as_ref()? {
        Selection::File { path, staged } => request(ui, path, if *staged { &Side::Staged } else { &Side::Unstaged }),
        Selection::Commit(oid) => request(ui, ui.inspection.path.as_deref()?, &Side::Commit(oid.clone())),
        Selection::Stash(oid) => request(ui, ui.stash.open_path()?, &Side::Stash(oid.clone())),
        Selection::Compare { to, .. } => request(ui, ui.compare.path.as_deref()?, &Side::Compare(to.clone())),
        Selection::Changes | Selection::Commits => None,
    }
}

pub(crate) fn refused_here(ui: &Ui) -> Option<String> {
    let Some(Selection::File { path, staged }) = &ui.selection else {
        return ui.head.is_none().then(|| NO_COMMITS.to_owned());
    };
    unavailable(ui, path, if *staged { &Side::Staged } else { &Side::Unstaged })
}

pub(crate) fn offered_here(ui: &Ui) -> bool {
    showing(ui) || here(ui).is_some()
}

pub(crate) fn selected(tab: &mut Tab, selection: &Selection) -> bool {
    let Some(open) = &tab.ui.blame.open else { return false };
    if open.request.owner.as_ref() == Some(selection) && tab.ui.selection.as_ref() == Some(selection) {
        return true;
    }
    tab.ui.blame.close();
    tab.ui.diff_options.measured = px(0.);
    false
}

pub(crate) fn leave(tab: &mut Tab) {
    if tab.ui.blame.close().is_some() {
        tab.ui.diff_options.measured = px(0.);
    }
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(open), Some(repo)) = (&tab.ui.blame.open, &tab.repo) else { return };
    if open.loading || open.resolved.is_some() || open.stamp.is_none() {
        return;
    }
    let Some(blamed) = open.stack.last() else { return };
    if crate::image_diff::stamp(&repo.root.join(&blamed.path)) != open.stamp {
        let place = scrolled(&tab.ui);
        load(tab, Some(place), cx);
    }
}

pub(crate) fn placeholder(ui: &Ui) -> Option<String> {
    if !showing(ui) {
        return None;
    }
    let state = &ui.blame;
    if let Some(reason) = state.refused().filter(|_| ui.diff.lines.is_empty()) {
        return Some(reason.to_owned());
    }
    if state.loading() {
        return Some(file_view::LOADING.to_owned());
    }
    match &ui.diff.state {
        PatchState::Unreadable(reason) => Some(format!("This file could not be shown. {reason}")),
        _ => None,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "file.blame",
        Group::View,
        |_, _| "Blame".into(),
        |ui, target, _| match target {
            Target::File { path, side } => Availability::unless(unavailable(ui, path, side)),
            _ => Availability::Hidden,
        },
        |ui, target| match target {
            Target::File { path, side } => request(ui, path, side).map(|request| view::Action::Blame(Action::Open(request))),
            _ => None,
        },
    )
    .words(&["annotate", "who changed this line", "author of each line", "praise"])
    .icon("commit"),
];

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(Request),
    OpenHere,
    Choose(usize),
    Step(isize),
    Unchoose,
    Before,
    ShowInGraph,
    CopySha,
    Details(bool),
    Back,
    Close,
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(request) => open(tab, request, cx),
        Action::OpenHere => {
            if refused_here(&tab.ui).is_none()
                && let Some(request) = here(&tab.ui)
            {
                open(tab, request, cx);
            }
        }
        Action::Choose(line) => {
            if let Some(open) = tab.ui.blame.open.as_mut() {
                open.chosen = open.notes.block_of(line);
            }
        }
        Action::Step(delta) => step(&mut tab.ui, delta),
        Action::Unchoose => {
            if let Some(open) = tab.ui.blame.open.as_mut() {
                open.chosen = None;
            }
        }
        Action::Before => before(tab, cx),
        Action::ShowInGraph => {
            let Some((_, commit)) = tab.ui.blame.chosen().filter(|(_, commit)| commit.committed()) else { return };
            let oid = commit.oid.clone();
            close(tab, cx);
            crate::file_history::leave(tab);
            tab.ui.diff_open = false;
            crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
        }
        Action::CopySha => {
            let Some((_, commit)) = tab.ui.blame.chosen().filter(|(_, commit)| commit.committed()) else { return };
            let action = crate::inspection::copy_sha(&commit.oid);
            tab.dispatch(action, window, cx);
        }
        Action::Details(shown) => {
            tab.ui.blame.bare = !shown;
            tab.ui.diff_options.measured = px(0.);
        }
        Action::Back => {
            let Some(open) = tab.ui.blame.open.as_mut() else { return };
            if open.stack.len() > 1 {
                open.stack.pop();
                let place = open.places.pop();
                load(tab, place, cx);
            } else {
                close(tab, cx);
            }
        }
        Action::Close => close(tab, cx),
    }
    cx.notify();
}

fn step(ui: &mut Ui, delta: isize) {
    let Some(open) = ui.blame.open.as_mut() else { return };
    let notes = &open.notes;
    let mut starts: Vec<(usize, usize)> = notes.blocks.iter().enumerate().map(|(index, block)| (block.start, index)).collect();
    if starts.is_empty() {
        return;
    }
    starts.sort_unstable();
    let at = open.chosen.and_then(|chosen| starts.iter().position(|(_, index)| *index == chosen));
    let last = starts.len() - 1;
    let wanted = match (delta, at) {
        (isize::MIN, _) => 0,
        (isize::MAX, _) => last,
        (_, Some(at)) => at.saturating_add_signed(delta).min(last),
        (delta, None) if delta < 0 => last,
        _ => 0,
    };
    let (line, index) = starts[wanted];
    open.chosen = Some(index);
    if let Some(visual) = view::diff_visual_of(ui, line) {
        ui.diff_scroll.scroll_to_item(visual, gpui::ScrollStrategy::Center);
    }
}

fn open(tab: &mut Tab, mut request: Request, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    let state = &mut tab.ui.blame;
    if let Some(open) = &state.open {
        request.owner = open.request.owner.clone();
        request.diff_open = open.request.diff_open;
    }
    state.close();
    state.open = Some(Open {
        stack: vec![request.blamed.clone()],
        places: Vec::new(),
        request,
        resolved: None,
        notes: Annotations::default(),
        feed: Arc::default(),
        loading: true,
        reading: false,
        refused: None,
        failed: None,
        link: None,
        chosen: None,
        stamp: None,
    });
    tab.ui.diff_open = true;
    load(tab, None, cx);
}

fn before(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some((block, _)) = tab.ui.blame.chosen() else { return };
    let Some((oid, path)) = block.previous.clone() else { return };
    let place = scrolled(&tab.ui);
    let Some(open) = tab.ui.blame.open.as_mut() else { return };
    open.stack.push(Blamed { path, at: At::Commit(oid) });
    open.places.push(place);
    load(tab, Some(place), cx);
}

fn close(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(request) = tab.ui.blame.close() else { return };
    tab.ui.diff_options.measured = px(0.);
    tab.ui.diff_open = request.diff_open;
    tab.clear_diff();
    if tab.ui.selection == request.owner {
        crate::diff_options::reload(tab, cx);
    }
}

fn scrolled(ui: &Ui) -> Point<Pixels> {
    ui.diff_scroll.0.borrow().base_handle.offset()
}

fn load(tab: &mut Tab, place: Option<Point<Pixels>>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let face_tab = tab.ui.diff_text.face.tab;
    let state = &mut tab.ui.blame;
    state.stop();
    state.ticket += 1;
    let ticket = state.ticket;
    let Some(open) = state.open.as_mut() else { return };
    let Some(blamed) = open.stack.last().cloned() else { return };
    open.resolved = None;
    open.notes = Annotations::default();
    open.feed = Arc::default();
    open.loading = true;
    open.reading = false;
    open.refused = None;
    open.failed = None;
    open.chosen = None;
    open.stamp = None;
    let owner = open.request.owner.clone();
    tab.diff_ticket += 1;
    tab.diff = None;
    let mut waiting = DiffDoc::default();
    waiting.state = PatchState::Loading;
    tab.ui.replace_diff(waiting, DiffLayout::default());
    tab.ui.diff_title = blamed.path.to_string_lossy().into_owned();
    if let Some(owner) = &owner {
        crate::image_diff::follow(tab, None, crate::image_diff::owner(owner, None));
    }
    crate::lines::reloaded(&mut tab.ui);
    crate::diff_select::reloaded(&mut tab.ui);
    tab.ui.diff_options.measured = px(0.);
    let epoch = tab.epoch;
    let path = blamed.path.clone();
    Tab::work(
        cx,
        move || repo.blame_contents(&blamed, face_tab),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.blame.ticket != ticket {
                return;
            }
            let Some(open) = tab.ui.blame.open.as_mut() else { return };
            open.loading = false;
            match result {
                Ok(prepared) => {
                    open.resolved = prepared.resolved.clone();
                    open.refused = prepared.refused;
                    open.stamp = prepared.stamp;
                    let reading = open.refused.is_none();
                    let shown_tab = tab.ui.diff_text.face.tab;
                    let layout = if shown_tab == face_tab { prepared.layout } else { DiffLayout::new(&prepared.doc, shown_tab) };
                    tab.diff_ticket += 1;
                    tab.ui.replace_diff(prepared.doc, layout);
                    if let Some(place) = place {
                        tab.ui.diff_scroll.0.borrow().base_handle.set_offset(place);
                    }
                    crate::lines::reloaded(&mut tab.ui);
                    crate::diff_select::reloaded(&mut tab.ui);
                    if tab.ui.syntax.on {
                        crate::syntax::start(tab, cx);
                    }
                    if reading {
                        read(tab, ticket, prepared.resolved, path, cx);
                    }
                }
                Err(error) => {
                    open.failed = Some(error.clone());
                    tab.ui.diff.state = PatchState::Unreadable(error);
                }
            }
            cx.notify();
        },
    );
}

fn drain(state: &mut State) -> bool {
    let Some(open) = state.open.as_mut() else { return false };
    let bytes = std::mem::take(&mut *open.feed.lock().unwrap_or_else(|e| e.into_inner()));
    if bytes.is_empty() {
        return false;
    }
    match open.notes.feed(&bytes) {
        Ok(changed) => changed,
        Err(error) => {
            open.failed = Some(error);
            if let Some(link) = open.link.take() {
                link.cancel();
            }
            true
        }
    }
}

fn read(tab: &mut Tab, ticket: u64, revision: Option<String>, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let Some(open) = tab.ui.blame.open.as_mut() else { return };
    let link = Link::new(true);
    let feed = open.feed.clone();
    open.link = Some(link.clone());
    open.reading = true;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || {
            repo.blame_lines(revision.as_deref(), &path, &link, move |bytes| {
                feed.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(bytes);
            })
        },
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.blame.ticket != ticket {
                return;
            }
            drain(&mut tab.ui.blame);
            let Some(open) = tab.ui.blame.open.as_mut() else { return };
            open.reading = false;
            open.link = None;
            if let Err(error) = result {
                open.failed.get_or_insert(error);
            } else if !open.notes.finished() && open.failed.is_none() {
                open.failed = Some("Git ended the blame early.".to_owned());
            }
            cx.notify();
        },
    );
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            let going = tab.update(cx, |tab, cx| {
                if tab.epoch != epoch || tab.ui.blame.ticket != ticket {
                    return false;
                }
                if drain(&mut tab.ui.blame) {
                    cx.notify();
                }
                tab.ui.blame.reading()
            });
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

pub(crate) fn label(ui: &Ui, line: usize) -> Option<String> {
    let notes = ui.blame.notes()?;
    let block = notes.blocks.get(notes.block_of(line)?)?;
    if block.start != line {
        return None;
    }
    let commit = notes.commits.get(block.commit)?;
    Some(if commit.committed() {
        format!("{}  {}", view::when(commit.time, &commit.date, view::now()), commit.author)
    } else {
        NOT_COMMITTED.to_owned()
    })
}

pub(crate) fn tooltip(commit: &Commit) -> String {
    if !commit.committed() {
        return format!("{NOT_COMMITTED}\nThese lines differ from the last commit");
    }
    format!("{}\n{} · {}\n{}", commit.summary, commit.author, view::exact_time(commit.time), commit.short())
}

pub(crate) fn cell<T: Desk>(ui: &Ui, line: usize, first: bool, face: &Face, cx: &Context<T>) -> Option<Stateful<Div>> {
    let open = ui.blame.open.as_ref()?;
    let base = div()
        .id(("blame-line", line))
        .w(px(width(ui)))
        .h_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .border_r_1()
        .border_color(rgb(palette().line));
    let Some(index) = open.notes.block_of(line) else { return Some(base) };
    let block = &open.notes.blocks[index];
    let commit = open.notes.commits.get(block.commit)?.clone();
    let committed = commit.committed();
    let chosen = open.chosen == Some(index);
    let opening = first && block.start == line;
    let ink = palette().lanes[block.hue % palette().lanes.len()];
    let text = (opening && !ui.blame.bare).then(|| label(ui, line)).flatten();
    let tip = commit.clone();
    Some(
        base.cursor_pointer()
            .when(committed, |d| d.bg(soft(ink, if chosen { CHOSEN_ALPHA } else { SEPARATOR_ALPHA })))
            .when(!committed && chosen, |d| d.bg(rgb(palette().selected)))
            .when(opening, |d| d.border_t_1())
            .hover(|s| s.bg(rgb(palette().hover)))
            .child(div().w(px(3.)).h_full().flex_shrink_0().when(committed, |d| d.bg(rgb(ink))))
            .when_some(text, |d, text| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(6.))
                        .pr(px(4.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(face.small())
                        .text_color(rgb(if committed { palette().text_2 } else { palette().text_3 }))
                        .child(text),
                )
            })
            .tooltip(kit::tip_with(move || tooltip(&tip)))
            .on_click(cx.listener(move |app, _, w, cx| app.dispatch(view::Action::Blame(Action::Choose(line)), w, cx))),
    )
}

pub(crate) fn title(ui: &Ui) -> Option<(String, String)> {
    let blamed = ui.blame.blamed()?;
    let at = match ui.blame.resolved() {
        Some(oid) => format!("At {}", oid.chars().take(7).collect::<String>()),
        None if ui.blame.loading() => String::new(),
        None => "Working copy".to_owned(),
    };
    Some((blamed.path.to_string_lossy().into_owned(), at))
}

pub(crate) fn button<T: Desk>(ui: &Ui, cx: &Context<T>) -> impl IntoElement + use<T> {
    let blaming = showing(ui);
    let refused = if blaming { None } else { refused_here(ui) };
    let tip = match &refused {
        Some(reason) => reason.clone(),
        None if blaming => "Back to where you were (Esc)".to_owned(),
        None => "Who last changed each line".to_owned(),
    };
    kit::btn("file-blame", "Blame")
        .small()
        .ghost()
        .selected(blaming)
        .enabled(refused.is_none())
        .tip(tip)
        .on(view::Action::Blame(if blaming { Action::Close } else { Action::OpenHere }), cx)
}

pub(crate) fn details<T: Desk>(ui: &Ui, cx: &Context<T>) -> impl IntoElement + use<T> {
    let shown = !ui.blame.bare;
    kit::btn("blame-details", "Details")
        .small()
        .ghost()
        .selected(shown)
        .tip(if shown { "Hide the author and date beside each change" } else { "Show the author and date beside each change" })
        .on(view::Action::Blame(Action::Details(!shown)), cx)
}

pub(crate) fn status(ui: &Ui) -> Option<String> {
    let open = ui.blame.open.as_ref()?;
    if let Some(error) = &open.failed {
        return Some(format!("The blame could not be read. {error}"));
    }
    if let Some(reason) = &open.refused {
        return Some(reason.clone());
    }
    if open.loading {
        return Some("Loading file…".to_owned());
    }
    let total = ui.diff.lines.len();
    if open.reading {
        return Some(format!("Blaming… {} of {total} lines", open.notes.covered().min(total)));
    }
    Some(HINT.to_owned())
}

fn strip<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let base = div()
        .flex_shrink_0()
        .min_h(px(40.))
        .px_3()
        .py(px(6.))
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(palette().panel))
        .border_t_1()
        .border_color(rgb(palette().line));
    let Some((block, commit)) = ui.blame.chosen() else {
        return base
            .when(ui.blame.reading(), |d| d.child(kit::spinner("blame-reading")))
            .child(kit::muted(status(ui).unwrap_or_default()));
    };
    let shown_path = ui.blame.blamed().map(|blamed| blamed.path.clone());
    let moved = (Some(&block.path) != shown_path.as_ref()).then(|| format!("as {}", block.path.display()));
    let earlier = block.previous.is_some();
    let before = kit::btn("blame-before", "Blame before this change")
        .small()
        .enabled(earlier)
        .tip(if earlier { "Blame the file as it was just before this change" } else { "These lines have no earlier version" })
        .on(view::Action::Blame(Action::Before), cx);
    let dismiss = kit::icon_btn("blame-unchoose", "x", "Close the details", view::Action::Blame(Action::Unchoose), cx);
    if !commit.committed() {
        return base
            .child(div().text_size(px(TEXT_SMALL)).font_weight(FontWeight::SEMIBOLD).child(NOT_COMMITTED))
            .child(kit::fit("These lines differ from the last commit").size(TEXT_SMALL).color(palette().text_3).grow())
            .child(before)
            .child(dismiss);
    }
    let when = view::when(commit.time, &commit.date, view::now());
    let time = commit.time;
    base.child(
        div()
            .flex_shrink_0()
            .font_family(MONO)
            .text_size(px(11.5))
            .text_color(rgb(palette().text_3))
            .child(commit.short()),
    )
    .child(kit::fit(commit.summary.clone()).size(TEXT_SMALL).weight(FontWeight::MEDIUM).grow())
    .child(
        div()
            .id("blame-chosen-when")
            .flex_shrink_0()
            .tooltip(kit::tip_with(move || view::exact_time(time)))
            .child(kit::muted(format!("{} · {when}", commit.author))),
    )
    .when_some(moved, |d, moved| d.child(kit::fit(moved).size(11.5).color(palette().text_2).cut(kit::Cut::Start)))
    .child(kit::btn("blame-copy-sha", "Copy SHA").small().ghost().on(view::Action::Blame(Action::CopySha), cx))
    .child(kit::btn("blame-show-in-graph", "Show in graph").small().ghost().on(view::Action::Blame(Action::ShowInGraph), cx))
    .child(before)
    .child(dismiss)
}

pub(crate) fn render<T: Desk>(app: &T, window: &Window, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let strip = strip(ui, cx);
    div().flex_1().min_w_0().min_h_0().flex().flex_col().child(view::diff_pane(app, window, cx)).child(strip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    const RECORDED: &[u8] = b"\
5e4b2b6a1f0d6c3a9e8d7c6b5a4f3e2d1c0b9a88 1 1 2
author Ann Example
author-mail <ann@example.invalid>
author-time 1767225600
author-tz +0000
committer Ann Example
committer-mail <ann@example.invalid>
committer-time 1767225600
committer-tz +0000
summary Move the notes
previous 1111111111111111111111111111111111111111 \"old notes \\303\\251.txt\"
filename \"docs/notes \\303\\251.md\"
2222222222222222222222222222222222222222 3 3 2
author Bo
author-mail <bo@example.invalid>
author-time 1767139200
author-tz +0000
committer Bo
committer-mail <bo@example.invalid>
committer-time 1767139200
committer-tz +0000
summary Start the notes
boundary
filename old notes.txt
5e4b2b6a1f0d6c3a9e8d7c6b5a4f3e2d1c0b9a88 5 5 1
previous 1111111111111111111111111111111111111111 \"old notes \\303\\251.txt\"
filename \"docs/notes \\303\\251.md\"
0000000000000000000000000000000000000000 6 6 1
author Not Committed Yet
author-mail <not.committed.yet>
author-time 1767312000
author-tz +0000
committer Not Committed Yet
committer-mail <not.committed.yet>
committer-time 1767312000
committer-tz +0000
summary Version of docs/notes from docs/notes
previous 5e4b2b6a1f0d6c3a9e8d7c6b5a4f3e2d1c0b9a88 \"docs/notes \\303\\251.md\"
filename \"docs/notes \\303\\251.md\"
";

    #[test]
    fn the_incremental_blame_names_each_block_its_commit_and_the_file_name_it_had_even_in_pieces() {
        for size in [1, 7, 64, RECORDED.len()] {
            let mut notes = Annotations::default();
            for piece in RECORDED.chunks(size) {
                notes.feed(piece).unwrap();
            }
            assert!(notes.finished());
            assert_eq!(notes.blocks.len(), 4);
            assert_eq!(notes.commits.len(), 3);
            assert_eq!(notes.covered(), 6);
            let commits: Vec<&str> = (0..6).map(|line| notes.commit_of(line).unwrap().oid.as_str()).collect();
            let moved = "5e4b2b6a1f0d6c3a9e8d7c6b5a4f3e2d1c0b9a88";
            let started = "2222222222222222222222222222222222222222";
            assert_eq!(commits, [moved, moved, started, started, moved, "0000000000000000000000000000000000000000"]);
            let first = &notes.blocks[0];
            assert_eq!((first.start, first.count), (0, 2));
            assert_eq!(first.path, Path::new("docs/notes é.md"));
            assert_eq!(
                first.previous,
                Some(("1111111111111111111111111111111111111111".to_owned(), PathBuf::from("old notes é.txt")))
            );
            let again = &notes.blocks[2];
            assert_eq!(again.commit, first.commit);
            assert_eq!(again.path, first.path);
            let start = &notes.blocks[1];
            assert_eq!((start.path.as_path(), start.previous.as_ref()), (Path::new("old notes.txt"), None));
            let author = notes.commit_of(0).unwrap();
            assert_eq!(
                (author.author.as_str(), author.email.as_str(), author.time, author.date.as_str(), author.summary.as_str()),
                ("Ann Example", "ann@example.invalid", 1767225600, "2026-01-01", "Move the notes")
            );
            assert!(author.committed());
            let working = notes.commit_of(5).unwrap();
            assert!(!working.committed());
            assert_eq!(notes.blocks[3].previous.as_ref().map(|(oid, _)| oid.as_str()), Some(moved));
            assert_ne!(notes.blocks[0].hue, notes.blocks[1].hue);
            assert_ne!(notes.blocks[1].hue, notes.blocks[2].hue);
        }
        let mut broken = Annotations::default();
        assert!(broken.feed(b"not-an-id 1 1 1\n").is_err());
        let mut half = Annotations::default();
        half.feed(&RECORDED[..40]).unwrap();
        assert!(!half.finished());
    }

    fn write(f: &Fixture, path: &str, content: &str) {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(f.dir.join(parent)).unwrap();
        }
        fs::write(f.dir.join(path), content).unwrap();
        f.repo.stage_file(Path::new(path)).unwrap();
    }

    fn head(f: &Fixture, spec: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", spec])).unwrap().trim().to_owned()
    }

    fn untouched(f: &Fixture) -> Vec<Vec<u8>> {
        vec![
            f.git(&["for-each-ref"]),
            f.git(&["ls-files", "-s"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
        ]
    }

    fn blamed(f: &Fixture, revision: Option<&str>, path: &str) -> Annotations {
        let feed: Arc<Mutex<Vec<u8>>> = Arc::default();
        let sink = feed.clone();
        f.repo
            .blame_lines(revision, Path::new(path), &Link::new(true), move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
            .unwrap();
        let mut notes = Annotations::default();
        notes.feed(&feed.lock().unwrap()).unwrap();
        assert!(notes.finished());
        notes
    }

    fn porcelain(f: &Fixture, revision: &[&str], path: &str) -> Vec<String> {
        let mut args = vec!["blame", "--porcelain"];
        args.extend_from_slice(revision);
        args.extend_from_slice(&["--", path]);
        let output = String::from_utf8(f.git(&args)).unwrap();
        let mut lines = Vec::new();
        let mut header = true;
        for line in output.lines() {
            if line.starts_with('\t') {
                header = true;
                continue;
            }
            if header {
                lines.push(line.split(' ').next().unwrap().to_owned());
                header = false;
            }
        }
        lines
    }

    const TRAIL: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    #[test]
    fn a_working_copy_blame_marks_the_edited_line_not_committed_and_follows_the_rename() {
        let f = Fixture::new();
        write(&f, "notes.txt", TRAIL);
        f.repo.commit("Start notes").unwrap();
        let start = head(&f, "HEAD");
        f.git(&["mv", "notes.txt", "moved.txt"]);
        write(&f, "moved.txt", &TRAIL.replace("two", "TWO"));
        f.repo.commit("Move notes").unwrap();
        let moved = head(&f, "HEAD");
        fs::write(f.dir.join("moved.txt"), TRAIL.replace("two", "TWO").replace("six", "SIX")).unwrap();
        let before = untouched(&f);

        let notes = blamed(&f, None, "moved.txt");
        assert_eq!(notes.covered(), 10);
        let commits: Vec<String> = (0..10).map(|line| notes.commit_of(line).unwrap().oid.clone()).collect();
        assert_eq!(commits, porcelain(&f, &[], "moved.txt"));
        assert_eq!(commits[1], moved);
        assert!(!notes.commit_of(5).unwrap().committed());
        assert!(commits.iter().enumerate().filter(|(line, _)| ![1, 5].contains(line)).all(|(_, oid)| *oid == start));
        let first = &notes.blocks[notes.block_of(0).unwrap()];
        assert_eq!(first.path, Path::new("notes.txt"));
        let edited = &notes.blocks[notes.block_of(1).unwrap()];
        assert_eq!(edited.path, Path::new("moved.txt"));
        assert_eq!(edited.previous, Some((start.clone(), PathBuf::from("notes.txt"))));
        let working = &notes.blocks[notes.block_of(5).unwrap()];
        assert_eq!(working.previous, Some((moved.clone(), PathBuf::from("moved.txt"))));
        assert_eq!(notes.commit_of(0).unwrap().author, "Cedar");

        let older = blamed(&f, Some(&start), "notes.txt");
        assert_eq!(older.covered(), 10);
        assert!((0..10).all(|line| older.commit_of(line).unwrap().oid == start));

        let working = Blamed { path: "moved.txt".into(), at: At::Working };
        let prepared = f.repo.blame_contents(&working, 4).unwrap();
        assert_eq!(prepared.resolved, None);
        assert!(prepared.refused.is_none() && prepared.stamp.is_some());
        assert_eq!(prepared.doc.file.as_ref().and_then(|shown| shown.contents.clone()), Some(TRAIL.replace("two", "TWO").replace("six", "SIX")));
        let parent = Blamed { path: "notes.txt".into(), at: At::ParentOf(moved.clone()) };
        let prepared = f.repo.blame_contents(&parent, 4).unwrap();
        assert_eq!(prepared.resolved.as_deref(), Some(start.as_str()));
        assert_eq!(prepared.doc.file.as_ref().and_then(|shown| shown.contents.clone()), Some(TRAIL.to_owned()));
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn large_binary_untracked_and_unnamed_files_are_refused() {
        let f = Fixture::new();
        let big = "x".repeat(LIMIT as usize + 1);
        write(&f, "big.txt", &big);
        fs::write(f.dir.join("picture.bin"), [0u8, 1, 2, 3, 0, 10]).unwrap();
        f.repo.stage_file(Path::new("picture.bin")).unwrap();
        write(&f, "small.txt", "small\n");
        f.repo.commit("Add files").unwrap();
        let tip = head(&f, "HEAD");
        let prepared = f.repo.blame_contents(&Blamed { path: "big.txt".into(), at: At::Commit(tip.clone()) }, 4).unwrap();
        assert_eq!(prepared.refused.as_deref(), Some(TOO_LARGE));
        assert!(prepared.doc.lines.is_empty());
        let prepared = f.repo.blame_contents(&Blamed { path: "big.txt".into(), at: At::Working }, 4).unwrap();
        assert_eq!(prepared.refused.as_deref(), Some(TOO_LARGE));
        let prepared = f.repo.blame_contents(&Blamed { path: "picture.bin".into(), at: At::Commit(tip.clone()) }, 4).unwrap();
        assert_eq!(prepared.refused.as_deref(), Some(BINARY));
        fs::write(f.dir.join("loose.txt"), "loose\n").unwrap();
        let sink = |_: &[u8]| {};
        assert!(f.repo.blame_lines(None, Path::new("loose.txt"), &Link::new(true), sink).is_err());
        assert!(f.repo.blame_lines(None, Path::new("../small.txt"), &Link::new(true), sink).is_err());
        assert!(f.repo.blame_lines(Some("HEAD"), Path::new("small.txt"), &Link::new(true), sink).is_err());
        assert!(f.repo.blame_contents(&Blamed { path: "small.txt".into(), at: At::ParentOf(tip) }, 4).is_err());
        assert!(f.repo.blame_contents(&Blamed { path: "small.txt".into(), at: At::Commit("main".into()) }, 4).is_err());
    }

    #[test]
    fn a_file_under_the_cap_but_over_the_file_view_limit_is_shown_whole() {
        let f = Fixture::new();
        let lines = (file_view::FILE_LIMIT + file_view::FILE_LIMIT / 2) / 16;
        let text: String = (0..lines).map(|line| format!("line {line:010}\n")).collect();
        assert!(text.len() > file_view::FILE_LIMIT && (text.len() as u64) < LIMIT);
        write(&f, "long.txt", &text);
        f.repo.commit("Add a long file").unwrap();
        let tip = head(&f, "HEAD");
        for at in [At::Commit(tip), At::Working] {
            let prepared = f.repo.blame_contents(&Blamed { path: "long.txt".into(), at }, 4).unwrap();
            assert!(prepared.refused.is_none());
            assert_eq!(prepared.doc.state, PatchState::Text);
            assert_eq!(prepared.doc.lines.len(), lines);
            assert_eq!(prepared.doc.file.as_ref().and_then(|shown| shown.contents.as_deref()), Some(text.as_str()));
        }
    }

    fn opened(link: &Link) -> State {
        State {
            open: Some(Open {
                request: Request { blamed: Blamed { path: "a.txt".into(), at: At::Working }, owner: None, diff_open: false },
                stack: vec![Blamed { path: "a.txt".into(), at: At::Working }],
                places: Vec::new(),
                resolved: None,
                notes: Annotations::default(),
                feed: Arc::default(),
                loading: false,
                reading: true,
                refused: None,
                failed: None,
                link: Some(link.clone()),
                chosen: None,
                stamp: None,
            }),
            ticket: 0,
            bare: false,
        }
    }

    fn running(name: &str) -> bool {
        std::process::Command::new("/usr/bin/pgrep")
            .args(["-f", name])
            .output()
            .is_ok_and(|output| !output.stdout.is_empty())
    }

    #[test]
    fn closing_the_view_stops_the_git_process_that_reads_the_blame() {
        let f = Fixture::new();
        let name = format!("blame-probe-{}-{}.txt", std::process::id(), f.dir.file_name().unwrap().to_string_lossy());
        write(&f, &name, "probe\n");
        f.repo.commit("Add the probe").unwrap();
        let tip = head(&f, "HEAD");
        let alternates = f.dir.join(".git/objects/info/alternates");
        fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        let fifo = std::ffi::CString::new(alternates.to_string_lossy().into_owned()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let link = Link::new(true);
        let mut state = opened(&link);
        let reader = {
            let (repo, link, path) = (f.repo.clone(), link.clone(), PathBuf::from(&name));
            std::thread::spawn(move || repo.blame_lines(Some(&tip), &path, &link, |_| {}))
        };
        let waited = Instant::now();
        while !running(&name) {
            assert!(waited.elapsed() < Duration::from_secs(10), "git blame never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let closed = Instant::now();
        assert!(state.close().is_some());
        assert!(link.cancelled());
        let result = reader.join().unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(closed.elapsed() < Duration::from_secs(3));
        assert!(!running(&name), "git blame is still running after the view closed");
        assert!(!state.open());
        fs::remove_file(&alternates).unwrap();
    }

    #[test]
    fn dropping_an_open_blame_cancels_its_read() {
        let link = Link::new(true);
        let state = opened(&link);
        assert!(!link.cancelled());
        drop(state);
        assert!(link.cancelled());
    }
}
