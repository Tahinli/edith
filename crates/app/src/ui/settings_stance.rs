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
//! Seams left for other builders (this task's scope boundary): a finer
//! fps/sample-rate/tone picker is the same builder's to extend; this page
//! only ever asks for [`Pick::Fps`]/[`Pick::SampleRate`]/[`Pick::Tone`]
//! exactly as `inspector.rs`'s legacy panel already does.
//!
//! The HDR reference numbers beside the tonemap row (`HDR reference` and
//! `Content light`) are the *file's* declared light level
//! ([`engine::colorspace::ContentLight`]), not a monitor fact -- read-only,
//! since nothing in [`engine::project::Project`] holds a place to persist an
//! override into.

use crate::ui::hitmap;
use crate::ui::type_scale;
use crate::*;
use engine::colorspace::ContentLight;

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

/// [`row`] for a setting a chord also reaches: the stroke rides in the row's
/// own right-hand side, ahead of the value, in the mono chord ink every other
/// darkroom surface wears it in (DESIGN §4 -- "every command wears its chord,
/// everywhere it appears"). Three settings arrived here from the spine when
/// the rail ran out of height (proxies, auto-proxies, the palette); each one
/// had its chord on the rail's badge, and none of them may lose it on the way
/// over.
fn row_keyed(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    action: ActionId,
    player: &Player,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    row_full(
        id,
        label_text,
        value,
        hint,
        Some(player.keymap.chord(action).into()),
        player,
        on_click,
        INK1(),
    )
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
    row_full(
        id, label_text, value, hint, None, player, on_click, value_ink,
    )
}

/// The one row body all three wrappers share. `chord` is the stroke that also
/// reaches this setting, drawn in mono `ink3` *beside* the value rather than
/// over it -- three settings came here off the spine and each one keeps the
/// badge it wore there (DESIGN §4).
#[allow(clippy::too_many_arguments)]
fn row_full(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    chord: Option<SharedString>,
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
            d.cursor_pointer()
                .hover(|s| s.bg(rgb(DARK_RAISED())))
                .on_click(on_click)
        })
        .children(hitmap::control(id, label_text, !exporting))
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
                .flex()
                .items_baseline()
                .gap(px(8.))
                .children(chord.map(|chord| {
                    let chord_style = type_scale::mono(
                        type_scale::CHORD_METADATA_MIN_PX,
                        gpui::FontWeight::MEDIUM,
                    );
                    div()
                        .font(chord_style.font)
                        .text_size(chord_style.size)
                        .text_color(rgb(INK3()))
                        .child(chord)
                }))
                .child(
                    div()
                        .font(value_style.font)
                        .text_size(value_style.size)
                        .text_color(rgb(value_ink))
                        .child(value),
                ),
        )
}

/// [`row_ink`] with no opener: a readout rather than a picker, for a value
/// this editor cannot set because nothing behind it can hold a pick --
/// [`engine::playback::PlaybackSession`] has no field to persist one into.
/// No `cursor_pointer`, no hover paint, no click: the row must not read as a
/// button that does nothing when pressed. The hint still names where the
/// number came from, since a reader who wants that detail should get it from
/// the same door every other row's hint answers from.
fn row_static(
    id: &'static str,
    label_text: &'static str,
    value: impl Into<SharedString>,
    hint: &'static str,
    value_ink: u32,
) -> impl IntoElement {
    let value: SharedString = value.into();
    let hint: SharedString = hint.into();
    let label_style = type_scale::label(type_scale::LABEL_ROW_PX, gpui::FontWeight::MEDIUM);
    let value_style = type_scale::mono(type_scale::LABEL_ROW_PX, gpui::FontWeight::MEDIUM);
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
    let rate_val = match player
        .session
        .as_ref()
        .map_or(player.pending_settings.2, |s| s.sample_rate())
    {
        Some(rate) => format!("{rate} Hz"),
        None => "Source".to_string(),
    };
    let (tone_val, tone_ink) = match &player.session {
        Some(session) => (format!("HDR {}", tone_label(session.tone())), INK1()),
        None => ("—".to_string(), INK4()),
    };
    // The reference row's own numbers: what source 0's file actually declared
    // ([`engine::colorspace::ContentLight`], read at open by
    // [`engine::playback::PlaybackSession::content_light`]) -- read-only,
    // since nothing in [`engine::project::Project`] holds an override to
    // persist one into. `—` for a project with no session, and for a session
    // whose file is SDR or simply declared nothing (most files declare
    // nothing here at all).
    let light = player.session.as_ref().map_or(
        ContentLight::default(),
        engine::playback::PlaybackSession::content_light,
    );
    let (master_val, master_ink) = match (&player.session, light.mastering_max) {
        (Some(_), Some(nits)) => (format!("{nits:.0} nits"), INK1()),
        _ => ("—".to_string(), INK4()),
    };
    let (content_val, content_ink) = match (&player.session, light.max_cll, light.max_fall) {
        (Some(_), Some(cll), Some(fall)) => (format!("{cll:.0}/{fall:.0} nits"), INK1()),
        (Some(_), Some(cll), None) => (format!("{cll:.0} nits"), INK1()),
        _ => ("—".to_string(), INK4()),
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
        .child(row_ink(
            "settings-tonemap",
            "HDR tonemap",
            tone_val,
            "which rendition HDR media is watched in; SDR media is untouched",
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| this.open_picker(Pick::Tone, event.position(), cx)),
            tone_ink,
        ))
        // The tonemap row's pair: what the file actually declared, read-only
        // ([`row_static`] -- there is nowhere in the project to persist a
        // pick even if this page offered one).
        .child(row_static(
            "settings-hdr-reference",
            "HDR reference",
            master_val,
            "the mastering display's own peak white, in cd/m^2, as the grade declared it -- what a 'Reference' tonemap targets before it ever looks at this film's own pixels",
            master_ink,
        ))
        .child(row_static(
            "settings-content-light",
            "Content light",
            content_val,
            "MaxCLL/MaxFALL: the brightest single pixel and the brightest frame average measured in the finished encode -- what a 'Reference' tonemap actually rolls off from when the file declares it, ahead of the mastering peak above",
            content_ink,
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
        .child(row_keyed(
            "settings-proxies",
            "Proxies",
            match player.proxies_on {
                true => format!("On{}", player.proxy_tail()),
                false => "Off".to_string(),
            },
            "cuts on small stand-ins of the big films; the sound and every export stay the film's own -- default kept in ~/.config/edith, the pick for an open project in its own .edith",
            ActionId::ToggleProxies,
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_proxies(cx)),
        ))
        .child(row_keyed(
            "settings-auto-proxies",
            "Auto proxies",
            if player.auto_proxies_on { "On" } else { "Off" },
            "makes a stand-in for every big film as it is imported -- default kept in ~/.config/edith, the pick for an open project in its own .edith",
            ActionId::ToggleAutoProxies,
            player,
            cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_auto_proxies(cx)),
        ))
        // The room's own palette, moved off the spine (user 2026-08-21: the
        // rail was too crowded to fit its own rows) -- a preference kept in
        // ~/.config/edith, so the EDITOR section is where it belongs; the
        // same `Pick::Theme` list `ActionId::Theme` has always opened.
        .child(row_keyed(
            "settings-theme",
            "Palette",
            crate::ui::theme::active().label(),
            "the room's own greys; the film's inks are extracted, never picked",
            ActionId::Theme,
            player,
            cx.listener(|this, event: &ClickEvent, _, cx| {
                this.open_picker(Pick::Theme, event.position(), cx)
            }),
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

/// The page itself: a plate in the room's own below-picture footprint,
/// closed by a click away or `esc` through [`Player::close_card`] like every
/// other card here.
///
/// It used to be a centred modal over a dimming scrim, which put the whole
/// settings sheet -- and every picker opened off one of its rows -- straight
/// over the picture: DESIGN §11 check 6's one hard occlusion rule, and half
/// of the user's 2026-08-21 report about menus "not aligning with our
/// design". Anchored to [`crate::ui::stance::below_picture_floor`] it sits
/// over the bench and ledger exactly as the export plate and the menus
/// already do, and the catcher behind it paints nothing rather than tinting
/// the frame.
pub(crate) fn render(
    player: &Player,
    window_size: Size<Pixels>,
    cx: &mut Context<Player>,
) -> impl IntoElement {
    let floor = crate::ui::stance::below_picture_floor(
        f32::from(window_size.height),
        player.split_px(Split::Bench, window_size),
    );
    // Below the time band, not merely below the picture: the transport and
    // the Export chip live on that band and a sheet over them hides controls
    // the editor is still using. Bench + ledger is this page's footprint.
    let top = floor + crate::ui::stance::TIME_BAND_H;
    let room = f32::from(window_size.height) - top;
    drag_scrim(cx)
        .flex()
        .justify_center()
        .items_end()
        .pt(px(top + 6.))
        .pb(px(6.))
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
                // The whole footprint's width, two columns inside: at 720p a
                // 420px single column put Mix, the palette and the subtitle
                // font below a fold with no visible scrollbar -- settings an
                // editor cannot see are settings only a chord reaches, which
                // is the defect class this page exists to close.
                .w_full()
                .max_h(px((room - 12.).max(120.)))
                .on_mouse_down(MouseButton::Left, swallow)
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(8.))
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
                                .font(
                                    type_scale::mono(
                                        type_scale::CHORD_METADATA_MIN_PX,
                                        gpui::FontWeight::MEDIUM,
                                    )
                                    .font,
                                )
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
                        .gap(px(16.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(project_section(player, cx)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .border_l_1()
                                .border_color(rgb(DARK_HAIRLINE()))
                                .pl(px(12.))
                                .child(editor_section(player, cx)),
                        ),
                ),
        )
}
