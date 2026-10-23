//! Round-4 GUI polish: row fit, card padding, plurals, popup-look lists, the
//! About box, tooltips, the Export tab and the viewer menu. No `TestAppContext`
//! here, so each check is a pure function or a scan of the wiring.

use super::*;
use crate::ui::widgets::spans_cover;
use crate::count_noun;
use keymap::ActionId;

/// REDESIGN §3.8: a media row is 64px with a 50px poster, name, one caption
/// line and a chip line (18 + 16 + 18), and a refused row stays the same height.
#[test]
fn a_media_row_is_64px_and_a_refused_row_does_not_grow() {
    use crate::ui::dock_stance::{SOURCE_ROW_H, source_row_h};
    const { assert!(SOURCE_ROW_H >= 50. + 2. * 6.) };
    assert_eq!(SOURCE_ROW_H, 64.);
    assert_eq!(source_row_h(false), source_row_h(true));
}

/// Card rows are inset on both sides, so a slider's knob at the end of its
/// bar and the last chip of a row never touch the card's right edge.
#[test]
fn card_rows_are_padded_on_both_sides() {
    for file in ["color", "silence", "transform", "mix"] {
        let text = src_text(&format!("ui/cards/{file}.rs"));
        assert!(!text.contains(".pl(px(6.))"), "{file}.rs has a row inset on the left only");
    }
}

#[test]
fn counts_are_pluralised_in_words() {
    assert_eq!(count_noun(1, "silence", "silences"), "1 silence");
    assert_eq!(count_noun(0, "silence", "silences"), "0 silences");
    assert_eq!(count_noun(2, "silence", "silences"), "2 silences");
    // No message ships a "(s)" any more.
    for file in ["player/cards.rs", "subs.rs", "player/library.rs"] {
        let code: String = src_text(file)
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for stale in ["clip(s)", "caption(s)", "track(s)"] {
            assert!(!code.contains(stale), "{file} still says {stale}");
        }
    }
    assert!(src_text("player/cards.rs").contains("count_noun(count, \"silence\", \"silences\")"));
}

/// A normal toast is not cut short: the bar gives it the room the format and
/// verbs on the right leave, up to a wide cap.
#[test]
fn a_toast_is_wide_enough_for_an_ordinary_message() {
    let line = "1 silence cut on V1+A1 — other tracks untouched — 2.7s shorter · undo Ctrl+Z";
    assert!(crate::ui::popup::text_w(line, 14., false) < crate::ui::stance::TOAST_MAX_W);
    assert!(src_text("ui/stance.rs").contains(".max_w(px(TOAST_MAX_W))"));
}

/// The effects add-list is the shared popup row on a popup-look plate.
#[test]
fn the_effects_add_list_wears_the_popup_look() {
    let text = src_text("ui/cards/effects.rs");
    assert!(text.contains("popup_row_icon((\"effects-kind\""));
    assert!(!text.contains("chip((\"effects-kind\""));
    assert!(text.contains("kit::plate("));
}

/// The range hints fit one line of the dock: no orphan "(I and O)".
#[test]
fn the_range_hints_are_short() {
    let none = crate::export_ui::range_options(None, false, 900, 30.);
    let late = crate::export_ui::range_options(None, true, 900, 30.);
    for why in [none.marks_off.unwrap(), late.marks_off.unwrap()] {
        let line = format!("off \u{2014} {why}");
        assert!(line.chars().count() <= 50, "{line}");
        assert!(!line.contains('('), "{line}");
    }
}

/// A preset that the settings equal is the one that shows checked; Custom is
/// for settings that equal none.
#[test]
fn the_matching_preset_is_checked_and_custom_only_when_none_match() {
    use crate::export_ui::{matching_preset, picked_with};
    for p in engine::export_presets::presets() {
        let picked = picked_with(picked_with_default(), p);
        assert_eq!(matching_preset(picked, Some(p.id)).map(|m| m.id), Some(p.id), "{}", p.id);
    }
    let first = &engine::export_presets::presets()[0];
    let off = crate::export_ui::Picked { bps: 123_456, ..picked_with(picked_with_default(), first) };
    assert!(matching_preset(off, None).is_none());
}

fn picked_with_default() -> crate::export_ui::Picked {
    crate::export_ui::Picked {
        format: Format::Mp4,
        bps: 0,
        audio_kbps: 128,
        size: None,
        depth: None,
        loudness: None,
        prores: Default::default(),
        hdr: Default::default(),
    }
}

/// A dock list row's plate only appears to say why it is refused; one that
/// repeats the row's own words is gone.
#[test]
fn a_dock_verb_row_has_no_tooltip_unless_refused() {
    let text = src_text("ui/dock_stance.rs");
    let body = &text[text.find("fn section_verb(").expect("section_verb")..];
    let at = body.find("action_hover").expect("the refusal plate");
    assert!(body[..at].contains(".when(!on"), "the plate must be inside the refused branch");
}

#[test]
fn the_lane_bed_has_no_hover_plate_over_its_clips() {
    assert!(spans_cover([(30, 60), (200, 10)], 30));
    assert!(spans_cover([(30, 60), (200, 10)], 89));
    assert!(!spans_cover([(30, 60), (200, 10)], 90));
    assert!(!spans_cover([(30, 60), (200, 10)], 150));
    assert!(!spans_cover([], 0));
    // The empty bed says nothing on hover: the drop highlight is its feedback.
    let lane = src_text("ui/bench_stance/lane.rs");
    assert!(!lane.contains("BedTip") && !lane.contains("Drop media here to add it to this track"));
}

/// Export done on a visible Export tab is its own panel; no toast beside it.
#[test]
fn the_export_outcome_toast_is_skipped_while_the_tab_shows_it() {
    let body = fn_body("poll_export");
    assert!(body.contains("export_tab_visible"), "{body}");
    let visible = fn_body("export_tab_visible");
    assert!(visible.contains("export_open") && visible.contains("keys_open"));
}

/// The viewer's menu: existing actions only, each with a label, none unbound.
#[test]
fn the_viewer_menu_offers_existing_actions_only() {
    let keymap = crate::keymap::Keymap::defaults();
    for a in crate::menus::VIEWER_ITEMS {
        assert!(ActionId::ALL.contains(&a));
        assert!(!a.label().is_empty());
        assert!(!keymap.display(a).contains("unbound"), "{a:?}");
    }
    for want in [ActionId::Play, ActionId::Screenshot, ActionId::Settings] {
        assert!(crate::menus::VIEWER_ITEMS.contains(&want), "{want:?}");
    }
    assert!(fn_body("open_viewer_menu").contains("self.menu_allowed()"));
    assert!(src_text("ui/stance.rs").contains("open_viewer_menu"));
}
