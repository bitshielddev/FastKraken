use crate::{
    Tab,
    commands::{Availability, Command, Group, on_repo},
    guard::Seen,
    notice::{Failure, Notice, Outcome},
    operation::{self, Kind, Stashing},
    repository::Repository,
    view::{self, Tone},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    sync::{Arc, Mutex},
};

pub(crate) const RETRY: &str = "Stash changes and retry";
const GITS: &str = "autostash";

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "repo.stash-in-the-way",
    Group::Stash,
    |ui, _| {
        if ui.autostash.always {
            "Ask before stashing changes that are in the way"
        } else {
            "Stash automatically when changes are in the way"
        }
        .into()
    },
    |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Autostash(Action::Toggle)),
)
.words(&["autostash", "auto stash", "stash and retry", "local changes would be overwritten"])
.icon("stash")];

#[derive(Clone, Debug)]
pub(crate) enum Retry {
    Switch(OsString),
    Branch(Box<crate::branch::Move>),
    Pick(Box<crate::pick::Ask>),
    Picks(Box<crate::pick::Series>),
    Revert(Box<crate::reverse::Ask>),
    Rebase(Box<crate::rebase::Ask>),
    Merge { source: crate::merge::Source, mode: crate::merge::Mode, expected: String, seen: Seen },
    Pull { head: String, strategy: crate::sync::Strategy },
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Retry(Retry),
    Toggle,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) always: bool,
}

impl State {
    pub(crate) fn new(always: bool) -> Self {
        Self { always }
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Toggle => {
            let always = !tab.ui.autostash.always;
            tab.ui.autostash.always = always;
            crate::settings::Settings::update(cx, |s| s.stash_in_the_way = always);
            let (title, detail) = if always {
                ("Changes in the way are stashed automatically", "They are put back once the operation has run.")
            } else {
                ("FastKraken asks before stashing changes in the way", "")
            };
            tab.ui.notices.show(Notice::new(Tone::Info, title, detail));
        }
        Action::Retry(retry) => {
            if tab.ui.busy() || tab.ui.dialog_open() {
                return;
            }
            match retry {
                Retry::Switch(name) => crate::refs::switch_to(tab, name, true, window, cx),
                Retry::Branch(step) => crate::branch::run(tab, *step, true, cx),
                Retry::Pick(ask) => crate::pick::run(tab, *ask, true, cx),
                Retry::Picks(series) => crate::pick::run_series(tab, *series, true, cx),
                Retry::Revert(ask) => crate::reverse::revert(tab, *ask, true, cx),
                Retry::Rebase(ask) => crate::rebase::run(tab, *ask, true, cx),
                Retry::Merge { source, mode, expected, seen } => crate::merge::start(tab, source, mode, expected, seen, true, cx),
                Retry::Pull { head, strategy } => crate::sync::pull_stashing(tab, head, strategy, cx),
            }
        }
    }
    cx.notify();
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Kept(Arc<Mutex<Option<Stashing>>>);

impl Kept {
    fn put(&self, stashing: Stashing) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(stashing);
    }

    pub(crate) fn take(&self) -> Option<Stashing> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub(crate) fn remember(&self, ui: &mut view::Ui) {
        if let Some(stashing) = self.take() {
            ui.operation.stash = Some(stashing);
        }
    }
}

pub(crate) fn in_the_way(failure: &Failure) -> bool {
    let said = |text: &str| {
        text.contains("Your local changes to the following files would be overwritten")
            || text.contains("your local changes would be overwritten by")
            || text.contains("cannot rebase: You have unstaged changes")
            || text.contains("cannot rebase: Your index contains uncommitted changes")
    };
    said(&failure.text) || failure.output.as_deref().is_some_and(said)
}

fn offer(failure: Failure, retry: Retry) -> Failure {
    failure.offer(RETRY, view::Action::Autostash(Action::Retry(retry)))
}

fn also(mut failure: Failure, sentence: &str) -> Failure {
    failure.text = format!("{}\n{sentence}", failure.text.trim_end());
    failure
}

fn files_need(count: usize) -> String {
    match count {
        1 => "1 file needs".to_owned(),
        n => format!("{n} files need"),
    }
}

fn capitalised(text: &str) -> String {
    let mut letters = text.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

pub(crate) fn lowered(title: &str) -> String {
    let mut letters = title.chars();
    letters.next().map(|first| first.to_lowercase().chain(letters).collect()).unwrap_or_default()
}

fn newest(repo: &Repository) -> Option<String> {
    repo.resolve(OsStr::new("refs/stash")).ok()
}

fn applied_conflicts(repo: &Repository) -> usize {
    match operation::snapshot(repo) {
        Ok(Some(now)) if now.kind == Kind::Applied => now.conflicts.len(),
        _ => 0,
    }
}

fn save(repo: &Repository, doing: &str) -> Result<Stashing, Failure> {
    repo.expect_no_operation()?;
    let before = newest(repo);
    let title = format!("FastKraken: before {doing}");
    let mut push = repo.stash_push();
    push.arg("--include-untracked").arg(format!("--message={title}"));
    crate::stash::push_changes(&mut push)?;
    let oid = newest(repo)
        .filter(|oid| Some(oid) != before.as_ref())
        .ok_or_else(|| Failure::from("Git did not save a stash, so nothing was changed.".to_owned()))?;
    Ok(Stashing { oid, title, pop: true })
}

pub(crate) struct Around<'a> {
    pub doing: String,
    pub stash: bool,
    pub kept: &'a Kept,
}

pub(crate) fn around(
    repo: &Repository,
    how: Around,
    retry: impl FnOnce() -> Retry,
    run: impl Fn(&Repository) -> Result<Outcome, Failure>,
    done: impl FnOnce(&Outcome) -> Option<String>,
) -> Result<Outcome, Failure> {
    let refused = match run(repo) {
        Err(failure) if in_the_way(&failure) => failure,
        other => return other,
    };
    if !how.stash {
        return Err(offer(refused, retry()));
    }
    let stash = save(repo, &how.doing)?;
    match run(repo) {
        Ok(outcome) => Ok(restore(repo, outcome, &stash, how.kept, done)?),
        Err(failure) => Err(put_back(repo, failure, &stash)),
    }
}

fn put_back(repo: &Repository, failure: Failure, stash: &Stashing) -> Failure {
    let title = &stash.title;
    if let Ok(Some(now)) = operation::snapshot(repo) {
        let noun = now.kind.noun();
        return also(failure, &format!("Your local changes are in the stash \"{title}\". Pop it once the {noun} is finished."));
    }
    match repo.pop_stash(&stash.oid) {
        Ok(_) => also(failure, "Your local changes were put back."),
        Err(err) => also(
            failure,
            &format!("Your local changes are in the stash \"{title}\", and could not be put back: {}", err.text),
        ),
    }
}

fn restore(
    repo: &Repository,
    mut outcome: Outcome,
    stash: &Stashing,
    kept: &Kept,
    done: impl FnOnce(&Outcome) -> Option<String>,
) -> Result<Outcome, Failure> {
    let title = &stash.title;
    let done = done(&outcome);
    let refused = match repo.pop_stash(&stash.oid) {
        Ok(_) => {
            match done {
                Some(done) => outcome.title = format!("Stashed your changes, {done} and restored them"),
                None => outcome.detail = sentences(&outcome.detail, "Your local changes were stashed first and put back."),
            }
            return Ok(outcome);
        }
        Err(refused) => refused,
    };
    let headline = match (outcome.title.is_empty(), done) {
        (true, Some(done)) => capitalised(&done),
        _ => outcome.title.clone(),
    };
    let conflicts = applied_conflicts(repo);
    if conflicts > 0 {
        kept.put(stash.clone());
        let mut failure = Failure::from(format!(
            "{headline} — Your changes did not go back cleanly. {} resolving, and they stay in the stash \"{title}\" until you drop it.",
            files_need(conflicts)
        ))
        .output(refused.output.unwrap_or(refused.text));
        failure.actions = outcome.actions;
        return Err(failure);
    }
    if !outcome.title.is_empty() || headline.is_empty() {
        outcome.detail = sentences(
            &outcome.detail,
            &format!("Your local changes are still in the stash \"{title}\": {}", refused.text),
        );
    } else {
        outcome.title = headline;
        outcome.detail = format!("Your local changes are still in the stash \"{title}\": {}", refused.text);
    }
    Ok(outcome.sticky())
}

fn sentences(first: &str, then: &str) -> String {
    match first.trim() {
        "" => then.to_owned(),
        first => format!("{first} {then}"),
    }
}

pub(crate) fn by_git<T>(
    repo: &Repository,
    stash: bool,
    kept: &Kept,
    retry: impl FnOnce() -> Retry,
    run: impl Fn(bool) -> Result<T, Failure>,
    done: impl FnOnce(&T) -> String,
) -> Result<(T, bool), Failure> {
    let refused = match run(false) {
        Err(failure) if in_the_way(&failure) => failure,
        other => return other.map(|result| (result, false)),
    };
    if !stash {
        return Err(offer(refused, retry()));
    }
    let before = newest(repo);
    match run(true) {
        Ok(result) => {
            settle(repo, before.as_deref(), kept, &done(&result))?;
            Ok((result, true))
        }
        Err(failure) => Err(match operation::snapshot(repo) {
            Ok(Some(now)) if now.kind != Kind::Applied => also(
                failure,
                &format!(
                    "Your local changes were stashed first. Git puts them back when the {} is finished or aborted.",
                    now.kind.noun()
                ),
            ),
            _ => failure,
        }),
    }
}

pub(crate) fn pending(repo: &Repository) -> Option<Option<String>> {
    let git = repo.git_dir();
    let waiting = ["MERGE_AUTOSTASH", "rebase-merge/autostash", "rebase-apply/autostash"]
        .iter()
        .any(|marker| git.join(marker).is_file());
    waiting.then(|| newest(repo))
}

pub(crate) fn settled(repo: &Repository, pending: Option<Option<String>>, kept: &Kept, outcome: Outcome) -> Result<Outcome, Failure> {
    let Some(before) = pending else {
        return Ok(outcome);
    };
    if self::pending(repo).is_some() {
        return Ok(outcome);
    }
    let done = if outcome.title.is_empty() { "Done".to_owned() } else { outcome.title.clone() };
    if let Err(mut failure) = settle(repo, before.as_deref(), kept, &done) {
        failure.actions = outcome.actions;
        return Err(failure);
    }
    Ok(outcome)
}

fn settle(repo: &Repository, before: Option<&str>, kept: &Kept, done: &str) -> Result<(), Failure> {
    let Ok(entries) = repo.stashes() else {
        return Ok(());
    };
    let Some(top) = entries.first().filter(|top| Some(top.oid.as_str()) != before && top.message == GITS) else {
        return Ok(());
    };
    let conflicts = applied_conflicts(repo);
    if conflicts == 0 {
        return Err(Failure::from(format!(
            "{done} — Git could not put your local changes back, so they are in the stash \"{GITS}\"."
        )));
    }
    kept.put(Stashing { oid: top.oid.clone(), title: top.title.clone(), pop: true });
    Err(Failure::from(format!(
        "{done} — Your changes did not go back cleanly. {} resolving, and they stay in the stash \"{GITS}\" until you drop it.",
        files_need(conflicts)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::{commit, oid};
    use crate::repository::tests::Fixture;
    use std::fs;

    pub(crate) fn apart() -> Fixture {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\nRidge\nCreek\nLake\nSummit\n", "Base");
        f.git(&["switch", "-qc", "other"]);
        commit(&f, "trail.txt", "Trail north\nRidge\nCreek\nLake\nSummit\n", "Turn north");
        f.git(&["switch", "-q", "main"]);
        f
    }

    fn switch(repo: &Repository) -> Result<Outcome, Failure> {
        crate::repository::checked(repo.command().args(["switch", "--no-guess", "--", "other"]))
            .map(|_| Outcome::detail("Now on other."))
            .map_err(Failure::from)
    }

    fn around_switch(f: &Fixture, stash: bool, kept: &Kept) -> Result<Outcome, Failure> {
        let how = Around { doing: "switching to other".to_owned(), stash, kept };
        around(&f.repo, how, || Retry::Switch("other".into()), switch, |_| Some("switched to other".to_owned()))
    }

    fn listed(f: &Fixture) -> String {
        String::from_utf8(f.git(&["stash", "list", "--format=%gs"])).unwrap()
    }

    #[test]
    fn changes_in_the_way_are_offered_a_retry_that_stashes_switches_and_restores_them() {
        let f = apart();
        fs::write(f.dir.join("trail.txt"), "Trail\nRidge\nCreek\nLake\nSummit east\n").unwrap();
        fs::write(f.dir.join("notes.txt"), "Pack water\n").unwrap();
        f.git(&["add", "notes.txt"]);
        fs::write(f.dir.join("loose.txt"), "Untracked\n").unwrap();
        let kept = Kept::default();

        let refused = around_switch(&f, false, &kept).unwrap_err();
        assert!(in_the_way(&refused), "{}", refused.text);
        assert_eq!(refused.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>(), [RETRY]);
        assert!(matches!(&refused.actions[0].1, view::Action::Autostash(Action::Retry(Retry::Switch(name))) if name == "other"));
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(listed(&f), "");

        let done = around_switch(&f, true, &kept).unwrap();
        assert_eq!(done.title, "Stashed your changes, switched to other and restored them");
        assert_eq!(done.detail, "Now on other.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/other\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail north\nRidge\nCreek\nLake\nSummit east\n");
        assert_eq!(fs::read_to_string(f.dir.join("loose.txt")).unwrap(), "Untracked\n");
        assert_eq!(fs::read_to_string(f.dir.join("notes.txt")).unwrap(), "Pack water\n");
        assert_eq!(listed(&f), "");
        assert!(kept.take().is_none());
        let status = String::from_utf8(f.git(&["status", "--porcelain=v1"])).unwrap();
        assert_eq!(status, "A  notes.txt\n M trail.txt\n?? loose.txt\n");
    }

    #[test]
    fn an_operation_that_needs_no_stash_runs_once_and_other_failures_keep_gits_words() {
        let f = apart();
        fs::write(f.dir.join("loose.txt"), "Untracked\n").unwrap();
        let kept = Kept::default();
        let done = around_switch(&f, true, &kept).unwrap();
        assert_eq!(done.detail, "Now on other.");
        assert_eq!(done.title, "");
        assert_eq!(listed(&f), "");
        let how = Around { doing: "switching to nowhere".to_owned(), stash: true, kept: &kept };
        let gone = around(
            &f.repo,
            how,
            || Retry::Switch("nowhere".into()),
            |repo| crate::repository::checked(repo.command().args(["switch", "--no-guess", "--", "nowhere"])).map(|_| Outcome::default()).map_err(Failure::from),
            |_| None,
        )
        .unwrap_err();
        assert!(gone.actions.is_empty() && !in_the_way(&gone), "{}", gone.text);
        assert_eq!(listed(&f), "");
    }

    #[test]
    fn changes_that_conflict_on_the_way_back_stay_in_their_stash_for_the_panel() {
        let f = apart();
        fs::write(f.dir.join("trail.txt"), "Trail south\nRidge\nCreek\nLake\nSummit\n").unwrap();
        let kept = Kept::default();
        let stopped = around_switch(&f, true, &kept).unwrap_err();
        assert!(
            stopped.text.starts_with("Switched to other — Your changes did not go back cleanly. 1 file needs resolving, and they stay in the stash \"FastKraken: before switching to other\""),
            "{}",
            stopped.text
        );
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/other\n");
        assert_eq!(listed(&f), "On main: FastKraken: before switching to other\n");
        let stash = kept.take().expect("the stash is remembered for the panel");
        assert_eq!(stash.oid, oid(&f, "refs/stash"));
        assert_eq!(stash.title, "FastKraken: before switching to other");
        assert!(stash.pop);
        let now = operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(now.kind, Kind::Applied);
        assert_eq!(now.conflicted(), [std::path::Path::new("trail.txt")]);
    }

    #[test]
    fn a_stash_the_switch_would_overwrite_stays_and_the_outcome_says_where() {
        let f = apart();
        f.git(&["switch", "-q", "other"]);
        commit(&f, "loose.txt", "Tracked on other\n", "Track loose");
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("trail.txt"), "Trail\nRidge\nCreek\nLake\nSummit east\n").unwrap();
        fs::write(f.dir.join("loose.txt"), "Mine\n").unwrap();
        let kept = Kept::default();
        let done = around_switch(&f, true, &kept).unwrap();
        assert_eq!(done.title, "Switched to other");
        assert!(done.detail.starts_with("Your local changes are still in the stash \"FastKraken: before switching to other\": loose.txt is already in your files"), "{}", done.detail);
        assert!(done.sticky);
        assert_eq!(listed(&f), "On main: FastKraken: before switching to other\n");
        assert!(kept.take().is_none());
        assert_eq!(fs::read_to_string(f.dir.join("loose.txt")).unwrap(), "Tracked on other\n");
    }

    #[test]
    fn a_failure_after_stashing_puts_the_changes_back_where_nothing_moved() {
        let f = apart();
        fs::write(f.dir.join("trail.txt"), "Trail\nRidge\nCreek\nLake\nSummit east\n").unwrap();
        let kept = Kept::default();
        let tries = std::cell::Cell::new(0);
        let how = Around { doing: "switching to other".to_owned(), stash: true, kept: &kept };
        let failed = around(
            &f.repo,
            how,
            || Retry::Switch("other".into()),
            |repo| {
                tries.set(tries.get() + 1);
                if tries.get() == 1 { switch(repo) } else { Err(Failure::from("The checked-out branch changed.".to_owned())) }
            },
            |_| None,
        )
        .unwrap_err();
        assert_eq!(failed.text, "The checked-out branch changed.\nYour local changes were put back.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail\nRidge\nCreek\nLake\nSummit east\n");
        assert_eq!(listed(&f), "");
    }

    #[test]
    fn a_repository_that_changed_refuses_the_stash() {
        let f = apart();
        fs::write(f.dir.join("trail.txt"), "Trail\nRidge\nCreek\nLake\nSummit east\n").unwrap();
        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{}\n", oid(&f, "other"))).unwrap();
        let kept = Kept::default();
        let how = Around { doing: "switching to other".to_owned(), stash: true, kept: &kept };
        let refused = around(
            &f.repo,
            how,
            || Retry::Switch("other".into()),
            |_| Err(Failure::from("error: Your local changes to the following files would be overwritten by checkout:".to_owned())),
            |_| None,
        )
        .unwrap_err();
        assert_eq!(refused.text, "Finish or abort the merge first.");
        assert_eq!(listed(&f), "");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail\nRidge\nCreek\nLake\nSummit east\n");
    }

    #[test]
    fn gits_autostash_that_conflicts_is_named_for_the_panel() {
        let f = apart();
        fs::write(f.dir.join("trail.txt"), "Trail south\nRidge\nCreek\nLake\nSummit\n").unwrap();
        let kept = Kept::default();
        let merged = by_git(
            &f.repo,
            true,
            &kept,
            || Retry::Switch("other".into()),
            |autostash| {
                let mut merge = f.repo.command();
                merge.args(["merge", "--no-edit", "--ff", if autostash { "--autostash" } else { "--no-autostash" }, "other"]);
                crate::repository::checked(&mut merge).map(|_| ()).map_err(Failure::from)
            },
            |_| "Merged other into main".to_owned(),
        )
        .unwrap_err();
        assert!(
            merged.text.starts_with("Merged other into main — Your changes did not go back cleanly. 1 file needs resolving, and they stay in the stash \"autostash\""),
            "{}",
            merged.text
        );
        assert_eq!(oid(&f, "HEAD"), oid(&f, "other"));
        let stash = kept.take().unwrap();
        assert_eq!((stash.oid, stash.title.as_str()), (oid(&f, "refs/stash"), "autostash"));
        assert_eq!(operation::snapshot(&f.repo).unwrap().unwrap().kind, Kind::Applied);
    }
}
