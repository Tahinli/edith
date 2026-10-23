//! Round-5 GUI polish: one text field for the renames, the sequence tab menu, a strip that
//! never shifts, tooltips off the timeline, grouped menu bar menus, two-line media meta, the
//! angle monitor's tiles and Flatten multicam's reach. Pure functions and scans of the wiring.

use super::*;
use crate::player::text_field::{FieldKey, TextField};
use crate::ui::menubar::{Entry, Title};
use keymap::ActionId;

/// Ctrl+A selects the whole name, typing replaces a selection, backspace clears it, and both the
/// sequence tab and the track head run the very same field.
#[test]
fn the_renames_share_one_text_field_with_select_all() {
    let mut f = TextField::selected("Sequence 2");
    assert!(f.all, "a rename opens fully selected");
    assert_eq!(f.key("c", false, false, Some("C"), 80), FieldKey::Edited);
    assert_eq!(f.text, "C", "typing replaces the selection");
    f.key("u", false, false, Some("u"), 80);
    assert_eq!(f.text, "Cu");
    f.key("a", true, false, Some("a"), 80);
    assert!(f.all && f.text == "Cu", "ctrl+a selects, and does not type");
    f.key("t", false, false, Some("t"), 80);
    assert_eq!(f.text, "t");
    f.key("a", true, false, None, 80);
    f.key("backspace", false, false, None, 80);
    assert_eq!(f.text, "");
    assert_eq!(f.key("enter", false, false, None, 80), FieldKey::Commit);
    assert_eq!(f.key("escape", false, false, None, 80), FieldKey::Cancel);
    let mut g = TextField::selected("");
    for c in "abcdef".chars() {
        g.key("x", false, false, Some(&c.to_string()), 3);
    }
    assert_eq!(g.text, "abc", "the cap holds");
    assert!(src_text("player/sequences.rs").contains("field.key(key, ctrl, alt, key_char, MAX_SEQUENCE_NAME_CHARS)"));
    assert!(src_text("player/lane_names.rs").contains("field.key(key, ctrl, alt, key_char, MAX_LANE_NAME_CHARS)"));
}

/// A sequence tab has a right-click menu in the shared popup look; deleting is undoable with a
/// toast offering Undo.
#[test]
fn a_sequence_tab_has_a_menu_and_delete_offers_undo() {
    assert_eq!(
        crate::menus::SEQ_TAB_ITEMS,
        [ActionId::RenameSequence, ActionId::DuplicateSequence, ActionId::DeleteSequence]
    );
    assert!(crate::menus::destructive(ActionId::DeleteSequence), "delete sits under the rule line");
    let bar = src_text("ui/toolbar.rs");
    assert!(bar.contains("MouseButton::Right") && bar.contains("open_seq_tab_menu(id"));
    assert!(src_text("ui/overlays.rs").contains("MenuOn::SeqTab => SEQ_TAB_ITEMS.to_vec()"));
    let seq = src_text("player/sequences.rs");
    assert!(seq.contains("crate::notices::ToastAction::Undo"), "the delete toast offers Undo");
}

/// The back chevron's slot is always there (drawn only inside a compound), so the tabs never shift.
#[test]
fn the_back_button_is_always_drawn_so_the_tabs_never_shift() {
    let bar = src_text("ui/toolbar.rs");
    let at = bar.find("\"seq-back\"").expect("the back button");
    let before = &bar[..at];
    assert!(before[before.len() - 300..].contains(".w(px(78.))") && before[before.len() - 300..].contains(".h(px(24.))"), "its slot has a fixed size");
    assert!(before[before.len() - 300..].contains(".when(back"), "it draws only inside a compound");
}

/// A tooltip with the pointer in the timeline region hangs above that region; above it, the
/// ordinary place.
#[test]
fn tooltips_over_the_timeline_hang_above_it() {
    use crate::ui::widgets::tip_lift;
    assert_eq!(tip_lift(300., 400.), None, "above the region: ordinary place");
    assert_eq!(tip_lift(300., 0.), None, "no region known");
    let lift = tip_lift(460., 400.).expect("inside the region");
    // The plate's bottom edge: the pointer's y (+1) minus the lift is above the region's top.
    assert!(461. - lift < 400.);
    assert_eq!(tip_lift(700., 400.), None, "over the lanes a plate stays by its pointer");
    let render = src_text("render.rs");
    assert!(render.contains("TIMELINE_TOP_BITS"));
    assert!(src_text("ui/widgets.rs").contains("tip_lift(my, top)"), "one placement rule, in widgets.rs");
}

fn drawn_lists(list: &'static [Entry], out: &mut Vec<&'static [Entry]>) {
    out.push(list);
    for e in list {
        if let Entry::Sub(_, inner) = e {
            drawn_lists(inner, out);
        }
    }
}

/// Every menu bar menu, with every row present, fits a 1280x720 window; the Clip menu groups its
/// verbs into flyouts the way Premiere does.
#[test]
fn every_menubar_menu_fits_1280_by_720() {
    let room = 720. - crate::layout::MENU_BAR_H - crate::layout::MENU_EDGE;
    for title in Title::ALL {
        let mut all = Vec::new();
        drawn_lists(title.layout(), &mut all);
        for list in all {
            let h = crate::ui::menubar::entries_h(list);
            assert!(h <= room, "{} menu: {h} px over {room}", title.label());
        }
    }
    let names: Vec<&str> = Title::Clip
        .layout()
        .iter()
        .filter_map(|e| if let Entry::Sub(n, _) = e { Some(*n) } else { None })
        .collect();
    for want in ["Trim", "Speed", "Adjust", "Compound", "Multicam"] {
        assert!(names.contains(&want), "Clip ▸ {want}");
    }
    assert!(Title::Clip.entries().contains(&Entry::Act(ActionId::FlattenMulticam)), "Flatten is in the Clip menu");
}

/// The media row's facts are one caption line that ends in an ellipsis rather
/// than breaking mid-phrase (REDESIGN §3.8).
#[test]
fn media_meta_is_one_unbroken_line() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains(".whitespace_nowrap()") && dock.contains("let (line2, line2_ink)"));
}

/// The monitor draws no empty slot.
#[test]
fn the_angle_monitor_never_draws_an_empty_slot() {
    use crate::player::multicam::monitor_rows;
    for n in 1..=9 {
        let rows = monitor_rows(n);
        assert_eq!(rows.iter().sum::<usize>(), n, "{n} angles");
        assert!(rows.iter().all(|&r| r > 0 && r <= 3));
    }
    assert_eq!(monitor_rows(2), [1, 1]);
    assert_eq!(monitor_rows(9), [3, 3, 3]);
}

/// Flatten multicam is on whenever any piece of a multicam clip is selected or under the
/// playhead, and hidden only for a plain clip.
#[test]
fn flatten_is_reachable_from_any_piece_of_a_multicam_clip() {
    use crate::oracle::{Ctx, Enable, enable};
    let piece = Ctx { multicam: true, timeline: true, playable: true, ..Ctx::default() };
    assert_eq!(enable(ActionId::FlattenMulticam, piece), Enable::Yes, "no clip asked about, a piece picked");
    let plain = Ctx {
        clip: Some((Clip::source_range(0, 0, 10), Lane::V1)),
        timeline: true,
        playable: true,
        ..Ctx::default()
    };
    assert!(matches!(enable(ActionId::FlattenMulticam, plain), Enable::Hidden(_)));
    let src = src_text("player/actions.rs");
    assert!(src.contains("picks.iter().any(|&(lane, idx)| session.multicam_at(lane, idx).is_some())"));
}
