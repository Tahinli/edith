//! Seeded mutation fuzz over the media corpus. Every entry point must return
//! `Ok` or `Err` on a damaged file: a panic (on the calling thread or any worker
//! it spawned) or a call that runs past `HANG` fails the test.
//!
//! Mutations per file, per iteration: truncate at a random offset, flip random
//! bits, or zero a random range. `EDITH_FUZZ_ITERS` sets the iterations per file
//! (default 16). Mutated copies go under `CARGO_TARGET_TMPDIR`, so /tmp stays out
//! of it.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use engine::{audio::AudioSession, decode::DecodeSession, demux, playback::PlaybackSession, subtitle};

/// Per-call budget. A file that takes longer than this is a hang.
const HANG: Duration = Duration::from_secs(5);
const MEDIA: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "mka", "mks", "webm", "mpg", "mpeg", "wav", "mp3", "flac", "ogg",
    "opus", "m4a", "aac", "srt", "vtt", "ass", "png", "jpg", "jpeg",
];

/// Panic messages from any thread, captured while a case runs.
static PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// xorshift64: deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| MEDIA.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
}

/// Every media file under the fixture dirs and `assets/`, sorted for a stable seed map.
fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["tests/data", "tests/fixtures/fuzz", "../../assets"] {
        walk(&root.join(dir), &mut files);
    }
    files.sort();
    files
}

/// One damaged copy of `bytes`, and a label for the failure report.
fn mutate(bytes: &[u8], rng: &mut Rng) -> (String, Vec<u8>) {
    let len = bytes.len();
    match rng.below(4) {
        0 => {
            let at = rng.below(len + 1);
            (format!("truncate@{at}"), bytes[..at].to_vec())
        }
        // Container and codec headers sit at the front, so most flips land there.
        m @ (1 | 3) => {
            let mut out = bytes.to_vec();
            let window = if m == 3 { len.min(512) } else { len };
            let flips = 1 + rng.below(16);
            for _ in 0..flips {
                let at = rng.below(window);
                out[at] ^= 1 << rng.below(8);
            }
            (format!("flip{flips}/w{window}"), out)
        }
        _ => {
            let start = rng.below(len);
            let span = 1 + rng.below((len - start).min(4096));
            let mut out = bytes.to_vec();
            out[start..start + span].fill(0);
            (format!("zero@{start}+{span}"), out)
        }
    }
}

/// Drains up to `n` items from a worker channel, stopping at the first gap or end.
fn drain<T>(rx: mpsc::Receiver<T>, n: usize) {
    for _ in 0..n {
        if rx.recv_timeout(Duration::from_secs(1)).is_err() {
            break;
        }
    }
}

/// Every public entry point a damaged file can reach.
fn exercise(path: &Path) {
    let _ = demux::probe_media(path);
    if let Ok(Some((_meta, rx))) = AudioSession::open(path) {
        drain(rx, 4);
    }
    if let Ok((_meta, rx)) = DecodeSession::open(path) {
        drain(rx, 3);
    }
    if let Ok(mut session) = PlaybackSession::open(path) {
        let _ = session.import(path);
    }
    let _ = PlaybackSession::parse_subtitles(path);
    let _ = subtitle::open(path, None);
}

fn payload_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic".into())
}

/// Runs `exercise` on its own thread. `Err` is a panic, `Hang` a timeout.
enum Outcome {
    Ok,
    Panic(String),
    Hang,
}

fn run_case(path: &Path) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        let result = panic::catch_unwind(AssertUnwindSafe(|| exercise(&owned)));
        let _ = tx.send(result.err().map(payload_text));
    });
    match rx.recv_timeout(HANG) {
        Ok(None) => Outcome::Ok,
        Ok(Some(msg)) => Outcome::Panic(msg),
        Err(RecvTimeoutError::Timeout) => Outcome::Hang,
        Err(RecvTimeoutError::Disconnected) => Outcome::Panic("case thread died".into()),
    }
}

/// Minimal inputs that once panicked or hung, kept as fixtures under
/// `tests/fixtures/fuzz/`. Each must come back `Ok` or `Err` from every entry point.
const REGRESSIONS: &[&str] = &["moov_largesize_wrap.mp4", "huge_block.mkv"];

#[test]
fn damaged_media_never_panics_or_hangs() {
    panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let line = format!("{location}: {info}");
        eprintln!("{line}");
        PANICS.lock().unwrap().push(line);
    }));

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fuzz");
    for name in REGRESSIONS {
        let path = fixtures.join(name);
        PANICS.lock().unwrap().clear();
        let outcome = run_case(&path);
        let captured = std::mem::take(&mut *PANICS.lock().unwrap());
        match outcome {
            Outcome::Ok if captured.is_empty() => {}
            Outcome::Ok => panic!("{name}: worker panic: {}", captured.join(" | ")),
            Outcome::Panic(msg) => panic!("{name}: panic: {msg}"),
            Outcome::Hang => panic!("{name}: hang over {HANG:?}"),
        }
    }

    let iters: usize = std::env::var("EDITH_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("media_fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    let mut failures = Vec::new();
    let files = corpus();
    assert!(!files.is_empty(), "no media files found");
    for (fi, file) in files.iter().enumerate() {
        let bytes = std::fs::read(file).unwrap();
        if bytes.is_empty() {
            continue;
        }
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("f");
        for iter in 0..iters {
            let seed = ((fi as u64 + 1) << 32) ^ (iter as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut rng = Rng(seed | 1);
            let (label, damaged) = mutate(&bytes, &mut rng);
            let case = scratch.join(format!("{stem}-{iter}.{ext}"));
            std::fs::write(&case, &damaged).unwrap();

            PANICS.lock().unwrap().clear();
            let outcome = run_case(&case);
            let captured = std::mem::take(&mut *PANICS.lock().unwrap());
            let verdict = match outcome {
                Outcome::Ok if captured.is_empty() => continue,
                Outcome::Ok => format!("worker panic: {}", captured.join(" | ")),
                Outcome::Panic(msg) => format!("panic: {msg}"),
                Outcome::Hang => format!("hang over {HANG:?}"),
            };
            failures.push(format!("{} [{label}] {verdict} (input {})", file.display(), case.display()));
        }
    }

    let _ = panic::take_hook();
    assert!(
        failures.is_empty(),
        "{} failing case(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
