//! DESIGN.md §5 -- the stance: fixed geography behind `EDITH_DARKROOM`, drawn
//! whole here rather than folded into the legacy four-region tree so the two
//! can be told apart, and swapped, with a single `if` in `render.rs`.
//!
//! This step lays out the six regions and nothing else: correct geometry,
//! correct surfaces, a section-head label per region. No content, no
//! interaction, no cut machinery -- `// hook:` comments below mark where the
//! later steps in DESIGN §12's package attach.

use crate::*;

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

/// The spine: 56px, left, full height. Every command as ghost glyph + chord
/// lands here later, grouped by task frequency (DESIGN §5).
fn spine() -> impl IntoElement {
    div()
        .id("stance-spine")
        .flex_none()
        .w(px(SPINE_W))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_r_1()
        .border_color(rgb(DARK_SEAM()))
        .flex()
        .flex_col()
        .items_center()
        .py(px(8.))
        .child(section_head("spine"))
        // hook: §12 step 3 -- ghost glyph + chord per command, task-frequency order.
}

/// The picture region: top of the centre column, takes the remaining space,
/// never occluded (DESIGN §5).
fn screen() -> impl IntoElement {
    div()
        .id("stance-screen")
        .flex_1()
        .min_h(px(0.))
        // §4: room chrome takes 0 radius.
        .rounded(px(0.))
        .bg(rgb(DARK_CANVAS()))
        .flex()
        .items_center()
        .justify_center()
        .child(section_head("screen"))
        // hook: §12 step 3 -- the picture, and two-up OUT|IN judging at rest on a cut.
}

/// Fixed-height strip under the screen: timecode leads, ghost transport, cut
/// readout, the contact strip, boxed Export at the end (DESIGN §5).
fn time_band() -> impl IntoElement {
    div()
        .id("stance-time-band")
        .flex_none()
        .h(px(TIME_BAND_H))
        .bg(rgb(DARK_PANEL()))
        .border_t_1()
        .border_color(rgb(DARK_SEAM()))
        .flex()
        .items_center()
        .px(px(12.))
        .child(section_head("time band"))
        // hook: §12 step 3 -- timecode, ghost transport, cut readout, contact strip, Export.
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
        .border_color(rgb(DARK_SEAM()))
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
        .border_color(rgb(DARK_SEAM()))
        .flex()
        .items_center()
        .px(px(12.))
        .child(section_head("ledger"))
        // hook: §12 step 7 -- project identity/state, last action, export progress, notices.
}

/// One tab of the dock's Src/Clip pair -- a plate (2px radius), `ink1` when
/// active and `ink2` at rest, never a hue (DESIGN §4).
fn dock_tab(label: &str, active: bool) -> impl IntoElement {
    div()
        .flex_1()
        .rounded(px(2.))
        .py(px(6.))
        .text_size(px(10.5))
        .text_color(rgb(if active { INK1() } else { INK2() }))
        .bg(rgb(if active { DARK_RAISED() } else { DARK_PANEL() }))
        .child(label.to_string())
}

/// The dock: the only side panel, right, fixed width, carrying the Src/Clip
/// tab pair (DESIGN §5).
fn dock() -> impl IntoElement {
    div()
        .id("stance-dock")
        .flex_none()
        .w(px(DOCK_W))
        .h_full()
        .bg(rgb(DARK_PANEL()))
        .border_l_1()
        .border_color(rgb(DARK_SEAM()))
        .flex()
        .flex_col()
        .child(
            div()
                .id("stance-dock-tabs")
                .flex_none()
                .flex()
                .gap(px(4.))
                .p(px(8.))
                .border_b_1()
                .border_color(rgb(DARK_HAIRLINE()))
                .child(dock_tab("Src", true))
                .child(dock_tab("Clip", false)),
        )
        .child(section_head("dock"))
        // hook: §12 step 4 -- Sources assembly (filter, usage chips, import) / Clip verbs
        // (Speed / Colour / Transform / EQ as ghost verbs over param rows).
}

/// The whole stance: spine, screen, time band, bench, ledger, dock, in the
/// order DESIGN §5 draws them. `player` and the window/context pair are
/// unread by this skeleton -- later steps in the §12 package are what fill
/// each region from live state (cut position, dock tab, lane contents).
pub(crate) fn render(
    _player: &mut Player,
    _window: &mut Window,
    _cx: &mut Context<Player>,
) -> impl IntoElement {
    div()
        .id("stance-room")
        .size_full()
        .flex()
        .bg(rgb(DARK_CANVAS()))
        .child(spine())
        .child(
            div()
                .id("stance-centre")
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(screen())
                .child(time_band())
                .child(bench())
                .child(ledger()),
        )
        .child(dock())
}
