//! End-to-end playback timing, without a UI. Both files run through the same
//! checks: `test_baseline.mp4` has no audio and is therefore always wall-paced,
//! `test_av.mp4` uses the audio clock when a PipeWire daemon is around and falls
//! back to the same wall path when it is not -- so neither test is `#[ignore]`d.
//!
//! ```text
//! cargo test -p engine --release --test playback -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::{DecodeSession, Frame, PlaybackSession};

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
    while let Some(frame) = session.try_frame() {
        if let Some(previous) = *last_index {
            assert_eq!(
                frame.index,
                previous + 1,
                "frames must arrive in index order"
            );
        }
        *last_index = Some(frame.index);
        // Stop at the first frame past the clock; the real app holds it for the
        // next tick, we simply stop draining.
        if f64::from(frame.index) > target {
            return;
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
fn next_frame(session: &mut PlaybackSession, what: &str) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = session.try_frame() {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame after {what}");
        sleep(Duration::from_millis(4));
    }
}

fn next_index(session: &mut PlaybackSession, what: &str) -> u32 {
    next_frame(session, what).index
}

/// Ticks like a front-end but takes every frame as fast as the decoder makes
/// them, until the timeline is played out. Returns how many arrived and the
/// last index.
fn drain_to_eof(session: &mut PlaybackSession) -> (u32, Option<u32>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut count, mut last) = (0, None);
    loop {
        session.tick();
        while let Some(frame) = session.try_frame() {
            count += 1;
            last = Some(frame.index);
        }
        if session.is_eos() {
            return (count, last);
        }
        assert!(Instant::now() < deadline, "still draining after 20 s");
        sleep(Duration::from_millis(4));
    }
}

/// The pixels of one source frame, decoded on its own -- the reference a
/// timeline frame is checked against.
fn source_frame(path: &Path, index: u32) -> Vec<u8> {
    let (_, rx, _cancel) = DecodeSession::open_range(path, index, index + 1).expect("open_range");
    let frame = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("reference frame");
    assert_eq!(frame.index, index, "open_range starts where it is told");
    frame.bgra
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
        next_index(&mut session, "seek(3.0)"),
        (3.0 * fps) as u32,
        "the first frame after a seek is the target frame"
    );
    sleep(Duration::from_millis(200));
    assert!(session.now() > 3.05, "clock stalled at {}", session.now());

    // Run it out, then seek back: the old workers are gone, not steered.
    let last_frame = session.meta().frame_count - 1;
    assert_eq!(drain_to_eof(&mut session).1, Some(last_frame));
    session.seek(0.0);
    assert_eq!(next_index(&mut session, "seek(0.0) after EOF"), 0);
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
    assert_eq!(
        next_index(&mut session, "seek(999.0)"),
        meta.frame_count - 1
    );

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
    assert_eq!(
        next_index(&mut session, "5 rapid seeks"),
        (2.0 * fps) as u32
    );

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
    let first = next_index(&mut session, "seek(3.0)");
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

/// The edit list end to end: two cuts, drop the middle clip, play the result.
/// What comes out is one contiguous timeline whose *pictures* skip the hole --
/// no audio needed, so this runs anywhere.
#[test]
fn edits_traverse_cuts() {
    let path = asset("test_baseline.mp4");
    let mut session = PlaybackSession::open(&path).expect("open");
    let (fps, total) = (session.meta().frame_rate, session.meta().frame_count);
    let whole = f64::from(total) / fps;
    assert!((session.timeline_duration() - whole).abs() < 1e-9);
    assert_eq!(session.clip_spans(), vec![(0.0, whole)]);

    assert!(session.cut_at(2.0), "cut at 2 s");
    assert!(session.cut_at(3.5), "cut at 3.5 s");
    assert!(
        !session.cut_at(2.0),
        "the same frame twice: already a boundary"
    );
    assert!(!session.cut_at(0.0), "nothing before the first frame");
    assert_eq!(session.clip_spans().len(), 3);
    // Cuts are metadata: the timeline is the same length as before.
    assert!((session.timeline_duration() - whole).abs() < 1e-9);

    let (cut, hole_end) = ((2.0 * fps) as u32, (3.5 * fps) as u32);
    assert!(session.delete_clip(1), "drop the middle clip");
    let kept = total - (hole_end - cut);
    assert_eq!(session.clip_spans().len(), 2);
    assert!(
        (session.timeline_duration() - f64::from(kept) / fps).abs() < 1e-9,
        "duration after the delete: {}",
        session.timeline_duration()
    );

    // Play the whole edited timeline from the top.
    session.seek(0.0);
    session.play();
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut expect, mut boundary) = (0, None);
    loop {
        session.tick();
        while let Some(frame) = session.try_frame() {
            assert_eq!(frame.index, expect, "timeline indices must be contiguous");
            if frame.index == cut {
                boundary = Some(frame.bgra);
            }
            expect += 1;
        }
        if session.is_eos() {
            break;
        }
        assert!(Instant::now() < deadline, "still draining after 20 s");
        sleep(Duration::from_millis(4));
    }
    assert_eq!(expect, kept, "the edited timeline, whole and nothing but");
    // From here the playhead has to stand still: an edit reseeks to *now*, so a
    // running clock would move the frame these assertions expect.
    session.pause();

    // The frames behind those indices skipped the deleted range: the picture at
    // the boundary is source `hole_end`, not the source frame with that number.
    let boundary = boundary.expect("no frame at the cut");
    assert!(
        boundary == source_frame(&path, hole_end),
        "the boundary frame is not source {hole_end}"
    );
    assert!(
        boundary != source_frame(&path, cut),
        "the deleted range is still being decoded"
    );

    // Seek into the surviving second clip: timeline 3.0 s is source 3.0 s + the
    // 1.5 s hole.
    session.seek(3.0);
    let landed = next_frame(&mut session, "seek(3.0) after edits");
    assert_eq!(landed.index, (3.0 * fps) as u32);
    assert!(
        landed.bgra == source_frame(&path, (4.5 * fps) as u32),
        "the seek landed on the wrong source frame"
    );
    assert!(!session.is_eos(), "a seek revives a played-out session");

    assert!(session.undo(), "undo the delete");
    assert_eq!(session.clip_spans().len(), 3);
    assert!(
        (session.timeline_duration() - whole).abs() < 1e-9,
        "undo did not restore the duration: {}",
        session.timeline_duration()
    );
    // The undo reseeks to the (still paused) playhead, and on the restored
    // timeline 3.0 s is source 3.0 s again -- the hole is back.
    let restored = next_frame(&mut session, "undo");
    assert_eq!(restored.index, (3.0 * fps) as u32);
    assert!(
        restored.bgra == source_frame(&path, (3.0 * fps) as u32),
        "undo did not restore the mapping"
    );
}

/// Copy and paste. A clipboard clip is a pair of *source* frame numbers, so the
/// pasted stretch has to decode its own range -- not the frames the timeline
/// used to hold at that position.
#[test]
fn paste_duplicates_a_clip_at_the_playhead() {
    let path = asset("test_baseline.mp4");
    let mut session = PlaybackSession::open(&path).expect("open");
    let (fps, total) = (session.meta().frame_rate, session.meta().frame_count);
    let whole = f64::from(total) / fps;

    assert!(session.cut_at(2.0), "cut at 2 s");
    let copied = session.clip_at(0).expect("clip 0");
    let copied_len = copied.len();
    assert_eq!(copied_len, (2.0 * fps) as u32);
    assert!(session.clip_at(2).is_none(), "only two clips so far");

    // Mid-clip: the paste splits clip 1 and lands between the halves.
    let at = (4.0 * fps) as u32;
    assert!(session.paste_at(4.0, copied), "paste at 4 s");
    assert_eq!(session.clip_spans().len(), 4);
    let grown = total + copied_len;
    assert!(
        (session.timeline_duration() - f64::from(grown) / fps).abs() < 1e-9,
        "duration after the paste: {}",
        session.timeline_duration()
    );

    // Play the edited timeline from the top, keeping the two seam frames.
    session.seek(0.0);
    session.play();
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut expect, mut pasted, mut resumed) = (0, None, None);
    loop {
        session.tick();
        while let Some(frame) = session.try_frame() {
            assert_eq!(frame.index, expect, "timeline indices must be contiguous");
            if frame.index == at {
                pasted = Some(frame.bgra);
            } else if frame.index == at + copied_len {
                resumed = Some(frame.bgra);
            }
            expect += 1;
        }
        if session.is_eos() {
            break;
        }
        assert!(Instant::now() < deadline, "still draining after 30 s");
        sleep(Duration::from_millis(4));
    }
    assert_eq!(expect, grown, "the pasted timeline, whole and nothing but");
    // As above: an edit reseeks to *now*, so stop the clock before asserting.
    session.pause();

    // At the paste position the picture is the copied clip's first frame, and
    // the split-off remainder resumes exactly where it was interrupted.
    assert!(
        pasted.expect("no frame at the paste") == source_frame(&path, copied.in_frame),
        "the paste is not showing source {}",
        copied.in_frame
    );
    assert!(
        resumed.expect("no frame after the paste") == source_frame(&path, at),
        "the timeline did not resume at source {at}"
    );

    assert!(session.undo(), "undo the paste");
    assert_eq!(session.clip_spans().len(), 2, "one step, back to the cut");
    assert!(
        (session.timeline_duration() - whole).abs() < 1e-9,
        "undo did not restore the duration: {}",
        session.timeline_duration()
    );
}

/// Editing while the device is running. A cut must not disturb playback at all
/// and a delete reseeks under it -- in both cases the audio keeps coming, which
/// is what stops `tick` from handing the timeline back to wall time.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn edits_keep_the_audio_clock() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let fps = session.meta().frame_rate;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(700));
    let before = session.now();
    assert!(before > 0.3, "clock stalled at {before:.3}s");

    // Metadata only: no worker is touched, so the indices stay contiguous
    // across the boundary `run_for` is about to cross.
    assert!(session.cut_at(1.5), "cut at 1.5 s");
    run_for(&mut session, &mut last_index, Duration::from_millis(900));
    let after_cut = session.now();
    assert!(
        after_cut > before + 0.6,
        "the clock stopped across the cut: {before:.3}s -> {after_cut:.3}s"
    );
    assert!(
        last_index.map(|i| f64::from(i) > 1.5 * fps) == Some(true),
        "playback never reached the cut: {last_index:?}"
    );

    // Deleting the head clip moves everything after it, so the session reseeks
    // under the playhead: same timeline position, new source frame, and the
    // frame numbering restarts at the landing frame.
    assert!(session.delete_clip(0), "drop the head clip");
    let landed = session.now();
    assert!(
        (landed - after_cut).abs() < 0.2,
        "the delete moved the playhead: {after_cut:.3}s -> {landed:.3}s"
    );
    let first = next_frame(&mut session, "the delete reseek").index;
    assert!(
        first.abs_diff((landed * fps) as u32) <= 1,
        "the delete landed at {first} instead of the playhead ({landed:.3}s)"
    );
    // No EOS assertion here: `run_for` drains a frame per 8 ms call, far faster
    // than real time (ledger), so how much timeline is left after it is a race.
    let mut after_delete = Some(first);
    run_for(&mut session, &mut after_delete, Duration::from_millis(500));
    assert!(after_delete > Some(first), "no frames after the delete");
    assert!(
        session.now() > landed + 0.3,
        "the clock stopped after the delete: {landed:.3}s -> {:.3}s",
        session.now()
    );
    eprintln!(
        "edits: {before:.3}s -> cut -> {after_cut:.3}s -> delete -> {:.3}s of a {:.3}s timeline",
        session.now(),
        session.timeline_duration()
    );
}
