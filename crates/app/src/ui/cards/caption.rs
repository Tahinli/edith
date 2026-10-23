//! The caption panel of the Clip tab: what a selected caption says and where it sits. The words
//! are typed in place, the start and end are number fields that scrub or type, the length reads
//! off them, and the caption's own actions sit under the numbers. The Subtitle style section
//! sits under the panel. Every write is one cue edit, so one undo step.

use super::common::*;
use crate::player::caption_cues::{CueMenu, CueNum, cue_secs};
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{self, ButtonSize, ButtonStyle, NumberEdit};
use crate::ui::type_scale::Typeset;
use crate::*;

/// The label column's width, as in the style card, so the two sections line up.
const LABEL_W: f32 = 112.;

/// How many words the header names the caption by.
const HEAD_WORDS: usize = 4;

/// One caption action button, a third of the row: the caller's run when it can run, its reason on
/// hover when not. The short `label` fits the row; the tooltip names the action in full. A button
/// that cannot run ignores the press.
#[allow(clippy::too_many_arguments)]
fn caption_action(
    id: &'static str,
    label: &'static str,
    full: &'static str,
    icon: Icon,
    why: Option<&'static str>,
    m: CueMenu,
    run: fn(&mut Player, CueMenu, &mut Context<Player>),
    cx: &mut Context<Player>,
) -> Div {
    let button = kit::Button::new(id)
        .label(label)
        .icon(icon)
        .style(ButtonStyle::Secondary)
        .size(ButtonSize::Small)
        .enabled(why.is_none())
        .build()
        .w_full()
        .tooltip(crate::ui::widgets::tip_hover(full, "", why));
    let button = match why {
        Some(_) => button,
        None => button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            run(this, m, cx);
        })),
    };
    div().flex_1().min_w(px(0.)).child(button)
}

impl Player {
    /// The panel of the selected caption, under a header like a clip's. `None` while no caption
    /// is selected.
    pub(crate) fn caption_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (lane, track, cue, start, end) = self.sel_cue_span()?;
        let t = crate::ui::theme::tokens();
        let session = self.session.as_ref()?;
        let subtitles = session.subtitles().get(track)?;
        let c = subtitles.cues.get(cue)?;
        let count = subtitles.cues.len();
        let picture = c.image.is_some();
        let typing = self.cue_edit.as_ref().filter(|e| (e.track, e.cue) == (track, cue) && e.in_panel);
        let words = match typing {
            Some(e) => e.text.clone(),
            None => c.text.clone(),
        };
        let locked = session.lane_locked(lane);
        let shown: AnyElement = match (picture, typing.is_some(), words.is_empty()) {
            (true, _, _) => SharedString::from("Picture caption").into_any_element(),
            (_, true, _) if typing.is_some_and(|e| e.all) => div().bg(rgba(t.accent_wash)).child(words.clone()).into_any_element(),
            (_, true, _) => SharedString::from(format!("{words}\u{258f}")).into_any_element(),
            (_, false, true) => SharedString::from("Type the words").into_any_element(),
            _ => SharedString::from(words.clone()).into_any_element(),
        };
        let field = div()
            .id("caption-text")
            .w_full()
            .min_h(px(56.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(crate::layout::redesign::R_CONTROL))
            .bg(rgb(t.e2))
            .border_1()
            .border_color(if typing.is_some() { rgb(t.accent).into() } else { gpui::transparent_black() })
            .when(!picture, |d| d.cursor(CursorStyle::IBeam))
            .type_style(crate::ui::type_scale::body())
            .text_color(rgb(if words.is_empty() && typing.is_none() { t.ink3 } else { t.ink1 }))
            .child(shown)
            .when(!picture, |d| {
                d.tooltip(crate::ui::widgets::tip_hover("Words", "enter saves, esc cancels", Some("shift+enter starts a new line")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_panel_cue_edit(cx);
                        }),
                    )
            });
        // A number being dragged shows the value it would take; at rest it shows the caption's own.
        let scrub = self.cue_num_scrub;
        let num = |id: &'static str, label: &'static str, which: CueNum, us: i64, cx: &mut Context<Self>| {
            let shown = scrub.filter(|s| s.which == which).map_or(us, |s| s.cur);
            let typing: Option<&NumberEdit> = self.cue_num.as_ref().filter(|(w, _)| *w == which).map(|(_, e)| e);
            let enabled = !picture && !locked;
            let field = kit::number_field(id, 96., cue_secs(shown), Some("s"), typing, enabled);
            match enabled {
                false => field.cursor_not_allowed(),
                true => field
                    .tooltip(crate::ui::widgets::tip_hover(label, "", Some("Drag to change, click to type")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_cue_num_scrub(which, f32::from(event.position.x), cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        // A press that never moved is a click: the number opens for typing,
                        // unless it is open already.
                        let moved = match event {
                            ClickEvent::Mouse(m) => (m.up.position.x - m.down.position.x).abs() > px(3.),
                            _ => false,
                        };
                        let open = this.cue_num.as_ref().is_some_and(|(w, _)| *w == which);
                        if !moved && !open {
                            this.begin_cue_num(which, cx);
                        }
                    })),
            }
        };
        let row = |label: &'static str, control: AnyElement| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(px(crate::layout::redesign::CONTROL_M))
                .px(px(6.))
                .child(div().flex_none().w(px(LABEL_W)).child(dark_row_label(label, false)))
                .child(control)
        };
        let length = format!("{} s", cue_secs(end - start));
        let header = div()
            .id("dock-caption-header")
            .flex_none()
            .h(px(44.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(Icon::Captions.el(IconSize::Inline, t.ink2))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child({
                        let first: Vec<&str> = words.split_whitespace().take(HEAD_WORDS).collect();
                        let title = match (picture, first.is_empty()) {
                            (true, _) => "Picture caption".to_string(),
                            (_, true) => "No words yet".to_string(),
                            _ => first.join(" "),
                        };
                        div()
                            .truncate()
                            .type_style(crate::ui::type_scale::emph())
                            .text_color(rgb(t.ink1))
                            .child(title)
                    })
                    .child(
                        div()
                            .truncate()
                            .type_style(crate::ui::type_scale::caption())
                            .text_color(rgb(t.ink3))
                            .child(format!("Caption {} of {} \u{b7} {}", cue + 1, count, length)),
                    ),
            );
        // The caption's own actions reuse the caption menu's, so a reason shows the same way.
        let m = CueMenu { lane, track, cue, at: gpui::point(px(0.), px(0.)) };
        let blocked = |why: Option<&'static str>| match locked {
            true => Some("This lane is locked"),
            false => why,
        };
        let split_why = blocked(self.cue_split_why(m));
        let merge_why = blocked(self.cue_merge_why(m));
        let delete_why = blocked(None);
        let actions = div()
            .id("caption-actions")
            .flex()
            .flex_nowrap()
            .gap(px(6.))
            .px(px(6.))
            .pt(px(6.))
            .child(caption_action("caption-split", "Split", "Split at playhead", Icon::Scissors, split_why, m, Player::split_cue_at_playhead, cx))
            .child(caption_action("caption-merge", "Merge", "Merge with next", Icon::Link, merge_why, m, Player::merge_cue_with_next, cx))
            .child(caption_action("caption-delete", "Delete", "Delete", Icon::Trash2, delete_why, m, Player::delete_cue_at, cx));
        // The length is the difference of the two numbers, so it reads only.
        let length_row = div()
            .id("caption-length")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(crate::layout::redesign::CONTROL_M))
            .px(px(6.))
            .child(div().flex_none().w(px(LABEL_W)).child(dark_row_label("Length", false)))
            .child(div().type_style(crate::ui::type_scale::body()).text_color(rgb(t.ink2)).child(length));
        Some(
            div()
                .id("dock-caption")
                .flex_none()
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .px(px(12.))
                        .pb(px(6.))
                        .child(field)
                        .child(row("Start", num("caption-start", "Start", CueNum::Start, start, cx).into_any_element()))
                        .child(row("End", num("caption-end", "End", CueNum::End, end, cx).into_any_element()))
                        .child(length_row)
                        .child(actions),
                ),
        )
    }
}
