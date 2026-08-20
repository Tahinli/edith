//! The spine's own content (DESIGN.md §5, MOCK-SPEC.md "Spine"): grouped
//! task-frequency clusters, each row a glyph stacked over its own chord --
//! not the flat ungrouped list of glyph-beside-chord rows the shipped room
//! drew. `stance.rs::spine()` keeps the panel frame (width, surface,
//! border); this module owns the rows inside it.
//!
//! corner-cut: sizes below are plain `px()` literals matching DESIGN §3's
//! scale (13px glyph, 9.5px chord, 9px head) rather than calls into a
//! `ui::type_scale` API -- that module had not landed in this tree at the
//! time this was written. Ceiling: swap these three constants for its
//! tokens once it lands; nothing else here would change.

use crate::*;

const GLYPH_SIZE: f32 = 13.;
const CHORD_SIZE: f32 = 9.5;
const HEAD_SIZE: f32 = 9.;

/// A group head: 9px uppercase Archivo 700(-weight token, same as every
/// other section head in the stance) in `ink3`, +0.14em tracking (DESIGN
/// §3) -- reused as its own small fn rather than `stance::section_head`
/// because that one is private to its module and carries a different
/// vertical rhythm (a region label, not a spine group head).
fn group_head(label: &str) -> impl IntoElement {
    div()
        .flex_none()
        .pt(px(6.))
        .text_size(px(HEAD_SIZE))
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
fn glyph(
    id: &'static str,
    glyph: &'static str,
    action: ActionId,
    active: bool,
    player: &Player,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    let enabled = player.enable(action, None);
    let key = player.keymap.display(action);
    let say: SharedString = match enabled.why() {
        Some(why) => format!("{key} — {why}"),
        None => format!("{key} — {}", action.label()),
    }
    .into();
    let on = enabled.yes();
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .px(px(4.))
        .py(px(4.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .tooltip(move |_, cx| cx.new(|_| Tip(say.clone())).into())
        .when(!on, |d| d.opacity(0.4).cursor_not_allowed())
        .when(on, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.act(action, window, cx)
                }))
        })
        .child(
            div()
                .text_size(px(GLYPH_SIZE))
                .text_color(rgb(if active { INK1() } else { INK2() }))
                .child(glyph),
        )
        .child(
            div()
                .text_size(px(CHORD_SIZE))
                .text_color(rgb(INK3()))
                .child(key),
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
        .gap(px(10.))
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
        .gap(px(10.))
        .overflow_y_scroll()
        .child(group_head("edit"))
        .child(glyph("spine-split", "||", ActionId::Cut, false, player, cx))
        .child(glyph("spine-delete", "⊂⊃", ActionId::Delete, false, player, cx))
        .child(glyph("spine-undo", "↺", ActionId::Undo, false, player, cx))
        .child(group_head("cut"))
        .child(pair(
            glyph("spine-cut-prev", "‹", ActionId::WalkCutPrev, false, player, cx),
            glyph("spine-cut-next", "›", ActionId::WalkCutNext, false, player, cx),
        ))
        .child(pair(
            glyph(
                "spine-trim-in",
                "±1",
                ActionId::TrimIn,
                player.loop_trim.is_some(),
                player,
                cx,
            ),
            glyph(
                "spine-trim-out",
                "±1",
                ActionId::TrimOut,
                player.loop_trim.is_some(),
                player,
                cx,
            ),
        ))
        .child(glyph(
            "spine-loop-trim",
            "↻",
            ActionId::LoopTrim,
            player.loop_trim.is_some(),
            player,
            cx,
        ))
        .child(group_head("track"))
        .child(glyph("spine-add-video", "+V", ActionId::AddVideoLane, false, player, cx))
        .child(glyph("spine-add-audio", "+A", ActionId::AddAudioLane, false, player, cx))
        .child(div().flex_1())
}
