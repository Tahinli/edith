//! The mix card: one strip per audio track, the strips scroll, and the master
//! strip stays under them.

use super::common::*;
use crate::player::pan::{PanTarget, pan_text};
use crate::player::cards::{
    METER_FLOOR_DB, MixDrag, MixMeter, MixStrip, VoiceParam, VoiceProc, gain_fraction,
};
use crate::ui::widgets::{SliderTyped, centred_track, slider_row_tip, typed_value_pill};
use engine::voice::{Strength, VoiceFx};
use super::tools::slider_track;
use crate::ui::icons::Icon;
use crate::ui::kit::{self, Segment};
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// What the dynamics section has switched on, in plain words and kept short: one line
/// under the section, so it never wraps into the rows below.
/// A clip's pitch as the row reads it: `0 semitones`, `+3 semitones`, `-12 semitones`.
fn pitch_text(semitones: i32) -> String {
    match semitones {
        0 => "0 semitones".into(),
        n => format!("{n:+} semitones"),
    }
}

fn voice_summary(fx: &VoiceFx) -> String {
    let minus = |v: f32| format!("{v:.0}").replace('-', "\u{2212}");
    let mut parts = Vec::new();
    if fx.gate.on {
        parts.push(format!("Quiet-part cut {} dB", minus(fx.gate.threshold_db)));
    }
    if fx.nr.on {
        parts.push(match fx.nr.profile.is_empty() {
            true => "Hiss removal, no sample yet".to_string(),
            false => format!("Hiss removal {:.0}%", fx.nr.strength * 100.),
        });
    }
    if fx.comp.on {
        parts.push(format!("Loud-part limit {} dB", minus(fx.comp.threshold_db)));
    }
    if fx.deess.on {
        parts.push(format!("Harsh-S softening {} dB", minus(fx.deess.threshold_db)));
    }
    parts.join(" \u{b7} ")
}

/// An element id that is unique per strip. Element ids must be `&'static str`
/// here and a strip's id is built from its lane, so each one is made once and
/// kept for the session -- the same few names come back every frame.
pub(crate) fn interned_id(name: String) -> &'static str {
    static IDS: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut ids = IDS.lock().unwrap_or_else(|e| e.into_inner());
    match ids.iter().find(|id| *id == &name) {
        Some(id) => id,
        None => {
            let id: &'static str = Box::leak(name.into_boxed_str());
            ids.push(id);
            id
        }
    }
}

/// A fader's value as the strip writes it: `0 dB` at unity, else signed.
fn gain_text(db: f32) -> String {
    match db.abs() < 0.5 {
        true => "0 dB".into(),
        false => format!("{db:+.0} dB").replace('-', "\u{2212}"),
    }
}

/// A level meter: the bar to the level, and a marker at the held peak. Red-ish
/// near full scale so a hot track reads as one before it clips.
fn meter_bar(meter: MixMeter) -> impl IntoElement {
    let t = tokens();
    let level = slider_fraction(meter.level_db, METER_FLOOR_DB, 0.);
    let hold = slider_fraction(meter.hold_db, METER_FLOOR_DB, 0.);
    // The scale under the track: a tick at each mark's own place on the track, its label centred
    // under the tick. The 0 dB mark is the track's right end, so its label is set in from there.
    let mark = |db: f32| {
        let at = slider_fraction(db, METER_FLOOR_DB, 0.);
        let text = match db >= 0. {
            true => "0 dB".to_string(),
            false => format!("{db:.0}").replace('-', "\u{2212}"),
        };
        let tick = div()
            .absolute()
            .top_0()
            .h(px(3.))
            .w(px(1.))
            .left(relative(at))
            .bg(rgb(t.ink3));
        let label = div()
            .absolute()
            .top(px(4.))
            .w(px(32.))
            .whitespace_nowrap()
            .type_style(type_scale::caption())
            .text_color(rgb(t.ink3))
            .child(text);
        let label = match db >= 0. {
            true => label.right_0().text_right(),
            false => label.left(relative(at)).ml(px(-16.)).text_center(),
        };
        [tick.into_any_element(), label.into_any_element()]
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(2.))
        // Named, so the dB marks under it cannot be read as the pan's scale above.
        .child(div().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child("Level"))
        .child(
            div()
                .relative()
                .w_full()
                .h(px(6.))
                .rounded(px(2.))
                .bg(rgb(t.e3))
                .child(div().h_full().w(relative(level)).rounded(px(2.)).bg(rgb(
                    match meter.level_db >= -1. {
                        true => STATUS_WARNING(),
                        false => ACCENT_PRIMARY(),
                    },
                )))
                .when(meter.hold_db > METER_FLOOR_DB, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .h_full()
                            .w(px(2.))
                            .left(relative(hold))
                            .bg(rgb(INK1())),
                    )
                }),
        )
        .child(
            div()
                .relative()
                .w_full()
                .h(px(16.))
                .children(mark(-24.).into_iter().chain(mark(-12.)).chain(mark(0.))),
        )
}

/// One of a duck's three numbers as its row shows it: its range, its step and its start value.
#[derive(Clone, Copy)]
struct DuckNum {
    field: creator::DuckField,
    name: &'static str,
    unit: &'static str,
    lo: f32,
    hi: f32,
    step: f32,
    default: f32,
}

impl DuckNum {
    const ALL: [DuckNum; 3] = [
        DuckNum {
            field: creator::DuckField::Depth,
            name: "Amount",
            unit: "dB",
            lo: engine::duck::MIN_DEPTH_DB,
            hi: engine::duck::MAX_DEPTH_DB,
            step: creator::DUCK_DEPTH_STEP,
            default: creator::DUCK_DEPTH_DEFAULT,
        },
        DuckNum {
            field: creator::DuckField::Attack,
            name: "Attack",
            unit: "ms",
            lo: engine::duck::MIN_TIME_MS,
            hi: engine::duck::MAX_ATTACK_MS,
            step: creator::DUCK_ATTACK_STEP,
            default: engine::duck::Duck::DEFAULT_ATTACK_MS,
        },
        DuckNum {
            field: creator::DuckField::Release,
            name: "Release",
            unit: "ms",
            lo: engine::duck::MIN_TIME_MS,
            hi: engine::duck::MAX_RELEASE_MS,
            step: creator::DUCK_RELEASE_STEP,
            default: engine::duck::Duck::DEFAULT_RELEASE_MS,
        },
    ];

    fn get(self, duck: &engine::duck::Duck) -> f32 {
        match self.field {
            creator::DuckField::Depth => duck.depth_db,
            creator::DuckField::Attack => duck.attack_ms,
            creator::DuckField::Release => duck.release_ms,
        }
    }

    /// The value a fraction `f` (0..1) of the track stands for.
    fn at(self, f: f32) -> f32 {
        self.lo + f * (self.hi - self.lo)
    }
}

impl Player {
    /// Takes the voice processing off a track.
    fn clean_voice_off(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut()
            && session.set_lane_voice(lane, VoiceFx::default())
        {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The fold that holds the strips of the tracks that are not the selected clip's.
    const MIX_OTHERS_FOLD: &'static str = "mix-others";

    fn toggle_mix_others(&mut self, cx: &mut Context<Self>) {
        if !self.folds.remove(Self::MIX_OTHERS_FOLD) {
            self.folds.insert(Self::MIX_OTHERS_FOLD.to_string());
        }
        crate::ui::widgets::save_folds(&self.folds);
        cx.notify();
    }

    /// A pan row: the label and the value (with its reset slot) on a line, then a slider track
    /// with `L` and `R` at its ends. `target` `None` is a clip pan with no audio clip selected:
    /// the track rests dimmed, so the card never changes height with the selection.
    /// Sets a pan from a typed number: the same write a drag on the pan makes.
    fn type_pan(&mut self, target: PanTarget, pan: f32) {
        if let Some(s) = self.session.as_mut() {
            match target {
                PanTarget::Clip(lane, idx) => s.set_clip_pan(lane, idx, pan),
                PanTarget::Lane(lane) => s.set_lane_pan(lane, pan),
            };
        }
        self.mark_dirty();
    }

    /// The selected clip's own volume, a fader above the clip's pan. It stacks with the track's
    /// fader under it, as the clip's pan stacks with the track's pan.
    fn clip_volume_row(&self, lane: Lane, idx: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let db = self
            .session
            .as_ref()
            .and_then(|s| s.clip_gain_of(lane, idx))
            .unwrap_or(0.);
        let bar = self.mix_panel.borrow().clip_bar.clone();
        let (lo, hi) = (engine::project::MIN_CLIP_GAIN_DB, engine::project::MAX_CLIP_GAIN_DB);
        let row = SliderRow {
            id: "mix-clip-volume",
            label: "Clip volume".into(),
            value: gain_text(db).into(),
            fraction: gain_fraction(db, hi),
            changed: db.abs() >= 0.05,
            bar,
            on_press: Box::new(move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.press_mix(MixDrag::Clip(lane, idx), x, cx);
            }),
            on_reset: Some(Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
                this.reset_mix(MixDrag::Clip(lane, idx), cx);
            })),
            // Typed in dB, as the track's fader is.
            typed: Some(SliderTyped {
                lo: f64::from(lo),
                hi: f64::from(hi),
                set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    if this.set_clip_volume(lane, idx, v as f32) {
                        this.mark_dirty();
                    }
                    cx.notify();
                }),
            }),
        };
        div()
            .px(px(6.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(slider_row_tip(row, "This clip's own volume. The track's fader under it moves it too.", cx))
            // The fader's two ends, as the track's fader has them.
            .child(
                div()
                    .flex()
                    .justify_between()
                    .type_style(type_scale::caption())
                    .text_color(rgb(tokens().ink3))
                    .child("\u{2212}60 dB")
                    .child("+24 dB"),
            )
    }

    fn pan_row(&self, id: &'static str, label: &'static str, target: Option<PanTarget>, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = tokens();
        let pan = target.map_or(0., |t| self.pan_shown(t));
        // Nothing to pan yet: the same word a centred pan reads, not a bare dash.
        let value = target.map_or_else(|| pan_text(0.), |_| pan_text(pan));
        let bar = Rc::new(Cell::new(Bounds::default()));
        let probe = Rc::clone(&bar);
        let me = cx.entity();
        let changed = target.is_some() && pan.abs() > 0.005;
        // Typed in percent, negative toward the left, the way a pan reads ("-40" is 40% L).
        let typed = target.map(|t| SliderTyped {
            lo: -100.,
            hi: 100.,
            set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                this.type_pan(t, (v / 100.) as f32);
                cx.notify();
            }),
        });
        let open = target.map(|t| (((self.pan_shown(t) * 100.).round() as i32).to_string(), "%".to_string()));
        let pill = typed_value_pill(
            interned_id(format!("{id}-value")),
            &value,
            open,
            76.,
            changed,
            typed,
            {
                move |_, app| {
                    if let Some(t) = target {
                        me.update(app, |this, cx| this.reset_pan(t, cx));
                    }
                }
            },
            cx,
        );
        let track = centred_track(
            id,
            (pan + 1.) / 2.,
            bar,
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                if let Some(t) = target {
                    let b = probe.get();
                    this.begin_pan_press(t, f32::from(x), frac_along(x, b), f32::from(b.size.width));
                    cx.notify();
                }
            },
            cx,
        )
        .when(target.is_none(), |d| d.opacity(0.4).cursor_not_allowed());
        let end = |text: &'static str| div().flex_none().w(px(10.)).type_style(type_scale::caption()).text_color(rgb(t.ink3)).child(text);
        div()
            .id(interned_id(format!("{id}-row")))
            .w_full()
            .flex()
            .flex_col()
            .when(target.is_none(), |d| {
                d.tooltip(crate::ui::widgets::overlay_tip_hover(
                    label,
                    "",
                    Some("Select an audio clip to pan it"),
                ))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(KEYS_ROW_H))
                    .child(dark_row_label(label, false))
                    .child(pill),
            )
            .child(div().flex().items_center().gap(px(6.)).child(end("L")).child(div().flex_1().child(track)).child(end("R")))
    }

    /// The selected clip's pitch: a centred track from -12 to +12 semitones with its notch at 0,
    /// and the value typed in whole semitones. Pitch keeps the clip's speed. Dimmed with no audio
    /// clip picked, as the clip pan is, so the card never changes height with the selection.
    fn pitch_row(&self, target: Option<(Lane, usize)>, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = tokens();
        let semis = target
            .and_then(|(lane, idx)| self.session.as_ref()?.clip_pitch_of(lane, idx))
            .unwrap_or(0);
        let value = pitch_text(i32::from(semis));
        let changed = semis != 0;
        let bar = self.mix_panel.borrow().pitch_bar.clone();
        let me = cx.entity();
        let typed = target.map(|(lane, idx)| SliderTyped {
            lo: f64::from(engine::project::MIN_CLIP_PITCH),
            hi: f64::from(engine::project::MAX_CLIP_PITCH),
            set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                if this.set_clip_pitch_mix(lane, idx, v.round() as i32) {
                    this.mark_dirty();
                }
                cx.notify();
            }),
        });
        let open = target.map(|_| (semis.to_string(), " semitones".to_string()));
        let pill = typed_value_pill(
            "mix-clip-pitch-value",
            &value,
            open,
            112.,
            changed,
            typed,
            move |_, app| {
                if let Some((lane, idx)) = target {
                    me.update(app, |this, cx| {
                        if this.set_clip_pitch_mix(lane, idx, 0) {
                            this.mark_dirty();
                        }
                        cx.notify();
                    });
                }
            },
            cx,
        );
        let track = centred_track(
            "mix-clip-pitch",
            (f32::from(semis) + 12.) / 24.,
            bar,
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                if let Some((lane, idx)) = target {
                    this.press_mix(MixDrag::Pitch(lane, idx), x, cx);
                }
            },
            cx,
        )
        .when(target.is_none(), |d| d.opacity(0.4).cursor_not_allowed());
        let end = |text: &'static str| div().flex_none().w(px(24.)).whitespace_nowrap().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child(text);
        div()
            .id("mix-clip-pitch-row")
            .w_full()
            .flex()
            .flex_col()
            .px(px(6.))
            .when(target.is_none(), |d| {
                d.tooltip(crate::ui::widgets::overlay_tip_hover(
                    "Pitch",
                    "",
                    Some("Select an audio clip to change its pitch"),
                ))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(KEYS_ROW_H))
                    .child(dark_row_label("Pitch", false))
                    .child(pill),
            )
            .child(div().flex().items_center().gap(px(6.)).child(end("\u{2212}12")).child(div().flex_1().child(track)).child(end("+12")))
    }

    /// The mix card: one strip per audio track -- its Mute and Solo, its fader
    /// and its meter -- and the master limiter with the master meter under them.
    /// The two settings that belong to the *sound of the whole timeline* rather
    /// than to any clip on it.
    ///
    /// A track's fader moves everything on that track by the same amount,
    /// every frequency of it: it is not the equalizer (one take, one band) and
    /// it is not the volume in the panel, which is what this machine monitors
    /// at and is written to no file. Mute is saved with the project; Solo is
    /// for listening only, and the export never hears it. The limiter is over
    /// the sum of them all, which is where a mix can pass full scale and where
    /// a clamp used to square it off.
    ///
    /// The strips scroll rather than the card growing past the window -- a
    /// timeline may hold more tracks than a 360 px window has room for faders.
    /// The arrow keys still pick a row: the strips in order, then the ceiling
    /// and the limiter switch.
    pub(crate) fn mix_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.mix_open {
            return None;
        }
        let session = self.session.as_ref();
        let limiter = session.map_or_else(Limiter::default, PlaybackSession::limiter);
        let reading = self.mix_reading(Instant::now());
        // The selected clip's track first, with the other tracks folded under a row of their own:
        // a long timeline would otherwise bury the one being worked on.
        let current = self
            .selected
            .anchor()
            .map(|(lane, _)| lane)
            .and_then(|lane| reading.strips.iter().position(|s| s.lane == lane))
            .unwrap_or(0);
        let strip_els = |from: &mut dyn Iterator<Item = usize>, cx: &mut Context<Self>| -> Vec<AnyElement> {
            from.map(|n| self.mix_strip(n, &reading.strips[n], cx).into_any_element()).collect()
        };
        let strips = strip_els(&mut (current..=current).filter(|n| *n < reading.strips.len()), cx);
        let others_n = reading.strips.len().saturating_sub(1);
        let others_open = self.folds.contains(Self::MIX_OTHERS_FOLD);
        let others = (others_n > 0).then(|| {
            let rows = others_open.then(|| strip_els(&mut (0..reading.strips.len()).filter(|n| *n != current), cx));
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(disclosure_row(
                    Self::MIX_OTHERS_FOLD,
                    format!("Other tracks ({others_n})"),
                    others_open,
                    Self::toggle_mix_others,
                    cx,
                ))
                .children(rows.into_iter().flatten())
        });
        let lanes = reading.strips.len();
        let ceiling_changed = (limiter.ceiling_db - Limiter::default().ceiling_db).abs() >= 0.05;
        let ceiling = SliderRow {
            id: "mix-ceiling",
            label: "Loudest peak".into(),
            value: format!("{:+.0} dB", limiter.ceiling_db).into(),
            fraction: slider_fraction(limiter.ceiling_db, Limiter::MIN_DB, Limiter::MAX_DB),
            changed: ceiling_changed,
            bar: reading.ceiling_bar.clone(),
            on_press: Box::new(|this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.press_mix(MixDrag::Ceiling, x, cx);
            }),
            on_reset: Some(Rc::new(|this: &mut Player, cx: &mut Context<Player>| {
                this.reset_mix(MixDrag::Ceiling, cx);
            })),
            // Typed in dB, the way the value reads.
            typed: Some(SliderTyped {
                lo: f64::from(Limiter::MIN_DB),
                hi: f64::from(Limiter::MAX_DB),
                set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    if this.set_mix_ceiling(v as f32) {
                        this.mark_dirty();
                    }
                    cx.notify();
                }),
            }),
        };
        // The selected audio clip's own pan, above the strips' lane pans it stacks with.
        let clip_pan = self
            .selected
            .anchor()
            .filter(|&(lane, idx)| lane.kind == LaneKind::Audio && session.is_some_and(|s| idx < s.lane_clips(lane).len()))
            .map(|(lane, idx)| PanTarget::Clip(lane, idx));
        let clip_pan = self.pan_row("mix-clip-pan", "Clip pan", clip_pan, cx).px(px(6.));
        let clip_pitch = self
            .selected
            .anchor()
            .filter(|&(lane, idx)| lane.kind == LaneKind::Audio && session.is_some_and(|s| idx < s.lane_clips(lane).len()));
        let clip_pitch = self.pitch_row(clip_pitch, cx);
        let clip_volume = self
            .selected
            .anchor()
            .filter(|&(lane, idx)| lane.kind == LaneKind::Audio && session.is_some_and(|s| idx < s.lane_clips(lane).len()))
            .map(|(lane, idx)| self.clip_volume_row(lane, idx, cx).into_any_element());
        let master_picked = self.mix_field >= lanes;
        let limiter_pick = lanes + 1;
        let master = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .pt(px(6.))
            .border_t_1()
            .border_color(rgba(DARK_SEAM()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(KEYS_ROW_H))
                    .child(dark_row_label("Master", master_picked))
                    .child(div().w(px(120.)).flex().child(dark_ghost_button(
                        "mix-normalize",
                        "Normalize\u{2026}",
                        "",
                        false,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.open_tools(crate::player::tools::Tool::Normalize, cx);
                        }),
                    ))),
            )
            .child(kit::switch_row(
                "mix-limiter",
                "Limiter",
                Some("Keeps the whole mix under the ceiling".into()),
                limiter.on,
                None,
                {
                    let me = cx.entity();
                    move |_, app| {
                        me.update(app, |this, cx| {
                            this.mix_field = limiter_pick;
                            this.nudge_mix(1, cx);
                        })
                    }
                },
            ))
            .child(slider_row_tip(ceiling, "The highest level the mix may reach.", cx))
            .child(meter_bar(reading.master));
        let help_text = "Drag a fader or the ceiling, or use the up and down arrows to pick one and left and right to move it (hold to run it).";
        let tail_text = match limiter.on {
            true => format!(
                "The mix is held under {:+.0} dB. Quieter passages are untouched.",
                limiter.ceiling_db
            ),
            false => "The limiter is off: a hot mix clips at full scale.".to_string(),
        };
        Some(
            self.card_outer(viewport, cx)
                // A press on a slider starts its drag; the drag itself is read
                // here, on the whole card, so the pointer may leave the bar.
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if this.mix_drag.is_some() {
                        match event.pressed_button == Some(MouseButton::Left) {
                            true => this.drag_mix(event.position.x, cx),
                            false => this.release_mix(cx),
                        }
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_mix(cx)),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(
                            COLOR_W,
                            self.card_maximized,
                            f32::from(viewport.width),
                        )))
                        .on_mouse_down(MouseButton::Left, swallow)
                        // The slot's height, not a fixed 336 px: the strips are
                        // the card's content, and a duck row under each fader
                        // made the old cap show one and a half of them while
                        // the inspector had room to spare (the EQ card is
                        // capped the same way).
                        .max_h(relative(1.))
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
.map(|d| self.card_surface(d))
                        .map(|d| {
                            d.child(dark_card_head(
                                "Mix",
                                None,
                                Some(help_text.into()),
                                Some(self.card_maximized),
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id("mix-rows")
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .overflow_y_scroll()
                                .children(clip_volume)
                                .child(clip_pan)
                                .child(clip_pitch)
                                .children(strips)
                                .children(others),
                        )
                        .child(master)
                        .child(dark_help("Solo is for listening only. Export ignores it."))
                        .map(|d| d.child(dark_help(tail_text.clone()))),
                ),
        )
    }

    /// One audio track's strip: its name with Mute and Solo, its fader and its
    /// meter. `n` is the row the arrow keys pick it by.
    fn mix_strip(&self, n: usize, strip: &MixStrip, cx: &mut Context<Self>) -> impl IntoElement {
        let lane = strip.lane;
        let session = self.session.as_ref();
        let db = session.map_or(0., |s| s.lane_gain_db(lane));
        let muted = session.is_some_and(|s| s.lane_muted(lane));
        let soloed = self.mix_solo.contains(&lane);
        let picked = self.mix_field == n;
        let gain = SliderRow {
            id: interned_id(format!("mix-gain-{}", lane.label())),
            label: "Track gain".into(),
            value: gain_text(db).into(),
            fraction: gain_fraction(db, engine::project::MAX_GAIN_DB),
            changed: db.abs() >= 0.05,
            bar: strip.bar.clone(),
            on_press: Box::new(
                move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                    this.press_mix(MixDrag::Gain(lane), x, cx);
                },
            ),
            on_reset: Some(Rc::new(
                move |this: &mut Player, cx: &mut Context<Player>| {
                    this.reset_mix(MixDrag::Gain(lane), cx);
                },
            )),
            // Typed in dB: "-6" is six dB down.
            typed: Some(SliderTyped {
                lo: f64::from(engine::project::MIN_GAIN_DB),
                hi: f64::from(engine::project::MAX_GAIN_DB),
                set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    if this.set_mix_gain(lane, v as f32) {
                        this.mark_dirty();
                    }
                    cx.notify();
                }),
            }),
        };
        let head = div()
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(KEYS_ROW_H))
            .child(dark_row_label(format!("Track {}", lane.label()), picked))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div().w(px(120.)).flex().gap(px(4.)).children([
                            dark_toggle_button(
                                ("mix-mute", n),
                                "Mute",
                                muted,
                                tokens().accent,
                                "Silence this track. Saved with the project.",
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.mix_field = n;
                                    this.toggle_mix_mute(lane, cx);
                                }),
                            )
                            .into_any_element(),
                            dark_toggle_button(
                                ("mix-solo", n),
                                "Solo",
                                soloed,
                                STATUS_WARNING(),
                                "Hear only this track while you listen. Export ignores it.",
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.mix_field = n;
                                    this.toggle_mix_solo(lane, cx);
                                }),
                            )
                            .into_any_element(),
                        ]),
                    )
            );
        div()
            .id(("mix-row", n))
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(6.))
            .py(px(4.))
            .rounded(px(3.))
            .when(picked, |d| d.bg(rgb(DARK_RAISED())))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.mix_field = n;
                cx.notify();
            }))
            .child(head)
            .child(slider_row(gain, cx))
            // The fader's two ends, read as the pan's L and R are.
            .child(
                div()
                    .flex()
                    .justify_between()
                    .type_style(type_scale::caption())
                    .text_color(rgb(tokens().ink3))
                    .child("\u{2212}60 dB")
                    .child("+12 dB"),
            )
            .child(self.pan_row(
                interned_id(format!("mix-pan-{}", lane.label())),
                "Track pan",
                Some(PanTarget::Lane(lane)),
                cx,
            ))
            .child(meter_bar(strip.meter))
            .child(self.voice_rows(n, lane, cx))
            .child(self.duck_rows(n, lane, cx))
    }

    /// A strip's voice processing: the one-click **Clean voice** with its
    /// strength choice, and the dynamics section that unfolds under it -- a
    /// bypass switch and sliders for each of the gate, noise reduction,
    /// compressor and de-esser ([`engine::voice`]).
    fn voice_rows(&self, n: usize, lane: Lane, cx: &mut Context<Self>) -> impl IntoElement {
        let fx = self
            .session
            .as_ref()
            .map_or_else(VoiceFx::default, |s| s.lane_voice(lane));
        let open = self.mix_voice_open(lane);
        let strengths = [Strength::Light, Strength::Medium, Strength::Strong];
        // Which strength is in force, read off the compressor (each preset's own): none while the
        // settings were moved by hand.
        let active = match fx.is_active() {
            false => Some(0),
            true => strengths
                .iter()
                .position(|s| VoiceFx::clean_voice(*s, Vec::new(), None).comp == fx.comp)
                .map(|i| i + 1),
        };
        let clean = div()
            .id(interned_id(format!("mix-clean-row-{n}")))
            .flex()
            .flex_col()
            .gap(px(2.))
            .min_h(px(KEYS_ROW_H))
            .tooltip(crate::ui::widgets::overlay_tip_hover(
                "Clean voice",
                "",
                Some("Removes hiss and hum from a voice in one step."),
            ))
            .child(dark_row_label("Clean voice", false))
            .child(kit::segmented_equal(
                interned_id(format!("mix-clean-{n}")),
                &["Off", "Light", "Medium", "Strong"].map(Segment::text),
                active.unwrap_or(usize::MAX),
                true,
                None,
                {
                    let me = cx.entity();
                    move |i, _, app| {
                        me.update(app, |this, cx| {
                            this.mix_field = n;
                            match i {
                                0 => this.clean_voice_off(lane, cx),
                                i => this.clean_mix_voice(lane, strengths[i - 1], cx),
                            }
                        })
                    }
                },
            ))
            .child(dark_help(format!("Applies to all of track {}.", lane.label())));
        let fold = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(KEYS_ROW_H))
                    .child(
                        div()
                            .id(interned_id(format!("mix-dynamics-row-{n}")))
                            .tooltip(crate::ui::widgets::overlay_tip_hover(
                                "Dynamics",
                                "",
                                Some("Gate, noise cut, compressor and de-esser for a voice."),
                            ))
                            .child(dark_row_label("Dynamics", false)),
                    )
                    .child(div().w(px(120.)).flex().child(dark_ghost_button(
                        ("mix-dynamics", n),
                        match open {
                            true => "Hide dynamics",
                            false => "Show dynamics",
                        },
                        "",
                        open,
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.mix_field = n;
                            this.toggle_voice_section(lane, cx);
                        }),
                    ))),
            )
            // The summary line is always there, empty while nothing is on, so the rows below
            // never move when a process is switched on or off. One line, cut with an ellipsis
            // rather than wrapped, so it cannot push anything down.
            .child(
                dark_help(match fx.is_active() {
                    true => voice_summary(&fx),
                    false => String::new(),
                })
                .h(px(16.))
                .truncate(),
            );
        let sections: Vec<AnyElement> = match open {
            false => Vec::new(),
            true => VoiceProc::ALL
                .into_iter()
                .enumerate()
                .map(|(k, which)| {
                    let on = which.is_on(&fx);
                    // Noise reduction learns its profile from the lane when it is switched on, so
                    // until it has one the row says what that switch will do.
                    let learn = (matches!(which, VoiceProc::Nr) && fx.nr.profile.is_empty())
                        .then_some("Learn the noise from a quiet moment first");
                    let mut col = div().flex().flex_col().gap(px(2.)).child({
                        let me = cx.entity();
                        kit::switch_row(
                            interned_id(format!("mix-voice-bypass-{n}-{k}")),
                            which.label(),
                            learn.map(SharedString::from),
                            on,
                            None,
                            move |_, app| {
                                me.update(app, |this, cx| {
                                    this.mix_field = n;
                                    this.toggle_voice_proc(lane, which, cx);
                                })
                            },
                        )
                    });
                    for &param in which.params() {
                        let v = param.get(&fx);
                        let (lo, hi, _) = param.range();
                        let default = param.get(&VoiceFx::default());
                        // The two strength rows read as percents, so they type in percents too.
                        let pct = if matches!(param, VoiceParam::NrStrength | VoiceParam::DeessAmount) { 100. } else { 1. };
                        col = col.child(slider_row(
                            SliderRow {
                                id: interned_id(format!("mix-voice-{}-{:?}", lane.label(), param)),
                                label: param.label().into(),
                                value: param.text(v).into(),
                                fraction: slider_fraction(v, lo, hi),
                                changed: (v - default).abs() > 1e-3,
                                bar: self.mix_voice_bar(lane, param),
                                on_press: Box::new(
                                    move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                                        this.press_mix(MixDrag::Voice(lane, param), x, cx);
                                    },
                                ),
                                on_reset: Some(Rc::new(
                                    move |this: &mut Player, cx: &mut Context<Player>| {
                                        this.reset_mix(MixDrag::Voice(lane, param), cx);
                                    },
                                )),
                                // Typed in the unit the value reads (a percent types 40 for 0.4).
                                typed: Some(SliderTyped {
                                    lo: f64::from(lo) * pct,
                                    hi: f64::from(hi) * pct,
                                    set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                                        if this.set_voice_param(lane, param, (v / pct) as f32) {
                                            this.mark_dirty();
                                        }
                                        cx.notify();
                                    }),
                                }),
                            },
                            cx,
                        ));
                    }
                    col.into_any_element()
                })
                .collect(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pt(px(2.))
            .child(clean)
            .child(fold)
            .children(sections)
    }

    /// A strip's ducking: "Lower when these play" with one chip per other audio track (the voice
    /// to duck under), and for each duck a row per number -- amount, attack, release -- in the
    /// settings grid: the name at the left, a slider and its value with unit at the right.
    fn duck_rows(&self, n: usize, lane: Lane, cx: &mut Context<Self>) -> Div {
        let keys: Vec<Lane> = self.mix_lanes().into_iter().filter(|l| *l != lane).collect();
        let ducks = self.session.as_ref().map_or_else(Vec::new, |s| s.lane_duck_list(lane));
        if keys.is_empty() {
            return div();
        }
        let chips: Vec<_> = keys
            .iter()
            .map(|&key| {
                let on = ducks.iter().any(|d| d.under == key.ord);
                let mut b = kit::Button::new(interned_id(format!("{}-{}-{}", "mix-duck-key", lane.label(), key.label())))
                    .label(key.label())
                    .style(if on { kit::ButtonStyle::Ghost } else { kit::ButtonStyle::Secondary })
                    .on(on);
                if on {
                    b = b.icon(Icon::Check);
                }
                let b = b
                    .build()
                    .tooltip(crate::ui::widgets::overlay_tip_hover(
                        &format!("{} {}", if on { "Stop ducking under" } else { "Duck under" }, key.label()),
                        "",
                        None,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_duck(lane, key, cx)));
                b.into_any_element()
            })
            .collect();
        let mut rows = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(dark_row_label("Lower when these play", false))
                    .child(div().flex().flex_wrap().gap(px(4.)).children(chips)),
            );
        for duck in &ducks {
            let Some(&key) = keys.iter().find(|k| k.ord == duck.under) else {
                continue;
            };
            let mut block = div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(6.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .min_h(px(KEYS_ROW_H))
                        .child(dark_row_label(format!("Under {}", key.label()), false))
                        .child(div().w(px(48.)).flex().child(dark_ghost_button(
                            ("mix-duck-off", n * 16 + key.ord % 16),
                            "Off",
                            "",
                            false,
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_duck(lane, key, cx)),
                        ))),
                );
            for num in DuckNum::ALL {
                block = block.child(self.duck_slider(duck, num, lane, key, cx));
            }
            rows = rows.child(block);
        }
        rows
    }

    /// One duck number's row: its name, a slider over its range and the value with its unit
    /// (and the reset slot). A press or a drag on the track sets it, a whole step at a time.
    fn duck_slider(
        &self,
        duck: &engine::duck::Duck,
        num: DuckNum,
        lane: Lane,
        key: Lane,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let v = num.get(duck);
        let bar = Rc::new(Cell::new(Bounds::default()));
        let (probe, drag_probe) = (Rc::clone(&bar), Rc::clone(&bar));
        let id = format!("mix-duck-{}-{}-{}", lane.label(), key.label(), num.name);
        let track = slider_track(
            interned_id(id.clone()),
            slider_fraction(v, num.lo, num.hi),
            bar,
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.duck_to(lane, key, num, num.at(frac_along(x, probe.get())), cx);
            },
            cx,
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if event.pressed_button == Some(MouseButton::Left) {
                this.duck_to(lane, key, num, num.at(frac_along(event.position.x, drag_probe.get())), cx);
            }
        }));
        let me = cx.entity();
        let reset = move |_: &mut Window, app: &mut App| {
            me.update(app, |this, cx| this.duck_to(lane, key, num, num.default, cx));
        };
        let value = format!("{v:.0}").replace('-', "\u{2212}");
        // Typed in the number's own unit: a dB amount or a time in ms.
        let typed = SliderTyped {
            lo: f64::from(num.lo),
            hi: f64::from(num.hi),
            set: Rc::new(move |this: &mut Player, v: f64, cx: &mut Context<Player>| {
                this.duck_to(lane, key, num, v as f32, cx);
            }),
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(KEYS_ROW_H))
            .child(div().flex_none().w(px(76.)).child(dark_row_label(num.name, false)))
            .child(div().flex_1().min_w(px(0.)).child(track))
            .child(typed_value_pill(
                interned_id(format!("{id}-value")),
                &format!("{value} {}", num.unit),
                None,
                64.,
                (v - num.default).abs() >= 0.5,
                Some(typed),
                reset,
                cx,
            ))
    }

    /// Sets one of a duck's numbers to `target`, moved by whole steps from where it stands now
    /// (read off the session, so a drag that comes faster than a frame still lands on the pointer).
    fn duck_to(&mut self, lane: Lane, key: Lane, num: DuckNum, target: f32, cx: &mut Context<Self>) {
        let now = self
            .session
            .as_ref()
            .and_then(|s| s.lane_duck_list(lane).into_iter().find(|d| d.under == key.ord));
        let Some(now) = now else {
            return;
        };
        let steps = ((target - num.get(&now)) / num.step).round() as i32;
        match steps {
            0 => cx.notify(),
            steps => self.step_duck(lane, key, num.field, steps, cx),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dynamics_summary_reads_in_plain_words_and_stays_short() {
        let mut fx = VoiceFx::default();
        assert_eq!(voice_summary(&fx), "");
        fx.gate.on = true;
        fx.gate.threshold_db = -40.;
        fx.comp.on = true;
        fx.comp.threshold_db = -20.;
        assert_eq!(voice_summary(&fx), "Quiet-part cut \u{2212}40 dB \u{b7} Loud-part limit \u{2212}20 dB");
    }

    #[test]
    fn a_clip_volume_drag_lands_where_the_knee_shows_it() {
        // The clip's fader is drawn on the knee and dragged on the same curve: a place on the
        // bar reads back as the value it was dragged to, for the clip's own range.
        let hi = engine::project::MAX_CLIP_GAIN_DB;
        for db in [-60., -24., -6., 0., 6., 24.] {
            let back = crate::player::cards::gain_at(gain_fraction(db, hi), hi);
            assert!((back - db).abs() < 0.01, "{db} dB came back as {back}");
        }
    }
}
