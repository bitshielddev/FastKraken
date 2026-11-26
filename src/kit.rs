use crate::{
    theme::*,
    view::{Action, ActivateControl, Desk},
};
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, Bounds, Canvas, Context, Deferred, Display,
    Div, Element, ElementId, FocusHandle, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, Pixels, Position, Render, ShapedLine,
    SharedString, Stateful, Style, Svg, TextRun, Transformation, Window, canvas, deferred, div,
    percentage, point, prelude::*, px, rgb, rgba, svg,
};
use std::{cell::Cell, rc::Rc, time::Duration};

pub use crate::scrollbar::scrollbar;

pub fn clear() -> gpui::Rgba {
    rgba(0x00000000)
}


pub fn icon(name: &str) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(14.))
        .flex_shrink_0()
        .text_color(rgb(TEXT_2))
}

pub fn spinner(id: impl Into<ElementId>) -> impl IntoElement {
    icon("spinner").text_color(rgb(ACCENT)).with_animation(
        id,
        Animation::new(Duration::from_millis(900)).repeat(),
        |icon, delta| icon.with_transformation(Transformation::rotate(percentage(delta))),
    )
}


#[derive(Clone, Copy, PartialEq)]
pub enum Cut {
    End,
    Start,
    Middle,
}

pub struct Fit {
    text: SharedString,
    size: f32,
    color: Hsla,
    weight: FontWeight,
    family: Option<SharedString>,
    cut: Cut,
    grow: bool,
    clipped: Option<Clipped>,
    cell: Option<(f32, f32)>,
}

#[derive(Clone, Default)]
pub struct Clipped(Rc<Cell<bool>>);

impl Clipped {
    pub fn get(&self) -> bool {
        self.0.get()
    }
}

pub fn fit(text: impl Into<SharedString>) -> Fit {
    Fit {
        text: text.into(),
        size: TEXT_BASE,
        color: rgb(TEXT).into(),
        weight: FontWeight::NORMAL,
        family: None,
        cut: Cut::End,
        grow: false,
        clipped: None,
        cell: None,
    }
}

impl Fit {
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    pub fn color(mut self, token: u32) -> Self {
        self.color = rgb(token).into();
        self
    }
    pub fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }
    pub fn mono(mut self) -> Self {
        self.family = Some(MONO.into());
        self
    }
    pub fn family(mut self, family: SharedString) -> Self {
        self.family = Some(family);
        self
    }
    pub fn cut(mut self, cut: Cut) -> Self {
        self.cut = cut;
        self
    }
    pub fn grow(mut self) -> Self {
        self.grow = true;
        self
    }
    pub fn clipped(mut self, clipped: &Clipped) -> Self {
        self.clipped = Some(clipped.clone());
        self
    }

    pub fn cell(mut self, width: f32, trailing: f32) -> Self {
        self.cell = Some((width, trailing));
        self
    }

    fn run(&self, len: usize, window: &Window) -> TextRun {
        let mut font = window.text_style().font();
        font.weight = self.weight;
        if let Some(family) = &self.family {
            font.family = family.clone();
        }
        TextRun {
            len,
            font,
            color: self.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    fn shape(&self, text: SharedString, window: &Window) -> ShapedLine {
        let run = self.run(text.len(), window);
        window
            .text_system()
            .shape_line(text, px(self.size), &[run], None)
    }

    fn shortened(&self, whole: &ShapedLine, width: Pixels, window: &Window) -> ShapedLine {
        let text = self.text.as_ref();
        let room = (width - self.shape("…".into(), window).width).max(px(0.));
        let cut = match self.cut {
            Cut::End => {
                let end = text.floor_char_boundary(whole.closest_index_for_x(room));
                format!("{}…", text[..end].trim_end())
            }
            Cut::Start => {
                let start = text.ceil_char_boundary(whole.closest_index_for_x(whole.width - room));
                format!("…{}", text[start..].trim_start())
            }
            Cut::Middle => {
                let tail_start = text
                    .char_indices()
                    .rev()
                    .nth(11)
                    .map_or(0, |(index, _)| index);
                let tail = &text[tail_start..];
                let tail_width = whole.width - whole.x_for_index(tail_start);
                if tail_start == 0 || tail_width >= room {
                    let end = text.floor_char_boundary(whole.closest_index_for_x(room));
                    format!("{}…", text[..end].trim_end())
                } else {
                    let end =
                        text.floor_char_boundary(whole.closest_index_for_x(room - tail_width));
                    format!("{}…{tail}", &text[..end.min(tail_start)])
                }
            }
        };
        self.shape(cut.into(), window)
    }
}

impl IntoElement for Fit {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Fit {
    type RequestLayoutState = ShapedLine;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ShapedLine) {
        let line = self.shape(self.text.clone(), window);
        let mut style = Style::default();
        style.flex_shrink = 1.;
        style.min_size.width = px(0.).into();
        style.size.height = px((self.size * 1.4).round()).into();
        if let Some((width, trailing)) = self.cell {
            style.flex_shrink = 0.;
            style.size.width = px((width - trailing).max(0.)).into();
            style.margin.right = px(trailing).into();
        } else if self.grow {
            style.flex_grow = 1.;
            style.flex_basis = px(0.).into();
        } else {
            style.size.width = line.width.ceil().into();
        }
        (window.request_layout(style, [], cx), line)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut ShapedLine,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        line: &mut ShapedLine,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let whole = line.width <= bounds.size.width + px(0.5);
        if let Some(clipped) = &self.clipped {
            clipped.0.set(!whole);
        }
        if bounds.size.width <= px(1.) {
            return;
        }
        let _ = if whole {
            line.paint(bounds.origin, bounds.size.height, window, cx)
        } else {
            self.shortened(line, bounds.size.width, window).paint(
                bounds.origin,
                bounds.size.height,
                window,
                cx,
            )
        };
    }
}


#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Normal,
    Primary,
    Ghost,
    Danger,
    Destructive,
}

pub struct Btn {
    id: ElementId,
    label: SharedString,
    icon: Option<&'static str>,
    trailing: Option<&'static str>,
    tone: Tone,
    small: bool,
    wide: bool,
    enabled: bool,
    selected: bool,
    count: Option<SharedString>,
    hint: Option<&'static str>,
    tip: Option<SharedString>,
    warn: bool,
}

pub fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Btn {
    Btn {
        id: id.into(),
        label: label.into(),
        icon: None,
        trailing: None,
        tone: Tone::Normal,
        small: false,
        wide: false,
        enabled: true,
        selected: false,
        count: None,
        hint: None,
        tip: None,
        warn: false,
    }
}

impl Btn {
    pub fn primary(mut self) -> Self {
        self.tone = Tone::Primary;
        self
    }
    pub fn ghost(mut self) -> Self {
        self.tone = Tone::Ghost;
        self
    }
    pub fn danger(mut self) -> Self {
        self.tone = Tone::Danger;
        self
    }
    pub fn destructive(mut self) -> Self {
        self.tone = Tone::Destructive;
        self
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }
    pub fn trailing(mut self, name: &'static str) -> Self {
        self.trailing = Some(name);
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn count(mut self, count: Option<usize>) -> Self {
        self.count = count.filter(|n| *n > 0).map(|n| n.to_string().into());
        self
    }
    pub fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
    pub fn tip(mut self, tip: impl Into<SharedString>) -> Self {
        self.tip = Some(tip.into());
        self
    }
    pub fn warn(mut self, warn: bool) -> Self {
        self.warn = warn;
        self
    }

    fn base(self) -> Stateful<Div> {
        let (fg, bg, border, hover) = match self.tone {
            Tone::Primary => (ACCENT_INK, rgb(ACCENT), rgb(ACCENT), rgb(ACCENT_HOVER)),
            Tone::Normal if self.selected => (TEXT, rgb(SELECTED), rgb(LINE_STRONG), rgb(SELECTED)),
            Tone::Normal => (TEXT, rgb(RAISED), rgb(LINE_STRONG), rgb(HOVER)),
            Tone::Ghost if self.selected => (TEXT, rgb(SELECTED), clear(), rgb(SELECTED)),
            Tone::Ghost => (TEXT, clear(), clear(), rgb(HOVER)),
            Tone::Danger => (DELETED, clear(), clear(), soft(DELETED, 0x26)),
            Tone::Destructive => (CANVAS, rgb(DELETED), rgb(DELETED), soft(DELETED, 0xd0)),
        };
        let icon_color = match self.tone {
            Tone::Primary | Tone::Danger | Tone::Destructive => fg,
            _ => TEXT_2,
        };
        let enabled = self.enabled;
        let mut base = div()
            .id(self.id)
            .flex()
            .items_center()
            .flex_shrink_0()
            .gap(px(if self.small { 5. } else { 6. }))
            .px(px(if self.small { 8. } else { 10. }))
            .h(px(if self.small { CONTROL_SMALL } else { CONTROL }))
            .rounded(px(if self.small { 5. } else { RADIUS }))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(rgb(fg))
            .text_size(px(if self.small { TEXT_SMALL } else { 12.5 }))
            .font_weight(if matches!(self.tone, Tone::Primary | Tone::Destructive) {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .whitespace_nowrap()
            .when(self.wide, |d| d.w_full().justify_center())
            .when(!enabled, |d| d.opacity(0.42))
            .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover)));
        if let Some(name) = self.icon {
            let size = if self.small { 12. } else { 14. };
            base = base.child(icon(name).size(px(size)).text_color(rgb(icon_color)));
        }
        if !self.label.is_empty() && self.wide {
            let weight = if matches!(self.tone, Tone::Primary | Tone::Destructive) {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            };
            let size = if self.small { TEXT_SMALL } else { 12.5 };
            base = base.child(fit(self.label).size(size).color(fg).weight(weight));
        } else if !self.label.is_empty() {
            base = base.child(self.label);
        }
        if let Some(count) = self.count {
            base = base.child(
                div()
                    .text_size(px(TEXT_TINY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(ACCENT))
                    .child(count),
            );
        }
        if self.warn {
            base = base.child(icon("alert").size(px(12.)).text_color(rgb(CONFLICT)));
        }
        if let Some(name) = self.trailing {
            base = base.child(icon(name).size(px(11.)).text_color(rgb(TEXT_3)));
        }
        if let Some(hint) = self.hint {
            let ink = if self.tone == Tone::Primary {
                soft(ACCENT_INK, 0xb0)
            } else {
                rgb(TEXT_3)
            };
            base = base.child(
                div()
                    .flex_shrink_0()
                    .ml_1()
                    .px_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(ink)
                    .text_size(px(10.5))
                    .text_color(ink)
                    .child(hint),
            );
        }
        if let Some(text) = self.tip {
            base = base.tooltip(tip(text));
        }
        base
    }

    pub fn on<T: Desk>(self, action: Action, cx: &Context<T>) -> Stateful<Div> {
        let enabled = self.enabled;
        let base = self.base();
        if !enabled {
            return base;
        }
        pressable(base.focusable().focus(|s| s.border_color(rgb(ACCENT))), action, cx)
    }

    pub fn tracked<T: Desk>(
        self,
        handle: &FocusHandle,
        action: Action,
        cx: &Context<T>,
    ) -> Stateful<Div> {
        let enabled = self.enabled;
        let base = self.base();
        if !enabled {
            return base;
        }
        let activate = action.clone();
        let base = base.track_focus(handle).focus(|s| s.border_color(rgb(ACCENT)));
        base.key_context("DeskButton")
            .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                if !event.is_keyboard() {
                    app.dispatch(action.clone(), w, cx)
                }
            }))
            .on_action(cx.listener(move |app, _: &ActivateControl, w, cx| {
                app.dispatch(activate.clone(), w, cx)
            }))
    }
}

pub fn pressable<T: Desk>(control: Stateful<Div>, action: Action, cx: &Context<T>) -> Stateful<Div> {
    let activate = action.clone();
    control
        .key_context("DeskButton")
        .tab_stop(true)
        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
            if !event.is_keyboard() {
                app.dispatch(action.clone(), w, cx)
            }
        }))
        .on_action(cx.listener(move |app, _: &ActivateControl, w, cx| {
            app.dispatch(activate.clone(), w, cx)
        }))
}

pub fn icon_btn<T: Desk>(
    id: impl Into<ElementId>,
    name: &'static str,
    label: impl Into<SharedString>,
    action: Action,
    cx: &Context<T>,
) -> Stateful<Div> {
    let activate = action.clone();
    div()
        .id(id)
        .size(px(22.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .border_color(clear())
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)))
        .focusable()
        .focus(|s| s.border_color(rgb(ACCENT)))
        .key_context("DeskButton")
        .tab_stop(true)
        .child(icon(name))
        .tooltip(tip(label))
        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
            cx.stop_propagation();
            if !event.is_keyboard() {
                app.dispatch(action.clone(), w, cx)
            }
        }))
        .on_action(cx.listener(move |app, _: &ActivateControl, w, cx| {
            app.dispatch(activate.clone(), w, cx)
        }))
}


struct Tip(SharedString, Option<Clipped>);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.1.as_ref().is_some_and(|clipped| !clipped.get()) {
            return div();
        }
        div()
            .px_2()
            .py_1()
            .max_w(px(320.))
            .rounded(px(RADIUS))
            .border_1()
            .border_color(rgb(LINE_STRONG))
            .bg(rgb(TIP))
            .shadow_md()
            .text_size(px(TEXT_SMALL))
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone(), None)).into()
}

pub fn tip_with<S: Into<SharedString>>(text: impl Fn() -> S + 'static) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        let text = text().into();
        cx.new(|_| Tip(text, None)).into()
    }
}

pub fn tip_when_clipped(
    text: impl Into<SharedString>,
    clipped: &Clipped,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, clipped) = (text.into(), clipped.clone());
    move |_, cx| cx.new(|_| Tip(text.clone(), Some(clipped.clone()))).into()
}


pub fn row(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .flex()
        .flex_shrink_0()
        .items_center()
        .border_l_2()
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(rgb(SELECTED)).border_color(rgb(ACCENT))
        })
        .when(!selected, |d| {
            d.border_color(clear()).hover(|s| s.bg(rgb(HOVER)))
        })
}

pub fn cursor(row: Stateful<Div>, at: bool) -> Stateful<Div> {
    row.when(at, |d| d.bg(rgb(HOVER)).border_color(rgb(LINE_STRONG)))
}

pub fn label(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .text_size(px(TEXT_TINY))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT_3))
        .whitespace_nowrap()
        .child(text.into())
}

pub fn muted(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(TEXT_3))
        .child(text.into())
}

pub fn grow() -> Div {
    div().flex_1().min_w_0()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Chip {
    Head,
    Local,
    Remote,
    Tag,
    Stash,
    Plain,
}

pub fn chip(kind: Chip, text: impl Into<SharedString>) -> Div {
    chip_marked(kind, text, &[])
}

pub fn chip_marked(kind: Chip, text: impl Into<SharedString>, marks: &[&'static str]) -> Div {
    let (fg, bg, lead) = match kind {
        Chip::Head => (ACCENT_INK, rgb(ACCENT), Some("check")),
        Chip::Local => (ACCENT, soft(ACCENT, 0x24), None),
        Chip::Remote => (REMOTE, soft(REMOTE, 0x26), Some("cloud")),
        Chip::Tag => (TAG, soft(TAG, 0x26), Some("tag")),
        Chip::Stash => (TEXT_2, clear(), Some("stash")),
        Chip::Plain => (TEXT_2, rgb(RAISED), None),
    };
    div()
        .h(px(18.))
        .px(px(6.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(4.))
        .max_w(px(190.))
        .rounded(px(RADIUS_CHIP))
        .bg(bg)
        .when(kind == Chip::Stash, |d| {
            d.border_1().border_dashed().border_color(rgb(LINE_STRONG))
        })
        .when_some(lead, |d, name| {
            d.child(icon(name).size(px(10.)).text_color(rgb(fg)))
        })
        .child(
            fit(text)
                .size(TEXT_TINY)
                .color(fg)
                .weight(if kind == Chip::Head {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                }),
        )
        .children(marks.iter().map(|name| icon(*name).flex_shrink_0().size(px(10.)).text_color(rgb(fg))))
}

pub fn badge(status: &str) -> Div {
    let (letter, color) = match status {
        "Added" => ("A", ADDED),
        "Untracked" => ("U", ADDED),
        "Deleted" => ("D", DELETED),
        "Conflicted" => ("!", CONFLICT),
        "Type changed" => ("T", REMOTE),
        "Renamed" => ("R", TAG),
        "Copied" => ("C", TAG),
        _ => ("M", MODIFIED),
    };
    div()
        .size(px(16.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_CHIP))
        .bg(soft(color, 0x28))
        .text_size(px(10.5))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(color))
        .child(letter)
}

pub fn file_label(
    id: impl Into<ElementId>,
    status: &str,
    path: &std::path::Path,
    from: Option<&std::path::Path>,
) -> Stateful<Div> {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    let folder = path.parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default();
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(badge(status))
        .child(fit(name).size(12.5).cut(Cut::Middle))
        .child(fit(folder).size(11.5).color(TEXT_3).cut(Cut::Start).grow())
        .when_some(from, |d, from| d.tooltip(tip(moved_from(status, from))))
}

pub fn moved_from(status: &str, from: &std::path::Path) -> String {
    let verb = if status == "Copied" { "Copied" } else { "Renamed" };
    format!("{verb} from {}", from.display())
}

pub struct Avatar {
    initials: SharedString,
    color: u32,
    size: Pixels,
    text: Pixels,
}

impl Avatar {
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn text_size(mut self, text: Pixels) -> Self {
        self.text = text;
        self
    }
}

pub fn avatar(name: &str) -> Avatar {
    const COLORS: [u32; 6] = [0x7fb3e6, 0xd9a55a, 0xa898f0, 0x8fd0a0, 0xe88fb0, 0x6fc9c9];
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    Avatar { initials: initials.into(), color: COLORS[hash as usize % COLORS.len()], size: px(16.), text: px(8.) }
}

impl IntoElement for Avatar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Avatar {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.flex_shrink = 0.;
        style.size.width = self.size.into();
        style.size.height = self.size.into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(gpui::fill(bounds, rgb(self.color)).corner_radii(self.size / 2.));
        if self.initials.is_empty() {
            return;
        }
        let mut font = window.text_style().font();
        font.weight = FontWeight::BOLD;
        let run = TextRun {
            len: self.initials.len(),
            font,
            color: rgb(CANVAS).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let size = self.text;
        let line = window.text_system().shape_line(self.initials.clone(), size, &[run], None);
        let height = (size * 1.4).round();
        let origin = point(
            bounds.origin.x + (bounds.size.width - line.width) / 2.,
            bounds.origin.y + (bounds.size.height - height) / 2.,
        );
        let _ = line.paint(origin, height, window, cx);
    }
}

pub fn checkbox(checked: bool) -> Div {
    div()
        .size(px(14.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(3.5))
        .border_1()
        .when(checked, |d| {
            d.bg(rgb(ACCENT))
                .border_color(rgb(ACCENT))
                .child(icon("check").size(px(10.)).text_color(rgb(ACCENT_INK)))
        })
        .when(!checked, |d| {
            d.bg(rgb(CANVAS)).border_color(rgb(LINE_STRONG))
        })
}

pub fn field(name: &'static str, control: impl IntoElement, help: Option<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .min_w_0()
        .child(label(name))
        .child(control)
        .when_some(help, |d, help| d.child(muted(help).text_size(px(11.5))))
}


pub fn float() -> Div {
    div()
        .occlude()
        .bg(rgb(FLOAT))
        .border_1()
        .border_color(rgb(LINE_STRONG))
        .rounded(px(RADIUS_FLOAT))
        .shadow_lg()
        .overflow_hidden()
}

pub fn popover(surface: impl IntoElement, right_aligned: bool) -> Deferred {
    let anchor = div().absolute().top(px(CONTROL + 6.));
    deferred(if right_aligned {
        anchor.right_0().child(surface)
    } else {
        anchor.left_0().child(surface)
    })
    .with_priority(1)
}

#[derive(Default)]
pub struct Over(Vec<AnyElement>);

type Spot = Rc<Cell<Option<Bounds<Pixels>>>>;

pub fn popover_over_modal<T: Desk>(
    surface: Div,
    right_aligned: bool,
    close: Action,
    over: &mut Over,
    cx: &Context<T>,
) -> Canvas<()> {
    let spot = Spot::default();
    let trigger = spot.clone();
    let surface = surface.on_mouse_down_out(cx.listener(move |app, event: &MouseDownEvent, w, cx| {
        if !trigger.get().is_some_and(|bounds| bounds.contains(&event.position)) {
            app.dispatch(close.clone(), w, cx)
        }
    }));
    let lifted = Lifted {
        spot: spot.clone(),
        right_aligned,
        surface: any(surface),
    };
    over.0.push(any(deferred(lifted).with_priority(3)));
    canvas(move |bounds, _, _| spot.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .inset_0()
}

struct Lifted {
    spot: Spot,
    right_aligned: bool,
    surface: AnyElement,
}

impl IntoElement for Lifted {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Lifted {
    type RequestLayoutState = LayoutId;
    type PrepaintState = bool;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, LayoutId) {
        let surface = self.surface.request_layout(window, cx);
        let mut style = Style::default();
        style.position = Position::Absolute;
        style.display = Display::Flex;
        (window.request_layout(style, [surface], cx), surface)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        surface: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(trigger) = self.spot.get() else {
            return false;
        };
        let size = window.layout_bounds(*surface).size;
        let room = window.viewport_size();
        let margin = px(8.);
        let x = if self.right_aligned {
            trigger.right() - size.width
        } else {
            trigger.left()
        };
        let x = x.min(room.width - size.width - margin).max(margin);
        let y = (trigger.top() + px(CONTROL + 8.))
            .min(room.height - size.height - margin)
            .max(margin);
        let offset = point(x, y) - bounds.origin;
        window.with_element_offset(point(offset.x.round(), offset.y.round()), |window| {
            self.surface.prepaint(window, cx)
        });
        true
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        drawn: &mut bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *drawn {
            self.surface.paint(window, cx);
        }
    }
}

pub fn segmented() -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(CANVAS))
}

pub fn segment<T: Desk>(
    id: impl Into<ElementId>,
    label: &'static str,
    lead: Option<&'static str>,
    chosen: bool,
    action: Action,
    cx: &Context<T>,
) -> Stateful<Div> {
    let segment = div()
        .id(id)
        .h(px(24.))
        .px(px(11.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(rgb(if chosen { TEXT } else { TEXT_3 }))
        .border_1()
        .border_color(clear())
        .when(chosen, |d| d.bg(rgb(RAISED)))
        .when(!chosen, |d| d.hover(|s| s.text_color(rgb(TEXT_2))))
        .when_some(lead, |d, name| {
            d.child(
                icon(name)
                    .size(px(12.))
                    .text_color(rgb(if chosen { TEXT_2 } else { TEXT_3 })),
            )
        })
        .child(label);
    pressable(segment.focusable().focus(|s| s.border_color(rgb(ACCENT))), action, cx)
}

pub fn stepper<T: Desk>(
    id: &'static str,
    value: impl Into<SharedString>,
    less: Option<Action>,
    more: Option<Action>,
    cx: &Context<T>,
) -> Div {
    let side = |index: usize, name: &'static str, label: &'static str, action: Option<Action>| match action {
        Some(action) => any(icon_btn((id, index), name, label, action, cx)),
        None => any(
            div()
                .size(px(22.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .opacity(0.35)
                .child(icon(name)),
        ),
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .p(px(2.))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(CANVAS))
        .child(side(0, "minus", "Less", less))
        .child(
            div()
                .min_w(px(64.))
                .flex()
                .justify_center()
                .text_size(px(12.5))
                .text_color(rgb(TEXT))
                .child(value.into()),
        )
        .child(side(1, "plus", "More", more))
}

pub fn link<T: Desk>(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    action: Action,
    cx: &Context<T>,
) -> Stateful<Div> {
    let link = div()
        .id(id)
        .flex_shrink_0()
        .cursor_pointer()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(ACCENT))
        .hover(|s| s.text_color(rgb(ACCENT_HOVER)))
        .focusable()
        .focus(|s| s.underline())
        .child(label.into());
    pressable(link, action, cx)
}

pub fn separator() -> Div {
    div().h(px(1.)).my_1().bg(rgb(LINE))
}

pub struct Item {
    id: ElementId,
    label: SharedString,
    icon: Option<&'static str>,
    check: Option<bool>,
    meta: Option<SharedString>,
    danger: bool,
    enabled: bool,
    tip: Option<SharedString>,
    highlighted: bool,
    chevron: bool,
    ink: Option<u32>,
}

pub fn item(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Item {
    Item {
        id: id.into(),
        label: label.into(),
        icon: None,
        check: None,
        meta: None,
        danger: false,
        enabled: true,
        tip: None,
        highlighted: false,
        chevron: false,
        ink: None,
    }
}

impl Item {
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }
    pub fn check(mut self, checked: bool) -> Self {
        self.check = Some(checked);
        self
    }
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tip(mut self, tip: Option<SharedString>) -> Self {
        self.tip = tip;
        self
    }

    pub fn highlighted(mut self, highlighted: bool) -> Self {
        self.highlighted = highlighted;
        self
    }

    pub fn chevron(mut self) -> Self {
        self.chevron = true;
        self
    }

    pub fn ink(mut self, colour: u32) -> Self {
        self.ink = Some(colour);
        self
    }

    pub fn inert(self) -> Stateful<Div> {
        let enabled = self.enabled && self.ink.is_none();
        let ink = self.ink.unwrap_or(if self.danger { DELETED } else { TEXT });
        let lead = match (self.check, self.icon) {
            (Some(true), _) => Some(icon("check").text_color(rgb(ACCENT))),
            (None, Some(name)) if self.ink.is_some() => Some(icon(name).text_color(rgb(ink))),
            (None, Some(name)) => Some(icon(name)),
            _ => None,
        };
        let dimmed = !self.enabled;
        let inner = div()
            .h(px(ROW))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(5.))
            .text_size(px(12.5))
            .text_color(rgb(ink))
            .when(dimmed, |d| d.opacity(0.42))
            .when(self.highlighted, |d| d.bg(rgb(SELECTED)))
            .when(enabled, |d| {
                d.group_hover("menu-item", |s| s.bg(rgb(SELECTED)))
            })
            .when(self.check.is_some() || self.icon.is_some(), |d| {
                d.child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .children(lead),
                )
            })
            .child(fit(self.label).size(12.5).color(ink))
            .child(grow())
            .when_some(self.meta, |d, meta| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(TEXT_TINY))
                        .text_color(rgb(TEXT_3))
                        .child(meta),
                )
            })
            .when(self.chevron, |d| {
                d.child(icon("chevron-right").size(px(11.)).text_color(rgb(TEXT_3)))
            });
        div()
            .id(self.id)
            .group("menu-item")
            .w_full()
            .px_1()
            .flex_shrink_0()
            .when(enabled, |d| d.cursor_pointer())
            .when_some(self.tip, |d, text| d.tooltip(tip(text)))
            .child(inner)
    }

    pub fn on<T: Desk>(self, action: Action, cx: &Context<T>) -> Stateful<Div> {
        let enabled = self.enabled;
        let base = self.inert();
        if !enabled {
            return base;
        }
        base.on_click(cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx)))
    }
}

pub fn modal<T: Desk>(
    surface: impl IntoElement,
    dismiss: Option<Action>,
    focus: &FocusHandle,
    cx: &Context<T>,
) -> Deferred {
    deferred(
        div()
            .id("modal-scrim")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(scrim())
            .when_some(dismiss, |d, action| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx)),
                )
            })
            .child(
                div()
                    .track_focus(focus)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(surface),
            ),
    )
    .with_priority(2)
}

pub fn modal_over<T: Desk>(
    surface: impl IntoElement,
    over: Over,
    dismiss: Option<Action>,
    focus: &FocusHandle,
    cx: &Context<T>,
) -> Vec<AnyElement> {
    std::iter::once(any(modal(surface, dismiss, focus, cx)))
        .chain(over.0)
        .collect()
}

pub fn dialog(title: impl Into<SharedString>, width: f32) -> Div {
    float()
        .w(px(width))
        .rounded(px(12.))
        .p(px(18.))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(title.into()),
        )
}

pub fn any(element: impl IntoElement) -> AnyElement {
    element.into_any_element()
}
