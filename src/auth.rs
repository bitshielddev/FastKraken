#[cfg(feature = "publish-replay")]
mod replay;
mod vault;
#[cfg(feature = "publish-replay")]
pub(crate) use replay::begin_replay;
pub(crate) use vault::secret;

use crate::{
    github::{Client, Tokens},
    repository::{Repository, checked},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, BufRead, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;

pub(crate) const LAPSED: &str = "The GitHub sign-in has lapsed";

#[derive(Clone, Deserialize, Serialize)]
struct Registration {
    id: String,
    slug: String,
}

impl From<&Client> for Registration {
    fn from(client: &Client) -> Self {
        Self {
            id: client.client_id().into(),
            slug: client.slug().into(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Binding {
    directory: Vec<u8>,
    remote: String,
    url: String,
    registration: Registration,
    account: u64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Settings {
    registration: Option<Registration>,
    active: BTreeMap<String, u64>,
    bindings: Vec<Binding>,
}

pub(crate) fn replaying() -> bool {
    #[cfg(feature = "publish-replay")]
    {
        replay::SESSION.get().is_some()
    }
    #[cfg(not(feature = "publish-replay"))]
    {
        false
    }
}

fn directory() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = tests::DIRECTORY.with(|dir| dir.borrow().clone()) {
        return Ok(dir);
    }
    #[cfg(feature = "publish-replay")]
    if let Some(session) = replay::SESSION.get() {
        return Ok(session.directory.clone());
    }
    storage::directory()
}

fn load(dir: &Path) -> Result<Settings> {
    storage::load(dir, "github.json", "GitHub settings")
}

fn lock(dir: &Path, name: &str) -> Result<File> {
    storage::lock(dir, name)
}

fn update(dir: &Path, change: impl FnOnce(&mut Settings) -> Result<()>) -> Result<()> {
    storage::update(dir, "github.json", "github.lock", "GitHub settings", change)
}

pub(crate) fn client() -> Result<Client> {
    #[cfg(feature = "publish-replay")]
    if let Some(session) = replay::SESSION.get() {
        return match load(&session.directory)?.registration {
            Some(reg)
                if reg.id == session.client.client_id() && reg.slug == session.client.slug() =>
            {
                Ok(session.client.clone())
            }
            _ => Err("Set the public client ID and slug from your GitHub App registration.".into()),
        };
    }
    match load(&directory()?)?.registration {
        Some(reg) => Client::new(&reg.id, &reg.slug),
        None => Client::configured(),
    }
}

pub(crate) fn has_built_in() -> bool {
    !replaying() && Client::configured().is_ok()
}

pub(crate) fn use_built_in() -> Result<()> {
    Client::configured()?;
    update(&directory()?, |s| {
        s.registration = None;
        Ok(())
    })
}

pub(crate) fn configure(id: &str, slug: &str) -> Result<Client> {
    let client = Client::new(id.trim(), slug.trim())?;
    update(&directory()?, |s| {
        s.registration = Some((&client).into());
        Ok(())
    })?;
    Ok(client)
}

fn token_lock(dir: &Path, client: &Client, account: u64) -> Result<File> {
    lock(
        dir,
        &format!("github-{}-{account}.lock", client.client_id()),
    )
}

pub(crate) fn save(client: &Client, account: u64, tokens: &Tokens) -> Result<()> {
    let dir = directory()?;
    let _guard = token_lock(&dir, client, account)?;
    vault::put(client.client_id(), account, tokens)?;
    update(&dir, |s| {
        s.active.insert(client.client_id().into(), account);
        Ok(())
    })
}

pub(crate) fn active(client: &Client) -> Result<Option<(u64, Tokens)>> {
    let dir = directory()?;
    let Some(account) = load(&dir)?.active.get(client.client_id()).copied() else {
        return Ok(None);
    };
    Ok(fresh(&dir, client, account)?.map(|t| (account, t)))
}

fn fresh(dir: &Path, client: &Client, account: u64) -> Result<Option<Tokens>> {
    let _guard = token_lock(dir, client, account)?;
    let Some(tokens) = vault::get(client.client_id(), account)? else {
        return Ok(None);
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    if tokens.expires_at.is_some_and(|end| end <= now + 90) {
        let refresh = tokens
            .refresh_token
            .as_deref()
            .ok_or_else(|| format!("{LAPSED}. Reconnect GitHub."))?;
        let next = client.refresh(refresh).map_err(|e| match e {
            crate::github::Error::Unauthorized(said) => format!("{LAPSED}. {said}"),
            e => format!("Renew GitHub sign-in: {e}"),
        })?;
        vault::put(client.client_id(), account, &next)?;
        return Ok(Some(next));
    }
    Ok(Some(tokens))
}

pub(crate) fn disconnect(client: &Client, expected: Option<u64>) -> Result<()> {
    let dir = directory()?;
    let Some(account) = still_shown(load(&dir)?.active.get(client.client_id()).copied(), expected)?
    else {
        return Ok(());
    };
    let _guard = token_lock(&dir, client, account)?;
    vault::remove(client.client_id(), account)?;
    update(&dir, |s| {
        if s.active.get(client.client_id()) == Some(&account) {
            s.active.remove(client.client_id());
        }
        Ok(())
    })
}

fn still_shown(active: Option<u64>, expected: Option<u64>) -> Result<Option<u64>> {
    match (active, expected) {
        (Some(active), Some(expected)) if active != expected => Err(
            "Another GitHub account was connected since. Review it and choose Disconnect again."
                .into(),
        ),
        _ => Ok(active),
    }
}

fn repo_path(url: &str) -> Option<&str> {
    let path = url.strip_prefix("https://github.com/")?;
    let (owner, repo) = path.strip_suffix(".git").unwrap_or(path).split_once('/')?;
    let segment = |s: &str| {
        !s.is_empty()
            && !matches!(s, "." | "..")
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c))
    };
    (segment(owner) && segment(repo)).then_some(path)
}

pub(crate) fn github_repo(url: &str) -> Option<(String, String)> {
    let path = repo_path(url)?;
    let (owner, name) = path.strip_suffix(".git").unwrap_or(path).split_once('/')?;
    Some((owner.to_owned(), name.to_owned()))
}

pub(crate) fn bindings(repo: &Repository) -> Result<Vec<(String, String)>> {
    let dir = repo.common_dir().as_os_str().as_bytes();
    Ok(load(&directory()?)?
        .bindings
        .into_iter()
        .filter(|b| b.directory == dir)
        .map(|b| (b.remote, b.url))
        .collect())
}

pub(crate) fn bound(repo: &Repository, remote: &str) -> Result<Option<String>> {
    Ok(bindings(repo)?.into_iter().find(|(name, _)| name == remote).map(|(_, url)| url))
}

pub(crate) fn bind(
    repo: &Repository,
    remote: &str,
    url: &str,
    client: &Client,
    account: u64,
) -> Result<()> {
    repo_path(url).ok_or("The remote must be the repository’s GitHub HTTPS clone URL.")?;
    crate::remotes::validate_name(remote)?;
    verify_push_url(repo, remote, url)?;
    let dir = repo.common_dir().as_os_str().as_bytes();
    update(&directory()?, |s| {
        s.bindings
            .retain(|b| !(b.directory == dir && b.remote == remote));
        if s.bindings.iter().any(|b| {
            b.directory == dir
                && b.url.eq_ignore_ascii_case(url)
                && (b.account != account || b.registration.id != client.client_id())
        }) {
            return Err(
                "This checkout already connects to that GitHub repository with another account."
                    .into(),
            );
        }
        s.bindings.push(Binding {
            directory: dir.into(),
            remote: remote.into(),
            url: url.into(),
            registration: client.into(),
            account,
        });
        Ok(())
    })
}

fn verify_push_url(repo: &Repository, remote: &str, expected: &str) -> Result<()> {
    crate::remotes::check_transport(repo, remote)?;
    let urls = checked(
        repo.read_command()
            .args(["remote", "get-url", "--push", "--all", "--"])
            .arg(remote),
    )
    .map_err(|_| "Cannot resolve the configured push destination.".to_owned())?;
    if urls.strip_suffix(b"\n") != Some(expected.as_bytes()) {
        return Err("The push destination changed or has multiple URLs. Confirm a single destination in Remotes → Add existing remote before publishing.".into());
    }
    Ok(())
}

pub(crate) fn check_push_destination(repo: &Repository, remote: &OsStr) -> Result<()> {
    let dir = repo.common_dir().as_os_str().as_bytes();
    if let Some(b) = load(&directory()?)?
        .bindings
        .iter()
        .find(|b| b.directory == dir && b.remote.as_bytes() == remote.as_bytes())
    {
        verify_push_url(repo, &b.remote, &b.url)?;
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn confirm_existing(repo: &Repository, remote: &str, url: &str) -> Result<()> {
    let dir = directory()?;
    let common = repo.common_dir().as_os_str().as_bytes();
    let obsolete = |b: &Binding| b.directory == common && b.remote == remote && b.url != url;
    if load(&dir)?.bindings.iter().any(obsolete) {
        verify_push_url(repo, remote, url)?;
        update(&dir, |s| {
            s.bindings.retain(|b| !obsolete(b));
            Ok(())
        })?;
    }
    Ok(())
}

pub(crate) fn remote_edited(repo: &Repository, old: &str, new: Option<&str>) -> Result<()> {
    let dir = directory()?;
    let common = repo.common_dir().as_os_str().as_bytes();
    let matches = |binding: &Binding| {
        binding.directory == common
            && (binding.remote == old || new == Some(binding.remote.as_str()))
    };
    if !load(&dir)?.bindings.iter().any(matches) {
        return Ok(());
    }
    update(&dir, |settings| {
        let previous = settings
            .bindings
            .iter()
            .find(|binding| binding.directory == common && binding.remote == old)
            .cloned();
        settings.bindings.retain(|binding| !matches(binding));
        if let (Some(mut binding), Some(name)) = (previous, new) {
            binding.remote = name.to_owned();
            settings.bindings.push(binding);
        }
        Ok(())
    })
}

pub(crate) fn configure_transport(repo: &Repository, command: &mut Command) -> Result<()> {
    #[cfg(feature = "publish-replay")]
    if let Some(session) = replay::SESSION.get() {
        command.arg("-c").arg(format!(
            "url.{}.insteadOf=https://github.com/TrailTeam/shared-ledger.git",
            session.git_destination.display()
        ));
    }
    let dir = repo.common_dir().as_os_str().as_bytes();
    let settings = load(&directory()?)?;
    let bindings: Vec<_> = settings
        .bindings
        .iter()
        .filter(|b| b.directory == dir)
        .collect();
    if bindings.is_empty() {
        return Ok(());
    }
    let executable = env::current_exe().map_err(|e| e.to_string())?;
    let exe = executable
        .to_str()
        .ok_or("The credential helper executable needs a UTF-8 path.")?;
    command.env("FASTKRAKEN_CREDENTIAL_REPOSITORY", &repo.root);
    for b in bindings {
        repo_path(&b.url).ok_or("Invalid stored GitHub destination.")?;
        let prefix = format!("credential.{}.", b.url);
        let helper = format!(
            "!exec {} --git-credential {}",
            shell_quote(exe),
            shell_quote(&b.url)
        );
        command
            .arg("-c")
            .arg(format!("{prefix}helper="))
            .arg("-c")
            .arg(format!("{prefix}helper={helper}"))
            .arg("-c")
            .arg(format!("{prefix}useHttpPath=true"))
            .arg("-c")
            .arg(format!("{prefix}username=x-access-token"));
    }
    Ok(())
}

pub(crate) fn git_credential(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let url = args
        .next()
        .and_then(|a| a.into_string().ok())
        .ok_or("Missing credential destination.")?;
    let expected = repo_path(&url).ok_or("Unsupported credential destination.")?;
    let verb = args.next().ok_or("Missing credential operation.")?;
    if args.next().is_some() {
        return Err("Unexpected credential arguments.".into());
    }
    if verb != "get" {
        return Ok(());
    }
    let input = io::stdin().lock();
    if !matches_request(input, expected)? {
        return Err("Credential destination does not match the selected repository.".into());
    }
    let root = env::var_os("FASTKRAKEN_CREDENTIAL_REPOSITORY")
        .ok_or("Missing checkout for the Git credential request.")?;
    let repo = Repository::open(Path::new(&root))?;
    let dir = directory()?;
    let settings = load(&dir)?;
    let binding = settings
        .bindings
        .iter()
        .find(|b| b.directory == repo.common_dir().as_os_str().as_bytes() && b.url == url)
        .ok_or("This repository has no matching GitHub account.")?;
    let client = Client::new(&binding.registration.id, &binding.registration.slug)?;
    let tokens = fresh(&dir, &client, binding.account)?
        .ok_or_else(|| format!("{LAPSED}. Reconnect GitHub."))?;
    if tokens.access_token.is_empty() || tokens.access_token.bytes().any(|b| b.is_ascii_control()) {
        return Err("Invalid GitHub credential. Reconnect the account.".into());
    }
    writeln!(
        io::stdout().lock(),
        "username=x-access-token\npassword={}\n",
        tokens.access_token
    )
    .map_err(|_| "Cannot return the credential to Git.".into())
}

fn matches_request(input: impl BufRead, expected: &str) -> Result<bool> {
    let mut fields = BTreeMap::new();
    for line in input.take(16 * 1024).lines() {
        let line = line.map_err(|_| "Invalid credential request.")?;
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once('=') {
            if matches!(k, "protocol" | "host" | "path")
                && fields.insert(k.to_owned(), v.to_owned()).is_some()
            {
                return Ok(false);
            }
        }
    }
    Ok(fields.get("protocol").map(String::as_str) == Some("https")
        && fields.get("host").map(String::as_str) == Some("github.com")
        && fields
            .get("path")
            .is_some_and(|p| p.eq_ignore_ascii_case(expected)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(super) static DIRECTORY: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(crate) struct Apart(tempfile::TempDir);

    impl Apart {
        pub(crate) fn new() -> Self {
            let dir = tempfile::Builder::new().prefix("fastkraken-github-test").tempdir().unwrap();
            DIRECTORY.with(|cell| *cell.borrow_mut() = Some(dir.path().to_owned()));
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            self.0.path()
        }
    }

    impl Drop for Apart {
        fn drop(&mut self) {
            DIRECTORY.with(|cell| *cell.borrow_mut() = None);
        }
    }

    #[test]
    fn a_sign_in_that_cannot_be_renewed_has_lapsed_and_never_touches_the_keychain() {
        assert!(vault::secret::kept_apart());
        let apart = Apart::new();
        let client = Client::for_test("http://127.0.0.1:9/");
        let expired = |refresh: Option<&str>| Tokens { access_token: "old".into(), refresh_token: refresh.map(str::to_owned), expires_at: Some(1) };
        vault::put(client.client_id(), 4242, &expired(None)).unwrap();
        assert!(fresh(apart.path(), &client, 4242).err().unwrap().starts_with(LAPSED));
        vault::put(client.client_id(), 4242, &expired(Some("refresh-fixture"))).unwrap();
        crate::github::tests::mock(
            vec![crate::github::tests::response(200, serde_json::json!({"error":"bad_refresh_token"})), String::new()],
            |client| {
                let refused = fresh(apart.path(), &client, 4242).err().unwrap();
                assert!(refused.starts_with(LAPSED), "{refused}");
                let unanswered = fresh(apart.path(), &client, 4242).err().unwrap();
                assert!(unanswered.starts_with("Renew GitHub sign-in"), "{unanswered}");
            },
        );
        vault::remove(client.client_id(), 4242).unwrap();
        assert!(fresh(apart.path(), &client, 4242).unwrap().is_none());
    }

    #[test]
    fn a_remote_added_by_hand_without_dot_git_can_be_bound() {
        assert_eq!(github_repo("https://github.com/TrailTeam/ledger"), Some(("TrailTeam".into(), "ledger".into())));
        assert_eq!(github_repo("https://github.com/TrailTeam/ledger.git"), Some(("TrailTeam".into(), "ledger".into())));
        for url in ["https://github.com/TrailTeam/ledger/", "https://github.com/TrailTeam", "git@github.com:TrailTeam/ledger.git"] {
            assert_eq!(github_repo(url), None, "{url}");
        }
        let expected = repo_path("https://github.com/TrailTeam/ledger").unwrap();
        let req = "protocol=https\nhost=github.com\npath=TrailTeam/ledger\n\n";
        assert!(matches_request(req.as_bytes(), expected).unwrap());
        let other = "protocol=https\nhost=github.com\npath=TrailTeam/ledger.git\n\n";
        assert!(!matches_request(other.as_bytes(), expected).unwrap());
    }

    #[test]
    fn disconnect_leaves_an_account_other_than_the_one_shown() {
        assert_eq!(still_shown(Some(7), Some(7)), Ok(Some(7)));
        assert!(still_shown(Some(8), Some(7)).unwrap_err().contains("Another GitHub account"));
        assert_eq!(still_shown(None, Some(7)), Ok(None));
        assert_eq!(still_shown(Some(8), None), Ok(Some(8)));
    }

    #[test]
    fn credentials_are_limited_to_the_exact_github_repository() {
        let expected = repo_path("https://github.com/dan/.github.git").unwrap();
        for url in [
            "https://github.com@example.org/a/b.git",
            "https://github.com/a/b.git?q=yes",
            "http://github.com/a/b.git",
            "https://github.com/a/../b.git",
            "https://github.com/a/b.git/sub",
        ] {
            assert!(repo_path(url).is_none());
        }
        for (host, path, ok) in [
            ("github.com", "Dan/.github.git", true),
            ("git.example.net", "dan/.github.git", false),
            ("github.com", "dan/.github.git/other", false),
            ("github.com:8080", "dan/.github.git", false),
        ] {
            let req = format!("protocol=https\nhost={host}\npath={path}\n\n");
            assert_eq!(matches_request(req.as_bytes(), expected).unwrap(), ok);
        }
    }
}
