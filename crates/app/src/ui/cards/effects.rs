//! The effects card: the selected picture clip's stack of effects, top to bottom in the
//! order they are applied. Each row switches, moves or removes its effect and carries one
//! control per setting; a button at the foot adds one.

use super::common::*;
use crate::player::generators::GenPaint;
use crate::player::effects::{ParamShape, mode_text, param_display, param_shape, param_unit};
use crate::ui::icons::{Icon, IconSize};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::kit::{self, Segment};
use crate::ui::widgets::{SliderRow, SliderTyped, slider_row};
use crate::*;
use engine::effects::{BlendMode, Effect, EffectKind, MAX_EFFECTS};

/// What an effect is called in the add list and its row: plain words, not the engine's name.
fn kind_name(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Blend => "Blend mode",
        other => other.label(),
    }
}

/// The one-line description a list row carries as its tooltip ("" for none).
fn kind_hint(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Blend => "Mix this picture with the lanes under it",
        EffectKind::Pixelate => "Blocks of colour; a mask below it limits them to its shape",
        _ => "",
    }
}

/// What a blend mode does, in plain words: the one line its list row carries.
fn blend_why(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Shows this picture as it is, covering the lanes under it",
        BlendMode::Multiply => "Darkens: this picture and the lanes under it multiply, so both darken",
        BlendMode::Screen => "Lightens: the dark parts of this picture drop out over the lanes under it",
        BlendMode::Overlay => "More contrast: darks get darker and lights get lighter over the lanes under it",
        BlendMode::Add => "Adds this picture's light to the lanes under it, so bright parts glow",
        BlendMode::Darken => "Keeps the darker of this picture and the lanes under it, pixel by pixel",
        BlendMode::Lighten => "Keeps the lighter of this picture and the lanes under it, pixel by pixel",
    }
}

/// The icon an effect's row in the add list wears.
fn kind_icon(kind: EffectKind) -> Icon {
    match kind {
        EffectKind::Blur => Icon::WandSparkles,
        EffectKind::Sharpen => Icon::Scan,
        EffectKind::Vignette => Icon::Eye,
        EffectKind::Glow => Icon::Sparkles,
        EffectKind::Shake => Icon::Move,
        EffectKind::ChromaKey => Icon::Palette,
        EffectKind::Blend => Icon::Layers,
        EffectKind::RectMask => Icon::Square,
        EffectKind::EllipseMask => Icon::Circle,
        EffectKind::Pixelate => Icon::Grid3x3,
    }
}

impl Player {
    /// The effects card. The stack is the clip's own, or the draft of a bar being held;
    /// for an audio clip or no clip it says so in words instead of showing rows.
    pub(crate) fn effects_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.effects_open?;
        let session = self.session.as_ref()?;
        let on_picture = lane.kind == LaneKind::Video && session.lane_clips(lane).get(idx).is_some();
        // The rows show what each effect is at the playhead: a keyed mask's shape there.
        let at = self.effects_src().unwrap_or(0);
        let stack: Vec<Effect> = self.effects_shown().iter().map(|e| e.posed(f64::from(at)).into_owned()).collect();
        let count = stack.len();
        let rows: Vec<AnyElement> = stack
            .iter()
            .enumerate()
            .map(|(i, e)| self.effect_row(i, e, count, cx))
            .collect();
        let help_text: SharedString = "switch an effect on or off, move it up or down the stack, or take it away; drag a bar to set it — a click away or esc closes".into();
        let tail_text: SharedString = match (on_picture, count) {
            (false, _) => "effects belong to picture clips — select one".into(),
            (true, n) if n >= MAX_EFFECTS => format!("All {MAX_EFFECTS} effects are in use — remove one to add another").into(),
            (true, _) => "Effects run top to bottom".into(),
        };
        let adding = self.effects_adding;
        let kinds: Vec<AnyElement> = EffectKind::ALL
            .into_iter()
            .enumerate()
            .map(|(k, kind)| {
                let row = crate::ui::popup::popup_row_icon(("effects-kind", k), kind_name(kind), "", Some(false), Some(kind_icon(kind)), None);
                let hint = kind_hint(kind);
                row.when(!hint.is_empty(), |r| r.tooltip(crate::ui::widgets::overlay_tip_hover(kind_name(kind), "", Some(hint))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.add_effect(kind, cx)))
                    .into_any_element()
            })
            .collect();
        let add_button = kit::Button::new("effects-add")
            .label("Add effect")
            .icon(Icon::Plus)
            .style(kit::ButtonStyle::Secondary)
            .on(adding)
            .enabled(count < MAX_EFFECTS)
            .build()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.effects_adding = !this.effects_adding;
                cx.notify();
            }));
        let empty = (on_picture && count == 0).then(|| {
            div().w_full().py(px(8.)).flex().justify_center().text_color(rgb(INK2())).child("No effects yet")
        });
        Some(
            self.card_outer(viewport, cx)
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(COLOR_W, self.card_maximized, f32::from(viewport.width))))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .map(|d| self.card_surface(d))
                        .child(div().id("effects-reset").w_full().flex().justify_end().child(dark_card_head_reset("", None, Some(help_text), None, None, cx)))
                        .child(
                            div()
                                .id("effects-rows")
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .children(empty)
                        .when(on_picture, |d| {
                            d.child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_none()
                                    .child(add_button)
                                    .when(adding, |d| {
                                        d.child(gpui::deferred(
                                            kit::plate(6.)
                                                .id("effects-kinds")
                                                .absolute()
                                                .top(px(36.))
                                                .left(px(0.))
                                                .w(px(200.))
                                                .p(px(crate::layout::MENU_PAD))
                                                .flex()
                                                .flex_col()
                                                .occlude()
                                                .children(kinds),
                                        ).with_priority(1))
                                    }),
                            )
                        })
                        .child(dark_help(tail_text)),
                ),
        )
    }

    /// One effect's row: its switch, name and the three buttons, then a control per setting.
    /// The Track row under a mask: follows the picture under it from the playhead, forward or
    /// backward, and shows how far that has got with a Cancel while it runs. The keys land
    /// when it ends, and show as diamonds on the position rows.
    fn track_row(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let job = self
            .effects_open
            .and_then(|(lane, idx)| self.track_jobs.iter().find(|j| j.lane == lane && j.idx == idx && j.slot == i));
        let progress = job.and_then(|j| match engine::track::status(j.id) {
            engine::track::Status::Running(f) => Some(f),
            engine::track::Status::Finished => Some(1.0),
            engine::track::Status::Gone => None,
        });
        let (words, buttons): (String, Vec<AnyElement>) = match progress {
            Some(f) => (
                format!("Tracking {:.0}%", f * 100.),
                vec![
                    dark_ghost_button(
                        "effects-track-cancel",
                        "Cancel",
                        "",
                        false,
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.cancel_track(i, cx)),
                    )
                    .into_any_element(),
                ],
            ),
            None => (
                "Follows the picture under the mask from the playhead".to_owned(),
                vec![
                    dark_ghost_button(
                        "effects-track-back",
                        "Track backward",
                        "",
                        false,
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.start_track(i, true, cx)),
                    )
                    .into_any_element(),
                    dark_ghost_button(
                        "effects-track-forward",
                        "Track forward",
                        "",
                        false,
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.start_track(i, false, cx)),
                    )
                    .into_any_element(),
                ],
            ),
        };
        // The words take the full width, the buttons sit under them, so neither is squeezed.
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(dark_help(words).w_full())
            .child(div().flex().justify_end().gap(px(6.)).min_h(px(CONTROL_H)).children(buttons))
            .into_any_element()
    }

    fn effect_row(&self, i: usize, e: &Effect, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let on = e.on;
        let me = cx.entity();
        let mut controls: Vec<AnyElement> = Vec::new();
        for (p, spec) in e.kind.params().iter().enumerate() {
            let spec = *spec;
            if e.kind == EffectKind::ChromaKey && (1..=2).contains(&p) {
                continue;
            }
            if e.kind == EffectKind::ChromaKey && p == 0 {
                let current = ((e.values[0] as u32) << 16) | ((e.values[1] as u32) << 8) | e.values[2] as u32;
                let (m, m2) = (me.clone(), me.clone());
                let typing = self.gen_hex.as_ref().filter(|(p, _)| *p == GenPaint::Key(i)).map(|(_, f)| f.text.clone());
                controls.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(dark_row_label("Key colour", false))
                        .child(kit::swatch_row(
                            "effects-key",
                            &[0x00ff00, 0x0000ff, 0xff00ff, 0x00ffff],
                            current,
                            typing.as_deref(),
                            move |c, _, app| {
                                m.update(app, |this, cx| {
                                    this.gen_hex = None;
                                    this.set_effect_colour(i, c, cx);
                                });
                            },
                            move |_, app| m2.update(app, |this, cx| this.begin_key_hex(i, cx)),
                        ))
                        .into_any_element(),
                );
                continue;
            }
            let m = me.clone();
            let control = match param_shape(e.kind, &spec) {
                ParamShape::Bar if spec.name == "seed" => {
                    let seed = e.values[p] as usize;
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(dark_row_label(spec.label, false))
                        .child(kit::segmented_equal(
                            "effects-pattern",
                            &["1", "2", "3", "4", "5"].map(Segment::text),
                            if (1..=5).contains(&seed) { seed - 1 } else { usize::MAX },
                            on,
                            None,
                            move |pick, _, app| m.update(app, |this, cx| this.set_effect_param(i, p, (pick + 1) as f32, cx)),
                        ))
                        .into_any_element()
                }
                ParamShape::Bar => {
                    let value = e.values[p];
                    let scale = param_unit(e.kind, &spec).scale();
                    // Wrapped so the shared bar id is unique under this row and setting.
                    div()
                        .id(("effects-param", i * 8 + p))
                        .child(slider_row(
                            SliderRow {
                                // Unique per bar: a typed value keeps its typing under the id.
                                id: super::mix::interned_id(format!("effects-bar-{i}-{p}")),
                                label: spec.label.into(),
                                value: param_display(e.kind, &spec, value).into(),
                                fraction: (value - spec.min) / (spec.max - spec.min),
                                changed: value != spec.default,
                                bar: self.effect_bar(i, p),
                                on_press: Box::new(move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                                    this.press_effect(i, p, x, cx);
                                }),
                                on_reset: Some(Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
                                    this.set_effect_param(i, p, spec.default, cx);
                                })),
                                // Typed in the unit the value reads: a percent types 40 for 0.4.
                                typed: Some(SliderTyped {
                                    lo: f64::from(spec.min) * f64::from(scale),
                                    hi: f64::from(spec.max) * f64::from(scale),
                                    set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                                        this.set_effect_param(i, p, (v / f64::from(scale)) as f32, cx);
                                    }),
                                }),
                            },
                            cx,
                        ))
                        .into_any_element()
                }
                ParamShape::Switch => {
                    let set = e.values[p] >= 0.5;
                    div()
                        .id(("effects-switch", i * 8 + p))
                        .child(kit::switch_row(
                            "effects-switch-row",
                            spec.label,
                            None,
                            set,
                            None,
                            move |_, app| m.update(app, |this, cx| this.set_effect_param(i, p, if set { 0. } else { 1. }, cx)),
                        ))
                        .into_any_element()
                }
                ParamShape::Mode => {
                    let current = BlendMode::from_value(e.values[p]);
                    let at = BlendMode::ALL.iter().position(|&x| x == current).unwrap_or(0);
                    let open = on && self.effects_mode_open == Some(i);
                    let field = kit::dropdown_field("effects-mode", mode_text(current), on)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.effects_mode_open = if this.effects_mode_open == Some(i) { None } else { Some(i) };
                            cx.notify();
                        }));
                    let field = div()
                        .id(("effects-mode-field", i))
                        .w_full()
                        .child(field)
                        .when(!on, |d| d.tooltip(overlay_tip_hover("Switch this effect on to change its mode", "", None)));
                    let list = open.then(|| {
                        let t = crate::ui::theme::tokens();
                        let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.effects_mode_open = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        );
                        let mut plate = kit::plate(6.).id(("effects-mode-list", i)).w_full().max_h(px(240.)).overflow_y_scroll().p(px(crate::layout::MENU_PAD)).flex().flex_col().occlude();
                        for (k, &mode) in BlendMode::ALL.iter().enumerate() {
                            plate = plate.child(
                                div()
                                    .id(("effects-mode-row", i * 8 + k))
                                    .flex_none()
                                    .px(px(8.))
                                    .py(px(4.))
                                    .flex()
                                    .items_start()
                                    .gap(px(8.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                                    .child(div().flex_none().size(px(16.)).children((k == at).then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .child(div().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(mode_text(mode)))
                                            .child(dark_help(blend_why(mode))),
                                    )
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.effects_mode_open = None;
                                        this.set_effect_param(i, p, k as f32, cx);
                                    })),
                            );
                        }
                        gpui::deferred(
                            div()
                                .absolute()
                                .top(px(0.))
                                .left(px(0.))
                                .child(backdrop)
                                .child(div().absolute().top(px(crate::layout::redesign::CONTROL_M + 2.)).left(px(0.)).w(px(280.)).child(plate)),
                        )
                    });
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(dark_row_label(spec.label, false))
                        .child(dark_help(kind_hint(e.kind)))
                        .child(div().relative().w_full().child(field).children(list))
                        .into_any_element()
                }
            };
            // A mask's position and size rows carry the keyframe diamond: a key for the shape at the
            // playhead. It sits above the slider whether or not a key is there, so nothing moves.
            let mask_row = matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask) && (p == 0 || p == 2);
            if mask_row {
                let here = self.effects_src();
                let filled = here.is_some_and(|at| e.keys.iter().any(|k| k.at == at));
                let t = crate::ui::theme::tokens();
                let key = div()
                    .id(("effects-mask-key", i * 8 + p))
                    .flex_none()
                    .size(px(crate::layout::redesign::CONTROL_M))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(t.e3)))
                    .tooltip(overlay_tip_hover(
                        if filled { "Remove the mask key at the playhead" } else { "Key the mask at the playhead" },
                        "",
                        None,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_mask_key(i, cx)))
                    .child(super::keys::diamond(12., if filled { t.accent } else { t.ink3 }, filled));
                controls.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().w_full().flex().justify_end().child(key))
                        .child(control)
                        .into_any_element(),
                );
            } else {
                controls.push(control);
            }
        }
        // A mask follows the picture under it: the Track row sits under its controls.
        if matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask) {
            controls.push(self.track_row(i, cx));
        }
        let (m1, m2, m3, m4) = (me.clone(), me.clone(), me.clone(), me);
        div()
            .id(("effects-row", i))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(kit::switch(("effects-on", i), on, true, None).on_click(move |_, _, app| {
                        m1.update(app, |this, cx| this.toggle_effect(i, cx));
                    }))
                    .child(div().flex_1().min_w(px(0.)).child(dark_row_label(kind_name(e.kind), on)))
                    .child(kit::icon_ghost(
                        "effects-up",
                        Icon::ArrowUp,
                        "Move up",
                        (i == 0).then_some("Already first"),
                        move |_, app| m2.update(app, |this, cx| this.move_effect(i, false, cx)),
                    ))
                    .child(kit::icon_ghost(
                        "effects-down",
                        Icon::ArrowDown,
                        "Move down",
                        (i + 1 >= count).then_some("Already last"),
                        move |_, app| m3.update(app, |this, cx| this.move_effect(i, true, cx)),
                    ))
                    .child(kit::icon_ghost("effects-remove", Icon::X, "Remove effect", None, move |_, app| {
                        m4.update(app, |this, cx| this.remove_effect(i, cx));
                    })),
            )
            .children(controls)
            .into_any_element()
    }
}
