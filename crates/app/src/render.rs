//! What the window looks like this frame: the one `render` the whole
//! interface hangs off, root to overlays.

use crate::*;
use crate::ui::type_scale::Typeset;

/// The frame `image`, letterboxed/pillarboxed into whatever box it is given,
/// filling that box on its own axis and leaving the other bare -- the bare
/// strip is the box's own background (`BG_CANVAS`), never painted here, which
/// is why every caller sits this on a div already carrying that colour.
///
/// `Img::object_fit(Contain)` reads right (verified by hand against
/// `ObjectFit::Contain::get_bounds`) but only when taffy has already given the
/// element a real size to fit *into* -- and for a plain `img()` with both axes
/// `Auto` (`size_full()`/`max_w_full().max_h_full()` both resolve that way in
/// this pin), it resolves to the image's own native size first and object-fit
/// never gets a box smaller than the frame to shrink into, so the frame reads
/// through un-fitted. A `canvas()` element carries no such precompute -- its
/// `request_layout` is a plain `Style` read, so it is sized by its parent
/// exactly as a `div()` would be -- and its `paint` callback is handed the
/// real resolved `bounds`, which is all `Contain::get_bounds` needs.
///
/// `.absolute()` with all four insets zero, not `.size_full()` in flex flow:
/// a flex item under `justify_center()`/`items_center()` (see `picture_area`
/// below) never resolves a percentage cross-axis size for a plain-`Style`
/// element like `canvas()`, which is a taffy circularity and not this
/// element's own bug (confirmed live: bounds handed to `paint` came back
/// `944px x 0px`, main axis right, cross axis collapsed). An absolutely
/// positioned box is sized against its containing block directly instead --
/// the nearest `.relative()` ancestor, which every caller here already sits
/// this on -- so it never asks taffy's flex algorithm for a cross-axis size
/// at all.
///
/// That containing block still has to have a real size of its own, though:
/// gpui's default `display` is `Block` (`Style::default()`), not `Flex`, and
/// a `Block` box with height `auto` is sized by its *normal-flow* children --
/// an `.absolute()` one, this, takes no part in that (confirmed live: with
/// the box lacking its own `.flex()`, this canvas's own bounds came back
/// `944px x 0px` even though its containing block, measured independently,
/// was a real `944px x 335px`). Every `.relative()` ancestor this sits on
/// must also be `.flex()` (see `ui::stance::screen`'s `stance-picture`) for
/// exactly this reason.
pub(crate) fn letterboxed_image(image: Arc<RenderImage>) -> impl IntoElement + Styled {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let fitted = gpui::ObjectFit::Contain.get_bounds(bounds, image.size(0));
            let _ = window.paint_image(fitted, Corners::default(), image, 0, false);
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0()
    .right_0()
    .size_full()
}

impl Player {
    /// The picture region alone: the image, the subtitle cue plate, the
    /// preview badge and the three transient bars over its bottom edge.
    /// Its own method so player fullscreen ([`Player::act`]) can stand it up
    /// as the window's only child without a second copy of what it draws --
    /// and so the darkroom's own screen region (DESIGN.md §5,
    /// `ui::stance::screen`) draws the same picture rather than a second one.
    pub(crate) fn picture_area(
        &mut self,
        position: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The compare view's picture as shot is read for the frame on screen, if it is not in hand.
        self.refresh_before(window, cx);
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            // The bed the cue plate is placed against: it hangs off the
            // bottom of the picture region, which is the one box that is the
            // picture and nothing else.
            .relative()
            // ...and the box the frame is *watched* in, which is what the
            // session paints at ([`Player::push_view_size`]).
            .child(bounds_probe(self.picture_box.clone()))
            .flex()
            .justify_center()
            .items_center()
            .bg(rgb(BG_CANVAS()))
            .children(
                // A clip whose file is off the disk shows black, not the last frame it had:
                // the banner says what is wrong, and a frame of another clip would read as this one.
                self.image
                    .clone()
                    .filter(|_| !self.picture_is_missing())
                    .map(|i| {
                        let native = self.viewer_native_or(&i);
                        // The compare view draws the graded frame with the picture as shot beside it.
                        // A source preview has no grade to compare, so it keeps the plain picture.
                        if self.compare != crate::viewer_view::Compare::Off && self.preview_session.is_none() {
                            return crate::viewer_view::compare_picture(
                                self.compare,
                                self.before.clone(),
                                i,
                                native,
                                self.viewer.zoom,
                                self.viewer_pan,
                                window.scale_factor(),
                            )
                            .into_any_element();
                        }
                        crate::viewer_view::viewer_picture(i, native, self.viewer, self.viewer_pan, window.scale_factor())
                            .into_any_element()
                    })
                    // With no file open the letterbox is the whole region,
                    // and a black rectangle says only that something is
                    // broken -- so it says what it wants instead. The window
                    // is already the drop target.
                    .or_else(|| {
                        (self.active_session().is_none()).then(|| self.empty_viewer(cx).into_any_element())
                    }),
            )
            // The empty project frame's edge, so a near-black canvas still reads as a frame.
            .children(self.empty_canvas_outline())
            // The clip under the playhead reads a file that is gone: say so over the picture.
            .children(self.missing_clip_note(cx))
            // After the picture, so the plate is drawn over it rather than
            // under.
            // Titles first, so the captions over them draw on top.
            // Placed against the picture as drawn: the fitted frame, or the zoomed and panned one.
            .children(self.title_overlay(position, window.scale_factor(), window, cx))
            .children(self.subtitle_overlay(position, window).map(|overlay| {
                // Zoomed, the caption's box is the picture's rectangle, not the region.
                match (self.viewer.zoom, self.viewer_frame_local(window.scale_factor())) {
                    (crate::viewer_view::Zoom::Fit, _) | (_, None) => overlay.into_any_element(),
                    (_, Some((x, y, w, h))) => div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(w))
                        .h(px(h))
                        .child(overlay)
                        .into_any_element(),
                }
            }))
            .children(self.viewer_reduced().then(|| {
                div()
                    .absolute()
                    .top(px(8.))
                    .left(px(8.))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(rgba(crate::ui::theme::with_alpha(crate::ui::theme::tokens().e0, 0x99)))
                    .type_style(crate::ui::type_scale::caption())
                    .text_color(rgb(crate::ui::theme::tokens().ink1))
                    .child(format!("{} res", self.viewer.quality.label()))
            }))
            // The three transient lines hang off the bottom of the picture
            // rather than taking a row of the column: a notice that arrives
            // must not push the transport, the toolbar and the timeline down
            // by its own height -- which is a control moving with state, on
            // every control below it at once.
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(self.import_bar(cx))
                    .children(self.seek_bar())
                    // How tall the lot came out, for the cue plate above to
                    // step over ([`sub_bottom`]) -- the bars are over the
                    // picture, and a message drawn across the line being
                    // read loses both of them. Zero with no bar up, since
                    // this box is then empty.
                    .child(height_probe(self.notice_h.clone())),
            )
    }
}

/// How long a viewer size must hold still while playing before the picture is
/// restarted for it. Each restart stalls the picture until the new stream
/// refills, so a window being dragged (a step every few pixels) is applied
/// once the drag rests instead of on every step.
const VIEW_SETTLE: std::time::Duration = std::time::Duration::from_millis(200);

impl Player {
    /// Tells the active session where the picture actually lands, in device
    /// pixels, so its seats can paint at that size instead of the project's
    /// ([`engine::PlaybackSession::set_view_size`]). Read off the picture
    /// area's own bounds -- the box `letterboxed_image` fits the frame into --
    /// and the project's aspect, which is the aspect of every frame it hands
    /// over.
    ///
    /// A size that moved enough for the eye to see gets a fresh picture: the
    /// canvas a span paints at is decided when it opens, so a resized window
    /// would otherwise sharpen on its next seek and not before.
    fn push_view_size(&mut self, window: &mut Window) {
        if self.shot_pending.is_some() {
            // A screenshot in flight asked for the project's own size and is
            // waiting on that frame: pushing the preview's size back now would
            // cancel the very picture it is waiting for.
            return;
        }
        let region = self.picture_box.get();
        if region.size.width <= px(1.) || region.size.height <= px(1.) {
            // Before the first paint there is no box to read, and a floor is
            // not a measurement.
            return;
        }
        let scale = window.scale_factor();
        let Some(session) = self.active_session() else {
            return;
        };
        let playing = session.is_playing();
        // Device pixels, the unit `Contain` measures its fitting size in.
        let meta = session.meta();
        let native = (meta.width, meta.height);
        let fitted = gpui::ObjectFit::Contain.get_bounds(
            region,
            size(
                gpui::DevicePixels(meta.width as i32),
                gpui::DevicePixels(meta.height as i32),
            ),
        );
        let w = (f32::from(fitted.size.width) * scale).round() as u32;
        let h = (f32::from(fitted.size.height) * scale).round() as u32;
        let (w, h) = self.viewer_target((w, h), native, playing);
        // Playing: a size that is still moving is held back until it has rested
        // for VIEW_SETTLE, since every apply restarts the picture. Paused, a frame
        // is cheap and must be sharp, so the size is applied at once.
        let settled = if !playing {
            self.view_seen = None;
            true
        } else {
            match self.view_seen {
                Some((size, since)) if size == (w, h) => since.elapsed() >= VIEW_SETTLE,
                _ => {
                    self.view_seen = Some(((w, h), Instant::now()));
                    false
                }
            }
        };
        if !settled {
            // The loop keeps turning while playing; asked again in case it does not.
            window.request_animation_frame();
            return;
        }
        let Some(session) = self.active_session_mut() else {
            return;
        };
        if session.set_view_size(w, h) {
            session.resync_picture();
        }
    }
}

impl Player {
    /// The focus invariant: keyboard focus is always somewhere inside the
    /// room, so the root's key handler and every ring handler under it hear
    /// the keys. A card, sheet or tab that unmounts while it (or a button in
    /// it) holds focus leaves the window focused on a handle that is no longer
    /// in the tree, and gpui then dispatches keys to nothing -- Space, Ctrl+Z
    /// and Esc all go dead until a click lands on a focusable box. The tree a
    /// render produces is only known once drawn, so a short check after it
    /// puts focus back on the root if the focused handle is gone. One door for
    /// every way a surface can close, rather than a refocus at each of them.
    fn guard_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.focus_guard_pending {
            return;
        }
        self.focus_guard_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            gpui::Timer::after(std::time::Duration::from_millis(60)).await;
            this.update_in(cx, |p, window, cx| {
                p.focus_guard_pending = false;
                if !p.focus.contains_focused(window, cx) {
                    window.focus(&p.focus);
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A file that has just been opened sits on its first frame with the
        // clock stopped: opening is not playing, whichever way the file
        // arrived. The play binding and the transport button start it.
        if let Some(session) = self.active_session_mut() {
            session.tick();
        }
        self.pump();
        // The dropped-frames words linger after a stop, timed from it, so the window is drawn
        // again until the linger has run out and the words can go.
        if self.drop_lingers() {
            window.refresh();
        }
        // The frames the pump replaced leave every window's sprite atlas here, where the
        // video output window's copy of them can be reached too.
        for old in std::mem::take(&mut self.retired_images) {
            cx.drop_image(old, Some(window));
        }
        self.sync_output(cx);
        self.sync_dock_tab();
        self.sync_sections(cx);
        // Every press is noted for the tooltips: one still waiting to show must not show after it.
        window.on_mouse_event(|e: &MouseDownEvent, phase, _, _| {
            if phase.capture() {
                crate::ui::widgets::note_press(e.position);
            }
        });
        self.sync_transcript();
        self.guard_focus(window, cx);
        // A screenshot waiting for its project-sized frame is written on the
        // repaint that lands it -- and before the size push below, which is
        // what gives the preview its own size back.
        self.flush_shot(cx);
        // ...at the size it is watched at, not the project's: a visualizer
        // seat redrawing 1920x1080 for a 600 px preview is nine times the
        // pixels for nothing the eye can see. An export has a seat of its own
        // and never comes through here.
        self.push_view_size(window);
        // What every hover label asks before it paints: a card or a menu is
        // drawn over whatever the pointer is resting on.
        OVERLAID.store(self.overlaid(), Ordering::Relaxed);
        let vs = window.viewport_size();
        let dock_left = f32::from(vs.width) - self.split_px(Split::Dock, vs);
        crate::ui::widgets::DOCK_W_BITS.store((f32::from(vs.width) - dock_left).to_bits(), Ordering::Relaxed);
        let timeline_top =
            crate::ui::stance::below_picture_floor(f32::from(vs.height), self.split_px(Split::Bench, vs));
        crate::ui::widgets::TIMELINE_TOP_BITS.store(timeline_top.to_bits(), Ordering::Relaxed);
        // A cleared seek is a frame delivered, which is the one readiness signal
        // there is: whatever a slider drag held back is written here.
        if self.seek_since.is_none() {
            self.flush_drag(cx);
        }
        self.poll_export();
        self.poll_queue();
        self.poll_import(cx);
        self.poll_collect(cx);
        self.poll_silence();
        self.poll_proxies(cx);
        self.poll_render(cx);
        self.poll_stabilise();
        self.poll_tracks();
        self.poll_reframe();
        self.poll_multicam();
        // Every way a source can arrive -- argv, an import, a project load --
        // has been through a repaint by the time its clips are drawn, so this
        // is the one place that has to notice a new one.
        self.cache_media(cx);
        self.cache_export_seat(cx);
        // The export sheet's one-second trial write, which prices its time.
        self.cache_export_pace();
        self.cache_hw_caps(cx);
        // What the compositor calls this window. Pushed only when it changes:
        // it is a protocol round trip and this runs at vsync.
        let title = shown_title(&self.name, &self.project_path, self.new_project.as_deref(), self.autosave_armed);
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }
        // A compositor keybind (the window manager's own fullscreen, not
        // ours) can drop `window.is_fullscreen()` without ever going through
        // [`Player::act`] -- the picture-only layout below must not survive
        // that on its own, or the chrome never comes back. Only the *leaving*
        // direction is reconciled here: the platform going fullscreen behind
        // our back is not this editor asking for the picture-only layout, so
        // it is left alone rather than guessed at.
        if self.player_fullscreen && !window.is_fullscreen() {
            self.player_fullscreen = false;
        }
        // No shadow flag: the session is the only truth about play state, and
        // [`Player::transport`] is the one place it is read.
        let state = self.transport();
        // A paused timeline has nothing to animate; the toggle handlers notify,
        // which is what starts the loop again. A paused seek keeps the loop
        // running by itself until `pump` has the frame it asked for. An export
        // pauses playback and still needs the loop: its progress only reaches
        // the screen on a repaint. A notice does not: it waits to be dismissed
        // rather than for a clock, so keeping the loop alive for it would spin
        // the GPU until someone answered it.
        // An import does too, and for the same reason: its clock and its sweep
        // only reach the screen on a repaint, and a still line is the very
        // thing it exists to disprove.
        if state.is_playing()
            || self.seek_since.is_some()
            || self.export.is_some()
            // ...and the queue, whose rows carry a percent.
            || self.queue.is_going()
            // ...and the sheet's trial write, whose answer is the time line.
            || self.export_sheet.bench.is_some()
            || self.importing.is_some()
            // A silence scan too: its progress and its two clocks only reach
            // the screen on a repaint, and a still line is the very thing this
            // card was rewritten to disprove.
            || self.silence_scan.is_some()
            // ...and a stand-in being made: its percentage on the library row
            // only reaches the screen on a repaint, and a bar that never moves
            // is exactly what a person reads as a hung encode.
            || self.making_proxies()
            // ...and a stabilisation analysis: its percent on the transform
            // card, and the corrected picture once it lands.
            || !self.stab_waiting.is_empty()
            // ...and a mask track: its percent on the effects card.
            || !self.track_jobs.is_empty()
            // ...and an auto reframe: its percent in the status bar.
            || self.reframe_run.is_some()
            // ...and angles being synced: the percent in the status bar.
            || self.mcam_run.is_some()
            // ...and a media collect: its bar and byte count only reach the screen on a repaint.
            || self.collect_running()
            // ...and a render preview: its bar fills, and its finish is seen, on a repaint.
            || self.render.as_ref().is_some_and(|(job, _)| !job.is_finished())
        {
            window.request_animation_frame();
        }

        // Read per render, never cached: a delete shortens the timeline and the
        // timecode, the ruler and the clamp below all have to follow it -- and
        // so does the room a tail being dragged needs to grow into.
        let duration = self.drawn_duration();
        let position = self.playhead(duration);
        // Re-settled every frame against the duration this one is drawing: an
        // edit that shortens the timeline moves the far end of the view, and a
        // playhead that has run off the bed pulls the view after it -- which is
        // what makes a zoomed-in timeline scroll while it plays.
        // ...but only a playhead that is *going* somewhere pulls it: following
        // is what a moving one does, during playback and through a seek. A view
        // yanked back to a playhead nobody moved is a hand's own scroll undone
        // by the very next frame, which is what made the wheel look dead.
        // ...and a hand that scrolled the view away from the head keeps it
        // ([`Player::panned`]): a follow that centres the head again would undo
        // the notch before it was seen, which is what made the wheel look dead
        // while playing. It is given straight back below, the moment the head
        // is on the bed a person chose to look at -- so the scroll wins now and
        // the follow resumes by itself, with nothing to press.
        // A fit asked for before the bed was painted ([`Player::zoom_fit`]) is
        // taken here, on the first frame that measured one; until then the
        // loop is kept alive so that frame comes.
        if self.fit_pending {
            match self.view().bed > 0. {
                true => {
                    self.fit_to_bed();
                    self.fit_pending = false;
                }
                false => window.request_animation_frame(),
            }
        }
        // A fit still in force follows the bed: the first frame measures the
        // window's initial size, and a compositor resize to the tiled size
        // after it changes the bed, so the fit is taken again for the new width
        // ([`Player::fit_to_bed`]). A hand's zoom or scroll has cleared it.
        if let Some(fitted) = self.fitted_bed {
            let bed = self.view().bed;
            if bed > 0. && bed != fitted {
                self.fit_to_bed();
            }
        }
        self.scale = match (state.is_playing() || self.seek_since.is_some()) && !self.panned {
            true => self.view().following(position),
            false => self.view().settled(),
        };
        if self.panned && self.view().shows(position) {
            self.panned = false;
        }

        ui::stance::render(self, window, cx)
    }
}
