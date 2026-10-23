//! What of the timeline is on the bed and what of it is picked: the zoom,
//! the scroll and the selection.

use crate::*;

/// The share of the bed a fit spreads the whole edit across: the rest is the
/// margin on the right, so the last frame never sits against the bed's edge
/// (DESIGN §7, about 2%).
const FIT_SHARE: f32 = 0.98;

/// The margin Zoom to selection leaves either side of the selected span, as a share of its length.
const SELECTION_MARGIN: f64 = 0.05;

impl Player {
    /// The one place a clip becomes *the* selected one: a plain click, a
    /// right-click that opens the menu, and every selection key go through
    /// here, so what a keyboard marks and what a pointer marks are the same
    /// state marked the same way (group and all -- see [`marked`]). One pick,
    /// because one click names one thing; [`Player::pick`] is the ctrl+click
    /// that grows the selection instead.
    pub(crate) fn select(&mut self, target: (Lane, usize), cx: &mut Context<Self>) {
        // A locked lane refuses selection (DESIGN §7): the item stays where it
        // is and the toast offers the unlock.
        if self.lane_locked_refusal(target.0) {
            return;
        }
        self.drop_title_selection(cx);
        self.drop_sel_cue();
        self.selected.set_one(target);
        self.gap = None;
        cx.notify();
    }

    /// A press with ctrl held: the pick joins the selection, or leaves it if
    /// it was already in -- the toggle that assembles a group by hand.
    /// Without ctrl, a press on a member of an existing multi-pick keeps the
    /// whole set rather than collapsing it -- collapsing here would make a
    /// set-drag unreachable, since the drag begins on this very press. A
    /// plain press anywhere else is exactly [`Player::select`]. Collapsing to
    /// the one pressed clip belongs to the click that follows, once it is
    /// plain a press and not the start of a drag (see the boxes' own
    /// `on_click`), which is why every box on the bed asks this one for the
    /// press and its own click listener for the collapse.
    pub(crate) fn pick(&mut self, target: (Lane, usize), ctrl: bool, cx: &mut Context<Self>) {
        if self.lane_locked_refusal(target.0) {
            return;
        }
        self.drop_title_selection(cx);
        self.drop_sel_cue();
        match ctrl {
            true => self.selected.toggle(target),
            false if self.selected.contains(target) => {}
            false => self.selected.set_one(target),
        }
        self.gap = None;
        // A picked envelope key belongs to the clip it sits on; another clip takes it away.
        if self.gain_pick.is_some_and(|(l, i, _)| (l, i) != target) {
            self.gain_pick = None;
        }
        cx.notify();
    }

    /// Empties every kind of pick the timeline holds at once: clips, a picked gap, a title and a
    /// caption. Escape does this, so the one thing left picked is always the one named last.
    pub(crate) fn clear_picks(&mut self, cx: &mut Context<Self>) {
        self.selected = Selection::new();
        self.gap = None;
        self.drop_title_selection(cx);
        self.drop_sel_cue();
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
            self.notify_user("Nothing under the playhead".into());
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
            // A locked lane's items are not selectable, so Select All leaves
            // them out rather than marking what the next edit would refuse.
            if session.lane_locked(lane) {
                continue;
            }
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
            self.notify_user("Nothing to select: the timeline is empty".into());
        }
        cx.notify();
    }

    /// Opens the list of fit policies for the clip the picture is coming from -- the
    /// clicked one when it is a video clip, else the composite's own, exactly as
    /// the colour card picks its target -- at `at`, the one in force ticked.
    ///
    /// Only means anything when the clip is not the project's size -- a clip
    /// that already fills the canvas looks the same under all four -- so the
    /// notice after a pick says the size it is placing, not just the word.
    pub(crate) fn open_fit_list(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match self.fit_target() {
            Ok((lane, idx)) => self.open_picker(Pick::Fit(lane, idx), at, cx),
            Err(why) => {
                self.notify_user(why.into());
                cx.notify();
            }
        }
    }

    /// The clip the fit list is about, or why there is none.
    pub(crate) fn fit_target(&self) -> Result<(Lane, usize), &'static str> {
        let session = self.session.as_ref().ok_or("Nothing to fit: no file open")?;
        // A caption the hand is on names the clip it was pinned to, and the
        // policy is that clip's; one in no group has no picture to fit and it
        // says so, rather than fitting the playhead's clip behind the hand's
        // back.
        let target = match self.selected.anchor() {
            Some((lane, idx)) if lane.kind == LaneKind::Subtitle => {
                Some(caption_media_half(session, (lane, idx), LaneKind::Video).ok_or("Nothing to fit: a caption has no picture")?)
            }
            // A sound half has no picture of its own: the playhead's clip, as
            // it always was.
            Some((lane, _)) if lane.kind == LaneKind::Audio => session.video_clip_at(session.now()),
            other => other.or_else(|| session.video_clip_at(session.now())),
        };
        target.ok_or("Nothing to fit: no clip under the playhead")
    }

    /// One clip's fit policy set, whichever asked: the stroke that steps to the
    /// next one and the list that names one outright come through here, so they
    /// cannot differ in what they do or in what they say they did.
    pub(crate) fn apply_fit(
        &mut self,
        lane: Lane,
        idx: usize,
        fit: FitPolicy,
        cx: &mut Context<Self>,
    ) {
        // The policy has to have moved before this is an edit: `set_fit`
        // answers "the index existed", not "the value changed" (the engine
        // snapshot-and-rewrites the same policy and still says true), and the
        // list keeps the row already in force clickable -- so the engine's
        // own bool would light the ledger's `unsaved` ghost over a project
        // nobody touched.
        if let Some(session) = &mut self.session
            && session.fit_of(lane, idx) != fit
            && session.set_fit(lane, idx, fit)
        {
            let (w, h) = session.resolution();
            // A clip's own fit policy, written from a click: arms autosave
            // and the close gate itself ([`Player::act`]'s list is the action
            // doors only).
            self.mark_dirty();
            self.notify_user(format!("Fit to frame: {} on {w}x{h}", fit_label(fit)).into());
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
        // A hand's zoom ends a fit that was following the bed: from here the
        // scale is the hand's, and a resized window keeps it.
        self.fitted_bed = None;
        cx.notify();
    }

    /// All the way back out: the whole timeline across the bed, and the one
    /// thing that reads the timeline's own length to decide how wide a second
    /// is drawn.
    pub(crate) fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        // An unpainted bed has no width to fit into: the fit is asked for again
        // on the first frame that has one ([`Render`]) rather than taken now
        // against the default scale, which is the 40 px/s a first import used
        // to land at.
        match self.view().bed > 0. {
            true => {
                self.fit_to_bed();
                self.fit_pending = false;
            }
            false => self.fit_pending = true,
        }
        cx.notify();
    }

    /// View > Zoom to selection: the view fitted to the span the selected clips cover, with a
    /// margin of [`SELECTION_MARGIN`] of that span either side. The oracle refuses it with
    /// nothing selected, so the empty case here only leaves the view as it is.
    pub(crate) fn zoom_to_selection(&mut self, cx: &mut Context<Self>) {
        let Some((from, to)) = self.selection_span() else {
            cx.notify();
            return;
        };
        let view = self.view();
        if view.bed <= 0. {
            cx.notify();
            return;
        }
        let margin = (to - from) * SELECTION_MARGIN;
        let from = (from - margin).max(0.);
        let len = (to + margin - from).max(1. / self.fps);
        let pps = f64::from(view.bed) / len;
        self.scale = View {
            scale: Scale { pps, start: from },
            ..view
        }
        .settled();
        // A hand's fit, the same bookkeeping a zoom keeps: the scale is the view's own now.
        self.panned = true;
        self.fitted_bed = None;
        cx.notify();
    }

    /// The seconds the selected placements cover, first start to last end, or `None` with no
    /// placement picked.
    fn selection_span(&self) -> Option<(f64, f64)> {
        let session = self.session.as_ref()?;
        let fps = self.fps;
        let (picks, _) = self.marks();
        let frames = picks.iter().filter_map(|&(lane, idx)| match lane.kind {
            LaneKind::Subtitle => session.sub_lane(lane).get(idx).map(|s| (s.start, s.start + s.frames)),
            _ => session.lane_clips(lane).get(idx).map(|c| (c.start, c.end())),
        });
        let (start, end) = frames.fold(None, |acc: Option<(u32, u32)>, (a, b)| match acc {
            None => Some((a, b)),
            Some((x, y)) => Some((x.min(a), y.max(b))),
        })?;
        Some((f64::from(start) / fps, f64::from(end) / fps))
    }

    /// The whole timeline across the bed, with the margin ([`FIT_SHARE`]), and
    /// the bed width it was worked out for. That width is what keeps the fit
    /// sticky: while it still matches the bed, nothing refits, and a window the
    /// compositor resizes -- the first frame measures the initial size -- is
    /// refitted by [`Render`] the next frame. Only a bed that has been measured
    /// is fitted, so a zero width is never stored.
    pub(crate) fn fit_to_bed(&mut self) {
        let view = self.view();
        self.scale = View {
            bed: view.bed * FIT_SHARE,
            ..view
        }
        .fit();
        self.fitted_bed = Some(view.bed);
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
        // A scroll is a hand's view too: it ends the sticky fit.
        self.fitted_bed = None;
        cx.notify();
    }

    /// A sample of a thumb drag: the window starts where the thumb's grabbed
    /// point sits under the pointer, clamped by the same `settled` every other
    /// scroll answers to. No reseek, no worker -- the view is the only thing
    /// moving.
    pub(crate) fn scroll_drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let at = px_along(x, self.scroll_track.get());
        self.pan_window_to(at, cx);
    }

    /// The window follows a drag held by `scroll_drag`: `at` is how far along
    /// the bed the pointer is, in px. Shared by the scrollbar's thumb and the
    /// overview's window box (`ui/bench_stance/overview.rs`), so both pan the
    /// same way. A drag is a hand's view, so it holds against the follow and
    /// ends the sticky fit.
    pub(crate) fn pan_window_to(&mut self, at: f32, cx: &mut Context<Self>) {
        let Some(grab) = self.scroll_drag else {
            return;
        };
        self.scale = panned_view(self.view(), at, grab);
        self.panned = true;
        self.fitted_bed = None;
        cx.notify();
    }

    /// The lane stack's own scroll, as one number: how far down the column
    /// has been taken, clamped to what it can show. The names and the beds
    /// are one column (`id("lanes")`), so one offset moves them together and
    /// this is the whole of the stack's scrolling -- the wheel over the names
    /// and the thumb on the strip both land here.
    fn lanes_taken(&self, to: f32) -> f32 {
        let max = f32::from(self.lanes_scroll.max_offset().height);
        to.clamp(0., max)
    }

    /// A sample of the lane thumb's drag: the stack is taken to where the
    /// thumb's grabbed point sits under the pointer, clamped by the same
    /// `max_offset` the column's own wheel answers to. No reseek, no repaint
    /// of anything outside the column -- the view is not moving.
    pub(crate) fn lanes_drag_to(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let Some(grab) = self.lanes_drag else {
            return;
        };
        let track = f32::from(self.lanes_scroll.bounds().size.height);
        let max = f32::from(self.lanes_scroll.max_offset().height);
        let (_, thumb_h) = lanes_thumb(track, track + max, track, 0.);
        let at = px_down(y, self.lanes_scroll.bounds());
        let to = self.lanes_taken((at - grab).max(0.) / (track - thumb_h).max(1.) * max);
        self.lanes_scroll
            .set_offset(point(self.lanes_scroll.offset().x, px(-to)));
        cx.notify();
    }

    /// One notch of the wheel anywhere over the timeline -- the ruler or a
    /// lane's bed alike, since a hand aims at the clip it is working on and not
    /// at the strip above it. Ctrl zooms about the pointer, Shift (or a sideways
    /// swipe) scrolls the view along, and a bare wheel scrolls the lanes when
    /// more lie below -- else it runs the view along: the NLE convention.
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
        let (dx, dy) = match event.delta {
            ScrollDelta::Lines(l) => (l.x, l.y),
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        };
        if event.modifiers.control {
            // Ctrl zooms about the pointer, the NLE convention.
            let anchor = px_along(event.position.x, self.ruler.get());
            self.zoom(factor, Some(anchor), cx);
        } else if event.modifiers.shift || dx.abs() > dy.abs() {
            // Shift, or a sideways swipe on a trackpad, runs the view along the film. Up or
            // left is back towards the head, the way a page reads.
            self.scroll_view(-d.signum(), cx);
        } else if f32::from(self.lanes_scroll.max_offset().height) > 0. {
            // A plain wheel scrolls the stack of lanes when there are more than fit.
            let by = match event.delta {
                ScrollDelta::Lines(l) => l.y * 40.,
                ScrollDelta::Pixels(p) => f32::from(p.y),
            };
            let to = self.lanes_taken(-f32::from(self.lanes_scroll.offset().y) - by);
            self.lanes_scroll.set_offset(point(self.lanes_scroll.offset().x, px(-to)));
            cx.notify();
        } else {
            // Nothing below to scroll to: the wheel runs the film, as it did before.
            self.scroll_view(-d.signum(), cx);
        }
    }
}

/// The scale a window drag asks for: the start moves so the point `grab` px
/// into the window sits under `at` (both px along the bed). `settled` owns
/// the ends -- a window dragged past either one stops at it, and so does a
/// thumb on a timeline so long it sits at its floor width.
pub(crate) fn panned_view(view: View, at: f32, grab: f32) -> Scale {
    let start = f64::from((at - grab).max(0.)) / f64::from(view.bed.max(1.)) * view.duration;
    View {
        scale: Scale {
            start,
            ..view.scale
        },
        ..view
    }
    .settled()
}
