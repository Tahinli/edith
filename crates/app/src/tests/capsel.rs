//! The caption panel of the Clip tab: a selected caption is the tab's own panel (header, words,
//! start and end that scrub or type, a read-only length, its three actions, then the style
//! section), and its number drags one whole frame at a time, one cue edit on the release.

use super::*;
use crate::player::caption_cues::{cue_secs, scrubbed_us};

#[test]
fn a_scrubbed_caption_number_moves_by_frames_and_clamps() {
    // Two seconds at 25 fps: 100 px at 0.02 s a pixel is two seconds more, on whole frames.
    assert_eq!(scrubbed_us(2_000_000, 0., false, 25.), 2_000_000);
    assert_eq!(scrubbed_us(2_000_000, 50., false, 25.), 3_000_000);
    // A fine drag (shift) moves a tenth as far: 100 px is 0.2 s.
    assert_eq!(scrubbed_us(2_000_000, 100., true, 25.), 2_200_000);
    // A value never goes below zero, nor past ten hours.
    assert_eq!(scrubbed_us(100_000, -1_000., false, 25.), 0);
    assert_eq!(scrubbed_us(35_000_000_000, 1_000_000., false, 25.), 36_000_000_000);
    // A frame rate that is not usable falls back to 25 fps rather than dividing by it.
    assert_eq!(scrubbed_us(2_000_000, 50., false, 0.), 3_000_000);
}

#[test]
fn a_caption_number_reads_in_seconds_with_two_places() {
    assert_eq!(cue_secs(1_840_000), "1.84");
    assert_eq!(cue_secs(0), "0.00");
}

#[test]
fn a_selected_caption_panel_has_its_actions_and_read_only_length() {
    let card = src_text("ui/cards/caption.rs");
    for label in ["Split at playhead", "Merge with next", "Delete", "Caption {} of {}"] {
        assert!(card.contains(label), "{label}");
    }
    // The three actions are the caption menu's own, so a reason shows the same way.
    for run in ["split_cue_at_playhead", "merge_cue_with_next", "delete_cue_at"] {
        assert!(card.contains(run), "{run}");
    }
    // The length is read off the two numbers: no number field of its own.
    assert!(!card.contains("num(\"caption-length\""), "length is read-only");
    // A number that is dragged is written through the caption ops, one edit each.
    let cues = src_text("player/caption_cues.rs");
    assert!(cues.contains("fn cue_num_scrub_end") && cues.contains("set_cue_times"));
}

#[test]
fn the_empty_clip_hero_waits_for_nothing_selected() {
    let dock = src_text("ui/dock_stance.rs");
    // The hero is the empty state only: a selected caption (`cue_up`) keeps the tab off it.
    assert!(dock.contains("let empty = anchor.is_none() && !player.clip_card_open() && !title_up && !cue_up;"));
    // A selected caption shows its panel and the style section only, none of a clip's sections.
    let at = dock.find("if cue_up {").expect("the caption branch");
    let branch = &dock[at..at + 260];
    assert!(branch.contains("player.caption_card(cx)") && branch.contains("subtitle_style_card"), "{branch}");
}
