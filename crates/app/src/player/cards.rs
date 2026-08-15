//! The cards the window opens over itself: the pickers, colour, speed,
//! silence, the mixer and the equalizer.

use crate::*;

impl Player {
    /// Cycles the *project's* resolution through [`RESOLUTIONS`], starting from
    /// the media's own -- the one size that must stay reachable, since a project
    /// moved off it has no other way back (the resolution is not an undo step).
    /// Every clip is recomposed onto it, so this is what makes "the project
    /// resolution and the media's are different things" a thing a user can see.
    pub(crate) fn cycle_resolution(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to resize — open a file first".into());
            cx.notify();
            return;
        };
        let (width, height) = next_resolution(session.resolution(), session.native_resolution());
        self.apply_resolution(width, height, cx);
    }

    /// The project resized, whichever asked: the stroke that steps to the next
    /// size and the list that names one outright come through here.
    pub(crate) fn apply_resolution(&mut self, width: u32, height: u32, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_resolution(width, height)
        {
            self.notify_user(format!("PROJECT: {width}x{height}").into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project cut at another rate: the list names one and this is where it
    /// happens, the way [`apply_resolution`](Self::apply_resolution) is for a
    /// size. The whole timeline is conformed to it by the engine
    /// ([`PlaybackSession::set_frame_rate`]) -- same seconds, same footage --
    /// and the rate the app itself counts frames in follows, since every
    /// timecode, ruler mark and step key here is measured in it.
    pub(crate) fn apply_frame_rate(&mut self, fps: f64, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_frame_rate(fps)
        {
            self.fps = session.meta().frame_rate;
            self.notify_user(format!("PROJECT: {} fps", fps_label(fps)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The project's HDR media shown another way: the list names a rendition and
    /// this is where it happens, the way [`apply_resolution`](Self::apply_resolution)
    /// is for a size. The engine remaps the frame under the playhead at once
    /// ([`PlaybackSession::set_tone`]), so the picture on screen is the picked
    /// one before the notice has faded -- and an SDR project is unmoved, which
    /// is what the notice says rather than pretending something happened.
    pub(crate) fn apply_tone(&mut self, preset: Preset, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_tone(preset)
        {
            self.notify_user(format!("HDR: {} — affects HDR media", tone_label(preset)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Opens a choice list on a setting, where it was asked for. One floating
    /// thing at a time: the click that opens it is the click that closes
    /// whatever menu it was opened from.
    pub(crate) fn open_picker(&mut self, of: Pick, at: Point<Pixels>, cx: &mut Context<Self>) {
        // On the row that is in force, so the first ↑ or ↓ steps off the
        // current value rather than off the top of the list.
        let sel = self
            .choices(of)
            .iter()
            .position(|(.., picked)| *picked)
            .unwrap_or(0);
        self.context_menu = None;
        self.library_menu = None;
        self.picker = Some(Picker { of, at, sel });
        cx.notify();
    }

    /// A row of the open list was picked. Closes the list first -- the rule
    /// every menu item here follows -- then does exactly what the stroke for
    /// that setting does, through the same door.
    pub(crate) fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.picker = None;
        match choice {
            Choice::Size(w, h) => self.apply_resolution(w, h, cx),
            Choice::Fps(fps) => self.apply_frame_rate(fps, cx),
            Choice::Fit(lane, idx, fit) => self.apply_fit(lane, idx, fit, cx),
            Choice::Tone(preset) => self.apply_tone(preset, cx),
            // The project's, like the tone map above and unlike the palette
            // below: kept in the `.edith` and read straight back out of the
            // session by the card and by the export.
            Choice::Encoder(seat) => self.apply_encoder(seat, cx),
            // In force for the next paint -- every token is read through
            // [`ui::theme::palette`], so one store repaints the whole window --
            // and kept for the next launch. A file that could not be written is
            // said out loud: the difference between "picked" and "picked for
            // good" is the user's to know.
            Choice::Theme(id) => {
                ui::theme::set(id);
                if let Err(e) = ui::theme::save(id) {
                    let path = ui::theme::config_path();
                    self.notify_user(
                        format!("THEME COULD NOT BE KEPT — {} — {e}", path.display()).into(),
                    );
                }
                cx.notify();
            }
            // The same field the row's key steps, set outright: a list picks a
            // value, it does not step to one.
            Choice::AudioRate(kbps) => {
                self.audio_kbps = kbps;
                cx.notify();
            }
        }
    }

    /// Every value the open list offers, in the order it lists them. Empty
    /// without a timeline, which is the state where nothing here has a value to
    /// offer -- and where the surfaces that open the list are dimmed anyway.
    pub(crate) fn choices(&self, of: Pick) -> Vec<ChoiceRow> {
        // The palette is not the project's, so it is offered before the
        // timeline is asked about: an empty window is painted too, and its
        // Theme button is live there like the snap beside it.
        if of == Pick::Theme {
            return ui::theme::PaletteId::ALL
                .into_iter()
                .map(|id| {
                    (
                        Choice::Theme(id),
                        id.label().into(),
                        id.detail().into(),
                        id == ui::theme::active(),
                    )
                })
                .collect();
        }
        let Some(session) = &self.session else {
            return Vec::new();
        };
        match of {
            Pick::Resolution => {
                resolution_choices(session.resolution(), session.native_resolution())
            }
            Pick::Fps => fps_choices(session.meta().frame_rate, session.native_frame_rate()),
            Pick::Fit(lane, idx) => {
                fit_choices(lane, idx, session.fit_of(lane, idx), session.resolution())
            }
            Pick::AudioRate => audio_rate_choices(self.audio_kbps),
            Pick::Tone => tone_choices(session.tone()),
            Pick::Encoder => encoder_choices(session.encoder_seat()),
            // Answered above, with or without a timeline.
            Pick::Theme => Vec::new(),
        }
    }

    /// Opens the colour card on the clip a grade would go on: the clip that was
    /// clicked when it is a video one, and otherwise the clip the picture is
    /// coming from -- the one the engine's own compositing rule picks, which is
    /// what a person means by "this shot". The fallback stands even now that a
    /// selection key exists: a grade asked for with nothing selected still means
    /// the shot on screen.
    pub(crate) fn open_color(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to grade — open a file first".into());
            cx.notify();
            return;
        };
        let target = self
            .selected
            .anchor()
            .filter(|(lane, _)| lane.kind == LaneKind::Video)
            .or_else(|| session.video_clip_at(session.now()));
        match target {
            Some(clip) => {
                self.color_open = Some(clip);
                self.color_band = 0;
                self.color_dragging = false;
                // A sample the last card held back belongs to the clip it was
                // dragged on, and this may be another one.
                self.pending_color = None;
                // One card at a time, the rule both the others already follow.
                self.keys_open = false;
                self.export_open = false;
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to grade".into()),
        }
        cx.notify();
    }

    /// What the card's clip is graded by right now -- the identity for one
    /// nobody has graded, which is what the sliders start at. A sample a drag is
    /// still holding wins over the clip's own: it is what the hand has asked
    /// for, so it is what the sliders show and what the next sample builds on.
    pub(crate) fn color_params(&self) -> ColorParams {
        if let Some(params) = self.pending_color {
            return params;
        }
        self.color_open
            .zip(self.session.as_ref())
            .and_then(|((lane, idx), session)| session.color_of(lane, idx).copied())
            .unwrap_or_default()
    }

    /// Puts `params` on the card's clip, or takes the grade off when they are
    /// the identity -- a slider walked back to the middle leaves the clip
    /// ungraded rather than carrying a do-nothing entry, which is what keeps an
    /// untouched project byte-identical. The engine reseeks on the edit, so the
    /// frame on screen repaints through the new grade; this only owes the flags
    /// that reseek clears.
    pub(crate) fn set_color(&mut self, params: ColorParams, cx: &mut Context<Self>) {
        self.write_color(params, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through
    /// (`PlaybackSession::set_color_live`). Either way the engine reseeks, so
    /// the picture -- and the histogram counted off it -- is regraded at once.
    pub(crate) fn write_color(&mut self, params: ColorParams, live: bool, cx: &mut Context<Self>) {
        // Any write supersedes a held sample, whichever way it arrived -- a key,
        // a reset, or the flush that took this one out of the stash.
        self.pending_color = None;
        let Some((lane, idx)) = self.color_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let grade = Some(params).filter(|p| !p.is_identity());
        let took = match live {
            true => session.set_color_live(lane, idx, grade),
            false => session.set_color(lane, idx, grade),
        };
        if took {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Moves the picked slider by `steps` of [`COLOR_STEP`], clamped to that
    /// band's range. One edit, so one undo step per press.
    pub(crate) fn nudge_color(&mut self, steps: f32, cx: &mut Context<Self>) {
        let mut params = self.color_params();
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let value = band_mut(&mut params, self.color_band);
        *value = (*value + steps * COLOR_STEP).clamp(low, high);
        self.set_color(params, cx);
    }

    /// Where the pointer sits along a slider, as that band's value: the left end
    /// of the bar is the bottom of its range and the right end the top. Called
    /// on every pointer sample, so the grade -- and the picture, and the
    /// histogram over it -- moves under the hand.
    ///
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live. That is why it writes even when
    /// the value did not change -- without that snapshot the rest of the drag
    /// would be unundoable.
    ///
    /// Values land on the [`COLOR_STEP`] grid the keys use, which also bounds
    /// one drag to forty-odd entries in the project's colour table.
    ///
    /// Samples crossed while the worker still owes a frame are held rather than
    /// written ([`stash_or_write`]): a reopen costs half a second on a big film,
    /// so a bar-wide sweep that wrote every step would queue forty opens, cancel
    /// thirty-nine of them and freeze the window for the sum. What is written is
    /// one grade per frame the worker actually delivers.
    pub(crate) fn drag_color(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let (_, low, high) = COLOR_BANDS[self.color_band];
        let along = frac_along(x, self.color_bars[self.color_band].get());
        let value = color_snap(low + along * (high - low)).clamp(low, high);
        let mut params = self.color_params();
        let at = band_mut(&mut params, self.color_band);
        if *at == value && !first {
            return;
        }
        *at = value;
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_color, params, first, busy) {
            Some(params) => self.write_color(params, !first, cx),
            // The sliders draw off the held sample, so the handle goes on
            // following the hand while the picture catches up.
            None => cx.notify(),
        }
    }

    /// Opens the speed card on the clip whose rate is to change: the selected
    /// one, or -- with nothing selected -- the clip the picture is coming from,
    /// which is what a person means by "this shot". Either half of a take will
    /// do: a rate applies to the whole group, so opening it on the sound and
    /// opening it on the picture are the same card.
    pub(crate) fn open_speed(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to re-time — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
        {
            Some(clip) => {
                self.speed_open = Some(clip);
                self.speed_dragging = false;
                // The colour card's rule: a held sample is the last clip's.
                self.pending_speed = None;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.close_silence();
                self.context_menu = None;
            }
            None => self.notify_user("no clip under the playhead to re-time".into()),
        }
        cx.notify();
    }

    /// What the card's clip plays at right now -- real time for one nobody has
    /// touched, which is where the bar starts.
    pub(crate) fn card_speed(&self) -> Speed {
        if let Some(speed) = self.pending_speed {
            return speed;
        }
        self.speed_open
            .zip(self.session.as_ref())
            .map_or(Speed::NORMAL, |((lane, idx), session)| {
                session.speed_of(lane, idx)
            })
    }

    /// Writes a rate at the card's clip and its whole group -- one undo step for
    /// the lot ([`engine::PlaybackSession::set_speed`]). The engine reseeks, so
    /// the picture runs at the new rate and the sound is resampled from the next
    /// chunk on; a refusal (a slower clip would run into its neighbour) comes
    /// back in the engine's own words and *names* the clip in the way, because
    /// "it did not fit" is not something a person can go and fix.
    pub(crate) fn set_speed(&mut self, speed: Speed, cx: &mut Context<Self>) {
        self.write_speed(speed, false, cx);
    }

    /// Both writes: `live` is the one that takes no undo step, which is what
    /// every sample *inside* a drag goes through -- so a drag from 1.00x to
    /// 2.00x is one undo press and lands back where the hand picked it up, and
    /// the whole linked group comes back with it.
    pub(crate) fn write_speed(&mut self, speed: Speed, live: bool, cx: &mut Context<Self>) {
        // The colour card's rule: a write supersedes whatever a drag was holding.
        self.pending_speed = None;
        let Some((lane, idx)) = self.speed_open else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        if speed != session.speed_of(lane, idx) {
            let wrote = match live {
                true => session.set_speed_live(lane, idx, speed),
                false => session.set_speed(lane, idx, speed),
            };
            match wrote {
                Ok(()) => self.reset_after_reseek(),
                Err(e) => self.notify_user(e.to_string().into()),
            }
        }
        cx.notify();
    }

    /// One [`SPEED_STEP`] per keystroke, clamped to what a [`Speed`] can hold.
    pub(crate) fn nudge_speed(&mut self, steps: i32, cx: &mut Context<Self>) {
        let at = i32::from(self.card_speed().permille()) + steps * SPEED_STEP;
        self.set_speed(speed_at(at), cx);
    }

    /// Where the pointer sits along the bar, as a rate: the left end is
    /// [`Speed::MIN`] and the right end [`Speed::MAX`], on the same
    /// [`SPEED_STEP`] grid the keys move on -- so a drag can land on exactly
    /// 1.00x and the same drag twice is one entry, not forty.
    /// `first` is the press: it takes the undo step the whole gesture rolls back
    /// to, and every sample after it is live -- the colour card's rule, for the
    /// colour card's reason.
    pub(crate) fn drag_speed(&mut self, x: Pixels, first: bool, cx: &mut Context<Self>) {
        let along = frac_along(x, self.speed_bar.get());
        let lo = f32::from(Speed::MIN.permille());
        let hi = f32::from(Speed::MAX.permille());
        let raw = lo + along * (hi - lo);
        // Snapped to the grid, then to real time itself when it is within half a
        // step of it: 1.00x is the one value a hand must be able to hit, and
        // nothing about the bar's geometry guarantees a pixel lands on it.
        let stepped = (raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP;
        // Held back while the worker is busy, the colour card's way and for a
        // sharper reason: a live rate also restarts the sound, so a sweep that
        // wrote every step would restart it forty times.
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_speed, speed_at(stepped), first, busy) {
            Some(speed) => self.write_speed(speed, !first, cx),
            None => cx.notify(),
        }
    }

    /// Writes what a slider drag held back, now that the worker has delivered.
    /// The gate is the frame that landed and never a timer: a 100 ms tick
    /// ([`SCRUB_GAP`]) says nothing about a reopen that costs half a second, and
    /// a drag gated on one would still queue opens nobody sees.
    ///
    /// Called again by the release, where readiness is beside the point: the
    /// value the hand let go on is owed whatever the worker is doing, and a
    /// gesture may not end on a sample that was dropped.
    pub(crate) fn flush_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(params) = self.pending_color.take() {
            self.write_color(params, true, cx);
        }
        if let Some(speed) = self.pending_speed.take() {
            self.write_speed(speed, true, cx);
        }
    }

    /// Opens the silence card on the clip to be scanned: the selected one, or
    /// -- with nothing selected -- the clip the picture is coming from, which is
    /// the rule the speed card follows and what a person means by "this shot".
    /// Either half of a take will do: both halves of an A/V take name the same
    /// file and play the same source frames, which is the whole of what a scan
    /// is of ([`ScanKey`]).
    ///
    /// The card is up on the next frame whatever the file is: a still is
    /// refused by name here, where the answer costs a look at the path, and
    /// everything the decoder has to open the file to know -- a track that is
    /// not there, a read that fails -- is refused the same way when the scan
    /// lands, because a fifty-second decode is not a thing to open a card
    /// behind ([`Player::start_silence_scan`]).
    pub(crate) fn open_silence(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &self.session else {
            self.notify_user("no timeline to scan — open a file first".into());
            cx.notify();
            return;
        };
        match self
            .selected
            .anchor()
            .or_else(|| session.video_clip_at(session.now()))
            .map(|clip| audio_half(session, clip))
        {
            Some((lane, idx)) => {
                let found = self.session.as_ref().and_then(|session| {
                    let clip = *session.lane_clips(lane).get(idx)?;
                    Some((session.sources().get(clip.source)?.clone(), clip))
                });
                // A still is asked *before* the decoder is: handing a png to the
                // mp4 demuxer answers "a box with a larger size than it", which
                // is a true sentence about a container and nothing a person can
                // act on. A picture has no sound for the same reason a silent
                // video has none, so it is refused in the same words.
                let Some((source, clip)) = found else {
                    cx.notify();
                    return;
                };
                if engine::is_image(&source.path) {
                    self.notify_user(unscannable(lane, idx, &source.path).into());
                    cx.notify();
                    return;
                }
                self.silence_open = Some((lane, idx));
                self.silence_field = 0;
                // One card at a time, the rule the other four follow.
                self.keys_open = false;
                self.export_open = false;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.context_menu = None;
                // The clip's own range, not the file's: the scan reads what this
                // clip plays and nothing else, so a take cut in half costs half
                // the decode and finds only what is still on the timeline.
                let key = (
                    source.path.clone(),
                    source.audio_stream,
                    clip.in_frame,
                    clip.out_frame,
                );
                match scan_plan(
                    self.silence_levels.contains_key(&key),
                    self.silence_scan.as_ref().map(|scan| &scan.key),
                    &key,
                ) {
                    ScanPlan::Marks => self.scan_silences(),
                    ScanPlan::Start => self.start_silence_scan(key, cx),
                    ScanPlan::Wait => {}
                }
            }
            None => self.notify_user("no clip under the playhead to scan".into()),
        }
        cx.notify();
    }

    /// Opens the mix card. `lane` is the row it lands on -- the track whose
    /// header was clicked -- and `None` starts at the top, which is what the
    /// stroke means.
    ///
    /// Nothing here is a clip's, so nothing is refused for want of a selection:
    /// a timeline with no audio track at all still has a limiter to set, and a
    /// fader on an empty track is the level the next take lands at.
    pub(crate) fn open_mix(&mut self, lane: Option<Lane>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.mix_open = true;
        self.mix_field = lane
            .and_then(|lane| self.mix_lanes().iter().position(|&l| l == lane))
            .unwrap_or(0);
        // One card at a time, the rule the other five follow.
        self.keys_open = false;
        self.export_open = false;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.context_menu = None;
        cx.notify();
    }

    /// The audio tracks the card shows a fader for, top to bottom: *every* one
    /// of them, empty ones included -- what the timeline lays out, not what the
    /// mixer happens to open (`Project::audio_lanes` leaves an empty track out,
    /// and a fader that disappeared when a track was cleared would be a setting
    /// nobody could reach).
    pub(crate) fn mix_lanes(&self) -> Vec<Lane> {
        self.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio)
                .collect()
        })
    }

    /// Moves the row the card has picked: a fader by [`MIX_DB_STEP`], the
    /// ceiling by the same, and the switch either way (a ring of two, like the
    /// silence card's unit row).
    ///
    /// Every one of them goes through the session, which hands it straight to
    /// the running mixer: what the ear hears while the arrow is held is the mix
    /// that is being set, and nothing is rebuilt to make that true -- no reseek,
    /// so no `reset_after_reseek` and no blink in the picture behind the card
    /// ([`engine::PlaybackSession::set_lane_gain_db`]).
    pub(crate) fn nudge_mix(&mut self, steps: i32, cx: &mut Context<Self>) {
        let lanes = self.mix_lanes();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match lanes.get(self.mix_field) {
            Some(&lane) => {
                let at = session.lane_gain_db(lane) + steps as f32 * MIX_DB_STEP;
                session.set_lane_gain_db(lane, at);
            }
            None => {
                let limiter = session.limiter();
                let at = match self.mix_field - lanes.len() {
                    0 => Limiter {
                        on: limiter.on,
                        ..limiter
                    }
                    .with_ceiling(limiter.ceiling_db + steps as f32 * MIX_DB_STEP),
                    _ => Limiter {
                        on: !limiter.on,
                        ..limiter
                    },
                };
                session.set_limiter(at);
            }
        }
        cx.notify();
    }

    /// Closes it and drops the preview with it: marks left on the lane after
    /// the card is gone would name frames the next edit has already moved.
    pub(crate) fn close_silence(&mut self) {
        self.silence_open = None;
        self.silence_marks.clear();
        self.cancel_silence_scan();
    }

    /// Tells the worker nobody is waiting any more. It gives up at its next
    /// chunk and the levels it had are dropped: half a track is not an answer,
    /// and the flag stays set on the [`Arc`] the landing closure holds, which is
    /// how that closure knows to keep its hands off the card.
    pub(crate) fn cancel_silence_scan(&mut self) {
        if let Some(scan) = self.silence_scan.take() {
            scan.progress
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hands the decode to a worker and returns at once -- the card is drawn by
    /// the very next frame, saying it is scanning. Fifty-one seconds on a 25 GB
    /// film is what this used to cost on the render thread, with the card
    /// marked open and nothing on screen.
    ///
    /// Whatever was scanning is cancelled first: one card, one scan, and the
    /// clip that has just been asked about is the one worth the disk.
    ///
    /// Only the clip's own `[in, out)` is read -- source frames over the
    /// project's rate, the same seconds [`engine::Project`] hands the decoder
    /// for playback -- so half a take is half a wait.
    pub(crate) fn start_silence_scan(&mut self, key: ScanKey, cx: &mut Context<Self>) {
        self.cancel_silence_scan();
        self.silence_marks.clear();
        let progress = Arc::new(engine::silence::Progress::default());
        let range = source_secs(&key, self.fps);
        let scan = cx.background_executor().spawn({
            let (key, progress) = (key.clone(), Arc::clone(&progress));
            async move { engine::silence::levels_with_progress(&key.0, key.1, range, &progress) }
        });
        let now = Instant::now();
        self.silence_scan = Some(SilenceScan {
            key: key.clone(),
            started: now,
            progress: Arc::clone(&progress),
            seen: 0,
            since: now,
        });
        cx.spawn(async move |this, cx| {
            let landed = scan.await;
            this.update(cx, |this, cx| {
                // Cancelled means the card moved on or closed: the levels are a
                // prefix of a track nobody asked about any more.
                if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.silence_scan = None;
                match landed {
                    Ok(Some(levels)) => {
                        this.silence_levels.insert(key.clone(), Arc::new(levels));
                        this.scan_silences();
                    }
                    // A source with no audio track is not one long silence: it
                    // is a clip this card has nothing to say about, named so the
                    // user knows which one it meant.
                    Ok(None) => {
                        if let Some((lane, idx)) = this.silence_open {
                            this.notify_user(unscannable(lane, idx, &key.0).into());
                        }
                        this.close_silence();
                    }
                    Err(e) => {
                        this.close_silence();
                        this.notify_user(format!("SCAN FAILED: {e}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the scanning line's stall clock, for [`Player::poll_import`]'s
    /// reason: sampled once per frame rather than while drawing.
    pub(crate) fn poll_silence(&mut self) {
        if let Some(scan) = &mut self.silence_scan {
            scan.poll();
        }
    }

    /// Applies the settings to levels already in hand and replaces the preview
    /// -- never stacks on it. Arithmetic only: the decode is
    /// [`Player::start_silence_scan`]'s and happens once per source, so every
    /// run here is numbers already read, which is what makes moving a threshold
    /// feel like moving a slider. A source still being scanned has no marks yet
    /// and says so on the card.
    ///
    /// Changes nothing about the project: a preview is not an edit, and no undo
    /// step is spent until a button is pressed.
    pub(crate) fn scan_silences(&mut self) {
        let Some((lane, idx)) = self.silence_open else {
            return;
        };
        self.silence_marks.clear();
        // Copied out before anything is written back: the cache below lives on
        // the same struct the session does.
        let Some((clip, source)) = self.session.as_ref().and_then(|session| {
            let clip = *session.lane_clips(lane).get(idx)?;
            Some((clip, session.sources().get(clip.source)?.clone()))
        }) else {
            return;
        };
        // Nothing read yet: the worker is running and the card is drawing its
        // line. The marks arrive with the levels.
        let Some(levels) = self
            .silence_levels
            .get(&(
                source.path.clone(),
                source.audio_stream,
                clip.in_frame,
                clip.out_frame,
            ))
            .cloned()
        else {
            return;
        };
        self.silence_marks = engine::silence::timeline_regions(
            &clip,
            self.fps,
            &engine::silence::regions(&levels, self.silence),
        );
    }

    /// Moves the picked row by `steps` and re-runs the scan against it, so the
    /// marks on the lane are always what the numbers on the card say.
    pub(crate) fn nudge_silence(&mut self, steps: i32) {
        let secs = |at: f64| {
            (at + f64::from(steps) * SILENCE_SECS_STEP)
                .clamp(SILENCE_SECS_RANGE.0, SILENCE_SECS_RANGE.1)
        };
        match self.silence_field {
            // Round either way, like the fit policy's cycle: three choices are
            // a ring, not a range.
            0 => {
                let at = SCOPES.iter().position(|&s| s == self.silence_scope);
                let step = steps.rem_euclid(SCOPES.len() as i32) as usize;
                self.silence_scope = SCOPES[(at.unwrap_or(0) + step) % SCOPES.len()];
            }
            1 => {
                self.silence.threshold_db = (self.silence.threshold_db
                    + steps as f32 * SILENCE_DB_STEP)
                    .clamp(SILENCE_DB_RANGE.0, SILENCE_DB_RANGE.1)
            }
            // Two spellings of the same level, so either arrow flips it -- a
            // ring of two, like the scope row's.
            2 => self.silence_dbfs = !self.silence_dbfs,
            3 => self.silence.min_silence = secs(self.silence.min_silence),
            4 => self.silence.padding = secs(self.silence.padding),
            5 => self.silence.min_keep = secs(self.silence.min_keep),
            _ => {
                self.silence_factor =
                    silence_rate(i32::from(self.silence_factor.permille()) + steps * SPEED_STEP)
            }
        }
        // Neither the scope nor the rate is part of the scan, but re-running is
        // cheap (the levels are cached) and one path is one place for the marks
        // to come from.
        self.scan_silences();
    }

    /// Which lanes an apply reaches, as the card's scope row says it: the
    /// lanes of the take the scanned clip belongs to, that clip's lane alone,
    /// or every lane there is.
    ///
    /// The take's lanes are the ones carrying its group id -- a link is one
    /// span on however many lanes, so "the take" is exactly the set of lanes
    /// that would otherwise be pulled apart. Nothing widens behind the user's
    /// back: [`Project::cut_regions`] refuses a scope that would split a take,
    /// and this row is how the user says the take instead.
    pub(crate) fn silence_lanes(&self) -> Vec<Lane> {
        let (Some((lane, idx)), Some(session)) = (self.silence_open, self.session.as_ref()) else {
            return Vec::new();
        };
        match self.silence_scope {
            Scope::Track => vec![lane],
            Scope::Everything => session.lanes(),
            Scope::Take => match session.lane_clips(lane).get(idx).and_then(|c| c.link) {
                None => vec![lane],
                Some(id) => session
                    .lanes()
                    .into_iter()
                    .filter(|&l| {
                        l == lane || session.lane_clips(l).iter().any(|c| c.link == Some(id))
                    })
                    .collect(),
            },
        }
    }

    /// What an apply acts on: the previewed set and the lanes it reaches, or
    /// nothing at all with a notice saying so in the numbers that found
    /// nothing.
    pub(crate) fn previewed(&mut self) -> Option<(Vec<(u32, u32)>, Vec<Lane>)> {
        if self.silence_marks.is_empty() {
            self.notify_user(
                format!(
                    "no silence under {:.0} dBFS lasting {:.2} s — raise the threshold or forgive less",
                    self.silence.threshold_db, self.silence.min_silence
                )
                .into(),
            );
            return None;
        }
        Some((self.silence_marks.clone(), self.silence_lanes()))
    }

    /// What an apply says afterwards: which tracks it reached, and -- when that
    /// was not all of them -- that the rest were left where they were. The
    /// scope is a choice, so the confirmation has to name the choice.
    pub(crate) fn silence_reach(&self, lanes: &[Lane]) -> String {
        let named = lanes
            .iter()
            .map(|l| l.label())
            .collect::<Vec<_>>()
            .join("+");
        match self.silence_scope {
            Scope::Everything => "on every track".to_string(),
            _ => format!("on {named} — other tracks untouched"),
        }
    }

    /// Cuts every previewed silence out of the lanes the scope names, rippling
    /// each hole closed -- one edit and **one** undo press however many there
    /// were ([`engine::PlaybackSession::cut_regions`]). Tracks outside the
    /// scope do not move; a scope that would take half a take with it comes
    /// back refused in the engine's own words, naming both halves.
    pub(crate) fn cut_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let saved = f64::from(regions.iter().map(|&(_, len)| len).sum::<u32>()) / self.fps;
        let (count, reach) = (regions.len(), self.silence_reach(&lanes));
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.cut_regions(&regions, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Every hole closed moves the clips after it up a place, so the
                // selection now names a different clip than the one that is
                // highlighted -- dropped here as after every other edit that
                // moves indexes (a delete, a paste, an undo).
                self.selected.clear();
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES CUT {reach} — {} shorter, {} takes it back",
                        secs_label(saved),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Plays them fast instead of cutting them, closing the room each one no
    /// longer needs. One undo press like the cut, and the same scope; the
    /// refusals (a clip lapping over a silence, a scope that would split a
    /// take) come back in the engine's own words and name the lane and frame,
    /// and the card stays up so the numbers that produced it are still on
    /// screen.
    pub(crate) fn speed_silences(&mut self, cx: &mut Context<Self>) {
        let Some((regions, lanes)) = self.previewed() else {
            cx.notify();
            return;
        };
        let (count, rate) = (regions.len(), self.silence_factor);
        let reach = self.silence_reach(&lanes);
        let Some(session) = self.session.as_mut() else {
            cx.notify();
            return;
        };
        match session.speed_regions(&regions, rate, &lanes) {
            Ok(()) => {
                self.close_silence();
                // Splitting each silence out and closing the room it no longer
                // needs moves indexes exactly as the cut does: the selection
                // goes with them.
                self.selected.clear();
                self.reset_after_reseek();
                self.notify_user(
                    format!(
                        "{count} SILENCES AT {rate} {reach} — {} takes it back",
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Whether a card owns the window. While one does the timeline under it is
    /// out of reach, so a right-click there opens no menu -- the same rule the
    /// key handler and the drop target already follow.
    /// Whether anything at all is drawn over the window -- a card, a menu or an
    /// open list. What the hover labels stand aside for ([`OVERLAID`]): a
    /// tooltip belongs to the surface the pointer is on, and while one of these
    /// is up that surface is behind it.
    pub(crate) fn overlaid(&self) -> bool {
        self.modal()
            || self.context_menu.is_some()
            || self.library_menu.is_some()
            || self.picker.is_some()
    }

    pub(crate) fn modal(&self) -> bool {
        self.keys_open
            || self.export_open
            || self.eq_open.is_some()
            || self.color_open.is_some()
            || self.speed_open.is_some()
            || self.silence_open.is_some()
            || self.mix_open
            || self.exporting().is_some()
    }

    /// The pointer's way out of whatever card is up: what every scrim's press
    /// calls, so `esc` is *a* way out and never the only one. One list, and the
    /// same one [`Player::modal`] reads -- a card that can be counted there but
    /// not closed here is a card a hand alone cannot shut, which is what
    /// `every_card_closes_without_the_keyboard` fails on.
    ///
    /// Every card at once because only one is ever up (`export_open`): closing
    /// "the" card and closing all of them are the same act.
    pub(crate) fn close_card(&mut self) {
        self.keys_open = false;
        self.keys_search.clear();
        self.rebinding = None;
        self.export_open = false;
        // The two things typed *into* the export card go with it: a field left
        // open would take the next keystroke for a card that is gone.
        self.mbps_edit = None;
        self.picker = None;
        self.eq_open = None;
        self.eq_dragging = false;
        self.color_open = None;
        self.speed_open = None;
        // Marks and a running scan go with this one, which is why it is a call
        // and not an assignment ([`Player::close_silence`]).
        if self.silence_open.is_some() {
            self.close_silence();
        }
        self.mix_open = false;
    }

    /// Opens the equalizer on the selected clip. Audio only, and it says so
    /// rather than opening a card of bands that would reach nothing: a video
    /// clip carries no sound of its own here (the sound is the audio lane's),
    /// and the model would take the setting without anything ever playing it.
    pub(crate) fn open_eq(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let refusal = match (self.selected.anchor(), &self.session) {
            (_, None) => Some("NO TIMELINE — open a file first".to_string()),
            (None, _) => Some(format!(
                "NOTHING SELECTED — click an audio clip or press {}, then ask again",
                self.keymap.display(ActionId::Select)
            )),
            (Some((lane, _)), _) if lane.kind != LaneKind::Audio => Some(
                "NOT AN AUDIO CLIP — the equalizer works on the sound, so pick a clip in an audio lane".to_string(),
            ),
            _ => None,
        };
        if let Some(refusal) = refusal {
            self.notify_user(refusal.into());
            cx.notify();
            return;
        }
        let (lane, idx) = self.selected.anchor().expect("checked above");
        let session = self.session.as_ref().expect("checked above");
        // What the clip already plays through, or the flat default -- so the
        // card opens on the curve that is in force and a reopen shows the last
        // drag rather than a fresh set of zeroes.
        self.eq_params = session
            .eq_of(lane, idx)
            .cloned()
            .unwrap_or_else(EqParams::default_layout);
        self.eq_band = 0;
        self.eq_dragging = false;
        self.eq_open = Some((lane, idx));
        // One card at a time, the rule the other two already follow.
        self.keys_open = false;
        self.export_open = false;
        self.context_menu = None;
        cx.notify();
    }

    /// Writes what the card is showing at its clip: one undo step, one entry in
    /// the append-only equalizer table, so this is called once per *gesture* --
    /// the end of a drag, a keystroke -- and never per pointer sample.
    ///
    /// A curve that moves nothing is stored as *no* equalizer at all, which is
    /// what keeps a clip nobody has touched on the identity path through
    /// playback and export (`engine::eq::EqParams::is_identity`).
    pub(crate) fn commit_eq(&mut self, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.eq_open else {
            return;
        };
        let params = (!self.eq_params.is_identity()).then(|| self.eq_params.clone());
        if let Some(session) = &mut self.session {
            session.set_eq(lane, idx, params);
        }
        // `set_eq` reseeks inside the engine -- that is what makes the change
        // audible at once -- and a reseek is what these flags are about.
        self.reset_after_reseek();
        cx.notify();
    }

    /// Changes the picked band in place and says whether anything moved. Every
    /// edit of a band goes through here -- the drag, each key, each stepper
    /// button -- so the card has exactly one place that clamps a band into what
    /// the graph can draw, and no caller has to remember the limits.
    pub(crate) fn set_band(&mut self, change: impl FnOnce(&mut Band)) -> bool {
        let Some(band) = self.eq_params.bands.get_mut(self.eq_band) else {
            return false;
        };
        let was = *band;
        change(band);
        band.freq_hz = band.freq_hz.clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH);
        band.gain_db = band.gain_db.clamp(-EQ_GAIN_LIMIT, EQ_GAIN_LIMIT);
        band.q = band.q.clamp(EQ_Q_LOW, EQ_Q_HIGH);
        *band != was
    }

    /// The keyboard's and the buttons' version of a drag: one step on the picked
    /// band, committed straight away -- neither has a release to wait for.
    pub(crate) fn nudge_band(&mut self, change: impl FnOnce(&mut Band), cx: &mut Context<Self>) {
        if self.set_band(change) {
            self.commit_eq(cx);
        }
    }

    /// Where the pointer sits in the graph, as the picked band's frequency and
    /// gain: across is the frequency axis and down is the gain one, so the
    /// handle follows the hand both ways rather than sliding up a rail. Called
    /// on every pointer sample, so the curve bends under it; the write is the
    /// release's ([`commit_eq`](Player::commit_eq)).
    pub(crate) fn drag_band(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let bounds = self.eq_graph.get();
        let gain = (0.5 - frac_down(at.y, bounds)) * 2. * EQ_GAIN_LIMIT;
        let freq = eq_freq(frac_along(at.x, bounds));
        if self.set_band(|b| {
            b.gain_db = gain;
            b.freq_hz = freq;
        }) {
            cx.notify();
        }
    }

    /// A band added beside the picked one, at the frequency with the most room
    /// around it ([`inserted_band`]), and picked so the next keystroke moves the
    /// band that was just made. Refused rather than silently ignored at the cap.
    pub(crate) fn add_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() >= EQ_BANDS_MAX {
            self.notify_user(
                format!(
                    "EQUALIZER FULL — {EQ_BANDS_MAX} bands is all this card holds; move one instead"
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let band = inserted_band(&self.eq_params.bands, self.eq_band);
        self.eq_band = (self.eq_band + 1).min(self.eq_params.bands.len());
        self.eq_params.bands.insert(self.eq_band, band);
        self.commit_eq(cx);
    }

    /// Takes the picked band out. The last one stays: an equalizer of no bands
    /// is a card with nothing to edit, and flattening is what "off" means here.
    pub(crate) fn remove_band(&mut self, cx: &mut Context<Self>) {
        if self.eq_params.bands.len() <= 1 {
            self.notify_user("LAST BAND — flatten it instead (r), or close the card".into());
            cx.notify();
            return;
        }
        self.eq_params.bands.remove(self.eq_band);
        self.eq_band = self.eq_band.min(self.eq_params.bands.len() - 1);
        self.commit_eq(cx);
    }

    /// Which band a press on the graph grabs: the nearest one along the
    /// frequency axis, so the whole box is the handle rather than a 10 px dot
    /// -- and a press that misses every dot still moves the band it is under.
    pub(crate) fn nearest_band(&self, x: Pixels) -> usize {
        let at = frac_along(x, self.eq_graph.get());
        self.eq_params
            .bands
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (eq_x(a.freq_hz) - at)
                    .abs()
                    .total_cmp(&(eq_x(b.freq_hz) - at).abs())
            })
            .map_or(0, |(i, _)| i)
    }
}
