//! Stdout of the real `edith_mcp` binary carries JSON-RPC lines and nothing else.
//!
//! An MCP client reads the server's stdout as a stream of JSON. A hardware-decode
//! fallback line, a libva message or a child process that inherits fd 1 would
//! each break that stream, so this drives the calls that touch the decoder and the
//! encoder (initialize, project_new, media_import, a frame, a short export), then
//! closes stdin and checks that every remaining stdout line parses as JSON too.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn() -> Self {
        Self::spawn_with_stderr(Stdio::inherit())
    }

    fn spawn_with_stderr(stderr: Stdio) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_edith_mcp"))
            // The child keeps its templates and tones out of the user's own data folder.
            .env("XDG_DATA_HOME", std::env::temp_dir().join(format!("edith_mcp-xdg-{}", std::process::id())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("spawn edith_mcp");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self { child, stdin: Some(stdin), stdout, next_id: 1 }
    }

    fn send(&mut self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        let stdin = self.stdin.as_mut().expect("stdin still open");
        stdin.write_all(line.as_bytes()).expect("write request");
        stdin.flush().expect("flush request");
    }

    /// Sends one request and reads exactly one response line, which must be JSON.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "server closed stdout during {method}");
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout line during {method} is not JSON: {e}: {line:?}"));
        assert_eq!(v["id"], json!(id), "response id mismatch for {method}");
        v
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Calls a tool; returns its text and whether it errored.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = r.get("result").unwrap_or_else(|| panic!("tools/call {name} gave no result: {r}"));
        let text = result["content"][0]["text"].as_str().unwrap_or("").to_owned();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        (text, is_error)
    }

    /// Closes stdin, then reads stdout to EOF: every line the server still writes must be JSON.
    fn drain_and_check(&mut self) {
        self.stdin = None;
        for line in (&mut self.stdout).lines() {
            let line = line.expect("read stdout");
            if line.trim().is_empty() {
                continue;
            }
            serde_json::from_str::<Value>(&line)
                .unwrap_or_else(|e| panic!("trailing stdout line is not JSON: {e}: {line:?}"));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn stdout_is_only_json_rpc() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .canonicalize()
        .expect("assets directory exists");
    let dir = std::env::temp_dir().join(format!("edith_mcp-stdout-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    let scratch = Scratch(dir);
    let dir = scratch.0.clone();
    let mut c = Client::spawn();

    let init = c.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stdout_clean", "version": "0"}}),
    );
    assert!(init["result"]["protocolVersion"].is_string(), "initialize: {init}");
    c.notify("notifications/initialized", json!({}));

    let project = dir.join("p.edith");
    let (t, e) = c.tool("project_new", json!({"path": project.display().to_string()}));
    assert!(!e, "project_new: {t}");

    let source = assets.join("test_av.mp4").display().to_string();
    let (t, e) = c.tool("media_import", json!({"paths": [source]}));
    assert!(!e, "media_import: {t}");

    let (t, e) = c.tool("edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "2s"]], "lane": "V1"}]}));
    assert!(!e, "edit: {t}");

    let (t, e) = c.tool("frame", json!({"at": "15f"}));
    assert!(!e, "frame: {t}");

    let out = dir.join("short.mkv");
    let (t, e) = c.tool(
        "export_start",
        json!({"out": out.display().to_string(), "range": {"from": 0, "to": "1s"}}),
    );
    assert!(!e && t.contains("job:1"), "export_start: {t}");

    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let (t, e) = c.tool("export_status", json!({"job": 1}));
        assert!(!e, "export failed: {t}");
        if t.contains("done") {
            break;
        }
        assert!(Instant::now() < deadline, "export did not finish: {t}");
        sleep(Duration::from_millis(250));
    }
    assert!(fs::metadata(&out).map(|m| m.len() > 0).unwrap_or(false), "exported file exists");

    c.drain_and_check();
}

/// Reads the server's stderr on a thread, so the notices it writes are collected while stdout is
/// driven. Join the handle after the server has exited to get the whole log.
fn log_stderr(c: &mut Client) -> std::thread::JoinHandle<String> {
    let mut stderr = c.child.stderr.take().expect("server stderr");
    std::thread::spawn(move || {
        let mut log = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut log);
        log
    })
}

/// Autosave notices are written when a session ends with unsaved edits, and when an earlier
/// session's autosave is set aside on open. Across two sessions that do both, stdout still carries
/// only JSON-RPC lines, and the notices are on stderr.
#[test]
fn autosave_notices_stay_off_stdout() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .canonicalize()
        .expect("assets directory exists");
    let dir = std::env::temp_dir().join(format!("edith_mcp-autosave-stdout-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    let scratch = Scratch(dir);
    let dir = scratch.0.clone();
    let project = dir.join("p.edith");
    let source = assets.join("test_av.mp4").display().to_string();
    let keep = json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "2s"]], "lane": "V1"}]});

    // Session one: an edit, then stdin closes. The unsaved edits are written beside the project.
    let mut first = Client::spawn_with_stderr(Stdio::piped());
    let log_first = log_stderr(&mut first);
    let init = first.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stdout_clean", "version": "0"}}),
    );
    assert!(init["result"]["protocolVersion"].is_string(), "initialize: {init}");
    first.notify("notifications/initialized", json!({}));
    let (t, e) = first.tool("project_new", json!({"path": project.display().to_string()}));
    assert!(!e, "project_new: {t}");
    let (t, e) = first.tool("media_import", json!({"paths": [source.clone()]}));
    assert!(!e, "media_import: {t}");
    let (t, e) = first.tool("edit", keep.clone());
    assert!(!e, "edit: {t}");
    first.drain_and_check();
    let _ = first.child.wait();
    let log_one = log_first.join().expect("stderr reader");
    assert!(log_one.contains("autosave: unsaved changes written to"), "session one autosave notice: {log_one}");

    // Session two: the earlier unsaved edits are recovered on open (the server refuses a plain open
    // past an autosave), then this session's edit is written at EOF. Both notices are on stderr;
    // stdout is checked line by line as before.
    let mut second = Client::spawn_with_stderr(Stdio::piped());
    let log_second = log_stderr(&mut second);
    let init = second.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stdout_clean", "version": "0"}}),
    );
    assert!(init["result"]["protocolVersion"].is_string(), "initialize: {init}");
    second.notify("notifications/initialized", json!({}));
    let (t, e) = second.tool("project_open", json!({"path": project.display().to_string(), "recover": true}));
    assert!(!e, "project_open: {t}");
    let (t, e) = second.tool("edit", keep);
    assert!(!e, "edit: {t}");
    second.drain_and_check();
    let _ = second.child.wait();
    let log_two = log_second.join().expect("stderr reader");
    assert!(log_two.contains("autosave:"), "session two autosave notice: {log_two}");
}
