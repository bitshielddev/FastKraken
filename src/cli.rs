use crate::{
    Tab,
    commands::{Availability, Bar, Command, Group, Target},
    notice::Notice,
    view::{self, Tone},
};
use gpui::{Context, Window};
use std::{
    env,
    ffi::{CString, OsStr},
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

pub(crate) const NAME: &str = "fastkraken";

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "app.install-command",
    Group::Settings,
    |_, _| "Install command line tool…".into(),
    |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Cli(Action::Install)),
)
.words(&["shell command", "terminal", "path", "fastkraken command", "cli"])
.icon("external")
.bar(Bar::App)];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Install,
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Install => {
            let installed = places().and_then(|places| {
                let executable = executable()?;
                install(&places, &executable).map(|installed| (installed, places))
            });
            let notice = match installed {
                Ok((installed, places)) => installed_notice(&installed, &places, crate::environment::host_dirs()),
                Err(reason) => Notice::new(Tone::Error, "Could not install the fastkraken command", reason),
            };
            tab.ui.notices.show(notice);
        }
    }
    cx.notify();
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Places {
    pub(crate) system: PathBuf,
    pub(crate) home: PathBuf,
}

impl Places {
    pub(crate) fn local(&self) -> PathBuf {
        self.home.join(".local").join("bin")
    }

    pub(crate) fn chosen(&self) -> PathBuf {
        if writable(&self.system) { self.system.clone() } else { self.local() }
    }

    pub(crate) fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
            _ => path.display().to_string(),
        }
    }
}

pub(crate) fn places() -> Result<Places, String> {
    if let Some(sandbox) = crate::acceptance::sandbox() {
        return Ok(Places { system: sandbox.join("usr/local/bin"), home: sandbox.join("home") });
    }
    if cfg!(test) {
        let root = env::temp_dir().join(format!("fastkraken-cli-test-{}", std::process::id()));
        return Ok(Places { system: root.join("usr/local/bin"), home: root.join("home") });
    }
    let home = env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute()).ok_or("Cannot find your home folder.")?;
    Ok(Places { system: PathBuf::from("/usr/local/bin"), home })
}

pub(crate) fn executable() -> Result<PathBuf, String> {
    env::current_exe().and_then(fs::canonicalize).map_err(|e| format!("Cannot find FastKraken's own program: {e}"))
}

fn writable(dir: &Path) -> bool {
    let Ok(name) = CString::new(dir.as_os_str().as_bytes()) else { return false };
    dir.is_dir() && unsafe { libc::access(name.as_ptr(), libc::W_OK) } == 0
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Installed {
    pub(crate) link: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) replaced: Option<PathBuf>,
    pub(crate) already: bool,
}

pub(crate) fn install(places: &Places, target: &Path) -> Result<Installed, String> {
    let dir = places.chosen();
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", places.shown(&dir)))?;
    let link = dir.join(NAME);
    let before = match fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => fs::read_link(&link).ok(),
        Ok(_) => {
            return Err(format!(
                "{} is already there and is not a link. Move it away, then install again.",
                places.shown(&link)
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not look at {}: {error}", places.shown(&link))),
    };
    if before.as_deref() == Some(target) {
        return Ok(Installed { link, target: target.to_owned(), replaced: None, already: true });
    }
    let fresh = dir.join(format!(".{NAME}-{}", std::process::id()));
    let _ = fs::remove_file(&fresh);
    std::os::unix::fs::symlink(target, &fresh).map_err(|e| format!("Could not create {}: {e}", places.shown(&link)))?;
    if let Err(error) = fs::rename(&fresh, &link) {
        let _ = fs::remove_file(&fresh);
        return Err(format!("Could not create {}: {error}", places.shown(&link)));
    }
    Ok(Installed { link, target: target.to_owned(), replaced: before, already: false })
}

pub(crate) fn on_path(dir: &Path, dirs: &[PathBuf]) -> bool {
    let real = fs::canonicalize(dir).ok();
    dirs.iter().any(|entry| entry == dir || (real.is_some() && fs::canonicalize(entry).ok() == real))
}

pub(crate) fn installed_notice(installed: &Installed, places: &Places, dirs: &[PathBuf]) -> Notice {
    let link = places.shown(&installed.link);
    let dir = installed.link.parent().unwrap_or(Path::new("/"));
    let title = if installed.already { "The fastkraken command is installed" } else { "Installed the fastkraken command" };
    let mut detail = format!("{link} opens FastKraken. In a terminal, fastkraken opens the folder you are in, and fastkraken <path> another.");
    if let Some(old) = &installed.replaced {
        detail.push_str(&format!(" It led to {} before.", old.display()));
    }
    if on_path(dir, dirs) {
        return Notice::new(Tone::Success, title, detail);
    }
    let shown = places.shown(dir);
    let export = match dir.strip_prefix(&places.home) {
        Ok(rest) => format!("export PATH=\"$HOME/{}:$PATH\"", rest.display()),
        Err(_) => format!("export PATH=\"{}:$PATH\"", dir.display()),
    };
    detail.push_str(&format!(
        " {shown} is not on your search path, so a terminal will not find it yet. Add it in your shell's profile: {export}"
    ));
    Notice::new(Tone::Info, title, detail)
        .sticky()
        .offer("Copy the line", view::Action::CopyText { text: export, what: "line" })
}


pub(crate) fn through_link(argv0: &OsStr, search: Option<&OsStr>, cwd: &Path) -> bool {
    let name = Path::new(argv0);
    let ran = if argv0.as_bytes().contains(&b'/') {
        Some(cwd.join(name))
    } else {
        search
            .into_iter()
            .flat_map(env::split_paths)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    };
    ran.and_then(|ran| fs::symlink_metadata(ran).ok()).is_some_and(|meta| meta.file_type().is_symlink())
}

pub(crate) fn bundle(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.ends_with("MacOS") && contents.ends_with("Contents") && app.extension() == Some(OsStr::new("app"))).then_some(app)
}

pub(crate) fn detach(executable: &Path, path: &Path) -> io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(executable);
    command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    command.spawn().map(|child| child.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn sandbox() -> (tempfile::TempDir, Places, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let places = Places { system: dir.path().join("usr/local/bin"), home: dir.path().join("home") };
        let app = dir.path().join("FastKraken.app/Contents/MacOS");
        fs::create_dir_all(&app).unwrap();
        let executable = app.join(NAME);
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        (dir, places, executable)
    }

    #[test]
    fn the_command_goes_in_local_bin_unless_usr_local_bin_may_be_written() {
        let (_dir, places, executable) = sandbox();
        let installed = install(&places, &executable).unwrap();
        assert_eq!(installed.link, places.home.join(".local/bin/fastkraken"));
        assert_eq!(fs::read_link(&installed.link).unwrap(), executable);
        assert!(!installed.already && installed.replaced.is_none());
        assert!(!places.system.exists());
        fs::create_dir_all(&places.system).unwrap();
        fs::set_permissions(&places.system, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(places.chosen(), places.local());
        assert_eq!(install(&places, &executable).unwrap().link, places.local().join(NAME));
        assert!(!places.system.join(NAME).exists());
        fs::set_permissions(&places.system, fs::Permissions::from_mode(0o755)).unwrap();
        let installed = install(&places, &executable).unwrap();
        assert_eq!(installed.link, places.system.join(NAME));
        assert_eq!(fs::read_link(places.system.join(NAME)).unwrap(), executable);
        let names: Vec<_> = fs::read_dir(&places.system).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, [OsStr::new(NAME)]);
    }

    #[test]
    fn installing_again_keeps_the_link_and_a_link_elsewhere_is_replaced() {
        let (dir, places, executable) = sandbox();
        let first = install(&places, &executable).unwrap();
        let again = install(&places, &executable).unwrap();
        assert!(again.already, "{again:?}");
        assert_eq!(fs::read_link(&again.link).unwrap(), executable);
        fs::remove_file(&first.link).unwrap();
        let old = dir.path().join("Old/FastKraken.app/Contents/MacOS/fastkraken");
        symlink(&old, &first.link).unwrap();
        let replaced = install(&places, &executable).unwrap();
        assert_eq!(replaced.replaced, Some(old));
        assert_eq!(fs::read_link(&first.link).unwrap(), executable);
    }

    #[test]
    fn a_file_of_that_name_that_is_not_a_link_is_left_alone() {
        let (_dir, places, executable) = sandbox();
        fs::create_dir_all(places.local()).unwrap();
        let theirs = places.local().join(NAME);
        fs::write(&theirs, "#!/bin/sh\necho mine\n").unwrap();
        let error = install(&places, &executable).unwrap_err();
        assert_eq!(error, "~/.local/bin/fastkraken is already there and is not a link. Move it away, then install again.");
        assert_eq!(fs::read_to_string(&theirs).unwrap(), "#!/bin/sh\necho mine\n");
        assert!(!fs::symlink_metadata(&theirs).unwrap().file_type().is_symlink());
        let (_dir, places, executable) = sandbox();
        fs::create_dir_all(&places.home).unwrap();
        fs::write(places.home.join(".local"), "").unwrap();
        let error = install(&places, &executable).unwrap_err();
        assert!(error.starts_with("Could not create ~/.local/bin: "), "{error}");
    }

    #[test]
    fn the_notice_says_where_the_command_is_and_warns_when_a_shell_will_not_find_it() {
        let (_dir, places, executable) = sandbox();
        let installed = install(&places, &executable).unwrap();
        let notice = installed_notice(&installed, &places, &[PathBuf::from("/usr/bin"), places.local()]);
        assert_eq!(notice.tone, Tone::Success);
        assert_eq!(notice.title, "Installed the fastkraken command");
        assert_eq!(
            notice.detail,
            "~/.local/bin/fastkraken opens FastKraken. In a terminal, fastkraken opens the folder you are in, and fastkraken <path> another."
        );
        let notice = installed_notice(&installed, &places, &[PathBuf::from("/usr/bin")]);
        assert_eq!(notice.tone, Tone::Info);
        assert!(
            notice.detail.ends_with(
                " ~/.local/bin is not on your search path, so a terminal will not find it yet. Add it in your shell's profile: export PATH=\"$HOME/.local/bin:$PATH\""
            ),
            "{}",
            notice.detail
        );
        assert_eq!(notice.actions.len(), 1);
        assert_eq!(notice.actions[0].0.as_ref(), "Copy the line");
        let (dir, places, executable) = sandbox();
        let installed = install(&places, &executable).unwrap();
        let other = dir.path().join("bin-link");
        symlink(places.local(), &other).unwrap();
        assert!(on_path(&places.local(), &[other]));
        let again = installed_notice(&install(&places, &executable).unwrap(), &places, &[places.local()]);
        assert_eq!(again.title, "The fastkraken command is installed");
        fs::create_dir_all(&places.system).unwrap();
        let system = install(&places, &executable).unwrap();
        let notice = installed_notice(&system, &places, &[]);
        assert!(notice.detail.contains(&format!("export PATH=\"{}:$PATH\"", places.system.display())), "{}", notice.detail);
        assert_eq!(installed.link, places.local().join(NAME));
    }

    #[test]
    fn the_command_is_told_from_the_program_run_by_its_own_path() {
        let (dir, places, executable) = sandbox();
        let installed = install(&places, &executable).unwrap();
        let cwd = dir.path();
        let search = env::join_paths([PathBuf::from("/usr/bin"), places.local()]).unwrap();
        assert!(through_link(OsStr::new(NAME), Some(&search), cwd));
        assert!(through_link(installed.link.as_os_str(), None, cwd));
        assert!(through_link(OsStr::new("home/.local/bin/fastkraken"), None, cwd));
        assert!(!through_link(executable.as_os_str(), Some(&search), cwd));
        assert!(!through_link(OsStr::new("FastKraken.app/Contents/MacOS/fastkraken"), None, cwd));
        assert!(!through_link(OsStr::new(NAME), Some(OsStr::new("/usr/bin")), cwd));
        assert!(!through_link(OsStr::new(NAME), None, cwd));
    }

    #[test]
    fn the_bundle_is_the_app_folder_around_the_program() {
        assert_eq!(
            bundle(Path::new("/Applications/FastKraken.app/Contents/MacOS/fastkraken")),
            Some(Path::new("/Applications/FastKraken.app"))
        );
        assert_eq!(bundle(Path::new("/Users/dan/fastkraken/target/release/fastkraken")), None);
        assert_eq!(bundle(Path::new("/x/Contents/MacOS/fastkraken")), None);
    }

    #[test]
    fn the_app_started_apart_from_the_terminal_has_a_session_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("app.sh");
        fs::write(&script, "sleep 5\n").unwrap();
        let pid = detach(Path::new("/bin/sh"), &script).unwrap() as libc::pid_t;
        let (session, group, ours) = unsafe { (libc::getsid(pid), libc::getpgid(pid), libc::getsid(0)) };
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert_eq!(session, pid, "the child leads a session of its own");
        assert_eq!(group, pid);
        assert_ne!(session, ours);
    }

    #[test]
    fn places_in_a_test_are_never_the_user_s() {
        let places = places().unwrap();
        assert!(places.system.starts_with(env::temp_dir()), "{places:?}");
        assert!(places.home.starts_with(env::temp_dir()), "{places:?}");
        assert_eq!(places.shown(&places.home.join(".local/bin")), "~/.local/bin");
        assert_eq!(places.shown(Path::new("/usr/local/bin")), "/usr/local/bin");
    }

    #[test]
    fn install_is_in_the_fastkraken_menu_after_preferences() {
        let ids: Vec<_> = crate::commands::in_bar(Bar::App).into_iter().map(|command| command.id).collect();
        assert_eq!(ids, ["app.about", "app.preferences", "app.install-command"]);
        assert_eq!(crate::commands::bar_index(Bar::App, "app.install-command"), Some(3));
        let command = crate::commands::find("app.install-command").unwrap();
        assert_eq!(crate::commands::title_case("Install command line tool…"), "Install Command Line Tool…");
        assert_eq!(command.shortcut, None);
    }
}
