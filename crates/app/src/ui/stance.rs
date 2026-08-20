//! DESIGN.md §5 -- the stance: fixed geography behind `EDITH_DARKROOM`, drawn
//! whole here rather than folded into the legacy four-region tree so the two
//! can be told apart, and swapped, with a single `if` in `render.rs`.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::*;
use crate::ui::dock_stance;

/// Left rail, full height (DESIGN §5).
const SPINE_W: f32 = 56.;
/// Fixed strip under the screen: timecode, transport, cut readout, contact
/// strip, Export -- all placeholder at this step.
const TIME_BAND_H: f32 = 88.;
/// corner-cut: the lane bed has no lanes to size itself against yet, so this
/// is a placeholder height rather than a measured one. Ceiling: replaced by
/// the real lane stack's own height once DESIGN §12 step 4 fills the bench.
const BENCH_H: f32 = 160.;
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
        .child(ghost(player, "‹", ActionId::WalkCutPrev))
        .child(ghost(player, "›", ActionId::WalkCutNext))
        .child(ghost(player, "[", ActionId::TrimIn))
        .child(ghost(player, "]", ActionId::TrimOut))
        .child(ghost(player, "↻", ActionId::LoopTrim))
        .child(ghost(player, "✂", ActionId::Cut))
}

/// The picture region: top of the centre column, takes the remaining space,
/// never occluded (DESIGN §5). Reuses [`Player::picture_area`] rather than a
/// second image element -- the darkroom draws the same picture the legacy
/// tree does -- and layers the two-up OUT|IN judging over it at rest on a
/// cut (DESIGN §6).
fn screen(player: &mut Player, position: f64, window: &mut Window, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("stance-screen")
        .flex_1()
        .min_h(px(0.))
        .relative()
        // §4: room chrome takes 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        .child(player.picture_area(position, window, cx))
        .children(player.two_up())
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
fn bench() -> impl IntoElement {
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
        .items_center()
        .px(px(12.))
        .child(section_head("bench"))
        // hook: §12 step 4 -- lanes V/A/S; ink spine + thumbnails/waveform + name plate + splice gaps.
}

/// Thin strip at the bottom of the centre column: project identity, last
/// action, export progress, position. Notices rise from here (DESIGN §5, §8).
fn ledger() -> impl IntoElement {
    div()
        .id("stance-ledger")
        .flex_none()
        .h(px(LEDGER_H))
        .bg(rgb(DARK_PANEL()))
        .border_t_1()
        .border_color(rgba(DARK_SEAM()))
        .flex()
        .items_center()
        .px(px(12.))
        .child(section_head("ledger"))
        // hook: §12 step 7 -- project identity/state, last action, export progress, notices.
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
                // hook: §12 step 7 -- the keys overlay and the export card
                // (and the notice plates that would announce an export's
                // progress) have no stance surface yet, so the two actions
                // that only ever show up there are suppressed here rather
                // than left to open a card nothing on screen can draw.
                if matches!(action, ActionId::ShowActions | ActionId::Export) {
                    return;
                }
                this.act(action, window, cx);
            }
        }))
        .size_full()
        .flex()
        .bg(rgb(DARK_CANVAS()))
        .child(spine(player))
        .child(
            div()
                .id("stance-centre")
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(screen(player, position, window, cx))
                .child(time_band(player))
                .child(bench())
                .child(ledger()),
        )
        .child(dock(player, window_h, cx))
}
