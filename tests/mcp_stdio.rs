//! Exercise the real launcher and SDK framing without a desktop or credentials.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{ffi::OsStrExt, net::UnixListener},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const TOKEN: &str = "test-only-fastkraken-stdio-credential";
const WAIT: Duration = Duration::from_secs(8);

fn launcher(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fastkraken"));
    command
        .env("FASTKRAKEN_STATE_DIR", state)
        .env("FASTKRAKEN_MCP_TOKEN", TOKEN)
        .env_remove("FASTKRAKEN_ASKPASS_SOCKET")
        .env_remove("FASTKRAKEN_SCENE");
    command
}

struct Session {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Session {
    fn start(state: &Path) -> Self {
        let mut child = launcher(state)
            .arg("--mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{message}").unwrap();
    }

    fn reply(&self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("no MCP response");
        assert!(!line.contains(TOKEN));
        serde_json::from_str(&line).unwrap()
    }

    fn finish(mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "helper outlived stdin EOF");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(self.lines.recv_timeout(WAIT), Err(mpsc::RecvTimeoutError::Disconnected)),
            "unexpected stdout or a surviving process with an open output pipe"
        );
        let mut diagnostic = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostic)
            .unwrap();
        assert!(!diagnostic.contains(TOKEN));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn modern_discovery_and_calls_work_without_spawning_the_desktop() {
    let state = tempfile::tempdir().unwrap();
    let mut s = Session::start(state.path());
    let metadata = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    });
    s.send(json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":metadata}}));
    let discovered = s.reply();
    assert!(
        discovered["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    assert_eq!(
        discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "FastKraken"
    );
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":metadata}}));
    let list = s.reply();
    assert_eq!(list["result"]["tools"], json!([]));
    assert_eq!(list["result"]["ttlMs"], 0);
    assert_eq!(list["result"]["cacheScope"], "private");
    s.send(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"_meta":metadata,"name":"get_current_view","arguments":{}}}));
    let denied = s.reply();
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(denied["result"]["structuredContent"]["code"], "app_unavailable");
    s.send(json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":metadata,"cursor":"bogus"}}));
    assert_eq!(s.reply()["error"]["code"], -32602);
    s.finish();
    assert_eq!(fs::read_dir(state.path()).unwrap().count(), 0);
}

#[test]
fn legacy_handshake_and_malformed_json_recover_and_exit_on_eof() {
    let state = tempfile::tempdir().unwrap();
    let mut s = Session::start(state.path());
    s.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}
    }}));
    assert_eq!(s.reply()["result"]["protocolVersion"], "2025-11-25");
    s.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    writeln!(s.child.stdin.as_mut().unwrap(), "{{!malformed").unwrap();
    // rmcp ignores unparsable lines and responds to well-formed JSON with an
    // invalid JSON-RPC shape. Both leave the established connection usable.
    s.send(json!({}));
    assert_eq!(s.reply()["error"]["code"], -32600);
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}));
    let list = s.reply();
    assert_eq!(list["result"]["tools"], json!([]));
    assert!(list["result"].get("ttlMs").is_none());
    s.finish();
    Session::start(state.path()).finish();
}

#[test]
fn extra_arguments_are_rejected_and_mcp_is_still_a_folder_name() {
    let state = tempfile::tempdir_in("/tmp").unwrap();
    let output = launcher(state.path()).args(["--mcp", TOKEN]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8(output.stderr).unwrap().contains(TOKEN));
    let dir = state.path().join("instance");
    fs::create_dir(&dir).unwrap();
    let folder = state.path().join("mcp");
    fs::create_dir(&folder).unwrap();
    let listener = UnixListener::bind(dir.join("open")).unwrap();
    let receiver = thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&conn).read_line(&mut request).unwrap();
        writeln!(conn, "{{\"opened\":true}}").unwrap();
        serde_json::from_str::<Value>(&request).unwrap()
    });
    let output = launcher(state.path())
        .current_dir(state.path())
        .arg("mcp")
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = folder.canonicalize().unwrap();
    assert_eq!(receiver.join().unwrap()["open"], json!(path.as_os_str().as_bytes()));
}
