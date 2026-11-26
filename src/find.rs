use crate::{
    Tab,
    commands::{Availability, Bar, Command, Group, Target, on_repo},
    diff_doc::DiffDoc,
    kit,
    theme::*,
    view::{self, Centre, CommitInput, Desk, DiffMode, Selection, Ui, commit_input::FieldCommand},
};
use gpui::{
    App, Context, Div, Entity, Focusable, HighlightStyle, KeyBinding, Pixels, ScrollStrategy, Window, actions, div,
    point, prelude::*, px, rgb,
};
use std::{ops::Range, path::PathBuf};

actions!(find, [NextMatch, PreviousMatch]);

const EDGE: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    Diff,
    Resolver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Pane {
    Diff,
    Current,
    Incoming,
    Original,
    Result,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) pane: Pane,
    pub(crate) row: usize,
    pub(crate) range: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Front,
    Close,
    Step(isize),
}

type Subject = (Option<Selection>, Option<PathBuf>);

pub(crate) struct State {
    pub(crate) input: Entity<CommitInput>,
    surface: Option<Surface>,
    query: String,
    hits: Vec<Hit>,
    current: Option<usize>,
    anchor: Option<(Subject, usize, usize)>,
    seen: Option<u64>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let input = cx.new(|cx| CommitInput::with_placeholder(cx, "Find"));
        cx.observe(&input, |tab, _, cx| typed(tab, cx)).detach();
        Self { input, surface: None, query: String::new(), hits: Vec::new(), current: None, anchor: None, seen: None }
    }

    pub(crate) fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub(crate) fn current(&self) -> Option<&Hit> {
        self.hits.get(self.current?)
    }

    pub(crate) fn counter(&self) -> Option<String> {
        if self.query.is_empty() {
            return None;
        }
        Some(match self.current {
            _ if self.hits.is_empty() => "No results".to_owned(),
            Some(index) => format!("{} of {}", index + 1, self.hits.len()),
            None => format!("{} found", self.hits.len()),
        })
    }

    fn close(&mut self) {
        self.surface = None;
        self.hits.clear();
        self.current = None;
        self.anchor = None;
        self.seen = None;
    }
}

pub(crate) fn sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

fn folded(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

pub(crate) fn occurrences(text: &str, query: &str, sensitive: bool) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    if sensitive {
        return text.match_indices(query).map(|(at, found)| at..at + found.len()).collect();
    }
    if query.is_ascii() {
        let (bytes, wanted) = (text.as_bytes(), query.as_bytes());
        let mut found = Vec::new();
        let mut at = 0;
        while at + wanted.len() <= bytes.len() {
            if bytes[at..at + wanted.len()].eq_ignore_ascii_case(wanted) {
                found.push(at..at + wanted.len());
                at += wanted.len();
            } else {
                at += 1;
            }
        }
        return found;
    }
    let wanted: Vec<char> = query.chars().collect();
    let mut found = Vec::new();
    let mut starts = text.char_indices();
    while let Some((start, _)) = starts.next() {
        let mut rest = text[start..].char_indices();
        let mut end = None;
        for (index, want) in wanted.iter().enumerate() {
            match rest.next() {
                Some((offset, have)) if folded(have, *want) => {
                    if index + 1 == wanted.len() {
                        end = Some(start + offset + have.len_utf8());
                    }
                }
                _ => break,
            }
        }
        if let Some(end) = end {
            found.push(start..end);
            while starts.clone().next().is_some_and(|(next, _)| next < end) {
                starts.next();
            }
        }
    }
    found
}

pub(crate) fn diff_hits(doc: &DiffDoc, query: &str) -> Vec<Hit> {
    let case = sensitive(query);
    doc.lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind.is_code())
        .flat_map(|(row, line)| {
            occurrences(doc.code(line), query, case).into_iter().map(move |range| Hit { pane: Pane::Diff, row, range })
        })
        .collect()
}

pub(crate) fn resolver_hits(merge: &crate::merge::State, query: &str) -> Vec<Hit> {
    let case = sensitive(query);
    let mut hits = Vec::new();
    merge.find_lines(|pane, row, text| {
        hits.extend(occurrences(text, query, case).into_iter().map(|range| Hit { pane, row, range }));
    });
    hits
}

fn front_filter(ui: &Ui) -> Option<&Entity<CommitInput>> {
    match ui.help.sheet() {
        Some(crate::help::Sheet::Shortcuts) => Some(&ui.help.filter),
        Some(_) => None,
        None if ui.preferences.shows_filter() => Some(&ui.preferences.filter),
        None if ui.refs.list_open && ui.refs.draft.is_none() => Some(&ui.refs.find),
        None if ui.commit_options.picking() => Some(&ui.commit_options.filter),
        None if ui.author_picker.open() => Some(&ui.author_picker.filter),
        None => None,
    }
}

pub(crate) fn filters(ui: &Ui) -> bool {
    front_filter(ui).is_some()
}

pub(crate) fn here(ui: &Ui) -> Option<Surface> {
    if ui.preferences.open() || ui.repo.is_empty() {
        return None;
    }
    match view::centre(ui) {
        Centre::Diff | Centre::FileHistory | Centre::Blame | Centre::Browse if view::diff_shown(ui) && crate::image_diff::showing(ui).is_none() => {
            Some(Surface::Diff)
        }
        Centre::Resolver => Some(Surface::Resolver),
        _ => None,
    }
}

pub(crate) fn shown(ui: &Ui) -> Option<Surface> {
    ui.find.surface.filter(|surface| here(ui) == Some(*surface))
}

fn subject(ui: &Ui) -> Subject {
    let path = match &ui.selection {
        Some(Selection::Commit(_)) => ui.inspection.path.clone(),
        Some(Selection::Stash(_)) => ui.stash.open_path().map(std::path::Path::to_path_buf),
        Some(Selection::Compare { .. }) => ui.compare.path.clone(),
        _ => None,
    };
    (ui.selection.clone(), path)
}

fn visual(ui: &Ui, hit: &Hit) -> Option<usize> {
    let first = view::diff_visual_of(ui, hit.row)?;
    Some(first + view::diff_part_at(ui, hit.row, hit.range.start))
}

fn compute(ui: &mut Ui) {
    let query = ui.find.query.clone();
    ui.find.hits = match ui.find.surface {
        Some(Surface::Diff) if !query.is_empty() => diff_hits(&ui.diff, &query),
        Some(Surface::Resolver) if !query.is_empty() => resolver_hits(&ui.merge, &query),
        _ => Vec::new(),
    };
    ui.find.seen = ui.merge.find_key();
}

fn first_in_view(ui: &Ui) -> Option<usize> {
    if ui.find.hits.is_empty() {
        return None;
    }
    if ui.find.surface != Some(Surface::Diff) {
        return Some(0);
    }
    let top = crate::diff_options::viewport(ui).map_or(0, |(top, _)| top);
    Some(ui.find.hits.iter().position(|hit| visual(ui, hit).is_some_and(|at| at >= top)).unwrap_or(0))
}

fn set_current(ui: &mut Ui, current: Option<usize>) {
    ui.find.current = current;
    let at = current.and_then(|index| ui.find.hits.get(index)).map(|hit| (hit.row, hit.range.start));
    ui.find.anchor = at.map(|(row, start)| (subject(ui), row, start));
}

pub(crate) fn reloaded(ui: &mut Ui) {
    if ui.find.surface != Some(Surface::Diff) {
        return;
    }
    compute(ui);
    if ui.diff.lines.is_empty() {
        ui.find.current = None;
        return;
    }
    let here = subject(ui);
    let current = match ui.find.anchor.take() {
        _ if ui.find.hits.is_empty() => None,
        Some((seen, row, start)) if seen == here => Some(
            ui.find
                .hits
                .iter()
                .position(|hit| (hit.row, hit.range.start) >= (row, start))
                .unwrap_or(ui.find.hits.len() - 1),
        ),
        _ => Some(0),
    };
    set_current(ui, current);
    reveal(ui, None);
}

pub(crate) fn settle(ui: &mut Ui) {
    let Some(surface) = ui.find.surface else { return };
    if here(ui) != Some(surface) {
        ui.find.close();
        return;
    }
    if surface == Surface::Resolver && ui.merge.find_key() != ui.find.seen {
        let before = ui.find.current;
        compute(ui);
        let current = before.filter(|_| !ui.find.hits.is_empty()).map(|index| index.min(ui.find.hits.len() - 1));
        let current = current.or_else(|| first_in_view(ui));
        set_current(ui, current);
    }
}

fn typed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let text = tab.ui.find.input.read(cx).text().to_owned();
    if text == tab.ui.find.query {
        return;
    }
    tab.ui.find.query = text;
    if tab.ui.find.surface.is_none() {
        return;
    }
    compute(&mut tab.ui);
    let current = first_in_view(&tab.ui);
    set_current(&mut tab.ui, current);
    reveal(&tab.ui, Some(cx));
    cx.notify();
}

fn cell(ui: &Ui, cx: &App) -> Option<Pixels> {
    let text = cx.text_system();
    let face = &ui.diff_text.face;
    let font = text.resolve_font(&gpui::font(face.family.clone()));
    text.ch_advance(font, face.text()).ok()
}

fn across(scroll: f32, width: f32, start: f32, end: f32) -> Option<f32> {
    if start >= scroll && end + EDGE <= scroll + width {
        return None;
    }
    let wanted = if start < scroll || end - start + EDGE * 2. > width { start - EDGE } else { end + EDGE - width };
    Some(wanted.max(0.))
}

fn reveal_across(ui: &Ui, hit: &Hit, cx: &App) {
    if ui.diff_options.wrapped.is_some() {
        return;
    }
    let Some(cell) = cell(ui, cx).map(f32::from) else { return };
    let Some(line) = ui.diff.lines.get(hit.row) else { return };
    let code = ui.diff.code(line);
    let tab = ui.diff_text.face.tab.max(1);
    let to = |offset: usize| cell * view::diff_cells(code.get(..offset).unwrap_or(code), tab) as f32;
    let (start, end) = (to(hit.range.start), to(hit.range.end));
    let base = ui.diff_scroll.0.borrow().base_handle.clone();
    let width = f32::from(base.bounds().size.width);
    if width <= 0. {
        return;
    }
    let (pan, room, gutter) = match ui.shown_mode() {
        DiffMode::Unified => (base, width, ui.diff_text.face.unified_gutter() + crate::blame::width(ui)),
        DiffMode::Split => (ui.split_pan().clone(), (width - 1.) / 2. - ui.diff_text.face.split_gutter(), 0.),
    };
    let offset = pan.offset();
    if let Some(scroll) = across(-f32::from(offset.x), room, start + gutter, end + gutter) {
        pan.set_offset(point(px(-scroll), offset.y));
    }
}

fn reveal(ui: &Ui, cx: Option<&App>) {
    let Some(hit) = ui.find.current() else { return };
    match hit.pane {
        Pane::Diff => {
            let Some(at) = visual(ui, hit) else { return };
            ui.diff_scroll.scroll_to_item(at, ScrollStrategy::Center);
            if let Some(cx) = cx {
                reveal_across(ui, hit, cx);
            }
        }
        pane => ui.merge.reveal(pane, hit.row),
    }
}

fn open(tab: &mut Tab, surface: Surface, window: &mut Window, cx: &mut Context<Tab>) {
    let ui = &mut tab.ui;
    let fresh = ui.find.surface != Some(surface);
    ui.find.query = ui.find.input.read(cx).text().to_owned();
    if fresh {
        ui.find.surface = Some(surface);
        ui.find.anchor = None;
        compute(ui);
        let current = first_in_view(ui);
        set_current(ui, current);
    }
    ui.text.unfocus();
    reveal(&tab.ui, Some(cx));
    let input = tab.ui.find.input.clone();
    window.focus(&input.focus_handle(cx));
    input.update(cx, |input, cx| input.choose(FieldCommand::SelectAll, window, cx));
}

fn field(input: &Entity<CommitInput>, window: &mut Window, cx: &mut Context<Tab>) {
    window.focus(&input.focus_handle(cx));
    input.update(cx, |input, cx| input.choose(FieldCommand::SelectAll, window, cx));
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Front if filters(&tab.ui) => {
            if let Some(filter) = front_filter(&tab.ui).cloned() {
                field(&filter, window, cx);
            }
        }
        Action::Front => match here(&tab.ui) {
            Some(surface) => open(tab, surface, window, cx),
            None => {
                let action = match crate::file_list::here(&tab.ui) {
                    _ if view::centre(&tab.ui) == Centre::Browse => view::Action::Browse(crate::browse::Action::FocusFilter),
                    Some(panel) => view::Action::FileList(crate::file_list::Action::Filter(panel)),
                    None => view::Action::Search(crate::search::Action::Open),
                };
                return tab.dispatch(action, window, cx);
            }
        },
        Action::Close if tab.ui.find.surface.is_some() => {
            tab.ui.find.close();
            view::focus(&tab.ui, window);
        }
        Action::Step(delta) if shown(&tab.ui).is_some() => {
            let count = tab.ui.find.hits.len();
            if count == 0 {
                return;
            }
            let next = match tab.ui.find.current {
                Some(index) => (index as isize + delta).rem_euclid(count as isize) as usize,
                None if delta < 0 => count - 1,
                None => first_in_view(&tab.ui).unwrap_or(0),
            };
            set_current(&mut tab.ui, Some(next));
            reveal(&tab.ui, Some(cx));
        }
        _ => return,
    }
    cx.notify();
}

pub(crate) fn styles() -> (HighlightStyle, HighlightStyle) {
    let found = HighlightStyle { background_color: Some(rgb(palette().find_match).into()), ..Default::default() };
    let focus = HighlightStyle {
        background_color: Some(rgb(palette().find_current).into()),
        color: Some(rgb(palette().find_current_ink).into()),
        ..Default::default()
    };
    (found, focus)
}

pub(crate) fn marks(ui: &Ui, pane: Pane, row: usize) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let state = &ui.find;
    let surface = if pane == Pane::Diff { Surface::Diff } else { Surface::Resolver };
    if state.hits.is_empty() || shown(ui) != Some(surface) {
        return Default::default();
    }
    let start = state.hits.partition_point(|hit| (hit.pane, hit.row) < (pane, row));
    let mut found = Vec::new();
    let mut focus = Vec::new();
    for (index, hit) in state.hits.iter().enumerate().skip(start) {
        if (hit.pane, hit.row) != (pane, row) {
            break;
        }
        if state.current == Some(index) {
            focus.push(hit.range.clone());
        } else {
            found.push(hit.range.clone());
        }
    }
    (found, focus)
}

pub(crate) fn bar<T: Desk>(app: &T, surface: Surface, cx: &Context<T>) -> Option<Div> {
    let ui = app.ui();
    if shown(ui) != Some(surface) {
        return None;
    }
    let state = &ui.find;
    let stepping = !state.hits.is_empty();
    let step = |id: &'static str, icon: &'static str, tip: &'static str, delta: isize| {
        kit::icon_btn(id, icon, tip, view::Action::Find(Action::Step(delta)), cx)
            .when(!stepping, |d| d.opacity(0.42))
    };
    Some(
        div()
            .key_context("DiffFind")
            .h(px(32.))
            .flex_shrink_0()
            .pl_3()
            .pr_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(palette().panel))
            .border_b_1()
            .border_color(rgb(palette().line))
            .child(kit::icon("search").text_color(rgb(palette().text_3)))
            .child(div().w(px(240.)).flex_shrink_0().child(state.input.clone()))
            .child(
                div()
                    .min_w(px(70.))
                    .flex_shrink_0()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(if stepping || state.query.is_empty() { palette().text_2 } else { palette().deleted }))
                    .children(state.counter()),
            )
            .child(step("find-previous", "chevron-up", "Previous match (⇧⌘G)", -1))
            .child(step("find-next", "chevron-down", "Next match (⌘G)", 1))
            .child(kit::grow())
            .when(!state.query.is_empty() && !sensitive(&state.query), |d| d.child(kit::muted("Ignoring case")))
            .when(sensitive(&state.query), |d| d.child(kit::muted("Matching case")))
            .child(kit::icon_btn("close-find", "x", "Close find (Esc)", view::Action::Find(Action::Close), cx)),
    )
}

pub(crate) fn bindings() -> Vec<KeyBinding> {
    let field = Some("DiffFind > CommitField");
    vec![KeyBinding::new("enter", NextMatch, field), KeyBinding::new("shift-enter", PreviousMatch, field)]
}

pub(crate) fn title(ui: &Ui) -> &'static str {
    match here(ui).filter(|_| !filters(ui)) {
        Some(Surface::Diff) if ui.diff.file.is_some() => "Find in file…",
        Some(Surface::Diff) => "Find in diff…",
        Some(Surface::Resolver) => "Find in conflict…",
        None if crate::file_list::here(ui).is_some() || view::centre(ui) == Centre::Browse => "Filter files…",
        None => "Find commits…",
    }
}

fn stepping(ui: &Ui, target: &Target, _: &App) -> Availability {
    if !on_repo(ui, target) {
        Availability::Hidden
    } else if shown(ui).is_none() {
        Availability::Disabled("Open the find bar with ⌘F first".to_owned())
    } else if ui.find.hits.is_empty() {
        Availability::Disabled("Nothing matches".to_owned())
    } else {
        Availability::Enabled
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "view.find-next",
        Group::View,
        |_, _| "Find next".into(),
        stepping,
        |_, _| Some(view::Action::Find(Action::Step(1))),
    )
    .keys("cmd-g")
    .bar(Bar::Edit)
    .words(&["search", "match", "again"]),
    Command::new(
        "view.find-previous",
        Group::View,
        |_, _| "Find previous".into(),
        stepping,
        |_, _| Some(view::Action::Find(Action::Step(-1))),
    )
    .keys("cmd-shift-g")
    .bar(Bar::Edit)
    .words(&["search", "match", "back"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::{Source, DiffDoc};
    use std::collections::HashMap;

    fn doc(text: &str) -> DiffDoc {
        DiffDoc::build(&Source {
            text,
            stageable: true,
            truncated: false,
            sizes: &HashMap::new(),
            working_size: None,
            options: Default::default(),
        })
    }

    #[test]
    fn a_lower_case_query_ignores_case_and_an_upper_case_letter_makes_it_exact() {
        let text = "Needle needle NEEDLE needles";
        assert_eq!(occurrences(text, "needle", sensitive("needle")), [0..6, 7..13, 14..20, 21..27]);
        assert_eq!(occurrences(text, "Needle", sensitive("Needle")), [0..6]);
        assert_eq!(occurrences("aaaa", "aa", false), [0..2, 2..4]);
        assert_eq!(occurrences("Straße STRASSE straße", "straße", false), [0..7, 16..23]);
        assert_eq!(occurrences("ÉCOLE école", "école", false), [0..6, 7..13]);
        assert_eq!(occurrences("€ x € x", "€ x", false), [0..5, 6..11]);
        assert!(occurrences("short", "longer query", false).is_empty());
        assert!(occurrences("anything", "", false).is_empty());
        assert!(!sensitive("ünïcode 42"));
        assert!(sensitive("Ünïcode"));
    }

    #[test]
    fn diff_matches_cover_code_rows_only_and_stay_in_document_order() {
        let patch = "diff --git a/tide.rs b/tide.rs\nindex 1111111..2222222 100644\n--- a/tide.rs\n+++ b/tide.rs\n@@ -1,3 +1,3 @@ fn tide()\n let tide = 1;\n-let low = tide;\n+let low = TIDE + tide;\n fn tide() {}\n";
        let doc = doc(patch);
        let hits = diff_hits(&doc, "tide");
        let found: Vec<(String, &str)> =
            hits.iter().map(|hit| (doc.text(&doc.lines[hit.row]).to_owned(), &doc.code(&doc.lines[hit.row])[hit.range.clone()])).collect();
        assert_eq!(
            found,
            [
                (" let tide = 1;".to_owned(), "tide"),
                ("-let low = tide;".to_owned(), "tide"),
                ("+let low = TIDE + tide;".to_owned(), "TIDE"),
                ("+let low = TIDE + tide;".to_owned(), "tide"),
                (" fn tide() {}".to_owned(), "tide"),
            ]
        );
        assert!(hits.windows(2).all(|pair| (pair[0].row, pair[0].range.start) < (pair[1].row, pair[1].range.start)));
        assert_eq!(diff_hits(&doc, "TIDE").len(), 1);
        assert!(diff_hits(&doc, "b/tide").is_empty());
    }

    #[test]
    fn a_match_off_screen_scrolls_just_enough_and_a_wide_one_shows_its_start() {
        let scroll = across;
        assert_eq!(scroll(0., 400., 100., 140.), None);
        assert_eq!(scroll(0., 400., 500., 540.), Some(540. + EDGE - 400.));
        assert_eq!(scroll(300., 400., 100., 140.), Some(100. - EDGE));
        assert_eq!(scroll(0., 100., 500., 700.), Some(500. - EDGE));
        assert_eq!(scroll(0., 400., 10., 20.), None);
        assert_eq!(scroll(50., 400., 10., 20.), Some(0.));
    }
}
