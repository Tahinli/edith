//! Track heights: the three sizes a lane can be set to, the menus and keys that set them, and
//! the y-to-lane arithmetic a column of mixed heights has to get right.

use super::*;
use crate::ui::bench_stance::{
    LANE_MAX_H, LANE_MIN_H, TrackSize, lane_at_y, lane_bands, set_track_size, size_h, snap_lane_h,
};
use crate::ui::menubar::Title;
use keymap::ActionId;

/// Small is the floor, Medium is the kind's own default, Large is taller than Medium and within
/// the ceiling a dragged lane may reach; the subtitle lane's Small and Medium are one height.
#[test]
fn the_three_sizes_order_by_height_within_the_lane_range() {
    for kind in [LaneKind::Video, LaneKind::Audio, LaneKind::Subtitle] {
        let (small, medium, large) = (
            size_h(kind, TrackSize::Small),
            size_h(kind, TrackSize::Medium),
            size_h(kind, TrackSize::Large),
        );
        assert_eq!(small, LANE_MIN_H, "{kind:?}");
        assert!(small <= medium && medium < large, "{kind:?}: {small} {medium} {large}");
        assert!(large <= LANE_MAX_H, "{kind:?}: {large}");
    }
    assert_eq!(size_h(LaneKind::Video, TrackSize::Medium), 52.);
    assert_eq!(size_h(LaneKind::Audio, TrackSize::Medium), 40.);
}

/// Steps move one size at a time and hold at the ends.
#[test]
fn a_step_moves_one_size_and_holds_at_the_ends() {
    use TrackSize::{Large, Medium, Small};
    assert_eq!(Small.step(true), Medium);
    assert_eq!(Medium.step(true), Large);
    assert_eq!(Large.step(true), Large);
    assert_eq!(Large.step(false), Medium);
    assert_eq!(Medium.step(false), Small);
    assert_eq!(Small.step(false), Small);
    assert_eq!([Small, Medium, Large].map(TrackSize::word), ["Small", "Medium", "Large"]);
}

/// A size written to a lane saves its height; Medium forgets it, so the lane goes back to following
/// the window. Other lanes are not touched.
#[test]
fn a_size_sets_its_own_lane_and_medium_forgets_it() {
    let mut saved = Vec::new();
    set_track_size(&mut saved, Lane::V1, TrackSize::Large);
    set_track_size(&mut saved, Lane::A1, TrackSize::Small);
    assert_eq!(saved, vec![("v1".to_string(), 96.), ("a1".to_string(), LANE_MIN_H)]);
    set_track_size(&mut saved, Lane::V1, TrackSize::Medium);
    assert_eq!(saved, vec![("a1".to_string(), LANE_MIN_H)]);
    set_track_size(&mut saved, Lane::A1, TrackSize::Large);
    assert_eq!(saved, vec![("a1".to_string(), 72.)], "a key is replaced, not repeated");
}

/// A drag that lands within a few pixels of a size lands on it; one that does not keeps its height.
#[test]
fn a_dragged_height_snaps_to_a_size_it_is_next_to() {
    assert_eq!(snap_lane_h(LaneKind::Video, 50.), 52.);
    assert_eq!(snap_lane_h(LaneKind::Video, 56.), 52.);
    assert_eq!(snap_lane_h(LaneKind::Video, 70.), 70.);
    assert_eq!(snap_lane_h(LaneKind::Audio, 74.), 72.);
    assert_eq!(snap_lane_h(LaneKind::Audio, LANE_MIN_H + 1.), LANE_MIN_H);
}

/// A mixed stack (video 96, audio 40, a small audio 28, video 52, rows apart by the 2px gap): each
/// y belongs to the lane whose own band holds it, and the gap between two lanes belongs to neither.
#[test]
fn a_y_lands_on_the_lane_whose_own_height_holds_it_in_a_mixed_stack() {
    let v1 = Lane::V1;
    let a1 = Lane::A1;
    let a2 = Lane::new(LaneKind::Audio, 1);
    let v2 = Lane::new(LaneKind::Video, 1);
    let bands = lane_bands(&[(v1, 96.), (a1, 40.), (a2, 28.), (v2, 52.)]);
    assert_eq!(bands[1], (a1, 98., 138.));
    assert_eq!(bands[2], (a2, 140., 168.));
    assert_eq!(bands[3], (v2, 170., 222.));
    assert_eq!(lane_at_y(&bands, 0.), Some(v1));
    assert_eq!(lane_at_y(&bands, 95.9), Some(v1));
    assert_eq!(lane_at_y(&bands, 97.), None, "the gap under V1 is no lane");
    assert_eq!(lane_at_y(&bands, 98.), Some(a1));
    assert_eq!(lane_at_y(&bands, 137.), Some(a1));
    assert_eq!(lane_at_y(&bands, 139.), None, "the gap under A1 is no lane");
    assert_eq!(lane_at_y(&bands, 141.), Some(a2));
    assert_eq!(lane_at_y(&bands, 167.9), Some(a2));
    assert_eq!(lane_at_y(&bands, 170.), Some(v2));
    assert_eq!(lane_at_y(&bands, 222.), None, "below the last lane");
    assert_eq!(lane_at_y(&bands, -1.), None, "above the first lane");
}

/// The header's menu offers the three heights as rows, before the rule that leads to the removes,
/// and they are enabled on every kind of lane.
#[test]
fn the_head_menu_offers_three_heights_before_the_removes() {
    for lane in [Lane::V1, Lane::A1, Lane::S1] {
        let items = lane_items(lane);
        let remove = items.iter().position(|&a| destructive(a)).unwrap_or(items.len());
        for size in [ActionId::TrackSmall, ActionId::TrackMedium, ActionId::TrackLarge] {
            let at = items.iter().position(|&a| a == size).expect("a height row");
            assert!(at < remove, "{lane:?}: {size:?} before the removes");
            assert!(matches!(enable_lane(size, lane, Ctx::default()), Enable::Yes));
        }
    }
}

/// The Timeline menu carries the three all-track rows and the two steps; the keys for the steps
/// are the ones the keymap binds, and the labels are the words a person reads.
#[test]
fn the_timeline_menu_and_keys_set_every_track_height() {
    let timeline: Vec<ActionId> = Title::Timeline.entries().into_iter().filter_map(|e| e.action()).collect();
    for action in [
        ActionId::TrackSmall,
        ActionId::TrackMedium,
        ActionId::TrackLarge,
        ActionId::TracksSmaller,
        ActionId::TracksLarger,
    ] {
        assert!(timeline.contains(&action), "{action:?} in the Timeline menu");
    }
    assert_eq!(ActionId::TrackSmall.label(), "All tracks: Small");
    assert_eq!(ActionId::TracksSmaller.label(), "Make tracks smaller");
    assert_eq!(ActionId::TracksLarger.label(), "Make tracks larger");
    let keys = keymap::Keymap::defaults();
    assert_eq!(keys.display(ActionId::TracksLarger), "Alt+=");
    assert_eq!(keys.display(ActionId::TracksSmaller), "Alt+-");
}
