//! Sequences and compound clips through the session: nesting keeps the picture and the sound
//! the flat timeline had, the project file round-trips several sequences (v45) and writes the
//! old dialect when there is only one, and every step undoes.
//!
//! ```text
//! cargo test -p engine --release --test sequences
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::project::{Lane, LaneKind};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn cut_session() -> PlaybackSession {
    let mut s = PlaybackSession::open(asset("test_25fps.mp4")).expect("open the fixture");
    s.set_gain(0.0);
    assert!(s.cut_at(0.4));
    assert!(s.cut_at(0.8));
    s
}

fn starts(s: &PlaybackSession, lane: Lane) -> Vec<(u32, u32, u32, usize)> {
    s.lane_clips(lane).iter().map(|c| (c.start, c.in_frame, c.out_frame, c.source)).collect()
}

#[test]
fn a_nested_render_is_the_flat_render() {
    let flat = cut_session();
    let mut nested = cut_session();
    let n = nested.nest(&[(Lane::V1, 1), (Lane::V1, 2), (Lane::A1, 1), (Lane::A1, 2)], Some("Tail")).expect("nest");
    assert_eq!(n.clips.len(), 2, "one compound clip each for the picture and the sound");
    assert_eq!(nested.lane_clips(Lane::V1).len(), 2, "the head and the compound");
    assert!(nested.compound_at(Lane::V1, 1).is_some());
    for frame in [0u32, 9, 10, 15, 19, 20, 24] {
        let a = flat.compose_at(frame, 320, 180).expect("flat frame");
        let b = nested.compose_at(frame, 320, 180).expect("nested frame");
        assert_eq!(a.bgra, b.bgra, "frame {frame} differs between the flat and the nested timeline");
    }
    let (fp, _) = flat.export_snapshot();
    let (np, _) = nested.export_snapshot();
    let fps = flat.meta().frame_rate;
    assert_eq!(fp.audio_segments_from(0, fps), np.audio_segments_from(0, fps), "the sound is the same segments");
    assert_eq!(fp.timeline_frames(), np.timeline_frames());
}

#[test]
fn sequences_round_trip_through_the_file() {
    let dir = Scratch::dir("ve_sequences_roundtrip");
    let media = dir.join("test_25fps.mp4");
    std::fs::copy(asset("test_25fps.mp4"), &media).expect("copy");
    let mut s = PlaybackSession::open(&media).expect("open");
    s.set_gain(0.0);
    assert!(s.cut_at(0.4));
    let n = s.nest(&[(Lane::V1, 1), (Lane::A1, 1)], Some("Back half")).expect("nest");
    let other = s.new_sequence(Some("Scratch pad")).expect("new");
    s.switch_sequence(other).expect("switch");
    assert!(s.place_at(Lane::V1, 0.0, engine::Clip::source_range(0, 0, 10)));
    s.switch_sequence(0).expect("back");
    let path = dir.join("p.edith");
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.starts_with("edith 45\n"), "{}", text.lines().next().unwrap_or(""));
    assert!(text.contains("\nseqactive 0 Sequence 1\n"), "{text}");
    assert!(text.contains("\nseq 1 Back half\n") && text.contains("\nendseq\n"), "{text}");

    let back = PlaybackSession::open_project(&path).expect("reopen");
    let names: Vec<String> = back.sequences().iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["Sequence 1", "Scratch pad", "Back half"], "the tab order is kept");
    assert_eq!(back.active_sequence(), 0);
    assert_eq!(starts(&back, Lane::V1), starts(&s, Lane::V1));
    assert_eq!(starts(&back, Lane::A1), starts(&s, Lane::A1));
    assert_eq!(back.compound_at(Lane::V1, 1), Some(n.sequence));
    // The parked ones came back whole.
    let mut back = back;
    back.switch_sequence(other).expect("switch");
    assert_eq!(starts(&back, Lane::V1), vec![(0, 0, 10, 0)]);
    back.switch_sequence(n.sequence).expect("switch");
    assert_eq!(back.lane_clips(Lane::V1).len(), 1);
    assert_eq!(back.lane_clips(Lane::A1).len(), 1);
    // And the render of the reopened project is the render that was saved.
    back.switch_sequence(0).expect("switch");
    let a = s.compose_at(15, 160, 90).expect("frame");
    let b = back.compose_at(15, 160, 90).expect("frame");
    assert_eq!(a.bgra, b.bgra);
}

#[test]
fn one_sequence_writes_the_old_dialect_and_an_old_file_is_one_sequence() {
    let dir = Scratch::dir("ve_sequences_old");
    let media = dir.join("test_25fps.mp4");
    std::fs::copy(asset("test_25fps.mp4"), &media).expect("copy");
    let s = PlaybackSession::open(&media).expect("open");
    s.set_gain(0.0);
    let path = dir.join("one.edith");
    s.save_project(&path).expect("save");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(!text.starts_with("edith 45") && !text.contains("seq"), "{text}");
    // A v44 file is read as it always was.
    let old = text.replacen(text.lines().next().unwrap_or(""), "edith 44", 1);
    let p44 = dir.join("v44.edith");
    std::fs::write(&p44, old).expect("write");
    let back = PlaybackSession::open_project(&p44).expect("open v44");
    assert_eq!(back.sequences().len(), 1);
    assert!(back.sequences()[0].active);
}

#[test]
fn nest_break_apart_and_undo_through_the_session() {
    let mut s = cut_session();
    let before = (starts(&s, Lane::V1), starts(&s, Lane::A1));
    s.nest(&[(Lane::V1, 0), (Lane::V1, 1), (Lane::A1, 0), (Lane::A1, 1)], None).expect("nest");
    assert_eq!(s.sequences().len(), 2);
    s.break_apart(&[(Lane::V1, 0)]).expect("break apart");
    assert_eq!((starts(&s, Lane::V1), starts(&s, Lane::A1)), before, "nest then break apart is the identity");
    assert!(s.undo());
    assert!(s.compound_at(Lane::V1, 0).is_some(), "undo of the break apart brings the compound back");
    assert!(s.undo());
    assert_eq!((starts(&s, Lane::V1), starts(&s, Lane::A1)), before);
    assert_eq!(s.sequences().len(), 1, "and the sequence the nest made goes with it");
    assert!(s.redo());
    assert_eq!(s.sequences().len(), 2);
}

#[test]
fn a_sequence_cannot_contain_itself() {
    let mut s = cut_session();
    let n = s.nest(&[(Lane::V1, 0), (Lane::A1, 0)], Some("Inner")).expect("nest");
    s.switch_sequence(n.sequence).expect("open it");
    let err = s.place_sequence_at(0, Lane::V1, 5.0).unwrap_err().to_string();
    assert!(err.contains("inside itself"), "{err}");
    let err = s.place_sequence_at(n.sequence, Lane::V1, 5.0).unwrap_err().to_string();
    assert!(err.contains("cannot contain itself"), "{err}");
    s.switch_sequence(0).expect("back");
    assert!(s.delete_sequence(n.sequence).is_err(), "a played sequence is not deleted");
    let _ = LaneKind::Video;
}

#[test]
fn nesting_the_picture_takes_its_linked_sound_along() {
    let mut s = cut_session();
    let n = s.nest(&[(Lane::V1, 1)], Some("Middle")).expect("nest");
    assert_eq!(n.clips.len(), 2, "the linked sound goes into the compound with the picture");
    assert!(s.compound_at(Lane::V1, 1).is_some());
    assert!(s.compound_at(Lane::A1, 1).is_some(), "no detached sound is left behind");
}

#[test]
fn a_compounds_own_placement_and_grade_reach_the_picture() {
    use engine::color::ColorParams;
    use engine::transform::TransformParams;
    let place = TransformParams { pos_x: 0.1, scale: 0.7, opacity: 0.5, ..TransformParams::default() };
    let grade = ColorParams { brightness: 0.2, saturation: 0.5, ..ColorParams::default() };
    let mut flat = cut_session();
    for i in [1, 2] {
        assert!(flat.set_transform(Lane::V1, i, Some(place)));
        assert!(flat.set_color(Lane::V1, i, Some(grade)));
    }
    let mut nested = cut_session();
    nested.nest(&[(Lane::V1, 1), (Lane::V1, 2), (Lane::A1, 1), (Lane::A1, 2)], None).expect("nest");
    let plain = nested.compose_at(12, 320, 180).expect("plain frame");
    assert!(nested.set_transform(Lane::V1, 1, Some(place)));
    assert!(nested.set_color(Lane::V1, 1, Some(grade)));
    for frame in [12u32, 20] {
        let a = flat.compose_at(frame, 320, 180).expect("flat frame");
        let b = nested.compose_at(frame, 320, 180).expect("nested frame");
        assert_eq!(a.bgra, b.bgra, "frame {frame}: the compound's own look is the look of its clips");
    }
    assert_ne!(plain.bgra, nested.compose_at(12, 320, 180).expect("frame").bgra, "and it does change the picture");
}

#[test]
fn a_compounds_pieces_and_a_sources_nested_use_are_visible() {
    let mut s = cut_session();
    let n = s.nest(&[(Lane::V1, 1), (Lane::V1, 2), (Lane::A1, 1), (Lane::A1, 2)], Some("Tail")).expect("nest");
    // The source is played by clips inside the new sequence, not by the active one's own clips.
    assert_eq!(s.source_uses_parked(0), vec![(n.sequence, 4)]);
    let compound = s.lane_clips(Lane::V1)[1].clone();
    let pieces = s.compound_pieces(LaneKind::Video, &compound);
    assert_eq!(pieces.len(), 2, "the two cut pieces of the picture");
    assert_eq!(pieces[0].start, 0, "placed relative to the compound's own start");
    assert!(pieces.iter().all(|p| p.source == 0));
    assert_eq!(pieces.iter().map(|p| p.frames()).sum::<u32>(), compound.frames());
    let sound = s.lane_clips(Lane::A1)[1].clone();
    assert_eq!(s.compound_pieces(LaneKind::Audio, &sound).len(), 2, "and the pieces of the sound");
    assert!(s.compound_pieces(LaneKind::Video, &s.lane_clips(Lane::V1)[0].clone()).is_empty(), "a plain clip has none");
}
