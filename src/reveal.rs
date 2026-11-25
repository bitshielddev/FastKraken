use crate::{
    Tab,
    graph_view::GraphView,
    notice::{Notice, Tone, short},
    repository::{Repository, validate_oid},
    view::{self, Desk, Selection, Ui},
};
use gpui::{Context, ScrollStrategy, Window};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const TRIES: u8 = 3;
const OFFERED: Duration = Duration::from_secs(12);

#[derive(Clone, Debug)]
pub enum Action {
    Commit(String),
    Stash(String),
    Cancel,
}

#[derive(Default)]
pub(crate) struct State {
    finding: Option<Finding>,
    ticket: u64,
}

struct Finding {
    selection: Selection,
    oid: String,
    position: Option<usize>,
    tries: u8,
    stop: Arc<AtomicBool>,
    notice: u64,
}

impl State {
    pub(crate) fn finding(&self) -> Option<&str> {
        self.finding.as_ref().map(|finding| finding.oid.as_str())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Place {
    At(usize),
    Hidden,
    Unlisted,
    Missing,
}

fn place(repo: &Repository, view: &GraphView, oid: &str, stop: &AtomicBool) -> Result<Place, String> {
    let wanted = HashSet::from([oid.to_owned()]);
    if let Some(&at) = repo.positions(&view.history_args(), &wanted, stop)?.get(oid) {
        return Ok(Place::At(at));
    }
    if !repo.has_commit(oid)? {
        return Ok(Place::Missing);
    }
    if view.filtered() && repo.on_any_ref(oid)? {
        return Ok(Place::Hidden);
    }
    Ok(Place::Unlisted)
}

fn said(place: Place, oid: &str) -> Notice {
    let short = short(oid);
    match place {
        Place::Hidden => Notice::new(
            Tone::Info,
            format!("{short} is hidden by the current graph filter"),
            "Its details are shown. Clear the filter to see it in the graph.",
        )
        .offer("Clear filter", view::Action::GraphView(crate::graph_view::Action::Clear))
        .lasting(OFFERED),
        Place::Unlisted => Notice::new(
            Tone::Info,
            format!("{short} is not on any branch or tag"),
            "Its details are shown, but nothing in the graph leads to it, so it has no row there.",
        )
        .lasting(OFFERED),
        Place::Missing | Place::At(_) => Notice::new(
            Tone::Info,
            format!("{short} is not in this repository"),
            "No commit with that object ID was found.",
        ),
    }
}

fn row_of(ui: &Ui, selection: &Selection, oid: &str) -> Option<usize> {
    match selection {
        Selection::Stash(stash) => ui.stash.stash_row(stash),
        _ => ui.history.iter().position(|row| row.oid == oid).map(|index| ui.stash.commit_row(index)),
    }
}

fn scroll(ui: &Ui, row: usize) {
    ui.history_scroll.scroll_to_item(row, ScrollStrategy::Center);
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Commit(oid) => {
            if validate_oid(&oid).is_err() {
                return;
            }
            let selection = Selection::Commit(oid.clone());
            choose(tab, &selection, window, cx);
            show(tab, selection, oid, cx);
        }
        Action::Stash(stash) => {
            let base = tab.ui.stash.base_of(&stash).map(str::to_owned);
            let selection = Selection::Stash(stash);
            choose(tab, &selection, window, cx);
            match base {
                Some(base) => show(tab, selection, base, cx),
                None => {
                    if let Some(row) = row_of(&tab.ui, &selection, "") {
                        scroll(&tab.ui, row);
                    }
                }
            }
        }
        Action::Cancel => end(tab, None),
    }
    cx.notify();
}

fn choose(tab: &mut Tab, selection: &Selection, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.selection.as_ref() != Some(selection) {
        tab.dispatch(view::Action::Select(selection.clone()), window, cx);
    }
}

fn show(tab: &mut Tab, selection: Selection, oid: String, cx: &mut Context<Tab>) {
    if tab.ui.selection.as_ref() != Some(&selection) || tab.repo.is_none() {
        return;
    }
    if let Some(row) = row_of(&tab.ui, &selection, &oid) {
        end(tab, None);
        scroll(&tab.ui, row);
        return;
    }
    if tab.ui.reveal.finding.as_ref().is_some_and(|finding| finding.selection == selection) {
        return;
    }
    end(tab, None);
    let notice = tab.ui.notices.show(
        Notice::new(Tone::Progress, format!("Finding {}…", short(&oid)), "")
            .offer("Cancel", view::Action::Reveal(Action::Cancel)),
    );
    tab.ui.reveal.finding = Some(Finding {
        selection,
        oid,
        position: None,
        tries: 0,
        stop: Arc::new(AtomicBool::new(false)),
        notice,
    });
    look(tab, cx);
}

fn look(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else {
        return end(tab, None);
    };
    let state = &mut tab.ui.reveal;
    let Some(finding) = state.finding.as_mut() else {
        return;
    };
    state.ticket += 1;
    let ticket = state.ticket;
    finding.position = None;
    finding.tries += 1;
    let oid = finding.oid.clone();
    let stop = finding.stop.clone();
    let view = tab.ui.graph_view.clone();
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || place(&repo, &view, &oid, &stop),
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.reveal.ticket != ticket {
                return;
            }
            let Some(finding) = tab.ui.reveal.finding.as_mut() else {
                return;
            };
            match result {
                Ok(Place::At(at)) => {
                    finding.position = Some(at);
                    pump(tab, cx);
                }
                Ok(place) => {
                    let notice = said(place, &finding.oid);
                    end(tab, Some(notice));
                }
                Err(error) => {
                    let notice = Notice::from_text(
                        Tone::Error,
                        &format!("Could not find {} in the graph — {error}", short(&finding.oid)),
                    );
                    end(tab, Some(notice));
                }
            }
            cx.notify();
        },
    );
}

fn pump(tab: &mut Tab, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let Some(finding) = ui.reveal.finding.as_ref() else {
        return;
    };
    if ui.selection.as_ref() != Some(&finding.selection) {
        return end(tab, None);
    }
    if let Some(row) = row_of(ui, &finding.selection, &finding.oid) {
        scroll(ui, row);
        return end(tab, None);
    }
    let Some(position) = finding.position else {
        return;
    };
    if tab.loading_page {
        return;
    }
    if ui.history.len() > position || !ui.has_more {
        if finding.tries >= TRIES {
            let notice = Notice::new(
                Tone::Info,
                format!("{} moved while FastKraken was finding it", short(&finding.oid)),
                "The history kept changing. Choose it again to try once more.",
            );
            return end(tab, Some(notice));
        }
        return look(tab, cx);
    }
    tab.load_history_to(position + 1 + crate::PAGE, cx);
}

pub(crate) fn landed(tab: &mut Tab, cx: &mut Context<Tab>) {
    pump(tab, cx);
}

pub(crate) fn chose(tab: &mut Tab, selection: &Selection) {
    if tab.ui.reveal.finding.as_ref().is_some_and(|finding| &finding.selection != selection) {
        end(tab, None);
    }
}

fn end(tab: &mut Tab, notice: Option<Notice>) {
    let state = &mut tab.ui.reveal;
    let Some(finding) = state.finding.take() else {
        if let Some(notice) = notice {
            tab.ui.notices.show(notice);
        }
        return;
    };
    state.ticket += 1;
    finding.stop.store(true, Ordering::Relaxed);
    match notice {
        Some(notice) => {
            tab.ui.notices.finish(finding.notice, notice);
        }
        None => {
            tab.ui.notices.remove(finding.notice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn tip(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_position_is_the_commit_row_in_the_listed_history() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        for subject in ["Side one", "Side two"] {
            f.git(&["commit", "--allow-empty", "-qm", subject]);
        }
        f.git(&["switch", "-q", "main"]);
        for subject in ["Main one", "Main two", "Main three"] {
            f.git(&["commit", "--allow-empty", "-qm", subject]);
        }
        f.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let view = GraphView::default();
        let history = f.repo.history_in(&view, 0, 100).unwrap();
        for (index, row) in history.iter().enumerate() {
            assert_eq!(place(&f.repo, &view, &row.oid, &never()), Ok(Place::At(index)), "{}", row.subject);
        }
        let many = history.iter().map(|row| row.oid.clone()).collect::<HashSet<_>>();
        let at = f.repo.positions(&view.history_args(), &many, &never()).unwrap();
        assert_eq!(at.len(), history.len());
        assert!(history.iter().enumerate().all(|(index, row)| at[&row.oid] == index));
    }

    #[test]
    fn a_commit_the_filter_hides_is_told_apart_from_one_on_no_ref() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        f.git(&["commit", "--allow-empty", "-qm", "Side only"]);
        let side = tip(&f);
        f.git(&["switch", "-qc", "doomed"]);
        f.git(&["commit", "--allow-empty", "-qm", "Doomed"]);
        let doomed = tip(&f);
        f.git(&["switch", "-q", "main"]);
        f.git(&["branch", "-qD", "doomed"]);
        let hidden = GraphView { hidden: ["refs/heads/side".to_owned()].into() };
        assert_eq!(place(&f.repo, &hidden, &side, &never()), Ok(Place::Hidden));
        assert_eq!(place(&f.repo, &GraphView::default(), &side, &never()), Ok(Place::At(0)));
        assert_eq!(place(&f.repo, &hidden, &doomed, &never()), Ok(Place::Unlisted));
        assert_eq!(place(&f.repo, &GraphView::default(), &doomed, &never()), Ok(Place::Unlisted));
        let absent = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(place(&f.repo, &GraphView::default(), absent, &never()), Ok(Place::Missing));
    }

    #[test]
    fn a_stash_is_never_listed_and_a_stopped_search_says_so() {
        let f = Fixture::new();
        std::fs::write(f.dir.join("note.txt"), "base\n").unwrap();
        f.git(&["add", "note.txt"]);
        f.git(&["commit", "-qm", "Base"]);
        std::fs::write(f.dir.join("note.txt"), "changed\n").unwrap();
        f.git(&["stash", "push", "-q"]);
        let stash = String::from_utf8(f.git(&["rev-parse", "refs/stash"])).unwrap().trim().to_owned();
        assert_eq!(place(&f.repo, &GraphView::default(), &stash, &never()), Ok(Place::Unlisted));
        let stopped = AtomicBool::new(true);
        assert!(place(&f.repo, &GraphView::default(), &stash, &stopped).is_err());
    }

    #[test]
    fn the_notices_name_the_commit_and_offer_to_clear_a_filter() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let hidden = said(Place::Hidden, oid);
        assert_eq!(hidden.title, "0123456 is hidden by the current graph filter");
        assert_eq!(hidden.actions.len(), 1);
        assert_eq!(hidden.actions[0].0.as_ref(), "Clear filter");
        assert!(matches!(
            hidden.actions[0].1,
            view::Action::GraphView(crate::graph_view::Action::Clear)
        ));
        let unlisted = said(Place::Unlisted, oid);
        assert_eq!(unlisted.title, "0123456 is not on any branch or tag");
        assert!(unlisted.actions.is_empty());
    }
}
