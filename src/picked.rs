use crate::{
    Tab,
    commands::{Side, Target},
    prompt,
    view::{self, Desk, DiscardAsk, Selection, Ui},
};
use gpui::{Context, Window};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListId {
    Unstaged,
    Staged,
    Branches,
}

impl ListId {
    pub(crate) fn of(staged: bool) -> Self {
        if staged { Self::Staged } else { Self::Unstaged }
    }

    fn side(self) -> Option<Side> {
        match self {
            Self::Unstaged => Some(Side::Unstaged),
            Self::Staged => Some(Side::Staged),
            Self::Branches => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum How {
    Toggle,
    Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picked<K: Ord + Clone> {
    pub list: ListId,
    pub anchor: K,
    pub keys: BTreeSet<K>,
}

impl<K: Ord + Clone> Picked<K> {
    pub(crate) fn one(list: ListId, key: K) -> Self {
        Self { list, anchor: key.clone(), keys: BTreeSet::from([key]) }
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    pub(crate) fn contains(&self, key: &K) -> bool {
        self.keys.contains(key)
    }

    pub(crate) fn ordered(&self, rows: &[K]) -> Vec<K> {
        rows.iter().filter(|row| self.keys.contains(row)).cloned().collect()
    }

    fn spanning(list: ListId, anchor: K, rows: &[K], from: usize, to: usize) -> Self {
        let (low, high) = if from <= to { (from, to) } else { (to, from) };
        Self { list, anchor, keys: rows[low..=high].iter().cloned().collect() }
    }

    pub(crate) fn clicked(
        current: Option<&Self>,
        primary: Option<&K>,
        list: ListId,
        rows: &[K],
        key: &K,
        how: How,
    ) -> Option<(Self, K)> {
        let at = rows.iter().position(|row| row == key)?;
        let base = match current.filter(|picked| picked.list == list) {
            Some(picked) => Some(picked.clone()),
            None => primary.filter(|primary| rows.contains(primary)).map(|primary| Self::one(list, primary.clone())),
        };
        let Some(mut picked) = base else {
            return Some((Self::one(list, key.clone()), key.clone()));
        };
        match how {
            How::Toggle if picked.contains(key) && picked.len() == 1 => Some((picked, key.clone())),
            How::Toggle if picked.contains(key) => {
                picked.keys.remove(key);
                let next = rows[at..]
                    .iter()
                    .chain(rows[..at].iter().rev())
                    .find(|row| picked.keys.contains(row))
                    .cloned()?;
                let primary = match primary {
                    Some(primary) if primary != key && picked.keys.contains(primary) => primary.clone(),
                    _ => next.clone(),
                };
                if &picked.anchor == key {
                    picked.anchor = primary.clone();
                }
                Some((picked, primary))
            }
            How::Toggle => {
                picked.keys.insert(key.clone());
                picked.anchor = key.clone();
                Some((picked, key.clone()))
            }
            How::Range => {
                let from = rows.iter().position(|row| row == &picked.anchor).unwrap_or(at);
                let anchor = rows[from].clone();
                Some((Self::spanning(list, anchor, rows, from, at), key.clone()))
            }
        }
    }

    pub(crate) fn extended(current: Option<&Self>, primary: &K, list: ListId, rows: &[K], delta: isize) -> Option<(Self, K)> {
        let at = rows.iter().position(|row| row == primary)?;
        let to = at.saturating_add_signed(delta).min(rows.len() - 1);
        let anchor = current
            .filter(|picked| picked.list == list)
            .map(|picked| picked.anchor.clone())
            .filter(|anchor| rows.contains(anchor))
            .unwrap_or_else(|| primary.clone());
        let from = rows.iter().position(|row| row == &anchor).unwrap_or(at);
        Some((Self::spanning(list, anchor, rows, from, to), rows[to].clone()))
    }

    pub(crate) fn all(list: ListId, rows: &[K], primary: &K) -> Option<Self> {
        if !rows.contains(primary) {
            return None;
        }
        Some(Self { list, anchor: primary.clone(), keys: rows.iter().cloned().collect() })
    }

    pub(crate) fn retain(&mut self, rows: &[K]) -> bool {
        let listed: BTreeSet<&K> = rows.iter().collect();
        self.keys.retain(|key| listed.contains(key));
        if !self.keys.contains(&self.anchor) {
            match rows.iter().find(|row| self.keys.contains(row)) {
                Some(first) => self.anchor = first.clone(),
                None => return false,
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Click { path: PathBuf, staged: bool, how: How },
    Extend(isize),
    All,
    Stage(Vec<PathBuf>),
    Unstage(Vec<PathBuf>),
    AskDiscard(Vec<PathBuf>),
}

impl Action {
    pub(crate) fn picks(&self) -> bool {
        matches!(self, Self::Click { .. } | Self::Extend(_) | Self::All)
    }

    pub(crate) fn stages(&self) -> bool {
        matches!(self, Self::Stage(_) | Self::Unstage(_))
    }
}

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group};
    fn list(target: &Target) -> Option<ListId> {
        match target {
            Target::File { side: Side::Unstaged, .. } | Target::Files { side: Side::Unstaged, .. } => Some(ListId::Unstaged),
            Target::File { side: Side::Staged, .. } | Target::Files { side: Side::Staged, .. } => Some(ListId::Staged),
            _ => None,
        }
    }
    &[Command::new(
        "file.pick-all",
        Group::Changes,
        |_, target| match list(target) {
            Some(ListId::Staged) => "Select all staged files".into(),
            _ => "Select all unstaged files".into(),
        },
        |ui, target, _| match list(target) {
            Some(list) if rows(ui, list).len() > 1 => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, _| Some(view::Action::Picked(Action::All)),
    )
    .words(&["pick", "multiple", "every file"])
    .keys("cmd-a")]
};

pub(crate) fn rows(ui: &Ui, list: ListId) -> Vec<PathBuf> {
    let staged = list == ListId::Staged;
    ui.files.iter().filter(|file| file.staged == staged).map(|file| file.path.clone()).collect()
}

fn primary(ui: &Ui) -> Option<(ListId, &PathBuf)> {
    match &ui.selection {
        Some(Selection::File { path, staged }) => Some((ListId::of(*staged), path)),
        _ => None,
    }
}

pub(crate) fn files(ui: &Ui) -> Option<(Vec<PathBuf>, Side)> {
    let (list, path) = primary(ui)?;
    let picked = ui.picked.as_ref().filter(|picked| picked.list == list && picked.len() > 1 && picked.contains(path))?;
    Some((picked.ordered(&rows(ui, list)), list.side()?))
}

pub(crate) fn kept_by(ui: &Ui, selection: &Selection) -> bool {
    match (selection, &ui.picked) {
        (Selection::File { path, staged }, Some(picked)) => picked.list == ListId::of(*staged) && picked.contains(path),
        _ => false,
    }
}

pub(crate) fn refresh(ui: &mut Ui) {
    let Some(mut picked) = ui.picked.take() else { return };
    let listed = rows(ui, picked.list);
    if picked.retain(&listed) {
        ui.picked = Some(picked);
    }
}

fn choose(tab: &mut Tab, picked: Picked<PathBuf>, path: PathBuf, window: &mut Window, cx: &mut Context<Tab>) {
    let selection = Selection::File { path, staged: picked.list == ListId::Staged };
    if tab.ui.selection.as_ref() != Some(&selection) {
        view::focus(&tab.ui, window);
        tab.dispatch(view::Action::Select(selection.clone()), window, cx);
    }
    if tab.ui.selection.as_ref() == Some(&selection) {
        tab.ui.picked = Some(picked);
    }
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Click { path, staged, how } => {
            let list = ListId::of(staged);
            let listed = rows(&tab.ui, list);
            let primary = primary(&tab.ui).filter(|(at, _)| *at == list).map(|(_, path)| path.clone());
            let current = tab.ui.picked.as_ref().filter(|_| primary.is_some());
            if let Some((picked, path)) = Picked::clicked(current, primary.as_ref(), list, &listed, &path, how) {
                choose(tab, picked, path, window, cx);
            }
        }
        Action::Extend(delta) => {
            let Some((list, path)) = primary(&tab.ui).map(|(list, path)| (list, path.clone())) else { return };
            let listed = rows(&tab.ui, list);
            let Some((picked, to)) = Picked::extended(tab.ui.picked.as_ref(), &path, list, &listed, delta) else {
                return;
            };
            let unstaged = tab.ui.file_ranges().0.len();
            if let Some(at) = listed.iter().position(|row| row == &to) {
                let row = match list {
                    ListId::Staged => 2 + unstaged + at,
                    _ => 1 + at,
                };
                tab.ui.file_scroll.scroll_to_item(row, gpui::ScrollStrategy::Center);
            }
            choose(tab, picked, to, window, cx);
        }
        Action::All => {
            let Some((list, path)) = primary(&tab.ui).map(|(list, path)| (list, path.clone())) else { return };
            if let Some(picked) = Picked::all(list, &rows(&tab.ui, list), &path) {
                tab.ui.picked = Some(picked);
                cx.notify();
            }
        }
        Action::Stage(paths) => tab.mutate(cx, move |repo| repo.stage_files(&paths)),
        Action::Unstage(paths) => tab.mutate(cx, move |repo| repo.unstage_files(&paths)),
        Action::AskDiscard(paths) => {
            if !tab.ui.can_edit_index() || tab.ui.prompt.open() {
                return;
            }
            let Some(ask) = discard_ask(&tab.ui, paths) else { return };
            prompt::ask(&mut tab.ui, prompt::Kind::Discard(ask), cx);
        }
    }
}

fn discard_ask(ui: &Ui, paths: Vec<PathBuf>) -> Option<DiscardAsk> {
    let mut untracked = BTreeSet::new();
    for path in &paths {
        let row = ui.files.iter().find(|file| !file.staged && &file.path == path)?;
        if row.status == "Untracked" {
            untracked.insert(path.clone());
        }
    }
    (!paths.is_empty()).then_some(DiscardAsk::Files { paths, untracked })
}

pub(crate) const NAMED: usize = 8;

pub(crate) fn describe_discard(paths: &[PathBuf], untracked: &BTreeSet<PathBuf>) -> prompt::Spec {
    let count = paths.len();
    let mut spec = prompt::Spec::new(format!("Discard changes to {count} files?"), format!("Discard {count} files"));
    for path in paths.iter().take(NAMED) {
        spec = spec.line(shown(path));
    }
    if count > NAMED {
        spec = spec.line(format!("and {} more", count - NAMED));
    }
    let consequence = match untracked.len() {
        0 => "Unstaged edits to these files are removed. Staged changes are kept.".to_owned(),
        n if n == count => "These files are not tracked, so discarding deletes them.".to_owned(),
        1 => "Unstaged edits to these files are removed, and the 1 untracked file is deleted. Staged changes are kept.".to_owned(),
        n => format!(
            "Unstaged edits to these files are removed, and the {n} untracked files are deleted. Staged changes are kept."
        ),
    };
    spec.line(consequence).line("Restore, in the notice that follows, brings them back.")
}

fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn file_tip(action: &str, count: usize, key: &str) -> String {
    format!("{action} {count} files ({key})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<&'static str> {
        vec!["a", "b", "c", "d", "e"]
    }

    fn keys(picked: &Picked<&'static str>) -> Vec<&'static str> {
        picked.ordered(&rows())
    }

    #[test]
    fn a_shift_click_picks_the_range_from_the_anchor_in_either_direction() {
        let rows = rows();
        let (picked, primary) = Picked::clicked(None, Some(&"b"), ListId::Unstaged, &rows, &"d", How::Range).unwrap();
        assert_eq!((keys(&picked), primary, picked.anchor), (vec!["b", "c", "d"], "d", "b"));
        let (picked, primary) =
            Picked::clicked(Some(&picked), Some(&"d"), ListId::Unstaged, &rows, &"a", How::Range).unwrap();
        assert_eq!((keys(&picked), primary, picked.anchor), (vec!["a", "b"], "a", "b"));
        let (picked, _) = Picked::clicked(None, None, ListId::Unstaged, &rows, &"c", How::Range).unwrap();
        assert_eq!(keys(&picked), vec!["c"]);
    }

    #[test]
    fn a_command_click_adds_and_removes_rows_and_keeps_one() {
        let rows = rows();
        let (picked, primary) = Picked::clicked(None, Some(&"a"), ListId::Staged, &rows, &"c", How::Toggle).unwrap();
        assert_eq!((keys(&picked), primary, picked.anchor), (vec!["a", "c"], "c", "c"));
        let (picked, primary) = Picked::clicked(Some(&picked), Some(&"c"), ListId::Staged, &rows, &"e", How::Toggle).unwrap();
        assert_eq!((keys(&picked), primary), (vec!["a", "c", "e"], "e"));
        let (picked, primary) = Picked::clicked(Some(&picked), Some(&"e"), ListId::Staged, &rows, &"c", How::Toggle).unwrap();
        assert_eq!((keys(&picked), primary, picked.anchor), (vec!["a", "e"], "e", "e"));
        let (picked, primary) = Picked::clicked(Some(&picked), Some(&"e"), ListId::Staged, &rows, &"e", How::Toggle).unwrap();
        assert_eq!((keys(&picked), primary), (vec!["a"], "a"));
        let (picked, primary) = Picked::clicked(Some(&picked), Some(&"a"), ListId::Staged, &rows, &"a", How::Toggle).unwrap();
        assert_eq!((keys(&picked), primary), (vec!["a"], "a"));
    }

    #[test]
    fn a_pick_never_spans_two_lists() {
        let rows = rows();
        let staged = Picked::all(ListId::Staged, &rows, &"b").unwrap();
        let (picked, primary) = Picked::clicked(Some(&staged), None, ListId::Unstaged, &rows, &"d", How::Toggle).unwrap();
        assert_eq!((picked.list, keys(&picked), primary), (ListId::Unstaged, vec!["d"], "d"));
        assert!(Picked::clicked(None, None, ListId::Unstaged, &rows, &"z", How::Toggle).is_none());
    }

    #[test]
    fn shift_arrows_grow_and_shrink_the_range_from_the_anchor() {
        let rows = rows();
        let (picked, to) = Picked::extended(None, &"b", ListId::Unstaged, &rows, 1).unwrap();
        assert_eq!((keys(&picked), to), (vec!["b", "c"], "c"));
        let (picked, to) = Picked::extended(Some(&picked), &"c", ListId::Unstaged, &rows, 1).unwrap();
        assert_eq!((keys(&picked), to), (vec!["b", "c", "d"], "d"));
        let (picked, to) = Picked::extended(Some(&picked), &"d", ListId::Unstaged, &rows, -1).unwrap();
        let (picked, to) = Picked::extended(Some(&picked), &to, ListId::Unstaged, &rows, -1).unwrap();
        let (picked, to) = Picked::extended(Some(&picked), &to, ListId::Unstaged, &rows, -1).unwrap();
        assert_eq!((keys(&picked), to), (vec!["a", "b"], "a"));
        let (picked, to) = Picked::extended(Some(&picked), &to, ListId::Unstaged, &rows, -1).unwrap();
        assert_eq!((keys(&picked), to), (vec!["a", "b"], "a"));
    }

    #[test]
    fn a_pick_survives_a_refresh_that_keeps_its_rows_and_shrinks_with_one_that_removes_some() {
        let mut picked = Picked::all(ListId::Unstaged, &rows(), &"a").unwrap();
        assert!(picked.retain(&["e", "d", "c", "b", "a", "f"]));
        assert_eq!(picked.len(), 5);
        assert!(picked.retain(&["b", "d", "f"]));
        assert_eq!((picked.ordered(&["b", "d", "f"]), picked.anchor), (vec!["b", "d"], "b"));
        assert!(!picked.retain(&["f"]));
    }

    #[test]
    fn the_discard_prompt_names_eight_files_and_counts_the_rest() {
        let paths: Vec<PathBuf> = (1..=11).map(|i| PathBuf::from(format!("notes/{i}.txt"))).collect();
        let untracked = BTreeSet::from([paths[0].clone(), paths[3].clone()]);
        let spec = describe_discard(&paths, &untracked);
        assert_eq!(spec.title, "Discard changes to 11 files?");
        assert_eq!(spec.confirm.as_ref(), "Discard 11 files");
        assert_eq!(spec.lines.len(), 11);
        assert_eq!(spec.lines[0], "notes/1.txt");
        assert_eq!(spec.lines[7], "notes/8.txt");
        assert_eq!(spec.lines[8], "and 3 more");
        assert!(spec.lines[9].contains("the 2 untracked files are deleted"), "{}", spec.lines[9]);
        let spec = describe_discard(&paths[..2], &BTreeSet::new());
        assert_eq!(
            spec.lines,
            [
                "notes/1.txt",
                "notes/2.txt",
                "Unstaged edits to these files are removed. Staged changes are kept.",
                "Restore, in the notice that follows, brings them back."
            ]
        );
        let spec = describe_discard(&paths[..1], &BTreeSet::from([paths[0].clone()]));
        assert!(spec.lines[1].starts_with("These files are not tracked"));
    }
}
