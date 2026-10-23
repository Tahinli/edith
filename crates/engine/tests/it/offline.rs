//! A project whose media has moved or been deleted opens with those sources
//! offline: their clips stay, nothing reads from them, and `relink_source` or
//! `open_project_relinked` points them back at the file. Only a project with no
//! saved timeline is kept when every source is gone.
//!
//! ```text
//! cargo test -p engine --release --test offline -- --nocapture
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

/// A saved project on one copied fixture, then that fixture moved away.
fn moved_project(name: &str) -> (Scratch, PathBuf, PathBuf, PathBuf) {
    let dir = Scratch::dir(&format!("ve_offline_{name}"));
    let media = dir.join("film.mp4");
    std::fs::copy(asset("test_av.mp4"), &media).expect("copy the fixture");
    let session = PlaybackSession::open(&media).expect("open the fixture");
    session.set_gain(0.0);
    let path = dir.join("proj.edith");
    session.save_project(&path).expect("save");
    let moved = dir.join("moved.mp4");
    std::fs::rename(&media, &moved).expect("move the media");
    (dir, path, media, moved)
}

#[test]
fn a_moved_source_opens_offline_with_its_clips() {
    let (_dir, path, _media, _moved) = moved_project("opens");
    let session = PlaybackSession::open_project(&path).expect("opens with the source offline");
    let offline = session.offline_sources();
    assert_eq!(offline.len(), 1, "{offline:?}");
    assert_eq!(offline[0].0, 0);
    assert!(!session.lanes().is_empty(), "the clips stay on the timeline");
}

#[test]
fn relinking_by_path_brings_the_source_back() {
    let (_dir, path, media, moved) = moved_project("relinked-open");
    let session = PlaybackSession::open_project_relinked(&path, &[(media, moved)])
        .expect("opens with the relink applied");
    assert!(session.offline_sources().is_empty());
}

#[test]
fn relink_source_checks_the_kind_and_accepts_the_same_material() {
    let (_dir, path, _media, moved) = moved_project("relink-kind");
    let mut session = PlaybackSession::open_project(&path).expect("opens offline");
    let sound = asset("test_ac3.mka");
    let refused = session.relink_source(0, &sound).expect_err("a sound file is not a picture");
    assert!(refused.to_string().contains("is a sound file"), "{refused}");
    assert_eq!(session.offline_sources().len(), 1, "a refusal changes nothing");
    let warnings = session.relink_source(0, &moved).expect("the same film relinks");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(session.offline_sources().is_empty());
}

#[test]
fn a_project_with_every_source_gone_opens_with_all_of_them_offline() {
    let (_dir, path, _media, _moved) = moved_project("all-gone");
    let session = PlaybackSession::open_project(&path).expect("opens, every source offline");
    assert_eq!(session.offline_sources().len(), session.sources().len());
    assert!(!session.lanes().is_empty(), "the clips stay");
}
