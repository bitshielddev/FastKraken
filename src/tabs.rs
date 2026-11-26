use crate::{
    Tab, autofetch,
    commands::{Availability, Bar, Command, Group, Target},
    kit, perf,
    settings::{self, Memory},
    theme::*,
    view::{self, Centre, Desk, Tone, Ui},
};
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, EntityId, FontWeight, Global, IntoElement, MouseButton, Render,
    Subscription, WeakEntity, Window, WindowHandle, div, prelude::*, px, rgb,
};
use std::path::{Path, PathBuf};

pub(crate) const NUMBERED: usize = 9;
pub(crate) const STRIP: f32 = 30.;

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
            |ui, _| {
                match view::centre(ui) {
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
            |ui, _| {
                Some(match view::centre(ui) {
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
    ]
};

fn covered(ui: &Ui) -> bool {
    matches!(view::centre(ui), Centre::Activity | Centre::Diff | Centre::FileHistory | Centre::Blame | Centre::Browse | Centre::Resolver | Centre::Chooser)
}

pub(crate) fn refusal(ui: &Ui) -> Option<String> {
    if let Some(running) = ui.jobs.refusal() {
        return Some(format!("{running}. Cancel it or let it finish before closing the tab."));
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
}

pub(crate) enum Request {
    New,
    Open { path: PathBuf, then: Then },
    Show(usize),
    Step(isize),
    Close(EntityId),
    Handed(PathBuf),
}

pub(crate) type Then = Box<dyn FnOnce(&mut Tab, &mut Context<Tab>)>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) id: EntityId,
    pub(crate) title: String,
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
        None => "New tab".to_owned(),
    }
}

impl Shown {
    fn of(tab: &Entity<Tab>, cx: &App) -> Self {
        let open = tab.read(cx);
        Self {
            id: tab.entity_id(),
            title: title(&open.ui),
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


pub(crate) struct Workspace {
    tabs: Vec<Entity<Tab>>,
    watching: Vec<Subscription>,
    front: usize,
    saved: Option<(Vec<PathBuf>, Option<PathBuf>)>,
}

impl Workspace {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>, bench: Option<perf::Run>) -> Self {
        cx.set_global(Strip { tabs: Vec::new(), front: 0, desk: Some((window.window_handle(), cx.weak_entity())) });
        let mut desk = Self { tabs: Vec::new(), watching: Vec::new(), front: 0, saved: None };
        cx.observe_window_appearance(window, |_, window, cx| crate::appearance::system_changed(window.appearance(), cx))
            .detach();
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
        let (session, front) = match settings::memory(cx) {
            Some(memory) if settings::settings(cx).reopen_last => (memory.session(), memory.last_repository()),
            _ => (Vec::new(), None),
        };
        let (here, gone): (Vec<PathBuf>, Vec<PathBuf>) = session.into_iter().partition(|root| root.exists());
        let mut front = front.and_then(|front| here.iter().position(|root| *root == front)).unwrap_or(0);
        for (index, root) in here.iter().enumerate() {
            let tab = if index == 0 { self.tabs[0].clone() } else { self.add(None, cx) };
            tab.update(cx, |tab, cx| tab.open(root.clone(), cx));
        }
        let restored = path.as_ref().and_then(|path| {
            let path = std::fs::canonicalize(path).ok()?;
            here.iter().position(|root| *root == path)
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
                let tab = self.add(None, cx);
                self.bring(self.tabs.len() - 1, window, cx);
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
                    tab.repo.is_none() && tab.ui.opening.is_none() && tab.can_open() && !tab.ui.dialog_open() && !tab.ui.modal_open()
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
        }
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
        tab.update(cx, |tab, _| autofetch::stop(tab));
        let front = after_close(self.front, index, self.tabs.len());
        self.tabs.remove(index);
        drop(self.watching.remove(index));
        let was_front = index == self.front;
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
        let front = self.front().read(cx).repo.as_ref().map(|repo| repo.root.clone());
        let session = (roots, front);
        if self.saved.as_ref() != Some(&session) {
            if self.saved.is_some() {
                Memory::update(cx, |memory| memory.set_session(&session.0, session.1.as_deref()));
            }
            self.saved = Some(session);
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(palette().canvas))
            .child(self.front().clone())
            .children(crate::view::commit_input::field_menu(window, cx))
    }
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
        .focus(|s| s.border_color(rgb(palette().accent)))
        .tooltip(kit::tip(tip))
        .on_mouse_down(MouseButton::Middle, cx.listener(move |app, _, window, cx| {
            app.dispatch(view::Action::Tabs(Action::Close(Some(index))), window, cx);
        }))
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
    kit::pressable(control, view::Action::Tabs(Action::Show(index)), cx)
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
        for (shortcut, id) in [("cmd-t", "tab.new"), ("cmd-w", "tab.close"), ("ctrl-tab", "tab.next"), ("ctrl-shift-tab", "tab.previous")] {
            let command = crate::commands::find(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(command.shortcut, Some(shortcut), "{id}");
            assert_eq!(command.group, Group::Tabs, "{id}");
        }
        assert_eq!(crate::commands::find("tab.new").unwrap().bar, Some(Bar::File));
        assert_eq!(crate::commands::find("tab.close").unwrap().bar, Some(Bar::File));
        assert_eq!(crate::commands::find("tab.next").unwrap().bar, Some(Bar::Window));
        assert!(crate::commands::find("view.close-diff").is_none());
    }
}
