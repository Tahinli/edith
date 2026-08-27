//! Where the playhead is and where it is going: the pump, the seeks, the
//! volume and the one button that starts it.

use crate::*;

impl Player {
    /// The session the transport, the pump and the clock all read: the
    /// preview while one is showing, the timeline otherwise -- never both, so
    /// a preview is watched without moving the edit's own playhead. See
    /// [`preview_session`](Player) for what stays untouched underneath.
    pub(crate) fn active_session(&self) -> Option<&PlaybackSession> {
        self.preview_session.as_ref().or(self.session.as_ref())
    }

    pub(crate) fn active_session_mut(&mut self) -> Option<&mut PlaybackSession> {
        self.preview_session.as_mut().or(self.session.as_mut())
    }

    /// The active session's own frame rate rather than the timeline's: a
    /// preview of a file shot at another rate clocks and steps at *its* rate,
    /// not the edit's.
    pub(crate) fn active_fps(&self) -> f64 {
        self.preview_session
            .as_ref()
            .map_or(self.fps, |s| s.meta().frame_rate)
    }

    /// Catches the display up to the clock: everything already due is taken off
    /// the channel and only the last of them is shown, which *is* the
    /// drop-when-behind policy. A frame that is not due yet waits in `held`, and
    /// while the clock is paused *nothing* is due -- a repaint re-presents the
    /// frame already on screen, whatever asked for the repaint.
    pub(crate) fn pump(&mut self, window: &mut Window) {
        // Where the transport was before this drain, so the crossing into
        // `Ended` can be recognised as the one transition it is.
        let was = self.transport();
        let fps = self.active_fps();
        // No timeline, nothing to catch up to: the window is showing its empty
        // state and there is no decoder to drain.
        // A direct field borrow rather than the `active_session_mut` helper: the
        // rest of this function reads and writes other fields on `self`
        // (`held`, `seek_since`, `dropped`, `displayed`, ...) alongside
        // `session`, and only a borrow of the two Option fields themselves --
        // not a call through a `&mut self` method -- lets the borrow checker
        // see those as disjoint.
        let Some(session) = self.preview_session.as_mut().or(self.session.as_mut()) else {
            return;
        };
        // Loop-trim and the i/o range (DESIGN.md §6): the transport is kept
        // inside the active window while one is armed, so a trim or a marked
        // range can be heard and seen without a manual replay after every
        // nudge. The range wins over the trim when both are set
        // ([`active_loop_window`]). Checked off the engine's own clock, not
        // a frame counter this pump keeps -- the same `now()`
        // [`Player::step`] reads.
        let loop_window = active_loop_window(self.loop_on, self.range, self.loop_trim);
        if session.is_playing() && should_loop_restart(frame_at(session.now(), fps), loop_window) {
            let start = loop_window.map_or(0, |(lo, _)| lo);
            session.seek(f64::from(start) / fps);
        }
        // Whether the span now decoding has yet to hand over a picture, read
        // *before* the drain: the frame the drain is about to take is the one
        // that ends the prime, and its own lateness -- however long the span's
        // reopen took -- is exactly what the resync below must not answer with
        // another reopen.
        let priming = session.picture_priming();
        let target = session.now() * fps;
        let mut newest: Option<Frame> = None;
        // A frame the screen is owed: a seek's landing, and the one readiness
        // signal there is ([`Player::reset_after_reseek`]).
        let owed = self.seek_since.is_some();
        // Paused, the clock is frozen and *nothing new is due*. Whatever the
        // decoder is still handing over is the backlog it was behind by when
        // the pause landed -- frames at a position the transport has already
        // left -- and taking one per repaint is what walked the picture on
        // after the sound had stopped, at exactly the rate the pointer was
        // moved over the timeline. Gated here, at the one place a frame ever
        // reaches the screen, rather than in the handlers that repaint: a
        // hover, a notice, a resize and a vsync are all the same event to this.
        // An owed frame is still taken, playing or not -- a scrub is paused by
        // definition, and its landing is the whole point of it.
        while session.is_playing() || owed {
            let frame = match self.held.take() {
                Some(frame) => frame,
                // Nothing waiting means either a clip boundary being rebuilt or
                // the real end of the timeline, and only the engine can tell
                // them apart -- `frame.index` is already a timeline index.
                None => match session.try_frame() {
                    Some(frame) => frame,
                    None => break,
                },
            };
            if f64::from(frame.index) <= target {
                self.dropped += u32::from(newest.is_some());
                newest = Some(frame);
            } else {
                self.held = Some(frame);
                break;
            }
        }

        // How far behind the master clock the picture just handed over is, in
        // seconds. Measured off a frame that really arrived and nothing else: a
        // clip boundary being reopened delivers nothing at all for hundreds of
        // milliseconds, and restarting *that* would only cancel the open it is
        // waiting on.
        let late = newest
            .as_ref()
            .map_or(0., |f| (target - f64::from(f.index)) / fps);

        if let Some(frame) = newest {
            self.displayed += 1;
            self.seek_since = None;
            self.started.get_or_insert_with(|| {
                eprintln!("first frame displayed (index {})", frame.index);
                Instant::now()
            });
            let buf = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
                .expect("frame buffer sized width*height*4");
            // Counted here rather than under `color_open`, because the card
            // opens on a frame that was pumped before it: gating this on the
            // card would leave its graph flat until something reseeked. A
            // thousandth of the pixels, against a conversion that just touched
            // all of them.
            self.histogram = histogram(buf.as_raw());
            let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
            if let Some(old) = self.image.replace(next) {
                // Every RenderImage gets a fresh id and its own atlas tile:
                // without this the sprite atlas grows for the whole video.
                let _ = window.drop_image(old);
            }
        }

        // Audio is the master clock and a decoder that cannot keep up with it
        // never gets back on its own: it hands over every frame in order,
        // whether or not its moment has passed, so what it is behind by only
        // grows -- a minute in, the picture is seconds behind what is being
        // heard, and that is the whole of "the video can't catch the audio".
        // Past `LATE_RESYNC` the backlog is abandoned and the picture restarted
        // at the clock, which touches neither the sound nor the clock
        // (`PlaybackSession::resync_picture`), so nothing the ear is following
        // moves. Never on a frame a seek owed: that one is late by however long
        // its own reopen took, and answering it with another reopen is a loop.
        // The same is true of a *clip boundary's* first frame -- a silence cut
        // leaves a timeline of clips shorter than their own reopen -- so the
        // restart waits for a span that has delivered and fallen behind after
        // it (`resync_due`, primed on `PlaybackSession::picture_priming`):
        // restarting the prime only buys the same lateness again, and every
        // `RESYNC_GAP` after that.
        //
        // corner-cut: on a machine that cannot decode the file in real time at
        // all this settles into one restart per `RESYNC_GAP` -- in sync, and
        // stuttering, which is the honest picture of what that machine can do.
        // The upgrade path is dropping late frames *inside* the worker (skip
        // the convert and the send for anything already past due), which needs
        // the deadline shared with it.
        if !owed && session.is_playing() && resync_due(late, self.resynced, priming) {
            eprintln!("picture {late:.3}s behind the clock: restarting it there");
            session.resync_picture();
            self.held = None;
            self.resynced = Some(Instant::now());
        }

        if self.transport() == Transport::Ended {
            // A seek whose worker never produced a frame (vanished file) would
            // otherwise repaint at vsync forever. Held clear for as long as the
            // state does, not just on the crossing: nothing else is coming.
            self.seek_since = None;
            // A loop-trim window whose far edge *is* the film's last frame
            // reaches Ended the same tick it would have reached the window's
            // own edge, so the top-of-function restart above never gets the
            // chance -- `is_playing()` there is already false by the time
            // this branch runs. Folded into the same crossing the
            // whole-timeline loop already used, so that race has nothing
            // left to win: reaching the end while *either* loop is armed
            // restarts rather than halts, regardless of which one owns it.
            // A bare i/o range never arms this crossing on its own -- it is
            // an export mark first ([`active_loop_window`]) -- so `range`
            // only ever widens `armed` through the `loop_on`/`loop_trim`
            // terms already here, never past them.
            let armed = self.loop_on || self.loop_trim.is_some();
            if crosses_into_loop(was, Transport::Ended, armed) {
                // The same door the restart button and key use
                // ([`Player::toggle_or_restart`]), minus the `cx` that door
                // spends only on a repaint -- the pump is already inside one.
                // One seam of a one-frame seam: the restart is a fresh seek
                // and its reopen, not a gapless splice.
                let start =
                    loop_restart_frame(self.loop_on, self.range, self.loop_trim).unwrap_or(0);
                if let Some(session) = self.active_session_mut() {
                    session.seek(f64::from(start) / fps);
                }
                self.reset_after_reseek();
                if let Some(session) = self.active_session_mut() {
                    session.play();
                }
            } else if was != Transport::Ended {
                // Ended is a *stopped* transport, so the clock stops with it,
                // on the out point the timecode and the playhead have been
                // showing all along. Nothing else ever stopped it: past the
                // last frame wall time takes over and `now()` walks off the end
                // of the timeline for as long as the window is left open -- and
                // the playhead is what a cut, a paste, an insert and the
                // analyser all act at, so every one of them was aiming into
                // empty space (measured: a 5 s timeline recognised its end at
                // clock 17.5 s under a slow renderer). End of stream is left
                // set, so this is still `Ended` and the next press restarts.
                if let Some(session) = self.active_session_mut() {
                    session.halt_at_end();
                }
                let elapsed = self.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
                eprintln!(
                    "eof after {elapsed:.3}s wall: {} frames displayed, {} dropped, clock {:.3}s",
                    self.displayed,
                    self.dropped,
                    self.active_session().map_or(0., PlaybackSession::now)
                );
            }
        }
    }

    /// Where the transport is, asked of the session rather than remembered:
    /// end of stream is the engine's own flag (any seek clears it, which is why
    /// an edit past the end revives the picture) and so is the clock. A held
    /// frame is one still owed to the screen, so the end is not the end yet.
    pub(crate) fn transport(&self) -> Transport {
        let Some(session) = self.active_session() else {
            return Transport::Stopped;
        };
        transport(
            session.is_playing(),
            session.is_eos() && self.held.is_none(),
        )
    }

    /// A frame owed to the screen after a reseek, and the buffered one dropped:
    /// what stops the picture from staying frozen on the old last frame. The
    /// end-of-stream flag itself is the engine's and its own seek clears it --
    /// edits reseek inside the engine and still owe this.
    pub(crate) fn reset_after_reseek(&mut self) {
        self.held = None;
        // Restarted on every reseek, not only on the first: what it measures is
        // the open now standing, which is what a person is waiting on.
        self.seek_since = Some(Instant::now());
        // A seek is a person saying where to look, so it takes the view back
        // from an earlier scroll: the frame asked for is the one to be shown.
        self.panned = false;
        // An edit moves the indices a drag in flight is holding -- a stroke
        // during one is exactly that -- and an edge committed against a moved
        // index would trim a clip nobody grabbed. Dropping it is the whole fix:
        // nothing has been written yet.
        self.trim = None;
        // ...and the shadow a drag is drawn under promises a landing on a lane
        // this edit has just reshaped. The next move of the drag draws it
        // again; until then it says nothing.
        self.ghost.clear();
    }

    /// Jumps the timeline.
    pub(crate) fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.active_session_mut() else {
            return;
        };
        session.seek(t);
        self.reset_after_reseek();
        cx.notify();
    }

    /// The keyboard's seek: whole frames along the timeline, through the same
    /// door a ruler click uses -- so a step while playing keeps playing, exactly
    /// as a click does. It starts from the frame the transport is showing, which
    /// past the end is the last one, and that is what lets a step back off EOS
    /// revive the picture (the engine's seek leaves [`Transport::Ended`]). Both
    /// ends clamp, so the two go-to actions are this same step asked for more
    /// frames than the timeline has. Selection is untouched: a seek is not an
    /// edit, and nothing it does moves a clip index.
    pub(crate) fn step(&mut self, frames: i64, cx: &mut Context<Self>) {
        let ended = self.transport() == Transport::Ended;
        let fps = self.active_fps();
        let Some(session) = self.active_session() else {
            return;
        };
        let last = ((session.timeline_duration() * fps).round() as i64 - 1).max(0);
        let now = match ended {
            true => last,
            false => i64::from(frame_at(session.now(), fps)),
        };
        let target = now.saturating_add(frames).clamp(0, last);
        self.seek(target as f64 / fps, cx);
    }

    /// [`scrub_to`](Player::scrub_to) for the preview's own seek bar: the
    /// mapping is `preview_seek_seconds` rather than a `Scale`, because a
    /// preview bar spans the file's whole length and never zooms. Seeks
    /// through the ordinary [`Player::seek`], so it lands on
    /// `preview_session` the same way the keyboard step does
    /// ([`Player::active_session_mut`]) -- a preview's scrub never reaches
    /// the timeline underneath it.
    pub(crate) fn preview_scrub_to(&mut self, x: Pixels, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = self.preview_session.as_ref() else {
            return;
        };
        let t = preview_seek_seconds(x, self.preview_bar.get(), session.timeline_duration());
        let target = (t * self.active_fps()) as u32;
        if commit || scrub_due(target, self.last_target, self.last_scrub.elapsed()) {
            self.last_target = target;
            self.last_scrub = Instant::now();
            self.seek(t, cx);
        }
    }

    /// Seeks to where the pointer sits along the ruler. `commit` is the press
    /// and the release, which must land exactly even when the throttle below
    /// would have skipped them.
    pub(crate) fn scrub_to(&mut self, x: Pixels, commit: bool, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        // Clamped to the timeline here rather than in the mapping: there is bed
        // past the last frame now, and a seek out there is a seek to the end.
        let t = self
            .scale
            .time_at(px_along(x, self.ruler.get()))
            .clamp(0., session.timeline_duration());
        let target = (t * self.fps) as u32;
        if commit || scrub_due(target, self.last_target, self.last_scrub.elapsed()) {
            self.last_target = target;
            self.last_scrub = Instant::now();
            self.seek(t, cx);
        }
    }

    /// The play binding and the transport button share it: once the timeline is finished
    /// the only sensible "play" is from the top.
    /// Pushes the current volume at the session, which is the only place it is
    /// ever pushed: after a change here, and after a session arrives. A session
    /// starts at full volume, so a file opened while muted has to be told --
    /// that is the whole reason this is not just called from the key handler.
    /// Silent no-op with no timeline, or with a run that has no audio device.
    pub(crate) fn apply_volume(&self) {
        if let Some(session) = self.active_session() {
            session.set_gain(self.volume.gain());
        }
    }

    /// The mute key and the two volume keys, and the click on the button. The
    /// picture is not touched: silencing the output is not pausing it, so the
    /// clock -- which the device still drives -- runs straight through.
    pub(crate) fn set_volume(&mut self, change: impl FnOnce(&mut Volume), cx: &mut Context<Self>) {
        change(&mut self.volume);
        self.apply_volume();
        cx.notify();
    }

    /// Where the pointer sits along the slider, as a level. The press and every
    /// sample after it come here, so the sound follows the hand rather than the
    /// release -- there is nothing to undo about a monitoring level, which is
    /// why this writes live and keeps no gesture state beyond the flag.
    pub(crate) fn drag_volume(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let along = frac_along(x, self.volume_bar.get());
        self.set_volume(|volume| volume.set_along(along), cx);
    }

    pub(crate) fn toggle_or_restart(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // Nothing to play is a message, not a transport state. An empty
        // timeline is [`Transport::Ended`] from its one black frame onward, so
        // the restart below would start a clock against a zero-length timeline
        // -- and it is Ended again by the next repaint, so no later press could
        // ever stop it: the button would read "Pause" and never pause. A delete
        // can empty the timeline mid-play, and that press must still stop it.
        if nothing_to_play(self.active_session()) {
            match self.active_session_mut().filter(|s| s.is_playing()) {
                Some(session) => session.pause(),
                None => self.notify_user(NOTHING_TO_PLAY.into()),
            }
            cx.notify();
            return;
        }
        // Pressing play is asking to watch, so a view scrolled away while
        // paused comes back to the head with it -- as a seek's does.
        self.panned = false;
        match self.transport() {
            // Nothing open: the button is dimmed and the key says nothing.
            Transport::Stopped => {}
            // Back to the top and away, for the key and the button alike --
            // whichever asked, the transport was showing Play.
            state if state.restarts() => {
                self.seek(0., cx);
                if let Some(session) = self.active_session_mut() {
                    session.play();
                }
            }
            _ => {
                if let Some(session) = self.active_session_mut() {
                    session.toggle();
                    // A paused timeline animates nothing; this is the repaint
                    // that puts the new glyph up.
                    cx.notify();
                }
            }
        }
    }
}

/// Whether a late picture is restarted at the clock: the rule
/// [`should_resync`] always was, plus the prime it must not interrupt. A span
/// that has not handed over a picture yet is late by exactly its own reopen
/// -- a clip boundary on a silence-cut timeline most of all, where the clips
/// are shorter than that reopen -- and restarting it spends the reopen again
/// for the same lateness, every `RESYNC_GAP` after that. The restart is for a
/// span that has *delivered* and fallen behind afterwards.
pub(crate) fn resync_due(late: f64, last: Option<Instant>, priming: bool) -> bool {
    !priming && should_resync(late, last)
}

/// Whether *this* repaint is the one crossing into `Ended` with loop on --
/// the moment to restart rather than halt. `now` is always `Ended` at the one
/// call site (it is inside that guard already), but taking it as a parameter
/// keeps this checkable on its own instead of only through a live session.
pub(crate) fn crosses_into_loop(was: Transport, now: Transport, loop_on: bool) -> bool {
    now == Transport::Ended && was != Transport::Ended && loop_on
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prime gate: a span still owing its first picture is never
    /// restarted, however late the last frame to arrive was. The same span
    /// once it has delivered keeps the rule -- thresholds and cooldown
    /// unchanged -- because a decoder that is genuinely too slow is still
    /// behind on its second frame, one repaint later.
    #[test]
    fn a_priming_span_is_never_restarted() {
        assert!(!resync_due(0., None, true));
        assert!(!resync_due(LATE_RESYNC, None, true));
        assert!(!resync_due(9., None, true));
        // ...and the rule itself, exactly as it was, once the prime is over.
        assert!(!resync_due(0., None, false));
        assert!(!resync_due(LATE_RESYNC, None, false));
        assert!(resync_due(LATE_RESYNC + 0.1, None, false));
        // A decoder still behind right after a restart waits out the gap
        // instead of reopening at every repaint...
        assert!(!resync_due(9., Some(Instant::now()), false));
        // ...and fires again once it has passed.
        assert!(resync_due(
            9.,
            Some(Instant::now() - RESYNC_GAP - Duration::from_millis(1)),
            false
        ));
    }

    /// The crossing fires exactly once per end and only with the flag on --
    /// not on every repaint the transport happens to sit at `Ended` through,
    /// and not at all with loop off (which is the halt path this test would
    /// otherwise silently stop covering).
    #[test]
    fn the_loop_restarts_only_on_the_crossing_with_loop_on() {
        assert!(crosses_into_loop(
            Transport::Playing,
            Transport::Ended,
            true
        ));
        assert!(!crosses_into_loop(
            Transport::Playing,
            Transport::Ended,
            false
        ));
        assert!(!crosses_into_loop(Transport::Ended, Transport::Ended, true));
        assert!(!crosses_into_loop(
            Transport::Ended,
            Transport::Playing,
            true
        ));
    }

    /// `loop_restart_frame` picks the seek target the crossing above plays
    /// from: the i/o range's near edge when one is armed and set (it wins
    /// over both the trim window and the whole-timeline loop), the trim
    /// window's near edge next, the top of the timeline for the
    /// whole-timeline loop alone, and no restart at all with nothing armed.
    #[test]
    fn loop_restart_frame_prefers_the_trim_window_then_the_whole_timeline_then_nothing() {
        assert_eq!(loop_restart_frame(false, None, Some((30, 60))), Some(30));
        assert_eq!(
            loop_restart_frame(true, None, Some((30, 60))),
            Some(30),
            "the trim window wins over the whole-timeline loop"
        );
        assert_eq!(loop_restart_frame(true, None, None), Some(0));
        assert_eq!(loop_restart_frame(false, None, None), None);
        // The i/o range beats the trim window when both are set...
        assert_eq!(
            loop_restart_frame(true, Some((10, 20)), Some((30, 60))),
            Some(10),
            "the marked range wins over the subject-cut trim"
        );
        // ...and a range alone, with loop off and no trim, never arms a
        // restart at all -- it is an export mark first.
        assert_eq!(
            loop_restart_frame(false, Some((10, 20)), None),
            None,
            "a bare i/o range must not start looping un-looped playback"
        );
        // Clearing the range falls back to the trim window, then to the
        // whole timeline.
        assert_eq!(
            loop_restart_frame(true, None, Some((30, 60))),
            Some(30),
            "clearing the range falls back to the trim window"
        );
        assert_eq!(
            loop_restart_frame(true, None, None),
            Some(0),
            "clearing both the range and the trim falls back to the whole timeline"
        );
    }

    /// The bug this session fixed: a loop-trim window whose far edge (60)
    /// *is* the film's own last frame reaches `Ended` the same tick the
    /// window's own edge would have restarted it, so `is_playing()` --
    /// gating the top-of-`pump` restart -- can already be false by the time
    /// that check runs. The crossing below must fire from `loop_trim` alone,
    /// with `loop_on` off, and it must resume at the window's own start
    /// (30), not the top of the timeline: reaching the end while a loop is
    /// armed restarts, it never silently halts.
    #[test]
    fn a_loop_trim_window_ending_at_the_films_last_frame_restarts_on_ended() {
        let loop_on = false;
        let loop_trim = Some((30, 60));
        let armed = loop_on || loop_trim.is_some();
        assert!(armed, "loop_trim alone must arm the crossing");
        assert!(crosses_into_loop(
            Transport::Playing,
            Transport::Ended,
            armed
        ));
        assert_eq!(loop_restart_frame(loop_on, None, loop_trim), Some(30));
    }

    /// A loop window sitting entirely before the film's end never reaches
    /// `Ended` at all -- the top-of-`pump` restart (`should_loop_restart`,
    /// gated on `is_playing()`) fires first, on the ordinary tick the
    /// playhead crosses the window's far edge, exactly as
    /// `should_loop_restart_only_at_the_windows_far_edge`
    /// (`tests::editing`) already covers. This crossing helper only ever
    /// applies to the end-of-film case above.
    #[test]
    fn a_mid_timeline_loop_window_never_needs_the_ended_crossing() {
        use crate::timeline_math::should_loop_restart;
        // Well short of the film's own end: the ordinary restart already
        // fires at frame 60 without the transport ever reaching Ended.
        assert!(should_loop_restart(60, Some((30, 60))));
    }
}
