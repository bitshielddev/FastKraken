use crate::repository::Run;
use crate::{
    Tab,
    guard::Seen,
    notice::{self, Failure, Notice, Outcome},
    operation::{self, Kind as Operation, Picking},
    prompt::{self, Answer, Spec},
    repository::{Repository, checked, validate_oid},
    reverse::{self, Parent},
    view::{self, Selection, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::{Arc, Mutex},
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy, commit, operating, unselected};
    &[Command::new(
        "commit.cherry-pick",
        Group::Switch,
        |ui, _| match on_branch(ui) {
            true => format!("Cherry-pick onto {}…", ui.branch),
            false => "Cherry-pick this commit…".to_owned(),
        },
        |ui, target, _| {
            let Some(oid) = commit(target) else {
                return Availability::Hidden;
            };
            if contains(ui, oid) == Some(true) {
                return Availability::Hidden;
            }
            Availability::unless(
                busy(ui)
                    .or_else(|| operating(ui))
                    .or_else(|| (!on_branch(ui)).then(|| "Check out a branch first".to_owned()))
                    .or_else(|| unselected(ui, oid)),
            )
        },
        |_, _| Some(view::Action::Pick(Action::Start)),
    )
    .words(&["cherry pick", "pick commit", "copy commit", "backport", "apply commit"])
    .icon("commit")]
};

fn on_branch(ui: &Ui) -> bool {
    !ui.detached && ui.head.is_some()
}

pub(crate) fn contains(ui: &Ui, oid: &str) -> Option<bool> {
    reaches(&ui.history, ui.head.as_deref()?, oid)
}

fn reaches(history: &[view::HistoryRow], head: &str, oid: &str) -> Option<bool> {
    if head == oid {
        return Some(true);
    }
    let rows: HashMap<&str, &[String]> =
        history.iter().map(|row| (row.oid.as_str(), row.parents.as_slice())).collect();
    if !rows.contains_key(oid) || !rows.contains_key(head) {
        return None;
    }
    let mut seen = HashSet::new();
    let mut todo = vec![head];
    while let Some(next) = todo.pop() {
        if next == oid {
            return Some(true);
        }
        if seen.insert(next) {
            todo.extend(rows.get(next).into_iter().flat_map(|parents| parents.iter().map(String::as_str)));
        }
    }
    Some(false)
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Start,
}

#[derive(Clone, Debug)]
pub(crate) struct Ask {
    branch: String,
    seen: Seen,
    head: String,
    target: String,
    subject: String,
    message: String,
    parents: Vec<Parent>,
    commit_now: bool,
    mainline: Option<usize>,
}

pub(crate) struct Picked {
    pub outcome: Outcome,
    pub staged: Option<String>,
}

fn needing(count: usize) -> String {
    match count {
        1 => "1 file needs".to_owned(),
        n => format!("{n} files need"),
    }
}

impl Ask {
    fn label(&self) -> String {
        match self.subject.as_str() {
            "" => notice::short(&self.target).to_owned(),
            subject => format!("{} {subject}", notice::short(&self.target)),
        }
    }

    pub(crate) fn pick(&self, repo: &Repository, stopped: &Mutex<Option<Picking>>) -> Result<Picked, Failure> {
        validate_oid(&self.head)?;
        validate_oid(&self.target)?;
        if self.seen.head_ref.is_none() || self.seen.head.as_deref() != Some(self.head.as_str()) {
            return Err(Failure::from("Check out a branch first.".to_owned()));
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&self.seen)?;
        for parent in &self.parents {
            validate_oid(&parent.oid)?;
        }
        let mainline = match (self.parents.len(), self.mainline) {
            (0, None) => None,
            (n, Some(m)) if n >= 2 && (1..=n).contains(&m) => Some(m),
            _ => return Err(Failure::from("Choose whose changes in the merge to pick.".to_owned())),
        };
        if !self.commit_now && !operation::staged_nothing(repo)? {
            return Err(Failure::from(
                "Commit or unstage your staged changes first, so that only the picked changes are staged.".to_owned(),
            ));
        }
        let mut command = repo.command();
        command.env("GIT_EDITOR", "true").env("GIT_MERGE_AUTOEDIT", "no").stdin(Stdio::null());
        command.arg("cherry-pick").arg(if self.commit_now { "--no-edit" } else { "--no-commit" });
        if let Some(m) = mainline {
            command.arg(format!("--mainline={m}"));
        }
        let output = command.arg(&self.target).run().map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let (branch, label) = (&self.branch, self.label());
        let already = || Picked {
            outcome: Outcome::titled(
                format!("{branch} already has these changes"),
                format!("Picking {label} would change nothing on {branch}, so nothing was picked."),
            ),
            staged: None,
        };
        let gits_words = || {
            let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let text = if text.is_empty() { printed.trim().to_owned() } else { text };
            let text = if text.is_empty() { format!("Git exited with {}", output.status) } else { text };
            Failure::from(text).output(printed.clone())
        };
        match operation::snapshot(repo)? {
            Some(now) if now.kind == Operation::CherryPick && !now.conflicts.is_empty() => Err(Failure::from(format!(
                "The cherry-pick stopped on a conflict. {} resolving before it can go on.",
                needing(now.conflicts.len())
            ))
            .output(printed)),
            Some(now) if now.kind == Operation::CherryPick && now.empty && self.commit_now => {
                operation::backend::quit_pick(repo)?;
                Ok(already())
            }
            Some(now) if now.kind == Operation::Applied && !self.commit_now && !now.conflicts.is_empty() => {
                *stopped.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Picking { oid: self.target.clone(), label, message: self.message.clone(), squash: false });
                Err(Failure::from(format!(
                    "The cherry-pick stopped on a conflict. {} resolving; the result stays staged, not committed.",
                    needing(now.conflicts.len())
                ))
                .output(printed))
            }
            Some(_) => Err(gits_words()),
            None if !output.status.success() => Err(gits_words()),
            None if !self.commit_now => {
                let empty = operation::staged_nothing(repo)?;
                operation::backend::quit_pick(repo)?;
                if empty {
                    return Ok(already());
                }
                Ok(Picked {
                    outcome: Outcome::titled(
                        "Cherry-pick staged",
                        format!("The changes from {label} are staged on {branch}. Commit them when you are ready."),
                    ),
                    staged: Some(self.message.clone()),
                })
            }
            None => {
                let head = checked(repo.read_command().args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
                let head = String::from_utf8_lossy(&head).trim().to_owned();
                if head == self.head {
                    return Ok(already());
                }
                Ok(Picked {
                    outcome: Outcome::titled(
                        format!("Cherry-picked {} as {}", notice::short(&self.target), notice::short(&head)),
                        format!("{branch} now has the changes from {label}."),
                    ),
                    staged: None,
                })
            }
        }
    }
}

impl Ask {
    pub(crate) fn attempt(
        &self,
        repo: &Repository,
        stopped: &Mutex<Option<Picking>>,
        stash: bool,
        kept: &crate::autostash::Kept,
    ) -> Result<Picked, Failure> {
        if !self.commit_now {
            return self.pick(repo, stopped);
        }
        let how = crate::autostash::Around {
            doing: format!("cherry-picking {}", notice::short(&self.target)),
            stash,
            kept,
        };
        let outcome = crate::autostash::around(
            repo,
            how,
            || crate::autostash::Retry::Pick(Box::new(self.clone())),
            |repo| self.pick(repo, stopped).map(|picked| picked.outcome),
            |outcome| outcome.title.starts_with("Cherry-picked").then(|| crate::autostash::lowered(&outcome.title)),
        )?;
        Ok(Picked { outcome, staged: None })
    }
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(ask)
}

fn pick_label(parents: &[Parent], mainline: usize) -> String {
    format!("Pick what {} brought in", reverse::brought_in(parents, mainline))
}

fn spec(ask: &Ask) -> Spec {
    let branch = &ask.branch;
    let short = notice::short(&ask.target);
    let commit = match ask.subject.as_str() {
        "" => short.to_owned(),
        subject => format!("{short}  {subject}"),
    };
    let mut spec = match ask.parents.len() {
        0 | 1 => Spec::new(format!("Cherry-pick this commit onto {branch}?"), "Cherry-pick commit").line(commit),
        n => {
            let mut spec = Spec::new(format!("Cherry-pick this merge onto {branch}?"), pick_label(&ask.parents, 1))
                .line(commit)
                .line(format!("A merge joins {n} lines of work. Choose whose changes to copy."));
            for mainline in 2..=n {
                spec = spec.choice(pick_label(&ask.parents, mainline), false);
            }
            spec
        }
    };
    spec = spec.line(format!(
        "A new commit with the same changes is added to {branch}. Clear the tick to leave the changes staged instead, to edit and commit yourself."
    ));
    spec.tick = Some(("Commit straight away".into(), true));
    spec
}

pub(crate) fn confirm(tab: &mut Tab, mut ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    ask.commit_now = answer.tick;
    ask.mainline = (ask.parents.len() >= 2).then(|| answer.choice.map_or(1, |i| i + 2));
    let stash = tab.ui.autostash.always;
    run(tab, ask, stash, cx);
}

pub(crate) fn run(tab: &mut Tab, ask: Ask, stash: bool, cx: &mut Context<Tab>) {
    let commit_now = ask.commit_now;
    let staged = Arc::new(Mutex::new(None));
    let kept = staged.clone();
    let stopped = Arc::new(Mutex::new(None));
    let stop = stopped.clone();
    let restored = crate::autostash::Kept::default();
    let left = restored.clone();
    tab.run_operation_or(
        cx,
        "Cherry-picking the commit…",
        "Cherry-picked",
        commit_now,
        move |repo| {
            ask.attempt(&repo, &stop, stash, &restored).map(|picked| {
                *kept.lock().unwrap_or_else(|e| e.into_inner()) = picked.staged;
                picked.outcome
            })
        },
        move |tab, cx| {
            if commit_now {
                return;
            }
            let message = staged.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(message) = message {
                operation::draft_message(&tab.ui, &message, cx);
                tab.select(Selection::Changes, cx);
            }
        },
        move |tab, _| {
            tab.ui.operation.picking = stopped.lock().unwrap_or_else(|e| e.into_inner()).take();
            left.remember(&mut tab.ui);
        },
    );
}

fn message_of(repo: &Repository, oid: &str) -> Result<String, String> {
    validate_oid(oid)?;
    let message = checked(repo.read_command().args(["log", "--no-walk", "--format=%B", oid, "--"]))?;
    Ok(String::from_utf8_lossy(&message).trim_end().to_owned())
}

fn check_then_ask(tab: &mut Tab, mut ask: Ask, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (head, target) = (ask.head.clone(), ask.target.clone());
    Tab::work(
        cx,
        move || {
            validate_oid(&head)?;
            validate_oid(&target)?;
            if repo.is_ancestor(&target, &head)? {
                return Ok::<_, String>(None);
            }
            Ok(Some((reverse::merge_parents(&repo, &target)?, message_of(&repo, &target)?)))
        },
        move |tab, checked, cx| {
            let ui = &tab.ui;
            if tab.epoch != epoch
                || Seen::of(ui) != ask.seen
                || ui.selection != Some(Selection::Commit(ask.target.clone()))
                || ui.busy()
                || ui.dialog_open()
            {
                return;
            }
            match checked {
                Ok(None) => {
                    let title = format!("{} already has {}", ask.branch, notice::short(&ask.target));
                    tab.ui.notices.show(Notice::new(
                        Tone::Info,
                        title,
                        format!("It is part of {}'s history, so there is nothing to pick.", ask.branch),
                    ));
                    cx.notify();
                }
                Ok(Some((parents, message))) => {
                    ask.parents = parents;
                    ask.message = message;
                    prompt::ask(&mut tab.ui, prompt::Kind::Pick(ask), cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    match action {
        Action::Start => {
            let (Some(head), Some(Selection::Commit(target))) = (tab.ui.head.clone(), tab.ui.selection.clone()) else {
                return;
            };
            if !on_branch(&tab.ui) {
                tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
                cx.notify();
                return;
            }
            let ui = &tab.ui;
            let ask = Ask {
                branch: ui.branch.clone(),
                seen: Seen::of(ui),
                head,
                subject: reverse::subject_of(ui, &target),
                target,
                message: String::new(),
                parents: Vec::new(),
                commit_now: true,
                mainline: None,
            };
            check_then_ask(tab, ask, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::{commit, oid};
    use crate::repository::tests::Fixture;
    use std::fs;

    fn forked() -> (Fixture, String, String, String) {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let clash = commit(&f, "trail.txt", "Trail via the ferry\n", "Route the trail to the ferry");
        f.git(&["switch", "-q", "main"]);
        let head = commit(&f, "trail.txt", "Trail via the bridge\n", "Route the trail to the bridge");
        (f, head, ferry, clash)
    }

    fn ask(f: &Fixture, head: &str, target: &str, commit_now: bool) -> Ask {
        Ask {
            branch: "main".into(),
            seen: Seen { head_ref: Some("refs/heads/main".into()), head: Some(head.into()) },
            head: head.into(),
            target: target.into(),
            subject: String::from_utf8(f.git(&["log", "-1", "--format=%s", target])).unwrap().trim().to_owned(),
            message: message_of(&f.repo, target).unwrap(),
            parents: reverse::merge_parents(&f.repo, target).unwrap(),
            commit_now,
            mainline: None,
        }
    }

    fn picked(f: &Fixture, ask: &Ask) -> (Result<Picked, Failure>, Option<Picking>) {
        let stopped = Mutex::new(None);
        let result = ask.pick(&f.repo, &stopped);
        (result, stopped.into_inner().unwrap())
    }

    fn status(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1"])).unwrap()
    }

    fn leftovers(f: &Fixture) -> Vec<&'static str> {
        ["CHERRY_PICK_HEAD", "MERGE_MSG", "AUTO_MERGE", "sequencer"]
            .into_iter()
            .filter(|name| f.repo.git_dir().join(name).exists())
            .collect()
    }

    #[test]
    fn a_clean_pick_adds_a_commit_with_the_same_patch() {
        let (f, head, ferry, _) = forked();
        let (result, stopped) = picked(&f, &ask(&f, &head, &ferry, true));
        let picked = result.ok().expect("the pick applies");
        let new = oid(&f, "HEAD");
        assert_eq!(picked.outcome.title, format!("Cherry-picked {} as {}", &ferry[..7], &new[..7]));
        assert_eq!(picked.outcome.detail, format!("main now has the changes from {} Add the ferry.", &ferry[..7]));
        assert!(picked.staged.is_none() && stopped.is_none());
        assert_eq!(oid(&f, "HEAD^"), head);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["log", "-1", "--format=%B", "HEAD"]), f.git(&["log", "-1", "--format=%B", &ferry]));
        assert_eq!(f.git(&["diff", "HEAD^", "HEAD"]), f.git(&["diff", &format!("{ferry}^"), &ferry]));
        assert_eq!(fs::read_to_string(f.dir.join("ferry.txt")).unwrap(), "Hourly crossings\n");
        assert_eq!(status(&f), "");
        assert!(leftovers(&f).is_empty() || leftovers(&f) == ["AUTO_MERGE"], "{:?}", leftovers(&f));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_pick_left_uncommitted_stages_the_patch_and_hands_over_the_message() {
        let (f, head, ferry, _) = forked();
        fs::write(f.dir.join("notes.txt"), "Untracked notes\n").unwrap();
        let (result, stopped) = picked(&f, &ask(&f, &head, &ferry, false));
        let picked = result.ok().expect("the pick applies");
        assert_eq!(picked.outcome.title, "Cherry-pick staged");
        assert_eq!(picked.staged.as_deref(), Some("Add the ferry"));
        assert!(stopped.is_none());
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(status(&f), "A  ferry.txt\n?? notes.txt\n");
        assert!(leftovers(&f).is_empty(), "{:?}", leftovers(&f));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_pick_left_uncommitted_refuses_staged_changes() {
        let (f, head, ferry, _) = forked();
        fs::write(f.dir.join("trail.txt"), "Staged trail\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        let index = f.git(&["ls-files", "--stage"]);
        let (result, _) = picked(&f, &ask(&f, &head, &ferry, false));
        assert_eq!(
            result.err().expect("refused").text,
            "Commit or unstage your staged changes first, so that only the picked changes are staged."
        );
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert!(!f.dir.join("ferry.txt").exists());
    }

    #[test]
    fn a_conflicted_pick_stops_for_the_panel_and_is_continued_there() {
        let (f, head, _, clash) = forked();
        let (result, stopped) = picked(&f, &ask(&f, &head, &clash, true));
        let err = result.err().expect("the pick conflicts");
        assert_eq!(err.text, "The cherry-pick stopped on a conflict. 1 file needs resolving before it can go on.");
        assert!(err.output.as_deref().is_some_and(|output| output.contains("CONFLICT")));
        assert!(stopped.is_none());
        let shown = operation::snapshot(&f.repo).unwrap().expect("stopped");
        assert_eq!(shown.kind, Operation::CherryPick);
        assert_eq!(shown.summary(), format!("Cherry-picking {} Route the trail to the ferry onto main · 1 file needs resolving", &clash[..7]));
        assert_eq!(shown.abort_label(), Some(operation::ABORT_PICK));
        fs::write(f.dir.join("trail.txt"), "Trail via the bridge and the ferry\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        let ready = operation::snapshot(&f.repo).unwrap().unwrap();
        let outcome = operation::backend::finish(&f.repo, &ready, ready.message.clone()).unwrap();
        let new = oid(&f, "HEAD");
        assert_eq!(outcome.title, format!("Cherry-picked {} as {}", &clash[..7], &new[..7]));
        assert_eq!(oid(&f, "HEAD^"), head);
        assert_eq!(f.git(&["log", "-1", "--format=%s", "HEAD"]), b"Route the trail to the ferry\n");
        assert_eq!(f.git(&["log", "-1", "--format=%an <%ae>", "HEAD"]), f.git(&["log", "-1", "--format=%an <%ae>", &clash]));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        assert!(!f.repo.git_dir().join("CHERRY_PICK_HEAD").exists());
    }

    #[test]
    fn a_conflicted_pick_left_uncommitted_is_named_and_aborted_back_to_the_status_before() {
        let (f, _, _, clash) = forked();
        fs::write(f.dir.join("notes.txt"), "Untracked notes\n").unwrap();
        let head2 = commit(&f, "plan.txt", "Plan\n", "Plan");
        fs::write(f.dir.join("plan.txt"), "Plan, edited\n").unwrap();
        let before = (status(&f), f.git(&["ls-files", "--stage"]), fs::read(f.dir.join("trail.txt")).unwrap());
        let (result, stopped) = picked(&f, &ask(&f, &head2, &clash, false));
        let err = result.err().expect("the pick conflicts");
        assert_eq!(err.text, "The cherry-pick stopped on a conflict. 1 file needs resolving; the result stays staged, not committed.");
        let picking = stopped.expect("remembered");
        assert_eq!(picking, Picking { oid: clash.clone(), label: format!("{} Route the trail to the ferry", &clash[..7]), message: "Route the trail to the ferry".into(), squash: false });
        let mut state = operation::State::default();
        state.picking = Some(picking.clone());
        state.mark(Some(Operation::Applied));
        state.update(operation::snapshot(&f.repo).unwrap());
        let shown = state.snapshot().unwrap().clone();
        assert_eq!(shown.kind, Operation::Applied);
        assert_eq!(shown.summary(), format!("Cherry-picking {} onto main, not committed · 1 file needs resolving", picking.label));
        assert_eq!((shown.ours(), shown.theirs()), ("your files".to_owned(), format!("picked commit {}", picking.label)));
        assert_eq!(shown.theirs_name(), &clash[..7]);
        assert_eq!(state.abort_label(), Some(operation::ABORT_PICK));
        assert_eq!(state.continue_label(), None);
        let mut unknown = shown.clone();
        unknown.picked = None;
        assert_eq!(unknown.abort_label(), None);
        assert!(operation::backend::abort(&f.repo, &unknown).is_err());
        let outcome = operation::backend::abort(&f.repo, &shown).unwrap();
        assert_eq!(outcome.title, "Cherry-pick aborted");
        assert_eq!(oid(&f, "HEAD"), head2);
        assert_eq!((status(&f), f.git(&["ls-files", "--stage"]), fs::read(f.dir.join("trail.txt")).unwrap()), before);
        assert_eq!(fs::read_to_string(f.dir.join("plan.txt")).unwrap(), "Plan, edited\n");
        assert!(leftovers(&f).is_empty(), "{:?}", leftovers(&f));
        state.update(operation::snapshot(&f.repo).unwrap());
        assert!(state.picking.is_none() && state.snapshot().is_none());
    }

    #[test]
    fn a_conflicted_pick_left_uncommitted_ends_staged_once_resolved() {
        let (f, head, _, clash) = forked();
        let (result, stopped) = picked(&f, &ask(&f, &head, &clash, false));
        assert!(result.is_err() && stopped.is_some());
        fs::write(f.dir.join("trail.txt"), "Trail via the ferry\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(status(&f), "M  trail.txt\n");
        assert!(!operation::staged_nothing(&f.repo).unwrap());
        let notice = operation::pick_resolved(&stopped.unwrap(), "main", true);
        assert_eq!(notice.title, "Cherry-pick staged");
    }

    #[test]
    fn a_conflicted_pick_left_uncommitted_and_resolved_to_the_branch_stages_nothing() {
        let (f, head, _, clash) = forked();
        let (result, stopped) = picked(&f, &ask(&f, &head, &clash, false));
        assert!(result.is_err());
        f.git(&["checkout", "--ours", "--", "trail.txt"]);
        f.git(&["add", "--", "trail.txt"]);
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        assert!(!f.git(&["ls-files", "--resolve-undo"]).is_empty());
        assert!(operation::staged_nothing(&f.repo).unwrap());
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(status(&f), "");
        let notice = operation::pick_resolved(&stopped.unwrap(), "main", false);
        assert_eq!(notice.title, "main already has these changes");
        assert_eq!(
            notice.detail,
            format!("Every conflict is resolved, and picking {} Route the trail to the ferry changed nothing on main, so nothing is staged.", &clash[..7])
        );
    }

    #[test]
    fn a_pick_of_changes_the_branch_already_has_says_so_and_leaves_nothing_behind() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let side = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        f.git(&["switch", "-q", "main"]);
        let head = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the same ferry");
        for commit_now in [true, false] {
            let (result, stopped) = picked(&f, &ask(&f, &head, &side, commit_now));
            let picked = result.ok().expect("reported");
            assert_eq!(picked.outcome.title, "main already has these changes");
            assert_eq!(
                picked.outcome.detail,
                format!("Picking {} Add the ferry would change nothing on main, so nothing was picked.", &side[..7])
            );
            assert!(picked.staged.is_none() && stopped.is_none());
            assert_eq!(oid(&f, "HEAD"), head);
            assert_eq!(status(&f), "");
            assert!(leftovers(&f).is_empty(), "{commit_now}: {:?}", leftovers(&f));
            assert!(operation::snapshot(&f.repo).unwrap().is_none());
        }
    }

    #[test]
    fn a_merge_is_picked_against_the_side_chosen() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "release"]);
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "bridge.txt", "Stone bridge\n", "Add the bridge");
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "feature"]);
        let merge = oid(&f, "HEAD");
        f.git(&["switch", "-q", "release"]);
        let head = oid(&f, "HEAD");
        let mut pick = ask(&f, &head, &merge, true);
        pick.branch = "release".into();
        pick.seen.head_ref = Some("refs/heads/release".into());
        assert_eq!(pick.parents.len(), 2);
        let shown = spec(&pick);
        assert_eq!(shown.title, "Cherry-pick this merge onto release?");
        assert_eq!(shown.confirm.as_ref(), "Pick what feature brought in");
        assert_eq!(shown.choices.iter().map(|c| c.label.to_string()).collect::<Vec<_>>(), [format!("Pick what {} brought in", pick.parents[0].name)]);
        assert_eq!(shown.tick.as_ref().map(|(label, on)| (label.to_string(), *on)), Some(("Commit straight away".into(), true)));
        for mainline in [None, Some(3)] {
            pick.mainline = mainline;
            assert!(picked(&f, &pick).0.err().unwrap().text.contains("Choose whose changes"));
            assert_eq!(oid(&f, "HEAD"), head);
        }
        pick.mainline = Some(1);
        picked(&f, &pick).0.ok().expect("picked");
        assert!(f.dir.join("ferry.txt").exists() && !f.dir.join("bridge.txt").exists());
        assert_eq!(oid(&f, "HEAD^"), head);
        f.git(&["reset", "-q", "--hard", &head]);
        pick.mainline = Some(2);
        picked(&f, &pick).0.ok().expect("picked");
        assert!(!f.dir.join("ferry.txt").exists() && f.dir.join("bridge.txt").exists());
    }

    #[test]
    fn a_pick_refuses_a_changed_branch_or_tip_and_an_operation_in_progress() {
        let (f, head, ferry, clash) = forked();
        let pick = ask(&f, &head, &ferry, true);
        f.git(&["switch", "-qc", "other"]);
        assert!(picked(&f, &pick).0.err().unwrap().text.contains("tip changed"));
        f.git(&["switch", "-q", "main"]);
        commit(&f, "later.txt", "Later\n", "Later work");
        let tip = oid(&f, "HEAD");
        assert!(picked(&f, &pick).0.err().unwrap().text.contains("tip changed"));
        assert_eq!(oid(&f, "HEAD"), tip);
        assert!(!f.dir.join("ferry.txt").exists());
        let fresh = ask(&f, &tip, &ferry, true);
        crate::operation::tests::stops(&f, &["cherry-pick", &clash]);
        let err = picked(&f, &fresh).0.err().unwrap();
        assert_eq!(err.text, "Finish or abort the cherry-pick first.");
        assert!(!f.dir.join("ferry.txt").exists());
    }

    #[test]
    fn the_menu_knows_which_commits_the_branch_has_from_the_history() {
        let (f, head, ferry, _) = forked();
        let base = oid(&f, "HEAD^");
        let mut history = f.repo.history(0, 100).unwrap();
        assert_eq!(reaches(&history, &head, &head), Some(true));
        assert_eq!(reaches(&history, &head, &base), Some(true));
        assert_eq!(reaches(&history, &head, &ferry), Some(false));
        assert_eq!(reaches(&history, &ferry, &base), Some(true));
        assert_eq!(reaches(&history, &head, &"0".repeat(40)), None);
        history.truncate(1);
        assert_eq!(reaches(&history, &head, &base), None);
    }

    #[test]
    fn a_pick_with_changes_in_its_way_stashes_them_when_asked_and_puts_them_back() {
        let (f, _, _, _) = forked();
        let base = commit(&f, "route.txt", "Start\nFord\nSummit\n", "Plan the route");
        f.git(&["switch", "-qc", "north"]);
        let first = commit(&f, "route.txt", "Start north\nFord\nSummit\n", "Start north");
        f.git(&["switch", "-qc", "south", &base]);
        let clash = commit(&f, "route.txt", "Start south\nFord\nSummit\n", "Start south");
        f.git(&["switch", "-q", "main"]);
        let head = commit(&f, "camp.txt", "Camp\n", "Pitch camp");
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        let kept = crate::autostash::Kept::default();
        let stopped = Mutex::new(None);

        let pick = ask(&f, &head, &first, true);
        let refused = pick.attempt(&f.repo, &stopped, false, &kept).err().expect("Git refuses");
        assert!(crate::autostash::in_the_way(&refused), "{}", refused.text);
        let [(label, view::Action::Autostash(crate::autostash::Action::Retry(crate::autostash::Retry::Pick(again))))] =
            refused.actions.as_slice()
        else {
            panic!("The refusal offered {:?}", refused.actions);
        };
        assert_eq!((label.as_ref(), again.target.as_str()), (crate::autostash::RETRY, first.as_str()));
        assert_eq!(oid(&f, "HEAD"), head);

        let done = again.attempt(&f.repo, &stopped, true, &kept).ok().expect("the pick applies");
        let new = oid(&f, "HEAD");
        assert_eq!(done.outcome.title, format!("Stashed your changes, cherry-picked {} as {} and restored them", &first[..7], &new[..7]));
        assert_eq!(oid(&f, "HEAD^"), head);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
        assert_eq!(status(&f), " M route.txt\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(kept.take().is_none());

        let conflicted = ask(&f, &new, &clash, true).attempt(&f.repo, &stopped, true, &kept).err().expect("the pick conflicts");
        assert!(
            conflicted.text.ends_with(&format!(
                "Your local changes are in the stash \"FastKraken: before cherry-picking {}\". Pop it once the cherry-pick is finished.",
                &clash[..7]
            )),
            "{}",
            conflicted.text
        );
        assert_eq!(operation::snapshot(&f.repo).unwrap().unwrap().kind, Operation::CherryPick);
        assert!(kept.take().is_none());
        f.git(&["cherry-pick", "--abort"]);
        f.git(&["stash", "pop", "-q"]);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
    }
}
