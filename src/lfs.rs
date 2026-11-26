use crate::{
    Tab, kit,
    compare::Rev,
    diff_doc::{DiffDoc, Kind, PatchState},
    image_diff::{self, Request, Side},
    repository::{Repository, nul_separated, raw_path, run, run_with_input, validate_oid},
    theme::*,
    view::{self, Desk, Selection, Tone, Ui},
};
use gpui::{Context, Div, div, prelude::*, px, rgb};
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    marked: HashMap<Source, HashSet<PathBuf>>,
    trees: VecDeque<String>,
    ticket: u64,
    checking: bool,
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

    pub(crate) fn strip_text(&self) -> String {
        format!("{NOT_INSTALLED} {}", if self.configured { WILL_FAIL } else { AS_POINTERS })
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

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Dismiss,
    CheckAgain,
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
    let dirs = crate::environment::host_dirs().to_vec();
    if !again {
        let known = HELPER.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, version)) = known.as_ref().filter(|(seen, _)| *seen == dirs) {
            return version.clone();
        }
    }
    let version = probe(repo.command());
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
    let reread = attributes || state.read_at.as_ref() != Some(&head);
    state.read_at = Some(head);
    state.ticket += 1;
    let (ticket, epoch, known) = (state.ticket, tab.epoch, state.declared);
    Tab::work(
        cx,
        move || {
            let declared = if reread { declared(&repo).unwrap_or(known) } else { known };
            let marked = filtered(&repo, &paths, None).unwrap_or_default();
            let used = declared || !marked.is_empty();
            let configured = used && configured(&repo);
            let helper = used.then(|| helper(&repo, false));
            (declared, marked, used, configured, helper)
        },
        move |tab, (declared, marked, used, configured, helper), cx| {
            if tab.epoch != epoch || tab.ui.lfs.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.lfs;
            let arrived = used && !state.used;
            state.declared = declared;
            state.used = used;
            state.configured = configured;
            if let Some(helper) = helper {
                state.helper = Some(helper);
            }
            state.keep(Source::Working, marked);
            if arrived {
                relist(tab, cx);
            }
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
                move || helper(&repo, true),
                move |tab, found, cx| {
                    if tab.epoch != epoch {
                        return;
                    }
                    tab.ui.lfs.checking = false;
                    match &found {
                        Some(version) => tab.ui.say(Tone::Info, format!("Found Git LFS: {version}")),
                        None => tab.ui.say(Tone::Error, "Git LFS is still not found. Install it, or add its folder to PATH in your shell's login files, then check again."),
                    }
                    tab.ui.lfs.helper = Some(found);
                    cx.notify();
                },
            );
        }
    }
}

pub(crate) fn badge() -> Div {
    kit::chip(kit::Chip::Plain, "LFS").flex_shrink_0()
}

pub(crate) fn strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let state = &app.ui().lfs;
    if !state.strip_shown() {
        return None;
    }
    Some(
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
            .child(div().flex_1().min_w_0().text_size(px(TEXT_SMALL)).child(state.strip_text()))
            .child(
                kit::btn("install-lfs", INSTALL_LABEL)
                    .small()
                    .icon("external")
                    .tip(INSTALL_PAGE)
                    .on(view::Action::Handoff(crate::handoff::Action::OpenUrl(INSTALL_PAGE.to_owned())), cx),
            )
            .child(
                kit::btn("check-lfs", "Check again")
                    .small()
                    .enabled(!state.checking)
                    .tip("Look for git-lfs on the search path again")
                    .on(view::Action::Lfs(Action::CheckAgain), cx),
            )
            .child(kit::icon_btn("dismiss-lfs", "x", "Dismiss", view::Action::Lfs(Action::Dismiss), cx)),
    )
}

#[cfg(test)]
mod tests {
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
