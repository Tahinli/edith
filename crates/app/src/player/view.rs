//! What of the timeline is on the bed and what of it is picked: the zoom,
//! the scroll and the selection.

use crate::*;

impl Player {
    /// The one place a clip becomes *the* selected one: a plain click, a
    /// right-click that opens the menu, and every selection key go through
    /// here, so what a keyboard marks and what a pointer marks are the same
    /// state marked the same way (group and all -- see [`marked`]). One pick,
    /// because one click names one thing; [`Player::pick`] is the ctrl+click
    /// that grows the selection instead.
    pub(crate) fn select(&mut self, target: (Lane, usize), cx: &mut Context<Self>) {
        self.selected.set_one(target);
        cx.notify();
    }

    /// A press with ctrl held: the pick joins the selection, or leaves it if
    /// it was already in -- the toggle that assembles a group by hand. Without
    /// ctrl this is exactly [`Player::select`], which is why every box on the
    /// bed asks this one and lets the modifier decide.
    pub(crate) fn pick(&mut self, target: (Lane, usize), ctrl: bool, cx: &mut Context<Self>) {
        match ctrl {
            true => self.selected.toggle(target),
            false => self.selected.set_one(target),
        }
        cx.notify();
    }

    /// Every clip the playhead is over, one per lane, in the order the lanes are
    /// drawn -- video first, which is the order [`PlaybackSession::lanes`] comes
    /// in. What the select key walks.
    pub(crate) fn under_playhead(&self) -> Vec<(Lane, usize)> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let now = session.now();
        session
            .lanes()
            .into_iter()
            .filter_map(|lane| Some((lane, session.lane_clip_at(lane, now)?)))
            .collect()
    }
    /// Selects the clip under the playhead, and on a repeat press the next
    /// lane's -- so one key reaches every clip the playhead is over, which is
    /// what makes selection (and everything that acts on a selection: delete,
    /// lift, the equalizer, the grade) reachable with no pointer at all.
    pub(crate) fn select_under_playhead(&mut self, cx: &mut Context<Self>) {
        let under = self.under_playhead();
        let Some(&first) = under.first() else {
            self.notify_user("NOTHING UNDER THE PLAYHEAD — move it onto a clip first".into());
            cx.notify();
            return;
        };
        // Where the current selection sits in that walk decides what "again"
        // means; a selection off the playhead starts the walk over.
        let next = self
            .selected
            .anchor()
            .and_then(|sel| under.iter().position(|&clip| clip == sel))
            .map_or(first, |at| under[(at + 1) % under.len()]);
        self.select(next, cx);
    }

    /// Walks the selection along its own lane, wrapping at either end. Nothing
    /// selected means nothing to walk from, so it selects under the playhead
    /// exactly as the select key does: either key can start as well as continue.
    pub(crate) fn select_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let clips = self
            .selected
            .anchor()
            .zip(self.session.as_ref())
            .map_or(0, |((lane, _), session)| session.lane_clips(lane).len());
        match (self.selected.anchor(), clips) {
            // An empty lane is a selection nothing can be stepped from -- as is
            // no selection at all, and the playhead answers both.
            (Some((lane, idx)), len) if len > 0 => {
                let next = if forward {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                };
                self.select((lane, next), cx);
            }
            _ => self.select_under_playhead(cx),
        }
    }

    /// Every placement on every lane, in the order the lanes are drawn: what
    /// Select All marks -- the clips and the captions both, one selection that
    /// a Group can take apart lane by lane and a Delete can walk pick by pick.
    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let mut selected = Selection::new();
        for lane in session.lanes() {
            let len = match lane.kind {
                LaneKind::Subtitle => session.sub_lane(lane).len(),
                _ => session.lane_clips(lane).len(),
            };
            for idx in 0..len {
                selected.add((lane, idx));
            }
        }
        self.selected = selected;
        if self.selected.is_empty() {
            // A key that does nothing looks broken: an empty timeline has
            // nothing to select, and that is a thing to say rather than skip.
            self.notify_user("NOTHING TO SELECT — the timeline is empty".into());
        }
        cx.notify();
    }

    /// Cycles the fit policy of the clip the picture is coming from -- the
    /// clicked one when it is a video clip, else the composite's own, exactly as
    /// the colour card picks its target. A whole card for one four-valued
    /// setting would be a card to close; a stroke that cycles it and says what
    /// it landed on is the same setting with nothing to dismiss.
    ///
    /// Only means anything when the clip is not the project's size -- a clip
    /// that already fills the canvas looks the same under all four -- so the
    /// notice says the size it is placing, not just the word.
    pub(crate) fn cycle_fit(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = &mut self.session else {
            self.notify_user("no timeline to fit — open a file first".into());
            cx.notify();
            return;
        };
        // A caption the hand is on names the clip it was pinned to, and the
        // policy is that clip's; one in no group has no picture to fit and it
        // says so, rather than fitting the playhead's clip behind the hand's
        // back.
        let target = match self.selected.anchor() {
            // A caption the hand is on names the clip it was pinned to, and the
            // policy is that clip's; one in no group has no picture to fit and
            // it says so, rather than fitting the playhead's clip behind the
            // hand's back.
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                match caption_media_half(session, (lane, idx), LaneKind::Video) {
                    Some(half) => Some(half),
                    None => {
                        self.notify_user(
                            "NOTHING TO FIT — a caption has no picture; group it with a clip \
                             first (ctrl-click both, then Group)"
                                .into(),
                        );
                        cx.notify();
                        return;
                    }
                }
            }
            // A sound half has no picture of its own: the playhead's clip, as
            // it always was.
            Some((lane, _)) if lane.kind == LaneKind::Audio => session.video_clip_at(session.now()),
            other => other.or_else(|| session.video_clip_at(session.now())),
        };
        let Some((lane, idx)) = target else {
            self.notify_user("no clip under the playhead to fit".into());
            cx.notify();
            return;
        };
        let next = next_fit(session.fit_of(lane, idx));
        self.apply_fit(lane, idx, next, cx);
    }

    /// One clip's fit policy set, whichever asked: the stroke that steps to the
    /// next one and the list that names one outright come through here, so they
    /// cannot differ in what they do or in what they say they did.
    pub(crate) fn apply_fit(&mut self, lane: Lane, idx: usize, fit: FitPolicy, cx: &mut Context<Self>) {
        if let Some(session) = &mut self.session
            && session.set_fit(lane, idx, fit)
        {
            let (w, h) = session.resolution();
            self.notify_user(format!("FIT POLICY: {} on {w}x{h}", fit_label(fit)).into());
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// The scale against the bed it is drawn on and the timeline it is drawn
    /// from: every clamp, zoom and scroll is worked out through this, and the
    /// bed is measured off the ruler's probe rather than remembered, so a
    /// resized window is a resized view on the very next answer.
    pub(crate) fn view(&self) -> View {
        View {
            scale: self.scale,
            bed: f32::from(self.ruler.get().size.width),
            duration: self.drawn_duration(),
            fps: self.fps,
        }
    }

    /// Magnifies the timeline about a point that stays put: `anchor` is how many
    /// pixels along the bed to hold still (a ctrl+wheel holds the pointer), and
    /// with none it is the playhead -- so the frame being worked on is still the
    /// frame on screen after the zoom. Clamped at both ends by [`View`]: out
    /// stops at the whole timeline on the bed, in at a handful of frames.
    pub(crate) fn zoom(&mut self, factor: f32, anchor: Option<f32>, cx: &mut Context<Self>) {
        let view = self.view();
        let at = self.playhead(view.duration);
        let anchor = anchor.unwrap_or_else(|| self.scale.px_at(at).clamp(0., view.bed));
        self.scale = view.zoomed(factor, anchor);
        // The view a hand chose. A zoom about the playhead leaves it on the
        // bed, so this is given back on the very next frame and only a zoom
        // that took the head off screen -- ctrl+wheel away from it -- holds.
        self.panned = true;
        cx.notify();
    }

    /// All the way back out: the whole timeline across the bed, and the one
    /// thing that reads the timeline's own length to decide how wide a second
    /// is drawn.
    pub(crate) fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        self.scale = self.view().fit();
        cx.notify();
    }

    /// Slides the view along the timeline by `notches` of the wheel, later in
    /// time for a positive one and [`SCROLL_NOTCH_SHARE`] of the bed each. The
    /// scale is untouched: this is the timeline's scrollbar, and the only thing
    /// on the panel that moves what is on screen without magnifying it.
    pub(crate) fn scroll_view(&mut self, notches: f32, cx: &mut Context<Self>) {
        let view = self.view();
        // Nothing painted yet: there is no bed to measure a notch against, and
        // a start moved against a zero width would be a jump to the head.
        if view.bed <= 0. {
            return;
        }
        self.scale = view.scrolled(notches * view.bed * SCROLL_NOTCH_SHARE);
        // The one gesture whose whole purpose is to look away from the
        // playhead: while playing it wins over the follow, which is what every
        // editor does with a scroll during playback.
        self.panned = true;
        cx.notify();
    }

    /// A press on the scrollbar's track: on the thumb, the drag begins from
    /// wherever in it the hand landed; beside it, the view jumps so the
    /// thumb's middle is at the press and the drag carries on from there --
    /// a click is a jump, and holding it is a jump that keeps going.
    pub(crate) fn scroll_press(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let view = self.view();
        if view.bed <= 0. || view.duration <= 0. {
            return;
        }
        let (thumb_x, thumb_w) =
            scroll_thumb(view.bed, view.duration, view.scale.start.max(0.), view.span());
        let at = px_along(x, self.scroll_track.get());
        let grab = match (thumb_x..thumb_x + thumb_w).contains(&at) {
            true => at - thumb_x,
            // Beside the thumb: jump so its middle is under the pointer. The
            // thumb is clamped inside the track by [`scroll_thumb`], so the
            // grab it leaves is always a real place inside it.
            false => {
                let view = View {
                    scale: Scale {
                        start: (view.scale.start.max(0.)
                            + f64::from(at - thumb_x - thumb_w / 2.) / view.scale.pps)
                            .max(0.),
                        ..view.scale
                    },
                    ..view
                };
                self.scale = view.settled();
                thumb_w / 2.
            }
        };
        self.scroll_drag = Some(grab);
        // The wheel's own claim on the follow: a hand on the scrollbar is
        // looking away from the playhead on purpose.
        self.panned = true;
        cx.notify();
    }

    /// A sample of a thumb drag: the window starts where the thumb's grabbed
    /// point sits under the pointer, clamped by the same `settled` every other
    /// scroll answers to. No reseek, no worker -- the view is the only thing
    /// moving.
    pub(crate) fn scroll_drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(grab) = self.scroll_drag else {
            return;
        };
        let view = self.view();
        let at = px_along(x, self.scroll_track.get());
        // The thumb's grabbed point under the pointer names the start, in the
        // track's own proportion. `settled` owns the ends -- and on a timeline
        // so long the thumb sits at its floor width, the last stretch of track
        // is the clamp's to hold, exactly as every scrollbar's is.
        let start = f64::from((at - grab).max(0.)) / f64::from(view.bed.max(1.)) * view.duration;
        self.scale = View {
            scale: Scale { start, ..view.scale },
            ..view
        }
        .settled();
        cx.notify();
    }

    /// One notch of the wheel anywhere over the timeline -- the ruler or a
    /// lane's bed alike, since a hand aims at the clip it is working on and not
    /// at the strip above it. Ctrl zooms about the pointer, bare scrolls the
    /// view along: the mapping Premiere, Movavi and CapCut share, and the one
    /// the user named.
    ///
    /// The anchor is measured off the ruler's probe wherever the pointer is,
    /// because that probe *is* the bed's x-to-time mapping ([`HEADER_W`]) and
    /// every lane is drawn through the same one.
    pub(crate) fn timeline_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = wheel_delta(event);
        if d == 0. {
            return;
        }
        let factor = match d > 0. {
            true => ZOOM_STEP,
            false => 1. / ZOOM_STEP,
        };
        match event.modifiers.control {
            true => {
                let anchor = px_along(event.position.x, self.ruler.get());
                self.zoom(factor, Some(anchor), cx);
            }
            // Up is back towards the head of the timeline, the way a wheel up
            // is back towards the top of a page.
            false => self.scroll_view(-d.signum(), cx),
        }
    }
}
