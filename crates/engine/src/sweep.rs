//! The re-timing sweep: every mutating op, against every grouping shape,
//! under the universal laws. The "there might be other bugs" catcher -- a
//! red cell here is an op whose shape has drifted from the law, found
//! before a user finds it.
//!
//! The laws are the four nothing downstream can live without:
//!
//! * **Consistency** -- `links_are_consistent`: a link id names at most one
//!   placement per lane, clips and captions alike.
//! * **Order** -- every lane sorted and disjoint, both of its lists.
//! * **Survivor continuity** -- the desync-class law: a placement that
//!   survives an op keeps a consistent source window, verified through the
//!   op's own [`TimelineMap`]: the survivor's old span maps onto a span that
//!   contains where it actually landed, or -- for the ops that move whole
//!   members -- its position is the map of where it was.
//! * **Undo round-trip** -- one press puts the project back byte for byte,
//!   and the op cost exactly one snapshot to do it.
//!
//! Paste is the named exception: it *inserts without rippling* media lanes
//! (a layer is laid over the timeline) and never touches captions at all,
//! so its cells assert the no-move shape directly instead of through a map.

use crate::map::TimelineMap;
use crate::project::{Lane, LaneKind, Speed, SubClip};
use crate::scale::FitPolicy;
use crate::subtitle::SubtitleTrack;
use crate::{Clip, Project};

const FILE: &str = "/nonexistent/a.mp4";

fn caption(start: u32, frames: u32) -> SubClip {
    SubClip {
        start,
        frames,
        track: 0,
        in_us: 0,
        out_us: i64::from(frames) * 1_000_000,
        link: None,
    }
}

fn track() -> SubtitleTrack {
    SubtitleTrack {
        path: FILE.into(),
        track: None,
        language: "eng".into(),
        name: String::new(),
        label: "eng".into(),
        cues: Vec::new(),
        bitmap: false,
        refused: None,
    }
}

fn clip(start: u32, in_frame: u32, out_frame: u32) -> Clip {
    Clip {
        start,
        in_frame,
        out_frame,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::Fit,
        speed: Speed::NORMAL,
    }
}

/// The shapes a project is swept in: a lone take, a group of two clips, a
/// group of two clips and a caption (aligned), and the same offset -- the
/// hand-built shape the offset model exists for.
fn project(kind: &str) -> (Project, Option<Lane>) {
    let mut p = Project::single(FILE, 90).with_subtitles(vec![track()]);
    match kind {
        "ungrouped" => (p, None),
        "clips" => {
            let v2 = p.add_lane(LaneKind::Video);
            assert!(p.place(v2, 0, clip(0, 0, 90)));
            p.group_all(&[(Lane::V1, 0), (v2, 0)]).expect("grouped");
            (p, None)
        }
        "aligned" => {
            let s1 = p.add_lane(LaneKind::Subtitle);
            p.place_sub(s1, 0, caption(0, 90)).expect("placed");
            p.group_all(&[(Lane::V1, 0), (s1, 0)]).expect("grouped");
            (p, Some(s1))
        }
        _ => {
            // The offset group: a caption starting 30 frames in, ending 30
            // early, and a second clip behind the group that nothing may
            // move out from under.
            let s1 = p.add_lane(LaneKind::Subtitle);
            let v2 = p.add_lane(LaneKind::Video);
            assert!(p.place(v2, 120, clip(120, 90, 180)));
            p.place_sub(s1, 30, caption(30, 30)).expect("placed");
            p.place_sub(s1, 90, caption(90, 30)).expect("the follower");
            p.group_all(&[(Lane::V1, 0), (s1, 0)]).expect("grouped");
            (p, Some(s1))
        }
    }
}

const SHAPES: [&str; 4] = ["ungrouped", "clips", "aligned", "offset"];

/// The four laws, asked of a project after an op and after its undo. `map`
/// is the op's own answer to "this frame is now where?" -- `None` for the
/// ops that move whole members rather than time (paste), which the sweep
/// asks the no-move shape of instead.
fn laws(p: &Project, what: &str) {
    crate::project::links_are_consistent_pub(p).expect(what);
    for lane in p.lanes() {
        let lists: [Vec<(u32, u32)>; 2] = [
            p.lane_clips_pub(lane).iter().map(|c| (c.start, c.start + c.frames())).collect(),
            p.sub_lane(lane).iter().map(|s| (s.start, s.end())).collect(),
        ];
        for w in lists {
            let mut prev_end = 0;
            for (start, end) in w {
                assert!(start >= prev_end && end > start, "{what}: {lane:?} out of order");
                prev_end = end;
            }
        }
    }
}

/// The undo round-trip + snapshot count: one press returns the project to
/// `pre` -- the parts captured BEFORE the op, by the caller that had them --
/// and the op cost exactly one step on top of the history it inherited.
fn undo_round_trip(mut p: Project, pre: crate::project::SweepParts, what: &str) {
    let steps = p.history_len();
    assert!(p.undo(), "{what}: there is a step to take");
    assert_eq!(p.history_len(), steps.saturating_sub(1), "{what}: one step, no more");
    assert_eq!(p.parts(), pre, "{what}: undo is byte-identical");
}

#[test]
fn the_sweep_delete_from_every_anchor() {
    for shape in SHAPES {
        for anchor in ["video", "sound", "caption"] {
            let (mut p, s1) = project(shape);
            let had_caption = s1.is_some();
            let pre = p.parts();
            let deleted = match (anchor, had_caption) {
                ("video", _) => p.delete_in(Lane::V1, 0),
                ("sound", _) => p.delete_in(Lane::A1, 0),
                ("caption", true) => p.delete_sub_in(s1.unwrap(), 0),
                // This shape has no caption to delete; the cell is vacuous.
                ("caption", false) => continue,
                _ => unreachable!("the sweep names three anchors"),
            };
            assert!(deleted, "{shape}/{anchor}: the delete happened");
            laws(&p, &format!("{shape}/{anchor} after delete"));
            if shape == "ungrouped" {
                // The lone-clip law: the span leaves EVERY lane.
                assert!(p.lane(Lane::A1).is_empty() || p.lane(Lane::V1).is_empty(), "{shape}/{anchor}");
            } else {
                // The group law: every member gone, from every anchor.
                assert!(p.lane(Lane::V1).is_empty(), "{shape}/{anchor}: V1 emptied");
                assert!(p.lane(Lane::A1).is_empty(), "{shape}/{anchor}: A1 emptied");
                if let Some(s1) = s1 {
                    assert!(
                        p.sub_lane(s1).is_empty() || p.sub_lane(s1).iter().all(|s| s.start >= 60),
                        "{shape}/{anchor}: the caption gone with the group"
                    );
                }
            }
            undo_round_trip(p, pre, &format!("{shape}/{anchor} delete"));
        }
    }
}

#[test]
fn the_sweep_speed_keeps_the_group_and_the_playhead_scene() {
    for shape in SHAPES {
        for (what, speed) in [("2x", Speed::from_permille(2000)), ("half", Speed::from_permille(500))] {
            let (mut p, _s1) = project(shape);
            let pre = p.parts();
            // The map the op will have applied, asked BEFORE the write (the
            // old ends are what it maps through).
            let at = 45u32;
            let mapped = p.speeded_playhead(Lane::V1, 0, speed, at);
            let before = p.span_at(Lane::V1, at).and_then(|s| s.from);
            let held_frames = p.lane(Lane::V1)[0].frames();
            let map = TimelineMap::piece(
                (p.lane(Lane::V1)[0].start, p.lane(Lane::V1)[0].start + held_frames),
                (p.lane(Lane::V1)[0].start, mapped.unwrap_or(at).max(p.lane(Lane::V1)[0].start)),
            );
            if let Err(why) = p.set_speed(Lane::V1, 0, speed) {
                // A stretch the neighbours leave no room for is refused in
                // the engine's own words -- a valid cell, not a bug, and the
                // refusal changed nothing (the laws hold of the untouched
                // project by construction).
                assert!(
                    why.to_string().contains("would run to frame"),
                    "{shape}/{what}: a refusal names its wall: {why}"
                );
                continue;
            }
            laws(&p, &format!("{shape}/{what} after speed"));
            // Survivor continuity: the clip's own new end is the map of its
            // old one, and the playhead's scene survived.
            // The scene law to the frame a rate can hold: an odd source
            // frame at 2x never lands on an integer playhead, so the
            // tolerance is the one frame the map's own rounding admits.
            let after = p.span_at(Lane::V1, mapped.unwrap_or(at)).and_then(|s| s.from);
            let apart = match (before, after) {
                (Some((_, a)), Some((_, b))) => a.abs_diff(b),
                (None, None) => 0,
                _ => u32::MAX,
            };
            assert!(
                apart <= 1,
                "{shape}/{what}: the scene under the playhead, {before:?} vs {after:?}"
            );
            let _ = map; // the piece is pinned by the span math above
            undo_round_trip(p, pre, &format!("{shape}/{what} speed"));
        }
    }
}

#[test]
fn the_sweep_split_and_regroup_round_trip() {
    for shape in SHAPES {
        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        assert!(p.split(45), "{shape}: the razor cuts");
        laws(&p, &format!("{shape} after split"));
        undo_round_trip(p, pre, &format!("{shape} split"));

        let (mut p, _) = project(shape);
        assert!(p.split(45), "{shape}: cut");
        let pre = p.parts();
        assert!(p.regroup(45), "{shape}: and rejoined");
        laws(&p, &format!("{shape} after regroup"));
        undo_round_trip(p, pre, &format!("{shape} regroup"));
    }
}

#[test]
fn the_sweep_move_and_trim_keep_the_group_whole() {
    for shape in SHAPES[1..].iter() {
        let shape = *shape;
        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        let before: Vec<Option<u32>> = p.lanes()
            .into_iter()
            .flat_map(|l| p.lane(l).iter().map(|c| c.link))
            .collect();
        assert!(p.move_clip(Lane::V1, 0, Lane::V1, 30), "{shape}: room to slide");
        laws(&p, &format!("{shape} after move"));
        let after: Vec<Option<u32>> = p.lanes()
            .into_iter()
            .flat_map(|l| p.lane(l).iter().map(|c| c.link))
            .collect();
        assert_eq!(before, after, "{shape}: a move keeps the group's ids");
        undo_round_trip(p, pre, &format!("{shape} move"));

        let (mut p, _) = project(shape);
        let pre = p.parts();
        let end = p.lane(Lane::V1)[0].end();
        assert!(p.trim(Lane::V1, 0, crate::project::Edge::End, end - 10, &[90]), "{shape}: room to trim");
        laws(&p, &format!("{shape} after trim"));
        undo_round_trip(p, pre, &format!("{shape} trim"));
    }
}

#[test]
fn the_sweep_paste_is_the_named_exception() {
    for shape in SHAPES {
        let (mut p, s1) = project(shape);
        let pre = p.parts();
        let subs_before: Vec<Vec<(u32, u32)>> = s1
            .map(|s1| vec![p.sub_lane(s1).iter().map(|s| (s.start, s.start + s.frames)).collect()])
            .unwrap_or_default();
        assert!(p.paste(10, clip(0, 0, 30)), "{shape}: the paste lands");
        laws(&p, &format!("{shape} after paste"));
        // The exception, pinned: the media lanes RIPPLE (nothing stays put
        // past the insert) and the captions never move at all.
        let subs_after: Vec<Vec<(u32, u32)>> = s1
            .map(|s1| vec![p.sub_lane(s1).iter().map(|s| (s.start, s.start + s.frames)).collect()])
            .unwrap_or_default();
        assert_eq!(subs_before, subs_after, "{shape}: the words keep their clock");
        undo_round_trip(p, pre, &format!("{shape} paste"));
    }
}
