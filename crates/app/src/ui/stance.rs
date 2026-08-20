//! DESIGN.md §5 -- the stance: fixed geography behind `EDITH_DARKROOM`, drawn
//! whole here rather than folded into the legacy four-region tree so the two
//! can be told apart, and swapped, with a single `if` in `render.rs`.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::*;
use crate::ui::bench_stance;
use crate::ui::dock_stance;

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
        .text_size(px(9.))
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
        .text_size(px(13.))
        .text_color(rgb(INK2()))
        .child(glyph.to_string())
        .child(
            div()
                .text_size(px(9.5))
                .text_color(rgb(INK3()))
                .child(player.keymap.display(action)),
        )
}

/// A ghost command whose stride-10 sibling is the same command at a
/// different scale (DESIGN §4): one glyph, both chords shown beside each
/// other rather than a second glyph -- the cheap fix VIOLATION 1 asked for,
/// reusing the ghost grammar instead of a new widget.
fn ghost_dual(player: &Player, glyph: &str, action: ActionId, stride: ActionId) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .py(px(6.))
        .rounded(px(3.))
        .text_size(px(13.))
        .text_color(rgb(INK2()))
        .child(glyph.to_string())
        .child(
            div()
                .flex()
                .gap(px(4.))
                .text_size(px(9.5))
                .text_color(rgb(INK3()))
                .child(player.keymap.display(action))
                .child(player.keymap.display(stride)),
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
/// corner-cut: DESIGN §9 wants chords surfaced *in place beside their
/// controls* across every region; this is one scrolling list plate instead,
/// anchored over the bench/ledger footprint so it never reaches the screen.
/// Ceiling: DESIGN §12 step 7's full geographic pass (each region draws its
/// own rows while held).
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
                        .text_size(px(9.))
                        .text_color(rgb(INK3()))
                        .child(category.label().to_uppercase())
                        .into_any_element(),
                    KeyRow::Act(action) => div()
                        .flex()
                        .justify_between()
                        .gap(px(12.))
                        .text_size(px(9.5))
                        .child(div().text_color(rgb(INK2())).child(action.label()))
                        .child(div().text_color(rgb(INK3())).child(player.keymap.display(action)))
                        .into_any_element(),
                    KeyRow::Fixed(i) => {
                        let f = &keymap::FIXED[i];
                        div()
                            .flex()
                            .justify_between()
                            .gap(px(12.))
                            .text_size(px(9.5))
                            .child(div().text_color(rgb(INK2())).child(f.label))
                            .child(div().text_color(rgb(INK3())).child(f.chord.clone()))
                            .into_any_element()
                    }
                })),
        )
}

/// The spine: 56px, left, full height. Every command as ghost glyph + chord,
/// grouped by task frequency (DESIGN §5) -- transport first (every second),
/// then the cut machinery (DESIGN §6, every cut), then the split and the one
/// boxed exception is left to the time band, which is where Export lives.
fn spine(player: &Player) -> impl IntoElement {
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
        .child(ghost(player, "▶", ActionId::Play))
        .child(ghost_dual(player, "‹", ActionId::WalkCutPrev, ActionId::WalkCutPrev10))
        .child(ghost_dual(player, "›", ActionId::WalkCutNext, ActionId::WalkCutNext10))
        .child(ghost(player, "{", ActionId::SelectPrev))
        .child(ghost(player, "}", ActionId::SelectNext))
        .child(ghost(player, "[", ActionId::TrimIn))
        .child(ghost(player, "]", ActionId::TrimOut))
        .child(ghost(player, "↻", ActionId::LoopTrim))
        .child(ghost(player, "✂", ActionId::Cut))
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
                .relative()
                .child(player.picture_area(position, window, cx)),
        )
        .children(two_up)
}

/// Fixed-height strip under the screen: timecode leads, ghost transport, cut
/// readout, the contact strip, boxed Export at the end (DESIGN §5).
fn time_band(player: &Player) -> impl IntoElement {
    let position = player.active_session().map_or(0., PlaybackSession::now);
    let tc = timecode(position, player.active_fps());
    // The odometer (DESIGN §6): the subject cut's own place among its
    // lane's, or an empty readout with nothing marked -- a plate that reads
    // blank rather than one that lies about a cut zero.
    let readout = player
        .selected
        .anchor()
        .and_then(|(lane, idx)| {
            player
                .session
                .as_ref()
                .map(|s| format!("{}/{}", idx + 1, s.lane_clips(lane).len()))
        })
        .unwrap_or_else(|| "—/—".to_string());
    div()
        .id("stance-time-band")
        .flex_none()
        .h(px(TIME_BAND_H))
        .bg(rgb(DARK_PANEL()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .items_center()
        .gap(px(16.))
        .px(px(12.))
        // The most-read element anchors its region (DESIGN §5): the
        // timecode leads, 13px hero, 700, colons in `ink3`.
        .child(
            div()
                .text_size(px(13.))
                .text_color(rgb(INK1()))
                .child(tc),
        )
        .child(ghost(player, if player.transport().is_playing() { "❚❚" } else { "▶" }, ActionId::Play))
        // The cut readout: a plate, mono, the odometer DESIGN §6 asks for.
        .child(
            div()
                .flex_none()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(2.))
                .bg(rgb(DARK_CANVAS()))
                .text_size(px(10.5))
                .text_color(rgb(INK1()))
                .child(readout),
        )
        .child(div().flex_1())
        // Boxed Export: the one bordered control on the surface (DESIGN §4).
        .child(
            div()
                .id("stance-export")
                .flex_none()
                .px(px(10.))
                .py(px(6.))
                .rounded(px(3.))
                .border_1()
                .border_color(rgb(DARK_HAIRLINE()))
                .bg(rgb(DARK_RAISED()))
                .text_size(px(10.5))
                .text_color(rgb(INK1()))
                .cursor_pointer()
                .child(format!("Export ({})", player.keymap.display(ActionId::Export))),
        )
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
        .text_size(px(10.))
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
        .child(div().text_size(px(9.5)).text_color(rgb(INK1())).child(identity))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_size(px(9.5))
                .text_color(rgb(INK3()))
                .child(last_action),
        )
        .children(export.map(|e| div().text_size(px(9.5)).text_color(rgb(INK2())).child(e)))
        .child(div().flex_none().text_size(px(9.5)).text_color(rgb(INK2())).child(tc))
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
    let window_h = window.viewport_size().height;
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
                // hook: §12 step 7 -- the export card (and the notice plates
                // that would announce an export's progress) has no stance
                // surface yet, so it is suppressed here rather than left to
                // open a card nothing on screen can draw. `ShowActions` now
                // has one ([`keys_overlay`]) so it is no longer on this list.
                if action == ActionId::Export {
                    return;
                }
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
        .child(spine(player))
        .child(
            div()
                .id("stance-centre")
                .relative()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(screen(player, position, window, cx))
                .child(time_band(player))
                .child(bench(player, cx))
                .child(ledger(player, position))
                .when_some(player.notices.front().cloned(), |el, n| {
                    el.child(notice_plate(n))
                })
                .when(player.keys_open, |el| el.child(keys_overlay(player))),
        )
        .child(dock(player, window_h, cx))
}
