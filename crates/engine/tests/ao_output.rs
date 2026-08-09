//! Audio output checks. These need a built `libengine_audio.so` plus a running
//! PipeWire daemon, so they are `#[ignore]`d by default. Run them with:
//!
//! ```text
//! cargo build --workspace --release
//! cargo test -p engine --release --test ao_output -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `no_daemon_opens_nothing` sets `PIPEWIRE_REMOTE` in-process, hence the
//! required `--test-threads=1`; it restores the environment before asserting.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use engine::ao::AoSession;

const RATE: u32 = 44100;
const CHANNELS: u32 = 2;

/// Half a second of a quiet 440 Hz tone, interleaved stereo.
fn tone() -> Vec<f32> {
    let frames = RATE as usize / 2;
    let mut samples = Vec::with_capacity(frames * CHANNELS as usize);
    for i in 0..frames {
        let value = (i as f32 * TAU * 440.0 / RATE as f32).sin() * 0.2;
        samples.extend([value; CHANNELS as usize]);
    }
    samples
}

/// Waits up to `limit` for the clock to report a played position past zero.
fn wait_for_playback(ao: &AoSession, limit: Duration) -> i64 {
    let start = Instant::now();
    loop {
        if let Some(position) = ao.position() {
            if position > 0 {
                return position;
            }
        }
        assert!(start.elapsed() < limit, "playback position never advanced");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "needs libengine_audio.so and a PipeWire daemon"]
fn plays_and_tracks_position() {
    assert!(AoSession::probe(), "plugin not loadable");
    let mut ao = AoSession::open(RATE, CHANNELS).expect("no PipeWire playback available");

    // The ring holds a second, so two halves go in whole -- enough real audio
    // to cover the measurement below without starving.
    let samples = tone();
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");
    assert_eq!(ao.write(&samples), Some(samples.len()), "short write");

    // Over 400 ms the clock must run at the rate we asked for, not at the
    // device's: the plugin converts PipeWire's ticks out of the graph rate.
    let first = wait_for_playback(&ao, Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(400));
    let second = ao.position().expect("position went unknown");
    let advanced = second - first;
    let expected = RATE as i64 * 400 / 1000;
    eprintln!("clock advanced {advanced} samples in 400 ms (expected ~{expected})");
    assert!(
        (advanced - expected).abs() < expected / 10,
        "clock runs at the wrong rate: {advanced} samples in 400 ms"
    );

    // Paused means the device plays nothing, so the master clock must not move.
    assert!(ao.set_active(false), "pause rejected");
    std::thread::sleep(Duration::from_millis(150));
    let paused = ao.position().expect("position went unknown");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        ao.position(),
        Some(paused),
        "position moved while paused: {paused}"
    );

    ao.write(&samples).expect("write after pause");
    assert!(ao.set_active(true), "resume rejected");
    std::thread::sleep(Duration::from_millis(200));
    let resumed = ao.position().expect("position went unknown");
    assert!(
        resumed > paused,
        "clock did not resume: {paused} -> {resumed}"
    );
}

/// No reachable daemon must fail the open cleanly and quickly, never hang.
#[test]
#[ignore = "needs libengine_audio.so and a PipeWire daemon"]
fn no_daemon_opens_nothing() {
    assert!(AoSession::probe(), "plugin not loadable");
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("PIPEWIRE_REMOTE", "/nonexistent") };
    let start = Instant::now();
    let session = AoSession::open(RATE, CHANNELS);
    let elapsed = start.elapsed();
    unsafe { std::env::remove_var("PIPEWIRE_REMOTE") };

    assert!(session.is_none(), "opened a stream with no daemon");
    assert!(elapsed < Duration::from_secs(2), "open took {elapsed:?}");
    eprintln!("open with no daemon failed in {elapsed:?}");
}
