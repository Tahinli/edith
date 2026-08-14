//! The three rows of chrome: the top bar over everything, the transport strip
//! under the picture, and the edit toolbar directly above the timeline.
//!
//! Every button in all three comes out of [`Player::action_control`], which
//! asks the one availability oracle whether the action can happen at all. That
//! is the whole point of this module existing: the toolbar used to decide with
//! ad-hoc booleans (`live && self.selected.is_some()`) while the keyboard asked
//! nobody, so with no file open Snap and `+ V` were dimmed *and dead* to the
//! pointer while the very same actions still fired from the keys. One oracle,
//! two doors -- if the key fires, the button fires.

use crate::*;
use crate::ui::widgets::*;

impl Player {
    /// A toolbar button for `action`: enabled exactly when [`enable`] says the
    /// action can happen, dimmed with the oracle's own reason in its tooltip
    /// when it cannot, and firing the same call the key fires when it can.
    ///
    /// `w` is the rect the label lives in. It is reserved once and never
    /// changes with state, so Play/Pause, Snap/No snap and Export/Cancel swap
    /// their words and their colour inside a box that does not move -- the
    /// complaint that started this redesign ("even button positions are
    /// changing according to its state").
    pub(crate) fn action_control(
        &self,
        id: &'static str,
        w: f32,
        bg: u32,
        glyph: Option<AnyElement>,
        label: impl Into<SharedString>,
        hint: &str,
        action: ActionId,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let enabled = self.enable(action, None);
        let key = self.keymap.display(action);
        // The dimmed button says *why* in the oracle's own words -- the same
        // string the key answers with, so the tooltip and the notice cannot
        // come to differ.
        let say = match enabled.why() {
            Some(why) => format!("{key} — {why}"),
            None => format!("{key} — {hint}"),
        };
        control(id, w, bg, glyph, label, say, enabled.yes(), on_click)
    }

    /// The bar over everything: what is open on the left, the two actions that
    /// write a file on the right. Export is the one accented button in the
    /// window -- the primary action of an editor, where every consumer editor
    /// puts it -- and it keeps its rect while it is a Cancel.
    pub(crate) fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let exporting = self.exporting().is_some();
        div()
            .flex_none()
            .h(px(TOPBAR_H))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .bg(rgb(BG_PANEL()))
            .border_b_1()
            .border_color(rgb(STROKE_DIVIDER()))
            .child(
                div()
                    .flex_none()
                    .w(px(10.))
                    .h(px(10.))
                    .rounded(px(2.))
                    .bg(rgb(ACCENT_PRIMARY())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .child(self.name.clone()),
            )
            .child(self.action_control(
                "save",
                0.,
                BG_RAISED(),
                None,
                "Save",
                "writes the project file",
                ActionId::Save,
                cx.listener(|this, _: &ClickEvent, _, cx| this.save_project(cx)),
            ))
            // One rect, two states: exporting swaps the word and the colour,
            // never the box. `the_export_button_never_moves` is the guard.
            .child(
                div()
                    .flex_none()
                    .w(px(EXPORT_SLOT_W))
                    .child(self.action_control(
                        "export",
                        EXPORT_SLOT_W,
                        // The one accented button in the window: the primary
                        // action of an editor, where every consumer editor puts
                        // it. Cancel keeps the rect and the accent both.
                        ACCENT_PRIMARY(),
                        None,
                        if exporting { "Cancel" } else { "Export" },
                        if exporting {
                            "stops the export; the part-written file goes"
                        } else {
                            "quality and destination, then writes the timeline out"
                        },
                        if exporting {
                            ActionId::CancelExport
                        } else {
                            ActionId::Export
                        },
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            if this.exporting().is_some() {
                                this.cancel_export();
                                cx.notify();
                            } else {
                                this.open_export(cx);
                            }
                        }),
                    ))
                    ,
            )
    }

    /// The player's own strip, under the picture and over the timeline: the
    /// transport, the clock, and the level. Where a consumer editor puts them,
    /// and the reason the edit toolbar below is nothing but edit tools.
    pub(crate) fn transport_bar(
        &self,
        position: f64,
        state: Transport,
        viewport_w: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = self.volume.muted;
        // The room this strip actually has: the window less the two side
        // panels. At the 640 px floor that is ~300 px, which the clock and the
        // level together do not fit -- so the level's *bar* stands down (its
        // button, its keys and its row on the actions card all remain), and
        // the clock keeps a narrower rect rather than being pushed off.
        let room = viewport_w - library_w(viewport_w) - inspector_w(viewport_w);
        let clock_w = TIME_W.min(room - 140.).max(96.);
        let slider = room >= TRANSPORT_ROOMY;
        div()
            .flex_none()
            .h(px(TRANSPORT_H))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .bg(rgb(BG_PANEL()))
            .border_t_1()
            .border_color(rgb(STROKE_DIVIDER()))
            .child(self.action_control(
                "transport",
                40.,
                BG_RAISED(),
                Some(transport_glyph(state).into_any_element()),
                "",
                "plays and pauses the timeline",
                ActionId::Play,
                cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_or_restart(cx)),
            ))
            // One line, one fixed width: changing digits must not push the
            // strip around, and the clock is the one number that changes
            // sixty times a second.
            .child(
                div()
                    .flex_none()
                    .w(px(clock_w))
                    .truncate()
                    .text_color(rgb(FG_PRIMARY()))
                    .child(format!(
                        "{} / {}",
                        timecode(position, self.fps),
                        timecode(
                            self.session
                                .as_ref()
                                .map_or(0., PlaybackSession::timeline_duration),
                            self.fps
                        )
                    )),
            )
            .child(div().flex_1().min_w(px(0.)))
            // The level as a number in its own rect, so muting swaps a glyph
            // and a colour rather than relabelling the button "Muted 80%" and
            // shoving the slider beside it along.
            .child(self.action_control(
                "volume",
                VOLUME_SLOT_W,
                BG_RAISED(),
                None,
                // The level is the label either way and it lives in a fixed
                // rect: muting adds a mark and changes the colour, where it
                // used to relabel the button "Muted 80%" and shove the slider
                // beside it along the row.
                match muted {
                    true => format!("× {}%", self.volume.percent()),
                    false => format!("{}%", self.volume.percent()),
                },
                "mutes and unmutes; the level is what it comes back to",
                ActionId::ToggleMute,
                cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.set_volume(|volume| volume.muted = !volume.muted, cx)
                }),
            ))
            .text_color(rgb(match muted {
                true => STATUS_WARNING(),
                false => FG_PRIMARY(),
            }))
            .when(slider, |d| {
                d.child(volume_slider(
                    self.volume,
                    self.volume_bar.clone(),
                    self.enable(ActionId::ToggleMute, None).yes(),
                    cx,
                ))
            })
    }

    /// The edit toolbar, directly above the timeline it acts on: split, delete,
    /// undo, the tracks, the magnet and the zoom -- the arrangement Movavi and
    /// CapCut share, and nothing in it that is not an edit or a way of looking
    /// at one.
    ///
    /// The row scrolls rather than losing its tail at the 640 px floor, and the
    /// door to everything scrolled off it is pinned outside the scrolling box:
    /// "it scrolls" is not "it can be found" (`toolbar_fits_the_smallest_window`).
    pub(crate) fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let exporting = self.exporting().is_some();
        div()
            .flex_none()
            .h(px(TOOLBAR_H))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .bg(rgb(BG_PANEL()))
            .border_t_1()
            .border_color(rgb(STROKE_DIVIDER()))
            .child(
                div()
                    .id("controls")
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .overflow_x_scroll()
                    .child(group_label("Edit"))
                    .child(self.action_control(
                        "cut",
                        0.,
                        BG_RAISED(),
                        Some(cut_glyph().into_any_element()),
                        "Split",
                        "splits the clip under the playhead",
                        ActionId::Cut,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.cut(cx)),
                    ))
                    .child(self.action_control(
                        "delete",
                        0.,
                        BG_RAISED(),
                        Some(delete_glyph().into_any_element()),
                        "Delete",
                        "takes the marked clip off the timeline",
                        ActionId::Delete,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.delete_selected(cx)),
                    ))
                    .child(self.action_control(
                        "undo",
                        0.,
                        BG_RAISED(),
                        None,
                        "Undo",
                        "takes the last edit back",
                        ActionId::Undo,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx)),
                    ))
                    .child(separator())
                    .child(group_label("Track"))
                    .child(self.action_control(
                        "add-video-lane",
                        0.,
                        BG_RAISED(),
                        None,
                        "+ V",
                        "adds a video track under the ones there",
                        ActionId::AddVideoLane,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.add_lane(LaneKind::Video, cx)
                        }),
                    ))
                    .child(self.action_control(
                        "add-audio-lane",
                        0.,
                        BG_RAISED(),
                        None,
                        "+ A",
                        "adds an audio track under the ones there",
                        ActionId::AddAudioLane,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.add_lane(LaneKind::Audio, cx)
                        }),
                    ))
                    .child(self.action_control(
                        "add-subtitle-lane",
                        0.,
                        BG_RAISED(),
                        None,
                        "+ S",
                        "adds a subtitle track under the ones there",
                        ActionId::AddSubtitleLane,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.add_lane(LaneKind::Subtitle, cx)
                        }),
                    ))
                    .child(separator())
                    .child(group_label("View"))
                    // Both toggles keep a rect of their own: the label is the
                    // state, and a state that resized its own button is what
                    // made the row shuffle every time it was pressed.
                    .child(self.action_control(
                        "snap",
                        SNAP_SLOT_W,
                        BG_RAISED(),
                        None,
                        if self.snap { "Snap on" } else { "Snap off" },
                        match self.snap {
                            true => "drags and trims land on clip edges, the playhead and the start",
                            false => "drags and trims land exactly where the hand leaves them",
                        },
                        ActionId::ToggleSnap,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_snap(cx)),
                    ))
                    .child(self.action_control(
                        "subs",
                        SNAP_SLOT_W,
                        BG_RAISED(),
                        None,
                        match (self.subtitle_track().is_some(), self.subs_on) {
                            (false, _) => "No subs",
                            (true, true) => "Subs on",
                            (true, false) => "Subs off",
                        },
                        "draws the cue under the playhead over the picture",
                        ActionId::ToggleSubtitles,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_subtitles(cx)),
                    ))
                    .child(self.action_control(
                        "theme",
                        THEME_SLOT_W,
                        BG_RAISED(),
                        None,
                        format!("Theme: {}", crate::ui::theme::active().label()),
                        "the colours this window is painted in; kept in ~/.config/edith/theme",
                        ActionId::Theme,
                        cx.listener(|this, event: &ClickEvent, _, cx| {
                            this.open_picker(Pick::Theme, event.position(), cx)
                        }),
                    ))
                    .child(separator())
                    .child(self.action_control(
                        "zoom-out",
                        CONTROL_H,
                        BG_RAISED(),
                        None,
                        "−",
                        "show more of the timeline; stops with all of it on the bed",
                        ActionId::ZoomOut,
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.zoom(1. / ZOOM_STEP, None, cx)
                        }),
                    ))
                    .child(self.action_control(
                        "zoom-fit",
                        ZOOM_SLOT_W,
                        BG_RAISED(),
                        None,
                        span_label(self.view().span()),
                        "fit the whole timeline on the bed",
                        ActionId::ZoomFit,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.zoom_fit(cx)),
                    ))
                    .child(self.action_control(
                        "zoom-in",
                        CONTROL_H,
                        BG_RAISED(),
                        None,
                        "+",
                        "magnify around the playhead; ctrl+wheel over the timeline zooms at the \
                         pointer, and a bare wheel scrolls it",
                        ActionId::ZoomIn,
                        cx.listener(|this, _: &ClickEvent, _, cx| this.zoom(ZOOM_STEP, None, cx)),
                    ))
                    .child(self.action_control(
                        "keys",
                        0.,
                        BG_RAISED(),
                        None,
                        "Actions",
                        "do any action, or change the key that does it",
                        ActionId::ShowActions,
                        cx.listener(|this, _: &ClickEvent, _, cx| match this.keys_open {
                            true => {
                                this.keys_open = false;
                                this.rebinding = None;
                                cx.notify();
                            }
                            false => this.show_actions(cx),
                        }),
                    )),
            )
            // The door to everything the row cannot show at this window, pinned
            // where a scroll cannot take it.
            .child(control(
                "controls-more",
                CONTROL_H,
                BG_RAISED(),
                None,
                "⋯",
                format!(
                    "{} — every action, including the ones scrolled off this row",
                    self.keymap.display(ActionId::ShowActions)
                ),
                !exporting,
                cx.listener(|this, _: &ClickEvent, _, cx| match this.keys_open {
                    true => {
                        this.keys_open = false;
                        this.rebinding = None;
                        cx.notify();
                    }
                    false => this.show_actions(cx),
                }),
            ))
    }
}

/// The rects the stateful buttons live in. Wide enough for the longest word
/// each of them can say, so the swap happens inside the box.
pub(crate) const EXPORT_SLOT_W: f32 = 76.;
pub(crate) const SNAP_SLOT_W: f32 = 76.;
pub(crate) const ZOOM_SLOT_W: f32 = 76.;
pub(crate) const VOLUME_SLOT_W: f32 = 76.;
/// The HDR rendition's names are the longest words on any button here.
pub(crate) const TONE_SLOT_W: f32 = 148.;
/// "Theme: " and the longest palette name: a fixed rect like the two toggles
/// beside it, so picking a palette repaints the button without moving the ones
/// after it.
pub(crate) const THEME_SLOT_W: f32 = 96.;

/// The width the transport strip needs before the level's bar fits beside the
/// clock: play + clock + the mute button + the bar, in their gaps.
pub(crate) const TRANSPORT_ROOMY: f32 = 460.;
