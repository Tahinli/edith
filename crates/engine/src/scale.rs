//! I420 rescale and frame placement: the primitive a project resolution needs.
//!
//! A timeline has one resolution; the media on it does not. Everything a
//! mixed-resolution timeline asks for is these three pieces:
//!
//! * [`scale_i420`] resamples a planar frame to another size,
//! * [`fit_rect`] is the pure geometry -- where the scaled picture lands on the
//!   project canvas, and which part of the source it comes from, under one of
//!   four [`FitPolicy`] rules,
//! * [`compose_i420`] blits the scaled picture onto a canvas, so the pixels a
//!   letterbox leaves over stay whatever the canvas was filled with
//!   ([`black_i420`] is that fill).
//!
//! The filter is bilinear in 16.16 fixed point: two taps per axis, column
//! weights precomputed once per plane, no float and no division in the pixel
//! loop. That is the honest quality level for v1 -- a downscale by more than 2x
//! aliases, because two taps cannot carry a wider kernel. The upgrade path is a
//! separable Lanczos-3 (or a box prefilter before the bilinear pass) behind the
//! same signature: nothing here exposes the tap count. 1280x720 -> 1920x1080
//! measures 3.82 ms/frame in release (`perf_720p_to_1080p` below), against the
//! 4.7 ms the BGRA conversion next door already costs per frame.
//!
//! Sample siting: each plane is resampled as its own image with centre-aligned
//! mapping, `src = (dst + 0.5) * src_len / dst_len - 0.5`. For chroma that
//! treats I420 as centre-sited where MPEG-2 calls it left-sited, a quarter-
//! chroma-sample bias that no eye finds and that a siting-aware kernel would fix
//! in the same place the Lanczos upgrade goes.

use crate::transform::TransformParams;

/// Limited-range black: what an untouched canvas pixel is.
pub const BLACK_Y: u8 = 16;
/// Neutral chroma: 128 in both planes.
pub const NEUTRAL_C: u8 = 128;

/// A sample of a planar picture: 8 bits for every picture the engine's own paths
/// compose, or 10 bits in a `u16` for a 10-bit export. The arithmetic is the same
/// at either depth, so it is written once: only the black, the neutral chroma and
/// the largest code change between the two.
pub trait Sample: Copy + Default + Send + Sync + 'static {
    /// Limited-range black, luma.
    const BLACK: Self;
    /// Neutral chroma.
    const NEUTRAL: Self;
    /// The largest code: 255, or 1023 for a 10-bit `u16`.
    const MAX: u32;
    /// The sample's code, widened to `u32`.
    fn code(self) -> u32;
    /// A code already in range, as a sample (truncating, as `as` does).
    fn from_code(code: u32) -> Self;
    /// A float rounded to the nearest code and clamped to the sample's range.
    fn from_f32(v: f32) -> Self;
}

impl Sample for u8 {
    const BLACK: Self = BLACK_Y;
    const NEUTRAL: Self = NEUTRAL_C;
    const MAX: u32 = 255;
    fn code(self) -> u32 {
        u32::from(self)
    }
    fn from_code(code: u32) -> Self {
        code as u8
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
}

/// The 10-bit sample: the same limited-range codes at four times the depth.
impl Sample for u16 {
    const BLACK: Self = 64;
    const NEUTRAL: Self = 512;
    const MAX: u32 = 1023;
    fn code(self) -> u32 {
        u32::from(self)
    }
    fn from_code(code: u32) -> Self {
        code as u16
    }
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 1023.0) as u16
    }
}

/// A rectangle on one plane, in luma samples. Always inside the plane it
/// describes; `fit_rect` never hands back one that is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// How a source picture of one shape meets a canvas of another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitPolicy {
    /// Whole picture, aspect kept, centred -- the leftover canvas is bars.
    #[default]
    Fit,
    /// Whole canvas, aspect kept, centred -- the leftover picture is cropped.
    Fill,
    /// Whole picture on the whole canvas; aspect is not kept.
    Stretch,
    /// No resampling at all: 1:1 samples, centred, padded or cropped per axis.
    Center,
}

/// Placement and crop for one source on one canvas.
///
/// Returns `(dst placement rect on the canvas, src crop rect on the source)`.
/// The caller scales the crop to the placement size and composes it there; a
/// `Center` result has the two rects the same size, i.e. nothing to scale.
///
/// Rounding, and why it is what it is: both rects are aligned to the I420
/// chroma grid, so that one chroma sample of the scaled picture is one chroma
/// sample of the canvas and no colour lands half a sample off. `x` and `y` are
/// floored to even; `w` and `h` are floored to even *unless* the rect ends
/// exactly at the plane edge, where an odd size is fine because both grids end
/// together (that is what keeps an equal-size `Fit` a byte-exact pass-through
/// on an odd-width source). The alignment can therefore cost one sample of
/// aspect accuracy -- under a tenth of a percent at any real resolution.
///
/// A zero-sized canvas or source yields two empty rects. An extreme aspect
/// never rounds the picture away: the smallest rect is one chroma sample (2x2),
/// clipped to the plane. A plane one sample wide or high has no grid to align
/// to and keeps that single sample.
pub fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, policy: FitPolicy) -> (Rect, Rect) {
    let empty = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return (empty, empty);
    }
    let full_src = Rect {
        x: 0,
        y: 0,
        w: src_w,
        h: src_h,
    };
    let full_dst = Rect {
        x: 0,
        y: 0,
        w: dst_w,
        h: dst_h,
    };

    match policy {
        FitPolicy::Stretch => (full_dst, full_src),
        FitPolicy::Fit => {
            // Widest box with the source aspect that still fits the canvas.
            let (w, h) =
                if u64::from(src_w) * u64::from(dst_h) <= u64::from(dst_w) * u64::from(src_h) {
                    (ratio(src_w, dst_h, src_h).min(dst_w), dst_h)
                } else {
                    (dst_w, ratio(src_h, dst_w, src_w).min(dst_h))
                };
            (centred(w, h, dst_w, dst_h), full_src)
        }
        FitPolicy::Fill => {
            // Largest box with the canvas aspect that still fits the source.
            let (w, h) =
                if u64::from(src_w) * u64::from(dst_h) >= u64::from(dst_w) * u64::from(src_h) {
                    (ratio(src_h, dst_w, dst_h).min(src_w), src_h)
                } else {
                    (src_w, ratio(src_w, dst_h, dst_w).min(src_h))
                };
            (full_dst, centred(w, h, src_w, src_h))
        }
        FitPolicy::Center => {
            let w = src_w.min(dst_w);
            let h = src_h.min(dst_h);
            let dst = centred(w, h, dst_w, dst_h);
            let src = centred(w, h, src_w, src_h);
            // Even-alignment may have shrunk one of them; 1:1 means both carry
            // the same sample count, so take the smaller of the two.
            let w = dst.w.min(src.w);
            let h = dst.h.min(src.h);
            (Rect { w, h, ..dst }, Rect { w, h, ..src })
        }
    }
}

/// `value * num / den`, rounded to nearest, in 64-bit so 8K x 8K cannot wrap.
fn ratio(value: u32, num: u32, den: u32) -> u32 {
    let den = u64::from(den).max(1);
    ((u64::from(value) * u64::from(num) + den / 2) / den) as u32
}

/// A `w` x `h` box centred in `extent_w` x `extent_h`, chroma-grid aligned.
fn centred(w: u32, h: u32, extent_w: u32, extent_h: u32) -> Rect {
    let (x, w) = align(extent_w.saturating_sub(w) / 2, w, extent_w);
    let (y, h) = align(extent_h.saturating_sub(h) / 2, h, extent_h);
    Rect { x, y, w, h }
}

/// Floors the offset to even, clips the size into the plane, and floors the
/// size to even unless the rect ends at the plane edge.
fn align(offset: u32, size: u32, extent: u32) -> (u32, u32) {
    let offset = (offset & !1).min(extent);
    let mut size = size.min(extent - offset);
    if offset + size < extent {
        size &= !1;
    }
    if size == 0 {
        // An extreme aspect (a 1-sample-wide source) would otherwise round its
        // picture away entirely. One chroma sample is the floor, not zero.
        size = extent.min(2);
    }
    (offset, size)
}

/// Chroma plane size for a luma dimension: `(w + 1) / 2`, both axes.
pub fn chroma_dims(w: usize, h: usize) -> (usize, usize) {
    (w.div_ceil(2), h.div_ceil(2))
}

/// A `w` x `h` I420 canvas filled with limited-range black.
pub fn black_i420(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    black_planes(w, h)
}

/// [`black_i420`] at 10 bits: luma 64, chroma 512.
pub fn black_i420_wide(w: usize, h: usize) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    black_planes(w, h)
}

/// [`black_i420`] at either sample type: a 10-bit canvas is black at 64 and 512.
fn black_planes<T: Sample>(w: usize, h: usize) -> (Vec<T>, Vec<T>, Vec<T>) {
    let (cw, ch) = chroma_dims(w, h);
    (
        vec![T::BLACK; w * h],
        vec![T::NEUTRAL; cw * ch],
        vec![T::NEUTRAL; cw * ch],
    )
}

/// Resamples a tightly packed I420 frame (stride == width) into another one.
///
/// Equal dimensions copy byte for byte -- a project at the media's own
/// resolution is not a resample. Otherwise each plane is bilinear-filtered
/// independently, chroma at its own `(w + 1) / 2` size, so odd dimensions need
/// no special case: a 7x5 source has a 4x3 chroma plane and a 13x9 destination
/// wants a 7x5 one, and that is just another rescale.
///
/// Panics if a slice is not exactly its plane's size; a mis-sized buffer is a
/// caller bug, not a runtime condition, and every alternative silently produces
/// a sheared picture.
#[allow(clippy::too_many_arguments)]
pub fn scale_i420<T: Sample>(
    src_y: &[T],
    src_u: &[T],
    src_v: &[T],
    src_w: usize,
    src_h: usize,
    dst_y: &mut [T],
    dst_u: &mut [T],
    dst_v: &mut [T],
    dst_w: usize,
    dst_h: usize,
) {
    let (src_cw, src_ch) = chroma_dims(src_w, src_h);
    let (dst_cw, dst_ch) = chroma_dims(dst_w, dst_h);
    assert_eq!(src_y.len(), src_w * src_h, "source luma plane");
    assert_eq!(src_u.len(), src_cw * src_ch, "source U plane");
    assert_eq!(src_v.len(), src_cw * src_ch, "source V plane");
    assert_eq!(dst_y.len(), dst_w * dst_h, "destination luma plane");
    assert_eq!(dst_u.len(), dst_cw * dst_ch, "destination U plane");
    assert_eq!(dst_v.len(), dst_cw * dst_ch, "destination V plane");

    if src_w == dst_w && src_h == dst_h {
        dst_y.copy_from_slice(src_y);
        dst_u.copy_from_slice(src_u);
        dst_v.copy_from_slice(src_v);
        return;
    }
    plane(src_y, src_w, src_h, dst_y, dst_w, dst_h);
    plane(src_u, src_cw, src_ch, dst_u, dst_cw, dst_ch);
    plane(src_v, src_cw, src_ch, dst_v, dst_cw, dst_ch);
}

/// 16.16 fixed point: one whole sample step.
const ONE: u32 = 1 << 16;
const HALF: u32 = ONE / 2;

/// The two source samples and the weight of the second one, for one output
/// position: `src = (dst + 0.5) * src_len / dst_len - 0.5`, clamped to the
/// plane so the edges replicate instead of reading past it.
fn taps(dst_len: usize, src_len: usize) -> Vec<(u32, u32, u32)> {
    let step = (src_len as u64) * u64::from(ONE) / dst_len as u64;
    let last = src_len as u32 - 1;
    (0..dst_len)
        .map(|i| {
            // (i + 0.5) * step - 0.5, kept in u64 until the subtraction so the
            // clamp at 0 is the only place a negative could have appeared.
            let centre = (2 * i as u64 + 1) * step / 2;
            let pos = centre.saturating_sub(u64::from(HALF));
            let whole = ((pos >> 16) as u32).min(last);
            let frac = if whole == last {
                0
            } else {
                (pos as u32) & (ONE - 1)
            };
            (whole, (whole + 1).min(last), frac)
        })
        .collect()
}

/// Bilinear resample of one tightly packed 8-bit plane.
fn plane<T: Sample>(src: &[T], src_w: usize, src_h: usize, dst: &mut [T], dst_w: usize, dst_h: usize) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    let cols = taps(dst_w, src_w);
    let rows = taps(dst_h, src_h);

    for (y, &(y0, y1, wy)) in rows.iter().enumerate() {
        let top = &src[y0 as usize * src_w..][..src_w];
        let bottom = &src[y1 as usize * src_w..][..src_w];
        let inv_wy = ONE - wy;
        let out = &mut dst[y * dst_w..][..dst_w];
        for (o, &(x0, x1, wx)) in out.iter_mut().zip(cols.iter()) {
            let inv_wx = ONE - wx;
            let (x0, x1) = (x0 as usize, x1 as usize);
            // Each stage stays under 255 * 65536 + 32768, well inside u32.
            let a = (top[x0].code() * inv_wx + top[x1].code() * wx + HALF) >> 16;
            let b = (bottom[x0].code() * inv_wx + bottom[x1].code() * wx + HALF) >> 16;
            *o = T::from_code((a * inv_wy + b * wy + HALF) >> 16);
        }
    }
}

/// Blits a frame of exactly `rect.w` x `rect.h` onto a canvas at `rect`.
///
/// The rect is clipped to the canvas first, so a rect from anywhere (not just
/// [`fit_rect`]) is safe; a clipped blit copies the part that fits, from the
/// frame's top-left. Everything outside the rect keeps whatever the canvas
/// already held -- [`black_i420`] for a letterbox.
///
/// Chroma rounding: `rect.x/2` and `rect.y/2` address the canvas chroma plane,
/// which is exact because [`fit_rect`] hands back even offsets. An odd offset
/// would put the frame's chroma half a sample off its own grid, so it is
/// floored to even here rather than silently smeared.
#[allow(clippy::too_many_arguments)]
pub fn compose_i420<T: Sample>(
    canvas_y: &mut [T],
    canvas_u: &mut [T],
    canvas_v: &mut [T],
    canvas_w: usize,
    canvas_h: usize,
    frame_y: &[T],
    frame_u: &[T],
    frame_v: &[T],
    rect: Rect,
) {
    let x = (rect.x & !1) as usize;
    let y = (rect.y & !1) as usize;
    if x >= canvas_w || y >= canvas_h {
        return;
    }
    let fw = rect.w as usize;
    let fh = rect.h as usize;
    let w = fw.min(canvas_w - x);
    let h = fh.min(canvas_h - y);
    if w == 0 || h == 0 {
        return;
    }
    for row in 0..h {
        canvas_y[(y + row) * canvas_w + x..][..w].copy_from_slice(&frame_y[row * fw..][..w]);
    }

    let (canvas_cw, _) = chroma_dims(canvas_w, canvas_h);
    let (fcw, _) = chroma_dims(fw, fh);
    // Half-resolution copy of the clipped luma region, rounded up so an odd
    // width still carries its last (half-covered) chroma sample.
    let (cw, ch) = chroma_dims(w, h);
    let (cx, cy) = (x / 2, y / 2);
    for row in 0..ch {
        let dst = (cy + row) * canvas_cw + cx;
        canvas_u[dst..][..cw].copy_from_slice(&frame_u[row * fcw..][..cw]);
        canvas_v[dst..][..cw].copy_from_slice(&frame_v[row * fcw..][..cw]);
    }
}

/// The source rect [`TransformParams`]'s crop fractions cut out of a `src_w` x
/// `src_h` picture, chroma-grid aligned exactly as [`fit_rect`]'s rects are.
/// Fractions are clamped to `0.0..=0.5` a side, so opposing crops can never
/// meet or cross -- the smallest surviving rect is one chroma sample, for
/// [`align`]'s reason.
pub fn crop_rect(src_w: u32, src_h: u32, t: &TransformParams) -> Rect {
    if src_w == 0 || src_h == 0 {
        return Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
    }
    let (cl, cr) = (t.crop_l.clamp(0.0, 0.5), t.crop_r.clamp(0.0, 0.5));
    let (ct, cb) = (t.crop_t.clamp(0.0, 0.5), t.crop_b.clamp(0.0, 0.5));
    let x0 = (f64::from(src_w) * f64::from(cl)).round() as u32;
    let x1 = (f64::from(src_w) * (1.0 - f64::from(cr))).round() as u32;
    let y0 = (f64::from(src_h) * f64::from(ct)).round() as u32;
    let y1 = (f64::from(src_h) * (1.0 - f64::from(cb))).round() as u32;
    let (x, w) = align(x0.min(src_w), x1.saturating_sub(x0), src_w);
    let (y, h) = align(y0.min(src_h), y1.saturating_sub(y0), src_h);
    Rect { x, y, w, h }
}

/// `dst` -- a placement [`fit_rect`] already worked out, paired with the `src`
/// rect (inside a `src_w` x `src_h` picture) it is sourced from -- moved by
/// [`TransformParams::pos_x`]/`pos_y` (canvas fractions, about `dst`'s own
/// centre) and resized by `scale` (about that same centre).
///
/// Translation and scale never distort the picture: the placed box keeps the
/// aspect its size and `scale` give it, full stop. What moving it off-canvas
/// does is clip -- the part of the box that falls outside `0..canvas_w` /
/// `0..canvas_h` is dropped, and the matching fraction of `src` is trimmed
/// off the same sides, so what remains on screen is an unscaled slice of the
/// source, not the whole source squeezed into whatever box survived
/// clamping. Both rects come back chroma-grid aligned like every other rect
/// here; a placement pushed entirely off the canvas comes back as two empty
/// rects, which is what [`compose_i420`] already treats as nothing to draw.
pub fn transformed_dst_rect(
    dst: Rect,
    src: Rect,
    src_w: u32,
    src_h: u32,
    t: &TransformParams,
    canvas_w: u32,
    canvas_h: u32,
) -> (Rect, Rect) {
    let empty = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    if dst.is_empty() || src.is_empty() || canvas_w == 0 || canvas_h == 0 {
        return (dst, src);
    }
    let scale = if t.scale.is_finite() && t.scale > 0.0 {
        f64::from(t.scale)
    } else {
        1.0
    };
    // Bounded well above any real canvas so the src-trim ratios below cannot
    // overflow i64, however extreme a stored scale value is.
    let cap_w = i64::from(canvas_w.max(1)) * 1024;
    let cap_h = i64::from(canvas_h.max(1)) * 1024;
    let new_w = ((f64::from(dst.w) * scale).round() as i64).clamp(1, cap_w);
    let new_h = ((f64::from(dst.h) * scale).round() as i64).clamp(1, cap_h);
    // ...and the position, to the same ceiling, for the same reason. A
    // `transform` line takes any finite `f32` for `pos_x`/`pos_y` (there is
    // no per-field clamp at the parse, unlike `scale`'s use of the value), and
    // `f64 as i64` *saturates* -- so a hostile 3e38 arrived here as `i64::MAX`
    // and the additions below, not the cast, are what overflowed: a panic on
    // the compositing thread in a debug build and a silently wrapped position
    // in a release one. A cap in canvas fractions is the same 1024 the scale
    // gets, and a box pushed that far off the canvas is long gone either way:
    // it comes back as the two empty rects the function already answers with.
    let pos_x = if t.pos_x.is_finite() {
        f64::from(t.pos_x)
    } else {
        0.0
    };
    let pos_y = if t.pos_y.is_finite() {
        f64::from(t.pos_y)
    } else {
        0.0
    };
    let cx = (i64::from(dst.x) + i64::from(dst.w) / 2)
        .saturating_add((f64::from(canvas_w) * pos_x.clamp(-1024.0, 1024.0)).round() as i64);
    let cy = (i64::from(dst.y) + i64::from(dst.h) / 2)
        .saturating_add((f64::from(canvas_h) * pos_y.clamp(-1024.0, 1024.0)).round() as i64);
    // The full box the transform describes, unclamped -- it may start
    // negative or run past either edge, which is exactly what "moved off the
    // canvas" means.
    let bx = cx - new_w / 2;
    let by = cy - new_h / 2;

    // How much of that box, in the box's own pixels, falls outside the
    // canvas on each side.
    let left = (-bx).clamp(0, new_w);
    let top = (-by).clamp(0, new_h);
    let right = (bx + new_w - i64::from(canvas_w)).clamp(0, new_w);
    let bottom = (by + new_h - i64::from(canvas_h)).clamp(0, new_h);
    if left + right >= new_w || top + bottom >= new_h {
        return (empty, empty);
    }

    let dst_x = (bx + left).clamp(0, i64::from(canvas_w)) as u32;
    let dst_y = (by + top).clamp(0, i64::from(canvas_h)) as u32;
    let (dst_x, dst_w) = align(dst_x, (new_w - left - right) as u32, canvas_w);
    let (dst_y, dst_h) = align(dst_y, (new_h - top - bottom) as u32, canvas_h);

    // The same fraction trimmed off `src`, on the matching side: uniform
    // scale means the ratio of trimmed-box-pixels to source-pixels is the
    // same on both axes as the ratio of the box's own size to `src`'s.
    let src_left = i64::from(src.w) * left / new_w;
    let src_right = i64::from(src.w) * right / new_w;
    let src_top = i64::from(src.h) * top / new_h;
    let src_bottom = i64::from(src.h) * bottom / new_h;
    let src_x = src.x + src_left as u32;
    let src_y = src.y + src_top as u32;
    let (src_x, src_w_) = align(
        src_x,
        src.w.saturating_sub((src_left + src_right) as u32),
        src_w,
    );
    let (src_y, src_h_) = align(
        src_y,
        src.h.saturating_sub((src_top + src_bottom) as u32),
        src_h,
    );

    (
        Rect {
            x: dst_x,
            y: dst_y,
            w: dst_w,
            h: dst_h,
        },
        Rect {
            x: src_x,
            y: src_y,
            w: src_w_,
            h: src_h_,
        },
    )
}

/// Nearest 90-degree step `degrees` renders at: `0` (0deg), `1` (90deg
/// clockwise), `2` (180deg) or `3` (270deg). Continuous rotation is not
/// rendered at all -- see the [`crate::transform`] module docs -- so this is
/// the one lossy step between what a project *stores* and what a frame
/// *shows*. Non-finite folds to `0`.
pub fn nearest_90_steps(degrees: f32) -> u8 {
    if !degrees.is_finite() {
        return 0;
    }
    let norm = degrees.rem_euclid(360.0);
    ((norm / 90.0).round() as i64).rem_euclid(4) as u8
}

/// Rotates a tightly packed I420 frame by `steps` quarter turns clockwise
/// (`steps % 4`; see [`nearest_90_steps`]). `0` and `2` keep the frame's
/// shape, `1` and `3` swap it -- the chroma planes rotate the same way at
/// their own `(w+1)/2` x `(h+1)/2` size, which stays exact because it is
/// exactly the luma size's, swapped the same way.
pub fn rotate_i420_90s<T: Sample>(
    y: &[T],
    u: &[T],
    v: &[T],
    w: u32,
    h: u32,
    steps: u8,
) -> (Vec<T>, Vec<T>, Vec<T>, u32, u32) {
    let mut out = (Vec::new(), Vec::new(), Vec::new());
    let (rw, rh) = rotate_i420_90s_into(y, u, v, w, h, steps, &mut out);
    (out.0, out.1, out.2, rw, rh)
}

/// [`rotate_i420_90s`] into a caller's own buffers: the top-left of a phone
/// file's display matrix has to be made on **every decoded frame**, so the
/// three plane allocations are a steady-state cost to pay once rather than a
/// per-frame one. The output is exactly what the allocating form returns; the
/// scratch tuple is resized and refilled, never appended to.
pub fn rotate_i420_90s_into<T: Sample>(
    y: &[T],
    u: &[T],
    v: &[T],
    w: u32,
    h: u32,
    steps: u8,
    out: &mut (Vec<T>, Vec<T>, Vec<T>),
) -> (u32, u32) {
    let steps = steps % 4;
    if steps == 0 || w == 0 || h == 0 {
        out.0.clear();
        out.0.extend_from_slice(y);
        out.1.clear();
        out.1.extend_from_slice(u);
        out.2.clear();
        out.2.extend_from_slice(v);
        return (w, h);
    }
    let (cw, ch) = chroma_dims(w as usize, h as usize);
    rotate_plane_into(y, w as usize, h as usize, steps, &mut out.0);
    rotate_plane_into(u, cw, ch, steps, &mut out.1);
    rotate_plane_into(v, cw, ch, steps, &mut out.2);
    if steps == 2 { (w, h) } else { (h, w) }
}

/// One tightly packed plane, rotated `steps` (1..=3) quarter turns clockwise,
/// written into `out` -- resized to the turned size, which is the source's
/// swapped at 90 and 270 and its own at 180.
fn rotate_plane_into<T: Sample>(src: &[T], w: usize, h: usize, steps: u8, out: &mut Vec<T>) {
    match steps {
        1 => {
            out.resize(h * w, T::default());
            for yp in 0..w {
                for xp in 0..h {
                    out[yp * h + xp] = src[(h - 1 - xp) * w + yp];
                }
            }
        }
        2 => {
            out.resize(w * h, T::default());
            for yp in 0..h {
                for xp in 0..w {
                    out[yp * w + xp] = src[(h - 1 - yp) * w + (w - 1 - xp)];
                }
            }
        }
        3 => {
            out.resize(h * w, T::default());
            for yp in 0..w {
                for xp in 0..h {
                    out[yp * h + xp] = src[xp * w + (w - 1 - yp)];
                }
            }
        }
        _ => unreachable!("steps % 4 is 1, 2 or 3 here"),
    }
}

/// One clip's pictures placed on the project canvas: the geometry, plus the
/// buffers it composes into so a frame costs no allocation.
///
/// This is the *one* definition of what a mixed-resolution timeline looks like:
/// playback ([`crate::decode`]) and export ([`crate::export`]) both hand their
/// decoded planes to [`place`](Composer::place), so what is watched and what is
/// written cannot drift apart.
///
/// **Order against the colour grade: grade first, place second.** A grade
/// belongs to the clip's own pixels -- it is graded at the resolution it was
/// shot at, which is also the cheaper end of a downscale -- and, decisively, the
/// letterbox bars are *not* the clip: a brightness grade that reached them would
/// lift the black frame around the picture. Callers therefore grade the source
/// planes and place the result; the bars stay exactly [`black_i420`].
///
/// How many `width` x `height` BGRA pictures a decode worker may keep ahead of
/// its consumer: ~96 MB of them, so the queue is a memory budget rather than a
/// frame count -- 16 at 720p (the ceiling), 12 at 1080p, 3 at 3840x2160, 2 at
/// 4096x2160 (the floor), all measured. A worker holds one more than this in its
/// hand, so a burst off a full queue is `queue_depth + 1`.
///
/// The floor of 2 is the bound this engine ran on everywhere before there was
/// any decode-ahead; the ceiling keeps a small picture from queueing a second of
/// video the consumer would have to walk through after a seek.
pub(crate) fn queue_depth(width: u32, height: u32) -> usize {
    const BUDGET: usize = 96 << 20;
    let bytes = width as usize * height as usize * 4;
    if bytes == 0 {
        return 2;
    }
    (BUDGET / bytes).clamp(2, 16)
}

/// A canvas of the picture's own size is a pass-through: [`place`](Composer::place)
/// hands the caller's own planes straight back, allocates nothing and touches no
/// byte, which is what keeps a project at its media's resolution the byte-for-byte
/// path it was before there was a project resolution at all.
///
/// [`place`]: Composer::place
pub struct Composer {
    width: u32,
    height: u32,
    policy: FitPolicy,
    /// The 8-bit buffers that [`place`](Composer::place) works in.
    narrow: Scratch<u8>,
    /// The 10-bit buffers of [`place_wide`](Composer::place_wide).
    wide: Scratch<u16>,
}

/// The three planes of an I420 picture at one sample type.
type Triple<T> = (Vec<T>, Vec<T>, Vec<T>);

/// One sample type's buffers for a [`Composer`]: the `src` rect of [`fit_rect`],
/// gathered tightly packed (only `Fill` and `Center` ever crop, so it stays empty
/// for a letterboxed project); the crop resampled to the placement rect; and the
/// canvas itself, refilled with black every frame.
#[derive(Default)]
struct Scratch<T> {
    crop: Triple<T>,
    scaled: Triple<T>,
    canvas: Triple<T>,
}

impl Composer {
    pub fn new(width: u32, height: u32, policy: FitPolicy) -> Self {
        Self {
            width,
            height,
            policy,
            narrow: Scratch::default(),
            wide: Scratch::default(),
        }
    }

    /// A composer with the same canvas and fit as this one and none of its scratch:
    /// what a second decode of the same clip is placed with.
    pub(crate) fn fresh(&self) -> Self {
        Self::new(self.width, self.height, self.policy)
    }

    /// A composer that places nothing: every picture passes through at its own
    /// size. What the file-level decode API opens with, where there is no
    /// project and therefore no canvas.
    pub fn passthrough() -> Self {
        Self::new(0, 0, FitPolicy::Fit)
    }

    /// Whether this canvas is the one that places nothing
    /// ([`passthrough`](Self::passthrough)) -- asked without a picture in hand,
    /// unlike [`is_passthrough`](Self::is_passthrough), because the caller that
    /// needs it is sizing a queue before anything has been decoded.
    pub(crate) fn places_nothing(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The canvas this composer places onto, for the painter that fills it
    /// directly at this size -- a picture authored at the canvas's own
    /// dimensions takes the pass-through path [`is_passthrough`](Self::is_passthrough)
    /// answers for, and it needs the numbers before there is a picture.
    pub(crate) fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// How many decoded pictures a worker over this canvas may keep ahead of
    /// its consumer: a budget in *bytes*, so a 4K project runs a shallower queue
    /// than a 1080p one instead of the same number of far bigger frames.
    ///
    /// A **pass-through** canvas has no size of its own -- the pictures come out
    /// at the source's -- so it cannot answer, and the caller that knows the
    /// stream asks [`queue_depth`] with those dimensions instead. Answering 2
    /// here would be the pre-decode-ahead bound, which is exactly the bug this
    /// pair exists to keep out of the file-open path.
    pub(crate) fn queue_depth(&self) -> usize {
        queue_depth(self.width, self.height)
    }

    /// Whether a `src_w` x `src_h` picture is already the canvas, i.e. this
    /// composer would hand it back untouched. What lets a caller keep a fused
    /// fast path (the graded conversion) for the common case.
    pub fn is_passthrough(&self, src_w: u32, src_h: u32) -> bool {
        self.width == 0 || self.height == 0 || (src_w, src_h) == (self.width, self.height)
    }

    /// The picture as the project sees it: `(y, u, v, width, height)`, either the
    /// caller's own planes (pass-through) or the canvas with the fitted picture
    /// composed onto it. The dimensions come back because those two differ.
    ///
    /// Panics on a plane that is not exactly `src_w` x `src_h` (chroma at
    /// `(w + 1) / 2`), as [`scale_i420`] does and for its reason.
    pub fn place<'a>(
        &'a mut self,
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
        src_w: u32,
        src_h: u32,
    ) -> (&'a [u8], &'a [u8], &'a [u8], u32, u32) {
        if self.is_passthrough(src_w, src_h) {
            return (y, u, v, src_w, src_h);
        }
        place_in(self.width, self.height, self.policy, &mut self.narrow, y, u, v, src_w, src_h)
    }

    /// [`place`](Self::place) for a picture at 10 bits a sample: the same fit, the
    /// same canvas, black at 64 and 512, composed at 10 bits.
    pub fn place_wide<'a>(
        &'a mut self,
        y: &'a [u16],
        u: &'a [u16],
        v: &'a [u16],
        src_w: u32,
        src_h: u32,
    ) -> (&'a [u16], &'a [u16], &'a [u16], u32, u32) {
        if self.is_passthrough(src_w, src_h) {
            return (y, u, v, src_w, src_h);
        }
        place_in(self.width, self.height, self.policy, &mut self.wide, y, u, v, src_w, src_h)
    }

    /// [`place`](Self::place) with a per-clip [`TransformParams`] applied on
    /// top of the fit policy: the source is cropped to [`crop_rect`], rotated
    /// by [`rotate_i420_90s`] to [`nearest_90_steps`] of `t.rotate`, fit to the
    /// canvas exactly as an untransformed picture is, and the fitted
    /// placement is then moved/resized/clipped by [`transformed_dst_rect`],
    /// which also trims the matching slice off the source rect so an
    /// off-canvas move clips instead of squeezing.
    /// `t.is_identity()` takes the exact [`place`](Self::place) path.
    ///
    /// corner-cut: unlike `place`, this always allocates its crop and rotate
    /// buffers fresh rather than reusing `self`'s -- the grade path already
    /// pays the same per-frame cost for a graded clip
    /// ([`crate::decode::Render::frame`]'s `graded` copy), so a transformed
    /// clip follows that precedent instead of adding a second steady-state
    /// buffer budget. Upgrade path if this shows up in a measurement: give
    /// `Composer` its own crop/rotate scratch fields, sized once like `crop`
    /// and `scaled` already are.
    pub fn place_transformed(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        src_w: u32,
        src_h: u32,
        t: &TransformParams,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, u32, u32) {
        if t.is_identity() {
            let (y, u, v, w, h) = self.place(y, u, v, src_w, src_h);
            return (y.to_vec(), u.to_vec(), v.to_vec(), w, h);
        }
        transform_in(self.width, self.height, self.policy, y, u, v, src_w, src_h, t)
    }

    /// [`place_transformed`](Self::place_transformed) for a 10-bit picture.
    pub fn place_transformed_wide(
        &mut self,
        y: &[u16],
        u: &[u16],
        v: &[u16],
        src_w: u32,
        src_h: u32,
        t: &TransformParams,
    ) -> (Vec<u16>, Vec<u16>, Vec<u16>, u32, u32) {
        if t.is_identity() {
            let (y, u, v, w, h) = self.place_wide(y, u, v, src_w, src_h);
            return (y.to_vec(), u.to_vec(), v.to_vec(), w, h);
        }
        transform_in(self.width, self.height, self.policy, y, u, v, src_w, src_h, t)
    }
}

/// [`Composer::place`] at one sample type: the picture fitted onto a `width` x
/// `height` canvas under `policy`, the canvas and its scratch in `s`.
#[allow(clippy::too_many_arguments)]
fn place_in<'a, T: Sample>(
    width: u32,
    height: u32,
    policy: FitPolicy,
    s: &'a mut Scratch<T>,
    y: &'a [T],
    u: &'a [T],
    v: &'a [T],
    src_w: u32,
    src_h: u32,
) -> (&'a [T], &'a [T], &'a [T], u32, u32) {
    let (dst, src) = fit_rect(src_w, src_h, width, height, policy);
    let (sw, sh) = (src_w as usize, src_h as usize);

    // Crop first, so the scaler only ever sees a tightly packed plane. Fit and
    // Stretch never crop, which is the whole cost of the common case.
    let (src_y, src_u, src_v) = if (src.w, src.h) == (src_w, src_h) {
        (y, u, v)
    } else {
        crop_i420(&mut s.crop, y, u, v, sw, sh, src);
        (&s.crop.0[..], &s.crop.1[..], &s.crop.2[..])
    };
    // ...then resample it to the placement, unless it already is that size.
    let (cw, ch) = (src.w as usize, src.h as usize);
    let (dw, dh) = (dst.w as usize, dst.h as usize);
    let (pic_y, pic_u, pic_v) = if (cw, ch) == (dw, dh) {
        (src_y, src_u, src_v)
    } else {
        let (ccw, cch) = chroma_dims(dw, dh);
        let (sy, su, sv) = &mut s.scaled;
        sy.resize(dw * dh, T::default());
        su.resize(ccw * cch, T::default());
        sv.resize(ccw * cch, T::default());
        scale_i420(src_y, src_u, src_v, cw, ch, sy, su, sv, dw, dh);
        (&sy[..], &su[..], &sv[..])
    };

    let (w, h) = (width as usize, height as usize);
    let (canvas_cw, canvas_ch) = chroma_dims(w, h);
    let (ky, ku, kv) = &mut s.canvas;
    fill(ky, w * h, T::BLACK);
    fill(ku, canvas_cw * canvas_ch, T::NEUTRAL);
    fill(kv, canvas_cw * canvas_ch, T::NEUTRAL);
    compose_i420(ky, ku, kv, w, h, pic_y, pic_u, pic_v, dst);
    (ky, ku, kv, width, height)
}

/// [`Composer::place_transformed`] at one sample type, for a picture that is not
/// the identity transform. The canvas is the picture's own size on a pass-through
/// composer.
#[allow(clippy::too_many_arguments)]
fn transform_in<T: Sample>(
    width: u32,
    height: u32,
    policy: FitPolicy,
    y: &[T],
    u: &[T],
    v: &[T],
    src_w: u32,
    src_h: u32,
    t: &TransformParams,
) -> (Vec<T>, Vec<T>, Vec<T>, u32, u32) {
    // A pass-through canvas still has a picture to place a transform on; treat it
    // as a canvas the exact size of the incoming picture.
    let (canvas_w, canvas_h) = if width == 0 || height == 0 {
        (src_w, src_h)
    } else {
        (width, height)
    };

    let crop = crop_rect(src_w, src_h, t);
    let mut buf1 = (Vec::new(), Vec::new(), Vec::new());
    let (cy, cu, cv) = if (crop.w, crop.h) == (src_w, src_h) {
        (y, u, v)
    } else {
        crop_i420(&mut buf1, y, u, v, src_w as usize, src_h as usize, crop);
        (&buf1.0[..], &buf1.1[..], &buf1.2[..])
    };

    // No rotation is the overwhelmingly common case (a move or a scale, never a
    // quarter turn): `rotate_i420_90s` always copies its picture, so that copy is
    // skipped when there is nothing to turn.
    let steps = nearest_90_steps(t.rotate);
    let rot_buf;
    let (ry, ru, rv, rw, rh) = if steps == 0 {
        (cy, cu, cv, crop.w, crop.h)
    } else {
        rot_buf = rotate_i420_90s(cy, cu, cv, crop.w, crop.h, steps);
        (&rot_buf.0[..], &rot_buf.1[..], &rot_buf.2[..], rot_buf.3, rot_buf.4)
    };

    let (dst, src) = fit_rect(rw, rh, canvas_w, canvas_h, policy);
    let (dst, src) = transformed_dst_rect(dst, src, rw, rh, t, canvas_w, canvas_h);
    let (rw_u, rh_u) = (rw as usize, rh as usize);
    let mut buf2 = (Vec::new(), Vec::new(), Vec::new());
    let (fy, fu, fv) = if (src.w, src.h) == (rw, rh) {
        (ry, ru, rv)
    } else {
        crop_i420(&mut buf2, ry, ru, rv, rw_u, rh_u, src);
        (&buf2.0[..], &buf2.1[..], &buf2.2[..])
    };

    let (sw, sh) = (src.w as usize, src.h as usize);
    let (dw, dh) = (dst.w as usize, dst.h as usize);
    let mut scaled = (Vec::new(), Vec::new(), Vec::new());
    let (py, pu, pv) = if (sw, sh) == (dw, dh) {
        (fy, fu, fv)
    } else {
        let (ccw, cch) = chroma_dims(dw, dh);
        scaled.0.resize(dw * dh, T::default());
        scaled.1.resize(ccw * cch, T::default());
        scaled.2.resize(ccw * cch, T::default());
        scale_i420(fy, fu, fv, sw, sh, &mut scaled.0, &mut scaled.1, &mut scaled.2, dw, dh);
        (&scaled.0[..], &scaled.1[..], &scaled.2[..])
    };

    let (w, h) = (canvas_w as usize, canvas_h as usize);
    let mut canvas = black_planes::<T>(w, h);
    compose_i420(&mut canvas.0, &mut canvas.1, &mut canvas.2, w, h, py, pu, pv, dst);
    // A single clip's opacity is its fade toward the black canvas it sits on.
    // Layers above a picture take it as their coverage instead (see
    // `export::LayerStack`), so they are placed with opacity 1 there.
    if t.opacity < 1.0 {
        fade_canvas_to_black(&mut canvas, t.opacity.max(0.0));
    }
    (canvas.0, canvas.1, canvas.2, canvas_w, canvas_h)
}

/// Fades an I420 picture drawn on a black canvas toward that black by `k`
/// (`0.0` is all black, `1.0` leaves it), per code value: luma toward black,
/// chroma toward neutral, rounded to the nearest code.
pub fn fade_canvas_to_black<T: Sample>(planes: &mut (Vec<T>, Vec<T>, Vec<T>), k: f32) {
    let k = k.clamp(0.0, 1.0);
    let toward = |plane: &mut [T], black: T| {
        for p in plane {
            let v = black.code() as f32 + (p.code() as f32 - black.code() as f32) * k;
            *p = T::from_f32(v);
        }
    };
    toward(&mut planes.0, T::BLACK);
    toward(&mut planes.1, T::NEUTRAL);
    toward(&mut planes.2, T::NEUTRAL);
}

/// Per-code lerp of two same-sized I420 frames: `out = a + (b - a) * t`, on
/// every plane. `t` is clamped to `[0, 1]`. A mismatched pair panics rather than
/// silently truncating a picture.
pub fn blend_i420<T: Sample>(
    a: (&[T], &[T], &[T]),
    b: (&[T], &[T], &[T]),
    t: f32,
) -> (Vec<T>, Vec<T>, Vec<T>) {
    let t = t.clamp(0.0, 1.0);
    let plane = |a: &[T], b: &[T]| -> Vec<T> {
        assert_eq!(a.len(), b.len(), "blend_i420: mismatched plane sizes");
        a.iter()
            .zip(b)
            .map(|(&av, &bv)| {
                let (av, bv) = (av.code() as f32, bv.code() as f32);
                T::from_f32(av + (bv - av) * t)
            })
            .collect()
    };
    (plane(a.0, b.0), plane(a.1, b.1), plane(a.2, b.2))
}

/// Resizes `plane` to `len` and sets every code to `value` -- a canvas reused
/// across frames is refilled, not reallocated.
fn fill<T: Sample>(plane: &mut Vec<T>, len: usize, value: T) {
    plane.clear();
    plane.resize(len, value);
}

/// Gathers `rect` out of a tightly packed I420 frame into another tightly packed
/// one. `rect` comes from [`fit_rect`], so its offsets are even and the chroma
/// crop is exactly half of it.
fn crop_i420<T: Sample>(
    out: &mut (Vec<T>, Vec<T>, Vec<T>),
    y: &[T],
    u: &[T],
    v: &[T],
    src_w: usize,
    src_h: usize,
    rect: Rect,
) {
    let (w, h) = (rect.w as usize, rect.h as usize);
    let (x, cy0) = (rect.x as usize, rect.y as usize);
    let (src_cw, _) = chroma_dims(src_w, src_h);
    let (cw, ch) = chroma_dims(w, h);
    let (oy, ou, ov) = out;
    oy.clear();
    ou.clear();
    ov.clear();
    for row in 0..h {
        oy.extend_from_slice(&y[(cy0 + row) * src_w + x..][..w]);
    }
    let (cx, cy0) = (x / 2, cy0 / 2);
    for row in 0..ch {
        let at = (cy0 + row) * src_cw + cx;
        ou.extend_from_slice(&u[at..][..cw]);
        ov.extend_from_slice(&v[at..][..cw]);
    }
    debug_assert_eq!(oy.len(), w * h);
    debug_assert_eq!(ou.len(), cw * ch);
}

/// Where a clip's picture lands on the canvas, worked out once from the same
/// helpers [`transform_in`] composes with: the crop, the quarter turn, the fit and
/// the placement's own move, scale and fit-crop.
struct Placed {
    crop: Rect,
    steps: u8,
    dst: Rect,
    src: Rect,
}

fn placed(src: (u32, u32), t: &TransformParams, canvas: (u32, u32), fit: FitPolicy) -> Placed {
    let crop = crop_rect(src.0, src.1, t);
    let steps = nearest_90_steps(t.rotate) % 4;
    let (rw, rh) = if steps % 2 == 1 { (crop.h, crop.w) } else { (crop.w, crop.h) };
    let (dst, srcr) = fit_rect(rw, rh, canvas.0.max(1), canvas.1.max(1), fit);
    let (dst, srcr) = transformed_dst_rect(dst, srcr, rw, rh, t, canvas.0.max(1), canvas.1.max(1));
    Placed { crop, steps, dst, src: srcr }
}

/// A point of the clip's source picture, in picture fractions (`0..=1` each), mapped
/// to where it shows on the canvas, in canvas fractions, under the placement `t`:
/// the inverse of [`canvas_to_picture`]. `src` is the picture's own size. A placement
/// that shows nothing returns the point unchanged.
pub fn picture_to_canvas(
    src: (u32, u32),
    t: &TransformParams,
    canvas: (u32, u32),
    fit: FitPolicy,
    p: (f32, f32),
) -> (f32, f32) {
    let pl = placed(src, t, canvas, fit);
    if pl.dst.is_empty() || pl.src.is_empty() || src.0 == 0 || src.1 == 0 {
        return p;
    }
    let (cw, ch) = (f64::from(pl.crop.w), f64::from(pl.crop.h));
    let x = f64::from(p.0) * f64::from(src.0) - f64::from(pl.crop.x);
    let y = f64::from(p.1) * f64::from(src.1) - f64::from(pl.crop.y);
    let (qx, qy) = match pl.steps {
        0 => (x, y),
        1 => (ch - y, x),
        2 => (cw - x, ch - y),
        _ => (y, cw - x),
    };
    let sx = f64::from(pl.dst.w) / f64::from(pl.src.w);
    let sy = f64::from(pl.dst.h) / f64::from(pl.src.h);
    let cx = f64::from(pl.dst.x) + (qx - f64::from(pl.src.x)) * sx;
    let cy = f64::from(pl.dst.y) + (qy - f64::from(pl.src.y)) * sy;
    ((cx / f64::from(canvas.0.max(1))) as f32, (cy / f64::from(canvas.1.max(1))) as f32)
}

/// A canvas point (canvas fractions) mapped back to the clip's source picture, in
/// picture fractions: the inverse of [`picture_to_canvas`]. A point off the placed
/// picture comes back outside `0..=1`.
pub fn canvas_to_picture(
    src: (u32, u32),
    t: &TransformParams,
    canvas: (u32, u32),
    fit: FitPolicy,
    c: (f32, f32),
) -> (f32, f32) {
    let pl = placed(src, t, canvas, fit);
    if pl.dst.is_empty() || pl.src.is_empty() || src.0 == 0 || src.1 == 0 {
        return c;
    }
    let (cw, ch) = (f64::from(pl.crop.w), f64::from(pl.crop.h));
    let sx = f64::from(pl.dst.w) / f64::from(pl.src.w);
    let sy = f64::from(pl.dst.h) / f64::from(pl.src.h);
    let qx = f64::from(pl.src.x) + (f64::from(c.0) * f64::from(canvas.0.max(1)) - f64::from(pl.dst.x)) / sx;
    let qy = f64::from(pl.src.y) + (f64::from(c.1) * f64::from(canvas.1.max(1)) - f64::from(pl.dst.y)) / sy;
    let (x, y) = match pl.steps {
        0 => (qx, qy),
        1 => (qy, ch - qx),
        2 => (cw - qx, ch - qy),
        _ => (cw - qy, qx),
    };
    (((x + f64::from(pl.crop.x)) / f64::from(src.0)) as f32, ((y + f64::from(pl.crop.y)) / f64::from(src.1)) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Allocates the destination planes for a size and runs a scale into them.
    fn scaled(
        src: &(Vec<u8>, Vec<u8>, Vec<u8>),
        src_w: usize,
        src_h: usize,
        dst_w: usize,
        dst_h: usize,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cw, ch) = chroma_dims(dst_w, dst_h);
        let mut dst = (
            vec![0u8; dst_w * dst_h],
            vec![0u8; cw * ch],
            vec![0u8; cw * ch],
        );
        scale_i420(
            &src.0, &src.1, &src.2, src_w, src_h, &mut dst.0, &mut dst.1, &mut dst.2, dst_w, dst_h,
        );
        dst
    }

    fn gradient(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cw, ch) = chroma_dims(w, h);
        (
            (0..w * h).map(|i| (i % 256) as u8).collect(),
            (0..cw * ch).map(|i| (i % 251) as u8).collect(),
            (0..cw * ch).map(|i| (255 - i % 256) as u8).collect(),
        )
    }

    /// The pass-through invariant: a project at the media's own resolution must
    /// hand the decoder's bytes to the encoder unchanged.
    #[test]
    fn identity_scale_is_byte_identical() {
        for (w, h) in [(64, 48), (7, 5), (1, 1)] {
            let src = gradient(w, h);
            assert_eq!(scaled(&src, w, h, w, h), src, "{w}x{h}");
        }
    }

    /// Halving is the one case where bilinear is exactly a 2x2 box average:
    /// the output centre falls on the shared corner of four input samples.
    /// 0/254 averages to 127 with no rounding slack at all.
    #[test]
    fn downscale_by_two_averages_a_checkerboard() {
        let (w, h) = (32, 24);
        let (cw, ch) = chroma_dims(w, h);
        let checker = |i: usize, stride: usize| {
            if (i / stride + i % stride).is_multiple_of(2) {
                0u8
            } else {
                254
            }
        };
        let src = (
            (0..w * h).map(|i| checker(i, w)).collect::<Vec<_>>(),
            (0..cw * ch).map(|i| checker(i, cw)).collect::<Vec<_>>(),
            (0..cw * ch).map(|i| checker(i, cw)).collect::<Vec<_>>(),
        );
        let (y, u, v) = scaled(&src, w, h, w / 2, h / 2);
        assert!(y.iter().all(|&s| s == 127), "luma: {:?}", &y[..8]);
        assert!(u.iter().all(|&s| s == 127), "U: {:?}", &u[..4]);
        assert!(v.iter().all(|&s| s == 127), "V: {:?}", &v[..4]);
    }

    /// Nothing may leak in from outside a plane: a flat frame stays flat at
    /// every edge, upscaled and downscaled, odd sizes included.
    #[test]
    fn flat_frame_stays_flat() {
        let sizes = [(7, 5), (16, 16), (13, 9), (1, 4)];
        for &(sw, sh) in &sizes {
            let (cw, ch) = chroma_dims(sw, sh);
            let src = (
                vec![80u8; sw * sh],
                vec![90u8; cw * ch],
                vec![200u8; cw * ch],
            );
            for &(dw, dh) in &sizes {
                let (y, u, v) = scaled(&src, sw, sh, dw, dh);
                assert!(y.iter().all(|&s| s == 80), "Y {sw}x{sh}->{dw}x{dh}");
                assert!(u.iter().all(|&s| s == 90), "U {sw}x{sh}->{dw}x{dh}");
                assert!(v.iter().all(|&s| s == 200), "V {sw}x{sh}->{dw}x{dh}");
            }
        }
    }

    /// Chroma alignment: an edge in the U plane must land at the *same* place
    /// after a 2x upscale, with a symmetric two-sample blend around it and
    /// untouched flats either side -- a half-sample bias shows up as an
    /// asymmetric pair here, which is colour bleeding one way.
    #[test]
    fn chroma_edge_keeps_its_position() {
        let (w, h) = (8, 8);
        let (cw, ch) = chroma_dims(w, h);
        // Chroma columns 0,1 = 90 and 2,3 = 200: the edge sits at the middle.
        let src = (
            vec![80u8; w * h],
            (0..cw * ch)
                .map(|i| if i % cw < cw / 2 { 90 } else { 200 })
                .collect(),
            vec![128u8; cw * ch],
        );
        let (_, u, _) = scaled(&src, w, h, w * 2, h * 2);
        let (dcw, _) = chroma_dims(w * 2, h * 2);
        assert_eq!(dcw, 8);
        let row = &u[dcw * 3..][..dcw];
        assert_eq!(&row[..3], &[90, 90, 90], "flat side moved: {row:?}");
        assert_eq!(&row[5..], &[200, 200, 200], "flat side moved: {row:?}");
        // The two blend samples are 117.5 and 172.5 before rounding: symmetric
        // about the 145 midpoint, so they sum to 290. Round-half-up costs at
        // most 1; anything further means the edge moved.
        assert!(
            (i32::from(row[3]) + i32::from(row[4]) - 290).abs() <= 1,
            "blend is off centre, i.e. chroma bled one way: {row:?}"
        );
    }

    /// Geometry, table-driven: 16:9 into 4:3 and back, plus odd sizes.
    /// `(src, dst, policy) -> (placement, crop)`.
    #[test]
    fn fit_rect_table() {
        let r = |x, y, w, h| Rect { x, y, w, h };
        let cases = [
            // 16:9 source on a 4:3 canvas: 640x360 letterboxed inside 640x480.
            (
                (1920, 1080),
                (640, 480),
                FitPolicy::Fit,
                r(0, 60, 640, 360),
                r(0, 0, 1920, 1080),
            ),
            // ...and the same pair filled: the canvas is whole, the source is
            // cropped to 4:3 (1440 wide) about its centre.
            (
                (1920, 1080),
                (640, 480),
                FitPolicy::Fill,
                r(0, 0, 640, 480),
                r(240, 0, 1440, 1080),
            ),
            // 4:3 source on a 16:9 canvas: pillarboxed 1440 wide.
            (
                (640, 480),
                (1920, 1080),
                FitPolicy::Fit,
                r(240, 0, 1440, 1080),
                r(0, 0, 640, 480),
            ),
            (
                (640, 480),
                (1920, 1080),
                FitPolicy::Fill,
                r(0, 0, 1920, 1080),
                r(0, 60, 640, 360),
            ),
            // Stretch ignores aspect entirely, both ways.
            (
                (1920, 1080),
                (640, 480),
                FitPolicy::Stretch,
                r(0, 0, 640, 480),
                r(0, 0, 1920, 1080),
            ),
            // 1:1 with a bigger source: cropped both ways, centred.
            (
                (1920, 1080),
                (640, 480),
                FitPolicy::Center,
                r(0, 0, 640, 480),
                r(640, 300, 640, 480),
            ),
            // 1:1 with a smaller source: padded both ways, centred.
            (
                (640, 480),
                (1920, 1080),
                FitPolicy::Center,
                r(640, 300, 640, 480),
                r(0, 0, 640, 480),
            ),
            // Odd sizes: offsets even, sizes even unless they reach the edge.
            ((7, 5), (7, 5), FitPolicy::Fit, r(0, 0, 7, 5), r(0, 0, 7, 5)),
            (
                (7, 5),
                (13, 9),
                FitPolicy::Fit,
                r(0, 0, 13, 9),
                r(0, 0, 7, 5),
            ),
            // 1:1 into a wider canvas: the 9 odd columns cannot survive an even
            // offset without putting chroma half a sample off, so the last one
            // is dropped -- 8 wide at x=2. Height reaches the edge, so 9 stays.
            (
                (9, 9),
                (13, 9),
                FitPolicy::Center,
                r(2, 0, 8, 9),
                r(0, 0, 8, 9),
            ),
            (
                (13, 9),
                (9, 9),
                FitPolicy::Center,
                r(0, 0, 8, 9),
                r(2, 0, 8, 9),
            ),
        ];
        for ((sw, sh), (dw, dh), policy, want_dst, want_src) in cases {
            let got = fit_rect(sw, sh, dw, dh, policy);
            assert_eq!(
                got,
                (want_dst, want_src),
                "{sw}x{sh} -> {dw}x{dh} {policy:?}"
            );
        }
        // Degenerate input is empty, not a panic.
        assert!(fit_rect(0, 10, 10, 10, FitPolicy::Fit).0.is_empty());
        assert!(fit_rect(10, 10, 10, 0, FitPolicy::Fill).1.is_empty());
    }

    /// Every rect a policy hands back must sit inside its plane, start on the
    /// chroma grid, and end on it or at the plane edge -- the invariant
    /// `compose_i420` relies on.
    #[test]
    fn fit_rect_stays_on_the_chroma_grid() {
        let sizes = [1u32, 2, 3, 7, 16, 33, 640, 1080, 1920];
        for policy in [
            FitPolicy::Fit,
            FitPolicy::Fill,
            FitPolicy::Stretch,
            FitPolicy::Center,
        ] {
            for &sw in &sizes {
                for &dh in &sizes {
                    let (sh, dw) = (sizes[0] + sw / 2 + 1, dh * 2 - 1);
                    let (dst, src) = fit_rect(sw, sh, dw, dh, policy);
                    let grid = [sw, sh, dw, dh].iter().all(|&e| e >= 2);
                    for (rect, (ew, eh), what) in [(dst, (dw, dh), "dst"), (src, (sw, sh), "src")] {
                        let ctx = format!("{policy:?} {sw}x{sh}->{dw}x{dh} {what} {rect:?}");
                        assert!(rect.x + rect.w <= ew, "{ctx} overflows width");
                        assert!(rect.y + rect.h <= eh, "{ctx} overflows height");
                        assert_eq!(rect.x % 2, 0, "{ctx} odd x");
                        assert_eq!(rect.y % 2, 0, "{ctx} odd y");
                        // A plane one sample wide or high has no chroma grid to
                        // be on: `Center` from a 2-high source onto a 1-high
                        // canvas is one row, and no size satisfies both edges.
                        // Nothing real is that shape; it must merely not
                        // escape its plane, which the two asserts above cover.
                        if grid {
                            assert!(rect.w % 2 == 0 || rect.x + rect.w == ew, "{ctx} odd w");
                            assert!(rect.h % 2 == 0 || rect.y + rect.h == eh, "{ctx} odd h");
                        }
                        assert!(!rect.is_empty(), "{ctx} empty");
                    }
                }
            }
        }
    }

    #[test]
    fn compose_fills_the_rect_and_nothing_else() {
        let (cw2, ch2) = (640usize, 360usize);
        let (canvas_w, canvas_h) = (640usize, 480usize);
        let (mut y, mut u, mut v) = black_i420(canvas_w, canvas_h);
        let (fcw, fch) = chroma_dims(cw2, ch2);
        let frame = (
            vec![200u8; cw2 * ch2],
            vec![70u8; fcw * fch],
            vec![30u8; fcw * fch],
        );
        let rect = Rect {
            x: 0,
            y: 60,
            w: cw2 as u32,
            h: ch2 as u32,
        };
        compose_i420(
            &mut y, &mut u, &mut v, canvas_w, canvas_h, &frame.0, &frame.1, &frame.2, rect,
        );

        for row in 0..canvas_h {
            let inside = (60..420).contains(&row);
            let want = if inside { 200 } else { BLACK_Y };
            assert!(
                y[row * canvas_w..][..canvas_w].iter().all(|&s| s == want),
                "luma row {row}"
            );
        }
        let (ccw, cch) = chroma_dims(canvas_w, canvas_h);
        for row in 0..cch {
            let inside = (30..210).contains(&row);
            let (wu, wv) = if inside {
                (70, 30)
            } else {
                (NEUTRAL_C, NEUTRAL_C)
            };
            assert!(
                u[row * ccw..][..ccw].iter().all(|&s| s == wu),
                "U row {row}"
            );
            assert!(
                v[row * ccw..][..ccw].iter().all(|&s| s == wv),
                "V row {row}"
            );
        }
    }

    /// An out-of-bounds rect clips instead of panicking or wrapping -- compose
    /// is public and a caller may compute a rect itself.
    #[test]
    fn compose_clips_a_rect_that_runs_off_the_canvas() {
        let (mut y, mut u, mut v) = black_i420(16, 16);
        let frame = (vec![99u8; 8 * 8], vec![99u8; 4 * 4], vec![99u8; 4 * 4]);
        let rect = Rect {
            x: 12,
            y: 12,
            w: 8,
            h: 8,
        };
        compose_i420(
            &mut y, &mut u, &mut v, 16, 16, &frame.0, &frame.1, &frame.2, rect,
        );
        assert_eq!(y[15 * 16 + 15], 99);
        assert_eq!(y[11 * 16 + 15], BLACK_Y);
        // Wholly outside: no write at all.
        let before = y.clone();
        compose_i420(
            &mut y,
            &mut u,
            &mut v,
            16,
            16,
            &frame.0,
            &frame.1,
            &frame.2,
            Rect {
                x: 100,
                y: 0,
                w: 8,
                h: 8,
            },
        );
        assert_eq!(y, before);
    }

    /// The whole pipeline the wiring slice will call: 4:3 media onto a 16:9
    /// project canvas, letterbox bars intact.
    #[test]
    fn fit_scale_compose_letterboxes() {
        let (sw, sh) = (640usize, 480usize);
        let (dw, dh) = (1920usize, 1080usize);
        let src = gradient(sw, sh);
        let (dst_rect, src_rect) =
            fit_rect(sw as u32, sh as u32, dw as u32, dh as u32, FitPolicy::Fit);
        assert_eq!(
            src_rect,
            Rect {
                x: 0,
                y: 0,
                w: 640,
                h: 480
            }
        );
        let (rw, rh) = (dst_rect.w as usize, dst_rect.h as usize);
        let picture = scaled(&src, sw, sh, rw, rh);
        let (mut y, mut u, mut v) = black_i420(dw, dh);
        compose_i420(
            &mut y, &mut u, &mut v, dw, dh, &picture.0, &picture.1, &picture.2, dst_rect,
        );
        // Pillarbox columns stay black, the picture area does not.
        let mid = (dh / 2) * dw;
        assert!(
            y[mid..mid + dst_rect.x as usize]
                .iter()
                .all(|&s| s == BLACK_Y)
        );
        assert_eq!(
            y[mid + dst_rect.x as usize..][..rw],
            picture.0[(rh / 2) * rw..][..rw]
        );
    }

    /// The wiring invariant: a project at its media's own resolution hands the
    /// decoder's own slices back, not a copy that happens to be equal. Asserted
    /// on the pointer, because "identical bytes" is cheap and "no work at all"
    /// is what the common case is promised.
    #[test]
    fn a_canvas_of_the_source_size_is_a_pass_through() {
        let (w, h) = (64usize, 48usize);
        let src = gradient(w, h);
        for mut canvas in [
            Composer::new(w as u32, h as u32, FitPolicy::Fit),
            Composer::new(w as u32, h as u32, FitPolicy::Fill),
            Composer::new(w as u32, h as u32, FitPolicy::Center),
            Composer::passthrough(),
        ] {
            let (y, u, v, cw, ch) = canvas.place(&src.0, &src.1, &src.2, w as u32, h as u32);
            assert_eq!(y.as_ptr(), src.0.as_ptr(), "luma was copied");
            assert_eq!(u.as_ptr(), src.1.as_ptr());
            assert_eq!(v.as_ptr(), src.2.as_ptr());
            assert_eq!((cw, ch), (w as u32, h as u32));
        }
    }

    /// 4:3 media on a 16:9 project: the picture lands where `fit_rect` says and
    /// the pillarbox columns are limited-range black, exactly -- a bar that is
    /// merely dark is a bar the grade reached.
    #[test]
    fn fit_pillarboxes_with_exact_black_bars() {
        let (sw, sh) = (640u32, 480u32);
        let (dw, dh) = (1920u32, 1080u32);
        let src = (
            vec![200u8; (sw * sh) as usize],
            vec![70u8; (sw * sh / 4) as usize],
            vec![30u8; (sw * sh / 4) as usize],
        );
        let mut canvas = Composer::new(dw, dh, FitPolicy::Fit);
        let (y, u, v, w, h) = canvas.place(&src.0, &src.1, &src.2, sw, sh);
        assert_eq!((w, h), (dw, dh));
        let (rect, _) = fit_rect(sw, sh, dw, dh, FitPolicy::Fit);
        assert_eq!(
            rect,
            Rect {
                x: 240,
                y: 0,
                w: 1440,
                h: 1080
            },
            "geometry moved"
        );
        let (w, h) = (w as usize, h as usize);
        let row = &y[(h / 2) * w..][..w];
        let x = rect.x as usize;
        assert!(row[..x].iter().all(|&s| s == BLACK_Y), "left bar not black");
        assert!(
            row[x + rect.w as usize..].iter().all(|&s| s == BLACK_Y),
            "right bar not black"
        );
        assert!(
            row[x..x + rect.w as usize].iter().all(|&s| s == 200),
            "picture"
        );
        // ...and the bars are neutral in chroma too, or they would be a colour.
        let (cw, _) = chroma_dims(w, h);
        let crow = &u[(h / 4) * cw..][..cw];
        assert!(
            crow[..x / 2].iter().all(|&s| s == NEUTRAL_C),
            "bar has colour"
        );
        assert!(crow[x / 2 + 8].abs_diff(70) <= 1, "picture chroma");
        assert_eq!(v[(h / 4) * cw], NEUTRAL_C);
    }

    /// The other three policies, on the same pair: `Fill` and `Stretch` cover
    /// the canvas (no bar anywhere), `Center` pads without resampling.
    #[test]
    fn fill_and_stretch_leave_no_bars_and_center_pads_1_to_1() {
        let (sw, sh) = (640u32, 480u32);
        let (dw, dh) = (1920u32, 1080u32);
        let src = (
            vec![200u8; (sw * sh) as usize],
            vec![70u8; (sw * sh / 4) as usize],
            vec![30u8; (sw * sh / 4) as usize],
        );
        for policy in [FitPolicy::Fill, FitPolicy::Stretch] {
            let mut canvas = Composer::new(dw, dh, policy);
            let (y, ..) = canvas.place(&src.0, &src.1, &src.2, sw, sh);
            assert!(
                y.iter().all(|&s| s == 200),
                "{policy:?} left a bar: the canvas is not covered"
            );
        }
        let mut canvas = Composer::new(dw, dh, FitPolicy::Center);
        let (y, _, _, w, h) = canvas.place(&src.0, &src.1, &src.2, sw, sh);
        let (rect, _) = fit_rect(sw, sh, dw, dh, FitPolicy::Center);
        assert_eq!(rect.w, sw, "Center resampled the picture");
        let (w, h) = (w as usize, h as usize);
        let row = &y[(h / 2) * w..][..w];
        assert!(row[..rect.x as usize].iter().all(|&s| s == BLACK_Y));
        assert!(
            row[rect.x as usize..][..sw as usize]
                .iter()
                .all(|&s| s == 200)
        );
    }

    /// A crop (`Fill` on a wider source) must gather the *middle* of the
    /// picture, chroma with it: a marked band at a known column has to come out
    /// at the column the geometry puts it at.
    #[test]
    fn fill_crops_about_the_centre() {
        let (sw, sh) = (32usize, 8usize);
        let (dw, dh) = (8u32, 8u32);
        // Column 16 (the centre) is bright, everything else is dark.
        let y: Vec<u8> = (0..sw * sh)
            .map(|i| if i % sw == 16 { 240 } else { 40 })
            .collect();
        let (cw, ch) = chroma_dims(sw, sh);
        let src = (y, vec![100u8; cw * ch], vec![150u8; cw * ch]);
        let mut canvas = Composer::new(dw, dh, FitPolicy::Fill);
        let (y, u, _, w, h) = canvas.place(&src.0, &src.1, &src.2, sw as u32, sh as u32);
        assert_eq!((w, h), (dw, dh));
        let (dst, crop) = fit_rect(sw as u32, sh as u32, dw, dh, FitPolicy::Fill);
        assert_eq!(
            dst,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8
            },
            "Fill must cover"
        );
        assert_eq!(
            crop,
            Rect {
                x: 12,
                y: 0,
                w: 8,
                h: 8
            },
            "crop off centre"
        );
        // The bright column is crop-relative column 4 of 8, untouched by the
        // 1:1 scale, and no sample of the canvas is black: nothing is a bar.
        let row = &y[..8];
        assert_eq!(row[4], 240, "the marked column moved: {row:?}");
        assert!(y.iter().all(|&s| s != BLACK_Y), "Fill left a bar");
        assert!(u.iter().all(|&s| s == 100), "chroma crop lost its plane");
    }

    /// Not asserted: only the release number means anything. Run with
    /// `cargo test -p engine --release scale::tests::perf -- --nocapture`.
    #[test]
    #[ignore = "perf: run with --ignored"]
    fn perf_720p_to_1080p() {
        let src = gradient(1280, 720);
        let (cw, ch) = chroma_dims(1920, 1080);
        let mut dst = (
            vec![0u8; 1920 * 1080],
            vec![0u8; cw * ch],
            vec![0u8; cw * ch],
        );
        let runs = 30;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            scale_i420(
                &src.0, &src.1, &src.2, 1280, 720, &mut dst.0, &mut dst.1, &mut dst.2, 1920, 1080,
            );
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("scale::scale_i420 1280x720 -> 1920x1080: {ms:.3} ms/frame");
    }

    /// The whole per-frame cost of a mixed-resolution timeline -- black fill,
    /// scale and blit -- which is what the playback budget has to hold. Printed,
    /// not asserted, like every other perf test here.
    #[test]
    #[ignore = "perf: run with --ignored"]
    fn perf_compose_720p_onto_1080p() {
        let src = gradient(1280, 720);
        let mut canvas = Composer::new(1920, 1080, FitPolicy::Fit);
        let runs = 30;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(canvas.place(&src.0, &src.1, &src.2, 1280, 720));
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("scale::Composer::place 1280x720 onto 1920x1080 Fit: {ms:.3} ms/frame");
    }

    /// [`blend_i420`] at the midpoint of two solid-colour frames lands exactly
    /// on the 50/50 average, on every plane -- the synthetic stand-in for "the
    /// dissolve's midpoint frame looks like the average colour" the export
    /// path is built on.
    #[test]
    fn blend_i420_is_50_50_at_the_midpoint() {
        let a = (vec![0u8; 16], vec![10u8; 4], vec![20u8; 4]);
        let b = (vec![100u8; 16], vec![50u8; 4], vec![120u8; 4]);
        let (y, u, v) = blend_i420((&a.0, &a.1, &a.2), (&b.0, &b.1, &b.2), 0.5);
        assert!(y.iter().all(|&p| p == 50), "Y midpoint: {y:?}");
        assert!(u.iter().all(|&p| p == 30), "U midpoint: {u:?}");
        assert!(v.iter().all(|&p| p == 70), "V midpoint: {v:?}");
        // The edges hand back each frame untouched.
        let (y0, _, _) = blend_i420((&a.0, &a.1, &a.2), (&b.0, &b.1, &b.2), 0.0);
        assert_eq!(y0, a.0);
        let (y1, _, _) = blend_i420((&a.0, &a.1, &a.2), (&b.0, &b.1, &b.2), 1.0);
        assert_eq!(y1, b.0);
    }

    /// The crop fractions cut a rect out of a 100x100 source at the very
    /// pixels the maths says, chroma-grid aligned like every other rect here.
    #[test]
    fn crop_rect_math() {
        let t = TransformParams {
            crop_l: 0.1,
            crop_r: 0.2,
            crop_t: 0.0,
            crop_b: 0.0,
            ..Default::default()
        };
        assert_eq!(
            crop_rect(100, 100, &t),
            Rect {
                x: 10,
                y: 0,
                w: 70,
                h: 100
            },
            "10% off the left, 20% off the right, nothing off top or bottom"
        );
        // Opposing crops clamp to 0.5 a side rather than meeting or crossing.
        let extreme = TransformParams {
            crop_l: 0.9,
            crop_r: 0.9,
            ..Default::default()
        };
        let r = crop_rect(100, 100, &extreme);
        assert!(r.w >= 2, "the smallest surviving rect is one chroma sample");
        // A zero-sized source has nothing to crop.
        assert_eq!(
            crop_rect(0, 100, &TransformParams::default()),
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            }
        );
    }

    /// A placement moved by `pos_x`/`pos_y` (canvas fractions about its own
    /// centre) and resized by `scale`, both about that same centre. Neither
    /// case here clips, so the source rect comes back untouched.
    #[test]
    fn transformed_dst_rect_at_pos_and_scale() {
        let dst = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let src = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        // Moved a quarter of the canvas to the right, untouched vertically.
        let moved = TransformParams {
            pos_x: 0.25,
            ..Default::default()
        };
        assert_eq!(
            transformed_dst_rect(dst, src, 100, 100, &moved, 200, 200),
            (
                Rect {
                    x: 50,
                    y: 0,
                    w: 100,
                    h: 100
                },
                src
            ),
            "25% of a 200-wide canvas is 50 pixels, about the placement's own centre"
        );
        // Doubled in place, box already centred on the canvas: the centre
        // stays, the box fills the canvas, nothing clips.
        let centred_dst = Rect {
            x: 50,
            y: 50,
            w: 100,
            h: 100,
        };
        let grown = TransformParams {
            scale: 2.0,
            ..Default::default()
        };
        assert_eq!(
            transformed_dst_rect(centred_dst, src, 100, 100, &grown, 200, 200),
            (
                Rect {
                    x: 0,
                    y: 0,
                    w: 200,
                    h: 200
                },
                src
            ),
            "a 100x100 box doubled about its own centre on a 200x200 canvas fills it"
        );
    }

    /// The defect this replaces: a positive offset used to SHRINK the dst
    /// rect (squeezing the whole source into what remained) and a negative
    /// offset used to render pixel-identical to the identity (the clamp at 0
    /// killed it). Real translation keeps the box's size and clips instead:
    /// the src rect trims by the same fraction that fell off-canvas, on the
    /// side that fell off, and the surviving box size never shrinks below
    /// what `scale` alone gives it.
    #[test]
    fn transformed_dst_rect_clips_instead_of_squeezing() {
        let dst = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        let src = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        // +30% of a 100-wide canvas: the box moves 30px right, half of it
        // (50px) falls off the right edge -- half the source is trimmed off
        // its right, not the whole source squeezed into 70px.
        let pos = TransformParams {
            pos_x: 0.30,
            ..Default::default()
        };
        let (d, s) = transformed_dst_rect(dst, src, 100, 100, &pos, 100, 100);
        assert_eq!(
            d,
            Rect {
                x: 30,
                y: 0,
                w: 70,
                h: 100
            },
            "dst size must not shrink below what clipping removes"
        );
        assert_eq!(
            s,
            Rect {
                x: 0,
                y: 0,
                w: 70,
                h: 100
            },
            "src trims off its right, does not squeeze"
        );

        // -30%: symmetric on the left, and no longer identical to identity.
        let neg = TransformParams {
            pos_x: -0.30,
            ..Default::default()
        };
        let (d, s) = transformed_dst_rect(dst, src, 100, 100, &neg, 100, 100);
        assert_eq!(
            d,
            Rect {
                x: 0,
                y: 0,
                w: 70,
                h: 100
            }
        );
        assert_eq!(
            s,
            Rect {
                x: 30,
                y: 0,
                w: 70,
                h: 100
            },
            "src trims off its left"
        );
        assert_ne!(
            (d, s),
            (dst, src),
            "a negative offset must move the picture, not no-op"
        );

        // Fully off canvas: both rects empty.
        let gone = TransformParams {
            pos_x: 2.0,
            ..Default::default()
        };
        let (d, s) = transformed_dst_rect(dst, src, 100, 100, &gone, 100, 100);
        assert!(
            d.is_empty() && s.is_empty(),
            "wholly off-canvas has nothing to draw"
        );
    }

    /// `pos_x`/`pos_y` are canvas fractions and a `transform` line takes any
    /// finite `f32`, so they are the one transform field with no bound on the
    /// way in -- `float()` (edith.rs) refuses only inf/NaN, and unlike
    /// `scale`, which this function caps at a thousand canvas widths precisely
    /// so its own arithmetic cannot overflow, the position was multiplied by
    /// the canvas and added to `dst.x` unchecked. Rust's `as i64` saturates, so
    /// a hostile 3e38 became `i64::MAX` and the *add* is what overflowed:
    /// a panic on the compositing thread in a debug build, a silently wrapped
    /// position in a release one. A placement pushed off the canvas is already
    /// a legal answer (the two empty rects above); a position past the point
    /// where the picture is long gone must give that same answer.
    #[test]
    fn a_transform_position_a_file_can_name_does_not_overflow_the_centre() {
        let dst = Rect {
            x: 10,
            y: 20,
            w: 100,
            h: 100,
        };
        let src = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        for (pos_x, pos_y) in [
            (3.0e38f32, 0.0),
            (-3.0e38, 0.0),
            (0.0, 3.0e38),
            (0.0, -3.0e38),
            (3.0e38, 3.0e38),
            (f32::MAX, f32::MIN),
        ] {
            let t = TransformParams {
                pos_x,
                pos_y,
                ..Default::default()
            };
            let (d, s) = transformed_dst_rect(dst, src, 100, 100, &t, 200, 200);
            // The answer is "nothing to draw" -- the box is a thousand canvas
            // widths off screen -- not an overflow and not a wrapped position
            // that lands back on the canvas.
            assert!(
                d.is_empty() && s.is_empty(),
                "pos ({pos_x}, {pos_y}) gave {d:?} / {s:?} rather than an empty rect pair"
            );
        }
    }

    /// [`nearest_90_steps`] rounds to the closest quarter turn and folds a
    /// non-finite value to the identity.
    #[test]
    fn nearest_90_steps_rounds_and_wraps() {
        assert_eq!(nearest_90_steps(0.0), 0);
        assert_eq!(nearest_90_steps(89.0), 1);
        assert_eq!(nearest_90_steps(91.0), 1);
        assert_eq!(nearest_90_steps(180.0), 2);
        assert_eq!(nearest_90_steps(-90.0), 3);
        assert_eq!(nearest_90_steps(360.0 + 90.0), 1);
        assert_eq!(nearest_90_steps(f32::NAN), 0);
    }

    /// A marked corner pixel lands where a clockwise quarter turn puts it:
    /// top-left to top-right at 90 degrees, to bottom-right at 180, to
    /// bottom-left at 270 -- and the plane's own shape swaps at 90 and 270
    /// and stays put at 180.
    #[test]
    fn rotate_i420_90s_places_a_marked_pixel() {
        let (w, h) = (4usize, 3usize);
        let mut y = vec![0u8; w * h];
        y[0] = 99; // (x=0, y=0), the top-left corner.
        let (u, v) = (vec![128u8; 2 * 2], vec![128u8; 2 * 2]);

        let mark_at = |plane: &[u8], w: u32, x: u32, y: u32| plane[(y * w + x) as usize];

        let (ry, _, _, rw, rh) = rotate_i420_90s(&y, &u, &v, w as u32, h as u32, 1);
        assert_eq!((rw, rh), (h as u32, w as u32), "dims swap at 90 degrees");
        assert_eq!(
            mark_at(&ry, rw, rw - 1, 0),
            99,
            "top-left moves to top-right"
        );

        let (ry, _, _, rw, rh) = rotate_i420_90s(&y, &u, &v, w as u32, h as u32, 2);
        assert_eq!(
            (rw, rh),
            (w as u32, h as u32),
            "dims stay put at 180 degrees"
        );
        assert_eq!(
            mark_at(&ry, rw, rw - 1, rh - 1),
            99,
            "top-left moves to bottom-right"
        );

        let (ry, _, _, rw, rh) = rotate_i420_90s(&y, &u, &v, w as u32, h as u32, 3);
        assert_eq!((rw, rh), (h as u32, w as u32), "dims swap at 270 degrees");
        assert_eq!(
            mark_at(&ry, rw, 0, rh - 1),
            99,
            "top-left moves to bottom-left"
        );

        // Zero steps, and four of them, are both the identity.
        let (ry, ru, rv, rw, rh) = rotate_i420_90s(&y, &u, &v, w as u32, h as u32, 0);
        assert_eq!(
            (ry, ru, rv, rw, rh),
            (y.clone(), u.clone(), v.clone(), w as u32, h as u32)
        );
        let (ry, _, _, rw, rh) = rotate_i420_90s(&y, &u, &v, w as u32, h as u32, 4);
        assert_eq!(
            (ry, rw, rh),
            (y, w as u32, h as u32),
            "four quarter turns is the identity"
        );
    }

    /// A 64x64 Y plane split down the middle: columns 0..32 are 200, 32..64
    /// are 40. A vertical edge to track through a horizontal move.
    fn vsplit_64() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..64 * 64)
            .map(|i| if i % 64 < 32 { 200u8 } else { 40u8 })
            .collect();
        (y, vec![NEUTRAL_C; 32 * 32], vec![NEUTRAL_C; 32 * 32])
    }

    /// Same split, but across rows: 0..32 are 200, 32..64 are 40 -- an edge
    /// to track through a vertical move.
    fn hsplit_64() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..64 * 64)
            .map(|i| if i / 64 < 32 { 200u8 } else { 40u8 })
            .collect();
        (y, vec![NEUTRAL_C; 32 * 32], vec![NEUTRAL_C; 32 * 32])
    }

    /// [`Composer::place_transformed`] at compose level, with the exact
    /// defect the geometry used to have: a positive `pos_x` squeezed the
    /// whole source into a shrunk box (content narrower, not just displaced)
    /// and a negative one rendered pixel-identical to the identity. These
    /// assert on the DISPLACED CONTENT (where the source's own edge lands),
    /// not on any rect field, so a squeeze that happens to leave rect sizes
    /// looking plausible cannot pass silently.
    #[test]
    fn place_transformed_displaces_content_not_just_a_rect() {
        let (src_w, src_h) = (64u32, 64u32);
        let (y, u, v) = vsplit_64();
        let mut canvas = Composer::new(src_w, src_h, FitPolicy::Fit);

        // Identity: the source's own edge stays at column 32, everywhere.
        let (iy, ..) =
            canvas.place_transformed(&y, &u, &v, src_w, src_h, &TransformParams::default());
        for row in 0..64 {
            let r = &iy[row * 64..][..64];
            assert!(
                r[..32].iter().all(|&p| p == 200) && r[32..].iter().all(|&p| p == 40),
                "row {row}: {r:?}"
            );
        }

        // +25%: the box moves right by 16 and does NOT shrink -- content
        // width stays 64 (the source's own 32/32 split), clipped to what
        // still fits. The edge (originally at box-column 32) now sits at
        // canvas column 16 + 32 = 48, not squeezed inward.
        let plus = TransformParams {
            pos_x: 0.25,
            ..Default::default()
        };
        let (py, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &plus);
        for row in 0..64 {
            let r = &py[row * 64..][..64];
            assert!(
                r[..16].iter().all(|&p| p == BLACK_Y),
                "row {row} left of box: {r:?}"
            );
            assert!(
                r[16..48].iter().all(|&p| p == 200),
                "row {row} squeezed inward: {r:?}"
            );
            assert!(
                r[48..].iter().all(|&p| p == 40),
                "row {row} edge moved: {r:?}"
            );
        }

        // -25%: mirror image -- this used to be pixel-identical to identity.
        let minus = TransformParams {
            pos_x: -0.25,
            ..Default::default()
        };
        let (ny, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &minus);
        assert_ne!(ny, iy, "a negative offset must move the picture, not no-op");
        for row in 0..64 {
            let r = &ny[row * 64..][..64];
            assert!(r[..16].iter().all(|&p| p == 200), "row {row}: {r:?}");
            assert!(r[16..48].iter().all(|&p| p == 40), "row {row}: {r:?}");
            assert!(
                r[48..].iter().all(|&p| p == BLACK_Y),
                "row {row} right of box: {r:?}"
            );
        }

        // Wholly off-canvas: nothing to draw, all black.
        let gone = TransformParams {
            pos_x: 2.0,
            ..Default::default()
        };
        let (gy, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &gone);
        assert!(
            gy.iter().all(|&p| p == BLACK_Y),
            "off-canvas must render background"
        );
    }

    /// The same defect on the vertical axis: `pos_y` moves content up/down,
    /// clipped, never squeezed.
    #[test]
    fn place_transformed_displaces_content_vertically() {
        let (src_w, src_h) = (64u32, 64u32);
        let (y, u, v) = hsplit_64();
        let mut canvas = Composer::new(src_w, src_h, FitPolicy::Fit);

        let plus = TransformParams {
            pos_y: 0.25,
            ..Default::default()
        };
        let (py, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &plus);
        for row in 0..16 {
            assert!(
                py[row * 64..][..64].iter().all(|&p| p == BLACK_Y),
                "row {row} above the box"
            );
        }
        for row in 16..48 {
            assert!(
                py[row * 64..][..64].iter().all(|&p| p == 200),
                "row {row} squeezed: {:?}",
                &py[row * 64..][..4]
            );
        }
        for row in 48..64 {
            assert!(
                py[row * 64..][..64].iter().all(|&p| p == 40),
                "row {row} edge moved"
            );
        }

        let minus = TransformParams {
            pos_y: -0.25,
            ..Default::default()
        };
        let (ny, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &minus);
        for row in 0..16 {
            assert!(ny[row * 64..][..64].iter().all(|&p| p == 200), "row {row}");
        }
        for row in 16..48 {
            assert!(ny[row * 64..][..64].iter().all(|&p| p == 40), "row {row}");
        }
        for row in 48..64 {
            assert!(
                ny[row * 64..][..64].iter().all(|&p| p == BLACK_Y),
                "row {row} below the box"
            );
        }
    }

    /// Position and scale together: a halved box moved right must place its
    /// (also halved) content at the moved-and-scaled location, not squeeze
    /// the full source into whatever survived clamping.
    #[test]
    fn place_transformed_displaces_content_with_scale() {
        let (src_w, src_h) = (64u32, 64u32);
        let (y, u, v) = vsplit_64();
        let mut canvas = Composer::new(src_w, src_h, FitPolicy::Fit);
        // scale 0.5, pos_x +0.25: box becomes 32x32 at canvas (32, 16),
        // fully on-canvas. The source's column-32 edge, halved, sits at
        // box-local column 16, i.e. canvas column 32 + 16 = 48.
        let t = TransformParams {
            scale: 0.5,
            pos_x: 0.25,
            ..Default::default()
        };
        let (sy, ..) = canvas.place_transformed(&y, &u, &v, src_w, src_h, &t);
        for row in 0..64 {
            let r = &sy[row * 64..][..64];
            let inside = (16..48).contains(&row);
            let want_bg = if inside { r[0] } else { BLACK_Y };
            assert_eq!(want_bg, BLACK_Y, "row {row} col 0 outside the box: {r:?}");
            if inside {
                assert!(
                    r[40].abs_diff(200) <= 1,
                    "row {row} col 40 should be the left half: {r:?}"
                );
                assert!(
                    r[56].abs_diff(40) <= 1,
                    "row {row} col 56 should be the right half: {r:?}"
                );
            }
        }
    }
}
