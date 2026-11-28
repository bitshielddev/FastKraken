//! Git reads its local config even with system/global config disabled. Run
//! repository reads in a private Git directory with only data-bearing config
//! and an alternate object store. A concurrent config edit cannot install a
//! filter, monitor, hook, transport, diff driver, pager or credential helper.
use super::{GitError, Result, changed, unavailable, unsupported};
use crate::repository::Repository;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{OpenOptionsExt, PermissionsExt, symlink},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const MAX_INDEX: usize = 32 * 1024 * 1024;
pub(super) const TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Limited {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub status: ExitStatus,
}

/// Read stdout to a bounded buffer. Enforce a wall-clock deadline even on a
/// silent child, reap it on every path and never reflect Git stderr to clients.
pub(super) fn run(
    cmd: &mut Command,
    input: Option<Vec<u8>>,
    limit: usize,
    deadline: Instant,
) -> Result<Limited> {
    if crate::agent_transport::read_cancelled() {
        return Err(cancelled());
    }
    if Instant::now() >= deadline {
        return Err(timed_out());
    }
    let mut child = cmd
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unavailable())?;
    let stdout = child.stdout.take().unwrap();
    let full = Arc::new(AtomicBool::new(false));
    let signal = full.clone();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        let truncated = bytes.len() > limit;
        if truncated {
            signal.store(true, Ordering::Release);
            bytes.truncate(limit);
        }
        Ok::<_, std::io::Error>((bytes, truncated))
    });
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        thread::spawn(move || stdin.write_all(&bytes))
    });
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None)
                if !crate::agent_transport::read_cancelled()
                    && !full.load(Ordering::Acquire)
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(3))
            }
            waited => {
                let timeout = Instant::now() >= deadline;
                // The PID is still ours until wait. Kill its process group too.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let status = child.wait().map_err(|_| unavailable());
                break if crate::agent_transport::read_cancelled() {
                    Err(cancelled())
                } else if timeout {
                    Err(timed_out())
                } else if waited.is_err() {
                    Err(unavailable())
                } else {
                    status
                };
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    if let Some(writer) = writer {
        writer
            .join()
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
    }
    Ok(Limited {
        bytes: bytes.0,
        truncated: bytes.1,
        status: result?,
    })
}

fn cancelled() -> GitError {
    GitError {
        code: "cancelled",
        message: "The local agent read was cancelled.",
    }
}

fn timed_out() -> GitError {
    unsupported("The bounded local Git read timed out. Retry after the repository is idle.")
}

pub(super) fn checked(out: Limited) -> Result<Vec<u8>> {
    if out.truncated {
        return Err(unsupported(
            "The repository result exceeds the local agent limit.",
        ));
    }
    if !out.status.success() {
        return Err(unavailable());
    }
    Ok(out.bytes)
}

fn base(dir: &Path) -> Result<Command> {
    let mut cmd = Command::new(Repository::agent_program().map_err(|_| unavailable())?);
    cmd.current_dir(dir)
        .env_clear()
        .env("LC_ALL", "C")
        .env("PATH", "/dev/null")
        .env("GIT_EXEC_PATH", "/dev/null")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .args([
            "--no-pager",
            "--no-lazy-fetch",
            "--no-replace-objects",
            "--no-optional-locks",
            "--literal-pathspecs",
        ]);
    Ok(cmd)
}

pub(crate) fn open(root: &Path) -> Result<Repository> {
    let deadline = Instant::now() + TIMEOUT;
    let root = root.canonicalize().map_err(|_| unavailable())?;
    let get = |arg| -> Result<PathBuf> {
        let mut cmd = base(&root)?;
        cmd.args(["rev-parse", "--path-format=absolute", arg]);
        let out = checked(run(&mut cmd, None, 64 * 1024, deadline)?)?;
        let path = OsStr::from_bytes(out.strip_suffix(b"\n").ok_or_else(unavailable)?);
        Path::new(path).canonicalize().map_err(|_| unavailable())
    };
    if get("--show-toplevel")? != root {
        return Err(changed());
    }
    let git = get("--git-dir")?;
    let common = get("--git-common-dir")?;
    Repository::agent_repository(root, git, common).map_err(|_| unavailable())
}

pub(super) type Config = Vec<(OsString, OsString)>;

/// This invocation only reads effective configuration, including conditional
/// includes. Its output stays in memory, and command-valued keys never get
/// copied into the private repository.
pub(super) fn configured_command(repo: &Repository) -> Result<Command> {
    let mut cmd = base(&repo.root)?;
    cmd.env_remove("GIT_CONFIG_NOSYSTEM")
        .env_remove("GIT_CONFIG_GLOBAL");
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_GLOBAL",
    ] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    #[cfg(test)]
    cmd.env("GIT_CONFIG_NOSYSTEM", "1").env(
        "GIT_CONFIG_GLOBAL",
        crate::repository::tests::global_config(),
    );
    cmd.arg("--git-dir")
        .arg(repo.git_dir())
        .arg("--work-tree")
        .arg(&repo.root);
    Ok(cmd)
}

pub(super) fn config(repo: &Repository, deadline: Instant) -> Result<Config> {
    let mut cmd = configured_command(repo)?;
    cmd.args(["config", "--includes", "--null", "--list"]);
    let data = checked(run(&mut cmd, None, 1024 * 1024, deadline)?)?;
    let mut entries = Vec::new();
    for field in data.split(|&b| b == 0).filter(|f| !f.is_empty()) {
        let mut parts = field.splitn(2, |&b| b == b'\n');
        let key = OsString::from_vec(parts.next().unwrap().to_vec());
        let value = OsString::from_vec(parts.next().unwrap_or(b"true").to_vec());
        entries.push((key, value));
    }
    Ok(entries)
}

fn preserved(key: &[u8]) -> bool {
    matches!(
        key,
        b"core.filemode"
            | b"core.logallrefupdates"
            | b"core.autocrlf"
            | b"core.eol"
            | b"core.ignorecase"
            | b"core.symlinks"
            | b"core.precomposeunicode"
            | b"core.excludesfile"
            | b"core.attributesfile"
    ) || key.starts_with(b"branch.") && (key.ends_with(b".remote") || key.ends_with(b".merge"))
        || key.starts_with(b"remote.") && key.ends_with(b".fetch")
}

pub(super) fn read_file(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other.map_err(|_| unavailable())?,
    };
    if !file.metadata().map_err(|_| unavailable())?.is_file() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > limit {
        return Err(unsupported(
            "The index or repository metadata exceeds the agent capture limit.",
        ));
    }
    Ok(Some(bytes))
}

pub(super) struct Local<'a> {
    pub repo: &'a Repository,
    pub dir: TempDir,
    pub config: Config,
    pub deadline: Instant,
    head: Vec<u8>,
    index: Option<Vec<u8>>,
    git: PathBuf,
}

impl<'a> Local<'a> {
    pub fn new(repo: &'a Repository, with_index: bool) -> Result<Self> {
        Self::capture(repo, with_index, None)
    }
    pub fn capture(
        repo: &'a Repository,
        with_index: bool,
        pins: Option<&super::pinned::Pins>,
    ) -> Result<Self> {
        let deadline = Instant::now() + TIMEOUT;
        let config = config(repo, deadline)?;
        let mut object_format = "sha1";
        for (k, v) in &config {
            let key = k.as_bytes();
            let value = v.as_bytes();
            if matches!(key, b"extensions.partialclone")
                || key.starts_with(b"remote.") && key.ends_with(b".promisor") && value != b"false"
            {
                return Err(unsupported(
                    "Partial-clone reads are unavailable. Open a complete local clone.",
                ));
            }
            if with_index
                && key.starts_with(b"filter.")
                && [b".process".as_slice(), b".clean", b".smudge"]
                    .iter()
                    .any(|suffix| key.ends_with(suffix))
                && !value.is_empty()
            {
                return Err(unsupported(
                    "An external Git filter is configured. Inspect working files in FastKraken.",
                ));
            }
            if key == b"extensions.objectformat" {
                object_format = match value {
                    b"sha1" => "sha1",
                    b"sha256" => "sha256",
                    _ => return Err(unavailable()),
                };
            }
            if key.starts_with(b"extensions.")
                && !matches!(
                    key,
                    b"extensions.objectformat" | b"extensions.worktreeconfig"
                )
            {
                return Err(unsupported(
                    "This Git repository extension is unavailable to the agent.",
                ));
            }
            if with_index
                && matches!(key, b"core.sparsecheckout" | b"index.sparse")
                && value != b"false"
            {
                return Err(unsupported("Inspect sparse worktrees in FastKraken."));
            }
        }
        let dir = tempfile::Builder::new()
            .prefix("fastkraken-agent-git-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|_| unavailable())?;
        if dir
            .path()
            .canonicalize()
            .map_err(|_| unavailable())?
            .starts_with(&repo.root)
        {
            return Err(unsupported(
                "Use a temporary directory outside the granted worktree.",
            ));
        }
        let git = Repository::agent_program().map_err(|_| unavailable())?;
        let join = |path| dir.path().join(path);
        fs::create_dir(join("objects")).map_err(|_| unavailable())?;
        fs::create_dir(join("info")).map_err(|_| unavailable())?;
        symlink(repo.common_dir().join("objects"), join("borrowed")).map_err(|_| unavailable())?;
        for path in [
            "refs",
            "packed-refs",
            "info/attributes",
            "info/exclude",
            "shallow",
        ] {
            if repo
                .common_dir()
                .join(path)
                .try_exists()
                .map_err(|_| unavailable())?
            {
                symlink(repo.common_dir().join(path), join(path)).map_err(|_| unavailable())?;
            }
        }
        let snapshot_config = format!(
            "[core]\nrepositoryformatversion = 1\nbare = false\n[extensions]\nobjectformat = {object_format}\n"
        );
        fs::write(join("config"), snapshot_config).map_err(|_| unavailable())?;
        let read_meta = |name: &str, limit| match pins {
            Some(pins) => pins.git.read(OsStr::new(name), limit),
            None => read_file(&repo.git_dir().join(name), limit),
        };
        let head = read_meta("HEAD", 8192)?.ok_or_else(unavailable)?;
        fs::write(join("HEAD"), &head).map_err(|_| unavailable())?;
        let index = if with_index {
            read_meta("index", MAX_INDEX)?
        } else {
            None
        };
        if let Some(bytes) = &index {
            fs::write(join("index"), bytes).map_err(|_| unavailable())?;
        }
        Ok(Self {
            repo,
            dir,
            git,
            deadline,
            config,
            head,
            index,
        })
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.git);
        cmd.current_dir(&self.repo.root)
            .env_clear()
            .env("LC_ALL", "C")
            .env("PATH", "/dev/null")
            .env("GIT_EXEC_PATH", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                self.dir.path().join("borrowed"),
            )
            .args([
                "--no-pager",
                "--no-lazy-fetch",
                "--no-replace-objects",
                "--no-optional-locks",
                "--literal-pathspecs",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "maintenance.auto=false",
            ])
            .arg("--git-dir")
            .arg(self.dir.path())
            .arg("--work-tree")
            .arg(&self.repo.root);
        let mut count = 0;
        for (key, value) in &self.config {
            if preserved(key.as_bytes()) {
                cmd.env(format!("GIT_CONFIG_KEY_{count}"), key)
                    .env(format!("GIT_CONFIG_VALUE_{count}"), value);
                count += 1;
            }
        }
        cmd.env("GIT_CONFIG_COUNT", count.to_string());
        cmd
    }
    pub fn read(&self, args: &[&str], limit: usize) -> Result<Vec<u8>> {
        checked(run(self.command().args(args), None, limit, self.deadline)?)
    }
    pub fn pinned_index_and_checkout(&self, pins: &super::pinned::Pins) -> Result<bool> {
        Ok(
            pins.git.read(OsStr::new("HEAD"), 8192)?.as_ref() == Some(&self.head)
                && pins.git.read(OsStr::new("index"), MAX_INDEX)? == self.index,
        )
    }

    pub fn same_index_and_checkout(&self) -> Result<bool> {
        Ok(
            read_file(&self.repo.git_dir().join("HEAD"), 8192)?.as_ref() == Some(&self.head)
                && read_file(&self.repo.git_dir().join("index"), MAX_INDEX)? == self.index,
        )
    }
}

/// Capture a regular untracked file without following any component through a
/// symlink. The no-index diff runs on this private copy, so concurrent renames
/// cannot redirect Git into another tree (or into a FIFO).
pub(super) fn capture_untracked(
    local: &Local,
    relative: &Path,
    limit: usize,
) -> Result<Option<(TempDir, bool)>> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
        os::unix::fs::PermissionsExt,
    };
    let mut fd = fs::File::open(&local.repo.root).map_err(|_| unavailable())?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(unavailable());
        }
        let name = CString::new(component.as_os_str().as_bytes()).map_err(|_| unavailable())?;
        let directory = if components.peek().is_some() {
            libc::O_DIRECTORY
        } else {
            0
        };
        let flags =
            libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_RDONLY | libc::O_CLOEXEC | directory;
        let next = unsafe { libc::openat(fd.as_raw_fd(), name.as_ptr(), flags) };
        if next < 0 {
            return Ok(None);
        }
        fd = unsafe { fs::File::from_raw_fd(next) };
    }
    let metadata = fd.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let temp = tempfile::tempdir_in(local.dir.path()).map_err(|_| unavailable())?;
    let dest = temp.path().join(relative);
    fs::create_dir_all(dest.parent().ok_or_else(unavailable)?).map_err(|_| unavailable())?;
    let mut out = fs::File::create(&dest).map_err(|_| unavailable())?;
    let bytes =
        std::io::copy(&mut fd.take(limit as u64 + 1), &mut out).map_err(|_| unavailable())?;
    fs::set_permissions(
        &dest,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o700),
    )
    .map_err(|_| unavailable())?;
    Ok(Some((temp, bytes > limit as u64)))
}
