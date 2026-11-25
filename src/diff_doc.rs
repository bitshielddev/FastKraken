use std::{collections::HashMap, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const LINE_LIMIT: usize = 4096;
const SHORTENED: &str = "… [line preview limited]";
pub(crate) const EMPTY_FILE: &str = "This file is empty.";
pub(crate) const EMPTY_DELETED: &str = "The deleted file was empty.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Context,
    Added,
    Deleted,
    Hunk,
    File,
    Note,
    NoNewline,
}

impl Kind {
    pub(crate) fn is_code(self) -> bool {
        matches!(self, Self::Context | Self::Added | Self::Deleted)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blob {
    Absent,
    Bytes(u64),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blank {
    Nothing,
    EmptyFile,
    EmptyDeleted,
    Renamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PatchState {
    Loading,
    Text,
    Empty(Blank),
    Binary { old: Blob, new: Blob },
    ModeOnly { old: u32, new: u32 },
    Truncated,
    Unreadable(String),
}

impl Default for PatchState {
    fn default() -> Self {
        Self::Empty(Blank::Nothing)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub(crate) kind: Kind,
    pub(crate) old: Option<u32>,
    pub(crate) new: Option<u32>,
    pub(crate) at: u32,
    pub(crate) text: Range<usize>,
    pub(crate) hunk: Option<usize>,
    pub(crate) words: Vec<Range<usize>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DiffDoc {
    pub(crate) state: PatchState,
    pub(crate) lines: Vec<Line>,
    pub(crate) hunks: Vec<Range<usize>>,
    pub(crate) stageable: bool,
    pub(crate) shortened: bool,
    text: String,
}

pub(crate) struct Source<'a> {
    pub(crate) text: &'a str,
    pub(crate) stageable: bool,
    pub(crate) truncated: bool,
    pub(crate) sizes: &'a HashMap<String, u64>,
    pub(crate) working_size: Option<u64>,
}

#[derive(Default)]
struct Header {
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    old_blob: Option<String>,
    new_blob: Option<String>,
    created: bool,
    deleted: bool,
    renamed: bool,
    binary: bool,
    hunks: usize,
}

impl Header {
    fn mode_change(&self) -> Option<(u32, u32)> {
        self.old_mode.zip(self.new_mode).filter(|(old, new)| old != new)
    }

    fn blobs(&self, source: &Source) -> (Blob, Blob) {
        (
            blob(self.old_blob.as_deref(), source.sizes, None),
            blob(self.new_blob.as_deref(), source.sizes, source.working_size),
        )
    }

    fn read(&mut self, text: &str) -> bool {
        if let Some(mode) = text.strip_prefix("old mode ") {
            self.old_mode = octal(mode);
        } else if let Some(mode) = text.strip_prefix("new mode ") {
            self.new_mode = octal(mode);
        } else if text.starts_with("new file mode") {
            self.created = true;
        } else if text.starts_with("deleted file mode") {
            self.deleted = true;
        } else if let Some((old, new)) = index_blobs(text) {
            self.old_blob = Some(old.to_owned());
            self.new_blob = Some(new.to_owned());
        } else if text.starts_with("Binary files ") && text.ends_with(" differ") {
            self.binary = true;
        } else {
            return text.is_empty() || text.starts_with("--- ") || text.starts_with("+++ ") || text.starts_with("index ");
        }
        true
    }
}

fn octal(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim(), 8).ok()
}

fn index_blobs(text: &str) -> Option<(&str, &str)> {
    let ids = text.strip_prefix("index ")?.split(' ').next()?;
    ids.split_once("..")
}

fn blob(id: Option<&str>, sizes: &HashMap<String, u64>, working: Option<u64>) -> Blob {
    match id {
        None => Blob::Unknown,
        Some(id) if id.bytes().all(|b| b == b'0') => Blob::Absent,
        Some(id) => working.or_else(|| sizes.get(id).copied()).map_or(Blob::Unknown, Blob::Bytes),
    }
}

pub(crate) fn binary_blobs(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut current = None;
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            current = None;
        } else if let Some(pair) = index_blobs(line) {
            current = Some(pair);
        } else if line.starts_with("Binary files ")
            && let Some((old, new)) = current.take()
        {
            ids.extend([old, new].into_iter().filter(|id| !id.bytes().all(|b| b == b'0')).map(str::to_owned));
        }
    }
    ids
}

fn size(blob: Blob) -> String {
    match blob {
        Blob::Bytes(bytes) => crate::conflict::size(bytes),
        Blob::Absent | Blob::Unknown => "unknown size".to_owned(),
    }
}

pub(crate) fn binary_text(old: Blob, new: Blob) -> String {
    match (old, new) {
        (Blob::Absent, Blob::Absent) => "Binary file".to_owned(),
        (Blob::Absent, new) => format!("Binary file added · {}", size(new)),
        (old, Blob::Absent) => format!("Binary file deleted · {}", size(old)),
        (old, new) => format!("Binary file · {} → {}", size(old), size(new)),
    }
}

pub(crate) fn mode_only_text(old: u32, new: u32) -> String {
    format!("Only the file mode changed: {old:o} → {new:o}")
}

fn mode_note(old: u32, new: u32) -> String {
    format!("File mode changed: {old:o} → {new:o}")
}

pub(crate) fn truncated_text(stageable_view: bool) -> &'static str {
    if stageable_view {
        "This diff is larger than 1 MiB. The first part is shown and cannot be staged from here."
    } else {
        "This diff is larger than 1 MiB. Only the first part is shown."
    }
}

pub(crate) fn shortened_text(stageable_view: bool) -> &'static str {
    if stageable_view {
        "Lines longer than 4,096 bytes are shortened, so this diff cannot be staged from here."
    } else {
        "Lines longer than 4,096 bytes are shortened."
    }
}

struct Builder<'a> {
    doc: DiffDoc,
    source: &'a Source<'a>,
    several: bool,
    header: Header,
    old: Range<u32>,
    new: Range<u32>,
    hunk: Option<usize>,
    previous_was_code: bool,
}

impl Builder<'_> {
    fn push(&mut self, kind: Kind, old: Option<u32>, new: Option<u32>, text: &str) {
        let start = self.doc.text.len();
        if text.len() > LINE_LIMIT {
            self.doc.text.push_str(&text[..text.floor_char_boundary(LINE_LIMIT)]);
            self.doc.text.push_str(SHORTENED);
            self.doc.shortened = true;
        } else {
            self.doc.text.push_str(text);
        }
        let at = match kind {
            Kind::Deleted => self.new.start.max(1),
            _ => new.unwrap_or(0),
        };
        let hunk = match kind {
            Kind::File | Kind::Note => None,
            _ => self.hunk,
        };
        if let Some(index) = hunk {
            self.doc.hunks[index].end = self.doc.lines.len() + 1;
        }
        self.doc.lines.push(Line {
            kind,
            old,
            new,
            at,
            text: start..self.doc.text.len(),
            hunk,
            words: Vec::new(),
        });
        self.previous_was_code = kind.is_code();
    }

    fn note(&mut self, text: &str) {
        self.push(Kind::Note, None, None, text);
    }

    fn finish_file(&mut self) {
        let header = std::mem::take(&mut self.header);
        if !self.several || header.hunks > 0 {
            return;
        }
        let mode = header.mode_change();
        if header.binary {
            if let Some((old, new)) = mode {
                self.note(&mode_note(old, new));
            }
            let (old, new) = header.blobs(self.source);
            self.note(&binary_text(old, new));
        } else if let Some((old, new)) = mode {
            self.note(&mode_only_text(old, new));
        } else if header.created {
            self.note(EMPTY_FILE);
        } else if header.deleted {
            self.note(EMPTY_DELETED);
        }
    }

    fn line(&mut self, text: &str) {
        if text.starts_with("diff --git ") {
            self.finish_file();
            self.old = 0..0;
            self.new = 0..0;
            self.hunk = None;
            self.previous_was_code = false;
            if self.several {
                self.push(Kind::File, None, None, text);
            }
        } else if text.starts_with("@@ ") {
            let mut fields = text.split_ascii_whitespace().skip(1);
            self.old = hunk_range(fields.next(), '-');
            self.new = hunk_range(fields.next(), '+');
            if self.header.hunks == 0
                && let Some((old, new)) = self.header.mode_change()
            {
                self.note(&mode_note(old, new));
            }
            self.header.hunks += 1;
            let index = self.doc.hunks.len();
            self.doc.hunks.push(self.doc.lines.len()..self.doc.lines.len());
            self.hunk = Some(index);
            self.push(Kind::Hunk, None, None, text);
        } else if self.previous_was_code && text.starts_with("\\ ") {
            self.push(Kind::NoNewline, None, None, text);
        } else if text.starts_with('+') && !self.new.is_empty() {
            let new = self.new.next();
            self.push(Kind::Added, None, new, text);
        } else if text.starts_with('-') && !self.old.is_empty() {
            let old = self.old.next();
            self.push(Kind::Deleted, old, None, text);
        } else if (text.starts_with(' ') || text.is_empty()) && !self.old.is_empty() && !self.new.is_empty() {
            let (old, new) = (self.old.next(), self.new.next());
            self.push(Kind::Context, old, new, text);
        } else {
            if ["rename from ", "copy from "].iter().any(|origin| text.starts_with(origin)) {
                self.header.renamed = true;
            }
            if renaming(text, self.several) || self.header.read(text) {
                return;
            }
            self.note(text);
        }
    }

    fn state(&self) -> PatchState {
        let header = &self.header;
        if self.source.truncated {
            PatchState::Truncated
        } else if !self.doc.lines.is_empty() {
            PatchState::Text
        } else if header.binary {
            let (old, new) = header.blobs(self.source);
            PatchState::Binary { old, new }
        } else if let Some((old, new)) = header.mode_change() {
            PatchState::ModeOnly { old, new }
        } else if header.created {
            PatchState::Empty(Blank::EmptyFile)
        } else if header.deleted {
            PatchState::Empty(Blank::EmptyDeleted)
        } else if header.renamed {
            PatchState::Empty(Blank::Renamed)
        } else {
            PatchState::Empty(Blank::Nothing)
        }
    }
}

fn renaming(text: &str, several: bool) -> bool {
    let origin = ["rename from ", "copy from "].iter().any(|header| text.starts_with(header));
    (origin && !several)
        || ["similarity index ", "dissimilarity index ", "rename to ", "copy to "]
            .iter()
            .any(|header| text.starts_with(header))
}

fn hunk_range(field: Option<&str>, prefix: char) -> Range<u32> {
    let field = field
        .and_then(|value| value.strip_prefix(prefix))
        .unwrap_or_default();
    let (start, count) = field.split_once(',').unwrap_or((field, "1"));
    let start = start.parse::<u32>().unwrap_or(1);
    start..start.saturating_add(count.parse().unwrap_or(1))
}

impl DiffDoc {
    pub(crate) fn build(source: &Source) -> Self {
        let files = source
            .text
            .lines()
            .filter(|line| line.starts_with("diff --git "))
            .count();
        let mut builder = Builder {
            doc: DiffDoc {
                text: String::with_capacity(source.text.len()),
                ..Self::default()
            },
            source,
            several: files > 1,
            header: Header::default(),
            old: 0..0,
            new: 0..0,
            hunk: None,
            previous_was_code: false,
        };
        for text in source.text.lines() {
            builder.line(text);
        }
        let state = builder.state();
        if !source.truncated {
            builder.finish_file();
        }
        let mut doc = builder.doc;
        doc.state = state;
        doc.stageable = source.stageable && !source.truncated && !doc.shortened;
        doc.mark_words();
        doc
    }

    pub(crate) fn text(&self, line: &Line) -> &str {
        &self.text[line.text.clone()]
    }

    pub(crate) fn code(&self, line: &Line) -> &str {
        let text = self.text(line);
        if line.kind.is_code() {
            text.get(1..).unwrap_or_default()
        } else {
            text
        }
    }

    pub(crate) fn has(&self, patch_line: &str) -> bool {
        self.lines.iter().any(|line| self.text(line) == patch_line)
    }

    pub(crate) fn stageable_hunks(&self) -> usize {
        if self.stageable { self.hunks.len() } else { 0 }
    }

    fn mark_words(&mut self) {
        let mut pairs = Vec::new();
        runs(&self.lines, |run| {
            if let Run::Change { deleted, added, .. } = run {
                pairs.extend(deleted.iter().copied().zip(added.iter().copied()));
            }
        });
        for (old, new) in pairs {
            let (before, after) = word_changes(self.code(&self.lines[old]), self.code(&self.lines[new]));
            self.lines[old].words = before;
            self.lines[new].words = after;
        }
    }
}

pub(crate) enum Run<'a> {
    Shared(usize),
    Both(usize),
    Change {
        deleted: &'a [usize],
        added: &'a [usize],
        old_note: Option<usize>,
        new_note: Option<usize>,
    },
}

pub(crate) fn runs(lines: &[Line], mut visit: impl FnMut(Run)) {
    let mut index = 0;
    let (mut deleted, mut added) = (Vec::new(), Vec::new());
    while index < lines.len() {
        let kind = lines[index].kind;
        if kind == Kind::Context {
            visit(Run::Both(index));
            index += 1;
        } else if !matches!(kind, Kind::Added | Kind::Deleted) {
            visit(Run::Shared(index));
            index += 1;
        } else {
            deleted.clear();
            added.clear();
            let (mut old_note, mut new_note) = (None, None);
            while index < lines.len() {
                match lines[index].kind {
                    Kind::Deleted => deleted.push(index),
                    Kind::Added => added.push(index),
                    Kind::NoNewline if lines[index - 1].kind == Kind::Deleted => old_note = Some(index),
                    Kind::NoNewline => new_note = Some(index),
                    _ => break,
                }
                index += 1;
            }
            visit(Run::Change {
                deleted: &deleted,
                added: &added,
                old_note,
                new_note,
            });
        }
    }
}

fn word_changes(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_words: Vec<_> = old.split_word_bound_indices().collect();
    let new_words: Vec<_> = new.split_word_bound_indices().collect();
    let prefix = old_words
        .iter()
        .zip(&new_words)
        .take_while(|(a, b)| a.1 == b.1)
        .count();
    let suffix = old_words[prefix..]
        .iter()
        .rev()
        .zip(new_words[prefix..].iter().rev())
        .take_while(|(a, b)| a.1 == b.1)
        .count();
    let old_middle = &old_words[prefix..old_words.len() - suffix];
    let new_middle = &new_words[prefix..new_words.len() - suffix];
    let (mut before, mut after) = (Vec::new(), Vec::new());

    const MAX_CELLS: usize = 4096;
    let cols = new_middle.len() + 1;
    let cells = (old_middle.len() + 1) * cols;
    if cells > MAX_CELLS || old_middle.is_empty() || new_middle.is_empty() {
        before.extend(token_range(old_middle));
        after.extend(token_range(new_middle));
        return (before, after);
    }

    let mut common = vec![0u16; cells];
    for (i, &(_, old_word)) in old_middle.iter().enumerate().rev() {
        for (j, &(_, new_word)) in new_middle.iter().enumerate().rev() {
            let cell = i * cols + j;
            common[cell] = if old_word == new_word {
                common[cell + cols + 1] + 1
            } else {
                common[cell + 1].max(common[cell + cols])
            };
        }
    }
    let (mut i, mut j, mut old_start, mut new_start) = (0, 0, 0, 0);
    while i < old_middle.len() && j < new_middle.len() {
        if old_middle[i].1 == new_middle[j].1 {
            mark(&mut before, old, &old_middle[old_start..i]);
            mark(&mut after, new, &new_middle[new_start..j]);
            (i, j, old_start, new_start) = (i + 1, j + 1, i + 1, j + 1);
        } else if common[i * cols + j + 1] > common[(i + 1) * cols + j] {
            j += 1;
        } else {
            i += 1;
        }
    }
    mark(&mut before, old, &old_middle[old_start..]);
    mark(&mut after, new, &new_middle[new_start..]);
    (before, after)
}

fn token_range(tokens: &[(usize, &str)]) -> Option<Range<usize>> {
    let start = tokens.first()?.0;
    let (end, last) = *tokens.last()?;
    Some(start..end + last.len())
}

fn mark(out: &mut Vec<Range<usize>>, text: &str, tokens: &[(usize, &str)]) {
    if let Some(range) = token_range(tokens) {
        if let Some(previous) = out.last_mut()
            && text[previous.end..range.start].trim().is_empty()
        {
            previous.end = range.end;
        } else {
            out.push(range);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn doc(text: &str) -> DiffDoc {
        with(text, |_| {})
    }

    fn with(text: &str, change: impl FnOnce(&mut Source)) -> DiffDoc {
        let sizes = HashMap::new();
        let mut source = Source {
            text,
            stageable: true,
            truncated: false,
            sizes: &sizes,
            working_size: None,
        };
        change(&mut source);
        DiffDoc::build(&source)
    }

    fn sized(text: &str, sizes: &[(&str, u64)], working_size: Option<u64>) -> DiffDoc {
        let sizes: HashMap<String, u64> = sizes.iter().map(|(id, size)| ((*id).to_owned(), *size)).collect();
        DiffDoc::build(&Source {
            text,
            stageable: false,
            truncated: false,
            sizes: &sizes,
            working_size,
        })
    }

    fn marked(text: &str, spans: &[Range<usize>]) -> Vec<String> {
        spans.iter().map(|span| text[span.clone()].to_owned()).collect()
    }

    fn texts(doc: &DiffDoc, kind: Kind) -> Vec<&str> {
        doc.lines.iter().filter(|line| line.kind == kind).map(|line| doc.text(line)).collect()
    }

    const OLD: &str = "1111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222";
    const NONE: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn each_line_of_code_knows_where_it_is_in_both_files() {
        let doc = doc(
            "diff --git a/trail.txt b/trail.txt\n--- a/trail.txt\n+++ b/trail.txt\n@@ -1,4 +1,4 @@\n first\n-ford\n+stepping stones\n summit\n@@ -9,2 +9,1 @@\n ninth\n-tenth",
        );
        let rows: Vec<(Kind, Option<u32>, Option<u32>, u32)> =
            doc.lines.iter().map(|line| (line.kind, line.old, line.new, line.at)).collect();
        assert_eq!(
            rows,
            [
                (Kind::Hunk, None, None, 0),
                (Kind::Context, Some(1), Some(1), 1),
                (Kind::Deleted, Some(2), None, 2),
                (Kind::Added, None, Some(2), 2),
                (Kind::Context, Some(3), Some(3), 3),
                (Kind::Hunk, None, None, 0),
                (Kind::Context, Some(9), Some(9), 9),
                (Kind::Deleted, Some(10), None, 10),
            ]
        );
        assert_eq!(doc.state, PatchState::Text);
        assert_eq!(doc.hunks, [0..5, 5..8]);
        assert_eq!(doc.lines.iter().map(|line| line.hunk).collect::<Vec<_>>(), [
            Some(0), Some(0), Some(0), Some(0), Some(0), Some(1), Some(1), Some(1)
        ]);
        assert_eq!(doc.code(&doc.lines[3]), "stepping stones");
        assert!(doc.has("+stepping stones"));
        assert_eq!(doc.stageable_hunks(), 2);
    }

    #[test]
    fn highlights_disjoint_changes_at_unicode_word_boundaries() {
        let old = "Length: café via 石 gate";
        let new = "Length: cafés via 木 gate";
        let (before, after) = word_changes(old, new);
        assert_eq!(marked(old, &before), ["café", "石"]);
        assert_eq!(marked(new, &after), ["cafés", "木"]);

        let old = "Waypoint: Willow";
        let new = "Waypoint: Willow (seasonal)";
        let (before, after) = word_changes(old, new);
        assert!(before.is_empty());
        assert_eq!(marked(new, &after), [" (seasonal)"]);
    }

    #[test]
    fn paired_changes_mark_their_changed_words() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -4,3 +4,4 @@ section\n here\n-Old hut\n+New hut\n+New trail\n end\n",
        ));
        assert_eq!(marked("Old hut", &doc.lines[2].words), ["Old"]);
        assert_eq!(marked("New hut", &doc.lines[3].words), ["New"]);
        assert!(doc.lines[4].words.is_empty());
    }

    #[test]
    fn a_binary_file_names_both_sizes() {
        let patch = format!("diff --git a/logo.png b/logo.png\nindex {OLD}..{NEW} 100644\nBinary files a/logo.png and b/logo.png differ\n");
        let doc = sized(&patch, &[(OLD, 12_000), (NEW, 14_500)], None);
        assert_eq!(doc.state, PatchState::Binary { old: Blob::Bytes(12_000), new: Blob::Bytes(14_500) });
        assert!(doc.lines.is_empty());
        assert_eq!(binary_text(Blob::Bytes(12_000), Blob::Bytes(14_500)), "Binary file · 12.0 KB → 14.5 KB");
        assert_eq!(binary_blobs(&patch), [OLD, NEW]);

        let working = sized(&patch, &[(OLD, 12_000)], Some(900));
        assert_eq!(working.state, PatchState::Binary { old: Blob::Bytes(12_000), new: Blob::Bytes(900) });

        let added = format!("diff --git a/logo.png b/logo.png\nnew file mode 100644\nindex {NONE}..{NEW}\nBinary files /dev/null and b/logo.png differ\n");
        assert_eq!(binary_blobs(&added), [NEW]);
        let doc = sized(&added, &[(NEW, 7)], None);
        assert_eq!(doc.state, PatchState::Binary { old: Blob::Absent, new: Blob::Bytes(7) });
        assert_eq!(binary_text(Blob::Absent, Blob::Bytes(7)), "Binary file added · 7 bytes");
        assert_eq!(binary_text(Blob::Bytes(7), Blob::Absent), "Binary file deleted · 7 bytes");
        assert_eq!(binary_text(Blob::Unknown, Blob::Bytes(7)), "Binary file · unknown size → 7 bytes");
    }

    fn read(f: &crate::repository::tests::Fixture, path: &str, staged: bool, working: bool) -> DiffDoc {
        let diff = f.repo.file_diff(std::path::Path::new(path), staged).unwrap();
        let sizes = f.repo.object_sizes(&binary_blobs(&diff.text)).unwrap();
        let working_size = working.then(|| std::fs::metadata(f.dir.join(path)).unwrap().len());
        DiffDoc::build(&Source {
            text: &diff.text,
            stageable: diff.hunks_stageable(),
            truncated: diff.truncated,
            sizes: &sizes,
            working_size,
        })
    }

    #[test]
    fn git_diffs_of_a_binary_file_and_a_mode_change_read_as_placeholders() {
        use std::os::unix::fs::PermissionsExt;
        let f = crate::repository::tests::Fixture::new();
        std::fs::write(f.dir.join("chart.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(f.dir.join("run.sh"), "run\n").unwrap();
        std::fs::set_permissions(f.dir.join("run.sh"), std::fs::Permissions::from_mode(0o644)).unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("base").unwrap();
        std::fs::write(f.dir.join("chart.bin"), [0u8, 1, 2, 3, 4, 5]).unwrap();
        std::fs::set_permissions(f.dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(f.dir.join("new.bin"), [0u8; 9]).unwrap();

        let unstaged = read(&f, "chart.bin", false, true);
        assert_eq!(unstaged.state, PatchState::Binary { old: Blob::Bytes(3), new: Blob::Bytes(6) });
        let untracked = read(&f, "new.bin", false, true);
        assert_eq!(untracked.state, PatchState::Binary { old: Blob::Absent, new: Blob::Bytes(9) });
        let mode = read(&f, "run.sh", false, false);
        assert_eq!(mode.state, PatchState::ModeOnly { old: 0o100644, new: 0o100755 });

        f.repo.stage_all().unwrap();
        let staged = read(&f, "chart.bin", true, false);
        assert_eq!(staged.state, PatchState::Binary { old: Blob::Bytes(3), new: Blob::Bytes(6) });
        assert_eq!(read(&f, "run.sh", true, false).state, PatchState::ModeOnly { old: 0o100644, new: 0o100755 });
        assert_eq!(read(&f, "new.bin", true, false).state, PatchState::Binary { old: Blob::Absent, new: Blob::Bytes(9) });
    }

    #[test]
    fn a_mode_change_is_named_alone_or_beside_the_text() {
        let only = doc("diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n");
        assert_eq!(only.state, PatchState::ModeOnly { old: 0o100644, new: 0o100755 });
        assert_eq!(mode_only_text(0o100644, 0o100755), "Only the file mode changed: 100644 → 100755");

        let both = doc(concat!(
            "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\nindex 012..345\n",
            "--- a/run.sh\n+++ b/run.sh\n@@ -1 +1 @@\n-one\n+two\n",
        ));
        assert_eq!(both.state, PatchState::Text);
        assert_eq!(texts(&both, Kind::Note), ["File mode changed: 100644 → 100755"]);
        assert_eq!(both.lines[0].hunk, None);
        assert_eq!(both.hunks, [1..4]);
    }

    #[test]
    fn files_without_text_say_why() {
        let empty = format!("diff --git a/blank b/blank\nnew file mode 100644\nindex {NONE}..e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n");
        assert_eq!(doc(&empty).state, PatchState::Empty(Blank::EmptyFile));
        let gone = format!("diff --git a/blank b/blank\ndeleted file mode 100644\nindex e69de29bb2d1d6434b8b29ae775ad8c2e48c5391..{NONE}\n");
        assert_eq!(doc(&gone).state, PatchState::Empty(Blank::EmptyDeleted));
        let moved = "diff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\n";
        assert_eq!(doc(moved).state, PatchState::Empty(Blank::Renamed));
        assert_eq!(doc("").state, PatchState::Empty(Blank::Nothing));
        assert_eq!(DiffDoc::default().state, PatchState::Empty(Blank::Nothing));
    }

    #[test]
    fn several_files_carry_their_own_notes() {
        let patch = format!(concat!(
            "diff --git a/logo.png b/logo.png\nindex {old}..{new} 100644\nBinary files a/logo.png and b/logo.png differ\n",
            "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n",
            "diff --git a/blank b/blank\nnew file mode 100644\nindex {none}..e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n",
            "diff --git a/kept b/kept\n--- a/kept\n+++ b/kept\n@@ -1 +1 @@\n-a\n+b\n",
        ), old = OLD, new = NEW, none = NONE);
        let doc = sized(&patch, &[(OLD, 3), (NEW, 5)], None);
        assert_eq!(doc.state, PatchState::Text);
        assert_eq!(texts(&doc, Kind::Note), [
            "Binary file · 3 bytes → 5 bytes",
            "Only the file mode changed: 100644 → 100755",
            EMPTY_FILE,
        ]);
        assert_eq!(texts(&doc, Kind::File).len(), 4);
        assert_eq!(doc.hunks.len(), 1);
    }

    #[test]
    fn a_cut_or_shortened_diff_cannot_be_staged() {
        let text = "diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -1,2 +1,2 @@\n-one\n+two\n";
        let cut = with(text, |source| source.truncated = true);
        assert_eq!(cut.state, PatchState::Truncated);
        assert!(!cut.stageable);
        assert_eq!(cut.stageable_hunks(), 0);

        let long = format!("diff --git a/big b/big\n@@ -1 +1 @@\n-one\n+{}\n", "x".repeat(LINE_LIMIT + 10));
        let shortened = doc(&long);
        assert!(shortened.shortened);
        assert!(!shortened.stageable);
        assert!(shortened.text(&shortened.lines[2]).ends_with(SHORTENED));
        assert!(doc(text).stageable);

        let cut_after_header = concat!(
            "diff --git a/kept b/kept\n--- a/kept\n+++ b/kept\n@@ -1 +1 @@\n-a\n+b\n",
            "diff --git a/blank b/blank\nnew file mode 100644\n",
        );
        let cut = with(cut_after_header, |source| source.truncated = true);
        assert!(texts(&cut, Kind::Note).is_empty());
        assert_eq!(texts(&doc(cut_after_header), Kind::Note), [EMPTY_FILE]);
    }

    #[test]
    fn a_type_change_offers_no_hunk_to_stage() {
        let f = crate::repository::tests::Fixture::new();
        std::fs::write(f.dir.join("link"), "hello\n").unwrap();
        f.repo.stage_all().unwrap();
        f.repo.commit("base").unwrap();
        std::fs::remove_file(f.dir.join("link")).unwrap();
        std::os::unix::fs::symlink("target", f.dir.join("link")).unwrap();

        let doc = read(&f, "link", false, false);
        assert_eq!(texts(&doc, Kind::File).len(), 2);
        assert_eq!(doc.hunks.len(), 2);
        assert!(!doc.stageable);
        assert_eq!(doc.stageable_hunks(), 0);
    }

    #[test]
    fn split_runs_pair_changes_and_keep_newline_notes() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\n@@ -1 +1 @@\n-Old hut\n",
            "\\ No newline at end of file\n+New hut\n\\ No newline at end of file\n",
            "diff --git a/map b/map\n@@ -0,0 +1 @@\n+map\n\n[an unexpected line]\n",
        ));
        let mut seen = Vec::new();
        runs(&doc.lines, |run| {
            seen.push(match run {
                Run::Shared(row) => format!("shared {row}"),
                Run::Both(row) => format!("both {row}"),
                Run::Change { deleted, added, old_note, new_note } => {
                    format!("change {deleted:?} {added:?} {old_note:?} {new_note:?}")
                }
            })
        });
        assert_eq!(seen, [
            "shared 0",
            "shared 1",
            "change [2] [4] Some(3) Some(5)",
            "shared 6",
            "shared 7",
            "change [] [8] None None",
            "shared 9",
        ]);
        assert_eq!(doc.lines[9].kind, Kind::Note);
        assert_eq!(doc.lines[8].new, Some(1));
    }

    #[test]
    fn rename_headers_are_hidden_for_one_file_and_name_the_old_path_among_several() {
        let one = doc(concat!(
            "diff --git a/old.txt b/new.txt\nsimilarity index 85%\nrename from old.txt\nrename to new.txt\n",
            "index 012..345 100644\n--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-four\n+FOUR\n",
        ));
        let kinds: Vec<Kind> = one.lines.iter().map(|row| row.kind).collect();
        assert_eq!(kinds, [Kind::Hunk, Kind::Deleted, Kind::Added]);

        let several = doc(concat!(
            "diff --git a/old.txt b/new.txt\nsimilarity index 100%\nrename from old.txt\nrename to new.txt\n",
            "diff --git a/kept.txt b/kept.txt\n--- a/kept.txt\n+++ b/kept.txt\n@@ -1 +1 @@\n-a\n+b\n",
        ));
        assert_eq!(texts(&several, Kind::Note), ["rename from old.txt"]);
        assert_eq!(texts(&several, Kind::File).len(), 2);
    }
}
