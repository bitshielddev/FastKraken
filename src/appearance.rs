use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, Target},
    preferences::setting,
    settings::{self, Settings},
    theme::{self, Shade},
    view::{self, Desk},
};
use gpui::{App, Context, Div, Global, SharedString, Window, WindowAppearance, div, prelude::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Choice {
    System,
    Light,
    #[default]
    Dark,
}

impl Choice {
    pub(crate) const ALL: [Choice; 3] = [Choice::System, Choice::Light, Choice::Dark];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Choice::System => "System",
            Choice::Light => "Light",
            Choice::Dark => "Dark",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Choose(Choice),
}

fn chosen_already<const N: usize>(_: &view::Ui, target: &Target, cx: &App) -> Availability {
    if *target != Target::Repo {
        return Availability::Hidden;
    }
    if settings::settings(cx).appearance == Choice::ALL[N] {
        Availability::Disabled("FastKraken already uses this appearance.".into())
    } else {
        Availability::Enabled
    }
}

fn choose<const N: usize>(_: &view::Ui, _: &Target) -> Option<view::Action> {
    Some(view::Action::Appearance(Action::Choose(Choice::ALL[N])))
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "app.appearance-system",
        Group::View,
        |_, _| "Follow the system appearance".into(),
        chosen_already::<0>,
        choose::<0>,
    )
    .words(&["switch theme", "theme", "appearance", "sync with system", "automatic", "light", "dark"]),
    Command::new(
        "app.appearance-light",
        Group::View,
        |_, _| "Use the light appearance".into(),
        chosen_already::<1>,
        choose::<1>,
    )
    .words(&["switch theme", "theme", "appearance", "light mode", "colours", "colors"]),
    Command::new(
        "app.appearance-dark",
        Group::View,
        |_, _| "Use the dark appearance".into(),
        chosen_already::<2>,
        choose::<2>,
    )
    .words(&["switch theme", "theme", "appearance", "dark mode", "colours", "colors"]),
];

struct System(Shade);

impl Global for System {}

pub(crate) fn shade_of(appearance: WindowAppearance) -> Shade {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Shade::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Shade::Dark,
    }
}

pub(crate) fn resolve(choice: Choice, system: Shade) -> Shade {
    match choice {
        Choice::System => system,
        Choice::Light => Shade::Light,
        Choice::Dark => Shade::Dark,
    }
}

pub(crate) fn system(cx: &App) -> Shade {
    cx.try_global::<System>().map_or(Shade::Dark, |system| system.0)
}

pub(crate) fn start(cx: &mut App) {
    if !cx.has_global::<System>() {
        let shade = shade_of(cx.window_appearance());
        cx.set_global(System(shade));
    }
    apply(cx);
}

pub(crate) fn system_changed(appearance: WindowAppearance, cx: &mut App) {
    cx.set_global(System(shade_of(appearance)));
    apply(cx);
}

fn apply(cx: &mut App) {
    let shade = resolve(settings::settings(cx).appearance, system(cx));
    if theme::use_shade(shade) {
        cx.refresh_windows();
    }
}

pub(crate) fn dispatch(_: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Choose(choice) => {
            Settings::update(cx, |s| s.appearance = choice);
            apply(cx);
        }
    }
    cx.notify();
}

pub(crate) fn help(choice: Choice, system: Shade) -> String {
    let now = match system {
        Shade::Light => "light",
        Shade::Dark => "dark",
    };
    match choice {
        Choice::System => format!("Follows the Light or Dark setting in macOS System Settings > Appearance, which is {now} now."),
        Choice::Light | Choice::Dark => {
            "System follows the Light or Dark setting in macOS System Settings > Appearance, and changes with it.".into()
        }
    }
}

pub(crate) fn preferences<T: Desk>(cx: &mut Context<T>) -> Div {
    let chosen = settings::settings(cx).appearance;
    let mut choices = kit::segmented();
    for (index, choice) in Choice::ALL.into_iter().enumerate() {
        choices = choices.child(kit::segment(
            ("preferences-appearance", index),
            choice.title(),
            None,
            chosen == choice,
            view::Action::Appearance(Action::Choose(choice)),
            cx,
        ));
    }
    let help: SharedString = help(chosen, system(cx)).into();
    div().flex().flex_col().child(setting("Appearance", Some(help), choices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;

    #[test]
    fn system_follows_the_window_and_the_fixed_choices_ignore_it() {
        assert_eq!(resolve(Choice::System, shade_of(WindowAppearance::Light)), Shade::Light);
        assert_eq!(resolve(Choice::System, shade_of(WindowAppearance::VibrantLight)), Shade::Light);
        assert_eq!(resolve(Choice::System, shade_of(WindowAppearance::Dark)), Shade::Dark);
        assert_eq!(resolve(Choice::System, shade_of(WindowAppearance::VibrantDark)), Shade::Dark);
        for system in [Shade::Light, Shade::Dark] {
            assert_eq!(resolve(Choice::Light, system), Shade::Light);
            assert_eq!(resolve(Choice::Dark, system), Shade::Dark);
        }
    }

    #[test]
    fn the_choice_is_saved_by_name_and_an_unknown_one_falls_back_to_dark() {
        assert_eq!(serde_json::to_value(Choice::System).unwrap(), "system");
        assert_eq!(serde_json::to_value(Choice::Light).unwrap(), "light");
        assert_eq!(serde_json::from_value::<Choice>("dark".into()).unwrap(), Choice::Dark);
        let settings: Settings = serde_json::from_str(r#"{"appearance": "sepia"}"#).unwrap();
        assert_eq!(settings.appearance, Choice::Dark);
        let settings: Settings = serde_json::from_str(r#"{"appearance": "light"}"#).unwrap();
        assert_eq!(settings.appearance, Choice::Light);
        assert_eq!(Settings::default().appearance, Choice::Dark);
    }

    #[test]
    fn the_help_says_what_system_follows_and_what_it_is_now() {
        assert!(help(Choice::System, Shade::Light).ends_with("which is light now."));
        assert!(help(Choice::System, Shade::Dark).ends_with("which is dark now."));
        assert!(help(Choice::Dark, Shade::Light).starts_with("System follows"));
    }

    #[test]
    fn the_palette_offers_each_appearance_by_its_theme_words() {
        let ids = ["app.appearance-system", "app.appearance-light", "app.appearance-dark"];
        for (index, id) in ids.into_iter().enumerate() {
            let command = commands::find(id).expect("an appearance command");
            assert!(command.words.contains(&"switch theme"), "{id}");
            assert_eq!(command.bar, None);
            assert_eq!(command.group, Group::View);
            assert_eq!(Choice::ALL[index].title(), ["System", "Light", "Dark"][index]);
        }
    }
}
