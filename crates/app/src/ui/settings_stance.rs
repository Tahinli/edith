//! The settings page (user complaint: "we should have a settings page that
//! we can edit project and editor settings"). Two clearly-headed sections,
//! per the split rule a rubric pass settled:
//!
//! - PROJECT: anything that changes the rendered output or is stored in the
//!   `.edith` file -- resolution, fps, sample-rate, the HDR tonemap, the mix.
//! - EDITOR: anything about this machine, this person, this window --
//!   proxies/auto-proxies (this window's decode/import policy, mirrored into
//!   the project like the resolution row above it once one is open --
//!   [`engine::PlaybackSession::set_proxies`] -- but with a window-level
//!   default, `false`/`true`, that answers before any project is, exactly as
//!   [`inspector.rs`]'s own legacy row already does) and the subtitle font
//!   (a reading preference, kept in `~/.config/edith` like the theme beside
//!   it, per [`crate::load_subtitle_style`], and never written into a
//!   project at all).
//!
//! Every row here reuses an opener or a picker this editor already has
//! ([`Player::open_picker`], [`Player::open_mix`], [`Player::open_subtitle_style`],
//! [`Player::toggle_proxies`], [`Player::toggle_auto_proxies`]) -- no new card
//! body, no second source of truth for what a value is or how it is set.
//! Values apply live through those same doors; there is no Apply button here
//! to be inconsistent with the rest of the room.
//!
//! Seams left for other builders (this task's scope boundary): the HDR
//! mastering-display target row (a monitor fact, not in `inspector.rs`'s own
//! project section yet) has no row here either -- it plugs into
//! `project_section` below, beside the tonemap row it is the pair of. A
//! finer fps/sample-rate/tone picker is the same builder's to extend; this
//! page only ever asks for [`Pick::Fps`]/[`Pick::SampleRate`]/[`Pick::Tone`]
//! exactly as `inspector.rs`'s legacy panel already does.

use crate::*;
use crate::ui::type_scale;

/// A 9px uppercase Archivo section head, `ink3` -- [`dock_stance::section_head`]'s
/// exact shape, repeated rather than imported across a `pub(crate)` seam for
/// one nine-line function.
fn section_head(text: impl Into<SharedString>) -> impl IntoElement {
    let style = type_scale::head();
    div()
        .flex_none()
        .pt(px(4.))
        .font(style.font)
        .text_size(style.size)
        .text_color(rgb(INK3()))
        .child(text.into())
}

/// One row: a ≤16-char label, its current value in mono (units carried,
/// never a bare number -- the density rule this task's charter names), and
/// a hover hint carrying the sentence a label must not. [`dock_stance::ghost_verb`]'s
/// anatomy, with a live value where that row has a chord badge -- this room
/// is a picker's own door, not a keystroke's.
fn row(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    row_ink(id, label_text, value, hint, player, on_click, INK1())
}

/// [`row`] with the value's ink chosen by the caller -- [`INK4`] for a value
/// that is not really a value yet (nothing to show until a project opens),
/// so it reads as absent rather than as a fifth real setting.
fn row_ink(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    value_ink: u32,
) -> impl IntoElement {
    let value: SharedString = value.into();
    let hint: SharedString = hint.into();
    let label_style = type_scale::label(type_scale::LABEL_ROW_PX, gpui::FontWeight::MEDIUM);
    let value_style = type_scale::mono(type_scale::LABEL_ROW_PX, gpui::FontWeight::MEDIUM);
    let exporting = player.exporting().is_some();
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .h(px(CONTROL_H))
        .px(px(8.))
        .rounded(px(3.))
        .tooltip(move |_, cx| cx.new(|_| Tip(hint.clone())).into())
        .when(exporting, |d| d.opacity(0.4).cursor_not_allowed())
        .when(!exporting, |d| {
            d.cursor_pointer().hover(|s| s.bg(rgb(DARK_RAISED()))).on_click(on_click)
        })
        .child(
            div()
                .font(label_style.font)
                .text_size(label_style.size)
                .text_color(rgb(INK2()))
                .child(label_text),
        )
        .child(
            div()
                .flex_none()
                .font(value_style.font)
                .text_size(value_style.size)
                .text_color(rgb(value_ink))
                .child(value),
        )
}

/// PROJECT: the canvas every clip is composed onto, the rate it is cut at,
/// the HDR rendition it is watched in, and the mix everything sums into --
/// [`inspector.rs`]'s own four-plus-Mix list, ported rather than
/// reimplemented (same `Pick`/`open_mix` doors).
fn project_section(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    // corner-cut: no genuine default resolution or fps exists before either
    // a project opens or a pick is made, so the empty state is a bare dash
    // in INK4 -- quieter than a real value, never a noun that reads as one
    // (the "Size"/"Rate"/"HDR" placeholders this row used to fall back to).
    let resolution = player.session.as_ref().map(PlaybackSession::resolution);
    let (res_val, res_ink) = match resolution.or(player.pending_settings.0) {
        Some((_, h)) => (format!("{h}p"), INK1()),
        None => ("—".to_string(), INK4()),
    };
    let (fps_val, fps_ink) = match (player.session.is_some(), player.pending_settings.1) {
        (true, _) => (format!("{} fps", fps_label(player.fps)), INK1()),
        (false, Some(fps)) => (format!("{} fps", fps_label(fps)), INK1()),
        (false, None) => ("—".to_string(), INK4()),
    };
    // Sample rate keeps its "Source" fallback: unlike resolution/fps it
    // states a real behaviour (derives from the first audio source) rather
    // than standing in for a value with nothing behind it.
    let rate_val = match player.session.as_ref().map_or(player.pending_settings.2, |s| s.sample_rate()) {
        Some(rate) => format!("{rate} Hz"),
        None => "Source".to_string(),
    };
    let (tone_val, tone_ink) = match &player.session {
        Some(session) => (format!("HDR {}", tone_label(session.tone())), INK1()),
        None => ("—".to_string(), INK4()),
    };
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(section_head("PROJECT · stored in the .edith file"))
        .child(row_ink(
            "settings-resolution",
            "Resolution",
            res_val,
            "the canvas every clip is composed onto, and the size the export comes out at",
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Resolution, event.position(), cx)),
            res_ink,
        ))
        .child(row_ink(
            "settings-fps",
            "Frame rate",
            fps_val,
            "the rate the whole timeline is cut and written at",
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Fps, event.position(), cx)),
            fps_ink,
        ))
        .child(row(
            "settings-sample-rate",
            "Sample rate",
            rate_val,
            "the rate the project's own sound mix is run at; source derives it from the first audio source",
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::SampleRate, event.position(), cx)),
        ))
        // hook: the mastering-display target (a monitor peak-nits fact, not
        // yet in `inspector.rs`'s own project section) is this row's pair --
        // another builder's row, plugged in right here once it exists.
        .child(row_ink(
            "settings-tonemap",
            "HDR tonemap",
            tone_val,
            "which rendition HDR media is watched in; SDR media is untouched",
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Tone, event.position(), cx)),
            tone_ink,
        ))
        .child(row(
            "settings-mix",
            "Mix",
            "Tracks…",
            "a fader per track and the limiter over the sum of them",
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.open_mix(None, cx)),
        ))
}

/// EDITOR: this window's own decode/import policy and this person's own
/// reading preference. The subtitle font never touches the `.edith` file;
/// the proxy switches do mirror into it once a project is open (they are
/// project state too), but default from the window rather than a project
/// pick, which is what keeps them answerable with nothing open.
fn editor_section(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(section_head("EDITOR · this machine, this window"))
        .child(row(
            "settings-proxies",
            "Proxies",
            match player.proxies_on {
                true => format!("On{}", player.proxy_tail()),
                false => "Off".to_string(),
            },
            "cuts on small stand-ins of the big films; the sound and every export stay the film's own -- default kept in ~/.config/edith, the pick for an open project in its own .edith",
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_proxies(cx)),
        ))
        .child(row(
            "settings-auto-proxies",
            "Auto proxies",
            if player.auto_proxies_on { "On" } else { "Off" },
            "makes a stand-in for every big film as it is imported -- default kept in ~/.config/edith, the pick for an open project in its own .edith",
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_auto_proxies(cx)),
        ))
        .child(row(
            "settings-subtitle-style",
            "Subtitle font",
            "Font…",
            "the cue plate's own font and size -- kept in ~/.config/edith, never in a project",
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.open_subtitle_style(cx)),
        ))
}

/// The page itself: a modal card, the same drag-scrim-and-click-away shape
/// every other card in this room takes ([`crate::ui::cards`]'s
/// eq/color/transform/speed cards) -- closed by a click away or `esc`,
/// through [`Player::close_card`], exactly like them.
pub(crate) fn render(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    drag_scrim(cx)
        .flex()
        .justify_center()
        .items_center()
        .bg(rgba(SCRIM()))
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
                .id("settings-card")
                .w_full()
                .max_w(px(360.))
                .max_h(relative(0.86))
                .on_mouse_down(MouseButton::Left, swallow)
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(6.))
                .bg(rgb(DARK_PANEL()))
                .border_1()
                .border_color(rgba(DARK_SEAM()))
                .child({
                    let head = type_scale::head();
                    div()
                        .flex_none()
                        .flex()
                        .justify_between()
                        .items_baseline()
                        .font(head.font)
                        .text_size(head.size)
                        .text_color(rgb(INK3()))
                        .child("SETTINGS")
                        .child(
                            div()
                                .font(type_scale::mono(type_scale::CHORD_METADATA_MIN_PX, gpui::FontWeight::MEDIUM).font)
                                .text_size(px(type_scale::CHORD_METADATA_MIN_PX))
                                .text_color(rgb(INK3()))
                                .child(player.keymap.display(ActionId::Settings)),
                        )
                })
                .child(
                    div()
                        .id("settings-rows")
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(project_section(player, cx))
                        .child(editor_section(player, cx)),
                ),
        )
}
