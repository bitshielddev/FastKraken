use crate::repository::Run;
use crate::{
    Tab,
    autostash::Kept,
    guard::Seen,
    merge::Source,
    notice::{self, Failure, Notice, Outcome},
    operation::{self, Kind as Operation},
    prompt::{self, Answer, Spec},
    repository::{Repository, checked, validate_oid},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    process::Stdio,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target, busy, operating};
    fn onto_title(ui: &Ui, name: &str) -> String {
        match on_branch(ui) {
            true => format!("Rebase {} onto {name}…", ui.branch),
            false => format!("Rebase onto {name}…"),
        }
    }
    fn rebase_current(ui: &Ui) -> Availability {
        Availability::unless(
            busy(ui)
                .or_else(|| operating(ui))
                .or_else(|| (!on_branch(ui)).then(|| "Check out a branch first".to_owned())),
        )
    }
    fn known(ui: &Ui, target: &Target) -> bool {
        ui.refs.oid_of(target).is_some()
    }
    fn onto(ui: &Ui, target: &Target) -> Option<view::Action> {
        let source = Source::of(target)?;
        source.shown(ui)?;
        Some(view::Action::Rebase(Action::Onto(source)))
    }
    &[
        Command::new(
            "branch.rebase-onto",
            Group::Switch,
            |ui, target| match target {
                Target::Branch(name) => onto_title(ui, &name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| match target {
                Target::Branch(name) if !checked_out(ui, name) && known(ui, target) => rebase_current(ui),
                _ => Availability::Hidden,
            },
            onto,
        )
        .words(&["rebase", "replay", "update from branch"])
        .icon("branch"),
        Command::new(
            "branch.rebase",
            Group::Switch,
            |ui, target| match target {
                Target::Branch(name) => format!("Rebase {} onto {}…", name.to_string_lossy(), ui.branch),
                _ => String::new(),
            },
            |ui, target, _| match target {
                Target::Branch(name) if on_branch(ui) && !checked_out(ui, name) && known(ui, target) => {
                    Availability::unless(busy(ui).or_else(|| operating(ui)))
                }
                _ => Availability::Hidden,
            },
            |_, target| match target {
                Target::Branch(name) => Some(view::Action::Rebase(Action::Branch(name.clone()))),
                _ => None,
            },
        )
        .words(&["rebase", "replay"])
        .icon("branch"),
        Command::new(
            "branch.rebase-onto-remote",
            Group::Switch,
            |ui, target| match target {
                Target::RemoteBranch(name) => onto_title(ui, &name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| match target {
                Target::RemoteBranch(_) if known(ui, target) => rebase_current(ui),
                _ => Availability::Hidden,
            },
            onto,
        )
        .words(&["rebase", "replay", "update from remote"])
        .icon("branch"),
        Command::new(
            "tag.rebase-onto",
            Group::Switch,
            |ui, target| match target {
                Target::Tag(name) => onto_title(ui, &name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| match target {
                Target::Tag(_) if known(ui, target) => rebase_current(ui),
                _ => Availability::Hidden,
            },
            onto,
        )
        .words(&["rebase", "replay", "release"])
        .icon("branch"),
        Command::new(
            "commit.rebase-onto",
            Group::Switch,
            |ui, _| onto_title(ui, "this commit"),
            |ui, target, _| match target {
                Target::Commit(oid) if crate::pick::contains(ui, oid) != Some(true) => rebase_current(ui),
                _ => Availability::Hidden,
            },
            onto,
        )
        .words(&["rebase", "replay", "move branch"])
        .icon("branch"),
    ]
};

pub(crate) fn on_branch(ui: &Ui) -> bool {
    !ui.detached && ui.head.is_some() && ui.head_ref.as_deref().is_some_and(|r| r.as_bytes().starts_with(b"refs/heads/"))
}

fn checked_out(ui: &Ui, name: &OsStr) -> bool {
    ui.head_ref.as_deref().is_some_and(|full| full.as_bytes().strip_prefix(b"refs/heads/") == Some(name.as_bytes()))
}

fn branch_ref(name: &OsStr) -> OsString {
    let mut full = OsString::from("refs/heads/");
    full.push(name);
    full
}

#[derive(Clone, Debug)]
pub enum Action {
    Onto(Source),
    Branch(OsString),
    Pair { branch: OsString, onto: Source },
}

#[derive(Clone, Debug)]
pub(crate) struct Ask {
    branch: String,
    full: OsString,
    switch: bool,
    seen: Seen,
    tip: String,
    onto: Source,
    target: String,
    label: String,
    commits: usize,
    merges: usize,
    upstream: Option<String>,
    keep_merges: bool,
}

struct Counted {
    commits: usize,
    merges: usize,
    upstream: Option<String>,
}

fn needing(count: usize) -> String {
    match count {
        1 => "1 file needs".to_owned(),
        n => format!("{n} files need"),
    }
}

fn rewritten(count: usize) -> String {
    match count {
        1 => "1 commit was rewritten".to_owned(),
        n => format!("{n} commits were rewritten"),
    }
}

fn count(repo: &Repository, filter: &str, tip: &str, target: &str) -> Result<usize, String> {
    validate_oid(tip)?;
    validate_oid(target)?;
    let counted = checked(repo.read_command().args(["rev-list", "--count", filter, tip, "--not", target, "--"]))?;
    String::from_utf8_lossy(&counted).trim().parse().map_err(|_| "Git did not report a commit count.".to_owned())
}

pub(crate) fn upstream_of(repo: &Repository, full: &OsStr) -> Result<Option<(String, String)>, String> {
    let listed = checked(
        repo.read_command()
            .args(["for-each-ref", "--format=%(refname)%00%(upstream)", "--"])
            .arg(full),
    )?;
    let upstream = listed.split(|&b| b == b'\n').find_map(|line| {
        let (refname, upstream) = line.split_at(line.iter().position(|&b| b == 0)?);
        (refname == full.as_bytes()).then(|| upstream[1..].to_vec())
    });
    let Some(upstream) = upstream.filter(|upstream| upstream.starts_with(b"refs/remotes/")) else {
        return Ok(None);
    };
    let mut peeled = OsString::from(OsStr::from_bytes(&upstream));
    peeled.push("^{commit}");
    let output = repo
        .read_command()
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(&peeled)
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    validate_oid(&oid)?;
    Ok(Some((crate::guard::display(OsStr::from_bytes(&upstream)), oid)))
}

fn upstream_has(repo: &Repository, full: &OsStr, tip: &str, target: &str) -> Result<Option<String>, String> {
    let Some((shown, upstream)) = upstream_of(repo, full)? else {
        return Ok(None);
    };
    let output = repo
        .read_command()
        .args(["merge-base", "--all", tip, &upstream])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    let bases: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect();
    if bases.is_empty() {
        return Ok(None);
    }
    let mut command = repo.read_command();
    command.args(["rev-list", "--count"]);
    for base in &bases {
        validate_oid(base)?;
        command.arg(base);
    }
    let counted = checked(command.args(["--not", target, "--"]))?;
    let shared: usize = String::from_utf8_lossy(&counted).trim().parse().map_err(|_| "Git did not report a commit count.".to_owned())?;
    Ok((shared > 0).then_some(shown))
}

fn counted(repo: &Repository, full: &OsStr, tip: &str, target: &str) -> Result<Option<Counted>, String> {
    if repo.is_ancestor(target, tip)? {
        return Ok(None);
    }
    Ok(Some(Counted {
        commits: count(repo, "--no-merges", tip, target)?,
        merges: count(repo, "--merges", tip, target)?,
        upstream: upstream_has(repo, full, tip, target)?,
    }))
}

impl Ask {
    pub(crate) fn rebase(&self, repo: &Repository, stash: bool, kept: &Kept) -> Result<Outcome, Failure> {
        validate_oid(&self.tip)?;
        validate_oid(&self.target)?;
        let attached = self.seen.head_ref.as_deref().is_some_and(|name| name.as_bytes().starts_with(b"refs/heads/"));
        if !attached || self.seen.head.is_none() {
            return Err(Failure::from("Check out a branch first.".to_owned()));
        }
        repo.expect_no_operation()?;
        repo.expect_checkout(&self.seen)?;
        let Some(short) = self.full.as_bytes().strip_prefix(b"refs/heads/").filter(|short| !short.is_empty()) else {
            return Err(Failure::from("Select a local branch to rebase.".to_owned()));
        };
        if self.switch {
            repo.expect_ref(&self.full, &self.tip)?;
        } else if self.seen.head_ref.as_deref() != Some(self.full.as_os_str())
            || self.seen.head.as_deref() != Some(self.tip.as_str())
        {
            return Err(Failure::from("The checked-out branch or its tip changed. Reopen the action.".to_owned()));
        }
        match &self.onto {
            Source::Commit(oid) => {
                let mut commit = OsString::from(oid);
                commit.push("^{commit}");
                if *oid != self.target || repo.resolve(&commit).ok().as_deref() != Some(self.target.as_str()) {
                    return Err(Failure::from("This commit is not in the repository. Refresh and select it again.".to_owned()));
                }
            }
            Source::Tag(_) => repo.expect_names_commit(&self.onto.full(), &self.target)?,
            Source::Branch(_) | Source::RemoteBranch(_) => repo.expect_ref(&self.onto.full(), &self.target)?,
        }
        let switch = self.switch.then(|| OsStr::from_bytes(short));
        let (branch, label) = (&self.branch, &self.label);
        let finished = format!("Rebased {branch} onto {label}");
        let (head, stashed) = crate::autostash::by_git(
            repo,
            stash,
            kept,
            || crate::autostash::Retry::Rebase(Box::new(self.clone())),
            |autostash| replay(repo, &self.target, self.keep_merges, switch, autostash),
            |_| finished.clone(),
        )
        .map_err(|mut failure| {
            if failure.text.starts_with(&finished) {
                failure.actions = operation::backend::offer_force_push(repo, Outcome::default()).actions;
            }
            failure
        })?;
        let put_back = |detail: String| match stashed {
            true => format!("{detail} Your local changes were stashed first and put back."),
            false => detail,
        };
        if head == self.tip {
            let detail = match self.switch {
                true => format!("FastKraken switched to {branch}; there was nothing to rebase."),
                false => "There was nothing to rebase.".to_owned(),
            };
            return Ok(Outcome::titled(format!("{branch} is already based on {label}"), put_back(detail)));
        }
        let detail = match count(repo, "--no-merges", &head, &self.target)? + count(repo, "--merges", &head, &self.target)? {
            0 if self.commits + self.merges > 0 => {
                format!("{label} already has {branch}'s changes, so {branch} moved to {label} at {}.", notice::short(&head))
            }
            0 => format!("{branch} had no commits of its own, so it moved to {label} at {}.", notice::short(&head)),
            n => format!("{}. {branch} is at {}.", rewritten(n), notice::short(&head)),
        };
        let outcome = Outcome::titled(format!("Rebased {branch} onto {label}"), put_back(detail));
        Ok(operation::backend::offer_force_push(repo, outcome))
    }
}

pub(crate) fn replay(
    repo: &Repository,
    target: &str,
    keep_merges: bool,
    switch: Option<&OsStr>,
    autostash: bool,
) -> Result<String, Failure> {
    validate_oid(target)?;
    let mut command = repo.command();
    command
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null());
    command.args([
        "-c",
        "rebase.autoSquash=false",
        "-c",
        "rebase.updateRefs=false",
        "rebase",
        "--merge",
        if autostash { "--autostash" } else { "--no-autostash" },
        "--no-autosquash",
        "--no-update-refs",
        "--no-fork-point",
        "--empty=drop",
    ]);
    command.arg(if keep_merges { "--rebase-merges" } else { "--no-rebase-merges" });
    command.arg("--end-of-options").arg(target);
    if let Some(short) = switch {
        command.arg(short);
    }
    let output = crate::repository::run(&mut command).map_err(|e| Failure::from(format!("Run bundled Git: {e}")))?;
    ended(repo, &output, autostash)
}

pub(crate) fn ended(repo: &Repository, output: &std::process::Output, autostash: bool) -> Result<String, Failure> {
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let gits_words = || {
        let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let text = if text.is_empty() { printed.trim().to_owned() } else { text };
        let text = if text.is_empty() { format!("Git exited with {}", output.status) } else { text };
        Failure::from(text).output(printed.clone())
    };
    match operation::snapshot(repo)? {
        Some(now) if now.kind == Operation::Rebase && !now.conflicts.is_empty() => {
            let at = now.step.map(|(at, of)| format!(" at commit {at} of {of}")).unwrap_or_default();
            Err(Failure::from(format!(
                "The rebase stopped on a conflict{at}. {} resolving before it can go on.",
                needing(now.conflicts.len())
            ))
            .output(printed))
        }
        Some(now) if autostash && now.kind == Operation::Applied && output.status.success() => head_now(repo),
        Some(_) => Err(gits_words()),
        None if !output.status.success() => Err(gits_words()),
        None => head_now(repo),
    }
}

pub(crate) fn head_now(repo: &Repository) -> Result<String, Failure> {
    let head = checked(repo.read_command().args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    validate_oid(&head)?;
    Ok(head)
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    spec(ask)
}

fn spec(ask: &Ask) -> Spec {
    let (branch, onto) = (&ask.branch, &ask.label);
    let mut spec = Spec::new(format!("Rebase {branch} onto {onto}?"), format!("Rebase {branch}"));
    spec = spec.line(match ask.commits {
        0 => format!("{branch} has no commits of its own that {onto} lacks, so it moves to {onto}. No commits are rewritten."),
        1 => format!("1 commit will be rewritten, replayed one by one on top of {onto}."),
        n => format!("{n} commits will be rewritten, replayed one by one on top of {onto}."),
    });
    if ask.switch {
        spec = spec.line(format!("{branch} is not checked out. FastKraken switches to it first."));
    }
    if let Some(upstream) = &ask.upstream {
        spec = spec.line(format!("{upstream} already has these commits. You will need to force push afterwards."));
    }
    if ask.merges > 0 {
        let merges = match ask.merges {
            1 => "1 merge commit".to_owned(),
            n => format!("{n} merge commits"),
        };
        spec = spec.line(format!(
            "The range also holds {merges}. Without Keep merge commits they are left out and {branch} becomes one straight line."
        ));
        spec.tick = Some(("Keep merge commits".into(), false));
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, mut ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    ask.keep_merges = ask.merges > 0 && answer.tick;
    let stash = tab.ui.autostash.always;
    run(tab, ask, stash, cx);
}

pub(crate) fn run(tab: &mut Tab, ask: Ask, stash: bool, cx: &mut Context<Tab>) {
    tab.ui.operation.started = Some((Operation::Rebase, ask.target.clone(), ask.label.clone()));
    tab.ui.refs.list_open = false;
    let kept = Kept::default();
    let stopped = kept.clone();
    let title = format!("Rebase {} onto {}", ask.branch, ask.label);
    tab.run_operation_or(
        cx,
        "Rebasing…",
        "Rebased",
        true,
        move |repo| crate::journal::titled(ask.rebase(&repo, stash, &kept), title),
        |_, _| {},
        move |tab, _| stopped.remember(&mut tab.ui),
    );
}

fn check_then_ask(tab: &mut Tab, mut ask: Ask, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (full, tip, target) = (ask.full.clone(), ask.tip.clone(), ask.target.clone());
    Tab::work(
        cx,
        move || counted(&repo, &full, &tip, &target),
        move |tab, counted, cx| {
            let ui = &tab.ui;
            if tab.epoch != epoch || Seen::of(ui) != ask.seen || ui.busy() || ui.dialog_open() || ui.operation.active() {
                return;
            }
            match counted {
                Ok(None) => {
                    let (branch, label) = (&ask.branch, &ask.label);
                    tab.ui.notices.show(Notice::new(
                        Tone::Info,
                        format!("{branch} is already based on {label}"),
                        format!("{label} is part of {branch}'s history, so there is nothing to rebase."),
                    ));
                    cx.notify();
                }
                Ok(Some(counted)) => {
                    ask.commits = counted.commits;
                    ask.merges = counted.merges;
                    ask.upstream = counted.upstream;
                    prompt::ask(&mut tab.ui, prompt::Kind::Rebase(ask), cx);
                }
                Err(error) => tab.error(error, cx),
            }
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() || tab.ui.operation.active() {
        return;
    }
    let ui = &tab.ui;
    let (Some(head), Some(head_ref)) = (ui.head.clone(), ui.head_ref.clone()) else {
        tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
        cx.notify();
        return;
    };
    if !on_branch(ui) {
        tab.ui.notices.show(Notice::new(Tone::Info, "Check out a branch first", ""));
        cx.notify();
        return;
    }
    let seen = Seen::of(ui);
    let current = OsStr::from_bytes(head_ref.as_bytes().strip_prefix(b"refs/heads/").unwrap_or_default()).to_owned();
    let action = match action {
        Action::Pair { branch, onto } if branch == current => Action::Onto(onto),
        action => action,
    };
    let ask = match action {
        Action::Onto(onto) => {
            if matches!(&onto, Source::Branch(name) if *name == current) {
                return;
            }
            let Some(target) = onto.shown(ui) else { return };
            Ask {
                branch: ui.branch.clone(),
                full: head_ref,
                switch: false,
                seen,
                tip: head,
                label: onto.label(),
                onto,
                target,
                commits: 0,
                merges: 0,
                upstream: None,
                keep_merges: false,
            }
        }
        Action::Branch(name) => {
            if name == current {
                return;
            }
            let Some(tip) = ui.refs.oid_of(&crate::commands::Target::Branch(name.clone())).map(str::to_owned) else {
                return;
            };
            Ask {
                branch: name.to_string_lossy().into_owned(),
                full: branch_ref(&name),
                switch: true,
                seen,
                tip,
                onto: Source::Branch(current),
                target: head,
                label: ui.branch.clone(),
                commits: 0,
                merges: 0,
                upstream: None,
                keep_merges: false,
            }
        }
        Action::Pair { branch, onto } => {
            if matches!(&onto, Source::Branch(name) if *name == branch) {
                return;
            }
            let Some(tip) = ui.refs.oid_of(&crate::commands::Target::Branch(branch.clone())).map(str::to_owned) else {
                return;
            };
            let Some(target) = onto.shown(ui) else { return };
            Ask {
                branch: branch.to_string_lossy().into_owned(),
                full: branch_ref(&branch),
                switch: true,
                seen,
                tip,
                label: onto.label(),
                onto,
                target,
                commits: 0,
                merges: 0,
                upstream: None,
                keep_merges: false,
            }
        }
    };
    check_then_ask(tab, ask, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{
        backend,
        tests::{commit, diverged, oid, stops},
    };
    use crate::repository::tests::Fixture;
    use std::fs;

    fn subjects(f: &Fixture, range: &str) -> Vec<String> {
        String::from_utf8(f.git(&["log", "--format=%s", range])).unwrap().lines().map(str::to_owned).collect()
    }

    fn status(f: &Fixture) -> String {
        String::from_utf8(f.git(&["status", "--porcelain=v1", "--untracked-files=all"])).unwrap()
    }

    fn apart() -> Fixture {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "feature"]);
        commit(&f, "map.txt", "creek\n", "Map the creek");
        commit(&f, "camp.txt", "ridge\n", "Pitch camp");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "bridge.txt", "stone\n", "Bridge the river");
        f
    }

    fn ask(f: &Fixture, branch: &str, onto: Source) -> Ask {
        let full = branch_ref(OsStr::new(branch));
        let tip = oid(f, branch);
        let target = match &onto {
            Source::Commit(oid) => oid.clone(),
            source => oid(f, &format!("{}^{{commit}}", source.full().to_string_lossy())),
        };
        let counted = counted(&f.repo, &full, &tip, &target).unwrap();
        Ask {
            branch: branch.into(),
            seen: Seen { head_ref: Some(full.clone()), head: Some(tip.clone()) },
            full,
            switch: false,
            tip,
            label: onto.label(),
            onto,
            target,
            commits: counted.as_ref().map_or(0, |c| c.commits),
            merges: counted.as_ref().map_or(0, |c| c.merges),
            upstream: counted.and_then(|c| c.upstream),
            keep_merges: false,
        }
    }

    fn ask_switching(f: &Fixture, branch: &str, current: &str) -> Ask {
        let mut ask = ask(f, branch, Source::Branch(current.into()));
        ask.switch = true;
        let full = branch_ref(OsStr::new(current));
        ask.seen = Seen { head_ref: Some(full), head: Some(oid(f, current)) };
        ask
    }

    fn origin(f: &Fixture, branch: &str, at: &str) {
        for (key, value) in [
            ("remote.origin.url", "/nonexistent/origin.git".to_owned()),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*".to_owned()),
            (&format!("branch.{branch}.remote") as &str, "origin".to_owned()),
            (&format!("branch.{branch}.merge"), format!("refs/heads/{branch}")),
        ] {
            f.git(&["config", key, &value]);
        }
        f.git(&["update-ref", &format!("refs/remotes/origin/{branch}"), at]);
    }

    #[test]
    fn a_clean_rebase_replays_the_commits_onto_the_target() {
        let f = apart();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        fs::write(f.dir.join("notes.txt"), "untracked\n").unwrap();
        let before = subjects(&f, "main..feature");
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!((rebase.commits, rebase.merges, rebase.upstream.as_deref()), (2, 0, None));
        let shown = spec(&rebase);
        assert_eq!(shown.title, "Rebase feature onto main?");
        assert_eq!(shown.confirm.as_ref(), "Rebase feature");
        assert_eq!(shown.lines, ["2 commits will be rewritten, replayed one by one on top of main."]);
        assert!(shown.tick.is_none() && !shown.danger);
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        let head = oid(&f, "HEAD");
        assert_eq!(outcome.title, "Rebased feature onto main");
        assert_eq!(outcome.detail, format!("2 commits were rewritten. feature is at {}.", &head[..7]));
        assert!(outcome.actions.is_empty(), "No upstream, so nothing to force push");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(oid(&f, "feature^^"), main);
        assert_eq!(subjects(&f, "main..feature"), before);
        assert_eq!(fs::read_to_string(f.dir.join("bridge.txt")).unwrap(), "stone\n");
        assert_eq!(fs::read_to_string(f.dir.join("camp.txt")).unwrap(), "ridge\n");
        assert_eq!(status(&f), "?? notes.txt\n");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
        assert_eq!(oid(&f, "main"), main);
    }

    #[test]
    fn a_rebase_run_as_a_job_reports_its_step_and_cannot_be_cancelled() {
        let f = apart();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        let link = crate::jobs::Link::new(false);
        assert!(!link.cancel());
        let head = link.enter(|| replay(&f.repo, &main, false, None, false)).ok().expect("the rebase applies");
        assert_eq!(head, oid(&f, "HEAD"));
        assert_eq!(oid(&f, "feature^^"), main);
        let progress = link.snapshot().0.expect("Git reported its steps");
        assert_eq!(progress.text(), "Rebasing 100% · commit 2 of 2");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_rebase_onto_a_remote_branch_a_tag_or_a_commit_uses_its_commit() {
        for onto in ["remote", "tag", "commit"] {
            let f = apart();
            let main = oid(&f, "main");
            let source = match onto {
                "remote" => {
                    f.git(&["update-ref", "refs/remotes/origin/main", &main]);
                    Source::RemoteBranch("origin/main".into())
                }
                "tag" => {
                    f.git(&["tag", "-a", "-m", "Release", "v1", &main]);
                    Source::Tag("v1".into())
                }
                _ => Source::Commit(main.clone()),
            };
            f.git(&["switch", "-q", "feature"]);
            let rebase = ask(&f, "feature", source);
            assert_eq!(rebase.target, main);
            let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
            let label = match onto {
                "remote" => "origin/main".to_owned(),
                "tag" => "v1".to_owned(),
                _ => main[..7].to_owned(),
            };
            assert_eq!(outcome.title, format!("Rebased feature onto {label}"));
            assert_eq!(oid(&f, "feature^^"), main, "{onto}");
        }
    }

    #[test]
    fn a_branch_that_is_not_checked_out_is_switched_to_and_rebased() {
        let f = apart();
        let main = oid(&f, "main");
        let rebase = ask_switching(&f, "feature", "main");
        let shown = spec(&rebase);
        assert_eq!(shown.title, "Rebase feature onto main?");
        assert_eq!(shown.lines[1], "feature is not checked out. FastKraken switches to it first.");
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        assert_eq!(outcome.title, "Rebased feature onto main");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(oid(&f, "feature^^"), main);
        assert_eq!(status(&f), "");
    }

    #[test]
    fn a_branch_named_like_an_option_is_switched_to_as_a_name() {
        let f = apart();
        let (main, feature) = (oid(&f, "main"), oid(&f, "feature"));
        f.git(&["update-ref", "refs/heads/--exec=false", &feature]);
        let mut rebase = ask_switching(&f, "feature", "main");
        rebase.branch = "--exec=false".into();
        rebase.full = branch_ref(OsStr::new("--exec=false"));
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        assert_eq!(outcome.title, "Rebased --exec=false onto main");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/--exec=false\n");
        assert_eq!(oid(&f, "refs/heads/--exec=false^^"), main);
        assert_eq!((oid(&f, "main"), oid(&f, "feature")), (main, feature));
    }

    #[test]
    fn a_branch_whose_changes_the_target_already_has_says_so() {
        let f = apart();
        f.git(&["switch", "-q", "main"]);
        f.git(&["cherry-pick", "feature~1", "feature"]);
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!(rebase.commits, 2);
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        let main = oid(&f, "main");
        assert_eq!(oid(&f, "feature"), main);
        assert_eq!(outcome.detail, format!("main already has feature's changes, so feature moved to main at {}.", &main[..7]));
    }

    #[test]
    fn a_branch_already_based_on_the_target_is_said_to_be() {
        let f = apart();
        f.git(&["switch", "-q", "feature"]);
        let base = oid(&f, "feature~2");
        let (full, tip) = (branch_ref(OsStr::new("feature")), oid(&f, "feature"));
        assert!(counted(&f.repo, &full, &tip, &base).unwrap().is_none());
        assert!(counted(&f.repo, &full, &tip, &tip).unwrap().is_none());
        let mut rebase = ask(&f, "feature", Source::Commit(base.clone()));
        rebase.label = "Base".into();
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("nothing to do");
        assert_eq!(outcome.title, "feature is already based on Base");
        assert_eq!(outcome.detail, "There was nothing to rebase.");
        assert_eq!(oid(&f, "HEAD"), tip);
    }

    #[test]
    fn a_branch_behind_its_target_moves_up_to_it() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "feature"]);
        f.git(&["switch", "-q", "main"]);
        let main = commit(&f, "bridge.txt", "stone\n", "Bridge the river");
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!(rebase.commits, 0);
        assert_eq!(spec(&rebase).lines[0], "feature has no commits of its own that main lacks, so it moves to main. No commits are rewritten.");
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        assert_eq!(outcome.detail, format!("feature had no commits of its own, so it moved to main at {}.", &main[..7]));
        assert_eq!(oid(&f, "feature"), main);
    }

    #[test]
    fn the_prompt_warns_when_the_upstream_has_the_commits_and_the_end_offers_force_push() {
        let f = apart();
        f.git(&["switch", "-q", "feature"]);
        origin(&f, "feature", &oid(&f, "feature^"));
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!(rebase.upstream.as_deref(), Some("origin/feature"));
        assert_eq!(spec(&rebase).lines[1], "origin/feature already has these commits. You will need to force push afterwards.");
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("the rebase applies");
        let offered: Vec<_> = outcome.actions.iter().map(|(label, action)| (label.to_string(), format!("{action:?}"))).collect();
        assert_eq!(offered.len(), 1, "{offered:?}");
        assert_eq!(offered[0].0, "Force push…");
        assert!(offered[0].1.contains("ForcePush") && offered[0].1.contains("\"feature\""), "{offered:?}");

        let f = apart();
        f.git(&["switch", "-q", "feature"]);
        origin(&f, "feature", &oid(&f, "main"));
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!(rebase.upstream, None);
        assert!(rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("applies").actions.is_empty());
    }

    #[test]
    fn merge_commits_are_kept_only_when_ticked() {
        let build = || {
            let f = apart();
            f.git(&["switch", "-q", "feature"]);
            f.git(&["switch", "-qc", "side", "feature~2"]);
            commit(&f, "side.txt", "side\n", "Walk the side path");
            f.git(&["switch", "-q", "feature"]);
            f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
            f
        };
        let f = build();
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert_eq!((rebase.commits, rebase.merges), (3, 1));
        let shown = spec(&rebase);
        assert_eq!(shown.tick.as_ref().map(|(label, on)| (label.to_string(), *on)), Some(("Keep merge commits".into(), false)));
        assert_eq!(
            shown.lines.last().unwrap(),
            "The range also holds 1 merge commit. Without Keep merge commits they are left out and feature becomes one straight line."
        );
        rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("applies");
        assert_eq!(f.git(&["rev-list", "--count", "--merges", "main..feature"]), b"0\n");
        assert_eq!(f.git(&["rev-list", "--count", "main..feature"]), b"3\n");

        let f = build();
        let mut rebase = ask(&f, "feature", Source::Branch("main".into()));
        rebase.keep_merges = true;
        let outcome = rebase.rebase(&f.repo, false, &Kept::default()).ok().expect("applies");
        assert_eq!(outcome.detail.split('.').next(), Some("4 commits were rewritten"));
        assert_eq!(f.git(&["rev-list", "--count", "--merges", "main..feature"]), b"1\n");
        assert!(f.dir.join("side.txt").exists());
    }

    #[test]
    fn a_conflicted_rebase_stops_for_the_panel_and_is_continued_there() {
        let f = diverged();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Commit(main.clone()));
        assert_eq!(rebase.commits, 3);
        let err = rebase.rebase(&f.repo, false, &Kept::default()).err().expect("the rebase conflicts");
        assert_eq!(err.text, "The rebase stopped on a conflict at commit 2 of 3. 1 file needs resolving before it can go on.");
        assert!(err.output.as_deref().is_some_and(|output| output.contains("CONFLICT")));
        let mut state = operation::State::default();
        state.started = Some((Operation::Rebase, main.clone(), "the bridge".into()));
        state.mark(Some(Operation::Rebase));
        state.update(operation::snapshot(&f.repo).unwrap());
        let shown = state.snapshot().unwrap().clone();
        assert_eq!(shown.summary(), "Rebasing feature onto the bridge · commit 2 of 3 · 1 file needs resolving");
        fs::write(f.dir.join("trail.txt"), "both sides\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        state.update(operation::snapshot(&f.repo).unwrap());
        let ready = state.snapshot().unwrap().clone();
        let outcome = backend::finish(&f.repo, &ready, ready.message.clone()).unwrap();
        assert_eq!(outcome.title, "Rebased feature onto the bridge");
        assert_eq!(subjects(&f, "main..feature"), ["Mark the ridge", "Climb the ridge", "Map the creek"]);
        assert_eq!(f.git(&["merge-base", "main", "feature"]), format!("{main}\n").as_bytes());
        assert_eq!(f.git(&["show", "feature:trail.txt"]), b"both sides\n");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_conflicted_rebase_skips_the_commit_or_aborts_back_to_where_it_was() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        assert!(rebase.rebase(&f.repo, false, &Kept::default()).is_err());
        let stopped = operation::snapshot(&f.repo).unwrap().expect("stopped");
        backend::skip(&f.repo, &stopped).unwrap();
        assert_eq!(subjects(&f, "main..feature"), ["Mark the ridge", "Map the creek"]);
        assert_eq!(f.git(&["show", "feature:trail.txt"]), b"main side\n");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());

        let f = diverged();
        f.git(&["switch", "-q", "main"]);
        let tip = oid(&f, "feature");
        let rebase = ask_switching(&f, "feature", "main");
        assert!(rebase.rebase(&f.repo, false, &Kept::default()).is_err());
        assert_eq!(fs::read_to_string(f.repo.git_dir().join("rebase-merge/head-name")).unwrap(), "refs/heads/feature\n");
        assert_eq!(fs::read_to_string(f.repo.git_dir().join("rebase-merge/orig-head")).unwrap(), format!("{tip}\n"));
        let stopped = operation::snapshot(&f.repo).unwrap().expect("stopped");
        let outcome = backend::abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.detail, format!("feature is back at {}.", &tip[..7]));
        assert_eq!(oid(&f, "feature"), tip);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(status(&f), "");
        assert!(operation::snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_rebase_refuses_a_tip_or_target_that_moved_and_an_operation_in_progress() {
        let f = apart();
        f.git(&["switch", "-q", "feature"]);
        let tip = oid(&f, "feature");
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        let later = commit(&f, "later.txt", "later\n", "Later work");
        assert!(rebase.rebase(&f.repo, false, &Kept::default()).err().unwrap().text.contains("tip changed"));
        assert_eq!(oid(&f, "HEAD"), later);
        f.git(&["reset", "-q", "--hard", &tip]);
        f.git(&["switch", "-q", "main"]);
        commit(&f, "ferry.txt", "ferry\n", "Add the ferry");
        f.git(&["switch", "-q", "feature"]);
        let err = rebase.rebase(&f.repo, false, &Kept::default()).err().unwrap();
        assert_eq!(err.text, "main changed since you chose it. Reopen the action.");
        assert_eq!(oid(&f, "HEAD"), tip);
        let mut missing = ask(&f, "feature", Source::Branch("main".into()));
        (missing.onto, missing.target) = (Source::Commit("1".repeat(40)), "1".repeat(40));
        assert!(missing.rebase(&f.repo, false, &Kept::default()).err().unwrap().text.contains("not in the repository"));
        f.git(&["switch", "-q", "main"]);
        let switching = ask_switching(&f, "feature", "main");
        f.git(&["update-ref", "refs/heads/feature", &oid(&f, "feature^")]);
        assert_eq!(switching.rebase(&f.repo, false, &Kept::default()).err().unwrap().text, "feature changed since you chose it. Reopen the action.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        stops(&f, &["cherry-pick", "main"]);
        assert_eq!(rebase.rebase(&f.repo, false, &Kept::default()).err().unwrap().text, "Finish or abort the cherry-pick first.");
        assert!(f.repo.git_dir().join("CHERRY_PICK_HEAD").exists());
    }

    #[test]
    fn a_rebase_with_changes_in_its_way_offers_gits_autostash_and_puts_them_back() {
        let f = apart();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        fs::write(f.dir.join("trail.txt"), "base, edited\n").unwrap();
        fs::write(f.dir.join("notes.txt"), "untracked\n").unwrap();
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        let kept = Kept::default();

        let refused = rebase.rebase(&f.repo, false, &kept).err().expect("Git refuses");
        assert_eq!(refused.text, "error: cannot rebase: You have unstaged changes.\nerror: Please commit or stash them.");
        let [(label, view::Action::Autostash(crate::autostash::Action::Retry(crate::autostash::Retry::Rebase(again))))] =
            refused.actions.as_slice()
        else {
            panic!("The refusal offered {:?}", refused.actions);
        };
        assert_eq!(label.as_ref(), crate::autostash::RETRY);
        assert_eq!(oid(&f, "HEAD"), rebase.tip);

        let done = again.rebase(&f.repo, true, &kept).ok().expect("the rebase applies");
        let head = oid(&f, "HEAD");
        assert_eq!(done.title, "Rebased feature onto main");
        assert_eq!(
            done.detail,
            format!("2 commits were rewritten. feature is at {}. Your local changes were stashed first and put back.", &head[..7])
        );
        assert_eq!(oid(&f, "feature^^"), main);
        assert_eq!(status(&f), " M trail.txt\n?? notes.txt\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "base, edited\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
        assert!(kept.take().is_none());
    }

    #[test]
    fn a_rebase_whose_stashed_changes_conflict_leaves_them_for_the_panel() {
        let f = apart();
        f.git(&["switch", "-q", "feature"]);
        let tip = oid(&f, "feature");
        f.git(&["switch", "-q", "main"]);
        commit(&f, "map.txt", "lake\n", "Map the lake");
        f.git(&["switch", "-q", "feature"]);
        let rebase = ask(&f, "feature", Source::Branch("main".into()));
        fs::write(f.dir.join("camp.txt"), "ridge, edited\n").unwrap();
        let kept = Kept::default();
        let stopped = rebase.rebase(&f.repo, true, &kept).err().expect("the rebase conflicts");
        assert!(
            stopped.text.ends_with("Your local changes were stashed first. Git puts them back when the rebase is finished or aborted."),
            "{}",
            stopped.text
        );
        assert!(crate::autostash::pending(&f.repo).is_some());
        let shown = operation::snapshot(&f.repo).unwrap().unwrap();
        let pending = crate::autostash::pending(&f.repo);
        let aborted = backend::abort(&f.repo, &shown).ok().expect("the abort runs");
        let aborted = crate::autostash::settled(&f.repo, pending, &kept, aborted).ok().expect("the edit goes back");
        assert_eq!(aborted.title, "Rebase aborted");
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(fs::read_to_string(f.dir.join("camp.txt")).unwrap(), "ridge, edited\n");
        assert_eq!(f.git(&["stash", "list"]), b"");

        f.git(&["checkout", "-q", "--", "camp.txt"]);
        f.git(&["switch", "-qc", "lakeside", "main~1"]);
        let lakeside = commit(&f, "trail.txt", "lakeside\n", "Walk the lakeside");
        f.git(&["switch", "-q", "feature"]);
        origin(&f, "feature", &tip);
        let rebase = ask(&f, "feature", Source::Branch("lakeside".into()));
        fs::write(f.dir.join("trail.txt"), "base, edited\n").unwrap();
        let left = rebase.rebase(&f.repo, true, &kept).err().expect("the edit conflicts");
        let offered: Vec<_> = left.actions.iter().map(|(label, _)| label.to_string()).collect();
        assert_eq!(offered, ["Force push…"]);
        assert!(
            left.text.starts_with(
                "Rebased feature onto lakeside — Your changes did not go back cleanly. 1 file needs resolving, and they stay in the stash \"autostash\""
            ),
            "{}",
            left.text
        );
        assert_eq!(oid(&f, "HEAD~2"), lakeside);
        let stash = kept.take().expect("the stash is named for the panel");
        assert_eq!((stash.oid, stash.title.as_str(), stash.pop), (oid(&f, "refs/stash"), "autostash", true));
        assert_eq!(operation::snapshot(&f.repo).unwrap().unwrap().kind, Operation::Applied);
    }
}
