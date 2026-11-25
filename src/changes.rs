use crate::{Tab, repository::Repository};
use gpui::{Context, Window};
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) const BATCH: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Size {
    pub(crate) files: u32,
    pub(crate) added: u32,
    pub(crate) deleted: u32,
}

impl Size {
    pub(crate) fn lines(&self) -> u32 {
        self.added.saturating_add(self.deleted)
    }

    pub(crate) fn tip(&self) -> String {
        let count = |count: u32, one: &str, many: &str| if count == 1 { format!("1 {one}") } else { format!("{count} {many}") };
        if self.files == 0 {
            return "No files changed".to_owned();
        }
        format!(
            "{} changed, {}, {}",
            count(self.files, "file", "files"),
            count(self.added, "addition", "additions"),
            count(self.deleted, "deletion", "deletions"),
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct State {
    sizes: HashMap<String, Size>,
    asked: HashSet<String>,
    queue: VecDeque<String>,
    running: bool,
}

impl State {
    pub(crate) fn of(&self, oid: &str) -> Option<Size> {
        self.sizes.get(oid).copied()
    }

    pub(crate) fn unknown<'a>(&self, oids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        oids.into_iter().filter(|oid| !self.asked.contains(*oid)).map(str::to_owned).collect()
    }

    fn ask(&mut self, oids: Vec<String>) {
        for oid in oids {
            if self.asked.insert(oid.clone()) {
                self.queue.push_back(oid);
            }
        }
    }

    fn next_batch(&mut self) -> Vec<String> {
        let count = self.queue.len().min(BATCH);
        self.queue.drain(..count).collect()
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Load(Vec<String>),
}

pub(crate) fn parse(output: &str) -> HashMap<String, Size> {
    let mut sizes = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(' ') && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            sizes.insert(trimmed.to_owned(), Size::default());
            current = Some(trimmed.to_owned());
            continue;
        }
        let Some(size) = current.as_ref().and_then(|oid| sizes.get_mut(oid)) else { continue };
        for part in trimmed.split(", ") {
            let Some((number, what)) = part.split_once(' ') else { continue };
            let Ok(number) = number.parse::<u32>() else { continue };
            if what.starts_with("file") {
                size.files = number;
            } else if what.starts_with("insertion") {
                size.added = number;
            } else if what.starts_with("deletion") {
                size.deleted = number;
            }
        }
    }
    sizes
}

pub(crate) fn measure(repo: &Repository, oids: &[String]) -> Result<HashMap<String, Size>, String> {
    Ok(parse(&repo.commit_sizes(oids)?))
}

fn run(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.changes.running {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let batch = tab.ui.changes.next_batch();
    if batch.is_empty() {
        return;
    }
    tab.ui.changes.running = true;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || measure(&repo, &batch),
        move |tab, result, cx| {
            if tab.epoch != epoch {
                return;
            }
            let state = &mut tab.ui.changes;
            state.running = false;
            if let Ok(sizes) = result {
                state.sizes.extend(sizes);
                cx.notify();
            }
            run(tab, cx);
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Load(oids) => {
            if !tab.ui.graph_columns.layout.shows(crate::columns::Column::Changes) {
                return;
            }
            tab.ui.changes.ask(oids);
            run(tab, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn head(f: &Fixture, revision: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", revision])).unwrap().trim().to_owned()
    }

    #[test]
    fn each_commit_is_measured_against_its_first_parent_and_an_empty_one_counts_nothing() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Empty root"]);
        let empty = head(&f, "HEAD");
        std::fs::write(f.dir.join("a.txt"), "one\ntwo\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.git(&["commit", "-qm", "Two lines"]);
        let two = head(&f, "HEAD");
        f.git(&["switch", "-qc", "side"]);
        std::fs::write(f.dir.join("a.txt"), "one\nthree\n").unwrap();
        std::fs::write(f.dir.join("b.txt"), "new\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "Side"]);
        let side = head(&f, "HEAD");
        f.git(&["switch", "-q", "main"]);
        std::fs::write(f.dir.join("c.txt"), "c\n").unwrap();
        f.git(&["add", "c.txt"]);
        f.git(&["commit", "-qm", "Main"]);
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let merge = head(&f, "HEAD");
        let before = (head(&f, "HEAD"), f.git(&["status", "--porcelain"]));
        let oids = vec![merge.clone(), side.clone(), two.clone(), empty.clone()];
        let sizes = measure(&f.repo, &oids).unwrap();
        assert_eq!(sizes[&two], Size { files: 1, added: 2, deleted: 0 });
        assert_eq!(sizes[&side], Size { files: 2, added: 2, deleted: 1 });
        assert_eq!(sizes[&merge], Size { files: 2, added: 2, deleted: 1 });
        assert_eq!(sizes[&empty], Size::default());
        assert_eq!(before, (head(&f, "HEAD"), f.git(&["status", "--porcelain"])));
        assert!(measure(&f.repo, &["HEAD".to_owned()]).is_err());
        assert!(measure(&f.repo, &["0".repeat(40)]).unwrap().is_empty());
    }

    #[test]
    fn shortstat_lines_are_read_whatever_they_leave_out() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        let output = format!("{a}\n 3 files changed, 10 insertions(+), 1 deletion(-)\n{b}\n 1 file changed, 4 deletions(-)\n{c}\n");
        let sizes = parse(&output);
        assert_eq!(sizes[&a], Size { files: 3, added: 10, deleted: 1 });
        assert_eq!(sizes[&b], Size { files: 1, added: 0, deleted: 4 });
        assert_eq!(sizes[&c], Size::default());
        assert_eq!(sizes[&a].lines(), 11);
        assert_eq!(sizes[&b].tip(), "1 file changed, 0 additions, 4 deletions");
        assert_eq!(sizes[&c].tip(), "No files changed");
    }

    #[test]
    fn rows_are_asked_for_once_and_measured_in_batches() {
        let mut state = State::default();
        let oids: Vec<String> = (0..BATCH + 5).map(|n| format!("{n:040x}")).collect();
        state.ask(oids.clone());
        state.ask(oids[..3].to_vec());
        assert!(state.unknown(oids.iter().map(String::as_str)).is_empty());
        assert_eq!(state.next_batch().len(), BATCH);
        assert_eq!(state.next_batch(), oids[BATCH..].to_vec());
        assert!(state.next_batch().is_empty());
        assert_eq!(state.unknown(["f".repeat(40).as_str()]), ["f".repeat(40)]);
    }
}
