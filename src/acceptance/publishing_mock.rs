use crate::github::{Client, validate_repo_name};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const FIXTURE_TOKEN: &str = "Bearer publishing-fixture";
const PERSONAL: (&str, u64) = ("Cedar", 11);
const ORG: (&str, u64) = ("TrailTeam", 22);

#[derive(Default)]
struct State {
    posts: HashMap<String, usize>,
    repos: HashMap<String, Value>,
}

pub(super) struct Mock {
    address: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Mock {
    pub(super) fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("publishing-replay".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { break };
                    let _ = handle(stream, &worker_state);
                }
            })?;
        Ok(Self {
            address,
            state,
            stop,
            worker: Some(worker),
        })
    }

    pub(super) fn client(&self) -> Client {
        Client::for_test(&format!("http://{}", self.address))
    }

    pub(super) fn creations(&self) -> usize {
        self.state.lock().unwrap().posts.values().sum()
    }

    pub(super) fn creations_for(&self, owner: &str, name: &str) -> usize {
        let key = format!("{owner}/{name}").to_ascii_lowercase();
        self.state
            .lock()
            .unwrap()
            .posts
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn is_private(&self, owner: &str, name: &str) -> bool {
        let key = format!("{owner}/{name}").to_ascii_lowercase();
        self.state
            .lock()
            .unwrap()
            .repos
            .get(&key)
            .is_some_and(|repo| repo["private"] == true)
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle(mut stream: TcpStream, state: &Mutex<State>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new((&stream).take(16 * 1024));
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut authorized = false;
    let mut length = 0;
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
                authorized = value.trim() == FIXTURE_TOKEN;
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(usize::MAX);
            }
        }
    }
    if length > 8192 {
        return write_response(&mut stream, 413, json!({}));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let mut first = first.split_ascii_whitespace();
    let method = first.next().unwrap_or_default();
    let path = first.next().unwrap_or_default();
    if method == "POST" && path.starts_with("/login/") {
        let fields: HashMap<_, _> = url::form_urlencoded::parse(&bytes).collect();
        let response = match path {
            "/login/device/code" => json!({
                "device_code":"publishing-device-fixture", "user_code":"DEMO-CODE",
                "verification_uri":"https://github.com/login/device", "interval":1, "expires_in":120
            }),
            "/login/oauth/access_token"
                if fields.get("device_code").map(|v| v.as_ref())
                    == Some("publishing-device-fixture")
                    || fields.get("refresh_token").map(|v| v.as_ref())
                        == Some("publishing-refresh-fixture") =>
            {
                json!({
                    "access_token":"publishing-fixture", "token_type":"bearer",
                    "expires_in":28800, "refresh_token":"publishing-refresh-fixture",
                })
            }
            _ => json!({"error":"invalid_grant"}),
        };
        return write_response(&mut stream, 200, response);
    }
    if !authorized {
        return write_response(&mut stream, 401, json!({}));
    }
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut state = state.lock().unwrap();
    let reply = route(&mut state, method, path, &body);
    drop(state);
    match reply {
        Some((status, body)) => write_response(&mut stream, status, body),
        None => Ok(()),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, body: Value) -> io::Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn route(state: &mut State, method: &str, path: &str, body: &Value) -> Option<(u16, Value)> {
    match (method, path) {
        ("GET", "/user") => Some((200, json!({"login": PERSONAL.0, "id": PERSONAL.1}))),
        ("GET", "/user/installations?per_page=100&page=1") => {
            let installations: Vec<_> = [(PERSONAL,"User"), (ORG,"Organization")].iter().map(|((login,id), kind)| json!({
                "account":{"login":login,"id":id,"type":kind},
                "permissions":{"contents":"write","repository_creation":"write"},"suspended_at":null,
            })).collect();
            Some((200, json!({"installations":installations})))
        }
        ("GET", path) if path.starts_with("/repos/") => {
            let key = path["/repos/".len()..].to_ascii_lowercase();
            Some(
                state
                    .repos
                    .get(&key)
                    .cloned()
                    .map(|repo| (200, repo))
                    .unwrap_or((404, Value::Null)),
            )
        }
        ("POST", "/user/repos") => create(state, PERSONAL, body),
        ("POST", "/orgs/TrailTeam/repos") => create(state, ORG, body),
        _ => Some((404, Value::Null)),
    }
}

fn create(state: &mut State, (login, id): (&str, u64), body: &Value) -> Option<(u16, Value)> {
    let name = body["name"].as_str().unwrap_or_default();
    let full_name = format!("{login}/{name}");
    let key = full_name.to_ascii_lowercase();
    *state.posts.entry(key.clone()).or_default() += 1;
    let Some(private) = body["private"].as_bool() else {
        return Some((422, Value::Null));
    };
    if body["auto_init"] != false || validate_repo_name(name).is_err() {
        return Some((422, Value::Null));
    }
    if state.repos.contains_key(&key) {
        return Some((
            422,
            json!({"errors":[{"field":"name","code":"already_exists"}]}),
        ));
    }
    let repo = json!({"id":100+state.repos.len(),"owner":{"id":id,"login":login},
        "full_name":full_name,"clone_url":format!("https://github.com/{full_name}.git"),
        "private":private,"visibility":if private {"private"} else {"public"}});
    state.repos.insert(key, repo.clone());
    if name == "uncertain" {
        None
    } else {
        Some((201, repo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Error;

    #[test]
    fn native_replay_provider_keeps_the_repository_when_a_response_is_lost() {
        let mock = Mock::start().unwrap();
        let api = mock.client();
        assert!(matches!(api.account("wrong"), Err(Error::Unauthorized(_))));
        let device = api.start_device().unwrap();
        assert_eq!(device.interval, 1);
        let crate::github::Poll::Ready(tokens) = api.poll_device(&device).unwrap() else {
            panic!("expected synthetic device approval")
        };
        let account = api.account(&tokens.access_token).unwrap();
        assert_eq!(account.owners.len(), 2);
        assert!(matches!(
            api.create("publishing-fixture", &account.owners[1], "uncertain", true),
            Err(Error::Uncertain(_))
        ));
        let repo = api
            .lookup("publishing-fixture", "TrailTeam", "uncertain")
            .unwrap()
            .unwrap();
        assert_eq!(repo.full_name, "TrailTeam/uncertain");
        assert_eq!(mock.creations_for("TrailTeam", "uncertain"), 1);
        assert_eq!(mock.creations(), 1);
        assert!(mock.is_private("TrailTeam", "uncertain"));
    }
}
