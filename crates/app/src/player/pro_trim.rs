//! Roll, slip and slide: the three trims that move *where* an edit falls
//! without changing how long the timeline is. The pointer makes them with a
//! shift-drag (an edge for a roll, the clip for a slip, shift+alt on the clip
//! for a slide) and the keyboard with frame detents ([`Player::pro_nudge`]);
//! both reach the engine through the same [`Player::pro_apply`], one undo step
//! each.

use crate::*;

impl Player {
    /// The clip a roll at `idx` is made on -- the one *before* the cut -- when
    /// the subject clip is `idx`: the cut after it if one abuts, else the cut
    /// before it.
    fn roll_anchor(&self, lane: Lane, idx: usize) -> Option<usize> {
        let session = self.session.as_ref()?;
        match (session.roll_room(lane, idx), idx.checked_sub(1)) {
            (Some(_), _) => Some(idx),
            (None, Some(before)) => session.roll_room(lane, before).map(|_| before),
            _ => None,
        }
    }

    /// Makes one pro trim of `by` timeline frames on the clip at `idx`. A
    /// roll's `idx` is the clip before its cut. Says why in the notices when the
    /// engine refuses, and returns whether anything changed -- so a drag
    /// let go where it began, or pressed against a media wall, takes no undo
    /// step and leaves the project looking unedited.
    pub(crate) fn pro_apply(&mut self, kind: ProKind, lane: Lane, idx: usize, by: i64) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let changed = match kind {
            ProKind::Roll => {
                let Some(cut) = session.lane_clips(lane).get(idx).map(|c| c.end()) else {
                    return false;
                };
                let to = (i64::from(cut) + by).max(0) as u32;
                match session.roll_clip(lane, idx, to) {
                    Some(at) => at != cut,
                    None => {
                        self.notify_user("Not rolled — there is no cut here".into());
                        return false;
                    }
                }
            }
            ProKind::Slide => {
                let Some(start) = session.lane_clips(lane).get(idx).map(|c| c.start) else {
                    return false;
                };
                let to = (i64::from(start) + by).max(0) as u32;
                match session.slide_clip(lane, idx, to) {
                    Some(at) => at != start,
                    None => {
                        self.notify_user("Not slid — it needs a clip on each side".into());
                        return false;
                    }
                }
            }
            ProKind::Slip => {
                let Some(source) = session.slip_source_delta(lane, idx, by) else {
                    self.notify_user("Not slipped — a still has no more footage".into());
                    return false;
                };
                match session.slip_clip(lane, idx, source) {
                    Some(moved) => moved != 0,
                    None => {
                        self.notify_user("Not slipped — a still has no more footage".into());
                        return false;
                    }
                }
            }
        };
        if changed {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        changed
    }

    /// The keys: one frame of roll, slip or slide on the subject clip.
    pub(crate) fn pro_nudge(&mut self, kind: ProKind, dir: i64, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let at = match kind {
            ProKind::Roll => self.roll_anchor(lane, idx),
            _ => Some(idx),
        };
        let Some(at) = at else {
            self.notify_user("Not rolled — there is no cut here".into());
            cx.notify();
            return;
        };
        self.pro_apply(kind, lane, at, dir);
        cx.notify();
    }

    /// A shift-press on a clip: starts the drag. `edge` is `Some` on an edge
    /// strip (a roll of the cut at that edge) and `None` on the body (a slip,
    /// or a slide with alt). The lock is asked first, like every edit door.
    pub(crate) fn start_pro(
        &mut self,
        lane: Lane,
        idx: usize,
        kind: ProKind,
        edge: Option<Edge>,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.covers_room()
            || self.exporting().is_some()
            || lane.kind == LaneKind::Subtitle
            || self.lane_locked_refusal(lane)
        {
            return;
        }
        // A roll names the clip before its cut: the strip's own clip at its
        // out edge, the one ahead of it at its in edge.
        let at = match (kind, edge) {
            (ProKind::Roll, Some(Edge::Start)) => match idx.checked_sub(1) {
                Some(before) => before,
                None => return,
            },
            _ => idx,
        };
        if kind == ProKind::Roll
            && self
                .session
                .as_ref()
                .is_none_or(|session| session.roll_room(lane, at).is_none())
        {
            self.notify_user("Not rolled — there is no cut here".into());
            cx.notify();
            return;
        }
        self.pick((lane, idx), false, cx);
        self.pro_drag = Some(ProDrag {
            lane,
            idx: at,
            kind,
            from: self.frame_under(x),
            by: 0,
        });
        cx.notify();
    }

    /// Follows the pointer: the frames moved, held inside the room a roll or a
    /// slide has (a slip is clamped by the engine at the release).
    pub(crate) fn pro_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.pro_drag else {
            return;
        };
        let by = i64::from(self.frame_under(x)) - i64::from(drag.from);
        let by = self.session.as_ref().map_or(0, |session| {
            let clip = session.lane_clips(drag.lane).get(drag.idx);
            match (drag.kind, clip) {
                (ProKind::Roll, Some(c)) => session
                    .roll_room(drag.lane, drag.idx)
                    .map_or(0, |(lo, hi)| i64::from(pro_target(c.end(), by, lo, hi)) - i64::from(c.end())),
                (ProKind::Slide, Some(c)) => session
                    .slide_room(drag.lane, drag.idx)
                    .map_or(0, |(lo, hi)| i64::from(pro_target(c.start, by, lo, hi)) - i64::from(c.start)),
                _ => by,
            }
        });
        if by != drag.by {
            self.pro_drag = Some(ProDrag { by, ..drag });
            cx.notify();
        }
    }

    /// The release: the whole drag reaches the engine as one edit.
    pub(crate) fn commit_pro(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.pro_drag.take() else {
            return;
        };
        self.hover_hint = None;
        if drag.by != 0 {
            self.pro_apply(drag.kind, drag.lane, drag.idx, drag.by);
        }
        cx.notify();
    }
}
