//! The timeline's re-timing map: one piecewise-affine answer to "this frame
//! is now where?", built by every operation that moves time.
//!
//! THE ARCHITECTURE LAW: a re-timing op NEVER touches lanes directly. It
//! (1) resolves its [`Members`](crate::project) once via `group_of`, (2)
//! builds a `TimelineMap` from each member's OWN geometry -- one piece per
//! clip or caption, that member's own old span onto its own new one -- (3)
//! applies that member's map to only its own lane, media clips AND
//! captions uniformly, and (4) the caller maps the playhead through the
//! SAME per-member map. The app never re-derives group or time semantics;
//! the engine's gates stay the single questions.
//!
//! The map itself is a sorted list of knots `(old frame, new frame)` with
//! affine interpolation between neighbours and identity outside the ends: a
//! compression is two knots with a shallower slope between them, a removal
//! is a knot pair that collapses a span onto a point, an insertion the same
//! run backwards, and a piece whose new span starts earlier than its old
//! one (a slow-down pulling a member's head earlier than its own old frame)
//! carries an extra knot that folds the run between the two los into that
//! same kind of collapse, so the piece never doubles back on the identity
//! run before it. Old frames strictly increase knot to knot; new frames
//! only need to not decrease -- a collapse ties two of them together on
//! purpose -- which is what keeps [`TimelineMap::invert`]'s walk a single
//! pass, never a step backward.

/// A piecewise-affine old→new frame map, identity outside its knots. Built
/// once per edit and read many times; `Clone` for the tests that pin its
/// laws, `Debug` for the messages those tests print when they fail.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineMap {
    /// Knot points, sorted by `old`, strictly increasing on `old` and
    /// non-decreasing on `new` -- a collapse ties two knots' `new` value
    /// together on purpose, the span it drops entering one new frame.
    knots: Vec<(u32, u32)>,
}

impl TimelineMap {
    /// The map that moves nothing: the base every piece is cut into.
    pub(crate) fn identity() -> Self {
        Self { knots: Vec::new() }
    }

    /// One non-identity piece -- `[old_lo, old_hi)` maps onto `[new_lo,
    /// new_hi)` -- over an identity base. The whole of a compression (a
    /// re-rate of a member); a removal is `new` collapsed onto its own lo
    /// (`new_lo == new_hi`, a literal point); an insertion is that run
    /// backwards; and a piece whose `new_lo` lands before `old_lo` (a
    /// slow-down pulling a member's head earlier than its own old frame)
    /// gets an extra anchor knot collapsing the run between the two los
    /// onto `new_lo`, so the piece stays non-decreasing instead of doubling
    /// back on the identity run before it. An empty old span (`old_lo >=
    /// old_hi`) comes out identity, because there is nothing in it to move;
    /// an inverted new span clamps its hi up to its lo, the collapse it means.
    pub(crate) fn piece(old: (u32, u32), new: (u32, u32)) -> Self {
        if old.0 >= old.1 {
            return Self::identity();
        }
        let new = (new.0, new.1.max(new.0));
        let mut knots = Vec::with_capacity(3);
        // The anchor: without it, frames below `old_lo` stay identity and
        // land ON the piece's own new_lo image the moment old_lo is
        // crossed -- a decreasing step, the non-monotone bug a shift-left
        // piece used to be. Folding the run into the same collapse a
        // removal already does keeps `apply` a single non-decreasing walk.
        if new.0 < old.0 {
            knots.push((new.0, new.0));
        }
        // Half-open boundary knots: interpolation runs between them and
        // the frame *at* the hi knot is the piece's own to give back
        // (the caller clamps inside), never a step past its new end.
        knots.push((old.0, new.0));
        knots.push((old.1, new.1));
        Self { knots }
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
        // Below the first knot the map is identity on the old axis *up to
        // the piece's old lo*, which on the new axis is the run from 0 to
        // the piece's new lo. A SHIFTED piece (new lo < old lo) puts part of
        // that run inside the piece's own destination, so the lerp through
        // the knot is right; an unshifted one has new lo == old lo and the
        // lerp degenerates to identity -- either way the single run covers
        // it, and a frame below min(old lo, new lo) is identity outright.
        if frame < self.knots[0].0.min(self.knots[0].1) {
            return Some(frame);
        }
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

    /// The laws every piece keeps, whatever shape cut it: `apply` walking
    /// old frames up never goes back down, and every new frame's own
    /// inverse answers back exactly through `apply` again -- the right
    /// inverse a many-to-one piece (a compression, a collapse, the run a
    /// shift folds into its own anchor) can still give, even where the
    /// left inverse can't (many old frames landing on one new frame means
    /// only one of them gets its own value back). Old frames strictly
    /// below `min(old_lo, new_lo)`, and at or past `old_hi`, sit outside
    /// the piece's own collapse -- pure identity or a pure shift, both
    /// exact bijections -- so those get the stronger left-inverse check.
    fn assert_piece_laws(old: (u32, u32), new: (u32, u32), span: u32) {
        let m = TimelineMap::piece(old, new);
        let mut last = 0;
        // The image `apply` actually produces over the span -- an
        // expansion skips new frames between old ones (no old frame answers
        // them), so the right-inverse law is only fair to ask of frames
        // `apply` really lands on, never the whole new axis blind.
        let mut image = std::collections::BTreeSet::new();
        for f in 0..span {
            let now = m.apply(f);
            assert!(now >= last, "{old:?}->{new:?}: frame {f} went back to {now} from {last}");
            image.insert(now);
            last = now;
        }
        for n in image {
            let back = m.invert(n).expect("the image is total");
            assert_eq!(m.apply(back), n, "{old:?}->{new:?}: new frame {n} round trips");
        }
        let lo = old.0.min(new.0);
        for f in (0..lo).chain(old.1 + 1..span) {
            assert_eq!(m.invert(m.apply(f)), Some(f), "{old:?}->{new:?}: frame {f} outside the piece round trips");
        }
    }

    /// Every piece shape the engine cuts, run through the laws above: an
    /// expansion, a compression, a shift-left (the shape a slow-down's
    /// caption overhang gives, and the one that used to go non-monotone),
    /// a shift-right, and a collapse to a literal point.
    #[test]
    fn a_piece_holds_its_laws_on_every_shape() {
        assert_piece_laws((100, 200), (100, 300), 500);
        assert_piece_laws((100, 200), (100, 150), 500);
        assert_piece_laws((30, 60), (15, 30), 200);
        assert_piece_laws((30, 60), (45, 90), 200);
        assert_piece_laws((100, 200), (100, 100), 500);
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
