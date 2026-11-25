use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    io::{BufRead, Read, Write},
    ops::Range,
    os::unix::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use crate::{graph_view::GraphView, hooks::Committed, line_patch::Direction, notice::Failure};

type Result<T> = std::result::Result<T, String>;
const MAX_DIFF_BYTES: usize = 1024 * 1024;
const SSH_TIMEOUTS: &str = "ssh -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2";
const WHOLE_FILE: &str = "--unified=1000000";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DiffOptions {
    pub(crate) whole_file: bool,
    pub(crate) ignore_whitespace: bool,
}

pub(crate) fn patch_options(renames: bool, options: DiffOptions) -> Vec<&'static str> {
    let mut args = vec![
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        if renames { "--find-renames" } else { "--no-renames" },
        "--full-index",
        if options.whole_file { WHOLE_FILE } else { "--unified=3" },
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ];
    if options.ignore_whitespace {
        args.push("--ignore-all-space");
    }
    args
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author: String,
    pub email: String,
    pub date: String,
    pub time: i64,
    pub refs: String,
    pub body: String,
}

pub(crate) fn first_line(body: &str) -> String {
    body.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned()
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
    pub from: Option<PathBuf>,
}

pub(crate) const RENAMED: &str = "Renamed";

fn status_label(code: u8) -> &'static str {
    match code {
        b'A' => "Added",
        b'D' => "Deleted",
        b'T' => "Type changed",
        b'U' => "Conflicted",
        b'R' => RENAMED,
        b'C' => "Copied",
        b'?' => "Untracked",
        _ => "Modified",
    }
}

#[derive(Clone, Debug)]
pub struct Diff {
    pub text: String,
    pub hunks: Vec<Hunk>,
    pub truncated: bool,
    source: Option<(PathBuf, PathBuf)>,
    staged: bool,
    bytes: Vec<u8>,
    prefix: Range<usize>,
    pub(crate) options: DiffOptions,
}

impl Diff {
    pub fn stageable(&self) -> bool {
        self.source.is_some() && !self.options.ignore_whitespace
    }

    pub(crate) fn shown_with(mut self, options: DiffOptions) -> Self {
        self.options = options;
        self
    }

    pub(crate) fn hunks_stageable(&self) -> bool {
        self.stageable() && !self.hunks.is_empty()
    }

    pub(crate) fn hunk_patch(&self, index: usize) -> Option<String> {
        let (header, hunk) = self.hunk_parts(index)?;
        let mut patch = header.to_vec();
        patch.extend_from_slice(hunk);
        if !patch.ends_with(b"\n") {
            patch.push(b'\n');
        }
        Some(String::from_utf8_lossy(&patch).into_owned())
    }

    pub(crate) fn hunk_parts(&self, index: usize) -> Option<(&[u8], &[u8])> {
        let hunk = self.hunks.get(index)?;
        let bytes = self.bytes.get(..hunk.bytes.end)?;
        let file = (0..hunk.bytes.start)
            .rev()
            .find(|&at| bytes[at..].starts_with(b"diff --git ") && (at == 0 || bytes[at - 1] == b'\n'))?;
        let header = self.hunks.iter().map(|hunk| hunk.bytes.start).find(|&start| start >= file)?;
        Some((&bytes[file..header], &bytes[hunk.bytes.clone()]))
    }

    pub(crate) fn same_patch(&self, other: &Diff) -> bool {
        !self.truncated && !other.truncated && self.bytes == other.bytes
    }
}

#[derive(Clone, Copy)]
enum Part {
    Hunk,
    Lines,
}

impl Part {
    fn name(self) -> &'static str {
        match self {
            Self::Hunk => "hunk",
            Self::Lines => "lines",
        }
    }

    fn belongs(self) -> &'static str {
        match self {
            Self::Hunk => "hunk belongs",
            Self::Lines => "lines belong",
        }
    }

    fn some(self) -> &'static str {
        match self {
            Self::Hunk => "a hunk",
            Self::Lines => "lines",
        }
    }
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

    #[cfg(test)]
    pub fn history(&self, offset: usize, limit: usize) -> Result<Vec<Commit>> {
        self.history_in(&GraphView::default(), offset, limit)
    }

    #[cfg(test)]
    pub(crate) fn history_in(&self, view: &GraphView, offset: usize, limit: usize) -> Result<Vec<Commit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Self::read_log(
            self.log_command(false)
                .args(view.history_args())
                .arg(format!("--skip={offset}"))
                .arg(format!("--max-count={limit}")),
        )
    }

    pub(crate) fn positions(
        &self,
        revisions: &[OsString],
        wanted: &HashSet<String>,
        stop: &AtomicBool,
    ) -> Result<HashMap<String, usize>> {
        let mut found = HashMap::new();
        if wanted.is_empty() {
            return Ok(found);
        }
        let mut command = self.read_command();
        command.args(["rev-list", "--topo-order"]).args(revisions).arg("--");
        let (started, at) = (std::time::Instant::now(), std::time::SystemTime::now());
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let mut read = Ok(());
        let mut whole = true;
        if let Some(stdout) = child.stdout.take() {
            for (index, line) in std::io::BufReader::new(stdout).lines().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    whole = false;
                    break;
                }
                match line {
                    Ok(line) if wanted.contains(line.as_str()) => {
                        found.insert(line, index);
                        if found.len() == wanted.len() {
                            whole = false;
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        read = Err(error.to_string());
                        whole = false;
                        break;
                    }
                }
            }
        }
        if !whole {
            let _ = child.kill();
        }
        let mut errors = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut errors);
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        crate::activity::record(&command, started, at, &Ok(Output { status, stdout: Vec::new(), stderr: errors.clone() }));
        read?;
        if stop.load(Ordering::Relaxed) {
            return Err("Stopped before Git finished listing the history.".to_owned());
        }
        if whole && !status.success() {
            return Err(git_error(&errors, status));
        }
        Ok(found)
    }

    pub(crate) fn has_commit(&self, oid: &str) -> Result<bool> {
        validate_oid(oid)?;
        let output = self
            .read_command()
            .args(["cat-file", "-e"])
            .arg(format!("{oid}^{{commit}}"))
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        Ok(output.status.success())
    }

    pub(crate) fn on_any_ref(&self, oid: &str) -> Result<bool> {
        validate_oid(oid)?;
        let refs = checked(
            self.read_command()
                .args(["for-each-ref", "--count=1", "--format=%(refname)"])
                .arg(format!("--contains={oid}"))
                .args(["refs/heads", "refs/remotes", "refs/tags"]),
        )?;
        Ok(!refs.trim_ascii().is_empty())
    }

    pub(crate) fn search_history(&self, query: &str, limit: usize) -> Result<Vec<Commit>> {
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let matching = |pattern: String| {
            Self::read_log(
                self.log_command(false)
                    .args(["--exclude=refs/stash", "--all", "--regexp-ignore-case", "--fixed-strings"])
                    .arg(pattern)
                    .arg(format!("--max-count={limit}")),
            )
        };
        let mut commits = matching(format!("--grep={query}"))?;
        let by_author = matching(format!("--author={query}"))?;
        if commits.is_empty() {
            commits = by_author;
        } else if !by_author.is_empty() {
            commits = self.in_history_order(commits, by_author, limit)?;
        }
        if (4..=64).contains(&query.len()) && query.bytes().all(|b| b.is_ascii_hexdigit()) {
            let prefix = query.to_ascii_lowercase();
            let mut exact = Self::read_log(
                self.log_command(false).arg("--max-count=1").arg(format!("{prefix}^{{commit}}")),
            ).unwrap_or_default();
            if let Some(commit) = exact.pop().filter(|commit| commit.oid.starts_with(&prefix)) {
                commits.retain(|row| row.oid != commit.oid);
                commits.insert(0, commit);
                commits.truncate(limit);
            }
        }
        Ok(commits)
    }

    fn in_history_order(&self, first: Vec<Commit>, second: Vec<Commit>, limit: usize) -> Result<Vec<Commit>> {
        let mut all = first;
        for commit in second {
            if !all.iter().any(|row| row.oid == commit.oid) {
                all.push(commit);
            }
        }
        let wanted = all.iter().map(|row| row.oid.clone()).collect::<HashSet<_>>();
        let at = self.positions(&GraphView::default().history_args(), &wanted, &AtomicBool::new(false))?;
        all.sort_by_key(|row| at.get(&row.oid).copied().unwrap_or(usize::MAX));
        all.truncate(limit);
        Ok(all)
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
            self.log_command(false)
                .arg(format!("--max-count={limit}"))
                .args(revisions.args()?),
        )
    }

    pub(crate) fn log_command(&self, bodies: bool) -> Command {
        let mut cmd = self.read_command();
        cmd.args([
            "log",
            "--topo-order",
            "--decorate=short",
            "--no-color",
            "--no-show-signature",
            "--no-patch",
            "-z",
            if bodies {
                "--format=%H%x00%P%x00%an%x00%ae%x00%as%x00%at%x00%D%x00%s%x00%b"
            } else {
                "--format=%H%x00%P%x00%an%x00%ae%x00%as%x00%at%x00%D%x00%s%x00"
            },
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
                email: field()?,
                date: field()?,
                time: field()?.parse().unwrap_or(0),
                refs: field()?,
                subject: field()?,
                body: first_line(&field()?),
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
            "-z",
        ]))?;
        let mut result = Status::default();
        let mut unmerged = false;
        let mut entries = output.split(|&b| b == 0);
        while let Some(entry) = entries.next() {
            if entry.is_empty() {
                continue;
            }
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
                    from: None,
                });
            } else if matches!(entry.first(), Some(b'1' | b'2' | b'u')) {
                unmerged |= entry[0] == b'u';
                let count = match entry[0] {
                    b'u' => 11,
                    b'2' => 10,
                    _ => 9,
                };
                let fields: Vec<_> = entry.splitn(count, |&b| b == b' ').collect();
                if fields.len() != count || fields[1].len() != 2 {
                    return Err("Invalid Git status record".into());
                }
                let path = raw_path(fields[count - 1]);
                let from = match entry[0] {
                    b'2' => Some(raw_path(
                        entries.next().filter(|from| !from.is_empty()).ok_or("Invalid Git status record")?,
                    )),
                    _ => None,
                };
                let paired = |code: u8| from.clone().filter(|_| matches!(code, b'R' | b'C'));
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
                        from: paired(fields[1][0]),
                    });
                }
                if fields[1][1] != b'.' {
                    result.unstaged.push(ChangedFile {
                        path,
                        status: label(fields[1][1]),
                        from: paired(fields[1][1]),
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

    #[cfg(test)]
    pub fn commit_diff(&self, oid: &str) -> Result<Diff> {
        self.commit_diff_with(oid, DiffOptions::default())
    }

    pub(crate) fn commit_diff_with(&self, oid: &str, options: DiffOptions) -> Result<Diff> {
        self.commit_patch(oid, None, None, options)
    }

    #[cfg(test)]
    pub fn commit_path_diff(&self, oid: &str, path: &Path, from: Option<&Path>) -> Result<Diff> {
        self.commit_path_diff_with(oid, path, from, DiffOptions::default())
    }

    pub(crate) fn commit_path_diff_with(&self, oid: &str, path: &Path, from: Option<&Path>, options: DiffOptions) -> Result<Diff> {
        validate_path(path)?;
        if let Some(from) = from {
            validate_path(from)?;
        }
        self.commit_patch(oid, Some(path), from, options)
    }

    fn commit_patch(&self, oid: &str, path: Option<&Path>, from: Option<&Path>, options: DiffOptions) -> Result<Diff> {
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
        .args(patch_options(true, options))
        .arg(oid)
        .arg("--")
        .args(from)
        .args(path);
        Ok(read_diff(cmd, None, false)?.shown_with(options))
    }

    #[cfg(test)]
    pub fn file_diff(&self, path: &Path, staged: bool) -> Result<Diff> {
        self.file_diff_with(path, staged, DiffOptions::default())
    }

    pub(crate) fn file_diff_with(&self, path: &Path, staged: bool, options: DiffOptions) -> Result<Diff> {
        validate_path(path)?;
        let mut cmd = self.read_command();
        cmd.arg("diff").args(patch_options(false, options));
        if staged {
            cmd.arg("--cached");
        }
        cmd.arg("--").arg(path);
        let source = Some((self.root.clone(), path.to_owned()));
        let mut diff = read_diff(cmd, source.clone(), false)?.shown_with(options);
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
            .args(patch_options(false, options))
            .args(["--", "/dev/null"])
            .arg(path);
        Ok(read_diff(cmd, source, true)?.shown_with(options))
    }

    #[cfg(test)]
    pub fn staged_rename_diff(&self, from: &Path, path: &Path) -> Result<Diff> {
        self.staged_rename_diff_with(from, path, DiffOptions::default())
    }

    pub(crate) fn staged_rename_diff_with(&self, from: &Path, path: &Path, options: DiffOptions) -> Result<Diff> {
        validate_path(from)?;
        validate_path(path)?;
        let mut cmd = self.read_command();
        cmd.args(["diff", "--cached"])
            .args(patch_options(true, options))
            .arg("--")
            .arg(from)
            .arg(path);
        let mut diff = read_diff(cmd, None, false)?.shown_with(options);
        diff.staged = true;
        Ok(diff)
    }

    pub(crate) fn object_sizes(&self, ids: &[String]) -> Result<std::collections::HashMap<String, u64>> {
        let mut input = Vec::new();
        for id in ids {
            validate_oid(id)?;
            input.extend_from_slice(id.as_bytes());
            input.push(b'\n');
        }
        let output = run_with_input(
            self.read_command().args(["cat-file", "--batch-check=%(objectname) %(objectsize)"]),
            input,
        )
        .map_err(|e| e.to_string())?;
        let output = successful(output)?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .filter_map(|line| {
                let (id, size) = line.split_once(' ')?;
                Some((id.to_owned(), size.parse().ok()?))
            })
            .collect())
    }

    pub(crate) fn blobs(&self, ids: &[String]) -> Result<std::collections::HashMap<String, Vec<u8>>> {
        let mut input = Vec::new();
        for id in ids {
            validate_oid(id)?;
            input.extend_from_slice(id.as_bytes());
            input.push(b'\n');
        }
        let output = run_with_input(self.read_command().args(["cat-file", "--batch=%(objectname) %(objecttype) %(objectsize)"]), input)
            .map_err(|e| e.to_string())?;
        let output = successful(output)?;
        let mut blobs = std::collections::HashMap::new();
        let mut rest = &output[..];
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = &rest[end + 1..];
            let mut fields = header.split(' ');
            let (Some(id), Some(kind), Some(size)) = (fields.next(), fields.next(), fields.next()) else { continue };
            let size: usize = size.parse().map_err(|_| "Git listed an object without a size".to_owned())?;
            let content = rest.get(..size).ok_or("Git ended an object early")?;
            rest = rest.get(size + 1..).unwrap_or_default();
            if kind == "blob" {
                blobs.insert(id.to_owned(), content.to_vec());
            }
        }
        Ok(blobs)
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

    pub(crate) fn stage_lines(&self, diff: &Diff, lines: &[(usize, usize)]) -> Result<()> {
        self.apply_lines(diff, lines, false, Direction::Forward, &["--cached"])
    }

    pub(crate) fn unstage_lines(&self, diff: &Diff, lines: &[(usize, usize)]) -> Result<()> {
        self.apply_lines(diff, lines, true, Direction::Reverse, &["--cached", "--reverse"])
    }

    pub(crate) fn discard_lines(&self, diff: &Diff, lines: &[(usize, usize)]) -> Result<()> {
        self.apply_lines(diff, lines, false, Direction::Reverse, &["--reverse"])
    }

    fn apply_lines(&self, diff: &Diff, lines: &[(usize, usize)], staged: bool, direction: Direction, mode: &[&str]) -> Result<()> {
        self.apply_part(diff, staged, Part::Lines, mode, |diff| {
            let hunks: Vec<&[u8]> = diff.hunks.iter().map(|hunk| &diff.bytes[hunk.bytes.clone()]).collect();
            crate::line_patch::build(&diff.bytes[diff.prefix.clone()], &hunks, lines, direction)
        })
    }

    fn apply_hunk(&self, diff: &Diff, index: usize, staged: bool, mode: &[&str]) -> Result<()> {
        self.apply_part(diff, staged, Part::Hunk, mode, |diff| {
            let hunk = diff
                .hunks
                .get(index)
                .ok_or("The selected hunk is unavailable.")?;
            let mut patch = Vec::new();
            for line in diff.bytes[diff.prefix.clone()].split_inclusive(|&b| b == b'\n') {
                if !line.starts_with(b"old mode ") && !line.starts_with(b"new mode ") {
                    patch.extend_from_slice(line);
                }
            }
            patch.extend_from_slice(&diff.bytes[hunk.bytes.clone()]);
            Ok(patch)
        })
    }

    fn apply_part(
        &self,
        diff: &Diff,
        staged: bool,
        part: Part,
        mode: &[&str],
        build: impl FnOnce(&Diff) -> Result<Vec<u8>>,
    ) -> Result<()> {
        self.ensure_index_writable()?;
        if diff.options.ignore_whitespace {
            return Err(format!(
                "Whitespace changes are hidden in this diff, so its {} cannot be applied. Show whitespace and select the {} again.",
                part.name(),
                part.name()
            ));
        }
        let (root, path) = diff
            .source
            .as_ref()
            .filter(|_| diff.staged == staged)
            .ok_or_else(|| format!("Select {} from {} file.", part.some(), if staged { "a staged" } else { "an unstaged" }))?;
        if root != &self.root {
            return Err(format!("The selected {} to a different repository.", part.belongs()));
        }
        let patch = build(diff)?;
        let latest = self.file_diff_with(path, staged, diff.options)?;
        if latest.bytes != diff.bytes {
            return Err(format!(
                "This file changed since its diff was opened. Refresh and select the {} again.",
                part.name()
            ));
        }
        let output = run_with_input(
            self.command().arg("apply").args(mode).args(["--whitespace=nowarn", "-"]),
            patch,
        )
        .map_err(|e| e.to_string())?;
        successful(output).map(drop)
    }

    pub fn unstage_file(&self, path: &Path) -> Result<()> {
        self.unstage_files(&[path.to_owned()])
    }

    pub(crate) fn with_origins(&self, paths: &[PathBuf], staged: bool) -> Result<Vec<PathBuf>> {
        for path in paths {
            validate_path(path)?;
        }
        let status = self.status()?;
        Ok(with_origins(if staged { &status.staged } else { &status.unstaged }, paths))
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
        self.stage_files(&[path.to_owned()])
    }

    pub(crate) fn with_paths(&self, args: &[&str], paths: &[PathBuf]) -> Result<Vec<u8>> {
        self.paths_into(self.command(), args, paths)
    }

    pub(crate) fn paths_into(&self, mut command: Command, args: &[&str], paths: &[PathBuf]) -> Result<Vec<u8>> {
        let input = nul_separated(paths, b"")?;
        command
            .arg("--literal-pathspecs")
            .args(args)
            .args(["--pathspec-from-file=-", "--pathspec-file-nul"]);
        successful(run_with_input(&mut command, input).map_err(|e| format!("Run bundled Git: {e}"))?)
    }

    pub fn stage_files(&self, paths: &[PathBuf]) -> Result<()> {
        self.ensure_index_writable()?;
        let paths = self.with_origins(paths, false)?;
        self.with_paths(&["add"], &paths).map(drop)
    }

    pub fn unstage_files(&self, paths: &[PathBuf]) -> Result<()> {
        self.ensure_index_writable()?;
        let paths = self.with_origins(paths, true)?;
        self.with_paths(&["reset", "--quiet"], &paths).map(drop)
    }

    #[cfg(test)]
    pub fn discard_files(&self, paths: &[PathBuf], untracked: &BTreeSet<PathBuf>) -> Result<String> {
        let kept = self.keep_files(paths, untracked)?;
        self.remove_file_changes(paths, untracked)?;
        Ok(kept)
    }

    pub(crate) fn keep_files(&self, paths: &[PathBuf], untracked: &BTreeSet<PathBuf>) -> Result<String> {
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
        self.snapshot(paths)
    }

    pub(crate) fn remove_file_changes(&self, paths: &[PathBuf], untracked: &BTreeSet<PathBuf>) -> Result<()> {
        const CLEANED_AT_ONCE: usize = 64;
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

    #[cfg(test)]
    pub fn commit(&self, message: &str) -> std::result::Result<Committed, Failure> {
        self.commit_with(message, Hooks::Run)
    }

    pub(crate) fn commit_with(&self, message: &str, hooks: Hooks) -> std::result::Result<Committed, Failure> {
        self.ensure_index_writable()?;
        if message.trim().is_empty() {
            return Err("Enter a commit message.".to_owned().into());
        }
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true", "commit"]).args(hooks.flag()).args(["-m", message]);
        self.committing(&mut command, &hooks.events(crate::hooks::COMMIT), false)
    }

    fn tip(&self) -> Option<Vec<u8>> {
        let output = self.read_command().args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"]).run().ok()?;
        output.status.success().then(|| without_final_newline(&output.stdout).to_vec())
    }

    fn tip_oid(&self) -> Option<String> {
        self.tip().map(|tip| String::from_utf8_lossy(&tip).into_owned())
    }

    fn committing(&self, command: &mut Command, events: &[&str], staged: bool) -> std::result::Result<Committed, Failure> {
        let link = crate::jobs::Link::current();
        let present = self.runnable_hooks(events);
        let before = self.tip();
        if let Some(link) = link.as_ref().filter(|_| !present.is_empty()) {
            link.expect_hooks(&present);
            link.release();
        }
        let (result, ran) = run_watching_hooks(command);
        if let Some(link) = &link {
            link.settle();
        }
        let moved = self.tip() != before;
        let output = match result {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted && !moved => {
                return Err(crate::hooks::cancelled(staged));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                let stopped = ran.as_deref().and_then(|ran| ran.iter().rev().find(|ran| ran.code.is_none())).map(|ran| ran.hook.clone());
                let hooks = crate::hooks::hooks_that_ran(ran.as_deref(), &present);
                return Ok(Committed { stopped: stopped.or_else(|| hooks.last().cloned()), hooks, head: self.tip_oid(), ..Committed::default() });
            }
            Err(error) => return Err(format!("Run bundled Git: {error}").into()),
            Ok(output) => output,
        };
        let said = String::from_utf8_lossy(&output.stderr).into_owned();
        let skipped = crate::explain::skipped_hooks(&said)
            .into_iter()
            .map(|(_, path)| self.root.join(path))
            .collect();
        if !output.status.success() {
            if !moved
                && let Some(failure) = crate::hooks::stopped(&crate::hooks::COMMITTING, ran.as_deref(), &present, &said)
            {
                return Err(failure);
            }
            return Err(git_error(&output.stderr, output.status).into());
        }
        let hooks = crate::hooks::hooks_that_ran(ran.as_deref(), &present);
        let printed = if hooks.is_empty() { String::new() } else { crate::hooks::printed(&said) };
        Ok(Committed { skipped, hooks, printed, stopped: None, head: self.tip_oid() })
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

    #[cfg(test)]
    pub fn amend(&self, oid: &str, reference: Option<&OsStr>, message: Option<&str>) -> std::result::Result<Committed, Failure> {
        self.amend_with(oid, reference, message, Hooks::Run)
    }

    pub(crate) fn amend_with(
        &self,
        oid: &str,
        reference: Option<&OsStr>,
        message: Option<&str>,
        hooks: Hooks,
    ) -> std::result::Result<Committed, Failure> {
        self.ensure_index_writable()?;
        validate_oid(oid)?;
        if message.is_some_and(|message| message.trim().is_empty()) {
            return Err("Enter a commit message.".to_owned().into());
        }
        let same_branch = self.head_reference()?.as_deref() == reference;
        let head = checked(self.read_command().args(["rev-parse", "--verify", "HEAD"]))?;
        if !same_branch || without_final_newline(&head) != oid.as_bytes() {
            return Err("The checked-out branch or its tip changed. Reopen Amend last commit.".to_owned().into());
        }
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true", "commit", "--amend", "--allow-empty"]).args(hooks.flag());
        if let Some(message) = message {
            command.args(["-m", message]);
        } else {
            command.arg("--no-edit");
        }
        self.committing(&mut command, &hooks.events(crate::hooks::AMEND), false)
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

    #[cfg(test)]
    pub fn commit_all(&self, head: Option<&str>, reference: Option<&OsStr>, message: &str) -> std::result::Result<Committed, Failure> {
        self.commit_all_with(head, reference, message, Hooks::Run)
    }

    pub(crate) fn commit_all_with(
        &self,
        head: Option<&str>,
        reference: Option<&OsStr>,
        message: &str,
        hooks: Hooks,
    ) -> std::result::Result<Committed, Failure> {
        self.ensure_index_writable()?;
        if message.trim().is_empty() {
            return Err("Enter a commit message.".to_owned().into());
        }
        if let Some(oid) = head {
            validate_oid(oid)?;
        }
        let tip = self.tip();
        if self.head_reference()?.as_deref() != reference || tip.as_deref() != head.map(str::as_bytes) {
            return Err("The checked-out branch or its tip changed. Look again before committing.".to_owned().into());
        }
        checked(self.command().args(["add", "--all", "--", "."]))?;
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true", "commit"]).args(hooks.flag()).args(["-m", message]);
        self.committing(&mut command, &hooks.events(crate::hooks::COMMIT), true)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Hooks {
    #[default]
    Run,
    Skip,
}

impl Hooks {
    fn flag(self) -> &'static [&'static str] {
        match self {
            Hooks::Run => &[],
            Hooks::Skip => &["--no-verify"],
        }
    }

    pub(crate) fn events(self, events: &'static [&'static str]) -> Vec<&'static str> {
        events
            .iter()
            .copied()
            .filter(|event| self == Hooks::Run || !crate::hooks::SKIPPED_BY_NO_VERIFY.contains(event))
            .collect()
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

pub(crate) fn with_origins(files: &[ChangedFile], paths: &[PathBuf]) -> Vec<PathBuf> {
    let chosen: BTreeSet<&PathBuf> = paths.iter().collect();
    let mut all = paths.to_vec();
    let mut added = BTreeSet::new();
    for file in files {
        if file.status == RENAMED
            && chosen.contains(&file.path)
            && let Some(from) = &file.from
            && !chosen.contains(from)
            && added.insert(from.clone())
        {
            all.push(from.clone());
        }
    }
    all
}

pub(crate) fn nul_separated(paths: &[PathBuf], prefix: &[u8]) -> Result<Vec<u8>> {
    if paths.is_empty() {
        return Err("Select at least one file.".into());
    }
    let mut input = Vec::new();
    for path in paths {
        validate_path(path)?;
        input.extend_from_slice(prefix);
        input.extend_from_slice(path.as_os_str().as_encoded_bytes());
        input.push(0);
    }
    Ok(input)
}

impl Repository {
    pub(crate) fn commit_sizes(&self, ids: &[String]) -> Result<String> {
        let mut input = Vec::new();
        for id in ids {
            validate_oid(id)?;
            input.extend_from_slice(id.as_bytes());
            input.push(b'\n');
        }
        let mut command = self.read_command();
        command.args(["diff-tree", "--stdin", "--shortstat", "--root", "--always", "--diff-merges=first-parent", "--no-color"]);
        let output = run_with_input(&mut command, input).map_err(|e| format!("Run bundled Git: {e}"))?;
        Ok(String::from_utf8_lossy(&successful(output)?).into_owned())
    }
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
            let code = code.strip_prefix(b"\n").unwrap_or(code);
            let status = changed_status(code).ok_or_else(|| {
                format!("Unknown file status {}", String::from_utf8_lossy(code))
            })?;
            let mut path = || records.next().map(raw_path).ok_or("Missing changed path");
            let from = if paired_status(code) { Some(path()?) } else { None };
            files.push(ChangedFile { status, path: path()?, from });
        }
    }
    Ok(files)
}

pub(crate) fn changed_status(code: &[u8]) -> Option<&'static str> {
    let scored = |rest: &[u8]| rest.iter().all(u8::is_ascii_digit);
    Some(match code {
        b"A" => "Added",
        b"D" => "Deleted",
        b"M" => "Modified",
        b"T" => "Type changed",
        [b'R', rest @ ..] if scored(rest) => RENAMED,
        [b'C', rest @ ..] if scored(rest) => "Copied",
        _ => return None,
    })
}

pub(crate) fn paired_status(code: &[u8]) -> bool {
    matches!(code.first(), Some(b'R' | b'C'))
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
    let link = crate::jobs::Link::current();
    let trace = link
        .as_ref()
        .filter(|_| crate::hooks::may_run(command))
        .and_then(|link| crate::hooks::Trace::attach(command, Some(link)));
    let result = execute(command, link.as_ref());
    if let Some(trace) = trace {
        trace.finish();
    }
    result
}

pub(crate) fn run_watching_hooks(command: &mut Command) -> (std::io::Result<Output>, Option<Vec<crate::hooks::Ran>>) {
    let link = crate::jobs::Link::current();
    let trace = crate::hooks::Trace::attach(command, link.as_ref());
    let result = execute(command, link.as_ref());
    (result, trace.map(crate::hooks::Trace::finish))
}

fn execute(command: &mut Command, link: Option<&crate::jobs::Link>) -> std::io::Result<Output> {
    let (started, at) = (std::time::Instant::now(), std::time::SystemTime::now());
    let result = match link {
        Some(link) => stream(command, link),
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
            out_link.printed(&buffer[..n]);
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
pub(crate) fn git_error(stderr: &[u8], status: std::process::ExitStatus) -> String {
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
        if let Some(end) = bytes.iter().rposition(|&b| b == b'\n') {
            bytes.truncate(end + 1);
        }
        return Ok(Diff {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            hunks: vec![],
            truncated: true,
            source: None,
            staged: false,
            bytes: vec![],
            prefix: 0..0,
            options: DiffOptions::default(),
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
        truncated: false,
        source,
        staged: false,
        bytes,
        prefix: 0..prefix_end.unwrap_or(0),
        options: DiffOptions::default(),
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
                status: "Untracked",
                from: None
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
                status: "Modified",
                from: None
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
        let error = f.repo.commit_all(Some(&shown), Some(&main), "Too late").unwrap_err().text;
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
            f.repo.commit("Ordinary commit").map(drop).map_err(|failure| failure.text),
            f.repo.commit_all(None, None, "Ordinary commit").map(drop).map_err(|failure| failure.text),
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
    fn blobs_reads_each_blob_whole_and_leaves_out_missing_ones_and_other_objects() {
        let f = Fixture::new();
        fs::write(f.dir.join("one.txt"), "first\nline two\n").unwrap();
        fs::write(f.dir.join("two.bin"), b"\0\n\nbytes\n\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "both"]);
        let id = |spec: &str| String::from_utf8(f.git(&["rev-parse", spec])).unwrap().trim().to_owned();
        let (one, two, tree) = (id("HEAD:one.txt"), id("HEAD:two.bin"), id("HEAD^{tree}"));
        let missing = "0123456789abcdef0123456789abcdef01234567".to_owned();
        let index = f.git(&["ls-files", "-s"]);
        let blobs = f.repo.blobs(&[one.clone(), missing.clone(), two.clone(), tree.clone()]).unwrap();
        assert_eq!(blobs.get(&one).map(Vec::as_slice), Some(&b"first\nline two\n"[..]));
        assert_eq!(blobs.get(&two).map(Vec::as_slice), Some(&b"\0\n\nbytes\n\n"[..]));
        assert!(!blobs.contains_key(&missing) && !blobs.contains_key(&tree));
        assert!(f.repo.blobs(&[]).unwrap().is_empty());
        assert!(f.repo.blobs(&["--batch-all-objects".into()]).is_err());
        assert_eq!(f.git(&["ls-files", "-s"]), index);
        assert_eq!(fs::read_to_string(f.dir.join("one.txt")).unwrap(), "first\nline two\n");
    }

    #[test]
    fn object_sizes_reads_each_blob_and_leaves_out_missing_ones() {
        let f = Fixture::new();
        fs::write(f.dir.join("chart.bin"), [0u8, 1, 2, 3, 4]).unwrap();
        f.repo.stage_file(Path::new("chart.bin")).unwrap();
        let staged = String::from_utf8(f.git(&["rev-parse", ":chart.bin"])).unwrap().trim().to_owned();
        let missing = "1".repeat(40);
        let sizes = f.repo.object_sizes(&[staged.clone(), missing.clone()]).unwrap();
        assert_eq!(sizes.get(&staged), Some(&5));
        assert!(!sizes.contains_key(&missing));
        assert!(f.repo.object_sizes(&[]).unwrap().is_empty());
        assert!(f.repo.object_sizes(&["--batch".into()]).is_err());
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"A  chart.bin\n");
    }

    #[test]
    fn a_hunk_copied_as_a_patch_carries_its_own_file_header_and_applies_in_reverse() {
        let f = Fixture::new();
        let lines = |edit: &dyn Fn(usize) -> String| (1..=20).map(edit).collect::<String>();
        fs::write(f.dir.join("log.txt"), lines(&|i| format!("line {i}\n"))).unwrap();
        fs::write(f.dir.join("map.txt"), "north\nsouth\n").unwrap();
        f.git(&["add", "log.txt", "map.txt"]);
        f.git(&["commit", "-qm", "Start the log"]);
        fs::write(f.dir.join("log.txt"), lines(&|i| if i == 2 || i == 19 { format!("LINE {i}\n") } else { format!("line {i}\n") })).unwrap();
        fs::write(f.dir.join("map.txt"), "north\nwest\nsouth\n").unwrap();
        f.git(&["commit", "-qam", "Edit the log and the map"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let diff = f.repo.commit_diff(String::from_utf8_lossy(&head).trim()).unwrap();
        assert_eq!(diff.hunks.len(), 3);
        let second = diff.hunk_patch(1).unwrap();
        assert!(second.starts_with("diff --git a/log.txt b/log.txt\n"), "{second}");
        assert!(second.contains("+LINE 19\n") && !second.contains("LINE 2\n") && !second.contains("map.txt"), "{second}");
        let map = diff.hunk_patch(2).unwrap();
        assert!(map.starts_with("diff --git a/map.txt b/map.txt\n") && map.ends_with("+west\n south\n"), "{map}");
        assert_eq!(map.matches("@@ ").count(), 1);
        assert!(diff.hunk_patch(3).is_none());
        let index = f.git(&["ls-files", "--stage"]);
        for (name, patch) in [("second.patch", &second), ("map.patch", &map)] {
            let file = f.dir.join(name);
            fs::write(&file, patch).unwrap();
            f.git(&["apply", "--check", "--reverse", file.to_str().unwrap()]);
            fs::remove_file(file).unwrap();
        }
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        let file = f.repo.file_diff(Path::new("log.txt"), false).unwrap();
        assert!(file.hunk_patch(0).is_none());
    }

    #[test]
    fn oversized_untracked_preview_is_bounded_and_read_only() {
        let f = Fixture::new();
        let name = Path::new("large.txt");
        fs::write(f.dir.join(name), "line\n".repeat(MAX_DIFF_BYTES / 4)).unwrap();
        let diff = f.repo.file_diff(name, false).unwrap();
        assert!(diff.truncated);
        assert!(diff.text.len() <= MAX_DIFF_BYTES && diff.text.ends_with("line\n"));
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

    const TRAIL: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    fn renamed(f: &Fixture, from: &str, to: &str) {
        fs::write(f.dir.join(from), TRAIL).unwrap();
        fs::write(f.dir.join("other.txt"), "other\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        if let Some(folder) = Path::new(to).parent() {
            fs::create_dir_all(f.dir.join(folder)).unwrap();
        }
        f.git(&["mv", "--", from, to]);
        fs::write(f.dir.join(to), TRAIL.replace("four", "FOUR")).unwrap();
        f.git(&["add", "--", to]);
    }

    fn changed_lines(diff: &Diff) -> Vec<&str> {
        diff.text
            .lines()
            .filter(|line| (line.starts_with('+') || line.starts_with('-')) && !line.starts_with("+++") && !line.starts_with("---"))
            .collect()
    }

    #[test]
    fn a_staged_rename_with_an_edit_is_one_row_and_its_diff_shows_only_the_changed_lines() {
        let f = Fixture::new();
        renamed(&f, "trail map.txt", "maps/trail é.txt");
        let status = f.repo.status().unwrap();
        assert_eq!(
            status.staged,
            [ChangedFile {
                path: "maps/trail é.txt".into(),
                status: RENAMED,
                from: Some("trail map.txt".into()),
            }]
        );
        assert!(status.unstaged.is_empty());
        assert_eq!(status.changed_paths, 1);
        let diff = f.repo.staged_rename_diff(Path::new("trail map.txt"), Path::new("maps/trail é.txt")).unwrap();
        assert_eq!(changed_lines(&diff), ["-four", "+FOUR"]);
        assert!(diff.text.contains("rename from trail map.txt"));
        assert!(!diff.stageable());
        assert!(f.repo.file_diff(Path::new("maps/trail é.txt"), true).unwrap().stageable());

        fs::write(f.dir.join("maps/trail é.txt"), TRAIL.replace("four", "FOUR").replace("nine", "NINE")).unwrap();
        let status = f.repo.status().unwrap();
        assert_eq!(status.staged[0].from.as_deref(), Some(Path::new("trail map.txt")));
        assert_eq!(
            status.unstaged,
            [ChangedFile { path: "maps/trail é.txt".into(), status: "Modified", from: None }]
        );
        assert_eq!(changed_lines(&f.repo.file_diff(Path::new("maps/trail é.txt"), false).unwrap()), ["-nine", "+NINE"]);
    }

    #[test]
    fn an_unstaged_move_stays_a_deletion_and_an_untracked_file() {
        let f = Fixture::new();
        fs::write(f.dir.join("old.txt"), TRAIL).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::rename(f.dir.join("old.txt"), f.dir.join("new.txt")).unwrap();
        let status = f.repo.status().unwrap();
        assert!(status.staged.is_empty());
        assert_eq!(
            status.unstaged,
            [
                ChangedFile { path: "old.txt".into(), status: "Deleted", from: None },
                ChangedFile { path: "new.txt".into(), status: "Untracked", from: None },
            ]
        );
        f.repo.stage_files(&["new.txt".into(), "old.txt".into()]).unwrap();
        assert_eq!(f.repo.status().unwrap().staged[0].from.as_deref(), Some(Path::new("old.txt")));
    }

    #[test]
    fn unstaging_a_staged_rename_unstages_both_paths_and_leaves_the_files() {
        let f = Fixture::new();
        renamed(&f, "old.txt", "new.txt");
        fs::write(f.dir.join("other.txt"), "other staged\n").unwrap();
        f.git(&["add", "other.txt"]);
        f.repo.unstage_file(Path::new("new.txt")).unwrap();
        assert_eq!(f.git(&["diff", "--cached", "--name-only", "-z"]), b"other.txt\0");
        assert_eq!(f.git(&["ls-files", "-z", "--", "old.txt", "new.txt"]), b"old.txt\0");
        assert!(!f.dir.join("old.txt").exists());
        assert_eq!(fs::read_to_string(f.dir.join("new.txt")).unwrap(), TRAIL.replace("four", "FOUR"));
        let status = f.repo.status().unwrap();
        assert!(status.unstaged.iter().any(|file| file.path == Path::new("old.txt") && file.status == "Deleted"));
        assert!(status.unstaged.iter().any(|file| file.path == Path::new("new.txt") && file.status == "Untracked"));

        f.repo.stage_files(&["old.txt".into(), "new.txt".into()]).unwrap();
        assert!(f.repo.status().unwrap().staged.iter().any(|file| file.status == RENAMED));
        f.repo.unstage_files(&["new.txt".into(), "other.txt".into()]).unwrap();
        assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
        assert_eq!(fs::read_to_string(f.dir.join("other.txt")).unwrap(), "other staged\n");
    }

    #[test]
    fn unstaging_a_rename_unstages_only_its_path_once_git_no_longer_pairs_it_and_is_refused_during_a_merge() {
        let f = Fixture::new();
        renamed(&f, "old.txt", "new.txt");
        fs::write(f.dir.join("old.txt"), "old again\n").unwrap();
        f.git(&["add", "old.txt"]);
        assert!(f.repo.status().unwrap().staged.iter().all(|file| file.from.is_none()));
        f.repo.unstage_file(Path::new("new.txt")).unwrap();
        assert_eq!(f.git(&["diff", "--cached", "--name-only", "-z"]), b"old.txt\0");
        assert_eq!(f.git(&["show", ":old.txt"]), b"old again\n");

        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "base\n").unwrap();
        fs::write(f.dir.join("old.txt"), TRAIL).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["switch", "-q", "-c", "side"]);
        fs::write(f.dir.join("trail.txt"), "side\n").unwrap();
        f.git(&["commit", "-qam", "side"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("trail.txt"), "main\n").unwrap();
        f.git(&["mv", "old.txt", "new.txt"]);
        f.git(&["commit", "-qam", "main"]);
        assert!(!f.repo.command().args(["merge", "-q", "side"]).output().unwrap().status.success());
        f.git(&["mv", "new.txt", "newer.txt"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);
        assert_eq!(f.repo.unstage_file(Path::new("newer.txt")).unwrap_err(), "Finish or abort the merge first.");
        assert_eq!(f.repo.stage_file(Path::new("newer.txt")).unwrap_err(), "Finish or abort the merge first.");
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
    }

    #[test]
    fn staging_an_intent_to_add_rename_stages_the_deletion_with_it() {
        let f = Fixture::new();
        fs::write(f.dir.join("old.txt"), TRAIL).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        fs::rename(f.dir.join("old.txt"), f.dir.join("new.txt")).unwrap();
        f.git(&["add", "--intent-to-add", "new.txt"]);
        let status = f.repo.status().unwrap();
        assert_eq!(
            status.unstaged,
            [ChangedFile { path: "new.txt".into(), status: RENAMED, from: Some("old.txt".into()) }]
        );
        f.repo.stage_file(Path::new("new.txt")).unwrap();
        let status = f.repo.status().unwrap();
        assert!(status.unstaged.is_empty());
        assert_eq!(
            status.staged,
            [ChangedFile { path: "new.txt".into(), status: RENAMED, from: Some("old.txt".into()) }]
        );
    }

    #[test]
    fn hunk_staging_an_ordinary_file_is_unchanged_beside_a_staged_rename() {
        let f = Fixture::new();
        renamed(&f, "old.txt", "new.txt");
        let lines: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        fs::write(f.dir.join("other.txt"), &lines).unwrap();
        f.git(&["add", "other.txt"]);
        f.git(&["commit", "-qm", "rename and lines"]);
        fs::write(f.dir.join("other.txt"), lines.replace("line 2\n", "line two\n").replace("line 28\n", "line 28!\n")).unwrap();
        let diff = f.repo.file_diff(Path::new("other.txt"), false).unwrap();
        assert_eq!(diff.hunks.len(), 2);
        assert!(diff.stageable());
        f.repo.stage_hunk(&diff, 0).unwrap();
        assert_eq!(f.git(&["show", ":other.txt"]), lines.replace("line 2\n", "line two\n").into_bytes());
        let staged = f.repo.status().unwrap().staged;
        assert_eq!(staged, [ChangedFile { path: "other.txt".into(), status: "Modified", from: None }]);
    }

    fn picks(diff: &Diff, wanted: &[&str]) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        for (index, hunk) in diff.hunks.iter().enumerate() {
            let body = diff.bytes[hunk.bytes.clone()].split_inclusive(|&b| b == b'\n').skip(1);
            for (line, text) in body.enumerate() {
                if wanted.iter().any(|want| text.strip_suffix(b"\n").unwrap_or(text) == want.as_bytes()) {
                    found.push((index, line));
                }
            }
        }
        assert_eq!(found.len(), wanted.len(), "{wanted:?} in {}", diff.text);
        found
    }

    fn committed(f: &Fixture, name: &str, text: &[u8]) {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", "base"]);
    }

    #[test]
    fn picking_every_changed_line_of_a_hunk_stages_what_the_hunk_stages() {
        let f = Fixture::new();
        let base: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        committed(&f, "log.txt", base.as_bytes());
        let edited = base.replace("line 3\n", "line three\n").replace("line 17\n", "line 17\nline 17b\n");
        fs::write(f.dir.join("log.txt"), &edited).unwrap();
        let path = Path::new("log.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        assert_eq!(diff.hunks.len(), 2);
        f.repo.stage_hunk(&diff, 0).unwrap();
        let by_hunk = f.git(&["ls-files", "--stage", "--", "log.txt"]);
        f.git(&["reset", "-q", "--", "log.txt"]);
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["-line 3", "+line three"])).unwrap();
        assert_eq!(f.git(&["ls-files", "--stage", "--", "log.txt"]), by_hunk);
        assert_eq!(fs::read_to_string(f.dir.join("log.txt")).unwrap(), edited);
        assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    }

    #[test]
    fn picking_one_of_two_adjacent_additions_stages_exactly_that_one() {
        let f = Fixture::new();
        committed(&f, "trail.txt", b"north\nsouth\n");
        fs::write(f.dir.join("trail.txt"), "north\neast\nwest\nsouth\n").unwrap();
        let path = Path::new("trail.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["+west"])).unwrap();
        assert_eq!(f.git(&["show", ":trail.txt"]), b"north\nwest\nsouth\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "north\neast\nwest\nsouth\n");
        let rest = f.repo.file_diff(path, false).unwrap();
        assert!(rest.text.contains("\n+east\n") && !rest.text.contains("+west"), "{}", rest.text);
    }

    #[test]
    fn picking_a_deletion_without_the_addition_that_replaced_it_removes_only_the_old_line() {
        let f = Fixture::new();
        committed(&f, "gear.txt", b"rope\ntent\nstove\n");
        fs::write(f.dir.join("gear.txt"), "rope\ntarp\nstove\n").unwrap();
        let path = Path::new("gear.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["-tent"])).unwrap();
        assert_eq!(f.git(&["show", ":gear.txt"]), b"rope\nstove\n");
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["+tarp"])).unwrap();
        assert_eq!(f.git(&["show", ":gear.txt"]), b"rope\ntarp\nstove\n");
        assert!(f.repo.file_diff(path, false).unwrap().text.is_empty());
    }

    #[test]
    fn a_pick_that_includes_the_last_line_of_a_file_without_a_final_newline_keeps_the_bytes() {
        let f = Fixture::new();
        committed(&f, "end.txt", b"one\ntwo");
        fs::write(f.dir.join("end.txt"), b"one\ntwo\nthree\nfour").unwrap();
        let path = Path::new("end.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        assert!(diff.text.contains("\\ No newline at end of file"), "{}", diff.text);
        f.repo.stage_lines(&diff, &picks(&diff, &["+four"])).unwrap();
        assert_eq!(f.git(&["show", ":end.txt"]), b"one\ntwo\nfour");
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["+three"])).unwrap();
        assert_eq!(f.git(&["show", ":end.txt"]), b"one\ntwo\nthree\nfour");
        assert_eq!(fs::read(f.dir.join("end.txt")).unwrap(), b"one\ntwo\nthree\nfour");

        let staged = f.repo.file_diff(path, true).unwrap();
        f.repo.unstage_lines(&staged, &picks(&staged, &["+four"])).unwrap();
        assert_eq!(f.git(&["show", ":end.txt"]), b"one\ntwo\nthree\n");
        let staged = f.repo.file_diff(path, true).unwrap();
        f.repo.unstage_lines(&staged, &picks(&staged, &["-two"])).unwrap();
        assert_eq!(f.git(&["show", ":end.txt"]), b"one\ntwo\ntwo\nthree\n");
    }

    #[test]
    fn unstaging_one_line_of_a_fully_staged_hunk_leaves_the_rest_staged() {
        let f = Fixture::new();
        committed(&f, "camp.txt", b"fire\nwater\n");
        fs::write(f.dir.join("camp.txt"), "fire\nwood\nwater\nfood\n").unwrap();
        f.git(&["add", "--", "camp.txt"]);
        let path = Path::new("camp.txt");
        let staged = f.repo.file_diff(path, true).unwrap();
        assert_eq!(staged.hunks.len(), 1);
        assert!(f.repo.stage_lines(&staged, &picks(&staged, &["+wood"])).is_err());
        f.repo.unstage_lines(&staged, &picks(&staged, &["+wood"])).unwrap();
        assert_eq!(f.git(&["show", ":camp.txt"]), b"fire\nwater\nfood\n");
        assert_eq!(fs::read_to_string(f.dir.join("camp.txt")).unwrap(), "fire\nwood\nwater\nfood\n");
        let unstaged = f.repo.file_diff(path, false).unwrap().text;
        let added: Vec<&str> = unstaged.lines().filter(|line| line.starts_with('+') && !line.starts_with("+++")).collect();
        assert_eq!(added, ["+wood"]);
    }

    #[test]
    fn discarding_one_line_leaves_the_other_edits_in_the_working_file() {
        let f = Fixture::new();
        committed(&f, "map.txt", b"hill\nriver\nvalley\n");
        fs::write(f.dir.join("map.txt"), "hill\nridge\nriver\nvale\n").unwrap();
        f.git(&["add", "--", "map.txt"]);
        fs::write(f.dir.join("map.txt"), "hill\nridge\nriver\nvale\nlake\nshore\n").unwrap();
        let index = f.git(&["ls-files", "--stage", "--", "map.txt"]);
        let path = Path::new("map.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        f.repo.discard_lines(&diff, &picks(&diff, &["+lake"])).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("map.txt")).unwrap(), "hill\nridge\nriver\nvale\nshore\n");
        assert_eq!(f.git(&["ls-files", "--stage", "--", "map.txt"]), index);
    }

    #[test]
    fn line_picks_in_new_and_deleted_files_stage_and_return_parts_of_them() {
        let f = Fixture::new();
        committed(&f, "old.txt", b"a\nb\nc\n");
        fs::write(f.dir.join("new.txt"), "x\ny\nz\n").unwrap();
        let new = Path::new("new.txt");
        let diff = f.repo.file_diff(new, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["+x", "+z"])).unwrap();
        assert_eq!(f.git(&["show", ":new.txt"]), b"x\nz\n");
        let staged = f.repo.file_diff(new, true).unwrap();
        f.repo.unstage_lines(&staged, &picks(&staged, &["+z"])).unwrap();
        assert_eq!(f.git(&["show", ":new.txt"]), b"x\n");
        let diff = f.repo.file_diff(new, false).unwrap();
        f.repo.discard_lines(&diff, &picks(&diff, &["+y"])).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("new.txt")).unwrap(), "x\nz\n");

        fs::write(f.dir.join("loose.txt"), "keep\ndrop\n").unwrap();
        let loose = Path::new("loose.txt");
        let diff = f.repo.file_diff(loose, false).unwrap();
        f.repo.discard_lines(&diff, &picks(&diff, &["+drop"])).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join("loose.txt")).unwrap(), "keep\n");
        assert!(f.git(&["ls-files", "--", "loose.txt"]).is_empty());

        fs::remove_file(f.dir.join("old.txt")).unwrap();
        let old = Path::new("old.txt");
        let diff = f.repo.file_diff(old, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["-b"])).unwrap();
        assert_eq!(f.git(&["show", ":old.txt"]), b"a\nc\n");
        assert!(!f.dir.join("old.txt").exists());
        let diff = f.repo.file_diff(old, false).unwrap();
        f.repo.stage_lines(&diff, &picks(&diff, &["-a", "-c"])).unwrap();
        assert!(f.git(&["ls-files", "--", "old.txt"]).is_empty());
        let staged = f.repo.file_diff(old, true).unwrap();
        f.repo.unstage_lines(&staged, &picks(&staged, &["-c"])).unwrap();
        assert_eq!(f.git(&["show", ":old.txt"]), b"c\n");
    }

    #[test]
    fn line_staging_refuses_a_file_that_changed_since_its_diff_was_read() {
        let f = Fixture::new();
        committed(&f, "note.txt", b"first\nsecond\n");
        fs::write(f.dir.join("note.txt"), "first\nadded\nsecond\n").unwrap();
        let path = Path::new("note.txt");
        let diff = f.repo.file_diff(path, false).unwrap();
        let lines = picks(&diff, &["+added"]);
        let index = f.git(&["ls-files", "--stage", "--", "note.txt"]);
        fs::write(f.dir.join("note.txt"), "first\nadded\nsecond\nlater\n").unwrap();
        let err = f.repo.stage_lines(&diff, &lines).unwrap_err();
        assert!(err.contains("changed since its diff was opened") && err.contains("the lines"), "{err}");
        let err = f.repo.discard_lines(&diff, &lines).unwrap_err();
        assert!(err.contains("changed since its diff was opened"), "{err}");
        assert_eq!(f.git(&["ls-files", "--stage", "--", "note.txt"]), index);
        assert_eq!(fs::read_to_string(f.dir.join("note.txt")).unwrap(), "first\nadded\nsecond\nlater\n");
        let err = f.repo.unstage_lines(&diff, &lines).unwrap_err();
        assert!(err.contains("Select lines from a staged file"), "{err}");
        let other = Fixture::new();
        committed(&other, "note.txt", b"first\nsecond\n");
        let err = other.repo.stage_lines(&diff, &lines).unwrap_err();
        assert!(err.contains("different repository"), "{err}");
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap();
        let committed_diff = f.repo.commit_diff(head.trim()).unwrap();
        assert!(f.repo.stage_lines(&committed_diff, &[(0, 0)]).is_err());
        assert_eq!(f.git(&["ls-files", "--stage", "--", "note.txt"]), index);
    }

    const WHOLE: DiffOptions = DiffOptions { whole_file: true, ignore_whitespace: false };
    const SPACES: DiffOptions = DiffOptions { whole_file: false, ignore_whitespace: true };

    #[test]
    fn a_whole_file_diff_is_one_hunk_holding_every_line_and_its_lines_still_stage() {
        let f = Fixture::new();
        let base: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        committed(&f, "log.txt", base.as_bytes());
        let edited = base.replace("line 5\n", "line five\n").replace("line 35\n", "line 35\nline 35b\n");
        fs::write(f.dir.join("log.txt"), &edited).unwrap();
        let path = Path::new("log.txt");
        assert_eq!(f.repo.file_diff(path, false).unwrap().hunks.len(), 2);
        let whole = f.repo.file_diff_with(path, false, WHOLE).unwrap();
        assert_eq!(whole.hunks.len(), 1);
        assert_eq!(whole.options, WHOLE);
        assert!(whole.stageable());
        for n in 1..=40 {
            let shown = whole.text.lines().any(|line| line.get(1..) == Some(&format!("line {n}")) || (n == 5 && line == "+line five"));
            assert!(shown, "line {n} is missing from {}", whole.text);
        }
        f.repo.stage_lines(&whole, &picks(&whole, &["+line 35b"])).unwrap();
        let expected = base.replace("line 35\n", "line 35\nline 35b\n");
        assert_eq!(f.git(&["show", ":log.txt"]), expected.as_bytes());
        assert_eq!(fs::read_to_string(f.dir.join("log.txt")).unwrap(), edited);
        let staged = f.repo.file_diff_with(path, true, WHOLE).unwrap();
        assert_eq!(staged.hunks.len(), 1);
        f.repo.unstage_lines(&staged, &picks(&staged, &["+line 35b"])).unwrap();
        assert_eq!(f.git(&["show", ":log.txt"]), base.as_bytes());
        let whole = f.repo.file_diff_with(path, false, WHOLE).unwrap();
        f.repo.stage_hunk(&whole, 0).unwrap();
        assert_eq!(f.git(&["show", ":log.txt"]), edited.as_bytes());
        assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), b"1\n");
    }

    #[test]
    fn a_whole_file_pick_is_refused_once_the_file_changes() {
        let f = Fixture::new();
        committed(&f, "note.txt", b"first\nsecond\nthird\n");
        fs::write(f.dir.join("note.txt"), "first\nadded\nsecond\nthird\n").unwrap();
        let path = Path::new("note.txt");
        let whole = f.repo.file_diff_with(path, false, WHOLE).unwrap();
        let lines = picks(&whole, &["+added"]);
        let index = f.git(&["ls-files", "--stage", "--", "note.txt"]);
        fs::write(f.dir.join("note.txt"), "first\nadded\nsecond\nthird changed\n").unwrap();
        let err = f.repo.stage_lines(&whole, &lines).unwrap_err();
        assert!(err.contains("changed since its diff was opened"), "{err}");
        assert_eq!(f.git(&["ls-files", "--stage", "--", "note.txt"]), index);
        assert_eq!(fs::read_to_string(f.dir.join("note.txt")).unwrap(), "first\nadded\nsecond\nthird changed\n");
    }

    #[test]
    fn ignoring_whitespace_leaves_reindented_lines_unmarked_and_refuses_to_stage() {
        let f = Fixture::new();
        committed(&f, "code.rs", b"fn main() {\n  let a = 1;\n  let b = 2;\n}\n");
        let edited = "fn main() {\n    let a = 1;\n    let b = 3;\n}\n";
        fs::write(f.dir.join("code.rs"), edited).unwrap();
        let path = Path::new("code.rs");
        let plain = f.repo.file_diff(path, false).unwrap();
        assert_eq!(changed_lines(&plain), ["-  let a = 1;", "-  let b = 2;", "+    let a = 1;", "+    let b = 3;"]);
        let quiet = f.repo.file_diff_with(path, false, SPACES).unwrap();
        assert_eq!(changed_lines(&quiet), ["-  let b = 2;", "+    let b = 3;"]);
        assert!(!quiet.stageable() && !quiet.hunks_stageable());
        let index = f.git(&["ls-files", "--stage", "--", "code.rs"]);
        let err = f.repo.stage_hunk(&quiet, 0).unwrap_err();
        assert!(err.contains("Whitespace changes are hidden"), "{err}");
        let err = f.repo.stage_lines(&quiet, &picks(&quiet, &["+    let b = 3;"])).unwrap_err();
        assert!(err.contains("Whitespace changes are hidden"), "{err}");
        let err = f.repo.discard_hunk(&quiet, 0).unwrap_err();
        assert!(err.contains("Whitespace changes are hidden"), "{err}");
        assert_eq!(f.git(&["ls-files", "--stage", "--", "code.rs"]), index);
        assert_eq!(fs::read_to_string(f.dir.join("code.rs")).unwrap(), edited);
        fs::write(f.dir.join("code.rs"), "fn main() {\n    let a = 1;\n    let b = 2;\n}\n").unwrap();
        let only_spaces = f.repo.file_diff_with(path, false, SPACES).unwrap();
        assert!(only_spaces.hunks.is_empty() && only_spaces.text.is_empty(), "{}", only_spaces.text);
        assert!(!f.repo.file_diff(path, false).unwrap().hunks.is_empty());
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap();
        let commit = f.repo.commit_diff_with(head.trim(), SPACES).unwrap();
        assert_eq!(commit.options, SPACES);
    }

    #[test]
    fn a_commit_file_and_a_staged_rename_are_read_with_the_shown_options() {
        let f = Fixture::new();
        let base: String = (1..=30).map(|n| format!("  row {n}\n")).collect();
        committed(&f, "deck.txt", base.as_bytes());
        let edited = base.replace("  row ", "    row ").replace("    row 20\n", "    row twenty\n");
        fs::write(f.dir.join("deck.txt"), &edited).unwrap();
        f.git(&["commit", "-qam", "edit"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap();
        let (head, path) = (head.trim(), Path::new("deck.txt"));
        let shows = |diff: &Diff, row: &str| diff.text.lines().any(|line| line.ends_with(row));
        let plain = f.repo.commit_path_diff(head, path, None).unwrap();
        assert_eq!(changed_lines(&plain).len(), 60);
        let quiet = f.repo.commit_path_diff_with(head, path, None, SPACES).unwrap();
        assert_eq!(changed_lines(&quiet), ["-  row 20", "+    row twenty"]);
        assert!(!shows(&quiet, "row 1") && !shows(&quiet, "row 30"));
        assert_eq!(quiet.options, SPACES);
        let both = DiffOptions { whole_file: true, ignore_whitespace: true };
        let whole = f.repo.commit_path_diff_with(head, path, None, both).unwrap();
        assert_eq!(changed_lines(&whole), ["-  row 20", "+    row twenty"]);
        assert_eq!(whole.hunks.len(), 1);
        assert!(shows(&whole, "row 1") && shows(&whole, "row 30"));
        assert_eq!(whole.options, both);

        let f = Fixture::new();
        renamed(&f, "trail map.txt", "maps/trail é.txt");
        let (from, to) = (Path::new("trail map.txt"), Path::new("maps/trail é.txt"));
        let plain = f.repo.staged_rename_diff(from, to).unwrap();
        assert!(!plain.text.lines().any(|line| line == " ten"));
        let whole = f.repo.staged_rename_diff_with(from, to, WHOLE).unwrap();
        assert_eq!(changed_lines(&whole), ["-four", "+FOUR"]);
        assert!(whole.text.lines().any(|line| line == " ten") && whole.text.contains("rename from trail map.txt"));
        assert_eq!((whole.hunks.len(), whole.options), (1, WHOLE));
        assert!(!whole.stageable());
    }

    #[test]
    fn a_staged_copy_keeps_its_own_stageable_diff() {
        let f = Fixture::new();
        fs::write(f.dir.join("source.txt"), TRAIL).unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["config", "status.renames", "copies"]);
        fs::write(f.dir.join("copy.txt"), TRAIL).unwrap();
        fs::write(f.dir.join("source.txt"), TRAIL.replace("ten", "TEN")).unwrap();
        f.git(&["add", "."]);
        let status = f.repo.status().unwrap();
        assert!(status.staged.contains(&ChangedFile {
            path: "copy.txt".into(),
            status: "Copied",
            from: Some("source.txt".into()),
        }));
        let diff = f.repo.file_diff(Path::new("copy.txt"), true).unwrap();
        assert!(diff.stageable());
        assert!(!diff.text.contains("source.txt") && !diff.text.contains("+TEN"));
        f.repo.unstage_file(Path::new("copy.txt")).unwrap();
        assert_eq!(f.git(&["diff", "--cached", "--name-only", "-z"]), b"source.txt\0");
    }

    #[test]
    fn changed_file_lists_read_renames_and_copies_with_both_paths() {
        let files = parse_changed_files(b"R085\0old name.txt\0new name.txt\0M\0kept.txt\0C100\0a.txt\0b.txt\0").unwrap();
        assert_eq!(
            files,
            [
                ChangedFile { path: "new name.txt".into(), status: RENAMED, from: Some("old name.txt".into()) },
                ChangedFile { path: "kept.txt".into(), status: "Modified", from: None },
                ChangedFile { path: "b.txt".into(), status: "Copied", from: Some("a.txt".into()) },
            ]
        );
        assert!(parse_changed_files(b"R085\0only.txt\0").is_err());
        assert!(parse_changed_files(b"Rx\0a\0b\0").is_err());
    }
}
