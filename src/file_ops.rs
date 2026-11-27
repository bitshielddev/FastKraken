use crate::{
    Tab,
    commands::{self, Availability, Side, Target},
    notice::{Notice, Outcome},
    prompt::{self, Answer, Field, Spec},
    repository::{Repository, checked, validate_path},
    view::{self, Desk, FileRow, Tone, Ui},
};
use gpui::{AnyWindowHandle, Context, Window};
use std::{
    ffi::OsStr,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

const LONGEST_NAME: usize = 255;
const MOST_COPIES: usize = 999;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    New(PathBuf),
    Rename(PathBuf),
    Trash(PathBuf),
    PutBack { trashed: PathBuf, path: PathBuf },
}

impl Action {
    pub(crate) fn during_operation(&self) -> bool {
        !matches!(self, Action::Rename(_))
    }
}

#[derive(Debug)]
pub(crate) enum Ask {
    New { folder: PathBuf },
    Rename { path: PathBuf, tracked: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Bin {
    System,
    Folder(PathBuf),
}

impl Bin {
    pub(crate) fn current() -> Result<Bin, String> {
        if let Some(sandbox) = crate::acceptance::sandbox() {
            return Ok(Bin::Folder(sandbox.join("Trash")));
        }
        if cfg!(test) || crate::acceptance::replaying() {
            return Err("No stand-in Trash is set up, so nothing was moved.".to_owned());
        }
        Ok(Bin::System)
    }

    fn take(&self, full: &Path) -> Result<PathBuf, String> {
        match self {
            Bin::System => system::trash(full),
            Bin::Folder(folder) => {
                fs::create_dir_all(folder).map_err(|err| format!("Make the Trash folder: {err}"))?;
                let name = full.file_name().ok_or("The file has no name")?;
                let place = free_place(folder, name).ok_or("The Trash already holds too many files of that name")?;
                fs::rename(full, &place).map_err(|err| format!("Move to the Trash: {err}"))?;
                Ok(place)
            }
        }
    }
}

fn free_place(folder: &Path, name: &OsStr) -> Option<PathBuf> {
    let first = folder.join(name);
    if first.symlink_metadata().is_err() {
        return Some(first);
    }
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or(name).to_string_lossy();
    let extension = path.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    (2..=MOST_COPIES).map(|n| folder.join(format!("{stem} {n}{extension}"))).find(|place| place.symlink_metadata().is_err())
}

fn shown_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

fn folder_words(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        "the top folder of the repository".to_owned()
    } else {
        format!("{}/", folder.display())
    }
}

pub(crate) fn name_problem(name: &str, nested: bool) -> Option<String> {
    if name.trim().is_empty() {
        return Some(String::new());
    }
    if name.chars().any(char::is_control) {
        return Some("A name cannot hold a line break or other control character.".to_owned());
    }
    if !nested && name.contains('/') {
        return Some("A name cannot hold a /.".to_owned());
    }
    for part in name.split('/') {
        if part.is_empty() {
            return Some("A folder name between the slashes is missing.".to_owned());
        }
        if part != part.trim() {
            return Some("A name cannot start or end with a space.".to_owned());
        }
        if part == "." || part == ".." {
            return Some(format!("{part} cannot be used as a name."));
        }
        if part.eq_ignore_ascii_case(".git") {
            return Some(".git belongs to Git and cannot be used as a name.".to_owned());
        }
        if part.len() > LONGEST_NAME {
            return Some(format!("A name can be at most {LONGEST_NAME} bytes long."));
        }
    }
    None
}

fn same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (left.symlink_metadata(), right.symlink_metadata()) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

fn taken(root: &Path, path: &Path, by: Option<&Path>) -> bool {
    let full = root.join(path);
    full.symlink_metadata().is_ok() && !by.is_some_and(|source| same_file(&root.join(source), &full))
}

fn renamed(path: &Path, name: &str) -> PathBuf {
    path.parent().unwrap_or(Path::new("")).join(name)
}

pub(crate) fn new_problem(ui: &Ui, folder: &Path, name: &str) -> Option<String> {
    name_problem(name, true).or_else(|| {
        let path = folder.join(name);
        taken(&ui.root, &path, None).then(|| format!("{} is already there.", path.display()))
    })
}

pub(crate) fn rename_problem(ui: &Ui, path: &Path, name: &str) -> Option<String> {
    if path.file_name() == Some(OsStr::new(name)) {
        return Some(String::new());
    }
    name_problem(name, false).or_else(|| {
        let target = renamed(path, name);
        taken(&ui.root, &target, Some(path)).then(|| format!("{} is already there.", target.display()))
    })
}

fn through_links(root: &Path, path: &Path) -> Result<(), String> {
    let mut at = PathBuf::new();
    let parts: Vec<Component> = path.components().collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match root.join(&at).symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{} is a link, so FastKraken does not create or move files through it.", at.display()));
            }
            Ok(meta) if !meta.is_dir() => return Err(format!("{} is a file, not a folder.", at.display())),
            _ => {}
        }
    }
    Ok(())
}

fn exclusive_rename(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        let text = |path: &Path| CString::new(path.as_os_str().as_bytes()).map_err(|_| "A path holds a NUL byte".to_owned());
        let (source, target) = (text(from)?, text(to)?);
        let flags = if same_file(from, to) { 0 } else { libc::RENAME_EXCL };
        if unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), flags) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(format!("{} is already there.", shown_name(to)));
        }
        Err(format!("Rename {}: {err}", shown_name(from)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if to.symlink_metadata().is_ok() && !same_file(from, to) {
            return Err(format!("{} is already there.", shown_name(to)));
        }
        fs::rename(from, to).map_err(|err| format!("Rename {}: {err}", shown_name(from)))
    }
}

impl Repository {
    fn tracked(&self, path: &Path) -> Result<bool, String> {
        let listed = checked(self.read_command().args(["ls-files", "-z", "--"]).arg(path))?;
        let mut wanted = path.as_os_str().as_bytes().to_vec();
        wanted.push(0);
        Ok(listed.split_inclusive(|&b| b == 0).any(|entry| entry == wanted.as_slice()))
    }

    fn still_tracked(&self, path: &Path, tracked: bool, verb: &str) -> Result<(), String> {
        let now = self.tracked(path)?;
        if now != tracked {
            return Err(format!(
                "{} is {} since you chose {verb}. Look at it again first.",
                shown_name(path),
                if now { "now tracked" } else { "no longer tracked" },
            ));
        }
        Ok(())
    }

    fn ignored(&self, path: &Path) -> bool {
        let mut check = self.command();
        check.args(["--no-optional-locks", "check-ignore", "-q", "--no-index", "--"]).arg(path);
        crate::repository::run(&mut check).is_ok_and(|output| output.status.success())
    }

    pub(crate) fn create_file(&self, folder: &Path, name: &str) -> Result<Outcome, String> {
        if let Some(problem) = name_problem(name, true) {
            return Err(if problem.is_empty() { "Type a name for the file.".to_owned() } else { problem });
        }
        let path = folder.join(name);
        validate_path(&path)?;
        through_links(&self.root, &path)?;
        let full = self.root.join(&path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("Make the folder for {}: {err}", shown_name(&path)))?;
        }
        fs::OpenOptions::new().write(true).create_new(true).open(&full).map_err(|err| match err.kind() {
            std::io::ErrorKind::AlreadyExists => format!("{} is already there.", path.display()),
            _ => format!("Create {}: {err}", shown_name(&path)),
        })?;
        let detail = if self.ignored(&path) {
            format!("It is in {}, and a .gitignore rule keeps it out of the list of changes.", folder_words(path.parent().unwrap_or(Path::new(""))))
        } else {
            format!("It is in {} and shows as a new file.", folder_words(path.parent().unwrap_or(Path::new(""))))
        };
        Ok(Outcome::titled(format!("Created {}", shown_name(&path)), detail))
    }

    pub(crate) fn rename_file(&self, path: &Path, name: &str, tracked: bool) -> Result<Outcome, String> {
        validate_path(path)?;
        if let Some(problem) = name_problem(name, false) {
            return Err(if problem.is_empty() { "Type the new name.".to_owned() } else { problem });
        }
        let target = renamed(path, name);
        validate_path(&target)?;
        through_links(&self.root, path)?;
        let (from, to) = (self.root.join(path), self.root.join(&target));
        if from.symlink_metadata().is_err() {
            return Err(format!("{} is no longer in the working tree.", shown_name(path)));
        }
        if taken(&self.root, &target, Some(path)) {
            return Err(format!("{} is already there. Choose another name.", target.display()));
        }
        self.still_tracked(path, tracked, "Rename")?;
        if tracked {
            self.ensure_index_writable()?;
            checked(self.command().args(["mv", "--"]).arg(path).arg(&target))?;
            Ok(Outcome::titled(
                format!("Renamed {} to {name}", shown_name(path)),
                "Git moved it with git mv, so the rename is staged.",
            ))
        } else {
            exclusive_rename(&from, &to)?;
            Ok(Outcome::titled(format!("Renamed {} to {name}", shown_name(path)), "It was not tracked, so Git has nothing to record."))
        }
    }

    pub(crate) fn trash_file(&self, path: &Path, tracked: bool, bin: &Bin) -> Result<(Outcome, PathBuf), String> {
        validate_path(path)?;
        through_links(&self.root, path)?;
        let full = self.root.join(path);
        match full.symlink_metadata() {
            Err(_) => return Err(format!("{} is no longer in the working tree.", shown_name(path))),
            Ok(meta) if meta.is_dir() => return Err(format!("{} is now a folder, not a file.", path.display())),
            Ok(_) => {}
        }
        self.still_tracked(path, tracked, "Move to Trash")?;
        let trashed = bin.take(&full)?;
        let detail = if tracked {
            "Git now shows it as deleted. Commit the deletion, or put the file back."
        } else {
            "It was not tracked, so Git has nothing to record."
        };
        Ok((Outcome::titled(format!("Moved {} to the Trash", shown_name(path)), detail), trashed))
    }

    pub(crate) fn put_back_from_trash(&self, trashed: &Path, path: &Path) -> Result<Outcome, String> {
        validate_path(path)?;
        through_links(&self.root, path)?;
        if trashed.symlink_metadata().is_err() {
            return Err(format!("{} is no longer in the Trash.", shown_name(path)));
        }
        let full = self.root.join(path);
        if full.symlink_metadata().is_ok() {
            return Err(format!("{} is already there again, so the copy in the Trash stays where it is.", path.display()));
        }
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("Make the folder for {}: {err}", shown_name(path)))?;
        }
        exclusive_rename(trashed, &full)?;
        Ok(Outcome::titled(format!("Put back {}", shown_name(path)), format!("It is in {} again.", folder_words(path.parent().unwrap_or(Path::new(""))))))
    }
}

fn listed<'a>(ui: &'a Ui, target: &'a Target) -> Option<(&'a PathBuf, Option<&'a FileRow>)> {
    let Target::File { path, side: side @ (Side::Unstaged | Side::Staged) } = target else { return None };
    let staged = *side == Side::Staged;
    Some((path, ui.files.iter().find(|row| row.path == *path && row.staged == staged)))
}

fn on_disk(ui: &Ui, path: &Path) -> Option<String> {
    crate::handoff::locate(&ui.root, path).err()
}

fn file_offered(ui: &Ui, target: &Target, refusal: impl Fn(&Ui) -> Option<String>) -> Availability {
    match listed(ui, target) {
        Some((path, row)) if !row.is_some_and(|row| row.submodule.is_some()) && !ui.root.as_os_str().is_empty() => {
            Availability::unless(refusal(ui).or_else(|| on_disk(ui, path)))
        }
        _ => Availability::Hidden,
    }
}

fn index_edit(ui: &Ui) -> Option<String> {
    commands::busy(ui).or_else(|| ui.operation.refusal())
}

fn folder_of(target: &Target) -> Option<PathBuf> {
    match target {
        Target::File { path, side: Side::Unstaged | Side::Staged } => Some(path.parent().unwrap_or(Path::new("")).to_path_buf()),
        Target::Folder { path, side: Side::Unstaged | Side::Staged } => Some(path.clone()),
        _ => None,
    }
}

fn new_offered(ui: &Ui, target: &Target) -> Availability {
    if folder_of(target).is_none() || ui.root.as_os_str().is_empty() {
        return Availability::Hidden;
    }
    Availability::unless(commands::busy(ui))
}

pub(crate) const COMMANDS: &[commands::Command] = {
    use crate::commands::{Command, Group};
    &[
        Command::new(
            "file.new",
            Group::Manage,
            |_, _| "New file…".into(),
            |ui, target, _| if matches!(target, Target::File { .. }) { new_offered(ui, target) } else { Availability::Hidden },
            |_, target| folder_of(target).map(|folder| view::Action::Files(Action::New(folder))),
        )
        .icon("file")
        .words(&["create file", "add file", "touch"]),
        Command::new(
            "file.rename",
            Group::Manage,
            |_, _| "Rename…".into(),
            |ui, target, _| file_offered(ui, target, index_edit),
            |ui, target| listed(ui, target).map(|(path, _)| view::Action::Files(Action::Rename(path.clone()))),
        )
        .icon("pencil")
        .words(&["move file", "git mv", "change name"]),
        Command::new(
            "file.trash",
            Group::Manage,
            |_, _| "Move to Trash".into(),
            |ui, target, _| file_offered(ui, target, commands::busy),
            |ui, target| listed(ui, target).map(|(path, _)| view::Action::Files(Action::Trash(path.clone()))),
        )
        .icon("trash")
        .words(&["delete file", "remove file", "bin"]),
        Command::new(
            "folder.new-file",
            Group::Manage,
            |_, _| "New file…".into(),
            |ui, target, _| if matches!(target, Target::Folder { .. }) { new_offered(ui, target) } else { Availability::Hidden },
            |_, target| folder_of(target).map(|folder| view::Action::Files(Action::New(folder))),
        )
        .icon("file")
        .words(&["create file", "add file", "touch"]),
    ]
};

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::New { folder } => {
            let mut spec = Spec::new("New file", "Create")
                .line(format!("Creates an empty file in {} and opens it in your editor.", folder_words(folder)));
            let folder = folder.clone();
            spec.field = Some(Field {
                label: "File name",
                placeholder: "notes.txt",
                initial: String::new(),
                masked: false,
                validate: Box::new(move |name, ui| new_problem(ui, &folder, name)),
            });
            spec
        }
        Ask::Rename { path, tracked } => {
            let mut spec = Spec::new(format!("Rename {}", shown_name(path)), "Rename").line(if *tracked {
                "Git renames the file with git mv, so the rename is staged."
            } else {
                "The file is not tracked, so it is renamed on disk only."
            });
            let source = path.clone();
            spec.field = Some(Field {
                label: "New name",
                placeholder: "Name",
                initial: shown_name(path),
                masked: false,
                validate: Box::new(move |name, ui| rename_problem(ui, &source, name)),
            });
            spec
        }
    }
}

fn tracked_row(ui: &Ui, path: &Path) -> Option<bool> {
    let rows: Vec<&FileRow> = ui.files.iter().filter(|row| row.path == path).collect();
    (!rows.is_empty()).then(|| rows.iter().any(|row| row.staged || row.status != "Untracked"))
}

fn refuse(tab: &mut Tab, reason: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() || tab.ui.prompt.open() {
        return;
    }
    match action {
        Action::New(folder) => {
            if validate_path(&folder.join("x")).is_err() {
                return;
            }
            prompt::ask(&mut tab.ui, prompt::Kind::Files(Ask::New { folder }), cx);
        }
        Action::Rename(path) => {
            let Some(tracked) = tracked_row(&tab.ui, &path) else {
                return refuse(tab, format!("{} is no longer in the list of changes", shown_name(&path)), cx);
            };
            prompt::ask(&mut tab.ui, prompt::Kind::Files(Ask::Rename { path, tracked }), cx);
        }
        Action::Trash(path) => {
            let Some(tracked) = tracked_row(&tab.ui, &path) else {
                return refuse(tab, format!("{} is no longer in the list of changes", shown_name(&path)), cx);
            };
            let bin = match Bin::current() {
                Ok(bin) => bin,
                Err(reason) => return refuse(tab, reason, cx),
            };
            let shown = path.clone();
            tab.apply_operation(
                cx,
                "Moving the file to the Trash…",
                Some("Moved to the Trash"),
                false,
                move |repo| {
                    let (outcome, trashed) = repo.trash_file(&shown, tracked, &bin)?;
                    Ok::<_, String>(outcome.offer("Put back", view::Action::Files(Action::PutBack { trashed, path: shown })))
                },
                |_, _| {},
            );
        }
        Action::PutBack { trashed, path } => tab.apply_operation(
            cx,
            "Putting the file back…",
            Some("Put back"),
            false,
            move |repo| repo.put_back_from_trash(&trashed, &path),
            |_, _| {},
        ),
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, window: &mut Window, cx: &mut Context<Tab>) {
    match ask {
        Ask::New { folder } => {
            let name = answer.text;
            let path = folder.join(&name);
            let handle = window.window_handle();
            tab.apply_operation(
                cx,
                "Creating the file…",
                Some("Created"),
                false,
                move |repo| repo.create_file(&folder, &name),
                move |_, cx| {
                    let edit = view::Action::Handoff(crate::handoff::Action::Edit { path, line: None });
                    later(handle, edit, cx);
                },
            );
        }
        Ask::Rename { path, tracked } => {
            let name = answer.text;
            tab.apply_operation(
                cx,
                "Renaming the file…",
                Some("Renamed"),
                false,
                move |repo| repo.rename_file(&path, &name, tracked),
                |_, _| {},
            );
        }
    }
}

fn later(handle: AnyWindowHandle, action: view::Action, cx: &mut Context<Tab>) {
    let tab = cx.entity();
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| tab.update(cx, |tab, cx| tab.dispatch(action, window, cx)));
    });
}

#[cfg(target_os = "macos")]
mod system {
    use std::{
        ffi::{CStr, c_char, c_void},
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    type Object = *mut c_void;

    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Object;
        fn sel_registerName(name: *const c_char) -> Object;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    unsafe fn selector(name: &CStr) -> Object {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    unsafe fn send(receiver: Object, name: &CStr) -> Object {
        unsafe {
            let send: unsafe extern "C" fn(Object, Object) -> Object = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector(name))
        }
    }

    unsafe fn text_of(string: Object) -> Option<String> {
        unsafe {
            if string.is_null() {
                return None;
            }
            let utf8: unsafe extern "C" fn(Object, Object) -> *const c_char = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let bytes = utf8(string, selector(c"UTF8String"));
            (!bytes.is_null()).then(|| CStr::from_ptr(bytes).to_string_lossy().into_owned())
        }
    }

    pub(super) fn trash(full: &Path) -> Result<PathBuf, String> {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let result = trash_in_pool(full);
            objc_autoreleasePoolPop(pool);
            result
        }
    }

    unsafe fn trash_in_pool(full: &Path) -> Result<PathBuf, String> {
        unsafe {
            let class = objc_getClass(c"NSFileManager".as_ptr());
            let url_class = objc_getClass(c"NSURL".as_ptr());
            if class.is_null() || url_class.is_null() {
                return Err("The Trash cannot be reached on this Mac.".to_owned());
            }
            let manager = send(class, c"defaultManager");
            if manager.is_null() {
                return Err("The Trash cannot be reached on this Mac.".to_owned());
            }
            let bytes = full.as_os_str().as_bytes();
            let named: unsafe extern "C" fn(Object, Object, *const c_char, usize) -> Object =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let string = named(manager, selector(c"stringWithFileSystemRepresentation:length:"), bytes.as_ptr().cast(), bytes.len());
            if string.is_null() {
                return Err(format!("{} has a name the Trash cannot take.", full.display()));
            }
            let located: unsafe extern "C" fn(Object, Object, Object) -> Object = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let url = located(url_class, selector(c"fileURLWithPath:"), string);
            if url.is_null() {
                return Err(format!("{} has a name the Trash cannot take.", full.display()));
            }
            let mut landed: Object = std::ptr::null_mut();
            let mut error: Object = std::ptr::null_mut();
            let trash: unsafe extern "C" fn(Object, Object, Object, *mut Object, *mut Object) -> bool =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            if !trash(manager, selector(c"trashItemAtURL:resultingItemURL:error:"), url, &mut landed, &mut error) {
                let reason = if error.is_null() { None } else { text_of(send(error, c"localizedDescription")) };
                return Err(reason.unwrap_or_else(|| "macOS did not move it to the Trash.".to_owned()));
            }
            if landed.is_null() {
                return Err("macOS moved it to the Trash but did not say where.".to_owned());
            }
            let representation: unsafe extern "C" fn(Object, Object) -> *const c_char =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let place = representation(landed, selector(c"fileSystemRepresentation"));
            if place.is_null() {
                return Err("macOS moved it to the Trash but did not say where.".to_owned());
            }
            Ok(PathBuf::from(std::ffi::OsStr::from_bytes(CStr::from_ptr(place).to_bytes())))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod system {
    use std::path::{Path, PathBuf};

    pub(super) fn trash(_: &Path) -> Result<PathBuf, String> {
        Err("Moving to the Trash needs macOS.".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    const TRAIL: &str = "FastKraken: git for the impatient\n";

    fn committed() -> Fixture {
        let f = Fixture::new();
        fs::create_dir(f.dir.join("docs")).unwrap();
        fs::write(f.dir.join("docs/trail.txt"), TRAIL).unwrap();
        f.git(&["add", "docs/trail.txt"]);
        f.git(&["commit", "-qm", "Mark the trail"]);
        f
    }

    fn status(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "--untracked-files=all"])).unwrap()
    }

    fn bin() -> (Bin, PathBuf, tempfile::TempDir) {
        let held = tempfile::tempdir().unwrap();
        let folder = held.path().join("Trash");
        (Bin::Folder(folder.clone()), folder, held)
    }

    #[test]
    fn names_are_checked_before_anything_is_made() {
        assert_eq!(name_problem("", true), Some(String::new()));
        assert_eq!(name_problem("  ", false), Some(String::new()));
        assert!(name_problem("notes.txt", false).is_none());
        assert!(name_problem("docs/notes.md", true).is_none());
        for bad in ["a/b", "line\nbreak", " lead", "trail ", "..", ".git"] {
            assert!(name_problem(bad, false).is_some_and(|problem| !problem.is_empty()), "{bad}");
        }
        for bad in ["a//b", "../x", "x/.git/hooks", ".GIT/x", "/abs"] {
            assert!(name_problem(bad, true).is_some_and(|problem| !problem.is_empty()), "{bad}");
        }
        assert!(name_problem(&"n".repeat(LONGEST_NAME + 1), false).is_some());
        assert_eq!(renamed(Path::new("docs/trail.txt"), "path.txt"), PathBuf::from("docs/path.txt"));
        assert_eq!(renamed(Path::new("trail.txt"), "path.txt"), PathBuf::from("path.txt"));
    }

    #[test]
    fn a_new_file_is_empty_untracked_and_never_written_over() {
        let f = committed();
        let index = f.git(&["ls-files", "--stage"]);
        let outcome = f.repo.create_file(Path::new("docs"), "notes.md").unwrap();
        assert_eq!(outcome.title, "Created notes.md");
        assert_eq!(fs::read(f.dir.join("docs/notes.md")).unwrap(), b"");
        assert_eq!(status(&f), "?? docs/notes.md\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        fs::write(f.dir.join("docs/notes.md"), "Kept words\n").unwrap();
        assert_eq!(f.repo.create_file(Path::new("docs"), "notes.md").unwrap_err(), "docs/notes.md is already there.");
        assert_eq!(fs::read_to_string(f.dir.join("docs/notes.md")).unwrap(), "Kept words\n");
        f.repo.create_file(Path::new(""), "maps/river/ford.txt").unwrap();
        assert!(f.dir.join("maps/river/ford.txt").is_file());
        fs::write(f.dir.join(".gitignore"), "*.log\n").unwrap();
        let ignored = f.repo.create_file(Path::new(""), "build.log").unwrap();
        assert!(ignored.detail.contains(".gitignore"), "{}", ignored.detail);
        for (folder, name) in [("", ".git/hooks/pre-commit"), ("..", "outside.txt"), ("docs", "")] {
            assert!(f.repo.create_file(Path::new(folder), name).is_err(), "{folder}/{name}");
        }
        assert!(!f.dir.join(".git/hooks/pre-commit").exists() && !f.dir.parent().unwrap().join("outside.txt").exists());
    }

    #[test]
    fn a_new_file_is_never_made_through_a_link_out_of_the_repository() {
        let f = committed();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), f.dir.join("elsewhere")).unwrap();
        let refused = f.repo.create_file(Path::new("elsewhere"), "planted.txt").unwrap_err();
        assert!(refused.contains("is a link"), "{refused}");
        assert!(!outside.path().join("planted.txt").exists());
        assert!(f.repo.create_file(Path::new("docs/trail.txt"), "under.txt").unwrap_err().contains("is a file"));
    }

    #[test]
    fn a_tracked_file_is_renamed_with_git_mv_and_the_rename_is_staged() {
        let f = committed();
        fs::write(f.dir.join("docs/trail.txt"), format!("{TRAIL}Edited\n")).unwrap();
        let outcome = f.repo.rename_file(Path::new("docs/trail.txt"), "path.txt", true).unwrap();
        assert_eq!(outcome.title, "Renamed trail.txt to path.txt");
        assert_eq!(status(&f), "RM docs/trail.txt -> docs/path.txt\n");
        assert_eq!(fs::read_to_string(f.dir.join("docs/path.txt")).unwrap(), format!("{TRAIL}Edited\n"));
        assert!(!f.dir.join("docs/trail.txt").exists());
        let head = f.git(&["rev-parse", "HEAD"]);
        f.repo.rename_file(Path::new("docs/path.txt"), "Path.txt", true).unwrap();
        assert!(String::from_utf8(f.git(&["ls-files"])).unwrap().contains("docs/Path.txt"));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn an_untracked_file_is_renamed_on_disk_and_another_file_is_never_replaced() {
        let f = committed();
        fs::write(f.dir.join("docs/draft.txt"), "Draft\n").unwrap();
        let index = f.git(&["ls-files", "--stage"]);
        f.repo.rename_file(Path::new("docs/draft.txt"), "plan.txt", false).unwrap();
        assert_eq!(status(&f), "?? docs/plan.txt\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(
            f.repo.rename_file(Path::new("docs/plan.txt"), "trail.txt", false).unwrap_err(),
            "docs/trail.txt is already there. Choose another name."
        );
        assert_eq!(fs::read_to_string(f.dir.join("docs/trail.txt")).unwrap(), TRAIL);
        assert_eq!(fs::read_to_string(f.dir.join("docs/plan.txt")).unwrap(), "Draft\n");
        assert!(exclusive_rename(&f.dir.join("docs/plan.txt"), &f.dir.join("docs/trail.txt")).is_err());
        assert_eq!(fs::read_to_string(f.dir.join("docs/trail.txt")).unwrap(), TRAIL);
        f.repo.rename_file(Path::new("docs/plan.txt"), "Plan.txt", false).unwrap();
        assert_eq!(status(&f), "?? docs/Plan.txt\n");
    }

    #[test]
    fn a_rename_is_refused_when_the_file_was_tracked_or_untracked_since_it_was_chosen() {
        let f = committed();
        fs::write(f.dir.join("docs/draft.txt"), "Draft\n").unwrap();
        f.git(&["add", "docs/draft.txt"]);
        let refused = f.repo.rename_file(Path::new("docs/draft.txt"), "plan.txt", false).unwrap_err();
        assert_eq!(refused, "draft.txt is now tracked since you chose Rename. Look at it again first.");
        f.git(&["rm", "-q", "--cached", "docs/trail.txt"]);
        let refused = f.repo.rename_file(Path::new("docs/trail.txt"), "path.txt", true).unwrap_err();
        assert!(refused.contains("no longer tracked"), "{refused}");
        assert!(f.dir.join("docs/draft.txt").exists() && f.dir.join("docs/trail.txt").exists());
        assert_eq!(status(&f), "A  docs/draft.txt\nD  docs/trail.txt\n?? docs/trail.txt\n");
        fs::remove_file(f.dir.join("docs/draft.txt")).unwrap();
        assert!(f.repo.rename_file(Path::new("docs/draft.txt"), "plan.txt", true).unwrap_err().contains("no longer in the working tree"));
    }

    #[test]
    fn a_tracked_file_moved_to_the_stand_in_trash_shows_as_an_unstaged_deletion() {
        let f = committed();
        let (bin, folder, _held) = bin();
        let index = f.git(&["ls-files", "--stage"]);
        let (outcome, trashed) = f.repo.trash_file(Path::new("docs/trail.txt"), true, &bin).unwrap();
        assert_eq!(outcome.title, "Moved trail.txt to the Trash");
        assert_eq!(trashed, folder.join("trail.txt"));
        assert_eq!(fs::read_to_string(&trashed).unwrap(), TRAIL);
        assert_eq!(status(&f), " D docs/trail.txt\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        fs::write(f.dir.join("docs/trail.txt"), "Another trail\n").unwrap();
        let (_, second) = f.repo.trash_file(Path::new("docs/trail.txt"), true, &bin).unwrap();
        assert_eq!(second, folder.join("trail 2.txt"));
        assert_eq!(fs::read_to_string(&trashed).unwrap(), TRAIL);
        f.repo.put_back_from_trash(&trashed, Path::new("docs/trail.txt")).unwrap();
        assert_eq!(status(&f), "");
        assert!(f.repo.put_back_from_trash(&second, Path::new("docs/trail.txt")).unwrap_err().contains("already there again"));
        assert!(second.exists());
    }

    #[test]
    fn moving_to_the_trash_is_refused_when_the_file_changed_since_it_was_chosen() {
        let f = committed();
        let (bin, folder, _held) = bin();
        fs::write(f.dir.join("docs/draft.txt"), "Draft\n").unwrap();
        f.git(&["add", "docs/draft.txt"]);
        let refused = f.repo.trash_file(Path::new("docs/draft.txt"), false, &bin).unwrap_err();
        assert_eq!(refused, "draft.txt is now tracked since you chose Move to Trash. Look at it again first.");
        assert!(f.dir.join("docs/draft.txt").exists() && !folder.exists());
        fs::remove_file(f.dir.join("docs/trail.txt")).unwrap();
        fs::create_dir(f.dir.join("docs/trail.txt")).unwrap();
        assert!(f.repo.trash_file(Path::new("docs/trail.txt"), true, &bin).unwrap_err().contains("now a folder"));
        assert!(f.repo.trash_file(Path::new("docs/gone.txt"), false, &bin).unwrap_err().contains("no longer in the working tree"));
        assert!(f.repo.trash_file(Path::new("../outside.txt"), false, &bin).is_err());
        assert!(!folder.exists());
    }

    #[test]
    fn tests_never_reach_the_system_trash() {
        assert!(Bin::current().is_err());
        assert_eq!(free_place(Path::new("/nonexistent-trash"), OsStr::new("a.txt")), Some(PathBuf::from("/nonexistent-trash/a.txt")));
    }
}
