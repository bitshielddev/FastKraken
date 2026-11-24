use crate::{
    Tab,
    commands::{self, Side, Submenu, Target},
    notice::{Notice, Outcome},
    prompt::{self, Spec},
    repository::{Repository, checked, nul_separated, raw_path, run_with_input, validate_path},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

pub(crate) const FILE: &str = ".gitignore";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    Path,
    Extension,
    Folder,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Ignore { paths: Vec<PathBuf>, rule: Rule },
}

#[derive(Debug)]
pub(crate) struct Ask {
    paths: Vec<PathBuf>,
    rule: Rule,
    tracked: Vec<PathBuf>,
    gone: BTreeSet<PathBuf>,
}

const MENU: Submenu = Submenu { id: "file.ignore-menu", title: |_, _| "Ignore".into(), icon: "eye-off" };

fn unstaged(target: &Target) -> Option<Vec<&PathBuf>> {
    match target {
        Target::File { path, side: Side::Unstaged } => Some(vec![path]),
        Target::Files { paths, side: Side::Unstaged } => Some(paths.iter().collect()),
        _ => None,
    }
}

fn single(target: &Target) -> Option<&PathBuf> {
    match target {
        Target::File { path, side: Side::Unstaged } => Some(path),
        _ => None,
    }
}

fn index_edit(ui: &Ui) -> Option<String> {
    commands::busy(ui).or_else(|| ui.operation.refusal())
}

fn offered(ui: &Ui, shown: bool) -> commands::Availability {
    if shown { commands::Availability::unless(index_edit(ui)) } else { commands::Availability::Hidden }
}

fn ignoring(target: &Target, rule: Rule) -> Option<view::Action> {
    let paths = match rule {
        Rule::Path => unstaged(target)?.into_iter().cloned().collect(),
        Rule::Extension | Rule::Folder => vec![single(target)?.clone()],
    };
    Some(view::Action::Ignore(Action::Ignore { paths, rule }))
}

pub(crate) const COMMANDS: &[commands::Command] = {
    use crate::commands::{Command, Group};
    &[
        Command::new(
            "file.ignore",
            Group::Changes,
            |_, target| match target {
                Target::Files { paths, .. } => format!("Ignore {} files", paths.len()),
                Target::File { path, .. } => format!("Ignore {}", name(path)),
                _ => "Ignore".into(),
            },
            |ui, target, _| offered(ui, unstaged(target).is_some()),
            |_, target| ignoring(target, Rule::Path),
        )
        .words(&["gitignore", "exclude"])
        .under(&MENU),
        Command::new(
            "file.ignore-extension",
            Group::Changes,
            |_, target| match single(target).and_then(|path| extension(path)) {
                Some(ext) => format!("Ignore all .{} files", ext.to_string_lossy()),
                None => "Ignore all files with this extension".into(),
            },
            |ui, target, _| offered(ui, single(target).and_then(|path| extension(path)).is_some()),
            |_, target| ignoring(target, Rule::Extension),
        )
        .words(&["gitignore", "exclude", "extension"])
        .under(&MENU),
        Command::new(
            "file.ignore-folder",
            Group::Changes,
            |_, target| match single(target).and_then(|path| folder(path)) {
                Some(dir) => format!("Ignore everything in {}/", dir.to_string_lossy()),
                None => "Ignore everything in this folder".into(),
            },
            |ui, target, _| offered(ui, single(target).and_then(|path| folder(path)).is_some()),
            |_, target| ignoring(target, Rule::Folder),
        )
        .words(&["gitignore", "exclude", "directory"])
        .under(&MENU),
    ]
};

fn name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

pub(crate) fn extension(path: &Path) -> Option<&OsStr> {
    let name = path.file_name()?.as_bytes();
    let dot = name.iter().rposition(|&b| b == b'.')?;
    (dot > 0 && dot + 1 < name.len()).then(|| OsStr::from_bytes(&name[dot + 1..]))
}

pub(crate) fn folder(path: &Path) -> Option<&Path> {
    path.parent().filter(|parent| !parent.as_os_str().is_empty())
}

fn escaped(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for &byte in text {
        if matches!(byte, b'\\' | b'*' | b'?' | b'[') {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out
}

pub(crate) fn line(path: &Path, rule: Rule) -> Result<Vec<u8>, String> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.iter().any(|&b| b == b'\n' || b == b'\r') {
        return Err(format!("{} has a line break in its name, so {FILE} cannot name it.", name(path)));
    }
    let mut line = match rule {
        Rule::Path => [b"/".as_slice(), &escaped(bytes)].concat(),
        Rule::Extension => {
            let ext = extension(path).ok_or_else(|| format!("{} has no extension.", name(path)))?;
            [b"*.".as_slice(), &escaped(ext.as_bytes())].concat()
        }
        Rule::Folder => {
            let dir = folder(path).ok_or_else(|| format!("{} is not in a folder.", name(path)))?;
            [b"/".as_slice(), &escaped(dir.as_os_str().as_bytes()), b"/"].concat()
        }
    };
    if matches!(line.first(), Some(b'#' | b'!')) {
        line.insert(0, b'\\');
    }
    let spaces = line.iter().rev().take_while(|&&b| b == b' ').count();
    if spaces > 0 {
        line.truncate(line.len() - spaces);
        for _ in 0..spaces {
            line.extend_from_slice(b"\\ ");
        }
    }
    Ok(line)
}

fn lines(paths: &[PathBuf], rule: Rule) -> Result<Vec<Vec<u8>>, String> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    for path in paths {
        let line = line(path, rule)?;
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    Ok(lines)
}

pub(crate) fn appended(existing: &[u8], lines: &[Vec<u8>]) -> (Vec<u8>, usize) {
    let present: BTreeSet<&[u8]> =
        existing.split(|&b| b == b'\n').map(|line| line.strip_suffix(b"\r").unwrap_or(line)).collect();
    let mut addition = Vec::new();
    let mut added = 0;
    for line in lines.iter().filter(|line| !present.contains(line.as_slice())) {
        addition.extend_from_slice(line);
        addition.push(b'\n');
        added += 1;
    }
    if added > 0 && !existing.is_empty() && !existing.ends_with(b"\n") {
        addition.insert(0, b'\n');
    }
    (addition, added)
}

fn title(paths: &[PathBuf], rule: Rule) -> String {
    match (rule, paths) {
        (Rule::Path, [one]) => format!("Ignored {}", name(one)),
        (Rule::Path, many) => format!("Ignored {} files", many.len()),
        (Rule::Extension, [one, ..]) => {
            format!("Ignored all .{} files", extension(one).map(|ext| ext.to_string_lossy()).unwrap_or_default())
        }
        (Rule::Folder, [one, ..]) => {
            format!("Ignored everything in {}/", folder(one).map(|dir| dir.to_string_lossy()).unwrap_or_default())
        }
        _ => "Ignored".to_owned(),
    }
}

fn count(n: usize) -> String {
    match n {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

impl Repository {
    fn not_ignored(&self, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
        let input = nul_separated(paths, b"./")?;
        let mut check = self.command();
        check.args(["--no-optional-locks", "check-ignore", "--no-index", "-z", "--stdin"]);
        let output = run_with_input(&mut check, input).map_err(|e| format!("Run bundled Git: {e}"))?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let ignored: BTreeSet<PathBuf> = output
            .stdout
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| raw_path(name.strip_prefix(b"./").unwrap_or(name)))
            .collect();
        Ok(paths.iter().filter(|path| !ignored.contains(*path)).cloned().collect())
    }

    pub(crate) fn ignore(&self, paths: &[PathBuf], rule: Rule, tracked: &[PathBuf]) -> Result<Outcome, String> {
        self.ensure_index_writable()?;
        if paths.is_empty() {
            return Err("Select at least one file.".into());
        }
        for path in paths {
            validate_path(path)?;
        }
        let rules = lines(paths, rule)?;
        let indexed: BTreeSet<PathBuf> = checked(self.read_command().args(["ls-files", "-z"]))?
            .split(|&b| b == 0)
            .filter(|name| !name.is_empty())
            .map(raw_path)
            .collect();
        let expected: BTreeSet<&PathBuf> = tracked.iter().collect();
        for path in paths {
            let now = indexed.contains(path);
            if now != expected.contains(path) {
                return Err(format!(
                    "{} is {} since you chose Ignore. Look at it again before ignoring it.",
                    path.display(),
                    if now { "now tracked" } else { "no longer tracked" },
                ));
            }
        }
        let file = self.root.join(FILE);
        let existing = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(format!("Read {FILE}: {err}")),
        };
        let (addition, added) = appended(&existing, &rules);
        if !tracked.is_empty() {
            self.with_paths(&["rm", "--cached", "--quiet"], tracked).map_err(|err| {
                if err.contains("staged content different") {
                    "Some of these files have staged changes that are in neither the last commit nor the file on disk. Commit or unstage them first.".to_owned()
                } else {
                    err
                }
            })?;
        }
        if added > 0 {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
                .and_then(|mut out| out.write_all(&addition))
                .map_err(|err| match tracked {
                    [] => format!("Write {FILE}: {err}"),
                    _ => format!("Git stopped tracking the files, but {FILE} could not be written: {err}"),
                })?;
        }
        let mut detail = Vec::new();
        detail.push(match (added, rules.as_slice()) {
            (0, _) => format!("The rule was already in {FILE}."),
            (1, [only]) => format!("Added {} to {FILE}.", String::from_utf8_lossy(only)),
            (n, _) => format!("Added {n} lines to {FILE}."),
        });
        let on_disk = |path: &PathBuf| std::fs::symlink_metadata(self.root.join(path)).is_ok();
        match tracked {
            [] => {}
            [one] if on_disk(one) => detail.push(format!(
                "Git no longer tracks {}: the next commit removes it from the repository, and the file stays on disk.",
                name(one)
            )),
            [one] => detail.push(format!(
                "Git no longer tracks {}: the next commit removes it from the repository.",
                name(one)
            )),
            many if many.iter().all(on_disk) => detail.push(format!(
                "Git no longer tracks {}: the next commit removes them from the repository, and the files stay on disk.",
                count(many.len())
            )),
            many => detail.push(format!(
                "Git no longer tracks {}: the next commit removes them from the repository, and the files still on disk stay there.",
                count(many.len())
            )),
        }
        match self.not_ignored(paths)?.as_slice() {
            [] => {}
            [one] => detail.push(format!("{} still shows, because another ignore rule includes it again.", name(one))),
            many => detail.push(format!("{} still show, because another ignore rule includes them again.", count(many.len()))),
        }
        Ok(Outcome::titled(title(paths, rule), detail.join(" ")))
    }
}

pub(crate) fn describe(ask: &Ask) -> Spec {
    let rules = lines(&ask.paths, ask.rule).unwrap_or_default();
    let added = match rules.as_slice() {
        [only] => format!("{} is added to {FILE}.", String::from_utf8_lossy(only)),
        many => format!("{} lines are added to {FILE}.", many.len()),
    };
    match ask.tracked.as_slice() {
        [one] => Spec::new(format!("Ignore and stop tracking {}?", name(one)), "Ignore and stop tracking")
            .line(added)
            .line(if ask.gone.contains(one) {
                format!(
                    "{} is already deleted, and Git stops tracking it: the next commit removes it from the repository.",
                    one.display()
                )
            } else {
                format!(
                    "{} stays on disk, but Git stops tracking it: the next commit removes it from the repository.",
                    one.display()
                )
            }),
        many => {
            let mut spec =
                Spec::new(format!("Ignore and stop tracking {}?", count(many.len())), "Ignore and stop tracking");
            for path in many.iter().take(crate::picked::NAMED) {
                spec = spec.line(path.to_string_lossy().into_owned());
            }
            if many.len() > crate::picked::NAMED {
                spec = spec.line(format!("and {} more", many.len() - crate::picked::NAMED));
            }
            spec.line(added).line(if ask.gone.is_empty() {
                "The files stay on disk, but Git stops tracking them: the next commit removes them from the repository."
            } else {
                "Files still on disk stay there, but Git stops tracking them: the next commit removes them from the repository."
            })
        }
    }
}

fn refuse(tab: &mut Tab, reason: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    let Action::Ignore { paths, rule } = action;
    if !tab.ui.can_edit_index() || tab.ui.prompt.open() || paths.is_empty() {
        return;
    }
    let mut tracked = Vec::new();
    let mut gone = BTreeSet::new();
    for path in &paths {
        let Some(row) = tab.ui.files.iter().find(|file| !file.staged && &file.path == path) else {
            return refuse(tab, format!("{} is no longer in the list of changes", name(path)), cx);
        };
        if row.status != "Untracked" {
            tracked.push(path.clone());
        }
        if row.status == "Deleted" {
            gone.insert(path.clone());
        }
    }
    if let Err(reason) = lines(&paths, rule) {
        return refuse(tab, reason, cx);
    }
    if tracked.is_empty() {
        run(tab, Ask { paths, rule, tracked, gone }, cx);
    } else {
        prompt::ask(&mut tab.ui, prompt::Kind::Ignore(Ask { paths, rule, tracked, gone }), cx);
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, cx: &mut Context<Tab>) {
    run(tab, ask, cx);
}

fn run(tab: &mut Tab, ask: Ask, cx: &mut Context<Tab>) {
    let Ask { paths, rule, tracked, .. } = ask;
    tab.apply_operation(
        cx,
        "Adding the rule to .gitignore…",
        Some("Ignored"),
        false,
        move |repo| repo.ignore(&paths, rule, &tracked),
        |_, _| {},
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn listed(f: &Fixture) -> String {
        String::from_utf8(f.git(&["-c", "core.quotePath=false", "status", "--porcelain=v1", "-uall"])).unwrap()
    }

    fn rule(path: &str, rule: Rule) -> String {
        String::from_utf8(line(Path::new(path), rule).unwrap()).unwrap()
    }

    #[test]
    fn rules_name_the_path_extension_or_folder_and_escape_what_git_would_read_specially() {
        assert_eq!(rule("debug.log", Rule::Path), "/debug.log");
        assert_eq!(rule("logs/a*b?[c].log", Rule::Path), "/logs/a\\*b\\?\\[c].log");
        assert_eq!(rule("#notes", Rule::Path), "/#notes");
        assert_eq!(rule("!bang", Rule::Path), "/!bang");
        assert_eq!(rule("space  ", Rule::Path), "/space\\ \\ ");
        assert_eq!(rule("back\\slash", Rule::Path), "/back\\\\slash");
        assert_eq!(rule("deep/tree/archive.tar.gz", Rule::Extension), "*.gz");
        assert_eq!(rule("deep/tree/archive.tar.gz", Rule::Folder), "/deep/tree/");
        assert_eq!(rule("nested repo/", Rule::Path), "/nested repo/");
        assert!(extension(Path::new(".env")).is_none());
        assert!(extension(Path::new("Makefile")).is_none());
        assert!(extension(Path::new("trailing.")).is_none());
        assert!(folder(Path::new("top.txt")).is_none());
        assert!(line(Path::new("two\nlines"), Rule::Path).unwrap_err().contains("line break"));
    }

    #[test]
    fn appending_adds_a_missing_final_newline_and_skips_rules_already_there() {
        let rules = vec![b"*.log".to_vec(), b"/tmp/".to_vec()];
        assert_eq!(appended(b"", &rules), (b"*.log\n/tmp/\n".to_vec(), 2));
        assert_eq!(appended(b"build/", &rules), (b"\n*.log\n/tmp/\n".to_vec(), 2));
        assert_eq!(appended(b"*.log\r\nbuild/", &rules), (b"\n/tmp/\n".to_vec(), 1));
        assert_eq!(appended(b"/tmp/\n*.log\n", &rules), (Vec::new(), 0));
    }

    #[test]
    fn ignoring_by_name_extension_and_folder_appends_rules_and_the_files_leave_the_list() {
        let f = Fixture::new();
        fs::write(f.dir.join(FILE), "build/").unwrap();
        fs::write(f.dir.join("kept.txt"), "kept\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        fs::create_dir_all(f.dir.join("logs")).unwrap();
        fs::create_dir_all(f.dir.join("tmp/cache")).unwrap();
        for name in ["a*b.log", "axb.log", "logs/trace.log", "tmp/cache/blob.bin", "tmp/cache/other.bin", "notes.txt", "x.txt"] {
            fs::write(f.dir.join(name), "made\n").unwrap();
        }

        let outcome = f.repo.ignore(&[PathBuf::from("a*b.log")], Rule::Path, &[]).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Ignored a*b.log", "Added /a\\*b.log to .gitignore."));
        assert!(listed(&f).contains("?? axb.log\n"), "{}", listed(&f));
        assert!(!listed(&f).contains("a*b.log"));

        let outcome = f.repo.ignore(&[PathBuf::from("logs/trace.log")], Rule::Extension, &[]).unwrap();
        assert_eq!(outcome.title, "Ignored all .log files");
        let outcome = f.repo.ignore(&[PathBuf::from("tmp/cache/blob.bin")], Rule::Folder, &[]).unwrap();
        assert_eq!(outcome.title, "Ignored everything in tmp/cache/");
        let outcome = f.repo.ignore(&[PathBuf::from("notes.txt"), PathBuf::from("x.txt")], Rule::Path, &[]).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Ignored 2 files", "Added 2 lines to .gitignore."));
        let outcome = f.repo.ignore(&[PathBuf::from("logs/trace.log")], Rule::Extension, &[]).unwrap();
        assert_eq!(outcome.detail, "The rule was already in .gitignore.");

        assert_eq!(fs::read_to_string(f.dir.join(FILE)).unwrap(), "build/\n/a\\*b.log\n*.log\n/tmp/cache/\n/notes.txt\n/x.txt\n");
        assert_eq!(listed(&f), " M .gitignore\n");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(f.git(&["ls-files"]), b".gitignore\nkept.txt\n");
    }

    #[test]
    fn ignoring_a_tracked_file_stops_tracking_it_and_leaves_it_on_disk() {
        let f = Fixture::new();
        fs::write(f.dir.join("settings.local"), "secret = no\n").unwrap();
        fs::write(f.dir.join("kept.txt"), "kept\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("settings.local"), "secret = mine\n").unwrap();
        let path = PathBuf::from("settings.local");
        let outcome = f.repo.ignore(&[path.clone()], Rule::Path, &[path.clone()]).unwrap();
        assert_eq!(outcome.title, "Ignored settings.local");
        assert!(outcome.detail.contains("Git no longer tracks settings.local"), "{}", outcome.detail);
        assert_eq!(f.git(&["ls-files"]), b"kept.txt\n");
        assert_eq!(f.git(&["diff", "--cached", "--name-status"]), b"D\tsettings.local\n");
        assert_eq!(fs::read(f.dir.join("settings.local")).unwrap(), b"secret = mine\n");
        assert_eq!(fs::read(f.dir.join(FILE)).unwrap(), b"/settings.local\n");
        assert_eq!(listed(&f), "D  settings.local\n?? .gitignore\n");
    }

    #[test]
    fn ignoring_a_tracked_file_already_deleted_stops_tracking_it_without_claiming_it_is_on_disk() {
        let f = Fixture::new();
        fs::write(f.dir.join("old.cache"), "stale\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::remove_file(f.dir.join("old.cache")).unwrap();
        let path = PathBuf::from("old.cache");
        let outcome = f.repo.ignore(&[path.clone()], Rule::Path, &[path.clone()]).unwrap();
        assert_eq!(
            outcome.detail,
            "Added /old.cache to .gitignore. Git no longer tracks old.cache: the next commit removes it from the repository."
        );
        assert!(f.git(&["ls-files"]).is_empty());
        assert!(!f.dir.join("old.cache").exists());
    }

    #[test]
    fn ignoring_changes_nothing_when_a_file_is_no_longer_what_the_menu_showed() {
        let f = Fixture::new();
        fs::write(f.dir.join("kept.txt"), "kept\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("draft.txt"), "draft\n").unwrap();
        fs::write(f.dir.join("kept.txt"), "edited\n").unwrap();
        f.git(&["add", "--", "draft.txt"]);
        let index = f.git(&["ls-files", "--stage"]);
        let err = f.repo.ignore(&[PathBuf::from("draft.txt")], Rule::Path, &[]).unwrap_err();
        assert!(err.contains("draft.txt is now tracked"), "{err}");
        let err = f
            .repo
            .ignore(&[PathBuf::from("kept.txt"), PathBuf::from("gone.txt")], Rule::Path, &[PathBuf::from("kept.txt"), PathBuf::from("gone.txt")])
            .unwrap_err();
        assert!(err.contains("gone.txt is no longer tracked"), "{err}");
        assert!(!f.dir.join(FILE).exists());
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert!(f.repo.ignore(&[PathBuf::from("../outside.txt")], Rule::Path, &[]).is_err());
        assert!(!f.dir.join(FILE).exists());
    }

    #[test]
    fn a_file_another_rule_includes_again_is_reported_as_still_shown() {
        let f = Fixture::new();
        fs::create_dir_all(f.dir.join("keep")).unwrap();
        fs::write(f.dir.join("keep/.gitignore"), "!*.log\n").unwrap();
        fs::write(f.dir.join("keep/wanted.log"), "wanted\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("keep/fresh.log"), "fresh\n").unwrap();
        let outcome = f.repo.ignore(&[PathBuf::from("keep/fresh.log")], Rule::Extension, &[]).unwrap();
        assert!(outcome.detail.ends_with("fresh.log still shows, because another ignore rule includes it again."), "{}", outcome.detail);
    }

    #[test]
    fn the_prompt_for_tracked_files_names_them_and_what_is_added() {
        let local = PathBuf::from("conf/app.local");
        let mut one = Ask { paths: vec![local.clone()], rule: Rule::Path, tracked: vec![local.clone()], gone: BTreeSet::new() };
        let spec = describe(&one);
        assert_eq!((spec.title.as_str(), spec.confirm.as_ref()), ("Ignore and stop tracking app.local?", "Ignore and stop tracking"));
        assert_eq!(spec.lines[0], "/conf/app.local is added to .gitignore.");
        assert!(spec.lines[1].starts_with("conf/app.local stays on disk"), "{}", spec.lines[1]);
        one.gone.insert(local);
        assert!(describe(&one).lines[1].starts_with("conf/app.local is already deleted"));
        let paths: Vec<PathBuf> = (1..=10).map(|i| PathBuf::from(format!("{i}.local"))).collect();
        let many = Ask { paths: paths.clone(), rule: Rule::Path, tracked: paths[..9].to_vec(), gone: BTreeSet::new() };
        let spec = describe(&many);
        assert_eq!(spec.title, "Ignore and stop tracking 9 files?");
        assert_eq!(spec.lines[8], "and 1 more");
        assert_eq!(spec.lines[9], "10 lines are added to .gitignore.");
    }
}
