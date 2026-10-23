//! Generators, colour labels and the clip switch on the window's side: the keys and menu rows
//! exist and are wired, the label list names the palette, and the clip box wears the marks.

use super::*;
use crate::ui::menubar::{Entry, Title};
use keymap::ActionId;

const NEW: [ActionId; 5] = [
    ActionId::NewSolid,
    ActionId::NewGradient,
    ActionId::NewCountdown,
    ActionId::NewBars,
    ActionId::NewAdjustment,
];

fn in_menu(title: Title, a: ActionId) -> bool {
    fn walk(list: &[Entry], a: ActionId) -> bool {
        list.iter().any(|e| match e {
            Entry::Sub(_, inner) => walk(inner, a),
            Entry::Act(x) | Entry::Switch(x) | Entry::Choice(x) => *x == a,
            Entry::Rule => false,
        })
    }
    walk(title.layout(), a)
}

#[test]
fn file_new_offers_the_five_generators_and_the_clip_menu_has_label_and_enabled() {
    for a in NEW {
        assert!(in_menu(Title::File, a), "{a:?} is under File > New");
        assert!(crate::menus::DOCK_ITEMS.contains(&a), "{a:?} is in the dock menu");
    }
    assert!(in_menu(Title::Clip, ActionId::ClipLabel));
    assert!(in_menu(Title::Clip, ActionId::ClipEnabled));
    assert!(MENU_ITEMS.contains(&ActionId::ClipLabel) && MENU_ITEMS.contains(&ActionId::ClipEnabled));
}

#[test]
fn shift_e_switches_a_clip_and_the_rest_are_menu_only() {
    let keymap = keymap::Keymap::defaults();
    assert_eq!(keymap.display(ActionId::ClipEnabled), "Shift+E");
    for a in NEW.into_iter().chain([ActionId::ClipLabel]) {
        assert_eq!(keymap.display(a), "unbound", "{a:?}");
    }
}

#[test]
fn every_new_action_is_dispatched_and_marks_the_project_dirty() {
    let act = src_text("player/actions.rs");
    for a in NEW {
        let name = format!("ActionId::{a:?}");
        assert!(act.matches(&name).count() >= 2, "{name} is in the dirty list and the table");
    }
    assert!(act.contains("ActionId::ClipEnabled => self.toggle_clip_enabled(cx)"));
    assert!(act.contains("ActionId::ClipLabel => self.open_label_list(cx)"));
}

#[test]
fn a_new_generator_is_one_undo_step_on_a_lane_with_room() {
    let src = src_text("player/generators.rs");
    let at = src.find("fn new_generator").expect("new_generator");
    let body = &src[at..];
    assert!(body.contains("begin_batch") && body.contains("end_batch"), "one step for lane and clip");
    assert!(body.contains("add_lane"), "a lane is added when none has room");
    assert!(body.contains("reverse()"), "an adjustment layer looks from the top lane down");
}

#[test]
fn the_label_list_names_the_palette_and_the_clip_box_wears_the_marks() {
    let cards = src_text("player/cards.rs");
    assert!(cards.contains("Pick::Label"));
    assert_eq!(engine::project::LABEL_COUNT, 8);
    assert_eq!(engine::project::label_name(1), Some("Violet"));
    let clip = src_text("ui/bench_stance/clip.rs");
    assert!(clip.contains("mark.label > 0") && clip.contains("mark.off"));
    assert!(src_text("files.rs").contains("engine::generate::title"));
}

#[test]
fn the_generator_card_is_mounted_in_the_clip_tab() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains("generator_card(room, cx)"));
    let card = src_text("ui/cards/generator.rs");
    assert!(card.contains("write_generator"));
}

/// Shift+E (and every other modified clip chord) must reach the keymap while a
/// docked card such as Colour is open: the card reads bare keys and shift+arrow
/// only.
#[test]
fn docked_card_leaves_modified_strokes_to_the_room() {
    use crate::ui::stance::docked_card_ignores as ig;
    assert!(ig("e", false, true, false), "shift+e");
    assert!(ig("v", true, false, true), "ctrl+alt+v");
    assert!(ig("delete", false, true, false), "shift+delete");
    assert!(!ig("r", false, false, false), "bare r is the card's reset");
    assert!(!ig("left", false, true, false), "shift+left is the eq card's");
    assert!(!ig("escape", false, true, false));
    let keymap = crate::keymap::Keymap::defaults();
    assert_eq!(keymap.lookup("e", false, true, false), Some(ActionId::ClipEnabled));
}

/// Key routing rule: a docked card eats only the strokes it reads. Bare `s`
/// (split), Space, and every other keymap letter reach the room while a card is
/// open in the Clip tab; only a covering sheet or a menu blocks the keymap.
#[test]
fn docked_card_eats_only_the_keys_it_reads() {
    let cards = src_text("player/cards.rs");
    let at = cards.find("pub(crate) fn param_card_key").expect("param_card_key");
    let end = cards[at..].find("export_moment_key(key, shift, cx)").expect("end");
    let body = &cards[at..at + end];
    // Every docked card's branch answers `false` for a key it does not read
    // (eq, transform, silence, effects, mix, subtitle: `else { return false }`;
    // colour and speed: `None => return false`).
    assert!(body.matches("return false;").count() >= 6, "card branches report unread keys");
    assert!(body.matches("None => return false").count() >= 2);
    let stance = src_text("ui/stance.rs");
    assert!(
        stance.find("if this.docked_card_only()\n                && this.context_menu.is_none()")
            < stance.find("if this.card_open()\n                || this.context_menu.is_some()"),
        "a docked card hands unread keys to the keymap before the blanket modal guard"
    );
}
