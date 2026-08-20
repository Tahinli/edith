//! What the window looks like this frame: the one `render` the whole
//! interface hangs off, root to overlays.

use crate::*;

/// Whether a stroke leaves player fullscreen. Bare escape does, the same
/// stroke every card and menu in this window answers to -- and it is
/// answered *first*: the picture-only layout is the chrome itself missing,
/// not a card drawn over it, so it goes before a menu close or a preview
/// close gets a look at the same key. See the call site in [`Player::render`]
/// for the deliberate order this buys with a preview open underneath.
pub(crate) fn escape_leaves_player_fullscreen(key: &str, ctrl: bool, player_fullscreen: bool) -> bool {
    key == ESCAPE && !ctrl && player_fullscreen
}

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
                        (self.active_session().is_none())
                            .then(|| empty_hint().into_any_element())
                    }),
            )
            // After the picture, so the plate is drawn over it rather than
            // under.
            .children(self.subtitle_overlay(position, window))
            .children(self.preview_badge(cx))
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
                    // DESIGN §8: no full-width bar over the picture, ever.
                    // The darkroom stance has its own §8-conformant notice
                    // plate above the ledger (`ui::stance::notice_plate`) --
                    // this legacy bar would otherwise paint a second,
                    // picture-covering copy of the same notice underneath
                    // it. The legacy (non-darkroom) room keeps this bar
                    // exactly as it was.
                    .children((!self.darkroom).then(|| self.notice_bar(cx)).flatten())
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

        // DESIGN.md §12 step 2: the stance skeleton, on by default now
        // (`OLD_GUI=1` opts back into the tree below).
        // `.into_any_element()` on both arms is the one thing this branch
        // costs the legacy tree below -- an `impl IntoElement` return can
        // only be one concrete type, and the two trees are not the same one.
        if self.darkroom {
            return ui::stance::render(self, window, cx).into_any_element();
        }

        (div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let ctrl = event.keystroke.modifiers.control;
                // `is_held` is the auto-repeat, and a value is the one thing
                // worth running on it: a held arrow on a card moves the slider
                // it picked, and a held volume key runs the volume. Everything
                // else is filtered exactly as it always was -- a repeat that
                // toggled playback, or cut the timeline, many times a second is
                // what this guard is for, and a row waiting for a stroke takes
                // none of it either (it would bind the key, then fire what it
                // just bound). See [`repeats`].
                if event.is_held
                    && !repeats(this.repeat_scope(), key, this.keymap.lookup(key, ctrl))
                {
                    return;
                }
                // The recovery notice is the one bar that reads a key as more
                // than "answered": `enter` loads the sidecar it named, and
                // every other key declines it -- the way any other key
                // dismisses any other notice. Asked before the generic
                // dismiss below shares the same door with it, not instead of
                // it: this recovers or declines, that pops the bar.
                if this.recovery_sidecar.is_some() {
                    if key == "enter" {
                        this.recover_from_sidecar(cx);
                    } else if let Some(sidecar) = this.recovery_sidecar.take() {
                        Player::discard_sidecar(&sidecar);
                    }
                }
                // Any key answers the message on the bar and brings the next of
                // them up, whatever it was -- and owes
                // the repaint itself: a notice no longer keeps the render loop
                // alive, and the arms below that do notify are not all of them
                // (an unbound key, or the copy chord, changes nothing else).
                if this.dismiss_notice() {
                    cx.notify();
                }
                // A row is waiting for a stroke, and while it is, that stroke is
                // data: it means the binding and nothing else, which is why this
                // answers before the export guard and before the keymap is
                // consulted at all.
                if let Some(action) = this.rebinding {
                    if key == ESCAPE {
                        this.rebinding = None;
                    } else if !is_bare_modifier(key) {
                        this.capture(action, key, ctrl);
                    }
                    cx.notify();
                    return;
                }
                // On linux gpui reports the copy chord as key "c" with the
                // control modifier set (the control code is mapped back), which
                // is why the keymap is keyed on the pair and never on the key
                // alone.
                let action = this.keymap.lookup(key, ctrl);
                // An export is reading the edit list every other action here
                // would change, so cancelling is the only one that means
                // anything until it is over.
                if this.exporting().is_some() {
                    if cancels_export(key, ctrl, action) {
                        this.cancel_export();
                    }
                    cx.notify();
                    return;
                }
                // The overlay owns the keyboard while it is up -- but it types
                // now: a printable stroke is the search box's, which is why
                // nothing here reaches the keymap. A waiting row is answered
                // above and still wins, so a rebind onto "v" binds the key
                // rather than typing it.
                if this.keys_open {
                    if key == ESCAPE {
                        // Two steps out, the way a search box anywhere gets
                        // out: the filter first -- the whole list back --
                        // and the card only once there is no search to clear.
                        if this.keys_search.is_empty() {
                            this.keys_open = false;
                        } else {
                            this.keys_search.clear();
                            this.scroll_keys(None);
                        }
                    // The rows past the fold, without a wheel: forty actions
                    // are four times what the viewport shows, and the hand
                    // typing in the search box is already on the keyboard.
                    } else if key == "up" {
                        this.scroll_keys(Some(KEYS_ROW_H));
                    } else if key == "down" {
                        this.scroll_keys(Some(-KEYS_ROW_H));
                    } else if key == "backspace" {
                        this.keys_search.pop();
                        this.scroll_keys(None);
                    } else if let Some(c) = typed(key) {
                        this.keys_search.push(c);
                        this.scroll_keys(None);
                    }
                    cx.notify();
                    return;
                }
                // The dock's Sources filter (MOCK-SPEC "Dock" §2): owns the
                // keyboard only while it was clicked into, exactly the
                // `keys_search` shape above -- a letter typed anywhere else in
                // the room still means what the spine says it means.
                if this.dock_filter_edit {
                    if key == "escape" || key == "enter" {
                        this.dock_filter_edit = false;
                    } else if key == "backspace" {
                        this.dock_filter.pop();
                    } else if let Some(c) = typed(key) {
                        this.dock_filter.push(c);
                    }
                    cx.notify();
                    return;
                }
                // `↵` adds the picked source at the playhead (MOCK-SPEC "Dock"
                // hint paragraph, gesture 2) -- the keyboard door beside the
                // legacy "Add at playhead" button's pointer one, live only
                // where a row is actually picked so a bare enter elsewhere
                // keeps meaning whatever it already means below.
                if key == "enter" && this.dock_src_active && this.selected_asset.is_some() && this.exporting().is_none() {
                    if let Some((path, stream)) = this.selected_asset.clone() {
                        this.insert_source(&path, stream, None, None, cx);
                    }
                    cx.notify();
                    return;
                }
                // The export card owns it the same way, and for the same
                // reason. Escape closes it -- nothing has been written yet, so
                // there is nothing here to cancel -- and the card's own letters
                // are its input: it has no widget that takes focus (nothing in
                // it does), so this listener is its keyboard, exactly as it is
                // a waiting row's.
                if this.export_open {
                    // A list open over the card is the innermost thing on
                    // screen, so it is what a stroke closes first -- the rule
                    // every menu here follows, said before the card's own keys
                    // so escape does not take the card out from under it.
                    if this.picker.take().is_some() {
                        cx.notify();
                        return;
                    }
                    // A number being typed is the next thing in: while the
                    // field is open every stroke is text, which is what makes
                    // it a field and not a capture -- the card's letters cannot
                    // fire under it, and escape gives up the edit before it
                    // touches the card.
                    if let Some(edit) = &mut this.mbps_edit {
                        if key == ESCAPE {
                            this.mbps_edit = None;
                        } else if key == "enter" {
                            // Committed or refused in its own words; a refused
                            // one stays open on what was typed, so the number
                            // can be fixed rather than typed again.
                            if let Some(mbps) = edit.commit() {
                                this.custom_mbps = mbps;
                                this.quality = Quality::Custom;
                                this.mbps_edit = None;
                            }
                        } else if key == "backspace" {
                            edit.backspace();
                        } else if key == "up" {
                            edit.step(1);
                        } else if key == "down" {
                            edit.step(-1);
                        } else if let Ok(digit) = key.parse::<u32>() {
                            edit.digit(digit);
                        }
                        cx.notify();
                        return;
                    }
                    if key == ESCAPE {
                        this.export_open = false;
                    } else if key == "enter" {
                        // The card's own button, by keyboard: the one thing in
                        // it that writes a file must not be pointer-only either.
                        this.start_export(cx);
                    } else if let Some(format) = format_key(key, this.format) {
                        // The codec rows by their own letter, so the card can be
                        // driven without a mouse -- the same card-local input
                        // the typed bitrate is, and for the same reason: a
                        // choice reachable only by pointer is not reachable by
                        // everyone. Not a keymap binding: it means nothing
                        // outside this card, exactly like the digits.
                        this.set_format(format);
                    } else if key == "c" {
                        this.cycle_container();
                    } else if key == "q" {
                        this.cycle_quality();
                    } else if key == "b" {
                        // The sound's rate, `q`'s pair for the other half of
                        // the file. Not a digit: those are the picture's.
                        this.cycle_audio_kbps();
                    } else if key == "e" {
                        // Which encoder writes the picture, `b`'s neighbour for
                        // the other thing about the file a person picks rather
                        // than types. Card-local like the rest of these.
                        this.cycle_encoder(cx);
                    } else if key == "d" {
                        // The save dialog, which was the one row here a
                        // keyboard could not open.
                        this.pick_destination(cx);
                    } else if let Some(preset) = ExportPreset::ALL
                        .into_iter()
                        .find(|p| p.key() == key)
                        // `Custom` shares `s` with Advanced below, which already
                        // handles that key -- reaching it here too would only
                        // race the same assignment against itself.
                        .filter(|p| *p != ExportPreset::Custom)
                    {
                        this.pick_preset(preset);
                    } else if key == "g" {
                        this.export_grouped = !this.export_grouped;
                    } else if key == "r" {
                        this.export_refusals_inline = !this.export_refusals_inline;
                    } else if key == "s" {
                        // The Advanced pane, by keyboard: the primary pane's
                        // own row for it opens the same way a click does.
                        this.export_advanced_open = !this.export_advanced_open;
                    } else if key == "n" {
                        // The custom row's field, by keyboard. The digits used
                        // to do this from anywhere in the card, which meant a
                        // stray keystroke changed the bitrate with nothing on
                        // screen to say it had: now a digit outside the field
                        // means nothing at all, and this is the way in.
                        this.edit_mbps();
                    }
                    cx.notify();
                    return;
                }
                // Every param card's own key branch, factored into
                // `Player::param_card_key` (`player/cards.rs`) so this tree and
                // the darkroom stance drive the same logic rather than a
                // second, divergent copy.
                if this.param_card_key(key, event.keystroke.modifiers.shift, cx) {
                    cx.notify();
                    return;
                }
                // A clip menu names an index, and every edit below moves
                // indices -- so a stroke closes it before it acts. Escape means
                // that and nothing else, which is the `esc` the keys menu
                // already lists (keymap.rs `FIXED`).
                // Both menus, taken rather than short-circuited: the library's
                // one names a row the edits below can remove, so it closes on a
                // stroke exactly as the clip menu does.
                // A choice list goes the same way and for the same reason: it
                // names a clip index too, and escape is the way out of it.
                // An open list is the innermost thing on screen, so it takes
                // the keys before anything under it does: ↑↓ walk it, enter
                // takes the row, and escape falls through to the close below --
                // the same three strokes every list in this editor answers.
                // The picture-only layout is the outermost thing on screen --
                // it is the chrome itself that is missing, not a card drawn
                // over it -- so its own escape is answered before any menu or
                // preview below gets a look at the stroke. Deliberate order:
                // with a preview open *and* the player fullscreen, the first
                // Escape only leaves fullscreen (the preview keeps playing,
                // now behind the chrome again), and it takes a second Escape
                // to stop the preview -- one stroke, one effect, same as
                // every other card in this chain.
                if escape_leaves_player_fullscreen(key, ctrl, this.player_fullscreen) {
                    this.player_fullscreen = false;
                    window.toggle_fullscreen();
                    cx.notify();
                    return;
                }
                if let Some(mut picker) = this.picker {
                    let rows = this.choices(picker.of);
                    if !rows.is_empty() && matches!(key, "up" | "down" | "enter") {
                        match key {
                            "down" => picker.sel = (picker.sel + 1) % rows.len(),
                            "up" => picker.sel = (picker.sel + rows.len() - 1) % rows.len(),
                            _ => {
                                let (choice, ..) = rows[picker.sel.min(rows.len() - 1)];
                                this.choose(choice, cx);
                                cx.notify();
                                return;
                            }
                        }
                        this.picker = Some(picker);
                        cx.notify();
                        return;
                    }
                }
                let clip_menu = this.context_menu.take().is_some();
                let row_menu = this.library_menu.take().is_some();
                let list = this.picker.take().is_some();
                if clip_menu || row_menu || list {
                    cx.notify();
                    if key == ESCAPE {
                        return;
                    }
                }
                // The innermost thing left on screen once no menu is up: a
                // preview takes the picture over the timeline's own, and
                // escape is its own way out, same as every card above it.
                if key == ESCAPE && !ctrl && this.preview_session.is_some() {
                    this.close_preview(cx);
                    return;
                }
                if let Some(action) = action {
                    this.act(action, window, cx);
                }
            }))
            // The whole window is the drop target: gpui turns an external file
            // drop into an `ExternalPaths` drag (window.rs:3626) delivered as a
            // mouse-up to every hovered hitbox, and the root's is the only one
            // that covers the picture as well as the panel.
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                // The overlay owns the pointer as well as the keyboard, and a
                // drop is a click the scrim cannot swallow: gpui delivers it to
                // the root's hitbox, which is under the scrim but is not a
                // sibling it can stop. The export card is over the timeline for
                // the same reason: importing under it would change the very
                // edit list the card is about to write out.
                if this.modal() {
                    return;
                }
                for path in paths.paths() {
                    // One queue for all of them, in arrival order: the fork --
                    // a project replaces the timeline, media joins the library
                    // -- is made when each one's worker starts ([`arrival`]),
                    // and neither is read on this thread.
                    this.import(path, cx);
                }
            }))
            // A drop event carries no path of its own -- gpui only tells the
            // target that something landed -- so the line that promises where
            // it will land is fed by the drag's own moves, which do carry the
            // pointer (gpui div.rs:282). On the root, because a drag crosses
            // the window: it starts on a clip or on a library row and ends over
            // a lane, and only an ancestor of both hears all of it.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ClipDrag>, _, cx| {
                // The clip the payload named, wherever an edit mid-drag has
                // since put it ([`Player::dragged`]): the line has to promise a
                // landing for the take actually in the hand.
                let drag = *event.drag(cx);
                if let Some(idx) = this.dragged(&drag) {
                    this.preview_drop(drag.lane, idx, event.event.position.x, cx);
                }
                // The shadow belongs to a *lane*, and which lane the pointer is
                // over is the one thing this element cannot see. Cleared here
                // and drawn again by the lane the pointer is actually inside
                // (`lane_row`), which gpui runs straight after this one: the
                // capture phase goes parent first, so a pointer over no lane at
                // all -- up in the library, say -- promises nothing.
                this.set_ghost(None, cx);
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<AssetDrag>, _, cx| {
                    this.preview_place(event.event.position.x, cx);
                    this.set_ghost(None, cx);
                }),
            )
            // The same pair for the two subtitle gestures, for the same reason:
            // a palette row starts up in the library and a caption starts on a
            // lane, and both are let go somewhere else entirely.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SubPick>, _, cx| {
                this.preview_place(event.event.position.x, cx);
                this.set_ghost(None, cx);
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SubDrag>, _, cx| {
                // The placement gpui froze into the payload, whose *length* is
                // all the line needs; which caption it is is the drop's
                // question ([`Player::dragged_sub`]).
                let (drag, x) = (*event.drag(cx), event.event.position.x);
                let cue = this.sub_drop_frame(drag.sub, x).1;
                this.set_cue(cue, x, cx);
                this.set_ghost(None, cx);
            }))
            // Scrubbing is tracked on the root because the pointer leaves the
            // 6 px ruler on the first drag and its own listeners then stop
            // firing; the root's hitbox is the whole window.
            .on_mouse_move(cx.listener(Self::drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::drag_release))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG_CANVAS()))
            .text_color(rgb(FG_PRIMARY()))
            .text_size(px(12.))
            // Four regions, the arrangement every consumer editor shares:
            // library left, picture centre, inspector right, and the timeline
            // full width along the bottom with its edit toolbar directly above
            // it. Nothing here moves when the state changes -- the regions are
            // fixed and the panels keep their room whether or not anything is
            // open in them.
            // Player fullscreen: the picture and nothing else -- what
            // `Fullscreen` gives every consumer video player. The four-region
            // layout is skipped rather than hidden under it (a hidden library
            // and inspector would still lay out and take the split's own
            // room), and the picture div itself is untouched either way --
            // [`Player::picture_area`] is the same call in both arms.
            .when(self.player_fullscreen, |d| {
                d.child(self.picture_area(position, window, cx))
            })
            .when(!self.player_fullscreen, |d| {
                d.child(self.topbar(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .child(self.library(
                                self.split_px(Split::Library, window.viewport_size()),
                                cx,
                            ))
                            // The seams, one per pair of regions: what a hand
                            // drags to give a panel more room and its
                            // neighbour less ([`divider`]).
                            .child(divider(Split::Library, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(self.picture_area(position, window, cx))
                                    .child(self.transport_bar(state, window.viewport_size(), cx)),
                            )
                            .child(divider(Split::Inspector, cx))
                            // The settings cards live in here rather than over
                            // the timeline: adjusting a clip must not hide the
                            // clip.
                            .child(self.inspector(window.viewport_size(), cx)),
                    )
                    // Above the toolbar rather than under it: the toolbar is
                    // a fixed strip belonging to the timeline, so the pair
                    // moves as one and the edge the hand is pulling is the
                    // edge under the pointer.
                    .child(divider(Split::Timeline, cx))
                    .child(self.toolbar(cx))
                    .child(self.timeline(position, state, window.viewport_size(), cx))
            })
            // Over the region they were opened on, and under the modal cards:
            // they are only ever up while none of those is (`modal`).
            .children(self.context_card(window.viewport_size(), cx))
            .children(self.library_card(window.viewport_size(), cx))
            // The two that are genuinely modal -- the whole registry, and the
            // card that writes a file -- are the only sheets left over the
            // window.
            .children(self.keys_overlay(cx))
            .children(self.export_card(window.viewport_size(), cx))
            // The same sheet once the card has been answered: the running
            // export is the one state in this window where nothing may be
            // edited, so it is drawn as what it is rather than as a strip.
            .children(self.export_progress_card(cx))
            // Last, so it floats over whatever opened it -- an inspector row or
            // a clip menu -- rather than under it.
            .children(self.picker_card(window.viewport_size(), cx)))
        .into_any_element()
    }
}
