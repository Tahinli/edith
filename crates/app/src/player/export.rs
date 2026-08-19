//! What is cached about the media and the machine, and the export the
//! two of them decide.

use crate::*;

impl Player {
    /// Starts a peak decode -- and a stream probe -- for every source that has
    /// arrived since the last
    /// repaint. One call from the render rather than three at the doors,
    /// because argv, an import and a project load are all doors and only this
    /// one is guaranteed to run after each of them.
    ///
    /// The decode itself runs on a background thread, like the file chooser:
    /// whole-file audio decode is ~1 s for a half-hour source, and on the render
    /// path that is the window not painting for a second. The lane draws a bed
    /// meanwhile and the repaint comes with the peaks. The entry is written
    /// *before* the spawn, so the sixty repaints that happen while a decode runs
    /// start no further ones.
    pub(crate) fn cache_media(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // How big each still is, for the row that has to say so. Inline, unlike
        // the two below: an image header is a few bytes off the front of the
        // file, where a sample table is a parse and a decode is a second.
        for path in unseen_paths(session.sources(), &self.sizes) {
            let size = engine::is_image(&path)
                .then(|| engine::image_size(&path).ok())
                .flatten();
            self.sizes.insert(path, size);
        }
        // Which audio streams each file has, for the library's rows. Header
        // only, but a big file's sample tables are not free to parse, so it
        // goes off the render thread like the peaks do.
        for path in unseen_paths(session.sources(), &self.streams) {
            self.streams.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::AudioSession::probe_streams(&path).unwrap_or_default() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.streams.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // What each file is coded at, for the card that says so. Header and
        // sample table only, but a Matroska indexes no samples and its open
        // walks every cluster header -- 6.7 s on a 12.9 GB film -- so this of
        // all of them cannot be on the render thread.
        for path in unseen_paths(session.sources(), &self.bitrates) {
            self.bitrates.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::probe_bitrate(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.bitrates.insert(path, Some(probed));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Where this file's own groups of pictures begin, for the cut that
        // wants to land on one ([`Player::sync_frames`]). The heaviest probe
        // here -- a Matroska's whole cluster walk, seconds on a film -- and the
        // one nothing waits for: until it answers, the snap is the clip-edge
        // snap it always was.
        for path in unseen_paths(session.sources(), &self.syncs) {
            self.syncs.insert(path.clone(), Vec::new());
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::demux::sync_points(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.syncs.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // Which decoder each file will run on, for the row that says so before
        // a frame of it plays. Off the render thread like the streams above: a
        // stream the plugin takes costs one VA-API init (~90 ms) to answer.
        for path in unseen_paths(session.sources(), &self.decoders) {
            self.decoders.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                // A song and a source no decoder here takes are both `None`:
                // the row says nothing about them rather than guessing, and
                // import refused the second at the door anyway.
                async move { engine::decode::probe(&path).ok() }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.decoders.insert(path, probed);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // A stand-in for every film this machine cannot cut at speed, started
        // the moment the file arrives and made while the editor keeps running
        // ([`engine::proxy`]). Off the render thread for the reason the sync
        // walk above is -- the decision reads the same header -- and the encode
        // itself is the engine's own worker, so what comes back here is a
        // handle to poll and not a wait.
        //
        // ...unless this project makes none by itself
        // ([`engine::PlaybackSession::auto_proxies`]), and then the Proxies
        // switch is the ask: turning it on brings the films that want a
        // stand-in through here at the next repaint, which is the only door
        // there is. Left unseen meanwhile, so nothing is missed by having been
        // imported while the switch was off.
        // Gathered here and started at the bottom of this function: the list is
        // read off the session, and starting one takes the whole player
        // ([`Player::start_proxy_for`], which the row's switch calls too), so
        // the two cannot be the same statement.
        let mut starting: Vec<PathBuf> = Vec::new();
        for path in proxies_to_start(
            session.auto_proxies(),
            session.proxies(),
            session.sources(),
            &self.proxies,
        ) {
            // Two at a time, and the rest at the next repaint: a stand-in is a
            // whole film re-encoded, and a library of ten dropped at once would
            // otherwise start ten encodes fighting over the one hardware seat.
            // The unstarted ones simply stay out of the map, which is what
            // brings them back here ([`unseen_paths`]).
            if self.in_flight_proxies() + starting.len() >= PROXIES_AT_ONCE {
                break;
            }
            starting.push(path);
        }
        for key in unseen_sources(session.sources(), &self.waves) {
            self.waves.insert(key.clone(), Wave::Loading);
            let decoded = cx.background_executor().spawn({
                let (path, stream) = key.clone();
                async move {
                    engine::waveform::peaks(&path, stream, WAVE_BPS)
                        .map(|peaks| peaks.map(|peaks| Arc::new(normalise(peaks))))
                        .inspect_err(|e| eprintln!("waveform: {}: {e}", path.display()))
                }
            });
            cx.spawn(async move |this, cx| {
                let decoded = decoded.await;
                this.update(cx, |this, cx| {
                    this.waves.insert(
                        key,
                        match decoded {
                            Ok(Some(peaks)) => Wave::Peaks(peaks),
                            // No audio track: an answer, and not worth asking
                            // about again.
                            Ok(None) => Wave::Silent,
                            // A file whose sound we could not read is not a
                            // silent one, and a lane that drew it as silent is
                            // how a broken decode passes for a design choice.
                            Err(_) => Wave::Failed,
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        for path in starting {
            self.start_proxy_for(&path, cx);
        }
    }

    /// Probes what an export would open, once per (settings, resolution, cuts)
    /// and only while the export card is up -- it opens the very VA-API encoder
    /// the export would and asks [`engine::export::planned_seats`] the very
    /// question the export asks itself, which is what makes the card's line a
    /// measurement instead of a promise, and also what makes it too slow for
    /// the render thread. Written before the spawn, like the probes above, so
    /// the repaints during it start no second one.
    ///
    /// The cuts are in the key because they are in the answer: moving one onto
    /// a sync point is exactly what turns "SW encode" into "copy", and a card
    /// that kept the old line would be lying about the file it is about to
    /// write.
    pub(crate) fn cache_export_seat(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let settings =
            export_settings(
            self.quality,
            self.custom_mbps,
            self.format,
            self.audio_kbps,
            self.encoder_seat(),
        );
        if !self.export_open {
            return;
        }
        // A format with no picture has no seat to probe -- and the *last*
        // format's is not its answer: cleared rather than left standing, or
        // picking MP3 after AV1 would read "SW encode (rav1e) · MP3 · SW
        // (rusty_mp3)", which names an encoder that will not run.
        if !settings.format.has_video() {
            self.export_seat = None;
            return;
        }
        // The timeline an export would be started with, owned so the probe can
        // run on a worker -- and the clips beside it, which are what tells this
        // that the question has changed.
        let (project, meta) = session.export_snapshot();
        let clips: Vec<Clip> = session
            .lanes()
            .into_iter()
            .flat_map(|lane| session.lane_clips(lane).to_vec())
            .collect();
        // Cloned rather than copied: the settings carry the picked subtitle
        // rows, which is a `Vec` ([`engine::export::ExportSettings`]).
        let key = (settings.clone(), (meta.width, meta.height), clips);
        if self
            .export_seat
            .as_ref()
            .is_some_and(|(asked, size, cuts, _)| (asked, size, cuts) == (&key.0, &key.1, &key.2))
        {
            return;
        }
        self.export_seat = Some((key.0.clone(), key.1, key.2.clone(), None));
        let probed = cx.background_executor().spawn(async move {
            engine::export::planned_seats(&project, &meta, &settings)
        });
        cx.spawn(async move |this, cx| {
            let probed = probed.await;
            this.update(cx, |this, cx| {
                // Only if the card is still asking the same question: a format
                // changed while the plugin opened has a probe of its own.
                if let Some(seat) = this.export_seat.as_mut().filter(|(asked, size, cuts, _)| {
                    (asked, size, cuts) == (&key.0, &key.1, &key.2)
                }) {
                    seat.3 = Some(probed);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks the plugin what this machine's GPU can do, once, the first time the
    /// export card is up to show it. Off the render thread for `cache_export_seat`'s
    /// reason: the plugin initialises VA-API to answer, and a driver that is
    /// slow to load must not be a frame the user waits for.
    pub(crate) fn cache_hw_caps(&mut self, cx: &mut Context<Self>) {
        if !self.export_open || self.hw_caps.is_some() {
            return;
        }
        // Written before the spawn, exactly as the probes above are, so the
        // repaints during it start no second one.
        self.hw_caps = Some("asking the driver…".into());
        let asked = cx
            .background_executor()
            .spawn(async move { engine::caps::hardware() });
        cx.spawn(async move |this, cx| {
            let line = asked.await;
            this.update(cx, |this, cx| {
                this.hw_caps = Some(line.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The export that owns the UI, if any. A cancelled one does not: it has
    /// its own copy of the edit list and owes only its own cleanup.
    pub(crate) fn exporting(&self) -> Option<&ExportHandle> {
        self.export.as_ref().filter(|_| !self.cancelling)
    }

    /// What the export action does now: opens the card, which is where the
    /// quality, the destination and the decision to write at all are. Nothing
    /// is encoded until the button in it is pressed.
    pub(crate) fn open_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        // Nothing to write out, and a refusal rather than a card about it: the
        // window is empty and the export path is not even chosen yet.
        if self.session.is_none() {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        }
        self.export_open = true;
        // One card at a time, and a waiting row must not outlive the card it
        // was waiting in. Nor may a half-typed number: the card opens on the
        // bitrate it will write, never on digits left behind by a closed one.
        self.keys_open = false;
        self.rebinding = None;
        self.mbps_edit = None;
        cx.notify();
    }

    /// A format row was clicked. The destination follows it at once -- a WAV
    /// written to a path ending in `.mp4` is a file every player will lie
    /// about -- keeping whatever stem the save dialog last left there. `false`
    /// on a refusal, so a caller that has more than the format to write (a
    /// preset's own quality) knows not to write the rest of it either.
    pub(crate) fn set_format(&mut self, format: Format) -> bool {
        // The one door both the row and its initial go through, so a format the
        // card greys out cannot be picked by keyboard either.
        if let Some(why) = self
            .session
            .as_ref()
            .and_then(|session| format_refusal(session, format))
        {
            self.notify_user(format!("NOT {} — {why}", format_label(format)).into());
            return false;
        }
        self.format = format;
        self.export_path = retarget(&self.export_path, format);
        true
    }

    /// A preset row, by click or by its own key. `Custom` opens the pane where
    /// the codec and the quality are picked apart and changes nothing itself;
    /// the rest are exactly the bundle they name -- and nothing at all on a
    /// refusal, because [`set_format`](Self::set_format) already fired the
    /// banner and a quality written after it declined would be a row that
    /// looks picked over a format that was not.
    pub(crate) fn pick_preset(&mut self, preset: ExportPreset) {
        match preset.bundle() {
            Some((format, quality)) => {
                if self.set_format(format) {
                    self.quality = quality;
                }
            }
            None => self.export_advanced_open = true,
        }
    }

    /// The container row: the same codec in the other box, which retargets the
    /// destination exactly as picking a codec does -- and does nothing at all
    /// for a codec with only one box, so the stroke cannot invent a choice the
    /// card is not offering.
    pub(crate) fn cycle_container(&mut self) {
        self.set_format(next_container(self.format));
    }

    /// The quality rows by keyboard, wrapping. Refused by name where the format
    /// has no bitrate to pick: a key that silently does nothing is the card
    /// looking broken.
    pub(crate) fn cycle_quality(&mut self) {
        if let Some(why) = bitrate_refusal(self.format) {
            self.notify_user(why.into());
            return;
        }
        let at = Quality::ALL
            .iter()
            .position(|&q| q == self.quality)
            .unwrap_or(0);
        self.quality = Quality::ALL[(at + 1) % Quality::ALL.len()];
    }

    /// The sound's rate by keyboard, wrapping through the offered ones -- the
    /// picture's quality row for the other half of the file. Refused by name
    /// where this timeline in this format has no rate to pick, exactly as
    /// [`Player::cycle_quality`] is: a key that silently does nothing is the
    /// card looking broken.
    pub(crate) fn cycle_audio_kbps(&mut self) {
        if let Some(why) = self.audio_rate_refusal() {
            self.notify_user(why.into());
            return;
        }
        self.audio_kbps = next_audio_kbps(self.audio_kbps);
    }

    /// Which encoder an export of this project would write the picture with
    /// ([`engine::export::EncoderSeat`]). The session's, because it is saved
    /// with the project -- there is no card-local copy to drift from it -- and
    /// the default with no timeline open, where the card shows nothing anyway.
    pub(crate) fn encoder_seat(&self) -> EncoderSeat {
        self.session
            .as_ref()
            .map_or_else(EncoderSeat::default, PlaybackSession::encoder_seat)
    }

    /// Pick the seat. Said out loud like every other pick that changes what a
    /// file will be written by -- and the card's planned line re-probes by
    /// itself, since the settings it is keyed on have changed
    /// ([`Self::cache_export_seat`]).
    pub(crate) fn apply_encoder(&mut self, seat: EncoderSeat, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session {
            session.set_encoder_seat(seat);
            self.notify_user(format!("Encoder: {}", encoder_label(seat)).into());
        }
        cx.notify();
    }

    /// The seat by keyboard, wrapping through the three -- the Sound row's
    /// rule, for the row beside it. Refused by name with no timeline: a key
    /// that silently does nothing is the card looking broken.
    pub(crate) fn cycle_encoder(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.notify_user("no timeline to export — open a file first".into());
            cx.notify();
            return;
        };
        let at = EncoderSeat::ALL
            .iter()
            .position(|&s| s == session.encoder_seat())
            .unwrap_or(0);
        self.apply_encoder(EncoderSeat::ALL[(at + 1) % EncoderSeat::ALL.len()], cx);
    }

    /// Why the sound row is not a choice right now, the engine answering about
    /// the very project it would export. No session is the same answer as no
    /// sound: there is nothing to write either way.
    pub(crate) fn audio_rate_refusal(&self) -> Option<&'static str> {
        match &self.session {
            Some(session) => session.audio_rate_refusal(self.format),
            None => Some("no sound to write"),
        }
    }

    /// The custom bitrate by pointer: the typed digits were the only control in
    /// this card a mouse could not reach. Clamped to the range the row states
    /// (the engine's own 1..50 Mbps), and picking the row is part of the step --
    /// a stepper that moves a number nobody is using would move nothing.
    pub(crate) fn nudge_mbps(&mut self, step: i32) {
        self.custom_mbps =
            (self.custom_mbps as i32 + step).clamp(MBPS_MIN as i32, MBPS_MAX as i32) as u32;
        self.quality = Quality::Custom;
    }

    /// The same number under the wheel, one step a notch, up for more: fifty
    /// presses of a stepper is not a way to reach the top of this range, and the
    /// wheel is what this editor already moves a value with (the timeline's
    /// zoom and scroll are the same gesture). Hold-to-run stays the keyboard's,
    /// as it is on every other card here -- a button that repeats while held is
    /// not a thing this program has.
    ///
    /// It moves the *field* while one is open, exactly as ↑↓ do, so the two
    /// ways in never disagree about which number is being changed.
    pub(crate) fn wheel_mbps(&mut self, event: &ScrollWheelEvent) {
        let by = wheel_delta(event);
        if by == 0. {
            return;
        }
        let by = by.signum() as i32;
        match &mut self.mbps_edit {
            Some(edit) => edit.step(by),
            None => self.nudge_mbps(by),
        }
    }

    /// Opens the custom bitrate's field on the number the row is carrying, and
    /// picks the row while it is at it: a field typed into is the row being
    /// chosen, and a number nobody is using would be a number typed at nothing.
    /// Nothing is committed here -- until enter, the card still exports at the
    /// bitrate it had.
    pub(crate) fn edit_mbps(&mut self) {
        self.quality = Quality::Custom;
        self.mbps_edit = Some(NumberEdit::new(self.custom_mbps));
    }

    /// The card's Destination row: the desktop's save dialog, on a background
    /// thread like the import chooser -- the user may sit in it and the window
    /// behind must not freeze. No chooser at all leaves the default path, which
    /// is what the refusal says.
    pub(crate) fn pick_destination(&mut self, cx: &mut Context<Self>) {
        let default = self.export_path.clone();
        let picked = cx
            .background_executor()
            .spawn(async move { pick_save(&default) });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| {
                // The dialog outlives the card: an export started meanwhile
                // took the old path and its notice must name what it wrote.
                if this.export.is_some() {
                    return;
                }
                match picked {
                    // The stem is the user's, the extension is the format's: a
                    // FLAC named `.mp4` is a file every player lies about.
                    Ok(Some(path)) => this.export_path = retarget(&path, this.format),
                    // Cancelled: the default stands, as it did before.
                    Ok(None) => {}
                    Err(text) => {
                        eprintln!("{text}");
                        this.notify_user(text.into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The subtitle tracks an export of this timeline carries: every one a
    /// subtitle *lane* places, with a cue left in the exported range
    /// ([`PlaybackSession::timeline_cues`], the very map the file is written
    /// from), in the library's own order.
    ///
    /// On the timeline or not at all: a track sitting in the palette is a track
    /// nobody put anywhere, and one dropped off its lane is one somebody took
    /// away -- neither belongs in the file. Without the placement test they came
    /// back through the engine's palette door (`export::planned_subtitles` falls
    /// back to these picks when no lane holds anything), so emptying every
    /// subtitle lane and exporting wrote all three of them into the output.
    ///
    /// Worked out from the timeline each time rather than kept as a pick, which
    /// is what makes it impossible to desync: a row added or taken off shifts
    /// every index after it, and a stored list would then name tracks nobody
    /// chose. `Player::sub_track` stays what it always was -- which palette row
    /// the list *marks* -- and has no say here, and neither has the lane that is
    /// shown over the picture ([`Player::active_sub_lane`]): every lane travels.
    ///
    /// The honest input and not the final answer: the engine filters it again
    /// per track (a track that could not be read, a picture one) and says so in
    /// the card's own words ([`engine::export::planned_subtitles`]).
    pub(crate) fn export_subs(&self) -> Vec<usize> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let placed: Vec<usize> = session
            .subtitle_lanes()
            .into_iter()
            .flat_map(|lane| session.sub_lane(lane).iter().map(|sub| sub.track))
            .collect();
        (0..session.subtitles().len())
            .filter(|i| placed.contains(i))
            .filter(|&i| !session.timeline_cues(i).is_empty())
            .collect()
    }

    /// That list in the card's words ([`subtitle_plan`]): what travels, and the
    /// reason beside every track that does not -- including the ones
    /// [`Self::export_subs`] filtered out before the engine ever saw them.
    pub(crate) fn subtitle_line(&self) -> String {
        let Some(session) = self.session.as_ref() else {
            return "none".to_string();
        };
        let picks = self.export_subs();
        let plan = session.planned_subtitles(self.format, picks.iter().copied());
        match self.format.has_video() {
            true => subtitle_plan(plan, session.subtitles(), &picks),
            // A format that is the sound alone has nowhere to put any of them
            // and the engine says that once, about the file. Naming the cues of
            // each track under it answers a question the format already closed.
            false => plan,
        }
    }

    /// Writes the edit list out, at the settings the card was left at. Playback
    /// stops first: the exporter opens its own decoder -- and, on the hardware
    /// path, an encoder -- so a running player would only compete with it for
    /// the GPU. A cancelled export still winding down holds this off for the
    /// frame it takes to notice, which is what keeps its `remove_file` off the
    /// new output.
    pub(crate) fn start_export(&mut self, cx: &mut Context<Self>) {
        if self.export.is_some() {
            return;
        }
        let mut settings =
            export_settings(
            self.quality,
            self.custom_mbps,
            self.format,
            self.audio_kbps,
            self.encoder_seat(),
        );
        // Whatever is on the timeline travels -- every track with a cue in the
        // exported range, not the one row the overlay happens to be drawing.
        // Set here rather than inside `export_settings`, which the card also
        // calls for the *estimate* and which nothing else needs a subtitle for.
        settings.subtitles = self.export_subs();
        let Some(session) = &mut self.session else {
            self.notify_user("NOTHING TO EXPORT — open a file first".into());
            cx.notify();
            return;
        };
        // An emptied timeline is a timeline; it is simply not a file. Refused by
        // name here rather than written as a project of no frames -- and the
        // engine refuses it again on the worker (`export::start`), so a caller
        // that is not this button cannot get past it either. Two fences on
        // purpose: this one is the one with a keystroke to blame.
        if session.is_empty() {
            self.notify_user("NOTHING TO EXPORT — the timeline is empty".into());
            cx.notify();
            return;
        }
        // The format row can be refused *after* it was picked -- mp4 is the
        // default and an audio-only timeline (or a second audio lane) is one
        // edit away -- so the button asks again rather than starting a worker
        // that will only settle with the same refusal minutes later.
        if let Some(why) = format_refusal(session, self.format) {
            self.notify_user(format!("NOT EXPORTED — {why}").into());
            cx.notify();
            return;
        }
        session.pause();
        self.export = Some(session.export_to_with(&self.export_path, &settings));
        // The clock starts with the worker, not with the first repaint that
        // happens to notice it.
        self.export_started = Some(Instant::now());
        self.export_marks.clear();
        // A confirm left armed by the *previous* export would open this one on
        // the pair, offering to cancel a job nobody has asked about yet.
        self.cancel_armed = false;
        // The card has been answered; the progress card takes the window from
        // here ([`Player::export_progress_card`]), and it is the running
        // export's chord that matters now.
        self.export_open = false;
        cx.notify();
    }

    /// Gives the editor back at once and leaves the worker to stop at its next
    /// frame and delete what it has written.
    pub(crate) fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
            self.cancelling = true;
            self.cancel_armed = false;
        }
    }

    /// Takes the export's verdict once it has one. The only place the app
    /// touches the handle's completion side.
    pub(crate) fn poll_export(&mut self) {
        // Sampled here rather than while drawing: a repaint stays a repaint,
        // and this runs once per repaint either way.
        if let (Some(progress), Some(started)) = (
            self.exporting().map(ExportHandle::progress),
            self.export_started,
        ) {
            note_progress(
                &mut self.export_marks,
                started.elapsed().as_secs_f32(),
                progress,
            );
        }
        let Some(result) = self.export.as_ref().and_then(ExportHandle::result) else {
            return;
        };
        self.export = None;
        self.export_started = None;
        // A cancellation is reported as an error, and the one who asked for it
        // has had the editor back since the keystroke. Nothing to say.
        if std::mem::take(&mut self.cancelling) {
            return;
        }
        let text = match result {
            Ok(()) => {
                // Written and still where it was written: the bar carries it
                // until some other notice takes the bar.
                self.exported = Some(self.export_path.clone());
                format!("{EXPORT_DONE}{}", file_name(&self.export_path))
            }
            Err(e) => format!("EXPORT FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
    }
}
