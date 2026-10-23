//! Sequences, second round: compound clips draw like clips, the library counts nested use,
//! the clip menu folds the compound verbs into one submenu, and plurals are words.

use super::*;
use keymap::ActionId;

/// No sequence message ships a "(s)".
#[test]
fn sequence_messages_use_count_noun() {
    let code = src_text("player/sequences.rs");
    assert!(!code.contains("(s)"), "sequences.rs still says (s)");
    assert!(code.contains("count_noun(clips.len(), \"clip\", \"clips\")"));
}

/// The clip menu lists the compound verbs, and they fold into a "Compound" submenu next to
/// "Adjust" so the top level still fits a 720 window.
#[test]
fn the_clip_menu_folds_the_compound_verbs_into_one_submenu() {
    for a in [ActionId::NestClips, ActionId::BreakApart, ActionId::OpenCompound, ActionId::FlattenMulticam] {
        assert!(MENU_ITEMS.contains(&a), "{a:?}");
        assert!(crate::menus::compound_item(a));
        assert!(!crate::menus::adjust_item(a));
    }
    assert!(!crate::menus::compound_item(ActionId::Cut));
    let overlays = src_text("ui/overlays.rs");
    assert!(overlays.contains("\"Compound\""), "the submenu row is drawn");
}

/// The compound's trace and the overview are drawn from the clips it stands for.
#[test]
fn a_compound_is_drawn_from_its_pieces() {
    let clip = src_text("ui/bench_stance/clip.rs");
    assert!(clip.contains("compound_pieces(lane.kind, clip)"));
    let overview = src_text("ui/bench_stance/overview.rs");
    assert!(overview.contains("compound_pieces(lane.kind, clip)"));
}

/// A source used only inside a nested sequence is on the timeline as far as the library goes.
#[test]
fn the_library_counts_use_in_every_sequence() {
    assert!(src_text("player/actions.rs").contains("source_uses_parked(idx)"));
    assert!(src_text("ui/dock_stance.rs").contains("source_uses_parked(source_idx)"));
}
