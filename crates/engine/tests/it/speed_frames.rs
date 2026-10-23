//! Clip lengths against their sources, through the front door: a file placed on
//! the timeline, sped up, trimmed, saved and opened again. The invariant under
//! test is the one a load holds a project to -- no clip reads past the end of its
//! source on its own lane -- at odd speeds and on sources whose picture and sound
//! do not end on the same frame.
//!
//! `test_23976fps.mp4` is 48 frames of 24000/1001 fps, 2.002 s of picture, and its
//! sound is exactly 2.000 s: 61 frames on a 30 fps timeline for the picture, 60 for
//! the sound. A grouped placement must be 60 frames on both lanes or one of them
//! reads past its source.

use std::path::{Path, PathBuf};

use engine::project::{Edge, Lane, LaneKind, Speed, WallBound};
use engine::scratch::Scratch;
use engine::{AudioSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The 30 fps timeline with `name` placed at its end as a grouped take.
fn placed(name: &str) -> PlaybackSession {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open the 30 fps scaffold");
    s.set_gain(0.0);
    let file = asset(name);
    s.import(&file).expect("the file joins the timeline");
    let end = s.timeline_duration();
    assert!(
        s.place_stream_at(end, &file, 0, None).expect("the drag"),
        "the file is placed"
    );
    s
}

/// The last clip on `lane`, which is the one a test just placed.
fn last(s: &PlaybackSession, lane: Lane) -> engine::Clip {
    s.lane_clips(lane).last().expect("a clip was placed").clone()
}

/// A sound's length in timeline frames, the way the audio lane is walled: its
/// playing time rounded up at 30 fps.
fn sound_frames(path: &Path, stream: usize) -> u32 {
    let secs = AudioSession::stream_duration_secs(path, stream)
        .expect("the stream reads")
        .expect("it has a length");
    (secs * 30.0).ceil() as u32
}

/// Every clip on every lane, checked against the length its source has on that lane.
fn assert_within_sources(s: &PlaybackSession) {
    assert_within_sources_for(s, "");
}

fn assert_within_sources_for(s: &PlaybackSession, case: &str) {
    for lane in s.lanes() {
        for (i, c) in s.lane_clips(lane).iter().enumerate() {
            let source = &s.sources()[c.source];
            let wall = match lane.kind {
                LaneKind::Audio => sound_frames(&source.path, source.audio_stream),
                _ => s.file_frames(&source.path),
            };
            assert!(
                c.out_frame <= wall,
                "{case}{lane:?} clip {i} ends at {} past its {wall} frames",
                c.out_frame
            );
        }
    }
}

fn save_and_open(s: &PlaybackSession, out: &Path) -> PlaybackSession {
    s.save_project(out).expect("the project saves");
    PlaybackSession::open_project(out).expect("the project it saved opens")
}

#[test]
fn a_grouped_placement_is_held_to_the_sound_on_both_lanes() {
    let s = placed("test_23976fps.mp4");
    let picture = last(&s, Lane::V1);
    let sound = last(&s, Lane::A1);
    assert_eq!(sound.out_frame, 60, "2.000 s of sound is 60 frames");
    assert_eq!(picture.out_frame, sound.out_frame, "one take, one length");
    assert_within_sources(&s);

    let out = Scratch::file("edith-frames-placed", "edith");
    let back = save_and_open(&s, &out);
    assert!(back.load_warnings().is_empty(), "nothing needed clamping: {:?}", back.load_warnings());
    assert_within_sources(&back);
}

/// Speed and save at factors that do not divide the frame counts: whatever the
/// rounding, the clip a file reopens to is the clip that was saved, with no clamp.
#[test]
fn odd_speeds_round_trip_through_save_and_load() {
    for name in ["test_23976fps.mp4", "test_25fps.mp4"] {
        for permille in [333u16, 700, 1100] {
            let mut s = placed(name);
            let idx = s.lane_clips(Lane::V1).len() - 1;
            s.set_speed(Lane::V1, idx, Speed::from_permille(permille))
                .expect("a clip at the end of the timeline can be re-timed");
            let before = last(&s, Lane::V1);
            assert_eq!(
                before.frames(),
                Speed::from_permille(permille).frames(before.len()),
                "the footprint is the speed's own arithmetic"
            );
            assert_within_sources(&s);

            let out = Scratch::file(&format!("edith-frames-{permille}"), "edith");
            let back = save_and_open(&s, &out);
            assert!(
                back.load_warnings().is_empty(),
                "{name} at {permille}: {:?}",
                back.load_warnings()
            );
            let after = last(&back, Lane::V1);
            assert_eq!(
                (after.in_frame, after.out_frame, after.speed),
                (before.in_frame, before.out_frame, before.speed),
                "{name} at {permille} reopens as it was saved"
            );
            assert_within_sources(&back);
        }
    }
}

/// A project that was saved past its source (by an older build) still opens: the
/// clip is clamped to the source's end, and the load says so.
#[test]
fn a_clip_past_its_source_is_clamped_on_load_and_reported() {
    let s = placed("test_23976fps.mp4");
    let out = Scratch::file("edith-frames-overrun", "edith");
    s.save_project(&out).expect("saves");
    let text = std::fs::read_to_string(&*out).expect("read the saved file");
    let mut tampered = String::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let mut fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        if fields.first().map(String::as_str) == Some("audio")
            && fields.get(2).map(String::as_str) == Some("150")
            && fields.len() > 4
        {
            let out_frame: u32 = fields[4].parse().expect("a number");
            fields[4] = (out_frame + 2).to_string();
        }
        tampered.push_str(&fields.join(" "));
        tampered.push('\n');
    }
    std::fs::write(&*out, tampered).expect("write the tampered file");

    let back = PlaybackSession::open_project(&out).expect("an overrun project still opens");
    assert_eq!(back.load_warnings().len(), 1, "one clamped clip: {:?}", back.load_warnings());
    assert!(back.load_warnings()[0].contains("clamped to 60"));
    assert_eq!(last(&back, Lane::A1).out_frame, 60);
    assert_within_sources(&back);

    // ...and what it re-saves is clean.
    let again = Scratch::file("edith-frames-overrun-again", "edith");
    let reopened = save_and_open(&back, &again);
    assert!(reopened.load_warnings().is_empty());
}

/// At half speed a trim can only land on the timeline frames a source frame
/// reaches, so an edge asked for on an odd frame snaps to the one below -- and an
/// edge asked for past the source's end stops at the end, with the media wall
/// reported as the reason rather than a neighbour that is not there.
#[test]
fn a_trim_of_a_sped_clip_stops_on_the_frames_it_can_reach() {
    let mut s = placed("test_23976fps.mp4");
    let idx = s.lane_clips(Lane::V1).len() - 1;
    s.set_speed(Lane::V1, idx, Speed::from_permille(500)).expect("slow it");
    let start = last(&s, Lane::V1).start;
    let end = last(&s, Lane::V1).end();
    let (lo, hi) = s.trim_room(Lane::V1, idx, Edge::End).expect("a clip");
    // The picture lane is walled by the picture's own 61 frames, at half speed
    // 122 timeline frames from the start; the sound's 60 is the group's end.
    let picture = s.file_frames(&asset("test_23976fps.mp4"));
    assert_eq!(hi, start + Speed::from_permille(500).frames(picture));
    assert!(hi >= end, "the picture may reach its own last frame");
    let walls = s.trim_walls(Lane::V1, idx, Edge::End).expect("a clip");
    assert!(walls.hi_media, "the source is the wall, not a neighbour");
    assert!(lo > start);

    // An odd timeline frame inside the room: the edge lands on the frame below.
    let odd = start + 2 * 10 + 1;
    assert!(s.trim_clip(Lane::V1, idx, Edge::End, odd));
    let shorter = last(&s, Lane::V1);
    assert!(shorter.end() <= odd, "snapped down, never past the request");
    assert_eq!((shorter.end() - start) % 2, 0, "half speed lands on even offsets");
    assert_within_sources(&s);
}

/// The trim walls and the save check must be one wall. For every odd speed, on
/// the picture and the sound of a linked take, each edge is trimmed to each of
/// its two walls -- once straight there, and once after shrinking the clip to the
/// other wall first (the regrow) -- and what it leaves must save, reopen with no
/// clamp, and read within its source.
#[test]
fn every_trim_wall_of_a_retimed_take_saves_and_reopens() {
    let name = "test_23976fps.mp4";
    for lane in [Lane::V1, Lane::A1] {
        for permille in [333u16, 770, 1000, 3900] {
            let speed = Speed::from_permille(permille);
            for edge in [Edge::End, Edge::Start] {
                for shrink_first in [false, true] {
                    for toward_hi in [false, true] {
                        let case = format!("{lane:?} {permille} {edge:?} shrink={shrink_first} hi={toward_hi}");
                        let mut s = placed(name);
                        let idx = s.lane_clips(lane).len() - 1;
                        s.set_speed(lane, idx, speed)
                            .unwrap_or_else(|e| panic!("{case}: retime: {e}"));
                        assert_within_sources_for(&s, &format!("{case}: "));

                        let walls = s.trim_walls(lane, idx, edge).expect("a clip");
                        if shrink_first {
                            // The inner wall first: the edge pulled as far into the clip as it goes.
                            let inner = if edge == Edge::Start { walls.hi } else { walls.lo };
                            s.trim_clip(lane, idx, edge, inner);
                            assert_within_sources_for(&s, &format!("{case}: "));
                        }
                        let target = if toward_hi { walls.hi } else { walls.lo };
                        s.trim_clip(lane, idx, edge, target);
                        assert_within_sources_for(&s, &format!("{case}: "));
                        // A wall is a place the edge can reach: the edge lands on it.
                        let landed = match edge {
                            Edge::End => last(&s, lane).end(),
                            Edge::Start => last(&s, lane).start,
                        };
                        assert_eq!(landed, target, "{case}: the edge lands on the wall it was dragged to");

                        let out = Scratch::file(&format!("edith-sweep-{lane:?}-{permille}-{edge:?}-{shrink_first}-{toward_hi}"), "edith");
                        let before = last(&s, lane);
                        let back = save_and_open(&s, &out);
                        assert!(
                            back.load_warnings().is_empty(),
                            "{case}: reopened with clamps: {:?}",
                            back.load_warnings()
                        );
                        assert_eq!(
                            last(&back, lane).out_frame,
                            before.out_frame,
                            "{case}: reopens as saved"
                        );
                        assert_within_sources_for(&back, &format!("{case} reopened: "));
                    }
                }
            }
        }
    }
}

/// A trim that comes to rest at a wall says what set it: the source's own end
/// when the next clip is ten seconds away, a neighbour at its frame when the
/// next clip is the closer wall. Named before the trim, so the answer does not
/// depend on where the clip was left.
#[test]
fn trim_bounds_name_the_wall_that_stops_the_edge() {
    let file = asset("test_23976fps.mp4");
    let mut s = placed("test_23976fps.mp4");
    let idx = s.lane_clips(Lane::V1).len() - 1;
    let end = s.lane_clips(Lane::V1)[idx].end();

    // A second copy ten seconds later: the first clip's tail is the wall, far from
    // its neighbour.
    let later = end as f64 / 30.0 + 10.0;
    assert!(s.place_stream_at(later, &file, 0, None).expect("the drag"));
    let (lo, hi) = s.trim_bounds(Lane::V1, idx, Edge::End).expect("a clip");
    assert_eq!(hi, WallBound::Source, "the source's end, not the clip ten seconds away");
    assert_eq!(lo, WallBound::MinLength);
    let walls = s.trim_walls(Lane::V1, idx, Edge::End).expect("a clip");
    assert!(walls.hi_media && walls.hi > end, "the tail is past the clip's end");
    // Dragged far past the source: the edge stops on the source's end and says so.
    assert!(s.trim_clip(Lane::V1, idx, Edge::End, walls.hi + 500));
    assert_eq!(s.lane_clips(Lane::V1)[idx].end(), walls.hi, "lands on the source's end");
    assert_within_sources(&s);
    let (_, hi_after) = s.trim_bounds(Lane::V1, idx, Edge::End).expect("a clip");
    assert_eq!(hi_after, WallBound::Source);

    // A neighbour that is the closer wall: shorten the first clip, then place its
    // neighbour right at its new end, so the next clip sets the high wall.
    let mut t = placed("test_23976fps.mp4");
    let j = t.lane_clips(Lane::V1).len() - 1;
    let s0 = last(&t, Lane::V1).start;
    assert!(t.trim_clip(Lane::V1, j, Edge::End, s0 + 30));
    let cut = s0 + 30;
    assert!(t.place_stream_at(cut as f64 / 30.0, &file, 0, None).expect("the drag"));
    let (_, hi) = t.trim_bounds(Lane::V1, j, Edge::End).expect("a clip");
    assert_eq!(hi, WallBound::Neighbour(cut), "the clip at frame {cut} sets the wall");
}
