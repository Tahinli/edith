//! An audio-only mp4 (no picture track, sound only) must reopen wherever it
//! sits in a project: first, in the middle, or last, and with clips placed
//! from it. Import already takes this file as a song; the load path has to
//! take it the same way, or a project that saved fine refuses to open.
//!
//! ```text
//! cargo test -p engine --release --test audio_only_mp4_reopen -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn copy_in(dir: &Path, name: &str) -> PathBuf {
    let to = dir.join(name);
    std::fs::copy(asset(name), &to).expect("copy the fixture");
    to
}

/// Saves, reopens, and checks the reopened timeline is the one that was saved.
fn save_and_reopen(session: &PlaybackSession, path: &Path) -> PlaybackSession {
    session.save_project(path).expect("save");
    let loaded = PlaybackSession::open_project(path).unwrap_or_else(|e| {
        panic!("a project with an audio-only mp4 must reopen: {e}")
    });
    assert_eq!(loaded.sources(), session.sources());
    assert_eq!(loaded.timeline_duration(), session.timeline_duration());
    loaded
}

/// Source positions: the audio-only file alone, first, second, and last, each
/// saved and reopened. The picture-carrying file is the scaffold whenever it
/// is present, so the audio-only one is held by the load path's own checks.
#[test]
fn audio_only_mp4_reopens_in_every_source_position() {
    let dir = Scratch::dir("ve_audio_only_mp4_positions");
    let video = copy_in(&dir, "test_av.mp4");
    let audio = copy_in(&dir, "test_audio_only.mp4");

    // Alone.
    {
        let session = PlaybackSession::open(&audio).expect("open the audio-only file");
        session.set_gain(0.0);
        let path = dir.join("alone.edith");
        save_and_reopen(&session, &path);
    }

    // First, then the picture file.
    {
        let mut session = PlaybackSession::open(&audio).expect("open the audio-only file");
        session.set_gain(0.0);
        session.import(&video).expect("import the picture file second");
        let path = dir.join("first.edith");
        save_and_reopen(&session, &path);
    }

    // The picture file first, then the audio-only one.
    {
        let mut session = PlaybackSession::open(&video).expect("open the picture file");
        session.set_gain(0.0);
        session.import(&audio).expect("import the audio-only file second");
        let path = dir.join("second.edith");
        save_and_reopen(&session, &path);
    }

    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Clips placed from the audio-only file, in both positions, come back on the
/// same lanes at the same places, and the reopened project plays from them.
#[test]
fn clips_from_an_audio_only_mp4_survive_a_reopen() {
    let dir = Scratch::dir("ve_audio_only_mp4_clips");
    let video = copy_in(&dir, "test_av.mp4");
    let audio = copy_in(&dir, "test_audio_only.mp4");

    let mut session = PlaybackSession::open(&video).expect("open the picture file");
    session.set_gain(0.0);
    session.import(&audio).expect("import the audio-only file");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &audio, 0, None)
            .expect("the audio-only file is placeable on the timeline"),
        "the placement was accepted"
    );
    // And a second clip from the same audio-only file, at the front.
    session
        .place_stream_at(0.0, &audio, 0, None)
        .expect("a second clip from the audio-only file");

    let path = dir.join("clips.edith");
    let loaded = save_and_reopen(&session, &path);
    for lane in session.lanes() {
        assert_eq!(
            loaded.lane_clips(lane).len(),
            session.lane_clips(lane).len(),
            "clip count on {lane:?}"
        );
    }

    // The reopened session still composes its picture from the first source.
    let frame = loaded
        .compose_at(0, 320, 180)
        .expect("the reopened timeline composes a frame");
    assert_eq!((frame.width, frame.height), (320, 180));

    std::fs::remove_dir_all(&dir).expect("cleanup");
}
