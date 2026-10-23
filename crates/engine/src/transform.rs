//! Per-clip position/scale/rotate/crop, applied engine-side on the placed
//! picture so that playback and export land the same pixel from the same
//! numbers -- [`crate::color::ColorParams`]'s twin in every respect but what
//! it moves: a grade changes what a sample *is*, this changes where it *goes*.
//!
//! Plain data with no invariants of its own: out-of-range or non-finite
//! values are clamped where the geometry is built ([`crate::scale::crop_rect`],
//! [`crate::scale::transformed_dst_rect`]), so a deserializer can hand this
//! over unchecked, exactly as [`ColorParams`](crate::color::ColorParams) can.

/// One clip's placement on the project canvas, on top of whatever
/// [`crate::scale::FitPolicy`] already does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformParams {
    /// Canvas-fraction offset from centred, positive right. `0.0` is
    /// identity.
    pub pos_x: f32,
    /// Canvas-fraction offset from centred, positive down. `0.0` is identity.
    pub pos_y: f32,
    /// Multiplier on the fit-policy placement size. `1.0` is identity.
    pub scale: f32,
    /// Degrees, clockwise. Rendered at the nearest 90-degree step
    /// ([`crate::scale::rotate_i420_90s`]); the value itself is kept exactly,
    /// so a UI that later grows continuous rotation loses nothing already
    /// saved. `0.0` is identity.
    pub rotate: f32,
    /// Fraction of the source cropped off the left, `0.0..0.5`.
    pub crop_l: f32,
    /// Fraction of the source cropped off the right, `0.0..0.5`.
    pub crop_r: f32,
    /// Fraction of the source cropped off the top, `0.0..0.5`.
    pub crop_t: f32,
    /// Fraction of the source cropped off the bottom, `0.0..0.5`.
    pub crop_b: f32,
    /// How much of the placed picture shows over what is beneath it, `0.0..=1.0`:
    /// `1.0` is opaque and the identity. A layer is blended over the layers
    /// below by this fraction of its own coverage; a single clip is faded toward
    /// the black canvas it sits on. The blend is on the stored Y'CbCr code values,
    /// exactly as a dissolve blends them: gamma-encoded video code values, not
    /// linear light. So 50% white over black is the midpoint of the two codes
    /// (limited range: luma 16 and 235 give 126), which the eye reads as mid grey.
    pub opacity: f32,
}

impl Default for TransformParams {
    fn default() -> Self {
        Self {
            pos_x: 0.0,
            pos_y: 0.0,
            scale: 1.0,
            rotate: 0.0,
            crop_l: 0.0,
            crop_r: 0.0,
            crop_t: 0.0,
            crop_b: 0.0,
            opacity: 1.0,
        }
    }
}

impl TransformParams {
    /// Whether this is the do-nothing setting. Compared exactly, for
    /// [`crate::color::ColorParams::is_identity`]'s reason: only a value that
    /// is precisely identity is allowed to skip the geometry.
    pub fn is_identity(&self) -> bool {
        self.pos_x == 0.0
            && self.pos_y == 0.0
            && self.scale == 1.0
            && self.rotate == 0.0
            && self.crop_l == 0.0
            && self.crop_r == 0.0
            && self.crop_t == 0.0
            && self.crop_b == 0.0
            && self.opacity == 1.0
    }
}

/// The most keys a clip's transform animation may hold
/// ([`crate::project::Clip::transform_keys`]).
pub const MAX_TRANSFORM_KEYS: usize = 256;

/// How a key eases into the segment after it: `Linear` moves at a constant
/// rate, `Ease` (ease in-out) starts and stops gently (smoothstep), `EaseIn`
/// starts slowly and ends at speed (cubic), `EaseOut` starts at speed and
/// lands gently (cubic), and `Hold` keeps the key's value until the next key
/// and then jumps (a step). The last key has no segment after it, so its own
/// ease is never read. One shape for every key type ([`ease_shape`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyEase {
    #[default]
    Linear,
    Ease,
    EaseIn,
    EaseOut,
    Hold,
}

impl KeyEase {
    /// Every ease, in the order a picker lists them.
    pub const ALL: [KeyEase; 5] = [
        KeyEase::Linear,
        KeyEase::EaseIn,
        KeyEase::EaseOut,
        KeyEase::Ease,
        KeyEase::Hold,
    ];

    /// The name the tools and the GUI use: `linear`, `ease_in`, `ease_out`,
    /// `ease_in_out` or `hold`.
    pub fn name(self) -> &'static str {
        match self {
            KeyEase::Linear => "linear",
            KeyEase::EaseIn => "ease_in",
            KeyEase::EaseOut => "ease_out",
            KeyEase::Ease => "ease_in_out",
            KeyEase::Hold => "hold",
        }
    }

    /// [`name`](Self::name) read back. `ease` is the older spelling of `ease_in_out`.
    pub fn from_name(name: &str) -> Option<KeyEase> {
        Some(match name {
            "linear" => KeyEase::Linear,
            "ease_in" => KeyEase::EaseIn,
            "ease_out" => KeyEase::EaseOut,
            "ease_in_out" | "ease" => KeyEase::Ease,
            "hold" => KeyEase::Hold,
            _ => return None,
        })
    }

    /// The token a project file writes (`lin`, `in`, `out`, `ease`, `hold`).
    pub fn token(self) -> &'static str {
        match self {
            KeyEase::Linear => "lin",
            KeyEase::EaseIn => "in",
            KeyEase::EaseOut => "out",
            KeyEase::Ease => "ease",
            KeyEase::Hold => "hold",
        }
    }

    /// [`token`](Self::token) read back.
    pub fn from_token(token: &[u8]) -> Option<KeyEase> {
        Some(match token {
            b"lin" => KeyEase::Linear,
            b"in" => KeyEase::EaseIn,
            b"out" => KeyEase::EaseOut,
            b"ease" => KeyEase::Ease,
            b"hold" => KeyEase::Hold,
            _ => return None,
        })
    }

    /// The next ease in [`ALL`](Self::ALL), wrapping: what a "cycle the easing" button does.
    pub fn next(self) -> KeyEase {
        let i = Self::ALL.iter().position(|e| *e == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// How far along its segment a value is, once `ease` has shaped the plain
/// fraction `t` (`0..=1`) of the way between two keys. `0` at `t == 0` and `1`
/// at `t == 1` for every ease but `Hold`, which stays `0` until the segment is
/// done. The one function every key type's interpolation goes through.
pub fn ease_shape(ease: KeyEase, t: f32) -> f32 {
    match ease {
        KeyEase::Linear => t,
        KeyEase::Ease => t * t * (3.0 - 2.0 * t),
        KeyEase::EaseIn => t * t * t,
        KeyEase::EaseOut => {
            let u = 1.0 - t;
            1.0 - u * u * u
        }
        KeyEase::Hold => {
            if t >= 1.0 {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// One point of a clip's transform animation ([`crate::project::Clip::transform_keys`]):
/// the whole [`TransformParams`] at source frame `at`. `at` is an absolute
/// source frame, numbered as `in_frame` is, so a trim leaves every key on the
/// footage it was set on -- the same promise a volume point makes
/// ([`crate::project::GainKey`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformKey {
    /// Source frame of the key.
    pub at: u32,
    /// The placement exactly at that frame.
    pub params: TransformParams,
    /// The easing of the segment that leaves this key.
    pub ease: KeyEase,
}

/// A key of an animation: a value at source frame `at` and the ease of the
/// segment that leaves it. The interpolation, the trim and the split are
/// written once over this and shared by every key type.
pub trait Keyed: Copy {
    /// Source frame of the key.
    fn at(&self) -> u32;
    /// The ease of the segment leaving the key.
    fn ease(&self) -> KeyEase;
    /// The same value moved to frame `at`, leaving with `ease`.
    fn placed(self, at: u32, ease: KeyEase) -> Self;
    /// The value `t` of the way (already eased) from `a` to `b`.
    fn mix(a: &Self, b: &Self, t: f32) -> Self;
}

/// The key an animation gives at source frame `src` (fractional), or `None`
/// with no keys. Held before the first and after the last key; between two
/// keys, interpolated with the earlier key's ease. Exact at a key. The
/// returned key's frame and ease are the earlier key's.
pub fn sample<K: Keyed>(keys: &[K], src: f64) -> Option<K> {
    let first = keys.first()?;
    let last = keys.last()?;
    if src <= f64::from(first.at()) {
        return Some(*first);
    }
    if src >= f64::from(last.at()) {
        return Some(*last);
    }
    // The first key strictly after `src`; its predecessor starts the segment
    // `src` is in, and both exist by the two holds above.
    let i = keys.partition_point(|k| f64::from(k.at()) <= src);
    let (a, b) = (&keys[i - 1], &keys[i]);
    let t = ((src - f64::from(a.at())) / f64::from(b.at() - a.at())) as f32;
    let t = ease_shape(a.ease(), t);
    Some(K::mix(a, b, t).placed(a.at(), a.ease()))
}

impl Keyed for TransformKey {
    fn at(&self) -> u32 {
        self.at
    }
    fn ease(&self) -> KeyEase {
        self.ease
    }
    fn placed(self, at: u32, ease: KeyEase) -> Self {
        Self { at, ease, ..self }
    }
    fn mix(a: &Self, b: &Self, t: f32) -> Self {
        Self {
            params: lerp_params(&a.params, &b.params, t),
            ..*a
        }
    }
}

/// One point of a clip's opacity animation ([`crate::project::Clip::opacity_keys`]):
/// how much of the placed picture shows (`0..=1`) at source frame `at`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpacityKey {
    /// Source frame of the key.
    pub at: u32,
    /// Opacity exactly there, `0.0` (clear) to `1.0` (opaque).
    pub opacity: f32,
    /// The easing of the segment that leaves this key.
    pub ease: KeyEase,
}

impl Keyed for OpacityKey {
    fn at(&self) -> u32 {
        self.at
    }
    fn ease(&self) -> KeyEase {
        self.ease
    }
    fn placed(self, at: u32, ease: KeyEase) -> Self {
        Self { at, ease, ..self }
    }
    fn mix(a: &Self, b: &Self, t: f32) -> Self {
        Self {
            opacity: a.opacity + (b.opacity - a.opacity) * t,
            ..*a
        }
    }
}

/// Every field of `a` moved `t` of the way to `b`, `t` in `0..=1`.
pub fn lerp_params(a: &TransformParams, b: &TransformParams, t: f32) -> TransformParams {
    let l = |x: f32, y: f32| x + (y - x) * t;
    TransformParams {
        pos_x: l(a.pos_x, b.pos_x),
        pos_y: l(a.pos_y, b.pos_y),
        scale: l(a.scale, b.scale),
        rotate: l(a.rotate, b.rotate),
        crop_l: l(a.crop_l, b.crop_l),
        crop_r: l(a.crop_r, b.crop_r),
        crop_t: l(a.crop_t, b.crop_t),
        crop_b: l(a.crop_b, b.crop_b),
        opacity: l(a.opacity, b.opacity),
    }
}

/// The placement the keys give at source frame `src` (fractional), or `None`
/// with no keys. Held before the first and after the last key; between two
/// keys, each field is interpolated with the earlier key's [`KeyEase`]. Exact
/// at a key.
pub fn keys_at(keys: &[TransformKey], src: f64) -> Option<TransformParams> {
    sample(keys, src).map(|k| k.params)
}

/// [`keys_at`], or `base` when there are no keys: the one placement a clip
/// shows at source frame `src`.
pub fn evaluate(base: &TransformParams, keys: &[TransformKey], src: f64) -> TransformParams {
    keys_at(keys, src).unwrap_or(*base)
}

/// The opacity the keys give at source frame `src`, or `base` with no keys.
pub fn evaluate_opacity(base: f32, keys: &[OpacityKey], src: f64) -> f32 {
    sample(keys, src).map_or(base, |k| k.opacity)
}

/// The keys that still shape the picture over source frames `lo..=hi`, after
/// a trim left the clip showing only that window. Keys inside it stay where
/// they are. A key before `lo` or after `hi` is dropped, and in its place
/// there is a key at the window's edge holding the placement the whole
/// animation gives there, so the visible part moves exactly as it did. An
/// edge with nothing dropped gets no key: the animation already holds there.
/// Empty in, empty out.
pub fn keys_within<K: Keyed>(keys: &[K], lo: u32, hi: u32) -> Vec<K> {
    if keys.is_empty() || lo > hi {
        return Vec::new();
    }
    // The easing of the segment the edge sits in, as a split takes it (`split_transform_keys`).
    let edge_key = |src: u32| {
        sample(keys, f64::from(src)).expect("keyed, so a value").placed(
            src,
            keys.iter()
                .rev()
                .find(|k| k.at() <= src)
                .map_or(KeyEase::Linear, |k| k.ease()),
        )
    };
    let mut out = Vec::new();
    if keys.iter().any(|k| k.at() < lo) && !keys.iter().any(|k| k.at() == lo) {
        out.push(edge_key(lo));
    }
    out.extend(keys.iter().copied().filter(|k| (lo..=hi).contains(&k.at())));
    if hi > lo && keys.iter().any(|k| k.at() > hi) && !keys.iter().any(|k| k.at() == hi) {
        out.push(edge_key(hi));
    }
    out
}

/// The keys of a still image after its head moved. A still has no frame to
/// walk an in-point back to, so its trim puts the in-point at frame 0 and
/// the picture plays from there; the keys must not follow that frame, or the
/// animation slides along the timeline. Instead each key keeps its *timeline*
/// time: the clip started at `old_start` playing from `old_in`, and now starts
/// at `new_start` with its in-point at 0 and its out-point at `new_out`, at
/// `speed` source frames per timeline frame. Keys that fall before the new
/// head or past the new tail are cut with [`keys_within`].
pub fn keys_after_still_head<K: Keyed>(
    keys: &[K],
    old_start: u32,
    old_in: u32,
    new_start: u32,
    new_out: u32,
    speed: f64,
) -> Vec<K> {
    // Source frames to add to a key: the old timeline time of the key minus
    // the new one, both measured from the new in-point at 0.
    let moved = (f64::from(old_start) - f64::from(new_start)) * speed;
    let shift = moved.round() as i64 - i64::from(old_in);
    // The old source frames that now bound the window: `lo` is the new frame
    // 0 and `hi` the new out-point, both shifted back into the old keys' frames.
    let lo = (-shift).max(0);
    let hi = (i64::from(new_out) - shift).max(lo);
    keys_within(keys, lo as u32, hi as u32)
        .into_iter()
        .map(|k| k.placed((i64::from(k.at()) + shift) as u32, k.ease()))
        .collect()
}

/// A place on the canvas a picture can be put in: the centre, or one of the
/// four corners. [`corner_offset`] turns one into `pos_x`/`pos_y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    Centre,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// Every corner, centre first, in the order a picker lists them.
    pub const ALL: [Corner; 5] = [
        Corner::Centre,
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    /// The sign of this corner on each axis: `(x, y)`, with `+1` right and down,
    /// `-1` left and up, and `0` for the centre.
    fn signs(self) -> (f64, f64) {
        match self {
            Corner::Centre => (0.0, 0.0),
            Corner::TopLeft => (-1.0, -1.0),
            Corner::TopRight => (1.0, -1.0),
            Corner::BottomLeft => (-1.0, 1.0),
            Corner::BottomRight => (1.0, 1.0),
        }
    }
}

/// The `pos_x`/`pos_y` that put a `src_w` x `src_h` picture in `corner` of a
/// `canvas_w` x `canvas_h` canvas, at `scale`, with `margin` of air between the
/// picture and the two edges it touches.
///
/// `margin` is a fraction of the canvas width, on both axes; the vertical margin
/// is that same distance in pixels, so it is `margin * canvas_w / canvas_h` of
/// the height. The picture is fitted inside the canvas the way the default
/// [`crate::scale::FitPolicy::Fit`] fits it, then `scale` is applied to that box:
/// a picture at least as wide (in aspect) as the canvas fills its width, a taller
/// one fills its height. So the half sizes are `scale / 2` on the filled axis and
/// the other follows the picture's aspect, and each offset is
/// `0.5 - half size - margin`, signed by the corner.
///
/// A 16:9 picture on a 16:9 canvas at scale 0.3 with a 0.03 margin lands at
/// `x = 0.32`, `y = -0.32` in the top-right corner. [`Corner::Centre`] is `(0, 0)`.
pub fn corner_offset(
    corner: Corner,
    scale: f32,
    margin: f32,
    src: (u32, u32),
    canvas: (u32, u32),
) -> (f32, f32) {
    let (half_w, half_h) = fitted_half(scale, src, canvas);
    let (cw, ch) = (f64::from(canvas.0.max(1)), f64::from(canvas.1.max(1)));
    let margin_x = f64::from(margin);
    let margin_y = margin_x * cw / ch;
    let (sx, sy) = corner.signs();
    // `+ 0.0` turns the -0.0 a centred axis would otherwise carry into 0.
    let x = sx * (0.5 - half_w - margin_x) + 0.0;
    let y = sy * (0.5 - half_h - margin_y) + 0.0;
    (x as f32, y as f32)
}

/// Half the width and height of a `src` picture at `scale` on a `canvas`, as
/// fractions of the canvas width and height: the box the default fit gives,
/// scaled. The shared measure of [`corner_offset`] and [`placed_half`].
fn fitted_half(scale: f32, src: (u32, u32), canvas: (u32, u32)) -> (f64, f64) {
    let (sw, sh) = (f64::from(src.0.max(1)), f64::from(src.1.max(1)));
    let (cw, ch) = (f64::from(canvas.0.max(1)), f64::from(canvas.1.max(1)));
    let half = f64::from(scale) / 2.0;
    if sw * ch >= cw * sh {
        (half, half * (sh / sw) * (cw / ch))
    } else {
        (half * (sw / sh) * (ch / cw), half)
    }
}

/// The gap a corner placement keeps from the two canvas edges it touches, in
/// pixels: the default the transform card and the `corner` op use.
pub const CORNER_MARGIN_PX: f32 = 32.0;

/// [`corner_offset`] for the picture a placement `t` shows: the picture is
/// measured as it appears, after its crop and its quarter turn (`src` is its
/// own pixel size), and `margin_px` is the gap in pixels. Only `scale` and the
/// shape come from `t`; the position is what this returns.
pub fn corner_placement(
    corner: Corner,
    t: &TransformParams,
    src: (u32, u32),
    canvas: (u32, u32),
    margin_px: f32,
) -> (f32, f32) {
    let crop = crate::scale::crop_rect(src.0, src.1, t);
    let turned = crate::scale::nearest_90_steps(t.rotate) % 2 == 1;
    let shape = if turned { (crop.h, crop.w) } else { (crop.w, crop.h) };
    corner_offset(corner, t.scale, margin_px / canvas.0.max(1) as f32, shape, canvas)
}

/// Half the width and height of the picture `t` shows, as fractions of the
/// canvas: measured as [`corner_placement`] measures it (after the crop and
/// the quarter turn), so a viewer outline matches what is drawn.
pub fn placed_half(t: &TransformParams, src: (u32, u32), canvas: (u32, u32)) -> (f32, f32) {
    let crop = crate::scale::crop_rect(src.0, src.1, t);
    let turned = crate::scale::nearest_90_steps(t.rotate) % 2 == 1;
    let shape = if turned { (crop.h, crop.w) } else { (crop.w, crop.h) };
    let (hw, hh) = fitted_half(t.scale, shape, canvas);
    (hw as f32, hh as f32)
}

/// The placement `new` should have when it changes only the scale of `old`:
/// a picture that sat in a corner stays in that corner at its new size, so its
/// position is recomputed. A custom position is kept as it is, and so is any
/// placement that did not change scale.
pub fn keep_corner(
    old: &TransformParams,
    new: TransformParams,
    src: (u32, u32),
    canvas: (u32, u32),
    margin_px: f32,
) -> TransformParams {
    if new.scale == old.scale {
        return new;
    }
    match corner_of(old, src, canvas, margin_px) {
        Some(corner) => {
            let (pos_x, pos_y) = corner_placement(corner, &new, src, canvas, margin_px);
            TransformParams { pos_x, pos_y, ..new }
        }
        None => new,
    }
}

/// The corner `t` already sits in, when its position is one [`corner_placement`]
/// gives to within half a pixel on both axes (at its own scale, crop and turn).
/// `None` when the position is custom.
pub fn corner_of(t: &TransformParams, src: (u32, u32), canvas: (u32, u32), margin_px: f32) -> Option<Corner> {
    let (cw, ch) = (canvas.0.max(1) as f32, canvas.1.max(1) as f32);
    Corner::ALL.into_iter().find(|&c| {
        let (x, y) = corner_placement(c, t, src, canvas, margin_px);
        (t.pos_x - x).abs() * cw <= 0.5 && (t.pos_y - y).abs() * ch <= 0.5
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity() {
        assert!(TransformParams::default().is_identity());
        assert!(!TransformParams {
            pos_x: 0.001,
            ..TransformParams::default()
        }
        .is_identity());
    }

    #[test]
    fn keys_interpolate_linearly_and_hold_outside() {
        let at = |frame, scale| TransformKey {
            at: frame,
            params: TransformParams {
                scale,
                ..TransformParams::default()
            },
            ease: KeyEase::Linear,
        };
        let keys = [at(0, 1.0), at(100, 1.2)];
        let mid = keys_at(&keys, 50.0).expect("keyed");
        assert!((mid.scale - 1.1).abs() < 1e-6, "midpoint of 1.0 and 1.2");
        assert_eq!(keys_at(&keys, 0.0).unwrap().scale, 1.0, "exact at a key");
        assert_eq!(keys_at(&keys, 100.0).unwrap().scale, 1.2, "exact at the last");
        assert_eq!(keys_at(&keys, -5.0).unwrap().scale, 1.0, "held before");
        assert_eq!(keys_at(&keys, 999.0).unwrap().scale, 1.2, "held after");
        assert!(keys_at(&[], 3.0).is_none(), "no keys is no placement");
        assert!(evaluate(&TransformParams::default(), &[], 3.0).is_identity());
    }

    fn scaled(scale: f32) -> TransformParams {
        TransformParams {
            scale,
            ..TransformParams::default()
        }
    }

    fn scaled_key(at: u32, scale: f32, ease: KeyEase) -> TransformKey {
        TransformKey {
            at,
            params: scaled(scale),
            ease,
        }
    }

    #[test]
    fn a_window_keeps_inside_keys_and_edges_hold_the_interpolated_value() {
        let keys = [
            scaled_key(0, 1.0, KeyEase::Linear),
            scaled_key(100, 2.0, KeyEase::Linear),
            scaled_key(200, 1.0, KeyEase::Linear),
        ];
        // Window 50..=150: the key at 100 stays, and the edges get the value
        // the animation has there (1.5 at 50, 1.5 at 150).
        let w = keys_within(&keys, 50, 150);
        let at: Vec<u32> = w.iter().map(|k| k.at).collect();
        assert_eq!(at, vec![50, 100, 150]);
        assert!((w[0].params.scale - 1.5).abs() < 1e-6);
        assert_eq!(w[1].params.scale, 2.0, "the inside key is untouched");
        assert!((w[2].params.scale - 1.5).abs() < 1e-6);
        // The animation still reads the same values over the window.
        for f in 50..=150 {
            let before = keys_at(&keys, f64::from(f)).unwrap().scale;
            let after = keys_at(&w, f64::from(f)).unwrap().scale;
            assert!((before - after).abs() < 1e-6, "frame {f}");
        }
    }

    #[test]
    fn a_window_with_no_key_before_or_after_gets_no_edge_key() {
        let keys = [scaled_key(100, 2.0, KeyEase::Linear), scaled_key(200, 1.0, KeyEase::Linear)];
        // Nothing before 100: the head needs no key, the animation holds there.
        let w = keys_within(&keys, 80, 300);
        assert_eq!(w, keys.to_vec());
        // A window that drops the last key adds one at its edge.
        let w = keys_within(&keys, 0, 150);
        assert_eq!(w.iter().map(|k| k.at).collect::<Vec<_>>(), vec![100, 150]);
    }

    #[test]
    fn a_window_past_every_key_keeps_one_holding_key() {
        let keys = [scaled_key(10, 1.5, KeyEase::Linear), scaled_key(20, 3.0, KeyEase::Linear)];
        // Every key is before the window: the animation is held at 3.0 there,
        // so one key at the window's start says so, and the clip stays keyed.
        let w = keys_within(&keys, 50, 60);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].at, 50);
        assert_eq!(w[0].params.scale, 3.0);
        assert!(keys_within::<TransformKey>(&[], 0, 10).is_empty());
    }

    #[test]
    fn a_still_keeps_its_keys_on_the_timeline_when_its_head_moves() {
        // A still at 30 fps placed at timeline 1 s (frame 30), with keys at
        // timeline 1 s, 5 s and 9 s (source 0, 120, 240). Trim the head to 2 s
        // (frame 60): the keys at 5 s and 9 s must stay there, and the key at
        // 1 s is replaced by one at 2 s holding the value it had there.
        let keys = [
            scaled_key(0, 0.3, KeyEase::Linear),
            scaled_key(120, 0.6, KeyEase::Linear),
            scaled_key(240, 1.0, KeyEase::Linear),
        ];
        let w = keys_after_still_head(&keys, 30, 0, 60, 210, 1.0);
        let timeline = |k: &TransformKey| 60 + k.at;
        assert_eq!(timeline(&w[0]), 60, "the boundary at the new head");
        let at_5s: Vec<u32> = w.iter().map(|k| k.at).collect();
        assert_eq!(at_5s, vec![0, 90, 210], "frames 90 and 210 are timeline 5 s and 9 s");
        // The picture at each timeline frame of the visible part is unchanged.
        for t in 60..=270u32 {
            let before = keys_at(&keys, f64::from(t - 30)).unwrap().scale;
            let after = keys_at(&w, f64::from(t - 60)).unwrap().scale;
            assert!((before - after).abs() < 1e-6, "timeline frame {t}");
        }
    }

    #[test]
    fn a_still_trimmed_to_grow_its_head_keeps_its_keys_too() {
        let keys = [scaled_key(0, 1.0, KeyEase::Linear), scaled_key(60, 2.0, KeyEase::Linear)];
        // Head moved earlier by 10 frames (start 30 -> 20): keys move later.
        let w = keys_after_still_head(&keys, 30, 0, 20, 100, 1.0);
        assert_eq!(w.iter().map(|k| k.at).collect::<Vec<_>>(), vec![10, 70]);
        assert_eq!(w[0].params.scale, 1.0, "the first key is still the first");
    }

    #[test]
    fn ease_slows_the_segment_ends_and_leaves_the_midpoint() {
        let mut keys = [
            TransformKey {
                at: 0,
                params: TransformParams::default(),
                ease: KeyEase::Ease,
            },
            TransformKey {
                at: 100,
                params: TransformParams {
                    pos_x: 0.4,
                    ..TransformParams::default()
                },
                ease: KeyEase::Linear,
            },
        ];
        let mid = keys_at(&keys, 50.0).unwrap().pos_x;
        assert!((mid - 0.2).abs() < 1e-6, "smoothstep is symmetric about 0.5");
        let quarter = keys_at(&keys, 25.0).unwrap().pos_x;
        assert!(quarter < 0.1, "an eased segment starts slower than linear");
        keys[0].ease = KeyEase::Linear;
        assert!((keys_at(&keys, 25.0).unwrap().pos_x - 0.1).abs() < 1e-6);
    }

    #[test]
    fn every_ease_is_zero_at_the_start_and_one_at_the_end_except_hold() {
        for e in [KeyEase::Linear, KeyEase::Ease, KeyEase::EaseIn, KeyEase::EaseOut] {
            assert_eq!(ease_shape(e, 0.0), 0.0, "{e:?} start");
            assert_eq!(ease_shape(e, 1.0), 1.0, "{e:?} end");
        }
        assert_eq!(ease_shape(KeyEase::Hold, 0.0), 0.0);
        assert_eq!(ease_shape(KeyEase::Hold, 0.999), 0.0, "hold stays until the segment is done");
        assert_eq!(ease_shape(KeyEase::Hold, 1.0), 1.0);
    }

    #[test]
    fn the_ease_shapes_have_their_documented_curves() {
        // Exact at the quarter and the half.
        assert_eq!(ease_shape(KeyEase::Linear, 0.25), 0.25);
        assert!((ease_shape(KeyEase::EaseIn, 0.25) - 0.015625).abs() < 1e-7);
        assert!((ease_shape(KeyEase::EaseIn, 0.5) - 0.125).abs() < 1e-7);
        assert!((ease_shape(KeyEase::EaseOut, 0.25) - 0.578125).abs() < 1e-7);
        assert!((ease_shape(KeyEase::EaseOut, 0.5) - 0.875).abs() < 1e-7);
        assert!((ease_shape(KeyEase::Ease, 0.25) - 0.15625).abs() < 1e-7, "smoothstep");
        assert!((ease_shape(KeyEase::Ease, 0.5) - 0.5).abs() < 1e-7, "in-out is symmetric");
        // In-out is symmetric about the middle; in and out mirror each other.
        for i in 1..10 {
            let t = i as f32 / 10.0;
            assert!((ease_shape(KeyEase::Ease, t) + ease_shape(KeyEase::Ease, 1.0 - t) - 1.0).abs() < 1e-6);
            assert!((ease_shape(KeyEase::EaseIn, t) + ease_shape(KeyEase::EaseOut, 1.0 - t) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn names_and_tokens_round_trip_and_cycle() {
        for e in KeyEase::ALL {
            assert_eq!(KeyEase::from_name(e.name()), Some(e));
            assert_eq!(KeyEase::from_token(e.token().as_bytes()), Some(e));
        }
        assert_eq!(KeyEase::from_name("ease"), Some(KeyEase::Ease), "the older spelling");
        assert_eq!(KeyEase::from_name("bounce"), None);
        let mut e = KeyEase::Linear;
        for _ in 0..KeyEase::ALL.len() {
            e = e.next();
        }
        assert_eq!(e, KeyEase::Linear, "a full cycle comes back");
    }

    #[test]
    fn opacity_keys_use_the_same_interpolation_with_every_ease() {
        let ok = |at, opacity, ease| OpacityKey { at, opacity, ease };
        let mid = |ease| evaluate_opacity(1.0, &[ok(0, 0.0, ease), ok(100, 1.0, ease)], 25.0);
        assert!((mid(KeyEase::Linear) - 0.25).abs() < 1e-6);
        assert!((mid(KeyEase::EaseIn) - 0.015625).abs() < 1e-6);
        assert!((mid(KeyEase::EaseOut) - 0.578125).abs() < 1e-6);
        assert!((mid(KeyEase::Ease) - 0.15625).abs() < 1e-6);
        assert_eq!(mid(KeyEase::Hold), 0.0, "hold keeps the first key's value");
        let keys = [ok(0, 0.2, KeyEase::Hold), ok(100, 0.9, KeyEase::Linear)];
        assert_eq!(evaluate_opacity(1.0, &keys, 100.0), 0.9, "the next key is exact");
        assert_eq!(evaluate_opacity(1.0, &keys, 150.0), 0.9, "held after");
        assert_eq!(evaluate_opacity(1.0, &keys, -3.0), 0.2, "held before");
        assert_eq!(evaluate_opacity(0.7, &[], 5.0), 0.7, "no keys is the base");
    }

    #[test]
    fn eased_keys_split_and_trim_through_the_shared_helpers() {
        let ok = |at, opacity, ease| OpacityKey { at, opacity, ease };
        let keys = [ok(0, 0.0, KeyEase::EaseIn), ok(100, 1.0, KeyEase::Linear)];
        let within = keys_within(&keys, 20, 80);
        assert_eq!(within.iter().map(|k| k.at).collect::<Vec<_>>(), vec![20, 80]);
        assert_eq!(within[0].ease, KeyEase::EaseIn, "an edge key keeps the segment's ease");
        let want = evaluate_opacity(1.0, &keys, 20.0);
        assert!((within[0].opacity - want).abs() < 1e-7);
    }

    #[test]
    fn corner_offset_matches_the_documented_examples() {
        // 16:9 on 16:9 at scale 0.3: half-width 0.15, half-height 0.15. The 0.03
        // margin is 0.03 of the width on x, and the same distance in pixels down
        // the height, 0.03 * 16 / 9 of it, so y is -(0.5 - 0.15 - 0.0533).
        let (x, y) = corner_offset(Corner::TopRight, 0.3, 0.03, (1920, 1080), (1920, 1080));
        assert!((x - 0.32).abs() < 1e-5, "{x}");
        assert!((y + (0.5 - 0.15 - 0.03 * 16.0 / 9.0) as f32).abs() < 1e-5, "{y}");
        let (x, y) = corner_offset(Corner::BottomLeft, 0.3, 0.03, (1920, 1080), (1920, 1080));
        assert!((x + 0.32).abs() < 1e-5, "{x}");
        assert!((y - (0.5 - 0.15 - 0.03 * 16.0 / 9.0) as f32).abs() < 1e-5, "{y}");
        // A square logo at 0.2 on 1080x1920 with a 32 px margin (0.0296 of the width):
        // x 0.37, y -0.427, as the MCP help states them.
        let margin = 32.0 / 1080.0;
        let (x, y) = corner_offset(Corner::TopRight, 0.2, margin, (1, 1), (1080, 1920));
        assert!((x - 0.37).abs() < 5e-4, "{x}");
        assert!((y + 0.427).abs() < 5e-4, "{y}");
    }

    #[test]
    fn corner_offset_signs_and_centre() {
        let m = 0.03;
        assert_eq!(corner_offset(Corner::Centre, 0.3, m, (16, 9), (16, 9)), (0.0, 0.0));
        let (x, y) = corner_offset(Corner::TopLeft, 0.3, m, (16, 9), (16, 9));
        assert!(x < 0.0 && y < 0.0);
        let (x, y) = corner_offset(Corner::BottomRight, 0.3, m, (16, 9), (16, 9));
        assert!(x > 0.0 && y > 0.0);
        // The same picture, mirrored, lands on the mirror image.
        let (x, y) = corner_offset(Corner::BottomRight, 0.3, m, (16, 9), (16, 9));
        let (mx, my) = corner_offset(Corner::TopLeft, 0.3, m, (16, 9), (16, 9));
        assert_eq!((x, y), (-mx, -my));
    }

    #[test]
    fn corner_offset_fits_a_taller_picture_by_its_height() {
        // A 9:16 picture on a 16:9 canvas fills the height at scale 1, so its
        // half-width is 0.5 * (9/16) * (9/16) and its half-height 0.5.
        let (x, y) = corner_offset(Corner::TopRight, 1.0, 0.0, (9, 16), (16, 9));
        let half_w = 0.5 * (9.0_f32 / 16.0) * (9.0 / 16.0);
        assert!((x - (0.5 - half_w)).abs() < 1e-5, "{x}");
        assert!((y + 0.0).abs() < 1e-5, "{y}");
    }

    #[test]
    fn corner_placement_measures_the_picture_as_shown() {
        // A 16:9 picture turned a quarter is 9:16 on screen, so its shape flips.
        let turned = TransformParams { rotate: 90.0, scale: 1.0, ..Default::default() };
        let plain = TransformParams { scale: 1.0, ..Default::default() };
        let canvas = (1080, 1920);
        let (x1, _) = corner_placement(Corner::TopRight, &turned, (1920, 1080), canvas, 0.0);
        let (x2, _) = corner_placement(Corner::TopRight, &plain, (1080, 1920), canvas, 0.0);
        assert!((x1 - x2).abs() < 1e-5, "a turned 16:9 is the same shape as a 9:16: {x1} {x2}");
        // The cropped picture is what is measured, not the whole source.
        let cropped = TransformParams { crop_l: 0.25, crop_r: 0.25, scale: 1.0, ..Default::default() };
        let (cx, _) = corner_placement(Corner::TopRight, &cropped, (1000, 1000), (1000, 1000), 0.0);
        assert!((cx - 0.25).abs() < 1e-4, "a half-width crop of a square fills half the width: {cx}");
    }

    #[test]
    fn corner_of_names_only_the_placed_corner() {
        let t = TransformParams { scale: 0.3, ..Default::default() };
        let src = (1920, 1080);
        let canvas = (1920, 1080);
        assert_eq!(corner_of(&t, src, canvas, 32.0), Some(Corner::Centre), "the origin is the centre choice");
        let (x, y) = corner_placement(Corner::BottomRight, &t, src, canvas, 32.0);
        let placed = TransformParams { pos_x: x, pos_y: y, ..t };
        assert_eq!(corner_of(&placed, src, canvas, 32.0), Some(Corner::BottomRight));
        // A nudge of ten pixels is a custom position.
        let moved = TransformParams { pos_x: x - 10.0 / 1920.0, ..placed };
        assert_eq!(corner_of(&moved, src, canvas, 32.0), None);
        // A different margin is a different place, too.
        assert_eq!(corner_of(&placed, src, canvas, 16.0), None);
    }

    #[test]
    fn corner_offset_fits_a_4_3_source_on_a_16_9_canvas_by_height() {
        // A 4:3 picture is 0.75 as wide as the canvas's height in pixels: at
        // scale 0.3 on 1280x720 it is 288 px wide and 216 px tall, so it fills
        // the height (half-height 0.15) and its half-width is 0.1125 of the width.
        // A 32 px margin then puts the centre at x = 1280/2 - 144 - 32 = 464 px and
        // y = 720/2 - 108 - 32 = 220 px, which is 0.3625 and 0.3056 of the canvas.
        let (x, y) = corner_offset(Corner::BottomRight, 0.3, 32.0 / 1280.0, (640, 480), (1280, 720));
        assert!((x * 1280.0 - 464.0).abs() < 0.5, "x {x} is {} px, want 464", x * 1280.0);
        assert!((y * 720.0 - 220.0).abs() < 0.5, "y {y} is {} px, want 220", y * 720.0);
        // Its right and bottom gaps are both the 32 px margin.
        let right = (0.5 - f64::from(x)) * 1280.0 - 144.0;
        let bottom = (0.5 - f64::from(y)) * 720.0 - 108.0;
        assert!((right - 32.0).abs() < 0.5 && (bottom - 32.0).abs() < 0.5, "gaps {right} {bottom}");
    }

    #[test]
    fn corner_offset_margin_is_a_width_fraction_on_both_axes() {
        // The same pixel margin on a 1000x500 canvas is 0.02 across and 0.04 down.
        let (x, y) = corner_offset(Corner::BottomRight, 0.0, 0.02, (1, 1), (1000, 500));
        assert!((x - 0.48).abs() < 1e-5, "{x}");
        assert!((y - 0.46).abs() < 1e-5, "{y}");
    }

    #[test]
    fn keep_corner_holds_the_corner_when_the_scale_changes() {
        let src = (1920, 1080);
        let canvas = (1920, 1080);
        let margin = CORNER_MARGIN_PX;
        let at_full = TransformParams {
            scale: 1.0,
            ..TransformParams::default()
        };
        let (x, y) = corner_placement(Corner::BottomRight, &at_full, src, canvas, margin);
        let placed = TransformParams { pos_x: x, pos_y: y, ..at_full };
        assert_eq!(corner_of(&placed, src, canvas, margin), Some(Corner::BottomRight));
        let smaller = TransformParams { scale: 0.35, ..placed };
        let kept = keep_corner(&placed, smaller, src, canvas, margin);
        assert_eq!(corner_of(&kept, src, canvas, margin), Some(Corner::BottomRight));
        assert!(kept.pos_x > 0.0 && kept.pos_y > 0.0, "still bottom right, not bottom centre");
        // A custom position is left alone.
        let moved = TransformParams { pos_x: 0.1, ..placed };
        assert_eq!(keep_corner(&moved, smaller, src, canvas, margin), smaller);
        // A scale change with no move keeps everything else.
        let same = keep_corner(&placed, placed, src, canvas, margin);
        assert_eq!(same, placed);
    }

    #[test]
    fn placed_half_is_the_box_corner_placement_fits() {
        let canvas = (1920, 1080);
        let t = TransformParams { scale: 0.3, ..TransformParams::default() };
        let (hw, hh) = placed_half(&t, (1920, 1080), canvas);
        assert!((hw - 0.15).abs() < 1e-6 && (hh - 0.15).abs() < 1e-6, "{hw} {hh}");
        let (cx, cy) = corner_placement(Corner::TopRight, &t, (1920, 1080), canvas, 0.0);
        assert!((cx - (0.5 - hw)).abs() < 1e-6 && (cy + (0.5 - hh)).abs() < 1e-6);
    }
}
