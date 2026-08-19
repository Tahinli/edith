//! What the window looks like this frame: the one `render` the whole
//! interface hangs off, root to overlays.

use crate::*;

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

        div()
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
                // And the equalizer card, the same way again. Its own strokes
                // are the card's input, exactly as the export card's digits
                // are: a band reachable only by dragging is a band a keyboard
                // cannot move at all, and every one of them is listed in the
                // keys menu (keymap.rs `FIXED`) rather than being a secret.
                if this.eq_open.is_some() {
                    // Shift makes the two horizontal keys Q instead of
                    // frequency: both are the *width* of the same hump, so they
                    // sit on the same axis rather than on two keys nobody would
                    // guess. Wider is a lower Q, which is why left widens.
                    let shift = event.keystroke.modifiers.shift;
                    if key == ESCAPE {
                        // Nothing to undo: every change is already at the clip,
                        // and undo is undo's own key.
                        this.eq_open = None;
                        this.eq_dragging = false;
                    } else if key == "up" {
                        this.nudge_band(|b| b.gain_db += EQ_STEP, cx);
                    } else if key == "down" {
                        this.nudge_band(|b| b.gain_db -= EQ_STEP, cx);
                    } else if key == "left" && shift {
                        this.nudge_band(|b| b.q /= EQ_Q_STEP, cx);
                    } else if key == "right" && shift {
                        this.nudge_band(|b| b.q *= EQ_Q_STEP, cx);
                    } else if key == "left" {
                        this.nudge_band(|b| b.freq_hz /= EQ_FREQ_STEP, cx);
                    } else if key == "right" {
                        this.nudge_band(|b| b.freq_hz *= EQ_FREQ_STEP, cx);
                    } else if key == "r" {
                        for band in &mut this.eq_params.bands {
                            band.gain_db = 0.;
                        }
                        this.commit_eq(cx);
                    } else if key == "f" {
                        // This one band back to flat, which is the undo of one
                        // hand movement -- `r` is the undo of the whole card.
                        this.nudge_band(|b| b.gain_db = 0., cx);
                    } else if key == "a" {
                        this.add_band(cx);
                    } else if key == "x" {
                        this.remove_band(cx);
                    } else if key == "s" {
                        // The analyser off and on. Nothing is committed: it is
                        // what the card *shows*, so it survives no further than
                        // this window.
                        this.eq_spectrum = !this.eq_spectrum;
                    } else if let Ok(digit) = key.parse::<usize>() {
                        // As the keys are laid out: 1-9 then 0 for the tenth,
                        // which is the cap ([`EQ_BANDS_MAX`]). A digit past the
                        // last band picks nothing rather than panics.
                        let band = match digit {
                            0 => EQ_BANDS_MAX - 1,
                            n => n - 1,
                        };
                        if band < this.eq_params.bands.len() {
                            this.eq_band = band;
                        }
                    }
                    cx.notify();
                    return;
                }
                // The colour card owns the keyboard the same way the export
                // card does, and its keys mean nothing outside it: the arrows
                // pick a slider and move it, and `r` takes the grade off. Not
                // keymap bindings for exactly that reason -- see `FIXED`, where
                // the keys menu still lists them.
                if this.color_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.color_open = None;
                            this.color_dragging = false;
                        }
                        Some(ColorKey::Band(step)) => {
                            this.color_band = (this.color_band + step) % COLOR_BANDS.len();
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_color(steps, cx),
                        Some(ColorKey::Reset) => {
                            this.set_color(ColorParams::default(), cx);
                        }
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The speed card, the same way again: its arrows move the rate
                // and `r` puts it back to real time, and neither means anything
                // outside the card -- so neither is a binding (see `FIXED`,
                // where the keys menu still lists them).
                if this.speed_open.is_some() {
                    match color_key(key) {
                        Some(ColorKey::Close) => {
                            this.speed_open = None;
                            this.speed_dragging = false;
                        }
                        // The card has one value, so the pair that picks a
                        // slider on the colour card moves this one by a whole
                        // preset's worth instead of a step.
                        Some(ColorKey::Band(step)) => {
                            this.nudge_speed(if step == 1 { -2 } else { 2 }, cx)
                        }
                        Some(ColorKey::Nudge(steps)) => this.nudge_speed(steps as i32, cx),
                        Some(ColorKey::Reset) => this.set_speed(Speed::NORMAL, cx),
                        None => {}
                    }
                    cx.notify();
                    return;
                }
                // The silence card, the same way again: the arrows pick one of
                // its rows and move it, and its two apply keys are the two
                // things it can do to the timeline. Card-local, every one of
                // them -- and listed in the keys menu (keymap.rs `FIXED`),
                // because a key that cuts forty places at once is not a secret.
                if this.silence_open.is_some() {
                    if key == ESCAPE {
                        // Nothing to undo: a preview is not an edit.
                        this.close_silence();
                    } else if key == "down" {
                        this.silence_field = (this.silence_field + 1) % SILENCE_ROWS;
                    } else if key == "up" {
                        this.silence_field = (this.silence_field + SILENCE_ROWS - 1) % SILENCE_ROWS;
                    } else if key == "right" {
                        this.nudge_silence(1);
                    } else if key == "left" {
                        this.nudge_silence(-1);
                    } else if key == "enter" {
                        this.cut_silences(cx);
                    } else if key == "f" {
                        this.speed_silences(cx);
                    }
                    cx.notify();
                    return;
                }
                // The mix card, the same way again: ↑↓ pick a row -- a track's
                // fader, the limiter's ceiling or its switch -- and ←→ move it,
                // held or pressed. Card-local like the four above it.
                if this.mix_open {
                    let rows = this.mix_lanes().len() + MIX_MASTER_ROWS;
                    if key == ESCAPE {
                        this.mix_open = false;
                    } else if key == "down" {
                        this.mix_field = (this.mix_field + 1) % rows;
                    } else if key == "up" {
                        this.mix_field = (this.mix_field + rows - 1) % rows;
                    } else if key == "right" {
                        this.nudge_mix(1, cx);
                    } else if key == "left" {
                        this.nudge_mix(-1, cx);
                    }
                    cx.notify();
                    return;
                }
                // The subtitle style card, the same way again: row 0 is the
                // size stepper (←→ moves it, held or pressed, the mix card's
                // rule) and every row after it a family in
                // `subtitle_fonts` -- ↑↓ walk the whole list and `enter`
                // picks the one the arrows are on, since a hand with no
                // mouse still has to be able to leave the platform default.
                if this.subtitle_style_open {
                    // Row 0 is the size stepper, row 1 the platform default,
                    // and every row after it a family in `subtitle_fonts`.
                    let rows = 2 + this.subtitle_fonts.len();
                    if key == ESCAPE {
                        this.subtitle_style_open = false;
                    } else if key == "down" {
                        this.subtitle_style_field = (this.subtitle_style_field + 1) % rows;
                    } else if key == "up" {
                        this.subtitle_style_field =
                            (this.subtitle_style_field + rows - 1) % rows;
                    } else if key == "right" {
                        this.nudge_sub_size(1, cx);
                    } else if key == "left" {
                        this.nudge_sub_size(-1, cx);
                    } else if key == "enter" && this.subtitle_style_field == 1 {
                        this.set_sub_family(None, cx);
                    } else if key == "enter" && this.subtitle_style_field > 1 {
                        let family = this.subtitle_fonts[this.subtitle_style_field - 2].clone();
                        this.set_sub_family(Some(family), cx);
                    }
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
            .child(self.topbar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.library(
                        self.split_px(Split::Library, window.viewport_size()),
                        cx,
                    ))
                    // The seams, one per pair of regions: what a hand drags to
                    // give a panel more room and its neighbour less
                    // ([`divider`]).
                    .child(divider(Split::Library, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_hidden()
                                    // The bed the cue plate is placed against:
                                    // it hangs off the bottom of the picture
                                    // region, which is the one box that is the
                                    // picture and nothing else.
                                    .relative()
                                    .flex()
                                    .justify_center()
                                    .items_center()
                                    .bg(rgb(BG_CANVAS()))
                                    .children(
                                        self.image
                                            .clone()
                                            .map(|i| {
                                                img(i)
                                                    .size_full()
                                                    .object_fit(gpui::ObjectFit::Contain)
                                                    .into_any_element()
                                            })
                                            // With no file open the letterbox
                                            // is the whole region, and a black
                                            // rectangle says only that
                                            // something is broken -- so it says
                                            // what it wants instead. The window
                                            // is already the drop target.
                                            .or_else(|| {
                                                self.session
                                                    .is_none()
                                                    .then(|| empty_hint().into_any_element())
                                            }),
                                    )
                                    // After the picture, so the plate is drawn
                                    // over it rather than under.
                                    .children(self.subtitle_overlay(position, window))
                                    .children(self.preview_badge(cx))
                                    // The three transient lines hang off the
                                    // bottom of the picture rather than taking
                                    // a row of the column: a notice that
                                    // arrives must not push the transport, the
                                    // toolbar and the timeline down by its own
                                    // height -- which is a control moving with
                                    // state, on every control below it at once.
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
                                            .children(self.notice_bar(cx))
                                            // How tall the lot came out, for the
                                            // cue plate above to step over
                                            // ([`sub_bottom`]) -- the bars are
                                            // over the picture, and a message
                                            // drawn across the line being read
                                            // loses both of them. Zero with no
                                            // bar up, since this box is then
                                            // empty.
                                            .child(height_probe(self.notice_h.clone())),
                                    ),
                            )
                            .child(self.transport_bar(state, window.viewport_size(), cx)),
                    )
                    .child(divider(Split::Inspector, cx))
                    // The settings cards live in here rather than over the
                    // timeline: adjusting a clip must not hide the clip.
                    .child(self.inspector(window.viewport_size(), cx)),
            )
            // Above the toolbar rather than under it: the toolbar is a fixed
            // strip belonging to the timeline, so the pair moves as one and the
            // edge the hand is pulling is the edge under the pointer.
            .child(divider(Split::Timeline, cx))
            .child(self.toolbar(cx))
            .child(self.timeline(position, state, window.viewport_size(), cx))
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
            .children(self.picker_card(window.viewport_size(), cx))
    }
}
