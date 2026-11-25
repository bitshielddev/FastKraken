use crate::{Tab, view::Selection};
use gpui::{Context, Window};
use std::{collections::BTreeSet, ffi::OsString};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GraphView {
    pub(crate) hidden: BTreeSet<String>,
}

impl GraphView {
    pub(crate) fn filtered(&self) -> bool {
        !self.hidden.is_empty()
    }

    pub(crate) fn history_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = self
            .hidden
            .iter()
            .map(|name| match name.strip_suffix('/') {
                Some(folder) => format!("--exclude={folder}/*").into(),
                None => format!("--exclude={name}").into(),
            })
            .collect();
        args.push("--exclude=refs/stash".into());
        args.push("--all".into());
        if self.filtered() {
            args.push("HEAD".into());
        }
        args
    }
}

pub(crate) fn hideable(name: &str) -> bool {
    name.strip_prefix("refs/").is_some_and(|rest| !rest.is_empty())
        && name != "refs/stash"
        && !name.contains(['*', '?', '[', '\\', ' ', '~', '^', ':'])
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

#[derive(Clone, Debug)]
pub enum Action {
    Hide(String),
    Clear,
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Hide(name) => {
            if !hideable(&name) {
                tab.error(format!("{name} is not a ref FastKraken can hide from the graph."), cx);
                return;
            }
            if tab.ui.graph_view.hidden.insert(name) {
                tab.load_history(true, cx);
            }
        }
        Action::Clear => {
            if !tab.ui.graph_view.filtered() {
                return;
            }
            tab.ui.graph_view = GraphView::default();
            tab.load_history(true, cx);
            if let Some(Selection::Commit(oid)) = tab.ui.selection.clone() {
                crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
            }
        }
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn args(view: &GraphView) -> Vec<String> {
        view.history_args().into_iter().map(|arg| arg.into_string().unwrap()).collect()
    }

    #[test]
    fn an_unfiltered_view_lists_every_ref_but_the_stash() {
        assert_eq!(args(&GraphView::default()), ["--exclude=refs/stash", "--all"]);
    }

    #[test]
    fn hidden_refs_are_excluded_before_all_and_head_stays() {
        let view = GraphView {
            hidden: ["refs/heads/feature/map".to_owned(), "refs/remotes/origin/".to_owned()].into(),
        };
        assert_eq!(
            args(&view),
            [
                "--exclude=refs/heads/feature/map",
                "--exclude=refs/remotes/origin/*",
                "--exclude=refs/stash",
                "--all",
                "HEAD"
            ]
        );
    }

    #[test]
    fn only_plain_ref_names_can_be_hidden() {
        assert!(hideable("refs/heads/feature/map"));
        assert!(hideable("refs/remotes/origin/"));
        for name in ["main", "refs/", "refs/stash", "refs/heads/*", "refs/heads/a b", "refs/heads/a..b", "--all"] {
            assert!(!hideable(name), "{name}");
        }
    }

    #[test]
    fn hiding_a_branch_drops_its_own_commits_but_never_head() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        f.git(&["commit", "--allow-empty", "-qm", "Side only"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["commit", "--allow-empty", "-qm", "Main only"]);
        let subjects = |view: &GraphView| -> Vec<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        assert_eq!(subjects(&GraphView::default()).len(), 3);
        let side = GraphView { hidden: ["refs/heads/side".to_owned()].into() };
        assert_eq!(subjects(&side), ["Main only", "Base"]);
        let main = GraphView { hidden: ["refs/heads/main".to_owned()].into() };
        assert_eq!(subjects(&main).len(), 3);
    }
}
