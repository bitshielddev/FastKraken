use super::{Action, Desk, Discard, Selection, Ui};
use crate::{
    diff_doc::{self, Blank, DiffDoc, Kind, Line, PatchState, Run, runs},
    diff_select::{self, Column},
    diff_text::{self, Face},
    kit,
    syntax::Token,
    theme::*,
};
use gpui::{
    Context, Div, ElementId, FontWeight, HighlightStyle, ListHorizontalSizingBehavior, MouseButton,
    MouseDownEvent, Pixels, SharedString, Stateful, StyledText, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use std::ops::Range;

pub(crate) const WHOLE_RENAME: &str = "Unstage the file to stage parts of it";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    #[default]
    Unified,
    Split,
}

#[derive(Debug, PartialEq, Eq)]
enum SplitRow {
    Shared(usize),
    Sides {
        old: Option<usize>,
        new: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) start: usize,
    pub(crate) deleted: Option<usize>,
    pub(crate) added: Option<usize>,
}

#[derive(Default)]
pub(crate) struct Layout {
    split: Vec<SplitRow>,
    split_of: Vec<usize>,
    changes: Vec<Change>,
    widest_unified: Option<usize>,
    widest_code: Option<usize>,
}

impl Layout {
    pub(crate) fn new(doc: &DiffDoc, tab: usize) -> Self {
        let widest_unified = doc
            .lines
            .iter()
            .enumerate()
            .max_by_key(|(_, row)| width_in_cells(doc.text(row), tab) + if row.kind.is_code() { 13 } else { 34 })
            .map(|(index, _)| index);
        let widest_code = doc
            .lines
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind.is_code())
            .max_by_key(|(_, row)| width_in_cells(doc.code(row), tab))
            .map(|(index, _)| index);
        let mut split = Vec::with_capacity(doc.lines.len());
        let mut changes = Vec::new();
        runs(&doc.lines, |run| match run {
            Run::Shared(row) => split.push(SplitRow::Shared(row)),
            Run::Both(row) => split.push(SplitRow::Sides { old: Some(row), new: Some(row) }),
            Run::Change { deleted, added, old_note, new_note } => {
                let (deleted_first, added_first) = (deleted.first().copied(), added.first().copied());
                if let Some(start) = deleted_first.into_iter().chain(added_first).min() {
                    changes.push(Change { start, deleted: deleted_first, added: added_first });
                }
                for offset in 0..deleted.len().max(added.len()) {
                    split.push(SplitRow::Sides {
                        old: deleted.get(offset).copied(),
                        new: added.get(offset).copied(),
                    });
                }
                if old_note.is_some() || new_note.is_some() {
                    split.push(SplitRow::Sides { old: old_note, new: new_note });
                }
            }
        });
        if let Some(file) = &doc.file {
            changes = crate::file_view::changes(&file.marks);
        }
        let mut split_of = vec![0; doc.lines.len()];
        for (index, item) in split.iter().enumerate() {
            match item {
                SplitRow::Shared(row) => split_of[*row] = index,
                SplitRow::Sides { old, new } => {
                    for row in [*old, *new].into_iter().flatten() {
                        split_of[row] = index;
                    }
                }
            }
        }
        Self {
            split,
            split_of,
            changes,
            widest_unified,
            widest_code,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Columns {
    pub(crate) unified: usize,
    pub(crate) split: usize,
    pub(crate) tab: usize,
}

const WRAP_MARGIN: f32 = 14.;
const NARROWEST: usize = 8;

impl Columns {
    pub(crate) fn fitting(width: Pixels, cell: Pixels, face: &Face, beside: f32) -> Self {
        let cell = f32::from(cell).max(1.);
        let width = f32::from(width);
        let fit = |room: f32| ((room / cell).floor().max(0.) as usize).max(NARROWEST);
        Self {
            unified: fit(width - face.unified_gutter() - beside - WRAP_MARGIN),
            split: fit((width - 1.) / 2. - face.split_gutter() - WRAP_MARGIN),
            tab: face.tab.max(1),
        }
    }
}

pub(crate) fn shaped(window: &Window, face: &Face, text: &str) -> gpui::ShapedLine {
    let mut run = window.text_style().to_run(text.len());
    run.font.family = face.family.clone();
    window.text_system().shape_line(text.to_owned().into(), face.text(), &[run], None)
}

pub(crate) fn measure(window: &Window, width: Pixels, face: &Face, beside: f32) -> Columns {
    Columns::fitting(width, shaped(window, face, "0").width, face, beside)
}

#[derive(Debug, Default)]
pub(crate) struct Wrapped {
    breaks: Vec<Vec<usize>>,
    rows: Vec<(usize, usize)>,
    first: Vec<usize>,
}

fn wide(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x115f
            | 0x2e80..=0x303e
            | 0x3041..=0x33ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xa000..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe30..=0xfe4f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1f64f
            | 0x1f900..=0x1f9ff
            | 0x20000..=0x3fffd
    )
}

fn cells(grapheme: &str, tab: usize) -> usize {
    if grapheme == "\t" {
        return tab;
    }
    match grapheme.chars().next() {
        Some(c) if c.is_ascii() => 1,
        Some(c) if wide(c) => 2,
        _ => 1,
    }
}

pub(crate) fn width_in_cells(text: &str, tab: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    if text.is_ascii() && !text.contains('\t') {
        return text.len();
    }
    text.graphemes(true).map(|grapheme| cells(grapheme, tab)).sum()
}

pub(crate) fn breaks(code: &str, width: usize, tab: usize) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    let width = width.max(1);
    if code.len() <= width && !code.contains('\t') {
        return Vec::new();
    }
    let mut cuts = Vec::new();
    let (mut used, mut start, mut inked) = (0, 0, false);
    let mut soft: Option<(usize, usize)> = None;
    for (at, grapheme) in code.grapheme_indices(true) {
        let size = cells(grapheme, tab);
        if used + size > width && used > 0 {
            match soft.filter(|&(offset, before)| offset > start && before * 2 >= width) {
                Some((offset, before)) => {
                    cuts.push(offset);
                    used -= before;
                    start = offset;
                    inked = used > 0;
                }
                None => {
                    cuts.push(at);
                    used = 0;
                    start = at;
                    inked = false;
                }
            }
            soft = None;
            if used + size > width && used > 0 {
                cuts.push(at);
                used = 0;
                start = at;
                inked = false;
            }
        }
        used += size;
        if !grapheme.chars().all(char::is_whitespace) {
            inked = true;
        } else if inked {
            soft = Some((at + grapheme.len(), used));
        }
    }
    cuts
}

pub(crate) fn wrap(doc: &DiffDoc, layout: &Layout, mode: Mode, columns: Columns) -> Wrapped {
    let width = match mode {
        Mode::Unified => columns.unified,
        Mode::Split => columns.split,
    };
    let breaks: Vec<Vec<usize>> = doc
        .lines
        .iter()
        .map(|line| if line.kind.is_code() { self::breaks(doc.code(line), width, columns.tab) } else { Vec::new() })
        .collect();
    let parts = |row: Option<usize>| row.and_then(|row| breaks.get(row)).map_or(1, |cuts| cuts.len() + 1);
    let items: Vec<usize> = match mode {
        Mode::Unified => (0..doc.lines.len()).map(|row| parts(Some(row))).collect(),
        Mode::Split => layout
            .split
            .iter()
            .map(|item| match item {
                SplitRow::Shared(_) => 1,
                SplitRow::Sides { old, new } => parts(*old).max(parts(*new)),
            })
            .collect(),
    };
    let mut rows = Vec::with_capacity(items.len());
    let mut first = Vec::with_capacity(items.len());
    for (item, count) in items.into_iter().enumerate() {
        first.push(rows.len());
        rows.extend((0..count).map(|part| (item, part)));
    }
    Wrapped { breaks, rows, first }
}

fn base_rows(ui: &Ui) -> usize {
    match ui.shown_mode() {
        Mode::Unified => ui.diff.lines.len(),
        Mode::Split => ui.diff_layout.split.len(),
    }
}

pub(crate) fn item(ui: &Ui, visual: usize) -> Option<(usize, usize)> {
    match &ui.diff_options.wrapped {
        Some(wrapped) => wrapped.rows.get(visual).copied(),
        None => (visual < base_rows(ui)).then_some((visual, 0)),
    }
}

pub(crate) fn piece(ui: &Ui, row: usize, part: usize) -> Range<usize> {
    let length = ui.diff.lines.get(row).map_or(0, |line| ui.diff.code(line).len());
    let Some(cuts) = ui.diff_options.wrapped.as_ref().and_then(|wrapped| wrapped.breaks.get(row)) else {
        return if part == 0 { 0..length } else { length..length };
    };
    let start = match part {
        0 => 0,
        _ => cuts.get(part - 1).copied().unwrap_or(length),
    };
    let end = cuts.get(part).copied().unwrap_or(length);
    start.min(end)..end
}

pub(crate) fn part_at(ui: &Ui, row: usize, offset: usize) -> usize {
    ui.diff_options
        .wrapped
        .as_ref()
        .and_then(|wrapped| wrapped.breaks.get(row))
        .map_or(0, |cuts| cuts.partition_point(|&cut| cut <= offset))
}

pub(crate) fn visual_of(ui: &Ui, row: usize) -> Option<usize> {
    let item = match ui.shown_mode() {
        Mode::Unified => (row < ui.diff.lines.len()).then_some(row)?,
        Mode::Split => *ui.diff_layout.split_of.get(row)?,
    };
    match &ui.diff_options.wrapped {
        Some(wrapped) => wrapped.first.get(item).copied(),
        None => Some(item),
    }
}

pub(crate) fn part(ui: &Ui, visual: usize) -> usize {
    item(ui, visual).map_or(0, |(_, part)| part)
}

pub(crate) fn changes(ui: &Ui) -> &[Change] {
    &ui.diff_layout.changes
}

pub(crate) fn rows(ui: &Ui) -> usize {
    match &ui.diff_options.wrapped {
        Some(wrapped) => wrapped.rows.len(),
        None => base_rows(ui),
    }
}

pub(crate) fn doc_row(ui: &Ui, visual: usize, column: Column) -> Option<usize> {
    let (item, _) = self::item(ui, visual)?;
    match (ui.shown_mode(), column) {
        (Mode::Unified, _) => (item < ui.diff.lines.len()).then_some(item),
        (Mode::Split, column) => match ui.diff_layout.split.get(item)? {
            SplitRow::Shared(row) => Some(*row),
            SplitRow::Sides { old, new } if column == Column::Old => old.or(*new),
            SplitRow::Sides { old, new } => new.or(*old),
        },
    }
}

pub(crate) fn layered(layers: &[(&[Range<usize>], HighlightStyle)]) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut cuts: Vec<usize> = layers
        .iter()
        .flat_map(|(spans, _)| spans.iter().flat_map(|span| [span.start, span.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut merged: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let style = layers
            .iter()
            .rev()
            .filter(|(spans, _)| spans.iter().any(|span| span.start <= from && to <= span.end))
            .fold(None, |style: Option<HighlightStyle>, (_, layer)| Some(style.unwrap_or_default().highlight(*layer)));
        let Some(style) = style else { continue };
        match merged.last_mut() {
            Some((range, last)) if range.end == from && *last == style => range.end = to,
            _ => merged.push((from..to, style)),
        }
    }
    merged
}

fn mini<T: Desk>(
    id: impl Into<ElementId>,
    label: &'static str,
    danger: bool,
    enabled: bool,
    action: Action,
    cx: &Context<T>,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(18.))
        .px(px(7.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .rounded(px(4.))
        .font_family(".SystemUIFont")
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(rgb(if danger { palette().deleted } else { palette().text }))
        .when(!danger, |d| {
            d.bg(rgb(palette().raised)).border_1().border_color(rgb(palette().line_strong))
        })
        .child(label);
    if !enabled {
        return base.opacity(0.42);
    }
    base.cursor_pointer()
        .hover(move |s| {
            s.bg(if danger {
                soft(palette().deleted, 0x26)
            } else {
                rgb(palette().hover)
            })
        })
        .on_click(cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx)))
}

pub(super) fn render<T: Desk>(app: &T, window: &Window, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let mode = ui.shown_mode();
    let file_view = ui.diff.file.is_some();
    let layout = &ui.diff_layout;
    let doc = &ui.diff;
    let side = match &ui.selection {
        Some(Selection::File { staged, .. }) => Some(*staged),
        _ => None,
    };
    let committed_file = matches!(ui.selection, Some(Selection::Commit(_)))
        .then(|| ui.inspection.path.clone())
        .flatten();
    let stashed_file = matches!(ui.selection, Some(Selection::Stash(_)))
        .then(|| ui.stash.open_path().map(std::path::Path::to_path_buf))
        .flatten();
    let compared_file = matches!(ui.selection, Some(Selection::Compare { .. })).then(|| ui.compare.path.clone()).flatten();
    let open_file = committed_file.clone().or_else(|| stashed_file.clone()).or_else(|| compared_file.clone());
    let from = ui
        .selection
        .as_ref()
        .and_then(|selection| ui.renamed_from(selection, open_file.as_deref()));
    let whole_rename = from.is_some() && side == Some(true);
    let heading = if ui.diff_title.is_empty() {
        "Changes".to_owned()
    } else {
        ui.diff_title.clone()
    };
    let editable = ui.can_edit_index();
    let picture = crate::image_diff::showing(ui);
    let module = crate::submodule::showing(ui);
    let texts = picture.is_none() && module.is_none();
    let blaming = crate::blame::showing(ui);
    let browsing = crate::browse::showing_file(ui);
    let header = if blaming {
        blame_header(ui, cx)
    } else if browsing {
        browse_header(ui, cx)
    } else {
        div()
            .min_h(px(34.))
            .flex_shrink_0()
            .pl_3()
            .pr_2()
            .py_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_2()
            .gap_y_1()
            .bg(rgb(palette().panel))
            .border_b_1()
            .border_color(rgb(palette().line))
            .child(
                kit::fit(match &open_file {
                    Some(path) => path.to_string_lossy().into_owned(),
                    None => heading,
                })
                .size(12.5)
                .weight(FontWeight::SEMIBOLD)
                .cut(kit::Cut::Start),
            )
            .when_some(from.clone(), |d, from| {
                d.child(
                    kit::fit(format!("from {}", from.display()))
                        .size(11.5)
                        .color(palette().text_3)
                        .cut(kit::Cut::Start),
                )
            })
            .when_some(side, |d, staged| {
                d.child(kit::chip(
                    kit::Chip::Plain,
                    if staged { "Staged" } else { "Unstaged" },
                ))
            })
            .child(kit::grow())
            .when_some(ui.selection.clone(), |d, selection| match selection {
                Selection::File {
                    path,
                    staged: false,
                } => d
                    .when(module.is_none(), |d| {
                        d.child(crate::stash::file_button(app, cx)).child(
                            kit::btn("discard-file", "Discard…")
                                .small()
                                .danger()
                                .enabled(editable)
                                .on(Action::AskDiscard(Discard::File(path)), cx),
                        )
                    })
                    .child(
                        kit::btn("stage-file", "Stage file")
                            .small()
                            .enabled(editable)
                            .on(Action::StageFile, cx),
                    ),
                Selection::File { path, staged: true } => d
                    .when(whole_rename, |d| d.child(kit::muted(WHOLE_RENAME)))
                    .child(
                        kit::btn("unstage-file", "Unstage file")
                            .small()
                            .enabled(editable)
                            .on(Action::UnstagePath(path), cx),
                    ),
                _ => d,
            })
            .when_some(edit_button(ui, cx).filter(|_| module.is_none()), |d, button| d.child(button))
            .when_some(crate::stash::restore_button(ui, cx), |d, button| d.child(button))
            .when(module.is_none() && crate::file_history::offered_here(ui), |d| d.child(crate::file_history::button(ui, cx)))
            .when(module.is_none() && crate::blame::offered_here(ui), |d| d.child(crate::blame::button(ui, cx)))
            .when(module.is_none() && crate::file_view::offered(ui), |d| d.child(crate::file_view::toggle(ui, cx)))
            .when(crate::image_diff::offers_toggle(ui), |d| d.child(crate::image_diff::toggle(ui, cx)))
            .when(texts && !file_view && !doc.lines.is_empty(), |d| {
                d.child(mode_control(mode, cx))
            })
            .when(texts, |d| d.child(options_control(ui, file_view, cx)))
            .when(texts && !doc.lines.is_empty() && !(file_view && layout.changes.is_empty()), |d| d.child(change_buttons(cx)))
            .child(kit::icon_btn(
                "close-diff",
                "x",
                "Back to the graph (Esc)",
                Action::CloseDiff,
                cx,
            ))
    };
    let list_id = match &ui.selection {
        _ if blaming => format!("blame-{:?}-{:?}", ui.blame.blamed(), ui.blame.resolved()),
        _ if browsing => format!("browse-{:?}-{:?}", ui.browse.rev(), ui.browse.chosen()),
        Some(Selection::File { path, staged }) => {
            format!("patch-{staged}-{}", path.to_string_lossy())
        }
        Some(Selection::Commit(oid)) => format!("patch-{oid}-{committed_file:?}"),
        Some(Selection::Stash(oid)) => format!("stash-patch-{oid}-{stashed_file:?}"),
        Some(Selection::Compare { from, to, since_fork }) => format!("compare-patch-{from:?}-{to:?}-{since_fork}-{compared_file:?}"),
        _ => "patch".to_owned(),
    };
    let split = mode == Mode::Split;
    let wrapping = ui.diff_options.wrapped.is_some();
    let face = ui.diff_text.face.clone();
    let code_width = if split && !wrapping {
        layout.widest_code.and_then(|row| doc.lines.get(row)).map(|row| {
            let text = diff_text::expanded(doc.code(row), face.tab);
            shaped(window, &face, &text).width.ceil() + px(8.)
        })
    } else {
        None
    };
    let count = rows(ui);
    let whole_file = doc.options.whole_file;
    let undoable = crate::recover::undo_offered(ui);
    let lines = uniform_list(
        SharedString::from(format!("{list_id}-{mode:?}-{wrapping}-{file_view}")),
        count,
        cx.processor(move |app, rows: Range<usize>, _, cx| {
            let ui = app.ui();
            let face = &ui.diff_text.face;
            let blaming = crate::blame::showing(ui);
            let editable = ui.can_edit_index();
            let layout = &ui.diff_layout;
            let doc = &ui.diff;
            rows
                .filter_map(|index| {
                    let (item, part) = self::item(ui, index)?;
                    let row = if split {
                        match layout.split.get(item)? {
                            SplitRow::Shared(row) => doc.lines.get(*row)?,
                            SplitRow::Sides { old, new } => {
                                let at = new.or(*old).and_then(|row| doc.lines.get(row)).map_or(0, |row| row.at);
                                return Some(line_menu(
                                    base_row(face)
                                        .child(split_cell(ui, index, *old, true, code_width, part, cx))
                                        .child(split_divider())
                                        .child(split_cell(ui, index, *new, false, code_width, part, cx)),
                                    at,
                                    [*old, *new],
                                    cx,
                                ));
                            }
                        }
                    } else {
                        doc.lines.get(item)?
                    };
                    let text = doc.text(row);
                    let first = part == 0;
                    Some(match row.kind {
                        Kind::Hunk => hunk_row(
                            face,
                            text,
                            row.hunk.filter(|_| doc.stageable && !whole_file),
                            side,
                            row.hunk.filter(|_| undoable),
                            editable,
                            split,
                            cx,
                        ),
                        Kind::File => {
                            let path = text.rsplit_once(" b/").map_or(text, |(_, path)| path);
                            base_row(face)
                                .pl_3()
                                .bg(rgb(palette().panel))
                                .border_b_1()
                                .border_color(rgb(palette().line))
                                .child(kit::fit(path.to_owned()).size(12.5).weight(FontWeight::SEMIBOLD))
                        }
                        Kind::Note | Kind::NoNewline => base_row(face)
                            .pl(if split { px(12.) } else { px(face.unified_gutter() + crate::blame::width(ui)) })
                            .child(note(text)),
                        _ => line_menu(
                            base_row(face)
                                .when_some(line_background(row.kind), |d, bg| d.bg(rgb(bg)))
                                .children(blaming.then(|| crate::blame::cell(ui, item, first, face, cx)).flatten())
                                .child(crate::lines::gutter(
                                    ui,
                                    item,
                                    match doc.file.as_deref() {
                                        Some(shown) => gutter_cells()
                                            .when(face.numbers, |d| {
                                                d.child(number(face, row.new.filter(|_| first), face.number(false) * 2.))
                                            })
                                            .child(crate::file_view::gutter_mark(shown.mark(item), first, face.marker())),
                                        None => gutter_cells()
                                            .when(face.numbers, |d| {
                                                d.child(number(face, row.old.filter(|_| first), face.number(false)))
                                                    .child(number(face, row.new.filter(|_| first), face.number(false)))
                                            })
                                            .child(marker(face, if first { row.kind } else { Kind::Context })),
                                    },
                                    cx,
                                ))
                                .child(diff_select::cell(
                                    ui,
                                    item,
                                    part,
                                    Column::Unified,
                                    code(ui, item, row, Column::Unified, part),
                                    cx,
                                )),
                            row.at,
                            [Some(item), None],
                            cx,
                        ),
                    })
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.diff_scroll.clone())
    .flex_1()
    .min_h_0();
    let mut lines = if split || wrapping {
        lines.with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::FitList)
    } else {
        lines
            .with_width_from_item(layout.widest_unified)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    };
    if split {
        lines.style().restrict_scroll_to_axis = Some(true);
    }
    let marks = showing_marks(ui);
    let measured = ui.diff_options.measured;
    let face_measured = ui.diff_text.face.clone();
    let beside = crate::blame::width(ui);
    let desk = cx.entity().downgrade();
    let bars = texts.then(|| bars(ui)).unwrap_or_default();
    let placeholder = (texts && doc.lines.is_empty() && doc.state != PatchState::Truncated)
        .then(|| placeholder(ui, from.is_some()));
    let showing = texts && placeholder.is_none();
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .children(crate::find::bar(app, crate::find::Surface::Diff, cx))
        .children(bars)
        .when(texts, |d| d.children(crate::lines::bar(ui, cx)))
        .children(picture.map(crate::image_diff::view))
        .children(module.map(|panel| crate::submodule::panel(ui, panel, cx)))
        .children(placeholder)
        .when(showing, |d| {
            let sideways = if split {
                kit::scrollbar("diff-pan", &ui.split_pan).within(&ui.diff_scroll)
            } else {
                kit::scrollbar("diff-pan", &ui.diff_scroll)
            };
            let list = div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(lines)
                .on_children_prepainted(move |bounds, window, cx| {
                    let Some(width) = bounds.first().map(|bounds| bounds.size.width) else { return };
                    if width == measured || width <= px(0.) {
                        return;
                    }
                    let columns = measure(window, width, &face_measured, beside);
                    let _ = desk.update(cx, |app, cx| {
                        if crate::diff_options::resized(app.ui_mut(), width, columns) {
                            cx.notify();
                        }
                    });
                });
            d.when(split, |d| d.child(split_headings()))
                .child(crate::lines::released(list, cx))
                .child(kit::scrollbar("diff-scrollbar", &ui.diff_scroll).marks(marks))
                .when(!wrapping, |d| d.child(sideways.horizontal()))
        })
}

fn blame_header<T: Desk>(ui: &Ui, cx: &Context<T>) -> Div {
    let (path, at) = crate::blame::title(ui).unwrap_or_default();
    div()
        .min_h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .py_1()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .gap_y_1()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::fit(path).size(12.5).weight(FontWeight::SEMIBOLD).cut(kit::Cut::Start))
        .when(!at.is_empty(), |d| d.child(kit::chip(kit::Chip::Plain, at)))
        .when(ui.blame.reading(), |d| d.child(kit::spinner("blame-header-reading")))
        .child(kit::grow())
        .child(crate::blame::details(ui, cx))
        .child(crate::blame::button(ui, cx))
        .when(!ui.diff.lines.is_empty(), |d| d.child(options_control(ui, true, cx)))
        .child(kit::icon_btn("close-blame", "x", "Back to where you were (Esc)", Action::Blame(crate::blame::Action::Close), cx))
}

fn browse_header<T: Desk>(ui: &Ui, cx: &Context<T>) -> Div {
    let (path, at) = crate::browse::title(ui).unwrap_or_default();
    div()
        .min_h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .py_1()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .gap_y_1()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(kit::fit(path).size(12.5).weight(FontWeight::SEMIBOLD).cut(kit::Cut::Start))
        .child(kit::chip(kit::Chip::Plain, at))
        .when(crate::browse::ignored(ui), |d| d.child(kit::chip(kit::Chip::Plain, "Ignored")))
        .child(kit::grow())
        .child(crate::browse::history_button(ui, cx))
        .child(crate::browse::blame_button(ui, cx))
        .when(!ui.diff.lines.is_empty(), |d| d.child(options_control(ui, true, cx)))
        .child(kit::icon_btn(
            "close-browsed-file",
            "x",
            "Back to the files (Esc)",
            Action::Browse(crate::browse::Action::Unchoose),
            cx,
        ))
}

pub(crate) fn placeholder_text(ui: &Ui, renamed: bool) -> Option<String> {
    if let Some(text) = crate::blame::placeholder(ui).or_else(|| crate::browse::placeholder(ui)) {
        return Some(text);
    }
    let whole = match &ui.selection {
        Some(Selection::Commit(_)) if ui.inspection.path.is_none() => Some("commit"),
        Some(Selection::Stash(_)) if ui.stash.open_path().is_none() => Some("stash"),
        Some(Selection::Compare { .. }) if ui.compare.path.is_none() => Some("comparison"),
        _ => None,
    };
    if let Some(text) = crate::file_view::placeholder(ui) {
        return Some(text);
    }
    Some(match &ui.diff.state {
        PatchState::Loading => "Loading diff…".to_owned(),
        PatchState::Text | PatchState::Truncated => return None,
        PatchState::Binary { old, new } => diff_doc::binary_text(*old, *new),
        PatchState::Lfs { old, new } => crate::lfs::summary(old.as_ref(), new.as_ref()),
        PatchState::ModeOnly { old, new } => diff_doc::mode_only_text(*old, *new),
        PatchState::Unreadable(reason) => format!("This diff could not be shown. {reason}"),
        PatchState::Empty(Blank::EmptyFile) => diff_doc::EMPTY_FILE.to_owned(),
        PatchState::Empty(Blank::EmptyDeleted) => diff_doc::EMPTY_DELETED.to_owned(),
        PatchState::Empty(Blank::Renamed) => RENAMED_ONLY.to_owned(),
        PatchState::Empty(Blank::Nothing) if ui.diff.options.ignore_whitespace => diff_doc::WHITESPACE_ONLY.to_owned(),
        PatchState::Empty(Blank::Nothing) => match whole {
            Some(what) => format!("This {what} changes no files."),
            None if renamed => RENAMED_ONLY.to_owned(),
            None => "No text changes to show for this selection.".to_owned(),
        },
    })
}

const RENAMED_ONLY: &str = "Renamed with no changes to its content.";

fn placeholder(ui: &Ui, renamed: bool) -> Div {
    let text = placeholder_text(ui, renamed).unwrap_or_default();
    div()
        .flex_1()
        .min_h_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .when(ui.diff.state == PatchState::Loading, |d| d.child(kit::spinner("diff-loading")))
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(palette().text_2))
                .child(text),
        )
}

pub(crate) fn bar_texts(ui: &Ui) -> Vec<&'static str> {
    if let Some(shown) = ui.diff.file.as_deref() {
        return crate::file_view::bar_texts(ui, shown);
    }
    let stageable_view = matches!(ui.selection, Some(Selection::File { .. }));
    let doc = &ui.diff;
    let mut texts = Vec::new();
    if doc.state == PatchState::Truncated {
        texts.push(diff_doc::truncated_text(stageable_view));
    }
    if doc.shortened {
        texts.push(diff_doc::shortened_text(stageable_view));
    }
    if stageable_view && doc.options.ignore_whitespace && doc.state == PatchState::Text {
        texts.push(WHITESPACE_HIDDEN);
    }
    texts
}

pub(crate) const WHITESPACE_HIDDEN: &str =
    "Whitespace changes are hidden, so this diff cannot be staged from here. Show them to stage hunks or lines.";

fn bars(ui: &Ui) -> Vec<Div> {
    bar_texts(ui)
        .into_iter()
        .map(|text| {
            div()
                .flex_shrink_0()
                .px_3()
                .py(px(6.))
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(palette().raised))
                .border_b_1()
                .border_color(rgb(palette().line))
                .child(kit::icon("alert").text_color(rgb(palette().modified)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(palette().text_2))
                        .child(text),
                )
        })
        .collect()
}

fn edit_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> Option<impl IntoElement + use<T>> {
    let Some(Selection::File { path, staged }) = &ui.selection else { return None };
    let target = crate::commands::Target::File {
        path: path.clone(),
        side: if *staged { crate::commands::Side::Staged } else { crate::commands::Side::Unstaged },
    };
    let entry = crate::commands::entry(crate::commands::find("file.edit")?, ui, &target, cx)?;
    let action = entry.action?;
    Some(kit::btn("edit-file", entry.title).small().ghost().icon("external").on(action, cx))
}

fn options_control<T: Desk>(ui: &Ui, file_view: bool, cx: &Context<T>) -> Div {
    use crate::diff_options::{Action as Show, Choice};
    let options = &ui.diff_options;
    let context = |id, label, whole: bool| {
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(options.whole_file == whole)
            .on(Action::DiffOptions(Show::Set(Choice::WholeFile, whole)), cx)
    };
    let toggle = |id, label, tip, choice: Choice| {
        let on = options.on(choice);
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(on)
            .tip(tip)
            .on(Action::DiffOptions(Show::Set(choice, !on)), cx)
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap_1()
        .when(!file_view, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .p(px(1.))
                    .rounded(px(RADIUS))
                    .bg(rgb(palette().canvas))
                    .border_1()
                    .border_color(rgb(palette().line))
                    .child(context("diff-hunks", "3 lines", false))
                    .child(context("diff-whole-file", "Whole file", true)),
            )
            .child(toggle(
                "diff-whitespace",
                "Ignore whitespace",
                "Hide changes that only add, remove or move whitespace",
                Choice::IgnoreWhitespace,
            ))
        })
        .child(toggle("diff-wrap", "Wrap", "Wrap long lines to the width of the diff", Choice::Wrap))
}

fn change_buttons<T: Desk>(cx: &Context<T>) -> Div {
    use crate::diff_options::Action as Show;
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .child(kit::icon_btn(
            "previous-change",
            "chevron-up",
            "Previous change (⌥↑)",
            Action::DiffOptions(Show::Previous),
            cx,
        ))
        .child(kit::icon_btn("next-change", "chevron-down", "Next change (⌥↓)", Action::DiffOptions(Show::Next), cx))
}

fn showing_marks(ui: &Ui) -> Vec<crate::scrollbar::Mark> {
    if ui.diff.lines.is_empty() {
        Vec::new()
    } else {
        crate::diff_options::marks(ui)
    }
}

fn mode_control<T: Desk>(mode: Mode, cx: &Context<T>) -> Div {
    let segment = |id, label, choice| {
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(mode == choice)
            .on(Action::DiffMode(choice), cx)
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .p(px(1.))
        .rounded(px(RADIUS))
        .bg(rgb(palette().canvas))
        .border_1()
        .border_color(rgb(palette().line))
        .child(segment("diff-unified", "Unified", Mode::Unified))
        .child(segment("diff-split", "Split", Mode::Split))
}

fn line_menu<T: Desk>(row: Div, at: u32, rows: [Option<usize>; 2], cx: &Context<T>) -> Div {
    row.on_mouse_down(
        MouseButton::Right,
        cx.listener(move |app, event: &MouseDownEvent, window, cx| {
            let ui = app.ui();
            let picked = rows.into_iter().flatten().any(|row| crate::lines::picked(ui, row));
            let target = picked.then(|| crate::lines::target(ui)).flatten();
            let Some(target) = target.or_else(|| rows.into_iter().flatten().find_map(|row| diff_select::code_target(ui, row, at)))
            else {
                return;
            };
            diff_select::keep_focus(app.ui_mut());
            let menu = super::Menu::Object { target, at: event.position };
            app.dispatch(Action::Menu(Some(menu)), window, cx);
        }),
    )
}

fn base_row(face: &Face) -> Div {
    div().h(px(face.line())).w_full().flex().items_center()
}

fn split_divider() -> Div {
    div().w(px(1.)).h_full().flex_shrink_0().bg(rgb(palette().line))
}

fn split_headings() -> Div {
    div()
        .h(px(DIFF_LINE))
        .w_full()
        .flex()
        .items_center()
        .flex_shrink_0()
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .text_size(px(TEXT_TINY))
        .text_color(rgb(palette().text_2))
        .child(div().pl_3().flex_1().child("Before"))
        .child(split_divider())
        .child(div().pl_3().flex_1().child("After"))
}

fn gutter_cells() -> Div {
    div().h_full().flex().flex_shrink_0().items_center()
}

fn split_cell<T: Desk>(
    ui: &Ui,
    index: usize,
    side: Option<usize>,
    old: bool,
    width: Option<Pixels>,
    part: usize,
    cx: &Context<T>,
) -> Div {
    let cell = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .overflow_hidden();
    let doc = &ui.diff;
    let Some((at, row)) = side.and_then(|i| Some((i, doc.lines.get(i)?))) else {
        return cell.bg(rgb(palette().panel));
    };
    if row.kind == Kind::NoNewline {
        return if part == 0 { cell.pl_3().child(note(doc.text(row))) } else { cell };
    }
    let column = Column::split(old);
    let first = part == 0;
    let face = &ui.diff_text.face;
    let mut text = div()
        .id((if old { "split-old" } else { "split-new" }, index))
        .flex_1()
        .min_w_0()
        .h_full()
        .overflow_x_scroll()
        .track_scroll(&ui.split_pan)
        .child(diff_select::cell(ui, at, part, column, code(ui, at, row, column, part).when_some(width, |d, width| d.min_w(width)), cx));
    text.style().restrict_scroll_to_axis = Some(true);
    let shown = if old { row.old } else { row.new };
    cell.when_some(line_background(row.kind), |d, bg| d.bg(rgb(bg)))
        .child(crate::lines::gutter(
            ui,
            at,
            gutter_cells()
                .when(face.numbers, |d| d.child(number(face, shown.filter(|_| first), face.number(true))))
                .child(marker(face, if first { row.kind } else { Kind::Context })),
            cx,
        ))
        .child(text)
}

fn line_background(kind: Kind) -> Option<u32> {
    match kind {
        Kind::Added => Some(palette().added_bg),
        Kind::Deleted => Some(palette().deleted_bg),
        _ => None,
    }
}

fn number(face: &Face, number: Option<u32>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .pr_2()
        .flex()
        .justify_end()
        .font_family(face.family.clone())
        .text_size(face.small())
        .text_color(rgb(palette().text_off))
        .when_some(number, |d, number| d.child(number.to_string()))
}

fn marker(face: &Face, kind: Kind) -> Div {
    let (sign, ink) = match kind {
        Kind::Added => ("+", palette().added),
        Kind::Deleted => ("−", palette().deleted),
        _ => ("", palette().text_3),
    };
    div()
        .w(px(face.marker()))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .font_family(face.family.clone())
        .text_size(face.text())
        .text_color(rgb(ink))
        .child(sign)
}

pub(crate) fn highlights(ui: &Ui, index: usize, row: &Line, column: Column) -> Vec<(Range<usize>, HighlightStyle)> {
    let color = if row.kind == Kind::Added { palette().added_hi } else { palette().deleted_hi };
    let chosen: Vec<Range<usize>> = diff_select::span(ui, index, column).into_iter().collect();
    let (found, focus) = crate::find::marks(ui, crate::find::Pane::Diff, index);
    Marks { chosen: &chosen, found: &found, focus: &focus, words: &row.words, changed: color, syntax: &row.syntax }.runs()
}

pub(crate) struct Marks<'a> {
    pub(crate) chosen: &'a [Range<usize>],
    pub(crate) found: &'a [Range<usize>],
    pub(crate) focus: &'a [Range<usize>],
    pub(crate) words: &'a [Range<usize>],
    pub(crate) changed: u32,
    pub(crate) syntax: &'a [(Range<usize>, Token)],
}

impl Marks<'_> {
    pub(crate) fn runs(&self) -> Vec<(Range<usize>, HighlightStyle)> {
        let changed = HighlightStyle {
            background_color: Some(rgb(self.changed).into()),
            ..Default::default()
        };
        let selected = HighlightStyle {
            background_color: Some(rgb(palette().text_selection).into()),
            ..Default::default()
        };
        let (chosen, found, focus, words, syntax) = (self.chosen, self.found, self.focus, self.words, self.syntax);
        let ink = |token: Token| HighlightStyle {
            color: Some(rgb(token.color()).into()),
            ..Default::default()
        };
        let plain = chosen.is_empty() && found.is_empty() && focus.is_empty();
        if syntax.is_empty() && plain {
            return words.iter().cloned().map(|span| (span, changed)).collect();
        }
        if words.is_empty() && plain {
            return syntax.iter().map(|(span, token)| (span.clone(), ink(*token))).collect();
        }
        let (matched, current) = crate::find::styles();
        let inked: Vec<(Vec<Range<usize>>, HighlightStyle)> = Token::ALL
            .iter()
            .map(|token| {
                let spans: Vec<Range<usize>> = syntax.iter().filter(|(_, held)| held == token).map(|(span, _)| span.clone()).collect();
                (spans, ink(*token))
            })
            .filter(|layer| !layer.0.is_empty())
            .collect();
        let readable = HighlightStyle { color: Some(rgb(palette().text).into()), ..Default::default() };
        let chosen_focus: Vec<Range<usize>> = chosen
            .iter()
            .flat_map(|chosen| focus.iter().map(move |hit| chosen.start.max(hit.start)..chosen.end.min(hit.end)))
            .filter(|span| span.start < span.end)
            .collect();
        let mut layers: Vec<(&[Range<usize>], HighlightStyle)> = vec![
            (&chosen_focus, readable),
            (chosen, selected),
            (focus, current),
            (found, matched),
            (words, changed),
        ];
        layers.extend(inked.iter().map(|(spans, style)| (spans.as_slice(), *style)));
        layered(&layers)
    }
}

pub(crate) fn clipped(runs: Vec<(Range<usize>, HighlightStyle)>, piece: &Range<usize>) -> Vec<(Range<usize>, HighlightStyle)> {
    runs.into_iter()
        .filter_map(|(range, style)| {
            let start = range.start.max(piece.start);
            let end = range.end.min(piece.end);
            (start < end).then(|| (start - piece.start..end - piece.start, style))
        })
        .collect()
}

fn code(ui: &Ui, index: usize, row: &Line, column: Column, part: usize) -> Div {
    let face = &ui.diff_text.face;
    let code = ui.diff.code(row);
    let (text, highlights) = if ui.diff_options.wrapped.is_some() {
        let piece = piece(ui, index, part);
        let text = code.get(piece.clone()).unwrap_or_default();
        (text, clipped(highlights(ui, index, row, column), &piece))
    } else {
        (code, highlights(ui, index, row, column))
    };
    drawn(face, text, highlights)
}

pub(crate) fn drawn(face: &Face, text: &str, highlights: Vec<(Range<usize>, HighlightStyle)>) -> Div {
    let highlights = diff_text::shown_runs(text, highlights, face.tab);
    let text = diff_text::expanded(text, face.tab).into_owned();
    div()
        .flex_shrink_0()
        .flex_grow()
        .h_full()
        .flex()
        .items_center()
        .font_family(face.family.clone())
        .whitespace_nowrap()
        .text_size(face.text())
        .text_color(rgb(palette().text))
        .child(StyledText::new(text).with_highlights(highlights))
}

fn note(text: &str) -> impl IntoElement {
    kit::fit(text.trim_start_matches("\\ ").to_owned())
        .size(TEXT_SMALL)
        .color(palette().text_3)
}

fn hunk_row<T: Desk>(
    face: &Face,
    text: &str,
    hunk: Option<usize>,
    side: Option<bool>,
    undo: Option<usize>,
    editable: bool,
    split: bool,
    cx: &Context<T>,
) -> Div {
    let (range, context) = text
        .get(2..)
        .and_then(|rest| rest.split_once("@@"))
        .map_or((text, ""), |(range, context)| (range.trim(), context.trim()));
    let hunk_size = face.text() - px(0.5);
    let row = base_row(face)
        .pl_3()
        .pr_2()
        .gap_2()
        .bg(rgb(palette().hunk))
        .border_b_1()
        .border_color(rgb(palette().line))
        .font_family(face.family.clone())
        .text_size(hunk_size)
        .whitespace_nowrap()
        .child(
            div()
                .flex_shrink_0()
                .text_color(rgb(palette().text_3))
                .child(format!("@@ {range} @@")),
        )
        .when(split, |d| {
            d.child(kit::fit(context.to_owned()).family(face.family.clone()).size(f32::from(hunk_size)).color(palette().text_2).grow())
        })
        .when(!split, |d| {
            d.child(div().text_color(rgb(palette().text_2)).child(context.to_owned()))
        });
    let row = row.when_some(undo, |d, hunk| {
        d.child(
            mini(
                ("undo-hunk", hunk),
                "Undo in my files",
                false,
                editable,
                Action::Recover(crate::recover::Action::UndoHunk(hunk)),
                cx,
            )
            .tooltip(kit::tip("Apply the inverse of this hunk to your files")),
        )
    });
    row.when_some(hunk.zip(side), |d, (hunk, staged)| {
        if staged {
            d.child(mini(
                ("unstage-hunk", hunk),
                "Unstage hunk",
                false,
                editable,
                Action::UnstageHunk(hunk),
                cx,
            ))
        } else {
            d.child(mini(
                ("discard-hunk", hunk),
                "Discard hunk",
                true,
                editable,
                Action::AskDiscard(Discard::Hunk(hunk)),
                cx,
            ))
            .child(mini(
                ("stage-hunk", hunk),
                "Stage hunk",
                false,
                editable,
                Action::StageHunk(hunk),
                cx,
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;

    #[test]
    fn layered_highlights_split_into_runs_that_do_not_overlap_and_the_first_layer_wins() {
        let paint = |color: u32| HighlightStyle { background_color: Some(rgb(color).into()), ..Default::default() };
        let (top, under) = (paint(palette().text_selection), paint(palette().added_hi));
        let runs = layered(&[(&[2..6], top), (&[0..3, 5..9], under)]);
        assert_eq!(runs, [(0..2, under), (2..6, top), (6..9, under)]);
        assert_eq!(layered(&[(&[], top), (&[1..4], under)]), [(1..4, under)]);
        assert_eq!(layered(&[(&[0..2, 2..5], top)]), [(0..5, top)]);
        let ink = HighlightStyle { color: Some(rgb(palette().text_2).into()), ..Default::default() };
        let runs = layered(&[(&[0..4], top), (&[2..8], ink)]);
        assert_eq!(runs, [(0..2, top), (2..4, ink.highlight(top)), (4..8, ink)]);
        assert!(layered(&[]).is_empty());
    }

    fn pieces<'a>(code: &'a str, width: usize) -> Vec<&'a str> {
        let cuts = breaks(code, width, 4);
        let mut starts = vec![0];
        starts.extend(cuts.iter().copied());
        let mut ends = cuts.clone();
        ends.push(code.len());
        starts.into_iter().zip(ends).map(|(start, end)| &code[start..end]).collect()
    }

    #[test]
    fn long_lines_break_at_the_last_space_that_fits_or_mid_word_when_none_does() {
        assert!(breaks("short line", 20, 4).is_empty());
        assert_eq!(pieces("alpha beta gamma delta", 12), ["alpha beta ", "gamma delta"]);
        assert_eq!(pieces("abcdefghijklmnop", 6), ["abcdef", "ghijkl", "mnop"]);
        assert_eq!(pieces("a bcdefghijklmnop", 6), ["a bcde", "fghijk", "lmnop"]);
        assert_eq!(pieces("\tx\ty", 5), ["\tx", "\ty"]);
        assert_eq!(pieces("名前名前名前", 4), ["名前", "名前", "名前"]);
        assert_eq!(pieces("café crème brûlée", 8), ["café ", "crème ", "brûlée"]);
        for (code, width) in [("one two three four five six seven", 7), ("x".repeat(50).as_str(), 9), ("é".repeat(30).as_str(), 4)] {
            let joined: String = pieces(code, width).concat();
            assert_eq!(joined, code);
            assert!(pieces(code, width).iter().all(|piece| piece.chars().count() <= width));
        }
    }

    #[test]
    fn wrapped_rows_map_back_to_their_line_and_the_part_of_its_text() {
        let text = concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -1,3 +1,3 @@\n short\n-the old line is rather long\n+the new line is longer than the old one\n end\n",
        );
        let doc = doc(text);
        let layout = Layout::new(&doc, 4);
        let columns = Columns { unified: 12, split: 12, tab: 4 };
        let unified = wrap(&doc, &layout, Mode::Unified, columns);
        assert_eq!(unified.rows, [(0, 0), (1, 0), (2, 0), (2, 1), (2, 2), (3, 0), (3, 1), (3, 2), (3, 3), (4, 0)]);
        assert_eq!(unified.first, [0, 1, 2, 5, 9]);
        let code = doc.code(&doc.lines[3]);
        let cuts = &unified.breaks[3];
        assert_eq!(&code[..cuts[0]], "the new ");
        assert_eq!(cuts.len(), 3);
        let split = wrap(&doc, &layout, Mode::Split, columns);
        assert_eq!(layout.split.len(), 4);
        assert_eq!(split.rows, [(0, 0), (1, 0), (2, 0), (2, 1), (2, 2), (2, 3), (3, 0)]);
        assert_eq!(layout.split_of, [0, 1, 2, 2, 3]);
        assert_eq!(layout.changes, [Change { start: 2, deleted: Some(2), added: Some(3) }]);
    }

    #[test]
    fn columns_fit_the_code_area_beside_the_gutter() {
        let face = Face { tab: 3, ..Face::default() };
        let columns = Columns::fitting(px(1096.), px(10.), &face, 0.);
        assert_eq!(columns, Columns { unified: 98, split: 48, tab: 3 });
        assert_eq!(Columns::fitting(px(50.), px(10.), &face, 0.).unified, NARROWEST);
        let bare = Face { numbers: false, ..face };
        assert_eq!(Columns::fitting(px(1096.), px(10.), &bare, 0.), Columns { unified: 106, split: 51, tab: 3 });
    }

    #[test]
    fn split_aligns_context_and_pairs_changed_rows_within_each_hunk() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -4,3 +4,4 @@ section\n here\n-Old hut\n+New hut\n+New trail\n end\n",
            "@@ -20 +21,0 @@\n-deleted only\n",
        ));
        let layout = Layout::new(&doc, 4);
        assert_eq!(layout.split, [
            SplitRow::Shared(0),
            SplitRow::Sides { old: Some(1), new: Some(1) },
            SplitRow::Sides { old: Some(2), new: Some(3) },
            SplitRow::Sides { old: None, new: Some(4) },
            SplitRow::Sides { old: Some(5), new: Some(5) },
            SplitRow::Shared(6),
            SplitRow::Sides { old: Some(7), new: None },
        ]);
        assert_eq!(doc.lines[0].hunk, Some(0));
        assert_eq!(doc.lines[6].hunk, Some(1));
        assert_eq!((doc.lines[5].old, doc.lines[5].new), (Some(6), Some(7)));
        assert_eq!(layout.widest_code, Some(7));
    }

    #[test]
    fn the_widest_row_counts_each_tab_at_the_chosen_width() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -1,2 +1,2 @@\n abcdefgh\n-\t\tx\n+\ty\n",
        ));
        assert_eq!(Layout::new(&doc, 1).widest_code, Some(1));
        assert_eq!(Layout::new(&doc, 4).widest_code, Some(2));
        assert_eq!(Layout::new(&doc, 8).widest_code, Some(2));
        assert_eq!(width_in_cells("\t\tx", 4), 9);
    }

    #[test]
    fn split_keeps_newline_annotations_and_file_boundaries() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\n@@ -1 +1 @@\n-Old hut\n",
            "\\ No newline at end of file\n+New hut\n\\ No newline at end of file\n",
            "diff --git a/map b/map\n@@ -0,0 +1 @@\n+map\n\n[an unexpected line]\n",
        ));
        let layout = Layout::new(&doc, 4);
        assert_eq!(layout.split, [
            SplitRow::Shared(0),
            SplitRow::Shared(1),
            SplitRow::Sides { old: Some(2), new: Some(4) },
            SplitRow::Sides { old: Some(3), new: Some(5) },
            SplitRow::Shared(6),
            SplitRow::Shared(7),
            SplitRow::Sides { old: None, new: Some(8) },
            SplitRow::Shared(9),
        ]);
    }
}
