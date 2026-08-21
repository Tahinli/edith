//! The spine's own content (DESIGN.md §5, MOCK-SPEC.md "Spine"): grouped
//! task-frequency clusters, each row a glyph stacked over its own chord --
//! not the flat ungrouped list of glyph-beside-chord rows the shipped room
//! drew. `stance.rs::spine()` keeps the panel frame (width, surface,
//! border); this module owns the rows inside it.

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
        .px(px(4.))
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
        .px(px(4.))
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

/// Two commands side by side in one row -- the opposite-direction pairs
/// (walk cuts, no-aim trim) that share a row in the mock rather than each
/// eating a full row of the spine's own height.
fn pair(left: impl IntoElement, right: impl IntoElement) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
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
        .child(trim_control(player.loop_trim.is_some(), player, cx))
        .child(glyph(
            "spine-loop-trim",
            "↻",
            ActionId::LoopTrim,
            player.loop_trim.is_some(),
            false,
            player,
            cx,
        ))
        // ToggleSnap (homeless per this task's audit): editing-adjacent --
        // it decides where a drag lands (keymap.rs's own category puts it
        // under Editing, not View) -- so it earns the CUT group beside the
        // trim/loop-trim it changes the feel of, not the quiet ROOM cluster
        // below. Active fill mirrors loop-trim's own on/off convention.
        .child(glyph(
            "spine-toggle-snap",
            "Sn",
            ActionId::ToggleSnap,
            player.snap,
            false,
            player,
            cx,
        ))
        // VIEW group, added this task: ZoomIn/ZoomOut/ZoomFit (keymap.rs) had
        // real, already-working handlers (`Player::zoom`/`zoom_fit`, wired
        // into the legacy `ui/toolbar.rs`) but no home anywhere in the
        // darkroom -- a DESIGN §9 "nothing lives only on a key" violation.
        // Placed between CUT and TRACK per §11 check 1 (task frequency):
        // reading the timeline at a different scale happens many times a
        // session (more than the once-per-project lane add below it), but
        // less than walking/trimming cuts above it.
        .child(group_head("view"))
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
        .child(group_head("track"))
        .child(glyph(
            "spine-add-video",
            "+V",
            ActionId::AddVideoLane,
            false,
            false,
            player,
            cx,
        ))
        .child(glyph(
            "spine-add-audio",
            "+A",
            ActionId::AddAudioLane,
            false,
            false,
            player,
            cx,
        ))
        // The subtitle trio (homeless per this task's audit): AddSubtitleLane
        // joins +V/+A on the same "+letter" grammar per this task's own
        // instruction ("+S joins +V/+A in TRACK"); remove/import are "the
        // subtitle work" the instruction says to place them beside, so they
        // share the row underneath rather than each eating a full row (the
        // same space-saving `pair` the walk-cuts and zoom in/out already
        // use). ToggleSubtitles (whether the cues draw over the picture at
        // all) is the fourth subtitle-lane command with no home anywhere in
        // the darkroom -- filed here too, "CC" being the glyph every player
        // already uses for exactly this toggle, rather than in the quiet
        // ROOM cluster below: cue visibility is judged shot to shot, closer
        // in frequency to a view toggle used often than to a once-a-session
        // preference.
        .child(glyph(
            "spine-add-subtitle",
            "+S",
            ActionId::AddSubtitleLane,
            false,
            false,
            player,
            cx,
        ))
        .child(pair(
            glyph(
                "spine-remove-subtitle",
                "−S",
                ActionId::RemoveSubtitleLane,
                false,
                false,
                player,
                cx,
            ),
            glyph(
                "spine-import-subtitles",
                "↓S",
                ActionId::ImportSubtitles,
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
        ))
        // ROOM: the once-a-session/once-a-project acts this task's charter
        // names explicitly (theme, proxies, screenshot, fullscreen) -- ink-
        // demoted (`quiet: true`) and paired two-to-a-row to keep the group
        // cheap, the same "rare acts live out of the way" pattern DESIGN §2
        // names for re-inking, applied here to the spine's own bottom
        // instead of a right-click menu (these four are toggled/fired by a
        // click, not by picking a source dot, so a context menu is not
        // their shape). Left off this list: Save, which sits beside Export
        // in the time band instead (see timeband_stance.rs) because the
        // ledger already gives the saved/unsaved state a home there, and
        // keymap.rs's own `Category::File` files Save beside Export and
        // Screenshot on the strength of what each one writes.
        .child(group_head("room"))
        .child(pair(
            glyph("spine-theme", "H", ActionId::Theme, false, true, player, cx),
            glyph(
                "spine-proxies",
                "P",
                ActionId::ToggleProxies,
                player.proxies_on,
                true,
                player,
                cx,
            ),
        ))
        .child(pair(
            glyph(
                "spine-auto-proxies",
                "AP",
                ActionId::ToggleAutoProxies,
                player.auto_proxies_on,
                true,
                player,
                cx,
            ),
            glyph(
                "spine-fullscreen",
                "FS",
                ActionId::Fullscreen,
                false,
                true,
                player,
                cx,
            ),
        ))
        .child(glyph(
            "spine-screenshot",
            "Sh",
            ActionId::Screenshot,
            false,
            true,
            player,
            cx,
        ))
        // The settings page's own visible door (user complaint: "some
        // options are only reachable via keyboard shortcut") -- a chord
        // alone is a defect class this room has hit repeatedly, so this
        // glyph sits beside the other once-a-session ROOM acts even though
        // its chord (`^,`) is a preference, not a mnemonic.
        .child(glyph(
            "spine-settings",
            "St",
            ActionId::Settings,
            player.settings_open,
            true,
            player,
            cx,
        ))
        .child(div().flex_1())
}
