use crate::{
    Tab,
    commands::{Entry, Note, Target},
    repository::{Repository, Run, validate_oid},
    sync::Strategy,
    view::{Menu, Selection, Ui},
};
use gpui::Context;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Forecast {
    Checking,
    Clean,
    Conflicts(Vec<String>),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wording {
    Merge,
    Rebase,
}

type Pair = (String, String);

fn key(ours: &str, theirs: &str) -> Pair {
    if ours <= theirs { (ours.to_owned(), theirs.to_owned()) } else { (theirs.to_owned(), ours.to_owned()) }
}

const KEPT: usize = 32;
const NAMED: usize = 3;
const TIPPED: usize = 20;

#[derive(Debug, Default)]
pub(crate) struct State {
    known: Vec<(Pair, Forecast)>,
}

impl State {
    pub(crate) fn of(&self, ours: &str, theirs: &str) -> Option<&Forecast> {
        let wanted = key(ours, theirs);
        self.known.iter().find(|(pair, _)| *pair == wanted).map(|(_, forecast)| forecast)
    }

    pub(crate) fn settled(&self) -> bool {
        self.known.iter().all(|(_, forecast)| *forecast != Forecast::Checking)
    }

    fn begin(&mut self, ours: &str, theirs: &str) -> bool {
        if self.of(ours, theirs).is_some() {
            return false;
        }
        if self.known.len() >= KEPT {
            if let Some(at) = self.known.iter().position(|(_, forecast)| *forecast != Forecast::Checking) {
                self.known.remove(at);
            }
        }
        self.known.push((key(ours, theirs), Forecast::Checking));
        true
    }

    fn land(&mut self, ours: &str, theirs: &str, forecast: Forecast) -> bool {
        let wanted = key(ours, theirs);
        match self.known.iter_mut().find(|(pair, now)| *pair == wanted && *now == Forecast::Checking) {
            Some(slot) => {
                slot.1 = forecast;
                true
            }
            None => false,
        }
    }
}

pub(crate) fn between(repo: &Repository, ours: &str, theirs: &str) -> Result<Forecast, String> {
    validate_oid(ours)?;
    validate_oid(theirs)?;
    let output = repo
        .read_command()
        .args(["merge-tree", "--write-tree", "--name-only", "--no-messages", "-z", "--end-of-options", ours, theirs])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    let names = || {
        output
            .stdout
            .split(|&b| b == 0)
            .skip(1)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect::<Vec<_>>()
    };
    match output.status.code() {
        Some(0) => Ok(Forecast::Clean),
        Some(1) => Ok(Forecast::Conflicts(names())),
        _ => {
            let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            Err(if said.is_empty() { "Git could not test the merge.".to_owned() } else { said })
        }
    }
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

fn listed(names: &[String], most: usize) -> String {
    let shown = names.iter().take(most).map(String::as_str).collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(most) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

pub(crate) fn sentence(forecast: &Forecast, wording: Wording) -> Option<String> {
    match forecast {
        Forecast::Checking => Some("Checking for conflicts…".to_owned()),
        Forecast::Clean => Some("No conflicts expected".to_owned()),
        Forecast::Conflicts(names) => {
            let verb = match wording {
                Wording::Merge => "Will conflict",
                Wording::Rebase => "May conflict",
            };
            Some(format!("{verb} in {}: {}", files(names.len()), listed(names, NAMED)))
        }
        Forecast::Unknown => None,
    }
}

pub(crate) fn line(ui: &Ui, ours: &str, theirs: &str, wording: Wording) -> Option<String> {
    sentence(ui.forecast.of(ours, theirs)?, wording)
}

pub(crate) fn conflicting(ui: &Ui, ours: &str, theirs: &str) -> bool {
    matches!(ui.forecast.of(ours, theirs), Some(Forecast::Conflicts(_)))
}

pub(crate) fn request(tab: &mut Tab, ours: String, theirs: String, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    if ours == theirs || validate_oid(&ours).is_err() || validate_oid(&theirs).is_err() {
        return;
    }
    if !tab.ui.forecast.begin(&ours, &theirs) {
        return;
    }
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || {
            let forecast = between(&repo, &ours, &theirs).unwrap_or(Forecast::Unknown);
            (ours, theirs, forecast)
        },
        move |tab, (ours, theirs, forecast), cx| {
            if tab.epoch == epoch && tab.ui.forecast.land(&ours, &theirs, forecast) {
                cx.notify();
            }
        },
    );
}

pub(crate) const MERGE_MENUS: [&str; 4] = ["branch.merge", "branch.merge-remote", "tag.merge", "commit.merge"];

fn merging(ui: &Ui, target: &Target) -> Option<Pair> {
    if ui.detached {
        return None;
    }
    let head = ui.head.as_deref()?;
    let theirs = match target {
        Target::Commit(oid) => oid.as_str(),
        Target::Branch(_) | Target::RemoteBranch(_) | Target::Tag(_) => ui.refs.oid_of(target)?,
        _ => return None,
    };
    (theirs != head).then(|| (head.to_owned(), theirs.to_owned()))
}

fn wanted(ui: &Ui, target: &Target) -> Vec<Pair> {
    merging(ui, target)
        .into_iter()
        .chain(crate::drag::forecast_ends(ui, target).map(|(_, ours, theirs, _)| (ours, theirs)))
        .collect()
}

pub(crate) fn menu_opened(tab: &mut Tab, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let pairs = match &ui.menu {
        Some(Menu::Object { target, .. }) => wanted(ui, target),
        Some(Menu::CommitActions) => match &ui.selection {
            Some(Selection::Commit(oid)) => wanted(ui, &Target::Commit(oid.clone())),
            _ => Vec::new(),
        },
        Some(Menu::Pair { branch, remote, .. }) => {
            [Target::Branch(branch.clone()), Target::RemoteBranch(remote.clone())].iter().flat_map(|target| wanted(ui, target)).collect()
        }
        _ => Vec::new(),
    };
    for (ours, theirs) in pairs {
        request(tab, ours, theirs, cx);
    }
}

fn note(ui: &Ui, ours: &str, theirs: &str, wording: Wording) -> Option<Entry> {
    let forecast = ui.forecast.of(ours, theirs)?;
    let title = sentence(forecast, wording)?;
    let (tip, warn) = match forecast {
        Forecast::Conflicts(names) => (Some(listed(names, TIPPED)), true),
        _ => (None, false),
    };
    Some(Entry::note("forecast", title, Note { tip, warn }))
}

pub(crate) fn annotate(ui: &Ui, target: &Target, entries: &mut Vec<Entry>) {
    if let Some((ours, theirs)) = merging(ui, target)
        && let Some(parent) =
            entries.iter_mut().find(|entry| MERGE_MENUS.contains(&entry.id) && entry.disabled.is_none() && !entry.children.is_empty())
        && let Some(row) = note(ui, &ours, &theirs, Wording::Merge)
    {
        parent.children.push(row);
    }
    if let Some((wording, ours, theirs, after)) = crate::drag::forecast_ends(ui, target)
        && let Some(at) = entries.iter().position(|entry| entry.id == after && entry.action.is_some())
        && let Some(row) = note(ui, &ours, &theirs, wording)
    {
        entries.insert(at + 1, row);
    }
}

pub(crate) struct Upstream {
    pub(crate) branch: String,
    pub(crate) upstream: String,
    pub(crate) ours: String,
    pub(crate) theirs: String,
}

pub(crate) fn diverged(ui: &Ui) -> Option<Upstream> {
    if ui.detached || ui.head.is_none() {
        return None;
    }
    let item = ui.refs.current(&ui.branch)?;
    if item.ahead == 0 || item.behind == 0 || item.gone {
        return None;
    }
    let upstream = item.upstream.as_deref()?;
    let theirs = ui.refs.oid_of(&Target::RemoteBranch(upstream.into()))?;
    Some(Upstream { branch: ui.branch.clone(), upstream: upstream.to_owned(), ours: item.oid.clone()?, theirs: theirs.to_owned() })
}

pub(crate) fn refs_loaded(tab: &mut Tab, cx: &mut Context<Tab>) {
    if let Some(pair) = diverged(&tab.ui) {
        request(tab, pair.ours, pair.theirs, cx);
    }
}

pub(crate) fn pull_warning(ui: &Ui) -> Option<String> {
    let pair = diverged(ui)?;
    let Some(Forecast::Conflicts(names)) = ui.forecast.of(&pair.ours, &pair.theirs) else {
        return None;
    };
    let (branch, upstream, count, names) = (&pair.branch, &pair.upstream, files(names.len()), listed(names, TIPPED));
    Some(match ui.sync.chosen().strategy {
        Strategy::Rebase => format!("Rebasing {branch} onto {upstream} may conflict in {count}: {names}"),
        _ => format!("Merging {upstream} into {branch} will conflict in {count}: {names}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().to_owned()
    }

    fn commit(f: &Fixture, file: &str, text: &str, subject: &str) {
        fs::write(f.dir.join(file), text).unwrap();
        f.git(&["add", "--", file]);
        f.git(&["commit", "-qm", subject]);
    }

    fn looked(f: &Fixture) -> Vec<Vec<u8>> {
        vec![
            f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            f.git(&["symbolic-ref", "HEAD"]),
            f.git(&["ls-files", "--stage"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
        ]
    }

    fn trail() -> (Fixture, String, String) {
        let f = Fixture::new();
        commit(&f, "notes.txt", "Pack\nWalk\nRest\n", "Start the notes");
        commit(&f, "map space.txt", "North\nSouth\n", "Draw the map");
        commit(&f, "camp.txt", "Tents\n", "Pitch the camp");
        f.git(&["switch", "-qc", "ridge"]);
        commit(&f, "notes.txt", "Pack\nClimb\nRest\n", "Climb instead");
        commit(&f, "map space.txt", "North\nRidge\n", "Mark the ridge");
        commit(&f, "lake.txt", "Swim\n", "Find the lake");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "notes.txt", "Pack\nSwim\nRest\n", "Swim instead");
        commit(&f, "map space.txt", "North\nCoast\n", "Mark the coast");
        let (main, ridge) = (rev(&f, "main"), rev(&f, "ridge"));
        (f, main, ridge)
    }

    #[test]
    fn a_forecast_names_the_files_a_merge_would_conflict_in_and_touches_no_ref_index_or_file() {
        let (f, main, ridge) = trail();
        fs::write(f.dir.join("camp.txt"), "Tents and a fire\n").unwrap();
        fs::write(f.dir.join("loose.txt"), "Untracked\n").unwrap();
        f.git(&["add", "--", "loose.txt"]);
        let before = looked(&f);
        let forecast = between(&f.repo, &main, &ridge).unwrap();
        assert_eq!(forecast, Forecast::Conflicts(vec!["map space.txt".into(), "notes.txt".into()]));
        assert_eq!(between(&f.repo, &ridge, &main).unwrap(), forecast);
        assert_eq!(looked(&f), before);
        assert_eq!(fs::read_to_string(f.dir.join("camp.txt")).unwrap(), "Tents and a fire\n");
        assert!(!f.dir.join(".git/MERGE_HEAD").exists());
        f.git(&["stash", "-q"]);
        let merged = f.repo.command().args(["merge", "--no-edit", "ridge"]).run().unwrap();
        assert!(!merged.status.success());
        let conflicted = f.git(&["diff", "--name-only", "--diff-filter=U"]);
        assert_eq!(String::from_utf8(conflicted).unwrap(), "map space.txt\nnotes.txt\n");
    }

    #[test]
    fn a_merge_that_changes_other_files_is_forecast_clean_and_unrelated_history_cannot_be_forecast() {
        let (f, main, _) = trail();
        f.git(&["switch", "-qc", "lake", "main~2"]);
        commit(&f, "lake.txt", "Swim\n", "Find the lake");
        let lake = rev(&f, "lake");
        f.git(&["switch", "-q", "main"]);
        let before = looked(&f);
        assert_eq!(between(&f.repo, &main, &lake).unwrap(), Forecast::Clean);
        assert_eq!(looked(&f), before);
        assert_eq!(between(&f.repo, &main, &main).unwrap(), Forecast::Clean);
        f.git(&["switch", "-q", "--orphan", "alone"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Start alone"]);
        let alone = rev(&f, "alone");
        f.git(&["switch", "-q", "main"]);
        let before_alone = looked(&f);
        let err = between(&f.repo, &main, &alone).unwrap_err();
        assert!(err.contains("unrelated histories"), "{err}");
        assert_eq!(looked(&f), before_alone);
        assert!(between(&f.repo, "main", &alone).is_err());
        assert!(between(&f.repo, &main, "--output=x").is_err());
    }

    #[test]
    fn a_forecast_is_kept_per_pair_of_commits_so_a_moved_branch_is_checked_again() {
        let (f, main, ridge) = trail();
        let mut state = State::default();
        assert!(state.begin(&main, &ridge));
        assert!(!state.begin(&ridge, &main));
        assert_eq!(state.of(&ridge, &main), Some(&Forecast::Checking));
        assert!(state.land(&main, &ridge, between(&f.repo, &main, &ridge).unwrap()));
        assert!(!state.land(&main, &ridge, Forecast::Clean));
        assert!(matches!(state.of(&main, &ridge), Some(Forecast::Conflicts(names)) if names.len() == 2));
        commit(&f, "notes.txt", "Pack\nClimb\nRest\n", "Agree with the ridge");
        commit(&f, "map space.txt", "North\nRidge\n", "Agree on the map");
        let moved = rev(&f, "main");
        assert_eq!(state.of(&moved, &ridge), None);
        assert!(!state.land(&moved, &ridge, Forecast::Clean));
        assert!(state.begin(&moved, &ridge));
        assert!(state.land(&moved, &ridge, between(&f.repo, &moved, &ridge).unwrap()));
        assert_eq!(state.of(&moved, &ridge), Some(&Forecast::Clean));
        for n in 0..KEPT * 2 {
            let other = format!("{n:040x}");
            state.begin(&main, &other);
            state.land(&main, &other, Forecast::Clean);
        }
        assert_eq!(state.known.len(), KEPT);
        assert_eq!(state.of(&main, &ridge), None);
    }

    #[test]
    fn the_wording_says_will_for_a_merge_and_may_for_a_rebase_and_shortens_long_lists() {
        let two = Forecast::Conflicts(vec!["map.txt".into(), "notes.txt".into()]);
        assert_eq!(sentence(&two, Wording::Merge).unwrap(), "Will conflict in 2 files: map.txt, notes.txt");
        assert_eq!(sentence(&two, Wording::Rebase).unwrap(), "May conflict in 2 files: map.txt, notes.txt");
        let one = Forecast::Conflicts(vec!["camp.txt".into()]);
        assert_eq!(sentence(&one, Wording::Merge).unwrap(), "Will conflict in 1 file: camp.txt");
        let many = Forecast::Conflicts((1..=5).map(|n| format!("f{n}.txt")).collect());
        assert_eq!(sentence(&many, Wording::Merge).unwrap(), "Will conflict in 5 files: f1.txt, f2.txt, f3.txt and 2 more");
        assert_eq!(sentence(&Forecast::Clean, Wording::Rebase).unwrap(), "No conflicts expected");
        assert_eq!(sentence(&Forecast::Checking, Wording::Merge).unwrap(), "Checking for conflicts…");
        assert_eq!(sentence(&Forecast::Unknown, Wording::Merge), None);
        let names: Vec<String> = (1..=25).map(|n| format!("f{n}")).collect();
        assert!(listed(&names, TIPPED).ends_with("f20 and 5 more"));
    }
}
