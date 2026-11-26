use crate::{
    Tab, kit,
    commands::{self, Group},
    notice::Notice,
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{
    Context, Entity, Focusable, FontWeight, UniformListScrollHandle, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use std::{ops::Range, path::PathBuf};

pub(crate) const COMMANDS: &[commands::Command] = {
    use commands::{Availability, Bar, Command, Target};
    fn always(target: &Target) -> Availability {
        if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
    }
    &[
        Command::new(
            "app.about",
            Group::App,
            |_, _| "About FastKraken".into(),
            |_, target, _| always(target),
            |_, _| Some(view::Action::Help(Action::About)),
        )
        .words(&["version"])
        .bar(Bar::App),
        Command::new(
            "app.shortcuts",
            Group::App,
            |_, _| "Keyboard shortcuts".into(),
            |_, target, _| always(target),
            |_, _| Some(view::Action::Help(Action::Shortcuts)),
        )
        .words(&["hotkeys", "bindings", "keys"])
        .icon("search")
        .keys("cmd-/")
        .bar(Bar::Help),
    ]
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sheet {
    About,
    Shortcuts,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    About,
    Shortcuts,
    Close,
    RevealNotices,
}

pub(crate) struct State {
    sheet: Option<Sheet>,
    pub filter: Entity<CommitInput>,
    git: Option<Result<String, String>>,
    scroll: UniformListScrollHandle,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter shortcuts"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self { sheet: None, filter, git: None, scroll: UniformListScrollHandle::new() }
    }

    pub(crate) fn open(&self) -> bool {
        self.sheet.is_some()
    }

    pub(crate) fn sheet(&self) -> Option<Sheet> {
        self.sheet
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::About => {
            tab.ui.help.sheet = Some(Sheet::About);
            view::focus(&tab.ui, window);
            if tab.ui.help.git.is_none() {
                Tab::work(cx, crate::repository::bundled_version, |tab, version, cx| {
                    tab.ui.help.git = Some(version);
                    cx.notify();
                });
            }
        }
        Action::Shortcuts if tab.ui.help.sheet == Some(Sheet::Shortcuts) => close(tab, window),
        Action::Shortcuts => {
            tab.ui.help.sheet = Some(Sheet::Shortcuts);
            tab.ui.help.filter.update(cx, |input, cx| input.clear(cx));
            window.focus(&tab.ui.help.filter.focus_handle(cx));
        }
        Action::Close => close(tab, window),
        Action::RevealNotices => match notices() {
            Some(path) if path.is_file() => cx.reveal_path(&path),
            path => {
                let place = path.map_or_else(|| "the app".to_owned(), |path| path.display().to_string());
                tab.ui
                    .notices
                    .show(Notice::new(Tone::Error, "Git notices not found", format!("Expected at {place}")));
            }
        },
    }
    cx.notify();
}

fn close(tab: &mut Tab, window: &mut Window) {
    tab.ui.help.sheet = None;
    view::focus(&tab.ui, window);
}

fn notices() -> Option<PathBuf> {
    let binary = std::env::current_exe().ok()?;
    let macos = binary.parent()?;
    match macos.parent() {
        Some(contents) if macos.ends_with("MacOS") && contents.ends_with("Contents") => {
            Some(contents.join("Resources/Licenses/git-notices.txt"))
        }
        _ => Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/macos/git-notices.txt")),
    }
}

fn heading(group: Group) -> &'static str {
    match group {
        Group::Repository | Group::Recent | Group::Manage | Group::Remotes => "Repository",
        Group::Sync => "Fetch, pull and push",
        Group::Changes | Group::Stash | Group::Reverse => "Changes and commits",
        Group::Create | Group::Switch => "Branches",
        Group::View | Group::Copy => "View",
        Group::Tabs => "Tabs",
        Group::Open => "Open elsewhere",
        Group::App | Group::Settings => "Help",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub section: &'static str,
    pub keys: String,
    pub title: String,
}

pub(crate) fn listing(ui: &Ui) -> Vec<Listed> {
    let mut commands: Vec<_> = commands::all().filter(|command| command.shortcut.is_some()).collect();
    commands.sort_by_key(|command| heading_order(heading(command.group)));
    let mut listed: Vec<Listed> = commands
        .into_iter()
        .map(|command| Listed {
            section: heading(command.group),
            keys: commands::keys(command.shortcut.unwrap_or_default()),
            title: (command.title)(ui, &commands::Target::Repo),
        })
        .collect();
    listed.extend(view::OTHER_KEYS.iter().map(|(keys, title)| Listed {
        section: "Window and moving around",
        keys: commands::keys(keys),
        title: (*title).to_owned(),
    }));
    listed.extend(crate::view::commit_input::TEXT_KEYS.iter().map(|(keys, title)| Listed {
        section: "Text fields",
        keys: commands::keys(keys),
        title: (*title).to_owned(),
    }));
    listed
}

fn heading_order(section: &str) -> usize {
    [
        "Repository",
        "Fetch, pull and push",
        "Changes and commits",
        "Branches",
        "View",
        "Tabs",
        "Open elsewhere",
        "Help",
    ]
    .iter()
    .position(|s| *s == section)
    .unwrap_or(usize::MAX)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    Heading(&'static str),
    Shortcut { keys: String, title: String },
}

pub(crate) fn lines(listing: &[Listed], filter: &str) -> Vec<Line> {
    let filter = filter.trim().to_lowercase();
    let glyphs = if filter.contains('-') {
        commands::keys(&filter.replace("option", "alt")).to_lowercase()
    } else {
        String::new()
    };
    let mut lines = Vec::new();
    let mut section = None;
    for item in listing {
        let keys = item.keys.to_lowercase();
        let matched = filter.is_empty()
            || item.title.to_lowercase().contains(&filter)
            || keys.contains(&filter)
            || (!glyphs.is_empty() && keys.contains(&glyphs));
        if !matched {
            continue;
        }
        if section != Some(item.section) {
            section = Some(item.section);
            lines.push(Line::Heading(item.section));
        }
        lines.push(Line::Shortcut { keys: item.keys.clone(), title: item.title.clone() });
    }
    lines
}

pub(crate) fn render<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    let close = view::Action::Help(Action::Close);
    let surface = match ui.help.sheet? {
        Sheet::About => about(ui, cx),
        Sheet::Shortcuts => shortcuts(ui, cx),
    };
    Some(kit::modal(surface, Some(close), &ui.modal_focus, cx))
}

fn about<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> gpui::Div {
    let git = match &ui.help.git {
        None => "Bundled Git: reading its version…".to_owned(),
        Some(Ok(version)) => format!("Bundled Git {version}"),
        Some(Err(error)) => format!("Bundled Git: {error}"),
    };
    let git = match crate::repository::git_in_use() {
        Some(path) => format!("{git}, not in use: Git runs from {}", path.display()),
        None => git,
    };
    kit::dialog("About FastKraken", 380.)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(TEXT_TITLE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("FastKraken {}", env!("CARGO_PKG_VERSION"))),
                )
                .child(kit::muted("Git for the impatient. One repository, one window."))
                .child(div().text_size(px(TEXT_SMALL)).text_color(rgb(palette().text_2)).child(git)),
        )
        .child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    kit::btn("about-notices", "Show Git notices")
                        .icon("folder")
                        .on(view::Action::Help(Action::RevealNotices), cx),
                )
                .child(kit::btn("about-close", "Close").primary().on(view::Action::Help(Action::Close), cx)),
        )
}

const LIST_HEIGHT: f32 = 380.;

fn shortcuts<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> gpui::Div {
    let list = shortcut_list("shortcut-lines", ui, |ui| &ui.help.filter, ui.help.scroll.clone(), Some(LIST_HEIGHT), cx);
    kit::dialog("Keyboard shortcuts", 460.)
        .child(ui.help.filter.clone())
        .child(list)
}

pub(crate) fn shortcut_list<T: Desk>(
    id: &'static str,
    ui: &Ui,
    field: fn(&Ui) -> &Entity<CommitInput>,
    scroll: UniformListScrollHandle,
    height: Option<f32>,
    cx: &mut Context<T>,
) -> gpui::Div {
    let sized = |d: gpui::Div| match height {
        Some(height) => d.h(px(height)),
        None => d.flex_1().min_h_0(),
    };
    let filter = field(ui).read(cx).text().to_owned();
    let count = lines(&listing(ui), &filter).len();
    if count == 0 {
        return sized(div())
            .p_3()
            .child(kit::muted(format!("No shortcut matches “{}”.", filter.trim())));
    }
    sized(div()).flex().flex_col().child(
        uniform_list(
            id,
            count,
            cx.processor(move |app, range: Range<usize>, _, cx| {
                let ui = app.ui();
                let filter = field(ui).read(cx).text().to_owned();
                let lines = lines(&listing(ui), &filter);
                range
                    .filter_map(|index| lines.get(index).cloned().map(|line| (index, line)))
                    .map(|(index, line)| match line {
                        Line::Heading(section) => div()
                            .id(("shortcut-line", index))
                            .w_full()
                            .h(px(ROW))
                            .px_2()
                            .flex()
                            .items_end()
                            .pb_1()
                            .text_size(px(TEXT_TINY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette().text_3))
                            .child(section.to_uppercase()),
                        Line::Shortcut { keys, title } => div()
                            .id(("shortcut-line", index))
                            .w_full()
                            .h(px(ROW))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(kit::fit(title).grow())
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .px_2()
                                    .rounded(px(RADIUS_CHIP))
                                    .bg(rgb(palette().raised))
                                    .text_size(px(TEXT_SMALL))
                                    .text_color(rgb(palette().text_2))
                                    .child(keys),
                            ),
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(scroll.clone())
        .flex_1()
        .min_h_0(),
    )
    .child(kit::scrollbar(gpui::SharedString::from(format!("{id}-scrollbar")), &scroll))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(section: &'static str, keys: &str, title: &str) -> Listed {
        Listed { section, keys: keys.into(), title: title.into() }
    }

    #[test]
    fn the_filter_keeps_matching_shortcuts_under_their_headings() {
        let listing = [
            listed("Fetch, pull and push", "⌘L", "Fetch all remotes"),
            listed("Fetch, pull and push", "⇧⌘L", "Pull"),
            listed("Changes and commits", "S", "Stage file"),
            listed("Changes and commits", "⇧⌘S", "Stage all"),
        ];
        assert_eq!(lines(&listing, "").len(), 6);
        assert_eq!(
            lines(&listing, "stage"),
            [
                Line::Heading("Changes and commits"),
                Line::Shortcut { keys: "S".into(), title: "Stage file".into() },
                Line::Shortcut { keys: "⇧⌘S".into(), title: "Stage all".into() },
            ]
        );
        let by_keys = |filter| {
            lines(&listing, filter)
                .into_iter()
                .filter_map(|line| match line {
                    Line::Shortcut { title, .. } => Some(title),
                    Line::Heading(_) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(by_keys("cmd-shift-l"), ["Pull"]);
        assert_eq!(by_keys("⇧⌘"), ["Pull", "Stage all"]);
        assert_eq!(by_keys("PULL"), ["Pull"]);
        assert!(lines(&listing, "rebase").is_empty());
    }

    #[test]
    fn every_command_shortcut_has_a_section_in_order() {
        for command in commands::all().filter(|command| command.shortcut.is_some()) {
            assert_ne!(heading_order(heading(command.group)), usize::MAX, "{}", command.id);
        }
    }

    #[test]
    fn the_git_notices_are_where_the_bundle_or_the_source_keeps_them() {
        let path = notices().expect("a place for the notices");
        assert!(path.ends_with("git-notices.txt"));
        assert!(path.is_file(), "{} is missing", path.display());
    }
}
