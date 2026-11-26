use crate::{
    Tab, kit,
    repository::Commit,
    theme::*,
    view::{self, CommitInput, Desk, Selection},
};
use gpui::{
    Context, Div, Entity, Focusable, ScrollStrategy, UniformListScrollHandle, Window, div,
    prelude::*, rgb, uniform_list,
};
use crate::theme::px;
use std::{collections::HashSet, ops::Range, time::Duration};

pub(crate) const COMMANDS: &[crate::commands::Command] = &[crate::commands::Command::new(
    "repo.find",
    crate::commands::Group::View,
    |ui, _| crate::find::title(ui).into(),
    |ui, target, _| {
        use crate::commands::Availability;
        if crate::find::filters(ui) && *target == crate::commands::Target::Repo {
            return Availability::Enabled;
        }
        if !crate::commands::on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(if ui.publish.open() || ui.refs.draft.is_some() || ui.prompt.open() {
            Some("Close the open dialog first".to_owned())
        } else {
            None
        })
    },
    |_, _| Some(view::Action::Find(crate::find::Action::Front)),
)
.icon("search")
.keys("cmd-f")
.bar(crate::commands::Bar::Edit)
.words(&["search", "log", "history"])];

const LIMIT: usize = 100;
const MORE: usize = 5;

#[derive(Clone, Debug)]
pub enum Action {
    Open,
    Close,
    Pick(Option<String>),
    Step(isize),
    More,
    ShowInGraph,
    ShowList,
}

pub struct State {
    pub input: Entity<CommitInput>,
    open: bool,
    query: String,
    ticket: u64,
    loading: bool,
    error: Option<String>,
    rows: Vec<Commit>,
    limit: usize,
    in_graph: bool,
    marked: HashSet<String>,
    scroll: UniformListScrollHandle,
}

impl State {
    pub fn new(cx: &mut Context<Tab>) -> Self {
        let input = cx.new(|cx| CommitInput::with_placeholder(cx, "Find commits"));
        cx.observe(&input, |tab, _, cx| refresh(tab, false, cx)).detach();
        Self {
            input,
            open: false,
            query: String::new(),
            ticket: 0,
            loading: false,
            error: None,
            rows: Vec::new(),
            limit: LIMIT,
            in_graph: false,
            marked: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn visible(&self) -> bool {
        self.open && !self.query.is_empty() && !self.in_graph
    }

    pub(crate) fn in_graph(&self) -> bool {
        self.open && self.in_graph
    }

    pub(crate) fn marks(&self, oid: &str) -> bool {
        self.in_graph() && self.marked.contains(oid)
    }

    pub(crate) fn shown(&self) -> &[Commit] {
        &self.rows[..self.rows.len().min(self.limit)]
    }

    pub(crate) fn matches(&self) -> usize {
        self.shown().len()
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn hide(&mut self, cx: &mut Context<Tab>) {
        if self.open {
            self.open = false;
            self.loading = false;
            self.ticket += 1;
            self.query.clear();
            self.rows.clear();
            self.limit = LIMIT;
            self.in_graph = false;
            self.marked.clear();
            self.error = None;
            self.input.update(cx, CommitInput::clear);
            cx.notify();
        }
    }

    fn selected(&self, selection: Option<&Selection>) -> Option<usize> {
        let Some(Selection::Commit(oid)) = selection else {
            return None;
        };
        self.shown().iter().position(|row| &row.oid == oid)
    }
}

pub fn refresh(tab: &mut Tab, force: bool, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.search;
    if !state.open {
        return;
    }
    let query = state.input.read(cx).text().trim().to_owned();
    if query == state.query && !force {
        return;
    }
    if query != state.query {
        state.limit = LIMIT;
        state.in_graph = false;
        state.marked.clear();
    }
    state.ticket += 1;
    let ticket = state.ticket;
    let limit = state.limit;
    state.query.clone_from(&query);
    if !force {
        state.rows.clear();
    }
    state.error = None;
    state.loading = !query.is_empty();
    cx.notify();
    if query.is_empty() {
        return;
    }
    let epoch = tab.epoch;
    cx.spawn(async move |tab, cx| {
        cx.background_executor().timer(Duration::from_millis(150)).await;
        let _ = tab.update(cx, |tab, cx| {
            if tab.epoch != epoch || tab.ui.search.ticket != ticket {
                return;
            }
            let Some(repo) = tab.repo.clone() else { return };
            Tab::work(
                cx,
                move || repo.search_history(&query, limit + 1),
                move |tab, result, cx| {
                    let state = &mut tab.ui.search;
                    if tab.epoch != epoch || state.ticket != ticket {
                        return;
                    }
                    state.loading = false;
                    match result {
                        Ok(rows) => {
                            state.rows = rows;
                            if state.in_graph {
                                state.marked = state.shown().iter().map(|row| row.oid.clone()).collect();
                            }
                            if !force {
                                state.scroll.scroll_to_item(0, ScrollStrategy::Top);
                            }
                        }
                        Err(err) => state.error = Some(err),
                    }
                    cx.notify();
                },
            );
        });
    })
    .detach();
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &mut tab.ui;
    let state = &mut ui.search;
    match action {
        Action::Open if tab.repo.is_some() && ui.refs.draft.is_none() && !ui.prompt.open() => {
            state.open = true;
            ui.diff_open = false;
            window.focus(&state.input.focus_handle(cx));
        }
        Action::Close if state.open => {
            state.hide(cx);
            view::focus(ui, window);
            if let Some(Selection::Commit(oid)) = ui.selection.clone() {
                crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
            }
        }
        Action::Pick(None) | Action::Step(_) if state.in_graph() => {
            let delta = match action {
                Action::Step(delta) => delta.signum(),
                _ => 1,
            };
            let count = state.matches();
            if count == 0 {
                return;
            }
            let next = match state.selected(ui.selection.as_ref()) {
                Some(index) => (index as isize + delta).rem_euclid(count as isize) as usize,
                None if delta < 0 => count - 1,
                None => 0,
            };
            let oid = state.rows[next].oid.clone();
            crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
        }
        Action::Pick(oid) if state.visible() => {
            let row = match oid {
                Some(oid) => state.shown().iter().find(|row| row.oid == oid),
                None => state.rows.get(state.selected(ui.selection.as_ref()).unwrap_or(0)),
            };
            if let Some(row) = row {
                let oid = row.oid.clone();
                view::focus(ui, window);
                crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
            }
        }
        Action::More if state.visible() && state.rows.len() > state.limit && !state.loading => {
            state.limit = state.limit.saturating_mul(MORE);
            refresh(tab, true, cx);
        }
        Action::ShowInGraph if state.visible() && state.matches() > 0 => {
            state.in_graph = true;
            state.marked = state.shown().iter().map(|row| row.oid.clone()).collect();
            ui.diff_open = false;
            window.focus(&state.input.focus_handle(cx));
            let current = state.selected(ui.selection.as_ref()).unwrap_or(0);
            let oid = state.rows[current].oid.clone();
            crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
        }
        Action::ShowList if state.in_graph() => {
            state.in_graph = false;
            state.marked.clear();
            if let Some(index) = state.selected(ui.selection.as_ref()) {
                state.scroll.scroll_to_item(index, ScrollStrategy::Center);
            }
            window.focus(&state.input.focus_handle(cx));
        }
        Action::Step(delta) if state.visible() => {
            let count = state.matches();
            if count == 0 {
                return;
            }
            let index = state.selected(ui.selection.as_ref()).map_or(0, |index| {
                index.saturating_add_signed(delta).min(count - 1)
            });
            let oid = state.rows[index].oid.clone();
            state.scroll.scroll_to_item(index, ScrollStrategy::Center);
            tab.dispatch(view::Action::Select(Selection::Commit(oid)), window, cx);
        }
        _ => return,
    }
    cx.notify();
}

pub fn toolbar<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let enabled = !ui.publish.open()
        && ui.refs.draft.is_none()
        && !ui.prompt.open();
    if ui.search.open && enabled {
        let state = &ui.search;
        let stepping = state.in_graph();
        let step = |id: &'static str, icon: &'static str, tip: &'static str, delta: isize| {
            kit::icon_btn(id, icon, tip, view::Action::Search(Action::Step(delta)), cx)
        };
        return div()
            .key_context("HistorySearch")
            .flex()
            .items_center()
            .gap_1()
            .child(div().w(px(138.)).child(ui.search.input.clone()))
            .when(stepping, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px_1()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(palette().text_2))
                        .child(counter(state, ui.selection.as_ref())),
                )
                .child(step("search-previous", "chevron-up", "Previous match (⇧↩)", -1))
                .child(step("search-next", "chevron-down", "Next match (↩)", 1))
                .child(kit::icon_btn(
                    "search-list",
                    "history",
                    "Show the matching commits as a list",
                    view::Action::Search(Action::ShowList),
                    cx,
                ))
            })
            .child(kit::icon_btn(
                "close-history-search",
                "x",
                "Close search (Esc)",
                view::Action::Search(Action::Close),
                cx,
            ));
    }
    div().child(
        kit::btn("history-search", "Search")
            .icon("search")
            .hint(if cfg!(target_os = "macos") { "⌘F" } else { "Ctrl F" })
            .enabled(enabled)
            .on(view::Action::Search(Action::Open), cx),
    )
}

fn counter(state: &State, selection: Option<&Selection>) -> String {
    let count = state.matches();
    let more = if state.rows.len() > state.limit { "+" } else { "" };
    match state.selected(selection) {
        Some(index) => format!("{} of {count}{more}", index + 1),
        None if count == 1 => "1 match".to_owned(),
        None => format!("{count}{more} matches"),
    }
}

pub(crate) fn status(state: &State) -> String {
    worded(state.loading, state.error.as_deref(), state.rows.len(), state.limit)
}

fn worded(loading: bool, error: Option<&str>, found: usize, limit: usize) -> String {
    let count = found.min(limit);
    if loading {
        "Searching commits…".into()
    } else if let Some(err) = error {
        format!("Search failed. {err}")
    } else if count == 0 {
        "No matching commits".into()
    } else if found > limit {
        format!("Showing the first {limit}")
    } else if count == 1 {
        "1 matching commit".into()
    } else {
        format!("{count} matching commits")
    }
}

pub fn results<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let state = &app.ui().search;
    let count = state.matches();
    let body = div().flex_1().min_w_0().min_h_0().flex().flex_col();
    let more = state.rows.len() > state.limit && !state.loading;
    body.child(
        div()
            .flex_shrink_0()
            .px_3()
            .py(px(5.))
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette().line))
            .child(kit::label(status(state)))
            .when(more, |d| {
                d.child(kit::btn("search-more", "Show more").small().on(view::Action::Search(Action::More), cx))
            })
            .child(kit::grow())
            .when(count > 0, |d| {
                d.child(
                    kit::btn("search-in-graph", "Show in graph")
                        .small()
                        .icon("commit")
                        .on(view::Action::Search(Action::ShowInGraph), cx),
                )
            }),
    )
    .when(count > 0, |body| {
        body.child(
            uniform_list(
                "history-search-results",
                count,
                cx.processor(|app, range: Range<usize>, _, cx| {
                    let ui = app.ui();
                    range.map(|index| {
                        let row = &ui.search.rows[index];
                        let active = matches!(&ui.selection, Some(Selection::Commit(oid)) if *oid == row.oid);
                        let oid = row.oid.clone();
                        kit::row(("search-match", index), active)
                            .h(whole_px(ROW))
                            .px_3()
                            .gap_2()
                            .child(kit::avatar(&row.author))
                            .child(kit::fit(row.subject.clone()).grow())
                            .child(kit::label(row.oid[..7].to_owned()).font_family(MONO))
                            .on_click(cx.listener(move |app, _, w, cx| {
                                app.dispatch(view::Action::Search(Action::Pick(Some(oid.clone()))), w, cx);
                            }))
                    }).collect::<Vec<_>>()
                }),
            )
            .track_scroll(state.scroll.clone())
            .flex_1()
            .min_h_0(),
        )
        .child(kit::scrollbar("history-search-scrollbar", &state.scroll))
    })
}

#[cfg(test)]
mod tests {
    use crate::repository::tests::Fixture;

    #[test]
    fn searches_commit_bodies_literally_and_resolves_an_abbreviated_sha() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-m", "Old commit", "-m", "Has case [substance] here."]);
        f.git(&["commit", "--allow-empty", "-m", "New commit"]);
        let tip = f.repo.history(1, 1).unwrap().remove(0);

        assert_eq!(f.repo.search_history("CASE [substance]", 1).unwrap(), [tip.clone()]);
        assert_eq!(f.repo.search_history(&tip.oid[..8].to_ascii_uppercase(), 1).unwrap(), [tip]);
        assert!(f.repo.search_history("--all", 10).unwrap().is_empty());
    }

    #[test]
    fn matches_the_author_or_the_message_and_keeps_history_order() {
        let f = Fixture::new();
        let by = |name: &str, email: &str, subject: &str| {
            let author = format!("--author={name} <{email}>");
            f.git(&["commit", "--allow-empty", "-q", &author, "-m", subject]);
        };
        by("Cedar Contributor", "cedar@example.invalid", "Plant juniper");
        by("Juniper Fox", "fox@example.invalid", "Survey the ridge");
        by("Cedar Contributor", "cedar@example.invalid", "Clear the path");
        by("Aspen Walker", "aspen@juniper.invalid", "Mark the camp");
        by("Cedar Contributor", "cedar@example.invalid", "Juniper berries ripen");
        let subjects = |limit| -> Vec<String> {
            f.repo.search_history("JUNIPER", limit).unwrap().into_iter().map(|row| row.subject).collect()
        };
        assert_eq!(subjects(10), ["Juniper berries ripen", "Mark the camp", "Survey the ridge", "Plant juniper"]);
        assert_eq!(subjects(2), ["Juniper berries ripen", "Mark the camp"]);
        assert!(f.repo.search_history("1700000000", 10).unwrap().is_empty());
    }

    #[test]
    fn results_past_the_limit_say_only_the_first_are_shown() {
        let f = Fixture::new();
        for index in 0..7 {
            f.git(&["commit", "--allow-empty", "-qm", &format!("Stake {index}")]);
        }
        let found = f.repo.search_history("stake", 6).unwrap().len();
        assert_eq!(found, 6);
        assert_eq!(super::worded(false, None, found, 5), "Showing the first 5");
        let found = f.repo.search_history("stake", 26).unwrap().len();
        assert_eq!(super::worded(false, None, found, 25), "7 matching commits");
        assert_eq!(super::worded(false, None, 1, 100), "1 matching commit");
        assert_eq!(super::worded(false, None, 0, 100), "No matching commits");
    }
}
