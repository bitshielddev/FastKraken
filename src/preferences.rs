use crate::{
    Tab, kit, navigate,
    commands::{Availability, Bar, Command, Group, Target},
    credentials::{self, Entry},
    settings::{self, MAX_FETCH_MINUTES, Settings},
    theme::*,
    view::{self, CommitInput, Desk, Ui},
};
use gpui::{
    App, Context, Div, Entity, Focusable, FontWeight, PathPromptOptions, SharedString, UniformListScrollHandle,
    Window, div, prelude::*, rgb,
};
use crate::theme::px;
use std::{os::unix::ffi::OsStrExt, path::PathBuf};

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "app.preferences",
    Group::Settings,
    |_, _| "Preferences…".into(),
    |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Preferences(Action::Open(None))),
)
.words(&["settings", "options", "configure", "accounts", "general", "appearance"])
.keys("cmd-,")
.bar(Bar::App)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    General,
    Appearance,
    Accounts,
    Git,
    Signing,
    Ssh,
    Commit,
    Diff,
    Tools,
    Shortcuts,
    Repository,
}

impl Section {
    fn title(self, ui: &Ui) -> String {
        match self {
            Section::General => "General".into(),
            Section::Appearance => "Appearance".into(),
            Section::Accounts => "Accounts".into(),
            Section::Git => "Git".into(),
            Section::Signing => "Signing".into(),
            Section::Ssh => "SSH".into(),
            Section::Commit => "Commit".into(),
            Section::Diff => "Diff".into(),
            Section::Tools => "External tools".into(),
            Section::Shortcuts => "Shortcuts".into(),
            Section::Repository => ui.repo.clone(),
        }
    }
}

pub(crate) fn sections(repository: bool) -> Vec<Section> {
    let mut listed = vec![
        Section::General,
        Section::Appearance,
        Section::Accounts,
        Section::Git,
        Section::Signing,
        Section::Ssh,
        Section::Commit,
        Section::Diff,
        Section::Tools,
        Section::Shortcuts,
    ];
    if repository {
        listed.push(Section::Repository);
    }
    listed
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open(Option<Section>),
    Close,
    Show(Section),
    ToggleReopen,
    ToggleLoginPath,
    FetchEvery(u32),
    ChooseCloneFolder,
    CloneFolder(Option<PathBuf>),
}

pub(crate) struct State {
    open: bool,
    section: Section,
    pub(crate) branch: Entity<CommitInput>,
    pub(crate) filter: Entity<CommitInput>,
    scroll: UniformListScrollHandle,
    page_scroll: gpui::ScrollHandle,
    saved: Option<Result<Vec<Entry>, String>>,
    saved_ticket: u64,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let branch = cx.new(|cx| CommitInput::with_placeholder(cx, "main"));
        cx.observe(&branch, |tab, _, cx| branch_typed(tab, cx)).detach();
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter shortcuts"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self {
            open: false,
            section: Section::General,
            branch,
            filter,
            scroll: UniformListScrollHandle::new(),
            page_scroll: gpui::ScrollHandle::new(),
            saved: None,
            saved_ticket: 0,
        }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn section(&self) -> Section {
        self.section
    }

    pub(crate) fn saved(&self) -> Option<&Result<Vec<Entry>, String>> {
        self.saved.as_ref()
    }

    pub(crate) fn shows_branch(&self) -> bool {
        self.open && self.section == Section::General
    }

    pub(crate) fn shows_identity(&self) -> bool {
        self.open && self.section == Section::Git
    }

    pub(crate) fn shows_diff(&self) -> bool {
        self.open && self.section == Section::Diff
    }

    pub(crate) fn shows_tools(&self) -> bool {
        self.open && self.section == Section::Tools
    }

    pub(crate) fn shows_filter(&self) -> bool {
        self.open && self.section == Section::Shortcuts
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(section) => {
            let state = &mut tab.ui.preferences;
            state.open = true;
            let section = section.unwrap_or(state.section);
            let shown = settings::settings(cx).default_branch();
            state.branch.update(cx, |field, cx| field.set_text(&shown, cx));
            state.filter.update(cx, |field, cx| field.clear(cx));
            crate::diff_text::opened(tab, cx);
            tab.ui.menu = None;
            show(tab, section, cx);
            view::focus(&tab.ui, window);
        }
        Action::Close => {
            tab.ui.preferences.open = false;
            crate::publish::hide_account(tab);
            view::focus(&tab.ui, window);
        }
        Action::Show(section) => {
            show(tab, section, cx);
            view::focus(&tab.ui, window);
        }
        Action::ToggleReopen => Settings::update(cx, |s| s.reopen_last = !s.reopen_last),
        Action::ToggleLoginPath => {
            let on = !settings::settings(cx).login_path;
            Settings::update(cx, |s| s.login_path = on);
            crate::environment::use_login_shell(on, cx);
        }
        Action::FetchEvery(minutes) => Settings::update(cx, |s| s.fetch_every = Some(minutes.min(MAX_FETCH_MINUTES))),
        Action::ChooseCloneFolder => {
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("Use this folder".into()),
            });
            window
                .spawn(cx, {
                    let tab = cx.weak_entity();
                    async move |cx| {
                        if let Ok(Ok(Some(paths))) = picker.await
                            && let Some(path) = paths.into_iter().next()
                        {
                            let _ = cx.update(|window, cx| {
                                tab.update(cx, |tab, cx| {
                                    tab.dispatch(view::Action::Preferences(Action::CloneFolder(Some(path))), window, cx)
                                })
                            });
                        }
                    }
                })
                .detach();
        }
        Action::CloneFolder(Some(folder)) if !folder.is_dir() => {
            tab.refuse(format!("{} is not a folder", folder.display()), cx);
        }
        Action::CloneFolder(folder) => {
            let bytes = folder.map(|folder| folder.as_os_str().as_bytes().to_vec());
            Settings::update(cx, |s| s.clone_folder = bytes);
        }
    }
    cx.notify();
}

fn show(tab: &mut Tab, section: Section, cx: &mut Context<Tab>) {
    let section = if section == Section::Repository && tab.repo.is_none() { Section::General } else { section };
    let state = &mut tab.ui.preferences;
    let was = state.section;
    state.section = section;
    if was == Section::Accounts && section != Section::Accounts {
        crate::publish::hide_account(tab);
    }
    match section {
        Section::Accounts => {
            read_saved(tab, cx);
            crate::publish::show_account(tab, cx);
        }
        Section::Tools => {
            crate::tools::start(false, cx);
            crate::difftool::read(tab, cx);
        }
        Section::Diff => crate::diff_text::look(tab, cx),
        Section::Repository => {
            crate::lfs::read_patterns(tab, cx);
            crate::sparse::read_tree(tab, cx);
        }
        Section::Signing => crate::keys::read(tab, true, cx),
        Section::Ssh => crate::keys::read(tab, false, cx),
        Section::Git => {
            crate::identity::read_global(tab, cx);
            crate::profiles::read_locals(tab, cx);
            crate::engine::read(tab, cx);
        }
        _ => {}
    }
}

fn read_saved(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.preferences;
    state.saved_ticket += 1;
    let ticket = state.saved_ticket;
    Tab::work(
        cx,
        || credentials::Saved::current().and_then(|saved| saved.list()),
        move |tab, listed, cx| {
            if tab.ui.preferences.saved_ticket != ticket {
                return;
            }
            tab.ui.preferences.saved = Some(listed.map(|mut entries| {
                entries.reverse();
                entries
            }));
            cx.notify();
        },
    );
}

pub(crate) fn saved_changed(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.preferences.open && tab.ui.preferences.section == Section::Accounts {
        read_saved(tab, cx);
    }
}

fn branch_typed(tab: &mut Tab, cx: &mut Context<Tab>) {
    if !tab.ui.preferences.shows_branch() {
        return;
    }
    let typed = tab.ui.preferences.branch.read(cx).text().trim().to_owned();
    if branch_problem(&typed).is_none() && settings::settings(cx).default_branch != typed {
        Settings::update(cx, |s| s.default_branch = typed);
    }
    cx.notify();
}

pub(crate) fn branch_problem(name: &str) -> Option<String> {
    crate::refs::name_problem(crate::refs::Kind::Branch, name, std::iter::empty())
}

pub(crate) fn key(tab: &mut Tab, action: navigate::Action, window: &mut Window, cx: &mut Context<Tab>) {
    let listed = sections(tab.repo.is_some());
    if let Some(next) = moved(&listed, tab.ui.preferences.section, action) {
        dispatch(tab, Action::Show(next), window, cx);
    }
}

pub(crate) fn moved(listed: &[Section], from: Section, action: navigate::Action) -> Option<Section> {
    use navigate::Action as N;
    let at = listed.iter().position(|section| *section == from).unwrap_or(0);
    let last = listed.len().checked_sub(1)?;
    let to = match action {
        N::Step(delta) | N::Page(delta) => at.saturating_add_signed(delta).min(last),
        N::Home | N::First => 0,
        N::End | N::Last => last,
        _ => return None,
    };
    (to != at).then(|| listed[to])
}

pub(crate) fn recents_help(count: usize) -> String {
    match count {
        0 => "None yet. The start screen lists the repositories you open.".into(),
        1 => "The start screen and File > Open Recent list one. Clear forgets it; the notice offers Undo.".into(),
        n => format!("The start screen and File > Open Recent list {n}. Clear forgets them; the notice offers Undo."),
    }
}

pub(crate) fn interval(minutes: u32) -> String {
    match minutes {
        0 => "Off".into(),
        1 => "Every minute".into(),
        n => format!("Every {n} minutes"),
    }
}


pub(crate) fn page<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.preferences;
    let mut nav = div()
        .w(px(210.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .bg(rgb(palette().panel))
        .child(div().px_2().pb_1().child(kit::label("PREFERENCES")));
    let row = |section: Section, cx: &Context<T>| {
        let chosen = state.section == section;
        let item = kit::row(("preferences-section", section as usize), chosen)
            .h(px(30.))
            .px_2()
            .rounded(px(RADIUS_CHIP))
            .text_size(px(12.5))
            .text_color(rgb(if chosen { palette().text } else { palette().text_2 }))
            .child(kit::fit(section.title(ui)).size(12.5));
        kit::pressable(item.focusable().focus(if chosen { kit::ring } else { kit::ring_clear }), view::Action::Preferences(Action::Show(section)), cx)
    };
    for section in sections(false) {
        nav = nav.child(row(section, cx));
    }
    if !ui.repo.is_empty() {
        nav = nav
            .child(div().px_2().pt_3().pb_1().child(kit::label("THIS REPOSITORY")))
            .child(row(Section::Repository, cx));
    }
    let nav = nav.child(kit::grow()).child(
        kit::btn("preferences-exit", "Exit Preferences")
            .wide()
            .hint("esc")
            .on(view::Action::Preferences(Action::Close), cx),
    );
    let content = match state.section {
        Section::Shortcuts => shortcuts(ui, cx),
        section => {
            let body = match section {
                Section::General => general(ui, cx),
                Section::Appearance => crate::appearance::preferences(cx),
                Section::Accounts => accounts(app, cx),
                Section::Git => git(ui, cx),
                Section::Signing => crate::keys::signing_page(ui, cx),
                Section::Ssh => crate::keys::ssh_page(ui, cx),
                Section::Commit => crate::commit_options::preferences(cx),
                Section::Diff => crate::diff_text::preferences(ui, cx),
                Section::Tools => crate::tools::preferences(ui, cx),
                _ => repository(ui, cx),
            };
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("preferences-page")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&ui.preferences.page_scroll)
                        .px(px(28.))
                        .py(px(22.))
                        .child(div().max_w(px(620.)).flex().flex_col().gap_1().child(heading(section.title(ui))).child(body)),
                )
                .child(kit::scrollbar("preferences-page-scrollbar", &ui.preferences.page_scroll))
        }
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(nav)
        .child(div().w(px(1.)).h_full().flex_shrink_0().bg(rgb(palette().line)))
        .child(content)
}

fn heading(title: impl Into<SharedString>) -> Div {
    div()
        .pb_2()
        .text_size(px(TEXT_TITLE))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(palette().text))
        .child(title.into())
}

pub(crate) fn group(title: &'static str) -> Div {
    div().pt_4().pb_1().child(kit::label(title))
}

pub(crate) fn setting(title: impl Into<SharedString>, help: Option<SharedString>, control: impl IntoElement) -> Div {
    div()
        .py(px(10.))
        .flex()
        .items_center()
        .gap_4()
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(px(13.)).text_color(rgb(palette().text)).child(title.into()))
                .when_some(help, |d, help| d.child(kit::muted(help))),
        )
        .child(div().flex_shrink_0().child(control))
}

pub(crate) fn tick<T: Desk>(
    id: &'static str,
    on: bool,
    title: &'static str,
    help: &'static str,
    action: view::Action,
    cx: &Context<T>,
) -> gpui::Stateful<Div> {
    let row = div()
        .id(id)
        .py(px(10.))
        .flex()
        .items_start()
        .gap_3()
        .border_b_1()
        .border_color(rgb(palette().line))
        .cursor_pointer()
        .focusable()
        .focus(kit::ring_clear)
        .child(div().pt(px(2.)).child(kit::checkbox(on)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(px(13.)).text_color(rgb(palette().text)).child(title))
                .child(kit::muted(help)),
        );
    kit::pressable(row, action, cx)
}

fn general<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let chosen = settings::settings(cx);
    let typed = ui.preferences.branch.read(cx).text().trim().to_owned();
    let branch_help = branch_problem(&typed).unwrap_or_else(|| "New Repository… starts on this branch.".to_owned());
    let minutes = chosen.fetch_minutes();
    let fetch = |n: u32| view::Action::Preferences(Action::FetchEvery(n));
    let recents = settings::memory(cx).map_or(0, |memory| memory.recents.len());
    let folder = chosen.clone_folder();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (folder_text, folder_help): (String, SharedString) = match &folder {
        Some(folder) => (crate::start::homely(folder, home.as_deref()), "Clone Repository… and New Repository… start here.".into()),
        None => ("Where the last clone went".to_owned(), "Or your home folder, before the first clone.".into()),
    };
    div()
        .flex()
        .flex_col()
        .child(tick(
            "preferences-reopen",
            chosen.reopen_last,
            "Reopen the last repositories when FastKraken starts",
            "Each opens in its own tab again. A repository or tab you closed is not reopened.",
            view::Action::Preferences(Action::ToggleReopen),
            cx,
        ))
        .child(setting(
            "Default branch for new repositories",
            Some(branch_help.into()),
            div().w(px(200.)).child(ui.preferences.branch.clone()),
        ))
        .child(setting(
            "Clones and new repositories go in",
            Some(folder_help),
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().max_w(px(220.)).child(kit::fit(folder_text).size(12.5).color(palette().text_2)))
                .when(folder.is_some(), |d| {
                    d.child(kit::link(
                        "preferences-clone-reset",
                        "Reset",
                        view::Action::Preferences(Action::CloneFolder(None)),
                        cx,
                    ))
                })
                .child(
                    kit::btn("preferences-clone-folder", "Choose…")
                        .small()
                        .on(view::Action::Preferences(Action::ChooseCloneFolder), cx),
                ),
        ))
        .child(setting(
            "Fetch automatically",
            Some("Fetches every remote of the open repository in the background. Off fetches only when you ask.".into()),
            kit::stepper(
                "preferences-fetch",
                interval(minutes),
                (minutes > 0).then(|| fetch(minutes - 1)),
                (minutes < MAX_FETCH_MINUTES).then(|| fetch(minutes + 1)),
                cx,
            ),
        ))
        .child(setting(
            "Recent repositories",
            Some(recents_help(recents).into()),
            kit::btn("preferences-clear-recents", "Clear")
                .small()
                .enabled(recents > 0)
                .on(view::Action::Start(crate::start::Action::Clear), cx),
        ))
        .child(tick(
            "preferences-prune",
            ui.sync.prune,
            "Remove deleted remote branches when fetching",
            "A branch deleted on the remote loses its remote-tracking branch here. Local branches stay.",
            view::Action::Sync(crate::sync::Action::TogglePrune),
            cx,
        ))
        .child(tick(
            "preferences-autostash",
            ui.autostash.always,
            "Stash changes that are in the way without asking",
            "A switch, pull or other operation stashes them first and puts them back afterwards.",
            view::Action::Autostash(crate::autostash::Action::Toggle),
            cx,
        ))
        .child(tick(
            "preferences-check-updates",
            chosen.check_updates,
            crate::updates::AUTOMATIC_TITLE,
            crate::updates::AUTOMATIC_HELP,
            view::Action::Updates(crate::updates::Action::Automatically(!chosen.check_updates)),
            cx,
        ))
}

fn accounts<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let saved = match ui.preferences.saved() {
        None => div().py_2().child(kit::muted("Reading saved passwords…")),
        Some(Err(error)) => div().py_2().text_size(px(TEXT_SMALL)).text_color(rgb(palette().deleted)).child(error.clone()),
        Some(Ok(entries)) if entries.is_empty() => {
            div().py_2().child(kit::muted("FastKraken has not saved a password for any host."))
        }
        Some(Ok(entries)) => {
            let mut list = div().flex().flex_col();
            for (index, entry) in entries.iter().enumerate() {
                list = list.child(setting(
                    credentials::named(entry),
                    None,
                    kit::btn(("preferences-forget", index), "Forget")
                        .small()
                        .on(view::Action::Credentials(credentials::Action::ForgetOne(entry.clone())), cx),
                ));
            }
            list.child(
                div().pt_3().flex().child(
                    kit::btn("preferences-forget-all", "Forget all")
                        .small()
                        .on(view::Action::Credentials(credentials::Action::Forget), cx),
                ),
            )
        }
    };
    div()
        .flex()
        .flex_col()
        .child(group("GITHUB"))
        .child(div().py_2().child(if ui.publish.account_shown() {
            crate::publish::accounts(app, cx)
        } else {
            div().child(kit::link(
                "preferences-github",
                "Show the GitHub connection",
                view::Action::Preferences(Action::Show(Section::Accounts)),
                cx,
            ))
        }))
        .child(group("SAVED PASSWORDS"))
        .child(kit::muted(
            "Passwords FastKraken keeps in your Keychain for HTTPS remotes. Forget one and Git asks for it again the next time it connects.",
        ))
        .child(saved)
}

fn git<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    div()
        .flex()
        .flex_col()
        .child(crate::identity::preferences(ui, cx))
        .child(crate::profiles::preferences(ui, cx))
        .child(group("GIT ON THIS MAC"))
        .child(tick(
            "preferences-login-path",
            settings::settings(cx).login_path,
            "Use my login shell’s search path",
            "Hooks, GPG, Git LFS and SSH commands are found where your shell finds them. Off, Git searches only the app’s own path and Homebrew’s folders.",
            view::Action::Preferences(Action::ToggleLoginPath),
            cx,
        ))
        .child(crate::engine::preferences(ui, cx))
}

fn repository<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    use crate::sync::{Mode as Pull, Strategy};
    let chosen = ui.sync.chosen();
    let pulls = |mode: Pull| match mode {
        Pull::Fetch => chosen.fetch,
        Pull::Pull(strategy) => !chosen.fetch && chosen.strategy == strategy,
    };
    let mut pull = kit::segmented();
    for (index, (label, mode)) in [
        ("Fetch", Pull::Fetch),
        ("Pull", Pull::Pull(Strategy::FastForwardIfPossible)),
        ("Fast-forward only", Pull::Pull(Strategy::FastForwardOnly)),
        ("Rebase", Pull::Pull(Strategy::Rebase)),
    ]
    .into_iter()
    .enumerate()
    {
        pull = pull.child(kit::segment(
            ("preferences-pull", index),
            label,
            None,
            pulls(mode),
            view::Action::Sync(crate::sync::Action::SetDefault(mode)),
            cx,
        ));
    }
    let mut merges = div().flex().flex_col().gap_1();
    for (index, mode) in crate::merge::Mode::ALL.into_iter().enumerate() {
        let row = div()
            .id(("preferences-merge", index))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_size(px(12.5))
            .text_color(rgb(palette().text_2))
            .focusable()
            .min_h(px(TARGET))
            .focus(kit::ring_clear)
            .child(radio(ui.merge.mode == mode))
            .child(mode.title());
        merges = merges.child(kit::pressable(row, view::Action::Merge(crate::merge::Action::SetDefault(mode)), cx));
    }
    div()
        .flex()
        .flex_col()
        .child(kit::muted("Chosen for this repository alone, as the Pull and Merge menus’ Set as default marks are."))
        .child(setting("What the Pull button does", Some("The pull menu still offers the others.".into()), pull))
        .child(setting("How merges run", Some("Merge menus still offer the others.".into()), merges))
        .child(tick(
            "preferences-update-submodules",
            ui.submodule.automatic,
            "Update submodules automatically",
            "After a pull or checkout, submodules that are not at the commits the branch expects are updated without asking. This can download their commits.",
            view::Action::Submodule(crate::submodule::Action::ToggleAutomatic),
            cx,
        ))
        .child(tick(
            "preferences-offer-initialise",
            ui.submodule.offer,
            "Offer to initialise submodules",
            "When this repository opens with submodules that are not initialised, a strip under the toolbar offers to initialise them.",
            view::Action::Submodule(crate::submodule::Action::ToggleOffer),
            cx,
        ))
        .child(crate::sparse::preferences(ui, cx))
        .child(crate::lfs::preferences(ui, cx))
}

pub(crate) fn radio(on: bool) -> Div {
    div()
        .size(px(14.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(if on { palette().accent } else { palette().line_strong }))
        .bg(rgb(palette().canvas))
        .when(on, |d| d.child(div().size(px(6.)).rounded_full().bg(rgb(palette().accent))))
}

fn shortcuts<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let list = crate::help::shortcut_list(
        "preferences-shortcuts",
        ui,
        |ui| &ui.preferences.filter,
        ui.preferences.scroll.clone(),
        None,
        cx,
    );
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .px(px(28.))
        .py(px(22.))
        .flex()
        .flex_col()
        .gap_2()
        .child(heading("Shortcuts"))
        .child(kit::muted("Every keyboard shortcut. They cannot be changed yet."))
        .child(div().max_w(px(620.)).child(ui.preferences.filter.clone()))
        .child(div().max_w(px(620.)).flex_1().min_h_0().flex().flex_col().child(list))
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    (!ui.preferences.shows_branch() && ui.preferences.branch.focus_handle(cx).is_focused(window))
        || (!ui.preferences.shows_filter() && ui.preferences.filter.focus_handle(cx).is_focused(window))
        || (!ui.preferences.shows_identity()
            && (ui.identity.global_name.focus_handle(cx).is_focused(window)
                || ui.identity.global_email.focus_handle(cx).is_focused(window)))
        || crate::tools::field_focused(ui, window, cx)
        || crate::profiles::field_focused(ui, window, cx)
        || crate::engine::field_focused(ui, window, cx)
        || crate::diff_text::field_focused(ui, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use navigate::Action as N;

    #[test]
    fn the_repositorys_section_is_listed_last_and_only_with_one_open() {
        assert_eq!(
            sections(false),
            [
                Section::General,
                Section::Appearance,
                Section::Accounts,
                Section::Git,
                Section::Signing,
                Section::Ssh,
                Section::Commit,
                Section::Diff,
                Section::Tools,
                Section::Shortcuts
            ]
        );
        assert_eq!(sections(true).last(), Some(&Section::Repository));
        assert_eq!(sections(true).len(), 11);
    }

    #[test]
    fn movement_keys_step_through_the_sections_and_stop_at_the_ends() {
        let listed = sections(true);
        assert_eq!(moved(&listed, Section::General, N::Step(1)), Some(Section::Appearance));
        assert_eq!(moved(&listed, Section::Appearance, N::Step(1)), Some(Section::Accounts));
        assert_eq!(moved(&listed, Section::Accounts, N::Step(-1)), Some(Section::Appearance));
        assert_eq!(moved(&listed, Section::General, N::Step(-1)), None);
        assert_eq!(moved(&listed, Section::Repository, N::Step(1)), None);
        assert_eq!(moved(&listed, Section::Git, N::Step(1)), Some(Section::Signing));
        assert_eq!(moved(&listed, Section::Ssh, N::Step(1)), Some(Section::Commit));
        assert_eq!(moved(&listed, Section::Commit, N::Step(1)), Some(Section::Diff));
        assert_eq!(moved(&listed, Section::Diff, N::Step(1)), Some(Section::Tools));
        assert_eq!(moved(&listed, Section::Git, N::Home), Some(Section::General));
        assert_eq!(moved(&listed, Section::Git, N::End), Some(Section::Repository));
        assert_eq!(moved(&listed, Section::Git, N::Page(9)), Some(Section::Repository));
        assert_eq!(moved(&listed, Section::Git, N::Left), None);
        assert_eq!(moved(&sections(false), Section::Git, N::Last), Some(Section::Shortcuts));
        assert_eq!(moved(&[], Section::General, N::Step(1)), None);
    }

    #[test]
    fn the_recent_repositories_are_counted_in_words() {
        assert!(recents_help(0).starts_with("None yet."));
        assert!(recents_help(1).contains("list one."));
        assert!(recents_help(12).contains("list 12."));
    }

    #[test]
    fn the_fetch_interval_reads_as_words() {
        assert_eq!(interval(0), "Off");
        assert_eq!(interval(1), "Every minute");
        assert_eq!(interval(5), "Every 5 minutes");
    }

    #[test]
    fn the_default_branch_is_checked_as_the_new_repository_dialog_checks_it() {
        assert_eq!(branch_problem("trunk"), None);
        assert_eq!(branch_problem("release/next"), None);
        assert!(branch_problem("").is_some());
        assert!(branch_problem("has space").is_some());
        assert!(branch_problem("two..dots").is_some());
        assert!(branch_problem("HEAD").is_some());
    }

    #[test]
    fn preferences_is_in_the_app_menu_after_about_and_opens_with_cmd_comma() {
        let command = commands::find("app.preferences").expect("the Preferences command");
        assert_eq!(command.shortcut, Some("cmd-,"));
        assert_eq!(command.bar, Some(Bar::App));
        let app: Vec<_> = commands::in_bar(Bar::App).into_iter().map(|c| c.id).collect();
        assert_eq!(app, ["app.about", "app.updates", "app.preferences", "app.install-command"]);
        assert_eq!(commands::bar_index(Bar::App, "app.preferences"), Some(3));
        let forget = commands::find("app.forget-credentials").expect("the forget command");
        assert_eq!(forget.bar, None);
    }
}
