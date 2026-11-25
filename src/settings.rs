use crate::{refs::Section, storage, sync::PullDefault, view::ColumnWidths, view::DiffMode};
use gpui::{App, Bounds, Global, Pixels, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    ffi::OsStr,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

const SETTINGS: &str = "settings.json";
const MEMORY: &str = "state.json";
const LOCK: &str = "state.lock";
const PAUSE: Duration = Duration::from_millis(500);
const REPOSITORIES: usize = 64;
pub(crate) const RECENTS: usize = 30;
pub(crate) const FETCH_MINUTES: u32 = 5;
pub(crate) const MAX_FETCH_MINUTES: u32 = 60;
pub(crate) const MIN_WINDOW: (f32, f32) = (860., 520.);

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Settings {
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) diff_mode: DiffMode,
    pub(crate) show_ref_labels: bool,
    pub(crate) sidebar_hidden: bool,
    pub(crate) panel_hidden: bool,
    #[serde(default)]
    pub(crate) push_new_tags: bool,
    #[serde(default)]
    pub(crate) stash_in_the_way: bool,
    pub(crate) prune_on_fetch: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) default_branch: String,
    pub(crate) reopen_last: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) fetch_every: Option<u32>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) clone_folder: Option<Vec<u8>>,
    pub(crate) login_path: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) editor: Option<String>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) editor_command: String,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) terminal: Option<String>,
    pub(crate) push_after_commit: bool,
    pub(crate) fill_from_template: bool,
    pub(crate) colour_code: bool,
    pub(crate) whole_file: bool,
    pub(crate) ignore_whitespace: bool,
    pub(crate) wrap_lines: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) diff_font: Option<String>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) diff_font_size: Option<u32>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) tab_width: Option<u32>,
    pub(crate) line_numbers: bool,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) graph_columns: crate::columns::Layout,
}

impl Settings {
    pub(crate) fn default_branch(&self) -> String {
        match self.default_branch.trim() {
            "" => "main".to_owned(),
            name => name.to_owned(),
        }
    }

    pub(crate) fn fetch_minutes(&self) -> u32 {
        self.fetch_every.unwrap_or(FETCH_MINUTES).min(MAX_FETCH_MINUTES)
    }

    pub(crate) fn clone_folder(&self) -> Option<PathBuf> {
        self.clone_folder.as_ref().map(|bytes| PathBuf::from(OsStr::from_bytes(bytes)))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            diff_mode: DiffMode::default(),
            show_ref_labels: true,
            sidebar_hidden: false,
            panel_hidden: false,
            push_new_tags: false,
            stash_in_the_way: false,
            prune_on_fetch: true,
            default_branch: "main".to_owned(),
            reopen_last: true,
            fetch_every: None,
            clone_folder: None,
            login_path: true,
            editor: None,
            editor_command: String::new(),
            terminal: None,
            push_after_commit: false,
            fill_from_template: true,
            colour_code: true,
            whole_file: false,
            ignore_whitespace: false,
            wrap_lines: false,
            diff_font: None,
            diff_font_size: None,
            tab_width: None,
            line_numbers: true,
            graph_columns: crate::columns::Layout::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Memory {
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) window: Option<Frame>,
    pub(crate) last_repository: Option<Vec<u8>>,
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) session: Vec<Vec<u8>>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) columns: Option<ColumnWidths>,
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) repositories: Vec<RepoView>,
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) recents: Vec<Recent>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) clone_parent: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Recent {
    pub(crate) path: Vec<u8>,
    pub(crate) name: String,
    pub(crate) opened: i64,
}

impl Recent {
    pub(crate) fn path(&self) -> PathBuf {
        PathBuf::from(OsStr::from_bytes(&self.path))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct RepoView {
    pub(crate) directory: Vec<u8>,
    #[serde(deserialize_with = "storage::lenient_list")]
    pub(crate) closed: Vec<Section>,
    pub(crate) folded: Vec<String>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) pull: Option<PullDefault>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) merge: Option<crate::merge::Mode>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) push_after: Option<bool>,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) graph: Option<crate::graph_view::GraphView>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FrameMode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Frame {
    pub(crate) display: Option<String>,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    #[serde(deserialize_with = "storage::lenient")]
    pub(crate) mode: FrameMode,
}

impl Frame {
    pub(crate) fn new(bounds: WindowBounds, display: Option<String>) -> Self {
        let (mode, frame) = match bounds {
            WindowBounds::Windowed(b) => (FrameMode::Windowed, b),
            WindowBounds::Maximized(b) => (FrameMode::Maximized, b),
            WindowBounds::Fullscreen(b) => (FrameMode::Fullscreen, b),
        };
        Self {
            display,
            x: frame.origin.x.into(),
            y: frame.origin.y.into(),
            width: frame.size.width.into(),
            height: frame.size.height.into(),
            mode,
        }
    }

    pub(crate) fn place<D>(
        &self,
        displays: impl IntoIterator<Item = (D, Option<String>, Bounds<Pixels>)>,
        primary: Option<(D, Bounds<Pixels>)>,
    ) -> Option<(D, WindowBounds)> {
        let numbers = [self.x, self.y, self.width, self.height];
        if numbers.iter().any(|n| !n.is_finite()) {
            return None;
        }
        let (display, screen) = match &self.display {
            Some(uuid) => displays
                .into_iter()
                .find(|(_, id, _)| id.as_ref() == Some(uuid))
                .map(|(display, _, screen)| (display, screen))?,
            None => primary?,
        };
        let (screen_w, screen_h) = (f32::from(screen.size.width), f32::from(screen.size.height));
        let width = self.width.max(MIN_WINDOW.0).min(screen_w.max(MIN_WINDOW.0));
        let height = self.height.max(MIN_WINDOW.1).min(screen_h.max(MIN_WINDOW.1));
        let visible = (self.x + width).min(screen_w) - self.x.max(0.);
        if visible < 120. || self.y < -1. || self.y > screen_h - 40. {
            return None;
        }
        let frame = Bounds::new(point(px(self.x), px(self.y)), size(px(width), px(height)));
        Some((
            display,
            match self.mode {
                FrameMode::Windowed => WindowBounds::Windowed(frame),
                FrameMode::Maximized => WindowBounds::Maximized(frame),
                FrameMode::Fullscreen => WindowBounds::Fullscreen(frame),
            },
        ))
    }
}

fn key(common_dir: &Path) -> Vec<u8> {
    common_dir.as_os_str().as_bytes().to_vec()
}

impl Memory {
    pub(crate) fn repo(&self, common_dir: &Path) -> Option<&RepoView> {
        let key = key(common_dir);
        self.repositories.iter().find(|view| view.directory == key)
    }

    pub(crate) fn change_repo(&mut self, common_dir: &Path, change: impl FnOnce(&mut RepoView)) {
        let key = key(common_dir);
        let mut view = match self.repositories.iter().position(|view| view.directory == key) {
            Some(index) => self.repositories.remove(index),
            None => RepoView {
                directory: key,
                ..RepoView::default()
            },
        };
        change(&mut view);
        if !view.closed.is_empty()
            || !view.folded.is_empty()
            || view.pull.is_some()
            || view.merge.is_some()
            || view.push_after.is_some()
            || view.graph.is_some()
        {
            self.repositories.push(view);
        }
        let excess = self.repositories.len().saturating_sub(REPOSITORIES);
        self.repositories.drain(..excess);
    }

    pub(crate) fn last_repository(&self) -> Option<PathBuf> {
        self.last_repository
            .as_ref()
            .map(|bytes| PathBuf::from(OsStr::from_bytes(bytes)))
    }

    pub(crate) fn session(&self) -> Vec<PathBuf> {
        if self.session.is_empty() {
            return self.last_repository().into_iter().collect();
        }
        self.session.iter().map(|bytes| PathBuf::from(OsStr::from_bytes(bytes))).collect()
    }

    pub(crate) fn set_session(&mut self, open: &[PathBuf], front: Option<&Path>) {
        self.session = open.iter().map(|root| root.as_os_str().as_bytes().to_vec()).collect();
        self.last_repository = front.map(|root| root.as_os_str().as_bytes().to_vec());
    }

    pub(crate) fn add_recent(&mut self, root: &Path, now: i64) {
        let path = root.as_os_str().as_bytes().to_vec();
        self.recents.retain(|recent| recent.path != path);
        let name = root.file_name().unwrap_or(root.as_os_str()).to_string_lossy().into_owned();
        self.recents.insert(0, Recent { path, name, opened: now });
        self.recents.truncate(RECENTS);
    }

    pub(crate) fn is_recent(&self, path: &Path) -> bool {
        let path = path.as_os_str().as_bytes();
        self.recents.iter().any(|recent| recent.path == path)
    }

    pub(crate) fn remove_recent(&mut self, path: &Path) {
        let path = path.as_os_str().as_bytes();
        self.recents.retain(|recent| recent.path != path);
    }

    pub(crate) fn clear_recents(&mut self) -> Vec<Recent> {
        std::mem::take(&mut self.recents)
    }

    pub(crate) fn restore_recents(&mut self, earlier: Vec<Recent>) {
        for recent in earlier {
            if !self.recents.iter().any(|kept| kept.path == recent.path) {
                self.recents.push(recent);
            }
        }
        self.recents.truncate(RECENTS);
    }

    pub(crate) fn set_clone_parent(&mut self, parent: &Path) {
        self.clone_parent = Some(parent.as_os_str().as_bytes().to_vec());
    }

    pub(crate) fn clone_parent(&self) -> Option<PathBuf> {
        self.clone_parent.as_ref().map(|bytes| PathBuf::from(OsStr::from_bytes(bytes)))
    }
}

struct Store {
    dir: Option<PathBuf>,
    settings: Settings,
    memory: Memory,
    generation: u64,
    written: u64,
    startup_notice: Option<String>,
}

impl Global for Store {}

struct Written {
    generation: u64,
    base: Option<(Settings, Memory)>,
}

static WRITTEN: Mutex<Written> = Mutex::new(Written { generation: 0, base: None });

pub(crate) fn load(dir: Option<PathBuf>, cx: &mut App) {
    let (settings, memory, notice) = match &dir {
        Some(dir) => read(dir),
        None => (Settings::default(), Memory::default(), None),
    };
    let written = {
        let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        written.base = Some((settings.clone(), memory.clone()));
        written.generation
    };
    cx.set_global(Store {
        dir,
        settings,
        memory,
        generation: written,
        written,
        startup_notice: notice,
    });
}

fn read(dir: &Path) -> (Settings, Memory, Option<String>) {
    let (mut settings, first) = storage::load_or_set_aside::<Settings>(dir, SETTINGS);
    settings.graph_columns = settings.graph_columns.sanitized();
    let (mut memory, second) = storage::load_or_set_aside::<Memory>(dir, MEMORY);
    memory.columns = memory.columns.map(ColumnWidths::sanitized);
    let notice = match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    };
    (settings, memory, notice)
}

pub(crate) fn settings(cx: &App) -> Settings {
    cx.try_global::<Store>()
        .map(|store| store.settings.clone())
        .unwrap_or_default()
}

pub(crate) fn memory(cx: &App) -> Option<&Memory> {
    cx.try_global::<Store>().map(|store| &store.memory)
}

pub(crate) fn on_disk(cx: &App) -> bool {
    cx.try_global::<Store>().is_some_and(|store| store.dir.is_some())
}

pub(crate) fn take_startup_notice(cx: &mut App) -> Option<String> {
    if !cx.has_global::<Store>() {
        return None;
    }
    cx.global_mut::<Store>().startup_notice.take()
}

impl Settings {
    pub(crate) fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        change_store(cx, |store| change(&mut store.settings));
    }
}

impl Memory {
    pub(crate) fn update(cx: &mut App, change: impl FnOnce(&mut Memory)) {
        change_store(cx, |store| change(&mut store.memory));
    }
}

pub(crate) fn remember_window(window: &Window, cx: &mut App) {
    let display = window
        .display(cx)
        .and_then(|display| display.uuid().ok())
        .map(|uuid| uuid.to_string());
    let frame = Frame::new(window.window_bounds(), display);
    Memory::update(cx, |memory| memory.window = Some(frame));
}

pub(crate) fn placement(cx: &App) -> Option<(gpui::DisplayId, WindowBounds)> {
    let frame = memory(cx)?.window.as_ref()?;
    let displays = cx
        .displays()
        .into_iter()
        .map(|d| (d.id(), d.uuid().ok().map(|uuid| uuid.to_string()), d.bounds()));
    let primary = cx.primary_display().map(|display| (display.id(), display.bounds()));
    frame.place(displays, primary)
}

fn change_store(cx: &mut App, change: impl FnOnce(&mut Store)) {
    if !cx.has_global::<Store>() {
        return;
    }
    let store = cx.global_mut::<Store>();
    let before = (store.settings.clone(), store.memory.clone());
    change(store);
    if before == (store.settings.clone(), store.memory.clone()) || store.dir.is_none() {
        return;
    }
    store.generation += 1;
    let generation = store.generation;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(PAUSE).await;
        let _ = cx.update(|cx| save(cx, generation, false));
    })
    .detach();
}

fn save(cx: &mut App, generation: u64, now: bool) {
    let Some(store) = cx.try_global::<Store>() else {
        return;
    };
    if store.generation != generation || (!now && store.written >= generation) {
        return;
    }
    let Some(dir) = store.dir.clone() else {
        return;
    };
    let snapshot = (store.settings.clone(), store.memory.clone());
    cx.global_mut::<Store>().written = generation;
    let write = move || {
        let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
        if written.generation >= generation {
            return;
        }
        match write_files(&dir, &snapshot.0, &snapshot.1, written.base.as_ref()) {
            Ok(()) => written.base = Some(snapshot),
            Err(err) => eprintln!("FastKraken could not save its settings: {err}"),
        }
        written.generation = generation;
    };
    if now {
        write();
    } else {
        cx.background_executor().spawn(async move { write() }).detach();
    }
}

fn write_files(
    dir: &Path,
    settings: &Settings,
    memory: &Memory,
    base: Option<&(Settings, Memory)>,
) -> Result<(), String> {
    let _guard = storage::lock(dir, LOCK)?;
    let settings = merged(dir, SETTINGS, settings, base.map(|base| &base.0))?;
    storage::write(dir, SETTINGS, "settings", &settings)?;
    let memory = merged(dir, MEMORY, memory, base.map(|base| &base.1))?;
    storage::write(dir, MEMORY, "remembered state", &memory)
}

fn merged<T: Serialize>(
    dir: &Path,
    name: &str,
    ours: &T,
    base: Option<&T>,
) -> Result<Value, String> {
    let object = |value: Value| match value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let ours = object(serde_json::to_value(ours).map_err(|e| e.to_string())?);
    let base = match base {
        Some(base) => Some(object(serde_json::to_value(base).map_err(|e| e.to_string())?)),
        None => None,
    };
    let mut file = fs::read(dir.join(name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .map(object)
        .unwrap_or_default();
    for (key, value) in ours {
        let before = base.as_ref().and_then(|base| base.get(&key));
        let value = match (key.as_str(), file.get(&key)) {
            (_, None) => value,
            ("repositories", Some(theirs)) => repositories(&value, before, theirs),
            (_, Some(theirs)) if before == Some(&value) => theirs.clone(),
            _ => value,
        };
        file.insert(key, value);
    }
    Ok(Value::Object(file))
}

fn repositories(ours: &Value, before: Option<&Value>, theirs: &Value) -> Value {
    let (Some(ours), Some(theirs)) = (ours.as_array(), theirs.as_array()) else {
        return ours.clone();
    };
    let before = before.and_then(Value::as_array).cloned().unwrap_or_default();
    let key = |entry: &Value| entry.get("directory").cloned();
    let mut merged = theirs.clone();
    for old in &before {
        if !ours.iter().any(|entry| key(entry) == key(old)) {
            merged.retain(|entry| key(entry) != key(old));
        }
    }
    for entry in ours {
        if !before.contains(entry) {
            merged.retain(|other| key(other) != key(entry));
            merged.push(entry.clone());
        }
    }
    let excess = merged.len().saturating_sub(REPOSITORIES);
    merged.drain(..excess);
    Value::Array(merged)
}

pub(crate) fn flush(cx: &mut App) {
    let Some(generation) = cx.try_global::<Store>().map(|store| store.generation) else {
        return;
    };
    save(cx, generation, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{repository::tests::Fixture, storage::tests::Folder};
    use std::fs;

    fn sample_memory() -> Memory {
        let mut memory = Memory {
            window: Some(Frame {
                display: Some("3C1F6A2E-0000-4000-8000-00000000CAFE".into()),
                x: 80.,
                y: 40.,
                width: 1300.,
                height: 820.,
                mode: FrameMode::Maximized,
            }),
            last_repository: Some(b"/tmp/Cedar".to_vec()),
            session: vec![b"/tmp/Oak".to_vec(), b"/tmp/Cedar".to_vec()],
            columns: Some(ColumnWidths::new(240., 330.)),
            repositories: Vec::new(),
            recents: Vec::new(),
            clone_parent: Some(b"/tmp/clones".to_vec()),
        };
        memory.add_recent(Path::new("/tmp/Oak"), 1_795_000_000);
        memory.add_recent(Path::new("/tmp/Cedar"), 1_795_000_100);
        memory.change_repo(Path::new("/tmp/Cedar/.git"), |view| {
            view.closed = vec![Section::Tags, Section::Stashes];
            view.folded = vec!["origin".into(), "feature".into()];
        });
        memory.change_repo(Path::new("/tmp/Oak/.git"), |view| {
            view.pull = Some(PullDefault { fetch: false, strategy: crate::sync::Strategy::Rebase });
        });
        memory.change_repo(Path::new("/tmp/Elm/.git"), |view| view.merge = Some(crate::merge::Mode::Squash));
        memory.change_repo(Path::new("/tmp/Ash/.git"), |view| view.push_after = Some(false));
        memory
    }

    #[test]
    fn settings_and_memory_round_trip() {
        let folder = Folder::new();
        let settings = Settings {
            diff_mode: DiffMode::Split,
            show_ref_labels: false,
            sidebar_hidden: true,
            panel_hidden: true,
            push_new_tags: true,
            stash_in_the_way: true,
            prune_on_fetch: false,
            default_branch: "trunk".to_owned(),
            reopen_last: false,
            fetch_every: Some(0),
            clone_folder: Some(b"/tmp/clones".to_vec()),
            login_path: false,
            editor: Some("custom".to_owned()),
            editor_command: "nvim +{line} {file}".to_owned(),
            terminal: Some("iterm".to_owned()),
            push_after_commit: true,
            fill_from_template: false,
            colour_code: false,
            whole_file: true,
            ignore_whitespace: true,
            wrap_lines: true,
            diff_font: Some("Monaco".to_owned()),
            diff_font_size: Some(15),
            tab_width: Some(2),
            line_numbers: false,
            graph_columns: crate::columns::Layout {
                hidden: vec![crate::columns::Column::Author],
                graph: Some(210.),
                author: 150.,
                date: 96.,
                sha: 70.,
            },
        };
        let memory = sample_memory();
        write_files(&folder.0, &settings, &memory, None).unwrap();
        let (loaded_settings, loaded_memory, notice) = read(&folder.0);
        assert_eq!(loaded_settings, settings);
        assert_eq!(loaded_memory, memory);
        assert_eq!(notice, None);
        assert_eq!(loaded_memory.last_repository(), Some(PathBuf::from("/tmp/Cedar")));
        assert_eq!(loaded_memory.session(), [PathBuf::from("/tmp/Oak"), PathBuf::from("/tmp/Cedar")]);
        assert_eq!(loaded_memory.recents.iter().map(|recent| recent.name.as_str()).collect::<Vec<_>>(), ["Cedar", "Oak"]);
        assert_eq!(loaded_memory.clone_parent(), Some(PathBuf::from("/tmp/clones")));
        assert_eq!(
            loaded_memory.repo(Path::new("/tmp/Cedar/.git")).unwrap().closed,
            [Section::Tags, Section::Stashes]
        );
        assert_eq!(loaded_memory.repo(Path::new("/tmp/Elm/.git")).unwrap().merge, Some(crate::merge::Mode::Squash));
        assert_eq!(loaded_memory.repo(Path::new("/tmp/Ash/.git")).unwrap().push_after, Some(false));
        assert_eq!(loaded_memory.repo(Path::new("/tmp/Oak/.git")).unwrap().merge, None);
    }

    #[test]
    fn the_session_is_the_open_tabs_or_an_older_file_s_last_repository() {
        let older: Memory = serde_json::from_str(r#"{"last_repository": [47, 114, 47, 79, 97, 107]}"#).unwrap();
        assert_eq!(older.session(), [PathBuf::from("/r/Oak")]);
        let mut memory = Memory::default();
        assert!(memory.session().is_empty());
        memory.set_session(&[PathBuf::from("/r/Oak"), PathBuf::from("/r/Elm")], Some(Path::new("/r/Elm")));
        assert_eq!(memory.session(), [PathBuf::from("/r/Oak"), PathBuf::from("/r/Elm")]);
        assert_eq!(memory.last_repository(), Some(PathBuf::from("/r/Elm")));
        memory.set_session(&[PathBuf::from("/r/Oak")], None);
        assert_eq!((memory.session(), memory.last_repository()), (vec![PathBuf::from("/r/Oak")], None));
        let damaged: Memory = serde_json::from_str(r#"{"session": 7, "recents": []}"#).unwrap();
        assert!(damaged.session.is_empty());
        memory.set_session(&[], None);
        assert!(memory.session().is_empty() && memory.last_repository.is_none());
    }

    #[test]
    fn recent_repositories_are_newest_first_once_each_and_capped() {
        let mut memory = Memory::default();
        memory.add_recent(Path::new("/r/Oak"), 10);
        memory.add_recent(Path::new("/r/Cedar"), 20);
        memory.add_recent(Path::new("/r/Oak"), 30);
        let listed: Vec<_> = memory.recents.iter().map(|recent| (recent.name.as_str(), recent.opened)).collect();
        assert_eq!(listed, [("Oak", 30), ("Cedar", 20)]);
        assert_eq!(memory.recents[0].path(), PathBuf::from("/r/Oak"));
        for n in 0..RECENTS + 5 {
            memory.add_recent(Path::new(&format!("/r/{n}")), 100 + n as i64);
        }
        assert_eq!(memory.recents.len(), RECENTS);
        assert_eq!(memory.recents[0].name, (RECENTS + 4).to_string());
        assert!(!memory.recents.iter().any(|recent| recent.name == "Cedar"));
        assert!(memory.is_recent(Path::new("/r/10")));
        memory.remove_recent(Path::new("/r/10"));
        assert!(!memory.is_recent(Path::new("/r/10")));
        assert_eq!(memory.recents.len(), RECENTS - 1);
        assert_eq!(memory.recents[0].name, (RECENTS + 4).to_string());
        let cleared = memory.clear_recents();
        assert!(memory.recents.is_empty() && cleared.len() == RECENTS - 1);
        memory.add_recent(Path::new("/r/Elm"), 500);
        memory.add_recent(Path::new("/r/12"), 501);
        memory.restore_recents(cleared);
        assert_eq!(memory.recents.len(), RECENTS);
        assert_eq!(memory.recents[0].name, "12");
        assert_eq!(memory.recents[1].name, "Elm");
        assert_eq!(memory.recents[2].name, (RECENTS + 4).to_string());
        assert_eq!(memory.recents.iter().filter(|recent| recent.name == "12").count(), 1);
    }

    #[test]
    fn an_empty_or_unreadable_default_branch_is_main() {
        let named = |name: &str| Settings { default_branch: name.to_owned(), ..Settings::default() };
        assert_eq!(Settings::default().default_branch(), "main");
        assert_eq!(named("  ").default_branch(), "main");
        assert_eq!(named(" trunk ").default_branch(), "trunk");
        let read: Settings = serde_json::from_str(r#"{"default_branch": 7}"#).unwrap();
        assert_eq!(read.default_branch(), "main");
    }

    #[test]
    fn the_fetch_interval_and_clone_folder_read_back_or_fall_back() {
        let read = |json: &str| serde_json::from_str::<Settings>(json).unwrap();
        assert_eq!(read(r#"{"fetch_every": 0}"#).fetch_minutes(), 0);
        assert_eq!(read(r#"{"fetch_every": 15}"#).fetch_minutes(), 15);
        assert_eq!(read(r#"{"fetch_every": 900}"#).fetch_minutes(), MAX_FETCH_MINUTES);
        assert_eq!(read(r#"{"fetch_every": "often"}"#).fetch_minutes(), FETCH_MINUTES);
        assert_eq!(read(r#"{"fetch_every": -3}"#).fetch_minutes(), FETCH_MINUTES);
        assert_eq!(read(r#"{"clone_folder": [47, 116]}"#).clone_folder(), Some(PathBuf::from("/t")));
        assert_eq!(read(r#"{"clone_folder": "/t"}"#).clone_folder(), None);
        assert!(!read(r#"{"reopen_last": false}"#).reopen_last);
    }

    #[test]
    fn diff_text_settings_read_back_or_fall_back_to_the_defaults() {
        let read = |json: &str| serde_json::from_str::<Settings>(json).unwrap();
        let older = read(r#"{"wrap_lines": true}"#);
        assert_eq!((older.diff_font, older.diff_font_size, older.tab_width), (None, None, None));
        assert!(older.line_numbers);
        let chosen = read(r#"{"diff_font": "SF Mono", "diff_font_size": 14, "tab_width": 8, "line_numbers": false}"#);
        assert_eq!(chosen.diff_font.as_deref(), Some("SF Mono"));
        assert_eq!((chosen.diff_font_size, chosen.tab_width, chosen.line_numbers), (Some(14), Some(8), false));
        let odd = read(r#"{"diff_font": 7, "diff_font_size": "huge", "tab_width": -2}"#);
        assert_eq!((odd.diff_font, odd.diff_font_size, odd.tab_width), (None, None, None));
    }

    #[test]
    fn missing_files_give_the_defaults() {
        let folder = Folder::new();
        let (settings, memory, notice) = read(&folder.0);
        assert_eq!(settings, Settings::default());
        assert!(settings.show_ref_labels);
        assert_eq!(memory, Memory::default());
        assert_eq!(notice, None);
    }

    #[test]
    fn files_from_another_version_still_load() {
        let folder = Folder::new();
        fs::write(
            folder.0.join(SETTINGS),
            r#"{"diff_mode": "split", "pull_strategy": "rebase", "theme": "ink", "future": {"a": 1}}"#,
        )
        .unwrap();
        fs::write(
            folder.0.join(MEMORY),
            r#"{"columns": {"sidebar": 260, "panel": 320, "graph": 400},
                "window": {"x": 1, "y": 2, "width": 900, "height": 600, "mode": "tiled"},
                "repositories": [
                    {"directory": [47, 114], "closed": ["tags", "worktrees"], "folded": ["origin"]},
                    {"directory": [47, 115], "pull": {"fetch": true, "strategy": "squash"}},
                    {"directory": [47, 116], "pull": "sometimes", "merge": "octopus"},
                    {"directory": [47, 117], "merge": "no-fast-forward"},
                    {"unexpected": true}
                ],
                "recents": [{"path": [47, 114], "name": "r", "opened": 5}, 7, {"path": "nope"}],
                "clone_parent": "nope",
                "tabs": ["one", "two"]}"#,
        )
        .unwrap();
        let (settings, memory, notice) = read(&folder.0);
        assert_eq!(notice, None);
        assert_eq!(settings.diff_mode, DiffMode::Split);
        assert!(settings.show_ref_labels);
        assert!(!settings.push_new_tags);
        assert!(!settings.stash_in_the_way);
        assert!(settings.prune_on_fetch);
        assert_eq!(settings.default_branch(), "main");
        assert!(settings.reopen_last && settings.login_path);
        assert_eq!(settings.fetch_minutes(), FETCH_MINUTES);
        assert_eq!(settings.clone_folder(), None);
        assert_eq!(memory.columns, Some(ColumnWidths::new(260., 320.)));
        assert_eq!(memory.recents, [Recent { path: b"/r".to_vec(), name: "r".into(), opened: 5 }]);
        assert_eq!(memory.clone_parent(), None);
        assert_eq!(memory.window.as_ref().unwrap().mode, FrameMode::Windowed);
        assert_eq!(memory.repositories[0].closed, [Section::Tags]);
        assert_eq!(memory.repositories[0].folded, ["origin"]);
        let fetch = PullDefault { fetch: true, strategy: crate::sync::Strategy::FastForwardIfPossible };
        assert_eq!(memory.repositories[1].pull, Some(fetch));
        assert_eq!(memory.repositories[2].pull, None);
        assert_eq!(memory.repositories[0].pull, None);
        assert_eq!(memory.repositories[2].merge, None);
        assert_eq!(memory.repositories[3].merge, Some(crate::merge::Mode::NoFastForward));
        assert_eq!(memory.repositories[0].merge, None);
    }

    #[test]
    fn unreadable_files_are_set_aside_and_named_once() {
        let folder = Folder::new();
        fs::write(folder.0.join(SETTINGS), b"{\"diff_mode\": ").unwrap();
        fs::write(folder.0.join(MEMORY), b"not json").unwrap();
        let (settings, memory, notice) = read(&folder.0);
        assert_eq!((settings, memory), (Settings::default(), Memory::default()));
        let notice = notice.unwrap();
        assert!(notice.contains("settings.json.broken") && notice.contains("state.json.broken"));
        assert!(folder.0.join("settings.json.broken").exists());
        assert!(!folder.0.join(SETTINGS).exists());
        write_files(&folder.0, &Settings::default(), &Memory::default(), None).unwrap();
        assert_eq!(fs::read(folder.0.join("state.json.broken")).unwrap(), b"not json");
        assert_eq!(read(&folder.0).2, None);
    }

    #[test]
    fn saved_column_widths_are_kept_within_the_drag_limits() {
        let folder = Folder::new();
        fs::write(
            folder.0.join(MEMORY),
            r#"{"columns": {"sidebar": 5000, "panel": -3}}"#,
        )
        .unwrap();
        let (_, memory, _) = read(&folder.0);
        assert_eq!(memory.columns, Some(ColumnWidths::new(420., 240.)));
    }

    #[test]
    fn a_save_keeps_what_another_instance_saved() {
        let folder = Folder::new();
        fs::write(folder.0.join(SETTINGS), r#"{"theme": "ink"}"#).unwrap();
        let (settings, memory, _) = read(&folder.0);
        let base = (settings, memory);
        let mut one = base.clone();
        one.0.diff_mode = DiffMode::Split;
        one.1.change_repo(Path::new("/r/Cedar/.git"), |view| view.closed = vec![Section::Tags]);
        write_files(&folder.0, &one.0, &one.1, Some(&base)).unwrap();
        let mut two = base.clone();
        two.1.window = sample_memory().window;
        two.1.change_repo(Path::new("/r/Oak/.git"), |view| view.folded = vec!["origin".into()]);
        write_files(&folder.0, &two.0, &two.1, Some(&base)).unwrap();
        let (settings, memory, _) = read(&folder.0);
        assert_eq!(settings.diff_mode, DiffMode::Split);
        assert_eq!(memory.window, sample_memory().window);
        assert_eq!(memory.repo(Path::new("/r/Cedar/.git")).unwrap().closed, [Section::Tags]);
        assert_eq!(memory.repo(Path::new("/r/Oak/.git")).unwrap().folded, ["origin"]);
        let mut three = two.clone();
        three.1.change_repo(Path::new("/r/Oak/.git"), |view| view.folded.clear());
        write_files(&folder.0, &three.0, &three.1, Some(&two)).unwrap();
        let (_, memory, _) = read(&folder.0);
        assert!(memory.repo(Path::new("/r/Oak/.git")).is_none());
        assert!(memory.repo(Path::new("/r/Cedar/.git")).is_some());
        let raw: Value =
            serde_json::from_slice(&fs::read(folder.0.join(SETTINGS)).unwrap()).unwrap();
        assert_eq!(raw["theme"], "ink");
    }

    fn screen(w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(w), px(h)))
    }

    #[test]
    fn a_frame_reopens_on_its_own_display() {
        let frame = Frame {
            display: Some("B".into()),
            x: 100.,
            y: 60.,
            width: 1200.,
            height: 800.,
            mode: FrameMode::Windowed,
        };
        let displays = [
            (1, Some("A".to_owned()), screen(1512., 982.)),
            (2, Some("B".to_owned()), screen(2560., 1440.)),
        ];
        let (display, bounds) = frame.place(displays.clone(), None).unwrap();
        assert_eq!(display, 2);
        assert_eq!(
            bounds,
            WindowBounds::Windowed(Bounds::new(
                point(px(100.), px(60.)),
                size(px(1200.), px(800.))
            ))
        );
        let small = Frame { width: 300., height: 200., ..frame.clone() };
        let (_, bounds) = small.place(displays, None).unwrap();
        assert_eq!(bounds.get_bounds().size, size(px(860.), px(520.)));
    }

    #[test]
    fn a_frame_off_every_display_is_rejected() {
        let displays = || [(1, Some("A".to_owned()), screen(1512., 982.))];
        let frame = Frame {
            display: Some("A".into()),
            x: 100.,
            y: 60.,
            width: 1200.,
            height: 800.,
            mode: FrameMode::Windowed,
        };
        assert!(frame.place(displays(), None).is_some());
        let unnamed = Frame { display: None, ..frame.clone() };
        assert_eq!(unnamed.place(displays(), Some((7, screen(1512., 982.)))).unwrap().0, 7);
        assert!(unnamed.place(displays(), None).is_none());
        for gone in [
            Frame { display: Some("B".into()), ..frame.clone() },
            Frame { x: 1450., ..frame.clone() },
            Frame { x: -1150., ..frame.clone() },
            Frame { y: -300., ..frame.clone() },
            Frame { y: 970., ..frame.clone() },
            Frame { width: f32::NAN, ..frame.clone() },
        ] {
            assert!(gone.place(displays(), None).is_none(), "{gone:?}");
        }
        assert!(frame.place(Vec::<(u8, Option<String>, Bounds<Pixels>)>::new(), None).is_none());
    }

    #[test]
    fn repository_view_state_is_keyed_by_the_common_directory() {
        let f = Fixture::new();
        fs::write(f.dir.join("a.txt"), "a\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.git(&["commit", "-qm", "Start"]);
        let linked = f.dir.with_extension("linked");
        f.git(&["worktree", "add", "-q", "-b", "side", linked.to_str().unwrap()]);
        let other = f.open(&linked);
        let mut memory = Memory::default();
        memory.change_repo(f.repo.common_dir(), |view| view.closed = vec![Section::Remotes]);
        assert_eq!(
            memory.repo(other.common_dir()).map(|view| view.closed.clone()),
            Some(vec![Section::Remotes])
        );
        memory.change_repo(other.common_dir(), |view| view.closed.clear());
        assert!(memory.repositories.is_empty());
        for n in 0..REPOSITORIES + 3 {
            memory.change_repo(Path::new(&format!("/r/{n}/.git")), |view| {
                view.folded = vec!["origin".into()]
            });
        }
        assert_eq!(memory.repositories.len(), REPOSITORIES);
        assert!(memory.repo(Path::new("/r/0/.git")).is_none());
        assert!(memory.repo(Path::new(&format!("/r/{}/.git", REPOSITORIES + 2))).is_some());
        f.git(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
    }
}
