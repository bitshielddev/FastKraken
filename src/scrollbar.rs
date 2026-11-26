use crate::theme::*;
use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Position, ScrollHandle, Style, Task, UniformListScrollHandle, Window,
    fill, point, px, rgb, size,
};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

pub const STRIP: f32 = 10.;
pub const THICKNESS: f32 = 6.;
pub const INSET: f32 = 2.;
pub const SHORTEST: f32 = 24.;
pub const MARK: f32 = 2.;
pub const LINGER: Duration = Duration::from_millis(900);
pub const FADE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    pub viewport: f32,
    pub content: f32,
    pub offset: f32,
}

impl Extent {
    pub fn of(handle: &ScrollHandle, axis: Axis) -> Self {
        let bounds = handle.bounds();
        let most = handle.max_offset();
        let offset = handle.offset();
        let (viewport, travel, offset) = match axis {
            Axis::Vertical => (bounds.size.height, most.height, offset.y),
            Axis::Horizontal => (bounds.size.width, most.width, offset.x),
        };
        let viewport = f32::from(viewport);
        Self { viewport, content: viewport + f32::from(travel), offset: -f32::from(offset) }
    }

    pub fn travel(&self) -> f32 {
        (self.content - self.viewport).max(0.)
    }

    pub fn scrollable(&self) -> bool {
        self.viewport > 0. && self.travel() >= 1.
    }

    pub fn clamp(&self, offset: f32) -> f32 {
        offset.clamp(0., self.travel())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub length: f32,
}

impl Thumb {
    pub fn end(&self) -> f32 {
        self.start + self.length
    }
}

pub fn thumb(extent: Extent, track: f32) -> Option<Thumb> {
    if !extent.scrollable() || track <= 0. {
        return None;
    }
    let length = (track * extent.viewport / extent.content).max(SHORTEST).min(track);
    let start = (track - length) * extent.clamp(extent.offset) / extent.travel();
    Some(Thumb { start, length })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Press {
    Grab(f32),
    PageBack,
    PageForward,
}

pub fn press(thumb: Thumb, at: f32) -> Press {
    if at < thumb.start {
        Press::PageBack
    } else if at > thumb.end() {
        Press::PageForward
    } else {
        Press::Grab(at - thumb.start)
    }
}

pub fn dragged(extent: Extent, track: f32, thumb: Thumb, grab: f32, at: f32) -> f32 {
    let room = track - thumb.length;
    if room <= 0. {
        return extent.clamp(extent.offset);
    }
    (at - grab).clamp(0., room) / room * extent.travel()
}

pub fn paged(extent: Extent, press: Press) -> f32 {
    match press {
        Press::PageBack => extent.clamp(extent.offset - extent.viewport),
        Press::PageForward => extent.clamp(extent.offset + extent.viewport),
        Press::Grab(_) => extent.clamp(extent.offset),
    }
}

pub fn opacity(always: bool, held: bool, idle: Option<Duration>) -> f32 {
    if always || held {
        return 1.;
    }
    match idle {
        Some(idle) if idle <= LINGER => 1.,
        Some(idle) if idle < LINGER + FADE => 1. - (idle - LINGER).as_secs_f32() / FADE.as_secs_f32(),
        _ => 0.,
    }
}

pub fn strip(area: Bounds<Pixels>, axis: Axis) -> Bounds<Pixels> {
    match axis {
        Axis::Vertical => Bounds {
            origin: point(area.right() - px(STRIP), area.top()),
            size: size(px(STRIP).min(area.size.width), area.size.height),
        },
        Axis::Horizontal => Bounds {
            origin: point(area.left(), area.bottom() - px(STRIP)),
            size: size(area.size.width, px(STRIP).min(area.size.height)),
        },
    }
}

fn track_length(strip: Bounds<Pixels>, axis: Axis) -> f32 {
    let length = match axis {
        Axis::Vertical => strip.size.height,
        Axis::Horizontal => strip.size.width,
    };
    (f32::from(length) - 2. * INSET).max(0.)
}

pub fn along(strip: Bounds<Pixels>, axis: Axis, position: Point<Pixels>) -> f32 {
    f32::from(match axis {
        Axis::Vertical => position.y - strip.top(),
        Axis::Horizontal => position.x - strip.left(),
    }) - INSET
}

pub fn segment(strip: Bounds<Pixels>, axis: Axis, start: f32, length: f32) -> Bounds<Pixels> {
    match axis {
        Axis::Vertical => Bounds {
            origin: point(strip.right() - px(INSET + THICKNESS), strip.top() + px(INSET + start)),
            size: size(px(THICKNESS), px(length)),
        },
        Axis::Horizontal => Bounds {
            origin: point(strip.left() + px(INSET + start), strip.bottom() - px(INSET + THICKNESS)),
            size: size(px(length), px(THICKNESS)),
        },
    }
}

pub fn mark_start(at: f32, track: f32) -> f32 {
    (at.clamp(0., 1.) * track).min((track - MARK).max(0.))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub at: f32,
    pub color: u32,
}

pub trait Scrolls {
    fn scroll_handle(&self) -> ScrollHandle;
}

impl Scrolls for ScrollHandle {
    fn scroll_handle(&self) -> ScrollHandle {
        self.clone()
    }
}

impl Scrolls for UniformListScrollHandle {
    fn scroll_handle(&self) -> ScrollHandle {
        self.0.borrow().base_handle.clone()
    }
}

fn move_to(handle: &ScrollHandle, axis: Axis, offset: f32) {
    let current = handle.offset();
    handle.set_offset(match axis {
        Axis::Vertical => point(current.x, px(-offset)),
        Axis::Horizontal => point(px(-offset), current.y),
    });
}

#[derive(Default)]
struct Memory {
    seen: Option<f32>,
    moved: Option<Instant>,
    grab: Option<f32>,
    wake: Option<(Instant, Task<()>)>,
}

pub struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
    area: Option<ScrollHandle>,
    axis: Axis,
    marks: Vec<Mark>,
}

pub fn scrollbar(id: impl Into<ElementId>, handle: &impl Scrolls) -> Scrollbar {
    Scrollbar {
        id: id.into(),
        handle: handle.scroll_handle(),
        area: None,
        axis: Axis::Vertical,
        marks: Vec::new(),
    }
}

impl Scrollbar {
    pub fn horizontal(mut self) -> Self {
        self.axis = Axis::Horizontal;
        self
    }

    pub fn within(mut self, area: &impl Scrolls) -> Self {
        self.area = Some(area.scroll_handle());
        self
    }

    pub fn marks(mut self, marks: impl IntoIterator<Item = Mark>) -> Self {
        self.marks.extend(marks);
        self
    }
}

pub struct Frame {
    hitbox: Option<Hitbox>,
    strip: Bounds<Pixels>,
    always: bool,
    memory: Rc<RefCell<Memory>>,
}

impl IntoElement for Scrollbar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        style.position = Position::Absolute;
        style.size.width = px(0.).into();
        style.size.height = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        let memory = window.with_element_state(
            id.expect("a scrollbar always has an id"),
            |memory: Option<Rc<RefCell<Memory>>>, _| {
                let memory = memory.unwrap_or_default();
                (memory.clone(), memory)
            },
        );
        let area = self.area.as_ref().unwrap_or(&self.handle).bounds();
        let strip = strip(area, self.axis);
        let extent = Extent::of(&self.handle, self.axis);
        let always = !cx.should_auto_hide_scrollbars();
        let mut state = memory.borrow_mut();
        if state.seen.is_some_and(|seen| (seen - extent.offset).abs() > 0.5) {
            state.moved = Some(Instant::now());
        }
        state.seen = Some(extent.offset);
        let hitbox = extent.scrollable().then(|| {
            let shown = always
                || state.grab.is_some()
                || strip.contains(&window.mouse_position())
                || opacity(false, false, state.moved.map(|moved| moved.elapsed())) > 0.;
            let behavior = if shown {
                HitboxBehavior::BlockMouseExceptScroll
            } else {
                HitboxBehavior::Normal
            };
            window.insert_hitbox(strip, behavior)
        });
        drop(state);
        Frame { hitbox, strip, always, memory }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(hitbox) = frame.hitbox.clone() else {
            return;
        };
        let axis = self.axis;
        let strip = frame.strip;
        let track = track_length(strip, axis);
        let extent = Extent::of(&self.handle, axis);
        for mark in &self.marks {
            let at = mark_start(mark.at, track);
            window.paint_quad(fill(segment(strip, axis, at, MARK), rgb(mark.color)));
        }
        let view = window.current_view();
        let (held, alpha, moved) = {
            let state = frame.memory.borrow();
            let held = state.grab.is_some() || hitbox.is_hovered(window);
            (held, opacity(frame.always, held, state.moved.map(|moved| moved.elapsed())), state.moved)
        };
        if alpha > 0. {
            if let Some(thumb) = thumb(extent, track) {
                if held || frame.always {
                    let lane = segment(strip, axis, 0., track);
                    window.paint_quad(
                        fill(lane, Hsla::from(rgb(palette().line)).opacity(alpha)).corner_radii(px(THICKNESS / 2.)),
                    );
                }
                let color = if held { palette().text_2 } else { palette().text_3 };
                window.paint_quad(
                    fill(segment(strip, axis, thumb.start, thumb.length), Hsla::from(rgb(color)).opacity(alpha))
                        .corner_radii(px(THICKNESS / 2.)),
                );
            }
            if let Some(moved) = moved.filter(|_| !frame.always && !held) {
                let idle = moved.elapsed();
                if idle < LINGER {
                    let mut state = frame.memory.borrow_mut();
                    if state.wake.as_ref().is_none_or(|(due, _)| *due != moved) {
                        let task = window.spawn(cx, async move |cx| {
                            cx.background_executor().timer(LINGER - idle).await;
                            let _ = cx.update(|_, cx| cx.notify(view));
                        });
                        state.wake = Some((moved, task));
                    }
                } else {
                    window.request_animation_frame();
                }
            }
        }
        let handle = self.handle.clone();
        let memory = frame.memory.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !target.is_hovered(window) {
                return;
            }
            let extent = Extent::of(&handle, axis);
            let Some(thumb) = thumb(extent, track) else {
                return;
            };
            let mut state = memory.borrow_mut();
            match press(thumb, along(strip, axis, event.position)) {
                Press::Grab(grab) => state.grab = Some(grab),
                page => move_to(&handle, axis, paged(extent, page)),
            }
            state.moved = Some(Instant::now());
            cx.stop_propagation();
            cx.notify(view);
        });
        let handle = self.handle.clone();
        let memory = frame.memory.clone();
        let target = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let mut state = memory.borrow_mut();
            let Some(grab) = state.grab else {
                if target.is_hovered(window) != held {
                    cx.notify(view);
                }
                return;
            };
            if event.pressed_button != Some(MouseButton::Left) {
                state.grab = None;
                cx.notify(view);
                return;
            }
            let extent = Extent::of(&handle, axis);
            if let Some(thumb) = thumb(extent, track) {
                move_to(&handle, axis, dragged(extent, track, thumb, grab, along(strip, axis, event.position)));
            }
            state.moved = Some(Instant::now());
            cx.stop_propagation();
            cx.notify(view);
        });
        let memory = frame.memory.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx: &mut App| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if memory.borrow_mut().grab.take().is_some() {
                cx.stop_propagation();
                cx.notify(view);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Extent {
        fn rows(count: usize, row: f32, viewport: f32, offset: f32) -> Self {
            Self { viewport, content: count as f32 * row, offset }
        }
    }

    fn area() -> Bounds<Pixels> {
        Bounds { origin: point(px(100.), px(50.)), size: size(px(300.), px(404.)) }
    }

    #[test]
    fn thumb_length_follows_the_share_of_rows_in_view() {
        let extent = Extent::rows(100, 20., 400., 0.);
        let thumb = thumb(extent, 400.).unwrap();
        assert_eq!(thumb, Thumb { start: 0., length: 80. });
    }

    #[test]
    fn thumb_position_follows_the_scroll_offset() {
        let rows = |offset| thumb(Extent::rows(100, 20., 400., offset), 400.).unwrap().start;
        assert_eq!(rows(0.), 0.);
        assert_eq!(rows(800.), 160.);
        assert_eq!(rows(1600.), 320.);
        assert_eq!(rows(5000.), 320.);
        assert_eq!(rows(-30.), 0.);
    }

    #[test]
    fn a_long_list_keeps_a_thumb_that_can_be_grabbed() {
        let extent = Extent::rows(100_000, 28., 560., 0.);
        assert_eq!(thumb(extent, 560.).unwrap().length, SHORTEST);
        let last = thumb(Extent { offset: extent.travel(), ..extent }, 560.).unwrap();
        assert_eq!(last.end(), 560.);
    }

    #[test]
    fn a_list_that_fits_has_no_thumb() {
        assert_eq!(thumb(Extent::rows(10, 20., 400., 0.), 400.), None);
        assert_eq!(thumb(Extent::rows(20, 20., 400., 0.), 400.), None);
        assert_eq!(thumb(Extent::rows(21, 20., 400., 0.), 0.), None);
        assert_eq!(thumb(Extent::rows(21, 20., 0., 0.), 400.), None);
        assert!(thumb(Extent::rows(21, 20., 400., 0.), 400.).is_some());
    }

    #[test]
    fn pressing_beside_the_thumb_pages_and_on_it_grabs() {
        let thumb = Thumb { start: 100., length: 80. };
        assert_eq!(press(thumb, 40.), Press::PageBack);
        assert_eq!(press(thumb, 100.), Press::Grab(0.));
        assert_eq!(press(thumb, 130.), Press::Grab(30.));
        assert_eq!(press(thumb, 180.), Press::Grab(80.));
        assert_eq!(press(thumb, 181.), Press::PageForward);
    }

    #[test]
    fn a_track_click_moves_one_page_and_stops_at_the_ends() {
        let extent = Extent::rows(100, 20., 400., 500.);
        assert_eq!(paged(extent, Press::PageForward), 900.);
        assert_eq!(paged(extent, Press::PageBack), 100.);
        assert_eq!(paged(Extent { offset: 1500., ..extent }, Press::PageForward), 1600.);
        assert_eq!(paged(Extent { offset: 150., ..extent }, Press::PageBack), 0.);
        assert_eq!(paged(extent, Press::Grab(10.)), 500.);
    }

    #[test]
    fn dragging_the_thumb_maps_the_pointer_to_an_offset() {
        let extent = Extent::rows(100, 20., 400., 0.);
        let start = thumb(extent, 400.).unwrap();
        let grab = 30.;
        assert_eq!(dragged(extent, 400., start, grab, 30.), 0.);
        assert_eq!(dragged(extent, 400., start, grab, 190.), 800.);
        assert_eq!(dragged(extent, 400., start, grab, 350.), 1600.);
        assert_eq!(dragged(extent, 400., start, grab, 900.), 1600.);
        assert_eq!(dragged(extent, 400., start, grab, -50.), 0.);
        let moved = thumb(Extent { offset: 800., ..extent }, 400.).unwrap();
        assert_eq!(moved.start, 190. - grab);
    }

    #[test]
    fn a_thumb_filling_the_track_does_not_move_the_list() {
        let extent = Extent::rows(21, 20., 400., 12.);
        let full = Thumb { start: 0., length: 400. };
        assert_eq!(dragged(extent, 400., full, 10., 300.), 12.);
    }

    #[test]
    fn the_extent_reads_a_scroll_handle() {
        let handle = ScrollHandle::new();
        handle.set_offset(point(px(-15.), px(-120.)));
        let vertical = Extent::of(&handle, Axis::Vertical);
        assert_eq!(vertical.offset, 120.);
        assert_eq!(Extent::of(&handle, Axis::Horizontal).offset, 15.);
        move_to(&handle, Axis::Vertical, 300.);
        assert_eq!(handle.offset(), point(px(-15.), px(-300.)));
        move_to(&handle, Axis::Horizontal, 40.);
        assert_eq!(handle.offset(), point(px(-40.), px(-300.)));
        let list = UniformListScrollHandle::new();
        move_to(&list.scroll_handle(), Axis::Vertical, 56.);
        assert_eq!(list.0.borrow().base_handle.offset().y, px(-56.));
    }

    #[test]
    fn a_handle_not_yet_laid_out_shows_no_bar() {
        let fresh = ScrollHandle::new();
        assert!(!Extent::of(&fresh, Axis::Horizontal).scrollable());
        assert!(!Extent::of(&fresh, Axis::Vertical).scrollable());
        assert_eq!(thumb(Extent::of(&fresh, Axis::Horizontal), 400.), None);
    }

    #[test]
    fn the_strip_overlays_the_edge_of_the_area() {
        let area = area();
        let vertical = strip(area, Axis::Vertical);
        assert_eq!(vertical.origin, point(px(390.), px(50.)));
        assert_eq!(vertical.size, size(px(STRIP), px(404.)));
        assert!(area.contains(&vertical.origin));
        assert_eq!(vertical.right(), area.right());
        let horizontal = strip(area, Axis::Horizontal);
        assert_eq!(horizontal.origin, point(px(100.), px(444.)));
        assert_eq!(horizontal.size, size(px(300.), px(STRIP)));
        assert_eq!(horizontal.bottom(), area.bottom());
        assert_eq!(track_length(vertical, Axis::Vertical), 400.);
        assert_eq!(track_length(horizontal, Axis::Horizontal), 296.);
    }

    #[test]
    fn pointer_positions_and_thumb_bounds_share_one_origin() {
        let vertical = strip(area(), Axis::Vertical);
        let thumb = segment(vertical, Axis::Vertical, 100., 80.);
        assert_eq!(thumb.origin, point(px(392.), px(152.)));
        assert_eq!(thumb.size, size(px(THICKNESS), px(80.)));
        assert_eq!(thumb.right(), vertical.right() - px(INSET));
        assert_eq!(along(vertical, Axis::Vertical, thumb.origin), 100.);
        let horizontal = strip(area(), Axis::Horizontal);
        let bar = segment(horizontal, Axis::Horizontal, 20., 50.);
        assert_eq!(bar.origin, point(px(122.), px(446.)));
        assert_eq!(along(horizontal, Axis::Horizontal, bar.origin), 20.);
    }

    #[test]
    fn marks_sit_on_the_track_at_their_share_of_the_content() {
        assert_eq!(mark_start(0., 400.), 0.);
        assert_eq!(mark_start(0.25, 400.), 100.);
        assert_eq!(mark_start(1., 400.), 400. - MARK);
        assert_eq!(mark_start(3., 400.), 400. - MARK);
        assert_eq!(mark_start(-1., 400.), 0.);
        assert_eq!(mark_start(0.5, 1.), 0.);
    }

    #[test]
    fn the_bar_shows_always_or_while_scrolling_and_hovered() {
        assert_eq!(opacity(true, false, None), 1.);
        assert_eq!(opacity(false, true, None), 1.);
        assert_eq!(opacity(false, false, None), 0.);
        assert_eq!(opacity(false, false, Some(Duration::from_millis(100))), 1.);
        assert_eq!(opacity(false, false, Some(LINGER)), 1.);
        let fading = opacity(false, false, Some(LINGER + FADE / 2));
        assert!(fading > 0.4 && fading < 0.6);
        assert_eq!(opacity(false, false, Some(LINGER + FADE)), 0.);
        assert_eq!(opacity(false, false, Some(Duration::from_secs(5))), 0.);
        assert_eq!(opacity(false, true, Some(Duration::from_secs(5))), 1.);
    }
}
