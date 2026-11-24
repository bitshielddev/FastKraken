use crate::{
    Tab, askpass,
    auth::secret,
    notice::{Notice, Tone},
    prompt::{self, Answer, Spec},
    storage,
    view,
};
use gpui::Context;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;

pub(crate) const HELPER: &str = "--git-credential-keychain";
const SERVICE: &str = "FastKraken: ";
const INDEX: &str = "credentials.json";

const APPLE_HELPERS: &[&str] = &[
    "/Library/Developer/CommandLineTools/usr/libexec/git-core",
    "/Applications/Xcode.app/Contents/Developer/usr/libexec/git-core",
];


fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

pub(crate) fn configure(command: &mut Command) {
    let Some(program) = askpass::program() else { return };
    command.arg("-c").arg(format!("credential.helper=!exec {} {HELPER}", quoted(&program)));
}

fn apple_helper() -> Option<PathBuf> {
    if cfg!(test) || crate::acceptance::replaying() {
        return None;
    }
    crate::environment::host_dirs()
        .iter()
        .map(PathBuf::as_path)
        .chain(APPLE_HELPERS.iter().map(Path::new))
        .map(|dir| dir.join("git-credential-osxkeychain"))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}


#[derive(Debug, Default, PartialEq, Eq)]
struct Request {
    protocol: String,
    host: String,
    username: Option<String>,
    password: Option<String>,
}

impl Request {
    fn read(input: impl BufRead) -> Result<Self> {
        let mut fields = BTreeMap::new();
        for line in input.take(16 * 1024).lines() {
            let line = line.map_err(|_| "Invalid credential request.")?;
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once('=') {
                if matches!(key, "protocol" | "host" | "username" | "password") && fields.insert(key.to_owned(), value.to_owned()).is_some() {
                    return Err("A credential request named a field twice.".into());
                }
            }
        }
        Ok(Self {
            protocol: fields.remove("protocol").unwrap_or_default(),
            host: fields.remove("host").unwrap_or_default(),
            username: fields.remove("username").filter(|user| !user.is_empty()),
            password: fields.remove("password"),
        })
    }

    fn web(&self) -> bool {
        matches!(self.protocol.as_str(), "https" | "http") && !self.host.is_empty()
    }
}

pub(crate) fn helper(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let verb = args.next().and_then(|verb| verb.into_string().ok()).ok_or("Missing credential operation.")?;
    if args.next().is_some() {
        return Err("Unexpected credential arguments.".into());
    }
    let saved = Saved::current()?;
    let apple = apple_helper();
    answer(&verb, io::stdin().lock(), &mut io::stdout().lock(), &saved, askpass::approved, apple.as_deref())
}

fn answer(
    verb: &str,
    input: impl BufRead,
    out: &mut impl Write,
    saved: &Saved,
    approve: impl Fn(&askpass::Credential) -> bool,
    apple: Option<&Path>,
) -> Result<()> {
    let request = Request::read(input)?;
    if !request.web() {
        return Ok(());
    }
    match verb {
        "get" => {
            let found = match saved.find(&request.protocol, &request.host, request.username.as_deref())? {
                Some(found) => Some(found),
                None => apple.and_then(|apple| ask_apple(apple, "get", &request)),
            };
            let Some((username, password)) = found else { return Ok(()) };
            writeln!(out, "username={username}\npassword={password}\n").map_err(|_| "Cannot return the credential to Git.".into())
        }
        "store" => {
            let (Some(username), Some(password)) = (request.username, request.password) else { return Ok(()) };
            let credential = askpass::Credential { protocol: request.protocol, host: request.host, username, password };
            if approve(&credential) { saved.put(&credential) } else { Ok(()) }
        }
        "erase" => {
            if let Some(apple) = apple {
                ask_apple(apple, "erase", &request);
            }
            saved.erase(&request.protocol, &request.host, request.username.as_deref(), request.password.as_deref()).map(drop)
        }
        _ => Ok(()),
    }
}

fn ask_apple(apple: &Path, verb: &str, request: &Request) -> Option<(String, String)> {
    use std::process::Stdio;
    let mut sent = format!("protocol={}\nhost={}\n", request.protocol, request.host);
    for (key, value) in [("username", &request.username), ("password", &request.password)] {
        if let Some(value) = value.as_deref().filter(|value| one_line(value)) {
            sent.push_str(&format!("{key}={value}\n"));
        }
    }
    sent.push('\n');
    let mut child = Command::new(apple).arg(verb).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let writer = std::thread::spawn(move || stdin.write_all(sent.as_bytes()));
    let output = child.wait_with_output().ok()?;
    let _ = writer.join();
    if !output.status.success() {
        return None;
    }
    let answer = Request::read(output.stdout.as_slice()).ok()?;
    match (answer.username, answer.password) {
        (Some(username), Some(password)) if request.username.as_ref().is_none_or(|wanted| *wanted == username) => Some((username, password)),
        _ => None,
    }
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub protocol: String,
    pub host: String,
    pub username: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Index {
    #[serde(deserialize_with = "storage::lenient_list")]
    saved: Vec<Entry>,
}

fn service(host: &str) -> String {
    format!("{SERVICE}{host}")
}

fn one_line(value: &str) -> bool {
    !value.contains(['\n', '\r', '\0'])
}

pub(crate) struct Saved {
    dir: PathBuf,
}

impl Saved {
    pub(crate) fn current() -> Result<Self> {
        Ok(Self { dir: storage::directory()? })
    }

    #[cfg(test)]
    fn at(dir: &Path) -> Self {
        Self { dir: dir.to_owned() }
    }

    fn change(&self, change: impl FnOnce(&mut Vec<Entry>)) -> Result<()> {
        storage::update(&self.dir, INDEX, "credentials.lock", "saved credentials", |index: &mut Index| {
            change(&mut index.saved);
            Ok(())
        })
    }

    pub(crate) fn list(&self) -> Result<Vec<Entry>> {
        Ok(storage::load::<Index>(&self.dir, INDEX, "saved credentials")?.saved)
    }

    pub(crate) fn find(&self, protocol: &str, host: &str, username: Option<&str>) -> Result<Option<(String, String)>> {
        let entries = self.list()?;
        let candidates = entries
            .iter()
            .rev()
            .filter(|entry| entry.protocol == protocol && entry.host == host && username.is_none_or(|user| user == entry.username));
        for entry in candidates {
            if let Some(bytes) = secret::get(&service(host), &entry.username)? {
                let Ok(password) = String::from_utf8(bytes) else { continue };
                if one_line(&entry.username) && one_line(&password) {
                    return Ok(Some((entry.username.clone(), password)));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn put(&self, credential: &askpass::Credential) -> Result<()> {
        if !one_line(&credential.username) || !one_line(&credential.password) || credential.username.is_empty() {
            return Err("That credential cannot be saved.".into());
        }
        secret::put(&service(&credential.host), &credential.username, credential.password.as_bytes())?;
        let entry = Entry { protocol: credential.protocol.clone(), host: credential.host.clone(), username: credential.username.clone() };
        self.change(|saved| {
            saved.retain(|other| *other != entry);
            saved.push(entry);
        })
    }

    pub(crate) fn erase(&self, protocol: &str, host: &str, username: Option<&str>, password: Option<&str>) -> Result<bool> {
        let mut gone = Vec::new();
        for entry in self.list()? {
            if entry.protocol != protocol || entry.host != host || username.is_some_and(|user| user != entry.username) {
                continue;
            }
            let Some(saved) = secret::get(&service(host), &entry.username)? else {
                gone.push(entry);
                continue;
            };
            if password.is_none_or(|password| password.as_bytes() == saved.as_slice()) {
                secret::remove(&service(host), &entry.username)?;
                gone.push(entry);
            }
        }
        if gone.is_empty() {
            return Ok(false);
        }
        self.change(|saved| saved.retain(|entry| !gone.contains(entry)))?;
        Ok(true)
    }

    pub(crate) fn forget(&self, entry: &Entry) -> Result<()> {
        let shared = self.list()?.iter().any(|other| other != entry && other.host == entry.host && other.username == entry.username);
        if !shared {
            secret::remove(&service(&entry.host), &entry.username)?;
        }
        self.change(|saved| saved.retain(|other| other != entry))
    }
}


pub(crate) const COMMANDS: &[crate::commands::Command] = &[crate::commands::Command::new(
    "app.forget-credentials",
    crate::commands::Group::App,
    |_, _| "Forget saved credentials…".into(),
    |_, target, _| {
        use crate::commands::{Availability, Target};
        if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
    },
    |_, _| Some(view::Action::Preferences(crate::preferences::Action::Open(Some(crate::preferences::Section::Accounts)))),
)
.words(&["passwords", "keychain", "sign out", "usernames", "tokens", "remember"])];

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Forget,
    ForgetOne(Entry),
}

#[derive(Debug)]
pub(crate) struct Ask {
    entries: Vec<Entry>,
}

impl Ask {
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

pub(crate) fn named(entry: &Entry) -> String {
    match entry.protocol.as_str() {
        "https" => format!("{} ({})", entry.host, entry.username),
        protocol => format!("{protocol}://{} ({})", entry.host, entry.username),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::ForgetOne(_) if tab.ui.prompt.open() => {}
        Action::ForgetOne(entry) => {
            prompt::ask(&mut tab.ui, prompt::Kind::Credentials(Ask { entries: vec![entry] }), cx);
            cx.notify();
        }
        Action::Forget => Tab::work(
            cx,
            || Saved::current().and_then(|saved| saved.list()),
            |tab, listed, cx| {
                match listed {
                    Err(error) => tab.error(format!("Saved credentials — {error}"), cx),
                    Ok(entries) if entries.is_empty() => {
                        tab.ui.notices.show(Notice::new(Tone::Info, "No saved credentials", "FastKraken has not saved a password for any host."));
                    }
                    Ok(entries) if tab.ui.prompt.open() => drop(entries),
                    Ok(mut entries) => {
                        entries.reverse();
                        prompt::ask(&mut tab.ui, prompt::Kind::Credentials(Ask { entries }), cx);
                    }
                }
                cx.notify();
            },
        ),
    }
}

pub(crate) fn describe(ask: &Ask) -> Spec {
    let mut spec = Spec::new("Forget saved credentials?", if ask.entries.len() == 1 { "Forget it" } else { "Forget all" })
        .line("FastKraken keeps these passwords in your Keychain. Forget one, or all of them, and Git asks for it again the next time it connects.")
        .danger();
    for entry in &ask.entries {
        spec = spec.choice(format!("Forget {}", named(entry)), true);
    }
    if ask.entries.len() == 1 {
        spec.choices.clear();
        spec = spec.line(named(&ask.entries[0]));
    }
    spec
}

pub(crate) fn confirm(_: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    let chosen: Vec<Entry> = match answer.choice {
        Some(i) => ask.entries.get(i).cloned().into_iter().collect(),
        None => ask.entries,
    };
    if chosen.is_empty() {
        return;
    }
    Tab::work(
        cx,
        move || {
            let saved = Saved::current()?;
            for entry in &chosen {
                saved.forget(entry)?;
            }
            Ok::<_, String>(chosen)
        },
        |tab, forgot, cx| {
            match forgot {
                Ok(forgot) => {
                    let title = match forgot.as_slice() {
                        [one] => format!("Forgot the password for {}", one.host),
                        all => format!("Forgot {} saved passwords", all.len()),
                    };
                    tab.ui.notices.show(Notice::new(Tone::Success, title, "Git asks for it again the next time it connects."));
                }
                Err(error) => tab.error(format!("Forget Failed — {error}"), cx),
            }
            crate::preferences::saved_changed(tab, cx);
            cx.notify();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("fastkraken-credentials-test-").tempdir().unwrap()
    }

    fn credential(host: &str, username: &str, password: &str) -> askpass::Credential {
        askpass::Credential { protocol: "https".into(), host: host.into(), username: username.into(), password: password.into() }
    }

    fn run(saved: &Saved, verb: &str, input: &str, approve: bool) -> String {
        let mut out = Vec::new();
        answer(verb, input.as_bytes(), &mut out, saved, |_| approve, None).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn get_returns_what_is_saved_for_the_host_and_nothing_for_another() {
        let dir = folder();
        let saved = Saved::at(dir.path());
        let host = "get.trail.test";
        assert_eq!(run(&saved, "get", &format!("protocol=https\nhost={host}\n\n"), false), "");
        saved.put(&credential(host, "cedar", "trail-pass")).unwrap();
        assert_eq!(run(&saved, "get", &format!("protocol=https\nhost={host}\n\n"), false), "username=cedar\npassword=trail-pass\n\n");
        assert_eq!(run(&saved, "get", &format!("protocol=https\nhost={host}\nusername=cedar\n\n"), false), "username=cedar\npassword=trail-pass\n\n");
        assert_eq!(run(&saved, "get", &format!("protocol=https\nhost={host}\nusername=scout\n\n"), false), "");
        for other in [
            "protocol=https\nhost=other.trail.test\n\n".to_owned(),
            format!("protocol=https\nhost={host}:8443\n\n"),
            format!("protocol=http\nhost={host}\n\n"),
            format!("protocol=cert\nhost={host}\n\n"),
        ] {
            assert_eq!(run(&saved, "get", &other, false), "", "{other}");
        }
        assert_eq!(secret::get(&format!("FastKraken: {host}"), "cedar").unwrap().as_deref(), Some(b"trail-pass".as_slice()));
        let index = std::fs::read_to_string(dir.path().join(INDEX)).unwrap();
        assert!(index.contains(host) && !index.contains("trail-pass"), "{index}");
        let mut out = Vec::new();
        assert!(answer("get", format!("protocol=https\nhost={host}\nhost=evil.test\n\n").as_bytes(), &mut out, &saved, |_| false, None).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn store_saves_only_what_the_job_approves() {
        let dir = folder();
        let saved = Saved::at(dir.path());
        let host = "store.trail.test";
        let accepted = format!("protocol=https\nhost={host}\nusername=cedar\npassword=trail-pass\n\n");
        run(&saved, "store", &accepted, false);
        assert_eq!(saved.find("https", host, None).unwrap(), None);
        assert!(saved.list().unwrap().is_empty());
        let asked = std::cell::RefCell::new(Vec::new());
        let mut out = Vec::new();
        answer("store", accepted.as_bytes(), &mut out, &saved, |credential| {
            asked.borrow_mut().push(credential.clone());
            true
        }, None)
        .unwrap();
        assert_eq!(asked.into_inner(), [credential(host, "cedar", "trail-pass")]);
        assert_eq!(saved.find("https", host, None).unwrap(), Some(("cedar".into(), "trail-pass".into())));
        assert_eq!(saved.list().unwrap(), [Entry { protocol: "https".into(), host: host.into(), username: "cedar".into() }]);
        run(&saved, "store", &format!("protocol=https\nhost={host}\nusername=cedar\npassword=new-pass\n\n"), true);
        assert_eq!(saved.find("https", host, None).unwrap(), Some(("cedar".into(), "new-pass".into())));
        assert_eq!(saved.list().unwrap().len(), 1);
        assert!(saved.put(&credential(host, "cedar", "two\nlines")).is_err());
    }

    #[test]
    fn erase_removes_the_refused_password_and_keeps_another() {
        let dir = folder();
        let saved = Saved::at(dir.path());
        let host = "erase.trail.test";
        saved.put(&credential(host, "cedar", "trail-pass")).unwrap();
        run(&saved, "erase", &format!("protocol=https\nhost={host}\nusername=cedar\npassword=typed-wrong\n\n"), false);
        assert_eq!(saved.find("https", host, None).unwrap(), Some(("cedar".into(), "trail-pass".into())));
        run(&saved, "erase", &format!("protocol=https\nhost={host}\nusername=cedar\npassword=trail-pass\n\n"), false);
        assert_eq!(saved.find("https", host, None).unwrap(), None);
        assert_eq!(secret::get(&format!("FastKraken: {host}"), "cedar").unwrap(), None);
        assert!(saved.list().unwrap().is_empty());
    }

    #[test]
    fn forget_removes_the_chosen_host_only() {
        let dir = folder();
        let saved = Saved::at(dir.path());
        saved.put(&credential("forget-a.trail.test", "cedar", "a-pass")).unwrap();
        saved.put(&credential("forget-b.trail.test", "scout", "b-pass")).unwrap();
        let entries = saved.list().unwrap();
        assert_eq!(entries.len(), 2);
        saved.forget(&entries[0]).unwrap();
        assert_eq!(saved.find("https", "forget-a.trail.test", None).unwrap(), None);
        assert_eq!(secret::get("FastKraken: forget-a.trail.test", "cedar").unwrap(), None);
        assert_eq!(saved.find("https", "forget-b.trail.test", None).unwrap(), Some(("scout".into(), "b-pass".into())));
        let ask = Ask { entries: saved.list().unwrap() };
        let mut spec = describe(&ask);
        assert!(spec.danger && spec.choices.is_empty() && spec.lines.last().map(String::as_str) == Some("forget-b.trail.test (scout)"));
        let two = Ask { entries: vec![entries[0].clone(), Entry { protocol: "http".into(), ..entries[1].clone() }] };
        spec = describe(&two);
        let labels: Vec<_> = spec.choices.iter().map(|choice| (choice.label.to_string(), choice.danger)).collect();
        assert_eq!(labels, [("Forget forget-a.trail.test (cedar)".into(), true), ("Forget http://forget-b.trail.test (scout)".into(), true)]);
        assert_eq!(spec.confirm.as_ref(), "Forget all");
    }

    #[test]
    fn apples_helper_is_asked_for_what_is_not_saved_and_told_of_refusals_but_never_stores() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = folder();
        let saved = Saved::at(dir.path());
        let host = "apple.trail.test";
        let log = dir.path().join("apple.log");
        let apple = dir.path().join("git-credential-osxkeychain");
        std::fs::write(
            &apple,
            format!("#!/bin/sh\n{{ echo \"$1\"; cat; }} >> '{}'\n[ \"$1\" = get ] && printf 'username=apple\\npassword=apple-pass\\n'\nexit 0\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&apple, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run = |verb: &str, input: String| {
            let mut out = Vec::new();
            answer(verb, input.as_bytes(), &mut out, &saved, |_| true, Some(&apple)).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(run("get", format!("protocol=https\nhost={host}\n\n")), "username=apple\npassword=apple-pass\n\n");
        assert_eq!(run("get", format!("protocol=https\nhost={host}\nusername=cedar\n\n")), "");
        run("store", format!("protocol=https\nhost={host}\nusername=cedar\npassword=trail-pass\n\n"));
        assert_eq!(saved.find("https", host, None).unwrap(), Some(("cedar".into(), "trail-pass".into())));
        assert_eq!(run("get", format!("protocol=https\nhost={host}\n\n")), "username=cedar\npassword=trail-pass\n\n");
        run("erase", format!("protocol=https\nhost={host}\nusername=apple\npassword=apple-pass\n\n"));
        let asked = std::fs::read_to_string(&log).unwrap();
        let verbs: Vec<&str> = asked.lines().filter(|line| matches!(*line, "get" | "store" | "erase")).collect();
        assert_eq!(verbs, ["get", "get", "erase"], "{asked}");
        assert!(asked.contains(&format!("erase\nprotocol=https\nhost={host}\nusername=apple\npassword=apple-pass\n")), "{asked}");
        assert!(!asked.contains("trail-pass"), "{asked}");
    }

    #[test]
    fn network_commands_register_the_helper_after_githubs_and_only_with_a_program() {
        let mut plain = Command::new("git");
        configure(&mut plain);
        assert_eq!(plain.get_args().count(), 0);
        let _program = askpass::tests::Program::set(PathBuf::from("/Apps/Fast Kraken.app/Contents/MacOS/fastkraken"));
        let mut command = Command::new("git");
        configure(&mut command);
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-c", "credential.helper=!exec '/Apps/Fast Kraken.app/Contents/MacOS/fastkraken' --git-credential-keychain"]);
    }
}
