//! Nothing in the window steps a multi-value setting to its next value on a press:
//! a setting with more than two values is a list the person picks from, so the
//! value they land on is the one they chose.

use super::*;
use crate::export_ui::{ease_label, fit_choices, resolution_choices};
use crate::menus::{Choice, Pick};
use engine::transform::KeyEase;
use keymap::ActionId;

/// The lines of `text` that are code or strings, not comments.
fn live(text: &str) -> String {
    text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// Spelled in two halves so this file never matches its own scan.
fn press_to(verb: &str) -> String {
    format!("{verb} to {}", "cycle")
}

#[test]
fn no_handler_or_key_steps_a_setting_to_its_next_value() {
    let src = ui_source();
    // `cycle_on_key_down` is the Tab ring between panels, not a setting.
    assert!(!src.replace("fn cycle_on_key_down(", "").contains("fn cycle_"), "a `cycle_*` handler is back in the window's source");
    let act = live(&fn_body("run_action"));
    for needle in ["cycle", ".next()", "next_fit", "next_resolution"] {
        assert!(!act.contains(needle), "act() steps a value with `{needle}`");
    }
    // The three actions that used to step now open their list.
    for arm in [
        "ActionId::Resolution => self.open_picker(Pick::Resolution",
        "ActionId::Fit => self.open_fit_list(",
        "ActionId::KeyEasing => self.open_key_easing(",
    ] {
        assert!(act.contains(arm), "act() lacks `{arm}`");
    }
}

#[test]
fn no_user_visible_string_says_cycle() {
    let src = live(&ui_source());
    for verb in ["click", "press", "Click", "Press"] {
        let phrase = press_to(verb);
        assert!(!src.contains(&phrase), "user-visible `{phrase}`");
    }
    for action in ActionId::ALL {
        let label = action.label().to_lowercase();
        assert!(!label.contains("cycle") && !label.contains("next colour"), "{action:?}: {label}");
    }
    let marker = src_text("ui/cards/markers.rs");
    assert!(!marker.contains("Next colour"), "the marker swatch still says it steps");
}

#[test]
fn the_rows_that_became_lists_say_so_in_their_labels() {
    assert_eq!(ActionId::Resolution.label(), "Project resolution…");
    assert_eq!(ActionId::Fit.label(), "Fit to frame…");
    assert_eq!(ActionId::KeyEasing.label(), "Key easing…");
    assert_eq!(ActionId::SpeedRampFastSlowFast.label(), "Speed ramp fast-slow-fast");
    assert_eq!(ActionId::ToggleProxies.label(), "Use editing copies");
}

#[test]
fn key_easing_offers_every_easing_by_name() {
    let names: Vec<_> = KeyEase::ALL.into_iter().map(ease_label).collect();
    assert_eq!(names, ["Linear", "Ease in", "Ease out", "Ease in-out", "Hold"]);
}

#[test]
fn the_resolution_and_fit_lists_hold_every_value_and_mark_one() {
    let rows = resolution_choices((1920, 1080), (1920, 1080));
    assert!(rows.len() >= 4);
    assert_eq!(rows.iter().filter(|r| r.3).count(), 1);
    assert!(rows.iter().all(|r| matches!(r.0, Choice::Size(..))));
    let fits = fit_choices(Lane::V1, 0, FitPolicy::Fill, (1920, 1080));
    assert_eq!(fits.len(), 4);
    assert_eq!(fits.iter().filter(|r| r.3).count(), 1);
    // The pickers exist for the new doors.
    let _ = (Pick::KeyEase(engine::project::KeyKind::Placement), Pick::MarkerColor(0));
}

#[test]
fn settings_says_what_it_means_in_words_a_person_uses() {
    let page = live(&src_text("ui/settings_stance.rs"));
    for jargon in ["stand-in", "\"Palette\"", "\"Auto proxies\"", "cd/m^2", "'Reference' tonemap"] {
        assert!(!page.contains(jargon), "settings still says `{jargon}`");
    }
    assert!(page.contains("\"Theme\"") && page.contains("\"Make them on import\""));
}

/// The lists open next to what opened them: a menu row as a flyout beside it, a shortcut
/// above the status-bar item it is about, a dock button under itself -- never at the pointer.
#[test]
fn every_list_opens_beside_the_thing_that_opened_it() {
    let act = live(&fn_body("run_action"));
    assert!(!act.contains("mouse_position"), "a shortcut opens a list at a stale pointer position");
    for arm in [
        "self.open_picker(Pick::Resolution, self.ledger_anchor(LedgerItem::Size)",
        "self.open_fit_list(self.ledger_anchor(LedgerItem::Clip)",
        "self.open_key_easing(self.ledger_anchor(LedgerItem::Clip)",
    ] {
        assert!(act.contains(arm), "act() lacks `{arm}`");
    }
    // The menu bar's rows are flyouts, not actions.
    for action in [ActionId::Fit, ActionId::KeyEasing] {
        let door = format!("Choice(ActionId::{action:?})");
        assert!(src_text("ui/menubar.rs").contains(&door), "{action:?} is not a flyout row");
        assert!(!src_text("ui/menubar.rs").contains(&format!("Act(ActionId::{action:?})")));
    }
    // The status bar's size and rate are the doors for the same two lists, and report where
    // they paint so a shortcut can hang its list above them.
    let ledger = src_text("ui/stance.rs");
    assert!(ledger.contains("ledger_readout"), "the status bar lost `ledger_readout`");
    // Dock and card buttons hang the list from their own foot, off the timeline floor.
    assert!(src_text("ui/dock_stance.rs").contains("p.floored = false"));
    assert!(src_text("ui/cards/markers.rs").contains("picker_under(Pick::MarkerColor"));
    let overlays = src_text("ui/overlays.rs");
    assert!(overlays.matches("!picker.floored").count() == 2);
}
