use crate::{
    Tab, diagnostics, handoff,
    notice::Notice,
    refs::Kind,
    settings::{self, Memory},
    tabs,
    updates::{self, Version},
    view::{self, Tone, Ui},
};
use gpui::{App, Context, Global, Window};

pub(crate) const WHATS_NEW: &str = "What's new";
pub(crate) const GOT_IT: &str = "Got it";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    Menus,
    Palette,
    Drag,
}

impl Hint {
    pub(crate) const ALL: [Self; 3] = [Self::Menus, Self::Palette, Self::Drag];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Menus => "menus",
            Self::Palette => "palette",
            Self::Drag => "drag",
        }
    }

    pub(crate) fn applies(self, open: bool, branches: usize) -> bool {
        match self {
            Self::Menus | Self::Palette => open,
            Self::Drag => open && branches >= 2,
        }
    }

    pub(crate) fn notice(self) -> Notice {
        let palette = if cfg!(target_os = "macos") { "⌘P" } else { "Ctrl-P" };
        let (title, detail) = match self {
            Self::Menus => (
                "Right-click anything for its actions".to_owned(),
                "Commits, branches, tags, stashes and files each have a menu of everything you can do with them.".to_owned(),
            ),
            Self::Palette => (
                format!("{palette} finds every command"),
                "Press it and type what you want to do, such as fetch, stash or new branch.".to_owned(),
            ),
            Self::Drag => (
                "Drag a branch onto another to merge or rebase".to_owned(),
                "Drop one branch on another in the sidebar or the graph, then choose how to bring them together.".to_owned(),
            ),
        };
        Notice::new(Tone::Info, title, detail).sticky().offer(GOT_IT, view::Action::Guide(Action::Seen(self)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Seen(Hint),
}

#[derive(Default)]
pub(crate) struct State {
    shown: Option<(Hint, u64)>,
}

#[derive(Default)]
struct Waiting(Vec<Notice>);

impl Global for Waiting {}

pub(crate) fn quiet(cx: &App) -> bool {
    cfg!(test)
        || crate::acceptance::replaying()
        || std::env::var_os("FASTKRAKEN_SCENE").is_some()
        || !settings::on_disk(cx)
}

pub(crate) fn fresh(memory: &Memory) -> bool {
    memory.window.is_none()
        && memory.last_repository.is_none()
        && memory.session.is_empty()
        && memory.recents.is_empty()
        && memory.last_version.is_none()
}

pub(crate) fn arrived(last: Option<&str>, fresh: bool, current: Version) -> Option<Version> {
    match last {
        Some(last) => Version::parse(last).filter(|last| *last < current).map(|_| current),
        None => (!fresh).then_some(current),
    }
}

pub(crate) fn hints_at_start(seen: Option<Vec<String>>, fresh: bool) -> Vec<String> {
    match seen {
        Some(seen) => seen,
        None if fresh => Vec::new(),
        None => Hint::ALL.iter().map(|hint| hint.key().to_owned()).collect(),
    }
}

pub(crate) fn release_notes(version: Version) -> String {
    format!("{}tag/v{version}", updates::releases_page())
}

pub(crate) fn updated(version: Version) -> Notice {
    Notice::new(
        Tone::Info,
        format!("Updated to FastKraken {version}"),
        "The release notes on GitHub say what is new and what was fixed.",
    )
    .sticky()
    .offer(WHATS_NEW, view::Action::Handoff(handoff::Action::OpenUrl(release_notes(version))))
}

pub(crate) fn start(cx: &mut App) {
    let memory = settings::memory(cx).cloned().unwrap_or_default();
    let mut waiting = Vec::new();
    if settings::on_disk(cx)
        && let Some(folder) = diagnostics::folder()
        && let Some(report) = diagnostics::unannounced(&folder, memory.report_seen.as_deref())
    {
        let name = report.file_name().map(|name| name.to_string_lossy().into_owned());
        Memory::update(cx, |memory| memory.report_seen = name);
        waiting.push(diagnostics::announcement(&report));
    }
    if !quiet(cx) {
        let fresh = fresh(&memory);
        let current = Version::current();
        if let Some(version) = arrived(memory.last_version.as_deref(), fresh, current) {
            waiting.push(updated(version));
        }
        let hints = hints_at_start(memory.hints_seen.clone(), fresh);
        Memory::update(cx, |memory| {
            memory.last_version = Some(current.to_string());
            memory.hints_seen = Some(hints);
        });
    }
    cx.set_global(Waiting(waiting));
}

pub(crate) fn wait(notice: Notice, cx: &mut App) {
    cx.default_global::<Waiting>().0.push(notice);
}

pub(crate) fn deliver(tab: &mut Tab, cx: &mut App) {
    if cx.try_global::<Waiting>().is_none_or(|waiting| waiting.0.is_empty()) {
        return;
    }
    for notice in std::mem::take(&mut cx.global_mut::<Waiting>().0) {
        tab.ui.notices.show(notice);
    }
}

pub(crate) fn pass_on(cx: &mut App) {
    if cx.try_global::<Waiting>().is_none_or(|waiting| waiting.0.is_empty()) || !tabs::present(cx) {
        return;
    }
    for notice in std::mem::take(&mut cx.global_mut::<Waiting>().0) {
        tabs::ask(cx, tabs::Request::Tell(notice));
    }
}

fn remember(hint: Hint, cx: &mut App) {
    Memory::update(cx, |memory| {
        let seen = memory.hints_seen.get_or_insert_with(Vec::new);
        if !seen.iter().any(|key| key == hint.key()) {
            seen.push(hint.key().to_owned());
        }
    });
}

pub(crate) fn next(seen: &[String], open: bool, branches: usize) -> Option<Hint> {
    Hint::ALL.into_iter().find(|hint| !seen.iter().any(|key| key == hint.key()) && hint.applies(open, branches))
}

fn branches(ui: &Ui) -> usize {
    ui.refs.items.iter().filter(|item| item.kind == Kind::Branch).count()
}

pub(crate) fn offer(tab: &mut Tab, cx: &mut Context<Tab>) {
    if quiet(cx) || !tab.front || tab.repo.is_none() {
        return;
    }
    if let Some((hint, id)) = tab.ui.guide.shown {
        if tab.ui.notices.get(id).is_none() {
            tab.ui.guide.shown = None;
            remember(hint, cx);
        }
        return;
    }
    if tab.ui.dialog_open() || tab.ui.busy() {
        return;
    }
    let Some(seen) = settings::memory(cx).and_then(|memory| memory.hints_seen.clone()) else { return };
    if let Some(hint) = next(&seen, !tab.ui.repo.is_empty(), branches(&tab.ui)) {
        let id = tab.ui.notices.show(hint.notice());
        tab.ui.guide.shown = Some((hint, id));
        cx.notify();
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Seen(hint) => {
            if let Some((_, id)) = tab.ui.guide.shown.take_if(|(shown, _)| *shown == hint) {
                tab.ui.notices.remove(id);
            }
            remember(hint, cx);
        }
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn the_update_notice_shows_once_after_the_version_rises() {
        let current = version("0.9.0");
        assert_eq!(arrived(Some("0.8.0"), false, current), Some(current));
        assert_eq!(arrived(Some("0.8.0"), true, current), Some(current));
        assert_eq!(arrived(Some("0.9.0"), false, current), None);
        assert_eq!(arrived(Some("1.0.0"), false, current), None);
        assert_eq!(arrived(Some("nightly"), false, current), None);
        assert_eq!(arrived(None, true, current), None);
        assert_eq!(arrived(None, false, current), Some(current));
        let shown = updated(current);
        assert_eq!((shown.tone, shown.title.as_str()), (Tone::Info, "Updated to FastKraken 0.9.0"));
        let [(label, view::Action::Handoff(handoff::Action::OpenUrl(url)))] = shown.actions.as_slice() else {
            panic!("{:?}", shown.actions);
        };
        assert_eq!((label.as_ref(), url.as_str()), (WHATS_NEW, "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0"));
        assert!(handoff::openable(url));
    }

    #[test]
    fn a_first_run_starts_with_every_hint_to_show_and_an_earlier_user_with_none() {
        let mut memory = Memory::default();
        assert!(fresh(&memory));
        assert!(hints_at_start(None, true).is_empty());
        assert_eq!(hints_at_start(None, false), ["menus", "palette", "drag"]);
        assert_eq!(hints_at_start(Some(vec!["menus".into()]), false), ["menus"]);
        memory.add_recent(std::path::Path::new("/tmp/Cedar"), 1_795_000_000);
        assert!(!fresh(&memory));
        let mut memory = Memory::default();
        memory.last_version = Some("0.9.0".into());
        assert!(!fresh(&memory));
    }

    #[test]
    fn hints_come_one_at_a_time_where_they_apply() {
        let seen = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
        assert_eq!(next(&[], false, 3), None);
        assert_eq!(next(&[], true, 1), Some(Hint::Menus));
        assert_eq!(next(&seen(&["menus"]), true, 1), Some(Hint::Palette));
        assert_eq!(next(&seen(&["menus", "palette"]), true, 1), None);
        assert_eq!(next(&seen(&["menus", "palette"]), true, 2), Some(Hint::Drag));
        assert_eq!(next(&seen(&["drag", "palette"]), true, 2), Some(Hint::Menus));
        assert_eq!(next(&seen(&["menus", "palette", "drag"]), true, 5), None);
        for hint in Hint::ALL {
            let shown = hint.notice();
            assert_eq!(shown.tone, Tone::Info);
            let [(label, view::Action::Guide(Action::Seen(seen)))] = shown.actions.as_slice() else {
                panic!("{:?}", shown.actions);
            };
            assert_eq!((label.as_ref(), *seen), (GOT_IT, hint));
        }
        assert_eq!(Hint::Palette.notice().title, "⌘P finds every command");
        assert_eq!(crate::commands::find("app.palette").unwrap().shortcut, Some("cmd-p"));
    }
}
