use crate::repository::Run;
use crate::{
    Tab,
    commands::{Availability, Command, Group, Side, Target, busy, operating},
    guard::Seen,
    notice::{self, Failure, Notice, Outcome},
    operation::{self, Kind as Operation},
    prompt::{self, Answer, Spec},
    repository::{Repository, checked, raw_path, run_with_input, validate_oid, validate_path},
    view::{self, Desk, Selection, Tone, Ui},
};
use gpui::{Context, PathPromptOptions, Window};
use std::{
    collections::VecDeque,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};

pub(crate) const LIMIT: u64 = 64 * 1024 * 1024;
pub(crate) const NOTHING: &str = "There are no changes to save";
pub(crate) const MERGE: &str = "A merge commit cannot be saved as a patch";
pub(crate) const CONFLICTED: &str = "Resolve the conflicts first";
pub(crate) const UNCHANGED: &str = "Nothing was changed.";
pub(crate) const ALREADY: &str = "The changes in the patch are already in your files. Nothing was changed.";
const DIFF: &[&str] =
    &["--binary", "--no-color", "--no-ext-diff", "--no-textconv", "--no-relative", "--src-prefix=a/", "--dst-prefix=b/"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Changes {
    All,
    Unstaged(Vec<PathBuf>),
    Staged(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Commits(Vec<String>),
    Changes(Changes),
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Save(Source),
    Write { source: Source, path: PathBuf },
    Apply,
    Check(PathBuf),
}

impl Action {
    pub(crate) fn reads(&self) -> bool {
        matches!(self, Self::Save(_) | Self::Write { .. })
    }
}

#[derive(Default)]
pub(crate) struct State {
    folder: Option<PathBuf>,
    checking: bool,
}

impl State {
    pub(crate) fn checking(&self) -> bool {
        self.checking
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Save { folder: PathBuf, name: String },
    Open,
}

static ANSWERS: Mutex<VecDeque<Option<PathBuf>>> = Mutex::new(VecDeque::new());
static ASKED: Mutex<Vec<Asked>> = Mutex::new(Vec::new());

pub(crate) fn answer(path: Option<PathBuf>) {
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).push_back(path);
}

pub(crate) fn asked() -> Vec<Asked> {
    ASKED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn choose(tab: &mut Tab, asked: Asked, then: impl FnOnce(PathBuf) -> Action + 'static, window: &mut Window, cx: &mut Context<Tab>) {
    if crate::tools::standing_in() {
        ASKED.lock().unwrap_or_else(|e| e.into_inner()).push(asked);
        let answered = ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).pop_front().flatten();
        if let Some(path) = answered {
            tab.dispatch(view::Action::Patch(then(path)), window, cx);
        }
        return;
    }
    let handle = cx.weak_entity();
    match asked {
        Asked::Save { folder, name } => {
            let picker = cx.prompt_for_new_path(&folder, Some(&name));
            window
                .spawn(cx, async move |cx| {
                    let chosen = picker.await;
                    let _ = cx.update(|window, cx| {
                        handle.update(cx, |tab, cx| match chosen {
                            Ok(Ok(Some(path))) => tab.dispatch(view::Action::Patch(then(path)), window, cx),
                            Ok(Err(error)) => tab.error(error.to_string(), cx),
                            _ => {}
                        })
                    });
                })
                .detach();
        }
        Asked::Open => {
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Apply".into()),
            });
            window
                .spawn(cx, async move |cx| {
                    let chosen = picker.await;
                    let _ = cx.update(|window, cx| {
                        handle.update(cx, |tab, cx| match chosen {
                            Ok(Ok(Some(paths))) => {
                                if let Some(path) = paths.into_iter().next() {
                                    tab.dispatch(view::Action::Patch(then(path)), window, cx);
                                }
                            }
                            Ok(Err(error)) => tab.error(error.to_string(), cx),
                            _ => {}
                        })
                    });
                })
                .detach();
        }
    }
}

fn working(target: &Target) -> Option<Changes> {
    match target {
        Target::File { path, side: Side::Unstaged } => Some(Changes::Unstaged(vec![path.clone()])),
        Target::File { path, side: Side::Staged } => Some(Changes::Staged(vec![path.clone()])),
        Target::Files { paths, side: Side::Unstaged } => Some(Changes::Unstaged(paths.clone())),
        Target::Files { paths, side: Side::Staged } => Some(Changes::Staged(paths.clone())),
        _ => None,
    }
}

fn folder(ui: &Ui, target: &Target) -> Option<Changes> {
    let Target::Folder { path, side } = target else { return None };
    let which = match side {
        Side::Unstaged => crate::file_list::Which::Unstaged,
        Side::Staged => crate::file_list::Which::Staged,
        _ => return None,
    };
    let paths = crate::file_list::under(ui, which, path);
    Some(if which == crate::file_list::Which::Staged { Changes::Staged(paths) } else { Changes::Unstaged(paths) })
}

fn conflicted(ui: &Ui) -> Option<String> {
    ui.operation.snapshot().is_some_and(|shown| !shown.conflicts.is_empty()).then(|| CONFLICTED.to_owned())
}

fn merges(ui: &Ui, oids: &[String]) -> Option<String> {
    oids.iter()
        .any(|oid| ui.history.iter().any(|row| &row.oid == oid && row.parents.len() > 1))
        .then(|| MERGE.to_owned())
}

fn picked(target: &Target) -> Option<&[String]> {
    match target {
        Target::Commits(oids) if oids.len() > 1 => Some(oids),
        _ => None,
    }
}

fn save(source: Source) -> Option<view::Action> {
    Some(view::Action::Patch(Action::Save(source)))
}

pub(crate) fn save_commits(oids: &[String]) -> view::Action {
    view::Action::Patch(Action::Save(Source::Commits(oids.to_vec())))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "commit.save-patch",
        Group::Copy,
        |_, _| "Save as patch…".into(),
        |ui, target, _| match target {
            Target::Commit(oid) => Availability::unless(merges(ui, std::slice::from_ref(oid))),
            _ => Availability::Hidden,
        },
        |_, target| match target {
            Target::Commit(oid) => save(Source::Commits(vec![oid.clone()])),
            _ => None,
        },
    )
    .words(&["create patch", "export", "format-patch", "diff file"])
    .icon("file"),
    Command::new(
        "commits.save-patch",
        Group::Copy,
        |_, target| format!("Save {} commits as a patch…", picked(target).map_or(0, <[String]>::len)),
        |ui, target, _| match picked(target) {
            Some(oids) => Availability::unless(merges(ui, oids)),
            None => Availability::Hidden,
        },
        |_, target| picked(target).map(save_commits),
    )
    .words(&["create patch", "export", "format-patch", "several"])
    .icon("file"),
    Command::new(
        "file.save-patch",
        Group::Copy,
        |_, target| match target {
            Target::Files { paths, .. } => format!("Save {} files as a patch…", paths.len()),
            _ => "Save changes as a patch…".to_owned(),
        },
        |ui, target, _| match working(target) {
            Some(_) => Availability::unless(conflicted(ui)),
            None => Availability::Hidden,
        },
        |_, target| working(target).map(Source::Changes).and_then(save),
    )
    .words(&["create patch", "export", "diff file"])
    .icon("file"),
    Command::new(
        "folder.save-patch",
        Group::Copy,
        |_, _| "Save folder as a patch…".into(),
        |ui, target, _| match folder(ui, target) {
            Some(Changes::Unstaged(paths) | Changes::Staged(paths)) if paths.is_empty() => {
                Availability::Disabled("No files in this folder are listed".to_owned())
            }
            Some(_) => Availability::unless(conflicted(ui)),
            None => Availability::Hidden,
        },
        |ui, target| folder(ui, target).map(Source::Changes).and_then(save),
    )
    .words(&["create patch", "export", "directory"])
    .icon("file"),
    Command::new(
        "repo.save-patch",
        Group::Copy,
        |_, _| "Save all changes as a patch…".into(),
        |ui, target, _| {
            if *target != Target::Repo || ui.root.as_os_str().is_empty() {
                return Availability::Hidden;
            }
            Availability::unless(ui.files.is_empty().then(|| NOTHING.to_owned()).or_else(|| conflicted(ui)))
        },
        |_, _| save(Source::Changes(Changes::All)),
    )
    .words(&["create patch", "export", "working directory", "uncommitted", "diff file"])
    .icon("file"),
    Command::new(
        "repo.apply-patch",
        Group::Changes,
        |_, _| "Apply patch…".into(),
        |ui, target, _| {
            if *target != Target::Repo || ui.root.as_os_str().is_empty() {
                return Availability::Hidden;
            }
            Availability::unless(busy(ui).or_else(|| operating(ui)))
        },
        |_, _| Some(view::Action::Patch(Action::Apply)),
    )
    .words(&["import patch", "git apply", "git am", "mailbox", "diff file"])
    .icon("file")
    .bar(crate::commands::Bar::File),
];

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '.' || c == '_' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches(|c| c == '-' || c == '.').chars().take(60).collect();
    slug.trim_end_matches(['-', '.']).to_owned()
}

fn file_name(stem: &str) -> String {
    format!("{stem}.patch")
}

pub(crate) fn suggested(ui: &Ui, source: &Source) -> String {
    match source {
        Source::Commits(oids) if oids.len() == 1 => {
            let subject = slug(&crate::reverse::subject_of(ui, &oids[0]));
            file_name(if subject.is_empty() { notice::short(&oids[0]) } else { &subject })
        }
        Source::Commits(oids) => file_name(&format!(
            "{}-commits-{}-{}",
            oids.len(),
            notice::short(oids.last().map_or("", String::as_str)),
            notice::short(oids.first().map_or("", String::as_str))
        )),
        Source::Changes(Changes::All) => file_name("changes"),
        Source::Changes(Changes::Unstaged(paths) | Changes::Staged(paths)) => match paths.as_slice() {
            [one] => {
                let name = slug(&one.file_name().unwrap_or(one.as_os_str()).to_string_lossy());
                file_name(if name.is_empty() { "changes" } else { &name })
            }
            _ => file_name("changes"),
        },
    }
}

fn start_folder(tab: &Tab) -> PathBuf {
    tab.ui.patch.folder.clone().filter(|folder| folder.is_dir()).unwrap_or_else(|| {
        let root = tab.repo.as_ref().map(|repo| repo.root.clone()).unwrap_or_default();
        root.parent().map(Path::to_path_buf).unwrap_or(root)
    })
}

fn remember(tab: &mut Tab, path: &Path) {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        tab.ui.patch.folder = Some(parent.to_path_buf());
    }
}

impl Repository {
    pub(crate) fn commits_patch(&self, oids: &[String]) -> Result<Vec<u8>, String> {
        if oids.is_empty() {
            return Err("Choose a commit first.".to_owned());
        }
        for oid in oids {
            validate_oid(oid)?;
        }
        let merges = checked(self.read_command().args(["rev-list", "--no-walk=unsorted", "--min-parents=2"]).args(oids).arg("--"))?;
        if !merges.is_empty() {
            return Err(format!("{MERGE}."));
        }
        checked(
            self.read_command()
                .args(["format-patch", "--stdout", "--no-cover-letter"])
                .arg(if oids.len() == 1 { "--max-count=1" } else { "--no-walk=unsorted" })
                .args(DIFF)
                .args(oids)
                .arg("--"),
        )
    }

    fn unmerged_among(&self, paths: &[PathBuf]) -> Result<bool, String> {
        let listed = checked(self.read_command().args(["ls-files", "-z", "--unmerged", "--"]).args(paths))?;
        Ok(!listed.is_empty())
    }

    pub(crate) fn changes_patch(&self, changes: &Changes) -> Result<Vec<u8>, String> {
        let paths = match changes {
            Changes::All => Vec::new(),
            Changes::Unstaged(paths) | Changes::Staged(paths) if paths.is_empty() => return Err(format!("{NOTHING}.")),
            Changes::Unstaged(paths) => self.with_origins(paths, false)?,
            Changes::Staged(paths) => self.with_origins(paths, true)?,
        };
        if self.unmerged_among(&paths)? {
            return Err(format!("{CONFLICTED}."));
        }
        if let Changes::Staged(_) = changes {
            return checked(self.read_command().args(["diff", "--cached"]).args(DIFF).arg("--").args(&paths));
        }
        let scratch = tempfile::tempdir().map_err(|e| format!("Prepare the patch: {e}"))?;
        let index = scratch.path().join("index");
        let real = self.git_dir().join("index");
        if real.exists() {
            std::fs::copy(&real, &index).map_err(|e| format!("Prepare the patch: {e}"))?;
        }
        let untracked = checked(
            self.read_command().args(["ls-files", "-z", "--others", "--exclude-standard", "--"]).args(&paths),
        )?;
        let untracked: Vec<PathBuf> = untracked.split(|&b| b == 0).filter(|path| !path.is_empty()).map(raw_path).collect();
        if !untracked.is_empty() {
            checked(self.read_command().env("GIT_INDEX_FILE", &index).args(["add", "--intent-to-add", "--"]).args(&untracked))?;
        }
        let mut diff = self.read_command();
        diff.env("GIT_INDEX_FILE", &index).arg("diff").args(DIFF);
        if *changes == Changes::All {
            diff.arg(self.base_tree()?);
        }
        checked(diff.arg("--").args(&paths))
    }

    fn base_tree(&self) -> Result<String, String> {
        let head = self.read_command().args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"]).run();
        let head = head.map_err(|e| format!("Run bundled Git: {e}"))?;
        let text = String::from_utf8_lossy(&head.stdout).trim().to_owned();
        if head.status.success() && !text.is_empty() {
            validate_oid(&text)?;
            return Ok(text);
        }
        let empty = run_with_input(self.read_command().args(["hash-object", "-t", "tree", "--stdin"]), Vec::new())
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let empty = String::from_utf8_lossy(&empty.stdout).trim().to_owned();
        validate_oid(&empty)?;
        Ok(empty)
    }

    pub(crate) fn save_patch(&self, source: &Source, path: &Path) -> Result<usize, String> {
        let bytes = match source {
            Source::Commits(oids) => self.commits_patch(oids)?,
            Source::Changes(changes) => self.changes_patch(changes)?,
        };
        if bytes.is_empty() {
            return Err(format!("{NOTHING}."));
        }
        write_file(path, &bytes)?;
        Ok(bytes.len())
    }
}

pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Choose where to save the patch.".to_owned());
    }
    if path.is_dir() {
        return Err(format!("{} is a folder.", path.display()));
    }
    let parent = path.parent().ok_or_else(|| "Choose where to save the patch.".to_owned())?;
    let mut file = tempfile::Builder::new()
        .prefix(".fastkraken-patch")
        .tempfile_in(parent)
        .map_err(|e| format!("Save the patch: {e}"))?;
    file.write_all(bytes).map_err(|e| format!("Save the patch: {e}"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).map_err(|e| format!("Save the patch: {e}"))?;
    file.persist(path).map_err(|e| format!("Save the patch: {}", e.error))?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Bytes(Arc<Vec<u8>>);

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} bytes", self.0.len())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Ask {
    pub(crate) name: String,
    patch: Bytes,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) conflicts: Vec<String>,
    pub(crate) commits: Vec<String>,
    branch: String,
    seen: Seen,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) conflicts: Vec<String>,
}

pub(crate) fn subjects(patch: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(patch);
    let mut subjects = Vec::new();
    let mut in_headers = false;
    let mut in_subject = false;
    for line in text.lines() {
        if is_mail_start(line) {
            in_headers = true;
            in_subject = false;
            subjects.push(String::new());
            continue;
        }
        if !in_headers {
            continue;
        }
        if line.is_empty() {
            in_headers = false;
            in_subject = false;
        } else if in_subject && line.starts_with([' ', '\t']) {
            if let Some(last) = subjects.last_mut() {
                last.push(' ');
                last.push_str(line.trim_start());
            }
        } else if let Some(subject) = line.strip_prefix("Subject: ") {
            in_subject = true;
            if let Some(last) = subjects.last_mut() {
                *last = subject.to_owned();
            }
        } else {
            in_subject = false;
        }
    }
    if !text.lines().next().is_some_and(is_mail_start) {
        return Vec::new();
    }
    subjects.iter().map(|raw| shown_subject(raw)).collect()
}

fn shown_subject(raw: &str) -> String {
    let subject = decoded(raw);
    let subject = subject.trim();
    let subject = match subject.strip_prefix('[') {
        Some(rest) if rest.split(']').next().is_some_and(|tag| tag.starts_with("PATCH")) => {
            rest.split_once(']').map_or(subject, |(_, after)| after.trim_start())
        }
        _ => subject,
    };
    subject.to_owned()
}

fn decoded(header: &str) -> String {
    let mut text = String::new();
    let mut bytes = Vec::new();
    for (index, word) in header.split(' ').enumerate() {
        if let Some(decoded) = encoded_word(word) {
            if index > 0 && bytes.is_empty() {
                text.push(' ');
            }
            bytes.extend(decoded);
            continue;
        }
        if !bytes.is_empty() {
            text.push_str(&String::from_utf8_lossy(&std::mem::take(&mut bytes)));
        }
        if index > 0 {
            text.push(' ');
        }
        text.push_str(word);
    }
    text.push_str(&String::from_utf8_lossy(&bytes));
    text
}

fn encoded_word(word: &str) -> Option<Vec<u8>> {
    let inner = word.strip_prefix("=?")?.strip_suffix("?=")?;
    let mut parts = inner.splitn(3, '?');
    let (_, encoding, data) = (parts.next()?, parts.next()?, parts.next()?);
    if !encoding.eq_ignore_ascii_case("q") {
        return None;
    }
    let mut bytes = Vec::new();
    let mut raw = data.bytes();
    while let Some(byte) = raw.next() {
        match byte {
            b'_' => bytes.push(b' '),
            b'=' => {
                let pair = [raw.next()?, raw.next()?];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&pair).ok()?, 16).ok()?);
            }
            other => bytes.push(other),
        }
    }
    Some(bytes)
}

fn is_mail_start(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("From ") else { return false };
    let Some((oid, date)) = rest.split_once(' ') else { return false };
    (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()) && date == "Mon Sep 17 00:00:00 2001"
}

pub(crate) fn touched(numstat: &[u8]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut fields = numstat.split(|&b| b == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, |&b| b == b'\t');
        let (_, _, path) = (parts.next(), parts.next(), parts.next());
        match path {
            Some(path) if !path.is_empty() => files.push(raw_path(path)),
            Some(_) => {
                let _from = fields.next();
                if let Some(to) = fields.next() {
                    files.push(raw_path(to));
                }
            }
            None => {}
        }
    }
    files
}

pub(crate) fn conflicting(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("Applied patch to '")?.strip_suffix("' with conflicts.").map(str::to_owned))
        .collect()
}

pub(crate) fn refusal(stderr: &str, untracked: impl Fn(&str) -> bool) -> String {
    let mut said: Vec<String> = Vec::new();
    let mut named: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let Some(error) = line.trim().strip_prefix("error: ") else { continue };
        let sentence = if let Some(at) = error.strip_prefix("patch failed: ") {
            let (file, number) = at.rsplit_once(':').unwrap_or((at, ""));
            named.push(file.to_owned());
            if number.is_empty() {
                format!("{file} has changed since the patch was made.")
            } else {
                format!("{file} has changed near line {number} since the patch was made.")
            }
        } else if let Some(file) = error.strip_suffix(": patch does not apply") {
            if named.iter().any(|seen| seen == file) {
                continue;
            }
            named.push(file.to_owned());
            format!("{file} does not match the patch.")
        } else if let Some(file) = error.strip_suffix(": does not match index") {
            named.push(file.to_owned());
            format!("{file} has changes that are not staged. Stage or stash them first.")
        } else if let Some(file) = error.strip_suffix(": does not exist in index").or_else(|| error.strip_suffix(": No such file or directory")) {
            if named.iter().any(|seen| seen == file) {
                continue;
            }
            named.push(file.to_owned());
            if untracked(file) {
                format!("{file} already exists as an untracked file.")
            } else {
                format!("{file} is not in this repository.")
            }
        } else if error.starts_with("cannot read the current contents of") {
            continue;
        } else if let Some(file) = error.strip_suffix(": already exists in index").or_else(|| error.strip_suffix(": already exists in working directory")) {
            named.push(file.to_owned());
            format!("{file} already exists.")
        } else if error.starts_with("No valid patches in input") {
            "The file holds no changes Git can read.".to_owned()
        } else if error.starts_with("repository lacks the necessary blob") {
            continue;
        } else {
            let mut text = error.to_owned();
            if let Some(first) = text.get(..1) {
                text = format!("{}{}", first.to_uppercase(), &text[1..]);
            }
            if !text.ends_with('.') {
                text.push('.');
            }
            text
        };
        if !said.contains(&sentence) {
            said.push(sentence);
        }
    }
    if said.is_empty() {
        said.push("Git could not read the patch.".to_owned());
    }
    said.push(UNCHANGED.to_owned());
    said.join(" ")
}

impl Repository {
    fn untracked(&self, file: &str) -> bool {
        let path = Path::new(file);
        if validate_path(path).is_err() || self.root.join(path).symlink_metadata().is_err() {
            return false;
        }
        checked(self.read_command().args(["ls-files", "-z", "--others", "--"]).arg(path)).is_ok_and(|listed| !listed.is_empty())
    }

    pub(crate) fn check_patch(&self, patch: &[u8]) -> Result<Checked, String> {
        let reverse = run_with_input(self.command().args(["apply", "--check", "--reverse"]), patch.to_vec())
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        if reverse.status.success() {
            return Err(ALREADY.to_owned());
        }
        let check = run_with_input(self.command().args(["apply", "--check", "--3way"]), patch.to_vec())
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let stderr = String::from_utf8_lossy(&check.stderr).into_owned();
        if !check.status.success() {
            return Err(refusal(&stderr, |file| self.untracked(file)));
        }
        let numstat = run_with_input(self.command().args(["apply", "--numstat", "-z"]), patch.to_vec())
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        if !numstat.status.success() {
            return Err(refusal(&String::from_utf8_lossy(&numstat.stderr), |file| self.untracked(file)));
        }
        let mut files = touched(&numstat.stdout);
        let mut seen = std::collections::HashSet::new();
        files.retain(|path| seen.insert(path.clone()));
        Ok(Checked { files, conflicts: conflicting(&stderr) })
    }
}

pub(crate) fn read_patch(path: &Path) -> Result<Vec<u8>, String> {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    let meta = std::fs::metadata(path).map_err(|e| format!("Read {name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} is not a file."));
    }
    if meta.len() > LIMIT {
        return Err(format!("{name} is larger than 64 MiB, too large to apply here."));
    }
    std::fs::read(path).map_err(|e| format!("Read {name}: {e}"))
}

fn needing(count: usize) -> String {
    match count {
        1 => "1 file needs".to_owned(),
        n => format!("{n} files need"),
    }
}

fn gits_words(output: &std::process::Output) -> Failure {
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.trim().strip_prefix("error: ").or_else(|| line.trim().strip_prefix("fatal: ")).map(str::to_owned))
        .unwrap_or_else(|| printed.trim().lines().next().unwrap_or_default().to_owned());
    let text = if text.is_empty() { format!("Git exited with {}", output.status) } else { text };
    Failure::from(text).output(printed)
}

fn files_count(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

impl Ask {
    pub(crate) fn new(name: String, patch: Vec<u8>, checked: Checked, ui: &Ui) -> Self {
        Self {
            name,
            commits: subjects(&patch),
            patch: Bytes(Arc::new(patch)),
            files: checked.files,
            conflicts: checked.conflicts,
            branch: if ui.detached || ui.branch.is_empty() { "HEAD".to_owned() } else { ui.branch.clone() },
            seen: Seen::of(ui),
        }
    }

    pub(crate) fn apply(&self, repo: &Repository, as_commits: bool) -> Result<Outcome, Failure> {
        repo.expect_no_operation()?;
        repo.expect_checkout(&self.seen)?;
        if as_commits {
            self.apply_as_commits(repo)
        } else {
            self.apply_to_files(repo)
        }
    }

    fn apply_to_files(&self, repo: &Repository) -> Result<Outcome, Failure> {
        let output = run_with_input(repo.command().args(["apply", "--3way"]), self.patch.0.to_vec())
            .map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        match operation::snapshot(repo)? {
            Some(now) if now.kind == Operation::Applied && !now.conflicts.is_empty() => Err(Failure::from(format!(
                "{} applied with conflicts. {} resolving; the rest of the patch is staged.",
                self.name,
                needing(now.conflicts.len())
            ))
            .output(printed)),
            _ if !output.status.success() => {
                Err(Failure::from(refusal(&String::from_utf8_lossy(&output.stderr), |file| repo.untracked(file))).output(printed))
            }
            _ => Ok(Outcome::titled(
                format!("Applied {}", self.name),
                format!("{} changed. The changes are staged, ready to commit.", files_count(self.files.len())),
            )),
        }
    }

    fn apply_as_commits(&self, repo: &Repository) -> Result<Outcome, Failure> {
        if self.commits.is_empty() {
            return Err(Failure::from(format!("{} holds no commits, only changes.", self.name)));
        }
        if self.seen.head.is_some() && !operation::staged_nothing(repo)? {
            return Err(Failure::from("Commit or unstage your staged changes first, so that only the patches are committed.".to_owned()));
        }
        let mut file = tempfile::Builder::new()
            .prefix("fastkraken-patches")
            .tempfile()
            .map_err(|e| Failure::from(format!("Prepare the patches: {e}")))?;
        file.write_all(&self.patch.0).map_err(|e| Failure::from(format!("Prepare the patches: {e}")))?;
        let mut command = repo.command();
        command.env("GIT_EDITOR", "true").stdin(Stdio::null());
        let output = command
            .args(["am", "--3way", "--"])
            .arg(file.path())
            .run()
            .map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let count = self.commits.len();
        match operation::snapshot(repo)? {
            Some(now) if now.kind == Operation::Patches => {
                let at = now.step.map_or_else(String::new, |(step, of)| format!(" at patch {step} of {of}"));
                let why = if now.conflicts.is_empty() {
                    format!("{} Skip it or abort in the panel.", gits_words(&output).text)
                } else {
                    format!("{} resolving before it can go on.", needing(now.conflicts.len()))
                };
                Err(Failure::from(format!("Applying the patches stopped{at}. {why}")).output(printed))
            }
            Some(_) => Err(gits_words(&output)),
            None if !output.status.success() => Err(gits_words(&output)),
            None => Ok(Outcome::titled(
                format!("Applied {} to {}", notice::commits(count), self.branch),
                format!("{} now has the commits from {}, with their authors and messages.", self.branch, self.name),
            )),
        }
    }
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    let mut spec = Spec::new(format!("Apply {}?", ask.name), "Apply to my files");
    let listed: Vec<String> = ask.files.iter().take(6).map(|path| path.display().to_string()).collect();
    let more = ask.files.len().saturating_sub(listed.len());
    let mut changes = format!("Changes {}: {}", files_count(ask.files.len()), listed.join(", "));
    if more > 0 {
        changes.push_str(&format!(" and {more} more"));
    }
    spec = spec.line(changes);
    if !ask.conflicts.is_empty() {
        spec = spec.line(format!(
            "{} {} changed since the patch was made, so you will resolve the conflicts afterwards.",
            ask.conflicts.join(", "),
            if ask.conflicts.len() == 1 { "has" } else { "have" }
        ));
    }
    spec = spec.line("Apply to my files stages the changes, ready to commit.");
    match ask.commits.len() {
        0 => {}
        1 => {
            spec = spec
                .line(format!("It is a commit: {}. Apply as a commit adds it to {}, keeping its author and message.", ask.commits[0], ask.branch))
                .choice("Apply as a commit", false)
        }
        n => {
            spec = spec
                .line(format!("It holds {n} commits, from {} to {}. Apply as commits adds them to {}, keeping their authors and messages.", ask.commits[0], ask.commits[n - 1], ask.branch))
                .choice(format!("Apply as {n} commits"), false)
        }
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    let as_commits = answer.choice == Some(0) && !ask.commits.is_empty();
    tab.run_operation_or(
        cx,
        if as_commits { "Applying the patches…" } else { "Applying the patch…" },
        "Applied",
        as_commits,
        move |repo| ask.apply(&repo, as_commits),
        move |tab, cx| {
            if !as_commits {
                tab.select(Selection::Changes, cx);
            }
        },
        |_, _| {},
    );
}

fn saved_notice(path: &Path, size: usize) -> Notice {
    let size = if size < 1024 { format!("{size} bytes") } else { format!("{} KB", size.div_ceil(1024)) };
    Notice::new(Tone::Success, "Patch saved", format!("{} · {size}", path.display()))
        .offer("Reveal in Finder", view::Action::Handoff(crate::handoff::Action::RevealSaved(path.to_path_buf())))
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    match action {
        Action::Save(source) => {
            let name = suggested(&tab.ui, &source);
            let folder = start_folder(tab);
            choose(tab, Asked::Save { folder, name }, move |path| Action::Write { source, path }, window, cx);
        }
        Action::Write { source, path } => {
            remember(tab, &path);
            let epoch = tab.epoch;
            let shown = path.clone();
            Tab::work(
                cx,
                move || repo.save_patch(&source, &path),
                move |tab, result, cx| {
                    if tab.epoch != epoch {
                        return;
                    }
                    match result {
                        Ok(size) => {
                            tab.ui.notices.show(saved_notice(&shown, size));
                        }
                        Err(error) => {
                            tab.ui.notices.show(Notice::new(Tone::Error, "The patch was not saved", error));
                        }
                    }
                    cx.notify();
                },
            );
        }
        Action::Apply => {
            if let Some(reason) = busy(&tab.ui).or_else(|| operating(&tab.ui)) {
                tab.refuse(reason, cx);
                return;
            }
            choose(tab, Asked::Open, Action::Check, window, cx);
        }
        Action::Check(path) => {
            if tab.ui.patch.checking || tab.ui.dialog_open() {
                return;
            }
            if let Some(reason) = busy(&tab.ui).or_else(|| operating(&tab.ui)) {
                tab.refuse(reason, cx);
                return;
            }
            remember(tab, &path);
            tab.ui.patch.checking = true;
            let epoch = tab.epoch;
            let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
            Tab::work(
                cx,
                move || {
                    let patch = read_patch(&path)?;
                    let checked = repo.check_patch(&patch)?;
                    Ok::<_, String>((patch, checked))
                },
                move |tab, result, cx| {
                    tab.ui.patch.checking = false;
                    if tab.epoch != epoch {
                        cx.notify();
                        return;
                    }
                    if tab.ui.dialog_open() {
                        tab.refuse(format!("Close the open dialog, then apply {name} again"), cx);
                        return;
                    }
                    match result {
                        Ok((patch, checked)) => {
                            let ask = Ask::new(name, patch, checked, &tab.ui);
                            prompt::ask(&mut tab.ui, prompt::Kind::Patch(ask), cx);
                        }
                        Err(error) => {
                            let title = if error == ALREADY { format!("{name} is already applied") } else { format!("{name} does not apply") };
                            tab.ui.notices.show(Notice::new(Tone::Error, title, error));
                            cx.notify();
                        }
                    }
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::{commit, oid};
    use crate::repository::tests::Fixture;
    use std::fs;

    fn text(f: &Fixture, args: &[&str]) -> String {
        String::from_utf8(f.git(args)).unwrap()
    }

    fn state(f: &Fixture) -> String {
        [
            text(f, &["for-each-ref"]),
            text(f, &["ls-files", "--stage"]),
            text(f, &["status", "--porcelain=v1", "--untracked-files=all"]),
        ]
        .join("\n")
    }

    fn seen(f: &Fixture) -> Seen {
        Seen { head_ref: Some("refs/heads/main".into()), head: Some(oid(f, "HEAD")) }
    }

    fn ask(f: &Fixture, name: &str, patch: Vec<u8>) -> Ask {
        let checked = f.repo.check_patch(&patch).expect("the patch applies");
        Ask {
            name: name.to_owned(),
            commits: subjects(&patch),
            patch: Bytes(Arc::new(patch)),
            files: checked.files,
            conflicts: checked.conflicts,
            branch: "main".to_owned(),
            seen: seen(f),
        }
    }

    fn trail() -> (Fixture, Vec<String>) {
        let f = Fixture::new();
        let base = commit(&f, "trail.txt", "Trail\nRidge\nLake\n", "Start the trail");
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let ridge = commit(&f, "trail.txt", "Trail\nHigh ridge\nLake\n", "Raise the ridge");
        let lake = commit(&f, "lake.txt", "Cold water\n", "Map the lake");
        (f, vec![base, ferry, ridge, lake])
    }

    fn other(f: &Fixture, base: &str) -> Fixture {
        let g = Fixture::new();
        let files = text(f, &["ls-tree", "-r", "--name-only", base]);
        for path in files.lines() {
            fs::write(g.dir.join(path), f.git(&["show", &format!("{base}:{path}")])).unwrap();
            g.git(&["add", "--", path]);
        }
        g.git(&["commit", "-qm", "Start elsewhere"]);
        g
    }

    #[test]
    fn a_commit_saves_as_format_patch_output_and_changes_nothing() {
        let (f, oids) = trail();
        let before = state(&f);
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("ridge.patch");
        let size = f.repo.save_patch(&Source::Commits(vec![oids[2].clone()]), &path).unwrap();
        let saved = fs::read(&path).unwrap();
        assert_eq!(saved.len(), size);
        assert_eq!(saved, f.git(&["format-patch", "-1", "--stdout", &oids[2]]));
        assert_eq!(subjects(&saved), ["Raise the ridge"]);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
        assert_eq!(state(&f), before);
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 1);
    }

    #[test]
    fn picked_commits_save_oldest_first_whatever_their_dates_and_merges_are_refused() {
        let (f, oids) = trail();
        let late = commit(&f, "camp.txt", "Tents\n", "Pitch the camp");
        let amended = f.repo.command().env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00").args(["commit", "-q", "--amend", "--no-edit"]).output().unwrap();
        assert!(amended.status.success());
        let camp = oid(&f, "HEAD");
        assert_ne!(camp, late);
        assert_eq!(text(&f, &["log", "-1", "--format=%cd", "--date=format:%Y", "HEAD"]), "2001\n");
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("three.patch");
        let newest_first = vec![camp.clone(), oids[3].clone(), oids[1].clone()];
        f.repo.save_patch(&Source::Commits(newest_first), &path).unwrap();
        let saved = fs::read(&path).unwrap();
        assert_eq!(subjects(&saved), ["Add the ferry", "Map the lake", "Pitch the camp"]);
        assert!(String::from_utf8_lossy(&saved).contains("Subject: [PATCH 1/3] Add the ferry"));

        f.git(&["switch", "-qc", "side", &oids[1]]);
        commit(&f, "side.txt", "Side\n", "Take the side path");
        f.git(&["switch", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let merge = oid(&f, "HEAD");
        let before = state(&f);
        let refused = out.path().join("merge.patch");
        assert_eq!(f.repo.save_patch(&Source::Commits(vec![merge, camp]), &refused), Err(format!("{MERGE}.")));
        assert!(!refused.exists());
        assert!(f.repo.save_patch(&Source::Commits(vec!["not-an-oid".into()]), &refused).is_err());
        assert_eq!(state(&f), before);
    }

    #[test]
    fn working_changes_save_with_untracked_files_and_staged_ones_alone() {
        let (f, _) = trail();
        fs::write(f.dir.join("trail.txt"), "Trail\nHigh ridge\nLake\nShore\n").unwrap();
        fs::write(f.dir.join("ferry.txt"), "Crossings every half hour\n").unwrap();
        f.git(&["add", "--", "ferry.txt"]);
        fs::create_dir(f.dir.join("notes")).unwrap();
        fs::write(f.dir.join("notes/loose.txt"), "Loose\n").unwrap();
        fs::write(f.dir.join(".gitignore"), "*.log\n").unwrap();
        fs::write(f.dir.join("debug.log"), "noise\n").unwrap();
        let before = state(&f);
        let out = tempfile::tempdir().unwrap();

        let all = out.path().join("all.patch");
        f.repo.save_patch(&Source::Changes(Changes::All), &all).unwrap();
        let unstaged = out.path().join("unstaged.patch");
        f.repo.save_patch(&Source::Changes(Changes::Unstaged(vec!["trail.txt".into(), "notes".into()])), &unstaged).unwrap();
        let staged = out.path().join("staged.patch");
        f.repo.save_patch(&Source::Changes(Changes::Staged(vec!["ferry.txt".into()])), &staged).unwrap();
        assert_eq!(state(&f), before);

        let names = |path: &Path| {
            let text = fs::read_to_string(path).unwrap();
            text.lines().filter_map(|line| line.strip_prefix("+++ b/").map(str::to_owned)).collect::<Vec<_>>()
        };
        assert_eq!(names(&all), [".gitignore", "ferry.txt", "notes/loose.txt", "trail.txt"]);
        assert_eq!(names(&unstaged), ["notes/loose.txt", "trail.txt"]);
        assert_eq!(names(&staged), ["ferry.txt"]);
        assert!(fs::read_to_string(&staged).unwrap().contains("+Crossings every half hour"));

        let clean = Fixture::new();
        commit(&clean, "trail.txt", "Trail\nHigh ridge\nLake\n", "Start the trail");
        commit(&clean, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let check = clean.repo.check_patch(&fs::read(&all).unwrap()).unwrap();
        assert_eq!(check.files, [PathBuf::from(".gitignore"), "ferry.txt".into(), "notes/loose.txt".into(), "trail.txt".into()]);
        assert!(check.conflicts.is_empty());
    }

    #[test]
    fn saving_refuses_nothing_to_save_and_a_file_that_became_conflicted() {
        let (f, _) = trail();
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("none.patch");
        assert_eq!(f.repo.save_patch(&Source::Changes(Changes::All), &path), Err(format!("{NOTHING}.")));
        assert_eq!(f.repo.save_patch(&Source::Changes(Changes::Unstaged(vec!["trail.txt".into()])), &path), Err(format!("{NOTHING}.")));
        assert_eq!(f.repo.save_patch(&Source::Changes(Changes::Staged(Vec::new())), &path), Err(format!("{NOTHING}.")));
        assert!(f.repo.save_patch(&Source::Changes(Changes::Unstaged(vec!["../outside".into()])), &path).is_err());
        assert!(!path.exists());

        f.git(&["switch", "-qc", "side", "HEAD~2"]);
        commit(&f, "trail.txt", "Trail\nLow ridge\nLake\n", "Lower the ridge");
        f.git(&["switch", "-q", "main"]);
        let output = f.repo.command().args(["merge", "side"]).output().unwrap();
        assert!(!output.status.success());
        let before = state(&f);
        assert_eq!(f.repo.save_patch(&Source::Changes(Changes::Unstaged(vec!["trail.txt".into()])), &path), Err(format!("{CONFLICTED}.")));
        assert_eq!(f.repo.save_patch(&Source::Changes(Changes::All), &path), Err(format!("{CONFLICTED}.")));
        assert!(!path.exists());
        assert_eq!(state(&f), before);
        assert!(write_file(Path::new("relative.patch"), b"x").is_err());
        assert!(write_file(out.path(), b"x").is_err());
    }

    #[test]
    fn a_saved_change_applies_elsewhere_staged_and_a_series_applies_as_commits() {
        let (f, oids) = trail();
        let g = other(&f, &oids[1]);
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("two.patch");
        f.repo.save_patch(&Source::Commits(vec![oids[3].clone(), oids[2].clone()]), &path).unwrap();
        let bytes = read_patch(&path).unwrap();
        let before = oid(&g, "HEAD");

        let plain = ask(&g, "two.patch", bytes.clone());
        assert_eq!(plain.files, [PathBuf::from("trail.txt"), "lake.txt".into()]);
        assert_eq!(plain.commits, ["Raise the ridge", "Map the lake"]);
        let outcome = plain.apply(&g.repo, false).unwrap();
        assert_eq!(outcome.title, "Applied two.patch");
        assert_eq!(outcome.detail, "2 files changed. The changes are staged, ready to commit.");
        assert_eq!(oid(&g, "HEAD"), before);
        assert_eq!(text(&g, &["status", "--porcelain=v1"]), "A  lake.txt\nM  trail.txt\n");
        assert_eq!(g.git(&["diff", "--cached", "HEAD"]), f.git(&["diff", &oids[1], &oids[3]]));
        g.git(&["reset", "-q", "--hard"]);

        let series = ask(&g, "two.patch", bytes);
        let outcome = series.apply(&g.repo, true).unwrap();
        assert_eq!(outcome.title, "Applied 2 commits to main");
        assert_eq!(text(&g, &["log", "--format=%s", &format!("{before}..HEAD")]), "Map the lake\nRaise the ridge\n");
        assert_eq!(text(&g, &["log", "-1", "--format=%an <%ae>", "HEAD"]), text(&f, &["log", "-1", "--format=%an <%ae>", &oids[3]]));
        assert_eq!(g.git(&["diff", &before, "HEAD"]), f.git(&["diff", &oids[1], &oids[3]]));
        assert_eq!(text(&g, &["status", "--porcelain=v1"]), "");
        assert!(operation::snapshot(&g.repo).unwrap().is_none());
    }

    #[test]
    fn a_patch_that_does_not_apply_names_the_file_and_changes_nothing() {
        let (f, oids) = trail();
        let g = Fixture::new();
        commit(&g, "trail.txt", "A different trail\nentirely\n", "Start elsewhere");
        let before = state(&g);
        let patch = f.git(&["format-patch", "-1", "--stdout", &oids[2]]);
        let refused = g.repo.check_patch(&patch).unwrap_err();
        assert_eq!(refused, "trail.txt has changed near line 1 since the patch was made. Nothing was changed.");
        assert_eq!(state(&g), before);
        let again = f.git(&["format-patch", "-1", "--stdout", &oids[3]]);
        assert_eq!(f.repo.check_patch(&again).unwrap_err(), ALREADY);
        assert_eq!(
            g.repo.check_patch(b"hello\n").unwrap_err(),
            "The file holds no changes Git can read. Nothing was changed."
        );
        let missing = f.git(&["format-patch", "-1", "--stdout", &oids[1]]);
        fs::write(g.dir.join("ferry.txt"), "Already here\n").unwrap();
        assert_eq!(g.repo.check_patch(&missing).unwrap_err(), "ferry.txt already exists as an untracked file. Nothing was changed.");
        fs::remove_file(g.dir.join("ferry.txt")).unwrap();
        let ridge = f.git(&["diff", &oids[1], &oids[2]]);
        fs::write(g.dir.join("trail.txt"), "Trail\nRidge\nLake\n").unwrap();
        g.git(&["commit", "-qam", "Match the trail"]);
        fs::write(g.dir.join("trail.txt"), "Trail\nRidge\nLake\nEdited\n").unwrap();
        assert_eq!(
            g.repo.check_patch(&ridge).unwrap_err(),
            "trail.txt has changes that are not staged. Stage or stash them first. Nothing was changed."
        );
    }

    #[test]
    fn a_patch_that_needs_a_merge_stops_for_the_panel_as_an_applied_change() {
        let (f, oids) = trail();
        let ridge = f.git(&["format-patch", "-1", "--stdout", &oids[2]]);
        f.git(&["reset", "-q", "--hard", &oids[1]]);
        commit(&f, "trail.txt", "Trail\nRidge path\nLake\n", "Rename the ridge");
        let checked = f.repo.check_patch(&ridge).unwrap();
        assert_eq!(checked.conflicts, ["trail.txt"]);
        let asked = ask(&f, "ridge.patch", ridge.clone());
        let err = asked.apply(&f.repo, false).unwrap_err();
        assert_eq!(err.text, "ridge.patch applied with conflicts. 1 file needs resolving; the rest of the patch is staged.");
        let shown = operation::snapshot(&f.repo).unwrap().expect("conflicted");
        assert_eq!(shown.kind, Operation::Applied);
        assert_eq!(shown.theirs(), "applied change");
        f.git(&["reset", "-q", "--merge"]);

        let err = asked.apply(&f.repo, true).unwrap_err();
        assert_eq!(err.text, "Applying the patches stopped at patch 1 of 1. 1 file needs resolving before it can go on.");
        let shown = operation::snapshot(&f.repo).unwrap().expect("stopped");
        assert_eq!(shown.kind, Operation::Patches);
        fs::write(f.dir.join("trail.txt"), "Trail\nHigh ridge path\nLake\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        let ready = operation::snapshot(&f.repo).unwrap().unwrap();
        operation::backend::finish(&f.repo, &ready, None).unwrap();
        assert_eq!(text(&f, &["log", "-1", "--format=%s", "HEAD"]), "Raise the ridge\n");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn applying_refuses_a_repository_that_changed_since_the_check() {
        let (f, oids) = trail();
        let g = other(&f, &oids[1]);
        let patch = f.git(&["format-patch", "-1", "--stdout", &oids[3]]);
        let asked = ask(&g, "lake.patch", patch);
        commit(&g, "camp.txt", "Tents\n", "Pitch the camp");
        let before = state(&g);
        for as_commits in [false, true] {
            assert_eq!(asked.apply(&g.repo, as_commits).unwrap_err().text, "The checked-out branch or its tip changed. Reopen the action.");
        }
        assert_eq!(state(&g), before);

        let fresh = ask(&g, "lake.patch", f.git(&["format-patch", "-1", "--stdout", &oids[3]]));
        fs::write(g.dir.join("camp.txt"), "Tents and a fire\n").unwrap();
        g.git(&["add", "--", "camp.txt"]);
        let before = state(&g);
        assert_eq!(
            fresh.apply(&g.repo, true).unwrap_err().text,
            "Commit or unstage your staged changes first, so that only the patches are committed."
        );
        assert_eq!(state(&g), before);
        let plain = ask(&g, "plain.patch", f.git(&["diff", &oids[2], &oids[3]]));
        assert!(plain.commits.is_empty());
        assert_eq!(plain.apply(&g.repo, true).unwrap_err().text, "plain.patch holds no commits, only changes.");
        assert_eq!(state(&g), before);
    }

    #[test]
    fn subjects_come_only_from_format_patch_mails() {
        let mail = b"From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001\nFrom: Cedar <cedar@example.invalid>\nSubject: [PATCH 2/3] Raise the ridge\n\nbody\nSubject: not a header\nFrom 89abcdef0123456789abcdef0123456789abcdef Mon Sep 17 00:00:00 2001\nSubject: [RFC] Keep the tag\n\n";
        assert_eq!(subjects(mail), ["Raise the ridge", "[RFC] Keep the tag"]);
        assert!(subjects(b"diff --git a/x b/x\n").is_empty());
        assert!(subjects(b"From me Mon Sep 17 00:00:00 2001\n").is_empty());
        assert_eq!(touched(b"1\t0\ta b.txt\0-\t-\tpic.png\02\t1\t\0old.txt\0new.txt\0"), [PathBuf::from("a b.txt"), "pic.png".into(), "new.txt".into()]);
        assert_eq!(conflicting("Applied patch to 'a.txt' cleanly.\nApplied patch to 'b c.txt' with conflicts.\nU b c.txt\n"), ["b c.txt"]);
        assert_eq!(slug("Raise the ridge: part 2/3!"), "Raise-the-ridge-part-2-3");
        assert_eq!(slug("..."), "");
    }

    #[test]
    fn subjects_read_long_and_unicode_subjects_as_the_commits_have_them() {
        let f = Fixture::new();
        commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        let long = "Mark the trail from the car park to the high ridge and on past the lake to the shore";
        let unicode = "Café by the lake: crème brûlée =?ok?= and under_scores, ünïcode all the way down to the water";
        let first = commit(&f, "trail.txt", "Trail\n", long);
        let second = commit(&f, "lake.txt", "Lake\n", unicode);
        let one = f.git(&["format-patch", "-1", "--stdout", &second]);
        assert!(String::from_utf8_lossy(&one).contains("=?UTF-8?q?"));
        assert_eq!(subjects(&one), [unicode]);
        let two = f.git(&["format-patch", "--stdout", &format!("{first}^..{second}")]);
        assert!(String::from_utf8_lossy(&two).lines().any(|line| line.starts_with(' ')));
        assert_eq!(subjects(&two), [long, unicode]);
        assert_eq!(decoded("=?UTF-8?q?Caf=C3?= =?UTF-8?q?=A9?= au lait"), "Café au lait");
        assert_eq!(decoded("=?UTF-8?b?Q2Fmw6k=?="), "=?UTF-8?b?Q2Fmw6k=?=");
        assert_eq!(decoded("plain =?broken"), "plain =?broken");
    }

    #[test]
    fn a_staged_rename_saves_with_its_old_path_removed() {
        let f = Fixture::new();
        commit(&f, "old map.txt", "Trail\nRidge\nLake\nShore\n", "Draw the map");
        let start = oid(&f, "HEAD");
        f.git(&["mv", "old map.txt", "maps é.txt"]);
        let before = state(&f);
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("renamed.patch");
        f.repo.save_patch(&Source::Changes(Changes::Staged(vec!["maps é.txt".into()])), &path).unwrap();
        assert_eq!(state(&f), before);
        f.git(&["reset", "-q", "--hard", &start]);
        let checked = f.repo.check_patch(&fs::read(&path).unwrap()).unwrap();
        assert!(checked.files.contains(&PathBuf::from("maps é.txt")));
        let applied = f.repo.command().args(["apply", "--index"]).arg(&path).output().unwrap();
        assert!(applied.status.success());
        assert_eq!(text(&f, &["ls-files", "-z"]), "maps é.txt\0");
    }
}
