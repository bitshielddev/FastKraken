use super::{Mode, Source};
use crate::repository::Run;
use crate::{
    guard::Seen,
    operation::{Id, Kind, current_id, snapshot},
    repository::{Repository, Revisions, checked, validate_oid, validate_path},
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    ops::Range,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

type Result<T> = std::result::Result<T, String>;
pub(crate) const LIMIT: usize = 1024 * 1024;

fn revision(repo: &Repository, name: &OsStr) -> Result<String> {
    let bytes = checked(
        repo.read_command()
            .args(["rev-parse", "--verify", "--end-of-options"])
            .arg(name),
    )?;
    let id = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let id = id.trim_end();
    validate_oid(id)?;
    Ok(id.to_owned())
}

fn entries(repo: &Repository, mode: &str, path: Option<&Path>) -> Result<Vec<u8>> {
    checked(
        repo.read_command()
            .args(["ls-files", mode, "-z", "--"])
            .args(path),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Merged {
    Nothing,
    FastForward(usize),
    Commit(String),
    Squashed(usize),
}

impl Merged {
    pub(crate) fn between(repo: &Repository, before: &str, after: &str) -> Result<Self> {
        validate_oid(before)?;
        validate_oid(after)?;
        if before == after {
            return Ok(Self::Nothing);
        }
        let parents = checked(
            repo.read_command()
                .args(["rev-list", "--parents", "--max-count=1", after, "--"]),
        )?;
        let parents = String::from_utf8_lossy(&parents);
        let mut parents = parents.split_whitespace().skip(1);
        if parents.next() == Some(before) && parents.next().is_some() {
            return Ok(Self::Commit(after.to_owned()));
        }
        Ok(Self::FastForward(repo.count(&Revisions::reachable(after).not(before))?))
    }

    pub(crate) fn sentence(&self, current: &str, source: &str) -> String {
        match self {
            Self::Nothing => format!("{current} already contains {source}."),
            Self::FastForward(count) => {
                format!("Fast-forwarded {current} by {}.", crate::notice::commits(*count))
            }
            Self::Commit(oid) => format!("Created merge commit {}.", crate::notice::short(oid)),
            Self::Squashed(count) => format!(
                "The changes from {} are staged on {current}, with a message in the commit box. Nothing is committed until you commit it.",
                crate::notice::commits(*count)
            ),
        }
    }
}

fn merge_name(repo: &Repository, full: &OsStr) -> Result<OsString> {
    let bytes = full.as_bytes();
    let Some(short) = ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|prefix| bytes.strip_prefix(prefix.as_bytes()))
    else {
        return Err("Select a branch or tag by its full ref name.".into());
    };
    let short = OsStr::from_bytes(short);
    let resolved = repo
        .read_command()
        .args(["rev-parse", "--verify", "--quiet", "--symbolic-full-name", "--end-of-options"])
        .arg(short)
        .run()
        .map_err(|e| e.to_string())?;
    let resolved = resolved.stdout.strip_suffix(b"\n").unwrap_or(&resolved.stdout);
    Ok(if resolved == bytes { short.to_owned() } else { full.to_owned() })
}

fn mode_args(mode: Mode) -> &'static [&'static str] {
    match mode {
        Mode::FastForwardIfPossible => &["--no-edit", "--commit", "--no-squash", "--ff"],
        Mode::NoFastForward => &["--no-edit", "--commit", "--no-squash", "--no-ff"],
        Mode::FastForwardOnly => &["--no-edit", "--commit", "--no-squash", "--ff-only"],
        Mode::Squash => &["--squash"],
    }
}

pub(crate) const NO_FAST_FORWARD: &str = "Cannot fast-forward";

pub(crate) fn squash_message(repo: &Repository) -> String {
    fs::read(repo.git_dir().join("SQUASH_MSG")).map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_owned()).unwrap_or_default()
}

pub(super) fn start(
    repo: &Repository,
    source: &Source,
    expected: &str,
    seen: &Seen,
    mode: Mode,
    autostash: bool,
) -> Result<Merged> {
    validate_oid(expected)?;
    let attached = seen
        .head_ref
        .as_deref()
        .is_some_and(|name| name.as_bytes().starts_with(b"refs/heads/"));
    let Some(before) = seen.head.as_deref().filter(|_| attached) else {
        return Err("Check out a local branch before merging.".into());
    };
    repo.expect_no_operation()?;
    repo.expect_checkout(seen)?;
    let name = match source {
        Source::Commit(oid) => {
            validate_oid(oid)?;
            let mut commit = OsString::from(oid);
            commit.push("^{commit}");
            if oid != expected || repo.resolve(&commit).ok().as_deref() != Some(expected) {
                return Err("This commit is not in the repository. Refresh and select it again.".into());
            }
            OsString::from(oid)
        }
        Source::Tag(_) => {
            let full = source.full();
            repo.expect_names_commit(&full, expected)?;
            merge_name(repo, &full)?
        }
        Source::Branch(_) | Source::RemoteBranch(_) => {
            let full = source.full();
            repo.expect_ref(&full, expected)?;
            merge_name(repo, &full)?
        }
    };
    if mode == Mode::FastForwardOnly && !repo.is_ancestor(expected, before)? && !repo.is_ancestor(before, expected)? {
        let branch = seen.head_ref.as_deref().map(crate::guard::display).unwrap_or_default();
        let own = repo.count(&Revisions::reachable(before).not(expected))?;
        let is = if own == 1 { "is" } else { "are" };
        return Err(format!(
            "{NO_FAST_FORWARD} {branch} to {} — {} on {branch} {is} not in {}, so it cannot simply move forward. Nothing was changed.",
            source.label(),
            crate::notice::commits(own),
            source.label(),
        ));
    }
    let output = repo
        .command()
        .arg("merge")
        .args(mode_args(mode))
        .args([if autostash { "--autostash" } else { "--no-autostash" }, "--"])
        .arg(name)
        .run()
        .map_err(|e| e.to_string())?;
    if output.status.success() && mode == Mode::Squash {
        let count = repo.count(&Revisions::reachable(expected).not(before))?;
        if count == 0 || crate::operation::staged_nothing(repo)? {
            return Ok(Merged::Nothing);
        }
        return Ok(Merged::Squashed(count));
    }
    if output.status.success() {
        let after = revision(repo, OsStr::new("HEAD"))?;
        return Merged::between(repo, before, &after);
    }
    if snapshot(repo)?.is_some_and(|s| s.kind == Kind::Merge && !s.conflicts.is_empty()) {
        return Err("Merge conflict. Open a conflicted file or abort the merge.".into());
    }
    if mode == Mode::Squash && snapshot(repo)?.is_some_and(|s| s.kind == Kind::Applied && !s.conflicts.is_empty()) {
        if crate::autostash::pending(repo).is_some() {
            return Err(format!("{SQUASH_CONFLICT}\n{SQUASH_STASHED}"));
        }
        return Err(SQUASH_CONFLICT.into());
    }
    let message = [output.stderr, output.stdout].concat();
    Err(String::from_utf8_lossy(&message).trim().to_owned())
}

pub(crate) const SQUASH_CONFLICT: &str =
    "The squash merge stopped on a conflict. Resolve the files; the result stays staged, not committed.";
const SQUASH_STASHED: &str =
    "Your local changes were stashed first. They are put back when you commit the result or abort the squash merge.";

fn regular_path(repo: &Repository, path: &Path) -> Result<PathBuf> {
    validate_path(path)?;
    let absolute = repo.root.join(path);
    let parent = absolute.parent().ok_or("Missing parent directory")?;
    if fs::canonicalize(parent).map_err(|e| e.to_string())? != parent
        || !fs::symlink_metadata(&absolute)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_file()
    {
        return Err("The line resolver opens only regular files.".into());
    }
    Ok(absolute)
}

fn file_bytes(file: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(file)
        .map_err(|e| e.to_string())?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err(
            "The file is larger than the 1 MiB the line resolver opens.".into(),
        );
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Current,
    Incoming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    One(Side),
    Both,
}

impl Choice {
    pub fn includes(self, side: Side) -> bool {
        self == Self::Both || self == Self::One(side)
    }
}

#[derive(Clone, Debug)]
enum Piece {
    Shared(Range<usize>),
    Hunk(usize),
}

#[derive(Debug)]
struct Hunk {
    current: Range<usize>,
    incoming: Range<usize>,
}

#[derive(Debug)]
pub(super) struct LinePair {
    pub current: Option<(usize, Range<usize>)>,
    pub incoming: Option<(usize, Range<usize>)>,
    pub shared: bool,
}

impl LinePair {
    pub fn on(&self, side: Side) -> Option<&(usize, Range<usize>)> {
        match side {
            Side::Current => self.current.as_ref(),
            Side::Incoming => self.incoming.as_ref(),
        }
    }
}

#[derive(Debug)]
pub(super) enum ResultRow {
    Text { range: Range<usize>, side: Option<Side> },
    Unresolved { hunk: usize, line: usize },
}

#[derive(Debug)]
pub(super) struct Preview {
    pub rows: Vec<ResultRow>,
    pub hunk_starts: Vec<usize>,
}

pub(super) type Choices = Vec<Vec<Option<Choice>>>;

fn source_lines(source: &str, range: Range<usize>) -> impl Iterator<Item = Range<usize>> {
    source[range.clone()].split_inclusive('\n').scan(range.start, |offset, line| {
        let start = *offset;
        *offset += line.len();
        Some(start..*offset)
    })
}

fn matching_lines(source: &str, current: &[Range<usize>], incoming: &[Range<usize>]) -> Vec<(usize, usize)> {
    const MAX_CELLS: usize = 65_536;
    let width = incoming.len() + 1;
    let cells = (current.len() + 1).saturating_mul(width);
    if current.is_empty() || incoming.is_empty() || cells > MAX_CELLS {
        return Vec::new();
    }
    let matches = |i: usize, j: usize| source[current[i].clone()] == source[incoming[j].clone()];
    let mut lengths = vec![0u32; cells];
    for i in (0..current.len()).rev() {
        for j in (0..incoming.len()).rev() {
            lengths[i * width + j] = if matches(i, j) {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }

    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < current.len() && j < incoming.len() {
        if matches(i, j) {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

fn align(source: &str, hunk: Hunk) -> Vec<LinePair> {
    let current = source_lines(source, hunk.current).collect::<Vec<_>>();
    let incoming = source_lines(source, hunk.incoming).collect::<Vec<_>>();
    let matches = matching_lines(source, &current, &incoming);
    let mut rows = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut pair = |current_idx: Option<usize>, incoming_idx: Option<usize>| {
        let current = current_idx.map(|i| (i + 1, current[i].clone()));
        let incoming = incoming_idx.map(|i| (i + 1, incoming[i].clone()));
        let shared = current.as_ref().zip(incoming.as_ref()).is_some_and(|((_, a), (_, b))| {
            source[a.clone()] == source[b.clone()]
        });
        rows.push(LinePair { current, incoming, shared });
    };
    for (a, b) in matches.into_iter().chain([(current.len(), incoming.len())]) {
        while i < a || j < b {
            let current_idx = (i < a).then_some(i);
            let incoming_idx = (j < b).then_some(j);
            pair(current_idx, incoming_idx);
            i += usize::from(current_idx.is_some());
            j += usize::from(incoming_idx.is_some());
        }
        if a < current.len() {
            pair(Some(i), Some(j));
            i += 1;
            j += 1;
        }
    }
    rows
}

#[derive(Debug)]
pub(super) struct Document {
    pub id: Id,
    pub path: PathBuf,
    pub source: String,
    stages: Vec<u8>,
    pieces: Vec<Piece>,
    pub hunks: Vec<Vec<LinePair>>,
}

pub(crate) fn marker_width(repo: &Repository, path: &Path) -> Result<usize> {
    let size_attr = checked(
        repo.read_command()
            .args(["check-attr", "-z", "conflict-marker-size", "--"])
            .arg(path),
    )?;
    Ok(std::str::from_utf8(size_attr.split(|&b| b == 0).nth(2).unwrap_or_default())
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(7))
}

pub(crate) fn marker(line: &[u8], width: usize) -> Option<u8> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let (&ch, _) = line.split_first()?;
    if !matches!(ch, b'<' | b'|' | b'=' | b'>')
        || line.len() < width
        || !line[..width].iter().all(|b| *b == ch)
    {
        return None;
    }
    if (ch == b'=' && line.len() == width) || (ch != b'=' && line.get(width) == Some(&b' ')) {
        Some(ch)
    } else {
        None
    }
}

fn parse(bytes: &[u8], width: usize) -> Result<(Vec<Piece>, Vec<Hunk>)> {
    let malformed =
        "The file has unusual or edited conflict markers.";
    let mut pieces = Vec::new();
    let mut hunks = Vec::new();
    let mut shared = 0;
    let mut offset = 0;
    let mut current = None;
    let mut ancestor = None;
    let mut incoming = None;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let end = offset + line.len();
        match marker(line, width) {
            Some(b'<') if current.is_none() => {
                pieces.push(Piece::Shared(shared..offset));
                current = Some(end);
            }
            Some(b'|') if current.is_some() && ancestor.is_none() && incoming.is_none() => {
                ancestor = Some(offset)
            }
            Some(b'=') if current.is_some() && incoming.is_none() => incoming = Some((offset, end)),
            Some(b'>') if current.is_some() && incoming.is_some() => {
                let (split, start) = incoming.take().unwrap();
                pieces.push(Piece::Hunk(hunks.len()));
                hunks.push(Hunk {
                    current: current.take().unwrap()..ancestor.take().unwrap_or(split),
                    incoming: start..offset,
                });
                shared = end;
            }
            Some(_) => return Err(malformed.into()),
            None => {}
        }
        offset = end;
    }
    if current.is_some() || hunks.is_empty() {
        return Err(malformed.into());
    }
    pieces.push(Piece::Shared(shared..bytes.len()));
    Ok((pieces, hunks))
}

impl Document {
    pub fn open(repo: &Repository, id: Id, path: PathBuf) -> Result<Self> {
        repo.expect_operation(&id)?;
        let absolute = regular_path(repo, &path)?;
        let stages = entries(repo, "--unmerged", Some(&path))?;
        let width = marker_width(repo, &path)?;
        let source = String::from_utf8(file_bytes(&absolute)?)
            .map_err(|_| "The file is not UTF-8 text.")?;
        if source.contains('\0') {
            return Err("The file holds binary data.".into());
        }
        let (pieces, mut hunks) = parse(source.as_bytes(), width)?;
        let ends_in_conflict =
            matches!(pieces.last(), Some(Piece::Shared(range)) if range.is_empty());
        let mut sides = 0;
        for record in stages.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let header = record.split(|&b| b == b'\t').next().unwrap_or_default();
            let fields = header.split(|&b| b == b' ').collect::<Vec<_>>();
            if fields.len() != 3 || !matches!(fields[0], b"100644" | b"100755") {
                return Err("The line resolver opens only regular files.".into());
            }
            if matches!(fields[2], b"2" | b"3") {
                sides += 1;
            }
            let oid = std::str::from_utf8(fields[1]).map_err(|e| e.to_string())?;
            validate_oid(oid)?;
            let size = checked(repo.read_command().args(["cat-file", "-s", oid]))?;
            if String::from_utf8_lossy(&size)
                .trim()
                .parse::<usize>()
                .map_err(|e| e.to_string())?
                > LIMIT
            {
                return Err("A side is larger than the 1 MiB the line resolver opens.".into());
            }
            let version = checked(repo.read_command().args(["cat-file", "blob", oid]))?;
            if ends_in_conflict && !version.ends_with(b"\n") {
                let last = hunks.last_mut().expect("parse returned at least one hunk");
                let range = match fields[2] {
                    b"2" => Some(&mut last.current),
                    b"3" => Some(&mut last.incoming),
                    _ => None,
                };
                if let Some(range) = range {
                    let side = &source.as_bytes()[range.clone()];
                    if side.ends_with(b"\n") {
                        let original_last_line =
                            version.rsplit(|b| *b == b'\n').next().unwrap_or_default();
                        let ending = [b"\n".as_slice(), b"\r\n".as_slice()].into_iter().find(|ending| {
                            side.strip_suffix(*ending).is_some_and(|fragment| {
                                fragment.rsplit(|b| *b == b'\n').next() == Some(original_last_line)
                            })
                        }).ok_or("The final conflict line was edited.")?;
                        range.end -= ending.len();
                    }
                }
            }
            if version.contains(&0)
                || version
                    .split_inclusive(|&b| b == b'\n')
                    .any(|line| marker(line, width).is_some())
            {
                return Err("A side holds binary data or lines that look like conflict markers.".into());
            }
        }
        if sides != 2 {
            return Err(
                "The line resolver needs a version from both sides.".into(),
            );
        }
        Ok(Self {
            id,
            path,
            hunks: hunks.into_iter().map(|hunk| align(&source, hunk)).collect(),
            source,
            stages,
            pieces,
        })
    }

    pub fn new_choices(&self) -> Choices {
        self.hunks.iter().map(|hunk| vec![None; hunk.len()]).collect()
    }

    pub fn unresolved(&self, choices: &[Vec<Option<Choice>>]) -> usize {
        self.hunks.iter().zip(choices).map(|(hunk, decisions)| {
            hunk.iter().zip(decisions).filter(|(line, choice)| !line.shared && choice.is_none()).count()
        }).sum()
    }

    pub fn preview(&self, choices: &[Vec<Option<Choice>>]) -> Preview {
        let mut rows = Vec::new();
        let mut hunk_starts = Vec::new();
        for piece in &self.pieces {
            match piece {
                Piece::Shared(range) => {
                    rows.extend(source_lines(&self.source, range.clone()).map(|range| {
                        ResultRow::Text { range, side: None }
                    }));
                }
                Piece::Hunk(hunk) => {
                    hunk_starts.push(rows.len());
                    for (line, pair) in self.hunks[*hunk].iter().enumerate() {
                        if pair.shared {
                            let (_, range) = pair.current.as_ref().expect("shared line has both versions");
                            rows.push(ResultRow::Text { range: range.clone(), side: None });
                        } else if let Some(choice) = choices[*hunk][line] {
                            for side in [Side::Current, Side::Incoming] {
                                if choice.includes(side) && let Some((_, range)) = pair.on(side) {
                                    rows.push(ResultRow::Text { range: range.clone(), side: Some(side) });
                                }
                            }
                        } else {
                            rows.push(ResultRow::Unresolved { hunk: *hunk, line });
                        }
                    }
                }
            }
        }
        Preview { rows, hunk_starts }
    }

    pub fn output(&self, choices: &[Vec<Option<Choice>>]) -> String {
        let mut text = String::with_capacity(self.source.len());
        let mut last_end = None;
        for row in self.preview(choices).rows {
            if let ResultRow::Text { range, .. } = row {
                if let Some(end) = last_end && !text.ends_with('\n') {
                    let ending = if self.source[end..].starts_with("\r\n") { "\r\n" } else { "\n" };
                    text.push_str(ending);
                }
                last_end = Some(range.end);
                text.push_str(&self.source[range]);
            }
        }
        text
    }

    pub fn save(&self, repo: &Repository, choices: &[Vec<Option<Choice>>]) -> Result<()> {
        if choices.len() != self.hunks.len()
            || self.hunks.iter().zip(choices).any(|(hunk, decisions)| hunk.len() != decisions.len())
            || self.unresolved(choices) != 0
        {
            return Err("Choose an outcome for every conflict line before saving.".into());
        }
        let absolute = regular_path(repo, &self.path)?;
        let result = self.output(choices);
        let temp = prepare(&absolute, result.as_bytes())?;
        let unchanged = || -> Result<bool> {
            repo.expect_operation(&self.id)?;
            Ok(regular_path(repo, &self.path)? == absolute
                && entries(repo, "--unmerged", Some(&self.path))? == self.stages
                && file_bytes(&absolute)? == self.source.as_bytes())
        };
        if !unchanged()? {
            return Err(
                "The conflicted file or index changed. Reopen the file before saving.".into(),
            );
        }
        temp.persist(&absolute).map_err(|e| e.to_string())?;
        if let Err(stage_error) = checked(
            repo.command()
                .args(["--literal-pathspecs", "add", "--"])
                .arg(&self.path),
        ) {
            if regular_path(repo, &self.path).is_ok()
                && file_bytes(&absolute)? == result.as_bytes()
                && entries(repo, "--unmerged", Some(&self.path))? == self.stages
                && current_id(repo)?.as_ref() == Some(&self.id)
            {
                prepare(&absolute, self.source.as_bytes())?
                    .persist(&absolute)
                    .map_err(|e| e.to_string())?;
            }
            return Err(stage_error);
        }
        Ok(())
    }
}

fn prepare(target: &Path, bytes: &[u8]) -> Result<NamedTempFile> {
    let parent = target.parent().ok_or("Missing parent directory")?;
    let mut file = NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    let mode = fs::metadata(target)
        .map_err(|e| e.to_string())?
        .permissions();
    file.as_file()
        .set_permissions(mode)
        .map_err(|e| e.to_string())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::os::unix::fs::symlink;

    fn branch(name: &str) -> Source {
        Source::Branch(name.into())
    }

    fn shown(repo: &Repository, id: &Id) -> Result<crate::operation::Snapshot> {
        let mut shown = snapshot(repo)?.ok_or("No merge is in progress.")?;
        shown.id = id.clone();
        Ok(shown)
    }

    fn abort(repo: &Repository, id: &Id) -> Result<()> {
        crate::operation::backend::abort(repo, &shown(repo, id)?).map(drop).map_err(|f| f.text)
    }

    fn finish(repo: &Repository, id: &Id) -> Result<()> {
        let shown = shown(repo, id)?;
        let message = shown.message.clone();
        crate::operation::backend::finish(repo, &shown, message).map(drop).map_err(|f| f.text)
    }

    fn on_main(head: &str) -> Seen {
        Seen { head_ref: Some("refs/heads/main".into()), head: Some(head.to_owned()) }
    }

    fn sides(doc: &Document, choices: &[Option<Side>]) -> Choices {
        doc.hunks.iter().zip(choices).map(|(hunk, side)| {
            vec![side.map(Choice::One); hunk.len()]
        }).collect()
    }

    fn divergent(path: &Path) -> (Fixture, String, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join(".gitattributes"), "* conflict-marker-size=9\n").unwrap();
        f.git(&["config", "--local", "merge.conflictStyle", "diff3"]);
        let base = (0..32).map(|i| format!("line {i}\n")).collect::<String>();
        let ours = base
            .replace("line 4\n", "Willow bridge\n")
            .replace("line 22\n", "Main waypoint\n")
            .replace("line 30\n", "Local notes\n");
        let theirs = base
            .replace("line 4\n", "Ridge pass\n")
            .replace("line 22\n", "Detour waypoint\n")
            .replace("line 13\n", "Incoming notes\n");
        fs::write(f.dir.join(path), base).unwrap();
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "base"]);
        f.git(&["switch", "-qc", "feature/detour"]);
        fs::write(f.dir.join(path), &theirs).unwrap();
        f.git(&["commit", "-qam", "incoming"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join(path), &ours).unwrap();
        f.git(&["commit", "-qam", "current"]);
        let incoming_oid = revision(&f.repo, OsStr::new("feature/detour")).unwrap();
        let current_head = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        let err = start(&f.repo, &branch("feature/detour"), &incoming_oid, &on_main(&current_head), Mode::FastForwardIfPossible, false).unwrap_err();
        assert!(err.contains("Merge conflict"), "{err}");
        (f, ours, theirs, incoming_oid)
    }

    #[test]
    fn mixed_hunks_preserve_other_changes_and_literal_paths() {
        let path = Path::new(":(glob)*\t\ntrail.txt");
        let (f, ours, _, _) = divergent(path);
        let state = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(state.conflicted(), [path]);
        let editor = Document::open(&f.repo, state.id.clone(), path.into()).unwrap();
        assert_eq!(editor.hunks.len(), 2);
        assert!(editor.save(&f.repo, &sides(&editor, &[Some(Side::Incoming), None])).is_err());
        let result = ours
            .replace("Willow bridge", "Ridge pass")
            .replace("line 13", "Incoming notes");
        let choices = sides(&editor, &[Some(Side::Incoming), Some(Side::Current)]);
        assert_eq!(editor.output(&choices), result);
        editor.save(&f.repo, &choices).unwrap();
        let ready = snapshot(&f.repo).unwrap().unwrap();
        assert!(ready.conflicts.is_empty());
        assert_eq!(ready.resolved, [path]);
        finish(&f.repo, &state.id).unwrap();
        assert_eq!(fs::read_to_string(f.dir.join(path)).unwrap(), result);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn editor_refuses_changed_worktree_index_and_symlinks() {
        let path = Path::new("trail.txt");
        let (f, _, _, _) = divergent(path);
        let id = snapshot(&f.repo).unwrap().unwrap().id;
        let doc = Document::open(&f.repo, id.clone(), path.into()).unwrap();
        let both = sides(&doc, &[Some(Side::Current), Some(Side::Incoming)]);
        let file = f.dir.join(path);
        fs::write(&file, "external edit\n").unwrap();
        assert!(doc.save(&f.repo, &both).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "external edit\n");
        fs::remove_file(&file).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), &file).unwrap();
        assert!(doc.save(&f.repo, &both).is_err());
        assert!(fs::read(outside.path()).unwrap().is_empty());
        fs::remove_file(&file).unwrap();
        fs::write(&file, doc.source.as_bytes()).unwrap();
        f.git(&["add", "--", "trail.txt"]);
        assert!(doc.save(&f.repo, &both).is_err());
        assert_eq!(fs::read(&file).unwrap(), doc.source.as_bytes());
        abort(&f.repo, &id).unwrap();
        assert!(finish(&f.repo, &id).is_err());
    }

    #[test]
    fn abort_undoes_resolutions_and_refuses_a_merge_other_than_the_one_named() {
        let path = Path::new("trail.txt");
        let (f, ours, _, _) = divergent(path);
        let head = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        let named = snapshot(&f.repo).unwrap().unwrap().id;

        f.git(&["merge", "--abort"]);
        f.git(&["switch", "-q", "feature/detour"]);
        f.git(&["commit", "--allow-empty", "-qm", "later incoming"]);
        let later = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-q", "main"]);
        assert!(
            start(&f.repo, &branch("feature/detour"), &later, &on_main(&head), Mode::FastForwardIfPossible, false)
                .unwrap_err()
                .contains("Merge conflict")
        );
        let state = snapshot(&f.repo).unwrap().unwrap();
        let doc = Document::open(&f.repo, state.id.clone(), path.into()).unwrap();
        doc.save(&f.repo, &sides(&doc, &[Some(Side::Incoming), Some(Side::Incoming)]))
            .unwrap();
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let resolved = fs::read(f.dir.join(path)).unwrap();
        let err = abort(&f.repo, &named).unwrap_err();
        assert!(err.contains("operation in progress changed"), "{err}");
        assert_eq!(f.git(&["rev-parse", "MERGE_HEAD"]), format!("{later}\n").as_bytes());
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(fs::read(f.dir.join(path)).unwrap(), resolved);

        abort(&f.repo, &state.id).unwrap();
        assert!(current_id(&f.repo).unwrap().is_none());
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
        assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), head);
        assert_eq!(fs::read_to_string(f.dir.join(path)).unwrap(), ours);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(abort(&f.repo, &state.id).is_err());
    }

    #[test]
    fn failed_staging_preserves_markers_and_retryable_choices() {
        let path = Path::new("trail.txt");
        let (f, _, _, _) = divergent(path);
        let state = snapshot(&f.repo).unwrap().unwrap();
        let doc = Document::open(&f.repo, state.id, path.into()).unwrap();
        let choices = sides(&doc, &[Some(Side::Current), Some(Side::Incoming)]);
        let lock = f.repo.git_dir().join("index.lock");
        fs::write(&lock, b"").unwrap();
        assert!(doc.save(&f.repo, &choices).is_err());
        assert_eq!(fs::read(&f.dir.join(path)).unwrap(), doc.source.as_bytes());
        fs::remove_file(lock).unwrap();
        doc.save(&f.repo, &choices).unwrap();
        assert!(entries(&f.repo, "--unmerged", None).unwrap().is_empty());
    }

    #[test]
    fn final_hunk_respects_each_sides_missing_newline_and_stale_tips() {
        let f = Fixture::new();
        let path = Path::new("end.txt");
        let write = |text: &str| {
            fs::write(f.dir.join(path), text).unwrap();
            f.git(&["add", "--", "end.txt"]);
            f.git(&["commit", "-qm", "update"]);
        };
        write("start\nbase");
        let base = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-qc", "incoming"]);
        write("start\nincoming");
        let incoming = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["branch", "alias-for-incoming"]);
        f.git(&["switch", "-q", "main"]);
        write("start\ncurrent\n");
        let head = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        assert!(start(&f.repo, &branch("incoming"), &incoming, &on_main(&base), Mode::FastForwardIfPossible, false).is_err());
        assert!(current_id(&f.repo).unwrap().is_none());
        assert!(
            start(&f.repo, &branch("incoming"), &incoming, &on_main(&head), Mode::FastForwardIfPossible, false)
                .unwrap_err()
                .contains("Merge conflict")
        );
        let state = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(state.theirs(), &incoming[..8]);
        let doc = Document::open(&f.repo, state.id, path.into()).unwrap();
        assert_eq!(doc.output(&sides(&doc, &[Some(Side::Current)])), "start\ncurrent\n");
        assert_eq!(doc.output(&sides(&doc, &[Some(Side::Incoming)])), "start\nincoming");
        assert_eq!(doc.output(&[vec![Some(Choice::Both)]]), "start\ncurrent\nincoming");
        doc.save(&f.repo, &sides(&doc, &[Some(Side::Incoming)])).unwrap();
        assert_eq!(f.git(&["show", ":end.txt"]), b"start\nincoming");
    }

    #[test]
    fn binary_conflict_can_be_resolved_externally_then_finished() {
        let f = Fixture::new();
        let path = Path::new("sample.bin");
        let commit = |contents: &[u8]| {
            fs::write(f.dir.join(path), contents).unwrap();
            f.git(&["add", "--", "sample.bin"]);
            f.git(&["commit", "-qm", "record binary sample"]);
        };
        commit(b"base\0sample");
        f.git(&["switch", "-qc", "incoming"]);
        commit(b"incoming\0sample");
        let theirs = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-q", "main"]);
        commit(b"current\0sample");
        let ours = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        assert!(
            start(&f.repo, &branch("incoming"), &theirs, &on_main(&ours), Mode::FastForwardIfPossible, false)
                .unwrap_err()
                .contains("Merge conflict")
        );
        let state = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(state.conflicted(), [path]);
        assert!(Document::open(&f.repo, state.id.clone(), path.into()).is_err());
        assert_eq!(fs::read(f.dir.join(path)).unwrap(), b"current\0sample");
        assert!(finish(&f.repo, &state.id).is_err());
        fs::write(f.dir.join(path), b"external\0result").unwrap();
        f.git(&["add", "--", "sample.bin"]);
        let ready = snapshot(&f.repo).unwrap().unwrap();
        assert!(ready.conflicts.is_empty());
        assert_eq!(ready.resolved, [path]);
        finish(&f.repo, &ready.id).unwrap();
        assert_eq!(f.git(&["show", "HEAD:sample.bin"]), b"external\0result");
    }

    #[test]
    fn crlf_and_missing_terminal_newline_in_shared_text_survive() {
        let text = "shared\r\n<<<<<<< HEAD\r\nours\r\n||||||| base\r\nold\r\n=======\r\ntheirs\r\n>>>>>>> feature\r\nsuffix";
        let (pieces, hunks) = parse(text.as_bytes(), 7).unwrap();
        let doc = Document {
            id: Id::blank(),
            path: "x".into(),
            source: text.to_owned(),
            pieces,
            hunks: hunks.into_iter().map(|hunk| align(text, hunk)).collect(),
            stages: vec![],
        };
        assert_eq!(
            doc.output(&sides(&doc, &[Some(Side::Incoming)])),
            "shared\r\ntheirs\r\nsuffix"
        );
        assert!(parse(b"<<<<<<< HEAD\nours\n=======\ntheirs\n", 7).is_err());
    }

    #[test]
    fn per_line_choices_keep_both_versions_allow_deletion_and_stage_the_preview() {
        let f = Fixture::new();
        let path = Path::new("notes.txt");
        let record = |body: &str| {
            fs::write(f.dir.join(path), body).unwrap();
            f.git(&["add", "--", "notes.txt"]);
            f.git(&["commit", "-qm", "edit notes"]);
        };
        record("Title\nbase first\nbase second\nending\n");
        f.git(&["switch", "-qc", "incoming"]);
        record("Title\nincoming first\nincoming second\nending\n");
        let incoming = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-q", "main"]);
        record("Title\ncurrent first\ncurrent second\ncurrent only\nending\n");
        let current = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        assert!(start(&f.repo, &branch("incoming"), &incoming, &on_main(&current), Mode::FastForwardIfPossible, false).is_err());
        let doc = Document::open(&f.repo, snapshot(&f.repo).unwrap().unwrap().id, path.into()).unwrap();
        assert_eq!(doc.hunks.len(), 1);
        let mut choices = doc.new_choices();
        choices[0][0] = Some(Choice::One(Side::Incoming));
        choices[0][1] = Some(Choice::Both);
        assert_eq!(doc.unresolved(&choices), 1);
        assert!(matches!(doc.preview(&choices).rows[4], ResultRow::Unresolved { hunk: 0, line: 2 }));
        assert!(doc.save(&f.repo, &choices).is_err());
        choices[0][2] = Some(Choice::One(Side::Incoming));
        let expected = "Title\nincoming first\ncurrent second\nincoming second\nending\n";
        assert_eq!(doc.output(&choices), expected);
        doc.save(&f.repo, &choices).unwrap();
        assert_eq!(f.git(&["show", ":notes.txt"]), expected.as_bytes());
    }

    #[test]
    fn alignment_keeps_common_lines_and_separates_both_missing_eof_newlines() {
        let source = "extra\r\nshared\r\ncurrent\r\nshared\r\nincoming\r\n";
        let hunk = Hunk { current: 0.."extra\r\nshared\r\ncurrent".len(), incoming: "extra\r\nshared\r\ncurrent\r\n".len()..source.len() - 2 };
        let rows = align(source, hunk);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].incoming.is_none());
        assert!(rows[1].shared);
        let doc = Document {
            id: Id::blank(),
            path: "x".into(),
            source: source.to_owned(),
            stages: vec![],
            pieces: vec![Piece::Hunk(0)],
            hunks: vec![rows],
        };
        let choices = vec![vec![Some(Choice::Both), None, Some(Choice::Both)]];
        assert_eq!(doc.output(&choices), "extra\r\nshared\r\ncurrent\r\nincoming");
    }

    fn apart() -> (Fixture, String, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "start\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", "Start"]);
        let start = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        fs::write(f.dir.join("remote.txt"), "remote\n").unwrap();
        f.git(&["add", "remote.txt"]);
        f.git(&["commit", "-qm", "Remote work"]);
        let remote = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["update-ref", "refs/remotes/origin/main", &remote]);
        f.git(&["reset", "-q", "--hard", &start]);
        f.git(&["switch", "-qc", "feature"]);
        fs::write(f.dir.join("feature.txt"), "feature\n").unwrap();
        f.git(&["add", "feature.txt"]);
        f.git(&["commit", "-qm", "Feature work"]);
        let feature = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        (f, start, remote, feature)
    }

    fn on(branch: &str, head: &str) -> Seen {
        Seen { head_ref: Some(format!("refs/heads/{branch}").into()), head: Some(head.to_owned()) }
    }

    fn remote(name: &str) -> Source {
        Source::RemoteBranch(name.into())
    }

    fn subject(f: &Fixture) -> String {
        String::from_utf8(f.git(&["log", "-1", "--format=%s"])).unwrap().trim_end().to_owned()
    }

    #[test]
    fn a_remote_branch_merges_with_gits_message_and_says_what_it_made() {
        let (f, _, origin, feature) = apart();
        let merged = start(&f.repo, &remote("origin/main"), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap();
        let head = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        assert_eq!(merged, Merged::Commit(head.clone()));
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{feature} {origin}\n").as_bytes());
        assert_eq!(subject(&f), "Merge remote-tracking branch 'origin/main' into feature");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(fs::read_to_string(f.dir.join("remote.txt")).unwrap(), "remote\n");
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        let outcome = super::super::outcome(&merged, "feature", "origin/main");
        assert_eq!(outcome.title, "Merged origin/main into feature");
        assert_eq!(outcome.detail, format!("Created merge commit {}.", &head[..7]));

        let again = start(&f.repo, &remote("origin/main"), &origin, &on("feature", &head), Mode::FastForwardIfPossible, false).unwrap();
        assert_eq!(again, Merged::Nothing);
        assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), head);
        let outcome = super::super::outcome(&again, "feature", "origin/main");
        assert_eq!(
            (outcome.title.as_str(), outcome.detail.as_str()),
            ("Already up to date", "feature already contains origin/main.")
        );
    }

    #[test]
    fn a_branch_behind_the_source_fast_forwards_and_counts_the_commits() {
        let (f, start_oid, origin, _) = apart();
        f.git(&["switch", "-qc", "behind", &start_oid]);
        let merged = start(&f.repo, &remote("origin/main"), &origin, &on("behind", &start_oid), Mode::FastForwardIfPossible, false).unwrap();
        assert_eq!(merged, Merged::FastForward(1));
        assert_eq!(revision(&f.repo, OsStr::new("refs/heads/behind")).unwrap(), origin);
        assert_eq!(merged.sentence("behind", "origin/main"), "Fast-forwarded behind by 1 commit.");
        assert_eq!(Merged::FastForward(3).sentence("x", "y"), "Fast-forwarded x by 3 commits.");
    }

    #[test]
    fn a_tag_and_a_commit_merge_under_their_own_names() {
        let (f, _, origin, feature) = apart();
        f.git(&["tag", "-a", "-m", "First release", "v1.2", &origin]);
        let merged = start(&f.repo, &Source::Tag("v1.2".into()), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap();
        let Merged::Commit(head) = merged else { panic!("{merged:?}") };
        assert_eq!(subject(&f), "Merge tag 'v1.2' into feature");
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{feature} {origin}\n").as_bytes());

        f.git(&["reset", "-q", "--hard", &feature]);
        let merged = start(&f.repo, &Source::Commit(origin.clone()), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap();
        assert!(matches!(merged, Merged::Commit(ref oid) if *oid != head), "{merged:?}");
        assert_eq!(subject(&f), format!("Merge commit '{origin}' into feature"));
        f.git(&["reset", "-q", "--hard", &feature]);
        let missing = "0123456789abcdef0123456789abcdef01234567";
        assert!(start(&f.repo, &Source::Commit(missing.into()), missing, &on("feature", &feature), Mode::FastForwardIfPossible, false).is_err());
        assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), feature);
    }

    #[test]
    fn a_name_that_git_would_read_as_another_ref_is_merged_by_its_full_name() {
        let (f, start_oid, origin, feature) = apart();
        f.git(&["branch", "origin/main", &start_oid]);
        let merged = start(&f.repo, &remote("origin/main"), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap();
        assert!(matches!(merged, Merged::Commit(_)), "{merged:?}");
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{feature} {origin}\n").as_bytes());
        assert_eq!(subject(&f), "Merge remote-tracking branch 'refs/remotes/origin/main' into feature");
    }

    #[test]
    fn a_merge_refuses_a_source_or_checkout_that_changed() {
        let (f, start_oid, origin, feature) = apart();
        let unchanged = |f: &Fixture| {
            assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), feature);
            assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
            assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
            assert!(f.git(&["status", "--porcelain"]).is_empty());
        };
        let err = start(&f.repo, &remote("origin/main"), &start_oid, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap_err();
        assert!(err.contains("origin/main changed"), "{err}");
        unchanged(&f);
        assert!(start(&f.repo, &remote("origin/gone"), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap_err().contains("no longer exists"));
        f.git(&["tag", "-a", "-m", "Moved", "v2", &start_oid]);
        assert!(start(&f.repo, &Source::Tag("v2".into()), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).is_err());
        f.git(&["branch", "twin"]);
        let err = start(&f.repo, &remote("origin/main"), &origin, &on("twin", &feature), Mode::FastForwardIfPossible, false).unwrap_err();
        assert!(err.contains("checked-out branch"), "{err}");
        assert!(start(&f.repo, &remote("origin/main"), &origin, &on("feature", &start_oid), Mode::FastForwardIfPossible, false).is_err());
        let detached = Seen { head_ref: None, head: Some(feature.clone()) };
        assert!(start(&f.repo, &remote("origin/main"), &origin, &detached, Mode::FastForwardIfPossible, false).unwrap_err().contains("local branch"));
        unchanged(&f);
        fs::write(f.repo.git_dir().join("CHERRY_PICK_HEAD"), format!("{origin}\n")).unwrap();
        let err = start(&f.repo, &remote("origin/main"), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap_err();
        assert_eq!(err, "Finish or abort the cherry-pick first.");
        fs::remove_file(f.repo.git_dir().join("CHERRY_PICK_HEAD")).unwrap();
        start(&f.repo, &remote("origin/main"), &origin, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap();
    }

    #[test]
    fn a_stopped_merge_is_named_by_the_branch_or_tag_it_brings_in() {
        let (f, _, origin, feature) = apart();
        fs::write(f.dir.join("trail.txt"), "feature side\n").unwrap();
        f.git(&["commit", "-qam", "Feature trail"]);
        assert_ne!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), feature);
        let feature = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-q", "--detach", &origin]);
        fs::write(f.dir.join("trail.txt"), "remote side\n").unwrap();
        f.git(&["commit", "-qam", "Remote trail"]);
        let incoming = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["update-ref", "refs/remotes/origin/main", &incoming]);
        f.git(&["update-ref", "refs/remotes/origin/HEAD", &incoming]);
        f.git(&["switch", "-q", "feature"]);
        let err = start(&f.repo, &remote("origin/main"), &incoming, &on("feature", &feature), Mode::FastForwardIfPossible, false).unwrap_err();
        assert!(err.contains("Merge conflict"), "{err}");
        let state = snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((state.theirs().as_str(), state.branch.as_str()), ("origin/main", "feature"));
        assert_eq!(state.source.as_ref().map(|s| s.oid.as_str()), Some(incoming.as_str()));
        f.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
        f.git(&["tag", "-a", "-m", "Trail", "v3", &incoming]);
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().theirs(), "v3");
        f.git(&["branch", "incoming", &incoming]);
        assert_eq!(snapshot(&f.repo).unwrap().unwrap().theirs(), "incoming");
        abort(&f.repo, &snapshot(&f.repo).unwrap().unwrap().id).unwrap();
    }

    #[test]
    fn a_merge_gives_local_changes_to_gits_autostash_only_when_asked() {
        let f = Fixture::new();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit\n").unwrap();
        f.git(&["add", "route.txt"]);
        f.git(&["commit", "-qm", "Plan the route"]);
        let head = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-qc", "north"]);
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nSummit\n").unwrap();
        f.git(&["commit", "-qam", "Start north"]);
        let north = revision(&f.repo, OsStr::new("HEAD")).unwrap();
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        f.git(&["config", "merge.autoStash", "true"]);
        let refused = start(&f.repo, &branch("north"), &north, &on_main(&head), Mode::FastForwardIfPossible, false).unwrap_err();
        assert!(refused.contains("would be overwritten by merge"), "{refused}");
        assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), head);
        assert_eq!(f.git(&["stash", "list"]), b"");
        let merged = start(&f.repo, &branch("north"), &north, &on_main(&head), Mode::FastForwardIfPossible, true).unwrap();
        assert_eq!(merged, Merged::FastForward(1));
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(!f.repo.git_dir().join("MERGE_AUTOSTASH").exists());
    }

    fn ahead() -> (Fixture, String, String) {
        use crate::operation::tests::{commit, oid};
        let f = Fixture::new();
        let main = commit(&f, "route.txt", "Start\nFord\nSummit\n", "Plan the route");
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "route.txt", "Start north\nFord\nSummit\n", "Start north");
        commit(&f, "camp.txt", "Camp by the ford\n", "Mark the camp");
        let feature = oid(&f, "feature");
        f.git(&["switch", "-q", "main"]);
        (f, main, feature)
    }

    #[test]
    fn each_merge_mode_does_what_it_says() {
        use crate::operation::tests::oid;
        let (f, main, feature) = ahead();
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::NoFastForward, false).unwrap();
        let head = oid(&f, "HEAD");
        assert_eq!(merged, Merged::Commit(head.clone()));
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{main} {feature}\n").as_bytes());
        assert_eq!(subject(&f), "Merge branch 'feature'");
        let again = start(&f.repo, &branch("feature"), &feature, &on_main(&head), Mode::NoFastForward, false).unwrap();
        assert_eq!((again, oid(&f, "HEAD")), (Merged::Nothing, head));

        f.git(&["reset", "-q", "--hard", &main]);
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::FastForwardOnly, false).unwrap();
        assert_eq!((merged, oid(&f, "HEAD")), (Merged::FastForward(2), feature.clone()));

        f.git(&["reset", "-q", "--hard", &main]);
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::Squash, false).unwrap();
        assert_eq!(merged, Merged::Squashed(2));
        assert_eq!(oid(&f, "HEAD"), main);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"A  camp.txt\nM  route.txt\n");
        assert_eq!(f.git(&["diff", "--cached", "feature"]), b"");
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
        assert_eq!(snapshot(&f.repo).unwrap(), None);
        let message = squash_message(&f.repo);
        assert!(message.starts_with("Squashed commit of the following:"), "{message:?}");
        assert!(message.contains("    Start north") && message.contains("    Mark the camp"), "{message:?}");
        assert_eq!(
            merged.sentence("main", "feature"),
            "The changes from 2 commits are staged on main, with a message in the commit box. Nothing is committed until you commit it."
        );
        f.repo.commit("Squash merge feature into main").unwrap();
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{main}\n").as_bytes());
        assert_eq!(f.git(&["diff", "HEAD", "feature"]), b"");
        assert!(squash_message(&f.repo).is_empty());
        let head = oid(&f, "HEAD");
        f.git(&["branch", "-f", "done", &main]);
        let nothing = start(&f.repo, &branch("done"), &main, &on_main(&head), Mode::Squash, false).unwrap();
        assert_eq!(nothing, Merged::Nothing);
        assert!(f.git(&["status", "--porcelain=v1"]).is_empty());
    }

    #[test]
    fn fast_forward_only_refuses_a_branch_with_commits_of_its_own_and_changes_nothing() {
        let (f, _, origin, feature) = apart();
        let err = start(&f.repo, &remote("origin/main"), &origin, &on("feature", &feature), Mode::FastForwardOnly, false)
            .unwrap_err();
        assert_eq!(
            err,
            "Cannot fast-forward feature to origin/main — 1 commit on feature is not in origin/main, so it cannot simply move forward. Nothing was changed."
        );
        assert_eq!(revision(&f.repo, OsStr::new("HEAD")).unwrap(), feature);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
    }

    #[test]
    fn a_squash_refuses_a_checkout_or_source_that_changed_and_stages_nothing() {
        use crate::operation::tests::{commit, oid};
        let (f, main, feature) = ahead();
        commit(&f, "notes.txt", "Pack water\n", "Pack");
        let err = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::Squash, false).unwrap_err();
        assert!(err.contains("changed"), "{err}");
        let head = oid(&f, "HEAD");
        let err = start(&f.repo, &branch("feature"), &main, &on_main(&head), Mode::Squash, false).unwrap_err();
        assert!(err.contains("feature changed"), "{err}");
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(!f.repo.git_dir().join("SQUASH_MSG").exists());
    }

    #[test]
    fn a_squash_that_conflicts_stops_uncommitted_and_aborts_back_to_the_files_before() {
        use crate::operation::{Kind, Picking, tests::{commit, oid}};
        let (f, main, feature) = ahead();
        let main_tip = commit(&f, "route.txt", "Start south\nFord\nSummit\n", "Start south");
        assert_ne!(main, main_tip);
        fs::write(f.dir.join("camp.txt"), "").unwrap_or_default();
        fs::remove_file(f.dir.join("camp.txt")).unwrap();
        fs::write(f.dir.join("notes.txt"), "Pack water\n").unwrap();
        f.git(&["add", "notes.txt"]);
        f.git(&["commit", "-qm", "Notes"]);
        let head = oid(&f, "HEAD");
        fs::write(f.dir.join("notes.txt"), "Pack water and a map\n").unwrap();
        let err = start(&f.repo, &branch("feature"), &feature, &on_main(&head), Mode::Squash, false).unwrap_err();
        assert_eq!(err, SQUASH_CONFLICT);
        let mut shown = snapshot(&f.repo).unwrap().expect("the conflict is shown");
        assert_eq!(shown.kind, Kind::Applied);
        assert_eq!(shown.conflicted(), [Path::new("route.txt")]);
        shown.picked = Some(Picking { oid: feature.clone(), label: "feature".into(), message: "Squash merge feature into main".into(), squash: true });
        assert_eq!(shown.summary(), "Squashing feature into main, not committed · 1 file needs resolving");
        assert_eq!((shown.theirs(), shown.theirs_name()), ("squashed feature".to_owned(), "feature".to_owned()));
        assert_eq!(shown.abort_label(), Some(crate::operation::ABORT_SQUASH));
        let aborted = crate::operation::backend::abort(&f.repo, &shown).map_err(|f| f.text).unwrap();
        assert_eq!(aborted.title, "Squash merge aborted");
        assert_eq!(aborted.detail, "Your files are as they were before feature was squashed in. main is unchanged.");
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" M notes.txt\n");
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start south\nFord\nSummit\n");
        assert!(!f.repo.git_dir().join("SQUASH_MSG").exists());
        assert_eq!(snapshot(&f.repo).unwrap(), None);
    }

    #[test]
    fn a_squash_over_changes_in_the_way_stashes_them_until_the_commit() {
        use crate::operation::tests::oid;
        let (f, main, feature) = ahead();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        let refused = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::Squash, false).unwrap_err();
        assert!(refused.contains("would be overwritten by merge"), "{refused}");
        assert!(f.git(&["diff", "--cached"]).is_empty());
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::Squash, true).unwrap();
        assert_eq!(merged, Merged::Squashed(2));
        assert!(crate::autostash::pending(&f.repo).is_none());
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"A  camp.txt\nMM route.txt\n");
        assert_eq!(f.git(&["stash", "list"]), b"");

        f.git(&["reset", "-q", "--hard", &main]);
        let main = crate::operation::tests::commit(&f, "notes.txt", "Pack water\n", "Pack");
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&main), Mode::Squash, true).unwrap();
        assert_eq!(merged, Merged::Squashed(2));
        assert!(crate::autostash::pending(&f.repo).is_some());
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit\n");
        let pending = crate::autostash::pending(&f.repo);
        f.repo.commit("Squash merge feature into main").unwrap();
        let kept = crate::autostash::Kept::default();
        let outcome = crate::autostash::settled(&f.repo, pending, &kept, crate::notice::Outcome::titled("Changes committed", ""))
            .map_err(|f| f.text)
            .unwrap();
        assert_eq!(outcome.title, "Changes committed");
        assert_ne!(oid(&f, "HEAD"), main);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(crate::autostash::pending(&f.repo).is_none());
    }

    #[test]
    fn a_squash_of_what_main_already_has_over_staged_changes_stages_nothing_of_its_own() {
        use crate::operation::tests::oid;
        let (f, _, feature) = ahead();
        f.git(&["merge", "-q", "--ff-only", "feature"]);
        let head = oid(&f, "HEAD");
        fs::write(f.dir.join("notes.txt"), "Pack water\n").unwrap();
        f.git(&["add", "notes.txt"]);
        let merged = start(&f.repo, &branch("feature"), &feature, &on_main(&head), Mode::Squash, false).unwrap();
        assert_eq!(merged, Merged::Nothing);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"A  notes.txt\n");
    }

    #[test]
    fn a_squash_that_conflicts_over_stashed_changes_says_so_and_its_abort_puts_them_back() {
        use crate::operation::{Picking, tests::{commit, oid}};
        let (f, _, feature) = ahead();
        commit(&f, "route.txt", "Start south\nFord\nSummit\n", "Start south");
        commit(&f, "notes.txt", "Pack water\n", "Notes");
        let head = oid(&f, "HEAD");
        fs::write(f.dir.join("notes.txt"), "Pack water and a map\n").unwrap();
        let err = start(&f.repo, &branch("feature"), &feature, &on_main(&head), Mode::Squash, true).unwrap_err();
        assert!(err.starts_with(SQUASH_CONFLICT), "{err}");
        assert!(err.contains("Your local changes were stashed first"), "{err}");
        assert_eq!(fs::read_to_string(f.dir.join("notes.txt")).unwrap(), "Pack water\n");
        let pending = crate::autostash::pending(&f.repo);
        assert!(pending.is_some());
        let mut shown = snapshot(&f.repo).unwrap().expect("the conflict is shown");
        shown.picked = Some(Picking { oid: feature.clone(), label: "feature".into(), message: String::new(), squash: true });
        let kept = crate::autostash::Kept::default();
        let aborted = crate::operation::backend::abort(&f.repo, &shown)
            .and_then(|outcome| crate::autostash::settled(&f.repo, pending, &kept, outcome))
            .map_err(|f| f.text)
            .unwrap();
        assert_eq!(aborted.title, "Squash merge aborted");
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(fs::read_to_string(f.dir.join("notes.txt")).unwrap(), "Pack water and a map\n");
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start south\nFord\nSummit\n");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" M notes.txt\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(crate::autostash::pending(&f.repo).is_none());
    }
}
