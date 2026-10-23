//! The dock's Transcript tab: the timeline's speech as paragraphs of words. A press on a word
//! takes the playhead there and starts a selection; drag or shift-click extends it; Delete cuts
//! it from every lane. Fillers are dimmed, the word being spoken is washed, search matches are
//! lit. Every state is a background on a word whose box never changes, so nothing moves.

use crate::layout::redesign as r;
use crate::ui::hitmap;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
use crate::ui::stance::{Surface, is_focus_cycle_key};
use crate::ui::theme::{tokens, with_alpha};
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use engine::speech;

pub(crate) fn transcript_tab(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let tr = &player.transcript;
    let fps = player.fps;
    let frame = player.transcript_frame();
    let playing = player.session.as_ref().is_some_and(|s| s.is_playing());
    let words = tr.words.as_deref();
    let editing = tr.search_edit;
    let has_query = !tr.search.is_empty();
    let fillers = words.map_or(0, |w| w.iter().filter(|x| speech::is_filler(&x.text)).count());
    let match_list = tr.match_list();
    let matches = match_list.len();
    let at = tr.search_at.and_then(|c| match_list.iter().position(|&i| i == c));
    let selection = tr.selection();
    let live = words.is_some() && !tr.stale;

    let search = div()
        .id("transcript-search")
        .flex_1()
        .min_w(px(0.))
        .cursor_text()
        .h(px(r::CONTROL_M))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(r::R_CONTROL))
        .border_1()
        .border_color(if editing { rgb(t.accent).into() } else { gpui::transparent_black() })
        .bg(rgb(t.e2))
        .type_style(type_scale::body())
        .text_color(rgb(if editing || has_query { t.ink1 } else { t.ink3 }))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            window.focus(&this.focus_inspector);
            this.transcript.search_edit = true;
            cx.notify();
        }))
        .tooltip(crate::ui::widgets::tip_hover("Search words", "type to find, ↵ jumps to the next", None))
        .children(hitmap::control("transcript.search", "Search words", true))
        .child(Icon::Search.el(IconSize::Inline, t.ink3))
        .child(div().flex_1().min_w(px(0.)).truncate().child(match has_query {
            false => "Search words".to_string(),
            true => tr.search.clone(),
        }))
        .children(has_query.then(|| {
            div()
                .id("transcript-search-clear")
                .flex_none()
                .cursor_pointer()
                .rounded(px(3.))
                .hover(|s| s.bg(rgba(with_alpha(0xffffff, 0x14))))
                .child(Icon::X.el(IconSize::Inline, t.ink3))
                .tooltip(crate::ui::widgets::tip_hover("Clear the search", "Esc", None))
                .children(hitmap::control("transcript.search.clear", "Clear search", true))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.clear_search();
                    cx.stop_propagation();
                    cx.notify();
                }))
        }));

    let step = |id: &'static str, icon: Icon, forward: bool, cx: &mut Context<Player>| {
        let (tip, keys, name) = match forward {
            true => ("Next match", "Enter", "Next match"),
            false => ("Previous match", "Shift+Enter", "Previous match"),
        };
        Button::new(id)
            .icon(icon)
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Small)
            .enabled(live && matches > 0)
            .build()
            .tooltip(crate::ui::widgets::tip_hover(tip, keys, (matches == 0).then_some("Type a word to search for first")))
            .children(hitmap::control(id, name, live && matches > 0))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_inspector);
                this.step_match(forward, cx);
            }))
    };
    let prev = step("transcript-prev", Icon::ChevronUp, false, cx);
    let next = step("transcript-next", Icon::ChevronDown, true, cx);

    let delete = Button::new("transcript-delete")
        .icon(Icon::Trash2)
        .style(ButtonStyle::Ghost)
        .enabled(live && selection.is_some())
        .build()
        .tooltip(crate::ui::widgets::tip_hover("Cut the selected words", "Delete", Some("Ripples every track as one undo step")))
        .children(hitmap::control("transcript.delete", "Cut selected words", live && selection.is_some()))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            window.focus(&this.focus_inspector);
            this.delete_selected_words(cx);
        }));

    let remove = Button::new("transcript-fillers")
        .label(if fillers > 0 { format!("Remove fillers ({fillers})") } else { "Remove fillers".to_string() })
        .size(ButtonSize::Small)
        .enabled(live && fillers > 0)
        .build()
        .tooltip(crate::ui::widgets::tip_hover(
            "Remove filler words",
            "",
            Some(if fillers > 0 { "um, uh and the like, from every track" } else { "No filler words left" }),
        ))
        .children(hitmap::control("transcript.fillers", "Remove fillers", live && fillers > 0))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.remove_transcript_fillers(cx)));

    let again = Button::new("transcript-again")
        .label("Transcribe again")
        .size(ButtonSize::Small)
        .min_w(128.)
        .build()
        .tooltip(crate::ui::widgets::tip_hover("Transcribe again", "", Some("The timeline changed, so these words no longer line up")))
        .children(hitmap::control("transcript.again", "Transcribe again", true))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_transcribing(cx)));

    let summary = match (words, tr.stale) {
        (None, _) => String::new(),
        (Some(_), true) => "Timeline changed since transcribing".to_string(),
        (Some(_), false) if has_query => match (matches, at) {
            (0, _) => "No matches".to_string(),
            (n, Some(p)) => format!("{} of {n}", p + 1),
            (n, None) => format!("{n} match{}", if n == 1 { "" } else { "es" }),
        },
        (Some(w), false) => match selection {
            Some((a, b)) => format!("{} selected", b - a + 1),
            None => format!("{} words", w.len()),
        },
    };

    let body: AnyElement = match words {
        _ if tr.running.is_some() => running_view(player, cx).into_any_element(),
        None => empty_view(player, cx).into_any_element(),
        Some(words) => {
            let active = (playing || frame > 0).then(|| tr.playing_word(frame, fps)).flatten();
            let paras = tr.paragraphs(fps);
            if playing
                && let Some(i) = active
                && let Some(p) = paras.iter().position(|&(a, b)| (a..=b).contains(&i))
                && tr.followed.get() != Some(p)
            {
                tr.followed.set(Some(p));
                tr.scroll.scroll_to_item(p);
            }
            let rows = paras.iter().map(|&(a, b)| paragraph(player, words, a, b, active, selection, cx).into_any_element());
            div()
                .id("transcript-words")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&tr.scroll)
                .flex()
                .flex_col()
                .gap(px(10.))
                .pb(px(12.))
                .children(rows)
                .into_any_element()
        }
    };

    div()
        .id("dock-transcript")
        .track_focus(&player.focus_inspector)
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if is_focus_cycle_key(event.keystroke.key.as_str()) {
                this.focus_surface(Surface::Bench, window, cx);
                cx.stop_propagation();
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_word(cx)))
        .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_word(cx)))
        .border_1()
        .border_color(gpui::transparent_black())
        .flex_1()
        .min_h(px(0.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(12.))
        .pt(px(8.))
        .when(words.is_some() && tr.running.is_none(), |d| {
            d.child(div().flex_none().flex().items_center().gap(px(4.)).child(search).child(prev).child(next).child(delete)).child(
                div()
                    .flex_none()
                    .min_h(px(r::CONTROL_S))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        // A stale line may wrap rather than cut its words short.
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .type_style(type_scale::caption())
                            .text_color(rgb(if tr.stale { t.warn } else { t.ink3 }))
                            .child(summary),
                    )
                    .child(if tr.stale { again.into_any_element() } else { remove.into_any_element() }),
            )
        })
        .child(body)
}

/// One paragraph: its start time, then the words wrapping like text.
fn paragraph(
    player: &Player,
    words: &[engine::speech::Spoken],
    first: usize,
    last: usize,
    active: Option<usize>,
    selection: Option<(usize, usize)>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let t = tokens();
    let fps = player.fps;
    let secs = f64::from(words[first].start_f) / fps;
    let stamp = format!("{}:{:02}", (secs / 60.0) as u32, (secs % 60.0) as u32);
    let stale = player.transcript.stale;
    div()
        .id(("transcript-para", first))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().type_style(type_scale::caption()).text_color(rgb(t.ink3)).child(stamp))
        .child(
            div().flex().flex_wrap().children((first..=last).map(|i| {
                let w = &words[i];
                let filler = speech::is_filler(&w.text);
                let selected = selection.is_some_and(|(a, b)| (a..=b).contains(&i));
                let hit = player.transcript.matches(i);
                let now = active == Some(i);
                let fill = if selected {
                    Some(with_alpha(t.accent, 0x66))
                } else if now {
                    Some(with_alpha(t.accent, 0x3d))
                } else if hit {
                    Some(with_alpha(t.warn, if player.transcript.search_at == Some(i) { 0xaa } else { 0x55 }))
                } else {
                    None
                };
                let ink = match (stale, filler, now || selected) {
                    (true, ..) => t.ink4,
                    (_, _, true) => t.ink1,
                    (_, true, _) => t.ink4,
                    _ => t.ink2,
                };
                div()
                    .id(("transcript-word", i))
                    .flex_none()
                    .px(px(3.))
                    .py(px(1.))
                    // A selected run is one wash with square ends, so the words join like text.
                    .when(!selected, |d| d.rounded(px(3.)))
                    .cursor_pointer()
                    .type_style(type_scale::body())
                    .text_color(rgb(ink))
                    .when_some(fill, |d, f| d.bg(rgba(f)))
                    .when(filler && !stale, |d| d.italic())
                    .when(fill.is_none(), |d| d.hover(move |s| s.bg(rgba(with_alpha(0xffffff, 0x14)))))
                    .when(stale, |d| d.tooltip(crate::ui::widgets::tip_hover("Transcribe again", "", Some("Transcribe again to use the words"))))
                    .when(!stale, |d| {
                        d.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                window.focus(&this.focus_inspector);
                                this.transcript.search_edit = false;
                                this.press_word(i, ev.modifiers.shift, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                            if ev.dragging() {
                                this.drag_word(i, cx);
                            }
                        }))
                    })
                    .child(w.text.trim().to_string())
            })),
        )
}

/// Before the first listen: what this does, and the one button that starts it. With no speech
/// model on the computer it says so plainly and offers to download one.
fn empty_view(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let ready = speech::available();
    let no_model = speech::stt_bin().is_ok() && speech::installed_models().is_empty();
    let not_built = speech::stt_bin().is_err();
    if let Some((job, _)) = &player.transcript.download {
        return download_view(job.fraction(), cx).into_any_element();
    }
    let note = match (&ready, player.transcript.status.as_str()) {
        (Err(_), status) if no_model && !status.is_empty() => format!("{status}. You can also put a model in the models folder yourself."),
        (Err(_), _) if no_model => {
            "Transcribing needs a speech model on this computer. It is a one-time download of about 290 MB, kept here, and nothing you edit ever leaves the machine.".to_string()
        }
        (Err(_), _) if not_built => "Speech transcription isn't included in this copy of edith.".to_string(),
        (Err(why), _) => why.clone(),
        (_, "") => "Listen to the timeline once, then edit it by its words: click a word to go there, select and delete to cut.".to_string(),
        (_, status) => status.to_string(),
    };
    div()
        .id("transcript-empty")
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(12.))
        .child(Icon::Captions.el(IconSize::Hero, t.ink3))
        .child(div().text_center().type_style(type_scale::body()).text_color(rgb(t.ink2)).child(note))
        .children((!not_built).then(|| match no_model {
            true => Button::new("transcript-download")
                .label("Download model")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Large)
                .build()
                .tooltip(crate::ui::widgets::tip_hover("Download the speech model", "", Some("About 290 MB, once; it stays on this computer")))
                .children(hitmap::control("transcript.download", "Download model", true))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_model_download(cx)))
                .into_any_element(),
            false => Button::new("transcript-start")
                .label("Transcribe")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Large)
                .enabled(ready.is_ok() && player.session.is_some())
                .build()
                .tooltip(crate::ui::widgets::tip_hover("Transcribe the timeline", "", Some("Runs the speech model here; nothing leaves the machine")))
                .children(hitmap::control("transcript.start", "Transcribe", ready.is_ok()))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_transcribing(cx)))
                .into_any_element(),
        }))
        .children(no_model.then(|| {
            Button::new("transcript-models-folder")
                .label("Open models folder")
                .style(ButtonStyle::Ghost)
                .build()
                .tooltip(crate::ui::widgets::tip_hover("Open the models folder", "", Some("Put a model folder here yourself instead of downloading")))
                .children(hitmap::control("transcript.models", "Open models folder", true))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_models_folder(cx)))
        }))
        .into_any_element()
}

/// While the speech model downloads: how far, and Cancel.
fn download_view(fraction: f32, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let mb = speech::model_bytes("base") as f32 * fraction / 1_000_000.;
    div()
        .id("transcript-downloading")
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(12.))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .type_style(type_scale::body())
                .text_color(rgb(t.ink2))
                .child("Downloading the speech model")
                .child(format!("{mb:.0} of {:.0} MB", speech::model_bytes("base") as f32 / 1_000_000.)),
        )
        .child(
            div()
                .w_full()
                .h(px(6.))
                .rounded(px(2.))
                .bg(rgb(t.e2))
                .child(div().h_full().w(relative(fraction.clamp(0.0, 1.0))).rounded(px(2.)).bg(rgb(t.accent))),
        )
        .child(
            Button::new("transcript-download-cancel")
                .label("Cancel")
                .build()
                .tooltip(crate::ui::widgets::tip_hover("Stop downloading", "", None))
                .children(hitmap::control("transcript.download.cancel", "Cancel download", true))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_model_download(cx))),
        )
}

/// While the model runs: the same top-aligned progress block the export shows, then Cancel.
fn running_view(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let t = tokens();
    let fraction = player.transcript.running.as_ref().map_or(0.0, |r| r.job.fraction()).clamp(0., 1.);
    div()
        .id("transcript-running")
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(10.))
        .py(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .type_style(type_scale::title())
                .text_color(rgb(t.ink1))
                .child(Icon::Captions.el(IconSize::Toolbar, t.accent))
                .child("Transcribing"),
        )
        .child(
            div().h(px(6.)).rounded(px(3.)).bg(rgb(t.e3)).child(div().h_full().w(relative(fraction)).rounded(px(3.)).bg(rgb(t.accent))),
        )
        .child(
            div()
                .type_style(type_scale::body())
                .text_color(rgb(t.ink1))
                .child(format!("Transcribing \u{b7} {:.0}%", fraction * 100.)),
        )
        .child(
            div().flex().pt(px(6.)).child(
                Button::new("transcript-cancel")
                    .label("Cancel")
                    .build()
                    .tooltip(crate::ui::widgets::tip_hover("Stop transcribing", "", None))
                    .children(hitmap::control("transcript.cancel", "Cancel", true))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_transcribing(cx))),
            ),
        )
}
