use crate::repository::Run;
use super::{Kind, Snapshot, snapshot, stages};
use crate::{
    notice::{self, Failure, Outcome},
    repository::{Repository, checked, validate_oid},
};
use std::{
    ffi::OsString,
    fs,
    process::{Command, Output, Stdio},
};

type Result<T> = std::result::Result<T, Failure>;

fn quiet(mut command: Command) -> Command {
    command
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null());
    command
}

fn run(command: &mut Command) -> Result<Output> {
    crate::repository::run(command).map_err(|e| Failure::from(format!("Run bundled Git: {e}")))
}

fn printed(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.trim().to_owned()
}

fn failed(output: &Output) -> Failure {
    let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let text = if text.is_empty() { printed(output) } else { text };
    let text = if text.is_empty() { format!("Git exited with {}", output.status) } else { text };
    Failure::from(text).output(printed(output))
}

fn verb(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Merge => "merge",
        Kind::Rebase => "rebase",
        Kind::CherryPick => "cherry-pick",
        Kind::Revert => "revert",
        Kind::Patches => "am",
        Kind::Applied | Kind::Bisect => return None,
    })
}

fn pending(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::CherryPick => Some("CHERRY_PICK_HEAD"),
        Kind::Revert => Some("REVERT_HEAD"),
        _ => None,
    }
}

fn expect(repo: &Repository, shown: &Snapshot) -> Result<()> {
    repo.expect_operation(&shown.id).map_err(Failure::from)
}

fn needed(message: Option<&str>) -> Result<&str> {
    message
        .filter(|message| !message.trim().is_empty())
        .ok_or_else(|| Failure::from("Write a message for the commit first.".to_owned()))
}

struct MessageFile {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl MessageFile {
    fn new(message: &str) -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("fastkraken-message")
            .tempdir()
            .map_err(|e| Failure::from(format!("Write the commit message: {e}")))?;
        let path = dir.path().join("MESSAGE");
        fs::write(&path, message).map_err(|e| Failure::from(format!("Write the commit message: {e}")))?;
        Ok(Self { _dir: dir, path })
    }

    fn editor(&self) -> Result<String> {
        let path = self.path.to_str().filter(|path| !path.contains('\''));
        let Some(path) = path else {
            return Err(Failure::from("The folder for temporary files has a name Git cannot be given.".to_owned()));
        };
        Ok(format!(
            "f='{path}'; sh -c 'if test -e \"$0\"; then mv -f \"$0\" \"$1\"; \
             else sed -e \"/ -\\{{24\\}} >8 -\\{{24\\}}\\$/,\\$d\" \"$1\" | git stripspace --strip-comments >\"$1.fastkraken\" \
             && mv -f \"$1.fastkraken\" \"$1\"; fi' \"$f\""
        ))
    }
}

fn commit_with(repo: &Repository, message: &str, allow_empty: bool) -> Result<()> {
    let file = MessageFile::new(message)?;
    let mut command = quiet(repo.command());
    command.args(["commit", "--cleanup=whitespace"]);
    if allow_empty {
        command.arg("--allow-empty");
    }
    command.arg("-F").arg(&file.path);
    let output = run(&mut command)?;
    if output.status.success() { Ok(()) } else { Err(failed(&output)) }
}

fn step(repo: &Repository, kind: Kind, option: &str, editor: Option<&str>) -> Result<Output> {
    let verb = verb(kind).ok_or_else(|| Failure::from("There is nothing to continue.".to_owned()))?;
    let mut command = quiet(repo.command());
    if let Some(editor) = editor {
        command.env("GIT_EDITOR", editor).args(["-c", "commit.cleanup=whitespace"]);
    }
    command.args([verb, option]);
    run(&mut command)
}

fn short_head(repo: &Repository) -> String {
    checked(repo.read_command().args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]))
        .map(|oid| notice::short(String::from_utf8_lossy(&oid).trim()).to_owned())
        .unwrap_or_default()
}

pub(crate) fn offer_force_push(repo: &Repository, outcome: Outcome) -> Outcome {
    let Ok(Some(full)) = super::branch(repo) else {
        return outcome;
    };
    let Some(name) = full.strip_prefix(b"refs/heads/") else {
        return outcome;
    };
    use std::os::unix::ffi::OsStrExt;
    let Ok(listed) = checked(
        repo.read_command()
            .args(["for-each-ref", "--format=%(refname)%00%(upstream)", "--"])
            .arg(std::ffi::OsStr::from_bytes(&full)),
    ) else {
        return outcome;
    };
    let upstream = listed.split(|&b| b == b'\n').find_map(|line| {
        let (refname, upstream) = line.split_at(line.iter().position(|&b| b == 0)?);
        (refname == full.as_slice()).then(|| upstream[1..].to_vec())
    });
    let Some(upstream) = upstream.filter(|upstream| upstream.starts_with(b"refs/remotes/")) else {
        return outcome;
    };
    let mut upstream = OsString::from(std::ffi::OsStr::from_bytes(&upstream));
    upstream.push("^{commit}");
    let Ok(oid) = checked(repo.read_command().args(["rev-parse", "--verify", "--quiet", "--end-of-options"]).arg(&upstream))
    else {
        return outcome;
    };
    let oid = String::from_utf8_lossy(&oid).trim().to_owned();
    if validate_oid(&oid).is_err() {
        return outcome;
    }
    let contained = repo
        .read_command()
        .args(["merge-base", "--is-ancestor", &oid, "HEAD"])
        .run()
        .map(|output| output.status)
        .is_ok_and(|status| status.success());
    if contained {
        return outcome;
    }
    let name = OsString::from(std::ffi::OsStr::from_bytes(name));
    outcome.offer(
        "Force push…",
        crate::view::Action::Sync(crate::sync::Action::ForcePush { name, checked_out: true }),
    )
}

fn reported(repo: &Repository, shown: &Snapshot, output: Option<&Output>, done: impl FnOnce() -> Outcome) -> Result<Outcome> {
    let ok = output.is_none_or(|output| output.status.success());
    let now = snapshot(repo).map_err(Failure::from)?;
    match now {
        Some(now) if now.kind == shown.kind => {
            let at = now.step.map(|(at, of)| format!(" at commit {at} of {of}")).unwrap_or_default();
            let noun = shown.kind.noun();
            let mut failure = if !now.conflicts.is_empty() {
                let files = match now.conflicts.len() {
                    1 => "1 file needs".to_owned(),
                    n => format!("{n} files need"),
                };
                Failure::from(format!(
                    "The {noun} stopped on a conflict{at}. {files} resolving before it can go on."
                ))
            } else if now.empty {
                let commit = now.commit.as_ref().map_or("The next commit", |c| c.label.as_str());
                Failure::from(format!("{commit} is now empty. Skip it, or keep it as an empty commit."))
            } else if !ok {
                return Err(failed(output.expect("a failed run has its output")));
            } else {
                let title = format!("The {noun} stopped{at}");
                return Ok(Outcome::titled(title, "Git stopped where the list of commits asks it to."));
            };
            if let Some(output) = output {
                failure = failure.output(printed(output));
            }
            Err(failure)
        }
        _ if !ok => Err(failed(output.expect("a failed run has its output"))),
        _ => Ok(done()),
    }
}

fn finished(repo: &Repository, shown: &Snapshot, what: Option<String>) -> Outcome {
    let detail = what.unwrap_or_default();
    match shown.kind {
        Kind::Merge => Outcome::titled("Merge committed", detail),
        Kind::Rebase => {
            let title = format!("Rebased {} onto {}", shown.branch, shown.target());
            offer_force_push(repo, Outcome::titled(title, detail))
        }
        Kind::CherryPick if !shown.queued && detail.is_empty() => match &shown.commit {
            Some(picked) => Outcome::titled(
                format!("Cherry-picked {} as {}", notice::short(&picked.oid), short_head(repo)),
                format!("{} now has the changes from {}.", shown.branch, picked.label),
            ),
            None => Outcome::titled(format!("Cherry-picked onto {}", shown.branch), format!("{} is at {}.", shown.branch, short_head(repo))),
        },
        Kind::CherryPick => {
            let at = short_head(repo);
            let detail = if detail.is_empty() { format!("{} is at {at}.", shown.branch) } else { detail };
            Outcome::titled(format!("Cherry-picked onto {}", shown.branch), detail)
        }
        Kind::Revert => Outcome::titled("Revert committed", detail),
        Kind::Patches => Outcome::titled(format!("Applied the patches to {}", shown.branch), detail),
        Kind::Applied | Kind::Bisect => Outcome::titled("", detail),
    }
}

pub(crate) fn finish(repo: &Repository, shown: &Snapshot, message: Option<String>) -> Result<Outcome> {
    expect(repo, shown)?;
    if !stages(repo, "--unmerged").map_err(Failure::from)?.is_empty() {
        return Err(Failure::from("Resolve every conflicted file first.".to_owned()));
    }
    if shown.empty {
        return Err(Failure::from("This commit is now empty. Skip it, or keep it as an empty commit.".to_owned()));
    }
    let git_dir = repo.git_dir();
    let output = match shown.kind {
        Kind::Revert if shown.uncommitted => {
            quit(repo)?;
            let commit = shown.commit.as_ref().map_or_else(|| "the commit".to_owned(), |c| c.label.clone());
            return Ok(Outcome::titled(
                "Revert staged",
                format!("The changes that undo {commit} are staged on {}. Commit them when you are ready.", shown.branch),
            ));
        }
        Kind::Merge => {
            commit_with(repo, needed(message.as_deref())?, false)?;
            None
        }
        Kind::CherryPick | Kind::Revert => {
            if pending(shown.kind).is_some_and(|marker| git_dir.join(marker).exists()) {
                commit_with(repo, needed(message.as_deref())?, false)?;
            }
            if git_dir.join("sequencer").exists() {
                Some(step(repo, shown.kind, "--continue", None)?)
            } else {
                None
            }
        }
        Kind::Rebase => {
            let commits = !super::staged_nothing(repo).map_err(Failure::from)?;
            let file = match message.as_deref() {
                Some(message) if commits => Some(MessageFile::new(needed(Some(message))?)?),
                _ => None,
            };
            let editor = file.as_ref().map(MessageFile::editor).transpose()?;
            Some(step(repo, shown.kind, "--continue", editor.as_deref())?)
        }
        Kind::Patches => Some(step(repo, shown.kind, "--continue", None)?),
        Kind::Applied | Kind::Bisect => {
            return Err(Failure::from("There is nothing to continue.".to_owned()));
        }
    };
    reported(repo, shown, output.as_ref(), || finished(repo, shown, None))
}

pub(crate) fn skip(repo: &Repository, shown: &Snapshot) -> Result<Outcome> {
    expect(repo, shown)?;
    if !shown.can_skip {
        return Err(Failure::from(format!("A {} has no commit to skip.", shown.kind.noun())));
    }
    let output = step(repo, shown.kind, "--skip", None)?;
    let skipped = shown.commit.as_ref().map(|c| format!("Skipped {}.", c.label));
    reported(repo, shown, Some(&output), || {
        let label = shown.commit.as_ref().map_or_else(|| "the commit".to_owned(), |c| c.label.clone());
        match shown.kind {
            Kind::CherryPick | Kind::Revert if !shown.queued => {
                Outcome::titled(format!("Skipped {label}"), format!("{} is unchanged.", shown.branch))
            }
            _ => finished(repo, shown, skipped),
        }
    })
}

pub(crate) fn keep_empty(repo: &Repository, shown: &Snapshot, message: Option<String>) -> Result<Outcome> {
    expect(repo, shown)?;
    if shown.uncommitted {
        return Err(Failure::from("This revert was started without committing. Skip it or abort it.".to_owned()));
    }
    let marker = pending(shown.kind).filter(|marker| repo.git_dir().join(marker).exists());
    if marker.is_none() || !shown.empty {
        return Err(Failure::from("Only a cherry-pick or a revert that came out empty can be kept empty.".to_owned()));
    }
    commit_with(repo, needed(message.as_deref())?, true)?;
    let output = if repo.git_dir().join("sequencer").exists() {
        Some(step(repo, shown.kind, "--continue", None)?)
    } else {
        None
    };
    let kept = shown.commit.as_ref().map(|c| format!("Kept {} as an empty commit.", c.label));
    reported(repo, shown, output.as_ref(), || finished(repo, shown, kept))
}

pub(crate) fn quit(repo: &Repository) -> Result<()> {
    let output = run(quiet(repo.command()).args(["revert", "--quit"]))?;
    if output.status.success() { Ok(()) } else { Err(failed(&output)) }
}

pub(crate) fn quit_pick(repo: &Repository) -> Result<()> {
    let output = run(quiet(repo.command()).args(["cherry-pick", "--quit"]))?;
    if output.status.success() { Ok(()) } else { Err(failed(&output)) }
}

pub(crate) fn abort(repo: &Repository, shown: &Snapshot) -> Result<Outcome> {
    if shown.kind == Kind::Applied {
        return abort_apply(repo, shown);
    }
    expect(repo, shown)?;
    let output = step(repo, shown.kind, "--abort", None)?;
    if !output.status.success() {
        return Err(failed(&output));
    }
    let title = match shown.kind {
        Kind::Merge => "Merge aborted",
        Kind::Rebase => "Rebase aborted",
        Kind::CherryPick => "Cherry-pick aborted",
        Kind::Revert => "Revert aborted",
        Kind::Patches => "Patches aborted",
        Kind::Applied | Kind::Bisect => "",
    };
    let detail = match shown.kind {
        Kind::Merge => String::new(),
        _ => format!("{} is back at {}.", shown.branch, short_head(repo)),
    };
    Ok(Outcome::titled(title, detail))
}

fn abort_apply(repo: &Repository, shown: &Snapshot) -> Result<Outcome> {
    if let (None, Some(picked)) = (&shown.stash, &shown.picked) {
        return abort_pick(repo, shown, picked);
    }
    let Some(stash) = &shown.stash else {
        return Err(Failure::from(
            "FastKraken did not start this change, so it cannot tell what aborting would lose. Resolve each file instead."
                .to_owned(),
        ));
    };
    expect(repo, shown)?;
    let restored = repo.stash_untracked(&stash.oid).map_err(Failure::from)?;
    let output = run(quiet(repo.command()).args(["reset", "--merge"]))?;
    if !output.status.success() {
        return Err(failed(&output));
    }
    let kept = repo.remove_unchanged(&restored).map_err(|err| {
        Failure::from(format!("The stash's changes were taken out, but its untracked files could not be removed: {err}"))
    })?;
    let mut detail = format!("Your files are as they were before {} was applied, and it is still in the stash list.", stash.title);
    match kept {
        0 => {}
        1 => detail.push_str(" 1 untracked file it restored was kept, because it has changed since."),
        n => detail.push_str(&format!(" {n} untracked files it restored were kept, because they have changed since.")),
    }
    Ok(Outcome::titled("Stash apply aborted", detail))
}

fn abort_pick(repo: &Repository, shown: &Snapshot, picked: &super::Picking) -> Result<Outcome> {
    expect(repo, shown)?;
    let autostash = if picked.squash { repo.resolve(std::ffi::OsStr::new("MERGE_AUTOSTASH")).ok() } else { None };
    let output = run(quiet(repo.command()).args(["reset", "--merge"]))?;
    if !output.status.success() {
        return Err(failed(&output));
    }
    if picked.squash {
        if let Some(oid) = autostash {
            let _ = repo.pop_stash(&oid);
        }
        return Ok(Outcome::titled(
            "Squash merge aborted",
            format!("Your files are as they were before {} was squashed in. {} is unchanged.", picked.label, shown.branch),
        ));
    }
    Ok(Outcome::titled(
        "Cherry-pick aborted",
        format!("Your files are as they were before {} was picked. {} is unchanged.", picked.label, shown.branch),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{
        Id,
        tests::{commit, diverged, oid, stops, write},
    };
    use crate::repository::tests::Fixture;

    fn shown(f: &Fixture) -> Snapshot {
        snapshot(&f.repo).unwrap().expect("an operation is in progress")
    }

    fn body(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["log", "-1", "--format=%B", name])).unwrap()
    }

    fn subjects(f: &Fixture, range: &str) -> Vec<String> {
        String::from_utf8(f.git(&["log", "--format=%s", range])).unwrap().lines().map(str::to_owned).collect()
    }

    fn status(f: &Fixture) -> Vec<u8> {
        f.git(&["status", "--porcelain=v1", "--untracked-files=all"])
    }

    fn resolve(f: &Fixture, text: &str) {
        write(f, "trail.txt", text);
        f.git(&["add", "trail.txt"]);
    }

    fn stale(shown: &Snapshot) -> Snapshot {
        let mut stale = shown.clone();
        stale.id = Id::blank();
        stale
    }

    #[test]
    fn a_merge_commits_the_message_as_edited_once_every_file_is_resolved() {
        let f = diverged();
        let (main, feature) = (oid(&f, "main"), oid(&f, "feature"));
        stops(&f, &["merge", "feature"]);
        let merge = shown(&f);
        assert_eq!(merge.message.as_deref(), Some("Merge branch 'feature'"));
        assert!(!merge.empty && !merge.queued);
        let err = finish(&f.repo, &merge, merge.message.clone()).unwrap_err();
        assert_eq!(err.text, "Resolve every conflicted file first.");
        assert_eq!(oid(&f, "HEAD"), main);

        resolve(&f, "both sides\n");
        let ready = shown(&f);
        assert!(finish(&f.repo, &ready, Some("  \n".into())).unwrap_err().text.contains("Write a message"));
        assert!(finish(&f.repo, &stale(&ready), ready.message.clone()).unwrap_err().text.contains("changed"));
        assert_eq!(oid(&f, "HEAD"), main);

        let message = "Merge feature: both sides of the trail\n\n#12 Kept the ridge and the bridge.";
        let outcome = finish(&f.repo, &ready, Some(message.into())).unwrap();
        assert_eq!(outcome.title, "Merge committed");
        assert_eq!(body(&f, "HEAD"), format!("{message}\n\n"));
        assert_eq!(f.git(&["show", "-s", "--format=%P", "HEAD"]), format!("{main} {feature}\n").as_bytes());
        assert!(!f.repo.git_dir().join("MERGE_HEAD").exists());
        assert!(status(&f).is_empty());
        assert!(finish(&f.repo, &ready, Some(message.into())).unwrap_err().text.contains("changed"));
    }

    #[test]
    fn a_merge_with_no_message_file_asks_for_one() {
        let f = diverged();
        stops(&f, &["merge", "feature"]);
        resolve(&f, "both sides\n");
        fs::remove_file(f.repo.git_dir().join("MERGE_MSG")).unwrap();
        let ready = shown(&f);
        assert_eq!(ready.message.as_deref(), Some(""));
        assert!(finish(&f.repo, &ready, ready.message.clone()).unwrap_err().text.contains("Write a message"));
        assert!(f.repo.git_dir().join("MERGE_HEAD").exists());
        finish(&f.repo, &ready, Some("Merge the feature by hand".into())).unwrap();
        assert_eq!(f.git(&["log", "-1", "--format=%s"]), b"Merge the feature by hand\n");
        assert!(snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_rebase_continues_with_an_edited_message_and_keeps_its_author() {
        let f = diverged();
        let main = oid(&f, "main");
        f.git(&["switch", "-q", "feature"]);
        f.git(&["rebase", "-q", "-x", "git commit -q --amend --no-edit --author='Other <other@example.invalid>'", "HEAD~3"]);
        stops(&f, &["rebase", "main"]);
        let stopped = shown(&f);
        assert_eq!(stopped.step, Some((2, 3)));
        assert_eq!(stopped.message.as_deref(), Some("Climb the ridge"));
        resolve(&f, "both sides\n");
        let ready = shown(&f);
        assert!(skip(&f.repo, &stale(&ready)).unwrap_err().text.contains("changed"));
        let outcome = finish(&f.repo, &ready, Some("Climb the ridge over the bridge\n\n#3 by the river".into())).unwrap();
        assert_eq!(outcome.title, "Rebased feature onto main");
        assert!(outcome.actions.is_empty(), "No upstream, so nothing to force push");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(subjects(&f, "main..feature"), ["Mark the ridge", "Climb the ridge over the bridge", "Map the creek"]);
        assert_eq!(body(&f, "feature~1"), "Climb the ridge over the bridge\n\n#3 by the river\n\n");
        assert_eq!(f.git(&["merge-base", "main", "feature"]), format!("{main}\n").as_bytes());
        assert_eq!(f.git(&["log", "-1", "--format=%an", "feature~1"]), b"Other\n");
        assert!(snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_finished_rebase_offers_force_push_when_its_upstream_has_the_old_commits() {
        let f = diverged();
        let tip = oid(&f, "feature");
        for (key, value) in [
            ("remote.origin.url", "/nonexistent/origin.git"),
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("branch.feature.remote", "origin"),
            ("branch.feature.merge", "refs/heads/feature"),
        ] {
            f.git(&["config", key, value]);
        }
        f.git(&["update-ref", "refs/remotes/origin/feature", &tip]);
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "main"]);
        resolve(&f, "both sides\n");
        let outcome = finish(&f.repo, &shown(&f), Some("Climb the ridge".into())).unwrap();
        assert_eq!(outcome.title, "Rebased feature onto main");
        let offered: Vec<_> = outcome.actions.iter().map(|(label, action)| (label.to_string(), format!("{action:?}"))).collect();
        assert_eq!(offered.len(), 1, "{offered:?}");
        assert_eq!(offered[0].0, "Force push…");
        assert!(offered[0].1.contains("ForcePush") && offered[0].1.contains("\"feature\""), "{offered:?}");
    }

    fn interactive(f: &Fixture, line: usize, command: &str) {
        f.git(&["switch", "-q", "feature"]);
        let edit = format!("sed -i.orig -e '{line}s/^pick/{command}/'");
        let output = f
            .repo
            .command()
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", edit)
            .args(["rebase", "-i", "main"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "the rebase did not stop");
    }

    #[test]
    fn a_later_squash_in_the_same_rebase_keeps_gits_own_clean_up() {
        let f = diverged();
        f.git(&["config", "commit.verbose", "true"]);
        interactive(&f, 3, "squash");
        resolve(&f, "both sides\n");
        finish(&f.repo, &shown(&f), Some("Climb the ridge over the bridge".into())).unwrap();
        assert_eq!(subjects(&f, "main..feature"), ["Climb the ridge over the bridge", "Map the creek"]);
        let squashed = body(&f, "feature");
        assert!(squashed.contains("Mark the ridge"), "{squashed:?}");
        assert!(!squashed.lines().any(|line| line.starts_with('#')), "{squashed:?}");
        assert!(!squashed.contains("diff --git"), "{squashed:?}");
    }

    #[test]
    fn a_resolution_that_stages_nothing_gives_its_message_to_no_later_commit() {
        let f = diverged();
        interactive(&f, 3, "reword");
        f.git(&["checkout", "--ours", "--", "trail.txt"]);
        f.git(&["add", "trail.txt"]);
        finish(&f.repo, &shown(&f), Some("Climb the ridge over the bridge".into())).unwrap();
        assert_eq!(subjects(&f, "main..feature"), ["Mark the ridge", "Map the creek"]);
    }

    #[test]
    fn a_continued_rebase_that_stops_again_says_where() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        commit(&f, "trail.txt", "feature again\n", "Cross again");
        stops(&f, &["rebase", "main"]);
        resolve(&f, "both sides\n");
        let ready = shown(&f);
        let message = ready.message.clone();
        let err = finish(&f.repo, &ready, message).unwrap_err();
        assert_eq!(err.text, "The rebase stopped on a conflict at commit 4 of 4. 1 file needs resolving before it can go on.");
        assert!(err.output.is_some());
        let again = shown(&f);
        assert_eq!((again.kind, again.step), (Kind::Rebase, Some((4, 4))));
        assert!(abort(&f.repo, &ready).unwrap_err().text.contains("changed"));
        abort(&f.repo, &again).unwrap();
    }

    #[test]
    fn a_skipped_rebase_commit_is_gone_and_the_others_stay() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        stops(&f, &["rebase", "main"]);
        let stopped = shown(&f);
        let outcome = skip(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Rebased feature onto main");
        assert!(outcome.detail.starts_with("Skipped ") && outcome.detail.ends_with(" Climb the ridge."), "{}", outcome.detail);
        assert_eq!(subjects(&f, "main..feature"), ["Mark the ridge", "Map the creek"]);
        assert_eq!(f.git(&["show", "feature:trail.txt"]), b"main side\n");
        assert!(snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn an_aborted_rebase_puts_the_branch_index_and_files_back() {
        let f = diverged();
        f.git(&["switch", "-q", "feature"]);
        let tip = oid(&f, "feature");
        write(&f, "notes.txt", "untracked notes\n");
        stops(&f, &["rebase", "main"]);
        resolve(&f, "both sides\n");
        let stopped = shown(&f);
        assert_eq!(stopped.resolved.len(), 1);
        f.git(&["-c", "core.editor=true", "rebase", "--continue"]);
        assert!(abort(&f.repo, &stopped).unwrap_err().text.contains("changed"));
        assert!(snapshot(&f.repo).unwrap().is_none());

        f.git(&["reset", "-q", "--hard", &tip]);
        stops(&f, &["rebase", "main"]);
        resolve(&f, "both sides\n");
        let stopped = shown(&f);
        let outcome = abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Rebase aborted");
        assert_eq!(outcome.detail, format!("feature is back at {}.", &tip[..7]));
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/feature\n");
        assert_eq!(status(&f), b"?? notes.txt\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "feature side\n");
    }

    #[test]
    fn a_cherry_pick_sequence_commits_the_edited_message_and_goes_on() {
        let f = diverged();
        let main = oid(&f, "main");
        let (creek, ridge) = (oid(&f, "feature~2"), oid(&f, "feature~1"));
        stops(&f, &["cherry-pick", &ridge, &creek]);
        let stopped = shown(&f);
        assert!(stopped.queued && !stopped.empty);
        assert_eq!(stopped.message.as_deref(), Some("Climb the ridge"));
        resolve(&f, "both sides\n");
        let ready = shown(&f);
        assert!(finish(&f.repo, &stale(&ready), Some("x".into())).unwrap_err().text.contains("changed"));
        let outcome = finish(&f.repo, &ready, Some("Climb the ridge, picked".into())).unwrap();
        assert_eq!(outcome.title, "Cherry-picked onto main");
        assert_eq!(subjects(&f, &format!("{main}..main")), ["Map the creek", "Climb the ridge, picked"]);
        assert!(snapshot(&f.repo).unwrap().is_none());
        assert!(!f.repo.git_dir().join("sequencer").exists());
    }

    #[test]
    fn an_empty_pick_is_skipped_or_kept_as_an_empty_commit() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        f.git(&["switch", "-qc", "side"]);
        let side = commit(&f, "trail.txt", "same\n", "Same change");
        f.git(&["switch", "-q", "main"]);
        let main = commit(&f, "trail.txt", "same\n", "Same change here");
        stops(&f, &["cherry-pick", &side]);
        let empty = shown(&f);
        assert!(empty.empty && empty.conflicts.is_empty());
        assert!(finish(&f.repo, &empty, empty.message.clone()).unwrap_err().text.contains("now empty"));
        assert!(keep_empty(&f.repo, &stale(&empty), empty.message.clone()).unwrap_err().text.contains("changed"));
        let outcome = skip(&f.repo, &empty).unwrap();
        assert_eq!(outcome.title, format!("Skipped {} Same change", &side[..7]));
        assert_eq!(oid(&f, "HEAD"), main);
        assert!(snapshot(&f.repo).unwrap().is_none());

        stops(&f, &["cherry-pick", &side]);
        let empty = shown(&f);
        let outcome = keep_empty(&f.repo, &empty, empty.message.clone()).unwrap();
        assert_eq!(outcome.detail, format!("Kept {} Same change as an empty commit.", &side[..7]));
        assert_eq!(f.git(&["rev-parse", "HEAD~1"]), format!("{main}\n").as_bytes());
        assert_eq!(f.git(&["log", "-1", "--format=%s", "HEAD"]), b"Same change\n");
        assert!(f.git(&["diff", "--stat", "HEAD~1", "HEAD"]).is_empty());
        assert!(snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_conflicted_revert_is_committed_or_aborted_from_the_app() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "base\n", "Base");
        let second = commit(&f, "trail.txt", "second\n", "Second");
        let third = commit(&f, "trail.txt", "third\n", "Third");
        write(&f, "notes.txt", "notes\n");
        stops(&f, &["revert", "--no-edit", &second]);
        let stopped = shown(&f);
        assert!(stopped.message.as_deref().is_some_and(|m| m.starts_with("Revert \"Second\"")));
        let outcome = abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Revert aborted");
        assert_eq!(oid(&f, "HEAD"), third);
        assert_eq!(status(&f), b"?? notes.txt\n");
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "third\n");

        stops(&f, &["revert", "--no-edit", &second]);
        resolve(&f, "base and third\n");
        let ready = shown(&f);
        let outcome = finish(&f.repo, &ready, Some("Undo the second waypoint".into())).unwrap();
        assert_eq!(outcome.title, "Revert committed");
        assert_eq!(body(&f, "HEAD"), "Undo the second waypoint\n\n");
        assert!(!f.repo.git_dir().join("REVERT_HEAD").exists());
        assert!(snapshot(&f.repo).unwrap().is_none());
    }

    #[test]
    fn a_patch_series_continues_skips_and_aborts() {
        let f = diverged();
        let patches = f.dir.join(".git/patches");
        f.git(&["format-patch", "-q", "-3", "-o", patches.to_str().unwrap(), "feature"]);
        let mut files: Vec<_> = fs::read_dir(&patches).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        let mut am = vec!["am".to_owned(), "-3".to_owned()];
        am.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
        let am: Vec<&str> = am.iter().map(String::as_str).collect();
        let main = oid(&f, "main");
        stops(&f, &am);
        let stopped = shown(&f);
        assert_eq!((stopped.kind, stopped.message.as_deref()), (Kind::Patches, None));
        let outcome = abort(&f.repo, &stopped).unwrap();
        assert_eq!(outcome.title, "Patches aborted");
        assert_eq!(oid(&f, "HEAD"), main);

        stops(&f, &am);
        let outcome = skip(&f.repo, &shown(&f)).unwrap();
        assert_eq!(outcome.title, "Applied the patches to main");
        assert_eq!(subjects(&f, &format!("{main}..main")), ["Mark the ridge", "Map the creek"]);
    }
}
