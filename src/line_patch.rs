use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Forward,
    Reverse,
}

impl Direction {
    fn target_only(self) -> u8 {
        match self {
            Self::Forward => b'+',
            Self::Reverse => b'-',
        }
    }

    fn base_only(self) -> u8 {
        match self {
            Self::Forward => b'-',
            Self::Reverse => b'+',
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Row<'a> {
    mark: u8,
    body: &'a [u8],
    newline: bool,
}

struct Range<'a> {
    old: i64,
    new: i64,
    old_count: i64,
    new_count: i64,
    rest: &'a [u8],
}

fn number(text: &[u8]) -> Option<i64> {
    std::str::from_utf8(text).ok()?.parse().ok()
}

fn side(field: &[u8]) -> Option<(i64, i64)> {
    match field.iter().position(|&b| b == b',') {
        Some(comma) => Some((number(&field[..comma])?, number(&field[comma + 1..])?)),
        None => Some((number(field)?, 1)),
    }
}

fn range(line: &[u8]) -> Option<Range<'_>> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let rest = line.strip_prefix(b"@@ -")?;
    let space = rest.iter().position(|&b| b == b' ')?;
    let (old, old_count) = side(&rest[..space])?;
    let rest = rest[space + 1..].strip_prefix(b"+")?;
    let space = rest.iter().position(|&b| b == b' ')?;
    let (new, new_count) = side(&rest[..space])?;
    let rest = rest[space + 1..].strip_prefix(b"@@")?;
    Some(Range { old, new, old_count, new_count, rest })
}

fn rows(body: &[u8]) -> (Vec<Row<'_>>, Vec<Option<usize>>) {
    let mut rows: Vec<Row> = Vec::new();
    let mut at = Vec::new();
    for line in body.split_inclusive(|&b| b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        match text.first() {
            Some(b'\\') => {
                if let Some(last) = rows.last_mut() {
                    last.newline = false;
                }
                at.push(None);
            }
            Some(&mark @ (b' ' | b'+' | b'-')) => {
                at.push(Some(rows.len()));
                rows.push(Row { mark, body: &text[1..], newline: true });
            }
            None => {
                at.push(Some(rows.len()));
                rows.push(Row { mark: b' ', body: b"", newline: true });
            }
            Some(_) => at.push(None),
        }
    }
    (rows, at)
}

fn rebuild<'a>(rows: &[Row<'a>], picked: &BTreeSet<usize>, direction: Direction) -> Vec<Row<'a>> {
    let mut kept = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let chosen = picked.contains(&index);
        let mark = match (row.mark, chosen, direction) {
            (b' ', ..) => Some(b' '),
            (mark, true, _) => Some(mark),
            (b'-', false, Direction::Forward) | (b'+', false, Direction::Reverse) => Some(b' '),
            _ => None,
        };
        if let Some(mark) = mark {
            kept.push(Row { mark, ..*row });
        }
    }
    let on_target = |row: &Row| row.mark == b' ' || row.mark == direction.target_only();
    let Some(last) = kept.iter().rposition(on_target) else { return kept };
    let mut out = Vec::with_capacity(kept.len() + 1);
    for (index, row) in kept.into_iter().enumerate() {
        if row.newline || index == last || !on_target(&row) {
            out.push(row);
        } else if row.mark == b' ' {
            let base = Row { mark: direction.base_only(), ..row };
            let target = Row { mark: direction.target_only(), newline: true, ..row };
            out.extend(if direction == Direction::Forward { [base, target] } else { [target, base] });
        } else {
            out.push(Row { newline: true, ..row });
        }
    }
    out
}

fn counts(rows: &[Row]) -> (i64, i64) {
    let old = rows.iter().filter(|row| row.mark != b'+').count() as i64;
    let new = rows.iter().filter(|row| row.mark != b'-').count() as i64;
    (old, new)
}

fn start(first: i64, count: i64) -> i64 {
    if count > 0 { first } else { (first - 1).max(0) }
}

fn first(start: i64, count: i64) -> i64 {
    if count > 0 { start } else { start + 1 }
}

fn swapped(path_line: &[u8], from: &[u8], to: &[u8]) -> Option<Vec<u8>> {
    let name = path_line.strip_suffix(b"\n").unwrap_or(path_line).get(4..)?;
    let (quoted, name) = match name.strip_prefix(b"\"") {
        Some(name) => (true, name),
        None => (false, name),
    };
    let name = name.strip_prefix(from)?;
    let mut line = Vec::with_capacity(name.len() + 8);
    line.extend_from_slice(if to == b"a/" { b"--- " } else { b"+++ " });
    if quoted {
        line.push(b'"');
    }
    line.extend_from_slice(to);
    line.extend_from_slice(name);
    line.push(b'\n');
    Some(line)
}

fn header(prefix: &[u8], modify: bool) -> Result<Vec<u8>, String> {
    let lines: Vec<&[u8]> = prefix.split_inclusive(|&b| b == b'\n').collect();
    let old_name = lines.iter().find(|line| line.starts_with(b"--- ")).copied();
    let new_name = lines.iter().find(|line| line.starts_with(b"+++ ")).copied();
    let mut out = Vec::with_capacity(prefix.len());
    for line in lines {
        if line.starts_with(b"old mode ") || line.starts_with(b"new mode ") {
            continue;
        }
        if modify {
            if line.starts_with(b"new file mode ") || line.starts_with(b"deleted file mode ") || line.starts_with(b"index ") {
                continue;
            }
            let absent = line.strip_suffix(b"\n").unwrap_or(line).ends_with(b" /dev/null");
            if absent && line.starts_with(b"--- ") {
                out.extend(new_name.and_then(|name| swapped(name, b"b/", b"a/")).ok_or("This file's diff has no new name.")?);
                continue;
            }
            if absent && line.starts_with(b"+++ ") {
                out.extend(old_name.and_then(|name| swapped(name, b"a/", b"b/")).ok_or("This file's diff has no old name.")?);
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    Ok(out)
}

fn write(out: &mut Vec<u8>, rows: &[Row]) {
    for row in rows {
        out.push(row.mark);
        out.extend_from_slice(row.body);
        out.push(b'\n');
        if !row.newline {
            out.extend_from_slice(b"\\ No newline at end of file\n");
        }
    }
}

pub(crate) fn build(prefix: &[u8], hunks: &[&[u8]], picks: &[(usize, usize)], direction: Direction) -> Result<Vec<u8>, String> {
    let mut chosen: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &(hunk, line) in picks {
        chosen.entry(hunk).or_default().insert(line);
    }
    if chosen.is_empty() {
        return Err("Pick an added or removed line first.".into());
    }
    let created = prefix.split(|&b| b == b'\n').any(|line| line.starts_with(b"new file mode "));
    let deleted = prefix.split(|&b| b == b'\n').any(|line| line.starts_with(b"deleted file mode "));
    let mut body = Vec::new();
    let mut delta = 0i64;
    let (mut old_total, mut new_total) = (0i64, 0i64);
    for (hunk, lines) in chosen {
        let bytes = hunks.get(hunk).ok_or("A picked line is no longer in this diff.")?;
        let split = bytes.iter().position(|&b| b == b'\n').map_or(bytes.len(), |at| at + 1);
        let range = range(&bytes[..split]).ok_or("A hunk header in this diff could not be read.")?;
        let (rows, at) = rows(&bytes[split..]);
        let mut picked = BTreeSet::new();
        for line in lines {
            let row = at.get(line).copied().flatten().filter(|&row| rows[row].mark != b' ');
            picked.insert(row.ok_or("Only added or removed lines can be staged.")?);
        }
        let rows = rebuild(&rows, &picked, direction);
        let (old_count, new_count) = counts(&rows);
        let (old, new) = match direction {
            Direction::Forward => {
                let new = start(first(range.old, range.old_count) + delta, new_count);
                delta += new_count - old_count;
                (range.old, new)
            }
            Direction::Reverse => {
                let old = start(first(range.new, range.new_count) + delta, old_count);
                delta += old_count - new_count;
                (old, range.new)
            }
        };
        old_total += old_count;
        new_total += new_count;
        body.extend_from_slice(format!("@@ -{old},{old_count} +{new},{new_count} @@").as_bytes());
        body.extend_from_slice(range.rest);
        body.push(b'\n');
        write(&mut body, &rows);
    }
    let modify = match direction {
        Direction::Forward => deleted && new_total > 0,
        Direction::Reverse => created && old_total > 0,
    };
    let mut patch = header(prefix, modify)?;
    patch.extend_from_slice(&body);
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &[u8] = b"diff --git a/log b/log\nindex 1111111..2222222 100644\n--- a/log\n+++ b/log\n";

    fn text(patch: Vec<u8>) -> String {
        String::from_utf8(patch).unwrap()
    }

    #[test]
    fn a_line_left_out_of_a_forward_patch_is_dropped_or_kept_as_context() {
        let hunk: &[u8] = b"@@ -1,3 +1,4 @@ intro\n a\n-b\n+B\n+C\n d\n";
        let patch = build(PREFIX, &[hunk], &[(0, 3)], Direction::Forward).unwrap();
        assert_eq!(
            text(patch),
            "diff --git a/log b/log\nindex 1111111..2222222 100644\n--- a/log\n+++ b/log\n@@ -1,3 +1,4 @@ intro\n a\n b\n+C\n d\n"
        );
        let patch = build(PREFIX, &[hunk], &[(0, 1)], Direction::Forward).unwrap();
        assert!(text(patch).ends_with("@@ -1,3 +1,2 @@ intro\n a\n-b\n d\n"));
    }

    #[test]
    fn a_reverse_patch_keeps_unpicked_additions_as_context_and_drops_unpicked_deletions() {
        let hunk: &[u8] = b"@@ -1,3 +1,4 @@\n a\n-b\n+B\n+C\n d\n";
        let patch = build(PREFIX, &[hunk], &[(0, 3)], Direction::Reverse).unwrap();
        assert!(text(patch).ends_with("@@ -1,3 +1,4 @@\n a\n B\n+C\n d\n"));
        let patch = build(PREFIX, &[hunk], &[(0, 1)], Direction::Reverse).unwrap();
        assert!(text(patch).ends_with("@@ -1,5 +1,4 @@\n a\n-b\n B\n C\n d\n"));
    }

    #[test]
    fn later_hunks_move_by_what_earlier_hunks_now_change() {
        let first: &[u8] = b"@@ -1,2 +1,4 @@\n a\n+x\n+y\n b\n";
        let second: &[u8] = b"@@ -10,2 +12,3 @@\n j\n+z\n k\n";
        let patch = text(build(PREFIX, &[first, second], &[(0, 1), (1, 1)], Direction::Forward).unwrap());
        assert!(patch.contains("@@ -1,2 +1,3 @@\n a\n+x\n b\n@@ -10,2 +11,3 @@\n j\n+z\n k\n"), "{patch}");
        let patch = text(build(PREFIX, &[first, second], &[(1, 1)], Direction::Forward).unwrap());
        assert!(patch.ends_with("--- a/log\n+++ b/log\n@@ -10,2 +10,3 @@\n j\n+z\n k\n"), "{patch}");
        let patch = text(build(PREFIX, &[first, second], &[(0, 2), (1, 1)], Direction::Reverse).unwrap());
        assert!(patch.contains("@@ -1,3 +1,4 @@\n a\n x\n+y\n b\n@@ -11,2 +12,3 @@\n j\n+z\n k\n"), "{patch}");
    }

    #[test]
    fn a_missing_final_newline_stays_on_its_line_and_is_moved_when_lines_follow() {
        let hunk: &[u8] = b"@@ -1,2 +1,3 @@\n x\n-y\n\\ No newline at end of file\n+y\n+z\n\\ No newline at end of file\n";
        let all = text(build(PREFIX, &[hunk], &[(0, 1), (0, 3), (0, 4)], Direction::Forward).unwrap());
        assert!(all.ends_with(std::str::from_utf8(hunk).unwrap()), "{all}");
        let only_z = text(build(PREFIX, &[hunk], &[(0, 4)], Direction::Forward).unwrap());
        assert!(
            only_z.ends_with("@@ -1,2 +1,3 @@\n x\n-y\n\\ No newline at end of file\n+y\n+z\n\\ No newline at end of file\n"),
            "{only_z}"
        );
        let no_z = text(build(PREFIX, &[hunk], &[(0, 1), (0, 3)], Direction::Forward).unwrap());
        assert!(no_z.ends_with("@@ -1,2 +1,2 @@\n x\n-y\n\\ No newline at end of file\n+y\n"), "{no_z}");
        let reverse = text(build(PREFIX, &[hunk], &[(0, 1)], Direction::Reverse).unwrap());
        assert!(reverse.ends_with("@@ -1,4 +1,3 @@\n x\n-y\n y\n z\n\\ No newline at end of file\n"), "{reverse}");
    }

    #[test]
    fn a_partial_pick_from_a_new_or_deleted_file_becomes_an_edit_when_it_must() {
        let created: &[u8] = b"diff --git a/new b/new\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new\n";
        let hunk: &[u8] = b"@@ -0,0 +1,2 @@\n+one\n+two\n";
        let staged = text(build(created, &[hunk], &[(0, 1)], Direction::Forward).unwrap());
        assert_eq!(staged, format!("{}@@ -0,0 +1,1 @@\n+two\n", std::str::from_utf8(created).unwrap()));
        let undone = text(build(created, &[hunk], &[(0, 1)], Direction::Reverse).unwrap());
        assert_eq!(undone, "diff --git a/new b/new\n--- a/new\n+++ b/new\n@@ -1,1 +1,2 @@\n one\n+two\n");
        let whole = text(build(created, &[hunk], &[(0, 0), (0, 1)], Direction::Reverse).unwrap());
        assert!(whole.starts_with("diff --git a/new b/new\nnew file mode 100644\n"), "{whole}");

        let removed: &[u8] =
            b"diff --git \"a/old \\303\\251\" \"b/old \\303\\251\"\ndeleted file mode 100755\nindex 4444444..0000000\n--- \"a/old \\303\\251\"\n+++ /dev/null\n";
        let hunk: &[u8] = b"@@ -1,2 +0,0 @@\n-one\n-two\n";
        let partial = text(build(removed, &[hunk], &[(0, 0)], Direction::Forward).unwrap());
        assert_eq!(
            partial,
            "diff --git \"a/old \\303\\251\" \"b/old \\303\\251\"\n--- \"a/old \\303\\251\"\n+++ \"b/old \\303\\251\"\n@@ -1,2 +1,1 @@\n-one\n two\n"
        );
        let back = text(build(removed, &[hunk], &[(0, 1)], Direction::Reverse).unwrap());
        assert!(back.contains("deleted file mode 100755\n") && back.ends_with("@@ -1,1 +0,0 @@\n-two\n"), "{back}");
    }

    #[test]
    fn mode_lines_are_left_out_and_bad_picks_are_refused() {
        let prefix: &[u8] = b"diff --git a/run b/run\nold mode 100644\nnew mode 100755\nindex 1..2\n--- a/run\n+++ b/run\n";
        let hunk: &[u8] = b"@@ -1 +1 @@\n-a\n+b\n";
        let patch = text(build(prefix, &[hunk], &[(0, 1)], Direction::Forward).unwrap());
        assert!(!patch.contains("mode"), "{patch}");
        assert!(patch.ends_with("@@ -1,1 +1,2 @@\n a\n+b\n"), "{patch}");
        assert!(build(prefix, &[hunk], &[], Direction::Forward).is_err());
        assert!(build(prefix, &[hunk], &[(1, 0)], Direction::Forward).is_err());
        let context: &[u8] = b"@@ -1,2 +1,2 @@\n a\n-b\n+c\n";
        assert!(build(prefix, &[context], &[(0, 0)], Direction::Forward).is_err());
        assert!(build(prefix, &[context], &[(0, 9)], Direction::Forward).is_err());
    }
}
