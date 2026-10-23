//! Process exit paths of the real `edith_mcp` binary: stdin closing keeps a running export
//! alive until it finishes, and SIGTERM cancels it and removes its `.part`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
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
    /// Spawns the server with its stderr written to `log`, so a failed assertion can quote
    /// what the server said about its exports.
    fn spawn(log: &Path) -> Self {
        let stderr = fs::File::create(log).expect("make the server log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_edith_mcp"))
            // The child keeps its templates and tones out of the user's own data folder.
            .env("XDG_DATA_HOME", std::env::temp_dir().join(format!("edith_mcp-xdg-{}", std::process::id())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn edith_mcp");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self { child, stdin, stdout, next_id: 1 }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write request");
        self.stdin.flush().expect("flush request");
        let mut reply = String::new();
        let n = self.stdout.read_line(&mut reply).expect("read response");
        assert!(n > 0, "server closed stdout during {method}");
        let v: Value = serde_json::from_str(&reply).expect("JSON reply");
        assert_eq!(v["id"], json!(id), "response id mismatch for {method}");
        v
    }

    fn tool(&mut self, name: &str, args: Value) -> String {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        assert!(result["isError"] != json!(true), "{name} failed: {result}");
        result["content"][0]["text"].as_str().unwrap_or_default().to_string()
    }

    /// A 320x180 project with `length` of a still on the timeline and an export of it started.
    /// Small on purpose: the test is about the process, not the encode, and the machine under
    /// test runs many cargo jobs at once.
    fn start_export(&mut self, dir: &Path, out: &Path, length: &str) {
        self.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "exit-paths", "version": "1"}}));
        let mut line = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write initialized");
        self.tool(
            "project_new",
            json!({"path": dir.join("p.edith").display().to_string(), "resolution": "320x180"}),
        );
        self.tool("media_import", json!({"paths": [still().display().to_string()]}));
        self.tool("edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s", "length": length}]}));
        let started = self.tool("export_start", json!({"out": out.display().to_string(), "seat": "software"}));
        assert!(started.contains("job:1 started"), "{started}");
    }
}

fn still() -> PathBuf {
    PathBuf::from(format!("{}/../../assets/test_still.png", env!("CARGO_MANIFEST_DIR")))
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("make test dir");
    dir
}

/// What the server wrote to stderr, for a failure message.
fn server_log(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_else(|e| format!("(no server log: {e})"))
}

/// The length of a media file in seconds, as ffprobe reads it.
fn probe_seconds(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(out.status.success(), "ffprobe failed on {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("no duration for {}", path.display()))
}

/// Waits for the child to exit, killing it past `limit` so a hang fails the test instead of stalling it.
fn wait_exit(child: &mut Child, limit: Duration) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            panic!("edith_mcp still running after {limit:?}");
        }
        sleep(Duration::from_millis(200));
    }
}

/// Closing stdin is not a stop: the export keeps running and its complete file appears, and
/// nothing is written to stdout after the last reply.
#[test]
fn an_export_outlives_stdin_closing() {
    let dir = fresh_dir("exit-eof");
    let out = dir.join("eof.mp4");
    let log = dir.join("eof.log");
    let mut client = Client::spawn(&log);
    client.start_export(&dir, &out, "3s");

    let Client { mut child, stdin, mut stdout, .. } = client;
    drop(stdin);
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).expect("read stdout to its end");
    assert!(rest.is_empty(), "stdout after EOF: {:?}", String::from_utf8_lossy(&rest));
    let status = wait_exit(&mut child, Duration::from_secs(300));
    let said = server_log(&log);
    assert!(status.success(), "exit {status:?}; server said:\n{said}");
    assert!(
        fs::metadata(&out).is_ok_and(|m| m.len() > 0),
        "export not written after EOF; server said:\n{said}"
    );
    let seconds = probe_seconds(&out);
    assert!((2.5..=3.5).contains(&seconds), "a 3 s export ran {seconds} s; server said:\n{said}");
    assert!(!dir.join("eof.mp4.part").exists(), "no .part left behind");
}

/// SIGTERM is a deliberate stop: the running export is cancelled, its `.part` is deleted,
/// no output is published, and the process exits with 128 + 15.
#[test]
fn sigterm_cancels_a_running_export_and_removes_its_part() {
    let dir = fresh_dir("exit-sigterm");
    let out = dir.join("term.mp4");
    let log = dir.join("term.log");
    let mut client = Client::spawn(&log);
    // Long enough that it is still running when the signal lands, short enough at 320x180 to
    // start writing within a second or two.
    client.start_export(&dir, &out, "300s");
    let part = dir.join("term.mp4.part");
    let wait = Instant::now();
    while !part.exists() && wait.elapsed() < Duration::from_secs(120) {
        sleep(Duration::from_millis(100));
    }
    assert!(part.exists(), "the export started writing its .part; server said:\n{}", server_log(&log));

    let pid = client.child.id() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "send SIGTERM");
    let status = wait_exit(&mut client.child, Duration::from_secs(300));
    let said = server_log(&log);
    assert_eq!(status.code(), Some(128 + libc::SIGTERM), "exit {status:?}; server said:\n{said}");
    assert!(!part.exists(), "the cancelled export deleted its .part");
    assert!(!out.exists(), "no output published by a cancelled export; server said:\n{said}");
}

/// A failure that nobody can read back over stdout is named on stderr at exit: here the
/// `.part`'s folder is removed mid-export, so the final rename fails.
#[test]
fn a_failed_export_after_stdin_closes_is_named_on_stderr() {
    let dir = fresh_dir("exit-failed");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).expect("make the output folder");
    let out = sub.join("gone.mp4");
    let log = dir.join("failed.log");
    let mut client = Client::spawn(&log);
    client.start_export(&dir, &out, "30s");
    let part = sub.join("gone.mp4.part");
    let wait = Instant::now();
    while !part.exists() && wait.elapsed() < Duration::from_secs(120) {
        sleep(Duration::from_millis(50));
    }
    assert!(part.exists(), "the export started writing its .part");
    fs::remove_dir_all(&sub).expect("remove the output folder under the export");

    let Client { mut child, stdin, mut stdout, .. } = client;
    drop(stdin);
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).expect("read stdout to its end");
    let status = wait_exit(&mut child, Duration::from_secs(300));
    let said = server_log(&log);
    assert!(status.success(), "exit {status:?}; server said:\n{said}");
    assert!(said.contains("export job:1 failed"), "the failure is named on stderr; server said:\n{said}");
    assert!(said.contains("gone.mp4"), "the failure names the file; server said:\n{said}");
}
