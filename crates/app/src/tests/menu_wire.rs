//! The click -> action path of every menu, and the invariants that keep a
//! card, a menu or a lost focus from leaving the room dead. This binary has no
//! `TestAppContext` to mount a `Player` in, so the row-by-row walk runs the
//! oracle over a project with media and a clip selected (the same answer the
//! menus draw their rows by) and scans the wiring that turns a click into the
//! action, the way the neighbouring guards do.

use super::*;
use crate::ui::menubar::{Entry, Title};
use keymap::ActionId;

/// A clip on V1, frames 30..90, the one every row is asked about.
fn clip() -> Clip {
    Clip {
        speed_keys: Vec::new(),
        effects: Vec::new(),
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        reversed: false,
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        start: 30,
        in_frame: 0,
        out_frame: 60,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    }
}

/// A project with media, a clip selected on `lane`, something on the
/// clipboard and history on both sides.
fn ctx(lane: Lane) -> Ctx {
    Ctx {
        clip: Some((clip(), lane)),
        playhead: 45,
        timeline: true,
        playable: true,
        clipboard: true,
        picks: 1,
        pick_lanes: 1,
        ..Ctx::default()
    }
}

/// Every row any menu can draw: the six bar menus, the clip menu, the ruler
/// menu, the dock's body menu and both kinds of lane head.
fn every_menu_row() -> Vec<(String, ActionId)> {
    let mut rows = Vec::new();
    for title in Title::ALL {
        for entry in title.entries() {
            if let Entry::Act(a) | Entry::Switch(a) = entry {
                rows.push((format!("{} menu", title.label()), a));
            }
        }
    }
    for a in MENU_ITEMS {
        rows.push(("clip menu".into(), a));
    }
    for a in BENCH_ITEMS {
        rows.push(("ruler menu".into(), a));
    }
    for a in crate::menus::DOCK_ITEMS {
        rows.push(("dock menu".into(), a));
    }
    for lane in [Lane::V1, Lane::A1] {
        for a in crate::oracle::lane_items(lane) {
            rows.push((format!("{lane:?} head menu"), a));
        }
    }
    rows
}

/// Every row either runs or says why it cannot: an enabled row has an arm in
/// the one action table (so the click reaches a verb, not a fall-through), a
/// disabled one carries its reason, and a hidden one is simply not drawn.
#[test]
fn every_menu_row_runs_or_says_why_it_cannot() {
    let act = src_text("player/actions.rs");
    let table = &act[act.find("pub(crate) fn act(").expect("the action table")..];
    let mut ran = 0;
    for (menu, action) in every_menu_row() {
        for lane in [Lane::V1, Lane::A1] {
            match enable(action, ctx(lane)) {
                Enable::Yes => {
                    ran += 1;
                    let named = format!("ActionId::{action:?}");
                    let by_kind = action.transition_kind().is_some();
                    assert!(
                        by_kind || table.contains(&named),
                        "{menu}: {action:?} is enabled but act() has no arm for it"
                    );
                }
                Enable::No(why) => {
                    assert!(!why.trim().is_empty(), "{menu}: {action:?} is dim with no reason")
                }
                Enable::Hidden(_) => {}
            }
        }
    }
    assert!(ran > 80, "the walk reached only {ran} live rows");
}

/// The rows that open a card, from every door, are the ones the dock tab
/// follows: each is a real action with a card verb behind it.
#[test]
fn card_rows_open_their_card_and_the_dock_follows() {
    let act = src_text("player/actions.rs");
    for (action, opener) in [
        (ActionId::Silence, "self.open_silence("),
        (ActionId::Speed, "self.open_speed("),
        (ActionId::Color, "self.open_color("),
        (ActionId::Transform, "self.open_transform("),
        (ActionId::Equalizer, "self.open_eq("),
        (ActionId::Effects, "self.open_effects("),
        (ActionId::Mix, "self.open_mix("),
        (ActionId::Markers, "self.open_markers("),
        (ActionId::SubtitleStyle, "self.open_subtitle_style("),
    ] {
        let arm = format!("ActionId::{action:?}");
        let at = act.find(&arm).unwrap_or_else(|| panic!("no arm for {action:?}"));
        assert!(
            act[at..at + 400].contains(opener),
            "{action:?} does not open its card through {opener}"
        );
        // Every menu that offers a card is one of the doors.
        assert!(
            Title::Clip.entries().contains(&Entry::Act(action))
                || Title::Timeline.entries().contains(&Entry::Act(action)),
            "{action:?} has no bar-menu row"
        );
    }
    // One invariant for all of them: a card up means the Clip tab shows.
    let render = src_text("render.rs");
    assert!(render.contains("self.sync_dock_tab();"), "the frame must sync the dock tab");
    let sync = fn_body("sync_dock_tab");
    assert!(
        sync.contains("self.dock_src_active && (self.clip_card_open() || self.export_open || self.transcript_open)")
            && sync.contains("self.dock_src_active = false;"),
        "a docked card must switch the dock to the Clip tab"
    );
}

/// Every check mark reads the state its action flips.
#[test]
fn switch_rows_have_a_check_mark_that_reads_their_state() {
    let checked = src_text("ui/menubar.rs");
    let checked = &checked[checked.find("fn checked(").expect("checked()")..];
    let checked = &checked[..checked.find("\n}\n").unwrap()];
    for title in Title::ALL {
        for entry in title.entries() {
            if let Entry::Switch(a) = entry {
                assert!(
                    checked.contains(&format!("ActionId::{a:?} =>")),
                    "{a:?} is a switch row whose check mark reads nothing"
                );
            }
        }
    }
}

/// Undo and Redo dim with nothing to take back or walk forward, in the menu,
/// the toolbar and the palette alike (they all ask `enable`).
#[test]
fn undo_and_redo_dim_with_an_empty_history() {
    let live = ctx(Lane::V1);
    assert!(enable(ActionId::Undo, live.clone()).yes());
    assert!(enable(ActionId::Redo, live.clone()).yes());
    let empty = Ctx { nothing_to_undo: true, nothing_to_redo: true, ..live };
    assert!(matches!(enable(ActionId::Undo, empty.clone()), Enable::No("nothing to undo")));
    assert!(matches!(enable(ActionId::Redo, empty), Enable::No("nothing to redo")));
    let ctx_fn = fn_body("ctx");
    assert!(ctx_fn.contains("session.undo_depth() == 0") && ctx_fn.contains("session.redo_depth() == 0"));
}

/// A docked card is not an invisible modal: a right-click on the bench
/// opens its menu and leaves the card up, and the transport keys keep their meaning.
#[test]
fn a_docked_card_does_not_swallow_right_clicks_or_transport_keys() {
    for open in ["open_menu", "open_gap_menu", "open_bench_menu", "open_head_menu"] {
        assert!(
            fn_body(open).contains("self.menu_allowed()"),
            "{open} must go through menu_allowed, not a bare modal() refusal"
        );
    }
    assert!(!fn_body("menu_allowed").contains("close_card"), "a docked card stays up under a menu");
    for safe in [ActionId::Play, ActionId::GoStart, ActionId::GoEnd, ActionId::Export, ActionId::Settings] {
        assert!(crate::ui::stance::docked_card_safe_action(safe), "{safe:?}");
    }
    // The keys keep their meaning while only a docked card shows: that is a card up that does
    // not cover the room, and not the Export tab.
    let docked = fn_body("docked_card_only");
    assert!(docked.contains("!self.covers_room()") && docked.contains("!self.export_open"), "{docked}");
}

/// The focus invariant: whatever closes, the room's keys keep a home. A check
/// after every frame puts focus back on the root when the focused handle is no
/// longer in the tree.
#[test]
fn focus_returns_to_the_room_when_the_focused_surface_unmounts() {
    let render = src_text("render.rs");
    assert!(render.contains("self.guard_focus(window, cx);"));
    let guard = fn_body("guard_focus");
    assert!(guard.contains("!p.focus.contains_focused(window, cx)"));
    assert!(guard.contains("window.focus(&p.focus)"));
}

/// The clip menu's Adjust submenu carries the Effects opener, and the row walk
/// above reaches it through MENU_ITEMS.
#[test]
fn the_adjust_submenu_opens_the_effects_card() {
    assert!(MENU_ITEMS.contains(&ActionId::Effects));
    assert!(crate::menus::adjust_item(ActionId::Effects));
    assert!(every_menu_row().iter().any(|(m, a)| m == "clip menu" && *a == ActionId::Effects));
}

/// A bar dropdown taller than the window (the Clip menu) scrolls inside its
/// plate, capped by the viewport height, instead of hanging off the bottom.
#[test]
fn a_bar_dropdown_scrolls_inside_the_window() {
    let bar = src_text("ui/menubar.rs");
    let drop = &bar[bar.find("fn dropdown(").expect("dropdown()")..];
    assert!(drop.contains("view_h") && drop.contains("max_h(px(room))") && drop.contains("overflow_y_scroll()"));
}

/// N3: Ungroup, Regroup and a lone Group dim, with a reason, on a clip with no
/// group mate, no cut under the playhead and no partner on another track.
#[test]
fn group_rows_dim_with_a_reason_on_a_plain_clip() {
    let plain = Ctx { alone: true, no_join: true, no_partner: true, playhead: 30, ..ctx(Lane::V1) };
    for a in [ActionId::Detach, ActionId::Regroup, ActionId::Group] {
        let e = enable(a, plain.clone());
        assert!(!e.yes(), "{a:?} must be dim on a plain clip");
        assert!(e.why().is_some(), "{a:?} dims with a reason");
    }
    let mut c = clip();
    c.link = Some(7);
    let grouped = Ctx { clip: Some((c, Lane::V1)), no_join: false, ..plain.clone() };
    assert!(!enable(ActionId::Detach, grouped.clone()).yes(), "alone still dims");
    let grouped = Ctx { alone: false, ..grouped };
    assert!(enable(ActionId::Detach, grouped).yes());
    let cut = Ctx { alone: false, no_join: false, no_partner: false, playhead: 30, ..ctx(Lane::V1) };
    assert!(enable(ActionId::Regroup, cut.clone()).yes());
    assert!(enable(ActionId::Group, cut).yes());
}

/// Item 17: every card opener has a chord, and no two share one.
#[test]
fn every_card_opener_has_a_shortcut() {
    let k = keymap::Keymap::defaults();
    for a in [
        ActionId::Speed, ActionId::Color, ActionId::Transform, ActionId::Equalizer,
        ActionId::Silence, ActionId::Visualizer, ActionId::Fit, ActionId::Markers,
        ActionId::Mix, ActionId::Effects, ActionId::Normalize, ActionId::Captions,
        ActionId::Fillers, ActionId::Voiceover,
    ] {
        assert_ne!(k.display(a), "unbound", "{a:?}");
        assert_ne!(k.display(a), "--", "{a:?}");
    }
}

/// Item 29: the palette sets its text on the shared scale.
#[test]
fn the_palette_uses_the_shared_type_scale() {
    let body = fn_body("palette_row");
    assert!(body.contains("type_scale::body_strong()") && body.contains("type_scale::caption()"));
    assert!(!body.contains("label(14.") && !body.contains("mono(13."));
}

/// Item 16: the shortcuts sheet is a dialog; the kit shell's X closes it.
#[test]
fn the_shortcuts_sheet_has_a_highlighted_tab_and_a_close() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui/dock_stance.rs")).unwrap();
    assert!(!src.contains(r#"dock_tab("dock-tab-keys""#), "the sheet is a dialog, not a dock tab");
    assert!(!src.contains("dock-keys-close"), "the shell's X is the only close");
}

/// The transcript's cut is a Clip row with its own name, and it says what it needs (a picked word)
/// when none is picked. Paste attributes, the chord with no row of its own, is a row of Edit.
#[test]
fn cut_selected_words_and_paste_attributes_have_menu_rows() {
    assert_eq!(ActionId::CutSelectedWords.label(), "Cut selected words");
    assert!(Title::Clip.entries().contains(&Entry::Act(ActionId::CutSelectedWords)), "the Clip menu lists the cut");
    let none = enable(ActionId::CutSelectedWords, ctx(Lane::V1));
    assert!(!none.yes(), "no words picked: the cut is dim");
    assert_eq!(none.why(), Some("no transcript words picked"));
    assert!(enable(ActionId::CutSelectedWords, Ctx { words: true, ..ctx(Lane::V1) }).yes());
    assert!(Title::Edit.entries().contains(&Entry::Act(ActionId::PasteAttributes)), "Edit lists Paste attributes");
}
