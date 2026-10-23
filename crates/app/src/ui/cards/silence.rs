//! The silence card: the bed stays readable under it.

use super::common::*;
use crate::ui::kit::{self, Segment};
use crate::ui::widgets::SliderTyped;
use crate::*;

impl Player {
    /// The silence card: what the scan is looking for, what it found, and the
    /// two things that can be done about it.
    ///
    /// Its scrim is the lightest of the cards' on purpose. Every other card is
    /// about the clip it names and can black the timeline out; this one is
    /// *about* the timeline -- the marks under it are the whole preview -- so
    /// the bed stays readable and the card sits up in the picture area rather
    /// than over the lanes.
    ///
    /// Four rows show at once and the rest sit behind `More …`: the numbers a
    /// first run turns are the threshold, the forgiveness and the rate.
    pub(crate) fn silence_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.silence_open?;
        let head_meta: SharedString = format!("{} clip {}", lane.label(), idx + 1).into();
        let help_text = "↑↓ picks a setting and ←→ moves it (hold to run it), or click a bar — the marks on the lane are what would go";
        let head = dark_card_head_reset(
            "Silences",
            Some(head_meta.clone()),
            Some(help_text.into()),
            Some(self.card_maximized),
            self.silence_any_changed()
                .then_some(Self::reset_silence as fn(&mut Self, &mut Context<Self>)),
            cx,
        );
        // The unit is a label, never a conversion: the threshold is a level
        // below full scale whichever of the two the row says (`silence_dbfs`).
        let unit = match self.silence_dbfs {
            true => "dBFS",
            false => "dB",
        };
        let lanes = self.silence_lanes();
        let me = cx.entity();
        let scope_chips = kit::segmented_equal(
            "silence-scope",
            &[Segment::text("This clip"), Segment::text("This track"), Segment::text("Whole timeline")],
            SCOPES.iter().position(|s| *s == self.silence_scope).unwrap_or(0),
            true,
            None,
            {
                let me = me.clone();
                move |n, _, app| {
                    me.update(app, |this, cx| {
                        this.silence_field = 0;
                        this.silence_scope = SCOPES[n];
                        cx.notify();
                    })
                }
            },
        )
        .into_any_element();
        let unit_chips = kit::segmented_equal(
            "silence-unit",
            &[Segment::text("dBFS"), Segment::text("dB")],
            usize::from(!self.silence_dbfs),
            true,
            None,
            {
                let me = me.clone();
                move |n, _, app| {
                    me.update(app, |this, cx| {
                        this.silence_field = 2;
                        this.silence_dbfs = n == 0;
                        cx.notify();
                    })
                }
            },
        )
        .into_any_element();
        let found = self.silence_marks.len();
        let secs =
            f64::from(self.silence_marks.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        // The line under the rows: where a scan still running has got to -- a
        // card is up from the frame it is asked for, numbers or no numbers --
        // or what the settings found in levels already read, and the lanes
        // the apply will reach.
        let status = match &self.silence_scan {
            Some(scan) => silence_line(
                scan.seen as f32 / 10.,
                scan.progress
                    .total
                    .load(std::sync::atomic::Ordering::Relaxed) as f32
                    / 10.,
                scan.started.elapsed().as_secs_f32(),
                scan.since.elapsed().as_secs_f32(),
            ),
            None => {
                let place = self.silence_place(&lanes);
                match (self.silence_keep_only, found) {
                    (true, 0) => format!("Nothing to cut: {place} needs both silence and speech."),
                    (true, 1) => format!("1 speech stretch, {}, in {place}. Silences are kept.", secs_label(secs)),
                    (true, n) => format!("{n} speech stretches, {}, in {place}. Silences are kept.", secs_label(secs)),
                    (false, 0) => format!("No silence found that's quiet and long enough in {place}."),
                    (false, 1) => format!("1 silence, {}, in {place}.", secs_label(secs)),
                    (false, n) => format!("{n} silences, {}, in {place}.", secs_label(secs)),
                }
            }
        };
        // Each row is one stop of the arrows: the picked one shows its rule,
        // and a press anywhere on it picks it.
        let mut rows: Vec<AnyElement> = vec![
            self.silence_chip_row(
                0,
                "Apply to",
                scope_chips,
                "Which tracks the cut or speed-up reaches",
            ),
            self.silence_slider(
                1,
                "silence-threshold",
                "Quiet below",
                crate::creator::minus(format!("{:.0} {unit}", self.silence.threshold_db)),
                "Sound quieter than this counts as silence",
                cx,
            ),
            self.silence_slider(
                3,
                "silence-forgive",
                "Ignore pauses shorter than",
                format!("{:.2} s", self.silence.min_silence),
                "A quiet stretch shorter than this is left alone",
                cx,
            ),
            self.silence_slider(
                6,
                "silence-speed",
                "Speed-up plays at",
                super::speed::speed_text(self.silence_factor),
                "How fast the quiet parts play in a speed-up",
                cx,
            ),
            disclosure_row(
                Self::SILENCE_MORE_FOLD,
                "More options",
                self.silence_more_open(),
                Self::toggle_silence_more,
                cx,
            )
            .into_any_element(),
        ];
        if self.silence_more_open() {
            rows.push(self.silence_chip_row(
                2,
                "Show levels as",
                unit_chips,
                "dBFS counts down from the loudest sound a file can hold; dB is the same level in plain decibels",
            ));
            rows.push(self.silence_slider(
                4,
                "silence-padding",
                "Keep either side of speech",
                format!("{:.2} s", self.silence.padding),
                "Room kept before and after each stretch of speech",
                cx,
            ));
            rows.push(self.silence_slider(
                5,
                "silence-sliver",
                "Drop leftover bits shorter than",
                format!("{:.2} s", self.silence.min_keep),
                "Pieces of sound shorter than this are cut as well",
                cx,
            ));
        }
        // The two applies, and the status line with the keep-only switch beside
        // it: a switch, not a third apply, so it is always pressable -- even
        // with nothing found, which is when the applies dim.
        let (cut_label, speed_label) = if self.silence_keep_only {
            ("Remove speech", "Speed up speech")
        } else {
            ("Remove silences", "Speed up silences")
        };
        // The two buttons the ask names, side by side: a mode toggle would hide
        // one of them behind the other, and there are only two.
        let button = |id: &'static str,
                      text: &'static str,
                      chord: &'static str,
                      act: fn(&mut Self, &mut Context<Self>)| {
            let enabled = found != 0;
            let b = kit::Button::new(id).label(text).enabled(enabled).build().flex_1();
            match enabled {
                true => b
                    .tooltip(crate::ui::widgets::overlay_tip_hover(text, chord, None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| act(this, cx)))
                    .into_any_element(),
                false => b
                    .tooltip(crate::ui::widgets::overlay_tip_hover(text, "", Some("Nothing found to work on yet")))
                    .into_any_element(),
            }
        };
        let keep_only = kit::switch_row(
            "silence-keep-only",
            "Keep only silence",
            Some("Work on the speech instead of the silences".into()),
            self.silence_keep_only,
            None,
            {
                let me = me.clone();
                move |_, app| {
                    me.update(app, |this, cx| {
                        this.toggle_silence_keep_only();
                        cx.notify();
                    })
                }
            },
        );
        Some(
            self.card_outer(viewport, cx)
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(
                            COLOR_W,
                            self.card_maximized,
                            f32::from(viewport.width),
                        )))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .map(|d| self.card_surface(d))
                        .child(head)
                        .children(rows)
                        .child(keep_only)
                        // Two lines (20 px a line) reserved, so a longer or shorter status never moves the buttons.
                        .child(div().w_full().flex_none().min_h(px(40.)).child(dark_help(status)))
                        .child(div().flex().gap(px(4.)).children([
                            button("silence-apply", cut_label, "\u{21b5}", Self::cut_silences),
                            button("silence-apply-speed", speed_label, "f", Self::speed_silences),
                        ])),
                ),
        )
    }

    /// Where the scope reaches, in the card's words. A clip is named by its file, the way the
    /// clip reads on the timeline's sources, never by the tracks under it.
    fn silence_place(&self, lanes: &[Lane]) -> String {
        let clip = self.silence_open.and_then(|(lane, idx)| {
            let session = self.session.as_ref()?;
            let clip = session.lane_clips(lane).get(idx)?;
            let file = session.sources().get(clip.source)?.path.file_name()?;
            Some(file.to_string_lossy().into_owned())
        });
        match (self.silence_scope, clip) {
            (Scope::Take, Some(name)) => format!("this clip ({name})"),
            (scope, _) => scope.label(lanes),
        }
    }

    /// A row's frame: the picked rule on its left edge and a press anywhere on
    /// it picks it. The explanation rides the label alone (see
    /// [`Self::silence_label`]), so working the control never raises a plate over it.
    fn silence_row(&self, n: usize, child: AnyElement) -> AnyElement {
        div()
            .id(("silence-field", n))
            .w_full()
            .flex()
            .flex_col()
            .px(px(6.))
            .when(self.silence_field == n, |d| d.bg(rgb(DARK_RAISED())))
            .child(child)
            .into_any_element()
    }

    /// A row's label; hovering it says what the setting is for.
    fn silence_label(&self, n: usize, label: &'static str, why: &'static str) -> Stateful<Div> {
        div()
            .id(("silence-label", n))
            .flex_none()
            .tooltip(crate::ui::widgets::overlay_tip_hover(why, "", None))
            .child(dark_row_label(label, self.silence_field == n))
    }

    /// A choice row: the label, then the segmented control across the width.
    fn silence_chip_row(&self, n: usize, label: &'static str, chips: AnyElement, why: &'static str) -> AnyElement {
        let row = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .py(px(2.))
            .child(div().flex().child(self.silence_label(n, label, why)))
            .child(chips);
        self.silence_row(n, row.into_any_element())
    }

    /// One slider row: [`slider_row`] with the card's own value, press and
    /// reset, framed as a row of the card.
    fn silence_slider(
        &self,
        n: usize,
        id: &'static str,
        label: &'static str,
        value: String,
        why: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bar = Rc::new(Cell::new(Bounds::default()));
        let probe = Rc::clone(&bar);
        let changed = self.silence_changed(n);
        let on_reset: Option<SliderReset<Self>> = Some(Rc::new(
            move |this: &mut Self, cx: &mut Context<Self>| {
                this.reset_silence_field(n, cx);
            },
        ));
        // Typed in the unit the value reads: a rate in "x" (1.5 for 1.5x), the rest as they show.
        let scale = if n == 6 { 1000. } else { 1. };
        let typed = Self::silence_range(n).map(|(lo, hi, _)| SliderTyped {
            lo: lo / scale,
            hi: hi / scale,
            set: Rc::new(move |this: &mut Self, v: f64, cx: &mut Context<Self>| {
                this.press_silence(n, ((v * scale - lo) / (hi - lo)) as f32);
                cx.notify();
            }),
        });
        // The row keeps its own bar, so a held press drags across the whole row.
        let held = Rc::clone(&bar);
        let slider = slider_row_tip(
            SliderRow {
                id,
                label: label.into(),
                value: value.into(),
                fraction: self.silence_fraction(n),
                changed,
                bar,
                on_press: Box::new(move |this: &mut Self, x: Pixels, cx: &mut Context<Self>| {
                    this.press_silence(n, frac_along(x, probe.get()));
                    cx.notify();
                }),
                on_reset,
                typed,
            },
            why,
            cx,
        )
        .into_any_element();
        // A press sets the value and a held button drags it, as the card's other sliders do.
        div()
            .w_full()
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let bar = held.get();
                if event.pressed_button == Some(MouseButton::Left) && bar.size.width > px(0.) {
                    this.press_silence(n, frac_along(event.position.x, bar));
                    cx.notify();
                }
            }))
            .child(self.silence_row(n, slider))
            .into_any_element()
    }
}
