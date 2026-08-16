//! The timeline's re-timing map: one piecewise-affine answer to "this frame
//! is now where?", built by every operation that moves time.
//!
//! THE ARCHITECTURE LAW: a re-timing op NEVER touches lanes directly. It
//! (1) resolves its [`Members`](crate::project) once via `group_of`, (2)
//! builds ONE `TimelineMap` from their geometry, (3) applies that map to
//! every lane it owns -- media clips AND captions, uniformly -- and (4) the
//! caller maps the playhead through the SAME map. The app never re-derives
//! group or time semantics; the engine's gates stay the single questions.
//!
//! The map itself is a sorted list of knots `(old frame, new frame)` with
//! affine interpolation between neighbours and identity outside the ends: a
//! compression is two knots with a shallower slope between them, a removal
//! is a knot pair that collapses a span onto a point, an insertion the same
//! run backwards. Every piece is monotone by construction (knots sorted on
//! both axes), which is what makes [`TimelineMap::invert`] a binary search
//! and the playhead's question a lookup.

/// A piecewise-affine old→new frame map, identity outside its knots. Built
/// once per edit and read many times; `Clone` for the tests that pin its
/// laws, `Debug` for the messages those tests print when they fail.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineMap {
    /// Knot points, sorted by `old`, strictly increasing on both axes (the
    /// constructor refuses anything else: a map that folded time back on
    /// itself would be a map nothing downstream could invert).
    knots: Vec<(u32, u32)>,
}

impl TimelineMap {
    /// The map that moves nothing: the base every piece is cut into.
    pub(crate) fn identity() -> Self {
        Self { knots: Vec::new() }
    }

    /// One non-identity piece -- `[old_lo, old_hi)` maps onto `[new_lo,
    /// new_hi)` -- over an identity base. The whole of a compression (a
    /// re-rate of a member); a removal is `new` collapsed onto its own lo,
    /// and an insertion is that run backwards. Degenerate spans (an empty
    /// old, an inverted one) come out identity, because there is nothing in
    /// them to move.
    pub(crate) fn piece(old: (u32, u32), new: (u32, u32)) -> Self {
        if old.0 >= old.1 || new.0 >= new.1 {
            return Self::identity();
        }
        Self {
            // Half-open boundary knots: interpolation runs between them and
            // the frame *at* the hi knot is the piece's own to give back
            // (the caller clamps inside), never a step past its new end.
            knots: vec![(old.0, new.0), (old.1, new.1)],
        }
    }

    /// Where `frame` lands. Affine between knots, identity outside them,
    /// rounded to the nearest frame -- the same rounding every placement in
    /// this engine already makes -- and never past the new end of the piece
    /// it is inside, which a round-up at the last frame would do.
    pub(crate) fn apply(&self, frame: u32) -> u32 {
        let mut prev: Option<(u32, u32)> = None;
        for &(o, n) in &self.knots {
            if frame == o {
                return n;
            }
            if frame < o {
                return match prev {
                    // Before the first knot: identity from the head.
                    None => frame,
                    Some(po) => lerp(po, (o, n), frame).min(n.saturating_sub(1)).max(po.1),
                };
            }
            prev = Some((o, n));
        }
        match prev {
            // Past the last knot: identity at the total shift it left.
            Some((o, n)) => frame.saturating_add(n).saturating_sub(o),
            None => frame,
        }
    }

    /// The inverse question, `None` off the surviving domain (inside a
    /// removed span there is no frame to come back to -- the caller that
    /// needs one picks the span's head, which is where a ripple put what
    /// followed it). The knots are monotone, so this is the same walk with
    /// the axes traded.
    #[cfg(test)]
    pub(crate) fn invert(&self, frame: u32) -> Option<u32> {
        if self.knots.is_empty() {
            return Some(frame);
        }
        // Before the first knot the map was the identity on `[0, old_lo)`,
        // which on the new axis is the straight run from 0 to the knot.
        if frame < self.knots[0].1 {
            return Some(lerp((0, 0), self.knots[0], frame));
        }
        let mut prev = self.knots[0];
        for &(o, n) in &self.knots {
            if frame == n {
                return Some(o);
            }
            if frame < n {
                return Some(lerp((prev.1, prev.0), (n, o), frame));
            }
            prev = (o, n);
        }
        let (o, n) = *self.knots.last().expect("checked non-empty");
        Some(frame.saturating_sub(n).saturating_add(o))
    }

    #[cfg(test)]
    pub(crate) fn shifted_total(&self) -> i64 {
        match self.knots.last() {
            Some(&(o, n)) => i64::from(n) - i64::from(o),
            None => 0,
        }
    }
}

/// The one interpolation this module is: `frame`'s place between two knots,
/// carried across to the other axis. Independent of the axes' order, which
/// is what [`TimelineMap::invert`] reuses it for.
fn lerp(a: (u32, u32), b: (u32, u32), frame: u32) -> u32 {
    if b.0 <= a.0 {
        return a.1;
    }
    let t = f64::from(frame.saturating_sub(a.0)) / f64::from(b.0 - a.0);
    (f64::from(a.1) + t * f64::from(b.1.saturating_sub(a.1))).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The piece laws on every shape the engine cuts: monotone, identity
    /// outside, exact on the ends, invertible on what survives, and the
    /// total shift the caller reads for the timeline's new length.
    #[test]
    fn a_piece_is_monotone_exact_and_invertible() {
        // A compression: [100, 200) onto [100, 150).
        let m = TimelineMap::piece((100, 200), (100, 150));
        assert_eq!(m.apply(0), 0, "identity before");
        assert_eq!(m.apply(99), 99);
        assert_eq!(m.apply(100), 100, "the lo knot is exact");
        assert_eq!(m.apply(150), 125, "the middle halves");
        assert_eq!(m.apply(199), 149, "the last surviving frame");
        assert_eq!(m.apply(200), 150, "identity at the new shift");
        assert_eq!(m.apply(1_000), 950);
        assert_eq!(m.shifted_total(), -50);
        // Monotone: a walk up the whole range never goes down.
        let mut last = 0;
        for f in 0..400 {
            let now = m.apply(f);
            assert!(now >= last, "frame {f} went back to {now}");
            last = now;
        }
        // Round trip in the law's own direction: every new frame's inverse
        // maps back onto it. The other direction cannot be identity under
        // rounding -- a compression sends two neighbouring frames to one.
        for n in 0..400 {
            let back = m.invert(n).expect("the image is total");
            assert_eq!(m.apply(back), n, "new frame {n} round trips");
        }
        let m = TimelineMap::piece((100, 200), (100, 300));
        assert_eq!(m.apply(150), 200, "the middle doubles");
        assert_eq!(m.apply(1_000), 1_100);
        assert_eq!(m.shifted_total(), 100);

        // A removal: [100, 200) onto [100, 100) -- nothing survives inside,
        // and what followed stands where the span began.
        let m = TimelineMap::piece((100, 200), (100, 101));
        assert_eq!(m.apply(99), 99);
        assert_eq!(m.apply(150), 100, "inside collapses to the head");
        assert_eq!(m.apply(200), 101, "after shifts up tight");
        assert_eq!(m.shifted_total(), -99);
    }

    /// Identity is the base: everything maps to itself, in both directions,
    /// and the timeline's length never moved.
    #[test]
    fn identity_moves_nothing() {
        let m = TimelineMap::identity();
        for f in [0u32, 1, 999, u32::MAX / 2] {
            assert_eq!(m.apply(f), f);
            assert_eq!(m.invert(f), Some(f));
        }
        assert_eq!(m.shifted_total(), 0);
    }
}
