//! The dialogs' keyboard and shell: Tab walks a dialog's controls, the root is focused when one
//! opens, an open list answers the arrows, every dialog is one shell.

use super::*;

#[test]
fn the_keyboard_ring_is_the_keyboards_alone() {
    crate::ui::kit::set_kbd_ring(true);
    crate::ui::kit::set_kbd_ring(false);
}

#[test]
fn every_dialog_is_built_from_the_one_shell() {
    let src = ui_source();
    // The shell carries the close X and sits below the menu bar.
    assert!(src.contains("\"dialog-x\""));
    assert!(src.contains(".top(px(crate::layout::MENU_BAR_H))"));
}

#[test]
fn tab_is_the_dialogs_while_one_is_up() {
    let stance = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/stance.rs")).unwrap();
    let nav = stance.find("dialog_nav_key").expect("stance routes Tab to the dialog");
    let about = stance.find("The About dialog: Esc or Enter closes it").unwrap();
    assert!(nav < about, "dialog navigation must come before every dialog's own keys");
    assert!(stance.contains("window.focus(&player.focus)"), "the root takes the keyboard when a dialog opens");
}

#[test]
fn an_open_list_takes_up_down_and_enter() {
    let stance = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/stance.rs")).unwrap();
    assert!(stance.contains("An open dropdown list: up and down move its highlighted row"));
}
