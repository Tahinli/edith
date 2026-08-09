//! End-to-end playback timing, without a UI. Both files run through the same
//! checks: `test_baseline.mp4` has no audio and is therefore always wall-paced,
//! `test_av.mp4` uses the audio clock when a PipeWire daemon is around and falls
//! back to the same wall path when it is not -- so neither test is `#[ignore]`d.
//!
//! ```text
//! cargo test -p engine --release --test playback -- --nocapture
//! ```

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One render tick of a front-end: advance the clock, take whatever frames are
/// due. Returns the highest frame index seen so far.
fn pump(session: &mut PlaybackSession, last_index: &mut Option<u32>) {
    session.tick();
    let target = session.now() * session.meta().frame_rate;
    loop {
        match session.frames().try_recv() {
            Ok(frame) => {
                if let Some(previous) = *last_index {
                    assert_eq!(
                        frame.index,
                        previous + 1,
                        "frames must arrive in index order"
                    );
                }
                *last_index = Some(frame.index);
                // Stop at the first frame past the clock; the real app holds it
                // for the next tick, we simply stop draining.
                if f64::from(frame.index) > target {
                    return;
                }
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
        }
    }
}

/// Pumps for `duration` of wall time at roughly a display refresh.
fn run_for(session: &mut PlaybackSession, last_index: &mut Option<u32>, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        pump(session, last_index);
        sleep(Duration::from_millis(8));
    }
}

fn plays_at_real_speed(name: &str) {
    let path = asset(name);
    let mut session = PlaybackSession::open(&path).expect("open");
    assert_eq!(session.now(), 0.0, "a fresh session starts at zero");
    assert!(!session.is_playing(), "a fresh session starts paused");
    let mut last_index = None;

    session.play();
    let started = Instant::now();
    run_for(&mut session, &mut last_index, Duration::from_secs(2));
    let (played, wall) = (session.now(), started.elapsed().as_secs_f64());
    eprintln!("{name}: clock {played:.3}s over {wall:.3}s wall");
    assert!(
        (played - wall).abs() < wall * 0.15,
        "clock ran at the wrong speed: {played:.3}s in {wall:.3}s"
    );
    assert!(last_index.is_some(), "no frames arrived");

    session.pause();
    let frozen = session.now();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    assert!(
        (session.now() - frozen).abs() < 0.025,
        "paused clock moved: {frozen:.3}s -> {:.3}s",
        session.now()
    );

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(500));
    let resumed = session.now();
    assert!(
        resumed > frozen + 0.3,
        "clock did not resume: {frozen:.3}s -> {resumed:.3}s"
    );
    // No time was invented across the pause: 2.5 s of playing, not 2.8 s.
    assert!(
        resumed < wall + 0.7,
        "the pause leaked into the timeline: {resumed:.3}s"
    );
}

#[test]
fn video_only_plays_at_real_speed() {
    plays_at_real_speed("test_baseline.mp4");
}

#[test]
fn audio_video_plays_at_real_speed() {
    plays_at_real_speed("test_av.mp4");
}
