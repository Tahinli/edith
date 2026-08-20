//! DESIGN.md §5 -- the stance: fixed geography, drawn by default now
//! (`OLD_GUI=1` opts back into the legacy four-region tree), whole here
//! rather than folded into that tree so the two can be told apart, and
//! swapped, with a single `if` in `render.rs`.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::*;
use crate::ui::bench_stance;
use crate::ui::dock_stance;
use crate::ui::timeband_stance;
use crate::ui::spine_stance;
use crate::ui::type_scale::{self, Typeset};

/// Left rail, full height (DESIGN §5).
const SPINE_W: f32 = 56.;
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
        assert_eq!(y, floor, "the top edge must not be walked back above the floor");
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
    if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) || native.width.0 <= 0 || native.height.0 <= 0 {
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

/// A section head: 9px, uppercase, `ink3` -- DESIGN §3's scale for the label
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
        .type_style(type_scale::label(13., gpui::FontWeight::MEDIUM))
        .text_color(rgb(INK2()))
        .child(glyph.to_string())
        .child(
            div()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                // FAULT 1: the badge shows the primary chord, compact --
                // same rule as every glyph in `spine_stance`.
                .child(player.keymap.chord(action)),
        )
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
                .children(keys_rows().into_iter().map(|row| match row {
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
fn screen(player: &mut Player, position: f64, window: &mut Window, cx: &mut Context<Player>) -> impl IntoElement {
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
fn bench(player: &mut Player, bench_h: f32, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-bench")
        .flex_none()
        .h(px(bench_h))
        // §4: lanes take 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .flex_col()
        .px(px(12.))
        .py(px(4.))
        .child(section_head("bench"))
        .child(bench_stance::render(player, bench_h - 20., cx))
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

/// A notice plate (DESIGN §8): one at a time, rising above the ledger, its
/// severity a 3px left spine rather than a colour flood. Fed by the same
/// [`Player::notify_user`]/`notices` queue the legacy bar reads
/// ([`Player::notice_bar`]) -- no second notice channel. Dismissal is
/// already wired: [`render`]'s `on_key_down` calls `dismiss_notice()` on
/// every stroke, the same door the legacy handler uses.
///
/// corner-cut: amber's "carries a jump action" is not wired -- the queue
/// only ever held plain text, no structured jump target, in either the
/// legacy bar or here. Ceiling: give `notify_user` an optional jump payload
/// once a call site actually has one to carry.
fn notice_plate(message: SharedString) -> impl IntoElement {
    div()
        .id("stance-notice")
        .absolute()
        .bottom(px(LEDGER_H + 6.))
        .left(px(12.))
        .max_w(px(360.))
        .flex()
        .items_center()
        .px(px(10.))
        .py(px(6.))
        .rounded(px(2.))
        .bg(rgb(DARK_PANEL()))
        // The severity spine (DESIGN §8): a 3px left border, coloured, same
        // shape as the legacy notice bar's own tone stripe (`notice_bar`).
        .border_l(px(3.))
        .border_color(rgb(notice_severity(&message)))
        .type_style(type_scale::label(10., gpui::FontWeight::MEDIUM))
        .text_color(rgb(INK1()))
        .child(message)
}

/// Thin strip at the bottom of the centre column: project identity, last
/// action, export progress, position. Notices rise from here (DESIGN §5, §8).
fn ledger(player: &Player, position: f64) -> impl IntoElement {
    let name = match player.project_path.as_os_str().is_empty() {
        true => "untitled".to_string(),
        false => file_name(&player.project_path),
    };
    let identity = format!("{name} · {}", if player.autosave_dirty { "unsaved" } else { "saved" });
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
/// "ui fields are not stretchable" for the dock.
fn dock(player: &Player, dock_w: f32, window_h: Pixels, cx: &mut Context<Player>) -> impl IntoElement {
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
        .child(dock_stance::render(player, dock_w, window_h, cx))
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
    let window_h = window_size.height;
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
            // export_open/exporting/colour/transform/speed/EQ/silence/mix/
            // subtitle-style/menu chain (`render.rs`) amounts to, read as
            // one bool instead of re-copied field by field: while a card, a
            // rebind capture, or a menu owns the keyboard, the darkroom
            // stance yields to it exactly as the legacy tree does. This is
            // the fix for the stance's `Home`-inside-the-Colour-card
            // misfire.
            if this.rebinding.is_some() || this.overlaid() {
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
                .child(bench(player, bench_h, cx))
                .child(ledger(player, position))
                .when_some(player.notices.front().cloned(), |el, n| {
                    el.child(notice_plate(n))
                })
                .when(player.keys_open, |el| el.child(keys_overlay(player)))
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
                .children(player.library_card(window_size, cx))
                // The export card and its running-progress sheet, mounted
                // over the whole room exactly as the legacy tree mounts them
                // (`render.rs`) -- the fix for the shipped "Export does
                // nothing" defect: the chip/chord open `export_open`, this is
                // what draws it once open.
                .children(player.export_card(window_size, cx))
                .children(player.export_progress_card(cx))
                .children(player.picker_card(window_size, cx)),
        )
        .child(divider(Split::Dock, cx))
        .child(dock(player, dock_w, window_h, cx))
}
