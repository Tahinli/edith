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

/// The next frame off the (post-seek, freshly opened) channel.
fn next_index(session: &PlaybackSession, what: &str) -> u32 {
    match session.frames().recv_timeout(Duration::from_secs(10)) {
        Ok(frame) => frame.index,
        Err(e) => panic!("no frame after {what}: {e:?}"),
    }
}

/// Ticks like a front-end but takes every frame as fast as the decoder makes
/// them, until the worker exits. Returns how many arrived and the last index.
fn drain_to_eof(session: &mut PlaybackSession) -> (u32, Option<u32>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut count, mut last) = (0, None);
    loop {
        session.tick();
        loop {
            match session.frames().try_recv() {
                Ok(frame) => {
                    count += 1;
                    last = Some(frame.index);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return (count, last),
            }
        }
        assert!(Instant::now() < deadline, "still draining after 20 s");
        sleep(Duration::from_millis(4));
    }
}

fn threads() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|n| n.trim().parse().ok())
        .expect("Threads: in /proc/self/status")
}

/// Wall-clock seek: no audio track, so this runs anywhere.
#[test]
fn seek_repositions_video_and_clock() {
    let mut session = PlaybackSession::open(asset("test_baseline.mp4")).expect("open");
    let fps = session.meta().frame_rate;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(500));

    session.seek(3.0);
    assert!(
        (session.now() - 3.0).abs() < 0.05,
        "clock at {}",
        session.now()
    );
    assert_eq!(
        next_index(&session, "seek(3.0)"),
        (3.0 * fps) as u32,
        "the first frame after a seek is the target frame"
    );
    sleep(Duration::from_millis(200));
    assert!(session.now() > 3.05, "clock stalled at {}", session.now());

    // Run it out, then seek back: the old workers are gone, not steered.
    let last_frame = session.meta().frame_count - 1;
    assert_eq!(drain_to_eof(&mut session).1, Some(last_frame));
    session.seek(0.0);
    assert_eq!(next_index(&session, "seek(0.0) after EOF"), 0);
    assert!(session.now() < 0.2, "clock at {}", session.now());
}

/// Past the end lands on the last frame instead of hanging, and the clock keeps
/// running -- with audio that means falling through to wall time, because the
/// device is never fed another sample.
#[test]
fn seek_past_the_end_clamps() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let meta = *session.meta();
    let duration = f64::from(meta.frame_count) / meta.frame_rate;

    session.play();
    session.seek(999.0);
    assert!(
        (session.now() - duration).abs() < 0.05,
        "clock at {} for a {duration}s file",
        session.now()
    );
    assert_eq!(next_index(&session, "seek(999.0)"), meta.frame_count - 1);

    let landed = session.now();
    let deadline = Instant::now() + Duration::from_secs(2);
    while session.now() <= landed + 0.05 {
        session.tick();
        assert!(Instant::now() < deadline, "clock froze at {landed}");
        sleep(Duration::from_millis(8));
    }
}

/// Scrubbing: seeks with no time in between must neither panic nor leave the
/// abandoned workers behind, and the last one still wins.
#[test]
fn rapid_seeks_settle_on_the_last_one() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let fps = session.meta().frame_rate;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    let before = threads();

    for t in [4.0, 1.0, 3.5, 0.5, 2.0] {
        session.seek(t);
    }
    assert_eq!(next_index(&session, "5 rapid seeks"), (2.0 * fps) as u32);

    sleep(Duration::from_millis(500)); // abandoned workers exit within an AU
    let after = threads();
    eprintln!("rapid seeks: {before} threads -> {after}");
    assert!(
        after <= before + 3,
        "workers piled up: {before} threads -> {after}"
    );
}

/// The audio path proper: needs a PipeWire daemon and the output plugin next to
/// the test binary (`LD_LIBRARY_PATH=target/release`).
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn seek_keeps_the_audio_clock() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let meta = *session.meta();
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_secs(1));

    session.seek(3.0);
    assert!(
        (session.now() - 3.0).abs() < 0.05,
        "clock at {}",
        session.now()
    );
    let first = next_index(&session, "seek(3.0)");
    assert_eq!(first, (3.0 * meta.frame_rate) as u32);
    let mut after_seek = None;
    run_for(&mut session, &mut after_seek, Duration::from_millis(600));
    let advanced = session.now();
    eprintln!("seek(3.0) -> {advanced:.3}s, frames {first}..={after_seek:?}");
    assert!(
        (3.3..3.9).contains(&advanced),
        "clock did not run at real speed after the seek: {advanced:.3}s"
    );

    // Everything from the target on, and nothing before it: ~2 s of frames.
    let (drained, last) = drain_to_eof(&mut session);
    let total = 1 + after_seek.map_or(0, |i| i - first) + drained;
    eprintln!("frames from 3.0s to EOF: {total}");
    // `run_for` pumps faster than real time, so it may have hit EOF by itself.
    assert_eq!(last.or(after_seek), Some(meta.frame_count - 1), "no EOF");
    assert!(
        (55..=65).contains(&total),
        "expected ~60 frames left, got {total}"
    );

    // A full second run after EOF, from zero.
    session.seek(0.0);
    session.play();
    let mut second = None;
    run_for(&mut session, &mut second, Duration::from_millis(800));
    assert_eq!(second.map(|i| i > 0), Some(true), "second run stalled");
    assert!(session.now() > 0.5, "second run clock at {}", session.now());
    assert_eq!(
        drain_to_eof(&mut session).1.or(second),
        Some(meta.frame_count - 1),
        "second run did not finish"
    );
}
