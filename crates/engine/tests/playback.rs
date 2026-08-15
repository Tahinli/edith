//! End-to-end playback timing, without a UI. Both files run through the same
//! checks: `test_baseline.mp4` has no audio and is therefore always wall-paced,
//! `test_av.mp4` uses the audio clock when a PipeWire daemon is around and falls
//! back to the same wall path when it is not -- so neither test is `#[ignore]`d.
//!
//! ```text
//! cargo test -p engine --release --test playback -- --nocapture
//! ```

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::export::{ExportSettings, Format};
use engine::project::Lane;
use engine::scratch::Scratch;
use engine::{Codec, DecodeSession, Frame, PlaybackSession};

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
            // One at a time, in order. Every session here is a *pull* consumer
            // -- `open` leaves `drop_late_pictures` off -- and such a caller is
            // owed every frame of the range it asked for, which is the contract
            // an export and the file-level API rest on. Only a session told it
            // is being watched in real time may skip, and the two tests that
            // turn that on take their frames through `next_index` instead.
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
    let before = session.live_workers();

    for t in [4.0, 1.0, 3.5, 0.5, 2.0] {
        session.seek(t);
    }
    assert_eq!(
        next_index(&mut session, "5 rapid seeks"),
        (2.0 * fps) as u32
    );

    sleep(Duration::from_millis(500)); // abandoned workers exit within an AU
    let after = session.live_workers();
    eprintln!("rapid seeks: {before} workers -> {after}");
    assert!(
        after <= before + 3,
        "workers piled up: {before} workers -> {after}"
    );
}

/// The storm the deferred open exists for: 40 seeks with nothing in between.
/// Each one abandons a worker and starts another, and since the open moved onto
/// the worker (`DecodeSession::open_worker_deferred`) not one of them reads the
/// file on this thread -- so the whole storm is 40 thread spawns, the parked
/// workers are still reaped as they go (the session's own count says so), and
/// the last seek still decides what is on screen.
#[test]
fn a_storm_of_seeks_stays_bounded_and_the_last_one_wins() {
    let mut session = open(asset("test_av.mp4"));
    let fps = session.meta().frame_rate;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    let before = session.live_workers();

    let storm = Instant::now();
    for i in 0..39 {
        session.seek(f64::from(i % 5) * 0.8);
    }
    session.seek(2.0);
    let issued = storm.elapsed();
    eprintln!("40 seeks issued in {issued:?} ({before} workers before)");
    assert!(
        issued < Duration::from_secs(1),
        "40 seeks cost the caller {issued:?} -- a seek is opening a file again"
    );
    assert_eq!(
        next_index(&mut session, "40 rapid seeks"),
        (2.0 * fps) as u32,
        "the last seek is the one that decides"
    );

    sleep(Duration::from_millis(500)); // abandoned workers exit within an AU
    let after = session.live_workers();
    eprintln!("seek storm: {before} workers -> {after}");
    assert!(
        after <= before + 3,
        "the retired list grew with the storm: {before} workers -> {after}"
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
    // From here on the *sound* is what the clock counts, and the sound restarts
    // at once -- the video decoder's reopen (up to a VA-API init) happens
    // underneath a stream that is already playing, so the window `next_index`
    // spends waiting for the first picture is time the timeline really moved
    // through. Measured against the wall rather than assumed free: counting it
    // is the fix for the offset an edit-while-playing used to leave behind
    // (`invalidation.rs`), where the clock re-anchored onto an empty ring and
    // the picture stayed that far behind the sound for good.
    let from = Instant::now();
    let first = next_index(&mut session, "seek(3.0)");
    assert_eq!(first, (3.0 * meta.frame_rate) as u32);
    let mut after_seek = None;
    run_for(&mut session, &mut after_seek, Duration::from_millis(600));
    let advanced = session.now();
    let real = from.elapsed().as_secs_f64();
    eprintln!(
        "seek(3.0) -> {advanced:.3}s after {real:.3}s of real time, frames {first}..={after_seek:?}"
    );
    assert!(
        (3.0 + real - 0.35..3.0 + real + 0.35).contains(&advanced),
        "clock did not run at real speed after the seek: {advanced:.3}s after {real:.3}s"
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

/// Drops `path` out of the page cache, so the next read of it is a real disk
/// read. `posix_fadvise(POSIX_FADV_DONTNEED)`, glibc's own and declared here
/// rather than pulled in as a dependency for one call.
fn evict(path: &Path) {
    unsafe extern "C" {
        fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
    }
    let file = File::open(path).expect("open to evict");
    // 0 length is "to the end of the file"; 4 is POSIX_FADV_DONTNEED on Linux.
    let rc = unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, 4) };
    assert_eq!(rc, 0, "could not evict {}", path.display());
}

/// What a seek costs the *caller* when the sound has to be opened again off a
/// cold page cache -- the audio half of the move
/// [`a_storm_of_seeks_stays_bounded_and_the_last_one_wins`] measures for the
/// picture. That open is a `pread` per track (21 s on a cold 25 GB film,
/// measured at the seat) and it used to run on whoever called `seek`, which in
/// the editor is the thread that paints: one ruler click froze the window for
/// the whole of it. On the feeder it is a thread spawn, whatever the file.
///
/// And the clock still anchors on the first real sample, which is the invariant
/// the wait threatens: the timeline holds where the seek put it for however
/// long the open takes, and starts counting from *there* rather than from the
/// silence the device played meanwhile.
///
/// `assets/test_av.mp4` by default, which makes this a regression guard;
/// `EDITH_COLD_FILM=<a big film>` makes it the measurement.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn a_cold_audio_open_does_not_block_the_seek() {
    let path = std::env::var_os("EDITH_COLD_FILM")
        .map(PathBuf::from)
        .unwrap_or_else(|| asset("test_av.mp4"));
    let mut session = open(&path);
    let target = session.timeline_duration() * 0.5;
    let mut last_index = None;
    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));

    // The seek's own reads and nothing else: the open above warmed every byte
    // of this file that anything here touches.
    evict(&path);
    let issued = Instant::now();
    session.seek(target);
    let blocked = issued.elapsed();
    eprintln!(
        "cold seek on {}: caller blocked {blocked:?}",
        path.display()
    );
    assert!(
        blocked < Duration::from_millis(10),
        "the caller opened the file itself: seek blocked {blocked:?}"
    );
    assert!(
        (session.now() - target).abs() < 0.05,
        "the seek did not land: clock at {}",
        session.now()
    );

    // The sound arrives when it arrives, and the clock does not move until it
    // does. Long deadline: that is the point of the test.
    let waited = Instant::now();
    let deadline = waited + Duration::from_secs(120);
    while session.now() <= target + 0.001 {
        session.tick();
        while session.try_frame().is_some() {}
        assert!(
            Instant::now() < deadline,
            "the clock never moved after a cold seek"
        );
        sleep(Duration::from_millis(8));
    }
    let (moved, waited) = (session.now(), waited.elapsed().as_secs_f64());
    eprintln!("clock moved to {moved:.3}s (target {target:.3}s) after {waited:.3}s of open");
    assert!(
        moved - target < 0.5,
        "the open leaked into the timeline: {waited:.3}s of it put the clock at \
         {moved:.3}s instead of {target:.3}s"
    );
}

/// The seek storm the *ear* is in: 20 random seeks into a cold file, each
/// resuming playback, and not one starved quantum out of the device.
///
/// The device is started by the feeder's first real sample, never by the intent
/// to play ([`PlaybackSession::play`] and the seek's own resume), because an
/// audio open is not free -- seconds, on a cold film -- and a stream made active
/// over an empty ring plays its own silence and counts every quantum of it as
/// an underrun. It sounds identical either way; what it destroys is the meaning
/// of the count, which is the only thing that says the decoder is keeping up.
/// This is the regression guard for that: cold storm, count must be nil.
///
/// `assets/test_av.mp4` by default; `EDITH_COLD_FILM=<a big film>` and
/// `EDITH_SEEKS=<n>` make it the measurement (the rubric's floor is 50 seeks
/// into a two-hour film).
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn a_cold_seek_storm_starves_no_quantum() {
    let path = std::env::var_os("EDITH_COLD_FILM")
        .map(PathBuf::from)
        .unwrap_or_else(|| asset("test_av.mp4"));
    let seeks: u32 = std::env::var("EDITH_SEEKS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(20);
    let mut session = open(&path);
    let duration = session.timeline_duration();
    let mut last_index = None;
    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    // Only a machine with the plugin *and* a daemon can starve at all; without
    // one the session plays wall-paced and there is nothing to measure.
    let Some((before, _)) = session.audio_underruns() else {
        eprintln!("no audio device: seek storm not measured");
        return;
    };
    evict(&path);

    // xorshift64 off a fixed seed: the same storm every run, so two runs of
    // this are comparable without a dependency to draw the numbers from.
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let storm = Instant::now();
    for i in 0..seeks {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        // Off the end of the timeline is a seek with nothing to play; keep the
        // storm inside the film.
        let target = duration * 0.98 * (rng >> 11) as f64 / (1u64 << 53) as f64;
        session.seek(target);
        // The sound has to *come back*, which is the other half of this: a
        // device that never plays starves no quantum either, so the count below
        // means nothing unless every seek resumed. The clock is held at the
        // target until the first real sample is queued, so it moving is the
        // sound arriving ([`PlaybackSession::tick`]).
        let mut landed = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        while session.now() <= target + 0.05 {
            pump(&mut session, &mut landed);
            assert!(
                Instant::now() < deadline,
                "seek {i} to {target:.1}s never resumed: the clock stood still"
            );
            sleep(Duration::from_millis(8));
        }
        // ...and then plays for a moment, which is where a late decoder shows.
        run_for(&mut session, &mut landed, Duration::from_millis(300));
    }
    let (now, last) = session.audio_underruns().expect("device still open");
    let underruns = now - before;
    eprintln!(
        "{seeks} cold seeks into {} in {:?}: {underruns} underruns ({before} before the storm, \
         last at {last:?})",
        path.display(),
        storm.elapsed()
    );
    assert_eq!(
        underruns, 0,
        "the device starved during the storm: {underruns} quanta of silence it \
         counted as a late decoder"
    );
}

/// What a *drag* costs in threads: 200 scrub steps across a cold film with no
/// wait between them, which is a ruler dragged from end to end. Every step
/// abandons an audio open and starts another, and an abandoned one used to run
/// its read to the end anyway -- on a cold film that is seconds each, so the
/// threads pile up behind the drag rather than following it.
///
/// The session's own count is the oracle (`live_workers`), and the process
/// thread count beside it says what the OS saw.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn a_cold_scrub_does_not_pile_up_threads() {
    let path = std::env::var_os("EDITH_COLD_FILM")
        .map(PathBuf::from)
        .unwrap_or_else(|| asset("test_av.mp4"));
    let steps: u32 = std::env::var("EDITH_STEPS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(200);
    let mut session = open(&path);
    let duration = session.timeline_duration();
    let mut last_index = None;
    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    evict(&path);

    let (mut peak, mut peak_os) = (0, 0);
    let scrub = Instant::now();
    for step in 0..steps {
        session.seek(duration * f64::from(step) / f64::from(steps));
        session.tick();
        while session.try_frame().is_some() {}
        peak = peak.max(session.live_workers());
        peak_os = peak_os.max(os_threads());
        sleep(Duration::from_millis(16)); // a drag reports at ~60 Hz
    }
    eprintln!(
        "{steps} cold scrub steps in {:?}: peak {peak} session workers, {peak_os} process threads",
        scrub.elapsed()
    );
    // The peak is *reported*, not asserted: three runs of one build over the
    // same film measured 14, 101 and 137, because what decides it is how much
    // disk the rest of the machine is using while the opens run. What the
    // session owes is a different question and a real one -- every worker the
    // drag started has to end, or a drag is a leak.
    let deadline = Instant::now() + Duration::from_secs(120);
    while session.live_workers() > 2 {
        assert!(
            Instant::now() < deadline,
            "the scrub leaked workers: {} still running two minutes on",
            session.live_workers()
        );
        sleep(Duration::from_millis(50));
    }
    eprintln!("all but {} of them returned", session.live_workers());
}

/// Threads in this process, out of `/proc/self/status`. Only ever a report
/// beside the session's own count -- a suite decoding other files at the same
/// time moves this number without touching the session under test.
fn os_threads() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Threads:")?.trim().parse().ok())
        })
        .unwrap_or(0)
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

/// A cut small enough to land *ahead* of the decoder rather than behind it:
/// less than half a second of file removed, which is the shape a silence cut
/// leaves at every boundary it makes. The picture worker reaches the next
/// span by decoding forward and dropping (the plugin's forward skip, no
/// flush) -- and what must survive that is the exactness of the landing: the
/// first frame past the boundary is the source frame the cut ended on, pixel
/// for pixel, exactly as the long cut above is asserted.
#[test]
fn a_small_cut_lands_frame_exact_across_the_boundary() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let (fps, total) = (session.meta().frame_rate, session.meta().frame_count);
    // 0.2 s out of the middle: 6 frames at 30 fps, well inside the half a
    // second a reposition may walk forward.
    let (cut, hole_end) = ((1.0 * fps) as u32, (1.2 * fps) as u32);
    assert!(session.cut_at(1.0), "cut at 1 s");
    assert!(session.cut_at(1.2), "cut at 1.2 s");
    assert!(session.delete_clip(Lane::V1, 1), "drop the hole");
    let kept = total - (hole_end - cut);

    // Play the whole edited timeline from the top, one frame at a time.
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
    session.pause();

    let boundary = boundary.expect("no frame at the cut");
    assert!(
        boundary == source_frame(&path, hole_end),
        "the boundary frame is not source {hole_end}"
    );
    assert!(
        boundary != source_frame(&path, cut),
        "the deleted range is still being decoded"
    );
}

/// The prime a picture restart spends, as the session reports it: owed from
/// the moment a span is started until that span's first frame arrives. A
/// front-end gates its late-picture restart on this, so what it reads has to
/// be exactly this and nothing looser.
#[test]
fn picture_priming_spans_a_restart_and_ends_at_its_first_frame() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    // The open itself is a span that has not delivered yet.
    assert!(session.picture_priming(), "a fresh session is priming");
    let first = next_frame(&mut session, "the open");
    assert!(!session.picture_priming(), "the first frame ends the prime");
    assert_eq!(first.index, 0);

    // A seek restarts the picture at its target: a new span, a new prime --
    // the same start every clip boundary and edit takes.
    session.seek(2.0);
    assert!(session.picture_priming(), "the reseek is priming again");
    let landed = next_frame(&mut session, "the reseek");
    assert!(!session.picture_priming());
    assert_eq!(landed.index, (2.0 * session.meta().frame_rate) as u32);
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

/// The two acts a second file on the timeline now takes, in the order a user
/// does them: taken into the library, then dragged onto the end of the
/// timeline. An import registers a source and places nothing
/// (`PlaybackSession::import`), so every test that wants a second source *on*
/// the lanes does both.
fn import_and_place(session: &mut PlaybackSession, path: &Path) {
    session.import(path).expect("the file joins this timeline");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, path, 0, None)
            .expect("a file just imported is on this timeline"),
        "the drag onto the end of the timeline"
    );
}

/// Frame count straight from the container, without decoding anything.
fn frame_count(path: &Path) -> u32 {
    engine::demux::Demuxer::open(path)
        .expect("demux")
        .0
        .frame_count
}

/// An imported file dragged onto the end of the timeline, played through as
/// one stream. No audio needed for the pictures, so this runs anywhere.
#[test]
fn a_placed_import_plays_across_the_join() {
    let (av, av2) = (asset("test_av.mp4"), asset("test_av2.mp4"));
    let mut session = open(&av);
    let (fps, first) = (session.meta().frame_rate, session.meta().frame_count);
    let second = frame_count(&av2);
    assert_eq!((first, second), (150, 120), "fixtures changed");

    // The import alone fills the library: nothing on the lanes moved.
    session.import(&av2).expect("test_av2 matches test_av");
    assert_eq!(session.clip_spans().len(), 1, "an import placed a clip");
    assert!((session.timeline_duration() - 5.0).abs() < 1e-9);
    assert_eq!(session.file_frames(&av2), second, "its own length is noted");
    // ...and the drag puts it at the end.
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &av2, 0, None)
            .expect("a file just imported is on this timeline")
    );
    assert_eq!(session.clip_spans().len(), 2);
    assert_eq!(
        session
            .clip_spans_by_source()
            .iter()
            .map(|&(.., s)| s)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "the placed clip plays the imported file"
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

/// The first import into a window with nothing open: the file fills the library
/// and the timeline stays *empty*, and the drag that follows is what puts it on
/// a lane -- at the file's own length, which nothing on the timeline could have
/// told the session. The whole point of an import that places nothing.
#[test]
fn the_first_import_opens_a_library_over_an_empty_timeline() {
    let av = asset("test_av.mp4");
    let mut session = PlaybackSession::open_library(&av).expect("open into the library");
    session.set_gain(0.0);
    assert!(session.is_empty(), "the timeline must start empty");
    assert_eq!(session.timeline_duration(), 0.0);
    assert!(session.clip_spans().is_empty());
    // But it is a session: the file's own picture, rate and clock, and its row.
    assert_eq!(session.sources().len(), 1, "the library row");
    assert_eq!(session.resolution(), (1280, 720));
    assert_eq!(session.meta().frame_rate, 30.0);
    assert_eq!(session.file_frames(&av), 150, "5 s at 30 fps, never placed");
    assert!(!session.undo(), "opening a library is not an undo step");

    // The drag: the row goes onto the timeline whole, and it plays from there.
    assert!(
        session
            .place_stream_at(0.0, &av, 0, None)
            .expect("its own file is on this timeline")
    );
    assert!(!session.is_empty(), "the drag left the timeline empty");
    assert!((session.timeline_duration() - 5.0).abs() < 1e-9);
    session.seek(0.0);
    let landed = next_frame(&mut session, "the dragged clip");
    assert_eq!(landed.index, 0);
    assert!(
        landed.bgra == source_frame(&av, 0),
        "the placed clip is not playing its file"
    );
    // ...and it is an edit like any other, so one `z` takes it back to empty.
    assert!(session.undo(), "undo the drag");
    assert!(session.is_empty());
    assert_eq!(session.sources().len(), 1, "the row survives the undo");
}

/// One timeline, one frame rate and one set of audio parameters: what is left
/// is refused by name and changes nothing.
#[test]
fn import_refuses_what_does_not_match() {
    let mut session = open(asset("test_av.mp4"));

    // A different *codec* is not among them any more -- every clip opens its
    // own decoder. The only refusal left on that axis is a decoder this machine
    // cannot open, and it is the plugin's own words, not the timeline's.
    match session.import(&asset("test_vp9.mp4")) {
        Ok(_) => assert_eq!(session.sources().len(), 2, "VP9 joined the H.264 timeline"),
        Err(e) => assert_eq!(
            e.to_string(),
            Codec::Vp9.needs_plugin(),
            "the only codec refusal left is the missing decoder"
        ),
    }
    let rows = session.sources().len();

    // A different resolution is no longer among the refusals -- it is placed on
    // the project canvas instead -- and neither is a file with no sound: it
    // contributes silence over its span.
    session
        .import(&asset("test_mismatch.mp4"))
        .expect("a silent 640x360 file joins a timeline with sound");

    // A frame rate of its own is no longer a refusal either: 25 fps joins a
    // 30 fps timeline and is read at `Rate` against it -- see
    // `tests/mixed_fps.rs` for what it then plays like.
    session
        .import(&asset("test_25fps.mp4"))
        .expect("25 fps may join a 30 fps timeline");
    session
        .remove_source(&asset("test_25fps.mp4"), 0)
        .expect("...and the row it made comes back off");

    // A sample *rate* of its own is no longer a refusal either: one output
    // device means one rate, and the segment's own resampler is what makes two
    // into one now ([`engine::audio::Resample`]).
    session
        .import(&asset("test_tone_48k.wav"))
        .expect("48 kHz may join a 44.1 kHz timeline");
    session
        .remove_source(&asset("test_tone_48k.wav"), 0)
        .expect("...and the row it made comes back off");

    // What is still refused, and all that is: one output device carries one
    // layout, and a mono track is not the stereo pair this timeline plays.
    let err = session
        .import(&asset("test_ac3.mp4"))
        .expect_err("a mono track cannot join a stereo timeline")
        .to_string();
    assert_eq!(err, "audio 1 ch does not match the timeline's 2 ch");

    assert!(
        session.import(&asset("no_such_file.mp4")).is_err(),
        "a missing file is an error, not a panic"
    );
    assert_eq!(session.clip_spans().len(), 1, "an import places nothing");
    assert_eq!(
        session.sources().len(),
        rows + 1,
        "only the silent file was taken in"
    );
    assert!((session.timeline_duration() - 5.0).abs() < 1e-9);

    // The mirror is still refused: the device was opened on source 0's track
    // and a silent timeline has none to open, so a file with sound cannot be
    // heard on one.
    let mut silent = open(asset("test_baseline.mp4"));
    let err = silent
        .import(&asset("test_av.mp4"))
        .expect_err("audio into a silent timeline")
        .to_string();
    assert!(err.contains("audio"), "refusal must name the audio: {err}");
}

/// The user path a silent take goes down: a file with no audio track lands on a
/// timeline that has one, is placed as the grouped take every video file is,
/// and plays -- pictures over its span, silence under them. The sound itself is
/// asserted on the samples in `tests/audio_export.rs`; what is asserted here is
/// that nothing about the *timeline* is refused or lost.
#[test]
fn a_silent_file_joins_a_timeline_with_sound_and_plays() {
    let mut session = open(asset("test_av.mp4"));
    import_and_place(&mut session, &asset("test_mismatch.mp4"));
    // 150 frames of test_av, then 60 of the silent file, at 30 fps.
    assert_eq!(session.clip_spans().len(), 2, "the silent take landed");
    assert!((session.timeline_duration() - 7.0).abs() < 1e-9);
    // Both lanes carry it: a silent file is a normal grouped take, and its
    // audio clip is the span the worker fills with silence.
    assert_eq!(session.lane_clips(Lane::V1).len(), 2);
    assert_eq!(session.lane_clips(Lane::A1).len(), 2);

    // ...and it decodes: a picture from inside the silent clip's span, at the
    // project's resolution like every other clip.
    session.seek(6.0);
    let frame = next_frame(&mut session, "the silent clip");
    assert_eq!((frame.width, frame.height), session.resolution());
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
    import_and_place(&mut session, &asset("test_mismatch.mp4"));
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
    import_and_place(&mut session, &asset("test_mismatch.mp4"));
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

/// The same file twice is one library row -- a source index is handed out once
/// and reused, which is what keeps clipboard clips valid forever -- and two
/// drags of that one row are two clips of it.
#[test]
fn importing_one_file_twice_reuses_its_source() {
    let mut session = open(asset("test_av.mp4"));
    let av2 = asset("test_av2.mp4");
    assert_eq!(session.import(&av2).expect("first import"), 1);
    assert_eq!(
        session.import(&av2).expect("second import"),
        1,
        "the second import is the same row"
    );
    assert_eq!(session.sources().len(), 2, "one row for one file");
    import_and_place(&mut session, &av2);
    import_and_place(&mut session, &av2);
    assert_eq!(session.sources().len(), 2, "and still one row");
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

/// A library row placed after the timeline has been played out revives the
/// session: the placement reseeks like every other edit, and that is what
/// clears `eos`. The import itself changes nothing, played out or not.
///
/// Where it resumes is not asserted, for the reason `paste_at_eos_revives_the_
/// session` does not assert it either: an edit reseeks to `now()`, and at EOS
/// the free-running clock has gone past the end.
#[test]
fn a_placement_at_eos_revives_the_session() {
    let av2 = asset("test_av2.mp4");
    let mut session = open(asset("test_av.mp4"));
    let first = session.meta().frame_count;

    session.play();
    assert_eq!(drain_to_eof(&mut session).1, Some(first - 1));
    assert!(session.is_eos());

    session.import(&av2).expect("import at EOS");
    assert!(
        session.is_eos(),
        "an import is not an edit: it cannot revive"
    );
    assert!(
        session
            .place_stream_at(session.timeline_duration(), &av2, 0, None)
            .expect("a file just imported is on this timeline")
    );
    assert!(
        !session.is_eos(),
        "the placement did not revive the session"
    );
    assert_eq!(
        session.timeline_duration(),
        f64::from(first + frame_count(&av2)) / session.meta().frame_rate
    );
    assert_eq!(session.clip_spans().len(), 2);
}

/// A source deleted mid-session: the clip that names it contributes no pictures
/// and the timeline still ends, instead of stalling on the missing file.
#[test]
fn a_vanished_source_is_skipped() {
    let scratch = Scratch::file("video_editor_vanish", "mp4");
    std::fs::copy(asset("test_av2.mp4"), &scratch).expect("copy the fixture");
    let mut session = open(asset("test_av.mp4"));
    let first = session.meta().frame_count;
    import_and_place(&mut session, &scratch);
    std::fs::remove_file(&scratch).expect("unlink");

    session.seek(0.0);
    session.play();
    let (count, last) = drain_to_eof(&mut session);
    assert_eq!(last, Some(first - 1), "the vanished clip made pictures");
    assert_eq!(count, first, "the first source still played whole");
    assert!(session.is_eos(), "the timeline still ends");
}

/// Importing and placing while the device is running: the placement reseeks
/// under the playhead, so what must not happen is a stall -- the joined
/// timeline still plays out whole. No audio needed, so this runs anywhere.
#[test]
fn import_during_play_does_not_stall() {
    let (av, av2) = (asset("test_av.mp4"), asset("test_av2.mp4"));
    let mut session = open(&av);
    let first = session.meta().frame_count;
    let mut last_index = None;

    session.play();
    run_for(&mut session, &mut last_index, Duration::from_millis(300));
    assert!(last_index.is_some(), "no frames before the import");

    import_and_place(&mut session, &av2);
    assert_eq!(session.clip_spans().len(), 2);
    assert!(session.is_playing(), "the placement stopped the clock");

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

/// An import is *not* an undo step -- a library row changes nothing playable,
/// so there is nothing for `z` to take back -- and the placement that follows
/// is one. Undoing it leaves the row in the library, where the user put it.
#[test]
fn an_import_is_not_an_undo_step_but_its_placement_is() {
    let av2 = asset("test_av2.mp4");
    let mut session = open(asset("test_av.mp4"));
    session.import(&av2).expect("import");
    assert!(!session.undo(), "an import must not be undoable");
    assert_eq!(session.sources().len(), 2, "and it is still in the library");

    import_and_place(&mut session, &av2);
    assert_eq!(session.clip_spans().len(), 2);
    assert!((session.timeline_duration() - 9.0).abs() < 1e-9);

    assert!(session.undo(), "undo the placement");
    assert_eq!(session.clip_spans().len(), 1, "one step, back to one clip");
    assert!(
        (session.timeline_duration() - 5.0).abs() < 1e-9,
        "undo did not restore the duration: {}",
        session.timeline_duration()
    );
    assert_eq!(
        session.sources().len(),
        2,
        "the library row survives an undo of the clip that used it"
    );

    // Still playable: the undo reseeked to the (paused) playhead at zero.
    session.play();
    assert_eq!(next_index(&mut session, "undo of a placement"), 0);
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
    import_and_place(&mut session, &asset("test_av2.mp4"));
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

    // Exporting nothing is a refusal in words, not a file of no frames -- and
    // in the *same* words whichever format asked, because the fence sits in
    // `export::start` ahead of the format, not in the mp4 path alone.
    for format in [Format::Mp4, Format::Wav, Format::Flac] {
        let dir = Scratch::dir("ve_nothing");
        let out = dir.join("ve_nothing");
        let handle = session.export_to_with(
            &out,
            &ExportSettings {
                format,
                ..ExportSettings::default()
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let err = loop {
            if let Some(result) = handle.result() {
                break result.expect_err("an empty timeline cannot be exported");
            }
            assert!(Instant::now() < deadline, "{format:?} export never settled");
            sleep(Duration::from_millis(10));
        };
        assert_eq!(
            err.to_string(),
            "the timeline is empty: there is nothing to export",
            "{format:?}"
        );
        assert!(!out.exists(), "{format:?} wrote a file anyway");
    }

    // It saves and loads back -- still a project, still two lanes, and still
    // naming the file whose frame rate the timeline counts in.
    let dir = Scratch::dir("ve_empty");
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

/// The session half of the late-frame policy: a *watched* session still lands
/// exactly on the frame a seek asked for. The floor the worker drops against is
/// the playhead mapped back into the file own frames, and a floor even one
/// frame ahead of the landing would eat the very picture the seek exists to
/// show ([`PlaybackSession::drop_late_pictures`]).
#[test]
fn a_watched_session_still_lands_on_the_frame_a_seek_asked_for() {
    let mut session = open(asset("test_baseline.mp4"));
    session.drop_late_pictures(true);
    let fps = session.meta().frame_rate;
    session.seek(3.0);
    assert_eq!(
        next_index(&mut session, "seek(3.0) on a watched session"),
        (3.0 * fps) as u32,
        "the landing frame was dropped as late"
    );
    // ...and again backwards: a floor left where the last span ended would drop
    // every picture of a seek that went back.
    session.seek(1.0);
    assert_eq!(
        next_index(&mut session, "seek(1.0) on a watched session"),
        (1.0 * fps) as u32
    );
}

/// The pixels a *reused* decoder hands back are the pixels a fresh one does.
/// One session takes three seeks -- forwards, backwards, forwards again -- and
/// the worker survives all of them (`Worker::reseek`: the file stays open, the
/// hardware session is repositioned rather than reopened), so a decoder left
/// holding one span references while another span is asked for would show here
/// as a smear and nowhere else. The reference is the file-level API, which
/// opens a decoder of its own per frame.
#[test]
fn a_reseek_decodes_the_same_picture_a_fresh_decoder_does() {
    let path = asset("test_baseline.mp4");
    let mut session = open(&path);
    let fps = session.meta().frame_rate;
    // Small targets on purpose: the fixture's only sync sample is frame 0, so
    // every one of these decodes from the start of the file and a debug-build
    // software decoder is what runs the suite.
    for target in [20u32, 5, 35] {
        session.seek(f64::from(target) / fps);
        let frame = next_frame(&mut session, "a seek onto a worker already open");
        assert_eq!(frame.index, target, "the seek landed elsewhere");
        assert!(
            frame.bgra == source_frame(&path, target),
            "frame {target} off the reused decoder is not the picture a fresh one decodes"
        );
    }
}

/// What the drop policy must never do: thin a picture the decoder was keeping up
/// with. `pump`'s own contiguity assert covers every *pull* consumer above;
/// nothing there bounds what a **watched** session delivers, and a one-in-N
/// regression inside the worker would pass the whole suite otherwise.
///
/// The fixture is the smallest one here (320x180) so the question can be asked
/// on a debug build at the file's own rate -- no retime, because a retimed
/// timeline legitimately shows only every n-th source frame and the sharp half
/// of this test is that the indices arrive one at a time. A box too slow even
/// for this says so and stops: thinning the picture there is the policy working
/// (`LATE_RUN`), not a regression.
#[test]
fn a_watched_session_keeps_every_picture_its_decoder_keeps_up_with() {
    let path = asset("test_speed_sync.mp4");

    // What this box decodes, flat out, with nothing dropped: frames per second.
    let mut measure = open(&path);
    let fps = measure.meta().frame_rate;
    measure.play();
    let mut decoded = 0u32;
    let taken = Instant::now();
    while taken.elapsed() < Duration::from_millis(500) && !measure.is_eos() {
        measure.tick();
        while measure.try_frame().is_some() {
            decoded += 1;
        }
        sleep(Duration::from_millis(2));
    }
    let capable = f64::from(decoded) / taken.elapsed().as_secs_f64();
    drop(measure);
    eprintln!("this box decodes the fixture at {capable:.1} fps, which plays at {fps:.1}");
    if capable < fps * 1.2 {
        eprintln!("SKIP: too slow to ask the question at all");
        return;
    }

    // Watched, playing at its own rate, drained like a front-end that keeps up:
    // every picture is due when it arrives and none of them may be dropped.
    let mut session = open(&path);
    session.drop_late_pictures(true);
    session.play();
    let mut delivered = 0u32;
    let mut last: Option<u32> = None;
    let watched = Instant::now();
    while watched.elapsed() < Duration::from_millis(500) && !session.is_eos() {
        session.tick();
        while let Some(frame) = session.try_frame() {
            delivered += 1;
            if let Some(previous) = last {
                assert_eq!(
                    frame.index,
                    previous + 1,
                    "a picture was dropped while the decoder was keeping up"
                );
            }
            last = Some(frame.index);
        }
        sleep(Duration::from_millis(2));
    }
    // Frames the clock went over, which is what the timeline owed the screen.
    let passed = session.now() * fps;
    let density = f64::from(delivered) / passed.max(1.0);
    eprintln!("watched: {delivered} delivered over {passed:.1} frames of clock");
    assert!(
        density >= 0.9,
        "the worker thinned a picture it was keeping up with: {delivered} of \
         {passed:.1} frames ({density:.2}), decoder measured at {capable:.1} fps"
    );
}

/// The decode-ahead a **freshly opened** file gets. Every session opens its first
/// worker pass-through -- a file just opened *is* the project resolution -- and
/// asking a pass-through canvas how deep to queue answers 2, the bound this
/// engine had before there was any decode-ahead: the whole file would then play
/// at that depth until the first seek built a real canvas. The depth is taken
/// from the stream instead (`scale::queue_depth`), and this is where that is
/// visible from outside: a paused session left alone fills its queue, and the
/// burst that comes off it is the depth plus the picture in the worker's hand.
///
/// 720p fixture: ~96 MB of BGRA is 16 pictures at that size (the ceiling), so
/// the burst is 17 and the regression it guards against is a burst of 3.
#[test]
fn a_freshly_opened_file_queues_more_than_two_pictures() {
    let mut session = open(asset("test_baseline.mp4"));
    // Paused throughout: no seek, no play, nothing that would build a canvas.
    // The best of three attempts, because filling the queue is the decoder's own
    // work and this box shares itself with whatever else is running.
    let mut burst = 0;
    for _ in 0..3 {
        sleep(Duration::from_millis(1500));
        let mut n = 0;
        while session.try_frame().is_some() {
            n += 1;
        }
        burst = burst.max(n);
    }
    eprintln!("burst off a freshly opened file: {burst} pictures");
    assert!(
        burst > 8,
        "a freshly opened file queued {burst} pictures: the first worker is \
         still sizing its queue from a pass-through canvas"
    );

    // ...and the same worker still answers. Four and a half seconds parked is
    // well past the idle mark at which it closes its hardware session
    // (`decode`'s `IDLE`), so this is the seek that has to reopen one lazily --
    // the one path that would have gone quiet if closing it left the worker
    // holding a decoder it could no longer use.
    let fps = session.meta().frame_rate;
    session.seek(2.0);
    assert_eq!(
        next_index(&mut session, "a seek after the worker went idle"),
        (2.0 * fps) as u32,
        "the worker did not come back from its idle park"
    );
}
