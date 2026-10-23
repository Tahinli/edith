//! The colour panel: the grade's controls -- the four sliders, the wheels, the
//! curves and a LUT, one section at a time -- under the graph they tilt: the
//! luma histogram, or a waveform, vectorscope or RGB parade.

use crate::ui::kit::{self, Button, ButtonStyle, Segment};
use crate::ui::icons::Icon;
use crate::ui::widgets::{SliderTyped, typed_readout, typed_value_pill};
use crate::viewer_view::Compare;
use crate::ui::type_scale::Typeset;
use super::common::*;
use crate::*;
use engine::project::KeyKind;

/// Each slider's id, by band ([`COLOR_SLOTS`]: the four basic controls, a
/// wheel's four channels, the five curve points, the LUT's intensity).
const COLOR_IDS: [&str; COLOR_SLOTS] = [
    "color-brightness",
    "color-contrast",
    "color-saturation",
    "color-tint",
    "color-wheel-master",
    "color-wheel-red",
    "color-wheel-green",
    "color-wheel-blue",
    "color-curve-blacks",
    "color-curve-shadows",
    "color-curve-mids",
    "color-curve-highlights",
    "color-curve-whites",
    "color-lut-mix",
];
/// The Looks dropdown's list width.
const LOOK_LIST_W: f32 = 260.;
/// A colour wheel's pad edge.
const WHEEL_PAD: f32 = 88.;
/// The curve editor's square edge.
const CURVE_H: f32 = 150.;
/// The value pill's width on a slider row ("-100%" fits with room).
const PILL_W: f32 = 56.;
/// Each band's Reset button id, by band, like [`COLOR_IDS`].
const RESET_IDS: [&str; COLOR_SLOTS] = [
    "color-brightness-reset",
    "color-contrast-reset",
    "color-saturation-reset",
    "color-tint-reset",
    "color-wheel-master-reset",
    "color-wheel-red-reset",
    "color-wheel-green-reset",
    "color-wheel-blue-reset",
    "color-curve-blacks-reset",
    "color-curve-shadows-reset",
    "color-curve-mids-reset",
    "color-curve-highlights-reset",
    "color-curve-whites-reset",
    "color-lut-mix-reset",
];
/// Each wheel's Reset button id.
const WHEEL_RESET_IDS: [&str; 3] = ["color-lift-reset", "color-gamma-reset", "color-gain-reset"];
/// Each wheel's master readout id, which a click types the level in.
const WHEEL_VALUE_IDS: [&str; 3] = ["color-lift-value", "color-gamma-value", "color-gain-value"];
/// Each control's tooltip: what moving it does to the picture.
const COLOR_WHY: [&str; 4] = [
    "Lifts or sinks the whole picture; 0% is the clip as shot",
    "Spreads the tones apart, or pulls them toward grey; 100% is as shot",
    "Colour strength; 0% is greyscale",
    "Moves the picture toward cool (blue) or warm (red)",
];

impl Player {
    /// The colour card: the graded frame's luma histogram over a slider per
    /// control, each one a bar the pointer drags straight to a value -- no
    /// stepper buttons, because a slider is a thing to pull, and the arrow keys
    /// still move the same value for anyone not using a pointer. Same scrim,
    /// surface and row shape as the other two cards, and the same plain divs, so
    /// the root keeps the keyboard.
    ///
    /// The values are read from the project every render: what is drawn is what
    /// the decoder is grading with, never a copy that could drift from it. The
    /// graph above them is counted off the frame that came *back* through that
    /// grade ([`histogram`]), so pulling exposure tilts it while the hand moves.
    pub(crate) fn color_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        self.color_open?;
        let params = self.color_params();
        let mut rows: Vec<AnyElement> = vec![self.section_chips(cx)];
        match self.color_section {
            ColorSection::Basic => rows.extend(self.basic_rows(params, cx)),
            ColorSection::Wheels => rows.extend(self.wheel_rows(params, cx)),
            ColorSection::Curves => rows.extend(self.curve_rows(params, cx)),
            ColorSection::Lut => rows.extend(self.lut_rows(params, cx)),
        }
        let view = self.scope_view;
        let help_text =
            "Drag a bar, or press ↑↓ to pick one and ←→ to move it. R resets the picked bar; Esc closes";
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
                        .child(div().id("color-reset").w_full().flex().justify_end().child(dark_card_head_reset(
                            "",
                            None,
                            Some(help_text.into()),
                            None,
                            Some(|this, cx| this.set_color(ColorParams::default(), cx)),
                            cx,
                        )))
                        // The frame as it is being graded, over the controls
                        // grading it: the line is what the picture's brightness
                        // is spread as, and every sample of a drag reseeks, so it
                        // moves with the bar under the hand.
                        .child(self.scope_chips(cx))
                        .child(
                            div()
                                .flex_none()
                                .h(px(HIST_H))
                                .rounded(px(3.))
                                .bg(rgb(crate::ui::theme::tokens().matte))
                                .relative()
                                .when(view == ScopeView::Histogram, |d| d.child(hist_curves(self.histogram)))
                                .when(view != ScopeView::Histogram, |d| d.child(scope_canvas(view, self.scopes.clone()))),
                        )
                        .children(rows),
                ),
        )
    }

    /// The four basic sliders: the two a grade starts with, the More fold, the
    /// other two, the key row and the compare row.
    fn basic_rows(&self, params: ColorParams, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // Every band is a main row, so there is nothing a "More" fold could reveal. Auto sits
        // above the sliders it sets, Looks below them, and Compare closes the section.
        let mut rows: Vec<AnyElement> = vec![self.auto_row(cx)];
        rows.extend((0..COLOR_BANDS.len()).map(|band| self.color_row(band, params, cx)));
        rows.extend(self.looks_rows(cx));
        rows.push(self.key_row(KeyKind::Color, cx));
        rows.push(self.compare_row(cx));
        rows
    }

    /// Auto: one press reads a correction off the picture as shot and sets the balance and the
    /// levels, which then show on the sliders above and can be moved from there.
    fn auto_row(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("color-auto")
            .w_full()
            .min_h(px(HIT_MIN))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(dark_row_label("Auto", false))
            .child(
                Button::new("color-auto-button")
                    .label("Auto colour")
                    .style(ButtonStyle::Secondary)
                    .build()
                    .tooltip(crate::ui::widgets::overlay_tip_hover(
                        "Auto colour",
                        "",
                        Some("Reads the white balance and the levels off this frame as shot. Saturation, tint, curves and the LUT stay as they are"),
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.auto_colour(cx))),
            )
            .into_any_element()
    }

    /// Looks: a dropdown of the built-in grades and the user's saved ones, then "Save current as
    /// look…", which opens a name field under it.
    fn looks_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let names = self.look_names();
        let field = kit::dropdown_field("color-look", self.look_label(), true)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_look_list(cx)));
        let list = self.look_open.then(|| {
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.look_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let labels: Vec<SharedString> = names.iter().map(|n| n.clone().into()).collect();
            let picks = names.clone();
            let me = cx.entity();
            let pick = move |i: usize, _: &mut Window, app: &mut App| {
                if let Some(name) = picks.get(i).cloned() {
                    me.update(app, |this, cx| this.apply_look(&name, cx));
                }
            };
            let menu = kit::choice_list("color-look-list", LOOK_LIST_W, &labels, usize::MAX, pick);
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .child(backdrop)
                    // Opens upward, as the subtitle font list does: the card sits low in the dock.
                    .child(div().absolute().bottom(px(crate::layout::redesign::CONTROL_M + 2.)).right(px(0.)).w(px(LOOK_LIST_W)).child(menu)),
            )
        });
        let dropdown = div().id("color-look-row").relative().w_full().child(field).children(list).into_any_element();
        let save = div()
            .w_full()
            .flex()
            .justify_end()
            .child(
                Button::new("color-look-save")
                    .label("Save current as look…")
                    .style(ButtonStyle::Ghost)
                    .build()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.begin_look_save(cx))),
            )
            .into_any_element();
        let mut rows = vec![dark_row_label("Look", false).into_any_element(), dropdown, save];
        if let Some(field) = self.look_name.as_ref() {
            let shown = field.text.clone();
            let t = crate::ui::theme::tokens();
            rows.push(
                div()
                    .id("color-look-name")
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        kit::plate(6.)
                            .id("color-look-name-field")
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(crate::layout::redesign::CONTROL_M))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .truncate()
                            .type_style(crate::ui::type_scale::body())
                            .text_color(rgb(if shown.is_empty() { t.ink3 } else { t.ink1 }))
                            .child(if shown.is_empty() { "Name this look".to_string() } else { format!("{shown}\u{258f}") }),
                    )
                    .child(
                        Button::new("color-look-name-save")
                            .label("Save")
                            .style(ButtonStyle::Primary)
                            .build()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_look_save(cx))),
                    )
                    .child(
                        Button::new("color-look-name-cancel")
                            .label("Cancel")
                            .style(ButtonStyle::Ghost)
                            .build()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.look_name = None;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    /// Compare: Off, Split or Side by side, one segmented row. The picture as shot is set against
    /// the graded one in the viewer; it is a preview only, and an export never carries it.
    fn compare_row(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("color-compare-row")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(dark_row_label("Compare", false))
            .child(self.chip_row("color-compare", Compare::ALL.to_vec(), self.compare, Player::set_compare, cx))
            .into_any_element()
    }

    /// One row of equal-width choices (a kit segmented control): the picked
    /// one raised, every pill the same weight so a pick moves nothing.
    fn chip_row<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        items: Vec<(T, &'static str)>,
        picked: T,
        pick: fn(&mut Player, T, &mut Context<Player>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let segments: Vec<Segment> = items.iter().map(|&(_, label)| Segment::text(label)).collect();
        let at = items.iter().position(|&(value, _)| value == picked).unwrap_or(0);
        let me = cx.entity();
        let values: Vec<T> = items.into_iter().map(|(value, _)| value).collect();
        let on_select = move |i: usize, _: &mut Window, cx: &mut App| {
            if let Some(&value) = values.get(i) {
                me.update(cx, |this, cx| pick(this, value, cx));
            }
        };
        div().w_full().child(kit::segmented_equal(id, &segments, at, true, None, on_select)).into_any_element()
    }

    /// The section picker: Basic, Wheels, Curves, LUT.
    fn section_chips(&self, cx: &mut Context<Self>) -> AnyElement {
        self.chip_row("color-section", COLOR_SECTIONS.to_vec(), self.color_section, Player::show_color_section, cx)
    }

    /// The graph's picker: Histogram, Waveform, Vectorscope, Parade, one control.
    fn scope_chips(&self, cx: &mut Context<Self>) -> AnyElement {
        // Short plain names, so the four fit the dock on one line.
        let items = SCOPE_VIEWS
            .iter()
            .map(|&(view, label)| {
                let label = match label {
                    "Histogram" => "Levels",
                    "Waveform" => "Wave",
                    "Vectorscope" => "Vector",
                    "RGB parade" => "Parade",
                    other => other,
                };
                (view, label)
            })
            .collect();
        self.chip_row("color-scope", items, self.scope_view, Player::show_scope, cx)
    }

    /// The Wheels section: Lift, Gamma and Gain side by side (stacked when the
    /// dock is narrow), each a hue/saturation pad with a puck and a master bar.
    /// How to use them is in each pad's tooltip.
    fn wheel_rows(&self, params: ColorParams, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let wheels = div()
            .id("color-wheels")
            .w_full()
            .flex()
            .flex_wrap()
            .justify_between()
            .gap(px(8.))
            .children((0..3).map(|w| self.wheel_column(w, params, cx)))
            .into_any_element();
        vec![wheels]
    }

    /// One wheel: its name, the pad with the puck at its current tint, the master
    /// bar under it and the master's value.
    fn wheel_column(&self, w: usize, params: ColorParams, cx: &mut Context<Self>) -> AnyElement {
        let [master, r, g, b] = {
            let mut v = [0.; 4];
            for (i, slot) in v.iter_mut().enumerate() {
                *slot = band_value(&params, WHEEL_BAND + i, w, self.color_curve);
            }
            v
        };
        let (px_, py_) = wheel_puck(r, g, b);
        let tinted = r.abs() + g.abs() + b.abs() > 1e-4;
        let fraction = slider_fraction(master, -1., 1.);
        let press = move |master_bar: bool| {
            move |this: &mut Player, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Player>| {
                this.color_wheel = w;
                if event.click_count >= 2 {
                    this.reset_wheel(w, master_bar, cx);
                } else {
                    this.color_wheel_drag = Some((w, master_bar));
                    this.drag_wheel(w, master_bar, event.position, true, cx);
                }
            }
        };
        let pad = div()
            .id(("color-wheel-pad", w))
            .flex_none()
            .relative()
            .size(px(WHEEL_PAD))
            .cursor_pointer()
            .tooltip(overlay_tip_hover(
                WHEEL_NAMES[w],
                "",
                Some(&format!("{}. Drag the puck to tint; the bar under it sets the level; double-click resets", WHEEL_WHY[w])),
            ))
            .on_mouse_down(MouseButton::Left, cx.listener(press(false)))
            .child(bounds_probe(self.color_pads[w].clone()))
            .child(wheel_disc(px_, py_));
        let bar = div()
            .id(("color-wheel-master", w))
            .relative()
            .flex_none()
            .w(px(WHEEL_PAD))
            .h(px(HIT_MIN))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(press(true)))
            .child(bounds_probe(self.color_masters[w].clone()))
            .child(div().w_full().h(px(4.)).rounded(px(2.)).bg(rgb(DARK_HAIRLINE())))
            .child(
                div()
                    .absolute()
                    .top(px((HIT_MIN - 12.) / 2.))
                    .left(relative(fraction))
                    .ml(px(-6.))
                    .size(px(12.))
                    .rounded_full()
                    .bg(rgb(INK1())),
            );
        div()
            .id(("color-wheel-col", w))
            .flex_none()
            .w(px(WHEEL_PAD))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().flex_1().min_w(px(0.)).child(dark_row_label(WHEEL_NAMES[w], false)))
                    .child(kit::reset_slot(WHEEL_RESET_IDS[w], master.abs() > 1e-4 || tinted, {
                        let me = cx.entity();
                        move |_, cx| {
                            me.update(cx, |this, cx| {
                                let mut params = this.color_params();
                                for band in WHEEL_BAND..CURVE_BAND {
                                    set_band(&mut params, band, w, this.color_curve, 0.);
                                }
                                this.set_color(params, cx);
                            });
                        }
                    })),
            )
            .child(pad)
            .child(bar)
            .child(
                div().w_full().flex().justify_center().child(typed_readout(
                    WHEEL_VALUE_IDS[w],
                    &format!("{:+.0}%", master * 100.),
                    None,
                    56.,
                    if master.abs() > 1e-4 || tinted { INK1() } else { INK2() },
                    // Typed in percent, like the slider it sits under.
                    Some(SliderTyped {
                        lo: -100.,
                        hi: 100.,
                        set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                            let mut params = this.color_params();
                            set_band(&mut params, WHEEL_BAND, w, this.color_curve, (v / 100.) as f32);
                            this.set_color(params, cx);
                        }),
                    }),
                    None,
                    cx,
                )),
            )
            .into_any_element()
    }

    /// The Curves section: which channel, then the five points of its curve,
    /// drawn over the graph above.
    fn curve_rows(&self, params: ColorParams, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let channels: Vec<(usize, &'static str)> = CHANNEL_NAMES.iter().copied().enumerate().collect();
        let mut rows = vec![self.chip_row(
            "color-curve",
            channels,
            self.color_curve,
            |this, c, cx| {
                this.color_curve = c;
                cx.notify();
            },
            cx,
        )];
        // The curve's own square, never drawn over the scope above.
        rows.push(
            div()
                .id("color-curve-graph")
                .flex_none()
                .relative()
                .w_full()
                .h(px(CURVE_H))
                .rounded(px(3.))
                .bg(rgb(DARK_HAIRLINE()))
                .child(curve_overlay(params.look.curves[self.color_curve], self.color_curve))
                .into_any_element(),
        );
        rows.extend(section_bands(ColorSection::Curves).map(|band| self.color_row(band, params, cx)));
        rows
    }

    /// The LUT section: the file and whether it is in use, Choose / Remove (and
    /// Find when the file has gone), and the intensity.
    fn lut_rows(&self, params: ColorParams, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let lut = params.look.lut;
        let (name, state) = match lut {
            None => ("None chosen".to_string(), None),
            Some(r) => {
                let path = r.path();
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                let state = match r.table() {
                    Ok(c) => format!("{} table, {} per side · after wheels and curves", if c.three_d { "3D" } else { "1D" }, c.size),
                    Err(why) => format!("Not applied: {why}"),
                };
                (name, Some(state))
            }
        };
        let missing = lut.is_some_and(|r| r.problem().is_some());
        let mut rows = vec![
            div()
                .id("color-lut-file")
                .w_full()
                .min_h(px(HIT_MIN))
                .flex()
                .flex_col()
                .justify_center()
                .child(dark_row_label("Look file", false))
                .child(dark_help(name))
                .children(state.map(dark_help))
                .into_any_element(),
            div()
                .id("color-lut-actions")
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    Button::new("color-lut-choose")
                        .label(if lut.is_some() { "Change…" } else { "Choose…" })
                        .icon(Icon::FolderOpen)
                        .style(ButtonStyle::Secondary)
                        .build()
                        .tooltip(crate::ui::widgets::overlay_tip_hover(
                            "Pick a look file",
                            "",
                            Some("A .cube file: a colour look applied after the wheels and curves"),
                        ))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_lut(false, cx))),
                )
                .when(missing, |d| {
                    d.child(
                        Button::new("color-lut-find")
                            .label("Find file…")
                            .icon(Icon::Search)
                            .style(ButtonStyle::Secondary)
                            .build()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_lut(true, cx))),
                    )
                })
                .when(lut.is_some(), |d| {
                    d.child(
                        Button::new("color-lut-remove")
                            .label("Remove")
                            .icon(Icon::Trash2)
                            .style(ButtonStyle::Ghost)
                            .build()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_lut(cx))),
                    )
                })
                .into_any_element(),
        ];
        if lut.is_some() {
            rows.push(self.color_row(LUT_BAND, params, cx));
        }
        rows
    }

    /// One control's slider: its value on the bar, Reset back to its default
    /// once it is off it, and a tooltip saying what it does. The row is picked
    /// by a click anywhere on it, which is what the arrow keys move.
    fn color_row(&self, band: usize, params: ColorParams, cx: &mut Context<Self>) -> AnyElement {
        let (label, low, high, _) = band_spec(band);
        let (wheel, curve) = (self.color_wheel, self.color_curve);
        let value = band_value(&params, band, wheel, curve);
        let base = band_neutral(band);
        let fraction = slider_fraction(value, low, high);
        let on_press: SliderPress<Player> = Box::new(
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.color_band = band;
                this.color_dragging = true;
                this.drag_color(x, true, cx);
            },
        );
        let text = if (WHEEL_BAND..CURVE_BAND).contains(&band) {
            format!("{:+.0}%", value * 100.)
        } else {
            format!("{:.0}%", value * 100.)
        };
        let changed = (value - base).abs() > f32::EPSILON;
        // Typed in percent, the way the value reads.
        let typed = SliderTyped {
            lo: f64::from(low) * 100.,
            hi: f64::from(high) * 100.,
            set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                let mut params = this.color_params();
                set_band(&mut params, band, this.color_wheel, this.color_curve, (v / 100.) as f32);
                this.set_color(params, cx);
            }),
        };
        let reset = {
            let me = cx.entity();
            move |_: &mut Window, cx: &mut App| {
                me.update(cx, |this, cx| {
                    let mut params = this.color_params();
                    set_band(&mut params, band, this.color_wheel, this.color_curve, base);
                    this.set_color(params, cx);
                });
            }
        };
        div()
            .id(("color-row", band))
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(3.))
            .child(
                div()
                    .id(("color-bar", band))
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(COLOR_IDS[band])
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .id(("color-label", band))
                                            .tooltip(overlay_tip_hover(label, "", Some(band_why(band))))
                                            .child(dark_row_label(label, false)),
                                    )
                                    .child(typed_value_pill(RESET_IDS[band], &text, None, PILL_W, changed, Some(typed), reset, cx)),
                            )
                            .child(self.color_track(band, fraction, slider_fraction(base, low, high), on_press, cx)),
                    ),
            )
            .into_any_element()
    }

    /// A slider's track: the pointer presses and drags straight to a value. The fill runs
    /// from the neutral value (`neutral`, a fraction of the track) to the value, so a
    /// bipolar control (brightness, tint) shows which way it has been pushed. The neutral
    /// point carries two notches, above and below the track, clear of the thumb's footprint,
    /// so it stays visible when the thumb sits on it.
    fn color_track(
        &self,
        band: usize,
        fraction: f32,
        neutral: f32,
        on_press: SliderPress<Player>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const THUMB: f32 = 14.;
        const NOTCH: f32 = 4.;
        let t = crate::ui::theme::tokens();
        let (from, to) = track_span(slider_fraction(fraction, 0., 1.), slider_fraction(neutral, 0., 1.));
        div()
            .relative()
            .w_full()
            .h(px(HIT_MIN))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| on_press(this, event.position.x, cx)),
            )
            .child(bounds_probe(self.color_bars[band].clone()))
            .child(
                div().relative().w_full().h(px(4.)).rounded(px(2.)).bg(rgb(t.e3)).child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(from))
                        .w(relative(to - from))
                        .rounded(px(2.))
                        .bg(rgb(t.accent)),
                ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(relative(neutral))
                    .w(px(1.))
                    .h(px(NOTCH))
                    .bg(rgb(t.ink3)),
            )
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left(relative(neutral))
                    .w(px(1.))
                    .h(px(NOTCH))
                    .bg(rgb(t.ink3)),
            )
            .child(
                div()
                    .absolute()
                    .top(px((HIT_MIN - THUMB) / 2.))
                    .left(relative(fraction))
                    .ml(px(-THUMB / 2.))
                    .size(px(THUMB))
                    .rounded_full()
                    .bg(rgb(t.ink1)),
            )
    }
}

/// The lit part of a track, as (from, to) fractions: from the neutral point to the value, in
/// whichever order they fall, so a bipolar control fills out from its middle.
fn track_span(fraction: f32, neutral: f32) -> (f32, f32) {
    (neutral.min(fraction), neutral.max(fraction))
}

/// What moving a band's slider does, for its tooltip.
fn band_why(band: usize) -> &'static str {
    match band {
        0..WHEEL_BAND => COLOR_WHY[band],
        WHEEL_BAND => "All three channels of this wheel together",
        b if b < CURVE_BAND => "This channel alone; the master adds on top",
        b if b < LUT_BAND => "Where the curve puts this part of the tonal range; the straight line is as shot",
        _ => "How much of the look file (LUT) shows; 0% is the clip without it",
    }
}

#[cfg(test)]
mod tests {
    use super::track_span;

    #[test]
    fn a_bipolar_fill_runs_from_the_neutral_point_both_ways() {
        assert_eq!(track_span(0.25, 0.5), (0.25, 0.5));
        assert_eq!(track_span(0.75, 0.5), (0.5, 0.75));
        assert_eq!(track_span(0.5, 0.5), (0.5, 0.5));
        assert_eq!(track_span(0.8, 0.), (0., 0.8));
    }
}
