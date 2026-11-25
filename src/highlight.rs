use crate::{Tab, view::HistoryRow};
use gpui::{Context, Window};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub(crate) const DWELL: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub(crate) struct State {
    ticket: u64,
    hovered: Option<String>,
    chain: Option<HashSet<String>>,
}

impl State {
    pub(crate) fn fades(&self, oid: &str) -> bool {
        self.chain.as_ref().is_some_and(|chain| !chain.contains(oid))
    }

    pub(crate) fn showing(&self) -> bool {
        self.chain.is_some()
    }

    pub(crate) fn scrolled_away<'a>(&self, shown: impl IntoIterator<Item = &'a str>) -> Option<String> {
        let hovered = self.hovered.as_deref()?;
        (!shown.into_iter().any(|oid| oid == hovered)).then(|| hovered.to_owned())
    }

    pub(crate) fn clear(&mut self) {
        self.ticket += 1;
        self.hovered = None;
        self.chain = None;
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Enter(String),
    Leave(String),
    Show(String),
}

pub(crate) fn chain(history: &[HistoryRow], tip: &str) -> HashSet<String> {
    let at: HashMap<&str, usize> = history.iter().enumerate().map(|(index, row)| (row.oid.as_str(), index)).collect();
    let mut chain = HashSet::new();
    let mut next = Some(tip);
    while let Some(oid) = next {
        let Some(&index) = at.get(oid) else { break };
        if !chain.insert(oid.to_owned()) {
            break;
        }
        next = history[index].parents.first().map(String::as_str);
    }
    chain
}

fn show(tab: &mut Tab, oid: &str) {
    let state = &mut tab.ui.highlight;
    state.hovered = Some(oid.to_owned());
    state.chain = Some(chain(&tab.ui.history, oid));
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Enter(oid) => {
            let state = &mut tab.ui.highlight;
            state.ticket += 1;
            let ticket = state.ticket;
            state.hovered = Some(oid.clone());
            cx.spawn(async move |tab, cx| {
                cx.background_executor().timer(DWELL).await;
                let _ = tab.update(cx, |tab, cx| {
                    let state = &tab.ui.highlight;
                    if state.ticket == ticket && state.hovered.as_deref() == Some(oid.as_str()) && !cx.has_active_drag() {
                        show(tab, &oid);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        Action::Leave(oid) => {
            let state = &mut tab.ui.highlight;
            if state.hovered.as_deref() == Some(oid.as_str()) {
                let shown = state.showing();
                state.clear();
                if shown {
                    cx.notify();
                }
            }
        }
        Action::Show(oid) => {
            tab.ui.highlight.ticket += 1;
            show(tab, &oid);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(oid: &str, parents: &[&str]) -> HistoryRow {
        HistoryRow {
            oid: oid.into(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            ..HistoryRow::default()
        }
    }

    #[test]
    fn hovering_a_label_keeps_only_its_first_parent_chain_among_the_loaded_rows() {
        let history = [
            row("merge", &["c", "f"]),
            row("f", &["b"]),
            row("c", &["b"]),
            row("b", &["a"]),
            row("a", &["older"]),
        ];
        let chain = chain(&history, "merge");
        assert_eq!(chain, HashSet::from(["merge", "c", "b", "a"].map(String::from)));
        let mut state = State { chain: Some(chain), ..State::default() };
        assert!(state.fades("f") && !state.fades("c") && state.fades("older"));
        state.clear();
        assert!(!state.fades("f") && !state.showing());
        assert!(super::chain(&history, "gone").is_empty());
        assert_eq!(super::chain(&history, "f"), HashSet::from(["f", "b", "a"].map(String::from)));
    }

    #[test]
    fn a_hovered_label_scrolled_out_of_the_list_lets_its_highlight_go() {
        let mut state = State::default();
        assert_eq!(state.scrolled_away(["a", "b"]), None);
        state.hovered = Some("b".into());
        state.chain = Some(HashSet::from(["b".to_owned()]));
        assert_eq!(state.scrolled_away(["a", "b", "c"]), None);
        assert_eq!(state.scrolled_away(["c", "d"]), Some("b".to_owned()));
        state.clear();
        assert_eq!(state.scrolled_away(["c", "d"]), None);
    }
}
