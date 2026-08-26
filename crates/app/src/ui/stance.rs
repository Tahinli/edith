//! DESIGN.md §5 -- the stance: fixed geography, drawn by default now
//! (`OLD_GUI=1` opts back into the legacy four-region tree), whole here
//! rather than folded into that tree so the two can be told apart, and
//! swapped, with a single `if` in `render.rs`.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::ui::bench_stance;
use crate::ui::dock_stance;
use crate::ui::hitmap;
use crate::ui::settings_stance;
use crate::ui::spine_stance;
use crate::ui::timeband_stance;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// Keyboard focus v1: the three regions Tab/Shift-Tab cycle a painted ring
/// through -- the bench, the dock's Sources tab (library) and the dock's
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
    SURFACE_CYCLE[if backward { (i + n - 1) % n } else { (i + 1) % n }]
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
    ///
    /// The ring is darkroom-only UI -- the legacy tree never mounts
    /// `focus_dock`/`focus_bench`/`focus_inspector` (only `track_focus`
    /// sites live in this module and `dock_stance.rs`), so under
    /// `OLD_GUI=1` a `window.focus` here would land on an unmounted
    /// handle and kill the root key listener (`main.rs`'s fallback focus)
    /// until the next mouse click. No-op there instead.
    pub(crate) fn focus_surface(
        &mut self,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.darkroom {
            return;
        }
        if let Some(src_active) = surface_wants_src_active(surface) {
            self.dock_src_active = src_active;
            crate::ui::dock_stance::save(src_active);
        }
        window.focus(self.focus_handle(surface));
        cx.notify();
    }
}

/// Left rail, full height (DESIGN §5).
pub(crate) const SPINE_W: f32 = 56.;
/// Fixed strip under the screen: timecode, transport, cut readout, contact
/// strip, Export -- all placeholder at this step. Kept `pub(crate)` for
/// `layout::split_bounds`'s bench ceiling, the same one-door reason
/// [`LEDGER_H`] already is.
pub(crate) const TIME_BAND_H: f32 = 88.;
/// The bench's untouched height -- [`Split::Bench`]'s own default, read by
/// `layout::split_size` the same way the legacy tree's `library_w`/
/// `inspector_w` feed `Split::Library`/`Split::Inspector`. A hand on the
/// seam above it overrides this; nothing else does.
pub(crate) const BENCH_H: f32 = 200.;
/// Thin strip at the foot of the centre column. Read by `layout`'s
/// `Split::Bench` drag formula (the seam sits above the fixed ledger, so a
/// drag has to leave room for it) -- kept `pub(crate)` for that one door
/// rather than a second copy of the number.
pub(crate) const LEDGER_H: f32 = 28.;
/// The dock's untouched width -- [`Split::Dock`]'s own default, the same
/// role [`BENCH_H`] plays for the bench.
pub(crate) const DOCK_W: f32 = 280.;
/// What [`bench`] spends above `bench_stance::render`'s own content: the
/// `bench` div's own `.border_t_1()` (1px) + its `py(4.)` top padding +
/// the section head's real line box -- `type_scale::head()` sizes text at
/// `SECTION_HEAD_PX` (12) but never calls `.line_height()`, so gpui's
/// default `TextStyle::line_height` (the golden ratio, `gpui::phi()` ==
/// `1.618034`, not 1x the font size) is what it actually draws:
/// `round(12. * 1.618034)` = `round(19.416408)` = `19`.
/// `1 + 4 + 19` = `24`. Named and `pub(crate)` rather than an inline
/// subtraction, so `layout::BENCH_MIN_H` can derive from the real chrome.
pub(crate) const BENCH_CHROME_H: f32 = 24.;

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
/// Darkroom only -- the legacy tree has no fixed split to clamp against.
pub(crate) fn menu_floor(
    at: Point<Pixels>,
    viewport: Size<Pixels>,
    darkroom: bool,
    bench_h: f32,
) -> (Point<Pixels>, Size<Pixels>) {
    if !darkroom {
        return (at, viewport);
    }
    let floor = below_picture_floor(f32::from(viewport.height), bench_h);
    let at = point(at.x, px(f32::from(at.y).max(floor)));
    let room = size(viewport.width, px(f32::from(viewport.height) - floor));
    (at, room)
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
        let (at, room) = menu_floor(point(px(300.), px(20.)), viewport, true, BENCH_H);
        assert_eq!(f32::from(at.y), floor);
        // 396px of rows (the charter's measured menu height) does not fit in
        // the 316px footprint room, so it must be capped to it, not the
        // window's 720.
        let list_h = (396f32).min(f32::from(room.height));
        assert!(list_h <= f32::from(room.height));
        let (_, y) = crate::oracle::menu_at(at, viewport, list_h);
        assert_eq!(
            y, floor,
            "the top edge must not be walked back above the floor"
        );
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
    // FAULT 3: the picture region's own laid-out box, measured each frame by
    // a [`bounds_probe`] the same shape `timeband_stance`'s contact strip
    // keeps (`STRIP_BOUNDS`) -- the scale plate needs the box the picture was
    // fitted into, which nothing upstream of paint knows.
    static PICTURE_BOUNDS: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
}

/// The pure half of [`picture_scale`]: given a box and the image's native
/// size, its `Contain` fit factor -- gpui's own tested `get_bounds` math
/// ([`letterboxed_image`] fits the picture the same way), not a ratio
/// reimplemented here. Split out from `picture_scale` so this arithmetic has
/// a runnable check with no `Player`/window to stand up.
fn fit_scale(bounds: Bounds<Pixels>, native: gpui::Size<gpui::DevicePixels>) -> Option<f32> {
    if bounds.size.width <= px(0.)
        || bounds.size.height <= px(0.)
        || native.width.0 <= 0
        || native.height.0 <= 0
    {
        return None;
    }
    let fitted = gpui::ObjectFit::Contain.get_bounds(bounds, native);
    Some(f32::from(fitted.size.width) / native.width.0 as f32)
}

/// The picture's own fit factor against its native size, read off the last
/// frame's measured box (see [`PICTURE_BOUNDS`]). `None` with nothing open
/// or before the first paint has measured anything.
fn picture_scale(player: &Player) -> Option<f32> {
    let image = player.image.clone()?;
    fit_scale(PICTURE_BOUNDS.with(Rc::clone).get(), image.size(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 400x100 source fit into a 200x100 box: width-bound (bounds_ratio
    /// 2 < image_ratio 4), so the picture lands at half its native width --
    /// the number the scale plate would read as `scale 0.50`.
    #[test]
    fn the_scale_plate_reads_contains_own_fit_factor() {
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(200.), px(100.)),
        };
        let native = size(gpui::DevicePixels(400), gpui::DevicePixels(100));
        assert!((fit_scale(bounds, native).unwrap() - 0.5).abs() < 0.001);
    }

    /// A degenerate (zero-size) box never divides by zero -- it says "no
    /// reading yet" instead of NaN or a panic, the same guard every other
    /// `bounds_probe`-fed measurement in this file makes before its first
    /// paint.
    #[test]
    fn an_unmeasured_box_reads_no_scale_rather_than_nan() {
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(0.), px(0.)),
        };
        let native = size(gpui::DevicePixels(400), gpui::DevicePixels(100));
        assert_eq!(fit_scale(bounds, native), None);
    }
}

/// The scale plate (MOCK-SPEC.md "Screen": `scale 1.12`, label `ink3`, value
/// in ink): a readout, so a plate (DESIGN §4), never a chip. Lives in the
/// screen region's own margin row above the picture -- a flex sibling, not
/// an overlay, so it can never cover a picture pixel (§11 check 6) whatever
/// the source's aspect ratio does to the letterbox.
fn scale_plate(player: &Player) -> impl IntoElement {
    let value = picture_scale(player)
        .map(|s| format!("{s:.2}"))
        .unwrap_or_else(|| "--".to_string());
    let style = type_scale::mono(type_scale::CHORD_METADATA_MIN_PX, gpui::FontWeight::MEDIUM);
    div()
        .id("stance-scale-plate")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(6.))
        .py(px(2.))
        .rounded(px(2.))
        .bg(rgb(DARK_CANVAS()))
        .font(style.font)
        .text_size(style.size)
        .child(div().text_color(rgb(INK3())).child("scale"))
        .child(div().text_color(rgb(INK1())).child(value))
}

/// A section head, uppercase, `ink3` -- DESIGN §3's scale for the label
/// that names a region before anything else lives in it.
fn section_head(label: &str) -> impl IntoElement {
    div()
        .flex_none()
        .type_style(type_scale::head())
        .text_color(rgb(INK3()))
        .child(label.to_uppercase())
}

/// A ghost command (DESIGN §4): borderless glyph (`ink2`) + dim chord
/// (`ink3`), read live off the keymap so a rebind can never leave the spine
/// showing a stroke that no longer fires it.
fn ghost(player: &Player, glyph: &str, action: ActionId) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .py(px(6.))
        .rounded(px(3.))
        .type_style(type_scale::label(
            type_scale::LABEL_ROW_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK2()))
        .child(glyph.to_string())
        // The badge is skipped when the stroke IS the glyph: `?` over `?`
        // drew as two identical rows stacked at the foot of the rail and read
        // as a duplicated control rather than as one command wearing its own
        // chord.
        .children((player.keymap.chord(action) != glyph).then(|| {
            div()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                // FAULT 1: the badge shows the primary chord, compact --
                // same rule as every glyph in `spine_stance`.
                .child(player.keymap.chord(action))
        }))
}

/// The keys overlay (DESIGN §9, §12 step 7): every bound command, sectioned
/// by [`keymap::Category`] exactly as the legacy actions card files them
/// ([`keys_rows`]) -- reusing that registry-driven order is what makes an
/// action added anywhere land here without a second list to forget. Held
/// open by `?` -- [`crate::ui::stance::render`]'s own `on_key_down` opens it
/// on the press (bypassing the modal `overlaid()` guard, which would
/// otherwise swallow the second `?` down while the first is still up) and
/// its `on_key_up` closes it on release, so this is the room dimming one
/// fill step for as long as the key is held, never a latch (DESIGN §9: "no
/// modal cheat-sheet").
///
/// corner-cut, named explicitly in DESIGN §9's own 2026-08-20 amendment
/// rather than silently shipped against the section's original text: it
/// wants chords surfaced *in place beside their controls* across every
/// region, and this is one scrolling list plate instead, anchored over the
/// bench/ledger footprint so it never reaches the screen (§11.6 holds).
/// Ceiling: DESIGN §12 step 7's full geographic pass (each region draws its
/// own rows while held) -- §9's amendment names the 56 actions still
/// without a home.
fn keys_overlay(player: &Player) -> impl IntoElement {
    div()
        .id("stance-keys-overlay")
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(px(BENCH_H + LEDGER_H))
        .bg(rgba(SCRIM()))
        .flex()
        .child(
            div()
                .id("stance-keys-list")
                .m_auto()
                .h(px(BENCH_H + LEDGER_H - 16.))
                .w(px(460.))
                .overflow_y_scroll()
                .p(px(10.))
                .rounded(px(4.))
                .bg(rgb(DARK_PANEL()))
                .border_1()
                .border_color(rgba(DARK_SEAM()))
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(section_head("all commands · hold ? · release to close"))
                .children(keys_rows().into_iter().map(|row| {
                    match row {
                        KeyRow::Head(category) => div()
                            .flex_none()
                            .pt(px(4.))
                            .type_style(type_scale::head())
                            .text_color(rgb(INK3()))
                            .child(category.label().to_uppercase())
                            .into_any_element(),
                        KeyRow::Act(action) => div()
                            .flex()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                div()
                                    .type_style(type_scale::label(
                                        type_scale::CHORD_METADATA_MIN_PX,
                                        gpui::FontWeight::MEDIUM,
                                    ))
                                    .text_color(rgb(INK2()))
                                    .child(action.label()),
                            )
                            .child(
                                div()
                                    .type_style(type_scale::mono(
                                        type_scale::CHORD_METADATA_MIN_PX,
                                        gpui::FontWeight::MEDIUM,
                                    ))
                                    .text_color(rgb(INK3()))
                                    // The keys overlay is the full-truth surface
                                    // (FAULT 1): every chord an action answers to,
                                    // not the badge's primary-only compact form.
                                    .child(player.keymap.display(action)),
                            )
                            .into_any_element(),
                        KeyRow::Fixed(i) => {
                            let f = &keymap::FIXED[i];
                            div()
                                .flex()
                                .justify_between()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .type_style(type_scale::label(
                                            type_scale::CHORD_METADATA_MIN_PX,
                                            gpui::FontWeight::MEDIUM,
                                        ))
                                        .text_color(rgb(INK2()))
                                        .child(f.label),
                                )
                                .child(
                                    div()
                                        .type_style(type_scale::mono(
                                            type_scale::CHORD_METADATA_MIN_PX,
                                            gpui::FontWeight::MEDIUM,
                                        ))
                                        .text_color(rgb(INK3()))
                                        .child(f.chord.clone()),
                                )
                                .into_any_element()
                        }
                    }
                })),
        )
}

/// The spine: 56px, left, full height. Frame only -- grouped rows, task
/// frequency, glyph-over-chord grammar and every click all live in
/// [`crate::ui::spine_stance`] now (MOCK-SPEC.md "Spine"); this fn keeps
/// the panel's own surface and the `?` row, which stays part of the frame
/// since it opens [`keys_overlay`] right here rather than through `act`.
fn spine(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-spine")
        .flex_none()
        .w(px(SPINE_W))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_r_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .flex_col()
        .items_center()
        .py(px(8.))
        .gap(px(4.))
        .child(section_head("spine"))
        .child(spine_stance::render(player, cx))
        .child(ghost(player, "?", ActionId::ShowActions))
}

/// The picture region: top of the centre column, takes the remaining space,
/// never occluded (DESIGN §5, §11 check 6). Reuses [`Player::picture_area`]
/// rather than a second image element -- the darkroom draws the same picture
/// the legacy tree does -- and, at rest on a cut, letterboxes it by stacking
/// the two-up OUT|IN judging *below* it as a flex sibling (DESIGN §6) rather
/// than layering plates over it: the picture's own `flex_1` shrinks to make
/// room, so every picture pixel that is drawn stays visible.
fn screen(
    player: &mut Player,
    position: f64,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let two_up = player.two_up().map(IntoElement::into_any_element);
    div()
        .id("stance-screen")
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        // §4: room chrome takes 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        // FAULT 3: the scale plate's own margin row, above the picture --
        // a flex sibling shrinks the picture's `flex_1` to make room for it
        // exactly as `two_up` already does below, so it is never drawn over
        // real picture pixels.
        .child(
            div()
                .flex_none()
                .flex()
                .justify_end()
                .px(px(6.))
                .pt(px(4.))
                .child(scale_plate(player)),
        )
        .child(
            div()
                .id("stance-picture")
                .flex_1()
                .min_h(px(0.))
                .flex()
                .relative()
                .child(player.picture_area(position, window, cx))
                .child(bounds_probe(PICTURE_BOUNDS.with(Rc::clone))),
        )
        .children(two_up)
        // The preview's own plate, a flex sibling for the same reason two_up is
        // one: drawn inside the picture it would cover the frame.
        .children(player.preview_plate(cx))
}

/// Fixed-height strip under the screen: timecode leads, ghost transport, cut
/// readout, the contact strip, boxed Export at the end (DESIGN §5,
/// MOCK-SPEC.md "Time band"). Frame only -- every row and the Export chip's
/// click live in [`crate::ui::timeband_stance`] now, the same split
/// `spine_stance`/`bench_stance`/`dock_stance` already make for their
/// regions.
fn time_band(player: &mut Player, position: f64, cx: &mut Context<Player>) -> impl IntoElement {
    // The position comes from `render`, the same one the screen and the ledger
    // read. Asking `active_session` here instead read the PREVIEW's clock while
    // a preview was up, so the hero timecode and the ledger's position -- two
    // readouts of one truth -- disagreed on screen.
    div()
        .id("stance-time-band")
        .flex_none()
        .h(px(TIME_BAND_H))
        .bg(rgb(DARK_PANEL()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
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
    div()
        .id("stance-bench")
        .track_focus(&player.focus_bench)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            if is_focus_cycle_key(key) {
                let next = next_surface(Surface::Bench, event.keystroke.modifiers.shift);
                this.focus_surface(next, window, cx);
                cx.stop_propagation();
            } else if is_focus_exit_key(key) {
                // Leaves the ring rather than deleting it: the root handle
                // gets focus back, and a second escape there is what does
                // whatever escape means at the root (nothing, today) --
                // this must not itself bubble, or the root's own handler
                // never sees a stroke that stayed a ring exit.
                window.focus(&this.focus);
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .flex_none()
        .h(px(bench_h))
        // §4: lanes take 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        // The ring (DESIGN §4's "1px, lamp-adjacent" convention -- the same
        // `.border_1().border_color(...)` a picked bench clip already uses):
        // painted on this outer frame, never on a row, and only while this
        // surface itself -- not a picked clip inside it -- holds the focus
        // ring Tab/Shift-Tab moves ([`next_surface`]).
        .when(focused, |d| d.border_1().border_color(rgb(STROKE_FOCUS())))
        .flex()
        .flex_col()
        .px(px(12.))
        .py(px(4.))
        .child(section_head("bench"))
        .child(bench_stance::render(player, bench_h - BENCH_CHROME_H, cx))
}

/// Which of the three §8 severities a notice's own words carry. Reuses
/// [`notice_tone`] (the legacy bar's own classification, by word) rather than
/// a second heuristic, and folds its four-way answer onto the darkroom's
/// three tokens: `STATUS_ERROR`/`STATUS_WARNING` are the same constants as
/// `NOTICE_DECIDE`/`NOTICE_LOOK` already (`ui/theme.rs`'s `darkroom` module
/// aliases them both ways), so the two failure/refusal tones fold straight
/// across and everything else -- success included -- reads as *told you*.
fn notice_severity(message: &str) -> u32 {
    let tone = notice_tone(message);
    if tone == STATUS_ERROR() {
        NOTICE_DECIDE()
    } else if tone == STATUS_WARNING() {
        NOTICE_LOOK()
    } else {
        NOTICE_TELL()
    }
}

/// [`notice_plate`]'s own `bottom` offset, pulled out as a pure function so
/// a `TestAppContext`-less test binary (this crate's own, no live `Context`
/// to mount a real `notice_plate` in) can still check the plate never lands
/// over the bench, for any `bench_h` -- without duplicating the arithmetic
/// by hand and risking the two drifting apart.
pub(crate) fn notice_bottom_offset(bench_h: f32) -> f32 {
    LEDGER_H + bench_h + 6.
}

/// A notice plate (DESIGN §8): one at a time, rising above the *bench*, its
/// severity a 3px left spine rather than a colour flood. Fed by the same
/// [`Player::notify_user`]/`notices` queue the legacy bar reads
/// ([`Player::notice_bar`]) -- no second notice channel.
///
/// Reads the *back* of the queue, not the front: dismissal only ever fires
/// on a keystroke ([`render`]'s `on_key_down` calling `dismiss_notice()`),
/// and most of what fills this queue -- a click on a gap, a menu row, a
/// drag -- is not one. A `front()` plate left showing a `ctrl+s` "SAVED"
/// notice sat frozen through an unrelated mouse-driven refusal that queued
/// in behind it: the ledger strip's own "last action" already reads
/// `back()` for exactly this reason, and the plate disagreeing with it is
/// what made the refusal look like it never reached the notice surface at
/// all. `back()` keeps the two in step and guarantees the newest, most
/// actionable message is always the one on screen.
///
/// Anchored off `bench_h` rather than a fixed offset off the ledger: at the
/// bench's floor a fixed `LEDGER_H + 6.` bottom offset put the plate right
/// over the V1/A1 lane chips -- a transient message hiding the lanes for as
/// long as it sat there. Floating it `bench_h` further up puts its bottom
/// edge at the bench's own *top* edge for any bench height, so it always
/// sits in the divider/time-band's slack above the bench rather than over
/// the lanes, and never has to reserve room in the bench itself for a
/// message that is not always there.
///
/// corner-cut: amber's "carries a jump action" is not wired -- the queue
/// only ever held plain text, no structured jump target, in either the
/// legacy bar or here. Ceiling: give `notify_user` an optional jump payload
/// once a call site actually has one to carry.
fn notice_plate(message: SharedString, bench_h: f32) -> impl IntoElement {
    div()
        .id("stance-notice")
        .absolute()
        .bottom(px(notice_bottom_offset(bench_h)))
        .left(px(12.))
        // `max_w` only caps a box after GPUI has measured its text at
        // max-content width. `w_full` supplies a definite width to that
        // measurement (then `max_w` bounds it at 480 px), and normal
        // whitespace gives the text system permission to shape every word
        // into the resulting lines. Keep this a block: as a flex child the
        // text's automatic min-content width can overflow its own plate.
        //
        // No fixed height: GPUI's shaped lines contribute their actual
        // default `phi()` line boxes, so two or three lines grow upward from
        // this bottom anchor without reaching down over the bench lanes.
        .w_full()
        .max_w(px(480.))
        .whitespace_normal()
        .px(px(10.))
        .py(px(6.))
        .rounded(px(2.))
        .bg(rgb(DARK_PANEL()))
        // The severity spine (DESIGN §8): a 3px left border, coloured, same
        // shape as the legacy notice bar's own tone stripe (`notice_bar`).
        .border_l(px(3.))
        .border_color(rgb(notice_severity(&message)))
        .type_style(type_scale::label(
            type_scale::LABEL_ROW_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK1()))
        // `div()` is a flex row by default, and a bare text child measures
        // its min-content as the *whole line* (`TextLayout::layout` only
        // wraps against a `Definite` available width, which a flex item's
        // content-sizing pass never supplies). `w_full`/`max_w` on the
        // plate itself only bounds the *container* -- the text still never
        // sees a definite width to wrap against unless it is a `flex_1`
        // child, which is handed the container's resolved width. Same fix
        // as `overlays.rs`'s notice bar (`div().flex_1().min_w(...)`).
        .child(div().flex_1().child(message))
}

/// Thin strip at the bottom of the centre column: project identity, last
/// action, export progress, position. Notices rise from here (DESIGN §5, §8).
fn ledger(player: &Player, position: f64) -> impl IntoElement {
    let name = match player.project_path.as_os_str().is_empty() {
        true => "untitled".to_string(),
        false => file_name(&player.project_path),
    };
    let identity = format!(
        "{name} · {}",
        if player.autosave_dirty {
            "unsaved"
        } else {
            "saved"
        }
    );
    let last_action = player.notices.back().cloned().unwrap_or_else(|| "—".into());
    let export = player
        .exporting()
        .map(|h| format!("EXPORTING {:.0}%", h.progress() * 100.));
    let tc = timecode(position, player.active_fps());
    div()
        .id("stance-ledger")
        .flex_none()
        .h(px(LEDGER_H))
        .bg(rgb(DARK_PANEL()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(12.))
        .child(section_head("ledger"))
        // MOCK-SPEC.md "Ledger": "All mono" -- project identity, last
        // action, export progress and position are all what the film/project
        // says, not the room's own voice.
        .child(
            div()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK1()))
                .child(identity),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .child(last_action),
        )
        .children(export.map(|e| {
            div()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK2()))
                .child(e)
        }))
        .child(
            div()
                .flex_none()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK2()))
                .child(tc),
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
    div()
        .id("stance-dock")
        .flex_none()
        .w(px(dock_w))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_l_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .flex_col()
        .child(dock_stance::render(player, dock_w, window_size, window, cx))
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

/// The whole stance: spine, screen, time band, bench, ledger, dock, in the
/// order DESIGN §5 draws them, over the same key handler the legacy tree
/// uses (DESIGN §12 step 3): the darkroom draws its own regions but answers
/// to the one keymap.
pub(crate) fn render(
    player: &mut Player,
    window: &mut Window,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let window_size = window.viewport_size();
    let position = player.playhead(player.drawn_duration());
    // The two seams a hand may drag (`Split::Dock`, `Split::Bench`): read
    // through the same door every legacy region measures itself by
    // ([`Player::split_px`]), so a dragged size and a drawn one cannot
    // disagree here either.
    let dock_w = player.split_px(Split::Dock, window_size);
    let bench_h = player.split_px(Split::Bench, window_size);
    div()
        .id("stance-room")
        .track_focus(&player.focus)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            let ctrl = event.keystroke.modifiers.control;
            if event.is_held && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl)) {
                return;
            }
            // The recovery notice reads a key as more than "answered" (legacy
            // `render.rs`'s own rule): `enter` loads the sidecar it named, any
            // other key declines it. Asked before the generic dismiss below
            // shares the door with it, not instead of it -- the darkroom
            // notice bar advertised "enter recovers it, any other key keeps
            // this" with no listener behind it until now (DESIGN §4).
            if this.recovery_sidecar.is_some() {
                if key == "enter" {
                    this.recover_from_sidecar(cx);
                } else if let Some(sidecar) = this.recovery_sidecar.take() {
                    Player::discard_sidecar(&sidecar);
                }
            }
            if this.dismiss_notice() {
                cx.notify();
            }
            // DESIGN §9: "`?` held ... surfaces chords ... release restores.
            // No modal cheat-sheet." Opened here, ahead of the modal guard
            // below (which would otherwise swallow a second `?` down once
            // `keys_open` is already true) -- `on_key_up` is the only thing
            // that closes it, so there is no latch to fall through to.
            if key == "?" && !ctrl {
                if !this.keys_open {
                    this.show_actions(cx);
                }
                cx.notify();
                return;
            }
            // The dock's Sources filter (MOCK-SPEC "Dock" §2), the same door
            // legacy `render.rs` gives it: owns the keyboard only while it
            // was clicked into (`dock_stance.rs` sets `dock_filter_edit` on
            // that click regardless of which tree is drawing the row), a
            // letter typed anywhere else in the room keeps meaning whatever
            // the spine says it means.
            if this.dock_filter_edit {
                if key == "escape" || key == "enter" {
                    this.dock_filter_edit = false;
                } else if key == "backspace" {
                    this.dock_filter.pop();
                } else if let Some(c) = typed(key) {
                    this.dock_filter.push(c);
                }
                cx.notify();
                return;
            }
            // `↵` adds the picked source at the playhead (MOCK-SPEC "Dock"
            // hint paragraph, gesture 2; `dock_stance.rs`'s own hint promised
            // this and nothing answered it -- FAULT 1). Live only where a row
            // is actually picked, so a bare enter elsewhere still means
            // whatever it means below. `insert_source` already refuses
            // visibly on its own ("NOTHING ADDED -- ...") when the span is
            // occupied, so there is nothing further to paint here.
            if key == "enter"
                && this.dock_src_active
                && this.selected_asset.is_some()
                && this.exporting().is_none()
            {
                if let Some((path, stream)) = this.selected_asset.clone() {
                    this.insert_source(&path, stream, None, None, cx);
                }
                cx.notify();
                return;
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
            // Same modal guard the legacy handler's rebinding/keys_open/
            // export_open/colour/transform/speed/EQ/silence/mix/
            // subtitle-style/menu chain (`render.rs`) amounts to, read as
            // one bool instead of re-copied field by field: while a card, a
            // rebind capture, or a menu owns the keyboard, the darkroom
            // stance yields to it exactly as the legacy tree does. This is
            // the fix for the stance's `Home`-inside-the-Colour-card
            // misfire.
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
            if this.rebinding.is_some()
                || this.card_open()
                || this.context_menu.is_some()
                || this.library_menu.is_some()
                || this.picker.is_some()
            {
                // Escape is every card's own way out (`Player::close_card`,
                // the same list `overlaid`/`modal` read) -- blocking every
                // key while a card is open must not also lock the card open.
                if key == ESCAPE {
                    this.rebinding = None;
                    this.close_card();
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
                this.context_menu = None;
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
            if let Some(action) = this.keymap.lookup(key, ctrl) {
                // `ActionId::Export` used to be suppressed here because the
                // darkroom drew no export surface for the card to land on
                // (DESIGN §12 step 7's own hook). It has one now
                // ([`timeband_stance::export_chip`] opens it, `render`
                // below mounts [`Player::export_card`]/`export_progress_card`
                // exactly where the legacy tree does), so `^e` dispatches
                // like every other action.
                this.act(action, window, cx);
            }
        }))
        // The release half of the `?` hold above: whatever opened it, this is
        // the one thing that closes it -- there is nothing else in the
        // darkroom that sets `keys_open` true, so this cannot close a card
        // `?` did not open.
        .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
            if event.keystroke.key.as_str() == "?" {
                this.keys_open = false;
                cx.notify();
            }
        }))
        // The seam drag is tracked on the root, same reason the legacy
        // ruler-scrub is (render.rs): the pointer outruns the 6px divider
        // hitbox on the first move, so only the whole-window root keeps
        // hearing it once a `Split::Dock`/`Split::Bench` drag has started.
        .on_mouse_move(cx.listener(Player::drag_move))
        .on_mouse_up(MouseButton::Left, cx.listener(Player::drag_release))
        .size_full()
        .flex()
        .bg(rgb(DARK_CANVAS()))
        .children(hitmap::frame())
        .child(mouse_exit_listener(cx))
        .child(spine(player, cx))
        .child(
            div()
                .id("stance-centre")
                .relative()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(screen(player, position, window, cx))
                .child(time_band(player, position, cx))
                .child(divider(Split::Bench, cx))
                .child(bench(player, bench_h, window, cx))
                .child(ledger(player, position))
                .when_some(player.notices.back().cloned(), |el, n| {
                    el.child(notice_plate(n, bench_h))
                })
                .when(player.keys_open, |el| el.child(keys_overlay(player)))
                .when(player.settings_open, |el| {
                    el.child(settings_stance::render(player, window_size, cx))
                })
                // The two menus (DESIGN §9: "verbs of the thing under the
                // cursor", plate styling, the same scrim-and-row component
                // the legacy tree already uses -- `library.rs`'s own doc
                // comment: "Built like `Player::context_card` ... because it
                // is the same menu on the other panel") and the open list a
                // picker sets: none of the three had a home in the stance,
                // so `overlaid()` refused every key over them for nothing
                // ever drawn (FAULT 2). `render.rs`'s children order is kept
                // -- clip menu, then library menu, then the open list last so
                // it floats over whichever row opened it.
                .children(player.context_card(window_size, cx))
                // The export card and its running-progress sheet, mounted
                // over the whole room exactly as the legacy tree mounts them
                // (`render.rs`) -- the fix for the shipped "Export does
                // nothing" defect: the chip/chord open `export_open`, this is
                // what draws it once open.
                .children(player.export_card(window_size, cx))
                .children(player.export_progress_card(cx))
                .children(player.picker_card(window_size, cx))
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
                        .children(player.silence_card(window_size, cx))
                        .children(player.mix_card(window_size, cx))
                        .children(player.subtitle_style_card(window_size, cx))
                }),
        )
        .child(divider(Split::Dock, cx))
        .child(dock(player, dock_w, window_size, window, cx))
        // The library row menu mounts on the ROOT, not inside
        // `stance-centre` like the clip menu beside it: it is opened from a
        // dock row, so its anchor is a window x past the centre column's own
        // right edge, and the dock -- painted after the centre -- covered
        // every pixel of it. That is the whole of the shipped "can not
        // remove a media from library" defect: the plate was built, placed
        // and never visible. Mounted here it paints last, over the dock it
        // belongs to, and its own `menu_floor` window coordinates finally
        // mean what they say.
        .children(player.library_card(window_size, cx))
}
