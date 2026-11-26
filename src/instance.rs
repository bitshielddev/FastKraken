use gpui::{App, AppContext};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

const LIMIT: u64 = 64 * 1024;
const ANSWER: Duration = Duration::from_secs(5);
const LONGEST: usize = 103;

#[derive(Serialize, Deserialize)]
struct Request {
    open: Vec<u8>,
}

#[derive(Default, Serialize, Deserialize)]
struct Reply {
    #[serde(default)]
    opened: bool,
}

pub(crate) fn socket(state: &Path) -> PathBuf {
    let inside = state.join("instance").join("open");
    if inside.as_os_str().len() <= LONGEST {
        return inside;
    }
    let user = unsafe { libc::getuid() };
    env::temp_dir().join(format!("fastkraken-{user}-{:016x}", fnv(state.as_os_str().as_bytes()))).join("open")
}

pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}


#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Handed {
    Taken,
    Alone,
}

pub(crate) fn hand_over(socket: &Path, path: &Path) -> Result<Handed, String> {
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => return Ok(Handed::Alone),
        Err(error) => return Err(format!("Could not reach the FastKraken that is running: {error}")),
    };
    let _ = stream.set_read_timeout(Some(ANSWER));
    let _ = stream.set_write_timeout(Some(ANSWER));
    let mut line = serde_json::to_vec(&Request { open: path.as_os_str().as_bytes().to_vec() }).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| format!("Could not reach the FastKraken that is running: {e}"))?;
    let mut answer = String::new();
    let read = BufReader::new(&stream).take(LIMIT).read_line(&mut answer);
    match read.ok().and_then(|_| serde_json::from_str::<Reply>(&answer).ok()) {
        Some(Reply { opened: true }) => Ok(Handed::Taken),
        _ => Err("FastKraken is running but did not open the folder. Try again, or quit it first.".into()),
    }
}


pub(crate) struct Listener {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Listener {
    pub(crate) fn bind(path: &Path, open: impl Fn(PathBuf) -> bool + Send + Sync + 'static) -> Result<Self, String> {
        let dir = path.parent().ok_or("The socket has no folder")?;
        fs::create_dir_all(dir).map_err(|e| format!("Create {}: {e}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| format!("Protect {}: {e}", dir.display()))?;
        if UnixStream::connect(path).is_ok() {
            return Err(format!("Another FastKraken already listens at {}", path.display()));
        }
        let _ = fs::remove_file(path);
        let listener = UnixListener::bind(path).map_err(|e| format!("Listen at {}: {e}", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let open = Arc::new(open);
        thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        let open = open.clone();
                        let _ = thread::Builder::new().name("instance-open".into()).spawn(move || serve(stream, &*open));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { path: path.to_owned(), stop })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent().filter(|dir| dir.file_name().is_some_and(|name| name != "instance")) {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn serve(mut stream: UnixStream, open: &dyn Fn(PathBuf) -> bool) {
    let _ = stream.set_read_timeout(Some(ANSWER));
    let mut line = String::new();
    if BufReader::new(&stream).take(LIMIT).read_line(&mut line).is_err() {
        return;
    }
    let path = serde_json::from_str::<Request>(&line)
        .ok()
        .map(|request| PathBuf::from(std::ffi::OsStr::from_bytes(&request.open)))
        .filter(|path| path.is_absolute());
    let opened = path.is_some_and(|path| open(path));
    let mut reply = serde_json::to_vec(&Reply { opened }).unwrap_or_default();
    reply.push(b'\n');
    let _ = stream.write_all(&reply);
}

static LISTENING: Mutex<Option<Listener>> = Mutex::new(None);

pub(crate) fn serve_app(state: &Path, cx: &mut App) -> Result<PathBuf, String> {
    let listener = listen(state, cx)?;
    let path = listener.path().to_owned();
    *LISTENING.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
    Ok(path)
}

pub(crate) fn listen(state: &Path, cx: &mut App) -> Result<Listener, String> {
    let (send, paths) = async_channel::unbounded::<PathBuf>();
    let listener = Listener::bind(&socket(state), move |path| send.try_send(path).is_ok())?;
    cx.spawn(async move |cx| {
        while let Ok(path) = paths.recv().await {
            let _ = cx.update(|cx| {
                if crate::tabs::ask(cx, crate::tabs::Request::Handed(path)) {
                    activate(cx);
                }
            });
        }
    })
    .detach();
    Ok(listener)
}

pub(crate) fn stop() {
    LISTENING.lock().unwrap_or_else(|e| e.into_inner()).take();
}

static ACTIVATED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn activated() -> u64 {
    ACTIVATED.load(Ordering::SeqCst)
}

fn activate(cx: &mut App) {
    if crate::tools::standing_in() {
        ACTIVATED.fetch_add(1, Ordering::SeqCst);
        return;
    }
    cx.activate(true);
    if let Some(window) = crate::tabs::window(cx) {
        let _ = cx.update_window(window, |_, window, _| window.activate_window());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn the_socket_follows_the_state_folder() {
        let short = Path::new("/Users/dan/Library/Application Support/FastKraken");
        assert_eq!(socket(short), short.join("instance/open"));
        let long = PathBuf::from(format!("/private/var/folders/{}/state", "x".repeat(80)));
        let other = PathBuf::from(format!("/private/var/folders/{}/state", "y".repeat(80)));
        let (one, two) = (socket(&long), socket(&other));
        assert!(one.starts_with(env::temp_dir()), "{one:?}");
        assert!(one.as_os_str().len() <= LONGEST, "{one:?}");
        assert_eq!(one.file_name().unwrap(), "open");
        assert_ne!(one, two);
        assert_eq!(one, socket(&long));
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_path_handed_over_reaches_the_running_app_and_the_socket_goes_with_it() {
        let state = tempfile::tempdir().unwrap();
        let at = socket(state.path());
        assert_eq!(hand_over(&at, Path::new("/tmp/trail")), Ok(Handed::Alone));
        let (send, received) = mpsc::channel();
        let listener = Listener::bind(&at, move |path| send.send(path).is_ok()).unwrap();
        let mode = fs::metadata(at.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(hand_over(&at, Path::new("/tmp/trail repo")), Ok(Handed::Taken));
        assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), PathBuf::from("/tmp/trail repo"));
        assert!(Listener::bind(&at, |_| true).is_err());
        assert_eq!(hand_over(&at, Path::new("/tmp/again")), Ok(Handed::Taken));
        assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), PathBuf::from("/tmp/again"));
        drop(listener);
        assert!(!at.exists());
        assert_eq!(hand_over(&at, Path::new("/tmp/trail")), Ok(Handed::Alone));
    }

    #[test]
    fn a_socket_left_by_an_ended_app_is_taken_over_and_a_relative_path_refused() {
        let state = tempfile::tempdir().unwrap();
        let at = socket(state.path());
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        drop(UnixListener::bind(&at).unwrap());
        assert!(at.exists());
        assert_eq!(hand_over(&at, Path::new("/tmp/trail")), Ok(Handed::Alone));
        let (send, received) = mpsc::channel();
        let _listener = Listener::bind(&at, move |path| send.send(path).is_ok()).unwrap();
        let error = hand_over(&at, Path::new("trail")).unwrap_err();
        assert!(error.contains("did not open the folder"), "{error}");
        assert!(received.recv_timeout(Duration::from_millis(200)).is_err());
        let mut stream = UnixStream::connect(&at).unwrap();
        stream.write_all(b"{\"close\": true}\n").unwrap();
        let mut reply = String::new();
        BufReader::new(&stream).read_line(&mut reply).unwrap();
        assert_eq!(reply.trim(), "{\"opened\":false}");
    }

    #[test]
    fn an_app_that_will_not_take_the_path_says_so() {
        let state = tempfile::tempdir().unwrap();
        let at = socket(state.path());
        let _listener = Listener::bind(&at, |_| false).unwrap();
        assert!(hand_over(&at, Path::new("/tmp/trail")).is_err());
    }
}
