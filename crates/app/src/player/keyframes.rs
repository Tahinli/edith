//! Keyframes on the property cards: the stopwatch diamond, the easing chip, and
//! the key-aware read and write the sliders go through once a clip is keyed.

use crate::*;
use engine::project::KeyKind;
use engine::transform::KeyEase;

impl Player {
    /// The timeline frame the playhead sits on: where a key "at the playhead" goes.
    pub(crate) fn key_frame(&self) -> u32 {
        let fps = self.active_fps();
        self.session
            .as_ref()
            .map_or(0, |session| crate::subs::frame_at(session.now(), fps))
    }

    /// The clip the open card edits, for keys of `kind`: the transform card owns
    /// placement and opacity, the colour card the grade.
    pub(crate) fn key_clip(&self, kind: KeyKind) -> Option<(Lane, usize)> {
        match kind {
            KeyKind::Placement | KeyKind::Opacity => self.transform_open,
            KeyKind::Color => self.color_open,
        }
    }

    /// Whether the clip of the `kind` card is keyed at all.
    pub(crate) fn key_active(&self, kind: KeyKind) -> bool {
        self.key_clip(kind)
            .zip(self.session.as_ref())
            .is_some_and(|((lane, idx), session)| session.is_keyed(lane, idx, kind))
    }

    /// The ease of the key on the playhead, `None` with no key there.
    pub(crate) fn key_here(&self, kind: KeyKind) -> Option<KeyEase> {
        let (lane, idx) = self.key_clip(kind)?;
        self.session
            .as_ref()?
            .key_ease_at(lane, idx, kind, self.key_frame())
    }

    /// The stopwatch: a key on the playhead holding what the clip shows there, or
    /// the key that is there taken off.
    pub(crate) fn toggle_key_here(&mut self, kind: KeyKind, cx: &mut Context<Self>) {
        // The stroke is the card's own: with the card shut it opens it first,
        // on the selected clip.
        if self.key_clip(kind).is_none() {
            match kind {
                KeyKind::Color => self.open_color(cx),
                _ => self.open_transform(cx),
            }
        }
        let frame = self.key_frame();
        let Some((lane, idx)) = self.key_clip(kind) else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        self.pending_transform = None;
        self.pending_color = None;
        match session.toggle_key(lane, idx, kind, frame) {
            Some(_) => {
                self.mark_dirty();
                self.reset_after_reseek();
            }
            None => self.notify_user("The playhead is not on this clip".into()),
        }
        cx.notify();
    }

    /// Which key the easing list is about: with the transform card open that is the
    /// placement key, or the opacity key when only that one is there.
    pub(crate) fn ease_kind(&self) -> KeyKind {
        if self.color_open.is_some() {
            KeyKind::Color
        } else if self.key_here(KeyKind::Placement).is_none() && self.key_here(KeyKind::Opacity).is_some() {
            KeyKind::Opacity
        } else {
            KeyKind::Placement
        }
    }

    /// Opens the list of easings for the key on the playhead, at `at`: Linear, Ease in,
    /// Ease out, Ease in-out, Hold, the one in force ticked. Nothing steps on its own.
    pub(crate) fn open_key_easing(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let kind = self.ease_kind();
        if self.key_here(kind).is_none() {
            self.notify_user("No key on the playhead — add one with the diamond".into());
            cx.notify();
            return;
        }
        self.open_picker(Pick::KeyEase(kind), at, cx);
    }

    /// Sets the key on the playhead to `ease`.
    pub(crate) fn set_key_ease_here(&mut self, kind: KeyKind, ease: KeyEase, cx: &mut Context<Self>) {
        let frame = self.key_frame();
        if let Some((lane, idx)) = self.key_clip(kind)
            && let Some(session) = &mut self.session
            && session.set_key_ease_at(lane, idx, kind, frame, ease)
        {
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Sets the opacity of the transform card's clip: a key when opacity is
    /// keyed, the placement's own value otherwise.
    pub(crate) fn set_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        let mut params = self.transform_params();
        params.opacity = opacity.clamp(0., 1.);
        self.set_transform(params, cx);
    }
}

/// The clip-relative offset (`0..frames`) whose source frame is `at`, found by
/// bisection over `pos`, the clip's offset-to-source map. `pos` moves one way
/// only, so the first offset that reaches `at` is the key's own. `None` when no
/// offset lands within a frame of `at` -- a key on footage the clip no longer
/// shows (trimmed away) has no place on the clip to mark.
pub(crate) fn offset_of_source(frames: u32, at: u32, pos: impl Fn(u32) -> Option<u32>) -> Option<u32> {
    let last = frames.checked_sub(1)?;
    let rising = pos(last)? >= pos(0)?;
    let (mut lo, mut hi) = (0, last);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let here = pos(mid)?;
        let reached = if rising { here >= at } else { here <= at };
        if reached {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    (pos(lo)?.abs_diff(at) <= 1).then_some(lo)
}

/// The indices of `xs` (positions, ascending) that keep a mark: one closer than
/// `min_gap` to the mark kept before it is dropped, so a run of keys a few
/// frames apart draws one diamond.
pub(crate) fn merge_mark_px(xs: &[f32], min_gap: f32) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::with_capacity(xs.len());
    for (n, &x) in xs.iter().enumerate() {
        if kept.last().is_none_or(|&last| x - xs[last] >= min_gap) {
            kept.push(n);
        }
    }
    kept
}

impl Player {
    /// Timeline frames of every key on the clip at (`lane`, `idx`), sorted and
    /// unique: the placement, opacity, colour and volume keys (kept on the
    /// footage, so mapped back onto the clip) and the speed ramp's keys (kept
    /// clip-relative already). Keys the clip no longer shows are left out.
    pub(crate) fn clip_key_frames(&self, lane: Lane, idx: usize) -> Vec<u32> {
        let Some(session) = self.session.as_ref() else {
            return Vec::new();
        };
        let Some(clip) = session.lane_clips(lane).get(idx) else {
            return Vec::new();
        };
        let frames = clip.frames();
        // Constant speed: the same arithmetic the engine plays the clip by.
        // With a ramp the engine's own source map is the only true one.
        let ramped = !clip.speed_keys.is_empty();
        let pos = |off: u32| -> Option<u32> {
            if ramped {
                return session.key_source_frame(lane, idx, clip.start + off);
            }
            let speed = clip.speed.as_f64();
            let src = match clip.reversed {
                true => f64::from(clip.out_frame.saturating_sub(1)) - f64::from(off) * speed,
                false => f64::from(clip.in_frame) + f64::from(off) * speed,
            };
            Some(src.round().max(0.) as u32)
        };
        let source_keys = clip
            .transform_keys
            .iter()
            .map(|k| k.at)
            .chain(clip.opacity_keys.iter().map(|k| k.at))
            .chain(clip.color_keys.iter().map(|k| k.at))
            .chain(clip.gain_keys.iter().map(|k| k.at))
            .chain(clip.effects.iter().flat_map(|e| e.keys.iter().map(|k| k.at)));
        let mut offsets: Vec<u32> = source_keys
            .filter_map(|at| offset_of_source(frames, at, pos))
            .collect();
        offsets.extend(clip.speed_keys.iter().map(|k| k.at).filter(|&off| off < frames));
        offsets.sort_unstable();
        offsets.dedup();
        offsets.into_iter().map(|off| clip.start + off).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant-speed map from offset to source frame, 2x speed from source 100.
    fn linear(off: u32) -> Option<u32> {
        Some(100 + off * 2)
    }

    #[test]
    fn a_key_is_found_at_its_own_offset() {
        assert_eq!(offset_of_source(50, 110, linear), Some(5));
        assert_eq!(offset_of_source(50, 100, linear), Some(0));
        // No offset lands on source 111: the first one past it (112) is within a frame, so it counts.
        assert_eq!(offset_of_source(50, 111, linear), Some(6));
    }

    #[test]
    fn a_key_off_the_clip_is_not_marked() {
        assert_eq!(offset_of_source(50, 50, linear), None, "before the clip's footage");
        assert_eq!(offset_of_source(50, 300, linear), None, "past the clip's footage");
        assert_eq!(offset_of_source(0, 100, linear), None, "an empty clip");
    }

    #[test]
    fn a_reversed_map_is_found_too() {
        let back = |off: u32| Some(200 - off * 2);
        assert_eq!(offset_of_source(50, 190, back), Some(5));
    }

    #[test]
    fn close_marks_merge_and_far_ones_stay() {
        assert_eq!(merge_mark_px(&[10., 12., 13., 20.], 4.), vec![0, 3]);
        assert_eq!(merge_mark_px(&[10., 14., 18.], 4.), vec![0, 1, 2]);
        assert_eq!(merge_mark_px(&[10., 13.9, 14.], 4.), vec![0, 2], "merged against the kept mark, not the last one");
        assert_eq!(merge_mark_px(&[], 4.), Vec::<usize>::new());
    }
}
