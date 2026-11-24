#[path = "commit_input/editing.rs"]
mod editing;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, EntityId, EntityInputHandler,
    FocusHandle, Focusable, Global, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, ShapedLine,
    ScrollWheelEvent, TextRun, UTF16Selection, WeakEntity, Window, actions, anchored, canvas, deferred, div, fill,
    point, prelude::*, px, rgb, size,
};
use crate::theme::*;
use editing::{Edit, Editor, Unit};
use std::{ops::Range, time::Instant};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    commit_field,
    [PasteSummary, CopySummary, CutSummary, SelectSummary, UndoText, RedoText]
);

pub fn bindings(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let ctx = Some("CommitField");
    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-a"), SelectSummary, ctx),
        KeyBinding::new(&format!("{modifier}-v"), PasteSummary, ctx),
        KeyBinding::new(&format!("{modifier}-c"), CopySummary, ctx),
        KeyBinding::new(&format!("{modifier}-x"), CutSummary, ctx),
        KeyBinding::new(&format!("{modifier}-z"), UndoText, ctx),
        KeyBinding::new(&format!("{modifier}-shift-z"), RedoText, ctx),
    ]);
}

pub(crate) const TEXT_KEYS: &[(&str, &str)] = &[
    ("alt-left", "Previous word in a text field"),
    ("alt-right", "Next word in a text field"),
    ("cmd-left", "Start of the line in a text field"),
    ("cmd-right", "End of the line in a text field"),
    ("cmd-up", "Start of the text in a text field"),
    ("cmd-down", "End of the text in a text field"),
    ("alt-backspace", "Delete the previous word"),
    ("alt-delete", "Delete the next word"),
    ("cmd-backspace", "Delete to the start of the line"),
    ("ctrl-a", "Start of the paragraph"),
    ("ctrl-e", "End of the paragraph"),
    ("ctrl-k", "Delete to the end of the paragraph"),
    ("cmd-z", "Undo typing"),
    ("cmd-shift-z", "Redo typing"),
];

const LINE_HEIGHT: f32 = 20.;
const BULLET: &str = "•";
const DESCRIPTION_ROWS: usize = 3;
const MENU_WIDTH: f32 = 200.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldCommand {
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuItem {
    pub command: FieldCommand,
    pub label: &'static str,
    pub keys: Option<&'static str>,
    pub enabled: bool,
}

fn menu_items(selected: bool, masked: bool, has_text: bool, paste: bool) -> Vec<MenuItem> {
    let copyable = selected && !masked;
    [
        (FieldCommand::Cut, "Cut", Some("cmd-x"), copyable),
        (FieldCommand::Copy, "Copy", Some("cmd-c"), copyable),
        (FieldCommand::Paste, "Paste", Some("cmd-v"), paste),
        (FieldCommand::Delete, "Delete", None, selected),
        (FieldCommand::SelectAll, "Select All", Some("cmd-a"), has_text),
    ]
    .into_iter()
    .map(|(command, label, keys, enabled)| MenuItem { command, label, keys, enabled })
    .collect()
}

struct OpenMenu {
    field: WeakEntity<CommitInput>,
    at: Point<Pixels>,
    paste: bool,
}

#[derive(Default)]
struct FieldMenu(Option<OpenMenu>);

impl Global for FieldMenu {}

fn close_menu(cx: &mut App) -> bool {
    let open = cx.try_global::<FieldMenu>().is_some_and(|menu| menu.0.is_some());
    if open {
        cx.set_global(FieldMenu(None));
    }
    open
}

pub(crate) fn menu_shown_for(field: EntityId, cx: &App) -> bool {
    cx.try_global::<FieldMenu>()
        .and_then(|menu| menu.0.as_ref())
        .is_some_and(|open| open.field.entity_id() == field)
}

fn menu_open_at(cx: &App) -> Option<Point<Pixels>> {
    cx.try_global::<FieldMenu>()?.0.as_ref().map(|open| open.at)
}

pub(crate) fn field_menu(window: &mut Window, cx: &mut App) -> Option<impl IntoElement + use<>> {
    let (field, at, paste) = {
        let open = cx.try_global::<FieldMenu>()?.0.as_ref()?;
        (open.field.clone(), open.at, open.paste)
    };
    let Some(entity) = field.upgrade().filter(|entity| entity.read(cx).focus.is_focused(window)) else {
        close_menu(cx);
        return None;
    };
    let mut menu = crate::kit::float().w(px(MENU_WIDTH)).py_1();
    for (index, item) in entity.read(cx).menu_items(paste).into_iter().enumerate() {
        if item.command == FieldCommand::SelectAll {
            menu = menu.child(crate::kit::separator());
        }
        let mut row = crate::kit::item(ElementId::NamedInteger("field-menu".into(), index as u64), item.label)
            .enabled(item.enabled);
        if let Some(keys) = item.keys {
            row = row.meta(crate::commands::keys(keys));
        }
        let row = row.inert();
        menu = menu.child(if item.enabled {
            let (entity, command) = (entity.clone(), item.command);
            row.on_click(move |_, window, cx| entity.update(cx, |input, cx| input.choose(command, window, cx)))
        } else {
            row
        });
    }
    let menu = menu.on_mouse_down_out(move |_, window, cx| {
        if menu_open_at(cx) == Some(at) {
            close_menu(cx);
            window.refresh();
        }
    });
    Some(
        deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(menu))
            .with_priority(5),
    )
}

struct InputLine {
    range: Range<usize>,
    shape: ShapedLine,
}

struct InputLayout {
    lines: Vec<InputLine>,
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
}

impl InputLayout {
    fn row(&self, index: usize) -> usize {
        self.lines.partition_point(|line| line.range.start <= index) - 1
    }

    fn point(&self, index: usize) -> Point<Pixels> {
        let row = self.row(index);
        let line = &self.lines[row];
        self.origin + point(line.shape.x_for_index(index - line.range.start), px(LINE_HEIGHT * row as f32))
    }

    fn hit(&self, position: Point<Pixels>) -> usize {
        let row = (((position.y - self.origin.y) / px(LINE_HEIGHT)) as usize).min(self.lines.len() - 1);
        let line = &self.lines[row];
        line.range.start + line.shape.closest_index_for_x(position.x - self.origin.x).min(line.range.len())
    }

    fn under(&self, position: Point<Pixels>) -> usize {
        let row = (((position.y - self.origin.y) / px(LINE_HEIGHT)) as usize).min(self.lines.len() - 1);
        let line = &self.lines[row];
        let glyph = line.shape.index_for_x(position.x - self.origin.x).unwrap_or(line.range.len());
        line.range.start + glyph.min(line.range.len())
    }
}

pub struct CommitInput {
    focus: FocusHandle,
    placeholder: &'static str,
    editor: Editor,
    marked: Option<Range<usize>>,
    layout: Option<InputLayout>,
    count: bool,
    scroll_top: Pixels,
    reveal_caret: bool,
    drag: Option<(Range<usize>, Unit)>,
}

impl CommitInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut input = Self::with_placeholder(cx, "Commit summary");
        input.count = true;
        input
    }

    pub fn description(cx: &mut Context<Self>) -> Self {
        Self::multiline(cx, "Description (optional)")
    }

    pub fn multiline(cx: &mut Context<Self>, placeholder: &'static str) -> Self {
        let mut input = Self::with_placeholder(cx, placeholder);
        input.editor.multiline = true;
        input
    }

    pub fn with_placeholder(cx: &mut Context<Self>, placeholder: &'static str) -> Self {
        Self {
            placeholder,
            focus: cx.focus_handle().tab_stop(true),
            editor: Editor::new(false, false),
            marked: None,
            layout: None,
            drag: None,
            count: false,
            scroll_top: px(0.),
            reveal_caret: true,
        }
    }

    pub fn secret(cx: &mut Context<Self>, placeholder: &'static str) -> Self {
        let mut input = Self::with_placeholder(cx, placeholder);
        input.editor.masked = true;
        input
    }

    pub fn text(&self) -> &str {
        &self.editor.value
    }

    pub(crate) fn selected_range(&self) -> Range<usize> {
        self.editor.selection()
    }

    pub fn set_placeholder(&mut self, placeholder: &'static str, cx: &mut Context<Self>) {
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    fn display(&self) -> String {
        if self.editor.masked {
            BULLET.repeat(self.editor.value.chars().count())
        } else {
            self.editor.value.clone()
        }
    }

    fn to_display(&self, index: usize) -> usize {
        if self.editor.masked {
            self.editor.value[..index].chars().count() * BULLET.len()
        } else {
            index
        }
    }

    fn from_display(&self, index: usize) -> usize {
        if self.editor.masked {
            self.editor.value.char_indices().nth(index / BULLET.len()).map_or(self.editor.value.len(), |(at, _)| at)
        } else {
            index
        }
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.editor.reset(text);
        self.changed(cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.layout = None;
        self.reveal_caret = true;
        cx.notify();
    }

    fn selection(&self) -> Range<usize> {
        self.editor.selection()
    }

    fn replace(&mut self, range: Range<usize>, text: &str, edit: Edit, cx: &mut Context<Self>) -> usize {
        let composing = self.marked.is_some();
        let inserted = self.editor.replace(range, text, edit, Instant::now(), composing);
        self.changed(cx);
        inserted
    }

    fn hit(&self, point: Point<Pixels>) -> usize {
        self.layout.as_ref().map_or(self.editor.value.len(), |layout| self.from_display(layout.hit(point)))
    }

    fn hit_for(&self, point: Point<Pixels>, unit: Unit) -> usize {
        match (&self.layout, unit) {
            (Some(layout), Unit::Word | Unit::Paragraph) => self.from_display(layout.under(point)),
            _ => self.hit(point),
        }
    }

    fn rows(&self) -> Option<Vec<Range<usize>>> {
        let layout = self.layout.as_ref().filter(|_| self.editor.multiline && !self.editor.masked)?;
        Some(layout.lines.iter().map(|line| line.range.clone()).collect())
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let s = self.selection();
        if !s.is_empty() && !self.editor.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.editor.value[s].to_owned()));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|v| v.text()) {
            self.replace(self.selection(), &text, Edit::Other, cx);
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if !self.editor.masked {
            self.copy(cx);
            self.replace(self.selection(), "", Edit::Other, cx);
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.editor.select_all();
        self.reveal_caret = true;
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if self.marked.is_none() && self.editor.undo() {
            self.changed(cx);
        }
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        if self.marked.is_none() && self.editor.redo() {
            self.changed(cx);
        }
    }

    pub(crate) fn menu_items(&self, paste: bool) -> Vec<MenuItem> {
        menu_items(!self.selection().is_empty(), self.editor.masked, !self.editor.value.is_empty(), paste)
    }

    pub(crate) fn choose(&mut self, command: FieldCommand, window: &mut Window, cx: &mut Context<Self>) {
        close_menu(cx);
        self.run(command, cx);
        window.refresh();
    }

    fn run(&mut self, command: FieldCommand, cx: &mut Context<Self>) {
        match command {
            FieldCommand::Cut => self.cut(cx),
            FieldCommand::Copy => self.copy(cx),
            FieldCommand::Paste => self.paste(cx),
            FieldCommand::Delete => {
                if self.editor.delete_selection(Instant::now()) {
                    self.changed(cx);
                }
            }
            FieldCommand::SelectAll => self.select_all(cx),
        }
    }

    pub(crate) fn open_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let paste = cx.read_from_clipboard().and_then(|item| item.text()).is_some();
        cx.set_global(FieldMenu(Some(OpenMenu { field: cx.weak_entity(), at, paste })));
        window.refresh();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if close_menu(cx) {
            cx.notify();
        }
        let k = &event.keystroke;
        if self.marked.is_some() {
            return;
        }
        let plain = !k.modifiers.platform && !k.modifiers.control && !k.modifiers.alt;
        match k.key.as_str() {
            "enter" if self.editor.multiline && plain => {
                self.replace(self.selection(), "\n", Edit::Typing, cx);
            }
            "up" | "down" if self.editor.multiline && plain => {
                let Some(layout) = &self.layout else { return };
                let caret = self.editor.caret;
                let row = layout.row(caret);
                let target = if k.key == "up" { row.checked_sub(1) } else { Some(row + 1) };
                let to = match target.and_then(|i| layout.lines.get(i).map(|_| i)) {
                    Some(i) => layout.hit(point(layout.point(caret).x, layout.origin.y + px(LINE_HEIGHT * (i as f32 + 0.5)))),
                    None if k.key == "up" => 0,
                    None => self.editor.value.len(),
                };
                let anchor = if k.modifiers.shift { self.editor.anchor } else { to };
                self.editor.select(anchor, to);
            }
            _ => {
                let Some(command) = editing::command(k) else { return };
                let length = self.editor.value.len();
                let rows = self.rows();
                self.editor.apply(command, rows.as_deref(), Instant::now());
                if self.editor.value.len() != length {
                    self.layout = None;
                }
            }
        }
        self.reveal_caret = true;
        cx.notify();
        cx.stop_propagation();
    }

    pub(crate) fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        close_menu(cx);
        let unit = Unit::of_clicks(event.click_count);
        let at = self.hit_for(event.position, unit);
        let origin = if event.modifiers.shift && unit == Unit::Character {
            let anchor = self.editor.anchor;
            self.editor.select(anchor, at);
            anchor..anchor
        } else {
            let range = self.editor.unit_at(at, unit);
            self.editor.select(range.start, range.end);
            range
        };
        self.drag = Some((origin, unit));
        self.reveal_caret = true;
        cx.notify();
    }

    fn press_right(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let at = self.hit(event.position);
        let selected = self.selection();
        if selected.is_empty() || at < selected.start || at > selected.end {
            self.editor.select(at, at);
        }
        self.drag = None;
        self.open_menu(event.position, window, cx);
        cx.notify();
        cx.stop_propagation();
    }

    fn to_bytes(text: &str, utf16: Range<usize>) -> Range<usize> {
        let byte = |n| {
            let mut used = 0;
            for (index, ch) in text.char_indices() {
                if used + ch.len_utf16() > n {
                    return index;
                }
                used += ch.len_utf16();
            }
            text.len()
        };
        byte(utf16.start)..byte(utf16.end)
    }

    fn to_utf16(&self, bytes: Range<usize>) -> Range<usize> {
        self.editor.value[..bytes.start].encode_utf16().count()
            ..self.editor.value[..bytes.end].encode_utf16().count()
    }
}

impl Focusable for CommitInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for CommitInput {
    fn text_for_range(
        &mut self,
        r: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = Self::to_bytes(&self.editor.value, r);
        *actual = Some(self.to_utf16(r.clone()));
        Some(self.editor.value[r].to_owned())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(self.selection()),
            reversed: self.editor.caret < self.editor.anchor,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone().map(|r| self.to_utf16(r))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        close_menu(cx);
        let r = range
            .map(|v| Self::to_bytes(&self.editor.value, v))
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        self.replace(r, text, Edit::Typing, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, text, window, cx);
        let end = self.editor.caret;
        let start = end - self.editor.normalized(text).len();
        self.marked = (!text.is_empty()).then_some(start..end);
        if let Some(r) = selected {
            let bytes = Self::to_bytes(&self.editor.value[start..end], r);
            self.editor.anchor = start + bytes.start;
            self.editor.caret = start + bytes.end;
        }
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = Self::to_bytes(&self.editor.value, range);
        let start = layout.point(self.to_display(range.start));
        let end = layout.point(self.to_display(range.end));
        let right = if start.y == end.y { end.x } else { layout.bounds.right() };
        Some(Bounds::from_corners(start, point(right.max(start.x), start.y + px(LINE_HEIGHT))))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        layout.bounds.contains(&p)
            .then(|| self.editor.value[..self.from_display(layout.hit(p))].encode_utf16().count())
    }
}

impl CommitInput {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> InputLayout {
        let font = window.text_style().font();
        let text_system = window.text_system();
        let font_size = px(12.5);
        let line_height = px(LINE_HEIGHT);
        let run = |len, color| TextRun {
            len,
            font: font.clone(),
            color: rgb(color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shape = |text: &str, color| {
            text_system.shape_line(text.to_owned().into(), font_size, &[run(text.len(), color)], None)
        };
        let mut lines = Vec::new();
        let mut offset = 0;
        let display = self.display();
        for text in display.split('\n') {
            let mut start = 0;
            let wrapped = self.editor.multiline.then(|| {
                text_system.shape_text(
                    text.to_owned().into(),
                    font_size,
                    &[run(text.len(), TEXT)],
                    Some(bounds.size.width.max(px(1.))),
                    None,
                )
            });
            if let Some(Ok(wrapped)) = wrapped
                && let Some(line) = wrapped.first()
            {
                for boundary in line.wrap_boundaries() {
                    let end = line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index;
                    if end > start && text.is_char_boundary(end) {
                        lines.push(InputLine { range: offset + start..offset + end, shape: shape(&text[start..end], TEXT) });
                        start = end;
                    }
                }
            }
            lines.push(InputLine { range: offset + start..offset + text.len(), shape: shape(&text[start..], TEXT) });
            offset += text.len() + 1;
        }
        let placeholder = self.editor.value.is_empty().then(|| shape(self.placeholder, TEXT_3));
        let mut layout = InputLayout { lines, bounds, origin: bounds.origin };
        let caret = layout.point(self.to_display(self.editor.caret));
        if self.editor.multiline {
            let max_scroll = (line_height * layout.lines.len() - bounds.size.height).max(px(0.));
            if self.reveal_caret {
                let row_top = caret.y - bounds.top();
                let first_visible = (row_top + line_height - bounds.size.height).max(px(0.));
                self.scroll_top = self.scroll_top.clamp(first_visible, row_top);
                self.reveal_caret = false;
            }
            self.scroll_top = self.scroll_top.clamp(px(0.), max_scroll);
            layout.origin.y -= self.scroll_top;
        } else {
            layout.origin.x -= (caret.x + px(2.) - bounds.right()).max(px(0.));
        }

        let selection = self.selection();
        let selection = self.to_display(selection.start)..self.to_display(selection.end);
        let marked_range = self.marked.as_ref().map(|marked| self.to_display(marked.start)..self.to_display(marked.end));
        let focused = self.focus.is_focused(window);
        for (i, line) in layout.lines.iter().enumerate() {
            let y = layout.origin.y + px(i as f32 * LINE_HEIGHT);
            if y + line_height <= bounds.top() || y >= bounds.bottom() {
                continue;
            }
            let x = |index| layout.origin.x + line.shape.x_for_index(index - line.range.start);
            let selected_newline = selection.start <= line.range.end
                && selection.end > line.range.end
                && display.as_bytes().get(line.range.end) == Some(&b'\n');
            let selected_text = selection.start < line.range.end && selection.end > line.range.start;
            if focused && (selected_text || selected_newline) {
                let start = x(selection.start.max(line.range.start));
                let end = if selected_newline {
                    bounds.right()
                } else {
                    x(selection.end.min(line.range.end))
                };
                window.paint_quad(fill(
                    Bounds::new(
                        point(start.min(end), y + px(2.)),
                        size((end - start).abs(), line_height - px(4.)),
                    ),
                    rgb(TEXT_SELECTION),
                ));
            }
            if let Some(marked) = &marked_range
                && marked.start < line.range.end && marked.end > line.range.start
            {
                let left = x(marked.start.max(line.range.start));
                let right = x(marked.end.min(line.range.end));
                window.paint_quad(fill(
                    Bounds::new(
                        point(left.min(right), y + line_height - px(2.)),
                        size((right - left).abs(), px(1.)),
                    ),
                    rgb(ACCENT),
                ));
            }
            let _ = line.shape.paint(point(layout.origin.x, y), line_height, window, cx);
        }
        if let Some(placeholder) = placeholder {
            let _ = placeholder.paint(bounds.origin, line_height, window, cx);
        }
        if focused && selection.is_empty() {
            let caret = layout.point(self.to_display(self.editor.caret));
            window.paint_quad(fill(
                Bounds::new(caret + point(px(0.), px(2.)), size(px(1.), line_height - px(4.))),
                rgb(ACCENT),
            ));
        }
        layout
    }
}

impl Render for CommitInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let multiline = self.editor.multiline;
        let input = canvas(
            |_, _, _| (),
            move |b, _, window, cx| {
                let focus = entity.update(cx, |input, cx| {
                    input.layout = Some(window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                        input.paint(b, window, cx)
                    }));
                    input.focus.clone()
                });
                window.handle_input(&focus, ElementInputHandler::new(b, entity.clone()), cx);
            },
        )
        .flex_1()
        .min_w_0()
        .h(px(LINE_HEIGHT * if multiline { DESCRIPTION_ROWS as f32 } else { 1. }));
        div()
            .id(self.placeholder)
            .key_context("CommitField")
            .track_focus(&self.focus)
            .h(px(if multiline { LINE_HEIGHT * DESCRIPTION_ROWS as f32 + 8. } else { 30. }))
            .flex()
            .items_center()
            .px(px(9.))
            .bg(rgb(CANVAS))
            .border_1()
            .border_color(rgb(LINE_STRONG))
            .rounded(px(RADIUS))
            .text_size(px(12.5))
            .focus(|s| s.border_color(rgb(ACCENT)))
            .cursor(CursorStyle::IBeam)
            .overflow_hidden()
            .on_key_down(cx.listener(Self::key))
            .when(multiline, |d| {
                d.on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                    let Some(layout) = &this.layout else { return };
                    let scroll = layout.lines.len() as f32 * LINE_HEIGHT;
                    if px(scroll) > layout.bounds.size.height {
                        this.scroll_top -= event.delta.pixel_delta(px(LINE_HEIGHT)).y;
                        this.reveal_caret = false;
                        cx.notify();
                        cx.stop_propagation();
                    }
                }))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::press_right))
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| {
                if let Some((origin, unit)) = &this.drag {
                    let (anchor, caret) = this.editor.dragged(origin, *unit, this.hit_for(ev.position, *unit));
                    this.editor.select(anchor, caret);
                    this.reveal_caret = true;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag = None),
            )
            .on_action(cx.listener(|this, _: &PasteSummary, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &SelectSummary, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &CopySummary, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &CutSummary, _, cx| this.cut(cx)))
            .on_action(cx.listener(|this, _: &UndoText, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &RedoText, _, cx| this.redo(cx)))
            .child(input)
            .when(self.count && !self.editor.value.is_empty(), |d| {
                d.gap(px(6.)).child(crate::kit::label(self.editor.value.graphemes(true).count().to_string()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn enabled(items: Vec<MenuItem>) -> Vec<(&'static str, bool)> {
        items.into_iter().map(|item| (item.label, item.enabled)).collect()
    }

    #[test]
    fn the_field_menu_offers_what_the_selection_and_clipboard_allow() {
        assert_eq!(
            enabled(menu_items(true, false, true, true)),
            [("Cut", true), ("Copy", true), ("Paste", true), ("Delete", true), ("Select All", true)]
        );
        assert_eq!(
            enabled(menu_items(false, false, true, false)),
            [("Cut", false), ("Copy", false), ("Paste", false), ("Delete", false), ("Select All", true)]
        );
        assert_eq!(
            enabled(menu_items(true, true, true, true)),
            [("Cut", false), ("Copy", false), ("Paste", true), ("Delete", true), ("Select All", true)]
        );
        assert_eq!(
            enabled(menu_items(false, false, false, true)),
            [("Cut", false), ("Copy", false), ("Paste", true), ("Delete", false), ("Select All", false)]
        );
        let keys: Vec<_> = menu_items(true, false, true, true).into_iter().map(|item| item.keys).collect();
        assert_eq!(keys, [Some("cmd-x"), Some("cmd-c"), Some("cmd-v"), None, Some("cmd-a")]);
    }

    #[test]
    fn every_listed_text_key_does_something_in_a_field() {
        for (keys, title) in TEXT_KEYS {
            let stroke = Keystroke::parse(keys).unwrap();
            let bound = matches!(*keys, "cmd-z" | "cmd-shift-z");
            assert!(bound || editing::command(&stroke).is_some(), "{keys} ({title})");
        }
    }
}
