//! The spine's own content (DESIGN.md §5, MOCK-SPEC.md "Spine"): grouped
//! task-frequency clusters, each row a glyph stacked over its own chord --
//! not the flat ungrouped list of glyph-beside-chord rows the shipped room
//! drew. `stance.rs::spine()` keeps the panel frame (width, surface,
//! border); this module owns the rows inside it.

use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// The glyph icon is a room verb, so it uses the label role rather than a
/// bespoke size: Archivo at `LABEL_ROW_PX`.

/// A group head: uppercase Archivo 700 in `ink3`, +0.14em tracking
/// (DESIGN §3) -- reused as its own small fn rather than `stance::section_head`
/// because that one is private to its module and carries a different
/// vertical rhythm (a region label, not a spine group head).
fn group_head(label: &str) -> impl IntoElement {
    div()
        .flex_none()
        .pt(px(6.))
        .line_height(relative(1.1))
        .type_style(type_scale::head())
        .text_color(rgb(INK3()))
        .child(label.to_uppercase())
}

/// One command: glyph over its chord, centred (MOCK-SPEC.md "Spine" --
/// "Not glyph-beside-chord on one line"). The chord is read live off
/// [`Keymap::display`], never a literal, so a rebind can never leave the
/// spine showing a stroke that no longer fires it. Ghost grammar (DESIGN
/// §4): borderless, hover is one fill step, active is `ink1` + fill, and a
/// refused verb greys with its reason on hover rather than disappearing
/// (DESIGN §8) -- the same oracle door `dock_stance::ghost_verb` opens.
/// `quiet` demotes a row a whole ink step (DESIGN §2's ink-demotion rule,
/// applied per this task's charter to the once-a-session/once-a-project
/// acts -- theme, proxies, screenshot, fullscreen -- rather than to a
/// re-inking gesture): glyph reads `ink3` instead of `ink2`, its chord
/// `ink4` instead of `ink3`, so the ROOM group sits visibly quieter than
/// the burst-use groups above it without a second widget shape.
fn glyph(
    id: &'static str,
    glyph: &'static str,
    action: ActionId,
    active: bool,
    quiet: bool,
    player: &Player,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let enabled = player.enable(action, None);
    // The badge shows the primary chord only (FAULT 1: a chord badge is a
    // compact token, not a sentence); the tooltip keeps every stroke an
    // action answers to, and the keys overlay ([`crate::ui::stance`]) is
    // still where the full truth lives.
    let full = player.keymap.display(action);
    let say: SharedString = match enabled.why() {
        Some(why) => format!("{full} — {why}"),
        None => format!("{full} — {}", action.label()),
    }
    .into();
    let compact = player.keymap.chord(action);
    let on = enabled.yes();
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.))
        .px(px(3.))
        .py(px(1.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
        .when(!on, |d| d.opacity(0.4).cursor_not_allowed())
        .when(on, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.act(action, window, cx)
                    }),
                )
        })
        .children(hitmap::action(action, on))
        .child(
            div()
                .line_height(relative(1.05))
                // FAULT 1: the glyph is the loudest thing in its row -- BOLD,
                // not the MEDIUM weight the chord beneath it also wears.
                .type_style(type_scale::label(
                    type_scale::LABEL_ROW_PX,
                    gpui::FontWeight::BOLD,
                ))
                .text_color(rgb(if active {
                    INK1()
                } else if quiet {
                    INK3()
                } else {
                    INK2()
                }))
                .child(glyph),
        )
        .child(
            div()
                .line_height(relative(1.05))
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(if quiet { INK4() } else { INK3() }))
                .child(compact),
        )
}

/// The trim control (MOCK-SPEC "Spine"): one row read as ONE control with
/// two strokes, not two identical `±1` boxes each carrying its own `[`/`]`
/// chord underneath (the shipped defect this replaces). `−1`/`+1` distinguish
/// the two strokes visually where a doubled `±1` couldn't; the chord line
/// beneath is shared and shows both strokes together (`[ ]`), the way the
/// mock's single `±1` / `[ ]` pair reads as one thing, not two.
fn trim_control(active: bool, player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let half = |id: &'static str, txt: &'static str, action: ActionId, cx: &mut Context<Player>| {
        let enabled = player.enable(action, None);
        let full = player.keymap.display(action);
        let say: SharedString = match enabled.why() {
            Some(why) => format!("{full} — {why}"),
            None => format!("{full} — {}", action.label()),
        }
        .into();
        let on = enabled.yes();
        div()
            .id(id)
            .flex_none()
            .px(px(2.))
            .rounded(px(3.))
            .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
            .when(!on, |d| d.opacity(0.4).cursor_not_allowed())
            .when(on, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK1())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.act(action, window, cx)
                    }))
            })
            .children(hitmap::action(action, on))
            .child(
                div()
                    .line_height(relative(1.05))
                    .type_style(type_scale::label(
                        type_scale::LABEL_ROW_PX,
                        gpui::FontWeight::BOLD,
                    ))
                    .text_color(rgb(if active { INK1() } else { INK2() }))
                    .child(txt),
            )
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(1.))
        .px(px(3.))
        .py(px(1.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .child(half("spine-trim-in", "−1", ActionId::TrimIn, cx))
                .child(half("spine-trim-out", "+1", ActionId::TrimOut, cx)),
        )
        .child(
            div()
                .line_height(relative(1.05))
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .child("[ ]"),
        )
}

/// Which spine section is expanded under the pointer (density reduction,
/// user feedback 2026-08-27: "feels complex" against the always-visible
/// ~40-glyph rail). `CutMore` is the low-frequency tail of the CUT group
/// (stride-10 walks, ±1 trim, trim-to-playhead, loop-trim) sitting under
/// the burst-use walk-cut pair, which stays visible on its own; VIEW,
/// TRACK and ROOM collapse whole since every row in them is once-per-scene
/// or rarer next to EDIT/CUT's every-cut cadence (DESIGN §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpineZone {
    CutMore,
    View,
    Track,
    Room,
}

/// A collapsed section: one hairline, no prose, no repeated group word
/// (DESIGN §8's rejected-instructional-copy rule applies to labels too --
/// a collapsed group reads as a seam in the rail, not a sentence).
fn hairline() -> impl IntoElement {
    div()
        .flex_none()
        .w(px(20.))
        .h(px(1.))
        .my(px(5.))
        .bg(rgba(DARK_SEAM()))
}

/// A section that is a hairline at rest and its full row set on hover
/// (task: "hover the collapsed group's zone -> its glyphs expand in
/// place; leave -> collapse"). The wrapping div is the hover target and
/// carries the hover state onto [`Player::spine_hover`] rather than any
/// local widget state, since gpui re-renders the whole spine on
/// `cx.notify()` and this is the one flag the whole rail reads. Every
/// glyph inside `rows` keeps its own `cx.listener` click handler, so a
/// verb revealed by hover is exactly as clickable as an always-visible one
/// once it is on screen -- collapsing only removes it from the layout, it
/// never disables the handler.
fn collapsible<T: IntoElement>(
    id: &'static str,
    zone: SpineZone,
    player: &Player,
    rows: T,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<T> {
    let hovered = player.spine_hover == Some(zone);
    div()
        .id(id)
        .flex_none()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .on_hover(cx.listener(move |this, now_hovered: &bool, _window, cx| {
            this.spine_hover = if *now_hovered {
                Some(zone)
            } else if this.spine_hover == Some(zone) {
                None
            } else {
                this.spine_hover
            };
            cx.notify();
        }))
        .child(if hovered {
            rows.into_any_element()
        } else {
            hairline().into_any_element()
        })
}

/// Two commands side by side in one row -- the opposite-direction pairs
/// (walk cuts, no-aim trim) that share a row in the mock rather than each
/// eating a full row of the spine's own height.
fn pair(left: impl IntoElement, right: impl IntoElement) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .w_full()
        .items_center()
        .justify_center()
        // 4px, not 6: two two-glyph halves plus their own padding used to
        // measure wider than the 56px rail and bled past its left edge
        // (measured at 1280x720: the `AP`/`FS` row started at x=0, outside
        // the spine's own surface).
        .gap(px(4.))
        .child(left)
        .child(right)
}

/// The spine's rows, grouped by task frequency (DESIGN §5, §11 check 1):
/// EDIT (split, delete, undo -- every edit), CUT (walk/trim/loop-trim --
/// every cut), TRACK (add a lane -- once per project, cheapest land at the
/// bottom of the frequent group). `?` stays on `stance::spine`'s own frame,
/// not duplicated here.
///
/// Gap, named rather than shipped as a dead control: the mock's LEAN row
/// (`⊡` / `\`, "lean/zen mode") has no [`ActionId`] anywhere in
/// `keymap.rs` -- `Fullscreen` is the nearest existing action but it is a
/// different verb (the platform's own fullscreen, bound to `f11`, not a
/// zen/lean UI mode) so reusing it under the mock's glyph would be exactly
/// the fake-control this task's own instructions forbid. Left out; the
/// group and its bottom anchor do not exist here.
pub(crate) fn render(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .id("spine-stance-rows")
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        // FAULT 1: rows close under their group head, real space (the group
        // head's own `pt`) only between one group and the next -- not one
        // uniform gap stretching every row down the window. The gap itself
        // is now the smallest gpui will draw (1px) rather than the 2px that
        // still read as "spread" against the mock's tight top-third rail.
        .gap(px(1.))
        .overflow_y_scroll()
        .child(group_head("edit"))
        .child(glyph(
            "spine-split",
            "||",
            ActionId::Cut,
            false,
            false,
            player,
            cx,
        ))
        .child(glyph(
            "spine-delete",
            "⊂⊃",
            ActionId::Delete,
            false,
            false,
            player,
            cx,
        ))
        .child(pair(
            glyph("spine-undo", "↺", ActionId::Undo, false, false, player, cx),
            // Redo had a legacy toolbar button (`toolbar.rs:347`) but no
            // darkroom home -- Undo got a glyph here and Redo did not.
            // Same row, same door (`this.act`), the pair anatomy the
            // cut-prev/cut-next row below already uses.
            glyph("spine-redo", "⟳", ActionId::Redo, false, false, player, cx),
        ))
        .child(group_head("cut"))
        .child(pair(
            glyph(
                "spine-cut-prev",
                "‹",
                ActionId::WalkCutPrev,
                false,
                false,
                player,
                cx,
            ),
            glyph(
                "spine-cut-next",
                "›",
                ActionId::WalkCutNext,
                false,
                false,
                player,
                cx,
            ),
        ))
        // Everything past the walk-cut pair above is CUT's low-frequency
        // tail (stride-10 walks, ±1 trim, trim-to-playhead, loop-trim) --
        // collapsed to a hairline, revealed on hover of this zone.
        .child(collapsible(
            "spine-zone-cut-more",
            SpineZone::CutMore,
            player,
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(pair(
                    glyph(
                        "spine-cut-prev-ten",
                        "‹10",
                        ActionId::WalkCutPrev10,
                        false,
                        false,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-cut-next-ten",
                        "10›",
                        ActionId::WalkCutNext10,
                        false,
                        false,
                        player,
                        cx,
                    ),
                ))
                .child(trim_control(player.loop_trim.is_some(), player, cx))
                .child(pair(
                    // Trim-to-playhead (debt #42): the keyboard's own version
                    // of the pointer's drag-to-a-spot trim, beside the nudge
                    // pair it shares its primitive with.
                    glyph(
                        "spine-trim-in-playhead",
                        "[▮",
                        ActionId::TrimInToPlayhead,
                        false,
                        false,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-trim-out-playhead",
                        "▮]",
                        ActionId::TrimOutToPlayhead,
                        false,
                        false,
                        player,
                        cx,
                    ),
                ))
                .child(glyph(
                    "spine-loop-trim",
                    "↻",
                    ActionId::LoopTrim,
                    player.loop_trim.is_some(),
                    false,
                    player,
                    cx,
                )),
            cx,
        ))
        // VIEW group: ZoomIn/ZoomOut/ZoomFit/ToggleSnap all read the
        // timeline at a scale rather than cutting it -- real handlers with
        // a darkroom home, but well under EDIT/CUT's every-cut cadence
        // (DESIGN §6), so the whole group collapses under one hairline
        // rather than carrying its own always-visible "view" word.
        .child(collapsible(
            "spine-zone-view",
            SpineZone::View,
            player,
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(pair(
                    glyph(
                        "spine-zoom-out",
                        "−",
                        ActionId::ZoomOut,
                        false,
                        false,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-zoom-in",
                        "+",
                        ActionId::ZoomIn,
                        false,
                        false,
                        player,
                        cx,
                    ),
                ))
                .child(glyph(
                    "spine-zoom-fit",
                    "⊡",
                    ActionId::ZoomFit,
                    false,
                    false,
                    player,
                    cx,
                ))
                // ToggleSnap: decides where a drag lands, which is what the
                // eye reads the zoom scale against -- kept beside the zoom
                // controls it shares this zone with. Active fill mirrors
                // loop-trim's own on/off convention.
                .child(glyph(
                    "snap",
                    "Sn",
                    ActionId::ToggleSnap,
                    player.snap,
                    false,
                    player,
                    cx,
                )),
            cx,
        ))
        // TRACK: once-per-project lane adds, collapsed whole.
        .child(collapsible(
            "spine-zone-track",
            SpineZone::Track,
            player,
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(pair(
                    glyph(
                        "spine-add-video",
                        "+V",
                        ActionId::AddVideoLane,
                        false,
                        false,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-add-audio",
                        "+A",
                        ActionId::AddAudioLane,
                        false,
                        false,
                        player,
                        cx,
                    ),
                ))
                // The subtitle lane pair: add beside remove, one row, the
                // same "+letter"/"−letter" grammar +V/+A above it already
                // reads in. Import (`↓S`) left this rail entirely -- it is
                // an import, so it lives in the dock's own IMPORT list
                // beside "Add files" (`dock_stance`), where an editor
                // already goes to bring a file into the room.
                .child(pair(
                    glyph(
                        "spine-add-subtitle",
                        "+S",
                        ActionId::AddSubtitleLane,
                        false,
                        false,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-remove-subtitle",
                        "−S",
                        ActionId::RemoveSubtitleLane,
                        false,
                        false,
                        player,
                        cx,
                    ),
                ))
                .child(glyph(
                    "spine-toggle-subtitles",
                    "CC",
                    ActionId::ToggleSubtitles,
                    player.subs_on,
                    false,
                    player,
                    cx,
                )),
            cx,
        ))
        // ROOM: the three acts that are *acts* rather than settings (user
        // 2026-08-21: "the spine menu is too crowded"). Theme, proxies and
        // auto-proxies stayed off this rail entirely (settings_stance).
        // Once-a-session/once-a-project cadence -- collapsed whole, same as
        // VIEW/TRACK above it.
        .child(collapsible(
            "spine-zone-room",
            SpineZone::Room,
            player,
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(pair(
                    glyph(
                        "spine-settings",
                        "St",
                        ActionId::Settings,
                        player.settings_open,
                        true,
                        player,
                        cx,
                    ),
                    glyph(
                        "spine-screenshot",
                        "Sh",
                        ActionId::Screenshot,
                        false,
                        true,
                        player,
                        cx,
                    ),
                ))
                .child(glyph(
                    "spine-fullscreen",
                    "FS",
                    ActionId::Fullscreen,
                    false,
                    true,
                    player,
                    cx,
                )),
            cx,
        ))
        .child(div().flex_1())
}
