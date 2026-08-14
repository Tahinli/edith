//! The right-hand region: what is selected, and what can be done to it.
//!
//! Every consumer editor has this column, and edith's settings used to be
//! floating cards *over the timeline* instead -- so adjusting a clip hid the
//! very clip being adjusted. The cards are docked in here now: the equalizer,
//! the grade, the speed, the silence scan and the mix are children of this
//! panel, and opening one occludes zero timeline pixels
//! (`an_inspector_section_occludes_no_timeline`).

use crate::*;
use crate::ui::toolbar::{TONE_SLOT_W, ZOOM_SLOT_W};
use crate::ui::widgets::*;

impl Player {
    /// The inspector column. Two sections that are always there -- the
    /// selection's and the project's -- and, over them, whichever settings card
    /// is open, which is a section of this panel rather than a sheet over the
    /// window.
    pub(crate) fn inspector(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let width = inspector_w(f32::from(viewport.width));
        // The cards measure themselves against the room they are given, and the
        // room they are given is this column -- not the window.
        let room = size(px(width), viewport.height);
        // The same affordance the lanes carry, on the column that needs it for
        // the same reason: at the 640x360 floor the project section is below the
        // fold, and a section nobody knows is there is a section that is not
        // there. Rows here are not a fixed height, so the column is asked how
        // far it can still be taken rather than counted -- and it answers with
        // the previous frame's layout, which is what makes the line live.
        let can_scroll = f32::from(self.inspector_scroll.max_offset().height) > 1.;
        let below = px_below(
            f32::from(self.inspector_scroll.max_offset().height),
            f32::from(self.inspector_scroll.offset().y),
        );
        div()
            .id("inspector")
            .flex_none()
            .w(px(width))
            .h_full()
            // The bed the docked cards are placed against: `scrim()` is
            // `absolute().inset_0()`, so it covers this panel and nothing else.
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(BG_PANEL()))
            .border_l_1()
            .border_color(rgb(STROKE_DIVIDER()))
            .child(section_head("Inspector"))
            .child(
                div()
                    .id("inspector-rows")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.inspector_scroll)
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(8.))
                    .child(self.selection_section(cx))
                    .child(self.project_section(cx)),
            )
            .when(can_scroll, |d| {
                d.child(
                    div()
                        .flex_none()
                        .h(px(LABEL_H))
                        .flex()
                        .items_center()
                        .justify_end()
                        .px(px(8.))
                        .text_size(px(10.))
                        .text_color(rgb(match below > 1. {
                            true => ACCENT_PRIMARY(),
                            false => FG_SECONDARY(),
                        }))
                        .child(match below > 1. {
                            true => "more below — scroll the inspector",
                            false => "the end — scroll up for the rest",
                        }),
                )
            })
            .children(self.eq_card(room, cx))
            .children(self.color_card(cx))
            .children(self.speed_card(cx))
            .children(self.silence_card(cx))
            .children(self.mix_card(cx))
    }

    /// What is marked, and the settings that belong to *it*. A dump of every
    /// property regardless of what is selected is the anti-pattern this avoids:
    /// with nothing marked the section says how to get one, and each button is
    /// dimmed with the oracle's reason rather than missing.
    fn selection_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.selected.and_then(|(lane, idx)| {
            let session = self.session.as_ref()?;
            let clip = session.lane_clips(lane).get(idx).copied()?;
            let source = session.sources().get(clip.source);
            let image = source.is_some_and(|s| engine::is_image(&s.path));
            Some((
                lane,
                clip,
                source.map_or_else(|| lane.label().to_string(), |s| file_name(&s.path)),
                image,
            ))
        });
        let head = match &picked {
            Some((lane, _, name, _)) => format!("{} · {}", lane.label(), name),
            None => "Nothing selected".to_string(),
        };
        let detail = match &picked {
            Some((_, clip, _, _)) => format!(
                "{} → {} · {} · {}",
                timecode(f64::from(clip.start) / self.fps, self.fps),
                timecode(f64::from(clip.start + clip.frames()) / self.fps, self.fps),
                span_label(f64::from(clip.frames()) / self.fps),
                clip.speed,
            ),
            None => "Click a clip on the timeline to see its settings".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .rounded(px(4.))
            .bg(rgb(BG_HOVER_DIM()))
            .child(
                div()
                    .flex_none()
                    .truncate()
                    .text_color(rgb(match picked.is_some() {
                        true => FG_PRIMARY(),
                        false => FG_SECONDARY(),
                    }))
                    // The kind's own colour, the same one its clip wears on the
                    // timeline: the panel and the box are about the same thing.
                    .when_some(picked.as_ref(), |d, (lane, _, _, image)| {
                        d.border_l_2()
                            .border_color(rgb(clip_kind(lane.kind, *image)))
                            .pl(px(6.))
                    })
                    .child(head),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child(detail),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(self.action_control(
                        "inspect-speed",
                        0.,
                        BG_RAISED(),
                        None,
                        "Speed",
                        "how fast this clip and its group play",
                        ActionId::Speed,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_speed(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-color",
                        0.,
                        BG_RAISED(),
                        None,
                        "Colour",
                        "exposure, contrast, saturation and temperature",
                        ActionId::Color,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_color(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-eq",
                        0.,
                        BG_RAISED(),
                        None,
                        "Equalizer",
                        "the bands this clip's sound is filtered through",
                        ActionId::Equalizer,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_eq(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-silence",
                        0.,
                        BG_RAISED(),
                        None,
                        "Silence",
                        "finds the quiet stretches and cuts or speeds them",
                        ActionId::Silence,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_silence(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-fit",
                        0.,
                        BG_RAISED(),
                        None,
                        "Fit",
                        "how this picture is placed on the project canvas",
                        ActionId::Fit,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.cycle_fit(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-delete",
                        0.,
                        BG_RAISED(),
                        None,
                        "Delete",
                        "takes this clip off the timeline",
                        ActionId::Delete,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.delete_selected(cx)),
                    )),
            )
    }

    /// The settings that are the *project's* rather than any clip's: the canvas
    /// every clip is composed onto, the rate it is cut at, the HDR rendition it
    /// is watched in, and the mix everything sums into. They used to be four
    /// more buttons in a toolbar row that already did not fit the floor.
    fn project_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let resolution = self.session.as_ref().map(PlaybackSession::resolution);
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(8.))
            .rounded(px(4.))
            .bg(rgb(BG_HOVER_DIM()))
            .child(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(rgb(FG_SECONDARY()))
                    .child("PROJECT"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(self.action_control(
                        "resolution",
                        ZOOM_SLOT_W,
                        BG_RAISED(),
                        None,
                        resolution.map_or_else(|| "Size".to_string(), |(_, h)| format!("{h}p")),
                        "the canvas every clip is composed onto, and the size the export comes out at",
                        ActionId::Resolution,
                        cx.listener(|this, event: &ClickEvent, _, cx| {
                            this.open_picker(Pick::Resolution, event.position(), cx)
                        }),
                    ))
                    .child(self.action_control(
                        "fps",
                        ZOOM_SLOT_W,
                        BG_RAISED(),
                        None,
                        match self.session.is_some() {
                            true => format!("{} fps", fps_label(self.fps)),
                            false => "Rate".to_string(),
                        },
                        "the rate the whole timeline is cut and written at",
                        ActionId::Resolution,
                        cx.listener(|this, event: &ClickEvent, _, cx| {
                            this.open_picker(Pick::Fps, event.position(), cx)
                        }),
                    ))
                    .child(self.action_control(
                        "tonemap",
                        TONE_SLOT_W,
                        BG_RAISED(),
                        None,
                        match &self.session {
                            Some(session) => format!("HDR {}", tone_label(session.tone())),
                            None => "HDR".to_string(),
                        },
                        "which rendition HDR media is watched in; SDR media is untouched",
                        ActionId::Resolution,
                        cx.listener(|this, event: &ClickEvent, _, cx| {
                            this.open_picker(Pick::Tone, event.position(), cx)
                        }),
                    ))
                    // Its own rect and a label that *is* the state, the rule
                    // the snap toggle writes down: a button that resized itself
                    // on every press made the row shuffle under the pointer.
                    .child(self.action_control(
                        "proxies",
                        TONE_SLOT_W,
                        BG_RAISED(),
                        None,
                        match self.session.as_ref().map(PlaybackSession::proxies) {
                            Some(true) => format!("Proxies on{}", self.proxy_tail()),
                            Some(false) => format!("Proxies off{}", self.proxy_tail()),
                            None => "Proxies".to_string(),
                        },
                        "cuts on small stand-ins of the big films; the sound and every export \
                         stay the film's own",
                        ActionId::ToggleProxies,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_proxies(cx)),
                    ))
                    // Beside it, because the two are one subject: that one is
                    // what the picture is decoded from, this one is whether the
                    // stand-in is ever made. Two states and no cycle, the rule
                    // the switch beside it keeps.
                    .child(self.action_control(
                        "auto-proxies",
                        TONE_SLOT_W,
                        BG_RAISED(),
                        None,
                        match self.session.as_ref().map(PlaybackSession::auto_proxies) {
                            Some(true) => "Auto proxies on",
                            Some(false) => "Auto proxies off",
                            None => "Auto proxies",
                        },
                        "makes a stand-in for every big film as it is imported; with it off, \
                         turning Proxies on is what asks for them",
                        ActionId::ToggleAutoProxies,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_auto_proxies(cx)),
                    ))
                    .child(self.action_control(
                        "inspect-mix",
                        0.,
                        BG_RAISED(),
                        None,
                        "Mix",
                        "a fader per track and the limiter over the sum of them",
                        ActionId::Mix,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.open_mix(None, cx)),
                    )),
            )
    }
}

/// A region's own title bar: the same 24 px strip in the library and the
/// inspector, so the two columns read as the pair they are.
pub(crate) fn section_head(title: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(24.))
        .flex()
        .items_center()
        .px(px(8.))
        .bg(rgb(BG_PANEL()))
        .border_b_1()
        .border_color(rgb(STROKE_DIVIDER()))
        .text_size(px(11.))
        .text_color(rgb(FG_SECONDARY()))
        .child(title)
}
