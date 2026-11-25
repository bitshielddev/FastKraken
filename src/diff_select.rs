use crate::{
    Tab,
    commands::{Availability, Command, Group, Target},
    diff_doc::{DiffDoc, Kind},
    notice::Notice,
    view::{self, Desk, DiffMode, Selection, Tone, Ui},
};
use gpui::{
    Context, CursorStyle, Div, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, UniformListScrollHandle, Window,
    prelude::*, px,
};
use std::{cell::Cell, ops::Range, path::PathBuf, rc::Rc, time::Duration};
use unicode_segmentation::UnicodeSegmentation;

const SCROLL_STEP: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Column {
    Unified,
    Old,
    New,
}

impl Column {
    pub(crate) fn holds(self, kind: Kind) -> bool {
        match self {
            Self::Unified => kind.is_code(),
            Self::Old => matches!(kind, Kind::Context | Kind::Deleted),
            Self::New => matches!(kind, Kind::Context | Kind::Added),
        }
    }

    fn shown(self, mode: DiffMode) -> bool {
        matches!((self, mode), (Self::Unified, DiffMode::Unified) | (Self::Old | Self::New, DiffMode::Split))
    }

    fn index(self) -> usize {
        match self {
            Self::Unified => 0,
            Self::Old => 1,
            Self::New => 2,
        }
    }

    pub(crate) fn split(old: bool) -> Self {
        if old { Self::Old } else { Self::New }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Spot {
    pub(crate) row: usize,
    pub(crate) at: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Character,
    Word,
    Line,
}

impl Unit {
    pub(crate) fn of(clicks: usize) -> Self {
        match clicks {
            0 | 1 => Self::Character,
            2 => Self::Word,
            _ => Self::Line,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Edge {
    #[default]
    Inside,
    Above,
    Below,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Press { column: Column, spot: Spot, clicks: usize, extend: bool },
    DragTo { spot: Spot, edge: Edge },
    Release,
    Clear,
    SelectAll,
    Copy,
    CopyLine(usize),
    CopyHunk(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Chosen {
    selection: Selection,
    fingerprint: u64,
    pub(crate) column: Column,
    pub(crate) anchor: Spot,
    pub(crate) head: Spot,
}

impl Chosen {
    pub(crate) fn ordered(&self) -> (Spot, Spot) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    origin: (Spot, Spot),
    unit: Unit,
}

#[derive(Default)]
pub(crate) struct State {
    chosen: Option<Chosen>,
    drag: Option<Drag>,
    edge: Edge,
    ticking: bool,
    focused: bool,
    origins: Rc<Cell<[Pixels; 3]>>,
}

impl State {
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(crate) fn unfocus(&mut self) {
        self.focused = false;
    }
}

fn showing(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::Diff
        && !ui.diff.lines.is_empty()
        && !crate::inspection::showing_history(ui)
        && crate::image_diff::showing(ui).is_none()
}

fn held(ui: &Ui) -> Option<&Chosen> {
    let chosen = ui.text.chosen.as_ref()?;
    (chosen.column.shown(ui.shown_mode()) && ui.selection.as_ref() == Some(&chosen.selection) && showing(ui)).then_some(chosen)
}

pub(crate) fn current(ui: &Ui) -> Option<&Chosen> {
    held(ui).filter(|chosen| chosen.anchor != chosen.head)
}

pub(crate) fn focused(ui: &Ui) -> bool {
    ui.text.focused && showing(ui) && !ui.modal_open()
}

pub(crate) fn keep_focus(ui: &mut Ui) {
    if current(ui).is_some() {
        ui.text.focused = true;
    }
}

fn boundary(text: &str, at: usize) -> usize {
    text.floor_char_boundary(at.min(text.len()))
}

pub(crate) fn span(ui: &Ui, row: usize, column: Column) -> Option<Range<usize>> {
    let chosen = current(ui).filter(|chosen| chosen.column == column)?;
    let (start, end) = chosen.ordered();
    if row < start.row || row > end.row {
        return None;
    }
    let line = ui.diff.lines.get(row).filter(|line| column.holds(line.kind))?;
    let code = ui.diff.code(line);
    let from = if row == start.row { boundary(code, start.at) } else { 0 };
    let to = if row == end.row { boundary(code, end.at) } else { code.len() };
    (from < to).then_some(from..to)
}

pub(crate) fn text(doc: &DiffDoc, column: Column, start: Spot, end: Spot) -> String {
    let mut copied = String::new();
    let mut first = true;
    let last = end.row.min(doc.lines.len().saturating_sub(1));
    for row in start.row..=last {
        let Some(line) = doc.lines.get(row).filter(|line| column.holds(line.kind)) else { continue };
        let code = doc.code(line);
        let from = if row == start.row { boundary(code, start.at) } else { 0 };
        let to = if row == end.row { boundary(code, end.at) } else { code.len() };
        if !first {
            copied.push('\n');
        }
        first = false;
        copied.push_str(&code[from..to.max(from)]);
    }
    copied
}

pub(crate) fn selected_text(ui: &Ui) -> Option<String> {
    let chosen = current(ui)?;
    let (start, end) = chosen.ordered();
    Some(text(&ui.diff, chosen.column, start, end))
}

fn word(text: &str, at: usize) -> Range<usize> {
    let at = boundary(text, at);
    let mut last = at..at;
    for (start, piece) in text.split_word_bound_indices() {
        last = start..start + piece.len();
        if at < last.end {
            break;
        }
    }
    last
}

fn unit_at(doc: &DiffDoc, spot: Spot, unit: Unit) -> (Spot, Spot) {
    let code = doc.lines.get(spot.row).map_or("", |line| doc.code(line));
    let row = spot.row;
    match unit {
        Unit::Character => {
            let at = boundary(code, spot.at);
            (Spot { row, at }, Spot { row, at })
        }
        Unit::Word => {
            let range = word(code, spot.at);
            (Spot { row, at: range.start }, Spot { row, at: range.end })
        }
        Unit::Line => (Spot { row, at: 0 }, Spot { row, at: code.len() }),
    }
}

fn dragged(doc: &DiffDoc, drag: Drag, spot: Spot) -> (Spot, Spot) {
    let reached = unit_at(doc, spot, drag.unit);
    if reached.0 < drag.origin.0 {
        (drag.origin.1, reached.0)
    } else {
        (drag.origin.0, reached.1.max(drag.origin.1))
    }
}

pub(crate) fn everything(doc: &DiffDoc, column: Column) -> Option<(Spot, Spot)> {
    let held = |(_, line): &(usize, &crate::diff_doc::Line)| column.holds(line.kind);
    let (first, _) = doc.lines.iter().enumerate().find(held)?;
    let (last, line) = doc.lines.iter().enumerate().rev().find(held)?;
    Some((Spot { row: first, at: 0 }, Spot { row: last, at: doc.code(line).len() }))
}

pub(crate) fn reloaded(ui: &mut Ui) {
    let kept = ui.text.chosen.as_ref().is_some_and(|chosen| {
        ui.selection.as_ref() == Some(&chosen.selection) && chosen.fingerprint == ui.diff.fingerprint()
    });
    if !kept {
        ui.text.chosen = None;
        ui.text.drag = None;
        ui.text.edge = Edge::Inside;
    }
}

fn fingerprint(ui: &Ui, selection: &Selection) -> u64 {
    match &ui.text.chosen {
        Some(chosen) if &chosen.selection == selection => chosen.fingerprint,
        _ => ui.diff.fingerprint(),
    }
}

fn choose(ui: &mut Ui, column: Column, anchor: Spot, head: Spot) {
    let Some(selection) = ui.selection.clone() else { return };
    let fingerprint = fingerprint(ui, &selection);
    ui.text.chosen = Some(Chosen { selection, fingerprint, column, anchor, head });
    ui.text.focused = true;
}

pub(crate) fn file_of(ui: &Ui, row: usize) -> Option<PathBuf> {
    match &ui.selection {
        Some(Selection::File { path, .. }) => return Some(path.clone()),
        Some(Selection::Commit(_)) if ui.inspection.path.is_some() => return ui.inspection.path.clone(),
        Some(Selection::Stash(_)) if ui.stash.open_path().is_some() => {
            return ui.stash.open_path().map(std::path::Path::to_path_buf);
        }
        _ => {}
    }
    let doc = &ui.diff;
    let header = doc.lines.get(..=row)?.iter().rev().find(|line| line.kind == Kind::File)?;
    header_path(doc.text(header))
}

pub(crate) fn header_path(header: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = header.strip_prefix("diff --git ")?;
    let path = if rest.ends_with('"') {
        let start = rest.rfind(" \"b/")? + 1;
        unquoted(&rest[start..])?.strip_prefix(b"b/")?.to_vec()
    } else {
        let same = rest.strip_prefix("a/").and_then(|both| {
            let length = both.len().checked_sub(3)? / 2;
            let (old, new) = (both.get(..length)?, both.get(length..)?);
            (new.strip_prefix(" b/")? == old).then_some(old)
        });
        match same {
            Some(path) => path,
            None => rest.rsplit_once(" b/")?.1,
        }
        .as_bytes()
        .to_vec()
    };
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(path)))
}

fn unquoted(quoted: &str) -> Option<Vec<u8>> {
    let mut bytes = quoted.strip_prefix('"')?.strip_suffix('"')?.bytes();
    let mut path = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            path.push(byte);
            continue;
        }
        let escaped = bytes.next()?;
        path.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'0'..=b'3' => {
                let (middle, low) = (bytes.next()?, bytes.next()?);
                let digit = |byte: u8| matches!(byte, b'0'..=b'7').then(|| byte - b'0');
                (escaped - b'0') * 64 + digit(middle)? * 8 + digit(low)?
            }
            other => other,
        });
    }
    Some(path)
}

pub(crate) fn code_target(ui: &Ui, row: usize, at: u32) -> Option<Target> {
    let line = ui.diff.lines.get(row).filter(|line| line.kind.is_code())?;
    let hunk = line.hunk.filter(|&hunk| hunk < ui.diff.hunks.len() && ui.diff.state == crate::diff_doc::PatchState::Text);
    Some(Target::Code {
        path: file_of(ui, row),
        line: editor_line(&ui.selection, &ui.files, at),
        row,
        hunk,
    })
}

pub(crate) fn editor_line(selection: &Option<Selection>, files: &[view::FileRow], at: u32) -> Option<u32> {
    let Some(Selection::File { path, staged }) = selection else { return None };
    if at == 0 || (*staged && files.iter().any(|file| !file.staged && file.path == *path)) {
        return None;
    }
    Some(at)
}

fn copy(tab: &mut Tab, text: String, what: &str, cx: &mut Context<Tab>) {
    view::copy(text, cx);
    tab.ui.notices.show(Notice::copied(what));
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Press { column, spot, clicks, extend } => {
            if !showing(&tab.ui) || !tab.ui.diff.lines.get(spot.row).is_some_and(|line| column.holds(line.kind)) {
                return;
            }
            let unit = Unit::of(clicks);
            let kept = held(&tab.ui).filter(|chosen| chosen.column == column).map(|chosen| chosen.anchor);
            let (anchor, head, origin) = match kept {
                Some(anchor) if extend && unit == Unit::Character => {
                    let (at, _) = unit_at(&tab.ui.diff, spot, unit);
                    (anchor, at, (anchor, anchor))
                }
                _ => {
                    let origin = unit_at(&tab.ui.diff, spot, unit);
                    (origin.0, origin.1, origin)
                }
            };
            tab.ui.text.drag = Some(Drag { origin, unit });
            tab.ui.text.edge = Edge::Inside;
            choose(&mut tab.ui, column, anchor, head);
            cx.notify();
        }
        Action::DragTo { spot, edge } => {
            let Some(drag) = tab.ui.text.drag else { return };
            let Some(column) = tab.ui.text.chosen.as_ref().map(|chosen| chosen.column) else { return };
            tab.ui.text.edge = edge;
            let (anchor, head) = dragged(&tab.ui.diff, drag, spot);
            if tab.ui.text.chosen.as_ref().is_some_and(|chosen| chosen.anchor != anchor || chosen.head != head) {
                choose(&mut tab.ui, column, anchor, head);
                cx.notify();
            }
            if edge != Edge::Inside {
                tick(tab, cx);
            }
        }
        Action::Release => {
            tab.ui.text.drag = None;
            tab.ui.text.edge = Edge::Inside;
        }
        Action::Clear => {
            tab.ui.text.chosen = None;
            tab.ui.text.drag = None;
            cx.notify();
        }
        Action::SelectAll => {
            if !showing(&tab.ui) {
                return;
            }
            let column = match tab.ui.shown_mode() {
                DiffMode::Unified => Column::Unified,
                DiffMode::Split => current(&tab.ui).map_or(Column::New, |chosen| chosen.column),
            };
            if let Some((start, end)) = everything(&tab.ui.diff, column) {
                tab.ui.text.drag = None;
                choose(&mut tab.ui, column, start, end);
                cx.notify();
            }
        }
        Action::Copy => {
            if let Some(text) = selected_text(&tab.ui) {
                copy(tab, text, "text", cx);
            }
        }
        Action::CopyLine(row) => {
            let line = tab.ui.diff.lines.get(row).filter(|line| line.kind.is_code());
            if let Some(text) = line.map(|line| tab.ui.diff.code(line).to_owned()) {
                copy(tab, text, "line", cx);
            }
        }
        Action::CopyHunk(hunk) => match tab.diff.as_ref().and_then(|diff| diff.hunk_patch(hunk)) {
            Some(patch) => copy(tab, patch, "hunk", cx),
            None => {
                tab.ui.say(Tone::Error, "This hunk cannot be copied as a patch");
                cx.notify();
            }
        },
    }
}

fn tick(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.text.ticking {
        return;
    }
    tab.ui.text.ticking = true;
    cx.spawn(async move |tab, cx| {
        loop {
            cx.background_executor().timer(SCROLL_STEP).await;
            let going = tab.update(cx, |tab, cx| step(tab, cx));
            if !matches!(going, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

fn step(tab: &mut Tab, cx: &mut Context<Tab>) -> bool {
    let ui = &tab.ui;
    let edge = ui.text.edge;
    let column = ui.text.chosen.as_ref().map(|chosen| chosen.column);
    let (Some(drag), Some(column)) = (ui.text.drag.filter(|_| edge != Edge::Inside && showing(ui)), column) else {
        tab.ui.text.ticking = false;
        return false;
    };
    let rows = view::diff_rows(ui);
    let Some(visual) = scrolled(&ui.diff_scroll, edge, rows, ui.diff_text.face.line()) else { return true };
    let Some(row) = view::diff_doc_row(ui, visual, column) else { return true };
    let piece = view::diff_piece(ui, row, view::diff_part(ui, visual));
    let at = if edge == Edge::Above { piece.start } else { piece.end };
    let (anchor, head) = dragged(&ui.diff, drag, Spot { row, at });
    choose(&mut tab.ui, column, anchor, head);
    cx.notify();
    true
}

fn scrolled(handle: &UniformListScrollHandle, edge: Edge, rows: usize, line: f32) -> Option<usize> {
    let state = handle.0.borrow();
    let base = &state.base_handle;
    let height = f32::from(base.bounds().size.height);
    if rows == 0 || height <= 0. {
        return None;
    }
    let bottom = -base.max_offset().height;
    let mut offset = base.offset();
    offset.y = match edge {
        Edge::Above => (offset.y + px(line)).min(px(0.)),
        _ => (offset.y - px(line)).max(bottom),
    };
    base.set_offset(offset);
    let top = -f32::from(offset.y);
    let visual = match edge {
        Edge::Above => (top / line).floor(),
        _ => ((top + height) / line).ceil() - 1.,
    };
    Some((visual.max(0.) as usize).min(rows - 1))
}

fn pointed(handle: &UniformListScrollHandle, y: Pixels, rows: usize, line: f32) -> Option<(usize, Edge)> {
    let state = handle.0.borrow();
    let base = &state.base_handle;
    let bounds = base.bounds();
    if rows == 0 || bounds.size.height <= px(0.) {
        return None;
    }
    let edge = if y < bounds.top() {
        Edge::Above
    } else if y >= bounds.bottom() {
        Edge::Below
    } else {
        Edge::Inside
    };
    let inside = y.max(bounds.top()).min(bounds.bottom() - px(1.));
    let visual = f32::from(inside - bounds.top() - base.offset().y) / line;
    Some(((visual.max(0.) as usize).min(rows - 1), edge))
}

pub(crate) fn origin(ui: &Ui, column: Column) -> Pixels {
    ui.text.origins.get()[column.index()]
}

pub(crate) fn hit(ui: &Ui, window: &Window, row: usize, part: usize, column: Column, x: Pixels, unit: Unit) -> usize {
    let Some(line) = ui.diff.lines.get(row) else { return 0 };
    let code = ui.diff.code(line);
    let piece = view::diff_piece(ui, row, part);
    let shown = code.get(piece.clone()).unwrap_or_default();
    if shown.is_empty() {
        return boundary(code, piece.start);
    }
    let x = x - origin(ui, column);
    let face = &ui.diff_text.face;
    let drawn = crate::diff_text::expanded(shown, face.tab);
    let shaped = view::diff_shaped(window, face, &drawn);
    let (at, nearest) = match unit {
        Unit::Character => (shaped.closest_index_for_x(x), true),
        Unit::Word | Unit::Line => (shaped.index_for_x(x).unwrap_or(drawn.len()), false),
    };
    let at = crate::diff_text::stored_at(shown, at, face.tab, nearest);
    boundary(code, piece.start + at)
}

pub(crate) fn cell<T: Desk>(ui: &Ui, row: usize, part: usize, column: Column, code: Div, cx: &Context<T>) -> Div {
    let origins = ui.text.origins.clone();
    code.cursor(CursorStyle::IBeam)
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(text) = bounds.first() {
                let mut all = origins.get();
                all[column.index()] = text.left();
                origins.set(all);
            }
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                let unit = Unit::of(event.click_count);
                let at = hit(app.ui(), window, row, part, column, event.position.x, unit);
                view::focus(app.ui(), window);
                let press = Action::Press {
                    column,
                    spot: Spot { row, at },
                    clicks: event.click_count,
                    extend: event.modifiers.shift,
                };
                app.dispatch(view::Action::Text(press), window, cx);
            }),
        )
}

pub(crate) fn moved<T: Desk>(app: &mut T, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<T>) {
    let ui = app.ui();
    if !ui.text.dragging() {
        return;
    }
    if event.pressed_button != Some(MouseButton::Left) {
        app.dispatch(view::Action::Text(Action::Release), window, cx);
        return;
    }
    let Some(column) = ui.text.chosen.as_ref().map(|chosen| chosen.column) else { return };
    let Some((visual, edge)) = pointed(&ui.diff_scroll, event.position.y, view::diff_rows(ui), ui.diff_text.face.line()) else {
        return;
    };
    let Some(row) = view::diff_doc_row(ui, visual, column) else { return };
    let at = hit(ui, window, row, view::diff_part(ui, visual), column, event.position.x, Unit::Character);
    app.dispatch(view::Action::Text(Action::DragTo { spot: Spot { row, at }, edge }), window, cx);
}

pub(crate) fn released<T: Desk>(app: &mut T, window: &mut Window, cx: &mut Context<T>) {
    if app.ui().text.dragging() {
        app.dispatch(view::Action::Text(Action::Release), window, cx);
    }
}

fn code(target: &Target) -> Option<(usize, Option<usize>)> {
    match target {
        Target::Code { row, hunk, .. } => Some((*row, *hunk)),
        _ => None,
    }
}

fn keyed(ui: &Ui, target: &Target) -> bool {
    *target == Target::Repo && focused(ui)
}

pub(crate) const COMMANDS: &[Command] = {
    use view::Action as Run;
    &[
        Command::new(
            "text.copy",
            Group::Copy,
            |_, _| "Copy".into(),
            |ui, target, _| match target {
                Target::Code { .. } => {
                    Availability::unless(current(ui).is_none().then(|| "Select text in the diff first".to_owned()))
                }
                _ if keyed(ui, target) => Availability::Enabled,
                _ => Availability::Hidden,
            },
            |_, _| Some(Run::Text(Action::Copy)),
        )
        .words(&["clipboard", "selection", "text"])
        .icon("copy")
        .keys("cmd-c")
        .scope("DiffText"),
        Command::new(
            "text.copy-line",
            Group::Copy,
            |_, _| "Copy line".into(),
            |_, target, _| if code(target).is_some() { Availability::Enabled } else { Availability::Hidden },
            |_, target| code(target).map(|(row, _)| Run::Text(Action::CopyLine(row))),
        )
        .words(&["clipboard"])
        .icon("copy"),
        Command::new(
            "text.copy-hunk",
            Group::Copy,
            |_, _| "Copy hunk as patch".into(),
            |_, target, _| match code(target) {
                Some((_, Some(_))) => Availability::Enabled,
                _ => Availability::Hidden,
            },
            |_, target| code(target).and_then(|(_, hunk)| hunk).map(|hunk| Run::Text(Action::CopyHunk(hunk))),
        )
        .words(&["clipboard", "diff", "patch"])
        .icon("copy"),
    ]
};

pub(crate) const SELECT_COMMANDS: &[Command] = &[Command::new(
    "text.select-all",
    Group::Copy,
    |_, _| "Select all".into(),
    |ui, target, _| {
        if code(target).is_some() || keyed(ui, target) { Availability::Enabled } else { Availability::Hidden }
    },
    |_, _| Some(view::Action::Text(Action::SelectAll)),
)
.words(&["selection", "text", "everything"])
.keys("cmd-a")
.scope("DiffText")];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;

    fn sample() -> DiffDoc {
        doc(concat!(
            "diff --git a/log b/log\nindex 012..345 100644\n--- a/log\n+++ b/log\n",
            "@@ -1,3 +1,3 @@\n keep the trail\n-old ford\n+new bridge\n end here\n",
            "@@ -20,2 +20,3 @@\n camp\n+fire pit\n water\n",
        ))
    }

    fn at(row: usize, at: usize) -> Spot {
        Spot { row, at }
    }

    #[test]
    fn each_column_holds_only_its_side_of_the_diff() {
        let held = |column: Column| -> Vec<usize> {
            let doc = sample();
            (0..doc.lines.len()).filter(|&row| column.holds(doc.lines[row].kind)).collect()
        };
        assert_eq!(held(Column::Unified), [1, 2, 3, 4, 6, 7, 8]);
        assert_eq!(held(Column::Old), [1, 2, 4, 6, 8]);
        assert_eq!(held(Column::New), [1, 3, 4, 6, 7, 8]);
    }

    #[test]
    fn copied_text_leaves_out_marks_headers_and_the_other_side() {
        let doc = sample();
        assert_eq!(text(&doc, Column::Unified, at(1, 5), at(3, 3)), "the trail\nold ford\nnew");
        assert_eq!(text(&doc, Column::Unified, at(4, 0), at(7, 4)), "end here\ncamp\nfire");
        assert_eq!(text(&doc, Column::Old, at(1, 0), at(4, 3)), "keep the trail\nold ford\nend");
        assert_eq!(text(&doc, Column::New, at(2, 2), at(3, 99)), "new bridge");
        assert_eq!(text(&doc, Column::New, at(3, 4), at(3, 4)), "");
        assert_eq!(everything(&doc, Column::Unified), Some((at(1, 0), at(8, 5))));
        assert_eq!(everything(&doc, Column::Old), Some((at(1, 0), at(8, 5))));
        let (start, end) = everything(&doc, Column::New).unwrap();
        assert_eq!(text(&doc, Column::New, start, end), "keep the trail\nnew bridge\nend here\ncamp\nfire pit\nwater");
    }

    #[test]
    fn offsets_inside_a_character_fall_back_to_its_start() {
        let doc = doc("diff --git a/c b/c\n@@ -1 +1 @@\n-café au lait\n+café noir\n");
        assert_eq!(text(&doc, Column::Unified, at(2, 4), at(2, 99)), "é noir");
        assert_eq!(text(&doc, Column::Unified, at(1, 0), at(1, 4)), "caf");
    }

    #[test]
    fn double_and_triple_presses_take_a_word_and_a_line_and_dragging_extends_by_them() {
        let doc = sample();
        assert_eq!(unit_at(&doc, at(3, 6), Unit::Word), (at(3, 4), at(3, 10)));
        assert_eq!(unit_at(&doc, at(3, 3), Unit::Word), (at(3, 3), at(3, 4)));
        assert_eq!(unit_at(&doc, at(1, 2), Unit::Line), (at(1, 0), at(1, 14)));
        assert_eq!(unit_at(&doc, at(1, 99), Unit::Character), (at(1, 14), at(1, 14)));
        let words = Drag { origin: unit_at(&doc, at(3, 6), Unit::Word), unit: Unit::Word };
        assert_eq!(dragged(&doc, words, at(4, 1)), (at(3, 4), at(4, 3)));
        assert_eq!(dragged(&doc, words, at(1, 6)), (at(3, 10), at(1, 5)));
        let lines = Drag { origin: unit_at(&doc, at(2, 0), Unit::Line), unit: Unit::Line };
        assert_eq!(dragged(&doc, lines, at(3, 1)), (at(2, 0), at(3, 10)));
        assert_eq!(Unit::of(1), Unit::Character);
        assert_eq!(Unit::of(2), Unit::Word);
        assert_eq!(Unit::of(3), Unit::Line);
    }

    #[test]
    fn the_file_of_a_row_comes_from_its_header_in_a_whole_commit() {
        assert_eq!(header_path("diff --git a/src/log b/src/log"), Some(PathBuf::from("src/log")));
        assert_eq!(header_path("diff --git \"a/my log\" \"b/my log\""), Some(PathBuf::from("my log")));
        assert_eq!(header_path("diff --git a/x b/"), None);
        assert_eq!(header_path("diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\""), Some(PathBuf::from("café.txt")));
        assert_eq!(header_path("diff --git \"a/tab\\tname\" \"b/tab\\tname\""), Some(PathBuf::from("tab\tname")));
        assert_eq!(header_path("diff --git a/x b/y.txt b/x b/y.txt"), Some(PathBuf::from("x b/y.txt")));
        assert_eq!(header_path("diff --git a/old name b/new name"), Some(PathBuf::from("new name")));
        assert_eq!(header_path("diff --git \"a/\\\"quote\\\\\" b/plain"), Some(PathBuf::from("plain")));
        assert_eq!(header_path("@@ -1 +1 @@"), None);
    }

    #[test]
    fn every_file_of_a_real_commit_diff_is_named_by_its_header() {
        let f = crate::repository::tests::Fixture::new();
        let names = ["café.txt", "tab\tname.txt", "x b/y.txt", "quote\"d.txt", "plain.txt"];
        for name in names {
            let path = f.dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "shore\n").unwrap();
        }
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "Name the files"]);
        let head = f.git(&["rev-parse", "HEAD"]);
        let diff = f.repo.commit_diff(String::from_utf8_lossy(&head).trim()).unwrap();
        let shown = doc(&diff.text);
        let mut found: Vec<PathBuf> = shown
            .lines
            .iter()
            .filter(|line| line.kind == Kind::File)
            .filter_map(|line| header_path(shown.text(line)))
            .collect();
        found.sort();
        let mut expected: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn the_editor_line_is_offered_only_while_the_working_tree_has_the_same_lines() {
        let row = |staged| view::FileRow { path: "trail.txt".into(), staged, status: "M", from: None };
        let selected = |staged| Some(Selection::File { path: "trail.txt".into(), staged });
        assert_eq!(editor_line(&selected(false), &[row(false)], 2), Some(2));
        assert_eq!(editor_line(&selected(true), &[row(true)], 2), Some(2));
        assert_eq!(editor_line(&selected(true), &[row(true), row(false)], 2), None);
        assert_eq!(editor_line(&selected(false), &[row(true), row(false)], 2), Some(2));
        assert_eq!(editor_line(&selected(false), &[row(false)], 0), None);
        assert_eq!(editor_line(&Some(Selection::Commit("abc".into())), &[], 2), None);
    }
}
