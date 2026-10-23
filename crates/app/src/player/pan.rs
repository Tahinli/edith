//! Pan: the scrub fields of the Mix section -- one for the selected audio clip, one on each lane's
//! strip. A drag follows the pointer and shows live; the engine is written once, on release, as one
//! undo step (a pan rides in the clip's envelope, so each write rebuilds the sound).

use crate::*;

/// What a pan scrub turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanTarget {
    Clip(Lane, usize),
    Lane(Lane),
}

/// A pan being scrubbed: where the press was, the pan it had, and the pan now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanScrub {
    pub(crate) target: PanTarget,
    pub(crate) x0: f32,
    pub(crate) v0: f32,
    pub(crate) cur: f32,
    /// Pan per pixel of drag: the track's width sweeps the range.
    pub(crate) per_px: f32,
}

/// The pan a drag of `dx` px from `v0` lands on, at `per_px` pan per pixel (a slider's track
/// sweeps the whole range): on the 1% grid, within `-1..=1`, with the centre caught within 3%.
pub(crate) fn scrub_pan_at(v0: f32, dx: f32, per_px: f32) -> f32 {
    let v = ((v0 + dx * per_px).clamp(-1., 1.) * 100.).round() / 100.;
    if v.abs() < 0.03 { 0. } else { v }
}

/// A pan as the field writes it: `Centre` for the middle, else the percent and the side
/// (`40% L`, `25% R`).
pub(crate) fn pan_text(pan: f32) -> String {
    let pct = (pan.abs() * 100.).round() as u32;
    match (pct, pan < 0.) {
        (0, _) => "Centre".into(),
        (n, true) => format!("{n}% L"),
        (n, false) => format!("{n}% R"),
    }
}

impl Player {
    /// The pan the target has in the engine.
    pub(crate) fn pan_of(&self, target: PanTarget) -> f32 {
        let Some(s) = self.session.as_ref() else {
            return 0.;
        };
        match target {
            PanTarget::Clip(lane, idx) => s.clip_pan_of(lane, idx).unwrap_or(0.),
            PanTarget::Lane(lane) => s.lane_pan(lane),
        }
    }

    /// The pan to show: the scrub's while one is turning this target, else the engine's.
    pub(crate) fn pan_shown(&self, target: PanTarget) -> f32 {
        match self.pan_scrub {
            Some(s) if s.target == target => s.cur,
            _ => self.pan_of(target),
        }
    }

    /// A press on a pan slider's track, `along` it (0..1) and `width` px wide: the pan jumps
    /// under the pointer and the drag that follows carries it, the thumb keeping to the hand.
    pub(crate) fn begin_pan_press(&mut self, target: PanTarget, x: f32, along: f32, width: f32) {
        let v0 = self.pan_of(target);
        let per_px = 2. / width.max(1.);
        let dx = (along.clamp(0., 1.) * 2. - 1. - v0) / per_px;
        let cur = scrub_pan_at(v0, dx, per_px);
        self.pan_scrub = Some(PanScrub { target, x0: x - dx, v0, cur, per_px });
    }

    /// The pointer moved with a pan scrub in flight.
    pub(crate) fn pan_scrub_move(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(s) = self.pan_scrub else {
            return;
        };
        let cur = scrub_pan_at(s.v0, x - s.x0, s.per_px);
        if cur != s.cur {
            self.pan_scrub = Some(PanScrub { cur, ..s });
            cx.notify();
        }
    }

    /// The button came up: the scrubbed pan is written as one undo step. `true` if a scrub ended.
    pub(crate) fn pan_scrub_end(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = self.pan_scrub.take() else {
            return false;
        };
        if s.cur != s.v0 {
            self.write_pan(s.target, s.cur);
        }
        cx.notify();
        true
    }

    /// Puts a pan back in the middle (a double-click on its field).
    pub(crate) fn reset_pan(&mut self, target: PanTarget, cx: &mut Context<Self>) {
        self.write_pan(target, 0.);
        cx.notify();
    }

    fn write_pan(&mut self, target: PanTarget, pan: f32) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        match target {
            PanTarget::Clip(lane, idx) => s.set_clip_pan(lane, idx, pan),
            PanTarget::Lane(lane) => s.set_lane_pan(lane, pan),
        };
    }
}
