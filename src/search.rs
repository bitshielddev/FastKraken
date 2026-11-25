use crate::{
    Tab, kit,
    repository::Commit,
    theme::*,
    view::{self, CommitInput, Desk, Selection},
};
use gpui::{
    Context, Div, Entity, Focusable, ScrollStrategy, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, uniform_list,
};
use std::{ops::Range, time::Duration};

pub(crate) const COMMANDS: &[crate::commands::Command] = &[crate::commands::Command::new(
    "repo.find",
    crate::commands::Group::View,
    |_, _| "Find commits…".into(),
    |ui, target, _| {
        use crate::commands::Availability;
        if !crate::commands::on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(if ui.publish.open() || ui.refs.draft.is_some() || ui.prompt.open() {
            Some("Close the open dialog first".to_owned())
        } else {
            None
        })
    },
    |_, _| Some(view::Action::Search(Action::Open)),
)
.icon("search")
.keys("cmd-f")
.bar(crate::commands::Bar::Edit)
.words(&["search", "log", "history"])];

const LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub enum Action {
    Open,
    Close,
    Pick(Option<String>),
    Step(isize),
}

pub struct State {
    pub input: Entity<CommitInput>,
    open: bool,
    query: String,
    ticket: u64,
    loading: bool,
    error: Option<String>,
    rows: Vec<Commit>,
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
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }

    pub fn visible(&self) -> bool {
        self.open && !self.query.is_empty()
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
            self.error = None;
            self.input.update(cx, CommitInput::clear);
            cx.notify();
        }
    }

    fn selected(&self, selection: Option<&Selection>) -> Option<usize> {
        let Some(Selection::Commit(oid)) = selection else {
            return None;
        };
        self.rows.iter().take(LIMIT).position(|row| &row.oid == oid)
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
    state.ticket += 1;
    let ticket = state.ticket;
    state.query.clone_from(&query);
    state.rows.clear();
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
                move || repo.search_history(&query, LIMIT + 1),
                move |tab, result, cx| {
                    let state = &mut tab.ui.search;
                    if tab.epoch != epoch || state.ticket != ticket {
                        return;
                    }
                    state.loading = false;
                    match result {
                        Ok(rows) => {
                            state.rows = rows;
                            state.scroll.scroll_to_item(0, ScrollStrategy::Top);
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
            if let Some(Selection::Commit(oid)) = &ui.selection {
                ui.scroll_to_commit(oid);
            }
            view::focus(ui, window);
        }
        Action::Pick(oid) if state.visible() => {
            let row = match oid {
                Some(oid) => state.rows[..state.rows.len().min(LIMIT)]
                    .iter()
                    .find(|row| row.oid == oid),
                None => state.rows.get(state.selected(ui.selection.as_ref()).unwrap_or(0)),
            };
            if let Some(row) = row {
                let oid = row.oid.clone();
                view::focus(ui, window);
                tab.dispatch(view::Action::Select(Selection::Commit(oid)), window, cx);
            }
        }
        Action::Step(delta) if state.visible() => {
            let count = state.rows.len().min(LIMIT);
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
        return div()
            .key_context("HistorySearch")
            .flex()
            .items_center()
            .gap_1()
            .child(div().w(px(138.)).child(ui.search.input.clone()))
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

pub fn results<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let state = &app.ui().search;
    let count = state.rows.len().min(LIMIT);
    let body = div().flex_1().min_w_0().min_h_0().flex().flex_col();
    let status = if state.loading {
        "Searching commits…".into()
    } else if let Some(err) = &state.error {
        format!("Search failed. {err}")
    } else if count == 0 {
        "No matching commits".into()
    } else if state.rows.len() > LIMIT {
        format!("First {LIMIT} matching commits")
    } else if count == 1 {
        "1 matching commit".into()
    } else {
        format!("{count} matching commits")
    };
    body.child(
        div()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(kit::label(status)),
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
                            .h(px(ROW))
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
}
