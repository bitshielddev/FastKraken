use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const TOKEN: &str = "hosting-replay-token";

#[derive(Clone, Debug)]
pub(super) struct Asked {
    pub(super) path: String,
    pub(super) since: Option<String>,
    pub(super) status: u16,
}

#[derive(Default)]
pub(super) struct Hosted {
    pub(super) open: BTreeMap<String, Vec<Value>>,
    pub(super) closed: BTreeMap<String, Vec<Value>>,
    pub(super) checks: BTreeMap<String, Vec<Value>>,
    pub(super) denied_pulls: BTreeSet<String>,
    pub(super) denied_checks: BTreeSet<String>,
    version: u64,
    pub(super) asked: Vec<Asked>,
}

pub(super) struct Hosting {
    address: SocketAddr,
    state: Arc<Mutex<Hosted>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

pub(super) struct Pull<'a> {
    pub(super) number: u64,
    pub(super) title: &'a str,
    pub(super) author: &'a str,
    pub(super) head: &'a str,
    pub(super) sha: &'a str,
    pub(super) repo: &'a str,
    pub(super) state: &'a str,
}

impl Pull<'_> {
    pub(super) fn json(&self) -> Value {
        let (state, draft, merged) = match self.state {
            "draft" => ("open", true, None),
            "merged" => ("closed", false, Some("2026-11-20T10:00:00Z")),
            "closed" => ("closed", false, None),
            _ => ("open", false, None),
        };
        json!({
            "number": self.number,
            "title": self.title,
            "state": state,
            "draft": draft,
            "merged_at": merged,
            "user": {"login": self.author},
            "head": {"ref": self.head, "sha": self.sha, "repo": {"full_name": self.repo}},
            "base": {"ref": "trunk"},
            "html_url": format!("https://github.com/cedar/badge-trail/pull/{}", self.number),
        })
    }
}

pub(super) fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({"name": name, "status": status, "conclusion": conclusion})
}

impl Hosting {
    pub(super) fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(Hosted::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_stop, worker_state) = (stop.clone(), state.clone());
        let worker = thread::Builder::new().name("hosting-replay".into()).spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { break };
                let _ = handle(stream, &worker_state);
            }
        })?;
        Ok(Self { address, state, stop, worker: Some(worker) })
    }

    pub(super) fn base(&self) -> String {
        format!("http://{}/", self.address)
    }

    pub(super) fn change(&self, change: impl FnOnce(&mut Hosted)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        change(&mut state);
        state.version += 1;
    }

    pub(super) fn asked(&self) -> Vec<Asked> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).asked.clone()
    }
}

impl Drop for Hosting {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle(mut stream: TcpStream, state: &Mutex<Hosted>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new((&stream).take(16 * 1024));
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let (mut authorized, mut since) = (false, None);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.trim_end().split_once(':') {
            if key.eq_ignore_ascii_case("authorization") {
                authorized = value.trim() == format!("Bearer {TOKEN}");
            }
            if key.eq_ignore_ascii_case("if-none-match") {
                since = Some(value.trim().to_owned());
            }
        }
    }
    let mut parts = first.split_ascii_whitespace();
    let (method, path) = (parts.next().unwrap_or_default().to_owned(), parts.next().unwrap_or_default().to_owned());
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let etag = format!("\"replay-{}\"", state.version);
    let (status, body) = if !authorized {
        (401, json!({"message": "Bad credentials"}))
    } else if method != "GET" {
        (404, json!({}))
    } else {
        route(&state, &path)
    };
    let (status, body) = match (status, &since) {
        (200, Some(since)) if *since == etag => (304, Value::Null),
        found => (found.0, body),
    };
    state.asked.push(Asked { path, since, status });
    drop(state);
    let body = if status == 304 { String::new() } else { body.to_string() };
    write!(
        stream,
        "HTTP/1.1 {status} Replay\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn route(state: &Hosted, path: &str) -> (u16, Value) {
    let Some(rest) = path.strip_prefix("/repos/") else { return (404, json!({})) };
    let mut parts = rest.splitn(3, '/');
    let (Some(owner), Some(name), Some(tail)) = (parts.next(), parts.next(), parts.next()) else { return (404, json!({})) };
    let repo = format!("{owner}/{name}").to_ascii_lowercase();
    let denied = json!({"message": "Resource not accessible by integration"});
    if tail == "pulls?state=open&per_page=100" || tail == "pulls?state=closed&sort=updated&direction=desc&per_page=50" {
        if state.denied_pulls.contains(&repo) {
            return (403, denied);
        }
        let listed = if tail.contains("state=open") { &state.open } else { &state.closed };
        return (200, Value::Array(listed.get(&repo).cloned().unwrap_or_default()));
    }
    if let Some(sha) = tail.strip_prefix("commits/").and_then(|rest| rest.strip_suffix("/check-runs?per_page=100")) {
        if state.denied_checks.contains(&repo) {
            return (403, denied);
        }
        let runs = state.checks.get(sha).cloned().unwrap_or_default();
        return (200, json!({"total_count": runs.len(), "check_runs": runs}));
    }
    (404, json!({}))
}
