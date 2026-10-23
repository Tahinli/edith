//! Source paths in a saved project: a file inside the project's directory tree
//! is written relative to the project, however its path was spelled, and so a
//! folder of media and its project can be copied anywhere and still open with
//! nothing offline.

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// A project at `dir/edit.edith` whose two sources both sit in `dir/media/`,
/// one of them placed on the timeline after the other.
fn project_with_media_in_a_subfolder(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("media")).expect("the media folder");
    std::fs::copy(asset("test_av.mp4"), dir.join("media/a.mp4")).expect("copy a");
    std::fs::copy(asset("test_av2.mp4"), dir.join("media/b.mp4")).expect("copy b");
    engine::ao::set_silent_output();
    let mut session = PlaybackSession::open(dir.join("media/a.mp4")).expect("open a");
    let second = dir.join("media/b.mp4");
    session.import(&second).expect("import b");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &second, 0, None)
            .expect("place b")
    );
    let project = dir.join("edit.edith");
    session.save_project(&project).expect("save");
    project
}

#[test]
fn media_in_a_subfolder_is_saved_relative_and_survives_a_copy() {
    let from = Scratch::dir("ve_relative_from");
    let project = project_with_media_in_a_subfolder(&from);
    let text = std::fs::read_to_string(&project).unwrap();
    assert!(
        text.contains("media/a.mp4") && text.contains("media/b.mp4"),
        "the sources are written relative to the project:\n{text}"
    );
    assert!(
        !text.contains(from.to_str().unwrap()),
        "no absolute path into the original folder:\n{text}"
    );

    // Copy the whole folder somewhere else, then remove the original: nothing
    // absolute can still resolve.
    let to = Scratch::dir("ve_relative_to");
    std::fs::create_dir_all(to.join("media")).unwrap();
    for name in ["media/a.mp4", "media/b.mp4", "edit.edith"] {
        std::fs::copy(from.join(name), to.join(name)).expect("copy");
    }
    std::fs::remove_dir_all(&from).expect("remove the original");

    let moved = PlaybackSession::open_project(&to.join("edit.edith")).expect("open the copy");
    assert!(
        moved.offline_sources().is_empty(),
        "offline after a copy: {:?}",
        moved.offline_sources()
    );
    assert!(moved.sources().iter().all(|s| s.path.starts_with(&to)));
}

#[test]
fn a_project_opened_through_a_symlink_saves_relative_sources() {
    let real = Scratch::dir("ve_relative_real");
    let project = project_with_media_in_a_subfolder(&real);
    let link = real.with_extension("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    // Opened through the link, the sources are reached through it too; saving
    // must still write them relative to the project.
    let via = PlaybackSession::open_project(&link.join("edit.edith")).expect("open via link");
    let out = link.join("again.edith");
    via.save_project(&out).expect("save via link");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("source 0 media/a.mp4") && text.contains("media/b.mp4"),
        "relative through a link:\n{text}"
    );
    assert!(!text.contains(real.to_str().unwrap()), "absolute:\n{text}");
    std::fs::remove_file(&link).ok();
    drop(project);
}

#[test]
fn a_file_outside_the_project_is_still_written_absolute() {
    let dir = Scratch::dir("ve_relative_outside");
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    std::fs::copy(asset("test_av.mp4"), dir.join("elsewhere.mp4")).unwrap();
    engine::ao::set_silent_output();
    let session = PlaybackSession::open(dir.join("elsewhere.mp4")).expect("open");
    let project = dir.join("proj/edit.edith");
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).unwrap();
    let absolute = dir.join("elsewhere.mp4");
    assert!(
        text.contains(absolute.to_str().unwrap()),
        "outside the tree stays absolute:\n{text}"
    );
}
