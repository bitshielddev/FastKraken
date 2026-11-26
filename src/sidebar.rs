use crate::{
    Tab,
    commands::{Availability, Bar, Command, Group, Target, on_repo},
    picked::{How, ListId, Picked},
    refs::{Kind, Section, SideRow},
    settings::Settings,
    view::{self, Ui},
};
use gpui::{Context, Focusable, Window};
use std::{ffi::OsString, os::unix::ffi::OsStrExt};

pub(crate) const SECTIONS: [Section; 4] = [Section::Branches, Section::Remotes, Section::Tags, Section::Stashes];

#[derive(Clone, Debug)]
pub(crate) enum Action {
    FocusFilter,
    ClearFilter,
    ToggleShown(Section),
    Pick { name: OsString, how: Option<How> },
    PickFolder(String),
    Unpick,
}

pub(crate) fn title(section: Section) -> &'static str {
    match section {
        Section::Branches => "Branches",
        Section::Remotes => "Remotes",
        Section::Tags => "Tags",
        Section::Stashes => "Stashes",
        Section::Worktrees => "Worktrees",
    }
}

pub(crate) fn repo_rows(section: Section) -> &'static [&'static str] {
    match section {
        Section::Branches => &["repo.new-branch"],
        Section::Remotes => &["repo.add-remote", "repo.fetch-all", "repo.prune-now"],
        Section::Tags => &["repo.new-tag"],
        Section::Stashes => &["repo.stash"],
        Section::Worktrees => &["worktree.new", "worktree.prune"],
    }
}

pub(crate) fn shown(ui: &Ui, section: Section) -> bool {
    !ui.refs.hidden_sections.contains(&section)
}

fn shown_title(ui: &Ui, section: Section) -> String {
    let verb = if shown(ui, section) { "Hide" } else { "Show" };
    format!("{verb} the {} section", title(section))
}

fn visibility(ui: &Ui, target: &Target) -> Availability {
    if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
}

pub(crate) const VISIBILITY: [&str; 4] = ["sidebar.branches", "sidebar.remotes", "sidebar.tags", "sidebar.stashes"];

pub(crate) fn bar_items(ui: &Ui) -> Vec<gpui::MenuItem> {
    VISIBILITY
        .iter()
        .filter_map(|id| crate::commands::find(id))
        .map(|command| {
            let name = crate::commands::title_case(&(command.title)(ui, &Target::Repo));
            gpui::MenuItem::action(name, crate::commands::Run { id: command.id })
        })
        .collect()
}

pub(crate) fn bar_titles(ui: &Ui) -> Vec<String> {
    SECTIONS.iter().map(|section| shown_title(ui, *section)).collect()
}

pub(crate) fn locals(ui: &Ui) -> Vec<OsString> {
    ui.refs.items.iter().filter(|item| item.kind == Kind::Branch).map(|item| item.name.clone()).collect()
}

pub(crate) fn listed_branches(ui: &Ui) -> Vec<OsString> {
    ui.refs
        .rows
        .iter()
        .filter_map(|row| match row {
            SideRow::Ref { index, .. } => ui.refs.items.get(*index).filter(|item| item.kind == Kind::Branch),
            _ => None,
        })
        .map(|item| item.name.clone())
        .collect()
}

pub(crate) fn in_folder(ui: &Ui, prefix: &str) -> Vec<OsString> {
    locals(ui).into_iter().filter(|name| name.as_bytes().starts_with(prefix.as_bytes())).collect()
}

pub(crate) fn folder_prefix(key: &str) -> Option<String> {
    key.strip_prefix("Branches:").map(|folder| format!("{folder}/"))
}

pub(crate) fn picked(ui: &Ui) -> Option<&Picked<OsString>> {
    ui.refs.picked.as_ref().filter(|picked| picked.len() > 1)
}

pub(crate) fn is_picked(ui: &Ui, name: &OsString) -> bool {
    picked(ui).is_some_and(|picked| picked.contains(name))
}

pub(crate) fn menu_target(ui: &Ui, name: &OsString) -> Target {
    match picked(ui).filter(|picked| picked.contains(name)) {
        Some(picked) => Target::Branches(picked.ordered(&locals(ui))),
        None => Target::Branch(name.clone()),
    }
}

pub(crate) fn retain(ui: &mut Ui) {
    let Some(mut picked) = ui.refs.picked.take() else { return };
    let locals = locals(ui);
    if picked.retain(&locals) {
        ui.refs.picked = Some(picked);
    }
    if ui.refs.primary.as_ref().is_some_and(|primary| !locals.contains(primary)) {
        ui.refs.primary = None;
    }
}

pub(crate) fn picked_folder(picked: Option<&Picked<OsString>>, inside: &[OsString]) -> Option<Picked<OsString>> {
    let first = inside.first()?;
    match picked {
        Some(current) if current.list == ListId::Branches && inside.iter().all(|name| current.contains(name)) => {
            let mut next = current.clone();
            next.keys.retain(|name| !inside.contains(name));
            let anchor = next.keys.iter().next()?.clone();
            if !next.keys.contains(&next.anchor) {
                next.anchor = anchor;
            }
            Some(next)
        }
        Some(current) if current.list == ListId::Branches => {
            let mut next = current.clone();
            next.keys.extend(inside.iter().cloned());
            next.anchor = first.clone();
            Some(next)
        }
        _ => Some(Picked { list: ListId::Branches, anchor: first.clone(), keys: inside.iter().cloned().collect() }),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::FocusFilter => {
            if tab.repo.is_none() {
                return;
            }
            if tab.ui.sidebar_hidden {
                view::Desk::dispatch(tab, view::Action::ToggleSidebar, window, cx);
            }
            tab.ui.refs.list_open = false;
            let filter = tab.ui.refs.filter.clone();
            window.focus(&filter.focus_handle(cx));
        }
        Action::ClearFilter => {
            let filter = tab.ui.refs.filter.clone();
            filter.update(cx, |field, cx| field.clear(cx));
            window.focus(&filter.focus_handle(cx));
        }
        Action::ToggleShown(section) => {
            let hidden = &mut tab.ui.refs.hidden_sections;
            match hidden.iter().position(|shown| *shown == section) {
                Some(at) => {
                    hidden.remove(at);
                }
                None => hidden.push(section),
            }
            hidden.sort_by_key(|section| SECTIONS.iter().position(|s| s == section));
            let chosen = hidden.clone();
            Settings::update(cx, |settings| settings.hidden_sections = chosen);
        }
        Action::Pick { name, how: None } => {
            tab.ui.refs.picked = tab.ui.refs.local(&name).map(|_| Picked::one(ListId::Branches, name.clone()));
            tab.ui.refs.primary = Some(name);
        }
        Action::Pick { name, how: Some(how) } => {
            let mut rows = listed_branches(&tab.ui);
            if !rows.contains(&name) {
                rows = locals(&tab.ui);
            }
            let primary = tab.ui.refs.primary.clone().filter(|primary| rows.contains(primary));
            let current = tab.ui.refs.picked.as_ref().filter(|_| primary.is_some());
            if let Some((picked, primary)) = Picked::clicked(current, primary.as_ref(), ListId::Branches, &rows, &name, how) {
                tab.ui.refs.picked = Some(picked);
                tab.ui.refs.primary = Some(primary);
            }
        }
        Action::PickFolder(prefix) => {
            let inside = in_folder(&tab.ui, &prefix);
            let picked = picked_folder(tab.ui.refs.picked.as_ref(), &inside);
            tab.ui.refs.primary = picked.as_ref().map(|picked| picked.anchor.clone());
            tab.ui.refs.picked = picked;
        }
        Action::Unpick => {
            tab.ui.refs.picked = None;
        }
    }
    cx.notify();
}

fn on_section(target: &Target) -> Option<Section> {
    match target {
        Target::Section(section) => Some(*section),
        _ => None,
    }
}

fn folding(ui: &Ui, target: &Target) -> Option<(Section, bool)> {
    let section = on_section(target)?;
    let keys = crate::refs::folder_keys(&ui.refs, section);
    if keys.is_empty() || ui.repo.is_empty() {
        return None;
    }
    Some((section, keys.iter().any(|key| !ui.refs.folded(key))))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "view.filter-sidebar",
        Group::View,
        |_, _| "Filter the sidebar".into(),
        |ui, target, _| visibility(ui, target),
        |_, _| Some(view::Action::Sidebar(Action::FocusFilter)),
    )
    .words(&["find branch", "search branches", "filter branches", "sidebar search"])
    .icon("search")
    .keys("alt-cmd-f")
    .bar(Bar::View),
    Command::new(
        "sidebar.collapse-all",
        Group::View,
        |ui, target| match folding(ui, target) {
            Some((_, true)) => "Collapse all".into(),
            _ => "Expand all".into(),
        },
        |ui, target, _| if folding(ui, target).is_some() { Availability::Enabled } else { Availability::Hidden },
        |ui, target| {
            let (section, fold) = folding(ui, target)?;
            Some(view::Action::Refs(crate::refs::Action::FoldAll { section, fold }))
        },
    )
    .icon("chevron-right"),
    Command::new(
        "sidebar.hide-section",
        Group::View,
        |_, _| "Hide this section".into(),
        |ui, target, _| match on_section(target) {
            Some(_) if !ui.repo.is_empty() => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| Some(view::Action::Sidebar(Action::ToggleShown(on_section(target)?))),
    )
    .icon("eye-off"),
    Command::new(
        "sidebar.branches",
        Group::View,
        |ui, _| shown_title(ui, Section::Branches),
        |ui, target, _| visibility(ui, target),
        |_, _| Some(view::Action::Sidebar(Action::ToggleShown(Section::Branches))),
    )
    .words(&["sidebar sections", "local branches"]),
    Command::new(
        "sidebar.remotes",
        Group::View,
        |ui, _| shown_title(ui, Section::Remotes),
        |ui, target, _| visibility(ui, target),
        |_, _| Some(view::Action::Sidebar(Action::ToggleShown(Section::Remotes))),
    )
    .words(&["sidebar sections", "remote branches"]),
    Command::new(
        "sidebar.tags",
        Group::View,
        |ui, _| shown_title(ui, Section::Tags),
        |ui, target, _| visibility(ui, target),
        |_, _| Some(view::Action::Sidebar(Action::ToggleShown(Section::Tags))),
    )
    .words(&["sidebar sections"]),
    Command::new(
        "sidebar.stashes",
        Group::View,
        |ui, _| shown_title(ui, Section::Stashes),
        |ui, target, _| visibility(ui, target),
        |_, _| Some(view::Action::Sidebar(Action::ToggleShown(Section::Stashes))),
    )
    .words(&["sidebar sections"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn keys(picked: &Picked<OsString>) -> Vec<String> {
        picked.keys.iter().map(|key| key.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn picking_a_folder_adds_every_branch_inside_and_picking_it_again_takes_them_out() {
        let inside = names(&["feature/a", "feature/b"]);
        let fresh = picked_folder(None, &inside).unwrap();
        assert_eq!((keys(&fresh), fresh.anchor.clone()), (vec!["feature/a".to_owned(), "feature/b".to_owned()], "feature/a".into()));
        let with_main = Picked { list: ListId::Branches, anchor: "main".into(), keys: BTreeSet::from(["main".into()]) };
        let grown = picked_folder(Some(&with_main), &inside).unwrap();
        assert_eq!(keys(&grown), ["feature/a", "feature/b", "main"]);
        let shrunk = picked_folder(Some(&grown), &inside).unwrap();
        assert_eq!((keys(&shrunk), shrunk.anchor.clone()), (vec!["main".to_owned()], "main".into()));
        assert!(picked_folder(Some(&fresh), &inside).is_none());
        assert!(picked_folder(Some(&fresh), &[]).is_none());
        let files = Picked { list: ListId::Staged, anchor: "x".into(), keys: BTreeSet::from(["x".into()]) };
        assert_eq!(keys(&picked_folder(Some(&files), &inside).unwrap()), ["feature/a", "feature/b"]);
    }

    #[test]
    fn a_folder_key_names_the_branches_under_it_and_only_local_folders_pick() {
        assert_eq!(folder_prefix("Branches:feature/ui").as_deref(), Some("feature/ui/"));
        assert_eq!(folder_prefix("Remotes:origin"), None);
        assert_eq!(folder_prefix("Tags:v1"), None);
        assert_eq!(repo_rows(Section::Remotes), ["repo.add-remote", "repo.fetch-all", "repo.prune-now"]);
        for section in SECTIONS {
            for id in repo_rows(section) {
                assert!(crate::commands::find(id).is_some(), "{id}");
            }
        }
    }
}
