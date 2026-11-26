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
    .icon("commit"),
    Command::new(
        "commits.cherry-pick",
        Group::Switch,
        |ui, target| {
            let count = crate::compare::commits_of(target).map_or(0, |oids| wanted(ui, oids).len());
            match on_branch(ui) {
                true => format!("Cherry-pick {} onto {}…", commits(count), ui.branch),
                false => format!("Cherry-pick {}…", commits(count)),
            }
        },
        |ui, target, _| {
            let Some(oids) = crate::compare::commits_of(target) else {
                return Availability::Hidden;
            };
            if wanted(ui, oids).is_empty() {
                return Availability::Hidden;
            }
            Availability::unless(
                busy(ui)
                    .or_else(|| operating(ui))
                    .or_else(|| (!on_branch(ui)).then(|| "Check out a branch first".to_owned()))
                    .or_else(|| holds_merge(ui, oids).then(|| MERGE_ALONE.to_owned())),
            )
        },
        |_, target| crate::compare::commits_of(target).map(|oids| view::Action::Pick(Action::Several(oids.to_vec()))),
    )
    .words(&["cherry pick", "pick commits", "copy commits", "backport", "several"])
    .icon("commit")]
};

pub(crate) const MERGE_ALONE: &str = "Cherry-pick a merge on its own";
const LISTED: usize = 12;

fn commits(count: usize) -> String {
    match count {
        1 => "1 commit".to_owned(),
        n => format!("{n} commits"),
    }
}

fn wanted<'a>(ui: &Ui, oids: &'a [String]) -> Vec<&'a String> {
    let on_head = ui.head.as_deref().map(|head| reached_from(&ui.history, head, oids)).unwrap_or_default();
    oids.iter().filter(|oid| !on_head.contains(oid.as_str())).collect()
}

pub(crate) fn reached_from<'a>(history: &[view::HistoryRow], head: &str, oids: &'a [String]) -> HashSet<&'a str> {
    let picked: HashSet<&str> = oids.iter().map(String::as_str).collect();
    let mut found: HashSet<&str> = picked.iter().copied().filter(|oid| *oid == head).collect();
    let mut passed = 0;
    let mut frontier: Option<HashSet<&str>> = None;
    for row in history {
        let oid = row.oid.as_str();
        let pick = picked.get(oid);
        if frontier.is_none() && oid == head {
            frontier = Some(HashSet::from([head]));
        }
        if let Some(frontier) = frontier.as_mut()
            && frontier.remove(oid)
        {
            if let Some(pick) = pick {
                found.insert(pick);
            }
            frontier.extend(row.parents.iter().map(String::as_str));
        }
        passed += usize::from(pick.is_some());
        if passed == picked.len() || frontier.as_ref().is_some_and(HashSet::is_empty) {
            break;
        }
    }
    found
}

fn holds_merge(ui: &Ui, oids: &[String]) -> bool {
    if oids == crate::compare::picked_commits(ui) {
        return ui.compare.merges();
    }
    oids.iter().any(|oid| ui.history.iter().any(|row| &row.oid == oid && row.parents.len() > 1))
}

pub(crate) fn offered(ui: &Ui, oids: &[String], cx: &gpui::App) -> Option<crate::commands::Entry> {
    let command = crate::commands::find("commits.cherry-pick")?;
    crate::commands::entry(command, ui, &crate::commands::Target::Commits(oids.to_vec()), cx)
}

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

#[derive(Clone, Debug)]
pub enum Action {
    Start,
    Several(Vec<String>),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub oid: String,
    pub subject: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Series {
    branch: String,
    seen: Seen,
    head: String,
    commits: Vec<Listed>,
    left_out: usize,
    message: String,
}

impl Series {
    pub(crate) fn commits(&self) -> &[Listed] {
        &self.commits
    }

    fn oids(&self) -> Vec<&str> {
        self.commits.iter().map(|commit| commit.oid.as_str()).collect()
    }

    fn single(&self) -> Option<Ask> {
        let [only] = self.commits.as_slice() else {
            return None;
        };
        Some(Ask {
            branch: self.branch.clone(),
            seen: self.seen.clone(),
            head: self.head.clone(),
            target: only.oid.clone(),
            subject: only.subject.clone(),
            message: self.message.clone(),
            parents: Vec::new(),
            commit_now: true,
            mainline: None,
        })
    }

    pub(crate) fn pick(&self, repo: &Repository) -> Result<Outcome, Failure> {
        validate_oid(&self.head)?;
        for commit in &self.commits {
            validate_oid(&commit.oid)?;
        }
        if self.commits.is_empty() {
            return Err(Failure::from("There is nothing to cherry-pick.".to_owned()));
        }
        if self.seen.head_ref.is_none() || self.seen.head.as_deref() != Some(self.head.as_str()) {
            return Err(Failure::from("Check out a branch first.".to_owned()));
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&self.seen)?;
        let oids = self.oids();
        if !merges_among(repo, &oids)?.is_empty() {
            return Err(Failure::from(format!("{MERGE_ALONE}.")));
        }
        overwritten(repo, &oids)?;
        let mut command = repo.command();
        command.env("GIT_EDITOR", "true").env("GIT_MERGE_AUTOEDIT", "no").stdin(Stdio::null());
        command.args(["cherry-pick", "--no-edit", "--empty=drop"]).args(&oids);
        let output = command.run().map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let gits_words = || {
            let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            let text = if text.is_empty() { printed.trim().to_owned() } else { text };
            if text.is_empty() { format!("Git exited with {}", output.status) } else { text }
        };
        let head = head_of(repo)?;
        match operation::snapshot(repo)? {
            Some(now) if now.kind == Operation::CherryPick => {
                let at = now.step.map(|(at, of)| format!(" at commit {at} of {of}")).unwrap_or_default();
                if !now.conflicts.is_empty() {
                    return Err(Failure::from(format!(
                        "The cherry-pick stopped on a conflict{at}. {} resolving before it can go on.",
                        needing(now.conflicts.len())
                    ))
                    .output(printed));
                }
                if now.empty {
                    let commit = now.commit.as_ref().map_or("The next commit", |c| c.label.as_str());
                    return Err(Failure::from(format!("{commit} is now empty. Skip it, or keep it as an empty commit.")).output(printed));
                }
                if head == self.head && !repo.git_dir().join("CHERRY_PICK_HEAD").exists() {
                    operation::backend::quit_pick(repo)?;
                    return Err(Failure::from(gits_words()).output(printed));
                }
                Err(Failure::from(format!("The cherry-pick stopped{at}. Continue it or abort it once you have dealt with this:\n{}", gits_words()))
                    .output(printed))
            }
            Some(_) => Err(Failure::from(gits_words()).output(printed)),
            None if !output.status.success() => Err(Failure::from(gits_words()).output(printed)),
            None => self.landed(repo, &head),
        }
    }

    fn landed(&self, repo: &Repository, head: &str) -> Result<Outcome, Failure> {
        let branch = &self.branch;
        let range = format!("{}..{head}", self.head);
        let added = checked(repo.read_command().args(["rev-list", "--count", "--first-parent", &range, "--"]))?;
        let added: usize = String::from_utf8_lossy(&added).trim().parse().unwrap_or(0);
        if added == 0 {
            return Ok(Outcome::titled(
                format!("{branch} already has these changes"),
                format!("Picking {} would change nothing on {branch}, so nothing was picked.", commits(self.commits.len())),
            ));
        }
        let mut detail = format!("{branch} is at {}.", notice::short(head));
        match self.commits.len().saturating_sub(added) {
            0 => {}
            1 => detail.push_str(&format!(" 1 commit was left out because {branch} already has its changes.")),
            n => detail.push_str(&format!(" {n} commits were left out because {branch} already has their changes.")),
        }
        Ok(Outcome::titled(format!("Cherry-picked {} onto {branch}", commits(added)), detail))
    }

    pub(crate) fn attempt(&self, repo: &Repository, stash: bool, kept: &crate::autostash::Kept) -> Result<Outcome, Failure> {
        let how = crate::autostash::Around { doing: format!("cherry-picking {}", commits(self.commits.len())), stash, kept };
        let midway = std::cell::RefCell::new(None);
        let result = crate::autostash::around(
            repo,
            how,
            || crate::autostash::Retry::Picks(Box::new(self.clone())),
            |repo| {
                let picked = self.pick(repo);
                if let Err(failure) = &picked
                    && crate::autostash::in_the_way(failure)
                    && repo.expect_no_operation().is_err()
                {
                    *midway.borrow_mut() = Some((failure.text.clone(), failure.output.clone()));
                }
                picked
            },
            |outcome| outcome.title.starts_with("Cherry-picked").then(|| crate::autostash::lowered(&outcome.title)),
        );
        match (result, midway.into_inner()) {
            (Err(_), Some((text, output))) => {
                let mut failure = Failure::from(text);
                failure.output = output;
                Err(failure)
            }
            (result, _) => result,
        }
    }
}

fn oldest_first(newest_first: &[String], subject: impl Fn(&str) -> String) -> Vec<Listed> {
    newest_first.iter().rev().map(|oid| Listed { oid: oid.clone(), subject: subject(oid) }).collect()
}

fn head_of(repo: &Repository) -> Result<String, Failure> {
    let head = checked(repo.read_command().args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
    Ok(String::from_utf8_lossy(&head).trim().to_owned())
}

fn merges_among(repo: &Repository, oids: &[&str]) -> Result<Vec<u8>, String> {
    checked(repo.read_command().args(["rev-list", "--no-walk=unsorted", "--min-parents=2"]).args(oids).arg("--"))
}

fn overwritten(repo: &Repository, oids: &[&str]) -> Result<(), Failure> {
    let status = checked(repo.read_command().args(["status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"]))?;
    let changed: HashSet<&[u8]> =
        status.split(|&b| b == 0).filter_map(|record| record.get(3..)).filter(|path| !path.is_empty()).collect();
    if changed.is_empty() {
        return Ok(());
    }
    let mut clashing = std::collections::BTreeSet::new();
    for oid in oids {
        let touched = checked(
            repo.read_command().args(["diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-z", "--no-renames", oid, "--"]),
        )?;
        for path in touched.split(|&b| b == 0).filter(|path| changed.contains(path)) {
            clashing.insert(String::from_utf8_lossy(path).into_owned());
        }
    }
    if clashing.is_empty() {
        return Ok(());
    }
    let listed: Vec<String> = clashing.iter().map(|path| format!("\t{path}")).collect();
    Err(Failure::from(format!(
        "Your local changes to the following files would be overwritten by cherry-pick:\n{}\nCommit or stash them first.",
        listed.join("\n")
    )))
}

fn left_out_line(series: &Series) -> Option<String> {
    let branch = &series.branch;
    match series.left_out {
        0 => None,
        1 => Some(format!("1 picked commit is already part of {branch}, so it is left out.")),
        n => Some(format!("{n} picked commits are already part of {branch}, so they are left out.")),
    }
}

pub(crate) fn describe_series(series: &Series, _: &Ui) -> Spec {
    series_spec(series)
}

fn series_spec(series: &Series) -> Spec {
    if let Some(ask) = series.single() {
        let mut spec = spec(&ask);
        if let Some(line) = left_out_line(series) {
            spec = spec.line(line);
        }
        return spec;
    }
    let (branch, count) = (&series.branch, series.commits.len());
    let mut spec = Spec::new(format!("Cherry-pick {count} commits onto {branch}?"), format!("Cherry-pick {count} commits"))
        .line("Oldest first:");
    for commit in series.commits.iter().take(LISTED) {
        spec = spec.line(match commit.subject.as_str() {
            "" => notice::short(&commit.oid).to_owned(),
            subject => format!("{}  {subject}", notice::short(&commit.oid)),
        });
    }
    if count > LISTED {
        spec = spec.line(format!("…and {} more", count - LISTED));
    }
    if let Some(line) = left_out_line(series) {
        spec = spec.line(line);
    }
    spec = spec.line(format!(
        "Each becomes a new commit on {branch}. If one conflicts, the cherry-pick stops there for you to resolve, skip or abort. Several commits are always committed straight away; pick one at a time to edit the changes first."
    ));
    spec.tick = Some(("Commit straight away".into(), true));
    spec.fixed_tick = true;
    spec
}

pub(crate) fn confirm_series(tab: &mut Tab, series: Series, answer: Answer, cx: &mut Context<Tab>) {
    let stash = tab.ui.autostash.always;
    match series.single() {
        Some(mut ask) => {
            ask.commit_now = answer.tick;
            run(tab, ask, stash, cx);
        }
        None => run_series(tab, series, stash, cx),
    }
}

pub(crate) fn run_series(tab: &mut Tab, series: Series, stash: bool, cx: &mut Context<Tab>) {
    let restored = crate::autostash::Kept::default();
    let left = restored.clone();
    tab.run_operation_or(
        cx,
        "Cherry-picking the commits…",
        "Cherry-picked",
        true,
        move |repo| {
            let title = format!("Cherry-pick {}", crate::notice::commits(series.commits.len()));
            crate::journal::titled(series.attempt(&repo, stash, &restored), title)
        },
        |_, _| {},
        move |tab, _| left.remember(&mut tab.ui),
    );
}

fn check_series(tab: &mut Tab, mut series: Series, picked: Vec<String>, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (head, listed) = (series.head.clone(), series.commits.clone());
    Tab::work(
        cx,
        move || {
            validate_oid(&head)?;
            let oids: Vec<&str> = listed.iter().map(|commit| commit.oid.as_str()).collect();
            for oid in &oids {
                validate_oid(oid)?;
            }
            if !merges_among(&repo, &oids)?.is_empty() {
                return Err(format!("{MERGE_ALONE}."));
            }
            let mut kept = Vec::new();
            let mut left_out = 0;
            for commit in listed {
                if repo.is_ancestor(&commit.oid, &head)? {
                    left_out += 1;
                } else {
                    kept.push(commit);
                }
            }
            let message = match kept.as_slice() {
                [only] => message_of(&repo, &only.oid)?,
                _ => String::new(),
            };
            Ok::<_, String>((kept, left_out, message))
        },
        move |tab, checked, cx| {
            let ui = &tab.ui;
            if tab.epoch != epoch
                || Seen::of(ui) != series.seen
                || crate::compare::picked_commits(ui) != picked
                || ui.busy()
                || ui.dialog_open()
            {
                return;
            }
            match checked {
                Ok((kept, _, _)) if kept.is_empty() => {
                    let branch = &series.branch;
                    tab.ui.notices.show(Notice::new(
                        Tone::Info,
                        format!("{branch} already has these commits"),
                        format!("They are all part of {branch}'s history, so there is nothing to pick."),
                    ));
                    cx.notify();
                }
                Ok((kept, left_out, message)) => {
                    series.commits = kept;
                    series.left_out = left_out;
                    series.message = message;
                    prompt::ask(&mut tab.ui, prompt::Kind::Picks(series), cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
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
            let title = format!("Cherry-pick {}", crate::notice::short(&ask.target));
            let picked = ask.attempt(&repo, &stop, stash, &restored).map(|picked| {
                *kept.lock().unwrap_or_else(|e| e.into_inner()) = picked.staged;
                picked.outcome
            });
            crate::journal::titled(picked, title)
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
        Action::Several(oids) => {
            let Some(head) = tab.ui.head.clone() else { return };
            if oids.len() < 2 {
                return;
            }
            if !on_branch(&tab.ui) {
                tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
                cx.notify();
                return;
            }
            let ui = &tab.ui;
            let commits = oldest_first(&oids, |oid| reverse::subject_of(ui, oid));
            let series = Series {
                branch: ui.branch.clone(),
                seen: Seen::of(ui),
                head,
                commits,
                left_out: 0,
                message: String::new(),
            };
            check_series(tab, series, oids, cx);
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

    fn trail() -> (Fixture, String, [String; 3]) {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Start\nFord\nSummit\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let camp = commit(&f, "trail.txt", "Start\nFord\nSummit camp\n", "Camp at the summit");
        commit(&f, "lake.txt", "Cold water\n", "Map the lake");
        let amended = f.repo.command().env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00").args(["commit", "-q", "--amend", "--no-edit"]).output().unwrap();
        assert!(amended.status.success());
        let lake = oid(&f, "HEAD");
        f.git(&["switch", "-q", "main"]);
        let head = commit(&f, "bridge.txt", "Stone bridge\n", "Build the bridge");
        (f, head, [ferry, camp, lake])
    }

    fn series(f: &Fixture, head: &str, newest_first: &[String]) -> Series {
        let subject = |oid: &str| String::from_utf8(f.git(&["log", "-1", "--format=%s", oid])).unwrap().trim().to_owned();
        Series {
            branch: "main".into(),
            seen: Seen { head_ref: Some("refs/heads/main".into()), head: Some(head.into()) },
            head: head.into(),
            commits: oldest_first(newest_first, subject),
            left_out: 0,
            message: String::new(),
        }
    }

    fn landed(f: &Fixture, since: &str) -> Vec<String> {
        String::from_utf8(f.git(&["log", "--reverse", "--format=%s", &format!("{since}..HEAD")])).unwrap().lines().map(str::to_owned).collect()
    }

    fn patch_of(f: &Fixture, oid: &str) -> Vec<u8> {
        f.git(&["diff", &format!("{oid}^"), oid])
    }

    #[test]
    fn several_commits_land_oldest_first_in_one_pick_whatever_their_dates() {
        let (f, head, [ferry, camp, lake]) = trail();
        assert_eq!(f.git(&["log", "-1", "--format=%cd", "--date=format:%Y", &lake]), b"2001\n");
        let picks = series(&f, &head, &[lake.clone(), camp.clone(), ferry.clone()]);
        assert_eq!(picks.oids(), [ferry.as_str(), camp.as_str(), lake.as_str()]);
        let outcome = picks.pick(&f.repo).ok().expect("the commits apply");
        let new = oid(&f, "HEAD");
        assert_eq!(outcome.title, "Cherry-picked 3 commits onto main");
        assert_eq!(outcome.detail, format!("main is at {}.", &new[..7]));
        assert_eq!(landed(&f, &head), ["Add the ferry", "Camp at the summit", "Map the lake"]);
        assert_eq!(oid(&f, "HEAD~3"), head);
        for (at, picked) in [(2, &ferry), (1, &camp), (0, &lake)] {
            let landed = oid(&f, &format!("HEAD~{at}"));
            assert_eq!(patch_of(&f, &landed), patch_of(&f, picked));
            assert_eq!(f.git(&["log", "-1", "--format=%an <%ae>%n%B", &landed]), f.git(&["log", "-1", "--format=%an <%ae>%n%B", picked]));
        }
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(status(&f), "");
        assert!(leftovers(&f).is_empty() || leftovers(&f) == ["AUTO_MERGE"], "{:?}", leftovers(&f));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_series_leaves_out_changes_the_branch_already_has() {
        let (f, _, [ferry, camp, lake]) = trail();
        let head = commit(&f, "ferry.txt", "Hourly crossings\n", "Copy the ferry");
        let outcome = series(&f, &head, &[lake.clone(), camp.clone(), ferry.clone()]).pick(&f.repo).ok().expect("picked");
        assert_eq!(outcome.title, "Cherry-picked 2 commits onto main");
        assert!(outcome.detail.ends_with(" 1 commit was left out because main already has its changes."), "{}", outcome.detail);
        assert_eq!(landed(&f, &head), ["Camp at the summit", "Map the lake"]);

        let tip = oid(&f, "HEAD");
        let outcome = series(&f, &tip, &[lake, camp]).pick(&f.repo).ok().expect("reported");
        assert_eq!(outcome.title, "main already has these changes");
        assert_eq!(outcome.detail, "Picking 2 commits would change nothing on main, so nothing was picked.");
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(status(&f), "");
        assert!(leftovers(&f).is_empty() || leftovers(&f) == ["AUTO_MERGE"], "{:?}", leftovers(&f));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_series_stops_at_each_conflict_with_its_step_and_goes_on_from_the_panel() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let east = commit(&f, "trail.txt", "Trail east\n", "Head east");
        let north = commit(&f, "trail.txt", "Trail east, then north\n", "Turn north");
        let lake = commit(&f, "lake.txt", "Cold water\n", "Map the lake");
        f.git(&["switch", "-q", "main"]);
        let head = commit(&f, "trail.txt", "Trail west\n", "Head west");
        let picks = series(&f, &head, &[lake.clone(), north.clone(), east.clone(), ferry.clone()]);
        let err = picks.pick(&f.repo).err().expect("the second commit conflicts");
        assert_eq!(err.text, "The cherry-pick stopped on a conflict at commit 2 of 4. 1 file needs resolving before it can go on.");
        assert!(err.output.as_deref().is_some_and(|output| output.contains("CONFLICT")));
        let shown = operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((shown.kind, shown.step, shown.queued), (Operation::CherryPick, Some((2, 4)), true));
        assert_eq!(shown.summary(), format!("Cherry-picking {} Head east onto main · commit 2 of 4 · 1 file needs resolving", &east[..7]));

        fs::write(f.dir.join("trail.txt"), "Trail west and east\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-q", "--no-edit"]);
        let between = operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!((between.kind, between.step), (Operation::CherryPick, Some((2, 4))));
        let err = operation::backend::finish(&f.repo, &between, between.message.clone()).err().expect("the third commit conflicts");
        assert!(err.text.starts_with("The cherry-pick stopped on a conflict at commit 3 of 4."), "{}", err.text);
        let third = operation::snapshot(&f.repo).unwrap().unwrap();
        assert_eq!(third.step, Some((3, 4)));
        assert_eq!(third.commit.as_ref().map(|c| c.oid.as_str()), Some(north.as_str()));
        let outcome = operation::backend::skip(&f.repo, &third).unwrap();
        assert_eq!(outcome.title, "Cherry-picked onto main");
        assert_eq!(landed(&f, &head), ["Add the ferry", "Head east", "Map the lake"]);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Trail west and east\n");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        assert!(!f.repo.git_dir().join("sequencer").exists());
    }

    #[test]
    fn a_series_refuses_changes_in_its_way_before_touching_anything_and_stashes_them_when_asked() {
        let (f, head, [ferry, camp, lake]) = trail();
        let newest_first = [lake.clone(), camp.clone(), ferry.clone()];
        fs::write(f.dir.join("trail.txt"), "Start here\nFord\nSummit\n").unwrap();
        fs::write(f.dir.join("lake.txt"), "My own lake notes\n").unwrap();
        let before = (status(&f), f.git(&["ls-files", "--stage"]));
        let refused = series(&f, &head, &newest_first).pick(&f.repo).err().expect("refused");
        assert_eq!(
            refused.text,
            "Your local changes to the following files would be overwritten by cherry-pick:\n\tlake.txt\n\ttrail.txt\nCommit or stash them first."
        );
        assert!(crate::autostash::in_the_way(&refused));
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!((status(&f), f.git(&["ls-files", "--stage"])), before);
        assert!(leftovers(&f).is_empty(), "{:?}", leftovers(&f));

        let kept = crate::autostash::Kept::default();
        let offered = series(&f, &head, &newest_first).attempt(&f.repo, false, &kept).err().expect("offered a stash");
        let [(label, view::Action::Autostash(crate::autostash::Action::Retry(crate::autostash::Retry::Picks(again))))] =
            offered.actions.as_slice()
        else {
            panic!("The refusal offered {:?}", offered.actions);
        };
        assert_eq!((label.as_ref(), again.oids()), (crate::autostash::RETRY, vec![ferry.as_str(), camp.as_str(), lake.as_str()]));
        fs::remove_file(f.dir.join("lake.txt")).unwrap();
        let done = again.attempt(&f.repo, true, &kept).ok().expect("picked after stashing");
        assert_eq!(done.title, "Stashed your changes, cherry-picked 3 commits onto main and restored them");
        assert_eq!(landed(&f, &head), ["Add the ferry", "Camp at the summit", "Map the lake"]);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Start here\nFord\nSummit camp\n");
        assert_eq!(status(&f), " M trail.txt\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
    }

    #[test]
    fn changes_git_finds_in_the_way_halfway_leave_the_series_stopped_for_the_panel() {
        let f = Fixture::new();
        commit(&f, "route.txt", "Start\nFord\nSummit\n", "Plan the route");
        f.git(&["switch", "-qc", "side"]);
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let north = commit(&f, "route.txt", "Start north\nFord\nSummit\n", "Start north");
        f.git(&["switch", "-q", "main"]);
        f.git(&["mv", "route.txt", "trail.txt"]);
        f.git(&["commit", "-qm", "Rename the route"]);
        let head = oid(&f, "HEAD");
        fs::write(f.dir.join("trail.txt"), "Start\nFord\nSummit east\n").unwrap();
        for stash in [false, true] {
            let kept = crate::autostash::Kept::default();
            let stopped = series(&f, &head, &[north.clone(), ferry.clone()]).attempt(&f.repo, stash, &kept).err().expect("stopped");
            assert!(stopped.text.starts_with("The cherry-pick stopped at commit 2 of 2. Continue it or abort it"), "{stash}: {}", stopped.text);
            assert!(crate::autostash::in_the_way(&stopped) && stopped.text.contains("trail.txt"), "{}", stopped.text);
            assert!(stopped.actions.is_empty(), "{:?}", stopped.actions);
            assert_eq!(f.git(&["stash", "list"]), b"");
            let shown = operation::snapshot(&f.repo).unwrap().expect("the sequence waits");
            assert_eq!((shown.kind, shown.step, shown.queued), (Operation::CherryPick, Some((2, 2)), true));
            assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Start\nFord\nSummit east\n");
            assert_eq!(landed(&f, &head), ["Add the ferry"]);
            let outcome = operation::backend::abort(&f.repo, &shown).unwrap();
            assert!(outcome.title.contains("aborted"), "{}", outcome.title);
            assert_eq!(oid(&f, "HEAD"), head);
            assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Start\nFord\nSummit east\n");
        }
    }

    #[test]
    fn a_series_refuses_staged_work_a_changed_checkout_a_merge_and_an_operation_in_progress() {
        let (f, head, [ferry, camp, lake]) = trail();
        let newest_first = [lake.clone(), camp.clone(), ferry.clone()];
        fs::write(f.dir.join("bridge.txt"), "Iron bridge\n").unwrap();
        f.git(&["add", "--", "bridge.txt"]);
        let index = f.git(&["ls-files", "--stage"]);
        let refused = series(&f, &head, &newest_first).pick(&f.repo).err().expect("Git refuses staged work");
        assert!(crate::autostash::in_the_way(&refused), "{}", refused.text);
        assert_eq!(oid(&f, "HEAD"), head);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert!(leftovers(&f).is_empty(), "{:?}", leftovers(&f));
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        f.git(&["reset", "-q", "--hard"]);

        let picks = series(&f, &head, &newest_first);
        f.git(&["switch", "-qc", "other"]);
        assert!(picks.pick(&f.repo).err().unwrap().text.contains("tip changed"));
        f.git(&["switch", "-q", "main"]);
        let tip = commit(&f, "later.txt", "Later\n", "Later work");
        assert!(picks.pick(&f.repo).err().unwrap().text.contains("tip changed"));
        assert_eq!(oid(&f, "HEAD"), tip);

        f.git(&["switch", "-qc", "joined", &head]);
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let merge = oid(&f, "HEAD");
        f.git(&["switch", "-q", "main"]);
        let err = series(&f, &tip, &[merge, ferry.clone()]).pick(&f.repo).err().unwrap();
        assert_eq!(err.text, format!("{MERGE_ALONE}."));
        assert_eq!(oid(&f, "HEAD"), tip);

        let clash = commit(&f, "trail.txt", "Start\nFord\nSummit ridge\n", "Mark the ridge");
        crate::operation::tests::stops(&f, &["cherry-pick", &camp]);
        let err = series(&f, &clash, &[lake, ferry]).pick(&f.repo).err().unwrap();
        assert_eq!(err.text, "Finish or abort the cherry-pick first.");
        assert!(!f.dir.join("ferry.txt").exists());
    }

    #[test]
    fn the_prompt_lists_several_commits_oldest_first_and_keeps_committing_on() {
        let (f, head, [ferry, camp, lake]) = trail();
        let mut picks = series(&f, &head, &[lake.clone(), camp.clone(), ferry.clone()]);
        picks.left_out = 1;
        let shown = series_spec(&picks);
        assert_eq!(shown.title, "Cherry-pick 3 commits onto main?");
        assert_eq!(shown.confirm.as_ref(), "Cherry-pick 3 commits");
        assert_eq!(
            shown.lines[..5],
            [
                "Oldest first:".to_owned(),
                format!("{}  Add the ferry", &ferry[..7]),
                format!("{}  Camp at the summit", &camp[..7]),
                format!("{}  Map the lake", &lake[..7]),
                "1 picked commit is already part of main, so it is left out.".to_owned(),
            ]
        );
        assert_eq!(shown.tick.as_ref().map(|(label, on)| (label.to_string(), *on)), Some(("Commit straight away".into(), true)));
        assert!(shown.fixed_tick);
        assert!(!prompt::stops(&shown, true).contains(&prompt::Stop::Tick));

        picks.commits = (0..15).map(|n| Listed { oid: format!("{n:040x}"), subject: format!("Step {n}") }).collect();
        let shown = series_spec(&picks);
        assert_eq!(shown.lines.iter().filter(|line| line.contains("  Step ")).count(), LISTED);
        assert!(shown.lines.contains(&"…and 3 more".to_owned()));

        picks.commits.truncate(1);
        let shown = series_spec(&picks);
        assert_eq!(shown.title, "Cherry-pick this commit onto main?");
        assert!(!shown.fixed_tick && shown.tick.is_some());
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
    fn the_series_menu_finds_the_picked_commits_the_branch_has_in_one_pass() {
        let f = Fixture::new();
        let base = commit(&f, "trail.txt", "Trail\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let ferry = commit(&f, "ferry.txt", "Hourly crossings\n", "Add the ferry");
        let lake = commit(&f, "lake.txt", "Cold water\n", "Map the lake");
        f.git(&["switch", "-q", "main"]);
        let bridge = commit(&f, "bridge.txt", "Stone bridge\n", "Build the bridge");
        f.git(&["merge", "-q", "--no-ff", "--no-edit", &ferry]);
        let head = oid(&f, "HEAD");
        let tower = commit(&f, "tower.txt", "Watch tower\n", "Raise the tower");
        f.git(&["switch", "-q", "--detach", &head]);
        let history = f.repo.history(0, 100).unwrap();
        let everything = [tower.clone(), head.clone(), lake.clone(), bridge.clone(), ferry.clone(), base.clone(), "0".repeat(40)];
        let mut found: Vec<&str> = reached_from(&history, &head, &everything).into_iter().collect();
        found.sort_unstable();
        let mut expected: Vec<&str> = everything
            .iter()
            .map(String::as_str)
            .filter(|oid| reaches(&history, &head, oid) == Some(true))
            .collect();
        expected.sort_unstable();
        assert_eq!(found, expected);
        let mut wanted = vec![head.as_str(), bridge.as_str(), ferry.as_str(), base.as_str()];
        wanted.sort_unstable();
        assert_eq!(found, wanted);
        assert!(reached_from(&history, &bridge, &[head.clone(), ferry.clone(), lake.clone()]).is_empty());
        assert_eq!(reached_from(&history, &"0".repeat(40), &["0".repeat(40), base.clone()]), HashSet::from(["0".repeat(40).as_str()]));
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
