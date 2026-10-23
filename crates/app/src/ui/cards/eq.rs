//! The EQ card and its band numbers.

use super::common::*;
use super::numedit;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit;
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::overlay_tip_hover;
use crate::*;

/// The left gutter the dB labels sit in, outside the plot.
const EQ_AXIS_L: f32 = 52.;
/// The strip over the plot the "Play to see sound" note sits in.
const EQ_NOTE_H: f32 = 18.;
/// The row under the plot the frequency labels sit in.
const EQ_LABEL_H: f32 = 14.;
/// The frequencies the axis names, the ends left to the box's own edges.
const EQ_NAMED_HZ: [f32; 3] = [100., 1000., 10000.];
/// A knob's radius. The plot is inset by it on the top, right and bottom, so a
/// band at +12 or -12 dB is drawn wholly inside the box and never on a label.
const EQ_KNOB_R: f32 = EQ_HANDLE / 2.;
/// The gains the plot rules a line across: the two limits, the two halves and 0 dB.
const EQ_DB_LINES: [f32; 5] = [EQ_GAIN_LIMIT, EQ_DB_GRID[0], 0., EQ_DB_GRID[1], -EQ_GAIN_LIMIT];

/// A frequency as its axis label: "20 Hz", "100 Hz", "1 kHz", "10 kHz", "20 kHz".
fn eq_hz_label(freq: f32) -> String {
    if freq >= 1000. {
        format!("{:.0} kHz", freq / 1000.)
    } else {
        format!("{freq:.0} Hz")
    }
}

/// A gain as its axis label, with a real minus sign. Only the top label carries the unit.
fn eq_db_label(db: f32, top: bool) -> String {
    let number = match db {
        d if d > 0. => format!("+{d:.0}"),
        d if d < 0. => format!("\u{2212}{:.0}", -d),
        _ => "0".to_string(),
    };
    if top { format!("{number} dB") } else { number }
}

/// What a band's shape does, in plain words: the one line under its name.
fn eq_shape_why(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowCut => "Removes rumble below the frequency",
        BandKind::LowShelf => "Raises or lowers everything below",
        BandKind::Peak => "Raises or lowers around the frequency",
        BandKind::HighShelf => "Raises or lowers everything above",
        BandKind::HighCut => "Removes hiss above the frequency",
    }
}

impl Player {
    /// The equalizer of one audio clip: its frequency response drawn as a
    /// curve, a handle per band sitting on it, each band's own bell under the
    /// sum, and a row that reads and moves the picked band's three numbers. The
    /// curve is the clip's actual filter (`EqParams::response_db` reads the
    /// coefficients the samples go through), and it is redrawn on every pointer
    /// sample of a drag, so the shape bends under the hand.
    ///
    /// Wider than the other cards and wider still on a bigger window
    /// ([`eq_card_w`]): every pixel across is frequency resolution, which none
    /// of the row-shaped cards have any use for. The same scrim and the same
    /// plain divs -- nothing here takes focus, so the root keeps the keyboard
    /// and the card's own strokes (a digit, the arrows, `a`, `x`, `f`, `r`,
    /// `s`) reach it.
    ///
    /// Every change is written at the clip as it is made
    /// ([`Player::commit_eq`]), so what the card shows is always what is
    /// playing: there is no OK button to forget, and closing it changes
    /// nothing. What takes a curve back off is undo, like every other edit.
    pub(crate) fn eq_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.eq_open?;
        // The rate the engine filters this timeline at, so the drawn curve is
        // the one those coefficients make -- near the top of the axis a 44.1 kHz
        // clip and a 48 kHz one are not the same shape.
        let sample_rate = self.timeline_audio().map_or(48_000, |(rate, _)| rate);
        let picked = self.eq_params.bands.get(self.eq_band);
        // What is playing, drawn behind the curve -- and only while something
        // *is* playing: the tap freezes with the device, and a still spectrum
        // under a paused timeline would look like sound that is not there.
        let spectrum = self
            .eq_spectrum
            .then_some(self.session.as_ref())
            .flatten()
            .filter(|session| session.is_playing())
            .and_then(PlaybackSession::audio_tap)
            .map(|(samples, rate)| eq_spectrum(&samples, rate))
            .filter(|levels| !levels.is_empty())
            .map(eq_spectrum_curve);
        let playing = self.session.as_ref().is_some_and(PlaybackSession::is_playing);
        let handles: Vec<_> = self
            .eq_params
            .bands
            .iter()
            .enumerate()
            .map(|(i, band)| {
                // The hit area, centred on the point: it hangs off the graph's
                // corner, so it is pulled back by half of itself both ways.
                // The dot is drawn in the middle of it.
                div()
                    .absolute()
                    .left(relative(eq_x(band.freq_hz)))
                    .top(relative(eq_y(if eq_takes_gain(band.kind) { band.gain_db } else { 0. })))
                    .ml(px(-EQ_HIT / 2.))
                    .mt(px(-EQ_HIT / 2.))
                    .w(px(EQ_HIT))
                    .h(px(EQ_HIT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(EQ_HANDLE))
                            .h(px(EQ_HANDLE))
                            .rounded(px(EQ_HANDLE / 2.))
                            .bg(rgb(if i == self.eq_band { INK1() } else { INK3() })),
                    )
            })
            .collect();
        // Maximized used to claim a fixed 320 px slice for the graph
        // regardless of how much room a small bench actually left the card,
        // so the numbers/buttons rows below it were the ones squeezed into a
        // scroll -- docked, at the same bench, showed all of them with no
        // scroll at all. `flex_1` between the docked floor and the maximized
        // ceiling lets the graph take whatever is *actually* left after the
        // rows below it claim their own natural height first, the same
        // leftover-space idiom `dock_stance::dock_sources` already uses for
        // its own scroll region.
        let graph_maximized = self.card_maximized;
        // The plot is inset from the box: its edges are where ±12 dB and the
        // ends of the frequency axis fall, and the knobs there are drawn half
        // outside them. The labels live in the gutters the inset leaves.
        let plot = div()
            .absolute()
            .left(px(EQ_AXIS_L))
            .right(px(EQ_KNOB_R))
            .top(px(EQ_NOTE_H + EQ_KNOB_R))
            .bottom(px(EQ_KNOB_R + EQ_LABEL_H))
            // The probe measures the plot, so a pointer's x and y map onto the
            // same inset axis the curve is drawn on.
            .child(bounds_probe(self.eq_graph.clone()))
            // The analyser first, so everything else is drawn on top of it.
            .children(spectrum)
            // The decades, so a hump can be read as "around 200 Hz" without
            // dropping the eye to the labels along the bottom. The two ends
            // are the plot's own edges and rule nothing.
            .children(
                EQ_TICKS
                    .iter()
                    .filter(|(freq, _)| *freq > EQ_FREQ_LOW && *freq < EQ_FREQ_HIGH)
                    .map(|(freq, _)| {
                        div()
                            .absolute()
                            .left(relative(eq_x(*freq)))
                            .w(px(1.))
                            .h_full()
                            .bg(rgb(DARK_HAIRLINE()))
                    })
                    .collect::<Vec<_>>(),
            )
            // The gain rules, the limits and 0 dB included: each one a line the
            // curve can be read against, with its number beside it in the gutter.
            .children(EQ_DB_LINES.map(|db| {
                div()
                    .absolute()
                    .top(relative(eq_y(db)))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(DARK_HAIRLINE()))
            }))
            .children(EQ_DB_LINES.iter().enumerate().map(|(i, &db)| {
                div()
                    .absolute()
                    .left(px(-EQ_AXIS_L))
                    .w(px(EQ_AXIS_L - EQ_KNOB_R - 4.))
                    .top(relative(eq_y(db)))
                    .mt(px(-7.))
                    .whitespace_nowrap()
                    .text_align(TextAlign::Right)
                    .map(|d| d.type_style(type_scale::caption()).text_color(rgb(INK3())))
                    .child(eq_db_label(db, i == 0))
            }))
            .child(eq_curve(self.eq_params.clone(), sample_rate))
            .children(handles)
            // The frequency labels under the plot, one row, each centred under its own
            // grid line. The range is the box's own edges, so only the three names in
            // the middle are written.
            .children(EQ_TICKS.iter().filter(|(freq, _)| EQ_NAMED_HZ.contains(freq)).map(|(freq, _)| {
                let text = eq_hz_label(*freq);
                let w = eq_tick_end_w(&text, type_scale::CHORD_METADATA_MIN_PX) + 8.;
                div()
                    .absolute()
                    .top(relative(1.))
                    .mt(px(EQ_KNOB_R))
                    .h(px(EQ_LABEL_H))
                    .w(px(w))
                    .left(relative(eq_x(*freq)))
                    .ml(px(-w / 2.))
                    .whitespace_nowrap()
                    .text_align(TextAlign::Center)
                    .map(|d| d.type_style(type_scale::caption()).text_color(rgb(INK3())))
                    .child(text)
            }));
        let graph = div()
            // Ided like the band rows it replaces: what the pointer presses on
            // is one element with its own hitbox, which is what a drag is
            // tracked from.
            .id("eq-graph")
            .relative()
            .when(graph_maximized, |d| {
                d.flex_1().min_h(px(EQ_GRAPH_H)).max_h(px(EQ_GRAPH_MAX_H))
            })
            .when(!graph_maximized, |d| d.flex_none().h(px(eq_graph_h(false))))
            .rounded(px(3.))
            .bg(rgb(DARK_HAIRLINE()))
            .cursor_pointer()
            // The press picks the band under it *and* is already the first
            // sample of the drag, so a plain click sets a value. A second click
            // takes that band back to flat instead: the gesture that undoes one
            // handle, with no modifier to remember.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.eq_band = this.nearest_band(event.position.x);
                    if event.click_count >= 2 {
                        this.eq_dragging = false;
                        this.nudge_band(|b| b.gain_db = 0., cx);
                        return;
                    }
                    this.eq_dragging = true;
                    this.drag_band(event.position, cx);
                }),
            )
            // A switched-on analyser over a stopped timeline would draw
            // nothing, which reads as broken: say what it is waiting for, in the
            // strip over the plot where no knob or curve can reach it.
            .children((self.eq_spectrum && !playing).then(eq_listen_note))
            .child(plot);
        // The bottom of the axis is not named: -12 dB would land in the same
        // corner as the 20 Hz tick, and the two lines above it (+6 and -6)
        // already say what the box is worth per pixel.
        //
        // The same affordance the column itself carries, read back off the
        // card's own handle: this is the tallest card in the inspector -- a
        // 132 px graph with a row of numbers and a row of buttons under it --
        // and at the 360 px floor it is taller than the column it is docked in.
        let can_scroll = f32::from(self.eq_scroll.max_offset().height) > 1.;
        let below = px_below(
            f32::from(self.eq_scroll.max_offset().height),
            f32::from(self.eq_scroll.offset().y),
        );
        Some(
            self.card_outer(viewport, cx)
                .child(
                    div()
                        // A cap, not a width: the card is docked in the
                        // inspector now, and that column is narrower than the
                        // floor a graph wants -- asking for [`eq_card_w`]
                        // outright hung the card's right-hand third off the edge
                        // of the window. The two cards beside it are built this
                        // way for the same reason.
                        .id("eq-card")
                        .w_full()
                        .max_w(px(eq_card_w(f32::from(viewport.width), self.card_maximized)))
                        // And a cap the other way, for the same reason the width
                        // has one: the card is docked in a column now, and at the
                        // 360 px floor it is taller than that column -- its title
                        // ran off the top and its buttons off the bottom, with
                        // neither reachable. The card itself owns that scroll:
                        // every child, including the below-fold line, is in its
                        // wheel surface rather than a fixed sibling dead zone.
                        .max_h(relative(1.))
                        .overflow_y_scroll()
                        .track_scroll(&self.eq_scroll)
                        // Maximized also *claims* the room the cap above
                        // only allows: `flex_1()`/`max_h` on the graph below
                        // has nothing to grow into if the card itself still
                        // hugs its own content height.
                        .when(graph_maximized, |d| d.h(relative(1.)))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .map(|d| self.card_surface(d))
                        .child(
                            div()
                                .id("eq-card-rows")
                                // A non-maximized card exposes the body's full
                                // natural height to its parent scroll surface;
                                // maximized fills the claimed card room so the
                                // graph can consume only the leftover height.
                                .when(!graph_maximized, |d| d.flex_none())
                                .when(graph_maximized, |d| d.flex_1())
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                // Which clip, because the card is modal and the lane it
                                // was opened from is behind a scrim by the time it is up.
                                .map(|d| {
                                    d.child(dark_card_head_reset(
                                        "Equalizer",
                                        Some(format!("{} clip {}", lane.label(), idx + 1).into()),
                                        Some("drag a handle, or a digit picks a band — ←→ moves it, ↑↓ its gain, shift+←→ its width; a adds, x removes, f flattens it, r all, s spectrum, m fills the room".into()),
                                        Some(self.card_maximized),
                                        Some(reset_eq_layout),
                                        cx,
                                    ))
                                })


                                .child(graph)
                                // Which band the keyboard is holding and every number it
                                // is set to, each with the pair of buttons that moves it:
                                // the curve shows the sum, and a band pushed against one
                                // pulling the other way is not readable off it.
                                .child(self.eq_numbers(picked, cx))
                                .child(self.eq_shape_row(picked, cx))
                                .child(kit::switch_row(
                                    "eq-spectrum",
                                    "Show spectrum",
                                    Some("Draws the sound that is playing behind the curve".into()),
                                    self.eq_spectrum,
                                    None,
                                    {
                                        let me = cx.entity();
                                        move |_, app| {
                                            me.update(app, |this, cx| {
                                                this.eq_spectrum = !this.eq_spectrum;
                                                cx.notify();
                                            })
                                        }
                                    },
                                ))
                                .child(
                                    div()
                                        .mt(px(4.))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .child(kit::icon_ghost("eq-add", Icon::Plus, "Add band", None, {
                                            let me = cx.entity();
                                            move |_, app| me.update(app, |this, cx| this.add_band(cx))
                                        }))
                                        .child(kit::icon_ghost("eq-remove", Icon::Trash2, "Remove band", None, {
                                            let me = cx.entity();
                                            move |_, app| me.update(app, |this, cx| this.remove_band(cx))
                                        }))
                                        .child(div().flex_1())
                                        .child(
                                            kit::Button::new("eq-reset")
                                                .label("Flatten all")
                                                .build()
                                                .tooltip(crate::ui::widgets::overlay_tip_hover("Flatten all", "r", None))
                                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    for band in &mut this.eq_params.bands {
                                                        band.gain_db = 0.;
                                                    }
                                                    this.commit_eq(cx);
                                                })),
                                        ),
                                ),
                        )
                        // The column's own line, on the card that needs it for
                        // the column's reason: a row nobody knows is under the
                        // fold is a row that is not there.
                        .when(can_scroll, |d| {
                            d.child(dark_help(match below > 1. {
                                true => "more below — scroll the card",
                                false => "the end — scroll up for the rest",
                            }))
                        })
                        ,
                ),
        )
    }

    /// The picked band's three numbers -- where it sits, how far it pushes and
    /// how wide it is -- each beside the pair of buttons that moves it. The
    /// arrows do the same three things, but a value only a key can change is a
    /// value a hand on the pointer cannot reach at all, which is the same reason
    /// [`Player::mbps_steppers`] exists.
    pub(crate) fn eq_numbers(
        &self,
        picked: Option<&Band>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = div()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(6.));
        let Some(band) = picked.copied() else {
            return row.child(dark_help("no bands — a adds one"));
        };
        // Gain and width are number fields: drag to scrub, click to type. The
        // name carries its own tooltip, so the plain meaning is one hover away.
        let number = |name: &'static str,
                      id: &'static str,
                      why: &'static str,
                      target: numedit::NumTarget,
                      value: f32,
                      cx: &mut Context<Self>| {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id((id, 1_usize))
                        .flex_1()
                        .min_w(px(0.))
                        .child(dark_row_label(name, false))
                        .tooltip(overlay_tip_hover(name, "", Some(why))),
                )
                .child(numedit::field(id, name, target, f64::from(value), cx))
        };
        // The band is picked from a row of pills, one per band, numbered by position.
        let count = self.eq_params.bands.len();
        let pills: Vec<kit::Segment> = (1..=count).map(|i| kit::Segment::text(i.to_string())).collect();
        let picker = kit::segmented_equal("eq-band", &pills, self.eq_band, true, None, {
            let me = cx.entity();
            move |i, _, app| {
                me.update(app, |this, cx| {
                    this.eq_band = i;
                    cx.notify();
                })
            }
        });
        row.child(div().w_full().flex().flex_col().gap(px(4.)).child(dark_row_label("Band", false)).child(picker))
        .child(number("Frequency", "eq-freq-field", "Where the band sits on the scale", numedit::NumTarget::Freq, band.freq_hz, cx))
        // A cut has no level, so a gain row under one would be a number that
        // does nothing -- it is left out rather than shown and ignored.
        .children(eq_takes_gain(band.kind).then(|| {
            number("Gain", "eq-gain-field", "How far the band lifts or cuts", numedit::NumTarget::Gain, band.gain_db, cx)
        }))
        .child(number("Width", "eq-q-field", "How wide the change is. Lower is wider.", numedit::NumTarget::Width, band.q, cx))
        .child(numedit::catcher(cx))
        .child(dark_ghost_button(
            "eq-flat-band",
            "Flatten band",
            "f",
            false,
            cx.listener(|this, _: &ClickEvent, _, cx| this.nudge_band(|b| b.gain_db = 0., cx)),
        ))
    }

    /// The picked band's shape as a dropdown, the way the Effects card picks a
    /// blend mode: the list says what each shape does in one plain line, and the
    /// line under the field says it for the one picked. Changing it is one edit
    /// like any other, so it goes through the same nudge as the numbers.
    fn eq_shape_row(&self, picked: Option<&Band>, cx: &mut Context<Self>) -> Div {
        let row = div().px(px(6.)).w_full().flex().flex_col().gap(px(4.));
        let Some(band) = picked else {
            return row;
        };
        let t = crate::ui::theme::tokens();
        let open = self.eq_shape_open;
        let current = EQ_KINDS.iter().position(|(k, ..)| *k == band.kind).unwrap_or(0);
        let field = kit::dropdown_field("eq-kind", EQ_KINDS[current].1, true).on_click(cx.listener(
            |this, _: &ClickEvent, _, cx| {
                this.eq_shape_open = !this.eq_shape_open;
                cx.notify();
            },
        ));
        let list = open.then(|| {
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.eq_shape_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let mut plate = kit::plate(6.)
                .id("eq-kind-list")
                .w_full()
                .max_h(px(284.))
                .overflow_y_scroll()
                .p(px(crate::layout::MENU_PAD))
                .flex()
                .flex_col()
                .occlude();
            for (k, (kind, name, _)) in EQ_KINDS.iter().enumerate() {
                let kind = *kind;
                plate = plate.child(
                    div()
                        .id(("eq-kind-row", k))
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                        .child(div().flex_none().size(px(16.)).children((k == current).then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(div().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(*name))
                                .child(dark_help(eq_shape_why(kind))),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.eq_shape_open = false;
                            this.nudge_band(|b| b.kind = kind, cx);
                        })),
                );
            }
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(crate::layout::redesign::CONTROL_M + 2.))
                    .left(px(0.))
                    .w_full()
                    .child(backdrop)
                    .child(plate),
            )
        });
        row.child(dark_row_label("Shape", false))
            .child(div().relative().w_full().child(field).children(list))
            .child(dark_help(eq_shape_why(band.kind)))
    }
}

/// The card's `Reset all`: the five-band layout a clip opens on, flat. The
/// picked band is kept inside the shorter list it may land in.
fn reset_eq_layout(this: &mut Player, cx: &mut Context<Player>) {
    this.eq_params = EqParams::default_layout();
    this.eq_band = this.eq_band.min(this.eq_params.bands.len() - 1);
    this.commit_eq(cx);
}

/// The analyser is switched on but nothing is playing, so there is no sound to
/// draw. Said in the strip over the plot, right-aligned, where no curve reaches.
fn eq_listen_note() -> impl IntoElement {
    div()
        .absolute()
        .top(px(1.))
        .right(px(4.))
        .h(px(16.))
        .flex()
        .items_center()
        .type_style(type_scale::label_tabular(
            type_scale::CHORD_METADATA_MIN_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK3()))
        .child("Play to see sound")
}
