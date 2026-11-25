use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Target, on_repo},
    graph_view::{BRANCHES, REMOTES, full_name},
    view::{self, Label, Ui},
};
use gpui::{Context, Window};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Pin {
    #[default]
    Head,
    Off,
    Ref(String),
}

#[derive(Clone, Debug)]
pub enum Action {
    Pin(String),
    Unpin,
    Jump,
}

pub(crate) fn pinnable(name: &str) -> bool {
    [BRANCHES, REMOTES].iter().any(|prefix| name.strip_prefix(prefix).is_some_and(|rest| !rest.is_empty() && !rest.ends_with('/')))
        && crate::graph_view::hideable(name)
}

fn target_of(name: &str) -> Option<Target> {
    if let Some(branch) = name.strip_prefix(BRANCHES) {
        Some(Target::Branch(branch.into()))
    } else {
        name.strip_prefix(REMOTES).map(|remote| Target::RemoteBranch(remote.into()))
    }
}

pub(crate) fn tip(ui: &Ui) -> Option<String> {
    match &ui.graph_view.pin {
        Pin::Off => None,
        Pin::Head => ui.head.clone(),
        Pin::Ref(name) => ui.refs.oid_of(&target_of(name)?).map(str::to_owned),
    }
}

pub(crate) fn active(ui: &Ui) -> bool {
    match &ui.graph_view.pin {
        Pin::Off => false,
        Pin::Head => true,
        Pin::Ref(name) => tip(ui).is_some() && !crate::graph_view::left_out(ui, name),
    }
}

pub(crate) fn starts(ui: &Ui, decorations: &str) -> bool {
    if decorations.is_empty() {
        return false;
    }
    let labels = view::labels(&ui.refs.remotes, decorations);
    match &ui.graph_view.pin {
        Pin::Off => false,
        Pin::Head => labels.iter().any(|label| label.chip == kit::Chip::Head),
        Pin::Ref(name) => labels.iter().any(|label| names(label, name)),
    }
}

fn names(label: &Label, name: &str) -> bool {
    label.target.as_ref().and_then(full_name).as_deref() == Some(name)
        || label.remote.as_ref().is_some_and(|remote| name.strip_prefix(REMOTES) == Some(remote.as_str()))
}

pub(crate) fn marked(ui: &Ui, label: &Label) -> bool {
    match &ui.graph_view.pin {
        Pin::Ref(name) => names(label, name),
        _ => false,
    }
}

fn pinned_here(ui: &Ui, name: &str) -> bool {
    match &ui.graph_view.pin {
        Pin::Off => false,
        Pin::Head => crate::graph_view::current(ui).is_some_and(|branch| name.strip_prefix(BRANCHES) == Some(branch)),
        Pin::Ref(pinned) => pinned == name,
    }
}

pub(crate) fn refs_loaded(ui: &mut Ui) {
    if ui.graph_reserved() != active(ui) {
        ui.relane();
    }
}

fn change(tab: &mut Tab, cx: &mut Context<Tab>, pin: Pin) {
    if tab.ui.graph_view.pin == pin {
        return;
    }
    tab.ui.graph_view.pin = pin;
    tab.ui.relane();
    crate::graph_view::remember(tab, cx);
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Pin(name) => {
            if !pinnable(&name) {
                tab.error(format!("{name} is not a branch FastKraken can pin to the left."), cx);
                return;
            }
            change(tab, cx, Pin::Ref(name));
        }
        Action::Unpin => change(tab, cx, Pin::Off),
        Action::Jump => {
            let Some(oid) = tip(&tab.ui) else {
                let reason = match &tab.ui.graph_view.pin {
                    Pin::Off => "No branch is pinned to the left.".to_owned(),
                    Pin::Head => "The checked-out branch has no commits yet.".to_owned(),
                    Pin::Ref(name) => format!("{name} is pinned to the left but no longer exists."),
                };
                tab.error(reason, cx);
                return;
            };
            crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
        }
    }
    cx.notify();
}

fn ref_name(ui: &Ui, target: &Target) -> Option<String> {
    if ui.repo.is_empty() {
        return None;
    }
    match target {
        Target::Branch(_) | Target::RemoteBranch(_) => full_name(target).filter(|name| pinnable(name)),
        _ => None,
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "graph.pin",
        Group::View,
        |_, _| "Pin to the left".into(),
        |ui, target, _| match ref_name(ui, target) {
            Some(name) if !pinned_here(ui, &name) => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |ui, target| Some(view::Action::Pin(Action::Pin(ref_name(ui, target)?))),
    )
    .words(&["graph", "lane", "first", "trunk"])
    .icon("pin"),
    Command::new(
        "graph.unpin",
        Group::View,
        |_, _| "Unpin from the left".into(),
        |ui, target, _| match ref_name(ui, target) {
            Some(name) if pinned_here(ui, &name) => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, _| Some(view::Action::Pin(Action::Unpin)),
    )
    .words(&["graph", "lane"])
    .icon("pin"),
    Command::new(
        "graph.jump-pinned",
        Group::View,
        |_, _| "Jump to the pinned branch".into(),
        |ui, target, _| {
            if !(on_repo(ui, target) || (*target == Target::Columns && !ui.repo.is_empty())) {
                Availability::Hidden
            } else if ui.graph_view.pin == Pin::Off {
                Availability::Disabled("No branch is pinned to the left".to_owned())
            } else {
                Availability::Enabled
            }
        },
        |_, _| Some(view::Action::Pin(Action::Jump)),
    )
    .words(&["graph", "pinned", "lane", "go to"])
    .icon("pin"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_branches_and_remote_branches_can_be_pinned() {
        assert!(pinnable("refs/heads/main"));
        assert!(pinnable("refs/remotes/origin/main"));
        assert!(!pinnable("refs/remotes/origin/"));
        assert!(!pinnable("refs/tags/v1"));
        assert!(!pinnable("refs/heads/"));
        assert!(!pinnable("refs/heads/a b"));
        assert!(!pinnable("main"));
    }

    #[test]
    fn a_stored_pin_reads_back_and_an_unknown_one_falls_back_to_the_checked_out_branch() {
        let stored = serde_json::to_string(&Pin::Ref("refs/heads/main".into())).unwrap();
        assert_eq!(serde_json::from_str::<Pin>(&stored).unwrap(), Pin::Ref("refs/heads/main".into()));
        assert_eq!(serde_json::from_str::<Pin>("\"off\"").unwrap(), Pin::Off);
        let view: crate::graph_view::GraphView = serde_json::from_str(r#"{"pin": 7}"#).unwrap();
        assert_eq!(view.pin, Pin::Head);
    }

    #[test]
    fn the_pinned_tip_is_found_by_its_label_even_when_paired_with_its_upstream() {
        let remotes = ["origin".to_owned()];
        let label = |decorations: &str| view::labels(&remotes, decorations);
        let main = label("HEAD -> main, origin/main");
        let paired = view::paired(label("HEAD -> main, origin/main"), |name| (name == "main").then(|| "origin/main".to_owned()));
        assert!(names(&main[0], "refs/heads/main"));
        assert!(names(&main[1], "refs/remotes/origin/main"));
        assert!(names(&paired[0], "refs/remotes/origin/main"));
        assert!(!names(&main[0], "refs/heads/mai"));
        assert!(label("tag: v1").iter().all(|label| !names(label, "refs/heads/v1")));
    }
}
