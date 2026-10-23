//! Track names: the head keeps its width, the rename verb is in the head's menu and the
//! double-click and keys are wired.

use super::*;
use crate::ui::bench_stance::{HEAD_W, LANE_DOT_D, LANE_NAME_MAX_W, PATCH_W};
use keymap::ActionId;

/// Every head offers "Rename track…" ahead of its remove row (the rule line draws before the
/// removes), and nothing else in the head's contents can widen it: a label is capped, the head
/// is the fixed column.
#[test]
fn the_head_menu_offers_rename_and_a_long_name_cannot_widen_the_head() {
    for lane in [Lane::V1, Lane::A1, Lane::S1] {
        let items = lane_items(lane);
        let rename = items.iter().position(|&a| a == ActionId::RenameLane).expect("rename row");
        let remove = items.iter().position(|&a| destructive(a)).unwrap_or(items.len());
        assert!(rename < remove, "{lane:?}: rename before the removes");
        assert!(matches!(enable_lane(ActionId::RenameLane, lane, Ctx::default()), Enable::Yes));
    }
    assert_eq!(ActionId::RenameLane.label(), "Rename track");
    // patch slot + dot + label + two switches, with the 5 px gaps between the five things, fit the head.
    let used = PATCH_W + LANE_DOT_D + 2. * HIT_MIN + 4. * 5. + LANE_NAME_MAX_W;
    assert!(used <= HEAD_W, "{used} > {HEAD_W}");
}

/// The head label is ellipsised and has the full name on hover; a double-click starts the edit;
/// the keys, the commit (one undo step through the session) and the menu row are wired.
#[test]
fn the_head_renames_on_double_click_with_an_ellipsis_and_a_tooltip() {
    let lane = src_text("ui/bench_stance/lane.rs");
    assert!(lane.contains(".w(px(LANE_NAME_MAX_W))") && lane.contains(".truncate()"));
    assert!(lane.contains("tip_hover(&title, \"double-click to rename\""), "full name on hover");
    assert!(lane.contains("event.click_count() >= 2 && this.lane_edit.is_none()"));
    assert!(lane.contains("this.begin_lane_edit(lane, cx)"));
    assert!(src_text("player/blur.rs").contains("self.lane_edit_key(key, ctrl, alt, key_char, cx)"));
    assert!(src_text("ui/stance.rs").contains("this.text_field_key(event, window, cx)"));
    assert!(src_text("player/actions.rs").contains("ActionId::RenameLane => self.begin_lane_edit(lane, cx)"));
    assert!(src_text("ui/overlays.rs").contains("| ActionId::RenameLane"));
    let names = src_text("player/lane_names.rs");
    assert!(names.contains("session.set_lane_name(lane, &text)") && names.contains("self.mark_dirty()"));
    assert!(names.contains("FieldKey::Cancel => self.lane_edit = None"));
}

/// While retyping, a name longer than the box shows its end beside the caret.
#[test]
fn a_name_longer_than_the_box_shows_its_end_while_typed() {
    use crate::player::lane_names::edit_tail;
    assert_eq!(edit_tail("Cam", 7), "Cam");
    assert_eq!(edit_tail("Interview wide", 7), "w wide");
    assert_eq!(edit_tail("abc", 1), "");
    use crate::player::lane_names::ellipsise;
    assert_eq!(ellipsise("V1", 7), "V1");
    assert_eq!(ellipsise("Interview cam", 7), "Interv…");
    assert_eq!(ellipsise("abcdefg", 7), "abcdefg");
    assert_eq!(edit_tail("", 0), "");
}

/// A clip's source field order decides whether the Deinterlace row exists, and the choice is
/// written through the session (one undo step).
#[test]
fn the_deinterlace_row_is_for_interlaced_sources_only() {
    let card = src_text("ui/cards/transform.rs");
    assert!(card.contains("let order = self.clip_interlaced(lane, idx)?;"));
    assert!(card.contains("(\"Frame\", engine::project::Deinterlace::Frame,"));
    assert!(card.contains("(\"Field\", engine::project::Deinterlace::Field,"));
    assert!(card.contains("(\"Off\", engine::project::Deinterlace::Off,"));
    assert!(card.contains(".video.as_ref()?.interlaced"));
    assert!(src_text("player/cards.rs").contains("session.set_deinterlace(lane, idx, mode)"));
}
