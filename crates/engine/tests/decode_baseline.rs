use std::path::PathBuf;
use std::time::Instant;

use engine::DecodeSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

#[test]
fn decodes_baseline_mp4() {
    let path = asset("test_baseline.mp4");
    let (meta, rx) = DecodeSession::open(&path).expect("open");
    assert_eq!((meta.width, meta.height), (1280, 720), "container dims");
    assert!(meta.frame_count > 0);

    // Capped: pure-Rust 720p decode is slow in a debug test build.
    // Set DECODE_FRAMES to drain the whole stream (checks clean EOF).
    let want: usize = std::env::var("DECODE_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let start = Instant::now();
    let mut frames = 0usize;
    for frame in rx.iter().take(want) {
        assert_eq!((frame.width, frame.height), (1280, 720));
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4);
        assert_eq!(frame.index as usize, frames);
        frames += 1;
    }
    assert!(frames > 0, "no frames decoded");
    eprintln!(
        "{frames} frames in {:?} ({:?}/frame)",
        start.elapsed(),
        start.elapsed() / frames as u32
    );
}
