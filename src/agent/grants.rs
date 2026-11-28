use super::{Error, Result};
use crate::{repository::Repository, storage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub(super) fn nonce() -> Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| Error::new("app_unavailable", "Secure randomness is unavailable."))?;
    Ok(hex::encode(bytes))
}

pub(super) fn digest(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

const MAX_GRANTS: usize = 256;
const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;

fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.trim() == label
        && label.len() <= 128
        && !label.chars().any(char::is_control)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Profile {
    ReadOnly,
    LocalCommit,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantInfo {
    pub id: String,
    pub label: String,
    pub profile: Profile,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Directory {
    path: Vec<u8>,
    device: u64,
    inode: u64,
}

impl Directory {
    fn capture(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path).map_err(|_| Error::denied())?;
        let stat = fs::metadata(&path).map_err(|_| Error::denied())?;
        Ok(Self {
            path: path.as_os_str().as_bytes().to_vec(),
            device: stat.dev(),
            inode: stat.ino(),
        })
    }
    fn path(&self) -> &Path {
        Path::new(std::ffi::OsStr::from_bytes(&self.path))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Worktree {
    root: Directory,
    git: Directory,
    common: Directory,
}

impl Worktree {
    pub fn capture(repo: &Repository) -> Result<Self> {
        Ok(Self {
            root: Directory::capture(&repo.root)?,
            git: Directory::capture(repo.git_dir())?,
            common: Directory::capture(repo.common_dir())?,
        })
    }

    /// The authenticated worker and the GUI tab must refer to the same opened
    /// worktree, including the per-worktree Git directory of a linked checkout.
    pub(crate) fn matches_opened(&self, repo: &Repository) -> bool {
        self.root.path() == repo.root
            && self.git.path() == repo.git_dir()
            && self.common.path() == repo.common_dir()
            && repo.agent_matches_identity(
                [&self.root, &self.git, &self.common].map(|d| (d.device, d.inode)),
            )
    }

    fn opened(repo: &Repository) -> Self {
        let [root, git, common] = repo.opened_identity();
        let directory = |path: &Path, (device, inode)| Directory {
            path: path.as_os_str().as_bytes().to_vec(),
            device,
            inode,
        };
        Self {
            root: directory(&repo.root, root),
            git: directory(repo.git_dir(), git),
            common: directory(repo.common_dir(), common),
        }
    }

    pub fn reopen(&self) -> Result<Repository> {
        let repo = crate::agent_git::open(self.root.path()).map_err(|_| Error::denied())?;
        if !self.matches_opened(&repo) || Self::capture(&repo)? != *self {
            return Err(Error::denied());
        }
        Ok(repo)
    }

    pub fn id(&self) -> String {
        digest(&serde_json::to_vec(self).expect("worktree identity is serializable"))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    info: GrantInfo,
    verifier: String,
    worktree: Worktree,
}

// Keep the verifier out of operation state, logs and the UI.
#[derive(Clone)]
pub(super) struct Authorized {
    pub info: GrantInfo,
    pub worktree: Worktree,
}

pub(super) struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(state: &Path) -> Self {
        Self {
            dir: state.join("agent-access-v1"),
        }
    }

    fn private(&self) -> Result<()> {
        fs::create_dir_all(self.dir.parent().ok_or_else(Error::denied)?)
            .map_err(|_| Error::denied())?;
        match DirBuilder::new().mode(0o700).create(&self.dir) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Error::denied()),
        }
        crate::agent_transport::validate_directory(&self.dir).map_err(|_| Error::denied())
    }

    fn load(&self) -> Result<Vec<Stored>> {
        self.private()?;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.dir.join("grants.json"))
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(Error::denied()),
        };
        let meta = file.metadata().map_err(|_| Error::denied())?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.len() > MAX_STATE_BYTES
        {
            return Err(Error::denied());
        }
        let entries: Vec<Stored> =
            serde_json::from_reader(file.take(MAX_STATE_BYTES)).map_err(|_| Error::denied())?;
        let mut ids = HashSet::new();
        let mut verifiers = HashSet::new();
        if entries.len() > MAX_GRANTS
            || entries.iter().any(|e| {
                !valid_label(&e.info.label)
                    || !valid_id(&e.info.id)
                    || !valid_id(&e.verifier)
                    || !ids.insert(&e.info.id)
                    || !verifiers.insert(&e.verifier)
                    || [&e.worktree.root, &e.worktree.git, &e.worktree.common]
                        .iter()
                        .any(|d| !d.path().is_absolute() || d.path.contains(&0))
            })
        {
            return Err(Error::denied());
        }
        Ok(entries)
    }

    fn change<T>(&self, f: impl FnOnce(&mut Vec<Stored>) -> Result<T>) -> Result<T> {
        self.private()?;
        let _lock = storage::lock(&self.dir, "grants.lock").map_err(|_| Error::denied())?;
        let mut entries = self.load()?;
        let result = f(&mut entries)?;
        let bytes = serde_json::to_vec_pretty(&entries).map_err(|_| Error::denied())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(Error::new(
                "busy",
                "Revoke an unused grant before adding another.",
            ));
        }
        storage::write(&self.dir, "grants.json", "agent grants", &entries).map_err(|_| {
            Error::new(
                "app_unavailable",
                "Could not save the agent access settings.",
            )
        })?;
        Ok(result)
    }

    pub fn issue(&self, repo: &Repository, label: &str, profile: Profile) -> Result<String> {
        let label = label.trim();
        if !valid_label(label) {
            return Err(Error::invalid(
                "Enter a client label of 1 to 128 bytes without control characters.",
            ));
        }
        // Re-resolve a linked worktree's .git file as well as the canonical
        // directories. A stale tab cannot mint a grant for its replacement.
        let worktree = Worktree::opened(repo);
        worktree.reopen()?;
        let token = nonce()?;
        self.change(|entries| {
            if entries.len() >= MAX_GRANTS {
                return Err(Error::new("busy", "Revoke an unused grant first."));
            }
            entries.push(Stored {
                info: GrantInfo {
                    id: nonce()?,
                    label: label.into(),
                    profile,
                },
                worktree,
                verifier: digest(token.as_bytes()),
            });
            Ok(token)
        })
    }

    pub fn authenticate(&self, token: &str) -> Result<Authorized> {
        if !valid_id(token) {
            return Err(Error::denied());
        }
        let verifier = digest(token.as_bytes());
        let grant = self
            .load()?
            .into_iter()
            .find(|entry| entry.verifier == verifier)
            .ok_or_else(Error::denied)?;
        Ok(Authorized {
            info: grant.info,
            worktree: grant.worktree,
        })
    }

    pub fn grants(&self, repo: &Repository) -> Result<Vec<GrantInfo>> {
        let identity = Worktree::opened(repo);
        Ok(self
            .load()?
            .into_iter()
            .filter(|v| v.worktree == identity)
            .map(|v| v.info)
            .collect())
    }

    pub fn revoke(&self, repo: &Repository, id: &str) -> Result<()> {
        let identity = Worktree::opened(repo);
        self.change(|entries| {
            let at = entries
                .iter()
                .position(|v| v.info.id == id && v.worktree == identity)
                .ok_or_else(Error::denied)?;
            entries.remove(at);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn independent_grants_rebind_to_the_exact_linked_worktree_and_fail_closed() {
        let repo = Fixture::new();
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path());
        let a = store.issue(&repo.repo, "first", Profile::ReadOnly).unwrap();
        let b = store
            .issue(&repo.repo, "second", Profile::ReadOnly)
            .unwrap();
        let main = store.authenticate(&a).unwrap();
        assert!(store.authenticate("").is_err());
        repo.git(&["commit", "--allow-empty", "-qm", "initial"]);
        let path = temp.path().join("linked");
        repo.git(&["worktree", "add", "-qb", "linked", path.to_str().unwrap()]);
        let linked = repo.open(&path);
        assert!(store.grants(&linked).unwrap().is_empty());
        assert!(store.revoke(&linked, &main.info.id).is_err());
        store.revoke(&repo.repo, &main.info.id).unwrap();
        assert!(store.authenticate(&a).is_err());
        assert_eq!(store.authenticate(&b).unwrap().info.label, "second");
        let file = store.dir.join("grants.json");
        assert!(!fs::read_to_string(&file).unwrap().contains(&b));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.authenticate(&b).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(file, b"bad JSON").unwrap();
        assert!(store.authenticate(&b).is_err());
    }

    #[test]
    fn duplicate_persisted_grant_ids_or_verifiers_fail_closed_for_every_client() {
        let fixture = Fixture::new();
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path());
        let one = store
            .issue(&fixture.repo, "read", Profile::ReadOnly)
            .unwrap();
        let two = store
            .issue(&fixture.repo, "write", Profile::LocalCommit)
            .unwrap();
        let path = store.dir.join("grants.json");
        let saved: Vec<Stored> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for duplicate_id in [true, false] {
            let mut records = saved.clone();
            if duplicate_id {
                records[1].info.id = records[0].info.id.clone();
            } else {
                records[1].verifier = records[0].verifier.clone();
            }
            fs::write(&path, serde_json::to_vec(&records).unwrap()).unwrap();
            assert!(store.authenticate(&one).is_err());
            assert!(store.authenticate(&two).is_err());
        }
    }

    #[test]
    fn a_retargeted_linked_worktree_file_cannot_mint_a_grant_from_the_old_tab() {
        let fixture = Fixture::new();
        fixture.git(&["commit", "--allow-empty", "-qm", "base"]);
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path());
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        fixture.git(&["worktree", "add", "-qb", "one", one.to_str().unwrap()]);
        fixture.git(&["worktree", "add", "-qb", "two", two.to_str().unwrap()]);
        let tab = fixture.open(&one);
        let token = store.issue(&tab, "one", Profile::ReadOnly).unwrap();
        let old = store.authenticate(&token).unwrap();
        fs::copy(two.join(".git"), one.join(".git")).unwrap();
        assert!(old.worktree.reopen().is_err());
        assert_eq!(
            store
                .issue(&tab, "stale", Profile::LocalCommit)
                .unwrap_err()
                .code,
            "access_denied"
        );
        assert!(!old.worktree.matches_opened(&fixture.open(&one)));
        store.revoke(&tab, &old.info.id).unwrap();
    }

    #[test]
    fn replacement_at_the_same_path_requires_a_new_grant() {
        let repo = Fixture::new();
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path());
        let token = store.issue(&repo.repo, "old", Profile::ReadOnly).unwrap();
        let original = store.authenticate(&token).unwrap();
        let identity = Worktree::capture(&repo.repo).unwrap();
        let path = repo.repo.git_dir();
        fs::rename(path, repo.dir.join("old-git")).unwrap();
        repo.git(&["init", "-qb", "main"]);
        assert!(identity.reopen().is_err());
        assert_eq!(
            store
                .issue(&repo.repo, "stale", Profile::LocalCommit)
                .unwrap_err()
                .code,
            "access_denied"
        );
        let replacement = repo.open(&repo.dir);
        let new_token = store.issue(&replacement, "new", Profile::ReadOnly).unwrap();
        let new = store.authenticate(&new_token).unwrap();
        assert!(!new.worktree.matches_opened(&repo.repo));
        assert!(!original.worktree.matches_opened(&replacement));
        assert!(new.worktree.matches_opened(&replacement));
        assert_eq!(store.grants(&repo.repo).unwrap().len(), 1);
        store.revoke(&repo.repo, &original.info.id).unwrap();
        assert!(store.authenticate(&token).is_err());
        assert!(store.authenticate(&new_token).is_ok());
    }
}
