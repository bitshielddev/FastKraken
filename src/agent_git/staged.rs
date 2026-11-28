use super::{
    GitError, Patch, Result, changed, label,
    local::{self, Config, Local, checked, run},
    patch,
    pinned::{Lock, Pins},
    unavailable, unsupported,
};
use crate::repository::{DiffOptions, Repository, parse_changed_files, patch_options};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reviewed {
    branch: OsString,
    expected_head: Option<String>,
    tree: String,
    entries: [u8; 32],
    identity: Identity,
}
impl Reviewed {
    pub(crate) fn size(&self) -> usize {
        self.branch.len()
            + self
                .identity
                .0
                .iter()
                .map(|value| value.len())
                .sum::<usize>()
            + 512
    }
}
#[derive(Debug, serde::Serialize)]
pub(crate) struct Committed {
    pub branch: String,
    pub parent: Option<String>,
    pub commit: String,
    pub tree: String,
}
pub(crate) struct ReviewCapture {
    pub review: Reviewed,
    pub data: Value,
    pub patch: Patch,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity([OsString; 4]);
impl Identity {
    fn apply(&self, cmd: &mut Command) {
        for (key, value) in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ]
        .into_iter()
        .zip(&self.0)
        {
            cmd.env(key, value);
        }
    }
}
fn get<'a>(config: &'a Config, key: &str) -> Option<&'a OsStr> {
    config
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_os_str())
}
fn policy(repo: &Repository, config: &Config, deadline: Instant) -> Result<Identity> {
    // The restricted child environment deliberately excludes invocation-level
    // Git settings. Refuse an inherited policy/identity we cannot reproduce.
    let injected =
        std::env::var_os("GIT_CONFIG_COUNT").is_some_and(|value| !value.is_empty() && value != "0");
    if injected
        || [
            "GIT_CONFIG_PARAMETERS",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
    {
        return Err(unsupported(
            "The desktop inherited a Git policy or identity override. Commit in FastKraken using that environment.",
        ));
    }
    if let Some(shared) = get(config, "core.sharedrepository")
        && !matches!(
            shared.to_string_lossy().to_ascii_lowercase().as_str(),
            "0" | "false" | "umask"
        )
    {
        return Err(unsupported(
            "Commit shared-permission repositories in FastKraken.",
        ));
    }
    if let Some(monitor) = get(config, "core.fsmonitor")
        && !matches!(
            monitor.to_string_lossy().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    {
        return Err(unsupported(
            "A filesystem monitor is configured. Commit in FastKraken for its normal repository refresh.",
        ));
    }
    if let Some(sign) = get(config, "commit.gpgsign")
        && !matches!(
            sign.to_string_lossy().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    {
        return Err(unsupported(
            "Signing is required. Sign the staged commit in FastKraken.",
        ));
    }
    if let Some(encoding) = get(config, "i18n.commitencoding")
        && !encoding.as_bytes().eq_ignore_ascii_case(b"utf-8")
        && !encoding.as_bytes().eq_ignore_ascii_case(b"utf8")
    {
        return Err(unsupported(
            "Commit in FastKraken to use the configured message encoding.",
        ));
    }
    // Git 2.56 resolves both executable hooks and configured hook.<name>.event
    // declarations, including conditional configuration and relative hooksPath.
    for event in crate::hooks::COMMIT
        .iter()
        .copied()
        .chain(["reference-transaction", "post-index-change"])
    {
        let mut cmd = local::configured_command(repo)?;
        let out = run(
            cmd.args(["hook", "list", "-z", "--", event]),
            None,
            64 * 1024,
            deadline,
        )?;
        if out.status.code() != Some(1) || !out.bytes.is_empty() || out.truncated {
            return Err(unsupported(
                "A commit or reference hook requires validation. Commit in FastKraken to run it.",
            ));
        }
    }
    let component = |key, default| {
        let value = get(config, key)
            .or_else(|| get(config, default))
            .filter(|v| !v.is_empty() && v.len() <= 1024 && !v.as_bytes().contains(&b'\n'));
        value.map(OsStr::to_os_string).ok_or_else(|| {
            unsupported("Set the effective Git author and committer name and email in FastKraken.")
        })
    };
    Ok(Identity([
        component("author.name", "user.name")?,
        component("author.email", "user.email")?,
        component("committer.name", "user.name")?,
        component("committer.email", "user.email")?,
    ]))
}

fn oid(bytes: &[u8]) -> Result<String> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let id = std::str::from_utf8(bytes).map_err(|_| unavailable())?;
    crate::repository::validate_oid(id).map_err(|_| unavailable())?;
    Ok(id.to_owned())
}

fn branch_tip(local: &Local) -> Result<(OsString, Option<String>)> {
    let mut cmd = local.command();
    let named = run(
        cmd.args(["symbolic-ref", "--quiet", "--no-recurse", "HEAD"]),
        None,
        8192,
        local.deadline,
    )?;
    if named.status.code() == Some(1) {
        return Err(unsupported(
            "HEAD is detached. Check out a local branch before reviewing.",
        ));
    }
    let name = checked(named)?;
    let name = OsString::from_vec(name.strip_suffix(b"\n").ok_or_else(unavailable)?.to_owned());
    if !name.as_bytes().starts_with(b"refs/heads/") {
        return Err(unsupported(
            "Check out an ordinary local branch before reviewing.",
        ));
    }
    let alias = run(
        local
            .command()
            .args(["symbolic-ref", "--quiet", "--no-recurse"])
            .arg(&name),
        None,
        8192,
        local.deadline,
    )?;
    if alias.status.code() != Some(1) {
        return Err(unsupported(
            "Symbolic branch aliases cannot be committed through the agent.",
        ));
    }
    let head = run(
        local
            .command()
            .args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
        None,
        128,
        local.deadline,
    )?;
    let head = if head.status.success() {
        Some(oid(&checked(head)?)?)
    } else {
        let exists = run(
            local.command().args(["show-ref", "--exists"]).arg(&name),
            None,
            128,
            local.deadline,
        )?;
        if exists.status.code() != Some(2) {
            return Err(unavailable());
        }
        None
    };
    Ok((name, head))
}

fn no_operation(repo: &Repository, unmerged: bool) -> Result<()> {
    if crate::operation::detect(repo.git_dir(), || Ok(unmerged))
        .map_err(|_| unavailable())?
        .is_some()
    {
        return Err(unsupported(
            "Resolve conflicts and finish the active Git operation before a staged commit.",
        ));
    }
    Ok(())
}
fn staged_entries(local: &Local) -> Result<[u8; 32]> {
    let entries = local.read(&["ls-files", "--stage", "-z"], 16 * 1024 * 1024)?;
    let unmerged = entries
        .split(|&b| b == 0)
        .filter(|v| !v.is_empty())
        .any(|entry| {
            let header = entry.split(|&b| b == b'\t').next().unwrap_or(&[]);
            !header.ends_with(b" 0")
        });
    no_operation(local.repo, unmerged)?;
    Ok(Sha256::digest(entries).into())
}
fn short_ref(name: &OsStr) -> String {
    label(OsStr::from_bytes(
        name.as_bytes()
            .strip_prefix(b"refs/heads/")
            .unwrap_or(name.as_bytes()),
    ))
}

pub(crate) fn review(repo: &Repository) -> Result<ReviewCapture> {
    let local = Local::new(repo, true)?;
    let (branch, expected_head) = branch_tip(&local)?;
    let entries = staged_entries(&local)?;
    let identity = policy(repo, &local.config, local.deadline)?;
    // Git writes cache-tree and tree objects only in the private directory.
    let tree = oid(&local.read(&["write-tree"], 128)?)?;
    let base = if let Some(tip) = &expected_head {
        oid(&local.read(&["rev-parse", "--verify", &format!("{tip}^{{tree}}")], 128)?)?
    } else {
        oid(&local.read(&["mktree"], 128)?)?
    };
    if base == tree {
        return Err(GitError {
            code: "nothing_staged",
            message: "Stage changes in FastKraken, then start a new review.",
        });
    }
    let raw = local.read(
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "--find-renames",
            "--ignore-submodules=none",
            "--submodule=short",
            "-z",
            &base,
            &tree,
            "--",
        ],
        256 * 1024,
    )?;
    let files = parse_changed_files(&raw).map_err(|_| unavailable())?;
    if files.len() > 1024 {
        return Err(unsupported(
            "The complete staged file list exceeds the local review limit.",
        ));
    }
    let files: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({"path":label(f.path.as_os_str()),"status":f.status,
        "from":f.from.as_ref().map(|p| label(p.as_os_str()))})
        })
        .collect();
    let mut diff = local.command();
    diff.arg("diff")
        .args(patch_options(true, DiffOptions::default()))
        .args([
            "--ignore-submodules=none",
            "--submodule=short",
            &base,
            &tree,
            "--",
        ]);
    let bytes = checked(run(&mut diff, None, 2 * 1024 * 1024, local.deadline)?)?;
    if !local.same_index_and_checkout()? || branch_tip(&local)?.1 != expected_head {
        return Err(changed());
    }
    let data = json!({"branch":short_ref(&branch),"expected_head":expected_head,"tree":tree,"files":files,"scope":"entire_index"});
    let patch = patch(
        json!({"source":"staged_review","side":"staged","base_tree":base,"tree":tree}),
        bytes,
        false,
    );
    Ok(ReviewCapture {
        review: Reviewed {
            branch,
            expected_head,
            tree,
            entries,
            identity,
        },
        data,
        patch,
    })
}

pub(super) fn busy() -> GitError {
    GitError {
        code: "busy",
        message: "Another Git writer holds the index. Retry when it finishes.",
    }
}
/// Git canonicalizes even a cwd obtained with fchdir. Running update-ref on the
/// live paths can therefore unlink a *replacement* repository's locks. Hold its
/// actual files-backend locks via directory descriptors and run Git's conditional
/// transaction on a private image of exactly the locked HEAD/branch/packed refs.
struct Transaction {
    _head: Lock,
    _packed: Lock,
    branch_lock: Lock,
    branch_name: OsString,
    log_name: PathBuf,
    head_log: Option<Vec<u8>>,
    branch_log: Option<Vec<u8>>,
}

impl Transaction {
    fn prepare(
        local: &Local,
        pins: &Pins,
        identity: &Identity,
        reviewed: &Reviewed,
        new: &str,
    ) -> Result<Self> {
        let full = Path::new(&reviewed.branch);
        let name = full.file_name().ok_or_else(unavailable)?;
        let parent = full.parent().ok_or_else(unavailable)?;
        let _head = pins.git.lock(OsStr::new("HEAD"))?;
        let branch_dir = pins.common.subdirs(parent, true)?;
        let mut branch_lock = branch_dir.lock(name)?;
        let _packed = pins.common.lock(OsStr::new("packed-refs"))?;
        if !local.pinned_index_and_checkout(pins)? {
            return Err(changed());
        }
        let head = pins
            .git
            .read(OsStr::new("HEAD"), 8192)?
            .ok_or_else(changed)?;
        let private = local.dir.path();
        fs::write(private.join("HEAD"), head).map_err(|_| unavailable())?;
        fs::remove_file(private.join("refs")).map_err(|_| unavailable())?;
        fs::create_dir_all(private.join(parent)).map_err(|_| unavailable())?;
        if private.join("packed-refs").is_symlink() {
            fs::remove_file(private.join("packed-refs")).map_err(|_| unavailable())?;
        }
        if let Some(packed) = pins
            .common
            .read(OsStr::new("packed-refs"), 16 * 1024 * 1024)?
        {
            fs::write(private.join("packed-refs"), packed).map_err(|_| unavailable())?;
        }
        if let Some(loose) = branch_dir.read(name, 8192)? {
            if loose.starts_with(b"ref:") {
                return Err(changed());
            }
            fs::write(private.join(full), loose).map_err(|_| unavailable())?;
        }
        if branch_tip(local)? != (reviewed.branch.clone(), reviewed.expected_head.clone()) {
            return Err(changed());
        }
        let log_name = Path::new("logs").join(full);
        for (dir, path) in [
            (&pins.common, log_name.as_path()),
            (&pins.git, Path::new("logs/HEAD")),
        ] {
            if dir.exists_path(path)? {
                fs::create_dir_all(private.join(path).parent().unwrap())
                    .map_err(|_| unavailable())?;
                fs::write(private.join(path), []).map_err(|_| unavailable())?;
            }
        }
        let mut cmd = local.command();
        identity.apply(&mut cmd);
        let zero = "0".repeat(new.len());
        let expected = reviewed.expected_head.as_deref().unwrap_or(&zero);
        checked(run(
            cmd.args([
                "update-ref",
                "-m",
                "commit: FastKraken local agent",
                "HEAD",
                new,
                expected,
            ]),
            None,
            128,
            local.deadline,
        )?)?;
        let value = local::read_file(&private.join(full), 128)?.ok_or_else(unavailable)?;
        if oid(&value)? != new {
            return Err(changed());
        }
        branch_lock.fill(&value)?;
        Ok(Self {
            _head,
            _packed,
            branch_lock,
            branch_name: name.to_owned(),
            head_log: local::read_file(&private.join("logs/HEAD"), 8192)?,
            branch_log: local::read_file(&private.join(&log_name), 8192)?,
            log_name,
        })
    }
    fn publish(&mut self, pins: &Pins, local: &Local) -> Result<()> {
        let objects = pins.common.subdirs(Path::new("objects"), false)?;
        for fanout in fs::read_dir(local.dir.path().join("objects")).map_err(|_| unavailable())? {
            let fanout = fanout.map_err(|_| unavailable())?;
            let prefix = fanout.file_name();
            if matches!(prefix.to_str(), Some("info" | "pack")) {
                continue;
            }
            if prefix.len() != 2 || !prefix.as_bytes().iter().all(u8::is_ascii_hexdigit) {
                return Err(unavailable());
            }
            let dest = objects.subdirs(Path::new(&prefix), true)?;
            for object in fs::read_dir(fanout.path()).map_err(|_| unavailable())? {
                if Instant::now() >= local.deadline {
                    return Err(busy());
                }
                let object = object.map_err(|_| unavailable())?;
                let suffix = object.file_name();
                if !matches!(suffix.len(), 38 | 62)
                    || !suffix.as_bytes().iter().all(u8::is_ascii_hexdigit)
                {
                    return Err(unavailable());
                }
                dest.install_object(&suffix, &object.path())?;
            }
        }
        // Like Git's files backend, write reflogs before installing the loose
        // ref. The single rename below is the branch publication point.
        if let Some(log) = &self.branch_log {
            let parent = pins.common.subdirs(self.log_name.parent().unwrap(), true)?;
            parent.append_log(&self.branch_name, log)?;
        }
        if let Some(log) = &self.head_log {
            pins.git
                .subdirs(Path::new("logs"), true)?
                .append_log(OsStr::new("HEAD"), log)?;
        }
        self.branch_lock.publish(&self.branch_name)
    }
}

pub(crate) fn commit<G>(
    repo: &Repository,
    reviewed: &Reviewed,
    message: &str,
    authorize: impl FnOnce() -> Result<G>,
) -> Result<Committed> {
    execute(repo, reviewed, message, || (), || (), authorize)
}

fn execute<G>(
    repo: &Repository,
    reviewed: &Reviewed,
    message: &str,
    before_lock: impl FnOnce(),
    before_prepare: impl FnOnce(),
    authorize: impl FnOnce() -> Result<G>,
) -> Result<Committed> {
    if message.trim().is_empty() || message.len() > 16 * 1024 || message.contains('\0') {
        return Err(GitError {
            code: "invalid_params",
            message: "Supply a nonempty commit message of at most 16 KiB.",
        });
    }
    let pins = match &repo.agent_pins {
        Some(pins) => pins.clone(),
        None => {
            Pins::open([&repo.root, repo.git_dir(), repo.common_dir()]).map_err(|_| changed())?
        }
    };
    if !repo.agent_matches_identity(pins.identity().map_err(|_| changed())?) {
        return Err(changed());
    }
    before_lock();
    let _lock = pins.git.lock(OsStr::new("index"))?;
    let local = Local::capture(repo, true, Some(&pins))?;
    if branch_tip(&local).map_err(|_| changed())?
        != (reviewed.branch.clone(), reviewed.expected_head.clone())
        || staged_entries(&local)? != reviewed.entries
    {
        return Err(changed());
    }
    let identity = policy(repo, &local.config, local.deadline)?;
    if identity != reviewed.identity {
        return Err(changed());
    }
    let object_command = || {
        let mut cmd = local.command();

        identity.apply(&mut cmd);
        cmd
    };
    let tree = oid(&checked(run(
        object_command().arg("write-tree"),
        None,
        128,
        local.deadline,
    )?)?)?;
    if tree != reviewed.tree {
        return Err(changed());
    }
    let mut cmd = object_command();
    cmd.args(["commit-tree", &tree]);
    if let Some(parent) = &reviewed.expected_head {
        cmd.args(["-p", parent]);
    }
    let mut body = message.as_bytes().to_owned();
    if !body.ends_with(b"\n") {
        body.push(b'\n');
    }
    let commit = oid(&checked(run(&mut cmd, Some(body), 128, local.deadline)?)?)?;
    before_prepare();
    let mut tx = Transaction::prepare(&local, &pins, &identity, reviewed, &commit)?;
    if !local.pinned_index_and_checkout(&pins)?
        || policy(repo, &local::config(repo, local.deadline)?, local.deadline)? != identity
    {
        return Err(changed());
    }
    no_operation(repo, false)?;
    let _authorization = authorize()?;
    let bound =
        Pins::open([&repo.root, repo.git_dir(), repo.common_dir()]).map_err(|_| changed())?;
    if pins.identity().map_err(|_| changed())? != bound.identity().map_err(|_| changed())? {
        return Err(changed());
    }
    tx.publish(&pins, &local)?;
    Ok(Committed {
        branch: short_ref(&reviewed.branch),
        parent: reviewed.expected_head.clone(),
        commit,
        tree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{io::Write, os::unix::fs::PermissionsExt, process::Stdio};
    fn fixture() -> Fixture {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "base"]);
        fs::write(f.dir.join("file"), "staged\n").unwrap();
        f.git(&["add", "file"]);
        f
    }
    fn git(f: &Fixture, args: &[&str]) -> String {
        String::from_utf8(f.git(args))
            .unwrap()
            .trim_end()
            .to_owned()
    }
    #[test]
    fn exact_reviewed_tree_covers_two_files_rename_deletion_and_modes() {
        let f = Fixture::new();
        for name in ["delete", "rename", "executable", "dual"] {
            fs::write(f.dir.join(name), format!("{name} base\n")).unwrap();
        }
        f.git(&["add", "--all"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["mv", "rename", "renamed"]);
        f.git(&["rm", "delete"]);
        fs::set_permissions(f.dir.join("executable"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(f.dir.join("dual"), "only staged\n").unwrap();
        f.git(&["add", "dual", "executable"]);
        fs::write(f.dir.join("dual"), "unstaged only\n").unwrap();
        let raw = fs::read(f.repo.git_dir().join("index")).unwrap();
        let objects = f.git(&["count-objects", "-v"]);
        let before = git(&f, &["rev-parse", "HEAD"]);
        let capture = review(&f.repo).unwrap();
        assert_eq!(raw, fs::read(f.repo.git_dir().join("index")).unwrap());
        assert_eq!(objects, f.git(&["count-objects", "-v"]));
        assert!(capture.patch.text.contains("+only staged"));
        assert!(!capture.patch.text.contains("unstaged only"));
        assert_eq!(capture.data["files"].as_array().unwrap().len(), 4);
        let out = commit(&f.repo, &capture.review, "Commit message", || Ok(())).unwrap();
        assert_eq!(out.parent.as_deref(), Some(before.as_str()));
        assert_eq!(out.parent, Some(git(&f, &["rev-parse", "HEAD^"])));
        assert_eq!(out.commit, git(&f, &["rev-parse", "HEAD"]));
        assert_eq!(out.tree, git(&f, &["rev-parse", "HEAD^{tree}"]));
        assert_eq!(out.tree, capture.data["tree"]);
        assert_eq!(git(&f, &["show", "HEAD:dual"]), "only staged");
        assert_eq!(fs::read(f.dir.join("dual")).unwrap(), b"unstaged only\n");
        assert!(git(&f, &["ls-tree", "HEAD", "executable"]).starts_with("100755"));
        assert_eq!(raw, fs::read(f.repo.git_dir().join("index")).unwrap());
        assert_eq!(
            git(&f, &["show", "-s", "--format=%an <%ae>/%cn <%ce>", "HEAD"]),
            "Cedar <cedar@example.invalid>/Cedar <cedar@example.invalid>"
        );
    }
    #[test]
    fn unborn_branch_declines_empty_then_commits_without_parent() {
        let f = Fixture::new();
        assert_eq!(review(&f.repo).err().unwrap().code, "nothing_staged");
        fs::write(f.dir.join("first"), "first\n").unwrap();
        f.git(&["add", "first"]);
        let cap = review(&f.repo).unwrap();
        let out = commit(&f.repo, &cap.review, "First commit", || Ok(())).unwrap();
        assert_eq!(out.parent, None);
        assert_eq!(out.tree, git(&f, &["rev-parse", "HEAD^{tree}"]));
        assert!(git(&f, &["show", "-s", "--format=%P", "HEAD"]).is_empty());
    }
    #[test]
    fn concurrent_branch_switch_at_identical_tip_aborts_prepared_transaction() {
        let f = fixture();
        f.git(&["branch", "other"]);
        let capture = review(&f.repo).unwrap();
        let refs = f.git(&["show-ref"]);
        let index = fs::read(f.repo.git_dir().join("index")).unwrap();
        let e = execute(
            &f.repo,
            &capture.review,
            "Stale",
            || (),
            || {
                // An independent process changes the symbolic checkout after the
                // index is locked, without changing either branch tip.
                f.git(&["symbolic-ref", "HEAD", "refs/heads/other"]);
            },
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(e.code, "state_changed");
        assert_eq!(f.git(&["show-ref"]), refs);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/other\n");
        assert_eq!(index, fs::read(f.repo.git_dir().join("index")).unwrap());
        assert!(!f.repo.git_dir().join("HEAD.lock").exists());
        assert!(!f.repo.git_dir().join("refs/heads/other.lock").exists());
    }
    #[test]
    fn prepared_transaction_locks_the_granted_linked_checkout_and_index() {
        let f = fixture();
        let path = tempfile::tempdir().unwrap();
        f.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            path.path().to_str().unwrap(),
            "HEAD",
        ]);
        let linked = f.open(path.path());
        let main = git(&f, &["rev-parse", "main"]);
        fs::write(path.path().join("linked-file"), "linked staged\n").unwrap();
        assert!(
            linked
                .command()
                .args(["add", "linked-file"])
                .output()
                .unwrap()
                .status
                .success()
        );
        let capture = review(&linked).unwrap();
        let result = commit(&linked, &capture.review, "Linked work", || {
            assert!(linked.git_dir().join("HEAD.lock").exists());
            assert!(linked.git_dir().join("index.lock").exists());
            assert!(linked.common_dir().join("refs/heads/linked.lock").exists());
            assert!(
                !linked
                    .command()
                    .args([
                        "-c",
                        "core.filesRefLockTimeout=10",
                        "symbolic-ref",
                        "HEAD",
                        "refs/heads/main"
                    ])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            assert!(
                !linked
                    .command()
                    .args(["add", "linked-file"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(result.branch, "linked");
        assert_eq!(git(&f, &["rev-parse", "main"]), main);
        assert_eq!(result.commit, git(&f, &["rev-parse", "linked"]));
    }

    #[test]
    fn replacement_after_preparation_keeps_all_sentinels_and_cleans_only_original_locks() {
        let f = fixture();
        let cap = review(&f.repo).unwrap();
        let old_objects = f.git(&["count-objects", "-v"]);
        let old_refs = f.git(&["show-ref"]);
        let gd = f.repo.git_dir();
        let displaced = gd.with_file_name("displaced.git");
        let result = commit(&f.repo, &cap.review, "Refused", || {
            fs::rename(gd, &displaced).unwrap();
            for path in ["refs/heads", "objects", "logs/refs/heads"] {
                fs::create_dir_all(gd.join(path)).unwrap();
            }
            for path in [
                "HEAD",
                "HEAD.lock",
                "index",
                "index.lock",
                "packed-refs.lock",
                "refs/heads/main",
                "refs/heads/main.lock",
                "logs/HEAD",
                "logs/refs/heads/main",
            ] {
                fs::write(gd.join(path), b"replacement sentinel").unwrap();
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err().code, "state_changed");
        for path in [
            "HEAD",
            "HEAD.lock",
            "index",
            "index.lock",
            "packed-refs.lock",
            "refs/heads/main",
            "refs/heads/main.lock",
            "logs/HEAD",
            "logs/refs/heads/main",
        ] {
            assert_eq!(fs::read(gd.join(path)).unwrap(), b"replacement sentinel");
        }
        assert_eq!(fs::read_dir(gd.join("objects")).unwrap().count(), 0);
        for name in [
            "index.lock",
            "HEAD.lock",
            "refs/heads/main.lock",
            "packed-refs.lock",
        ] {
            assert!(!displaced.join(name).exists());
        }
        fs::rename(gd, gd.with_file_name("replacement.git")).unwrap();
        fs::rename(displaced, gd).unwrap();
        assert_eq!(f.git(&["count-objects", "-v"]), old_objects);
        assert_eq!(f.git(&["show-ref"]), old_refs);
    }

    #[test]
    fn packed_branch_publication_preserves_native_head_and_branch_reflogs() {
        let f = fixture();
        f.git(&["pack-refs", "--all", "--prune"]);
        assert!(!f.repo.common_dir().join("refs/heads/main").exists());
        let previous = git(&f, &["rev-parse", "HEAD"]);
        let cap = review(&f.repo).unwrap();
        let out = commit(&f.repo, &cap.review, "Native logs", || Ok(())).unwrap();
        for path in ["logs/HEAD", "logs/refs/heads/main"] {
            let log = fs::read_to_string(f.repo.common_dir().join(path)).unwrap();
            let last = log.lines().last().unwrap();
            assert!(last.starts_with(&format!(
                "{previous} {} Cedar <cedar@example.invalid> ",
                out.commit
            )));
        }
        assert_eq!(git(&f, &["rev-parse", "HEAD"]), out.commit);
    }

    #[test]
    fn independent_staging_and_queued_revocation_are_refused() {
        let f = fixture();
        let reviewed = review(&f.repo).unwrap().review;
        let refs = f.git(&["show-ref"]);
        let e = execute(
            &f.repo,
            &reviewed,
            "Stale",
            || {
                fs::write(f.dir.join("file"), "restaged\n").unwrap();
                f.git(&["add", "file"]);
            },
            || (),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(e.code, "state_changed");
        assert_eq!(git(&f, &["show", ":file"]), "restaged");
        let reviewed = review(&f.repo).unwrap().review;
        let e = commit(&f.repo, &reviewed, "Revoked", || {
            Err::<(), _>(GitError {
                code: "access_denied",
                message: "Revoked.",
            })
        })
        .unwrap_err();
        assert_eq!(e.code, "access_denied");
        assert_eq!(f.git(&["show-ref"]), refs);
        assert!(!f.repo.git_dir().join("index.lock").exists());
    }
    #[test]
    fn hook_policy_is_enforced_at_review_prepare_and_publication_without_execution() {
        let f = fixture();
        let cap = review(&f.repo).unwrap();
        f.git(&["config", "--unset", "core.hooksPath"]);
        let hooks = f.repo.common_dir().join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let marker = f.dir.join("recorded");
        let hook = hooks.join("reference-transaction");
        let install = || {
            fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        };
        let refs = f.git(&["show-ref"]);
        let e = execute(&f.repo, &cap.review, "Refused", || (), install, || Ok(())).unwrap_err();
        assert_eq!(e.code, "unsupported_configuration");
        assert!(!marker.exists());
        assert_eq!(f.git(&["show-ref"]), refs);
        assert_eq!(
            review(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
        fs::remove_file(hook).unwrap();
        f.git(&["config", "commit.gpgsign", "true"]);
        let signer = f.dir.join("recording-signer");
        fs::write(
            &signer,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&signer, fs::Permissions::from_mode(0o755)).unwrap();
        f.git(&["config", "gpg.program", &signer.to_string_lossy()]);
        assert_eq!(
            review(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
        assert_eq!(
            commit(&f.repo, &cap.review, "Refused", || Ok(()))
                .unwrap_err()
                .code,
            "unsupported_configuration"
        );
        assert!(!marker.exists());
    }
    #[test]
    fn unmerged_index_entries_cannot_be_reviewed_without_operation_markers() {
        let f = fixture();
        let blob = git(&f, &["rev-parse", ":file"]);
        let mut cmd = f.repo.command();
        let mut child = cmd
            .args(["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "0 {}\tfile", "0".repeat(40)).unwrap();
        for stage in [1, 2, 3] {
            writeln!(stdin, "100644 {blob} {stage}\tfile").unwrap();
        }
        drop(stdin);
        assert!(child.wait().unwrap().success());
        assert_eq!(
            review(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
    }

    #[test]
    fn active_git_operations_detached_and_busy_have_distinct_refusals() {
        let f = fixture();
        let cap = review(&f.repo).unwrap();
        let lock = f.repo.git_dir().join("index.lock");
        fs::write(&lock, "another writer").unwrap();
        assert_eq!(
            commit(&f.repo, &cap.review, "Refused", || Ok(()))
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(fs::read(&lock).unwrap(), b"another writer");
        fs::remove_file(lock).unwrap();
        let marker = f.repo.git_dir().join("MERGE_HEAD");
        fs::write(&marker, f.git(&["rev-parse", "HEAD"])).unwrap();
        assert_eq!(
            review(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
        fs::remove_file(marker).unwrap();
        f.git(&["switch", "--detach", "-q"]);
        assert_eq!(
            review(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
    }
}
