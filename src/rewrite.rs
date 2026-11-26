use crate::repository::Run;
use crate::{
    Tab,
    autostash::{Kept, Retry},
    guard::Seen,
    notice::{self, Failure, Notice, Outcome},
    prompt::{self, Answer, Field, Spec},
    repository::{Repository, checked, validate_oid},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Path,
    process::Stdio,
};

pub(crate) const TODO_FILE: &str = "FASTKRAKEN_TODO_FILE";
const TODO_NAME: &str = "git-rebase-todo";
pub(crate) const ACROSS_MERGE: &str = "This range includes a merge. Rewriting across a merge is not supported.";
const NOT_A_RUN: &str = "Pick commits that follow one another to squash them";
const FIRST_COMMIT: &str = "This is the first commit, so there is none before it";
const LISTED: usize = 12;

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, commit};
    use crate::compare::commits_of;
    &[
        Command::new(
            "commit.reword",
            Group::Reverse,
            |_, _| "Edit commit message…".into(),
            |ui, target, _| match commit(target) {
                Some(oid) if on_branch(ui, std::slice::from_ref(&oid.to_owned())) => rewritable(ui),
                _ => Availability::Hidden,
            },
            |_, target| commit(target).map(|oid| view::Action::Rewrite(Action::Reword(oid.to_owned()))),
        )
        .words(&["reword", "edit message", "change message", "fix message", "amend message"])
        .icon("pencil"),
        Command::new(
            "commit.squash-into-parent",
            Group::Reverse,
            |_, _| "Squash into the commit before it…".into(),
            |ui, target, _| match commit(target) {
                Some(oid) if on_branch(ui, std::slice::from_ref(&oid.to_owned())) => match first_commit(ui, oid) {
                    true => Availability::Disabled(FIRST_COMMIT.to_owned()),
                    false => rewritable(ui),
                },
                _ => Availability::Hidden,
            },
            |_, target| commit(target).map(|oid| view::Action::Rewrite(Action::SquashIntoParent(oid.to_owned()))),
        )
        .words(&["squash", "fixup", "combine", "meld", "join commits"])
        .icon("merge"),
        Command::new(
            "commit.drop",
            Group::Reverse,
            |_, _| "Drop commit…".into(),
            |ui, target, _| match commit(target) {
                Some(oid) if on_branch(ui, std::slice::from_ref(&oid.to_owned())) => match first_commit(ui, oid) {
                    true => Availability::Disabled("The first commit cannot be dropped".to_owned()),
                    false => rewritable(ui),
                },
                _ => Availability::Hidden,
            },
            |_, target| commit(target).map(|oid| view::Action::Rewrite(Action::Drop(vec![oid.to_owned()]))),
        )
        .words(&["drop", "remove commit", "delete commit", "rewrite"])
        .icon("trash")
        .danger(),
        Command::new(
            "commits.squash",
            Group::Reverse,
            |_, target| format!("Squash {}…", commits(commits_of(target).map_or(0, <[String]>::len))),
            |ui, target, _| match commits_of(target) {
                Some(oids) if on_branch(ui, oids) => match picked_merges(ui, oids) {
                    true => Availability::Disabled(ACROSS_MERGE.to_owned()),
                    false if !consecutive(ui, oids) => Availability::Disabled(NOT_A_RUN.to_owned()),
                    false => rewritable(ui),
                },
                _ => Availability::Hidden,
            },
            |_, target| commits_of(target).map(|oids| view::Action::Rewrite(Action::Squash(oids.to_vec()))),
        )
        .words(&["squash", "fixup", "combine", "meld", "join commits"])
        .icon("merge"),
        Command::new(
            "commits.drop",
            Group::Reverse,
            |_, target| format!("Drop {}…", commits(commits_of(target).map_or(0, <[String]>::len))),
            |ui, target, _| match commits_of(target) {
                Some(oids) if on_branch(ui, oids) => match picked_merges(ui, oids) {
                    true => Availability::Disabled(ACROSS_MERGE.to_owned()),
                    false => rewritable(ui),
                },
                _ => Availability::Hidden,
            },
            |_, target| commits_of(target).map(|oids| view::Action::Rewrite(Action::Drop(oids.to_vec()))),
        )
        .words(&["drop", "remove commits", "delete commits", "rewrite"])
        .icon("trash")
        .danger(),
    ]
};

fn rewritable(ui: &Ui) -> crate::commands::Availability {
    use crate::commands::{busy, operating};
    crate::commands::Availability::unless(busy(ui).or_else(|| operating(ui)))
}

fn on_branch(ui: &Ui, oids: &[String]) -> bool {
    let Some(head) = ui.head.as_deref().filter(|_| crate::rebase::on_branch(ui)) else {
        return false;
    };
    crate::pick::reached_from(&ui.history, head, oids).len() == oids.len()
}

fn first_commit(ui: &Ui, oid: &str) -> bool {
    ui.history.iter().find(|row| row.oid == oid).is_some_and(|row| row.parents.is_empty())
}

fn picked_merges(ui: &Ui, oids: &[String]) -> bool {
    if oids == crate::compare::picked_commits(ui) {
        return ui.compare.merges();
    }
    oids.iter().any(|oid| ui.history.iter().any(|row| &row.oid == oid && row.parents.len() > 1))
}

fn consecutive(ui: &Ui, oids: &[String]) -> bool {
    let mut expected: Option<&str> = None;
    let mut left = oids.len();
    for row in &ui.history {
        if !oids.contains(&row.oid) {
            continue;
        }
        if expected.is_some_and(|parent| parent != row.oid) {
            return false;
        }
        left -= 1;
        if left == 0 {
            return true;
        }
        expected = row.parents.first().map(String::as_str);
        if expected.is_none() {
            return false;
        }
    }
    false
}

pub(crate) fn offered(ui: &Ui, id: &'static str, oids: &[String], cx: &gpui::App) -> Option<crate::commands::Entry> {
    let command = crate::commands::find(id)?;
    let target = match oids {
        [oid] => crate::commands::Target::Commit(oid.clone()),
        oids => crate::commands::Target::Commits(oids.to_vec()),
    };
    crate::commands::entry(command, ui, &target, cx)
}

fn commits(count: usize) -> String {
    match count {
        1 => "1 commit".to_owned(),
        n => format!("{n} commits"),
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Reword(String),
    SquashIntoParent(String),
    Squash(Vec<String>),
    Drop(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Reword,
    Squash,
    Drop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) oid: String,
    pub(crate) subject: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) kind: Kind,
    branch: String,
    seen: Seen,
    base: Option<String>,
    range: Vec<Listed>,
    chosen: Vec<String>,
    pub(crate) summary: String,
    pub(crate) description: String,
    upstream: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Rewrite {
    plan: Plan,
    message: Option<String>,
}

impl Plan {
    fn tip(&self) -> &str {
        self.seen.head.as_deref().unwrap_or_default()
    }

    fn rewords_tip(&self) -> bool {
        self.kind == Kind::Reword && self.chosen.len() == 1 && self.chosen[0] == self.tip()
    }

    fn later(&self) -> usize {
        let newest = self.chosen.last().and_then(|last| self.range.iter().position(|listed| listed.oid == *last));
        newest.map_or(0, |at| self.range.len() - at - 1)
    }

    fn subject_of(&self, oid: &str) -> &str {
        self.range.iter().find(|listed| listed.oid == oid).map_or("", |listed| listed.subject.as_str())
    }

    fn undo_title(&self) -> String {
        match (self.kind, self.chosen.as_slice()) {
            (Kind::Reword, [oid]) => format!("Reword '{}'", self.subject_of(oid)),
            (Kind::Drop, [oid]) => format!("Drop '{}'", self.subject_of(oid)),
            (Kind::Drop, chosen) => format!("Drop {}", commits(chosen.len())),
            (_, chosen) => format!("Squash {}", commits(chosen.len())),
        }
    }
}

fn split(message: &str) -> (String, String) {
    let message = message.replace("\r\n", "\n");
    let message = message.trim();
    match message.split_once('\n') {
        Some((summary, rest)) => (summary.trim_end().to_owned(), rest.trim().to_owned()),
        None => (message.to_owned(), String::new()),
    }
}

pub(crate) fn joined(summary: &str, description: &str) -> String {
    let summary = summary.replace("\r\n", "\n");
    let description = description.replace("\r\n", "\n");
    match description.trim() {
        "" => format!("{}\n", summary.trim()),
        body => format!("{}\n\n{body}\n", summary.trim()),
    }
}

fn parents_of(repo: &Repository, oid: &str) -> Result<Vec<String>, String> {
    validate_oid(oid)?;
    let listed = checked(repo.read_command().args(["rev-list", "--parents", "--max-count=1", oid, "--"]))?;
    let parents: Vec<String> = String::from_utf8_lossy(&listed).split_whitespace().skip(1).map(str::to_owned).collect();
    for parent in &parents {
        validate_oid(parent)?;
    }
    Ok(parents)
}

fn oldest_of(repo: &Repository, chosen: &[String]) -> Result<String, String> {
    if let [only] = chosen {
        return Ok(only.clone());
    }
    let base = checked(repo.read_command().arg("merge-base").arg("--octopus").args(chosen))?;
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    validate_oid(&base)?;
    Ok(base)
}

struct Row {
    listed: Listed,
    parents: usize,
}

fn listing(repo: &Repository, tip: &str, base: Option<&str>) -> Result<Vec<Row>, String> {
    let mut command = repo.read_command();
    command.args(["log", "--reverse", "--no-show-signature", "--no-notes", "--format=%H%x00%P%x00%s", tip]);
    if let Some(base) = base {
        command.arg("--not").arg(base);
    }
    let output = checked(command.arg("--"))?;
    let mut rows = Vec::new();
    for line in output.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, |&b| b == 0);
        let oid = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        let parents = String::from_utf8_lossy(fields.next().ok_or("Git listed a commit without its parents.")?).split_whitespace().count();
        let subject = String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        validate_oid(&oid)?;
        rows.push(Row { listed: Listed { oid, subject }, parents });
    }
    Ok(rows)
}

pub(crate) fn plan(
    repo: &Repository,
    action: &Action,
    branch: &str,
    seen: &Seen,
) -> Result<Result<Plan, String>, String> {
    let tip = seen.head.clone().ok_or("Check out a branch first.")?;
    validate_oid(&tip)?;
    let (kind, mut chosen) = match action {
        Action::Reword(oid) => (Kind::Reword, vec![oid.clone()]),
        Action::SquashIntoParent(oid) => (Kind::Squash, vec![oid.clone()]),
        Action::Squash(oids) => (Kind::Squash, oids.clone()),
        Action::Drop(oids) => (Kind::Drop, oids.clone()),
    };
    if chosen.is_empty() {
        return Ok(Err("Pick the commits first.".to_owned()));
    }
    for oid in &chosen {
        validate_oid(oid)?;
        if !repo.is_ancestor(oid, &tip)? {
            return Ok(Err(format!(
                "{} is not on {branch}. Only commits on the checked-out branch can be rewritten.",
                notice::short(oid)
            )));
        }
    }
    if let Action::SquashIntoParent(oid) = action {
        match parents_of(repo, oid)?.as_slice() {
            [] => return Ok(Err(format!("{FIRST_COMMIT} to squash into."))),
            [parent] => chosen.insert(0, parent.clone()),
            _ => return Ok(Err(ACROSS_MERGE.to_owned())),
        }
    }
    let oldest = oldest_of(repo, &chosen)?;
    let base = match parents_of(repo, &oldest)?.as_slice() {
        [] => None,
        [parent] => Some(parent.clone()),
        _ => return Ok(Err(ACROSS_MERGE.to_owned())),
    };
    let rows = listing(repo, &tip, base.as_deref())?;
    let linear = rows.iter().enumerate().all(|(at, row)| row.parents == usize::from(at > 0 || base.is_some()));
    if !linear || rows.first().map(|row| row.listed.oid.as_str()) != Some(oldest.as_str()) {
        return Ok(Err(ACROSS_MERGE.to_owned()));
    }
    let range: Vec<Listed> = rows.into_iter().map(|row| row.listed).collect();
    let mut positions = Vec::with_capacity(chosen.len());
    for oid in &chosen {
        match range.iter().position(|listed| listed.oid == *oid) {
            Some(at) => positions.push(at),
            None => return Ok(Err(ACROSS_MERGE.to_owned())),
        }
    }
    positions.sort_unstable();
    positions.dedup();
    let chosen: Vec<String> = positions.iter().map(|&at| range[at].oid.clone()).collect();
    if kind == Kind::Squash && (chosen.len() < 2 || positions.windows(2).any(|pair| pair[1] != pair[0] + 1)) {
        return Ok(Err(format!("{NOT_A_RUN}.")));
    }
    if kind == Kind::Drop && base.is_none() {
        return Ok(Err("The first commit cannot be dropped.".to_owned()));
    }
    let (summary, description) = match kind {
        Kind::Drop => (String::new(), String::new()),
        Kind::Reword => split(&repo.commit_message(&chosen[0])?),
        Kind::Squash => {
            let mut messages = Vec::with_capacity(chosen.len());
            for oid in &chosen {
                messages.push(repo.commit_message(oid)?.trim().to_owned());
            }
            split(&messages.join("\n\n"))
        }
    };
    let head_ref = seen.head_ref.as_deref().ok_or("Check out a branch first.")?;
    let upstream = match crate::rebase::upstream_of(repo, head_ref)? {
        Some((shown, oid)) if repo.is_ancestor(&range[0].oid, &oid)? => Some(shown),
        _ => None,
    };
    Ok(Ok(Plan { kind, branch: branch.to_owned(), seen: seen.clone(), base, range, chosen, summary, description, upstream }))
}

fn quoted(text: &[u8]) -> Vec<u8> {
    let mut quoted = Vec::with_capacity(text.len() + 2);
    quoted.push(b'\'');
    for &byte in text {
        match byte {
            b'\'' => quoted.extend_from_slice(b"'\\''"),
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'\'');
    quoted
}

fn amend_line(message: &str) -> Result<String, String> {
    if message.contains('\0') {
        return Err("A commit message cannot hold a NUL character.".to_owned());
    }
    let mut line = String::from("exec printf '%s\\n'");
    for part in message.trim_end_matches('\n').split('\n') {
        line.push(' ');
        line.push_str(&String::from_utf8_lossy(&quoted(part.as_bytes())));
    }
    line.push_str(" | git commit --amend --no-edit --allow-empty --no-verify --cleanup=whitespace --quiet -F -");
    Ok(line)
}

pub(crate) fn todo(plan: &Plan, message: Option<&str>) -> Result<String, String> {
    let amend = message.map(amend_line).transpose()?;
    let mut lines = Vec::with_capacity(plan.range.len() + 1);
    for listed in &plan.range {
        let chosen = plan.chosen.contains(&listed.oid);
        let first = plan.chosen.first() == Some(&listed.oid);
        match plan.kind {
            Kind::Drop if chosen => continue,
            Kind::Squash if chosen && !first => lines.push(format!("fixup {}", listed.oid)),
            _ => lines.push(format!("pick {}", listed.oid)),
        }
        if let Some(amend) = amend.as_ref().filter(|_| plan.chosen.last() == Some(&listed.oid)) {
            lines.push(amend.clone());
        }
    }
    if lines.is_empty() {
        lines.push("noop".to_owned());
    }
    Ok(lines.join("\n") + "\n")
}

pub(crate) fn edit_todo(prepared: &OsStr, target: &OsStr) -> Option<i32> {
    let target = Path::new(target);
    if target.file_name() != Some(OsStr::new(TODO_NAME)) {
        return None;
    }
    Some(match fs::read(prepared).and_then(|listed| fs::write(target, listed)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("FastKraken could not write the rebase to-do list: {error}");
            1
        }
    })
}

fn sequence_editor(folder: &Path) -> Result<OsString, String> {
    #[cfg(test)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = folder.join("copy-todo");
        let copy = format!("#!/bin/sh\nexec /bin/cp \"${TODO_FILE}\" \"$1\"\n");
        fs::write(&script, copy).map_err(|e| format!("Write the to-do editor: {e}"))?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).map_err(|e| format!("Write the to-do editor: {e}"))?;
        Ok(OsString::from_vec(quoted(script.as_os_str().as_bytes())))
    }
    #[cfg(not(test))]
    {
        let _ = folder;
        let program = std::env::current_exe().map_err(|e| format!("Find FastKraken itself: {e}"))?;
        Ok(OsString::from_vec(quoted(program.as_os_str().as_bytes())))
    }
}

fn scripted(repo: &Repository, base: Option<&str>, todo: &str, autostash: bool) -> Result<String, Failure> {
    if let Some(base) = base {
        validate_oid(base)?;
    }
    let folder = tempfile::Builder::new()
        .prefix("fastkraken-rewrite-")
        .tempdir()
        .map_err(|e| Failure::from(format!("Make a private folder for the rebase: {e}")))?;
    let prepared = folder.path().join("todo");
    fs::write(&prepared, todo).map_err(|e| Failure::from(format!("Write the rebase to-do list: {e}")))?;
    let editor = sequence_editor(folder.path())?;
    let mut command = repo.command();
    command
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", editor)
        .env(TODO_FILE, &prepared)
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null());
    command.args([
        "-c",
        "rebase.autoSquash=false",
        "-c",
        "rebase.updateRefs=false",
        "-c",
        "rebase.missingCommitsCheck=ignore",
        "-c",
        "rebase.abbreviateCommands=false",
        "rebase",
        "--interactive",
        "--keep-empty",
        "--no-autosquash",
        "--no-update-refs",
        "--no-fork-point",
        "--no-rebase-merges",
        "--no-reschedule-failed-exec",
        "--empty=drop",
        if autostash { "--autostash" } else { "--no-autostash" },
    ]);
    match base {
        Some(base) => command.arg("--end-of-options").arg(base),
        None => command.args(["--root", "--end-of-options"]),
    };
    let output = command.run().map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
    let ended = crate::rebase::ended(repo, &output, autostash);
    drop(folder);
    ended
}

fn amend_message(repo: &Repository, message: &str) -> Result<String, Failure> {
    let mut command = repo.command();
    command.env("GIT_EDITOR", "true").args([
        "-c",
        "user.useConfigOnly=true",
        "commit",
        "--amend",
        "--only",
        "--allow-empty",
        "--no-verify",
        "--cleanup=whitespace",
        "--quiet",
        "-F",
        "-",
    ]);
    let output = crate::repository::run_with_input(&mut command, message.as_bytes().to_vec())
        .map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(Failure::from(if said.is_empty() { format!("Git exited with {}", output.status) } else { said }));
    }
    crate::rebase::head_now(repo)
}

impl Rewrite {
    pub(crate) fn apply(&self, repo: &Repository, stash: bool, kept: &Kept) -> Result<Outcome, Failure> {
        let plan = &self.plan;
        let attached = plan.seen.head_ref.as_deref().is_some_and(|name| name.as_bytes().starts_with(b"refs/heads/"));
        if !attached || plan.seen.head.is_none() {
            return Err(Failure::from("Check out a branch first.".to_owned()));
        }
        for oid in plan.chosen.iter().chain(plan.base.iter()) {
            validate_oid(oid)?;
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&plan.seen)?;
        let branch = &plan.branch;
        let message = match (&self.message, plan.kind) {
            (Some(message), Kind::Reword | Kind::Squash) if !message.trim().is_empty() => Some(message.as_str()),
            (_, Kind::Drop) => None,
            _ => return Err(Failure::from("Enter a commit message.".to_owned())),
        };
        let (title, done) = match plan.kind {
            Kind::Reword => ("Commit message updated".to_owned(), "Updated the commit message".to_owned()),
            Kind::Squash => (format!("Squashed {} into one", commits(plan.chosen.len())), format!("Squashed {}", commits(plan.chosen.len()))),
            Kind::Drop => (format!("Dropped {} from {branch}", commits(plan.chosen.len())), format!("Dropped {}", commits(plan.chosen.len()))),
        };
        if plan.rewords_tip() {
            let head = amend_message(repo, message.unwrap_or_default())?;
            let outcome = Outcome::titled(title, format!("{branch} is at {}.", notice::short(&head)));
            return Ok(crate::operation::backend::offer_force_push(repo, outcome));
        }
        let todo = todo(plan, message)?;
        let (head, stashed) = crate::autostash::by_git(
            repo,
            stash,
            kept,
            || Retry::Rewrite(Box::new(self.clone())),
            |autostash| scripted(repo, plan.base.as_deref(), &todo, autostash),
            |_| done.clone(),
        )
        .map_err(|mut failure| {
            if failure.text.starts_with(&done) {
                failure.actions = crate::operation::backend::offer_force_push(repo, Outcome::default()).actions;
            }
            failure
        })?;
        let rewritten = plan.later() + usize::from(plan.kind != Kind::Drop);
        let mut detail = match rewritten {
            0 => format!("{branch} is at {}.", notice::short(&head)),
            1 => format!("1 commit was rewritten. {branch} is at {}.", notice::short(&head)),
            n => format!("{n} commits were rewritten. {branch} is at {}.", notice::short(&head)),
        };
        if stashed {
            detail.push_str(" Your local changes were stashed first and put back.");
        }
        Ok(crate::operation::backend::offer_force_push(repo, Outcome::titled(title, detail)))
    }
}

pub(crate) fn describe(plan: &Plan, _: &Ui) -> Spec {
    spec(plan)
}

fn spec(plan: &Plan) -> Spec {
    let later = plan.later();
    let count = plan.chosen.len();
    let mut spec = match plan.kind {
        Kind::Reword => {
            let spec = Spec::new("Edit commit message", "Update Message");
            match plan.rewords_tip() {
                true => spec.line("Only the message of the last commit changes. Its files and anything staged stay as they are."),
                false => spec.line(format!(
                    "Rewording this commit message will cause {} to be rebased.",
                    commits(later + 1)
                )),
            }
        }
        Kind::Squash => {
            let spec = Spec::new(format!("Squash {}", commits(count)), format!("Squash {}", commits(count)))
                .line(format!("Their changes become one commit on {}, with the message below.", plan.branch));
            match later {
                0 => spec,
                1 => spec.line("1 later commit will be rebased."),
                n => spec.line(format!("{n} later commits will be rebased.")),
            }
        }
        Kind::Drop => {
            let (title, confirm, lead) = match plan.chosen.as_slice() {
                [oid] => (
                    format!("Drop commit {}?", notice::short(oid)),
                    "Drop commit".to_owned(),
                    format!("This commit and its changes leave {}:", plan.branch),
                ),
                chosen => (
                    format!("Drop {}?", commits(chosen.len())),
                    format!("Drop {}", commits(chosen.len())),
                    format!("These commits and their changes leave {}:", plan.branch),
                ),
            };
            let mut spec = Spec::new(title, confirm).danger().line(lead);
            for oid in plan.chosen.iter().rev().take(LISTED) {
                spec = spec.line(format!("{} {}", notice::short(oid), plan.subject_of(oid)));
            }
            if count > LISTED {
                spec = spec.line(format!("and {} more", count - LISTED));
            }
            spec = match later {
                0 => spec,
                1 => spec.line("1 later commit will be rebased."),
                n => spec.line(format!("{n} later commits will be rebased.")),
            };
            spec.line("Undo, in the notice that follows, brings them back.")
        }
    };
    if let Some(upstream) = &plan.upstream {
        let has = match (plan.kind, plan.range.len()) {
            (Kind::Reword, _) if plan.rewords_tip() => "this commit",
            (_, 1) => "this commit",
            _ => "these commits",
        };
        spec = spec.line(format!("{upstream} already has {has}. You will need to force push afterwards."));
    }
    if plan.kind != Kind::Drop {
        spec.field = Some(Field {
            label: "Summary",
            placeholder: "Summary",
            initial: plan.summary.clone(),
            masked: false,
            validate: Box::new(|text, _| text.trim().is_empty().then(|| "Enter a summary.".to_owned())),
        });
        spec.second = Some(Field {
            label: "Description",
            placeholder: "Description (optional)",
            initial: plan.description.clone(),
            masked: false,
            validate: Box::new(|_, _| None),
        });
        spec.second_lines = true;
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, plan: Plan, answer: Answer, cx: &mut Context<Tab>) {
    let message = match plan.kind {
        Kind::Drop => None,
        Kind::Reword | Kind::Squash => {
            if answer.text.trim().is_empty() {
                return;
            }
            Some(joined(&answer.text, &answer.second))
        }
    };
    if plan.kind == Kind::Reword && message.as_deref() == Some(joined(&plan.summary, &plan.description).as_str()) {
        tab.ui.notices.show(Notice::new(Tone::Info, "The message is unchanged", "Nothing was rewritten."));
        cx.notify();
        return;
    }
    let stash = tab.ui.autostash.always;
    run(tab, Rewrite { plan, message }, stash, cx);
}

pub(crate) fn run(tab: &mut Tab, rewrite: Rewrite, stash: bool, cx: &mut Context<Tab>) {
    let progress = match rewrite.plan.kind {
        Kind::Reword => "Rewording…",
        Kind::Squash => "Squashing…",
        Kind::Drop => "Dropping…",
    };
    let kept = Kept::default();
    let stopped = kept.clone();
    let title = rewrite.plan.undo_title();
    tab.run_operation_or(
        cx,
        progress,
        "Rewritten",
        true,
        move |repo| crate::journal::titled(rewrite.apply(&repo, stash, &kept), title),
        |_, _| {},
        move |tab, _| stopped.remember(&mut tab.ui),
    );
}

fn refused(kind: &Action) -> &'static str {
    match kind {
        Action::Reword(_) => "The message cannot be edited here",
        Action::SquashIntoParent(_) | Action::Squash(_) => "These commits cannot be squashed",
        Action::Drop(_) => "This cannot be dropped",
    }
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() || tab.ui.operation.active() {
        return;
    }
    if !crate::rebase::on_branch(&tab.ui) {
        tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
        cx.notify();
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let seen = Seen::of(&tab.ui);
    let branch = tab.ui.branch.clone();
    Tab::work(
        cx,
        {
            let (seen, action) = (seen.clone(), action.clone());
            move || plan(&repo, &action, &branch, &seen)
        },
        move |tab, planned, cx| {
            let ui = &tab.ui;
            if tab.epoch != epoch || Seen::of(ui) != seen || ui.busy() || ui.dialog_open() || ui.operation.active() {
                return;
            }
            match planned {
                Ok(Ok(plan)) => prompt::ask(&mut tab.ui, prompt::Kind::Rewrite(plan), cx),
                Ok(Err(limit)) => {
                    tab.ui.notices.show(Notice::new(Tone::Info, refused(&action), limit));
                    cx.notify();
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::tests::{commit, oid};
    use crate::repository::tests::Fixture;

    fn log(f: &Fixture) -> Vec<String> {
        String::from_utf8(f.git(&["log", "--format=%s", "main"])).unwrap().lines().map(str::to_owned).collect()
    }

    fn tree(f: &Fixture, rev: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", &format!("{rev}^{{tree}}")])).unwrap().trim().to_owned()
    }

    fn status(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "--untracked-files=all"])).unwrap()
    }

    fn trail() -> (Fixture, Vec<String>) {
        let f = Fixture::new();
        let oids = vec![
            commit(&f, "camp.txt", "tents\n", "Pitch the camp"),
            commit(&f, "map.txt", "creek\n", "Map the creek"),
            commit(&f, "ridge.txt", "pass\n", "Climb the ridge"),
            commit(&f, "lake.txt", "cold\n", "Swim in the lake"),
            commit(&f, "summit.txt", "flag\n", "Reach the summit"),
        ];
        (f, oids)
    }

    fn seen(f: &Fixture) -> Seen {
        Seen { head_ref: Some("refs/heads/main".into()), head: Some(oid(f, "HEAD")) }
    }

    fn planned(f: &Fixture, action: Action) -> Plan {
        plan(&f.repo, &action, "main", &seen(f)).unwrap().expect("the plan is allowed")
    }

    fn refusal(f: &Fixture, action: Action) -> String {
        plan(&f.repo, &action, "main", &seen(f)).unwrap().err().expect("the plan is refused")
    }

    fn rewrite(plan: Plan, message: Option<&str>) -> Rewrite {
        Rewrite { plan, message: message.map(str::to_owned) }
    }

    fn applied(f: &Fixture, plan: Plan, message: Option<&str>) -> Outcome {
        rewrite(plan, message).apply(&f.repo, false, &Kept::default()).ok().expect("the rewrite applies")
    }

    #[test]
    fn rewording_an_older_commit_changes_only_its_message_and_keeps_every_tree() {
        let (f, oids) = trail();
        let trees: Vec<String> = (0..5).map(|back| tree(&f, &format!("main~{back}"))).collect();
        let authors = String::from_utf8(f.git(&["log", "--format=%an %ae %ad", "main"])).unwrap();
        f.git(&["commit", "-q", "--amend", "-m", "Reach the summit\n\nPlant the flag.\nTake a photo."]);
        let shown = planned(&f, Action::Reword(oids[2].clone()));
        assert_eq!((shown.summary.as_str(), shown.description.as_str()), ("Climb the ridge", ""));
        assert_eq!(shown.range.len(), 3);
        let spec = spec(&shown);
        assert_eq!(spec.title, "Edit commit message");
        assert_eq!(spec.confirm.as_ref(), "Update Message");
        assert_eq!(spec.lines, ["Rewording this commit message will cause 3 commits to be rebased."]);
        assert!(spec.second_lines && !spec.danger);
        assert_eq!(spec.field.as_ref().map(|field| field.initial.as_str()), Some("Climb the ridge"));
        let message = joined("Climb the 'high' ridge", "Over the pass.\n\n# Not a comment");
        let outcome = applied(&f, shown, Some(&message));
        assert_eq!(outcome.title, "Commit message updated");
        assert_eq!(outcome.detail, format!("3 commits were rewritten. main is at {}.", notice::short(&oid(&f, "main"))));
        assert_eq!(log(&f), ["Reach the summit", "Swim in the lake", "Climb the 'high' ridge", "Map the creek", "Pitch the camp"]);
        assert_eq!(f.repo.commit_message(&oid(&f, "main~2")).unwrap(), "Climb the 'high' ridge\n\nOver the pass.\n\n# Not a comment\n");
        assert_eq!(f.repo.commit_message(&oid(&f, "main")).unwrap(), "Reach the summit\n\nPlant the flag.\nTake a photo.\n");
        assert_eq!((0..5).map(|back| tree(&f, &format!("main~{back}"))).collect::<Vec<_>>(), trees);
        assert_eq!(String::from_utf8(f.git(&["log", "--format=%an %ae %ad", "main"])).unwrap(), authors);
        assert_eq!(oid(&f, "main~3"), oids[1]);
        assert_eq!(status(&f), "");
        assert!(crate::operation::snapshot(&f.repo).unwrap().is_none());
        assert!(!f.dir.join(".git/rebase-merge").exists());
    }

    #[test]
    fn rewording_the_tip_amends_only_its_message_and_leaves_staged_changes_staged() {
        let (f, oids) = trail();
        fs::write(f.dir.join("notes.txt"), "staged\n").unwrap();
        f.git(&["add", "notes.txt"]);
        fs::write(f.dir.join("camp.txt"), "edited\n").unwrap();
        let shown = planned(&f, Action::Reword(oids[4].clone()));
        assert!(shown.rewords_tip());
        let spec = spec(&shown);
        assert_eq!(spec.lines, ["Only the message of the last commit changes. Its files and anything staged stay as they are."]);
        let before = tree(&f, "main");
        let outcome = applied(&f, shown, Some(&joined("Reach the summit at dawn", "")));
        assert_eq!(outcome.detail, format!("main is at {}.", notice::short(&oid(&f, "main"))));
        assert_eq!(log(&f)[0], "Reach the summit at dawn");
        assert_eq!(tree(&f, "main"), before);
        assert_eq!(oid(&f, "main^"), oids[3]);
        assert_eq!(status(&f), " M camp.txt\nA  notes.txt\n");
    }

    #[test]
    fn dropping_commits_removes_their_changes_and_keeps_the_rest() {
        let (f, oids) = trail();
        let shown = planned(&f, Action::Drop(vec![oids[3].clone(), oids[1].clone()]));
        assert_eq!(shown.chosen, [oids[1].clone(), oids[3].clone()]);
        let spec = spec(&shown);
        assert_eq!(spec.title, "Drop 2 commits?");
        assert_eq!(spec.confirm.as_ref(), "Drop 2 commits");
        assert!(spec.danger && spec.field.is_none());
        assert_eq!(
            spec.lines,
            [
                "These commits and their changes leave main:".to_owned(),
                format!("{} Swim in the lake", notice::short(&oids[3])),
                format!("{} Map the creek", notice::short(&oids[1])),
                "1 later commit will be rebased.".to_owned(),
                "Undo, in the notice that follows, brings them back.".to_owned(),
            ]
        );
        assert_eq!(todo(&shown, None).unwrap(), format!("pick {}\npick {}\n", oids[2], oids[4]));
        let outcome = applied(&f, shown, None);
        assert_eq!(outcome.title, "Dropped 2 commits from main");
        assert_eq!(log(&f), ["Reach the summit", "Climb the ridge", "Pitch the camp"]);
        let files = String::from_utf8(f.git(&["ls-tree", "--name-only", "main"])).unwrap();
        assert_eq!(files, "camp.txt\nridge.txt\nsummit.txt\n");
        assert!(!f.dir.join("map.txt").exists() && !f.dir.join("lake.txt").exists());
        assert_eq!(status(&f), "");
    }

    #[test]
    fn dropping_the_tip_moves_the_branch_to_its_parent() {
        let (f, oids) = trail();
        let shown = planned(&f, Action::Drop(vec![oids[4].clone()]));
        assert_eq!(todo(&shown, None).unwrap(), "noop\n");
        let spec = spec(&shown);
        assert_eq!(spec.title, format!("Drop commit {}?", notice::short(&oids[4])));
        applied(&f, shown, None);
        assert_eq!(oid(&f, "main"), oids[3]);
        assert_eq!(status(&f), "");
    }

    #[test]
    fn squashing_a_run_folds_the_changes_into_one_commit_with_the_new_message() {
        let (f, oids) = trail();
        let shown = planned(&f, Action::Squash(vec![oids[3].clone(), oids[2].clone(), oids[1].clone()]));
        assert_eq!(shown.summary, "Map the creek");
        assert_eq!(shown.description, "Climb the ridge\n\nSwim in the lake");
        let spec = spec(&shown);
        assert_eq!(spec.title, "Squash 3 commits");
        assert_eq!(spec.lines, ["Their changes become one commit on main, with the message below.", "1 later commit will be rebased."]);
        let message = joined("Cross the valley", "Creek, ridge and lake.");
        let listed = todo(&shown, Some(&message)).unwrap();
        assert_eq!(
            listed,
            format!(
                "pick {}\nfixup {}\nfixup {}\nexec printf '%s\\n' 'Cross the valley' '' 'Creek, ridge and lake.' | git commit --amend --no-edit --allow-empty --no-verify --cleanup=whitespace --quiet -F -\npick {}\n",
                oids[1], oids[2], oids[3], oids[4]
            )
        );
        let summit = tree(&f, "main");
        let outcome = applied(&f, shown, Some(&message));
        assert_eq!(outcome.title, "Squashed 3 commits into one");
        assert_eq!(log(&f), ["Reach the summit", "Cross the valley", "Pitch the camp"]);
        assert_eq!(f.repo.commit_message(&oid(&f, "main^")).unwrap(), "Cross the valley\n\nCreek, ridge and lake.\n");
        let changed = String::from_utf8(f.git(&["diff", "--name-only", "main~2", "main^"])).unwrap();
        assert_eq!(changed, "lake.txt\nmap.txt\nridge.txt\n");
        assert_eq!(tree(&f, "main"), summit);
    }

    #[test]
    fn squashing_into_the_commit_before_it_takes_the_parent_and_the_root_can_be_squashed() {
        let (f, oids) = trail();
        let shown = planned(&f, Action::SquashIntoParent(oids[1].clone()));
        assert_eq!(shown.chosen, [oids[0].clone(), oids[1].clone()]);
        assert_eq!(shown.base, None);
        assert_eq!(shown.summary, "Pitch the camp");
        assert_eq!(shown.description, "Map the creek");
        applied(&f, shown, Some(&joined("Pitch the camp by the creek", "")));
        assert_eq!(log(&f), ["Reach the summit", "Swim in the lake", "Climb the ridge", "Pitch the camp by the creek"]);
        let files = String::from_utf8(f.git(&["ls-tree", "--name-only", "main~3"])).unwrap();
        assert_eq!(files, "camp.txt\nmap.txt\n");
        assert_eq!(refusal(&f, Action::SquashIntoParent(oid(&f, "main~3"))), format!("{FIRST_COMMIT} to squash into."));
    }

    #[test]
    fn a_range_with_a_merge_a_gap_or_another_branch_is_refused_before_anything_starts() {
        let (f, oids) = trail();
        f.git(&["switch", "-qc", "side", &oids[1]]);
        let side = commit(&f, "side.txt", "path\n", "Take the side path");
        f.git(&["switch", "-q", "main"]);
        f.git(&["merge", "-q", "--no-ff", "-m", "Join the side path", "side"]);
        commit(&f, "hut.txt", "warm\n", "Build the hut");
        let before = String::from_utf8(f.git(&["for-each-ref"])).unwrap();
        assert_eq!(refusal(&f, Action::Reword(oids[2].clone())), ACROSS_MERGE);
        assert_eq!(refusal(&f, Action::Drop(vec![side.clone()])), ACROSS_MERGE);
        assert_eq!(refusal(&f, Action::Squash(vec![oids[3].clone(), side.clone()])), ACROSS_MERGE);
        assert_eq!(refusal(&f, Action::SquashIntoParent(oid(&f, "main"))), ACROSS_MERGE);
        assert_eq!(refusal(&f, Action::Squash(vec![oid(&f, "main"), oid(&f, "main^")])), ACROSS_MERGE);
        planned(&f, Action::Reword(oid(&f, "main")));
        f.git(&["switch", "-q", "side"]);
        let side_seen = Seen { head_ref: Some("refs/heads/side".into()), head: Some(side.clone()) };
        let refused = plan(&f.repo, &Action::Reword(oids[4].clone()), "side", &side_seen).unwrap().unwrap_err();
        assert_eq!(refused, format!("{} is not on side. Only commits on the checked-out branch can be rewritten.", notice::short(&oids[4])));
        let gap = plan(&f.repo, &Action::Squash(vec![side.clone(), oids[0].clone()]), "side", &side_seen).unwrap().unwrap_err();
        assert_eq!(gap, format!("{NOT_A_RUN}."));
        let root = plan(&f.repo, &Action::Drop(vec![oids[0].clone()]), "side", &side_seen).unwrap().unwrap_err();
        assert_eq!(root, "The first commit cannot be dropped.");
        assert_eq!(String::from_utf8(f.git(&["for-each-ref"])).unwrap(), before);
    }

    #[test]
    fn a_rewrite_is_refused_when_the_branch_moved_since_it_was_planned() {
        let (f, oids) = trail();
        let shown = planned(&f, Action::Drop(vec![oids[2].clone()]));
        commit(&f, "outside.txt", "late\n", "Committed elsewhere");
        let tip = oid(&f, "main");
        let refused = rewrite(shown, None).apply(&f.repo, false, &Kept::default()).err().expect("refused");
        assert_eq!(refused.text, "The checked-out branch or its tip changed. Reopen the action.");
        assert_eq!(oid(&f, "main"), tip);
        assert_eq!(log(&f).len(), 6);
        let tip_plan = planned(&f, Action::Reword(tip.clone()));
        f.git(&["commit", "-q", "--amend", "-m", "Changed outside"]);
        let refused = rewrite(tip_plan, Some("New\n")).apply(&f.repo, false, &Kept::default()).err().expect("refused");
        assert_eq!(refused.text, "The checked-out branch or its tip changed. Reopen the action.");
        assert_eq!(log(&f)[0], "Changed outside");
    }

    #[test]
    fn local_changes_in_the_way_are_offered_a_stash_and_retry_that_puts_them_back() {
        let (f, oids) = trail();
        fs::write(f.dir.join("map.txt"), "creek and ford\n").unwrap();
        let shown = planned(&f, Action::Reword(oids[1].clone()));
        let refused = rewrite(shown.clone(), Some("Map the ford\n")).apply(&f.repo, false, &Kept::default()).err().expect("refused");
        assert!(crate::autostash::in_the_way(&refused), "{refused:?}");
        assert!(refused.actions.iter().any(|(label, _)| label.as_ref() == crate::autostash::RETRY), "{:?}", refused.actions);
        assert_eq!(oid(&f, "main"), oids[4]);
        let outcome = rewrite(shown, Some("Map the ford\n")).apply(&f.repo, true, &Kept::default()).ok().expect("applies");
        assert!(outcome.detail.ends_with("Your local changes were stashed first and put back."), "{}", outcome.detail);
        assert_eq!(log(&f)[3], "Map the ford");
        assert_eq!(fs::read_to_string(f.dir.join("map.txt")).unwrap(), "creek and ford\n");
        assert!(f.repo.stashes().unwrap().is_empty());
    }

    #[test]
    fn a_drop_that_conflicts_stops_as_a_rebase_that_abort_puts_back() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "start\n", "Start");
        let middle = commit(&f, "trail.txt", "start\nford\n", "Mark the ford");
        commit(&f, "trail.txt", "start\nford\nsummit\n", "Mark the summit");
        let tip = oid(&f, "main");
        let shown = planned(&f, Action::Drop(vec![middle]));
        let stopped = rewrite(shown, None).apply(&f.repo, false, &Kept::default()).err().expect("stops");
        assert!(stopped.text.starts_with("The rebase stopped on a conflict"), "{}", stopped.text);
        let now = crate::operation::snapshot(&f.repo).unwrap().expect("a rebase is in progress");
        assert_eq!(now.kind, crate::operation::Kind::Rebase);
        f.git(&["rebase", "--abort"]);
        assert_eq!(oid(&f, "main"), tip);
    }

    #[test]
    fn a_pushed_range_says_a_force_push_is_needed_and_the_notice_offers_it() {
        let (f, oids) = trail();
        for (key, value) in [
            ("remote.origin.url", "/nonexistent/origin.git"),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("branch.main.remote", "origin"),
            ("branch.main.merge", "refs/heads/main"),
        ] {
            f.git(&["config", key, value]);
        }
        f.git(&["update-ref", "refs/remotes/origin/main", &oids[2]]);
        let pushed = planned(&f, Action::Reword(oids[1].clone()));
        let spec = spec(&pushed);
        assert_eq!(spec.lines.last().map(String::as_str), Some("origin/main already has these commits. You will need to force push afterwards."));
        let unpushed = planned(&f, Action::Drop(vec![oids[3].clone()]));
        assert!(unpushed.upstream.is_none());
        let outcome = applied(&f, pushed, Some("Map the creek bed\n"));
        assert!(outcome.actions.iter().any(|(label, _)| label.as_ref().starts_with("Force push")), "{:?}", outcome.actions);
    }

    #[test]
    fn the_journal_records_one_branch_move_that_undo_reverses() {
        let (f, oids) = trail();
        let before = oid(&f, "main");
        let shown = planned(&f, Action::Squash(vec![oids[2].clone(), oids[1].clone()]));
        let title = shown.undo_title();
        assert_eq!(title, "Squash 2 commits");
        let entry = crate::journal::tests::entry_of(&f.repo, |repo| {
            crate::journal::titled(rewrite(shown, Some("Creek and ridge\n")).apply(repo, false, &Kept::default()), title)
        });
        assert_eq!(entry.title, "Squash 2 commits");
        assert_eq!(entry.changes.len(), 1, "{:?}", entry.changes);
        assert!(matches!(&entry.changes[0], crate::journal::Change::MoveBranch { before: from, .. } if *from == before));
        crate::journal::replay(&f.repo, &entry, crate::journal::Way::Undo).ok().expect("undo applies");
        assert_eq!(oid(&f, "main"), before);
        assert_eq!(status(&f), "");
    }

    #[test]
    fn the_sequence_editor_copies_the_prepared_list_only_over_a_rebase_to_do_file() {
        let folder = tempfile::tempdir().unwrap();
        let prepared = folder.path().join("prepared");
        fs::write(&prepared, "noop\n").unwrap();
        let target = folder.path().join(TODO_NAME);
        fs::write(&target, "pick 1234567\n").unwrap();
        assert_eq!(edit_todo(prepared.as_os_str(), target.as_os_str()), Some(0));
        assert_eq!(fs::read_to_string(&target).unwrap(), "noop\n");
        let other = folder.path().join("COMMIT_EDITMSG");
        assert_eq!(edit_todo(prepared.as_os_str(), other.as_os_str()), None);
        assert!(!other.exists());
        let missing = folder.path().join("missing");
        assert_eq!(edit_todo(missing.as_os_str(), target.as_os_str()), Some(1));
    }

    #[test]
    fn messages_are_quoted_for_the_shell_line_by_line() {
        assert_eq!(
            amend_line("It's done\n\n$HOME `x` \"y\"\n").unwrap(),
            "exec printf '%s\\n' 'It'\\''s done' '' '$HOME `x` \"y\"' | git commit --amend --no-edit --allow-empty --no-verify --cleanup=whitespace --quiet -F -"
        );
        assert!(amend_line("bad\0").is_err());
        assert_eq!(joined("  Summary  ", "\n\nBody\r\nline\n\n"), "Summary\n\nBody\nline\n");
        assert_eq!(split("Summary\n\n\nBody\n"), ("Summary".to_owned(), "Body".to_owned()));
    }
}
