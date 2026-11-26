use crate::{
    Tab, jobs, kit,
    commands::{self, Availability, Side as FileSide, Submenu, Target, busy, networking, on_repo, operating},
    compare::Rev,
    diff_doc::{DiffDoc, Kind, PatchState},
    image_diff::{self, Request, Side},
    notice::Outcome,
    preferences::{self as settings_page, Section},
    prompt::{self, Answer, Field, Spec},
    repository::{Repository, checked, git_error, nul_separated, raw_path, run, run_with_input, validate_oid, validate_path, with_system_tools},
    theme::*,
    view::{self, Desk, Selection, Tone, Ui},
};
use gpui::{Context, Div, div, prelude::*, px, rgb};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsStr,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

pub(crate) const INSTALL_PAGE: &str = "https://git-lfs.com";
pub(crate) const NOT_INSTALLED: &str = "This repository stores large files with Git LFS, which is not installed on this Mac.";
const AS_POINTERS: &str = "Those files will appear as small text pointers.";
const WILL_FAIL: &str = "Checking out or staging those files fails until it is installed.";
pub(crate) const INSTALL_LABEL: &str = "How to install Git LFS";
pub(crate) const NOT_SET_UP: &str =
    "This repository stores large files with Git LFS, which is not set up for it. Those files appear as small text pointers until it is.";
pub(crate) const SET_UP_FIRST: &str = "Set up Git LFS for this repository first";
pub(crate) const SET_UP_TITLE: &str = "Set up Git LFS for this repository";
pub(crate) const ALREADY_SET_UP: &str = "Git LFS is already set up for this repository.";
pub(crate) const ROOT_ATTRIBUTES: &str = ".gitattributes";
const GLOB: &[char] = &['*', '?', '[', ']', '\\'];
const VERSIONS: [&str; 2] = ["https://git-lfs.github.com/spec/v1", "https://hawser.github.com/spec/v1"];
const POINTER_LIMIT: u64 = 1024;
const TREES_KEPT: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Working,
    Compared,
    Tree(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pointer {
    pub(crate) oid: String,
    pub(crate) size: u64,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) used: bool,
    pub(crate) configured: bool,
    pub(crate) helper: Option<Option<String>>,
    pub(crate) dismissed: bool,
    declared: bool,
    read_at: Option<Option<String>>,
    attributes_listed: bool,
    marked: HashMap<Source, HashSet<PathBuf>>,
    trees: VecDeque<String>,
    ticket: u64,
    checking: bool,
    pub(crate) patterns: Option<Result<Vec<Pattern>, String>>,
    patterns_ticket: u64,
}

impl State {
    pub(crate) fn marked(&self, source: &Source, path: &Path) -> bool {
        self.marked.get(source).is_some_and(|paths| paths.contains(path))
    }

    pub(crate) fn missing(&self) -> bool {
        self.used && matches!(self.helper, Some(None))
    }

    pub(crate) fn strip_shown(&self) -> bool {
        self.missing() && !self.dismissed
    }

    pub(crate) fn found(&self) -> bool {
        matches!(self.helper, Some(Some(_)))
    }

    pub(crate) fn offer_shown(&self) -> bool {
        self.used && self.found() && !self.configured && !self.dismissed
    }

    pub(crate) fn listed(&self) -> &[Pattern] {
        match &self.patterns {
            Some(Ok(patterns)) => patterns,
            _ => &[],
        }
    }

    pub(crate) fn strip_text(&self) -> String {
        format!("{NOT_INSTALLED} {}", if self.configured { WILL_FAIL } else { AS_POINTERS })
    }

    fn rereads(&self, head: &Option<String>, attributes: bool) -> bool {
        attributes || self.attributes_listed || self.read_at.as_ref() != Some(head)
    }

    fn keep(&mut self, source: Source, paths: HashSet<PathBuf>) {
        if let Source::Tree(oid) = &source {
            if !self.trees.contains(oid) {
                self.trees.push_back(oid.clone());
            }
            while self.trees.len() > TREES_KEPT {
                if let Some(old) = self.trees.pop_front() {
                    self.marked.remove(&Source::Tree(old));
                }
            }
            self.marked.entry(source).or_default().extend(paths);
        } else {
            self.marked.insert(source, paths);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    Extension,
    File,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    pub(crate) pattern: String,
    pub(crate) source: String,
}

impl Pattern {
    pub(crate) fn removable(&self) -> bool {
        self.source == ROOT_ATTRIBUTES
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tracking {
    pub(crate) pattern: String,
    pub(crate) literal: bool,
    pub(crate) file: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Pattern,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Dismiss,
    CheckAgain,
    SetUp,
    Track { path: PathBuf, rule: Rule },
    AddPattern,
    Untrack(Pattern),
    Fetch,
    Pull,
    Prune,
}

impl Action {
    pub(crate) fn during_operation(&self) -> bool {
        matches!(self, Action::Dismiss | Action::CheckAgain | Action::SetUp | Action::Fetch)
    }
}

pub(crate) fn filtered(repo: &Repository, paths: &[PathBuf], source: Option<&str>) -> Result<HashSet<PathBuf>, String> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let input = nul_separated(paths, b"")?;
    let mut command = repo.read_command();
    command.args(["check-attr", "-z", "--stdin"]);
    if let Some(source) = source {
        validate_oid(source)?;
        command.arg(format!("--source={source}"));
    }
    command.arg("filter");
    let output = run_with_input(&mut command, input).map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Err(crate::repository::git_error(&output.stderr, output.status));
    }
    Ok(parse_filters(&output.stdout))
}

fn parse_filters(output: &[u8]) -> HashSet<PathBuf> {
    let fields: Vec<&[u8]> = output.split(|&b| b == 0).collect();
    fields
        .chunks_exact(3)
        .filter(|record| record[1] == b"filter" && record[2] == b"lfs")
        .map(|record| raw_path(record[0]))
        .collect()
}

pub(crate) fn declared(repo: &Repository) -> Result<bool, String> {
    let info = repo.common_dir().join("info/attributes");
    if std::fs::read_to_string(info).is_ok_and(|text| text.contains("filter=lfs")) {
        return Ok(true);
    }
    let mut command = repo.command();
    command.args([
        "--no-optional-locks",
        "grep",
        "--no-recurse-submodules",
        "--no-color",
        "-l",
        "-z",
        "-I",
        "-F",
        "-e",
        "filter=lfs",
        "--",
        ":(glob)**/.gitattributes",
    ]);
    let output = run(&mut command).map_err(|e| format!("Run bundled Git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) if output.stderr.is_empty() => Ok(false),
        _ => Err(crate::repository::git_error(&output.stderr, output.status)),
    }
}

pub(crate) fn configured(repo: &Repository) -> bool {
    let mut command = repo.read_command();
    command.args(["config", "--get-regexp", r"^filter\.lfs\.(process|smudge|clean)$"]);
    run(&mut command).is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

static HELPER: Mutex<Option<(Vec<PathBuf>, Option<String>)>> = Mutex::new(None);

pub(crate) fn helper(repo: &Repository, again: bool) -> Option<String> {
    cached(repo.command(), again)
}

pub(crate) fn helper_on_path() -> bool {
    crate::repository::bundled_command(Path::new("/")).is_ok_and(|command| cached(command, false).is_some())
}

fn cached(command: Command, again: bool) -> Option<String> {
    let dirs = crate::environment::host_dirs().to_vec();
    if !again {
        let known = HELPER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, version)) = known.as_ref().filter(|(seen, _)| *seen == dirs) {
            return version.clone();
        }
    }
    let version = probe(command);
    *HELPER.lock().unwrap_or_else(|e| e.into_inner()) = Some((dirs, version.clone()));
    version
}

pub(crate) fn probe(mut command: Command) -> Option<String> {
    command.args(["-c", "help.autocorrect=0", "lfs", "version"]);
    let output = run(&mut command).ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    (output.status.success() && line.starts_with("git-lfs/")).then(|| line.to_owned())
}

fn git_lfs(repo: &Repository, network: bool) -> Result<Command, String> {
    let mut command = if network { repo.network_command()? } else { repo.command() };
    with_system_tools(&mut command);
    #[cfg(test)]
    tests::stub_first(repo, &mut command);
    command.args(["-c", "help.autocorrect=0", "lfs"]);
    Ok(command)
}

pub(crate) fn pattern_problem(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return Some("Enter a pattern, such as *.zip.".to_owned());
    }
    if text.contains(['\n', '\r', '\0']) {
        return Some("A pattern cannot have a line break.".to_owned());
    }
    if text.trim() != text {
        return Some("A pattern cannot start or end with a space.".to_owned());
    }
    if text.starts_with('#') {
        return Some("A pattern cannot start with #, which .gitattributes reads as a comment.".to_owned());
    }
    if text.starts_with('!') {
        return Some("A pattern cannot start with !, because .gitattributes has no patterns that exclude.".to_owned());
    }
    None
}

fn listed_problem(text: &str, ui: &Ui) -> Option<String> {
    pattern_problem(text).or_else(|| {
        ui.lfs.listed().iter().any(|known| known.pattern == text).then(|| format!("{text} is already tracked with Git LFS."))
    })
}

fn name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

fn extension_of(path: &Path) -> Option<&str> {
    crate::ignore::extension(path).and_then(OsStr::to_str)
}

fn folder_of(path: &Path) -> Option<&str> {
    crate::ignore::folder(path).and_then(Path::to_str)
}

pub(crate) fn tracking(path: &Path, rule: Rule) -> Result<Tracking, String> {
    validate_path(path)?;
    let text = path.to_str().ok_or_else(|| format!("Git LFS cannot track {}, whose name is not UTF-8.", name(path)))?;
    let (pattern, named) = match rule {
        Rule::File => (text.to_owned(), None),
        Rule::Extension => {
            let ext = extension_of(path).ok_or_else(|| format!("{} has no extension.", name(path)))?;
            (format!("*.{ext}"), Some(ext))
        }
        Rule::Folder => {
            let dir = folder_of(path).ok_or_else(|| format!("{} is not in a folder.", name(path)))?;
            (format!("{dir}/**"), Some(dir))
        }
    };
    if named.is_some_and(|part| part.contains(GLOB)) {
        return Err(format!(
            "{} has a character that patterns read as a wildcard. Add a pattern for it in Preferences instead.",
            named.unwrap_or_default()
        ));
    }
    if let Some(problem) = pattern_problem(&pattern) {
        return Err(problem);
    }
    Ok(Tracking { pattern, literal: rule == Rule::File, file: Some(path.to_owned()) })
}

pub(crate) fn parse_patterns(text: &str) -> Vec<Pattern> {
    let mut found = Vec::new();
    let mut section = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            section += 1;
            continue;
        }
        if section > 1 {
            continue;
        }
        let Some((pattern, source)) = line.trim().strip_suffix(')').and_then(|rest| rest.rsplit_once(" (")) else { continue };
        let pattern = pattern.strip_suffix(" [lockable]").unwrap_or(pattern).trim();
        if !pattern.is_empty() && !source.is_empty() {
            found.push(Pattern { pattern: pattern.to_owned(), source: source.to_owned() });
        }
    }
    found
}

pub(crate) fn patterns(repo: &Repository) -> Result<Vec<Pattern>, String> {
    let mut command = git_lfs(repo, false)?;
    command.env("GIT_LFS_TRACK_NO_INSTALL_HOOKS", "1").arg("track");
    let output = checked(&mut command)?;
    Ok(parse_patterns(&String::from_utf8_lossy(&output)))
}

pub(crate) fn install(repo: &Repository) -> Result<Outcome, String> {
    if configured(repo) {
        return Err(ALREADY_SET_UP.to_owned());
    }
    checked(git_lfs(repo, false)?.args(["install", "--local"]))?;
    if !configured(repo) {
        return Err("Git LFS ran but did not set up its filters for this repository.".to_owned());
    }
    let outcome = Outcome::titled(
        SET_UP_TITLE,
        "Files that .gitattributes gives to Git LFS are stored with it from their next commit, and pushing uploads them.",
    );
    Ok(if declared(repo).unwrap_or(false) { outcome.offer("Pull LFS files", view::Action::Lfs(Action::Pull)) } else { outcome })
}

pub(crate) fn set_up_new(root: &Path) -> Result<(), String> {
    install(&Repository::open(root)?).map(|_| ())
}

fn attributes(repo: &Repository) -> Option<Vec<u8>> {
    std::fs::read(repo.root.join(ROOT_ATTRIBUTES)).ok()
}

pub(crate) fn track(repo: &Repository, tracking: &Tracking) -> Result<Outcome, String> {
    repo.ensure_index_writable()?;
    if let Some(problem) = pattern_problem(&tracking.pattern) {
        return Err(problem);
    }
    match &tracking.file {
        Some(file) if !filtered(repo, std::slice::from_ref(file), None)?.is_empty() => {
            return Err(format!("{} is already stored with Git LFS. Look again.", name(file)));
        }
        None if patterns(repo)?.iter().any(|known| known.pattern == tracking.pattern) => {
            return Err(format!("{} is already tracked with Git LFS.", tracking.pattern));
        }
        _ => {}
    }
    let before = attributes(repo);
    let mut command = git_lfs(repo, false)?;
    command.arg("track");
    if tracking.literal {
        command.arg("--filename");
    }
    let said = checked(command.arg("--").arg(&tracking.pattern))?;
    if attributes(repo) == before {
        let said = String::from_utf8_lossy(&said).trim().to_owned();
        return Err(if said.is_empty() {
            "Git LFS did not change .gitattributes.".to_owned()
        } else {
            format!("Git LFS did not change .gitattributes: {said}")
        });
    }
    Ok(Outcome::titled(
        format!("Tracking {} with Git LFS", tracking.pattern),
        "The pattern is in .gitattributes, for you to commit. Files that match are stored with Git LFS from their next commit.",
    ))
}

pub(crate) fn untrack(repo: &Repository, expected: &Pattern) -> Result<Outcome, String> {
    repo.ensure_index_writable()?;
    if !expected.removable() {
        return Err(format!("{} is set in {}. Edit that file to remove it.", expected.pattern, expected.source));
    }
    if !patterns(repo)?.contains(expected) {
        return Err(format!("{} is no longer tracked in .gitattributes. Look again.", expected.pattern));
    }
    checked(git_lfs(repo, false)?.args(["untrack", "--"]).arg(&expected.pattern))?;
    if patterns(repo)?.contains(expected) {
        return Err(format!("Git LFS did not remove {} from .gitattributes.", expected.pattern));
    }
    Ok(Outcome::titled(
        format!("Stopped tracking {} with Git LFS", expected.pattern),
        "It is gone from .gitattributes, for you to commit. Files already stored with Git LFS stay so until they are added again.",
    ))
}

fn remote_problem(repo: &Repository) -> Result<(), String> {
    let remotes = checked(repo.read_command().arg("remote"))?;
    if remotes.iter().all(u8::is_ascii_whitespace) {
        return Err("This repository has no remote to download LFS files from.".to_owned());
    }
    Ok(())
}

pub(crate) fn fetch(repo: &Repository) -> Result<Outcome, String> {
    remote_problem(repo)?;
    checked(git_lfs(repo, true)?.arg("fetch"))?;
    Ok(Outcome::titled(
        "Fetched LFS files",
        "Git LFS downloaded the large files of the checked-out commit that were missing. The working copy is unchanged.",
    ))
}

pub(crate) fn pull(repo: &Repository) -> Result<Outcome, String> {
    repo.ensure_index_writable()?;
    remote_problem(repo)?;
    if !configured(repo) {
        return Err(format!("{SET_UP_FIRST}."));
    }
    checked(git_lfs(repo, true)?.arg("pull"))?;
    Ok(Outcome::titled("Pulled LFS files", "The large files of the checked-out commit are in the working copy in place of their pointers."))
}

pub(crate) fn pruned(said: &str) -> Option<usize> {
    said.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("prune: ")?;
        let (local, rest) = rest.split_once(" local object")?;
        let local: usize = local.trim().parse().ok()?;
        let retained = rest
            .split(", ")
            .find_map(|part| part.trim().strip_suffix(" retained").and_then(|count| count.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        Some(local.saturating_sub(retained))
    })
}

pub(crate) fn pruned_text(count: Option<usize>) -> String {
    match count {
        Some(0) => "Every local LFS file is still needed or not pushed yet, so none was removed.".to_owned(),
        Some(1) => "Git LFS deleted 1 local copy of a file that is pushed and no longer needed here. It can be fetched again.".to_owned(),
        Some(n) => format!("Git LFS deleted {n} local copies of files that are pushed and no longer needed here. They can be fetched again."),
        None => "Git LFS deleted the local copies it no longer needs. They can be fetched again.".to_owned(),
    }
}

pub(crate) fn prune(repo: &Repository) -> Result<Outcome, String> {
    repo.ensure_index_writable()?;
    let output = run(&mut git_lfs(repo, true)?.arg("prune")).map_err(|e| format!("Run Git LFS: {e}"))?;
    if !output.status.success() {
        return Err(git_error(&output.stderr, output.status));
    }
    let said = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Ok(Outcome::titled("Removed old LFS files", pruned_text(pruned(&said))))
}

pub(crate) fn parse(lines: &[&str]) -> Option<Pointer> {
    let (first, rest) = lines.split_first()?;
    let version = first.strip_prefix("version ")?;
    if !VERSIONS.contains(&version) {
        return None;
    }
    let (mut oid, mut size) = (None, None);
    for line in rest {
        let (key, value) = line.split_once(' ')?;
        match key {
            "oid" => {
                let hex = value.strip_prefix("sha256:")?;
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || oid.is_some() {
                    return None;
                }
                oid = Some(hex.to_owned());
            }
            "size" if size.is_none() && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => size = Some(value.parse().ok()?),
            key if key.starts_with("ext-") => {}
            _ => return None,
        }
    }
    Some(Pointer { oid: oid?, size: size? })
}

fn pointer_file(path: &Path) -> Option<Pointer> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > POINTER_LIMIT {
        return None;
    }
    let mut text = String::new();
    file.take(POINTER_LIMIT).read_to_string(&mut text).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    parse(&lines)
}

pub(crate) fn pointers(doc: &DiffDoc) -> Option<(Option<Pointer>, Option<Pointer>)> {
    if doc.state != PatchState::Text || doc.hunks.len() != 1 {
        return None;
    }
    let [file] = doc.files.as_slice() else { return None };
    let (mut old, mut new) = (Vec::new(), Vec::new());
    for line in &doc.lines {
        let text = doc.text(line);
        let body = text.get(1..).unwrap_or_default();
        match line.kind {
            Kind::Context => {
                old.push(body);
                new.push(body);
            }
            Kind::Deleted => old.push(body),
            Kind::Added => new.push(body),
            Kind::Hunk | Kind::NoNewline => {}
            Kind::File | Kind::Note => return None,
        }
    }
    let absent = |id: &Option<String>| id.as_deref().is_some_and(|id| id.bytes().all(|b| b == b'0'));
    let side = |lines: &[&str], gone: bool| -> Option<Option<Pointer>> {
        match (lines.is_empty(), gone) {
            (true, true) => Some(None),
            (false, false) => parse(lines).map(Some),
            _ => None,
        }
    };
    let old = side(&old, absent(&file.old_blob))?;
    let new = side(&new, absent(&file.new_blob))?;
    (old.is_some() || new.is_some()).then_some((old, new))
}

pub(crate) fn summary(old: Option<&Pointer>, new: Option<&Pointer>) -> String {
    let size = |pointer: &Pointer| crate::conflict::size(pointer.size);
    let id = |pointer: &Pointer| pointer.oid.get(..12).unwrap_or(&pointer.oid).to_owned();
    match (old, new) {
        (Some(old), Some(new)) => format!("LFS object · {} → {}\nsha256 {} → {}", size(old), size(new), id(old), id(new)),
        (None, Some(new)) => format!("LFS object added · {}\nsha256 {}", size(new), id(new)),
        (Some(old), None) => format!("LFS object deleted · {}\nsha256 {}", size(old), id(old)),
        (None, None) => "LFS object".to_owned(),
    }
}

fn stored(repo: &Repository, pointer: &Pointer) -> Option<PathBuf> {
    let file = repo.common_dir().join("lfs/objects").join(&pointer.oid[..2]).join(&pointer.oid[2..4]).join(&pointer.oid);
    std::fs::metadata(&file).is_ok_and(|metadata| metadata.is_file() && metadata.len() == pointer.size).then_some(file)
}

fn working(file: PathBuf) -> Side {
    Side::Working { stamp: image_diff::stamp(&file), path: file }
}

fn picture(repo: &Repository, path: &Path, old: Option<&Pointer>, new: Option<&Pointer>, selection: &Selection) -> Option<Request> {
    let format = image_diff::format_of(path)?;
    let old = match old {
        Some(pointer) => Some(working(stored(repo, pointer)?)),
        None => None,
    };
    let on_disk = matches!(selection, Selection::File { staged: false, .. } | Selection::Compare { to: Rev::WorkingTree, .. });
    let new = match new {
        Some(pointer) => {
            let file = repo.root.join(path);
            let real = on_disk && std::fs::symlink_metadata(&file).is_ok_and(|metadata| metadata.is_file()) && pointer_file(&file).is_none();
            Some(working(if real { file } else { stored(repo, pointer)? }))
        }
        None => None,
    };
    (old.is_some() || new.is_some()).then(|| Request { path: path.to_owned(), format, binary: true, sides: [old, new], heading: None })
}

pub(crate) fn recognise(repo: &Repository, doc: &mut DiffDoc, selection: &Selection) -> Option<Option<Request>> {
    let (old, new) = pointers(doc)?;
    let path = doc.files.first().and_then(|file| file.path.clone())?;
    let image = picture(repo, &path, old.as_ref(), new.as_ref(), selection);
    doc.replace_with(PatchState::Lfs { old, new });
    Some(image)
}

pub(crate) fn status_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let mut paths: Vec<PathBuf> = tab.ui.files.iter().map(|file| file.path.clone()).collect();
    paths.sort();
    paths.dedup();
    let head = tab.ui.head.clone();
    let attributes = paths.iter().any(|path| path.file_name().is_some_and(|name| name == ".gitattributes"));
    let state = &mut tab.ui.lfs;
    let reread = state.rereads(&head, attributes);
    state.ticket += 1;
    let (ticket, epoch, known) = (state.ticket, tab.epoch, state.declared);
    Tab::work(
        cx,
        move || {
            let declared = if reread { declared(&repo).unwrap_or(known) } else { known };
            let marked = filtered(&repo, &paths, None).unwrap_or_default();
            let used = declared || !marked.is_empty();
            let helper = helper(&repo, false);
            let configured = (used || helper.is_some()) && configured(&repo);
            (declared, marked, used, configured, helper)
        },
        move |tab, (declared, marked, used, configured, helper), cx| {
            if tab.epoch != epoch || tab.ui.lfs.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.lfs;
            let arrived = used && !state.used;
            let reread = state.patterns.is_none() || attributes || state.attributes_listed;
            state.read_at = Some(head);
            state.attributes_listed = attributes;
            state.declared = declared;
            state.used = used;
            state.configured = configured;
            state.helper = Some(helper);
            state.keep(Source::Working, marked);
            if arrived {
                relist(tab, cx);
            }
            if reread && patterns_wanted(&tab.ui) {
                read_patterns(tab, cx);
            }
            cx.notify();
        },
    );
}

pub(crate) fn patterns_wanted(ui: &Ui) -> bool {
    ui.preferences.open() && ui.preferences.section() == Section::Repository && ui.lfs.found()
}

pub(crate) fn read_patterns(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if !tab.ui.lfs.found() {
        return;
    }
    let state = &mut tab.ui.lfs;
    state.patterns_ticket += 1;
    let (ticket, epoch) = (state.patterns_ticket, tab.epoch);
    Tab::work(
        cx,
        move || patterns(&repo),
        move |tab, read, cx| {
            if tab.epoch != epoch || tab.ui.lfs.patterns_ticket != ticket {
                return;
            }
            tab.ui.lfs.patterns = Some(read);
            cx.notify();
        },
    );
}

pub(crate) fn listed(tab: &mut Tab, source: Source, paths: Vec<PathBuf>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if !tab.ui.lfs.used || paths.is_empty() {
        return;
    }
    let epoch = tab.epoch;
    let asked = source.clone();
    Tab::work(
        cx,
        move || {
            let tree = match &asked {
                Source::Tree(oid) => Some(oid.as_str()),
                Source::Working | Source::Compared => None,
            };
            filtered(&repo, &paths, tree)
        },
        move |tab, marked, cx| {
            if tab.epoch != epoch {
                return;
            }
            if let Ok(marked) = marked {
                tab.ui.lfs.keep(source, marked);
                cx.notify();
            }
        },
    );
}

pub(crate) fn compared_source(ui: &Ui) -> Option<Source> {
    match &ui.selection {
        Some(Selection::Compare { to: Rev::WorkingTree, .. }) => Some(Source::Compared),
        Some(Selection::Compare { to: Rev::Commit(oid), .. }) => Some(Source::Tree(oid.clone())),
        _ => None,
    }
}

fn relist(tab: &mut Tab, cx: &mut Context<Tab>) {
    let wanted = match tab.ui.selection.clone() {
        Some(Selection::Commit(oid)) => Some((Source::Tree(oid), tab.ui.inspection.files.iter().map(|file| file.path.clone()).collect())),
        Some(Selection::Stash(oid)) => Some((Source::Tree(oid), tab.ui.stash.files.iter().map(|file| file.path.clone()).collect())),
        Some(Selection::Compare { .. }) => {
            compared_source(&tab.ui).map(|source| (source, tab.ui.compare.files.iter().map(|file| file.path.clone()).collect()))
        }
        _ => None,
    };
    if let Some((source, paths)) = wanted {
        listed(tab, source, paths, cx);
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::Dismiss => {
            tab.ui.lfs.dismissed = true;
            cx.notify();
        }
        Action::CheckAgain => {
            let Some(repo) = tab.repo.clone() else { return };
            if tab.ui.lfs.checking {
                return;
            }
            tab.ui.lfs.checking = true;
            cx.notify();
            let epoch = tab.epoch;
            Tab::work(
                cx,
                move || {
                    let found = helper(&repo, true);
                    let configured = configured(&repo);
                    (found, configured)
                },
                move |tab, (found, configured), cx| {
                    if tab.epoch != epoch {
                        return;
                    }
                    tab.ui.lfs.checking = false;
                    match &found {
                        Some(version) => tab.ui.say(Tone::Info, format!("Found Git LFS: {version}")),
                        None => tab.ui.say(Tone::Error, "Git LFS is still not found. Install it, or add its folder to PATH in your shell's login files, then check again."),
                    }
                    tab.ui.lfs.helper = Some(found);
                    tab.ui.lfs.configured = configured;
                    tab.ui.lfs.patterns = None;
                    if patterns_wanted(&tab.ui) {
                        read_patterns(tab, cx);
                    }
                    cx.notify();
                },
            );
        }
        Action::SetUp => {
            if refused(tab, setup_refusal(&tab.ui), cx) {
                return;
            }
            tab.run_operation_then(cx, "Setting up Git LFS…", "Git LFS set up", false, |repo| install(&repo), |_, _| {});
        }
        Action::Track { path, rule } => {
            if refused(tab, track_refusal(&tab.ui, &path), cx) {
                return;
            }
            match tracking(&path, rule) {
                Ok(tracking) => track_with(tab, tracking, cx),
                Err(problem) => tab.error(problem, cx),
            }
        }
        Action::AddPattern => {
            if refused(tab, pattern_refusal(&tab.ui), cx) {
                return;
            }
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Lfs(Ask::Pattern), cx);
        }
        Action::Untrack(pattern) => {
            if refused(tab, operating(&tab.ui).or_else(|| busy(&tab.ui)), cx) {
                return;
            }
            let job = move |repo: Repository| untrack(&repo, &pattern);
            tab.run_operation_then(cx, "Removing the pattern…", "Pattern removed", false, job, read_patterns);
        }
        Action::Fetch | Action::Pull | Action::Prune => transfer(tab, action, cx),
    }
}

fn transfer(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    if refused(tab, transfer_refusal(&tab.ui, &action), cx) {
        return;
    }
    let (progress, notice, job): (&str, &'static str, fn(&Repository) -> Result<Outcome, String>) = match action {
        Action::Fetch => ("Fetching LFS files…", "Fetched LFS files", fetch),
        Action::Pull => ("Pulling LFS files…", "Pulled LFS files", pull),
        _ => ("Removing old LFS files…", "Removed old LFS files", prune),
    };
    tab.run_network(cx, jobs::Spec::lfs(), progress, notice, false, move |repo| job(&repo), |_, _| {});
}

fn refused(tab: &mut Tab, reason: Option<String>, cx: &mut Context<Tab>) -> bool {
    let Some(reason) = reason else { return false };
    tab.ui.menu = None;
    tab.ui.say(Tone::Info, reason);
    cx.notify();
    true
}

fn track_with(tab: &mut Tab, tracking: Tracking, cx: &mut Context<Tab>) {
    let job = move |repo: Repository| track(&repo, &tracking);
    tab.run_operation_then(cx, "Tracking with Git LFS…", "Tracked with Git LFS", false, job, read_patterns);
}

fn setup_refusal(ui: &Ui) -> Option<String> {
    if !ui.lfs.found() {
        return Some("Git LFS is not installed on this Mac".to_owned());
    }
    if ui.lfs.configured {
        return Some(ALREADY_SET_UP.trim_end_matches('.').to_owned());
    }
    busy(ui)
}

fn pattern_refusal(ui: &Ui) -> Option<String> {
    if !ui.lfs.found() {
        return Some("Git LFS is not installed on this Mac".to_owned());
    }
    (!ui.lfs.configured).then(|| SET_UP_FIRST.to_owned()).or_else(|| operating(ui)).or_else(|| busy(ui))
}

fn track_refusal(ui: &Ui, path: &Path) -> Option<String> {
    pattern_refusal(ui).or_else(|| ui.lfs.marked(&Source::Working, path).then(|| "It is already stored with Git LFS".to_owned()))
}

fn transfer_refusal(ui: &Ui, action: &Action) -> Option<String> {
    if !ui.lfs.found() {
        return Some("Git LFS is not installed on this Mac".to_owned());
    }
    let writes = matches!(action, Action::Pull | Action::Prune);
    let set_up = matches!(action, Action::Pull) && !ui.lfs.configured;
    networking(ui)
        .or_else(|| writes.then(|| operating(ui).or_else(|| busy(ui))).flatten())
        .or_else(|| set_up.then(|| SET_UP_FIRST.to_owned()))
}

pub(crate) fn describe(_: &Ask, _: &Ui) -> Spec {
    let mut spec = Spec::new("Track a pattern with Git LFS", "Track").line(
        "Files whose paths match it are stored with Git LFS from their next commit. Git LFS adds it to .gitattributes, for you to commit.",
    );
    spec.field = Some(Field {
        label: "Pattern",
        placeholder: "For example *.zip",
        initial: String::new(),
        masked: false,
        validate: Box::new(listed_problem),
    });
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Pattern => {
            if let Some(problem) = listed_problem(&answer.text, &tab.ui) {
                return tab.error(problem, cx);
            }
            track_with(tab, Tracking { pattern: answer.text, literal: false, file: None }, cx);
        }
    }
}

const MENU: Submenu = Submenu { id: "file.lfs-menu", title: |_, _| "Git LFS".into(), icon: "file" };

fn working_file(target: &Target) -> Option<&PathBuf> {
    match target {
        Target::File { path, side: FileSide::Unstaged | FileSide::Staged } => Some(path),
        _ => None,
    }
}

fn track_when(ui: &Ui, target: &Target, shown: fn(&Path) -> bool) -> Availability {
    match working_file(target) {
        Some(path) if ui.lfs.found() && shown(path) => Availability::unless(track_refusal(ui, path)),
        _ => Availability::Hidden,
    }
}

fn track_run(target: &Target, rule: Rule) -> Option<view::Action> {
    working_file(target).map(|path| view::Action::Lfs(Action::Track { path: path.clone(), rule }))
}

fn transfer_when(ui: &Ui, target: &Target, action: Action) -> Availability {
    if !on_repo(ui, target) || !ui.lfs.found() || !ui.lfs.used {
        return Availability::Hidden;
    }
    Availability::unless(transfer_refusal(ui, &action))
}

pub(crate) const COMMANDS: &[commands::Command] = {
    use crate::commands::{Command, Group};
    &[
        Command::new(
            "lfs.setup",
            Group::Settings,
            |_, _| SET_UP_TITLE.into(),
            |ui, target, _| {
                if !on_repo(ui, target) || !ui.lfs.found() || ui.lfs.configured {
                    return Availability::Hidden;
                }
                Availability::unless(setup_refusal(ui))
            },
            |_, _| Some(view::Action::Lfs(Action::SetUp)),
        )
        .words(&["git lfs install", "initialise lfs", "initialize lfs", "large files"])
        .icon("plus"),
        Command::new(
            "lfs.settings",
            Group::Settings,
            |_, _| "Git LFS settings…".into(),
            |ui, target, _| if on_repo(ui, target) && ui.lfs.found() { Availability::Enabled } else { Availability::Hidden },
            |_, _| Some(view::Action::Preferences(settings_page::Action::Open(Some(Section::Repository)))),
        )
        .words(&["configure lfs", "lfs patterns", "git lfs track", "large files"]),
        Command::new(
            "lfs.track-extension",
            Group::Changes,
            |_, target| match working_file(target).and_then(|path| extension_of(path)) {
                Some(ext) => format!("Track all .{ext} files with Git LFS"),
                None => "Track all files with this extension".into(),
            },
            |ui, target, _| track_when(ui, target, |path| extension_of(path).is_some()),
            |_, target| track_run(target, Rule::Extension),
        )
        .words(&["git lfs track", "large files", "extension"])
        .under(&MENU),
        Command::new(
            "lfs.track-file",
            Group::Changes,
            |_, target| match working_file(target) {
                Some(path) => format!("Track {} with Git LFS", name(path)),
                None => "Track this file with Git LFS".into(),
            },
            |ui, target, _| track_when(ui, target, |_| true),
            |_, target| track_run(target, Rule::File),
        )
        .words(&["git lfs track", "large files"])
        .under(&MENU),
        Command::new(
            "lfs.track-folder",
            Group::Changes,
            |_, target| match working_file(target).and_then(|path| folder_of(path)) {
                Some(dir) => format!("Track everything in {dir}/ with Git LFS"),
                None => "Track everything in this folder with Git LFS".into(),
            },
            |ui, target, _| track_when(ui, target, |path| folder_of(path).is_some()),
            |_, target| track_run(target, Rule::Folder),
        )
        .words(&["git lfs track", "large files", "directory"])
        .under(&MENU),
        Command::new(
            "lfs.fetch",
            Group::Sync,
            |_, _| "Fetch LFS files".into(),
            |ui, target, _| transfer_when(ui, target, Action::Fetch),
            |_, _| Some(view::Action::Lfs(Action::Fetch)),
        )
        .words(&["git lfs fetch", "download large files"])
        .icon("fetch"),
        Command::new(
            "lfs.pull",
            Group::Sync,
            |_, _| "Pull LFS files".into(),
            |ui, target, _| transfer_when(ui, target, Action::Pull),
            |_, _| Some(view::Action::Lfs(Action::Pull)),
        )
        .words(&["git lfs pull", "download large files", "replace pointers", "checkout lfs"])
        .icon("arrow-down"),
        Command::new(
            "lfs.prune",
            Group::Sync,
            |_, _| "Remove old LFS files".into(),
            |ui, target, _| transfer_when(ui, target, Action::Prune),
            |_, _| Some(view::Action::Lfs(Action::Prune)),
        )
        .words(&["git lfs prune", "free space", "delete old large files"])
        .icon("trash"),
    ]
};

pub(crate) fn badge() -> Div {
    kit::chip(kit::Chip::Plain, "LFS").flex_shrink_0()
}

fn strip_frame(text: String) -> Div {
    div()
        .min_h(px(36.))
        .flex_shrink_0()
        .pl(px(14.))
        .pr(px(10.))
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .bg(soft(MODIFIED, 0x18))
        .border_b_1()
        .border_color(soft(MODIFIED, 0x48))
        .child(kit::icon("alert").size(px(13.)).text_color(rgb(MODIFIED)))
        .child(div().flex_1().min_w_0().text_size(px(TEXT_SMALL)).child(text))
}

fn install_button<T: Desk>(id: &'static str, cx: &Context<T>) -> gpui::Stateful<Div> {
    kit::btn(id, INSTALL_LABEL)
        .small()
        .icon("external")
        .tip(INSTALL_PAGE)
        .on(view::Action::Handoff(crate::handoff::Action::OpenUrl(INSTALL_PAGE.to_owned())), cx)
}

fn check_button<T: Desk>(id: &'static str, state: &State, cx: &Context<T>) -> gpui::Stateful<Div> {
    kit::btn(id, "Check again")
        .small()
        .enabled(!state.checking)
        .tip("Look for git-lfs on the search path again")
        .on(view::Action::Lfs(Action::CheckAgain), cx)
}

pub(crate) fn strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let state = &app.ui().lfs;
    let dismiss = |cx: &mut Context<T>| kit::icon_btn("dismiss-lfs", "x", "Dismiss", view::Action::Lfs(Action::Dismiss), cx);
    if state.strip_shown() {
        return Some(
            strip_frame(state.strip_text())
                .child(install_button("install-lfs", cx))
                .child(check_button("check-lfs", state, cx))
                .child(dismiss(cx)),
        );
    }
    if state.offer_shown() {
        return Some(
            strip_frame(NOT_SET_UP.to_owned())
                .child(
                    kit::btn("setup-lfs", "Set up Git LFS")
                        .small()
                        .enabled(!app.ui().busy())
                        .tip("git lfs install --local")
                        .on(view::Action::Lfs(Action::SetUp), cx),
                )
                .child(dismiss(cx)),
        );
    }
    None
}

pub(crate) fn status_text(state: &State) -> Option<String> {
    let Some(Some(version)) = &state.helper else { return None };
    let shown = version.split_whitespace().next().unwrap_or(version);
    Some(if state.configured {
        format!("Set up for this repository, with {shown}.")
    } else {
        format!("Found {shown}, not set up for this repository yet. Setting it up adds its filters and hooks here.")
    })
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.lfs;
    let body = div().flex().flex_col().child(settings_page::group("GIT LFS"));
    let Some(status) = status_text(state) else {
        return match &state.helper {
            None => body.child(div().py_2().child(kit::muted("Looking for Git LFS…"))),
            Some(_) => body.child(settings_page::setting(
                "Git LFS is not installed on this Mac",
                Some("Install it to set it up here, track file patterns and download large files.".into()),
                div().flex().gap_2().child(install_button("preferences-install-lfs", cx)).child(check_button("preferences-check-lfs", state, cx)),
            )),
        };
    };
    let control = if state.configured {
        div()
    } else {
        div().child(
            kit::btn("preferences-lfs-setup", "Set up")
                .small()
                .enabled(setup_refusal(ui).is_none())
                .on(view::Action::Lfs(Action::SetUp), cx),
        )
    };
    let mut body = body.child(settings_page::setting("Git LFS", Some(status.into()), control)).child(settings_page::group("TRACKED PATTERNS"));
    body = match &state.patterns {
        None => body.child(div().py_2().child(kit::muted("Reading the patterns…"))),
        Some(Err(error)) => body.child(div().py_2().text_size(px(TEXT_SMALL)).text_color(rgb(DELETED)).child(error.clone())),
        Some(Ok(patterns)) if patterns.is_empty() => body.child(div().py_2().child(kit::muted("No file patterns are stored with Git LFS yet."))),
        Some(Ok(patterns)) => {
            let mut list = div().flex().flex_col();
            for (index, pattern) in patterns.iter().enumerate() {
                let control = if pattern.removable() {
                    div().child(
                        kit::btn(("preferences-lfs-untrack", index), "Remove")
                            .small()
                            .enabled(operating(ui).or_else(|| busy(ui)).is_none())
                            .on(view::Action::Lfs(Action::Untrack(pattern.clone())), cx),
                    )
                } else {
                    kit::muted("Edit that file to remove it")
                };
                list = list.child(settings_page::setting(pattern.pattern.clone(), Some(format!("In {}", pattern.source).into()), control));
            }
            body.child(list)
        }
    };
    let add = pattern_refusal(ui);
    body.child(
        div()
            .pt_3()
            .flex()
            .gap_2()
            .child(
                kit::btn("preferences-lfs-add", "Add pattern…")
                    .small()
                    .icon("plus")
                    .enabled(add.is_none())
                    .tip(add.unwrap_or_else(|| "git lfs track".to_owned()))
                    .on(view::Action::Lfs(Action::AddPattern), cx),
            )
            .when(state.listed().iter().any(Pattern::removable), |d| {
                d.child(
                    kit::btn("preferences-lfs-attributes", "Open .gitattributes")
                        .small()
                        .icon("external")
                        .on(view::Action::Handoff(crate::handoff::Action::Edit { path: PathBuf::from(ROOT_ATTRIBUTES), line: None }), cx),
                )
            }),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        diff_doc::{DiffDoc, Source as DiffSource},
        repository::tests::Fixture,
    };
    use std::{fs, os::unix::fs::PermissionsExt};

    fn pointer_text(digit: char, size: u64) -> String {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {size}\n", digit.to_string().repeat(64))
    }

    fn lfs_repo() -> Fixture {
        let f = Fixture::new();
        fs::write(f.dir.join(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        fs::write(f.dir.join("cover.psd"), pointer_text('a', 12_400_000)).unwrap();
        fs::write(f.dir.join("notes.txt"), "Plain text\n").unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("Add the cover").unwrap();
        f
    }

    fn read(f: &Fixture, path: &str, staged: bool) -> DiffDoc {
        let diff = f.repo.file_diff(Path::new(path), staged).unwrap();
        let sizes = HashMap::new();
        DiffDoc::build(&DiffSource {
            text: &diff.text,
            stageable: diff.hunks_stageable(),
            truncated: diff.truncated,
            sizes: &sizes,
            working_size: None,
            options: diff.options,
        })
    }

    fn stub(f: &Fixture, script: &str) -> Command {
        let bin = f.dir.join(".git/stub-bin");
        fs::create_dir_all(&bin).unwrap();
        let program = bin.join("git-lfs");
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = f.repo.command();
        command.env("PATH", std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap());
        command
    }

    thread_local! {
        static STUB: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn with_stub<R>(bin: &Path, script: &str, run: impl FnOnce() -> R) -> R {
        fs::create_dir_all(bin).unwrap();
        fs::write(bin.join("git-lfs"), script).unwrap();
        fs::set_permissions(bin.join("git-lfs"), fs::Permissions::from_mode(0o755)).unwrap();
        STUB.with(|stub| *stub.borrow_mut() = Some(bin.to_owned()));
        let result = run();
        STUB.with(|stub| *stub.borrow_mut() = None);
        result
    }

    pub(super) fn stub_first(repo: &Repository, command: &mut Command) {
        let bin = STUB.with(|stub| stub.borrow().clone()).unwrap_or_else(|| repo.git_dir().join("stub-bin"));
        if !bin.is_dir() {
            return;
        }
        let search = command.get_envs().find(|(name, _)| *name == "PATH").and_then(|(_, value)| value.map(OsStr::to_owned)).unwrap_or_default();
        let dirs = std::iter::once(bin).chain(std::env::split_paths(&search));
        command.env("PATH", std::env::join_paths(dirs).unwrap());
    }

    pub(crate) const MANAGER: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
case "$1" in
version) echo 'git-lfs/3.6.1 (test stub)' ;;
install) git config --local filter.lfs.clean 'git-lfs clean -- %f' && git config --local filter.lfs.smudge 'git-lfs smudge -- %f' && echo 'Git LFS initialized.' ;;
clean|smudge) exec cat ;;
track)
  shift
  [ "$1" = --filename ] && shift
  [ "$1" = -- ] && shift
  if [ $# -eq 0 ]; then
    [ -f .gitattributes ] || exit 0
    echo 'Listing tracked patterns'
    while read -r pattern rest; do
      case "$rest" in *filter=lfs*) printf '    %s (.gitattributes)\n' "$pattern" ;; esac
    done < .gitattributes
    echo 'Listing excluded patterns'
    exit 0
  fi
  if [ -f .gitattributes ]; then
    while read -r pattern rest; do
      [ "$pattern" = "$1" ] && echo "\"$1\" already supported" && exit 0
    done < .gitattributes
  fi
  printf '%s filter=lfs diff=lfs merge=lfs -text\n' "$1" >> .gitattributes
  echo "Tracking \"$1\"" ;;
untrack)
  shift
  [ "$1" = -- ] && shift
  grep -v -F "$1 filter=lfs" .gitattributes > .gitattributes.new
  mv .gitattributes.new .gitattributes
  echo "Untracking \"$1\"" ;;
fetch|pull) echo "$1: Fetching reference refs/heads/main" ;;
prune) echo 'prune: 6 local objects, 2 retained, done.' >&2 ;;
*) exit 1 ;;
esac
"#;

    fn managed() -> Fixture {
        let f = Fixture::new();
        fs::create_dir_all(f.dir.join("art")).unwrap();
        fs::write(f.dir.join("art/cover.psd"), b"8BPS\x00\x01").unwrap();
        fs::write(f.dir.join("notes.txt"), "Plain text\n").unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("Add the art").unwrap();
        let bin = f.dir.join(".git/stub-bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("git-lfs"), MANAGER).unwrap();
        fs::set_permissions(bin.join("git-lfs"), fs::Permissions::from_mode(0o755)).unwrap();
        f
    }

    fn calls(f: &Fixture) -> Vec<String> {
        fs::read_to_string(f.dir.join(".git/stub-bin/calls")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn unchanged(f: &Fixture, head: &str, status: &str) {
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim(), head);
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), status);
        assert_eq!(String::from_utf8(f.git(&["diff", "--cached", "--name-only"])).unwrap(), "");
    }

    fn head(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    #[test]
    fn setting_up_git_lfs_turns_its_filters_on_and_is_refused_once_they_are() {
        let f = managed();
        let first = head(&f);
        assert!(!configured(&f.repo));
        let outcome = install(&f.repo).unwrap();
        assert_eq!(outcome.title, SET_UP_TITLE);
        assert!(outcome.actions.is_empty());
        assert!(configured(&f.repo));
        assert_eq!(calls(&f), ["install --local"]);
        assert_eq!(String::from_utf8(f.git(&["config", "--local", "filter.lfs.clean"])).unwrap(), "git-lfs clean -- %f\n");
        unchanged(&f, &first, "");
        assert_eq!(install(&f.repo).unwrap_err(), ALREADY_SET_UP);
        assert_eq!(calls(&f), ["install --local"]);

        let g = managed();
        fs::write(g.dir.join(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        g.git(&["add", ".gitattributes"]);
        g.git(&["commit", "-qm", "Use LFS"]);
        let offers: Vec<String> = install(&g.repo).unwrap().actions.iter().map(|(label, _)| label.to_string()).collect();
        assert_eq!(offers, ["Pull LFS files"]);
    }

    #[test]
    fn a_file_is_tracked_by_extension_name_or_folder_and_refused_once_git_lfs_stores_it() {
        let f = managed();
        let first = head(&f);
        let cover = Path::new("art/cover.psd");
        let by_extension = tracking(cover, Rule::Extension).unwrap();
        assert_eq!(by_extension, Tracking { pattern: "*.psd".into(), literal: false, file: Some(cover.into()) });
        assert_eq!(tracking(cover, Rule::Folder).unwrap().pattern, "art/**");
        assert_eq!(tracking(cover, Rule::File).unwrap(), Tracking { pattern: "art/cover.psd".into(), literal: true, file: Some(cover.into()) });
        assert_eq!(tracking(Path::new("notes"), Rule::Extension).unwrap_err(), "notes has no extension.");
        assert_eq!(tracking(Path::new("notes.txt"), Rule::Folder).unwrap_err(), "notes.txt is not in a folder.");
        assert!(tracking(Path::new("raw[1]/shot.tif"), Rule::Folder).unwrap_err().contains("wildcard"));
        assert_eq!(tracking(Path::new("raw[1]/shot.tif"), Rule::File).unwrap().pattern, "raw[1]/shot.tif");
        assert!(tracking(Path::new("../outside.psd"), Rule::File).is_err());

        let outcome = track(&f.repo, &by_extension).unwrap();
        assert_eq!(outcome.title, "Tracking *.psd with Git LFS");
        assert_eq!(fs::read_to_string(f.dir.join(".gitattributes")).unwrap(), "*.psd filter=lfs diff=lfs merge=lfs -text\n");
        assert_eq!(calls(&f), ["track -- *.psd"]);
        assert_eq!(filtered(&f.repo, &[cover.into()], None).unwrap(), HashSet::from([cover.to_path_buf()]));
        unchanged(&f, &first, "?? .gitattributes\n");

        assert_eq!(track(&f.repo, &tracking(cover, Rule::File).unwrap()).unwrap_err(), "cover.psd is already stored with Git LFS. Look again.");
        assert_eq!(calls(&f).len(), 1);
        let notes = tracking(Path::new("notes.txt"), Rule::File).unwrap();
        track(&f.repo, &notes).unwrap();
        assert_eq!(calls(&f).last().map(String::as_str), Some("track --filename -- notes.txt"));

        let zip = Tracking { pattern: "*.zip".into(), literal: false, file: None };
        track(&f.repo, &zip).unwrap();
        assert!(fs::read_to_string(f.dir.join(".gitattributes")).unwrap().ends_with("*.zip filter=lfs diff=lfs merge=lfs -text\n"));
        assert_eq!(track(&f.repo, &zip).unwrap_err(), "*.zip is already tracked with Git LFS.");
        let fooled = Tracking { pattern: "*.zip".into(), literal: false, file: Some("notes.md".into()) };
        assert_eq!(track(&f.repo, &fooled).unwrap_err(), "Git LFS did not change .gitattributes: \"*.zip\" already supported");
        assert!(track(&f.repo, &Tracking { pattern: "#*.zip".into(), literal: false, file: None }).is_err());
        unchanged(&f, &first, "?? .gitattributes\n");

        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{first}\n")).unwrap();
        let refused = track(&f.repo, &Tracking { pattern: "*.mov".into(), literal: false, file: None }).unwrap_err();
        assert!(refused.contains("merge"), "{refused}");
        assert!(!fs::read_to_string(f.dir.join(".gitattributes")).unwrap().contains("*.mov"));
    }

    #[test]
    fn patterns_are_read_from_git_lfs_and_removed_only_while_gitattributes_still_has_them() {
        let listed = "Listing tracked patterns\n    *.psd (.gitattributes)\n    *.png [lockable] (.gitattributes)\n    art/raw/*.tif (art/raw/.gitattributes)\n    my (old) file.bin (.gitattributes)\nListing excluded patterns\n    *.txt (.gitattributes)\n";
        let parsed = parse_patterns(listed);
        let pattern = |pattern: &str, source: &str| Pattern { pattern: pattern.into(), source: source.into() };
        assert_eq!(
            parsed,
            [
                pattern("*.psd", ".gitattributes"),
                pattern("*.png", ".gitattributes"),
                pattern("art/raw/*.tif", "art/raw/.gitattributes"),
                pattern("my (old) file.bin", ".gitattributes"),
            ]
        );
        assert!(parsed[0].removable() && !parsed[2].removable());
        assert!(parse_patterns("").is_empty());

        let f = managed();
        fs::write(f.dir.join(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n*.zip filter=lfs diff=lfs merge=lfs -text\n*.txt text\n").unwrap();
        f.git(&["add", ".gitattributes"]);
        f.git(&["commit", "-qm", "Track art"]);
        let second = head(&f);
        let read = patterns(&f.repo).unwrap();
        assert_eq!(read, [pattern("*.psd", ".gitattributes"), pattern("*.zip", ".gitattributes")]);
        assert_eq!(calls(&f), ["track"]);
        unchanged(&f, &second, "");

        let outcome = untrack(&f.repo, &read[0]).unwrap();
        assert_eq!(outcome.title, "Stopped tracking *.psd with Git LFS");
        assert_eq!(fs::read_to_string(f.dir.join(".gitattributes")).unwrap(), "*.zip filter=lfs diff=lfs merge=lfs -text\n*.txt text\n");
        assert!(calls(&f).contains(&"untrack -- *.psd".to_owned()));
        unchanged(&f, &second, " M .gitattributes\n");

        let before = calls(&f).len();
        assert_eq!(untrack(&f.repo, &read[0]).unwrap_err(), "*.psd is no longer tracked in .gitattributes. Look again.");
        assert_eq!(untrack(&f.repo, &pattern("art/raw/*.tif", "art/raw/.gitattributes")).unwrap_err(), "art/raw/*.tif is set in art/raw/.gitattributes. Edit that file to remove it.");
        assert!(!calls(&f)[before..].iter().any(|call| call.starts_with("untrack")));
        assert_eq!(fs::read_to_string(f.dir.join(".gitattributes")).unwrap(), "*.zip filter=lfs diff=lfs merge=lfs -text\n*.txt text\n");
    }

    #[test]
    fn lfs_files_are_fetched_pulled_and_pruned_only_where_git_lfs_can_reach_them() {
        let f = managed();
        let first = head(&f);
        assert_eq!(fetch(&f.repo).unwrap_err(), "This repository has no remote to download LFS files from.");
        assert_eq!(pull(&f.repo).unwrap_err(), "This repository has no remote to download LFS files from.");
        assert!(calls(&f).is_empty());

        let remote = f.dir.join(".git/stub-remote.git");
        f.git(&["init", "-q", "--bare", remote.to_str().unwrap()]);
        f.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        assert_eq!(fetch(&f.repo).unwrap().title, "Fetched LFS files");
        assert_eq!(calls(&f), ["fetch"]);
        assert_eq!(pull(&f.repo).unwrap_err(), format!("{SET_UP_FIRST}."));
        install(&f.repo).unwrap();
        assert_eq!(pull(&f.repo).unwrap().title, "Pulled LFS files");
        let outcome = prune(&f.repo).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Removed old LFS files", pruned_text(Some(4)).as_str()));
        assert_eq!(calls(&f), ["fetch", "install --local", "pull", "prune"]);
        unchanged(&f, &first, "");
        assert_eq!(String::from_utf8(f.git(&["for-each-ref", "--format=%(refname)"])).unwrap(), "refs/heads/main\n");

        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{first}\n")).unwrap();
        assert!(pull(&f.repo).unwrap_err().contains("merge"));
        assert!(prune(&f.repo).unwrap_err().contains("merge"));
        assert_eq!(calls(&f).len(), 4);
    }

    #[test]
    fn prune_reports_how_many_local_copies_went() {
        assert_eq!(pruned("prune: 6 local objects, 2 retained, done.\nprune: Deleting objects: 100% (4/4), done."), Some(4));
        assert_eq!(pruned("prune: 1 local object, 0 retained, 1 verified with remote, done."), Some(1));
        assert_eq!(pruned("prune: 3 local objects, 3 retained, done."), Some(0));
        assert_eq!(pruned("something else"), None);
        assert_eq!(pruned_text(Some(0)), "Every local LFS file is still needed or not pushed yet, so none was removed.");
        assert!(pruned_text(Some(1)).starts_with("Git LFS deleted 1 local copy of a file"));
        assert!(pruned_text(Some(3)).starts_with("Git LFS deleted 3 local copies of files"));
    }

    #[test]
    fn a_typed_pattern_is_checked_before_git_lfs_sees_it() {
        assert_eq!(pattern_problem("*.zip"), None);
        assert_eq!(pattern_problem("art/**"), None);
        assert_eq!(pattern_problem("  "), Some("Enter a pattern, such as *.zip.".to_owned()));
        assert_eq!(pattern_problem("*.zip\n*.psd"), Some("A pattern cannot have a line break.".to_owned()));
        assert_eq!(pattern_problem(" *.zip"), Some("A pattern cannot start or end with a space.".to_owned()));
        assert!(pattern_problem("#zip").is_some_and(|problem| problem.contains("comment")));
        assert!(pattern_problem("!*.zip").is_some());
    }

    #[test]
    fn the_set_up_strip_shows_for_a_repository_using_lfs_with_the_helper_but_not_its_filters() {
        let mut state = State { used: true, helper: Some(Some("git-lfs/3.6.1 (GitHub; darwin arm64)".into())), ..State::default() };
        assert!(state.offer_shown() && !state.strip_shown());
        assert_eq!(status_text(&state).unwrap(), "Found git-lfs/3.6.1, not set up for this repository yet. Setting it up adds its filters and hooks here.");
        state.configured = true;
        assert!(!state.offer_shown());
        assert_eq!(status_text(&state).unwrap(), "Set up for this repository, with git-lfs/3.6.1.");
        state.configured = false;
        state.dismissed = true;
        assert!(!state.offer_shown());
        state.dismissed = false;
        state.used = false;
        assert!(!state.offer_shown());
        state.helper = Some(None);
        assert_eq!(status_text(&state), None);
        assert!(Action::Fetch.during_operation() && !Action::Pull.during_operation() && !Action::AddPattern.during_operation());
    }

    #[test]
    fn files_with_the_lfs_filter_are_found_in_the_working_tree_and_in_a_commit() {
        let f = lfs_repo();
        let first = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let paths = vec![PathBuf::from("cover.psd"), PathBuf::from("notes.txt"), PathBuf::from("art/new.psd")];
        assert_eq!(filtered(&f.repo, &paths, None).unwrap(), HashSet::from([PathBuf::from("cover.psd"), PathBuf::from("art/new.psd")]));
        assert!(declared(&f.repo).unwrap());

        fs::write(f.dir.join(".gitattributes"), "*.png filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        assert_eq!(filtered(&f.repo, &paths, None).unwrap(), HashSet::new());
        assert_eq!(filtered(&f.repo, &paths, Some(&first)).unwrap(), HashSet::from([PathBuf::from("cover.psd"), PathBuf::from("art/new.psd")]));
        assert!(filtered(&f.repo, &paths, Some("--output=x")).is_err());
        assert_eq!(filtered(&f.repo, &[], None).unwrap(), HashSet::new());

        let status = String::from_utf8(f.git(&["status", "--porcelain"])).unwrap();
        assert_eq!(status, " M .gitattributes\n");
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim(), first);
    }

    #[test]
    fn a_repository_declares_lfs_in_any_gitattributes_or_its_info_attributes() {
        let f = Fixture::new();
        fs::write(f.dir.join("notes.txt"), "Plain text\n").unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("Plain").unwrap();
        assert!(!declared(&f.repo).unwrap());
        assert!(!configured(&f.repo));

        fs::create_dir_all(f.dir.join("art/raw")).unwrap();
        fs::write(f.dir.join("art/raw/.gitattributes"), "*.tif filter=lfs -text\n").unwrap();
        assert!(!declared(&f.repo).unwrap());
        f.repo.stage_all().unwrap();
        assert!(declared(&f.repo).unwrap());

        let g = Fixture::new();
        fs::write(g.dir.join("notes.txt"), "Plain text\n").unwrap();
        g.repo.stage_all().unwrap();
        g.repo.commit("Plain").unwrap();
        fs::create_dir_all(g.dir.join(".git/info")).unwrap();
        fs::write(g.dir.join(".git/info/attributes"), "*.bin filter=lfs\n").unwrap();
        assert!(declared(&g.repo).unwrap());

        g.git(&["config", "filter.lfs.process", "git-lfs filter-process"]);
        assert!(configured(&g.repo));
        assert_eq!(String::from_utf8(g.git(&["status", "--porcelain"])).unwrap(), "");
    }

    #[test]
    fn the_helper_is_found_by_its_version_and_missing_when_git_has_no_lfs_command() {
        let f = Fixture::new();
        let found = stub(&f, "#!/bin/sh\n[ \"$1\" = version ] && echo 'git-lfs/3.6.1 (GitHub; darwin arm64; go 1.23.4)'\n");
        assert_eq!(probe(found).as_deref(), Some("git-lfs/3.6.1 (GitHub; darwin arm64; go 1.23.4)"));
        let broken = stub(&f, "#!/bin/sh\necho 'something else'\nexit 0\n");
        assert_eq!(probe(broken), None);
        let mut absent = f.repo.command();
        absent.env("PATH", "/usr/bin:/bin");
        assert_eq!(probe(absent), None);
    }

    #[test]
    fn a_pointer_is_read_only_when_it_follows_the_specification() {
        let text = pointer_text('b', 13_100_000);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(parse(&lines), Some(Pointer { oid: "b".repeat(64), size: 13_100_000 }));
        let with_extension = ["version https://git-lfs.github.com/spec/v1", "ext-0-foo sha256:1", &lines[1], &lines[2]];
        assert_eq!(parse(&with_extension).map(|pointer| pointer.size), Some(13_100_000));
        for wrong in [
            vec!["version https://example.com/spec/v1", lines[1], lines[2]],
            vec![lines[1], lines[0], lines[2]],
            vec![lines[0], "oid sha256:abc", lines[2]],
            vec![lines[0], lines[1], "size -4"],
            vec![lines[0], lines[1]],
            vec![lines[0], lines[1], lines[2], "colour blue"],
        ] {
            assert_eq!(parse(&wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn a_pointer_diff_reads_as_the_sizes_of_the_lfs_objects() {
        let f = lfs_repo();
        fs::write(f.dir.join("cover.psd"), pointer_text('c', 13_100_000)).unwrap();
        fs::write(f.dir.join("notes.txt"), "Plain text, edited\n").unwrap();
        let mut changed = read(&f, "cover.psd", false);
        let (old, new) = pointers(&changed).unwrap();
        assert_eq!((old.as_ref().unwrap().size, new.as_ref().unwrap().size), (12_400_000, 13_100_000));
        let selection = Selection::File { path: "cover.psd".into(), staged: false };
        assert_eq!(recognise(&f.repo, &mut changed, &selection), Some(None));
        assert!(changed.lines.is_empty() && !changed.stageable);
        let PatchState::Lfs { old, new } = &changed.state else { panic!("{:?}", changed.state) };
        assert_eq!(summary(old.as_ref(), new.as_ref()), format!("LFS object · 12.4 MB → 13.1 MB\nsha256 {} → {}", "a".repeat(12), "c".repeat(12)));
        assert!(pointers(&read(&f, "notes.txt", false)).is_none());

        fs::write(f.dir.join("poster.psd"), pointer_text('d', 900)).unwrap();
        f.git(&["add", "poster.psd"]);
        let added = read(&f, "poster.psd", true);
        assert_eq!(pointers(&added), Some((None, Some(Pointer { oid: "d".repeat(64), size: 900 }))));
        assert_eq!(summary(None, Some(&Pointer { oid: "d".repeat(64), size: 900 })), format!("LFS object added · 900 bytes\nsha256 {}", "d".repeat(12)));
        f.git(&["rm", "-q", "--cached", "cover.psd"]);
        let deleted = read(&f, "cover.psd", true);
        assert_eq!(pointers(&deleted).map(|(old, new)| (old.map(|p| p.size), new)), Some((Some(12_400_000), None)));

        fs::write(f.dir.join("cover.psd"), "not a pointer any more\n").unwrap();
        assert!(pointers(&read(&f, "cover.psd", false)).is_none());
        let index = String::from_utf8(f.git(&["diff", "--cached", "--name-status"])).unwrap();
        assert_eq!(index, "D\tcover.psd\nA\tposter.psd\n");
    }

    #[test]
    fn an_lfs_image_is_shown_from_the_working_tree_or_the_lfs_store() {
        let f = Fixture::new();
        fs::write(f.dir.join(".gitattributes"), "*.png filter=lfs diff=lfs merge=lfs -text\n").unwrap();
        let old = Pointer { oid: "e".repeat(64), size: 8 };
        fs::write(f.dir.join("logo.png"), pointer_text('e', 8)).unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("Add the logo").unwrap();
        fs::write(f.dir.join("logo.png"), pointer_text('f', 9)).unwrap();
        let selection = Selection::File { path: "logo.png".into(), staged: false };
        let mut nothing_stored = read(&f, "logo.png", false);
        assert_eq!(recognise(&f.repo, &mut nothing_stored, &selection), Some(None));

        let store = f.dir.join(".git/lfs/objects/ee/ee");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join(&old.oid), b"\x89PNG\r\n\x1a\n").unwrap();
        let mut still_a_pointer = read(&f, "logo.png", false);
        assert_eq!(recognise(&f.repo, &mut still_a_pointer, &selection), Some(None));

        let real = f.dir.join(".git/lfs/objects/ff/ff");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("f".repeat(64)), b"\x89PNG\r\n\x1a\n\x00").unwrap();
        let mut both = read(&f, "logo.png", false);
        let request = recognise(&f.repo, &mut both, &selection).unwrap().unwrap();
        assert!(request.binary);
        assert!(matches!(&request.sides[0], Some(Side::Working { path, .. }) if path.ends_with(format!("lfs/objects/ee/ee/{}", old.oid))));
        assert!(matches!(&request.sides[1], Some(Side::Working { path, .. }) if path.ends_with(format!("lfs/objects/ff/ff/{}", "f".repeat(64)))));

        let doc = read(&f, "logo.png", false);
        fs::write(f.dir.join("logo.png"), b"\x89PNG\r\n\x1a\n\x00\x01").unwrap();
        let mut checked_out = doc.clone();
        let request = recognise(&f.repo, &mut checked_out, &selection).unwrap().unwrap();
        assert!(matches!(&request.sides[1], Some(Side::Working { path, .. }) if path.ends_with("logo.png") && !path.to_string_lossy().contains("lfs/objects")));
        let committed = Selection::Commit("HEAD".into());
        let mut from_commit = doc;
        let request = recognise(&f.repo, &mut from_commit, &committed).unwrap().unwrap();
        assert!(matches!(&request.sides[1], Some(Side::Working { path, .. }) if path.ends_with(format!("lfs/objects/ff/ff/{}", "f".repeat(64)))));
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), " M logo.png\n");
    }

    #[test]
    fn the_strip_shows_only_for_a_repository_using_lfs_without_the_helper_until_dismissed() {
        let mut state = State::default();
        assert!(!state.strip_shown());
        state.used = true;
        assert!(!state.strip_shown());
        state.helper = Some(Some("git-lfs/3.6.1".into()));
        assert!(!state.strip_shown());
        state.helper = Some(None);
        assert!(state.strip_shown());
        assert_eq!(
            state.strip_text(),
            "This repository stores large files with Git LFS, which is not installed on this Mac. Those files will appear as small text pointers."
        );
        state.configured = true;
        assert!(state.strip_text().ends_with("Checking out or staging those files fails until it is installed."));
        state.dismissed = true;
        assert!(!state.strip_shown() && state.missing());
    }

    #[test]
    fn the_attributes_are_read_again_until_a_reading_lands_and_after_a_listed_gitattributes_goes() {
        let mut state = State::default();
        let head = Some("a".repeat(40));
        assert!(state.rereads(&head, false));
        assert!(state.rereads(&head, false));
        state.read_at = Some(head.clone());
        assert!(!state.rereads(&head, false));
        assert!(state.rereads(&head, true));
        assert!(state.rereads(&Some("b".repeat(40)), false));
        state.attributes_listed = true;
        assert!(state.rereads(&head, false));
        state.attributes_listed = false;
        assert!(!state.rereads(&head, false));
    }

    #[test]
    fn marked_trees_are_merged_and_only_the_latest_are_kept() {
        let mut state = State::default();
        state.keep(Source::Working, HashSet::from([PathBuf::from("a.psd")]));
        state.keep(Source::Working, HashSet::from([PathBuf::from("b.psd")]));
        assert!(!state.marked(&Source::Working, Path::new("a.psd")) && state.marked(&Source::Working, Path::new("b.psd")));
        let tree = |n: usize| Source::Tree(format!("{n:040}"));
        state.keep(tree(0), HashSet::from([PathBuf::from("a.psd")]));
        state.keep(tree(0), HashSet::from([PathBuf::from("c.psd")]));
        assert!(state.marked(&tree(0), Path::new("a.psd")) && state.marked(&tree(0), Path::new("c.psd")));
        for n in 1..=TREES_KEPT {
            state.keep(tree(n), HashSet::from([PathBuf::from("a.psd")]));
        }
        assert!(!state.marked(&tree(0), Path::new("a.psd")));
        assert!(state.marked(&tree(TREES_KEPT), Path::new("a.psd")) && state.marked(&Source::Working, Path::new("b.psd")));
    }
}
