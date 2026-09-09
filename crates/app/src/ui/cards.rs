//! The property cards -- docked into the inspector, or floated when modal.

use crate::ui::hitmap;
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;

/// DESIGN §3/§4 atoms shared by every param/setting card below (colour,
/// transform, speed, EQ, silence, mix, subtitle style): what a row's label
/// and value paint in the darkroom, so the seven card bodies each pick
/// tokens rather than reimplement the same font/colour swap seven times.
///
/// A row label is what the room says about the control -- Archivo, §3's
/// label-row size, `ink2` at rest / `ink1` picked (no permanent pill: the
/// picked state is a 1px `ink1` rule, §4's focus ring, not a fill).
fn dark_row_label(text: impl Into<SharedString>, picked: bool) -> Div {
    div()
        .type_style(type_scale::label(
            type_scale::LABEL_ROW_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(if picked { INK1() } else { INK2() }))
        .child(text.into())
}

/// A transform row's value with its unit: degrees for rotation, `×` for
/// scale (a multiplier, not a fraction of the frame), percent for
/// position/crop (both are a fraction of the frame's own size) -- the bare
/// `{value:.2}` this used to print could not tell a percent from a
/// multiplier apart.
fn transform_row_value(band: usize, value: f32) -> String {
    if band == ROTATE_BAND {
        format!("{value:.0}°")
    } else if band == SCALE_BAND {
        format!("{value:.2}×")
    } else {
        format!("{:.0}%", value * 100.)
    }
}

/// A row's value -- what the film/the setting *says*, mono per §3.
fn dark_row_value(text: impl Into<SharedString>) -> Div {
    div()
        .type_style(type_scale::mono(
            type_scale::LABEL_ROW_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK1()))
        .child(text.into())
}

/// A card's own head line: the verb in Archivo (§3 section-head casing) and,
/// where there is one, the clip it names in mono beside it -- "which clip"
/// is metadata about the footage, not the room's own voice. `help`, when
/// given, is the card's own how-to sentence -- it used to sit under the
/// head as a permanent line (a terminal-screen row of prose the user named
/// directly); now it rides a `?` glyph beside the head and only shows on
/// hover, the same hover-only convention [`dock_stance::ghost_verb`]
/// already uses for a verb's own description.
/// `maximize` is `None` for the one card this head builds that is not one of
/// the seven param cards (the export progress sheet): everything else passes
/// `Some(self.card_maximized)` and gets the affordance this session's
/// complaint asked for -- worn on the head (DESIGN.md:91's "chord is worn"),
/// so it never was a keyboard-only option (the repeated "some options are
/// only reachable via keyboard shortcut" complaint). A double-click anywhere
/// on the head does the same thing the glyph's click and the `m` chord do
/// ([`Player::toggle_maximize`]) -- mouse and key reach the same switch.
// `+ use<>` (edition 2024 precise capturing): without it, the elided
// lifetime on `cx` would be captured into the returned opaque type by
// default, so a hoisted `let head = true.then(|| dark_card_head(..., cx));`
// would keep `cx` borrowed until `head` is finally consumed -- fighting
// every other `cx.listener(...)` call built in between, which is exactly
// the E0500/E0501 chain this card's own hoist is here to avoid.
fn dark_card_head(
    verb: &str,
    meta: Option<SharedString>,
    help: Option<SharedString>,
    maximize: Option<bool>,
    cx: &mut Context<Player>,
) -> impl IntoElement + use<> {
    div()
        .id("card-head")
        .flex_none()
        .px(px(6.))
        .flex()
        .items_baseline()
        .gap(px(6.))
        .when(maximize.is_some(), |d| {
            d.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    if event.click_count >= 2 {
                        this.toggle_maximize();
                        cx.notify();
                    }
                }),
            )
        })
        .children(maximize.map(|max| {
            div()
                .id("card-head-maximize")
                .flex_none()
                .cursor_pointer()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MAX_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.toggle_maximize();
                    cx.notify();
                }))
                .tooltip(move |_, cx| {
                    cx.new(|_| {
                        OverlayTip(if max {
                            "m -- back to the room".into()
                        } else {
                            "m -- fills the room".into()
                        })
                    })
                    .into()
                })
                .child(if max { "▣ m" } else { "⤢ m" })
                .tooltip(crate::ui::widgets::overlay_tip_hover("Toggle card size", "", None))
                .children(hitmap::control("card.maximize", "Toggle card size", true))
        }))
        .child(
            div()
                .type_style(type_scale::head())
                .text_color(rgb(INK3()))
                .child(verb.to_uppercase()),
        )
        .children(meta.map(|m| {
            div()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MAX_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .child(m)
        }))
        .children(help.map(|h| {
            div()
                .id("card-head-help")
                .flex_none()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MAX_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .tooltip(move |_, cx| cx.new(|_| OverlayTip(h.clone())).into())
                .child("?")
        }))
}

/// The help/status line under a card's head: what the keys do, which is
/// metadata about the room's controls -- mono, §3.
fn dark_help(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .type_style(type_scale::mono(
            type_scale::LABEL_ROW_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK3()))
        .child(text.into())
}

/// A ghost action row (DESIGN §4): borderless label + its chord, never a
/// filled box -- the shape every card's Reset/Flatten/Add/Remove/toggle
/// button takes now that "boxes are commitments" and Export is the one box
/// in the room. `active` is a toggle's own held-on state (EQ's spectrum
/// switch), not hover -- it keeps the fill and brightens the ink the same
/// way a picked param row's ring does.
/// The mix/silence/subtitle-size shape's own nudge glyph: ghost, not the
/// filled pill it used to be a permanent box -- resting bare, one fill step
/// on hover/press, same grammar as [`dark_ghost_button`] at a stepper's size.
fn dark_step_glyph(
    id: impl Into<gpui::ElementId>,
    plus: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let hitmap = hitmap::enabled().then(|| {
        let id = id.to_string();
        hitmap::dynamic(
            move || {
                (
                    format!("card.{id}"),
                    if plus { "Increase" } else { "Decrease" }.into(),
                )
            },
            true,
        )
    });
    div()
        .id(id)
        .flex_none()
        .w(px(HIT_MIN))
        .h(px(KEYS_ROW_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .on_click(on_click)
        .tooltip(crate::ui::widgets::overlay_tip_hover(
            if plus { "Step up" } else { "Step down" },
            "",
            None,
        ))
        .children(hitmap.flatten())
        .child(dark_row_value(if plus { "+" } else { "−" }))
}

/// One segment of the export moment's plan line (row 3): the thing it says
/// *is* the button that changes it, so `H.264 \u{b7} MP4` is pressed to walk the
/// files and wears `c` beside it exactly as the Settings row does (user
/// 2026-09-09: "couldn't find how to change encode options, simplifying
/// doesn't mean getting rid of advanced settings"). A ghost (DESIGN \u{a7}4) at a
/// readout's size: `ink3` at rest so the line stays as quiet as the one it
/// replaced, `ink2` and one fill step on hover, the chord in `ink4` beside it.
fn moment_segment(
    id: &'static str,
    text: impl Into<SharedString>,
    chord: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let text: SharedString = text.into();
    let hitmap = hitmap::enabled().then(|| {
        let label = text.clone();
        hitmap::dynamic(move || (format!("card.{id}"), label.to_string()), true)
    });
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_baseline()
        .gap(px(5.))
        .px(px(4.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())).text_color(rgb(INK2())))
        .on_click(on_click)
        .children(hitmap.flatten())
        .type_style(type_scale::mono(
            type_scale::CHORD_METADATA_MIN_PX,
            gpui::FontWeight::MEDIUM,
        ))
        .text_color(rgb(INK3()))
        .child(text)
        .child(div().flex_none().text_color(rgb(INK4())).child(chord.into()))
}

fn dark_ghost_button(
    id: impl Into<gpui::ElementId>,
    text: impl Into<SharedString>,
    chord: &str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.into();
    let text: SharedString = text.into();
    let hitmap = hitmap::enabled().then(|| {
        let id = id.to_string();
        let label = text.clone();
        hitmap::dynamic(move || (format!("card.{id}"), label.to_string()), true)
    });
    div()
        .id(id)
        .flex_1()
        .flex()
        .h(px(CONTROL_H))
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(3.))
        .when(active, |d| d.bg(rgb(DARK_RAISED())))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(DARK_RAISED())))
        .on_click(on_click)
        .tooltip(crate::ui::widgets::overlay_tip_hover(&text, chord, None))
        .children(hitmap.flatten())
        .child(
            div()
                .type_style(type_scale::label(
                    type_scale::LABEL_ROW_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(if active { INK1() } else { INK2() }))
                .child(text),
        )
        .when(!chord.is_empty(), |d| {
            d.child(
                div()
                    .type_style(type_scale::mono(
                        type_scale::CHORD_METADATA_MIN_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK3()))
                    .child(chord.to_string()),
            )
        })
}

/// Where an export lands, said the way the destination row says it: the
/// directory under `~` where it is one, so the row is a place a person
/// recognises rather than an absolute path that will not fit on the line.
fn dest_dir(path: &Path) -> String {
    let dir = path.parent().unwrap_or(path).to_string_lossy().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && dir.starts_with(&home) => {
            format!("~{}", &dir[home.len()..])
        }
        _ => dir,
    }
}

impl Player {
    /// The export moment (DESIGN §4/§8/§10, the approved artboard): three
    /// lines, ~200px, no scroll, no sections and no prose. Where it lands
    /// (`d` opens the save dialog), what it costs (one lever, read as a rate
    /// and as a size), and what it resolved to on its own beside the one
    /// boxed chip.
    ///
    /// Everything the old card asked apart -- codec, container, sound rate,
    /// encoder seat, the presets, the Advanced pane -- keeps its state and
    /// its setters and is simply not on this surface: an export is one
    /// decision, the budget, and the rest is a preference that belongs in
    /// Settings (user 2026-09-09: "too complicated plus I can only give mbps
    /// level bitrate").
    pub(crate) fn export_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.export_open {
            return None;
        }
        let budget = self.budget_bps();
        let seconds = self.export_seconds();
        // Both halves of the file, which is what a person means by "how big":
        // the picture's budget and the sound's own rate over the same span.
        let bytes = estimated_bytes(budget + self.audio_bps(), seconds);
        let fill = ((budget.saturating_sub(BPS_MIN)) as f32 / (BPS_MAX - BPS_MIN) as f32).clamp(0., 1.);
        let name = file_name(&self.export_path);
        let (stem, ext) = match name.rfind('.') {
            Some(at) => name.split_at(at),
            None => (name.as_str(), ""),
        };
        // The picture's own seat, as the probe found it -- `SW` until it says
        // otherwise is a promise; `GPU` is what `planned_seats` measured.
        let hardware = self
            .export_seat
            .as_ref()
            .and_then(|(.., seats)| *seats)
            .and_then(|(video, _)| video)
            .is_some_and(|seat| seat.contains("VA-API") || seat.contains("GPU"));
        let audio = self
            .session
            .as_ref()
            .map_or("", |s| s.planned_audio(self.format, self.range.is_some()));
        let marks = self.range.map(|(start, end)| {
            format!(
                "{}\u{2013}{}",
                timecode(f64::from(start) / self.fps, self.fps),
                timecode(f64::from(end) / self.fps, self.fps),
            )
        });
        // Row 3's segments are the Settings room's own EXPORT rows worn as
        // ghosts on the line that used to only *report* them -- the same
        // setters, so both surfaces can never disagree.
        // What the engine says the sound *will* be ("AAC 256 kbps", or "AAC
        // copy" where the source's own stream is carried through): a rate
        // nobody is choosing wears no chord, the Settings row's own rule for
        // a codec with no rate to pick.
        let (codec, rated) = crate::ui::settings_stance::sound_codec(self.format);
        let sound_label = match audio.is_empty() {
            true => codec.to_string(),
            false => audio.to_string(),
        };
        let sound_keyed = rated && !audio.contains("copy");
        // `auto` is a promise until the probe answers; once it has, the seat
        // says what it resolved to rather than the word nobody picked.
        let seat = self.encoder_seat();
        let seat_label = match seat {
            EncoderSeat::Auto => format!(
                "auto \u{2192} {}",
                match hardware {
                    true => "GPU",
                    false => "SW",
                }
            ),
            _ => crate::ui::settings_stance::encoder_word(seat).to_string(),
        };
        let range = range_word(marks.as_deref(), self.format.has_video());
        // The refusal the button used to carry, said before it is pressed and
        // in the readout's own place: a few words and the chord that fixes it,
        // never the sentence the old card wrapped over three lines.
        let blocked = self
            .session
            .as_ref()
            .and_then(|s| format_refusal(s, self.format))
            .map(|why| {
                format!(
                    "{} — sound format in settings {}",
                    why.split(" — ").next().unwrap_or("cannot export"),
                    self.keymap.chord(ActionId::Settings)
                )
            });
        let chord = |key: &'static str| {
            div()
                .flex_none()
                .type_style(type_scale::mono(
                    type_scale::CHORD_METADATA_MIN_PX,
                    gpui::FontWeight::MEDIUM,
                ))
                .text_color(rgb(INK3()))
                .child(key)
        };
        // Row 1: the file. A readout -- `d` is the one way to change it, the
        // desktop's own save dialog, exactly as the old row's `d` was.
        let file_row = div()
            .id("destination")
            .flex_none()
            .flex()
            .items_baseline()
            .gap(px(16.))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_destination(cx)))
            .child(
                // One line, always: the stem gives way with an ellipsis and
                // the extension rides beside it. A nested block child would
                // wrap it under the name instead, which is what the narrow
                // window showed.
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_baseline()
                    .type_style(type_scale::mono(18., gpui::FontWeight::BOLD))
                    .text_color(rgb(INK1()))
                    .child(div().min_w(px(0.)).truncate().child(stem.to_string()))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(INK2()))
                            .child(ext.to_string()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(10.))
                    .type_style(type_scale::mono(
                        type_scale::CHORD_METADATA_MIN_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK3()))
                    .child(dest_dir(&self.export_path))
                    .child("d"),
            );
        // Row 2: the budget. One lever, in bits per second, read as a rate and
        // as a size -- the wheel moves it (shift by whole Mbps), `n` types one
        // (`850k`, `6.5M`, `6.5`, `1.2G` of file), the track sets it by hand.
        let reading: Vec<AnyElement> = match &self.budget_edit {
            Some(text) => {
                let parsed = parse_budget(text, seconds, self.audio_bps());
                vec![
                    div()
                        .type_style(type_scale::mono(18., gpui::FontWeight::BOLD))
                        .text_color(rgb(INK1()))
                        .child(format!("{text}\u{258f}"))
                        .into_any_element(),
                    div()
                        .type_style(type_scale::mono(15., gpui::FontWeight::MEDIUM))
                        .text_color(rgb(INK2()))
                        .child(match parsed {
                            Some(bps) => format!(
                                "{} Mbps ≈ {}",
                                rate_label(bps),
                                size_label(estimated_bytes(bps + self.audio_bps(), seconds))
                            ),
                            None => "↵ takes it · esc leaves it".to_string(),
                        })
                        .into_any_element(),
                ]
            }
            None => vec![
                div()
                    .flex()
                    .items_baseline()
                    .type_style(type_scale::mono(18., gpui::FontWeight::BOLD))
                    .text_color(rgb(INK1()))
                    .child(rate_label(budget))
                    .child(
                        div()
                            .type_style(type_scale::mono(
                                type_scale::CHORD_METADATA_MIN_PX,
                                gpui::FontWeight::MEDIUM,
                            ))
                            .text_color(rgb(INK2()))
                            .child(" Mbps"),
                    )
                    .into_any_element(),
                div()
                    .type_style(type_scale::mono(15., gpui::FontWeight::MEDIUM))
                    .text_color(rgb(INK2()))
                    .child(format!("≈ {}", size_label(bytes)))
                    .into_any_element(),
            ],
        };
        let budget_row = div()
            .id("budget")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.wheel_budget(event);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(16.))
                    .id("budget-read")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.edit_budget();
                        cx.notify();
                    }))
                    .children(reading)
                    .child(div().flex_1())
                    .child(chord("n")),
            )
            .child(
                // 1px to look at, a whole row to grab (WCAG 2.5.8) -- the
                // ruler's own split between what is drawn and what is hit.
                div()
                    .id("budget-track")
                    .relative()
                    .h(px(KEYS_ROW_H))
                    .cursor_pointer()
                    .child(bounds_probe(self.budget_bar.clone()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.drag_budget(event.position.x);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            this.drag_budget(event.position.x);
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(KEYS_ROW_H / 2.))
                            .h(px(1.))
                            .bg(rgb(DARK_HAIRLINE())),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(KEYS_ROW_H / 2.))
                            .h(px(1.))
                            .w(relative(fill))
                            .bg(rgb(INK2())),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(fill))
                            .top(px(KEYS_ROW_H / 2. - 4.))
                            .w(px(2.))
                            .h(px(9.))
                            .bg(rgb(INK1())),
                    ),
            );
        // Row 3: what auto resolved to, in the faintest ink -- or, where the
        // press would be refused, the refusal in its place -- and the one
        // boxed chip in the room (§4).
        let commit_row = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .children(self.format.has_video().then(|| {
                        moment_segment(
                            "moment-picture",
                            crate::ui::settings_stance::picture_label(self.format),
                            "c",
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cycle_export_picture();
                                cx.notify();
                            }),
                        )
                    }))
                    .children(sound_keyed.then(|| {
                        moment_segment(
                            "moment-sound",
                            sound_label.clone(),
                            "b",
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cycle_audio_kbps();
                                cx.notify();
                            }),
                        )
                    }))
                    // A codec with no rate to pick is a readout, not a button
                    // that would do nothing when pressed -- the Settings row's
                    // own rule.
                    .children((!sound_keyed).then(|| {
                        div()
                            .flex_none()
                            .type_style(type_scale::mono(
                                type_scale::CHORD_METADATA_MIN_PX,
                                gpui::FontWeight::MEDIUM,
                            ))
                            .text_color(rgb(INK4()))
                            .child(sound_label.clone())
                    }))
                    .children(self.format.has_video().then(|| {
                        moment_segment(
                            "moment-encoder",
                            seat_label.clone(),
                            "g",
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cycle_encoder(cx);
                            }),
                        )
                    }))
                    // The range is set on the timeline with the marks
                    // themselves, so here it is what the file will hold --
                    // or, where the press would be refused, the refusal in
                    // its place (DESIGN \u{a7}8: said before it is pressed).
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .type_style(type_scale::mono(
                                type_scale::CHORD_METADATA_MIN_PX,
                                gpui::FontWeight::MEDIUM,
                            ))
                            .text_color(rgb(match blocked.is_some() {
                                true => STATUS_WARNING(),
                                false => INK4(),
                            }))
                            .child(blocked.clone().unwrap_or(range)),
                    ),
            )
            // The door to everything else this file could be written as: the
            // room's own settings chord, on the moment that needed it.
            .child(moment_segment(
                "moment-settings",
                "settings",
                self.keymap.chord(ActionId::Settings),
                cx.listener(|this, _: &ClickEvent, _, cx| this.open_settings(cx)),
            ))
            .child(
                div()
                    .id("export-confirm")
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(px(22.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(rgb(match blocked.is_some() {
                        true => INK4(),
                        false => INK3(),
                    }))
                    .when(blocked.is_none(), |d| {
                        d.cursor_pointer().hover(|s| s.bg(rgb(DARK_RAISED())))
                    })
                    .when(blocked.is_some(), |d| d.cursor_not_allowed())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_export(cx)))
                    .type_style(type_scale::label(
                        type_scale::LABEL_ROW_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(match blocked.is_some() {
                        true => INK4(),
                        false => INK1(),
                    }))
                    .child("Export")
                    .child(
                        div()
                            .type_style(type_scale::mono(
                                type_scale::CHORD_METADATA_MIN_PX,
                                gpui::FontWeight::MEDIUM,
                            ))
                            .text_color(rgb(INK3()))
                            .child(self.keymap.chord(ActionId::Export)),
                    ),
            );
        let floor = crate::ui::stance::below_picture_floor(
            f32::from(viewport.height),
            self.split_px(Split::Bench, viewport),
        );
        Some(
            div()
                .id("export-click-catcher")
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .items_end()
                // Below the time band, not merely below the picture: the
                // chip that opened this moment is on that band.
                .pt(px(floor + crate::ui::stance::TIME_BAND_H + 6.))
                .pb(px(6.))
                .px(px(6.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .id("export-plate")
                        .flex_1()
                        .min_w(px(0.))
                        // The artboard is 784 wide; wider windows give the
                        // moment room, never a 2000px line of three words.
                        .max_w(px(EXPORT_W * 2.))
                        .h(px(EXPORT_MOMENT_H))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .justify_between()
                        .pt(px(22.))
                        .pb(px(20.))
                        .px(px(28.))
                        .rounded(px(4.))
                        .bg(rgb(DARK_PANEL()))
                        .border_1()
                        .border_color(rgba(DARK_SEAM()))
                        .child(file_row)
                        .child(budget_row)
                        .child(commit_row)
                        .into_any_element(),
                )
                .into_any_element(),
        )
    }

    /// The export while it runs, on the same sheet it was asked for on: an
    /// editor that takes no edit until the worker is done says so as a card and
    /// not as a strip under a panel nobody may touch. Same scrim, same width,
    /// same raised box as [`Player::export_card`] -- the answer arrives where
    /// the question was put -- and the timeline is still read around it.
    ///
    /// Two things it does *not* do. It never closes: not on a press away, not
    /// on `esc`. There is nothing here to dismiss while the export is still
    /// running, and a modal that vanishes leaves a locked editor with no reason
    /// on screen. And its cancel is two presses, never one -- an hour of
    /// encoding must not end on a stray click, which is why the stroke is a
    /// chord as well ([`cancels_export`]).
    pub(crate) fn export_progress_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let export = self.exporting()?;
        let progress = export.progress().clamp(0., 1.);
        let elapsed = self
            .export_started
            .map_or(0., |t| t.elapsed().as_secs_f32());
        // Two numbers that must both be honest: the one that counts up is
        // measured, the one that counts down is a guess and says so.
        let left = eta_secs(&self.export_marks, elapsed, progress).map_or_else(
            || "estimating…".to_owned(),
            |s| format!("~{} left", clock(s)),
        );
        // What is being read, the same files the engine names on stderr
        // ("export source:"): the project's own sources, never a stand-in.
        let source = match self.sources() {
            [] => "the timeline".to_owned(),
            [one] => file_name(&one.path),
            [first, rest @ ..] => format!("{} +{} more", file_name(&first.path), rest.len()),
        };
        let note = |text: SharedString| dark_help(text).into_any_element();
        let armed = self.cancel_armed;
        let percent_line: SharedString = format!(
            "{}% · {} elapsed · {left}",
            (progress * 100.) as u32,
            clock(elapsed),
        )
        .into();
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                // No `close_card` here, unlike every other sheet: a press away
                // is not a way out of a running export, and the only thing that
                // ends this card is the export ending.
                .on_mouse_down(MouseButton::Left, swallow)
                .child(
                    div()
                        .w(px(EXPORT_W))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(|d| d.child(dark_card_head("Exporting", None, None, None, cx)))
                        // The bar itself: the same number as the percentage
                        // below it, and it only ever moves forward -- the
                        // worker's progress is a monotone `fetch_max`.
                        .child(
                            div()
                                .flex_none()
                                .mx(px(6.))
                                .h(px(6.))
                                .rounded(px(3.))
                                .bg(rgb(DARK_HAIRLINE()))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(progress))
                                        .rounded(px(3.))
                                        .bg(rgb(STATUS_PROGRESS())),
                                ),
                        )
                        .map(|d| d.child(dark_row_value(percent_line.clone())))
                        // The row that was picked, then the seats the worker
                        // actually opened -- so a fallback to the software
                        // encoder shows here rather than being invisible.
                        .child(note(
                            format!(
                                "{} · {}",
                                format_label(self.format),
                                export
                                    .encoders()
                                    .unwrap_or_else(|| "opening the encoder".to_string()),
                            )
                            .into(),
                        ))
                        .child(note(
                            format!("{source} → {}", file_name(&self.export_path)).into(),
                        ))
                        .child(note(
                            match armed {
                                true => {
                                    "cancelling deletes what has been written so far".to_string()
                                }
                                false => format!(
                                    "{} cancels · esc alone does nothing while this runs · the \
                                     timeline is read-only until it finishes",
                                    self.keymap.display(ActionId::CancelExport)
                                ),
                            }
                            .into(),
                        ))
                        // One button, or the pair that answers it: never a
                        // control that cycles -- both choices are on screen at
                        // once, each saying which one it is.
                        .child(
                            div()
                                .mt(px(2.))
                                .flex()
                                .gap(px(6.))
                                .justify_end()
                                .when(armed, |d| {
                                    d.child(dark_ghost_button(
                                        "export-keep",
                                        "Keep exporting",
                                        "",
                                        true,
                                        cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.cancel_armed = false;
                                            cx.notify();
                                        }),
                                    ))
                                })
                                .map(|d| {
                                    d.child(dark_ghost_button(
                                        "export-cancel",
                                        "Cancel export",
                                        "",
                                        armed,
                                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            match this.cancel_armed {
                                                true => this.cancel_export(),
                                                false => this.cancel_armed = true,
                                            }
                                            cx.notify();
                                        }),
                                    ))
                                }),
                        ),
                ),
        )
    }

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
        let handles: Vec<_> = self
            .eq_params
            .bands
            .iter()
            .enumerate()
            .map(|(i, band)| {
                div()
                    .absolute()
                    .left(relative(eq_x(band.freq_hz)))
                    .top(relative(eq_y(band.gain_db)))
                    // Centred on its own point: it hangs off the graph's corner,
                    // so it is pulled back by half of itself both ways.
                    .ml(px(-EQ_HANDLE / 2.))
                    .mt(px(-EQ_HANDLE / 2.))
                    .w(px(EQ_HANDLE))
                    .h(px(EQ_HANDLE))
                    .rounded(px(EQ_HANDLE / 2.))
                    .bg(rgb(if i == self.eq_band { INK1() } else { INK3() }))
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
            .child(bounds_probe(self.eq_graph.clone()))
            // The analyser first, so everything else is drawn on top of it.
            .children(spectrum)
            // The decades, so a hump can be read as "around 200 Hz" without
            // dropping the eye to the labels along the bottom. The two ends
            // are the box's own edges and rule nothing.
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
            // Half way to each limit, each carrying its own number: the curve
            // is a curve of decibels, and until now only one of them was drawn.
            .children(EQ_DB_GRID.map(|db| {
                div()
                    .absolute()
                    .top(relative(eq_y(db)))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(DARK_HAIRLINE()))
                    .child(
                        div()
                            .absolute()
                            .left(px(4.))
                            .top(px(-11.))
                            .map(|d| {
                                d.type_style(type_scale::mono(
                                    type_scale::CHORD_METADATA_MIN_PX,
                                    gpui::FontWeight::MEDIUM,
                                ))
                                .text_color(rgb(INK3()))
                            })
                            .child(format!("{db:+.0}")),
                    )
            }))
            // 0 dB: the line a boost is a boost *from*.
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .w_full()
                    .h(px(1.))
                    .bg(rgb(DARK_HAIRLINE())),
            )
            .child(eq_curve(self.eq_params.clone(), sample_rate))
            .children(handles)
            .children(EQ_TICKS.iter().enumerate().map(|(i, (freq, label))| {
                // Centred on its own frequency for the three inner ticks --
                // pulled back by half its own width, as the comment above
                // used to say for all five. The two ends used the same
                // centring and, sitting at 0%/100% of the axis, hung half
                // their own label off the graph's edge: "20 Hz" clipped to a
                // bare "z", "20k" lost its "k" and half its trailing "0".
                // Anchored to the graph's own edge instead and read inward,
                // both now sit wholly inside the plot they label.
                // The end boxes are widened to their own label at the size
                // they draw at (`eq_tick_end_w`), not just anchored -- an
                // anchor alone still let "20 Hz" wrap onto two lines inside
                // a box narrower than the text. `.whitespace_nowrap()` is
                // the platform's own backstop, so a mis-measured box clips
                // or overhangs instead of silently wrapping again.
                let end_px = type_scale::CHORD_METADATA_MIN_PX;
                let div = div().absolute().bottom(px(1.)).whitespace_nowrap();
                let div = if i == 0 {
                    div.w(px(eq_tick_end_w(label, end_px)))
                        .left(px(0.))
                        .text_align(TextAlign::Left)
                } else if i == EQ_TICKS.len() - 1 {
                    div.w(px(eq_tick_end_w(label, end_px)))
                        .right(px(0.))
                        .text_align(TextAlign::Right)
                } else {
                    div.w(px(24.))
                        .left(relative(eq_x(*freq)))
                        .ml(px(-12.))
                        .text_align(TextAlign::Center)
                };
                div.map(|d| {
                    d.type_style(type_scale::mono(
                        type_scale::CHORD_METADATA_MIN_PX,
                        gpui::FontWeight::MEDIUM,
                    ))
                    .text_color(rgb(INK3()))
                })
                .child(*label)
            }))
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(4.))
                    .map(|d| {
                        d.type_style(type_scale::mono(
                            type_scale::CHORD_METADATA_MIN_PX,
                            gpui::FontWeight::MEDIUM,
                        ))
                        .text_color(rgb(INK3()))
                    })
                    .child(format!("+{EQ_GAIN_LIMIT:.0} dB")),
            );
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
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| d.top(px(crate::ui::stance::maximized_card_top(f32::from(viewport.height), self.split_px(Split::Bench, viewport)))))
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
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
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
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
                                    d.child(dark_card_head(
                                        "Equalizer",
                                        Some(format!("{} clip {}", lane.label(), idx + 1).into()),
                                        Some("drag a handle, or a digit picks a band — ←→ moves it, ↑↓ its gain, shift+←→ its width; a adds, x removes, f flattens it, r all, s spectrum, m fills the room; a click away or esc closes".into()),
                                        Some(self.card_maximized),
                                        cx,
                                    ))
                                })

                                .when(self.notices.front().is_some(), |d| {
                                    d.child(dark_help(self.notices.front().cloned().unwrap_or_default()))
                                })

                                .child(graph)
                                // Which band the keyboard is holding and every number it
                                // is set to, each with the pair of buttons that moves it:
                                // the curve shows the sum, and a band pushed against one
                                // pulling the other way is not readable off it.
                                .child(self.eq_numbers(picked, cx))
                                .child(
                                    div()
                                        .mt(px(4.))
                                        .flex()
                                        // The row of numbers above wraps for this
                                        // reason and so does this one: four buttons
                                        // are wider than the column at the floor,
                                        // and the one that ran off the edge --
                                        // the spectrum switch -- was then reachable
                                        // by its key alone.
                                        .flex_wrap()
                                        .gap(px(4.))
                                        .map(|d| {
                                            d.child(dark_ghost_button(
                                                "eq-reset",
                                                "Flatten all",
                                                "r",
                                                false,
                                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    for band in &mut this.eq_params.bands {
                                                        band.gain_db = 0.;
                                                    }
                                                    this.commit_eq(cx);
                                                }),
                                            ))
                                        })

                                        // The two that change how many bands there are.
                                        // The engine takes any cascade -- the count was
                                        // only ever fixed because this card had no way
                                        // to say otherwise.
                                        .map(|d| {
                                            d.child(dark_ghost_button(
                                                "eq-add",
                                                "Add band",
                                                "a",
                                                false,
                                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.add_band(cx)
                                                }),
                                            ))
                                        })

                                        .map(|d| {
                                            d.child(dark_ghost_button(
                                                "eq-remove",
                                                "Remove band",
                                                "x",
                                                false,
                                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.remove_band(cx)
                                                }),
                                            ))
                                        })

                                        // The analyser's switch, next to the one other
                                        // button the card has: `s` does the same, and a
                                        // toggle only a keystroke can reach is one most
                                        // people never find.
                                        .map(|d| {
                                            d.child(dark_ghost_button(
                                                "eq-spectrum",
                                                match self.eq_spectrum {
                                                    true => "Spectrum on",
                                                    false => "Spectrum off",
                                                },
                                                "s",
                                                self.eq_spectrum,
                                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                                    this.eq_spectrum = !this.eq_spectrum;
                                                    cx.notify();
                                                }),
                                            ))
                                        })
                                        ,
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
            .flex()
            // Three numbers and their steppers are wider than the inspector
            // column: without this the Q pair and the flatten button hang off
            // the card, which off the right-hand column means off the window.
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .px(px(6.));
        let Some(band) = picked.copied() else {
            return row.child(dark_help("no bands — a adds one"));
        };
        let step = |id: &'static str,
                    label: &'static str,
                    change: fn(&mut Band),
                    cx: &mut Context<Self>| {
            let on_click =
                cx.listener(move |this, _: &ClickEvent, _, cx| this.nudge_band(change, cx));
            dark_step_glyph(id, label == "+", on_click).into_any_element()
        };
        let number = |value: String,
                      ids: (&'static str, &'static str),
                      by: (fn(&mut Band), fn(&mut Band)),
                      cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(dark_row_value(value).into_any_element())
                .child(step(ids.0, "−", by.0, cx))
                .child(step(ids.1, "+", by.1, cx))
        };
        row.child(
            dark_help(format!(
                "Band {} of {}",
                self.eq_band + 1,
                self.eq_params.bands.len()
            ))
            .into_any_element(),
        )
        .child(number(
            band_label(&band),
            ("eq-freq-down", "eq-freq-up"),
            (|b| b.freq_hz /= EQ_FREQ_STEP, |b| b.freq_hz *= EQ_FREQ_STEP),
            cx,
        ))
        .child(number(
            format!("{:+.1} dB", band.gain_db),
            ("eq-gain-down", "eq-gain-up"),
            (|b| b.gain_db -= EQ_STEP, |b| b.gain_db += EQ_STEP),
            cx,
        ))
        // Q is width, so its buttons are labelled by what they *do* to the
        // hump rather than by which way the number goes: a wider band is a
        // smaller Q, and nobody should have to know that to use the card.
        .child(number(
            format!("Q {:.2}", band.q),
            ("eq-q-wider", "eq-q-narrower"),
            (|b| b.q /= EQ_Q_STEP, |b| b.q *= EQ_Q_STEP),
            cx,
        ))
        .child(
            div()
                .id("eq-flat-band")
                .flex()
                .h(px(HIT_MIN))
                .px(px(8.))
                .items_center()
                .rounded(px(3.))
                .bg(rgb(BG_PANEL()))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.nudge_band(|b| b.gain_db = 0., cx)
                }))
                .child("Flatten this"),
        )
    }

    /// The colour card: the graded frame's histogram over a row per control,
    /// each row a bar the pointer drags straight to a value -- no stepper
    /// buttons, because a slider is a thing to pull, and the arrow keys still
    /// move the same value for anyone not using a pointer. Same scrim, surface
    /// and row shape as the other two cards, and the same plain divs, so the
    /// root keeps the keyboard.
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
        let (lane, idx) = self.color_open?;
        let params = self.color_params();
        let rows: Vec<_> = COLOR_BANDS
            .iter()
            .enumerate()
            .map(|(i, &(label, low, high))| {
                let mut read = params;
                let value = *band_mut(&mut read, i);
                let frac = ((value - low) / (high - low)).clamp(0., 1.);
                let picked = i == self.color_band;
                div()
                    .id(("color-row", i))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    // Darkroom: no permanent pill under a label -- the picked
                    // row is a 1px `ink1` rule (§4's focus ring), never a fill.
                    // Hover is the one fill step §4 allows.
                    .when(picked, |d| d.border_l_2().border_color(rgb(INK1())))
                    .hover(|s| s.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.color_band = i;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .map(|d| d.child(dark_row_label(label, picked))),
                    )
                    .child(
                        // The bar is 4 px to look at and a whole row to hit
                        // (WCAG 2.5.8), the same split the ruler makes between
                        // what is drawn and what is grabbed. The press is
                        // already the first sample of the drag, so a plain click
                        // sets the value it landed on.
                        div()
                            .id(("color-bar", i))
                            .relative()
                            .flex_1()
                            .min_w(px(0.))
                            .max_w(px(COLOR_BAR_W))
                            .h(px(KEYS_ROW_H))
                            .flex()
                            .items_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.color_band = i;
                                    this.color_dragging = true;
                                    this.drag_color(event.position.x, true, cx);
                                }),
                            )
                            .child(bounds_probe(self.color_bars[i].clone()))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(DARK_PANEL()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(frac))
                                            .rounded(px(2.))
                                            .bg(rgb(ACCENT_PRIMARY())),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .map(|d| d.child(dark_row_value(format!("{:.0}%", value * 100.)))),
                    )
            })
            .collect();
        let head_meta: SharedString = format!("{} clip {}", lane.label(), idx + 1).into();
        let help_text =
            "drag a bar, or ↑↓ picks one and ←→ moves it, r resets — a click away or esc closes";
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
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
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(|d| {
                            d.child(dark_card_head(
                                "Colour",
                                Some(head_meta.clone()),
                                Some(help_text.into()),
                                Some(self.card_maximized),
                                cx,
                            ))
                        })
                        // The frame as it is being graded, over the controls
                        // grading it: the three lines are what the picture is
                        // made of, and every sample of a drag reseeks, so they
                        // move with the bar under the hand.
                        .child(
                            div()
                                .flex_none()
                                .h(px(HIST_H))
                                .rounded(px(3.))
                                .bg(rgb(DARK_HAIRLINE()))
                                .relative()
                                .child(hist_curves(self.histogram)),
                        )
                        .children(rows)
                        .map(|d| {
                            d.child(dark_ghost_button(
                                "color-reset",
                                "Reset",
                                "r",
                                false,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_color(ColorParams::default(), cx);
                                }),
                            ))
                        }),
                ),
        )
    }

    /// The transform card: [`color_card`](Self::color_card)'s own shape, one
    /// row per [`TRANSFORM_BANDS`] entry instead of four, and no histogram --
    /// there is nothing here a graded frame would tilt.
    pub(crate) fn transform_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.transform_open?;
        let params = self.transform_params();
        let rows: Vec<_> = TRANSFORM_BANDS
            .iter()
            .enumerate()
            .map(|(i, &(label, low, high))| {
                let mut read = params;
                let value = *transform_band_mut(&mut read, i);
                let frac = ((value - low) / (high - low)).clamp(0., 1.);
                let picked = i == self.transform_band;
                div()
                    .id(("transform-row", i))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(picked, |d| d.border_l_2().border_color(rgb(INK1())))
                    .hover(|s| s.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.transform_band = i;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .map(|d| d.child(dark_row_label(label, picked))),
                    )
                    .child(
                        div()
                            .id(("transform-bar", i))
                            .relative()
                            .flex_1()
                            .min_w(px(0.))
                            .max_w(px(TRANSFORM_BAR_W))
                            .h(px(KEYS_ROW_H))
                            .flex()
                            .items_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.transform_band = i;
                                    this.transform_dragging = true;
                                    this.drag_transform(event.position.x, true, cx);
                                }),
                            )
                            .child(bounds_probe(self.transform_bars[i].clone()))
                            .child(
                                div()
                                    .w_full()
                                    .h(px(4.))
                                    .rounded(px(2.))
                                    .bg(rgb(DARK_PANEL()))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(frac))
                                            .rounded(px(2.))
                                            .bg(rgb(ACCENT_PRIMARY())),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .map(|d| d.child(dark_row_value(transform_row_value(i, value)))),
                    )
            })
            .collect();
        let head_meta: SharedString = format!("{} clip {}", lane.label(), idx + 1).into();
        let help_text = "drag a bar, or ↑↓ picks one and ←→ moves it (rotation steps \
                     by 90°), r resets — a click away or esc closes";
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
                )
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
                        .gap(px(2.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(|d| {
                            d.child(dark_card_head(
                                "Transform",
                                Some(head_meta.clone()),
                                Some(help_text.into()),
                                Some(self.card_maximized),
                                cx,
                            ))
                        })
                        .children(rows)
                        .map(|d| {
                            d.child(dark_ghost_button(
                                "transform-reset",
                                "Reset",
                                "r",
                                false,
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.set_transform(TransformParams::default(), cx);
                                }),
                            ))
                        }),
                ),
        )
    }

    /// The speed card: one bar from a quarter speed to four times it, the rates
    /// people name as buttons under it, and the clip's new length in frames --
    /// which is the number a person is actually choosing. Built like the colour
    /// card down to the scrim and the bar's own hit height, because it is the
    /// same kind of card: one continuous value on one clip, live at the clip as
    /// it moves.
    ///
    /// Honest about what it does: the sound is *resampled*, so the pitch goes up
    /// with the rate, which is what the tape in the title means.
    pub(crate) fn speed_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.speed_open?;
        let speed = self.card_speed();
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx).copied()?;
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let frac = ((f32::from(speed.permille()) - lo) / (hi - lo)).clamp(0., 1.);
        let presets: Vec<_> = SPEED_PRESETS
            .into_iter()
            .map(|permille| {
                let at = Speed::from_permille(permille);
                let picked = at == speed;
                div()
                    .id(("speed-preset", usize::from(permille)))
                    .flex_1()
                    .flex()
                    .h(px(CONTROL_H))
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .when(picked, |d| d.bg(rgb(DARK_RAISED())))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_speed(at, cx);
                    }))
                    .map(|d| d.child(dark_row_value(format!("{at}"))))
            })
            .collect();
        let head_meta: SharedString = format!("{} clip {}", lane.label(), idx + 1).into();
        let help_text = "drag the bar or ←→ moves it, r is 1.00x — the pitch moves with the rate; a click away or esc closes";
        let tail_text: SharedString = format!(
            "{speed} — {} source frames over {} on the timeline ({})",
            clip.len(),
            clip.frames(),
            frames_timecode(clip.frames(), self.fps)
        )
        .into();
        Some(
            drag_scrim(cx)
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
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
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(|d| {
                            d.child(dark_card_head(
                                "Speed (tape)",
                                Some(head_meta.clone()),
                                Some(help_text.into()),
                                Some(self.card_maximized),
                                cx,
                            ))
                        })
                        .child(
                            // 4 px to look at and a whole row to hit (WCAG
                            // 2.5.8), the split the colour sliders and the ruler
                            // both make.
                            div()
                                .id("speed-bar")
                                .relative()
                                .w_full()
                                .h(px(KEYS_ROW_H))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        this.speed_dragging = true;
                                        this.drag_speed(event.position.x, true, cx);
                                    }),
                                )
                                .child(bounds_probe(self.speed_bar.clone()))
                                .child(
                                    div()
                                        .w_full()
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(rgb(DARK_PANEL()))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative(frac))
                                                .rounded(px(2.))
                                                .bg(rgb(ACCENT_PRIMARY())),
                                        ),
                                ),
                        )
                        .child(div().flex().gap(px(4.)).children(presets))
                        .map(|d| d.child(dark_help(tail_text.clone()))),
                ),
        )
    }

    /// The mix card: one fader per audio track and the master limiter under
    /// them -- the two settings that belong to the *sound of the whole
    /// timeline* rather than to any clip on it.
    ///
    /// A track's fader moves everything on that track by the same amount,
    /// every frequency of it: it is not the equalizer (one take, one band) and
    /// it is not the volume in the panel, which is what this machine monitors
    /// at and is written to no file. The limiter is over the sum of them all,
    /// which is where a mix can pass full scale and where a clamp used to
    /// square it off.
    ///
    /// The silence card's shape, down to the steppers: a row is a label, a
    /// value and the two presses that move it, and the arrows pick a row and
    /// move it too. The rows scroll rather than the card growing past the
    /// window -- a timeline may hold more tracks than a 360 px window has room
    /// for faders.
    pub(crate) fn mix_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.mix_open {
            return None;
        }
        let session = self.session.as_ref();
        let lanes = self.mix_lanes();
        let limiter = session.map_or_else(Limiter::default, PlaybackSession::limiter);
        let mut rows: Vec<(String, String)> = lanes
            .iter()
            .map(|&lane| {
                let db = session.map_or(0., |s| s.lane_gain_db(lane));
                (format!("{} plays at", lane.label()), format!("{db:+.0} dB"))
            })
            .collect();
        // The ceiling in dBFS and the faders in dB, the silence card's rule:
        // a ceiling is a level below full scale, a fader is a change.
        rows.push((
            "Limiter ceiling".into(),
            format!("{:+.0} dBFS", limiter.ceiling_db),
        ));
        rows.push((
            "Limiter".into(),
            match limiter.on {
                true => "on".into(),
                false => "off".into(),
            },
        ));
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                let picked = n == self.mix_field;
                div()
                    .id(("mix-row", n))
                    .flex()
                    .flex_none()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .when(picked, |d| d.border_l_2().border_color(rgb(INK1())))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.mix_field = n;
                        cx.notify();
                    }))
                    .child(div().map(|d| d.child(dark_row_label(label.clone(), picked))))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .map(|d| d.child(dark_row_value(value.clone())))
                            .children([-1, 1].map(|steps: i32| {
                                let id = ("mix-step", n * 2 + usize::from(steps > 0));
                                dark_step_glyph(
                                    id,
                                    steps > 0,
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.mix_field = n;
                                        this.nudge_mix(steps, cx);
                                    }),
                                )
                                .into_any_element()
                            })),
                    )
            })
            .collect();
        let help_text = "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — a track fader moves everything on that track; a click away or esc closes";
        let tail_text = match limiter.on {
            true => format!(
                "the mix is held under {:+.0} dBFS — quieter passages are untouched",
                limiter.ceiling_db
            ),
            false => "the limiter is out of circuit — a hot mix clips at full scale".to_string(),
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                // Click away closes it, as on every card here.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
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
                        .max_h(px(360. - 24.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
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
                                .children(rows),
                        )
                        .map(|d| d.child(dark_help(tail_text.clone()))),
                ),
        )
    }

    /// The subtitle style card: the size stepper on top, the platform's own
    /// font list scrolling under it -- a picker's rows and not a cycle, so a
    /// hundred-odd families are each one click and not a hundred presses of
    /// the same key. Nothing here is a clip's or the project's, the mix
    /// card's shape for the same reason: app-global, kept in a file beside
    /// the theme, and drawn straight off `self.sub_text` / `self.sub_family`
    /// so a change is on the cue underneath before the card is closed.
    pub(crate) fn subtitle_style_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.subtitle_style_open {
            return None;
        }
        let help_text = "− and + move the size, or ↑↓ picks a row and ←→ moves it (held or pressed); a family row picks it outright — a click away or esc closes";
        let head = Some(dark_card_head(
            "Subtitle style",
            None,
            Some(help_text.into()),
            Some(self.card_maximized),
            cx,
        ));
        let size_picked = self.subtitle_style_field == 0;
        let size_row = div()
            .id("subtitle-size-row")
            .flex()
            .flex_none()
            .min_h(px(KEYS_ROW_H))
            .items_center()
            .justify_between()
            .px(px(6.))
            .rounded(px(3.))
            .when(size_picked, |d| d.border_l_2().border_color(rgb(INK1())))
            .child(div().map(|d| d.child(dark_row_label("Size", size_picked))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .map(|d| d.child(dark_row_value(format!("{:.0}px", self.sub_text))))
                    .children([-1, 1].map(|steps: i32| {
                        let id = ("subtitle-size-step", usize::from(steps > 0));
                        dark_step_glyph(
                            id,
                            steps > 0,
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.subtitle_style_field = 0;
                                this.nudge_sub_size(steps, cx);
                            }),
                        )
                        .into_any_element()
                    })),
            );
        let default_picked = self.subtitle_style_field == 1;
        let default_row = div()
            .id("subtitle-family-row-default")
            .flex()
            .flex_none()
            .min_h(px(KEYS_ROW_H))
            .items_center()
            .px(px(6.))
            .rounded(px(3.))
            .when(default_picked, |d| d.border_l_2().border_color(rgb(INK1())))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(DARK_RAISED())))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.set_sub_family(None, cx);
            }))
            .map(|d| d.child(dark_row_label("System default", default_picked)));
        let family_rows = self.subtitle_fonts.iter().enumerate().map(|(n, name)| {
            let picked = self.subtitle_style_field == n + 2
                || self.sub_family.as_deref() == Some(name.as_str());
            div()
                .id(("subtitle-family-row", n))
                .flex()
                .flex_none()
                .min_h(px(KEYS_ROW_H))
                .items_center()
                .px(px(6.))
                .rounded(px(3.))
                .when(picked, |d| d.border_l_2().border_color(rgb(INK1())))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(cx.listener({
                    let name = name.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        this.set_sub_family(Some(name.clone()), cx);
                    }
                }))
                .map(|d| d.child(dark_row_label(name.clone(), picked)))
        });
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_center()
                .bg(rgba(SCRIM()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
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
                        .max_h(px(360. - 24.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(move |d| d.child(head.unwrap()))
                        .child(size_row)
                        .child(
                            div()
                                .id("subtitle-family-rows")
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .child(default_row)
                                .children(family_rows),
                        ),
                ),
        )
    }

    /// The silence card: what the scan is looking for, what it found, and the
    /// two things that can be done about it.
    ///
    /// Its scrim is the lightest of the cards' on purpose. Every other card is
    /// about the clip it names and can black the timeline out; this one is
    /// *about* the timeline -- the marks under it are the whole preview -- so
    /// the bed stays readable and the card sits up in the picture area rather
    /// than over the lanes.
    pub(crate) fn silence_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (lane, idx) = self.silence_open?;
        let head_meta: SharedString = format!("{} clip {}", lane.label(), idx + 1).into();
        let help_text = "− and + move a setting, or ↑↓ picks one and ←→ moves it (hold to run it) — the marks on the lane are what would go; a click away or esc closes";
        let head = Some(dark_card_head(
            "Silences",
            Some(head_meta.clone()),
            Some(help_text.into()),
            Some(self.card_maximized),
            cx,
        ));
        let cfg = self.silence;
        // The unit is a label, never a conversion: the threshold is a level
        // below full scale whichever of the two the row says (`silence_dbfs`).
        let unit = match self.silence_dbfs {
            true => "dBFS",
            false => "dB",
        };
        let rows = [
            ("Apply to", self.silence_scope.label(&self.silence_lanes())),
            (
                "Silence is under",
                format!("{:.0} {unit}", cfg.threshold_db),
            ),
            ("Level read in", format!("{unit} (0 = full scale)")),
            (
                "Forgive quiet shorter than",
                format!("{:.2} s", cfg.min_silence),
            ),
            (
                "Keep either side of speech",
                format!("{:.2} s", cfg.padding),
            ),
            (
                "Swallow kept slivers under",
                format!("{:.2} s", cfg.min_keep),
            ),
            ("Speed-up plays at", format!("{}", self.silence_factor)),
        ];
        let found = self.silence_marks.len();
        let secs =
            f64::from(self.silence_marks.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        // The line under the rows: where a scan still running has got to -- a
        // card is up from the frame it is asked for, numbers or no numbers --
        // or what the settings found in levels already read.
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
            None => match found {
                0 => "nothing quiet enough for long enough".to_string(),
                1 => format!("1 silence, {}", secs_label(secs)),
                n => format!("{n} silences, {}", secs_label(secs)),
            },
        };
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(n, (label, value))| {
                let picked = n == self.silence_field;
                div()
                    .id(("silence-row", n))
                    .flex()
                    .min_h(px(KEYS_ROW_H))
                    .items_center()
                    .justify_between()
                    .px(px(6.))
                    .rounded(px(3.))
                    .when(picked, |d| d.border_l_2().border_color(rgb(INK1())))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(DARK_RAISED())))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.silence_field = n;
                        cx.notify();
                    }))
                    .child(div().map(|d| d.child(dark_row_label(label, picked))))
                    // The value and the two steps that move it. Every other
                    // card has something to drag or press; this one had the
                    // arrow keys and nothing else, so a row was a setting a
                    // pointer could pick but never change. One press each, the
                    // same call the arrows make -- the hold-to-run is the
                    // keyboard's own and is not a thing a button has.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .map(|d| d.child(dark_row_value(value.clone())))
                            .children([-1, 1].map(|steps: i32| {
                                let id = ("silence-step", n * 2 + usize::from(steps > 0));
                                dark_step_glyph(
                                    id,
                                    steps > 0,
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.silence_field = n;
                                        this.nudge_silence(steps);
                                        cx.notify();
                                    }),
                                )
                                .into_any_element()
                            })),
                    )
            })
            .collect();
        // The two buttons the ask names, side by side: a mode toggle would hide
        // one of them behind the other, and there are only two.
        let button = |n: usize, text: String, act: fn(&mut Self, &mut Context<Self>)| {
            let enabled = found != 0;
            dark_ghost_button(
                ("silence-apply", n),
                text,
                "",
                enabled,
                cx.listener(move |this, _: &ClickEvent, _, cx| act(this, cx)),
            )
            .into_any_element()
        };
        Some(
            scrim()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(HEADER_H + 8.))
                // Light enough to read the lanes and the marks on them through:
                // the preview is the point of this card.
                .bg(rgba(SCRIM_LIGHT()))
                .when(self.card_maximized, |d| {
                    d.top(px(crate::ui::stance::maximized_card_top(
                        f32::from(viewport.height),
                        self.split_px(Split::Bench, viewport),
                    )))
                })
                // Click away closes it, as on every card here -- and the marks
                // go with it, which is what makes this one a call and not a flag.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.close_card();
                        cx.notify();
                        cx.stop_propagation();
                    }),
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
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(12.))
                        .rounded(px(6.))
                        .bg(rgb(DARK_PANEL()))
                        .map(|d| d.border_1().border_color(rgba(DARK_SEAM())))
                        .map(move |d| d.child(head.unwrap()))
                        .children(rows)
                        .map(|d| d.child(dark_help(status.clone())))
                        .child(div().flex().gap(px(4.)).children([
                            button(0, "Cut them out (enter)".into(), Self::cut_silences),
                            button(
                                1,
                                format!("Play them at {} (f)", self.silence_factor),
                                Self::speed_silences,
                            ),
                        ])),
                ),
        )
    }
}
