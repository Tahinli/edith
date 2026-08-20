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
/// strip, Export -- all placeholder at this step.
const TIME_BAND_H: f32 = 88.;
/// corner-cut: a fixed share of the window rather than one measured against
/// the picture's own room, matching the placeholder every other stance
/// region still carries at this step. Ceiling: fold into the same split the
/// legacy timeline's `Split::Timeline` answers once the stance grows a
/// resizable seam of its own.
const BENCH_H: f32 = 200.;
/// Thin strip at the foot of the centre column.
const LEDGER_H: f32 = 28.;
/// Right side panel, fixed width, carrying the Src/Clip tab pair.
const DOCK_W: f32 = 280.;

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
        .child(
            div()
                .id("stance-picture")
                .flex_1()
                .min_h(px(0.))
                .flex()
                .relative()
                .child(player.picture_area(position, window, cx)),
        )
        .children(two_up)
}

/// Fixed-height strip under the screen: timecode leads, ghost transport, cut
/// readout, the contact strip, boxed Export at the end (DESIGN §5,
/// MOCK-SPEC.md "Time band"). Frame only -- every row and the Export chip's
/// click live in [`crate::ui::timeband_stance`] now, the same split
/// `spine_stance`/`bench_stance`/`dock_stance` already make for their
/// regions.
fn time_band(player: &mut Player, cx: &mut Context<Player>) -> impl IntoElement {
    let position = player.active_session().map_or(0., PlaybackSession::now);
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
fn bench(player: &mut Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-bench")
        .flex_none()
        .h(px(BENCH_H))
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
        .child(bench_stance::render(player, BENCH_H - 20., cx))
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
fn dock(player: &Player, window_h: Pixels, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-dock")
        .flex_none()
        .w(px(DOCK_W))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_l_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .flex_col()
        .child(dock_stance::render(player, DOCK_W, window_h, cx))
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
    div()
        .id("stance-room")
        .track_focus(&player.focus)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            let ctrl = event.keystroke.modifiers.control;
            if event.is_held && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl)) {
                return;
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
                cx.notify();
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
                .child(time_band(player, cx))
                .child(bench(player, cx))
                .child(ledger(player, position))
                .when_some(player.notices.front().cloned(), |el, n| {
                    el.child(notice_plate(n))
                })
                .when(player.keys_open, |el| el.child(keys_overlay(player)))
                // The export card and its running-progress sheet, mounted
                // over the whole room exactly as the legacy tree mounts them
                // (`render.rs`) -- the fix for the shipped "Export does
                // nothing" defect: the chip/chord open `export_open`, this is
                // what draws it once open.
                .children(player.export_card(window_size, cx))
                .children(player.export_progress_card(cx)),
        )
        .child(dock(player, window_h, cx))
}
