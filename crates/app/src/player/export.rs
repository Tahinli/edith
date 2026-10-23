//! What is cached about the media and the machine, and the export the
//! two of them decide.

use super::actions::open_in_default_player;
use crate::*;
use std::time::Instant;

impl Player {
    /// What [`timeline_math::lane_refuses`] asks to decide a video lane's
    /// refusal: the probed answer if `cache_media` has one yet, and the
    /// extension's own guess -- the same one every door used before the
    /// probe existed -- while it is still asking.
    pub(crate) fn has_video(&self, path: &Path) -> bool {
        self.has_video
            .get(path)
            .copied()
            .unwrap_or_else(|| !engine::is_audio(path))
    }

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
        // Whether a file's own header carries a picture at all, for the lane
        // a drag may land on ([`Player::has_video`]). The two kinds whose
        // extension already tells the truth (`engine::is_audio`,
        // `engine::is_image`) are answered inline; the rest is the one case
        // an extension can lie about -- a song muxed into an `.mp4` -- so it
        // costs the same header open [`engine::decode::probe`] pays and goes
        // off the render thread for the same reason.
        for path in unseen_paths(session.sources(), &self.has_video) {
            if engine::is_audio(&path) {
                self.has_video.insert(path, false);
                continue;
            }
            if engine::is_image(&path) {
                self.has_video.insert(path, true);
                continue;
            }
            self.has_video.insert(path.clone(), true);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move {
                    !matches!(engine::demux::Demuxer::open(&path),
                        Err(e) if engine::demux::NoVideoTrack::is_it(&e))
                }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.has_video.insert(path, probed);
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
        // The bench's poster frame per file (`Thumb`, DESIGN §5's "real
        // thumbnails across video clips"): one representative frame, decoded
        // off the render thread like the peaks above through the same
        // single-frame door `engine::decode::DecodeSession::open_range`
        // already opens for a poster frame -- no second decode path invented.
        // Filtered to files with a picture at all: an audio-only source has
        // no frame to ask for, so it never enters this map (mirroring `Wave`'s
        // own `Silent`, but as an absence rather than a state).
        for path in unseen_paths(session.sources(), &self.thumbs)
            .into_iter()
            .filter(|p| !engine::is_audio(p))
        {
            self.thumbs.insert(path.clone(), Thumb::Loading);
            let decoded = cx.background_executor().spawn({
                let path = path.clone();
                async move {
                    let (_, rx, _cancel) =
                        engine::decode::DecodeSession::open_range(&path, 0, 1).ok()?;
                    let frame = rx.recv().ok()?;
                    let buf = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)?;
                    Some(std::sync::Arc::new(RenderImage::new(vec![
                        image::Frame::new(buf),
                    ])))
                }
            });
            cx.spawn(async move |this, cx| {
                let decoded = decoded.await;
                this.update(cx, |this, cx| {
                    this.thumbs.insert(
                        path,
                        match decoded {
                            Some(image) => Thumb::Ready(image),
                            None => Thumb::Failed,
                        },
                    );
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
            self.decoders.insert(path.clone(), Decoder::Pending);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                // A refusal is kept in the decoder's own words, and the row
                // shows them: a file this machine cannot decode is the one
                // whose second line has to say so. A song is a different
                // answer entirely -- it has no coded stream, and the probe
                // says so by naming the still seat.
                async move { Decoder::from(engine::decode::probe(&path)) }
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
        for key in unseen_sources(session.sources(), &self.waves, Instant::now()) {
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
                            // Stamped, and not an answer: a repaint past
                            // [`WAVE_RETRY`] asks again, so a failure whose
                            // cause went away (a file still being copied when
                            // it arrived) draws its envelope without a reopen.
                            Err(_) => Wave::Failed(Instant::now()),
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        // The silence card's own decode, started here instead of at the card
        // -- a whole source, off the render thread, the moment it arrives
        // rather than the moment somebody opens the card on it
        // ([`Player::start_silence_scan`] used to be the only door and cost
        // the card its first fifty seconds). Landed under `full_scan_key` in
        // the very map the card reads, so `scan_silences` finds it warm.
        //
        // A still has no sound to read, and `silence_bg`'s presence is the
        // dedupe: a source already scanning, already scanned, or scanned and
        // then removed mid-read (its entry stays until the scan notices the
        // cancel) is not started twice. Gathered here, off `session`, for
        // `starting`'s own reason: starting one below takes `self` mutably
        // and `session` is a borrow of it.
        let silence_starting: Vec<(PathBuf, usize)> = session
            .sources()
            .iter()
            .map(|s| (s.path.clone(), s.audio_stream))
            .filter(|key| {
                !engine::is_image(&key.0)
                    && !self.silence_bg.contains_key(key)
                    && !self
                        .silence_levels
                        .contains_key(&full_scan_key(&key.0, key.1))
            })
            .collect();
        for path in starting {
            self.start_proxy_for(&path, cx);
        }
        for key in silence_starting {
            let progress = Arc::new(engine::silence::Progress::default());
            self.silence_bg.insert(key.clone(), Arc::clone(&progress));
            let scan = cx.background_executor().spawn({
                let (path, stream, progress) = (key.0.clone(), key.1, Arc::clone(&progress));
                async move {
                    engine::silence::levels_with_progress(
                        &path,
                        stream,
                        (0., f64::INFINITY),
                        &progress,
                    )
                }
            });
            cx.spawn(async move |this, cx| {
                let landed = scan.await;
                this.update(cx, |this, cx| {
                    this.silence_bg.remove(&key);
                    // Cancelled means the source was removed while this read
                    // was running: half a file's levels answer for a source
                    // that is not on the list any more.
                    if progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    if let Ok(Some(levels)) = landed {
                        this.silence_levels
                            .insert(full_scan_key(&key.0, key.1), Arc::new(levels));
                        // A card already open and waiting on this very source
                        // draws its marks the moment they land, same as an
                        // on-demand scan does.
                        this.scan_silences();
                        cx.notify();
                    }
                    // `Ok(None)` (no audio track) and `Err` are left unrecorded:
                    // the card's own on-demand scan is what a person waiting on
                    // this file actually sees, and it gives the same answer
                    // (`unscannable`/`Scan failed`) when it is opened.
                })
                .ok();
            })
            .detach();
        }
        // What each file holds, for the properties plates. Last, because it
        // needs `self` mutably and the session borrow above ends here.
        self.cache_facts(cx);
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
        let settings = probe_settings(self.format, self.audio_kbps, self.encoder_seat());
        if !self.export_open {
            return;
        }
        // A format with no picture has no seat to probe -- and the *last*
        // format's is not its answer: cleared rather than left standing, or
        // picking MP3 after AV1 would read "SW encode (rav1e) · MP3 · SW
        // (ec-mp3)", which names an encoder that will not run.
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
        let probed = cx
            .background_executor()
            .spawn(async move { engine::export::planned_seats(&project, &meta, &settings) });
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

    /// What the export action does now: opens the sheet, which is where the
    /// preset, the range, the destination and the decision to write at all are.
    /// Nothing is encoded until the button in it is pressed. While an export
    /// runs, the same door brings its progress back.
    pub(crate) fn open_export(&mut self, cx: &mut Context<Self>) {
        // Nothing to write out, and a refusal rather than a card about it: the
        // window is empty and the export path is not even chosen yet.
        if self.session.is_none() {
            self.notify_user("Nothing to export: no file open".into());
            cx.notify();
            return;
        }
        // A finished or failed face belongs to the export that made it; a
        // sheet opened afterwards is a fresh one.
        if self.export.is_none() {
            self.export_sheet.done = None;
            self.export_sheet.failed = None;
            self.export_sheet.cancelled = false;
        }
        self.export_sheet.field = None;
        self.export_sheet.note = None;
        self.export_ask = false;
        self.export_open = true;
        self.transcript_open = false;
        // One card at a time, and a waiting row must not outlive the card it
        // was waiting in. Nor may a half-typed number: the card opens on the
        // bitrate it will write, never on digits left behind by a closed one.
        self.keys_open = false;
        self.budget_edit = None;
        cx.notify();
    }

    /// A format row, from Settings. The destination follows it at once -- a
    /// WAV written to a path ending in `.mp4` is a file every player will lie
    /// about -- keeping whatever stem the save dialog last left there.
    /// `false` on a refusal, so a caller with more than the format to write
    /// knows not to write the rest of it either.
    pub(crate) fn set_format(&mut self, format: Format) -> bool {
        // The one door both the row and its initial go through, so a format
        // the room greys out cannot be picked by keyboard either.
        if let Some(why) = self
            .session
            .as_ref()
            .and_then(|session| format_refusal(session, format))
        {
            self.notify_user(format!("Not {} — {why}", format_label(format)).into());
            return false;
        }
        self.format = format;
        // Only an HEVC file carries an HDR tag; the engine refuses it on any other.
        if !format.is_hevc() {
            self.export_sheet.hdr = HdrTransfer::Sdr;
        }
        self.export_path = retarget(&self.export_path, format);
        self.save_export_pick();
        true
    }

    /// Writes the picture and sound this computer exports with, so the next
    /// launch opens on them. A failed write is said once; the pick still holds
    /// for this session.
    pub(crate) fn save_export_pick(&mut self) {
        // Under test the real ~/.config is never written: only a scratch
        // XDG_CONFIG_HOME (the override the config round-trip tests set) is.
        #[cfg(test)]
        if crate::keymap::xdg_config_home().is_none_or(|v| v.is_empty()) {
            return;
        }
        if crate::export_ui::save_export_prefs(&self.export_prefs()).is_err() {
            self.notify_user(
                "Could not save the export pick. Check that ~/.config/edith is writable.".into(),
            );
        }
    }

    /// The choices this computer keeps between launches.
    pub(crate) fn export_prefs(&self) -> crate::export_ui::ExportPrefs {
        crate::export_ui::ExportPrefs {
            format: self.format,
            audio_kbps: self.audio_kbps,
            bps: self.custom_bps,
            size: self.export_sheet.size,
            depth: self.export_sheet.bit_depth,
            prores: self.export_sheet.prores,
            hdr: self.export_sheet.hdr,
            hevc_speed: self.export_sheet.hevc_speed,
            sound_bits: self.export_sheet.sound_bits,
        }
    }

    /// A Quality chip: the budget it names, set as the lever would set it. Any
    /// typed value is dropped with it, so the field cannot reopen on a stale one.
    pub(crate) fn set_budget(&mut self, bps: u64) {
        self.custom_bps = bps.clamp(BPS_MIN, BPS_MAX);
        self.budget_edit = None;
        self.save_export_pick();
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
            let was = session.encoder_seat();
            session.set_encoder_seat(seat);
            // The seat is written by a settings-row click, so nothing in
            // [`Player::act`]'s list arms it -- and the engine's setter takes
            // no answer back, so the arm rides the seat having moved.
            let changed = session.encoder_seat() != was;
            if changed {
                self.mark_dirty();
            }
            self.notify_user(format!("Encoder: {}", encoder_label(seat)).into());
        }
        cx.notify();
    }

    /// Why the sound row is not a choice right now, the engine answering about
    /// the very project it would export. No session is the same answer as no
    /// sound: there is nothing to write either way.
    pub(crate) fn audio_rate_refusal(&self) -> Option<&'static str> {
        match &self.session {
            Some(session) => session.audio_rate_refusal(self.format, self.export_span().is_some()),
            None => Some("no sound to write"),
        }
    }

    /// The budget in force: what was chosen, or -- before anybody has chosen
    /// -- the engine's own automatic figure for the picture that is open
    /// ([`auto_bps`], its `BITS_PER_PIXEL` rule). Never zero, which is the
    /// whole point: the lever it feeds is a promise about the file.
    pub(crate) fn budget_bps(&self) -> u64 {
        if self.custom_bps > 0 {
            return self.custom_bps;
        }
        self.session.as_ref().map_or(6_000_000, |s| {
            let (w, h) = s.resolution();
            auto_bps(w, h, s.meta().frame_rate)
        })
    }

    /// What the sound costs over the same span, in bits per second -- the
    /// other half of "how big is it": zero for a format that writes no sound
    /// and for a timeline that has none.
    pub(crate) fn audio_bps(&self) -> u64 {
        match self.audio_rate_refusal().is_none() {
            true => u64::from(self.audio_kbps) * 1_000,
            false => 0,
        }
    }

    /// How many seconds of film the export writes: the marked span where
    /// there is one, the whole timeline otherwise. The size on the budget row
    /// is this times the rate.
    pub(crate) fn export_seconds(&self) -> f64 {
        self.session.as_ref().map_or(0., |s| match self.export_span() {
            Some((start, end)) => f64::from(end - start) / s.meta().frame_rate,
            None => s.timeline_duration(),
        })
    }

    /// The missing files the export would read: sources that are not on disk and that a clip
    /// inside the chosen range uses. A file only clips outside the range use does not count.
    pub(crate) fn export_missing(&self) -> Vec<PathBuf> {
        let Some(session) = &self.session else { return Vec::new() };
        let offline = session.offline_sources();
        if offline.is_empty() {
            return Vec::new();
        }
        let span = self.export_span();
        let mut out: Vec<PathBuf> = Vec::new();
        for lane in session.lanes() {
            for clip in session.lane_clips(lane) {
                let (start, end) = (clip.start, clip.start.saturating_add(clip.frames()));
                if let Some((from, to)) = span
                    && (end <= from || start >= to)
                {
                    continue;
                }
                if let Some((_, path)) = offline.iter().find(|(i, _)| *i == clip.source)
                    && !out.contains(path)
                {
                    out.push(path.clone());
                }
            }
        }
        out
    }

    /// Why the Export button is off, in a short plain phrase -- `None` when it can start.
    pub(crate) fn export_block_reason(&self) -> Option<String> {
        let Some(session) = &self.session else {
            return Some("Nothing to export yet".to_string());
        };
        if let Some(why) = format_refusal(session, self.format) {
            return Some(why.split(" \u{2014} ").next().unwrap_or("cannot export").to_string());
        }
        if self.export_seconds() <= 0. {
            return Some("Nothing to export: the range is empty".to_string());
        }
        let missing = self.export_missing();
        match missing.len() {
            0 => None,
            1 => Some("1 file is missing: relink it first".to_string()),
            n => Some(format!("{n} files are missing: relink them first")),
        }
    }

    /// The marked range as the export will really use it: cut to the timeline,
    /// and `None` where nothing of it is left (marks left from before an edit
    /// shortened the timeline) -- so a range that would write zero seconds is
    /// never offered, and the export is the whole timeline, said so on the card.
    pub(crate) fn export_range(&self) -> Option<(u32, u32)> {
        let s = self.session.as_ref()?;
        clamp_range(self.range?, crate::subs::frame_at(s.timeline_duration(), self.fps))
    }

    /// The range the export will write: the marks when they are valid and the
    /// sheet's radio is on them, the whole timeline (`None`) otherwise.
    pub(crate) fn export_span(&self) -> Option<(u32, u32)> {
        self.export_range().filter(|_| !self.export_sheet.whole)
    }

    /// The settings a preset can change, as they stand.
    pub(crate) fn picked(&self) -> Picked {
        Picked {
            format: self.format,
            bps: self.custom_bps,
            audio_kbps: self.audio_kbps,
            size: self.export_sheet.size,
            depth: self.export_sheet.bit_depth,
            loudness: self.export_sheet.loudness,
            prores: self.export_sheet.prores,
            hdr: self.export_sheet.hdr,
        }
    }

    /// The preset tile that describes the settings exactly, or `None` -- the
    /// sheet's Custom.
    pub(crate) fn current_preset(&self) -> Option<&'static engine::export_presets::Preset> {
        matching_preset(self.picked(), self.export_sheet.preset).filter(|_| !self.export_sheet.custom)
    }

    /// The preset the box names: an engine preset the settings equal, else
    /// "Match project" while they are exactly that, else `None` (Custom).
    pub(crate) fn preset_now(&self) -> Option<&'static str> {
        if self.export_sheet.custom {
            return None;
        }
        if let Some(p) = self.current_preset() {
            return Some(p.id);
        }
        let auto = self.session.as_ref().map_or(0, |s| {
            let (w, h) = s.resolution();
            auto_bps(w, h, s.meta().frame_rate)
        });
        let s = &self.export_sheet;
        (self.format == Format::Mp4
            && s.size.is_none()
            && s.bit_depth.is_none()
            && s.loudness.is_none()
            && (self.custom_bps == 0 || self.custom_bps == auto)
            && self.audio_kbps == DEFAULT_AUDIO_KBPS)
            .then_some(MATCH_PROJECT)
    }

    /// A preset tile: every setting it names laid over the current ones by the
    /// engine's own rule. A format this timeline cannot be written in is refused
    /// by name and nothing changes.
    pub(crate) fn choose_preset(&mut self, id: &str) {
        if id == MATCH_PROJECT {
            // H.264 MP4 at the project's own size and rate, the bitrate left
            // automatic (the engine's figure for that size).
            if !self.set_format(Format::Mp4) {
                return;
            }
            self.audio_kbps = DEFAULT_AUDIO_KBPS;
            self.custom_bps = 0;
            self.export_sheet.size = None;
            self.export_sheet.bit_depth = None;
            self.export_sheet.loudness = None;
            self.export_sheet.hdr = HdrTransfer::Sdr;
            self.export_sheet.preset = Some(MATCH_PROJECT);
            self.export_sheet.custom = false;
            self.budget_edit = None;
            self.save_export_pick();
            return;
        }
        let Some(preset) = engine::export_presets::by_id(id) else {
            return;
        };
        let next = picked_with(self.picked(), preset);
        if next.format != self.format && !self.set_format(next.format) {
            return;
        }
        self.audio_kbps = next.audio_kbps;
        if next.bps > 0 {
            self.custom_bps = next.bps.clamp(BPS_MIN, BPS_MAX);
        }
        self.export_sheet.size = next.size;
        self.export_sheet.bit_depth = next.depth;
        self.export_sheet.loudness = next.loudness;
        self.export_sheet.prores = next.prores;
        self.export_sheet.hdr = next.hdr;
        self.export_sheet.preset = Some(preset.id);
        self.export_sheet.custom = false;
        self.budget_edit = None;
        self.save_export_pick();
    }

    /// The rows of one Details list ([`Pick::Export`]), the one in force marked.
    pub(crate) fn export_choices(&self, row: ExportRow) -> Vec<ChoiceRow> {
        let pick = |p: ExportPick, label: String, detail: String, on: bool| -> ChoiceRow {
            (Choice::Export(p), label.into(), detail.into(), on)
        };
        match row {
            ExportRow::Preset => {
                let project = self.session.as_ref().map(PlaybackSession::resolution);
                let current = self.preset_now();
                let mut rows: Vec<ChoiceRow> = vec![pick(
                    ExportPick::Preset(MATCH_PROJECT),
                    "Match project".into(),
                    "project size \u{b7} H.264 \u{b7} auto".into(),
                    current == Some(MATCH_PROJECT),
                )];
                rows.extend(engine::export_presets::presets()
                    .iter()
                    .map(|p| pick(ExportPick::Preset(p.id), p.label.into(), preset_spec(p, project), current == Some(p.id))));
                rows.push(pick(ExportPick::Custom, "Custom".into(), "set in Details".into(), current.is_none()));
                rows
            }
            ExportRow::Container => {
                let mut rows: Vec<ChoiceRow> = Vec::new();
                for &(c, _, f) in &COMBOS {
                    if rows.iter().any(|(_, l, ..)| l.as_ref() == c) {
                        continue;
                    }
                    let target = format_in_container(c, self.format).unwrap_or(f);
                    rows.push(pick(ExportPick::Format(target), c.into(), String::new(), container_word(self.format) == c));
                }
                rows
            }
            ExportRow::Codec => COMBOS
                .iter()
                .filter(|(c, ..)| *c == container_word(self.format))
                .map(|&(_, k, f)| pick(ExportPick::Format(f), k.into(), String::new(), self.format == f))
                .collect(),
            ExportRow::Seat => ENCODER_SEATS
                .into_iter()
                .map(|s| (Choice::Encoder(s), encoder_label(s).into(), seat_plain(s).into(), self.encoder_seat() == s))
                .collect(),
            ExportRow::Bitrate => {
                let budget = self.budget_bps();
                let mut rows: Vec<ChoiceRow> = BITRATE_STEPS
                    .iter()
                    .map(|&m| pick(ExportPick::Bitrate(Some(m * 1_000_000)), format!("{m} Mb/s"), String::new(), budget == m * 1_000_000))
                    .collect();
                if !BITRATE_STEPS.iter().any(|&m| budget == m * 1_000_000) {
                    rows.push(pick(ExportPick::Bitrate(Some(budget)), format!("{} Mb/s", rate_label(budget)), "now".into(), true));
                }
                rows.push(pick(ExportPick::Bitrate(None), "Type a value\u{2026}".into(), String::new(), false));
                rows
            }
            ExportRow::Size => {
                let project = self.session.as_ref().map(PlaybackSession::resolution);
                let same = project.map_or("Same as project".to_string(), |(w, h)| format!("Same as project ({w}\u{d7}{h})"));
                std::iter::once(pick(ExportPick::Size(None), same, String::new(), self.export_sheet.size.is_none()))
                    .chain(RESOLUTIONS.iter().map(|&(w, h)| {
                        pick(ExportPick::Size(Some((w, h))), format!("{w}\u{d7}{h}"), String::new(), self.export_sheet.size == Some((w, h)))
                    }))
                    .collect()
            }
            ExportRow::GifWidth => crate::export_ui::GIF_WIDTHS
                .into_iter()
                .map(|w| pick(ExportPick::GifWidth(w), format!("{w} px"), String::new(), self.export_sheet.gif.width == w))
                .collect(),
            ExportRow::GifFps => crate::export_ui::GIF_FPS
                .into_iter()
                .map(|f| pick(ExportPick::GifFps(f), format!("{f} fps"), String::new(), self.export_sheet.gif.fps == f))
                .collect(),
            ExportRow::GifLoop => [(true, "Loop forever"), (false, "Play once")]
                .into_iter()
                .map(|(on, l)| pick(ExportPick::GifLoop(on), l.into(), String::new(), self.export_sheet.gif.loop_forever == on))
                .collect(),
            ExportRow::Depth => [(None, "Same as source"), (Some(8), "8-bit"), (Some(10), "10-bit")]
                .into_iter()
                .map(|(d, l)| pick(ExportPick::Depth(d), l.into(), String::new(), self.export_sheet.bit_depth == d))
                .collect(),
            ExportRow::Profile => ProResProfile::ALL
                .into_iter()
                .map(|q| pick(ExportPick::Profile(q), q.name().into(), profile_plain(q).into(), self.export_sheet.prores == q))
                .collect(),
            ExportRow::Hdr => [
                (HdrTransfer::Sdr, "Off", "standard picture"),
                (HdrTransfer::Pq, "HDR10 (PQ)", "for HDR TVs and YouTube HDR"),
                (HdrTransfer::Hlg, "HLG", "for broadcast HDR"),
            ]
            .into_iter()
            .map(|(h, l, d)| pick(ExportPick::Hdr(h), l.into(), d.into(), self.export_sheet.hdr == h))
            .collect(),
            ExportRow::SoundDepth => [(Some(16), "16-bit", "smaller files"), (None, "24-bit", "studio quality")]
                .into_iter()
                .map(|(b, l, d)| pick(ExportPick::SoundBits(b), l.into(), d.into(), self.export_sheet.sound_bits == b))
                .collect(),
            ExportRow::Kbps => audio_rate_choices(self.audio_kbps),
            ExportRow::Loudness => std::iter::once(pick(ExportPick::Loudness(None), "Off".into(), String::new(), self.export_sheet.loudness.is_none()))
                .chain(LOUDNESS_CHOICES.iter().enumerate().map(|(n, &(label, lufs, peak))| {
                    let on = self.export_sheet.loudness == Some(engine::export::LoudnessTarget { lufs, true_peak_dbtp: peak });
                    pick(ExportPick::Loudness(Some(n)), label.into(), crate::export_ui::loudness_plain(lufs).into(), on)
                }))
                .collect(),
            ExportRow::Captions => [(true, "Keep as a track", "when the file can"), (false, "Leave out", "")]
                .into_iter()
                .map(|(keep, l, d)| pick(ExportPick::Captions(keep), l.into(), d.into(), self.export_sheet.no_captions != keep))
                .collect(),
        }
    }

    /// Why a list row cannot be picked now (shown on hover, the row dimmed).
    pub(crate) fn export_choice_refusal(&self, choice: Choice) -> Option<String> {
        match choice {
            Choice::Export(ExportPick::Format(f)) => self
                .session
                .as_ref()
                .and_then(|s| format_refusal(s, f))
                .map(|why| why.split(" \u{2014} ").next().unwrap_or("not available").to_string()),
            _ => None,
        }
    }

    /// A Details answer, through the same doors the old tiles and chips used.
    /// Any detail flips the preset box to "Custom (from X)".
    pub(crate) fn apply_export_pick(&mut self, pick: ExportPick, cx: &mut Context<Self>) {
        match pick {
            ExportPick::Preset(id) => {
                self.choose_preset(id);
                return cx.notify();
            }
            ExportPick::Custom => {
                if self.folds.insert(EXPORT_MORE_FOLD.to_owned()) {
                    save_folds(&self.folds);
                }
            }
            // Typing a rate in: the field commits it ([`Self::commit_budget`]).
            ExportPick::Bitrate(None) => self.edit_budget(),
            detail => {
                self.export_sheet.custom = true;
                match detail {
                    ExportPick::Format(f) => {
                        self.set_format(f);
                    }
                    ExportPick::Size(size) => self.set_export_size(size),
                    ExportPick::Bitrate(bps) => self.set_budget(bps.unwrap_or_default()),
                    ExportPick::Depth(d) => {
                        self.export_sheet.bit_depth = d;
                        // HDR is 10-bit only: asking for 8 bits turns it off.
                        if d == Some(8) {
                            self.export_sheet.hdr = HdrTransfer::Sdr;
                        }
                    }
                    ExportPick::Profile(q) => self.export_sheet.prores = q,
                    ExportPick::Hdr(h) => {
                        self.export_sheet.hdr = h;
                        if h != HdrTransfer::Sdr {
                            self.export_sheet.bit_depth = Some(10);
                        }
                    }
                    ExportPick::HevcSpeed(s) => self.export_sheet.hevc_speed = s,
                    ExportPick::SoundBits(b) => self.export_sheet.sound_bits = b,
                    ExportPick::Loudness(n) => {
                        self.export_sheet.loudness = n.map(|n| {
                            let (_, lufs, true_peak_dbtp) = LOUDNESS_CHOICES[n];
                            engine::export::LoudnessTarget { lufs, true_peak_dbtp }
                        })
                    }
                    ExportPick::Captions(keep) => self.export_sheet.no_captions = !keep,
                    ExportPick::GifWidth(w) => self.export_sheet.gif.width = w,
                    ExportPick::GifFps(f) => self.export_sheet.gif.fps = f,
                    ExportPick::GifLoop(on) => self.export_sheet.gif.loop_forever = on,
                    ExportPick::Preset(_) | ExportPick::Custom => {}
                }
                self.save_export_pick();
            }
        }
        cx.notify();
    }

    /// The arrows on the tiles: the next preset in table order, wrapping.
    pub(crate) fn step_preset(&mut self, delta: i32, cx: &mut Context<Self>) {
        let table = engine::export_presets::presets();
        let at = self
            .preset_now()
            .or(self.export_sheet.preset)
            .and_then(|id| table.iter().position(|q| q.id == id));
        let next = match at {
            Some(at) => (at as i32 + delta).rem_euclid(table.len() as i32) as usize,
            None => 0,
        };
        self.choose_preset(table[next].id);
        cx.notify();
    }

    /// The canvas list: `None` follows the project.
    pub(crate) fn set_export_size(&mut self, size: Option<(u32, u32)>) {
        self.export_sheet.size = size;
    }

    /// The file's canvas as the export will draw it.
    pub(crate) fn export_canvas(&self) -> (u32, u32) {
        self.export_sheet
            .size
            .or_else(|| self.session.as_ref().map(PlaybackSession::resolution))
            .unwrap_or((0, 0))
    }

    /// Whether the timeline carries any sound under this format and range.
    pub(crate) fn export_has_sound(&self) -> bool {
        self.session.as_ref().is_some_and(|s| {
            let plan = s.planned_audio(self.format, self.export_span().is_some());
            !plan.is_empty() && plan != "no sound to write"
        })
    }

    /// Whether any video lane holds a clip.
    pub(crate) fn export_has_picture(&self) -> bool {
        self.session.as_ref().is_some_and(|s| {
            s.lanes()
                .into_iter()
                .any(|l| l.kind == LaneKind::Video && !s.lane_clips(l).is_empty())
        })
    }

    /// The lever by `steps` notches -- a tenth of a Mbps each, a whole Mbps
    /// with shift. Clamped to the engine's own bounds, so the number on the
    /// row is the number the encoder is given.
    pub(crate) fn nudge_budget(&mut self, steps: i32, coarse: bool) {
        // A video setting, like the field `n` opens: the sound-only moment
        // draws no row for the lever, so nothing moves what it does not
        // show -- the arrows, `+`/`-` and the wheel answer nothing there.
        if !self.format.has_video() {
            return;
        }
        let step = i64::from(steps)
            * match coarse {
                true => BPS_COARSE as i64,
                false => BPS_FINE as i64,
            };
        let at = self.budget_bps() as i64 + step;
        self.custom_bps = (at.max(0) as u64).clamp(BPS_MIN, BPS_MAX);
    }

    /// Opens the budget's field, empty: `n` is a way to *say* a budget
    /// (`850k`, `6.5M`, `6.5`, `1.2G` of file), not a way to edit digits, and
    /// the number in force is still on the row until enter takes the new one.
    pub(crate) fn edit_budget(&mut self) {
        // A video setting: the sound-only moment draws no row to type into,
        // so its door opens nothing either -- the chord stays swallowed (the
        // moment owns the keyboard) but no field opens under it.
        if !self.format.has_video() {
            return;
        }
        self.budget_edit = Some(String::new());
    }

    /// Takes what was typed, or says nothing and keeps the field open: a
    /// number that parses is clamped into the engine's range (and shown
    /// clamped, in the same keystroke), and one that does not is left to be
    /// fixed in place.
    pub(crate) fn commit_budget(&mut self) {
        let Some(text) = self.budget_edit.clone() else {
            return;
        };
        let (seconds, audio) = (self.export_seconds(), self.audio_bps());
        if let Some(bps) = parse_budget(&text, seconds, audio) {
            self.custom_bps = bps;
            self.budget_edit = None;
            self.export_sheet.custom = true;
        }
    }

    /// Every stroke the sheet answers, and the reason it answers *all* of
    /// them: a chord this surface consumes must never also reach the room's
    /// keymap underneath it (the shipped defect where `n` opened the field
    /// *and* toggled the timeline snap). While the sheet is up it owns the
    /// keyboard -- `d`, `n`, `e`/`↵`, `←`/`→` on the tiles, `−`/`+`, escape do
    /// their thing, and everything else is swallowed rather than falling through.
    ///
    /// Escape retreats one step at a time (DESIGN.md:146): the question or the
    /// field first, the sheet second. A running export is not stopped by
    /// closing the sheet.
    pub(crate) fn export_moment_key(
        &mut self,
        key: &str,
        shift: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.export_open {
            return false;
        }
        if self.export_ask {
            match key {
                "enter" => self.answer_overwrite(Overwrite::Replace, cx),
                "k" => self.answer_overwrite(Overwrite::KeepBoth, cx),
                ESCAPE => self.answer_overwrite(Overwrite::Cancel, cx),
                _ => {}
            }
            return true;
        }
        if let Some(text) = &mut self.budget_edit {
            match key {
                ESCAPE => self.budget_edit = None,
                "enter" => self.commit_budget(),
                "backspace" => {
                    text.pop();
                }
                "up" | "right" => self.nudge_budget(1, shift),
                "down" | "left" => self.nudge_budget(-1, shift),
                // One character a stroke, which is all a rate is made of.
                k if k.chars().count() == 1 => text.push_str(k),
                _ => {}
            }
            return true;
        }
        // Running, finished or failed: the sheet shows that face, and its keys
        // are the way out of it.
        if self.export.is_some()
            || self.export_sheet.done.is_some()
            || self.export_sheet.failed.is_some()
            || self.export_sheet.cancelled
        {
            match key {
                // Escape never takes the tab away; it only closes a popup.
                ESCAPE => self.picker = None,
                "enter" if self.export.is_none() => self.leave_sheet_face(),
                _ => {}
            }
            return true;
        }
        match key {
            ESCAPE => self.picker = None,
            "d" => self.pick_destination(cx),
            // The budget lives under More settings: typing one opens it.
            "n" => {
                if self.format.has_video() && self.folds.insert(EXPORT_MORE_FOLD.to_owned()) {
                    save_folds(&self.folds);
                }
                self.edit_budget();
            }
            "e" | "enter" => self.start_export(cx),
            "left" => self.step_preset(-1, cx),
            "right" => self.step_preset(1, cx),
            "up" => self.scroll_export(Some(48.)),
            "down" => self.scroll_export(Some(-48.)),
            "pageup" => self.scroll_export(Some(320.)),
            "pagedown" => self.scroll_export(Some(-320.)),
            "home" => self.scroll_export(None),
            "end" => self.scroll_export(Some(-1e6)),
            // corner-cut: the settings door answers a bare `,` as well as the
            // room's `^,` it wears, because `param_card_key` is handed shift
            // only -- ceiling: no modifier reaches a card's keys. Upgrade:
            // thread `Modifiers` through `ui::stance`'s handler.
            "," => self.open_settings(cx),
            "+" | "=" => self.nudge_budget(1, shift),
            "-" => self.nudge_budget(-1, shift),
            _ => {}
        }
        true
    }

    /// Moves the export panel's body by `by` pixels (positive shows earlier
    /// rows), or back to the top for `None`; clamped to what there is.
    pub(crate) fn scroll_export(&self, by: Option<f32>) {
        let at = match by {
            Some(by) => (f32::from(self.export_scroll.offset().y) + by)
                .clamp(-f32::from(self.export_scroll.max_offset().height), 0.),
            None => 0.,
        };
        self.export_scroll.set_offset(point(px(0.), px(at)));
    }

    /// Starts typing into a destination field, seeded with what it holds.
    pub(crate) fn begin_field(&mut self, field: SheetField) {
        let text = match field {
            SheetField::Name => self
                .export_path
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
            SheetField::Folder => dest_dir_text(&self.export_path),
        };
        self.export_sheet.field = Some((field, text));
        self.export_sheet.field_all = true;
        self.export_sheet.note = None;
    }

    /// The keyboard while a destination field is being typed into (ahead of the
    /// sheet's own chords, as the cue editor is): enter takes it, escape gives
    /// it up, the rest is text. `key_char` is the character the layout made.
    pub(crate) fn export_field_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let all = &mut self.export_sheet.field_all;
        match key {
            ESCAPE => {
                self.export_sheet.field = None;
                self.export_sheet.note = None;
                self.export_sheet.field_all = false;
            }
            "enter" | "tab" => self.commit_field(),
            "a" if ctrl && !alt => *all = true,
            "backspace" => {
                if let Some((_, text)) = self.export_sheet.field.as_mut() {
                    if *all {
                        text.clear();
                    } else {
                        text.pop();
                    }
                }
                *all = false;
            }
            _ => {
                if !ctrl
                    && !alt
                    && let Some((_, text)) = self.export_sheet.field.as_mut()
                    && let Some(chars) = key_char
                {
                    if *all {
                        text.clear();
                    }
                    text.extend(chars.chars().filter(|c| !c.is_control()));
                    *all = false;
                }
            }
        }
        cx.notify();
    }

    /// Takes a destination edit still being typed before an export starts. A
    /// refused one stays open with its note, and the export does not start, so
    /// the file is never written somewhere other than what was typed.
    fn commit_pending_field(&mut self) -> bool {
        if self.export_sheet.field.is_some() {
            self.commit_field();
        }
        self.export_sheet.field.is_none()
    }

    /// Leaves a finished, failed or cancelled face for the settings, with the
    /// sheet reset. The Export tab stays open.
    pub(crate) fn leave_sheet_face(&mut self) {
        self.export_sheet.done = None;
        self.export_sheet.failed = None;
        self.export_sheet.cancelled = false;
        self.export_sheet.field = None;
        self.export_sheet.note = None;
        self.cancel_armed = false;
    }

    /// Whether `path` is one of the project's own source files.
    pub(crate) fn is_source_path(&self, path: &std::path::Path) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let sources: Vec<&std::path::Path> = session.sources().iter().map(|s| s.path.as_path()).collect();
        is_a_source(path, &sources)
    }

    /// Takes the field's text: a refusal is said under the field in a plain
    /// sentence and the field stays open to be fixed.
    pub(crate) fn commit_field(&mut self) {
        let Some((field, text)) = self.export_sheet.field.clone() else {
            return;
        };
        let home = std::env::var("HOME").ok();
        let landed = match field {
            SheetField::Name => parse_stem(&text).map(|stem| {
                self.export_path
                    .with_file_name(crate::export_ui::typed_name(&stem, self.format))
            }),
            SheetField::Folder => parse_folder(&text, home.as_deref()).map(|dir| {
                dir.join(
                    self.export_path
                        .file_name()
                        .map_or_else(|| "export".into(), |n| n.to_owned()),
                )
            }),
        };
        // A name that is a source would write over the footage: refused here as
        // well as where it is typed, so no route can take it.
        let landed = landed.and_then(|path| match self.is_source_path(&path) {
            true => Err(SOURCE_NAME_TAKEN),
            false => Ok(path),
        });
        match landed {
            Ok(path) => {
                self.export_path = path;
                self.export_sheet.field = None;
                self.export_sheet.note = None;
            }
            Err(why) => self.export_sheet.note = Some(why.to_string()),
        }
    }

    /// The answer to "that file already exists", from the sheet's buttons or
    /// its keys. Replace marks the target this window's own and goes back
    /// through the door; Keep both writes beside it under a new name.
    pub(crate) fn answer_overwrite(&mut self, answer: Overwrite, cx: &mut Context<Self>) {
        self.export_ask = false;
        match answer {
            Overwrite::Replace => {
                self.export_known = Some(self.export_path.clone());
                self.start_export(cx);
            }
            Overwrite::KeepBoth => {
                self.export_path = unique_path(&self.export_path, |p| p.exists());
                self.start_export(cx);
            }
            Overwrite::Cancel => {}
        }
        cx.notify();
    }

    /// The sheet's Show in folder button.
    pub(crate) fn show_export_in_folder(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.export_sheet.done.as_ref().map(|d| d.path.clone()) else {
            return;
        };
        cx.background_executor()
            .spawn(async move { show_in_file_manager(&path) })
            .detach();
    }

    /// The sheet's Play button: the desktop's own player, off the UI thread.
    pub(crate) fn play_export(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.export_sheet.done.as_ref().map(|d| d.path.clone()) else {
            return;
        };
        cx.background_executor()
            .spawn(async move { open_in_default_player(&path) })
            .detach();
    }

    /// The measured pace for the format on the sheet: this codec's trial write
    /// if there was one, else what the last real export measured.
    pub(crate) fn sheet_pace(&self) -> Option<f64> {
        match self.export_sheet.paces.get(codec_word(self.format)) {
            Some(Some(pace)) => Some(*pace),
            _ => measured_pace(),
        }
    }

    /// Whether the trial write for this format is still running or has not
    /// been tried: the time then reads "estimating…" and not a number.
    pub(crate) fn pace_pending(&self) -> bool {
        let key = codec_word(self.format);
        !self.export_sheet.paces.contains_key(key)
    }

    /// Writes one second of this project in the sheet's format on the software
    /// seat (never the GPU) to a temporary file and times it, so the estimate is
    /// a measurement of this machine. Once per codec per session, on a worker,
    /// and only while the sheet is up and no real export is running.
    pub(crate) fn cache_export_pace(&mut self) {
        // Land a finished trial first, open sheet or not, so the loop that
        // waits on it stops.
        if let Some(bench) = &self.export_sheet.bench
            && let Some(result) = bench.handle.result()
        {
            let wall = bench.started.elapsed().as_secs_f64();
            let pace = result.ok().filter(|_| wall > 0.).map(|_| bench.media_secs / wall);
            let (landed, out) = (bench.key, bench.out.clone());
            self.export_sheet.paces.insert(landed, pace);
            self.export_sheet.bench = None;
            std::fs::remove_file(out).ok();
        }
        if !self.export_open || self.export.is_some() || self.export_ask {
            return;
        }
        let key = codec_word(self.format);
        if self.export_sheet.bench.is_some() || self.export_sheet.paces.contains_key(key) {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        if session.is_empty() || session.timeline_duration() <= 0. {
            return;
        }
        let fps = session.meta().frame_rate;
        let media_secs = session.timeline_duration().min(1.);
        let frames = ((media_secs * fps).round() as u32).max(1);
        let settings = pace_bench_settings(
            self.budget_bps(),
            self.format,
            self.audio_kbps,
            self.export_sheet.size,
            self.export_sheet.bit_depth,
            frames,
        );
        let out = pace_bench_path(self.format);
        // `export::start` runs the encode on its own worker thread; this only
        // starts it, and the repaint loop lands it above.
        let handle = session.export_to_with(&out, &settings);
        self.export_sheet.bench = Some(PaceBench {
            key,
            handle,
            started: Instant::now(),
            media_secs: f64::from(frames) / fps,
            out,
        });
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
    /// Copies the chapters as YouTube description text ([`engine::playback::chapter_listing`]) and
    /// says so. Nothing happens with no chapters: the button is off then.
    pub(crate) fn copy_chapters(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let (text, _) = session.youtube_chapters();
        if text.is_empty() {
            return;
        }
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.notify_user(format!("Copied {lines} chapter lines").into());
        cx.notify();
    }

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
    #[allow(dead_code)] // shown by Settings now, not by the moment
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

    /// Whether an export has to be answered instead of started: the target
    /// exists on disk and is not a file this window wrote (or had confirmed)
    /// -- the same rule the save door keeps, with `export_known` playing
    /// `autosave_armed`'s part (user 2026-09-11, "not just save but export
    /// too": exporting over an existing file must warn first).
    pub(crate) fn export_needs_answer(&self) -> bool {
        Self::export_asks(
            self.export_path.exists(),
            self.export_known.as_deref() == Some(self.export_path.as_path()),
        )
    }

    /// The export question as a value: a target that is there and not ours.
    /// Split out of [`Player::export_needs_answer`] so the rule can be read
    /// and tested without a window or a session -- the same shape
    /// [`Player::save_asks`] keeps for Save.
    pub(crate) fn export_asks(target_exists: bool, known: bool) -> bool {
        target_exists && !known
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
        if !self.commit_pending_field() {
            return cx.notify();
        }
        // One encode at a time: while the queue runs, a direct Export is one more job at its end.
        if self.queue.is_going() {
            self.enqueue_export(cx);
            return;
        }
        let settings = self.export_settings_now();
        // Asked here, before `session` borrows `self` mutably for the fences
        // below; nothing between here and the ask touches `export_path` or
        // `export_known`, so the answer cannot go stale on the way.
        let overwrite = self.export_needs_answer();
        let missing = self.export_missing();
        let Some(session) = &mut self.session else {
            self.notify_user("Nothing to export: no file open".into());
            cx.notify();
            return;
        };
        // An emptied timeline is a timeline; it is simply not a file. Refused by
        // name here rather than written as a project of no frames -- and the
        // engine refuses it again on the worker (`export::start`), so a caller
        // that is not this button cannot get past it either. Two fences on
        // purpose: this one is the one with a keystroke to blame.
        if session.is_empty() {
            self.notify_user("Nothing to export: the timeline is empty".into());
            cx.notify();
            return;
        }
        // A source the range reads that is not on disk is named before anything starts, not as a
        // bare "No such file" from the worker.
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| file_name(p)).collect();
            self.notify_user(format!("Not exported — missing: {}", names.join(", ")).into());
            cx.notify();
            return;
        }
        // The format row can be refused *after* it was picked -- mp4 is the
        // default and an audio-only timeline (or a second audio lane) is one
        // edit away -- so the button asks again rather than starting a worker
        // that will only settle with the same refusal minutes later.
        if let Some(why) = format_refusal(session, self.format) {
            self.notify_user(format!("Not exported — {why}").into());
            cx.notify();
            return;
        }
        // The last fence, after every refusal so an export that cannot run
        // never asks: writing over a file this window did not write is a
        // question, not a default. The card closes with the asking -- the
        // plate rises where notices rise and the moment is an overlay over
        // exactly that strip, so the two may not be on screen together.
        // `enter` there ([`ui::stance`]'s root handler) marks the target
        // known and comes back through this door, which then passes.
        if overwrite {
            self.export_ask = true;
            cx.notify();
            return;
        }
        session.pause();
        // A trial write still running would fight the real one for the CPU.
        if let Some(bench) = self.export_sheet.bench.take() {
            bench.handle.cancel();
            std::fs::remove_file(&bench.out).ok();
        }
        self.export_sheet.done = None;
        self.export_sheet.failed = None;
        self.export_sheet.cancelled = false;
        self.export = Some(session.export_to_with(&self.export_path, &settings));
        // Ours from the first packet on: a re-export over this window's own
        // output is not a question (the plate's `enter` sets the same mark
        // before coming back through the door).
        self.export_known = Some(self.export_path.clone());
        // The clock starts with the worker, not with the first repaint that
        // happens to notice it.
        self.export_started = Some(Instant::now());
        self.export_marks.clear();
        // A confirm left armed by the *previous* export would open this one on
        // the pair, offering to cancel a job nobody has asked about yet.
        self.cancel_armed = false;
        // The sheet stays up and turns into the progress face; "Keep editing"
        // closes it and the export carries on under the Export chip.
        cx.notify();
    }

    /// The settings the sheet is left at, as the exporter takes them.
    fn export_settings_now(&self) -> ExportSettings {
        let mut settings = export_settings(
            self.budget_bps(),
            self.format,
            self.audio_kbps,
            self.encoder_seat(),
        );
        // Whatever is on the timeline travels -- every track with a cue in the
        // exported range, not the one row the overlay happens to be drawing.
        // Set here rather than inside `export_settings`, which the card also
        // calls for the *estimate* and which nothing else needs a subtitle for.
        settings.subtitles = self.export_subs();
        settings.range = self.export_span();
        settings.size = self.export_sheet.size;
        settings.video_bit_depth = self.export_sheet.bit_depth;
        settings.loudness = self.export_sheet.loudness;
        settings.prores_profile = self.export_sheet.prores;
        settings.hdr = HdrOutput { transfer: self.export_sheet.hdr, ..HdrOutput::default() };
        settings.hevc_speed = (self.format.is_hevc() && self.export_sheet.hevc_speed != HevcSpeed::Default).then_some(self.export_sheet.hevc_speed);
        settings.audio_bit_depth = self.export_sheet.sound_bits;
        settings.no_soft_subtitles = self.export_sheet.no_captions;
        settings.gif = self.export_sheet.gif;
        // A still is the frame under the playhead, whatever the range was.
        if self.format == Format::Still {
            let at = self.session.as_ref().map_or(0, |s| (s.now() * self.fps).floor().max(0.0) as u32);
            settings.range = Some((at, at + 1));
        }
        settings
    }

    /// "Add to queue": the sheet's settings and the timeline as it is now,
    /// kept as one more job. Nothing is encoded until the queue is started
    /// (or is already going). Refused for the same reasons a direct export is,
    /// and never asks about overwriting: a name already taken gets a number.
    pub(crate) fn enqueue_export(&mut self, cx: &mut Context<Self>) {
        if !self.commit_pending_field() {
            return cx.notify();
        }
        let missing = self.export_missing();
        let Some(session) = &self.session else {
            self.notify_user("Nothing to export: no file open".into());
            cx.notify();
            return;
        };
        let refusal = if session.is_empty() {
            Some("Nothing to export: the timeline is empty".to_string())
        } else if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|p| file_name(p)).collect();
            Some(format!("Not exported \u{2014} missing: {}", names.join(", ")))
        } else {
            format_refusal(session, self.format).map(|why| format!("Not exported \u{2014} {why}"))
        };
        if let Some(why) = refusal {
            self.notify_user(why.into());
            cx.notify();
            return;
        }
        let mut settings = self.export_settings_now();
        if settings.chapters.is_empty() {
            settings.chapters = session.chapter_list_ns(settings.range);
        }
        let (mut project, meta) = session.export_snapshot();
        if let Some(range) = settings.range {
            project.drop_offline_outside(range, engine::source_present);
        }
        let snapshot = crate::export_queue::Snapshot { project, meta, settings, sample_rate: session.sample_rate() };
        let out = crate::export_queue::free_name(&self.export_path, |p| p.exists() || self.queue.claimed(p));
        let what = self.current_preset().map_or_else(|| sheet_format_name(self.format).to_string(), |p| p.label.to_string());
        let summary = format!("{what} \u{b7} {}", crate::ui::cards::length_words(self.export_seconds() as f32));
        self.queue.add(out, summary, snapshot);
        // The next one added starts from a clean sheet face, not the last result.
        self.export_sheet.done = None;
        self.export_sheet.failed = None;
        self.export_sheet.cancelled = false;
        let n = self.queue.unfinished();
        let text = match self.queue.is_going() {
            true => format!("Added to the queue \u{2014} {n} to go"),
            false => format!("Added to the queue ({n}). Start it when you are ready"),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// "Start queue": the jobs run one after another from here on.
    pub(crate) fn queue_start(&mut self, cx: &mut Context<Self>) {
        self.queue.start();
        cx.notify();
    }

    /// "Pause after this job": the one writing now finishes, the rest wait.
    pub(crate) fn queue_pause(&mut self, cx: &mut Context<Self>) {
        self.queue.pause();
        cx.notify();
    }

    pub(crate) fn queue_cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        self.queue.cancel(id);
        cx.notify();
    }

    pub(crate) fn queue_remove(&mut self, id: u64, cx: &mut Context<Self>) {
        self.queue.remove(id);
        cx.notify();
    }

    pub(crate) fn queue_clear_finished(&mut self, cx: &mut Context<Self>) {
        self.queue.clear_finished();
        cx.notify();
    }

    pub(crate) fn queue_show_in_folder(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(path) = self.queue.job(id).map(|j| j.out.clone()) else {
            return;
        };
        cx.background_executor()
            .spawn(async move { show_in_file_manager(&path) })
            .detach();
    }

    /// Settles the queue's running job when its worker is done, and starts
    /// the next one when the queue is going and nothing else is encoding.
    pub(crate) fn poll_queue(&mut self) {
        if let Some(id) = self.queue.running_id() {
            let result = self
                .queue
                .job(id)
                .and_then(|j| j.handle.as_ref())
                .and_then(ExportHandle::result);
            if let Some(result) = result {
                let out = self.queue.job(id).map(|j| j.out.clone()).unwrap_or_default();
                let bytes = std::fs::metadata(&out).map_or(0, |m| m.len());
                let state = self.queue.settle(id, result.map_err(|e| e.to_string()), bytes);
                let name = file_name(&out);
                match state {
                    Some(crate::export_queue::JobState::Done { .. }) => {
                        eprintln!("{EXPORT_DONE}{name}");
                        self.notify_user(format!("{EXPORT_DONE}{name}").into());
                    }
                    Some(crate::export_queue::JobState::Failed(why)) => {
                        eprintln!("Export failed: {name}: {why}");
                        self.notify_user(format!("Export failed: {name} \u{2014} {why}").into());
                    }
                    _ => {}
                }
            }
        }
        if let Some(id) = self.queue.next_ready(self.export.is_some())
            && let Some((snap, out)) = self.queue.begin(id)
        {
            if let Some(session) = &mut self.session {
                session.pause();
            }
            let handle = engine::export::start(snap.project, snap.meta, &out, &snap.settings, snap.sample_rate);
            self.queue.attach(id, handle);
        }
    }

    /// The quit question's words about the queue, if it has work left.
    pub(crate) fn queue_quit_note(&self) -> Option<String> {
        let n = self.queue.unfinished();
        let running = self.queue.running_id().is_some();
        match (n, running) {
            (0, _) => None,
            (1, true) => Some("An export is still being written; closing stops it.".to_string()),
            (n, true) => Some(format!("{n} exports are in the queue, one is being written; closing stops them.")),
            (1, false) => Some("An export is waiting in the queue; closing drops it.".to_string()),
            (n, false) => Some(format!("{n} exports are waiting in the queue; closing drops them.")),
        }
    }

    /// Gives the editor back at once and leaves the worker to stop at its next
    /// frame and delete what it has written.
    pub(crate) fn cancel_export(&mut self) {
        if let Some(export) = &self.export {
            export.cancel();
            self.cancelling = true;
            self.cancel_armed = false;
            self.notify_user("Export cancelled".into());
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
        let wall = self
            .export_started
            .take()
            .map(|t| t.elapsed().as_secs_f64());
        // A cancel that landed is reported as an error: the sheet says it was
        // cancelled. A job that finished before the cancel took hold is a
        // finished export, and its done face is the one shown.
        let cancelled = std::mem::take(&mut self.cancelling);
        self.cancel_armed = false;
        if cancelled && result.is_err() {
            self.export_sheet.cancelled = true;
            return;
        }
        let text = match result {
            Ok(()) => {
                // The next time estimate is priced from this machine's own pace.
                if let Some(wall) = wall {
                    crate::export_ui::note_pace(self.export_seconds(), wall);
                }
                // The sheet's done face: what landed, how big, how long.
                self.export_sheet.done = Some(ExportDone {
                    path: self.export_path.clone(),
                    bytes: std::fs::metadata(&self.export_path).map_or(0, |m| m.len()),
                    wall_secs: wall.unwrap_or(0.),
                });
                // Written and still where it was written: the bar carries it
                // until some other notice takes the bar.
                self.exported = Some(self.export_path.clone());
                format!("{EXPORT_DONE}{}", file_name(&self.export_path))
            }
            Err(e) => {
                let mut why = e.to_string();
                // A bare "No such file or directory" says nothing: name the files that are gone.
                if why.contains("No such file") || why.contains("os error 2") {
                    let gone: Vec<String> = self
                        .session
                        .as_ref()
                        .map(|s| s.offline_sources().iter().map(|(_, p)| file_name(p)).collect())
                        .unwrap_or_default();
                    if !gone.is_empty() {
                        why = format!("{why} \u{2014} missing: {}", gone.join(", "));
                    }
                }
                self.export_sheet.failed = Some(why.clone());
                eprintln!("Export failed: {why}");
                format!(
                    "Export failed: {}",
                    crate::export_ui::failure_words(&why, &dest_dir_text(&self.export_path))
                )
            }
        };
        eprintln!("{text}");
        // The Export tab's own done/failed face says it; a toast on top of it
        // would be the same news twice.
        if !self.export_tab_visible() {
            self.notify_user(text.into());
        }
    }

    /// Whether the dock is showing the Export tab right now.
    pub(crate) fn export_tab_visible(&self) -> bool {
        self.export_open && !self.keys_open
    }
}

/// `range` cut to a timeline `len` frames long; `None` where nothing is left.
pub(crate) fn clamp_range((start, end): (u32, u32), len: u32) -> Option<(u32, u32)> {
    let end = end.min(len);
    (start < end).then_some((start, end))
}

#[cfg(test)]
mod range_tests {
    use super::clamp_range;

    #[test]
    fn stale_marks_never_make_a_zero_second_export() {
        assert_eq!(clamp_range((0, 1), 3120), Some((0, 1)));
        assert_eq!(clamp_range((100, 5000), 3120), Some((100, 3120)));
        // Marks left past the end of a since-shortened timeline: whole film.
        assert_eq!(clamp_range((3200, 3300), 3120), None);
        assert_eq!(clamp_range((3120, 3121), 3120), None);
    }
}
