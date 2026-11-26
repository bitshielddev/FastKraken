use crate::{
    Tab, branch, kit, pick, publish, reverse, stash, sync, tag, tracking,
    theme::*,
    view::{self, ActivateControl, CommitInput, Desk, Ui},
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton, NoAction, SharedString, Window,
    actions, deferred, div, prelude::*, px, rgb,
};

actions!(fastkraken_prompt, [Return, Accept]);

const ITEMS_HEIGHT: f32 = 240.;

pub(crate) fn bindings(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let prompt = Some("Prompt");
    cx.bind_keys([
        KeyBinding::new("enter", Return, prompt),
        KeyBinding::new("enter", Return, Some("Prompt > DeskButton")),
        KeyBinding::new(&format!("{modifier}-enter"), Accept, prompt),
        KeyBinding::new("enter", NoAction, Some("PromptLines > CommitField")),
    ]);
}

pub(crate) struct Spec {
    pub title: String,
    pub lines: Vec<String>,
    pub danger: bool,
    pub confirm: SharedString,
    pub field: Option<Field>,
    pub second: Option<Field>,
    pub second_lines: bool,
    pub note: Option<Note>,
    pub tick: Option<(SharedString, bool)>,
    pub fixed_tick: bool,
    pub tick_opens_second: bool,
    pub items: Vec<Item>,
    pub choices: Vec<Choice>,
    pub unblocked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub label: String,
    pub detail: String,
    pub ticked: bool,
}

pub(crate) struct Note {
    pub text: String,
    pub link: Option<(SharedString, view::Action)>,
}

pub(crate) struct Choice {
    pub label: SharedString,
    pub danger: bool,
}

impl Spec {
    pub(crate) fn new(title: impl Into<String>, confirm: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            lines: Vec::new(),
            danger: false,
            confirm: confirm.into(),
            field: None,
            second: None,
            second_lines: false,
            note: None,
            tick: None,
            fixed_tick: false,
            tick_opens_second: false,
            items: Vec::new(),
            choices: Vec::new(),
            unblocked: false,
        }
    }

    pub(crate) fn item(mut self, label: impl Into<String>, detail: impl Into<String>, ticked: bool) -> Self {
        self.items.push(Item { label: label.into(), detail: detail.into(), ticked });
        self
    }

    pub(crate) fn line(mut self, line: impl Into<String>) -> Self {
        self.lines.push(line.into());
        self
    }

    pub(crate) fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub(crate) fn choice(mut self, label: impl Into<SharedString>, danger: bool) -> Self {
        self.choices.push(Choice { label: label.into(), danger });
        self
    }
}

pub(crate) struct Field {
    pub label: &'static str,
    pub placeholder: &'static str,
    pub initial: String,
    pub masked: bool,
    pub validate: Box<dyn Fn(&str, &Ui) -> Option<String>>,
}

#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub text: String,
    pub second: String,
    pub choice: Option<usize>,
    pub tick: bool,
    pub items: Vec<bool>,
}

#[derive(Debug)]
pub(crate) enum Kind {
    Discard(view::DiscardAsk),
    Publish(publish::Ask),
    Branch(branch::Ask),
    Reverse(reverse::Ask),
    Pick(pick::Ask),
    Picks(pick::Series),
    Rebase(crate::rebase::Ask),
    Tag(tag::Ask),
    Tracking(tracking::Ask),
    Sync(sync::Ask),
    Stash(stash::Ask),
    Operation(crate::operation::Ask),
    Conflict(crate::conflict::Ask),
    Explain(crate::explain::Ask),
    Askpass(crate::askpass::Ask),
    Credentials(crate::credentials::Ask),
    Address(crate::address::Ask),
    Ignore(crate::ignore::Ask),
    Recover(crate::recover::Ask),
    Cleanup(crate::cleanup::Ask),
    Drag(crate::drag::Ask),
    PushTo(crate::push_to::Ask),
    Patch(crate::patch::Ask),
    Rewrite(crate::rewrite::Plan),
    RebaseMessage(crate::rebase_editor::Reword),
    Worktree(crate::worktree::Ask),
    Submodule(crate::submodule::Ask),
    Lfs(crate::lfs::Ask),
    Keys(crate::keys::Ask),
    Profiles(crate::profiles::Ask),
}

fn describe(kind: &Kind, ui: &Ui) -> Spec {
    match kind {
        Kind::Branch(ask) => branch::describe(ask, ui),
        Kind::Discard(ask) => view::describe_discard(ask),
        Kind::Publish(ask) => publish::describe(ask),
        Kind::Reverse(ask) => reverse::describe(ask, ui),
        Kind::Pick(ask) => pick::describe(ask, ui),
        Kind::Picks(series) => pick::describe_series(series, ui),
        Kind::Rebase(ask) => crate::rebase::describe(ask, ui),
        Kind::Tag(ask) => tag::describe(ask, ui),
        Kind::Tracking(ask) => tracking::describe(ask, ui),
        Kind::Sync(ask) => sync::describe(ask, ui),
        Kind::Stash(ask) => stash::describe(ask, ui),
        Kind::Operation(ask) => crate::operation::describe(ask, ui),
        Kind::Conflict(ask) => crate::conflict::describe(ask, ui),
        Kind::Explain(ask) => crate::explain::describe(ask, ui),
        Kind::Askpass(ask) => crate::askpass::describe(ask),
        Kind::Credentials(ask) => crate::credentials::describe(ask),
        Kind::Address(ask) => crate::address::describe(ask, ui),
        Kind::Ignore(ask) => crate::ignore::describe(ask),
        Kind::Recover(ask) => crate::recover::describe(ask, ui),
        Kind::Cleanup(ask) => crate::cleanup::describe(ask, ui),
        Kind::Drag(ask) => crate::drag::describe(ask, ui),
        Kind::PushTo(ask) => crate::push_to::describe(ask, ui),
        Kind::Patch(ask) => crate::patch::describe(ask, ui),
        Kind::Rewrite(plan) => crate::rewrite::describe(plan, ui),
        Kind::RebaseMessage(reword) => crate::rebase_editor::describe_reword(reword, ui),
        Kind::Worktree(ask) => crate::worktree::describe(ask, ui),
        Kind::Submodule(ask) => crate::submodule::describe_ask(ask, ui),
        Kind::Lfs(ask) => crate::lfs::describe(ask, ui),
        Kind::Keys(ask) => crate::keys::describe(ask, ui),
        Kind::Profiles(ask) => crate::profiles::describe(ask),
    }
}

fn confirm(tab: &mut Tab, kind: Kind, answer: Answer, window: &mut Window, cx: &mut Context<Tab>) {
    match kind {
        Kind::Discard(ask) => tab.discard(ask, cx),
        Kind::Publish(ask) => publish::confirm(tab, ask, answer, cx),
        Kind::Branch(ask) => branch::confirm(tab, ask, answer, cx),
        Kind::Reverse(ask) => reverse::confirm(tab, ask, answer, cx),
        Kind::Pick(ask) => pick::confirm(tab, ask, answer, cx),
        Kind::Picks(series) => pick::confirm_series(tab, series, answer, cx),
        Kind::Rebase(ask) => crate::rebase::confirm(tab, ask, answer, cx),
        Kind::Tag(ask) => tag::confirm(tab, ask, answer, cx),
        Kind::Tracking(ask) => tracking::confirm(tab, ask, answer, cx),
        Kind::Sync(ask) => sync::confirm(tab, ask, answer, cx),
        Kind::Stash(ask) => stash::confirm(tab, ask, answer, cx),
        Kind::Operation(ask) => crate::operation::confirm(tab, ask, answer, cx),
        Kind::Conflict(ask) => crate::conflict::confirm(tab, ask, answer, cx),
        Kind::Explain(ask) => crate::explain::confirm(tab, ask, answer, cx),
        Kind::Askpass(ask) => crate::askpass::confirm(tab, ask, answer, window, cx),
        Kind::Credentials(ask) => crate::credentials::confirm(tab, ask, answer, cx),
        Kind::Address(ask) => crate::address::confirm(tab, ask, answer, cx),
        Kind::Ignore(ask) => crate::ignore::confirm(tab, ask, cx),
        Kind::Recover(ask) => crate::recover::confirm(tab, ask, cx),
        Kind::Cleanup(ask) => crate::cleanup::confirm(tab, ask, answer, cx),
        Kind::Drag(ask) => crate::drag::confirm(tab, ask, answer, cx),
        Kind::PushTo(ask) => crate::push_to::confirm(tab, ask, answer, cx),
        Kind::Patch(ask) => crate::patch::confirm(tab, ask, answer, cx),
        Kind::Rewrite(plan) => crate::rewrite::confirm(tab, plan, answer, cx),
        Kind::RebaseMessage(reword) => crate::rebase_editor::confirm_reword(tab, reword, answer, cx),
        Kind::Worktree(ask) => crate::worktree::confirm(tab, ask, answer, cx),
        Kind::Submodule(ask) => crate::submodule::confirm(tab, ask, answer, cx),
        Kind::Lfs(ask) => crate::lfs::confirm(tab, ask, answer, cx),
        Kind::Keys(ask) => crate::keys::confirm(tab, ask, answer, cx),
        Kind::Profiles(ask) => crate::profiles::confirm(tab, ask, cx),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Confirm,
    Cancel,
    Return,
    Toggle,
    ToggleItem(usize),
    Choose(usize),
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Field,
    Second,
    Tick,
    Item(usize),
    Cancel,
    Confirm,
    Choice(usize),
}

pub(crate) struct State {
    asked: Option<Kind>,
    field: Option<Entity<CommitInput>>,
    second: Option<Entity<CommitInput>>,
    tick: bool,
    items: Vec<bool>,
    cancel: FocusHandle,
    confirm: FocusHandle,
    toggle: FocusHandle,
    item_handles: Vec<FocusHandle>,
    choices: Vec<FocusHandle>,
    focus_pending: bool,
    restore: Option<FocusHandle>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        Self {
            asked: None,
            field: None,
            second: None,
            tick: false,
            items: Vec::new(),
            cancel: cx.focus_handle(),
            confirm: cx.focus_handle(),
            toggle: cx.focus_handle(),
            item_handles: Vec::new(),
            choices: Vec::new(),
            focus_pending: false,
            restore: None,
        }
    }

    pub(crate) fn open(&self) -> bool {
        self.asked.is_some()
    }

    pub(crate) fn asked(&self) -> Option<&Kind> {
        self.asked.as_ref()
    }

    fn handle(&self, stop: Stop, cx: &App) -> Option<FocusHandle> {
        match stop {
            Stop::Field => self.field.as_ref().map(|field| field.focus_handle(cx)),
            Stop::Second => self.second.as_ref().map(|field| field.focus_handle(cx)),
            Stop::Tick => Some(self.toggle.clone()),
            Stop::Item(i) => self.item_handles.get(i).cloned(),
            Stop::Cancel => Some(self.cancel.clone()),
            Stop::Confirm => Some(self.confirm.clone()),
            Stop::Choice(i) => self.choices.get(i).cloned(),
        }
    }

    pub(crate) fn focused(&self, window: &Window, cx: &App) -> Option<Stop> {
        [Stop::Field, Stop::Second, Stop::Tick, Stop::Cancel, Stop::Confirm]
            .into_iter()
            .chain((0..self.item_handles.len()).map(Stop::Item))
            .chain((0..self.choices.len()).map(Stop::Choice))
            .find(|stop| self.handle(*stop, cx).is_some_and(|h| h.is_focused(window)))
    }

    pub(crate) fn field(&self) -> Option<&Entity<CommitInput>> {
        self.field.as_ref()
    }

    pub(crate) fn second(&self) -> Option<&Entity<CommitInput>> {
        self.second.as_ref()
    }

    fn text(&self, cx: &App) -> String {
        self.field
            .as_ref()
            .map(|field| field.read(cx).text().to_owned())
            .unwrap_or_default()
    }

    fn second_text(&self, cx: &App) -> String {
        self.second
            .as_ref()
            .map(|field| field.read(cx).text().to_owned())
            .unwrap_or_default()
    }

    pub(crate) fn ticked(&self) -> bool {
        self.tick
    }

    pub(crate) fn items(&self) -> &[bool] {
        &self.items
    }
}

pub(crate) fn ticks(spec_items: &[Item], state: &[bool]) -> Vec<bool> {
    if state.len() == spec_items.len() {
        state.to_vec()
    } else {
        spec_items.iter().map(|item| item.ticked).collect()
    }
}

pub(crate) fn ask(ui: &mut Ui, kind: Kind, cx: &mut Context<Tab>) {
    let spec = describe(&kind, ui);
    let state = &mut ui.prompt;
    let lines = spec.second_lines;
    let mut made = |field: Field, multiline: bool| {
        let input = cx.new(|cx| {
            let mut input = if field.masked {
                CommitInput::secret(cx, field.placeholder)
            } else if multiline {
                CommitInput::multiline(cx, field.placeholder)
            } else {
                CommitInput::with_placeholder(cx, field.placeholder)
            };
            input.set_text(&field.initial, cx);
            input
        });
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        input
    };
    state.field = spec.field.map(|field| made(field, false));
    state.second = spec.second.map(|field| made(field, lines));
    state.tick = spec.tick.is_some_and(|(_, ticked)| ticked);
    state.items = spec.items.iter().map(|item| item.ticked).collect();
    state.item_handles = spec.items.iter().map(|_| cx.focus_handle()).collect();
    state.choices = spec.choices.iter().map(|_| cx.focus_handle()).collect();
    state.asked = Some(kind);
    state.focus_pending = true;
    cx.notify();
}

pub(crate) fn shown(ui: &Ui) -> Option<Spec> {
    ui.prompt.asked.as_ref().map(|kind| describe(kind, ui))
}

fn problem(spec: &Spec, text: &str, second: &str, ui: &Ui) -> Option<String> {
    if !spec.items.is_empty() && !ticks(&spec.items, ui.prompt.items()).contains(&true) {
        return Some(String::new());
    }
    spec.field
        .as_ref()
        .and_then(|field| (field.validate)(text, ui))
        .or_else(|| spec.second.as_ref().and_then(|field| (field.validate)(second, ui)))
}

pub(crate) fn withdraw(ui: &mut Ui, cx: &mut Context<Tab>) {
    let state = &mut ui.prompt;
    state.asked = None;
    state.field = None;
    state.second = None;
    state.items.clear();
    state.item_handles.clear();
    state.choices.clear();
    state.focus_pending = false;
    state.restore = None;
    ui.focus_on_open = true;
    cx.notify();
}

pub(crate) fn stops(spec: &Spec, can_confirm: bool) -> Vec<Stop> {
    let mut stops = Vec::with_capacity(5 + spec.items.len() + spec.choices.len());
    if spec.field.is_some() {
        stops.push(Stop::Field);
    }
    let tick = spec.tick.is_some() && !spec.fixed_tick;
    if tick && spec.tick_opens_second {
        stops.push(Stop::Tick);
    }
    if spec.second.is_some() {
        stops.push(Stop::Second);
    }
    if tick && !spec.tick_opens_second {
        stops.push(Stop::Tick);
    }
    stops.extend((0..spec.items.len()).map(Stop::Item));
    if spec.choices.is_empty() {
        stops.push(Stop::Cancel);
        if can_confirm {
            stops.push(Stop::Confirm);
        }
    } else {
        if can_confirm {
            stops.push(Stop::Confirm);
        }
        stops.extend((0..spec.choices.len()).map(Stop::Choice));
        stops.push(Stop::Cancel);
    }
    stops
}

pub(crate) fn first(spec: &Spec) -> Stop {
    if spec.danger {
        Stop::Cancel
    } else if spec.field.is_some() {
        Stop::Field
    } else {
        Stop::Confirm
    }
}

pub(crate) fn cycle(stops: &[Stop], from: Option<Stop>, forward: bool) -> Stop {
    let n = stops.len();
    match from.and_then(|stop| stops.iter().position(|s| *s == stop)) {
        Some(i) if forward => stops[(i + 1) % n],
        Some(i) => stops[(i + n - 1) % n],
        None if forward => stops[0],
        None => stops[n - 1],
    }
}

pub(crate) fn on_return(spec: &Spec, focused: Option<Stop>) -> Action {
    match focused {
        Some(Stop::Cancel) => Action::Cancel,
        Some(Stop::Tick) => Action::Toggle,
        Some(Stop::Item(i)) => Action::ToggleItem(i),
        Some(Stop::Choice(i)) => match spec.choices.get(i) {
            Some(choice) if !choice.danger => Action::Choose(i),
            _ => Action::Cancel,
        },
        _ if spec.danger => Action::Cancel,
        _ => Action::Confirm,
    }
}

pub(crate) fn refocus(ui: &mut Ui) {
    if ui.prompt.open() {
        ui.prompt.focus_pending = true;
    }
}

pub(crate) fn take_focus(ui: &mut Ui, window: &mut Window, cx: &App) {
    if !ui.prompt.focus_pending {
        return;
    }
    let Some(kind) = ui.prompt.asked.as_ref() else {
        return;
    };
    ui.prompt.focus_pending = false;
    let spec = describe(kind, ui);
    ui.prompt.restore = window.focused(cx);
    if let Some(handle) = ui.prompt.handle(first(&spec), cx) {
        window.focus(&handle);
    }
}

fn close(tab: &mut Tab, window: &mut Window) {
    let state = &mut tab.ui.prompt;
    if let Some(Kind::Askpass(ask)) = state.asked.take() {
        crate::askpass::cancelled(ask);
    }
    state.field = None;
    state.second = None;
    state.item_handles.clear();
    state.choices.clear();
    state.focus_pending = false;
    let restore = state.restore.take();
    match restore {
        Some(handle) if tab.ui.publish.open() => window.focus(&handle),
        _ => view::focus(&tab.ui, window),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(kind) = tab.ui.prompt.asked.as_ref() else {
        return;
    };
    let spec = describe(kind, &tab.ui);
    let text = tab.ui.prompt.text(cx);
    let second = tab.ui.prompt.second_text(cx);
    let free = spec.unblocked || !tab.ui.busy();
    let can_confirm = free && problem(&spec, &text, &second, &tab.ui).is_none();
    let action = match action {
        Action::Return => on_return(&spec, tab.ui.prompt.focused(window, cx)),
        action => action,
    };
    match action {
        Action::Cancel => close(tab, window),
        Action::Confirm if can_confirm => answer(tab, text, second, None, window, cx),
        Action::Choose(i) if free && i < spec.choices.len() => answer(tab, text, second, Some(i), window, cx),
        Action::Confirm | Action::Return | Action::Choose(_) => {}
        Action::Toggle if spec.tick.is_some() && !spec.fixed_tick => tab.ui.prompt.tick = !tab.ui.prompt.tick,
        Action::Toggle => {}
        Action::ToggleItem(i) => {
            if let Some(ticked) = tab.ui.prompt.items.get_mut(i) {
                *ticked = !*ticked;
            }
        }
        Action::Next | Action::Previous => {
            let mut stops = stops(&spec, can_confirm);
            if spec.tick_opens_second && !tab.ui.prompt.tick {
                stops.retain(|stop| *stop != Stop::Second);
            }
            let from = tab.ui.prompt.focused(window, cx);
            let to = cycle(&stops, from, matches!(action, Action::Next));
            if let Some(handle) = tab.ui.prompt.handle(to, cx) {
                window.focus(&handle);
            }
        }
    }
    cx.notify();
}

fn answer(tab: &mut Tab, text: String, second: String, choice: Option<usize>, window: &mut Window, cx: &mut Context<Tab>) {
    let answer = Answer {
        text,
        second,
        choice,
        tick: tab.ui.prompt.tick,
        items: std::mem::take(&mut tab.ui.prompt.items),
    };
    let Some(kind) = tab.ui.prompt.asked.take() else {
        return;
    };
    close(tab, window);
    confirm(tab, kind, answer, window, cx);
}

pub(crate) fn render<T: Desk>(app: &T, capslock: bool, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    let state = &ui.prompt;
    let spec = describe(state.asked.as_ref()?, ui);
    let text = state.text(cx);
    let second = state.second_text(cx);
    let first_problem = spec.field.as_ref().and_then(|field| (field.validate)(&text, ui));
    let second_problem = spec.second.as_ref().and_then(|field| (field.validate)(&second, ui));
    let ticked = ticks(&spec.items, state.items());
    let can_confirm = (spec.unblocked || !ui.busy())
        && first_problem.is_none()
        && second_problem.is_none()
        && (spec.items.is_empty() || ticked.contains(&true));
    let help = |field: &Field, problem: Option<String>| -> Option<SharedString> {
        match problem.filter(|problem| !problem.is_empty()) {
            Some(problem) => Some(problem.into()),
            None if field.masked && capslock => Some("Caps Lock is on.".into()),
            None => None,
        }
    };
    let act = |action| view::Action::Prompt(action);
    let mut body = kit::dialog(spec.title.clone(), 440.);
    for line in &spec.lines {
        body = body.child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(palette().text_2))
                .child(line.clone()),
        );
    }
    if let (Some(field), Some(input)) = (&spec.field, &state.field) {
        body = body.child(kit::field(field.label, input.clone(), help(field, first_problem)));
    }
    let shows_second = !spec.tick_opens_second || state.tick;
    let second_row = match (&spec.second, &state.second) {
        (Some(field), Some(input)) if shows_second => {
            let input = div().when(spec.second_lines, |d| d.key_context("PromptLines")).child(input.clone());
            Some(kit::field(field.label, input, help(field, second_problem)).into_any_element())
        }
        _ => None,
    };
    let note_row = spec.note.as_ref().map(|note| {
        let mut line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(palette().text_2))
            .child(note.text.clone());
        if let Some((label, action)) = &note.link {
            line = line.child(kit::link("prompt-note-link", label.clone(), action.clone(), cx));
        }
        line.into_any_element()
    });
    let tick_row = match (&spec.tick, spec.fixed_tick) {
        (Some((label, _)), true) => Some(
            div()
                .id("prompt-tick")
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .opacity(0.55)
                .text_size(px(12.5))
                .child(kit::checkbox(state.tick))
                .child(label.clone())
                .into_any_element(),
        ),
        (Some((label, _)), false) => {
            let toggle = act(Action::Toggle);
            Some(
                div()
                    .id("prompt-tick")
                    .key_context("DeskButton")
                    .track_focus(&state.toggle)
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .rounded(px(RADIUS_CHIP))
                    .border_1()
                    .border_color(kit::clear())
                    .focus(|s| s.border_color(rgb(palette().accent)))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .child(kit::checkbox(state.tick))
                    .child(label.clone())
                    .on_click(cx.listener({
                        let toggle = toggle.clone();
                        move |app, _, w, cx| app.dispatch(toggle.clone(), w, cx)
                    }))
                    .on_action(cx.listener(move |app, _: &ActivateControl, w, cx| {
                        app.dispatch(toggle.clone(), w, cx)
                    }))
                    .into_any_element(),
            )
        }
        (None, _) => None,
    };
    body = if spec.tick_opens_second {
        body.children(note_row).children(tick_row).children(second_row)
    } else {
        body.children(second_row).children(note_row).children(tick_row)
    };
    if !spec.items.is_empty() {
        let mut list = div()
            .id("prompt-items")
            .max_h(px(ITEMS_HEIGHT))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .rounded(px(RADIUS))
            .border_1()
            .border_color(rgb(palette().line))
            .bg(rgb(palette().canvas))
            .py_1();
        for (i, (item, handle)) in spec.items.iter().zip(&state.item_handles).enumerate() {
            let toggle = act(Action::ToggleItem(i));
            let on = ticked.get(i).copied().unwrap_or(item.ticked);
            list = list.child(
                div()
                    .id(("prompt-item", i))
                    .key_context("DeskButton")
                    .track_focus(handle)
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .mx_1()
                    .px_2()
                    .py(px(3.))
                    .rounded(px(RADIUS_CHIP))
                    .border_1()
                    .border_color(kit::clear())
                    .focus(|s| s.border_color(rgb(palette().accent)))
                    .hover(|s| s.bg(rgb(palette().hover)))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .child(kit::checkbox(on))
                    .child(kit::fit(item.label.clone()).grow())
                    .child(kit::muted(item.detail.clone()).flex_shrink_0())
                    .on_click(cx.listener({
                        let toggle = toggle.clone();
                        move |app, _, w, cx| app.dispatch(toggle.clone(), w, cx)
                    }))
                    .on_action(cx.listener(move |app, _: &ActivateControl, w, cx| {
                        app.dispatch(toggle.clone(), w, cx)
                    })),
            );
        }
        body = body.child(list);
    }
    let confirm = kit::btn("prompt-confirm", spec.confirm.clone()).enabled(can_confirm);
    let confirm = if spec.danger {
        confirm.destructive()
    } else {
        confirm.primary()
    };
    let cancel = kit::btn("prompt-cancel", "Cancel").tracked(&state.cancel, act(Action::Cancel), cx);
    if spec.choices.is_empty() {
        body = body.child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(cancel)
                .child(confirm.tracked(&state.confirm, act(Action::Confirm), cx)),
        );
    } else {
        let mut column = div()
            .mt_1()
            .flex()
            .flex_col()
            .gap_2()
            .child(confirm.wide().tracked(&state.confirm, act(Action::Confirm), cx));
        for (i, (choice, handle)) in spec.choices.iter().zip(&state.choices).enumerate() {
            let button = kit::btn(("prompt-choice", i), choice.label.clone()).wide().enabled(!ui.busy());
            let button = if choice.danger { button.danger() } else { button };
            column = column.child(button.tracked(handle, act(Action::Choose(i)), cx));
        }
        body = body
            .child(column)
            .child(div().flex().justify_end().child(cancel));
    }
    let surface = div()
        .key_context("Prompt")
        .on_modifiers_changed(|_, window, _| window.refresh())
        .on_action(cx.listener(move |app, _: &Return, w, cx| {
            app.dispatch(act(Action::Return), w, cx)
        }))
        .on_action(cx.listener(move |app, _: &Accept, w, cx| {
            app.dispatch(act(Action::Confirm), w, cx)
        }))
        .child(body);
    Some(
        deferred(
            div()
                .id("prompt-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |app, _, w, cx| app.dispatch(act(Action::Cancel), w, cx)),
                )
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(surface),
                ),
        )
        .with_priority(4),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(danger: bool, tick: bool) -> Spec {
        let mut spec = Spec::new("Rename main", "Rename");
        spec.danger = danger;
        spec.field = Some(Field {
            label: "Name",
            placeholder: "Branch name",
            initial: "main".into(),
            masked: false,
            validate: Box::new(|text, _| text.trim().is_empty().then(|| "Enter a name.".to_owned())),
        });
        spec.tick = tick.then(|| ("Also rename the remote branch".into(), false));
        spec
    }

    #[test]
    fn return_on_a_danger_prompt_cancels_and_cancel_has_focus() {
        let danger = Spec::new("Abort the merge of a into b?", "Abort merge").danger();
        for focused in [None, Some(Stop::Cancel), Some(Stop::Confirm), Some(Stop::Field)] {
            assert!(matches!(on_return(&danger, focused), Action::Cancel), "{focused:?}");
        }
        assert!(matches!(on_return(&form(true, true), Some(Stop::Tick)), Action::Toggle));
        assert_eq!(first(&danger), Stop::Cancel);
        assert_eq!(first(&form(true, false)), Stop::Cancel);
        let plain = Spec::new("Disconnect GitHub account octo?", "Disconnect");
        assert!(matches!(on_return(&plain, None), Action::Confirm));
        assert!(matches!(on_return(&plain, Some(Stop::Confirm)), Action::Confirm));
        assert!(matches!(on_return(&plain, Some(Stop::Cancel)), Action::Cancel));
        assert!(matches!(on_return(&form(false, false), Some(Stop::Field)), Action::Confirm));
        assert_eq!(first(&plain), Stop::Confirm);
        assert_eq!(first(&form(false, false)), Stop::Field);
    }

    #[test]
    fn tab_does_not_leave_the_prompt() {
        let spec = form(false, true);
        let order = stops(&spec, true);
        assert_eq!(order, [Stop::Field, Stop::Tick, Stop::Cancel, Stop::Confirm]);
        let mut at = Some(first(&spec));
        for _ in 0..9 {
            let next = cycle(&order, at, true);
            assert!(order.contains(&next));
            at = Some(next);
        }
        assert_eq!(at, Some(Stop::Tick));
        assert_eq!(cycle(&order, Some(Stop::Confirm), true), Stop::Field);
        assert_eq!(cycle(&order, Some(Stop::Field), false), Stop::Confirm);
        assert_eq!(cycle(&order, None, true), Stop::Field);
        assert_eq!(cycle(&order, None, false), Stop::Confirm);
        assert_eq!(stops(&spec, false), [Stop::Field, Stop::Tick, Stop::Cancel]);
        let bare = Spec::new("Discard this hunk?", "Discard hunk").danger();
        let order = stops(&bare, true);
        assert_eq!(order, [Stop::Cancel, Stop::Confirm]);
        assert_eq!(cycle(&order, Some(Stop::Confirm), true), Stop::Cancel);
        assert_eq!(cycle(&order, Some(Stop::Cancel), false), Stop::Confirm);
    }

    #[test]
    fn a_tick_that_opens_the_second_field_comes_before_it() {
        let mut spec = form(false, true);
        spec.second = Some(Field {
            label: "New branch",
            placeholder: "Name of the new branch",
            initial: String::new(),
            masked: false,
            validate: Box::new(|_, _| None),
        });
        assert_eq!(stops(&spec, true), [Stop::Field, Stop::Second, Stop::Tick, Stop::Cancel, Stop::Confirm]);
        spec.tick_opens_second = true;
        assert_eq!(stops(&spec, true), [Stop::Field, Stop::Tick, Stop::Second, Stop::Cancel, Stop::Confirm]);
        spec.fixed_tick = true;
        assert_eq!(stops(&spec, true), [Stop::Field, Stop::Second, Stop::Cancel, Stop::Confirm]);
    }

    #[test]
    fn ticked_rows_sit_between_the_field_and_the_buttons_and_return_toggles_them() {
        let spec = Spec::new("Delete merged branches", "Delete branches")
            .item("feature/ford", "merged", true)
            .item("stale", "remote branch gone", false);
        assert_eq!(stops(&spec, true), [Stop::Item(0), Stop::Item(1), Stop::Cancel, Stop::Confirm]);
        assert!(matches!(on_return(&spec, Some(Stop::Item(1))), Action::ToggleItem(1)));
        assert!(matches!(on_return(&spec, Some(Stop::Confirm)), Action::Confirm));
        assert_eq!(ticks(&spec.items, &[]), [true, false]);
        assert_eq!(ticks(&spec.items, &[false, true]), [false, true]);
        assert_eq!(ticks(&spec.items, &[false]), [true, false]);
    }

    #[test]
    fn other_choices_follow_confirm_and_return_never_takes_a_danger_one() {
        let spec = Spec::new("Check out origin/topic", "Switch to topic as it is")
            .choice("Create another branch…", false)
            .choice("Reset topic to origin/topic", true);
        assert_eq!(stops(&spec, true), [Stop::Confirm, Stop::Choice(0), Stop::Choice(1), Stop::Cancel]);
        assert_eq!(stops(&spec, false), [Stop::Choice(0), Stop::Choice(1), Stop::Cancel]);
        assert_eq!(first(&spec), Stop::Confirm);
        assert!(matches!(on_return(&spec, Some(Stop::Confirm)), Action::Confirm));
        assert!(matches!(on_return(&spec, Some(Stop::Choice(0))), Action::Choose(0)));
        assert!(matches!(on_return(&spec, Some(Stop::Choice(1))), Action::Cancel));
        assert!(matches!(on_return(&spec, Some(Stop::Choice(7))), Action::Cancel));
        let order = stops(&spec, true);
        assert_eq!(cycle(&order, Some(Stop::Cancel), true), Stop::Confirm);
        assert_eq!(cycle(&order, Some(Stop::Confirm), false), Stop::Cancel);
    }
}
