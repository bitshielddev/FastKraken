use gpui::{AssetSource, SharedString};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

const LENT: &str = "lent/";

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "alert",
    "arrow-down",
    "arrow-up",
    "branch",
    "check",
    "chevron-down",
    "chevron-right",
    "chevron-up",
    "cloud",
    "commit",
    "copy",
    "crosshair",
    "external",
    "eye",
    "eye-off",
    "fetch",
    "folder",
    "funnel",
    "globe",
    "history",
    "list",
    "list-tree",
    "lock",
    "merge",
    "minus",
    "more",
    "ok",
    "pin",
    "plus",
    "search",
    "sort",
    "spinner",
    "stash",
    "tag",
    "trash",
    "undo",
    "x",
);

pub struct Assets;

fn lent() -> std::sync::MutexGuard<'static, HashMap<String, Vec<u8>>> {
    static LENT_BYTES: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    LENT_BYTES.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn lend(extension: &str, bytes: Vec<u8>) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!("{LENT}{}.{extension}", NEXT.fetch_add(1, Ordering::Relaxed));
    lent().insert(name.clone(), bytes);
    name
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(LENT) {
            return Ok(lent().remove(path).map(Cow::Owned));
        }
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
