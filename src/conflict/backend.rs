use crate::{
    merge::{self, Side},
    operation::{self, Conflict, Id, Snapshot},
    repository::{Repository, checked, validate_path},
};
use std::{
    fs,
    io::{self, BufRead, BufReader},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;

type Entries = [Option<(String, String)>; 3];

#[derive(Clone, Debug)]
pub(crate) struct Shown {
    pub id: Id,
    pub path: PathBuf,
    stages: Entries,
}

impl Shown {
    pub(crate) fn of(snapshot: &Snapshot, conflict: &Conflict) -> Self {
        Self {
            id: snapshot.id.clone(),
            path: conflict.path.clone(),
            stages: conflict.stages.clone().map(|stage| stage.map(|version| (version.mode, version.oid))),
        }
    }

    fn one_side_deleted(&self) -> bool {
        self.stages[1].is_none() || self.stages[2].is_none()
    }

    fn submodule(&self) -> bool {
        self.stages.iter().flatten().any(|(mode, _)| mode == "160000")
    }
}

pub(crate) fn named(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn entries(repo: &Repository, which: &str, path: &Path) -> Result<Entries> {
    entries_in(repo, None, which, path)
}

fn entries_in(repo: &Repository, index: Option<&Path>, which: &str, path: &Path) -> Result<Entries> {
    let mut command = repo.read_command();
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let records = checked(command.args(["ls-files", which, "-z", "--"]).arg(path))?;
    Ok(operation::parse_stages(&records)?.remove(path).unwrap_or_default())
}

fn is_none(entries: &Entries) -> bool {
    entries.iter().all(Option::is_none)
}

fn expect(repo: &Repository, shown: &Shown) -> Result<()> {
    validate_path(&shown.path)?;
    repo.expect_operation(&shown.id)?;
    let now = entries(repo, "--unmerged", &shown.path)?;
    if is_none(&now) {
        return Err(format!("{} is no longer in conflict.", named(&shown.path)));
    }
    if now != shown.stages {
        return Err(format!("{} changed since the window showed it. Look at it again.", named(&shown.path)));
    }
    Ok(())
}

fn settled(repo: &Repository, path: &Path) -> Result<()> {
    if is_none(&entries(repo, "--unmerged", path)?) {
        Ok(())
    } else {
        Err(format!("Git left {} in conflict.", named(path)))
    }
}

fn on_disk(repo: &Repository, path: &Path) -> Result<Option<fs::Metadata>> {
    let absolute = repo.root.join(path);
    let parent = absolute.parent().ok_or("Missing parent directory")?;
    match fs::canonicalize(parent) {
        Ok(real) if real == parent => {}
        Ok(_) => return Err(format!("{} is inside a folder that is a link.", named(path))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Read {}: {e}", named(path))),
    }
    match fs::symlink_metadata(&absolute) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Read {}: {e}", named(path))),
    }
}

fn scan(mut reader: impl BufRead, width: usize) -> io::Result<bool> {
    let keep = width + 2;
    let mut head = Vec::with_capacity(keep);
    let mut full = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let read = buffer.len();
        for &byte in buffer {
            if byte == b'\n' {
                if !full {
                    head.push(b'\n');
                }
                if merge::marker(&head, width).is_some() {
                    return Ok(true);
                }
                head.clear();
                full = false;
            } else if !full {
                head.push(byte);
                full = head.len() >= keep;
            }
        }
        reader.consume(read);
    }
    Ok(merge::marker(&head, width).is_some())
}

pub(crate) fn has_markers(repo: &Repository, path: &Path) -> Result<bool> {
    validate_path(path)?;
    match on_disk(repo, path)? {
        Some(meta) if meta.file_type().is_file() => {}
        _ => return Ok(false),
    }
    let width = merge::marker_width(repo, path)?;
    let file = fs::File::open(repo.root.join(path)).map_err(|e| format!("Read {}: {e}", named(path)))?;
    scan(BufReader::new(file), width).map_err(|e| format!("Read {}: {e}", named(path)))
}

fn add(repo: &Repository, path: &Path) -> Result<()> {
    checked(repo.command().args(["--literal-pathspecs", "add", "--"]).arg(path)).map(drop)
}

pub(crate) fn take(repo: &Repository, shown: &Shown, side: Side) -> Result<()> {
    expect(repo, shown)?;
    let (stage, flag) = match side {
        Side::Current => (1, "--ours"),
        Side::Incoming => (2, "--theirs"),
    };
    let Some((mode, oid)) = shown.stages[stage].clone() else {
        return Err(format!("That side deleted {}. Delete the file instead.", named(&shown.path)));
    };
    if mode == "160000" {
        let path = shown.path.as_os_str().as_bytes();
        let mut input = format!("0 {}\t", "0".repeat(oid.len())).into_bytes();
        input.extend_from_slice(path);
        input.push(0);
        input.extend_from_slice(format!("{mode} {oid} 0\t").as_bytes());
        input.extend_from_slice(path);
        input.push(0);
        operation::with_input(repo.command().args(["update-index", "-z", "--index-info"]), input)?;
    } else {
        checked(repo.command().args(["--literal-pathspecs", "checkout", flag, "--"]).arg(&shown.path))?;
        add(repo, &shown.path)?;
    }
    settled(repo, &shown.path)
}

pub(crate) fn keep(repo: &Repository, shown: &Shown) -> Result<()> {
    expect(repo, shown)?;
    if !shown.one_side_deleted() {
        return Err(format!("Both sides have a version of {}. Take one of them instead.", named(&shown.path)));
    }
    if on_disk(repo, &shown.path)?.is_none() {
        return Err(format!(
            "{} is no longer in the working tree, so there is no changed file to keep. Delete it instead, or put it back first.",
            named(&shown.path)
        ));
    }
    add(repo, &shown.path)?;
    settled(repo, &shown.path)
}

pub(crate) fn delete(repo: &Repository, shown: &Shown) -> Result<()> {
    expect(repo, shown)?;
    if !shown.one_side_deleted() {
        return Err(format!("Both sides have a version of {}. Take one of them instead.", named(&shown.path)));
    }
    checked(repo.command().args(["--literal-pathspecs", "rm", "-q", "--"]).arg(&shown.path))?;
    settled(repo, &shown.path)
}

pub(crate) fn mark_resolved(repo: &Repository, shown: &Shown, check: bool) -> Result<()> {
    expect(repo, shown)?;
    if shown.submodule() {
        return Err(format!("Take one side of the submodule {} instead.", named(&shown.path)));
    }
    if on_disk(repo, &shown.path)?.is_none() {
        return Err(format!("{} is not in the working tree. Take a side, or put the file back first.", named(&shown.path)));
    }
    if check && has_markers(repo, &shown.path)? {
        return Err(format!("{} still contains conflict markers.", named(&shown.path)));
    }
    add(repo, &shown.path)?;
    settled(repo, &shown.path)
}

pub(crate) fn mark_all_resolved(repo: &Repository, shown: &[Shown], check: bool) -> Result<()> {
    if shown.is_empty() {
        return Err("No conflicted file can be marked resolved as it is.".into());
    }
    for one in shown {
        expect(repo, one)?;
        if shown_cannot_be_marked(repo, one)? {
            return Err(format!("{} cannot be marked resolved as it is. Take a side for it instead.", named(&one.path)));
        }
        if check && has_markers(repo, &one.path)? {
            return Err(format!("{} still contains conflict markers.", named(&one.path)));
        }
    }
    for one in shown {
        add(repo, &one.path)?;
        settled(repo, &one.path)?;
    }
    Ok(())
}

fn shown_cannot_be_marked(repo: &Repository, shown: &Shown) -> Result<bool> {
    Ok(shown.submodule() || on_disk(repo, &shown.path)?.is_none())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Tooled {
    Resolved,
    Unchanged,
    Stopped(String),
    Ended,
}

pub(crate) fn merge_in_tool(repo: &Repository, shown: &Shown, call: &crate::difftool::Call) -> Result<Tooled> {
    expect(repo, shown)?;
    let scratch = call.own.then(tempfile::tempdir).transpose().map_err(|e| format!("Prepare the merge tool: {e}"))?;
    let apart = scratch.as_ref().map(|dir| dir.path().join("index"));
    let mut command = crate::difftool::command_with(repo, &call.settings);
    if let Some(index) = &apart {
        fs::copy(repo.git_dir().join("index"), index).map_err(|e| format!("Prepare the merge tool: {e}"))?;
        command.env("GIT_INDEX_FILE", index);
    }
    command.args(["mergetool", "--no-prompt", "--"]).arg(&shown.path);
    let output = crate::repository::run(&mut command).map_err(|e| format!("Run bundled Git: {e}"))?;
    let resolved = is_none(&entries_in(repo, apart.as_deref(), "--unmerged", &shown.path)?);
    let by_tool = apart.is_none() && resolved && shown.id.kind() == operation::Kind::Applied;
    if !by_tool && repo.expect_operation(&shown.id).is_err() {
        return Ok(Tooled::Ended);
    }
    if resolved {
        if apart.is_some() {
            add(repo, &shown.path)?;
            settled(repo, &shown.path)?;
        }
        return Ok(Tooled::Resolved);
    }
    Ok(match crate::difftool::told(&output) {
        Some(said) => Tooled::Stopped(said),
        None => Tooled::Unchanged,
    })
}

pub(crate) fn unresolve(repo: &Repository, id: &Id, path: &Path) -> Result<()> {
    validate_path(path)?;
    repo.expect_operation(id)?;
    if !is_none(&entries(repo, "--unmerged", path)?) {
        return Err(format!("{} is already in conflict.", named(path)));
    }
    if is_none(&entries(repo, "--resolve-undo", path)?) {
        return Err(format!("Git no longer has the conflicted versions of {}.", named(path)));
    }
    checked(repo.command().args(["update-index", "--unresolve", "--"]).arg(path))?;
    let now = entries(repo, "--unmerged", path)?;
    let regular = |stage: &Option<(String, String)>| {
        stage.as_ref().is_some_and(|(mode, _)| mode == "100644" || mode == "100755")
    };
    let restore = if regular(&now[1]) && regular(&now[2]) {
        Some("-m")
    } else if now.iter().flatten().any(|(mode, _)| mode == "160000") {
        None
    } else if now[1].is_some() {
        Some("--ours")
    } else if now[2].is_some() {
        Some("--theirs")
    } else if now[0].is_some() {
        None
    } else {
        return Err(format!("Git could not put {} back in conflict.", named(path)));
    };
    if let Some(flag) = restore {
        checked(repo.command().args(["--literal-pathspecs", "checkout", flag, "--"]).arg(path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{Shape, snapshot, tests::{commit, oid, stops, write}};
    use crate::repository::tests::Fixture;
    use std::os::unix::fs::symlink;

    fn shown(f: &Fixture, path: &str) -> Shown {
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        let conflict = snapshot.conflict(Path::new(path)).unwrap_or_else(|| panic!("{path} is not conflicted"));
        Shown::of(&snapshot, conflict)
    }

    fn stage(f: &Fixture, path: &str) -> String {
        String::from_utf8(f.git(&["ls-files", "--stage", "--", path])).unwrap()
    }

    fn unmerged(f: &Fixture, path: &str) -> String {
        String::from_utf8(f.git(&["ls-files", "--unmerged", "--", path])).unwrap()
    }

    fn blob(f: &Fixture, spec: &str) -> String {
        oid(f, spec)
    }

    fn merged() -> Fixture {
        let f = Fixture::new();
        write(&f, "gone-here.txt", "base\n");
        write(&f, "gone-there.txt", "base\n");
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0base").unwrap();
        f.git(&["add", "-A"]);
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        f.git(&["rm", "-q", "gone-here.txt"]);
        write(&f, "gone-there.txt", "changed there\n");
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0side").unwrap();
        f.git(&["add", "-A"]);
        commit(&f, "trail.txt", "side\n", "Side");
        f.git(&["switch", "-q", "main"]);
        write(&f, "gone-here.txt", "changed here\n");
        f.git(&["rm", "-q", "gone-there.txt"]);
        fs::write(f.dir.join("image.bin"), b"\x89PNG\0main").unwrap();
        f.git(&["add", "-A"]);
        commit(&f, "trail.txt", "main\n", "Main");
        stops(&f, &["merge", "side"]);
        f
    }

    #[test]
    fn marking_every_conflict_resolved_stages_them_all_or_none() {
        let f = Fixture::new();
        write(&f, "ridge.txt", "base\n");
        f.git(&["add", "--", "ridge.txt"]);
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        write(&f, "ridge.txt", "side\n");
        f.git(&["add", "--", "ridge.txt"]);
        commit(&f, "trail.txt", "side\n", "Side");
        f.git(&["switch", "-q", "main"]);
        write(&f, "ridge.txt", "main\n");
        f.git(&["add", "--", "ridge.txt"]);
        commit(&f, "trail.txt", "main\n", "Main");
        stops(&f, &["merge", "side"]);
        let both = [shown(&f, "ridge.txt"), shown(&f, "trail.txt")];
        write(&f, "ridge.txt", "settled\n");
        let err = mark_all_resolved(&f.repo, &both, true).unwrap_err();
        assert_eq!(err, "trail.txt still contains conflict markers.");
        assert!(!unmerged(&f, "ridge.txt").is_empty() && !unmerged(&f, "trail.txt").is_empty());

        write(&f, "trail.txt", "agreed\n");
        f.git(&["add", "--", "trail.txt"]);
        let err = mark_all_resolved(&f.repo, &both, true).unwrap_err();
        assert_eq!(err, "trail.txt is no longer in conflict.");
        assert!(!unmerged(&f, "ridge.txt").is_empty());

        let ridge = [shown(&f, "ridge.txt")];
        mark_all_resolved(&f.repo, &ridge, true).unwrap();
        assert!(unmerged(&f, "ridge.txt").is_empty() && unmerged(&f, "trail.txt").is_empty());
        assert_eq!(f.git(&["show", ":ridge.txt"]), b"settled\n");
        assert_eq!(f.git(&["show", ":trail.txt"]), b"agreed\n");
        assert!(mark_all_resolved(&f.repo, &[], true).is_err());
    }

    #[test]
    fn taking_a_side_of_a_binary_file_stages_that_sides_blob() {
        let f = merged();
        let image = shown(&f, "image.bin");
        take(&f.repo, &image, Side::Incoming).unwrap();
        assert_eq!(stage(&f, "image.bin"), format!("100644 {} 0\timage.bin\n", blob(&f, "side:image.bin")));
        assert_eq!(fs::read(f.dir.join("image.bin")).unwrap(), b"\x89PNG\0side");
        let after = snapshot(&f.repo).unwrap().unwrap();
        assert!(!after.has_conflict(Path::new("image.bin")));
        assert!(after.resolved.contains(&PathBuf::from("image.bin")));
        assert_eq!(take(&f.repo, &image, Side::Current).unwrap_err(), "image.bin is no longer in conflict.");

        let trail = shown(&f, "trail.txt");
        take(&f.repo, &trail, Side::Current).unwrap();
        assert_eq!(stage(&f, "trail.txt"), format!("100644 {} 0\ttrail.txt\n", blob(&f, "main:trail.txt")));
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "main\n");
    }

    #[test]
    fn a_take_refuses_a_file_or_operation_that_changed() {
        let f = merged();
        let image = shown(&f, "image.bin");
        let moved = Shown { stages: [None, image.stages[1].clone(), image.stages[2].clone()], ..image.clone() };
        assert!(take(&f.repo, &moved, Side::Current).unwrap_err().contains("changed since the window showed it"));
        f.git(&["add", "image.bin"]);
        assert_eq!(take(&f.repo, &image, Side::Current).unwrap_err(), "image.bin is no longer in conflict.");
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        unresolve(&f.repo, &id, Path::new("image.bin")).unwrap();
        f.git(&["merge", "--abort"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Later"]);
        stops(&f, &["merge", "side"]);
        let err = take(&f.repo, &image, Side::Current).unwrap_err();
        assert!(err.contains("The operation in progress changed"), "{err}");
        assert!(!unmerged(&f, "image.bin").is_empty());
        let outside = Shown { path: PathBuf::from("../image.bin"), ..shown(&f, "image.bin") };
        assert!(take(&f.repo, &outside, Side::Current).is_err());
    }

    #[test]
    fn a_file_deleted_on_one_side_is_kept_or_deleted() {
        let f = merged();
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(snapshot.conflict(Path::new("gone-here.txt")).unwrap().shape, Shape::DeletedByThem);
        assert_eq!(snapshot.conflict(Path::new("gone-there.txt")).unwrap().shape, Shape::DeletedByUs);
        let here = shown(&f, "gone-here.txt");
        assert!(take(&f.repo, &here, Side::Incoming).unwrap_err().contains("Delete the file instead"));
        delete(&f.repo, &here).unwrap();
        assert_eq!(stage(&f, "gone-here.txt"), "");
        assert!(!f.dir.join("gone-here.txt").exists());

        let there = shown(&f, "gone-there.txt");
        keep(&f.repo, &there).unwrap();
        assert_eq!(stage(&f, "gone-there.txt"), format!("100644 {} 0\tgone-there.txt\n", blob(&f, "side:gone-there.txt")));
        assert_eq!(fs::read_to_string(f.dir.join("gone-there.txt")).unwrap(), "changed there\n");

        let image = shown(&f, "image.bin");
        assert!(delete(&f.repo, &image).unwrap_err().contains("Take one of them instead"));
        assert!(keep(&f.repo, &image).is_err());
        assert!(!unmerged(&f, "image.bin").is_empty());
    }

    #[test]
    fn keeping_a_changed_file_that_is_gone_is_refused_rather_than_deleting_it() {
        let f = merged();
        let there = shown(&f, "gone-there.txt");
        fs::remove_file(f.dir.join("gone-there.txt")).unwrap();
        assert!(keep(&f.repo, &there).unwrap_err().contains("no longer in the working tree"));
        assert!(!unmerged(&f, "gone-there.txt").is_empty());
        assert!(mark_resolved(&f.repo, &shown(&f, "gone-there.txt"), true).unwrap_err().contains("not in the working tree"));
    }

    #[test]
    fn marking_resolved_stages_the_working_tree_and_checks_for_markers() {
        let f = merged();
        assert!(has_markers(&f.repo, Path::new("trail.txt")).unwrap());
        let trail = shown(&f, "trail.txt");
        let err = mark_resolved(&f.repo, &trail, true).unwrap_err();
        assert_eq!(err, "trail.txt still contains conflict markers.");
        assert!(!unmerged(&f, "trail.txt").is_empty());
        write(&f, "trail.txt", "main and side\n");
        assert!(!has_markers(&f.repo, Path::new("trail.txt")).unwrap());
        mark_resolved(&f.repo, &trail, true).unwrap();
        assert_eq!(f.git(&["show", ":trail.txt"]), b"main and side\n");
        assert!(unmerged(&f, "trail.txt").is_empty());
        let f = merged();
        let trail = shown(&f, "trail.txt");
        mark_resolved(&f.repo, &trail, false).unwrap();
        assert!(String::from_utf8(f.git(&["show", ":trail.txt"])).unwrap().starts_with("<<<<<<< HEAD\n"));
        let image = shown(&f, "image.bin");
        mark_resolved(&f.repo, &image, true).unwrap();
        assert_eq!(f.git(&["show", ":image.bin"]), b"\x89PNG\0main");
    }

    #[test]
    fn markers_are_found_at_the_files_marker_width_and_nowhere_else() {
        let found = |text: &str, width| scan(io::Cursor::new(text.as_bytes()), width).unwrap();
        assert!(found("a\n<<<<<<< HEAD\nb\n", 7));
        assert!(found("a\n=======\n", 7));
        assert!(found("a\r\n=======\r\n", 7));
        assert!(found("a\n>>>>>>> side", 7));
        assert!(found("||||||| base\n", 7));
        assert!(!found("========\n", 7));
        assert!(!found("a <<<<<<< b\n", 7));
        assert!(!found("<<<<<<<<\n", 7));
        assert!(!found("<<<<<<< HEAD\n", 9));
        assert!(found("<<<<<<<<< HEAD\n", 9));
        let long = format!("{}\n=======\n", "x".repeat(200_000));
        assert!(found(&long, 7));
        assert!(!found(&format!("=======x{}\n", "y".repeat(100_000)), 7));
    }

    #[test]
    fn a_file_marked_conflicted_again_gets_its_stages_and_markers_back() {
        let f = merged();
        let before = unmerged(&f, "trail.txt");
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        assert!(unresolve(&f.repo, &id, Path::new("trail.txt")).unwrap_err().contains("already in conflict"));
        take(&f.repo, &shown(&f, "trail.txt"), Side::Incoming).unwrap();
        unresolve(&f.repo, &id, Path::new("trail.txt")).unwrap();
        assert_eq!(unmerged(&f, "trail.txt"), before);
        let again = fs::read_to_string(f.dir.join("trail.txt")).unwrap();
        assert_eq!(again, "<<<<<<< ours\nmain\n=======\nside\n>>>>>>> theirs\n");
        assert!(snapshot(&f.repo).unwrap().unwrap().has_conflict(Path::new("trail.txt")));

        let image = unmerged(&f, "image.bin");
        take(&f.repo, &shown(&f, "image.bin"), Side::Incoming).unwrap();
        unresolve(&f.repo, &id, Path::new("image.bin")).unwrap();
        assert_eq!(unmerged(&f, "image.bin"), image);
        assert_eq!(fs::read(f.dir.join("image.bin")).unwrap(), b"\x89PNG\0main");

        let here = unmerged(&f, "gone-here.txt");
        delete(&f.repo, &shown(&f, "gone-here.txt")).unwrap();
        unresolve(&f.repo, &id, Path::new("gone-here.txt")).unwrap();
        assert_eq!(unmerged(&f, "gone-here.txt"), here);
        assert_eq!(fs::read_to_string(f.dir.join("gone-here.txt")).unwrap(), "changed here\n");
        let there = unmerged(&f, "gone-there.txt");
        keep(&f.repo, &shown(&f, "gone-there.txt")).unwrap();
        fs::write(f.dir.join("gone-there.txt"), "edited after\n").unwrap();
        unresolve(&f.repo, &id, Path::new("gone-there.txt")).unwrap();
        assert_eq!(unmerged(&f, "gone-there.txt"), there);
        assert_eq!(fs::read_to_string(f.dir.join("gone-there.txt")).unwrap(), "changed there\n");
    }

    #[test]
    fn conflicted_again_refuses_another_operation_and_a_path_never_conflicted() {
        let f = merged();
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        take(&f.repo, &shown(&f, "trail.txt"), Side::Current).unwrap();
        let err = unresolve(&f.repo, &id, Path::new("unrelated.txt")).unwrap_err();
        assert!(err.contains("no longer has the conflicted versions"), "{err}");
        f.git(&["merge", "--abort"]);
        let err = unresolve(&f.repo, &id, Path::new("trail.txt")).unwrap_err();
        assert!(err.contains("The operation in progress changed"), "{err}");
        assert!(unmerged(&f, "trail.txt").is_empty());
    }

    #[test]
    fn a_symlink_conflict_takes_a_side_and_comes_back_without_markers() {
        let f = Fixture::new();
        symlink("base", f.dir.join("link")).unwrap();
        f.git(&["add", "link"]);
        f.git(&["commit", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        fs::remove_file(f.dir.join("link")).unwrap();
        symlink("side", f.dir.join("link")).unwrap();
        f.git(&["commit", "-qam", "Side"]);
        f.git(&["switch", "-q", "main"]);
        fs::remove_file(f.dir.join("link")).unwrap();
        symlink("main", f.dir.join("link")).unwrap();
        f.git(&["commit", "-qam", "Main"]);
        stops(&f, &["merge", "side"]);
        let link = shown(&f, "link");
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().conflict(Path::new("link")).unwrap().shape, Shape::Symlink);
        let before = unmerged(&f, "link");
        take(&f.repo, &link, Side::Incoming).unwrap();
        assert_eq!(fs::read_link(f.dir.join("link")).unwrap(), PathBuf::from("side"));
        assert!(stage(&f, "link").starts_with("120000 "));
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        unresolve(&f.repo, &id, Path::new("link")).unwrap();
        assert_eq!(unmerged(&f, "link"), before);
        assert_eq!(fs::read_link(f.dir.join("link")).unwrap(), PathBuf::from("main"));
    }

    #[test]
    fn a_submodule_conflict_records_the_chosen_commit() {
        let f = Fixture::new();
        f.git(&["commit", "-q", "--allow-empty", "-m", "One"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Two"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Three"]);
        let (one, two, three) = (oid(&f, "HEAD~2"), oid(&f, "HEAD~1"), oid(&f, "HEAD"));
        f.git(&["switch", "-q", "--orphan", "work"]);
        f.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{one},module")]);
        f.git(&["commit", "-qm", "Add the module"]);
        f.git(&["switch", "-qc", "side"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{two},module")]);
        f.git(&["commit", "-qm", "Side"]);
        f.git(&["switch", "-q", "work"]);
        f.git(&["update-index", "--cacheinfo", &format!("160000,{three},module")]);
        f.git(&["commit", "-qm", "Work"]);
        stops(&f, &["merge", "side"]);
        let module = shown(&f, "module");
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().conflict(Path::new("module")).unwrap().shape, Shape::Submodule);
        assert!(mark_resolved(&f.repo, &module, true).unwrap_err().contains("Take one side"));
        let before = unmerged(&f, "module");
        take(&f.repo, &module, Side::Incoming).unwrap();
        assert_eq!(stage(&f, "module"), format!("160000 {two} 0\tmodule\n"));
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        unresolve(&f.repo, &id, Path::new("module")).unwrap();
        assert_eq!(unmerged(&f, "module"), before);
        take(&f.repo, &shown(&f, "module"), Side::Current).unwrap();
        assert_eq!(stage(&f, "module"), format!("160000 {three} 0\tmodule\n"));
    }

    #[test]
    fn every_worker_refuses_a_file_resolved_elsewhere_or_another_operation() {
        let f = merged();
        let (trail, image, here, there) =
            (shown(&f, "trail.txt"), shown(&f, "image.bin"), shown(&f, "gone-here.txt"), shown(&f, "gone-there.txt"));
        f.git(&["add", "trail.txt", "image.bin"]);
        f.git(&["rm", "-q", "gone-here.txt"]);
        f.git(&["add", "gone-there.txt"]);
        let gone = |err: String, name: &str| assert_eq!(err, format!("{name} is no longer in conflict."));
        gone(mark_resolved(&f.repo, &trail, false).unwrap_err(), "trail.txt");
        gone(take(&f.repo, &image, Side::Current).unwrap_err(), "image.bin");
        gone(delete(&f.repo, &here).unwrap_err(), "gone-here.txt");
        gone(keep(&f.repo, &there).unwrap_err(), "gone-there.txt");
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        for path in ["trail.txt", "image.bin", "gone-here.txt", "gone-there.txt"] {
            unresolve(&f.repo, &id, Path::new(path)).unwrap();
        }
        f.git(&["merge", "--abort"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Later"]);
        stops(&f, &["merge", "side"]);
        let index = String::from_utf8(f.git(&["ls-files", "--stage"])).unwrap();
        for err in [
            mark_resolved(&f.repo, &trail, false).unwrap_err(),
            take(&f.repo, &image, Side::Incoming).unwrap_err(),
            delete(&f.repo, &here).unwrap_err(),
            keep(&f.repo, &there).unwrap_err(),
        ] {
            assert!(err.contains("The operation in progress changed"), "{err}");
        }
        assert_eq!(String::from_utf8(f.git(&["ls-files", "--stage"])).unwrap(), index);
    }

    #[test]
    fn files_added_on_both_sides_or_over_the_resolvers_limit_take_a_side() {
        let f = Fixture::new();
        let big = |word: &str| format!("{word}\n").repeat(300_000);
        commit(&f, "large.txt", &big("base"), "Base");
        f.git(&["switch", "-qc", "side"]);
        write(&f, "both.txt", "added on side\n");
        f.git(&["add", "both.txt"]);
        commit(&f, "large.txt", &big("side"), "Side");
        f.git(&["switch", "-q", "main"]);
        write(&f, "both.txt", "added on main\n");
        f.git(&["add", "both.txt"]);
        commit(&f, "large.txt", &big("main"), "Main");
        stops(&f, &["merge", "side"]);
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(snapshot.conflict(Path::new("both.txt")).unwrap().shape, Shape::BothAdded);
        assert_eq!(snapshot.conflict(Path::new("large.txt")).unwrap().shape, Shape::TooLarge);
        assert!(has_markers(&f.repo, Path::new("large.txt")).unwrap());
        take(&f.repo, &shown(&f, "large.txt"), Side::Incoming).unwrap();
        assert_eq!(f.git(&["show", ":large.txt"]), big("side").into_bytes());
        take(&f.repo, &shown(&f, "both.txt"), Side::Current).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("both.txt")).unwrap(), "added on main\n");
        assert_eq!(unmerged(&f, "."), "");
    }

    #[test]
    fn a_file_both_sides_renamed_away_is_deleted_and_comes_back() {
        let f = Fixture::new();
        commit(&f, "trail.txt", &(1..=50).map(|i| format!("{i}\n")).collect::<String>(), "Base");
        f.git(&["switch", "-qc", "side"]);
        f.git(&["mv", "trail.txt", "ridge.txt"]);
        f.git(&["commit", "-qm", "Side"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["mv", "trail.txt", "river.txt"]);
        f.git(&["commit", "-qm", "Main"]);
        stops(&f, &["merge", "side"]);
        let trail = shown(&f, "trail.txt");
        assert!(trail.stages[0].is_some() && trail.stages[1].is_none() && trail.stages[2].is_none());
        let before = unmerged(&f, "trail.txt");
        assert!(keep(&f.repo, &trail).unwrap_err().contains("no longer in the working tree"));
        delete(&f.repo, &trail).unwrap();
        assert_eq!(unmerged(&f, "trail.txt"), "");
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        unresolve(&f.repo, &id, Path::new("trail.txt")).unwrap();
        assert_eq!(unmerged(&f, "trail.txt"), before);
        assert!(!f.dir.join("trail.txt").exists());
    }

    #[test]
    fn a_rebase_takes_the_side_its_label_names() {
        let f = crate::operation::tests::diverged();
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "main"]);
        let snapshot = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((snapshot.ours_name(), snapshot.theirs_name().len()), ("main".to_owned(), 7));
        take(&f.repo, &shown(&f, "trail.txt"), Side::Incoming).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "feature side\n");
    }
}
