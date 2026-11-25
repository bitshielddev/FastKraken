use crate::{
    Tab, acceptance,
    commands::{self, Availability, Bar, Command, Entry, Group, Target, on_repo},
    kit, navigate,
    notice::Notice,
    palette,
    settings::{self, Memory, Recent},
    storage,
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{
    App, Context, Div, Entity, Focusable, FontWeight, ScrollStrategy, UniformListScrollHandle, Window, div, prelude::*,
    px, rgb, uniform_list,
};
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

pub(crate) const IN_MENU: usize = 5;
pub(crate) const IN_BAR: usize = 10;
const LIST_ROWS: usize = 9;
const LIST_ROW: f32 = 34.;

pub(crate) const COMMANDS: &[Command] = {
    fn closable(ui: &Ui, target: &Target, _: &App) -> Availability {
        if !on_repo(ui, target) {
            return Availability::Hidden;
        }
        Availability::unless(
            ui.jobs
                .refusal()
                .map(|running| format!("{running}. Cancel it or let it finish first"))
                .or_else(|| commands::busy(ui)),
        )
    }
    &[
        Command::new(
            "repo.open",
            Group::Repository,
            |_, _| "Open repository…".into(),
            |_, target, _| {
                if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(view::Action::Open),
        )
        .words(&["folder", "load"])
        .icon("folder")
        .keys("cmd-o")
        .bar(Bar::File),
        Command::new(
            "repo.open-recent",
            Group::Repository,
            |_, _| "Open recent repository…".into(),
            |_, target, _| {
                if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(view::Action::Palette(palette::Action::Open(palette::Mode::Recent))),
        )
        .words(&["recents", "switch repository", "repositories", "projects"])
        .icon("history")
        .keys("cmd-shift-o"),
        Command::new(
            "repo.close",
            Group::Repository,
            |_, _| "Close repository".into(),
            closable,
            |_, _| Some(view::Action::Start(Action::Close)),
        )
        .words(&["start screen", "welcome"])
        .icon("x")
        .bar(Bar::File),
        Command::new(
            "repo.clear-recents",
            Group::Recent,
            |_, _| "Clear recent repositories".into(),
            |_, target, cx| {
                if *target != Target::Repo {
                    return Availability::Hidden;
                }
                let none = settings::memory(cx).is_none_or(|memory| memory.recents.is_empty());
                Availability::unless(none.then(|| "There are no recent repositories".to_owned()))
            },
            |_, _| Some(view::Action::Start(Action::Clear)),
        )
        .words(&["forget recents", "recent list"]),
    ]
};

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastkraken_desk, no_json)]
pub(crate) struct OpenRecent {
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(PathBuf),
    Choose(Option<usize>),
    Hover(usize),
    Remove(PathBuf),
    Clear,
    Restore(Vec<Recent>),
    Close,
}

pub(crate) struct State {
    pub(crate) filter: Entity<CommitInput>,
    pick: Option<(String, usize)>,
    scroll: UniformListScrollHandle,
    focus_pending: bool,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter recent repositories"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self { filter, pick: None, scroll: UniformListScrollHandle::new(), focus_pending: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) parent: String,
    pub(crate) opened: i64,
    pub(crate) missing: bool,
}

pub(crate) fn matching(recents: &[Recent], query: &str, exists: impl Fn(&Path) -> bool) -> Vec<Row> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut scored: Vec<(i32, Row)> = recents
        .iter()
        .filter_map(|recent| {
            let path = recent.path();
            let shown = path.display().to_string();
            let score = palette::score(query, &recent.name).or_else(|| palette::score(query, &shown).map(|s| s - 1_000))?;
            let parent = path.parent().map(|parent| homely(parent, home.as_deref())).unwrap_or_default();
            let missing = !exists(&path);
            Some((score, Row { path, name: recent.name.clone(), parent, opened: recent.opened, missing }))
        })
        .collect();
    if !query.trim().is_empty() {
        scored.sort_by_key(|(score, _)| -score);
    }
    scored.into_iter().map(|(_, row)| row).collect()
}

pub(crate) fn homely(folder: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| folder.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => folder.display().to_string(),
    }
}

fn typed(ui: &Ui, cx: &App) -> String {
    ui.start.filter.read(cx).text().trim().to_owned()
}

pub(crate) fn rows(ui: &Ui, cx: &App) -> Vec<Row> {
    let Some(memory) = settings::memory(cx) else { return Vec::new() };
    matching(&memory.recents, &typed(ui, cx), Path::exists)
}

pub(crate) fn highlighted(ui: &Ui, len: usize, cx: &App) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let text = typed(ui, cx);
    Some(match &ui.start.pick {
        Some((picked, row)) if *picked == text => (*row).min(len - 1),
        _ => 0,
    })
}

pub(crate) fn others(ui: &Ui, cx: &App, limit: usize) -> Vec<(String, PathBuf)> {
    let Some(memory) = settings::memory(cx) else { return Vec::new() };
    titled(&memory.recents, &ui.root, limit)
}

fn titled(recents: &[Recent], open: &Path, limit: usize) -> Vec<(String, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shown: Vec<&Recent> = recents.iter().filter(|recent| recent.path() != open).take(limit).collect();
    shown
        .iter()
        .map(|recent| {
            let path = recent.path();
            let twin = shown.iter().filter(|other| other.name == recent.name).count() > 1;
            let title = match path.parent() {
                Some(parent) if twin => format!("{} — {}", recent.name, homely(parent, home.as_deref())),
                _ => recent.name.clone(),
            };
            (title, path)
        })
        .collect()
}

pub(crate) fn menu_rows(ui: &Ui, cx: &App) -> Vec<Entry> {
    others(ui, cx, IN_MENU).into_iter().map(|(title, path)| entry(title, path, "folder")).collect()
}

pub(crate) fn palette_rows(ui: &Ui, query: &str, cx: &App) -> Vec<(i32, Entry)> {
    let query = open_query(query);
    others(ui, cx, settings::RECENTS)
        .into_iter()
        .filter_map(|(title, path)| {
            let score = palette::score(query, &title)?;
            Some((score, entry(format!("Open {title}"), path, "folder")))
        })
        .collect()
}

fn open_query(query: &str) -> &str {
    let lower = query.to_lowercase();
    match lower.strip_prefix("open ") {
        Some(_) if query.is_char_boundary(5) => query[5..].trim_start(),
        _ => query,
    }
}

fn entry(title: String, path: PathBuf, icon: &'static str) -> Entry {
    Entry {
        id: "repo.recent",
        title,
        icon: Some(icon),
        group: Group::Recent,
        shortcut: None,
        danger: false,
        disabled: None,
        action: Some(view::Action::Start(Action::Open(path))),
        children: Vec::new(),
        mark: None,
    }
}

pub(crate) fn bar_menu(recents: &[(String, PathBuf)]) -> gpui::MenuItem {
    let mut items: Vec<gpui::MenuItem> = recents
        .iter()
        .map(|(title, path)| gpui::MenuItem::action(title.clone(), OpenRecent { path: path.clone() }))
        .collect();
    if !items.is_empty() {
        items.push(gpui::MenuItem::separator());
    }
    items.push(gpui::MenuItem::action("More…", commands::Run { id: "repo.open-recent" }));
    items.push(gpui::MenuItem::action("Clear Menu", commands::Run { id: "repo.clear-recents" }));
    gpui::MenuItem::submenu(gpui::Menu { name: "Open Recent".into(), items })
}

pub(crate) fn remember(root: &Path, cx: &mut App) {
    Memory::update(cx, |memory| memory.add_recent(root, view::now()));
    if !cfg!(test) && !acceptance::replaying() && storage::override_directory().is_none() && settings::on_disk(cx) {
        cx.add_recent_document(root);
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => tab.open(path, cx),
        Action::Choose(index) => {
            let mut rows = rows(&tab.ui, cx);
            let Some(at) = index.or_else(|| highlighted(&tab.ui, rows.len(), cx)) else { return };
            if at < rows.len() {
                let row = rows.swap_remove(at);
                tab.open(row.path, cx);
            }
        }
        Action::Hover(row) => {
            let pick = Some((typed(&tab.ui, cx), row));
            if tab.ui.start.pick == pick {
                return;
            }
            tab.ui.start.pick = pick;
        }
        Action::Remove(path) => Memory::update(cx, |memory| memory.remove_recent(&path)),
        Action::Clear => {
            let earlier = settings::memory(cx).map(|memory| memory.recents.clone()).unwrap_or_default();
            if earlier.is_empty() {
                return;
            }
            Memory::update(cx, |memory| {
                memory.clear_recents();
            });
            let count = match earlier.len() {
                1 => "one repository".to_owned(),
                n => format!("{n} repositories"),
            };
            tab.ui.notices.show(
                Notice::new(Tone::Info, "Cleared the recent repositories", format!("The list no longer shows {count}."))
                    .offer("Undo", view::Action::Start(Action::Restore(earlier))),
            );
        }
        Action::Restore(earlier) => Memory::update(cx, |memory| memory.restore_recents(earlier)),
        Action::Close => tab.close(cx),
    }
    cx.notify();
}

pub(crate) fn key(tab: &mut Tab, action: navigate::Action, window: &mut Window, cx: &mut Context<Tab>) {
    if action == navigate::Action::Choose {
        dispatch(tab, Action::Choose(None), window, cx);
        return;
    }
    let len = rows(&tab.ui, cx).len();
    let current = highlighted(&tab.ui, len, cx);
    let Some(step) = navigate::how(action, LIST_ROWS - 1) else { return };
    let Some(row) = navigate::moved(current, len, step) else { return };
    tab.ui.start.scroll.scroll_to_item(row, ScrollStrategy::Center);
    tab.ui.start.pick = Some((typed(&tab.ui, cx), row));
    cx.notify();
}

pub(crate) fn take_focus(ui: &mut Ui, window: &mut Window, cx: &mut App) {
    window.focus(&ui.start.filter.focus_handle(cx));
    ui.start.focus_pending = false;
}

pub(crate) fn rearm(ui: &mut Ui) {
    ui.start.focus_pending = true;
}

pub(crate) fn filter_shown(ui: &Ui, cx: &App) -> bool {
    ui.repo.is_empty()
        && ui.opening.is_none()
        && !ui.preferences.open()
        && settings::memory(cx).is_some_and(|memory| !memory.recents.is_empty())
}

pub(crate) fn wants_focus(ui: &Ui, cx: &App) -> bool {
    ui.start.focus_pending && filter_shown(ui, cx) && !ui.modal_open() && !ui.dialog_open()
}

pub(crate) fn clear_filter(ui: &Ui, cx: &mut App) -> bool {
    if ui.repo.is_empty() && !ui.start.filter.read(cx).text().is_empty() {
        ui.start.filter.clone().update(cx, |filter, cx| filter.clear(cx));
        return true;
    }
    false
}


fn hint(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

pub(crate) fn screen<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let buttons = div()
        .flex()
        .gap_2()
        .child(
            kit::btn("choose", "Open repository…")
                .primary()
                .icon("folder")
                .hint(hint("⌘O", "Ctrl O"))
                .on(view::Action::Open, cx),
        )
        .child(
            kit::btn("clone", "Clone repository…")
                .icon("cloud")
                .hint(hint("⌘N", "Ctrl N"))
                .on(view::Action::Clone(crate::clone::Action::Open), cx),
        )
        .child(
            kit::btn("init", "New repository…")
                .icon("plus")
                .hint(hint("⌘I", "Ctrl I"))
                .on(view::Action::Init(crate::init::Action::Open), cx),
        );
    let column = div()
        .w(px(600.))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Open a Git repository"),
        )
        .child(
            div()
                .text_color(rgb(TEXT_2))
                .child("Choose a local working tree to see its history and changes, clone one from its address, or start a new one."),
        )
        .child(buttons)
        .child(recents(app, cx));
    div().flex_1().min_h_0().flex().flex_col().items_center().pt(px(88.)).child(column)
}

fn recents<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let none = settings::memory(cx).is_none_or(|memory| memory.recents.is_empty());
    let heading = div()
        .mt_4()
        .flex()
        .items_center()
        .child(kit::label("Recent repositories"))
        .child(kit::grow())
        .when(!none, |d| {
            d.child(
                kit::btn("recents-clear", "Clear")
                    .ghost()
                    .small()
                    .tip("Take every repository off this list. Nothing on disk changes.")
                    .on(view::Action::Start(Action::Clear), cx),
            )
        });
    if none {
        return div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading)
            .child(kit::muted("Repositories you open, clone or create are listed here."));
    }
    let shown = rows(ui, cx);
    let count = shown.len();
    let height = px(LIST_ROW * count.clamp(1, LIST_ROWS) as f32 + 8.);
    let list = if count == 0 {
        div()
            .h(height)
            .px_3()
            .flex()
            .items_center()
            .child(kit::muted(format!("No recent repository matches “{}”.", typed(ui, cx))))
    } else {
        div().h(height).py_1().flex().flex_col().child(
            uniform_list(
                "recent-rows",
                count,
                cx.processor(|app, range: Range<usize>, _, cx| {
                    let ui = app.ui();
                    let rows = rows(ui, cx);
                    let at = highlighted(ui, rows.len(), cx);
                    let now = view::now();
                    rows.into_iter()
                        .enumerate()
                        .skip(range.start)
                        .take(range.len())
                        .map(|(index, row)| recent_row(row, index, at == Some(index), now, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(ui.start.scroll.clone())
            .flex_1()
            .min_h_0(),
        )
        .child(kit::scrollbar("recent-scrollbar", &ui.start.scroll))
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(heading)
        .child(div().key_context("StartScreen").child(ui.start.filter.clone()))
        .child(
            div()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(PANEL))
                .overflow_hidden()
                .child(list),
        )
}

fn recent_row<T: Desk>(row: Row, index: usize, at: bool, now: i64, cx: &Context<T>) -> gpui::Stateful<Div> {
    let when = if row.missing {
        "No longer there".to_owned()
    } else {
        view::when(row.opened, &view::date_of(row.opened), now)
    };
    let tip = if row.missing {
        format!("Nothing is at {} any more. Opening it offers to take it off this list.", row.path.display())
    } else {
        row.path.display().to_string()
    };
    let path = row.path.clone();
    kit::cursor(kit::row(("recent", index), false), at)
        .h(px(LIST_ROW))
        .px_2()
        .gap_2()
        .when(row.missing, |d| d.opacity(0.45))
        .tooltip(kit::tip(tip))
        .child(kit::icon("folder").text_color(rgb(if row.missing { TEXT_3 } else { ACCENT })))
        .child(div().flex_shrink_0().max_w(px(220.)).min_w_0().flex().child(kit::fit(row.name).weight(FontWeight::MEDIUM)))
        .child(kit::fit(row.parent).size(TEXT_SMALL).color(TEXT_3).cut(kit::Cut::Start).grow())
        .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).text_color(rgb(TEXT_3)).child(when))
        .child(kit::icon_btn(
            ("recent-remove", index),
            "x",
            "Remove from recent repositories",
            view::Action::Start(Action::Remove(row.path)),
            cx,
        ))
        .on_click(cx.listener(move |app, _, w, cx| app.dispatch(view::Action::Start(Action::Open(path.clone())), w, cx)))
        .on_hover(cx.listener(move |app, hovered: &bool, w, cx| {
            if *hovered {
                app.dispatch(view::Action::Start(Action::Hover(index)), w, cx);
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recent(path: &str, opened: i64) -> Recent {
        let mut memory = Memory::default();
        memory.add_recent(Path::new(path), opened);
        memory.recents.remove(0)
    }

    #[test]
    fn the_list_is_newest_first_and_a_filter_ranks_name_matches_first() {
        let recents = [recent("/w/Juniper-trail", 30), recent("/w/apps/Aspen-ridge", 20), recent("/w/trail/sketches", 10)];
        let all = matching(&recents, "", |_| true);
        assert_eq!(all.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Juniper-trail", "Aspen-ridge", "sketches"]);
        assert_eq!(all[1].parent, "/w/apps");
        assert_eq!(all[0].opened, 30);
        let trail = matching(&recents, "trail", |_| true);
        assert_eq!(trail.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Juniper-trail", "sketches"]);
        assert!(matching(&recents, "zzz", |_| true).is_empty());
        assert_eq!(matching(&recents, "asp", |_| true)[0].name, "Aspen-ridge");
    }

    #[test]
    fn a_repository_that_has_gone_is_marked_missing() {
        let folder = tempfile::tempdir().unwrap();
        let here = folder.path().join("Here");
        std::fs::create_dir(&here).unwrap();
        let gone = folder.path().join("Gone");
        let recents = [recent(gone.to_str().unwrap(), 2), recent(here.to_str().unwrap(), 1)];
        let rows = matching(&recents, "", Path::exists);
        assert_eq!(rows.iter().map(|row| (row.name.as_str(), row.missing)).collect::<Vec<_>>(), [("Gone", true), ("Here", false)]);
    }

    #[test]
    fn folders_in_the_home_folder_start_with_a_tilde() {
        let home = Path::new("/Users/dana");
        assert_eq!(homely(Path::new("/Users/dana/Code/apps"), Some(home)), "~/Code/apps");
        assert_eq!(homely(Path::new("/Users/dana"), Some(home)), "~");
        assert_eq!(homely(Path::new("/Volumes/Work"), Some(home)), "/Volumes/Work");
        assert_eq!(homely(Path::new("/Users/danalyn"), Some(home)), "/Users/danalyn");
    }

    #[test]
    fn menus_leave_out_the_open_repository_and_name_twins_by_their_folder() {
        let recents = [
            recent("/w/Open-one", 9),
            recent("/a/site", 8),
            recent("/b/site", 7),
            recent("/w/Oak", 6),
            recent("/w/Elm", 5),
            recent("/w/Ash", 4),
            recent("/w/Fir", 3),
        ];
        let listed = titled(&recents, Path::new("/w/Open-one"), IN_MENU);
        let titles: Vec<_> = listed.iter().map(|(title, _)| title.as_str()).collect();
        assert_eq!(titles, ["site — /a", "site — /b", "Oak", "Elm", "Ash"]);
        assert_eq!(listed[0].1, PathBuf::from("/a/site"));
        assert_eq!(titled(&recents, Path::new(""), 2).into_iter().map(|(title, _)| title).collect::<Vec<_>>(), ["Open-one", "site"]);
    }

    #[test]
    fn a_palette_query_may_start_with_open() {
        assert_eq!(open_query("open aspen"), "aspen");
        assert_eq!(open_query("Open  Juniper"), "Juniper");
        assert_eq!(open_query("opener"), "opener");
        assert_eq!(open_query("aspen"), "aspen");
    }

    #[test]
    fn the_commands_are_where_the_plan_puts_them() {
        let open_recent = commands::find("repo.open-recent").expect("Open recent repository…");
        assert_eq!(open_recent.shortcut, Some("cmd-shift-o"));
        assert_eq!(open_recent.bar, None);
        let close = commands::find("repo.close").expect("Close repository");
        assert_eq!(close.bar, Some(Bar::File));
        assert!(view::REPO_MENU.contains(&"repo.close"));
        assert_eq!(commands::find("repo.open").and_then(|command| command.shortcut), Some("cmd-o"));
    }
}
