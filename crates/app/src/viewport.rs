//! How much timeline is on the bed, and how a time is written.

use crate::*;

/// What the zoom button says it is showing: how much timeline fits on the bed,
/// in the coarsest unit that still tells two zooms apart.
pub(crate) fn span_label(span: f64) -> String {
    match span {
        s if !s.is_finite() || s <= 0. => "—".to_string(),
        // Hours once a timeline is long enough to be measured in them: the far
        // stop follows the content now, so "315m" is a span a user can be sat
        // at, and no one reads a pill in minutes past sixty.
        s if s >= 3600. => format!("{:.1}h", s / 3600.),
        s if s >= 600. => format!("{:.0}m", s / 60.),
        s if s >= 60. => format!("{:.1}m", s / 60.),
        s if s >= 10. => format!("{s:.0}s"),
        s => secs_label(s),
    }
}

/// A span of seconds as a person reads it: one decimal above a second, two
/// below -- [`scaled`]'s rule, for its reason. The tightest zoom is
/// [`ZOOM_MIN_FRAMES`] across the bed, which on the 240 fps slow-motion a phone
/// writes is 0.03s, and a single frame of quiet at 60 fps is 0.02s: one decimal
/// prints both as "0.0s", a pill and a notice saying the thing they are about
/// has no length at all.
pub(crate) fn secs_label(secs: f64) -> String {
    match secs >= 1. {
        true => format!("{secs:.1}s"),
        false => format!("{secs:.2}s"),
    }
}

/// The mapping the whole panel is drawn and clicked through: `pps` pixels to a
/// second of timeline, `start` the moment at the bed's left edge. Absolute --
/// how wide a clip is drawn depends on how long the clip is and on nothing
/// else, so adding a second clip does not redraw the first one narrower and
/// zooming out always makes every box smaller.
///
/// The one place seconds become pixels: every box, the playhead, a seek and a
/// trim all go through it, so none of them can drift away from the others when
/// the view moves. What clamps it -- the stops, the scroll, the fit -- needs the
/// bed it is drawn on and lives on [`View`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Scale {
    pub(crate) pps: f64,
    pub(crate) start: f64,
}

impl Default for Scale {
    fn default() -> Self {
        Scale {
            pps: PPS_DEFAULT,
            start: 0.,
        }
    }
}

impl Scale {
    /// Where a moment sits on the bed, in pixels from its left edge. Negative
    /// for a moment scrolled off to the left, which is exactly the offset a
    /// half-visible clip is drawn at.
    pub(crate) fn px_at(self, at: f64) -> f32 {
        ((at - self.start) * self.pps) as f32
    }

    /// How wide a stretch of `len` seconds is drawn. Wider than the bed once
    /// zoomed in, which is the point of zooming in; never negative, which gpui
    /// has no meaning for.
    pub(crate) fn width_px(self, len: f64) -> f32 {
        (len * self.pps).max(0.) as f32
    }

    /// The moment `x` pixels along the bed is pointing at: the inverse of
    /// [`Scale::px_at`], and what every seek and every trim reads. Clamped at
    /// the head of the timeline only -- there is bed past the last frame now,
    /// and a tail dragged into it is a longer clip, not an error.
    pub(crate) fn time_at(self, x: f32) -> f64 {
        if self.pps > 0. {
            (self.start + f64::from(x) / self.pps).max(0.)
        } else {
            self.start
        }
    }

    /// [`SNAP_PX`] in timeline frames at the scale the bed is drawn at: a snap
    /// is a distance on screen, so zoomed right in it is worth less than a frame
    /// (no snap at all, which is what a hand placing single frames wants) and
    /// zoomed out it is worth many.
    pub(crate) fn snap_frames(self, fps: f64) -> u32 {
        if self.pps > 0. {
            (SNAP_PX / self.pps * fps) as u32
        } else {
            0
        }
    }
}

/// A [`Scale`] against the bed it is drawn on and the timeline it is drawn
/// from. The bed's width is what turns a scale into "how much is on screen",
/// and that is all the stops, the scroll clamp and the fit are made of.
///
/// Built per use out of [`Player::view`] and thrown away again -- the state is
/// the `Scale`, and this is what a bed of `bed` px showing `duration` seconds
/// at `fps` may do to it. So no call site can measure a moment against a bed or
/// a duration that another one did not.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    pub(crate) scale: Scale,
    pub(crate) bed: f32,
    pub(crate) duration: f64,
    pub(crate) fps: f64,
}

impl View {
    /// How much of the timeline is on the bed, in seconds.
    pub(crate) fn span(self) -> f64 {
        if self.scale.pps > 0. {
            f64::from(self.bed) / self.scale.pps
        } else {
            0.
        }
    }

    /// The two stops, in pixels per second: [`ZOOM_MIN_FRAMES`] across the bed
    /// is as tight as it goes, and as wide as it goes is the whole timeline on
    /// the bed with [`ZOOM_OUT_MARGIN`] to spare -- the far stop is *relative*,
    /// so the end of a five hour timeline is reachable for the same reason the
    /// end of a five minute one is. A fixed far stop could not say that: a
    /// timeline longer than it had an end no zoom could reach, which is the bug
    /// this is. Short of [`PPS_MIN`] the timeline's own length is not worth
    /// widening to and the pixel takes over, so a short import can still be
    /// zoomed out of and the resting scale is nobody's content.
    ///
    /// `None` on a bed that was never painted -- there is nothing to measure
    /// them against yet, and a stop guessed off a zero width would throw away a
    /// zoom the user asked for.
    pub(crate) fn stops(self) -> Option<(f64, f64)> {
        (self.bed > 0.).then(|| {
            let bed = f64::from(self.bed);
            let whole = match self.duration > 0. {
                true => bed / (self.duration * ZOOM_OUT_MARGIN),
                false => PPS_MIN,
            };
            let min = whole.min(PPS_MIN);
            (min, (bed * self.fps / ZOOM_MIN_FRAMES).max(min))
        })
    }

    /// Clamped to the bed it draws on: between the two stops, and never
    /// scrolled past either end of the timeline. Unlike the fractional view
    /// this replaced, the *resting* scale is not the content -- a five second
    /// timeline zooms out as far as an hour long one does, and both are drawn
    /// at [`PPS_DEFAULT`] until someone zooms. Only the far stop knows the
    /// length, and only once the length is worth more than [`PPS_MIN`].
    pub(crate) fn settled(self) -> Scale {
        let pps = match self.scale.pps.is_finite() && self.scale.pps > 0. {
            true => self.scale.pps,
            false => PPS_DEFAULT,
        };
        let start = match self.scale.start.is_finite() {
            true => self.scale.start,
            false => 0.,
        };
        let Some((min, max)) = self.stops() else {
            return Scale {
                pps,
                start: start.max(0.),
            };
        };
        let pps = pps.clamp(min, max);
        let span = f64::from(self.bed) / pps;
        Scale {
            pps,
            start: start.clamp(0., (self.duration - span).max(0.)),
        }
    }

    /// Zoomed by `factor` about `anchor` (pixels along the bed): whatever moment
    /// was under that point stays under it, so a zoom magnifies what was being
    /// looked at rather than throwing it off the edge.
    pub(crate) fn zoomed(self, factor: f32, anchor: f32) -> Scale {
        let at = self.scale.time_at(anchor);
        // Clamped *before* the offset is worked out, not after: a press that
        // runs into either stop must still leave the anchor where it is, and a
        // start measured against a scale the stop then took away would slide it.
        let raw = self.scale.pps * f64::from(factor);
        let pps = match self.stops() {
            Some((min, max)) => raw.clamp(min, max),
            None => raw,
        };
        View {
            scale: Scale {
                pps,
                start: at - f64::from(anchor) / pps,
            },
            ..self
        }
        .settled()
    }

    /// Slid along by `by` pixels, later in the timeline for a positive one: the
    /// one move that changes what is on the bed without changing how wide a
    /// second is drawn, which is what a bare wheel does. Clamped by
    /// [`View::settled`] like every other move, so a run at either end stops at
    /// the end rather than scrolling the timeline off the bed -- and the extent
    /// it stops against is the content's own length, never a number.
    pub(crate) fn scrolled(self, by: f32) -> Scale {
        let pps = self.settled().pps;
        View {
            scale: Scale {
                pps,
                start: self.scale.start + f64::from(by) / pps,
            },
            ..self
        }
        .settled()
    }

    /// The whole timeline across the bed. The one place the content's own
    /// length sets the scale, and the only one -- everywhere else a second is a
    /// second -- because this is a user pressing a key that asks for exactly
    /// that.
    pub(crate) fn fit(self) -> Scale {
        let pps = match self.duration > 0. && self.bed > 0. {
            true => f64::from(self.bed) / self.duration,
            false => PPS_DEFAULT,
        };
        View {
            scale: Scale { pps, start: 0. },
            ..self
        }
        .settled()
    }

    /// The scale a playhead at `at` needs: the same one while it is on the bed,
    /// and one centred on it once it has run off -- which is how a zoomed-in
    /// timeline scrolls, during playback and after a seek alike. With the whole
    /// timeline on the bed this can never fire, so a panel showing all of it is
    /// untouched by it.
    pub(crate) fn following(self, at: f64) -> Scale {
        // Nothing is drawn yet, so nothing has run off anything.
        if self.bed <= 0. {
            return self.scale;
        }
        let scale = self.settled();
        if self.shows(at) {
            return scale;
        }
        let span = f64::from(self.bed) / scale.pps;
        View {
            scale: Scale {
                start: at - span / 2.,
                ..scale
            },
            ..self
        }
        .settled()
    }

    /// Whether the moment `at` is on the bed as it is drawn now. The one
    /// question both halves of the follow ask -- [`View::following`] to decide
    /// whether to chase a head that has run off, and the render to decide when
    /// a hand's own scroll ([`Player::panned`]) has been caught up with and the
    /// follow may have the view back -- so the two can never disagree about
    /// where the edge of the bed is.
    pub(crate) fn shows(self, at: f64) -> bool {
        if self.bed <= 0. {
            return false;
        }
        let scale = self.settled();
        at >= scale.start && at <= scale.start + f64::from(self.bed) / scale.pps
    }
}

/// NLE timecode: `HH:MM:SS:FF`, the frame counted inside its own second.
pub(crate) fn timecode(t: f64, fps: f64) -> String {
    let t = t.max(0.);
    let secs = t as u64;
    let last = (fps.ceil() as u64).saturating_sub(1);
    let frame = ((t - secs as f64) * fps) as u64;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        frame.min(last)
    )
}

/// NLE timecode for a *duration* given as a frame count: the same
/// `HH:MM:SS:FF` shape as [`timecode`], but built from the count directly
/// rather than a float number of seconds. `timecode` truncates `(t - secs) *
/// fps`, which loses a whole frame to float error on a duration derived from
/// dividing a frame count by `fps` first (346 frames at 30 fps printed
/// `00:00:11:15`, one short of the true `:16`); integer frame math has no
/// such rounding to lose.
pub(crate) fn frames_timecode(frames: u32, fps: f64) -> String {
    let per_sec = (fps.ceil() as u64).max(1);
    let frames = u64::from(frames);
    let secs = frames / per_sec;
    let frame = frames % per_sec;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        frame
    )
}

/// Wall clock for a progress line: `M:SS`, minutes past the hour included
/// rather than an hours field nobody reads on an export.
pub(crate) fn clock(secs: f32) -> String {
    let secs = secs.max(0.) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}
