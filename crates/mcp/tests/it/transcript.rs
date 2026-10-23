//! End-to-end JSON-RPC transcript against the real `edith_mcp` binary.
//!
//! Drives one sequential session over stdio: project, media, edits (including
//! dry runs, atomic rollback, undo/redo), save and reopen, and a full export.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_edith_mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn edith_mcp");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self { child, stdin, stdout, next_id: 1 }
    }

    fn send(&mut self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write request");
        self.stdin.flush().expect("flush request");
    }

    /// Sends one request and reads exactly one response line.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "server closed stdout during {method}");
        let v: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bad JSON from server during {method}: {e}: {line}"));
        assert_eq!(v["id"], json!(id), "response id mismatch for {method}");
        v
    }

    /// Sends a notification (no id): the server must not reply, so nothing is read.
    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Calls a tool; returns (text, isError, structuredContent or Null).
    fn tool(&mut self, name: &str, args: Value) -> (String, bool, Value) {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = r.get("result").unwrap_or_else(|| panic!("tools/call {name} gave no result: {r}"));
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tools/call {name} gave no text content: {result}"))
            .to_owned();
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let structured = result.get("structuredContent").cloned().unwrap_or(Value::Null);
        eprintln!("[{name}] isError={is_error}\n{text}\n");
        (text, is_error, structured)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Removes the scratch directory when the test ends, even on panic.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn lane<'a>(tl: &'a Value, name: &str) -> Option<&'a Value> {
    tl["lanes"].as_array()?.iter().find(|l| l["lane"] == name)
}

fn clip_count(tl: &Value, name: &str) -> usize {
    lane(tl, name).and_then(|l| l["clips"].as_array()).map_or(0, Vec::len)
}

/// Lane-by-lane (start, in, out) projection of a timeline, for equality checks.
fn layout(tl: &Value) -> Value {
    let lanes: Vec<Value> = tl["lanes"]
        .as_array()
        .expect("lanes array")
        .iter()
        .map(|l| {
            let clips: Vec<Value> = l["clips"]
                .as_array()
                .expect("clips array")
                .iter()
                .map(|c| json!([c["start"], c["in"], c["out"]]))
                .collect();
            json!({"lane": l["lane"], "clips": clips})
        })
        .collect();
    json!({"fps": tl["fps"], "duration_frames": tl["duration_frames"], "lanes": lanes})
}

#[test]
fn transcript() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .canonicalize()
        .expect("assets directory exists");
    let scratch_dir = std::env::temp_dir().join(format!("edith_mcp-transcript-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch_dir);
    fs::create_dir_all(&scratch_dir).expect("create scratch dir");
    // Declared first so it drops last: the server is killed before the directory goes.
    let scratch = Scratch(scratch_dir);
    let mut c = Client::spawn();
    let dir = scratch.0.clone();
    let asset = |name: &str| assets.join(name).display().to_string();

    // 1. initialize, initialized notification, tools/list.
    let init = c.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "transcript", "version": "0"}}),
    );
    assert!(init["result"]["protocolVersion"].is_string(), "initialize: {init}");
    c.notify("notifications/initialized", json!({}));

    let list = c.call("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    for t in tools {
        let name = t["name"].as_str().expect("tool name");
        assert!(t["description"].is_string(), "{name} has no description");
        assert_eq!(t["inputSchema"]["type"], "object", "{name} inputSchema is not an object");
    }
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for want in [
        "project_new", "project_open", "project_save", "project_status", "project_settings",
        "media_probe", "media_import", "media_list", "timeline", "clip_info", "edit", "undo",
        "redo", "export_start", "export_presets", "export_status", "export_cancel",
    ] {
        assert!(names.contains(&want), "missing tool {want}; have {names:?}");
    }

    // 2. project and media.
    let cut = dir.join("cut.edith");
    let cut_s = cut.display().to_string();
    let (t, e, _) = c.tool("project_new", json!({"path": cut_s}));
    assert!(!e, "project_new: {t}");

    let (t, e, _) = c.tool("media_probe", json!({"glob": asset("test_tone.*")}));
    assert!(!e && t.contains("test_tone.wav"), "media_probe: {t}");

    let (t, e, _) = c.tool("media_import", json!({"paths": [asset("test_av.mp4"), asset("test_tone.wav")]}));
    assert!(!e && t.contains("src:0") && t.contains("src:1"), "media_import: {t}");

    // 3. rough cut with keep.
    let (t, e, _) = c.tool(
        "edit",
        json!({"ops": [{"op": "keep", "source": 0, "ranges": [["0s", "1s"], ["2s", "3s"], ["4s", "4.5s"]]}]}),
    );
    assert!(!e, "keep: {t}");
    let (_, _, tl) = c.tool("timeline", json!({}));
    assert_eq!(clip_count(&tl, "V1"), 3, "V1 should hold three kept clips: {tl}");

    // 4. dry run leaves the timeline untouched.
    let before = c.tool("timeline", json!({})).2;
    let (t, e, _) = c.tool("edit", json!({"ops": [{"op": "split", "at": "0.5s"}], "dry_run": true}));
    assert!(!e, "dry_run split: {t}");
    let after = c.tool("timeline", json!({})).2;
    assert_eq!(before, after, "dry_run changed the timeline");

    // 5. a failing batch rolls back everything before it.
    let (t, e, _) = c.tool(
        "edit",
        json!({"ops": [
            {"op": "fade", "clip": "V1#0", "in": "0.2s"},
            {"op": "trim", "clip": "V1#9", "edge": "out", "to": "1s"},
        ]}),
    );
    assert!(e, "bad batch should be an error: {t}");
    assert!(t.contains("op 2") && t.contains("rolled back"), "bad batch text: {t}");
    let after_bad = c.tool("timeline", json!({})).2;
    assert_eq!(before, after_bad, "failed batch changed the timeline");

    // 6. place onto lane A2 (adding it first if needed), set its gain, then undo and redo.
    let tl_now = c.tool("timeline", json!({})).2;
    let mut ops = Vec::new();
    if lane(&tl_now, "A2").is_none() {
        ops.push(json!({"op": "lane_add", "kind": "audio"}));
    }
    ops.push(json!({"op": "place", "source": 1, "lane": "A2", "at": 0}));
    ops.push(json!({"op": "lane_gain", "lane": "A2", "db": -12}));
    let (t, e, _) = c.tool("edit", json!({"ops": ops}));
    assert!(!e, "A2 batch: {t}");
    let with_a2 = c.tool("timeline", json!({})).2;
    assert_eq!(clip_count(&with_a2, "A2"), 1, "A2 should hold the placed clip: {with_a2}");

    let (t, e, _) = c.tool("undo", json!({}));
    assert!(!e, "undo: {t}");
    let undone = c.tool("timeline", json!({})).2;
    assert_eq!(clip_count(&undone, "A2"), 0, "undo should remove the A2 clip: {undone}");

    let (t, e, _) = c.tool("redo", json!({}));
    assert!(!e, "redo: {t}");
    let redone = c.tool("timeline", json!({})).2;
    assert_eq!(clip_count(&redone, "A2"), 1, "redo should bring the A2 clip back: {redone}");

    // 7. clip_info resolves a clip back to its source.
    let (t, e, _) = c.tool("clip_info", json!({"clip": "V1#0"}));
    assert!(!e && t.contains("src:0"), "clip_info: {t}");

    // 8. save, reopen, and compare the layout.
    let pre_save = c.tool("timeline", json!({})).2;
    let (t, e, _) = c.tool("project_save", json!({}));
    assert!(!e, "project_save: {t}");
    assert!(cut.exists(), "project file missing after save");

    let (t, e, st) = c.tool("project_open", json!({"path": cut_s}));
    assert!(!e, "project_open: {t}");
    assert_eq!(st["dirty"], json!(false), "freshly opened project should not be dirty: {st}");
    let reopened = c.tool("timeline", json!({})).2;
    assert_eq!(layout(&pre_save), layout(&reopened), "layout changed across save and reopen");

    // 9. export a one-second range, poll to completion, then refuse to overwrite.
    let out = dir.join("out.mp4");
    let out_s = out.display().to_string();
    let (t, e, _) = c.tool("export_start", json!({"out": out_s, "range": {"from": 0, "to": "1s"}}));
    assert!(!e && t.contains("job:1"), "export_start: {t}");

    // A one-second range is a few seconds of encode on an idle machine, but this
    // runs an unoptimized test build alongside other builds (load average ~40 on
    // 12 cores), where the software H.264/AAC encode took over two minutes while
    // still making progress (75% to 81%). The budget is wall-clock, so it must
    // allow for a loaded host; progress is what the test cares about, not speed.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let (t, e, _) = c.tool("export_status", json!({"job": 1}));
        if e {
            panic!("export failed: {t}");
        }
        if t.contains("done") {
            break;
        }
        assert!(Instant::now() < deadline, "export did not finish within 120s: {t}");
        sleep(Duration::from_millis(250));
    }
    let size = fs::metadata(&out).expect("exported file exists").len();
    assert!(size > 0, "exported file is empty");

    let (t, e, _) = c.tool("export_start", json!({"out": out_s, "range": {"from": 0, "to": "1s"}}));
    assert!(e, "second export over an existing file should be refused: {t}");

    // 10. unknown tool is a tool error; unknown method is a JSON-RPC error.
    let (t, e, _) = c.tool("no_such_tool", json!({}));
    assert!(e, "unknown tool should be an error: {t}");
    let bogus = c.call("bogus/method", json!({}));
    assert!(bogus["error"].is_object(), "unknown method should be a JSON-RPC error: {bogus}");
}
