use crate::{
    Tab, clone, explain, kit,
    notice::Notice,
    refs, repository,
    settings::{self, Memory},
    theme::*,
    view::{self, CommitInput, Desk, Tone},
};
use gpui::{App, Context, Entity, Focusable, KeyDownEvent, PathPromptOptions, Window, div, prelude::*, px, rgb};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Target};
    &[Command::new(
        "repo.init",
        Group::Repository,
        |_, _| "New repository…".into(),
        |_, target, _| if *target == Target::Repo { Availability::Enabled } else { Availability::Hidden },
        |_, _| Some(view::Action::Init(Action::Open)),
    )
    .words(&["git init", "init repo", "initialise", "initialize", "create a repository", "start a project"])
    .icon("plus")
    .keys("cmd-i")
    .bar(Bar::File)]
};

pub(crate) const FIRST_COMMIT: &str = "Initial commit";
const README: &str = "README.md";
const SCANNED: usize = 2000;
const OFFERED: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    New,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub mode: Mode,
    pub parent: PathBuf,
    pub name: String,
    pub folder: PathBuf,
    pub branch: String,
    pub readme: bool,
    pub lfs: bool,
}

impl Request {
    pub(crate) fn destination(&self) -> PathBuf {
        match self.mode {
            Mode::New => self.parent.join(&self.name),
            Mode::Existing => self.folder.clone(),
        }
    }

    pub(crate) fn title(&self) -> String {
        let destination = self.destination();
        destination.file_name().map_or_else(|| destination.display().to_string(), |name| name.to_string_lossy().into_owned())
    }

    pub(crate) fn problem(&self) -> Option<String> {
        self.name_problem().or_else(|| self.place_problem()).or_else(|| self.branch_problem())
    }

    fn name_problem(&self) -> Option<String> {
        (self.mode == Mode::New).then(|| clone::name_problem(&self.name)).flatten()
    }

    pub(crate) fn place_problem(&self) -> Option<String> {
        match self.mode {
            Mode::New => clone::destination_problem(&self.parent, &self.destination()).err(),
            Mode::Existing => {
                let folder = &self.folder;
                if folder.as_os_str().is_empty() {
                    Some(String::new())
                } else if !folder.is_absolute() || !folder.is_dir() {
                    Some(format!("{} is not a folder that exists.", folder.display()))
                } else if fs::symlink_metadata(folder.join(".git")).is_ok() {
                    Some(format!("{} is already a Git repository.", folder.display()))
                } else if git_folder(folder) {
                    Some(format!("{} is a bare repository or a repository's Git folder.", folder.display()))
                } else if self.readme && fs::symlink_metadata(folder.join(README)).is_ok() {
                    Some(format!("{} already has a {README}. Untick Add a README to keep it as it is.", folder.display()))
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn branch_problem(&self) -> Option<String> {
        refs::name_problem(refs::Kind::Branch, &self.branch, std::iter::empty())
    }
}

fn git_folder(folder: &Path) -> bool {
    folder.join("HEAD").is_file() && folder.join("objects").is_dir() && folder.join("refs").is_dir()
}

#[derive(Debug)]
pub(crate) struct Created {
    pub root: PathBuf,
    pub commit: Option<Result<(), String>>,
    pub lfs: Option<Result<(), String>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Open,
    Here(PathBuf),
    SetMode(Mode),
    Choose,
    SetParent(PathBuf),
    SetFolder(PathBuf),
    ToggleReadme,
    ToggleLfs,
    Submit,
    Close,
    Show(PathBuf),
    Forget(PathBuf),
}

pub(crate) struct State {
    open: bool,
    mode: Mode,
    pub(crate) name: Entity<CommitInput>,
    pub(crate) branch: Entity<CommitInput>,
    parent: PathBuf,
    folder: PathBuf,
    readme: bool,
    lfs: bool,
    lfs_found: bool,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let name = cx.new(|cx| CommitInput::with_placeholder(cx, "Repository name"));
        let branch = cx.new(|cx| CommitInput::with_placeholder(cx, "main"));
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        cx.observe(&branch, |_, _, cx| cx.notify()).detach();
        Self {
            open: false,
            mode: Mode::New,
            name,
            branch,
            parent: PathBuf::new(),
            folder: PathBuf::new(),
            readme: true,
            lfs: false,
            lfs_found: false,
        }
    }

    pub(crate) fn open(&self) -> bool {
        self.open
    }

    pub(crate) fn request(&self, cx: &App) -> Request {
        Request {
            mode: self.mode,
            parent: self.parent.clone(),
            name: self.name.read(cx).text().trim().to_owned(),
            folder: self.folder.clone(),
            branch: self.branch.read(cx).text().trim().to_owned(),
            readme: self.readme,
            lfs: self.lfs && self.lfs_found,
        }
    }
}


pub(crate) fn create(request: &Request) -> Result<Created, String> {
    let failed = |reason: String| format!("Create Failed — {reason}");
    if let Some(problem) = request.problem() {
        return Err(failed(if problem.is_empty() { "Enter a name and a folder for the repository.".to_owned() } else { problem }));
    }
    let folder = request.destination();
    let created = match request.mode {
        Mode::New => match fs::create_dir(&folder) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !clone::destination_problem(&request.parent, &folder).map_err(failed)? {
                    return Err(failed(format!("{} changed while the repository was being created.", folder.display())));
                }
                false
            }
            Err(error) => return Err(failed(format!("Could not create {}: {error}.", folder.display()))),
        },
        Mode::Existing => false,
    };
    let result = repository::git_command(&folder).and_then(|mut command| {
        command.arg("init").arg(format!("--initial-branch={}", request.branch)).arg("--").arg(&folder);
        repository::checked(&mut command)
    });
    if let Err(error) = result {
        match request.mode {
            Mode::New => clone::discard(&folder, created),
            Mode::Existing => {
                let _ = fs::remove_dir_all(folder.join(".git"));
            }
        }
        return Err(failed(error));
    }
    let root = fs::canonicalize(&folder).map_err(|error| failed(format!("{} cannot be read: {error}.", folder.display())))?;
    let lfs = request.lfs.then(|| crate::lfs::set_up_new(&root));
    let commit = request.readme.then(|| readme(&root, &request.branch));
    Ok(Created { root, commit, lfs })
}

fn readme(root: &Path, branch: &str) -> Result<(), String> {
    use std::io::Write as _;
    let name = root.file_name().map_or_else(|| root.display().to_string(), |name| name.to_string_lossy().into_owned());
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(README))
        .and_then(|mut file| file.write_all(format!("# {name}\n").as_bytes()));
    match written {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("{README} was already there, so it was left as it is and not committed."));
        }
        Err(error) => return Err(format!("{README} could not be written: {error}.")),
    }
    if let Some(reason) = identity_problem(root) {
        return Err(reason);
    }
    first_commit(root, branch)
}

pub(crate) fn identity_problem(root: &Path) -> Option<String> {
    let known = |variable: &str| {
        repository::git_command(root)
            .and_then(|mut command| {
                command.args(["-c", "user.useConfigOnly=true", "var", variable]);
                repository::run(&mut command).map_err(|error| error.to_string())
            })
            .is_ok_and(|output| output.status.success())
    };
    (!known("GIT_AUTHOR_IDENT") || !known("GIT_COMMITTER_IDENT")).then(|| {
        format!("{README} is not committed: Git has no name and email for you yet. Add them in the commit panel, then commit it.")
    })
}

pub(crate) fn first_commit(root: &Path, branch: &str) -> Result<(), String> {
    let git = |args: &[&str]| {
        repository::git_command(root).map(|mut command| {
            command.args(args);
            command
        })
    };
    let head = repository::checked(&mut git(&["--no-optional-locks", "symbolic-ref", "--quiet", "HEAD"])?)?;
    let tip = repository::run(&mut git(&["--no-optional-locks", "rev-parse", "--quiet", "--verify", "HEAD^{commit}"])?)
        .map_err(|error| format!("Run bundled Git: {error}"))?;
    if String::from_utf8_lossy(&head).trim_end() != format!("refs/heads/{branch}") || tip.status.success() {
        return Err(format!("The repository changed before its first commit, so {README} was not committed."));
    }
    repository::checked(&mut git(&["add", "--", README])?)?;
    repository::checked(&mut git(&["-c", "user.useConfigOnly=true", "commit", "--quiet", "-m", FIRST_COMMIT, "--", README])?)?;
    Ok(())
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unopenable {
    Missing,
    NotRepository { folder: PathBuf, holds: Vec<PathBuf> },
    Bare,
    GitFolder { tree: Option<PathBuf> },
    Other(String),
}

impl Unopenable {
    pub(crate) fn of(path: &Path, error: String) -> Self {
        if fs::metadata(path).is_err_and(|error| {
            matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory)
        }) {
            return Self::Missing;
        }
        if error.contains("dubious ownership") {
            return Self::Other(error);
        }
        let folder = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        let asked = repository::git_command(folder).and_then(|mut command| {
            command.args(["--no-optional-locks", "rev-parse", "--is-bare-repository", "--is-inside-git-dir"]);
            repository::checked(&mut command)
        });
        if let Ok(said) = asked {
            let said = String::from_utf8_lossy(&said);
            let answers: Vec<&str> = said.lines().collect();
            match answers.as_slice() {
                ["true", _] => return Self::Bare,
                ["false", "true"] => return Self::GitFolder { tree: tree_above(folder) },
                _ => {}
            }
        }
        if error.contains("not a git repository") {
            return Self::NotRepository { folder: folder.to_owned(), holds: repositories_in(folder) };
        }
        Self::Other(error)
    }
}

fn tree_above(git_folder: &Path) -> Option<PathBuf> {
    let tree = git_folder.parent()?;
    (git_folder.file_name()? == ".git" && tree.join(".git").is_dir()).then(|| tree.to_owned())
}

pub(crate) fn repositories_in(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
    let mut found: Vec<PathBuf> = entries
        .take(SCANNED)
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && fs::symlink_metadata(path.join(".git")).is_ok())
        .collect();
    found.sort_by_key(|path| path.file_name().map(|name| name.to_string_lossy().to_lowercase()));
    found
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(crate) fn refused(tab: &mut Tab, path: PathBuf, why: Unopenable, cx: &mut Context<Tab>) {
    let notice = words(&path, why, |path| settings::memory(cx).is_some_and(|memory| memory.is_recent(path)));
    tab.ui.notices.show(notice);
    cx.notify();
}

pub(crate) fn words(path: &Path, why: Unopenable, recent: impl Fn(&Path) -> bool) -> Notice {
    let choose = || ("Choose another…".to_owned(), view::Action::Open);
    let name = name_of(path);
    let (title, detail, offers): (String, String, Vec<(String, view::Action)>) = match why {
        Unopenable::Missing => (
            format!("{name} no longer exists"),
            format!("Nothing is at {} any more. It was moved or deleted.", path.display()),
            recent(path)
                .then(|| ("Remove from recents".to_owned(), view::Action::Init(Action::Forget(path.to_owned()))))
                .into_iter()
                .chain([choose()])
                .collect(),
        ),
        Unopenable::NotRepository { folder, holds } if holds.is_empty() => (
            format!("{} is not a Git repository", name_of(&folder)),
            format!("{} has no repository. Create one here, or choose another folder.", folder.display()),
            vec![
                ("Create one here".to_owned(), view::Action::Init(Action::Here(folder.clone()))),
                choose(),
            ],
        ),
        Unopenable::NotRepository { folder, holds } => {
            let names: Vec<String> = holds.iter().map(|path| name_of(path)).collect();
            let shown = if names.len() > 5 {
                format!("{}, and {} more", names[..5].join(", "), names.len() - 5)
            } else {
                listed(&names)
            };
            let count = if holds.len() == 1 { "a repository".to_owned() } else { format!("{} repositories", holds.len()) };
            (
                format!("{} is not a Git repository", name_of(&folder)),
                format!("{} holds {count}: {shown}. Open one of them, or choose another folder.", folder.display()),
                holds
                    .iter()
                    .take(OFFERED)
                    .map(|repository| (format!("Open {}", name_of(repository)), view::Action::Init(Action::Show(repository.clone()))))
                    .chain([choose()])
                    .collect(),
            )
        }
        Unopenable::Bare => {
            let address = path.display().to_string();
            let again = clone::Request {
                address,
                parent: path.parent().map(Path::to_owned).unwrap_or_default(),
                name: clone::folder_name(&path.display().to_string()).unwrap_or_default(),
                options: clone::Options::default(),
                account: None,
            };
            (
                format!("{name} is a bare repository"),
                "FastKraken needs a working tree, with the repository's files. Clone it to get one.".to_owned(),
                vec![("Clone it…".to_owned(), view::Action::Clone(clone::Action::Edit(again))), choose()],
            )
        }
        Unopenable::GitFolder { tree: Some(tree) } => (
            format!("{name} is a repository's Git folder"),
            format!("FastKraken opens the folder that holds it, {}.", tree.display()),
            vec![(format!("Open {}", name_of(&tree)), view::Action::Init(Action::Show(tree.clone()))), choose()],
        ),
        Unopenable::GitFolder { tree: None } => (
            format!("{name} is a repository's Git folder"),
            "FastKraken needs the repository's working tree, the folder with its files.".to_owned(),
            vec![choose()],
        ),
        Unopenable::Other(error) => {
            let said = if error.starts_with("Open Failed") { error } else { format!("Open Failed — {error}") };
            return explain::notice(said);
        }
    };
    let notice = Notice::new(Tone::Info, title, detail).sticky();
    offers.into_iter().fold(notice, |notice, (label, action)| notice.offer(label, action))
}


fn show(tab: &mut Tab, mode: Mode, folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Tab>) {
    tab.ui.menu = None;
    let parent = clone::default_parent(cx);
    let branch = settings::settings(cx).default_branch();
    let state = &mut tab.ui.init;
    state.open = true;
    state.mode = mode;
    state.parent = parent;
    state.folder = folder.unwrap_or_default();
    state.readme = mode == Mode::New;
    state.lfs = false;
    state.name.update(cx, |name, cx| name.set_text("", cx));
    state.branch.update(cx, |field, cx| field.set_text(&branch, cx));
    let field = if mode == Mode::New { &tab.ui.init.name } else { &tab.ui.init.branch };
    window.focus(&field.focus_handle(cx));
    Tab::work(cx, crate::lfs::helper_on_path, |tab, found, cx| {
        tab.ui.init.lfs_found = found;
        cx.notify();
    });
}

fn close(tab: &mut Tab, window: &mut Window) {
    tab.ui.init.open = false;
    view::focus(&tab.ui, window);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open if tab.ui.init.open => {}
        Action::Open => show(tab, Mode::New, None, window, cx),
        Action::Here(folder) => show(tab, Mode::Existing, Some(folder), window, cx),
        Action::SetMode(mode) => {
            let state = &mut tab.ui.init;
            if state.mode != mode {
                state.mode = mode;
                state.readme = mode == Mode::New;
            }
        }
        Action::Close => close(tab, window),
        Action::Choose => {
            let existing = tab.ui.init.mode == Mode::Existing;
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(if existing { "Use this folder" } else { "Create here" }.into()),
            });
            let tab_handle = cx.weak_entity();
            window
                .spawn(cx, async move |cx| {
                    if let Ok(Ok(Some(paths))) = picker.await {
                        if let Some(path) = paths.into_iter().next() {
                            let action = if existing { Action::SetFolder(path) } else { Action::SetParent(path) };
                            let _ = cx.update(|window, cx| tab_handle.update(cx, |tab, cx| tab.dispatch(view::Action::Init(action), window, cx)));
                        }
                    }
                })
                .detach();
        }
        Action::SetParent(path) => tab.ui.init.parent = path,
        Action::SetFolder(path) => tab.ui.init.folder = path,
        Action::ToggleReadme => tab.ui.init.readme = !tab.ui.init.readme,
        Action::ToggleLfs => tab.ui.init.lfs = !tab.ui.init.lfs,
        Action::Submit => {
            let request = tab.ui.init.request(cx);
            if request.problem().is_some() {
                return;
            }
            close(tab, window);
            start(tab, request, cx);
        }
        Action::Show(path) if tab.repo.is_some() && crate::tabs::present(cx) => {
            crate::tabs::open_elsewhere(path, |_, _| {}, cx);
        }
        Action::Show(path) => tab.open(path, cx),
        Action::Forget(path) => {
            Memory::update(cx, |memory| memory.remove_recent(&path));
            tab.ui.notices.show(Notice::new(Tone::Info, format!("Removed {} from recent repositories", name_of(&path)), ""));
        }
    }
    cx.notify();
}

pub(crate) fn start(tab: &mut Tab, request: Request, cx: &mut Context<Tab>) {
    let title = request.title();
    let epoch = tab.epoch;
    let spinner = tab.ui.notices.progress(format!("Creating {title}…"));
    cx.notify();
    let asked = request.clone();
    Tab::work(cx, move || create(&asked), move |tab, result, cx| {
        let logged = tab.repo.as_ref().and_then(|_| tab.trail.entry(true));
        let created = match result {
            Ok(created) => created,
            Err(failure) => {
                tab.ui.notices.finish(spinner, explain::notice(failure).logged(logged));
                cx.notify();
                return;
            }
        };
        let branch = &request.branch;
        let made = match (&created.commit, &created.lfs) {
            (Some(Err(reason)), lfs) => {
                let detail = match lfs {
                    Some(Err(why)) => format!("{reason} Git LFS was not set up for it: {why}"),
                    _ => reason.clone(),
                };
                Notice::new(Tone::Info, format!("Created {title}"), detail).sticky()
            }
            (_, Some(Err(reason))) => {
                Notice::new(Tone::Info, format!("Created {title}"), format!("Git LFS was not set up for it: {reason}")).sticky()
            }
            (commit, lfs) => {
                let mut detail = match commit {
                    None => format!("On {branch}, with no commits yet."),
                    _ => format!("On {branch}, with {README} as its first commit."),
                };
                if lfs.is_some() {
                    detail.push_str(" Git LFS is set up for it.");
                }
                Notice::new(Tone::Success, format!("Created {title}"), detail)
            }
        };
        let root = created.root;
        let in_the_way = tab.ui.dialog_open() || tab.ui.modal_open();
        if tab.repo.is_some() && !in_the_way && crate::tabs::present(cx) {
            tab.ui.notices.remove(spinner);
            crate::tabs::open_elsewhere(root, move |tab, _| { tab.ui.notices.show(made); }, cx);
        } else if tab.epoch == epoch && tab.ui.opening.is_none() && tab.can_open() && !tab.ui.dialog_open() && !tab.ui.modal_open() {
            tab.ui.notices.remove(spinner);
            tab.open_then(root, move |tab, _| { tab.ui.notices.show(made); }, cx);
        } else {
            tab.ui.notices.finish(spinner, made.offer("Open now", view::Action::Init(Action::Show(root))).sticky());
        }
        cx.notify();
    });
}


fn chooser<T: Desk>(path: &Path, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let shown = if path.as_os_str().is_empty() { "No folder chosen".to_owned() } else { path.display().to_string() };
    div()
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
                .border_color(rgb(LINE))
                .bg(rgb(CANVAS))
                .child(kit::fit(shown).cut(kit::Cut::Start).grow()),
        )
        .child(kit::btn("init-choose", "Choose…").on(view::Action::Init(Action::Choose), cx))
}

pub(crate) fn dialog<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let ui = app.ui();
    let state = &ui.init;
    if !state.open {
        return None;
    }
    let request = state.request(cx);
    let name = request.name_problem().filter(|problem| !problem.is_empty());
    let branch = request.branch_problem();
    let place = request.place_problem().filter(|problem| !problem.is_empty() && name.is_none() && (request.mode == Mode::Existing || !request.name.is_empty()));
    let ready = request.problem().is_none();
    fn submit<T: Desk>(cx: &mut Context<T>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(|app: &mut T, e: &KeyDownEvent, w, cx| {
            if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
                app.dispatch(view::Action::Init(Action::Submit), w, cx);
                cx.stop_propagation();
            }
        })
    }
    let modes = kit::segmented()
        .child(kit::segment("init-new", "Create a folder", Some("plus"), state.mode == Mode::New, view::Action::Init(Action::SetMode(Mode::New)), cx))
        .child(kit::segment(
            "init-existing",
            "Use an existing folder",
            Some("folder"),
            state.mode == Mode::Existing,
            view::Action::Init(Action::SetMode(Mode::Existing)),
            cx,
        ));
    let mut body = kit::dialog("Create a repository", 480.).child(div().flex().child(modes));
    body = match state.mode {
        Mode::New => {
            let full = if request.name.is_empty() || name.is_some() {
                kit::muted("Full path: choose a name")
            } else {
                kit::muted(format!("Full path: {}", request.destination().display()))
            };
            body.child(div().on_key_down(submit(cx)).child(kit::field("Name", state.name.clone(), name.map(Into::into))))
                .child(kit::field("Create in", chooser(&request.parent, cx), None))
                .child(full.text_size(px(TEXT_SMALL)))
        }
        Mode::Existing => body.child(kit::field(
            "Folder",
            chooser(&request.folder, cx),
            Some("Git starts tracking this folder. Its files stay as they are.".into()),
        )),
    };
    let tick = div()
        .id("init-readme")
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_CHIP))
        .text_size(px(12.5))
        .cursor_pointer()
        .text_color(rgb(TEXT_2))
        .focusable()
        .focus(|s| s.text_color(rgb(TEXT)))
        .child(kit::checkbox(state.readme))
        .child(format!("Add a {README} and make it the first commit"));
    let body = body
        .child(div().on_key_down(submit(cx)).child(kit::field("Default branch", state.branch.clone(), branch.map(Into::into))))
        .child(kit::pressable(tick, view::Action::Init(Action::ToggleReadme), cx))
        .when(state.lfs_found, |d| {
            let tick = div()
                .id("init-lfs")
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS_CHIP))
                .text_size(px(12.5))
                .cursor_pointer()
                .text_color(rgb(TEXT_2))
                .focusable()
                .focus(|s| s.text_color(rgb(TEXT)))
                .child(kit::checkbox(state.lfs))
                .child("Set up Git LFS for large files");
            d.child(kit::pressable(tick, view::Action::Init(Action::ToggleLfs), cx))
        })
        .when_some(place, |d, place| d.child(div().text_size(px(TEXT_SMALL)).text_color(rgb(DELETED)).child(place)))
        .child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(kit::btn("init-cancel", "Cancel").on(view::Action::Init(Action::Close), cx))
                .child(kit::btn("init-start", "Create repository").primary().enabled(ready).on(view::Action::Init(Action::Submit), cx)),
        );
    Some(kit::modal(body, Some(view::Action::Init(Action::Close)), &ui.modal_focus, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = repository::bundled_command(dir).unwrap().args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn new(parent: &Path, name: &str, branch: &str, readme: bool) -> Request {
        Request { mode: Mode::New, parent: parent.into(), name: name.into(), folder: PathBuf::new(), branch: branch.into(), readme, lfs: false }
    }

    fn existing(folder: &Path, readme: bool) -> Request {
        Request {
            mode: Mode::Existing,
            parent: PathBuf::new(),
            name: String::new(),
            folder: folder.into(),
            branch: "main".into(),
            readme,
            lfs: false,
        }
    }

    fn identity(dir: &Path) -> GlobalConfig {
        let file = dir.join("gitconfig");
        fs::write(&file, "[user]\n\tname = Aspen Walker\n\temail = aspen@example.invalid\n[commit]\n\tgpgSign = false\n[core]\n\thooksPath = /dev/null\n").unwrap();
        GlobalConfig::set(&file)
    }

    #[test]
    fn a_new_repository_with_a_readme_has_one_commit_on_the_chosen_branch() {
        let dir = tempfile::tempdir().unwrap();
        let _identity = identity(dir.path());
        let request = new(dir.path(), "Aspen-ridge", "trunk", true);
        assert_eq!(request.problem(), None);
        let created = create(&request).unwrap();
        assert_eq!(created.root, fs::canonicalize(dir.path().join("Aspen-ridge")).unwrap());
        assert_eq!(created.commit, Some(Ok(())));
        let root = &created.root;
        assert_eq!(git(root, &["symbolic-ref", "HEAD"]), "refs/heads/trunk\n");
        assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1\n");
        assert_eq!(git(root, &["log", "--format=%s|%an <%ae>"]), "Initial commit|Aspen Walker <aspen@example.invalid>\n");
        assert_eq!(git(root, &["ls-tree", "-r", "--name-only", "HEAD"]), "README.md\n");
        assert_eq!(git(root, &["show", "HEAD:README.md"]), "# Aspen-ridge\n");
        assert_eq!(git(root, &["status", "--porcelain"]), "");
        assert_eq!(git(root, &["for-each-ref", "--format=%(refname)"]), "refs/heads/trunk\n");
    }

    #[test]
    fn a_new_repository_can_start_with_git_lfs_set_up_and_says_when_it_could_not() {
        use crate::lfs::tests::{MANAGER, with_stub};
        let dir = tempfile::tempdir().unwrap();
        let _identity = identity(dir.path());
        let bin = dir.path().join("stub-bin");
        let request = Request { lfs: true, ..new(dir.path(), "Large-files", "main", true) };
        let created = with_stub(&bin, MANAGER, || create(&request)).unwrap();
        assert_eq!((created.lfs.clone(), created.commit.clone()), (Some(Ok(())), Some(Ok(()))));
        let root = &created.root;
        assert_eq!(git(root, &["config", "--local", "filter.lfs.clean"]), "git-lfs clean -- %f\n");
        assert_eq!(fs::read_to_string(bin.join("calls")).unwrap(), "install --local\n");
        assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1\n");
        assert_eq!(git(root, &["status", "--porcelain"]), "");

        let refused = "#!/bin/sh\necho 'Hook already exists: pre-push' >&2\nexit 2\n";
        let broken = Request { lfs: true, ..new(dir.path(), "No-large-files", "main", false) };
        let created = with_stub(&bin, refused, || create(&broken)).unwrap();
        assert_eq!(created.lfs, Some(Err("Hook already exists: pre-push".to_owned())));
        assert!(!git(&created.root, &["config", "--local", "--list"]).contains("filter.lfs"));
        let plain = with_stub(&bin, refused, || create(&new(dir.path(), "Plain-files", "main", false))).unwrap();
        assert_eq!(plain.lfs, None);
    }

    #[test]
    fn git_lfs_is_set_up_in_a_new_repository_even_when_the_global_settings_have_its_filters() {
        use crate::lfs::tests::{MANAGER, with_stub};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gitconfig");
        fs::write(
            &file,
            "[user]\n\tname = Aspen Walker\n\temail = aspen@example.invalid\n[core]\n\thooksPath = /dev/null\n[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n",
        )
        .unwrap();
        let _global = GlobalConfig::set(&file);
        let bin = dir.path().join("stub-bin");
        let request = Request { lfs: true, ..new(dir.path(), "Shared-filters", "main", false) };
        let created = with_stub(&bin, MANAGER, || create(&request)).unwrap();
        assert_eq!(created.lfs, Some(Ok(())));
        assert_eq!(fs::read_to_string(bin.join("calls")).unwrap(), "install --local\n");
        assert_eq!(git(&created.root, &["config", "--local", "filter.lfs.clean"]), "git-lfs clean -- %f\n");
    }

    #[test]
    fn without_a_readme_the_repository_has_no_commits() {
        let dir = tempfile::tempdir().unwrap();
        let created = create(&new(dir.path(), "Bare-slope", "main", false)).unwrap();
        assert_eq!(created.commit, None);
        assert_eq!(git(&created.root, &["symbolic-ref", "HEAD"]), "refs/heads/main\n");
        assert_eq!(git(&created.root, &["for-each-ref"]), "");
        assert_eq!(fs::read_dir(&created.root).unwrap().count(), 1, "only .git");
    }

    #[test]
    fn without_an_identity_the_readme_is_written_and_not_committed() {
        let dir = tempfile::tempdir().unwrap();
        let created = create(&new(dir.path(), "Nameless", "main", true)).unwrap();
        let reason = created.commit.unwrap().unwrap_err();
        assert!(reason.contains("Git has no name and email for you yet"), "{reason}");
        assert_eq!(identity_problem(&created.root).as_deref(), Some(reason.as_str()));
        assert_eq!(fs::read_to_string(created.root.join("README.md")).unwrap(), "# Nameless\n");
        assert_eq!(git(&created.root, &["for-each-ref"]), "");
        assert_eq!(git(&created.root, &["status", "--porcelain"]), "?? README.md\n");
        git(&created.root, &["config", "user.name", "Local Walker"]);
        git(&created.root, &["config", "user.email", "local@example.invalid"]);
        assert_eq!(identity_problem(&created.root), None);
    }

    #[test]
    fn an_existing_folder_is_initialised_in_place_and_keeps_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let _identity = identity(dir.path());
        let folder = dir.path().join("notes");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("plan.txt"), "Walk the ridge\n").unwrap();
        let created = create(&existing(&folder, true)).unwrap();
        assert_eq!(created.commit, Some(Ok(())));
        assert_eq!(git(&created.root, &["ls-tree", "-r", "--name-only", "HEAD"]), "README.md\n");
        assert_eq!(fs::read_to_string(folder.join("plan.txt")).unwrap(), "Walk the ridge\n");
        assert_eq!(git(&created.root, &["status", "--porcelain"]), "?? plan.txt\n");
        let again = existing(&folder, false).problem().unwrap();
        assert_eq!(again, format!("{} is already a Git repository.", folder.display()));
    }

    #[test]
    fn a_folder_that_is_taken_or_a_readme_that_is_there_is_refused_before_git_runs() {
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("Taken");
        fs::create_dir(&taken).unwrap();
        fs::write(taken.join("keep.txt"), "mine\n").unwrap();
        let problem = new(dir.path(), "Taken", "main", true).problem().unwrap();
        assert_eq!(problem, format!("{} already exists and is not empty.", taken.display()));
        assert_eq!(create(&new(dir.path(), "Taken", "main", true)).unwrap_err(), format!("Create Failed — {problem}"));
        assert!(!taken.join(".git").exists());
        fs::write(taken.join("README.md"), "Mine\n").unwrap();
        assert!(existing(&taken, true).problem().unwrap().ends_with("already has a README.md. Untick Add a README to keep it as it is."));
        assert_eq!(existing(&taken, false).problem(), None);
        assert_eq!(new(dir.path(), "a/b", "main", false).problem().as_deref(), Some("Enter a folder name without slashes."));
        assert_eq!(new(dir.path(), "", "main", false).problem().as_deref(), Some(""));
        assert_eq!(new(dir.path(), "Fine", "bad name", false).problem().as_deref(), Some("Branch names cannot contain spaces"));
        assert_eq!(new(dir.path(), "Fine", "", false).problem().as_deref(), Some("Type a name for the branch"));
        assert_eq!(existing(&dir.path().join("gone"), false).problem().unwrap(), format!("{} is not a folder that exists.", dir.path().join("gone").display()));
    }

    #[test]
    fn a_folder_filled_after_the_dialog_checked_it_is_refused_by_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let request = new(dir.path(), "Late", "main", true);
        assert_eq!(request.problem(), None);
        fs::create_dir(dir.path().join("Late")).unwrap();
        fs::write(dir.path().join("Late/mine.txt"), "mine\n").unwrap();
        assert!(create(&request).unwrap_err().ends_with("already exists and is not empty."));
        assert_eq!(fs::read_dir(dir.path().join("Late")).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_init_leaves_nothing_behind() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("keep.txt"), "mine\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let failure = create(&existing(&locked, false)).unwrap_err();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failure.starts_with("Create Failed — "), "{failure}");
        assert!(!locked.join(".git").exists());
        assert_eq!(fs::read_dir(&locked).unwrap().count(), 1);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let failure = create(&new(&locked, "Inner", "main", true)).unwrap_err();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(failure.starts_with("Create Failed — Could not create "), "{failure}");
        assert!(!locked.join("Inner").exists());
        fs::write(dir.path().join("Blocked"), "a file\n").unwrap();
        assert!(create(&new(dir.path(), "Blocked", "main", false)).unwrap_err().ends_with("already exists and is not a folder."));
        assert_eq!(fs::read_to_string(dir.path().join("Blocked")).unwrap(), "a file\n");
    }

    #[test]
    fn the_first_commit_is_refused_in_a_repository_that_changed() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "FastKraken: git for the impatient\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", "Start the trail"]);
        let tip = f.git(&["rev-parse", "HEAD"]);
        fs::write(f.dir.join("README.md"), "# Fixture\n").unwrap();
        let refused = first_commit(&f.dir, "main").unwrap_err();
        assert!(refused.contains("changed before its first commit"), "{refused}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), tip);
        assert_eq!(f.git(&["status", "--porcelain"]), b"?? README.md\n");
        let dir = tempfile::tempdir().unwrap();
        let fresh = create(&new(dir.path(), "Moved", "main", false)).unwrap();
        fs::write(fresh.root.join("README.md"), "# Moved\n").unwrap();
        git(&fresh.root, &["symbolic-ref", "HEAD", "refs/heads/other"]);
        assert!(first_commit(&fresh.root, "main").is_err());
        assert_eq!(git(&fresh.root, &["for-each-ref"]), "");
        assert_eq!(git(&fresh.root, &["status", "--porcelain"]), "?? README.md\n");
    }

    #[test]
    fn a_folder_that_will_not_open_is_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        fs::create_dir(&plain).unwrap();
        let open = |path: &Path| match repository::Repository::open(path) {
            Ok(_) => panic!("{} opened", path.display()),
            Err(error) => Unopenable::of(path, error),
        };
        assert_eq!(open(&plain), Unopenable::NotRepository { folder: plain.clone(), holds: Vec::new() });
        assert_eq!(open(&dir.path().join("gone")), Unopenable::Missing);
        let bare = dir.path().join("Juniper.git");
        git(dir.path(), &["init", "-q", "--bare", bare.to_str().unwrap()]);
        assert_eq!(open(&bare), Unopenable::Bare);
        let tree = create(&new(dir.path(), "Tree", "main", false)).unwrap().root;
        assert_eq!(open(&tree.join(".git")), Unopenable::GitFolder { tree: Some(tree.clone()) });
        let shelf = dir.path().join("shelf");
        fs::create_dir(&shelf).unwrap();
        for name in ["oak", "Birch", "cedar"] {
            create(&new(&shelf, name, "main", false)).unwrap();
        }
        fs::create_dir(shelf.join("loose")).unwrap();
        fs::write(shelf.join("notes.txt"), "").unwrap();
        let held = ["Birch", "cedar", "oak"].map(|name| shelf.join(name)).to_vec();
        assert_eq!(open(&shelf), Unopenable::NotRepository { folder: shelf.clone(), holds: held });
        assert!(matches!(Unopenable::of(&plain, "fatal: detected dubious ownership in repository at '/x'".into()), Unopenable::Other(_)));
    }

    #[test]
    fn a_folder_that_cannot_be_reached_has_not_gone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        let inside = locked.join("repo");
        fs::create_dir_all(&inside).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let why = match repository::Repository::open(&inside) {
            Ok(_) => None,
            Err(error) => Some(Unopenable::of(&inside, error)),
        };
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(why, Some(Unopenable::Other(_))), "{why:?}");
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(Unopenable::of(&file.join("repo"), "not there".into()), Unopenable::Missing);
    }

    #[test]
    fn a_bare_repository_or_a_git_folder_is_not_initialised_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("Juniper.git");
        git(dir.path(), &["init", "-q", "--bare", bare.to_str().unwrap()]);
        let problem = existing(&bare, false).problem().unwrap();
        assert_eq!(problem, format!("{} is a bare repository or a repository's Git folder.", bare.display()));
        assert_eq!(create(&existing(&bare, false)).unwrap_err(), format!("Create Failed — {problem}"));
        assert!(!bare.join(".git").exists());
        let tree = create(&new(dir.path(), "Tree", "main", false)).unwrap().root;
        assert!(existing(&tree.join(".git"), false).problem().unwrap().ends_with("is a bare repository or a repository's Git folder."));
        assert!(!tree.join(".git/.git").exists());
    }

    #[test]
    fn each_folder_that_will_not_open_says_why_and_offers_the_way_on() {
        let labels = |notice: &Notice| notice.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>();
        let plain = PathBuf::from("/Users/me/plain");
        let not = words(&plain, Unopenable::NotRepository { folder: plain.clone(), holds: Vec::new() }, |_| false);
        assert_eq!(not.title, "plain is not a Git repository");
        assert_eq!(labels(&not), ["Create one here", "Choose another…"]);
        assert!(matches!(&not.actions[0].1, view::Action::Init(Action::Here(folder)) if *folder == plain));
        let holds = ["a", "b", "c", "d"].map(|name| plain.join(name)).to_vec();
        let shelf = words(&plain, Unopenable::NotRepository { folder: plain.clone(), holds }, |_| false);
        assert_eq!(shelf.detail, "/Users/me/plain holds 4 repositories: a, b, c and d. Open one of them, or choose another folder.");
        assert_eq!(labels(&shelf), ["Open a", "Open b", "Open c", "Choose another…"]);
        let bare = words(Path::new("/srv/Juniper.git"), Unopenable::Bare, |_| false);
        assert_eq!(bare.title, "Juniper.git is a bare repository");
        assert!(matches!(&bare.actions[0].1, view::Action::Clone(clone::Action::Edit(request)) if request.name == "Juniper" && request.parent == Path::new("/srv")));
        let gone = Path::new("/Users/me/gone");
        let missing = words(gone, Unopenable::Missing, |path| path == gone);
        assert_eq!(missing.title, "gone no longer exists");
        assert_eq!(labels(&missing), ["Remove from recents", "Choose another…"]);
        assert_eq!(labels(&words(gone, Unopenable::Missing, |_| false)), ["Choose another…"]);
        let dubious = "fatal: detected dubious ownership in repository at '/Volumes/Shared/app'\nTo add an exception for this directory, call:\n\n\tgit config --global --add safe.directory /Volumes/Shared/app";
        let trust = words(Path::new("/Volumes/Shared/app"), Unopenable::Other(dubious.into()), |_| false);
        assert_eq!(trust.title, "Git does not trust this folder because another user owns it");
        assert_eq!(labels(&trust), ["Copy the command that trusts it"]);
    }
}
