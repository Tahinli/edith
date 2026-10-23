//! DESIGN.md §5 -- the stance: fixed geography, the only room now the
//! legacy four-region tree is gone (`self.darkroom` no longer branches
//! anything in `render.rs`), whole here as its own module rather than
//! folded into that tree.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::ui::bench_stance;
use crate::ui::dock_stance;
use crate::ui::hitmap;
use crate::ui::menubar;
use crate::ui::settings_stance;
use crate::ui::timeband_stance;
use crate::ui::toolbar;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// Keyboard focus v1: the three regions Tab/Shift-Tab cycle focus through
/// (no ring is painted for it any more, user 2026-09-10) -- the bench, the dock's Sources tab (library) and the dock's
/// Clip tab (inspector), the darkroom's own stand-ins for "timeline",
/// "library" and "inspector" (there is no separate inspector *region* in
/// this tree -- MOCK-SPEC's Clip tab, `dock_stance::clip_tab`, is it).
/// `Player::focus` (the root handle every keybind hangs off) is not a
/// member: Tab already means "select the clip under the playhead"
/// (`ActionId::Select`, `keymap.rs`) at the root, unchanged here, and only
/// starts meaning "enter the ring" once one of these three already holds
/// focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Surface {
    Dock,
    Bench,
    Inspector,
}

/// Fixed cycle order: dock, bench, inspector.
const SURFACE_CYCLE: [Surface; 3] = [Surface::Dock, Surface::Bench, Surface::Inspector];

/// Tab (`backward` false) or Shift-Tab (`backward` true) from `current`,
/// wrapping at either end of [`SURFACE_CYCLE`].
pub(crate) fn next_surface(current: Surface, backward: bool) -> Surface {
    let i = SURFACE_CYCLE.iter().position(|&s| s == current).unwrap();
    let n = SURFACE_CYCLE.len();
    SURFACE_CYCLE[if backward {
        (i + n - 1) % n
    } else {
        (i + 1) % n
    }]
}

/// The one key a surface's own `on_key_down` answers. Everything else is
/// left alone on purpose, unhandled and un-stopped, so it bubbles to the
/// root's fallback handler exactly as an unfocused key already does
/// (`window.rs`'s key dispatch walks the focused node's ancestors in
/// `Bubble` phase and stops only at `cx.stop_propagation()` -- the three
/// surface handlers call that solely from this branch, gpui-0.2.2
/// `window.rs:3897-3906`).
pub(crate) fn is_focus_cycle_key(key: &str) -> bool {
    key == "tab"
}

/// The other key a surface's own `on_key_down` answers: leaves the ring for
/// the root handle (`Player::focus`) rather than letting the stroke bubble
/// there itself -- same one-door reason as [`is_focus_cycle_key`], so the
/// bench and both dock tabs cannot answer "what closes the ring" two
/// different ways.
pub(crate) fn is_focus_exit_key(key: &str) -> bool {
    key == "escape"
}

/// The room-wide chords an open card does not take.
///
/// A card owns the keyboard for its own *values* -- the arrows on a slider, `r`
/// for reset, `m` for maximize, the digits on the equalizer, `enter` to apply
/// ([`Player::param_card_key`]'s branch list) -- and every one of those is a
/// *bare* stroke its own branch claims. These five are not: each acts on the
/// document rather than on the card the hand is looking at, and while a card is
/// open the room must still answer them. It is the equalizer card's own comment
/// that says so from the inside: "Nothing to undo: every change is already at
/// the clip, and undo is undo's own key".
///
/// - `Undo`/`Redo`: the edit history. The one chord a hand reaches for *while*
///   looking at what it just changed, which is exactly when a card is open.
/// - `Save`: the project file. Its chord is `ctrl+s`, and no card's branch
///   answers a ctrl chord on purpose (`param_card_key` is handed the shift
///   modifier alone).
/// - `Copy`/`Paste`: the clip clipboard, the pair beside the history keys.
///
/// Asked against the [`keymap::ActionId`] the stroke resolved to, never against
/// a key literal: a rebind moves the chord and this list follows it.
///
/// Deliberately *not* here, in three kinds:
///
/// 1. The strokes a card's own branch list claims, which the card must keep:
///    escape (every card's way out, and `Deselect`'s own stroke), the arrows
///    (`StepBack`/`StepForward`/`JumpBack`/`JumpForward` -- the branch reads
///    them with or without ctrl), `m` (`ToggleMute`) and the digits, plus the
///    bare letters the cards' verbs sit on -- `r` (`Loop`, `Resolution`), `f`
///    (`Mix`, `Crossfade`), `a` (`AddAudioLane`, `SelectAll`), `s`
///    (`Cut` -- the EQ card's spectrum), `x` (`Delete`, `Dissolve` -- the
///    band's removal), `k` (`Color`), `j`, `u`, `y` (`SubtitleStyle`), `enter`
///    (the silence card's apply).
/// 2. The chords that open a card: `Export`/`Settings`/`Color`/`Transform`/
///    `Speed`/`Silence`/`Mix`/`Equalizer`/`SubtitleStyle`. They own the very
///    modal slot the open card is sitting in, and swapping the surface under a
///    hand mid-stroke is not a chord this fix is for.
/// 3. Everything else the keymap binds (the odometer, the trims, the zoom, the
///    lane add/remove pair, the theme, the screenshot...). They are left to the
///    card on purpose, the way [`Player::param_card_key`] left them before this:
///    the defect is a card swallowing the *history* while the user edits through
///    it, and each of these is one press away again the moment the card closes.
pub(crate) fn card_safe_action(action: crate::keymap::ActionId) -> bool {
    use crate::keymap::ActionId;
    matches!(
        action,
        ActionId::Undo | ActionId::Redo | ActionId::Save | ActionId::Copy | ActionId::Paste
    )
}

/// The strokes that keep their meaning while only a docked card is up
/// ([`Player::docked_card_only`]): the transport and the room's own doors. The
/// stepping, arrow and letter keys are left to the card, which reads them.
pub(crate) fn docked_card_safe_action(action: crate::keymap::ActionId) -> bool {
    use crate::keymap::ActionId;
    matches!(
        action,
        ActionId::Play
            | ActionId::GoStart
            | ActionId::GoEnd
            | ActionId::ShuttleStop
            | ActionId::Export
            | ActionId::Settings
            | ActionId::ShowActions
            | ActionId::Fullscreen
            | ActionId::Theme
            | ActionId::AccentColour
    )
}

/// The strokes that keep their meaning while the Export tab shows: the transport's, and the
/// ones that set what it exports -- I and O mark the range its In-Out choice uses, the arrows
/// that step to the frame, and clearing the marks. The tab is a tab of the dock, not a sheet
/// over the room, so the timeline stays in reach of the keyboard.
pub(crate) fn export_tab_action(stroke: crate::keymap::ActionId) -> bool {
    use crate::keymap::ActionId;
    docked_card_safe_action(stroke)
        || matches!(
            stroke,
            ActionId::SetIn | ActionId::SetOut | ActionId::ClearRange | ActionId::StepBack | ActionId::StepForward
        )
}

/// Whether a docked card never reads this stroke. The cards read bare keys
/// (arrows, digits, letters like `r`, `m`) and shift+arrow; a stroke with ctrl
/// or alt, or shift on anything but an arrow, is nobody's card key, so the
/// room's own bindings (Shift+E enable, Shift+R freeze, Ctrl+Alt+V...) answer it
/// instead of the card swallowing it with the rest.
pub(crate) fn docked_card_ignores(key: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
    let arrow = matches!(key, "left" | "right" | "up" | "down");
    ctrl || alt || (shift && !arrow && key != "escape" && key != "tab")
}

/// The dock mounts only one of its two tabs at a time
/// (`dock_stance::render`'s `match src_active`), so `focus_dock` only has a
/// tree node under Sources and `focus_inspector` only under Clip -- focusing
/// the unmounted one leaves gpui with a focused handle no node answers to,
/// and every key dies until the next mouse click. Answers "does entering
/// `surface` require flipping the dock tab first", pure so it's testable
/// without a `Window`: `Some(true)`/`Some(false)` is the `dock_src_active`
/// the surface needs mounted, `None` (bench) means leave the tab alone.
pub(crate) fn surface_wants_src_active(surface: Surface) -> Option<bool> {
    match surface {
        Surface::Dock => Some(true),
        Surface::Inspector => Some(false),
        Surface::Bench => None,
    }
}

impl Player {
    /// The `FocusHandle` a [`Surface`] paints its ring on and Tab/Shift-Tab
    /// moves to next -- one door, so `next_surface`'s answer and the handle
    /// `window.focus` is given cannot name two different surfaces.
    pub(crate) fn focus_handle(&self, surface: Surface) -> &FocusHandle {
        match surface {
            Surface::Dock => &self.focus_dock,
            Surface::Bench => &self.focus_bench,
            Surface::Inspector => &self.focus_inspector,
        }
    }

    /// One door onto every ring move: flips the dock tab first
    /// ([`surface_wants_src_active`]) so `surface`'s handle is always
    /// mounted before `window.focus` lands on it, then persists the flip
    /// exactly as the tab's own click does (`dock_stance::dock_tab`'s
    /// `on_click`), so the ring and a mouse click can't disagree about
    /// which tab was last chosen.
    pub(crate) fn focus_surface(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(src_active) = surface_wants_src_active(surface) {
            self.dock_src_active = src_active;
            crate::ui::dock_stance::save(src_active);
        }
        window.focus(self.focus_handle(surface));
        cx.notify();
    }
}

/// Fixed strip under the screen: timecode, transport, cut readout -- one row
/// of 44px (DESIGN §5). The overview strip moved up above the ruler, so the
/// band no longer carries a minimap. Kept `pub(crate)` for
/// `layout::split_bounds`'s bench ceiling, the same one-door reason
/// [`LEDGER_H`] already is.
pub(crate) const TIME_BAND_H: f32 = 44.;
/// The bench's untouched height -- [`Split::Bench`]'s own default, read by
/// `layout::split_size` the same way the legacy tree's `library_w`/
/// `inspector_w` feed `Split::Library`/`Split::Inspector`. A hand on the
/// seam above it overrides this; nothing else does.
pub(crate) const BENCH_H: f32 = 200.;
/// Thin strip at the foot of the centre column. Read by `layout`'s
/// `Split::Bench` drag formula (the seam sits above the fixed ledger, so a
/// drag has to leave room for it) -- kept `pub(crate)` for that one door
/// rather than a second copy of the number.
pub(crate) const LEDGER_H: f32 = 24.;
/// The dock's untouched width -- [`Split::Dock`]'s own default (DESIGN §5).
/// Its floor is `layout::DOCK_MIN_W`, not the inspector's.
pub(crate) const DOCK_W: f32 = 320.;
/// What [`bench`] spends above `bench_stance::render`'s own content: the
/// `bench` div's own `.border_t_1()` (1px) + its `py(4.)` top padding, and
/// nothing else since the `BENCH` section head went (cleanse round 2 -- the
/// ruler row starts the region and the geography says what it is). `1 + 4`
/// = `5`. Named and `pub(crate)` rather than an inline subtraction, so
/// `layout::BENCH_MIN_H` can derive from the real chrome.
pub(crate) const BENCH_CHROME_H: f32 = 5.;

/// The lowest a menu's top edge may sit and still land inside the
/// bench/ledger/dock footprint below the screen (DESIGN §5, §9, §11 check 6):
/// a right-click near the top of the window must not walk the menu up over
/// the picture just because the pointer is there. `overlays.rs`/`library.rs`
/// clamp their `menu.at.y` against this before handing it to `menu_at`, only
/// in the darkroom stance -- the legacy tree has no fixed screen/time-band
/// split for this to mean anything against, and must not move. `bench_h` is
/// the live, possibly hand-dragged `Split::Bench` height (`Player::split_px`)
/// rather than the fixed [`BENCH_H`] default, so a widened bench clamps the
/// menu against the room actually left, not the room it started with.
pub(crate) fn below_picture_floor(viewport_h: f32, bench_h: f32) -> f32 {
    (viewport_h - TIME_BAND_H - bench_h - LEDGER_H).max(0.)
}

/// The maximized inspector card's own top -- deliberately *not*
/// [`below_picture_floor`] fed the live bench straight through, the way
/// `menu_floor` above correctly does for a menu. A maximized card is anchored
/// to the same floor and grows downward to the window's own bottom edge, so
/// its height is `viewport_h - floor` -- exactly the bench/ledger footprint,
/// `TIME_BAND_H + bench_h + LEDGER_H`. Feeding the *live* bench in there means
/// a hand-dragged bench smaller than [`BENCH_H`] starves the maximized card of
/// room: driven at `bench=BENCH_MIN_H` the card shrank to 210px and its curve
/// and band handles stopped rendering at all, while the same card *docked*
/// (whose height was never bench-shaped to begin with) still drew everything
/// -- maximize made the card strictly worse, backwards from what the feature
/// promises. Floored at [`BENCH_H`], the bench's own untouched default, the
/// maximized card is never smaller than it is at the bench nobody has
/// dragged; a bench pulled *larger* than that still grows the card further,
/// same as before, and the case actually verified good (card top exactly on
/// the picture floor, picture unobstructed) is untouched because it ran at a
/// bench already at or above this floor.
pub(crate) fn maximized_card_top(viewport_h: f32, bench_h: f32) -> f32 {
    below_picture_floor(viewport_h, bench_h.max(BENCH_H))
}

/// The floor-clamped placement for every scrolling menu (clip context menu,
/// picker, library menu): the anchor pulled down to [`below_picture_floor`]
/// *and* the room its list may fill capped to what is actually left between
/// that floor and the window's bottom edge -- not the whole viewport. Passing
/// the untouched viewport to `menu_rows_h` let a menu taller than the
/// bench/ledger/dock footprint size itself against the full window, and then
/// `menu_at`'s own bottom-edge clamp (`v.min(room - size)`) pulled the
/// already-floored top edge back up over the picture to make it fit -- the
/// menu-occlusion defect this exists to close. Sizing the list against the
/// room *below the floor* instead means it never needs more than that room,
/// so `menu_at` never has a reason to walk it back up; a list still taller
/// than the room scrolls inside its own plate, same as the keys overlay.
///
/// The floor is the picture's own *painted* bottom edge
/// ([`picture_bottom`]) when a frame is up, not the screen region's: the
/// region letterboxes, and a black bar is not picture. With a 3840x1608
/// film in a 2560x1440 window that edge sits ~170px above the region's own
/// bottom, which is two more row slots the menu no longer has to scroll
/// through. Nothing is shown (safelight, empty room, before the first
/// paint) -> the region's bottom, unchanged. Returned alongside the anchor
/// so the caller can flip the plate *above* its pointer without a second
/// copy of this arithmetic.
pub(crate) fn menu_floor(
    at: Point<Pixels>,
    viewport: Size<Pixels>,
    bench_h: f32,
    picture_bottom: Option<f32>,
) -> (Point<Pixels>, Size<Pixels>, f32) {
    let region = below_picture_floor(f32::from(viewport.height), bench_h);
    let floor = picture_bottom.unwrap_or(region).clamp(0., region);
    let at = point(at.x, px(f32::from(at.y).max(floor)));
    let room = size(viewport.width, px(f32::from(viewport.height) - floor));
    (at, room, floor)
}

/// The plate's top edge, flipped to sit *above* the pointer when the room
/// below it does not hold the whole list -- `library.rs` already places its
/// menu this way for the same reason, and every row above the fold is a row
/// nobody has to wheel to. Never above `floor`: a menu that gained rows by
/// climbing over the picture is the occlusion this file exists to prevent
/// (DESIGN §5, §11 check 6). `room` from [`menu_floor`] already sizes the
/// list to fit between `floor` and the window's bottom edge, so the clamped
/// flip always fits.
pub(crate) fn menu_above(
    at: Point<Pixels>,
    viewport: Size<Pixels>,
    floor: f32,
    h: f32,
) -> Point<Pixels> {
    if f32::from(at.y) + h + MENU_EDGE <= f32::from(viewport.height) {
        return at;
    }
    point(at.x, px((f32::from(at.y) - h).max(floor)))
}

#[cfg(test)]
mod menu_floor_tests {
    use super::*;

    /// The defect measured live: a 396px-tall menu anchored high in a 720px
    /// window used to size itself against the full window (720), so
    /// `menu_at`'s bottom clamp walked its floored top edge back up to
    /// 720-396=324 -- 10px above the picture's own floor (404 at this
    /// window height: 720-88-200-28). The room handed to the list sizer must
    /// now be capped to the footprint (316px here), so the placed top edge
    /// never leaves the floor.
    #[test]
    fn a_menu_taller_than_the_footprint_stays_pinned_to_the_floor_not_walked_back_over_it() {
        let viewport = size(px(1280.), px(720.));
        let floor = below_picture_floor(f32::from(viewport.height), BENCH_H);
        let (at, room, _) = menu_floor(point(px(300.), px(20.)), viewport, BENCH_H, None);
        assert_eq!(f32::from(at.y), floor);
        // 17 rows -- more than the footprint holds -- sized through the real
        // sizer, so the `MENU_EDGE` margin is in the number the clamp sees.
        let list_h = crate::oracle::menu_rows_h(17, room);
        assert!(list_h <= f32::from(room.height));
        let h = crate::layout::MENU_PAD * 2. + list_h;
        let (_, y) = crate::oracle::menu_at(at, viewport, h);
        assert_eq!(
            y, floor,
            "the top edge must not be walked back above the floor"
        );
    }

    /// Defect 1 as the user met it (`f-clip-menu.png`): a menu opened near the
    /// right and bottom of a 1280x720 window ran off both edges -- chords cut
    /// mid-word at the dock seam, rows past the frame. Pure clamp math, both
    /// window sizes the charter names, and both the floored (clip) menu and
    /// the unfloored (library) one: whatever the anchor, the whole plate lands
    /// inside the frame with [`MENU_EDGE`] to spare, and the floored one never
    /// buys that room off the picture.
    #[test]
    fn a_menu_opened_in_the_bottom_right_corner_lands_whole_inside_the_window() {
        use crate::layout::{MENU_EDGE, MENU_PAD, MENU_W};
        for viewport in [size(px(1280.), px(720.)), size(px(2560.), px(1440.))] {
            let (w, v) = (f32::from(viewport.width), f32::from(viewport.height));
            let floor = below_picture_floor(v, BENCH_H);
            for rows in [1usize, 5, 16, 40] {
                for anchor in [
                    point(px(w - 1.), px(v - 1.)),
                    point(px(w - 1.), px(0.)),
                    point(px(0.), px(v - 1.)),
                ] {
                    // The clip menu's path: floored below the picture, sized
                    // against the room that leaves, then clamped.
                    // Both the picture floors a menu can get: the region's
                    // own bottom (nothing shown) and a letterboxed film's
                    // painted edge, 120px above it.
                    for pic in [None, Some(floor - 120.)] {
                        let (at, room, f) = menu_floor(anchor, viewport, BENCH_H, pic);
                        let h = MENU_PAD * 2. + crate::oracle::menu_rows_h(rows, room);
                        // Through the flip: the placement the app actually uses.
                        let at = menu_above(at, viewport, f, h);
                        let (x, y) = crate::oracle::menu_at(at, viewport, h);
                        assert!(y >= f, "y {y} walked above the menu floor {f}");
                        assert!(
                            x >= MENU_EDGE && x + MENU_W <= w - MENU_EDGE,
                            "x {x} rows {rows}"
                        );
                        assert!(
                            y >= MENU_EDGE && y + h <= v - MENU_EDGE,
                            "y {y} h {h} rows {rows}"
                        );
                    }
                    // The library menu's path: no floor, hung a row below the
                    // pointer, same clamp.
                    let h = MENU_PAD * 2. + crate::oracle::menu_rows_h(rows, viewport);
                    let (x, y) =
                        crate::oracle::menu_at(point(anchor.x, anchor.y + px(52.)), viewport, h);
                    assert!(x >= MENU_EDGE && x + MENU_W <= w - MENU_EDGE, "lib x {x}");
                    assert!(y >= MENU_EDGE && y + h <= v - MENU_EDGE, "lib y {y} h {h}");
                }
            }
        }
    }

    /// The floor the charter names (R4): a wide film letterboxed into a tall
    /// region stops being picture well above that region's bottom, and the
    /// menu may have that black. A 3840x1608 film fit into the real
    /// 2560x1440 darkroom's picture box (2224x1129) lands 931px tall -- a
    /// 99px bar top and bottom -- so the floor rises 99px, two more row
    /// slots. A 4:3 source in a 4:3 box has no bar at all and the floor stays
    /// the region's own bottom.
    #[test]
    fn the_menu_floor_is_the_picture_s_painted_edge_not_the_region_s() {
        let native = |w, h| size(gpui::DevicePixels(w), gpui::DevicePixels(h));
        let box_of = |w: f32, h: f32| Bounds {
            origin: point(px(0.), px(88.)),
            size: size(px(w), px(h)),
        };
        // Wide film, tall box: bars top and bottom, so the edge rises.
        let wide = fitted_bottom(box_of(2224., 1129.), native(3840, 1608)).unwrap();
        let painted_h = 2224. * 1608. / 3840.;
        let want = 88. + (1129. - painted_h) / 2. + painted_h;
        assert!(
            (wide - want).abs() < 1.,
            "wide film's painted bottom {wide}, wanted {want}"
        );
        assert!(88. + 1129. - wide > 90., "the bar the menu gains is real");
        // Same ratio as its box: no bar, the edge IS the box's bottom.
        let square = fitted_bottom(box_of(800., 600.), native(1600, 1200)).unwrap();
        assert!((square - 688.).abs() < 0.01, "4:3 in a 4:3 box: {square}");
        // Nothing measured yet reads as no floor, never a NaN one.
        assert_eq!(fitted_bottom(box_of(0., 0.), native(1600, 1200)), None);

        // And the floor menu_floor hands the sizer follows it: the wide film
        // buys the menu rows, the region's bottom is still the ceiling on it.
        let viewport = size(px(2560.), px(1440.));
        let region = below_picture_floor(1440., BENCH_H);
        let (_, room, floor) = menu_floor(point(px(10.), px(10.)), viewport, BENCH_H, Some(wide));
        assert_eq!(floor, wide);
        assert!(f32::from(room.height) > 1440. - region);
        // A bogus reading below the bench never lets a menu climb: clamped.
        let (_, _, floor) = menu_floor(point(px(10.), px(10.)), viewport, BENCH_H, Some(9999.));
        assert_eq!(floor, region);
    }
}

#[cfg(test)]
mod project_readout_tests {
    use super::*;

    /// The ledger's resting readout (nothing selected): the timeline's length
    /// as a timecode and in seconds, and its clip count; an empty timeline
    /// says so rather than reading `00:00:00:00 · 0 clips`.
    #[test]
    fn the_resting_readout_names_the_length_and_the_clip_count() {
        assert_eq!(project_readout(285, 2, 30.), "Timeline 0:10 · 2 clips");
        assert_eq!(project_readout(50, 1, 25.), "Timeline 0:02 · 1 clip");
        assert_eq!(project_readout(6600, 20, 30.), "Timeline 3:40 · 20 clips");
        assert_eq!(project_readout(0, 0, 25.), "Empty timeline · drag media here to start");
    }
}

#[cfg(test)]
mod maximized_card_top_tests {
    use super::*;

    /// The regression D1 shipped as: the old call site fed the *live* bench
    /// straight into `below_picture_floor`, so a bench dragged down to
    /// `crate::BENCH_MIN_H` (a legal value -- `layout::split_size` clamps
    /// anything smaller up to it) gave the maximized card *less* room than
    /// docked, not more. This binary has no `TestAppContext`, so it cannot
    /// read back a painted rect the way a driven screenshot can -- this is a
    /// value-level check of the two formulas the maximize feature is built
    /// on, not a substitute for driving the app at the smallest bench. Swept
    /// across the *whole* legal bench range (the earlier guard this replaces
    /// checked only the untouched default, which is exactly why a small,
    /// legal bench slipped through), maximized room must never fall below the
    /// room at the bench nobody has dragged, and must never be smaller than
    /// what the same live bench leaves for `below_picture_floor`'s own live
    /// use (menu placement) -- the two must not tangle back together.
    #[test]
    fn maximized_room_never_shrinks_below_the_default_bench_across_every_legal_bench_height() {
        let viewport = size(px(1280.), px(720.));
        let v = f32::from(viewport.height);
        let default_room = v - below_picture_floor(v, BENCH_H);
        let mut bench_h = crate::BENCH_MIN_H;
        while bench_h <= 600. {
            let room = v - maximized_card_top(v, bench_h);
            assert!(
                room >= default_room,
                "bench={bench_h}: maximized room {room} fell below the default-bench \
                 room {default_room}"
            );
            // Still strictly bigger than a bare `below_picture_floor` room
            // would ever have given it below `BENCH_H` -- the coupling this
            // fix breaks.
            if bench_h < BENCH_H {
                assert!(room > v - below_picture_floor(v, bench_h));
            }
            bench_h += 25.;
        }
    }
}

thread_local! {
    // The picture region's own laid-out box, measured each frame by a
    // [`bounds_probe`] the same shape `timeband_stance`'s contact strip keeps
    // (`STRIP_BOUNDS`) -- [`picture_bottom`] needs the box the picture was
    // fitted into, which nothing upstream of paint knows. (Cleanse round 2,
    // 2026-09-10: the `scale 0.59` readout this also fed is deleted -- fit is
    // `^0` and the number was a state nobody acts on.)
    pub(crate) static PICTURE_BOUNDS: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
}

/// The pure half of [`picture_bottom`]: the bottom edge of the `Contain`
/// fit of `native` into `bounds` -- gpui's own `get_bounds`, the same call
/// [`letterboxed_image`] paints through, so this is the frame's real edge
/// and not aspect math reimplemented here.
fn fitted_bottom(bounds: Bounds<Pixels>, native: gpui::Size<gpui::DevicePixels>) -> Option<f32> {
    if bounds.size.width <= px(0.)
        || bounds.size.height <= px(0.)
        || native.width.0 <= 0
        || native.height.0 <= 0
    {
        return None;
    }
    let fitted = gpui::ObjectFit::Contain.get_bounds(bounds, native);
    Some(f32::from(fitted.origin.y + fitted.size.height))
}

/// Where the picture actually stops being picture: the letterboxed frame's
/// bottom edge in window coordinates, read off the last frame's measured box
/// ([`PICTURE_BOUNDS`]). `None` with nothing shown or before the first paint
/// measured anything -- [`menu_floor`] falls back to the region's bottom.
pub(crate) fn picture_bottom(player: &Player) -> Option<f32> {
    let image = player.image.clone()?;
    fitted_bottom(PICTURE_BOUNDS.with(Rc::clone).get(), image.size(0))
}

/// Gap between the picture and the viewer's edge (DESIGN §6.2: "letterboxed
/// and centred, with 12px padding").
const VIEWER_PAD: f32 = 12.;

/// The viewer header (REDESIGN §2.1): 28px on `e1` (24 on a window shorter than
/// 800px), above the picture instead of a chip drawn over it. Left, the viewer's
/// zoom and playback-quality choices and the guides toggle -- the viewer only
/// fits the picture today, so they are drawn disabled with the reason; right,
/// the project's format as a plain readout.
fn viewer_header(player: &Player, window: &Window, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::icons::{Icon, IconSize};
    use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
    let t = crate::ui::theme::tokens();
    let short = f32::from(window.viewport_size().height) < 800.;
    let h = if short { crate::layout::redesign::VIEWER_HDR_H_SHORT } else { crate::layout::redesign::VIEWER_HDR_H };
    // Each control sits in a fixed-width cell, so a longer value never moves its neighbours.
    let dropdown = |id: &'static str, slot: usize, text: SharedString, w: f32, action: ActionId, cx: &mut Context<Player>| {
        let yes = player.enable(action, None).yes();
        div()
            .relative()
            .flex_none()
            .w(px(w))
            .child(bounds_probe(player.viewer_btns[slot].clone()))
            .child(
                Button::new(id)
                    .style(ButtonStyle::Ghost)
                    .size(ButtonSize::Small)
                    .label(text)
                    .min_w(w)
                    .enabled(yes)
                    .build()
                    .child(Icon::ChevronDown.el(IconSize::Inline, t.ink3))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.act(action, window, cx)))
                    .tooltip(crate::ui::widgets::action_hover(player, action))
                    .children(crate::ui::hitmap::action(action, yes)),
            )
    };
    let format = player.session.as_ref().map(|s| {
        let (w, h) = s.resolution();
        format!("{} · {} fps", size_label(w, h), fps_label(player.fps))
    });
    div()
        .id("stance-viewer-header")
        .flex_none()
        .w_full()
        .h(px(h))
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(12.))
        .bg(rgb(t.e1))
        .child(dropdown("vh-zoom", 0, player.viewer.zoom.label().into(), 64., ActionId::ViewZoom, cx))
        .child(dropdown("vh-quality", 1, player.viewer.quality.closed_label().into(), 128., ActionId::PlaybackQuality, cx))
        .child(dropdown("vh-guides", 2, format!("Guides: {}", player.viewer.guide.label()).into(), 136., ActionId::GuideStyle, cx))
        .child(dropdown("vh-mask", 3, format!("Mask: {}", player.viewer.mask.label()).into(), 104., ActionId::GuideMask, cx))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .justify_end()
                .overflow_hidden()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink2))
                .children(source_name(player).map(|name| div().min_w(px(0.)).truncate().pr(px(8.)).child(name))),
        )
        .children(player.session.is_some().then(|| source_toggle(player, cx)))
        // The readout is a door to the project's own settings, where the size
        // and rate are changed: a hover says so, and a click goes there.
        .children(format.map(|text| {
            div()
                .id("vh-project")
                .flex_none()
                .px(px(4.))
                .rounded(px(4.))
                .cursor_pointer()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink3))
                .hover(|s| s.bg(rgb(t.e2)).text_color(rgb(t.ink1)))
                .tooltip(crate::ui::widgets::overlay_tip_hover(
                    "Project settings",
                    "",
                    Some("The size and frame rate of this project: click to change them"),
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.settings_tab = crate::ui::settings_stance::SettingsTab::Project;
                    this.open_settings(cx);
                }))
                .child(text)
        }))
}

/// The name of the clip open in the source viewer, shown in the header while it is up.
fn source_name(player: &Player) -> Option<SharedString> {
    let (path, _) = player.source_target.as_ref().filter(|_| player.preview_session.is_some())?;
    Some(stem(path).into())
}

/// The Source / Timeline switch of the viewer header: which of the two the
/// picture shows. Source is the library item opened in the source viewer (its
/// name is shown in the header beside it, [`source_name`]); Timeline
/// is the edit. Both pills keep their width in either state, and Source is
/// disabled until an item has been opened.
fn source_toggle(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::kit::{segmented, Segment};
    let target = player.source_target.clone();
    // The pill always says "Source": the clip's name is in the viewer header beside it, so the
    // pill never changes width.
    let label: SharedString = "Source".into();
    let empty_source = player.source_hint && target.is_none();
    let selected = usize::from(player.preview_session.is_none() && !empty_source);
    let no_source = target.is_none();
    let me = cx.entity();
    div()
        .id("vh-source-wrap")
        .flex_none()
        .mr(px(8.))
        .child(segmented(
            "vh-source",
            &[Segment::text(label), Segment::text("Timeline")],
            selected,
            player.session.is_some(),
            None,
            move |i, _, app| {
                me.update(app, |this, cx| match (i, this.source_target.clone()) {
                    (0, Some((path, stream))) if this.preview_session.is_none() => this.open_preview(&path, stream, cx),
                    // Nothing opened yet: the viewer says how to open a clip, instead of nothing happening.
                    (0, None) => {
                        this.source_hint = true;
                        cx.notify();
                    }
                    (1, _) => {
                        this.source_hint = false;
                        this.close_preview(cx);
                        cx.notify();
                    }
                    _ => {}
                })
            },
        ))
        .tooltip(crate::ui::widgets::tip_hover(
            "Source or timeline",
            "Esc",
            no_source.then_some("Double-click a media item to open it here"),
        ))
        .children(crate::ui::hitmap::control("viewer.source", "Source or timeline", target.is_some()))
}

/// What the Source side shows before any clip was opened in it: the way to open one.
fn source_empty() -> impl IntoElement {
    use crate::ui::icons::{Icon, IconSize};
    let t = crate::ui::theme::tokens();
    div()
        .id("source-empty")
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(Icon::Film.el(IconSize::Hero, t.ink3))
        .child(div().type_style(type_scale::title()).text_color(rgb(t.ink2)).child("Open a clip from Media to preview it here"))
        .child(
            div()
                .type_style(type_scale::body())
                .text_color(rgb(t.ink3))
                .child("Double-click it, or right-click it and choose Open in source viewer"),
        )
}

/// The picture region: top of the centre column, takes the remaining space.
/// Reuses [`Player::picture_area`] rather than a second image element -- the
/// darkroom draws the same picture the legacy tree does. The picture is fitted
/// into the viewer minus [`VIEWER_PAD`] on every side, and nothing is drawn over it (DESIGN §6.2).
fn screen(
    player: &mut Player,
    position: f64,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    div()
        .id("stance-screen")
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.open_viewer_menu(event.position, cx);
            }),
        )
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .relative()
        // §4: room chrome takes 0 radius.
        .rounded(px(0.))
        .bg(rgb(crate::ui::theme::tokens().matte))
        .child(viewer_header(player, window, cx))
        .child(
            div()
                .id("stance-picture")
                .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                    this.viewer_wheel(event, window, cx);
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, _| this.viewer_drag_start(e.position)))
                .on_mouse_down(MouseButton::Middle, cx.listener(|this, e: &MouseDownEvent, _, _| this.viewer_drag_start(e.position)))
                .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                    if e.pressed_button.is_some() {
                        this.viewer_drag_move(e.position, e.modifiers.shift, window, cx);
                    } else {
                        this.viewer_drag_end(cx);
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.viewer_drag_end(cx)))
                .on_mouse_up(MouseButton::Middle, cx.listener(|this, _: &MouseUpEvent, _, cx| this.viewer_drag_end(cx)))
                .flex_1()
                .min_h(px(0.))
                .p(px(VIEWER_PAD))
                .flex()
                .gap(px(VIEWER_PAD))
                // The source monitor's picture is draggable onto a lane: the
                // marked part (the whole source with no marks) lands where it
                // is let go, through the same door a library row uses.
                .when_some(player.source_target.clone().filter(|_| player.preview_session.is_some()), |d, (path, stream)| {
                    let ghost: SharedString = crate::file_name(&path).into();
                    let me = cx.entity();
                    d.on_drag(crate::interact::AssetDrag(path, stream), move |_, _, _, cx| {
                        me.update(cx, |this, _| {
                            this.drag_lane = None;
                            this.drag_x = None;
                        });
                        cx.new(|_| crate::ui::widgets::Tip(ghost.clone()))
                    })
                })
                .child(
                    // The padded box is the picture's box: the bounds probe
                    // measures it, so the fit and the subtitle placement both
                    // work in the same inset region.
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .flex()
                        .relative()
                        .children(program_and_monitor(player, position, window, cx))
                        .child(bounds_probe(PICTURE_BOUNDS.with(Rc::clone)))
                        .children(player.viewer_handle_layer(window.scale_factor(), cx)),
                ),
        )
        // The preview's own plate, a flex sibling: drawn inside the picture it would cover the frame.
        .children(player.source_strip(window, cx))
        // Only while an edit point is being trimmed, rolled, slipped, slid or
        // nudged: a split over the picture's own box, taking no room.
        .children(player.trim_two_up())
}

/// The narrowest strip the angle monitor takes beside the program picture, in px. Narrower than
/// this, the monitor shares the row with the picture instead, and the picture gives it room.
const MONITOR_STRIP_MIN: f32 = 200.;

/// The free strip left of the program picture, in px, from the picture box as the last frame
/// measured it and the picture as it is drawn (zoom, pan and fit included). `None` before the
/// first paint has measured the box.
fn monitor_strip(player: &Player, window: &Window) -> Option<f32> {
    let b = PICTURE_BOUNDS.with(Rc::clone).get();
    if b.size.width <= px(0.) || b.size.height <= px(0.) {
        return None;
    }
    let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let bx = (f32::from(b.size.width), f32::from(b.size.height));
    let Some(image) = player.image.clone() else {
        return Some(bx.0);
    };
    let native = player.viewer_native_or(&image);
    let frame = crate::viewer_view::picture_rect(player.viewer.zoom, native, bx, (ox, oy), player.viewer_pan, window.scale_factor());
    Some(frame.0 - ox)
}

/// The program picture and, while the angle monitor is on, the monitor. The monitor sits over
/// the empty canvas left of the picture, so the picture keeps its exact geometry. Where that
/// strip is narrower than [`MONITOR_STRIP_MIN`], the monitor is a flex sibling of the picture
/// instead, and the picture makes room for it.
fn program_and_monitor(player: &mut Player, position: f64, window: &mut Window, cx: &mut Context<Player>) -> Vec<AnyElement> {
    let program = match player.source_hint && player.source_target.is_none() && player.preview_session.is_none() {
        true => source_empty().into_any_element(),
        false => player.picture_area(position, window, cx).into_any_element(),
    };
    if !(player.angle_monitor && player.preview_session.is_none()) {
        return vec![program];
    }
    let strip = monitor_strip(player, window).filter(|s| *s >= MONITOR_STRIP_MIN);
    let Some(monitor) = player.angle_monitor_view(cx, if strip.is_some() { 2 } else { 3 }) else {
        return vec![program];
    };
    match strip {
        Some(strip) => {
            let overlay = div()
                .absolute()
                .top(px(VIEWER_PAD))
                .bottom(px(VIEWER_PAD))
                .left(px(VIEWER_PAD))
                .w(px(strip - 2. * VIEWER_PAD))
                .child(monitor);
            vec![program, overlay.into_any_element()]
        }
        None => vec![monitor, program],
    }
}

/// Fixed-height strip under the screen: timecode leads, the transport sits
/// centred, the cut readout closes the row (DESIGN §5). Frame only -- every
/// row lives in [`crate::ui::timeband_stance`], the same split
/// `bench_stance`/`dock_stance` already make for their regions.
fn time_band(player: &mut Player, position: f64, cx: &mut Context<Player>) -> impl IntoElement {
    // The position comes from `render`, the same one the screen and the ledger
    // read. Asking `active_session` here instead read the PREVIEW's clock while
    // a preview was up, so the hero timecode and the ledger's position -- two
    // readouts of one truth -- disagreed on screen.
    div()
        .id("stance-time-band")
        .relative()
        .flex_none()
        .h(px(TIME_BAND_H))
        .bg(rgb(crate::ui::theme::tokens().e1))
        .border_t_1()
        .border_color(rgba(crate::ui::theme::tokens().line))
        .flex()
        // Measured, not computed: the record sheet hangs just above this band.
        .child(crate::ui::anchor::probe("record-band"))
        .child(timeband_stance::render(player, position, cx))
}

/// Lanes, under the time band. `canvas` is the bench background too (DESIGN
/// §2's token table), so it shares its surface with the screen above it.
/// Height comes from [`Player::split_px`] now, not the fixed [`BENCH_H`]:
/// the seam [`crate::ui::stance::render`] mounts above this region is what
/// answers "ui fields are not stretchable" for the bench.
fn bench(
    player: &mut Player,
    bench_h: f32,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let focused = player.focus_bench.is_focused(window);
    bench_stance::set_window_h(f32::from(window.viewport_size().height));
    div()
        .id("stance-bench")
        .track_focus(&player.focus_bench)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            if is_focus_cycle_key(key) {
                let next = next_surface(Surface::Bench, event.keystroke.modifiers.shift);
                this.focus_surface(next, window, cx);
                cx.stop_propagation();
            } else if is_focus_exit_key(key) && !this.any_text_field() {
                // A dismissible overlay (context menu, library menu, picker,
                // any card, a live preview) opened by a click into the bench
                // takes this escape first -- the same door the root handler
                // itself closes it through (`Player::escape_closes_overlay`)
                // -- so it does not go on sitting open just because focus
                // happened to be inside this ring when escape was pressed.
                if this.escape_closes_overlay(cx) {
                    cx.stop_propagation();
                    return;
                }
                // Leaves the ring for the root and lets the stroke bubble on to it: the root
                // answers escape as Deselect (the caption, title and clip selections go).
                window.focus(&this.focus);
                cx.notify();
            }
        }))
        .flex_none()
        .h(px(bench_h))
        // The toast stack hangs off this box (DESIGN §8), so it is the anchor.
        .relative()
        // §4: lanes take 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        .border_t_1()
        // DESIGN §4.10: the focused region's top edge is the 1px ink2 rule,
        // recoloured in place so no size moves. No ring or outline: a region
        // is never outlined for holding focus (user 2026-09-10, "clicking
        // through timeline draws a white overlay around the timeline"). Tab
        // and Shift-Tab still land here ([`next_surface`]); the only ring left
        // in the bench is the selection one a picked clip draws (DESIGN §2/§4).
        .border_color(if focused {
            rgb(INK2())
        } else {
            rgba(DARK_SEAM())
        })
        .flex()
        .flex_col()
        // The 12px side gutter is spent INSIDE `bench_stance`'s own content
        // box, not here: gpui fires `on_drop` only on an element whose own
        // hitbox holds the pointer (`div.rs:2089`), and padding on this
        // parent left the gutter outside every registered target -- a clip
        // let go at x < 12, hard against the left edge where a hand aiming
        // at frame 0 puts it, reached nothing at all and moved nothing,
        // silently (DESIGN §8: a release that has aimed has made an edit).
        // Same box, same pixels; only the catch-all's bounds grow.
        // The edit toolbar tops the box (DESIGN §6.4), so its height is spent
        // here out of `bench_h` and the lanes get what is left -- the same
        // `TOOLBAR_H` the bench's floor (`BENCH_MIN_H`) already counts.
        .child(toolbar::render(player, cx))
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .py(px(4.))
                .child(bench_stance::render(player, cx)),
        )
}

/// DESIGN §8's toasts: ONE at a time, the newest notice, shown in the status
/// bar ([`ledger`]) in place of its readout. Never over the timeline or the
/// picture, and the bar's height is fixed, so no toast moves anything.
///
/// One toast: a 3px stripe in its tone, the line in INK1, the button its action
/// wears as a ghost, and a ✕. An info or warn toast starts its hide timer the
/// first frame it is drawn ([`Toast::timer_armed`]); the timer names the toast
/// by id, so a late one finds nothing and hides nothing.
/// The widest a toast grows in the status bar before its text is elided.
pub(crate) const TOAST_MAX_W: f32 = 760.;

/// REDESIGN §2's region heights for the stance, as new names (P3 adopts them
/// and retires the legacy constants above). The shared definitions live in
/// [`crate::layout::redesign`]; these four are the ones this module owns.
#[allow(dead_code, reason = "defined by P1, adopted by P3")]
pub(crate) mod redesign {
    use crate::layout::redesign as r;
    pub(crate) const TIME_BAND_H: f32 = r::TRANSPORT_H;
    /// The bench (timeline panel) default share of the window height.
    pub(crate) const BENCH_SHARE: f32 = r::TIMELINE_SHARE;
    pub(crate) const LEDGER_H: f32 = r::STATUS_H;
    pub(crate) const DOCK_W: f32 = r::DOCK_W;
}

/// The words a toast shows in its text box: the message, cut with an ellipsis to the width
/// the box had on the last frame. The box is measured with a probe (as the readout is), so the
/// cut is the one the strip paints. Before the first measurement the whole message is given and
/// the box's own clip holds it; the probe asks for one more frame when its width changes.
fn toast_words(text: &str, text_box: &Rc<Cell<Bounds<Pixels>>>, cx: &App) -> String {
    let words = ledger_line(text);
    // The probe covers the box with its 8 px side padding on each side.
    // At least the room a toast can have: a box sized by its own text (a toast with no buttons)
    // would otherwise never grow past the cut it was last given.
    let width = (f32::from(text_box.get().size.width) - 16.).max(TOAST_MAX_W - 120.);
    if width <= 0. {
        return words;
    }
    bench_stance::clip::fit_label(cx, &words, width).unwrap_or_default()
}

/// A canvas that records the toast text box's bounds for [`toast_words`], and asks for
/// another frame when the width changed, so the next frame cuts to the new width.
fn toast_text_probe(into: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            if into.replace(bounds).size.width != bounds.size.width {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| (),
    )
    .absolute()
    .size_full()
}

fn toast_card(toast: &Toast, text_box: &Rc<Cell<Bounds<Pixels>>>, cx: &mut Context<Player>) -> gpui::Stateful<gpui::Div> {
    let id = toast.id;
    if toast.hides() && !toast.timer_armed.replace(true) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_HIDE).await;
            let _ = this.update(cx, |this, cx| this.dismiss_toast(id, cx));
        })
        .detach();
    }
    let t = crate::ui::theme::tokens();
    let (glyph, tone_ink) = match toast.tone {
        Tone::Info => (crate::ui::icons::Icon::Info, t.ink3),
        Tone::Warn => (crate::ui::icons::Icon::TriangleAlert, t.warn),
        Tone::Err => (crate::ui::icons::Icon::CircleX, t.err),
    };
    let ghost = |label: &'static str| {
        div()
            .flex_none()
            .h(px(18.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .type_style(type_scale::caption_strong())
            .text_color(rgb(t.ink2))
            .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)).text_color(rgb(crate::ui::theme::tokens().ink1)))
            .cursor_pointer()
            .child(label)
    };
    div()
        .id(SharedString::from(format!("stance-toast-{id}")))
        // Shrinks only when the strip itself runs out (the format and verbs on
        // the right keep their width): an ordinary message shows whole.
        .flex_shrink()
        .min_w(px(0.))
        .max_w(px(TOAST_MAX_W))
        .h(px(LEDGER_H - 2.))
        .flex()
        .items_center()
        .overflow_hidden()
        .rounded(px(6.))
        .bg(rgb(t.e3))
        .border_1()
        .border_color(rgba(t.line_hi))
        .pl(px(8.))
        .child(glyph.el(crate::ui::icons::IconSize::Inline, tone_ink))
        .child(
            div()
                .id(SharedString::from(format!("stance-toast-text-{id}")))
                .flex_grow().flex_shrink()
                .min_w(px(0.))
                .px(px(8.))
                // Cut short in the strip, whole on hover.
                .tooltip(crate::ui::widgets::tip_hover(&toast.text, "", None))
                // Ellipsis, not a bare cut: the name inside the message is
                // elided through its middle first ([`ledger_line`]) so the
                // state word at the front is never what gets dropped. Clipped
                // at its own edge too, so the words never paint over the buttons.
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(t.ink1))
                .relative()
                .child(toast_words(&toast.text, text_box, cx))
                .child(toast_text_probe(text_box.clone())),
        )
        .children(toast.actions.iter().enumerate().map(|(index, action)| {
            let label = match action {
                ToastAction::Undo => "Undo",
                ToastAction::Play(_) => "Play",
                ToastAction::ShowInFolder(_) => "Show in folder",
                ToastAction::Unlock(_) => "Unlock",
                ToastAction::Reload => "Reload",
                ToastAction::KeepMine => "Keep mine",
                ToastAction::Recover => "Recover",
                ToastAction::Discard => "Discard",
                ToastAction::SaveThenOpen(_) => "Save",
                ToastAction::OpenDiscarding(_) => "Don't save",
            };
            ghost(label)
                .id(SharedString::from(format!(
                    "stance-toast-action-{id}-{index}"
                )))
                .mx(px(6.))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toast_action(id, index, cx)
                    }),
                )
        }))
        .child(
            div()
                .id(SharedString::from(format!("stance-toast-close-{id}")))
                .flex_none()
                .w(px(28.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(crate::ui::icons::Icon::X.el(crate::ui::icons::IconSize::Inline, t.ink3))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.dismiss_toast(id, cx)),
                ),
        )
}

/// The clip readout the ledger's left side shows while a clip is selected
/// (DESIGN §6.9): the source's stem, the length in seconds, and where the clip
/// starts as a m:ss span. The transport already shows the timecode.
/// `city_dusk · 9.5 s · starts at 0:00`.
pub(crate) fn clip_readout(name: &str, start: u32, frames: u32, fps: f64) -> String {
    format!(
        "{name} · {:.1} s · starts at {}",
        f64::from(frames) / fps,
        bench_stance::mmss(f64::from(start) / fps),
    )
}

/// A frame rate the way the status bar writes it: `30`, or `29.97` where the
/// rate is fractional.
/// `1920×1080 (1080p)`: the size with its common name when it has one, else just `W×H`.
pub(crate) fn size_label(w: u32, h: u32) -> String {
    let name = match (w, h) {
        (1280, 720) => Some("720p"),
        (1920, 1080) => Some("1080p"),
        (2560, 1440) => Some("1440p"),
        (3840, 2160) => Some("4K"),
        _ => None,
    };
    match name {
        Some(n) => format!("{w}×{h} ({n})"),
        None => format!("{w}×{h}"),
    }
}

pub(crate) fn fps_label(fps: f64) -> String {
    format!("{fps:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// The gap readout the ledger's left side shows while a gap is picked (DESIGN
/// §6.9): the hole's length in seconds and where it starts as a m:ss span.
/// `Gap · 2.4 s · starts at 0:04`.
pub(crate) fn gap_readout(start: u32, frames: u32, fps: f64) -> String {
    format!(
        "Gap · {:.1} s · starts at {}",
        f64::from(frames) / fps,
        bench_stance::mmss(f64::from(start) / fps),
    )
}

/// The readout the ledger's left side shows while nothing is selected or
/// picked (DESIGN §6.9 left blank then; the audit asked for the film's own
/// length instead of nothing): the whole timeline's length as a m:ss span, and
/// how many clips make it up. `Timeline 1:50 · 20 clips`; an empty timeline says so.
pub(crate) fn project_readout(frames: u32, clips: usize, fps: f64) -> String {
    if clips == 0 {
        return "Empty timeline · drag media here to start".to_string();
    }
    format!(
        "Timeline {} · {clips} {}",
        bench_stance::mmss(f64::from(frames) / fps),
        if clips == 1 { "clip" } else { "clips" },
    )
}

/// The width of the status bar's readout slot.
const READOUT_W: f32 = 300.;
/// The slot while a trim says what it is doing: `Trimming both sides · Start −0.5 s · End +0.5 s`.
const TRIM_LINE_W: f32 = 420.;

/// Thin strip at the bottom of the centre column (DESIGN §6.9): the selected
/// clip or picked gap (else the timeline's length) and export progress on the left, the picture's format
/// and the two room verbs on the right. Project identity and save state live
/// in the menu bar. Notices rise from here (DESIGN §5, §8).
fn ledger(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let fps = player.active_fps();
    // A selected clip, else a picked gap. `Clip` is `Copy`, so the lookup
    // hands back the clip itself and not a borrow of the session. A subtitle
    // pick has no `Clip` and reads nothing here.
    let selected = player
        .session
        .as_ref()
        .zip(player.selected.anchor())
        .filter(|(_, (lane, _))| lane.kind != LaneKind::Subtitle)
        .and_then(|(s, (lane, idx))| s.lane_clips(lane).get(idx).cloned());
    // One toast at a time, in this strip: it takes the readout's place for as
    // long as it lives, so nothing is ever drawn over the timeline.
    let toast = crate::notices::current_toast(&player.notices).map(|t| toast_card(t, &player.toast_text, cx));
    // A trim in hand, or a roll armed, says what it is doing in this strip in place of the
    // selection's readout (and the pointer's hint), so the transport band never changes.
    let trimming = crate::ui::timeband_stance::trim_text(player).or_else(|| bench_stance::clip::trim_in_flight(player));
    let trimming_now = trimming.is_some();
    let readout = selected
        .map(|c| {
            let name = player
                .sources()
                .get(c.source)
                .map_or_else(|| "clip".to_string(), |s| stem(&s.path));
            clip_readout(&name, c.start, c.len(), fps)
        })
        .or_else(|| {
            player
                .gap_pick()
                .map(|g| gap_readout(g.start, g.frames, fps))
        })
        // Nothing selected: the film's own length and clip count, so the
        // strip never sits empty while a project is open.
        .or_else(|| {
            player.session.as_ref().map(|s| {
                let frames = (s.timeline_duration() * fps).round() as u32;
                let clips = s.lanes().iter().map(|&l| s.lane_clips(l).len()).sum();
                project_readout(frames, clips, fps)
            })
        });
    let readout = trimming.or(readout);
    // What the file actions have to say is no longer read here: it rises as
    // toasts above this strip (DESIGN §8), so the strip keeps
    // its readout, export progress and the room's verbs only.
    let export = player
        .exporting()
        .map(|h| match h.stage() {
            Some((name, f)) => format!("{name} {:.0}%", f * 100.),
            None => format!("Exporting {:.0}%", h.progress() * 100.),
        })
        .or_else(|| player.reframe_words())
        .or_else(|| player.mcam_words())
        .or_else(|| player.render_words());
    div()
        .id("stance-ledger")
        .flex_none()
        .h(px(LEDGER_H))
        .bg(rgb(crate::ui::theme::tokens().e1))
        .border_t_1()
        .border_color(rgba(crate::ui::theme::tokens().line))
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(12.))
        // The readout reads first, on the left: the same mono strip, so it is
        // a status and not a dialog.
        .children(toast)
        .children(readout.filter(|_| player.notices.is_empty()).map(|r| {
            div()
                .relative()
                .child(bounds_probe(player.ledger_readout.clone()))
                .flex_none()
                // A fixed slot: the readout changes with every selection, and what follows it
                // (the drag hint) must not move with it.
                .w(px(if trimming_now { TRIM_LINE_W } else { READOUT_W }))
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(INK2()))
                .child(r)
        }))
        .children(export.map(|e| {
            div()
                .flex_none()
                // `Exporting 5%` to `Exporting 100%`: held at the widest.
                .w(px(13. * bench_stance::char_w(type_scale::CHORD_METADATA_MIN_PX)))
                .whitespace_nowrap()
                .type_style(type_scale::caption())
                .text_color(rgb(INK2()))
                .child(e)
        }))
        // The pointer's drag hint, or the drag in flight and how far it has
        // gone (`bench_stance::clip::drag_status`): here, in the strip, so
        // nothing floats over the lanes to say it.
        .children((!trimming_now).then(|| bench_stance::clip::drag_status(player)).flatten().map(|h| {
            div()
                .flex_shrink()
                .min_w(px(0.))
                .truncate()
                .type_style(type_scale::caption())
                .text_color(rgb(INK2()))
                .child(h)
        }))
        .child(div().flex_1().min_w(px(0.)))
        // The dropped-frames words, at the right edge in a slot as wide as their longest
        // form: they come and go without moving anything else in the strip.
        .child(
            div()
                .flex_none()
                .w(px(40. * bench_stance::char_w(type_scale::CHORD_METADATA_MIN_PX)))
                .overflow_hidden()
                .whitespace_nowrap()
                .type_style(type_scale::caption())
                .text_color(rgb(INK2()))
                .child(player.dropped_words().unwrap_or_default()),
        )
}

/// The dock: the only side panel, right, fixed width, carrying the Src/Clip
/// tab pair (DESIGN §5). The frame is drawn here; `dock_stance.rs` owns the
/// tab row and both tabs' content (DESIGN §12 step 4).
/// Width comes from [`Player::split_px`] now, not the fixed [`DOCK_W`]: the
/// seam [`crate::ui::stance::render`] mounts to its left is what answers
/// "ui fields are not stretchable" for the dock. `window_size` is threaded
/// through so the maximize-in-place cards inside the dock (`dock_stance.rs`)
/// see the real viewport rather than a fabricated `size(px(DOCK_W), h)`.
fn dock(
    player: &Player,
    dock_w: f32,
    window_size: Size<Pixels>,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // Either dock tab's handle counts: Sources and Clip are one region (§6.7).
    let focused = player.focus_dock.is_focused(window) || player.focus_inspector.is_focused(window);
    div()
        .id("stance-dock")
        // The library's own answer to a file dragged in from outside: the
        // same raised fill every other drop target in the darkroom lifts to
        // (DESIGN §8's affordance, no plate and no copy). The drop itself is
        // heard by the room root (`render`), which covers this panel too.
        .drag_over::<gpui::ExternalPaths>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
        .flex_none()
        .w(px(dock_w))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_l_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .flex_col()
        .child(dock_stance::render(player, dock_w, window_size, window, cx))
        // DESIGN §4.10: the focused region's top rule, laid over the panel's
        // first pixel row so no size moves.
        .when(focused, |el| el.child(focus_top_rule()))
}

/// The 1px ink2 top rule that marks a focused region (DESIGN §4.10). Absolute,
/// so it paints over the region's top edge without taking space in its flow.
fn focus_top_rule() -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(1.))
        .bg(rgb(INK2()))
}

/// A zero-size element whose only job is `window.on_mouse_event`'s door:
/// `Interactivity`'s fluent `on_mouse_*` builders (the ones `render`'s root
/// div uses for the drag itself, just below) have no `MouseExitEvent` case,
/// so this is the lowest rung that reaches it -- registered fresh every
/// frame, the same way `canvas()` is already used elsewhere in this crate
/// for paint-time access `div()` does not expose. See
/// [`Player::drag_left_window`] for why this event, and not a release, is
/// what a seam drag ending outside the window is saved on.
fn mouse_exit_listener(cx: &mut Context<Player>) -> impl IntoElement {
    let player = cx.entity();
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
                if phase == gpui::DispatchPhase::Bubble {
                    player.update(cx, |this, cx| this.drag_left_window(cx));
                }
            });
        },
    )
    .absolute()
    .size(px(0.))
}

/// The whole stance: screen, time band, bench, ledger, dock, in the
/// order DESIGN §5 draws them, over the same key handler the legacy tree
/// uses (DESIGN §12 step 3): the darkroom draws its own regions but answers
/// to the one keymap.
pub(crate) fn render(
    player: &mut Player,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    // The window, and the room below the menu bar (DESIGN §6.1): the regions lay
    // out against the room; only the root's own floating cards take the window.
    let viewport = window.viewport_size();
    let window_size = size(viewport.width, viewport.height - px(MENU_BAR_H));
    // Dialogs: a fresh frame of tab stops, and the keyboard goes to the root whenever one opens
    // (whatever field or control held it before), so Esc/Enter/Tab are always heard.
    crate::ui::kit::tab_frame_reset();
    let pool = player.dlg_pool.clone();
    let dialog_up = player.dialog_up();
    if dialog_up != player.dlg_was_up {
        player.dlg_was_up = dialog_up;
        if dialog_up {
            window.focus(&player.focus);
        }
    }
    let position = player.playhead(player.drawn_duration());
    // The two seams a hand may drag (`Split::Dock`, `Split::Bench`): read
    // through the same door every legacy region measures itself by
    // ([`Player::split_px`]), so a dragged size and a drawn one cannot
    // disagree here either.
    let dock_w = player.split_px(Split::Dock, window_size);
    let bench_h = player.split_px(Split::Bench, window_size);
    div()
        .id("stance-room")
        // Inter is the default face for any text that sets no role of its own.
        .font_family(type_scale::INTER)
        .track_focus(&player.focus)
        // A press anywhere leaves a text field first; the field's own click then reopens it.
        .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
            // A click puts the ring away: it is the keyboard's.
            crate::ui::kit::set_kbd_ring(false);
            if this.any_text_field() {
                this.blur_text_fields(window, cx);
            }
        }))
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            let ctrl = event.keystroke.modifiers.control;
            let shift = event.keystroke.modifiers.shift;
            let alt = event.keystroke.modifiers.alt;
            if event.is_held
                && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl, shift, alt))
            {
                return;
            }
            // Escape also cancels a clip drag in the air: a drag let go with no
            // target sends no release that clears its dimmed source, snap line
            // and shadow. Falls through, so escape still closes what it did.
            if key == ESCAPE {
                this.end_drag_preview(cx);
            }
            // Esc shuts an open menu-bar dropdown first (DESIGN §4.12).
            if this.menu_open.is_some() && key == "escape" {
                this.menu_open = None;
                cx.notify();
                return;
            }
            // The clip menu's Adjust submenu: Right opens it, Left closes it.
            if let (Some(m), "left" | "right") = (this.context_menu.as_mut(), key)
                && matches!(m.on, MenuOn::Clip(_))
            {
                m.adjust = key == "right";
                cx.notify();
                return;
            }
            // Every dialog: Tab walks its controls, a focused control keeps Enter and Space.
            if this.dialog_up() && this.dialog_nav_key(key, shift, window, cx) {
                return;
            }
            // The caption font list's search line takes the letters typed into it before the keymap.
            if this.sub_font_key(key, ctrl, alt, event.keystroke.key_char.as_deref(), cx) {
                return;
            }
            // An open kit dropdown list answers Up, Down and Enter before anything else.
            if !ctrl && !alt && crate::ui::kit::list_nav(key, window, cx) {
                cx.notify();
                return;
            }
            // The About dialog: Esc or Enter closes it, and it takes no other key.
            if this.about_open {
                if matches!(key, "escape" | "enter") {
                    this.about_open = false;
                    cx.notify();
                }
                return;
            }
            // Left and Right walk the open menus, wrapping at the ends.
            if let (Some(open), "left" | "right") = (this.menu_open, key) {
                let n = crate::ui::menubar::Title::ALL.len();
                let at = crate::ui::menubar::Title::ALL
                    .iter()
                    .position(|t| *t == open)
                    .unwrap_or(0);
                let to = match key == "right" {
                    true => (at + 1) % n,
                    false => (at + n - 1) % n,
                };
                this.menu_open = Some(crate::ui::menubar::Title::ALL[to]);
                cx.notify();
                return;
            }
            // Go to time's field on the transport owns the keyboard while it is typed into.
            if this.time_edit.is_some() {
                this.time_edit_key(key, ctrl, alt, event.keystroke.key_char.as_deref(), cx);
                return;
            }
            // The File menu's project dialogs own the keyboard while one is up.
            if this.proj_dialog.is_some() {
                this.project_dialog_key(key, ctrl, alt, event.keystroke.key_char.as_deref(), cx);
                return;
            }
            // The unsaved-work question (user 2026-09-10: "if timeline is not
            // empty and user does alt+f4 or exit somehow, ask user to save the
            // file if it's not already saved"). Asked ahead of every branch
            // below because while the plate is up it owns the keyboard: three
            // answers and nothing else -- `↵` saves and goes, `d` goes without
            // saving, `esc` stays, any other key does nothing at all.
            if this.quit_ask {
                match key {
                    // Nothing to save, only exports to lose: Enter keeps the window, never closes it.
                    "enter" if !this.close_dirty() => {
                        this.quit_ask = false;
                        cx.notify();
                    }
                    "enter" => {
                        // The Save action's own door, so the notice, the
                        // sidecar cleanup and the dirty flag are the ones a
                        // ctrl+s writes. The window goes only once the file is
                        // written: a failed save says why and keeps the work.
                        // corner-cut: the overwrite guard is Save's door (`Player::save_guarded`), not this plate's -- this save was already answered.
                        if this.save_project(cx) {
                            window.remove_window();
                        }
                    }
                    "d" => {
                        // Don't save: the edits' autosave goes too, so the next open
                        // does not offer them back as recovery.
                        if this.close_dirty() {
                            this.discard_unsaved_edits();
                        }
                        this.queue.cancel_all();
                        window.remove_window();
                    }
                    "escape" => {
                        this.quit_ask = false;
                        cx.notify();
                    }
                    _ => {}
                }
                return;
            }
            // A chord that is the app's, not text editing (Ctrl+K, Alt+T, Ctrl+Y ...), is never
            // eaten by a field: the field is left and the chord runs.
            if crate::player::blur::is_app_chord(key, ctrl, shift, alt) && this.any_text_field() {
                this.blur_text_fields(window, cx);
            }
            // The overwrite question (user 2026-09-11: saving over an
            // existing file must warn first, "same pattern as the exit
            // guard"). Raised only by Save's own door
            // ([`Player::save_guarded`]) -- the target exists on disk and
            // `autosave_armed` says this window never loaded or wrote it --
            // so the ledger's unsaved ghost routes through the same ask.
            // Same ownership as the plate above: two answers and nothing
            // else -- `↵` overwrites and saves, `esc` keeps the file, any
            // other key does nothing at all.
            if this.save_ask {
                match key {
                    "enter" => {
                        // The answer IS the confirmation, so this goes
                        // straight to the unconditional write and never
                        // back through the guard; [`Player::save_project`]
                        // arms `autosave_armed`, making later ^s on this
                        // file silent again.
                        this.save_project(cx);
                        this.save_ask = false;
                        cx.notify();
                    }
                    "escape" => {
                        this.save_ask = false;
                        cx.notify();
                    }
                    _ => {}
                }
                return;
            }
            // The export's own overwrite question (user 2026-09-11: "not just
            // save but export too"), asked inside the export sheet with three
            // buttons. Its keys: `↵` replaces, `k` keeps both under a new
            // name, `esc` keeps the file and goes back to the sheet; any
            // other key does nothing at all ([`Player::answer_overwrite`]).
            if this.export_ask {
                match key {
                    "enter" => this.answer_overwrite(Overwrite::Replace, cx),
                    "k" => this.answer_overwrite(Overwrite::KeepBoth, cx),
                    "escape" => this.answer_overwrite(Overwrite::Cancel, cx),
                    _ => {}
                }
                return;
            }
            // Every open text field takes the keyboard here, in one place (`Player::text_field_key`):
            // a letter, Backspace, Ctrl+A, the arrows and Escape go into the field, and none of them
            // reaches the palette, the chords or the timeline. Only an app chord (Ctrl+K, Alt+T ...)
            // leaves the field, and then runs.
            if this.text_field_key(event, window, cx) {
                return;
            }
            // The command palette (DESIGN §4.4) owns the keyboard while it is
            // up: letters and digits go to its field, the arrows move the
            // highlight, enter runs the row. Ctrl+K again and escape both shut
            // it; nothing else reaches the keymap meanwhile.
            if crate::ui::gallery::is_open() {
                if key == "escape" {
                    crate::ui::gallery::set_open(false);
                    cx.notify();
                }
                return;
            }
            if this.palette_open {
                let toggles =
                    this.keymap.lookup(key, ctrl, shift, alt) == Some(ActionId::ShowActions);
                match key {
                    _ if toggles || key == "escape" => this.palette_open = false,
                    "up" => this.palette_move(-1),
                    "down" => this.palette_move(1),
                    "enter" => this.palette_run(window, cx),
                    "backspace" => {
                        this.palette_query.pop();
                        this.palette_sel = 0;
                        this.palette_scroll.set_offset(point(px(0.), px(0.)));
                    }
                    _ => {
                        if let Some(c) = typed(key).filter(|_| !ctrl) {
                            this.palette_query.push(c);
                            this.palette_sel = 0;
                            this.palette_scroll.set_offset(point(px(0.), px(0.)));
                        }
                    }
                }
                cx.notify();
                return;
            }
            // DESIGN §10.2: `?` and F1 are the ShowKeys action, which swings the dock to its
            // KEYS tab and a second `?` swings it back -- no plate, no hold,
            // no modal cheat-sheet. Asked ahead of the modal guard below so
            // the stroke is heard whatever else is up, and toggling here
            // rather than latching is what makes the same key the way out.
            // `?` is shift+`/` on a US board, and a layout that reports the
            // unshifted key name still means the same stroke.
            if this.keymap.lookup(key, ctrl, shift, alt) == Some(ActionId::ShowKeys)
                || (key == "/" && shift && !ctrl && !alt)
            {
                match this.keys_open {
                    true => this.keys_open = false,
                    false => this.show_keys(cx),
                }
                cx.notify();
                return;
            }
            // The other way back to the tab underneath (the tab strip being
            // the third): only when no card is up, so escape still shuts a
            // card first -- the KEYS tab is a dock tab now, not an overlay,
            // and nothing about it owns the keyboard.
            if this.keys_open && !this.card_open() {
                if key == "escape" {
                    if this.keys_filter.is_empty() {
                        this.keys_open = false;
                    } else {
                        this.keys_filter.clear();
                    }
                    cx.notify();
                    return;
                }
                if key == "backspace" {
                    this.keys_filter.pop();
                    cx.notify();
                    return;
                }
                if let Some(c) = typed(key).filter(|_| !ctrl && !alt) {
                    this.keys_filter.push(c);
                    cx.notify();
                    return;
                }
            }
            // The Media panel's own keys (DESIGN §4, §6.7): with the panel
            // focused and a row picked, Space previews it and `↵` adds it.
            // Ahead of the keymap, so Space's Play never sees the stroke
            // there; with nothing picked or a menu/card up, it falls through.
            let panel_focused = this.focus_inspector.is_focused(window);
            if !this.overlaid()
                && this.transcript_key(key, ctrl, shift, alt, panel_focused, cx)
            {
                return;
            }
            let media_focused = this.dock_src_active && this.focus_dock.is_focused(window);
            if !this.overlaid()
                && let Some(item) = dock_stance::media_key(
                    key,
                    event.keystroke.modifiers,
                    media_focused,
                    this.selected_asset.is_some(),
                )
            {
                this.act_on_picked_row(item, cx);
                return;
            }
            // `↵` adds the picked source at the playhead (MOCK-SPEC "Dock"
            // hint paragraph, gesture 2; `dock_stance.rs`'s own hint promised
            // this and nothing answered it -- FAULT 1). Live only where a row
            // is actually picked, so a bare enter elsewhere still means
            // whatever it means below. `insert_source` already refuses
            // visibly on its own ("Nothing added: ...") when the span is
            // occupied, so there is nothing further to paint here.
            if key == "enter"
                && this.dock_src_active
                && this.selected_asset.is_some()
                && this.exporting().is_none()
            {
                if let Some((path, stream)) = this.selected_asset.clone() {
                    this.append_source(&path, stream, cx);
                }
                cx.notify();
                return;
            }
            // A card is not a key-eating black box. The room-wide chords that
            // act on the document rather than on the card -- the edit history,
            // the project file, the clipboard ([`card_safe_action`]) -- run
            // here, ahead of the cards' own key branches below and ahead of the
            // blanket modal guard under them, so an open card cannot swallow
            // the stroke a hand reaches for while looking at what it changed.
            // `param_card_key` answers `true` for every stroke once a card is
            // open, so this has to stand in front of *it*, not only in front of
            // the guard: `ctrl+z` never reached `keymap.lookup` at all, and the
            // card's own comment says undo was always meant to be undo's key.
            //
            // Asked through the keymap rather than as a table of key literals:
            // a rebind moves the chord and this follows.
            if let Some(action) = this.keymap.lookup(key, ctrl, shift, alt)
                && card_safe_action(action)
            {
                this.act(action, window, cx);
                return;
            }
            // A docked card is not a modal: the transport and the room's own
            // doors answer while it shows ([`docked_card_safe_action`]).
            if this.docked_card_only()
                && let Some(action) = this.keymap.lookup(key, ctrl, shift, alt)
                && (docked_card_safe_action(action)
                    || docked_card_ignores(key, ctrl, shift, alt))
            {
                this.act(action, window, cx);
                return;
            }
            // The Export tab is a tab, not a sheet: the range it exports is marked on the
            // timeline with I and O, so those strokes (and the transport's) still answer.
            if this.export_open
                && this.exporting().is_none()
                && !this.export_ask
                && this.context_menu.is_none()
                && this.library_menu.is_none()
                && this.picker.is_none()
                && let Some(action) = this.keymap.lookup(key, ctrl, shift, alt)
                && export_tab_action(action)
            {
                this.act(action, window, cx);
                return;
            }
            // An open dropdown list: up and down move its highlighted row, enter picks it, escape
            // (above) closes only the list; any other key still closes it.
            if let Some(pk) = this.picker
                && !ctrl
                && !alt
                && matches!(key, "up" | "down" | "enter")
                && this.context_menu.is_none()
                && this.library_menu.is_none()
            {
                let rows = this.choices(pk.of);
                let live: Vec<usize> = (0..rows.len()).filter(|&n| this.export_choice_refusal(rows[n].0).is_none()).collect();
                if let Some(at) = live.iter().position(|&n| n == pk.sel).or((!live.is_empty()).then_some(0)) {
                    match key {
                        "enter" => this.choose(rows[live[at]].0, cx),
                        _ => {
                            let to = crate::ui::choice::step(live.len(), at, key == "down");
                            if let Some(p) = this.picker.as_mut() {
                                p.sel = live[to];
                            }
                            cx.notify();
                        }
                    }
                    return;
                }
            }
            // The param cards' own key branches (arrows nudge the focused
            // slider, digits pick an EQ band, `r` resets, card verbs) --
            // `Player::param_card_key` (`player/cards.rs`), the same
            // function `render.rs`'s legacy handler now calls too, so this
            // is not a second copy of that logic (DESIGN §6: "drag while
            // playing... `r` resets"). Asked before the generic modal guard
            // below so a card open in the darkroom is not just a key-eating
            // black box the way the old uniform guard made it.
            if this.param_card_key(key, event.keystroke.modifiers.shift, cx) {
                cx.notify();
                return;
            }
            // One rule: a docked card eats only the strokes it reads
            // (`param_card_key` above answered `false` for this one); every
            // other stroke belongs to the room's keymap. Only a sheet that
            // covers the room, or a menu, blocks the keymap outright.
            if this.docked_card_only()
                && this.context_menu.is_none()
                && this.library_menu.is_none()
                && this.picker.is_none()
            {
                let hit = this.keymap.lookup(key, ctrl, shift, alt);
                if let Some(action) = hit {
                    this.act(action, window, cx);
                }
                return;
            }
            // Same modal guard the legacy handler's keys_open/export_open/
            // colour/transform/speed/EQ/silence/mix/subtitle-style/menu
            // chain (`render.rs`) amounts to, read as one bool instead of
            // re-copied field by field: while a card or a menu owns the
            // keyboard, the darkroom stance yields to it exactly as the
            // legacy tree does. This is the fix for the stance's
            // `Home`-inside-the-Colour-card misfire.
            //
            // `exporting()` alone is deliberately left out of this list
            // (`Player::card_open` is `modal()` minus it): a running export
            // draws no card of its own to own the keyboard for, and a key
            // caught here never reaches `enable()`/`act()` below, so a
            // refusal the oracle would have spoken (`ActionId::Settings`
            // among them) never got said -- the room went silent instead.
            // Letting an export-only moment fall through costs nothing this
            // guard exists for: every action the oracle would still refuse
            // during an export says so through `act()`'s own `Enable::No`
            // branch, and the six it leaves live (Theme, Fullscreen,
            // SubtitleStyle, CancelExport, ...) are exactly the ones meant
            // to keep working.
            if this.card_open()
                || this.context_menu.is_some()
                || this.cue_menu.is_some()
                || this.library_menu.is_some()
                || this.picker.is_some()
            {
                // Escape is every card's own way out (`Player::close_card`,
                // the same list `overlaid`/`modal` read) -- blocking every
                // key while a card is open must not also lock the card open.
                // `escape_closes_overlay` (`player/cards.rs`) is the same
                // call a focus surface's own escape branch makes first, so
                // this and that never disagree on what escape closes.
                if key == ESCAPE {
                    this.escape_closes_overlay(cx);
                    return;
                }
                // FAULT 2, general shape: `context_menu`/`library_menu`/
                // `picker` are three of the four reasons `overlaid()` refuses
                // every key, and none of the three is in `close_card`'s own
                // list (`modal()` never counted them) -- left uncleared, any
                // one of them left the room an invisible, permanent modal
                // that only escape even tried to leave, and closed nothing.
                // Any key answers a menu, the same rule the legacy tree's own
                // clip_menu/row_menu/list chain follows (`render.rs`): this is
                // the fix for "no state may make the room modal without
                // showing anything", not only the source-dot's menu.
                //
                // Except the document chords, which were answered *above* and
                // left the menu standing: a menu is a visible surface with
                // escape and any other key still shutting it, and losing an
                // undo to a square of the screen a right-click left open would
                // be the black box this handler no longer is.
                this.context_menu = None;
                this.cue_menu = None;
                this.library_menu = None;
                this.picker = None;
                cx.notify();
                return;
            }
            // The innermost thing left once no menu or card is up: a preview
            // takes the picture over the screen, and escape is its own way
            // out -- the chord `preview.rs`'s "Stop (esc)" bar already
            // advertised, dead until now (FAULT 3) because only the legacy
            // tree's handler answered it.
            if key == ESCAPE && !ctrl && this.preview_session.is_some() {
                this.close_preview(cx);
                return;
            }
            if key == ESCAPE && !ctrl && this.source_hint {
                this.source_hint = false;
                cx.notify();
                return;
            }
            // Escape is Deselect, which an export refuses; said here with the
            // chord that does cancel, so a press during an export is never a
            // silent no-op that could be mistaken for a cancel.
            if key == ESCAPE && !ctrl && this.exporting().is_some() {
                let cancel = this.keymap.chord(ActionId::CancelExport);
                this.notify_user(
                    format!("Esc keeps the export running. {cancel} cancels it.").into(),
                );
                cx.notify();
                return;
            }
            if let Some(action) = this.keymap.lookup(key, ctrl, shift, alt) {
                // `ActionId::Export` used to be suppressed here because the
                // darkroom drew no export surface for the card to land on
                // (DESIGN §12 step 7's own hook). It has one now
                // ([`timeband_stance::export_chip`] opens it, `render`
                // below mounts [`Player::export_card`]
                // exactly where the legacy tree does), so `^e` dispatches
                // like every other action.
                this.act(action, window, cx);
            }
        }))
        // The seam drag is tracked on the root, same reason the legacy
        // ruler-scrub is (render.rs): the pointer outruns the 6px divider
        // hitbox on the first move, so only the whole-window root keeps
        // hearing it once a `Split::Dock`/`Split::Bench` drag has started.
        // A file dropped from outside the window (gpui's `ExternalPaths`,
        // Wayland's `text/uri-list`) lands on the room itself, not on the
        // centre column: the dock's Sources panel is the centre's flex
        // sibling, so a handler mounted there heard no drop
        // over the library at all -- the shipped "drag and drop import does
        // not work on the library panel" defect. Here it is the one surface
        // under every region, so a drop anywhere in the darkroom is heard.
        // No prior press starts this -- the drop itself arrives as a
        // `MouseUp` (gpui `window.rs`).
        .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
            // `Player::import` is the one door a project or media path
            // already goes through (argv, the Import button, Paste path) --
            // it forks on `.edith` vs everything else itself (`arrival`) and
            // already refuses during an export, so a drop names nothing this
            // loop has to.
            for path in paths.paths() {
                this.import(path, cx);
            }
        }))
        .on_mouse_move(cx.listener(Player::drag_move))
        // A press off the cue editor's plate commits the words in it (the
        // plate stops its own presses), so the words are not lost to a click.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(Player::cue_edit_press_outside),
        )
        .on_mouse_up(MouseButton::Left, cx.listener(Player::drag_release))
        .size_full()
        // The menu bar is laid over the top rows (`menubar::bar`, absolute), so
        // the room starts below it.
        .pt(px(MENU_BAR_H))
        .flex()
        .bg(rgb(DARK_CANVAS()))
        .children(hitmap::frame())
        .child(mouse_exit_listener(cx))
        .child(
            div()
                .id("stance-centre")
                // Same drag affordance the dock lifts to, on the other half
                // of the room: the drop is heard by the root above, this
                // only says where the file will land.
                .drag_over::<gpui::ExternalPaths>(|d, _, _, _| d.bg(rgb(DARK_RAISED())))
                .relative()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(screen(player, position, window, cx))
                .child(time_band(player, position, cx))
                .child(divider(
                    Split::Bench,
                    player.split_drag == Some(Split::Bench),
                    cx,
                ))
                .child(bench(player, bench_h, window, cx))
                .child(ledger(player, cx))
                // A maximized clip param card (EQ/Colour/Transform/Speed/
                // Silence/Mix/Subtitle style) escapes the dock to mount here
                // instead, the same window-space `stance-centre` context
                // every card above already renders in -- `below_picture_floor`
                // is a window coordinate, and `dock_stance.rs`'s
                // "dock-clip-rows" is a ~280-390px strip whose own box is the
                // containing block for anything absolutely positioned inside
                // it, `.relative()` or not, so a card asking for the room
                // below the picture cannot get it there. Un-maximized, the
                // same seven functions stay mounted in the dock
                // (`dock_stance::clip_tab`'s own `.when(!card_maximized, ...)`
                // guard keeps this from ever double-mounting one).
                .when(player.card_maximized, |el| {
                    el.children(player.eq_card(window_size, cx))
                        .children(player.color_card(window_size, cx))
                        .children(player.transform_card(window_size, cx))
                        .children(player.speed_card(window_size, cx))
                        .children(player.effects_card(window_size, cx))
                        .children(player.silence_card(window_size, cx))
                        .children(player.beats_card(window_size, cx))
                        .children(player.mix_card(window_size, cx))
                        .children(player.markers_card(window_size, cx))
                        .children(player.history_card(window_size, cx))
                        .children(player.tools_card(window_size, cx))
                        .children(player.subtitle_style_card(window_size, cx))
                }),
        )
        .child(divider(
            Split::Dock,
            player.split_drag == Some(Split::Dock),
            cx,
        ))
        .child(dock(player, dock_w, window_size, window, cx))
        // All three hanging menus (DESIGN §9: "verbs of the thing under the
        // cursor") mount on the ROOT, and none of them inside
        // `stance-centre`. Their anchors are *window* coordinates -- a
        // `MouseDownEvent::position` -- while an absolutely placed child of
        // the centre column is laid out from that column's own origin and
        // ends where it ends: the rail that used to stand left of the centre
        // shifted every clip menu right by its own 56px, and the dock,
        // painted after the centre, covered every
        // pixel past x = width - dock. That is the shipped "can not remove a
        // media from library" defect (the library plate was built, placed and
        // never visible) and the clip menu whose chords were cut off mid-word
        // at the dock seam. Mounted here they paint last, over the dock they
        // may overlap, and `menu_floor`/`menu_at`'s window coordinates finally
        // mean what they say. Order is `render.rs`'s -- clip menu, library
        // menu, then the open list last so it floats over whichever row set it.
        // The menu bar sits over the room's top edge and its click-away under
        // the root's own menus; its dropdown is deferred, so it paints over both.
        .child(menubar::bar(player, f32::from(viewport.width), f32::from(viewport.height), cx))
        .children(menubar::backdrop(player, cx))
        .children(player.context_card(viewport, cx))
        .children(player.library_card(viewport, cx))
        .children(player.picker_card(viewport, cx))
        .children(player.settings_open.then(|| crate::ui::kit::tab_scope(&pool, || settings_stance::render(player, window_size, cx))))
        .children(crate::ui::kit::tab_scope(&pool, || player.about_card(window_size, cx)))
        .children(crate::ui::kit::tab_scope(&pool, || player.project_dialog_card(window_size, cx)))
        // The command palette is the last thing painted: it floats over every
        // card and menu above (DESIGN §4.4).
        .children(player.command_palette(window_size, cx))
        // The component gallery (a debug page for the design kit) covers it all.
        .children(crate::ui::gallery::overlay(cx))
}
