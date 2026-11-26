use std::{
    env,
    ffi::{OsStr, OsString},
    io::Read,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        OnceLock, mpsc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(3);
const MARKER: &str = "__FASTKRAKEN_PATH__";
const FALLBACK: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

static LOGIN: OnceLock<Option<OsString>> = OnceLock::new();
static EARLY: OnceLock<Vec<PathBuf>> = OnceLock::new();
static RESOLVED: OnceLock<Vec<PathBuf>> = OnceLock::new();

static USE_LOGIN: AtomicBool = AtomicBool::new(true);

fn isolated() -> bool {
    cfg!(test) || crate::acceptance::replaying()
}

pub(crate) fn use_login_shell(on: bool, cx: &gpui::App) {
    USE_LOGIN.store(on, Ordering::Relaxed);
    if on {
        start(cx);
    }
}

fn start(cx: &gpui::App) {
    if isolated() || LOGIN.get().is_some() {
        return;
    }
    cx.background_executor()
        .spawn(async {
            let answer = login_shell().and_then(|shell| probe(&shell, LIMIT));
            let _ = LOGIN.set(answer);
        })
        .detach();
}

pub(crate) fn host_dirs() -> &'static [PathBuf] {
    let process = || env::var_os("PATH");
    if isolated() {
        return EARLY.get_or_init(|| {
            let mut dirs = compose(None, process().as_deref(), false);
            if let Some(bin) = crate::acceptance::sandbox().map(|sandbox| sandbox.join("bin")) {
                if !dirs.contains(&bin) {
                    dirs.push(bin);
                }
            }
            dirs
        });
    }
    match answered(LOGIN.get(), USE_LOGIN.load(Ordering::Relaxed)) {
        Some(login) => RESOLVED.get_or_init(|| compose(login.as_deref(), process().as_deref(), true)),
        None => EARLY.get_or_init(|| compose(None, process().as_deref(), true)),
    }
}

fn answered(login: Option<&Option<OsString>>, wanted: bool) -> Option<&Option<OsString>> {
    login.filter(|_| wanted)
}

pub(crate) fn compose(login: Option<&OsStr>, process: Option<&OsStr>, fallback: bool) -> Vec<PathBuf> {
    let homebrew = (fallback && login.is_none()).then(|| FALLBACK.map(PathBuf::from)).into_iter().flatten();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in login.into_iter().chain(process).flat_map(env::split_paths).chain(homebrew) {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

pub(crate) fn login_shell() -> Option<PathBuf> {
    let shell = env::var_os("SHELL").map(PathBuf::from).filter(|shell| shell.is_absolute()).or_else(account_shell)?;
    shell.is_file().then_some(shell)
}

fn account_shell() -> Option<PathBuf> {
    unsafe {
        let mut record: libc::passwd = std::mem::zeroed();
        let mut buffer = vec![0 as libc::c_char; 4096];
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let code = libc::getpwuid_r(libc::getuid(), &mut record, buffer.as_mut_ptr(), buffer.len(), &mut found);
        if code != 0 || found.is_null() || record.pw_shell.is_null() {
            return None;
        }
        let shell = std::ffi::CStr::from_ptr(record.pw_shell).to_bytes();
        let shell = PathBuf::from(OsStr::from_bytes(shell));
        shell.is_absolute().then_some(shell)
    }
}

fn arguments(shell: &Path) -> &'static [&'static str] {
    let name = shell.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.ends_with("csh") { &["-c"] } else { &["-i", "-l", "-c"] }
}

fn script() -> String {
    format!("printf '{MARKER}'; /usr/bin/printenv PATH; printf '{MARKER}'")
}

pub(crate) fn probe(shell: &Path, limit: Duration) -> Option<OsString> {
    let deadline = Instant::now() + limit;
    let mut command = Command::new(shell);
    command
        .args(arguments(shell))
        .arg(script())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    if let Some(home) = env::var_os("HOME").filter(|home| Path::new(home).is_dir()) {
        command.current_dir(home);
    }
    let mut child = command.spawn().ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        stop(child, deadline);
        return None;
    };
    let (sent, heard) = mpsc::channel();
    thread::spawn(move || {
        let mut said = Vec::new();
        let mut buffer = [0; 4096];
        while answer(&said).is_none() && said.len() < 1 << 20 {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => said.extend_from_slice(&buffer[..n]),
            }
        }
        let _ = sent.send(said);
    });
    let said = heard.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok();
    let found = said.as_deref().and_then(answer);
    if found.is_some() {
        thread::spawn(move || stop(child, deadline));
    } else {
        stop(child, deadline);
    }
    found
}

fn stop(mut child: Child, deadline: Instant) {
    let group = child.id() as libc::pid_t;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => break,
        }
    }
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
}

fn answer(said: &[u8]) -> Option<OsString> {
    let marker = MARKER.as_bytes();
    let find = |bytes: &[u8]| bytes.windows(marker.len()).position(|window| window == marker);
    let start = find(said)? + marker.len();
    let length = find(&said[start..])?;
    let mut path = &said[start..start + length];
    while let [rest @ .., b'\n' | b'\r'] = path {
        path = rest;
    }
    (!path.is_empty()).then(|| OsString::from_vec(path.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn the_login_shells_path_is_used_only_while_the_setting_is_on() {
        let said = Some(OsString::from("/Users/cedar/.local/bin"));
        assert_eq!(answered(Some(&said), true), Some(&said));
        assert_eq!(answered(Some(&said), false), None);
        assert_eq!(answered(None, true), None);
        assert_eq!(answered(Some(&None), true), Some(&None));
    }

    fn shell(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn alive(pid: &str) -> bool {
        pid.trim().parse::<libc::pid_t>().is_ok_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
    }

    #[test]
    fn the_path_is_read_between_the_markers_whatever_else_the_shell_prints() {
        let dir = tempfile::tempdir().unwrap();
        let fish = shell(
            dir.path(),
            "fish",
            r#"[ "$1 $2 $3" = "-i -l -c" ] || exit 3
echo "Welcome to fish, the friendly interactive shell"
PATH=/opt/fish/bin:/usr/bin:/bin; export PATH
/bin/sh -c "$4"
echo "Goodbye""#,
        );
        assert_eq!(probe(&fish, LIMIT), Some(OsString::from("/opt/fish/bin:/usr/bin:/bin")));
        let zsh = shell(dir.path(), "zsh", "PATH='/Users/me/Library/Application Support/bin:/usr/bin'; export PATH\nexec /bin/sh -c \"$4\"");
        assert_eq!(probe(&zsh, LIMIT), Some(OsString::from("/Users/me/Library/Application Support/bin:/usr/bin")));
        let tcsh = shell(dir.path(), "tcsh", "[ \"$1\" = -c ] || exit 3\nexec /bin/sh -c \"$2\"");
        assert!(probe(&tcsh, LIMIT).is_some());
        assert_eq!(answer(b"motd\n__FASTKRAKEN_PATH__/a:/b\r\n__FASTKRAKEN_PATH__bye"), Some(OsString::from("/a:/b")));
        assert_eq!(answer(b"__FASTKRAKEN_PATH__/a:/b"), None, "the second marker never came");
        assert_eq!(answer(b"__FASTKRAKEN_PATH__\n__FASTKRAKEN_PATH__"), None, "an empty PATH is no answer");
    }

    #[test]
    fn a_shell_that_fails_or_says_nothing_gives_no_answer() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe(&shell(dir.path(), "zsh", "echo 'zsh: bad option: -i' >&2\nexit 1"), LIMIT), None);
        assert_eq!(probe(&shell(dir.path(), "bash", "echo 'no markers here'"), LIMIT), None);
        assert_eq!(probe(&dir.path().join("missing-shell"), LIMIT), None);
    }

    #[test]
    fn a_shell_that_never_answers_is_stopped_at_the_limit_with_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let hanging = shell(
            dir.path(),
            "zsh",
            &format!("sleep 30 &\necho $! > '{}'\necho $$ >> '{}'\nwait", pids.display(), pids.display()),
        );
        let started = Instant::now();
        assert_eq!(probe(&hanging, Duration::from_millis(1500)), None);
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(1500) && took < Duration::from_secs(4), "{took:?}");
        let pids = fs::read_to_string(&pids).unwrap_or_default();
        for _ in 0..50 {
            if !pids.lines().any(alive) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!pids.lines().any(alive), "{pids}");
    }

    #[test]
    fn an_answer_counts_even_when_the_shell_lingers_or_leaves_something_running() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let lingering = shell(dir.path(), "bash", &format!("echo $$ > '{}'\n/bin/sh -c \"$4\"\nsleep 30", pid.display()));
        let limit = Duration::from_millis(2500);
        let started = Instant::now();
        assert_eq!(probe(&lingering, limit), Some(env::var_os("PATH").unwrap()));
        assert!(started.elapsed() < limit, "the answer did not wait for the shell: {:?}", started.elapsed());
        let pid = fs::read_to_string(&pid).unwrap();
        while alive(&pid) && started.elapsed() < limit * 2 {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(&pid), "the shell outlived its limit");
    }

    #[test]
    fn the_shell_runs_in_a_session_of_its_own_away_from_any_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let shell = shell(dir.path(), "zsh", &format!("echo $$ > '{}'\n/bin/sh -c \"$4\"\nsleep 30", pid.display()));
        assert!(probe(&shell, Duration::from_millis(2500)).is_some());
        let pid: libc::pid_t = fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
        let (theirs, ours) = unsafe { (libc::getsid(pid), libc::getsid(0)) };
        assert_eq!(theirs, pid, "the shell leads a session");
        assert_ne!(theirs, ours);
    }

    #[test]
    fn the_script_works_in_the_real_fish_zsh_and_bash() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        fs::write(home.join(".config/fish/config.fish"), "echo 'hello from fish'\nset -gx PATH /opt/from-fish/bin $PATH\n").unwrap();
        fs::write(home.join(".zprofile"), "echo 'hello from zprofile'\nexport PATH=/opt/from-zprofile/bin:$PATH\n").unwrap();
        fs::write(home.join(".zshrc"), "echo 'hello from zshrc'\nexport PATH=/opt/from-zshrc/bin:$PATH\n").unwrap();
        fs::write(home.join(".bash_profile"), "echo 'hello from bash'\nexport PATH=/opt/from-bash/bin:$PATH\n").unwrap();
        let mut tried = 0;
        for (real, added) in [
            ("/opt/homebrew/bin/fish", vec!["/opt/from-fish/bin"]),
            ("/usr/local/bin/fish", vec!["/opt/from-fish/bin"]),
            ("/bin/zsh", vec!["/opt/from-zprofile/bin", "/opt/from-zshrc/bin"]),
            ("/bin/bash", vec!["/opt/from-bash/bin"]),
        ] {
            if !Path::new(real).is_file() {
                continue;
            }
            tried += 1;
            let name = Path::new(real).file_name().unwrap().to_str().unwrap();
            let folder = dir.path().join(tried.to_string());
            fs::create_dir(&folder).unwrap();
            let wrapper = shell(
                &folder,
                name,
                &format!(
                    "unset TERM_PROGRAM\nexport HOME='{home}' ZDOTDIR='{home}' XDG_CONFIG_HOME='{home}/.config' XDG_DATA_HOME='{home}/.local/share' XDG_CACHE_HOME='{home}/.cache'\nexec {real} \"$@\"",
                    home = home.display()
                ),
            );
            let found = probe(&wrapper, Duration::from_secs(10)).unwrap_or_else(|| panic!("{real} gave no answer"));
            let dirs: Vec<PathBuf> = env::split_paths(&found).collect();
            for dir in added {
                assert!(dirs.contains(&PathBuf::from(dir)), "{real}: {found:?}");
            }
            assert!(dirs.contains(&PathBuf::from("/usr/bin")), "{real}: {found:?}");
        }
        assert!(tried > 0, "this Mac has none of the shells");
    }

    #[test]
    fn the_search_path_is_the_shells_then_the_apps_then_homebrews_until_the_shell_answers() {
        let paths = |dirs: Vec<PathBuf>| dirs.into_iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>();
        let finder = OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        assert_eq!(
            paths(compose(None, Some(finder), true)),
            ["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/opt/homebrew/bin", "/usr/local/bin"]
        );
        let login = OsStr::new("/Users/me/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:relative::/usr/bin");
        assert_eq!(
            paths(compose(Some(login), Some(finder), true)),
            ["/Users/me/.local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        );
        assert_eq!(paths(compose(Some(OsStr::new("/usr/bin")), Some(OsStr::new("/bin")), true)), ["/usr/bin", "/bin"]);
        assert_eq!(paths(compose(None, Some(OsStr::new("/tmp/empty")), false)), ["/tmp/empty"]);
        assert!(compose(None, None, false).is_empty());
        assert!(LOGIN.get().is_none());
        assert_eq!(host_dirs(), compose(None, env::var_os("PATH").as_deref(), false).as_slice());
    }

    #[test]
    fn the_csh_family_is_asked_without_an_interactive_login() {
        assert_eq!(arguments(Path::new("/bin/tcsh")), ["-c"]);
        assert_eq!(arguments(Path::new("/opt/homebrew/bin/fish")), ["-i", "-l", "-c"]);
        assert_eq!(arguments(Path::new("/bin/zsh")), ["-i", "-l", "-c"]);
    }
}
