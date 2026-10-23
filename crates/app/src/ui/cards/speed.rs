//! The speed card: one continuous rate on one clip.

use super::common::*;
use crate::player::cards::speed_fraction;
use super::tools::slider_track;
use crate::ui::icons::Icon;
use crate::layout::redesign as r;
use crate::ui::kit::{self, Segment};
use crate::ui::widgets::{SliderTyped, typed_value_pill};
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// A rate as the card reads it: "1.5×", "0.25×", "1×" -- as many decimals as
/// the rate needs, so the readout is the number a person would have typed.
pub(crate) fn speed_text(speed: Speed) -> String {
    let s = format!("{:.2}", speed.as_f64());
    format!("{}×", s.trim_end_matches('0').trim_end_matches('.'))
}

/// A preset chip's face: the bare multiplier, the way the chips are named.
fn preset_text(permille: u16) -> String {
    format!("{}", f64::from(permille) / 1000.)
}

impl Player {
    /// The speed card: one log-scaled bar from a quarter speed to four times it
    /// (equal travel per doubling), the rates people name as buttons under it,
    /// and the clip's new length in frames -- which is the number a person is
    /// actually choosing. Built like the colour card down to the scrim, because
    /// it is the same kind of card: one continuous value on one clip, live at
    /// the clip as it moves.
    ///
    /// The engine time-stretches a sped-up or slowed clip, so the pitch stays
    /// where it was recorded; the card says so under the bar.
    pub(crate) fn speed_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.speed_open?;
        let speed = self.card_speed();
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx).cloned()?;
        let ramp_mode = self.speed_ramp_mode;
        let presets = kit::segmented_equal(
            "speed-preset",
            &SPEED_PRESETS.map(|permille| Segment::text(format!("{}\u{d7}", preset_text(permille)))),
            SPEED_PRESETS.iter().position(|p| Speed::from_permille(*p) == speed).unwrap_or(usize::MAX),
            true,
            None,
            {
                let me = cx.entity();
                move |i, _, app| me.update(app, |this, cx| this.set_speed(Speed::from_permille(SPEED_PRESETS[i]), cx))
            },
        );
        // The one value on the card: ink1 once it is off real time, and the
        // row's own Reset puts it back. In the ramp mode it is the speed of the
        // next key instead.
        let slider = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(CONTROL_H))
                    .child(dark_row_label(if ramp_mode { "Key speed" } else { "Rate" }, false))
                    .child(typed_value_pill(
                        "speed-value",
                        &speed_text(speed),
                        None,
                        64.,
                        speed != Speed::NORMAL,
                        // Typed in times, "1.5" for 1.5x, on the bar's own grid.
                        Some(SliderTyped {
                            lo: f64::from(Speed::MIN.permille()) / 1000.,
                            hi: f64::from(Speed::MAX.permille()) / 1000.,
                            set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                                let step = f64::from(crate::audio_cards::SPEED_STEP);
                                let permille = ((v * 1000. / step).round() * step) as u16;
                                this.set_speed(Speed::from_permille(permille), cx);
                            }),
                        }),
                        {
                            let me = cx.entity();
                            move |_, app| me.update(app, |this, cx| this.set_speed(Speed::NORMAL, cx))
                        },
                        cx,
                    )),
            )
            .child(slider_track(
                "speed-bar",
                speed_fraction(f32::from(speed.permille())),
                self.speed_bar.clone(),
                |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                    this.speed_dragging = true;
                    this.drag_speed(x, true, cx);
                },
                cx,
            ))
            .into_any_element();
        // The direction, beside the rate: a toggle that plays the clip and its
        // group last frame first, composing with whatever the bar says.
        let reversed = session.reversed_of(lane, idx);
        let reverse_row = kit::switch_row("speed-reverse", "Reverse", Some("Play the clip backwards".into()), reversed, None, {
            let me = cx.entity();
            move |_, app| me.update(app, |this, cx| this.write_reverse(lane, idx, !reversed, cx))
        });
        let help_text = match ramp_mode {
            true => "Set the bar, then Add key puts that speed at the playhead. Plus and minus step a key, the cross removes it.",
            false => "Drag the bar, or use the left and right arrows for 0.05× a step. R puts it back to 1×.",
        };
        let tail_text: SharedString = format!(
            "Pitch kept · {:.1} s of footage in {:.1} s",
            f64::from(clip.len()) / self.fps.max(1.),
            f64::from(clip.frames()) / self.fps.max(1.)
        )
        .into();
        // The two modes of the card: one rate for the whole clip, or a ramp of keys.
        // Constant takes a ramp off, which is what pressing it means.
        let tabs = kit::segmented_equal(
            "speed-mode",
            &[Segment::text("Constant"), Segment::text("Ramp")],
            usize::from(ramp_mode),
            true,
            None,
            {
                let me = cx.entity();
                move |i, _, app| {
                    me.update(app, |this, cx| {
                        if i == 0 {
                            this.clear_ramp(cx);
                            this.speed_ramp_mode = false;
                        } else {
                            this.speed_ramp_mode = true;
                            this.ramp_key_speed = None;
                        }
                        cx.notify();
                    })
                }
            },
        );
        let ramp = ramp_mode.then(|| self.ramp_editor(&clip, cx));
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
                        .map(|d| {
                            d.child(div().w_full().flex().justify_end().child(dark_card_head(
                                "Speed",
                                None,
                                Some(help_text.into()),
                                Some(self.card_maximized),
                                cx,
                            )))
                        })
                        .child(tabs)
                        .children(ramp)
                        .child(slider)
                        .when(!ramp_mode, |d| {
                            d.child(presets)
                        })
                        .child(reverse_row)
                        // One line, never wrapped: this caption wrapped to two lines here, and the
                        // dock measured it far too tall, leaving a screenful of gap under the card.
                        .map(|d| d.child(div().flex_none().type_style(type_scale::caption()).text_color(rgb(tokens().ink3)).child(tail_text.clone()))),
                ),
        )
    }

    /// The ramp mode's body: the speed curve over the clip, the key list under it,
    /// the shapes people name and the two verbs on the keys. Every control is a
    /// button, so a ramp can be built without a keyboard.
    fn ramp_editor(&self, clip: &Clip, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let keys = clip.speed_keys.clone();
        let frames = clip.frames();
        let curve = ramp_curve(&keys, clip.speed, frames, 64);
        let key_xs: Vec<(f32, f32)> = keys
            .iter()
            .map(|k| {
                (
                    (k.at as f32 / frames.max(1) as f32).clamp(0., 1.),
                    speed_fraction(f32::from(k.speed.permille())),
                )
            })
            .collect();
        // Where the playhead is on the clip, as a fraction of its length.
        let playhead = self.session.as_ref().and_then(|s| {
            let at = crate::subs::frame_at(s.now(), self.fps);
            (at >= clip.start && at < clip.end())
                .then(|| (at - clip.start) as f32 / frames.max(1) as f32)
        });
        let plot = div()
            .id("speed-ramp-graph")
            .relative()
            .flex_1()
            .h(px(RAMP_GRAPH_H))
            .rounded(px(3.))
            .bg(rgb(DARK_RAISED()))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let (o, s) = (bounds.origin, bounds.size);
                        let (w, h) = (f32::from(s.width), f32::from(s.height));
                        if w <= 0. || h <= 0. {
                            return;
                        }
                        // Inset by the key diamond's radius, so an end key sits whole on the plot.
                        let at = |x: f32, y: f32| point(o.x + px(RAMP_KEY_R + x * (w - 2. * RAMP_KEY_R)), o.y + px((1. - y) * h));
                        // Real time, as a quiet rule across the middle.
                        let mid = speed_fraction(1000.);
                        let mut rule = PathBuilder::stroke(px(1.));
                        rule.move_to(at(0., mid));
                        rule.line_to(at(1., mid));
                        if let Ok(rule) = rule.build() {
                            window.paint_path(rule, rgba(DARK_SEAM()));
                        }
                        if let Some(x) = playhead {
                            let mut line = PathBuilder::stroke(px(1.));
                            line.move_to(at(x, 0.));
                            line.line_to(at(x, 1.));
                            if let Ok(line) = line.build() {
                                window.paint_path(line, rgb(INK3()));
                            }
                        }
                        let mut path = PathBuilder::stroke(px(1.5));
                        for (i, &(x, y)) in curve.iter().enumerate() {
                            match i {
                                0 => path.move_to(at(x, y)),
                                _ => path.line_to(at(x, y)),
                            }
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(path, rgb(INK1()));
                        }
                        // A key is a small diamond on the curve.
                        for &(x, y) in &key_xs {
                            let c = at(x, y);
                            let r = px(RAMP_KEY_R);
                            let mut dot = PathBuilder::fill();
                            dot.add_polygon(
                                &[
                                    point(c.x, c.y - r),
                                    point(c.x + r, c.y),
                                    point(c.x, c.y + r),
                                    point(c.x - r, c.y),
                                ],
                                true,
                            );
                            if let Ok(dot) = dot.build() {
                                window.paint_path(dot, rgb(INK1()));
                            }
                        }
                    },
                )
                .size_full(),
            );
        // The speed labels live in a gutter left of the plot, so no label sits on the curve.
        let gutter = div()
            .relative()
            .flex_none()
            .w(px(RAMP_GUTTER_W))
            .h(px(RAMP_GRAPH_H))
            .children([("4\u{d7}", 0.), ("1\u{d7}", 1. - speed_fraction(1000.)), ("0.25\u{d7}", 1.)].map(|(label, at)| {
                div()
                    .absolute()
                    .right(px(4.))
                    .top(relative(at))
                    .mt(px(if at >= 1. { -16. } else if at <= 0. { 0. } else { -8. }))
                    .type_style(type_scale::caption())
                    .text_color(rgb(INK3()))
                    .child(label)
            }));
        let graph = div()
            .flex()
            .w_full()
            .gap(px(6.))
            .child(gutter)
            .child(plot);
        // The caption sits under the plot in a reserved row, so the hint is never crossed
        // by the 1x rule and adding the first key does not move the list below.
        let caption = div()
            .flex_none()
            .h(px(RAMP_CAPTION_H))
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .type_style(type_scale::caption())
            .text_color(rgb(INK3()))
            .child(if keys.is_empty() { "Add keys to shape the speed over the clip" } else { "Speed over the clip, left to right" });
        let fps = self.fps;
        let rows: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                div()
                    .id(("speed-ramp-key", i))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(r::CONTROL_M))
                    .child(div().flex_1().child(dark_row_label(format!("{:.1} s", f64::from(k.at) / fps.max(1.)), false)))
                    .child(kit::icon_ghost("ramp-key-step", Icon::Minus, "Slower", None, {
                        let me = cx.entity();
                        move |_, app| me.update(app, |this, cx| this.step_ramp_key(i, -1, cx))
                    }))
                    .child(div().w(px(56.)).flex().justify_center().child(dark_row_value(speed_text(k.speed), true)))
                    .child(kit::icon_ghost("ramp-key-step-up", Icon::Plus, "Faster", None, {
                        let me = cx.entity();
                        move |_, app| me.update(app, |this, cx| this.step_ramp_key(i, 1, cx))
                    }))
                    .child(kit::icon_ghost("ramp-key-remove", Icon::X, "Remove key", None, {
                        let me = cx.entity();
                        move |_, app| me.update(app, |this, cx| this.remove_ramp_key(i, cx))
                    }))
            })
            .collect();
        let shape = |preset: RampPreset, cx: &mut Context<Self>| {
            let name = preset.label();
            dark_ghost_button(
                ("ramp-preset", preset as usize),
                name,
                "",
                false,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_ramp_preset(preset, cx);
                }),
            )
            .into_any_element()
        };
        let shapes = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().flex().gap(px(4.)).children(RampPreset::ALL[..2].iter().map(|p| shape(*p, cx))))
            .child(div().flex().gap(px(4.)).children(RampPreset::ALL[2..].iter().map(|p| shape(*p, cx))));
        let verbs = div()
            .flex()
            .gap(px(4.))
            .child(dark_ghost_button(
                "ramp-add-key",
                "Add key at playhead",
                "",
                false,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.add_ramp_key(cx);
                }),
            ))
            .child(dark_ghost_button(
                "ramp-clear",
                "Clear ramp",
                "",
                false,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.clear_ramp(cx);
                }),
            ));
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(graph)
            .child(caption)
            .children(rows)
            .child(shapes)
            .child(verbs)
    }
}

/// The curve's height on the card.
const RAMP_GRAPH_H: f32 = 120.;
/// A key's diamond radius: the plot is inset this far from each side, so an end key is drawn whole.
const RAMP_KEY_R: f32 = 4.;
/// The left gutter that holds the speed labels, outside the plot.
const RAMP_GUTTER_W: f32 = 40.;
/// The caption row under the plot, reserved so it never moves the list.
const RAMP_CAPTION_H: f32 = 18.;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readout_is_the_number_a_person_would_type() {
        assert_eq!(speed_text(Speed::NORMAL), "1×");
        assert_eq!(speed_text(Speed::from_permille(1500)), "1.5×");
        assert_eq!(speed_text(Speed::from_permille(250)), "0.25×");
        assert_eq!(speed_text(Speed::from_permille(4000)), "4×");
        assert_eq!(speed_text(Speed::from_permille(1050)), "1.05×");
    }

    #[test]
    fn the_chips_are_named_bare() {
        let names: Vec<String> = SPEED_PRESETS.into_iter().map(preset_text).collect();
        assert_eq!(names, ["0.25", "0.5", "1", "1.5", "2", "4"]);
    }
}
