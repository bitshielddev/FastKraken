use crate::{
    Tab, kit,
    commands::{self, Entry, Group, Target},
    navigate,
    notice::Notice,
    refs::Kind,
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{
    App, Context, Entity, Focusable, ScrollStrategy, UniformListScrollHandle, Window, div, prelude::*, px,
    rgb, uniform_list,
};
use std::ops::Range;

pub(crate) const COMMANDS: &[commands::Command] = {
    use commands::{Availability, Bar, Command};
    &[Command::new(
        "app.palette",
        Group::App,
        |_, _| "Command palette…".into(),
        |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(view::Action::Palette(Action::Open(Mode::Commands))),
    )
    .icon("search")
    .keys("cmd-p")
    .bar(Bar::Help)
    .words(&["commands", "actions", "run"])]
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Commands,
    Recent,
}

impl Mode {
    fn placeholder(self) -> &'static str {
        match self {
            Self::Commands => "Type a command or a branch name",
            Self::Recent => "Type the name of a recent repository",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(Mode),
    Close,
    Choose(Option<usize>),
    Hover(usize),
}

pub(crate) struct State {
    open: bool,
    mode: Mode,
    pub field: Entity<CommitInput>,
    pick: Option<(String, usize)>,
    scroll: UniformListScrollHandle,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let field = cx.new(|cx| CommitInput::with_placeholder(cx, Mode::Commands.placeholder()));
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        Self { open: false, mode: Mode::Commands, field, pick: None, scroll: UniformListScrollHandle::new() }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn toggled_by(&self, id: &str) -> bool {
        self.open
            && matches!((id, self.mode), ("app.palette", Mode::Commands) | ("repo.open-recent", Mode::Recent))
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(mode) if tab.ui.palette.open && tab.ui.palette.mode == mode => close(tab, window),
        Action::Open(mode) => {
            tab.ui.menu = None;
            tab.ui.palette.open = true;
            tab.ui.palette.mode = mode;
            tab.ui.palette.pick = None;
            tab.ui.palette.field.update(cx, |field, cx| {
                field.clear(cx);
                field.set_placeholder(mode.placeholder(), cx);
            });
            tab.ui.palette.scroll.scroll_to_item(0, ScrollStrategy::Top);
            window.focus(&tab.ui.palette.field.focus_handle(cx));
        }
        Action::Close => close(tab, window),
        Action::Choose(index) => choose(tab, index, window, cx),
        Action::Hover(row) => {
            let pick = Some((typed(&tab.ui, cx), row));
            if tab.ui.palette.pick == pick {
                return;
            }
            tab.ui.palette.pick = pick;
        }
    }
    cx.notify();
}

fn close(tab: &mut Tab, window: &mut Window) {
    tab.ui.palette.open = false;
    view::focus(&tab.ui, window);
}

fn choose(tab: &mut Tab, index: Option<usize>, window: &mut Window, cx: &mut Context<Tab>) {
    let mut rows = rows(&tab.ui, cx);
    let Some(at) = index.or_else(|| highlighted(&tab.ui, rows.len(), cx)) else { return };
    if at >= rows.len() {
        return;
    }
    let row = rows.swap_remove(at);
    match row.action {
        Some(action) => {
            close(tab, window);
            tab.dispatch(action, window, cx);
        }
        None => {
            let reason = row.disabled.unwrap_or_else(|| "That command cannot run now".to_owned());
            tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
        }
    }
    cx.notify();
}

pub(crate) fn key(tab: &mut Tab, action: navigate::Action, window: &mut Window, cx: &mut Context<Tab>) {
    if action == navigate::Action::Choose {
        choose(tab, None, window, cx);
        return;
    }
    let len = rows(&tab.ui, cx).len();
    let current = highlighted(&tab.ui, len, cx);
    let Some(step) = navigate::how(action, PAGE) else { return };
    let Some(row) = navigate::moved(current, len, step) else { return };
    tab.ui.palette.scroll.scroll_to_item(row, ScrollStrategy::Center);
    tab.ui.palette.pick = Some((typed(&tab.ui, cx), row));
    cx.notify();
}

const PAGE: usize = 11;

fn typed(ui: &Ui, cx: &App) -> String {
    ui.palette.field.read(cx).text().trim().to_owned()
}

fn highlighted(ui: &Ui, len: usize, cx: &App) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let text = typed(ui, cx);
    Some(match &ui.palette.pick {
        Some((picked, row)) if *picked == text => (*row).min(len - 1),
        _ => 0,
    })
}

pub(crate) fn cursor(ui: &Ui, cx: &App) -> Option<usize> {
    highlighted(ui, rows(ui, cx).len(), cx)
}

pub(crate) fn score(query: &str, text: &str) -> Option<i32> {
    const HIT: i32 = 1;
    const WORD: i32 = 10;
    const NEXT: i32 = 8;
    const NONE: i32 = i32::MIN / 2;
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
    let Some((&first, rest)) = query.split_first() else { return Some(0) };
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().copied().map(fold).collect();
    let starts = |j: usize| {
        j == 0 || !chars[j - 1].is_alphanumeric() || (chars[j - 1].is_lowercase() && chars[j].is_uppercase())
    };
    let hit = |j: usize| HIT + if starts(j) { WORD } else { 0 };
    let mut best: Vec<i32> = (0..lower.len())
        .map(|j| if lower[j] == first { hit(j) - j.min(20) as i32 } else { NONE })
        .collect();
    for &letter in rest {
        let mut next = vec![NONE; lower.len()];
        let mut apart = NONE;
        for j in 1..lower.len() {
            if j >= 2 {
                apart = apart.max(best[j - 2]);
            }
            let from = (best[j - 1] + NEXT).max(apart);
            if lower[j] == letter && from > NONE / 2 {
                next[j] = from + hit(j);
            }
        }
        best = next;
    }
    best.into_iter().filter(|score| *score > NONE / 2).max()
}

fn command_score(query: &str, title: &str, words: &[&str]) -> Option<i32> {
    let by_word = words.iter().filter_map(|word| score(query, word)).max().map(|s| s - 1);
    score(query, title).max(by_word)
}

fn branch_query(query: &str) -> &str {
    let lower = query.to_lowercase();
    for lead in ["switch to ", "switch ", "checkout ", "check out "] {
        if lower.starts_with(lead) && query.is_char_boundary(lead.len()) {
            return query[lead.len()..].trim_start();
        }
    }
    query
}

fn rank<T>(mut rows: Vec<(i32, Group, bool, T)>, typed: bool) -> Vec<T> {
    if typed {
        rows.sort_by_key(|(score, _, disabled, _)| (-score, *disabled));
    } else {
        rows.sort_by_key(|(_, group, _, _)| *group);
    }
    rows.into_iter().map(|(_, _, _, row)| row).collect()
}

pub(crate) fn rows(ui: &Ui, cx: &App) -> Vec<Entry> {
    let query = typed(ui, cx);
    if ui.palette.mode == Mode::Recent {
        let scored = crate::start::palette_rows(ui, &query, cx)
            .into_iter()
            .map(|(score, entry)| (score, entry.group, false, entry))
            .collect();
        return rank(scored, !query.is_empty());
    }
    let targets = commands::targets(ui);
    let mut scored = Vec::new();
    for command in commands::all().filter(|command| command.id != "app.palette") {
        let Some(entry) = targets.iter().find_map(|target| {
            let mut entry = commands::entry(command, ui, target, cx)?;
            entry.title = commands::full_title(command, ui, target);
            Some(entry)
        }) else {
            continue;
        };
        if let Some(score) = command_score(&query, &entry.title, command.words) {
            scored.push((score, entry.group, entry.disabled.is_some(), entry));
        }
    }
    if !query.is_empty()
        && !ui.repo.is_empty()
        && let Some(switch) = commands::find("branch.switch")
    {
        let name = branch_query(&query);
        for item in ui.refs.items.iter().filter(|item| item.kind == Kind::Branch) {
            let Some(score) = score(name, &item.name.to_string_lossy()) else { continue };
            if let Some(entry) = commands::entry(switch, ui, &Target::Branch(item.name.clone()), cx) {
                scored.push((score, entry.group, entry.disabled.is_some(), entry));
            }
        }
    }
    if !query.is_empty() {
        for (score, entry) in crate::start::palette_rows(ui, &query, cx) {
            scored.push((score, entry.group, false, entry));
        }
    }
    rank(scored, !query.is_empty())
}

const LIST_ROWS: usize = 12;

pub(crate) fn render<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    if !ui.palette.open {
        return None;
    }
    let shown = rows(ui, cx);
    let count = shown.len();
    let at = highlighted(ui, count, cx);
    let text = typed(ui, cx);
    let height = px(ROW * LIST_ROWS as f32 + 8.);
    let list = if count == 0 {
        let empty = match ui.palette.mode {
            Mode::Commands => format!("No command or branch matches “{text}”."),
            Mode::Recent if text.is_empty() => "There are no other recent repositories yet.".to_owned(),
            Mode::Recent => format!("No recent repository matches “{text}”."),
        };
        div().h(height).p_3().child(kit::muted(empty))
    } else {
        div().h(height).py_1().flex().flex_col().child(
            uniform_list(
                "palette-rows",
                count,
                cx.processor(|app, range: Range<usize>, _, cx| {
                    let ui = app.ui();
                    let rows = rows(ui, cx);
                    let at = highlighted(ui, rows.len(), cx);
                    rows.into_iter()
                        .enumerate()
                        .skip(range.start)
                        .take(range.len())
                        .map(|(index, entry)| row(entry, index, at == Some(index), cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(ui.palette.scroll.clone())
            .flex_1()
            .min_h_0(),
        )
    };
    let reason = at.and_then(|at| shown.into_iter().nth(at)).and_then(|entry| entry.disabled);
    let footer = div()
        .h(px(ROW))
        .px_3()
        .flex()
        .items_center()
        .border_t_1()
        .border_color(rgb(LINE))
        .text_size(px(TEXT_SMALL))
        .child(match reason {
            Some(reason) => kit::fit(reason).size(TEXT_SMALL).color(TEXT_2).grow().into_any_element(),
            None => kit::fit("↑↓ to move, ↩ to run, esc to close")
                .size(TEXT_SMALL)
                .color(TEXT_3)
                .grow()
                .into_any_element(),
        });
    let surface = kit::float()
        .w(px(560.))
        .rounded(px(12.))
        .flex()
        .flex_col()
        .child(
            div()
                .p_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .key_context("CommandPalette")
                .child(ui.palette.field.clone()),
        )
        .child(list)
        .child(footer);
    Some(kit::modal(surface, Some(view::Action::Palette(Action::Close)), &ui.modal_focus, cx))
}

fn meta(disabled: Option<&str>, shortcut: Option<&str>) -> Option<String> {
    const LONGEST: usize = 48;
    let reason = disabled.map(|reason| match reason.char_indices().nth(LONGEST) {
        Some((cut, _)) => format!("{}…", reason[..cut].trim_end()),
        None => reason.to_owned(),
    });
    let keys = shortcut.map(commands::keys);
    match (reason, keys) {
        (Some(reason), Some(keys)) => Some(format!("{reason}   {keys}")),
        (reason, keys) => reason.or(keys),
    }
}

fn row<T: Desk>(entry: Entry, index: usize, highlighted: bool, cx: &Context<T>) -> gpui::Stateful<gpui::Div> {
    let mut item = kit::item(("palette-row", index), entry.title)
        .enabled(entry.action.is_some())
        .highlighted(highlighted);
    if let Some(text) = meta(entry.disabled.as_deref(), entry.shortcut) {
        item = item.meta(text);
    }
    item = item.tip(entry.disabled.map(Into::into));
    item = match entry.icon {
        Some(icon) => item.icon(icon),
        None => item.check(false),
    };
    if entry.danger {
        item = item.danger();
    }
    item.inert()
        .on_click(cx.listener(move |app, _, w, cx| {
            app.dispatch(view::Action::Palette(Action::Choose(Some(index))), w, cx)
        }))
        .on_hover(cx.listener(move |app, hovered: &bool, w, cx| {
            if *hovered {
                app.dispatch(view::Action::Palette(Action::Hover(index)), w, cx);
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best<'a>(query: &str, titles: &[&'a str]) -> Vec<&'a str> {
        let scored = titles
            .iter()
            .filter_map(|title| Some((score(query, title)?, Group::Repository, false, *title)))
            .collect();
        rank(scored, true)
    }

    #[test]
    fn letters_must_come_in_order() {
        assert!(score("fetch", "Fetch all remotes").is_some());
        assert!(score("fch", "Fetch all remotes").is_some());
        assert!(score("hcf", "Fetch all remotes").is_none());
        assert!(score("pushx", "Push").is_none());
        assert!(score("FETCH", "fetch all remotes").is_some());
        assert!(score("fetch all", "Fetch all remotes").is_some());
        assert_eq!(score("", "Anything"), Some(0));
        assert_eq!(score("x", ""), None);
        assert!(score("İstanbul", "feature/İstanbul").is_some());
        assert!(score("İ", "İ").is_some());
    }

    #[test]
    fn word_starts_and_runs_rank_first() {
        let titles = ["Pop stash", "Stash changes", "Stage all", "Unstage all", "Show diffs side by side"];
        assert_eq!(best("st", &titles)[..2], ["Stash changes", "Stage all"]);
        assert_eq!(best("sa", &titles)[0], "Stage all");
        assert_eq!(best("ua", &titles)[0], "Unstage all");
        assert_eq!(best("sds", &titles)[0], "Show diffs side by side");
        assert_eq!(
            best("fetch", &["Switch branch…", "Fetch all remotes", "Refresh", "Find commits…"]),
            ["Fetch all remotes"]
        );
        assert!(score("fk", "About FastKraken") > score("fk", "About Fastkraken"));
    }

    #[test]
    fn hidden_words_match_just_below_the_title() {
        assert!(command_score("finder", "Reveal in Finder", &[]).is_some());
        assert!(command_score("shell", "Open in Terminal", &["shell"]).is_some());
        assert!(command_score("shell", "Open in Terminal", &[]).is_none());
        let by_title = command_score("push", "Push", &[]).unwrap();
        let by_word = command_score("push", "Publish branch…", &["push"]).unwrap();
        assert!(by_title > by_word);
    }

    #[test]
    fn dimmed_rows_follow_enabled_ones_at_an_equal_score_and_menus_keep_groups() {
        let rows = vec![
            (5, Group::Sync, true, "pull"),
            (5, Group::Sync, false, "push"),
            (9, Group::Copy, true, "copy"),
        ];
        assert_eq!(rank(rows.clone(), true), ["copy", "push", "pull"]);
        assert_eq!(rank(rows, false), ["pull", "push", "copy"]);
    }

    #[test]
    fn a_dimmed_row_shows_its_reason_before_its_keys() {
        assert_eq!(meta(None, Some("cmd-l")).as_deref(), Some("⌘L"));
        assert_eq!(meta(None, None), None);
        assert_eq!(
            meta(Some("This repository has no remote to fetch from"), Some("cmd-l")).as_deref(),
            Some("This repository has no remote to fetch from   ⌘L")
        );
        let long = "a reason that goes on and on well past what fits beside a title";
        let shown = meta(Some(long), None).unwrap();
        assert!(shown.ends_with('…') && shown.chars().count() <= 49, "{shown}");
    }

    #[test]
    fn a_branch_query_may_start_with_switch_or_checkout() {
        assert_eq!(branch_query("switch to feature/map"), "feature/map");
        assert_eq!(branch_query("Checkout main"), "main");
        assert_eq!(branch_query("feature"), "feature");
        assert_eq!(branch_query("switchboard"), "switchboard");
    }

    #[test]
    fn the_palette_is_in_the_help_menu_on_cmd_p_with_hidden_words_where_they_help() {
        let palette = commands::find("app.palette").expect("the palette's command");
        assert_eq!(palette.shortcut, Some("cmd-p"));
        assert_eq!(palette.bar, Some(commands::Bar::Help));
        for (id, title, word) in [
            ("repo.reveal", "Reveal in Finder", "file manager"),
            ("file.reveal", "Reveal in Finder", "file manager"),
            ("repo.terminal", "Open in Terminal", "shell"),
            ("repo.switch-branch", "Switch branch…", "checkout"),
            ("app.shortcuts", "Keyboard shortcuts", "hotkeys"),
            ("repo.find", "Find commits…", "search"),
            ("repo.fetch-all", "Fetch all remotes", "download"),
        ] {
            let command = commands::find(id).unwrap_or_else(|| panic!("{id}"));
            assert!(score(word, title).is_none(), "{word} already matches {title}");
            assert!(command_score(word, title, command.words).is_some(), "{word} does not find {id}");
        }
    }
}
