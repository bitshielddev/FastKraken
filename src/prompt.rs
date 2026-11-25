use crate::{
    Tab, branch, kit, pick, publish, reverse, stash, sync, tag, tracking,
    theme::*,
    view::{self, ActivateControl, CommitInput, Desk, Ui},
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, KeyBinding, MouseButton, SharedString, Window,
    actions, deferred, div, prelude::*, px, rgb,
};

actions!(fastkraken_prompt, [Return, Accept]);

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
    ]);
}

pub(crate) struct Spec {
    pub title: String,
    pub lines: Vec<String>,
    pub danger: bool,
    pub confirm: SharedString,
    pub field: Option<Field>,
    pub second: Option<Field>,
    pub note: Option<Note>,
    pub tick: Option<(SharedString, bool)>,
    pub choices: Vec<Choice>,
    pub unblocked: bool,
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
            note: None,
            tick: None,
            choices: Vec::new(),
            unblocked: false,
        }
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
}

#[derive(Debug)]
pub(crate) enum Kind {
    Discard(view::DiscardAsk),
    Publish(publish::Ask),
    Branch(branch::Ask),
    Reverse(reverse::Ask),
    Pick(pick::Ask),
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
}

fn describe(kind: &Kind, ui: &Ui) -> Spec {
    match kind {
        Kind::Branch(ask) => branch::describe(ask, ui),
        Kind::Discard(ask) => view::describe_discard(ask),
        Kind::Publish(ask) => publish::describe(ask),
        Kind::Reverse(ask) => reverse::describe(ask, ui),
        Kind::Pick(ask) => pick::describe(ask, ui),
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
    }
}

fn confirm(tab: &mut Tab, kind: Kind, answer: Answer, window: &mut Window, cx: &mut Context<Tab>) {
    match kind {
        Kind::Discard(ask) => tab.discard(ask, cx),
        Kind::Publish(ask) => publish::confirm(tab, ask, answer, cx),
        Kind::Branch(ask) => branch::confirm(tab, ask, answer, cx),
        Kind::Reverse(ask) => reverse::confirm(tab, ask, answer, cx),
        Kind::Pick(ask) => pick::confirm(tab, ask, answer, cx),
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
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Confirm,
    Cancel,
    Return,
    Toggle,
    Choose(usize),
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Field,
    Second,
    Tick,
    Cancel,
    Confirm,
    Choice(usize),
}

pub(crate) struct State {
    asked: Option<Kind>,
    field: Option<Entity<CommitInput>>,
    second: Option<Entity<CommitInput>>,
    tick: bool,
    cancel: FocusHandle,
    confirm: FocusHandle,
    toggle: FocusHandle,
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
            cancel: cx.focus_handle(),
            confirm: cx.focus_handle(),
            toggle: cx.focus_handle(),
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
            Stop::Cancel => Some(self.cancel.clone()),
            Stop::Confirm => Some(self.confirm.clone()),
            Stop::Choice(i) => self.choices.get(i).cloned(),
        }
    }

    pub(crate) fn focused(&self, window: &Window, cx: &App) -> Option<Stop> {
        [Stop::Field, Stop::Second, Stop::Tick, Stop::Cancel, Stop::Confirm]
            .into_iter()
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
}

pub(crate) fn ask(ui: &mut Ui, kind: Kind, cx: &mut Context<Tab>) {
    let spec = describe(&kind, ui);
    let state = &mut ui.prompt;
    let mut made = |field: Field| {
        let input = cx.new(|cx| {
            let mut input = if field.masked {
                CommitInput::secret(cx, field.placeholder)
            } else {
                CommitInput::with_placeholder(cx, field.placeholder)
            };
            input.set_text(&field.initial, cx);
            input
        });
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        input
    };
    state.field = spec.field.map(&mut made);
    state.second = spec.second.map(&mut made);
    state.tick = spec.tick.is_some_and(|(_, ticked)| ticked);
    state.choices = spec.choices.iter().map(|_| cx.focus_handle()).collect();
    state.asked = Some(kind);
    state.focus_pending = true;
    cx.notify();
}

pub(crate) fn shown(ui: &Ui) -> Option<Spec> {
    ui.prompt.asked.as_ref().map(|kind| describe(kind, ui))
}

fn problem(spec: &Spec, text: &str, second: &str, ui: &Ui) -> Option<String> {
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
    state.choices.clear();
    state.focus_pending = false;
    state.restore = None;
    ui.focus_on_open = true;
    cx.notify();
}

pub(crate) fn stops(spec: &Spec, can_confirm: bool) -> Vec<Stop> {
    let mut stops = Vec::with_capacity(5 + spec.choices.len());
    if spec.field.is_some() {
        stops.push(Stop::Field);
    }
    if spec.second.is_some() {
        stops.push(Stop::Second);
    }
    if spec.tick.is_some() {
        stops.push(Stop::Tick);
    }
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
        Action::Toggle if spec.tick.is_some() => tab.ui.prompt.tick = !tab.ui.prompt.tick,
        Action::Toggle => {}
        Action::Next | Action::Previous => {
            let stops = stops(&spec, can_confirm);
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
    let can_confirm = (spec.unblocked || !ui.busy()) && first_problem.is_none() && second_problem.is_none();
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
                .text_color(rgb(TEXT_2))
                .child(line.clone()),
        );
    }
    if let (Some(field), Some(input)) = (&spec.field, &state.field) {
        body = body.child(kit::field(field.label, input.clone(), help(field, first_problem)));
    }
    if let (Some(field), Some(input)) = (&spec.second, &state.second) {
        body = body.child(kit::field(field.label, input.clone(), help(field, second_problem)));
    }
    if let Some(note) = &spec.note {
        let mut line = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(rgb(TEXT_2))
            .child(note.text.clone());
        if let Some((label, action)) = &note.link {
            line = line.child(kit::link("prompt-note-link", label.clone(), action.clone(), cx));
        }
        body = body.child(line);
    }
    if let Some((label, _)) = &spec.tick {
        let toggle = act(Action::Toggle);
        body = body.child(
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
                .focus(|s| s.border_color(rgb(ACCENT)))
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
                })),
        );
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
