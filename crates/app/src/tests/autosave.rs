//! The autosave's writer and the crash path: an autosave lands whole or not at
//! all, a failed one leaves the saved file alone, a recovery is one undo step
//! over the saved project, and an autosave that outlives a crash is offered
//! again on the next open.

use super::*;
use crate::Player;
use crate::player::library::{recovered_session, recovery_replaces_words};
use engine::project::{LaneKind, Marker, MarkerColor};
use engine::scratch::Scratch;

/// Makes `sidecar` the younger of the two files, as it is after a save and
/// then an autosave of edits made since. The mtimes are set rather than left
/// to the clock, so the offer does not depend on how fast the test ran.
fn autosave_is_newer(project: &Path, sidecar: &Path) {
    let now = std::time::SystemTime::now();
    std::fs::File::open(project)
        .unwrap()
        .set_modified(now - Duration::from_secs(3600))
        .unwrap();
    std::fs::File::open(sidecar)
        .unwrap()
        .set_modified(now)
        .unwrap();
}

/// A successful autosave is renamed into place: no temp file is left beside
/// it, and what is there opens as a project.
#[test]
fn an_autosave_leaves_no_temp_file_behind() {
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let dir = Scratch::dir("ve_autosave_temp");
    let file = dir.join("p.autosave.edith");

    session.save_autosave(&file).expect("write the autosave");

    assert!(file.is_file(), "the autosave is on disk");
    assert!(
        !dir.join("p.autosave.edith.part").exists(),
        "the temp file was renamed over the name, not left beside it"
    );
    assert!(
        PlaybackSession::open_project(&file).is_ok(),
        "it opens as a project"
    );
}

/// An autosave that cannot start its temp file fails, and the autosave that
/// was already on disk is exactly as it was: the next good write replaces
/// it whole, and a crash in between still has something to recover.
#[test]
fn a_failed_autosave_leaves_the_one_on_disk_intact() {
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let dir = Scratch::dir("ve_autosave_intact");
    let file = dir.join("p.autosave.edith");
    session.save_autosave(&file).expect("the first write");
    let before = std::fs::read(&file).expect("read the first write");

    // A directory where the temp file has to be created: the write cannot
    // start, so nothing reaches the real name.
    std::fs::create_dir(dir.join("p.autosave.edith.part")).expect("block the temp file");
    assert!(
        session.save_autosave(&file).is_err(),
        "the blocked write reports itself"
    );

    assert_eq!(std::fs::read(&file).expect("read it again"), before);
    assert!(PlaybackSession::open_project(&file).is_ok(), "still opens");
}

/// Recovering takes the autosave's edits on the saved project as one undo
/// step: the edits are there, and one undo takes them all back to the saved
/// timeline.
#[test]
fn a_recovery_is_one_undo_step_over_the_saved_project() {
    let dir = Scratch::dir("ve_recover_step");
    let saved = dir.join("p.edith");
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.save_project(&saved).expect("the manual save");
    let saved_lanes = PlaybackSession::open_project(&saved)
        .expect("reopen")
        .lanes();

    let mut edited = PlaybackSession::open_project(&saved).expect("reopen");
    let v2 = edited.add_lane(LaneKind::Video);
    let auto = dir.join("p.autosave.edith");
    edited.save_autosave(&auto).expect("autosave the edit");

    let mut recovered = recovered_session(Some(&saved), &auto).expect("recovers");
    assert!(recovered.lanes().contains(&v2), "the unsaved lane is back");
    assert_eq!(recovered.undo_depth(), 1, "the recovery is one step");
    assert_eq!(recovered.undo_label(), Some("Recover unsaved edits"), "and it is named");

    assert!(recovered.undo());
    assert_eq!(
        recovered.lanes(),
        saved_lanes,
        "one undo gives the saved timeline"
    );
    assert!(!recovered.lanes().contains(&v2));
}

/// Recover keeps the edits made since the file was opened: the window's own
/// timeline becomes one undo step back, and undo brings it back to the exact
/// state (lanes and revision) it had before the recovery.
#[test]
fn recovering_over_edits_made_since_the_open_is_one_undo_step_back_to_them() {
    let dir = Scratch::dir("ve_recover_keeps");
    let saved = dir.join("p.edith");
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.save_project(&saved).expect("the manual save");
    let saved_revision = PlaybackSession::open_project(&saved).expect("reopen").revision();

    // The autosave holds one edit (a lane the file does not have).
    let mut autosaved = PlaybackSession::open_project(&saved).expect("reopen");
    let recovered_lane = autosaved.add_lane(LaneKind::Video);
    let auto = dir.join("p.autosave.edith");
    autosaved.save_autosave(&auto).expect("autosave the edit");

    // The window kept working after the open: its own edit, never autosaved.
    let mut current = PlaybackSession::open_project(&saved).expect("reopen");
    let own_lane = current.add_lane(LaneKind::Audio);
    let before = current.lanes();
    let before_revision = current.revision();
    assert_ne!(before_revision, saved_revision, "the window is off the saved timeline");

    let recovered = recovered_session(Some(&saved), &auto).expect("recovers");
    assert!(current.adopt_as_step(&recovered), "the recovery goes on the window's edits");
    assert!(current.lanes().contains(&recovered_lane), "the recovered edit is in");
    assert!(!current.lanes().contains(&own_lane), "and the window's own edit is replaced");

    assert!(current.undo(), "the recovery is one step");
    assert_eq!(current.lanes(), before, "undo gives the window's edits back");
    assert_eq!(current.revision(), before_revision, "by the same revision they had");
    assert!(current.redo());
    assert!(current.lanes().contains(&recovered_lane));
}

/// The recovery toast says what recovering costs once the window has edits.
#[test]
fn the_recovery_toast_says_recovering_replaces_the_current_edits() {
    assert!(recovery_replaces_words().contains("Recovering will replace the current edits"));
}

/// An untitled session has no saved file to stand on: the autosave is the
/// project, and there is nothing earlier for undo to return to.
#[test]
fn an_untitled_recovery_is_the_autosave_with_nothing_to_undo_to() {
    let dir = Scratch::dir("ve_recover_untitled");
    let auto = dir.join("untitled.autosave.edith");
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let v2 = session.add_lane(LaneKind::Video);
    session.save_autosave(&auto).expect("autosave");

    let recovered = recovered_session(None, &auto).expect("recovers");

    assert!(recovered.lanes().contains(&v2));
    assert_eq!(recovered.undo_depth(), 0);
}

/// The kill test: a session saves, edits, autosaves, and then goes away with
/// nothing saved since (the crash). Reopening the project offers the
/// autosave, recovering it brings the edit back, and declining it leaves the
/// offer gone for good.
#[test]
fn unsaved_edits_survive_a_crash_and_are_offered_on_reopen() {
    let dir = Scratch::dir("ve_autosave_crash");
    let project = dir.join("p.edith");
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.save_project(&project).expect("the manual save");
    let v2 = session.add_lane(LaneKind::Video);
    let sidecar = Player::autosave_target(&project, true).expect("a place for the autosave");
    session.save_autosave(&sidecar).expect("the autosave");

    // The crash: no save, no close, the session is simply gone.
    drop(session);
    autosave_is_newer(&project, &sidecar);

    // The reopen: the offer names the autosave beside the project.
    assert_eq!(
        Player::recovery_offer(&project, true),
        Some(sidecar.clone())
    );
    let recovered = recovered_session(Some(&project), &sidecar).expect("recovers");
    assert!(
        recovered.lanes().contains(&v2),
        "the edit made before the crash is back"
    );

    // Declined instead: the autosave is deleted, and nothing is offered again.
    Player::discard_sidecar(&sidecar);
    assert!(!sidecar.exists());
    assert_eq!(Player::recovery_offer(&project, true), None);
}

/// The crash with several sequences: a second sequence, a copy of it, a compound
/// clip over a clip of the copy and a marker all live only in the autosave. The
/// recovery brings every sequence back, the compound clip and the marker with them.
#[test]
fn a_crash_with_several_sequences_recovers_every_one() {
    let dir = Scratch::dir("ve_autosave_sequences");
    let project = dir.join("p.edith");
    let media = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&media).expect("open the fixture");
    session.save_project(&project).expect("the manual save");
    let b = session.new_sequence(Some("Cut B")).expect("a second sequence");
    session.switch_sequence(b).expect("switch to it");
    assert!(session.place_stream_at(0.0, &media, 0, None).expect("placeable"));
    let copy = session.duplicate_sequence(b).expect("duplicate it");
    session.switch_sequence(copy).expect("switch to the copy");
    session.nest(&[(Lane::V1, 0)], Some("Inner")).expect("nest a clip");
    session.add_marker(Marker { frame: 3, length: 0, name: "beat".into(), note: String::new(), color: MarkerColor::Red })
        .expect("a marker");
    assert_eq!(session.markers().len(), 1, "the marker is on the sequence being edited");
    let sidecar = Player::autosave_target(&project, true).expect("a place for the autosave");
    session.save_autosave(&sidecar).expect("the autosave");

    // The crash: nothing saved since the manual save.
    let want = session.sequences();
    drop(session);
    autosave_is_newer(&project, &sidecar);

    let recovered = recovered_session(Some(&project), &sidecar).expect("every sequence recovers");
    assert_eq!(recovered.sequences(), want, "the same sequences, in the same order");
    assert_eq!(recovered.active_sequence(), copy, "the copy is still the one being edited");
    let markers = recovered.markers();
    assert_eq!(markers.len(), 1, "the marker comes back");
    assert_eq!((markers[0].frame, markers[0].name.as_str()), (3, "beat"));
}

/// A recovery that cannot read its autosave is refused as a whole: the saved file
/// is untouched, the autosave is still on disk, and the reason comes back as text.
#[test]
fn a_refused_recovery_keeps_the_saved_file_and_the_autosave() {
    let dir = Scratch::dir("ve_autosave_refused");
    let project = dir.join("p.edith");
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.save_project(&project).expect("the manual save");
    let sidecar = dir.join("p.autosave.edith");
    std::fs::write(&sidecar, b"not a project at all").expect("a damaged autosave");
    autosave_is_newer(&project, &sidecar);
    let before = std::fs::read(&project).expect("the saved file");

    let refused = recovered_session(Some(&project), &sidecar);
    assert!(refused.is_err(), "a damaged autosave is not recovered");
    assert!(sidecar.exists(), "the autosave is kept");
    assert_eq!(std::fs::read(&project).expect("the saved file"), before, "the saved file is untouched");
}

/// Opening a project is not an edit: the window comes up with nothing to undo, so Undo is
/// off until the person has changed something. The load's own fixes (a relink, a clamped
/// clip, the settings it carries) must not leave a step behind.
#[test]
fn opening_a_project_leaves_nothing_to_undo() {
    let dir = Scratch::dir("ve_open_no_undo");
    let saved = dir.join("p.edith");
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.save_project(&saved).expect("the manual save");

    let mut opened = PlaybackSession::open_project(&saved).expect("reopen");
    assert_eq!(opened.undo_depth(), 0, "a load pushes no undo step");
    assert_eq!(opened.undo_label(), None, "so there is no step to name");

    // The window's own default for a project with no seat of its own is applied on
    // load too, and it is no step either.
    assert_eq!(opened.encoder_seat(), engine::export::EncoderSeat::Auto, "the fixture carries no seat");
    crate::export_ui::gui_default_seat(&mut opened);
    assert_eq!(opened.encoder_seat(), engine::export::EncoderSeat::Software);
    assert_eq!(opened.undo_depth(), 0, "the default seat is not a pick");
}
