use super::{Forced, Strategy};
use crate::{
    autostash::{self, Kept, Retry},
    guard::display,
    merge::Merged,
    notice::{self, Failure},
    remotes::{self, Flag, Lease, PushError, PushReport, PushRequest, Said, Upstream, checked_branch, push_branch},
    repository::{Repository, Revisions, checked, validate_oid},
};
use crate::repository::Run;
use std::sync::Mutex;

fn upstream(repo: &Repository, branch: &str, head: &str) -> Result<Upstream, String> {
    checked_branch(repo, branch, head, true)?
        .ok_or_else(|| format!("Set an upstream branch for {branch} before synchronizing."))
}

fn upstream_of(repo: &Repository, branch: &str, head: &str) -> Result<Upstream, String> {
    checked_branch(repo, branch, head, false)?
        .ok_or_else(|| format!("Set an upstream for {branch} first."))
}

pub(super) fn fetch_all(repo: &Repository, prune: bool) -> Result<String, String> {
    let names = checked(repo.read_command().arg("remote"))?;
    if names.is_empty() {
        return Err("No remotes are configured for this repository.".into());
    }
    let names: Vec<String> = String::from_utf8_lossy(&names).lines().map(str::to_owned).collect();
    let prune = prune && prunable(repo, &names)?;
    let output = checked(repo.network_command()?.args(["fetch", "--all", "--porcelain", "--progress", "--jobs=1", pruning(prune), "--no-prune-tags"]))?;
    Ok(fetched(&output))
}

pub(super) fn fetch_remote(repo: &Repository, name: &str, prune: bool) -> Result<String, String> {
    remotes::validate_name(name)?;
    if !String::from_utf8_lossy(&checked(repo.read_command().arg("remote"))?).lines().any(|listed| listed == name) {
        return Err(format!("{name} is not a remote of this repository any more."));
    }
    let prune = prune && prunable(repo, &[name.to_owned()])?;
    let output = checked(repo.network_command()?.args(["fetch", "--porcelain", "--progress", pruning(prune), "--no-prune-tags", "--", name]))?;
    Ok(fetched(&output))
}

fn pruning(prune: bool) -> &'static str {
    if prune { "--prune" } else { "--no-prune" }
}

fn prunable(repo: &Repository, remotes: &[String]) -> Result<bool, String> {
    let output = repo
        .read_command()
        .args(["config", "--null", "--get-regexp", r"^remote\..*\.fetch$"])
        .run()
        .map_err(|e| e.to_string())?;
    match output.status.code() {
        Some(0) => {}
        Some(1) => return Ok(true),
        _ => return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
    Ok(output.stdout.split(|&b| b == 0).all(|entry| {
        let entry = String::from_utf8_lossy(entry);
        let Some((key, refspec)) = entry.split_once('\n') else { return true };
        let Some(name) = key.strip_prefix("remote.").and_then(|key| key.strip_suffix(".fetch")) else { return true };
        if !remotes.iter().any(|remote| remote == name) {
            return true;
        }
        match refspec.trim_start_matches('+').split_once(':') {
            Some((_, destination)) if !refspec.starts_with('^') && !destination.is_empty() => destination.starts_with("refs/remotes/"),
            _ => true,
        }
    }))
}

fn fetched(output: &[u8]) -> String {
    let (mut updated, mut created, mut tags, mut deleted) = (0, 0, 0, 0);
    let (mut moved_tags, mut deleted_tags) = (0, 0);
    for line in output.split(|&b| b == b'\n') {
        let (Some(&flag), Some(name)) = (line.first(), line.rsplit(|&b| b == b' ').next()) else {
            continue;
        };
        let branch = name.starts_with(b"refs/remotes/");
        let tag = name.starts_with(b"refs/tags/");
        match flag {
            b' ' | b'+' if branch => updated += 1,
            b'*' if branch => created += 1,
            b'-' if branch => deleted += 1,
            b'*' if tag => tags += 1,
            b't' | b'+' if tag => moved_tags += 1,
            b'-' if tag => deleted_tags += 1,
            _ => {}
        }
    }
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let mut parts = Vec::new();
    if updated > 0 {
        parts.push(format!("{} updated", plural(updated, "branch", "branches")));
    }
    if created > 0 {
        parts.push(if updated > 0 { format!("{created} new") } else { plural(created, "new branch", "new branches") });
    }
    if deleted > 0 {
        parts.push(format!("{} removed", plural(deleted, "branch", "branches")));
    }
    if tags > 0 {
        parts.push(plural(tags, "new tag", "new tags"));
    }
    if moved_tags > 0 {
        parts.push(format!("{} updated", plural(moved_tags, "tag", "tags")));
    }
    if deleted_tags > 0 {
        parts.push(format!("{} removed", plural(deleted_tags, "tag", "tags")));
    }
    if parts.is_empty() {
        "Everything is up to date.".to_owned()
    } else {
        format!("{}.", parts.join(", "))
    }
}

pub(super) fn push(repo: &Repository, branch: &str, head: &str) -> Result<(String, Said), PushError> {
    let target = upstream(repo, branch, head)?;
    let report = push_branch(
        repo,
        branch,
        head,
        &target.remote,
        &target.remote_ref,
        false,
        true,
    )?;
    Ok((pushed(repo, &report, &target), report.said()))
}

pub(super) fn push_named(repo: &Repository, branch: &str, head: &str) -> Result<(String, Said), PushError> {
    let target = upstream_of(repo, branch, head)?;
    let report = push_branch(repo, branch, head, &target.remote, &target.remote_ref, false, false)?;
    Ok((pushed(repo, &report, &target), report.said()))
}

#[derive(Clone, Debug)]
pub(crate) struct Fetched {
    target: Upstream,
    oid: String,
}

pub(super) fn pull_named_fetch(repo: &Repository, branch: &str, head: &str) -> Result<Fetched, String> {
    let target = upstream_of(repo, branch, head)?;
    let oid = fetch_tip(repo, &target)?;
    Ok(Fetched { target, oid })
}

pub(super) fn pull_named_integrate(repo: &Repository, branch: &str, head: &str, fetched: Fetched) -> Result<(String, String), String> {
    let Fetched { target, oid } = fetched;
    if upstream_of(repo, branch, head)? != target {
        return Err("The configured upstream changed during fetch. Refresh and try again.".into());
    }
    let source = display(&target.tracking_ref);
    Ok(match advance(repo, branch, head, &oid, &source)? {
        Some(count) => (
            format!("Pulled {branch}"),
            format!("Fast-forwarded {branch} by {} from {source}.", notice::commits(count)),
        ),
        None => (format!("{branch} is up to date"), format!("{branch} already contains {source}.")),
    })
}

#[cfg(test)]
pub(super) fn pull_named(repo: &Repository, branch: &str, head: &str) -> Result<(String, String), String> {
    let fetched = pull_named_fetch(repo, branch, head)?;
    pull_named_integrate(repo, branch, head, fetched)
}

pub(super) fn fast_forward(
    repo: &Repository,
    branch: &str,
    head: &str,
    tracking: &std::ffi::OsStr,
    to: &str,
) -> Result<(String, String), String> {
    validate_oid(to)?;
    let target = upstream_of(repo, branch, head)?;
    let source = display(tracking);
    if target.tracking_ref != tracking {
        return Err(format!("{branch} no longer tracks {source}. Refresh and try again."));
    }
    repo.expect_ref(tracking, to)?;
    Ok(match advance(repo, branch, head, to, &source)? {
        Some(count) => (
            format!("Fast-forwarded {branch}"),
            format!("{} from {source}. Now at {}.", notice::commits(count), notice::short(to)),
        ),
        None => (format!("{branch} is up to date"), format!("{branch} already contains {source}.")),
    })
}

pub(super) fn advance_to(
    repo: &Repository,
    branch: &str,
    head: &str,
    source: &crate::merge::Source,
    to: &str,
) -> Result<(String, String), String> {
    validate_oid(to)?;
    let label = source.label();
    match source {
        crate::merge::Source::Commit(oid) => {
            let mut commit = std::ffi::OsString::from(oid);
            commit.push("^{commit}");
            if oid != to || repo.resolve(&commit).ok().as_deref() != Some(to) {
                return Err("That commit is not in the repository. Refresh and try again.".into());
            }
        }
        _ => repo.expect_names_commit(&source.full(), to)?,
    }
    Ok(match advance(repo, branch, head, to, &label)? {
        Some(count) => (
            format!("Fast-forwarded {branch}"),
            format!("{} from {label}. Now at {}.", notice::commits(count), notice::short(to)),
        ),
        None => (format!("{branch} is up to date"), format!("{branch} already contains {label}.")),
    })
}

fn advance(repo: &Repository, branch: &str, head: &str, to: &str, source: &str) -> Result<Option<usize>, String> {
    validate_oid(head)?;
    validate_oid(to)?;
    if branch.is_empty() || branch.starts_with('-') {
        return Err("Select a branch by its name.".into());
    }
    if head == to || repo.is_ancestor(to, head)? {
        return Ok(None);
    }
    if !repo.is_ancestor(head, to)? {
        let own = repo.count(&Revisions::reachable(head).not(to))?;
        return Err(format!(
            "{branch} has {} that {source} does not, so it cannot be fast-forwarded. Check out {branch} to merge or rebase.",
            notice::commits(own)
        ));
    }
    let count = repo.count(&Revisions::reachable(to).not(head))?;
    let local = format!("refs/heads/{branch}");
    repo.expect_ref(std::ffi::OsStr::new(&local), head)?;
    checked(repo.command().args([
        "fetch",
        "--porcelain",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        "--no-prune",
        "--",
        ".",
        &format!("{to}:{local}"),
    ]))?;
    Ok(Some(count))
}

fn fetch_tip(repo: &Repository, target: &Upstream) -> Result<String, String> {
    let fetched = checked(
        repo.network_command()?
            .args(["fetch", "--porcelain", "--verbose", "--progress", "--"])
            .arg(&target.remote)
            .arg(&target.remote_ref),
    )?;
    let oid = fetched
        .split(|&b| b == b'\n')
        .find_map(|line| {
            let mut fields = line.rsplit(|&b| b == b' ');
            if fields.next()? != b"FETCH_HEAD" {
                return None;
            }
            std::str::from_utf8(fields.next()?).ok()
        })
        .ok_or("Git did not report the tip of the fetched branch.")?;
    validate_oid(oid)?;
    Ok(oid.to_owned())
}

fn pushed(repo: &Repository, report: &PushReport, target: &Upstream) -> String {
    let name = display(&target.tracking_ref);
    let Some(pushed) = report.refs.iter().find(|r| r.destination == target.remote_ref) else {
        return format!("Pushed to {name}.");
    };
    match pushed.flag {
        Flag::UpToDate => format!("{name} is already up to date."),
        Flag::Created => format!("Created {name}."),
        Flag::FastForward | Flag::Forced => {
            let count = pushed
                .summary
                .split_once("...")
                .or_else(|| pushed.summary.split_once(".."))
                .and_then(|(old, new)| repo.count(&Revisions::reachable(new).not(old)).ok());
            match count {
                Some(count) => format!("{} to {name}.", notice::commits(count)),
                None => format!("Pushed to {name}."),
            }
        }
        Flag::Deleted | Flag::Rejected => format!("Pushed to {name}."),
    }
}

const LISTED: usize = 5;

pub(super) fn behind_remote(repo: &Repository, branch: &str, head: &str, current: bool) -> Result<(String, usize), String> {
    let target = checked_branch(repo, branch, head, current)?
        .ok_or_else(|| format!("Set an upstream for {branch} first."))?;
    let oid = fetch_tip(repo, &target)?;
    let behind = repo.count(&Revisions::reachable(oid.as_str()).not(head))?;
    Ok((display(&target.tracking_ref), behind))
}

pub(super) fn force_preview(repo: &Repository, branch: &str, head: &str, current: bool) -> Result<Forced, String> {
    let target = checked_branch(repo, branch, head, current)?
        .ok_or_else(|| format!("Set an upstream for {branch} first."))?;
    let remote = target
        .remote
        .to_str()
        .ok_or("FastKraken can only push to a remote whose name is valid UTF-8.")?
        .to_owned();
    if remote == "." {
        return Err(format!(
            "{branch} tracks the local branch {}, so there is no remote branch to force push it to.",
            display(&target.tracking_ref)
        ));
    }
    let expect = repo
        .resolve(&target.tracking_ref)
        .map_err(|_| format!("Fetch {remote} first, so FastKraken can see what would be replaced."))?;
    let gone = Revisions::reachable(expect.as_str()).not(head);
    let replaced = repo.count(&gone)?;
    let sent = repo.count(&Revisions::reachable(head).not(expect.as_str()))?;
    let subjects = if replaced == 0 {
        Vec::new()
    } else {
        repo.subjects(&gone, LISTED)?
            .into_iter()
            .map(|commit| (notice::short(&commit.oid).to_owned(), commit.subject))
            .collect()
    };
    Ok(Forced {
        branch: branch.to_owned(),
        tip: head.to_owned(),
        current,
        remote,
        remote_ref: target.remote_ref,
        tracking_ref: target.tracking_ref,
        expect,
        replaced,
        sent,
        subjects,
    })
}

pub(super) fn force_push(repo: &Repository, forced: &Forced) -> Result<(String, bool, Said), PushError> {
    validate_oid(&forced.expect)?;
    let target = Upstream {
        remote: forced.remote.clone().into(),
        remote_ref: forced.remote_ref.clone(),
        tracking_ref: forced.tracking_ref.clone(),
    };
    let unchanged = |repo: &Repository| -> Result<(), String> {
        if checked_branch(repo, &forced.branch, &forced.tip, forced.current)?.as_ref() != Some(&target) {
            return Err(format!("The upstream of {} changed. Refresh and try again.", forced.branch));
        }
        Ok(())
    };
    unchanged(repo)?;
    let mut refspec = std::ffi::OsString::from(format!("refs/heads/{}:", forced.branch));
    refspec.push(&forced.remote_ref);
    let request = PushRequest {
        remote: target.remote.clone(),
        refspecs: vec![refspec],
        lease: Some(Lease { remote_ref: forced.remote_ref.clone(), expect: forced.expect.clone() }),
        set_upstream: false,
    };
    let report = remotes::push(repo, &request, |repo| {
        unchanged(repo)?;
        crate::tracking::expect_tracks(repo, &forced.remote, &forced.remote_ref, &forced.tracking_ref)?;
        repo.expect_ref(&forced.tracking_ref, &forced.expect)
    })?;
    let name = display(&forced.tracking_ref);
    let said = report.said();
    let Some(pushed) = report.refs.iter().find(|r| r.destination == forced.remote_ref) else {
        return Ok((format!("Pushed to {name}."), false, said));
    };
    if pushed.flag != Flag::Forced {
        return Ok((self::pushed(repo, &report, &target), false, said));
    }
    let counted = pushed.summary.split_once("...").and_then(|(old, new)| {
        let replaced = repo.count(&Revisions::reachable(old).not(new)).ok()?;
        Some((replaced, repo.count(&Revisions::reachable(new).not(old)).ok()?))
    });
    let detail = match counted {
        Some((replaced, 0)) => format!("Took {} off {name}.", notice::commits(replaced)),
        Some((replaced, sent)) => {
            format!("Replaced {} on {name} with {sent} from {}.", notice::commits(replaced), forced.branch)
        }
        None => format!("Replaced {name}."),
    };
    Ok((detail, true, said))
}

pub(super) fn put_back(
    repo: &Repository,
    remote: &str,
    remote_ref: &std::ffi::OsStr,
    tracking: &std::ffi::OsStr,
    old: &str,
    new: &str,
) -> Result<String, PushError> {
    remotes::validate_name(remote)?;
    validate_oid(old)?;
    validate_oid(new)?;
    repo.resolve(std::ffi::OsStr::new(old))?;
    let mut refspec = std::ffi::OsString::from(format!("{old}:"));
    refspec.push(remote_ref);
    let request = PushRequest {
        remote: remote.into(),
        refspecs: vec![refspec],
        lease: Some(Lease { remote_ref: remote_ref.to_owned(), expect: new.to_owned() }),
        set_upstream: false,
    };
    remotes::push(repo, &request, |repo| {
        crate::tracking::expect_remote(repo, remote)?;
        crate::tracking::expect_tracks(repo, remote, remote_ref, tracking)?;
        repo.expect_ref(tracking, new)
    })?;
    Ok(format!("It is at {} again.", notice::short(old)))
}

#[cfg(test)]
pub(super) fn pull(
    repo: &Repository,
    branch: &str,
    head: &str,
    strategy: Strategy,
    started: &Mutex<Option<(String, String)>>,
    stash: bool,
    kept: &Kept,
) -> Result<String, Failure> {
    let fetched = pull_fetch(repo, branch, head)?;
    pull_integrate(repo, branch, head, false, fetched, strategy, started, stash, kept)
}

pub(super) fn pull_fetch(repo: &Repository, branch: &str, head: &str) -> Result<Fetched, Failure> {
    repo.ensure_index_writable()?;
    let target = upstream(repo, branch, head)?;
    let oid = fetch_tip(repo, &target)?;
    Ok(Fetched { target, oid })
}

pub(super) fn fast_forward_checked_out(repo: &Repository, branch: &str, head: &str) -> Result<String, String> {
    let fetched = pull_fetch(repo, branch, head).map_err(|failure| failure.text)?;
    let oid = fetched.oid.clone();
    if oid != head && !repo.is_ancestor(&oid, head)? && !repo.is_ancestor(head, &oid)? {
        let own = repo.count(&Revisions::reachable(head).not(oid.as_str()))?;
        return Err(format!(
            "{branch} has {} that {} does not, so it was left as it is. Open the repository to merge or rebase.",
            notice::commits(own),
            display(&fetched.target.tracking_ref)
        ));
    }
    let started = Mutex::new(None);
    pull_integrate(repo, branch, head, false, fetched, Strategy::FastForwardOnly, &started, false, &Kept::default())
        .map_err(|failure| failure.text)
}

pub(super) fn moved_tip(repo: &Repository, branch: &str, head: &str) -> Result<String, String> {
    validate_oid(head)?;
    let local = format!("refs/heads/{branch}");
    let attached = checked(repo.read_command().args(["symbolic-ref", "--quiet", "HEAD"]))
        .map_err(|_| "HEAD was detached while the pull fetched. Refresh and try again.".to_owned())?;
    if attached.strip_suffix(b"\n") != Some(local.as_bytes()) {
        return Err("The checked-out branch changed while the pull fetched. Refresh and try again.".into());
    }
    let tip = repo.resolve(std::ffi::OsStr::new(&local))?;
    if tip != head && !repo.is_ancestor(head, &tip)? {
        return Err(format!("{branch} was rewritten while the pull fetched. Refresh and try again."));
    }
    Ok(tip)
}

pub(super) fn pull_integrate(
    repo: &Repository,
    branch: &str,
    head: &str,
    moved: bool,
    fetched: Fetched,
    strategy: Strategy,
    started: &Mutex<Option<(String, String)>>,
    stash: bool,
    kept: &Kept,
) -> Result<String, Failure> {
    let head = if moved { moved_tip(repo, branch, head)? } else { head.to_owned() };
    let head = head.as_str();
    let Fetched { target, oid } = fetched;
    let oid = oid.as_str();
    if upstream(repo, branch, head)? != target {
        return Err(Failure::from("The configured upstream changed during fetch. Refresh and try again.".to_owned()));
    }
    repo.ensure_index_writable()?;
    let source = display(&target.tracking_ref);
    if strategy == Strategy::Rebase {
        return pull_rebase(repo, branch, head, oid, &source, started, stash, kept);
    }
    let ff = match strategy {
        Strategy::FastForwardOnly => "--ff-only",
        _ => "--ff",
    };
    let message = format!(
        "Merge {} into {branch}",
        target.tracking_ref.to_string_lossy()
    );
    let merge = |autostash: bool| -> Result<(), Failure> {
        let merged = checked(repo.command().env("GIT_MERGE_AUTOEDIT", "no").args([
            "merge",
            "--no-edit",
            "--commit",
            "--no-squash",
            ff,
            if autostash { "--autostash" } else { "--no-autostash" },
            "-m",
            &message,
            "--",
            oid,
        ]));
        let Err(err) = merged else { return Ok(()) };
        if repo.git_dir().join("MERGE_HEAD").is_file() {
            if !checked(repo.read_command().args(["ls-files", "--unmerged", "-z"]))?.is_empty() {
                return Err(Failure::from("Pull left a merge in progress. Resolve the conflicted files in the merge panel or abort the merge.".to_owned()));
            }
            return Err(Failure::from(format!(
                "{err}. Git left a merge in progress. Open the merge panel to finish or abort it."
            )));
        }
        Err(Failure::from(err))
    };
    let retry = || Retry::Pull { head: head.to_owned(), strategy };
    let ((), stashed) = autostash::by_git(repo, stash, kept, retry, merge, |_| pulled_from(&source))?;
    let after = checked(repo.read_command().args(["rev-parse", "--verify", "HEAD^{commit}"]))?;
    let merged = Merged::between(repo, head, String::from_utf8_lossy(&after).trim_end())?;
    let said = match merged {
        Merged::Nothing => merged.sentence(branch, &source),
        _ => from(&source, &merged.sentence(branch, &source)),
    };
    Ok(restored(said, stashed))
}

fn pulled_from(source: &str) -> String {
    format!("Pulled from {source}")
}

fn restored(said: String, stashed: bool) -> String {
    match stashed {
        true => format!("{said} Your local changes were stashed first and put back."),
        false => said,
    }
}

fn from(source: &str, sentence: &str) -> String {
    let mut letters = sentence.chars();
    let first = letters.next().map(|c| c.to_lowercase().collect::<String>()).unwrap_or_default();
    format!("From {source}: {first}{}", letters.as_str())
}

fn pull_rebase(
    repo: &Repository,
    branch: &str,
    head: &str,
    oid: &str,
    source: &str,
    started: &Mutex<Option<(String, String)>>,
    stash: bool,
    kept: &Kept,
) -> Result<String, Failure> {
    validate_oid(head)?;
    validate_oid(oid)?;
    if head == oid || repo.is_ancestor(oid, head)? {
        return Ok(Merged::Nothing.sentence(branch, source));
    }
    let incoming = repo.count(&Revisions::reachable(oid).not(head))?;
    let own = repo.count(&Revisions::reachable(head).not(oid))?;
    *started.lock().unwrap_or_else(|e| e.into_inner()) = Some((oid.to_owned(), source.to_owned()));
    let retry = || Retry::Pull { head: head.to_owned(), strategy: Strategy::Rebase };
    let replay = |autostash: bool| crate::rebase::replay(repo, oid, false, None, autostash);
    let (after, stashed) = autostash::by_git(repo, stash, kept, retry, replay, |_| pulled_from(source))?;
    let replayed = repo.count(&Revisions::reachable(after.as_str()).not(oid))?;
    let at = notice::short(&after);
    Ok(restored(match (own, replayed) {
        (0, _) => from(source, &Merged::FastForward(incoming).sentence(branch, source)),
        (_, 0) => format!("From {source}: {source} already had {branch}'s changes, so {branch} moved to it at {at}."),
        (_, n) => format!(
            "From {source}: {} brought in, and {branch}'s {} replayed on top. {branch} is at {at}.",
            notice::commits(incoming),
            notice::commits(n)
        ),
    }, stashed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };
    use std::ffi::OsStr;
    use tempfile::TempDir;

    fn pull(repo: &Repository, branch: &str, head: &str, strategy: Strategy) -> Result<String, String> {
        super::pull(repo, branch, head, strategy, &Mutex::new(None), false, &Kept::default()).map_err(|failure| failure.text)
    }

    struct Network {
        local: Fixture,
        remotes: TempDir,
        bare: PathBuf,
        peer: PathBuf,
    }

    impl Network {
        fn new() -> Self {
            let local = Fixture::new();
            local.git(&["commit", "--allow-empty", "-qm", "Start notebook"]);
            let remotes = tempfile::tempdir().unwrap();
            let bare = remotes.path().join("upstream.git");
            let peer = remotes.path().join("peer");
            local.git(&[
                "init",
                "-q",
                "--bare",
                "-b",
                "published",
                bare.to_str().unwrap(),
            ]);
            local.git(&["remote", "add", "team", bare.to_str().unwrap()]);
            local.git(&[
                "config",
                "remote.team.fetch",
                "+refs/heads/*:refs/remotes/review/*",
            ]);
            local.git(&["push", "-qu", "team", "main:refs/heads/published"]);
            local.git(&[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                peer.to_str().unwrap(),
            ]);
            Self {
                local,
                remotes,
                bare,
                peer,
            }
        }

        fn at(&self, path: &Path, args: &[&str]) -> String {
            let data = checked(self.local.repo.command().current_dir(path).args(args)).unwrap();
            String::from_utf8(data).unwrap().trim_end().into()
        }

        fn peer_commit(&self) -> String {
            self.at(
                &self.peer,
                &[
                    "-c",
                    "user.name=Peer",
                    "-c",
                    "user.email=peer@example.invalid",
                    "commit",
                    "-qm",
                    "Incoming work",
                    "--allow-empty",
                ],
            );
            self.at(&self.peer, &["push", "-q", "origin", "published"]);
            self.at(&self.peer, &["rev-parse", "HEAD"])
        }

        fn head(&self) -> String {
            self.at(&self.local.dir, &["rev-parse", "HEAD"])
        }

        fn install_hook(&self, name: &str, script: &str) {
            let hooks = self.local.repo.git_dir().join("hooks");
            fs::create_dir_all(&hooks).unwrap();
            let hook = hooks.join(name);
            fs::write(&hook, script).unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            self.local
                .git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        }
    }

    #[test]
    fn push_targets_only_the_displayed_tip_and_its_configured_upstream() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        f.git(&["branch", "private"]);
        f.git(&["commit", "-qm", "Publish this tip", "--allow-empty"]);
        let tip = n.head();
        f.git(&["tag", "-a", "unpublished", "-m", "Keep local"]);
        f.git(&["config", "push.followTags", "true"]);
        f.git(&["config", "push.default", "matching"]);
        f.git(&["config", "remote.team.mirror", "true"]);
        f.git(&["config", "remote.team.push", "+refs/heads/*:refs/heads/*"]);
        f.git(&["config", "remote.pushDefault", "other"]);
        assert!(push(&f.repo, "main", &base).is_err());
        assert_eq!(push(&f.repo, "main", &tip).unwrap().0, "1 commit to review/published.");
        assert_eq!(push(&f.repo, "main", &tip).unwrap().0, "review/published is already up to date.");
        assert_eq!(
            n.at(
                &n.bare,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            format!("refs/heads/published {tip}")
        );
        assert_eq!(
            n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]),
            tip
        );

        f.git(&["switch", "-q", "private"]);
        f.git(&["reset", "--hard", "-q", &tip]);
        assert!(push(&f.repo, "main", &tip).is_err());
        assert!(
            String::from(push(&f.repo, "private", &tip).unwrap_err()).contains("upstream")
        );
        f.git(&["switch", "-q", "--detach", &tip]);
        assert!(pull(&f.repo, "main", &tip, Strategy::FastForwardIfPossible).is_err());
    }

    #[test]
    fn a_commit_that_skipped_its_hooks_is_pushed_by_the_tip_it_returned_and_only_while_that_is_the_tip() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        n.install_hook("pre-commit", "#!/bin/sh\nexit 1\n");
        fs::write(f.dir.join("notes.txt"), "pushed after committing\n").unwrap();
        f.git(&["add", "notes.txt"]);
        assert!(f.repo.commit_with("Blocked", crate::repository::Hooks::Run).is_err());
        assert_eq!(n.head(), base);
        let committed = f.repo.commit_with("Push after committing", crate::repository::Hooks::Skip).unwrap();
        let tip = committed.head.clone().unwrap();
        assert_eq!(tip, n.head());
        assert_eq!(push(&f.repo, "main", &tip).unwrap().0, "1 commit to review/published.");
        assert_eq!(n.at(&n.bare, &["rev-parse", "refs/heads/published"]), tip);
        assert_eq!(n.at(&n.bare, &["show", "published:notes.txt"]), "pushed after committing");
        fs::write(f.dir.join("notes.txt"), "moved on\n").unwrap();
        f.git(&["add", "notes.txt"]);
        f.repo.commit_with("Moved on", crate::repository::Hooks::Skip).unwrap();
        assert!(push(&f.repo, "main", &tip).is_err());
        assert_eq!(n.at(&n.bare, &["rev-parse", "refs/heads/published"]), tip);
    }

    #[test]
    fn pre_push_hook_can_reject_the_checked_out_branch_by_name() {
        let n = Network::new();
        let base = n.head();
        n.local
            .git(&["commit", "-qm", "Check branch policy", "--allow-empty"]);
        let tip = n.head();
        n.install_hook(
            "pre-push",
            r#"#!/bin/sh
while read -r local_ref local_oid remote_ref remote_oid; do
    if test "$local_ref" = refs/heads/main; then
        echo "Rejected $local_ref $local_oid -> $remote_ref $remote_oid" >&2
        exit 1
    fi
done
"#,
        );
        let err = push(&n.local.repo, "main", &tip).unwrap_err();
        assert!(!super::super::rejected(&err), "{err:?}");
        let err = String::from(err);
        assert!(err.contains(&format!(
            "Rejected refs/heads/main {tip} -> refs/heads/published {base}"
        )));
        assert_eq!(n.at(&n.bare, &["rev-parse", "refs/heads/published"]), base);
    }

    #[test]
    fn fetch_all_updates_both_remotes_without_touching_pending_work() {
        let n = Network::new();
        let f = &n.local;
        let other = n.remotes.path().join("other.git");
        f.git(&[
            "clone",
            "-q",
            "--bare",
            n.bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ]);
        f.git(&["remote", "add", "second", other.to_str().unwrap()]);
        let local = n.head();
        let incoming = n.peer_commit();
        fs::write(f.dir.join("working"), "staged\n").unwrap();
        f.git(&["add", "working"]);
        fs::write(f.dir.join("working"), "unstaged\n").unwrap();
        let before = f.git(&["ls-files", "--stage", "-z"]);
        f.git(&["config", "fetch.parallel", "2"]);
        assert_eq!(fetch_all(&f.repo, true).unwrap(), "1 branch updated, 1 new.");
        assert_eq!(fetch_all(&f.repo, true).unwrap(), "Everything is up to date.");
        assert_eq!(
            n.at(&f.dir, &["rev-parse", "refs/remotes/second/published"]),
            local
        );
        assert_eq!(
            n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]),
            incoming
        );
        assert_eq!(n.head(), local);
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), before);
        assert_eq!(fs::read(f.dir.join("working")).unwrap(), b"unstaged\n");
    }

    #[test]
    fn divergent_pull_respects_the_selected_strategy_and_push_never_forces() {
        let n = Network::new();
        let f = &n.local;
        f.git(&["commit", "-qm", "Local revision", "--allow-empty"]);
        let local = n.head();
        let incoming = n.peer_commit();
        f.git(&["config", "pull.rebase", "true"]);
        f.git(&["config", "branch.main.rebase", "true"]);
        f.git(&["config", "pull.ff", "only"]);
        let refused = push(&f.repo, "main", &local).unwrap_err();
        assert!(super::super::rejected(&refused), "{refused:?}");
        assert!(String::from(refused).contains("published: [rejected] (fetch first)"));
        assert_eq!(n.at(&n.bare, &["rev-parse", "published"]), incoming);
        assert!(pull(&f.repo, "main", &local, Strategy::FastForwardOnly).is_err());
        assert_eq!(n.head(), local);
        assert_eq!(
            n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]),
            incoming
        );
        let said = pull(&f.repo, "main", &local, Strategy::FastForwardIfPossible).unwrap();
        assert_eq!(
            n.at(&f.dir, &["show", "-s", "--format=%P", "HEAD"]),
            format!("{local} {incoming}")
        );
        let merge = n.head();
        assert_eq!(said, format!("From review/published: created merge commit {}.", &merge[..7]));
        let said = pull(&f.repo, "main", &merge, Strategy::FastForwardIfPossible).unwrap();
        assert_eq!(said, "main already contains review/published.");
        assert_eq!(n.head(), merge);
        assert_eq!(push(&f.repo, "main", &merge).unwrap().0, "2 commits to review/published.");
        n.at(&n.peer, &["pull", "-q", "--ff-only", "origin", "published"]);
        let ahead = n.peer_commit();
        let said = pull(&f.repo, "main", &merge, Strategy::FastForwardOnly).unwrap();
        assert_eq!(said, "From review/published: fast-forwarded main by 1 commit.");
        assert_eq!(n.head(), ahead);
    }

    #[test]
    fn conflicting_pull_preserves_the_merge_for_resolution_or_abort() {
        let n = Network::new();
        let f = &n.local;
        fs::write(f.dir.join("notes"), "local version\n").unwrap();
        f.git(&["add", "notes"]);
        f.git(&["commit", "-qm", "Local note"]);
        let local = n.head();
        fs::write(n.peer.join("notes"), "incoming version\n").unwrap();
        n.at(&n.peer, &["add", "notes"]);
        let incoming = n.peer_commit();

        let err = pull(&f.repo, "main", &local, Strategy::FastForwardIfPossible).unwrap_err();
        assert!(err.contains("merge in progress"));
        assert!(err.contains("Resolve the conflicted files"));
        assert!(!err.contains("exit"));
        assert_eq!(n.head(), local);
        assert_eq!(n.at(&f.dir, &["rev-parse", "MERGE_HEAD"]), incoming);
        assert_eq!(n.at(&f.dir, &["show", ":2:notes"]), "local version");
        assert_eq!(n.at(&f.dir, &["show", ":3:notes"]), "incoming version");
        assert!(f.repo.ensure_index_writable().is_err());
        f.git(&["merge", "--abort"]);
        assert!(f.repo.ensure_index_writable().is_ok());
        assert_eq!(fs::read(f.dir.join("notes")).unwrap(), b"local version\n");
    }

    #[test]
    fn pull_detects_a_checkout_that_changed_during_fetch() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let incoming = n.peer_commit();
        f.git(&["branch", "other"]);
        n.install_hook(
            "reference-transaction",
            r#"#!/bin/sh
if test "$1" = committed; then
    while read -r old new ref; do
        if test "$ref" = refs/remotes/review/published; then
            git -c core.hooksPath=/dev/null symbolic-ref HEAD refs/heads/other || exit 1
        fi
    done
fi
"#,
        );
        assert!(
            pull(&f.repo, "main", &base, Strategy::FastForwardIfPossible)
                .unwrap_err()
                .contains("branch")
        );
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "--short", "HEAD"]), "other");
        assert_eq!(n.head(), base);
        assert_eq!(n.at(&f.dir, &["rev-parse", "main"]), base);
        assert_eq!(
            n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]),
            incoming
        );
    }

    fn peer_file(n: &Network, file: &str, text: &str, subject: &str) -> String {
        n.at(&n.peer, &["pull", "-q", "--ff-only", "origin", "published"]);
        fs::write(n.peer.join(file), text).unwrap();
        n.at(&n.peer, &["add", "--", file]);
        n.at(&n.peer, &["-c", "user.name=Peer", "-c", "user.email=peer@example.invalid", "commit", "-qm", subject]);
        n.at(&n.peer, &["push", "-q", "origin", "published"]);
        n.at(&n.peer, &["rev-parse", "HEAD"])
    }

    fn local_file(n: &Network, file: &str, text: &str, subject: &str) -> String {
        fs::write(n.local.dir.join(file), text).unwrap();
        n.local.git(&["add", "--", file]);
        n.local.git(&["commit", "-qm", subject]);
        n.head()
    }

    #[test]
    fn a_rebase_pull_puts_the_local_commits_on_top_of_the_fetched_tip() {
        let n = Network::new();
        let f = &n.local;
        local_file(&n, "camp.txt", "camp\n", "Pitch camp");
        let local = local_file(&n, "map.txt", "creek\n", "Map the creek");
        let incoming = peer_file(&n, "bridge.txt", "stone\n", "Bridge the river");
        f.git(&["config", "pull.rebase", "false"]);
        f.git(&["config", "pull.ff", "true"]);
        fs::write(f.dir.join("notes.txt"), "untracked\n").unwrap();
        let started = Mutex::new(None);
        let said = super::pull(&f.repo, "main", &local, Strategy::Rebase, &started, false, &Kept::default()).unwrap();
        let head = n.head();
        assert_eq!(said, format!("From review/published: 1 commit brought in, and main's 2 commits replayed on top. main is at {}.", &head[..7]));
        assert_eq!(n.at(&f.dir, &["rev-parse", "HEAD~2"]), incoming);
        assert_eq!(n.at(&f.dir, &["rev-list", "--count", "--merges", "HEAD"]), "0");
        assert_eq!(n.at(&f.dir, &["log", "--format=%s", "-3"]), "Map the creek\nPitch camp\nBridge the river");
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(tracking(&n), incoming);
        assert_eq!(n.at(&f.dir, &["status", "--porcelain=v1", "--untracked-files=all"]), "?? notes.txt");
        assert!(crate::operation::snapshot(&f.repo).unwrap().is_none());
        assert_eq!(published(&n), incoming);
        assert_eq!(started.lock().unwrap().as_ref().map(|(oid, label)| (oid.as_str(), label.as_str())), Some((incoming.as_str(), "review/published")));

        let said = pull(&f.repo, "main", &head, Strategy::Rebase).unwrap();
        assert_eq!(said, "main already contains review/published.");
        assert_eq!(n.head(), head);
    }

    #[test]
    fn a_pull_with_changes_in_its_way_offers_gits_autostash_and_puts_them_back() {
        for strategy in [Strategy::FastForwardIfPossible, Strategy::Rebase] {
            let n = Network::new();
            let f = &n.local;
            local_file(&n, "route.txt", "Start\nFord\nSummit\n", "Plan the route");
            f.git(&["push", "-q", "team", "main:refs/heads/published"]);
            let base = n.head();
            let incoming = peer_file(&n, "route.txt", "Start north\nFord\nSummit\n", "Start north");
            fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
            let kept = Kept::default();
            let started = Mutex::new(None);
            let refused = super::pull(&f.repo, "main", &base, strategy, &started, false, &kept).unwrap_err();
            assert!(autostash::in_the_way(&refused), "{strategy:?}: {}", refused.text);
            let [(label, crate::view::Action::Autostash(autostash::Action::Retry(Retry::Pull { head, strategy: again })))] =
                refused.actions.as_slice()
            else {
                panic!("{strategy:?}: the refusal offered {:?}", refused.actions);
            };
            assert_eq!((label.as_ref(), head.as_str(), *again), (autostash::RETRY, base.as_str(), strategy));
            assert_eq!(n.head(), base);

            let said = super::pull(&f.repo, "main", &base, strategy, &started, true, &kept).unwrap();
            assert_eq!(
                said,
                "From review/published: fast-forwarded main by 1 commit. Your local changes were stashed first and put back.",
                "{strategy:?}"
            );
            assert_eq!(n.head(), incoming);
            assert_eq!(fs::read_to_string(f.dir.join("route.txt")).unwrap(), "Start north\nFord\nSummit east\n");
            assert_eq!(f.git(&["stash", "list"]), b"", "{strategy:?}");
            assert!(kept.take().is_none());
        }
    }

    #[test]
    fn a_rebase_pull_fast_forwards_a_branch_with_nothing_of_its_own() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let incoming = peer_file(&n, "bridge.txt", "stone\n", "Bridge the river");
        let said = pull(&f.repo, "main", &base, Strategy::Rebase).unwrap();
        assert_eq!(said, "From review/published: fast-forwarded main by 1 commit.");
        assert_eq!(n.head(), incoming);
        let n = Network::new();
        let f = &n.local;
        let local = local_file(&n, "bridge.txt", "stone\n", "Bridge the river");
        let patch = f.git(&["format-patch", "--stdout", "-1", "HEAD"]);
        n.at(&n.peer, &["pull", "-q", "--ff-only", "origin", "published"]);
        fs::write(n.remotes.path().join("bridge.patch"), patch).unwrap();
        n.at(&n.peer, &["-c", "user.name=Peer", "-c", "user.email=peer@example.invalid", "am", "-q", "../bridge.patch"]);
        n.at(&n.peer, &["push", "-q", "origin", "published"]);
        let incoming = n.at(&n.peer, &["rev-parse", "HEAD"]);
        let said = pull(&f.repo, "main", &local, Strategy::Rebase).unwrap();
        assert_eq!(
            said,
            format!("From review/published: review/published already had main's changes, so main moved to it at {}.", &incoming[..7])
        );
        assert_eq!(n.head(), incoming);
    }

    #[test]
    fn a_rebase_pull_that_conflicts_stops_for_the_operation_panel() {
        let n = Network::new();
        let f = &n.local;
        let local = local_file(&n, "notes", "local version\n", "Local note");
        let incoming = peer_file(&n, "notes", "incoming version\n", "Incoming note");
        let started = Mutex::new(None);
        let err = super::pull(&f.repo, "main", &local, Strategy::Rebase, &started, false, &Kept::default()).unwrap_err();
        assert_eq!(err.text, "The rebase stopped on a conflict at commit 1 of 1. 1 file needs resolving before it can go on.");
        assert!(err.output.as_deref().is_some_and(|said| said.contains("CONFLICT")), "{:?}", err.output);
        let stopped = crate::operation::snapshot(&f.repo).unwrap().expect("a rebase in progress");
        assert_eq!(stopped.kind, crate::operation::Kind::Rebase);
        assert_eq!(stopped.source.as_ref().map(|s| s.oid.as_str()), Some(incoming.as_str()));
        assert_eq!(started.lock().unwrap().clone(), Some((incoming.clone(), "review/published".to_owned())));
        assert_eq!(n.at(&f.dir, &["show", ":3:notes"]), "local version");
        assert_eq!(n.at(&f.dir, &["show", ":2:notes"]), "incoming version");
        assert!(pull(&f.repo, "main", &local, Strategy::Rebase).unwrap_err().contains("rebase"));
        crate::operation::backend::abort(&f.repo, &stopped).unwrap();
        assert_eq!(n.head(), local);
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "HEAD"]), "refs/heads/main");
        assert_eq!(fs::read(f.dir.join("notes")).unwrap(), b"local version\n");
    }

    #[test]
    fn a_rebase_pull_refuses_a_branch_that_moved_or_changed_during_the_fetch() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let local = local_file(&n, "camp.txt", "camp\n", "Pitch camp");
        let incoming = peer_file(&n, "bridge.txt", "stone\n", "Bridge the river");
        let err = pull(&f.repo, "main", &base, Strategy::Rebase).unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert_eq!(n.head(), local);
        f.git(&["branch", "other"]);
        n.install_hook(
            "reference-transaction",
            r#"#!/bin/sh
if test "$1" = committed; then
    while read -r old new ref; do
        if test "$ref" = refs/remotes/review/published; then
            git -c core.hooksPath=/dev/null symbolic-ref HEAD refs/heads/other || exit 1
        fi
    done
fi
"#,
        );
        let started = Mutex::new(None);
        let err = super::pull(&f.repo, "main", &local, Strategy::Rebase, &started, false, &Kept::default()).unwrap_err();
        assert!(err.text.contains("branch"), "{}", err.text);
        assert!(started.lock().unwrap().is_none());
        assert_eq!(n.at(&f.dir, &["rev-parse", "main"]), local);
        assert_eq!(tracking(&n), incoming);
        assert!(crate::operation::snapshot(&f.repo).unwrap().is_none());
    }

    fn loose_commit(n: &Network, parent: &str, subject: &str) -> String {
        n.at(&n.local.dir, &["commit-tree", "-p", parent, "-m", subject, &format!("{parent}^{{tree}}")])
    }

    fn work_on_side(n: &Network) -> Vec<u8> {
        let f = &n.local;
        f.git(&["switch", "-qc", "side"]);
        fs::write(f.dir.join("working"), "staged\n").unwrap();
        f.git(&["add", "working"]);
        fs::write(f.dir.join("working"), "unstaged\n").unwrap();
        f.git(&["ls-files", "--stage", "-z"])
    }

    #[test]
    fn a_branch_that_is_not_checked_out_fast_forwards_and_leaves_head_alone() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let index = work_on_side(&n);
        let incoming = n.peer_commit();
        f.git(&["fetch", "-q", "team"]);
        let tracking = OsStr::new("refs/remotes/review/published");
        let (title, detail) = fast_forward(&f.repo, "main", &base, tracking, &incoming).unwrap();
        assert_eq!(title, "Fast-forwarded main");
        assert_eq!(detail, format!("1 commit from review/published. Now at {}.", &incoming[..7]));
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/heads/main"]), incoming);
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "HEAD"]), "refs/heads/side");
        assert_eq!(n.head(), base);
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        assert_eq!(fs::read(f.dir.join("working")).unwrap(), b"unstaged\n");
        let (title, detail) = fast_forward(&f.repo, "main", &incoming, tracking, &incoming).unwrap();
        assert_eq!((title.as_str(), detail.as_str()), ("main is up to date", "main already contains review/published."));

        let err = fast_forward(&f.repo, "main", &base, tracking, &incoming).unwrap_err();
        assert!(err.contains("main changed since you chose it"), "{err}");
        let err = fast_forward(&f.repo, "main", &incoming, tracking, &base).unwrap_err();
        assert!(err.contains("review/published changed since you chose it"), "{err}");
        let own = loose_commit(&n, &base, "Local work");
        f.git(&["update-ref", "refs/heads/main", &own]);
        let err = fast_forward(&f.repo, "main", &own, tracking, &incoming).unwrap_err();
        assert_eq!(
            err,
            "main has 1 commit that review/published does not, so it cannot be fast-forwarded. Check out main to merge or rebase."
        );
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/heads/main"]), own);
        f.git(&["update-ref", "refs/heads/main", &base]);
        let err = fast_forward(&f.repo, "side", &base, tracking, &incoming).unwrap_err();
        assert_eq!(err, "Set an upstream for side first.");
        let other = f.dir.with_extension("worktree");
        f.git(&["worktree", "add", "-q", other.to_str().unwrap(), "main"]);
        let err = fast_forward(&f.repo, "main", &base, tracking, &incoming).unwrap_err();
        assert!(err.contains("refusing to fetch into branch"), "{err}");
        f.git(&["worktree", "remove", "--force", other.to_str().unwrap()]);
        f.git(&["stash", "-q"]);
        f.git(&["switch", "-q", "main"]);
        let err = fast_forward(&f.repo, "main", &base, tracking, &incoming).unwrap_err();
        assert!(err.contains("refusing to fetch into branch"), "{err}");
        assert_eq!(n.head(), base);
    }

    fn integrate(repo: &Repository, head: &str, moved: bool, fetched: Fetched) -> Result<String, String> {
        let strategy = Strategy::FastForwardIfPossible;
        pull_integrate(repo, "main", head, moved, fetched, strategy, &Mutex::new(None), false, &Kept::default())
            .map_err(|failure| failure.text)
    }

    #[test]
    fn a_pull_whose_fetch_ends_after_a_commit_here_integrates_into_the_new_tip() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let incoming = n.peer_commit();
        let fetched = pull_fetch(&f.repo, "main", &base).unwrap();
        assert_eq!(n.head(), base);
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]), incoming);
        fs::write(f.dir.join("notes.txt"), "camp\n").unwrap();
        f.git(&["add", "notes.txt"]);
        f.git(&["commit", "-qm", "Note the camp"]);
        let local = n.head();
        let refused = integrate(&f.repo, &base, false, fetched.clone()).unwrap_err();
        assert!(refused.contains("changed"), "{refused}");
        assert_eq!(n.head(), local);
        let said = integrate(&f.repo, &base, true, fetched).unwrap();
        assert!(said.starts_with("From review/published: "), "{said}");
        assert_eq!(n.at(&f.dir, &["rev-parse", "HEAD^1"]), local);
        assert_eq!(n.at(&f.dir, &["rev-parse", "HEAD^2"]), incoming);
        assert_eq!(n.at(&f.dir, &["status", "--porcelain=v1"]), "");
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"camp\n");
    }

    #[test]
    fn a_pull_does_not_integrate_into_a_branch_switched_or_rewritten_while_it_fetched() {
        let n = Network::new();
        let f = &n.local;
        f.git(&["commit", "--allow-empty", "-qm", "Second"]);
        let head = n.head();
        let first = n.at(&f.dir, &["rev-parse", "HEAD~1"]);
        n.peer_commit();
        let fetched = pull_fetch(&f.repo, "main", &head).unwrap();
        f.git(&["switch", "-qc", "side"]);
        let err = integrate(&f.repo, &head, true, fetched.clone()).unwrap_err();
        assert_eq!(err, "The checked-out branch changed while the pull fetched. Refresh and try again.");
        f.git(&["switch", "-q", "main"]);
        f.git(&["reset", "-q", "--hard", &first]);
        let err = integrate(&f.repo, &head, true, fetched).unwrap_err();
        assert_eq!(err, "main was rewritten while the pull fetched. Refresh and try again.");
        assert_eq!(n.head(), first);
        assert!(pull_fetch(&f.repo, "main", &head).is_err());
    }

    #[test]
    fn pulling_a_branch_that_is_not_checked_out_fetches_it_and_fast_forwards() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let index = work_on_side(&n);
        let incoming = n.peer_commit();
        let (title, detail) = pull_named(&f.repo, "main", &base).unwrap();
        assert_eq!(title, "Pulled main");
        assert_eq!(detail, "Fast-forwarded main by 1 commit from review/published.");
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/heads/main", "refs/remotes/review/published"]), format!("{incoming}\n{incoming}"));
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "HEAD"]), "refs/heads/side");
        assert_eq!(f.git(&["ls-files", "--stage", "-z"]), index);
        let (title, detail) = pull_named(&f.repo, "main", &incoming).unwrap();
        assert_eq!((title.as_str(), detail.as_str()), ("main is up to date", "main already contains review/published."));
        let own = loose_commit(&n, &incoming, "Local work");
        f.git(&["update-ref", "refs/heads/main", &own]);
        let newer = n.peer_commit();
        let err = pull_named(&f.repo, "main", &own).unwrap_err();
        assert!(err.contains("cannot be fast-forwarded"), "{err}");
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/heads/main", "refs/remotes/review/published"]), format!("{own}\n{newer}"));
        let err = pull_named(&f.repo, "main", &incoming).unwrap_err();
        assert!(err.contains("main changed since you chose it"), "{err}");
    }

    #[test]
    fn pushing_a_branch_that_is_not_checked_out_sends_only_that_branch() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        work_on_side(&n);
        let tip = loose_commit(&n, &base, "Publish from the side");
        f.git(&["update-ref", "refs/heads/main", &tip]);
        assert_eq!(push_named(&f.repo, "main", &tip).unwrap(), ("1 commit to review/published.".to_owned(), Said::default()));
        assert_eq!(n.at(&n.bare, &["for-each-ref", "--format=%(refname) %(objectname)"]), format!("refs/heads/published {tip}"));
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]), tip);
        assert_eq!(n.at(&f.dir, &["symbolic-ref", "HEAD"]), "refs/heads/side");
        let err = String::from(push_named(&f.repo, "main", &base).unwrap_err());
        assert!(err.contains("main changed since you chose it"), "{err}");
        let err = String::from(push_named(&f.repo, "side", &base).unwrap_err());
        assert_eq!(err, "Set an upstream for side first.");
        n.at(&n.peer, &["pull", "-q", "--ff-only", "origin", "published"]);
        let incoming = n.peer_commit();
        let own = loose_commit(&n, &tip, "Local work");
        f.git(&["update-ref", "refs/heads/main", &own]);
        let refused = push_named(&f.repo, "main", &own).unwrap_err();
        assert!(super::super::rejected(&refused), "{refused:?}");
        assert_eq!(n.at(&n.bare, &["rev-parse", "published"]), incoming);
    }

    fn tracking(n: &Network) -> String {
        n.at(&n.local.dir, &["rev-parse", "refs/remotes/review/published"])
    }

    fn published(n: &Network) -> String {
        n.at(&n.bare, &["rev-parse", "refs/heads/published"])
    }

    #[test]
    fn a_rewritten_branch_force_pushes_under_a_lease_and_can_be_put_back() {
        let n = Network::new();
        let f = &n.local;
        f.git(&["commit", "-qm", "Draft the notes", "--allow-empty"]);
        let pushed = n.head();
        push(&f.repo, "main", &pushed).unwrap();
        f.git(&["commit", "--amend", "-qm", "Final notes", "--allow-empty"]);
        let amended = n.head();
        let refused = push(&f.repo, "main", &amended).unwrap_err();
        assert!(super::super::rejected(&refused), "{refused:?}");
        assert_eq!(published(&n), pushed);

        let forced = force_preview(&f.repo, "main", &amended, true).unwrap();
        assert_eq!((forced.replaced, forced.sent), (1, 1));
        assert_eq!((forced.remote.as_str(), forced.expect.as_str()), ("team", pushed.as_str()));
        assert_eq!((forced.remote_ref.to_str(), forced.tracking_ref.to_str()), (Some("refs/heads/published"), Some("refs/remotes/review/published")));
        assert_eq!(forced.subjects, [(pushed[..7].to_owned(), "Draft the notes".to_owned())]);
        assert!(force_preview(&f.repo, "main", &pushed, true).is_err());

        let moved = Forced { tip: pushed.clone(), ..forced.clone() };
        let err = String::from(force_push(&f.repo, &moved).unwrap_err());
        assert!(err.contains("changed"), "{err}");
        let other = Forced { current: true, branch: "side".into(), ..forced.clone() };
        f.git(&["branch", "side"]);
        assert!(force_push(&f.repo, &other).is_err());
        f.git(&["update-ref", "refs/remotes/review/published", &amended]);
        let err = String::from(force_push(&f.repo, &forced).unwrap_err());
        assert!(err.contains("review/published"), "{err}");
        f.git(&["update-ref", "refs/remotes/review/published", &pushed]);
        f.git(&["config", "--add", "remote.team.fetch", "+refs/heads/published:refs/heads/copy"]);
        assert!(force_push(&f.repo, &forced).is_err());
        f.git(&["config", "--unset", "remote.team.fetch", "refs/heads/copy"]);
        assert_eq!(published(&n), pushed);

        let (detail, replaced, _) = force_push(&f.repo, &forced).unwrap();
        assert_eq!(detail, "Replaced 1 commit on review/published with 1 from main.");
        assert!(replaced);
        assert_eq!(published(&n), amended);
        assert_eq!(tracking(&n), amended);
        assert_eq!(n.head(), amended);
        assert_eq!(n.at(&f.dir, &["status", "--porcelain"]), "");
        let again = Forced { expect: amended.clone(), ..forced.clone() };
        assert_eq!(force_push(&f.repo, &again).unwrap(), ("review/published is already up to date.".to_owned(), false, Said::default()));

        let (remote_ref, tracking_ref) = (OsStr::new("refs/heads/published"), OsStr::new("refs/remotes/review/published"));
        let err = String::from(put_back(&f.repo, "team", remote_ref, tracking_ref, &pushed, &pushed).unwrap_err());
        assert!(err.contains("review/published"), "{err}");
        assert_eq!(put_back(&f.repo, "team", remote_ref, tracking_ref, &pushed, &amended).unwrap(), format!("It is at {} again.", &pushed[..7]));
        assert_eq!(published(&n), pushed);
        assert_eq!(tracking(&n), pushed);
        assert_eq!(n.head(), amended);
        assert!(put_back(&f.repo, "team", remote_ref, tracking_ref, &pushed, &amended).is_err());
        assert_eq!(published(&n), pushed);
        n.at(&n.bare, &["update-ref", "refs/heads/published", &amended]);
        let start = n.at(&f.dir, &["rev-parse", "HEAD~1"]);
        let err = put_back(&f.repo, "team", remote_ref, tracking_ref, &start, &pushed).unwrap_err();
        let failure = super::super::restore_refused(err, "review/published");
        assert_eq!(failure.text, "Restore Failed — review/published changed since the force push. Fetch and look again.");
        let offered: Vec<_> = failure.actions.iter().map(|(label, _)| label.as_ref()).collect();
        assert_eq!(offered, ["Fetch"]);
        let said = failure.output.clone().unwrap_or_default();
        assert!(said.contains("published: [rejected] (stale info)"), "{said}");
        assert_eq!(published(&n), amended);
    }

    #[test]
    fn a_lease_against_an_out_of_date_tracking_ref_is_refused_and_leaves_the_remote_alone() {
        let n = Network::new();
        let f = &n.local;
        let base = n.head();
        let incoming = n.peer_commit();
        f.git(&["commit", "--amend", "-qm", "Rewritten start", "--allow-empty"]);
        let amended = n.head();
        let forced = force_preview(&f.repo, "main", &amended, true).unwrap();
        assert_eq!((forced.expect.as_str(), forced.replaced, forced.sent), (base.as_str(), 1, 1));
        let err = force_push(&f.repo, &forced).unwrap_err();
        assert!(matches!(err.rejection(), Some(crate::remotes::Rejection::Stale)), "{err:?}");
        let failure = super::super::force_refused(err, &forced);
        assert_eq!(failure.text, "Force Push Failed — review/published changed since your last fetch.");
        let offered: Vec<_> = failure.actions.iter().map(|(label, _)| label.as_ref()).collect();
        assert_eq!(offered, ["Fetch"]);
        let said = failure.output.clone().unwrap_or_default();
        assert!(said.contains("published: [rejected] (stale info)") && said.contains("failed to push"), "{said}");
        assert_eq!(published(&n), incoming);
        assert_eq!(tracking(&n), base);
        assert_eq!(n.head(), amended);

        f.git(&["update-ref", "-d", "refs/remotes/review/published"]);
        assert_eq!(
            force_preview(&f.repo, "main", &amended, true).unwrap_err(),
            "Fetch team first, so FastKraken can see what would be replaced."
        );
        assert_eq!(published(&n), incoming);
    }

    #[test]
    fn a_branch_that_tracks_a_local_branch_has_nothing_to_force_push() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "One"]);
        f.git(&["commit", "--allow-empty", "-qm", "Two"]);
        f.git(&["branch", "-q", "--track", "topic", "main"]);
        f.git(&["switch", "-q", "topic"]);
        f.git(&["reset", "-q", "--hard", "HEAD~1"]);
        f.git(&["commit", "--allow-empty", "-qm", "Three"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let main = String::from_utf8(f.git(&["rev-parse", "main"])).unwrap();
        assert_eq!(
            force_preview(&f.repo, "topic", &head, true).unwrap_err(),
            "topic tracks the local branch main, so there is no remote branch to force push it to."
        );
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "main"])).unwrap(), main);
    }

    #[test]
    fn a_refused_push_fetches_counts_what_the_remote_has_and_offers_pull_and_force_push() {
        let n = Network::new();
        let f = &n.local;
        f.git(&["commit", "-qm", "Local revision", "--allow-empty"]);
        let local = n.head();
        let incoming = n.peer_commit();
        let refused = push(&f.repo, "main", &local).unwrap_err();
        let failure = super::super::refused_push(&f.repo, "main", &local, true, refused);
        assert_eq!(failure.text, "Push Rejected — review/published has 1 commit that main does not.");
        let [(pull, crate::view::Action::Sync(super::super::Action::PullInto(into))), (force, crate::view::Action::Sync(super::super::Action::ForcePush { name, checked_out: true }))] =
            failure.actions.as_slice()
        else {
            panic!("The rejection offered {:?}", failure.actions);
        };
        assert_eq!((pull.as_ref(), into.to_str(), force.as_ref(), name.to_str()), ("Pull", Some("main"), "Force push…", Some("main")));
        assert!(failure.output.as_deref().is_some_and(|said| said.contains("fetch first")), "{:?}", failure.output);
        assert_eq!(tracking(&n), incoming);
        assert_eq!(n.head(), local);
        assert_eq!(published(&n), incoming);

        f.git(&["switch", "-q", "-c", "side"]);
        let refused = push_named(&f.repo, "main", &local).unwrap_err();
        let failure = super::super::refused_push(&f.repo, "main", &local, false, refused);
        let offered: Vec<_> = failure.actions.iter().map(|(label, _)| label.as_ref()).collect();
        assert_eq!(offered, ["Force push…"]);
        n.install_hook("pre-push", "#!/bin/sh\necho 'Not today' >&2\nexit 1\n");
        f.git(&["switch", "-q", "main"]);
        let refused = push(&f.repo, "main", &local).unwrap_err();
        let failure = super::super::refused_push(&f.repo, "main", &local, true, refused);
        assert!(failure.actions.is_empty() && failure.text.starts_with("Push Failed — "), "{failure:?}");
    }

    #[test]
    fn a_fetch_counts_what_changed_from_the_porcelain_lines() {
        let zero = "0".repeat(40);
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let lines = format!(
            "  {a} {b} refs/remotes/origin/main\n+ {a} {b} refs/remotes/origin/wip\n* {zero} {b} refs/remotes/origin/new\n* {zero} {b} refs/remotes/fork/new\n- {a} {zero} refs/remotes/origin/old\n* {zero} {b} refs/tags/v1\nt {a} {b} refs/tags/v2\n"
        );
        assert_eq!(fetched(lines.as_bytes()), "2 branches updated, 2 new, 1 branch removed, 1 new tag, 1 tag updated.");
        let tags = format!("t {a} {b} refs/tags/v2\nt {a} {b} refs/tags/v3\n- {a} {zero} refs/tags/v4\n");
        assert_eq!(fetched(tags.as_bytes()), "2 tags updated, 1 tag removed.");
        assert_eq!(fetched(format!("* {zero} {b} refs/remotes/origin/new\n").as_bytes()), "1 new branch.");
        assert_eq!(fetched(b""), "Everything is up to date.");
    }

    #[test]
    fn fetching_one_remote_moves_only_its_tracking_refs() {
        let n = Network::new();
        let f = &n.local;
        let other = n.remotes.path().join("other.git");
        f.git(&["clone", "-q", "--bare", n.bare.to_str().unwrap(), other.to_str().unwrap()]);
        f.git(&["remote", "add", "second", other.to_str().unwrap()]);
        let before = n.head();
        let incoming = n.peer_commit();
        n.at(&n.peer, &["push", "-q", other.to_str().unwrap(), "published:refs/heads/extra"]);
        assert_eq!(fetch_remote(&f.repo, "team", true).unwrap(), "1 branch updated.");
        assert_eq!(n.at(&f.dir, &["rev-parse", "refs/remotes/review/published"]), incoming);
        assert_eq!(n.at(&f.dir, &["for-each-ref", "--format=%(refname)", "refs/remotes/second"]), "");
        assert_eq!(n.head(), before);
        assert_eq!(fetch_remote(&f.repo, "team", true).unwrap(), "Everything is up to date.");
        assert_eq!(fetch_remote(&f.repo, "second", true).unwrap(), "2 new branches.");
        f.git(&["remote", "remove", "second"]);
        assert_eq!(fetch_remote(&f.repo, "second", true).unwrap_err(), "second is not a remote of this repository any more.");
        assert!(fetch_remote(&f.repo, "--all", true).is_err());
        assert_eq!(n.at(&f.dir, &["for-each-ref", "--format=%(refname)", "refs/remotes/second"]), "");
    }

    #[test]
    fn pruning_removes_only_remote_tracking_branches_deleted_on_the_remote() {
        let n = Network::new();
        let f = &n.local;
        let bare = n.bare.to_str().unwrap().to_owned();
        n.at(&n.peer, &["push", "-q", "origin", "published:refs/heads/old-route", "published:refs/heads/side-route"]);
        f.git(&["remote", "add", "mirror", &bare]);
        f.git(&["config", "remote.mirror.fetch", "+refs/heads/*:refs/heads/mirrored/*"]);
        assert_eq!(fetch_remote(&f.repo, "team", true).unwrap(), "2 new branches.");
        fetch_remote(&f.repo, "mirror", true).unwrap();
        f.git(&["branch", "--track", "old-route", "review/old-route"]);
        f.git(&["tag", "kept-here"]);
        f.git(&["config", "fetch.pruneTags", "true"]);
        n.at(&n.peer, &["push", "-q", "origin", ":refs/heads/old-route", ":refs/heads/side-route"]);
        let refs = || n.at(&f.dir, &["for-each-ref", "--format=%(refname)"]);
        assert_eq!(fetch_all(&f.repo, false).unwrap(), "Everything is up to date.");
        assert!(refs().contains("refs/remotes/review/old-route"));
        assert_eq!(fetch_remote(&f.repo, "team", true).unwrap(), "2 branches removed.");
        let after = refs();
        assert!(!after.contains("refs/remotes/review/old-route") && !after.contains("refs/remotes/review/side-route"), "{after}");
        for kept in ["refs/heads/old-route", "refs/tags/kept-here", "refs/heads/mirrored/old-route", "refs/remotes/review/published"] {
            assert!(after.lines().any(|line| line == kept), "{kept} in {after}");
        }
        assert_eq!(fetch_all(&f.repo, true).unwrap(), "Everything is up to date.");
        assert!(refs().lines().any(|line| line == "refs/heads/mirrored/old-route"));
        assert!(!prunable(&f.repo, &["mirror".to_owned()]).unwrap());
        assert!(prunable(&f.repo, &["team".to_owned()]).unwrap());
        assert!(!prunable(&f.repo, &["team".to_owned(), "mirror".to_owned()]).unwrap());
    }

    #[test]
    fn a_branch_that_is_not_checked_out_fast_forwards_to_any_ref_and_refuses_what_moved() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Pitch the camp"]);
        let base = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        f.git(&["branch", "lookout"]);
        f.git(&["switch", "-qc", "ridge"]);
        f.git(&["commit", "--allow-empty", "-qm", "Climb the ridge"]);
        f.git(&["commit", "--allow-empty", "-qm", "Reach the summit"]);
        let ridge = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        f.git(&["switch", "-q", "main"]);
        f.git(&["tag", "summit", &ridge]);
        let index = f.git(&["ls-files", "--stage"]);
        let source = crate::merge::Source::Branch("ridge".into());
        let err = advance_to(&f.repo, "lookout", &base, &source, &base).unwrap_err();
        assert!(err.contains("changed since you chose it"), "{err}");
        let tag = crate::merge::Source::Tag("summit".into());
        let (title, detail) = advance_to(&f.repo, "lookout", &base, &tag, &ridge).unwrap();
        assert_eq!((title.as_str(), detail.as_str()), ("Fast-forwarded lookout", format!("2 commits from summit. Now at {}.", &ridge[..7]).as_str()));
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "lookout"])).unwrap().trim(), ridge);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim(), base);
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        let err = advance_to(&f.repo, "lookout", &base, &source, &ridge).unwrap_err();
        assert!(err.contains("lookout changed"), "{err}");
        let err = advance_to(&f.repo, "main", &base, &source, &ridge).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "main"])).unwrap().trim(), base);
        let commit = crate::merge::Source::Commit("0123456789abcdef0123456789abcdef01234567".into());
        let err = advance_to(&f.repo, "lookout", &ridge, &commit, "0123456789abcdef0123456789abcdef01234567").unwrap_err();
        assert_eq!(err, "That commit is not in the repository. Refresh and try again.");
    }
}
