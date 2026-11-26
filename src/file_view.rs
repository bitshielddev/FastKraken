use crate::{
    Tab,
    commands::{Availability, Command, Group, Target, on_repo},
    diff_doc::{self, Blob, DiffDoc, File, PatchState, Run, runs},
    image_diff,
    kit,
    notice::Notice,
    repository::{Repository, run_with_input, validate_oid},
    theme::*,
    view::{self, DiffChange, Selection, Ui},
};
use gpui::{Context, Div, ImageFormat, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command as Process, Output, Stdio},
    time::{Instant, SystemTime},
};

pub(crate) const FILE_LIMIT: usize = 1024 * 1024;
const BINARY_PROBE: usize = 8000;
pub(crate) const CUT: &str = "This file is larger than 1 MiB. Only the first part is shown.";
pub(crate) const LOADING: &str = "Loading file…";

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "view.file-or-diff",
        Group::View,
        |ui, _| if ui.file_view.view == View::File { "Show the diff" } else { "Show the whole file" }.into(),
        |ui, target, _| {
            if on_repo(ui, target) && offered(ui) { Availability::Enabled } else { Availability::Hidden }
        },
        |ui, _| {
            let view = if ui.file_view.view == View::File { View::Diff } else { View::File };
            Some(view::Action::FileView(Action::Show(view)))
        },
    )
    .words(&["file view", "whole file", "contents", "revision", "diff view"]),
    Command::new(
        "text.copy-file",
        Group::Copy,
        |_, _| "Copy file contents".into(),
        |ui, target, _| {
            let offered = matches!(target, Target::Code { .. }) || on_repo(ui, target);
            match ui.diff.file.as_deref().filter(|_| offered && showing(ui)) {
                None => Availability::Hidden,
                Some(shown) if shown.contents.is_some() => Availability::Enabled,
                Some(shown) if shown.cut => Availability::Disabled("Only the first part of this file is loaded".to_owned()),
                Some(_) => Availability::Disabled("This file has no text to copy".to_owned()),
            }
        },
        |_, _| Some(view::Action::FileView(Action::CopyContents)),
    )
    .words(&["clipboard", "whole file", "contents"])
    .icon("copy"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum View {
    #[default]
    Diff,
    File,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Show(View),
    CopyContents,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) view: View,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Touch {
    Added,
    Changed,
    Removed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Version {
    #[default]
    Changed,
    Before,
    Working,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) version: Version,
    pub(crate) marks: Vec<Option<Touch>>,
    pub(crate) contents: Option<String>,
    pub(crate) size: u64,
    pub(crate) cut: bool,
    pub(crate) working: bool,
}

impl Shown {
    pub(crate) fn mark(&self, row: usize) -> Option<Touch> {
        self.marks.get(row).copied().flatten()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Stored(String),
    Revision(String),
    Working(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Contents {
    pub(crate) bytes: Vec<u8>,
    pub(crate) size: u64,
    pub(crate) cut: bool,
    pub(crate) blob: Option<String>,
}

fn absent(id: &str) -> bool {
    id.bytes().all(|b| b == b'0')
}

pub(crate) fn single(selection: &Selection, open: Option<&Path>) -> Option<PathBuf> {
    match selection {
        Selection::File { path, .. } => Some(path.clone()),
        Selection::Commit(_) | Selection::Stash(_) | Selection::Compare { .. } => open.map(Path::to_path_buf),
        Selection::Changes | Selection::Commits => None,
    }
}

pub(crate) fn wanted(ui: &Ui, selection: &Selection, open: Option<&Path>) -> bool {
    ui.file_view.view == View::File && single(selection, open).is_some()
}

pub(crate) fn offered(ui: &Ui) -> bool {
    let Some(selection) = &ui.selection else { return false };
    view::diff_shown(ui)
        && !ui.blame.open()
        && !crate::browse::showing(ui)
        && single(selection, image_diff::selection_open(ui).as_deref()).is_some()
}

pub(crate) fn showing(ui: &Ui) -> bool {
    ui.diff.file.is_some() && view::diff_shown(ui)
}

fn loading(ui: &Ui) -> bool {
    ui.diff.state == PatchState::Loading
        && ui
            .selection
            .as_ref()
            .is_some_and(|selection| wanted(ui, selection, image_diff::selection_open(ui).as_deref()))
}

pub(crate) fn locate(diff: &DiffDoc, selection: &Selection, path: &Path, root: &Path) -> Option<(Origin, Version)> {
    let present = |id: Option<&String>| id.filter(|id| !absent(id)).cloned();
    let file = match diff.files.as_slice() {
        [file] => Some(file),
        files => {
            let named = || files.iter().filter(|file| file.path.as_deref() == Some(path));
            named().find(|file| present(file.new_blob.as_ref()).is_some()).or_else(|| named().next())
        }
    };
    let (old, new) = file.map_or((None, None), |file| (present(file.old_blob.as_ref()), present(file.new_blob.as_ref())));
    let listed = file.is_some_and(|file| file.new_blob.is_some());
    if listed && new.is_none() {
        if let Some(old) = old {
            return Some((Origin::Stored(old), Version::Before));
        }
    }
    let named = path.to_str().filter(|name| !name.is_empty() && !name.contains('\n'));
    let revision = |at: &str| named.map(|name| Origin::Revision(format!("{at}:{name}")));
    Some(match selection {
        Selection::File { staged: false, .. } | Selection::Compare { to: crate::compare::Rev::WorkingTree, .. } => {
            let on_disk = root.join(path);
            if std::fs::symlink_metadata(&on_disk).is_ok() {
                (Origin::Working(on_disk), Version::Working)
            } else {
                (old.map(Origin::Stored).or_else(|| revision(":0"))?, Version::Before)
            }
        }
        Selection::File { staged: true, .. } => (new.map(Origin::Stored).or_else(|| revision(":0"))?, Version::Changed),
        Selection::Commit(oid) | Selection::Stash(oid) | Selection::Compare { to: crate::compare::Rev::Commit(oid), .. } => {
            (new.map(Origin::Stored).or_else(|| revision(oid))?, Version::Changed)
        }
        Selection::Changes | Selection::Commits => return None,
    })
}

fn head(command: &mut Process, limit: usize) -> Result<Vec<u8>, String> {
    let (started, at) = (Instant::now(), SystemTime::now());
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let read = match child.stdout.take() {
        Some(stdout) => stdout.take(limit as u64).read_to_end(&mut bytes).map(|_| ()),
        None => Err(std::io::Error::other("Git output is unavailable")),
    };
    let _ = child.kill();
    let status = child.wait().map_err(|error| error.to_string())?;
    crate::activity::record(command, started, at, &Ok(Output { status, stdout: Vec::new(), stderr: Vec::new() }));
    read.map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn working(path: &Path, limit: usize) -> Result<Contents, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "The file is no longer in your working copy.".to_owned())?;
    if metadata.file_type().is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        let target = std::fs::read_link(path).map_err(|error| error.to_string())?;
        let bytes = target.as_os_str().as_bytes().to_vec();
        return Ok(Contents { size: bytes.len() as u64, bytes, cut: false, blob: None });
    }
    if !metadata.is_file() {
        return Err("This path is a folder, so it has no text to show.".to_owned());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    let cut = bytes.len() > limit;
    bytes.truncate(limit);
    Ok(Contents { bytes, size: metadata.len(), cut, blob: None })
}

impl Repository {
    pub(crate) fn blob_named(&self, revision: &str) -> Result<(String, u64), String> {
        if revision.is_empty() || revision.contains('\n') {
            return Err("This file cannot be looked up by its name.".to_owned());
        }
        let output = run_with_input(
            self.read_command().args(["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"]),
            format!("{revision}\n").into_bytes(),
        )
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let listed = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut fields = listed.trim_end().rsplitn(3, ' ');
        let (size, kind, id) = (fields.next(), fields.next(), fields.next());
        match (id, kind, size.and_then(|size| size.parse().ok())) {
            (Some(id), Some("blob"), Some(size)) if validate_oid(id).is_ok() => Ok((id.to_owned(), size)),
            (Some(_), Some("tree" | "commit"), _) => Err("This path is not a file at this version.".to_owned()),
            _ => Err("This version of the file is not in the repository.".to_owned()),
        }
    }

    pub(crate) fn blob_head(&self, id: &str, limit: usize) -> Result<Contents, String> {
        validate_oid(id)?;
        let size = *self
            .object_sizes(&[id.to_owned()])?
            .get(id)
            .ok_or("This version of the file is not in the repository.")?;
        let wanted = limit.min(usize::try_from(size).unwrap_or(usize::MAX));
        let bytes = if wanted as u64 == size {
            self.blobs(&[id.to_owned()])?.remove(id).ok_or("This version of the file could not be read.")?
        } else {
            head(self.read_command().args(["cat-file", "blob", id]), wanted)?
        };
        if (bytes.len() as u64) < wanted as u64 {
            return Err("Git ended the file early.".to_owned());
        }
        Ok(Contents { cut: (bytes.len() as u64) < size, size, bytes, blob: Some(id.to_owned()) })
    }

    pub(crate) fn file_contents(&self, origin: &Origin, limit: usize) -> Result<Contents, String> {
        match origin {
            Origin::Working(path) => working(path, limit),
            Origin::Stored(id) => self.blob_head(id, limit),
            Origin::Revision(revision) => {
                let (id, _) = self.blob_named(revision)?;
                self.blob_head(&id, limit)
            }
        }
    }
}

pub(crate) fn marks(diff: &DiffDoc, rows: usize) -> Vec<Option<Touch>> {
    let mut marks = vec![None; rows];
    if rows == 0 {
        return marks;
    }
    runs(&diff.lines, |run| {
        let Run::Change { deleted, added, .. } = run else { return };
        if added.is_empty() {
            let Some(&first) = deleted.first() else { return };
            let at = (diff.lines[first].at as usize).clamp(1, rows) - 1;
            marks[at].get_or_insert(Touch::Removed);
            return;
        }
        let touch = if deleted.is_empty() { Touch::Added } else { Touch::Changed };
        for &row in added {
            if let Some(slot) = diff.lines[row].new.and_then(|number| marks.get_mut((number as usize).checked_sub(1)?)) {
                *slot = Some(touch);
            }
        }
    });
    marks
}

pub(crate) fn changes(marks: &[Option<Touch>]) -> Vec<DiffChange> {
    let mut changes = Vec::new();
    let mut previous = None;
    for (row, mark) in marks.iter().enumerate() {
        if let Some(touch) = *mark
            && *mark != previous
        {
            changes.push(DiffChange {
                start: row,
                deleted: matches!(touch, Touch::Removed | Touch::Changed).then_some(row),
                added: matches!(touch, Touch::Added | Touch::Changed).then_some(row),
            });
        }
        previous = *mark;
    }
    changes
}

fn heading(selection: &Selection, version: Version) -> &'static str {
    match (selection, version) {
        (_, Version::Before) => "Before it was deleted",
        (_, Version::Working) => "Working copy",
        (Selection::File { .. }, _) => "Staged",
        (Selection::Stash(_), _) => "In this stash",
        (Selection::Compare { .. }, _) => "On the newer side",
        _ => "At this commit",
    }
}

fn picture(
    selection: &Selection,
    path: &Path,
    origin: &Origin,
    blob: Option<&str>,
    binary: bool,
    version: Version,
) -> Option<image_diff::Request> {
    let format = image_diff::format_of(path)?;
    if !binary && format != ImageFormat::Svg {
        return None;
    }
    let side = match origin {
        Origin::Working(on_disk) => image_diff::Side::Working { stamp: image_diff::stamp(on_disk), path: on_disk.clone() },
        _ => image_diff::Side::Stored(blob?.to_owned()),
    };
    Some(image_diff::Request {
        path: path.to_path_buf(),
        format,
        binary,
        sides: [None, Some(side)],
        heading: Some(heading(selection, version)),
    })
}

pub(crate) fn failed(version: Version, reason: String) -> DiffDoc {
    let mut doc = DiffDoc::default();
    doc.state = PatchState::Unreadable(reason);
    doc.file = Some(Box::new(Shown { version, ..Shown::default() }));
    doc
}

pub(crate) fn shape(
    diff: &DiffDoc,
    selection: &Selection,
    path: &Path,
    origin: &Origin,
    version: Version,
    contents: Contents,
) -> (DiffDoc, Option<image_diff::Request>) {
    let blob = contents.blob.clone().or_else(|| {
        diff.files.iter().find_map(|file| file.new_blob.clone().filter(|id| !absent(id) && version == Version::Working))
    });
    let binary = contents.bytes[..contents.bytes.len().min(BINARY_PROBE)].contains(&0);
    let image = picture(selection, path, origin, blob.as_deref(), binary, version);
    let working = matches!(origin, Origin::Working(_));
    let shown = |marks, text: Option<String>| Shown { version, marks, contents: text, size: contents.size, cut: contents.cut, working };
    if binary {
        let mut doc = DiffDoc::default();
        doc.state = PatchState::Binary { old: Blob::Absent, new: Blob::Bytes(contents.size) };
        doc.files = vec![File { path: Some(path.to_path_buf()), old_blob: None, new_blob: blob, rows: 0..0 }];
        doc.file = Some(Box::new(shown(Vec::new(), None)));
        return (doc, image);
    }
    let mut bytes = &contents.bytes[..];
    if contents.cut
        && let Some(end) = bytes.iter().rposition(|&b| b == b'\n')
    {
        bytes = &bytes[..=end];
    }
    let text = String::from_utf8_lossy(bytes);
    let mut doc = DiffDoc::plain(path.to_path_buf(), blob, &text);
    if contents.cut && !doc.lines.is_empty() {
        doc.state = PatchState::Truncated;
    }
    let marks = if version == Version::Before { Vec::new() } else { marks(diff, doc.lines.len()) };
    let copy = (!contents.cut).then(|| text.into_owned());
    doc.file = Some(Box::new(shown(marks, copy)));
    (doc, image)
}

pub(crate) fn open(repo: &Repository, diff: &DiffDoc, selection: &Selection, open: Option<&Path>) -> (DiffDoc, Option<image_diff::Request>) {
    let Some(path) = single(selection, open) else {
        return (failed(Version::Changed, "Select a single file to see it whole.".to_owned()), None);
    };
    let Some((origin, version)) = locate(diff, selection, &path, &repo.root) else {
        return (failed(Version::Changed, "This file's name cannot be looked up.".to_owned()), None);
    };
    match repo.file_contents(&origin, FILE_LIMIT) {
        Ok(contents) => shape(diff, selection, &path, &origin, version, contents),
        Err(reason) => (failed(version, reason), None),
    }
}

pub(crate) fn working_root(doc: &DiffDoc, root: &Path) -> Option<PathBuf> {
    doc.file.as_ref().filter(|shown| shown.working).map(|_| root.to_path_buf())
}

fn deleted_text(selection: &Option<Selection>) -> &'static str {
    match selection {
        Some(Selection::Commit(_)) => "This commit deletes the file, so the version before it is shown.",
        Some(Selection::Stash(_)) => "This stash deletes the file, so the version before it is shown.",
        Some(Selection::Compare { .. }) => "The newer side has no such file, so the older side's version is shown.",
        _ => "This file is deleted, so the version before the deletion is shown.",
    }
}

pub(crate) fn bar_texts(ui: &Ui, shown: &Shown) -> Vec<&'static str> {
    let mut texts = Vec::new();
    if shown.version == Version::Before {
        texts.push(deleted_text(&ui.selection));
    }
    if shown.cut {
        texts.push(CUT);
    }
    if ui.diff.shortened {
        texts.push(diff_doc::shortened_text(false));
    }
    texts
}

pub(crate) fn placeholder(ui: &Ui) -> Option<String> {
    if loading(ui) {
        return Some(LOADING.to_owned());
    }
    ui.diff.file.as_ref()?;
    match &ui.diff.state {
        PatchState::Binary { new: Blob::Bytes(size), .. } => {
            Some(format!("This is a binary file ({}), so it is not shown as text.", crate::conflict::size(*size)))
        }
        PatchState::Binary { .. } => Some("This is a binary file, so it is not shown as text.".to_owned()),
        PatchState::Unreadable(reason) => Some(format!("This file could not be shown. {reason}")),
        PatchState::Empty(_) => Some(diff_doc::EMPTY_FILE.to_owned()),
        _ => None,
    }
}

pub(crate) fn toggle<T: view::Desk>(ui: &Ui, cx: &Context<T>) -> Div {
    let segment = |id, label, tip, choice| {
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(ui.file_view.view == choice)
            .tip(tip)
            .on(view::Action::FileView(Action::Show(choice)), cx)
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .p(px(1.))
        .rounded(px(RADIUS))
        .bg(rgb(palette().canvas))
        .border_1()
        .border_color(rgb(palette().line))
        .child(segment("show-diff", "Diff", "Show the changes", View::Diff))
        .child(segment("show-file", "File", "Show the whole file at this version", View::File))
}

pub(crate) fn gutter_mark(mark: Option<Touch>, first_part: bool, width: f32) -> Div {
    let cell = div().w(px(width)).h_full().flex_shrink_0().relative();
    let Some(mark) = mark.filter(|mark| first_part || *mark != Touch::Removed) else { return cell };
    match mark {
        Touch::Removed => {
            cell.child(div().absolute().top_0().left(px(((width - 10.) / 2.).floor())).w(px(10.)).h(px(2.)).bg(rgb(palette().deleted)))
        }
        Touch::Added | Touch::Changed => {
            let ink = if mark == Touch::Added { palette().added } else { palette().modified };
            cell.child(div().absolute().top_0().bottom_0().left(px(((width - 3.) / 2.).floor())).w(px(3.)).bg(rgb(ink)))
        }
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Show(view) => {
            if tab.ui.file_view.view == view {
                return;
            }
            tab.ui.file_view.view = view;
            if offered(&tab.ui) {
                crate::diff_options::reload(tab, cx);
            }
        }
        Action::CopyContents => {
            let Some(text) = tab.ui.diff.file.as_ref().filter(|_| showing(&tab.ui)).and_then(|shown| shown.contents.clone()) else {
                return;
            };
            view::copy(text, cx);
            tab.ui.notices.show(Notice::copied("file contents"));
        }
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{DiffOptions, tests::Fixture};
    use std::{collections::HashMap, fs};

    fn doc_of(text: &str) -> DiffDoc {
        let sizes = HashMap::new();
        DiffDoc::build(&diff_doc::Source {
            text,
            stageable: false,
            truncated: false,
            sizes: &sizes,
            working_size: None,
            options: DiffOptions::default(),
        })
    }

    fn id(f: &Fixture, spec: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", spec])).unwrap().trim().to_owned()
    }

    fn rows(doc: &DiffDoc) -> Vec<(Option<u32>, String)> {
        doc.lines.iter().map(|line| (line.new, doc.code(line).to_owned())).collect()
    }

    fn untouched(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (f.git(&["status", "--porcelain=v1", "-uall"]), f.git(&["ls-files", "-s"]), f.git(&["for-each-ref"]))
    }

    #[test]
    fn a_commit_shows_its_version_and_marks_the_lines_it_changed() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "first"]);
        fs::write(f.dir.join("trail.txt"), "one\nTWO\nthree\nfive\nsix\nseven\n").unwrap();
        f.git(&["commit", "-qam", "second"]);
        fs::write(f.dir.join("trail.txt"), "working\n").unwrap();
        let head = id(&f, "HEAD");
        let before = untouched(&f);
        let diff = f.repo.commit_path_diff_with(&head, Path::new("trail.txt"), None, DiffOptions::default()).unwrap();
        let diff = doc_of(&diff.text);
        let selection = Selection::Commit(head.clone());
        let (doc, image) = open(&f.repo, &diff, &selection, Some(Path::new("trail.txt")));
        assert!(image.is_none());
        assert_eq!(doc.state, PatchState::Text);
        assert_eq!(rows(&doc), [
            (Some(1), "one".to_owned()),
            (Some(2), "TWO".to_owned()),
            (Some(3), "three".to_owned()),
            (Some(4), "five".to_owned()),
            (Some(5), "six".to_owned()),
            (Some(6), "seven".to_owned()),
        ]);
        let shown = doc.file.as_deref().unwrap();
        assert_eq!(shown.version, Version::Changed);
        assert_eq!(shown.marks, [None, Some(Touch::Changed), None, Some(Touch::Removed), Some(Touch::Added), Some(Touch::Added)]);
        assert_eq!(shown.contents.as_deref(), Some("one\nTWO\nthree\nfive\nsix\nseven\n"));
        assert!(!shown.working && !shown.cut);
        assert_eq!(doc.files[0].new_blob.as_deref(), Some(id(&f, "HEAD:trail.txt").as_str()));
        assert_eq!(doc.files[0].rows, 0..6);
        assert!(!doc.stageable && doc.hunks.is_empty());
        assert_eq!(
            changes(&shown.marks),
            [
                DiffChange { start: 1, deleted: Some(1), added: Some(1) },
                DiffChange { start: 3, deleted: Some(3), added: None },
                DiffChange { start: 4, deleted: None, added: Some(4) },
            ]
        );
        assert_eq!(untouched(&f), before);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "working\n");
    }

    #[test]
    fn a_deleted_file_shows_the_version_before_without_marks() {
        let f = Fixture::new();
        fs::write(f.dir.join("old.txt"), "kept\nlines\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "add"]);
        let added = id(&f, "HEAD:old.txt");
        f.git(&["rm", "-q", "old.txt"]);
        f.git(&["commit", "-qm", "remove"]);
        let head = id(&f, "HEAD");
        let diff = f.repo.commit_path_diff_with(&head, Path::new("old.txt"), None, DiffOptions::default()).unwrap();
        let diff = doc_of(&diff.text);
        let selection = Selection::Commit(head);
        let (origin, version) = locate(&diff, &selection, Path::new("old.txt"), &f.dir).unwrap();
        assert_eq!((origin, version), (Origin::Stored(added), Version::Before));
        let (doc, _) = open(&f.repo, &diff, &selection, Some(Path::new("old.txt")));
        assert_eq!(rows(&doc), [(Some(1), "kept".to_owned()), (Some(2), "lines".to_owned())]);
        let shown = doc.file.as_deref().unwrap();
        assert_eq!(shown.version, Version::Before);
        assert!(shown.marks.is_empty());
    }

    #[test]
    fn a_file_replaced_by_a_link_shows_the_link_not_the_version_before() {
        let f = Fixture::new();
        fs::write(f.dir.join("target.txt"), "aim\n").unwrap();
        fs::write(f.dir.join("swap me.txt"), "plain\ntext\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "file"]);
        fs::remove_file(f.dir.join("swap me.txt")).unwrap();
        std::os::unix::fs::symlink("target.txt", f.dir.join("swap me.txt")).unwrap();
        let path = Path::new("swap me.txt");
        let unstaged = Selection::File { path: path.into(), staged: false };
        let diff = doc_of(&f.repo.file_diff_with(path, false, DiffOptions::default()).unwrap().text);
        assert_eq!(diff.files.len(), 2);
        assert_eq!(locate(&diff, &unstaged, path, &f.dir), Some((Origin::Working(f.dir.join(path)), Version::Working)));
        let (doc, _) = open(&f.repo, &diff, &unstaged, None);
        assert_eq!(doc.file.as_deref().unwrap().contents.as_deref(), Some("target.txt"));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "link"]);
        let head = id(&f, "HEAD");
        let link = id(&f, "HEAD:swap me.txt");
        let diff = doc_of(&f.repo.commit_path_diff_with(&head, path, None, DiffOptions::default()).unwrap().text);
        assert_eq!(diff.files.len(), 2);
        let selection = Selection::Commit(head);
        assert_eq!(locate(&diff, &selection, path, &f.dir), Some((Origin::Stored(link), Version::Changed)));
        let (doc, _) = open(&f.repo, &diff, &selection, Some(path));
        assert_eq!(rows(&doc), [(Some(1), "target.txt".to_owned())]);
        let shown = doc.file.as_deref().unwrap();
        assert_eq!((shown.version, shown.marks.as_slice()), (Version::Changed, [Some(Touch::Added)].as_slice()));
    }

    #[test]
    fn each_selection_reads_its_own_version_of_the_file() {
        let f = Fixture::new();
        fs::write(f.dir.join("map.txt"), "committed\n").unwrap();
        fs::write(f.dir.join("gone.txt"), "was here\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("map.txt"), "staged\n").unwrap();
        f.git(&["add", "map.txt"]);
        fs::write(f.dir.join("map.txt"), "staged\nand working\n").unwrap();
        fs::remove_file(f.dir.join("gone.txt")).unwrap();
        let before = untouched(&f);
        let read = |staged: bool, path: &str| {
            let diff = f.repo.file_diff_with(Path::new(path), staged, DiffOptions::default()).unwrap();
            let diff = doc_of(&diff.text);
            let selection = Selection::File { path: path.into(), staged };
            let (origin, version) = locate(&diff, &selection, Path::new(path), &f.dir).unwrap();
            let (doc, _) = open(&f.repo, &diff, &selection, None);
            (origin, version, doc.file.as_deref().unwrap().contents.clone(), doc.file.as_deref().unwrap().working)
        };
        let (origin, version, text, working) = read(false, "map.txt");
        assert_eq!((origin, version, working), (Origin::Working(f.dir.join("map.txt")), Version::Working, true));
        assert_eq!(text.as_deref(), Some("staged\nand working\n"));
        let (origin, version, text, working) = read(true, "map.txt");
        assert_eq!((origin, version, working), (Origin::Stored(id(&f, ":map.txt")), Version::Changed, false));
        assert_eq!(text.as_deref(), Some("staged\n"));
        let (origin, version, text, _) = read(false, "gone.txt");
        assert_eq!((origin, version), (Origin::Stored(id(&f, ":gone.txt")), Version::Before));
        assert_eq!(text.as_deref(), Some("was here\n"));
        assert_eq!(untouched(&f), before);
        let mode_only = doc_of("diff --git a/map.txt b/map.txt\nold mode 100644\nnew mode 100755\n");
        let commit = Selection::Commit("a".repeat(40));
        assert_eq!(
            locate(&mode_only, &commit, Path::new("map.txt"), &f.dir),
            Some((Origin::Revision(format!("{}:map.txt", "a".repeat(40))), Version::Changed))
        );
        assert_eq!(
            locate(&mode_only, &Selection::File { path: "map.txt".into(), staged: true }, Path::new("map.txt"), &f.dir),
            Some((Origin::Revision(":0:map.txt".to_owned()), Version::Changed))
        );
        assert_eq!(locate(&mode_only, &commit, Path::new("line\nbreak"), &f.dir), None);
        assert_eq!(locate(&mode_only, &Selection::Changes, Path::new("map.txt"), &f.dir), None);
    }

    #[test]
    fn contents_are_read_by_name_or_id_and_cut_at_the_limit() {
        let f = Fixture::new();
        fs::create_dir(f.dir.join("folder")).unwrap();
        fs::write(f.dir.join("folder/notes.txt"), "abcdefghij\nklmnop\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "notes"]);
        let before = untouched(&f);
        let blob = id(&f, "HEAD:folder/notes.txt");
        assert_eq!(f.repo.blob_named("HEAD:folder/notes.txt").unwrap(), (blob.clone(), 18));
        let whole = f.repo.file_contents(&Origin::Revision("HEAD:folder/notes.txt".into()), 100).unwrap();
        assert_eq!(whole, Contents { bytes: b"abcdefghij\nklmnop\n".to_vec(), size: 18, cut: false, blob: Some(blob.clone()) });
        let cut = f.repo.file_contents(&Origin::Stored(blob.clone()), 5).unwrap();
        assert_eq!(cut, Contents { bytes: b"abcde".to_vec(), size: 18, cut: true, blob: Some(blob.clone()) });
        let exact = f.repo.file_contents(&Origin::Stored(blob.clone()), 18).unwrap();
        assert!(!exact.cut && exact.bytes.len() == 18);
        let on_disk = f.repo.file_contents(&Origin::Working(f.dir.join("folder/notes.txt")), 4).unwrap();
        assert_eq!(on_disk, Contents { bytes: b"abcd".to_vec(), size: 18, cut: true, blob: None });
        assert!(f.repo.blob_named("HEAD:folder").unwrap_err().contains("not a file"));
        assert!(f.repo.blob_named("HEAD:missing.txt").unwrap_err().contains("not in the repository"));
        assert!(f.repo.blob_named("HEAD:a\nb").is_err());
        assert!(f.repo.file_contents(&Origin::Stored("--batch".into()), 10).is_err());
        assert!(f.repo.file_contents(&Origin::Stored("1".repeat(40)), 10).is_err());
        assert!(f.repo.file_contents(&Origin::Working(f.dir.join("folder")), 10).unwrap_err().contains("folder"));
        assert!(f.repo.file_contents(&Origin::Working(f.dir.join("nowhere")), 10).is_err());
        assert_eq!(untouched(&f), before);
    }

    #[test]
    fn a_cut_file_ends_at_a_whole_line_and_cannot_be_copied() {
        let diff = DiffDoc::default();
        let selection = Selection::Commit("b".repeat(40));
        let contents = Contents { bytes: b"first\nsecond\nthi".to_vec(), size: 4096, cut: true, blob: Some("c".repeat(40)) };
        let (doc, _) = shape(&diff, &selection, Path::new("long.txt"), &Origin::Stored("c".repeat(40)), Version::Changed, contents);
        assert_eq!(doc.state, PatchState::Truncated);
        assert_eq!(rows(&doc), [(Some(1), "first".to_owned()), (Some(2), "second".to_owned())]);
        let shown = doc.file.as_deref().unwrap();
        assert!(shown.cut && shown.contents.is_none());
        let empty = Contents { bytes: Vec::new(), size: 0, cut: false, blob: Some("e".repeat(40)) };
        let (doc, _) = shape(&diff, &selection, Path::new("empty.txt"), &Origin::Stored("e".repeat(40)), Version::Changed, empty);
        assert_eq!(doc.state, PatchState::Empty(diff_doc::Blank::EmptyFile));
        assert_eq!(doc.file.as_deref().unwrap().contents.as_deref(), Some(""));
    }

    #[test]
    fn a_binary_file_is_not_shown_as_text_and_an_image_is_shown_as_a_picture() {
        let diff = DiffDoc::default();
        let blob = "d".repeat(40);
        let bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        let selection = Selection::Commit("b".repeat(40));
        let contents = Contents { bytes: bytes.clone(), size: 2048, cut: false, blob: Some(blob.clone()) };
        let (doc, image) = shape(&diff, &selection, Path::new("chart.png"), &Origin::Stored(blob.clone()), Version::Changed, contents);
        assert_eq!(doc.state, PatchState::Binary { old: Blob::Absent, new: Blob::Bytes(2048) });
        assert!(doc.lines.is_empty() && doc.file.as_deref().unwrap().contents.is_none());
        let image = image.unwrap();
        assert_eq!(image.sides, [None, Some(image_diff::Side::Stored(blob.clone()))]);
        assert!(image.binary);
        assert_eq!(image_diff::labels(&image)[1], "At this commit");
        let contents = Contents { bytes, size: 2048, cut: false, blob: Some(blob.clone()) };
        let (_, image) = shape(&diff, &selection, Path::new("data.bin"), &Origin::Stored(blob.clone()), Version::Changed, contents);
        assert!(image.is_none());
        let svg = Contents { bytes: b"<svg/>\n".to_vec(), size: 7, cut: false, blob: None };
        let working = Origin::Working(PathBuf::from("/nowhere/icon.svg"));
        let unstaged = Selection::File { path: "icon.svg".into(), staged: false };
        let (doc, image) = shape(&diff, &unstaged, Path::new("icon.svg"), &working, Version::Working, svg);
        assert_eq!(doc.state, PatchState::Text);
        let image = image.unwrap();
        assert!(!image.binary);
        assert!(matches!(&image.sides[1], Some(image_diff::Side::Working { .. })));
        assert_eq!(image_diff::labels(&image)[1], "Working copy");
    }

    #[test]
    fn marks_follow_the_new_line_numbers_of_the_diff() {
        let diff = doc_of(concat!(
            "diff --git a/t b/t\nindex 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644\n",
            "--- a/t\n+++ b/t\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -9,2 +9 @@\n i\n-j\n",
        ));
        assert_eq!(marks(&diff, 9), [None, Some(Touch::Changed), None, None, None, None, None, None, Some(Touch::Removed)]);
        assert_eq!(marks(&diff, 0), Vec::<Option<Touch>>::new());
        assert_eq!(marks(&doc_of(""), 2), [None, None]);
        let added = doc_of("diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+x\n+y\n");
        assert_eq!(marks(&added, 2), [Some(Touch::Added), Some(Touch::Added)]);
        assert!(changes(&[None, None]).is_empty());
        assert_eq!(changes(&[Some(Touch::Added), Some(Touch::Added), Some(Touch::Removed)]).len(), 2);
    }

    #[test]
    fn a_plain_doc_numbers_every_line_and_keeps_long_lines_short() {
        let long = "x".repeat(diff_doc::LINE_LIMIT + 10);
        let doc = DiffDoc::plain(PathBuf::from("a.rs"), Some("f".repeat(40)), &format!("fn a() {{}}\r\n\n{long}"));
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.code(&doc.lines[0]), "fn a() {}");
        assert_eq!(doc.code(&doc.lines[1]), "");
        assert!(doc.shortened);
        assert!(doc.lines.iter().all(|line| line.kind == diff_doc::Kind::Context && line.hunk.is_none() && line.old.is_none()));
        assert_eq!(doc.lines.iter().map(|line| line.at).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(doc.files, [File { path: Some("a.rs".into()), old_blob: None, new_blob: Some("f".repeat(40)), rows: 0..3 }]);
    }
}
