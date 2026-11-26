use crate::{
    Tab, kit,
    commands::{Availability, Command as Entry, Group, Target},
    identity::Scope,
    jobs,
    notice::Notice,
    preferences::{self, Section, group, radio, setting},
    prompt::{self, Answer, Field, Spec},
    repository::{self, Repository},
    signing::{Config, Format},
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use gpui::{Context, Div, SharedString, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub(crate) const PREFIX: &str = "id_ed25519_";
const LONGEST_NAME: usize = 64;
const APPLE_SSH_ADD: &str = "/usr/bin/ssh-add";

fn shown_on_repo(target: &Target) -> Availability {
    if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
}

pub(crate) const COMMANDS: &[Entry] = &[
    Entry::new(
        "keys.signing",
        Group::Settings,
        |_, _| "Signing settings…".into(),
        |_, target, _| shown_on_repo(target),
        |_, _| Some(view::Action::Preferences(preferences::Action::Open(Some(Section::Signing)))),
    )
    .words(&["gpg", "sign commits", "sign tags", "signing key", "commit.gpgsign", "gpg.format", "ssh signing"]),
    Entry::new(
        "keys.ssh",
        Group::Settings,
        |_, _| "SSH keys…".into(),
        |_, target, _| shown_on_repo(target),
        |_, _| Some(view::Action::Preferences(preferences::Action::Open(Some(Section::Ssh)))),
    )
    .words(&["ssh", "public key", "id_ed25519", "ssh-agent", "core.sshcommand"]),
    Entry::new(
        "keys.generate",
        Group::Settings,
        |_, _| "Generate SSH key…".into(),
        |_, target, _| shown_on_repo(target),
        |_, _| Some(view::Action::Keys(Action::Generate)),
    )
    .words(&["ssh-keygen", "new ssh key", "ed25519", "create key"]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    GitHub,
    GitLab,
    Bitbucket,
}

impl Host {
    pub(crate) const ALL: [Host; 3] = [Host::GitHub, Host::GitLab, Host::Bitbucket];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Host::GitHub => "GitHub",
            Host::GitLab => "GitLab",
            Host::Bitbucket => "Bitbucket",
        }
    }

    pub(crate) fn page(self) -> &'static str {
        match self {
            Host::GitHub => "https://github.com/settings/ssh/new",
            Host::GitLab => "https://gitlab.com/-/user_settings/ssh_keys",
            Host::Bitbucket => "https://bitbucket.org/account/settings/ssh-keys/",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Printed {
    pub bits: String,
    pub fingerprint: String,
    pub comment: String,
    pub kind: String,
}

pub(crate) fn printed(line: &str) -> Option<Printed> {
    let mut parts = line.trim().splitn(3, ' ');
    let bits = parts.next()?.to_owned();
    let fingerprint = parts.next().filter(|print| print.contains(':'))?.to_owned();
    let rest = parts.next().unwrap_or_default().trim();
    let (comment, kind) = match rest.rsplit_once(" (") {
        Some((comment, kind)) if kind.ends_with(')') => (comment.trim(), kind.trim_end_matches(')')),
        _ if rest.starts_with('(') && rest.ends_with(')') => ("", rest.trim_matches(['(', ')'])),
        _ => (rest, ""),
    };
    let comment = if comment == "no comment" { "" } else { comment };
    Some(Printed { bits, fingerprint, comment: comment.to_owned(), kind: kind.to_owned() })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SshKey {
    pub public: PathBuf,
    pub private: Option<PathBuf>,
    pub printed: Printed,
    pub text: String,
}

impl SshKey {
    pub(crate) fn name(&self) -> String {
        self.public.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub(crate) fn identity(&self) -> &Path {
        self.private.as_deref().unwrap_or(&self.public)
    }

    pub(crate) fn detail(&self) -> String {
        let kind = if self.printed.kind.is_empty() { "SSH".to_owned() } else { self.printed.kind.clone() };
        match self.printed.comment.as_str() {
            "" => format!("{kind} · {}", self.printed.fingerprint),
            comment => format!("{kind} · {} · {comment}", self.printed.fingerprint),
        }
    }

    pub(crate) fn chosen_by(&self, key: &str) -> bool {
        let key = key.trim();
        Path::new(key) == self.public || self.private.as_deref() == Some(Path::new(key)) || same_text(key, &self.text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentKey {
    pub text: String,
    pub printed: Option<Printed>,
}

impl AgentKey {
    pub(crate) fn value(&self) -> String {
        format!("key::{}", self.text)
    }

    pub(crate) fn title(&self) -> String {
        let comment = self.text.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");
        if comment.is_empty() { self.text.split_whitespace().next().unwrap_or("Key").to_owned() } else { comment }
    }

    pub(crate) fn detail(&self) -> String {
        match &self.printed {
            Some(printed) => format!("In the agent · {} · {}", printed.kind, printed.fingerprint),
            None => "In the agent".to_owned(),
        }
    }

    pub(crate) fn chosen_by(&self, key: &str) -> bool {
        same_text(key.trim().strip_prefix("key::").unwrap_or(key.trim()), &self.text)
    }
}

fn same_text(one: &str, other: &str) -> bool {
    let body = |text: &str| text.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let (one, other) = (body(one), body(other));
    !one.is_empty() && one.contains(' ') && one == other
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GpgKey {
    pub id: String,
    pub fingerprint: String,
    pub user: String,
    pub usable: bool,
}

impl GpgKey {
    pub(crate) fn chosen_by(&self, key: &str) -> bool {
        let key = key.trim().trim_end_matches('!').trim_start_matches("0x").to_ascii_uppercase();
        !key.is_empty()
            && (key == self.id
                || key == self.fingerprint
                || (key.len() >= 8 && key.chars().all(|c| c.is_ascii_hexdigit()) && self.fingerprint.ends_with(&key))
                || self.user.to_ascii_uppercase().contains(&key) && key.contains('@'))
    }
}

pub(crate) fn parse_gpg(listed: &str) -> Vec<GpgKey> {
    let mut keys: Vec<GpgKey> = Vec::new();
    let mut in_secret = false;
    for line in listed.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or_default();
        match field(0) {
            "sec" => {
                in_secret = true;
                let validity = field(1);
                let capabilities = field(11);
                let usable = !matches!(validity, "e" | "r" | "d" | "i" | "n") && (capabilities.contains('S') || capabilities.contains('s'));
                keys.push(GpgKey { id: field(4).to_ascii_uppercase(), fingerprint: String::new(), user: String::new(), usable });
            }
            "fpr" if in_secret => {
                if let Some(key) = keys.last_mut().filter(|key| key.fingerprint.is_empty()) {
                    key.fingerprint = field(9).to_ascii_uppercase();
                }
            }
            "uid" if in_secret => {
                if let Some(key) = keys.last_mut().filter(|key| key.user.is_empty()) {
                    key.user = unescape_colons(field(9));
                }
            }
            "ssb" | "pub" | "sub" => in_secret = field(0) == "ssb" && in_secret,
            _ => {}
        }
    }
    keys
}

fn unescape_colons(text: &str) -> String {
    text.replace("\\x3a", ":").replace("\\x5c", "\\")
}

pub(crate) struct Places {
    pub folder: PathBuf,
    pub agent: Option<PathBuf>,
    pub keychain: bool,
}

impl Places {
    pub(crate) fn here() -> Self {
        if let Some(sandbox) = crate::acceptance::sandbox() {
            return Self { folder: sandbox.join("home/.ssh"), agent: None, keychain: false };
        }
        if cfg!(test) {
            return Self { folder: PathBuf::from("/var/empty/fastkraken-test-ssh"), agent: None, keychain: false };
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/empty"));
        let agent = std::env::var_os("SSH_AUTH_SOCK").filter(|socket| !socket.is_empty()).map(PathBuf::from);
        Self { folder: home.join(".ssh"), agent, keychain: true }
    }
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub(crate) fn tool(name: &str) -> PathBuf {
    crate::environment::host_dirs()
        .iter()
        .map(|dir| dir.join(name))
        .find(|path| executable(path))
        .unwrap_or_else(|| Path::new("/usr/bin").join(name))
}

fn command(program: &Path, agent: Option<&Path>) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null()).env_remove("SSH_ASKPASS").env_remove(crate::askpass::TOKEN);
    match agent {
        Some(agent) => command.env("SSH_AUTH_SOCK", agent),
        None => command.env_remove("SSH_AUTH_SOCK"),
    };
    #[cfg(test)]
    command.env("GNUPGHOME", repository::tests::NO_KEYRING);
    command
}

fn ran(command: &mut Command) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    repository::run(command).map_err(|error| format!("Run {program}: {error}"))
}

fn said(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if text.is_empty() { format!("It exited with {}", output.status) } else { text }
}

fn key_at(public: &Path) -> Option<SshKey> {
    let text = fs::read_to_string(public).ok()?.lines().next()?.trim().to_owned();
    let output = ran(command(&tool("ssh-keygen"), None).arg("-l").arg("-f").arg(public)).ok()?;
    if !output.status.success() {
        return None;
    }
    let printed = printed(String::from_utf8_lossy(&output.stdout).lines().next()?)?;
    let private = public.with_extension("");
    let private = (public.extension().is_some_and(|ext| ext == "pub") && private.is_file()).then_some(private);
    Some(SshKey { public: public.to_owned(), private, printed, text })
}

pub(crate) fn ssh_keys(folder: &Path) -> Result<Vec<SshKey>, String> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Read {}: {error}", folder.display())),
    };
    let mut publics: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pub") && path.is_file())
        .collect();
    publics.sort();
    Ok(publics.iter().filter_map(|public| key_at(public)).collect())
}

pub(crate) fn agent_keys(agent: Option<&Path>) -> Result<Vec<AgentKey>, String> {
    let agent = agent.ok_or_else(|| "No SSH agent is running for FastKraken.".to_owned())?;
    let program = tool("ssh-add");
    let listed = ran(command(&program, Some(agent)).arg("-L"))?;
    let text = String::from_utf8_lossy(&listed.stdout).into_owned();
    if !listed.status.success() {
        if listed.status.code() == Some(1) && text.to_lowercase().contains("no identities") {
            return Ok(Vec::new());
        }
        return Err(format!("The SSH agent did not answer: {}", said(&listed)));
    }
    let printed_lines = ran(command(&program, Some(agent)).arg("-l"))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).lines().map(printed).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| AgentKey { text: line.to_owned(), printed: printed_lines.get(index).cloned().flatten() })
        .collect())
}

pub(crate) fn gpg_program(configured: Option<&str>) -> Option<PathBuf> {
    let configured = configured.map(str::trim).filter(|name| !name.is_empty());
    match configured {
        Some(path) if path.starts_with('/') => Some(PathBuf::from(path)).filter(|path| executable(path)),
        Some(name) => crate::environment::host_dirs().iter().map(|dir| dir.join(name)).find(|path| executable(path)),
        None => crate::environment::host_dirs().iter().map(|dir| dir.join("gpg")).find(|path| executable(path)),
    }
}

pub(crate) fn gpg_keys(program: Option<&Path>) -> Result<Vec<GpgKey>, String> {
    let program = program.ok_or_else(|| {
        "GPG is not installed on this Mac. Install GnuPG (brew install gnupg) to sign with a GPG key, or choose SSH.".to_owned()
    })?;
    let output = ran(command(program, None).args(["--batch", "--no-tty", "--with-colons", "--fixed-list-mode", "--list-secret-keys"]))?;
    if !output.status.success() {
        return Err(format!("GPG could not list your keys: {}", said(&output)));
    }
    Ok(parse_gpg(&String::from_utf8_lossy(&output.stdout)))
}

pub(crate) fn name_problem(name: &str, folder: &Path) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some(String::new());
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if name.len() > LONGEST_NAME || !name.chars().all(allowed) || name.starts_with(['.', '-']) || name.ends_with(".pub") {
        return Some("Use letters, digits, dots, dashes and underscores.".to_owned());
    }
    let file = format!("{PREFIX}{name}");
    let private = folder.join(&file);
    let public = folder.join(format!("{file}.pub"));
    if fs::symlink_metadata(&private).is_ok() || fs::symlink_metadata(&public).is_ok() {
        return Some(format!("{file} already exists in {}.", folder.display()));
    }
    None
}

pub(crate) fn generating(folder: &Path, name: &str, comment: Option<&str>) -> Command {
    let mut command = command(&tool("ssh-keygen"), None);
    command.args(["-q", "-t", "ed25519"]);
    if let Some(comment) = comment.map(str::trim).filter(|comment| !comment.is_empty()) {
        command.arg("-C").arg(comment);
    }
    command.arg("-f").arg(folder.join(format!("{PREFIX}{}", name.trim())));
    command
}

pub(crate) fn generate(folder: &Path, name: &str, comment: Option<&str>) -> Result<SshKey, String> {
    if let Some(problem) = name_problem(name, folder) {
        return Err(if problem.is_empty() { "Enter a name for the key.".to_owned() } else { problem });
    }
    let link = jobs::Link::current();
    let session = link.as_ref().and_then(jobs::Link::session);
    if session.is_none() || crate::askpass::program().is_none() {
        return Err("FastKraken cannot hand the passphrase to ssh-keygen here.".to_owned());
    }
    if !folder.is_dir() {
        fs::create_dir_all(folder).map_err(|error| format!("Create {}: {error}", folder.display()))?;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).map_err(|error| format!("Protect {}: {error}", folder.display()))?;
    }
    let mut command = generating(folder, name, comment);
    crate::askpass::configure(&mut command, session.map(|session| &**session));
    let output = ran(&mut command)?;
    if !output.status.success() {
        return Err(format!("ssh-keygen could not make the key: {}", said(&output)));
    }
    let public = folder.join(format!("{PREFIX}{}.pub", name.trim()));
    key_at(&public).ok_or_else(|| format!("ssh-keygen made no readable key at {}", public.display()))
}

pub(crate) fn opens_without_passphrase(private: &Path) -> bool {
    ran(command(&tool("ssh-keygen"), None).args(["-y", "-P", "", "-f"]).arg(private)).is_ok_and(|output| output.status.success())
}

pub(crate) fn kept_locked(key: SshKey) -> Result<SshKey, String> {
    if !opens_without_passphrase(key.identity()) {
        return Ok(key);
    }
    let _ = fs::remove_file(key.identity());
    let _ = fs::remove_file(&key.public);
    Err(format!("ssh-keygen made {} without the passphrase, so FastKraken removed it. Try again.", key.name()))
}

pub(crate) fn adding(key: &Path, agent: &Path, keychain: bool) -> Command {
    let apple = Path::new(APPLE_SSH_ADD);
    let keychain = keychain && executable(apple);
    let mut command = command(&if keychain { apple.to_owned() } else { tool("ssh-add") }, Some(agent));
    command.env("SSH_ASKPASS", "/usr/bin/false").env("SSH_ASKPASS_REQUIRE", "force");
    if keychain {
        command.arg("--apple-use-keychain");
    }
    command.arg(key);
    command
}

pub(crate) fn add_to_agent(key: &Path, agent: Option<&Path>, keychain: bool) -> Result<(), String> {
    let agent = agent.ok_or_else(|| "No SSH agent is running, so the key cannot be added to one.".to_owned())?;
    let keychain = keychain && !cfg!(test) && !crate::acceptance::replaying();
    let mut command = adding(key, agent, keychain);
    if let Some(link) = jobs::Link::current() {
        crate::askpass::configure(&mut command, link.session().map(|session| &**session));
    }
    let output = ran(&mut command)?;
    if !output.status.success() {
        return Err(format!("ssh-add could not add the key: {}", said(&output)));
    }
    Ok(())
}

fn safe(path: &str) -> bool {
    !path.is_empty() && path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@%:=,~".contains(c)) && !path.starts_with('~')
}

pub(crate) fn quoted(path: &str) -> String {
    if safe(path) { path.to_owned() } else { format!("'{}'", path.replace('\'', "'\\''")) }
}

pub(crate) fn ssh_command_for(key: &Path) -> String {
    format!("ssh -i {} -o IdentitiesOnly=yes", quoted(&key.to_string_lossy()))
}

pub(crate) fn key_in(command: &str) -> Option<String> {
    let rest = command.split_once(" -i ")?.1.trim_start();
    if let Some(quoted) = rest.strip_prefix('\'') {
        let mut found = String::new();
        let mut text = quoted;
        loop {
            let (part, after) = text.split_once('\'')?;
            found.push_str(part);
            match after.strip_prefix("\\''") {
                Some(more) => {
                    found.push('\'');
                    text = more;
                }
                None => return Some(found),
            }
        }
    }
    rest.split_whitespace().next().map(str::to_owned)
}

pub(crate) fn local_ssh_command(repo: &Repository) -> Result<Option<String>, String> {
    let output = repo
        .read_command()
        .args(["config", "get", "--local", "core.sshCommand"])
        .output()
        .map_err(|error| format!("Run bundled Git: {error}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_owned())),
        Some(1) => Ok(None),
        _ => Err(repository::git_error(&output.stderr, output.status)),
    }
}

pub(crate) fn use_key(repo: &Repository, key: Option<&Path>, seen: &Option<String>) -> Result<Option<String>, String> {
    let now = local_ssh_command(repo)?;
    if now != *seen {
        return Err(match &now {
            Some(now) => format!("This repository's SSH command changed to {now} meanwhile. Check it and choose again."),
            None => "This repository's SSH command was removed meanwhile. Check it and choose again.".to_owned(),
        });
    }
    let mut command = repo.command();
    match key {
        Some(key) => command.args(["config", "set", "--local", "--all", "--end-of-options", "core.sshCommand"]).arg(ssh_command_for(key)),
        None => command.args(["config", "unset", "--local", "--all", "core.sshCommand"]),
    };
    if now.is_some() || key.is_some() {
        repository::checked(&mut command)?;
    }
    local_ssh_command(repo)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    Format,
    Key,
    Commits,
    Tags,
}

impl Setting {
    pub(crate) const ALL: [Setting; 4] = [Setting::Format, Setting::Key, Setting::Commits, Setting::Tags];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Setting::Format => "gpg.format",
            Setting::Key => "user.signingkey",
            Setting::Commits => "commit.gpgsign",
            Setting::Tags => "tag.gpgsign",
        }
    }

    fn words(self) -> &'static str {
        match self {
            Setting::Format => "The signing format",
            Setting::Key => "The signing key",
            Setting::Commits => "Sign commits by default",
            Setting::Tags => "Sign tags by default",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Raw {
    pub values: [Option<String>; 4],
}

impl Raw {
    pub(crate) fn get(&self, setting: Setting) -> Option<&str> {
        self.values[setting as usize].as_deref()
    }

    fn set(&mut self, key: &str, value: Option<&str>) {
        if let Some(setting) = Setting::ALL.into_iter().find(|setting| setting.key() == key) {
            self.values[setting as usize] = Some(value.unwrap_or("true").to_owned());
        }
    }

    pub(crate) fn config(&self) -> Config {
        let mut config = Config::default();
        for setting in Setting::ALL {
            if let Some(value) = self.get(setting) {
                config.read(setting.key(), Some(value));
            }
        }
        config
    }

    pub(crate) fn any(&self) -> bool {
        self.values.iter().any(Option::is_some)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Signing {
    pub global: Raw,
    pub local: Raw,
    pub effective: Raw,
    pub program: Option<String>,
}

impl Signing {
    pub(crate) fn at(&self, scope: Scope) -> &Raw {
        match scope {
            Scope::Global => &self.global,
            Scope::Local => &self.local,
        }
    }

    pub(crate) fn shown(&self, scope: Scope) -> Config {
        match scope {
            Scope::Global => self.global.config(),
            Scope::Local => self.effective.config(),
        }
    }
}

pub(crate) fn parse_signing(listed: &[u8]) -> Signing {
    let mut signing = Signing::default();
    let mut parts = listed.split(|&byte| byte == 0);
    while let (Some(scope), Some(entry)) = (parts.next(), parts.next()) {
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = match entry.split_once('\n') {
            Some((key, value)) => (key, Some(value)),
            None => (entry.as_ref(), None),
        };
        if key == "gpg.program" || key == "gpg.openpgp.program" {
            signing.program = value.map(str::to_owned);
            continue;
        }
        signing.effective.set(key, value);
        match scope {
            b"global" => signing.global.set(key, value),
            b"local" | b"worktree" => signing.local.set(key, value),
            _ => {}
        }
    }
    signing
}

fn listing(mut command: Command) -> Result<Signing, String> {
    command.args(["config", "list", "--show-scope", "-z"]);
    repository::checked(&mut command).map(|listed| parse_signing(&listed))
}

pub(crate) fn read_signing(repo: Option<&Repository>) -> Result<Signing, String> {
    match repo {
        Some(repo) => listing(repo.read_command()),
        None => {
            let mut command = repository::git_command(Path::new("/"))?;
            command.arg("--no-optional-locks");
            listing(command)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub scope: Scope,
    pub setting: Setting,
    pub value: Option<String>,
    pub seen: Option<String>,
}

pub(crate) fn change(repo: Option<&Repository>, change: &Change) -> Result<Signing, String> {
    if change.scope == Scope::Local && repo.is_none() {
        return Err("Open a repository to give it its own signing settings.".to_owned());
    }
    let before = read_signing(repo)?;
    let now = before.at(change.scope).get(change.setting).map(str::to_owned);
    if now != change.seen {
        let place = match change.scope {
            Scope::Global => "for every repository",
            Scope::Local => "for this repository",
        };
        return Err(format!("{} {place} changed meanwhile. Check it and choose again.", change.setting.words()));
    }
    if now == change.value {
        return Ok(before);
    }
    let mut command = match (change.scope, repo) {
        (Scope::Local, Some(repo)) => repo.command(),
        _ => repository::git_command(Path::new("/"))?,
    };
    match &change.value {
        Some(value) => command.args(["config", "set", change.scope.flag(), "--all", "--end-of-options", change.setting.key(), value]),
        None => command.args(["config", "unset", change.scope.flag(), "--all", change.setting.key()]),
    };
    repository::checked(&mut command)?;
    read_signing(repo)
}

pub(crate) struct Ask {
    pub name: String,
    pub passphrase: Option<String>,
    pub comment: Option<String>,
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ask").field("name", &self.name).field("repeat", &self.passphrase.is_some()).finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Reload,
    SetScope(Scope),
    SetFormat(Format),
    UseSigningKey(String),
    ToggleCommits,
    ToggleTags,
    Generate,
    AddToAgent(PathBuf),
    UseForRepository(Option<PathBuf>),
}

struct Reading {
    ssh: Result<Vec<SshKey>, String>,
    agent: Result<Vec<AgentKey>, String>,
    gpg: Option<Result<Vec<GpgKey>, String>>,
    signing: Result<Signing, String>,
    command: Option<Result<Option<String>, String>>,
}

pub(crate) struct State {
    pub(crate) places: Places,
    pub(crate) ssh: Option<Result<Vec<SshKey>, String>>,
    pub(crate) agent: Option<Result<Vec<AgentKey>, String>>,
    pub(crate) gpg: Option<Result<Vec<GpgKey>, String>>,
    pub(crate) signing: Option<Result<Signing, String>>,
    pub(crate) command: Option<Result<Option<String>, String>>,
    pub(crate) scope: Scope,
    scope_chosen: bool,
    ticket: u64,
    pub(crate) reading: bool,
    pub(crate) saving: bool,
    pub(crate) working: bool,
    pub(crate) error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            places: Places::here(),
            ssh: None,
            agent: None,
            gpg: None,
            signing: None,
            command: None,
            scope: Scope::Global,
            scope_chosen: false,
            ticket: 0,
            reading: false,
            saving: false,
            working: false,
            error: None,
        }
    }
}

impl State {
    pub(crate) fn keys(&self) -> &[SshKey] {
        match &self.ssh {
            Some(Ok(keys)) => keys,
            _ => &[],
        }
    }

    pub(crate) fn agent_keys(&self) -> &[AgentKey] {
        match &self.agent {
            Some(Ok(keys)) => keys,
            _ => &[],
        }
    }

    pub(crate) fn used_here(&self) -> Option<String> {
        match &self.command {
            Some(Ok(Some(command))) => key_in(command),
            _ => None,
        }
    }

    pub(crate) fn settled(&self) -> bool {
        !self.reading && !self.saving && !self.working
    }

    fn scope_for(&self, repository: bool) -> Scope {
        if repository { self.scope } else { Scope::Global }
    }
}

pub(crate) fn read(tab: &mut Tab, gpg: bool, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.keys;
    state.ticket += 1;
    state.reading = true;
    let (ticket, epoch) = (state.ticket, tab.epoch);
    let (folder, agent) = (state.places.folder.clone(), state.places.agent.clone());
    let repo = tab.repo.clone();
    Tab::work(
        cx,
        move || {
            let signing = read_signing(repo.as_ref());
            let program = signing.as_ref().ok().and_then(|signing| signing.program.clone());
            Reading {
                ssh: ssh_keys(&folder),
                agent: agent_keys(agent.as_deref()),
                gpg: gpg.then(|| gpg_keys(gpg_program(program.as_deref()).as_deref())),
                command: repo.as_ref().map(local_ssh_command),
                signing,
            }
        },
        move |tab, reading, cx| {
            let repository = tab.repo.is_some();
            let state = &mut tab.ui.keys;
            if state.ticket != ticket || tab.epoch != epoch {
                return;
            }
            state.reading = false;
            if !state.scope_chosen {
                state.scope = match &reading.signing {
                    Ok(signing) if repository && signing.local.any() => Scope::Local,
                    _ => Scope::Global,
                };
            }
            state.ssh = Some(reading.ssh);
            state.agent = Some(reading.agent);
            if reading.gpg.is_some() {
                state.gpg = reading.gpg;
            }
            state.signing = Some(reading.signing);
            state.command = reading.command;
            cx.notify();
        },
    );
}

fn wanted_gpg(tab: &Tab) -> bool {
    tab.ui.preferences.open() && tab.ui.preferences.section() == Section::Signing
}

fn email(ui: &Ui) -> Option<String> {
    ui.identity.effective().and_then(|values| values.email.clone()).or_else(|| match ui.identity.global() {
        Some(Ok(values)) => values.email.clone(),
        _ => None,
    })
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Reload => read(tab, true, cx),
        Action::SetScope(scope) => {
            if scope == Scope::Local && tab.repo.is_none() {
                return;
            }
            tab.ui.keys.scope = scope;
            tab.ui.keys.scope_chosen = true;
            tab.ui.keys.error = None;
        }
        Action::SetFormat(format) => {
            let value = match format {
                Format::Ssh => "ssh",
                Format::X509 => "x509",
                Format::Gpg => "openpgp",
            };
            write(tab, Setting::Format, Some(value.to_owned()), cx);
        }
        Action::UseSigningKey(key) => write(tab, Setting::Key, Some(key), cx),
        Action::ToggleCommits | Action::ToggleTags => {
            let setting = if matches!(action, Action::ToggleCommits) { Setting::Commits } else { Setting::Tags };
            let Some(Ok(signing)) = &tab.ui.keys.signing else { return };
            let shown = signing.shown(tab.ui.keys.scope_for(tab.repo.is_some()));
            let on = if setting == Setting::Commits { shown.commits } else { shown.tags };
            write(tab, setting, Some((!on).to_string()), cx);
        }
        Action::Generate => {
            if !(tab.ui.preferences.open() && tab.ui.preferences.section() == Section::Ssh) {
                preferences::dispatch(tab, preferences::Action::Open(Some(Section::Ssh)), window, cx);
            }
            if tab.ui.keys.working {
                return tab.refuse("Wait for the key to be made".to_owned(), cx);
            }
            let comment = email(&tab.ui);
            prompt::ask(&mut tab.ui, prompt::Kind::Keys(Ask { name: String::new(), passphrase: None, comment }), cx);
        }
        Action::AddToAgent(key) => add(tab, key, cx),
        Action::UseForRepository(key) => use_for_repository(tab, key, cx),
    }
    cx.notify();
}

fn write(tab: &mut Tab, setting: Setting, value: Option<String>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.keys;
    let Some(Ok(signing)) = &state.signing else { return };
    if state.saving {
        return;
    }
    let scope = state.scope_for(tab.repo.is_some());
    let seen = signing.at(scope).get(setting).map(str::to_owned);
    let change = Change { scope, setting, value, seen };
    state.saving = true;
    state.error = None;
    let repo = tab.repo.clone();
    let epoch = tab.epoch;
    Tab::work(cx, move || self::change(repo.as_ref(), &change), move |tab, result, cx| {
        if tab.epoch != epoch {
            return;
        }
        let state = &mut tab.ui.keys;
        state.saving = false;
        match result {
            Ok(signing) => state.signing = Some(Ok(signing)),
            Err(error) => {
                state.error = Some(error);
                read(tab, wanted_gpg(tab), cx);
            }
        }
        if tab.repo.is_some() {
            tab.refresh(false, cx);
        }
        cx.notify();
    });
}

fn job<R: Send + 'static>(
    tab: &mut Tab,
    progress: &'static str,
    passphrase: Option<String>,
    work: impl FnOnce() -> Result<R, String> + Send + 'static,
    done: impl FnOnce(&mut Tab, R) -> Notice + 'static,
    cx: &mut Context<Tab>,
) {
    if tab.ui.busy() {
        return tab.refuse("Wait for the current operation to finish".to_owned(), cx);
    }
    let link = jobs::Link::answering();
    if let (Some(passphrase), Some(session)) = (passphrase, link.session()) {
        session.choose_passphrase(passphrase);
    }
    let spinner = tab.ui.notices.progress(progress);
    tab.ui.jobs.start_write(progress, link.clone(), Some(spinner));
    tab.ui.keys.working = true;
    jobs::watch(tab, cx);
    cx.notify();
    Tab::work(
        cx,
        move || {
            let result = link.enter(work);
            (result, link.declined())
        },
        move |tab, (result, declined), cx| {
            tab.ui.jobs.end_write();
            tab.ui.keys.working = false;
            crate::askpass::poll(tab, cx);
            let notice = match result {
                Ok(value) => done(tab, value),
                Err(_) if declined => Notice::new(Tone::Info, "Stopped", "No passphrase was given."),
                Err(error) => Notice::from_text(Tone::Error, &error),
            };
            tab.ui.notices.finish(spinner, notice);
            read(tab, wanted_gpg(tab), cx);
            tab.resume(cx);
            cx.notify();
        },
    );
}

fn add(tab: &mut Tab, key: PathBuf, cx: &mut Context<Tab>) {
    let (agent, keychain) = (tab.ui.keys.places.agent.clone(), tab.ui.keys.places.keychain);
    let name = key.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    job(
        tab,
        "Adding the key to the SSH agent…",
        None,
        move || add_to_agent(&key, agent.as_deref(), keychain),
        move |_, _| {
            let detail = if keychain {
                format!("{name} is unlocked for ssh until you log out, and its passphrase is kept in your Keychain.")
            } else {
                format!("{name} is unlocked for ssh until the agent stops.")
            };
            Notice::new(Tone::Success, "Key added to the agent", detail)
        },
        cx,
    );
}

fn use_for_repository(tab: &mut Tab, key: Option<PathBuf>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let seen = match &tab.ui.keys.command {
        Some(Ok(command)) => command.clone(),
        _ => return,
    };
    if tab.ui.keys.saving {
        return;
    }
    tab.ui.keys.saving = true;
    tab.ui.keys.error = None;
    let (title, epoch) = (tab.ui.repo.clone(), tab.epoch);
    Tab::work(cx, move || use_key(&repo, key.as_deref(), &seen).map(|now| (now, key)), move |tab, result, cx| {
        if tab.epoch != epoch {
            return;
        }
        tab.ui.keys.saving = false;
        match result {
            Ok((now, key)) => {
                let notice = match key {
                    Some(key) => Notice::new(Tone::Success, "SSH key chosen", format!("{title} connects with {} only.", key.display())),
                    None => Notice::new(Tone::Success, "SSH key no longer chosen", format!("{title} connects with your usual SSH settings.")),
                };
                tab.ui.keys.command = Some(Ok(now));
                tab.ui.notices.show(notice);
            }
            Err(error) => {
                tab.ui.keys.error = Some(error);
                read(tab, wanted_gpg(tab), cx);
            }
        }
        cx.notify();
    });
}

pub(crate) fn describe(ask: &Ask, ui: &Ui) -> Spec {
    let folder = ui.keys.places.folder.clone();
    if ask.passphrase.is_some() {
        let mut spec = Spec::new("Type the passphrase again", "Generate")
            .line(format!("To be sure it is the one you meant for {PREFIX}{}.", ask.name));
        spec.field = Some(Field {
            label: "Passphrase",
            placeholder: "Passphrase again",
            initial: String::new(),
            masked: true,
            validate: Box::new(|text, _| text.is_empty().then(String::new)),
        });
        return spec;
    }
    let mut spec = Spec::new("Generate an SSH key", "Continue")
        .line(format!("ssh-keygen makes an Ed25519 key in {}.", folder.display()));
    if let Some(comment) = &ask.comment {
        spec = spec.line(format!("Its label is {comment}."));
    }
    spec = spec.line("FastKraken hands the passphrase to ssh-keygen itself, never on a command line. Leave it empty for a key without one.");
    spec.field = Some(Field {
        label: "Name",
        placeholder: "work",
        initial: ask.name.clone(),
        masked: false,
        validate: Box::new(move |text, _| name_problem(text, &folder)),
    });
    spec.second = Some(Field {
        label: "Passphrase",
        placeholder: "Passphrase",
        initial: String::new(),
        masked: true,
        validate: Box::new(|_, _| None),
    });
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask.passphrase {
        None if !answer.second.is_empty() => {
            let next = Ask { name: answer.text.trim().to_owned(), passphrase: Some(answer.second), comment: ask.comment };
            prompt::ask(&mut tab.ui, prompt::Kind::Keys(next), cx);
        }
        None => make(tab, answer.text.trim().to_owned(), String::new(), ask.comment, cx),
        Some(passphrase) if passphrase == answer.text => make(tab, ask.name, passphrase, ask.comment, cx),
        Some(_) => {
            tab.ui.notices.show(Notice::new(Tone::Info, "The passphrases did not match", "Type the same passphrase twice."));
            let again = Ask { name: ask.name, passphrase: None, comment: ask.comment };
            prompt::ask(&mut tab.ui, prompt::Kind::Keys(again), cx);
        }
    }
}

fn make(tab: &mut Tab, name: String, passphrase: String, comment: Option<String>, cx: &mut Context<Tab>) {
    let folder = tab.ui.keys.places.folder.clone();
    if let Some(problem) = name_problem(&name, &folder) {
        return tab.refuse(if problem.is_empty() { "Enter a name for the key.".to_owned() } else { problem }, cx);
    }
    let locked = !passphrase.is_empty();
    job(
        tab,
        "Generating the SSH key…",
        Some(passphrase),
        move || generate(&folder, &name, comment.as_deref()).and_then(|key| if locked { kept_locked(key) } else { Ok(key) }),
        move |_, key| {
            let place = key.public.parent().map(|folder| folder.display().to_string()).unwrap_or_default();
            let lock = if locked { "with a passphrase" } else { "with no passphrase" };
            Notice::new(Tone::Success, "SSH key generated", format!("{} is in {place}, {lock}. Copy its public key to your host.", key.name()))
                .offer("Copy public key", view::Action::CopyText { text: key.text.clone(), what: "public key" })
        },
        cx,
    );
}

fn line<T: Desk>(id: (&'static str, usize), title: String, detail: String, on: Option<bool>, action: Option<view::Action>, cx: &Context<T>) -> gpui::Stateful<Div> {
    let row = div()
        .id(id)
        .py(px(8.))
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(rgb(palette().line))
        .when_some(on, |d, on| d.child(radio(on)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(kit::fit(title).size(13.).color(palette().text))
                .child(kit::fit(detail).size(TEXT_SMALL).color(palette().text_3)),
        );
    match action {
        Some(action) => kit::pressable(row.cursor_pointer().focusable().focus(|s| s.bg(rgb(palette().hover))), action, cx),
        None => row,
    }
}

fn note(text: impl Into<SharedString>, colour: u32) -> Div {
    div().py_2().text_size(px(TEXT_SMALL)).text_color(rgb(colour)).child(text.into())
}

fn scope_row<T: Desk>(scope: Scope, chosen: bool, cx: &Context<T>) -> gpui::Stateful<Div> {
    let row = div()
        .id(("keys-scope", scope as usize))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(rgb(if chosen { palette().text } else { palette().text_2 }))
        .focusable()
        .focus(|s| s.text_color(rgb(palette().text)))
        .child(radio(chosen))
        .child(scope.title());
    kit::pressable(row, view::Action::Keys(Action::SetScope(scope)), cx)
}

pub(crate) fn signing_page<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.keys;
    let repository = !ui.repo.is_empty();
    let scope = state.scope_for(repository);
    let mut page = div().flex().flex_col();
    let signing = match &state.signing {
        None => return page.child(note("Reading your signing settings…", palette().text_3)),
        Some(Err(error)) => return page.child(note(error.clone(), palette().deleted)),
        Some(Ok(signing)) => signing,
    };
    let shown = signing.shown(scope);
    page = page.child(kit::muted("FastKraken writes these to Git's own configuration, so Git signs the same way from a terminal."));
    if repository {
        page = page.child(setting(
            "Where these apply",
            None,
            div().flex().flex_col().gap_1().child(scope_row(Scope::Global, scope == Scope::Global, cx)).child(scope_row(Scope::Local, scope == Scope::Local, cx)),
        ));
        if scope == Scope::Global && signing.local.any() {
            page = page.child(kit::muted(format!("{} has its own signing settings, which win there. Choose For this repository only to see them.", ui.repo)));
        }
    }
    let mut formats = kit::segmented();
    for (index, (label, format)) in [("GPG", Format::Gpg), ("SSH", Format::Ssh)].into_iter().enumerate() {
        formats = formats.child(kit::segment(("keys-format", index), label, None, shown.format == format, view::Action::Keys(Action::SetFormat(format)), cx));
    }
    page = page.child(setting("Format", Some("GPG signs with a key in your GPG keyring; SSH signs with an SSH key.".into()), formats));
    page = page.child(
        div().pt_4().pb_1().flex().items_center().justify_between().child(kit::label("KEY")).child(kit::link(
            "keys-reload",
            "Reload keys",
            view::Action::Keys(Action::Reload),
            cx,
        )),
    );
    let key = shown.key.clone();
    let mut known = false;
    let mut list = div().flex().flex_col();
    match shown.format {
        Format::Ssh => {
            for (index, found) in state.keys().iter().enumerate() {
                let on = key.as_deref().is_some_and(|key| found.chosen_by(key));
                known |= on;
                let value = found.public.to_string_lossy().into_owned();
                list = list.child(line(("keys-signing-ssh", index), found.name(), found.detail(), Some(on), Some(view::Action::Keys(Action::UseSigningKey(value))), cx));
            }
            for (index, found) in state.agent_keys().iter().enumerate() {
                let on = key.as_deref().is_some_and(|key| found.chosen_by(key));
                known |= on;
                list = list.child(line(("keys-signing-agent", index), found.title(), found.detail(), Some(on), Some(view::Action::Keys(Action::UseSigningKey(found.value()))), cx));
            }
            if state.keys().is_empty() && state.agent_keys().is_empty() {
                list = list.child(
                    div().py_2().flex().items_center().gap_2().child(kit::muted(format!("No SSH keys in {}.", state.places.folder.display()))).child(kit::link(
                        "keys-signing-generate",
                        "Generate one",
                        view::Action::Keys(Action::Generate),
                        cx,
                    )),
                );
            }
        }
        _ => match &state.gpg {
            None => list = list.child(note("Reading your GPG keys…", palette().text_3)),
            Some(Err(error)) => list = list.child(note(error.clone(), palette().text_3)),
            Some(Ok(keys)) => {
                if keys.is_empty() {
                    list = list.child(kit::muted("Your GPG keyring has no secret keys. Make one with gpg --full-generate-key."));
                }
                for (index, found) in keys.iter().enumerate() {
                    let on = key.as_deref().is_some_and(|key| found.chosen_by(key));
                    known |= on;
                    let detail = if found.usable { format!("{} · {}", found.id, found.fingerprint) } else { format!("{} · expired, revoked or unable to sign", found.id) };
                    let action = found.usable.then(|| view::Action::Keys(Action::UseSigningKey(found.id.clone())));
                    list = list.child(line(("keys-signing-gpg", index), found.user.clone(), detail, Some(on), action, cx));
                }
            }
        },
    }
    if !known && let Some(shown_key) = shown.key_shown() {
        list = list.child(line(("keys-signing-other", 0), shown_key, "Not found on this Mac".to_owned(), Some(true), None, cx));
    }
    page = page.child(list);
    let saving = state.saving;
    page = page
        .child(preferences::tick(
            "keys-sign-commits",
            shown.commits,
            "Sign commits by default",
            "Every commit is signed unless you untick Sign this commit in the commit box.",
            view::Action::Keys(Action::ToggleCommits),
            cx,
        ))
        .child(preferences::tick(
            "keys-sign-tags",
            shown.tags,
            "Sign tags by default",
            "New tags are signed unless you untick Sign this tag.",
            view::Action::Keys(Action::ToggleTags),
            cx,
        ));
    if saving {
        page = page.child(note("Saving…", palette().text_3));
    }
    page.when_some(state.error.clone(), |d, error| d.child(note(error, palette().deleted)))
}

pub(crate) fn ssh_page<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.keys;
    let repository = !ui.repo.is_empty();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let folder = crate::start::homely(&state.places.folder, home.as_deref());
    let used = state.used_here();
    let mut list = div().flex().flex_col();
    match &state.ssh {
        None => list = list.child(note("Reading your SSH keys…", palette().text_3)),
        Some(Err(error)) => list = list.child(note(error.clone(), palette().deleted)),
        Some(Ok(keys)) if keys.is_empty() => list = list.child(kit::muted(format!("No SSH keys in {folder} yet."))),
        Some(Ok(keys)) => {
            for (index, key) in keys.iter().enumerate() {
                let here = used.as_deref().is_some_and(|used| Path::new(used) == key.identity() || Path::new(used) == key.public);
                let mut controls = div().flex().flex_shrink_0().items_center().gap_2().child(
                    kit::btn(("keys-copy", index), "Copy public key")
                        .small()
                        .on(view::Action::CopyText { text: key.text.clone(), what: "public key" }, cx),
                );
                if let Some(private) = &key.private {
                    controls = controls.child(
                        kit::btn(("keys-agent", index), "Add to the agent")
                            .small()
                            .enabled(state.places.agent.is_some() && !state.working)
                            .on(view::Action::Keys(Action::AddToAgent(private.clone())), cx),
                    );
                }
                if repository {
                    controls = controls.child(if here {
                        kit::btn(("keys-use", index), "Used here").small().selected(true).on(view::Action::Keys(Action::UseForRepository(None)), cx)
                    } else {
                        kit::btn(("keys-use", index), "Use for this repository")
                            .small()
                            .enabled(!state.saving && matches!(state.command, Some(Ok(_))))
                            .on(view::Action::Keys(Action::UseForRepository(Some(key.identity().to_owned()))), cx)
                    });
                }
                list = list.child(
                    div()
                        .py(px(8.))
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(rgb(palette().line))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(kit::fit(key.name()).size(13.).color(palette().text))
                                .child(kit::fit(key.detail()).size(TEXT_SMALL).color(palette().text_3)),
                        )
                        .child(controls),
                );
            }
        }
    }
    let mut agent = div().flex().flex_col();
    match &state.agent {
        None => {}
        Some(Err(error)) => agent = agent.child(note(error.clone(), palette().text_3)),
        Some(Ok(keys)) if keys.is_empty() => agent = agent.child(kit::muted("The agent holds no keys.")),
        Some(Ok(keys)) => {
            for (index, key) in keys.iter().enumerate() {
                agent = agent.child(line(("keys-agent-key", index), key.title(), key.detail(), None, None, cx));
            }
        }
    }
    let mut page = div()
        .flex()
        .flex_col()
        .child(kit::muted(format!("Keys in {folder}. ssh offers them when Git connects to a remote over SSH.")))
        .child(list)
        .child(
            div().pt_3().flex().gap_2().child(
                kit::btn("keys-generate", "Generate key…")
                    .small()
                    .enabled(!state.working)
                    .on(view::Action::Keys(Action::Generate), cx),
            ),
        )
        .child(group("IN THE AGENT"))
        .child(agent);
    if repository {
        let words = match (&state.command, &used) {
            (Some(Ok(_)), Some(key)) => format!("{} connects with {key} only.", ui.repo),
            (Some(Ok(Some(command))), None) => format!("{} connects with its own SSH command: {command}", ui.repo),
            (Some(Err(error)), _) => error.clone(),
            _ => format!("{} connects with your usual SSH settings and ~/.ssh/config.", ui.repo),
        };
        page = page.child(group("THIS REPOSITORY")).child(
            div().py_2().flex().items_center().gap_3().child(div().flex_1().min_w_0().child(kit::muted(words))).when(
                matches!(state.command, Some(Ok(Some(_)))),
                |d| d.child(kit::link("keys-stop-using", "Stop using it", view::Action::Keys(Action::UseForRepository(None)), cx)),
            ),
        );
    }
    let mut hosts = div().py_2().flex().items_center().gap_4();
    for (index, host) in Host::ALL.into_iter().enumerate() {
        hosts = hosts.child(kit::link(
            ("keys-host", index),
            format!("Add a key on {}", host.name()),
            view::Action::Handoff(crate::handoff::Action::OpenUrl(host.page().to_owned())),
            cx,
        ));
    }
    page.child(group("ADD A KEY TO YOUR HOST"))
        .child(kit::muted("Copy a public key above, then paste it on your host's page for SSH keys."))
        .child(hosts)
        .when_some(state.error.clone(), |d, error| d.child(note(error, palette().deleted)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};
    use crate::signing::tests::{KEYGEN, gpg, keygen_present, ssh_key};

    fn temp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("fk-keys").tempdir_in("/tmp").unwrap()
    }

    fn script(path: &Path, body: &str) -> PathBuf {
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_owned()
    }

    fn snapshot(f: &Fixture) -> Vec<u8> {
        let mut seen = f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
        seen.extend(f.git(&["status", "--porcelain"]));
        seen.extend(f.git(&["ls-files", "--stage"]));
        seen
    }

    fn config(file: &Path, key: &str) -> Option<String> {
        let output = repository::bundled_command(Path::new("/")).unwrap().args(["config", "--file"]).arg(file).args(["--get", key]).output().unwrap();
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    }

    #[test]
    fn tests_never_look_in_the_users_own_ssh_folder_or_agent() {
        let places = Places::here();
        assert_eq!(places.folder, Path::new("/var/empty/fastkraken-test-ssh"));
        assert!(places.agent.is_none() && !places.keychain);
    }

    #[test]
    fn a_fingerprint_line_is_read_into_its_parts() {
        let line = printed("256 SHA256:2bPq4m0M1R9c cedar@example.invalid (ED25519)\n").unwrap();
        assert_eq!(
            line,
            Printed { bits: "256".into(), fingerprint: "SHA256:2bPq4m0M1R9c".into(), comment: "cedar@example.invalid".into(), kind: "ED25519".into() }
        );
        let bare = printed("3072 SHA256:abc no comment (RSA)").unwrap();
        assert_eq!((bare.comment.as_str(), bare.kind.as_str()), ("", "RSA"));
        let spaced = printed("256 SHA256:abc work laptop (ECDSA)").unwrap();
        assert_eq!(spaced.comment, "work laptop");
        assert_eq!(printed("not a key"), None);
    }

    #[test]
    fn gpg_secret_keys_are_read_from_the_colon_listing() {
        let listed = "sec:u:255:22:1234ABCD1234ABCD:1764115200:::u:::scESC:::+:::ed25519:::0:\nfpr:::::::::F00DF00DF00DF00DF00DF00D1234ABCD1234ABCD:\ngrp:::::::::ABC:\nuid:u::::1764115200::HASH::Cedar \\x3a Trail <cedar@example.invalid>::::::::::0:\nssb:u:255:18:9999AAAA9999AAAA:1764115200::::::e:::+:::cv25519::\nfpr:::::::::0000000000000000000000009999AAAA9999AAAA:\nsec:e:4096:1:5555EEEE5555EEEE:1600000000:1610000000::u:::sc:::+::::::0:\nfpr:::::::::ABCDEF00000000000000000000005555EEEE5555EEEE:\nuid:e::::1600000000::HASH::Old Key <old@example.invalid>::::::::::0:\n";
        let keys = parse_gpg(listed);
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[0],
            GpgKey {
                id: "1234ABCD1234ABCD".into(),
                fingerprint: "F00DF00DF00DF00DF00DF00D1234ABCD1234ABCD".into(),
                user: "Cedar : Trail <cedar@example.invalid>".into(),
                usable: true
            }
        );
        assert!(!keys[1].usable);
        assert!(keys[0].chosen_by("1234abcd1234abcd") && keys[0].chosen_by("0x1234ABCD1234ABCD!") && keys[0].chosen_by("F00DF00DF00DF00DF00DF00D1234ABCD1234ABCD"));
        assert!(keys[0].chosen_by("1234ABCD") && keys[0].chosen_by("cedar@example.invalid"));
        assert!(!keys[0].chosen_by("5555EEEE5555EEEE") && !keys[0].chosen_by(""));
    }

    #[test]
    fn a_key_for_one_repository_is_written_as_an_ssh_command_and_read_back() {
        assert_eq!(ssh_command_for(Path::new("/Users/cedar/.ssh/id_ed25519_work")), "ssh -i /Users/cedar/.ssh/id_ed25519_work -o IdentitiesOnly=yes");
        let spaced = ssh_command_for(Path::new("/Users/cedar/My Keys/it's"));
        assert_eq!(spaced, "ssh -i '/Users/cedar/My Keys/it'\\''s' -o IdentitiesOnly=yes");
        assert_eq!(key_in(&spaced).as_deref(), Some("/Users/cedar/My Keys/it's"));
        assert_eq!(key_in("ssh -i ~/.ssh/id_work -o IdentitiesOnly=yes").as_deref(), Some("~/.ssh/id_work"));
        assert_eq!(key_in("ssh -o ConnectTimeout=5"), None);
        assert_eq!(quoted("~/keys"), "'~/keys'");
    }

    #[test]
    fn a_new_keys_name_is_checked_and_a_name_in_use_is_refused() {
        let dir = temp();
        assert_eq!(name_problem("", dir.path()).as_deref(), Some(""));
        assert_eq!(name_problem("work", dir.path()), None);
        assert_eq!(name_problem("work.laptop-2_b", dir.path()), None);
        for bad in ["has space", "../up", "-flag", ".hidden", "a/b", "x.pub"] {
            assert!(name_problem(bad, dir.path()).is_some_and(|problem| problem.starts_with("Use letters")), "{bad}");
        }
        fs::write(dir.path().join("id_ed25519_work.pub"), "ssh-ed25519 AAAA work\n").unwrap();
        assert_eq!(name_problem("work", dir.path()), Some(format!("id_ed25519_work already exists in {}.", dir.path().display())));
        std::os::unix::fs::symlink("/nowhere", dir.path().join("id_ed25519_gone")).unwrap();
        assert!(name_problem("gone", dir.path()).is_some());
    }

    #[test]
    fn the_keys_in_a_folder_are_listed_with_their_type_and_fingerprint() {
        if !keygen_present() {
            return;
        }
        let dir = temp();
        assert_eq!(ssh_keys(&dir.path().join("missing")).unwrap(), Vec::new());
        let public = ssh_key(dir.path(), "trail");
        fs::write(dir.path().join("lonely.pub"), fs::read_to_string(&public).unwrap()).unwrap();
        fs::write(dir.path().join("broken.pub"), "not a key\n").unwrap();
        fs::write(dir.path().join("config"), "Host *\n").unwrap();
        let keys = ssh_keys(dir.path()).unwrap();
        let names: Vec<String> = keys.iter().map(SshKey::name).collect();
        assert_eq!(names, ["lonely", "trail"]);
        let trail = &keys[1];
        assert_eq!(trail.private.as_deref(), Some(dir.path().join("trail").as_path()));
        assert_eq!(keys[0].private, None);
        assert_eq!(keys[0].identity(), dir.path().join("lonely.pub"));
        let printed_now = Command::new(KEYGEN).arg("-lf").arg(&public).output().unwrap();
        let fingerprint = String::from_utf8_lossy(&printed_now.stdout).split_whitespace().nth(1).unwrap().to_owned();
        assert_eq!((trail.printed.kind.as_str(), trail.printed.fingerprint.as_str(), trail.printed.comment.as_str()), ("ED25519", fingerprint.as_str(), "trail"));
        assert_eq!(trail.text, fs::read_to_string(&public).unwrap().trim());
        assert!(trail.chosen_by(public.to_str().unwrap()) && trail.chosen_by(dir.path().join("trail").to_str().unwrap()));
        assert!(!trail.chosen_by(&format!("key::{}", trail.text)) && trail.chosen_by(&trail.text));
        assert!(agent_keys(None).unwrap_err().contains("No SSH agent"));
    }

    #[test]
    fn a_generated_key_takes_its_passphrase_from_the_app_and_never_from_its_arguments() {
        if !keygen_present() {
            return;
        }
        let dir = temp();
        let folder = dir.path().join("home/.ssh");
        let asked = dir.path().join("asked.log");
        let program = script(
            &dir.path().join("askpass.sh"),
            &format!("echo \"$1\" >> '{}'\n[ -n \"$FASTKRAKEN_ASKPASS_TOKEN\" ] && [ \"$SSH_ASKPASS_REQUIRE\" = force ] || exit 1\necho 'summit lantern'\n", asked.display()),
        );
        let made = generating(&folder, "trail", Some("cedar@example.invalid"));
        let args: Vec<String> = made.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let path = folder.join("id_ed25519_trail");
        assert_eq!(args, ["-q", "-t", "ed25519", "-C", "cedar@example.invalid", "-f", path.to_str().unwrap()]);
        assert!(made.get_envs().any(|(name, value)| name == "SSH_AUTH_SOCK" && value.is_none()));

        assert!(generate(&folder, "trail", None).unwrap_err().contains("cannot hand the passphrase"));
        let _program = crate::askpass::tests::Program::set(program);
        assert!(crate::jobs::Link::new(false).enter(|| generate(&folder, "trail", None)).is_err());
        assert!(!folder.exists(), "a refused key made its folder");
        let link = crate::jobs::Link::answering();
        let key = link.enter(|| generate(&folder, "trail", Some("cedar@example.invalid"))).unwrap();
        assert_eq!(key.public, folder.join("id_ed25519_trail.pub"));
        assert_eq!(key.private.as_deref(), Some(path.as_path()));
        assert_eq!((key.printed.kind.as_str(), key.printed.comment.as_str()), ("ED25519", "cedar@example.invalid"));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        let prompts: Vec<crate::askpass::Question> = fs::read_to_string(&asked).unwrap().lines().map(crate::askpass::Question::parse).collect();
        assert_eq!(
            prompts,
            [crate::askpass::Question::NewPassphrase { key: Some(path.display().to_string()) }, crate::askpass::Question::RepeatPassphrase]
        );
        let unlocked = Command::new(KEYGEN).args(["-y", "-P", "", "-f"]).arg(&path).env_remove("SSH_AUTH_SOCK").stdin(Stdio::null()).output().unwrap();
        assert!(!unlocked.status.success(), "the key has no passphrase");

        let before = (fs::read(&path).unwrap(), fs::read(&key.public).unwrap());
        let again = link.enter(|| generate(&folder, "trail", None)).unwrap_err();
        assert!(again.contains("already exists"), "{again}");
        assert_eq!((fs::read(&path).unwrap(), fs::read(&key.public).unwrap()), before);
        assert_eq!(fs::read_to_string(&asked).unwrap().lines().count(), 2);
    }

    #[test]
    fn a_key_made_without_the_chosen_passphrase_is_removed_and_refused() {
        if !keygen_present() {
            return;
        }
        let dir = temp();
        let folder = dir.path().join("home/.ssh");
        let program = script(&dir.path().join("askpass.sh"), "exit 1\n");
        let _program = crate::askpass::tests::Program::set(program);
        let link = crate::jobs::Link::answering();
        link.session().unwrap().choose_passphrase("summit lantern".into());
        let key = link.enter(|| generate(&folder, "trail", None)).unwrap();
        assert!(opens_without_passphrase(key.identity()));
        let refused = kept_locked(key).unwrap_err();
        assert!(refused.contains("id_ed25519_trail without the passphrase"), "{refused}");
        assert!(!folder.join("id_ed25519_trail").exists() && !folder.join("id_ed25519_trail.pub").exists());

        let opener = script(&dir.path().join("open.sh"), "echo 'summit lantern'\n");
        let _program = crate::askpass::tests::Program::set(opener);
        let locked = link.enter(|| generate(&folder, "trail", None)).unwrap();
        assert!(!opens_without_passphrase(locked.identity()));
        let kept = kept_locked(locked).unwrap();
        assert!(kept.identity().is_file() && kept.public.is_file());
    }

    #[test]
    fn adding_to_the_agent_uses_the_agent_given_and_the_keychain_only_when_asked() {
        let key = Path::new("/k/id_ed25519_trail");
        let agent = Path::new("/tmp/fk-agent.sock");
        let plain = adding(key, agent, false);
        let args: Vec<_> = plain.get_args().collect();
        assert_eq!(args, [key.as_os_str()]);
        assert!(plain.get_envs().any(|(name, value)| name == "SSH_AUTH_SOCK" && value == Some(agent.as_os_str())));
        assert!(plain.get_envs().any(|(name, value)| name == "SSH_ASKPASS_REQUIRE" && value.is_some_and(|value| value == "force")));
        if executable(Path::new(APPLE_SSH_ADD)) {
            let keychain = adding(key, agent, true);
            assert_eq!(keychain.get_program(), APPLE_SSH_ADD);
            assert_eq!(keychain.get_args().next().unwrap(), "--apple-use-keychain");
        }
        assert!(add_to_agent(key, None, false).unwrap_err().contains("No SSH agent"));
    }

    #[test]
    fn a_key_added_to_a_private_agent_is_listed_by_it() {
        if !keygen_present() || !executable(Path::new("/usr/bin/ssh-agent")) {
            return;
        }
        let dir = temp();
        let socket = dir.path().join("agent.sock");
        let mut agent = Command::new("/usr/bin/ssh-agent").arg("-D").arg("-a").arg(&socket).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let start = std::time::Instant::now();
        while !socket.exists() && start.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let public = ssh_key(dir.path(), "trail");
        let empty = agent_keys(Some(&socket));
        let added = add_to_agent(&dir.path().join("trail"), Some(&socket), true);
        let listed = agent_keys(Some(&socket));
        let _ = agent.kill();
        let _ = agent.wait();
        assert_eq!(empty.unwrap(), Vec::new());
        added.unwrap();
        let listed = listed.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].chosen_by(&format!("key::{}", fs::read_to_string(&public).unwrap().trim())));
        assert_eq!(listed[0].title(), "trail");
        assert!(listed[0].printed.as_ref().is_some_and(|printed| printed.kind == "ED25519"));
    }

    #[test]
    fn a_repository_is_given_its_own_key_and_a_changed_ssh_command_is_refused() {
        let f = Fixture::new();
        let before = snapshot(&f);
        let key = Path::new("/Users/cedar/.ssh/id_ed25519_work");
        let set = use_key(&f.repo, Some(key), &None).unwrap();
        assert_eq!(set.as_deref(), Some("ssh -i /Users/cedar/.ssh/id_ed25519_work -o IdentitiesOnly=yes"));
        assert_eq!(config(&f.repo.git_dir().join("config"), "core.sshCommand"), set);
        assert_eq!(snapshot(&f), before);
        f.git(&["config", "--local", "core.sshCommand", "ssh -i /elsewhere"]);
        let refused = use_key(&f.repo, Some(Path::new("/Users/cedar/.ssh/id_ed25519_home")), &set).unwrap_err();
        assert!(refused.contains("changed to ssh -i /elsewhere meanwhile"), "{refused}");
        assert_eq!(local_ssh_command(&f.repo).unwrap().as_deref(), Some("ssh -i /elsewhere"));
        let stopped = use_key(&f.repo, None, &Some("ssh -i /elsewhere".into())).unwrap();
        assert_eq!(stopped, None);
        assert_eq!(config(&f.repo.git_dir().join("config"), "core.sshCommand"), None);
        assert_eq!(use_key(&f.repo, None, &None).unwrap(), None);
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn signing_settings_are_read_by_scope() {
        let listed = b"global\0gpg.format\nssh\0global\0user.signingkey\n/k/a.pub\0local\0commit.gpgsign\ntrue\0local\0tag.gpgsign\0global\0gpg.program\n/opt/gpg\0global\0user.name\nCedar\0";
        let signing = parse_signing(listed);
        assert_eq!(signing.global.get(Setting::Format), Some("ssh"));
        assert_eq!(signing.local.get(Setting::Tags), Some("true"));
        assert_eq!(signing.local.get(Setting::Key), None);
        assert_eq!(signing.program.as_deref(), Some("/opt/gpg"));
        let local = signing.shown(Scope::Local);
        assert!(local.commits && local.tags && local.format == Format::Ssh && local.key.as_deref() == Some("/k/a.pub"));
        let global = signing.shown(Scope::Global);
        assert!(!global.commits && !global.tags && global.format == Format::Ssh);
        assert!(signing.local.any() && !Raw::default().any());
    }

    #[test]
    fn signing_settings_are_written_where_chosen_and_a_changed_setting_is_refused() {
        let f = Fixture::new();
        let dir = temp();
        let global_file = dir.path().join("gitconfig");
        fs::write(&global_file, "").unwrap();
        let _global = GlobalConfig::set(&global_file);
        let before = snapshot(&f);
        let local_file = f.repo.git_dir().join("config");
        let write = |scope, setting, value: &str, seen: Option<&str>| {
            change(Some(&f.repo), &Change { scope, setting, value: Some(value.to_owned()), seen: seen.map(str::to_owned) })
        };
        let fresh = |scope: Scope, setting: Setting, value: &str| {
            let seen = read_signing(Some(&f.repo)).unwrap().at(scope).get(setting).map(str::to_owned);
            write(scope, setting, value, seen.as_deref())
        };
        let signing = fresh(Scope::Global, Setting::Format, "ssh").unwrap();
        assert_eq!(config(&global_file, "gpg.format").as_deref(), Some("ssh"));
        assert_eq!(config(&local_file, "gpg.format"), None);
        assert_eq!(signing.global.get(Setting::Format), Some("ssh"));
        fresh(Scope::Global, Setting::Key, "/k/cedar.pub").unwrap();
        fresh(Scope::Global, Setting::Commits, "true").unwrap();
        let signing = fresh(Scope::Local, Setting::Commits, "false").unwrap();
        assert_eq!(config(&local_file, "commit.gpgsign").as_deref(), Some("false"));
        assert_eq!(config(&global_file, "commit.gpgsign").as_deref(), Some("true"));
        assert!(!signing.shown(Scope::Local).commits && signing.shown(Scope::Global).commits);
        let signing = fresh(Scope::Local, Setting::Tags, "true").unwrap();
        assert!(signing.shown(Scope::Local).tags);
        let identity = crate::identity::of(&f.repo).unwrap();
        assert_eq!(identity.signing.key.as_deref(), Some("/k/cedar.pub"));
        assert!(identity.signing.tags && !identity.signing.commits && identity.signing.format == Format::Ssh);

        f.git(&["config", "--local", "user.signingkey", "/k/other.pub"]);
        let refused = write(Scope::Local, Setting::Key, "/k/mine.pub", None).unwrap_err();
        assert_eq!(refused, "The signing key for this repository changed meanwhile. Check it and choose again.");
        assert_eq!(config(&local_file, "user.signingkey").as_deref(), Some("/k/other.pub"));
        fs::write(&global_file, "[gpg]\n\tformat = openpgp\n").unwrap();
        let refused = write(Scope::Global, Setting::Format, "ssh", Some("ssh")).unwrap_err();
        assert!(refused.contains("for every repository changed meanwhile"), "{refused}");
        assert_eq!(fs::read_to_string(&global_file).unwrap(), "[gpg]\n\tformat = openpgp\n");
        let unset = change(Some(&f.repo), &Change { scope: Scope::Local, setting: Setting::Key, value: None, seen: Some("/k/other.pub".into()) }).unwrap();
        assert_eq!(unset.local.get(Setting::Key), None);
        assert!(change(None, &Change { scope: Scope::Local, setting: Setting::Tags, value: Some("true".into()), seen: None }).is_err());
        let outside = change(None, &Change { scope: Scope::Global, setting: Setting::Tags, value: Some("true".into()), seen: None }).unwrap();
        assert_eq!(outside.global.get(Setting::Tags), Some("true"));
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn the_gpg_keys_in_an_isolated_keyring_are_listed() {
        let Some(gpg) = gpg() else { return };
        let f = Fixture::new();
        let serial = f.dir.file_name().unwrap().to_string_lossy().rsplit('-').next().unwrap().to_owned();
        let keyring = crate::signing::tests::Keyring::new(&gpg, PathBuf::from(format!("/tmp/fk-gpg-keys-{}-{serial}", std::process::id())));
        let program = keyring.program(&f.repo.git_dir().join("gpg.sh"));
        assert_eq!(gpg_keys(Some(&program)).unwrap(), Vec::new());
        let made = keyring.run(
            &["--gen-key"],
            "%no-protection\nKey-Type: eddsa\nKey-Curve: ed25519\nKey-Usage: sign\nName-Real: Cedar\nName-Email: cedar@example.invalid\nExpire-Date: 0\n%commit\n",
        );
        if !made.status.success() {
            return;
        }
        let keys = gpg_keys(Some(&program)).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].user, "Cedar <cedar@example.invalid>");
        assert!(keys[0].usable && keys[0].id.len() == 16 && keys[0].fingerprint.ends_with(&keys[0].id));
        assert!(gpg_keys(None).unwrap_err().starts_with("GPG is not installed"));
        assert_eq!(gpg_program(Some("/nowhere/gpg")), None);
    }
}
