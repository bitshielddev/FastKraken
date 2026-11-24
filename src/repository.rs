use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    io::{Read, Write},
    ops::Range,
    os::unix::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
};

type Result<T> = std::result::Result<T, String>;
const MAX_DIFF_BYTES: usize = 1024 * 1024;
const SSH_TIMEOUTS: &str = "ssh -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2";
pub(crate) const PATCH_OPTIONS: &[&str] = &[
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
    "--full-index",
    "--unified=3",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

#[derive(Clone, Debug)]
pub struct Repository {
    pub root: PathBuf,
    engine: Engine,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct Engine {
    executable: PathBuf,
    exec_dir: PathBuf,
    templates: PathBuf,
    bin: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub time: i64,
    pub refs: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Revisions {
    tips: Vec<OsString>,
    hidden: Vec<OsString>,
    only_on: Option<OsString>,
    only_on_remote: Option<OsString>,
}

impl Revisions {
    pub(crate) fn reachable(tip: impl Into<OsString>) -> Self {
        Self {
            tips: vec![tip.into()],
            hidden: Vec::new(),
            only_on: None,
            only_on_remote: None,
        }
    }

    pub(crate) fn and(mut self, tip: impl Into<OsString>) -> Self {
        self.tips.push(tip.into());
        self
    }

    pub(crate) fn not(mut self, other: impl Into<OsString>) -> Self {
        self.hidden.push(other.into());
        self
    }

    pub(crate) fn only_on(mut self, branch: impl Into<OsString>) -> Self {
        self.only_on = Some(branch.into());
        self
    }

    pub(crate) fn only_on_remote(mut self, remote: impl Into<OsString>) -> Self {
        self.only_on_remote = Some(remote.into());
        self
    }

    fn args(&self) -> Result<Vec<OsString>> {
        let revision = |name: &OsString| {
            let bytes = name.as_encoded_bytes();
            if bytes.is_empty() || bytes[0] == b'-' || bytes.iter().any(u8::is_ascii_whitespace) {
                Err("Select a branch, tag or commit by its full name.".to_owned())
            } else {
                Ok(name.clone())
            }
        };
        let mut args = self.tips.iter().map(revision).collect::<Result<Vec<_>>>()?;
        if self.hidden.is_empty() && self.only_on.is_none() && self.only_on_remote.is_none() {
            return Ok(args);
        }
        args.push("--not".into());
        for name in &self.hidden {
            args.push(revision(name)?);
        }
        if self.only_on.is_none() && self.only_on_remote.is_none() {
            return Ok(args);
        }
        let exclude = |name: &OsString| {
            let bytes = name.as_encoded_bytes();
            if bytes.is_empty()
                || bytes[0] == b'-'
                || bytes.iter().any(|b| b"*?[\\".contains(b) || b.is_ascii_whitespace())
            {
                return Err("Select a branch by its name.".to_owned());
            }
            let mut exclude = OsString::from("--exclude=");
            exclude.push(name);
            Ok(exclude)
        };
        if let Some(branch) = &self.only_on {
            args.push(exclude(branch)?);
        }
        args.push("--branches".into());
        if let Some(remote) = &self.only_on_remote {
            args.push(exclude(remote)?);
        }
        args.extend(["--remotes".into(), "--tags".into()]);
        Ok(args)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Status {
    pub branch: String,
    pub upstream: Option<String>,
    pub detached: bool,
    pub head: Option<String>,
    pub head_ref: Option<OsString>,
    pub staged: Vec<ChangedFile>,
    pub unstaged: Vec<ChangedFile>,
    pub changed_paths: usize,
    pub operation: Option<crate::operation::Kind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub status: &'static str,
}

fn status_label(code: u8) -> &'static str {
    match code {
        b'A' => "Added",
        b'D' => "Deleted",
        b'T' => "Type changed",
        b'U' => "Conflicted",
        b'?' => "Untracked",
        _ => "Modified",
    }
}

#[derive(Clone, Debug)]
pub struct Diff {
    pub text: String,
    pub hunks: Vec<Hunk>,
    source: Option<(PathBuf, PathBuf)>,
    staged: bool,
    bytes: Vec<u8>,
    prefix: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub start_line: usize,
    bytes: Range<usize>,
}

impl Engine {
    fn bundled() -> Result<Self> {
        let binary = env::current_exe().and_then(std::fs::canonicalize).map_err(|e| e.to_string())?;
        let macos = binary
            .parent()
            .ok_or("Cannot locate app executable directory")?;
        let runtime = match macos.parent() {
            Some(contents) if macos.ends_with("MacOS") && contents.ends_with("Contents") => {
                contents.join("Helpers/git")
            }
            _ => {
                let path = env::var_os("FASTKRAKEN_GIT_ROOT").ok_or("Set FASTKRAKEN_GIT_ROOT to the private Git runtime when running outside the app bundle.")?;
                PathBuf::from(path)
                    .canonicalize()
                    .map_err(|e| format!("Open private Git runtime: {e}"))?
            }
        };
        Self::at(&runtime.join("bin/git"))
    }

    fn at(executable: &Path) -> Result<Self> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err(format!(
                "Bundled Git is missing at {}. Reinstall the app or rebuild its Git runtime.",
                executable.display()
            ));
        }
        let bin = executable.parent().ok_or("Invalid bundled Git path")?;
        let root = bin.parent().ok_or("Invalid bundled Git directory")?;
        env::join_paths([bin]).map_err(|e| e.to_string())?;
        Ok(Self {
            executable: executable.to_owned(),
            exec_dir: root.join("libexec/git-core"),
            templates: root.join("share/git-core/templates"),
            bin: bin.to_owned(),
        })
    }

    fn search_path(&self) -> OsString {
        let dirs = std::iter::once(self.bin.as_path()).chain(crate::environment::host_dirs().iter().map(PathBuf::as_path));
        env::join_paths(dirs).unwrap_or_else(|_| self.bin.clone().into_os_string())
    }

    fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.executable);
        cmd.current_dir(dir)
            .env("PATH", self.search_path())
            .env("GIT_EXEC_PATH", &self.exec_dir)
            .env("GIT_TEMPLATE_DIR", &self.templates)
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("--no-pager");
        #[cfg(test)]
        {
            cmd.env("GIT_CONFIG_GLOBAL", tests::global_config())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_SSH_COMMAND")
                .env_remove("GIT_SSH")
                .env_remove("GIT_ASKPASS")
                .env_remove("SSH_ASKPASS");
        }
        for variable in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_PREFIX",
        ] {
            cmd.env_remove(variable);
        }
        cmd
    }
}

pub(crate) fn bundled_command(directory: &Path) -> Result<Command> {
    Ok(Engine::bundled()?.command(directory))
}

pub(crate) fn clone_command(directory: &Path) -> Result<Command> {
    let engine = Engine::bundled()?;
    let mut command = engine.command(directory);
    let mut read = engine.command(Path::new("/"));
    read.arg("--no-optional-locks");
    timeouts(&mut command, read);
    answers(&mut command);
    Ok(command)
}

fn timeouts(command: &mut Command, read: Command) {
    command.args(["-c", "http.lowSpeedLimit=1000", "-c", "http.lowSpeedTime=30"]);
    if own_ssh_command(read).is_none() {
        command.env("GIT_SSH_COMMAND", SSH_TIMEOUTS);
    }
}

fn answers(command: &mut Command) {
    crate::credentials::configure(command);
    let link = crate::jobs::Link::current();
    crate::askpass::configure(command, link.as_ref().and_then(|link| link.session()).map(|session| &**session));
}

fn own_ssh_command(mut read: Command) -> Option<String> {
    for variable in ["GIT_SSH_COMMAND", "GIT_SSH"] {
        if let Some(value) = env::var_os(variable).filter(|_| !cfg!(test)) {
            return Some(value.to_string_lossy().into_owned());
        }
    }
    let output = read.args(["config", "--get", "core.sshCommand"]).run();
    match output {
        Ok(output) if output.status.code() == Some(1) => None,
        Ok(output) => Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()),
        Err(_) => Some(String::new()),
    }
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_engine(path, Engine::bundled()?)
    }

    #[cfg(test)]
    fn with_git(path: &Path, git: &Path) -> Result<Self> {
        Self::open_with_engine(path, Engine::at(git)?)
    }

    fn open_with_engine(path: &Path, engine: Engine) -> Result<Self> {
        let directory = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let query = |flags: &[&str]| -> Result<Vec<u8>> {
            checked(
                engine
                    .command(directory)
                    .args(["--no-optional-locks", "rev-parse", "--path-format=absolute"])
                    .args(flags),
            )
        };
        let flags = ["--show-toplevel", "--git-dir", "--git-common-dir"];
        let combined = query(&flags)?;
        let lines: Vec<_> = without_final_newline(&combined)
            .split(|&b| b == b'\n')
            .collect();
        let paths = if lines.len() == 3 {
            lines.into_iter().map(raw_path).collect::<Vec<_>>()
        } else {
            flags
                .iter()
                .map(|flag| query(&[flag]).map(|bytes| raw_path(without_final_newline(&bytes))))
                .collect::<Result<_>>()?
        };
        Ok(Self {
            root: paths[0].clone(),
            engine,
            git_dir: paths[1].clone(),
            common_dir: paths[2].clone(),
        })
    }

    pub fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.root.clone(),
            self.git_dir.clone(),
            self.common_dir.clone(),
        ];
        paths.sort();
        paths.dedup();
        paths
    }

    pub(crate) fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub(crate) fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub(crate) fn ensure_index_writable(&self) -> Result<()> {
        self.expect_no_operation()
    }

    pub(crate) fn command(&self) -> Command {
        self.engine.command(&self.root)
    }

    pub(crate) fn network_command(&self) -> Result<Command> {
        let mut command = self.command();
        timeouts(&mut command, self.read_command());
        crate::auth::configure_transport(self, &mut command)?;
        answers(&mut command);
        Ok(command)
    }

    pub(crate) fn read_command(&self) -> Command {
        let mut cmd = self.command();
        cmd.args(["--no-optional-locks", "--literal-pathspecs"]);
        cmd
    }

    pub fn history(&self, offset: usize, limit: usize) -> Result<Vec<Commit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Self::read_log(
            self.log_command()
                .args(["--exclude=refs/stash", "--all"])
                .arg(format!("--skip={offset}"))
                .arg(format!("--max-count={limit}")),
        )
    }

    pub(crate) fn search_history(&self, query: &str, limit: usize) -> Result<Vec<Commit>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut commits = Self::read_log(
            self.log_command()
                .args(["--exclude=refs/stash", "--all", "--regexp-ignore-case", "--fixed-strings"])
                .arg(format!("--grep={query}"))
                .arg(format!("--max-count={limit}")),
        )?;
        if (4..=64).contains(&query.len()) && query.bytes().all(|b| b.is_ascii_hexdigit()) {
            let prefix = query.to_ascii_lowercase();
            let mut exact = Self::read_log(
                self.log_command().arg("--max-count=1").arg(format!("{prefix}^{{commit}}")),
            ).unwrap_or_default();
            if let Some(commit) = exact.pop().filter(|commit| commit.oid.starts_with(&prefix)) {
                commits.retain(|row| row.oid != commit.oid);
                commits.insert(0, commit);
                commits.truncate(limit);
            }
        }
        Ok(commits)
    }

    pub(crate) fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        validate_oid(ancestor)?;
        validate_oid(descendant)?;
        let output = self
            .read_command()
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        }
    }

    pub(crate) fn count(&self, revisions: &Revisions) -> Result<usize> {
        let count = checked(
            self.read_command()
                .args(["rev-list", "--count"])
                .args(revisions.args()?)
                .arg("--"),
        )?;
        String::from_utf8_lossy(&count)
            .trim()
            .parse()
            .map_err(|_| "Git did not report a commit count.".to_owned())
    }

    pub(crate) fn subjects(&self, revisions: &Revisions, limit: usize) -> Result<Vec<Commit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Self::read_log(
            self.log_command()
                .arg(format!("--max-count={limit}"))
                .args(revisions.args()?),
        )
    }

    fn log_command(&self) -> Command {
        let mut cmd = self.read_command();
        cmd.args([
            "log",
            "--topo-order",
            "--decorate=short",
            "--no-color",
            "--no-show-signature",
            "--no-patch",
            "-z",
            "--format=%H%x00%P%x00%an%x00%as%x00%at%x00%D%x00%s",
        ]);
        cmd
    }

    fn read_log(cmd: &mut Command) -> Result<Vec<Commit>> {
        let output = checked(cmd.arg("--"))?;
        if output.is_empty() {
            return Ok(Vec::new());
        }
        let data = output.strip_suffix(&[0]).unwrap_or(&output);
        let mut commits = Vec::new();
        let mut fields = data.split(|&b| b == 0);
        while let Some(id) = fields.next() {
            let mut field = || {
                fields
                    .next()
                    .map(|v| String::from_utf8_lossy(v).into_owned())
                    .ok_or("Invalid history record")
            };
            commits.push(Commit {
                oid: String::from_utf8_lossy(id).into_owned(),
                parents: field()?.split_whitespace().map(String::from).collect(),
                author: field()?,
                date: field()?,
                time: field()?.parse().unwrap_or(0),
                refs: field()?,
                subject: field()?,
            });
        }
        Ok(commits)
    }

    pub fn status(&self) -> Result<Status> {
        let output = checked(self.read_command().args([
            "status",
            "--porcelain=v2",
            "--branch",
            "--no-ahead-behind",
            "--untracked-files=all",
            "--no-renames",
            "-z",
        ]))?;
        let mut result = Status::default();
        let mut unmerged = false;
        for entry in output.split(|&b| b == 0).filter(|entry| !entry.is_empty()) {
            if let Some(name) = entry.strip_prefix(b"# branch.head ") {
                result.branch = String::from_utf8_lossy(name).into_owned();
                result.head_ref = if name == b"(detached)" {
                    self.head_reference()?
                } else {
                    Some(OsString::from_vec([b"refs/heads/", name].concat()))
                };
            } else if let Some(name) = entry.strip_prefix(b"# branch.upstream ") {
                result.upstream = Some(String::from_utf8_lossy(name).into_owned());
            } else if let Some(id) = entry.strip_prefix(b"# branch.oid ") {
                if id != b"(initial)" {
                    result.head = Some(String::from_utf8_lossy(id).into_owned());
                }
            } else if let Some(path) = entry.strip_prefix(b"? ") {
                result.changed_paths += 1;
                result.unstaged.push(ChangedFile {
                    path: raw_path(path),
                    status: status_label(b'?'),
                });
            } else if matches!(entry.first(), Some(b'1' | b'u')) {
                unmerged |= entry[0] == b'u';
                let count = if entry[0] == b'u' { 11 } else { 9 };
                let fields: Vec<_> = entry.splitn(count, |&b| b == b' ').collect();
                if fields.len() != count || fields[1].len() != 2 {
                    return Err("Invalid Git status record".into());
                }
                let path = raw_path(fields[count - 1]);
                result.changed_paths += 1;
                let label = |code| {
                    if entry[0] == b'u' {
                        "Conflicted"
                    } else {
                        status_label(code)
                    }
                };
                if fields[1][0] != b'.' {
                    result.staged.push(ChangedFile {
                        path: path.clone(),
                        status: label(fields[1][0]),
                    });
                }
                if fields[1][1] != b'.' {
                    result.unstaged.push(ChangedFile {
                        path,
                        status: label(fields[1][1]),
                    });
                }
            }
        }
        result.operation = crate::operation::detect(&self.git_dir, || Ok(unmerged))?;
        if result.branch == "(detached)" && result.head_ref.is_none() {
            result.detached = true;
            result.branch = result
                .head
                .as_deref()
                .and_then(|oid| oid.get(..8))
                .map(|short| format!("Detached at {short}"))
                .unwrap_or_else(|| "Detached HEAD".into());
        }
        Ok(result)
    }

    pub fn commit_diff(&self, oid: &str) -> Result<Diff> {
        self.commit_patch(oid, None)
    }

    pub fn commit_path_diff(&self, oid: &str, path: &Path) -> Result<Diff> {
        validate_path(path)?;
        self.commit_patch(oid, Some(path))
    }

    fn commit_patch(&self, oid: &str, path: Option<&Path>) -> Result<Diff> {
        validate_oid(oid)?;
        let mut cmd = self.read_command();
        cmd.args([
            "show",
            "--format=",
            "--no-show-signature",
            "--diff-merges=first-parent",
            "--root",
            "--patch",
        ])
        .args(PATCH_OPTIONS)
        .arg(oid)
        .arg("--")
        .args(path);
        read_diff(cmd, None, false)
    }

    pub fn file_diff(&self, path: &Path, staged: bool) -> Result<Diff> {
        validate_path(path)?;
        let mut cmd = self.read_command();
        cmd.arg("diff").args(PATCH_OPTIONS);
        if staged {
            cmd.arg("--cached");
        }
        cmd.arg("--").arg(path);
        let source = Some((self.root.clone(), path.to_owned()));
        let mut diff = read_diff(cmd, source.clone(), false)?;
        diff.staged = staged;
        if staged || !diff.bytes.is_empty() || !diff.text.is_empty() {
            return Ok(diff);
        }

        let untracked = checked(
            self.read_command()
                .args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
                .arg(path),
        )?;
        if untracked.is_empty() {
            return Ok(diff);
        }
        let metadata = self
            .root
            .join(path)
            .symlink_metadata()
            .map_err(|e| e.to_string())?;
        if !(metadata.is_file() || metadata.is_symlink()) {
            return Err("A text diff is available for individual files.".into());
        }
        let mut cmd = self.read_command();
        cmd.args(["diff", "--no-index"])
            .args(PATCH_OPTIONS)
            .args(["--", "/dev/null"])
            .arg(path);
        read_diff(cmd, source, true)
    }

    pub fn stage_hunk(&self, diff: &Diff, index: usize) -> Result<()> {
        self.apply_hunk(diff, index, false, &["--cached"])
    }

    pub fn unstage_hunk(&self, diff: &Diff, index: usize) -> Result<()> {
        self.apply_hunk(diff, index, true, &["--cached", "--reverse"])
    }

    pub fn discard_hunk(&self, diff: &Diff, index: usize) -> Result<()> {
        self.apply_hunk(diff, index, false, &["--reverse"])
    }

    fn apply_hunk(&self, diff: &Diff, index: usize, staged: bool, mode: &[&str]) -> Result<()> {
        self.ensure_index_writable()?;
        let (root, path) = diff
            .source
            .as_ref()
            .filter(|_| diff.staged == staged)
            .ok_or(if staged {
                "Select a hunk from a staged file."
            } else {
                "Select a hunk from an unstaged file."
            })?;
        if root != &self.root {
            return Err("The selected hunk belongs to a different repository.".into());
        }
        let hunk = diff
            .hunks
            .get(index)
            .ok_or("The selected hunk is unavailable.")?;
        let latest = self.file_diff(path, staged)?;
        if latest.bytes != diff.bytes {
            return Err(
                "This file changed since its diff was opened. Refresh and select the hunk again."
                    .into(),
            );
        }
        let mut patch = Vec::new();
        for line in diff.bytes[diff.prefix.clone()].split_inclusive(|&b| b == b'\n') {
            if !line.starts_with(b"old mode ") && !line.starts_with(b"new mode ") {
                patch.extend_from_slice(line);
            }
        }
        patch.extend_from_slice(&diff.bytes[hunk.bytes.clone()]);
        let output = run_with_input(
            self.command().arg("apply").args(mode).args(["--whitespace=nowarn", "-"]),
            patch,
        )
        .map_err(|e| e.to_string())?;
        successful(output).map(drop)
    }

    pub fn unstage_file(&self, path: &Path) -> Result<()> {
        self.ensure_index_writable()?;
        validate_path(path)?;
        checked(
            self.command()
                .args(["--literal-pathspecs", "reset", "--quiet", "--"])
                .arg(path),
        )?;
        Ok(())
    }

    pub fn discard_file(&self, path: &Path, untracked: bool) -> Result<()> {
        self.ensure_index_writable()?;
        validate_path(path)?;
        let tracked = !checked(
            self.read_command()
                .args(["--literal-pathspecs", "ls-files", "-z", "--"])
                .arg(path),
        )?
        .is_empty();
        if tracked == untracked {
            return Err(format!(
                "{} is {} since you chose Discard. Look at it again before discarding.",
                path.display(),
                if tracked { "now tracked" } else { "no longer tracked" },
            ));
        }
        let mut cmd = self.command();
        cmd.arg("--literal-pathspecs");
        if tracked {
            cmd.args(["restore", "--worktree", "--"]);
        } else {
            cmd.args(["clean", "--force", "--quiet", "--"]);
        }
        checked(cmd.arg(path))?;
        Ok(())
    }

    pub fn stage_file(&self, path: &Path) -> Result<()> {
        self.ensure_index_writable()?;
        validate_path(path)?;
        checked(
            self.command()
                .args(["--literal-pathspecs", "add", "--"])
                .arg(path),
        )?;
        Ok(())
    }

    pub(crate) fn with_paths(&self, args: &[&str], paths: &[PathBuf]) -> Result<Vec<u8>> {
        if paths.is_empty() {
            return Err("Select at least one file.".into());
        }
        let mut input = Vec::new();
        for path in paths {
            validate_path(path)?;
            input.extend_from_slice(path.as_os_str().as_encoded_bytes());
            input.push(0);
        }
        let mut command = self.command();
        command
            .arg("--literal-pathspecs")
            .args(args)
            .args(["--pathspec-from-file=-", "--pathspec-file-nul"]);
        successful(run_with_input(&mut command, input).map_err(|e| format!("Run bundled Git: {e}"))?)
    }

    pub fn stage_files(&self, paths: &[PathBuf]) -> Result<()> {
        self.ensure_index_writable()?;
        self.with_paths(&["add"], paths).map(drop)
    }

    pub fn unstage_files(&self, paths: &[PathBuf]) -> Result<()> {
        self.ensure_index_writable()?;
        self.with_paths(&["reset", "--quiet"], paths).map(drop)
    }

    pub fn discard_files(&self, paths: &[PathBuf], untracked: &BTreeSet<PathBuf>) -> Result<()> {
        const CLEANED_AT_ONCE: usize = 64;
        self.ensure_index_writable()?;
        if paths.is_empty() {
            return Err("Select at least one file.".into());
        }
        for path in paths {
            validate_path(path)?;
        }
        let listed = checked(self.read_command().args(["ls-files", "-z", "--others", "--exclude-standard"]))?;
        let now_untracked: BTreeSet<PathBuf> = listed
            .split(|&b| b == 0)
            .filter(|entry| !entry.is_empty())
            .map(raw_path)
            .collect();
        for path in paths {
            let expected = untracked.contains(path);
            if now_untracked.contains(path) != expected {
                return Err(format!(
                    "{} is {} since you chose Discard. Look at it again before discarding.",
                    path.display(),
                    if expected { "now tracked" } else { "no longer tracked" },
                ));
            }
        }
        let (doomed, tracked): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.iter().cloned().partition(|path| untracked.contains(path));
        if !tracked.is_empty() {
            self.with_paths(&["restore", "--worktree"], &tracked)?;
        }
        for chunk in doomed.chunks(CLEANED_AT_ONCE) {
            checked(
                self.command()
                    .args(["--literal-pathspecs", "clean", "--force", "--quiet", "--"])
                    .args(chunk),
            )?;
        }
        Ok(())
    }

    pub fn commit(&self, message: &str) -> Result<Vec<PathBuf>> {
        self.ensure_index_writable()?;
        if message.trim().is_empty() {
            return Err("Enter a commit message.".into());
        }
        self.committing(self.command().args(["-c", "user.useConfigOnly=true", "commit", "-m", message]))
    }

    fn committing(&self, command: &mut Command) -> Result<Vec<PathBuf>> {
        let output = run(command).map_err(|e| format!("Run bundled Git: {e}"))?;
        let skipped = crate::explain::skipped_hooks(&String::from_utf8_lossy(&output.stderr))
            .into_iter()
            .map(|(_, path)| self.root.join(path))
            .collect();
        successful(output)?;
        Ok(skipped)
    }

    pub fn commit_message(&self, oid: &str) -> Result<String> {
        validate_oid(oid)?;
        let output = checked(self.read_command().args([
            "show", "--no-patch", "--no-notes", "--no-show-signature", "--format=format:%B", oid,
            "--",
        ]))?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    fn head_reference(&self) -> Result<Option<OsString>> {
        let output = self
            .read_command()
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .run()
            .map_err(|err| err.to_string())?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        successful(output).map(|name| Some(OsString::from_vec(without_final_newline(&name).into())))
    }

    pub fn amend(&self, oid: &str, reference: Option<&OsStr>, message: Option<&str>) -> Result<Vec<PathBuf>> {
        self.ensure_index_writable()?;
        validate_oid(oid)?;
        if message.is_some_and(|message| message.trim().is_empty()) {
            return Err("Enter a commit message.".into());
        }
        let same_branch = self.head_reference()?.as_deref() == reference;
        let head = checked(self.read_command().args(["rev-parse", "--verify", "HEAD"]))?;
        if !same_branch || without_final_newline(&head) != oid.as_bytes() {
            return Err("The checked-out branch or its tip changed. Reopen Amend last commit.".into());
        }
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true", "commit", "--amend", "--allow-empty"]);
        if let Some(message) = message {
            command.args(["-m", message]);
        } else {
            command.arg("--no-edit");
        }
        self.committing(&mut command)
    }

    pub fn stage_all(&self) -> Result<()> {
        self.ensure_index_writable()?;
        checked(self.command().args(["add", "--all", "--", "."]))?;
        Ok(())
    }

    pub fn unstage_all(&self) -> Result<()> {
        self.ensure_index_writable()?;
        checked(self.command().args(["reset", "--quiet", "--", "."]))?;
        Ok(())
    }

    pub fn commit_all(&self, head: Option<&str>, reference: Option<&OsStr>, message: &str) -> Result<Vec<PathBuf>> {
        self.ensure_index_writable()?;
        if message.trim().is_empty() {
            return Err("Enter a commit message.".into());
        }
        if let Some(oid) = head {
            validate_oid(oid)?;
        }
        let tip = self
            .read_command()
            .args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let tip = tip.status.success().then(|| without_final_newline(&tip.stdout).to_vec());
        if self.head_reference()?.as_deref() != reference || tip.as_deref() != head.map(str::as_bytes) {
            return Err("The checked-out branch or its tip changed. Look again before committing.".into());
        }
        checked(self.command().args(["add", "--all", "--", "."]))?;
        self.committing(self.command().args(["-c", "user.useConfigOnly=true", "commit", "-m", message]))
    }
}

pub(crate) fn bundled_version() -> Result<String> {
    let output = checked(bundled_command(Path::new("/"))?.arg("--version"))?;
    version_of(&String::from_utf8_lossy(&output)).ok_or_else(|| "Its version could not be read".to_owned())
}

fn version_of(output: &str) -> Option<String> {
    let version = output.trim().strip_prefix("git version ")?;
    version.starts_with(|c: char| c.is_ascii_digit()).then(|| version.to_owned())
}

pub(crate) fn validate_oid(oid: &str) -> Result<()> {
    if matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Select a commit by its full object ID.".into())
    }
}

pub(crate) fn validate_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Select a path relative to this repository.".into());
    }
    Ok(())
}

pub(crate) fn parse_changed_files(paths: &[u8]) -> Result<Vec<ChangedFile>> {
    let mut files = Vec::new();
    if !paths.is_empty() {
        let paths = paths
            .strip_suffix(b"\0")
            .ok_or("Incomplete changed-file list")?;
        let mut records = paths.split(|&b| b == 0);
        while let Some(code) = records.next() {
            let status = match code {
                b"A" => "Added",
                b"D" => "Deleted",
                b"M" => "Modified",
                b"T" => "Type changed",
                other => {
                    return Err(format!(
                        "Unknown file status {}",
                        String::from_utf8_lossy(other)
                    ));
                }
            };
            let path = raw_path(records.next().ok_or("Missing changed path")?);
            files.push(ChangedFile { status, path });
        }
    }
    Ok(files)
}

pub(crate) fn raw_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_owned()))
}
fn without_final_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
pub(crate) fn checked(command: &mut Command) -> Result<Vec<u8>> {
    successful(run(command).map_err(|e| format!("Run bundled Git: {e}"))?)
}

pub(crate) fn run(command: &mut Command) -> std::io::Result<Output> {
    let (started, at) = (std::time::Instant::now(), std::time::SystemTime::now());
    let result = match crate::jobs::Link::current() {
        Some(link) => stream(command, &link),
        None => command.output(),
    };
    crate::activity::record(command, started, at, &result);
    result
}

pub(crate) trait Run {
    fn run(&mut self) -> std::io::Result<Output>;
}

impl Run for Command {
    fn run(&mut self) -> std::io::Result<Output> {
        run(self)
    }
}

pub(crate) fn run_with_input(command: &mut Command, input: Vec<u8>) -> std::io::Result<Output> {
    let (started, at) = (std::time::Instant::now(), std::time::SystemTime::now());
    let result = (|| {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("Git input stream is unavailable"))?;
        let writer = thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output()?;
        let written = writer.join().map_err(|_| std::io::Error::other("Writing to Git failed"))?;
        match written {
            Err(error) if output.status.success() => Err(error),
            _ => Ok(output),
        }
    })();
    crate::activity::record(command, started, at, &result);
    result
}

fn cancelled() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
}

static RUNNING: std::sync::Mutex<Vec<libc::pid_t>> = std::sync::Mutex::new(Vec::new());

struct Running(libc::pid_t);

impl Running {
    fn add(group: libc::pid_t) -> Self {
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).push(group);
        Self(group)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|group| *group != self.0);
    }
}

pub(crate) fn stop_running() {
    for group in RUNNING.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        unsafe { libc::kill(-group, libc::SIGTERM) };
    }
}

const GRACE: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) fn stream(command: &mut Command, link: &crate::jobs::Link) -> std::io::Result<Output> {
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc;
    use std::time::Instant;
    if link.cancelled() {
        return Err(cancelled());
    }
    let own = link.cancellable();
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if own {
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let group = child.id() as libc::pid_t;
    let _running = own.then(|| Running::add(group));
    let (closed, pipes) = mpsc::channel();
    let mut stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("no standard output"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| std::io::Error::other("no standard error"))?;
    let out_closed = closed.clone();
    let out_link = link.clone();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        while let Ok(n) = stdout.read(&mut buffer) {
            if n == 0 {
                break;
            }
            out_link.alive();
            bytes.extend_from_slice(&buffer[..n]);
        }
        let _ = out_closed.send(());
        bytes
    });
    let err_link = link.clone();
    let err = thread::spawn(move || {
        let mut kept = Vec::new();
        let mut line = Vec::new();
        let mut buffer = [0; 4096];
        let mut after_return = false;
        let end = |line: &mut Vec<u8>, kept: &mut Vec<u8>| {
            let text = String::from_utf8_lossy(line).into_owned();
            if !text.trim().is_empty() {
                err_link.heard(&text);
            }
            if !crate::jobs::is_progress(&text) {
                kept.extend_from_slice(line);
                kept.push(b'\n');
            }
            line.clear();
        };
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 {
                break;
            }
            err_link.alive();
            for &byte in &buffer[..n] {
                match byte {
                    b'\n' if after_return && line.is_empty() => {}
                    b'\r' if line.is_empty() => {}
                    b'\r' | b'\n' => end(&mut line, &mut kept),
                    _ => line.push(byte),
                }
                after_return = byte == b'\r';
            }
        }
        if !line.is_empty() {
            end(&mut line, &mut kept);
            kept.pop();
        }
        let _ = closed.send(());
        kept
    });
    let signal = |signal: libc::c_int| {
        if own {
            unsafe { libc::kill(-group, signal) };
        }
    };
    let mut open = 2;
    let mut stopped: Option<Instant> = None;
    let mut killed = false;
    while open > 0 {
        match pipes.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(()) => open -= 1,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !link.cancelled() {
            continue;
        }
        match stopped {
            None => {
                signal(libc::SIGTERM);
                stopped = Some(Instant::now());
            }
            Some(at) if !killed && at.elapsed() >= GRACE => {
                signal(libc::SIGKILL);
                killed = true;
            }
            Some(at) if at.elapsed() >= GRACE * 2 => break,
            Some(_) => {}
        }
    }
    let status = child.wait()?;
    if stopped.is_some() {
        signal(libc::SIGKILL);
        return Err(cancelled());
    }
    let stdout = out.join().map_err(|_| std::io::Error::other("Cannot read Git's output"))?;
    let stderr = err.join().map_err(|_| std::io::Error::other("Cannot read Git's diagnostics"))?;
    Ok(Output { status, stdout, stderr })
}
fn successful(output: Output) -> Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(git_error(&output.stderr, output.status))
}
fn git_error(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let msg = String::from_utf8_lossy(stderr);
    if msg.trim().is_empty() {
        format!("Git exited with {status}")
    } else {
        msg.trim().to_owned()
    }
}

pub(crate) fn read_diff(
    mut command: Command,
    source: Option<(PathBuf, PathBuf)>,
    no_index: bool,
) -> Result<Diff> {
    let (started, at) = (std::time::Instant::now(), std::time::SystemTime::now());
    let spawned = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            let result = Err(error);
            crate::activity::record(&command, started, at, &result);
            return Err(result.unwrap_err().to_string());
        }
    };
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        while let Ok(n) = stderr.read(&mut buffer) {
            if n == 0 {
                break;
            }
            if bytes.len() < 64 * 1024 {
                bytes.extend_from_slice(&buffer[..n]);
            }
        }
        bytes
    });
    let mut bytes = Vec::new();
    let read = stdout
        .take((MAX_DIFF_BYTES + 1) as u64)
        .read_to_end(&mut bytes);
    let truncated = bytes.len() > MAX_DIFF_BYTES;
    if truncated || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let errors = errors.join().map_err(|_| "Cannot read Git diagnostics")?;
    let said = Output { status, stdout: Vec::new(), stderr: errors.clone() };
    crate::activity::record(&command, started, at, &Ok(said));
    read.map_err(|e| e.to_string())?;
    if truncated {
        bytes.truncate(MAX_DIFF_BYTES);
        return Ok(Diff {
            text: format!(
                "{}\n\n[Preview limited to 1 MiB]",
                String::from_utf8_lossy(&bytes)
            ),
            hunks: vec![],
            source: None,
            staged: false,
            bytes: vec![],
            prefix: 0..0,
        });
    }
    if !(status.success() || no_index && status.code() == Some(1)) {
        return Err(git_error(&errors, status));
    }
    Ok(parse_diff(bytes, source))
}

fn parse_diff(bytes: Vec<u8>, source: Option<(PathBuf, PathBuf)>) -> Diff {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut byte_offset = 0;
    let mut active = None::<usize>;
    let mut prefix_end = None;
    let mut files = 0;
    for (line_number, line) in bytes.split_inclusive(|&b| b == b'\n').enumerate() {
        let starts_hunk = line.starts_with(b"@@ ");
        let starts_file = line.starts_with(b"diff --git ");
        if starts_file {
            files += 1;
        }
        if starts_hunk || starts_file {
            if let Some(previous) = active.take() {
                hunks[previous].bytes.end = byte_offset;
            }
            if starts_hunk {
                prefix_end.get_or_insert(byte_offset);
                active = Some(hunks.len());
                hunks.push(Hunk {
                    start_line: line_number,
                    bytes: byte_offset..byte_offset,
                });
            }
        }
        byte_offset += line.len();
    }
    if let Some(i) = active {
        hunks[i].bytes.end = byte_offset;
    }
    if source.is_some() && files > 1 {
        hunks.clear();
    }
    Diff {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        hunks,
        source,
        staged: false,
        bytes,
        prefix: 0..prefix_end.unwrap_or(0),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    thread_local! {
        static GLOBAL_CONFIG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn global_config() -> PathBuf {
        GLOBAL_CONFIG.with(|file| file.borrow().clone()).unwrap_or_else(|| PathBuf::from("/dev/null"))
    }

    pub(crate) struct GlobalConfig(Option<PathBuf>);

    impl GlobalConfig {
        pub(crate) fn set(file: &Path) -> Self {
            Self(GLOBAL_CONFIG.with(|global| global.replace(Some(file.to_owned()))))
        }
    }

    impl Drop for GlobalConfig {
        fn drop(&mut self) {
            let before = self.0.take();
            GLOBAL_CONFIG.with(|global| *global.borrow_mut() = before);
        }
    }

    pub(crate) struct Fixture {
        pub(crate) dir: PathBuf,
        pub(crate) repo: Repository,
        git: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let runtime = env::var_os("FASTKRAKEN_GIT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(option_env!("CARGO_MANIFEST_DIR").unwrap_or("."))
                        .join("target/portable-git/runtime")
                });
            let git = runtime
                .join("bin/git")
                .canonicalize()
                .expect("Build private Git first or set FASTKRAKEN_GIT_ROOT");
            let engine = Engine::at(&git).unwrap();
            let dir = env::temp_dir().join(format!(
                "fastkraken-git-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            checked(
                engine
                    .command(&dir)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .args(["init", "-q", "-b", "main"]),
            )
            .unwrap();
            let repo = Repository::with_git(&dir, &git).unwrap();
            let fixture = Self { dir, repo, git };
            for (key, value) in [
                ("user.name", "Cedar"),
                ("user.email", "cedar@example.invalid"),
                ("core.hooksPath", "/dev/null"),
                ("commit.gpgSign", "false"),
                ("core.autocrlf", "false"),
            ] {
                fixture.git(&["config", "--local", key, value]);
            }
            fixture
        }
        pub(crate) fn git(&self, args: &[&str]) -> Vec<u8> {
            checked(self.repo.command().args(args)).unwrap()
        }

        pub(crate) fn open(&self, path: &Path) -> Repository {
            Repository::with_git(path, &self.git).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn amend_message_and_index_preserve_commit_parents_and_unstaged_edits() {
        let f = Fixture::new();
        let path = f.dir.join("notes");
        fs::write(&path, "base\n").unwrap();
        f.git(&["add", "notes"]);
        f.repo.commit("Base").unwrap();
        fs::write(&path, "original\n").unwrap();
        f.git(&["add", "notes"]);
        let message = "Original summary\n\nFirst paragraph.\n\nSecond paragraph.";
        f.repo.commit(message).unwrap();
        let head = || f.repo.status().unwrap().head.unwrap();
        let tip = head();
        assert_eq!(f.repo.commit_message(&tip).unwrap(), format!("{message}\n"));

        f.git(&["branch", "same-tip"]);
        f.git(&["switch", "same-tip"]);
        assert!(f.repo.amend(&tip, Some(OsStr::new("refs/heads/main")), Some("Wrong branch")).is_err());
        assert_eq!(head(), tip);
        f.git(&["switch", "main"]);

        f.repo.amend(&tip, Some(OsStr::new("refs/heads/main")), Some("New message")).unwrap();
        let amended = head();
        assert_ne!(amended, tip);
        assert_eq!(f.git(&["show", "HEAD:notes"]), b"original\n");
        assert!(f.repo.amend(&tip, Some(OsStr::new("refs/heads/main")), Some("Stale tip")).is_err());

        fs::write(&path, "staged\n").unwrap();
        f.git(&["add", "notes"]);
        fs::write(&path, "unstaged\n").unwrap();
        f.repo.amend(&amended, Some(OsStr::new("refs/heads/main")), None).unwrap();
        assert_eq!(f.repo.commit_message(&head()).unwrap(), "New message\n");
        assert_eq!(f.git(&["log", "-1", "--format=%s", "HEAD^"]), b"Base\n");
        assert_eq!(f.git(&["show", "HEAD:notes"]), b"staged\n");
        assert_eq!(fs::read_to_string(path).unwrap(), "unstaged\n");

        f.git(&["switch", "-c", "(detached)"]);
        let status = f.repo.status().unwrap();
        assert!(!status.detached);
        assert_eq!(status.head_ref.as_deref(), Some(OsStr::new("refs/heads/(detached)")));
        f.repo.amend(&head(), status.head_ref.as_deref(), Some("Branch with parentheses")).unwrap();
        f.git(&["switch", "--detach"]);
        let status = f.repo.status().unwrap();
        assert!(status.detached);
        assert!(status.head_ref.is_none());
        f.repo.amend(&head(), None, Some("Detached tip")).unwrap();
    }

    #[test]
    fn staged_hunk_commit_preserves_unselected_and_external_edits() {
        let f = Fixture::new();
        let name = Path::new(":(glob)* line\n-logbook");
        let target = f.dir.join(name);
        let baseline = (0..24).map(|i| format!("Marker {i}\n")).collect::<String>();
        fs::write(&target, &baseline).unwrap();
        assert_eq!(
            f.repo.status().unwrap().unstaged,
            [ChangedFile {
                path: name.into(),
                status: "Untracked"
            }]
        );
        assert!(f.repo.history(0, 10).unwrap().is_empty());
        assert!(f.repo.status().unwrap().head.is_none());
        assert_eq!(f.repo.file_diff(name, false).unwrap().hunks.len(), 1);
        f.repo.stage_file(name).unwrap();
        f.repo.commit("Base").unwrap();
        let edits = baseline
            .replace("Marker 1\n", "North\n")
            .replace("Marker 23\n", "South\n");
        fs::write(&target, &edits).unwrap();
        let before = f.repo.file_diff(name, false).unwrap();
        assert_eq!(before.hunks.len(), 2);
        fs::write(&target, edits.replace("South\n", "Newest\n")).unwrap();
        assert!(
            f.repo
                .stage_hunk(&before, 0)
                .unwrap_err()
                .contains("changed")
        );
        assert!(f.repo.status().unwrap().staged.is_empty());
        let current = f.repo.file_diff(name, false).unwrap();
        f.repo.stage_hunk(&current, 0).unwrap();
        let status = f.repo.status().unwrap();
        assert_eq!(status.branch, "main");
        assert_eq!(status.changed_paths, 1);
        assert_eq!(
            status.staged,
            [ChangedFile {
                path: name.into(),
                status: "Modified"
            }]
        );
        assert_eq!(status.unstaged, status.staged);
        let staged = f.repo.file_diff(name, true).unwrap();
        let remainder = f.repo.file_diff(name, false).unwrap();
        assert!(staged.text.contains("+North") && !staged.text.contains("+Newest"));
        assert!(remainder.text.contains("+Newest") && !remainder.text.contains("+North"));
        f.repo.commit("North only").unwrap();
        assert!(f.repo.status().unwrap().staged.is_empty());
        let head = f.repo.history(0, 1).unwrap().remove(0);
        let committed = f.repo.commit_diff(&head.oid).unwrap();
        assert_eq!(head.subject, "North only");
        assert!(committed.text.contains("+North") && !committed.text.contains("+Newest"));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            edits.replace("South\n", "Newest\n")
        );
    }

    #[test]
    fn bulk_index_actions_preserve_worktree_bytes_even_before_the_first_commit() {
        let f = Fixture::new();
        let path = f.dir.join("first");
        fs::write(&path, "before\n").unwrap();
        f.repo.stage_all().unwrap();
        f.repo.unstage_all().unwrap();
        assert!(f.git(&["ls-files", "-z"]).is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"before\n");
        f.repo.stage_all().unwrap();
        f.repo.commit("first").unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(f.dir.join("new"), "after\n").unwrap();
        f.repo.stage_all().unwrap();
        let status = f.repo.status().unwrap();
        assert!(status.unstaged.is_empty());
        assert_eq!(
            status.staged.iter().map(|p| p.status).collect::<Vec<_>>(),
            ["Deleted", "Added"]
        );
        f.repo.unstage_all().unwrap();
        assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
        assert!(!path.exists());
        assert_eq!(fs::read(f.dir.join("new")).unwrap(), b"after\n");
    }

    #[test]
    fn ordinary_staging_cannot_resolve_a_merge_behind_the_merge_panel() {
        let f = Fixture::new();
        let path = Path::new("trail.txt");
        let record = |value: &str| {
            fs::write(f.dir.join(path), value).unwrap();
            f.repo.stage_file(path).unwrap();
            f.repo.commit(value.trim()).unwrap();
        };
        record("Base\n");
        f.git(&["switch", "-qc", "feature"]);
        record("Incoming\n");
        f.git(&["switch", "-q", "main"]);
        record("Current\n");
        assert!(checked(f.repo.command().args(["merge", "--no-edit", "feature"])).is_err());
        let entries = f.git(&["ls-files", "-u", "-z"]);
        assert!(!entries.is_empty());
        let worktree = fs::read(f.dir.join(path)).unwrap();
        assert!(f.repo.stage_file(path).is_err());
        assert!(f.repo.stage_all().is_err());
        assert!(f.repo.unstage_all().is_err());
        assert_eq!(f.git(&["ls-files", "-u", "-z"]), entries);
        assert_eq!(fs::read(f.dir.join(path)).unwrap(), worktree);
    }

    #[test]
    fn commit_all_stages_every_change_and_commits_it() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "one\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        fs::write(f.dir.join("trail.txt"), "one\ntwo\n").unwrap();
        fs::write(f.dir.join("new.txt"), "new\n").unwrap();
        fs::write(f.dir.join("gone.txt"), "x").unwrap();
        f.git(&["add", "--", "gone.txt"]);
        fs::remove_file(f.dir.join("gone.txt")).unwrap();
        let branch = OsString::from("refs/heads/main");
        f.repo.commit_all(Some(&head), Some(&branch), "Record everything").unwrap();
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert_eq!(f.git(&["rev-parse", "HEAD~1"]), format!("{head}\n").into_bytes());
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Record everything\n");
        assert_eq!(
            f.git(&["diff", "--name-status", "HEAD~1", "HEAD"]),
            b"A\tnew.txt\nM\ttrail.txt\n"
        );
        assert_eq!(fs::read(f.dir.join("trail.txt")).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn commit_all_works_on_an_unborn_branch() {
        let f = Fixture::new();
        fs::write(f.dir.join("first.txt"), "first\n").unwrap();
        let branch = OsString::from("refs/heads/main");
        f.repo.commit_all(None, Some(&branch), "First").unwrap();
        assert_eq!(f.git(&["ls-files"]), b"first.txt\n");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
    }

    #[test]
    fn counts_and_lists_the_commits_a_range_holds() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Start notebook"]);
        f.git(&["switch", "-qc", "side"]);
        for subject in ["Draft one", "Draft two", "Draft three"] {
            f.git(&["commit", "--allow-empty", "-qm", subject]);
        }
        let side = Revisions::reachable("refs/heads/side").not("refs/heads/main");
        assert_eq!(f.repo.count(&side).unwrap(), 3);
        assert_eq!(f.repo.count(&Revisions::reachable("refs/heads/side")).unwrap(), 4);
        let listed = f.repo.subjects(&side, 2).unwrap();
        assert_eq!(
            listed.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(),
            ["Draft three", "Draft two"]
        );
        assert_eq!(listed[0].oid.as_bytes(), without_final_newline(&f.git(&["rev-parse", "side"])));
        assert!(f.repo.subjects(&side, 0).unwrap().is_empty());

        let only = Revisions::reachable("refs/heads/side").only_on("side");
        assert_eq!(f.repo.count(&only).unwrap(), 3);
        f.git(&["tag", "kept", "side~2"]);
        assert_eq!(f.repo.count(&only).unwrap(), 2);
        f.git(&["update-ref", "refs/remotes/origin/side", "side~1"]);
        assert_eq!(f.repo.count(&only).unwrap(), 1);
        f.git(&["branch", "copy", "side"]);
        assert_eq!(f.repo.count(&only).unwrap(), 0);
        assert_eq!(f.repo.subjects(&only, 5).unwrap(), Vec::new());

        for bad in [
            Revisions::reachable("--all"),
            Revisions::reachable("refs/heads/side").not("-n1"),
            Revisions::reachable("refs/heads/side").not(""),
            Revisions::reachable("refs/heads/side").only_on("--all"),
            Revisions::reachable("refs/heads/side").only_on("s*"),
        ] {
            assert!(f.repo.count(&bad).is_err());
            assert!(f.repo.subjects(&bad, 1).is_err());
        }
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/side\n");
        assert!(f.git(&["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn commit_all_refuses_a_branch_that_moved_and_stages_nothing() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "one\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start"]);
        let shown = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        f.git(&["commit", "-q", "--allow-empty", "-m", "Elsewhere"]);
        let moved = f.git(&["rev-parse", "HEAD"]);
        fs::write(f.dir.join("trail.txt"), "one\ntwo\n").unwrap();
        let index = f.git(&["ls-files", "--stage"]);
        let main = OsString::from("refs/heads/main");
        let error = f.repo.commit_all(Some(&shown), Some(&main), "Too late").unwrap_err();
        assert!(error.contains("changed"), "{error}");
        f.git(&["switch", "-qc", "side"]);
        let head = String::from_utf8(moved.clone()).unwrap().trim().to_owned();
        assert!(f.repo.commit_all(Some(&head), Some(&main), "Wrong branch").is_err());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), moved);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" M trail.txt\n");
        assert_eq!(fs::read(f.dir.join("trail.txt")).unwrap(), b"one\ntwo\n");
    }

    #[test]
    fn the_bundled_version_is_read_from_its_banner() {
        assert_eq!(version_of("git version 2.56.1\n").as_deref(), Some("2.56.1"));
        assert_eq!(version_of("git version 2.56.1.windows.1").as_deref(), Some("2.56.1.windows.1"));
        assert_eq!(version_of("fatal: nope"), None);
        assert_eq!(version_of("git version x"), None);
    }

    pub(crate) fn hanging_remote(f: &Fixture) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = f.repo.git_dir().join("hang.sh");
        let pid = f.repo.git_dir().join("hang.pid");
        fs::write(&script, format!("#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 600\n", pid.display())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        f.git(&["config", "core.sshCommand", script.to_str().unwrap()]);
        f.git(&["remote", "add", "origin", "ssh://hang.invalid/trail.git"]);
        pid
    }

    pub(crate) fn started(pid: &Path) -> i32 {
        let start = std::time::Instant::now();
        loop {
            if let Some(pid) = fs::read_to_string(pid).ok().and_then(|text| text.trim().parse().ok()) {
                return pid;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "the SSH script did not start");
            thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(crate) fn gone(pid: i32) -> bool {
        let start = std::time::Instant::now();
        while unsafe { libc::kill(pid, 0) } == 0 {
            if start.elapsed() > std::time::Duration::from_secs(2) {
                return false;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        true
    }

    #[test]
    fn only_a_job_that_talks_to_a_remote_runs_in_a_process_group_that_quitting_stops() {
        let script = "/bin/ps -o pgid= -p $$; echo $$";
        let groups = |link: &crate::jobs::Link| {
            let output = stream(Command::new("/bin/sh").args(["-c", script]), link).unwrap();
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let numbers: Vec<libc::pid_t> = text.split_whitespace().map(|n| n.parse().unwrap()).collect();
            (numbers[0], numbers[1])
        };
        let (group, pid) = groups(&crate::jobs::Link::new(true));
        assert_eq!(group, pid);
        let (group, pid) = groups(&crate::jobs::Link::new(false));
        assert_eq!(group, unsafe { libc::getpgrp() });
        assert_ne!(group, pid);
        let held = crate::jobs::Link::new(true);
        held.hold();
        let (group, _) = groups(&held);
        assert_eq!(group, unsafe { libc::getpgrp() });
    }

    #[test]
    fn a_streamed_command_hands_its_progress_to_the_job_and_keeps_it_out_of_the_error() {
        let script = "printf 'remote: Counting objects:  50%% (1/2)\\rremote: Counting objects: 100%% (2/2), done.\\nReceiving objects:  40%% (2/5)\\r' >&2; printf 'fatal: the remote hung up\\n' >&2; printf porcelain; exit 3";
        let link = crate::jobs::Link::new(true);
        let output = stream(Command::new("/bin/sh").args(["-c", script]), &link).unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"porcelain");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "fatal: the remote hung up\n");
        let progress = link.snapshot().0.unwrap();
        assert_eq!((progress.phase.as_str(), progress.percent), ("Receiving objects", Some(40)));
        let plain = run(Command::new("/bin/sh").args(["-c", script])).unwrap();
        assert!(String::from_utf8_lossy(&plain.stderr).contains("Counting objects"));
        let error = link.enter(|| checked(Command::new("/bin/sh").args(["-c", script]))).unwrap_err();
        assert_eq!(error, "fatal: the remote hung up");
    }

    #[test]
    fn cancel_stops_a_fetch_that_never_answers_and_everything_it_started() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Start"]);
        let pid = hanging_remote(&f);
        let link = crate::jobs::Link::new(true);
        let canceller = {
            let (link, pid) = (link.clone(), pid.clone());
            thread::spawn(move || {
                let script = started(&pid);
                link.cancel();
                (script, std::time::Instant::now())
            })
        };
        let error = link
            .enter(|| checked(f.repo.network_command().unwrap().args(["fetch", "--progress", "origin"])))
            .unwrap_err();
        let (script, cancelled) = canceller.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
        assert!(cancelled.elapsed() < std::time::Duration::from_secs(3));
        assert!(gone(script), "process {script} is still running");
        assert!(link.enter(|| checked(f.repo.read_command().arg("status"))).unwrap_err().contains("cancelled"));
        assert!(f.git(&["for-each-ref", "refs/remotes"]).is_empty());
    }

    #[test]
    fn a_commit_lands_while_a_fetch_waits_on_the_network() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Start"]);
        let pid = hanging_remote(&f);
        let link = crate::jobs::Link::new(true);
        let fetch = {
            let (repo, link) = (f.repo.clone(), link.clone());
            thread::spawn(move || link.enter(|| checked(repo.network_command().unwrap().args(["fetch", "origin"]))))
        };
        let script = started(&pid);
        fs::write(f.dir.join("trail.txt"), "camp\n").unwrap();
        f.repo.stage_file(Path::new("trail.txt")).unwrap();
        f.repo.commit("Note the camp").unwrap();
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Note the camp\n");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert!(!fetch.is_finished());
        link.cancel();
        assert!(fetch.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(gone(script));
    }

    #[test]
    fn network_commands_give_up_on_stalled_transfers_unless_ssh_is_the_users_own() {
        let f = Fixture::new();
        let command = f.repo.network_command().unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        for setting in ["http.lowSpeedLimit=1000", "http.lowSpeedTime=30"] {
            assert!(args.windows(2).any(|pair| pair == ["-c", setting]), "{args:?}");
        }
        let ssh = |command: &Command| {
            command.get_envs().find(|(key, _)| *key == "GIT_SSH_COMMAND").and_then(|(_, value)| value.map(OsStr::to_owned))
        };
        assert_eq!(ssh(&command).as_deref(), Some(OsStr::new(SSH_TIMEOUTS)));
        assert_eq!(SSH_TIMEOUTS, "ssh -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2");
        f.git(&["config", "core.sshCommand", "ssh -i /keys/trail"]);
        assert_eq!(ssh(&f.repo.network_command().unwrap()), None);
    }

    fn check_blocked_index(f: &Fixture, path: &Path, expected: crate::operation::Kind) {
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let before = fs::read(f.dir.join(path)).unwrap();
        assert_eq!(f.repo.status().unwrap().operation, Some(expected));
        let diff = f.repo.file_diff(path, false).unwrap();
        for result in [
            f.repo.stage_hunk(&diff, 0),
            f.repo.stage_file(path),
            f.repo.stage_all(),
            f.repo.unstage_all(),
            f.repo.commit("Ordinary commit").map(drop),
            f.repo.commit_all(None, None, "Ordinary commit").map(drop),
        ] {
            assert_eq!(result.unwrap_err(), format!("{}.", expected.refusal()));
        }
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(fs::read(f.dir.join(path)).unwrap(), before);
    }

    #[test]
    fn conflicted_stash_blocks_ordinary_index_writes_until_externally_resolved() {
        let f = Fixture::new();
        let path = Path::new("notes.txt");
        for (text, subject) in [("base\n", "Base"), ("different tip\n", "Diverge")] {
            fs::write(f.dir.join(path), text).unwrap();
            f.git(&["add", "--", "notes.txt"]);
            f.git(&["commit", "-qm", subject]);
            if subject == "Base" {
                fs::write(f.dir.join(path), "stashed edit\n").unwrap();
                f.git(&["stash", "push", "-q"]);
            }
        }
        let stash = f.git(&["rev-parse", "refs/stash"]);
        assert!(checked(f.repo.command().args(["stash", "pop"])).is_err());
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
        check_blocked_index(&f, path, crate::operation::Kind::Applied);
        assert_eq!(f.git(&["rev-parse", "refs/stash"]), stash);
        fs::write(f.dir.join(path), "external resolution\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        assert_eq!(f.repo.status().unwrap().operation, None);
        f.repo.commit("Resolve stashed edit").unwrap();
    }

    #[test]
    fn ordinary_commit_cannot_finish_a_revert_after_external_staging() {
        let f = Fixture::new();
        let path = Path::new("notes.txt");
        for text in ["base", "second", "third"] {
            fs::write(f.dir.join(path), format!("{text}\n")).unwrap();
            f.git(&["add", "--", "notes.txt"]);
            f.git(&["commit", "-qm", text]);
        }
        let tip = f.git(&["rev-parse", "HEAD"]);
        assert!(checked(f.repo.command().args(["revert", "--no-edit", "HEAD^"])).is_err());
        check_blocked_index(&f, path, crate::operation::Kind::Revert);
        fs::write(f.dir.join(path), "external resolution\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        assert!(f.git(&["ls-files", "--unmerged"]).is_empty());
        check_blocked_index(&f, path, crate::operation::Kind::Revert);
        f.git(&["revert", "--abort"]);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
        assert_eq!(f.repo.status().unwrap().operation, None);
        f.repo.stage_all().unwrap();
    }

    #[test]
    fn merge_diff_shows_first_parent_change_and_history_paginates() {
        let f = Fixture::new();
        let base = Path::new("field");
        fs::write(f.dir.join(base), "base\n").unwrap();
        f.repo.stage_file(base).unwrap();
        f.repo.commit("base").unwrap();
        f.git(&["switch", "-qc", "feature"]);
        fs::write(f.dir.join("map"), "Route\n").unwrap();
        f.repo.stage_file(Path::new("map")).unwrap();
        f.repo.commit("add map").unwrap();
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join(base), "revised\n").unwrap();
        f.repo.stage_file(base).unwrap();
        f.repo.commit("local work").unwrap();
        f.git(&["merge", "--no-edit", "feature"]);
        let commits = f.repo.history(0, 20).unwrap();
        assert_eq!(commits.len(), 4);
        assert_eq!(commits[0].parents.len(), 2);
        assert_eq!(commits[2..], f.repo.history(2, 20).unwrap());
        assert!(commits[0].refs.contains("main"));
        let patch = f.repo.commit_diff(&commits[0].oid).unwrap();
        assert!(patch.text.contains("+Route") && !patch.text.contains("+revised"));
        let root = f.repo.commit_diff(&commits.last().unwrap().oid).unwrap();
        assert!(root.text.contains("+base"));
    }

    #[test]
    fn detached_commit_is_visible_without_a_branch_ref() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "start"]);
        f.git(&["switch", "-q", "--detach"]);
        f.git(&["commit", "--allow-empty", "-qm", "detached work"]);
        let head = f.repo.history(0, 1).unwrap().remove(0);
        assert_eq!(head.subject, "detached work");
        assert_eq!(
            f.repo.status().unwrap().head.as_deref(),
            Some(head.oid.as_str())
        );
        assert_eq!(
            f.repo.status().unwrap().branch,
            format!("Detached at {}", &head.oid[..8])
        );
    }

    #[test]
    fn staging_content_leaves_independent_mode_change_in_worktree() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let name = Path::new("script");
        let path = f.dir.join(name);
        fs::write(&path, "one\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        f.repo.stage_file(name).unwrap();
        f.repo.commit("base").unwrap();
        fs::write(&path, "two\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let patch = f.repo.file_diff(name, false).unwrap();
        f.repo.stage_hunk(&patch, 0).unwrap();
        assert!(f.git(&["ls-files", "-s", "script"]).starts_with(b"100644 "));
        let remaining = f.repo.file_diff(name, false).unwrap();
        assert!(remaining.text.contains("new mode 100755"));
        assert!(!remaining.text.contains("+two"));
    }

    #[test]
    fn oversized_untracked_preview_is_bounded_and_read_only() {
        let f = Fixture::new();
        let name = Path::new("large.txt");
        fs::write(f.dir.join(name), "line\n".repeat(MAX_DIFF_BYTES / 4)).unwrap();
        let diff = f.repo.file_diff(name, false).unwrap();
        assert!(diff.text.contains("Preview limited"));
        assert!(diff.hunks.is_empty());
        assert!(f.repo.stage_hunk(&diff, 0).is_err());
        assert!(f.repo.status().unwrap().staged.is_empty());
    }

    #[test]
    fn unstage_and_discard_act_on_one_path_or_hunk_and_leave_the_rest() {
        let f = Fixture::new();
        let lines = |edit: &dyn Fn(usize) -> String| (1..=20).map(edit).collect::<String>();
        fs::write(f.dir.join("log.txt"), lines(&|i| format!("line {i}\n"))).unwrap();
        fs::write(f.dir.join("other.txt"), "other\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        let two_edits = lines(&|i| match i {
            2 => "line 2 edited\n".into(),
            18 => "line 18 edited\n".into(),
            _ => format!("line {i}\n"),
        });
        fs::write(f.dir.join("log.txt"), &two_edits).unwrap();
        fs::write(f.dir.join("other.txt"), "other edited\n").unwrap();
        f.git(&["add", "."]);
        let log = Path::new("log.txt");

        let staged = f.repo.file_diff(log, true).unwrap();
        assert_eq!(staged.hunks.len(), 2);
        assert!(f.repo.stage_hunk(&staged, 1).is_err());
        f.repo.unstage_hunk(&staged, 1).unwrap();
        let index = String::from_utf8(f.git(&["show", ":log.txt"])).unwrap();
        assert!(index.contains("line 2 edited") && index.contains("line 18\n"));
        assert_eq!(fs::read_to_string(f.dir.join("log.txt")).unwrap(), two_edits);

        let unstaged = f.repo.file_diff(log, false).unwrap();
        assert_eq!(unstaged.hunks.len(), 1);
        f.repo.discard_hunk(&unstaged, 0).unwrap();
        let work = fs::read_to_string(f.dir.join("log.txt")).unwrap();
        assert!(work.contains("line 2 edited") && work.contains("line 18\n"));

        f.repo.unstage_file(log).unwrap();
        assert_eq!(f.git(&["diff", "--cached", "--name-only"]), b"other.txt\n");

        f.repo.discard_file(log, false).unwrap();
        assert!(!fs::read_to_string(f.dir.join("log.txt")).unwrap().contains("edited"));
        fs::write(f.dir.join("scratch.txt"), "new\n").unwrap();
        f.repo.discard_file(Path::new("scratch.txt"), true).unwrap();
        assert!(!f.dir.join("scratch.txt").exists());
        assert_eq!(fs::read(f.dir.join("other.txt")).unwrap(), b"other edited\n");
    }

    #[test]
    fn discard_refuses_a_file_that_is_no_longer_what_the_prompt_described() {
        let f = Fixture::new();
        fs::write(f.dir.join("kept.txt"), "base\n").unwrap();
        f.git(&["add", "--", "kept.txt"]);
        f.git(&["commit", "-qm", "base"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);

        fs::write(f.dir.join("draft.txt"), "draft\n").unwrap();
        f.git(&["add", "--", "draft.txt"]);
        let staged = f.git(&["ls-files", "--stage", "-z"]);
        let err = f.repo.discard_file(Path::new("draft.txt"), true).unwrap_err();
        assert!(err.contains("now tracked"), "{err}");
        assert_eq!(fs::read(f.dir.join("draft.txt")).unwrap(), b"draft\n");
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), staged);
        f.git(&["rm", "-q", "--cached", "--", "draft.txt"]);
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);

        fs::write(f.dir.join("kept.txt"), "edited\n").unwrap();
        f.git(&["rm", "-q", "--cached", "--", "kept.txt"]);
        let err = f.repo.discard_file(Path::new("kept.txt"), false).unwrap_err();
        assert!(err.contains("no longer tracked"), "{err}");
        assert_eq!(fs::read(f.dir.join("kept.txt")).unwrap(), b"edited\n");
        assert!(f.git(&["ls-files", "-z"]).is_empty());

        fs::write(f.dir.join("*.txt"), "star\n").unwrap();
        let err = f.repo.discard_file(Path::new("*.txt"), false).unwrap_err();
        assert!(err.contains("no longer tracked"), "{err}");
        f.repo.discard_file(Path::new("*.txt"), true).unwrap();
        assert!(!f.dir.join("*.txt").exists());
        assert!(f.dir.join("draft.txt").exists() && f.dir.join("kept.txt").exists());
    }

    fn many_names() -> Vec<PathBuf> {
        (0..50)
            .map(|i| match i % 5 {
                0 => PathBuf::from(format!("notes {i}.txt")),
                1 => PathBuf::from(format!("café/crème brûlée {i}.txt")),
                2 => PathBuf::from(format!("日本語 {i}.md")),
                3 => PathBuf::from(format!("-dash *{i}?.txt")),
                _ => PathBuf::from(format!("deep/path with  spaces/[{i}] ☃.txt")),
            })
            .collect()
    }

    fn names(bytes: Vec<u8>) -> BTreeSet<PathBuf> {
        bytes.split(|&b| b == 0).filter(|entry| !entry.is_empty()).map(raw_path).collect()
    }

    #[test]
    fn staging_fifty_paths_with_spaces_and_non_ascii_names_moves_exactly_those_paths() {
        let f = Fixture::new();
        fs::write(f.dir.join("base.txt"), "base\n").unwrap();
        f.git(&["add", "--", "base.txt"]);
        f.git(&["commit", "-qm", "base"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let paths = many_names();
        for path in &paths {
            fs::create_dir_all(f.dir.join(path).parent().unwrap()).unwrap();
            fs::write(f.dir.join(path), format!("{}\n", path.display())).unwrap();
        }
        fs::write(f.dir.join("notes 1.txt"), "left alone\n").unwrap();
        fs::write(f.dir.join("base.txt"), "edited\n").unwrap();

        f.repo.stage_files(&paths).unwrap();
        let staged = names(f.git(&["-c", "core.quotePath=false", "diff", "--cached", "--name-only", "-z"]));
        assert_eq!(staged, paths.iter().cloned().collect::<BTreeSet<_>>());
        assert_eq!(f.git(&["diff", "--name-only"]), b"base.txt\n");
        assert_eq!(f.git(&["ls-files", "--others", "--exclude-standard"]), b"notes 1.txt\n");
        assert_eq!(fs::read(f.dir.join(&paths[1])).unwrap(), format!("{}\n", paths[1].display()).into_bytes());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);

        f.repo.unstage_files(&paths[..20]).unwrap();
        let staged = names(f.git(&["diff", "--cached", "--name-only", "-z"]));
        assert_eq!(staged, paths[20..].iter().cloned().collect::<BTreeSet<_>>());
        assert!(paths.iter().all(|path| f.dir.join(path).exists()));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn staging_and_unstaging_several_files_works_before_the_first_commit_and_stages_deletions() {
        let f = Fixture::new();
        let paths = [PathBuf::from("one.txt"), PathBuf::from("two words.txt")];
        for path in &paths {
            fs::write(f.dir.join(path), "new\n").unwrap();
        }
        f.repo.stage_files(&paths).unwrap();
        assert_eq!(names(f.git(&["ls-files", "-z"])), paths.iter().cloned().collect());
        f.repo.unstage_files(&paths).unwrap();
        assert!(f.git(&["ls-files", "-z"]).is_empty());

        f.repo.stage_files(&paths).unwrap();
        f.git(&["commit", "-qm", "both"]);
        fs::remove_file(f.dir.join(&paths[0])).unwrap();
        fs::write(f.dir.join(&paths[1]), "edited\n").unwrap();
        f.repo.stage_files(&paths).unwrap();
        assert_eq!(f.git(&["diff", "--cached", "--name-status"]), b"D\tone.txt\nM\ttwo words.txt\n");
    }

    #[test]
    fn staging_several_files_is_refused_during_a_conflict_or_for_a_path_outside_the_repository() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "base\n").unwrap();
        fs::write(f.dir.join("other.txt"), "other\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["switch", "-q", "-c", "side"]);
        fs::write(f.dir.join("trail.txt"), "side\n").unwrap();
        f.git(&["commit", "-qam", "side"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("trail.txt"), "main\n").unwrap();
        f.git(&["commit", "-qam", "main"]);
        assert!(!f.repo.command().args(["merge", "-q", "side"]).output().unwrap().status.success());
        fs::write(f.dir.join("other.txt"), "edited\n").unwrap();
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let paths = [PathBuf::from("other.txt"), PathBuf::from("trail.txt")];
        let refusal = "Finish or abort the merge first.";
        assert_eq!(f.repo.stage_files(&paths).unwrap_err(), refusal);
        assert_eq!(f.repo.unstage_files(&paths).unwrap_err(), refusal);
        assert_eq!(f.repo.discard_files(&paths, &BTreeSet::new()).unwrap_err(), refusal);
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(fs::read(f.dir.join("other.txt")).unwrap(), b"edited\n");

        f.git(&["merge", "--abort"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let outside = [PathBuf::from("other.txt"), PathBuf::from("../escape.txt")];
        assert!(f.repo.stage_files(&outside).is_err());
        assert!(f.repo.stage_files(&[]).is_err());
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
    }

    #[test]
    fn discarding_several_files_restores_tracked_ones_deletes_untracked_ones_and_keeps_staged_changes() {
        let f = Fixture::new();
        for name in ["a.txt", "b c.txt", "kept.txt"] {
            fs::write(f.dir.join(name), "base\n").unwrap();
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        fs::write(f.dir.join("a.txt"), "staged\n").unwrap();
        f.git(&["add", "--", "a.txt"]);
        fs::write(f.dir.join("a.txt"), "staged and more\n").unwrap();
        fs::write(f.dir.join("b c.txt"), "edited\n").unwrap();
        fs::write(f.dir.join("kept.txt"), "edited too\n").unwrap();
        fs::write(f.dir.join("new é.txt"), "new\n").unwrap();
        fs::write(f.dir.join("other new.txt"), "new\n").unwrap();
        let paths = [PathBuf::from("a.txt"), PathBuf::from("b c.txt"), PathBuf::from("new é.txt")];
        let untracked = BTreeSet::from([PathBuf::from("new é.txt")]);

        f.repo.discard_files(&paths, &untracked).unwrap();
        assert_eq!(fs::read(f.dir.join("a.txt")).unwrap(), b"staged\n");
        assert_eq!(f.git(&["show", ":a.txt"]), b"staged\n");
        assert_eq!(fs::read(f.dir.join("b c.txt")).unwrap(), b"base\n");
        assert!(!f.dir.join("new é.txt").exists());
        assert_eq!(fs::read(f.dir.join("kept.txt")).unwrap(), b"edited too\n");
        assert!(f.dir.join("other new.txt").exists());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn discarding_several_files_changes_nothing_when_one_is_no_longer_what_the_prompt_described() {
        let f = Fixture::new();
        fs::write(f.dir.join("tracked.txt"), "base\n").unwrap();
        f.git(&["add", "--", "tracked.txt"]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(f.dir.join("tracked.txt"), "edited\n").unwrap();
        fs::write(f.dir.join("draft.txt"), "draft\n").unwrap();
        fs::write(f.dir.join("scratch.txt"), "scratch\n").unwrap();
        let paths = [PathBuf::from("tracked.txt"), PathBuf::from("draft.txt"), PathBuf::from("scratch.txt")];
        let untracked = BTreeSet::from([PathBuf::from("draft.txt"), PathBuf::from("scratch.txt")]);
        f.git(&["add", "--", "draft.txt"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);

        let err = f.repo.discard_files(&paths, &untracked).unwrap_err();
        assert!(err.contains("draft.txt is now tracked"), "{err}");
        assert_eq!(fs::read(f.dir.join("tracked.txt")).unwrap(), b"edited\n");
        assert_eq!(fs::read(f.dir.join("scratch.txt")).unwrap(), b"scratch\n");
        assert_eq!(fs::read(f.dir.join("draft.txt")).unwrap(), b"draft\n");
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);

        f.git(&["rm", "-q", "--cached", "--", "tracked.txt"]);
        let only_tracked = [PathBuf::from("tracked.txt")];
        let err = f.repo.discard_files(&only_tracked, &BTreeSet::new()).unwrap_err();
        assert!(err.contains("no longer tracked"), "{err}");
        assert_eq!(fs::read(f.dir.join("tracked.txt")).unwrap(), b"edited\n");
    }
}
