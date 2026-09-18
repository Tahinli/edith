//! A silent leading source must not silence the timeline for good.
//!
//! The reported sequence: (1) a video with no audio track is added to the
//! timeline, (2) it is removed, (3) another file -- one that *does* have audio
//! -- is added. The engine used to pin the session to its first source: the
//! device was opened (or, here, not opened) from that one file and never
//! re-armed, and the import door refused the newcomer outright ("the file has
//! audio, the timeline is silent"). So the second file's picture showed but its
//! audio was silent -- or the file could not join at all.
//!
//! Three seams held that pin, and each is covered here:
//! - the import gate (`audio_matches_probed` / `first_audio_of`) refused a file
//!   with sound onto a timeline whose leading source has none;
//! - the session never opened a device when such a file did join
//!   (`PlaybackSession::arm_audio`);
//! - playback itself re-picked the first non-image source and returned
//!   `Ok(None)` -- silence -- however many later segments named a source with
//!   audio (`AudioSession::open_multi_streams_speed_at_fade`).
//!
//! ```text
//! cargo test -p engine --test silent_first_source -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use engine::{AudioSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The real phone clip the report named, when it is on this machine: mono-ish
/// ambient at about -36 dBFS, so the "non-silent" bar is its own peak, not a
/// loud fixture's. Skipped (and the run says so) when the file is absent.
fn real_clip() -> Option<PathBuf> {
    let path = PathBuf::from("/home/tahinli/Downloads/VID_20260918_065107.mp4");
    path.is_file().then_some(path)
}

fn peak(session: &PlaybackSession) -> f32 {
    session
        .audio_tap()
        .map(|(s, _)| s.iter().fold(0.0f32, |m, s| m.max(s.abs())))
        .unwrap_or(0.0)
}

fn pump(session: &mut PlaybackSession, secs: f64) -> f32 {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    let mut best = 0.0f32;
    while Instant::now() < deadline {
        session.tick();
        best = best.max(peak(session));
        sleep(Duration::from_millis(8));
    }
    best
}

fn remove_all_clips(session: &mut PlaybackSession) {
    for lane in session.lanes() {
        let mut i = session.lane_clips(lane).len();
        while i > 0 {
            i -= 1;
            session.delete_clip(lane, i);
        }
    }
}

/// The app's own import door: the gate taken off the live session, the probe
/// read off it (what the worker does), then the registration.
fn app_import(session: &mut PlaybackSession, b: &Path) -> engine::Result<usize> {
    let gate = session.import_gate();
    let probe = PlaybackSession::probe_import(gate, b)?;
    session.import_probed(b, probe)
}

/// The reported add/remove/add, end to end at the session level: a silent first
/// file, removed from the timeline, then a file with sound imported into the
/// *same* session and placed -- its audio must decode non-silent.
fn add_remove_add(a: &Path, b: &Path, loud: f32) {
    let mut session = PlaybackSession::open_library(a).expect("open_library A");
    session.set_gain(0.0);

    // (1) A joins the timeline. Silent by nature: no track to open a device on.
    let end = session.timeline_duration();
    assert!(
        session.place_stream_at(end, a, 0, None).expect("place A"),
        "A lands"
    );
    session.play();
    let a_peak = pump(&mut session, 1.5);
    assert!(a_peak < 0.01, "A must be silent, got {a_peak}");
    session.pause();
    session.seek(0.0);

    // (2) A comes off the timeline; the library keeps it.
    remove_all_clips(&mut session);

    // (3) B joins the library through the app's own door, then the timeline.
    app_import(&mut session, b).expect("B must be admitted onto a silent timeline");
    assert!(
        session.audio_disabled_reason().is_none(),
        "the device was not armed for B: {:?}",
        session.audio_disabled_reason()
    );
    let end = session.timeline_duration();
    assert!(
        session.place_stream_at(end, b, 0, None).expect("place B"),
        "B lands"
    );
    session.play();
    let b_peak = pump(&mut session, 2.5);
    assert!(
        b_peak > loud,
        "B's audio is silent after the sequence: peak {b_peak}"
    );
}

#[test]
fn a_second_file_with_sound_is_heard_after_a_silent_first_file() {
    add_remove_add(&asset("test_baseline.mp4"), &asset("test_av2.mp4"), 0.05);
}

/// The same sequence with the report's own file as the second import.
#[test]
fn the_reports_second_file_is_heard_too() {
    match real_clip() {
        Some(real) => add_remove_add(&asset("test_baseline.mp4"), &real, 0.02),
        None => eprintln!("SKIPPED: {} is not on this machine", "/home/tahinli/Downloads/VID_20260918_065107.mp4"),
    }
}

/// The playback seam on its own: a segment naming the second source must decode
/// that source's samples even though the first source has no track. Pre-fix
/// this returned `Ok(None)` -- the whole timeline silent.
#[test]
fn playback_walks_past_a_trackless_leading_source() {
    let sources = vec![(asset("test_baseline.mp4"), 0usize), (asset("test_av2.mp4"), 0usize)];
    let segs = vec![(Some(1usize), 0.0f64, f64::INFINITY)];
    let (_, rx) = AudioSession::open_multi_streams(&sources, &segs)
        .expect("open the second source's audio")
        .expect("the second source has a track");
    let (mut n, mut peak) = (0usize, 0.0f32);
    for chunk in rx {
        n += chunk.samples.len();
        peak = chunk.samples.iter().fold(peak, |m, s| m.max(s.abs()));
    }
    assert!(n > 0, "nothing decoded");
    assert!(peak > 0.05, "the second source's audio is silent: peak {peak}");
}
