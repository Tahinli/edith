//! The title row: one thin row above the lanes, there only while the project has titles. Each
//! title is a block of the title colour with its first line of words; a press selects it (its
//! section opens in the Clip tab) and a drag along the row moves it, a double-click puts the
//! keyboard in its words.

use super::*;
use crate::player::titling::ScrubField;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::theme::{ClipKind, tokens};

/// The row's height: a subtitle lane's.
pub(super) const ROW_H: f32 = LANE_SUB_H;

pub(super) fn titles_row(player: &Player, scale: Scale, cx: &mut Context<Player>) -> Option<impl IntoElement + use<>> {
    let session = player.session.as_ref()?;
    if session.titles().is_empty() {
        return None;
    }
    let t = tokens();
    let tone = t.clip(ClipKind::Title);
    let fps = player.fps.max(1.);
    let picked = player.title_target().map(|(i, _)| i);
    let blocks: Vec<_> = session
        .titles()
        .iter()
        .enumerate()
        .map(|(i, stored)| {
            let title = player.shown_title(stored);
            let left = scale.px_at(f64::from(title.start) / fps);
            let span = scale.width_px(f64::from(title.frames) / fps);
            let width = clip_width(span);
            // The chip's own name, cut with an ellipsis to the room the chip has (its padding and
            // border take 14px) like a clip's name; nothing when not even a letter would show.
            let first = super::clip::fit_label(cx, title.text.lines().next().unwrap_or(""), width - 14.).unwrap_or_default();
            let on = picked == Some(i);
            div()
                .id(("bench-title", i))
                .absolute()
                .top(px(2.))
                .bottom(px(2.))
                .left(px(left))
                .w(px(width))
                .flex()
                .items_center()
                .px(px(6.))
                .overflow_hidden()
                .rounded(px(3.))
                .bg(rgb(tone.strip))
                .border_1()
                .border_color(if on { rgb(t.ink1).into() } else { gpui::transparent_black() })
                .cursor_pointer()
                .type_style(type_scale::caption())
                .text_color(rgb(tone.label))
                .child(div().min_w(px(0.)).truncate().child(first))
                .tooltip(crate::ui::widgets::tip_hover("Title", "double-click to type", None))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        if this.title_target().map(|(i, _)| i) != Some(i) {
                            this.open_title(i, false, cx);
                        }
                        this.begin_title_scrub(ScrubField::Move, f32::from(event.position.x), cx);
                    }),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    if event.click_count() >= 2 {
                        this.open_title(i, true, cx);
                    }
                }))
        })
        .collect();
    Some(
        div()
            .id("bench-titles")
            .flex_none()
            .h(px(ROW_H))
            .flex()
            .child(
                div()
                    .flex_none()
                    .w(px(HEAD_W))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .bg(rgb(t.e1))
                    .border_r_1()
                    .border_b_1()
                    .border_color(rgb(t.e0))
                    .child(Icon::Type.el(IconSize::Inline, t.ink2))
                    .child(div().type_style(type_scale::section()).text_color(rgb(t.ink1)).child("Titles")),
            )
            .child(
                div()
                    .id("bench-titles-bed")
                    .relative()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .overflow_hidden()
                    .bg(rgb(t.well))
                    .border_b_1()
                    .border_color(rgba(t.line))
                    .children(blocks),
            ),
    )
}
