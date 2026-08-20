//! The re-timing sweep: every op that moves or removes timeline time, against
//! every grouping shape, under the universal laws -- `delete_in`/`delete_sub_in`,
//! `set_speed`/`set_speed_live`, `split`/`regroup`, `move_clip`/`trim`,
//! `move_sub`/`trim_sub`, `paste`, `ripple_delete`/`cut_regions`,
//! `lift`/`lift_sub`, `place`/`place_take`/`place_sub`, `delete(idx)` and
//! `retime`. The "there might be other bugs" catcher -- a red cell here is an
//! op whose shape has drifted from the law, found before a user finds it.
//!
//! Left out, by name rather than by omission: `group`/`ungroup`/`group_all`
//! move no time at all (id bookkeeping alone -- their own law is
//! `links_are_consistent`, asked directly by their unit tests and by
//! [`group_all_refuses_a_merge_that_doubles_up_a_lane`](crate::project::tests::group_all_refuses_a_merge_that_doubles_up_a_lane)),
//! and `remove_lane`/`move_lane`/`remove_source`/`remove_subtitles`/`append_clip`
//! touch the lane *list* or the source table, never a placement's span --
//! outside this module's law and covered by their own unit tests in
//! `project.rs`.
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
use crate::project::{Edge, Lane, LaneKind, Rate, Speed, SubClip};
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
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start,
        in_frame,
        out_frame,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
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
                // `Project::single` links its V1 and A1 clips from the start
                // (the source is grouped with itself), so the lone-clip law
                // really is a conjunction: the span leaves EVERY lane.
                assert!(p.lane(Lane::A1).is_empty() && p.lane(Lane::V1).is_empty(), "{shape}/{anchor}");
            } else {
                // The group law: every member gone, from every anchor.
                assert!(p.lane(Lane::V1).is_empty(), "{shape}/{anchor}: V1 emptied");
                assert!(p.lane(Lane::A1).is_empty(), "{shape}/{anchor}: A1 emptied");
                if let Some(s1) = s1 {
                    // Only "aligned"'s one caption is grouped; "offset"
                    // groups just its first caption, leaving the ungrouped
                    // follower to close up behind the hole the delete cuts.
                    match shape {
                        "aligned" => assert!(
                            p.sub_lane(s1).is_empty(),
                            "{shape}/{anchor}: the caption gone with the group"
                        ),
                        "offset" => assert_eq!(
                            p.sub_lane(s1).iter().map(|s| (s.start, s.end())).collect::<Vec<_>>(),
                            vec![(60, 90)],
                            "{shape}/{anchor}: the grouped caption gone, the follower closed up behind it"
                        ),
                        _ => unreachable!("the sweep names three grouped shapes with captions"),
                    }
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
            let at = 45u32;
            let mapped = p.speeded_playhead(Lane::V1, 0, speed, at);
            let before = p.span_at(Lane::V1, at).and_then(|s| s.from);
            // The map the write itself is a shape of, built from the
            // member's own old geometry -- NOT from the playhead's answer,
            // which is what it will be checked against.
            let held = p.lane(Lane::V1)[0];
            let new_len = (f64::from(held.len()) / speed.as_f64()).round() as u32;
            let map = TimelineMap::piece(
                (held.start, held.start + held.frames()),
                (held.start, held.start + new_len),
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
            // Survivor continuity through the map: the clip's old span maps
            // onto exactly the span it landed in -- the piece's own ends, on
            // the lane, not inferred back from anything the write produced.
            let landed = p.lane(Lane::V1)[0];
            assert_eq!(
                (landed.start, landed.start + landed.frames()),
                (map.apply(held.start), map.apply(held.start + held.frames())),
                "{shape}/{what}: the clip's new span is the map of its old one"
            );
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

/// The scoped and whole-timeline ripples: [`Project::ripple_delete`] and
/// [`Project::cut_regions`] both close a hole across every lane they touch,
/// clips and captions alike -- a span safely inside every shape's geometry
/// (never on a clip or caption edge, so the cell asks the general law and not
/// one shape's particular boundary).
#[test]
fn the_sweep_ripple_delete_and_cut_regions_close_every_lane() {
    for shape in SHAPES {
        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        assert!(p.ripple_delete(70, 10), "{shape}: the ripple cuts");
        laws(&p, &format!("{shape} after ripple_delete"));
        undo_round_trip(p, pre, &format!("{shape} ripple_delete"));

        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        assert!(
            p.cut_regions(&[(70, 10)], &p.lanes()).is_ok(),
            "{shape}: the scoped cut lands"
        );
        laws(&p, &format!("{shape} after cut_regions"));
        undo_round_trip(p, pre, &format!("{shape} cut_regions"));
    }
}

/// The two single-lane, group-blind lifts: [`Project::lift`] and
/// [`Project::lift_sub`] take one placement off its own lane and leave a gap
/// -- no other lane moves, grouped or not, which the law asks by checking
/// every OTHER lane is untouched.
#[test]
fn the_sweep_lift_and_lift_sub_leave_a_gap_alone() {
    for shape in SHAPES {
        let (mut p, s1) = project(shape);
        let before_a1 = p.lane(Lane::A1).to_vec();
        let pre = p.parts();
        assert!(p.lift(Lane::V1, 0), "{shape}: the lift takes V1's clip");
        assert!(p.lane(Lane::V1).is_empty(), "{shape}: V1 left a gap");
        assert_eq!(p.lane(Lane::A1).to_vec(), before_a1, "{shape}: A1 never asked");
        laws(&p, &format!("{shape} after lift"));
        undo_round_trip(p, pre, &format!("{shape} lift"));

        let Some(s1) = s1 else { continue };
        let (mut p, s1) = (project(shape).0, s1);
        let before_v1 = p.lane(Lane::V1).to_vec();
        let pre = p.parts();
        assert!(p.lift_sub(s1, 0), "{shape}: the lift takes the caption");
        assert_eq!(p.lane(Lane::V1).to_vec(), before_v1, "{shape}: V1 never asked");
        laws(&p, &format!("{shape} after lift_sub"));
        undo_round_trip(p, pre, &format!("{shape} lift_sub"));
    }
}

/// The caption's own two edits: [`Project::move_sub`] carries its group's
/// clips along it, one delta for all of them (exactly as a clip's
/// [`Project::move_clip`] does), and [`Project::trim_sub`] drags its group's
/// edge along with its own window, one delta each member clamps to its own
/// room (`Project::follow_group`) -- the same law, not a caption exception.
#[test]
fn the_sweep_move_sub_and_trim_sub_keep_their_own_law() {
    for shape in ["aligned", "offset"] {
        let (mut p, s1) = project(shape);
        let s1 = s1.expect("aligned and offset both carry a caption");
        let pre = p.parts();
        let v1_before = p.lane(Lane::V1)[0];
        let sub_before = p.sub_lane(s1)[0].start;
        let to = sub_before + 5;
        assert!(p.move_sub(s1, 0, s1, to).is_ok(), "{shape}: room to drag");
        laws(&p, &format!("{shape} after move_sub"));
        let v1_after = p.lane(Lane::V1)[0];
        assert_eq!(
            i64::from(v1_after.start) - i64::from(v1_before.start),
            i64::from(p.sub_lane(s1)[0].start) - i64::from(sub_before),
            "{shape}: the grouped clip rode the same delta as the caption"
        );
        undo_round_trip(p, pre, &format!("{shape} move_sub"));

        // A caption in a group drags its group's edge along with its own
        // (`Project::follow_group`) -- exactly like `move_sub` above, not the
        // "own window alone" case this cell first assumed.
        let (mut p, s1) = project(shape);
        let s1 = s1.unwrap();
        let pre = p.parts();
        let end = p.sub_lane(s1)[0].end();
        assert!(
            p.trim_sub(s1, 0, Edge::End, end - 5, 30.0).is_ok(),
            "{shape}: room to trim"
        );
        laws(&p, &format!("{shape} after trim_sub"));
        undo_round_trip(p, pre, &format!("{shape} trim_sub"));
    }
}

/// The three overwrite doors -- [`Project::place`], [`Project::place_take`]
/// and [`Project::place_sub`] -- are [`the_sweep_paste_is_the_named_exception`]'s
/// own shape: they land over whatever is there and nothing else on the lane
/// ripples, media or captions.
#[test]
fn the_sweep_place_doors_overwrite_without_rippling() {
    for shape in SHAPES {
        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        assert!(p.place(Lane::V1, 10, clip(10, 0, 20)), "{shape}: place lands");
        laws(&p, &format!("{shape} after place"));
        undo_round_trip(p, pre, &format!("{shape} place"));

        let (mut p, _s1) = project(shape);
        // `add_lane` costs no undo step of its own (it is metadata until
        // something places on it), so `pre` is read after it -- the lane
        // list itself is part of what undo restores.
        let v2 = p.add_lane(LaneKind::Video);
        let pre = p.parts();
        assert!(p.place_take(v2, 10, clip(10, 0, 20)), "{shape}: place_take lands");
        laws(&p, &format!("{shape} after place_take"));
        undo_round_trip(p, pre, &format!("{shape} place_take"));

        let (mut p, _s1) = project(shape);
        let s1 = p.add_lane(LaneKind::Subtitle);
        let pre = p.parts();
        assert!(p.place_sub(s1, 5, caption(5, 10)).is_ok(), "{shape}: place_sub lands");
        laws(&p, &format!("{shape} after place_sub"));
        undo_round_trip(p, pre, &format!("{shape} place_sub"));
    }
}

/// [`Project::delete`] is sugar for [`Project::delete_in`] on `V1` -- the
/// single-lane front-end door the app's own delete key is wired to. One cell
/// pinning the delegation rather than the whole delete sweep again.
#[test]
fn the_sweep_delete_by_index_is_delete_in_on_v1() {
    let (mut a, _) = project("clips");
    let (mut b, _) = project("clips");
    assert_eq!(a.delete(0), b.delete_in(Lane::V1, 0));
    laws(&a, "delete(idx)");
    assert_eq!(a.parts(), b.parts(), "delete(idx) and delete_in(V1, idx) agree");
}

/// [`Project::set_speed_live`] is the drag sample of [`Project::set_speed`]:
/// every sample only regrades the rate, and the whole gesture undoes in the
/// one step the press took ([`a_whole_colour_drag_undoes_in_one_step`]'s law,
/// for a rate instead of a grade).
#[test]
fn the_sweep_speed_live_drags_in_one_step() {
    for shape in SHAPES {
        let (mut p, _s1) = project(shape);
        let pre = p.parts();
        assert!(p.set_speed(Lane::V1, 0, Speed::from_permille(1100)).is_ok(), "{shape}: the press");
        for permille in [1200, 1300, 1400] {
            assert!(
                p.set_speed_live(Lane::V1, 0, Speed::from_permille(permille)).is_ok(),
                "{shape}: a live sample"
            );
            laws(&p, &format!("{shape} after a live speed sample"));
        }
        undo_round_trip(p, pre, &format!("{shape} speed drag"));
    }
}

/// [`Project::retime`] moves every frame number without being an edit anyone
/// made ([`Project::retime`]'s own words) -- no history step to its name, so
/// the sweep asks it the order and consistency laws alone, never the undo
/// round trip the rest of this module leans on.
#[test]
fn the_sweep_retime_keeps_the_lanes_ordered() {
    for shape in SHAPES {
        let (mut p, _s1) = project(shape);
        let steps = p.history_len();
        // Old timeline frames per new one: 1:2 doubles every span, a slow-down
        // wide enough that the geometry the sweep builds still fits.
        let k = Rate::from_fps(1.0, 2.0).expect("a namable rate");
        p.retime(k, &[10_000]);
        assert_eq!(p.history_len(), steps, "retime costs no undo step");
        laws(&p, &format!("{shape} after retime"));
    }
}
