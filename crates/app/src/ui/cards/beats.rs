//! The mark-the-beats section: the Clip tab's card for a sound clip. Its head is
//! the section verb in the dock; this is the body that folds under it.

use super::common::*;
use crate::ui::kit::{self, Segment};
use crate::*;

impl Player {
    /// The section body while it is open: the three choices of beat, one line of
    /// explanation, the run's progress while it listens, the last run's result,
    /// and the Add button.
    pub(crate) fn beats_card(
        &self,
        room: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        self.beats_open?;
        let busy = self.beats_scan.is_some();
        let me = cx.entity();
        let chips = kit::segmented_equal(
            "beats-every",
            &[
                Segment::text("Every beat"),
                Segment::text("Every 2nd"),
                Segment::text("Every 4th"),
            ],
            self.beats_every.min(2),
            !busy,
            None,
            move |n, _, app| {
                me.update(app, |this, cx| {
                    this.beats_every = n;
                    cx.notify();
                })
            },
        )
        .into_any_element();
        let status: SharedString = match (self.beats_scan, &self.beats_last) {
            (Some(since), _) => format!(
                "Listening to the sound… {:.0} s",
                since.elapsed().as_secs_f32()
            )
            .into(),
            (None, Some(last)) => last.clone().into(),
            (None, None) => "Marks each beat; a rerun replaces them.".into(),
        };
        let add = kit::Button::new("beats-add")
            .label("Mark beats")
            .style(kit::ButtonStyle::Primary)
            .enabled(!busy)
            .build()
            .flex_1();
        let add = match busy {
            false => add
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_beats(cx)))
                .into_any_element(),
            true => add.into_any_element(),
        };
        Some(
            self.card_outer(room, cx).child(
                div()
                    .w_full()
                    .max_w(px(card_max_w(
                        COLOR_W,
                        self.card_maximized,
                        f32::from(room.width),
                    )))
                    .on_mouse_down(MouseButton::Left, swallow)
                    .flex()
                    .flex_col()
                    .gap(px(CARD_GAP))
                    .map(|d| self.card_surface(d))
                    .child(chips)
                    .child(div().w_full().child(dark_help(status)))
                    .child(div().flex().child(add)),
            ),
        )
    }
}
