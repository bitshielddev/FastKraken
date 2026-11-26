use crate::{
    Tab, auth, explain,
    github::{Error, Listed},
    jobs, kit,
    notice::{self, Notice},
    remotes, repository,
    settings::Memory,
    theme::*,
    view::{self, CommitInput, Desk, Tone},
};
use gpui::{App, Context, Entity, Focusable, KeyDownEvent, PathPromptOptions, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Target};
    &[Command::new(
        "repo.clone",
        Group::Repository,
        |_, _| "Clone repository…".into(),
        |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(view::Action::Clone(Action::Open)),
    )
    .words(&["git clone", "download", "url", "address", "copy a repository", "checkout"])
    .icon("arrow-down")
    .keys("cmd-n")
    .bar(Bar::File),
    Command::new(
        "repo.clone-github",
        Group::Repository,
        |_, _| "Clone from GitHub…".into(),
        |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(view::Action::Clone(Action::Browse)),
    )
    .words(&["git clone", "github", "my repositories", "list", "download"])
    .icon("cloud")]
};

pub(crate) const LISTED_NOTE: &str = "Showing repositories on accounts where FastKraken is installed.";
pub(crate) const CHOOSE: &str = "Choose repositories…";

pub(crate) const MAX_DEPTH: u32 = 1_000_000;
pub(crate) const DEPTH_PROBLEM: &str = "Enter how many of the latest commits to download, a whole number from 1.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Options {
    pub depth: Option<String>,
    pub branch: Option<String>,
    pub blobless: bool,
    pub sparse: bool,
    pub submodules: bool,
}

impl Options {
    pub(crate) fn any(&self) -> bool {
        *self != Self::default()
    }

    pub(crate) fn depth(&self) -> Option<u32> {
        self.depth.as_deref().and_then(|text| text.trim().parse().ok()).filter(|depth| (1..=MAX_DEPTH).contains(depth))
    }

    fn branch(&self) -> Option<&str> {
        self.branch.as_deref().map(str::trim).filter(|name| !name.is_empty())
    }

    pub(crate) fn problem(&self) -> Option<String> {
        if self.depth.is_some() && self.depth().is_none() {
            return Some(DEPTH_PROBLEM.to_owned());
        }
        self.branch().and_then(crate::preferences::branch_problem)
    }

    pub(crate) fn arguments(&self, local: bool) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(depth) = self.depth() {
            arguments.push(format!("--depth={depth}"));
        }
        match (self.branch.is_some(), self.branch()) {
            (true, Some(name)) => arguments.extend(["--single-branch".to_owned(), format!("--branch={name}")]),
            (true, None) => arguments.push("--single-branch".to_owned()),
            (false, _) if self.depth().is_some() => arguments.push("--no-single-branch".to_owned()),
            (false, _) => {}
        }
        if self.blobless {
            arguments.push("--filter=blob:none".to_owned());
        }
        if self.sparse {
            arguments.push("--sparse".to_owned());
        }
        if self.submodules {
            arguments.push("--recurse-submodules".to_owned());
        }
        if local && (self.depth().is_some() || self.blobless) {
            arguments.push("--no-local".to_owned());
        }
        arguments
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Shallow,
    Single,
    Blobless,
    Sparse,
    Submodules,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Request {
    pub address: String,
    pub parent: PathBuf,
    pub name: String,
    pub options: Options,
    pub account: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Source {
    #[default]
    Address,
    GitHub,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub account: u64,
    pub login: String,
    pub install: String,
    pub repositories: Vec<Listed>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Listing {
    #[default]
    Idle,
    Loading,
    SignedOut(Option<String>),
    Failed(String),
    Ready(Account),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Connection {
    Bound,
    Unbound(String),
}

impl Request {
    pub(crate) fn destination(&self) -> PathBuf {
        self.parent.join(&self.name)
    }

    pub(crate) fn problem(&self) -> Option<String> {
        address_problem(&self.address)
            .or_else(|| name_problem(&self.name))
            .or_else(|| destination_problem(&self.parent, &self.destination()).err())
            .or_else(|| self.options.problem())
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open,
    Edit(Request),
    ChooseParent,
    SetParent(PathBuf),
    Submit,
    Close,
    Again(Request),
    Show(PathBuf),
    ToggleAdvanced,
    Toggle(Choice),
    Browse,
    Source(Source),
    Reload,
    Pick(String),
    PickFirst,
    ConnectGitHub,
}

pub(crate) struct State {
    open: bool,
    pub(crate) address: Entity<CommitInput>,
    pub(crate) name: Entity<CommitInput>,
    parent: PathBuf,
    derived: String,
    pub(crate) depth: Entity<CommitInput>,
    pub(crate) branch: Entity<CommitInput>,
    advanced: bool,
    shallow: bool,
    single: bool,
    blobless: bool,
    sparse: bool,
    submodules: bool,
    source: Source,
    listing: Listing,
    ticket: u64,
    pub(crate) filter: Entity<CommitInput>,
    picked: Option<(String, u64)>,
    list_scroll: gpui::ScrollHandle,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let address = cx.new(|cx| CommitInput::with_placeholder(cx, "https://github.com/owner/repository.git"));
        let name = cx.new(|cx| CommitInput::with_placeholder(cx, "Folder name"));
        cx.observe(&address, |tab, _, cx| follow(tab, cx)).detach();
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        let depth = cx.new(|cx| CommitInput::with_placeholder(cx, "1"));
        let branch = cx.new(|cx| CommitInput::with_placeholder(cx, "The default branch"));
        cx.observe(&depth, |_, _, cx| cx.notify()).detach();
        cx.observe(&branch, |_, _, cx| cx.notify()).detach();
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter by owner or name"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self {
            open: false,
            address,
            name,
            parent: PathBuf::new(),
            derived: String::new(),
            depth,
            branch,
            advanced: false,
            shallow: false,
            single: false,
            blobless: false,
            sparse: false,
            submodules: false,
            source: Source::Address,
            listing: Listing::Idle,
            ticket: 0,
            filter,
            picked: None,
            list_scroll: gpui::ScrollHandle::new(),
        }
    }

    pub(crate) fn source(&self) -> Source {
        self.source
    }

    pub(crate) fn listing(&self) -> &Listing {
        &self.listing
    }

    pub(crate) fn shown(&self, cx: &App) -> Vec<&Listed> {
        let Listing::Ready(account) = &self.listing else { return Vec::new() };
        let filter = self.filter.read(cx).text().trim().to_ascii_lowercase();
        account.repositories.iter().filter(|repository| repository.full_name().to_ascii_lowercase().contains(&filter)).collect()
    }

    pub(crate) fn advanced(&self) -> bool {
        self.advanced
    }

    pub(crate) fn chosen(&self, choice: Choice) -> bool {
        match choice {
            Choice::Shallow => self.shallow,
            Choice::Single => self.single,
            Choice::Blobless => self.blobless,
            Choice::Sparse => self.sparse,
            Choice::Submodules => self.submodules,
        }
    }

    fn options(&self, cx: &App) -> Options {
        Options {
            depth: self.shallow.then(|| self.depth.read(cx).text().trim().to_owned()),
            branch: self.single.then(|| self.branch.read(cx).text().trim().to_owned()),
            blobless: self.blobless,
            sparse: self.sparse,
            submodules: self.submodules,
        }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn parent(&self) -> &Path {
        &self.parent
    }

    pub(crate) fn request(&self, cx: &App) -> Request {
        let address = self.address.read(cx).text().trim().to_owned();
        let account = self.picked.as_ref().filter(|(url, _)| *url == address).map(|(_, account)| *account);
        Request {
            address,
            parent: self.parent.clone(),
            name: self.name.read(cx).text().trim().to_owned(),
            options: self.options(cx),
            account,
        }
    }
}

fn follow(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.cloning;
    let derived = folder_name(state.address.read(cx).text().trim()).unwrap_or_default();
    if derived != state.derived {
        if state.name.read(cx).text() == state.derived {
            state.name.update(cx, |name, cx| name.set_text(&derived, cx));
        }
        state.derived = derived;
    }
    cx.notify();
}


pub(crate) fn folder_name(address: &str) -> Option<String> {
    let mut rest = address.trim().trim_end_matches('/');
    for suffix in ["/.git", ".git", ".bundle"] {
        if let Some(stripped) = rest.strip_suffix(suffix) {
            rest = stripped.trim_end_matches('/');
            break;
        }
    }
    let last = rest.rsplit(['/', ':']).next().unwrap_or(rest);
    let name = last.trim();
    (!name.is_empty() && !matches!(name, "." | "..") && !name.contains('\\')).then(|| name.to_owned())
}

pub(crate) fn copied_address(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(['\n', '\r']) || remotes::validate_url(text).is_err() || address_problem(text).is_some() {
        return None;
    }
    let scp = !text.contains("://") && text.split_once(':').is_some_and(|(host, _)| host.contains('@') && !host.contains('/'));
    (text.contains("://") || scp || text.ends_with(".git")).then(|| text.to_owned())
}

fn local(address: &str) -> bool {
    !address.contains("://") && !address.split_once(':').is_some_and(|(host, _)| !host.contains('/'))
}

fn address_problem(address: &str) -> Option<String> {
    if address.is_empty() {
        return Some(String::new());
    }
    if let Err(problem) = remotes::validate_url(address) {
        return Some(problem);
    }
    (local(address) && !address.starts_with('/')).then(|| "Name a repository on this Mac by its full path, starting with /.".to_owned())
}

pub(crate) fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some(String::new());
    }
    (name.contains(['/', '\0']) || matches!(name, "." | ".."))
        .then(|| "Enter a folder name without slashes.".to_owned())
}

pub(crate) fn destination_problem(parent: &Path, destination: &Path) -> Result<bool, String> {
    if !parent.is_absolute() || !parent.is_dir() {
        return Err(format!("{} is not a folder that exists.", parent.display()));
    }
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{} cannot be read: {error}.", destination.display())),
        Ok(meta) if !meta.is_dir() => Err(format!("{} already exists and is not a folder.", destination.display())),
        Ok(_) => match fs::read_dir(destination).map(|mut entries| entries.next().is_none()) {
            Ok(true) => Ok(true),
            Ok(false) => Err(format!("{} already exists and is not empty.", destination.display())),
            Err(error) => Err(format!("{} cannot be read: {error}.", destination.display())),
        },
    }
}


#[cfg(test)]
pub(crate) fn clone(request: &Request) -> Result<PathBuf, String> {
    cloned(request).map(|(root, _)| root)
}

pub(crate) fn cloned(request: &Request) -> Result<(PathBuf, Option<Connection>), String> {
    let failed = |reason: String| format!("Clone Failed — {reason}");
    if let Some(problem) = request.problem() {
        return Err(failed(if problem.is_empty() { "Enter an address and a folder name.".to_owned() } else { problem }));
    }
    let destination = request.destination();
    let access = match request.account {
        Some(account) => Some(auth::clone_access(&destination, &request.address, account).map_err(failed)?),
        None => None,
    };
    let created = match fs::create_dir(&destination) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !destination_problem(&request.parent, &destination).map_err(failed)? {
                return Err(failed(format!("{} changed while the clone started.", destination.display())));
            }
            false
        }
        Err(error) => return Err(failed(format!("Could not create {}: {error}.", destination.display()))),
    };
    let result = repository::clone_command(&request.parent).and_then(|mut command| {
        if let Some(access) = &access {
            access.configure(&mut command)?;
        }
        command.args(["clone", "--progress"]).args(request.options.arguments(local(&request.address)));
        if request.options.submodules {
            repository::with_system_tools(&mut command);
        }
        command.arg("--").arg(&request.address).arg(&request.name);
        repository::checked(&mut command).map_err(|error| {
            if error.contains("transport 'file' not allowed") { crate::submodule::LOCAL_REFUSED.to_owned() } else { error }
        })
    });
    match result {
        Ok(_) => {
            let root = fs::canonicalize(&destination).map_err(|error| failed(format!("{} cannot be read: {error}.", destination.display())))?;
            let connection = access.as_ref().map(|access| match repository::Repository::open(&root).and_then(|repo| access.bind(&repo)) {
                Ok(()) => Connection::Bound,
                Err(why) => Connection::Unbound(why),
            });
            Ok((root, connection))
        }
        Err(error) => {
            discard(&destination, created);
            let said: Vec<&str> = error.lines().filter(|line| !line.starts_with("Cloning into ")).collect();
            Err(failed(said.join("\n")))
        }
    }
}

pub(crate) fn discard(destination: &Path, created: bool) {
    if created {
        let _ = fs::remove_dir_all(destination);
        return;
    }
    if let Ok(entries) = fs::read_dir(destination) {
        for entry in entries.flatten() {
            let path = entry.path();
            let _ = match entry.file_type() {
                Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
                _ => fs::remove_file(&path),
            };
        }
    }
}


pub(crate) fn default_parent(cx: &App) -> PathBuf {
    crate::settings::settings(cx)
        .clone_folder()
        .filter(|parent| parent.is_dir())
        .or_else(|| crate::settings::memory(cx).and_then(|memory| memory.clone_parent()).filter(|parent| parent.is_dir()))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_dir()))
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn clipboard(cx: &App) -> Option<String> {
    if cfg!(test) || crate::acceptance::replaying() || std::env::var_os("FASTKRAKEN_SCENE").is_some() {
        return None;
    }
    copied_address(&cx.read_from_clipboard()?.text()?)
}

fn show(tab: &mut Tab, request: Option<Request>, window: &mut Window, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let request = request.unwrap_or_else(|| Request {
        address: clipboard(cx).unwrap_or_default(),
        parent: default_parent(cx),
        name: String::new(),
        options: Options::default(),
        account: None,
    });
    let state = &mut tab.ui.cloning;
    state.open = true;
    state.source = Source::Address;
    state.ticket += 1;
    state.listing = Listing::Idle;
    state.picked = request.account.map(|account| (request.address.clone(), account));
    state.parent = request.parent;
    state.derived = String::new();
    let options = request.options;
    state.advanced = options.any();
    state.shallow = options.depth.is_some();
    state.single = options.branch.is_some();
    state.blobless = options.blobless;
    state.sparse = options.sparse;
    state.submodules = options.submodules;
    state.depth.update(cx, |field, cx| field.set_text(options.depth.as_deref().unwrap_or_default(), cx));
    state.branch.update(cx, |field, cx| field.set_text(options.branch.as_deref().unwrap_or_default(), cx));
    state.name.update(cx, |name, cx| name.set_text(&request.name, cx));
    state.address.update(cx, |address, cx| address.set_text(&request.address, cx));
    if request.name.is_empty() {
        follow(tab, cx);
    }
    window.focus(&tab.ui.cloning.address.focus_handle(cx));
}

fn list(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.cloning;
    if state.listing == Listing::Loading {
        return;
    }
    state.ticket += 1;
    let ticket = state.ticket;
    state.listing = Listing::Loading;
    cx.notify();
    Tab::work(cx, listed, move |tab, listing, cx| {
        if tab.ui.cloning.ticket == ticket {
            tab.ui.cloning.listing = listing;
            cx.notify();
        }
    });
}

pub(crate) fn listed() -> Listing {
    let client = match auth::client() {
        Ok(client) => client,
        Err(why) => return Listing::SignedOut(Some(why)),
    };
    let (account, tokens) = match auth::active(&client) {
        Ok(Some(signed_in)) => signed_in,
        Ok(None) => return Listing::SignedOut(None),
        Err(why) if why.starts_with(auth::LAPSED) => return Listing::SignedOut(Some(why)),
        Err(why) => return Listing::Failed(why),
    };
    let lapsed = |error: Error| match error {
        Error::Unauthorized(said) => Listing::SignedOut(Some(format!("{}. {said}", auth::LAPSED))),
        error => Listing::Failed(error.to_string()),
    };
    let login = match client.user(&tokens.access_token) {
        Ok((id, login)) if id == account => login,
        Ok(_) => return Listing::SignedOut(Some("GitHub returned a different account. Connect again.".into())),
        Err(error) => return lapsed(error),
    };
    match client.repositories(&tokens.access_token) {
        Ok(repositories) => Listing::Ready(Account { account, login, install: client.installation_url(), repositories }),
        Err(error) => lapsed(error),
    }
}

fn pick(tab: &mut Tab, full_name: &str, window: &mut Window, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.cloning;
    let Listing::Ready(account) = &state.listing else { return };
    let Some(repository) = account.repositories.iter().find(|repository| repository.full_name() == full_name) else { return };
    let url = repository.clone_url();
    state.picked = Some((url.clone(), account.account));
    state.address.update(cx, |address, cx| address.set_text(&url, cx));
    follow(tab, cx);
    window.focus(&tab.ui.cloning.name.focus_handle(cx));
}

fn close(tab: &mut Tab, window: &mut Window) {
    tab.ui.cloning.open = false;
    view::focus(&tab.ui, window);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open if tab.ui.cloning.open => {}
        Action::Open => show(tab, None, window, cx),
        Action::Edit(request) => show(tab, Some(request), window, cx),
        Action::Close => close(tab, window),
        Action::ChooseParent => {
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("Clone here".into()),
            });
            let tab_handle = cx.weak_entity();
            window
                .spawn(cx, async move |cx| {
                    if let Ok(Ok(Some(paths))) = picker.await {
                        if let Some(path) = paths.into_iter().next() {
                            let _ = cx.update(|window, cx| {
                                tab_handle.update(cx, |tab, cx| {
                                    tab.dispatch(view::Action::Clone(Action::SetParent(path)), window, cx)
                                })
                            });
                        }
                    }
                })
                .detach();
        }
        Action::SetParent(path) => tab.ui.cloning.parent = path,
        Action::ToggleAdvanced => tab.ui.cloning.advanced = !tab.ui.cloning.advanced,
        Action::Toggle(choice) => {
            let state = &mut tab.ui.cloning;
            let flag = match choice {
                Choice::Shallow => &mut state.shallow,
                Choice::Single => &mut state.single,
                Choice::Blobless => &mut state.blobless,
                Choice::Sparse => &mut state.sparse,
                Choice::Submodules => &mut state.submodules,
            };
            *flag = !*flag;
            let field = match choice {
                Choice::Shallow if state.shallow => Some(state.depth.clone()),
                Choice::Single if state.single => Some(state.branch.clone()),
                _ => None,
            };
            if let Some(field) = field {
                if choice == Choice::Shallow && field.read(cx).text().trim().is_empty() {
                    field.update(cx, |field, cx| field.set_text("1", cx));
                }
                window.focus(&field.focus_handle(cx));
            }
        }
        Action::Submit => {
            let request = tab.ui.cloning.request(cx);
            if request.problem().is_some() {
                return;
            }
            if let Some(reason) = tab.ui.jobs.refusal() {
                tab.refuse(format!("{reason}. Cancel it or let it finish before cloning."), cx);
                return;
            }
            close(tab, window);
            start(tab, request, cx);
        }
        Action::Again(request) => {
            if request.problem().is_some() {
                show(tab, Some(request), window, cx);
            } else {
                start(tab, request, cx);
            }
        }
        Action::Show(path) if tab.repo.is_some() && crate::tabs::present(cx) => {
            crate::tabs::open_elsewhere(path, |_, _| {}, cx);
        }
        Action::Show(_) if !tab.can_open() || tab.ui.opening.is_some() => {
            tab.refuse("Wait for the current operation to finish, then open the clone.".to_owned(), cx);
        }
        Action::Show(path) => tab.open(path, cx),
        Action::Browse => {
            if !tab.ui.cloning.open {
                show(tab, None, window, cx);
            }
            tab.ui.cloning.source = Source::GitHub;
            list(tab, cx);
            window.focus(&tab.ui.cloning.filter.focus_handle(cx));
        }
        Action::Source(source) => {
            tab.ui.cloning.source = source;
            match source {
                Source::GitHub => {
                    if !matches!(tab.ui.cloning.listing, Listing::Ready(_)) {
                        list(tab, cx);
                    }
                    window.focus(&tab.ui.cloning.filter.focus_handle(cx));
                }
                Source::Address => window.focus(&tab.ui.cloning.address.focus_handle(cx)),
            }
        }
        Action::Reload => {
            tab.ui.cloning.listing = Listing::Idle;
            list(tab, cx);
        }
        Action::Pick(full_name) => pick(tab, &full_name, window, cx),
        Action::PickFirst => {
            let first = tab.ui.cloning.shown(cx).first().map(|repository| repository.full_name());
            if let Some(full_name) = first {
                pick(tab, &full_name, window, cx);
            }
        }
        Action::ConnectGitHub => {
            if tab.repo.is_none() {
                tab.refuse("Open a repository, connect GitHub from Publish to GitHub…, then your repositories are listed here".to_owned(), cx);
                return;
            }
            close(tab, window);
            let again = Some(Box::new(view::Action::Clone(Action::Browse)));
            crate::publish::sign_in(tab, crate::publish::After::Retry(again), cx);
        }
    }
    cx.notify();
}

pub(crate) fn start(tab: &mut Tab, request: Request, cx: &mut Context<Tab>) {
    if let Some(reason) = tab.ui.jobs.refusal() {
        tab.refuse(format!("{reason}. Cancel it or let it finish before cloning."), cx);
        return;
    }
    if tab.taking_over() {
        return tab.take_over(jobs::Kind::Clone, move |tab, cx| start(tab, request, cx), cx);
    }
    let parent = request.parent.clone();
    Memory::update(cx, |memory| memory.set_clone_parent(&parent));
    let host = explain::host_of(&request.address).filter(|_| !local(&request.address));
    let progress = format!("Cloning into {}…", request.destination().display());
    let link = jobs::Link::interactive();
    let spinner = tab.ui.notices.progress(progress.clone());
    let id = tab.ui.jobs.start_network(jobs::Spec::clone_from(host), &progress, link.clone(), Some(spinner));
    jobs::tick(&mut tab.ui, std::time::Instant::now());
    jobs::watch(tab, cx);
    cx.notify();
    let about = explain::About {
        url: Some(request.address.clone()),
        again: Some(view::Action::Clone(Action::Again(request.clone()))),
        cloning: true,
        ..explain::About::default()
    };
    let asked = request.clone();
    let work = move || link.enter(|| cloned(&asked).map_err(|error| explain::explain(notice::Failure::from(error), &about)));
    Tab::work(cx, work, move |tab, result, cx| {
        let Some(job) = tab.ui.jobs.end_network(id) else { return };
        crate::askpass::poll(tab, cx);
        let logged = |tab: &Tab, failed: bool| tab.repo.as_ref().and_then(|_| tab.trail.entry(failed));
        match result {
            Err(_) if job.link.cancelled() => {
                let shown = jobs::cancelled(&job.spec).logged(logged(tab, true));
                tab.ui.notices.finish(spinner, shown);
            }
            Err(_) if job.link.declined() => {
                tab.ui.notices.finish(spinner, Notice::new(Tone::Info, "Cancelled", ""));
            }
            Err(failure) => {
                let shown = failure.notice().logged(logged(tab, true));
                tab.ui.notices.finish(spinner, shown);
            }
            Ok((root, connection)) => {
                Memory::update(cx, |memory| memory.add_recent(&root, view::now()));
                let name = root.file_name().map_or_else(|| request.name.clone(), |name| name.to_string_lossy().into_owned());
                let connected = match connection {
                    None => String::new(),
                    Some(Connection::Bound) => " origin uses your GitHub connection.".to_owned(),
                    Some(Connection::Unbound(why)) => format!(" The GitHub connection was not set up for origin: {why}"),
                };
                let cloned = Notice::new(Tone::Success, format!("Cloned {name}"), format!("Into {}.{connected}", request.parent.display()));
                let in_the_way = tab.ui.dialog_open() || tab.ui.modal_open();
                if tab.repo.is_some() && !in_the_way && crate::tabs::present(cx) {
                    tab.ui.notices.remove(spinner);
                    crate::tabs::open_elsewhere(root, move |tab, _| { tab.ui.notices.show(cloned); }, cx);
                } else if tab.repo.is_none() && tab.ui.opening.is_none() && !in_the_way {
                    tab.ui.notices.remove(spinner);
                    tab.open_then(root, move |tab, _| { tab.ui.notices.show(cloned); }, cx);
                } else {
                    let shown = cloned.offer("Open now", view::Action::Clone(Action::Show(root))).sticky().logged(logged(tab, false));
                    tab.ui.notices.finish(spinner, shown);
                }
            }
        }
        cx.notify();
    });
}


fn submit<T: Desk>(cx: &mut Context<T>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(|app: &mut T, e: &KeyDownEvent, w, cx| {
        if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
            app.dispatch(view::Action::Clone(Action::Submit), w, cx);
            cx.stop_propagation();
        }
    })
}

pub(crate) fn dialog<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    let state = &ui.cloning;
    if !state.open {
        return None;
    }
    let request = state.request(cx);
    let address = address_problem(&request.address).filter(|problem| !problem.is_empty());
    let name = name_problem(&request.name).filter(|problem| !problem.is_empty());
    let destination = destination_problem(&request.parent, &request.destination()).err();
    let ready = request.problem().is_none();
    let parent = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .px(px(10.))
                .py(px(6.))
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(palette().line))
                .bg(rgb(palette().canvas))
                .child(kit::fit(request.parent.display().to_string()).cut(kit::Cut::Start).grow()),
        )
        .child(kit::btn("clone-choose", "Choose…").on(view::Action::Clone(Action::ChooseParent), cx));
    let full = if request.name.is_empty() || name.is_some() {
        kit::muted("Full path: choose a folder name")
    } else {
        kit::muted(format!("Full path: {}", request.destination().display()))
    };
    let taken = destination.filter(|_| name.is_none() && !request.name.is_empty()).map(|problem| {
        let at = request.destination().display().to_string();
        match problem.strip_prefix(&at) {
            Some(rest) => format!("A folder named {}{rest}", request.name),
            None => "The folder to clone into does not exist. Choose another.".to_owned(),
        }
    });
    let source = state.source;
    let sources = kit::segmented()
        .child(kit::segment("clone-source-address", "Address", None, source == Source::Address, view::Action::Clone(Action::Source(Source::Address)), cx))
        .child(kit::segment("clone-source-github", "GitHub", Some("cloud"), source == Source::GitHub, view::Action::Clone(Action::Source(Source::GitHub)), cx));
    let body = kit::dialog("Clone a repository", 480.)
        .child(div().flex().child(sources))
        .when(source == Source::Address, |d| {
            d.child(div().on_key_down(submit(cx)).child(kit::field("Repository address", state.address.clone(), address.map(Into::into))))
        })
        .when(source == Source::GitHub, |d| {
            d.child(github(app, &request, cx)).when(!request.address.is_empty(), |d| d.child(kit::muted(format!("Address: {}", request.address))))
        })
        .child(kit::field("Clone into", parent, None))
        .child(div().on_key_down(submit(cx)).child(kit::field("Folder name", state.name.clone(), name.map(Into::into))))
        .child(full.text_size(px(TEXT_SMALL)))
        .when_some(taken, |d, taken| d.child(div().text_size(px(TEXT_SMALL)).text_color(rgb(palette().deleted)).child(taken)))
        .child(advanced(app, &request, cx))
        .child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(kit::btn("clone-cancel", "Cancel").on(view::Action::Clone(Action::Close), cx))
                .child(kit::btn("clone-start", "Clone").primary().enabled(ready).on(view::Action::Clone(Action::Submit), cx)),
        );
    Some(kit::modal(body, Some(view::Action::Clone(Action::Close)), &ui.modal_focus, cx))
}

fn pick_first<T: Desk>(cx: &mut Context<T>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(|app: &mut T, e: &KeyDownEvent, w, cx| {
        if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
            app.dispatch(view::Action::Clone(Action::PickFirst), w, cx);
            cx.stop_propagation();
        }
    })
}

pub(crate) fn choose(account: &Account) -> view::Action {
    view::Action::Handoff(crate::handoff::Action::OpenUrl(account.install.clone()))
}

pub(crate) fn pushed(repository: &Listed, now: i64) -> Option<String> {
    repository.pushed.map(|time| format!("Pushed {}", view::when(time, &view::date_of(time), now)))
}

fn github<T: Desk>(app: &T, request: &Request, cx: &mut Context<T>) -> gpui::Div {
    let ui = app.ui();
    let state = &ui.cloning;
    let body = div().flex().flex_col().gap_2();
    let problem = |text: String| div().text_size(px(TEXT_SMALL)).text_color(rgb(palette().deleted)).child(text);
    match &state.listing {
        Listing::Idle | Listing::Loading => body.child(
            div()
                .h(px(64.))
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .child(kit::spinner("clone-github-spinner"))
                .child(kit::muted("Reading your GitHub repositories…")),
        ),
        Listing::SignedOut(why) => {
            let connect = if ui.repo.is_empty() {
                kit::muted("Open a repository and connect GitHub from Publish to GitHub…, then come back here.").into_any_element()
            } else {
                div().flex().child(kit::btn("clone-github-connect", "Connect GitHub…").primary().on(view::Action::Clone(Action::ConnectGitHub), cx)).into_any_element()
            };
            body.child(div().text_size(px(12.5)).text_color(rgb(palette().text)).child("Connect GitHub to list the repositories you can clone."))
                .when_some(why.clone(), |d, why| d.child(kit::muted(why)))
                .child(connect)
        }
        Listing::Failed(why) => body
            .child(problem(format!("The repositories were not read: {why}")))
            .child(div().flex().child(kit::btn("clone-github-retry", "Try again").on(view::Action::Clone(Action::Reload), cx))),
        Listing::Ready(account) => {
            let shown = state.shown(cx);
            let filter = state.filter.read(cx).text().trim().to_owned();
            let now = view::now();
            let mut list = div()
                .id("clone-github-list")
                .h(px(196.))
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&state.list_scroll)
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(palette().line))
                .bg(rgb(palette().canvas));
            if shown.is_empty() {
                let empty = if account.repositories.is_empty() {
                    "No repositories yet on the accounts where FastKraken is installed.".to_owned()
                } else {
                    format!("No repository matches “{filter}”.")
                };
                list = list.child(div().p(px(10.)).child(kit::muted(empty)));
            }
            for (i, repository) in shown.iter().enumerate() {
                let full_name = repository.full_name();
                let chosen = request.account.is_some() && request.address == repository.clone_url();
                let row = kit::row(("clone-github-row", i), chosen)
                    .h(px(32.))
                    .px(px(10.))
                    .gap_2()
                    .focusable()
                    .focus(|s| s.bg(rgb(palette().hover)))
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(palette().line)))
                    .child(kit::icon(if repository.private { "lock" } else { "globe" }).size(px(12.)).text_color(rgb(palette().text_3)))
                    .child(kit::fit(full_name.clone()).size(12.5).grow())
                    .child(kit::muted(if repository.private { "Private" } else { "Public" }))
                    .when_some(pushed(repository, now), |d, pushed| d.child(kit::muted(pushed).w(px(112.)).flex().justify_end()));
                list = list.child(kit::pressable(row, view::Action::Clone(Action::Pick(full_name)), cx));
            }
            body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().on_key_down(pick_first(cx)).child(state.filter.clone()))
                    .child(kit::muted(format!("Signed in as {}", account.login)))
                    .child(kit::link("clone-github-reload", "Refresh", view::Action::Clone(Action::Reload), cx)),
            )
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kit::muted(LISTED_NOTE).flex_1().min_w_0())
                    .child(kit::link("clone-github-choose", CHOOSE, choose(account), cx)),
            )
        }
    }
}

fn choice<T: Desk>(state: &State, choice: Choice, title: &'static str, help: &'static str, cx: &Context<T>) -> gpui::Stateful<gpui::Div> {
    let row = div()
        .id(("clone-option", choice as usize))
        .flex()
        .items_start()
        .gap_2()
        .py(px(3.))
        .cursor_pointer()
        .focusable()
        .focus(|s| s.bg(rgb(palette().hover)))
        .child(div().pt(px(2.)).child(kit::checkbox(state.chosen(choice))))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(12.5)).text_color(rgb(palette().text)).child(title))
                .child(kit::muted(help).text_size(px(11.5))),
        );
    kit::pressable(row, view::Action::Clone(Action::Toggle(choice)), cx)
}

fn advanced<T: Desk>(app: &T, request: &Request, cx: &mut Context<T>) -> gpui::Div {
    let state = &app.ui().cloning;
    let open = state.advanced;
    let disclosure = div()
        .id("clone-advanced")
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(palette().text_2))
        .focusable()
        .focus(|s| s.text_color(rgb(palette().text)))
        .child(kit::icon(if open { "chevron-down" } else { "chevron-right" }).size(px(12.)).text_color(rgb(palette().text_3)))
        .child("Advanced")
        .when(!open && request.options.any(), |d| d.child(kit::muted("· options chosen")));
    let body = div().flex().flex_col().gap_1().child(kit::pressable(disclosure, view::Action::Clone(Action::ToggleAdvanced), cx));
    if !open {
        return body;
    }
    let depth = request.options.depth.is_some().then(|| request.options.depth().is_none()).unwrap_or(false);
    let branch = request.options.branch.is_some().then(|| request.options.problem()).flatten().filter(|_| !depth);
    let field = |input: Entity<CommitInput>, problem: Option<String>, width: f32, cx: &mut Context<T>| {
        div()
            .pl(px(22.))
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(div().w(px(width)).on_key_down(submit(cx)).child(input))
            .when_some(problem, |d, problem| d.child(div().text_size(px(TEXT_SMALL)).text_color(rgb(palette().deleted)).child(problem)))
    };
    let mut options = div().pl_1().flex().flex_col().gap_1();
    options = options.child(choice(state, Choice::Shallow, "Only the latest commits", "Downloads less history. Fetch the full history later from the end of the graph.", cx));
    if state.shallow {
        options = options.child(field(state.depth.clone(), depth.then(|| DEPTH_PROBLEM.to_owned()), 90., cx));
    }
    options = options.child(choice(state, Choice::Single, "Only one branch", "Leave the name empty for the repository's default branch.", cx));
    if state.single {
        options = options.child(field(state.branch.clone(), branch, 220., cx));
    }
    let options = options
        .child(choice(state, Choice::Blobless, "Download file contents when needed", "Older versions of files are fetched when you look at them. The host must support it, as GitHub does.", cx))
        .child(choice(state, Choice::Sparse, "Start with only the top-level files", "Choose the folders to check out afterwards, in Preferences under the repository.", cx))
        .child(choice(state, Choice::Submodules, "Include submodules", "Clones each submodule at the commit the repository records.", cx));
    body.child(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::net::TcpListener;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    #[test]
    fn the_folder_name_is_the_last_part_of_the_address_without_git() {
        for (address, name) in [
            ("https://github.com/team/trail.git", "trail"),
            ("https://github.com/team/trail", "trail"),
            ("https://github.com/team/trail/", "trail"),
            ("git@github.com:team/trail.git", "trail"),
            ("git@github.com:trail.git", "trail"),
            ("ssh://git@git.example.invalid:2222/team/trail.git", "trail"),
            ("/srv/git/trail.git", "trail"),
            ("/srv/git/trail/.git", "trail"),
            ("/srv/git/trail", "trail"),
            ("file:///srv/git/Juniper-trail.git", "Juniper-trail"),
        ] {
            assert_eq!(folder_name(address).as_deref(), Some(name), "{address}");
        }
        for address in ["", "/", ".git", "https://"] {
            assert_eq!(folder_name(address), None, "{address}");
        }
    }

    #[test]
    fn only_a_plain_git_address_on_the_clipboard_is_offered() {
        for text in [
            "https://github.com/team/trail.git",
            "  https://github.com/team/trail\n",
            "git@github.com:team/trail.git",
            "ssh://git@git.example.invalid/team/trail",
            "/srv/git/trail.git",
        ] {
            assert_eq!(copied_address(text).as_deref(), Some(text.trim()), "{text:?}");
        }
        for text in [
            "",
            "Remember to mark the bridge.",
            "https://github.com/team/trail.git\nand more",
            "https://user:secret@github.com/team/trail.git",
            "/Users/me/notes.txt",
            "../peer.git",
            "-u https://github.com/team/trail.git",
        ] {
            assert_eq!(copied_address(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_relative_path_or_a_slash_in_the_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let request = |address: &str, name: &str| Request { address: address.into(), parent: dir.path().into(), name: name.into(), ..Request::default() };
        assert_eq!(request("", "trail").problem().as_deref(), Some(""));
        assert_eq!(request("https://github.com/team/trail.git", "").problem().as_deref(), Some(""));
        assert!(request("../peer.git", "peer").problem().unwrap().contains("full path"));
        assert!(request("peer.git", "peer").problem().unwrap().contains("full path"));
        assert_eq!(request("/srv/git/trail.git", "a/b").problem().as_deref(), Some("Enter a folder name without slashes."));
        assert_eq!(request("/srv/git/trail.git", "..").problem().as_deref(), Some("Enter a folder name without slashes."));
        assert_eq!(request("https://github.com/team/trail.git", "trail").problem(), None);
        let gone = Request { parent: dir.path().join("missing"), ..request("/srv/git/trail.git", "trail") };
        assert!(gone.problem().unwrap().ends_with("is not a folder that exists."));
    }

    fn bare(f: &Fixture, dir: &Path) -> PathBuf {
        fs::write(f.dir.join("trail.txt"), "FastKraken: git for the impatient\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", "Start the trail"]);
        fs::write(f.dir.join("camp.txt"), "Camp at the ridge\n").unwrap();
        f.git(&["add", "camp.txt"]);
        f.git(&["commit", "-qm", "Mark the camp"]);
        f.git(&["tag", "v1"]);
        let bare = dir.join("Juniper-trail.git");
        f.git(&["clone", "-q", "--bare", f.dir.to_str().unwrap(), bare.to_str().unwrap()]);
        bare
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = repository::bundled_command(dir).unwrap().args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn a_clone_over_the_file_protocol_has_every_commit_and_checks_out_main() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let bare = bare(&f, dir.path());
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let address = format!("file://{}", bare.display());
        let request = Request { address: address.clone(), parent: clones.clone(), name: folder_name(&address).unwrap(), ..Request::default() };
        assert_eq!(request.problem(), None);
        let root = clone(&request).unwrap();
        assert_eq!(root, fs::canonicalize(clones.join("Juniper-trail")).unwrap());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), String::from_utf8(f.git(&["rev-parse", "main"])).unwrap());
        assert_eq!(git(&root, &["symbolic-ref", "--short", "HEAD"]), "main\n");
        assert_eq!(git(&root, &["log", "--format=%s"]), "Mark the camp\nStart the trail\n");
        assert_eq!(git(&root, &["tag", "--list"]), "v1\n");
        assert_eq!(git(&root, &["for-each-ref", "--format=%(refname)", "refs/remotes"]), "refs/remotes/origin/HEAD\nrefs/remotes/origin/main\n");
        assert_eq!(git(&root, &["remote", "get-url", "origin"]), format!("{address}\n"));
        assert_eq!(git(&root, &["status", "--porcelain"]), "");
        assert_eq!(fs::read_to_string(root.join("camp.txt")).unwrap(), "Camp at the ridge\n");
        fs::create_dir(clones.join("empty")).unwrap();
        let into_empty = Request { name: "empty".into(), ..request.clone() };
        assert_eq!(into_empty.problem(), None);
        let root = clone(&into_empty).unwrap();
        assert_eq!(git(&root, &["log", "-1", "--format=%s"]), "Mark the camp\n");
    }

    #[test]
    fn the_advanced_options_become_clone_arguments_and_are_checked_first() {
        let shallow = Options { depth: Some("3".into()), ..Options::default() };
        assert_eq!(shallow.arguments(false), ["--depth=3", "--no-single-branch"]);
        assert_eq!(shallow.arguments(true), ["--depth=3", "--no-single-branch", "--no-local"]);
        let single = Options { branch: Some(" ridge ".into()), ..Options::default() };
        assert_eq!(single.arguments(false), ["--single-branch", "--branch=ridge"]);
        let default_branch = Options { depth: Some("1".into()), branch: Some(String::new()), ..Options::default() };
        assert_eq!(default_branch.arguments(false), ["--depth=1", "--single-branch"]);
        let rest = Options { blobless: true, sparse: true, submodules: true, ..Options::default() };
        assert_eq!(rest.arguments(true), ["--filter=blob:none", "--sparse", "--recurse-submodules", "--no-local"]);
        assert_eq!(Options { sparse: true, ..Options::default() }.arguments(true), ["--sparse"]);
        assert!(Options::default().arguments(true).is_empty());
        assert!(!Options::default().any() && rest.any());
        for depth in ["", "0", "-1", "two", "1.5", "1000001"] {
            assert_eq!(Options { depth: Some(depth.into()), ..Options::default() }.problem().as_deref(), Some(DEPTH_PROBLEM), "{depth:?}");
        }
        assert_eq!(Options { depth: Some(" 12 ".into()), ..Options::default() }.depth(), Some(12));
        for name in ["-ridge", "two words", "a..b", "ridge.lock"] {
            assert!(Options { branch: Some(name.into()), ..Options::default() }.problem().is_some(), "{name:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let request = Request {
            address: "/srv/git/trail.git".into(),
            parent: dir.path().into(),
            name: "trail".into(),
            options: Options { depth: Some("0".into()), ..Options::default() },
            account: None,
        };
        assert_eq!(request.problem().as_deref(), Some(DEPTH_PROBLEM));
        assert_eq!(clone(&request).unwrap_err(), format!("Clone Failed — {DEPTH_PROBLEM}"));
        assert!(!dir.path().join("trail").exists());
    }

    #[test]
    fn a_shallow_single_branch_clone_from_a_folder_downloads_only_what_was_asked() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let bare = bare(&f, dir.path());
        f.git(&["switch", "-q", "-c", "ridge"]);
        fs::write(f.dir.join("ridge.txt"), "Ridge path\n").unwrap();
        f.git(&["add", "ridge.txt"]);
        f.git(&["commit", "-qm", "Climb the ridge"]);
        f.git(&["push", "-q", bare.to_str().unwrap(), "ridge"]);
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let ask = |name: &str, options: Options| Request { address: bare.display().to_string(), parent: clones.clone(), name: name.into(), options, account: None };
        let root = clone(&ask("shallow", Options { depth: Some("1".into()), ..Options::default() })).unwrap();
        assert_eq!(git(&root, &["rev-parse", "--is-shallow-repository"]), "true\n");
        assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "1\n");
        assert_eq!(git(&root, &["for-each-ref", "--format=%(refname)", "refs/remotes"]), "refs/remotes/origin/HEAD\nrefs/remotes/origin/main\nrefs/remotes/origin/ridge\n");
        assert_eq!(fs::read_to_string(root.join("camp.txt")).unwrap(), "Camp at the ridge\n");
        let root = clone(&ask("single", Options { branch: Some("ridge".into()), ..Options::default() })).unwrap();
        assert_eq!(git(&root, &["symbolic-ref", "--short", "HEAD"]), "ridge\n");
        assert_eq!(git(&root, &["rev-parse", "--is-shallow-repository"]), "false\n");
        assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "3\n");
        assert_eq!(git(&root, &["for-each-ref", "--format=%(refname)", "refs/remotes"]), "refs/remotes/origin/ridge\n");
        assert_eq!(git(&root, &["config", "--get-all", "remote.origin.fetch"]), "+refs/heads/ridge:refs/remotes/origin/ridge\n");
        let missing = clone(&ask("missing", Options { branch: Some("valley".into()), ..Options::default() })).unwrap_err();
        assert!(missing.contains("valley"), "{missing}");
        assert!(!clones.join("missing").exists());
    }

    #[test]
    fn a_blobless_sparse_clone_starts_with_the_top_level_files_and_no_old_contents() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(f.dir.join("maps")).unwrap();
        fs::write(f.dir.join("maps/lake.txt"), "Lake map\n").unwrap();
        f.git(&["add", "maps/lake.txt"]);
        let bare = bare(&f, dir.path());
        git(&bare, &["config", "uploadpack.allowFilter", "true"]);
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let options = Options { blobless: true, sparse: true, ..Options::default() };
        let request = Request { address: bare.display().to_string(), parent: clones.clone(), name: "light".into(), options, account: None };
        let root = clone(&request).unwrap();
        assert_eq!(git(&root, &["config", "remote.origin.promisor"]), "true\n");
        assert_eq!(git(&root, &["config", "remote.origin.partialclonefilter"]), "blob:none\n");
        assert_eq!(git(&root, &["config", "core.sparseCheckout"]), "true\n");
        assert_eq!(git(&root, &["sparse-checkout", "list"]), "");
        assert!(root.join("trail.txt").exists() && root.join("camp.txt").exists());
        assert!(!root.join("maps").exists());
        let missing = git(&root, &["rev-list", "--objects", "--all", "--missing=print"]);
        assert!(missing.lines().any(|line| line.starts_with('?')), "{missing}");
        assert_eq!(git(&root, &["status", "--porcelain"]), "");
    }

    #[test]
    fn submodules_are_cloned_when_asked_and_a_local_folder_refusal_is_explained() {
        use crate::repository::tests::GlobalConfig;
        let library = Fixture::new();
        fs::write(library.dir.join("map.txt"), "Ridge map\n").unwrap();
        library.git(&["add", "map.txt"]);
        library.git(&["commit", "-qm", "Draw the ridge map"]);
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        f.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", library.dir.to_str().unwrap(), "maps"]);
        let bare = bare(&f, dir.path());
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let ask = |name: &str| Request {
            address: bare.display().to_string(),
            parent: clones.clone(),
            name: name.into(),
            options: Options { submodules: true, ..Options::default() },
            account: None,
        };
        let empty = tempfile::NamedTempFile::new().unwrap();
        let refused = {
            let _config = GlobalConfig::set(empty.path());
            clone(&ask("refused")).unwrap_err()
        };
        assert_eq!(refused, format!("Clone Failed — {}", crate::submodule::LOCAL_REFUSED));
        assert!(!clones.join("refused").exists());
        let mut allowed = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut allowed, b"[protocol \"file\"]\n\tallow = always\n").unwrap();
        let _config = GlobalConfig::set(allowed.path());
        let root = clone(&ask("nested")).unwrap();
        assert_eq!(fs::read_to_string(root.join("maps/map.txt")).unwrap(), "Ridge map\n");
        assert_eq!(git(&root.join("maps"), &["rev-parse", "HEAD"]), String::from_utf8(library.git(&["rev-parse", "HEAD"])).unwrap());
    }

    #[test]
    fn a_folder_that_is_there_and_not_empty_is_refused_before_git_runs() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let bare = bare(&f, dir.path());
        let clones = dir.path().join("clones");
        let taken = clones.join("Juniper-trail");
        fs::create_dir_all(&taken).unwrap();
        fs::write(taken.join("notes.txt"), "Keep me\n").unwrap();
        let request = Request { address: bare.display().to_string(), parent: clones.clone(), name: "Juniper-trail".into(), ..Request::default() };
        let problem = request.problem().unwrap();
        assert_eq!(problem, format!("{} already exists and is not empty.", taken.display()));
        let refused = clone(&request).unwrap_err();
        assert_eq!(refused, format!("Clone Failed — {problem}"));
        let left: Vec<_> = fs::read_dir(&taken).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(left, ["notes.txt"]);
        fs::write(clones.join("plain"), "a file\n").unwrap();
        let file = Request { name: "plain".into(), ..request.clone() };
        assert!(clone(&file).unwrap_err().ends_with("already exists and is not a folder."));
        assert_eq!(fs::read_to_string(clones.join("plain")).unwrap(), "a file\n");
    }

    #[test]
    fn a_folder_filled_after_the_dialog_checked_it_is_refused_by_the_worker() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let bare = bare(&f, dir.path());
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let request = Request { address: bare.display().to_string(), parent: clones.clone(), name: "late".into(), ..Request::default() };
        assert_eq!(request.problem(), None);
        fs::create_dir(clones.join("late")).unwrap();
        fs::write(clones.join("late/mine.txt"), "mine\n").unwrap();
        assert!(clone(&request).unwrap_err().ends_with("already exists and is not empty."));
        assert_eq!(fs::read_to_string(clones.join("late/mine.txt")).unwrap(), "mine\n");
        assert!(!clones.join("late/.git").exists());
    }

    #[test]
    fn a_failed_clone_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let request = Request { address: dir.path().join("nowhere.git").display().to_string(), parent: dir.path().into(), name: "nowhere".into(), ..Request::default() };
        let failure = clone(&request).unwrap_err();
        assert!(failure.starts_with("Clone Failed — fatal: repository '"), "{failure}");
        assert!(!failure.contains("Cloning into"), "{failure}");
        assert!(!dir.path().join("nowhere").exists());
        fs::create_dir(dir.path().join("kept")).unwrap();
        let kept = Request { name: "kept".into(), ..request };
        clone(&kept).unwrap_err();
        assert!(dir.path().join("kept").is_dir());
        assert_eq!(fs::read_dir(dir.path().join("kept")).unwrap().count(), 0);
    }

    #[test]
    fn a_clone_command_has_the_timeouts_and_hands_ssh_questions_to_the_window() {
        use crate::askpass::tests::Program;
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let bare = bare(&f, dir.path());
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let ssh = script(
            "ssh.sh",
            "answer=$(\"$SSH_ASKPASS\" \"Enter passphrase for key '/k/id_trail': \") || { echo 'Permission denied (publickey).' >&2; exit 255; }\n[ \"$answer\" = 'open sesame' ] || { echo 'Permission denied (publickey).' >&2; exit 255; }\nfor last; do :; done\nexec /bin/sh -c \"git ${last#git-}\"\n",
        );
        let program = script("askpass.sh", "[ -n \"$FASTKRAKEN_ASKPASS_TOKEN\" ] && [ \"$SSH_ASKPASS_REQUIRE\" = force ] || exit 1\necho 'open sesame'\n");
        let _program = Program::set(program.clone());
        let clones = dir.path().join("clones");
        fs::create_dir(&clones).unwrap();
        let command = repository::clone_command(&clones).unwrap();
        let args: Vec<_> = command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        for setting in ["http.lowSpeedLimit=1000", "http.lowSpeedTime=30"] {
            assert!(args.windows(2).any(|pair| pair == ["-c", setting]), "{args:?}");
        }
        assert!(args.iter().any(|arg| arg.starts_with("credential.helper=") && arg.contains(program.to_str().unwrap())), "{args:?}");
        let env = |command: &std::process::Command, key: &str| {
            command.get_envs().find(|(name, _)| *name == key).and_then(|(_, value)| value.map(OsStr::to_owned))
        };
        assert!(env(&command, "GIT_SSH_COMMAND").is_some_and(|ssh| ssh.to_string_lossy().contains("ConnectTimeout=15")));
        let address = format!("ssh://git@trail.invalid{}", bare.display());
        let run = |link: &jobs::Link, name: &str| {
            link.enter(|| {
                let mut command = repository::clone_command(&clones).unwrap();
                command.env("GIT_SSH_COMMAND", &ssh).args(["-c", "ssh.variant=ssh", "clone", "-q", "--"]).arg(&address).arg(name);
                repository::checked(&mut command)
            })
        };
        let refused = run(&jobs::Link::new(true), "background").unwrap_err();
        assert!(refused.contains("Permission denied (publickey)"), "{refused}");
        let interactive = jobs::Link::interactive();
        assert!(interactive.session().is_some());
        run(&interactive, "asked").unwrap();
        assert_eq!(git(&clones.join("asked"), &["log", "--format=%s"]), "Mark the camp\nStart the trail\n");
    }

    struct Silent {
        port: u16,
        stop: Arc<AtomicBool>,
    }

    impl Silent {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            std::thread::spawn(move || {
                let mut held = Vec::new();
                while !stopping.load(Ordering::SeqCst) {
                    if let Ok((stream, _)) = listener.accept() {
                        held.push(stream);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            Self { port, stop }
        }
    }

    impl Drop for Silent {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancelling_a_clone_removes_the_folder_it_made() {
        let server = Silent::start();
        let dir = tempfile::tempdir().unwrap();
        let request = Request {
            address: format!("http://127.0.0.1:{}/Hollow-creek.git", server.port),
            parent: dir.path().into(),
            name: "Hollow-creek".into(),
            ..Request::default()
        };
        let link = jobs::Link::new(true);
        let running = link.clone();
        let asked = request.clone();
        let worker = std::thread::spawn(move || running.enter(|| clone(&asked)));
        let start = Instant::now();
        while !dir.path().join("Hollow-creek/.git").exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "The clone never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(link.cancel());
        assert!(worker.join().unwrap().is_err());
        assert!(!dir.path().join("Hollow-creek").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
