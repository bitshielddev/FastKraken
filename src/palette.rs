use crate::{
    Tab, kit,
    commands::{self, Availability, Entry, Group, Side, Target, on_repo},
    compare::Rev,
    jobs::Link,
    navigate,
    notice::Notice,
    refs::Kind,
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{
    App, Context, Entity, Focusable, ScrollStrategy, UniformListScrollHandle, Window, div, prelude::*,
    rgb, uniform_list,
};
use crate::theme::px;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) const LIMIT: usize = 200_000;
const NO_COMMITS: &str = "There are no commits yet";

fn by_name(ui: &Ui, target: &Target, needs_commits: bool) -> Availability {
    if !on_repo(ui, target) {
        Availability::Hidden
    } else if needs_commits && ui.head.is_none() {
        Availability::Disabled(NO_COMMITS.to_owned())
    } else {
        Availability::Enabled
    }
}

pub(crate) const COMMANDS: &[commands::Command] = {
    use commands::{Bar, Command};
    &[
        Command::new(
            "app.palette",
            Group::App,
            |_, _| "Command palette…".into(),
            |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
            |_, _| Some(view::Action::Palette(Action::Open(Mode::Commands))),
        )
        .icon("search")
        .keys("cmd-p")
        .bar(Bar::Help)
        .words(&["commands", "actions", "run"]),
        Command::new(
            "file.open-by-name",
            Group::View,
            |_, _| "Open file…".into(),
            |ui, target, _| by_name(ui, target, false),
            |_, _| Some(view::Action::Palette(Action::Open(Mode::Files(Goal::Open)))),
        )
        .icon("file")
        .words(&["view file", "go to file", "find a file", "show file contents"]),
        Command::new(
            "file.history-by-name",
            Group::View,
            |_, _| "File history…".into(),
            |ui, target, _| by_name(ui, target, true),
            |_, _| Some(view::Action::Palette(Action::Open(Mode::Files(Goal::History)))),
        )
        .icon("history")
        .keys("cmd-shift-h")
        .words(&["history of a file", "log of a file", "commits that changed a file"]),
        Command::new(
            "file.blame-by-name",
            Group::View,
            |_, _| "Blame file…".into(),
            |ui, target, _| by_name(ui, target, true),
            |_, _| Some(view::Action::Palette(Action::Open(Mode::Files(Goal::Blame)))),
        )
        .icon("commit")
        .words(&["annotate a file", "who changed a file", "praise"]),
    ]
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Goal {
    Open,
    History,
    Blame,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Commands,
    Recent,
    Files(Goal),
}

impl Mode {
    fn placeholder(self) -> &'static str {
        match self {
            Self::Commands => "Type a command or a branch name",
            Self::Recent => "Type the name of a recent repository",
            Self::Files(Goal::Open) => "Type the name of a file to open",
            Self::Files(Goal::History) => "Type the name of a file to see its history",
            Self::Files(Goal::Blame) => "Type the name of a file to blame",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) path: PathBuf,
    text: String,
    name: usize,
}

impl Found {
    pub(crate) fn new(path: PathBuf) -> Self {
        let text = path.to_string_lossy().into_owned();
        let name = text.rfind('/').map_or(0, |at| at + 1);
        Self { path, text, name }
    }

    fn name(&self) -> &str {
        &self.text[self.name..]
    }

    fn folder(&self) -> &str {
        self.text[..self.name].trim_end_matches('/')
    }
}

#[derive(Default)]
pub(crate) struct Finder {
    ticket: u64,
    paths: Arc<Vec<Found>>,
    loading: bool,
    failed: Option<String>,
    capped: bool,
    query: Option<String>,
    ranked: Vec<u32>,
    link: Option<Link>,
}

impl Finder {
    pub(crate) fn loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn count(&self) -> usize {
        self.ranked.len()
    }

    pub(crate) fn at(&self, row: usize) -> Option<&Path> {
        let index = *self.ranked.get(row)? as usize;
        self.paths.get(index).map(|found| found.path.as_path())
    }

    fn stop(&mut self) {
        if let Some(link) = self.link.take() {
            link.cancel();
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn ranked(paths: &[Found], query: &str, within: Option<&[u32]>) -> Vec<u32> {
    let query = query.trim();
    if query.is_empty() {
        return (0..paths.len() as u32).collect();
    }
    let score_of = |index: u32| {
        let found = paths.get(index as usize)?;
        Some((command_score(query, &found.text, &[found.name()])?, index))
    };
    let mut scored: Vec<(i32, u32)> = match within {
        Some(within) => within.iter().filter_map(|&index| score_of(index)).collect(),
        None => (0..paths.len() as u32).filter_map(score_of).collect(),
    };
    scored.sort_unstable_by_key(|&(score, index)| (-score, index));
    scored.into_iter().map(|(_, index)| index).collect()
}

fn rerank(tab: &mut Tab, cx: &App) {
    if !matches!(tab.ui.palette.mode, Mode::Files(_)) || !tab.ui.palette.open {
        return;
    }
    let text = typed(&tab.ui, cx);
    let finder = &mut tab.ui.palette.finder;
    if finder.loading || finder.query.as_deref() == Some(text.as_str()) {
        return;
    }
    let narrowed = finder.query.as_deref().is_some_and(|before| !before.is_empty() && text.starts_with(before));
    let within = narrowed.then(|| std::mem::take(&mut finder.ranked));
    finder.ranked = ranked(&finder.paths, &text, within.as_deref());
    finder.query = Some(text);
}

fn load_files(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let finder = &mut tab.ui.palette.finder;
    finder.stop();
    finder.ticket += 1;
    finder.loading = true;
    finder.failed = None;
    finder.paths = Arc::default();
    finder.ranked.clear();
    finder.query = None;
    let link = Link::new(true);
    finder.link = Some(link.clone());
    let ticket = finder.ticket;
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || repo.tracked_paths(&link, LIMIT).map(|(paths, capped)| (paths.into_iter().map(Found::new).collect::<Vec<_>>(), capped)),
        move |tab, result, cx| {
            let finder = &mut tab.ui.palette.finder;
            if tab.epoch != epoch || finder.ticket != ticket {
                return;
            }
            finder.loading = false;
            finder.link = None;
            match result {
                Ok((paths, capped)) => {
                    finder.paths = Arc::new(paths);
                    finder.capped = capped;
                }
                Err(error) => finder.failed = Some(error),
            }
            finder.query = None;
            finder.ranked.clear();
            rerank(tab, cx);
            cx.notify();
        },
    );
}

fn file_action(ui: &Ui, goal: Goal, path: &Path) -> Option<view::Action> {
    let unstaged = Side::Unstaged;
    match goal {
        Goal::Open => Some(view::Action::Browse(crate::browse::Action::Reveal(Rev::WorkingTree, path.to_path_buf()))),
        Goal::History => crate::file_history::request(ui, path, &unstaged)
            .map(|request| view::Action::FileHistory(crate::file_history::Action::Open(request))),
        Goal::Blame => crate::blame::request(ui, path, &unstaged)
            .map(|request| view::Action::Blame(crate::blame::Action::Open(request))),
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
    pub(crate) finder: Finder,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let field = cx.new(|cx| CommitInput::with_placeholder(cx, Mode::Commands.placeholder()));
        cx.observe(&field, |tab, _, cx| {
            rerank(tab, cx);
            cx.notify()
        })
        .detach();
        Self { open: false, mode: Mode::Commands, field, pick: None, scroll: UniformListScrollHandle::new(), finder: Finder::default() }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn toggled_by(&self, id: &str) -> bool {
        self.open
            && matches!(
                (id, self.mode),
                ("app.palette", Mode::Commands)
                    | ("repo.open-recent", Mode::Recent)
                    | ("file.open-by-name", Mode::Files(Goal::Open))
                    | ("file.history-by-name", Mode::Files(Goal::History))
                    | ("file.blame-by-name", Mode::Files(Goal::Blame))
            )
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
            if matches!(mode, Mode::Files(_)) {
                load_files(tab, cx);
            } else {
                tab.ui.palette.finder = Finder::default();
            }
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
    tab.ui.palette.finder.stop();
    view::focus(&tab.ui, window);
}

fn choose(tab: &mut Tab, index: Option<usize>, window: &mut Window, cx: &mut Context<Tab>) {
    if let Mode::Files(goal) = tab.ui.palette.mode {
        let len = tab.ui.palette.finder.count();
        let Some(at) = index.or_else(|| highlighted(&tab.ui, len, cx)) else { return };
        let Some(path) = tab.ui.palette.finder.at(at).map(Path::to_path_buf) else { return };
        close(tab, window);
        tab.ui.palette.finder = Finder::default();
        match file_action(&tab.ui, goal, &path) {
            Some(action) => tab.dispatch(action, window, cx),
            None => {
                tab.ui.notices.show(Notice::new(Tone::Info, "That file cannot be opened that way", ""));
            }
        }
        cx.notify();
        return;
    }
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
    let len = count(&tab.ui, cx);
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
    highlighted(ui, count(ui, cx), cx)
}

fn count(ui: &Ui, cx: &App) -> usize {
    match ui.palette.mode {
        Mode::Files(_) => ui.palette.finder.count(),
        _ => rows(ui, cx).len(),
    }
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
    if matches!(ui.palette.mode, Mode::Files(_)) {
        return Vec::new();
    }
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

fn file_row<T: Desk>(found: &Found, index: usize, highlighted: bool, cx: &Context<T>) -> gpui::Stateful<gpui::Div> {
    let mut item = kit::item(("palette-file", index), found.name().to_owned()).highlighted(highlighted).icon("file");
    if !found.folder().is_empty() {
        item = item.meta(found.folder().to_owned());
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

pub(crate) fn render<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    if !ui.palette.open {
        return None;
    }
    let files = matches!(ui.palette.mode, Mode::Files(_));
    let shown = rows(ui, cx);
    let count = if files { ui.palette.finder.count() } else { shown.len() };
    let at = highlighted(ui, count, cx);
    let text = typed(ui, cx);
    let height = px(ROW * LIST_ROWS as f32 + 8.);
    let finder = &ui.palette.finder;
    let list = if count == 0 {
        let empty = match ui.palette.mode {
            Mode::Commands => format!("No command or branch matches “{text}”."),
            Mode::Recent if text.is_empty() => "There are no other recent repositories yet.".to_owned(),
            Mode::Recent => format!("No recent repository matches “{text}”."),
            Mode::Files(_) if finder.loading => "Reading the list of files…".to_owned(),
            Mode::Files(_) => match &finder.failed {
                Some(error) => format!("The list of files could not be read. {error}"),
                None if text.is_empty() => "This repository has no tracked files.".to_owned(),
                None => format!("No file matches “{text}”."),
            },
        };
        div().h(height).p_3().child(kit::muted(empty))
    } else if files {
        div().h(height).py_1().flex().flex_col().child(
            uniform_list(
                "palette-files",
                count,
                cx.processor(|app, range: Range<usize>, _, cx| {
                    let ui = app.ui();
                    let finder = &ui.palette.finder;
                    let at = highlighted(ui, finder.count(), cx);
                    range
                        .filter_map(|row| {
                            let index = *finder.ranked.get(row)? as usize;
                            Some(file_row(finder.paths.get(index)?, row, at == Some(row), cx))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(ui.palette.scroll.clone())
            .flex_1()
            .min_h_0(),
        )
        .child(kit::scrollbar("palette-scrollbar", &ui.palette.scroll))
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
        .child(kit::scrollbar("palette-scrollbar", &ui.palette.scroll))
    };
    let reason = at.and_then(|at| shown.into_iter().nth(at)).and_then(|entry| entry.disabled);
    let footer = div()
        .h(whole_px(ROW))
        .px_3()
        .flex()
        .items_center()
        .border_t_1()
        .border_color(rgb(palette().line))
        .text_size(px(TEXT_SMALL))
        .child(match reason {
            Some(reason) => kit::fit(reason).size(TEXT_SMALL).color(palette().text_2).grow().into_any_element(),
            None if files && finder.capped => kit::fit("Only the first 200,000 files are listed")
                .size(TEXT_SMALL)
                .color(palette().text_2)
                .grow()
                .into_any_element(),
            None if files => kit::fit("↑↓ to move, ↩ to open, esc to close")
                .size(TEXT_SMALL)
                .color(palette().text_3)
                .grow()
                .into_any_element(),
            None => kit::fit("↑↓ to move, ↩ to run, esc to close")
                .size(TEXT_SMALL)
                .color(palette().text_3)
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
                .border_color(rgb(palette().line))
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

    fn found(paths: &[&str]) -> Vec<Found> {
        paths.iter().map(|path| Found::new(PathBuf::from(path))).collect()
    }

    fn named(paths: &[Found], ranked: &[u32]) -> Vec<String> {
        ranked.iter().map(|&index| paths[index as usize].text.clone()).collect()
    }

    #[test]
    fn files_rank_like_commands_with_the_file_name_as_a_hidden_word_and_narrowing_keeps_the_order() {
        let paths = found(&["docs/trail-guide.md", "src/camp/trail.rs", "src/trailhead/map.rs", "trail.txt", "notes/old.txt"]);
        assert_eq!(named(&paths, &ranked(&paths, "", None)), [
            "docs/trail-guide.md",
            "src/camp/trail.rs",
            "src/trailhead/map.rs",
            "trail.txt",
            "notes/old.txt",
        ]);
        let trail = ranked(&paths, "trail", None);
        assert_eq!(named(&paths, &trail)[0], "trail.txt");
        assert!(!named(&paths, &trail).contains(&"notes/old.txt".to_owned()));
        assert_eq!(named(&paths, &ranked(&paths, "map", None)), ["src/trailhead/map.rs"]);
        let narrowed = ranked(&paths, "trail.r", Some(&trail));
        assert_eq!(narrowed, ranked(&paths, "trail.r", None));
        assert_eq!(named(&paths, &narrowed)[0], "src/camp/trail.rs");
        assert!(ranked(&paths, "zzz", None).is_empty());
        let one = Found::new(PathBuf::from("src/camp/trail.rs"));
        assert_eq!((one.name(), one.folder()), ("trail.rs", "src/camp"));
        let top = Found::new(PathBuf::from("trail.txt"));
        assert_eq!((top.name(), top.folder()), ("trail.txt", ""));
    }

    #[test]
    fn the_file_finders_are_in_the_palette_with_cmd_shift_h_for_history() {
        for (id, title) in [("file.open-by-name", "Open file…"), ("file.history-by-name", "File history…"), ("file.blame-by-name", "Blame file…")] {
            let command = commands::find(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(command.bar, None, "{id}");
            assert!(command_score("file", title, command.words).is_some(), "{id}");
        }
        assert_eq!(commands::find("file.history-by-name").and_then(|command| command.shortcut), Some("cmd-shift-h"));
        assert_eq!(commands::all().filter(|command| command.shortcut == Some("cmd-shift-h")).count(), 1);
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
