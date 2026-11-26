use crate::{
    Tab,
    commands::{Availability, Command, Group, Target, operating},
    settings::Settings,
    storage,
    theme::{MONO, TEXT_SMALL, palette},
    view::{self, Desk, Selection, Ui},
};
use gpui::{App, Context, Pixels, TextRun, Window, rgb};
use crate::theme::px;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(crate) const DESCRIPTION_MIN: f32 = 160.;
pub(crate) const ROW_EDGE: f32 = 2.;
pub(crate) const NAMED_AUTHOR: f32 = 60.;
const GRAPH_MIN: f32 = 38.;
const GRAPH_MAX: f32 = 640.;
const FOLLOW_MARGIN: f32 = 14.;
const AUTHOR_TEXT: f32 = 12.5;
const SHA_TEXT: f32 = 11.5;
const AUTHOR_LEAD: f32 = 16. + 6. + 8.;
const CELL_PAD: f32 = 8.;
const SLACK: f32 = 2.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Column {
    Graph,
    Description,
    Author,
    Changes,
    Date,
    Sha,
}

const FIXED: [Column; 5] = [Column::Graph, Column::Author, Column::Changes, Column::Date, Column::Sha];

impl Column {
    pub(crate) const RIGHT: [Column; 4] = [Column::Author, Column::Changes, Column::Date, Column::Sha];

    pub(crate) fn optional(self) -> bool {
        self == Column::Changes
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Column::Graph => "Graph",
            Column::Description => "Description",
            Column::Author => "Author",
            Column::Changes => "Changes",
            Column::Date => "Date",
            Column::Sha => "SHA",
        }
    }

    fn range(self) -> (f32, f32) {
        match self {
            Column::Graph => (GRAPH_MIN, GRAPH_MAX),
            Column::Description => (DESCRIPTION_MIN, f32::MAX),
            Column::Author => (28., 400.),
            Column::Changes => (48., 240.),
            Column::Date => (56., 240.),
            Column::Sha => (52., 200.),
        }
    }

    fn standard(self) -> f32 {
        match self {
            Column::Author => 140.,
            Column::Changes => 90.,
            Column::Date => 84.,
            Column::Sha => 66.,
            Column::Graph | Column::Description => 0.,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Layout {
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) hidden: Vec<Column>,
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) shown: Vec<Column>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) graph: Option<f32>,
    pub(crate) author: f32,
    pub(crate) changes: f32,
    pub(crate) date: f32,
    pub(crate) sha: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            hidden: Vec::new(),
            shown: Vec::new(),
            graph: None,
            author: Column::Author.standard(),
            changes: Column::Changes.standard(),
            date: Column::Date.standard(),
            sha: Column::Sha.standard(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Widths {
    pub(crate) graph: f32,
    pub(crate) description: f32,
    pub(crate) author: f32,
    pub(crate) changes: f32,
    pub(crate) date: f32,
    pub(crate) sha: f32,
}

impl Widths {
    pub(crate) fn of(&self, column: Column) -> f32 {
        match column {
            Column::Graph => self.graph,
            Column::Description => self.description,
            Column::Author => self.author,
            Column::Changes => self.changes,
            Column::Date => self.date,
            Column::Sha => self.sha,
        }
    }

    fn set(&mut self, column: Column, width: f32) {
        match column {
            Column::Graph => self.graph = width,
            Column::Description => self.description = width,
            Column::Author => self.author = width,
            Column::Changes => self.changes = width,
            Column::Date => self.date = width,
            Column::Sha => self.sha = width,
        }
    }
}

impl Layout {
    pub(crate) fn shows(&self, column: Column) -> bool {
        if column.optional() {
            return self.shown.contains(&column);
        }
        column == Column::Description || !self.hidden.contains(&column)
    }

    pub(crate) fn sanitized(mut self) -> Self {
        let fallback = Self::default();
        let clamp = |value: f32, column: Column, default: f32| {
            let (min, max) = column.range();
            if value.is_finite() { value.clamp(min, max) } else { default }
        };
        self.author = clamp(self.author, Column::Author, fallback.author);
        self.changes = clamp(self.changes, Column::Changes, fallback.changes);
        self.date = clamp(self.date, Column::Date, fallback.date);
        self.sha = clamp(self.sha, Column::Sha, fallback.sha);
        self.graph = self.graph.filter(|width| width.is_finite()).map(|width| clamp(width, Column::Graph, width));
        let hidden: BTreeSet<Column> =
            self.hidden.into_iter().filter(|column| *column != Column::Description && !column.optional()).collect();
        self.hidden = hidden.into_iter().collect();
        let shown: BTreeSet<Column> = self.shown.into_iter().filter(|column| column.optional()).collect();
        self.shown = shown.into_iter().collect();
        self
    }

    fn chosen(&self, column: Column, graph_auto: f32) -> f32 {
        if !self.shows(column) {
            return 0.;
        }
        match column {
            Column::Graph => self.graph.unwrap_or(graph_auto),
            Column::Author => self.author,
            Column::Changes => self.changes,
            Column::Date => self.date,
            Column::Sha => self.sha,
            Column::Description => 0.,
        }
    }

    pub(crate) fn resolve(&self, available: f32, graph_auto: f32) -> Widths {
        let fixed = FIXED;
        let mut widths = Widths::default();
        for column in fixed {
            widths.set(column, self.chosen(column, graph_auto));
        }
        let total: f32 = fixed.iter().map(|column| widths.of(*column)).sum();
        let room = (available - DESCRIPTION_MIN).max(0.);
        if total > room {
            let floors = fixed.map(|column| column.range().0.min(widths.of(column)));
            let least: f32 = floors.iter().sum();
            let spare = total - least;
            let scale = if spare > 0. { ((room - least).max(0.) / spare).min(1.) } else { 0. };
            for (column, low) in fixed.into_iter().zip(floors) {
                widths.set(column, low + (widths.of(column) - low) * scale);
            }
        }
        let used: f32 = fixed.iter().map(|column| widths.of(*column)).sum();
        widths.description = (available - used).max(0.);
        widths
    }

    fn with_width(mut self, column: Column, width: f32) -> Self {
        let (min, max) = column.range();
        let width = width.clamp(min, max);
        match column {
            Column::Graph => self.graph = Some(width),
            Column::Author => self.author = width,
            Column::Changes => self.changes = width,
            Column::Date => self.date = width,
            Column::Sha => self.sha = width,
            Column::Description => {}
        }
        self
    }

    pub(crate) fn dragged(&self, column: Column, x: f32, available: f32, graph_auto: f32) -> Self {
        let widths = self.resolve(available, graph_auto);
        let wanted = match column {
            Column::Graph => x - ROW_EDGE,
            Column::Description => return self.clone(),
            _ => {
                let after: f32 = Column::RIGHT
                    .iter()
                    .skip_while(|other| **other != column)
                    .skip(1)
                    .map(|other| widths.of(*other))
                    .sum();
                available + ROW_EDGE - after - x
            }
        };
        let others: f32 = FIXED
            .iter()
            .filter(|other| **other != column)
            .map(|other| widths.of(*other))
            .sum();
        let (min, _) = column.range();
        let room = (available - DESCRIPTION_MIN - others).max(min);
        self.clone().with_width(column, wanted.min(room))
    }

    pub(crate) fn toggled(&self, column: Column) -> Self {
        let mut next = self.clone();
        if column == Column::Description {
            return next;
        }
        if column.optional() {
            if next.shows(column) {
                next.shown.retain(|shown| *shown != column);
            } else {
                next.shown.push(column);
                next.shown.sort();
            }
            return next;
        }
        if next.shows(column) {
            next.hidden.push(column);
            next.hidden.sort();
        } else {
            next.hidden.retain(|hidden| *hidden != column);
        }
        next
    }
}

#[derive(Debug, Default)]
struct Follow {
    oid: Option<String>,
    rows: Option<usize>,
    done: bool,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) layout: Layout,
    pub(crate) offset: f32,
    pan_from: Option<(f32, f32)>,
    follow: Follow,
}

impl State {
    pub(crate) fn new(layout: Layout) -> Self {
        Self { layout, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HeaderDrag(pub(crate) Column);

#[derive(Clone, Copy, Debug)]
pub(crate) struct GraphPan {
    pub(crate) limit: f32,
}

#[derive(Clone, Debug)]
pub enum Action {
    Toggle(Column),
    Resize(Column, f32),
    Fit(Column),
    Reset,
    Pan(f32),
    Bodies,
}

pub(crate) fn available(centre: f32) -> f32 {
    (centre - ROW_EDGE).max(0.)
}

pub(crate) fn drawn(ui: &Ui, centre: f32) -> Widths {
    ui.graph_columns.layout.resolve(available(centre), ui.graph_auto_width())
}

pub(crate) fn limit(ui: &Ui, graph: f32) -> f32 {
    (ui.graph_content_width() - graph).max(0.)
}

pub(crate) fn into_view(offset: f32, x: f32, width: f32) -> f32 {
    if width <= 0. {
        return offset;
    }
    if x - FOLLOW_MARGIN < offset {
        (x - FOLLOW_MARGIN).max(0.)
    } else if x + FOLLOW_MARGIN > offset + width {
        x + FOLLOW_MARGIN - width
    } else {
        offset
    }
}

pub(crate) fn settle(ui: &mut Ui, centre: f32) {
    let graph = drawn(ui, centre).graph;
    let selected = match &ui.selection {
        Some(Selection::Commit(oid)) => Some(oid.as_str()),
        _ => None,
    };
    if ui.graph_columns.follow.oid.as_deref() != selected {
        ui.graph_columns.follow = Follow { oid: selected.map(str::to_owned), rows: None, done: selected.is_none() };
    }
    let rows = ui.history.len();
    if !ui.graph_columns.follow.done && ui.graph_columns.follow.rows != Some(rows) && graph > 0. {
        ui.graph_columns.follow.rows = Some(rows);
        let wanted = ui.graph_columns.follow.oid.as_deref();
        if let Some(index) = ui.history.iter().position(|row| Some(row.oid.as_str()) == wanted) {
            if let Some(x) = ui.graph_node_x(index) {
                ui.graph_columns.offset = into_view(ui.graph_columns.offset, x, graph);
            }
            ui.graph_columns.follow.done = true;
        }
    }
    let most = limit(ui, graph);
    ui.graph_columns.offset = ui.graph_columns.offset.clamp(0., most);
}

fn store(layout: &Layout, cx: &mut App) {
    let layout = layout.clone();
    Settings::update(cx, |settings| settings.graph_columns = layout);
}

pub(crate) fn drag<T: Desk>(app: &mut T, column: Column, x: f32, header: f32, cx: &mut Context<T>) {
    let ui = app.ui();
    let next = ui.graph_columns.layout.dragged(column, x, available(header), ui.graph_auto_width());
    if next != ui.graph_columns.layout {
        store(&next, cx);
        app.ui_mut().graph_columns.layout = next;
        cx.notify();
    }
}

pub(crate) fn pan<T: Desk>(app: &mut T, by: f32, limit: f32, cx: &mut Context<T>) {
    let state = &mut app.ui_mut().graph_columns;
    let next = (state.offset + by).clamp(0., limit.max(0.));
    if next != state.offset {
        state.offset = next;
        cx.notify();
    }
}

pub(crate) fn start_pan(ui: &mut Ui, x: Pixels) {
    ui.graph_columns.pan_from = Some((f32::from(x), ui.graph_columns.offset));
}

pub(crate) fn panned<T: Desk>(app: &mut T, x: Pixels, limit: f32, cx: &mut Context<T>) {
    let Some((from, offset)) = app.ui().graph_columns.pan_from else { return };
    let next = (offset + from - f32::from(x)).clamp(0., limit.max(0.));
    if next != app.ui().graph_columns.offset {
        app.ui_mut().graph_columns.offset = next;
        cx.notify();
    }
}

fn widest(window: &Window, texts: BTreeSet<String>, size: f32, mono: bool) -> f32 {
    let mut font = window.text_style().font();
    if mono {
        font.family = MONO.into();
    }
    texts
        .into_iter()
        .map(|text| {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: rgb(palette().text).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(window.text_system().shape_line(text.into(), px(size), &[run], None).width)
        })
        .fold(0., f32::max)
}

fn fitted(ui: &Ui, column: Column, window: &Window) -> Option<f32> {
    let stashes = ui.stash.entries.iter().map(|entry| (&entry.author, &entry.date, entry.time));
    let rows = ui.history.iter().map(|row| (&row.author, &row.date, row.time)).chain(stashes);
    match column {
        Column::Graph => Some(ui.graph_content_width()),
        Column::Description => None,
        Column::Author => {
            let names: BTreeSet<String> = rows.map(|(author, _, _)| author.clone()).collect();
            Some(AUTHOR_LEAD + widest(window, names, AUTHOR_TEXT, false) + SLACK)
        }
        Column::Date => {
            let now = view::now();
            let dates: BTreeSet<String> = rows.map(|(_, date, time)| view::when(time, date, now)).collect();
            Some(widest(window, dates, TEXT_SMALL, false) + CELL_PAD + SLACK)
        }
        Column::Sha => Some(widest(window, ["0123456".to_owned()].into(), SHA_TEXT, true) + CELL_PAD + SLACK),
        Column::Changes => Some(Column::Changes.standard()),
    }
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let layout = &tab.ui.graph_columns.layout;
    let next = match action {
        Action::Toggle(Column::Description) => {
            tab.ui.notices.show(crate::notice::Notice::new(view::Tone::Info, "Description cannot be hidden", ""));
            cx.notify();
            return;
        }
        Action::Toggle(column) => layout.toggled(column),
        Action::Resize(column, width) => layout.clone().with_width(column, width),
        Action::Fit(column) => match fitted(&tab.ui, column, window) {
            Some(width) => layout.clone().with_width(column, width),
            None => return,
        },
        Action::Bodies => {
            show_bodies(tab, !tab.ui.bodies, cx);
            cx.notify();
            return;
        }
        Action::Reset => {
            if !tab.ui.refs.show_labels {
                tab.ui.refs.show_labels = true;
                Settings::update(cx, |settings| settings.show_ref_labels = true);
            }
            show_bodies(tab, false, cx);
            tab.ui.graph_columns.offset = 0.;
            Layout::default()
        }
        Action::Pan(by) => {
            let graph = drawn(&tab.ui, tab.ui.columns_drawn(f32::from(window.viewport_size().width)).1).graph;
            let most = limit(&tab.ui, graph);
            pan(tab, by, most, cx);
            return;
        }
    };
    if next != tab.ui.graph_columns.layout {
        store(&next, cx);
        tab.ui.graph_columns.layout = next;
    }
    cx.notify();
}

fn show_bodies(tab: &mut Tab, show: bool, cx: &mut Context<Tab>) {
    if tab.ui.bodies == show {
        return;
    }
    tab.ui.bodies = show;
    Settings::update(cx, |settings| settings.commit_bodies = show);
    tab.load_history(true, cx);
}

fn on_header(target: &Target) -> bool {
    *target == Target::Columns
}

fn shown_here(_: &Ui, target: &Target, _: &App) -> Availability {
    if on_header(target) { Availability::Enabled } else { Availability::Hidden }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "columns.labels",
        Group::View,
        |_, _| "Branch and tag labels".into(),
        |ui, target, _| {
            if on_header(target) { Availability::unless(operating(ui)) } else { Availability::Hidden }
        },
        |_, _| Some(view::Action::Refs(crate::refs::Action::ToggleLabels)),
    )
    .tick(|ui, _| crate::refs::show_labels(&ui.refs)),
    Command::new(
        "columns.graph",
        Group::View,
        |_, _| Column::Graph.title().into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Toggle(Column::Graph))),
    )
    .tick(|ui, _| ui.graph_columns.layout.shows(Column::Graph)),
    Command::new(
        "columns.description",
        Group::View,
        |_, _| Column::Description.title().into(),
        |_, target, _| {
            if on_header(target) {
                Availability::Disabled("Description cannot be hidden".to_owned())
            } else {
                Availability::Hidden
            }
        },
        |_, _| None,
    )
    .tick(|_, _| true),
    Command::new(
        "columns.bodies",
        Group::View,
        |_, _| "Commit body preview".into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Bodies)),
    )
    .words(&["description", "message", "body", "graph"])
    .tick(|ui, _| ui.bodies),
    Command::new(
        "columns.author",
        Group::View,
        |_, _| Column::Author.title().into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Toggle(Column::Author))),
    )
    .tick(|ui, _| ui.graph_columns.layout.shows(Column::Author)),
    Command::new(
        "columns.changes",
        Group::View,
        |_, _| Column::Changes.title().into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Toggle(Column::Changes))),
    )
    .words(&["size", "lines", "additions", "deletions", "stat"])
    .tick(|ui, _| ui.graph_columns.layout.shows(Column::Changes)),
    Command::new(
        "columns.date",
        Group::View,
        |_, _| Column::Date.title().into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Toggle(Column::Date))),
    )
    .tick(|ui, _| ui.graph_columns.layout.shows(Column::Date)),
    Command::new(
        "columns.sha",
        Group::View,
        |_, _| Column::Sha.title().into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Toggle(Column::Sha))),
    )
    .tick(|ui, _| ui.graph_columns.layout.shows(Column::Sha)),
    Command::new(
        "columns.reset",
        Group::Settings,
        |_, _| "Reset columns".into(),
        shown_here,
        |_, _| Some(view::Action::Columns(Action::Reset)),
    )
    .icon("undo"),
];

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO: f32 = 80.;

    fn layout() -> Layout {
        Layout::default()
    }

    #[test]
    fn wide_centres_draw_every_column_at_its_chosen_width() {
        let widths = layout().resolve(1000., AUTO);
        assert_eq!((widths.graph, widths.author, widths.date, widths.sha), (AUTO, 140., 84., 66.));
        assert_eq!(widths.description, 1000. - AUTO - 140. - 84. - 66.);
    }

    #[test]
    fn hidden_columns_take_no_room_and_description_never_hides() {
        let hidden = layout().toggled(Column::Author).toggled(Column::Sha).toggled(Column::Description);
        assert_eq!(hidden.hidden, [Column::Author, Column::Sha]);
        assert!(hidden.shows(Column::Description));
        let widths = hidden.resolve(800., AUTO);
        assert_eq!((widths.author, widths.sha), (0., 0.));
        assert_eq!(widths.description, 800. - AUTO - 84.);
        let shown = hidden.toggled(Column::Author);
        assert_eq!(shown.hidden, [Column::Sha]);
    }

    #[test]
    fn narrow_centres_shrink_columns_to_their_minimums_before_description_gives_way() {
        let wide = layout().resolve(AUTO + 140. + 84. + 66. + DESCRIPTION_MIN, AUTO);
        assert_eq!(wide.description, DESCRIPTION_MIN);
        assert_eq!(wide.author, 140.);
        let squeezed = layout().resolve(500., AUTO);
        assert!((squeezed.description - DESCRIPTION_MIN).abs() < 0.01, "{squeezed:?}");
        assert!(squeezed.author < 140. && squeezed.author > 28., "{squeezed:?}");
        let fixed = squeezed.graph + squeezed.author + squeezed.date + squeezed.sha;
        assert!((fixed - (500. - DESCRIPTION_MIN)).abs() < 0.01);
        let tight = layout().resolve(250., AUTO);
        assert_eq!((tight.graph, tight.author, tight.date, tight.sha), (GRAPH_MIN, 28., 56., 52.));
        assert_eq!(tight.description, 250. - GRAPH_MIN - 28. - 56. - 52.);
        let none = layout().resolve(100., AUTO);
        assert_eq!(none.description, 0.);
    }

    #[test]
    fn dragging_a_divider_follows_the_pointer_within_the_room_left() {
        let available = 1000.;
        let graph = layout().dragged(Column::Graph, 202., available, AUTO);
        assert_eq!(graph.graph, Some(200.));
        let widths = layout().resolve(available, AUTO);
        let sha_left = available + ROW_EDGE - widths.sha;
        let sha = layout().dragged(Column::Sha, sha_left - 30., available, AUTO);
        assert_eq!(sha.sha, 96.);
        let date_left = available + ROW_EDGE - widths.sha - widths.date;
        let date = layout().dragged(Column::Date, date_left + 10., available, AUTO);
        assert_eq!(date.date, 74.);
        assert_eq!(layout().dragged(Column::Author, 0., available, AUTO).author, 400.);
        let author = layout().dragged(Column::Author, 0., 600., AUTO);
        assert_eq!(author.author, 600. - DESCRIPTION_MIN - AUTO - 84. - 66.);
        let tiny = layout().dragged(Column::Graph, -50., available, AUTO);
        assert_eq!(tiny.graph, Some(GRAPH_MIN));
        assert_eq!(layout().dragged(Column::Description, 300., available, AUTO), layout());
    }

    #[test]
    fn stored_layouts_are_cleaned_and_unknown_columns_dropped() {
        let read: Layout = serde_json::from_str(
            r#"{"hidden": ["sha", "description", "changes", "sha"], "graph": 9000, "author": -4, "date": 100}"#,
        )
        .unwrap();
        let clean = read.sanitized();
        assert_eq!(clean.hidden, [Column::Sha]);
        assert_eq!(clean.graph, Some(GRAPH_MAX));
        assert_eq!((clean.author, clean.date, clean.sha), (28., 100., 66.));
        let saved = serde_json::to_value(&clean).unwrap();
        assert_eq!(saved["hidden"], serde_json::json!(["sha"]));
    }

    #[test]
    fn the_changes_column_is_off_until_chosen_and_sits_between_author_and_date() {
        assert!(!layout().shows(Column::Changes));
        let before: Layout = serde_json::from_str(r#"{"hidden": [], "author": 150}"#).unwrap();
        assert!(!before.sanitized().shows(Column::Changes));
        let chosen = layout().toggled(Column::Changes);
        assert_eq!(chosen.shown, [Column::Changes]);
        assert!(chosen.hidden.is_empty() && chosen.shows(Column::Changes));
        let widths = chosen.resolve(1000., AUTO);
        assert_eq!(widths.changes, 90.);
        assert_eq!(widths.description, 1000. - AUTO - 140. - 90. - 84. - 66.);
        assert_eq!(layout().resolve(1000., AUTO).changes, 0.);
        let date_left = 1000. + ROW_EDGE - widths.sha - widths.date;
        let changes_left = date_left - widths.changes;
        assert_eq!(chosen.dragged(Column::Changes, changes_left - 20., 1000., AUTO).changes, 110.);
        assert_eq!(Column::RIGHT, [Column::Author, Column::Changes, Column::Date, Column::Sha]);
        assert!(!chosen.toggled(Column::Changes).shows(Column::Changes));
        let odd: Layout = serde_json::from_str(r#"{"shown": ["sha", "changes", "changes"], "changes": 9}"#).unwrap();
        let clean = odd.sanitized();
        assert_eq!((clean.shown.as_slice(), clean.changes), ([Column::Changes].as_slice(), 48.));
    }

    #[test]
    fn a_node_is_brought_into_view_with_a_margin() {
        assert_eq!(into_view(0., 30., 100.), 0.);
        assert_eq!(into_view(0., 300., 100.), 214.);
        assert_eq!(into_view(250., 100., 100.), 86.);
        assert_eq!(into_view(50., 10., 100.), 0.);
        assert_eq!(into_view(40., 90., 0.), 40.);
    }
}
