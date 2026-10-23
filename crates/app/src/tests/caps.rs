//! Captions as an NLE draws them: a block per cue with its words inside, edits through the
//! engine's cue ops, the plain tool names, and the subtitle style card built from kit rows.

use super::*;
use crate::player::caption_cues::{CueGrab, dragged_times};

#[test]
fn a_caption_drag_stays_between_its_neighbours() {
    let (orig, room) = ((3_000_000, 5_000_000), (2_000_000, 6_000_000));
    assert_eq!(dragged_times(CueGrab::Start, orig, room, 0, 0), (2_000_000, 5_000_000));
    assert_eq!(dragged_times(CueGrab::End, orig, room, 0, 9_000_000), (3_000_000, 6_000_000));
    assert_eq!(dragged_times(CueGrab::Body, orig, room, 4_000_000, 9_000_000), (4_000_000, 6_000_000));
}

#[test]
fn speech_models_have_plain_names_and_the_real_one_in_the_tooltip() {
    assert_eq!(crate::creator::model_label("tiny"), "Fast");
    assert_eq!(crate::creator::model_label("base"), "Balanced");
    assert_eq!(crate::creator::model_label("small"), "Accurate");
    assert_eq!(crate::creator::model_label("medium"), "Medium");
    assert!(crate::creator::model_tip("tiny").contains("tiny"));
}

#[test]
fn each_caption_is_a_block_with_a_menu_an_editor_and_an_edit_hint() {
    let sub = src_text("ui/bench_stance/sub.rs");
    for row in ["Edit text", "Split at playhead", "Merge with next", "Delete", "Style\\u{2026}"] {
        assert!(sub.contains(row), "the caption menu offers {row}");
    }
    assert!(sub.contains("Double-click to edit"), "the hover tooltip names the double-click");
    assert!(sub.contains("start_cue_drag(lane, track, cue, CueGrab::Body"), "a body press arms the move");
    assert!(sub.contains("kit::plate"), "the editor and the menu are on the one plate");
    let cues = src_text("player/caption_cues.rs");
    for call in ["set_cue_times", "merge_cue_next", "split_cues_at", "delete_cue"] {
        assert!(cues.contains(call), "{call} is one engine edit, so one undo step");
    }
}

#[test]
fn the_tools_card_leads_with_the_transcript_and_names_its_tabs_plainly() {
    let tools = src_text("ui/cards/tools.rs");
    assert!(tools.contains("From the transcript") && tools.contains("Listen again"));
    assert!(tools.contains("segmented_equal"), "the tab strip is kit segments, never clipped");
    let p = src_text("player/tools.rs");
    for name in ["Normalize", "Captions", "Filler words", "Voiceover"] {
        assert!(p.contains(&format!("\"{name}\"")), "{name} is a tab name");
    }
}

#[test]
fn the_subtitle_style_card_is_settings_style_rows() {
    let s = src_text("ui/cards/substyle.rs");
    assert!(s.contains("HIGHLIGHT_SWATCHES") && s.contains("subtitle-hl-hex"), "highlight swatches, then the hex on its own row");
    assert!(s.contains("segmented_equal") && s.contains("kit::switch"));
    assert!(s.contains("LABEL_W"), "every label shares one column");
}

/// Only Settings and the project dialogs cover the room. A card docked in the right panel never
/// blocks the timeline's menus, drags or clicks, however many are up at once.
#[test]
fn only_real_overlays_cover_the_room() {
    let cards = src_text("player/cards.rs");
    let at = cards.find("pub(crate) fn covers_room").unwrap();
    let body = &cards[at..at + 120];
    assert!(body.contains("self.settings_open || self.proj_dialog.is_some()"), "{body}");
    for docked in ["export_open", "eq_open", "tools", "markers_open", "silence_open", "subtitle_style_open"] {
        assert!(!body.contains(docked), "{docked} is docked and must not cover the room");
    }
}

/// A selected caption is the Clip tab's own panel: its words, start, end and length, then the
/// Subtitle style section -- never the "Select a clip" hero.
#[test]
fn a_selected_caption_has_its_own_panel_in_the_clip_tab() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains("player.sel_cue_span().is_some()") && dock.contains("player.caption_card(cx)"));
    let card = src_text("ui/cards/caption.rs");
    for id in ["caption-text", "caption-start", "caption-end", "caption-length"] {
        assert!(card.contains(id), "{id}");
    }
    let cues = src_text("player/caption_cues.rs");
    assert!(cues.contains("set_cue_times"), "a number is one cue edit, so one undo step");
}
