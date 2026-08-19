//! What comes in and what goes out: the sources, the imports, the
//! subtitles, the project file and the lanes.

use crate::*;

impl Player {
    /// The one way a library row reaches the timeline: the Add button and a row
    /// dragged onto a lane both come here, so there is a single answer to what
    /// "add this source" does. The whole source goes in as one grouped take at
    /// `at` -- the frame the pointer let it go on, or the playhead for the
    /// button, which names no place. It is the same insert a paste makes, so
    /// everything after it moves along rather than being painted over. Reseeks
    /// like every other edit, and drops the timeline's selection with it: the
    /// insert has just moved the indices it pointed at.
    pub(crate) fn insert_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The lane the pointer named cannot hold this kind of file: refused by
        // name, in the same words the ghost was tinted by on the way down
        // ([`lane_refuses`]). The Add button names no lane and so is never
        // refused here -- where a file goes when nobody says is the engine's
        // choice, in `place_stream_at`, not one made twice here.
        if let Some(why) = onto.and_then(|lane| lane_refuses(path, lane)) {
            self.notify_user(why.into());
            cx.notify();
            return;
        }
        // The engine's own length for the file, noted when the import took it
        // in: a row that has never been on a lane is placeable at its full
        // length, which is the whole point of an import that only fills the
        // library.
        let fps = self.fps;
        let placed = match &mut self.session {
            // Seconds, because that is what the engine's own door takes: the
            // frame the pointer named goes back through the same rate every box
            // on the bed is drawn at, so it lands on the frame it was let go on
            // rather than a neighbouring one.
            Some(session) => {
                let at = at.map_or_else(|| session.now(), |frame| f64::from(frame) / fps);
                session.place_stream_at(at, path, stream, onto)
            }
            None => Ok(false),
        };
        match placed {
            Ok(true) => {
                self.selected.clear();
                self.reset_after_reseek();
            }
            // The engine's own words: a stream that cannot join this timeline
            // says which property disagrees, exactly as a refused import does.
            Err(e) => self.notify_user(format!("NOTHING ADDED — {e}").into()),
            Ok(false) => {
                self.notify_user("NOTHING ADDED — that file could not be placed here".into())
            }
        }
        cx.notify();
    }

    /// Takes a library row's file out of the list, which is the one thing a row
    /// can lose. Refused in the engine's own words while clips still play from
    /// it -- and those words name the lanes holding them, so the refusal says
    /// what to delete first. The list itself is the report that it worked: the
    /// row is gone from it.
    pub(crate) fn remove_source(&mut self, path: &Path, stream: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_source(path, stream));
        let text = match removed {
            Some(Ok(idx)) => {
                // Tells a running background scan of this source nobody wants
                // its levels any more, and drops the entry that was standing
                // in the way of a re-add starting a fresh one
                // ([`Player::cache_media`]'s dedupe reads presence here).
                if let Some(scan) = self.silence_bg.remove(&(path.to_path_buf(), stream)) {
                    scan.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                // The picked row may be the one that just went, and the engine
                // reseeks, so this owes the flag reset like every other edit.
                if self.selected_asset.as_ref() == Some(&(path.to_path_buf(), stream)) {
                    self.selected_asset = None;
                }
                // A copied clip names its source by *index*, and every index
                // past the one that went has just moved down: without this the
                // next paste puts some other file on the timeline.
                self.clipboard = clipboard_after_remove(self.clipboard, idx);
                self.reset_after_reseek();
                // The last row leaves a session naming no file: nothing to
                // play, nothing to save and nothing to show, which is the empty
                // window the editor launches as. The next import scaffolds a
                // fresh timeline from whatever file it is, at that file's own
                // rate -- which is why the session goes rather than lingering
                // on with the gone file's parameters.
                match self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.sources().is_empty())
                {
                    true => {
                        self.close_session();
                        format!(
                            "REMOVED {} — the library is empty; import a file to start again",
                            file_name(path)
                        )
                    }
                    // The undo stack goes with it (`Project::remove_source`):
                    // said here, because a `z` that does nothing afterwards
                    // would otherwise read as a bug.
                    false => format!(
                        "REMOVED {} — there is nothing left to undo",
                        file_name(path)
                    ),
                }
            }
            Some(Err(e)) => format!("NOT REMOVED — {e}"),
            None => "NO TIMELINE — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// Back to the window the editor launches as: no timeline, no library, no
    /// picture -- and the hint that says to open a file. What removing the last
    /// library row leaves, since a session whose library is empty has nothing
    /// left to be ([`Player::remove_source`]).
    ///
    /// Everything a *loaded project* resets goes here for its reasons (an index
    /// into a timeline that is gone names nothing), plus the three per-file
    /// caches: they are keyed by path, and the next file to arrive fills them
    /// again.
    pub(crate) fn close_session(&mut self) {
        self.session = None;
        // The picture goes with it, or the empty window would keep showing the
        // last frame of a timeline that no longer exists.
        //
        // corner-cut: its atlas tile is not released -- `window.drop_image` wants
        // a `&mut Window` this door has no other reason to take. One tile per
        // emptied library, against one per displayed frame in `pump`; the
        // upgrade path is threading the window through `act_on_row`.
        self.image = None;
        // The drawn cue with it, and its tile for the same reason as above.
        self.sub_image = None;
        self.clipboard = None;
        self.selected.clear();
        self.selected_asset = None;
        // The subtitle rows go with the timeline they were on, and so does the
        // lane that was shown: S1 of one project is not S1 of the next.
        self.sub_track = 0;
        self.sub_lane = None;
        self.context_menu = None;
        self.library_menu = None;
        self.eq_open = None;
        self.color_open = None;
        self.speed_open = None;
        self.close_silence();
        self.waves.clear();
        self.streams.clear();
        self.bitrates.clear();
        self.sizes.clear();
        self.syncs.clear();
        // Scanned off sources that are not in the library any more.
        self.silence_levels.clear();
        // ...and every background read still chasing one of them: cancelled
        // rather than merely dropped, so a landing closure over a gone window
        // does not write into a `silence_levels` the next session starts
        // empty (`Player::cache_media`'s spawn checks this flag before it
        // inserts).
        for scan in self.silence_bg.values() {
            scan.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.silence_bg.clear();
        // Every gesture in flight, dropped for `reset_after_reseek`'s reason
        // (it drops the trim below): a drag holds a bar, a clip or a band of a
        // timeline that has just stopped existing.
        self.scrubbing = false;
        self.volume_dragging = false;
        self.eq_dragging = false;
        self.speed_dragging = false;
        self.color_dragging = false;
        self.pending_color = None;
        self.pending_speed = None;
        self.displayed = 0;
        self.dropped = 0;
        self.started = None;
        // The empty window's own: no name in the titlebar, nowhere chosen to
        // export or save to yet, and a rate that only keeps the timecode
        // reading in frames until a file brings its own (`main`).
        self.name = NO_FILE.into();
        self.export_path = PathBuf::new();
        self.project_path = PathBuf::new();
        self.fps = 30.;
        // No decoder to wait for a frame from: the hint is what shows. The
        // transport reads `Stopped` from the session being gone, so there is no
        // end-of-stream state left to clear here.
        self.reset_after_reseek();
        self.seek_since = None;
    }

    /// The row's own play button: opens `path` as a session of its own and
    /// shows it in place of the timeline's picture, without moving the
    /// timeline's playhead or touching its undo stack -- what a library
    /// preview is. [`PlaybackSession::open_with_audio_stream`] builds exactly
    /// this (a one-clip session, ungraded, passthrough) bound to `stream`, the
    /// row's own audio track rather than always the file's first.
    pub(crate) fn open_preview(
        &mut self,
        path: &std::path::Path,
        stream: usize,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The timeline would otherwise keep playing under the preview -- two
        // soundtracks at once -- so it is paused here and resumed on the way
        // out only if this is what paused it ([`Player::close_preview`]).
        self.preview_playing = self.session.as_ref().is_some_and(PlaybackSession::is_playing);
        if let Some(session) = self.session.as_mut() {
            session.pause();
        }
        match PlaybackSession::open_with_audio_stream(path, stream) {
            Ok(mut session) => {
                session.drop_late_pictures(true);
                session.set_gain(self.volume.gain());
                // The button says "plays it" -- a preview that opens paused
                // reads as a frozen picture, not as a player awaiting a click.
                session.play();
                self.preview_session = Some(session);
                self.notify_user(
                    format!("PREVIEWING {} — not on the timeline; esc stops it", file_name(path))
                        .into(),
                );
            }
            Err(e) => self.notify_user(format!("PREVIEW FAILED: {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// The way out of a preview: `esc`, or the banner's own button. The
    /// timeline was never pumped while the preview played, so its picture is
    /// stale where it left off -- a reseek to the position it is already at
    /// is what brings a fresh frame back, exactly as loading a project does.
    pub(crate) fn close_preview(&mut self, cx: &mut Context<Self>) {
        if self.preview_session.take().is_none() {
            return;
        }
        self.notify_user("PREVIEW STOPPED — back to the timeline".into());
        match self.session.as_ref().map(PlaybackSession::now) {
            Some(now) => {
                self.seek(now, cx);
                if self.preview_playing {
                    if let Some(session) = self.session.as_mut() {
                        session.play();
                    }
                }
            }
            None => {
                self.image = None;
                cx.notify();
            }
        }
        self.preview_playing = false;
    }

    /// The rate and layout the whole timeline's audio is, taken from the stream
    /// of the first source that could have one: what a library row has to match
    /// to be placeable. `None` until that file has been probed, and then nothing
    /// is greyed for it.
    ///
    /// The first source that is *not a still*, which is the rule the engine
    /// holds every import to (`playback::audio_source_of`) -- a picture at the
    /// head of the list (a removal moves indexes) has no stream to describe
    /// anything with.
    pub(crate) fn timeline_audio(&self) -> Option<(u32, u16)> {
        let first = self
            .session
            .as_ref()?
            .sources()
            .iter()
            .find(|s| !engine::is_image(&s.path))?;
        let info = self
            .streams
            .get(&first.path)?
            .iter()
            .find(|s| s.index == first.audio_stream)?;
        Some((info.sample_rate, info.channels))
    }

    /// Queues a file for the library. Nothing is read here: the reading is
    /// [`read_ahead`] on a worker, and [`Player::take_import`] is what finally
    /// touches the timeline, one repaint later and with the pages warm. A drop
    /// is not a key press, so the export guard on the key handler does not
    /// cover it and this checks for itself.
    ///
    /// One file at a time, in arrival order: a drop can carry six and argv can
    /// name more, and six header walks racing over one disk finish no sooner
    /// than six in a row -- while the line above the panel has exactly one file
    /// to name.
    pub(crate) fn import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.imports.push_back(path.to_path_buf());
        self.start_import(cx);
    }

    /// Starts the worker for the next queued file, if no worker is running.
    /// Called again as each import lands, which is what drains the queue.
    pub(crate) fn start_import(&mut self, cx: &mut Context<Self>) {
        if self.importing.is_some() {
            return;
        }
        let Some(path) = self.imports.pop_front() else {
            return;
        };
        let stage = Arc::new(std::sync::atomic::AtomicU8::new(ImportStage::Header as u8));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The fork is made here, once, and carried to the landing: an import is
        // *probed* on the worker and registered from what came back, while the
        // file argv named -- and a file arriving at a window with no timeline to
        // import into -- is *opened* on the worker and handed over whole. None
        // of the three leaves the UI thread anything to read: a cold 24 GB
        // header walk is twenty seconds, and the window keeps painting through
        // all of them.
        let what = arrival(self.opening.as_deref(), &path);
        // The timeline the file will be checked against, taken here because the
        // worker cannot reach the session: two clones and no disk
        // ([`PlaybackSession::import_gate`]). `None` is a window with nothing to
        // import into, which is the fork that opens the file outright.
        let gate = self.session.as_ref().map(PlaybackSession::import_gate);
        let read = cx.background_executor().spawn({
            let (path, stage) = (path.clone(), Arc::clone(&stage));
            let cancelled = Arc::clone(&cancelled);
            // The Cancel beside the line reaches the *walk* this way: every
            // cluster the reads below step over checks this flag, so a stopped
            // import stops the disk rather than only the landing
            // ([`engine::demux::with_cancel`]).
            async move {
                engine::demux::with_cancel(&cancelled, || open_ahead(what, &path, &stage, gate))
            }
        });
        let now = Instant::now();
        self.importing = Some(Import {
            path: path.clone(),
            started: now,
            stage,
            seen: ImportStage::Header,
            since: now,
            cancelled: Arc::clone(&cancelled),
        });
        cx.spawn(async move |this, cx| {
            let landed = read.await;
            this.update(cx, |this, cx| {
                this.importing = None;
                // Cancelled while it read: the window was given back at the
                // click and said so then, so what the worker carried is dropped
                // without a second word ([`Player::cancel_import`]).
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.take_import(&path, landed, cx);
                // The next one is started by the repaint this notified, which
                // is also what starts the files argv named ([`poll_import`]).
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the import line's two clocks honest: the elapsed one runs from the
    /// worker, and the stall one from the last time the stage it is naming
    /// actually changed. Sampled here rather than while drawing, for
    /// [`Player::poll_export`]'s reason.
    ///
    /// ...and starts whatever is queued behind it, which is the one place the
    /// files argv named can begin: they are put in the queue before there is a
    /// context to spawn a worker from.
    /// Takes each finished stand-in's verdict, once. Sampled here rather than
    /// while drawing, for [`Player::poll_export`]'s reason -- and it is the one
    /// place a proxy job's outcome is read, so "ready" on a row and the file in
    /// the cache are the same fact.
    ///
    /// Nothing waits for it: until a proxy is ready the film itself is what
    /// plays, switch or no switch ([`engine::PlaybackSession::picture_path`]),
    /// so a failure costs a line and no more.
    ///
    /// A stopped one settles here too ([`Player::cancel_proxy`]), which is what
    /// makes the stop honest: the worker's own answer is what says the encode
    /// is over and its half-written file gone -- and where the encode *beat*
    /// the cancel, the answer is a finished stand-in and it is reported as one
    /// rather than thrown away.
    pub(crate) fn poll_proxies(&mut self, cx: &mut Context<Self>) {
        let done: Vec<(PathBuf, bool, engine::Result<PathBuf>)> = self
            .proxies
            .iter()
            .filter_map(|(path, state)| match state {
                Proxy::Making(job) => Some((path.clone(), false, job.outcome()?)),
                Proxy::Cancelling(job) => Some((path.clone(), true, job.outcome()?)),
                _ => None,
            })
            .collect();
        for (path, stopped, outcome) in done {
            let text = match (&outcome, stopped) {
                (Ok(_), _) => format!(
                    "PROXY READY for {} — Proxies on cuts on it",
                    file_name(&path)
                ),
                // Not a failure and not worded as one: the worker gave up
                // because it was asked to, and it took its half-written file
                // with it.
                (Err(_), true) => format!(
                    "PROXY STOPPED for {} — nothing of it was kept, and the film itself is what \
                     plays",
                    file_name(&path)
                ),
                (Err(e), false) => format!(
                    "PROXY FAILED for {} — {e} — the film itself is what plays",
                    file_name(&path)
                ),
            };
            eprintln!("{text}");
            self.notify_user(text.into());
            let state = match (outcome, stopped) {
                (Ok(_), _) => Proxy::Ready,
                (Err(_), true) => Proxy::Cancelled,
                (Err(_), false) => Proxy::Failed,
            };
            self.proxies.insert(path, state);
            cx.notify();
        }
    }

    /// Starts the stand-in for one file: the slot is taken the moment the ask
    /// goes out, so a repaint mid-encode cannot start a second one, and what
    /// comes back off the worker settles the slot.
    ///
    /// One body, two doors: the sweep [`Player::cache_media`] runs over
    /// everything the session has not seen, and the switch on a library row
    /// ([`Player::toggle_proxy`]) for the one file under the pointer. The cap on
    /// how many run at once belongs to the sweep -- a person who asks for *this*
    /// film's stand-in has asked, and is told by the row how far it has got.
    pub(crate) fn start_proxy_for(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let path = path.to_path_buf();
        self.proxies.insert(path.clone(), Proxy::Asked);
        let started = cx.background_executor().spawn({
            let path = path.clone();
            async move { engine::proxy::generate_if_wanted(&path) }
        });
        cx.spawn(async move |this, cx| {
            let started = started.await;
            this.update(cx, |this, cx| {
                // Switched off while the header was being read
                // ([`Player::toggle_proxy`]): the slot is no longer the `Asked`
                // this put there, so what came back is *stopped as it lands*
                // rather than installed over the stop. Without this the second
                // half of a click made during the ask is an encoder nothing in
                // the map can reach -- and it finishes minutes later and puts
                // back the stand-in somebody switched off.
                let stopped = !matches!(this.proxies.get(&path), Some(Proxy::Asked));
                let state = match started {
                    // Cancelled and *held*, exactly as a stop of a running one
                    // is: the worker's own answer is what settles it, so an
                    // encode that beat the stop is reported as the finished
                    // stand-in it is ([`Player::poll_proxies`]).
                    Ok(Some(job)) if stopped => {
                        job.cancel();
                        Proxy::Cancelling(job)
                    }
                    // ...and a stop of something that was never going to be
                    // encoded is simply the film itself, which is what it was.
                    Ok(None) | Err(_) if stopped => Proxy::Cancelled,
                    Ok(Some(job)) => Proxy::Making(job),
                    // A film that needs none keeps the state it was inserted
                    // with, which is what it is.
                    Ok(None) => Proxy::Native,
                    Err(e) => {
                        let text = format!(
                            "NO PROXY for {} — {e} — the film itself is what plays",
                            file_name(&path)
                        );
                        eprintln!("{text}");
                        this.notify_user(text.into());
                        Proxy::Failed
                    }
                };
                this.proxies.insert(path, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The switch on a library row: this file's stand-in on, or off. One
    /// control for the whole life of the thing, because a row with a separate
    /// start, stop and delete is three buttons for one fact.
    ///
    /// * being made -- the ask is to *stop*, which is
    ///   [`Player::cancel_proxy`]'s half-written-file-and-all path and never a
    ///   deletion;
    /// * asked for, and the worker has not answered yet -- the same stop, made
    ///   before there is a job to stop: the slot is marked and the answer is
    ///   cancelled as it lands ([`Player::start_proxy_for`]). Never a second
    ///   start, which is what two encoders on one `.part` path used to be;
    /// * there is one -- the file in the cache goes ([`engine::proxy::delete`]),
    ///   and the slot is left [`Proxy::Cancelled`] rather than emptied: an empty
    ///   slot is what the next repaint's sweep starts an encode from
    ///   ([`proxies_to_start`] passes on everything the map has not seen), so
    ///   emptying it would rebuild the stand-in a second after it was turned
    ///   off. A delete that could not happen leaves the row *on*, because the
    ///   file is still there and still what plays;
    /// * anything else -- start one.
    ///
    /// `showed_stop` is what the row was drawing when the pointer went down:
    /// a stand-in that finished between that paint and this click is a click on
    /// a **stop**, and a stop settles as the ready stand-in it is rather than
    /// deleting a file nobody asked to lose ([`Player::cancel_proxy`]'s own
    /// promise).
    ///
    /// The off is remembered past the window ([`engine::proxy::set_off`]): the
    /// sweep runs again at every launch, and a switch that forgets is one that
    /// re-encodes tomorrow what was turned off today.
    pub(crate) fn toggle_proxy(
        &mut self,
        path: &std::path::Path,
        showed_stop: bool,
        cx: &mut Context<Self>,
    ) {
        match self.proxies.get(path) {
            Some(Proxy::Making(_)) => self.cancel_proxy(path, cx),
            // Already winding down: the second click on a stop is nothing.
            Some(Proxy::Cancelling(_)) => {}
            Some(Proxy::Asked) => {
                engine::proxy::set_off(path, true);
                self.proxies.insert(path.to_path_buf(), Proxy::Cancelled);
                let text = format!("STOPPING the stand-in for {}…", file_name(path));
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            // The encode beat the click: what is under the pointer is a stop,
            // and the film has a stand-in now.
            Some(Proxy::Ready) if showed_stop => {
                let text = format!(
                    "PROXY READY for {} — it finished before the stop; click again to delete it",
                    file_name(path)
                );
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            Some(Proxy::Ready) => {
                let text = match engine::proxy::delete(path) {
                    Ok(gone) => {
                        engine::proxy::set_off(path, true);
                        self.proxies.insert(path.to_path_buf(), Proxy::Cancelled);
                        match gone {
                            true => format!(
                                "PROXY OFF for {} — the film itself is what plays",
                                file_name(path)
                            ),
                            false => {
                                format!("NO PROXY for {} — nothing to delete", file_name(path))
                            }
                        }
                    }
                    // The file is still on the disk and [`engine::proxy::cached`]
                    // is still handing it to playback, so the row stays on: the
                    // one thing it must never do is say off over a proxy that is
                    // playing.
                    Err(e) => format!(
                        "PROXY KEPT for {} — {e} — it could not be deleted and is still what plays",
                        file_name(path)
                    ),
                };
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            _ => {
                engine::proxy::set_off(path, false);
                self.start_proxy_for(path, cx);
            }
        }
    }

    /// The × on a library row while its stand-in is being made: the worker is
    /// asked to give up at its next frame and delete the half-written file it
    /// was writing ([`engine::proxy::Job::cancel`]), and the row says so until
    /// it has. A whole feature film re-encoded is minutes of a machine the cut
    /// is being made on, and there was no way to take it back.
    ///
    /// Nothing is deleted from here and nothing waits: the film itself is what
    /// plays throughout ([`engine::PlaybackSession::picture_path`] stats the
    /// cache and finds nothing), so a stop costs a line and no more. Asking
    /// twice is asking once -- the second click finds a state that is not
    /// [`Proxy::Making`] and answers nothing -- and a cancel that arrives after
    /// the encode has already written its file settles as the *ready* stand-in
    /// it is ([`Player::poll_proxies`]), never as a deletion of one in use.
    pub(crate) fn cancel_proxy(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Taken out of its slot and put back one state on, rather than removed:
        // a path that left this map is a path the next repaint starts an encode
        // for all over again ([`Player::cache_media`]).
        let Some(slot) = self.proxies.get_mut(path) else {
            return;
        };
        if !matches!(slot, Proxy::Making(_)) {
            return;
        }
        let Proxy::Making(job) = std::mem::replace(slot, Proxy::Cancelled) else {
            unreachable!("the slot was just read as one being made")
        };
        job.cancel();
        *slot = Proxy::Cancelling(job);
        // Remembered past this window, for [`Player::toggle_proxy`]'s reason:
        // the launch sweep would otherwise start again what was just stopped.
        engine::proxy::set_off(path, true);
        let text = format!("STOPPING the stand-in for {}…", file_name(path));
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// What the Proxies button says after the state: how far the stand-ins
    /// have got, since "on" with nothing made yet and "on" with the whole
    /// library standing in are two different things to be looking at. Empty
    /// where no film here wants one, which is the ordinary case.
    pub(crate) fn proxy_tail(&self) -> String {
        let making = self.proxies.values().filter_map(|p| match p {
            Proxy::Making(job) => Some(job.progress()),
            _ => None,
        });
        // The one furthest along would read as done while three others sit at
        // nothing: the *least* finished is what the button reports.
        match making.fold(f32::INFINITY, f32::min) {
            least if least.is_finite() => format!(" · {}%", (least * 100.) as u32),
            _ => String::new(),
        }
    }

    /// Pushes the window's two stand-in switches at the session, which is the
    /// only place they are ever written -- [`Player::apply_volume`]'s contract,
    /// and for a sharper reason: a fresh session comes up making stand-ins for
    /// every big film, so a person who turned that off before importing
    /// anything would watch the very import they were waiting to prevent start
    /// an encode.
    pub(crate) fn apply_proxies(&mut self) {
        let (on, auto) = (self.proxies_on, self.auto_proxies_on);
        if let Some(session) = &mut self.session {
            session.set_proxies(on);
            session.set_auto_proxies(auto);
        }
    }

    /// Whether any stand-in is still being made -- what keeps the frame loop
    /// alive while one is, so its percentage moves on screen.
    ///
    /// One being stopped counts: its worker is still running until it reaches
    /// the frame it gives up at, and the repaint that notices *that* is this
    /// same loop -- without it a stopped row would sit on "stopping…" until
    /// something else happened to repaint the window.
    pub(crate) fn making_proxies(&self) -> bool {
        self.proxies
            .values()
            .any(|p| matches!(p, Proxy::Making(_) | Proxy::Cancelling(_)))
    }

    /// How many films are between "shall we?" and "done": the number the start
    /// of another is held against ([`PROXIES_AT_ONCE`]). A header being read
    /// counts, because the answer may be an encode -- and so does one still
    /// winding down from a stop, which is an encoder that has not let go of the
    /// seat yet.
    pub(crate) fn in_flight_proxies(&self) -> usize {
        self.proxies
            .values()
            .filter(|p| matches!(p, Proxy::Asked | Proxy::Making(_) | Proxy::Cancelling(_)))
            .count()
    }

    /// Cuts on the stand-ins, or on the films themselves. The picture and only
    /// the picture: the sound is the film's either way, and so is every frame
    /// an export writes.
    ///
    /// Set on the window and pushed at the session, not the other way about:
    /// this is an import option and a person picks it before the first film
    /// arrives ([`Player::apply_proxies`]). With nothing open it is the pick
    /// the next session comes up with.
    pub(crate) fn toggle_proxies(&mut self, cx: &mut Context<Self>) {
        let on = !self.proxies_on;
        self.proxies_on = on;
        if let Some(session) = &mut self.session {
            session.set_proxies(on);
        }
        // What is really under the switch, said in the same breath: a project
        // whose films have no stand-in yet would otherwise read as a switch
        // that does nothing.
        let ready = self
            .proxies
            .values()
            .filter(|p| matches!(p, Proxy::Ready))
            .count();
        let making = self
            .proxies
            .values()
            .filter(|p| matches!(p, Proxy::Making(_)))
            .count();
        let text = match (on, ready, making) {
            (false, ..) => "PROXIES OFF — the films themselves are what play".to_string(),
            (true, 0, 0) => {
                "PROXIES ON — no film here has one, so the films themselves play".to_string()
            }
            (true, 0, n) => format!("PROXIES ON — {n} still being made; the films play until then"),
            (true, n, 0) => format!("PROXIES ON — cutting on {n}"),
            (true, n, m) => format!("PROXIES ON — cutting on {n}, {m} still being made"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        // The switch reseeks the session, and the wait for that picture is what
        // this clears -- with nothing open there is no open to wait for, and a
        // clock started against one would be a seek line about nothing.
        if self.session.is_some() {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Makes the stand-ins as the films arrive, or makes none until they are
    /// asked for. The other half of the switch above: this one decides what is
    /// *encoded*, that one what is *watched*.
    ///
    /// With it off nothing is started by an import at all -- turning Proxies on
    /// is what asks for the ones this project needs ([`Player::cache_media`]),
    /// which is said here because a switch whose only effect is elsewhere is a
    /// switch that reads as doing nothing.
    ///
    /// The window's own, like the switch above and for its reason: this one
    /// decides what an *import* does, so it is answerable before the import.
    pub(crate) fn toggle_auto_proxies(&mut self, cx: &mut Context<Self>) {
        let on = !self.auto_proxies_on;
        self.auto_proxies_on = on;
        if let Some(session) = &mut self.session {
            session.set_auto_proxies(on);
        }
        let text = match on {
            true => "AUTO PROXIES ON — a film that wants a stand-in gets one as it arrives",
            false => "AUTO PROXIES OFF — no import makes one; Proxies on is what asks for them",
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    pub(crate) fn poll_import(&mut self, cx: &mut Context<Self>) {
        match &mut self.importing {
            Some(import) => {
                import.poll();
            }
            None => self.start_import(cx),
        }
    }

    /// The Cancel beside the import line: the window is given back at once and
    /// the file does not land. Everything queued behind it goes too -- a person
    /// who has stopped an import of six dropped files has stopped the six, and
    /// leaving five to start themselves would be the same wait under another
    /// name.
    ///
    /// The read in flight is stopped too, where the container lets it be: a
    /// Matroska walk checks this flag between clusters and gives up there
    /// ([`engine::demux::with_cancel`]). See [`Import::cancelled`] for what an
    /// mp4 header still costs.
    pub(crate) fn cancel_import(&mut self, cx: &mut Context<Self>) {
        let Some(import) = self.importing.take() else {
            return;
        };
        import
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let waiting = self.imports.len();
        self.imports.clear();
        let tail = match waiting {
            0 => String::new(),
            n => format!(" — {n} more dropped from the queue"),
        };
        let text = format!(
            "IMPORT CANCELLED: {}{tail} — the read stops where the container lets it",
            file_name(&import.path)
        );
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a read-ahead file into the library and nowhere else: the timeline
    /// is not touched, and the row is dragged onto a lane when it is wanted
    /// there. Nothing moves, so nothing reseeks; a refusal is shown as the
    /// engine worded it and changes nothing.
    ///
    /// The export guard again, and not for the caller's sake: an export can
    /// have started during the seconds the worker was reading, and a drop
    /// during an export has always been a silent no-op.
    pub(crate) fn take_import(&mut self, path: &std::path::Path, landed: Landed, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // The file argv named is the one file in the queue that is not an
        // import: it *is* the timeline, and the worker has already opened it.
        // All that is left here is to hang everything derived from it off the
        // window -- the clock, the title, where an export and a save go -- and
        // that is arithmetic, not a read.
        let (subs, probe) = match landed {
            Landed::Read(subs, probe) => (subs, probe),
            what => {
                // Only when *this* is the file argv named: a project dropped
                // while that one is still being read must not make it land as
                // an import.
                if self.opening.as_deref() == Some(path) {
                    self.opening = None;
                }
                match what {
                    Landed::Project(opened) => self.install_project(path, opened, cx),
                    Landed::Media(opened, place) => {
                        let text = self.install_media(path, opened, place);
                        eprintln!("{text}");
                        self.notify_user(text.into());
                        cx.notify();
                    }
                    Landed::Read(..) => unreachable!("matched above"),
                }
                // The line a launch has always printed, now printed when the
                // file actually arrives: it is the mark that says the timeline
                // is up, as the window's own appearance is the other one.
                if let Some(meta) = self.session.as_ref().map(PlaybackSession::meta) {
                    println!(
                        "{}: {}x{} @ {:.2} fps, {} samples",
                        path.display(),
                        meta.width,
                        meta.height,
                        meta.frame_rate,
                        meta.frame_count
                    );
                }
                return;
            }
        };
        // An empty window has no library to add to yet: the file opens one, and
        // the timeline under it stays empty, because an import is an import
        // whether or not a session was already up. A file *named at launch* is
        // the other fork -- that one is an open, and it does become the
        // timeline (`main`).
        // A subtitle file is not a source and lands on no lane: it joins the
        // timeline's own list of them, which is what the library's subtitle
        // section shows and what the overlay draws. With no timeline open there
        // is nothing for the cues to be timed against, and it says so.
        if is_subtitle(path) {
            self.take_subtitles(path, subs, cx);
            return;
        }
        // The container was read on the worker and what came back is registered
        // here ([`engine::PlaybackSession::import_probed`]): no header walk, no
        // decoder open, no probe of the timeline's own first source -- the three
        // reads that used to be spent on this thread. A song and a still fork
        // before the demuxer and pay their own small read
        // ([`engine::PlaybackSession::import`]); a window whose timeline went
        // away while the worker read falls to the slow door below, which is the
        // one that can still open one.
        let registered = match (self.session.as_mut(), probe) {
            (Some(session), Some(Ok(probe))) => Some(session.import_probed(path, probe)),
            (Some(_), Some(Err(refused))) => Some(Err(refused)),
            (Some(session), None) => Some(session.import(path)),
            (None, _) => None,
        };
        let text = match registered {
            Some(Ok(_)) => {
                // The file's own subtitle tracks with it, exactly as an open
                // takes them: an import is the other door the same file arrives
                // through. The cues were read on the worker
                // ([`read_ahead`]); what happens here is the push.
                let tail = self
                    .session
                    .as_mut()
                    .and_then(|session| subtitle_tail(session, subs))
                    .unwrap_or_default();
                format!(
                    "IMPORTED {} to the library — drag it onto a lane to place it{tail}",
                    file_name(path)
                )
            }
            // Named, because two files can fail in one launch and the queue now
            // shows both: "No such file or directory" twice over, with nothing
            // saying which file, is two messages that answer nothing.
            Some(Err(e)) => format!("IMPORT FAILED: {} — {e}", file_name(path)),
            None => self.open_media(path, false, subs),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a file as the session an empty window is waiting for. Everything
    /// derived from the media -- the clock, the title, where an export and a
    /// save go -- is set here, exactly as a launch with a file argument sets
    /// it. Paused with its first frame showing, like every other way a timeline
    /// arrives.
    ///
    /// `place` is the difference between the two doors that come here: a file
    /// *opened* is the timeline, one *imported* into an empty window fills the
    /// library and leaves the lanes empty for a drag.
    pub(crate) fn open_media(&mut self, path: &std::path::Path, place: bool, subs: Subs) -> String {
        self.install_media(path, open_session(path, place, subs), place)
    }

    /// The second half of it: everything the window derives from a session that
    /// has already been opened. Split from the open itself because the file
    /// argv named is opened on a worker ([`open_ahead`]) -- the twelve seconds
    /// of a cold header walk are not the UI thread's to spend -- and lands
    /// here, where nothing is read and nothing blocks.
    pub(crate) fn install_media(
        &mut self,
        path: &std::path::Path,
        opened: Result<(PlaybackSession, String), String>,
        place: bool,
    ) -> String {
        match opened {
            Ok((mut session, subs)) => {
                // A size and/or a rate picked before this file existed
                // ([`Player::apply_resolution`], [`Player::apply_frame_rate`])
                // are the project's from its very first frame -- the same
                // precedence a `.edith`'s own saved settings already have over
                // the scaffold ([`engine::PlaybackSession::open_project`]).
                // Consumed once: a later file replacing this session finds
                // nothing left pending.
                if let Some((w, h)) = self.pending_settings.0.take() {
                    session.set_resolution(w, h);
                }
                if let Some(fps) = self.pending_settings.1.take() {
                    session.set_frame_rate(fps);
                }
                self.fps = session.meta().frame_rate;
                // This window *is* the real-time viewer: a picture already past
                // the clock is one `pump` takes off the channel and throws away,
                // so the worker stops paying for it instead
                // ([`PlaybackSession::drop_late_pictures`]).
                session.drop_late_pictures(true);
                // Read before the session moves: a file that plays silent says
                // so here or nowhere.
                let silent = audio_notice(&session);
                // A file replaces the one that was open, and track 3 of that one
                // is not track 3 of this.
                self.sub_track = 0;
                self.session = Some(session);
                // A fresh session comes up at full volume; the player's own
                // setting outlives the file, so it is pushed at every new one.
                self.apply_volume();
                // ...and so do the stand-in switches, which were answerable
                // before this file existed and decide what its own import does.
                self.apply_proxies();
                // Beside the new file, but still the format the card is set to:
                // opening another clip is not a change of mind about that.
                self.export_path = retarget(&export_path(path), self.format);
                self.project_path = project_path(path);
                self.name = file_name(path).into();
                self.reset_after_reseek();
                let name = file_name(path);
                // The library is filled and the timeline is empty; the only
                // thing that says so is this line, so it says what to do next.
                let what = match place {
                    true => format!("OPENED {name}"),
                    false => {
                        format!("IMPORTED {name} to the library — drag it onto a lane to place it")
                    }
                };
                format!("{what}{}{subs}", silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        }
    }

    /// The Import button: asks the desktop for a path and takes it the same way
    /// a drop would. The chooser is another process and the user may sit in it,
    /// so it runs on a background thread -- blocking here would freeze the
    /// window behind the dialog.
    pub(crate) fn pick_and_import(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — import") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                // One queue, and the fork is made when its worker starts
                // ([`arrival`]): a project replaces the timeline, media joins
                // the library, and neither is read on this thread.
                Ok(Some(path)) => this.import(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The `+ S` button and its key: asks the desktop for a file and takes the
    /// subtitle tracks out of it -- a standalone `.srt`/`.vtt`/`.ass` is one of
    /// them, a Matroska however many are inside. Only the subtitles: the file
    /// itself does not join the library, which is what the Import button beside
    /// this one is for.
    ///
    /// The chooser is another process and the user may sit in it, so it runs on
    /// a background thread, exactly as [`Player::pick_and_import`] does.
    pub(crate) fn pick_and_add_subtitles(&mut self, cx: &mut Context<Self>) {
        // What dims the `+ S` button, asked here as well so the key answers the
        // same question -- and *before* the chooser rather than after it: a door
        // that opens a dialog, waits for a file and only then says the timeline
        // was never there is the second door disagreeing with the first.
        if let Some(why) = self.enable(ActionId::ImportSubtitles, None).why() {
            let text = format!("NO SUBTITLES ADDED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — subtitles to add") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.add_subtitles(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(text) => {
                    eprintln!("{text}");
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a file's subtitle tracks onto the timeline, off the render thread.
    /// The walk reads the whole container for its cues
    /// (`engine::PlaybackSession::parse_subtitles`) -- ~200 ms on a two-hour 4K
    /// remux and 1.3 s on a cold 3 GB one -- and a button that costs the window
    /// that many frames is a button that freezes it. So the *parse* is the
    /// worker's, whole, and the UI thread only pushes what came back
    /// ([`PlaybackSession::add_subtitle_tracks`]): no borrow crosses the await,
    /// because the parse is an associated fn that owns nothing.
    pub(crate) fn add_subtitles(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Nothing to time the cues against: said now rather than after a walk
        // of a 25 GB file that was never going to be kept.
        if self.session.is_none() {
            self.landed_subtitles(path, None, cx);
            return;
        }
        self.notify_user(format!("READING {} for subtitles…", file_name(path)).into());
        let parsed = cx.background_executor().spawn({
            let path = path.to_path_buf();
            async move { engine::PlaybackSession::parse_subtitles(&path) }
        });
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let parsed = parsed.await;
            this.update(cx, |this, cx| {
                // The dedupe lives inside the push, so a second `+ S` on the
                // same file still answers 0 and still says so below.
                let added = this
                    .session
                    .as_mut()
                    .map(|session| parsed.and_then(|tracks| pushed(session, &path, tracks)));
                this.landed_subtitles(&path, added, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Every subtitle track a `.srt`/`.vtt`/`.ass` carries, onto the timeline
    /// and nowhere else: they are not clips and land on no lane. The cues came
    /// off the worker that read the file ([`read_ahead`]), like every other
    /// door's do, and what is left here is the push. The engine dedupes by
    /// (file, track), so the same `.srt` twice is one row and says so.
    pub(crate) fn take_subtitles(&mut self, path: &std::path::Path, subs: Subs, cx: &mut Context<Self>) {
        let added = self
            .session
            .as_mut()
            .map(|session| subs.and_then(|tracks| pushed(session, path, tracks)));
        self.landed_subtitles(path, added, cx);
    }

    /// What the timeline says once the tracks are on it, whichever worker did
    /// the reading: the `+ S` button and its key ([`Self::add_subtitles`]), a
    /// dropped or imported subtitle file ([`Self::take_subtitles`]), and a
    /// window with nothing to time cues against all word the outcome here,
    /// once, so no two doors can drift apart.
    pub(crate) fn landed_subtitles(
        &mut self,
        path: &std::path::Path,
        added: Option<engine::Result<usize>>,
        cx: &mut Context<Self>,
    ) {
        let text = match added {
            Some(Ok(0)) => format!(
                "{}'s subtitles are in the palette already",
                file_name(path)
            ),
            // Where they are and what to do with them: the tracks are in the
            // palette and nothing is placed until one is dragged onto an S
            // track, which is the whole of what this door does.
            Some(Ok(n)) => format!(
                "SUBTITLES {} — {n} track(s) in the palette — drag one onto an S track ({} adds \
                 one), showing over the picture meanwhile, {} hides them",
                file_name(path),
                self.keymap.display(ActionId::AddSubtitleLane),
                self.keymap.display(ActionId::ToggleSubtitles)
            ),
            Some(Err(e)) => format!("SUBTITLE IMPORT FAILED: {e}"),
            None => "NO SUBTITLES ADDED — open a file for them to run against first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// The × on a subtitle row, and the stroke that takes the picked one off:
    /// the track leaves the timeline and the pick moves with it. Every index
    /// past the one that went moves down
    /// ([`engine::Project::remove_subtitles`]), so a pick left where it was
    /// would name a *different* track -- and the pick is what an export writes
    /// into the file.
    ///
    /// Not an undo step: subtitles are not on the history's snapshots, so the
    /// way back is putting the file's subtitles on again -- which is a door of
    /// its own ([`Player::pick_and_add_subtitles`]) and reads the subtitles
    /// alone, never the media. The notice says that rather than promising a
    /// ctrl+z that would do nothing.
    pub(crate) fn remove_subtitle_track(&mut self, track: usize, cx: &mut Context<Self>) {
        // The one availability oracle, for the same reason the × on a row and
        // the stroke are one call: an empty list is not a failure, it is an
        // action with nothing to act on, and the engine's "there is no subtitle
        // track 0" is an index nobody typed. A real removal that fails still
        // says what the engine said, below.
        // The palette's list is what this takes a row off, and an empty list is
        // the very fact the toggle is refused by -- so the same arm answers
        // both (oracle.rs) rather than a second reading of the same state.
        if let Some(why) = self.enable(ActionId::ToggleSubtitles, None).why() {
            let text = format!("NO SUBTITLES REMOVED — {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        // Read before it goes: a notice naming an index names nothing.
        let name = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), track))
            .unwrap_or_else(|| format!("subtitle track {track}"));
        let text = match self
            .session
            .as_mut()
            .map(|session| session.remove_subtitles(track))
        {
            Some(Ok(())) => {
                let left = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.subtitles().len());
                self.sub_track = sub_pick_after_removal(self.sub_track, track, left);
                // The drawn cue is keyed by that index ([`Player::sub_picture`])
                // and the index now stands for another track.
                //
                // corner-cut: its atlas tile is not released -- `close_session`'s
                // note, for its reason and with its upgrade path.
                self.sub_image = None;
                format!(
                    "{name} REMOVED — {} puts a file's subtitles back in the palette, the file \
                     itself stays off",
                    self.keymap.display(ActionId::ImportSubtitles)
                )
            }
            Some(Err(e)) => format!("NO SUBTITLES REMOVED — {e}"),
            None => "NO SUBTITLES REMOVED — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Swaps the whole timeline for one restored from a `.edith`, for
    /// [`Player::install_media`]'s reason: the open is a worker's -- a project
    /// naming a 24 GB film opens that film, which is the same twenty seconds
    /// ([`arrival`] sends every `.edith` through the one queue) -- and this is
    /// what is left once it lands. Nothing is replaced until the new session is
    /// in hand, so a refusal is shown as the engine worded it and leaves what is
    /// playing alone.
    pub(crate) fn install_project(
        &mut self,
        path: &std::path::Path,
        opened: Result<PlaybackSession, String>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let text = match opened {
            Ok(mut session) => {
                // A `.edith` carries its own saved size and rate, read at
                // [`engine::PlaybackSession::open_project`] -- a pick made
                // before this project loaded is not this project's.
                self.pending_settings = (None, None);
                self.fps = session.meta().frame_rate;
                // As at the file door above: the window watches in real time.
                session.drop_late_pictures(true);
                let silent = audio_notice(&session);
                // A project is named after itself but still exports beside its
                // media: that is the only place an export has ever landed.
                self.export_path = retarget(&export_path(&session.sources()[0].path), self.format);
                self.session = Some(session);
                self.apply_volume();
                // The other way round from every other door: a project carries
                // its own two switches ([`engine::edith`]) and they are what it
                // was saved with, so the window takes *them* rather than
                // pushing what it had -- and the buttons say what was loaded.
                if let Some(session) = &self.session {
                    self.proxies_on = session.proxies();
                    self.auto_proxies_on = session.auto_proxies();
                }
                self.project_path = path.to_path_buf();
                self.name = file_name(path).into();
                // A copied clip names its source by index, which means a
                // different file -- or none -- in another project.
                self.clipboard = None;
                self.selected.clear();
                // A menu can be up while a project is dropped on the window --
                // the scrim swallows clicks, never a drop -- and its index
                // would name some other timeline's clip. The two clip cards
                // hold a (lane, idx) of the old timeline for the same reason.
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                // Marks are timeline frames of the timeline that was.
                self.close_silence();
                // A different set of sources: the row that was picked is not
                // the file that index names any more -- and neither is the
                // subtitle track that was showing.
                self.selected_asset = None;
                self.sub_track = 0;
                // The counters describe one timeline; the eof line must not
                // report the old one's frames against the new one.
                self.displayed = 0;
                self.dropped = 0;
                self.started = None;
                // Loaded paused at its saved playhead, so the still it owes
                // reaches the screen the way a seek's does. The old picture is
                // released by the swap in `pump`, as after any other seek.
                self.reset_after_reseek();
                format!("LOADED {}{}", file_name(path), silent.unwrap_or_default())
            }
            Err(e) => format!("OPEN FAILED: {e}"),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Writes the timeline back to its project file. Overwrites silently, like
    /// an export: the path was chosen once and the notice is the confirmation.
    pub(crate) fn save_project(&mut self, cx: &mut Context<Self>) {
        let saved = self
            .session
            .as_ref()
            .map(|session| session.save_project(&self.project_path));
        let text = match saved {
            Some(Ok(())) => format!("SAVED {}", file_name(&self.project_path)),
            Some(Err(e)) => format!("SAVE FAILED: {e}"),
            None => "NOTHING TO SAVE — open a file first".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// A new empty track under the ones already there. One undo step in the
    /// engine, so the stroke that takes back an edit takes back a track too, and
    /// no reseek: nothing plays differently until something is dropped on it.
    /// The selection stays -- the lanes it indexes into have not moved.
    pub(crate) fn add_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match &mut self.session {
            Some(session) => {
                let lane = session.add_lane(kind);
                self.notify_user(
                    format!(
                        "{} ADDED — drag {} onto it, {} takes it back",
                        lane.label(),
                        // A subtitle lane takes no clip at all: what lands on it
                        // is a palette row, so the notice names the panel the
                        // hand has to go to rather than "a clip", which is the
                        // one thing that will never work here.
                        match kind {
                            LaneKind::Subtitle => "a subtitle from the panel",
                            _ => "a clip",
                        },
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            None => self.notify_user("NO TRACK ADDED — open a file first".into()),
        }
        cx.notify();
    }

    /// The × in a track's header: the add taken back, one undo step, and the
    /// engine's own words when it refuses -- those name the clips still on the
    /// track, so the notice says what to delete first. A removal never deletes a
    /// clip.
    ///
    /// Everything holding a `(lane, idx)` is dropped, because the tracks below
    /// the one that went have just moved up an `ord`
    /// ([`engine::Project::remove_lane`]): a selection or an open card kept
    /// across it would be pointing at the *next* track's clip.
    pub(crate) fn remove_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_lane(lane));
        let text = match removed {
            Some(Ok(())) => {
                self.selected.clear();
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                self.speed_open = None;
                self.close_silence();
                // The shown lane with them: a `Lane` is a position among
                // its kind, so a pick now names another track -- the same
                // reason the selection and the cards go. The first lane
                // left is drawn ([`subs::active_lane`]).
                self.sub_lane = None;
                format!(
                    "{} REMOVED — {} brings it back",
                    lane.label(),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Some(Err(e)) => format!("NO TRACK REMOVED — {e}"),
            None => "NO TRACK REMOVED — open a file first".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// A header let go over another header: the track in the hand takes that
    /// one's place in the stack, clips and all
    /// ([`engine::Project::move_lane`]), one undo step. The gesture every
    /// editor reorders tracks with, and the only way the order is ever changed
    /// -- there is no second list of it to keep in step.
    ///
    /// Display order is the stack, so moving a video track past another video
    /// track changes which picture wins, here and in an export alike; audio is
    /// summed and does not care, which is what makes `A1` above `V1` a purely
    /// visual arrangement. A label is a position among the tracks of its kind,
    /// so a track that crossed one of its own kind comes back under a different
    /// name -- and everything holding a `(lane, idx)` is dropped exactly then,
    /// for [`Player::remove_lane`]'s reason: those handles now name another
    /// track's clip. A move that crossed only the other kind renames nothing
    /// and keeps the selection.
    pub(crate) fn reorder_lane(&mut self, lane: Lane, onto: Lane, cx: &mut Context<Self>) {
        self.lane_drop = None;
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(to) = session.lanes().iter().position(|&l| l == onto) else {
            return;
        };
        // Picked up and put back down where it was is a click, and a click says
        // nothing -- `move_lane` refuses it and every other no-op.
        let Some(moved) = session.move_lane(lane, to) else {
            cx.notify();
            return;
        };
        if moved != lane {
            self.selected.clear();
            self.context_menu = None;
            self.eq_open = None;
            self.color_open = None;
            self.speed_open = None;
            self.close_silence();
            // The shown lane with them: a `Lane` is a position among its
            // kind, so a pick now names another track -- the same reason
            // the selection and the cards go.
            self.sub_lane = None;
        }
        self.notify_user(
            format!(
                "{} IS TRACK {} NOW — {} puts it back",
                moved.label(),
                to + 1,
                self.keymap.display(ActionId::Undo)
            )
            .into(),
        );
        cx.notify();
    }

    /// What the remove keys act on: the last track of that kind, which is the
    /// one the matching add key appended. Nothing at all before a file is open,
    /// where the timeline drawn is a placeholder pair.
    pub(crate) fn remove_last_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        let last = self.session.as_ref().and_then(|session| {
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == kind)
                .next_back()
        });
        match last {
            Some(lane) => self.remove_lane(lane, cx),
            // Two different nothings: no timeline at all, and a timeline whose
            // lanes of this kind have all gone -- which is the ordinary state of
            // the subtitle ones, since a project starts with none. "Open a file
            // first" over an open file is a notice that reads as a bug.
            None => {
                self.notify_user(
                    match self.session.is_some() {
                        true => format!(
                            "NO TRACK REMOVED — there is no {} track to remove",
                            match kind {
                                LaneKind::Video => "video",
                                LaneKind::Audio => "audio",
                                LaneKind::Subtitle => "subtitle",
                            }
                        ),
                        false => "NO TRACK REMOVED — open a file first".to_string(),
                    }
                    .into(),
                );
                cx.notify();
            }
        }
    }
}
