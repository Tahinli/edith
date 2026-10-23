//! Timeline fixes: the empty-area menu and the plain wording of the clip menu.

use crate::menus::{BENCH_ITEMS, MENU_ITEMS};
use crate::keymap::ActionId;

#[test]
fn the_empty_area_menu_pastes_and_drops_a_marker() {
    assert!(BENCH_ITEMS.contains(&ActionId::Paste));
    assert!(BENCH_ITEMS.contains(&ActionId::MarkerHere));
    assert!(BENCH_ITEMS.contains(&ActionId::PlayheadHere));
}

#[test]
fn the_clip_menu_keeps_its_action_ids_while_the_words_are_plain() {
    for id in [ActionId::LoopTrim, ActionId::Dissolve, ActionId::Regroup] {
        assert!(MENU_ITEMS.contains(&id), "{id:?}");
    }
    let src = include_str!("../ui/overlays.rs");
    for word in ["Loop while trimming", "Blend into next clip", "Group again"] {
        assert!(src.contains(word), "{word}");
    }
}
