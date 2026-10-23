//! A timeline's rate changed and changed back lands every edit where it was.
//!
//! The edit is cut on a 30 fps timeline with frames that do not sit on a 25 or
//! a 24 fps grid (a dissolve of 15 frames, a fade of 7, a cut at frame 63 and
//! at frame 111), and a title. Going 30 -> 25 -> 30 (or 30 -> 24 -> 30) must
//! hand back every clip's start, length, in-point, dissolve and fade, and every
//! title's window and fades, within one frame. Exact is what the grid allows:
//! a frame of 25 fps is 1.2 frames of 30, so a boundary that falls between two
//! of them cannot come back exactly, and rounding to nearest keeps it to the one
//! frame that is the most a round trip may lose.
//!
//! ```text
//! cargo test -p engine --test fps_round_trip
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::project::{Lane, Title};
use engine::text::TextStyle;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// One lane's clips, as the numbers a round trip has to restore.
type LaneClips = Vec<(u32, u32, u32, u32, u32, u32, u32)>;

/// The edit, as the numbers a round trip has to restore.
#[derive(Debug, PartialEq, Clone)]
struct Shape {
    lanes: Vec<(Lane, LaneClips)>,
    titles: Vec<(u32, u32, u32, u32)>,
}

fn shape(s: &PlaybackSession) -> Shape {
    let lanes = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind != engine::project::LaneKind::Subtitle)
        .map(|lane| {
            let clips = s
                .lane_clips(lane)
                .iter()
                .map(|c| {
                    (
                        c.start,
                        c.frames(),
                        c.in_frame,
                        c.transition_out,
                        c.fade_in,
                        c.fade_out,
                        c.out_frame,
                    )
                })
                .collect();
            (lane, clips)
        })
        .collect();
    let titles = s
        .titles()
        .iter()
        .map(|t: &Title| (t.start, t.frames, t.fade_in, t.fade_out))
        .collect();
    Shape { lanes, titles }
}

/// A 30 fps edit with the awkward frames in it.
fn edit() -> PlaybackSession {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    assert!(s.cut_at(0.5), "cut at frame 15");
    assert!(s.cut_at(2.1), "cut at frame 63");
    assert!(s.cut_at(3.7), "cut at frame 111");
    assert!(s.set_transition_out(Lane::V1, 0, 15), "a 15-frame dissolve");
    assert!(s.set_fade_in(Lane::V1, 1, 7), "a 7-frame fade in");
    assert!(s.set_fade_out(Lane::A1, 2, 9), "a 9-frame fade out");
    s.add_title(Title {
        start: 45,
        frames: 50,
        text: "T".into(),
        style: TextStyle::default(),
        fade_in: 9,
        fade_out: 6,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    })
    .expect("title");
    s
}

/// Every number of `a` is within `tol` frames of the matching one of `b`.
fn within(a: &Shape, b: &Shape, tol: u32) -> Result<(), String> {
    if a.lanes.len() != b.lanes.len() || a.titles.len() != b.titles.len() {
        return Err(format!("shape changed: {a:?} vs {b:?}"));
    }
    for ((la, ca), (lb, cb)) in a.lanes.iter().zip(&b.lanes) {
        if la != lb || ca.len() != cb.len() {
            return Err(format!("lane {la:?} changed: {ca:?} vs {cb:?}"));
        }
        for (x, y) in ca.iter().zip(cb) {
            let d = [
                x.0.abs_diff(y.0),
                x.1.abs_diff(y.1),
                x.2.abs_diff(y.2),
                x.3.abs_diff(y.3),
                x.4.abs_diff(y.4),
                x.5.abs_diff(y.5),
            ];
            if d.iter().any(|&d| d > tol) {
                return Err(format!("{la:?} clip {x:?} came back {y:?}"));
            }
        }
    }
    for (x, y) in a.titles.iter().zip(&b.titles) {
        let d = [
            x.0.abs_diff(y.0),
            x.1.abs_diff(y.1),
            x.2.abs_diff(y.2),
            x.3.abs_diff(y.3),
        ];
        if d.iter().any(|&d| d > tol) {
            return Err(format!("title {x:?} came back {y:?}"));
        }
    }
    Ok(())
}

fn round_trip(via: f64) -> (Shape, Shape) {
    let mut s = edit();
    let before = shape(&s);
    assert!(s.set_frame_rate(via), "to {via}");
    assert!(s.set_frame_rate(30.0), "back to 30");
    (before, shape(&s))
}

#[test]
fn a_30_25_30_round_trip_lands_within_a_frame() {
    let (before, after) = round_trip(25.0);
    within(&before, &after, 1).expect("within one frame");
}

#[test]
fn a_30_24_30_round_trip_lands_within_a_frame() {
    let (before, after) = round_trip(24.0);
    within(&before, &after, 1).expect("within one frame");
}

#[test]
fn audio_clips_keep_their_source_length_through_a_round_trip() {
    let (before, after) = round_trip(25.0);
    for (lane, clips) in &before.lanes {
        if lane.kind != engine::project::LaneKind::Audio {
            continue;
        }
        let back = after.lanes.iter().find(|(l, _)| l == lane).expect("lane");
        for (x, y) in clips.iter().zip(&back.1) {
            assert!(
                x.1.abs_diff(y.1) <= 1,
                "audio clip {x:?} came back {y:?}: more than a frame"
            );
        }
    }
}

/// An edit whose every frame sits on the 25 and 24 fps grids too: cuts at whole
/// seconds, and dissolves, fades and a title in multiples of 30 frames (which
/// both 25 and 24 divide into whole frames, every 6 or 5 of them).
fn on_grid_edit() -> PlaybackSession {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    assert!(s.cut_at(1.0), "cut at frame 30");
    assert!(s.cut_at(3.0), "cut at frame 90");
    assert!(s.cut_at(4.0), "cut at frame 120");
    assert!(s.set_transition_out(Lane::V1, 0, 30), "a 30-frame dissolve");
    assert!(s.set_fade_in(Lane::V1, 1, 30), "a 30-frame fade in");
    assert!(s.set_fade_out(Lane::A1, 2, 30), "a 30-frame fade out");
    s.add_title(Title {
        start: 60,
        frames: 60,
        text: "T".into(),
        style: TextStyle::default(),
        fade_in: 30,
        fade_out: 30,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    })
    .expect("title");
    s
}

#[test]
fn an_on_grid_edit_comes_back_exactly_through_25_and_24() {
    for via in [25.0, 24.0] {
        let mut s = on_grid_edit();
        let before = shape(&s);
        assert!(s.set_frame_rate(via));
        assert!(s.set_frame_rate(30.0));
        let after = shape(&s);
        assert_eq!(before, after, "30 -> {via} -> 30 is exact on the grid");
    }
}

#[test]
fn a_second_round_trip_does_not_drift() {
    let mut s = edit();
    assert!(s.set_frame_rate(25.0));
    assert!(s.set_frame_rate(30.0));
    let once = shape(&s);
    assert!(s.set_frame_rate(25.0));
    assert!(s.set_frame_rate(30.0));
    assert_eq!(shape(&s), once, "a second 30 -> 25 -> 30 lands where the first did");
}
