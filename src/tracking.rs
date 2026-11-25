use crate::{
    Tab,
    commands::Target,
    guard::display,
    notice::{self, Failure, Notice, Outcome},
    prompt::{self, Answer, Field, Spec},
    refs::{Kind, NamedRef},
    remotes::{self, Flag, Lease, PushError, PushReport, PushRequest, Rejection},
    repository::{Repository, Revisions, checked, validate_oid},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy, networking, operating, on_repo};
    fn tracked(ui: &Ui, target: &Target) -> Option<OsString> {
        match target {
            Target::Branch(name) if ui.refs.local(name).is_some() => Some(name.clone()),
            Target::Repo if on_repo(ui, target) && !ui.detached && !ui.branch.is_empty() => {
                let name = OsString::from(ui.branch.clone());
                ui.refs.local(&name).is_some().then_some(name)
            }
            _ => None,
        }
    }
    &[
        Command::new(
            "branch.delete-remote",
            Group::Manage,
            |_, target| match target {
                Target::RemoteBranch(name) => format!("Delete {}…", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                let Target::RemoteBranch(name) = target else {
                    return Availability::Hidden;
                };
                if ui.refs.oid_of(target).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| operating(ui)).or_else(|| {
                    crate::branch::split_remote(&ui.refs.remotes, name)
                        .is_none()
                        .then(|| "No configured remote has this branch".to_owned())
                }))
            },
            |_, target| match target {
                Target::RemoteBranch(name) => {
                    Some(view::Action::Tracking(Action::DeleteRemote { name: name.clone(), local: None }))
                }
                _ => None,
            },
        )
        .words(&["remove remote branch", "delete on remote", "push delete"])
        .icon("x")
        .danger(),
        Command::new(
            "branch.delete-both",
            Group::Manage,
            |ui, target| match pair(ui, target) {
                Some((local, remote)) => format!("Delete {} and {}…", local.name.to_string_lossy(), remote.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| {
                let Some((local, remote)) = pair(ui, target) else {
                    return Availability::Hidden;
                };
                Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| operating(ui)).or_else(|| {
                    if crate::branch::split_remote(&ui.refs.remotes, &remote).is_none() {
                        return Some("No configured remote has this branch".to_owned());
                    }
                    checked_out(ui, &local.name).then(|| "Switch to another branch first".to_owned())
                }))
            },
            |ui, target| {
                let (local, remote) = pair(ui, target)?;
                Some(view::Action::Tracking(Action::DeleteRemote { name: remote, local: Some(local.name.clone()) }))
            },
        )
        .words(&["remove local and remote branch", "delete everywhere"])
        .icon("x")
        .danger(),
        Command::new(
            "branch.set-upstream",
            Group::Remotes,
            |ui, target| match tracked(ui, target) {
                Some(name) => format!("Set upstream of {}…", name.to_string_lossy()),
                None => "Set upstream…".into(),
            },
            |ui, target, _| {
                let Some(name) = tracked(ui, target) else {
                    return Availability::Hidden;
                };
                let tracks = ui.refs.local(&name).is_some_and(|item| item.upstream.is_some());
                if !tracks && ui.refs.remotes.is_empty() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    (!tracks && !ui.refs.items.iter().any(|item| item.kind == Kind::Remote))
                        .then(|| "Fetch a remote first, so there is a remote branch to track".to_owned())
                }))
            },
            |ui, target| Some(view::Action::Tracking(Action::SetUpstream(tracked(ui, target)?))),
        )
        .words(&["track remote branch", "upstream", "unset upstream", "stop tracking"])
        .icon("cloud"),
    ]
};

fn pair<'a>(ui: &'a Ui, target: &Target) -> Option<(&'a NamedRef, OsString)> {
    match target {
        Target::Branch(name) => {
            let local = ui.refs.local(name)?;
            let upstream = OsString::from(local.upstream.clone()?);
            ui.refs.oid_of(&Target::RemoteBranch(upstream.clone()))?;
            Some((local, upstream))
        }
        Target::RemoteBranch(name) => {
            ui.refs.oid_of(target)?;
            Some((follower(&ui.refs.items, &ui.refs.remotes, name)?, name.clone()))
        }
        _ => None,
    }
}

fn follower<'a>(items: &'a [NamedRef], remotes: &[String], remote_branch: &OsStr) -> Option<&'a NamedRef> {
    let same = crate::branch::split_remote(remotes, remote_branch).map(|(_, branch)| branch);
    let mut followers = items.iter().filter(|item| tracks(item, remote_branch));
    let first = followers.next()?;
    Some(
        std::iter::once(first)
            .chain(followers)
            .find(|item| same.as_deref() == Some(item.name.as_os_str()))
            .unwrap_or(first),
    )
}

fn tracks(item: &NamedRef, remote_branch: &OsStr) -> bool {
    item.kind == Kind::Branch && item.upstream.as_deref().map(str::as_bytes) == Some(remote_branch.as_bytes())
}

fn offered<'a>(
    items: &'a [NamedRef],
    remotes: &[String],
    remote_branch: &OsStr,
    chosen: Option<&OsStr>,
) -> Option<(&'a NamedRef, bool)> {
    chosen
        .and_then(|name| items.iter().find(|item| item.name == name && tracks(item, remote_branch)))
        .map(|item| (item, true))
        .or_else(|| follower(items, remotes, remote_branch).map(|item| (item, false)))
}

fn checked_out(ui: &Ui, name: &OsStr) -> bool {
    !ui.detached && ui.branch.as_bytes() == name.as_bytes()
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    DeleteRemote { name: OsString, local: Option<OsString> },
    RestoreRemote { remote: String, branch: OsString, oid: String },
    SetUpstream(OsString),
    Track { name: OsString, seen: Option<String>, chosen: String },
}

#[derive(Clone, Debug)]
pub(crate) struct Doomed {
    remote_branch: OsString,
    remote: String,
    branch: OsString,
    tip: String,
    only: usize,
    subjects: Vec<(String, String)>,
    local: Option<Follower>,
}

#[derive(Clone, Debug)]
pub(crate) struct Follower {
    name: OsString,
    tip: String,
    only: usize,
    current: bool,
    ticked: bool,
}

impl Doomed {
    fn tracking_ref(&self) -> OsString {
        let mut full = OsString::from("refs/remotes/");
        full.push(&self.remote_branch);
        full
    }

    fn remote_ref(&self) -> OsString {
        let mut full = OsString::from("refs/heads/");
        full.push(&self.branch);
        full
    }
}

#[derive(Debug)]
pub(crate) enum Ask {
    DeleteRemote(Doomed),
    SetUpstream {
        name: OsString,
        upstream: Option<String>,
        gone: bool,
        named: Vec<String>,
        initial: String,
    },
}

const LISTED: usize = 5;

const STOP: usize = 0;

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::DeleteRemote(doomed) => delete_spec(doomed),
        Ask::SetUpstream { name, upstream, gone, named, initial } => {
            upstream_spec(name, upstream.as_deref(), *gone, named, initial)
        }
    }
}

fn delete_spec(doomed: &Doomed) -> Spec {
    let shown = doomed.remote_branch.to_string_lossy();
    let remote = &doomed.remote;
    let mut spec = Spec::new(format!("Delete {shown}?"), format!("Delete {shown}"))
        .line(format!("This removes the branch for everyone who uses {remote}."));
    if doomed.only > 0 {
        spec = spec.line(match doomed.only {
            1 => format!("1 commit is only on {shown}. It will no longer appear in the graph."),
            n => format!("{n} commits are only on {shown}. They will no longer appear in the graph."),
        });
        for (short, subject) in &doomed.subjects {
            spec = spec.line(format!("{short}  {subject}"));
        }
        match doomed.only.saturating_sub(doomed.subjects.len()) {
            0 => {}
            1 => spec = spec.line("and 1 more commit"),
            n => spec = spec.line(format!("and {n} more commits")),
        }
    }
    spec = spec.line(format!(
        "It is deleted only while {remote} still has it at {}, where it was when you last fetched.",
        notice::short(&doomed.tip)
    ));
    if let Some(local) = &doomed.local {
        let name = local.name.to_string_lossy();
        if local.current {
            spec = spec.line(format!("{name} tracks it and stays, because it is checked out."));
        } else {
            let more = local.only.saturating_sub(doomed.only);
            if more > 0 {
                spec = spec.line(format!(
                    "Deleting {name} too takes {} more out of the graph.",
                    notice::commits(more)
                ));
            }
            spec.tick = Some((format!("Also delete {name} here").into(), local.ticked));
        }
    }
    spec.danger()
}

fn upstream_spec(name: &OsStr, upstream: Option<&str>, gone: bool, named: &[String], initial: &str) -> Spec {
    let shown = name.to_string_lossy().into_owned();
    let mut spec = Spec::new(format!("Set upstream of {shown}"), "Set upstream")
        .line(format!("Which remote branch should {shown} push to and pull from?"))
        .line(match (upstream, gone) {
            (Some(upstream), true) => format!("It tracks {upstream}, which no longer exists."),
            (Some(upstream), false) => format!("It tracks {upstream} now."),
            (None, _) => "It tracks no remote branch now.".to_owned(),
        });
    if !named.is_empty() {
        spec = spec.line(format!("Remote branches named {shown}: {}.", sentence_list(named)));
    }
    let current = upstream.map(str::to_owned);
    spec.field = Some(Field {
        label: "Remote branch",
        placeholder: "origin/main",
        initial: initial.to_owned(),
        masked: false,
        validate: Box::new(move |text, ui| {
            if text.is_empty() || current.as_deref() == Some(text) {
                return Some(String::new());
            }
            let listed = ui.refs.oid_of(&Target::RemoteBranch(text.into())).is_some();
            (!listed).then(|| format!("There is no remote branch {text}. Fetch first, or name one such as origin/main."))
        }),
    });
    if let Some(upstream) = upstream {
        spec = spec.choice(format!("Stop tracking {upstream}"), false);
    }
    spec
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::DeleteRemote(doomed) => {
            let with_local = answer.tick && doomed.local.as_ref().is_some_and(|local| !local.current);
            let spec = crate::jobs::Spec::push(Some(doomed.remote.clone()));
            tab.run_network(
                cx,
                if with_local { spec.writing() } else { spec },
                "Deleting the remote branch…",
                "Remote branch deleted",
                false,
                move |repo| delete_remote(&repo, &doomed, with_local),
                |_, _| {},
            );
        }
        Ask::SetUpstream { name, upstream, .. } => {
            let chosen = match answer.choice {
                None => Some(answer.text),
                Some(STOP) => None,
                Some(_) => return,
            };
            tab.run_operation_then(
                cx,
                "Setting the upstream…",
                "Upstream set",
                false,
                move |repo| set_upstream(&repo, &name, upstream.as_deref(), chosen.as_deref()),
                |_, _| {},
            );
        }
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    match action {
        Action::DeleteRemote { name, local: chosen } => {
            let target = Target::RemoteBranch(name.clone());
            let Some(tip) = tab.ui.refs.oid_of(&target).map(str::to_owned) else { return };
            let Some((remote, branch)) = crate::branch::split_remote(&tab.ui.refs.remotes, &name) else {
                tab.ui.notices.show(Notice::new(Tone::Info, "No configured remote has this branch", ""));
                cx.notify();
                return;
            };
            let remote = remote.to_owned();
            let (follower, ticked) = match offered(&tab.ui.refs.items, &tab.ui.refs.remotes, &name, chosen.as_deref()) {
                Some((item, ticked)) => {
                    (item.oid.clone().map(|tip| (item.name.clone(), tip, checked_out(&tab.ui, &item.name))), ticked)
                }
                None => (None, false),
            };
            tab.ui.menu = None;
            let epoch = tab.epoch;
            let (counted, seen, local) = (name.clone(), tip.clone(), follower.clone());
            Tab::work(
                cx,
                move || {
                    let alone = Revisions::reachable(&seen).only_on_remote(&counted);
                    let only = repo.count(&alone)?;
                    let subjects = if only == 0 { Vec::new() } else { repo.subjects(&alone, LISTED)? };
                    let both = match &local {
                        Some((local, local_tip, _)) => {
                            let both = Revisions::reachable(&seen).and(local_tip).only_on(local).only_on_remote(&counted);
                            repo.count(&both)?
                        }
                        None => 0,
                    };
                    Ok::<_, String>((only, subjects, both))
                },
                move |tab, counted, cx| {
                    let local_now = follower
                        .as_ref()
                        .map(|(local, _, _)| tab.ui.refs.local(local).and_then(|item| item.oid.clone()));
                    if tab.epoch != epoch
                        || tab.ui.refs.oid_of(&target) != Some(tip.as_str())
                        || local_now != follower.as_ref().map(|(_, local_tip, _)| Some(local_tip.clone()))
                        || tab.ui.busy()
                        || tab.ui.dialog_open()
                    {
                        return;
                    }
                    match counted {
                        Ok((only, subjects, both)) => {
                            let subjects = subjects
                                .into_iter()
                                .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
                                .collect();
                            let local = follower.map(|(name, tip, current)| Follower { name, tip, only: both, current, ticked });
                            let doomed = Doomed { remote_branch: name, remote, branch, tip, only, subjects, local };
                            prompt::ask(&mut tab.ui, prompt::Kind::Tracking(Ask::DeleteRemote(doomed)), cx);
                        }
                        Err(error) => tab.error(error, cx),
                    }
                    cx.notify();
                },
            );
        }
        Action::RestoreRemote { remote, branch, oid } => tab.run_network(
            cx,
            crate::jobs::Spec::push(Some(remote.clone())),
            "Restoring the remote branch…",
            "Remote branch restored",
            false,
            move |repo| restore_remote(&repo, &remote, &branch, &oid),
            |_, _| {},
        ),
        Action::Track { name, seen, chosen } => {
            tab.run_operation_then(
                cx,
                "Setting the upstream…",
                "Upstream set",
                false,
                move |repo| set_upstream(&repo, &name, seen.as_deref(), Some(&chosen)),
                |_, _| {},
            );
        }
        Action::SetUpstream(name) => {
            let Some(item) = tab.ui.refs.local(&name) else { return };
            let (upstream, gone) = (item.upstream.clone(), item.gone);
            let mut named: Vec<String> = tab
                .ui
                .refs
                .items
                .iter()
                .filter(|item| item.kind == Kind::Remote)
                .filter(|item| {
                    crate::branch::split_remote(&tab.ui.refs.remotes, &item.name)
                        .is_some_and(|(_, branch)| branch == name)
                })
                .map(|item| item.name.to_string_lossy().into_owned())
                .collect();
            named.sort();
            let home = crate::tag::home_remote(&tab.ui).map(|remote| format!("{remote}/{}", name.to_string_lossy()));
            let initial = upstream
                .clone()
                .filter(|_| !gone)
                .or_else(|| home.filter(|home| named.contains(home)))
                .or_else(|| named.first().cloned())
                .unwrap_or_default();
            tab.ui.menu = None;
            let ask = Ask::SetUpstream { name, upstream, gone, named, initial };
            prompt::ask(&mut tab.ui, prompt::Kind::Tracking(ask), cx);
        }
    }
    cx.notify();
}

pub(crate) fn tooltip(item: &NamedRef) -> String {
    let Some(upstream) = &item.upstream else {
        return "Tracks no remote branch".to_owned();
    };
    if item.gone {
        return format!("Tracks {upstream}, which no longer exists. Set another upstream, or push to create it again");
    }
    match (item.ahead, item.behind) {
        (0, 0) => format!("Tracks {upstream}, and matches it"),
        (ahead, 0) => format!("Tracks {upstream}. {} ahead", notice::commits(ahead)),
        (0, behind) => format!("Tracks {upstream}. {} behind", notice::commits(behind)),
        (ahead, behind) => format!(
            "Tracks {upstream}. {} ahead and {} behind",
            notice::commits(ahead),
            notice::commits(behind)
        ),
    }
}

pub(crate) fn counts(item: &NamedRef, current: bool) -> Vec<(&'static str, String, String)> {
    let Some(upstream) = item.upstream.as_deref().filter(|_| !item.gone) else {
        return Vec::new();
    };
    let mut buttons = Vec::new();
    if item.ahead > 0 {
        let tip = format!("{} ahead of {upstream}. Click to push", notice::commits(item.ahead));
        buttons.push(("branch.push", format!("↑{}", item.ahead), tip));
    }
    if item.behind > 0 {
        let (id, verb) = if current { ("branch.pull", "pull") } else { ("branch.fast-forward", "fast-forward") };
        let tip = format!("{} behind {upstream}. Click to {verb}", notice::commits(item.behind));
        buttons.push((id, format!("↓{}", item.behind), tip));
    }
    buttons
}

fn sentence_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn plain(name: &OsStr) -> Result<(), String> {
    if name.is_empty() || name.as_bytes()[0] == b'-' {
        return Err("Select a branch by its name.".into());
    }
    Ok(())
}

pub(crate) fn expect_remote(repo: &Repository, remote: &str) -> Result<(), String> {
    let names = checked(repo.read_command().arg("remote"))?;
    if !String::from_utf8_lossy(&names).lines().any(|name| name == remote) {
        return Err(format!("The remote {remote} no longer exists. Reopen the action."));
    }
    Ok(())
}

pub(crate) fn expect_tracks(repo: &Repository, remote: &str, remote_ref: &OsStr, tracking: &OsStr) -> Result<(), String> {
    remotes::check_fetch_mapping(repo, remote, &remote_ref.to_string_lossy(), false)?;
    let mapped = remotes::mapped(repo, remote, remote_ref.as_bytes())?;
    if !mapped.iter().any(|local| local == tracking.as_bytes()) {
        return Err(format!(
            "The fetch mapping of {remote} does not keep {} at {}. Review remote.{remote}.fetch first.",
            display(remote_ref),
            display(tracking)
        ));
    }
    Ok(())
}

fn delete_remote(repo: &Repository, doomed: &Doomed, with_local: bool) -> Result<Outcome, Failure> {
    remotes::validate_name(&doomed.remote)?;
    plain(&doomed.branch)?;
    validate_oid(&doomed.tip)?;
    let (remote, tip) = (doomed.remote.as_str(), doomed.tip.as_str());
    let (remote_ref, tracking) = (doomed.remote_ref(), doomed.tracking_ref());
    let shown = doomed.remote_branch.to_string_lossy().into_owned();
    let local = doomed.local.as_ref().filter(|_| with_local);
    let mut deletion = OsString::from(":");
    deletion.push(&remote_ref);
    let request = PushRequest {
        remote: remote.into(),
        refspecs: vec![deletion],
        lease: Some(Lease { remote_ref: remote_ref.clone(), expect: tip.to_owned() }),
        set_upstream: false,
    };
    let report = remotes::push(repo, &request, |repo| {
        expect_remote(repo, remote)?;
        expect_tracks(repo, remote, &remote_ref, &tracking)?;
        repo.expect_ref(&tracking, tip)?;
        if let Some(local) = local {
            let mut full = OsString::from("refs/heads/");
            full.push(&local.name);
            repo.expect_ref(&full, &local.tip)?;
        }
        Ok(())
    });
    let branch = doomed.branch.to_string_lossy();
    match report {
        Ok(report) if deleted(&report, &remote_ref) => {}
        Ok(report) => {
            return Err(Failure::from(format!("Delete Failed — Git did not say whether {remote} deleted {branch}."))
                .output(report.output));
        }
        Err(PushError::Refused(report)) => return Err(refused(report, remote, &branch, &shown)),
        Err(PushError::Failed(message)) => return Err(Failure::from(format!("Delete Failed — {message}"))),
    }
    let restore = Action::RestoreRemote { remote: remote.to_owned(), branch: doomed.branch.clone(), oid: tip.to_owned() };
    let restore = view::Action::Tracking(restore);
    let short = notice::short(tip);
    let Some(local) = local else {
        return Ok(Outcome::titled(format!("Deleted {shown}"), format!("was {short}"))
            .offer("Restore", restore)
            .sticky());
    };
    let name = local.name.to_string_lossy();
    match crate::branch::delete(repo, &local.name, &local.tip) {
        Ok(gone) => {
            let detail = if local.tip == tip {
                format!("Both were at {short}.")
            } else {
                format!("{name} was {}, {shown} was {short}.", notice::short(&local.tip))
            };
            let mut outcome = Outcome::titled(format!("Deleted {name} and {shown}"), detail).sticky();
            for (_, action) in gone.actions {
                outcome = outcome.offer(format!("Restore {name}"), action);
            }
            Ok(outcome.offer(format!("Restore {shown}"), restore))
        }
        Err(error) => Err(Failure::from(format!("Delete Failed — Deleted {shown}, but not {name}: {error}"))
            .offer(format!("Restore {shown}"), restore)),
    }
}

fn deleted(report: &PushReport, remote_ref: &OsStr) -> bool {
    report.refs.iter().any(|pushed| pushed.destination == remote_ref && pushed.flag == Flag::Deleted)
}

fn refused(report: PushReport, remote: &str, branch: &str, shown: &str) -> Failure {
    let why = report.rejection().map(|(_, why)| why);
    let raw = report.output.clone();
    let text = match &why {
        Some(Rejection::Stale) => {
            return Failure::from(format!("Delete Failed — {shown} changed on the remote. Fetch and look again."))
                .offer("Fetch", view::Action::Sync(crate::sync::Action::FetchAll))
                .output(raw);
        }
        Some(Rejection::Remote(reason)) => {
            let said = format!("{reason}\n{raw}").to_lowercase();
            if said.contains("protected") {
                format!("{remote} protects {branch}, so it cannot be deleted there.")
            } else if said.contains("current branch") {
                format!("{branch} is the default branch of {remote}, which refuses to delete it.")
            } else if said.contains("checked out") {
                format!("{branch} is checked out in {remote}, which refuses to delete it.")
            } else {
                format!("{remote} refused to delete {branch}: {reason}.")
            }
        }
        _ => return Failure::from(format!("Delete Failed — {}", String::from(PushError::Refused(report)))).output(raw),
    };
    Failure::from(format!("Delete Failed — {text}")).output(raw).worded()
}

fn restore_remote(repo: &Repository, remote: &str, branch: &OsStr, oid: &str) -> Result<Outcome, Failure> {
    remotes::validate_name(remote)?;
    plain(branch)?;
    validate_oid(oid)?;
    repo.resolve(OsStr::new(oid))?;
    let mut remote_ref = OsString::from("refs/heads/");
    remote_ref.push(branch);
    let mut tracking = OsString::from(format!("refs/remotes/{remote}/"));
    tracking.push(branch);
    let mut refspec = OsString::from(format!("{oid}:"));
    refspec.push(&remote_ref);
    let request = PushRequest {
        remote: remote.into(),
        refspecs: vec![refspec],
        lease: Some(Lease { remote_ref: remote_ref.clone(), expect: String::new() }),
        set_upstream: false,
    };
    let report = remotes::push(repo, &request, |repo| {
        expect_remote(repo, remote)?;
        expect_tracks(repo, remote, &remote_ref, &tracking)
    });
    let shown = format!("{remote}/{}", branch.to_string_lossy());
    let short = notice::short(oid);
    match report {
        Ok(report) => match report.refs.iter().find(|pushed| pushed.destination == remote_ref).map(|pushed| pushed.flag) {
            Some(Flag::UpToDate) => Ok(Outcome::titled(format!("{remote} already has {shown}"), format!("It is at {short}."))),
            Some(_) => Ok(Outcome::titled(format!("Restored {shown}"), format!("at {short}"))),
            None => Err(Failure::from(format!("Restore Failed — Git did not say whether {remote} took {shown}."))),
        },
        Err(PushError::Refused(report)) => Err(match report.rejection().map(|(_, why)| why) {
            Some(Rejection::Stale) => Failure::from(format!(
                "Restore Failed — {remote} has a branch {} again. Fetch and look again.",
                branch.to_string_lossy()
            ))
            .offer("Fetch", view::Action::Sync(crate::sync::Action::FetchAll))
            .output(report.output),
            _ => Failure::from(format!("Restore Failed — {}", String::from(PushError::Refused(report)))),
        }),
        Err(PushError::Failed(message)) => Err(Failure::from(format!("Restore Failed — {message}"))),
    }
}

fn upstream_now(repo: &Repository, name: &OsStr) -> Result<Option<String>, String> {
    let mut full = OsString::from("refs/heads/");
    full.push(name);
    let listed = checked(
        repo.read_command()
            .args(["for-each-ref", "--format=%(refname)%00%(upstream)%00%(upstream:remotename)", "--"])
            .arg(&full),
    )?;
    let entry = listed
        .split(|&b| b == b'\n')
        .map(|line| line.split(|&b| b == 0).collect::<Vec<_>>())
        .find(|fields| fields.first() == Some(&full.as_bytes()))
        .ok_or_else(|| format!("{} no longer exists. Reopen the action.", name.to_string_lossy()))?;
    match entry.as_slice() {
        [_, upstream, remote] if !remote.is_empty() && *remote != b"." => Ok(upstream
            .strip_prefix(b"refs/remotes/")
            .filter(|short| !short.is_empty())
            .map(|short| String::from_utf8_lossy(short).into_owned())),
        _ => Ok(None),
    }
}

fn set_upstream(repo: &Repository, name: &OsStr, seen: Option<&str>, chosen: Option<&str>) -> Result<Outcome, String> {
    plain(name)?;
    if upstream_now(repo, name)?.as_deref() != seen {
        return Err(format!("The upstream of {} changed since you chose it. Reopen the action.", name.to_string_lossy()));
    }
    let shown = name.to_string_lossy();
    let Some(chosen) = chosen else {
        let Some(seen) = seen else {
            return Ok(Outcome::titled(format!("{shown} tracks nothing"), "It had no upstream."));
        };
        checked(repo.command().args(["branch", "--unset-upstream", "--"]).arg(name))?;
        return Ok(Outcome::titled(format!("{shown} tracks nothing"), format!("It no longer tracks {seen}.")));
    };
    if chosen.is_empty() || chosen.starts_with('-') {
        return Err("Name a remote branch, such as origin/main.".into());
    }
    let tracking = format!("refs/remotes/{chosen}");
    let tip = repo.resolve(OsStr::new(&tracking)).map_err(|_| format!("There is no remote branch {chosen}. Fetch first."))?;
    let mut option = OsString::from("--set-upstream-to=");
    option.push(&tracking);
    checked(repo.command().arg("branch").arg(option).arg("--").arg(name))?;
    let mut local = OsString::from("refs/heads/");
    local.push(name);
    let local = repo.resolve(&local)?;
    let ahead = repo.count(&Revisions::reachable(&local).not(&tip))?;
    let behind = repo.count(&Revisions::reachable(&tip).not(&local))?;
    let detail = match (ahead, behind) {
        (0, 0) => format!("It matches {chosen}."),
        (ahead, 0) => format!("It is {} ahead of {chosen}.", notice::commits(ahead)),
        (0, behind) => format!("It is {} behind {chosen}.", notice::commits(behind)),
        (ahead, behind) => format!(
            "It is {} ahead of {chosen} and {} behind.",
            notice::commits(ahead),
            notice::commits(behind)
        ),
    };
    Ok(Outcome::titled(format!("{shown} tracks {chosen}"), detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", name])).unwrap().trim_end().to_owned()
    }

    fn remote_heads(f: &Fixture, url: &str) -> String {
        String::from_utf8(f.git(&["--git-dir", url, "for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"]))
            .unwrap()
    }

    fn exists(f: &Fixture, name: &str) -> bool {
        !f.git(&["for-each-ref", "--format=%(refname)", name]).is_empty()
    }

    fn published(dir: &Path) -> (Fixture, String, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        let url = dir.join("origin.git").to_str().unwrap().to_owned();
        f.git(&["init", "-q", "--bare", "-b", "main", &url]);
        f.git(&["remote", "add", "origin", &url]);
        f.git(&["push", "-q", "-u", "origin", "main"]);
        f.git(&["switch", "-qc", "topic"]);
        fs::write(f.dir.join("trail.txt"), "Mark the ford\n").unwrap();
        f.git(&["commit", "-qam", "Mark the ford"]);
        f.git(&["push", "-q", "-u", "origin", "topic"]);
        f.git(&["switch", "-q", "main"]);
        let (main, topic) = (rev(&f, "main"), rev(&f, "topic"));
        (f, main, topic, url)
    }

    fn doomed(f: &Fixture, local: Option<&str>) -> Doomed {
        let tip = rev(f, "refs/remotes/origin/topic");
        Doomed {
            remote_branch: "origin/topic".into(),
            remote: "origin".into(),
            branch: "topic".into(),
            tip,
            only: 0,
            subjects: Vec::new(),
            local: local.map(|name| Follower {
                name: name.into(),
                tip: rev(f, &format!("refs/heads/{name}")),
                only: 0,
                current: false,
                ticked: true,
            }),
        }
    }

    #[test]
    fn a_remote_branch_is_deleted_under_a_lease_and_restore_pushes_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let (f, main, topic, url) = published(dir.path());
        let outcome = delete_remote(&f.repo, &doomed(&f, None), false).unwrap();
        assert_eq!(outcome.title, "Deleted origin/topic");
        assert_eq!(outcome.detail, format!("was {}", &topic[..7]));
        assert!(outcome.sticky);
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\n"));
        assert!(!exists(&f, "refs/remotes/origin/topic"));
        assert_eq!(rev(&f, "refs/heads/topic"), topic);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");

        let [(label, view::Action::Tracking(Action::RestoreRemote { remote, branch, oid }))] = outcome.actions.as_slice() else {
            panic!("Deleting offered {:?}", outcome.actions);
        };
        assert_eq!(label.as_ref(), "Restore");
        let restored = restore_remote(&f.repo, remote, branch, oid).unwrap();
        assert_eq!((restored.title.as_str(), restored.detail.as_str()), ("Restored origin/topic", format!("at {}", &topic[..7]).as_str()));
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {topic}\n"));
        assert_eq!(rev(&f, "refs/remotes/origin/topic"), topic);
        f.git(&["push", "-q", "origin", "--delete", "topic"]);
        f.git(&["push", "-q", "origin", "main:refs/heads/topic"]);
        let err = restore_remote(&f.repo, remote, branch, oid).unwrap_err();
        assert!(err.text.contains("has a branch topic again"), "{}", err.text);
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {main}\n"));
    }

    #[test]
    fn a_remote_branch_that_moved_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (f, main, topic, url) = published(dir.path());
        let seen = doomed(&f, None);
        let other = dir.path().join("other");
        f.git(&["clone", "-q", &url, other.to_str().unwrap()]);
        let at = |args: &[&str]| f.git(&[&["-C", other.to_str().unwrap()], args].concat());
        at(&["switch", "-q", "topic"]);
        at(&["-c", "user.name=Peer", "-c", "user.email=peer@example.invalid", "commit", "-q", "--allow-empty", "-m", "Mark the stile"]);
        at(&["push", "-q", "origin", "topic"]);
        let moved = String::from_utf8(at(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let err = delete_remote(&f.repo, &seen, false).unwrap_err();
        assert_eq!(err.text, "Delete Failed — origin/topic changed on the remote. Fetch and look again.");
        assert_eq!(err.actions.iter().map(|(label, _)| label.as_ref()).collect::<Vec<_>>(), ["Fetch"]);
        assert!(err.output.as_deref().is_some_and(|output| output.contains("failed to push")), "{:?}", err.output);
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {moved}\n"));
        assert_eq!(rev(&f, "refs/remotes/origin/topic"), topic);
        f.git(&["fetch", "-q", "origin"]);
        let err = delete_remote(&f.repo, &seen, false).unwrap_err();
        assert!(err.text.contains("origin/topic changed since you chose it"), "{}", err.text);
        let fresh = doomed(&f, None);
        f.git(&["config", "--replace-all", "remote.origin.fetch", "+refs/heads/*:refs/remotes/mirror/*"]);
        let err = delete_remote(&f.repo, &fresh, false).unwrap_err();
        assert!(err.text.contains("does not keep topic at origin/topic"), "{}", err.text);
        f.git(&["config", "--replace-all", "remote.origin.fetch", "+refs/heads/*:refs/heads/*"]);
        let err = delete_remote(&f.repo, &fresh, false).unwrap_err();
        assert!(err.text.contains("outside refs/remotes/"), "{}", err.text);
        f.git(&["remote", "rename", "origin", "upstream"]);
        let err = delete_remote(&f.repo, &fresh, false).unwrap_err();
        assert!(err.text.contains("The remote origin no longer exists"), "{}", err.text);
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {moved}\n"));
        assert_eq!(rev(&f, "refs/heads/topic"), topic);
    }

    #[test]
    fn deleting_both_removes_the_local_branch_after_the_remote_one_and_offers_both_back() {
        let dir = tempfile::tempdir().unwrap();
        let (f, main, topic, url) = published(dir.path());
        let outcome = delete_remote(&f.repo, &doomed(&f, Some("topic")), true).unwrap();
        assert_eq!(outcome.title, "Deleted topic and origin/topic");
        assert_eq!(outcome.detail, format!("Both were at {}.", &topic[..7]));
        assert!(!exists(&f, "refs/heads/topic") && !exists(&f, "refs/remotes/origin/topic"));
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\n"));
        let labels: Vec<_> = outcome.actions.iter().map(|(label, _)| label.as_ref()).collect();
        assert_eq!(labels, ["Restore topic", "Restore origin/topic"]);
        let view::Action::Branch(crate::branch::Action::Restore { .. }) = &outcome.actions[0].1 else {
            panic!("{:?}", outcome.actions[0]);
        };
        let (f, _, topic, _) = published(&place(dir.path(), "again"));
        delete_remote(&f.repo, &doomed(&f, Some("topic")), false).unwrap();
        assert_eq!(rev(&f, "refs/heads/topic"), topic);
        let (f, main, topic, url) = published(&place(dir.path(), "moved"));
        let seen = doomed(&f, Some("topic"));
        f.git(&["update-ref", "refs/heads/topic", &main]);
        let err = delete_remote(&f.repo, &seen, true).unwrap_err();
        assert!(err.text.contains("topic changed since you chose it"), "{}", err.text);
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {topic}\n"));
    }

    fn place(dir: &Path, name: &str) -> std::path::PathBuf {
        let place = dir.join(name);
        fs::create_dir_all(&place).unwrap();
        place
    }

    #[test]
    fn a_remote_s_refusal_reads_as_a_sentence_and_keeps_git_s_words() {
        let dir = tempfile::tempdir().unwrap();
        let (f, main, topic, url) = published(dir.path());
        let hooks = Path::new(&url).join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-receive");
        fs::write(&hook, "#!/bin/sh\necho 'GH006: Protected branch update failed for refs/heads/topic.' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let err = delete_remote(&f.repo, &doomed(&f, None), false).unwrap_err();
        assert_eq!(err.text, "Delete Failed — origin protects topic, so it cannot be deleted there.");
        assert!(err.output.as_deref().is_some_and(|output| output.contains("GH006")), "{:?}", err.output);
        fs::remove_file(&hook).unwrap();
        assert_eq!(remote_heads(&f, &url), format!("refs/heads/main {main}\nrefs/heads/topic {topic}\n"));
        let main_branch = Doomed {
            remote_branch: "origin/main".into(),
            branch: "main".into(),
            tip: main.clone(),
            ..doomed(&f, None)
        };
        let err = delete_remote(&f.repo, &main_branch, false).unwrap_err();
        assert_eq!(err.text, "Delete Failed — main is the default branch of origin, which refuses to delete it.");
        assert!(err.output.is_some());
        assert_eq!(rev(&f, "refs/remotes/origin/main"), main);
    }

    #[test]
    fn counting_what_a_remote_delete_takes_out_of_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let (f, _, topic, _) = published(dir.path());
        let alone = Revisions::reachable(&topic).only_on_remote("origin/topic");
        assert_eq!(f.repo.count(&alone).unwrap(), 0);
        let both = Revisions::reachable(&topic).and(&topic).only_on("topic").only_on_remote("origin/topic");
        assert_eq!(f.repo.count(&both).unwrap(), 1);
        f.git(&["branch", "-D", "topic"]);
        assert_eq!(f.repo.count(&alone).unwrap(), 1);
        let subjects: Vec<_> = f.repo.subjects(&alone, LISTED).unwrap().into_iter().map(|c| c.subject).collect();
        assert_eq!(subjects, ["Mark the ford"]);
        f.git(&["tag", "ford", &topic]);
        assert_eq!(f.repo.count(&alone).unwrap(), 0);
    }

    #[test]
    fn the_remote_delete_prompt_is_red_and_offers_the_local_branch_too() {
        let mut doomed = Doomed {
            remote_branch: "origin/topic".into(),
            remote: "origin".into(),
            branch: "topic".into(),
            tip: "a".repeat(40),
            only: 2,
            subjects: vec![("abc0001".into(), "Mark the ford".into())],
            local: Some(Follower { name: "topic".into(), tip: "a".repeat(40), only: 3, current: false, ticked: false }),
        };
        let spec = delete_spec(&doomed);
        assert!(spec.danger);
        assert_eq!(spec.title, "Delete origin/topic?");
        assert_eq!(spec.confirm.as_ref(), "Delete origin/topic");
        assert_eq!(
            spec.lines,
            [
                "This removes the branch for everyone who uses origin.",
                "2 commits are only on origin/topic. They will no longer appear in the graph.",
                "abc0001  Mark the ford",
                "and 1 more commit",
                "It is deleted only while origin still has it at aaaaaaa, where it was when you last fetched.",
                "Deleting topic too takes 1 commit more out of the graph.",
            ]
        );
        assert_eq!(spec.tick.as_ref().map(|(label, ticked)| (label.as_ref(), *ticked)), Some(("Also delete topic here", false)));
        assert_eq!(prompt::first(&spec), prompt::Stop::Cancel);
        assert!(matches!(prompt::on_return(&spec, Some(prompt::Stop::Confirm)), prompt::Action::Cancel));
        doomed.local.as_mut().unwrap().current = true;
        let spec = delete_spec(&doomed);
        assert!(spec.tick.is_none());
        assert_eq!(spec.lines.last().map(String::as_str), Some("topic tracks it and stays, because it is checked out."));
    }

    #[test]
    fn set_upstream_tracks_a_remote_branch_and_stop_tracking_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let (f, main, _, _) = published(dir.path());
        f.git(&["branch", "notes", &main]);
        f.git(&["branch", "origin/topic", &main]);
        let outcome = set_upstream(&f.repo, OsStr::new("notes"), None, Some("origin/topic")).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("notes tracks origin/topic", "It is 1 commit behind origin/topic."));
        assert_eq!(f.git(&["config", "branch.notes.remote"]), b"origin\n");
        assert_eq!(f.git(&["config", "branch.notes.merge"]), b"refs/heads/topic\n");
        assert_eq!(rev(&f, "refs/heads/notes"), main);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        let outcome = set_upstream(&f.repo, OsStr::new("notes"), Some("origin/topic"), Some("origin/main")).unwrap();
        assert_eq!(outcome.detail, "It matches origin/main.");
        let err = set_upstream(&f.repo, OsStr::new("notes"), Some("origin/topic"), None).unwrap_err();
        assert_eq!(err, "The upstream of notes changed since you chose it. Reopen the action.");
        assert_eq!(f.git(&["config", "branch.notes.merge"]), b"refs/heads/main\n");
        let outcome = set_upstream(&f.repo, OsStr::new("notes"), Some("origin/main"), None).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("notes tracks nothing", "It no longer tracks origin/main."));
        let config = String::from_utf8(f.git(&["config", "--local", "--list"])).unwrap();
        assert!(!config.contains("branch.notes."), "{config}");
        let err = set_upstream(&f.repo, OsStr::new("notes"), None, Some("origin/nowhere")).unwrap_err();
        assert!(err.contains("There is no remote branch origin/nowhere"), "{err}");
        assert!(set_upstream(&f.repo, OsStr::new("notes"), None, Some("--all")).is_err());
        assert!(set_upstream(&f.repo, OsStr::new("-D"), None, Some("origin/main")).is_err());
        f.git(&["branch", "-D", "notes"]);
        let err = set_upstream(&f.repo, OsStr::new("notes"), None, Some("origin/main")).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
    }

    #[test]
    fn the_upstream_prompt_lists_same_named_branches_and_offers_to_stop() {
        let spec = upstream_spec(OsStr::new("topic"), Some("origin/topic"), true, &["fork/topic".into(), "origin/topic".into()], "");
        assert_eq!(spec.title, "Set upstream of topic");
        assert_eq!(
            spec.lines,
            [
                "Which remote branch should topic push to and pull from?",
                "It tracks origin/topic, which no longer exists.",
                "Remote branches named topic: fork/topic and origin/topic.",
            ]
        );
        assert_eq!(spec.choices.iter().map(|c| c.label.as_ref()).collect::<Vec<_>>(), ["Stop tracking origin/topic"]);
        assert!(!spec.danger);
        assert_eq!(prompt::first(&spec), prompt::Stop::Field);
        let none = upstream_spec(OsStr::new("notes"), None, false, &[], "");
        assert_eq!(none.lines[1], "It tracks no remote branch now.");
        assert!(none.choices.is_empty());
    }

    fn branch(upstream: Option<&str>, ahead: usize, behind: usize, gone: bool) -> NamedRef {
        NamedRef {
            kind: Kind::Branch,
            name: "topic".into(),
            oid: Some("a".repeat(40)),
            object: "a".repeat(40),
            annotation: None,
            ahead,
            behind,
            upstream: upstream.map(str::to_owned),
            gone,
            time: 0,
        }
    }

    #[test]
    fn delete_both_offers_the_local_branch_it_was_chosen_for() {
        let named = |name: &str, upstream: Option<&str>| NamedRef { name: name.into(), ..branch(upstream, 0, 0, false) };
        let items = [
            named("topic", Some("origin/topic")),
            named("review", Some("origin/topic")),
            named("notes", None),
        ];
        let remotes = ["origin".to_owned()];
        let offered = |chosen: Option<&str>| {
            offered(&items, &remotes, OsStr::new("origin/topic"), chosen.map(OsStr::new))
                .map(|(item, ticked)| (item.name.to_string_lossy().into_owned(), ticked))
        };
        assert_eq!(offered(Some("review")), Some(("review".to_owned(), true)));
        assert_eq!(offered(None), Some(("topic".to_owned(), false)));
        assert_eq!(offered(Some("notes")), Some(("topic".to_owned(), false)));
        assert_eq!(offered(Some("gone")), Some(("topic".to_owned(), false)));
    }

    #[test]
    fn a_branch_says_what_it_tracks_and_its_counts_are_buttons() {
        let upstream = Some("origin/topic");
        assert_eq!(tooltip(&branch(None, 0, 0, false)), "Tracks no remote branch");
        assert_eq!(tooltip(&branch(upstream, 0, 0, false)), "Tracks origin/topic, and matches it");
        assert_eq!(tooltip(&branch(upstream, 1, 2, false)), "Tracks origin/topic. 1 commit ahead and 2 commits behind");
        assert_eq!(
            tooltip(&branch(upstream, 0, 0, true)),
            "Tracks origin/topic, which no longer exists. Set another upstream, or push to create it again"
        );
        assert!(counts(&branch(upstream, 0, 0, false), false).is_empty());
        assert!(counts(&branch(upstream, 0, 0, true), false).is_empty());
        let both = counts(&branch(upstream, 1, 2, false), false);
        assert_eq!(
            both,
            [
                ("branch.push", "↑1".to_owned(), "1 commit ahead of origin/topic. Click to push".to_owned()),
                ("branch.fast-forward", "↓2".to_owned(), "2 commits behind origin/topic. Click to fast-forward".to_owned()),
            ]
        );
        let current = counts(&branch(upstream, 0, 3, false), true);
        assert_eq!(current, [("branch.pull", "↓3".to_owned(), "3 commits behind origin/topic. Click to pull".to_owned())]);
    }
}
