//! Saving a project and opening it again: the timeline that comes back is the
//! one that went in, a folder of media plus its `.edith` can be moved
//! anywhere, and every way the files on disk can have changed underneath is a
//! refusal that says which file and why.
//!
//! ```text
//! cargo test -p engine --release --test saveload -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A fresh directory to litter. Canonical, because the saved paths are
/// relative to it and `Project` canonicalises what it is handed.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ve_saveload_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::canonicalize(&dir).expect("canonical scratch dir")
}

fn copy_in(dir: &Path, name: &str) -> PathBuf {
    let to = dir.join(name);
    std::fs::copy(asset(name), &to).expect("copy the fixture");
    to
}

/// Two files, four clips, one of them deleted, playhead at 3 s -- a timeline
/// with something to lose in every field.
fn edited(dir: &Path) -> PlaybackSession {
    let mut session = PlaybackSession::open(copy_in(dir, "test_av.mp4")).expect("open");
    session
        .import(&copy_in(dir, "test_av2.mp4"))
        .expect("import the second file");
    assert!(session.cut_at(2.0), "cut inside the first file");
    assert!(session.cut_at(6.5), "cut inside the second file");
    assert!(session.delete_clip(1), "drop the second half of the first");
    session.seek(3.0);
    assert_eq!(session.timeline_duration(), 6.0);
    session
}

/// What has to survive a round trip, all of it public API.
fn shape(session: &PlaybackSession) -> (Vec<(f64, f64, usize)>, Vec<PathBuf>, f64) {
    (
        session.clip_spans_by_source(),
        session.sources().to_vec(),
        session.timeline_duration(),
    )
}

#[test]
fn a_saved_project_reopens_as_the_same_timeline() {
    let dir = scratch("round_trip");
    let saved = edited(&dir);
    let path = dir.join("edit.edith");
    saved.save_project(&path).expect("save");

    let mut loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(shape(&loaded), shape(&saved), "clips, sources and duration");
    assert!(
        (loaded.now() - saved.now()).abs() < 1.0 / loaded.meta().frame_rate,
        "playhead landed at {:.3}s, saved at {:.3}s",
        loaded.now(),
        saved.now()
    );
    assert!(!loaded.is_playing(), "a loaded project starts paused");
    assert!(!loaded.undo(), "history is not saved");

    // Saving what was loaded reproduces the file byte for byte -- the format
    // has no state the round trip drops.
    let again = dir.join("again.edith");
    loaded
        .save_project(&again)
        .expect("save the loaded project");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&again).unwrap(),
        "save -> load -> save is not a fixed point"
    );
    assert!(!path.with_extension("edith.part").exists(), "left a .part");
}

#[test]
fn a_folder_of_media_and_its_project_can_be_moved() {
    let from = scratch("relocate_from");
    let saved = edited(&from);
    saved.save_project(&from.join("edit.edith")).expect("save");
    drop(saved);

    let to = scratch("relocate_to");
    for name in ["test_av.mp4", "test_av2.mp4", "edit.edith"] {
        std::fs::copy(from.join(name), to.join(name)).expect("copy");
    }
    // The originals go away entirely: nothing absolute can be resolving here.
    std::fs::remove_dir_all(&from).expect("remove the original folder");

    let moved = PlaybackSession::open_project(&to.join("edit.edith")).expect("open the copy");
    assert_eq!(moved.timeline_duration(), 6.0);
    assert!(
        moved.sources().iter().all(|s| s.starts_with(&to)),
        "sources did not follow the folder: {:?}",
        moved.sources()
    );
}

#[test]
fn orphan_sources_are_not_written() {
    let dir = scratch("orphans");
    let mut session = PlaybackSession::open(copy_in(&dir, "test_av.mp4")).expect("open");
    session
        .import(&copy_in(&dir, "test_av2.mp4"))
        .expect("import");
    assert!(session.undo(), "take the import back");
    assert_eq!(
        session.sources().len(),
        2,
        "the orphan entry stays in-session"
    );

    let path = dir.join("orphan.edith");
    session.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("source ")).count(),
        1,
        "the orphan reached the file:\n{text}"
    );

    // And the load survives the orphan's file disappearing, which is the whole
    // point of pruning it.
    std::fs::remove_file(dir.join("test_av2.mp4")).expect("unlink the orphan");
    let loaded = PlaybackSession::open_project(&path).expect("open the project");
    assert_eq!(loaded.sources().len(), 1);
    assert_eq!(loaded.timeline_duration(), 5.0);
}

#[test]
fn a_source_that_vanished_is_refused_by_name() {
    let dir = scratch("missing");
    let session = edited(&dir);
    let path = dir.join("edit.edith");
    session.save_project(&path).expect("save");
    drop(session);

    let gone = dir.join("test_av2.mp4");
    std::fs::remove_file(&gone).expect("unlink");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a project whose source is gone must not open")
        .to_string();
    assert!(
        err.contains("test_av2.mp4"),
        "the refusal must name the file: {err}"
    );

    // The first source is no different -- it is simply refused earlier.
    std::fs::remove_file(dir.join("test_av.mp4")).expect("unlink");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("no first source, no session")
        .to_string();
    assert!(err.contains("test_av.mp4"), "{err}");
}

#[test]
fn a_source_that_shrank_is_refused_by_clip() {
    let dir = scratch("shrunk");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("shrunk.edith");
    // Hand-written: a clip that ran to frame 10000 of a 150-frame file, which
    // is what re-encoding a source shorter leaves behind.
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 30 0\nclip 30 10000 0\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a clip past the end of its file must not open")
        .to_string();
    assert!(
        err.contains("clip 1") && err.contains("test_av.mp4") && err.contains("150"),
        "the refusal must name the clip, the file and its length: {err}"
    );

    // One frame short of the end is still inside the file.
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 150 0\n",
    )
    .expect("write");
    let whole = PlaybackSession::open_project(&path).expect("the exact length is legal");
    assert_eq!(whole.timeline_duration(), 5.0);
}

#[test]
fn a_project_that_names_itself_is_refused() {
    let dir = scratch("self_reference");
    let path = dir.join("ouroboros.edith");
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource ouroboros.edith\nclip 0 30 0\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a project is not a video")
        .to_string();
    assert!(err.contains("ouroboros.edith"), "{err}");
}

#[test]
fn malformed_files_are_numbered_errors_and_never_panics() {
    let dir = scratch("malformed");
    copy_in(&dir, "test_av.mp4");
    let path = dir.join("bad.edith");
    let good = "edith 1\nplayhead 0\nsource test_av.mp4\nclip 0 30 0\n";

    for (text, want) in [
        ("edith 2\nsource test_av.mp4\nclip 0 30 0\n", "line 1"),
        ("not a project at all\n", "line 1"),
        ("edith 1\nsource test_av.mp4\nclip 0 30\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nclip 0 30 4\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nclip 30 30 0\n", "line 3"),
        ("edith 1\nsource test_av.mp4\n\nclip 0 30 0\n", "line 3"),
        ("edith 1\nsource test_av.mp4\nwhat 1\n", "line 3"),
    ] {
        std::fs::write(&path, text).expect("write");
        let err = PlaybackSession::open_project(&path)
            .err()
            .unwrap_or_else(|| panic!("accepted {text:?}"))
            .to_string();
        assert!(err.starts_with(want), "{text:?} -> {err}");
    }

    // Every truncation of a good file: some are valid shorter projects, none
    // may panic or be reported as anything but an error.
    for cut in 0..good.len() {
        std::fs::write(&path, &good[..cut]).expect("write");
        let _ = PlaybackSession::open_project(&path);
    }
    // ...and a file that is not there at all.
    std::fs::remove_file(&path).expect("unlink");
    assert!(PlaybackSession::open_project(&path).is_err());
}

#[test]
fn a_source_that_no_longer_matches_the_timeline_is_refused_in_import_words() {
    let dir = scratch("mismatch");
    copy_in(&dir, "test_av.mp4");
    std::fs::copy(asset("test_mismatch.mp4"), dir.join("test_av2.mp4")).expect("substitute");
    let path = dir.join("swapped.edith");
    std::fs::write(
        &path,
        "edith 1\nplayhead 0\nsource test_av.mp4\nsource test_av2.mp4\nclip 0 30 0\nclip 0 30 1\n",
    )
    .expect("write");
    let err = PlaybackSession::open_project(&path)
        .err()
        .expect("a source that stopped matching must not open")
        .to_string();
    // The suffix is `import`'s own refusal, word for word.
    assert_eq!(
        err,
        format!(
            "source {}: 640x360 does not match the timeline's 1280x720",
            dir.join("test_av2.mp4").display()
        )
    );
}
