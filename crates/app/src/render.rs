//! What the window looks like this frame: the one `render` the whole
//! interface hangs off, root to overlays.

use crate::*;

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
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            // The bed the cue plate is placed against: it hangs off the
            // bottom of the picture region, which is the one box that is the
            // picture and nothing else.
            .relative()
            .flex()
            .justify_center()
            .items_center()
            .bg(rgb(BG_CANVAS()))
            .children(
                self.image
                    .clone()
                    .map(|i| letterboxed_image(i).into_any_element())
                    // With no file open the letterbox is the whole region,
                    // and a black rectangle says only that something is
                    // broken -- so it says what it wants instead. The window
                    // is already the drop target.
                    .or_else(|| {
                        (self.active_session().is_none()).then(|| empty_hint().into_any_element())
                    }),
            )
            // After the picture, so the plate is drawn over it rather than
            // under.
            .children(self.subtitle_overlay(position, window))
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
            // The preview's scrub bar, drawn LAST: the notice box above shares
            // its bottom edge, and a bar under the "PREVIEWING ..." plate was
            // invisible for exactly the moment it matters.
            .children(self.preview_scrub_bar(cx))
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
        self.pump(window);
        // What every hover label asks before it paints: a card or a menu is
        // drawn over whatever the pointer is resting on.
        OVERLAID.store(self.overlaid(), Ordering::Relaxed);
        // A cleared seek is a frame delivered, which is the one readiness signal
        // there is: whatever a slider drag held back is written here.
        if self.seek_since.is_none() {
            self.flush_drag(cx);
        }
        self.poll_export();
        self.poll_import(cx);
        self.poll_silence();
        self.poll_proxies(cx);
        // Every way a source can arrive -- argv, an import, a project load --
        // has been through a repaint by the time its clips are drawn, so this
        // is the one place that has to notice a new one.
        self.cache_media(cx);
        self.cache_export_seat(cx);
        self.cache_hw_caps(cx);
        // What the compositor calls this window. Pushed only when it changes:
        // it is a protocol round trip and this runs at vsync.
        let title = window_title(&self.name);
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
            || self.importing.is_some()
            // A silence scan too: its progress and its two clocks only reach
            // the screen on a repaint, and a still line is the very thing this
            // card was rewritten to disprove.
            || self.silence_scan.is_some()
            // ...and a stand-in being made: its percentage on the library row
            // only reaches the screen on a repaint, and a bar that never moves
            // is exactly what a person reads as a hung encode.
            || self.making_proxies()
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
