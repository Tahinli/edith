//! Regressions from the UX bug batch: the export range keys, the seam click, scrubs released
//! outside the window, the zoomed picture's rectangle, plain menu words, the New project dialog.

use crate::keymap::ActionId;
use crate::player::project_dialogs::project_file_name;
use crate::viewer_view::{Zoom, picture_rect};
use gpui::{Pixels, Point, point, px};

/// The Export tab is up while the range is marked: I and O must still reach the timeline.
#[test]
fn the_export_tab_lets_the_range_marks_through() {
    let km = crate::keymap::Keymap::defaults();
    for key in ["i", "o"] {
        let action = km.lookup(key, false, false, false).expect("a mark key");
        assert!(crate::ui::stance::export_tab_action(action), "{key} is swallowed by the Export tab");
    }
    assert!(crate::ui::stance::export_tab_action(ActionId::ClearRange));
    // Editing strokes stay the room's, not the tab's.
    assert!(!crate::ui::stance::export_tab_action(ActionId::Delete));
}

/// A press on a seam that never moves is a click: it changes nothing and saves nothing.
#[test]
fn a_seam_click_without_travel_is_not_a_drag() {
    use crate::player::timeline_edit::split_travelled;
    let at = |x: f32, y: f32| -> Point<Pixels> { point(px(x), px(y)) };
    assert!(!split_travelled(at(500., 400.), at(500., 400.)));
    assert!(!split_travelled(at(500., 400.), at(501., 402.)));
    assert!(split_travelled(at(500., 400.), at(500., 410.)));
}

/// A number being scrubbed is committed when the pointer leaves the window, because its
/// release is never delivered there.
#[test]
fn leaving_the_window_commits_the_scrubs() {
    let src = include_str!("../player/timeline_edit.rs");
    let at = src.find("pub(crate) fn drag_left_window").expect("the leave handler");
    let body = &src[at..at + src[at..].find("\n    }\n").unwrap()];
    assert!(body.contains("pan_scrub_end"), "a pan scrub dropped on leave");
    assert!(body.contains("title_scrub_end"), "a title scrub dropped on leave");
}

/// Titles and captions are placed against the picture as drawn: zoomed it is the zoomed rectangle.
#[test]
fn the_zoomed_picture_has_its_own_rectangle() {
    let native = (1920., 1080.);
    let bx = (800., 450.);
    // Fit: the frame letterboxed into the box, centred.
    assert_eq!(picture_rect(Zoom::Fit, native, bx, (0., 0.), (0., 0.), 1.), (0., 0., 800., 450.));
    // 50%: 960x540 in an 800x450 box, centred, so it overhangs by 80 and 45 on each side.
    assert_eq!(picture_rect(Zoom::P50, native, bx, (0., 0.), (0., 0.), 1.), (-80., -45., 960., 540.));
    // Panned: moved by the pan, held inside the overhang.
    assert_eq!(picture_rect(Zoom::P50, native, bx, (0., 0.), (30., 0.), 1.), (-50., -45., 960., 540.));
    assert_eq!(picture_rect(Zoom::P50, native, bx, (0., 0.), (500., 0.), 1.).0, 0.);
    // 25%: smaller than the box, centred.
    assert_eq!(picture_rect(Zoom::P25, native, bx, (0., 0.), (0., 0.), 1.), (160., 90., 480., 270.));
    // Neither the titles nor the captions are dropped while zoomed any more.
    let render = include_str!("../render.rs");
    assert!(!render.contains("== crate::viewer_view::Zoom::Fit))"), "an overlay is hidden while zoomed");
}

/// The clip menu reads plainly, and Trim out has its icon like Trim in.
#[test]
fn the_clip_menu_words_are_plain_and_trim_out_has_an_icon() {
    assert_eq!(ActionId::Detach.label(), "Unlink audio and video");
    assert_eq!(ActionId::Regroup.label(), "Join clips back");
    assert!(crate::menus::note(ActionId::Detach).is_some(), "Unlink says what it does on hover");
    assert!(crate::menus::icon(ActionId::TrimOut).is_some());
    assert!(crate::menus::icon(ActionId::TrimIn).is_some());
    for action in crate::menus::MENU_ITEMS {
        let label = action.label();
        assert!(!label.contains("group") && !label.contains("Group") || action == ActionId::Group, "{label}");
    }
}

#[test]
fn a_typed_project_name_makes_a_safe_file_name() {
    assert_eq!(project_file_name("Trip"), Some("Trip.edith".into()));
    assert_eq!(project_file_name("  Trip.edith "), Some("Trip.edith".into()));
    assert_eq!(project_file_name("a/b"), Some("a-b.edith".into()));
    assert_eq!(project_file_name("  "), None);
    assert_eq!(project_file_name(".."), None);
}
