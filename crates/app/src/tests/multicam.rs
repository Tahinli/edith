//! Multicam on the window's side: the oracle's rules, the number keys, the angle monitor's place
//! in the viewer (a flex sibling of the program picture, never over the timeline), and the
//! timeline's angle label.

use super::*;
use crate::oracle::{Ctx, Enable, enable};
use keymap::ActionId;

const ANGLE_KEYS: [ActionId; 9] = [
    ActionId::Angle1,
    ActionId::Angle2,
    ActionId::Angle3,
    ActionId::Angle4,
    ActionId::Angle5,
    ActionId::Angle6,
    ActionId::Angle7,
    ActionId::Angle8,
    ActionId::Angle9,
];

#[test]
fn the_digits_are_the_angle_keys() {
    let keymap = keymap::Keymap::defaults();
    for (i, a) in ANGLE_KEYS.iter().enumerate() {
        assert_eq!(a.angle_number(), Some(i + 1));
        assert_eq!(keymap.lookup(&(i + 1).to_string(), false, false, false), Some(*a), "key {}", i + 1);
    }
    assert_eq!(ActionId::Cut.angle_number(), None);
}

#[test]
fn creating_needs_two_picks_and_flattening_a_multicam_clip() {
    let mut ctx = Ctx { timeline: true, ..Ctx::default() };
    assert!(matches!(enable(ActionId::CreateMulticam, ctx.clone()), Enable::No(_)));
    ctx.media_picks = 1;
    assert!(matches!(enable(ActionId::CreateMulticam, ctx.clone()), Enable::No(_)));
    ctx.media_picks = 2;
    assert!(matches!(enable(ActionId::CreateMulticam, ctx.clone()), Enable::Yes));
    assert!(matches!(enable(ActionId::FlattenMulticam, ctx.clone()), Enable::No(_)));
}

#[test]
fn the_angle_keys_are_silent_outside_the_monitor_and_bounded_by_the_angles() {
    let mut ctx = Ctx { timeline: true, ..Ctx::default() };
    for a in ANGLE_KEYS {
        assert!(matches!(enable(a, ctx.clone()), Enable::Hidden(_)), "{a:?} monitor off");
    }
    ctx.monitor = true;
    ctx.angles_here = 2;
    assert!(matches!(enable(ActionId::Angle2, ctx.clone()), Enable::Yes));
    assert!(matches!(enable(ActionId::Angle3, ctx.clone()), Enable::No(_)));
    ctx.angles_here = 0;
    assert!(matches!(enable(ActionId::Angle1, ctx.clone()), Enable::Hidden(_)));
}

/// The program picture is always drawn at its own geometry; the monitor sits over the free strip
/// beside it, and only a strip narrower than the minimum makes it a sibling that shares the row.
/// Nothing floats over the timeline.
#[test]
fn the_angle_monitor_keeps_the_program_picture_in_place() {
    let stance = src_text("ui/stance.rs");
    let at = stance.find(".id(\"stance-picture\")").expect("the picture row");
    let row = &stance[at..at + 4000];
    assert!(row.contains(".children(program_and_monitor("), "the picture box holds the program and the monitor");
    let helper = &stance[stance.find("fn program_and_monitor").expect("the helper")..];
    assert!(helper.contains("let program = match"), "the program picture is built first, in every case");
    assert!(helper.contains(".absolute()") && helper.contains("Some(strip) =>"), "beside the picture, the monitor is an overlay on the free strip");
    assert!(stance.contains("const MONITOR_STRIP_MIN: f32 = 200.;"), "a strip under 200 px is the fallback");
    assert!(stance.contains("let strip = monitor_strip(player, window)"), "the strip is read from the picture as drawn");
    assert!(!stance.contains(".children(if player.angle_monitor"), "the monitor is not a sibling beside the picture");
    let monitor = src_text("player/multicam.rs");
    let view = &monitor[monitor.find("fn angle_monitor_view").expect("the monitor")..];
    assert!(!view.contains(".absolute()"), "the monitor takes room and covers nothing");
    assert!(view.contains("No multicam clip under the playhead"), "no multicam clip under the playhead, a calm placeholder");
    assert_eq!(crate::player::multicam::monitor_rows(2), [1, 1]);
    assert_eq!(crate::player::multicam::monitor_rows(3), [2, 1]);
    assert_eq!(crate::player::multicam::monitor_rows(4), [2, 2]);
    assert_eq!(crate::player::multicam::monitor_rows(5), [3, 2]);
    // Beside the picture the strip holds two across.
    assert_eq!(crate::player::multicam::monitor_rows_within(4, 2), [2, 2]);
    assert_eq!(crate::player::multicam::monitor_rows_within(9, 2), [2, 2, 2, 2, 1]);
}

#[test]
fn the_timeline_names_the_angle_each_segment_shows() {
    let clip = src_text("ui/bench_stance/clip.rs");
    assert!(clip.contains("multicam_at(lane, idx)") && clip.contains("clip.angle.video"));
}

#[test]
fn the_view_menu_has_the_monitor_switch_and_the_clip_menu_the_create_row() {
    let bar = src_text("ui/menubar.rs");
    assert!(bar.contains("Switch(ActionId::ToggleAngleMonitor)"));
    assert!(bar.contains("Act(ActionId::CreateMulticam)"));
    assert!(bar.contains("Act(ActionId::FlattenMulticam)"));
}

#[test]
fn the_live_angle_wears_the_accent_ring_without_changing_the_tile() {
    let monitor = src_text("player/multicam.rs");
    assert!(monitor.contains(".border_2()") && monitor.contains("ACCENT_PRIMARY()"), "a 2px border on every tile, accent on the live one");
    assert!(monitor.contains("Syncing angles"), "the sync reports its percent in the status bar");
    assert!(monitor.contains("std::thread::spawn"), "the sync runs off the window's thread");
}

/// A Ctrl-click on a source row adds it to the picks, a plain click picks it alone, and a double
/// click opens the source instead of picking.
#[test]
fn a_ctrl_click_on_a_source_row_adds_to_the_picks() {
    use gpui::Modifiers;
    use crate::ui::dock_stance::click_adds_to_picks;
    let ctrl = Modifiers { control: true, ..Default::default() };
    let shift = Modifiers { shift: true, ..Default::default() };
    assert!(click_adds_to_picks(ctrl, 1), "Ctrl-click adds");
    assert!(click_adds_to_picks(shift, 1), "Shift-click adds too (no range)");
    assert!(!click_adds_to_picks(Modifiers::default(), 1), "a plain click picks that row alone");
    assert!(!click_adds_to_picks(ctrl, 2), "a double click opens the source");
}

/// Two Ctrl-clicks after a plain pick leave two picks; a third Ctrl-click on one of them takes it
/// back out. The first Ctrl-click takes the row that was selected along with it.
#[test]
fn ctrl_clicks_build_two_picks_and_a_repeat_removes_one() {
    use crate::player::multicam::toggle_pick;
    use std::path::PathBuf;
    let a = (PathBuf::from("/m/a.mp4"), 0);
    let b = (PathBuf::from("/m/b.mp4"), 0);
    let mut picks = Vec::new();
    toggle_pick(&mut picks, Some(a.clone()), b.clone());
    assert_eq!(picks, vec![a.clone(), b.clone()], "two picks");
    toggle_pick(&mut picks, Some(b.clone()), a.clone());
    assert_eq!(picks, vec![b.clone()], "a Ctrl-click on a pick removes it");
}
