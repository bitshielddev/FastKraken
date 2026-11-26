use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Target},
    notice::{self, Notice},
    repository::{Repository, checked, raw_path, same_place, validate_oid},
    theme::*,
    view::{self, Desk, Menu, Tone, Ui},
};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, Window, div, prelude::*, px, rgb,
};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Worktree {
    pub(crate) path: PathBuf,
    pub(crate) head: Option<String>,
    pub(crate) branch: Option<OsString>,
    pub(crate) locked: Option<String>,
    pub(crate) prunable: Option<String>,
    pub(crate) current: bool,
}

impl Worktree {
    pub(crate) fn name(&self) -> String {
        self.path
            .file_name()
            .map_or_else(|| self.path.display().to_string(), |name| name.to_string_lossy().into_owned())
    }

    pub(crate) fn on(&self) -> String {
        match (&self.branch, &self.head) {
            (Some(branch), _) => branch.to_string_lossy().into_owned(),
            (None, Some(head)) => format!("detached at {}", notice::short(head)),
            (None, None) => "no commits yet".to_owned(),
        }
    }
}

pub(crate) fn parse(output: &[u8], root: &Path) -> Vec<Worktree> {
    let mut found = Vec::new();
    let mut open: Option<(Worktree, bool)> = None;
    let finish = |open: Option<(Worktree, bool)>, found: &mut Vec<Worktree>| {
        if let Some((worktree, false)) = open {
            found.push(worktree);
        }
    };
    for field in output.split(|&b| b == 0) {
        if field.is_empty() {
            finish(open.take(), &mut found);
            continue;
        }
        let (key, value) = match field.iter().position(|&b| b == b' ') {
            Some(at) => (&field[..at], Some(&field[at + 1..])),
            None => (field, None),
        };
        if key == b"worktree" {
            finish(open.take(), &mut found);
            let Some(path) = value.filter(|path| !path.is_empty()) else { continue };
            let path = raw_path(path);
            let current = same_place(&path, root);
            open = Some((Worktree { path, current, ..Worktree::default() }, false));
            continue;
        }
        let Some((worktree, bare)) = open.as_mut() else { continue };
        let text = || value.map(|value| String::from_utf8_lossy(value).into_owned()).unwrap_or_default();
        match key {
            b"HEAD" => {
                let oid = text();
                if validate_oid(&oid).is_ok() && oid.bytes().any(|b| b != b'0') {
                    worktree.head = Some(oid);
                }
            }
            b"branch" => {
                worktree.branch = value
                    .and_then(|full| full.strip_prefix(b"refs/heads/"))
                    .filter(|name| !name.is_empty())
                    .map(|name| OsStr::from_bytes(name).to_owned());
            }
            b"bare" => *bare = true,
            b"locked" => worktree.locked = Some(text()),
            b"prunable" => worktree.prunable = Some(text()),
            _ => {}
        }
    }
    finish(open.take(), &mut found);
    found
}

pub(crate) fn list(repo: &Repository) -> Result<Vec<Worktree>, String> {
    let output = checked(repo.read_command().args(["worktree", "list", "--porcelain", "-z"]))?;
    Ok(parse(&output, &repo.root))
}

pub(crate) fn uncommitted(repo: &Repository, path: &Path) -> Result<usize, String> {
    if !path.is_dir() {
        return Err(format!("{} is missing", path.display()));
    }
    let output = checked(
        repo.read_command()
            .arg("-C")
            .arg(path)
            .args(["status", "--porcelain=v2", "--untracked-files=normal", "--ignore-submodules=none"]),
    )?;
    Ok(output.split(|&b| b == b'\n').filter(|line| !line.is_empty() && !line.starts_with(b"#")).count())
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) list: Vec<Worktree>,
    ticket: u64,
    counts: HashMap<PathBuf, Result<usize, String>>,
    asked: bool,
}

impl State {
    pub(crate) fn linked(&self) -> bool {
        self.list.len() > 1
    }

    pub(crate) fn others(&self) -> impl Iterator<Item = &Worktree> {
        self.list.iter().filter(|worktree| !worktree.current)
    }

    pub(crate) fn find(&self, path: &Path) -> Option<&Worktree> {
        self.list.iter().find(|worktree| worktree.path == path)
    }

    pub(crate) fn holding(&self, branch: &OsStr) -> Option<&Worktree> {
        self.others().find(|worktree| worktree.branch.as_deref() == Some(branch))
    }

    pub(crate) fn at<'a>(&'a self, oid: &'a str) -> impl Iterator<Item = &'a Worktree> + 'a {
        self.others().filter(move |worktree| worktree.head.as_deref() == Some(oid))
    }

    pub(crate) fn count(&self, path: &Path) -> Option<&Result<usize, String>> {
        self.counts.get(path)
    }

    pub(crate) fn wanted(&self) -> bool {
        !self.asked && self.others().next().is_some()
    }
}

pub(crate) fn in_use(ui: &Ui, branch: &OsStr) -> Option<String> {
    ui.worktree.holding(branch).map(|worktree| format!("Checked out in worktree {}", worktree.name()))
}

pub(crate) fn changes_line(state: &State, worktree: &Worktree) -> String {
    match state.count(&worktree.path) {
        Some(Ok(0)) => "No uncommitted changes".to_owned(),
        Some(Ok(1)) => "1 uncommitted change".to_owned(),
        Some(Ok(count)) => format!("{count} uncommitted changes"),
        Some(Err(_)) => "Its changes could not be counted".to_owned(),
        None => "Counting uncommitted changes…".to_owned(),
    }
}

pub(crate) fn tip(state: &State, worktree: &Worktree) -> String {
    let mut lines = vec![worktree.path.display().to_string(), format!("On {}", worktree.on())];
    if worktree.current {
        lines.push("Open in this tab".to_owned());
    } else {
        lines.push(changes_line(state, worktree));
    }
    match worktree.locked.as_deref() {
        Some("") => lines.push("Locked".to_owned()),
        Some(reason) => lines.push(format!("Locked: {reason}")),
        None => {}
    }
    if worktree.prunable.is_some() {
        lines.push("Its folder is missing".to_owned());
    }
    lines.join("\n")
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(PathBuf),
    Count,
}

fn holder<'a>(ui: &'a Ui, target: &Target) -> Option<&'a Worktree> {
    match target {
        Target::Branch(name) => ui.worktree.holding(name),
        Target::RemoteBranch(name) => {
            let (_, local) = crate::branch::split_remote(&ui.refs.remotes, name)?;
            ui.worktree.holding(&local)
        }
        _ => None,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "worktree.open",
        Group::Switch,
        |_, _| "Open in a new tab".into(),
        |ui, target, _| {
            let Target::Worktree(path) = target else { return Availability::Hidden };
            match ui.worktree.find(path) {
                None => Availability::Hidden,
                Some(worktree) if worktree.current => Availability::Disabled("This worktree is open in this tab".to_owned()),
                Some(worktree) if worktree.prunable.is_some() => Availability::Disabled("Its folder is missing".to_owned()),
                Some(_) => Availability::Enabled,
            }
        },
        |_, target| match target {
            Target::Worktree(path) => Some(view::Action::Worktree(Action::Open(path.clone()))),
            _ => None,
        },
    )
    .icon("folder"),
    Command::new(
        "branch.open-worktree",
        Group::Switch,
        |_, _| "Open that worktree".into(),
        |ui, target, _| match holder(ui, target) {
            None => Availability::Hidden,
            Some(worktree) if worktree.prunable.is_some() => {
                Availability::Disabled(format!("The folder of worktree {} is missing", worktree.name()))
            }
            Some(_) => Availability::Enabled,
        },
        |ui, target| holder(ui, target).map(|worktree| view::Action::Worktree(Action::Open(worktree.path.clone()))),
    )
    .icon("folder"),
];

pub(crate) fn refused_switch(tab: &mut Tab, branch: &OsStr, cx: &mut Context<Tab>) -> bool {
    let Some(worktree) = tab.ui.worktree.holding(branch) else { return false };
    let notice = Notice::new(
        Tone::Info,
        format!("{} is checked out in worktree {}", branch.to_string_lossy(), worktree.name()),
        "Git keeps a branch in one worktree at a time.",
    )
    .offer("Open that worktree", view::Action::Worktree(Action::Open(worktree.path.clone())));
    tab.ui.notices.show(notice);
    cx.notify();
    true
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => open(tab, path, cx),
        Action::Count => count(tab, cx),
    }
}

fn open(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    let listed = &tab.ui.worktree;
    let found = listed.find(&path).or_else(|| listed.list.iter().find(|worktree| same_place(&worktree.path, &path)));
    let Some(worktree) = found.cloned() else {
        tab.ui.say(Tone::Info, "That worktree is no longer listed");
        cx.notify();
        return;
    };
    if worktree.current {
        tab.ui.say(Tone::Info, "This worktree is open in this tab");
        cx.notify();
        return;
    }
    if worktree.prunable.is_some() || !worktree.path.is_dir() {
        tab.ui.notices.show(Notice::new(
            Tone::Error,
            format!("The folder of worktree {} is missing", worktree.name()),
            worktree.path.display().to_string(),
        ));
        cx.notify();
        return;
    }
    if !crate::tabs::open_elsewhere(worktree.path.clone(), |_, _| {}, cx) {
        tab.open(worktree.path, cx);
    }
}

pub(crate) fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    tab.ui.worktree.ticket += 1;
    let ticket = tab.ui.worktree.ticket;
    Tab::work(
        cx,
        move || list(&repo),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.worktree.ticket != ticket {
                return;
            }
            let state = &mut tab.ui.worktree;
            state.list = result.unwrap_or_default();
            state.counts.clear();
            state.asked = false;
            cx.notify();
        },
    );
}

fn count(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.worktree;
    if !state.wanted() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    state.asked = true;
    let paths: Vec<PathBuf> = state.others().map(|worktree| worktree.path.clone()).collect();
    let (epoch, ticket) = (tab.epoch, state.ticket);
    Tab::work(
        cx,
        move || paths.into_iter().map(|path| {
            let counted = uncommitted(&repo, &path);
            (path, counted)
        }).collect::<Vec<_>>(),
        move |tab, counted, cx| {
            if tab.epoch != epoch || tab.ui.worktree.ticket != ticket {
                return;
            }
            tab.ui.worktree.counts.extend(counted);
            cx.notify();
        },
    );
}

pub(crate) fn side_row<T: Desk>(ui: &Ui, index: usize, row: usize, pointed: bool, cx: &mut Context<T>) -> AnyElement {
    let Some(worktree) = ui.worktree.list.get(index) else { return div().into_any_element() };
    let current = worktree.current;
    let (opened, menu_path) = (worktree.path.clone(), worktree.path.clone());
    let head = worktree.head.clone();
    let shown = matches!((&ui.selection, &worktree.head), (Some(view::Selection::Commit(id)), Some(oid)) if id == oid);
    let dirty = if current { ui.changed_count() > 0 } else { matches!(ui.worktree.count(&worktree.path), Some(Ok(count)) if *count > 0) };
    kit::row(("side-worktree", row), false)
        .h(px(TREE_ROW))
        .pl(px(12.))
        .pr(px(10.))
        .gap(px(6.))
        .child(
            div()
                .w(px(11.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .when(current, |d| d.child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))),
        )
        .child(
            kit::fit(worktree.name())
                .size(12.5)
                .color(if current || shown { TEXT } else { TEXT_2 })
                .weight(if current { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .grow(),
        )
        .when(dirty, |d| d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(rgb(MODIFIED))))
        .when(worktree.locked.is_some(), |d| d.child(kit::icon("lock").size(px(10.)).text_color(rgb(TEXT_3))))
        .when(worktree.prunable.is_some(), |d| d.child(kit::label("missing")))
        .child(div().max_w(px(110.)).flex_shrink_0().child(kit::fit(worktree.on()).size(TEXT_SMALL).color(TEXT_3)))
        .when(pointed, |d| d.tooltip(kit::tip(tip(&ui.worktree, worktree))))
        .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
            if event.click_count() > 1 && !current {
                app.dispatch(view::Action::Worktree(Action::Open(opened.clone())), w, cx);
            } else if let Some(oid) = head.clone() {
                app.dispatch(view::Action::Reveal(crate::reveal::Action::Commit(oid)), w, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                let target = Target::Worktree(menu_path.clone());
                app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
            }),
        )
        .into_any_element()
}

pub(crate) fn chips<T: Desk>(ui: &Ui, oid: &str, cx: &Context<T>) -> Vec<AnyElement> {
    ui.worktree
        .at(oid)
        .enumerate()
        .map(|(n, worktree)| {
            let (opened, menu_path) = (worktree.path.clone(), worktree.path.clone());
            kit::chip_marked(kit::Chip::Plain, worktree.name(), &["folder"])
                .id(("worktree-chip", n))
                .cursor_pointer()
                .tooltip(kit::tip(tip(&ui.worktree, worktree)))
                .on_click(cx.listener(move |app, event: &ClickEvent, w, cx| {
                    if event.click_count() > 1 {
                        cx.stop_propagation();
                        app.dispatch(view::Action::Worktree(Action::Open(opened.clone())), w, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                        cx.stop_propagation();
                        let target = Target::Worktree(menu_path.clone());
                        app.dispatch(view::Action::Menu(Some(Menu::Object { target, at: event.position })), w, cx)
                    }),
                )
                .into_any_element()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn commit(f: &Fixture, name: &str, text: &str, subject: &str) -> String {
        fs::write(f.dir.join(name), text).unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", subject]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn snapshot(f: &Fixture) -> Vec<u8> {
        [
            f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            f.git(&["ls-files", "--stage"]),
            f.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
            f.git(&["worktree", "list", "--porcelain"]),
        ]
        .concat()
    }

    #[test]
    fn worktrees_are_listed_with_their_branch_head_and_flags_and_the_open_one_is_marked() {
        let f = Fixture::new();
        let start = commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["branch", "ridge"]);
        let ridge = f.dir.join("ridge-tree");
        let summit = f.dir.join("summit-tree");
        let gone = f.dir.join("gone-tree");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        f.git(&["worktree", "add", "-q", "--detach", summit.to_str().unwrap(), "HEAD"]);
        f.git(&["worktree", "add", "-q", "-b", "gone", gone.to_str().unwrap()]);
        f.git(&["worktree", "lock", "--reason", "On the backup disk", summit.to_str().unwrap()]);
        fs::remove_dir_all(&gone).unwrap();
        let before = snapshot(&f);
        let listed = list(&f.repo).unwrap();
        assert_eq!(snapshot(&f), before);
        let main = f.dir.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(listed[0].name(), main);
        assert!(listed[0].current && listed.iter().skip(1).all(|worktree| !worktree.current));
        assert_eq!(listed[0].branch.as_deref(), Some(OsStr::new("main")));
        let named = |name: &str| listed.iter().find(|worktree| worktree.name() == name).cloned().unwrap();
        assert_eq!(listed.len(), 4);
        let (ridge_row, summit_row, gone_row) = (named("ridge-tree"), named("summit-tree"), named("gone-tree"));
        assert_eq!(ridge_row.branch.as_deref(), Some(OsStr::new("ridge")));
        assert_eq!(ridge_row.head.as_deref(), Some(start.as_str()));
        assert!(same_place(&ridge_row.path, &ridge));
        assert_eq!((summit_row.branch.clone(), summit_row.on()), (None, format!("detached at {}", notice::short(&start))));
        assert_eq!(summit_row.locked.as_deref(), Some("On the backup disk"));
        assert!(gone_row.prunable.is_some() && ridge_row.prunable.is_none() && ridge_row.locked.is_none());
        let from_ridge = list(&f.open(&ridge)).unwrap();
        assert!(from_ridge.iter().all(|worktree| worktree.current == (worktree.name() == "ridge-tree")));
    }

    #[test]
    fn a_bare_main_repository_and_an_unborn_worktree_are_read_from_porcelain() {
        let root = Path::new("/trail/ridge");
        let oid = "a".repeat(40);
        let zero = "0".repeat(40);
        let output = format!(
            "worktree /trail/bare.git\0bare\0\0worktree /trail/ridge\0HEAD {oid}\0branch refs/heads/ridge\0locked\0\0worktree /trail/new\0HEAD {zero}\0branch refs/heads/fresh\0prunable gitdir file points to non-existent location\0\0"
        );
        let listed = parse(output.as_bytes(), root);
        assert_eq!(listed.len(), 2);
        assert!(listed[0].current);
        assert_eq!(listed[0].locked.as_deref(), Some(""));
        assert_eq!(listed[1].head, None);
        assert_eq!(listed[1].on(), "fresh");
        assert_eq!(listed[1].prunable.as_deref(), Some("gitdir file points to non-existent location"));
        let state = State { list: listed, ..State::default() };
        assert_eq!(tip(&state, &state.list[0]), "/trail/ridge\nOn ridge\nOpen in this tab\nLocked");
        assert_eq!(tip(&state, &state.list[1]), "/trail/new\nOn fresh\nCounting uncommitted changes…\nIts folder is missing");
        assert_eq!(state.holding(OsStr::new("fresh")).map(Worktree::name).as_deref(), Some("new"));
        assert!(state.holding(OsStr::new("ridge")).is_none());
    }

    #[test]
    fn a_branch_taken_by_another_worktree_after_the_list_was_read_is_refused_by_git_and_explained() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        f.git(&["branch", "ridge"]);
        let state = State { list: list(&f.repo).unwrap(), ..State::default() };
        assert!(state.holding(OsStr::new("ridge")).is_none());
        let ridge = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", ridge.to_str().unwrap(), "ridge"]);
        let before = snapshot(&f);
        let refused = f.repo.command().args(["switch", "--quiet", "--no-guess", "--", "ridge"]).output().unwrap();
        assert!(!refused.status.success());
        assert_eq!(snapshot(&f), before);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
        let said = String::from_utf8_lossy(&refused.stderr).into_owned();
        let Some(crate::explain::Cause::InWorktree { branch, path: Some(path) }) = crate::explain::classify(&said) else {
            panic!("{said}")
        };
        assert_eq!(branch.as_deref(), Some("ridge"));
        let fresh = State { list: list(&f.repo).unwrap(), ..State::default() };
        let holder = fresh.holding(OsStr::new("ridge")).unwrap();
        assert!(same_place(&holder.path, Path::new(&path)));
        assert_eq!(holder.name(), "ridge-tree");
    }

    #[test]
    fn uncommitted_changes_are_counted_in_the_other_worktree_without_touching_it() {
        let f = Fixture::new();
        commit(&f, "trail.txt", "Trail\n", "Start the trail");
        commit(&f, "camp.txt", "Camp\n", "Pitch the camp");
        let ridge = f.dir.join("ridge-tree");
        f.git(&["worktree", "add", "-q", "-b", "ridge", ridge.to_str().unwrap()]);
        assert_eq!(uncommitted(&f.repo, &ridge).unwrap(), 0);
        fs::write(ridge.join("trail.txt"), "Trail, edited\n").unwrap();
        fs::write(ridge.join("lake.txt"), "Lake\n").unwrap();
        fs::remove_file(ridge.join("camp.txt")).unwrap();
        let linked = f.open(&ridge);
        let before = (snapshot(&f), checked(linked.command().args(["status", "--porcelain=v2"])).unwrap());
        assert_eq!(uncommitted(&f.repo, &ridge).unwrap(), 3);
        assert_eq!(uncommitted(&f.repo, &f.dir).unwrap(), 1);
        assert_eq!((snapshot(&f), checked(linked.command().args(["status", "--porcelain=v2"])).unwrap()), before);
        assert!(uncommitted(&f.repo, &f.dir.join("missing-tree")).is_err());
        let mut state = State { list: list(&f.repo).unwrap(), ..State::default() };
        assert!(state.wanted());
        let other = state.holding(OsStr::new("ridge")).unwrap().clone();
        state.counts.insert(other.path.clone(), Ok(3));
        state.asked = true;
        assert!(!state.wanted());
        assert_eq!(changes_line(&state, &other), "3 uncommitted changes");
        assert_eq!(state.at(other.head.as_deref().unwrap()).count(), 1);
    }
}
