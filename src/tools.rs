use crate::{
    Tab, kit,
    notice::Notice,
    settings::{self, Settings},
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, Window, div, prelude::*, px, rgb};
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const CUSTOM: &str = "custom";
const SETTLE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lines {
    Goto,
    Suffix,
    Flag,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Known {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) command: Option<&'static str>,
    pub(crate) apps: &'static [&'static str],
    pub(crate) inside: Option<&'static str>,
    pub(crate) lines: Lines,
}

pub(crate) const EDITORS: &[Known] = &[
    Known {
        id: "code",
        name: "VS Code",
        command: Some("code"),
        apps: &["Visual Studio Code.app"],
        inside: Some("Contents/Resources/app/bin/code"),
        lines: Lines::Goto,
    },
    Known {
        id: "cursor",
        name: "Cursor",
        command: Some("cursor"),
        apps: &["Cursor.app"],
        inside: Some("Contents/Resources/app/bin/cursor"),
        lines: Lines::Goto,
    },
    Known {
        id: "zed",
        name: "Zed",
        command: Some("zed"),
        apps: &["Zed.app"],
        inside: Some("Contents/MacOS/cli"),
        lines: Lines::Suffix,
    },
    Known {
        id: "subl",
        name: "Sublime Text",
        command: Some("subl"),
        apps: &["Sublime Text.app"],
        inside: Some("Contents/SharedSupport/bin/subl"),
        lines: Lines::Suffix,
    },
    Known {
        id: "idea",
        name: "IntelliJ IDEA",
        command: Some("idea"),
        apps: &["IntelliJ IDEA.app", "IntelliJ IDEA Ultimate.app", "IntelliJ IDEA CE.app"],
        inside: Some("Contents/MacOS/idea"),
        lines: Lines::Flag,
    },
    Known {
        id: "xed",
        name: "Xcode",
        command: None,
        apps: &["Xcode.app"],
        inside: Some("Contents/Developer/usr/bin/xed"),
        lines: Lines::Flag,
    },
];

pub(crate) const TERMINALS: &[Known] = &[
    Known { id: "terminal", name: "Terminal", command: None, apps: &["Terminal.app"], inside: None, lines: Lines::Flag },
    Known { id: "iterm", name: "iTerm", command: None, apps: &["iTerm.app"], inside: None, lines: Lines::Flag },
    Known { id: "ghostty", name: "Ghostty", command: None, apps: &["Ghostty.app"], inside: None, lines: Lines::Flag },
    Known { id: "warp", name: "Warp", command: None, apps: &["Warp.app"], inside: None, lines: Lines::Flag },
    Known { id: "kitty", name: "kitty", command: None, apps: &["kitty.app"], inside: None, lines: Lines::Flag },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum How {
    Program(PathBuf),
    App(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) tool: &'static Known,
    pub(crate) how: How,
}

impl Found {
    pub(crate) fn place(&self) -> &Path {
        match &self.how {
            How::Program(path) | How::App(path) => path,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Detected {
    pub(crate) editors: Vec<Found>,
    pub(crate) terminals: Vec<Found>,
}

impl Detected {
    fn editor(&self, id: &str) -> Option<&Found> {
        self.editors.iter().find(|found| found.tool.id == id)
    }

    fn terminal(&self, id: &str) -> Option<&Found> {
        self.terminals.iter().find(|found| found.tool.id == id)
    }
}

fn runnable(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn on_path(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(name)).find(|path| runnable(path))
}

fn application(tool: &Known, apps: &[PathBuf]) -> Option<PathBuf> {
    apps.iter()
        .flat_map(|folder| tool.apps.iter().map(move |name| folder.join(name)))
        .find(|path| path.is_dir())
}

fn find(tool: &'static Known, dirs: &[PathBuf], apps: &[PathBuf]) -> Option<Found> {
    if let Some(program) = tool.command.and_then(|command| on_path(dirs, command)) {
        return Some(Found { tool, how: How::Program(program) });
    }
    let app = application(tool, apps)?;
    let how = match tool.inside.map(|inside| app.join(inside)).filter(|program| runnable(program)) {
        Some(program) => How::Program(program),
        None => How::App(app),
    };
    Some(Found { tool, how })
}

pub(crate) fn detect(dirs: &[PathBuf], apps: &[PathBuf]) -> Detected {
    Detected {
        editors: EDITORS.iter().filter_map(|tool| find(tool, dirs, apps)).collect(),
        terminals: TERMINALS.iter().filter_map(|tool| find(tool, dirs, apps)).collect(),
    }
}

fn places() -> (Vec<PathBuf>, Vec<PathBuf>) {
    if let Some(sandbox) = crate::acceptance::sandbox() {
        return (vec![sandbox.join("bin")], vec![sandbox.join("Applications")]);
    }
    let mut apps: Vec<PathBuf> =
        ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute()) {
        apps.push(home.join("Applications"));
    }
    (crate::environment::host_dirs().to_vec(), apps)
}

struct Cache {
    dirs: Vec<PathBuf>,
    found: Arc<Detected>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

pub(crate) fn detected() -> Option<Arc<Detected>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|cache| cache.found.clone())
}

fn fresh() -> Option<Arc<Detected>> {
    let (dirs, _) = places();
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|cache| cache.dirs == dirs)
        .map(|cache| cache.found.clone())
}

fn look() -> Arc<Detected> {
    let (dirs, apps) = places();
    let found = Arc::new(detect(&dirs, &apps));
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Cache { dirs, found: found.clone() });
    found
}

pub(crate) fn start(again: bool, cx: &mut Context<Tab>) {
    if !again && fresh().is_some() {
        return;
    }
    Tab::work(cx, look, |_, _, cx| cx.notify());
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Editor {
    Found(Found),
    Custom(String),
    Gone(&'static Known),
}

pub(crate) fn editor(chosen: Option<&str>, custom: &str, found: &Detected) -> Option<Editor> {
    match chosen {
        Some(CUSTOM) => (!custom.trim().is_empty()).then(|| Editor::Custom(custom.trim().to_owned())),
        Some(id) => match found.editor(id) {
            Some(found) => Some(Editor::Found(found.clone())),
            None => EDITORS.iter().find(|tool| tool.id == id).map(Editor::Gone).or_else(|| first(&found.editors)),
        },
        None => first(&found.editors),
    }
}

fn first(found: &[Found]) -> Option<Editor> {
    found.first().cloned().map(Editor::Found)
}

pub(crate) fn terminal(chosen: Option<&str>, found: &Detected) -> Option<Result<Found, &'static Known>> {
    match chosen.and_then(|id| TERMINALS.iter().find(|tool| tool.id == id)) {
        Some(tool) => Some(found.terminal(tool.id).cloned().ok_or(tool)),
        None => found.terminals.first().cloned().map(Ok),
    }
}

pub(crate) fn editor_name(editor: &Editor) -> String {
    match editor {
        Editor::Found(found) => found.tool.name.to_owned(),
        Editor::Gone(tool) => tool.name.to_owned(),
        Editor::Custom(command) => words(command)
            .ok()
            .and_then(|words| words.into_iter().next())
            .map(|program| Path::new(&program).file_name().map_or(program.clone(), |name| name.to_string_lossy().into_owned()))
            .filter(|name| name != "open" && !name.contains('{'))
            .unwrap_or_else(|| "your editor".to_owned()),
    }
}

pub(crate) struct State {
    pub(crate) editor: Option<String>,
    pub(crate) terminal: Option<String>,
    pub(crate) custom: String,
    pub(crate) command: Entity<CommitInput>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let chosen = settings::settings(cx);
        let custom = chosen.editor_command.clone();
        let command = cx.new(|cx| {
            let mut field = CommitInput::with_placeholder(cx, "bbedit +{line} {file}");
            field.set_text(&custom, cx);
            field
        });
        cx.observe(&command, |tab, _, cx| typed(tab, cx)).detach();
        start(false, cx);
        Self { editor: chosen.editor, terminal: chosen.terminal, custom, command }
    }

    pub(crate) fn editor(&self) -> Option<Editor> {
        editor(self.editor.as_deref(), &self.custom, &*detected()?)
    }

    pub(crate) fn terminal(&self) -> Option<Result<Found, &'static Known>> {
        terminal(self.terminal.as_deref(), &*detected()?)
    }
}

fn typed(tab: &mut Tab, cx: &mut Context<Tab>) {
    let text = tab.ui.tools.command.read(cx).text().to_owned();
    if text != tab.ui.tools.custom {
        tab.ui.tools.custom = text.clone();
        Settings::update(cx, |s| s.editor_command = text);
        cx.notify();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Look,
    Editor(String),
    Terminal(String),
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Look => start(true, cx),
        Action::Editor(id) => {
            tab.ui.tools.editor = Some(id.clone());
            let custom = id == CUSTOM;
            Settings::update(cx, |s| s.editor = Some(id));
            if custom {
                tab.ui.tools.command.focus_handle(cx).focus(window);
            }
        }
        Action::Terminal(id) => {
            tab.ui.tools.terminal = Some(id.clone());
            Settings::update(cx, |s| s.terminal = Some(id));
        }
    }
    cx.notify();
}


pub(crate) fn file_title(ui: &Ui, line: Option<u32>) -> String {
    match ui.tools.editor() {
        Some(editor) => match line {
            Some(line) => format!("Open in {} at line {line}", editor_name(&editor)),
            None => format!("Open in {}", editor_name(&editor)),
        },
        None => "Open in external editor…".to_owned(),
    }
}

pub(crate) fn repo_title(ui: &Ui) -> String {
    match ui.tools.editor() {
        Some(editor) => format!("Open repository in {}", editor_name(&editor)),
        None => "Open repository in external editor…".to_owned(),
    }
}

pub(crate) fn terminal_title(ui: &Ui) -> String {
    match ui.tools.terminal() {
        Some(Ok(found)) => format!("Open in {}", found.tool.name),
        Some(Err(tool)) => format!("Open in {}", tool.name),
        None => "Open in terminal…".to_owned(),
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) dir: PathBuf,
}

impl Launch {
    fn open_with(app: &Path, target: &Path, dir: &Path) -> Self {
        Self {
            program: PathBuf::from("/usr/bin/open"),
            args: vec!["-a".into(), app.into(), target.into()],
            dir: dir.to_owned(),
        }
    }

    pub(crate) fn words(&self) -> Vec<String> {
        std::iter::once(self.program.to_string_lossy().into_owned())
            .chain(self.args.iter().map(|arg| arg.to_string_lossy().into_owned()))
            .collect()
    }
}

pub(crate) fn edit(editor: &Editor, root: &Path, file: Option<&Path>, line: Option<u32>, dirs: &[PathBuf]) -> Result<Launch, String> {
    let target = file.unwrap_or(root);
    let found = match editor {
        Editor::Found(found) => found,
        Editor::Gone(tool) => return Err(format!("{} is no longer installed", tool.name)),
        Editor::Custom(command) => return custom(command, root, target, line, dirs),
    };
    let program = match &found.how {
        How::App(app) => return Ok(Launch::open_with(app, target, root)),
        How::Program(program) => program.clone(),
    };
    let args: Vec<OsString> = match (file, line) {
        (Some(file), Some(line)) => match found.tool.lines {
            Lines::Goto => vec!["--goto".into(), at(file, line)],
            Lines::Suffix => vec![at(file, line)],
            Lines::Flag => vec!["--line".into(), line.to_string().into(), file.into()],
        },
        _ => vec![target.into()],
    };
    Ok(Launch { program, args, dir: root.to_owned() })
}

fn at(file: &Path, line: u32) -> OsString {
    let mut text = file.as_os_str().to_owned();
    text.push(format!(":{line}"));
    text
}

fn custom(command: &str, root: &Path, target: &Path, line: Option<u32>, dirs: &[PathBuf]) -> Result<Launch, String> {
    let words = words(command)?;
    let line = line.unwrap_or(1).to_string();
    let named = words.iter().any(|word| word.contains("{file}") || word.contains("{repo}"));
    let file = target.to_string_lossy();
    let repo = root.to_string_lossy();
    let mut filled: Vec<String> =
        words.iter().map(|word| word.replace("{file}", &file).replace("{line}", &line).replace("{repo}", &repo)).collect();
    if !named {
        filled.push(file.into_owned());
    }
    let mut filled = filled.into_iter();
    let program = filled.next().ok_or("The editor command is empty")?;
    let program = if let Some(rest) = program.strip_prefix("~/") {
        std::env::var_os("HOME").map(PathBuf::from).ok_or("Your home folder is not known")?.join(rest)
    } else if program.contains('/') {
        PathBuf::from(&program)
    } else {
        on_path(dirs, &program).ok_or_else(|| format!("{program} is not on your search path"))?
    };
    Ok(Launch { program, args: filled.map(OsString::from).collect(), dir: root.to_owned() })
}

pub(crate) fn words(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("The editor command has a quote that is not closed".into()),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c) => word.push(c),
                            None => return Err("The editor command ends in a backslash".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("The editor command has a quote that is not closed".into()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some(c) => word.get_or_insert_with(String::new).push(c),
                None => return Err("The editor command ends in a backslash".into()),
            },
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    if words.is_empty() {
        return Err("The editor command is empty".into());
    }
    Ok(words)
}

pub(crate) fn terminal_launch(terminal: &Found, root: &Path) -> Launch {
    match &terminal.how {
        How::App(app) => Launch::open_with(app, root, root),
        How::Program(program) => Launch { program: program.clone(), args: vec![root.into()], dir: root.to_owned() },
    }
}

static LAUNCHED: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

pub(crate) fn launched() -> Vec<Vec<String>> {
    LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn standing_in() -> bool {
    cfg!(test) || crate::acceptance::replaying()
}

pub(crate) fn record(words: Vec<String>) {
    LAUNCHED.lock().unwrap_or_else(|e| e.into_inner()).push(words);
}

pub(crate) fn run(launch: &Launch) -> Result<(), String> {
    if standing_in() {
        record(launch.words());
        return if runnable(&launch.program) {
            Ok(())
        } else {
            Err(format!("{} was not found", launch.program.display()))
        };
    }
    spawn(launch, crate::environment::host_dirs())
}

fn spawn(launch: &Launch, dirs: &[PathBuf]) -> Result<(), String> {
    let path = std::env::join_paths(dirs).unwrap_or_default();
    let mut said = tempfile::tempfile().map_err(|error| error.to_string())?;
    let stderr = said.try_clone().map_err(|error| error.to_string())?;
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .current_dir(&launch.dir)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + SETTLE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let mut text = String::new();
                if said.seek(SeekFrom::Start(0)).is_ok() {
                    let _ = (&mut said).take(64 * 1024).read_to_string(&mut text);
                }
                let text = text.trim();
                return Err(if text.is_empty() { format!("It stopped with {status}") } else { text.to_owned() });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}


fn current() -> Arc<Detected> {
    fresh().unwrap_or_else(look)
}

pub(crate) fn open_in_editor(tab: &mut Tab, file: Option<PathBuf>, line: Option<u32>, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(root) = tab.repo.as_ref().map(|repo| repo.root.clone()) else { return };
    let Some(editor) = editor(tab.ui.tools.editor.as_deref(), &tab.ui.tools.custom, &current()) else {
        let custom = tab.ui.tools.editor.as_deref() == Some(CUSTOM);
        let detail = if custom { "Type the command that opens your editor." } else { NONE_FOUND };
        return choose("an editor", detail, tab, window, cx);
    };
    let name = editor_name(&editor);
    let (dirs, _) = places();
    match edit(&editor, &root, file.as_deref(), line, &dirs) {
        Ok(launch) => start_launch(name, launch, cx),
        Err(error) => failed(tab, &name, error, cx),
    }
}

pub(crate) fn open_terminal(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    let Some(root) = tab.repo.as_ref().map(|repo| repo.root.clone()) else { return };
    match terminal(tab.ui.tools.terminal.as_deref(), &current()) {
        None => choose("a terminal", NONE_FOUND, tab, window, cx),
        Some(Err(tool)) => failed(tab, tool.name, format!("{} is no longer installed", tool.name), cx),
        Some(Ok(found)) => start_launch(found.tool.name.to_owned(), terminal_launch(&found, &root), cx),
    }
}

fn start_launch(name: String, launch: Launch, cx: &mut Context<Tab>) {
    cx.notify();
    Tab::work(cx, move || run(&launch), move |tab, result, cx| {
        if let Err(error) = result {
            failed(tab, &name, error, cx);
        }
    });
}

fn tools_section() -> view::Action {
    view::Action::Preferences(crate::preferences::Action::Open(Some(crate::preferences::Section::Tools)))
}

const NONE_FOUND: &str = "FastKraken found none it knows on this Mac.";

fn choose(what: &str, detail: &str, tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    crate::preferences::dispatch(tab, crate::preferences::Action::Open(Some(crate::preferences::Section::Tools)), window, cx);
    tab.ui.notices.show(Notice::new(Tone::Info, format!("Choose {what} first"), detail.to_owned()));
    cx.notify();
}

fn failed(tab: &mut Tab, name: &str, error: String, cx: &mut Context<Tab>) {
    tab.ui.notices.show(Notice::new(Tone::Error, format!("{name} did not open"), error).offer("External tools…", tools_section()));
    cx.notify();
}


pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    use crate::preferences::{group, radio, setting};
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let found = detected();
    let chosen = ui.tools.editor();
    let row = |id: (&'static str, usize), title: String, place: Option<String>, on: bool, action: view::Action, cx: &Context<T>| {
        let item = div()
            .id(id)
            .py(px(8.))
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(LINE))
            .cursor_pointer()
            .focusable()
            .focus(|s| s.bg(rgb(HOVER)))
            .child(radio(on))
            .child(div().w(px(130.)).flex_shrink_0().text_size(px(13.)).text_color(rgb(TEXT)).child(title))
            .child(div().flex_1().min_w_0().when_some(place, |d, place| d.child(kit::fit(place).size(TEXT_SMALL).color(TEXT_3))));
        kit::pressable(item, action, cx)
    };
    let mut editors = div().flex().flex_col();
    let mut terminals = div().flex().flex_col();
    match &found {
        None => {
            editors = editors.child(div().py_2().child(kit::muted("Looking for editors…")));
            terminals = terminals.child(div().py_2().child(kit::muted("Looking for terminals…")));
        }
        Some(found) => {
            if found.editors.is_empty() {
                editors = editors.child(div().py_2().child(kit::muted(
                    "No editor was found on your search path or in Applications. FastKraken looks for VS Code, Cursor, Zed, Sublime Text, IntelliJ IDEA and Xcode.",
                )));
            }
            for (index, tool) in found.editors.iter().enumerate() {
                let on = matches!(&chosen, Some(Editor::Found(shown)) if shown == tool);
                let place = crate::start::homely(tool.place(), home.as_deref());
                editors = editors.child(row(
                    ("tools-editor", index),
                    tool.tool.name.to_owned(),
                    Some(place),
                    on,
                    view::Action::Tools(Action::Editor(tool.tool.id.to_owned())),
                    cx,
                ));
            }
            if let Some(Editor::Gone(tool)) = &chosen {
                editors = editors.child(row(
                    ("tools-editor-gone", 0),
                    tool.name.to_owned(),
                    Some("Not found any more".to_owned()),
                    true,
                    view::Action::Tools(Action::Editor(tool.id.to_owned())),
                    cx,
                ));
            }
            if found.terminals.is_empty() {
                terminals = terminals.child(div().py_2().child(kit::muted("No terminal was found in Applications.")));
            }
            let terminal = ui.tools.terminal();
            for (index, tool) in found.terminals.iter().enumerate() {
                let on = matches!(&terminal, Some(Ok(shown)) if shown == tool);
                let place = crate::start::homely(tool.place(), home.as_deref());
                terminals = terminals.child(row(
                    ("tools-terminal", index),
                    tool.tool.name.to_owned(),
                    Some(place),
                    on,
                    view::Action::Tools(Action::Terminal(tool.tool.id.to_owned())),
                    cx,
                ));
            }
            if let Some(Err(tool)) = &terminal {
                terminals = terminals.child(row(
                    ("tools-terminal-gone", 0),
                    tool.name.to_owned(),
                    Some("Not found any more".to_owned()),
                    true,
                    view::Action::Tools(Action::Terminal(tool.id.to_owned())),
                    cx,
                ));
            }
        }
    }
    let custom_on = ui.tools.editor.as_deref() == Some(CUSTOM);
    let problem = custom_on.then(|| words(&ui.tools.custom).err()).flatten();
    let custom = row(
        ("tools-editor-custom", 0),
        "Your own command".to_owned(),
        None,
        custom_on,
        view::Action::Tools(Action::Editor(CUSTOM.to_owned())),
        cx,
    );
    let help = problem.unwrap_or_else(|| {
        "{file} is the file, or the repository's folder when you open the repository; {line} is the line, or 1; {repo} is the repository's folder. Without {file} or {repo}, the path goes last.".to_owned()
    });
    editors = editors.child(custom).child(setting(
        "Command",
        Some(help.into()),
        div().w(px(260.)).child(ui.tools.command.clone()),
    ));
    div()
        .flex()
        .flex_col()
        .child(kit::muted("Files and the repository open in the editor (Cmd-Shift-E), and Option-T opens the terminal at the repository's folder."))
        .child(group("EDITOR"))
        .child(editors)
        .child(group("TERMINAL"))
        .child(terminals)
        .child(div().pt_3().flex().child(kit::link(
            "tools-look",
            "Look again",
            view::Action::Tools(Action::Look),
            cx,
        )))
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    !ui.preferences.shows_tools() && ui.tools.command.focus_handle(cx).is_focused(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    struct Mac {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        apps: PathBuf,
    }

    impl Mac {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            let apps = dir.path().join("Applications");
            fs::create_dir_all(&bin).unwrap();
            fs::create_dir_all(&apps).unwrap();
            Self { bin, apps, _dir: dir }
        }

        fn program(&self, path: &Path) -> PathBuf {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_owned()
        }

        fn command(&self, name: &str) -> PathBuf {
            self.program(&self.bin.join(name))
        }

        fn app(&self, name: &str) -> PathBuf {
            let app = self.apps.join(name);
            fs::create_dir_all(app.join("Contents")).unwrap();
            app
        }

        fn detect(&self) -> Detected {
            detect(&[self.bin.clone()], &[self.apps.clone()])
        }
    }

    fn ids(found: &[Found]) -> Vec<&'static str> {
        found.iter().map(|found| found.tool.id).collect()
    }

    #[test]
    fn editors_are_found_by_command_or_application_and_terminals_by_application() {
        let mac = Mac::new();
        assert_eq!(mac.detect(), Detected::default());
        let code = mac.command("code");
        fs::write(mac.bin.join("subl"), "").unwrap();
        let zed = mac.app("Zed.app");
        let zed_cli = mac.program(&zed.join("Contents/MacOS/cli"));
        let idea = mac.app("IntelliJ IDEA CE.app");
        mac.app("iTerm.app");
        mac.app("Terminal.app");
        mac.command("xed");
        let found = mac.detect();
        assert_eq!(ids(&found.editors), ["code", "zed", "idea"]);
        assert_eq!(found.editors[0].how, How::Program(code));
        assert_eq!(found.editors[1].how, How::Program(zed_cli));
        assert_eq!(found.editors[2].how, How::App(idea));
        assert_eq!(ids(&found.terminals), ["terminal", "iterm"]);
        assert_eq!(fs::read_to_string(mac.bin.join("code")).unwrap(), "#!/bin/sh\nexit 0\n");
    }

    #[test]
    fn the_editor_is_the_chosen_one_or_the_first_found() {
        let mac = Mac::new();
        mac.command("code");
        mac.command("zed");
        let found = mac.detect();
        let id = |editor: Option<Editor>| match editor {
            Some(Editor::Found(found)) => found.tool.id.to_owned(),
            other => format!("{other:?}"),
        };
        assert_eq!(id(editor(None, "", &found)), "code");
        assert_eq!(id(editor(Some("zed"), "", &found)), "zed");
        assert_eq!(editor(Some("subl"), "", &found), Some(Editor::Gone(&EDITORS[3])));
        assert_eq!(id(editor(Some("emacs"), "", &found)), "code");
        assert_eq!(editor(Some(CUSTOM), " nvim {file} ", &found), Some(Editor::Custom("nvim {file}".into())));
        assert_eq!(editor(Some(CUSTOM), "  ", &found), None);
        assert_eq!(editor(None, "", &Detected::default()), None);
        assert_eq!(editor_name(&Editor::Custom("/opt/bin/nvim +{line} {file}".into())), "nvim");
        assert_eq!(editor_name(&Editor::Custom("open -a BBEdit {file}".into())), "your editor");
    }

    #[test]
    fn the_terminal_is_the_chosen_one_or_terminal() {
        let mac = Mac::new();
        mac.app("Terminal.app");
        mac.app("Ghostty.app");
        let found = mac.detect();
        assert_eq!(terminal(None, &found).unwrap().unwrap().tool.id, "terminal");
        assert_eq!(terminal(Some("ghostty"), &found).unwrap().unwrap().tool.id, "ghostty");
        assert_eq!(terminal(Some("iterm"), &found), Some(Err(&TERMINALS[1])));
        assert_eq!(terminal(Some("nonesuch"), &found).unwrap().unwrap().tool.id, "terminal");
        assert_eq!(terminal(None, &Detected::default()), None);
    }

    #[test]
    fn each_editor_is_given_the_line_its_own_way() {
        let root = Path::new("/r/Cedar");
        let file = Path::new("/r/Cedar/src/main.rs");
        let launch = |id: &str, how: How, line: Option<u32>, file: Option<&Path>| {
            let tool = EDITORS.iter().find(|tool| tool.id == id).unwrap();
            let launch = edit(&Editor::Found(Found { tool, how }), root, file, line, &[]).unwrap();
            assert_eq!(launch.dir, root);
            launch.words()
        };
        let program = |name: &str| How::Program(PathBuf::from(format!("/bin/{name}")));
        assert_eq!(launch("code", program("code"), Some(12), Some(file)), ["/bin/code", "--goto", "/r/Cedar/src/main.rs:12"]);
        assert_eq!(launch("zed", program("zed"), Some(12), Some(file)), ["/bin/zed", "/r/Cedar/src/main.rs:12"]);
        assert_eq!(launch("idea", program("idea"), Some(12), Some(file)), ["/bin/idea", "--line", "12", "/r/Cedar/src/main.rs"]);
        assert_eq!(launch("code", program("code"), None, Some(file)), ["/bin/code", "/r/Cedar/src/main.rs"]);
        assert_eq!(launch("subl", program("subl"), None, None), ["/bin/subl", "/r/Cedar"]);
        let app = How::App(PathBuf::from("/Applications/Xcode.app"));
        assert_eq!(launch("xed", app, Some(3), Some(file)), ["/usr/bin/open", "-a", "/Applications/Xcode.app", "/r/Cedar/src/main.rs"]);
        let gone = edit(&Editor::Gone(&EDITORS[0]), root, None, None, &[]);
        assert_eq!(gone, Err("VS Code is no longer installed".to_owned()));
    }

    #[test]
    fn a_custom_command_fills_its_placeholders_and_finds_its_program() {
        let mac = Mac::new();
        let nvim = mac.command("nvim");
        let dirs = [mac.bin.clone()];
        let root = Path::new("/r/My Cedar");
        let file = Path::new("/r/My Cedar/a b.txt");
        let run = |command: &str, file: Option<&Path>, line| {
            edit(&Editor::Custom(command.into()), root, file, line, &dirs).map(|launch| launch.words())
        };
        let nvim = nvim.to_string_lossy().into_owned();
        assert_eq!(run("nvim +{line} {file}", Some(file), Some(7)).unwrap(), [nvim.as_str(), "+7", "/r/My Cedar/a b.txt"]);
        assert_eq!(run("nvim +{line} {file}", None, None).unwrap(), [nvim.as_str(), "+1", "/r/My Cedar"]);
        assert_eq!(run("nvim --cmd 'cd {repo}' {file}", Some(file), None).unwrap(), [nvim.as_str(), "--cmd", "cd /r/My Cedar", "/r/My Cedar/a b.txt"]);
        assert_eq!(run("nvim -R", Some(file), Some(2)).unwrap(), [nvim.as_str(), "-R", "/r/My Cedar/a b.txt"]);
        assert_eq!(run("/opt/ed {file}", Some(file), None).unwrap(), ["/opt/ed", "/r/My Cedar/a b.txt"]);
        assert_eq!(run("emacsclient {file}", Some(file), None), Err("emacsclient is not on your search path".into()));
        assert_eq!(run("nvim \"{file}\"; echo gone", Some(file), None).unwrap(), [nvim.as_str(), "/r/My Cedar/a b.txt;", "echo", "gone"]);
    }

    #[test]
    fn command_words_split_as_a_shell_splits_them() {
        assert_eq!(words("code  --goto {file}:{line}").unwrap(), ["code", "--goto", "{file}:{line}"]);
        assert_eq!(words(r#""/Applications/My Ed/ed" 'a b' c\ d "e\"f""#).unwrap(), ["/Applications/My Ed/ed", "a b", "c d", "e\"f"]);
        assert_eq!(words("ed ''").unwrap(), ["ed", ""]);
        assert!(words("ed 'open").is_err());
        assert!(words("ed \"open").is_err());
        assert!(words("ed \\").is_err());
        assert_eq!(words("   "), Err("The editor command is empty".into()));
    }

    #[test]
    fn a_terminal_opens_at_the_repository() {
        let found = Found { tool: &TERMINALS[1], how: How::App(PathBuf::from("/Applications/iTerm.app")) };
        let launch = terminal_launch(&found, Path::new("/tmp/Session"));
        assert_eq!(launch.words(), ["/usr/bin/open", "-a", "/Applications/iTerm.app", "/tmp/Session"]);
        assert_eq!(launch.dir, Path::new("/tmp/Session"));
    }

    #[test]
    fn a_launcher_that_leaves_its_editor_running_does_not_take_it_down() {
        let mac = Mac::new();
        let marker = mac.bin.join("still-running");
        let launcher = mac.bin.join("launcher");
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\n( /bin/sleep 0.3; echo 'editor log' >&2; echo alive > '{}' ) &\nexit 0\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Launch { program: launcher, args: vec![], dir: mac.bin.clone() };
        assert_eq!(spawn(&launch, &[]), Ok(()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fs::read_to_string(&marker).unwrap_or_default(), "alive\n");
        let failing = mac.bin.join("failing");
        fs::write(&failing, "#!/bin/sh\necho 'no such window' >&2\nexit 3\n").unwrap();
        fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Launch { program: failing, args: vec![], dir: mac.bin.clone() };
        assert_eq!(spawn(&launch, &[]), Err("no such window".to_owned()));
    }

    #[test]
    fn under_test_nothing_is_started_and_a_missing_program_fails() {
        let f = Fixture::new();
        let mac = Mac::new();
        let marker = mac.command("tools-marker-editor");
        let refs = f.git(&["for-each-ref"]);
        let launch = Launch { program: marker.clone(), args: vec!["--marker".into()], dir: f.repo.root.clone() };
        assert_eq!(run(&launch), Ok(()));
        let kept = launched();
        assert!(kept.iter().any(|words| *words == [marker.to_string_lossy().as_ref(), "--marker"]), "{kept:?}");
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert_eq!(f.git(&["for-each-ref"]), refs);
        let gone = Launch { program: mac.bin.join("tools-marker-gone"), args: vec![], dir: f.repo.root.clone() };
        assert!(run(&gone).unwrap_err().ends_with("tools-marker-gone was not found"));
    }
}
