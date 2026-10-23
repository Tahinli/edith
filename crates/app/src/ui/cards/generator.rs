//! The Generator card: the parameters of a generated clip (solid colour, gradient, countdown),
//! in the Clip tab beside the other cards. It is in the dock only while the selected clip is a
//! generator with something to set; every control writes one undo step.

use super::common::*;
use crate::*;
use crate::player::generators::GenPaint;
use super::tools::drag_catcher;
use crate::ui::kit::{self, NumberEdit, Segment};
use engine::generate::{Form, Gen, Kind, Shape};

/// The colours the chips offer: the palette of a title card, plain enough to find by eye.
const SWATCHES: [u32; 12] = [
    0x000000, 0xffffff, 0x808080, 0xe53935, 0xfb8c00, 0xfdd835, 0x43a047, 0x00acc1, 0x1e88e5, 0x5e35b1, 0xd81b60, 0x6d4c41,
];

/// The chips a shape's rows offer: ten that fit one line beside None.
const SHAPE_SWATCHES: [u32; 10] = [0x000000, 0xffffff, 0xe53935, 0xfb8c00, 0xfdd835, 0x43a047, 0x00acc1, 0x1e88e5, 0x5e35b1, 0xd81b60];

impl Player {
    /// The generator under the selection's anchor, with where it sits.
    fn generator_target(&self) -> Option<(Lane, usize, Gen)> {
        let session = self.session.as_ref()?;
        let (lane, idx) = self.selected.anchor()?;
        session.generator_of(lane, idx).map(|g| (lane, idx, g))
    }

    /// A labelled number: the field at the right, which drags to scrub and types on a click.
    /// `why` greys the row out and says so on hover.
    fn num_row(&self, id: &'static str, label: &'static str, paint: GenPaint, unit: &'static str, why: Option<&'static str>, cx: &mut Context<Self>) -> Div {
        let value = self.gen_num_value(paint).unwrap_or(0.);
        let scrub = self.gen_scrub.filter(|s| s.paint == paint);
        let typing = self.gen_hex.as_ref().filter(|(p, _)| *p == paint).map(|(_, f)| NumberEdit::begin(&f.text));
        let enabled = why.is_none();
        // Dimmed by the row, not the field, so the reason stays readable.
        let field = kit::number_field(id, 76., format!("{:.0}", scrub.map_or(value, |s| s.cur)), Some(unit), typing.as_ref(), true)
            .tooltip(crate::ui::widgets::overlay_tip_hover(label, "", Some(why.unwrap_or("Drag to change, click to type"))))
            .when(enabled, |f| {
                f.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.begin_gen_scrub(paint, f32::from(event.position.x), cx);
                    }),
                )
            });
        div()
            .relative()
            .w_full()
            .min_h(px(crate::layout::redesign::CONTROL_M))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(dark_row_label(label, false))
            .child(field)
            .when(!enabled, |d| d.opacity(0.5))
            .child(drag_catcher(
                cx.entity(),
                scrub.is_some(),
                |this, x, fine, cx| this.gen_scrub_move(x, fine, cx),
                |this, cx| this.gen_scrub_end(cx),
            ))
    }

    /// A colour row: the label (and, where the paint may be absent, a None/Colour choice), then the
    /// swatches with the current colour ringed, custom ones included, and the hex field.
    #[allow(clippy::too_many_arguments)]
    fn paint_row(
        &self,
        tag: &'static str,
        label: &'static str,
        current: Option<u32>,
        paint: GenPaint,
        can_none: bool,
        colours: &'static [u32],
        target: (Lane, usize, Gen),
        cx: &mut Context<Self>,
    ) -> Div {
        let (lane, idx, spec) = target;
        let editing = self.gen_hex.as_ref().filter(|(p, _)| *p == paint).map(|(_, f)| f.text.clone());
        let write = move |this: &mut Self, c: Option<u32>, cx: &mut Context<Self>| {
            this.commit_gen_hex(cx);
            let kind = match (paint, spec.kind) {
                (GenPaint::Solid, Kind::Solid { .. }) => Kind::Solid { rgb: c.unwrap_or(0) },
                (GenPaint::From, Kind::Gradient { b, angle, .. }) => Kind::Gradient { a: c.unwrap_or(0), b, angle },
                (GenPaint::To, Kind::Gradient { a, angle, .. }) => Kind::Gradient { a, b: c.unwrap_or(0), angle },
                (_, Kind::Shape(mut sh)) => {
                    if paint == GenPaint::Fill {
                        sh.fill = c;
                    } else {
                        sh.stroke = c;
                        if c.is_some() && sh.width == 0 {
                            sh.width = 6;
                        }
                    }
                    Kind::Shape(sh)
                }
                (_, k) => k,
            };
            this.write_generator(lane, idx, Gen { kind, ..spec }, cx);
        };
        let mut col = div().w_full().flex().flex_col().gap(px(6.));
        let mut head = div().w_full().flex().items_center().justify_between().gap(px(8.)).child(dark_row_label(label, false));
        if can_none {
            let me = cx.entity();
            head = head.child(kit::segmented(
                tag,
                &[Segment::text("None"), Segment::text("Colour")],
                usize::from(current.is_some()),
                true,
                None,
                move |pick, _, app| {
                    me.update(app, |this, cx| write(this, (pick == 1).then_some(0xe53935), cx));
                },
            ));
        }
        col = col.child(head);
        if let Some(now) = current {
            let (me, me2) = (cx.entity(), cx.entity());
            col = col.child(kit::swatch_row(
                tag,
                colours,
                now,
                editing.as_deref(),
                move |c, _, app| me.update(app, |this, cx| write(this, Some(c), cx)),
                move |_, app| me2.update(app, |this, cx| this.begin_gen_hex(paint, cx)),
            ));
        }
        col
    }

    /// The rows of a shape: its form, fill, stroke, width and corner radius.
    fn shape_rows(&self, sh: Shape, target: (Lane, usize, Gen), rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let (lane, idx, spec) = target;
        let me = cx.entity();
        let form = kit::segmented_equal(
            "gen-form",
            &Form::ALL.map(|f| Segment::text(f.title())),
            Form::ALL.iter().position(|f| *f == sh.form).unwrap_or(0),
            true,
            None,
            move |pick, _, app| {
                me.update(app, |this, cx| {
                    let form = Form::ALL[pick];
                    let mut next = sh;
                    next.form = form;
                    if !form.fills() {
                        next.stroke = next.stroke.or(Shape::of(form).stroke);
                        next.width = if next.width == 0 { Shape::of(form).width } else { next.width };
                    } else if next.fill.is_none() && (next.stroke.is_none() || next.width == 0) {
                        next.fill = Shape::of(form).fill;
                    }
                    this.write_generator(lane, idx, Gen { kind: Kind::Shape(next), ..spec }, cx);
                })
            },
        );
        rows.push(div().w_full().child(form).into_any_element());
        if sh.form.fills() {
            let can_none = sh.stroke.is_some() && sh.width > 0;
            rows.push(self.paint_row("gen-fill", "Fill", sh.fill, GenPaint::Fill, can_none, &SHAPE_SWATCHES, target, cx).into_any_element());
        }
        let can_none = sh.form.fills() && sh.fill.is_some();
        rows.push(self.paint_row("gen-stroke", "Stroke", sh.stroke, GenPaint::Stroke, can_none, &SHAPE_SWATCHES, target, cx).into_any_element());
        let why = sh.stroke.is_none().then_some("Turn on a stroke to set its width");
        rows.push(self.num_row("gen-width", "Width", GenPaint::Width, "px", why, cx).into_any_element());
        if sh.form == Form::Rectangle {
            rows.push(self.num_row("gen-radius", "Corners", GenPaint::Radius, "px", None, cx).into_any_element());
        }
    }

    /// The Generator card, or nothing when the selected clip is not a generator with a setting.
    pub(crate) fn generator_card(&self, _room: Size<Pixels>, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let target = self.generator_target()?;
        let spec = target.2;
        let note = match spec.kind {
            Kind::Solid { .. } => None,
            Kind::Gradient { .. } => None,
            Kind::Countdown { .. } => None,
            Kind::Bars => Some("Nothing to set — the bars are fixed"),
            Kind::Shape(_) => None,
            Kind::Adjust => Some("Colour, Effects and Transform act on every layer below, for as long as this clip lasts"),
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        match spec.kind {
            Kind::Solid { rgb } => {
                rows.push(self.paint_row("gen-solid", "Colour", Some(rgb), GenPaint::Solid, false, &SWATCHES, target, cx).into_any_element())
            }
            Kind::Gradient { a, b, .. } => {
                rows.push(self.paint_row("gen-from", "From", Some(a), GenPaint::From, false, &SWATCHES, target, cx).into_any_element());
                rows.push(self.paint_row("gen-to", "To", Some(b), GenPaint::To, false, &SWATCHES, target, cx).into_any_element());
                rows.push(self.num_row("gen-angle", "Angle", GenPaint::Angle, "°", None, cx).into_any_element());
            }
            Kind::Countdown { .. } => rows.push(self.num_row("gen-from-s", "From", GenPaint::Count, "s", None, cx).into_any_element()),
            Kind::Shape(sh) => self.shape_rows(sh, target, &mut rows, cx),
            Kind::Bars | Kind::Adjust => {}
        }
        Some(
            self.card_outer(_room, cx).child(
                self.card_surface(div().w_full().flex().flex_col().gap(px(CARD_GAP)).px(px(6.)).py(px(6.)))
                    .id("gen-card")
                    .children(rows)
                    .children(note.map(dark_help)),
            ),
        )
    }
}
