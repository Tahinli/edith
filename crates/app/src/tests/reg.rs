//! Regressions found after many merges: the timeline's menus and keys under the Export tab, the
//! Clip tab's Speed gap, one open/close rule for the sections, the media tile's hover "+".

use super::*;

/// The Export tab is a dock region, not a sheet: counting it as a card made every timeline
/// right-click (and every key) do nothing while it was showing.
#[test]
fn the_export_tab_showing_does_not_block_menus_or_keys() {
    let cards = src_text("player/cards.rs");
    let at = cards.find("pub(crate) fn menu_allowed").unwrap();
    assert!(cards[at..at + 300].contains("!self.covers_room()"), "menus ask covers_room, not modal");
    let t = src_text("player/timeline_edit.rs");
    assert!(!t.contains("self.modal()"), "timeline gestures ask covers_room");
    for open in ["open_head_menu", "open_gap_menu", "open_bench_menu"] {
        let t = src_text("player/timeline_edit.rs");
        assert!(t.contains(&format!("fn {open}")), "{open} exists");
    }
}

#[test]
fn silence_opens_beside_the_other_sections() {
    let cards = src_text("player/cards.rs");
    let at = cards.find("self.silence_open = Some((lane, idx));").unwrap();
    assert!(cards[..at].trim_end().ends_with("self.close_other_cards();"));
}

#[test]
fn the_speed_caption_is_one_unwrapped_line_and_the_tile_plus_shows_on_hover() {
    assert!(!src_text("ui/cards/speed.rs").contains("dark_help(tail_text"));
    let dock = src_text("ui/dock_stance.rs");
    let at = dock.find("dock-tile-add-btn").unwrap();
    assert!(dock[at - 400..at].contains("group_hover"));
}
