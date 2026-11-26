use crate::{
    Tab, activity, handoff,
    commands::{Availability, Bar, Command, Group, Target},
    notice::Notice,
    view::{self, Tone},
};
use gpui::{Context, Window};
use std::{
    cell::Cell,
    fmt::Write as _,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Once, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const REPORT_PREFIX: &str = "crash-";
pub(crate) const REPORT_SUFFIX: &str = ".txt";
pub(crate) const ACTIVITY_LOG: &str = "activity.log";
pub(crate) const RECENT: usize = 20;
pub(crate) const REVEAL_REPORT: &str = "Reveal report";
pub(crate) const RECORDED: &str = "FastKraken recorded a problem";
const STAYS: &str = "This report stays on this Mac. FastKraken does not send it anywhere.";
const TOKEN_PREFIXES: &[&str] = &["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "app.logs",
    Group::App,
    |_, _| "Reveal logs".into(),
    |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Diagnostics(Action::RevealLogs)),
)
.words(&["support logs", "log files", "crash report", "diagnostics", "problem", "bug report"])
.icon("folder")
.bar(Bar::Help)];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    RevealLogs,
}

static FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);
static SYSTEM: OnceLock<String> = OnceLock::new();

thread_local! {
    static WRITING: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn folder_for(state: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match (state, home) {
        (Some(state), _) => Some(state.join("Logs")),
        (None, Some(home)) if cfg!(target_os = "macos") => Some(home.join("Library/Logs/FastKraken")),
        (None, Some(home)) => Some(home.join(".local/state/fastkraken/logs")),
        (None, None) => None,
    }
}

pub(crate) fn folder() -> Option<PathBuf> {
    FOLDER.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn install(folder: Option<PathBuf>) {
    *FOLDER.lock().unwrap_or_else(|e| e.into_inner()) = folder;
    SYSTEM.get_or_init(system);
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            recorded(info);
            previous(info);
        }));
    });
}

fn recorded(info: &std::panic::PanicHookInfo<'_>) {
    if WRITING.with(|writing| writing.replace(true)) {
        return;
    }
    if let Some(folder) = folder() {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "The program stopped without saying why.".to_owned());
        let crash = Crash {
            at: SystemTime::now(),
            thread: std::thread::current().name().unwrap_or("unnamed").to_owned(),
            message,
            place: info.location().map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column())),
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        };
        let text = report(&crash, &activity::recent(RECENT));
        if let Ok(path) = write_report(&folder, crash.at, &text) {
            eprintln!("FastKraken saved a report of this problem at {}", path.display());
        }
    }
    WRITING.with(|writing| writing.set(false));
}

fn system() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "macos") {
        let version = fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
            .ok()
            .and_then(|plist| product_version(&plist))
            .unwrap_or_else(|| "(version unknown)".to_owned());
        format!("macOS {version} ({arch})")
    } else {
        format!("{} ({arch})", std::env::consts::OS)
    }
}

pub(crate) fn product_version(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>ProductVersion</key>")? + "<key>ProductVersion</key>".len()..];
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    let version = after[start..end].trim();
    (!version.is_empty() && version.len() < 32).then(|| version.to_owned())
}

pub(crate) struct Crash {
    pub at: SystemTime,
    pub thread: String,
    pub message: String,
    pub place: Option<String>,
    pub backtrace: String,
}

fn local(at: SystemTime) -> Option<libc::tm> {
    let seconds = at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs()) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null()).then_some(tm)
}

pub(crate) fn file_stamp(at: SystemTime) -> String {
    match local(at) {
        Some(tm) => format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs()).to_string(),
    }
}

fn written_at(at: SystemTime) -> String {
    match local(at) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => String::new(),
    }
}

pub(crate) fn masked(text: &str) -> String {
    let text = activity::redact(text);
    let mut shown = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some((at, prefix)) = TOKEN_PREFIXES.iter().filter_map(|prefix| rest.find(prefix).map(|at| (at, prefix))).min() {
        let begins_word = rest[..at].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let body = &rest[at + prefix.len()..];
        let length = body.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(body.len());
        shown.push_str(&rest[..at]);
        if begins_word && length >= 8 {
            shown.push_str(prefix);
            shown.push_str(activity::MASK);
        } else {
            shown.push_str(&rest[at..at + prefix.len() + length]);
        }
        rest = &body[length..];
    }
    shown.push_str(rest);
    shown
}

fn indented(text: &mut String, lines: &str) {
    for line in lines.trim_end().lines() {
        let _ = writeln!(text, "  {line}");
    }
}

fn entries(text: &mut String, recent: &[Arc<activity::Entry>]) {
    if recent.is_empty() {
        let _ = writeln!(text, "  None ran.");
    }
    for entry in recent {
        let _ = writeln!(text, "  {}", entry.line());
        if !entry.succeeded() {
            let said: Vec<&str> = entry.stderr.trim_end().lines().filter(|line| !line.trim().is_empty()).collect();
            for line in &said[said.len().saturating_sub(3)..] {
                let _ = writeln!(text, "      {}", line.trim_end());
            }
        }
    }
}

pub(crate) fn report(crash: &Crash, recent: &[Arc<activity::Entry>]) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "FastKraken problem report");
    let _ = writeln!(text);
    let _ = writeln!(text, "FastKraken {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(text, "{}", SYSTEM.get_or_init(system));
    let _ = writeln!(text, "Time: {}", written_at(crash.at));
    let _ = writeln!(text, "Thread: {}", crash.thread);
    let _ = writeln!(text);
    let _ = writeln!(text, "What happened");
    indented(&mut text, &crash.message);
    if let Some(place) = &crash.place {
        let _ = writeln!(text, "  at {place}");
    }
    let _ = writeln!(text);
    match recent.len() {
        0 => writeln!(text, "Git commands"),
        count => writeln!(text, "The last {count} Git commands, oldest first, with addresses and passwords masked"),
    }
    .ok();
    entries(&mut text, recent);
    let _ = writeln!(text);
    let _ = writeln!(text, "Backtrace");
    indented(&mut text, &crash.backtrace);
    let _ = writeln!(text);
    let _ = writeln!(text, "{STAYS}");
    masked(&text)
}

pub(crate) fn activity_log(recent: &[Arc<activity::Entry>], at: SystemTime) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "FastKraken {} · {}", env!("CARGO_PKG_VERSION"), SYSTEM.get_or_init(system));
    let _ = writeln!(text, "Git commands FastKraken ran, oldest first, as of {}. Addresses and passwords are masked.", written_at(at));
    let _ = writeln!(text);
    entries(&mut text, recent);
    masked(&text)
}

fn private_folder(folder: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(folder)
}

pub(crate) fn write_report(folder: &Path, at: SystemTime, text: &str) -> io::Result<PathBuf> {
    private_folder(folder)?;
    let base = format!("{REPORT_PREFIX}{}", file_stamp(at));
    for count in 1..100 {
        let name = if count == 1 { format!("{base}{REPORT_SUFFIX}") } else { format!("{base}-{count}{REPORT_SUFFIX}") };
        let path = folder.join(name);
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("Too many reports in one second"))
}

pub(crate) fn write_activity(folder: &Path, text: &str) -> io::Result<PathBuf> {
    private_folder(folder)?;
    let path = folder.join(ACTIVITY_LOG);
    let mut file = tempfile::Builder::new().prefix(".activity").permissions(fs::Permissions::from_mode(0o600)).tempfile_in(folder)?;
    file.write_all(text.as_bytes())?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(path)
}

fn report_key(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix(REPORT_PREFIX)?;
    Some(name.strip_suffix(REPORT_SUFFIX)?.to_owned())
}

pub(crate) fn reports(folder: &Path) -> Vec<PathBuf> {
    let Ok(listed) = fs::read_dir(folder) else { return Vec::new() };
    let mut found: Vec<(String, PathBuf)> = listed
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| Some((report_key(&path)?, path)))
        .collect();
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

pub(crate) fn unannounced(folder: &Path, seen: Option<&str>) -> Option<PathBuf> {
    let newest = reports(folder).pop()?;
    let key = report_key(&newest)?;
    let seen = seen.and_then(|seen| report_key(Path::new(seen)));
    seen.is_none_or(|seen| key > seen).then_some(newest)
}

pub(crate) fn announcement(report: &Path) -> Notice {
    let when = fs::metadata(report)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map(|since| format!(" from {}", crate::dates::short(since.as_secs() as i64)))
        .unwrap_or_default();
    Notice::new(
        Tone::Info,
        RECORDED,
        format!("A report{when} is in FastKraken's logs folder. It stays on this Mac: nothing was sent anywhere."),
    )
    .sticky()
    .offer(REVEAL_REPORT, view::Action::Handoff(handoff::Action::RevealSaved(report.to_owned())))
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::RevealLogs => match folder() {
            None => tab.ui.say(Tone::Info, "FastKraken keeps no logs in this session"),
            Some(folder) => match write_activity(&folder, &activity_log(&activity::entries(), SystemTime::now())) {
                Ok(path) => return handoff::dispatch(tab, handoff::Action::RevealSaved(path), window, cx),
                Err(error) => {
                    tab.ui.notices.show(Notice::new(
                        Tone::Error,
                        "Could not write FastKraken's logs",
                        format!("{}: {error}", folder.display()),
                    ));
                }
            },
        },
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::Folder;
    use std::{path::Path, time::Duration};

    fn entry(argv: &[&str], exit: i32, stderr: &str) -> Arc<activity::Entry> {
        Arc::new(activity::Entry {
            id: 0,
            at: UNIX_EPOCH + Duration::from_secs(1_795_000_000),
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            dir: PathBuf::from("/tmp/Beacon"),
            read_only: false,
            duration: Duration::from_millis(12),
            exit: Some(exit),
            note: None,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })
    }

    fn crash(message: &str) -> Crash {
        Crash {
            at: UNIX_EPOCH + Duration::from_secs(1_795_000_100),
            thread: "main".into(),
            message: message.into(),
            place: Some("src/view.rs:12:5".into()),
            backtrace: "   0: fastkraken::view::draw\n   1: main".into(),
        }
    }

    #[test]
    fn logs_go_to_the_state_folder_when_one_is_given_and_otherwise_to_the_user_s_logs() {
        assert_eq!(folder_for(Some(Path::new("/tmp/sandbox/state")), Some(Path::new("/Users/cedar"))), Some(PathBuf::from("/tmp/sandbox/state/Logs")));
        assert_eq!(folder_for(None, Some(Path::new("/Users/cedar"))), Some(PathBuf::from("/Users/cedar/Library/Logs/FastKraken")));
        assert_eq!(folder_for(None, None), None);
        assert_eq!(folder(), None);
    }

    #[test]
    fn the_macos_version_is_read_from_the_system_version_file() {
        let plist = "<dict>\n\t<key>ProductName</key>\n\t<string>macOS</string>\n\t<key>ProductVersion</key>\n\t<string>26.1</string>\n</dict>";
        assert_eq!(product_version(plist).as_deref(), Some("26.1"));
        assert_eq!(product_version("<dict></dict>"), None);
        assert_eq!(product_version("<key>ProductVersion</key><string></string>"), None);
        assert!(system().contains(std::env::consts::ARCH));
    }

    #[test]
    fn a_report_names_the_version_the_problem_and_the_last_twenty_commands_with_no_credential() {
        let mut recent: Vec<_> = (0..25).map(|n| entry(&["git", "status", &format!("--n={n}")], 0, "")).collect();
        recent.push(entry(&["git", "fetch", "https://•••@example.invalid/beacon.git"], 128, "fatal: unable to access\nfatal: Authentication failed for 'https://•••@example.invalid/beacon.git/'"));
        let recent = &recent[recent.len() - RECENT..];
        let text = report(
            &crash("index out of range while reading https://cedar:hunter2@example.invalid/beacon.git with password=hunter2 and token ghp_abcdef1234567890"),
            recent,
        );
        assert!(text.starts_with("FastKraken problem report\n"), "{text}");
        assert!(text.contains(&format!("FastKraken {}\n", env!("CARGO_PKG_VERSION"))), "{text}");
        assert!(text.contains(&system()), "{text}");
        assert!(text.contains("Thread: main\n"), "{text}");
        assert!(text.contains("  index out of range while reading https://•••@example.invalid/beacon.git"), "{text}");
        assert!(text.contains("  at src/view.rs:12:5\n"), "{text}");
        assert!(text.contains("The last 20 Git commands"), "{text}");
        assert!(!text.contains("--n=5\n") && !text.contains("--n=5 ") && text.contains("--n=6 "), "{text}");
        assert!(text.contains("git fetch https://•••@example.invalid/beacon.git · exit 128 · 12 ms · in /tmp/Beacon"), "{text}");
        assert!(text.contains("      fatal: Authentication failed"), "{text}");
        assert!(text.contains("  1: main\n"), "{text}");
        assert!(text.trim_end().ends_with(STAYS), "{text}");
        for secret in ["hunter2", "cedar:", "abcdef1234567890"] {
            assert!(!text.contains(secret), "{secret} is in\n{text}");
        }
        assert!(report(&crash("stopped"), &[]).contains("  None ran.\n"));
    }

    #[test]
    fn tokens_are_masked_only_where_a_token_begins() {
        assert_eq!(masked("Bearer ghp_0123456789abcdef!"), "Bearer ghp_•••!");
        assert_eq!(masked("github_pat_11ABCDEFG0123456789"), "github_pat_•••");
        assert_eq!(masked("x-access-token ghs_short"), "x-access-token ghs_short");
        assert_eq!(masked("Thoughp_0123456789abcdef"), "Thoughp_0123456789abcdef");
        assert_eq!(masked("plain words"), "plain words");
    }

    #[test]
    fn reports_are_private_files_with_their_own_names_and_only_a_newer_one_is_announced() {
        let folder = Folder::new();
        let logs = folder.0.join("Logs");
        let at = UNIX_EPOCH + Duration::from_secs(1_795_000_100);
        assert_eq!(unannounced(&logs, None), None);
        let first = write_report(&logs, at, "one").unwrap();
        let second = write_report(&logs, at, "two").unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "one");
        assert_eq!(fs::metadata(&first).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&logs).unwrap().permissions().mode() & 0o777, 0o700);
        let name = first.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(name, format!("crash-{}.txt", file_stamp(at)));
        fs::write(logs.join(ACTIVITY_LOG), "kept").unwrap();
        fs::write(logs.join("notes.txt"), "kept").unwrap();
        assert_eq!(reports(&logs), [first.clone(), second.clone()]);
        assert_eq!(unannounced(&logs, None), Some(second.clone()));
        assert_eq!(unannounced(&logs, Some(&name)), Some(second.clone()));
        let newest = second.file_name().unwrap().to_str().unwrap();
        assert_eq!(unannounced(&logs, Some(newest)), None);
        let later = write_report(&logs, at + Duration::from_secs(3600), "three").unwrap();
        assert_eq!(unannounced(&logs, Some(newest)), Some(later.clone()));
        let shown = announcement(&later);
        assert_eq!((shown.tone, shown.title.as_str()), (Tone::Info, RECORDED));
        assert!(shown.detail.contains("nothing was sent"), "{}", shown.detail);
        let [(label, view::Action::Handoff(handoff::Action::RevealSaved(path)))] = shown.actions.as_slice() else {
            panic!("{:?}", shown.actions);
        };
        assert_eq!((label.as_ref(), path), (REVEAL_REPORT, &later));
    }

    #[test]
    fn the_activity_log_is_written_whole_into_the_logs_folder() {
        let folder = Folder::new();
        let logs = folder.0.join("Logs");
        let recent = [entry(&["git", "commit", "-q", "-F", "-"], 0, ""), entry(&["git", "push", "origin"], 1, "error: failed to push some refs to 'https://cedar:hunter2@example.invalid/x.git'")];
        let text = activity_log(&recent, UNIX_EPOCH + Duration::from_secs(1_795_000_100));
        assert!(text.contains("git commit -q -F - · exit 0"), "{text}");
        assert!(text.contains("error: failed to push some refs to 'https://•••@example.invalid/x.git'"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        let path = write_activity(&logs, &text).unwrap();
        assert_eq!(path, logs.join(ACTIVITY_LOG));
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        write_activity(&logs, "again").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "again");
        assert_eq!(fs::read_dir(&logs).unwrap().count(), 1);
    }

    #[test]
    fn reveal_logs_sits_in_the_help_menu() {
        let command = crate::commands::find("app.logs").unwrap();
        assert_eq!(command.bar, Some(Bar::Help));
        assert_eq!(crate::commands::title_case("Reveal logs"), "Reveal Logs");
    }
}
