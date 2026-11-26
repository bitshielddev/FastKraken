use crate::repository::Run;
use crate::{
    Tab,
    commands::Target,
    guard::Seen,
    journal::{Change, Discarded, Mode},
    notice::{self, Failure, Notice, Outcome},
    operation,
    prompt::{self, Answer, Spec},
    refs::Kind,
    repository::{Repository, Revisions, checked, validate_oid},
    view::{self, Selection, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::Stdio,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{
        Availability, Command, Group, Submenu, busy, commit, operating, on_repo, unselected,
    };
    const RESET: Submenu = Submenu {
        id: "commit.reset",
        title: |ui, _| {
            if ui.detached { "Reset to this commit".to_owned() } else { format!("Reset {} to this commit", ui.branch) }
        },
        icon: "history",
    };
    fn on_branch(ui: &view::Ui, target: &Target) -> Availability {
        let Some(oid) = commit(target) else {
            return Availability::Hidden;
        };
        Availability::unless(
            busy(ui)
                .or_else(|| operating(ui))
                .or_else(|| {
                    (ui.detached || ui.head.is_none())
                        .then(|| "Check out a branch first".to_owned())
                })
                .or_else(|| unselected(ui, oid)),
        )
    }
    &[
        Command::new(
            "commit.revert",
            Group::Reverse,
            |_, _| "Revert commit…".into(),
            |ui, target, _| on_branch(ui, target),
            |_, _| Some(view::Action::Reverse(Action::Start(Operation::Revert))),
        )
        .words(&["undo commit"])
        .icon("undo"),
        Command::new(
            "commit.reset-soft",
            Group::Reverse,
            |_, _| "Soft: keep all changes staged…".into(),
            |ui, target, _| on_branch(ui, target),
            |_, _| Some(view::Action::Reverse(Action::Start(Operation::SoftReset))),
        )
        .under(&RESET)
        .words(&["move branch", "soft reset"])
        .icon("history"),
        Command::new(
            "commit.reset-mixed",
            Group::Reverse,
            |_, _| "Mixed: keep the files, clear the staging area…".into(),
            |ui, target, _| on_branch(ui, target),
            |_, _| Some(view::Action::Reverse(Action::Start(Operation::MixedReset))),
        )
        .under(&RESET)
        .words(&["move branch", "mixed reset", "unstage"])
        .icon("history"),
        Command::new(
            "commit.reset-hard",
            Group::Reverse,
            |_, _| "Hard: discard all changes…".into(),
            |ui, target, _| on_branch(ui, target),
            |_, _| Some(view::Action::Reverse(Action::Start(Operation::HardReset))),
        )
        .under(&RESET)
        .words(&["move branch", "hard reset", "throw away commits"])
        .icon("history")
        .danger(),
        Command::new(
            "branch.reset-to-upstream",
            Group::Reverse,
            |ui, _| match upstream(ui) {
                Some(found) => format!("Reset {} to {}…", ui.branch, found.shown),
                None => "Reset branch to its upstream…".into(),
            },
            |ui, target, _| {
                match target {
                    Target::Branch(name) => {
                        if ui.detached || ui.branch.as_bytes() != name.as_bytes() || upstream(ui).is_none() {
                            return Availability::Hidden;
                        }
                    }
                    Target::Repo if on_repo(ui, target) => {}
                    _ => return Availability::Hidden,
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| upstream_problem(ui)))
            },
            |_, _| Some(view::Action::Reverse(Action::ResetToUpstream)),
        )
        .words(&["hard reset", "discard local commits", "match remote", "origin"])
        .icon("history")
        .danger(),
    ]
};

struct Upstream {
    full: OsString,
    shown: String,
    tip: Option<String>,
}

fn upstream(ui: &Ui) -> Option<Upstream> {
    if ui.detached {
        return None;
    }
    let shown = ui.refs.current(&ui.branch)?.upstream.clone()?;
    let item = ui
        .refs
        .items
        .iter()
        .find(|item| item.kind == Kind::Remote && item.name.to_string_lossy() == shown);
    let mut full = OsString::from("refs/remotes/");
    full.push(item.map_or_else(|| OsString::from(&shown), |item| item.name.clone()));
    Some(Upstream { full, shown, tip: item.and_then(|item| item.oid.clone()) })
}

fn upstream_problem(ui: &Ui) -> Option<String> {
    if ui.detached || ui.head.is_none() {
        return Some("Check out a branch first".to_owned());
    }
    let Some(found) = upstream(ui) else {
        return Some(format!("{} has no upstream branch", ui.branch));
    };
    let Some(tip) = found.tip else {
        return Some(format!("Fetch first, so FastKraken can see {}", found.shown));
    };
    let changed = ui.files.iter().any(|file| file.status != "Untracked");
    (ui.head.as_deref() == Some(tip.as_str()) && !changed)
        .then(|| format!("{} already matches {}", ui.branch, found.shown))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    SoftReset,
    MixedReset,
    HardReset,
    Revert,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Start(Operation),
    ResetToUpstream,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Worktree {
    changed: usize,
    staged: usize,
    partial: usize,
    untracked: usize,
    replaced: usize,
    ignored: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Ask {
    operation: Operation,
    branch: String,
    seen: Seen,
    head: String,
    target: String,
    subject: String,
    upstream: Option<(OsString, String)>,
    leaving: usize,
    listed: Vec<(String, String)>,
    worktree: Worktree,
    parents: Vec<Parent>,
    commit_now: bool,
    mainline: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parent {
    pub(crate) oid: String,
    pub(crate) name: String,
}

pub(crate) struct Reverted {
    pub outcome: Outcome,
    pub staged: Option<String>,
}

const LISTED: usize = 5;

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

impl Ask {
    fn leaving_line(&self) -> String {
        let branch = &self.branch;
        match self.leaving {
            _ if self.target == self.head => format!("{branch} stays at this commit."),
            0 => format!("No commits leave {branch}."),
            1 => format!("1 commit leaves {branch}."),
            n => format!("{n} commits leave {branch}."),
        }
    }

    fn execute(&self, repo: &Repository) -> Result<Outcome, String> {
        validate_oid(&self.head)?;
        validate_oid(&self.target)?;
        if self.seen.head_ref.is_none() || self.seen.head.as_deref() != Some(self.head.as_str()) {
            return Err("Check out a branch first.".into());
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&self.seen)?;
        if let Some((full, _)) = &self.upstream {
            repo.expect_ref(full, &self.target)?;
        }
        let recounted = match self.operation {
            Operation::HardReset => Some(worktree(repo, &self.target)? != self.worktree),
            Operation::MixedReset => Some(worktree(repo, &self.target)?.partial != self.worktree.partial),
            _ => None,
        };
        if recounted == Some(true) {
            return Err("Your uncommitted changes are not the ones the prompt counted. Reopen the action.".into());
        }
        let target = self.target.as_str();
        let mut saved = None;
        match self.operation {
            Operation::SoftReset => checked(repo.command().args(["reset", "--soft", target, "--"])),
            Operation::MixedReset => checked(repo.command().args(["reset", "--mixed", target, "--"])),
            Operation::HardReset => {
                saved = repo.save_tracked()?;
                checked(repo.command().args(["reset", "--hard", "--no-recurse-submodules", target, "--"]))
            }
            Operation::Revert => return self.revert(repo).map(|reverted| reverted.outcome).map_err(|f| f.text),
        }?;
        let mode = match self.operation {
            Operation::SoftReset => Mode::Soft,
            Operation::MixedReset => Mode::Mixed,
            _ => Mode::Keep,
        };
        let mut outcome = self.outcome();
        let undone = outcome.title.clone();
        outcome = outcome.undone_as(undone).moving(mode);
        if let Some(saved) = saved {
            outcome = outcome.changed(Change::Discard { saved, discarded: Discarded::Tracked });
        }
        if self.operation == Operation::HardReset {
            outcome = outcome.offering_undo().sticky();
        }
        Ok(outcome)
    }

    pub(crate) fn revert(&self, repo: &Repository) -> Result<Reverted, Failure> {
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
            _ => return Err(Failure::from("Choose which side of the merge to undo.".to_owned())),
        };
        if !self.commit_now && !operation::staged_nothing(repo)? {
            return Err(Failure::from(
                "Commit or unstage your staged changes first, so that only the revert is staged.".to_owned(),
            ));
        }
        let mut command = repo.command();
        command.env("GIT_EDITOR", "true").env("GIT_MERGE_AUTOEDIT", "no").stdin(Stdio::null());
        command.arg("revert").arg(if self.commit_now { "--no-edit" } else { "--no-commit" });
        if let Some(m) = mainline {
            command.arg(format!("--mainline={m}"));
        }
        let output = command.arg(&self.target).run().map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let label = match self.subject.as_str() {
            "" => notice::short(&self.target).to_owned(),
            subject => format!("{} {subject}", notice::short(&self.target)),
        };
        let branch = &self.branch;
        let now = operation::snapshot(repo)?;
        let undone = || Reverted {
            outcome: Outcome::titled(
                "Already undone",
                format!("{branch} no longer has the changes from {label}, so there was nothing to revert."),
            ),
            staged: None,
        };
        match now {
            Some(now) if now.kind == operation::Kind::Revert && !now.conflicts.is_empty() => {
                let files = match now.conflicts.len() {
                    1 => "1 file needs".to_owned(),
                    n => format!("{n} files need"),
                };
                Err(Failure::from(format!("The revert stopped on a conflict. {files} resolving before it can go on."))
                    .output(printed))
            }
            Some(now) if now.kind == operation::Kind::Revert && !self.commit_now && output.status.success() => {
                let empty = operation::staged_nothing(repo)?;
                operation::backend::quit(repo)?;
                if empty {
                    return Ok(undone());
                }
                Ok(Reverted {
                    outcome: Outcome::titled(
                        "Revert staged",
                        format!("The changes that undo {label} are staged on {branch}. Commit them when you are ready."),
                    ),
                    staged: now.message,
                })
            }
            Some(_) => {
                let text = match printed.trim() {
                    "" => format!("Git exited with {} and left the revert stopped.", output.status),
                    text => text.to_owned(),
                };
                Err(Failure::from(text).output(printed))
            }
            None if output.status.code() == Some(1)
                && checked(repo.read_command().args(["rev-parse", "HEAD"]))? == format!("{}\n", self.head).into_bytes()
                && operation::staged_nothing(repo)? =>
            {
                operation::backend::quit(repo)?;
                Ok(undone())
            }
            None if !output.status.success() => {
                let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                let text = if text.is_empty() { printed.trim().to_owned() } else { text };
                Err(Failure::from(text).output(printed))
            }
            None => Ok(Reverted { outcome: Outcome::titled("Revert committed", format!("Undid {label} on {branch}.")), staged: None }),
        }
    }

    pub(crate) fn attempt(&self, repo: &Repository, stash: bool, kept: &crate::autostash::Kept) -> Result<Reverted, Failure> {
        if !self.commit_now {
            return self.revert(repo);
        }
        let how = crate::autostash::Around { doing: format!("reverting {}", notice::short(&self.target)), stash, kept };
        let short = notice::short(&self.target).to_owned();
        let outcome = crate::autostash::around(
            repo,
            how,
            || crate::autostash::Retry::Revert(Box::new(self.clone())),
            |repo| self.revert(repo).map(|reverted| reverted.outcome),
            |outcome| (outcome.title == "Revert committed").then(|| format!("reverted {short}")),
        )?;
        Ok(Reverted { outcome: outcome.undone_as(format!("Revert {short}")), staged: None })
    }

    fn outcome(&self) -> Outcome {
        let (branch, short) = (&self.branch, notice::short(&self.target));
        let left = match self.leaving {
            0 => String::new(),
            n => format!("{} left {branch}. ", notice::commits(n)),
        };
        let title = match &self.upstream {
            Some((_, shown)) => format!("Reset {branch} to {shown}"),
            None => format!("Reset {branch} to {short}"),
        };
        match self.operation {
            Operation::Revert => Outcome::default(),
            Operation::SoftReset if self.leaving == 0 => {
                Outcome::titled(title, "The index and your files are unchanged.")
            }
            Operation::SoftReset => Outcome::titled(title, format!("{left}Their changes are staged.")),
            Operation::MixedReset if self.leaving == 0 => {
                Outcome::titled(title, "The staging area is clear. Your files are unchanged.")
            }
            Operation::MixedReset => {
                Outcome::titled(title, format!("{left}Their changes are in your files, unstaged."))
            }
            Operation::HardReset => {
                let kept = self.worktree.untracked - self.worktree.replaced;
                let kept = match kept {
                    0 => "",
                    1 => " Your untracked file was kept.",
                    _ => " Your untracked files were kept.",
                };
                Outcome::titled(title, format!("{left}Your files match {short}.{kept}"))
            }
        }
    }
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(ask)
}

fn spec(ask: &Ask) -> Spec {
    let branch = &ask.branch;
    let short = notice::short(&ask.target);
    let commit = match &ask.upstream {
        Some((_, shown)) if ask.subject.is_empty() => format!("{shown} is at {short}."),
        Some((_, shown)) => format!("{shown} is at {short}  {}", ask.subject),
        None if ask.subject.is_empty() => short.to_owned(),
        None => format!("{short}  {}", ask.subject),
    };
    match ask.operation {
        Operation::Revert => {
            let mut spec = match ask.parents.len() {
                0 | 1 => Spec::new(format!("Revert this commit on {branch}?"), "Revert commit").line(commit),
                n => {
                    let mut spec = Spec::new(format!("Revert this merge on {branch}?"), undo(&ask.parents, 1))
                        .line(commit)
                        .line(format!(
                            "A merge joins {n} lines of work. Choose whose changes to take out; the rest stay."
                        ));
                    for mainline in 2..=n {
                        spec = spec.choice(undo(&ask.parents, mainline), false);
                    }
                    spec
                }
            };
            spec = spec.line(format!(
                "A new commit that undoes it is added to {branch}. Clear the tick to leave the changes staged instead, to edit and commit yourself."
            ));
            spec.tick = Some(("Commit the revert straight away".into(), true));
            spec
        }
        Operation::SoftReset => Spec::new(format!("Reset {branch} to this commit?"), "Reset branch")
            .line(commit)
            .line(match ask.leaving {
                _ if ask.target == ask.head => "The branch is already at this commit.".to_owned(),
                0 => "No commits leave the branch. The index and your files stay as they are.".to_owned(),
                1 => "1 commit will leave the branch. Its changes stay staged, so no work is lost.".to_owned(),
                n => format!(
                    "{} will leave the branch. Their changes stay staged, so no work is lost.",
                    notice::commits(n)
                ),
            }),
        Operation::MixedReset => {
            let spec = Spec::new(format!("Reset {branch} and clear the staging area?"), "Reset branch")
                .line(commit)
                .line(match ask.leaving {
                    0 => ask.leaving_line(),
                    1 => format!("1 commit leaves {branch}. Its changes stay in your files, unstaged."),
                    n => format!("{n} commits leave {branch}. Their changes stay in your files, unstaged."),
                });
            match (ask.worktree.staged, ask.worktree.partial) {
                (0, _) => spec.line("Your files do not change."),
                (n, 0) => spec.line(format!(
                    "The staged changes to {} become unstaged. Your files do not change.",
                    files(n)
                )),
                (n, partial) => {
                    let spec = match n.saturating_sub(partial) {
                        0 => spec,
                        whole => spec.line(format!("The staged changes to {} become unstaged.", files(whole))),
                    };
                    spec.line(match partial {
                        1 => "1 file was edited after it was staged. Its staged version will be lost; the file keeps your latest edits.".to_owned(),
                        n => format!("{n} files were edited after they were staged. Their staged versions will be lost; the files keep your latest edits."),
                    })
                    .danger()
                }
            }
        }
        Operation::HardReset => {
            let (title, confirm) = match &ask.upstream {
                Some((_, shown)) => (format!("Reset {branch} to {shown}?"), format!("Reset to {shown}")),
                None => (format!("Reset {branch} and discard all changes?"), "Discard and reset".to_owned()),
            };
            let discarded = match ask.worktree.changed {
                0 => String::new(),
                n => format!(" Changes in {} will be discarded.", files(n)),
            };
            let back = if ask.worktree.changed == 0 && ask.leaving == 1 { "it" } else { "them" };
            let mut spec = Spec::new(title, confirm)
                .line(commit)
                .line(format!("{}{discarded} Undo, in the notice that follows, brings {back} back.", ask.leaving_line()));
            for (short, subject) in &ask.listed {
                spec = spec.line(format!("{short}  {subject}"));
            }
            match ask.leaving.saturating_sub(ask.listed.len()) {
                0 => {}
                1 => spec = spec.line("and 1 more commit"),
                n => spec = spec.line(format!("and {n} more commits")),
            }
            let Worktree { untracked, replaced, .. } = ask.worktree;
            if replaced > 0 {
                spec = spec.line(match replaced {
                    1 => "1 untracked file will be replaced by the version in this commit.".to_owned(),
                    n => format!("{n} untracked files will be replaced by the versions in this commit."),
                });
            }
            spec = match ask.worktree.ignored {
                0 => spec,
                1 => spec.line("1 ignored file will be replaced by the version in this commit."),
                n => spec.line(format!("{n} ignored files will be replaced by the versions in this commit.")),
            };
            spec = match (untracked - replaced, replaced) {
                (0, _) => spec,
                (1, 0) => spec.line("Your untracked file is kept."),
                (n, 0) => spec.line(format!("Your {n} untracked files are kept.")),
                (1, _) => spec.line("The other untracked file is kept."),
                (n, _) => spec.line(format!("The other {n} untracked files are kept.")),
            };
            spec.danger()
        }
    }
}

pub(crate) fn confirm(tab: &mut Tab, mut ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    if ask.operation != Operation::Revert {
        tab.run_operation_then(cx, "Resetting the branch…", "Branch reset", true, move |repo| ask.execute(&repo), |_, _| {});
        return;
    }
    ask.commit_now = answer.tick;
    ask.mainline = (ask.parents.len() >= 2).then(|| answer.choice.map_or(1, |i| i + 2));
    let stash = tab.ui.autostash.always;
    revert(tab, ask, stash, cx);
}

pub(crate) fn revert(tab: &mut Tab, ask: Ask, stash: bool, cx: &mut Context<Tab>) {
    tab.ui.operation.uncommitted = (!ask.commit_now).then(|| ask.target.clone());
    let commit_now = ask.commit_now;
    let staged = std::sync::Arc::new(std::sync::Mutex::new(None));
    let kept = staged.clone();
    let restored = crate::autostash::Kept::default();
    let left = restored.clone();
    tab.run_operation_or(
        cx,
        "Reverting the commit…",
        "Revert committed",
        commit_now,
        move |repo| {
            ask.attempt(&repo, stash, &restored).map(|reverted| {
                *kept.lock().unwrap_or_else(|e| e.into_inner()) = reverted.staged;
                reverted.outcome
            })
        },
        move |tab, cx| {
            if commit_now {
                return;
            }
            let message = staged.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(message) = message {
                operation::draft_message(&tab.ui, &message, cx);
            }
            tab.select(Selection::Changes, cx);
        },
        move |tab, _| left.remember(&mut tab.ui),
    );
}

pub(crate) fn merge_parents(repo: &Repository, target: &str) -> Result<Vec<Parent>, String> {
    validate_oid(target)?;
    let shown = checked(repo.read_command().args(["log", "--no-walk", "--format=%P%x00%s", target, "--"]))?;
    let shown = String::from_utf8_lossy(&shown);
    let (parents, subject) = shown.trim_end().split_once('\0').unwrap_or((shown.trim_end(), ""));
    let parents: Vec<&str> = parents.split_whitespace().collect();
    if parents.len() < 2 {
        return Ok(Vec::new());
    }
    let (merged, into) = merged_names(subject);
    parents
        .iter()
        .enumerate()
        .map(|(i, oid)| {
            validate_oid(oid)?;
            let said = match (i, parents.len()) {
                (0, _) => into.clone(),
                (1, 2) => merged.clone(),
                _ => None,
            };
            let name = match said {
                Some(name) => name,
                None => operation::ref_name(repo, oid)?.unwrap_or_else(|| notice::short(oid).to_owned()),
            };
            Ok(Parent { oid: (*oid).to_owned(), name })
        })
        .collect()
}

fn merged_names(subject: &str) -> (Option<String>, Option<String>) {
    let named = |name: &str| Some(name.trim().to_owned()).filter(|name| !name.is_empty());
    if let Some(rest) = subject.strip_prefix("Merge pull request ") {
        return (rest.split_once(" from ").and_then(|(_, from)| named(from)), None);
    }
    let Some((_, quoted)) = subject.strip_prefix("Merge ").and_then(|rest| rest.split_once('\'')) else {
        return (None, None);
    };
    let Some((merged, after)) = quoted.split_once('\'') else {
        return (None, None);
    };
    (named(merged), after.strip_prefix(" into ").and_then(named))
}

fn undo(parents: &[Parent], mainline: usize) -> String {
    format!("Undo what {} brought in", brought_in(parents, mainline))
}

pub(crate) fn brought_in(parents: &[Parent], mainline: usize) -> String {
    let others: Vec<&str> = parents
        .iter()
        .enumerate()
        .filter(|(i, _)| i + 1 != mainline)
        .map(|(_, parent)| parent.name.as_str())
        .collect();
    match others.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn worktree(repo: &Repository, target: &str) -> Result<Worktree, String> {
    validate_oid(target)?;
    let status = repo.status()?;
    let mut changed = BTreeSet::new();
    let mut untracked = Vec::new();
    let mut partial = 0;
    for file in &status.staged {
        changed.insert(&file.path);
    }
    for file in &status.unstaged {
        if file.status == "Untracked" {
            untracked.push(&file.path);
        } else if !changed.insert(&file.path) {
            partial += 1;
        }
    }
    let replaced = if untracked.is_empty() { 0 } else { in_commit(repo, target, &untracked)? };
    let missing = checked(repo.read_command().args([
        "diff",
        "--cached",
        "--name-only",
        "--no-renames",
        "--diff-filter=D",
        "-z",
        target,
        "--",
    ]))?;
    let known: BTreeSet<&PathBuf> = untracked.iter().copied().collect();
    let ignored = missing
        .split(|b| *b == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(std::ffi::OsStr::from_bytes(path)))
        .filter(|path| !known.contains(path) && repo.root.join(path).symlink_metadata().is_ok())
        .count();
    Ok(Worktree {
        changed: changed.len(),
        staged: status.staged.len(),
        partial,
        untracked: untracked.len(),
        replaced,
        ignored,
    })
}

fn in_commit(repo: &Repository, target: &str, paths: &[&PathBuf]) -> Result<usize, String> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(target.as_bytes());
        input.push(b':');
        input.extend_from_slice(path.as_os_str().as_bytes());
        input.push(0);
    }
    let output = crate::repository::run_with_input(
        repo.read_command().args(["cat-file", "--batch-check=%(objecttype)", "-Z"]),
        input,
    )
    .map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|record| matches!(*record, b"blob" | b"tree" | b"commit"))
        .count())
}

fn count_then_ask(tab: &mut Tab, mut ask: Ask, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (head, target, operation) = (ask.head.clone(), ask.target.clone(), ask.operation);
    Tab::work(
        cx,
        move || {
            validate_oid(&head)?;
            validate_oid(&target)?;
            let own = Revisions::reachable(&head).not(&target);
            let leaving = repo.count(&own)?;
            let listed = if operation == Operation::HardReset && leaving > 0 {
                repo.subjects(&own, LISTED)?
            } else {
                Vec::new()
            };
            let (worktree, parents) = match operation {
                Operation::Revert => (Worktree::default(), merge_parents(&repo, &target)?),
                _ => (worktree(&repo, &target)?, Vec::new()),
            };
            Ok::<_, String>((leaving, listed, worktree, parents))
        },
        move |tab, counted, cx| {
            let ui = &tab.ui;
            let still = match &ask.upstream {
                Some(_) => upstream(ui).and_then(|found| found.tip).as_deref() == Some(ask.target.as_str()),
                None => ui.selection == Some(Selection::Commit(ask.target.clone())),
            };
            if tab.epoch != epoch
                || Seen::of(ui) != ask.seen
                || !still
                || ui.busy()
                || ui.dialog_open()
            {
                return;
            }
            match counted {
                Ok((leaving, listed, worktree, parents)) => {
                    ask.parents = parents;
                    ask.leaving = leaving;
                    ask.listed = listed
                        .into_iter()
                        .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
                        .collect();
                    ask.worktree = worktree;
                    prompt::ask(&mut tab.ui, prompt::Kind::Reverse(ask), cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

pub(crate) fn subject_of(ui: &Ui, oid: &str) -> String {
    let inspected = matches!(&ui.selection, Some(Selection::Commit(selected)) if selected == oid);
    inspected
        .then(|| ui.inspection.subject.clone())
        .flatten()
        .or_else(|| ui.history.iter().find(|row| row.oid == oid).map(|row| row.subject.clone()))
        .unwrap_or_default()
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    let ask = |ui: &Ui, operation, head: &str, target: &str| Ask {
        operation,
        branch: ui.branch.clone(),
        seen: Seen::of(ui),
        head: head.to_owned(),
        target: target.to_owned(),
        subject: subject_of(ui, target),
        upstream: None,
        leaving: 0,
        listed: Vec::new(),
        worktree: Worktree::default(),
        parents: Vec::new(),
        commit_now: true,
        mainline: None,
    };
    match action {
        Action::Start(operation) => {
            let (Some(head), Some(Selection::Commit(target))) = (&tab.ui.head, &tab.ui.selection) else {
                return;
            };
            if tab.ui.detached {
                return;
            }
            let ask = ask(&tab.ui, operation, head, target);
            count_then_ask(tab, ask, cx);
        }
        Action::ResetToUpstream => {
            if let Some(reason) = upstream_problem(&tab.ui) {
                tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
                cx.notify();
                return;
            }
            let (Some(head), Some(Upstream { full, shown, tip: Some(tip) })) = (tab.ui.head.clone(), upstream(&tab.ui))
            else {
                return;
            };
            let mut ask = ask(&tab.ui, Operation::HardReset, &head, &tip);
            ask.upstream = Some((full, shown));
            count_then_ask(tab, ask, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn notebook() -> (Fixture, String, String) {
        let f = Fixture::new();
        for (path, text, subject) in [
            ("notes.txt", "First page\n", "Start notebook"),
            ("checklist.txt", "Review notes\n", "Add checklist"),
        ] {
            fs::write(f.dir.join(path), text).unwrap();
            f.git(&["add", "--", path]);
            f.git(&["commit", "-qm", subject]);
        }
        let base = oid(&f, "HEAD^");
        let head = oid(&f, "HEAD");
        (f, base, head)
    }

    fn remote_ref(name: &str) -> OsString {
        let mut full = OsString::from("refs/remotes/");
        full.push(name);
        full
    }

    fn oid(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name]))
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn prompt(operation: Operation, head: &str, target: &str) -> Ask {
        Ask {
            operation,
            seen: Seen {
                head_ref: Some("refs/heads/main".into()),
                head: Some(head.into()),
            },
            head: head.into(),
            target: target.into(),
            branch: "main".into(),
            subject: String::new(),
            upstream: None,
            leaving: 1,
            listed: Vec::new(),
            worktree: Worktree::default(),
            parents: Vec::new(),
            commit_now: true,
            mainline: None,
        }
    }

    fn counted(f: &Fixture, head: &str, target: &str) -> Ask {
        let mut ask = prompt(Operation::HardReset, head, target);
        ask.worktree = worktree(&f.repo, target).unwrap();
        ask
    }

    #[test]
    fn soft_reset_preserves_both_versions_of_a_partially_staged_file() {
        let (f, base, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Staged revision\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        fs::write(f.dir.join("notes.txt"), "Working revision\n").unwrap();
        let index = f.git(&["ls-files", "--stage", "-z"]);
        let outcome = prompt(Operation::SoftReset, &head, &base)
            .execute(&f.repo)
            .unwrap();
        assert_eq!(outcome.title, format!("Reset main to {}", notice::short(&base)));
        assert_eq!(outcome.detail, "1 commit left main. Their changes are staged.");
        assert_eq!(
            f.git(&["rev-parse", "HEAD"]),
            format!("{base}\n").as_bytes()
        );
        assert_eq!(
            f.git(&["rev-parse", "ORIG_HEAD"]),
            format!("{head}\n").as_bytes()
        );
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(
            fs::read(f.dir.join("notes.txt")).unwrap(),
            b"Working revision\n"
        );
    }

    #[test]
    fn mixed_reset_keeps_the_files_and_unstages_everything() {
        let (f, base, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Staged revision\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        let outcome = prompt(Operation::MixedReset, &head, &base)
            .execute(&f.repo)
            .unwrap();
        assert_eq!(outcome.detail, "1 commit left main. Their changes are in your files, unstaged.");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{base}\n").as_bytes());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert!(f.git(&["diff", "--cached", "--name-only"]).is_empty());
        assert_eq!(f.git(&["ls-files"]), b"notes.txt\n");
        assert_eq!(
            f.git(&["status", "--porcelain=v1"]),
            b" M notes.txt\n?? checklist.txt\n"
        );
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"Staged revision\n");
        assert_eq!(fs::read(f.dir.join("checklist.txt")).unwrap(), b"Review notes\n");
    }

    #[test]
    fn a_mixed_reset_warns_that_a_file_edited_after_staging_loses_its_staged_version() {
        let (f, base, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Staged revision\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        fs::write(f.dir.join("notes.txt"), "Working revision\n").unwrap();
        fs::write(f.dir.join("checklist.txt"), "Staged only\n").unwrap();
        f.git(&["add", "--", "checklist.txt"]);
        let stale = prompt(Operation::MixedReset, &head, &base);
        let err = stale.execute(&f.repo).unwrap_err();
        assert!(err.contains("not the ones the prompt counted"), "{err}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{head}\n").as_bytes());
        let mut ask = prompt(Operation::MixedReset, &head, &base);
        ask.worktree = worktree(&f.repo, &base).unwrap();
        assert_eq!(ask.worktree, Worktree { changed: 2, staged: 2, partial: 1, untracked: 0, replaced: 0, ignored: 0 });
        let shown = spec(&ask);
        assert!(shown.danger);
        assert_eq!(
            shown.lines[2..],
            [
                "The staged changes to 1 file become unstaged.",
                "1 file was edited after it was staged. Its staged version will be lost; the file keeps your latest edits.",
            ]
        );
        ask.execute(&f.repo).unwrap();
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{base}\n").as_bytes());
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"Working revision\n");
    }

    #[test]
    fn hard_reset_matches_the_target_and_keeps_untracked_files() {
        let (f, base, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Unsaved thoughts\n").unwrap();
        fs::write(f.dir.join("checklist.txt"), "Staged edit\n").unwrap();
        f.git(&["add", "--", "checklist.txt"]);
        fs::write(f.dir.join("scratch.txt"), "Keep me\n").unwrap();
        let ask = counted(&f, &head, &base);
        assert_eq!(ask.worktree, Worktree { changed: 2, staged: 1, partial: 0, untracked: 1, replaced: 0, ignored: 0 });
        let outcome = ask.execute(&f.repo).unwrap();
        assert_eq!(
            outcome.detail,
            format!("1 commit left main. Your files match {}. Your untracked file was kept.", notice::short(&base))
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{base}\n").as_bytes());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert!(f.git(&["diff", "--cached", "--name-only", &base]).is_empty());
        assert!(f.git(&["diff", "--name-only"]).is_empty());
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"?? scratch.txt\n");
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"First page\n");
        assert!(!f.dir.join("checklist.txt").exists());
        assert_eq!(fs::read(f.dir.join("scratch.txt")).unwrap(), b"Keep me\n");
    }

    #[test]
    fn an_untracked_file_the_target_has_is_counted_as_replaced() {
        let (f, base, head) = notebook();
        f.git(&["reset", "-q", "--hard", &base]);
        fs::write(f.dir.join("checklist.txt"), "My own checklist\n").unwrap();
        fs::write(f.dir.join("other.txt"), "Mine\n").unwrap();
        let mut ask = prompt(Operation::HardReset, &base, &head);
        ask.seen.head = Some(base.clone());
        ask.worktree = worktree(&f.repo, &head).unwrap();
        assert_eq!(ask.worktree, Worktree { changed: 0, staged: 0, partial: 0, untracked: 2, replaced: 1, ignored: 0 });
        let lines = spec(&ask).lines;
        assert!(lines.contains(&"1 untracked file will be replaced by the version in this commit.".to_owned()), "{lines:?}");
        assert!(lines.contains(&"The other untracked file is kept.".to_owned()), "{lines:?}");
        ask.execute(&f.repo).unwrap();
        assert_eq!(fs::read(f.dir.join("checklist.txt")).unwrap(), b"Review notes\n");
        assert_eq!(fs::read(f.dir.join("other.txt")).unwrap(), b"Mine\n");
    }

    #[test]
    fn an_ignored_file_the_target_has_is_counted_as_replaced() {
        let (f, base, _) = notebook();
        f.git(&["rm", "-q", "--cached", "--", "notes.txt"]);
        fs::write(f.dir.join(".gitignore"), "notes.txt\nbuild.log\n").unwrap();
        f.git(&["add", "--", ".gitignore"]);
        f.git(&["commit", "-qm", "Stop tracking notes"]);
        let head = oid(&f, "HEAD");
        fs::write(f.dir.join("notes.txt"), "My own notes\n").unwrap();
        fs::write(f.dir.join("build.log"), "Ignored, not in any commit\n").unwrap();
        let mut ask = prompt(Operation::HardReset, &head, &base);
        ask.worktree = worktree(&f.repo, &base).unwrap();
        assert_eq!(ask.worktree, Worktree { changed: 0, staged: 0, partial: 0, untracked: 0, replaced: 0, ignored: 1 });
        let lines = spec(&ask).lines;
        assert!(lines.contains(&"1 ignored file will be replaced by the version in this commit.".to_owned()), "{lines:?}");
        ask.execute(&f.repo).unwrap();
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"First page\n");
        assert_eq!(fs::read(f.dir.join("build.log")).unwrap(), b"Ignored, not in any commit\n");
    }

    #[test]
    fn a_hard_reset_refuses_changes_it_did_not_count() {
        let (f, base, head) = notebook();
        let ask = counted(&f, &head, &base);
        fs::write(f.dir.join("notes.txt"), "Written after the prompt opened\n").unwrap();
        let err = ask.execute(&f.repo).unwrap_err();
        assert!(err.contains("not the ones the prompt counted"), "{err}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{head}\n").as_bytes());
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"Written after the prompt opened\n");
        f.git(&["checkout", "--", "notes.txt"]);
        let ask = counted(&f, &head, &base);
        fs::write(f.dir.join("new.txt"), "New\n").unwrap();
        assert!(ask.execute(&f.repo).is_err());
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{head}\n").as_bytes());
        assert!(f.dir.join("checklist.txt").exists());
    }

    #[test]
    fn a_reset_to_the_upstream_refuses_a_remote_branch_that_moved() {
        let (f, base, head) = notebook();
        f.git(&["update-ref", "refs/remotes/origin/main", &base]);
        let mut ask = counted(&f, &head, &base);
        ask.upstream = Some((remote_ref("origin/main"), "origin/main".into()));
        f.git(&["update-ref", "refs/remotes/origin/main", &head]);
        let err = ask.execute(&f.repo).unwrap_err();
        assert!(err.contains("origin/main changed"), "{err}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{head}\n").as_bytes());
        f.git(&["update-ref", "refs/remotes/origin/main", &base]);
        let outcome = ask.execute(&f.repo).unwrap();
        assert_eq!(outcome.title, "Reset main to origin/main");
        assert_eq!(f.git(&["rev-parse", "main"]), format!("{base}\n").as_bytes());
        assert!(f.git(&["status", "--porcelain=v1"]).is_empty());
    }

    #[test]
    fn every_operation_rejects_a_changed_branch_or_tip() {
        let (f, base, head) = notebook();
        let all = [Operation::SoftReset, Operation::MixedReset, Operation::HardReset, Operation::Revert];
        f.git(&["switch", "-qc", "other"]);
        for op in all {
            let err = prompt(op, &head, &base).execute(&f.repo).unwrap_err();
            assert!(err.contains("tip changed"), "{err}");
        }
        assert_eq!(
            f.git(&["rev-parse", "other"]),
            format!("{head}\n").as_bytes()
        );
        f.git(&["switch", "-q", "main"]);
        f.git(&["commit", "--allow-empty", "-qm", "Later work"]);
        let tip = f.git(&["rev-parse", "main"]);
        for op in all {
            let err = prompt(op, &head, &base).execute(&f.repo).unwrap_err();
            assert!(err.contains("tip changed"), "{err}");
        }
        assert_eq!(f.git(&["rev-parse", "main"]), tip);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert!(f.dir.join("checklist.txt").exists());
    }

    #[test]
    fn the_prompts_say_what_each_mode_keeps_and_loses() {
        let (head, target) = ("a".repeat(40), "b".repeat(40));
        let mut ask = prompt(Operation::MixedReset, &head, &target);
        ask.subject = "Start notebook".into();
        ask.leaving = 2;
        ask.worktree = Worktree { changed: 3, staged: 1, partial: 0, untracked: 2, replaced: 0, ignored: 0 };
        let shown = spec(&ask);
        assert_eq!(shown.title, "Reset main and clear the staging area?");
        assert!(!shown.danger);
        assert_eq!(
            shown.lines,
            [
                "bbbbbbb  Start notebook",
                "2 commits leave main. Their changes stay in your files, unstaged.",
                "The staged changes to 1 file become unstaged. Your files do not change.",
            ]
        );
        ask.operation = Operation::HardReset;
        ask.listed = vec![("ccccccc".into(), "Add checklist".into())];
        let shown = spec(&ask);
        assert_eq!(shown.title, "Reset main and discard all changes?");
        assert_eq!(shown.confirm.as_ref(), "Discard and reset");
        assert!(shown.danger);
        assert_eq!(
            shown.lines,
            [
                "bbbbbbb  Start notebook",
                "2 commits leave main. Changes in 3 files will be discarded. Undo, in the notice that follows, brings them back.",
                "ccccccc  Add checklist",
                "and 1 more commit",
                "Your 2 untracked files are kept.",
            ]
        );
        ask.upstream = Some((remote_ref("origin/main"), "origin/main".into()));
        ask.worktree = Worktree::default();
        ask.leaving = 1;
        let shown = spec(&ask);
        assert_eq!(shown.title, "Reset main to origin/main?");
        assert_eq!(shown.confirm.as_ref(), "Reset to origin/main");
        assert_eq!(shown.lines[..2], ["origin/main is at bbbbbbb  Start notebook", "1 commit leaves main. Undo, in the notice that follows, brings it back."]);
        ask.operation = Operation::SoftReset;
        ask.upstream = None;
        ask.target = head.clone();
        assert_eq!(spec(&ask).lines[1], "The branch is already at this commit.");
    }

    #[test]
    fn the_three_modes_share_one_submenu_and_only_hard_is_red() {
        let modes: Vec<_> = ["commit.reset-soft", "commit.reset-mixed", "commit.reset-hard"]
            .into_iter()
            .map(|id| crate::commands::find(id).unwrap_or_else(|| panic!("{id}")))
            .collect();
        for command in &modes {
            assert_eq!(command.submenu.map(|sub| sub.id), Some("commit.reset"), "{}", command.id);
        }
        assert_eq!(modes.iter().map(|command| command.danger).collect::<Vec<_>>(), [false, false, true]);
        assert!(crate::commands::find("commit.reset").is_none());
        assert!(crate::commands::find("branch.reset-to-upstream").is_some_and(|command| command.danger));
    }

    #[test]
    fn many_untracked_files_are_counted_without_a_stall() {
        let (f, base, head) = notebook();
        let long = "a-rather-long-untracked-file-name-to-fill-the-pipe".repeat(2);
        for i in 0..1500 {
            fs::write(f.dir.join(format!("{long}-{i}.txt")), "x\n").unwrap();
        }
        fs::write(f.dir.join("checklist.txt"), "Mine\n").unwrap();
        f.git(&["rm", "-q", "--cached", "--", "checklist.txt"]);
        let counted = worktree(&f.repo, &head).unwrap();
        assert_eq!(counted, Worktree { changed: 1, staged: 1, partial: 0, untracked: 1501, replaced: 1, ignored: 0 });
        assert_eq!(worktree(&f.repo, &base).unwrap().replaced, 0);
    }

    #[test]
    fn conflicted_revert_reports_failure_and_keeps_git_recovery_state() {
        let (f, base, addition) = notebook();
        fs::write(f.dir.join("checklist.txt"), "Later checklist edit\n").unwrap();
        f.git(&["commit", "-qam", "Revise checklist"]);
        let tip = oid(&f, "HEAD");
        let tip = tip.as_str();
        assert!(
            prompt(Operation::Revert, tip, &addition)
                .execute(&f.repo)
                .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "REVERT_HEAD"]),
            format!("{addition}\n").as_bytes()
        );
        assert_eq!(
            f.git(&["diff", "--diff-filter=U", "--name-only"]),
            b"checklist.txt\n"
        );
        for op in [Operation::SoftReset, Operation::MixedReset, Operation::HardReset] {
            assert!(prompt(op, tip, &base).execute(&f.repo).is_err());
        }
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
        f.git(&["revert", "--abort"]);
        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert_eq!(
            fs::read(f.dir.join("checklist.txt")).unwrap(),
            b"Later checklist edit\n"
        );
    }

    fn waypoints() -> (Fixture, String, String) {
        let f = Fixture::new();
        for (text, subject) in [("base\n", "Base"), ("second\n", "Second"), ("third\n", "Third")] {
            fs::write(f.dir.join("trail.txt"), text).unwrap();
            f.git(&["add", "--", "trail.txt"]);
            f.git(&["commit", "-qm", subject]);
        }
        fs::write(f.dir.join("notes.txt"), "untracked notes\n").unwrap();
        let (second, tip) = (oid(&f, "HEAD~1"), oid(&f, "HEAD"));
        (f, second, tip)
    }

    fn revert_of(head: &str, target: &str, commit_now: bool) -> Ask {
        let mut ask = prompt(Operation::Revert, head, target);
        ask.commit_now = commit_now;
        ask
    }

    fn git_dir_has(f: &Fixture, name: &str) -> bool {
        f.repo.git_dir().join(name).exists()
    }

    #[test]
    fn a_conflicted_revert_stops_for_the_panel_and_is_committed_there() {
        let (f, second, tip) = waypoints();
        let err = revert_of(&tip, &second, true).revert(&f.repo).err().expect("the revert conflicts");
        assert_eq!(err.text, "The revert stopped on a conflict. 1 file needs resolving before it can go on.");
        assert!(err.output.as_deref().is_some_and(|out| out.contains("CONFLICT")), "{:?}", err.output);
        let stopped = operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(stopped.kind, operation::Kind::Revert);
        assert_eq!(stopped.commit.as_ref().map(|c| c.oid.as_str()), Some(second.as_str()));
        assert!(!stopped.uncommitted);
        assert_eq!(stopped.continue_label(), Some("Commit revert"));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
        fs::write(f.dir.join("trail.txt"), "base and third\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        let ready = operation::snapshot(&f.repo).unwrap().unwrap();
        let outcome = operation::backend::finish(&f.repo, &ready, ready.message.clone()).unwrap();
        assert_eq!(outcome.title, "Revert committed");
        assert_eq!(f.git(&["rev-parse", "HEAD^"]), format!("{tip}\n").as_bytes());
        assert!(String::from_utf8(f.git(&["log", "-1", "--format=%B"])).unwrap().starts_with("Revert \"Second\""));
        assert!(!git_dir_has(&f, "REVERT_HEAD"));
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"?? notes.txt\n");
    }

    #[test]
    fn a_revert_left_uncommitted_stages_the_reversal_and_hands_over_the_message() {
        let (f, _, head) = notebook();
        let reverted = revert_of(&head, &head, false).revert(&f.repo).ok().expect("the revert applies");
        assert_eq!(reverted.outcome.title, "Revert staged");
        assert_eq!(
            reverted.outcome.detail,
            format!("The changes that undo {} are staged on main. Commit them when you are ready.", &head[..7])
        );
        assert_eq!(reverted.staged, Some(format!("Revert \"Add checklist\"\n\nThis reverts commit {head}.")));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{head}\n").as_bytes());
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"D  checklist.txt\n");
        for left in ["REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE", "sequencer"] {
            assert!(!git_dir_has(&f, left), "{left} was left behind");
        }
        assert!(f.repo.operation().unwrap().is_none());
    }

    #[test]
    fn a_revert_left_uncommitted_refuses_staged_changes() {
        let (f, _, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Staged edit\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        let index = f.git(&["ls-files", "--stage"]);
        let err = revert_of(&head, &head, false).revert(&f.repo).err().expect("refused");
        assert!(err.text.contains("staged changes"), "{}", err.text);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert!(f.dir.join("checklist.txt").exists());
        assert!(!git_dir_has(&f, "REVERT_HEAD"));
    }

    #[test]
    fn a_conflicted_revert_left_uncommitted_finishes_staged_or_aborts() {
        let (f, second, _) = waypoints();
        fs::write(f.dir.join("other.txt"), "x\n").unwrap();
        f.git(&["add", "--", "other.txt"]);
        f.git(&["commit", "-qm", "Other"]);
        let tip = oid(&f, "HEAD");
        fs::write(f.dir.join("other.txt"), "unstaged edit\n").unwrap();
        let before = (f.git(&["status", "--porcelain=v1"]), f.git(&["ls-files", "--stage"]));
        assert!(revert_of(&tip, &second, false).revert(&f.repo).is_err());
        let mut state = operation::State::default();
        state.uncommitted = Some(second.clone());
        state.update(operation::snapshot(&f.repo).unwrap());
        let stopped = state.snapshot().unwrap().clone();
        assert!(stopped.uncommitted);
        assert_eq!(stopped.headline(), format!("Reverting {} Second on main, not committed", &second[..7]));
        assert_eq!(state.continue_label(), Some(operation::UNCOMMITTED));
        let outcome = operation::backend::abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Revert aborted");
        assert_eq!((f.git(&["status", "--porcelain=v1"]), f.git(&["ls-files", "--stage"])), before);
        assert_eq!(fs::read(f.dir.join("other.txt")).unwrap(), b"unstaged edit\n");

        assert!(revert_of(&tip, &second, false).revert(&f.repo).is_err());
        let early = operation::backend::finish(&f.repo, &stopped, None).unwrap_err();
        assert_eq!(early.text, "Resolve every conflicted file first.");
        fs::write(f.dir.join("trail.txt"), "base and third\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        state.update(operation::snapshot(&f.repo).unwrap());
        let ready = state.snapshot().unwrap().clone();
        assert!(ready.uncommitted);
        let outcome = operation::backend::finish(&f.repo, &ready, ready.message.clone()).unwrap();
        assert_eq!(outcome.title, "Revert staged");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
        assert_eq!(f.git(&["status", "--porcelain=v1", "--", "trail.txt"]), b"M  trail.txt\n");
        assert_eq!(f.git(&["show", ":trail.txt"]), b"base and third\n");
        assert!(!git_dir_has(&f, "REVERT_HEAD") && !git_dir_has(&f, "MERGE_MSG"));
        state.update(operation::snapshot(&f.repo).unwrap());
        assert!(state.snapshot().is_none() && state.uncommitted.is_none());
    }

    #[test]
    fn an_empty_revert_left_uncommitted_is_never_committed() {
        let (f, second, tip) = waypoints();
        assert!(revert_of(&tip, &second, false).revert(&f.repo).is_err());
        f.git(&["checkout", "--ours", "--", "trail.txt"]);
        f.git(&["add", "--", "trail.txt"]);
        let mut state = operation::State::default();
        state.uncommitted = Some(second.clone());
        state.update(operation::snapshot(&f.repo).unwrap());
        let stopped = state.snapshot().unwrap().clone();
        assert!(stopped.empty && stopped.uncommitted);
        let refused = operation::backend::keep_empty(&f.repo, &stopped, stopped.message.clone()).unwrap_err();
        assert_eq!(refused.text, "This revert was started without committing. Skip it or abort it.");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
        assert!(git_dir_has(&f, "REVERT_HEAD"));
        let skipped = operation::backend::skip(&f.repo, &stopped).unwrap();
        assert_eq!(skipped.title, format!("Skipped {} Second", &second[..7]));
        assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
        assert!(!git_dir_has(&f, "REVERT_HEAD"));
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"?? notes.txt\n");
    }

    #[test]
    fn a_revert_of_changes_already_undone_says_so_and_leaves_nothing_behind() {
        let (f, _, head) = notebook();
        f.git(&["rm", "-q", "--", "checklist.txt"]);
        f.git(&["commit", "-qm", "Drop the checklist by hand"]);
        let tip = oid(&f, "HEAD");
        for commit_now in [true, false] {
            let reverted = revert_of(&tip, &head, commit_now).revert(&f.repo).ok().expect("reported");
            assert_eq!(reverted.outcome.title, "Already undone");
            assert_eq!(reverted.staged, None);
            assert_eq!(f.git(&["rev-parse", "HEAD"]), format!("{tip}\n").as_bytes());
            assert!(f.git(&["status", "--porcelain=v1"]).is_empty());
            for left in ["REVERT_HEAD", "MERGE_MSG", "AUTO_MERGE"] {
                assert!(!git_dir_has(&f, left), "{left} was left behind ({commit_now})");
            }
        }
    }

    fn merged(message: &str) -> (Fixture, String) {
        let (f, _, _) = notebook();
        f.git(&["switch", "-qc", "feature"]);
        fs::write(f.dir.join("ferry.txt"), "Ferry\n").unwrap();
        f.git(&["add", "--", "ferry.txt"]);
        f.git(&["commit", "-qm", "Add the ferry"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("bridge.txt"), "Bridge\n").unwrap();
        f.git(&["add", "--", "bridge.txt"]);
        f.git(&["commit", "-qm", "Add the bridge"]);
        f.git(&["merge", "-q", "--no-ff", "-m", message, "feature"]);
        let merge = oid(&f, "HEAD");
        (f, merge)
    }

    #[test]
    fn a_merge_is_reverted_against_the_side_chosen() {
        let (f, merge) = merged("Merge branch 'feature' into main");
        let parents = merge_parents(&f.repo, &merge).unwrap();
        assert_eq!(parents.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["main", "feature"]);
        let mut ask = revert_of(&merge, &merge, true);
        ask.parents = parents;
        for mainline in [None, Some(3)] {
            ask.mainline = mainline;
            assert!(ask.revert(&f.repo).err().unwrap().text.contains("Choose which side"));
        }
        ask.mainline = Some(1);
        let reverted = ask.revert(&f.repo).ok().unwrap();
        assert_eq!(reverted.outcome.title, "Revert committed");
        assert_eq!(f.git(&["diff", "--name-status", "HEAD^", "HEAD"]), b"D\tferry.txt\n");
        assert_eq!(f.git(&["rev-parse", "HEAD^"]), format!("{merge}\n").as_bytes());

        f.git(&["reset", "-q", "--hard", &merge]);
        ask.mainline = Some(2);
        ask.revert(&f.repo).ok().unwrap();
        assert_eq!(f.git(&["diff", "--name-status", "HEAD^", "HEAD"]), b"D\tbridge.txt\n");
        assert!(f.repo.operation().unwrap().is_none());
    }

    #[test]
    fn a_merge_revert_prompt_offers_each_side_and_the_tick() {
        let (f, merge) = merged("Merge branch 'feature'");
        let mut ask = revert_of(&merge, &merge, true);
        ask.subject = "Merge branch 'feature'".into();
        ask.parents = merge_parents(&f.repo, &merge).unwrap();
        let first = oid(&f, "HEAD^1");
        assert_eq!(ask.parents[0], Parent { oid: first.clone(), name: first[..7].to_owned() });
        let shown = spec(&ask);
        assert_eq!(shown.title, "Revert this merge on main?");
        assert_eq!(shown.confirm.as_ref(), "Undo what feature brought in");
        assert_eq!(shown.choices.iter().map(|c| c.label.to_string()).collect::<Vec<_>>(), [format!("Undo what {} brought in", &first[..7])]);
        assert!(shown.choices.iter().all(|c| !c.danger) && !shown.danger);
        assert_eq!(shown.tick.as_ref().map(|(label, on)| (label.to_string(), *on)), Some(("Commit the revert straight away".into(), true)));

        let plain = revert_of(&first, &first, true);
        let shown = spec(&plain);
        assert_eq!((shown.title.as_str(), shown.confirm.as_ref()), ("Revert this commit on main?", "Revert commit"));
        assert!(shown.choices.is_empty() && shown.tick.is_some());
        assert_eq!(merge_parents(&f.repo, &first).unwrap(), []);
    }

    #[test]
    fn a_merge_subject_names_what_it_joined() {
        let names = |subject: &str| merged_names(subject);
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(names("Merge branch 'feature/x' into main"), (some("feature/x"), some("main")));
        assert_eq!(names("Merge branch 'feature'"), (some("feature"), None));
        assert_eq!(names("Merge remote-tracking branch 'origin/main' into topic"), (some("origin/main"), some("topic")));
        assert_eq!(names("Merge pull request #12 from owner/feature"), (some("owner/feature"), None));
        assert_eq!(names("Merge tag 'v1.0'"), (some("v1.0"), None));
        assert_eq!(names("Fix the bridge"), (None, None));
        assert_eq!(names("Merge branch ''"), (None, None));
    }

    #[test]
    fn revert_preserves_a_pending_staged_edit_when_git_refuses_it() {
        let (f, _, head) = notebook();
        fs::write(f.dir.join("notes.txt"), "Private working notes\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        let index = f.git(&["ls-files", "--stage", "-z"]);
        assert!(
            prompt(Operation::Revert, &head, &head)
                .execute(&f.repo)
                .is_err()
        );
        assert_eq!(
            f.git(&["rev-parse", "HEAD"]),
            format!("{head}\n").as_bytes()
        );
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(
            fs::read(f.dir.join("notes.txt")).unwrap(),
            b"Private working notes\n"
        );
        assert!(f.dir.join("checklist.txt").is_file());
    }

    #[test]
    fn a_revert_with_changes_in_its_way_stashes_them_when_asked_and_puts_them_back() {
        let f = Fixture::new();
        let rev = |name: &str| String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().to_owned();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit\n").unwrap();
        f.git(&["add", "--", "route.txt"]);
        f.git(&["commit", "-qm", "Plan the route"]);
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nSummit\n").unwrap();
        f.git(&["commit", "-qam", "Start north"]);
        let north = rev("HEAD");
        fs::write(f.dir.join("camp.txt"), "Camp\n").unwrap();
        f.git(&["add", "--", "camp.txt"]);
        f.git(&["commit", "-qm", "Pitch camp"]);
        let head = rev("HEAD");
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nSummit east\n").unwrap();
        let kept = crate::autostash::Kept::default();
        let undo = revert_of(&head, &north, true);
        let refused = undo.attempt(&f.repo, false, &kept).err().expect("Git refuses");
        assert!(crate::autostash::in_the_way(&refused), "{}", refused.text);
        assert!(matches!(
            &refused.actions[..],
            [(_, view::Action::Autostash(crate::autostash::Action::Retry(crate::autostash::Retry::Revert(_))))]
        ));
        assert_eq!(rev("HEAD"), head);

        let done = undo.attempt(&f.repo, true, &kept).ok().expect("the revert applies");
        assert_eq!(done.outcome.title, format!("Stashed your changes, reverted {} and restored them", &north[..7]));
        assert_eq!(rev("HEAD^"), head);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(kept.take().is_none());
    }
}
