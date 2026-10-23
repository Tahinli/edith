//! The creator tools card: four tabs (Normalize, Captions, Fillers, Voiceover) over the
//! engine's shared tools. Choices are dropdowns and segmented rows, the numbers are a slider
//! with a number field (drag it to scrub, click it to type), and a tool that cannot run says
//! why and offers the way to get what it lacks.

use super::common::*;
use crate::creator::{self, LANGUAGES, Said};
use crate::player::tools::{Cycle, NumField, Tool, Tools};
use crate::ui::icons::Icon;
use crate::ui::kit::{self, Segment};
use crate::*;
use engine::speech::Hear;
use crate::layout::redesign as r;
use crate::ui::type_scale::{self, Typeset};
use gpui::{DispatchPhase, Entity, canvas, deferred};

/// The pick (clip, caption, title) and the scope a result was read under.
type ResultPick = (Option<(engine::project::Lane, usize)>, Option<(engine::project::Lane, usize, usize)>, Option<usize>, bool);

thread_local! {
    /// The pick and scope the Normalize line on screen was first drawn under, with the line. The
    /// line is drawn only while those still hold (see `result_shown`).
    static NORMALIZE_SEEN: std::cell::RefCell<Option<(ResultPick, String)>> = const { std::cell::RefCell::new(None) };
}

/// A window-wide pointer catcher for a drag that began on one of this card's controls: while
/// `live`, every move goes to `on_move` (window x, Shift held) and the release to `on_up`, even
/// when the pointer has left the control. Paints nothing; nothing is registered while idle.
pub(super) fn drag_catcher(
    me: Entity<Player>,
    live: bool,
    on_move: impl Fn(&mut Player, f32, bool, &mut Context<Player>) + 'static,
    on_up: impl Fn(&mut Player, &mut Context<Player>) + 'static,
) -> impl IntoElement {
    let (me_up, on_move, on_up) = (me.clone(), Rc::new(on_move), Rc::new(on_up));
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            if !live {
                return;
            }
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    me.update(cx, |this, cx| on_move(this, f32::from(e.position.x), e.modifiers.shift, cx));
                }
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    me_up.update(cx, |this, cx| on_up(this, cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}

/// A 4px track with a 14px thumb at `fraction`, the whole thing `HIT_MIN` tall to hit. A press
/// is `on_press(window x)`; the caller's catcher carries the drag on from there.
pub(super) fn slider_track(
    id: &'static str,
    fraction: f32,
    bar: Rc<Cell<Bounds<Pixels>>>,
    on_press: impl Fn(&mut Player, Pixels, &mut Context<Player>) + 'static,
    cx: &mut Context<Player>,
) -> Stateful<Div> {
    const THUMB: f32 = 14.;
    let t = tokens();
    let fraction = slider_fraction(fraction, 0., 1.);
    div()
        .id(id)
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
        .child(bounds_probe(bar))
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgb(t.e3))
                .child(div().h_full().w(relative(fraction)).rounded(px(2.)).bg(rgb(t.accent))),
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

/// A dropdown row: the label, then the closed field; the open list hangs from the field's foot,
/// and a press anywhere else shuts it.
#[allow(clippy::too_many_arguments)]
fn drop_row(
    id: &'static str,
    label: &'static str,
    items: Vec<SharedString>,
    current: usize,
    open: bool,
    what: Cycle,
    list_w: f32,
    busy: bool,
    cx: &mut Context<Player>,
) -> Div {
    // A picker holds still while a run it feeds is going.
    let enabled = !items.is_empty() && !busy;
    let value: SharedString = items.get(current).cloned().unwrap_or_else(|| "None installed".into());
    let me = cx.entity();
    let field = kit::dropdown_field(id, value, enabled).when(enabled, |d| {
        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.tools_toggle_drop(what, cx)))
    });
    let list = (open && enabled).then(|| {
        let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                this.tools_toggle_drop(what, cx);
                cx.stop_propagation();
            }),
        );
        let pick = {
            let me = me.clone();
            move |i: usize, _: &mut Window, app: &mut App| me.update(app, |this, cx| this.tools_pick(what, i, cx))
        };
        deferred(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .child(backdrop)
                .child(div().absolute().top(px(r::CONTROL_M + 2.)).left(px(0.)).child(kit::choice_list(id, list_w, &items, current, pick))),
        )
    });
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(r::CONTROL_M))
        .child(div().flex_none().w(px(84.)).child(dark_row_label(label, false)))
        .child(div().relative().flex_1().min_w(px(0.)).child(field).children(list))
}

/// One Normalize number: the label and a number field (with its unit and reset slot) on a line,
/// the slider under it.
fn num_row(
    id: &'static str,
    label: &'static str,
    unit: &'static str,
    field: NumField,
    t: &Tools,
    cx: &mut Context<Player>,
) -> Div {
    let me = cx.entity();
    let (value, default) = match field {
        NumField::Target => (t.lufs, creator::LUFS_PRESETS[0].1),
        NumField::Ceiling => (t.ceiling, engine::normalize::TARGET_TRUE_PEAK),
    };
    let (lo, hi) = field.range();
    let editing = t.num.as_ref().filter(|(f, _)| *f == field).map(|(_, e)| e);
    let scrubbing = t.drag.is_some_and(|d| d.field == field);
    let sliding = t.slide == Some(field);
    let bar = Rc::new(Cell::new(Bounds::default()));
    let probe = Rc::clone(&bar);
    let probe2 = Rc::clone(&bar);
    let num = kit::number_field(id, 112., field.shown(value), Some(unit), editing, true)
        .tooltip(crate::ui::widgets::overlay_tip_hover(
            label,
            "",
            Some(match field {
                NumField::Target => "YouTube and Spotify use \u{2212}14. Drag to change, click to type.",
                NumField::Ceiling => "Drag to change, click to type",
            }),
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.begin_num_drag(field, f32::from(event.position.x));
            }),
        );
    let reset = kit::reset_slot(
        // One slot id per field: an id the number's own never repeats.
        if field == NumField::Target { "tools-target-reset" } else { "tools-ceiling-reset" },
        (value - default).abs() > 1e-9,
        {
            let me = me.clone();
            move |_, app| me.update(app, |this, cx| this.tools_set_num(field, default, cx))
        },
    );
    let slide_id = if field == NumField::Target { "tools-target-bar" } else { "tools-ceiling-bar" };
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .min_h(px(r::CONTROL_M))
                .child(dark_row_label(label, false))
                .child(div().flex().items_center().gap(px(2.)).child(num).child(reset)),
        )
        .child(slider_track(
            slide_id,
            slider_fraction(value as f32, lo as f32, hi as f32),
            bar,
            move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.begin_slide(field);
                this.tools_slide(field, frac_along(x, probe.get()), cx);
            },
            cx,
        ))
        .child(drag_catcher(
            me.clone(),
            scrubbing || sliding,
            {
                move |this, x, fine, cx| match this.tools.as_ref().is_some_and(|t| t.slide.is_some()) {
                    true => this.tools_slide(field, frac_along(px(x), probe2.get()), cx),
                    false => this.num_drag_move(x, fine, cx),
                }
            },
            |this, cx| this.num_drag_end(cx),
        ))
}

/// The progress line of a running job, with its Cancel.
fn running_row(t: &Tools, cx: &mut Context<Player>) -> Option<Div> {
    let running = t.running.as_ref()?;
    let fraction = running.job.fraction();
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(dark_row_label(running.what, true))
                    .child(dark_row_value(format!("{:.0}%", fraction * 100.), true)),
            )
            .child(
                div()
                    .h(px(6.))
                    .rounded(px(2.))
                    .bg(rgb(tokens().ink4))
                    .child(div().h_full().w(relative(fraction.clamp(0.0, 1.0))).rounded(px(2.)).bg(rgb(tokens().accent))),
            )
            .child(div().flex().child(dark_ghost_button(
                "tools-cancel",
                "Cancel",
                "esc",
                false,
                cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_tools_job(cx)),
            ))),
    )
}

/// A kit button that can be off. Off it ignores input and its tooltip gives `why`; on, its
/// tooltip is the name alone (no key shown on the button or beside it).
fn gated_button(
    id: &'static str,
    label: SharedString,
    why: Option<&str>,
    on_click: impl Fn(&mut Player, &mut Context<Player>) + 'static,
    cx: &mut Context<Player>,
) -> Stateful<Div> {
    let b = kit::Button::new(id).label(label.clone()).enabled(why.is_none()).build();
    match why {
        Some(why) => b.tooltip(crate::ui::widgets::overlay_tip_hover(label.as_ref(), "", Some(why))),
        None => b
            .tooltip(crate::ui::widgets::overlay_tip_hover(label.as_ref(), "", None))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| on_click(this, cx))),
    }
}

/// A full-width action button (see [`gated_button`]).
fn action_button(
    id: &'static str,
    label: &'static str,
    why: Option<&str>,
    on_click: impl Fn(&mut Player, &mut Context<Player>) + 'static,
    cx: &mut Context<Player>,
) -> Stateful<Div> {
    gated_button(id, label.into(), why, on_click, cx).w_full()
}

/// A filler's tick: the Lucide check on an accent square when it is ticked, the empty square
/// when it is not.
fn tick_box(on: bool) -> Div {
    let t = tokens();
    match on {
        true => div()
            .size(px(14.))
            .rounded(px(3.))
            .bg(rgb(t.accent))
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::Check.el_px(px(11.), t.on_accent)),
        false => div()
            .size(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::Square.el_px(px(14.), t.ink3)),
    }
}

/// What a tool says when it has nothing to work with: the sentence, then the way to get it.
fn missing(text: &'static str, actions: Vec<Stateful<Div>>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .p(px(8.))
        .rounded(px(r::R_CONTROL))
        .bg(rgb(tokens().e2))
        .child(dark_help(text))
        .child(div().flex().gap(px(6.)).children(actions))
}

impl Player {
    /// The tools card. A click on a head folds it; keys: left and right switch tool, Enter runs it.
    pub(crate) fn tools_card(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let t = self.tools.as_ref()?;
        // Two rows of two, so a plain name never clips and the one in force stands out: the
        // row it is in marks it with the accent ring, the other row has none lit.
        let tab_row = |id: &'static str, tools: [Tool; 2], cx: &mut Context<Self>| {
            let at = tools.iter().position(|x| *x == t.tool);
            let me = cx.entity();
            kit::segmented_equal(
                id,
                &tools.map(|tool| Segment::text(tool.label())),
                at.unwrap_or(usize::MAX),
                true,
                at.map(|i| (i, kit::Look::Focus)),
                move |i, _, app| me.update(app, |this, cx| this.tools_tab(tools[i], cx)),
            )
        };
        let tabs = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(tab_row("tools-tab-a", [Tool::Normalize, Tool::Captions], cx))
            .child(tab_row("tools-tab-b", [Tool::Fillers, Tool::Voiceover], cx));
        let list_w = (f32::from(viewport.width) - 96.).max(120.);
        let body = match t.tool {
            Tool::Normalize => self.normalize_body(t, cx),
            Tool::Captions => self.listen_body(t, false, list_w, cx),
            Tool::Fillers => self.listen_body(t, true, list_w, cx),
            Tool::Voiceover => self.voiceover_body(t, list_w, cx),
        };
        let help_text = "Left and right switch tool, Enter runs it. Esc stops a run in progress.";
        Some(
            self.card_outer(viewport, cx).child(
                div()
                    .w_full()
                    .max_w(px(card_max_w(COLOR_W, self.card_maximized, f32::from(viewport.width))))
                    .on_mouse_down(MouseButton::Left, swallow)
                    .flex()
                    .flex_col()
                    .gap(px(CARD_GAP))
                    .map(|d| self.card_surface(d))
                    .child(dark_card_head("Tools", None, Some(help_text.into()), Some(self.card_maximized), cx))
                    .child(tabs)
                    .child(div().id("tools-rows").flex().flex_col().gap(px(4.)).child(body))
                    .when(self.result_shown(t), |d| d.child(self.status_row(t.said(t.tool), cx))),
            ),
        )
    }

    /// Whether the tool's last word is drawn. A Normalize result is for the pick and the scope it
    /// was read under: once either changes, its line is let go rather than left naming the old one.
    fn result_shown(&self, t: &Tools) -> bool {
        let said = t.said(t.tool);
        if said.line.is_empty() {
            return false;
        }
        if t.tool != Tool::Normalize {
            return true;
        }
        let pick = (self.selected.anchor(), self.sel_cue, self.title_sel, t.each);
        NORMALIZE_SEEN.with(|seen| {
            let mut seen = seen.borrow_mut();
            match seen.as_ref() {
                Some((at, line)) if *line == said.line => *at == pick,
                _ => {
                    *seen = Some((pick, said.line.clone()));
                    true
                }
            }
        })
    }

    /// The tool's last word: its sentence (the whole error in the tooltip when the sentence cut it
    /// short), and a Relink when the refusal is for files that are gone.
    fn status_row(&self, said: &Said, cx: &mut Context<Self>) -> Div {
        let mut line = div().id("tools-status").w_full().child(dark_help(said.line.clone()));
        if let Some(full) = &said.detail {
            line = line.tooltip(crate::ui::widgets::overlay_tip_hover("Why", "", Some(full.as_str())));
        }
        let mut row = div().flex().flex_col().gap(px(6.)).child(line);
        let offline = self.session.as_ref().and_then(|s| s.sources().iter().find(|src| !src.path.exists()).map(|src| src.path.clone()));
        if let (true, Some(path)) = (said.offline, offline) {
            row = row.child(
                div().flex().child(
                    kit::Button::new("tools-relink")
                        .label("Relink")
                        .build()
                        .tooltip(crate::ui::widgets::overlay_tip_hover("Relink", "", Some("Point the first offline file at its new place")))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.relink_pick(path.clone(), cx))),
                ),
            );
        }
        row
    }

    fn normalize_body(&self, t: &Tools, cx: &mut Context<Self>) -> Div {
        let presets = kit::segmented_equal(
            "tools-preset",
            &creator::LUFS_PRESETS.map(|(name, _)| Segment::text(name)),
            creator::LUFS_PRESETS.iter().position(|(_, lufs)| (t.lufs - lufs).abs() < 0.05).unwrap_or(usize::MAX),
            true,
            None,
            {
                let me = cx.entity();
                move |i, _, app| me.update(app, |this, cx| this.tools_set_target(creator::LUFS_PRESETS[i].1, cx))
            },
        );
        let scopes = kit::segmented_equal(
            "tools-scope",
            &[Segment::text("Whole mix"), Segment::text("Each track")],
            usize::from(t.each),
            true,
            None,
            {
                let me = cx.entity();
                move |i, _, app| {
                    me.update(app, |this, cx| {
                        // A result names the scope it ran on: the old line goes with the switch.
                        if let Some(t) = this.tools.as_mut() {
                            t.set_said(Tool::Normalize, Said::default());
                        }
                        this.tools_set_each(i == 1, cx)
                    })
                }
            },
        );
        let silent = self.session.as_ref().is_none_or(|s| creator::normalizable_lanes(s).is_empty());
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(presets)
            .child(num_row("tools-target", "Target loudness", "LUFS", NumField::Target, t, cx))
            .child(num_row("tools-ceiling", "Peak limit", "dB peak", NumField::Ceiling, t, cx))
            .child(scopes)
            .child(action_button(
                "tools-normalize",
                match t.each {
                    true => "Normalize each track",
                    false => "Normalize whole mix",
                },
                silent.then_some("Nothing to normalize: no unmuted audio track has clips."),
                |this, cx| this.run_normalize(cx),
                cx,
            ))
            .child(dark_help(match t.each {
                true => "Each track is set on its own to the target. Measures before and after. One undo step.",
                false => "The whole mix moves by one amount, so the tracks keep their balance. Measures before and after. One undo step.",
            }))
    }

    /// Captions and Fillers share their pickers and their run row; the rest is their own. With a
    /// transcript to read from, "From the transcript" is the first and default choice (instant);
    /// "Listen again" keeps the sound pickers under it.
    fn listen_body(&self, t: &Tools, fillers: bool, list_w: f32, cx: &mut Context<Self>) -> Div {
        let models = self.tools_models();
        let hears = self.tools_hears();
        let downloading = self.transcript.download.is_some();
        let from_transcript = self.transcript_words().is_some() && !t.relisten;
        let hear_items: Vec<SharedString> = hears.iter().map(|h| creator::hear_label(*h).into()).collect();
        let lang_items: Vec<SharedString> = LANGUAGES.iter().map(|(_, name)| (*name).into()).collect();
        let model_items: Vec<SharedString> = models.iter().map(|m| creator::model_label(m).into()).collect();
        let _ = Hear::Mix;
        let why = match (models.is_empty() && !from_transcript, t.running.is_some()) {
            (true, _) => Some("Install a speech model first"),
            (false, true) => Some("Already listening"),
            _ => None,
        };
        let go = match running_row(t, cx) {
            Some(row) => row,
            None => div().flex().child(action_button(
                if fillers { "tools-find-fillers" } else { "tools-captions" },
                if fillers { "Find filler words" } else { "Generate captions" },
                why,
                move |this, cx| this.start_listening(fillers, cx),
                cx,
            )),
        };
        let busy = t.running.is_some();
        let mut body = div().flex().flex_col().gap(px(4.));
        if self.transcript_words().is_some() {
            body = body.child(kit::segmented_equal(
                "tools-source",
                &[Segment::text("From the transcript"), Segment::text("Listen again")],
                usize::from(t.relisten),
                t.running.is_none(),
                None,
                {
                    let me = cx.entity();
                    move |i, _, app| me.update(app, |this, cx| this.tools_set_relisten(i == 1, cx))
                },
            ));
        }
        if !from_transcript {
            let model_row = drop_row("tools-model", "Model", model_items, t.model, t.open == Some(Cycle::Model), Cycle::Model, list_w, busy, cx);
            let model_tip = models.get(t.model).map(|m| creator::model_tip(m));
            body = body
                .child(drop_row("tools-hear", "Listen to", hear_items, t.hear, t.open == Some(Cycle::Hear), Cycle::Hear, list_w, busy, cx))
                .child(drop_row("tools-lang", "Language", lang_items, t.lang, t.open == Some(Cycle::Language), Cycle::Language, list_w, busy, cx))
                .child(match model_tip {
                    Some(tip) => model_row.id("tools-model-row").tooltip(crate::ui::widgets::overlay_tip_hover("Model", "", Some(&tip))).into_any_element(),
                    None => model_row.into_any_element(),
                });
        }
        if models.is_empty() && !from_transcript {
            body = body.child(missing(
                "No speech model is installed yet. Download one, or put one in the models folder.",
                vec![
                    kit::Button::new("tools-download-model")
                        .label(if downloading { "Downloading\u{2026}" } else { "Download model" })
                        .style(kit::ButtonStyle::Primary)
                        .enabled(!downloading)
                        .build()
                        .when(!downloading, |d| d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_model_download(cx)))),
                    kit::Button::new("tools-models-folder")
                        .label("Open models folder")
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_models_folder(cx))),
                ],
            ));
        }
        // The go button sits above the filler list, so it stays put when the list appears.
        body = body.child(go);
        if fillers && !t.found.is_empty() {
            body = body.child(self.filler_rows(t, cx));
        }
        body.child(dark_help(match (fillers, from_transcript) {
            (true, _) => "Ticked filler words are cut from every track, closing each gap. One undo step.",
            (false, true) => "Instant: the words already in the transcript become one caption track. One undo step.",
            (false, false) => "The words become one caption track. One undo step.",
        }))
    }

    fn filler_rows(&self, t: &Tools, cx: &mut Context<Self>) -> Div {
        let fps = self.fps;
        let ticked = t.found.iter().filter(|(_, on)| *on).count();
        let cut = match ticked {
            0 => "Cut fillers".to_string(),
            1 => "Cut 1 filler".to_string(),
            n => format!("Cut {n} fillers"),
        };
        let cut_why = (ticked == 0).then_some("Tick the filler words to cut");
        let rows: Vec<_> = t
            .found
            .iter()
            .enumerate()
            .map(|(i, (f, on))| {
                let picked = t.pick == i;
                div()
                    .id(("tools-filler", i))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(KEYS_ROW_H))
                    .px(px(6.))
                    .rounded(px(3.))
                    .when(picked, |d| d.bg(rgb(DARK_RAISED())))
                    .child(
                        div()
                            .id(("tools-filler-box", i))
                            .flex_none()
                            .w(px(HIT_MIN / 2.))
                            .flex()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_filler(i, cx)))
                            .child(tick_box(*on)),
                    )
                    .child(
                        div()
                            .id(("tools-filler-go", i))
                            .flex_none()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.seek_filler(i, cx)))
                            .tooltip(crate::ui::widgets::overlay_tip_hover("Go to filler", "", None))
                            .child(dark_row_value(frames_timecode(f.start_f, fps), picked)),
                    )
                    .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(dark_row_label(f.text.to_lowercase(), picked)))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .id("tools-fillers")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child(dark_ghost_button(
                        "tools-fillers-all",
                        "All",
                        "",
                        false,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.set_all_fillers(true, cx)),
                    ))
                    .child(dark_ghost_button(
                        "tools-fillers-none",
                        "None",
                        "",
                        false,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.set_all_fillers(false, cx)),
                    ))
                    .child(gated_button(
                        "tools-cut-fillers",
                        cut.into(),
                        cut_why,
                        |this, cx| this.cut_ticked_fillers(cx),
                        cx,
                    )),
            )
    }

    fn voiceover_body(&self, t: &Tools, list_w: f32, cx: &mut Context<Self>) -> Div {
        let t_ = tokens();
        let voices = self.tools_voices();
        let voice_items: Vec<SharedString> = voices.iter().map(|v| v.name.clone().into()).collect();
        let typing_text = t.typing && t.num.is_none();
        let shown: SharedString = match (typing_text, t.text.is_empty()) {
            (true, _) => format!("{}\u{258f}", t.text).into(),
            (false, true) => "Type what the voice should say".into(),
            (false, false) => t.text.clone().into(),
        };
        let at = self.session.as_ref().map_or(0.0, |s| s.now());
        let why = match (voices.is_empty(), t.text.trim().is_empty()) {
            (true, _) => Some("Install a voice first"),
            (false, true) => Some("Type what the voice should say first"),
            _ => None,
        };
        let go = match running_row(t, cx) {
            Some(row) => row,
            None => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(action_button("tools-speak", "Add voiceover at playhead", why, |this, cx| this.start_voiceover(cx), cx))
                .child(div().flex().justify_center().child(dark_help(format!("Placed at the playhead ({})", creator::clock(at))).w_auto())),
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .id("tools-text")
                    .min_h(px(KEYS_ROW_H * 2.))
                    .p(px(8.))
                    .rounded(px(r::R_CONTROL))
                    .border_1()
                    .border_color(if typing_text { rgb(t_.accent) } else { rgba(t_.line) })
                    .bg(rgb(t_.e2))
                    .cursor_text()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.begin_voiceover_text(cx)))
                    .child(
                        div()
                            .type_style(type_scale::body())
                            .text_color(rgb(if t.text.is_empty() && !typing_text { t_.ink3 } else { t_.ink1 }))
                            .child(shown),
                    ),
            )
            .child(drop_row("tools-voice", "Voice", voice_items, t.voice, t.open == Some(Cycle::Voice), Cycle::Voice, list_w, t.running.is_some(), cx));
        if voices.is_empty() {
            body = body.child(missing(
                "No voice is installed yet. Put a voice in the voices folder.",
                vec![
                    kit::Button::new("tools-voices-folder")
                        .label("Open voices folder")
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_voices_folder(cx))),
                ],
            ));
        }
        body.child(go).child(dark_help("The voice reads your text aloud, then the clip lands on the first free audio track. One undo step."))
    }
}
