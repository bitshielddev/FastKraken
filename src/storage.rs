use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

type Result<T> = std::result::Result<T, String>;

pub(crate) const STATE_DIR: &str = "FASTKRAKEN_STATE_DIR";

pub(crate) fn override_directory() -> Option<PathBuf> {
    env::var_os(STATE_DIR)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub(crate) fn directory() -> Result<PathBuf> {
    if let Some(path) = override_directory() {
        return Ok(path);
    }
    #[cfg(target_os = "linux")]
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|p| Path::new(p).is_absolute()) {
        return Ok(PathBuf::from(path).join("fastkraken"));
    }
    let home = env::var_os("HOME").ok_or("Cannot find your home directory.")?;
    let base = if cfg!(target_os = "macos") {
        "Library/Application Support/FastKraken"
    } else {
        ".config/fastkraken"
    };
    Ok(PathBuf::from(home).join(base))
}

pub(crate) fn lock(dir: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(dir).map_err(|e| format!("Create FastKraken settings directory: {e}"))?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join(name))
        .map_err(|e| format!("Open settings lock: {e}"))?;
    f.lock().map_err(|e| format!("Lock settings: {e}"))?;
    Ok(f)
}

pub(crate) fn load<T: DeserializeOwned + Default>(dir: &Path, name: &str, what: &str) -> Result<T> {
    match fs::read(dir.join(name)) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| format!("Cannot read FastKraken’s {what}."))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("Read {what}: {e}")),
    }
}

pub(crate) fn write<T: Serialize>(dir: &Path, name: &str, what: &str, value: &T) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| format!("Create FastKraken settings directory: {e}"))?;
    let mut tmp = NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut tmp, value).map_err(|e| e.to_string())?;
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist(dir.join(name))
        .map_err(|e| format!("Save {what}: {e}"))?;
    Ok(())
}

pub(crate) fn update<T: DeserializeOwned + Serialize + Default>(
    dir: &Path,
    name: &str,
    lock_name: &str,
    what: &str,
    change: impl FnOnce(&mut T) -> Result<()>,
) -> Result<()> {
    let _guard = lock(dir, lock_name)?;
    let mut value = load(dir, name, what)?;
    change(&mut value)?;
    write(dir, name, what, &value)
}

pub(crate) fn load_or_set_aside<T: DeserializeOwned + Default>(
    dir: &Path,
    name: &str,
) -> (T, Option<String>) {
    let path = dir.join(name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (T::default(), None),
        Err(e) => return (T::default(), Some(format!("Cannot read {name}: {e}"))),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => (value, None),
        Err(_) => {
            let broken = dir.join(format!("{name}.broken"));
            let message = match fs::rename(&path, &broken) {
                Ok(()) => format!(
                    "{name} could not be read, so FastKraken started with its defaults. The old file is saved as {}.",
                    broken.display()
                ),
                Err(e) => format!("{name} could not be read and was left in place: {e}"),
            };
            (T::default(), Some(message))
        }
    }
}

pub(crate) fn lenient<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

pub(crate) fn lenient_list<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let serde_json::Value::Array(items) = value else {
        return Ok(Vec::new());
    };
    Ok(items
        .into_iter()
        .filter_map(|item| T::deserialize(item).ok())
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Folder(pub(crate) PathBuf);

    impl Folder {
        pub(crate) fn new() -> Self {
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let dir = env::temp_dir().join(format!(
                "fastkraken-storage-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
    #[serde(default)]
    struct Sample {
        count: u32,
        names: Vec<String>,
    }

    #[test]
    fn update_creates_then_changes_the_file() {
        let folder = Folder::new();
        let dir = folder.0.join("nested");
        update(&dir, "sample.json", "sample.lock", "sample", |s: &mut Sample| {
            s.count = 1;
            Ok(())
        })
        .unwrap();
        update(&dir, "sample.json", "sample.lock", "sample", |s: &mut Sample| {
            s.count += 1;
            s.names.push("cedar".into());
            Ok(())
        })
        .unwrap();
        let loaded: Sample = load(&dir, "sample.json", "sample").unwrap();
        assert_eq!(loaded, Sample { count: 2, names: vec!["cedar".into()] });
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["sample.json", "sample.lock"]);
    }

    #[test]
    fn a_failed_change_leaves_the_file_alone() {
        let folder = Folder::new();
        write(&folder.0, "sample.json", "sample", &Sample { count: 3, names: vec![] }).unwrap();
        let err = update(&folder.0, "sample.json", "sample.lock", "sample", |s: &mut Sample| {
            s.count = 9;
            Err("refused".into())
        });
        assert_eq!(err, Err("refused".into()));
        let loaded: Sample = load(&folder.0, "sample.json", "sample").unwrap();
        assert_eq!(loaded.count, 3);
    }

    #[test]
    fn a_missing_file_is_the_default_and_an_unreadable_one_is_set_aside() {
        let folder = Folder::new();
        let (missing, message) = load_or_set_aside::<Sample>(&folder.0, "sample.json");
        assert_eq!((missing, message), (Sample::default(), None));

        fs::write(folder.0.join("sample.json"), b"{\"count\": 4,").unwrap();
        let (loaded, message) = load_or_set_aside::<Sample>(&folder.0, "sample.json");
        assert_eq!(loaded, Sample::default());
        assert!(message.unwrap().contains("sample.json.broken"));
        assert!(!folder.0.join("sample.json").exists());
        assert_eq!(
            fs::read(folder.0.join("sample.json.broken")).unwrap(),
            b"{\"count\": 4,"
        );
        fs::write(folder.0.join("other.json"), b"[1,").unwrap();
        assert!(load::<Sample>(&folder.0, "other.json", "sample").is_err());
    }
}
