use crate::{
    Tab,
    commands::Target,
    notice::{self, Failure, Outcome},
    prompt::{self, Answer, Field, Spec},
    remotes::{self, Flag, PushError, PushRequest, Rejection},
    repository::{Repository, Revisions, checked, validate_oid},
    view::{self, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, networking, operating};
    fn local<'a>(ui: &'a Ui, target: &Target) -> Option<&'a crate::refs::NamedRef> {
        let Target::Branch(name) = target else { return None };
        if ui.refs.remotes.is_empty() {
            return None;
        }
        ui.refs.local(name).filter(|item| item.oid.is_some())
    }
    &[
        Command::new(
            "branch.push-to",
            Group::Sync,
            |ui, target| match local(ui, target) {
                Some(item) => format!("Push {} to…", item.name.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| match local(ui, target) {
                Some(_) => Availability::unless(networking(ui).or_else(|| operating(ui))),
                None => Availability::Hidden,
            },
            |ui, target| Some(view::Action::PushTo(Action::Ask(local(ui, target)?.name.clone()))),
        )
        .words(&["push to another branch", "push to remote", "upload branch"])
        .icon("arrow-up"),
        Command::new(
            "branch.push-all",
            Group::Sync,
            |ui, target| match local(ui, target) {
                Some(item) => format!("Push {} to all remotes", item.name.to_string_lossy()),
                None => String::new(),
            },
            |ui, target, _| match local(ui, target) {
                Some(_) if ui.refs.remotes.len() > 1 => Availability::unless(networking(ui).or_else(|| operating(ui))),
                _ => Availability::Hidden,
            },
            |ui, target| {
                let item = local(ui, target)?;
                Some(view::Action::PushTo(Action::Everywhere {
                    branch: item.name.clone(),
                    tip: item.oid.clone()?,
                    remotes: ui.refs.remotes.clone(),
                }))
            },
        )
        .words(&["push everywhere", "push to every remote", "mirror branch"])
        .icon("arrow-up"),
    ]
};

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Ask(OsString),
    Push { branch: OsString, tip: String, remote: String, destination: OsString },
    Everywhere { branch: OsString, tip: String, remotes: Vec<String> },
}

#[derive(Debug)]
pub(crate) struct Ask {
    branch: OsString,
    tip: String,
    remote: String,
    destination: String,
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    let shown = ask.branch.to_string_lossy().into_owned();
    let mut spec = Spec::new(format!("Push {shown} to…"), "Push")
        .line(format!("Choose a remote and the branch there that {shown} should update."))
        .line("The push never replaces commits on the remote. If the branch there has commits that this one lacks, it stops.");
    spec.field = Some(Field {
        label: "Remote",
        placeholder: "origin",
        initial: ask.remote.clone(),
        masked: false,
        validate: Box::new(|text, ui| remote_problem(text, &ui.refs.remotes)),
    });
    spec.second = Some(Field {
        label: "Branch on the remote",
        placeholder: "main",
        initial: ask.destination.clone(),
        masked: false,
        validate: Box::new(|text, _| name_problem(text)),
    });
    spec
}

pub(crate) fn remote_problem(text: &str, remotes: &[String]) -> Option<String> {
    if text.is_empty() {
        return Some(String::new());
    }
    if remotes.iter().any(|remote| remote == text) {
        return None;
    }
    Some(match remotes {
        [] => "This repository has no remote. Add one first.".to_owned(),
        _ => format!("There is no remote {text}. Choose {}.", either(remotes)),
    })
}

pub(crate) fn name_problem(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some(String::new());
    }
    let bad = text.starts_with(['-', '/', '.'])
        || text.ends_with(['/', '.'])
        || text.ends_with(".lock")
        || text == "HEAD"
        || text.contains("..")
        || text.contains("//")
        || text.contains("@{")
        || text.contains("/.")
        || text.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    bad.then(|| format!("{text} is not a name Git allows for a branch."))
}

fn either(remotes: &[String]) -> String {
    match remotes {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn sentence_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    let (remote, destination) = (answer.text.trim().to_owned(), answer.second.trim().to_owned());
    if remote_problem(&remote, &tab.ui.refs.remotes).is_some() || name_problem(&destination).is_some() {
        return;
    }
    push(tab, ask.branch, ask.tip, remote, destination.into(), cx);
}

fn push(tab: &mut Tab, branch: OsString, tip: String, remote: String, destination: OsString, cx: &mut Context<Tab>) {
    tab.run_network(
        cx,
        crate::jobs::Spec::push(Some(remote.clone())),
        "Pushing the branch…",
        "Pushed Successfully",
        false,
        move |repo| {
            send(&repo, &branch, &tip, &remote, &destination)
                .map(|sent| sent.outcome(&branch, &remote, &destination))
                .map_err(|why| Failure::from(format!("Push Failed — {why}")))
        },
        |_, _| {},
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() || tab.repo.is_none() {
        return;
    }
    match action {
        Action::Ask(branch) => {
            let Some(item) = tab.ui.refs.local(&branch) else { return };
            let Some(tip) = item.oid.clone() else { return };
            let upstream = item
                .upstream
                .as_deref()
                .and_then(|upstream| crate::branch::split_remote(&tab.ui.refs.remotes, OsStr::new(upstream)));
            let (remote, destination) = match upstream {
                Some((remote, there)) => (remote.to_owned(), there.to_string_lossy().into_owned()),
                None => (crate::tag::home_remote(&tab.ui).unwrap_or_default(), branch.to_string_lossy().into_owned()),
            };
            tab.ui.menu = None;
            let ask = Ask { branch, tip, remote, destination };
            prompt::ask(&mut tab.ui, prompt::Kind::PushTo(ask), cx);
        }
        Action::Push { branch, tip, remote, destination } => push(tab, branch, tip, remote, destination, cx),
        Action::Everywhere { branch, tip, remotes } => {
            if remotes.is_empty() {
                return;
            }
            tab.run_network(
                cx,
                crate::jobs::Spec::push((remotes.len() == 1).then(|| remotes[0].clone())),
                "Pushing the branch to every remote…",
                "Pushed Successfully",
                false,
                move |repo| everywhere(&repo, &branch, &tip, &remotes),
                |_, _| {},
            );
        }
    }
    cx.notify();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Created,
    Already,
    Moved(usize),
}

impl Sent {
    fn outcome(self, branch: &OsStr, remote: &str, destination: &OsStr) -> Outcome {
        let (shown, there) = (branch.to_string_lossy(), format!("{remote}/{}", destination.to_string_lossy()));
        match self {
            Sent::Created => Outcome::titled(format!("Pushed {shown}"), format!("Created {there}.")),
            Sent::Already => Outcome::titled(format!("{there} is up to date"), format!("It already has {shown}.")),
            Sent::Moved(count) => Outcome::titled(format!("Pushed {shown}"), format!("{} to {there}.", notice::commits(count))),
        }
    }
}

fn plain(name: &OsStr) -> Result<(), String> {
    if name.is_empty() || name.as_bytes()[0] == b'-' {
        return Err("Select a branch by its name.".into());
    }
    Ok(())
}

fn expect_remote(repo: &Repository, remote: &str) -> Result<(), String> {
    let names = checked(repo.read_command().arg("remote"))?;
    if !String::from_utf8_lossy(&names).lines().any(|name| name == remote) {
        return Err(format!("The remote {remote} no longer exists. Reopen the action."));
    }
    Ok(())
}

pub(crate) fn send(repo: &Repository, branch: &OsStr, tip: &str, remote: &str, destination: &OsStr) -> Result<Sent, String> {
    plain(branch)?;
    validate_oid(tip)?;
    remotes::validate_name(remote)?;
    let there = destination.to_string_lossy().into_owned();
    if name_problem(&there).is_some() {
        return Err(format!("{there} is not a name Git allows for a branch."));
    }
    let mut source = OsString::from("refs/heads/");
    source.push(branch);
    let mut target = OsString::from("refs/heads/");
    target.push(destination);
    checked(repo.read_command().arg("check-ref-format").arg(&target))
        .map_err(|_| format!("{there} is not a name Git allows for a branch."))?;
    let mut refspec = source.clone();
    refspec.push(":");
    refspec.push(&target);
    let mut tracking = OsString::from(format!("refs/remotes/{remote}/"));
    tracking.push(destination);
    let before = repo.resolve(&tracking).ok();
    let request = PushRequest { remote: remote.into(), refspecs: vec![refspec], lease: None, set_upstream: false };
    let shown = branch.to_string_lossy();
    let report = remotes::push(repo, &request, |repo| {
        expect_remote(repo, remote)?;
        repo.expect_ref(&source, tip)
    });
    let report = match report {
        Ok(report) => report,
        Err(PushError::Refused(report)) => {
            return Err(match report.rejection().map(|(_, why)| why) {
                Some(Rejection::NonFastForward | Rejection::FetchFirst) => format!(
                    "{remote}/{there} has commits that {shown} does not. Fetch, then merge or rebase {shown} before pushing."
                ),
                Some(Rejection::Remote(why)) => format!("{remote} refused {remote}/{there}: {why}."),
                _ => String::from(PushError::Refused(report)),
            });
        }
        Err(PushError::Failed(message)) => return Err(message),
    };
    match report.refs.iter().find(|pushed| pushed.destination == target).map(|pushed| pushed.flag) {
        Some(Flag::UpToDate) => Ok(Sent::Already),
        Some(Flag::Created) => Ok(Sent::Created),
        Some(_) => {
            let count = match &before {
                Some(old) => repo.count(&Revisions::reachable(tip).not(old.as_str())).unwrap_or(0),
                None => 0,
            };
            Ok(Sent::Moved(count))
        }
        None => Err(format!("Git did not say whether {remote} took {shown}.")),
    }
}

pub(crate) fn everywhere(repo: &Repository, branch: &OsStr, tip: &str, remotes: &[String]) -> Result<Outcome, Failure> {
    let shown = branch.to_string_lossy().into_owned();
    let there = |remote: &str| format!("{remote}/{shown}");
    let (mut created, mut moved, mut already, mut failed) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for remote in remotes {
        match send(repo, branch, tip, remote, branch) {
            Ok(Sent::Created) => created.push(there(remote)),
            Ok(Sent::Moved(count)) => moved.push(format!("{} to {}", notice::commits(count), there(remote))),
            Ok(Sent::Already) => already.push(there(remote)),
            Err(why) => failed.push(format!("{remote}: {why}")),
        }
    }
    let mut said = Vec::new();
    if !created.is_empty() {
        said.push(format!("Created {}.", sentence_list(&created)));
    }
    if !moved.is_empty() {
        said.push(format!("Pushed {}.", sentence_list(&moved)));
    }
    match already.len() {
        0 => {}
        1 => said.push(format!("{} was already up to date.", already[0])),
        _ => said.push(format!("{} were already up to date.", sentence_list(&already))),
    }
    if failed.is_empty() {
        return Ok(Outcome::titled(format!("Pushed {shown} to all remotes"), said.join(" ")));
    }
    let mut text = failed.join("\n");
    if !said.is_empty() {
        text = format!("{text}\n{}", said.join(" "));
    }
    Err(Failure::from(format!("Push Failed — {text}")).worded())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{fs, path::Path};

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().to_owned()
    }

    fn remote_tip(f: &Fixture, bare: &str, name: &str) -> Option<String> {
        let listed = String::from_utf8(f.git(&["ls-remote", "--", bare, name])).unwrap();
        listed.lines().find_map(|line| line.split_once('\t').filter(|(_, at)| *at == name).map(|(oid, _)| oid.to_owned()))
    }

    fn two_remotes(dir: &Path) -> (Fixture, String, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        f.git(&["commit", "-qm", "Start trail"]);
        let origin = dir.join("origin.git").to_str().unwrap().to_owned();
        let fork = dir.join("fork.git").to_str().unwrap().to_owned();
        for (name, url) in [("origin", &origin), ("fork", &fork)] {
            f.git(&["init", "-q", "--bare", "-b", "main", url]);
            f.git(&["remote", "add", name, url]);
        }
        f.git(&["push", "-q", "-u", "origin", "main"]);
        f.git(&["switch", "-qc", "ridge"]);
        fs::write(f.dir.join("ridge.txt"), "Climb\n").unwrap();
        f.git(&["add", "--", "ridge.txt"]);
        f.git(&["commit", "-qm", "Climb the ridge"]);
        f.git(&["switch", "-q", "main"]);
        let tip = rev(&f, "ridge");
        (f, origin, fork, tip)
    }

    #[test]
    fn pushing_a_branch_to_any_remote_branch_creates_then_moves_it_and_leaves_the_checkout_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (f, origin, _, tip) = two_remotes(dir.path());
        let index = f.git(&["ls-files", "--stage"]);
        let sent = send(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("review/ridge")).unwrap();
        assert_eq!(sent, Sent::Created);
        assert_eq!(remote_tip(&f, &origin, "refs/heads/review/ridge").as_deref(), Some(tip.as_str()));
        assert_eq!(rev(&f, "refs/remotes/origin/review/ridge"), tip);
        assert_eq!(sent.outcome(OsStr::new("ridge"), "origin", OsStr::new("review/ridge")).detail, "Created origin/review/ridge.");
        let again = send(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("review/ridge")).unwrap();
        assert_eq!(again, Sent::Already);
        let main = rev(&f, "main");
        let moved = send(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("main")).unwrap();
        assert_eq!(moved, Sent::Moved(1));
        assert_eq!(remote_tip(&f, &origin, "refs/heads/main").as_deref(), Some(tip.as_str()));
        assert_eq!(rev(&f, "main"), main);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        let config = String::from_utf8(f.git(&["config", "--local", "--list"])).unwrap();
        assert!(!config.contains("branch.ridge."), "{config}");
    }

    #[test]
    fn a_push_refuses_a_moved_branch_a_removed_remote_a_bad_name_and_never_replaces_remote_commits() {
        let dir = tempfile::tempdir().unwrap();
        let (f, origin, fork, tip) = two_remotes(dir.path());
        let base = rev(&f, "main");
        let err = send(&f.repo, OsStr::new("ridge"), &base, "origin", OsStr::new("ridge")).unwrap_err();
        assert!(err.contains("changed") || err.contains("Refresh"), "{err}");
        assert_eq!(remote_tip(&f, &origin, "refs/heads/ridge"), None);
        f.git(&["remote", "remove", "fork"]);
        let err = send(&f.repo, OsStr::new("ridge"), &tip, "fork", OsStr::new("ridge")).unwrap_err();
        assert!(err.contains("no longer exists") || err.contains("fork"), "{err}");
        assert_eq!(remote_tip(&f, &fork, "refs/heads/ridge"), None);
        for bad in ["-f", "a..b", "with space", "end.lock", "HEAD"] {
            assert!(send(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new(bad)).is_err(), "{bad}");
        }
        assert!(send(&f.repo, OsStr::new("-D"), &tip, "origin", OsStr::new("ridge")).is_err());
        send(&f.repo, OsStr::new("ridge"), &tip, "origin", OsStr::new("ridge")).unwrap();
        let err = send(&f.repo, OsStr::new("main"), &base, "origin", OsStr::new("ridge")).unwrap_err();
        assert_eq!(err, "origin/ridge has commits that main does not. Fetch, then merge or rebase main before pushing.");
        assert_eq!(remote_tip(&f, &origin, "refs/heads/ridge").as_deref(), Some(tip.as_str()));
    }

    #[test]
    fn pushing_to_all_remotes_reports_each_remote_in_one_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let (f, origin, fork, tip) = two_remotes(dir.path());
        let remotes = vec!["origin".to_owned(), "fork".to_owned()];
        let outcome = everywhere(&f.repo, OsStr::new("ridge"), &tip, &remotes).unwrap();
        assert_eq!(outcome.title, "Pushed ridge to all remotes");
        assert_eq!(outcome.detail, "Created origin/ridge and fork/ridge.");
        assert_eq!(remote_tip(&f, &origin, "refs/heads/ridge").as_deref(), Some(tip.as_str()));
        assert_eq!(remote_tip(&f, &fork, "refs/heads/ridge").as_deref(), Some(tip.as_str()));
        let outcome = everywhere(&f.repo, OsStr::new("ridge"), &tip, &remotes).unwrap();
        assert_eq!(outcome.detail, "origin/ridge and fork/ridge were already up to date.");
        let base = rev(&f, "main");
        let failure = everywhere(&f.repo, OsStr::new("main"), &base, &["origin".to_owned(), "nowhere".to_owned()]).unwrap_err();
        assert!(failure.text.contains("nowhere:"), "{}", failure.text);
        assert!(failure.text.ends_with("\norigin/main was already up to date."), "{}", failure.text);
    }

    #[test]
    fn the_prompt_checks_the_remote_and_the_branch_name() {
        let remotes = ["origin".to_owned(), "fork".to_owned()];
        assert_eq!(remote_problem("origin", &remotes), None);
        assert_eq!(remote_problem("", &remotes), Some(String::new()));
        assert_eq!(remote_problem("upstream", &remotes).as_deref(), Some("There is no remote upstream. Choose origin or fork."));
        assert_eq!(name_problem("feature/map"), None);
        assert_eq!(name_problem(""), Some(String::new()));
        for bad in ["-x", "a b", "a..b", "x.lock", "a/", "HEAD", "a~1", "a:b"] {
            assert!(name_problem(bad).is_some(), "{bad}");
        }
    }
}
