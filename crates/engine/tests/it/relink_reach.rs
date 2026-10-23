//! A relink is refused by the frames the clips read from the source, not by the
//! source's whole length: a clip that reads the first 60 of 150 frames relinks to
//! a 60-frame file, and a clip that reads all 150 does not.
//!
//! ```text
//! cargo test -p engine --release --test relink_reach -- --nocapture
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::project::{Edge, Lane};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// A 150-frame, 30 fps film on the timeline, with its first clip trimmed to
/// `frames` timeline frames, saved, and the film moved away so the project opens
/// with that source offline.
fn offline_with_reach(name: &str, frames: Option<u32>) -> (Scratch, PathBuf) {
    let dir = Scratch::dir(&format!("ve_relink_reach_{name}"));
    let media = dir.join("film.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the fixture");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    if let Some(frames) = frames {
        let idx = session.lane_clips(Lane::V1).len() - 1;
        assert!(session.trim_clip(Lane::V1, idx, Edge::End, frames), "the trim");
    }
    let path = dir.join("proj.edith");
    session.save_project(&path).expect("save");
    std::fs::rename(&media, dir.join("moved.mp4")).expect("move the media");
    (dir, path)
}

#[test]
fn a_clip_reading_part_of_a_source_relinks_to_a_file_of_that_part() {
    let (_dir, path) = offline_with_reach("part", Some(60));
    let mut session = PlaybackSession::open_project(&path).expect("opens offline");
    assert_eq!(session.offline_sources().len(), 1);
    let part = asset("test_hevc.mp4");
    let relinked = session.relink_source(0, &part);
    assert!(relinked.is_ok(), "a 60-frame file serves a clip reading 60 frames: {:?}", relinked.err());
    assert!(session.offline_sources().is_empty());
}

#[test]
fn a_clip_reading_the_whole_source_refuses_a_shorter_file_by_its_reach() {
    let (_dir, path) = offline_with_reach("whole", None);
    let mut session = PlaybackSession::open_project(&path).expect("opens offline");
    let err = session
        .relink_source(0, &asset("test_hevc.mp4"))
        .expect_err("60 frames cannot serve a clip reading 150");
    let text = err.to_string();
    assert!(text.contains("reach frame 150"), "names the real reach: {text}");
    assert_eq!(session.offline_sources().len(), 1, "a refusal changes nothing");
}
