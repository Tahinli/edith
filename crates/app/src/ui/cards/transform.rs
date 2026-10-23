//! The transform card: where a clip sits on the canvas and how it is turned.
//! Position, scale and rotation are the main rows; the crop rows and their
//! diagram sit under `More …`.

use super::common::*;
use crate::player::cards::{PIP_SCALE, TRANSFORM_MAIN_BANDS, is_position_band};
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{self, ButtonSize, ButtonStyle, Segment};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::transform_ui::{SCALE_BAND, TRANSFORM_BANDS, transform_snap};
use crate::*;
use engine::project::KeyKind;
use engine::stabilise::{Crop, Stabilise, Status};

/// Each row's slider id, in [`TRANSFORM_BANDS`] order.
const TRANSFORM_IDS: [&str; 8] = [
    "transform-pos-x",
    "transform-pos-y",
    "transform-scale",
    "transform-rotate",
    "transform-crop-l",
    "transform-crop-r",
    "transform-crop-t",
    "transform-crop-b",
];

/// The rotation chips, in the order the card lists them, with the degrees each
/// one sets. `−90` goes through [`transform_snap`], which stores it as 270.
const ROTATE_CHIPS: [(&str, f32); 4] = [("−90°", -90.), ("0°", 0.), ("+90°", 90.), ("180°", 180.)];

/// The Place row's choices, in the order the row reads.
const PLACE_CORNERS: [(&str, engine::transform::Corner); 5] = [
    ("Centre", engine::transform::Corner::Centre),
    ("Top left", engine::transform::Corner::TopLeft),
    ("Top right", engine::transform::Corner::TopRight),
    ("Bottom left", engine::transform::Corner::BottomLeft),
    ("Bottom right", engine::transform::Corner::BottomRight),
];

/// The crop diagram's box. Its own size, not the canvas's: the kept rectangle's
/// shape is what it shows.
const CROP_DIAGRAM_W: f32 = 120.;
const CROP_DIAGRAM_H: f32 = 68.;

/// One band's value in a copy of `params`.
fn band_value(params: TransformParams, band: usize) -> f32 {
    let mut read = params;
    *transform_band_mut(&mut read, band)
}

/// What a row reads on the card. Position is whole pixels of the canvas (the
/// engine keeps a fraction of it), scale is a percent of the clip's own size,
/// rotation reads 270 back as the −90 it was set from, and crop is a percent of
/// the frame.
pub(crate) fn band_readout(band: usize, value: f32, canvas: (u32, u32)) -> String {
    if is_position_band(band) {
        let span = (if band == 0 { canvas.0 } else { canvas.1 }) as f32;
        // `+ 0.0` turns a rounded -0.0 into 0, so a centred row reads "0 px".
        let px = (value * span).round() + 0.0;
        format!("{px:.0} px")
    } else if band == SCALE_BAND {
        format!("{:.0}%", value * 100.)
    } else if band == ROTATE_BAND && value == 270. {
        "−90°".to_owned()
    } else {
        transform_row_value(band, value)
    }
}

/// The crop diagram: the frame, with the rectangle the crop keeps drawn inside
/// it. The kept share is what is left after the four crops, so it is never
/// empty (each crop is at most 45%).
fn crop_diagram(params: TransformParams) -> impl IntoElement {
    let kept_w = 1. - params.crop_l - params.crop_r;
    let kept_h = 1. - params.crop_t - params.crop_b;
    div()
        .flex_none()
        .w(px(CROP_DIAGRAM_W))
        .h(px(CROP_DIAGRAM_H))
        .relative()
        .rounded(px(2.))
        .border_1()
        .border_color(rgba(DARK_SEAM()))
        .bg(rgb(DARK_PANEL()))
        .child(
            div()
                .absolute()
                .left(relative(params.crop_l))
                .top(relative(params.crop_t))
                .w(relative(kept_w))
                .h(relative(kept_h))
                .border_1()
                .border_color(rgb(INK3()))
                .bg(rgb(DARK_RAISED())),
        )
}

/// The strength chips, with the strength each one sets.
/// The deinterlace choices the Properties row offers, in the order they read.
const DEINTERLACE_CHIPS: [(&str, engine::project::Deinterlace, &str); 5] = [
    ("Frame", engine::project::Deinterlace::Frame, "Smooth: blends the two fields"),
    ("Field", engine::project::Deinterlace::Field, "Sharp: doubles the motion"),
    ("Off", engine::project::Deinterlace::Off, "Leave the lines as they are"),
    ("Film (auto)", engine::project::Deinterlace::IvtcAuto, "Back to 24 film frames; finds the pattern itself"),
    ("Film", engine::project::Deinterlace::Ivtc, "Back to 24 film frames from 30 fps TV"),
];
const STRENGTH_CHIPS: [(&str, u8); 3] = [("Light", 25), ("Medium", 50), ("Strong", 80)];

/// The edge chips: what the corrected frame does at its edges.
const EDGE_CHIPS: [(&str, Crop); 2] = [("Zoom in", Crop::Zoom), ("Full frame", Crop::Full)];

/// What the analysis line says.
pub(crate) fn stabilise_words(status: &Status, zoom: Option<f64>) -> String {
    match status {
        Status::Running(f) => format!("Analysing {:.0}%", f * 100.),
        Status::Ready => match zoom {
            Some(z) if z > 1.0005 => format!("Steadied · enlarged {:.0}% to hide the edges", (z - 1.) * 100.),
            _ => "Steadied".to_owned(),
        },
        Status::Failed(e) if e.contains("cancelled") => "Stopped — plays as shot until measured".to_owned(),
        Status::Failed(e) => format!("Could not measure the shake: {e}"),
        Status::Missing => "Not measured yet".to_owned(),
    }
}

/// Reset all: the whole placement back to identity.
fn reset_transform(this: &mut Player, cx: &mut Context<Player>) {
    this.set_transform(TransformParams::default(), cx);
}

impl Player {
    /// The Stabilise section: an on/off row, then (when on) how strong and what
    /// the edges do, and where the shake measurement stands. `None` for a clip
    /// with no camera to steady (a still, a song).
    fn transform_stabilise(&self, lane: Lane, idx: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.as_ref()?;
        let (path, lo, hi) = session.stabilise_range(lane, idx)?;
        let st = session.stabilise_of(lane, idx);
        let on = st.is_some();
        let me = cx.entity();
        let toggle = kit::switch_row(
            "transform-stabilise",
            "Stabilise",
            Some("Take the camera shake out of this clip".into()),
            on,
            None,
            move |_, app| {
                me.update(app, |this, cx| {
                    let next = if on { None } else { Some(Stabilise::default()) };
                    this.write_stabilise(lane, idx, next, cx);
                });
            },
        );
        let mut section = div().id("transform-stabilise-section").w_full().flex().flex_col().min_w(px(0.)).overflow_hidden().gap(px(4.)).px(px(6.)).child(toggle);
        let Some(st) = st else {
            return Some(section.into_any_element());
        };
        let label = |text: &'static str| div().type_style(type_scale::body()).text_color(rgb(INK2())).child(text);
        let me = cx.entity();
        let strength = kit::segmented_equal(
            "transform-stabilise-strength",
            &STRENGTH_CHIPS.map(|(text, _)| Segment::text(text)),
            STRENGTH_CHIPS.iter().position(|&(_, v)| v == st.strength()).unwrap_or(usize::MAX),
            true,
            None,
            move |pick, _, app| {
                me.update(app, |this, cx| {
                    this.write_stabilise(lane, idx, Some(Stabilise::with_percent(STRENGTH_CHIPS[pick].1, st.crop())), cx);
                });
            },
        );
        let me = cx.entity();
        let edges = kit::segmented_equal(
            "transform-stabilise-edges",
            &EDGE_CHIPS.map(|(text, _)| Segment::text(text)),
            EDGE_CHIPS.iter().position(|&(_, c)| c == st.crop()).unwrap_or(usize::MAX),
            true,
            None,
            move |pick, _, app| {
                me.update(app, |this, cx| {
                    this.write_stabilise(lane, idx, Some(Stabilise::with_percent(st.strength(), EDGE_CHIPS[pick].1)), cx);
                });
            },
        );
        let status = engine::stabilise::status(&path, lo, hi);
        let zoom = matches!(status, Status::Ready)
            .then(|| engine::stabilise::correction(&path, lo, hi, st, session.meta().frame_rate).map(|c| c.zoom))
            .flatten();
        let action: Option<AnyElement> = match status {
            Status::Running(_) => Some(
                dark_ghost_button(
                    "transform-stabilise-cancel",
                    "Stop",
                    "",
                    false,
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.cancel_stabilise(lane, idx, cx)),
                )
                .into_any_element(),
            ),
            Status::Failed(_) | Status::Missing => Some(
                dark_ghost_button(
                    "transform-stabilise-retry",
                    "Measure",
                    "",
                    false,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.wait_stabilise(lane, idx);
                        cx.notify();
                    }),
                )
                .into_any_element(),
            ),
            Status::Ready => None,
        };
        section = section
            .child(label("Strength"))
            .child(strength)
            .child(label("Edges"))
            .child(edges)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap(px(8.))
                    // The Stop / Measure button comes and goes with the analysis;
                    // the row keeps its height either way.
                    .min_h(px(CONTROL_H))
                    .child(dark_help(stabilise_words(&status, zoom)).flex_1().w_auto())
                    .children(action),
            );
        Some(section.into_any_element())
    }

    /// The field order (`tff` / `bff`) of the clip's source, `None` for a progressive one, one
    /// not probed yet, and anything that is not footage.
    pub(crate) fn clip_interlaced(&self, lane: Lane, idx: usize) -> Option<&'static str> {
        let (path, _, _) = self.session.as_ref()?.stabilise_range(lane, idx)?;
        self.facts.get(&path)?.as_ref()?.video.as_ref()?.interlaced
    }

    /// The Deinterlace row: Frame / Field / Off, only for a clip whose source is interlaced.
    fn transform_deinterlace(&self, lane: Lane, idx: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let order = self.clip_interlaced(lane, idx)?;
        let order_why = if order == "tff" {
            "This video's lines come top field first"
        } else {
            "This video's lines come bottom field first"
        };
        let current = self.session.as_ref()?.deinterlace_of(lane, idx);
        let at = DEINTERLACE_CHIPS.iter().position(|&(_, m, _)| m == current).unwrap_or(0);
        let open = self.deinterlace_open;
        let field = kit::dropdown_field("transform-deinterlace", DEINTERLACE_CHIPS[at].0, true)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.deinterlace_open = !this.deinterlace_open;
                cx.notify();
            }));
        let list = open.then(|| {
            let t = crate::ui::theme::tokens();
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.deinterlace_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let mut plate = kit::plate(6.).id("transform-deinterlace-list").w(px(280.)).p(px(crate::layout::MENU_PAD)).flex().flex_col().occlude();
            for (i, &(name, mode, why)) in DEINTERLACE_CHIPS.iter().enumerate() {
                plate = plate.child(
                    div()
                        .id(("transform-deinterlace-row", i))
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                        .child(div().flex_none().size(px(16.)).children((i == at).then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(div().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(name))
                                .child(dark_help(why)),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.deinterlace_open = false;
                            this.write_deinterlace(lane, idx, mode, cx);
                        })),
                );
            }
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .child(backdrop)
                    .child(div().absolute().top(px(crate::layout::redesign::CONTROL_M + 2.)).right(px(0.)).child(plate)),
            )
        });
        Some(
            div()
                .id("transform-deinterlace-section")
                .w_full()
                .flex()
                .flex_col()
                .min_w(px(0.))
                .gap(px(4.))
                .pt(px(4.))
                .px(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(div().type_style(type_scale::body()).text_color(rgb(INK2())).id("transform-deinterlace-label").tooltip(crate::ui::widgets::overlay_tip_hover("Deinterlace", "", Some(order_why))).child("Deinterlace"))
                        .child(div().relative().flex_none().w(px(150.)).child(field).children(list)),
                )
                // Always two lines tall, so the longest line never moves what is below.
                .child(dark_help(DEINTERLACE_CHIPS[at].2).min_h(px(30.)))
                .into_any_element(),
        )
    }

    /// The Reframe row: one click follows the subject and keys the placement so the
    /// clip fills the project ([`engine::reframe`]). It is an action, not a switch: while
    /// it measures the button reads Stop and the caption shows the percent; once it has
    /// landed on this clip the caption says how many keys it wrote. `None` for a clip with no footage.
    fn transform_reframe(&self, lane: Lane, idx: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.as_ref()?;
        session.stabilise_range(lane, idx)?;
        let running = self.reframe_run.is_some();
        let caption = match (self.reframe_words(), self.reframed) {
            (Some(words), _) => words,
            (None, Some((l, i, n))) if l == lane && i == idx => {
                format!("Reframed with {n} key{}", if n == 1 { "" } else { "s" })
            }
            (None, _) => "Follows the subject so this clip fills the project".to_string(),
        };
        let me = cx.entity();
        let button = kit::Button::new("transform-reframe")
            .label(if running { "Stop reframing" } else { "Reframe to fill the frame" })
            .icon(Icon::Scan)
            .style(ButtonStyle::Secondary)
            .size(ButtonSize::Small)
            .build()
            .w_full()
            .justify_center()
            .on_click(move |_, _, app| {
                me.update(app, |this, cx| {
                    if this.reframe_run.is_some() {
                        this.cancel_reframe(cx);
                    } else {
                        this.start_reframe(lane, idx, cx);
                    }
                });
            });
        Some(
            div()
                .w_full()
                .px(px(6.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(button)
                .child(div().type_style(type_scale::caption()).text_color(rgb(INK3())).child(caption))
                .into_any_element(),
        )
    }

    /// The transform card: [`color_card`](Self::color_card)'s own shape, the
    /// placement's rows and their rotation chips, and the crop under a More
    /// fold. There is no histogram -- nothing here a graded frame would tilt.
    pub(crate) fn transform_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.transform_open?;
        let params = self.transform_params();
        let canvas = self.transform_canvas();
        let crop_open = self.transform_crop_open();
        // Rotation has its own row of quarter turns below, not a bar.
        let mut rows: Vec<AnyElement> = (0..TRANSFORM_MAIN_BANDS)
            .filter(|&band| band != ROTATE_BAND)
            .map(|band| self.transform_slider(band, params, canvas, cx))
            .collect();
        rows.push(self.transform_rotation(params, canvas, cx));
        rows.push(self.transform_place(params, cx));
        rows.push(self.opacity_row(params.opacity, cx));
        rows.push(self.key_row(KeyKind::Placement, cx));
        rows.push(self.key_row(KeyKind::Opacity, cx));
        if let Some(row) = self.transform_stabilise(lane, idx, cx) {
            rows.push(row);
        }
        if let Some(row) = self.transform_deinterlace(lane, idx, cx) {
            rows.push(row);
        }
        if let Some(row) = self.transform_reframe(lane, idx, cx) {
            rows.push(row);
        }
        rows.push(
            disclosure_row(
                "transform-more",
                "Crop",
                crop_open,
                |this: &mut Player, cx: &mut Context<Player>| this.toggle_transform_crop(cx),
                cx,
            )
            .into_any_element(),
        );
        if crop_open {
            rows.push(self.transform_crop(params, canvas, cx));
        }
        let help_text = "drag a bar, or ↑↓ picks one and ←→ moves it (rotation steps by 90°, \
                     position by 10 px), r resets — a click away or esc closes";
        // The head's Reset all shows only once the placement is off identity.
        let reset_all: Option<fn(&mut Player, &mut Context<Player>)> = if params.is_identity() {
            None
        } else {
            Some(reset_transform as fn(&mut Player, &mut Context<Player>))
        };
        Some(
            self.card_outer(viewport, cx)
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(
                            TRANSFORM_W,
                            self.card_maximized,
                            f32::from(viewport.width),
                        )))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
.map(|d| self.card_surface(d))
                        .child(
                            div()
                                .id("transform-reset")
                                .w_full()
                                .flex()
                                .justify_end()
                                .child(dark_card_head_reset("", None, Some(help_text.into()), None, reset_all, cx)),
                        )
                        .children(rows),
                ),
        )
    }

    /// One slider row: the shared [`slider_row`] for one band, under a tooltip
    /// that names it.
    fn transform_slider(
        &self,
        band: usize,
        params: TransformParams,
        canvas: (u32, u32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (label, _, _) = TRANSFORM_BANDS[band];
        let value = band_value(params, band);
        let default = band_value(TransformParams::default(), band);
        let on_press: SliderPress<Player> = Box::new(
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.transform_band = band;
                this.opacity_drag = false;
                this.transform_dragging = true;
                this.drag_transform(x, true, cx);
            },
        );
        let on_reset: SliderReset<Player> =
            Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
                let old = this.transform_params();
                let mut params = old;
                *transform_band_mut(&mut params, band) = default;
                if band == SCALE_BAND {
                    params = this.scaled_in_place(old, params);
                }
                this.set_transform(params, cx);
            });
        // Typed in the unit the row reads: pixels for position, percent for scale and crop.
        let typed = (band != ROTATE_BAND).then(|| {
            let (_, low, high) = TRANSFORM_BANDS[band];
            let scale = if is_position_band(band) {
                (if band == 0 { canvas.0 } else { canvas.1 }).max(1) as f32
            } else {
                100.
            };
            SliderTyped {
                lo: f64::from(low * scale),
                hi: f64::from(high * scale),
                set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    let old = this.transform_params();
                    let mut params = old;
                    *transform_band_mut(&mut params, band) = (v / f64::from(scale)) as f32;
                    if band == SCALE_BAND {
                        params = this.scaled_in_place(old, params);
                    }
                    this.set_transform(params, cx);
                }),
            }
        });
        let row = SliderRow {
            id: TRANSFORM_IDS[band],
            label: label.into(),
            value: band_readout(band, value, canvas).into(),
            fraction: knob_fraction(band, value),
            changed: value != default,
            bar: self.transform_bars[band].clone(),
            on_press,
            on_reset: Some(on_reset),
            typed,
        };
        // The ids are the ones the keys menu's reach points at: a row to pick,
        // and a bar the arrows move.
        div()
            .id(("transform-row", band))
            .w_full()
            .flex()
            .flex_col()
            .px(px(6.))
            .rounded(px(3.))
            .tooltip(overlay_tip_hover(label, "", None))
            .child(
                div()
                    .id(("transform-bar", band))
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(slider_row(row, cx)),
            )
            .into_any_element()
    }

    /// Rotation: a read-out and four chips (−90, 0, +90, 180). Rotation has no
    /// slider, because the engine only ever renders a quarter turn.
    fn transform_rotation(
        &self,
        params: TransformParams,
        canvas: (u32, u32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = band_value(params, ROTATE_BAND);
        let me = cx.entity();
        let chips = kit::segmented_equal(
            "transform-rotate-chip",
            &ROTATE_CHIPS.map(|(text, _)| Segment::text(text)),
            ROTATE_CHIPS.iter().position(|&(_, d)| value == transform_snap(ROTATE_BAND, d)).unwrap_or(usize::MAX),
            true,
            None,
            move |pick, _, app| {
                me.update(app, |this, cx| {
                    this.transform_band = ROTATE_BAND;
                    let mut params = this.transform_params();
                    params.rotate = transform_snap(ROTATE_BAND, ROTATE_CHIPS[pick].1);
                    this.set_transform(params, cx);
                    cx.notify();
                });
            },
        );
        let reset = cx.entity();
        div()
            .id("transform-rotation")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(6.))
            .rounded(px(3.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().type_style(type_scale::body()).text_color(rgb(INK2())).child("Rotation"))
                    // The value box sits where a slider row's does: its reset slot is 16px and
                    // 8px off the box (value_pill's is 24px and 2px off), so pull the box 2px right.
                    .child(div().mr(px(-2.)).child(typed_value_pill(
                        "transform-rotate-pill",
                        &band_readout(ROTATE_BAND, value, canvas),
                        None,
                        56.,
                        value != 0.,
                        // Typed in degrees. The engine only turns a quarter at a time, so the
                        // number is snapped to the nearest quarter turn, as the chips are.
                        Some(SliderTyped {
                            // From -90, which reads the same turn as 270.
                            lo: -90.,
                            hi: f64::from(TRANSFORM_BANDS[ROTATE_BAND].2),
                            set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                                this.transform_band = ROTATE_BAND;
                                let mut params = this.transform_params();
                                params.rotate = transform_snap(ROTATE_BAND, v as f32);
                                this.set_transform(params, cx);
                                cx.notify();
                            }),
                        }),
                        move |_, app| {
                            reset.update(app, |this, cx| {
                                let mut params = this.transform_params();
                                params.rotate = 0.;
                                this.set_transform(params, cx);
                            });
                        },
                        cx,
                    ))),
            )
            .child(chips)
            .into_any_element()
    }

    /// The Place row: where the clip sits, as one of five corners. A dropdown
    /// shows the corner the position matches (Custom when the position is
    /// none of them) and lists the five; a Picture in picture button beside the label
    /// sets bottom right at 30%. Disabled, with the reason as its tooltip, when
    /// the clip has no picture size to place by.
    fn transform_place(&self, params: TransformParams, cx: &mut Context<Self>) -> AnyElement {
        let Some((lane, idx)) = self.transform_open else {
            return div().into_any_element();
        };
        let size = self.clip_picture_size(lane, idx);
        let canvas = self.transform_canvas();
        let current = size.and_then(|size| engine::transform::corner_of(&params, size, canvas, engine::transform::CORNER_MARGIN_PX));
        let picked = current.and_then(|c| PLACE_CORNERS.iter().position(|&(_, k)| k == c)).unwrap_or(usize::MAX);
        let why = size.is_none().then_some("This clip has no picture size to place by");
        let enabled = why.is_none();
        // A position that is no named corner (after Auto reframe, or dragged) reads Custom.
        let value = PLACE_CORNERS.get(picked).map_or("Custom", |&(name, _)| name);
        let field = kit::dropdown_field("transform-place", value, enabled).when(enabled, |f| {
            f.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.place_open = !this.place_open;
                cx.notify();
            }))
        });
        let list = (self.place_open && enabled).then(|| {
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.place_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let names: Vec<SharedString> = PLACE_CORNERS.iter().map(|&(name, _)| name.into()).collect();
            // The list opens above the field when the window has no room below it.
            let above = kit::list_opens_above("transform-place", names.len());
            let me = cx.entity();
            let choices = kit::choice_list("transform-place-list", 200., &names, picked, move |pick, _, app| {
                me.update(app, |this, cx| {
                    this.place_open = false;
                    this.place_in_corner(PLACE_CORNERS[pick].1, None, cx);
                });
            });
            let gap = px(crate::layout::redesign::CONTROL_M + 2.);
            let at = div().absolute().right(px(0.)).child(choices);
            let at = if above { at.bottom(gap) } else { at.top(gap) };
            gpui::deferred(div().absolute().top(px(0.)).right(px(0.)).child(backdrop).child(at))
        });
        let mut pip = kit::states(
            div()
                .id("transform-pip")
                .flex_none()
                .h(px(CONTROL_H))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .type_style(type_scale::body())
                .text_color(rgb(if enabled { tokens().ink1 } else { tokens().ink4 }))
                .child("Picture in picture"),
            kit::Fills::ghost(),
            enabled,
            None,
        )
        .tooltip(overlay_tip_hover(
            "Picture in picture",
            "",
            Some(why.unwrap_or("Bottom right at 30%, with a 32 px margin")),
        ));
        if enabled {
            let me = cx.entity();
            pip = pip.on_click(move |_, _, app| {
                me.update(app, |this, cx| this.place_in_corner(engine::transform::Corner::BottomRight, Some(PIP_SCALE), cx));
            });
        }
        div()
            .id("transform-place")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(6.))
            .rounded(px(3.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().type_style(type_scale::body()).text_color(rgb(INK2())).child("Place"))
                    .child(pip),
            )
            .child(div().relative().w_full().child(field).children(list))
            .into_any_element()
    }

    /// The crop rows under the More fold: the diagram, then one slider per side.
    fn transform_crop(
        &self,
        params: TransformParams,
        canvas: (u32, u32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows: Vec<AnyElement> = (TRANSFORM_MAIN_BANDS..TRANSFORM_BANDS.len())
            .map(|band| self.transform_slider(band, params, canvas, cx))
            .collect();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pt(px(4.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(6.))
                    .child(crop_diagram(params))
                    .child(dark_help("The lighter part stays in the picture; the rest is cut away.").flex_1().w_auto()),
            )
            .children(rows)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::cards::snap_position_px;

    const HD: (u32, u32) = (1920, 1080);

    #[test]
    fn the_stabilise_line_reads_in_plain_words() {
        assert_eq!(stabilise_words(&Status::Running(0.4), None), "Analysing 40%");
        assert_eq!(stabilise_words(&Status::Ready, Some(1.07)), "Steadied · enlarged 7% to hide the edges");
        assert_eq!(stabilise_words(&Status::Ready, Some(1.0)), "Steadied");
        assert!(stabilise_words(&Status::Failed("stabilise analysis cancelled".into()), None).starts_with("Stopped"));
    }

    #[test]
    fn position_reads_in_whole_canvas_pixels_with_no_minus_on_zero() {
        assert_eq!(band_readout(0, 0.5, HD), "960 px");
        assert_eq!(band_readout(1, -0.25, HD), "-270 px");
        assert_eq!(band_readout(0, -0.0, HD), "0 px");
    }

    #[test]
    fn scale_and_crop_read_as_percent_and_rotation_reads_minus_ninety() {
        assert_eq!(band_readout(SCALE_BAND, 1.0, HD), "100%");
        assert_eq!(band_readout(SCALE_BAND, 0.1, HD), "10%");
        assert_eq!(band_readout(4, 0.25, HD), "25%");
        assert_eq!(band_readout(ROTATE_BAND, 270.0, HD), "−90°");
        assert_eq!(band_readout(ROTATE_BAND, 90.0, HD), "90°");
    }

    #[test]
    fn a_position_drag_lands_on_a_whole_pixel_inside_its_range() {
        for raw in [0.1234_f32, 0.5004, -0.3333, 0.99999] {
            let snapped = snap_position_px(0, raw, HD);
            let px = snapped * 1920.;
            assert!((px - px.round()).abs() < 1e-3, "{raw} gave {px} px");
            assert!(
                (snapped - raw).abs() <= 0.5 / 1920. + 1e-6,
                "{raw} moved to {snapped}"
            );
        }
        assert_eq!(snap_position_px(0, 5., HD), 1.);
        assert_eq!(snap_position_px(1, -5., HD), -1.);
    }
}
