use crate::{
    Tab, autofetch,
    commands::{Availability, Bar, Command, Group, Target},
    kit, perf,
    settings::{self, Memory},
    theme::*,
    view::{self, Centre, Desk, Tone, Ui},
};
use gpui::{
    AnyWindowHandle, App, AppContext, Context, DispatchPhase, Entity, EntityId, ExternalPaths, FontWeight, Global, IntoElement,
    MouseButton, MouseDownEvent, Render, Subscription, WeakEntity, Window, WindowHandle, canvas, div, prelude::*, rgb,
};
use crate::theme::px;
use std::path::{Path, PathBuf};

pub(crate) const NUMBERED: usize = 9;
pub(crate) const STRIP: f32 = 30.;
const REM: f32 = 16.;

pub(crate) const COMMANDS: &[Command] = {
    fn anywhere(_: &Ui, target: &Target, _: &App) -> Availability {
        if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
    }
    fn several(_: &Ui, target: &Target, cx: &App) -> Availability {
        if *target != Target::Repo {
            return Availability::Hidden;
        }
        Availability::unless((count(cx) < 2).then(|| "There is only one tab".to_owned()))
    }
    fn closing(ui: &Ui, target: &Target, cx: &App) -> Availability {
        if let Target::Tab(index) = target {
            return Availability::unless((*index >= count(cx)).then(|| "That tab has closed".to_owned()));
        }
        if *target != Target::Repo {
            return Availability::Hidden;
        }
        if !ui.repo.is_empty() && covered(ui) {
            return Availability::Enabled;
        }
        if count(cx) < 2 && ui.repo.is_empty() {
            return Availability::Disabled("This is the only tab".to_owned());
        }
        Availability::unless(refusal(ui))
    }
    &[
        Command::new("tab.new", Group::Tabs, |_, _| "New tab".into(), anywhere, |_, _| {
            Some(view::Action::Tabs(Action::New))
        })
        .words(&["another repository", "window"])
        .icon("plus")
        .keys("cmd-t")
        .bar(Bar::File),
        Command::new(
            "tab.close",
            Group::Tabs,
            |ui, target| {
                match view::centre(ui) {
                    _ if matches!(target, Target::Tab(_)) => "Close tab",
                    _ if ui.repo.is_empty() => "Close tab",
                    Centre::Activity => "Close the activity log",
                    Centre::Diff | Centre::Resolver | Centre::Chooser => "Close the open file",
                    Centre::FileHistory => "Close file history",
                    Centre::Blame => "Close blame",
                    Centre::Browse => "Close the file browser",
                    Centre::RebaseEditor => "Cancel the interactive rebase",
                    Centre::Search | Centre::History => "Close tab",
                }
                .into()
            },
            closing,
            |ui, target| {
                Some(match view::centre(ui) {
                    _ if matches!(target, Target::Tab(_)) => view::Action::Tabs(Action::Close(index_of_target(target))),
                    _ if ui.repo.is_empty() => view::Action::Tabs(Action::Close(None)),
                    Centre::Activity => view::Action::Activity(crate::activity::Action::Close),
                    Centre::Resolver => view::Action::Merge(crate::merge::Action::Close),
                    Centre::Chooser => view::Action::Conflict(crate::conflict::Action::Close),
                    Centre::Diff => view::Action::CloseDiff,
                    Centre::FileHistory => view::Action::FileHistory(crate::file_history::Action::Close),
                    Centre::Blame => view::Action::Blame(crate::blame::Action::Close),
                    Centre::Browse => view::Action::Browse(crate::browse::Action::Close),
                    Centre::RebaseEditor => view::Action::RebaseEditor(crate::rebase_editor::Action::Cancel),
                    Centre::Search | Centre::History => view::Action::Tabs(Action::Close(None)),
                })
            },
        )
        .words(&["back to graph", "close file", "close diff"])
        .icon("x")
        .keys("cmd-w")
        .bar(Bar::File),
        Command::new("tab.next", Group::Tabs, |_, _| "Show next tab".into(), several, |_, _| {
            Some(view::Action::Tabs(Action::Next))
        })
        .words(&["switch tab", "other repository"])
        .keys("ctrl-tab")
        .bar(Bar::Window),
        Command::new("tab.previous", Group::Tabs, |_, _| "Show previous tab".into(), several, |_, _| {
            Some(view::Action::Tabs(Action::Previous))
        })
        .words(&["switch tab", "other repository"])
        .keys("ctrl-shift-tab")
        .bar(Bar::Window),
        Command::new("tab.reopen", Group::Tabs, |_, _| "Reopen closed tab".into(), reopenable, |_, _| {
            Some(view::Action::Tabs(Action::Reopen))
        })
        .words(&["closed recently", "undo close tab", "restore tab", "last closed"])
        .icon("undo")
        .keys("cmd-shift-t")
        .bar(Bar::File),
        Command::new("tab.rename", Group::Tabs, |_, _| "Rename tab…".into(), on_a_tab, |_, target| {
            Some(view::Action::Tabs(Action::Rename(index_of_target(target))))
        })
        .words(&["tab name", "alias", "label tab", "worktree"])
        .icon("pencil")
        .bar(Bar::Window),
        Command::new("tab.close-others", Group::Tabs, |_, _| "Close other tabs".into(), others_open, |_, target| {
            Some(view::Action::Tabs(Action::CloseOthers(index_of_target(target))))
        })
        .words(&["close all but this", "keep only this tab"])
        .icon("x"),
        Command::new("tab.close-right", Group::Tabs, |_, _| "Close tabs to the right".into(), right_open, |_, target| {
            Some(view::Action::Tabs(Action::CloseRight(index_of_target(target))))
        })
        .words(&["close later tabs"])
        .icon("x"),
        Command::new("tab.move-left", Group::Tabs, |_, _| "Move tab left".into(), |ui, target, cx| movable(ui, target, cx, -1), |_, target| {
            Some(view::Action::Tabs(Action::Shift(index_of_target(target), -1)))
        })
        .words(&["reorder tabs", "move tab earlier"]),
        Command::new("tab.move-right", Group::Tabs, |_, _| "Move tab right".into(), |ui, target, cx| movable(ui, target, cx, 1), |_, target| {
            Some(view::Action::Tabs(Action::Shift(index_of_target(target), 1)))
        })
        .words(&["reorder tabs", "move tab later"]),
    ]
};

fn index_of_target(target: &Target) -> Option<usize> {
    match target {
        Target::Tab(index) => Some(*index),
        _ => None,
    }
}

fn target_index(target: &Target, cx: &App) -> Option<usize> {
    match target {
        Target::Tab(index) => Some(*index).filter(|index| *index < count(cx)),
        Target::Repo => Some(strip(cx).map_or(0, |strip| strip.front)),
        _ => None,
    }
}

fn reopenable(_: &Ui, target: &Target, cx: &App) -> Availability {
    if !matches!(target, Target::Repo | Target::Tab(_)) {
        return Availability::Hidden;
    }
    Availability::unless(last_closed(cx).is_none().then(|| "No tab has been closed".to_owned()))
}

fn on_a_tab(_: &Ui, target: &Target, cx: &App) -> Availability {
    match target_index(target, cx) {
        Some(_) => Availability::Enabled,
        None => Availability::Hidden,
    }
}

fn others_open(_: &Ui, target: &Target, cx: &App) -> Availability {
    if target_index(target, cx).is_none() {
        return Availability::Hidden;
    }
    Availability::unless((count(cx) < 2).then(|| "There is only one tab".to_owned()))
}

fn right_open(_: &Ui, target: &Target, cx: &App) -> Availability {
    let Some(index) = target_index(target, cx) else { return Availability::Hidden };
    Availability::unless((index + 1 >= count(cx)).then(|| "No tab is to the right of this one".to_owned()))
}

fn movable(_: &Ui, target: &Target, cx: &App, by: isize) -> Availability {
    let Some(index) = target_index(target, cx) else { return Availability::Hidden };
    let len = count(cx);
    let blocked = if by < 0 && index == 0 {
        Some("This is the first tab")
    } else if by > 0 && index + 1 >= len {
        Some("This is the last tab")
    } else {
        None
    };
    Availability::unless(blocked.map(str::to_owned))
}

fn covered(ui: &Ui) -> bool {
    matches!(view::centre(ui), Centre::Activity | Centre::Diff | Centre::FileHistory | Centre::Blame | Centre::Browse | Centre::Resolver | Centre::Chooser)
}

pub(crate) fn refusal(ui: &Ui) -> Option<String> {
    if let Some(running) = ui.jobs.refusal() {
        return Some(format!("{running}. Cancel it or let it finish before closing the tab."));
    }
    if ui.repos.running() {
        return Some(crate::repos::BUSY.to_owned());
    }
    (!ui.jobs.idle_but_background()).then(|| "Wait for the current operation to finish".to_owned())
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastkraken_desk, no_json)]
pub(crate) struct ShowTab {
    pub index: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    New,
    Show(usize),
    Next,
    Previous,
    Close(Option<usize>),
    Reopen,
    Rename(Option<usize>),
    CloseOthers(Option<usize>),
    CloseRight(Option<usize>),
    Shift(Option<usize>, isize),
    Move { from: usize, to: usize },
    Bring(EntityId),
}

pub(crate) enum Request {
    New,
    Open { path: PathBuf, then: Then },
    Show(usize),
    Step(isize),
    Close(EntityId),
    Handed(PathBuf),
    Linked(PathBuf),
    Clone(String),
    Tell(crate::notice::Notice),
    Reopen,
    CloseOthers(EntityId),
    CloseRight(EntityId),
    Move { from: usize, to: usize },
    Bring(EntityId),
    Name(EntityId, Option<String>),
    Manage,
}

pub(crate) const CLOSED_KEPT: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Closed {
    pub(crate) root: PathBuf,
    pub(crate) name: Option<String>,
    pub(crate) at: usize,
}

#[derive(Default)]
struct ClosedTabs(Vec<Closed>);

impl Global for ClosedTabs {}

pub(crate) fn remember_closed(root: PathBuf, name: Option<String>, at: usize, cx: &mut App) {
    let closed = &mut cx.default_global::<ClosedTabs>().0;
    closed.retain(|earlier| earlier.root != root);
    closed.push(Closed { root, name, at });
    let excess = closed.len().saturating_sub(CLOSED_KEPT);
    closed.drain(..excess);
}

fn pop_existing(closed: &mut Vec<Closed>) -> Option<Closed> {
    while let Some(last) = closed.pop() {
        if last.root.is_dir() {
            return Some(last);
        }
    }
    None
}

pub(crate) fn last_closed(cx: &App) -> Option<&Closed> {
    cx.try_global::<ClosedTabs>().and_then(|closed| closed.0.last())
}

fn take_closed(cx: &mut App) -> Option<Closed> {
    pop_existing(&mut cx.default_global::<ClosedTabs>().0)
}

pub(crate) fn index_of(id: EntityId, cx: &App) -> Option<usize> {
    strip(cx).and_then(|strip| strip.tabs.iter().position(|shown| shown.id == id))
}

pub(crate) fn open_roots(cx: &App) -> Vec<PathBuf> {
    strip(cx).map_or_else(Vec::new, |strip| strip.tabs.iter().filter_map(|shown| shown.root.clone()).collect())
}

pub(crate) fn moved(front: usize, from: usize, to: usize) -> usize {
    if front == from {
        to
    } else if from < front && to >= front {
        front - 1
    } else if from > front && to <= front {
        front + 1
    } else {
        front
    }
}

pub(crate) fn inside(path: &Path, root: &Path) -> bool {
    path.starts_with(root) && path.ancestors().take_while(|folder| *folder != root).all(|folder| !folder.join(".git").exists())
}

fn engaged(tab: &Tab) -> bool {
    let ui = &tab.ui;
    ui.dialog_open() || ui.modal_open() || ui.preferences.open() || ui.menu.is_some()
}

pub(crate) const OPENED_BEHIND: &str = "It waits behind this tab, so nothing you were doing here was interrupted.";

pub(crate) type Then = Box<dyn FnOnce(&mut Tab, &mut Context<Tab>)>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) id: EntityId,
    pub(crate) title: String,
    pub(crate) name: Option<String>,
    pub(crate) root: Option<PathBuf>,
    pub(crate) busy: bool,
    pub(crate) asking: bool,
    pub(crate) stale: bool,
}

pub(crate) struct Strip {
    pub(crate) tabs: Vec<Shown>,
    pub(crate) front: usize,
    desk: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl Global for Strip {}

pub(crate) fn strip(cx: &App) -> Option<&Strip> {
    cx.try_global::<Strip>()
}

pub(crate) fn count(cx: &App) -> usize {
    strip(cx).map_or(1, |strip| strip.tabs.len().max(1))
}

pub(crate) fn in_front(id: EntityId, cx: &App) -> bool {
    strip(cx).is_none_or(|strip| strip.tabs.get(strip.front).is_none_or(|shown| shown.id == id))
}

fn in_front_at(index: usize, cx: &App) -> bool {
    strip(cx).is_none_or(|strip| strip.front == index)
}

pub(crate) fn holding(root: &Path, except: EntityId, cx: &App) -> Option<usize> {
    let strip = strip(cx)?;
    strip.tabs.iter().position(|shown| shown.id != except && shown.root.as_deref() == Some(root))
}

pub(crate) fn present(cx: &App) -> bool {
    strip(cx).is_some_and(|strip| strip.desk.as_ref().is_some_and(|(_, desk)| desk.upgrade().is_some()))
}

pub(crate) fn window(cx: &App) -> Option<AnyWindowHandle> {
    strip(cx).and_then(|strip| strip.desk.as_ref()).map(|(window, _)| *window)
}

pub(crate) fn ask(cx: &mut App, request: Request) -> bool {
    let Some((window, desk)) = strip(cx).and_then(|strip| strip.desk.clone()) else { return false };
    if desk.upgrade().is_none() {
        return false;
    }
    cx.defer(move |cx| {
        let _ = cx.update_window(window, |_, window, cx| {
            let _ = desk.update(cx, |desk, cx| desk.handle(request, window, cx));
        });
    });
    true
}

pub(crate) fn open_elsewhere(
    path: PathBuf,
    then: impl FnOnce(&mut Tab, &mut Context<Tab>) + 'static,
    cx: &mut App,
) -> bool {
    present(cx) && ask(cx, Request::Open { path, then: Box::new(then) })
}

pub(crate) fn title(ui: &Ui) -> String {
    if !ui.repo.is_empty() {
        return ui.repo.clone();
    }
    match &ui.opening {
        Some(path) => {
            let path = Path::new(path);
            let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
            format!("Opening {name}…")
        }
        None if ui.repos.open => crate::repos::HEADING.to_owned(),
        None => "New tab".to_owned(),
    }
}

impl Shown {
    fn of(tab: &Entity<Tab>, cx: &App) -> Self {
        let open = tab.read(cx);
        let title = match (&open.name, &open.repo) {
            (Some(name), _) => name.clone(),
            (None, Some(repo)) => crate::repos::display_name(&repo.root, cx).unwrap_or_else(|| title(&open.ui)),
            (None, None) => title(&open.ui),
        };
        Self {
            id: tab.entity_id(),
            title,
            name: open.name.clone(),
            root: open.repo.as_ref().map(|repo| repo.root.clone()),
            busy: !open.settled(),
            asking: open.ui.prompt.open(),
            stale: open.stale.is_some(),
        }
    }
}

pub(crate) fn stepped(front: usize, len: usize, by: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (front as isize + by).rem_euclid(len as isize) as usize
}

pub(crate) fn after_close(front: usize, closed: usize, len: usize) -> usize {
    let left = len.saturating_sub(1);
    let front = if closed < front { front - 1 } else { front };
    front.min(left.saturating_sub(1))
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    let me = cx.entity_id();
    if tab.ui.dialog_open() {
        tab.ui.notices.show(crate::notice::Notice::new(Tone::Info, "Close the open dialog first", ""));
        cx.notify();
        return;
    }
    match action {
        Action::New => {
            ask(cx, Request::New);
        }
        Action::Show(index) => {
            let len = count(cx);
            if index >= len {
                let shown = match len {
                    1 => "There is only one tab".to_owned(),
                    n => format!("There are {n} tabs"),
                };
                tab.ui.notices.show(crate::notice::Notice::new(Tone::Info, format!("There is no tab {}", index + 1), shown));
            } else if !in_front_at(index, cx) {
                ask(cx, Request::Show(index));
            }
        }
        Action::Next | Action::Previous if count(cx) < 2 => {
            tab.refuse("There is only one tab".to_owned(), cx);
        }
        Action::Next => {
            ask(cx, Request::Step(1));
        }
        Action::Previous => {
            ask(cx, Request::Step(-1));
        }
        Action::Reopen => {
            if last_closed(cx).is_none() {
                tab.refuse("No tab has been closed".to_owned(), cx);
            } else if !ask(cx, Request::Reopen) {
                reopen_here(tab, cx);
            }
        }
        Action::Rename(index) => {
            let Some(id) = chosen(index, me, cx) else { return };
            let current = if id == me {
                tab.name.clone()
            } else {
                strip(cx).and_then(|strip| strip.tabs.iter().find(|shown| shown.id == id)).and_then(|shown| shown.name.clone())
            };
            crate::repos::rename_tab(tab, id, current, cx);
        }
        Action::CloseOthers(index) => {
            let Some(id) = chosen(index, me, cx) else { return };
            if count(cx) < 2 {
                tab.refuse("There is only one tab".to_owned(), cx);
            } else {
                ask(cx, Request::CloseOthers(id));
            }
        }
        Action::CloseRight(index) => {
            let Some(id) = chosen(index, me, cx) else { return };
            ask(cx, Request::CloseRight(id));
        }
        Action::Shift(index, by) => {
            let Some(from) = index.or_else(|| index_of(me, cx)).or(Some(0)).filter(|from| *from < count(cx)) else { return };
            let to = from as isize + by;
            if to < 0 || to as usize >= count(cx) {
                tab.refuse(if by < 0 { "This is the first tab" } else { "This is the last tab" }.to_owned(), cx);
            } else {
                ask(cx, Request::Move { from, to: to as usize });
            }
        }
        Action::Move { from, to } => {
            ask(cx, Request::Move { from, to });
        }
        Action::Bring(id) => {
            ask(cx, Request::Bring(id));
        }
        Action::Close(index) => {
            let id = match index {
                Some(index) => match strip(cx).and_then(|strip| strip.tabs.get(index)) {
                    Some(shown) => shown.id,
                    None => return,
                },
                None => me,
            };
            if id != me {
                ask(cx, Request::Close(id));
            } else if let Some(reason) = refusal(&tab.ui) {
                tab.refuse(reason, cx);
            } else if count(cx) < 2 {
                tab.close(cx);
                view::focus(&tab.ui, window);
            } else {
                ask(cx, Request::Close(id));
            }
        }
    }
    cx.notify();
}

fn chosen(index: Option<usize>, me: EntityId, cx: &App) -> Option<EntityId> {
    match index {
        Some(index) => strip(cx).and_then(|strip| strip.tabs.get(index)).map(|shown| shown.id),
        None => Some(me),
    }
}

fn reopen_here(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.repo.is_some() || tab.ui.opening.is_some() || !tab.can_open() {
        tab.refuse("Close this repository first to reopen the closed one here".to_owned(), cx);
        return;
    }
    let Some(closed) = take_closed(cx) else {
        tab.refuse("The closed repositories are no longer there".to_owned(), cx);
        return;
    };
    tab.name = closed.name;
    tab.open(closed.root, cx);
}

fn forward(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    if std::mem::replace(&mut tab.front, true) {
        return;
    }
    tab.ui.title_shown = None;
    tab.ui.bar_shown.clear();
    view::focus(&tab.ui, window);
    crate::prompt::refocus(&mut tab.ui);
    mirror(tab, cx);
    let refs = tab.stale.take().unwrap_or(false);
    tab.refresh(refs, cx);
    cx.notify();
}

pub(crate) fn mirror(tab: &mut Tab, cx: &mut Context<Tab>) {
    let chosen = settings::settings(cx);
    let ui = &mut tab.ui;
    ui.sidebar_hidden = chosen.sidebar_hidden;
    ui.panel_hidden = chosen.panel_hidden;
    let mode_changed = ui.diff_mode != chosen.diff_mode;
    ui.diff_mode = chosen.diff_mode;
    if mode_changed {
        ui.rewrap();
    }
    ui.refs.show_labels = chosen.show_ref_labels;
    ui.refs.hidden_sections = chosen.hidden_sections.clone();
    ui.graph_columns.layout = chosen.graph_columns.clone();
    ui.sync.prune = chosen.prune_on_fetch;
    ui.autostash.always = chosen.stash_in_the_way;
    ui.tools.editor = chosen.editor.clone();
    ui.tools.terminal = chosen.terminal.clone();
    if ui.tools.custom != chosen.editor_command {
        ui.tools.custom = chosen.editor_command.clone();
        let field = ui.tools.command.clone();
        field.update(cx, |field, cx| field.set_text(&chosen.editor_command, cx));
    }
    if tab.ui.syntax.on != chosen.colour_code {
        crate::syntax::follow(tab, chosen.colour_code, cx);
    }
    crate::diff_options::follow(tab, &chosen, cx);
    crate::diff_text::follow(tab, &chosen);
}


type Session = (Vec<PathBuf>, Option<PathBuf>, Vec<(PathBuf, String)>);

pub(crate) struct Workspace {
    tabs: Vec<Entity<Tab>>,
    watching: Vec<Subscription>,
    front: usize,
    saved: Option<Session>,
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>, bench: Option<perf::Run>) -> Self {
        cx.set_global(Strip { tabs: Vec::new(), front: 0, desk: Some((window.window_handle(), cx.weak_entity())) });
        let mut desk = Self { tabs: Vec::new(), watching: Vec::new(), front: 0, saved: None };
        cx.observe_window_appearance(window, |_, window, cx| crate::appearance::system_changed(window.appearance(), cx))
            .detach();
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                crate::appearance::window_activated(cx);
            }
        })
        .detach();
        if bench.is_none() {
            crate::updates::arm(cx);
        }
        desk.add(bench, cx);
        desk
    }

    pub(crate) fn front(&self) -> &Entity<Tab> {
        &self.tabs[self.front]
    }

    fn add(&mut self, bench: Option<perf::Run>, cx: &mut Context<Self>) -> Entity<Tab> {
        let tab = cx.new(|cx| Tab::new(cx, bench));
        self.watching.push(cx.observe(&tab, |desk, _, cx| desk.sync(cx)));
        self.tabs.push(tab.clone());
        tab
    }

    pub(crate) fn launch(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let (session, front, names) = match settings::memory(cx) {
            Some(memory) if settings::settings(cx).reopen_last => {
                let session = memory.session();
                let names: Vec<Option<String>> = session.iter().map(|root| memory.tab_name_of(root).map(str::to_owned)).collect();
                (session, memory.last_repository(), names)
            }
            _ => (Vec::new(), None, Vec::new()),
        };
        let (here, gone): (Vec<(PathBuf, Option<String>)>, Vec<(PathBuf, Option<String>)>) =
            session.into_iter().zip(names.into_iter().chain(std::iter::repeat(None))).partition(|(root, _)| root.exists());
        let gone: Vec<PathBuf> = gone.into_iter().map(|(root, _)| root).collect();
        let mut front = front.and_then(|front| here.iter().position(|(root, _)| *root == front)).unwrap_or(0);
        for (index, (root, name)) in here.iter().enumerate() {
            let tab = if index == 0 { self.tabs[0].clone() } else { self.add(None, cx) };
            tab.update(cx, |tab, cx| {
                tab.name = name.clone();
                tab.open(root.clone(), cx)
            });
        }
        let here: Vec<PathBuf> = here.into_iter().map(|(root, _)| root).collect();
        let restored = path.as_ref().and_then(|path| {
            let path = std::fs::canonicalize(path).ok()?;
            here.iter()
                .enumerate()
                .filter(|(_, root)| inside(&path, root))
                .max_by_key(|(_, root)| root.as_os_str().len())
                .map(|(index, _)| index)
        });
        if let Some(index) = restored {
            front = index;
        } else if let Some(path) = path {
            let tab = if here.is_empty() { self.tabs[0].clone() } else { self.add(None, cx) };
            front = self.tabs.len() - 1;
            tab.update(cx, |tab, cx| {
                tab.transient = !here.is_empty();
                tab.open(path, cx);
            });
        }
        self.front = front;
        for (index, tab) in self.tabs.iter().enumerate() {
            tab.update(cx, |tab, _| tab.front = index == front);
        }
        let opened = !here.is_empty() || self.tabs.len() > 1 || self.tabs[0].read(cx).ui.opening.is_some();
        let shown = self.tabs[front].clone();
        shown.update(cx, |tab, cx| {
            if !gone.is_empty() {
                let names = gone.iter().map(|root| root.display().to_string()).collect::<Vec<_>>();
                let missing = match names.as_slice() {
                    [one] => format!("The last repository, {one}, is no longer there. Choose another to open."),
                    many => format!("The last repositories {} are no longer there.", many.join(", ")),
                };
                let notice = if opened { None } else { settings::take_startup_notice(cx) };
                let text = match notice {
                    Some(notice) => format!("{missing} {notice}"),
                    None => missing,
                };
                tab.ui.notices.show(
                    crate::notice::Notice::from_text(Tone::Info, &text).offer("Open repository…", view::Action::Open),
                );
            } else if !opened {
                if let Some(notice) = settings::take_startup_notice(cx) {
                    tab.ui.say(Tone::Info, notice);
                }
            }
            if !opened {
                crate::guide::deliver(tab, cx);
            }
            view::focus(&tab.ui, window);
            cx.notify();
        });
        self.sync(cx);
    }

    fn handle(&mut self, request: Request, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            Request::New => {
                self.add(None, cx);
                self.bring(self.tabs.len() - 1, window, cx);
            }
            Request::Open { path, then } => {
                if let Some(index) = self.holding(&path, cx) {
                    return self.bring(index, window, cx);
                }
                let front = self.front().clone();
                let behind = engaged(front.read(cx));
                let tab = self.add(None, cx);
                if behind {
                    tab.update(cx, |tab, _| tab.front = false);
                    let name = crate::repos::display_name(&path, cx).unwrap_or_else(|| crate::repos::folder_name(&path));
                    let notice = crate::notice::Notice::new(Tone::Info, format!("Opening {name} in a new tab"), OPENED_BEHIND)
                        .offer("Show", view::Action::Tabs(Action::Bring(tab.entity_id())));
                    front.update(cx, |front, cx| {
                        front.ui.notices.show(notice);
                        cx.notify();
                    });
                    self.sync(cx);
                } else {
                    self.bring(self.tabs.len() - 1, window, cx);
                }
                tab.update(cx, |tab, cx| {
                    tab.transient = true;
                    tab.open_then(path, then, cx);
                });
            }
            Request::Show(index) if index < self.tabs.len() => self.bring(index, window, cx),
            Request::Show(_) => {}
            Request::Step(by) => self.bring(stepped(self.front, self.tabs.len(), by), window, cx),
            Request::Close(id) => self.close(id, window, cx),
            Request::Handed(path) => {
                if let Some(index) = self.holding(&path, cx) {
                    return self.bring(index, window, cx);
                }
                let front = self.front().clone();
                let free = {
                    let tab = front.read(cx);
                    tab.repo.is_none() && tab.ui.opening.is_none() && tab.can_open() && !engaged(tab) && !tab.ui.repos.running()
                };
                if free {
                    front.update(cx, |tab, cx| {
                        tab.open(path, cx);
                        view::focus(&tab.ui, window);
                    });
                } else {
                    self.handle(Request::Open { path, then: Box::new(|_, _| {}) }, window, cx);
                }
            }
            Request::Linked(path) => {
                if let Some(index) = self.holding(&path, cx) {
                    return self.bring(index, window, cx);
                }
                self.handle(Request::Tell(crate::links::offer(&path)), window, cx);
            }
            Request::Clone(address) => self.clone_from_link(address, window, cx),
            Request::Tell(notice) => {
                let front = self.front().clone();
                if front.read(cx).ui.opening.is_some() {
                    crate::guide::wait(notice, cx);
                } else {
                    front.update(cx, |tab, cx| {
                        tab.ui.notices.show(notice);
                        cx.notify();
                    });
                }
            }
            Request::Reopen => self.reopen(window, cx),
            Request::CloseOthers(id) => {
                let ids: Vec<EntityId> = self.tabs.iter().map(|tab| tab.entity_id()).filter(|other| *other != id).collect();
                self.close_many(id, ids, window, cx);
            }
            Request::CloseRight(id) => {
                let Some(at) = self.position(id) else { return };
                let ids: Vec<EntityId> = self.tabs[at + 1..].iter().map(|tab| tab.entity_id()).collect();
                if ids.is_empty() {
                    let front = self.front().clone();
                    front.update(cx, |tab, cx| tab.refuse("No tab is to the right of this one".to_owned(), cx));
                    return;
                }
                self.close_many(id, ids, window, cx);
            }
            Request::Move { from, to } => {
                self.shift(from, to, cx);
            }
            Request::Bring(id) => {
                if let Some(index) = self.position(id) {
                    self.bring(index, window, cx);
                }
            }
            Request::Name(id, name) => {
                let Some(index) = self.position(id) else { return };
                let tab = self.tabs[index].clone();
                tab.update(cx, |tab, cx| {
                    tab.name = name;
                    tab.ui.bar_shown.clear();
                    cx.notify();
                });
                self.sync(cx);
            }
            Request::Manage => {
                let index = self.tabs.iter().position(|tab| {
                    let tab = tab.read(cx);
                    tab.repo.is_none() && tab.ui.opening.is_none() && !tab.ui.dialog_open() && !tab.ui.modal_open()
                });
                let index = match index {
                    Some(index) => index,
                    None => {
                        self.add(None, cx);
                        self.tabs.len() - 1
                    }
                };
                self.bring(index, window, cx);
                let tab = self.tabs[index].clone();
                tab.update(cx, |tab, cx| tab.dispatch(view::Action::Repos(crate::repos::Action::Open), window, cx));
            }
        }
    }

    fn position(&self, id: EntityId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.entity_id() == id)
    }

    fn shift(&mut self, from: usize, to: usize, cx: &mut Context<Self>) -> bool {
        let len = self.tabs.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        let watch = self.watching.remove(from);
        self.watching.insert(to, watch);
        self.front = moved(self.front, from, to);
        self.sync(cx);
        cx.notify();
        true
    }

    fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = take_closed(cx) else {
            let front = self.front().clone();
            front.update(cx, |tab, cx| tab.refuse("The closed repositories are no longer there".to_owned(), cx));
            return;
        };
        if let Some(index) = self.holding(&closed.root, cx) {
            return self.bring(index, window, cx);
        }
        let front = self.front().clone();
        let free = {
            let tab = front.read(cx);
            tab.repo.is_none() && tab.ui.opening.is_none() && tab.can_open() && !tab.ui.dialog_open() && !tab.ui.modal_open() && !tab.ui.repos.running()
        };
        let tab = if free {
            front
        } else {
            let tab = self.add(None, cx);
            let at = closed.at.min(self.tabs.len() - 1);
            self.shift(self.tabs.len() - 1, at, cx);
            self.bring(at, window, cx);
            tab
        };
        tab.update(cx, |tab, cx| {
            tab.name = closed.name;
            tab.open(closed.root, cx);
            view::focus(&tab.ui, window);
        });
        self.sync(cx);
    }

    fn close_many(&mut self, keep: EntityId, ids: Vec<EntityId>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.position(keep) else { return };
        self.bring(at, window, cx);
        let mut busy = 0;
        for id in ids {
            let Some(index) = self.position(id) else { continue };
            if refusal(&self.tabs[index].read(cx).ui).is_some() {
                busy += 1;
                continue;
            }
            self.remove(index, cx);
        }
        if let Some(index) = self.position(keep) {
            self.front = index;
        }
        self.bring(self.front, window, cx);
        if busy > 0 {
            let kept = match busy {
                1 => "1 tab is".to_owned(),
                n => format!("{n} tabs are"),
            };
            let notice = crate::notice::Notice::new(
                Tone::Info,
                format!("{kept} still busy, so it stayed open"),
                "Close it when its job has finished.",
            );
            let front = self.front().clone();
            front.update(cx, |tab, cx| {
                tab.ui.notices.show(notice);
                cx.notify();
            });
        }
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        let tab = self.tabs[index].clone();
        let closed = tab.update(cx, |tab, _| {
            autofetch::stop(tab);
            tab.repo.as_ref().map(|repo| (repo.root.clone(), tab.name.clone()))
        });
        if let Some((root, name)) = closed {
            remember_closed(root, name, index, cx);
        }
        self.tabs.remove(index);
        drop(self.watching.remove(index));
        if index < self.front {
            self.front -= 1;
        }
        self.front = self.front.min(self.tabs.len().saturating_sub(1));
    }

    fn clone_from_link(&mut self, address: String, window: &mut Window, cx: &mut Context<Self>) {
        let linked = view::Action::Clone(crate::clone::Action::Linked(address));
        let front = self.front().clone();
        let free = {
            let ui = &front.read(cx).ui;
            ui.opening.is_none() && !ui.dialog_open() && !ui.modal_open() && !ui.preferences.open()
        };
        if free {
            front.update(cx, |tab, cx| tab.dispatch(linked.clone(), window, cx));
            if front.read(cx).ui.cloning.linked() {
                return;
            }
        }
        let tab = self.add(None, cx);
        self.bring(self.tabs.len() - 1, window, cx);
        tab.update(cx, |tab, cx| tab.dispatch(linked, window, cx));
    }

    fn holding(&self, path: &Path, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.read(cx).repo.as_ref().is_some_and(|repo| repo.root == path))
    }

    fn bring(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let index = index.min(self.tabs.len() - 1);
        self.front = index;
        for (at, tab) in self.tabs.iter().enumerate() {
            if at == index {
                tab.update(cx, |tab, cx| forward(tab, window, cx));
            } else {
                tab.update(cx, |tab, _| tab.front = false);
            }
        }
        self.sync(cx);
        cx.notify();
    }

    fn close(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.entity_id() == id) else { return };
        let tab = self.tabs[index].clone();
        if let Some(reason) = refusal(&tab.read(cx).ui) {
            self.bring(index, window, cx);
            tab.update(cx, |tab, cx| tab.refuse(reason, cx));
            return;
        }
        if self.tabs.len() == 1 {
            tab.update(cx, |tab, cx| tab.close(cx));
            return;
        }
        let front = after_close(self.front, index, self.tabs.len());
        let was_front = index == self.front;
        self.remove(index, cx);
        self.front = front;
        if was_front {
            self.bring(front, window, cx);
        } else {
            self.sync(cx);
            cx.notify();
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let me = cx.entity_id();
        let Some(strip) = strip(cx) else { return };
        if strip.desk.as_ref().is_none_or(|(_, desk)| desk.entity_id() != me) {
            return;
        }
        let tabs: Vec<Shown> = self.tabs.iter().map(|tab| Shown::of(tab, cx)).collect();
        if strip.tabs != tabs || strip.front != self.front {
            let desk = strip.desk.clone();
            cx.set_global(Strip { tabs, front: self.front, desk });
            cx.notify();
        }
        let roots: Vec<PathBuf> =
            self.tabs.iter().filter_map(|tab| tab.read(cx).repo.as_ref().map(|repo| repo.root.clone())).collect();
        let named: Vec<(PathBuf, String)> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let tab = tab.read(cx);
                Some((tab.repo.as_ref()?.root.clone(), tab.name.clone()?))
            })
            .collect();
        let front = self.front().read(cx).repo.as_ref().map(|repo| repo.root.clone());
        let session = (roots, front, named);
        if self.saved.as_ref() != Some(&session) {
            if self.saved.is_some() {
                Memory::update(cx, |memory| {
                    memory.set_session(&session.0, session.1.as_deref());
                    memory.set_tab_names(&session.2);
                });
            }
            self.saved = Some(session);
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(REM));
        div()
            .size_full()
            .bg(rgb(palette().canvas))
            .capture_key_down(|_, window, _| {
                if crate::kit::use_keyboard_focus(true) {
                    window.refresh();
                }
            })
            .on_drop(|dropped: &ExternalPaths, _, cx| crate::links::dropped(dropped.paths(), cx))
            .child(pointer_hides_focus_rings())
            .child(self.front().clone())
            .children(crate::view::commit_input::field_menu(window, cx))
    }
}

fn pointer_hides_focus_rings() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |_, _, window, _| {
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Capture && crate::kit::use_keyboard_focus(false) {
                    window.refresh();
                }
            })
        },
    )
    .absolute()
}

#[derive(Clone, Copy)]
pub(crate) struct Front(pub(crate) WindowHandle<Workspace>);

impl Front {
    pub(crate) fn update<C: AppContext, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut Tab, &mut Window, &mut Context<Tab>) -> R,
    ) -> gpui::Result<R> {
        cx.update_window(self.0.into(), |root, window, cx| {
            let desk = root.downcast::<Workspace>().map_err(|_| std::io::Error::other("the window has no tabs"))?;
            let tab = desk.read(cx).front().clone();
            Ok(tab.update(cx, |tab, cx| update(tab, window, cx)))
        })?
    }

    pub(crate) fn read_with<C: AppContext, R>(&self, cx: &C, read: impl FnOnce(&Tab, &App) -> R) -> gpui::Result<R> {
        cx.read_window(&self.0, |desk, cx| {
            let tab = desk.read(cx).front().clone();
            read(tab.read(cx), cx)
        })
    }

    pub(crate) fn worktree_tab<C: AppContext>(&self, cx: &C, worktree: &crate::agent::Worktree) -> gpui::Result<Option<Entity<Tab>>> {
        cx.read_window(&self.0, |desk, cx| {
            desk.read(cx).tabs.iter().find(|tab| {
                tab.read(cx).repo.as_ref().is_some_and(|repo| worktree.matches_opened(repo))
            }).cloned()
        })
    }

    pub(crate) fn read_tab<C: AppContext, R>(&self, cx: &C, index: usize, read: impl FnOnce(&Tab, &App) -> R) -> gpui::Result<Option<R>> {
        cx.read_window(&self.0, |desk, cx| {
            let tab = desk.read(cx).tabs.get(index).cloned();
            tab.map(|tab| read(tab.read(cx), cx))
        })
    }

    pub(crate) fn tabs<C: AppContext>(&self, cx: &C) -> gpui::Result<(usize, usize)> {
        cx.read_window(&self.0, |desk, cx| {
            let desk = desk.read(cx);
            (desk.tabs.len(), desk.front)
        })
    }
}

impl From<Front> for AnyWindowHandle {
    fn from(front: Front) -> Self {
        front.0.into()
    }
}


pub(crate) fn render<T: Desk>(_: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let strip = strip(cx).filter(|strip| strip.tabs.len() > 1)?;
    let front = strip.front;
    let tabs = strip.tabs.clone();
    let mut row = div()
        .id("tab-strip")
        .h(px(STRIP))
        .flex_shrink_0()
        .flex()
        .items_end()
        .gap(px(2.))
        .px(px(8.))
        .bg(rgb(palette().panel))
        .border_b_1()
        .border_color(rgb(palette().line))
        .overflow_x_scroll();
    for (index, shown) in tabs.into_iter().enumerate() {
        row = row.child(button(index, &shown, index == front, cx));
    }
    let new = kit::icon_btn("tab-new", "plus", "New tab (⌘T)", view::Action::Tabs(Action::New), cx);
    Some(row.child(div().h_full().flex().items_center().pl_1().child(new)))
}

fn button<T: Desk>(index: usize, shown: &Shown, front: bool, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let tip = match (&shown.root, index < NUMBERED) {
        (Some(root), true) => format!("{} (⌘{})", root.display(), index + 1),
        (Some(root), false) => root.display().to_string(),
        (None, true) => format!("{} (⌘{})", shown.title, index + 1),
        (None, false) => shown.title.clone(),
    };
    let mark = if shown.asking {
        Some(div().size(px(7.)).rounded_full().bg(rgb(palette().accent)))
    } else if shown.busy {
        Some(div().size(px(7.)).rounded_full().bg(rgb(palette().text_3)))
    } else {
        None
    };
    let close = kit::icon_btn(
        ("tab-close", index),
        "x",
        "Close tab (⌘W)",
        view::Action::Tabs(Action::Close(Some(index))),
        cx,
    );
    let carried = Carried { from: index, title: shown.title.clone() };
    let control = div()
        .id(("tab", index))
        .h(px(STRIP - 5.))
        .min_w(px(120.))
        .max_w(px(220.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .pl(px(10.))
        .pr(px(3.))
        .rounded_t(px(RADIUS))
        .border_t_2()
        .text_size(px(TEXT_SMALL))
        .cursor_pointer()
        .when(front, |d| d.bg(rgb(palette().canvas)).border_color(rgb(palette().accent)).text_color(rgb(palette().text)))
        .when(!front, |d| d.border_color(kit::clear()).text_color(rgb(palette().text_2)).hover(|s| s.bg(rgb(palette().hover))))
        .focusable()
        .focus(if front { kit::ring } else { kit::ring_clear })
        .tooltip(kit::tip(tip))
        .on_mouse_down(MouseButton::Middle, cx.listener(move |app, _, window, cx| {
            app.dispatch(view::Action::Tabs(Action::Close(Some(index))), window, cx);
        }))
        .on_mouse_down(MouseButton::Right, cx.listener(move |app, event: &MouseDownEvent, window, cx| {
            cx.stop_propagation();
            let menu = view::Menu::Object { target: Target::Tab(index), at: event.position };
            app.dispatch(view::Action::Menu(Some(menu)), window, cx);
        }))
        .on_drag(carried, |carried, grabbed, _, cx| {
            let title = carried.title.clone();
            cx.new(|_| Lifted { title, grabbed })
        })
        .children(mark)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .when(front, |d| d.font_weight(FontWeight::MEDIUM))
                .child(shown.title.clone()),
        )
        .child(close);
    let control = if cx.has_active_drag() {
        control
            .drag_over::<Carried>(move |style, carried, _, _| match carried.from == index {
                true => style,
                false => style.bg(soft(palette().accent, 0x2e)),
            })
            .on_drop(cx.listener(move |app, carried: &Carried, window, cx| {
                app.dispatch(view::Action::Tabs(Action::Move { from: carried.from, to: index }), window, cx)
            }))
    } else {
        control
    };
    kit::pressable(control, view::Action::Tabs(Action::Show(index)), cx)
}

#[derive(Clone)]
struct Carried {
    from: usize,
    title: String,
}

struct Lifted {
    title: String,
    grabbed: gpui::Point<gpui::Pixels>,
}

impl Render for Lifted {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let left = (self.grabbed.x - px(8.)).max(px(0.));
        let top = (self.grabbed.y - px(10.)).max(px(0.));
        div().pl(left).pt(top).child(
            div()
                .px_2()
                .py_1()
                .max_w(px(220.))
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(palette().accent))
                .bg(rgb(palette().raised))
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(palette().text))
                .opacity(0.92)
                .child(self.title.clone()),
        )
    }
}

pub(crate) fn bar_rows(cx: &App) -> Vec<gpui::MenuItem> {
    let Some(strip) = strip(cx) else { return Vec::new() };
    strip
        .tabs
        .iter()
        .enumerate()
        .map(|(index, shown)| gpui::MenuItem::action(shown.title.clone(), ShowTab { index }))
        .collect()
}

pub(crate) fn bar_titles(cx: &App) -> Vec<String> {
    strip(cx).map_or_else(Vec::new, |strip| strip.tabs.iter().map(|shown| shown.title.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_round_both_ways() {
        assert_eq!(stepped(0, 3, 1), 1);
        assert_eq!(stepped(2, 3, 1), 0);
        assert_eq!(stepped(0, 3, -1), 2);
        assert_eq!(stepped(1, 3, -1), 0);
        assert_eq!(stepped(0, 1, 1), 0);
        assert_eq!(stepped(0, 0, 1), 0);
    }

    #[test]
    fn closing_keeps_the_front_tab_or_takes_its_right_hand_neighbour() {
        assert_eq!(after_close(2, 0, 4), 1);
        assert_eq!(after_close(1, 3, 4), 1);
        assert_eq!(after_close(1, 1, 4), 1);
        assert_eq!(after_close(3, 3, 4), 2);
        assert_eq!(after_close(1, 1, 2), 0);
        assert_eq!(after_close(0, 0, 2), 0);
    }

    #[test]
    fn every_tab_key_runs_its_command() {
        for (shortcut, id) in [
            ("cmd-t", "tab.new"),
            ("cmd-w", "tab.close"),
            ("ctrl-tab", "tab.next"),
            ("ctrl-shift-tab", "tab.previous"),
            ("cmd-shift-t", "tab.reopen"),
        ] {
            let command = crate::commands::find(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(command.shortcut, Some(shortcut), "{id}");
            assert_eq!(command.group, Group::Tabs, "{id}");
        }
        assert_eq!(crate::commands::find("tab.new").unwrap().bar, Some(Bar::File));
        assert_eq!(crate::commands::find("tab.close").unwrap().bar, Some(Bar::File));
        assert_eq!(crate::commands::find("tab.next").unwrap().bar, Some(Bar::Window));
        assert!(crate::commands::find("view.close-diff").is_none());
        assert_eq!(crate::commands::find("tab.reopen").unwrap().bar, Some(Bar::File));
        assert_eq!(crate::commands::find("tab.rename").unwrap().bar, Some(Bar::Window));
        assert_eq!(crate::commands::find("repo.manage").and_then(|command| command.shortcut), Some("alt-cmd-o"));
    }

    #[test]
    fn moving_a_tab_keeps_the_same_tab_in_front() {
        assert_eq!(moved(1, 1, 3), 3);
        assert_eq!(moved(2, 0, 3), 1);
        assert_eq!(moved(2, 3, 0), 3);
        assert_eq!(moved(2, 0, 1), 2);
        assert_eq!(moved(0, 2, 3), 0);
        assert_eq!(moved(3, 1, 3), 2);
    }

    #[test]
    fn a_folder_counts_as_inside_a_repository_only_without_a_repository_between() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("Harbour");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("docs/guides")).unwrap();
        std::fs::create_dir_all(root.join("vendor/inner/.git")).unwrap();
        std::fs::create_dir_all(root.join("vendor/inner/src")).unwrap();
        assert!(inside(&root, &root));
        assert!(inside(&root.join("docs/guides"), &root));
        assert!(!inside(&root.join("vendor/inner"), &root));
        assert!(!inside(&root.join("vendor/inner/src"), &root));
        assert!(inside(&root.join("vendor/inner/src"), &root.join("vendor/inner")));
        assert!(!inside(&folder.path().join("Harbour-two"), &root));
    }

    #[test]
    fn the_closed_list_keeps_each_repository_once_and_skips_ones_that_have_gone() {
        let folder = tempfile::tempdir().unwrap();
        let here = folder.path().join("Here");
        std::fs::create_dir(&here).unwrap();
        let gone = folder.path().join("Gone");
        let mut closed = ClosedTabs::default();
        for (root, at) in [(here.clone(), 0), (gone.clone(), 1), (here.clone(), 2)] {
            closed.0.retain(|earlier| earlier.root != root);
            closed.0.push(Closed { root, name: None, at });
        }
        assert_eq!(closed.0.iter().map(|closed| closed.at).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(pop_existing(&mut closed.0).map(|closed| closed.root), Some(here));
        assert_eq!(pop_existing(&mut closed.0), None);
    }
}
