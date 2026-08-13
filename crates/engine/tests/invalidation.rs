//! What an edit is allowed to tear down, and what it is not.
//!
//! Every setting a person can move while a timeline plays dirties *one* half of
//! the pipeline at most: a grade, a fit policy and the project resolution are
//! pictures, an equalizer is sound, and a fader is not even that -- it is a
//! number the running mixer picks up between two blocks. The tests here are the
//! measurement of that claim, and the thing they measure is the **audio tap**:
//! it is the sound the device was handed last, and a restart clears it (the
//! ring is flushed and nothing is fed again until a decoder has reopened, tens
//! of milliseconds later). An empty tap right after an edit therefore *is* the
//! hole in the sound the user reported hearing when they dragged a colour
//! slider.
//!
//! Every test runs anywhere: with no PipeWire daemon there is no device, no tap
//! and nothing to protect, so the audio assertions are made only when there is
//! sound to make them about -- the clock assertions run either way.
//!
//! ```text
//! cargo test -p engine --test invalidation -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use engine::PlaybackSession;
use engine::color::ColorParams;
use engine::limiter::Limiter;
use engine::project::Lane;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Silent, like every other suite that plays a real file: these tests are about
/// whether the sound is *interrupted*, not about what it sounds like, and the
/// gain is the last multiply before the device -- everything measured here is
/// decoded, fed and counted exactly as it is when it is audible.
fn open(path: &Path) -> PlaybackSession {
    let session = PlaybackSession::open(path).expect("open");
    session.set_gain(0.0);
    session
}

/// How many samples the device has been handed lately. `None` with no audio
/// device at all -- the machine's business, and nothing to assert about.
fn heard(session: &PlaybackSession) -> Option<usize> {
    session.audio_tap().map(|(tap, _)| tap.len())
}

/// Plays until the device has really been handed something, so a later empty
/// tap means "this edit emptied it" rather than "it was never filled".
fn play_until_heard(session: &mut PlaybackSession) -> Option<usize> {
    session.play();
    for _ in 0..200 {
        session.tick();
        while session.try_frame().is_some() {}
        if heard(session).is_none_or(|n| n > 0) {
            break;
        }
        sleep(Duration::from_millis(10));
    }
    let filled = heard(session);
    if filled == Some(0) {
        panic!("the device was never fed: nothing to measure");
    }
    filled
}

/// A grade dragged while the timeline plays. The slider commits per pointer
/// sample (`set_color_live`), which used to reseek the whole session per
/// sample: an audio flush, an audio re-open and a video decoder rebuild, ~40
/// times across one drag of the bar. The sound must not notice.
#[test]
fn a_grade_during_playback_never_touches_the_sound() {
    let mut session = open(&asset("test_av.mp4"));
    let filled = play_until_heard(&mut session);
    let before = session.now();

    for step in 0..20 {
        let grade = ColorParams {
            saturation: 1.0 - step as f32 / 40.0,
            ..Default::default()
        };
        assert!(
            session.set_color_live(Lane::V1, 0, Some(grade)),
            "the grade takes at step {step}"
        );
        // Right now, with no wait: a flush shows up here before anything can
        // refill it.
        if let Some(n) = heard(&session) {
            assert!(
                n > 0,
                "step {step} of the drag emptied the device queue -- the sound was torn down by a colour change"
            );
        }
        session.tick();
        while session.try_frame().is_some() {}
    }

    assert_eq!(
        heard(&session).is_some(),
        filled.is_some(),
        "the audio path disappeared across the drag"
    );
    let after = session.now();
    assert!(
        after >= before,
        "the clock went backwards across the drag: {before:.3}s -> {after:.3}s"
    );
    assert!(
        after - before < 0.5,
        "20 grade steps cost {:.3}s of timeline",
        after - before
    );
    eprintln!("20 live grades: clock {before:.3}s -> {after:.3}s, tap {filled:?} samples");
}

/// The project resolution, changed while the timeline plays -- the 4K film the
/// report came from. Every clip is recomposed onto the new canvas, which is a
/// video decoder rebuild and nothing else: no sample changes, so no sample may
/// be discarded and the clock may not be re-anchored (that re-anchor, onto an
/// empty ring, is what put the picture permanently ahead of the sound).
#[test]
fn a_resolution_change_during_playback_never_touches_the_sound() {
    let mut session = open(&asset("test_av.mp4"));
    let filled = play_until_heard(&mut session);
    let native = session.native_resolution();
    let before = session.now();

    for (w, h) in [(1920, 1080), (640, 360), (3840, 2160)] {
        assert!(session.set_resolution(w, h), "resize to {w}x{h}");
        if let Some(n) = heard(&session) {
            assert!(
                n > 0,
                "resizing to {w}x{h} emptied the device queue -- the sound was torn down by a picture setting"
            );
        }
        let at = session.now();
        assert!(
            at >= before && at - before < 0.5,
            "the clock jumped across the resize: {before:.3}s -> {at:.3}s"
        );
        assert_eq!(session.resolution(), (w, h));
        session.tick();
        while session.try_frame().is_some() {}
    }
    assert_eq!(
        session.native_resolution(),
        native,
        "the media's own size is not the project's"
    );
    assert_eq!(
        heard(&session).is_some(),
        filled.is_some(),
        "the audio path disappeared across the resizes"
    );

    // ...and it is still running afterwards, at real speed.
    let mark = session.now();
    for _ in 0..30 {
        session.tick();
        while session.try_frame().is_some() {}
        sleep(Duration::from_millis(10));
    }
    let ran = session.now() - mark;
    eprintln!("after three resizes the clock ran {ran:.3}s in 0.3s of real time");
    assert!(ran > 0.05, "the clock stalled after the resize: {ran:.3}s");
}

/// A fader and the master ceiling, moved while the timeline plays. Both live at
/// the mix and nowhere else, so a running mixer picks them up between blocks --
/// nothing is flushed, nothing is reopened, and the picture does not blink.
///
/// The *first* move is allowed one restart: a single audio lane at unity with
/// the limiter off is not mixed at all (the bit-exact path), so moving off it
/// is what opens a mixer. Every move after that is live, which is what an arrow
/// key held down on the mix card actually does.
#[test]
fn a_fader_during_playback_stops_restarting_the_sound() {
    let mut session = open(&asset("test_av.mp4"));
    let filled = play_until_heard(&mut session);
    if filled.is_none() {
        eprintln!("no audio device: nothing to mix");
        return;
    }

    // The move that opens the mixer, and the refill after it.
    assert!(session.set_lane_gain_db(Lane::A1, -3.0), "turn A1 down");
    play_until_heard(&mut session);

    for step in 1..=20 {
        let db = -3.0 - step as f32 * 0.5;
        assert!(session.set_lane_gain_db(Lane::A1, db), "fader to {db} dB");
        assert_eq!(
            heard(&session),
            filled,
            "nudge {step} emptied the device queue -- a fader restarted the stream"
        );
    }
    // ...and the ceiling beside it, which is retuned rather than rebuilt.
    for step in 0..8 {
        let limiter = Limiter {
            on: step % 2 == 0,
            ..Limiter::default()
        }
        .with_ceiling(-1.0 - step as f32);
        assert!(session.set_limiter(limiter), "ceiling {step}");
        assert_eq!(
            heard(&session),
            filled,
            "limiter step {step} emptied the device queue"
        );
    }
    assert_eq!(session.lane_gain_db(Lane::A1), -13.0);
    eprintln!("20 fader nudges and 8 limiter moves: not one restart");
}

/// The same rule on the timeline that has **no mixer at all**: one audio lane
/// at unity with the limiter off is the bit-exact single-stream path, and a
/// ceiling dragged while the limiter is *off* changes not one sample of it.
/// That drag used to reopen the sound at every nudge -- there was no mixer to
/// hand the number to, so the rebuild was the only way to apply a setting that
/// did not apply to anything.
///
/// The device queue is the oracle, as above; the worker count is the second
/// one, because a rebuild is a feeder thread per nudge whether the ear catches
/// it or not.
#[test]
fn a_ceiling_dragged_with_the_limiter_off_rebuilds_nothing() {
    let mut session = open(&asset("test_av.mp4"));
    let filled = play_until_heard(&mut session);
    let workers = session.live_workers();

    for step in 1..=20 {
        // Off throughout: this is the setting nobody can hear yet. Every step a
        // different ceiling, since setting the one already in force is refused
        // and would measure nothing.
        let limiter = Limiter {
            on: false,
            ..Limiter::default()
        }
        .with_ceiling(-1.0 - step as f32 * 0.25);
        assert!(session.set_limiter(limiter), "ceiling {step}");
        assert_eq!(
            heard(&session),
            filled,
            "nudge {step} emptied the device queue -- a silent setting restarted the stream"
        );
        assert!(
            session.live_workers() <= workers,
            "nudge {step} started a worker: {} against {workers} before the drag",
            session.live_workers()
        );
    }

    // ...and turning it *on* is the one move that has to open a mixer, which is
    // a rebuild and is allowed to be one.
    assert!(session.set_limiter(Limiter::default().with_ceiling(-1.0)), "limiter on");
    eprintln!("20 ceiling nudges with the limiter off: not one rebuild");
}

/// The other half of the rule, and the one that makes it a *classification*
/// rather than a blanket "never rebuild": an equalizer really is a change to
/// the samples, so it does restart the audio -- and must leave the picture
/// alone while it does.
#[test]
fn an_equalizer_rebuilds_the_sound_and_only_the_sound() {
    let mut session = open(&asset("test_av.mp4"));
    play_until_heard(&mut session);
    let before = session.now();

    let params = engine::eq::EqParams::default_layout();
    let mut curve = params.clone();
    curve.bands[0].gain_db = 6.0;
    assert!(session.set_eq(Lane::A1, 0, Some(curve)), "equalize A1's clip");

    let after = session.now();
    assert!(
        after >= before && after - before < 0.5,
        "the equalizer moved the playhead: {before:.3}s -> {after:.3}s"
    );
    // The picture keeps coming: the video worker was never cancelled.
    let mut frames = 0;
    for _ in 0..60 {
        session.tick();
        while session.try_frame().is_some() {
            frames += 1;
        }
        sleep(Duration::from_millis(10));
    }
    eprintln!("{frames} frames across an equalizer change, clock at {:.3}s", session.now());
    assert!(frames > 0, "the picture stopped when the sound was rebuilt");
}

/// The same classification asked at the one place it decides something a person
/// can see: **after the timeline has played out**. A sound edit rebuilds the
/// sound and starts no picture worker, so end of stream stands -- the last frame
/// is still the last frame, the transport is still `Ended`, and the next press
/// restarts from the top with the new curve on it. A revival here would spend
/// that state on an edit with nothing new to show, leaving a play press to run
/// the clock off the end instead of starting the film again.
#[test]
fn a_sound_edit_after_the_end_leaves_the_end_where_it_is() {
    let mut session = open(&asset("test_av.mp4"));
    // A breath short of the end: the tail is what this is about.
    session.seek(4.8);
    session.play();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !session.is_eos() {
        assert!(
            std::time::Instant::now() < deadline,
            "never reached the end of a 5 s file"
        );
        session.tick();
        while session.try_frame().is_some() {}
    }
    // What the front-end does on the crossing (`Player::pump`): stopped, on the
    // out point, and still ended.
    session.halt_at_end();
    let ended_at = session.now();
    assert_eq!(ended_at, session.timeline_duration());

    let mut curve = engine::eq::EqParams::default_layout();
    curve.bands[0].gain_db = 6.0;
    assert!(session.set_eq(Lane::A1, 0, Some(curve)), "equalize at the end");
    assert!(
        session.is_eos(),
        "an equalizer past the end revived the session: nothing new to show"
    );
    assert!(!session.is_playing(), "...and it started the transport again");
    assert!(
        (session.now() - ended_at).abs() < 1e-9,
        "the playhead left the out point: {:.6}s",
        session.now()
    );
    // The parameters landed all the same -- that is what "the edit took" means.
    assert!(session.eq_of(Lane::A1, 0).is_some(), "the curve did not apply");

    // The fader's first move at the end goes down the same road (no mix is
    // running to push it into, so it reseeks the audio too).
    assert!(session.set_lane_gain_db(Lane::A1, -6.0), "fader at the end");
    assert!(session.is_eos(), "a fader past the end revived the session");
    assert_eq!(session.lane_gain_db(Lane::A1), -6.0);

    // And the restart off that end is still there, which is what the state was
    // being kept for.
    session.seek(0.0);
    assert!(!session.is_eos(), "a seek revives a played-out session");
}
