use crate::{
    Tab, kit,
    notice::Notice,
    repository::{self, Repository},
    settings::{self, Settings},
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, KeyDownEvent, PathPromptOptions, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, String>;

const PROXY_VARIABLES: [[&str; 2]; 2] = [["https_proxy", "HTTPS_PROXY"], ["all_proxy", "ALL_PROXY"]];
const PROXY_KINDS: [&str; 6] = ["http", "https", "socks4", "socks4a", "socks5", "socks5h"];
const SCUTIL: &str = "/usr/sbin/scutil";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitProgram {
    #[default]
    Bundled,
    System,
    Chosen(Vec<u8>),
}

impl GitProgram {
    pub(crate) fn chosen_path(&self) -> Option<PathBuf> {
        match self {
            Self::Chosen(bytes) => Some(PathBuf::from(OsStr::from_bytes(bytes))),
            _ => None,
        }
    }

    pub(crate) fn of(path: &Path) -> Self {
        Self::Chosen(path.as_os_str().as_bytes().to_vec())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) path: PathBuf,
    pub(crate) version: Result<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MacProxy {
    Direct,
    Server(String),
    Automatic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proxy {
    pub(crate) configured: Option<(String, String)>,
    pub(crate) global: Option<String>,
    pub(crate) environment: Option<(&'static str, String)>,
    pub(crate) mac: MacProxy,
}

impl Proxy {
    pub(crate) fn used(&self) -> Option<(String, String)> {
        if let Some((scope, value)) = &self.configured {
            if value.is_empty() {
                return None;
            }
            let from = match scope.as_str() {
                "global" => "Set by http.proxy in your Git settings for every repository.".to_owned(),
                "local" | "worktree" => "Set by http.proxy in this repository's own Git settings.".to_owned(),
                "system" => "Set by http.proxy in this Mac's system Git settings.".to_owned(),
                other => format!("Set by http.proxy ({other})."),
            };
            return Some((value.clone(), from));
        }
        self.environment.as_ref().map(|(name, value)| (value.clone(), format!("Set by {name} in FastKraken's environment.")))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Looked {
    pub(crate) bundled: Result<String>,
    pub(crate) system: Option<Found>,
    pub(crate) chosen: Option<Found>,
    pub(crate) proxy: Result<Proxy>,
}

pub(crate) struct State {
    pub(crate) looked: Option<Looked>,
    pub(crate) ssh: Entity<CommitInput>,
    pub(crate) proxy: Entity<CommitInput>,
    pub(crate) problem: Option<String>,
    pub(crate) ssh_problem: Option<String>,
    pub(crate) proxy_problem: Option<String>,
    pub(crate) switching: bool,
    pub(crate) saving: bool,
    ticket: u64,
    answered: u64,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let own = settings::settings(cx).ssh_command;
        let ssh = cx.new(|cx| {
            let mut field = CommitInput::with_placeholder(cx, "ssh -F ~/.ssh/work_config");
            field.set_text(&own, cx);
            field
        });
        cx.observe(&ssh, |tab, _, cx| {
            if tab.ui.engine.ssh_problem.take().is_some() {
                cx.notify();
            }
        })
        .detach();
        let proxy = cx.new(|cx| CommitInput::with_placeholder(cx, "http://proxy.example.com:3128"));
        cx.observe(&proxy, |tab, _, cx| {
            if tab.ui.engine.proxy_problem.take().is_some() {
                cx.notify();
            }
        })
        .detach();
        Self {
            looked: None,
            ssh,
            proxy,
            problem: None,
            ssh_problem: None,
            proxy_problem: None,
            switching: false,
            saving: false,
            ticket: 0,
            answered: 0,
        }
    }

    pub(crate) fn reading(&self) -> bool {
        self.answered != self.ticket
    }

    fn seen_proxy(&self) -> Option<Option<String>> {
        self.looked.as_ref()?.proxy.as_ref().ok().map(|proxy| proxy.global.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Look,
    Use(GitProgram),
    ChooseGit,
    SystemSsh,
    OwnSsh,
    SaveProxy,
    UseMacProxy,
}

pub(crate) fn resolve(program: &GitProgram, dirs: &[PathBuf], bundled: Option<&Path>, developer: Option<&Path>) -> Result<Option<PathBuf>> {
    match program {
        GitProgram::Bundled => Ok(None),
        GitProgram::System => repository::system_git(dirs, bundled, developer)
            .map(Some)
            .ok_or_else(|| "No Git other than FastKraken's own was found on the search path or in /usr/bin.".to_owned()),
        GitProgram::Chosen(_) => {
            let path = program.chosen_path().unwrap_or_default();
            if bundled.is_some_and(|bundled| path.parent().is_some_and(|folder| repository::same_place(folder, bundled))) {
                return Err("That is FastKraken's own Git. Choose Bundled to use it.".to_owned());
            }
            Ok(Some(path))
        }
    }
}

pub(crate) fn ready(program: &GitProgram) -> Result<(Option<PathBuf>, String)> {
    let developer = repository::developer_git();
    let bundled = repository::bundled_bin();
    match resolve(program, crate::environment::host_dirs(), bundled.as_deref(), developer.as_deref())? {
        None => Ok((None, repository::bundled_version()?)),
        Some(path) => {
            let version = repository::git_version(&path, developer.as_deref())?;
            Ok((Some(path), version))
        }
    }
}

pub(crate) fn apply_saved(cx: &mut App) {
    if crate::acceptance::replaying() {
        return;
    }
    let chosen = settings::settings(cx);
    repository::use_ssh_program(Some(chosen.ssh_command.clone()));
    if chosen.git_program == GitProgram::Bundled {
        return;
    }
    let applied = ready(&chosen.git_program).and_then(|(path, _)| repository::use_git(path.as_deref()));
    if let Err(reason) = applied {
        settings::tell_at_startup(
            cx,
            format!("FastKraken is running its bundled Git, because the Git chosen in Preferences cannot be used. {reason}"),
        );
    }
}

fn look(repo: Option<&Repository>, chosen: Option<PathBuf>) -> Looked {
    let developer = repository::developer_git();
    let bundled_bin = repository::bundled_bin();
    let found = |path: PathBuf| Found { version: repository::git_version(&path, developer.as_deref()), path };
    let system = repository::system_git(crate::environment::host_dirs(), bundled_bin.as_deref(), developer.as_deref()).map(found);
    let chosen = chosen.map(found);
    let listing = mac_listing();
    Looked {
        bundled: repository::bundled_version(),
        system,
        chosen,
        proxy: read_proxy(repo, |name| std::env::var(name).ok(), listing.as_deref()),
    }
}

fn mac_listing() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let program = if crate::acceptance::replaying() {
        crate::acceptance::sandbox()?.join("bin/scutil")
    } else {
        PathBuf::from(SCUTIL)
    };
    if !repository::runnable(&program) {
        return None;
    }
    let output = Command::new(program).arg("--proxy").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn mac_proxy(listing: &str) -> MacProxy {
    let values: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| line.split_once(" : "))
        .map(|(key, value)| (key.trim(), value.trim()))
        .collect();
    let value = |key: &str| values.iter().find(|(name, _)| *name == key).map(|(_, value)| *value);
    for (kind, scheme) in [("HTTPS", "http"), ("HTTP", "http"), ("SOCKS", "socks5")] {
        if value(&format!("{kind}Enable")) != Some("1") {
            continue;
        }
        let Some(host) = value(&format!("{kind}Proxy")).filter(|host| !host.is_empty()) else {
            continue;
        };
        return MacProxy::Server(match value(&format!("{kind}Port")).filter(|port| !port.is_empty()) {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        });
    }
    if value("ProxyAutoConfigEnable") == Some("1") || value("ProxyAutoDiscoveryEnable") == Some("1") {
        return MacProxy::Automatic;
    }
    MacProxy::Direct
}

pub(crate) fn environment_proxy(variable: impl Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    let (name, value) = PROXY_VARIABLES.iter().find_map(|names| names.iter().find_map(|name| variable(name).map(|value| (*name, value))))?;
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some((name, value))
}

fn config_value(command: &mut Command) -> Result<Option<String>> {
    let output = repository::run(command.stdin(Stdio::null())).map_err(|e| format!("Run Git: {e}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_owned())),
        Some(1) => Ok(None),
        _ => Err(repository::git_error(&output.stderr, output.status)),
    }
}

fn outside() -> Result<Command> {
    let mut command = repository::git_command(Path::new("/"))?;
    command.arg("--no-optional-locks");
    Ok(command)
}

pub(crate) fn global_proxy() -> Result<Option<String>> {
    config_value(outside()?.args(["config", "get", "--global", "http.proxy"]))
}

pub(crate) fn read_proxy(repo: Option<&Repository>, variable: impl Fn(&str) -> Option<String>, mac: Option<&str>) -> Result<Proxy> {
    let mut command = match repo {
        Some(repo) => repo.read_command(),
        None => outside()?,
    };
    let configured = config_value(command.args(["config", "get", "--show-scope", "http.proxy"]))?.map(|line| match line.split_once('\t') {
        Some((scope, value)) => (scope.to_owned(), value.to_owned()),
        None => (String::new(), line),
    });
    Ok(Proxy {
        configured,
        global: global_proxy()?,
        environment: environment_proxy(variable),
        mac: mac.map_or(MacProxy::Direct, mac_proxy),
    })
}

pub(crate) fn set_proxy(seen: Option<String>, value: Option<String>) -> Result<Option<String>> {
    let now = global_proxy()?;
    if now != seen {
        return Err("The proxy in your Git settings changed since Preferences read it. Check it and save again.".to_owned());
    }
    if now == value {
        return Ok(now);
    }
    let mut command = repository::git_command(Path::new("/"))?;
    match &value {
        Some(value) => command.args(["config", "set", "--global", "--all", "--end-of-options", "http.proxy", value]),
        None => command.args(["config", "unset", "--global", "--all", "http.proxy"]),
    };
    repository::checked(command.stdin(Stdio::null()))?;
    global_proxy()
}

pub(crate) fn proxy_problem(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("A proxy address has no spaces.".to_owned());
    }
    let rest = match text.split_once("://") {
        Some((kind, rest)) if PROXY_KINDS.contains(&kind.to_ascii_lowercase().as_str()) => rest,
        Some((kind, _)) => {
            return Some(format!("Git does not know the proxy kind {kind}. Use http, https, socks4, socks4a, socks5 or socks5h."));
        }
        None => text,
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let place = match rest.rsplit_once('@') {
        Some((user, _)) if user.contains(':') => {
            return Some("Leave the password out: Git keeps its settings as plain text. Git asks for it when the proxy wants one.".to_owned());
        }
        Some(("", _)) => return Some("Type a user name before the @, or leave the @ out.".to_owned()),
        Some((_, place)) => place,
        None => rest,
    };
    if place.contains('/') {
        return Some("A proxy address is a host and a port, without a path.".to_owned());
    }
    let (host, port) = match place.strip_prefix('[') {
        Some(bracketed) => match bracketed.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, after)) => match after.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return Some("Put the port after a colon.".to_owned()),
            },
            None => return Some("Close the brackets around the address.".to_owned()),
        },
        None => match place.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (place, None),
        },
    };
    if host.is_empty() {
        return Some("Type the proxy's host name.".to_owned());
    }
    if port.is_some_and(|port| port.parse::<u16>().map_or(true, |port| port == 0)) {
        return Some("The port is a number from 1 to 65535.".to_owned());
    }
    None
}

fn program_word(command: &str) -> Option<&str> {
    match command.chars().next() {
        Some(quote @ ('\'' | '"')) => command[1..].find(quote).map(|end| &command[1..1 + end]),
        _ => command.split_whitespace().next(),
    }
}

pub(crate) fn ssh_problem(command: &str, dirs: &[PathBuf], home: Option<&Path>) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return Some("Type the command Git should run instead of ssh.".to_owned());
    }
    let Some(program) = program_word(command) else {
        return Some("Close the quote around the program.".to_owned());
    };
    let expanded = match (program.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(program),
    };
    if program.contains('/') {
        if !expanded.is_absolute() {
            return Some("Give the program's full path, or a name found on the search path.".to_owned());
        }
        return (!repository::runnable(&expanded)).then(|| format!("{program} is not a program FastKraken can run."));
    }
    let system = [PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
    let found = dirs.iter().chain(system.iter()).any(|dir| repository::runnable(&dir.join(program)));
    (!found).then(|| format!("{program} was not found on the search path."))
}

pub(crate) fn read(tab: &mut Tab, cx: &mut Context<Tab>) {
    let repo = tab.repo.clone();
    let chosen = settings::settings(cx).git_program.chosen_path();
    tab.ui.engine.ticket += 1;
    let ticket = tab.ui.engine.ticket;
    Tab::work(cx, move || look(repo.as_ref(), chosen), move |tab, looked, cx| {
        let state = &mut tab.ui.engine;
        if state.ticket != ticket {
            return;
        }
        let before = state.seen_proxy().flatten().unwrap_or_default();
        let after = looked.proxy.as_ref().ok().and_then(|proxy| proxy.global.clone()).unwrap_or_default();
        state.looked = Some(looked);
        state.answered = ticket;
        let field = state.proxy.clone();
        if field.read(cx).text().trim() == before {
            field.update(cx, |field, cx| field.set_text(&after, cx));
        }
        cx.notify();
    });
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Look => read(tab, cx),
        Action::Use(program) => switch(tab, program, cx),
        Action::ChooseGit => {
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Use this Git".into()),
            });
            window
                .spawn(cx, {
                    let tab = cx.weak_entity();
                    async move |cx| {
                        if let Ok(Ok(Some(paths))) = picker.await
                            && let Some(path) = paths.into_iter().next()
                        {
                            let _ = cx.update(|window, cx| {
                                tab.update(cx, |tab, cx| {
                                    tab.dispatch(view::Action::Engine(Action::Use(GitProgram::of(&path))), window, cx)
                                })
                            });
                        }
                    }
                })
                .detach();
        }
        Action::SystemSsh => {
            repository::use_ssh_program(None);
            tab.ui.engine.ssh_problem = None;
            let was = settings::settings(cx).ssh_command;
            Settings::update(cx, |s| s.ssh_command = String::new());
            if !was.is_empty() {
                tab.ui.notices.show(Notice::new(Tone::Success, "SSH program set", "Git connects with the system's ssh again, with FastKraken's timeouts."));
            }
        }
        Action::OwnSsh => {
            let field = tab.ui.engine.ssh.clone();
            let typed = field.read(cx).text().trim().to_owned();
            let home = std::env::var_os("HOME").map(PathBuf::from);
            if let Some(problem) = ssh_problem(&typed, crate::environment::host_dirs(), home.as_deref()) {
                tab.ui.engine.ssh_problem = Some(problem);
                field.focus_handle(cx).focus(window);
            } else {
                repository::use_ssh_program(Some(typed.clone()));
                Settings::update(cx, |s| s.ssh_command = typed.clone());
                tab.ui.notices.show(Notice::new(Tone::Success, "SSH program set", format!("Git connects with {typed}.")));
            }
        }
        Action::SaveProxy => save_proxy(tab, cx),
        Action::UseMacProxy => {
            let mac = tab.ui.engine.looked.as_ref().and_then(|looked| looked.proxy.as_ref().ok()).map(|proxy| proxy.mac.clone());
            if let Some(MacProxy::Server(address)) = mac {
                let field = tab.ui.engine.proxy.clone();
                field.update(cx, |field, cx| field.set_text(&address, cx));
                save_proxy(tab, cx);
            }
        }
    }
    cx.notify();
}

fn switch(tab: &mut Tab, program: GitProgram, cx: &mut Context<Tab>) {
    if tab.ui.engine.switching {
        return;
    }
    if tab.ui.busy() {
        tab.refuse("Wait for the current operation to finish before changing Git.".to_owned(), cx);
        return;
    }
    tab.ui.engine.switching = true;
    tab.ui.engine.problem = None;
    let checked = program.clone();
    Tab::work(cx, move || ready(&checked), move |tab, result, cx| {
        tab.ui.engine.switching = false;
        let applied = result.and_then(|(path, version)| repository::use_git(path.as_deref()).map(|_| (path, version)));
        match applied {
            Ok((path, version)) => {
                Settings::update(cx, |s| s.git_program = program);
                let notice = match path {
                    Some(path) => Notice::new(Tone::Success, format!("Using Git {version}"), format!("FastKraken now runs {} for every repository.", path.display())),
                    None => Notice::new(Tone::Success, "Using the bundled Git", format!("FastKraken runs its own Git {version} again.")),
                };
                tab.ui.notices.show(notice);
                read(tab, cx);
                if tab.repo.is_some() {
                    tab.refresh(true, cx);
                }
            }
            Err(reason) => {
                tab.ui.engine.problem = Some(reason.clone());
                tab.refuse(reason, cx);
            }
        }
        cx.notify();
    });
}

fn save_proxy(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.engine;
    if state.saving {
        return;
    }
    let typed = state.proxy.read(cx).text().trim().to_owned();
    if let Some(problem) = proxy_problem(&typed) {
        state.proxy_problem = Some(problem);
        return;
    }
    let Some(seen) = state.seen_proxy() else {
        state.proxy_problem = Some("Wait for Preferences to read the proxy first.".to_owned());
        return;
    };
    let value = (!typed.is_empty()).then_some(typed);
    state.saving = true;
    state.proxy_problem = None;
    Tab::work(cx, move || set_proxy(seen, value), move |tab, result, cx| {
        tab.ui.engine.saving = false;
        match result {
            Ok(Some(value)) => {
                tab.ui.notices.show(Notice::new(Tone::Success, "Proxy saved", format!("Git uses {value} for every repository.")));
            }
            Ok(None) => {
                tab.ui.notices.show(Notice::new(Tone::Success, "Proxy removed", "Your Git settings no longer name a proxy."));
            }
            Err(reason) => {
                tab.ui.engine.proxy_problem = Some(reason.clone());
                tab.refuse(reason, cx);
            }
        }
        read(tab, cx);
        cx.notify();
    });
}

fn submit<T: Desk>(action: Action, cx: &mut Context<T>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |app: &mut T, e: &KeyDownEvent, w, cx| {
        if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
            app.dispatch(view::Action::Engine(action.clone()), w, cx);
            cx.stop_propagation();
        }
    })
}

fn found_place(found: Option<&Found>, home: Option<&Path>, missing: &str) -> String {
    match found {
        None => missing.to_owned(),
        Some(found) => {
            let place = crate::start::homely(&found.path, home);
            match &found.version {
                Ok(version) => format!("Git {version} · {place}"),
                Err(_) => format!("Cannot be used · {place}"),
            }
        }
    }
}

fn problem_line(text: &str) -> Div {
    div().py_1().text_size(px(TEXT_SMALL)).text_color(rgb(DELETED)).child(text.to_owned())
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    use crate::preferences::{group, setting};
    let state = &ui.engine;
    let chosen = settings::settings(cx);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let looked = state.looked.as_ref();
    let row = crate::tools::choice::<T>;
    let use_git = |program: GitProgram| view::Action::Engine(Action::Use(program));
    let bundled = match looked.map(|looked| &looked.bundled) {
        None => "Reading its version…".to_owned(),
        Some(Ok(version)) => format!("Git {version} · inside FastKraken"),
        Some(Err(error)) => error.clone(),
    };
    let system = match looked {
        None => "Looking…".to_owned(),
        Some(looked) => found_place(looked.system.as_ref(), home.as_deref(), "None found on the search path or in /usr/bin"),
    };
    let another = match (looked, chosen.git_program.chosen_path()) {
        (_, None) => "Choose a git program".to_owned(),
        (Some(looked), Some(_)) => found_place(looked.chosen.as_ref(), home.as_deref(), "Reading its version…"),
        (None, Some(path)) => crate::start::homely(&path, home.as_deref()),
    };
    let reason = |found: Option<&Found>| found.and_then(|found| found.version.as_ref().err().cloned());
    let unusable = match &chosen.git_program {
        GitProgram::Bundled => None,
        GitProgram::System => looked.and_then(|looked| reason(looked.system.as_ref())),
        GitProgram::Chosen(_) => looked.and_then(|looked| reason(looked.chosen.as_ref())),
    };
    let problem = state.problem.clone().or(unusable);
    let programs = div()
        .flex()
        .flex_col()
        .child(row(("engine-git", 0), "Bundled".to_owned(), Some(bundled), chosen.git_program == GitProgram::Bundled, use_git(GitProgram::Bundled), cx))
        .child(row(("engine-git", 1), "This Mac's Git".to_owned(), Some(system), chosen.git_program == GitProgram::System, use_git(GitProgram::System), cx))
        .child(row(
            ("engine-git", 2),
            "Another Git…".to_owned(),
            Some(another),
            matches!(chosen.git_program, GitProgram::Chosen(_)),
            view::Action::Engine(Action::ChooseGit),
            cx,
        ))
        .child(div().pt_2().child(kit::muted(format!(
            "FastKraken needs Git {}.{} or newer. Another Git brings its own credential helpers and Git LFS; the bundled one stays in the app.",
            repository::OLDEST_GIT[0],
            repository::OLDEST_GIT[1]
        ))))
        .child(div().pt_1().flex().child(kit::link("engine-look", "Look again", view::Action::Engine(Action::Look), cx)))
        .when(state.switching, |d| d.child(div().py_1().child(kit::muted("Checking that Git…"))))
        .when_some(problem, |d, problem| d.child(problem_line(&problem)));

    let own_ssh = !chosen.ssh_command.trim().is_empty();
    let ssh_field = div().w(px(260.)).on_key_down(submit(Action::OwnSsh, cx)).child(state.ssh.clone());
    let ssh = div()
        .flex()
        .flex_col()
        .child(row(
            ("engine-ssh", 0),
            "The system's ssh".to_owned(),
            Some("The first ssh on the search path, usually /usr/bin/ssh, with 15-second connection timeouts".to_owned()),
            !own_ssh,
            view::Action::Engine(Action::SystemSsh),
            cx,
        ))
        .child(row(
            ("engine-ssh", 1),
            "Your own command".to_owned(),
            own_ssh.then(|| chosen.ssh_command.clone()),
            own_ssh,
            view::Action::Engine(Action::OwnSsh),
            cx,
        ))
        .child(setting(
            "Command",
            Some("Git runs it as GIT_SSH_COMMAND in place of ssh, and FastKraken's timeouts no longer apply. A repository with its own core.sshCommand keeps using that.".into()),
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ssh_field)
                .child(kit::btn("engine-ssh-use", "Use").small().on(view::Action::Engine(Action::OwnSsh), cx)),
        ))
        .when_some(state.ssh_problem.clone(), |d, problem| d.child(problem_line(&problem)));

    let proxy = looked.map(|looked| &looked.proxy);
    let (used, from) = match proxy {
        None => ("Reading…".to_owned(), None),
        Some(Err(error)) => (error.clone(), None),
        Some(Ok(proxy)) => match proxy.used() {
            Some((value, from)) => (value, Some(from)),
            None => ("None".to_owned(), Some("Git connects directly.".to_owned())),
        },
    };
    let mac = proxy.and_then(|proxy| proxy.as_ref().ok()).map(|proxy| proxy.mac.clone());
    let mac_line = match &mac {
        Some(MacProxy::Server(address)) => Some(format!("macOS is set to use {address}, which Git does not read by itself.")),
        Some(MacProxy::Automatic) => Some("macOS finds its proxy automatically, which Git cannot follow. If Git cannot connect, type the proxy here.".to_owned()),
        _ => None,
    };
    let proxy_field = div().w(px(260.)).on_key_down(submit(Action::SaveProxy, cx)).child(state.proxy.clone());
    let proxies = div()
        .flex()
        .flex_col()
        .child(setting(
            "Git uses",
            from.map(Into::into),
            div().max_w(px(260.)).child(kit::fit(used).size(12.5).color(TEXT_2)),
        ))
        .child(setting(
            "Proxy for every repository",
            Some("Sets http.proxy in your Git settings. Leave it empty to remove it.".into()),
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(proxy_field)
                .child(
                    kit::btn("engine-proxy-save", if state.saving { "Saving…" } else { "Save" })
                        .small()
                        .enabled(!state.saving && proxy.is_some_and(|proxy| proxy.is_ok()))
                        .on(view::Action::Engine(Action::SaveProxy), cx),
                ),
        ))
        .when_some(mac_line, |d, line| {
            d.child(
                div()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(kit::muted(line)))
                    .when(matches!(mac, Some(MacProxy::Server(_))), |d| {
                        d.child(kit::link("engine-proxy-mac", "Use it", view::Action::Engine(Action::UseMacProxy), cx))
                    }),
            )
        })
        .when_some(state.proxy_problem.clone(), |d, problem| d.child(problem_line(&problem)));

    div()
        .flex()
        .flex_col()
        .child(group("GIT PROGRAM"))
        .child(programs)
        .child(group("SSH PROGRAM"))
        .child(ssh)
        .child(group("PROXY"))
        .child(proxies)
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    !ui.preferences.shows_identity()
        && (ui.engine.ssh.focus_handle(cx).is_focused(window) || ui.engine.proxy.focus_handle(cx).is_focused(window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};
    use std::{fs, os::unix::fs::PermissionsExt};

    fn script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn snapshot(fixture: &Fixture) -> String {
        let refs = fixture.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
        let status = fixture.git(&["status", "--porcelain"]);
        let index = fs::read(fixture.dir.join(".git/index")).unwrap_or_default();
        format!(
            "{}{}{index:?}{}",
            String::from_utf8_lossy(&refs),
            String::from_utf8_lossy(&status),
            fs::read_to_string(fixture.dir.join(".git/config")).unwrap()
        )
    }

    #[test]
    fn the_proxy_is_written_to_the_global_settings_only_when_they_are_as_read() {
        let fixture = Fixture::new();
        fs::write(fixture.dir.join("one.txt"), "one\n").unwrap();
        fixture.git(&["add", "one.txt"]);
        fixture.git(&["commit", "-qm", "One"]);
        let global = fixture.dir.join("global-config");
        fs::write(&global, "").unwrap();
        let _global = GlobalConfig::set(&global);
        let before = snapshot(&fixture);
        assert_eq!(global_proxy().unwrap(), None);
        assert_eq!(set_proxy(None, Some("http://proxy.invalid:3128".to_owned())).unwrap().as_deref(), Some("http://proxy.invalid:3128"));
        assert!(fs::read_to_string(&global).unwrap().contains("proxy = http://proxy.invalid:3128"));
        let read = read_proxy(Some(&fixture.repo), |_| None, None).unwrap();
        assert_eq!(read.configured, Some(("global".to_owned(), "http://proxy.invalid:3128".to_owned())));
        assert_eq!(read.used().unwrap().1, "Set by http.proxy in your Git settings for every repository.");

        let refused = set_proxy(None, Some("http://other.invalid:8080".to_owned())).unwrap_err();
        assert!(refused.starts_with("The proxy in your Git settings changed"), "{refused}");
        assert_eq!(global_proxy().unwrap().as_deref(), Some("http://proxy.invalid:3128"));

        fixture.git(&["config", "--local", "http.proxy", "socks5h://local.invalid:1080"]);
        let local = read_proxy(Some(&fixture.repo), |_| None, None).unwrap();
        assert_eq!(local.configured, Some(("local".to_owned(), "socks5h://local.invalid:1080".to_owned())));
        assert_eq!(local.global.as_deref(), Some("http://proxy.invalid:3128"));
        fixture.git(&["config", "--local", "--unset", "http.proxy"]);

        assert_eq!(set_proxy(Some("http://proxy.invalid:3128".to_owned()), None).unwrap(), None);
        assert_eq!(global_proxy().unwrap(), None);
        assert_eq!(snapshot(&fixture), before);
    }

    #[test]
    fn with_no_proxy_in_git_the_environment_and_then_macos_are_shown() {
        let fixture = Fixture::new();
        let global = fixture.dir.join("global-config");
        fs::write(&global, "").unwrap();
        let _global = GlobalConfig::set(&global);
        let variables = |name: &str| match name {
            "https_proxy" => Some("http://lower.invalid:8080".to_owned()),
            "ALL_PROXY" => Some("socks5://all.invalid:1080".to_owned()),
            _ => None,
        };
        let listing = "<dictionary> {\n  HTTPSEnable : 1\n  HTTPSPort : 8443\n  HTTPSProxy : corp.invalid\n}\n";
        let read = read_proxy(Some(&fixture.repo), variables, Some(listing)).unwrap();
        assert_eq!(read.configured, None);
        assert_eq!(read.environment, Some(("https_proxy", "http://lower.invalid:8080".to_owned())));
        assert_eq!(read.used(), Some(("http://lower.invalid:8080".to_owned(), "Set by https_proxy in FastKraken's environment.".to_owned())));
        assert_eq!(read.mac, MacProxy::Server("http://corp.invalid:8443".to_owned()));
        assert_eq!(environment_proxy(|name| (name == "ALL_PROXY").then(|| " socks5://all.invalid ".to_owned())), Some(("ALL_PROXY", "socks5://all.invalid".to_owned())));
        assert_eq!(environment_proxy(|_| Some(String::new())), None);
        let both = |name: &str| match name {
            "HTTPS_PROXY" => Some("http://upper.invalid:8080".to_owned()),
            "https_proxy" => Some("http://lower.invalid:8080".to_owned()),
            "all_proxy" => Some("socks5://lower-all.invalid:1080".to_owned()),
            "ALL_PROXY" => Some("socks5://upper-all.invalid:1080".to_owned()),
            _ => None,
        };
        assert_eq!(environment_proxy(both), Some(("https_proxy", "http://lower.invalid:8080".to_owned())));
        assert_eq!(environment_proxy(|name| both(name).filter(|_| name.ends_with("_PROXY") || name.starts_with("all"))), Some(("HTTPS_PROXY", "http://upper.invalid:8080".to_owned())));
        assert_eq!(environment_proxy(|name| both(name).filter(|_| name.starts_with("all") || name.starts_with("ALL"))), Some(("all_proxy", "socks5://lower-all.invalid:1080".to_owned())));
        assert_eq!(environment_proxy(|name| if name == "https_proxy" { Some(String::new()) } else { both(name) }), None);

        fixture.git(&["config", "--local", "http.proxy", ""]);
        let emptied = read_proxy(Some(&fixture.repo), variables, None).unwrap();
        assert_eq!(emptied.configured, Some(("local".to_owned(), String::new())));
        assert_eq!(emptied.environment, Some(("https_proxy", "http://lower.invalid:8080".to_owned())));
        assert_eq!(emptied.used(), None);
    }

    #[test]
    fn the_macos_proxy_is_read_from_scutil() {
        assert_eq!(mac_proxy("<dictionary> {\n  ExceptionsList : <array> {\n    0 : *.local\n  }\n  FTPPassive : 1\n}\n"), MacProxy::Direct);
        assert_eq!(mac_proxy("  HTTPEnable : 1\n  HTTPProxy : web.invalid\n  HTTPPort : 80\n  HTTPSEnable : 0\n  HTTPSProxy : off.invalid\n"), MacProxy::Server("http://web.invalid:80".to_owned()));
        assert_eq!(mac_proxy("  SOCKSEnable : 1\n  SOCKSProxy : socks.invalid\n  SOCKSPort : 1080\n"), MacProxy::Server("socks5://socks.invalid:1080".to_owned()));
        assert_eq!(mac_proxy("  ProxyAutoConfigEnable : 1\n  ProxyAutoConfigURLString : http://wpad.invalid/proxy.pac\n"), MacProxy::Automatic);
        assert_eq!(mac_proxy("  HTTPSEnable : 1\n  HTTPSProxy : \n"), MacProxy::Direct);
    }

    #[test]
    fn a_proxy_address_is_checked_before_it_is_written() {
        for good in ["", "proxy.invalid", "proxy.invalid:3128", "http://proxy.invalid:3128/", "socks5h://user@proxy.invalid:1080", "HTTPS://[::1]:8443", "[::1]"] {
            assert_eq!(proxy_problem(good), None, "{good}");
        }
        assert_eq!(proxy_problem("http://user:secret@proxy.invalid:3128").unwrap(), "Leave the password out: Git keeps its settings as plain text. Git asks for it when the proxy wants one.");
        assert!(proxy_problem("ftp://proxy.invalid").unwrap().contains("proxy kind ftp"));
        assert_eq!(proxy_problem("proxy invalid").unwrap(), "A proxy address has no spaces.");
        assert_eq!(proxy_problem("http://proxy.invalid:99999").unwrap(), "The port is a number from 1 to 65535.");
        assert_eq!(proxy_problem("http://proxy.invalid:0").unwrap(), "The port is a number from 1 to 65535.");
        assert_eq!(proxy_problem("http://:3128").unwrap(), "Type the proxy's host name.");
        assert_eq!(proxy_problem("http://proxy.invalid/path").unwrap(), "A proxy address is a host and a port, without a path.");
        assert_eq!(proxy_problem("http://@proxy.invalid").unwrap(), "Type a user name before the @, or leave the @ out.");
        assert_eq!(proxy_problem("[::1").unwrap(), "Close the brackets around the address.");
    }

    #[test]
    fn an_ssh_command_must_name_a_program_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(&bin.join("work-ssh"), "exit 0\n");
        fs::write(bin.join("plain"), "").unwrap();
        let dirs = [bin.clone()];
        assert_eq!(ssh_problem("work-ssh -F config", &dirs, None), None);
        assert_eq!(ssh_problem("ssh -o ConnectTimeout=5", &[], None), None);
        let full = bin.join("work-ssh");
        assert_eq!(ssh_problem(&format!("'{}' -v", full.display()), &[], None), None);
        assert_eq!(ssh_problem("~/bin/work-ssh -v", &[], Some(dir.path())), None);
        assert_eq!(ssh_problem("  ", &dirs, None).unwrap(), "Type the command Git should run instead of ssh.");
        assert_eq!(ssh_problem("missing-ssh -v", &dirs, None).unwrap(), "missing-ssh was not found on the search path.");
        assert_eq!(ssh_problem("plain", &dirs, None).unwrap(), "plain was not found on the search path.");
        assert_eq!(ssh_problem("bin/work-ssh", &dirs, None).unwrap(), "Give the program's full path, or a name found on the search path.");
        assert!(ssh_problem(&bin.join("plain").display().to_string(), &dirs, None).unwrap().ends_with("is not a program FastKraken can run."));
        assert_eq!(ssh_problem("'unclosed -v", &dirs, None).unwrap(), "Close the quote around the program.");
    }

    #[test]
    fn the_systems_git_is_the_first_on_the_search_path_other_than_the_bundled_one() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled");
        let brew = dir.path().join("brew");
        let empty = dir.path().join("empty");
        for folder in [&bundled, &brew, &empty] {
            fs::create_dir(folder).unwrap();
        }
        script(&bundled.join("git"), "exit 0\n");
        script(&brew.join("git"), "exit 0\n");
        let dirs = [empty.clone(), bundled.clone(), brew.clone()];
        assert_eq!(repository::system_git(&dirs, Some(&bundled), None), Some(brew.join("git")));
        assert_eq!(resolve(&GitProgram::System, &dirs, Some(&bundled), None), Ok(Some(brew.join("git"))));
        let with_xcode = repository::system_git(&[empty.clone(), bundled.clone()], Some(&bundled), Some(Path::new("/developer/usr/bin/git")));
        assert_eq!(with_xcode.as_deref(), Path::new("/usr/bin/git").is_file().then_some(Path::new("/usr/bin/git")));
        assert_eq!(repository::system_git(&[empty.clone(), bundled.clone()], Some(&bundled), None), None);
        assert!(resolve(&GitProgram::System, &[bundled.clone()], Some(&bundled), None).unwrap_err().starts_with("No Git other than FastKraken's own"));
        assert_eq!(resolve(&GitProgram::Bundled, &dirs, Some(&bundled), None), Ok(None));
        assert_eq!(resolve(&GitProgram::of(&brew.join("git")), &dirs, Some(&bundled), None), Ok(Some(brew.join("git"))));
        assert_eq!(resolve(&GitProgram::of(&bundled.join("git")), &dirs, Some(&bundled), None).unwrap_err(), "That is FastKraken's own Git. Choose Bundled to use it.");
    }

    #[test]
    fn an_old_or_broken_git_is_refused_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old-git");
        script(&old, "echo 'git version 2.39.5 (Apple Git-154)'\n");
        let fine = dir.path().join("new-git");
        script(&fine, "echo 'git version 2.46.0'\n");
        let other = dir.path().join("other");
        script(&other, "echo 'hello'\n");
        let plain = dir.path().join("plain");
        fs::write(&plain, "").unwrap();
        assert_eq!(repository::git_version(&fine, None).unwrap(), "2.46.0");
        assert_eq!(
            repository::git_version(&old, None).unwrap_err(),
            format!("{} is Git 2.39.5 (Apple Git-154), and FastKraken needs Git 2.46 or newer.", old.display())
        );
        assert_eq!(repository::git_version(&other, None).unwrap_err(), format!("{} did not answer as Git does to --version.", other.display()));
        assert_eq!(repository::git_version(&plain, None).unwrap_err(), format!("{} is not a program FastKraken can run.", plain.display()));
        assert_eq!(repository::git_version(Path::new("relative/git"), None).unwrap_err(), "relative/git is not a full path to a program.");
        assert!(repository::git_version(Path::new("/usr/bin/git"), None).unwrap_err().starts_with("The Command Line Tools are not installed"));
        assert!(repository::new_enough("2.46.0"));
        assert!(repository::new_enough("2.54.0 (Apple Git-157)"));
        assert!(repository::new_enough("3.0.0"));
        assert!(!repository::new_enough("2.45.9"));
        assert!(!repository::new_enough("2"));
        assert_eq!(repository::version_number("2.47.0.rc1"), [2, 47, 0]);
    }

    #[test]
    fn the_git_program_setting_reads_its_chosen_path() {
        let chosen = GitProgram::of(Path::new("/opt/git/bin/git"));
        assert_eq!(chosen.chosen_path(), Some(PathBuf::from("/opt/git/bin/git")));
        assert_eq!(GitProgram::System.chosen_path(), None);
        assert_eq!(serde_json::to_string(&GitProgram::System).unwrap(), "\"system\"");
        let read: GitProgram = serde_json::from_str("\"bundled\"").unwrap();
        assert_eq!(read, GitProgram::Bundled);
    }
}
