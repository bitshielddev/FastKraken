use crate::repository::Run;
use crate::{
    Tab, branch,
    commands::{Availability, Command, Group, Target, busy, on_repo, operating},
    notice::{self, Notice, Outcome},
    prompt::{self, Answer, Spec},
    repository::{Repository, Revisions, checked},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
};

pub(crate) const KEPT_NAMES: &[&str] = &["main", "master", "develop"];
pub(crate) const NAMED: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Removed {
    pub name: OsString,
    pub oid: String,
    pub tracking: Option<branch::Tracking>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub name: OsString,
    pub tip: String,
    pub merged: bool,
    pub gone: bool,
    pub unmerged: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    AskDelete(Vec<OsString>),
    AskMerged,
}

#[derive(Debug)]
pub(crate) enum Ask {
    Delete { branches: Vec<(OsString, String)>, unique: usize, kept: Option<String> },
    Merged { base: String, found: Vec<Candidate> },
}

fn shown(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

pub(crate) fn listing(names: &[String]) -> String {
    let named: Vec<&str> = names.iter().take(NAMED).map(String::as_str).collect();
    match (named.as_slice(), names.len().saturating_sub(NAMED)) {
        ([], _) => String::new(),
        ([one], 0) => (*one).to_owned(),
        (many, 0) => format!("{} and {}", many[..many.len() - 1].join(", "), many[many.len() - 1]),
        (many, rest) => format!("{} and {rest} more", many.join(", ")),
    }
}

pub(crate) fn branches_counted(count: usize) -> String {
    match count {
        1 => "1 branch".to_owned(),
        n => format!("{n} branches"),
    }
}

fn deletable(ui: &Ui, names: &[OsString]) -> (Vec<OsString>, Option<String>) {
    let current = crate::graph_view::current(ui);
    let mut kept = None;
    let mut left = Vec::new();
    for name in names {
        if current.is_some_and(|current| current.as_bytes() == name.as_bytes()) {
            kept = Some(shown(name));
        } else if ui.refs.local(name).is_some() {
            left.push(name.clone());
        }
    }
    (left, kept)
}

fn detail(candidate: &Candidate, base: &str) -> String {
    match (candidate.merged, candidate.gone) {
        (true, true) => "merged · remote branch gone".to_owned(),
        (true, false) => "merged".to_owned(),
        (false, _) => format!("remote branch gone · {} not in {base}", notice::commits(candidate.unmerged)),
    }
}

pub(crate) fn describe(ask: &Ask, ui: &Ui) -> Spec {
    match ask {
        Ask::Delete { branches, unique, kept } => delete_spec(branches, *unique, kept.as_deref()),
        Ask::Merged { base, found } => {
            let defaults: Vec<bool> = found.iter().map(|candidate| candidate.merged).collect();
            let ticks = if ui.prompt.items().len() == found.len() { ui.prompt.items() } else { &defaults };
            merged_spec(base, found, ticks)
        }
    }
}

fn delete_spec(branches: &[(OsString, String)], unique: usize, kept: Option<&str>) -> Spec {
    let count = branches.len();
    let mut spec = Spec::new(format!("Delete {}?", branches_counted(count)), format!("Delete {}", branches_counted(count)));
    for (name, _) in branches.iter().take(NAMED) {
        spec = spec.line(shown(name));
    }
    if count > NAMED {
        spec = spec.line(format!("and {} more", count - NAMED));
    }
    spec = spec.line(match (unique, count) {
        (0, 1) => "Its commits are also on other branches or tags.".to_owned(),
        (0, _) => "Their commits are also on other branches or tags.".to_owned(),
        (_, 1) => "It has commits found nowhere else. They will no longer appear in the graph.".to_owned(),
        (1, _) => "1 of them has commits found nowhere else. Those commits will no longer appear in the graph.".to_owned(),
        (n, _) => format!("{n} of them have commits found nowhere else. Those commits will no longer appear in the graph."),
    });
    if let Some(kept) = kept {
        spec = spec.line(format!("{kept} is checked out, so it is kept."));
    }
    spec.line("Undo, in the notice that follows, brings them back.").danger()
}

fn merged_spec(base: &str, found: &[Candidate], ticks: &[bool]) -> Spec {
    let mut kept: Vec<&str> = vec![base];
    kept.extend(KEPT_NAMES.iter().copied().filter(|name| *name != base));
    let kept = format!("{} and {}", kept[..kept.len() - 1].join(", "), kept[kept.len() - 1]);
    let mut spec = Spec::new("Delete merged branches", "Delete branches").line(format!(
        "Local branches merged into {base}, or whose remote branch is gone. {kept} are never listed."
    ));
    for candidate in found {
        spec = spec.item(shown(&candidate.name), detail(candidate, base), candidate.merged);
    }
    let count = ticks.iter().filter(|ticked| **ticked).count();
    if count > 0 {
        spec.confirm = format!("Delete {}", branches_counted(count)).into();
    }
    if found.iter().zip(ticks).any(|(candidate, ticked)| *ticked && !candidate.merged) {
        spec = spec.danger();
    }
    spec.line("Undo, in the notice that follows, brings them back.")
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    let branches = match ask {
        Ask::Delete { branches, .. } => branches,
        Ask::Merged { found, .. } => found
            .into_iter()
            .zip(answer.items.iter().copied().chain(std::iter::repeat(false)))
            .filter(|(_, ticked)| *ticked)
            .map(|(candidate, _)| (candidate.name, candidate.tip))
            .collect(),
    };
    if branches.is_empty() {
        return;
    }
    tab.ui.refs.picked = None;
    tab.run_operation_then(cx, "Deleting branches…", "Branches deleted", false, move |repo| delete_all(&repo, &branches), |_, _| {});
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.busy() || tab.ui.dialog_open() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    match action {
        Action::AskDelete(names) => {
            let (names, kept) = deletable(&tab.ui, &names);
            if names.is_empty() {
                let text = kept.map_or_else(|| "Pick branches to delete".to_owned(), |kept| format!("{kept} is checked out"));
                tab.ui.notices.show(Notice::new(Tone::Info, text, "Switch to another branch first."));
                cx.notify();
                return;
            }
            let branches: Vec<(OsString, String)> = names
                .into_iter()
                .filter_map(|name| {
                    let tip = tab.ui.refs.local(&name)?.oid.clone()?;
                    Some((name, tip))
                })
                .collect();
            let (epoch, counted, head) = (tab.epoch, branches.clone(), tab.ui.head.is_some());
            Tab::work(
                cx,
                move || unique(&repo, &counted, head),
                move |tab, counted, cx| {
                    let unchanged = branches.iter().all(|(name, tip)| {
                        tab.ui.refs.local(name).and_then(|item| item.oid.as_deref()) == Some(tip.as_str())
                    });
                    if tab.epoch != epoch || !unchanged || tab.ui.dialog_open() {
                        return;
                    }
                    match counted {
                        Ok(unique) => {
                            tab.ui.menu = None;
                            prompt::ask(&mut tab.ui, prompt::Kind::Cleanup(Ask::Delete { branches, unique, kept }), cx);
                        }
                        Err(error) => tab.error(error, cx),
                    }
                },
            );
        }
        Action::AskMerged => {
            let epoch = tab.epoch;
            Tab::work(
                cx,
                move || candidates(&repo),
                move |tab, found, cx| {
                    if tab.epoch != epoch || tab.ui.dialog_open() {
                        return;
                    }
                    match found {
                        Ok((base, found)) if found.is_empty() => {
                            let detail = format!("No local branch is merged into {base} or has lost its remote branch.");
                            tab.ui.notices.show(Notice::new(Tone::Info, "No merged branches to delete", detail));
                            cx.notify();
                        }
                        Ok((base, found)) => {
                            tab.ui.menu = None;
                            prompt::ask(&mut tab.ui, prompt::Kind::Cleanup(Ask::Merged { base, found }), cx);
                        }
                        Err(error) => tab.error(error, cx),
                    }
                },
            );
        }
    }
    cx.notify();
}

fn names_of(output: &[u8]) -> impl Iterator<Item = &[u8]> {
    output.split(|b| *b == b'\n').filter(|line| !line.is_empty())
}

fn checked_out(repo: &Repository) -> Result<Option<OsString>, String> {
    let output = repo
        .read_command()
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let full = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    Ok(full.strip_prefix(b"refs/heads/").map(|name| OsString::from_vec(name.to_vec())))
}

pub(crate) fn candidates(repo: &Repository) -> Result<(String, Vec<Candidate>), String> {
    let Some(base) = checked_out(repo)? else {
        return Err("Check out a branch first. Merged branches are those merged into the checked-out branch.".into());
    };
    let merged: HashSet<Vec<u8>> = names_of(&checked(
        repo.read_command().args(["for-each-ref", "--merged=HEAD", "--format=%(refname)", "refs/heads"]),
    )?)
    .map(<[u8]>::to_vec)
    .collect();
    let records = checked(repo.read_command().args([
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(upstream)%00%(upstream:track,nobracket)",
        "refs/heads",
    ]))?;
    let mut found = Vec::new();
    for line in names_of(&records) {
        let mut fields = line.split(|b| *b == 0);
        let (Some(full), Some(tip), Some(upstream), Some(track)) = (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err("Invalid branch record from Git.".into());
        };
        let Some(name) = full.strip_prefix(b"refs/heads/") else { continue };
        if name == base.as_bytes() || KEPT_NAMES.iter().any(|kept| kept.as_bytes() == name) {
            continue;
        }
        let merged = merged.contains(full);
        let gone = !upstream.is_empty() && track == b"gone";
        if !merged && !gone {
            continue;
        }
        let tip = String::from_utf8_lossy(tip).into_owned();
        let unmerged = if merged { 0 } else { repo.count(&Revisions::reachable(tip.clone()).not("HEAD"))? };
        found.push(Candidate { name: OsString::from_vec(name.to_vec()), tip, merged, gone, unmerged });
    }
    Ok((shown(&base), found))
}

pub(crate) fn unique(repo: &Repository, branches: &[(OsString, String)], head: bool) -> Result<usize, String> {
    let mut excludes = Vec::with_capacity(branches.len());
    for (name, tip) in branches {
        branch::plain(name)?;
        crate::repository::validate_oid(tip)?;
        let mut exclude = OsString::from("--exclude=");
        exclude.push(name);
        excludes.push(exclude);
    }
    let mut count = 0;
    for (_, tip) in branches {
        let mut cmd = repo.read_command();
        cmd.args(["rev-list", "--count", tip.as_str(), "--not"])
            .args(&excludes)
            .args(["--branches", "--remotes", "--tags"]);
        if head {
            cmd.arg("HEAD");
        }
        let output = checked(cmd.arg("--"))?;
        let own: usize = String::from_utf8_lossy(&output)
            .trim()
            .parse()
            .map_err(|_| "Git did not report a commit count.".to_owned())?;
        count += usize::from(own > 0);
    }
    Ok(count)
}

pub(crate) fn delete_all(repo: &Repository, branches: &[(OsString, String)]) -> Result<Outcome, String> {
    if branches.is_empty() {
        return Err("Pick branches to delete.".into());
    }
    for (name, tip) in branches {
        branch::plain(name)?;
        repo.expect_ref(&branch::full(name), tip)?;
    }
    if let Some(current) = checked_out(repo)?
        && let Some((name, _)) = branches.iter().find(|(name, _)| *name == current)
    {
        return Err(format!("{} is checked out. Switch to another branch first.", shown(name)));
    }
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for (name, tip) in branches {
        match branch::remove(repo, name, tip) {
            Ok(tracking) => removed.push(Removed { name: name.clone(), oid: tip.clone(), tracking }),
            Err(error) => failed.push(format!("{}: {error}", shown(name))),
        }
    }
    if removed.is_empty() {
        return Err(failed.join(" "));
    }
    let names: Vec<String> = removed.iter().map(|removed| shown(&removed.name)).collect();
    let title = match removed.as_slice() {
        [one] => format!("Deleted branch {}", shown(&one.name)),
        many => format!("Deleted {}", branches_counted(many.len())),
    };
    let mut detail = listing(&names);
    if !failed.is_empty() {
        detail = format!("{detail}. Not deleted: {}", failed.join(" "));
    }
    let undone = match removed.as_slice() {
        [one] => format!("Delete branch {}", shown(&one.name)),
        many => format!("Delete {}", branches_counted(many.len())),
    };
    Ok(Outcome::titled(title, detail).undone_as(undone).offering_undo().sticky())
}

fn picked(target: &Target) -> Option<&Vec<OsString>> {
    match target {
        Target::Branches(names) => Some(names),
        _ => None,
    }
}

fn delete_picked_title((deletable, picked): (usize, usize)) -> String {
    let count = if deletable == 0 { picked } else { deletable };
    format!("Delete {}…", branches_counted(count))
}

fn merged_here(ui: &Ui, target: &Target) -> bool {
    matches!(target, Target::Section(crate::refs::Section::Branches)) && !ui.repo.is_empty() || on_repo(ui, target)
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "branch.delete-picked",
        Group::Manage,
        |ui, target| delete_picked_title(picked(target).map_or((0, 0), |names| (deletable(ui, names).0.len(), names.len()))),
        |ui, target, _| {
            let Some(names) = picked(target) else { return Availability::Hidden };
            Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                deletable(ui, names).0.is_empty().then(|| "Switch to another branch first".to_owned())
            }))
        },
        |_, target| Some(view::Action::Cleanup(Action::AskDelete(picked(target)?.clone()))),
    )
    .icon("x")
    .danger(),
    Command::new(
        "repo.delete-merged",
        Group::Manage,
        |_, _| "Delete merged branches…".into(),
        |ui, target, _| {
            if !merged_here(ui, target) {
                return Availability::Hidden;
            }
            Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                (ui.detached || ui.head.is_none() || ui.branch.is_empty()).then(|| "Check out a branch first".to_owned())
            }))
        },
        |_, _| Some(view::Action::Cleanup(Action::AskMerged)),
    )
    .words(&["clean up local branches", "prune branches", "stale branches", "gone", "tidy"])
    .icon("trash"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn oid(f: &Fixture, revision: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", revision])).unwrap().trim_end().to_owned()
    }

    fn heads(f: &Fixture) -> String {
        String::from_utf8(f.git(&["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads"])).unwrap()
    }

    fn commit(f: &Fixture, text: &str) {
        fs::write(f.dir.join("camp.txt"), format!("{text}\n")).unwrap();
        f.git(&["commit", "-qam", text]);
    }

    fn camp() -> Fixture {
        let f = Fixture::new();
        fs::write(f.dir.join("camp.txt"), "Camp\n").unwrap();
        f.git(&["add", "--", "camp.txt"]);
        f.git(&["commit", "-qm", "Pitch camp"]);
        for name in ["feature/ford", "develop", "master"] {
            f.git(&["switch", "-qc", name]);
            commit(&f, &format!("Work on {name}"));
            f.git(&["switch", "-q", "main"]);
            f.git(&["merge", "-q", "--no-ff", "--no-edit", name]);
        }
        f.git(&["branch", "feature/bridge"]);
        f.git(&["remote", "add", "origin", f.dir.join("missing.git").to_str().unwrap()]);
        f.git(&["switch", "-qc", "stale"]);
        commit(&f, "Scout the stale trail");
        f.git(&["update-ref", "refs/remotes/origin/stale", "HEAD"]);
        f.git(&["config", "branch.stale.remote", "origin"]);
        f.git(&["config", "branch.stale.merge", "refs/heads/stale"]);
        f.git(&["update-ref", "-d", "refs/remotes/origin/stale"]);
        f.git(&["switch", "-qc", "wip", "main"]);
        commit(&f, "Unfinished work");
        f.git(&["switch", "-q", "main"]);
        f
    }

    #[test]
    fn merged_and_gone_branches_are_listed_and_the_main_lines_are_not() {
        let f = camp();
        let (base, found) = candidates(&f.repo).unwrap();
        assert_eq!(base, "main");
        let listed: Vec<(String, bool, bool, usize)> =
            found.iter().map(|c| (shown(&c.name), c.merged, c.gone, c.unmerged)).collect();
        assert_eq!(
            listed,
            [
                ("feature/bridge".to_owned(), true, false, 0),
                ("feature/ford".to_owned(), true, false, 0),
                ("stale".to_owned(), false, true, 1),
            ]
        );
        assert_eq!(found[2].tip, oid(&f, "stale"));
        f.git(&["switch", "-q", "--detach", "main"]);
        assert!(candidates(&f.repo).unwrap_err().contains("Check out a branch first"));
    }

    #[test]
    fn deleting_several_branches_removes_them_and_restore_all_brings_them_back_with_their_upstream() {
        let f = camp();
        let before = heads(&f);
        let index = f.git(&["ls-files", "--stage"]);
        let picked: Vec<(OsString, String)> =
            ["feature/ford", "stale"].iter().map(|name| (OsString::from(name), oid(&f, name))).collect();
        let outcome = delete_all(&f.repo, &picked).unwrap();
        assert_eq!(outcome.title, "Deleted 2 branches");
        assert_eq!(outcome.detail, "feature/ford and stale");
        assert!(outcome.sticky);
        let heads_now = heads(&f);
        assert!(!heads_now.contains("feature/ford ") && !heads_now.contains("stale "), "{heads_now}");
        assert!(heads_now.contains("feature/bridge ") && heads_now.contains("wip "), "{heads_now}");
        let config = String::from_utf8(f.git(&["config", "--local", "--list"])).unwrap();
        assert!(!config.contains("branch.stale."), "{config}");
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(f.git(&["status", "--porcelain"]), b"");

        assert!(outcome.journal.offered && outcome.actions.is_empty());
        assert_eq!(outcome.journal.title.as_deref(), Some("Delete 2 branches"));
        for (name, tip) in &picked {
            f.git(&["branch", name.to_str().unwrap(), tip]);
        }
        f.git(&["config", "branch.stale.remote", "origin"]);
        f.git(&["config", "branch.stale.merge", "refs/heads/stale"]);
        assert_eq!(heads(&f), before);
        let entry = crate::journal::tests::entry_of(&f.repo, |repo| delete_all(repo, &picked).map_err(Into::into));
        crate::journal::replay(&f.repo, &entry, crate::journal::Way::Undo).unwrap();
        assert_eq!(heads(&f), before);
        assert_eq!(f.git(&["config", "branch.stale.remote"]), b"origin\n");
        assert_eq!(f.git(&["config", "branch.stale.merge"]), b"refs/heads/stale\n");
        let again = crate::journal::replay(&f.repo, &entry, crate::journal::Way::Undo).unwrap_err();
        assert!(again.text.starts_with("Something else changed"), "{}", again.text);
    }

    #[test]
    fn a_delete_refuses_every_branch_when_one_moved_and_never_takes_the_checked_out_one() {
        let f = camp();
        let before = heads(&f);
        let picked: Vec<(OsString, String)> =
            ["feature/ford", "feature/bridge"].iter().map(|name| (OsString::from(name), oid(&f, name))).collect();
        f.git(&["update-ref", "refs/heads/feature/bridge", "HEAD~1"]);
        let moved = heads(&f);
        let err = delete_all(&f.repo, &picked).unwrap_err();
        assert!(err.contains("changed since you chose it"), "{err}");
        assert_eq!(heads(&f), moved);
        assert_ne!(moved, before);
        f.git(&["branch", "-D", "feature/bridge"]);
        let err = delete_all(&f.repo, &picked).unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        let main = vec![(OsString::from("main"), oid(&f, "main"))];
        let err = delete_all(&f.repo, &main).unwrap_err();
        assert!(err.contains("main is checked out"), "{err}");
        assert_eq!(oid(&f, "refs/heads/main"), main[0].1);
        assert!(delete_all(&f.repo, &[(OsString::from("-D"), oid(&f, "main"))]).is_err());
        assert!(delete_all(&f.repo, &[]).is_err());
    }

    #[test]
    fn a_branch_checked_out_in_another_worktree_is_kept_and_the_rest_are_deleted() {
        let f = camp();
        let other = f.dir.with_extension("worktree");
        f.git(&["worktree", "add", "-q", other.to_str().unwrap(), "wip"]);
        let picked: Vec<(OsString, String)> =
            ["wip", "feature/ford"].iter().map(|name| (OsString::from(name), oid(&f, name))).collect();
        let outcome = delete_all(&f.repo, &picked).unwrap();
        assert_eq!(outcome.title, "Deleted branch feature/ford");
        assert!(outcome.detail.contains("Not deleted: wip"), "{}", outcome.detail);
        assert_eq!(oid(&f, "refs/heads/wip"), picked[0].1);
        assert_eq!(outcome.journal.title.as_deref(), Some("Delete branch feature/ford"));
        f.git(&["worktree", "remove", "--force", other.to_str().unwrap()]);
    }

    #[test]
    fn commits_found_nowhere_else_are_counted_across_the_picked_branches() {
        let f = camp();
        let pick = |names: &[&str]| -> Vec<(OsString, String)> {
            names.iter().map(|name| (OsString::from(name), oid(&f, name))).collect()
        };
        assert_eq!(unique(&f.repo, &pick(&["feature/ford", "feature/bridge"]), true).unwrap(), 0);
        assert_eq!(unique(&f.repo, &pick(&["stale", "wip", "feature/ford"]), true).unwrap(), 2);
        f.git(&["branch", "wip-copy", "wip"]);
        assert_eq!(unique(&f.repo, &pick(&["wip"]), true).unwrap(), 0);
        assert_eq!(unique(&f.repo, &pick(&["wip", "wip-copy"]), true).unwrap(), 2);
        f.git(&["tag", "kept", "stale"]);
        assert_eq!(unique(&f.repo, &pick(&["stale"]), true).unwrap(), 0);
        assert!(unique(&f.repo, &[(OsString::from("--all"), oid(&f, "main"))], true).is_err());
    }

    #[test]
    fn the_prompts_name_the_branches_and_say_what_would_be_lost() {
        let branches: Vec<(OsString, String)> = (1..=10).map(|i| (OsString::from(format!("old/{i}")), "a".repeat(40))).collect();
        let spec = delete_spec(&branches, 3, Some("main"));
        assert_eq!(spec.title, "Delete 10 branches?");
        assert_eq!(spec.confirm.as_ref(), "Delete 10 branches");
        assert!(spec.danger);
        assert_eq!(spec.lines[0], "old/1");
        assert_eq!(spec.lines[7], "old/8");
        assert_eq!(spec.lines[8], "and 2 more");
        assert_eq!(
            spec.lines[9..],
            [
                "3 of them have commits found nowhere else. Those commits will no longer appear in the graph.",
                "main is checked out, so it is kept.",
                "Undo, in the notice that follows, brings them back.",
            ]
        );
        assert_eq!(delete_picked_title((2, 3)), "Delete 2 branches…");
        assert_eq!(delete_picked_title((0, 1)), "Delete 1 branch…");
        assert_eq!(delete_picked_title((1, 1)), "Delete 1 branch…");
        let spec = delete_spec(&branches[..2], 0, None);
        assert_eq!(spec.lines[2], "Their commits are also on other branches or tags.");
        assert_eq!(listing(&["a".into()]), "a");
        assert_eq!(listing(&["a".into(), "b".into(), "c".into()]), "a, b and c");
        let many: Vec<String> = (1..=10).map(|i| format!("b{i}")).collect();
        assert_eq!(listing(&many), "b1, b2, b3, b4, b5, b6, b7, b8 and 2 more");

        let candidate = |name: &str, merged: bool, gone: bool, unmerged: usize| Candidate {
            name: name.into(),
            tip: "b".repeat(40),
            merged,
            gone,
            unmerged,
        };
        let found = [candidate("ford", true, false, 0), candidate("bridge", true, true, 0), candidate("stale", false, true, 2)];
        let spec = merged_spec("main", &found, &[true, true, false]);
        let items: Vec<(&str, &str, bool)> =
            spec.items.iter().map(|item| (item.label.as_str(), item.detail.as_str(), item.ticked)).collect();
        assert_eq!(
            items,
            [
                ("ford", "merged", true),
                ("bridge", "merged · remote branch gone", true),
                ("stale", "remote branch gone · 2 commits not in main", false),
            ]
        );
        assert_eq!(spec.confirm.as_ref(), "Delete 2 branches");
        assert!(!spec.danger);
        assert_eq!(
            spec.lines[0],
            "Local branches merged into main, or whose remote branch is gone. main, master and develop are never listed."
        );
        assert!(merged_spec("trunk", &found, &[true, true, false]).lines[0].ends_with("trunk, main, master and develop are never listed."));
        let spec = merged_spec("main", &found, &[false, false, true]);
        assert_eq!(spec.confirm.as_ref(), "Delete 1 branch");
        assert!(spec.danger);
        assert_eq!(merged_spec("main", &found, &[false, false, false]).confirm.as_ref(), "Delete branches");
    }
}
