use crate::{
    Tab, kit,
    commands::{Availability, Bar, Command, Group, Target, on_repo},
    refs::{Kind, NamedRef, Section},
    settings::Memory,
    storage,
    theme::*,
    view::{self, CommitInput, Desk, Selection, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
};

pub(crate) const BRANCHES: &str = "refs/heads/";
pub(crate) const REMOTES: &str = "refs/remotes/";
pub(crate) const TAGS: &str = "refs/tags/";
pub(crate) const SHOWN_AUTHORS: usize = 10;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Person {
    pub(crate) name: String,
    pub(crate) email: String,
}

impl Person {
    pub(crate) fn wrote(&self, author: &str, email: &str) -> bool {
        if self.email.is_empty() { author == self.name } else { email.eq_ignore_ascii_case(&self.email) }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct GraphView {
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) hidden: BTreeSet<String>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) solo: BTreeSet<String>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) only_current: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) hide_stashes: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) author: Option<Person>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) pin: crate::pin::Pin,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) stale_months: Option<u32>,
    #[serde(skip)]
    pub(crate) upstream: Option<String>,
    #[serde(skip)]
    pub(crate) stale: BTreeSet<String>,
    #[serde(skip)]
    pub(crate) bodies: bool,
}

pub(crate) const STALE_MONTHS: [u32; 4] = [1, 3, 6, 12];
const MONTH: i64 = 2_629_746;

pub(crate) fn stale(refs: &[NamedRef], months: Option<u32>, keep: Option<&str>, now: i64) -> BTreeSet<String> {
    let Some(months) = months else { return BTreeSet::new() };
    let cutoff = now - i64::from(months) * MONTH;
    refs.iter()
        .filter(|item| item.kind == Kind::Remote && item.time > 0 && item.time < cutoff)
        .map(|item| format!("{REMOTES}{}", item.name.to_string_lossy()))
        .filter(|name| hideable(name) && keep != Some(name.as_str()))
        .collect()
}

fn months_text(months: u32) -> String {
    if months == 1 { "a month".to_owned() } else { format!("{months} months") }
}

fn exact_glob(name: &str) -> String {
    match name.char_indices().rev().find(|(_, c)| c.is_ascii_alphanumeric()) {
        Some((at, c)) => format!("--glob={}[{c}]{}", &name[..at], &name[at + 1..]),
        None => format!("--glob={name}"),
    }
}

fn pattern(name: &str) -> String {
    match name.strip_suffix('/') {
        Some(folder) => format!("{folder}/*"),
        None => name.to_owned(),
    }
}

fn covers(entry: &str, full: &str) -> bool {
    entry == full || (entry.ends_with('/') && full.starts_with(entry))
}

impl GraphView {
    pub(crate) fn filtered(&self) -> bool {
        !self.hidden.is_empty() || !self.stale.is_empty() || !self.solo.is_empty() || self.only_current
    }

    pub(crate) fn active(&self) -> bool {
        self.filtered() || self.hide_stashes || self.author.is_some() || self.stale_months.is_some()
    }

    pub(crate) fn listing(&self) -> (&BTreeSet<String>, &BTreeSet<String>, &BTreeSet<String>, bool, Option<&String>) {
        (&self.hidden, &self.stale, &self.solo, self.only_current, self.upstream.as_ref().filter(|_| self.only_current))
    }

    pub(crate) fn history_args(&self) -> Vec<OsString> {
        let excludes: Vec<OsString> = self
            .hidden
            .iter()
            .chain(self.stale.iter().filter(|name| self.hidden_by(name).is_none()))
            .map(|name| format!("--exclude={}", pattern(name)).into())
            .collect();
        let mut args: Vec<OsString> = Vec::new();
        if self.only_current {
            if let Some(upstream) = &self.upstream {
                args.extend(excludes.iter().cloned());
                args.push(exact_glob(upstream).into());
            }
        } else if !self.solo.is_empty() {
            for name in &self.solo {
                args.extend(excludes.iter().cloned());
                args.push(match name.strip_suffix('/') {
                    Some(folder) => format!("--glob={folder}/*").into(),
                    None => exact_glob(name).into(),
                });
            }
        } else {
            args.extend(excludes);
            args.push("--exclude=refs/stash".into());
            args.push(crate::journal::HIDDEN.into());
            args.push("--all".into());
        }
        if self.filtered() {
            args.push("--ignore-missing".into());
            args.push("HEAD".into());
        }
        args
    }

    pub(crate) fn hidden_by(&self, full: &str) -> Option<&str> {
        self.hidden.iter().find(|entry| covers(entry, full)).map(String::as_str)
    }

    pub(crate) fn soloed(&self, full: &str) -> bool {
        self.solo.iter().any(|entry| covers(entry, full))
    }

    pub(crate) fn shows(&self, full: &str, current: Option<&str>) -> bool {
        if self.hidden_by(full).is_some() || self.stale.contains(full) {
            return false;
        }
        if self.only_current {
            return current.is_some_and(|branch| full.strip_prefix(BRANCHES) == Some(branch))
                || self.upstream.as_deref() == Some(full);
        }
        self.solo.is_empty() || self.soloed(full)
    }

    pub(crate) fn dims(&self, author: &str, email: &str) -> bool {
        self.author.as_ref().is_some_and(|person| !person.wrote(author, email))
    }

    pub(crate) fn sanitized(mut self) -> Self {
        self.hidden.retain(|name| hideable(name));
        self.solo.retain(|name| hideable(name) && !self.hidden.contains(name));
        self.upstream = None;
        self.stale.clear();
        self.stale_months = self.stale_months.filter(|months| STALE_MONTHS.contains(months));
        if let crate::pin::Pin::Ref(name) = &self.pin {
            if !crate::pin::pinnable(name) {
                self.pin = crate::pin::Pin::Head;
            }
        }
        self
    }

    fn stored(&self) -> Option<Self> {
        let kept = Self { upstream: None, stale: BTreeSet::new(), bodies: false, ..self.clone() };
        (kept != Self::default()).then_some(kept)
    }
}

pub(crate) fn hideable(name: &str) -> bool {
    name.strip_prefix("refs/").is_some_and(|rest| !rest.is_empty())
        && name != "refs/stash"
        && !name.contains(['*', '?', '[', '\\', ' ', '~', '^', ':'])
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

pub(crate) fn full_name(target: &Target) -> Option<String> {
    match target {
        Target::Branch(name) => Some(format!("{BRANCHES}{}", name.to_string_lossy())),
        Target::RemoteBranch(name) => Some(format!("{REMOTES}{}", name.to_string_lossy())),
        Target::Tag(name) => Some(format!("{TAGS}{}", name.to_string_lossy())),
        Target::Remote(name) => Some(format!("{REMOTES}{name}/")),
        _ => None,
    }
}

pub(crate) fn of_ref(kind: Kind, name: &str) -> String {
    match kind {
        Kind::Branch => format!("{BRANCHES}{name}"),
        Kind::Remote => format!("{REMOTES}{name}"),
        Kind::Tag => format!("{TAGS}{name}"),
    }
}

pub(crate) fn section_prefix(section: Section) -> Option<&'static str> {
    match section {
        Section::Branches => Some(BRANCHES),
        Section::Remotes => Some(REMOTES),
        Section::Tags => Some(TAGS),
        Section::Stashes | Section::Worktrees => None,
    }
}

pub(crate) fn current(ui: &Ui) -> Option<&str> {
    (!ui.detached && !ui.branch.is_empty()).then_some(ui.branch.as_str())
}

pub(crate) fn query(ui: &Ui) -> GraphView {
    let mut view = ui.graph_view.clone();
    let tracked = current(ui)
        .and_then(|branch| ui.refs.branch(branch))
        .and_then(|item| item.upstream.as_ref())
        .map(|upstream| format!("{REMOTES}{upstream}"));
    view.stale = stale(&ui.refs.items, view.stale_months, tracked.as_deref(), view::now());
    view.upstream = tracked;
    view.bodies = ui.bodies;
    view
}

fn short(entry: &str) -> String {
    let name = entry.trim_end_matches('/');
    [BRANCHES, REMOTES, TAGS]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix.trim_end_matches('/')).map(|rest| rest.trim_start_matches('/')))
        .unwrap_or(name)
        .to_owned()
}

fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

fn named(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third] => format!("{first}, {second} and {third}"),
        [first, second, rest @ ..] => format!("{first}, {second} and {} more", rest.len()),
    }
}

pub(crate) fn summary(ui: &Ui) -> Vec<String> {
    let view = &ui.graph_view;
    let mut parts = Vec::new();
    let (mut branches, mut tags, mut remotes) = (0, 0, Vec::new());
    for entry in &view.hidden {
        match entry.as_str() {
            BRANCHES => parts.push("all local branches hidden".to_owned()),
            REMOTES => parts.push("all remotes hidden".to_owned()),
            TAGS => parts.push("all tags hidden".to_owned()),
            folder if folder.ends_with('/') => remotes.push(short(folder)),
            tag if tag.starts_with(TAGS) => tags += 1,
            _ => branches += 1,
        }
    }
    if branches > 0 {
        parts.push(format!("{} hidden", counted(branches, "branch", "branches")));
    }
    if tags > 0 {
        parts.push(format!("{} hidden", counted(tags, "tag", "tags")));
    }
    match remotes.as_slice() {
        [] => {}
        [one] => parts.push(format!("remote {one} hidden")),
        many => parts.push(format!("{} hidden", counted(many.len(), "remote", "remotes"))),
    }
    if view.only_current {
        let branch = current(ui).unwrap_or("HEAD");
        parts.push(match query(ui).upstream {
            Some(_) => format!("showing only {branch} and its upstream"),
            None => format!("showing only {branch}"),
        });
    } else if !view.solo.is_empty() {
        let names: Vec<String> = view.solo.iter().map(|entry| short(entry)).collect();
        parts.push(format!("showing only {}", named(&names)));
    }
    if let (Some(months), false) = (view.stale_months, view.stale.is_empty()) {
        let count = counted(view.stale.len(), "remote branch", "remote branches");
        parts.push(format!("{count} with no commits in {} hidden", months_text(months)));
    }
    if view.hide_stashes {
        parts.push("stashes hidden".to_owned());
    }
    if let Some(person) = &view.author {
        parts.push(format!("commits by {}", person.name));
    }
    parts
}

pub(crate) fn authors(ui: &Ui) -> Vec<Person> {
    let mut counted: HashMap<String, (usize, usize, Person)> = HashMap::new();
    for (index, row) in ui.history.iter().enumerate() {
        if row.author.trim().is_empty() {
            continue;
        }
        let key = if row.email.is_empty() { row.author.clone() } else { row.email.to_lowercase() };
        let entry = counted
            .entry(key)
            .or_insert_with(|| (0, index, Person { name: row.author.clone(), email: row.email.clone() }));
        entry.0 += 1;
    }
    let mut listed: Vec<_> = counted.into_values().collect();
    listed.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    listed.into_iter().map(|(_, _, person)| person).collect()
}

pub(crate) fn matching(ui: &Ui, cx: &App) -> Vec<Person> {
    let query = ui.author_picker.filter.read(cx).text().trim().to_lowercase();
    authors(ui)
        .into_iter()
        .filter(|person| {
            query.split_whitespace().all(|word| {
                person.name.to_lowercase().contains(word) || person.email.to_lowercase().contains(word)
            })
        })
        .collect()
}

pub(crate) struct Picker {
    open: bool,
    pub(crate) filter: Entity<CommitInput>,
    pick: Option<(String, usize)>,
}

impl Picker {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter authors"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self { open: false, filter, pick: None }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }
}

fn highlighted(ui: &Ui, cx: &App) -> Option<usize> {
    let shown = matching(ui, cx).len().min(SHOWN_AUTHORS);
    if shown == 0 {
        return None;
    }
    let text = ui.author_picker.filter.read(cx).text();
    Some(match &ui.author_picker.pick {
        Some((typed, row)) if typed == text => (*row).min(shown - 1),
        _ => 0,
    })
}

#[derive(Clone, Debug)]
pub enum Action {
    Hide(String),
    HideEach(Vec<String>),
    SoloEach(Vec<String>),
    Show(String),
    ShowAll(String),
    Solo(String),
    Unsolo(String),
    StopSoloing,
    ToggleCurrent,
    ToggleStashes,
    Stale(Option<u32>),
    OpenAuthors,
    CloseAuthors,
    Author(Option<Person>),
    Clear,
}

pub(crate) fn remember(tab: &Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    let stored = tab.ui.graph_view.stored();
    Memory::update(cx, |memory| memory.change_repo(&common, |view| view.graph = stored));
}

pub(crate) fn restore(tab: &mut Tab, cx: &App) {
    let Some(repo) = &tab.repo else { return };
    tab.ui.graph_view = crate::settings::memory(cx)
        .and_then(|memory| memory.repo(repo.common_dir()))
        .and_then(|view| view.graph.clone())
        .map(GraphView::sanitized)
        .unwrap_or_default();
}

pub(crate) fn place_stashes(ui: &mut Ui) {
    if ui.graph_view.hide_stashes {
        ui.stash.unplace();
    } else {
        ui.stash.place(&ui.history, 0);
    }
}

pub(crate) fn follow_upstream(tab: &mut Tab, cx: &mut Context<Tab>) {
    let wanted = query(&tab.ui);
    let view = &tab.ui.graph_view;
    if (view.only_current && wanted.upstream != view.upstream) || wanted.stale != view.stale {
        tab.load_history(true, cx);
    }
}

fn change(tab: &mut Tab, widened: bool, window: &mut Window, cx: &mut Context<Tab>, edit: impl FnOnce(&mut GraphView)) {
    let before = tab.ui.graph_view.clone();
    edit(&mut tab.ui.graph_view);
    tab.ui.graph_view.stale = query(&tab.ui).stale;
    let view = &tab.ui.graph_view;
    if *view == before {
        return;
    }
    if view.listing() != before.listing() {
        tab.load_history(true, cx);
        if widened {
            if tab.loading_page {
                crate::reveal::reloading(tab);
            }
            if let Some(Selection::Commit(oid)) = tab.ui.selection.clone() {
                crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
            }
        }
    }
    if tab.ui.graph_view.hide_stashes != before.hide_stashes {
        place_stashes(&mut tab.ui);
    }
    remember(tab, cx);
}

fn hide(view: &mut GraphView, name: String) {
    if name.ends_with('/') {
        view.hidden.retain(|entry| !entry.starts_with(&name));
    }
    view.solo.retain(|entry| !covers(&name, entry));
    view.hidden.insert(name);
}

fn solo(view: &mut GraphView, name: String) {
    view.only_current = false;
    view.hidden.retain(|entry| !covers(entry, &name));
    view.solo.insert(name);
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Hide(name) => {
            if !hideable(&name) {
                tab.error(format!("{name} is not a ref FastKraken can hide from the graph."), cx);
                return;
            }
            change(tab, false, window, cx, |view| hide(view, name));
        }
        Action::HideEach(names) => {
            if let Some(name) = names.iter().find(|name| !hideable(name)) {
                tab.error(format!("{name} is not a ref FastKraken can hide from the graph."), cx);
                return;
            }
            change(tab, false, window, cx, |view| names.into_iter().for_each(|name| hide(view, name)));
        }
        Action::SoloEach(names) => {
            if let Some(name) = names.iter().find(|name| !hideable(name)) {
                tab.error(format!("{name} is not a ref FastKraken can show alone in the graph."), cx);
                return;
            }
            change(tab, false, window, cx, |view| names.into_iter().for_each(|name| solo(view, name)));
        }
        Action::Show(name) => change(tab, true, window, cx, |view| {
            view.hidden.retain(|entry| !covers(entry, &name));
        }),
        Action::ShowAll(prefix) => change(tab, true, window, cx, |view| {
            view.hidden.retain(|entry| !entry.starts_with(&prefix));
        }),
        Action::Solo(name) => {
            if !hideable(&name) {
                tab.error(format!("{name} is not a ref FastKraken can show alone in the graph."), cx);
                return;
            }
            change(tab, false, window, cx, |view| solo(view, name));
        }
        Action::Unsolo(name) => change(tab, true, window, cx, |view| {
            view.solo.remove(&name);
        }),
        Action::StopSoloing => change(tab, true, window, cx, |view| view.solo.clear()),
        Action::ToggleCurrent => {
            let on = !tab.ui.graph_view.only_current;
            tab.ui.graph_view.upstream = query(&tab.ui).upstream;
            change(tab, !on, window, cx, |view| {
                view.only_current = on;
                if on {
                    view.solo.clear();
                }
            });
        }
        Action::ToggleStashes => change(tab, false, window, cx, |view| view.hide_stashes = !view.hide_stashes),
        Action::Stale(months) => {
            if months.is_some_and(|months| !STALE_MONTHS.contains(&months)) {
                tab.error("FastKraken hides remote branches idle for 1, 3, 6 or 12 months.".to_owned(), cx);
                return;
            }
            let widened = months.is_none_or(|months| tab.ui.graph_view.stale_months.is_some_and(|was| months > was));
            change(tab, widened, window, cx, |view| view.stale_months = months);
        }
        Action::OpenAuthors => {
            let picker = &mut tab.ui.author_picker;
            if picker.open {
                picker.open = false;
                view::focus(&tab.ui, window);
            } else {
                picker.open = true;
                picker.pick = None;
                tab.ui.menu = None;
                let filter = picker.filter.clone();
                filter.update(cx, |field, cx| field.clear(cx));
                window.focus(&filter.focus_handle(cx));
            }
        }
        Action::CloseAuthors => {
            tab.ui.author_picker.open = false;
            view::focus(&tab.ui, window);
        }
        Action::Author(person) => {
            tab.ui.author_picker.open = false;
            view::focus(&tab.ui, window);
            change(tab, false, window, cx, |view| view.author = person);
        }
        Action::Clear => {
            if !tab.ui.graph_view.active() {
                return;
            }
            change(tab, true, window, cx, |view| *view = GraphView { pin: view.pin.clone(), ..GraphView::default() });
        }
    }
    cx.notify();
}

pub(crate) fn key(tab: &mut Tab, action: crate::navigate::Action, window: &mut Window, cx: &mut Context<Tab>) {
    use crate::navigate::{Action as Key, how, moved};
    let shown: Vec<Person> = matching(&tab.ui, cx).into_iter().take(SHOWN_AUTHORS).collect();
    let current = highlighted(&tab.ui, cx);
    if action == Key::Choose {
        if let Some(person) = current.and_then(|at| shown.get(at)).cloned() {
            dispatch(tab, Action::Author(Some(person)), window, cx);
        }
        return;
    }
    let Some(step) = how(action, SHOWN_AUTHORS) else { return };
    let Some(row) = moved(current, shown.len(), step) else { return };
    let text = tab.ui.author_picker.filter.read(cx).text().to_owned();
    tab.ui.author_picker.pick = Some((text, row));
    cx.notify();
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    !ui.author_picker.open && ui.author_picker.filter.focus_handle(cx).is_focused(window)
}

pub(crate) fn picker<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let found = matching(ui, cx);
    let at = highlighted(ui, cx);
    let none = ui.history.is_empty();
    let more = found.len().saturating_sub(SHOWN_AUTHORS);
    let chosen = ui.graph_view.author.as_ref();
    let mut list = div().py_1().flex().flex_col();
    if chosen.is_some() {
        list = list.child(
            kit::item("author-everyone", "Clear filter")
                .meta("Everyone")
                .on(view::Action::GraphView(Action::Author(None)), cx),
        );
    }
    for (index, person) in found.iter().take(SHOWN_AUTHORS).enumerate() {
        let ticked = chosen == Some(person);
        list = list.child(
            kit::item(("graph-author", index), person.name.clone())
                .check(ticked)
                .meta(person.email.clone())
                .highlighted(at == Some(index))
                .on(view::Action::GraphView(Action::Author(Some(person.clone()))), cx),
        );
    }
    kit::float()
        .w(px(320.))
        .flex()
        .flex_col()
        .child(
            div()
                .p_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .key_context("AuthorPicker")
                .child(ui.author_picker.filter.clone()),
        )
        .child(div().px_3().pt_2().child(kit::muted("Dim every commit by anyone else")))
        .when(none, |d| d.child(div().px_3().py_2().child(kit::muted("No commits are loaded yet."))))
        .when(!none && found.is_empty(), |d| d.child(div().px_3().py_2().child(kit::muted("No author matches."))))
        .child(list)
        .when(more > 0, |d| {
            let text = if more == 1 { "1 more. Type to narrow the list.".to_owned() } else { format!("{more} more. Type to narrow the list.") };
            d.child(div().px_3().pb_2().child(kit::muted(text)))
        })
}

pub(crate) fn strip<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<gpui::Stateful<Div>> {
    let ui = app.ui();
    let parts = summary(ui);
    if parts.is_empty() {
        return None;
    }
    let author = ui.graph_view.author.is_some();
    Some(
        div()
            .id("graph-filter-strip")
            .h(px(STRIP))
            .flex_shrink_0()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(RAISED))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(kit::icon("funnel").size(px(12.)).text_color(rgb(ACCENT)))
            .child(kit::fit(parts.join(" · ")).size(TEXT_SMALL).color(TEXT_2).grow())
            .when(author, |d| {
                d.child(kit::link("graph-filter-author", "Author…", view::Action::GraphView(Action::OpenAuthors), cx))
            })
            .child(kit::link("graph-filter-clear", "Clear", view::Action::GraphView(Action::Clear), cx)),
    )
}

pub(crate) fn left_out(ui: &Ui, full: &str) -> bool {
    !ui.graph_view.shows(full, current(ui))
}

pub(crate) fn eye<T: Desk>(
    ui: &Ui,
    full: String,
    id: (&'static str, usize),
    group: &'static str,
    pointed: bool,
    cx: &Context<T>,
) -> Option<gpui::AnyElement> {
    if !hideable(&full) {
        return None;
    }
    let view = &ui.graph_view;
    if !pointed && view.hidden_by(&full).is_none() && !view.solo.contains(&full) {
        return Some(div().size(px(18.)).flex_shrink_0().into_any_element());
    }
    let (icon, tip, color, action, always) = if let Some(entry) = view.hidden_by(&full) {
        let tip = if entry == full { "Show in the graph".to_owned() } else { format!("Show in the graph, with everything in {}", short(entry)) };
        ("eye-off", tip, TEXT_3, Action::Show(full), true)
    } else if view.solo.contains(&full) {
        ("crosshair", "Stop soloing".to_owned(), ACCENT, Action::Unsolo(full), true)
    } else {
        ("eye", "Hide in the graph".to_owned(), TEXT_2, Action::Hide(full), false)
    };
    let action = view::Action::GraphView(action);
    Some(
        div()
            .id(id)
            .size(px(18.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .when(!always, |d| d.invisible().group_hover(group, |s| s.visible()))
            .hover(|s| s.bg(rgb(RAISED)))
            .child(kit::icon(icon).size(px(12.)).text_color(rgb(color)))
            .tooltip(kit::tip(tip))
            .on_click(cx.listener(move |app, _, w, cx| {
                cx.stop_propagation();
                app.dispatch(action.clone(), w, cx);
            }))
            .into_any_element(),
    )
}

pub(crate) const STRIP: f32 = 26.;

fn hideable_target(ui: &Ui, target: &Target) -> Option<String> {
    if ui.repo.is_empty() {
        return None;
    }
    full_name(target).filter(|name| hideable(name))
}

fn when_ref(ui: &Ui, target: &Target, wanted: impl Fn(&GraphView, &str) -> bool) -> Availability {
    match hideable_target(ui, target) {
        Some(name) if wanted(&ui.graph_view, &name) => Availability::Enabled,
        _ => Availability::Hidden,
    }
}

fn picked_names(target: &Target) -> Option<Vec<String>> {
    let Target::Branches(names) = target else { return None };
    let names: Vec<String> = names.iter().map(|name| of_ref(Kind::Branch, &name.to_string_lossy())).collect();
    names.iter().all(|name| hideable(name)).then_some(names)
}

fn hide_picked_title(target: &Target) -> String {
    picked_names(target).map_or_else(String::new, |names| format!("Hide {} in the graph", crate::cleanup::branches_counted(names.len())))
}

fn solo_picked_title(target: &Target) -> String {
    picked_names(target).map_or_else(String::new, |names| format!("Solo {}", crate::cleanup::branches_counted(names.len())))
}

fn on_section(target: &Target) -> Option<Section> {
    match target {
        Target::Section(section) => Some(*section),
        _ => None,
    }
}

fn section_title(section: Section, show: bool) -> String {
    let what = match section {
        Section::Branches => "local branches",
        Section::Remotes => "remotes",
        Section::Tags => "tags",
        Section::Stashes => "stashes",
        Section::Worktrees => "worktrees",
    };
    format!("{} all {what}", if show { "Show" } else { "Hide" })
}

fn stale_here(ui: &Ui, target: &Target) -> bool {
    !ui.repo.is_empty() && (*target == Target::Repo || *target == Target::Section(Section::Remotes))
}

const STALE_MENU: crate::commands::Submenu = crate::commands::Submenu {
    id: "graph.stale-menu",
    title: |_, _| "Hide remote branches with no commits in the last…".into(),
    icon: "eye-off",
};

fn stale_title<const N: usize>(_: &Ui, _: &Target) -> String {
    let months = STALE_MONTHS[N];
    if months == 1 { "Month".to_owned() } else { format!("{months} months") }
}

fn stale_when<const N: usize>(ui: &Ui, target: &Target, _: &App) -> Availability {
    if stale_here(ui, target) { Availability::Enabled } else { Availability::Hidden }
}

fn stale_run<const N: usize>(ui: &Ui, _: &Target) -> Option<view::Action> {
    let months = STALE_MONTHS[N];
    let chosen = (ui.graph_view.stale_months != Some(months)).then_some(months);
    Some(view::Action::GraphView(Action::Stale(chosen)))
}

fn stale_tick<const N: usize>(ui: &Ui, _: &Target) -> bool {
    ui.graph_view.stale_months == Some(STALE_MONTHS[N])
}

const STALE_WORDS: &[&str] = &["stale", "old", "idle", "remote branches", "hide", "graph"];

pub(crate) const COMMANDS: &[Command] = &[
    Command::new("graph.stale-month", Group::View, stale_title::<0>, stale_when::<0>, stale_run::<0>)
        .under(&STALE_MENU)
        .tick(stale_tick::<0>)
        .words(STALE_WORDS),
    Command::new("graph.stale-quarter", Group::View, stale_title::<1>, stale_when::<1>, stale_run::<1>)
        .under(&STALE_MENU)
        .tick(stale_tick::<1>)
        .words(STALE_WORDS),
    Command::new("graph.stale-half-year", Group::View, stale_title::<2>, stale_when::<2>, stale_run::<2>)
        .under(&STALE_MENU)
        .tick(stale_tick::<2>)
        .words(STALE_WORDS),
    Command::new("graph.stale-year", Group::View, stale_title::<3>, stale_when::<3>, stale_run::<3>)
        .under(&STALE_MENU)
        .tick(stale_tick::<3>)
        .words(STALE_WORDS),
    Command::new(
        "graph.hide",
        Group::View,
        |_, _| "Hide in the graph".into(),
        |ui, target, _| when_ref(ui, target, |view, name| view.hidden_by(name).is_none()),
        |ui, target| Some(view::Action::GraphView(Action::Hide(hideable_target(ui, target)?))),
    )
    .icon("eye-off"),
    Command::new(
        "graph.show",
        Group::View,
        |_, _| "Show in the graph".into(),
        |ui, target, _| when_ref(ui, target, |view, name| view.hidden_by(name).is_some()),
        |ui, target| Some(view::Action::GraphView(Action::Show(hideable_target(ui, target)?))),
    )
    .icon("eye"),
    Command::new(
        "graph.solo",
        Group::View,
        |_, _| "Solo".into(),
        |ui, target, _| when_ref(ui, target, |view, name| !view.solo.contains(name)),
        |ui, target| Some(view::Action::GraphView(Action::Solo(hideable_target(ui, target)?))),
    )
    .words(&["show only", "graph"])
    .icon("crosshair"),
    Command::new(
        "graph.unsolo",
        Group::View,
        |_, _| "Stop soloing".into(),
        |ui, target, _| when_ref(ui, target, |view, name| view.solo.contains(name)),
        |ui, target| Some(view::Action::GraphView(Action::Unsolo(hideable_target(ui, target)?))),
    )
    .icon("crosshair"),
    Command::new(
        "graph.hide-picked",
        Group::View,
        |_, target| hide_picked_title(target),
        |ui, target, _| match picked_names(target) {
            Some(names) if !ui.repo.is_empty() && names.iter().any(|name| ui.graph_view.hidden_by(name).is_none()) => {
                Availability::Enabled
            }
            _ => Availability::Hidden,
        },
        |_, target| Some(view::Action::GraphView(Action::HideEach(picked_names(target)?))),
    )
    .icon("eye-off"),
    Command::new(
        "graph.solo-picked",
        Group::View,
        |_, target| solo_picked_title(target),
        |ui, target, _| match picked_names(target) {
            Some(names) if !ui.repo.is_empty() && names.iter().any(|name| !ui.graph_view.solo.contains(name)) => {
                Availability::Enabled
            }
            _ => Availability::Hidden,
        },
        |_, target| Some(view::Action::GraphView(Action::SoloEach(picked_names(target)?))),
    )
    .icon("crosshair"),
    Command::new(
        "graph.hide-section",
        Group::View,
        |_, target| on_section(target).map_or_else(String::new, |section| section_title(section, false)),
        |ui, target, _| match on_section(target).and_then(section_prefix) {
            Some(prefix) if !ui.graph_view.hidden.contains(prefix) && !ui.repo.is_empty() => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| {
            let prefix = on_section(target).and_then(section_prefix)?;
            Some(view::Action::GraphView(Action::Hide(prefix.to_owned())))
        },
    )
    .icon("eye-off"),
    Command::new(
        "graph.show-section",
        Group::View,
        |_, target| on_section(target).map_or_else(String::new, |section| section_title(section, true)),
        |ui, target, _| match on_section(target).and_then(section_prefix) {
            Some(prefix) if ui.graph_view.hidden.iter().any(|entry| entry.starts_with(prefix)) => Availability::Enabled,
            _ => Availability::Hidden,
        },
        |_, target| {
            let prefix = on_section(target).and_then(section_prefix)?;
            Some(view::Action::GraphView(Action::ShowAll(prefix.to_owned())))
        },
    )
    .icon("eye"),
    Command::new(
        "graph.stashes",
        Group::View,
        |_, _| "Show stashes in the graph".into(),
        |ui, target, _| {
            if on_section(target) == Some(Section::Stashes) && !ui.repo.is_empty() {
                Availability::Enabled
            } else {
                Availability::Hidden
            }
        },
        |_, _| Some(view::Action::GraphView(Action::ToggleStashes)),
    )
    .tick(|ui, _| !ui.graph_view.hide_stashes),
    Command::new(
        "graph.stop-soloing",
        Group::View,
        |_, _| "Stop soloing".into(),
        |ui, target, _| {
            if on_repo(ui, target) && !ui.graph_view.solo.is_empty() {
                Availability::Enabled
            } else {
                Availability::Hidden
            }
        },
        |_, _| Some(view::Action::GraphView(Action::StopSoloing)),
    )
    .words(&["solo", "graph", "show all"])
    .icon("crosshair"),
    Command::new(
        "graph.only-current",
        Group::View,
        |ui, _| {
            if ui.graph_view.only_current { "Show all branches" } else { "Show only the current branch" }.into()
        },
        |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(view::Action::GraphView(Action::ToggleCurrent)),
    )
    .words(&["graph", "solo", "upstream", "smart branches"])
    .icon("branch")
    .bar(Bar::View),
    Command::new(
        "graph.author",
        Group::View,
        |_, _| "Filter by author…".into(),
        |ui, target, _| {
            if !on_repo(ui, target) {
                Availability::Hidden
            } else if view::centre(ui) != view::Centre::History {
                Availability::Disabled("Show the graph first".to_owned())
            } else {
                Availability::Enabled
            }
        },
        |_, _| Some(view::Action::GraphView(Action::OpenAuthors)),
    )
    .words(&["graph", "commits by", "dim"])
    .icon("funnel")
    .bar(Bar::View),
    Command::new(
        "graph.clear",
        Group::View,
        |_, _| "Clear the graph filter".into(),
        |ui, target, _| {
            if on_repo(ui, target) && ui.graph_view.active() { Availability::Enabled } else { Availability::Hidden }
        },
        |_, _| Some(view::Action::GraphView(Action::Clear)),
    )
    .words(&["show all", "hidden", "solo", "author"])
    .icon("eye"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn args(view: &GraphView) -> Vec<String> {
        view.history_args().into_iter().map(|arg| arg.into_string().unwrap()).collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn head(f: &Fixture, revision: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", revision])).unwrap().trim().to_owned()
    }

    #[test]
    fn an_unfiltered_view_lists_every_ref_but_the_stash() {
        assert_eq!(args(&GraphView::default()), ["--exclude=refs/stash", "--exclude=refs/fastkraken/*", "--all"]);
        let quiet = GraphView { hide_stashes: true, author: Some(Person::default()), ..GraphView::default() };
        assert_eq!(args(&quiet), ["--exclude=refs/stash", "--exclude=refs/fastkraken/*", "--all"]);
        assert!(!quiet.filtered() && quiet.active());
    }

    #[test]
    fn hidden_refs_are_excluded_before_all_and_head_stays() {
        let view = GraphView { hidden: set(&["refs/heads/feature/map", "refs/remotes/origin/"]), ..GraphView::default() };
        assert_eq!(
            args(&view),
            [
                "--exclude=refs/heads/feature/map",
                "--exclude=refs/remotes/origin/*",
                "--exclude=refs/stash",
                "--exclude=refs/fastkraken/*",
                "--all",
                "--ignore-missing",
                "HEAD"
            ]
        );
    }

    #[test]
    fn a_solo_set_lists_exactly_those_refs_and_head_with_the_hidden_ones_excluded_from_each() {
        let view = GraphView {
            hidden: set(&["refs/remotes/origin/old"]),
            solo: set(&["refs/heads/feature/x", "refs/remotes/origin/"]),
            ..GraphView::default()
        };
        assert_eq!(
            args(&view),
            [
                "--exclude=refs/remotes/origin/old",
                "--glob=refs/heads/feature/[x]",
                "--exclude=refs/remotes/origin/old",
                "--glob=refs/remotes/origin/*",
                "--ignore-missing",
                "HEAD"
            ]
        );
    }

    #[test]
    fn only_the_current_branch_lists_head_and_its_upstream() {
        let mut view = GraphView { only_current: true, solo: set(&["refs/heads/x"]), ..GraphView::default() };
        assert_eq!(args(&view), ["--ignore-missing", "HEAD"]);
        view.upstream = Some("refs/remotes/origin/main".into());
        assert_eq!(args(&view), ["--glob=refs/remotes/origin/mai[n]", "--ignore-missing", "HEAD"]);
    }

    #[test]
    fn only_plain_ref_names_can_be_hidden() {
        assert!(hideable("refs/heads/feature/map"));
        assert!(hideable("refs/remotes/origin/"));
        assert!(hideable(BRANCHES) && hideable(REMOTES) && hideable(TAGS));
        for name in ["main", "refs/", "refs/stash", "refs/heads/*", "refs/heads/a b", "refs/heads/a..b", "--all"] {
            assert!(!hideable(name), "{name}");
        }
    }

    #[test]
    fn a_label_shows_unless_the_view_leaves_its_ref_out() {
        let hidden = GraphView { hidden: set(&["refs/remotes/origin/", "refs/tags/v1"]), ..GraphView::default() };
        assert!(!hidden.shows("refs/remotes/origin/main", Some("main")));
        assert!(hidden.shows("refs/remotes/upstream/main", Some("main")));
        assert!(!hidden.shows("refs/tags/v1", None) && hidden.shows("refs/tags/v10", None));
        assert_eq!(hidden.hidden_by("refs/remotes/origin/a/b"), Some("refs/remotes/origin/"));
        let solo = GraphView { solo: set(&["refs/heads/x"]), ..GraphView::default() };
        assert!(solo.shows("refs/heads/x", Some("main")) && !solo.shows("refs/heads/main", Some("main")));
        let mut current = GraphView { only_current: true, upstream: Some("refs/remotes/origin/main".into()), ..GraphView::default() };
        assert!(current.shows("refs/heads/main", Some("main")) && current.shows("refs/remotes/origin/main", Some("main")));
        assert!(!current.shows("refs/heads/x", Some("main")) && !current.shows("refs/tags/v1", Some("main")));
        current.hidden.insert("refs/remotes/origin/main".into());
        assert!(!current.shows("refs/remotes/origin/main", Some("main")));
    }

    #[test]
    fn the_author_filter_dims_by_email_or_by_name_when_there_is_no_email() {
        let view = GraphView { author: Some(Person { name: "Dana".into(), email: "dana@example.invalid".into() }), ..GraphView::default() };
        assert!(!view.dims("Dana S", "DANA@example.invalid"));
        assert!(view.dims("Dana", "other@example.invalid"));
        let nameless = GraphView { author: Some(Person { name: "Dana".into(), email: String::new() }), ..GraphView::default() };
        assert!(!nameless.dims("Dana", "") && nameless.dims("Robin", ""));
        assert!(!GraphView::default().dims("Anyone", ""));
    }

    #[test]
    fn a_stored_view_drops_what_cannot_be_hidden_and_reads_back() {
        let view = GraphView {
            hidden: set(&["refs/heads/a", "refs/heads/*", "main"]),
            solo: set(&["refs/heads/b", "refs/heads/a"]),
            only_current: true,
            hide_stashes: true,
            author: Some(Person { name: "Dana".into(), email: "dana@example.invalid".into() }),
            pin: crate::pin::Pin::Ref("refs/heads/a".into()),
            stale_months: Some(3),
            upstream: Some("refs/remotes/origin/main".into()),
            stale: set(&["refs/remotes/origin/old"]),
            bodies: true,
        };
        let json = serde_json::to_value(view.stored().unwrap()).unwrap();
        assert!(json.get("upstream").is_none() && json.get("stale").is_none() && json.get("bodies").is_none());
        let back: GraphView = serde_json::from_value(json).unwrap();
        let clean = back.sanitized();
        assert_eq!(clean.hidden, set(&["refs/heads/a"]));
        assert_eq!(clean.solo, set(&["refs/heads/b"]));
        assert!(clean.only_current && clean.hide_stashes && clean.author.is_some() && clean.upstream.is_none());
        assert_eq!(clean.pin, crate::pin::Pin::Ref("refs/heads/a".into()));
        assert_eq!(clean.stale_months, Some(3));
        assert!(clean.stale.is_empty() && !clean.bodies);
        let odd_pin: GraphView = serde_json::from_value(serde_json::json!({"pin": {"ref": "refs/tags/v1"}})).unwrap();
        assert_eq!(odd_pin.sanitized().pin, crate::pin::Pin::Head);
        let odd: GraphView = serde_json::from_value(serde_json::json!({"hidden": 3, "author": "x", "only_current": true})).unwrap();
        assert!(odd.hidden.is_empty() && odd.author.is_none() && odd.only_current);
        assert_eq!(GraphView::default().stored(), None);
        assert_eq!(GraphView { upstream: Some("refs/remotes/o/m".into()), ..GraphView::default() }.stored(), None);
    }

    #[test]
    fn hiding_a_branch_drops_its_own_commits_but_never_head() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        f.git(&["commit", "--allow-empty", "-qm", "Side only"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["commit", "--allow-empty", "-qm", "Main only"]);
        let subjects = |view: &GraphView| -> Vec<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        assert_eq!(subjects(&GraphView::default()).len(), 3);
        let side = GraphView { hidden: set(&["refs/heads/side"]), ..GraphView::default() };
        assert_eq!(subjects(&side), ["Main only", "Base"]);
        let main = GraphView { hidden: set(&["refs/heads/main"]), ..GraphView::default() };
        assert_eq!(subjects(&main).len(), 3);
        let every = GraphView { hidden: set(&[BRANCHES]), ..GraphView::default() };
        assert_eq!(subjects(&every), ["Main only", "Base"]);
    }

    #[test]
    fn soloing_lists_only_the_soloed_history_and_head_and_a_vanished_ref_lists_nothing_extra() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["branch", "side"]);
        f.git(&["branch", "sidecar"]);
        f.git(&["switch", "-q", "side"]);
        f.git(&["commit", "--allow-empty", "-qm", "Side only"]);
        f.git(&["switch", "-q", "sidecar"]);
        f.git(&["commit", "--allow-empty", "-qm", "Sidecar only"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["commit", "--allow-empty", "-qm", "Main only"]);
        f.git(&["tag", "v1", "side"]);
        let before = (head(&f, "main"), head(&f, "side"), head(&f, "sidecar"));
        let subjects = |view: &GraphView| -> BTreeSet<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        let side = GraphView { solo: set(&["refs/heads/side"]), ..GraphView::default() };
        assert_eq!(subjects(&side), set(&["Side only", "Main only", "Base"]));
        let gone = GraphView { solo: set(&["refs/heads/gone"]), ..GraphView::default() };
        assert_eq!(subjects(&gone), set(&["Main only", "Base"]));
        let tag = GraphView { solo: set(&["refs/tags/v1"]), hidden: set(&["refs/heads/main"]), ..GraphView::default() };
        assert_eq!(subjects(&tag), set(&["Side only", "Main only", "Base"]));
        assert_eq!((head(&f, "main"), head(&f, "side"), head(&f, "sidecar")), before);
        assert_eq!(f.git(&["status", "--porcelain"]), b"");
    }

    #[test]
    fn a_filtered_view_still_lists_history_while_head_is_an_unborn_branch() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "side"]);
        f.git(&["commit", "--allow-empty", "-qm", "Side only"]);
        f.git(&["switch", "-q", "--orphan", "fresh"]);
        let subjects = |view: &GraphView| -> BTreeSet<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        assert_eq!(subjects(&GraphView { hidden: set(&["refs/heads/side"]), ..GraphView::default() }), set(&["Base"]));
        assert_eq!(subjects(&GraphView { solo: set(&["refs/heads/side"]), ..GraphView::default() }), set(&["Side only", "Base"]));
        assert_eq!(subjects(&GraphView { only_current: true, ..GraphView::default() }), BTreeSet::new());
        let side = head(&f, "side");
        let at = f.repo.positions(&GraphView { hidden: set(&[TAGS]), ..GraphView::default() }.history_args(), &[side.clone()].into(), &std::sync::atomic::AtomicBool::new(false));
        assert_eq!(at.unwrap().get(&side), Some(&0));
    }

    #[test]
    fn soloing_a_branch_whose_name_ends_in_punctuation_or_an_accent_lists_its_history() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        for name in ["café", "wow!", "fix]", "cafés"] {
            f.git(&["switch", "-qc", name, "main"]);
            f.git(&["commit", "--allow-empty", "-qm", &format!("On {name}")]);
        }
        f.git(&["switch", "-q", "main"]);
        let subjects = |view: &GraphView| -> BTreeSet<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        for name in ["café", "wow!", "fix]"] {
            let solo = GraphView { solo: set(&[&format!("{BRANCHES}{name}")]), ..GraphView::default() };
            assert_eq!(subjects(&solo), set(&[&format!("On {name}"), "Base"]), "{name}");
        }
        assert_eq!(args(&GraphView { solo: set(&["refs/heads/café"]), ..GraphView::default() })[0], "--glob=refs/heads/ca[f]é");
        let mut current = GraphView { only_current: true, ..GraphView::default() };
        current.upstream = Some(format!("{BRANCHES}wow!"));
        assert_eq!(subjects(&current), set(&["On wow!", "Base"]));
    }

    #[test]
    fn only_the_current_branch_follows_its_upstream_and_survives_a_missing_one() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        f.git(&["switch", "-qc", "other"]);
        f.git(&["commit", "--allow-empty", "-qm", "Other only"]);
        f.git(&["switch", "-q", "main"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "other"]);
        f.git(&["update-ref", "-d", "refs/heads/other"]);
        f.git(&["switch", "-qc", "mine"]);
        f.git(&["commit", "--allow-empty", "-qm", "Mine only"]);
        let subjects = |view: &GraphView| -> BTreeSet<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        let mut view = GraphView { only_current: true, ..GraphView::default() };
        assert_eq!(subjects(&view), set(&["Mine only", "Base"]));
        view.upstream = Some("refs/remotes/origin/main".into());
        assert_eq!(subjects(&view), set(&["Mine only", "Other only", "Base"]));
        view.upstream = Some("refs/remotes/origin/gone".into());
        assert_eq!(subjects(&view), set(&["Mine only", "Base"]));
    }

    #[test]
    fn picked_branch_titles_count_one_branch_in_the_singular() {
        let picked = |names: &[&str]| Target::Branches(names.iter().map(std::ffi::OsString::from).collect());
        assert_eq!(hide_picked_title(&picked(&["feature/map"])), "Hide 1 branch in the graph");
        assert_eq!(solo_picked_title(&picked(&["feature/map"])), "Solo 1 branch");
        assert_eq!(hide_picked_title(&picked(&["a", "b", "c"])), "Hide 3 branches in the graph");
        assert_eq!(solo_picked_title(&picked(&["a", "b"])), "Solo 2 branches");
        assert_eq!(hide_picked_title(&Target::Repo), "");
    }

    #[test]
    fn the_strip_names_what_is_hidden_soloed_and_dimmed() {
        assert_eq!(named(&[]), "");
        assert_eq!(named(&["a".into(), "b".into()]), "a and b");
        assert_eq!(named(&["a".into(), "b".into(), "c".into(), "d".into()]), "a, b and 2 more");
        assert_eq!(short("refs/remotes/origin/"), "origin");
        assert_eq!(short("refs/heads/feature/x"), "feature/x");
        assert_eq!(short("refs/tags/v1"), "v1");
    }

    #[test]
    fn remote_branches_idle_for_the_chosen_months_leave_the_graph_but_the_tracked_one_stays() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        let old = |message: &str| {
            let made = crate::repository::checked(
                f.repo
                    .command()
                    .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
                    .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
                    .args(["commit-tree", "-p", "HEAD", "-m", message, "HEAD^{tree}"]),
            )
            .unwrap();
            String::from_utf8(made).unwrap().trim().to_owned()
        };
        let ancient = old("Ancient");
        let tracked = old("Tracked");
        f.git(&["update-ref", "refs/remotes/origin/ancient", &ancient]);
        f.git(&["update-ref", "refs/remotes/origin/tracked", &tracked]);
        f.git(&["update-ref", "refs/heads/local-old", &ancient]);
        f.git(&["commit", "--allow-empty", "-qm", "Fresh"]);
        f.git(&["update-ref", "refs/remotes/origin/fresh", "HEAD"]);
        let refs = crate::refs::list(&f.repo).unwrap();
        let ancient_ref = refs.iter().find(|item| item.kind == Kind::Remote && item.name == "origin/ancient").unwrap();
        assert_eq!(ancient_ref.time, 978_307_200);
        let now = view::now();
        assert!(stale(&refs, None, None, now).is_empty());
        let idle = stale(&refs, Some(6), Some("refs/remotes/origin/tracked"), now);
        assert_eq!(idle, set(&["refs/remotes/origin/ancient"]));
        assert_eq!(stale(&refs, Some(12), None, now), set(&["refs/remotes/origin/ancient", "refs/remotes/origin/tracked"]));
        assert!(stale(&refs, Some(1), None, ancient_ref.time + MONTH / 2).is_empty());

        let subjects = |view: &GraphView| -> BTreeSet<String> {
            f.repo.history_in(view, 0, 10).unwrap().into_iter().map(|row| row.subject).collect()
        };
        let view = GraphView { stale_months: Some(6), stale: idle, ..GraphView::default() };
        assert!(view.filtered() && view.active());
        assert!(!view.shows("refs/remotes/origin/ancient", Some("main")));
        assert!(view.shows("refs/remotes/origin/tracked", Some("main")));
        assert_eq!(subjects(&GraphView { hidden: set(&["refs/heads/local-old"]), ..GraphView::default() }).len(), 4);
        let hidden = GraphView { hidden: set(&["refs/heads/local-old"]), ..view.clone() };
        assert_eq!(subjects(&hidden), set(&["Base", "Tracked", "Fresh"]));
        assert_eq!(view.stored().map(|kept| kept.stale.len()), Some(0));
        assert_eq!(view.stored().and_then(|kept| kept.stale_months), Some(6));
        assert_eq!(months_text(1), "a month");
        assert_eq!(months_text(6), "6 months");
        let stored: GraphView = serde_json::from_str(r#"{"stale_months": 5}"#).unwrap();
        assert_eq!(stored.sanitized().stale_months, None);
    }
}
