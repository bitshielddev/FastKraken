use crate::{
    Tab, kit,
    commands::{Availability, Bar, Command, Group, Target},
    preferences::setting,
    settings::{self, Settings},
    theme,
    view::{self, Desk},
};
use gpui::{App, Context, Div, Pixels, SharedString, Size, Window, div, prelude::*};

pub(crate) const STEPS: [u16; 8] = [80, 90, 100, 110, 120, 130, 140, 150];
const LABELS: [&str; 8] = ["80%", "90%", "100%", "110%", "120%", "130%", "140%", "150%"];
pub(crate) const ACTUAL: u16 = 100;
const SMALLEST: u16 = STEPS[0];
const LARGEST: u16 = STEPS[STEPS.len() - 1];
const TRAFFIC_LIGHTS: f32 = 14.;

#[derive(Clone, Debug)]
pub(crate) enum Action {
    In,
    Out,
    Actual,
    Set(u16),
}

pub(crate) fn snapped(percent: u16) -> u16 {
    STEPS.into_iter().min_by_key(|step| step.abs_diff(percent)).unwrap_or(ACTUAL)
}

pub(crate) fn chosen(settings: &Settings) -> u16 {
    settings.zoom.map_or(ACTUAL, snapped)
}

pub(crate) fn next(percent: u16, action: &Action) -> u16 {
    let percent = snapped(percent);
    match action {
        Action::In => STEPS.into_iter().find(|step| *step > percent).unwrap_or(LARGEST),
        Action::Out => STEPS.into_iter().rev().find(|step| *step < percent).unwrap_or(SMALLEST),
        Action::Actual => ACTUAL,
        Action::Set(wanted) => snapped(*wanted),
    }
}

pub(crate) fn refusal(percent: u16, action: &Action) -> Option<String> {
    match action {
        Action::In if percent >= LARGEST => Some(format!("FastKraken is at its largest size, {LARGEST}%.")),
        Action::Out if percent <= SMALLEST => Some(format!("FastKraken is at its smallest size, {SMALLEST}%.")),
        Action::Actual if percent == ACTUAL => Some("FastKraken is already at its actual size.".into()),
        _ => None,
    }
}

fn offered<const N: usize>(_: &view::Ui, target: &Target, cx: &App) -> Availability {
    if *target != Target::Repo {
        return Availability::Hidden;
    }
    let action = [Action::In, Action::Out, Action::Actual][N].clone();
    Availability::unless(refusal(chosen(&settings::settings(cx)), &action))
}

fn run<const N: usize>(_: &view::Ui, _: &Target) -> Option<view::Action> {
    Some(view::Action::Zoom([Action::In, Action::Out, Action::Actual][N].clone()))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new("view.zoom-in", Group::View, |_, _| "Zoom In".into(), offered::<0>, run::<0>)
        .keys("cmd-=")
        .bar(Bar::View)
        .words(&["zoom", "bigger", "larger", "text size", "interface size", "scale", "magnify"]),
    Command::new("view.zoom-out", Group::View, |_, _| "Zoom Out".into(), offered::<1>, run::<1>)
        .keys("cmd--")
        .bar(Bar::View)
        .words(&["zoom", "smaller", "text size", "interface size", "scale"]),
    Command::new("view.actual-size", Group::View, |_, _| "Actual Size".into(), offered::<2>, run::<2>)
        .keys("cmd-0")
        .bar(Bar::View)
        .words(&["zoom", "reset zoom", "100%", "normal size", "text size", "interface size"]),
];

pub(crate) fn min_window() -> (f32, f32) {
    let factor = theme::zoom();
    (settings::MIN_WINDOW.0 * factor, settings::MIN_WINDOW.1 * factor)
}

pub(crate) fn min_window_size(cx: &App) -> Size<Pixels> {
    let (width, height) = min_window();
    let screen = cx.primary_display().map(|display| display.bounds().size);
    let width = screen.map_or(width, |screen| width.min(f32::from(screen.width).max(settings::MIN_WINDOW.0)));
    let height = screen.map_or(height, |screen| height.min(f32::from(screen.height).max(settings::MIN_WINDOW.1)));
    Size::new(gpui::px(width), gpui::px(height))
}

pub(crate) fn traffic_lights_top() -> f32 {
    ((theme::TOOLBAR * theme::zoom() - TRAFFIC_LIGHTS) / 2.).round()
}

pub(crate) fn start(cx: &mut App) {
    if apply(cx) {
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, cx| fit(window, cx));
        }
    }
}

fn apply(cx: &mut App) -> bool {
    let changed = theme::use_zoom(chosen(&settings::settings(cx)));
    if changed {
        cx.refresh_windows();
    }
    changed
}

fn fit(window: &mut Window, cx: &App) {
    if window.is_fullscreen() {
        return;
    }
    let (least_width, least_height) = min_window();
    let now = window.viewport_size();
    let screen = window.display(cx).map(|display| display.bounds().size);
    let widest = screen.map_or(f32::MAX, |screen| f32::from(screen.width));
    let tallest = screen.map_or(f32::MAX, |screen| f32::from(screen.height));
    let width = f32::from(now.width).max(least_width.min(widest));
    let height = f32::from(now.height).max(least_height.min(tallest));
    if width > f32::from(now.width) || height > f32::from(now.height) {
        window.resize(Size::new(gpui::px(width), gpui::px(height)));
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let now = chosen(&settings::settings(cx));
    if let Some(reason) = refusal(now, &action) {
        tab.ui.notices.show(crate::notice::Notice::new(view::Tone::Info, reason, ""));
        cx.notify();
        return;
    }
    let wanted = next(now, &action);
    Settings::update(cx, |s| s.zoom = (wanted != ACTUAL).then_some(wanted));
    if apply(cx) {
        fit(window, cx);
        let here = window.window_handle();
        for handle in cx.windows().into_iter().filter(|handle| *handle != here) {
            let _ = handle.update(cx, |_, window, cx| fit(window, cx));
        }
    }
    cx.notify();
}

pub(crate) const HELP: &str = "Scales text, rows, icons and spacing. In the View menu, ⌘= zooms in, ⌘- zooms out and ⌘0 returns to 100%.";

pub(crate) fn preferences<T: Desk>(cx: &mut Context<T>) -> Div {
    let now = chosen(&settings::settings(cx));
    let mut choices = kit::segmented();
    for (index, step) in STEPS.into_iter().enumerate() {
        choices = choices.child(kit::segment(
            ("preferences-zoom", index),
            LABELS[index],
            None,
            now == step,
            view::Action::Zoom(Action::Set(step)),
            cx,
        ));
    }
    div().flex().flex_col().child(setting("Zoom", Some(SharedString::new_static(HELP)), choices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;

    #[test]
    fn steps_are_ten_per_cent_apart_from_eighty_to_one_hundred_and_fifty() {
        assert_eq!(STEPS.first(), Some(&80));
        assert_eq!(STEPS.last(), Some(&150));
        assert!(STEPS.windows(2).all(|pair| pair[1] - pair[0] == 10));
        assert!(STEPS.contains(&ACTUAL));
        for (step, label) in STEPS.into_iter().zip(LABELS) {
            assert_eq!(format!("{step}%"), label);
        }
    }

    #[test]
    fn zooming_moves_one_step_and_stops_at_either_end() {
        assert_eq!(next(100, &Action::In), 110);
        assert_eq!(next(100, &Action::Out), 90);
        assert_eq!(next(150, &Action::In), 150);
        assert_eq!(next(80, &Action::Out), 80);
        assert_eq!(next(130, &Action::Actual), 100);
        assert_eq!(next(100, &Action::Set(140)), 140);
        let mut percent = ACTUAL;
        for _ in 0..10 {
            percent = next(percent, &Action::In);
        }
        assert_eq!(percent, 150);
        for _ in 0..10 {
            percent = next(percent, &Action::Out);
        }
        assert_eq!(percent, 80);
    }

    #[test]
    fn a_stored_size_off_the_steps_snaps_to_the_nearest_and_anything_else_is_actual_size() {
        assert_eq!(snapped(104), 100);
        assert_eq!(snapped(116), 120);
        assert_eq!(snapped(10), 80);
        assert_eq!(snapped(900), 150);
        let settings: Settings = serde_json::from_str(r#"{"zoom": 133}"#).unwrap();
        assert_eq!(chosen(&settings), 130);
        let settings: Settings = serde_json::from_str(r#"{"zoom": "huge"}"#).unwrap();
        assert_eq!(chosen(&settings), ACTUAL);
        let settings: Settings = serde_json::from_str(r#"{"zoom": -5}"#).unwrap();
        assert_eq!(chosen(&settings), ACTUAL);
        assert_eq!(chosen(&Settings::default()), ACTUAL);
    }

    #[test]
    fn the_ends_and_actual_size_say_why_they_do_nothing() {
        assert_eq!(refusal(150, &Action::In).as_deref(), Some("FastKraken is at its largest size, 150%."));
        assert_eq!(refusal(80, &Action::Out).as_deref(), Some("FastKraken is at its smallest size, 80%."));
        assert_eq!(refusal(100, &Action::Actual).as_deref(), Some("FastKraken is already at its actual size."));
        assert_eq!(refusal(140, &Action::In), None);
        assert_eq!(refusal(90, &Action::Out), None);
        assert_eq!(refusal(110, &Action::Actual), None);
        assert_eq!(refusal(150, &Action::Set(150)), None);
    }

    #[test]
    fn the_view_menu_offers_the_three_sizes_on_the_mac_shortcuts() {
        let wanted = [("view.zoom-in", "cmd-=", "⌘="), ("view.zoom-out", "cmd--", "⌘-"), ("view.actual-size", "cmd-0", "⌘0")];
        for (id, shortcut, shown) in wanted {
            let command = commands::find(id).expect("a zoom command");
            assert_eq!(command.shortcut, Some(shortcut));
            assert_eq!(command.bar, Some(Bar::View));
            assert_eq!(command.group, Group::View);
            assert!(command.words.contains(&"zoom"), "{id}");
            assert_eq!(commands::keys(shortcut), shown);
            assert!(gpui::Keystroke::parse(shortcut).is_ok(), "{shortcut}");
            assert!(HELP.contains(shown), "the Zoom description names {shown}");
        }
    }
}
