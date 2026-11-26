use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, on_repo},
    notice::Notice,
    repository::{self, Repository},
    theme::*,
    view::{self, CommitInput, Desk, Selection, Tone, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, KeyDownEvent, Window, div, prelude::*, px, rgb};
use std::{path::Path, process::Command as Process};

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "repo.identity",
    Group::Changes,
    |_, _| "Set name and email for commits…".into(),
    |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Identity(Action::Edit)),
)
.words(&["identity", "author", "committer", "user.name", "user.email", "git config", "email address", "who am i"])
.icon("commit")];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    #[default]
    Global,
    Local,
}

impl Scope {
    pub(crate) fn flag(self) -> &'static str {
        match self {
            Scope::Global => "--global",
            Scope::Local => "--local",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Scope::Global => "For every repository",
            Scope::Local => "For this repository only",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Values {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Values {
    pub(crate) fn shown(&self) -> Option<String> {
        Some(format!("{} <{}>", self.name.as_deref()?, self.email.as_deref()?))
    }

    fn complete(&self) -> bool {
        self.name.is_some() && self.email.is_some()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Identity {
    pub effective: Values,
    pub global: Values,
    pub local: Values,
    pub signing: crate::signing::Config,
}

impl Identity {
    fn of(&self, scope: Scope) -> &Values {
        match scope {
            Scope::Global => &self.global,
            Scope::Local => &self.local,
        }
    }
}

pub(crate) fn reason(values: &Values) -> Option<String> {
    match (&values.name, &values.email) {
        (Some(_), Some(_)) => None,
        (None, None) => Some("Set your name and email to commit".into()),
        (None, Some(_)) => Some("Set your name to commit".into()),
        (Some(_), None) => Some("Set your email to commit".into()),
    }
}

fn missing_words(values: &Values) -> &'static str {
    match (&values.name, &values.email) {
        (None, Some(_)) => "Git has no name for your commits yet. Add it to commit.",
        (Some(_), None) => "Git has no email for your commits yet. Add it to commit.",
        _ => "Git has no name and email for your commits yet. Add them to commit.",
    }
}

pub(crate) fn problem(name: &str, email: &str) -> Option<String> {
    let (name, email) = (name.trim(), email.trim());
    if name.is_empty() || email.is_empty() {
        Some(String::new())
    } else if name.contains(['<', '>', '\n', '\r']) {
        Some("A name cannot contain < or >.".into())
    } else if email.contains(|c: char| c.is_whitespace() || c == '<' || c == '>') {
        Some("An email cannot contain spaces, < or >.".into())
    } else if !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        Some("Enter an email address, such as you@example.com.".into())
    } else {
        None
    }
}

pub(crate) fn parse(listed: &[u8]) -> Identity {
    let mut identity = Identity::default();
    let mut parts = listed.split(|&byte| byte == 0);
    while let (Some(scope), Some(entry)) = (parts.next(), parts.next()) {
        let entry = String::from_utf8_lossy(entry);
        let (key, given) = match entry.split_once('\n') {
            Some((key, value)) => (key, Some(value)),
            None => (entry.as_ref(), None),
        };
        identity.signing.read(key, given);
        let value = Some(given.unwrap_or_default().trim().to_owned()).filter(|value| !value.is_empty());
        let pick = |values: &mut Values| match key {
            "user.name" => values.name = value.clone(),
            "user.email" => values.email = value.clone(),
            _ => {}
        };
        if key != "user.name" && key != "user.email" {
            continue;
        }
        pick(&mut identity.effective);
        match scope {
            b"global" => pick(&mut identity.global),
            b"local" | b"worktree" => pick(&mut identity.local),
            _ => {}
        }
    }
    identity
}

fn read(mut command: Process) -> Result<Identity, String> {
    command.args(["config", "list", "--show-scope", "-z"]);
    repository::checked(&mut command).map(|listed| parse(&listed))
}

pub(crate) fn of(repo: &Repository) -> Result<Identity, String> {
    read(repo.read_command())
}

pub(crate) fn outside() -> Result<Identity, String> {
    let mut command = repository::bundled_command(Path::new("/"))?;
    command.arg("--no-optional-locks");
    read(command)
}

pub(crate) fn still(repo: &Repository, shown: &Values) -> Result<(), String> {
    let now = of(repo)?.effective;
    if now == *shown {
        return Ok(());
    }
    Err(match now.shown() {
        Some(now) => format!(
            "Not Committed — Commits here now carry {now}, not the name and email shown. Check them under the Commit button, then commit again."
        ),
        None => "Not Committed — Git no longer has a name and email for commits here. Set them under the Commit button.".into(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub scope: Scope,
    pub name: String,
    pub email: String,
    pub seen: Values,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Changed { now: Values, words: String },
    Failed(String),
}

pub(crate) fn save(repo: Option<&Repository>, change: &Change) -> Result<(Identity, bool), Refusal> {
    if let Some(problem) = problem(&change.name, &change.email) {
        return Err(Refusal::Failed(if problem.is_empty() { "Enter a name and an email.".into() } else { problem }));
    }
    let current = || match repo {
        Some(repo) => of(repo),
        None if change.scope == Scope::Local => Err("Open a repository to give it its own name and email.".into()),
        None => outside(),
    };
    let before = current().map_err(Refusal::Failed)?;
    let now = before.of(change.scope).clone();
    if now != change.seen {
        let words = match (change.scope, now.shown()) {
            (Scope::Global, Some(now)) => format!("Your name and email for every repository changed to {now} meanwhile. Check them and save again."),
            (Scope::Local, Some(now)) => format!("This repository's own name and email changed to {now} meanwhile. Check them and save again."),
            _ => "The name and email changed meanwhile. Check them and save again.".into(),
        };
        return Err(Refusal::Changed { now, words });
    }
    let (name, email) = (change.name.trim(), change.email.trim());
    let mut wrote = false;
    for (key, value, had) in [("user.name", name, &now.name), ("user.email", email, &now.email)] {
        if had.as_deref() == Some(value) {
            continue;
        }
        let mut command = match (change.scope, repo) {
            (Scope::Local, Some(repo)) => repo.command(),
            _ => repository::bundled_command(Path::new("/")).map_err(Refusal::Failed)?,
        };
        command.args(["config", "set", change.scope.flag(), "--all", "--end-of-options", key, value]);
        repository::checked(&mut command).map_err(Refusal::Failed)?;
        wrote = true;
    }
    let after = current().map_err(Refusal::Failed)?;
    Ok((after, wrote))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    Panel,
    Preferences,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Edit,
    Cancel,
    SetScope(Scope),
    Save(Place),
}

pub(crate) struct State {
    known: Option<Identity>,
    editing: bool,
    filled: bool,
    pub(crate) name: Entity<CommitInput>,
    pub(crate) email: Entity<CommitInput>,
    scope: Scope,
    base: Identity,
    saving: bool,
    error: Option<String>,
    global: Option<Result<Values, String>>,
    global_ticket: u64,
    pub(crate) global_name: Entity<CommitInput>,
    pub(crate) global_email: Entity<CommitInput>,
    global_base: Values,
    global_error: Option<String>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let field = |cx: &mut Context<Tab>, placeholder: &'static str| {
            let field = cx.new(|cx| CommitInput::with_placeholder(cx, placeholder));
            cx.observe(&field, |_, _, cx| cx.notify()).detach();
            field
        };
        Self {
            known: None,
            editing: false,
            filled: false,
            name: field(cx, "Your name"),
            email: field(cx, "you@example.com"),
            scope: Scope::Global,
            base: Identity::default(),
            saving: false,
            error: None,
            global: None,
            global_ticket: 0,
            global_name: field(cx, "Your name"),
            global_email: field(cx, "you@example.com"),
            global_base: Values::default(),
            global_error: None,
        }
    }

    pub(crate) fn known(&self) -> Option<&Identity> {
        self.known.as_ref()
    }

    pub(crate) fn reason(&self) -> Option<String> {
        if self.saving {
            return Some("Wait for the name and email to be saved".into());
        }
        reason(&self.known.as_ref()?.effective)
    }

    pub(crate) fn effective(&self) -> Option<&Values> {
        self.known.as_ref().map(|known| &known.effective)
    }

    fn missing(&self) -> bool {
        self.known.as_ref().is_some_and(|known| !known.effective.complete())
    }

    pub(crate) fn form_shown(&self) -> bool {
        self.editing || self.missing()
    }

    pub(crate) fn scope(&self) -> Scope {
        self.scope
    }

    pub(crate) fn saving(&self) -> bool {
        self.saving
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn global(&self) -> Option<&Result<Values, String>> {
        self.global.as_ref()
    }

    pub(crate) fn global_error(&self) -> Option<&str> {
        self.global_error.as_deref()
    }

    pub(crate) fn committing_as(&self) -> Option<String> {
        self.effective()?.shown().map(|shown| format!("Committing as {shown}"))
    }

    fn fill(&mut self, cx: &mut App) {
        let known = self.known.clone().unwrap_or_default();
        self.name.update(cx, |field, cx| field.set_text(known.effective.name.as_deref().unwrap_or(""), cx));
        self.email.update(cx, |field, cx| field.set_text(known.effective.email.as_deref().unwrap_or(""), cx));
        self.scope = if known.local.name.is_some() || known.local.email.is_some() { Scope::Local } else { Scope::Global };
        self.base = known;
        self.error = None;
        self.filled = true;
    }
}

pub(crate) fn landed(tab: &mut Tab, identity: Result<Identity, String>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.identity;
    state.known = identity.ok();
    if !state.form_shown() {
        state.filled = false;
    } else if !state.filled {
        state.fill(cx);
    }
    if tab.ui.identity.form_shown() && !crate::profiles::listed(cx).is_empty() {
        crate::profiles::read_locals(tab, cx);
    }
}

pub(crate) fn close_form(tab: &mut Tab) {
    let state = &mut tab.ui.identity;
    state.editing = false;
    state.error = None;
    state.filled = false;
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Edit => {
            if tab.repo.is_none() || crate::operation::owns_panel(&tab.ui) {
                tab.dispatch(view::Action::Preferences(crate::preferences::Action::Open(Some(crate::preferences::Section::Git))), window, cx);
                return;
            }
            if tab.ui.preferences.open() {
                crate::preferences::dispatch(tab, crate::preferences::Action::Close, window, cx);
            }
            if tab.ui.panel_hidden {
                tab.dispatch(view::Action::TogglePanel, window, cx);
            }
            if !tab.ui.working() {
                tab.dispatch(view::Action::Select(Selection::Changes), window, cx);
            }
            let state = &mut tab.ui.identity;
            state.editing = true;
            state.fill(cx);
            window.focus(&tab.ui.identity.name.focus_handle(cx));
            crate::profiles::read_locals(tab, cx);
        }
        Action::Cancel => {
            let state = &mut tab.ui.identity;
            state.editing = false;
            state.error = None;
            if !state.missing() {
                state.filled = false;
            }
            view::focus(&tab.ui, window);
        }
        Action::SetScope(scope) => {
            tab.ui.identity.scope = scope;
            tab.ui.identity.error = None;
        }
        Action::Save(place) => save_from(tab, place, cx),
    }
    cx.notify();
}

fn save_from(tab: &mut Tab, place: Place, cx: &mut Context<Tab>) {
    let state = &tab.ui.identity;
    if state.saving {
        return;
    }
    let (name, email) = match place {
        Place::Panel => (&state.name, &state.email),
        Place::Preferences => (&state.global_name, &state.global_email),
    };
    let (name, email) = (name.read(cx).text().trim().to_owned(), email.read(cx).text().trim().to_owned());
    let scope = if place == Place::Panel { state.scope } else { Scope::Global };
    let seen = match place {
        Place::Panel => state.base.of(scope).clone(),
        Place::Preferences => state.global_base.clone(),
    };
    if let Some(problem) = problem(&name, &email) {
        let words = if problem.is_empty() { "Enter a name and an email.".to_owned() } else { problem };
        match place {
            Place::Panel => tab.ui.identity.error = Some(words),
            Place::Preferences => tab.ui.identity.global_error = Some(words),
        }
        return;
    }
    if place == Place::Panel && scope == Scope::Local && tab.repo.is_none() {
        return;
    }
    let repo = if place == Place::Panel { tab.repo.clone() } else { None };
    let change = Change { scope, name, email, seen };
    tab.ui.identity.saving = true;
    let title = tab.ui.repo.clone();
    let epoch = tab.epoch;
    Tab::work(cx, move || save(repo.as_ref(), &change).map(|saved| (saved, change)), move |tab, result, cx| {
        if tab.epoch != epoch {
            match result {
                Ok(((identity, wrote), change)) => {
                    tab.ui.notices.show(saved(place, &change, &identity, wrote, &title));
                }
                Err(Refusal::Changed { words, .. } | Refusal::Failed(words)) => tab.ui.say(Tone::Error, words),
            }
            cx.notify();
            return;
        }
        let state = &mut tab.ui.identity;
        state.saving = false;
        match result {
            Ok(((identity, wrote), change)) => {
                let notice = saved(place, &change, &identity, wrote, &title);
                match place {
                    Place::Panel => {
                        state.editing = false;
                        state.error = None;
                        state.known = Some(identity);
                        state.filled = false;
                        if state.missing() {
                            state.fill(cx);
                        }
                    }
                    Place::Preferences => {
                        state.global_error = None;
                        state.global_base = identity.global.clone();
                        state.global = Some(Ok(identity.global));
                    }
                }
                tab.ui.notices.show(notice);
                if tab.repo.is_some() {
                    tab.refresh(false, cx);
                }
            }
            Err(Refusal::Changed { now, words }) => match place {
                Place::Panel => {
                    match state.scope {
                        Scope::Global => state.base.global = now,
                        Scope::Local => state.base.local = now,
                    }
                    state.error = Some(words);
                }
                Place::Preferences => {
                    state.global_base = now;
                    state.global_error = Some(words);
                }
            },
            Err(Refusal::Failed(words)) => match place {
                Place::Panel => state.error = Some(words),
                Place::Preferences => state.global_error = Some(words),
            },
        }
        cx.notify();
    });
}

fn saved(place: Place, change: &Change, identity: &Identity, wrote: bool, title: &str) -> Notice {
    let shown = format!("{} <{}>", change.name, change.email);
    let detail = match change.scope {
        Scope::Global => {
            let mut detail = format!("Commits in every repository carry {shown}.");
            if place == Place::Panel
                && let Some(own) = identity.effective.shown().filter(|own| *own != shown)
            {
                detail.push_str(&format!(" {title} keeps its own: {own}."));
            }
            detail
        }
        Scope::Local => format!("Commits in {title} carry {shown}. Other repositories are unchanged."),
    };
    Notice::new(Tone::Success, if wrote { "Name and email saved" } else { "Name and email unchanged" }, detail)
}

pub(crate) fn read_global(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.identity;
    state.global_ticket += 1;
    let ticket = state.global_ticket;
    Tab::work(cx, || outside().map(|identity| identity.global), move |tab, read, cx| {
        let state = &mut tab.ui.identity;
        if state.global_ticket != ticket {
            return;
        }
        if let Ok(values) = &read {
            state.global_name.update(cx, |field, cx| field.set_text(values.name.as_deref().unwrap_or(""), cx));
            state.global_email.update(cx, |field, cx| field.set_text(values.email.as_deref().unwrap_or(""), cx));
            state.global_base = values.clone();
        }
        state.global_error = None;
        state.global = Some(read);
        cx.notify();
    });
}


fn submit<T: Desk>(place: Place, cx: &mut Context<T>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |app: &mut T, e: &KeyDownEvent, w, cx| {
        if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
            app.dispatch(view::Action::Identity(Action::Save(place)), w, cx);
            cx.stop_propagation();
        }
    })
}

fn choice<T: Desk>(scope: Scope, chosen: bool, cx: &Context<T>) -> gpui::Stateful<Div> {
    let row = div()
        .id(("identity-scope", scope as usize))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .rounded(px(RADIUS_CHIP))
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(if chosen { TEXT } else { TEXT_2 }))
        .focusable()
        .focus(|s| s.text_color(rgb(TEXT)))
        .child(crate::preferences::radio(chosen))
        .child(scope.title());
    kit::pressable(row, view::Action::Identity(Action::SetScope(scope)), cx)
}

pub(crate) fn form<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Option<Div> {
    let state = &ui.identity;
    if !state.form_shown() {
        return None;
    }
    let missing = state.missing();
    let typed = problem(state.name.read(cx).text(), state.email.read(cx).text());
    let ready = typed.is_none() && !state.saving && (state.scope == Scope::Global || !ui.repo.is_empty());
    let note = state.error.clone().map(|error| (error, DELETED)).or_else(|| typed.filter(|problem| !problem.is_empty()).map(|problem| (problem, TEXT_3)));
    let intro = match state.known.as_ref() {
        Some(known) if missing => missing_words(&known.effective),
        _ => "The name and email your commits carry.",
    };
    Some(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded(px(RADIUS))
            .border_1()
            .border_color(rgb(if missing { MODIFIED } else { LINE }))
            .bg(rgb(CANVAS))
            .child(div().text_size(px(TEXT_SMALL)).text_color(rgb(if missing { TEXT } else { TEXT_2 })).child(intro))
            .child(div().on_key_down(submit(Place::Panel, cx)).child(state.name.clone()))
            .child(div().on_key_down(submit(Place::Panel, cx)).child(state.email.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(choice(Scope::Global, state.scope == Scope::Global, cx))
                    .child(choice(Scope::Local, state.scope == Scope::Local, cx)),
            )
            .children(crate::profiles::chooser(ui, cx))
            .when_some(note, |d, (note, color)| d.child(div().text_size(px(TEXT_SMALL)).text_color(rgb(color)).child(note)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .when(!missing, |d| {
                        d.child(kit::btn("identity-cancel", "Cancel").small().on(view::Action::Identity(Action::Cancel), cx))
                    })
                    .child(
                        kit::btn("identity-save", if state.saving { "Saving…" } else { "Save" })
                            .small()
                            .enabled(ready)
                            .on(view::Action::Identity(Action::Save(Place::Panel)), cx),
                    ),
            ),
    )
}

pub(crate) fn committing_as<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let state = &ui.identity;
    if state.form_shown() {
        return None;
    }
    let text = state.committing_as()?;
    let line = div()
        .id("commit-identity")
        .flex()
        .min_w_0()
        .cursor_pointer()
        .rounded(px(RADIUS_CHIP))
        .border_1()
        .border_color(kit::clear())
        .focusable()
        .focus(|s| s.border_color(rgb(ACCENT)))
        .tooltip(kit::tip(format!("{text}. Choose to change the name and email your commits carry.")))
        .child(kit::fit(text).size(11.5).color(TEXT_3));
    Some(kit::pressable(line, view::Action::Identity(Action::Edit), cx))
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.identity;
    let typed = problem(state.global_name.read(cx).text(), state.global_email.read(cx).text());
    let unchanged = Some(state.global_name.read(cx).text().trim()) == state.global_base.name.as_deref()
        && Some(state.global_email.read(cx).text().trim()) == state.global_base.email.as_deref();
    let ready = typed.is_none() && !state.saving && !unchanged && matches!(state.global, Some(Ok(_)));
    let note = match (&state.global, &state.global_error) {
        (None, _) => Some(("Reading your name and email…".to_owned(), TEXT_3)),
        (Some(Err(error)), _) => Some((error.clone(), DELETED)),
        (_, Some(error)) => Some((error.clone(), DELETED)),
        _ => typed.filter(|problem| !problem.is_empty()).map(|problem| (problem, TEXT_3)),
    };
    let own = state.known.as_ref().and_then(|known| known.local.shown()).filter(|_| !ui.repo.is_empty());
    let name = div().w(px(260.)).on_key_down(submit(Place::Preferences, cx)).child(state.global_name.clone());
    let email = div().w(px(260.)).on_key_down(submit(Place::Preferences, cx)).child(state.global_email.clone());
    div()
        .flex()
        .flex_col()
        .child(crate::preferences::group("YOUR NAME AND EMAIL"))
        .child(kit::muted("Every commit carries them, in every repository that has none of its own."))
        .child(crate::preferences::setting("Name", None, name))
        .child(crate::preferences::setting("Email", None, email))
        .child(
            div()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .child(div().flex_1().min_w_0().when_some(note, |d, (note, color)| {
                    d.child(div().text_size(px(TEXT_SMALL)).text_color(rgb(color)).child(note))
                }))
                .child(
                    kit::btn("preferences-identity-save", if state.saving { "Saving…" } else { "Save" })
                        .small()
                        .enabled(ready)
                        .on(view::Action::Identity(Action::Save(Place::Preferences)), cx),
                ),
        )
        .when_some(own, |d, own| {
            d.child(kit::muted(format!("{} has its own, which its commits carry: {own}. Change it from Committing as under its Commit button.", ui.repo)))
        })
}

pub(crate) fn panel_field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    let drawn = ui.identity.form_shown() && ui.working() && !ui.panel_hidden && !ui.operation.active() && !ui.preferences.open();
    !drawn && (ui.identity.name.focus_handle(cx).is_focused(window) || ui.identity.email.focus_handle(cx).is_focused(window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};
    use std::fs;

    fn values(name: Option<&str>, email: Option<&str>) -> Values {
        Values { name: name.map(str::to_owned), email: email.map(str::to_owned) }
    }

    fn nameless() -> (Fixture, tempfile::TempDir, std::path::PathBuf, GlobalConfig) {
        let f = Fixture::new();
        f.git(&["config", "--local", "--unset", "user.name"]);
        f.git(&["config", "--local", "--unset", "user.email"]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gitconfig");
        fs::write(&file, "").unwrap();
        let global = GlobalConfig::set(&file);
        (f, dir, file, global)
    }

    fn config(file: &Path, key: &str) -> Option<String> {
        let output = repository::bundled_command(Path::new("/")).unwrap().args(["config", "--file"]).arg(file).args(["--get", key]).output().unwrap();
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    }

    #[test]
    fn the_listing_is_read_by_scope_and_the_last_value_counts() {
        let listed = b"system\0user.name\nSystem Name\0global\0user.name\nAspen Walker\0global\0user.email\naspen@example.invalid\0global\0core.editor\nvi\0local\0user.email\nwork@example.invalid\0command\0user.useconfigonly\ntrue\0";
        let identity = parse(listed);
        assert_eq!(identity.effective, values(Some("Aspen Walker"), Some("work@example.invalid")));
        assert_eq!(identity.global, values(Some("Aspen Walker"), Some("aspen@example.invalid")));
        assert_eq!(identity.local, values(None, Some("work@example.invalid")));
        let identity = parse(b"global\0user.name\nAspen\0local\0user.name\n  \0worktree\0user.email\0");
        assert_eq!(identity.effective, values(None, None));
        assert_eq!(identity.global, values(Some("Aspen"), None));
        assert_eq!(parse(b""), Identity::default());
    }

    #[test]
    fn the_reason_names_what_is_missing() {
        assert_eq!(reason(&values(Some("A"), Some("a@b"))), None);
        assert_eq!(reason(&values(None, None)).as_deref(), Some("Set your name and email to commit"));
        assert_eq!(reason(&values(None, Some("a@b"))).as_deref(), Some("Set your name to commit"));
        assert_eq!(reason(&values(Some("A"), None)).as_deref(), Some("Set your email to commit"));
        assert_eq!(values(Some("Aspen"), Some("a@b.c")).shown().as_deref(), Some("Aspen <a@b.c>"));
        assert_eq!(values(Some("Aspen"), None).shown(), None);
    }

    #[test]
    fn the_form_checks_the_name_and_the_email() {
        assert_eq!(problem("Aspen Walker", "aspen@example.invalid"), None);
        assert_eq!(problem("  Aspen ", " aspen@example.invalid "), None);
        assert_eq!(problem("", "a@b").as_deref(), Some(""));
        assert_eq!(problem("Aspen", "  ").as_deref(), Some(""));
        assert_eq!(problem("Aspen <a@b>", "a@b").as_deref(), Some("A name cannot contain < or >."));
        assert_eq!(problem("Aspen", "a @b").as_deref(), Some("An email cannot contain spaces, < or >."));
        assert_eq!(problem("Aspen", "aspen").as_deref(), Some("Enter an email address, such as you@example.com."));
        assert!(problem("Aspen", "aspen@").is_some());
    }

    #[test]
    fn the_identity_comes_from_the_repository_or_the_users_own_configuration() {
        let f = Fixture::new();
        let identity = of(&f.repo).unwrap();
        assert_eq!(identity.effective, values(Some("Cedar"), Some("cedar@example.invalid")));
        assert_eq!(identity.local, identity.effective);
        assert_eq!(identity.global, Values::default());
        assert_eq!(reason(&identity.effective), None);
    }

    #[test]
    fn with_an_empty_global_configuration_nothing_can_be_committed() {
        let (f, _dir, _file, _global) = nameless();
        let identity = of(&f.repo).unwrap();
        assert_eq!(identity, Identity::default());
        assert_eq!(reason(&identity.effective).as_deref(), Some("Set your name and email to commit"));
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        let error = f.repo.commit("Start the trail").unwrap_err().text;
        assert!(error.contains("auto-detection is disabled"), "{error}");
        assert_eq!(crate::explain::classify(&error), Some(crate::explain::Cause::Identity));
        let head = repository::run(f.repo.read_command().args(["rev-parse", "--verify", "--quiet", "HEAD"])).unwrap();
        assert!(!head.status.success(), "a commit was made");
        assert_eq!(String::from_utf8(f.git(&["diff", "--cached", "--name-only"])).unwrap(), "trail.txt\n");
    }

    #[test]
    fn saving_for_every_repository_writes_the_users_own_configuration_only() {
        let (f, _dir, file, _global) = nameless();
        let change = Change { scope: Scope::Global, name: " Aspen Walker ".into(), email: "aspen@example.invalid".into(), seen: Values::default() };
        let (identity, wrote) = save(Some(&f.repo), &change).unwrap();
        assert!(wrote);
        assert_eq!(identity.effective, values(Some("Aspen Walker"), Some("aspen@example.invalid")));
        assert_eq!(identity.global, identity.effective);
        assert_eq!(identity.local, Values::default());
        assert_eq!(config(&file, "user.name").as_deref(), Some("Aspen Walker"));
        assert_eq!(config(&file, "user.email").as_deref(), Some("aspen@example.invalid"));
        assert_eq!(config(&f.repo.git_dir().join("config"), "user.name"), None);
        fs::write(f.dir.join("trail.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "trail.txt"]);
        still(&f.repo, &identity.effective).unwrap();
        f.repo.commit("Start the trail").unwrap();
        assert_eq!(String::from_utf8(f.git(&["log", "-1", "--format=%an <%ae>|%cn <%ce>"])).unwrap(), "Aspen Walker <aspen@example.invalid>|Aspen Walker <aspen@example.invalid>\n");
        let again = Change { seen: identity.global.clone(), name: "Aspen Walker".into(), ..change };
        assert_eq!(save(Some(&f.repo), &again).unwrap().1, false);
    }

    #[test]
    fn a_repositorys_own_name_and_email_win_and_leave_the_users_own_alone() {
        let (f, _dir, file, _global) = nameless();
        fs::write(&file, "[user]\n\tname = Aspen Walker\n\temail = aspen@example.invalid\n").unwrap();
        let before = fs::read(&file).unwrap();
        let change = Change { scope: Scope::Local, name: "Aspen at Work".into(), email: "aspen@work.invalid".into(), seen: Values::default() };
        let (identity, wrote) = save(Some(&f.repo), &change).unwrap();
        assert!(wrote);
        assert_eq!(identity.effective, values(Some("Aspen at Work"), Some("aspen@work.invalid")));
        assert_eq!(identity.local, identity.effective);
        assert_eq!(identity.global, values(Some("Aspen Walker"), Some("aspen@example.invalid")));
        assert_eq!(fs::read(&file).unwrap(), before, "the global file changed");
        assert_eq!(config(&f.repo.git_dir().join("config"), "user.email").as_deref(), Some("aspen@work.invalid"));
        assert!(matches!(save(None, &change), Err(Refusal::Failed(_))));
    }

    #[test]
    fn saving_refuses_a_configuration_that_changed_since_the_form_was_filled() {
        let (f, _dir, file, _global) = nameless();
        let filled = of(&f.repo).unwrap();
        fs::write(&file, "[user]\n\tname = Someone Else\n").unwrap();
        let change = Change { scope: Scope::Global, name: "Aspen Walker".into(), email: "aspen@example.invalid".into(), seen: filled.global.clone() };
        match save(Some(&f.repo), &change) {
            Err(Refusal::Changed { now, words }) => {
                assert_eq!(now, values(Some("Someone Else"), None));
                assert!(words.contains("meanwhile"), "{words}");
            }
            other => panic!("saved: {other:?}"),
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), "[user]\n\tname = Someone Else\n");
        f.git(&["config", "--local", "user.email", "own@example.invalid"]);
        let local = Change { scope: Scope::Local, seen: filled.local.clone(), ..change.clone() };
        assert!(matches!(save(Some(&f.repo), &local), Err(Refusal::Changed { .. })));
        assert_eq!(config(&f.repo.git_dir().join("config"), "user.name"), None);
        let bad = Change { email: "aspen".into(), seen: values(Some("Someone Else"), None), ..change };
        assert!(matches!(save(Some(&f.repo), &bad), Err(Refusal::Failed(words)) if words.starts_with("Enter an email")));
        assert_eq!(fs::read_to_string(&file).unwrap(), "[user]\n\tname = Someone Else\n");
    }

    #[test]
    fn a_value_that_looks_like_an_option_is_written_as_it_is() {
        let (f, _dir, file, _global) = nameless();
        let change = Change { scope: Scope::Global, name: "--global".into(), email: "-x@example.invalid".into(), seen: Values::default() };
        save(Some(&f.repo), &change).unwrap();
        assert_eq!(config(&file, "user.name").as_deref(), Some("--global"));
        assert_eq!(config(&file, "user.email").as_deref(), Some("-x@example.invalid"));
        fs::write(&file, "[user]\n\tname = One\n\tname = Two\n\temail = a@b.c\n").unwrap();
        let change = Change { scope: Scope::Global, name: "Three".into(), email: "a@b.c".into(), seen: values(Some("Two"), Some("a@b.c")) };
        save(Some(&f.repo), &change).unwrap();
        assert_eq!(config(&file, "user.name").as_deref(), Some("Three"));
    }

    #[test]
    fn a_commit_is_refused_when_the_identity_changed_since_the_panel_showed_it() {
        let f = Fixture::new();
        let shown = of(&f.repo).unwrap().effective;
        still(&f.repo, &shown).unwrap();
        f.git(&["config", "--local", "user.email", "other@example.invalid"]);
        let error = still(&f.repo, &shown).unwrap_err();
        assert!(error.starts_with("Not Committed — Commits here now carry Cedar <other@example.invalid>"), "{error}");
        f.git(&["config", "--local", "--unset", "user.name"]);
        let error = still(&f.repo, &shown).unwrap_err();
        assert!(error.contains("no longer has a name and email"), "{error}");
    }

    #[test]
    fn a_name_and_email_the_users_own_configuration_includes_for_a_repository_are_said() {
        let (f, dir, file, _global) = nameless();
        let work = dir.path().join("work.gitconfig");
        fs::write(&work, "[user]\n\temail = aspen@work.invalid\n").unwrap();
        let git_dir = fs::canonicalize(f.repo.git_dir()).unwrap();
        fs::write(
            &file,
            format!("[user]\n\tname = Aspen\n\temail = aspen@home.invalid\n[includeIf \"gitdir:{}\"]\n\tpath = {}\n", git_dir.display(), work.display()),
        )
        .unwrap();
        let filled = of(&f.repo).unwrap();
        assert_eq!(filled.effective, values(Some("Aspen"), Some("aspen@work.invalid")));
        let change = Change { scope: Scope::Global, name: "Aspen".into(), email: "aspen@new.invalid".into(), seen: filled.global };
        let (identity, wrote) = save(Some(&f.repo), &change).unwrap();
        assert!(wrote);
        assert_eq!(config(&file, "user.email").as_deref(), Some("aspen@new.invalid"));
        assert_eq!(identity.effective, values(Some("Aspen"), Some("aspen@work.invalid")));
        let notice = saved(Place::Panel, &change, &identity, wrote, "trail");
        assert_eq!(notice.title, "Name and email saved");
        assert_eq!(notice.detail, "Commits in every repository carry Aspen <aspen@new.invalid>. trail keeps its own: Aspen <aspen@work.invalid>.");
        let notice = saved(Place::Preferences, &change, &identity, false, "trail");
        assert_eq!((notice.title.as_str(), notice.detail.as_str()), ("Name and email unchanged", "Commits in every repository carry Aspen <aspen@new.invalid>."));
        let local = Change { scope: Scope::Local, ..change };
        assert_eq!(saved(Place::Panel, &local, &identity, true, "trail").detail, "Commits in trail carry Aspen <aspen@new.invalid>. Other repositories are unchanged.");
    }

    #[test]
    fn outside_a_repository_only_the_users_own_configuration_counts() {
        let (_f, _dir, file, _global) = nameless();
        fs::write(&file, "[user]\n\tname = Aspen Walker\n").unwrap();
        let identity = outside().unwrap();
        assert_eq!(identity.global, values(Some("Aspen Walker"), None));
        assert_eq!(identity.local, Values::default());
        let change = Change { scope: Scope::Global, name: "Aspen Walker".into(), email: "aspen@example.invalid".into(), seen: identity.global };
        let (after, wrote) = save(None, &change).unwrap();
        assert!(wrote);
        assert_eq!(after.global, values(Some("Aspen Walker"), Some("aspen@example.invalid")));
    }
}
