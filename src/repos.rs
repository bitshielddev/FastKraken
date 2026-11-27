use crate::{
    Tab,
    commands::{Availability, Bar, Command, Group, Target, on_repo},
    kit,
    notice::Notice,
    prompt::{self, Answer, Field, Spec},
    repository::{Repository, checked},
    settings::{self, Memory},
    tabs,
    theme::*,
    view::{self, Desk, Tone, Ui},
};
use crate::theme::px;
use gpui::{App, Context, Div, EntityId, FontWeight, PathPromptOptions, Window, div, prelude::*, rgb};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const LIMIT: usize = 3;
pub(crate) const DEPTH: usize = 3;
const FOUND_LIMIT: usize = 400;
const VISITED_LIMIT: usize = 20_000;
const ROW: f32 = 34.;
const LIST_HEIGHT: f32 = 420.;
pub(crate) const HEADING: &str = "Repositories";

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "repo.manage",
        Group::Repository,
        |_, _| "Manage repositories…".into(),
        |_, target, _| {
            if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden }
        },
        |_, _| Some(view::Action::Repos(Action::Open)),
    )
    .words(&["repository manager", "favourites", "favorites", "projects folder", "fetch several", "pull several", "all repositories"])
    .icon("list")
    .keys("alt-cmd-o")
    .bar(Bar::File),
    Command::new(
        "repo.favourite",
        Group::Manage,
        |_, _| "Add to favourites".into(),
        |ui, target, cx| starred_when(ui, target, cx, false),
        |ui, _| Some(view::Action::Repos(Action::Favourite(ui.root.clone(), true))),
    )
    .words(&["star", "favorite", "favourite repository"])
    .icon("star"),
    Command::new(
        "repo.unfavourite",
        Group::Manage,
        |_, _| "Remove from favourites".into(),
        |ui, target, cx| starred_when(ui, target, cx, true),
        |ui, _| Some(view::Action::Repos(Action::Favourite(ui.root.clone(), false))),
    )
    .words(&["unstar", "favorite", "favourite repository"])
    .icon("star-filled"),
    Command::new(
        "repo.display-name",
        Group::Manage,
        |_, _| "Set display name…".into(),
        |ui, target, _| {
            if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
        },
        |ui, _| Some(view::Action::Repos(Action::Rename(ui.root.clone()))),
    )
    .words(&["alias", "rename repository", "nickname", "repository name"])
    .icon("pencil"),
];

fn starred_when(ui: &Ui, target: &Target, cx: &App, starred: bool) -> Availability {
    if !on_repo(ui, target) {
        return Availability::Hidden;
    }
    let now = settings::memory(cx).is_some_and(|memory| memory.is_favourite(&ui.root));
    if now == starred { Availability::Enabled } else { Availability::Hidden }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Fetch,
    Pull,
}

impl Kind {
    fn doing(self) -> &'static str {
        match self {
            Self::Fetch => "Fetching",
            Self::Pull => "Pulling",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Self::Fetch => "Fetched",
            Self::Pull => "Pulled",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Pull => "pull",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open,
    Close,
    Favourite(PathBuf, bool),
    Pick(PathBuf),
    PickAll,
    PickNone,
    ChooseProjects,
    UseProjects(PathBuf),
    ForgetProjects,
    Rescan,
    Run(Kind),
    Stop,
    Rename(PathBuf),
    OpenRepository(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Waiting,
    Running,
    Done(Result<String, String>),
    Skipped,
}

pub(crate) struct Batch {
    pub(crate) kind: Kind,
    pub(crate) rows: Vec<(PathBuf, Step)>,
    pub(crate) running: usize,
    pub(crate) peak: usize,
    ticket: u64,
    stopped: bool,
}

impl Batch {
    fn new(kind: Kind, paths: Vec<PathBuf>, ticket: u64) -> Self {
        Self { kind, rows: paths.into_iter().map(|path| (path, Step::Waiting)).collect(), running: 0, peak: 0, ticket, stopped: false }
    }

    fn take_next(&mut self) -> Option<PathBuf> {
        if self.stopped || self.running >= LIMIT {
            return None;
        }
        let (path, step) = self.rows.iter_mut().find(|(_, step)| *step == Step::Waiting)?;
        *step = Step::Running;
        self.running += 1;
        self.peak = self.peak.max(self.running);
        Some(path.clone())
    }

    fn finish(&mut self, path: &Path, result: Result<String, String>) {
        if let Some((_, step)) = self.rows.iter_mut().find(|(row, step)| row == path && *step == Step::Running) {
            *step = Step::Done(result);
            self.running = self.running.saturating_sub(1);
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
        for (_, step) in &mut self.rows {
            if *step == Step::Waiting {
                *step = Step::Skipped;
            }
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.running == 0 && !self.rows.iter().any(|(_, step)| *step == Step::Waiting)
    }

    pub(crate) fn step(&self, path: &Path) -> Option<&Step> {
        self.rows.iter().find(|(row, _)| row == path).map(|(_, step)| step)
    }

    fn counts(&self) -> (usize, usize, usize) {
        let ok = self.rows.iter().filter(|(_, step)| matches!(step, Step::Done(Ok(_)))).count();
        let failed = self.rows.iter().filter(|(_, step)| matches!(step, Step::Done(Err(_)))).count();
        let skipped = self.rows.iter().filter(|(_, step)| *step == Step::Skipped).count();
        (ok, failed, skipped)
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) open: bool,
    picked: BTreeSet<PathBuf>,
    found: Vec<PathBuf>,
    scanned: Option<PathBuf>,
    scanning: Option<u64>,
    tickets: u64,
    batch: Option<Batch>,
}

impl State {
    pub(crate) fn picked(&self) -> &BTreeSet<PathBuf> {
        &self.picked
    }

    pub(crate) fn found(&self) -> &[PathBuf] {
        &self.found
    }

    pub(crate) fn scanning(&self) -> bool {
        self.scanning.is_some()
    }

    pub(crate) fn batch(&self) -> Option<&Batch> {
        self.batch.as_ref()
    }

    pub(crate) fn running(&self) -> bool {
        self.batch.as_ref().is_some_and(|batch| !batch.finished())
    }

    fn ticket(&mut self) -> u64 {
        self.tickets += 1;
        self.tickets
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Favourites,
    Recent,
    Projects,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Listed {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) parent: String,
    pub(crate) section: Section,
    pub(crate) favourite: bool,
    pub(crate) missing: bool,
    pub(crate) open: bool,
}

pub(crate) fn folder_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

pub(crate) fn listed(memory: &Memory, found: &[PathBuf], open: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Vec<Listed> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    let candidates = memory
        .favourites()
        .into_iter()
        .map(|path| (path, Section::Favourites))
        .chain(memory.recents.iter().map(|recent| (recent.path(), Section::Recent)))
        .chain(found.iter().map(|path| (path.clone(), Section::Projects)));
    for (path, section) in candidates {
        if !seen.insert(path.clone()) {
            continue;
        }
        let name = memory.name_of(&path).map_or_else(|| folder_name(&path), str::to_owned);
        let parent = path.parent().map(|parent| crate::start::homely(parent, home.as_deref())).unwrap_or_default();
        rows.push(Listed {
            favourite: memory.is_favourite(&path),
            missing: !exists(&path),
            open: open.contains(&path),
            path,
            name,
            parent,
            section,
        });
    }
    rows
}

pub(crate) fn rows(ui: &Ui, cx: &App) -> Vec<Listed> {
    let Some(memory) = settings::memory(cx) else { return Vec::new() };
    listed(memory, &ui.repos.found, &tabs::open_roots(cx), Path::exists)
}

pub(crate) fn display_name(root: &Path, cx: &App) -> Option<String> {
    settings::memory(cx).and_then(|memory| memory.name_of(root)).map(str::to_owned)
}

pub(crate) fn shown(ui: &Ui, cx: &App) -> String {
    if ui.repo.is_empty() {
        return ui.repo.clone();
    }
    display_name(&ui.root, cx).unwrap_or_else(|| ui.repo.clone())
}

fn is_repository(dir: &Path) -> bool {
    let git = dir.join(".git");
    git.is_file() || (git.is_dir() && git.join("HEAD").is_file())
}

pub(crate) fn scan(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if is_repository(folder) {
        found.push(folder.to_owned());
        return found;
    }
    let mut visited = 0;
    walk(folder, 1, &mut found, &mut visited);
    found
}

fn walk(folder: &Path, depth: usize, found: &mut Vec<PathBuf>, visited: &mut usize) {
    let Ok(entries) = fs::read_dir(folder) else { return };
    let mut folders: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    for inner in folders {
        if found.len() >= FOUND_LIMIT || *visited >= VISITED_LIMIT {
            return;
        }
        *visited += 1;
        if is_repository(&inner) {
            found.push(inner);
        } else if depth < DEPTH {
            walk(&inner, depth + 1, found, visited);
        }
    }
}

pub(crate) fn own_root(repo: &Repository, path: &Path) -> Result<(), String> {
    let wanted = fs::canonicalize(path).map_err(|_| "The folder is no longer there.".to_owned())?;
    let root = fs::canonicalize(&repo.root).unwrap_or_else(|_| repo.root.clone());
    if root != wanted {
        return Err(format!("The folder is now inside {}, not a repository of its own.", repo.root.display()));
    }
    Ok(())
}

fn opened(path: &Path) -> Result<Repository, String> {
    if !path.is_dir() {
        return Err("The folder is no longer there.".into());
    }
    let repo = Repository::open(path).map_err(|error| format!("It is not a Git repository FastKraken can open: {error}"))?;
    own_root(&repo, path)?;
    Ok(repo)
}

pub(crate) fn fetch_repository(repo: &Repository, prune: bool) -> Result<String, String> {
    crate::sync::fetch_every(repo, prune)
}

pub(crate) fn checked_out(repo: &Repository) -> Result<(String, String), String> {
    let attached = checked(repo.read_command().args(["symbolic-ref", "--quiet", "HEAD"]))
        .map_err(|_| "HEAD is detached, so there is no branch to pull into.".to_owned())?;
    let attached = String::from_utf8_lossy(&attached).trim_end().to_owned();
    let branch = attached
        .strip_prefix("refs/heads/")
        .filter(|name| !name.is_empty())
        .ok_or("HEAD is not on a local branch, so there is nothing to pull into.")?
        .to_owned();
    let head = checked(repo.read_command().args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]))
        .map_err(|_| format!("{branch} has no commits yet, so there is nothing to pull into."))?;
    Ok((branch, String::from_utf8_lossy(&head).trim_end().to_owned()))
}

pub(crate) fn pull_from(repo: &Repository, branch: &str, head: &str) -> Result<String, String> {
    crate::sync::fast_forward_checked_out(repo, branch, head)
}

pub(crate) fn pull_repository(repo: &Repository) -> Result<String, String> {
    let (branch, head) = checked_out(repo)?;
    pull_from(repo, &branch, &head)
}

fn work(kind: Kind, path: &Path, prune: bool) -> Result<String, String> {
    let repo = opened(path)?;
    match kind {
        Kind::Fetch => fetch_repository(&repo, prune),
        Kind::Pull => pull_repository(&repo),
    }
}

fn names(paths: &[PathBuf], cx: &App) -> Vec<String> {
    paths.iter().map(|path| display_name(path, cx).unwrap_or_else(|| folder_name(path))).collect()
}

fn joined(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn repositories(count: usize) -> String {
    match count {
        1 => "1 repository".to_owned(),
        n => format!("{n} repositories"),
    }
}

pub(crate) fn summary(batch: &Batch, named: impl Fn(&Path) -> String) -> Notice {
    let (ok, failed, skipped) = batch.counts();
    let total = batch.rows.len();
    let kind = batch.kind;
    let lines: Vec<String> = batch
        .rows
        .iter()
        .map(|(path, step)| {
            let said = match step {
                Step::Done(Ok(detail)) if detail.trim().is_empty() => kind.done().to_owned(),
                Step::Done(Ok(detail)) => detail.trim().to_owned(),
                Step::Done(Err(error)) => format!("Failed — {}", error.trim()),
                Step::Skipped => "Not started".to_owned(),
                Step::Waiting | Step::Running => "Still running".to_owned(),
            };
            format!("{}: {said}", named(path))
        })
        .collect();
    let succeeded: Vec<String> = batch
        .rows
        .iter()
        .filter(|(_, step)| matches!(step, Step::Done(Ok(_))))
        .map(|(path, _)| named(path))
        .collect();
    let mut notice = if failed == 0 && skipped == 0 {
        Notice::new(Tone::Success, format!("{} {}", kind.done(), repositories(ok)), format!("{} {}.", kind.done(), joined(&succeeded)))
    } else if failed == 0 {
        let were = if skipped == 1 { "was" } else { "were" };
        Notice::new(
            Tone::Info,
            format!("{} {ok} of {total} repositories", kind.done()),
            format!("Stopped before the other {} {were} started.", repositories(skipped)),
        )
    } else {
        let first = batch.rows.iter().find_map(|(path, step)| match step {
            Step::Done(Err(error)) => Some(format!("{}: {}", named(path), error.trim())),
            _ => None,
        });
        let title = match ok {
            0 if skipped == 0 => format!("Could not {} {}", kind.verb(), repositories(failed)),
            _ => format!("{} {ok} of {total} repositories", kind.done()),
        };
        let mut detail = first.unwrap_or_default();
        if failed > 1 {
            detail.push_str(&format!(" {} more could not be {}.", failed - 1, kind.done().to_lowercase()));
        }
        Notice::new(Tone::Error, title, detail).sticky()
    };
    notice.output = Some(lines.join("\n"));
    notice
}

#[derive(Debug)]
pub(crate) enum Ask {
    Tab { id: EntityId, current: String },
    Display { path: PathBuf, current: String },
}

fn length(text: &str, _: &Ui) -> Option<String> {
    (text.trim().chars().count() > settings::NAME_LENGTH).then(|| format!("Use at most {} characters.", settings::NAME_LENGTH))
}

pub(crate) fn describe(ask: &Ask, _: &Ui) -> Spec {
    match ask {
        Ask::Tab { current, .. } => {
            let mut spec = Spec::new("Rename tab", "Rename")
                .line("The name shows on the tab and in the Window menu. Leave it empty to show the repository's own name again.");
            spec.field = Some(Field {
                label: "Tab name",
                placeholder: "Name for this tab",
                initial: current.clone(),
                masked: false,
                validate: Box::new(length),
            });
            spec
        }
        Ask::Display { path, current } => {
            let mut spec = Spec::new(format!("Display name for {}", folder_name(path)), "Save")
                .line("FastKraken shows this name in tabs, the window title and its lists of repositories. The folder keeps its name on disk. Leave it empty to use the folder's name.");
            spec.field = Some(Field {
                label: "Display name",
                placeholder: "Name to show",
                initial: current.clone(),
                masked: false,
                validate: Box::new(length),
            });
            spec
        }
    }
}

pub(crate) fn confirm(tab: &mut Tab, ask: Ask, answer: Answer, cx: &mut Context<Tab>) {
    match ask {
        Ask::Tab { id, .. } => {
            let name = settings::tidy_name(&answer.text);
            if !tabs::ask(cx, tabs::Request::Name(id, name.clone())) {
                tab.name = name;
            }
        }
        Ask::Display { path, .. } => {
            let name = settings::tidy_name(&answer.text);
            Memory::update(cx, |memory| memory.set_name(&path, name.as_deref()));
            tab.ui.title_shown = None;
            tab.ui.bar_shown.clear();
            let folder = folder_name(&path);
            let notice = match name {
                Some(name) => Notice::new(Tone::Success, format!("{folder} is shown as {name}"), "The folder keeps its name on disk."),
                None => Notice::new(Tone::Success, format!("{folder} is shown by its folder name"), ""),
            };
            tab.ui.notices.show(notice);
        }
    }
    cx.notify();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open if tab.repo.is_some() || tab.ui.opening.is_some() => {
            if !tabs::ask(cx, tabs::Request::Manage) {
                tab.refuse("Close this repository first to manage repositories".to_owned(), cx);
            }
        }
        Action::Open => {
            tab.ui.repos.open = true;
            rescan_if_needed(tab, cx);
        }
        Action::Close => tab.ui.repos.open = false,
        Action::Favourite(path, favourite) => {
            let mut kept = true;
            Memory::update(cx, |memory| kept = memory.set_favourite(&path, favourite));
            if !kept {
                tab.refuse(format!("FastKraken keeps at most {} favourites. Remove one first.", settings::FAVOURITES), cx);
            } else if tab.repo.is_some() {
                let name = shown(&tab.ui, cx);
                let title = if favourite { format!("{name} is a favourite") } else { format!("{name} is no longer a favourite") };
                tab.ui.notices.show(Notice::new(Tone::Success, title, ""));
            }
        }
        Action::Pick(path) => {
            if !tab.ui.repos.picked.remove(&path) {
                tab.ui.repos.picked.insert(path);
            }
        }
        Action::PickAll => {
            let all: BTreeSet<PathBuf> = rows(&tab.ui, cx).into_iter().filter(|row| !row.missing).map(|row| row.path).collect();
            tab.ui.repos.picked = all;
        }
        Action::PickNone => tab.ui.repos.picked.clear(),
        Action::ChooseProjects => choose_projects(window, cx),
        Action::UseProjects(folder) => {
            let folder = fs::canonicalize(&folder).unwrap_or(folder);
            if !folder.is_dir() {
                tab.refuse(format!("{} is not a folder", folder.display()), cx);
                return;
            }
            Memory::update(cx, |memory| memory.set_projects(Some(&folder)));
            start_scan(tab, folder, cx);
        }
        Action::ForgetProjects => {
            Memory::update(cx, |memory| memory.set_projects(None));
            let state = &mut tab.ui.repos;
            state.scanning = None;
            state.scanned = None;
            let found: BTreeSet<PathBuf> = std::mem::take(&mut state.found).into_iter().collect();
            state.picked.retain(|path| !found.contains(path));
        }
        Action::Rescan => {
            if let Some(folder) = settings::memory(cx).and_then(Memory::projects) {
                start_scan(tab, folder, cx);
            }
        }
        Action::Run(kind) => run(tab, kind, cx),
        Action::Stop => {
            if let Some(batch) = tab.ui.repos.batch.as_mut().filter(|batch| !batch.finished()) {
                batch.stop();
                if batch.finished() {
                    announce(tab, cx);
                }
            }
        }
        Action::Rename(path) => {
            let current = settings::memory(cx).and_then(|memory| memory.name_of(&path)).unwrap_or_default().to_owned();
            prompt::ask(&mut tab.ui, prompt::Kind::Repos(Ask::Display { path, current }), cx);
        }
        Action::OpenRepository(path) => {
            if tab.repo.is_none() && tab.ui.opening.is_none() {
                tab.open(path, cx);
            } else {
                tabs::open_elsewhere(path, |_, _| {}, cx);
            }
        }
    }
    cx.notify();
}

pub(crate) fn rename_tab(tab: &mut Tab, id: EntityId, current: Option<String>, cx: &mut Context<Tab>) {
    let current = current.unwrap_or_default();
    prompt::ask(&mut tab.ui, prompt::Kind::Repos(Ask::Tab { id, current }), cx);
}

fn choose_projects(window: &mut Window, cx: &mut Context<Tab>) {
    let picker = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Use as projects folder".into()),
    });
    let tab = cx.weak_entity();
    window
        .spawn(cx, async move |cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                if let Some(folder) = paths.into_iter().next() {
                    let _ = cx.update(|window, cx| {
                        tab.update(cx, |tab, cx| dispatch(tab, Action::UseProjects(folder), window, cx))
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = tab.update(cx, |tab, cx| tab.error(error.to_string(), cx));
            }
            _ => {}
        })
        .detach();
}

fn rescan_if_needed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(folder) = settings::memory(cx).and_then(Memory::projects) else { return };
    if tab.ui.repos.scanned.as_ref() != Some(&folder) && tab.ui.repos.scanning.is_none() {
        start_scan(tab, folder, cx);
    }
}

fn start_scan(tab: &mut Tab, folder: PathBuf, cx: &mut Context<Tab>) {
    let ticket = tab.ui.repos.ticket();
    tab.ui.repos.scanning = Some(ticket);
    let looked = folder.clone();
    Tab::work(
        cx,
        move || scan(&looked),
        move |tab, found, cx| {
            if tab.ui.repos.scanning != Some(ticket) {
                return;
            }
            let state = &mut tab.ui.repos;
            state.scanning = None;
            state.scanned = Some(folder);
            state.found = found;
            cx.notify();
        },
    );
}

fn run(tab: &mut Tab, kind: Kind, cx: &mut Context<Tab>) {
    if tab.ui.repos.running() {
        tab.refuse("Wait for the repositories being fetched or pulled to finish, or stop them".to_owned(), cx);
        return;
    }
    let listed: Vec<PathBuf> = rows(&tab.ui, cx).into_iter().filter(|row| !row.missing).map(|row| row.path).collect();
    let paths: Vec<PathBuf> = listed.into_iter().filter(|path| tab.ui.repos.picked.contains(path)).collect();
    if paths.is_empty() {
        tab.refuse("Tick the repositories to fetch or pull first".to_owned(), cx);
        return;
    }
    let ticket = tab.ui.repos.ticket();
    tab.ui.repos.batch = Some(Batch::new(kind, paths, ticket));
    for _ in 0..LIMIT {
        next(tab, cx);
    }
}

fn next(tab: &mut Tab, cx: &mut Context<Tab>) {
    let prune = settings::settings(cx).prune_on_fetch;
    let Some(batch) = tab.ui.repos.batch.as_mut() else { return };
    let (kind, ticket) = (batch.kind, batch.ticket);
    let Some(path) = batch.take_next() else { return };
    let working = path.clone();
    Tab::work(
        cx,
        move || work(kind, &working, prune),
        move |tab, result, cx| {
            let Some(batch) = tab.ui.repos.batch.as_mut().filter(|batch| batch.ticket == ticket) else { return };
            batch.finish(&path, result);
            if batch.finished() {
                announce(tab, cx);
            } else {
                next(tab, cx);
            }
            cx.notify();
        },
    );
}

fn announce(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(batch) = tab.ui.repos.batch.as_ref() else { return };
    let paths: Vec<PathBuf> = batch.rows.iter().map(|(path, _)| path.clone()).collect();
    let shown = names(&paths, cx);
    let notice = summary(batch, |path| {
        paths.iter().position(|row| row == path).and_then(|at| shown.get(at).cloned()).unwrap_or_else(|| folder_name(path))
    });
    tab.ui.notices.show(notice);
}

fn step_text(kind: Kind, step: &Step) -> (String, u32, Option<String>) {
    match step {
        Step::Waiting => ("Waiting".to_owned(), palette().text_3, None),
        Step::Running => (format!("{}…", kind.doing()), palette().accent, None),
        Step::Done(Ok(detail)) => (kind.done().to_owned(), palette().added, Some(detail.clone())),
        Step::Done(Err(error)) => ("Failed".to_owned(), palette().deleted, Some(error.clone())),
        Step::Skipped => ("Not started".to_owned(), palette().text_3, None),
    }
}

fn heading(section: &Section, projects: Option<&Path>) -> String {
    match section {
        Section::Favourites => "Favourites".to_owned(),
        Section::Recent => "Recent".to_owned(),
        Section::Projects => match projects {
            Some(folder) => format!("In {}", folder_name(folder)),
            None => "In the projects folder".to_owned(),
        },
    }
}

pub(crate) fn screen<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.repos;
    let projects = settings::memory(cx).and_then(Memory::projects);
    let listed = rows(ui, cx);
    let picked = listed.iter().filter(|row| !row.missing && state.picked.contains(&row.path)).count();
    let running = state.running();
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child(HEADING))
        .child(kit::grow())
        .child(kit::btn("repos-back", "Back").ghost().icon("x").hint("Esc").on(view::Action::Repos(Action::Close), cx));
    let folder_row = match &projects {
        Some(folder) => {
            let found = if state.scanning() {
                "Looking for repositories…".to_owned()
            } else {
                format!("{} found", repositories(state.found.len()))
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(kit::icon("folder").text_color(rgb(palette().accent)))
                .child(div().flex_shrink_0().max_w(px(300.)).min_w_0().flex().child(kit::fit(folder.display().to_string()).cut(kit::Cut::Start)))
                .child(
                    div()
                        .id("repos-found")
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .tooltip(kit::tip(format!("FastKraken looks up to {DEPTH} folders deep and skips hidden folders.")))
                        .child(kit::fit(found).size(TEXT_SMALL).color(palette().text_3).grow()),
                )
                .child(kit::btn("repos-rescan", "Look again").ghost().small().enabled(!state.scanning()).on(view::Action::Repos(Action::Rescan), cx))
                .child(kit::btn("repos-projects", "Change…").ghost().small().on(view::Action::Repos(Action::ChooseProjects), cx))
                .child(
                    kit::btn("repos-forget", "Forget")
                        .ghost()
                        .small()
                        .tip("Stop listing the repositories in this folder. Nothing on disk changes.")
                        .on(view::Action::Repos(Action::ForgetProjects), cx),
                )
        }
        None => div()
            .flex()
            .items_center()
            .gap_2()
            .child(kit::muted("Choose a projects folder to list every repository in it, up to three folders deep."))
            .child(kit::grow())
            .child(
                kit::btn("repos-projects", "Choose projects folder…")
                    .icon("folder")
                    .small()
                    .on(view::Action::Repos(Action::ChooseProjects), cx),
            ),
    };
    let progress = state.batch.as_ref().filter(|batch| !batch.finished()).map(|batch| {
        let done = batch.rows.iter().filter(|(_, step)| matches!(step, Step::Done(_))).count();
        format!("{} {} of {}…", batch.kind.doing(), done + batch.running.min(1), batch.rows.len())
    });
    let actions = div()
        .flex()
        .items_center()
        .gap_2()
        .child(kit::btn("repos-pick-all", "Select all").ghost().small().on(view::Action::Repos(Action::PickAll), cx))
        .child(
            kit::btn("repos-pick-none", "Select none")
                .ghost()
                .small()
                .enabled(!state.picked.is_empty())
                .on(view::Action::Repos(Action::PickNone), cx),
        )
        .child(kit::grow())
        .children(progress.map(|text| kit::muted(text)))
        .when(running, |d| d.child(kit::btn("repos-stop", "Stop").small().on(view::Action::Repos(Action::Stop), cx)))
        .child(
            kit::btn("repos-fetch", format!("Fetch {}", repositories(picked)))
                .icon("fetch")
                .small()
                .enabled(picked > 0 && !running)
                .tip(format!("Fetch from every remote, at most {LIMIT} repositories at a time."))
                .on(view::Action::Repos(Action::Run(Kind::Fetch)), cx),
        )
        .child(
            kit::btn("repos-pull", format!("Pull {}", repositories(picked)))
                .icon("arrow-down")
                .small()
                .enabled(picked > 0 && !running)
                .tip("Fast-forward each checked-out branch from its upstream. A branch with commits of its own is left for you to merge or rebase.")
                .on(view::Action::Repos(Action::Run(Kind::Pull)), cx),
        );
    let mut list = div().id("repos-list").max_h(px(LIST_HEIGHT)).overflow_y_scroll().py_1().flex().flex_col();
    if listed.is_empty() {
        list = list.child(
            div()
                .h(px(ROW * 2.))
                .px_3()
                .flex()
                .items_center()
                .child(kit::muted("Repositories you open, star or keep in your projects folder are listed here.")),
        );
    }
    let mut section = None;
    for (index, row) in listed.into_iter().enumerate() {
        if section.as_ref() != Some(&row.section) {
            list = list.child(div().px_3().pt_2().pb_1().child(kit::label(heading(&row.section, projects.as_deref()))));
            section = Some(row.section.clone());
        }
        let step = state.batch.as_ref().and_then(|batch| batch.step(&row.path).map(|step| (batch.kind, step.clone())));
        let ticked = state.picked.contains(&row.path);
        list = list.child(listed_row(row, index, step, ticked, cx));
    }
    div().flex_1().min_h_0().flex().flex_col().items_center().pt(px(56.)).child(
        div()
            .w(px(760.))
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(div().text_color(rgb(palette().text_2)).child(
                "Star the repositories you use most, find the ones in a projects folder, and fetch or pull several at once.",
            ))
            .child(folder_row)
            .child(actions)
            .child(
                div()
                    .rounded(px(RADIUS))
                    .border_1()
                    .border_color(rgb(palette().line))
                    .bg(rgb(palette().panel))
                    .overflow_hidden()
                    .child(list),
            ),
    )
}

fn listed_row<T: Desk>(row: Listed, index: usize, step: Option<(Kind, Step)>, picked: bool, cx: &Context<T>) -> gpui::Stateful<Div> {
    let (status, ink, detail) = match &step {
        Some((kind, step)) => step_text(*kind, step),
        None if row.missing => ("No longer there".to_owned(), palette().text_3, None),
        None if row.open => ("Open in a tab".to_owned(), palette().text_3, None),
        None => (String::new(), palette().text_3, None),
    };
    let tip = match detail.filter(|detail| !detail.trim().is_empty()) {
        Some(detail) => format!("{}\n{}", row.path.display(), detail.trim()),
        None => row.path.display().to_string(),
    };
    let path = row.path.clone();
    let pick = row.path.clone();
    let starring = view::Action::Repos(Action::Favourite(row.path.clone(), !row.favourite));
    kit::row(("repos-row", index), false)
        .h(px(ROW))
        .px_2()
        .gap_2()
        .when(row.missing, |d| d.opacity(0.45))
        .tooltip(kit::tip(tip))
        .child(
            div()
                .id(("repos-pick", index))
                .size(px(TARGET))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(kit::checkbox(picked))
                .on_click(cx.listener(move |app, _, w, cx| {
                    cx.stop_propagation();
                    app.dispatch(view::Action::Repos(Action::Pick(pick.clone())), w, cx)
                })),
        )
        .child(star(("repos-star", index), row.favourite, starring, cx))
        .child(div().flex_shrink_0().max_w(px(240.)).min_w_0().flex().child(kit::fit(row.name).weight(FontWeight::MEDIUM)))
        .child(kit::fit(row.parent).size(TEXT_SMALL).color(palette().text_3).cut(kit::Cut::Start).grow())
        .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).text_color(rgb(ink)).child(status))
        .child(kit::icon_btn(
            ("repos-rename", index),
            "pencil",
            "Set display name…",
            view::Action::Repos(Action::Rename(row.path.clone())),
            cx,
        ))
        .on_click(cx.listener(move |app, _, w, cx| app.dispatch(view::Action::Repos(Action::OpenRepository(path.clone())), w, cx)))
}

pub(crate) fn star<T: Desk>(id: impl Into<gpui::ElementId>, favourite: bool, action: view::Action, cx: &Context<T>) -> gpui::Stateful<Div> {
    let (name, tip, ink) = match favourite {
        true => ("star-filled", "Remove from favourites", palette().accent),
        false => ("star", "Add to favourites", palette().text_3),
    };
    div()
        .id(id)
        .size(px(TARGET))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(palette().hover)))
        .tooltip(kit::tip(tip))
        .child(kit::icon(name).text_color(rgb(ink)))
        .on_click(cx.listener(move |app, _, w, cx| {
            cx.stop_propagation();
            app.dispatch(action.clone(), w, cx)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    fn text(f: &Fixture, dir: &Path, args: &[&str]) -> String {
        let bytes = checked(f.repo.command().current_dir(dir).args(args)).unwrap();
        String::from_utf8(bytes).unwrap().trim_end().to_owned()
    }

    struct Remote {
        local: Fixture,
        _folder: tempfile::TempDir,
        peer: PathBuf,
    }

    impl Remote {
        fn new() -> Self {
            let local = Fixture::new();
            fs::write(local.dir.join("trail.txt"), "start\n").unwrap();
            local.git(&["add", "trail.txt"]);
            local.git(&["commit", "-qm", "Start the trail"]);
            let folder = tempfile::tempdir().unwrap();
            let bare = folder.path().join("origin.git");
            let peer = folder.path().join("peer");
            local.git(&["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()]);
            local.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
            local.git(&["push", "-qu", "origin", "main"]);
            local.git(&["clone", "-q", bare.to_str().unwrap(), peer.to_str().unwrap()]);
            Self { local, _folder: folder, peer }
        }

        fn peer_commit(&self, subject: &str) -> String {
            let f = &self.local;
            fs::write(self.peer.join("trail.txt"), format!("{subject}\n")).unwrap();
            text(f, &self.peer, &["-c", "user.name=Peer", "-c", "user.email=peer@example.invalid", "commit", "-qam", subject]);
            text(f, &self.peer, &["push", "-q", "origin", "main"]);
            text(f, &self.peer, &["rev-parse", "HEAD"])
        }

        fn at(&self, args: &[&str]) -> String {
            text(&self.local, &self.local.dir, args)
        }
    }

    #[test]
    fn a_fetch_updates_only_the_remote_tracking_branch() {
        let remote = Remote::new();
        let head = remote.at(&["rev-parse", "HEAD"]);
        let incoming = remote.peer_commit("Peer work");
        fs::write(remote.local.dir.join("draft.txt"), "draft\n").unwrap();
        fetch_repository(&remote.local.repo, false).unwrap();
        assert_eq!(remote.at(&["rev-parse", "refs/remotes/origin/main"]), incoming);
        assert_eq!(remote.at(&["rev-parse", "HEAD"]), head);
        assert_eq!(remote.at(&["status", "--porcelain=v1"]), "?? draft.txt");
        assert_eq!(fs::read_to_string(remote.local.dir.join("trail.txt")).unwrap(), "start\n");
    }

    #[test]
    fn a_pull_fast_forwards_the_checked_out_branch_and_keeps_other_changes() {
        let remote = Remote::new();
        let incoming = remote.peer_commit("Peer work");
        fs::write(remote.local.dir.join("draft.txt"), "draft\n").unwrap();
        let said = pull_repository(&remote.local.repo).unwrap();
        assert!(said.contains("main"), "{said}");
        assert_eq!(remote.at(&["rev-parse", "HEAD"]), incoming);
        assert_eq!(remote.at(&["rev-parse", "refs/remotes/origin/main"]), incoming);
        assert_eq!(fs::read_to_string(remote.local.dir.join("trail.txt")).unwrap(), "Peer work\n");
        assert_eq!(remote.at(&["status", "--porcelain=v1"]), "?? draft.txt");
        assert_eq!(remote.at(&["diff", "--cached", "--name-only"]), "");
    }

    #[test]
    fn a_pull_leaves_a_branch_with_its_own_commits_alone() {
        let remote = Remote::new();
        remote.peer_commit("Peer work");
        fs::write(remote.local.dir.join("mine.txt"), "mine\n").unwrap();
        remote.local.git(&["add", "mine.txt"]);
        remote.local.git(&["commit", "-qm", "My own work"]);
        let mine = remote.at(&["rev-parse", "HEAD"]);
        let refused = pull_repository(&remote.local.repo).unwrap_err();
        assert!(refused.contains("has 1 commit that origin/main does not"), "{refused}");
        assert_eq!(remote.at(&["rev-parse", "HEAD"]), mine);
        assert_eq!(remote.at(&["status", "--porcelain=v1"]), "");
        assert_eq!(remote.at(&["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn a_pull_refuses_a_branch_that_moved_after_it_was_read() {
        let remote = Remote::new();
        let incoming = remote.peer_commit("Peer work");
        let (branch, head) = checked_out(&remote.local.repo).unwrap();
        assert_eq!(branch, "main");
        remote.local.git(&["commit", "--allow-empty", "-qm", "Moved meanwhile"]);
        let moved = remote.at(&["rev-parse", "HEAD"]);
        let refused = pull_from(&remote.local.repo, &branch, &head).unwrap_err();
        assert!(refused.contains("changed"), "{refused}");
        assert_eq!(remote.at(&["rev-parse", "HEAD"]), moved);
        assert_ne!(moved, incoming);
        remote.local.git(&["switch", "-q", "--detach"]);
        assert!(pull_repository(&remote.local.repo).unwrap_err().contains("detached"));
        assert_eq!(remote.at(&["rev-parse", "HEAD"]), moved);
    }

    #[test]
    fn a_pull_without_an_upstream_says_so_and_changes_nothing() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Alone"]);
        let head = text(&f, &f.dir, &["rev-parse", "HEAD"]);
        let refused = pull_repository(&f.repo).unwrap_err();
        assert!(refused.contains("upstream"), "{refused}");
        assert_eq!(text(&f, &f.dir, &["rev-parse", "HEAD"]), head);
        let empty = Fixture::new();
        assert!(pull_repository(&empty.repo).unwrap_err().contains("no commits"));
    }

    #[test]
    fn a_folder_inside_another_repository_is_not_its_own_root() {
        let f = Fixture::new();
        f.git(&["commit", "--allow-empty", "-qm", "Start"]);
        let inner = f.dir.join("docs");
        fs::create_dir(&inner).unwrap();
        let opened = f.open(&inner);
        assert!(own_root(&opened, &inner).unwrap_err().contains("inside"));
        own_root(&f.repo, &f.dir).unwrap();
        assert!(own_root(&f.repo, &f.dir.join("gone")).unwrap_err().contains("no longer there"));
    }

    #[test]
    fn the_projects_folder_is_searched_three_folders_deep() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let repository = |path: &Path| {
            fs::create_dir_all(path.join(".git")).unwrap();
            fs::write(path.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        };
        repository(&root.join("Alder"));
        repository(&root.join("team/Birch"));
        repository(&root.join("team/apps/Hazel"));
        repository(&root.join("a/b/c/Deep"));
        repository(&root.join(".hidden/Secret"));
        repository(&root.join("Alder/vendor/Inner"));
        fs::create_dir_all(root.join("notes/plain")).unwrap();
        fs::create_dir_all(root.join("Worktree")).unwrap();
        fs::write(root.join("Worktree/.git"), "gitdir: /elsewhere\n").unwrap();
        fs::create_dir_all(root.join("Broken/.git")).unwrap();
        std::os::unix::fs::symlink(root.join("team"), root.join("linked")).unwrap();
        let found = scan(root);
        let names: Vec<_> = found.iter().map(|path| path.strip_prefix(root).unwrap().display().to_string()).collect();
        assert_eq!(names, ["Alder", "Worktree", "team/Birch", "team/apps/Hazel"]);
        assert_eq!(scan(&root.join("Alder")), [root.join("Alder")]);
        assert!(scan(&root.join("missing")).is_empty());
    }

    #[test]
    fn the_list_puts_favourites_first_and_shows_each_repository_once() {
        let mut memory = Memory::default();
        memory.add_recent(Path::new("/w/Oak"), 1);
        memory.add_recent(Path::new("/w/Elm"), 2);
        memory.set_favourite(Path::new("/w/Oak"), true);
        memory.set_favourite(Path::new("/w/Ash"), true);
        memory.set_name(Path::new("/w/Elm"), Some("Elm (site)"));
        let found = [PathBuf::from("/p/Fir"), PathBuf::from("/w/Elm")];
        let open = [PathBuf::from("/w/Elm")];
        let rows = listed(&memory, &found, &open, |path| path != Path::new("/w/Ash"));
        let shown: Vec<_> = rows.iter().map(|row| (row.name.as_str(), row.section.clone(), row.favourite, row.missing, row.open)).collect();
        assert_eq!(
            shown,
            [
                ("Oak", Section::Favourites, true, false, false),
                ("Ash", Section::Favourites, true, true, false),
                ("Elm (site)", Section::Recent, false, false, true),
                ("Fir", Section::Projects, false, false, false),
            ]
        );
        assert_eq!(rows[3].parent, "/p");
    }

    #[test]
    fn a_batch_runs_three_at_a_time_and_counts_what_happened() {
        let paths: Vec<PathBuf> = ["/a", "/b", "/c", "/d", "/e"].iter().map(PathBuf::from).collect();
        let mut batch = Batch::new(Kind::Fetch, paths.clone(), 1);
        let started: Vec<_> = std::iter::from_fn(|| batch.take_next()).collect();
        assert_eq!(started, paths[..LIMIT]);
        assert_eq!((batch.running, batch.peak), (LIMIT, LIMIT));
        batch.finish(Path::new("/b"), Ok("1 new branch".into()));
        assert_eq!(batch.take_next(), Some(PathBuf::from("/d")));
        assert_eq!(batch.take_next(), None);
        batch.finish(Path::new("/a"), Err("Could not reach origin".into()));
        batch.finish(Path::new("/a"), Ok("twice".into()));
        assert_eq!(batch.step(Path::new("/a")), Some(&Step::Done(Err("Could not reach origin".into()))));
        batch.stop();
        assert_eq!(batch.take_next(), None);
        assert_eq!(batch.step(Path::new("/e")), Some(&Step::Skipped));
        assert!(!batch.finished());
        batch.finish(Path::new("/c"), Ok(String::new()));
        batch.finish(Path::new("/d"), Ok(String::new()));
        assert!(batch.finished());
        assert_eq!(batch.counts(), (3, 1, 1));
        assert!(batch.peak <= LIMIT);
        let notice = summary(&batch, |path| path.display().to_string().trim_start_matches('/').to_uppercase());
        assert_eq!(notice.title, "Fetched 3 of 5 repositories");
        assert!(notice.detail.starts_with("A: Could not reach origin"), "{}", notice.detail);
        assert_eq!(notice.output.as_deref(), Some("A: Failed — Could not reach origin\nB: 1 new branch\nC: Fetched\nD: Fetched\nE: Not started"));

        let mut all = Batch::new(Kind::Pull, paths[..3].to_vec(), 2);
        while let Some(path) = all.take_next() {
            all.finish(&path, Ok(String::new()));
        }
        let notice = summary(&all, |path| path.display().to_string().trim_start_matches('/').to_uppercase());
        assert_eq!((notice.title.as_str(), notice.detail.as_str()), ("Pulled 3 repositories", "Pulled A, B and C."));
    }
}
