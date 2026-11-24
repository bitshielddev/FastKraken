use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    FocusHandle, Focusable, KeyBinding, KeyDownEvent, MouseButton, Pixels, Point, ShapedLine,
    ScrollWheelEvent, TextRun, UTF16Selection, Window, actions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use crate::theme::*;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    commit_field,
    [PasteSummary, CopySummary, CutSummary, SelectSummary]
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
    ]);
}

const LINE_HEIGHT: f32 = 20.;
const BULLET: &str = "•";
const DESCRIPTION_ROWS: usize = 3;

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
}

pub struct CommitInput {
    focus: FocusHandle,
    placeholder: &'static str,
    value: String,
    anchor: usize,
    caret: usize,
    marked: Option<Range<usize>>,
    layout: Option<InputLayout>,
    multiline: bool,
    count: bool,
    masked: bool,
    scroll_top: Pixels,
    reveal_caret: bool,
    dragging: bool,
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
        input.multiline = true;
        input
    }

    pub fn with_placeholder(cx: &mut Context<Self>, placeholder: &'static str) -> Self {
        Self {
            placeholder,
            focus: cx.focus_handle().tab_stop(true),
            value: String::new(),
            anchor: 0,
            caret: 0,
            marked: None,
            layout: None,
            dragging: false,
            multiline: false,
            count: false,
            masked: false,
            scroll_top: px(0.),
            reveal_caret: true,
        }
    }

    pub fn secret(cx: &mut Context<Self>, placeholder: &'static str) -> Self {
        let mut input = Self::with_placeholder(cx, placeholder);
        input.masked = true;
        input
    }

    pub fn text(&self) -> &str {
        &self.value
    }

    pub fn set_placeholder(&mut self, placeholder: &'static str, cx: &mut Context<Self>) {
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    fn display(&self) -> String {
        if self.masked {
            BULLET.repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }

    fn to_display(&self, index: usize) -> usize {
        if self.masked {
            self.value[..index].chars().count() * BULLET.len()
        } else {
            index
        }
    }

    fn from_display(&self, index: usize) -> usize {
        if self.masked {
            self.value.char_indices().nth(index / BULLET.len()).map_or(self.value.len(), |(at, _)| at)
        } else {
            index
        }
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace(0..self.value.len(), text, cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.value.clear();
        self.anchor = 0;
        self.caret = 0;
        self.marked = None;
        self.layout = None;
        self.reveal_caret = true;
        cx.notify();
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn normalized(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text = self.normalized(text);
        self.value.replace_range(range.clone(), &text);
        self.caret = range.start + text.len();
        self.anchor = self.caret;
        self.marked = None;
        self.layout = None;
        self.reveal_caret = true;
        cx.notify();
    }

    fn hit(&self, point: Point<Pixels>) -> usize {
        self.layout.as_ref().map_or(self.value.len(), |layout| self.from_display(layout.hit(point)))
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let s = self.selection();
        if !s.is_empty() && !self.masked {
            cx.write_to_clipboard(ClipboardItem::new_string(self.value[s].to_owned()));
        }
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if self.marked.is_some() || k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
            return;
        }
        let selected = self.selection();
        let prev = || {
            self.value[..self.caret]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |p| p.0)
        };
        let next = || {
            self.caret
                + self.value[self.caret..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
        };
        match k.key.as_str() {
            "enter" if self.multiline => self.replace(selected, "\n", cx),
            "up" | "down" if self.multiline => {
                if let Some(layout) = &self.layout {
                    let row = layout.row(self.caret);
                    let target = if k.key == "up" { row.checked_sub(1) } else { Some(row + 1) };
                    self.caret = match target.and_then(|i| layout.lines.get(i).map(|_| i)) {
                        Some(i) => layout.hit(point(layout.point(self.caret).x, layout.origin.y + px(LINE_HEIGHT * (i as f32 + 0.5)))),
                        None if k.key == "up" => 0,
                        None => self.value.len(),
                    };
                    if !k.modifiers.shift {
                        self.anchor = self.caret;
                    }
                    cx.notify();
                }
            }
            "left" | "right" | "home" | "end" => {
                self.caret = match k.key.as_str() {
                    "home" => self.value[..self.caret].rfind('\n').map_or(0, |i| i + 1),
                    "end" => self.value[self.caret..]
                        .find('\n')
                        .map_or(self.value.len(), |i| self.caret + i),
                    "left" if !k.modifiers.shift && !selected.is_empty() => selected.start,
                    "right" if !k.modifiers.shift && !selected.is_empty() => selected.end,
                    "left" => prev(),
                    _ => next(),
                };
                if !k.modifiers.shift {
                    self.anchor = self.caret;
                }
                cx.notify();
            }
            "backspace" | "delete" => {
                let range = if !selected.is_empty() {
                    selected
                } else if k.key == "backspace" {
                    prev()..self.caret
                } else {
                    self.caret..next()
                };
                self.replace(range, "", cx);
            }
            _ => return,
        }
        self.reveal_caret = true;
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
        self.value[..bytes.start].encode_utf16().count()
            ..self.value[..bytes.end].encode_utf16().count()
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
        let r = Self::to_bytes(&self.value, r);
        *actual = Some(self.to_utf16(r.clone()));
        Some(self.value[r].to_owned())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(self.selection()),
            reversed: self.caret < self.anchor,
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
        let r = range
            .map(|v| Self::to_bytes(&self.value, v))
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        self.replace(r, text, cx);
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
        let start = self.caret - self.normalized(text).len();
        self.marked = (!text.is_empty()).then_some(start..self.caret);
        if let Some(r) = selected {
            let bytes = Self::to_bytes(&self.value[start..self.caret], r);
            self.anchor = start + bytes.start;
            self.caret = start + bytes.end;
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
        let range = Self::to_bytes(&self.value, range);
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
            .then(|| self.value[..self.from_display(layout.hit(p))].encode_utf16().count())
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
            let wrapped = self.multiline.then(|| {
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
        let placeholder = self.value.is_empty().then(|| shape(self.placeholder, TEXT_3));
        let mut layout = InputLayout { lines, bounds, origin: bounds.origin };
        let caret = layout.point(self.to_display(self.caret));
        if self.multiline {
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
            let caret = layout.point(self.to_display(self.caret));
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
        .h(px(LINE_HEIGHT * if self.multiline { DESCRIPTION_ROWS as f32 } else { 1. }));
        div()
            .id(self.placeholder)
            .key_context("CommitField")
            .track_focus(&self.focus)
            .h(px(if self.multiline { LINE_HEIGHT * DESCRIPTION_ROWS as f32 + 8. } else { 30. }))
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
            .when(self.multiline, |d| {
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, w, cx| {
                    w.focus(&this.focus);
                    this.caret = this.hit(ev.position);
                    this.reveal_caret = true;
                    if !ev.modifiers.shift {
                        this.anchor = this.caret;
                    }
                    this.dragging = true;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _, cx| {
                if this.dragging {
                    this.caret = this.hit(ev.position);
                    this.reveal_caret = true;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = false),
            )
            .on_action(cx.listener(|this, _: &PasteSummary, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|v| v.text()) {
                    this.replace(this.selection(), &text, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SelectSummary, _, cx| {
                this.anchor = 0;
                this.caret = this.value.len();
                this.reveal_caret = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CopySummary, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &CutSummary, _, cx| {
                if !this.masked {
                    this.copy(cx);
                    this.replace(this.selection(), "", cx);
                }
            }))
            .child(input)
            .when(self.count && !self.value.is_empty(), |d| {
                d.gap(px(6.)).child(crate::kit::label(self.value.graphemes(true).count().to_string()))
            })
    }
}
