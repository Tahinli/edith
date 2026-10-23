#![forbid(unsafe_code)]
mod help;
mod parse;
mod rpc;
mod server;
#[cfg(test)]
mod testutil;
mod tools;

use std::io::{self, BufRead, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGINT, SIGTERM};

/// The signal that asked this server to stop, or 0. Only the signal thread writes it.
static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);

fn stop_signal() -> i32 {
    STOP_SIGNAL.load(Ordering::SeqCst)
}

/// SIGTERM and SIGINT are a deliberate stop: running exports are cancelled (each deletes its
/// `.part`), then the process exits with 128 + the signal. The handler only sets the flag.
fn install_stop_handlers() {
    // A thread waits for the signals and records them; no signal handler runs
    // any code of ours, so the whole path stays safe Rust.
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])
        .expect("stop signal handlers");
    thread::spawn(move || {
        for sig in signals.forever() {
            STOP_SIGNAL.store(sig, Ordering::SeqCst);
        }
    });
}

fn main() {
    engine::ao::set_silent_output();
    // Stdout carries JSON-RPC lines and nothing else. The writer below gets its own copy of
    // the real stdout; fd 1 itself is pointed at stderr, so a C library or child process that
    // writes to fd 1 lands in the log instead of the protocol. Before any thread starts.
    let rpc_out = io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .expect("dup of stdout failed");
    rustix::stdio::dup2_stdout(io::stderr()).expect("redirect stdout to stderr");
    tools::see::sweep_stale_scratch();
    let mut server = server::Server::new();
    let mut out = io::BufWriter::new(std::fs::File::from(rpc_out));
    install_stop_handlers();
    // stdin is read on its own thread, so a stop signal is seen while the client is quiet.
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            for line in io::stdin().lock().lines() {
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        })
        .expect("start the stdin reader");
    loop {
        if stop_signal() != 0 {
            break;
        }
        // A queued export starts the moment the one before it ends, polled or not.
        tools::export::advance(&mut server);
        let line = match lines_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(line)) => line,
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str(&line) {
            Ok(msg) => rpc::handle(&mut server, msg),
            Err(e) => Some(rpc::error(serde_json::Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(v) = reply
            && write_reply(&mut out, &v).is_err()
        {
            break;
        }
    }
    // Stdout is not written past this point: the protocol ends with the last reply.
    drop(out);
    // Unsaved work is written aside before anything else can go wrong: an autosave
    // next to the project, never over it (see `project_open`, which offers it).
    tools::project::autosave_at_eof(&server);
    wait_for_exports(&mut server);
    tools::see::remove_scratch();
    let sig = stop_signal();
    if sig != 0 {
        std::process::exit(128 + sig);
    }
}

/// Writes one reply as one line and flushes it. The stderr lock is held across the write, so no
/// `eprintln!` from an engine or export thread can land inside the line, even when a client
/// merges the two streams into one pipe.
fn write_reply(out: &mut impl Write, reply: &serde_json::Value) -> io::Result<()> {
    let mut line = reply.to_string();
    line.push('\n');
    let _stderr = io::stderr().lock();
    out.write_all(line.as_bytes())?;
    out.flush()
}

/// Keeps polling the export jobs after stdin closes, so a client that disconnects
/// does not lose a running export. Returns once no job is still writing. A stop signal
/// cancels the jobs still writing instead, and they delete their `.part`s.
fn wait_for_exports(server: &mut server::Server) {
    let mut last_log: Option<Instant> = None;
    let debug = std::env::var_os("EDITH_DEBUG").is_some();
    // The jobs still writing when the client left: their outcomes are the ones to report.
    let left: Vec<usize> = server
        .jobs
        .iter()
        .enumerate()
        .filter(|(_, j)| tools::export::job_running(j))
        .map(|(i, _)| i)
        .collect();
    let mut cancelled = false;
    loop {
        if stop_signal() != 0 && !cancelled {
            cancelled = true;
            let running = server.jobs.iter().filter(|j| tools::export::job_running(j)).count();
            eprintln!("stop signal {}: cancelling {running} export job(s); their .part files are deleted", stop_signal());
            for job in server.jobs.iter_mut().filter(|j| tools::export::job_running(j)) {
                job.cancel();
            }
        }
        tools::export::advance(server);
        let running: Vec<(usize, f32)> = server
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| tools::export::job_running(j))
            .map(|(i, j)| (i + 1, j.progress()))
            .collect();
        if running.is_empty() {
            // Nothing else can read this outcome back, so it goes to stderr once: a failure
            // always (the client is gone and a missing file would otherwise be silent), a
            // success only under EDITH_DEBUG.
            for &i in &left {
                let job = &server.jobs[i];
                match &job.outcome {
                    Some(Ok(())) if debug => eprintln!("export job:{} finished: {} ({:.0?} after start)", i + 1, job.out.display(), job.started.elapsed()),
                    Some(Ok(())) => {}
                    Some(Err(e)) => eprintln!("export job:{} failed: {}: {e}", i + 1, job.out.display()),
                    None => eprintln!("export job:{} ended without a result: {}", i + 1, job.out.display()),
                }
            }
            return;
        }
        if debug && last_log.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
            for (n, p) in &running {
                eprintln!("stdin closed; export job:{n} still running: {:.0}%", p * 100.0);
            }
            last_log = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A reply bigger than the writer's buffer still goes out as one JSON line, and nothing else is
    /// written with it: the line ends the reply and the next one starts on a line of its own.
    #[test]
    fn a_big_reply_is_one_json_line() {
        let big = json!({"jsonrpc": "2.0", "id": 7, "result": {"text": "x\n".repeat(20_000)}});
        let mut out = io::BufWriter::new(Vec::new());
        write_reply(&mut out, &big).expect("write the reply");
        write_reply(&mut out, &json!({"jsonrpc": "2.0", "id": 8, "result": {}})).expect("write the next reply");
        let bytes = out.into_inner().expect("flush the buffer");
        let text = String::from_utf8(bytes).expect("utf-8");
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(lines.len(), 2, "one line per reply");
        assert_eq!(serde_json::from_str::<serde_json::Value>(lines[0]).expect("first line")["id"], 7);
        assert_eq!(serde_json::from_str::<serde_json::Value>(lines[1]).expect("second line")["id"], 8);
    }

    #[test]
    fn no_jobs_means_no_wait() {
        let mut server = server::Server::new();
        wait_for_exports(&mut server);
        assert!(server.jobs.is_empty());
    }

    /// A job still writing when stdin closes is waited for, and its outcome recorded.
    #[test]
    fn a_running_export_is_waited_for_after_eof() {
        engine::ao::set_silent_output();
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        let session = engine::PlaybackSession::open(&src).expect("open assets/test_av.mp4");
        let mut server = server::Server::new();
        server.session = Some(session);
        let dir = crate::testutil::TestDir::new("main-eof");
        let out = dir.join("eof.mp4");
        let started = tools::export::tools()
            .into_iter()
            .find(|t| t.name == "export_start")
            .map(|t| (t.run)(&mut server, &json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}})))
            .expect("export_start is registered");
        assert!(!started.is_error, "{}", started.text);

        wait_for_exports(&mut server);
        assert!(server.jobs.iter().all(|j| j.outcome.is_some()), "every job settled");
        assert!(matches!(server.jobs[0].outcome, Some(Ok(()))), "{:?}", server.jobs[0].outcome);
        assert!(std::fs::metadata(&out).is_ok_and(|m| m.len() > 0));
    }

    /// A `.part` a killed process left beside the output is named by the next export to it, and
    /// cleared before that export writes its own.
    #[test]
    fn a_stale_part_is_named_and_cleared_by_the_next_export() {
        engine::ao::set_silent_output();
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        let session = engine::PlaybackSession::open(&src).expect("open assets/test_av.mp4");
        let mut server = server::Server::new();
        server.session = Some(session);
        let dir = crate::testutil::TestDir::new("main-stale-part");
        let out = dir.join("c.mp4");
        let part = dir.join("c.mp4.part");
        std::fs::write(&part, b"left by a killed process").expect("write the stale part");
        let start = tools::export::tools()
            .into_iter()
            .find(|t| t.name == "export_start")
            .expect("export_start is registered");
        let r = (start.run)(&mut server, &json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("did not finish") && r.text.contains("left behind — deleted"), "{}", r.text);
        wait_for_exports(&mut server);
        assert!(matches!(server.jobs[0].outcome, Some(Ok(()))), "{:?}", server.jobs[0].outcome);
        assert!(!part.exists(), "the export wrote its own part and renamed it away");
        assert!(std::fs::metadata(&out).is_ok_and(|m| m.len() > 0));
    }

    /// Two exports still running when stdin closes are both waited for: each finishes, leaves no
    /// `.part`, and its outcome is recorded.
    #[test]
    fn two_running_exports_are_both_waited_for_after_eof() {
        engine::ao::set_silent_output();
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        let session = engine::PlaybackSession::open(&src).expect("open assets/test_av.mp4");
        let mut server = server::Server::new();
        server.session = Some(session);
        let dir = crate::testutil::TestDir::new("main-eof-two");
        let start = tools::export::tools()
            .into_iter()
            .find(|t| t.name == "export_start")
            .expect("export_start is registered");
        let outs = [dir.join("a.mp4"), dir.join("b.mp4")];
        for out in &outs {
            let r = (start.run)(&mut server, &json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}}));
            assert!(!r.is_error, "{}", r.text);
        }
        assert_eq!(server.jobs.len(), 2);

        wait_for_exports(&mut server);
        assert!(server.jobs.iter().all(|j| matches!(j.outcome, Some(Ok(())))), "every job finished");
        for out in &outs {
            assert!(std::fs::metadata(out).is_ok_and(|m| m.len() > 0), "{} written", out.display());
            let part = out.with_extension("mp4.part");
            assert!(!part.exists(), "{} left behind", part.display());
        }
    }
}
