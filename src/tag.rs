use crate::repository::Run;
use crate::{
    Tab,
    commands::{Entry, Target},
    notice::{self, Failure, Notice, Outcome},
    prompt::{self, Answer, Spec},
    refs::NamedRef,
    remotes::{self, Flag, PushError, PushRequest, Rejection},
    repository::{Repository, checked, validate_oid},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, busy, networking, operating};
    fn tagged<'a>(ui: &Ui, target: &'a Target) -> Option<&'a OsString> {
        match target {
            Target::Tag(name) if ui.refs.tag(name).is_some() => Some(name),
            _ => None,
        }
    }
    &[
        Command::new(
            "tag.push",
            Group::Remotes,
            |ui, target| match target {
                Target::Tag(name) => push_title(ui, name),
                _ => String::new(),
            },
            |ui, target, _| {
                if tagged(ui, target).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| operating(ui)).or_else(|| {
                    ui.refs.remotes.is_empty().then(|| "Add a remote to push the tag to".to_owned())
                }))
            },
            |ui, target| {
                let name = tagged(ui, target)?.clone();
                Some(view::Action::Tag(Action::Push { name, remotes: ui.refs.remotes.clone() }))
            },
        )
        .words(&["publish tag", "share tag", "upload"])
        .icon("arrow-up"),
        Command::new(
            "tag.annotate",
            Group::Manage,
            |_, target| match target {
                Target::Tag(name) => format!("Annotate {}…", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                match tagged(ui, target).and_then(|name| ui.refs.tag(name)) {
                    Some(item) if lightweight(item) => Availability::unless(busy(ui).or_else(|| operating(ui))),
                    _ => Availability::Hidden,
                }
            },
            |ui, target| Some(view::Action::Tag(Action::Annotate(tagged(ui, target)?.clone()))),
        )
        .words(&["annotated tag", "tag message"])
        .icon("tag"),
        Command::new(
            "tag.delete",
            Group::Manage,
            |_, target| match target {
                Target::Tag(name) => format!("Delete {}…", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                if tagged(ui, target).is_none() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)))
            },
            |ui, target| Some(view::Action::Tag(Action::Delete(tagged(ui, target)?.clone()))),
        )
        .words(&["remove tag", "delete locally"])
        .icon("x")
        .danger(),
        Command::new(
            "tag.delete-remote",
            Group::Manage,
            |ui, target| match target {
                Target::Tag(name) => format!("{}…", delete_remote_title(ui, name)),
                _ => String::new(),
            },
            |ui, target, _| {
                if tagged(ui, target).is_none() || ui.refs.remotes.is_empty() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| operating(ui)))
            },
            |ui, target| {
                let name = tagged(ui, target)?.clone();
                Some(view::Action::Tag(Action::DeleteRemote { name, remotes: ui.refs.remotes.clone() }))
            },
        )
        .words(&["remove tag from remote", "unpublish tag"])
        .icon("x")
        .danger(),
    ]
};

fn lightweight(item: &NamedRef) -> bool {
    item.annotation.is_none() && item.oid.as_deref() == Some(item.object.as_str())
}

fn push_title(ui: &Ui, name: &OsStr) -> String {
    let shown = name.to_string_lossy();
    match ui.refs.remotes.as_slice() {
        [] => format!("Push {shown} to a remote"),
        [remote] => format!("Push {shown} to {remote}"),
        _ => format!("Push {shown} to all remotes"),
    }
}

fn delete_remote_title(ui: &Ui, name: &OsStr) -> String {
    let shown = name.to_string_lossy();
    match ui.refs.remotes.as_slice() {
        [remote] => format!("Delete {shown} from {remote}"),
        _ => format!("Delete {shown} from all remotes"),
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Push { name: OsString, remotes: Vec<String> },
    Annotate(OsString),
    Delete(OsString),
    DeleteRemote { name: OsString, remotes: Vec<String> },
    Restore { name: OsString, object: String, commit: String },
}

#[derive(Debug)]
pub(crate) enum Ask {
    Annotate {
        name: OsString,
        commit: String,
    },
    Delete {
        name: OsString,
        object: String,
        commit: String,
        message: Option<String>,
        remotes: Vec<String>,
    },
    DeleteRemote {
        name: OsString,
        remotes: Vec<String>,
    },
}

pub(crate) fn describe(ask: &Ask, ui: &Ui) -> Spec {
    match ask {
        Ask::Annotate { name, commit } => {
            let shown = name.to_string_lossy();
            let mut spec = Spec::new(format!("Annotate {shown}"), "Annotate").line(format!(
                "{shown} becomes an annotated tag on the same commit, {}, with this message, your name and today's date.",
                notice::short(commit)
            ));
            spec.field = Some(prompt::Field {
                label: "Message",
                placeholder: "Tag message",
                initial: String::new(),
                masked: false,
                validate: Box::new(|text, _| text.trim().is_empty().then(String::new)),
            });
            spec
        }
        Ask::Delete { name, commit, message, remotes, .. } => {
            let shown = name.to_string_lossy();
            let mut spec = Spec::new(format!("Delete tag {shown}?"), "Delete tag")
                .line(format!("The commit it marks, {}, stays.", notice::short(commit)));
            if let Some(message) = message {
                spec = spec.line(format!("Its message goes with it: {message}"));
            }
            if !remotes.is_empty() {
                spec = spec.line(format!("{} keep any copy they have until you delete it there too.", sentence_list(remotes)));
            }
            spec
        }
        Ask::DeleteRemote { name, remotes } => {
            let shown = name.to_string_lossy();
            let (title, confirm, who) = match remotes.as_slice() {
                [remote] => (
                    format!("Delete {shown} from {remote}?"),
                    format!("Delete from {remote}"),
                    format!("This removes the tag for everyone who uses {remote}."),
                ),
                _ => (
                    format!("Delete {shown} from all remotes?"),
                    "Delete from all remotes".to_owned(),
                    format!("This removes the tag from {}, for everyone who uses them.", sentence_list(remotes)),
                ),
            };
            let mut spec = Spec::new(title, confirm).line(who);
            if ui.refs.tag(name).is_some() {
                spec = spec.line(format!("{shown} stays in this repository."));
            }
            spec.danger()
        }
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Annotate { name, commit } => tab.run_operation_then(
            cx,
            "Annotating the tag…",
            "Tag annotated",
            false,
            move |repo| annotate(&repo, &name, &commit, &answer.text),
            |_, _| {},
        ),
        Ask::Delete { name, object, commit, remotes, .. } => tab.run_operation_then(
            cx,
            "Deleting the tag…",
            "Tag deleted",
            false,
            move |repo| delete(&repo, &name, &object, &commit, &remotes),
            |_, _| {},
        ),
        Ask::DeleteRemote { name, remotes } => tab.run_network(
            cx,
            crate::jobs::Spec::push((remotes.len() == 1).then(|| remotes[0].clone())),
            "Deleting the tag from the remote…",
            "Tag deleted from the remote",
            false,
            move |repo| unpublish(&repo, &name, &remotes),
            |_, _| {},
        ),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    match action {
        Action::Push { name, remotes } => {
            let Some(object) = tab.ui.refs.tag(&name).map(|item| item.object.clone()) else { return };
            if remotes.is_empty() {
                tab.ui.notices.show(Notice::new(Tone::Info, "Add a remote to push the tag to", ""));
                cx.notify();
                return;
            }
            tab.run_network(
                cx,
                crate::jobs::Spec::push((remotes.len() == 1).then(|| remotes[0].clone())),
                "Pushing the tag…",
                "Tag pushed",
                false,
                move |repo| push(&repo, &name, &object, &remotes),
                |_, _| {},
            );
        }
        Action::Annotate(name) => {
            let Some(item) = tab.ui.refs.tag(&name).filter(|item| lightweight(item)) else { return };
            let commit = item.object.clone();
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Tag(Ask::Annotate { name, commit }), cx);
        }
        Action::Delete(name) => {
            let Some(item) = tab.ui.refs.tag(&name) else { return };
            let object = item.object.clone();
            let commit = item.oid.clone().unwrap_or_else(|| object.clone());
            let message = item.annotation.as_ref().map(|a| a.subject.clone()).filter(|s| !s.is_empty());
            let remotes = tab.ui.refs.remotes.clone();
            tab.ui.menu = None;
            let ask = Ask::Delete { name, object, commit, message, remotes };
            prompt::ask(&mut tab.ui, prompt::Kind::Tag(ask), cx);
        }
        Action::DeleteRemote { name, remotes } => {
            if remotes.is_empty() {
                return;
            }
            tab.ui.menu = None;
            prompt::ask(&mut tab.ui, prompt::Kind::Tag(Ask::DeleteRemote { name, remotes }), cx);
        }
        Action::Restore { name, object, commit } => tab.run_operation_then(
            cx,
            "Restoring the tag…",
            "Tag restored",
            false,
            move |repo| restore(&repo, &name, &object, &commit),
            |_, _| {},
        ),
    }
    cx.notify();
}

pub(crate) fn push_new(tab: &mut Tab, name: OsString, remote: String, cx: &mut Context<Tab>) {
    tab.run_network(
        cx,
        crate::jobs::Spec::push(Some(remote.clone())),
        "Pushing the tag…",
        "Tag pushed",
        false,
        move |repo| {
            let object = repo.resolve(&full(&name))?;
            push(&repo, &name, &object, &[remote])
        },
        |_, _| {},
    );
}

pub(crate) fn home_remote(ui: &Ui) -> Option<String> {
    let remotes = &ui.refs.remotes;
    let upstream = ui
        .refs
        .current(&ui.branch)
        .filter(|_| !ui.detached)
        .and_then(|branch| branch.upstream.as_deref())
        .and_then(|upstream| crate::branch::split_remote(remotes, OsStr::new(upstream)))
        .map(|(remote, _)| remote.to_owned());
    upstream
        .or_else(|| remotes.iter().find(|remote| *remote == "origin").cloned())
        .or_else(|| remotes.first().cloned())
}

pub(crate) fn tooltip(item: &NamedRef) -> String {
    let Some(annotation) = &item.annotation else {
        return match item.oid.as_deref() {
            Some(oid) => format!("Lightweight tag at {}", notice::short(oid)),
            None => "Lightweight tag".to_owned(),
        };
    };
    let mut text = "Annotated tag".to_owned();
    if !annotation.tagger.is_empty() {
        text.push_str(&format!(" by {}", annotation.tagger));
    }
    if !annotation.date.is_empty() {
        text.push_str(&format!(" on {}", annotation.date));
    }
    if !annotation.subject.is_empty() {
        text.push_str(&format!(": {}", annotation.subject));
    }
    text
}

pub(crate) fn fan_out(ui: &Ui, entries: &mut Vec<Entry>, flat: bool) {
    let remotes = &ui.refs.remotes;
    if remotes.len() < 2 {
        return;
    }
    let mut out = Vec::with_capacity(entries.len());
    for mut entry in entries.drain(..) {
        let (name, push) = match &entry.action {
            Some(view::Action::Tag(Action::Push { name, .. })) => (name.clone(), true),
            Some(view::Action::Tag(Action::DeleteRemote { name, .. })) => (name.clone(), false),
            _ => {
                out.push(entry);
                continue;
            }
        };
        let shown = name.to_string_lossy().into_owned();
        let each = |chosen: Vec<String>| {
            let name = name.clone();
            view::Action::Tag(if push {
                Action::Push { name, remotes: chosen }
            } else {
                Action::DeleteRemote { name, remotes: chosen }
            })
        };
        let choices = remotes
            .iter()
            .map(|remote| (remote.clone(), remote.clone(), vec![remote.clone()]))
            .chain([("All remotes".to_owned(), "all remotes".to_owned(), remotes.clone())]);
        let row = |entry: &Entry, title: String, action| Entry {
            id: entry.id,
            title,
            icon: if flat { entry.icon } else { None },
            group: entry.group,
            shortcut: None,
            danger: entry.danger,
            disabled: None,
            action: Some(action),
            children: Vec::new(),
            mark: None,
            check: None,
        };
        if flat {
            for (_, label, chosen) in choices {
                let title = if push { format!("Push {shown} to {label}") } else { format!("Delete {shown} from {label}…") };
                out.push(row(&entry, title, each(chosen)));
            }
        } else {
            let children = choices
                .map(|(label, _, chosen)| row(&entry, if push { label } else { format!("{label}…") }, each(chosen)))
                .collect();
            entry.title = if push { format!("Push {shown} to") } else { format!("Delete {shown} from") };
            entry.children = children;
            entry.action = None;
            out.push(entry);
        }
    }
    *entries = out;
}

fn sentence_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn full(name: &OsStr) -> OsString {
    let mut full = OsString::from("refs/tags/");
    full.push(name);
    full
}

fn plain(name: &OsStr) -> Result<(), String> {
    if name.is_empty() || name.as_bytes()[0] == b'-' {
        return Err("Select a tag by its name.".into());
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

fn expect_kept_here(repo: &Repository, remote: &str, name: &OsStr) -> Result<(), String> {
    let tag = full(name);
    let entries = repo
        .read_command()
        .args(["config", "--null", "--get-all", &format!("remote.{remote}.fetch")])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if entries.status.code() == Some(1) {
        return Ok(());
    }
    if !entries.status.success() {
        return Err(format!("Cannot read the fetch mapping of {remote}."));
    }
    for entry in entries.stdout.split(|&b| b == 0).filter(|entry| !entry.starts_with(b"^")) {
        let entry = entry.strip_prefix(b"+").unwrap_or(entry);
        let Some(colon) = entry.iter().position(|&b| b == b':') else { continue };
        let Some(matched) = remotes::refspec_match(&entry[..colon], tag.as_bytes()) else { continue };
        let configured = &entry[colon + 1..];
        let local = match configured.iter().position(|&b| b == b'*') {
            Some(i) => [&configured[..i], matched, &configured[i + 1..]].concat(),
            None => configured.to_vec(),
        };
        if local.is_empty() || local.starts_with(b"refs/remotes/") {
            continue;
        }
        if repo.resolve(OsStr::from_bytes(&local)).is_ok() {
            let shown = name.to_string_lossy();
            return Err(format!(
                "Deleting {shown} from {remote} would delete {} here too, because the fetch mapping of {remote} copies it into this repository. Review remote.{remote}.fetch first.",
                String::from_utf8_lossy(&local)
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Created,
    Already,
}

fn send(repo: &Repository, name: &OsStr, object: &str, remote: &str) -> Result<Sent, String> {
    let shown = name.to_string_lossy();
    let tag = full(name);
    let mut refspec = tag.clone();
    refspec.push(":");
    refspec.push(&tag);
    let request = PushRequest { remote: remote.into(), refspecs: vec![refspec], lease: None, set_upstream: false };
    let report = remotes::push(repo, &request, |repo| {
        expect_remote(repo, remote)?;
        repo.expect_ref(&tag, object)
    });
    match report {
        Ok(report) => match report.refs.iter().find(|pushed| pushed.destination == tag).map(|pushed| pushed.flag) {
            Some(Flag::UpToDate) => Ok(Sent::Already),
            Some(_) => Ok(Sent::Created),
            None => Err(format!("Git did not say whether {remote} took {shown}.")),
        },
        Err(PushError::Refused(report)) => Err(match report.rejection().map(|(_, why)| why) {
            Some(Rejection::AlreadyExists) => format!("{remote} already has a different {shown}."),
            Some(Rejection::Remote(why)) => format!("{remote} refused {shown}: {why}."),
            _ => String::from(PushError::Refused(report)),
        }),
        Err(PushError::Failed(message)) => Err(message),
    }
}

fn push(repo: &Repository, name: &OsStr, object: &str, remotes: &[String]) -> Result<Outcome, Failure> {
    plain(name)?;
    validate_oid(object)?;
    let (mut created, mut already, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    for remote in remotes {
        match send(repo, name, object, remote) {
            Ok(Sent::Created) => created.push(remote.clone()),
            Ok(Sent::Already) => already.push(remote.clone()),
            Err(why) => failed.push(why),
        }
    }
    let shown = name.to_string_lossy();
    let mut also = Vec::new();
    if !created.is_empty() && !failed.is_empty() {
        also.push(format!("Pushed it to {}.", sentence_list(&created)));
    }
    if !already.is_empty() && (!created.is_empty() || !failed.is_empty()) {
        also.push(format!("{} already had it.", sentence_list(&already)));
    }
    if !failed.is_empty() {
        let mut text = failed.join("\n");
        let failure = if also.is_empty() {
            Failure::from(format!("Push Failed — {text}"))
        } else {
            text = format!("{text} {}", also.join(" "));
            Failure::from(format!("Push Failed — {text}")).worded()
        };
        return Err(failure);
    }
    Ok(if created.is_empty() {
        Outcome::titled(format!("{} already has {shown}", sentence_list(&already)), "Nothing was sent.")
    } else {
        Outcome::titled(format!("Pushed {shown} to {}", sentence_list(&created)), also.join(" "))
    })
}

fn unpublish(repo: &Repository, name: &OsStr, remotes: &[String]) -> Result<Outcome, Failure> {
    plain(name)?;
    let shown = name.to_string_lossy();
    let mut deletion = OsString::from(":");
    deletion.push(full(name));
    let (mut deleted, mut failed) = (Vec::new(), Vec::new());
    for remote in remotes {
        let request =
            PushRequest { remote: remote.into(), refspecs: vec![deletion.clone()], lease: None, set_upstream: false };
        let kept = |repo: &Repository| {
            expect_remote(repo, remote)?;
            expect_kept_here(repo, remote, name)
        };
        match remotes::push(repo, &request, kept) {
            Ok(_) => deleted.push(remote.clone()),
            Err(PushError::Refused(report)) => failed.push(match report.rejection().map(|(_, why)| why) {
                Some(Rejection::Remote(why)) => format!("{remote} refused to delete {shown}: {why}."),
                _ => String::from(PushError::Refused(report)),
            }),
            Err(PushError::Failed(message)) => failed.push(message),
        }
    }
    if !failed.is_empty() {
        let text = failed.join("\n");
        if deleted.is_empty() {
            return Err(Failure::from(format!("Delete Failed — {text}")));
        }
        let text = format!("{text} Deleted it from {}.", sentence_list(&deleted));
        return Err(Failure::from(format!("Delete Failed — {text}")).worded());
    }
    Ok(Outcome::titled(format!("Deleted {shown} from {}", sentence_list(&deleted)), ""))
}

fn delete(repo: &Repository, name: &OsStr, object: &str, commit: &str, remotes: &[String]) -> Result<Outcome, String> {
    plain(name)?;
    validate_oid(object)?;
    let tag = full(name);
    repo.expect_ref(&tag, object)?;
    checked(repo.command().args(["update-ref", "--no-deref", "-d"]).arg(&tag).arg(object))?;
    let shown = name.to_string_lossy();
    let restore = Action::Restore { name: name.to_owned(), object: object.to_owned(), commit: commit.to_owned() };
    let mut outcome = Outcome::titled(format!("Deleted tag {shown}"), format!("was {}", notice::short(commit)))
        .offer("Restore", view::Action::Tag(restore));
    if !remotes.is_empty() {
        let label = match remotes {
            [remote] => format!("Delete from {remote} too"),
            _ => "Delete from all remotes too".to_owned(),
        };
        let remote = Action::DeleteRemote { name: name.to_owned(), remotes: remotes.to_vec() };
        outcome = outcome.offer(label, view::Action::Tag(remote));
    }
    Ok(outcome.sticky())
}

fn annotate(repo: &Repository, name: &OsStr, commit: &str, message: &str) -> Result<Outcome, String> {
    plain(name)?;
    validate_oid(commit)?;
    let tag = full(name);
    repo.expect_ref(&tag, commit)?;
    let message = checked_with_input(repo.read_command().arg("stripspace"), message.as_bytes())?;
    if message.is_empty() {
        return Err("Type a message for the tag.".into());
    }
    let tagger = checked(repo.read_command().args(["var", "GIT_COMMITTER_IDENT"]))?;
    let tagger = tagger.strip_suffix(b"\n").unwrap_or(&tagger);
    let mut body = format!("object {commit}\ntype commit\ntag ").into_bytes();
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(b"\ntagger ");
    body.extend_from_slice(tagger);
    body.extend_from_slice(b"\n\n");
    body.extend_from_slice(&message);
    let object = checked_with_input(repo.command().arg("mktag"), &body)?;
    let object = String::from_utf8_lossy(&object).trim_end().to_owned();
    validate_oid(&object)?;
    checked(repo.command().args(["update-ref", "--no-deref"]).arg(&tag).arg(&object).arg(commit))?;
    let shown = name.to_string_lossy();
    Ok(Outcome::titled(format!("Annotated {shown}"), format!("It still marks {}.", notice::short(commit))))
}

fn checked_with_input(command: &mut std::process::Command, input: &[u8]) -> Result<Vec<u8>, String> {
    let output = crate::repository::run_with_input(command, input.to_vec()).map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output.stdout)
}

fn restore(repo: &Repository, name: &OsStr, object: &str, commit: &str) -> Result<Outcome, String> {
    plain(name)?;
    validate_oid(object)?;
    checked(repo.command().args(["update-ref", "--no-deref"]).arg(full(name)).arg(object).arg(""))?;
    let shown = name.to_string_lossy();
    Ok(Outcome::titled(format!("Restored tag {shown}"), format!("at {}", notice::short(commit))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{fs, path::Path};

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", name])).unwrap().trim_end().to_owned()
    }

    fn released(f: &Fixture, remote: &str, dir: &Path) -> (String, String, String) {
        fs::write(f.dir.join("notes.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        f.git(&["commit", "-qm", "Start notebook"]);
        f.git(&["tag", "-a", "-m", "First release", "v1"]);
        let url = dir.join(format!("{remote}.git"));
        let url = url.to_str().unwrap().to_owned();
        f.git(&["init", "-q", "--bare", "-b", "main", &url]);
        f.git(&["remote", "add", remote, &url]);
        (rev(f, "HEAD"), rev(f, "refs/tags/v1"), url)
    }

    fn remote_tags(f: &Fixture, url: &str) -> String {
        String::from_utf8(f.git(&["--git-dir", url, "for-each-ref", "--format=%(refname) %(objecttype) %(objectname)", "refs/tags"]))
            .unwrap()
    }

    #[test]
    fn a_tag_pushes_as_its_tag_object_and_nothing_else_follows() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, object, url) = released(&f, "origin", dir.path());
        f.git(&["tag", "unasked"]);
        let outcome = push(&f.repo, OsStr::new("v1"), &object, &["origin".into()]).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Pushed v1 to origin", ""));
        assert_eq!(remote_tags(&f, &url), format!("refs/tags/v1 tag {object}\n"));
        assert_eq!(String::from_utf8(f.git(&["--git-dir", &url, "rev-parse", "v1^{commit}"])).unwrap().trim(), commit);
        let again = push(&f.repo, OsStr::new("v1"), &object, &["origin".into()]).unwrap();
        assert_eq!(again.title, "origin already has v1");
        f.git(&["tag", "light"]);
        push(&f.repo, OsStr::new("light"), &commit, &["origin".into()]).unwrap();
        assert!(remote_tags(&f, &url).contains(&format!("refs/tags/light commit {commit}\n")));
        assert!(!remote_tags(&f, &url).contains("unasked"));
    }

    #[test]
    fn a_remote_with_a_different_tag_of_that_name_keeps_it() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (_, object, url) = released(&f, "origin", dir.path());
        f.git(&["commit", "-q", "--allow-empty", "-m", "Elsewhere"]);
        f.git(&["push", "-q", "origin", "HEAD:refs/tags/v1", "HEAD:refs/heads/main"]);
        let theirs = rev(&f, "HEAD");
        let failure = push(&f.repo, OsStr::new("v1"), &object, &["origin".into()]).unwrap_err();
        assert_eq!(failure.text, "Push Failed — origin already has a different v1.");
        assert_eq!(remote_tags(&f, &url), format!("refs/tags/v1 commit {theirs}\n"));
        assert_eq!(rev(&f, "refs/tags/v1"), object);
    }

    #[test]
    fn a_push_to_every_remote_says_what_each_did() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (_, object, origin) = released(&f, "origin", dir.path());
        let mirror = dir.path().join("mirror.git");
        let mirror = mirror.to_str().unwrap();
        f.git(&["init", "-q", "--bare", "-b", "main", mirror]);
        f.git(&["remote", "add", "mirror", mirror]);
        f.git(&["push", "-q", "mirror", "refs/tags/v1"]);
        let both: Vec<String> = vec!["origin".into(), "mirror".into()];
        let outcome = push(&f.repo, OsStr::new("v1"), &object, &both).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Pushed v1 to origin", "mirror already had it."));
        assert_eq!(remote_tags(&f, &origin), remote_tags(&f, mirror));

        let outcome = unpublish(&f.repo, OsStr::new("v1"), &both).unwrap();
        assert_eq!(outcome.title, "Deleted v1 from origin and mirror");
        assert_eq!((remote_tags(&f, &origin), remote_tags(&f, mirror)), (String::new(), String::new()));
        unpublish(&f.repo, OsStr::new("v1"), &both).unwrap();
        assert_eq!(rev(&f, "refs/tags/v1"), object);
    }

    #[test]
    fn a_push_that_reached_some_remotes_says_so_when_it_is_explained() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (_, object, origin) = released(&f, "origin", dir.path());
        let missing = dir.path().join("missing.git");
        f.git(&["remote", "add", "missing", missing.to_str().unwrap()]);
        let both: Vec<String> = vec!["origin".into(), "missing".into()];
        let explained = |failure: Failure| crate::explain::explain(failure, &crate::explain::About::default()).notice();
        let notice = explained(push(&f.repo, OsStr::new("v1"), &object, &both).unwrap_err());
        assert_eq!(notice.title, "Push Failed");
        assert!(notice.detail.ends_with("Pushed it to origin."), "{}", notice.detail);
        assert!(notice.detail.contains("does not appear to be a git repository"), "{}", notice.detail);
        assert_eq!(remote_tags(&f, &origin), format!("refs/tags/v1 tag {object}\n"));
        let notice = explained(unpublish(&f.repo, OsStr::new("v1"), &both).unwrap_err());
        assert_eq!(notice.title, "Delete Failed");
        assert!(notice.detail.ends_with("Deleted it from origin."), "{}", notice.detail);
        let notice = explained(push(&f.repo, OsStr::new("v1"), &object, &["missing".into()]).unwrap_err());
        assert_eq!(notice.title, "The remote does not point at a repository you can reach");
    }

    #[test]
    fn a_push_refuses_a_tag_that_moved_or_a_remote_that_went() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, object, url) = released(&f, "origin", dir.path());
        f.git(&["tag", "-f", "-a", "-m", "Second thoughts", "v1", &commit]);
        let failure = push(&f.repo, OsStr::new("v1"), &object, &["origin".into()]).unwrap_err();
        assert_eq!(failure.text, "Push Failed — v1 changed since you chose it. Reopen the action.");
        assert_eq!(remote_tags(&f, &url), "");
        let moved = rev(&f, "refs/tags/v1");
        let failure = push(&f.repo, OsStr::new("v1"), &moved, &["gone".into()]).unwrap_err();
        assert_eq!(failure.text, "Push Failed — The remote gone no longer exists. Reopen the action.");
        let failure = unpublish(&f.repo, OsStr::new("v1"), &["gone".into()]).unwrap_err();
        assert_eq!(failure.text, "Delete Failed — The remote gone no longer exists. Reopen the action.");
        assert!(push(&f.repo, OsStr::new("-v1"), &moved, &["origin".into()]).is_err());
    }

    #[test]
    fn a_remote_delete_refuses_a_fetch_mapping_that_would_take_the_local_tag_too() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, object, url) = released(&f, "origin", dir.path());
        f.git(&["push", "-q", "origin", "refs/tags/v1"]);
        f.git(&["config", "--add", "remote.origin.fetch", "+refs/tags/*:refs/tags/*"]);
        let failure = unpublish(&f.repo, OsStr::new("v1"), &["origin".into()]).unwrap_err();
        assert_eq!(
            failure.text,
            "Delete Failed — Deleting v1 from origin would delete refs/tags/v1 here too, because the fetch mapping of origin copies it into this repository. Review remote.origin.fetch first."
        );
        assert_eq!(rev(&f, "refs/tags/v1"), object);
        assert_eq!(remote_tags(&f, &url), format!("refs/tags/v1 tag {object}\n"));
        delete(&f.repo, OsStr::new("v1"), &object, &commit, &[]).unwrap();
        unpublish(&f.repo, OsStr::new("v1"), &["origin".into()]).unwrap();
        assert_eq!(remote_tags(&f, &url), "");
        f.git(&["config", "--unset-all", "remote.origin.fetch", "tags"]);
        f.git(&["config", "--add", "remote.origin.fetch", "+refs/tags/*:refs/remotes/origin/tags/*"]);
        restore(&f.repo, OsStr::new("v1"), &object, &commit).unwrap();
        f.git(&["push", "-q", "origin", "refs/tags/v1"]);
        unpublish(&f.repo, OsStr::new("v1"), &["origin".into()]).unwrap();
        assert_eq!((rev(&f, "refs/tags/v1"), remote_tags(&f, &url)), (object, String::new()));
    }

    #[test]
    fn a_deleted_tag_comes_back_as_the_same_object_with_its_message() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, object, _) = released(&f, "origin", dir.path());
        let head = rev(&f, "HEAD");
        let outcome = delete(&f.repo, OsStr::new("v1"), &object, &commit, &["origin".into()]).unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Deleted tag v1", format!("was {}", &commit[..7]).as_str()));
        let offers: Vec<_> = outcome.actions.iter().map(|(label, _)| label.to_string()).collect();
        assert_eq!(offers, ["Restore", "Delete from origin too"]);
        assert!(outcome.sticky);
        assert_eq!(f.git(&["for-each-ref", "refs/tags"]), b"");
        assert_eq!(rev(&f, "HEAD"), head);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");

        let outcome = restore(&f.repo, OsStr::new("v1"), &object, &commit).unwrap();
        assert_eq!(outcome.title, "Restored tag v1");
        assert_eq!(rev(&f, "refs/tags/v1"), object);
        assert_eq!(f.git(&["tag", "-l", "--format=%(contents:subject)", "v1"]), b"First release\n");
        assert!(restore(&f.repo, OsStr::new("v1"), &object, &commit).is_err());
        assert_eq!(rev(&f, "refs/tags/v1"), object);
    }

    #[test]
    fn a_delete_refuses_a_tag_that_moved_or_went() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, object, _) = released(&f, "origin", dir.path());
        f.git(&["tag", "-f", "-a", "-m", "Second thoughts", "v1", &commit]);
        let moved = rev(&f, "refs/tags/v1");
        let error = delete(&f.repo, OsStr::new("v1"), &object, &commit, &[]).unwrap_err();
        assert_eq!(error, "v1 changed since you chose it. Reopen the action.");
        assert_eq!(rev(&f, "refs/tags/v1"), moved);
        f.git(&["tag", "-d", "v1"]);
        let error = delete(&f.repo, OsStr::new("v1"), &moved, &commit, &[]).unwrap_err();
        assert_eq!(error, "v1 no longer exists. Reopen the action.");
        f.git(&["branch", "v1"]);
        assert!(delete(&f.repo, OsStr::new("v1"), &commit, &commit, &[]).is_err());
        assert_eq!(rev(&f, "refs/heads/v1"), commit);
    }

    #[test]
    fn annotate_turns_a_lightweight_tag_into_an_annotated_one_on_the_same_commit() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, _, _) = released(&f, "origin", dir.path());
        f.git(&["tag", "light"]);
        let outcome = annotate(&f.repo, OsStr::new("light"), &commit, "# Field notes  \n\n\nThe ford is low.").unwrap();
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Annotated light", format!("It still marks {}.", &commit[..7]).as_str()));
        assert_eq!(f.git(&["cat-file", "-t", "refs/tags/light"]), b"tag\n");
        assert_eq!(rev(&f, "refs/tags/light^{commit}"), commit);
        assert_eq!(f.git(&["tag", "-l", "--format=%(contents)", "light"]), b"# Field notes\n\nThe ford is low.\n\n");
        assert_eq!(f.git(&["tag", "-l", "--format=%(taggername) %(*objecttype)", "light"]), b"Cedar commit\n");
        assert_eq!(f.git(&["fsck", "--no-dangling", "--strict"]), b"");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        f.git(&["tag", "bare"]);
        assert!(annotate(&f.repo, OsStr::new("bare"), &commit, " \n ").is_err());
        assert_eq!(f.git(&["cat-file", "-t", "refs/tags/bare"]), b"commit\n");
    }

    #[test]
    fn annotate_refuses_a_tag_that_moved() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, _, _) = released(&f, "origin", dir.path());
        f.git(&["tag", "light"]);
        f.git(&["commit", "-q", "--allow-empty", "-m", "Later"]);
        f.git(&["tag", "-f", "light"]);
        let later = rev(&f, "HEAD");
        let error = annotate(&f.repo, OsStr::new("light"), &commit, "Too late").unwrap_err();
        assert_eq!(error, "light changed since you chose it. Reopen the action.");
        assert_eq!(rev(&f, "refs/tags/light"), later);
        assert_eq!(f.git(&["cat-file", "-t", "refs/tags/light"]), b"commit\n");
    }

    #[test]
    fn a_lightweight_tag_says_so_and_an_annotated_one_names_its_tagger() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let (commit, _, _) = released(&f, "origin", dir.path());
        f.git(&["tag", "light"]);
        let items = crate::refs::list(&f.repo).unwrap();
        let tip = |name: &str| tooltip(items.iter().find(|item| item.name == name).unwrap());
        let today = String::from_utf8(f.git(&["tag", "-l", "--format=%(taggerdate:short)", "v1"])).unwrap();
        assert_eq!(tip("v1"), format!("Annotated tag by Cedar on {}: First release", today.trim()));
        assert_eq!(tip("light"), format!("Lightweight tag at {}", &commit[..7]));
    }

    #[test]
    fn a_list_of_remotes_reads_as_a_sentence() {
        assert_eq!(sentence_list(&["origin".into()]), "origin");
        assert_eq!(sentence_list(&["origin".into(), "fork".into(), "mirror".into()]), "origin, fork and mirror");
    }
}
