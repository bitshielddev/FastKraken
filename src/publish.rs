mod hosted;
mod panel;
mod recovery;

use crate::{
    Tab, jobs,
    github::{Account, Client, CreatedRepo, DeviceCode},
    remotes::{self, PushOutcome, Remote},
    repository::Repository,
    view::{self, CommitInput, Desk},
};
use gpui::{App, Context, Entity, Focusable, Window, prelude::*};
pub(crate) use panel::{accounts, branch_form, dialog};
use recovery::Receipt;

pub(crate) const REMOTES_MENU: &[&str] = &["repo.publish", "repo.add-remote", "repo.manage-remotes"];

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Target, busy, networking, operating, on_repo};
    use view::Ui;
    fn ready(ui: &Ui, target: &Target) -> Availability {
        if on_repo(ui, target) {
            Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| operating(ui)))
        } else {
            Availability::Hidden
        }
    }
    fn remote(target: &Target) -> Option<&String> {
        match target {
            Target::Remote(name) => Some(name),
            _ => None,
        }
    }
    fn editable(ui: &Ui, target: &Target) -> Availability {
        if remote(target).is_none() {
            return Availability::Hidden;
        }
        Availability::unless(busy(ui).or_else(|| {
            if let Some(reason) = ui.operation.refusal() {
                Some(reason)
            } else if ui.prompt.open() {
                Some("Close the open dialog first".to_owned())
            } else {
                None
            }
        }))
    }
    &[
        Command::new(
            "repo.publish-branch",
            Group::Sync,
            |_, _| "Publish branch…".into(),
            |ui, target, _| {
                if !on_repo(ui, target) || ui.upstream.is_some() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| networking(ui)).or_else(|| {
                    if let Some(reason) = ui.operation.refusal() {
                        Some(reason)
                    } else if ui.refs.remotes.is_empty() {
                        Some("This repository has no remote to publish to".to_owned())
                    } else {
                        None
                    }
                }))
            },
            |_, _| Some(view::Action::Publish(Action::PushBranch)),
        )
        .icon("arrow-up"),
        Command::new(
            "repo.publish",
            Group::Remotes,
            |_, _| "Publish to GitHub…".into(),
            |ui, target, _| ready(ui, target),
            |_, _| Some(view::Action::Publish(Action::Publish)),
        )
        .words(&["create repository", "github"])
        .icon("cloud"),
        Command::new(
            "repo.add-remote",
            Group::Remotes,
            |_, _| "Add existing remote…".into(),
            |ui, target, _| ready(ui, target),
            |_, _| Some(view::Action::Publish(Action::Add)),
        )
        .icon("plus"),
        Command::new(
            "repo.manage-remotes",
            Group::Manage,
            |_, _| "Manage remotes…".into(),
            |ui, target, _| ready(ui, target),
            |_, _| Some(view::Action::Publish(Action::Menu)),
        )
        .words(&["origin", "urls"])
        .icon("folder"),
        Command::new(
            "remote.rename",
            Group::Remotes,
            |_, _| "Rename remote…".into(),
            |ui, target, _| editable(ui, target),
            |_, target| {
                remote(target).map(|name| {
                    view::Action::Publish(Action::EditRemote(RemoteEdit::Rename(name.clone())))
                })
            },
        ),
        Command::new(
            "remote.remove",
            Group::Remotes,
            |_, _| "Remove remote…".into(),
            |ui, target, _| editable(ui, target),
            |_, target| {
                remote(target).map(|name| {
                    view::Action::Publish(Action::EditRemote(RemoteEdit::Remove(name.clone())))
                })
            },
        )
        .danger(),
    ]
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Closed,
    Menu,
    Add,
    Branch,
    GitHub,
    Account,
}

#[derive(Clone, Debug)]
pub(crate) enum RemoteEdit {
    Rename(String),
    Remove(String),
}

impl RemoteEdit {
    fn name(&self) -> &str {
        match self {
            Self::Rename(name) | Self::Remove(name) => name,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Menu,
    Add,
    Publish,
    PushBranch,
    PrepareBranch,
    Escape,
    AddRemote,
    EditRemote(RemoteEdit),
    EditAddress(String),
    SaveRemoteEdit,
    SelectRemote(usize),
    SubmitPush,
    Connect,
    EditConfig,
    RegisterApp,
    SaveConfig,
    Disconnect,
    CopyCode,
    OpenVerification,
    RefreshOwners,
    Install,
    SelectOwner(usize),
    SetPrivate(bool),
    CreateAndConnect,
    CreateAndPush,
    Recover,
    ConfirmDestination,
    ConnectOnly,
    ForgetPending,
    ConfirmForget,
    TogglePush,
    ToggleAccountMenu,
    ToggleOwnerMenu,
    CloseMenus,
    UseBuiltInApp,
}

pub(crate) struct State {
    page: Page,
    pub(crate) name: Entity<CommitInput>,
    pub(crate) url: Entity<CommitInput>,
    pub(crate) push_url: Entity<CommitInput>,
    pub(crate) repository_name: Entity<CommitInput>,
    pub(crate) destination_branch: Entity<CommitInput>,
    pub(crate) client_id: Entity<CommitInput>,
    pub(crate) slug: Entity<CommitInput>,
    remotes: Vec<Remote>,
    remote_edit: Option<RemoteEdit>,
    selected_remote: Option<usize>,
    remotes_loaded: bool,
    remote_scroll: gpui::ScrollHandle,
    client: Option<Client>,
    account: Option<Account>,
    device: Option<DeviceCode>,
    owner: Option<usize>,
    private: bool,
    pending: Option<Receipt>,
    confirmation: Option<CreatedRepo>,
    confirm_forget: bool,
    repair: Option<FirstPush>,
    pub(crate) message: String,
    failed: bool,
    pub(crate) working: bool,
    push_after: bool,
    account_menu: bool,
    owner_menu: bool,
    ticket: u64,
    connect_now: bool,
    pub(crate) after: Option<After>,
}

#[derive(Clone, Debug)]
pub(crate) enum After {
    Bind(crate::connect::Bind),
    Retry(Option<Box<view::Action>>),
}

impl State {
    pub(crate) fn new<T: 'static>(cx: &mut Context<T>) -> Self {
        fn input<T: 'static>(cx: &mut Context<T>, prompt: &'static str) -> Entity<CommitInput> {
            let value = cx.new(|cx| CommitInput::with_placeholder(cx, prompt));
            cx.observe(&value, |_, _, cx| cx.notify()).detach();
            value
        }
        Self {
            page: Page::Closed,
            name: input(cx, "Remote name"),
            url: input(cx, "Repository URL"),
            push_url: input(cx, "The repository URL"),
            repository_name: input(cx, "Repository name"),
            destination_branch: input(cx, "Remote branch"),
            client_id: input(cx, "GitHub App client ID"),
            slug: input(cx, "GitHub App slug"),
            remotes: Vec::new(),
            remote_edit: None,
            selected_remote: None,
            remotes_loaded: false,
            remote_scroll: gpui::ScrollHandle::new(),
            client: None,
            account: None,
            device: None,
            owner: None,
            private: true,
            pending: None,
            confirmation: None,
            confirm_forget: false,
            repair: None,
            message: String::new(),
            failed: false,
            working: false,
            push_after: true,
            account_menu: false,
            owner_menu: false,
            ticket: 0,
            connect_now: false,
            after: None,
        }
    }

    pub(crate) fn open(&self) -> bool {
        !matches!(self.page, Page::Closed | Page::Account)
    }

    pub(crate) fn account_shown(&self) -> bool {
        self.page == Page::Account
    }

    pub(crate) fn branch_open(&self) -> bool {
        self.page == Page::Branch
    }

    fn stop_reads(&mut self) {
        self.ticket += 1;
        self.working = false;
        self.device = None;
        self.confirmation = None;
        self.confirm_forget = false;
    }
    #[cfg(feature = "publish-replay")]
    pub(crate) fn awaiting_device(&self) -> bool {
        self.device.is_some()
    }
    #[cfg(feature = "publish-replay")]
    pub(crate) fn awaiting_confirmation(&self) -> bool {
        self.confirmation.is_some()
    }
    #[cfg(feature = "publish-replay")]
    pub(crate) fn menus(&self) -> (bool, bool) {
        (self.account_menu, self.owner_menu)
    }
    pub(crate) fn owner(&self) -> Option<&crate::github::Owner> {
        self.owner
            .and_then(|i| self.account.as_ref()?.owners.get(i))
    }
    fn remote(&self) -> Option<&Remote> {
        self.selected_remote.and_then(|i| self.remotes.get(i))
    }
}

#[derive(Clone, Debug)]
struct FirstPush {
    remote: String,
    url: String,
    branch: String,
    head: String,
    destination: String,
}

fn branch(tab: &Tab) -> Option<(String, String)> {
    tab.ui
        .head
        .as_ref()
        .filter(|_| !tab.ui.detached)
        .map(|oid| (tab.ui.branch.clone(), oid.clone()))
}

fn set<T: 'static>(field: &Entity<CommitInput>, value: &str, cx: &mut Context<T>) {
    field.update(cx, |input, cx| input.set_text(value, cx));
}

#[derive(Debug)]
pub(crate) struct Ask {
    login: Option<String>,
    account: Option<u64>,
}

pub(crate) fn describe(ask: &Ask) -> crate::prompt::Spec {
    use crate::prompt::Spec;
    match &ask.login {
        Some(login) => Spec::new(format!("Disconnect GitHub account {login}?"), "Disconnect")
            .line("FastKraken forgets this account's sign-in on this Mac. You can connect again at any time."),
        None => Spec::new("Forget the saved GitHub sign-in?", "Forget sign-in")
            .line("FastKraken forgets the GitHub sign-in it saved on this Mac. You can connect again at any time."),
    }
    .line("Existing remotes stay as they are, and nothing changes on GitHub.")
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, _: crate::prompt::Answer, cx: &mut Context<Tab>) {
    let page = tab.ui.publish.page;
    if !tab.ui.busy() && (page == Page::Account || (tab.repo.is_some() && page == Page::GitHub)) {
        hosted::disconnect(tab, ask.account, cx);
    }
}

fn text(field: &Entity<CommitInput>, cx: &App) -> String {
    field.read(cx).text().trim().to_owned()
}

fn suggest_name(remotes: &[Remote]) -> String {
    for candidate in ["origin", "github"]
        .into_iter()
        .map(str::to_owned)
        .chain((2..).map(|i| format!("github-{i}")))
    {
        if !remotes.iter().any(|r| r.name == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn message(tab: &mut Tab, msg: impl Into<String>, cx: &mut Context<Tab>) {
    tab.ui.publish.message = msg.into();
    tab.ui.publish.failed = true;
    cx.notify();
}

fn info(tab: &mut Tab, msg: impl Into<String>, cx: &mut Context<Tab>) {
    tab.ui.publish.message = msg.into();
    tab.ui.publish.failed = false;
    cx.notify();
}

fn job<T: Send + 'static>(
    tab: &mut Tab,
    cx: &mut Context<Tab>,
    mutation: bool,
    progress: &str,
    run: impl FnOnce(Repository) -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&mut Tab, T, &mut Context<Tab>) + 'static,
) {
    let Some(repo) = tab.repo.clone() else { return };
    account_job(tab, cx, mutation, progress, move || run(repo), done);
}

fn account_job<T: Send + 'static>(
    tab: &mut Tab,
    cx: &mut Context<Tab>,
    mutation: bool,
    progress: &str,
    run: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&mut Tab, T, &mut Context<Tab>) + 'static,
) {
    if mutation {
        if let Some(reason) = tab.ui.jobs.refusal() {
            return message(tab, format!("{reason}. Cancel it or let it finish first."), cx);
        }
        if tab.ui.busy() {
            return message(tab, "Wait for the current operation to finish.", cx);
        }
        if tab.taking_over() {
            let progress = progress.to_owned();
            let start = move |tab: &mut Tab, cx: &mut Context<Tab>| account_job(tab, cx, mutation, &progress, run, done);
            return tab.take_over(jobs::Kind::Publish, start, cx);
        }
    }
    let epoch = tab.epoch;
    let state = &mut tab.ui.publish;
    state.ticket += 1;
    let ticket = state.ticket;
    state.working = true;
    state.failed = false;
    state.message = progress.to_owned();
    let link = mutation.then(jobs::Link::interactive);
    let id = link.clone().map(|link| tab.ui.jobs.start_network(jobs::Spec::publish(), progress, link, None));
    if mutation {
        jobs::watch(tab, cx);
    }
    cx.notify();
    Tab::work(
        cx,
        move || match link {
            Some(link) => link.enter(run),
            None => run(),
        },
        move |tab, result, cx| {
            let ended = id.and_then(|id| tab.ui.jobs.end_network(id));
            let declined = ended.as_ref().is_some_and(|job| job.link.declined());
            let cancelled = ended.is_some_and(|job| job.link.cancelled());
            crate::askpass::poll(tab, cx);
            if mutation && tab.repo.is_some() {
                tab.refs_dirty = false;
                tab.refresh(true, cx);
                tab.resume(cx);
            }
            if tab.epoch != epoch || tab.ui.publish.ticket != ticket {
                return;
            }
            tab.ui.publish.working = false;
            tab.ui.publish.message.clear();
            match result {
                Ok(value) => done(tab, value, cx),
                Err(_) if cancelled => message(
                    tab,
                    "Cancelled. A push that had started may or may not have reached the remote; fetch to check.",
                    cx,
                ),
                Err(_) if declined => message(tab, "Cancelled.", cx),
                Err(e) => message(tab, crate::explain::message(e), cx),
            }
            cx.notify();
        },
    );
}

fn stop(tab: &mut Tab, cx: &mut Context<Tab>) {
    if let Some(job) = tab.ui.jobs.network.as_ref().filter(|job| job.spec.kind == jobs::Kind::Publish) {
        if job.link.cancel() {
            tab.ui.publish.message = "Cancelling…".to_owned();
        }
    }
    cx.notify();
}

pub(crate) fn cancellable(ui: &view::Ui) -> Option<u64> {
    ui.jobs.cancellable().filter(|_| ui.jobs.network.as_ref().is_some_and(|job| job.spec.kind == jobs::Kind::Publish))
}

fn close(tab: &mut Tab) {
    let state = &mut tab.ui.publish;
    state.stop_reads();
    state.connect_now = false;
    state.after = None;
    state.page = Page::Closed;
    state.remote_edit = None;
    state.message.clear();
}

fn completed(tab: &mut Tab, notice: impl Into<String>) {
    close(tab);
    let shown = crate::notice::Notice::from_text(view::Tone::Success, &notice.into()).logged(tab.trail.entry(false));
    tab.ui.notices.show(shown);
}

fn pushed(tab: &mut Tab, notice: String, said: remotes::Said) {
    close(tab);
    let outcome = crate::sync::told(crate::notice::Outcome::titled(notice, ""), said);
    tab.ui.notices.show(outcome.notice().logged(tab.trail.entry(false)));
}

pub(crate) fn offer_branch(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.repo.is_some() && !tab.ui.busy() && !tab.ui.publish.open() {
        enter(tab, Page::Branch, cx);
    }
}

fn enter(tab: &mut Tab, page: Page, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.publish;
    state.stop_reads();
    state.page = page;
    state.remote_edit = None;
    state.failed = false;
    state.account_menu = false;
    state.owner_menu = false;
    state.push_after = true;
    state.remotes_loaded = false;
    state.repair = None;
    state.account = None;
    state.owner = None;
    state.private = true;
    state.pending = None;
    state.connect_now = false;
    state.after = None;
    set(&state.url, "", cx);
    set(&state.push_url, "", cx);
    set(&state.name, "origin", cx);
    set(&state.repository_name, &tab.ui.repo, cx);
    set(&state.destination_branch, &tab.ui.branch, cx);
    job(
        tab,
        cx,
        false,
        "Reading remotes…",
        move |repo| {
            let remotes = remotes::list(&repo)?;
            let pending = if page == Page::GitHub {
                Receipt::load(&repo)?
            } else {
                None
            };
            Ok((remotes, pending))
        },
        move |tab, (remotes, pending), cx| {
            let state = &mut tab.ui.publish;
            state.remotes_loaded = true;
            state.selected_remote = if remotes.len() == 1 { Some(0) } else { None };
            set(
                &state.name,
                &pending
                    .as_ref()
                    .map(|r| r.remote.clone())
                    .unwrap_or_else(|| suggest_name(&remotes)),
                cx,
            );
            state.remotes = remotes;
            state.pending = pending;
            if page == Page::GitHub {
                hosted::load_account(tab, cx);
            }
        },
    );
}

pub(crate) fn show_account(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.publish.open() {
        return;
    }
    let state = &mut tab.ui.publish;
    state.stop_reads();
    state.page = Page::Account;
    state.message.clear();
    state.failed = false;
    state.account = None;
    state.owner = None;
    state.pending = None;
    state.connect_now = false;
    state.after = None;
    hosted::load_account(tab, cx);
}

pub(crate) fn hide_account(tab: &mut Tab) {
    if tab.ui.publish.page == Page::Account {
        close(tab);
    }
}

pub(crate) fn sign_in(tab: &mut Tab, after: After, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    if tab.ui.busy() || tab.ui.publish.working || tab.ui.prompt.open() {
        tab.ui.notices.show(crate::notice::Notice::new(
            view::Tone::Info,
            "Wait for the current operation to finish",
            "Then connect GitHub again.",
        ));
        cx.notify();
        return;
    }
    tab.ui.focus_on_open = true;
    enter(tab, Page::GitHub, cx);
    tab.ui.publish.connect_now = true;
    tab.ui.publish.after = Some(after);
}

fn add_remote(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &tab.ui.publish;
    let name = text(&state.name, cx);
    let url = text(&state.url, cx);
    let push = Some(text(&state.push_url, cx)).filter(|push| !push.is_empty());
    let checked = remotes::validate_name(&name)
        .and_then(|_| remotes::validate_url(&url))
        .and_then(|_| push.as_deref().map_or(Ok(()), |push| remotes::validate_url(push).map_err(|e| format!("Push address: {e}"))));
    if let Err(e) = checked {
        message(tab, e, cx);
        return;
    }
    job(
        tab,
        cx,
        true,
        "Adding remote…",
        move |repo| {
            remotes::add_with_push(&repo, &name, &url, push.as_deref())?;
            match push.as_deref().filter(|push| *push != url) {
                Some(_) => crate::auth::remote_edited(&repo, &name, None)?,
                None => crate::auth::confirm_existing(&repo, &name, &url)?,
            }
            Ok((remotes::list(&repo)?, name))
        },
        |tab, (items, name), cx| {
            let state = &mut tab.ui.publish;
            state.selected_remote = items.iter().position(|r| r.name == name);
            state.remotes = items;
            state.remotes_loaded = true;
            state.page = Page::Branch;
            info(tab, format!("Connected remote {name}."), cx);
            crate::sync::fetch_remote(tab, name, cx);
        },
    );
}

fn edit_remote(tab: &mut Tab, edit: RemoteEdit, window: &mut Window, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.publish;
    let available = match state.page {
        Page::Menu if state.remotes_loaded => state.remotes.iter().any(|r| r.name == edit.name()),
        Page::Closed => {
            state.remotes_loaded = false;
            tab.ui.refs.remotes.iter().any(|name| name == edit.name())
        }
        _ => false,
    };
    if !available || remotes::validate_name(edit.name()).is_err() {
        return;
    }
    state.page = Page::Menu;
    state.message.clear();
    if let RemoteEdit::Rename(name) = &edit {
        set(&state.name, name, cx);
        window.focus(&state.name.focus_handle(cx));
    }
    state.remote_edit = Some(edit);
    cx.notify();
}

fn save_remote_edit(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &tab.ui.publish;
    let Some(edit) = state.remote_edit.clone() else { return };
    let (new_name, progress, notice) = match &edit {
        RemoteEdit::Rename(old) => {
            let name = text(&state.name, cx);
            if let Err(error) = remotes::validate_name(&name) {
                message(tab, error, cx);
                return;
            }
            if &name == old {
                return;
            }
            let notice = format!("Renamed remote {old} to {name}");
            (Some(name), "Renaming remote…", notice)
        }
        RemoteEdit::Remove(name) => (None, "Removing remote…", format!("Removed remote {name}")),
    };
    job(
        tab,
        cx,
        true,
        progress,
        move |repo| {
            let old = edit.name();
            match &new_name {
                Some(new) => remotes::rename(&repo, old, new)?,
                None => remotes::remove(&repo, old)?,
            }
            Ok(crate::auth::remote_edited(&repo, old, new_name.as_deref()).err())
        },
        move |tab, warning, _| {
            if let Some(warning) = warning {
                close(tab);
                tab.ui.say(view::Tone::Error, format!("{notice} — {warning}"));
            } else {
                completed(tab, notice);
            }
        },
    );
}

fn first_push(tab: &mut Tab, cx: &mut Context<Tab>) {
    let repair = tab.ui.publish.repair.clone();
    let target = if let Some(target) = &repair {
        target.clone()
    } else {
        let Some((branch, head)) = branch(tab) else {
            message(
                tab,
                "Check out a local branch with a commit before pushing.",
                cx,
            );
            return;
        };
        let Some(remote) = tab.ui.publish.remote() else {
            return;
        };
        let [url] = remote.push_urls.as_slice() else {
            message(tab, "Select a remote with one push destination.", cx);
            return;
        };
        FirstPush {
            branch,
            head,
            remote: remote.name.clone(),
            url: url.clone(),
            destination: text(&tab.ui.publish.destination_branch, cx),
        }
    };
    let repairing = repair.is_some();
    job(
        tab,
        cx,
        true,
        if repairing {
            "Finishing tracking setup…"
        } else {
            "Pushing branch…"
        },
        move |repo| {
            let FirstPush {
                branch,
                head,
                remote,
                url,
                destination,
            } = &target;
            let outcome = if repairing {
                remotes::repair_tracking(&repo, branch, head, remote, url, destination)?;
                PushOutcome::Tracking(remotes::Said::default())
            } else {
                remotes::first_push(&repo, branch, head, remote, url, destination)?
            };
            Ok((target, outcome))
        },
        |tab, (target, outcome), cx| match outcome {
            PushOutcome::Tracking(said) => pushed(
                tab,
                format!(
                    "{} now tracks {}/{}.",
                    target.branch, target.remote, target.destination
                ),
                said,
            ),
            PushOutcome::PublishedWithoutTracking(reason) => {
                tab.ui.publish.repair = Some(target);
                message(tab, reason, cx);
            }
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if let Action::CloseMenus = action {
        let state = &mut tab.ui.publish;
        (state.account_menu, state.owner_menu) = (false, false);
        return cx.notify();
    }
    let account = tab.ui.publish.page == Page::Account;
    if tab.repo.is_some() && tab.ui.busy() && matches!(action, Action::Escape) && !account {
        return stop(tab, cx);
    }
    if (tab.repo.is_none() && !account) || tab.ui.busy() {
        return;
    }
    view::focus(&tab.ui, window);
    match action {
        Action::Escape => {
            let state = &mut tab.ui.publish;
            if state.remote_edit.take().is_some() && state.remotes_loaded {
                state.message.clear();
            } else {
                close(tab);
            }
            cx.notify();
        }
        Action::Menu if tab.ui.publish.page == Page::Menu => {
            close(tab);
            cx.notify();
        }
        Action::Menu => enter(tab, Page::Menu, cx),
        Action::Add => enter(tab, Page::Add, cx),
        Action::Publish => enter(tab, Page::GitHub, cx),
        Action::PushBranch => enter(tab, Page::Branch, cx),
        Action::PrepareBranch if tab.ui.publish.open() => {
            close(tab);
            if tab.ui.detached {
                tab.ui.refs.list_open = false;
                crate::refs::dispatch(tab, crate::refs::Action::ToggleList, window, cx);
            } else {
                tab.dispatch(view::Action::ViewChanges, window, cx);
                window.focus(&tab.ui.input.focus_handle(cx));
            }
            cx.notify();
        }
        Action::CopyCode | Action::OpenVerification => hosted::device_action(tab, action, cx),
        Action::TogglePush => {
            tab.ui.publish.push_after = !tab.ui.publish.push_after;
            cx.notify();
        }
        Action::ToggleAccountMenu => {
            let state = &mut tab.ui.publish;
            (state.account_menu, state.owner_menu) = (!state.account_menu, false);
            cx.notify();
        }
        Action::ToggleOwnerMenu => {
            let state = &mut tab.ui.publish;
            (state.owner_menu, state.account_menu) = (!state.owner_menu, false);
            cx.notify();
        }
        _ if tab.ui.publish.working => {}
        Action::EditRemote(edit) => edit_remote(tab, edit, window, cx),
        Action::EditAddress(name) if tab.ui.publish.page == Page::Menu => {
            close(tab);
            crate::address::dispatch(tab, crate::address::Action::Edit(name), window, cx);
        }
        Action::SaveRemoteEdit if tab.ui.publish.page == Page::Menu => save_remote_edit(tab, cx),
        Action::SelectRemote(i)
            if tab.ui.publish.page == Page::Branch && i < tab.ui.publish.remotes.len() =>
        {
            tab.ui.publish.selected_remote = Some(i);
            tab.ui.publish.repair = None;
            cx.notify()
        }
        Action::AddRemote if tab.ui.publish.page == Page::Add => add_remote(tab, cx),
        Action::SubmitPush if tab.ui.publish.page == Page::Branch => first_push(tab, cx),
        action if matches!(tab.ui.publish.page, Page::GitHub | Page::Account) => {
            tab.ui.publish.account_menu = false;
            tab.ui.publish.owner_menu = false;
            hosted::dispatch(tab, action, cx)
        }
        _ => {}
    }
}
