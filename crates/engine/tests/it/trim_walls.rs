//! Which wall a trim is stopped by, named the way it binds: a neighbour clip
//! that meets the source's own limit on the same frame is the neighbour, and
//! the source is named only when no neighbour is in the way.
//!
//! ```text
//! cargo test -p engine --test trim_walls
//! ```

use std::path::PathBuf;

use engine::project::{Clip, Edge, Lane, Speed, WallBound};
use engine::scale::FitPolicy;
use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// `test_av.mp4` is 150 frames on its 30 fps timeline: one clip, V1 0..150.
fn session() -> PlaybackSession {
    let s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    s.set_gain(0.0);
    s
}

fn clip(source: usize, in_frame: u32, out_frame: u32) -> Clip {
    Clip {
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        start: 0,
        in_frame,
        out_frame,
        source,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    }
}

#[test]
fn a_neighbour_on_the_source_limit_is_named_as_the_neighbour() {
    let mut s = session();
    // The source's last frame is frame 150 of the timeline, and that is where
    // the next clip starts: the two walls coincide.
    let end = s.timeline_duration();
    assert!(s.place_at(Lane::V1, end, clip(0, 0, 30)), "a clip after the first");
    assert_eq!(s.lane_clips(Lane::V1).len(), 2);

    // The first clip's out edge: its source would run to 150, and the clip
    // after it starts at 150. The neighbour is the wall that is named.
    let (_, hi) = s.trim_bounds(Lane::V1, 0, Edge::End).expect("bounds");
    assert_eq!(hi, WallBound::Neighbour(150), "a tie names the neighbour");
    assert_eq!(s.trim_room(Lane::V1, 0, Edge::End).expect("room").1, 150);

    // The second clip's in edge: its head would run back to 0 of the source,
    // which is 150 on the timeline, and the first clip ends there.
    let (lo, _) = s.trim_bounds(Lane::V1, 1, Edge::Start).expect("bounds");
    assert_eq!(lo, WallBound::Neighbour(150), "a tie names the neighbour");
}

#[test]
fn the_source_is_named_when_no_neighbour_is_in_the_way() {
    let mut s = session();
    let end = s.timeline_duration();
    assert!(s.place_at(Lane::V1, end, clip(0, 0, 30)));
    // The last clip's out edge has no neighbour: its source is the wall.
    let (_, hi) = s.trim_bounds(Lane::V1, 1, Edge::End).expect("bounds");
    assert_eq!(hi, WallBound::Source);
}

#[test]
fn a_neighbour_inside_the_source_is_named_as_the_neighbour() {
    let mut s = session();
    // Cut at 4 s: the first clip's source runs on to 150, past the cut at 120.
    assert!(s.cut_at(4.0));
    let (_, hi) = s.trim_bounds(Lane::V1, 0, Edge::End).expect("bounds");
    assert_eq!(hi, WallBound::Neighbour(120), "the neighbour binds first");
}
