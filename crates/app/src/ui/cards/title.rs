//! The Title section of the Clip tab: the words, a one-click style, where it sits, how it is
//! drawn, when it is up and how it fades. It is in the inspector while a title is selected (from
//! the timeline's title row, the picture, or File > New > Title); every control writes one undo
//! step through [`Player::write_title`], and the viewer shows the words and numbers live.

use super::common::*;
use crate::player::titling::{HexTarget, ScrubField, anim_default_frames, hex_of, preset_label};
use engine::titleanim::TitleAnim;
use engine::project::TitlePreset as Preset;
use crate::ui::icons::Icon;
use crate::player::generators::GenPaint;
use crate::ui::kit::{self, NumberEdit, ScrubLook, Segment};
use crate::ui::type_scale::Typeset;
use crate::*;
use engine::text::{Align, HPlace, Outline, Place, Weight};

fn hsla_of(c: u32) -> gpui::Hsla {
    rgb(c).into()
}

/// A label with its control at the right, the shape every row of the section has.
fn row(label: &'static str, control: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(dark_row_label(label, false))
        .child(control)
}

/// What a number row says when it can change: the drag and the typing.
const DRAG_HINT: &str = "Drag to change, click to type";
/// The size row's tooltip: what the number is.
const SIZE_HINT: &str = "Text height as a share of the picture height";

/// The letters' swatches: white, the warm and cool tones a lower third wears, and black.
const INK_SWATCHES: [[u8; 3]; 8] = [
    [0xff, 0xff, 0xff],
    [0xff, 0xcc, 0x00],
    [0xff, 0x8a, 0x3d],
    [0xff, 0x4d, 0x4d],
    [0x4d, 0xd2, 0xff],
    [0x7b, 0xe0, 0x7b],
    [0xc9, 0xa0, 0xff],
    [0x11, 0x11, 0x11],
];

/// The animation names the Entrance and Exit lists show, in [`TitleAnim::ALL`]'s order.
const ANIM_NAMES: [&str; 7] = ["None", "Fade", "Typewriter", "Slide up", "Slide left", "Pop", "Word by word"];

/// Which side of a title an animation list sets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnimSide {
    In,
    Out,
}

/// The box's swatches: black (the default), deep tones, white and yellow.
const BOX_SWATCHES: [[u8; 3]; 8] = [
    [0x00, 0x00, 0x00],
    [0x11, 0x22, 0x33],
    [0x1f, 0x3a, 0x5f],
    [0x5a, 0x1a, 0x24],
    [0x1d, 0x4d, 0x3a],
    [0x44, 0x44, 0x44],
    [0xff, 0xff, 0xff],
    [0xff, 0xcc, 0x00],
];

impl Player {
    /// An Entrance or Exit row: the animation in force in the field, and a list of the eight
    /// beneath it. Picking one sets it and moves the playhead to the title's start, so the
    /// animation shows in the viewer at once.
    fn title_anim_row(&self, side: AnimSide, current: TitleAnim, fps: f64, start: u32, cx: &mut Context<Self>) -> Div {
        let (label, id, list_id) = match side {
            AnimSide::In => ("Entrance", "title-in", "title-in-list"),
            AnimSide::Out => ("Exit", "title-out", "title-out-list"),
        };
        let open = match side {
            AnimSide::In => self.title_in_open,
            AnimSide::Out => self.title_out_open,
        };
        let at = TitleAnim::ALL.iter().position(|a| *a == current).unwrap_or(0);
        let field = kit::dropdown_field(id, ANIM_NAMES[at], true).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.title_in_open = side == AnimSide::In && !this.title_in_open;
            this.title_out_open = side == AnimSide::Out && !this.title_out_open;
            cx.notify();
        }));
        let list = open.then(|| {
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.title_in_open = false;
                    this.title_out_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let names: Vec<SharedString> = ANIM_NAMES.iter().map(|&n| n.into()).collect();
            // The list opens above the field when the window has no room below it.
            let above = kit::list_opens_above(id, names.len());
            let me = cx.entity();
            let choices = kit::choice_list(list_id, 160., &names, at, move |pick, _, app| {
                me.update(app, |this, cx| {
                    this.title_in_open = false;
                    this.title_out_open = false;
                    let anim = TitleAnim::ALL[pick];
                    let frames = anim_default_frames(fps);
                    this.write_title(
                        |t| {
                            t.adopt_fades();
                            match side {
                                AnimSide::In => t.in_anim = anim,
                                AnimSide::Out => t.out_anim = anim,
                            }
                            if t.anim_frames == 0 {
                                t.anim_frames = frames.min(t.frames);
                            }
                        },
                        cx,
                    );
                    this.seek(f64::from(start) / fps, cx);
                });
            });
            let gap = px(crate::layout::redesign::CONTROL_M + 2.);
            let at = div().absolute().right(px(0.)).child(choices);
            let at = if above { at.bottom(gap) } else { at.top(gap) };
            gpui::deferred(div().absolute().top(px(0.)).right(px(0.)).child(backdrop).child(at))
        });
        row(label, div().relative().w(px(150.)).child(field).children(list))
    }

    /// A colour row: the hex field at the right, with a line of swatches under the label. The
    /// swatch of the current colour wears the accent ring (every swatch has a ring, transparent
    /// when off, so choosing one moves nothing). `enabled` false dims it and ignores clicks.
    #[allow(clippy::too_many_arguments)]
    fn title_colour_rows(
        &self,
        id: &'static str,
        label: &'static str,
        swatches: &'static [[u8; 3]],
        current: [u8; 3],
        target: HexTarget,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let tk = crate::ui::theme::tokens();
        let editing = self.title_hex.as_ref().filter(|(t, _)| *t == target);
        let value = match editing {
            Some((_, f)) => f.text.clone(),
            None => format!("#{}", hex_of(current).to_uppercase()),
        };
        let hex_id = if target == HexTarget::Ink { "title-ink-hex" } else { "title-box-hex" };
        let field = kit::scrub_field(
            hex_id,
            76.,
            value,
            None,
            // Dimmed by the row, not the field, so the reason stays readable.
            true,
            if editing.is_some() { Some(ScrubLook::Typing) } else { None },
        )
        .cursor(CursorStyle::IBeam)
        .tooltip(crate::ui::widgets::tip_hover(label, "type hex", Some("Six hex digits such as ffcc00; enter to apply")))
        .when(enabled, |el| {
            el.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.commit_gen_hex(cx);
                    this.begin_title_hex(target, cx);
                }),
            )
        });
        let dots = div().w_full().flex().items_center().justify_between().children(swatches.iter().enumerate().map(|(i, rgb)| {
            let rgb = *rgb;
            let on = rgb == current && editing.is_none();
            let [r, g, b] = rgb;
            div()
                .id((id, i))
                .flex_none()
                .size(px(22.))
                .p(px(2.))
                .rounded_full()
                .border_2()
                .border_color(if on { hsla_of(tk.accent) } else { gpui::transparent_black() })
                .when(enabled, |el| el.cursor_pointer())
                .child(
                    div()
                        .size_full()
                        .rounded_full()
                        .border_1()
                        .border_color(hsla_of(tk.line))
                        .bg(hsla_of(u32::from_be_bytes([0, r, g, b]))),
                )
                .tooltip(crate::ui::widgets::tip_hover(&format!("#{}", hex_of(rgb)), "", None))
                .when(enabled, |el| {
                    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.commit_title_hex(cx);
                        this.write_title(
                            |t| match target {
                                HexTarget::Ink => t.style.ink = rgb,
                                HexTarget::Box => t.style.box_color = [rgb[0], rgb[1], rgb[2], t.style.box_color[3]],
                            },
                            cx,
                        );
                    }))
                })
        }));
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(CARD_GAP))
            .child(row(label, field).when(!enabled, |d| d.opacity(0.5)))
            .child(dots.when(!enabled, |d| d.opacity(0.4)))
    }

    /// One number: drag it sideways to scrub, click it to type. `why` greys the row out.
    #[allow(clippy::too_many_arguments)]
    fn title_scrub_row(
        &self,
        id: &'static str,
        label: &'static str,
        value: String,
        unit: Option<&'static str>,
        field: ScrubField,
        why: Option<&'static str>,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let live = self.title_scrub.is_some_and(|s| s.field == field);
        let enabled = why.is_none();
        let typing = self.gen_hex.as_ref().filter(|(p, _)| *p == GenPaint::Title(field)).map(|(_, f)| NumberEdit::begin(&f.text));
        let control = match &typing {
            Some(_) => kit::number_field(id, 64., value.clone(), unit, typing.as_ref(), true),
            None => kit::scrub_field(id, 64., value.clone(), unit, true, live.then_some(ScrubLook::Hover)),
        }
        .tooltip(crate::ui::widgets::overlay_tip_hover(label, "", Some(why.unwrap_or(hint))));
        let line = row(
            label,
            if enabled {
                control
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.begin_title_scrub(field, f32::from(event.position.x), cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        // A press that never moved is a click: the number opens for typing.
                        let moved = match event {
                            ClickEvent::Mouse(m) => (m.up.position.x - m.down.position.x).abs() > px(3.),
                            _ => false,
                        };
                        if !moved {
                            this.begin_title_num(field, value.clone(), cx);
                        }
                    }))
            } else {
                control.cursor_not_allowed()
            },
        );
        line.when(!enabled, |d| d.opacity(0.5))
    }

    /// Opens a Title number for typing on `shown`, all of it selected.
    fn begin_title_num(&mut self, field: ScrubField, shown: String, cx: &mut Context<Self>) {
        self.commit_title_draft(cx);
        self.commit_gen_hex(cx);
        self.gen_hex = Some((GenPaint::Title(field), crate::player::text_field::TextField::selected(shown)));
        cx.notify();
    }

    /// The Title section, or nothing while no title is selected.
    pub(crate) fn title_card(&self, room: Size<Pixels>, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (_, stored) = self.title_target()?;
        let t = self.shown_title(&stored);
        let fps = self.session.as_ref().map_or(25., |s| s.meta().frame_rate).max(1.);
        // Every time reads in seconds with one decimal and its unit.
        let secs = move |frames: u32| format!("{:.1}", f64::from(frames) / fps);
        let me = cx.entity();
        let typing = self.title_draft.is_some();
        let words = match &self.title_draft {
            Some(f) => f.text.clone(),
            None => t.text.clone(),
        };

        let presets = div().w_full().flex().flex_wrap().gap(px(4.)).children(Preset::ALL.into_iter().enumerate().map(|(i, p)| {
            kit::Button::new(["title-preset-0", "title-preset-1", "title-preset-2"][i])
                .label(preset_label(p))
                .style(kit::ButtonStyle::Secondary)
                .size(kit::ButtonSize::Small)
                .build()
                .tooltip(crate::ui::widgets::tip_hover(preset_label(p), "", Some("Replaces the look; keeps the words and timing")))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.apply_title_preset(p, cx)))
                .into_any_element()
        }));

        let tk = crate::ui::theme::tokens();
        // Words selected for typing over (a new title, ctrl+A) wear the accent wash.
        let all = self.title_draft.as_ref().is_some_and(|f| f.all && !f.text.is_empty());
        let shown: AnyElement = match (words.is_empty(), typing) {
            (true, false) => SharedString::from("Type the words").into_any_element(),
            (_, true) if all => div().bg(rgba(tk.accent_wash)).child(words.clone()).into_any_element(),
            (_, true) => SharedString::from(format!("{words}▏")).into_any_element(),
            _ => SharedString::from(words.clone()).into_any_element(),
        };
        let field = div()
            .id("title-text")
            .w_full()
            .min_h(px(72.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(crate::layout::redesign::R_CONTROL))
            .bg(rgb(tk.e2))
            .border_1()
            .border_color(if typing { rgb(tk.accent).into() } else { gpui::transparent_black() })
            .cursor(CursorStyle::IBeam)
            .type_style(crate::ui::type_scale::body())
            .text_color(rgb(if words.is_empty() && !typing { tk.ink3 } else { tk.ink1 }))
            .child(shown)
            .tooltip(crate::ui::widgets::tip_hover("Words", "esc to finish", Some("enter starts a new line")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.commit_gen_hex(cx);
                    if this.title_draft.is_none()
                        && let Some((_, t)) = this.title_target()
                    {
                        this.title_draft = Some(crate::player::text_field::TextField { text: t.text, all: false });
                        cx.notify();
                    }
                }),
            );

        let place = kit::segmented(
            "title-place",
            &[Segment::text("Top"), Segment::text("Centre"), Segment::text("Bottom")],
            match t.style.place {
                Place::Top => 0,
                Place::Centre => 1,
                Place::Bottom => 2,
            },
            true,
            None,
            {
                let me = me.clone();
                move |pick, _, app| {
                    me.update(app, |this, cx| {
                        let place = [Place::Top, Place::Centre, Place::Bottom][pick];
                        this.write_title(|t| t.style.place = place, cx);
                    })
                }
            },
        );
        let outline = kit::segmented(
            "title-outline",
            &[Segment::text("None"), Segment::text("Thin"), Segment::text("Thick")],
            match t.style.outline {
                Outline::None => 0,
                Outline::Thin => 1,
                Outline::Thick => 2,
            },
            true,
            None,
            {
                let me = me.clone();
                move |pick, _, app| {
                    me.update(app, |this, cx| {
                        let outline = [Outline::None, Outline::Thin, Outline::Thick][pick];
                        this.write_title(|t| t.style.outline = outline, cx);
                    })
                }
            },
        );
        let side = kit::segmented(
            "title-side",
            &[Segment::text("Left"), Segment::text("Centre"), Segment::text("Right")],
            match t.style.hplace {
                HPlace::Left => 0,
                HPlace::Centre => 1,
                HPlace::Right => 2,
            },
            true,
            None,
            {
                let me = me.clone();
                move |pick, _, app| {
                    me.update(app, |this, cx| {
                        let hplace = [HPlace::Left, HPlace::Centre, HPlace::Right][pick];
                        this.write_title(|t| t.style.hplace = hplace, cx);
                    })
                }
            },
        );
        let weight = kit::segmented(
            "title-weight",
            &[Segment::text("Regular"), Segment::text("Bold")],
            usize::from(t.style.weight == Weight::Bold),
            true,
            None,
            {
                let me = me.clone();
                move |pick, _, app| {
                    me.update(app, |this, cx| {
                        let weight = [Weight::Regular, Weight::Bold][pick];
                        this.write_title(|t| t.style.weight = weight, cx);
                    })
                }
            },
        );
        let align = kit::segmented(
            "title-align",
            &[Segment::text("Left"), Segment::text("Centre"), Segment::text("Right")],
            match t.style.align {
                Align::Left => 0,
                Align::Centre => 1,
                Align::Right => 2,
            },
            true,
            None,
            {
                let me = me.clone();
                move |pick, _, app| {
                    me.update(app, |this, cx| {
                        let align = [Align::Left, Align::Centre, Align::Right][pick];
                        this.write_title(|t| t.style.align = align, cx);
                    })
                }
            },
        );
        let box_rgb = [t.style.box_color[0], t.style.box_color[1], t.style.box_color[2]];
        let boxed = kit::switch("title-box", t.style.boxed, true, None)
            .tooltip(crate::ui::widgets::tip_hover("Background box", "", Some("A translucent black plate behind the words")))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.write_title(|t| t.style.boxed = !t.style.boxed, cx);
            }));

        // The header (icon, words, length) is the dock's, like a clip's; the remove verb ends the card.
        let remove = kit::Button::new("title-remove")
            .label("Remove title")
            .icon(Icon::Trash2)
            .style(kit::ButtonStyle::Ghost)
            .build()
            .tooltip(crate::ui::widgets::tip_hover("Remove title", &self.keymap.chord(ActionId::Delete), None))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.remove_title_selected(cx)));

        Some(
            self.card_outer(room, cx).child(
                self.card_surface(div().w_full().flex().flex_col().gap(px(CARD_GAP)).px(px(6.)).py(px(6.)))
                    .id("title-card")
                    .child(presets)
                    .child(field)
                    .child(row("Position", place))
                    .child(row("Side", side))
                    .child(row("Align lines", align))
                    .child(row("Weight", weight))
                    .child(self.title_colour_rows("title-ink", "Colour", &INK_SWATCHES, t.style.ink, HexTarget::Ink, true, cx))
                    .child(row("Outline", outline))
                    .child(row("Background box", boxed))
                    .child(self.title_colour_rows("title-boxc", "Box colour", &BOX_SWATCHES, box_rgb, HexTarget::Box, t.style.boxed, cx))
                    .child(
                        self.title_scrub_row(
                            "title-box-opacity",
                            "Box opacity",
                            format!("{}", (u32::from(t.style.box_color[3]) * 100 + 127) / 255),
                            Some("%"),
                            ScrubField::BoxOpacity,
                            (!t.style.boxed).then_some("Turn on Background box"),
                            DRAG_HINT,
                            cx,
                        )
                    )
                    // The engine keeps the size per mille of the picture height; the card shows the share.
                    .child(self.title_scrub_row("title-size", "Size", format!("{:.1}", f64::from(t.style.size) / 10.), Some("%"), ScrubField::Size, None, SIZE_HINT, cx))
                    .child(self.title_scrub_row("title-start", "Start", secs(t.start), Some("s"), ScrubField::Start, None, DRAG_HINT, cx))
                    .child(self.title_scrub_row("title-length", "Length", secs(t.frames), Some("s"), ScrubField::Length, None, DRAG_HINT, cx))
                    .child(self.title_anim_row(AnimSide::In, t.entrance().0, fps, t.start, cx))
                    .child(self.title_anim_row(AnimSide::Out, t.exit().0, fps, t.start, cx))
                    .child(self.title_scrub_row(
                        "title-anim-length",
                        "Animation length",
                        secs(if t.anim_frames == 0 { anim_default_frames(fps) } else { t.anim_frames }),
                        Some("s"),
                        ScrubField::AnimLength,
                        (t.entrance().0 == TitleAnim::None && t.exit().0 == TitleAnim::None).then_some("Pick an entrance or an exit first"),
                        DRAG_HINT,
                        cx,
                    ))
                    .child(div().flex().justify_start().child(remove)),
            ),
        )
    }
}
