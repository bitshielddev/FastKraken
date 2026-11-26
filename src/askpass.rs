use crate::{
    Tab,
    prompt::{self, Answer, Field, Note, Spec},
    view::{self, Desk},
};
use gpui::{Context, Window};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock, Weak, mpsc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) const SOCKET: &str = "FASTKRAKEN_ASKPASS_SOCKET";
pub(crate) const TOKEN: &str = "FASTKRAKEN_ASKPASS_TOKEN";
const LIMIT: u64 = 16 * 1024;
const LONGEST: usize = 2000;

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    remember: Option<Credential>,
    #[serde(default)]
    asker: Option<u32>,
}

#[derive(Default, Serialize, Deserialize)]
struct Reply {
    #[serde(default)]
    answer: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Credential {
    pub protocol: String,
    pub host: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}@{} (password hidden)", self.protocol, self.username, self.host)
    }
}


pub(crate) fn helper(question: &OsStr) -> i32 {
    let socket = env::var_os(SOCKET).unwrap_or_default();
    let token = env::var(TOKEN).unwrap_or_default();
    match ask(Path::new(&socket), &token, &question.to_string_lossy()) {
        Some(answer) => {
            let mut out = std::io::stdout().lock();
            if writeln!(out, "{answer}").and_then(|_| out.flush()).is_ok() { 0 } else { 1 }
        }
        None => 1,
    }
}

pub(crate) fn ask(socket: &Path, token: &str, question: &str) -> Option<String> {
    if token.is_empty() || socket.as_os_str().is_empty() {
        return None;
    }
    let asker = u32::try_from(unsafe { libc::getppid() }).ok();
    exchange(socket, &Request { token: token.to_owned(), prompt: Some(question.to_owned()), remember: None, asker })
}

pub(crate) fn approved(credential: &Credential) -> bool {
    let socket = env::var_os(SOCKET).unwrap_or_default();
    let token = env::var(TOKEN).unwrap_or_default();
    approved_at(Path::new(&socket), &token, credential)
}

fn approved_at(socket: &Path, token: &str, credential: &Credential) -> bool {
    if token.is_empty() || socket.as_os_str().is_empty() {
        return false;
    }
    let request = Request { token: token.to_owned(), prompt: None, remember: Some(credential.clone()), asker: None };
    exchange(socket, &request).as_deref() == Some("yes")
}

fn exchange(socket: &Path, request: &Request) -> Option<String> {
    let mut stream = UnixStream::connect(socket).ok()?;
    let mut line = serde_json::to_vec(request).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    let mut reply = String::new();
    BufReader::new(&stream).take(LIMIT).read_line(&mut reply).ok()?;
    let answer = serde_json::from_str::<Reply>(&reply).ok()?.answer?;
    (!answer.contains(['\n', '\r', '\0'])).then_some(answer)
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Question {
    SignIn { protocol: String, host: String, username: Option<String> },
    Passphrase { key: String },
    HostKey { host: String, kind: Option<String>, fingerprint: Option<String> },
    Confirm { text: String },
    Other { text: String },
}

impl Question {
    pub(crate) fn parse(prompt: &str) -> Self {
        let text = plain(prompt);
        let trimmed = text.trim();
        for (what, password) in [("Username for '", false), ("Password for '", true)] {
            if let Some(url) = trimmed.strip_prefix(what).and_then(|rest| rest.strip_suffix("':")) {
                if let Some((protocol, user, host)) = address(url) {
                    return Self::SignIn { protocol, host, username: if password { user } else { None } };
                }
            }
        }
        if let Some(key) = between(trimmed, "Enter passphrase for key '", "'") {
            return Self::Passphrase { key };
        }
        if let Some(rest) = trimmed.strip_prefix("Enter passphrase for ") {
            let key = rest.trim_end_matches(':').trim().trim_matches(['\'', '"']);
            if key.starts_with('/') || key.starts_with('~') {
                return Self::Passphrase { key: key.to_owned() };
            }
        }
        if trimmed.contains("(yes/no") {
            if let Some(named) = between(trimmed, "The authenticity of host '", "'") {
                let host = named.split(" (").next().unwrap_or(&named).to_owned();
                let (kind, fingerprint) = trimmed
                    .lines()
                    .find_map(|line| line.split_once(" key fingerprint is "))
                    .map(|(kind, print)| (Some(kind.trim().to_owned()), Some(print.trim().trim_end_matches('.').to_owned())))
                    .unwrap_or((None, None));
                return Self::HostKey { host, kind, fingerprint };
            }
            return Self::Confirm { text: trimmed.to_owned() };
        }
        Self::Other { text: trimmed.to_owned() }
    }
}

fn plain(prompt: &str) -> String {
    prompt.chars().filter(|c| *c == '\n' || !c.is_control()).take(LONGEST).collect()
}

fn between(text: &str, start: &str, end: &str) -> Option<String> {
    let rest = &text[text.find(start)? + start.len()..];
    Some(rest[..rest.find(end)?].to_owned()).filter(|found| !found.is_empty())
}

fn address(url: &str) -> Option<(String, Option<String>, String)> {
    let (protocol, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    let (user, host) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_owned()).filter(|user| !user.is_empty()), host),
        None => (None, authority),
    };
    (!protocol.is_empty() && !host.is_empty()).then(|| (protocol.to_owned(), user, host.to_owned()))
}


pub(crate) struct Broker {
    path: PathBuf,
    sessions: Mutex<HashMap<String, Weak<Session>>>,
}

static GLOBAL: OnceLock<Option<Arc<Broker>>> = OnceLock::new();

impl Broker {
    pub(crate) fn global() -> Option<&'static Arc<Self>> {
        GLOBAL
            .get_or_init(|| match Self::listen(&home()) {
                Ok(broker) => Some(broker),
                Err(error) => {
                    eprintln!("FastKraken cannot ask for credentials: {error}");
                    None
                }
            })
            .as_ref()
    }

    pub(crate) fn listen(dir: &Path) -> Result<Arc<Self>, String> {
        fs::create_dir_all(dir).map_err(|e| format!("Create {}: {e}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| format!("Protect {}: {e}", dir.display()))?;
        sweep(dir);
        let path = dir.join(std::process::id().to_string());
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|e| format!("Listen at {}: {e}", path.display()))?;
        let broker = Arc::new(Self { path, sessions: Mutex::new(HashMap::new()) });
        let weak = Arc::downgrade(&broker);
        thread::Builder::new()
            .name("askpass".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Some(broker) = weak.upgrade() else { break };
                    if let Ok(stream) = stream {
                        let _ = thread::Builder::new().name("askpass-question".into()).spawn(move || broker.serve(stream));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(broker)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn session(self: &Arc<Self>) -> Arc<Session> {
        let session = Arc::new(Session {
            token: token(),
            socket: self.path.clone(),
            broker: Arc::downgrade(self),
            talk: Mutex::new(Talk::default()),
            touched: Mutex::new(Instant::now()),
        });
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(session.token.clone(), Arc::downgrade(&session));
        session
    }

    fn find(&self, token: &str) -> Option<Arc<Session>> {
        if token.is_empty() {
            return None;
        }
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(token).and_then(Weak::upgrade)
    }

    fn serve(&self, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        if BufReader::new(&stream).take(LIMIT).read_line(&mut line).is_err() {
            return;
        }
        let Ok(request) = serde_json::from_str::<Request>(&line) else { return };
        let answer = match self.find(&request.token) {
            None => None,
            Some(session) => match (request.prompt, request.remember) {
                (_, Some(credential)) => session.approves(&credential).then(|| "yes".to_owned()),
                (Some(prompt), None) => session.answer(Question::parse(&prompt), request.asker, &stream),
                (None, None) => None,
            },
        };
        let mut reply = serde_json::to_vec(&Reply { answer }).unwrap_or_default();
        reply.push(b'\n');
        let _ = stream.write_all(&reply);
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<libc::pid_t>().ok()) else { continue };
        let ended = unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if ended {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub(crate) fn stop() {
    if let Some(Some(broker)) = GLOBAL.get() {
        let _ = fs::remove_file(&broker.path);
    }
}

fn home() -> PathBuf {
    if !cfg!(test) {
        if let Ok(dir) = crate::storage::directory() {
            let dir = dir.join("ask");
            if dir.as_os_str().len() + 12 < 104 {
                return dir;
            }
        }
    }
    env::temp_dir().join(format!("fastkraken-ask-{}", unsafe { libc::getuid() }))
}

fn token() -> String {
    let mut bytes = [0u8; 16];
    let read = fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    if read.is_err() {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let seed = now ^ (u128::from(std::process::id()) << 64) ^ u128::from(SERIAL.fetch_add(1, Ordering::Relaxed));
        bytes = seed.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) struct Session {
    token: String,
    socket: PathBuf,
    broker: Weak<Broker>,
    talk: Mutex<Talk>,
    touched: Mutex<Instant>,
}

#[derive(Default)]
struct Talk {
    next: u64,
    pending: Option<Pending>,
    entered: Option<Credential>,
    remember: bool,
    tries: HashMap<(String, Option<u32>), u32>,
    declined: bool,
    shown: u32,
}

struct Pending {
    id: u64,
    question: Question,
    tries: u32,
    shown: bool,
    reply: mpsc::Sender<Option<String>>,
}

static SHOWN: AtomicU64 = AtomicU64::new(0);

pub(crate) fn shown_so_far() -> u64 {
    SHOWN.load(Ordering::SeqCst)
}

impl Session {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    fn talk(&self) -> std::sync::MutexGuard<'_, Talk> {
        self.talk.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn touch(&self) {
        *self.touched.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    pub(crate) fn touched(&self) -> Instant {
        *self.touched.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn declined(&self) -> bool {
        self.talk().declined
    }

    pub(crate) fn waiting(&self) -> bool {
        self.talk().pending.is_some()
    }

    pub(crate) fn waits_for(&self, id: u64) -> bool {
        self.talk().pending.as_ref().is_some_and(|pending| pending.id == id)
    }

    #[cfg(test)]
    pub(crate) fn shown(&self) -> u32 {
        self.talk().shown
    }

    fn unshown(&self) -> Option<(u64, Question, u32)> {
        let mut talk = self.talk();
        let pending = talk.pending.as_mut().filter(|pending| !pending.shown)?;
        pending.shown = true;
        let found = (pending.id, pending.question.clone(), pending.tries);
        talk.shown += 1;
        SHOWN.fetch_add(1, Ordering::SeqCst);
        Some(found)
    }

    fn known(&self, question: &Question) -> Option<String> {
        let talk = self.talk();
        let entered = talk.entered.as_ref()?;
        match question {
            Question::SignIn { protocol, host, username } if *protocol == entered.protocol && *host == entered.host => match username {
                Some(user) if *user == entered.username => Some(entered.password.clone()),
                Some(_) => None,
                None => Some(entered.username.clone()),
            },
            _ => None,
        }
    }

    fn answer(&self, question: Question, asker: Option<u32>, stream: &UnixStream) -> Option<String> {
        if let Some(known) = self.known(&question) {
            return Some(known);
        }
        let (tx, rx) = mpsc::channel();
        let id = {
            let mut talk = self.talk();
            talk.next += 1;
            let id = talk.next;
            let tries = match &question {
                Question::Passphrase { key } => {
                    let count = talk.tries.entry((key.clone(), asker)).or_default();
                    *count += 1;
                    *count - 1
                }
                _ => 0,
            };
            talk.pending = Some(Pending { id, question, tries, shown: false, reply: tx });
            id
        };
        self.touch();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(answer) => {
                    self.touch();
                    return answer;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) if gone(stream) => {
                    self.withdraw(id);
                    return None;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn withdraw(&self, id: u64) {
        let mut talk = self.talk();
        if talk.pending.as_ref().is_some_and(|pending| pending.id == id) {
            talk.pending = None;
        }
    }

    fn reply(&self, id: u64, answer: Option<String>) -> bool {
        let mut talk = self.talk();
        let Some(pending) = talk.pending.take_if(|pending| pending.id == id) else { return false };
        if answer.is_none() {
            talk.declined = true;
        }
        pending.reply.send(answer).is_ok()
    }

    pub(crate) fn sign_in(&self, id: u64, credential: Credential, remember: bool) -> bool {
        let reply = {
            let mut talk = self.talk();
            let fixed = match talk.pending.as_ref().filter(|p| p.id == id).map(|p| &p.question) {
                Some(Question::SignIn { username, .. }) => username.is_some(),
                _ => return false,
            };
            let reply = if fixed { credential.password.clone() } else { credential.username.clone() };
            talk.entered = Some(credential);
            talk.remember = remember;
            reply
        };
        self.reply(id, Some(reply))
    }

    pub(crate) fn respond(&self, id: u64, answer: String) -> bool {
        self.reply(id, Some(answer))
    }

    pub(crate) fn decline(&self, id: u64) -> bool {
        self.reply(id, None)
    }

    fn approves(&self, credential: &Credential) -> bool {
        let talk = self.talk();
        talk.remember && talk.entered.as_ref() == Some(credential)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.upgrade() {
            broker.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.token);
        }
    }
}

fn gone(stream: &UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let gone = match (&*stream).read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => !matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted),
    };
    let _ = stream.set_nonblocking(false);
    gone
}


#[cfg(test)]
thread_local! {
    static TEST_PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn use_program(program: Option<PathBuf>) {
    TEST_PROGRAM.with(|current| *current.borrow_mut() = program);
}

pub(crate) fn program() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_PROGRAM.with(|current| current.borrow().clone())
    }
    #[cfg(not(test))]
    {
        env::current_exe().ok()
    }
}

pub(crate) fn configure(command: &mut Command, session: Option<&Session>) {
    let Some(program) = program() else { return };
    command
        .env("GIT_ASKPASS", &program)
        .env("SSH_ASKPASS", &program)
        .env("SSH_ASKPASS_REQUIRE", "force");
    match session {
        Some(session) => command.env(SOCKET, session.socket()).env(TOKEN, session.token()),
        None => command.env(SOCKET, "").env_remove(TOKEN),
    };
}


pub(crate) struct Ask {
    session: Arc<Session>,
    id: u64,
    question: Question,
    tries: u32,
    connect: bool,
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask").field("id", &self.id).field("question", &self.question).finish()
    }
}

pub(crate) fn poll(tab: &mut Tab, cx: &mut Context<Tab>) -> bool {
    if let Some(prompt::Kind::Askpass(ask)) = tab.ui.prompt.asked() {
        if ask.session.waits_for(ask.id) {
            return false;
        }
        prompt::withdraw(&mut tab.ui, cx);
        return true;
    }
    if tab.ui.prompt.open() {
        return false;
    }
    let jobs = [tab.ui.jobs.network.as_ref(), tab.ui.jobs.write.as_ref()];
    let Some((kind, session, (id, question, tries))) = jobs.into_iter().flatten().find_map(|job| {
        let session = job.link.session()?;
        Some((job.spec.kind, session.clone(), session.unshown()?))
    }) else {
        return false;
    };
    let connect = !matches!(kind, crate::jobs::Kind::Clone | crate::jobs::Kind::Write) && tab.repo.is_some();
    prompt::ask(&mut tab.ui, prompt::Kind::Askpass(Ask { session, id, question, tries, connect }), cx);
    true
}

pub(crate) fn token_page(host: &str) -> Option<&'static str> {
    match host {
        "github.com" => Some("https://github.com/settings/tokens"),
        "gitlab.com" => Some("https://gitlab.com/-/user_settings/personal_access_tokens"),
        "bitbucket.org" => Some("https://bitbucket.org/account/settings/app-passwords/"),
        _ => None,
    }
}

fn key_name(key: &str) -> &str {
    key.rsplit('/').next().filter(|name| !name.is_empty()).unwrap_or(key)
}

pub(crate) fn describe(ask: &Ask) -> Spec {
    let mut spec = match &ask.question {
        Question::SignIn { protocol, host, username } => {
            let mut spec = Spec::new(format!("Sign in to {host}"), "Sign in");
            match username {
                Some(user) => {
                    spec = spec.line(format!("Git needs the password for {user} at {protocol}://{host}."));
                }
                None => {
                    spec = spec.line(format!("Git needs a username and password for {protocol}://{host}."));
                    spec.field = Some(Field {
                        label: "Username",
                        placeholder: "Username",
                        initial: String::new(),
                        masked: false,
                        validate: Box::new(|text, _| text.is_empty().then(String::new)),
                    });
                }
            }
            let password = Field {
                label: "Password",
                placeholder: "Password",
                initial: String::new(),
                masked: true,
                validate: Box::new(|text, _| text.is_empty().then(String::new)),
            };
            if spec.field.is_none() {
                spec.field = Some(password);
            } else {
                spec.second = Some(password);
            }
            if let Some(page) = token_page(host) {
                spec.note = Some(Note {
                    text: "This host needs a personal access token, not your account password.".to_owned(),
                    link: Some(("Create a token".into(), view::Action::Handoff(crate::handoff::Action::OpenUrl(page.to_owned())))),
                });
            }
            if host == "github.com" && ask.connect {
                spec = spec.choice("Connect GitHub instead", false);
            }
            spec.tick = Some(("Remember in Keychain".into(), false));
            spec
        }
        Question::Passphrase { key } => {
            let mut spec = Spec::new(format!("Unlock SSH key {}", key_name(key)), "Unlock");
            if ask.tries > 0 {
                spec = spec.line("That passphrase did not work.");
            }
            spec = spec.line(format!("ssh needs the passphrase for {key}."));
            spec.field = Some(Field {
                label: "Passphrase",
                placeholder: "Passphrase",
                initial: String::new(),
                masked: true,
                validate: Box::new(|_, _| None),
            });
            spec
        }
        Question::HostKey { host, kind, fingerprint } => {
            let mut spec = Spec::new(format!("Connect to {host} for the first time?"), "Trust and connect")
                .line(format!("ssh has not connected to {host} before, so it cannot tell whether this is the server you mean."));
            if let Some(fingerprint) = fingerprint {
                spec = spec.line(format!("{} key fingerprint: {fingerprint}", kind.as_deref().unwrap_or("Its")));
            }
            spec.line("Compare it with the fingerprint the host publishes. Once trusted, the key is saved and not asked about again.")
        }
        Question::Confirm { text } => Spec::new("ssh asks", "Continue").line(text.clone()),
        Question::Other { text } => {
            let mut spec = Spec::new("Git needs an answer", "Answer").line(text.clone());
            spec.field = Some(Field {
                label: "Answer",
                placeholder: "Answer",
                initial: String::new(),
                masked: true,
                validate: Box::new(|_, _| None),
            });
            spec
        }
    };
    spec.unblocked = true;
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, window: &mut Window, cx: &mut Context<Tab>) {
    let Ask { session, id, question, connect: ask_connect, .. } = ask;
    match question {
        Question::SignIn { .. } if answer.choice == Some(0) && ask_connect => {
            session.decline(id);
            let remote = tab.ui.jobs.network.as_ref().and_then(|job| job.spec.remote.clone());
            let bind = crate::connect::Bind { remote, ..Default::default() };
            tab.dispatch(view::Action::Connect(crate::connect::Action::Use(bind)), window, cx);
        }
        Question::SignIn { protocol, host, username } => {
            let (user, password) = match username {
                Some(user) => (user, answer.text),
                None => (answer.text, answer.second),
            };
            session.sign_in(id, Credential { protocol, host, username: user, password }, answer.tick);
        }
        Question::HostKey { .. } | Question::Confirm { .. } => {
            session.respond(id, "yes".to_owned());
        }
        Question::Passphrase { .. } | Question::Other { .. } => {
            session.respond(id, answer.text);
        }
    }
}

pub(crate) fn cancelled(ask: Ask) {
    ask.session.decline(ask.id);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn broker() -> (tempfile::TempDir, Arc<Broker>) {
        let dir = tempfile::Builder::new().prefix("fk-ask").tempdir_in("/tmp").unwrap();
        let broker = Broker::listen(&dir.path().join("ask")).unwrap();
        (dir, broker)
    }

    fn shown(session: &Session) -> (u64, Question, u32) {
        let start = Instant::now();
        loop {
            if let Some(found) = session.unshown() {
                return found;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "no question arrived");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn questions_are_read_by_their_words() {
        assert_eq!(
            Question::parse("Username for 'https://example.com': "),
            Question::SignIn { protocol: "https".into(), host: "example.com".into(), username: None }
        );
        assert_eq!(
            Question::parse("Password for 'http://cedar@127.0.0.1:8081': "),
            Question::SignIn { protocol: "http".into(), host: "127.0.0.1:8081".into(), username: Some("cedar".into()) }
        );
        assert_eq!(
            Question::parse("Password for 'https://dan%40work@dev.azure.com/org/app.git': "),
            Question::SignIn { protocol: "https".into(), host: "dev.azure.com".into(), username: Some("dan%40work".into()) }
        );
        assert_eq!(
            Question::parse("Enter passphrase for key '/Users/cedar/.ssh/id_ed25519': "),
            Question::Passphrase { key: "/Users/cedar/.ssh/id_ed25519".into() }
        );
        assert_eq!(
            Question::parse("Enter passphrase for \"/Users/cedar/.ssh/id_signing\": "),
            Question::Passphrase { key: "/Users/cedar/.ssh/id_signing".into() }
        );
        let first = "The authenticity of host 'github.com (140.82.121.4)' can't be established.\nED25519 key fingerprint is SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(
            Question::parse(first),
            Question::HostKey {
                host: "github.com".into(),
                kind: Some("ED25519".into()),
                fingerprint: Some("SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU".into())
            }
        );
        let differs = "Warning: the ECDSA host key for 'git.example.com' differs from the key for the IP address '10.0.0.4'\nAre you sure you want to continue connecting (yes/no)? ";
        assert!(matches!(Question::parse(differs), Question::Confirm { text } if text.starts_with("Warning: the ECDSA")));
        assert_eq!(
            Question::parse("cedar@trail.example's password: \u{1b}[2J\u{7}"),
            Question::Other { text: "cedar@trail.example's password: [2J".into() }
        );
        assert_eq!(Question::parse(&"x".repeat(5000)), Question::Other { text: "x".repeat(LONGEST) });
    }

    #[test]
    fn the_helper_gets_the_answer_the_window_gives_through_the_socket() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let (socket, token) = (broker.path().to_owned(), session.token().to_owned());
        let asking = thread::spawn(move || ask(&socket, &token, "Enter passphrase for key '/k/id_trail': "));
        let (id, question, tries) = shown(&session);
        assert_eq!((question, tries), (Question::Passphrase { key: "/k/id_trail".into() }, 0));
        assert!(session.waiting() && session.waits_for(id));
        assert!(session.unshown().is_none());
        assert!(session.respond(id, "open sesame".into()));
        assert_eq!(asking.join().unwrap().as_deref(), Some("open sesame"));
        assert!(!session.waiting() && !session.declined());

        let (socket, token) = (broker.path().to_owned(), session.token().to_owned());
        let again = thread::spawn(move || ask(&socket, &token, "Enter passphrase for key '/k/id_trail': "));
        let (id, _, tries) = shown(&session);
        assert_eq!(tries, 1);
        let spec = describe(&Ask { session: session.clone(), id, question: Question::Passphrase { key: "/k/id_trail".into() }, tries, connect: true });
        assert_eq!(spec.title, "Unlock SSH key id_trail");
        assert_eq!(spec.lines[0], "That passphrase did not work.");
        assert!(spec.field.as_ref().is_some_and(|field| field.masked));
        assert!(session.decline(id));
        assert_eq!(again.join().unwrap(), None);
        assert!(session.declined());
    }

    #[test]
    fn a_token_that_is_not_a_jobs_is_refused_and_a_job_without_one_asks_nothing() {
        let (_dir, broker) = broker();
        let session = broker.session();
        assert_eq!(ask(broker.path(), "0123456789abcdef0123456789abcdef", "Username for 'https://example.com': "), None);
        assert_eq!(ask(broker.path(), "", "Username for 'https://example.com': "), None);
        assert_eq!(ask(Path::new(""), session.token(), "Username for 'https://example.com': "), None);
        assert_eq!(session.shown(), 0);
        assert!(session.unshown().is_none());
        let token = session.token().to_owned();
        drop(session);
        assert_eq!(ask(broker.path(), &token, "Username for 'https://example.com': "), None);
        assert_ne!(broker.session().token(), broker.session().token());
    }

    #[test]
    fn a_passphrase_is_called_wrong_only_when_the_same_ssh_asks_again() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let asked_by = |asker: u32| {
            let (socket, token) = (broker.path().to_owned(), session.token().to_owned());
            thread::spawn(move || {
                let prompt = Some("Enter passphrase for key '/k/id_trail': ".to_owned());
                exchange(&socket, &Request { token, prompt, remember: None, asker: Some(asker) })
            })
        };
        let origin = asked_by(101);
        let (id, _, tries) = shown(&session);
        assert_eq!(tries, 0);
        assert!(session.respond(id, "open sesame".into()));
        assert_eq!(origin.join().unwrap().as_deref(), Some("open sesame"));
        let upstream = asked_by(102);
        let (id, _, tries) = shown(&session);
        assert_eq!(tries, 0, "a new ssh's first question is not a retry");
        assert!(session.respond(id, "wrong".into()));
        assert_eq!(upstream.join().unwrap().as_deref(), Some("wrong"));
        let again = asked_by(102);
        let (id, _, tries) = shown(&session);
        assert_eq!(tries, 1);
        assert!(session.decline(id));
        assert_eq!(again.join().unwrap(), None);
    }

    #[test]
    fn a_helper_that_goes_away_withdraws_its_question() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let mut stream = UnixStream::connect(broker.path()).unwrap();
        let request = Request { token: session.token().to_owned(), prompt: Some("Username for 'https://example.com': ".into()), remember: None, asker: None };
        let mut line = serde_json::to_vec(&request).unwrap();
        line.push(b'\n');
        stream.write_all(&line).unwrap();
        let (id, ..) = shown(&session);
        assert!(session.waits_for(id));
        drop(stream);
        let start = Instant::now();
        while session.waits_for(id) {
            assert!(start.elapsed() < Duration::from_secs(3), "the question still waits");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!session.declined());
        assert!(!session.respond(id, "late".into()));
    }

    #[test]
    fn a_sign_in_answers_both_of_gits_questions_and_is_saved_only_when_ticked() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let socket = broker.path().to_owned();
        let token = session.token().to_owned();
        let (s, t) = (socket.clone(), token.clone());
        let git = thread::spawn(move || {
            let user = ask(&s, &t, "Username for 'https://git.example.com': ");
            let password = ask(&s, &t, "Password for 'https://cedar@git.example.com': ");
            (user, password)
        });
        let (id, question, _) = shown(&session);
        assert_eq!(question, Question::SignIn { protocol: "https".into(), host: "git.example.com".into(), username: None });
        let entered = Credential { protocol: "https".into(), host: "git.example.com".into(), username: "cedar".into(), password: "trail-pass".into() };
        assert!(session.sign_in(id, entered.clone(), false));
        assert_eq!(git.join().unwrap(), (Some("cedar".into()), Some("trail-pass".into())));
        assert_eq!(session.shown(), 1);
        assert!(!approved_at(&socket, &token, &entered));

        let ticked = broker.session();
        let (s, t) = (socket.clone(), ticked.token().to_owned());
        let git = thread::spawn(move || ask(&s, &t, "Password for 'https://cedar@git.example.com': "));
        let (id, question, _) = shown(&ticked);
        let spec = describe(&Ask { session: ticked.clone(), id, question: question.clone(), tries: 0, connect: true });
        assert_eq!(spec.title, "Sign in to git.example.com");
        assert_eq!(spec.lines, ["Git needs the password for cedar at https://git.example.com."]);
        assert!(spec.field.as_ref().is_some_and(|field| field.masked && field.label == "Password") && spec.second.is_none());
        assert!(spec.unblocked && spec.note.is_none() && spec.choices.is_empty());
        assert!(ticked.sign_in(id, entered.clone(), true));
        assert_eq!(git.join().unwrap().as_deref(), Some("trail-pass"));
        assert!(approved_at(&socket, ticked.token(), &entered));
        for other in [
            Credential { password: "other".into(), ..entered.clone() },
            Credential { username: "scout".into(), ..entered.clone() },
            Credential { host: "evil.example".into(), ..entered.clone() },
        ] {
            assert!(!approved_at(&socket, ticked.token(), &other), "{other:?}");
        }
        assert!(!approved_at(&socket, session.token(), &entered));
        assert!(!approved_at(&socket, "", &entered));
    }

    #[test]
    fn hosts_that_need_a_token_say_so_and_github_offers_its_connection() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let ask = |host: &str| Ask {
            session: session.clone(),
            id: 1,
            question: Question::SignIn { protocol: "https".into(), host: host.into(), username: None },
            tries: 0,
            connect: true,
        };
        let github = describe(&ask("github.com"));
        assert_eq!(github.title, "Sign in to github.com");
        let fields = (github.field.as_ref().map(|f| (f.label, f.masked)), github.second.as_ref().map(|f| (f.label, f.masked)));
        assert_eq!(fields, (Some(("Username", false)), Some(("Password", true))));
        let note = github.note.as_ref().unwrap();
        assert_eq!(note.text, "This host needs a personal access token, not your account password.");
        assert!(matches!(&note.link, Some((_, view::Action::Handoff(crate::handoff::Action::OpenUrl(url)))) if url == "https://github.com/settings/tokens"));
        assert_eq!(github.choices.iter().map(|c| c.label.to_string()).collect::<Vec<_>>(), ["Connect GitHub instead"]);
        assert_eq!(github.tick.as_ref().map(|(label, ticked)| (label.to_string(), *ticked)), Some(("Remember in Keychain".into(), false)));
        let cloning = Ask { connect: false, ..ask("github.com") };
        assert!(describe(&cloning).choices.is_empty() && describe(&cloning).note.is_some());
        let gitlab = describe(&ask("gitlab.com"));
        assert!(gitlab.note.is_some() && gitlab.choices.is_empty());
        let own = describe(&ask("git.example.com"));
        assert!(own.note.is_none() && own.choices.is_empty());

        let host = describe(
            &Ask {
                session: session.clone(),
                id: 2,
                question: Question::HostKey { host: "trail.example".into(), kind: Some("ED25519".into()), fingerprint: Some("SHA256:abc".into()) },
                tries: 0,
                connect: true,
            },
        );
        assert_eq!((host.title.as_str(), host.confirm.as_ref()), ("Connect to trail.example for the first time?", "Trust and connect"));
        assert!(host.field.is_none() && host.lines.iter().any(|line| line == "ED25519 key fingerprint: SHA256:abc"));
    }

    #[test]
    fn a_background_job_sets_no_token_and_an_interactive_one_sets_its_own() {
        let (_dir, broker) = broker();
        let session = broker.session();
        let env = |command: &Command, key: &str| {
            command.get_envs().find(|(k, _)| *k == key).map(|(_, value)| value.map(|v| v.to_string_lossy().into_owned()))
        };
        let mut plain = Command::new("/usr/bin/true");
        configure(&mut plain, Some(&session));
        assert_eq!(env(&plain, "GIT_ASKPASS"), None);

        let program = PathBuf::from("/Applications/FastKraken.app/Contents/MacOS/fastkraken");
        let _program = Program::set(program.clone());
        let mut interactive = Command::new("/usr/bin/true");
        configure(&mut interactive, Some(&session));
        let shown = program.display().to_string();
        assert_eq!(env(&interactive, "GIT_ASKPASS"), Some(Some(shown.clone())));
        assert_eq!(env(&interactive, "SSH_ASKPASS"), Some(Some(shown.clone())));
        assert_eq!(env(&interactive, "SSH_ASKPASS_REQUIRE"), Some(Some("force".into())));
        assert_eq!(env(&interactive, SOCKET), Some(Some(broker.path().display().to_string())));
        assert_eq!(env(&interactive, TOKEN), Some(Some(session.token().to_owned())));
        let mut background = Command::new("/usr/bin/true");
        configure(&mut background, None);
        assert_eq!(env(&background, "GIT_ASKPASS"), Some(Some(shown)));
        assert_eq!(env(&background, SOCKET), Some(Some(String::new())));
        assert_eq!(env(&background, TOKEN), Some(None));
    }

    #[test]
    fn an_interactive_job_hands_ssh_questions_to_the_askpass_program_and_a_background_one_does_not() {
        use crate::repository::{checked, tests::Fixture};
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Start the trail"]);
        let bare = f.repo.git_dir().join("trail.git");
        f.git(&["init", "-q", "--bare", bare.to_str().unwrap()]);
        f.git(&["push", "-q", bare.to_str().unwrap(), "main"]);
        let script = |name: &str, body: &str| {
            let path = f.repo.git_dir().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let ssh = script(
            "ssh.sh",
            "answer=$(\"$SSH_ASKPASS\" \"Enter passphrase for key '/k/id_trail': \") || { echo 'Permission denied (publickey).' >&2; exit 255; }\n[ \"$answer\" = 'open sesame' ] || { echo 'Permission denied (publickey).' >&2; exit 255; }\nfor last; do :; done\nexec /bin/sh -c \"git ${last#git-}\"\n",
        );
        f.git(&["config", "core.sshCommand", ssh.to_str().unwrap()]);
        f.git(&["config", "ssh.variant", "ssh"]);
        f.git(&["remote", "add", "origin", &format!("ssh://git@trail.invalid{}", bare.display())]);
        let asked = f.repo.git_dir().join("asked.log");
        let program = script(
            "askpass.sh",
            &format!("echo \"$1\" >> '{}'\n[ -n \"$FASTKRAKEN_ASKPASS_TOKEN\" ] && [ \"$SSH_ASKPASS_REQUIRE\" = force ] || exit 1\necho 'open sesame'\n", asked.display()),
        );
        let _program = Program::set(program);
        let fetch = |link: &crate::jobs::Link| link.enter(|| checked(f.repo.network_command().unwrap().args(["fetch", "-q", "origin"])));

        let background = crate::jobs::Link::new(true);
        let refused = fetch(&background).unwrap_err();
        assert!(refused.contains("Permission denied (publickey)"), "{refused}");
        assert!(f.git(&["for-each-ref", "refs/remotes"]).is_empty());

        let interactive = crate::jobs::Link::interactive();
        assert!(interactive.session().is_some());
        fetch(&interactive).unwrap();
        assert_eq!(f.git(&["rev-parse", "refs/remotes/origin/main"]), f.git(&["rev-parse", "main"]));
        assert_eq!(fs::read_to_string(&asked).unwrap(), "Enter passphrase for key '/k/id_trail': \n".repeat(2));
    }

    pub(crate) struct Program;

    impl Program {
        pub(crate) fn set(program: PathBuf) -> Self {
            use_program(Some(program));
            Self
        }
    }

    impl Drop for Program {
        fn drop(&mut self) {
            use_program(None);
        }
    }
}
