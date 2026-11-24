use crate::repository::Run;
use crate::{
    Tab,
    commands::Target,
    guard::Seen,
    notice::{self, Notice, Outcome},
    prompt::{self, Answer, Field, Spec},
    refs::{self, Kind},
    repository::{Repository, Revisions, checked, validate_oid},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, busy, operating, on_repo};
    fn renamed(ui: &Ui, target: &Target) -> Option<OsString> {
        match target {
            Target::Branch(name) => Some(name.clone()),
            Target::Repo if !ui.detached && !ui.branch.is_empty() => Some(ui.branch.clone().into()),
            _ => None,
        }
    }
    &[
        Command::new(
            "branch.rename",
            Group::Manage,
            |_, target| match target {
                Target::Branch(name) => format!("Rename {}…", name.to_string_lossy()),
                _ => "Rename branch…".into(),
            },
            |ui, target, _| match target {
                Target::Branch(name) if ui.refs.local(name).is_some() => {
                    Availability::unless(busy(ui).or_else(|| operating(ui)))
                }
                Target::Repo if on_repo(ui, target) => Availability::unless(
                    busy(ui).or_else(|| operating(ui)).or_else(|| match renamed(ui, target) {
                        None => Some("Check out a branch first".to_owned()),
                        Some(name) if ui.refs.local(&name).is_none() => {
                            Some("Make the first commit before renaming the branch".to_owned())
                        }
                        Some(_) => None,
                    }),
                ),
                _ => Availability::Hidden,
            },
            |ui, target| Some(view::Action::Branch(Action::Rename(renamed(ui, target)?))),
        )
        .words(&["change branch name"])
        .icon("branch")
        .bar(Bar::Branch),
        Command::new(
            "branch.delete",
            Group::Manage,
            |_, target| match target {
                Target::Branch(name) => format!("Delete {}…", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                let Target::Branch(name) = target else {
                    return Availability::Hidden;
                };
                if ui.refs.local(name).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| current(ui, name)))
            },
            |_, target| match target {
                Target::Branch(name) => Some(view::Action::Branch(Action::Delete(name.clone()))),
                _ => None,
            },
        )
        .words(&["remove branch"])
        .icon("x")
        .danger(),
        Command::new(
            "branch.checkout-remote",
            Group::Switch,
            |_, target| match target {
                Target::RemoteBranch(name) => format!("Check out {}", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                let Target::RemoteBranch(name) = target else {
                    return Availability::Hidden;
                };
                if ui.refs.oid_of(target).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    match split_remote(&ui.refs.remotes, name) {
                        None => Some("No configured remote has this branch".to_owned()),
                        Some((_, local)) => checked_out(ui, &local),
                    }
                }))
            },
            |_, target| match target {
                Target::RemoteBranch(name) => Some(view::Action::Branch(Action::CheckoutRemote(name.clone()))),
                _ => None,
            },
        )
        .words(&["checkout", "switch", "track remote branch"])
        .icon("branch"),
        Command::new(
            "commit.checkout",
            Group::Switch,
            |_, _| "Check out this commit".into(),
            |ui, target, _| match target {
                Target::Commit(oid) => Availability::unless(
                    busy(ui)
                        .or_else(|| operating(ui))
                        .or_else(|| detached_at(ui, oid).then(|| "This commit is already checked out".to_owned())),
                ),
                _ => Availability::Hidden,
            },
            |_, target| match target {
                Target::Commit(oid) => Some(view::Action::Branch(Action::Detach { oid: oid.clone(), tag: None })),
                _ => None,
            },
        )
        .words(&["checkout", "detach", "detached head"])
        .icon("commit"),
        Command::new(
            "tag.checkout",
            Group::Switch,
            |_, target| match target {
                Target::Tag(name) => format!("Check out the commit at {}", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                let (Target::Tag(name), Some(oid)) = (target, ui.refs.oid_of(target)) else {
                    return Availability::Hidden;
                };
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    detached_at(ui, oid)
                        .then(|| format!("The commit at {} is already checked out", name.to_string_lossy()))
                }))
            },
            |ui, target| match target {
                Target::Tag(name) => Some(view::Action::Branch(Action::Detach {
                    oid: ui.refs.oid_of(target)?.to_owned(),
                    tag: Some(name.clone()),
                })),
                _ => None,
            },
        )
        .words(&["checkout", "detach", "detached head", "release"])
        .icon("tag"),
    ]
};

fn detached_at(ui: &Ui, oid: &str) -> bool {
    ui.detached && ui.head.as_deref() == Some(oid)
}

pub(crate) fn split_remote<'a>(remotes: &'a [String], name: &OsStr) -> Option<(&'a str, OsString)> {
    let bytes = name.as_bytes();
    remotes
        .iter()
        .filter_map(|remote| {
            let rest = bytes.strip_prefix(remote.as_bytes())?.strip_prefix(b"/")?;
            (!rest.is_empty()).then(|| (remote.as_str(), OsStr::from_bytes(rest).to_owned()))
        })
        .max_by_key(|(remote, _)| remote.len())
}

fn checked_out(ui: &Ui, local: &OsStr) -> Option<String> {
    (!ui.detached && ui.branch.as_bytes() == local.as_bytes())
        .then(|| format!("{} is already checked out", local.to_string_lossy()))
}

fn current(ui: &Ui, name: &OsStr) -> Option<String> {
    (!ui.detached && ui.branch.as_bytes() == name.as_bytes())
        .then(|| "Switch to another branch first".to_owned())
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Delete(OsString),
    Rename(OsString),
    Restore { name: OsString, oid: String, tracking: Option<Tracking> },
    CheckoutRemote(OsString),
    Detach { oid: String, tag: Option<OsString> },
}

#[derive(Clone, Debug)]
pub(crate) struct Checkout {
    remote_branch: OsString,
    local: OsString,
    tip: String,
    local_tip: Option<String>,
    tracks: bool,
    seen: Seen,
}

impl Checkout {
    fn remote_ref(&self) -> OsString {
        let mut full = OsString::from("refs/remotes/");
        full.push(&self.remote_branch);
        full
    }

    fn shown(&self) -> (String, String) {
        (self.local.to_string_lossy().into_owned(), self.remote_branch.to_string_lossy().into_owned())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Same,
    Behind(usize),
    Kept(usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tracking {
    remote: OsString,
    merge: OsString,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Delete {
        name: OsString,
        tip: String,
        only: usize,
        subjects: Vec<(String, String)>,
    },
    Rename {
        name: OsString,
        tip: String,
        upstream: Option<String>,
    },
    Diverged {
        checkout: Checkout,
        ahead: usize,
        behind: usize,
        subjects: Vec<(String, String)>,
    },
    Another { checkout: Checkout },
}

const ANOTHER: usize = 0;
const RESET: usize = 1;

const LISTED: usize = 5;

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::Delete { name, only, subjects, .. } => delete_spec(name, *only, subjects),
        Ask::Rename { name, upstream, .. } => {
            let old = name.clone();
            let shown = name.to_string_lossy().into_owned();
            let mut spec = Spec::new(format!("Rename {shown}"), "Rename");
            if let Some(upstream) = upstream {
                spec = spec.line(format!(
                    "{upstream} keeps its name. Push the branch afterwards to publish it under the new name."
                ));
            }
            spec.field = Some(Field {
                label: "New name",
                placeholder: "New branch name",
                initial: shown,
                masked: false,
                validate: Box::new(move |text, ui| {
                    if text.as_bytes() == old.as_bytes() {
                        return Some(String::new());
                    }
                    ui.refs.rename_problem(&old, text)
                }),
            });
            spec
        }
        Ask::Diverged { checkout, ahead, behind, subjects } => diverged_spec(checkout, *ahead, *behind, subjects),
        Ask::Another { checkout } => {
            let (local, remote) = checkout.shown();
            let mut spec = Spec::new(format!("Create a branch from {remote}"), "Create branch")
                .line(format!("The new branch starts at {remote} and tracks it. {local} stays as it is."));
            spec.field = Some(Field {
                label: "Branch name",
                placeholder: "New branch name",
                initial: String::new(),
                masked: false,
                validate: Box::new(|text, ui| {
                    if text.is_empty() {
                        return Some(String::new());
                    }
                    ui.refs.name_problem(Kind::Branch, text)
                }),
            });
            spec
        }
    }
}

fn diverged_spec(checkout: &Checkout, ahead: usize, behind: usize, subjects: &[(String, String)]) -> Spec {
    let (local, remote) = checkout.shown();
    let has = match ahead {
        1 => format!("{local} already exists, with 1 commit that {remote} does not have"),
        n => format!("{local} already exists, with {n} commits that {remote} does not have"),
    };
    let mut spec = Spec::new(format!("Check out {remote}"), format!("Switch to {local} as it is"))
        .line(match behind {
            0 => format!("{has}."),
            n => format!("{has}. It is also {} behind.", notice::commits(n)),
        });
    for (short, subject) in subjects {
        spec = spec.line(format!("{short}  {subject}"));
    }
    match ahead.saturating_sub(subjects.len()) {
        0 => {}
        1 => spec = spec.line("and 1 more commit"),
        n => spec = spec.line(format!("and {n} more commits")),
    }
    spec.line(match ahead {
        1 => format!("Resetting {local} to {remote} takes this commit off {local}."),
        _ => format!("Resetting {local} to {remote} takes these commits off {local}."),
    })
    .choice("Create another branch…", false)
    .choice(format!("Reset {local} to {remote}"), true)
}

fn delete_spec(name: &OsStr, only: usize, subjects: &[(String, String)]) -> Spec {
    let spec = Spec::new(format!("Delete branch {}?", name.to_string_lossy()), "Delete branch");
    if only == 0 {
        return spec.line("Its commits are also on other branches or tags.");
    }
    let mut spec = spec.line(match only {
        1 => "1 commit is only on this branch. It will no longer appear in the graph.".to_owned(),
        n => format!("{n} commits are only on this branch. They will no longer appear in the graph."),
    });
    for (short, subject) in subjects {
        spec = spec.line(format!("{short}  {subject}"));
    }
    match only.saturating_sub(subjects.len()) {
        0 => {}
        1 => spec = spec.line("and 1 more commit"),
        n => spec = spec.line(format!("and {n} more commits")),
    }
    spec.danger()
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Delete { name, tip, .. } => tab.run_operation_then(
            cx,
            "Deleting the branch…",
            "Branch deleted",
            false,
            move |repo| delete(&repo, &name, &tip),
            |_, _| {},
        ),
        Ask::Rename { name, tip, upstream } => tab.run_operation_then(
            cx,
            "Renaming the branch…",
            "Branch renamed",
            false,
            move |repo| {
                rename(&repo, &name, &tip, &answer.text)?;
                let (old, new) = (name.to_string_lossy(), &answer.text);
                let detail = upstream.map_or_else(String::new, |upstream| {
                    format!("{upstream} keeps its name. Push {new} to publish it under the new name.")
                });
                Ok::<_, String>(Outcome::titled(format!("Renamed {old} to {new}"), detail))
            },
            |_, _| {},
        ),
        Ask::Diverged { checkout, ahead, behind, .. } => match answer.choice {
            None => start(tab, Move::Switch { checkout, found: Found::Kept(ahead, behind) }, cx),
            Some(ANOTHER) => prompt::ask(&mut tab.ui, prompt::Kind::Branch(Ask::Another { checkout }), cx),
            Some(RESET) => start(tab, Move::Reset { checkout }, cx),
            Some(_) => {}
        },
        Ask::Another { checkout } => start(tab, Move::Track { checkout, name: OsString::from(answer.text) }, cx),
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Move {
    Track { checkout: Checkout, name: OsString },
    Switch { checkout: Checkout, found: Found },
    Reset { checkout: Checkout },
    Detach { oid: String, tag: Option<OsString>, seen: Seen },
}

impl Move {
    fn doing(&self) -> String {
        match self {
            Self::Track { checkout, .. } => format!("checking out {}", checkout.remote_branch.to_string_lossy()),
            Self::Switch { checkout, .. } => format!("switching to {}", checkout.local.to_string_lossy()),
            Self::Reset { checkout } => {
                let (local, remote) = checkout.shown();
                format!("resetting {local} to {remote}")
            }
            Self::Detach { tag: Some(tag), .. } => format!("checking out {}", tag.to_string_lossy()),
            Self::Detach { oid, .. } => format!("checking out {}", notice::short(oid)),
        }
    }

    fn go(&self, repo: &Repository) -> Result<Outcome, String> {
        match self {
            Self::Track { checkout, name } => track(repo, checkout, name),
            Self::Switch { checkout, found } => switch(repo, checkout, *found),
            Self::Reset { checkout } => reset(repo, checkout),
            Self::Detach { oid, tag, seen } => detach(repo, oid, tag.as_deref(), seen),
        }
    }

    fn run(&self, repo: &Repository, stash: bool, kept: &crate::autostash::Kept) -> Result<Outcome, notice::Failure> {
        let how = crate::autostash::Around { doing: self.doing(), stash, kept };
        crate::autostash::around(
            repo,
            how,
            || crate::autostash::Retry::Branch(Box::new(self.clone())),
            |repo| self.go(repo).map_err(notice::Failure::from),
            |outcome| Some(crate::autostash::lowered(&outcome.title)),
        )
    }
}

fn start(tab: &mut Tab, step: Move, cx: &mut Context<Tab>) {
    let stash = tab.ui.autostash.always;
    run(tab, step, stash, cx);
}

pub(crate) fn run(tab: &mut Tab, step: Move, stash: bool, cx: &mut Context<Tab>) {
    let (progress, done) = match &step {
        Move::Track { .. } => ("Creating the branch…", "Branch created and checked out"),
        Move::Switch { .. } => ("Switching branch…", "Switched branch"),
        Move::Reset { .. } => ("Resetting the branch…", "Branch reset"),
        Move::Detach { .. } => ("Checking out the commit…", "Checked out"),
    };
    let kept = crate::autostash::Kept::default();
    let stopped = kept.clone();
    tab.run_operation_or(
        cx,
        progress,
        done,
        true,
        move |repo| step.run(&repo, stash, &kept),
        |_, _| {},
        move |tab, _| stopped.remember(&mut tab.ui),
    );
}

fn checkout_remote(tab: &mut Tab, name: OsString, cx: &mut Context<Tab>) {
    let target = Target::RemoteBranch(name.clone());
    let Some(tip) = tab.ui.refs.oid_of(&target).map(str::to_owned) else {
        return;
    };
    let reason = match split_remote(&tab.ui.refs.remotes, &name) {
        None => Some("No configured remote has this branch".to_owned()),
        Some((_, local)) => checked_out(&tab.ui, &local),
    };
    if let Some(reason) = reason {
        tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
        return;
    }
    let Some((_, local)) = split_remote(&tab.ui.refs.remotes, &name) else { return };
    let existing = tab.ui.refs.local(&local);
    if existing.is_some_and(|item| item.oid.is_none()) {
        return;
    }
    let local_tip = existing.and_then(|item| item.oid.clone());
    let tracks = existing.and_then(|item| item.upstream.as_deref()).map(str::as_bytes) == Some(name.as_bytes());
    let checkout = Checkout { remote_branch: name, local, tip, local_tip, tracks, seen: Seen::of(&tab.ui) };
    tab.ui.refs.list_open = false;
    let Some(local_tip) = checkout.local_tip.clone() else {
        let name = checkout.local.clone();
        return start(tab, Move::Track { checkout, name }, cx);
    };
    if local_tip == checkout.tip {
        return start(tab, Move::Switch { checkout, found: Found::Same }, cx);
    }
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    let (local_seen, remote_seen) = (local_tip, checkout.tip.clone());
    Tab::work(
        cx,
        move || {
            let own = Revisions::reachable(&local_seen).not(&remote_seen);
            let ahead = repo.count(&own)?;
            let behind = repo.count(&Revisions::reachable(&remote_seen).not(&local_seen))?;
            let subjects = if ahead == 0 { Vec::new() } else { repo.subjects(&own, LISTED)? };
            Ok::<_, String>((ahead, behind, subjects))
        },
        move |tab, counted, cx| {
            let remote_now = tab.ui.refs.oid_of(&Target::RemoteBranch(checkout.remote_branch.clone()));
            let local_now = tab.ui.refs.local(&checkout.local).and_then(|item| item.oid.as_deref());
            if tab.epoch != epoch
                || remote_now != Some(checkout.tip.as_str())
                || local_now != checkout.local_tip.as_deref()
                || tab.ui.busy()
                || tab.ui.dialog_open()
            {
                return;
            }
            match counted {
                Ok((0, behind, _)) => start(tab, Move::Switch { checkout, found: Found::Behind(behind) }, cx),
                Ok((ahead, behind, subjects)) => {
                    let subjects = subjects
                        .into_iter()
                        .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
                        .collect();
                    let ask = Ask::Diverged { checkout, ahead, behind, subjects };
                    prompt::ask(&mut tab.ui, prompt::Kind::Branch(ask), cx);
                }
                Err(error) => tab.error(error, cx),
            }
            cx.notify();
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    match action {
        Action::Delete(name) => {
            let Some(tip) = tab.ui.refs.local(&name).and_then(|item| item.oid.clone()) else {
                return;
            };
            if let Some(reason) = current(&tab.ui, &name) {
                tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
                cx.notify();
                return;
            }
            let epoch = tab.epoch;
            let (counted, seen) = (name.clone(), tip.clone());
            Tab::work(
                cx,
                move || {
                    let own = Revisions::reachable(&seen).only_on(&counted);
                    let only = repo.count(&own)?;
                    let subjects = if only == 0 { Vec::new() } else { repo.subjects(&own, LISTED)? };
                    Ok::<_, String>((only, subjects))
                },
                move |tab, counted, cx| {
                    let unchanged = tab.ui.refs.local(&name).and_then(|item| item.oid.as_deref())
                        == Some(tip.as_str());
                    if tab.epoch != epoch || !unchanged || tab.ui.dialog_open() {
                        return;
                    }
                    match counted {
                        Ok((only, subjects)) => {
                            let subjects = subjects
                                .into_iter()
                                .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
                                .collect();
                            let ask = Ask::Delete { name, tip, only, subjects };
                            prompt::ask(&mut tab.ui, prompt::Kind::Branch(ask), cx);
                        }
                        Err(error) => tab.error(error, cx),
                    }
                },
            );
        }
        Action::Rename(name) => {
            let Some(item) = tab.ui.refs.local(&name) else { return };
            let Some(tip) = item.oid.clone() else { return };
            let upstream = item.upstream.clone();
            tab.ui.menu = None;
            let ask = Ask::Rename { name, tip, upstream };
            prompt::ask(&mut tab.ui, prompt::Kind::Branch(ask), cx);
        }
        Action::CheckoutRemote(name) => checkout_remote(tab, name, cx),
        Action::Detach { oid, tag } => {
            if detached_at(&tab.ui, &oid) {
                tab.ui.notices.show(Notice::new(Tone::Info, "This commit is already checked out", ""));
                cx.notify();
                return;
            }
            let seen = Seen::of(&tab.ui);
            tab.ui.refs.list_open = false;
            start(tab, Move::Detach { oid, tag, seen }, cx);
        }
        Action::Restore { name, oid, tracking } => {
            tab.run_operation_then(
                cx,
                "Restoring the branch…",
                "Branch restored",
                false,
                move |repo| restore(&repo, &name, &oid, tracking.as_ref()),
                |_, _| {},
            );
        }
    }
    cx.notify();
}

fn full(name: &OsStr) -> OsString {
    let mut full = OsString::from("refs/heads/");
    full.push(name);
    full
}

fn plain(name: &OsStr) -> Result<(), String> {
    if name.is_empty() || name.as_bytes()[0] == b'-' {
        return Err("Select a branch by its name.".into());
    }
    Ok(())
}

fn config(repo: &Repository, key: &OsStr) -> Result<Option<OsString>, String> {
    let output = repo
        .read_command()
        .args(["config", "--local", "--get", "--"])
        .arg(key)
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    match output.status.code() {
        Some(0) => {
            let value = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
            Ok(Some(OsStr::from_bytes(value).to_owned()))
        }
        Some(1) => Ok(None),
        _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

fn key(name: &OsStr, field: &str) -> OsString {
    let mut key = OsString::from("branch.");
    key.push(name);
    key.push(".");
    key.push(field);
    key
}

pub(crate) fn delete(repo: &Repository, name: &OsStr, tip: &str) -> Result<Outcome, String> {
    plain(name)?;
    repo.expect_ref(&full(name), tip)?;
    let tracking = match (config(repo, &key(name, "remote"))?, config(repo, &key(name, "merge"))?) {
        (Some(remote), Some(merge)) => Some(Tracking { remote, merge }),
        _ => None,
    };
    checked(repo.command().args(["branch", "-D", "--"]).arg(name))?;
    let shown = name.to_string_lossy();
    let restore = Action::Restore { name: name.to_owned(), oid: tip.to_owned(), tracking };
    Ok(Outcome::titled(format!("Deleted branch {shown}"), format!("was {}", notice::short(tip)))
        .offer("Restore", view::Action::Branch(restore))
        .sticky())
}

fn restore(repo: &Repository, name: &OsStr, oid: &str, tracking: Option<&Tracking>) -> Result<Outcome, String> {
    plain(name)?;
    validate_oid(oid)?;
    if let Some(tracking) = tracking {
        plain(&tracking.remote)?;
        plain(&tracking.merge)?;
    }
    checked(repo.command().args(["branch", "--no-track", "--"]).arg(name).arg(oid))?;
    if let Some(tracking) = tracking {
        for (field, value) in [("remote", &tracking.remote), ("merge", &tracking.merge)] {
            checked(repo.command().args(["config", "--local"]).arg(key(name, field)).arg(value))?;
        }
    }
    let shown = name.to_string_lossy();
    Ok(Outcome::titled(format!("Restored branch {shown}"), format!("at {}", notice::short(oid))))
}

fn rename(repo: &Repository, old: &OsStr, tip: &str, new: &str) -> Result<(), String> {
    plain(old)?;
    if let Some(problem) = refs::name_problem(Kind::Branch, new, []) {
        return Err(problem);
    }
    repo.expect_ref(&full(old), tip)?;
    checked(repo.command().args(["branch", "-m", "--"]).arg(old).arg(new)).map(|_| ())
}

fn expect_unchanged(repo: &Repository, checkout: &Checkout) -> Result<(), String> {
    plain(&checkout.local)?;
    plain(&checkout.remote_branch)?;
    repo.expect_no_operation()?;
    repo.expect_checkout(&checkout.seen)?;
    repo.expect_ref(&checkout.remote_ref(), &checkout.tip)?;
    if let Some(local_tip) = &checkout.local_tip {
        repo.expect_ref(&full(&checkout.local), local_tip)?;
    }
    Ok(())
}

fn track(repo: &Repository, checkout: &Checkout, name: &OsStr) -> Result<Outcome, String> {
    if let Some(problem) = refs::name_problem(Kind::Branch, &name.to_string_lossy(), []) {
        return Err(problem);
    }
    expect_unchanged(repo, checkout)?;
    checked(
        repo.command()
            .args(["switch", "--no-guess", "--track", "-c"])
            .arg(name)
            .arg(checkout.remote_ref()),
    )?;
    let (_, remote) = checkout.shown();
    let name = name.to_string_lossy();
    let outcome = Outcome::titled(format!("Switched to {name}"), format!("A new branch that tracks {remote}."));
    Ok(left_on_no_branch(repo, checkout.seen.detached_tip(), outcome))
}

fn switch(repo: &Repository, checkout: &Checkout, found: Found) -> Result<Outcome, String> {
    expect_unchanged(repo, checkout)?;
    checked(repo.command().args(["switch", "--no-guess", "--"]).arg(&checkout.local))?;
    let (local, remote) = checkout.shown();
    let title = format!("Switched to {local}");
    let outcome = match found {
        Found::Same => Outcome::titled(title, format!("It matches {remote}.")),
        Found::Behind(n) => {
            let outcome = Outcome::titled(title, format!("It is {} behind {remote}.", notice::commits(n)));
            if checkout.tracks {
                outcome.offer("Pull", view::Action::Sync(crate::sync::Action::PullInto(checkout.local.clone())))
            } else {
                outcome
            }
        }
        Found::Kept(ahead, 0) => {
            Outcome::titled(title, format!("It has {} that {remote} does not.", notice::commits(ahead)))
        }
        Found::Kept(ahead, behind) => Outcome::titled(
            title,
            format!("It has {} that {remote} does not, and is {} behind.", notice::commits(ahead), notice::commits(behind)),
        ),
    };
    Ok(left_on_no_branch(repo, checkout.seen.detached_tip(), outcome))
}

fn reset(repo: &Repository, checkout: &Checkout) -> Result<Outcome, String> {
    let Some(local_tip) = &checkout.local_tip else {
        return Err("The branch no longer exists. Reopen the action.".into());
    };
    expect_unchanged(repo, checkout)?;
    checked(
        repo.command()
            .args(["switch", "--no-guess", "--track", "-C"])
            .arg(&checkout.local)
            .arg(checkout.remote_ref()),
    )?;
    let (local, remote) = checkout.shown();
    let outcome = Outcome::titled(
        format!("Reset {local} to {remote}"),
        format!("Now on {local}. It was at {}.", notice::short(local_tip)),
    );
    Ok(left_on_no_branch(repo, checkout.seen.detached_tip(), outcome))
}

fn detach(repo: &Repository, oid: &str, tag: Option<&OsStr>, seen: &Seen) -> Result<Outcome, String> {
    validate_oid(oid)?;
    repo.expect_no_operation()?;
    repo.expect_checkout(seen)?;
    if let Some(tag) = tag {
        let mut full = OsString::from("refs/tags/");
        full.push(tag);
        repo.expect_names_commit(&full, oid)?;
    }
    checked(repo.command().args(["switch", "--no-guess", "--detach", oid]))?;
    let short = notice::short(oid);
    let outcome = match tag {
        None => Outcome::titled(format!("Checked out {short}"), "You are not on a branch."),
        Some(tag) => Outcome::titled(
            format!("Checked out the commit at {}", tag.to_string_lossy()),
            format!("You are not on a branch. HEAD is at {short}."),
        ),
    };
    let outcome = left_on_no_branch(repo, seen.detached_tip(), outcome);
    let branch_here = refs::Action::NewAt(Kind::Branch, oid.to_owned());
    Ok(outcome.offer("Create branch here…", view::Action::Refs(branch_here)))
}

pub(crate) fn detached_head(repo: &Repository) -> Option<String> {
    let attached = repo.read_command().args(["symbolic-ref", "--quiet", "HEAD"]).run().ok()?;
    if attached.status.code() != Some(1) {
        return None;
    }
    let tip = checked(repo.read_command().args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])).ok()?;
    let tip = String::from_utf8_lossy(&tip).trim().to_owned();
    validate_oid(&tip).is_ok().then_some(tip)
}

pub(crate) fn left_on_no_branch(repo: &Repository, old: Option<&str>, mut outcome: Outcome) -> Outcome {
    let Some(old) = old else {
        return outcome;
    };
    let left = left_behind(repo, old);
    if left == 0 {
        return outcome;
    }
    let (old_short, lost) = (notice::short(old), notice::commits(left));
    outcome.detail.push_str(&format!(" You left {lost} behind on no branch, ending at {old_short}."));
    let keep = refs::Action::NewAt(Kind::Branch, old.to_owned());
    outcome.offer(format!("Create branch at {old_short}…"), view::Action::Refs(keep)).sticky()
}

fn left_behind(repo: &Repository, old: &str) -> usize {
    if validate_oid(old).is_err() {
        return 0;
    }
    checked(repo.read_command().args(["rev-list", "--count", old, "--not", "--exclude=refs/stash", "--all", "--"]))
        .ok()
        .and_then(|count| String::from_utf8_lossy(&count).trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn oid(f: &Fixture, revision: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", revision]))
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn exists(f: &Fixture, name: &str) -> bool {
        !f.git(&["for-each-ref", "--format=%(refname)", name]).is_empty()
    }

    fn trail() -> (Fixture, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        f.git(&["branch", "merged"]);
        f.git(&["switch", "-qc", "topic"]);
        for subject in ["Mark the ford", "Mark the bridge"] {
            fs::write(f.dir.join("trail.txt"), format!("{subject}\n")).unwrap();
            f.git(&["commit", "-qam", subject]);
        }
        f.git(&["switch", "-q", "main"]);
        let (main, topic) = (oid(&f, "main"), oid(&f, "topic"));
        (f, main, topic)
    }

    #[test]
    fn counts_what_only_the_branch_holds() {
        let (f, main, topic) = trail();
        let own = |name: &str, tip: &str| Revisions::reachable(tip).only_on(name);
        assert_eq!(f.repo.count(&own("merged", &main)).unwrap(), 0);
        assert_eq!(f.repo.count(&own("topic", &topic)).unwrap(), 2);
        let subjects: Vec<_> = f.repo.subjects(&own("topic", &topic), LISTED).unwrap().into_iter().map(|c| c.subject).collect();
        assert_eq!(subjects, ["Mark the bridge", "Mark the ford"]);
        f.git(&["update-ref", "refs/remotes/origin/topic", &topic]);
        assert_eq!(f.repo.count(&own("topic", &topic)).unwrap(), 0);
    }

    #[test]
    fn delete_removes_the_ref_and_its_upstream_and_restore_brings_both_back() {
        let (f, _, topic) = trail();
        f.git(&["config", "branch.topic.remote", "origin"]);
        f.git(&["config", "branch.topic.merge", "refs/heads/topic"]);
        let outcome = delete(&f.repo, OsStr::new("topic"), &topic).unwrap();
        assert_eq!(outcome.title, "Deleted branch topic");
        assert_eq!(outcome.detail, format!("was {}", &topic[..7]));
        assert!(outcome.sticky);
        assert!(!exists(&f, "refs/heads/topic"));
        let config = String::from_utf8(f.git(&["config", "--local", "--list"])).unwrap();
        assert!(!config.contains("branch.topic."), "{config}");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Start\n");

        let [(label, view::Action::Branch(Action::Restore { name, oid: at, tracking }))] = outcome.actions.as_slice() else {
            panic!("Delete offered {:?}", outcome.actions);
        };
        assert_eq!(label.as_ref(), "Restore");
        let restored = restore(&f.repo, name, at, tracking.as_ref()).unwrap();
        assert_eq!(restored.title, "Restored branch topic");
        assert_eq!(oid(&f, "refs/heads/topic"), topic);
        assert_eq!(f.git(&["config", "branch.topic.remote"]), b"origin\n");
        assert_eq!(f.git(&["config", "branch.topic.merge"]), b"refs/heads/topic\n");
        let err = restore(&f.repo, name, at, None).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn delete_refuses_a_branch_that_moved_or_went_and_the_checked_out_one() {
        let (f, main, topic) = trail();
        f.git(&["update-ref", "refs/heads/topic", &main]);
        let err = delete(&f.repo, OsStr::new("topic"), &topic).unwrap_err();
        assert!(err.contains("refs/heads/topic") || err.contains("topic changed"), "{err}");
        assert_eq!(oid(&f, "refs/heads/topic"), main);
        f.git(&["branch", "-D", "topic"]);
        let err = delete(&f.repo, OsStr::new("topic"), &topic).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        f.git(&["branch", "topic/inner", &topic]);
        let err = delete(&f.repo, OsStr::new("topic"), &topic).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        assert_eq!(oid(&f, "refs/heads/topic/inner"), topic);
        let err = delete(&f.repo, OsStr::new("main"), &main).unwrap_err();
        assert!(err.to_lowercase().contains("main"), "{err}");
        assert_eq!(oid(&f, "refs/heads/main"), main);
        assert!(delete(&f.repo, OsStr::new("-D"), &main).is_err());
    }

    #[test]
    fn delete_passes_on_git_s_refusal_of_a_branch_checked_out_elsewhere() {
        let (f, _, topic) = trail();
        let other = f.dir.with_extension("worktree");
        f.git(&["worktree", "add", "-q", other.to_str().unwrap(), "topic"]);
        let err = delete(&f.repo, OsStr::new("topic"), &topic).unwrap_err();
        assert!(err.contains("worktree"), "{err}");
        assert_eq!(oid(&f, "refs/heads/topic"), topic);
        f.git(&["worktree", "remove", "--force", other.to_str().unwrap()]);
    }

    #[test]
    fn rename_keeps_the_commit_and_the_upstream_and_follows_head() {
        let (f, main, topic) = trail();
        f.git(&["config", "branch.topic.remote", "origin"]);
        f.git(&["config", "branch.topic.merge", "refs/heads/topic"]);
        rename(&f.repo, OsStr::new("topic"), &topic, "trail/bridge").unwrap();
        assert!(!exists(&f, "refs/heads/topic"));
        assert_eq!(oid(&f, "refs/heads/trail/bridge"), topic);
        assert_eq!(f.git(&["config", "branch.trail/bridge.merge"]), b"refs/heads/topic\n");
        assert_eq!(f.git(&["config", "branch.trail/bridge.remote"]), b"origin\n");
        rename(&f.repo, OsStr::new("main"), &main, "trunk").unwrap();
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/trunk\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
    }

    #[test]
    fn rename_refuses_a_moved_branch_a_taken_name_and_a_bad_one() {
        let (f, main, topic) = trail();
        f.git(&["update-ref", "refs/heads/topic", &main]);
        let err = rename(&f.repo, OsStr::new("topic"), &topic, "bridge").unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert!(exists(&f, "refs/heads/topic") && !exists(&f, "refs/heads/bridge"));
        let err = rename(&f.repo, OsStr::new("topic"), &main, "merged").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(oid(&f, "refs/heads/merged"), main);
        assert!(exists(&f, "refs/heads/topic"));
        for bad in ["-M", "has space", "a..b", ""] {
            assert!(rename(&f.repo, OsStr::new("topic"), &main, bad).is_err(), "{bad}");
        }
        assert!(exists(&f, "refs/heads/topic"));
    }

    fn remote_trail() -> (Fixture, String, String, String) {
        let (f, main, topic) = trail();
        f.git(&["remote", "add", "origin", "/nonexistent/origin.git"]);
        f.git(&["switch", "-q", "topic"]);
        fs::write(f.dir.join("trail.txt"), "Mark the stile\n").unwrap();
        f.git(&["commit", "-qam", "Mark the stile"]);
        let ahead = oid(&f, "HEAD");
        f.git(&["update-ref", "refs/remotes/origin/topic", &ahead]);
        f.git(&["update-ref", "refs/remotes/origin/level", &main]);
        f.git(&["reset", "-q", "--hard", &topic]);
        f.git(&["switch", "-q", "main"]);
        (f, main, topic, ahead)
    }

    fn on_main(f: &Fixture, main: &str) -> Seen {
        let _ = f;
        Seen { head_ref: Some("refs/heads/main".into()), head: Some(main.to_owned()) }
    }

    fn checkout(f: &Fixture, remote_branch: &str, local: &str, seen: Seen) -> Checkout {
        let local_tip = exists(f, &format!("refs/heads/{local}")).then(|| oid(f, &format!("refs/heads/{local}")));
        Checkout {
            remote_branch: remote_branch.into(),
            local: local.into(),
            tip: oid(f, &format!("refs/remotes/{remote_branch}")),
            local_tip,
            tracks: true,
            seen,
        }
    }

    fn upstream(f: &Fixture, name: &str) -> String {
        let format = "--format=%(upstream:short)";
        String::from_utf8(f.git(&["for-each-ref", format, &format!("refs/heads/{name}")])).unwrap()
    }

    #[test]
    fn a_remote_branch_names_its_local_branch_after_the_longest_remote() {
        let remotes = ["team".to_owned(), "team/fork".to_owned(), "origin".to_owned()];
        let split = |name: &str| split_remote(&remotes, OsStr::new(name)).map(|(r, l)| (r.to_owned(), l));
        assert_eq!(split("team/fork/main"), Some(("team/fork".to_owned(), "main".into())));
        assert_eq!(split("team/main"), Some(("team".to_owned(), "main".into())));
        assert_eq!(split("origin/feature/x"), Some(("origin".to_owned(), "feature/x".into())));
        assert_eq!(split("origins/x"), None);
        assert_eq!(split("upstream/x"), None);
        assert_eq!(split("origin/"), None);
    }

    #[test]
    fn checking_out_a_new_remote_branch_creates_a_branch_that_tracks_it() {
        let (f, main, _, _) = remote_trail();
        f.git(&["remote", "add", "team/fork", "/nonexistent/fork.git"]);
        f.git(&["update-ref", "refs/remotes/team/fork/feature/x", &main]);
        f.git(&["branch", "team/fork/feature/x", "topic"]);
        let remotes = ["origin".to_owned(), "team/fork".to_owned()];
        let (remote, local) = split_remote(&remotes, OsStr::new("team/fork/feature/x")).unwrap();
        assert_eq!((remote, local.to_str().unwrap()), ("team/fork", "feature/x"));
        let co = checkout(&f, "team/fork/feature/x", "feature/x", on_main(&f, &main));
        let outcome = track(&f.repo, &co, OsStr::new("feature/x")).unwrap();
        assert_eq!(outcome.title, "Switched to feature/x");
        assert_eq!(outcome.detail, "A new branch that tracks team/fork/feature/x.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature/x\n");
        assert_eq!(oid(&f, "refs/heads/feature/x"), main);
        let full = f.git(&["for-each-ref", "--format=%(upstream)", "refs/heads/feature/x"]);
        assert_eq!(full, b"refs/remotes/team/fork/feature/x\n");
        assert_eq!(f.git(&["config", "branch.feature/x.remote"]), b"team/fork\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
        let again = checkout(&f, "team/fork/feature/x", "feature/x", Seen {
            head_ref: Some("refs/heads/feature/x".into()),
            head: Some(main.clone()),
        });
        let err = track(&f.repo, &Checkout { local_tip: None, ..again }, OsStr::new("topic")).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(track(&f.repo, &co, OsStr::new("-f")).is_err());
    }

    #[test]
    fn checking_out_switches_to_a_local_branch_that_matches_or_lags() {
        let (f, main, topic, ahead) = remote_trail();
        f.git(&["branch", "level", &main]);
        f.git(&["switch", "-q", "topic"]);
        let seen = Seen { head_ref: Some("refs/heads/topic".into()), head: Some(topic.clone()) };
        let level = checkout(&f, "origin/level", "level", seen);
        let outcome = switch(&f.repo, &level, Found::Same).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Switched to level", "It matches origin/level."));
        assert!(outcome.actions.is_empty());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/level\n");

        let behind = checkout(&f, "origin/topic", "topic", on_main(&f, &main));
        let seen = Seen { head_ref: Some("refs/heads/level".into()), head: Some(main.clone()) };
        let behind = Checkout { seen, ..behind };
        assert_eq!(f.repo.count(&Revisions::reachable(&topic).not(&ahead)).unwrap(), 0);
        assert_eq!(f.repo.count(&Revisions::reachable(&ahead).not(&topic)).unwrap(), 1);
        let outcome = switch(&f.repo, &behind, Found::Behind(1)).unwrap();
        assert_eq!(outcome.detail, "It is 1 commit behind origin/topic.");
        let [(label, view::Action::Sync(crate::sync::Action::PullInto(name)))] = outcome.actions.as_slice() else {
            panic!("Switching offered {:?}", outcome.actions);
        };
        assert_eq!((label.as_ref(), name.to_str()), ("Pull", Some("topic")));
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/topic\n");
        assert_eq!(oid(&f, "refs/heads/topic"), topic);
        assert_eq!(oid(&f, "refs/remotes/origin/topic"), ahead);
        f.git(&["switch", "-q", "main"]);
        let untracked = Checkout { tracks: false, seen: on_main(&f, &main), ..behind };
        assert!(switch(&f.repo, &untracked, Found::Behind(1)).unwrap().actions.is_empty());
    }

    #[test]
    fn a_diverged_branch_is_kept_reset_or_left_for_another() {
        let (f, main, _, ahead) = remote_trail();
        f.git(&["switch", "-q", "topic"]);
        fs::write(f.dir.join("trail.txt"), "Mark the gate\n").unwrap();
        f.git(&["commit", "-qam", "Mark the gate"]);
        let own = oid(&f, "HEAD");
        f.git(&["switch", "-q", "main"]);
        let co = checkout(&f, "origin/topic", "topic", on_main(&f, &main));
        assert_eq!(f.repo.count(&Revisions::reachable(&own).not(&ahead)).unwrap(), 1);

        let outcome = track(&f.repo, &co, OsStr::new("topic-remote")).unwrap();
        assert_eq!(outcome.title, "Switched to topic-remote");
        assert_eq!(oid(&f, "refs/heads/topic-remote"), ahead);
        assert_eq!(upstream(&f, "topic-remote"), "origin/topic\n");
        assert_eq!(oid(&f, "refs/heads/topic"), own);
        assert_eq!(upstream(&f, "topic"), "\n");

        let seen = Seen { head_ref: Some("refs/heads/topic-remote".into()), head: Some(ahead.clone()) };
        let kept = Checkout { seen, ..co.clone() };
        let outcome = switch(&f.repo, &kept, Found::Kept(1, 1)).unwrap();
        assert_eq!(outcome.detail, "It has 1 commit that origin/topic does not, and is 1 commit behind.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/topic\n");
        assert_eq!(oid(&f, "refs/heads/topic"), own);

        f.git(&["switch", "-q", "main"]);
        let outcome = reset(&f.repo, &co).unwrap();
        assert_eq!(outcome.title, "Reset topic to origin/topic");
        assert_eq!(outcome.detail, format!("Now on topic. It was at {}.", &own[..7]));
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/topic\n");
        assert_eq!(oid(&f, "refs/heads/topic"), ahead);
        assert_eq!(upstream(&f, "topic"), "origin/topic\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Mark the stile\n");
    }

    #[test]
    fn a_check_out_refuses_branches_or_a_checkout_that_changed() {
        let (f, main, topic, ahead) = remote_trail();
        let co = checkout(&f, "origin/topic", "topic", on_main(&f, &main));
        let unchanged = |f: &Fixture| {
            assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
            assert_eq!(oid(f, "refs/heads/topic"), topic);
        };
        f.git(&["update-ref", "refs/remotes/origin/topic", &main]);
        for err in [
            reset(&f.repo, &co).unwrap_err(),
            switch(&f.repo, &co, Found::Behind(1)).unwrap_err(),
            track(&f.repo, &co, OsStr::new("fresh")).unwrap_err(),
        ] {
            assert!(err.contains("origin/topic changed"), "{err}");
        }
        unchanged(&f);
        assert!(!exists(&f, "refs/heads/fresh"));
        f.git(&["update-ref", "refs/remotes/origin/topic", &ahead]);
        f.git(&["update-ref", "refs/heads/topic", &main]);
        let err = reset(&f.repo, &co).unwrap_err();
        assert!(err.contains("topic changed"), "{err}");
        assert_eq!(oid(&f, "refs/heads/topic"), main);
        f.git(&["update-ref", "refs/heads/topic", &topic]);
        f.git(&["switch", "-q", "merged"]);
        let err = reset(&f.repo, &co).unwrap_err();
        assert!(err.contains("checked-out branch"), "{err}");
        assert_eq!(oid(&f, "refs/heads/topic"), topic);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("trail.txt"), "Unsaved\n").unwrap();
        let err = switch(&f.repo, &co, Found::Behind(1)).unwrap_err();
        assert!(err.contains("would be overwritten"), "{err}");
        unchanged(&f);
        f.git(&["checkout", "--", "trail.txt"]);
        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{topic}\n")).unwrap();
        let err = switch(&f.repo, &co, Found::Behind(1)).unwrap_err();
        assert_eq!(err, "Finish or abort the merge first.");
        fs::remove_file(f.dir.join(".git/MERGE_HEAD")).unwrap();
        f.git(&["update-ref", "-d", "refs/remotes/origin/topic"]);
        let err = switch(&f.repo, &co, Found::Same).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        unchanged(&f);
    }

    #[test]
    fn the_diverged_prompt_offers_three_answers_and_lists_what_reset_drops() {
        let co = Checkout {
            remote_branch: "origin/topic".into(),
            local: "topic".into(),
            tip: "a".repeat(40),
            local_tip: Some("b".repeat(40)),
            tracks: true,
            seen: Seen::default(),
        };
        let subjects = vec![("abc0001".to_owned(), "Mark the gate".to_owned())];
        let spec = diverged_spec(&co, 1, 2, &subjects);
        assert!(!spec.danger);
        assert_eq!(spec.title, "Check out origin/topic");
        assert_eq!(spec.confirm.as_ref(), "Switch to topic as it is");
        assert_eq!(
            spec.lines,
            [
                "topic already exists, with 1 commit that origin/topic does not have. It is also 2 commits behind.",
                "abc0001  Mark the gate",
                "Resetting topic to origin/topic takes this commit off topic.",
            ]
        );
        let choices: Vec<_> = spec.choices.iter().map(|c| (c.label.as_ref(), c.danger)).collect();
        assert_eq!(choices[ANOTHER], ("Create another branch…", false));
        assert_eq!(choices[RESET], ("Reset topic to origin/topic", true));
        assert!(matches!(prompt::on_return(&spec, Some(prompt::Stop::Choice(RESET))), prompt::Action::Cancel));
        assert!(matches!(prompt::on_return(&spec, Some(prompt::Stop::Choice(ANOTHER))), prompt::Action::Choose(ANOTHER)));
        assert_eq!(prompt::first(&spec), prompt::Stop::Confirm);
        let many = diverged_spec(&co, 7, 0, &subjects);
        assert_eq!(many.lines[0], "topic already exists, with 7 commits that origin/topic does not have.");
        assert_eq!(many.lines[2], "and 6 more commits");
    }

    fn detached(f: &Fixture) -> bool {
        let head = f.repo.read_command().args(["symbolic-ref", "--quiet", "HEAD"]).output().unwrap();
        head.status.code() == Some(1)
    }

    #[test]
    fn checking_out_a_commit_or_a_tag_detaches_head_there() {
        let (f, main, topic) = trail();
        f.git(&["tag", "-a", "-m", "First release", "v1", &main]);
        let tag_object = oid(&f, "refs/tags/v1");
        fs::write(f.dir.join("notes.txt"), "Kept\n").unwrap();
        let outcome = detach(&f.repo, &topic, None, &on_main(&f, &main)).unwrap();
        assert_eq!(outcome.title, format!("Checked out {}", &topic[..7]));
        assert_eq!(outcome.detail, "You are not on a branch.");
        let [(label, view::Action::Refs(refs::Action::NewAt(Kind::Branch, at)))] = outcome.actions.as_slice() else {
            panic!("Checking out a commit offered {:?}", outcome.actions);
        };
        assert_eq!((label.as_ref(), at.as_str()), ("Create branch here…", topic.as_str()));
        assert!(detached(&f));
        assert_eq!(oid(&f, "HEAD"), topic);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Mark the bridge\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"?? notes.txt\n");
        let branches = f.git(&["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"]);
        assert_eq!(
            String::from_utf8(branches).unwrap(),
            format!("refs/heads/main {main}\nrefs/heads/merged {main}\nrefs/heads/topic {topic}\n")
        );

        let seen = Seen { head_ref: None, head: Some(topic.clone()) };
        let outcome = detach(&f.repo, &main, Some(OsStr::new("v1")), &seen).unwrap();
        assert_eq!(outcome.title, "Checked out the commit at v1");
        assert_eq!(outcome.detail, format!("You are not on a branch. HEAD is at {}.", &main[..7]));
        assert!(detached(&f));
        assert_eq!(oid(&f, "HEAD"), main);
        assert_eq!(oid(&f, "refs/tags/v1"), tag_object);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Start\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"?? notes.txt\n");
    }

    #[test]
    fn leaving_commits_made_on_a_detached_head_says_so_and_offers_a_branch() {
        let (f, main, topic) = trail();
        f.git(&["switch", "-q", "--detach", &topic]);
        let seen = Seen { head_ref: None, head: Some(topic.clone()) };
        let outcome = detach(&f.repo, &main, None, &seen).unwrap();
        assert_eq!(outcome.detail, "You are not on a branch.");
        assert_eq!(outcome.actions.len(), 1);
        assert!(!outcome.sticky);
        f.git(&["switch", "-q", "--detach", &topic]);
        for subject in ["Mark the gate", "Mark the stile"] {
            fs::write(f.dir.join("trail.txt"), format!("{subject}\n")).unwrap();
            f.git(&["commit", "-qam", subject]);
        }
        let made = oid(&f, "HEAD");
        let seen = Seen { head_ref: None, head: Some(made.clone()) };
        let outcome = detach(&f.repo, &main, None, &seen).unwrap();
        assert_eq!(
            outcome.detail,
            format!("You are not on a branch. You left 2 commits behind on no branch, ending at {}.", &made[..7])
        );
        let [
            (keep, view::Action::Refs(refs::Action::NewAt(Kind::Branch, kept))),
            (here, view::Action::Refs(refs::Action::NewAt(Kind::Branch, at))),
        ] = outcome.actions.as_slice()
        else {
            panic!("Leaving commits behind offered {:?}", outcome.actions);
        };
        assert_eq!((keep.as_ref(), kept.as_str()), (format!("Create branch at {}…", &made[..7]).as_str(), made.as_str()));
        assert_eq!((here.as_ref(), at.as_str()), ("Create branch here…", main.as_str()));
        assert!(outcome.sticky);
        assert_eq!(oid(&f, "HEAD"), main);
        assert_eq!(left_behind(&f.repo, &main), 0);
        assert_eq!(left_behind(&f.repo, "not an id"), 0);
    }

    #[test]
    fn switching_or_checking_out_a_remote_branch_from_a_detached_head_says_what_it_left() {
        let (f, main, _, _) = remote_trail();
        assert_eq!(detached_head(&f.repo), None);
        let made_on_no_branch = |f: &Fixture, subject: &str| {
            f.git(&["switch", "-q", "--detach", &main]);
            fs::write(f.dir.join("trail.txt"), format!("{subject}\n")).unwrap();
            f.git(&["commit", "-qam", subject]);
            oid(f, "HEAD")
        };
        let made = made_on_no_branch(&f, "Mark the cairn");
        assert_eq!(detached_head(&f.repo).as_deref(), Some(made.as_str()));
        f.git(&["switch", "-q", "main"]);
        let outcome = left_on_no_branch(&f.repo, Some(&made), Outcome::detail("Now on main."));
        assert_eq!(
            outcome.detail,
            format!("Now on main. You left 1 commit behind on no branch, ending at {}.", &made[..7])
        );
        let [(keep, view::Action::Refs(refs::Action::NewAt(Kind::Branch, kept)))] = outcome.actions.as_slice() else {
            panic!("Leaving a commit behind offered {:?}", outcome.actions);
        };
        assert_eq!((keep.as_ref(), kept.as_str()), (format!("Create branch at {}…", &made[..7]).as_str(), made.as_str()));
        assert!(outcome.sticky);
        let plain = left_on_no_branch(&f.repo, None, Outcome::detail("Now on main."));
        assert_eq!((plain.detail.as_str(), plain.actions.len(), plain.sticky), ("Now on main.", 0, false));
        let made = made_on_no_branch(&f, "Mark the waypost");
        f.git(&["update-ref", "refs/remotes/origin/feature/x", &main]);
        let seen = Seen { head_ref: None, head: Some(made.clone()) };
        let co = checkout(&f, "origin/feature/x", "feature/x", seen);
        let outcome = track(&f.repo, &co, OsStr::new("feature/x")).unwrap();
        assert_eq!(
            outcome.detail,
            format!("A new branch that tracks origin/feature/x. You left 1 commit behind on no branch, ending at {}.", &made[..7])
        );
        assert!(outcome.sticky);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature/x\n");
    }

    #[test]
    fn a_detach_refuses_a_checkout_or_a_tag_that_changed() {
        let (f, main, topic) = trail();
        f.git(&["tag", "-a", "-m", "First release", "v1", &main]);
        let seen = on_main(&f, &main);
        let on_main_still = |f: &Fixture| {
            assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
            assert_eq!(oid(f, "HEAD"), main);
        };
        f.git(&["switch", "-q", "merged"]);
        let err = detach(&f.repo, &topic, None, &seen).unwrap_err();
        assert!(err.contains("checked-out branch"), "{err}");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/merged\n");
        f.git(&["switch", "-q", "main"]);
        f.git(&["tag", "-f", "-a", "-m", "Moved", "v1", &topic]);
        let err = detach(&f.repo, &main, Some(OsStr::new("v1")), &seen).unwrap_err();
        assert_eq!(err, "v1 changed since you chose it. Reopen the action.");
        f.git(&["tag", "-d", "v1"]);
        let err = detach(&f.repo, &main, Some(OsStr::new("v1")), &seen).unwrap_err();
        assert_eq!(err, "v1 no longer exists. Reopen the action.");
        on_main_still(&f);
        fs::write(f.dir.join("trail.txt"), "Unsaved\n").unwrap();
        let err = detach(&f.repo, &topic, None, &seen).unwrap_err();
        assert!(err.contains("would be overwritten"), "{err}");
        on_main_still(&f);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "Unsaved\n");
        f.git(&["checkout", "--", "trail.txt"]);
        fs::write(f.dir.join(".git/MERGE_HEAD"), format!("{topic}\n")).unwrap();
        assert_eq!(detach(&f.repo, &topic, None, &seen).unwrap_err(), "Finish or abort the merge first.");
        fs::remove_file(f.dir.join(".git/MERGE_HEAD")).unwrap();
        assert!(detach(&f.repo, "--orphan", None, &seen).is_err());
        assert!(detach(&f.repo, &"1".repeat(40), None, &seen).is_err());
        on_main_still(&f);
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
    }

    #[test]
    fn the_delete_prompt_says_what_would_be_lost() {
        let topic = OsStr::new("topic");
        let safe = delete_spec(topic, 0, &[]);
        assert!(!safe.danger);
        assert_eq!(safe.title, "Delete branch topic?");
        assert_eq!(safe.confirm.as_ref(), "Delete branch");
        assert_eq!(safe.lines, ["Its commits are also on other branches or tags."]);
        let subjects: Vec<_> = (0..LISTED).map(|i| (format!("abc000{i}"), format!("Step {i}"))).collect();
        let risky = delete_spec(topic, 7, &subjects);
        assert!(risky.danger);
        assert_eq!(
            risky.lines.first().map(String::as_str),
            Some("7 commits are only on this branch. They will no longer appear in the graph.")
        );
        assert_eq!(risky.lines[1], "abc0000  Step 0");
        assert_eq!(risky.lines.len(), 1 + LISTED + 1);
        assert_eq!(risky.lines.last().map(String::as_str), Some("and 2 more commits"));
        let one = delete_spec(topic, 1, &[("abc0000".into(), "Step".into())]);
        assert!(one.danger);
        assert_eq!(one.lines, ["1 commit is only on this branch. It will no longer appear in the graph.", "abc0000  Step"]);
    }

    fn route_in_the_way() -> (Fixture, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nBridge\nRidge\nSummit\n").unwrap();
        f.git(&["add", "--", "route.txt"]);
        f.git(&["commit", "-qm", "Plan the route"]);
        let main = oid(&f, "main");
        f.git(&["switch", "-qc", "ridge"]);
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nBridge\nRidge\nSummit\n").unwrap();
        f.git(&["commit", "-qam", "Start north"]);
        let ridge = oid(&f, "ridge");
        f.git(&["remote", "add", "origin", "https://example.invalid/route.git"]);
        f.git(&["update-ref", "refs/remotes/origin/ridge", &ridge]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["branch", "-D", "ridge"]);
        fs::write(f.dir.join("route.txt"), "Start\nFord\nBridge\nRidge\nSummit east\n").unwrap();
        fs::write(f.dir.join("notes.txt"), "Pack water\n").unwrap();
        (f, main, ridge)
    }

    fn stashes(f: &Fixture) -> Vec<u8> {
        f.git(&["stash", "list", "--format=%gs"])
    }

    #[test]
    fn a_check_out_with_changes_in_its_way_stashes_them_when_asked_and_puts_them_back() {
        let (f, main, ridge) = route_in_the_way();
        let kept = crate::autostash::Kept::default();
        let step = Move::Track { checkout: checkout(&f, "origin/ridge", "ridge", on_main(&f, &main)), name: "ridge".into() };

        let refused = step.run(&f.repo, false, &kept).unwrap_err();
        assert!(refused.text.contains("would be overwritten by checkout"), "{}", refused.text);
        let [(label, view::Action::Autostash(crate::autostash::Action::Retry(crate::autostash::Retry::Branch(again))))] =
            refused.actions.as_slice()
        else {
            panic!("The refusal offered {:?}", refused.actions);
        };
        assert_eq!(label.as_ref(), crate::autostash::RETRY);
        assert!(matches!(&**again, Move::Track { name, .. } if name == "ridge"));
        assert!(!exists(&f, "refs/heads/ridge"));
        assert_eq!(stashes(&f), b"");

        let done = again.run(&f.repo, true, &kept).unwrap();
        assert_eq!(done.title, "Stashed your changes, switched to ridge and restored them");
        assert_eq!(done.detail, "A new branch that tracks origin/ridge.");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/ridge\n");
        assert_eq!(upstream(&f, "ridge"), "origin/ridge\n");
        assert_eq!(oid(&f, "HEAD"), ridge);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nBridge\nRidge\nSummit east\n");
        assert_eq!(fs::read_to_string(f.dir.join("notes.txt")).unwrap(), "Pack water\n");
        assert_eq!(stashes(&f), b"");
        assert!(kept.take().is_none());

        let seen = Seen { head_ref: Some("refs/heads/ridge".into()), head: Some(ridge.clone()) };
        let done = Move::Detach { oid: main.clone(), tag: None, seen }.run(&f.repo, true, &kept).unwrap();
        assert_eq!(done.title, format!("Stashed your changes, checked out {} and restored them", &main[..7]));
        assert!(done.actions.iter().any(|(label, _)| label.as_ref() == "Create branch here…"));
        assert!(detached(&f));
        assert_eq!(oid(&f, "HEAD"), main);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start\nFord\nBridge\nRidge\nSummit east\n");
        assert_eq!(stashes(&f), b"");
    }

    #[test]
    fn a_check_out_that_changed_since_it_was_chosen_stashes_nothing() {
        let (f, main, ridge) = route_in_the_way();
        let kept = crate::autostash::Kept::default();
        let stale = Seen { head_ref: Some("refs/heads/main".into()), head: Some(ridge.clone()) };
        let err = Move::Detach { oid: ridge.clone(), tag: None, seen: stale }.run(&f.repo, true, &kept).unwrap_err();
        assert!(err.actions.is_empty(), "{:?}", err.actions);
        assert_eq!(stashes(&f), b"");
        assert_eq!(oid(&f, "HEAD"), main);
        assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start\nFord\nBridge\nRidge\nSummit east\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b" M route.txt\n?? notes.txt\n");
    }
}
