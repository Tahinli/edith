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

use engine::project::Lane;
use engine::{DecodeSession, Frame, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Opens a session the way every test here wants one: silent. The suite plays
/// real files through the real device, so without this a `cargo test` is two
/// minutes of noise out of the speakers of whoever ran it.
///
/// Muting is the gain knob, not a pause and not a fake device: the stream is
/// still connected, the daemon still schedules it, the samples are still
/// decoded, written, and handed over -- only the last multiply before the
/// device is zero. Every clock, `fed` and `played_out` assertion below is
/// therefore measuring exactly what it measured when this was audible.
fn open(path: impl AsRef<Path>) -> PlaybackSession {
    let session = PlaybackSession::open(path).expect("open");
    // False for the silent fixtures, which have no device to quieten.
    session.set_gain(0.0);
    session
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
    let mut session = open(&path);
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
    let mut session = open(asset("test_baseline.mp4"));
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
    let mut session = open(asset("test_av.mp4"));
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
    let mut session = open(asset("test_av.mp4"));
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
    let mut session = open(asset("test_av.mp4"));
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
    let mut session = open(&path);
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
    assert!(session.delete_clip(Lane::V1, 1), "drop the middle clip");
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
    let mut session = open(&path);
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

/// Pasting at the playhead while it is running: like a delete this moves every
/// following frame, so the session reseeks under itself -- what must not happen
/// is a stall, and the grown timeline still plays out whole. No audio needed.
#[test]
fn paste_while_playing_does_not_stall() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let (fps, total) = (session.meta().frame_rate, session.meta().frame_count);
    let mut last_index = None;

    assert!(session.cut_at(2.0), "cut at 2 s");
    let copied = session.clip_at(0).expect("clip 0");
    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    assert!(last_index.is_some(), "no frames before the paste");

    // Into the clip under the running playhead: the paste splits it in two and
    // lands between the halves.
    assert!(
        session.paste_at(session.now(), copied),
        "paste at the playhead"
    );
    assert!(session.is_playing(), "the paste stopped the clock");
    assert_eq!(session.clip_spans().len(), 4);
    let grown = total + copied.len();
    assert!(
        (session.timeline_duration() - f64::from(grown) / fps).abs() < 1e-9,
        "duration after the paste: {}",
        session.timeline_duration()
    );

    // As after a mid-play import: the reseek makes the indices step backwards
    // once, so this drains without `pump`'s index-order check.
    let (count, last) = drain_to_eof(&mut session);
    eprintln!("paste while playing: {count} more frames, last {last:?}");
    assert_eq!(last, Some(grown - 1), "the grown timeline did not play out");
}

/// Pasting past the end of a played-out timeline appends, and the reseek every
/// edit does clears `eos` -- so the session plays on into the pasted clip
/// instead of staying dead.
#[test]
fn paste_at_eos_revives_the_session() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let total = session.meta().frame_count;

    assert!(session.cut_at(2.0), "cut at 2 s");
    let copied = session.clip_at(0).expect("clip 0");
    session.play();
    assert_eq!(drain_to_eof(&mut session).1, Some(total - 1));
    assert!(session.is_eos());

    assert!(
        session.paste_at(999.0, copied),
        "paste past the end appends"
    );
    assert!(!session.is_eos(), "the paste did not revive the session");
    assert_eq!(
        session.clip_spans().len(),
        3,
        "two clips plus the pasted one"
    );
    // No assertion on *where* it resumes: an edit reseeks to `now()`, and at EOS
    // that is wherever the wall clock got to while draining -- a race. What is
    // fixed is that frames come again and the whole grown timeline plays out.
    next_frame(&mut session, "paste at EOS");
    assert_eq!(
        drain_to_eof(&mut session).1,
        Some(total + copied.len() - 1),
        "the pasted clip did not play out"
    );
}

/// Editing while the device is running. A cut must not disturb playback at all
/// and a delete reseeks under it -- in both cases the audio keeps coming, which
/// is what stops `tick` from handing the timeline back to wall time.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn edits_keep_the_audio_clock() {
    let mut session = open(asset("test_av.mp4"));
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
    assert!(session.delete_clip(Lane::V1, 0), "drop the head clip");
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

/// Frame count straight from the container, without decoding anything.
fn frame_count(path: &Path) -> u32 {
    engine::demux::Demuxer::open(path)
        .expect("demux")
        .0
        .frame_count
}

/// Import: a second file appended to the timeline, played through as one
/// stream. No audio needed for the pictures, so this runs anywhere.
#[test]
fn import_appends_a_second_source_and_plays_across_the_join() {
    let (av, av2) = (asset("test_av.mp4"), asset("test_av2.mp4"));
    let mut session = open(&av);
    let (fps, first) = (session.meta().frame_rate, session.meta().frame_count);
    let second = frame_count(&av2);
    assert_eq!((first, second), (150, 120), "fixtures changed");

    session.import(&av2).expect("test_av2 matches test_av");
    assert_eq!(session.clip_spans().len(), 2);
    assert_eq!(
        session
            .clip_spans_by_source()
            .iter()
            .map(|&(.., s)| s)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "the appended clip plays the imported file"
    );
    assert!(
        (session.timeline_duration() - 9.0).abs() < 1e-9,
        "5 s + 4 s: {}",
        session.timeline_duration()
    );

    // Play the joined timeline whole, keeping the picture at the join.
    session.seek(0.0);
    session.play();
    let deadline = Instant::now() + Duration::from_secs(30);
    let (mut expect, mut boundary) = (0, None);
    loop {
        session.tick();
        while let Some(frame) = session.try_frame() {
            assert_eq!(
                frame.index, expect,
                "timeline indices must be contiguous across a source join"
            );
            if frame.index == first {
                boundary = Some(frame.bgra);
            }
            expect += 1;
        }
        if session.is_eos() {
            break;
        }
        assert!(Instant::now() < deadline, "still draining after 30 s");
        sleep(Duration::from_millis(4));
    }
    assert_eq!(expect, first + second, "270 frames, whole and nothing but");
    // As everywhere else: stop the clock before asserting on landing frames.
    session.pause();
    assert!(
        boundary.expect("no frame at the join") == source_frame(&av2, 0),
        "timeline frame {first} is not the imported file's frame 0"
    );

    // Edits across the join keep every clip pointing at its own file: cut one
    // second into the imported clip, then drop the whole original.
    assert!(session.cut_at(6.0), "cut 1 s into the imported clip");
    assert_eq!(session.clip_spans().len(), 3);
    assert!(session.delete_clip(Lane::V1, 0), "drop the first source");
    assert_eq!(session.clip_spans_by_source()[0].2, 1);
    assert!(
        (session.timeline_duration() - f64::from(second) / fps).abs() < 1e-9,
        "only the imported file is left: {}",
        session.timeline_duration()
    );
    session.seek(0.0);
    let landed = next_frame(&mut session, "seek(0.0) after the delete");
    assert_eq!(landed.index, 0);
    assert!(
        landed.bgra == source_frame(&av2, 0),
        "the delete lost the source mapping"
    );

    assert!(session.undo(), "undo the delete");
    assert_eq!(session.clip_spans().len(), 3);
    let restored = next_frame(&mut session, "undo");
    assert_eq!(restored.index, 0);
    assert!(
        restored.bgra == source_frame(&av, 0),
        "undo did not restore the first source"
    );
}

/// One timeline, one set of parameters: everything else is refused by name and
/// changes nothing.
#[test]
fn import_refuses_what_does_not_match() {
    let mut session = open(asset("test_av.mp4"));

    // A different *codec* cannot join: one timeline is one kind of source.
    let err = session
        .import(&asset("test_vp9.mp4"))
        .expect_err("VP9 must be refused")
        .to_string();
    assert!(err.contains("VP9"), "refusal must name the codec: {err}");

    // A different resolution is no longer among the refusals -- it is placed on
    // the project canvas instead -- but this file is also silent, and that is.
    let err = session
        .import(&asset("test_mismatch.mp4"))
        .expect_err("a silent file must be refused")
        .to_string();
    assert!(err.contains("audio"), "refusal must name the audio: {err}");
    assert!(
        !err.contains("640x360"),
        "a resolution of its own is not a refusal any more: {err}"
    );

    // Same size and rate, no audio track: the timeline has one.
    let err = session
        .import(&asset("test_baseline.mp4"))
        .expect_err("a silent file must be refused")
        .to_string();
    assert!(err.contains("audio"), "refusal must name the audio: {err}");

    assert!(
        session.import(&asset("no_such_file.mp4")).is_err(),
        "a missing file is an error, not a panic"
    );
    assert_eq!(session.clip_spans().len(), 1, "a refusal changes nothing");
    assert!((session.timeline_duration() - 5.0).abs() < 1e-9);

    // The mirror: audio into a silent timeline is refused just as loudly.
    let mut silent = open(asset("test_baseline.mp4"));
    let err = silent
        .import(&asset("test_av.mp4"))
        .expect_err("audio into a silent timeline")
        .to_string();
    assert!(err.contains("audio"), "refusal must name the audio: {err}");
}

/// The user-facing point of a project resolution: media of two sizes on one
/// timeline, each composed onto the project's own canvas.
///
/// 640x360 joins a 1280x720 timeline (it would have been refused before this
/// slice), and both clips come out of `try_frame` at the project's size --
/// which is what makes the export, the window and the renderer one shape.
#[test]
fn media_of_two_resolutions_share_one_timeline() {
    let mut session = open(asset("test_baseline.mp4"));
    assert_eq!(session.resolution(), (1280, 720), "source 0's picture");
    assert_eq!(session.native_resolution(), (1280, 720));
    session
        .import(&asset("test_mismatch.mp4"))
        .expect("640x360 must join a 1280x720 timeline");
    // 150 frames of the first file, then 60 of the second, at 30 fps.
    assert_eq!(session.clip_spans().len(), 2);
    assert!((session.timeline_duration() - 7.0).abs() < 1e-9);

    for (at, what) in [(1.0, "the 1280x720 clip"), (6.0, "the 640x360 clip")] {
        session.seek(at);
        let frame = next_frame(&mut session, what);
        assert_eq!(
            (frame.width, frame.height),
            (1280, 720),
            "{what} did not come out at the project resolution"
        );
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4);
    }
}

/// The geometry, asserted on the pixels: on a canvas whose aspect the media does
/// not share, a `Fit` clip is letterboxed -- bars exactly black, picture where
/// `scale::fit_rect` puts it -- and `Fill` crops the bars away.
#[test]
fn a_fitted_clip_is_letterboxed_and_a_filled_one_is_not() {
    let mut session = open(asset("test_baseline.mp4"));
    session
        .import(&asset("test_mismatch.mp4"))
        .expect("640x360 joins");
    // A 4:3 project: both clips are 16:9, so both are letterboxed on it. 960x720
    // holds the whole 640x360 picture as 960x540 with 90 rows of bar either way
    // (fit_rect's own arithmetic, asserted in scale.rs).
    assert!(session.set_resolution(960, 720), "4:3 project");
    session.seek(6.0);
    let frame = next_frame(&mut session, "the 640x360 clip on a 4:3 canvas");
    assert_eq!((frame.width, frame.height), (960, 720));
    let row = |frame: &Frame, y: usize| {
        frame.bgra[y * 960 * 4..][..960 * 4]
            .chunks_exact(4)
            .map(|px| (px[0], px[1], px[2]))
            .collect::<Vec<_>>()
    };
    let black = |row: &[(u8, u8, u8)]| row.iter().all(|&px| px == (0, 0, 0));
    assert!(black(&row(&frame, 0)), "top bar is not black");
    assert!(black(&row(&frame, 89)), "the bar stops one row early");
    assert!(black(&row(&frame, 719)), "bottom bar is not black");
    assert!(
        !black(&row(&frame, 360)),
        "the picture area is black: nothing was composed"
    );
    // Same clip, filled: the canvas is covered, so no row is a bar.
    let (lane, idx) = session.video_clip_at(6.0).expect("a clip at 6 s");
    assert!(session.set_fit(lane, idx, engine::scale::FitPolicy::Fill));
    session.seek(6.0);
    let filled = next_frame(&mut session, "the same clip filled");
    assert_eq!((filled.width, filled.height), (960, 720));
    assert!(!black(&row(&filled, 0)), "Fill left a top bar");
    assert!(!black(&row(&filled, 719)), "Fill left a bottom bar");
    assert_ne!(
        row(&frame, 360),
        row(&filled, 360),
        "Fill showed the same pixels as Fit: the crop did nothing"
    );
}

/// The clip that pays nothing: a project at its media's own size must hand the
/// decoder's bytes through untouched, not merely equal ones. Same frame, same
/// bytes, whether or not the composition path exists.
#[test]
fn a_project_at_the_media_size_is_the_decoder_untouched() {
    let mut session = open(asset("test_baseline.mp4"));
    session.seek(1.0);
    let frame = next_frame(&mut session, "seek to 1 s");
    assert_eq!((frame.width, frame.height), (1280, 720));
    assert_eq!(
        frame.bgra,
        source_frame(&asset("test_baseline.mp4"), frame.index),
        "the pass-through path changed a byte"
    );
}

/// The same file twice is two clips of one source -- a source index is handed
/// out once and reused, which is what keeps clipboard clips valid forever.
#[test]
fn importing_one_file_twice_reuses_its_source() {
    let mut session = open(asset("test_av.mp4"));
    let av2 = asset("test_av2.mp4");
    session.import(&av2).expect("first import");
    session.import(&av2).expect("second import");
    assert_eq!(
        session
            .clip_spans_by_source()
            .iter()
            .map(|&(.., s)| s)
            .collect::<Vec<_>>(),
        vec![0, 1, 1],
        "one new source, two clips of it"
    );
    assert!(
        (session.timeline_duration() - 13.0).abs() < 1e-9,
        "5 s + 4 s + 4 s: {}",
        session.timeline_duration()
    );
}

/// Importing after the timeline has been played out revives the session at the
/// join, not at wherever the free-running clock got to.
#[test]
fn import_at_eos_resumes_into_the_new_clip() {
    let av2 = asset("test_av2.mp4");
    let mut session = open(asset("test_av.mp4"));
    let first = session.meta().frame_count;

    session.play();
    assert_eq!(drain_to_eof(&mut session).1, Some(first - 1));
    assert!(session.is_eos());

    session.import(&av2).expect("import at EOS");
    assert!(!session.is_eos(), "the import did not revive the session");
    let landed = next_frame(&mut session, "import at EOS");
    assert_eq!(landed.index, first, "resumed at the join");
    assert!(
        landed.bgra == source_frame(&av2, 0),
        "the resume is not showing the imported file"
    );
    assert_eq!(
        drain_to_eof(&mut session).1,
        Some(first + frame_count(&av2) - 1),
        "the appended clip did not play out"
    );
}

/// A source deleted mid-session: the clip that names it contributes no pictures
/// and the timeline still ends, instead of stalling on the missing file.
#[test]
fn a_vanished_source_is_skipped() {
    let scratch =
        std::env::temp_dir().join(format!("video_editor_vanish_{}.mp4", std::process::id()));
    std::fs::copy(asset("test_av2.mp4"), &scratch).expect("copy the fixture");
    let mut session = open(asset("test_av.mp4"));
    let first = session.meta().frame_count;
    session.import(&scratch).expect("import the copy");
    std::fs::remove_file(&scratch).expect("unlink");

    session.seek(0.0);
    session.play();
    let (count, last) = drain_to_eof(&mut session);
    assert_eq!(last, Some(first - 1), "the vanished clip made pictures");
    assert_eq!(count, first, "the first source still played whole");
    assert!(session.is_eos(), "the timeline still ends");
}

/// Importing while the device is running: the import reseeks under the
/// playhead, so what must not happen is a stall -- the joined timeline still
/// plays out whole. No audio needed, so this runs anywhere.
#[test]
fn import_during_play_does_not_stall() {
    let (av, av2) = (asset("test_av.mp4"), asset("test_av2.mp4"));
    let mut session = open(&av);
    let first = session.meta().frame_count;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    assert!(last_index.is_some(), "no frames before the import");

    session.import(&av2).expect("import while playing");
    assert_eq!(session.clip_spans().len(), 2);
    assert!(session.is_playing(), "the import stopped the clock");

    // From here on the indices go *backwards* once: `run_for` decodes far ahead
    // of the clock (ledger), and the import reseeks to `now()` -- which is why
    // this drains with `drain_to_eof` instead of the order-checking `pump`.
    let (count, last) = drain_to_eof(&mut session);
    eprintln!("import while playing: {count} more frames, last {last:?}");
    assert_eq!(
        last,
        Some(first + frame_count(&av2) - 1),
        "the joined timeline did not play out"
    );
}

/// An import is *one* undo step: the appended clip goes, the source entry stays
/// behind (documented, harmless -- source indexes are forever), and the session
/// is still playable on the restored timeline.
#[test]
fn undo_after_import_is_one_step() {
    let av2 = asset("test_av2.mp4");
    let mut session = open(asset("test_av.mp4"));
    session.import(&av2).expect("import");
    assert_eq!(session.clip_spans().len(), 2);
    assert!((session.timeline_duration() - 9.0).abs() < 1e-9);

    assert!(session.undo(), "undo the import");
    assert_eq!(session.clip_spans().len(), 1, "one step, back to one clip");
    assert!(
        (session.timeline_duration() - 5.0).abs() < 1e-9,
        "undo did not restore the duration: {}",
        session.timeline_duration()
    );

    // The orphan source is only observable on the project -- `PlaybackSession`
    // exposes sources through `clip_spans_by_source`, and the undone clip is
    // exactly the one that named it.
    let mut project = engine::Project::single(asset("test_av.mp4"), 150);
    let source = project.import(&av2, 0);
    assert!(project.append_clip(source, 120));
    assert!(project.undo());
    assert_eq!(project.clips().len(), 1);
    assert_eq!(project.sources().len(), 2, "the source entry stays behind");

    // Still playable: the undo reseeked to the (paused) playhead at zero.
    session.play();
    assert_eq!(next_index(&mut session, "undo of an import"), 0);
}

/// The audio path across a source join: one worker spans both files, so the
/// clock must keep running at real speed through the boundary. (Whether it is
/// still the *audio* clock is not observable from outside -- a device that ran
/// dry would fall back to wall time at the same speed -- so this catches a
/// stall, not a silent fallback.)
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn audio_runs_across_a_source_join() {
    let mut session = open(asset("test_av.mp4"));
    session.import(&asset("test_av2.mp4")).expect("import");
    let first = session.meta().frame_count;

    session.seek(4.5); // half a second before the join
    session.play();
    let mut last_index = None;
    run_for(&mut session, &mut last_index, Duration::from_millis(1500));
    let now = session.now();
    eprintln!("join: clock {now:.3}s, frames ..={last_index:?}");
    assert!(
        (5.4..6.6).contains(&now),
        "the clock did not run at real speed across the join: {now:.3}s"
    );
    assert!(
        last_index.map(|i| i > first) == Some(true),
        "playback never crossed the join: {last_index:?}"
    );
    // No EOS assertion: `run_for` takes a frame per 8 ms call, far faster than
    // real time, so the decoder may well have reached 9 s while the clock is at
    // 6 s (ledger; same reason `edits_keep_the_audio_clock` has none).
}

/// A hole in the video lane. Lifting a clip out of one lane leaves a *gap*
/// rather than closing up, and a gap plays: the timeline keeps its length, the
/// frame numbering stays contiguous straight through, and every picture over
/// the hole is black. No audio needed, so this runs anywhere.
#[test]
fn a_video_gap_plays_black_without_shortening_the_timeline() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let (fps, total) = (session.meta().frame_rate, session.meta().frame_count);
    let whole = session.timeline_duration();

    assert!(session.cut_at(2.0), "split at 2 s");
    assert!(session.cut_at(3.5), "split at 3.5 s");
    assert!(
        session.lift_clip(Lane::V1, 1),
        "lift the middle picture out"
    );
    assert_eq!(
        session.clip_spans().len(),
        2,
        "two placements, and a hole between them"
    );
    assert!(
        (session.timeline_duration() - whole).abs() < 1e-9,
        "a lift leaves a gap: the timeline is exactly as long as it was"
    );

    let (hole, hole_end) = ((2.0 * fps) as u32, (3.5 * fps) as u32);
    session.seek(0.0);
    session.play();
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut expect, mut black, mut lit) = (0u32, 0u32, 0u32);
    loop {
        session.tick();
        while let Some(frame) = session.try_frame() {
            assert_eq!(frame.index, expect, "timeline indices must be contiguous");
            let is_black = frame.bgra.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]);
            if (hole..hole_end).contains(&frame.index) {
                assert!(is_black, "frame {} is inside the gap", frame.index);
                black += 1;
            } else {
                lit += 1;
            }
            expect += 1;
        }
        if session.is_eos() {
            break;
        }
        assert!(Instant::now() < deadline, "still draining after 20 s");
        sleep(Duration::from_millis(4));
    }
    assert_eq!(expect, total, "the whole timeline, gap included");
    assert_eq!(black, hole_end - hole, "every frame of the hole was black");
    assert!(lit > 0, "and the clips around it still decoded");

    // The gap is one undo step back to the clip that was there.
    assert!(session.undo());
    assert_eq!(session.clip_spans().len(), 3);
}

/// A hole in the *audio* lane, with the device running: the master clock counts
/// samples the device has been fed, so silence has to be fed as real chunks or
/// the timeline would stall on the hole. Needs a PipeWire daemon and the output
/// plugin next to the test binary (`LD_LIBRARY_PATH=target/release`).
///
/// Muted like every other device test: what it asserts is the clock and the
/// frame numbering, both of which count fed samples and not their loudness. The
/// silence of a gap is proved off the decoder instead, with no device involved,
/// in `audio_segments::a_gap_segment_is_silence_of_its_own_length`.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn an_audio_gap_is_silent_and_the_clock_keeps_counting() {
    let mut session = open(asset("test_av.mp4"));
    assert!(session.cut_at(1.0), "split at 1 s");
    assert!(session.cut_at(2.0), "split at 2 s");
    // Silence over [1, 2) s while the picture plays on -- the two lanes part.
    assert!(session.lift_clip(Lane::A1, 1), "lift the middle sound out");
    assert_eq!(session.clip_spans().len(), 3, "the video lane is untouched");
    let duration = session.timeline_duration();

    session.seek(0.0);
    let mut last_index = None;
    session.play();
    // Straight across the hole: 2.4 s of wall time over a 1 s gap starting at
    // 1 s, so a stall anywhere inside it cannot hide.
    run_for(&mut session, &mut last_index, Duration::from_millis(2_400));
    let now = session.now();
    assert!(
        now > 2.2 && now < duration,
        "the clock stalled on the audio gap at {now:.3}s"
    );
    assert!(
        last_index.map(|i| f64::from(i) > 2.0 * session.meta().frame_rate) == Some(true),
        "pictures stopped over the silence: {last_index:?}"
    );
    eprintln!("audio gap: clock at {now:.3}s of {duration:.3}s, frame {last_index:?}");
}

/// The emptied timeline, end to end: the last clip deletes like any other, what
/// is left plays as black and silence with a duration of zero, it saves and
/// loads back as the project it is, and one undo brings the clip back. Silent
/// fixture, so this runs anywhere.
#[test]
fn an_emptied_timeline_plays_black_saves_loads_and_undoes() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let whole = session.timeline_duration();
    assert!(whole > 0.0);

    // The sole take goes -- picture and sound of it, on every lane at once.
    assert!(
        session.delete_clip(Lane::V1, 0),
        "the last clip deletes like any other"
    );
    assert!(session.is_empty(), "and the timeline is empty");
    assert_eq!(session.timeline_duration(), 0.0);
    assert!(session.clip_spans().is_empty());
    assert_eq!(session.lanes().len(), 2, "the lanes are still there");

    // It scrubs: every seek lands at zero and shows black rather than the
    // picture of the clip that was deleted.
    for t in [0.0, -1.0, whole, 1e9] {
        session.seek(t);
        assert_eq!(session.now(), 0.0, "seek to {t} on an empty timeline");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut black = None;
        while black.is_none() && Instant::now() < deadline {
            session.tick();
            black = session.try_frame();
            sleep(Duration::from_millis(4));
        }
        let frame = black.expect("an empty timeline shows one black frame");
        assert_eq!(frame.index, 0);
        assert!(
            frame.bgra.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]),
            "and it is black"
        );
    }
    // It plays: nothing to show, so it is at its end at once, and nothing hangs.
    session.play();
    let mut last_index = None;
    run_for(&mut session, &mut last_index, Duration::from_millis(200));
    assert!(session.is_eos(), "an empty timeline is played out at once");
    session.pause();

    // It saves and loads back -- still a project, still two lanes, and still
    // naming the file whose frame rate the timeline counts in.
    let dir = std::env::temp_dir().join(format!("ve_empty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let project = dir.join("empty.edith");
    session
        .save_project(&project)
        .expect("save an empty timeline");
    let loaded = PlaybackSession::open_project(&project).expect("load it back");
    assert!(loaded.is_empty(), "what came back is the empty timeline");
    assert_eq!(loaded.lanes().len(), 2);
    assert_eq!(loaded.sources().len(), 1, "source 0 is kept, orphan or not");
    assert_eq!(loaded.meta().frame_rate, session.meta().frame_rate);
    drop(loaded);
    let _ = std::fs::remove_dir_all(&dir);

    // ...and one gesture is one undo: the take comes back whole.
    assert!(session.undo(), "the delete undoes");
    assert!(!session.is_empty());
    assert!((session.timeline_duration() - whole).abs() < 1e-9);
    assert_eq!(session.clip_spans().len(), 1);
}
