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

/// Limited-range black: what an untouched canvas pixel is.
pub const BLACK_Y: u8 = 16;
/// Neutral chroma: 128 in both planes.
pub const NEUTRAL_C: u8 = 128;

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
    let (cw, ch) = chroma_dims(w, h);
    (
        vec![BLACK_Y; w * h],
        vec![NEUTRAL_C; cw * ch],
        vec![NEUTRAL_C; cw * ch],
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
pub fn scale_i420(
    src_y: &[u8],
    src_u: &[u8],
    src_v: &[u8],
    src_w: usize,
    src_h: usize,
    dst_y: &mut [u8],
    dst_u: &mut [u8],
    dst_v: &mut [u8],
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
fn plane(src: &[u8], src_w: usize, src_h: usize, dst: &mut [u8], dst_w: usize, dst_h: usize) {
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
            let a = (u32::from(top[x0]) * inv_wx + u32::from(top[x1]) * wx + HALF) >> 16;
            let b = (u32::from(bottom[x0]) * inv_wx + u32::from(bottom[x1]) * wx + HALF) >> 16;
            *o = ((a * inv_wy + b * wy + HALF) >> 16) as u8;
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
pub fn compose_i420(
    canvas_y: &mut [u8],
    canvas_u: &mut [u8],
    canvas_v: &mut [u8],
    canvas_w: usize,
    canvas_h: usize,
    frame_y: &[u8],
    frame_u: &[u8],
    frame_v: &[u8],
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

    /// Not asserted: only the release number means anything. Run with
    /// `cargo test -p engine --release scale::tests::perf -- --nocapture`.
    #[test]
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
}
