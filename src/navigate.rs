use crate::{
    Tab, notice,
    stash::HistoryItem,
    theme::{FILE_ROW, ROW, TREE_ROW},
    view::{self, Desk, HistoryRow, Selection, Tone},
};
use gpui::{Context, Point, ScrollStrategy, UniformListScrollHandle, Window, px};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Zone {
    Sidebar,
    #[default]
    Graph,
    Files,
}

#[derive(Default)]
pub(crate) struct State {
    zone: Zone,
    file: usize,
    file_of: Option<String>,
    side: usize,
}

impl State {
    pub(crate) fn zone(&self, ui: &view::Ui) -> Zone {
        match (&ui.selection, self.zone) {
            (Some(Selection::File { .. }), _) => Zone::Files,
            (Some(Selection::Commit(_)), Zone::Files) if !ui.panel_hidden => Zone::Files,
            (_, Zone::Files) => Zone::Graph,
            (_, Zone::Sidebar) if ui.sidebar_hidden => Zone::Graph,
            (_, zone) => zone,
        }
    }

    pub(crate) fn file_cursor(&self, ui: &view::Ui) -> Option<usize> {
        let Some(Selection::Commit(oid)) = &ui.selection else {
            return None;
        };
        (self.zone(ui) == Zone::Files && self.file_of.as_ref() == Some(oid)).then_some(self.file)
    }

    pub(crate) fn side_cursor(&self, ui: &view::Ui) -> Option<usize> {
        (self.zone(ui) == Zone::Sidebar).then_some(self.side)
    }

    pub(crate) fn chose(&mut self, selection: &Selection) {
        self.zone = match selection {
            Selection::File { .. } => Zone::Files,
            _ => Zone::Graph,
        };
    }

    pub(crate) fn opened(&mut self, oid: &str, index: usize) {
        self.zone = Zone::Files;
        self.file = index;
        self.file_of = Some(oid.to_owned());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Step(isize),
    Page(isize),
    Home,
    End,
    First,
    Last,
    Parent,
    Child,
    Left,
    Right,
    Choose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    By(isize),
    First,
    Last,
}

pub(crate) fn moved(current: Option<usize>, len: usize, how: Move) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (how, current) {
        (Move::First, _) => 0,
        (Move::Last, _) => len - 1,
        (Move::By(_), None) => 0,
        (Move::By(delta), Some(at)) => at.saturating_add_signed(delta).min(len - 1),
    })
}

pub(crate) fn page_rows(height: f32, row: f32) -> usize {
    if height <= 0. || row <= 0. {
        return 10;
    }
    ((height / row) as usize).saturating_sub(1).max(1)
}

pub(crate) fn first_parent(history: &[HistoryRow], at: usize) -> Option<usize> {
    let parent = history.get(at)?.parents.first()?;
    history.iter().position(|row| &row.oid == parent)
}

pub(crate) fn first_child(history: &[HistoryRow], at: usize) -> Option<usize> {
    let oid = &history.get(at)?.oid;
    (0..at).rev().find(|&row| history[row].parents.first() == Some(oid))
}

pub(crate) fn beside(zone: Zone, right: bool, sidebar: bool, files: bool) -> Zone {
    match (zone, right) {
        (Zone::Sidebar, true) => Zone::Graph,
        (Zone::Graph, true) if files => Zone::Files,
        (Zone::Graph, false) if sidebar => Zone::Sidebar,
        (Zone::Files, false) => Zone::Graph,
        (zone, _) => zone,
    }
}

fn page(handle: &UniformListScrollHandle, row: f32) -> usize {
    let height = f32::from(handle.0.borrow().base_handle.bounds().size.height);
    page_rows(height, row)
}

pub(crate) fn how(action: Action, page: usize) -> Option<Move> {
    let page = page as isize;
    match action {
        Action::Step(delta) => Some(Move::By(delta)),
        Action::Page(direction) => Some(Move::By(direction.signum() * page)),
        Action::Home | Action::First => Some(Move::First),
        Action::End | Action::Last => Some(Move::Last),
        _ => None,
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    use crate::commands::Key;
    if tab.ui.menu.is_some() {
        let key = match action {
            Action::Step(delta) if delta < 0 => Key::Up,
            Action::Step(_) => Key::Down,
            Action::Left => Key::Left,
            Action::Right => Key::Right,
            Action::Choose => Key::Enter,
            _ => return,
        };
        tab.dispatch(view::Action::MenuKey(key), window, cx);
        return;
    }
    if tab.ui.palette.open() {
        crate::palette::key(tab, action, window, cx);
        return;
    }
    if tab.ui.commit_options.picking() {
        crate::commit_options::key(tab, action, window, cx);
        return;
    }
    if tab.ui.author_picker.open() {
        crate::graph_view::key(tab, action, window, cx);
        return;
    }
    let ui = &tab.ui;
    if ui.dialog_open() || ui.help.open() {
        return;
    }
    if ui.preferences.open() {
        crate::preferences::key(tab, action, window, cx);
        return;
    }
    if ui.opening.is_some() && ui.repo.is_empty() {
        return;
    }
    if ui.repo.is_empty() {
        crate::start::key(tab, action, window, cx);
        return;
    }
    if ui.refs.list_open {
        crate::refs::switcher_key(tab, action, window, cx);
        return;
    }
    let zone = ui.navigate.zone(ui);
    if ui.activity.open() && zone == Zone::Graph {
        let how = match action {
            Action::Step(delta) => Move::By(delta),
            Action::Page(pages) => Move::By(pages * 10),
            Action::Home | Action::First => Move::First,
            Action::End | Action::Last => Move::Last,
            _ => return,
        };
        tab.dispatch(view::Action::Activity(crate::activity::Action::Step(how)), window, cx);
        return;
    }
    if crate::view::centre(ui) == crate::view::Centre::Blame && zone != Zone::Sidebar {
        use crate::blame::Action as Blame;
        let wanted = match action {
            Action::Step(delta) => Blame::Step(delta),
            Action::First => Blame::Step(isize::MIN),
            Action::Last => Blame::Step(isize::MAX),
            Action::Page(_) | Action::Home | Action::End => {
                scroll_patch(&tab.ui, action);
                cx.notify();
                return;
            }
            _ => return,
        };
        tab.dispatch(view::Action::Blame(wanted), window, cx);
        return;
    }
    if crate::view::centre(ui) == crate::view::Centre::FileHistory && zone != Zone::Sidebar {
        use crate::file_history::Action as History;
        let last = ui.file_history.entries().len().saturating_sub(1);
        let wanted = match action {
            Action::Step(delta) => History::Step(delta),
            Action::First => History::Choose(0),
            Action::Last => History::Choose(last),
            Action::Page(_) | Action::Home | Action::End => {
                scroll_patch(&tab.ui, action);
                cx.notify();
                return;
            }
            _ => return,
        };
        tab.dispatch(view::Action::FileHistory(wanted), window, cx);
        return;
    }
    if ui.operation.active()
        && (matches!(crate::view::centre(ui), crate::view::Centre::Resolver | crate::view::Centre::Chooser)
            || (zone == Zone::Files && !matches!(ui.selection, Some(Selection::Commit(_)))))
    {
        return;
    }
    if ui.diff_open
        && zone != Zone::Sidebar
        && matches!(action, Action::Page(_) | Action::Home | Action::End)
    {
        scroll_patch(&tab.ui, action);
        cx.notify();
        return;
    }
    match action {
        Action::Left | Action::Right => sideways(tab, zone, action == Action::Right, window, cx),
        Action::Parent | Action::Child => {
            if zone == Zone::Graph {
                along(tab, action == Action::Parent, window, cx);
            } else if zone == Zone::Files && matches!(tab.ui.selection, Some(Selection::File { .. })) {
                let delta = if action == Action::Parent { 1 } else { -1 };
                tab.dispatch(view::Action::Picked(crate::picked::Action::Extend(delta)), window, cx);
            }
        }
        Action::Choose => choose(tab, zone, window, cx),
        _ => match zone {
            Zone::Sidebar => sidebar(tab, action, cx),
            Zone::Graph => graph(tab, action, window, cx),
            Zone::Files => files(tab, action, window, cx),
        },
    }
}

fn scroll_patch(ui: &view::Ui, action: Action) {
    let state = ui.diff_scroll.0.borrow();
    let handle = &state.base_handle;
    let height = handle.bounds().size.height;
    let bottom = -handle.max_offset().height;
    let offset = handle.offset();
    let y = match action {
        Action::Home => px(0.),
        Action::End => bottom,
        Action::Page(direction) if direction < 0 => (offset.y + height).min(px(0.)),
        _ => (offset.y - height).max(bottom),
    };
    handle.set_offset(Point { x: offset.x, y });
}

fn select(tab: &mut Tab, selection: Selection, window: &mut Window, cx: &mut Context<Tab>) {
    view::focus(&tab.ui, window);
    tab.dispatch(view::Action::Select(selection), window, cx);
}

fn graph(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let rows = ui.history_items();
    let Some(how) = how(action, page(&ui.history_scroll, ROW)) else {
        return;
    };
    if ui.search.visible() && !ui.diff_open {
        let delta = match how {
            Move::By(delta) => delta,
            Move::First => isize::MIN,
            Move::Last => isize::MAX,
        };
        tab.dispatch(view::Action::Search(crate::search::Action::Step(delta)), window, cx);
        return;
    }
    let current = ui.selected_history_row();
    let up = matches!(how, Move::First) || matches!(how, Move::By(delta) if delta < 0);
    let to_changes = match (how, current) {
        (Move::First, _) => true,
        (Move::By(delta), Some(at)) => delta < 0 && at.checked_add_signed(delta).is_none(),
        _ => false,
    };
    if to_changes && up {
        ui.history_scroll.scroll_to_item(0, ScrollStrategy::Top);
        select(tab, Selection::Changes, window, cx);
        return;
    }
    if ui.working() && up {
        return;
    }
    let current = current.or_else(|| match how {
        Move::By(delta) if ui.working() && delta > 1 => Some(0),
        _ => None,
    });
    if let Some(row) = moved(current, rows, how) {
        go_to_row(tab, row, window, cx);
    }
}

fn go_to_row(tab: &mut Tab, row: usize, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    ui.history_scroll.scroll_to_item(row, ScrollStrategy::Center);
    let selection = match ui.stash.item(row) {
        HistoryItem::Commit(index) => Selection::Commit(ui.history[index].oid.clone()),
        HistoryItem::Stash { stash, .. } => Selection::Stash(ui.stash.entries[stash].oid.clone()),
    };
    select(tab, selection, window, cx);
}

fn along(tab: &mut Tab, down: bool, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let commit = match &ui.selection {
        Some(Selection::Commit(oid)) => ui.history.iter().position(|row| &row.oid == oid),
        Some(Selection::Changes) if down => {
            ui.head.as_ref().and_then(|head| ui.history.iter().position(|row| &row.oid == head))
        }
        Some(Selection::Stash(oid)) if down => {
            let row = ui.stash.stash_row(oid);
            match row.map(|row| ui.stash.item(row)) {
                Some(HistoryItem::Stash { base, .. }) => Some(base),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(at) = commit else {
        return;
    };
    let onto = if matches!(ui.selection, Some(Selection::Commit(_))) {
        if down { first_parent(&ui.history, at) } else { first_child(&ui.history, at) }
    } else {
        Some(at)
    };
    match onto {
        Some(index) => {
            let row = tab.ui.stash.commit_row(index);
            go_to_row(tab, row, window, cx);
        }
        None if !down && tab.ui.head.as_deref() == Some(tab.ui.history[at].oid.as_str()) => {
            tab.ui.history_scroll.scroll_to_item(0, ScrollStrategy::Top);
            select(tab, Selection::Changes, window, cx);
        }
        None if down && tab.ui.has_more && !tab.ui.history[at].parents.is_empty() => {
            tab.ui.notices.show(notice::Notice::new(
                Tone::Info,
                "Loading more history",
                "Press Shift-Down again to follow the first parent.",
            ));
            tab.dispatch(view::Action::More, window, cx);
        }
        None => {}
    }
}

fn files(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match tab.ui.selection.clone() {
        Some(Selection::File { path, staged }) => {
            let ui = &tab.ui;
            let current = ui.files.iter().position(|f| f.staged == staged && f.path == path);
            let Some(how) = how(action, page(&ui.file_scroll, FILE_ROW)) else {
                return;
            };
            let Some(index) = moved(current, ui.files.len(), how) else {
                return;
            };
            let (unstaged, staged) = ui.file_ranges();
            let row = if ui.files[index].staged {
                2 + unstaged.len() + index - staged.start
            } else {
                1 + index - unstaged.start
            };
            ui.file_scroll.scroll_to_item(row, ScrollStrategy::Center);
            let file = &ui.files[index];
            let selection = Selection::File { path: file.path.clone(), staged: file.staged };
            select(tab, selection, window, cx);
        }
        Some(Selection::Commit(oid)) => {
            let ui = &tab.ui;
            let count = ui.inspection.files.len();
            let Some(how) = how(action, page(&ui.inspection.scroll, FILE_ROW)) else {
                return;
            };
            let current = ui.navigate.file_cursor(ui);
            let Some(index) = moved(current, count, how) else {
                return;
            };
            ui.inspection.scroll.scroll_to_item(index, ScrollStrategy::Center);
            let open = ui.diff_open && ui.inspection.path.is_some();
            let path = ui.inspection.files[index].path.clone();
            tab.ui.navigate.opened(&oid, index);
            if open {
                tab.dispatch(view::Action::Inspect(crate::inspection::Action::OpenFile(path)), window, cx);
            }
            cx.notify();
        }
        _ => {}
    }
}

fn sideways(tab: &mut Tab, zone: Zone, right: bool, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let files = !ui.panel_hidden
        && match &ui.selection {
            Some(Selection::Commit(_)) => !ui.inspection.files.is_empty(),
            Some(Selection::Changes | Selection::File { .. }) => !ui.files.is_empty() && !ui.operation.active(),
            _ => false,
        };
    let to = beside(zone, right, !ui.sidebar_hidden, files);
    if to == zone {
        return;
    }
    match (zone, to) {
        (Zone::Graph, Zone::Files) => match tab.ui.selection.clone() {
            Some(Selection::Commit(oid)) => {
                let ui = &tab.ui;
                let open = ui.inspection.path.as_ref().and_then(|path| {
                    ui.inspection.files.iter().position(|file| &file.path == path)
                });
                let kept = ui.navigate.file_of.as_ref() == Some(&oid);
                let index = open.unwrap_or(if kept { ui.navigate.file } else { 0 });
                let last = ui.inspection.files.len() - 1;
                tab.ui.navigate.opened(&oid, index.min(last));
                tab.ui.inspection.scroll.scroll_to_item(tab.ui.navigate.file, ScrollStrategy::Center);
            }
            _ => {
                let first = &tab.ui.files[0];
                let selection = Selection::File { path: first.path.clone(), staged: first.staged };
                tab.ui.file_scroll.scroll_to_item(0, ScrollStrategy::Top);
                select(tab, selection, window, cx);
            }
        },
        (Zone::Files, Zone::Graph) => {
            tab.ui.navigate.zone = Zone::Graph;
            if tab.ui.diff_open || tab.ui.working() {
                tab.dispatch(view::Action::CloseDiff, window, cx);
            }
        }
        (_, to) => {
            tab.ui.navigate.zone = to;
            let rows = tab.ui.refs.rows.len();
            tab.ui.navigate.side = tab.ui.navigate.side.min(rows.saturating_sub(1));
            if to == Zone::Sidebar {
                tab.ui.refs.scroll.scroll_to_item(tab.ui.navigate.side, ScrollStrategy::Center);
            }
        }
    }
    view::focus(&tab.ui, window);
    cx.notify();
}

fn choose(tab: &mut Tab, zone: Zone, window: &mut Window, cx: &mut Context<Tab>) {
    match zone {
        Zone::Sidebar => {
            let index = tab.ui.navigate.side;
            crate::refs::activate_row(tab, index, window, cx);
            tab.ui.navigate.zone = Zone::Sidebar;
        }
        Zone::Files => match tab.ui.selection.clone() {
            Some(Selection::Commit(_)) => {
                let Some(index) = tab.ui.navigate.file_cursor(&tab.ui) else {
                    return;
                };
                if let Some(file) = tab.ui.inspection.files.get(index) {
                    let path = file.path.clone();
                    tab.dispatch(view::Action::Inspect(crate::inspection::Action::OpenFile(path)), window, cx);
                }
            }
            Some(Selection::File { .. }) if !tab.ui.diff_open => {
                tab.dispatch(view::Action::OpenDiff, window, cx);
            }
            _ => {}
        },
        Zone::Graph => {}
    }
}

fn sidebar(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    let ui = &tab.ui;
    let Some(how) = how(action, page(&ui.refs.scroll, TREE_ROW)) else {
        return;
    };
    let rows = &ui.refs.rows;
    let current = Some(ui.navigate.side.min(rows.len().saturating_sub(1)));
    let Some(index) = moved(current, rows.len(), how) else {
        return;
    };
    ui.refs.scroll.scroll_to_item(index, ScrollStrategy::Center);
    tab.ui.navigate.side = index;
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(oid: &str, parents: &[&str]) -> HistoryRow {
        HistoryRow {
            oid: oid.repeat(40),
            parents: parents.iter().map(|parent| parent.repeat(40)).collect(),
            subject: String::new(),
            author: String::new(),
            email: String::new(),
            date: String::new(),
            time: 0,
            refs: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn moves_stop_at_the_ends_of_a_list() {
        assert_eq!(moved(Some(3), 10, Move::By(1)), Some(4));
        assert_eq!(moved(Some(3), 10, Move::By(-1)), Some(2));
        assert_eq!(moved(Some(0), 10, Move::By(-1)), Some(0));
        assert_eq!(moved(Some(8), 10, Move::By(5)), Some(9));
        assert_eq!(moved(Some(4), 10, Move::By(-20)), Some(0));
        assert_eq!(moved(Some(4), 10, Move::First), Some(0));
        assert_eq!(moved(Some(4), 10, Move::Last), Some(9));
        assert_eq!(moved(None, 10, Move::By(1)), Some(0));
        assert_eq!(moved(None, 10, Move::Last), Some(9));
        assert_eq!(moved(Some(0), 0, Move::By(1)), None);
    }

    #[test]
    fn a_page_is_what_fits_less_one_row() {
        assert_eq!(page_rows(280., 28.), 9);
        assert_eq!(page_rows(29., 28.), 1);
        assert_eq!(page_rows(10., 28.), 1);
        assert_eq!(page_rows(0., 28.), 10);
    }

    #[test]
    fn shift_follows_the_first_parent_through_an_interleaved_graph() {
        let history = [
            commit("d", &["b", "c"]),
            commit("c", &["x"]),
            commit("b", &["a"]),
            commit("x", &["a"]),
            commit("a", &[]),
        ];
        assert_eq!(first_parent(&history, 0), Some(2));
        assert_eq!(first_parent(&history, 2), Some(4));
        assert_eq!(first_parent(&history, 1), Some(3));
        assert_eq!(first_parent(&history, 4), None);
        assert_eq!(first_child(&history, 4), Some(3));
        assert_eq!(first_child(&history, 2), Some(0));
        assert_eq!(first_child(&history, 1), None);
        assert_eq!(first_child(&history, 0), None);
    }

    #[test]
    fn left_and_right_move_between_the_columns_that_are_drawn() {
        assert_eq!(beside(Zone::Graph, true, true, true), Zone::Files);
        assert_eq!(beside(Zone::Graph, true, true, false), Zone::Graph);
        assert_eq!(beside(Zone::Graph, false, true, true), Zone::Sidebar);
        assert_eq!(beside(Zone::Graph, false, false, true), Zone::Graph);
        assert_eq!(beside(Zone::Files, false, true, true), Zone::Graph);
        assert_eq!(beside(Zone::Files, true, true, true), Zone::Files);
        assert_eq!(beside(Zone::Sidebar, true, true, true), Zone::Graph);
        assert_eq!(beside(Zone::Sidebar, false, true, true), Zone::Sidebar);
    }
}
