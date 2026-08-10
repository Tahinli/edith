//! Per-segment colour transform, applied engine-side on the decoded YUV samples
//! so that playback and export produce the same picture from the same numbers.
//!
//! The maths lives on YUV rather than RGB because every path already has YUV in
//! hand -- the decoders hand back I420 and the encoders take it -- and because
//! the four controls are each a straight per-sample affine there:
//!
//! * brightness and contrast move Y alone (contrast pivots on limited-range mid
//!   grey, `(16 + 235) / 2`; brightness shifts by a fraction of the 219-code
//!   luma range, so `1.0` lifts black to white),
//! * saturation scales U and V about 128, so `0.0` is exactly grey,
//! * tint is a warm/cool *temperature* axis, not a magenta/green one: U carries
//!   blue-difference and V red-difference, so warm (positive) pushes V up and U
//!   down by the same amount. A hue rotation would need a 2x2 matrix on (U,V)
//!   and buys nothing a colourist asks for by name.
//!
//! Every control therefore collapses into three 256-entry lookup tables built
//! once per call, and the pixel loop is a byte indexing a table -- no float per
//! pixel, no chance of a `u8` wrap. Identity params short-circuit before the
//! tables exist, which is what makes "no colour set" byte-identical rather than
//! merely close.

/// Limited-range mid grey; contrast pivots here so a change does not also
/// shift the picture's overall level.
const MID_Y: f32 = 125.5;
/// Codes between limited-range black (16) and white (235).
const LUMA_RANGE: f32 = 219.0;
/// Chroma codes a full-warm or full-cool tint moves U and V by. Picked so the
/// extreme is a strong grade rather than a broken picture.
const TINT_RANGE: f32 = 32.0;

/// One segment's colour setting. Plain data with no invariants: out-of-range or
/// non-finite values are clamped where the tables are built, so a deserializer
/// can hand this over unchecked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorParams {
    /// -1..1, 0 = identity. 1.0 lifts limited-range black to white.
    pub brightness: f32,
    /// 0..2, 1 = identity. 0 flattens the picture to mid grey.
    pub contrast: f32,
    /// 0..2, 1 = identity. 0 is greyscale, 2 is doubled chroma.
    pub saturation: f32,
    /// -1..1, 0 = identity. Positive is warm (more red, less blue).
    pub tint: f32,
}

impl Default for ColorParams {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            tint: 0.0,
        }
    }
}

impl ColorParams {
    /// Whether this setting is the do-nothing one. Compared exactly on purpose:
    /// anything that is not the identity value goes through the transform, and
    /// only a `true` here is allowed to skip it.
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 1.0 && self.saturation == 1.0 && self.tint == 0.0
    }
}

/// The three per-plane tables. Built once per frame (768 float ops against ~3 M
/// samples at 1080p, so the build never shows up in a measurement).
pub(crate) struct Lut {
    pub(crate) y: [u8; 256],
    pub(crate) u: [u8; 256],
    pub(crate) v: [u8; 256],
}

impl Lut {
    pub(crate) fn new(params: &ColorParams) -> Self {
        let brightness = sane(params.brightness, 0.0).clamp(-1.0, 1.0) * LUMA_RANGE;
        let contrast = sane(params.contrast, 1.0).clamp(0.0, 2.0);
        let saturation = sane(params.saturation, 1.0).clamp(0.0, 2.0);
        let tint = sane(params.tint, 0.0).clamp(-1.0, 1.0) * TINT_RANGE;

        let mut lut = Self {
            y: [0; 256],
            u: [0; 256],
            v: [0; 256],
        };
        for code in 0..256 {
            let f = code as f32;
            // Clamped to the full 0..255 swing, not to 16..235: content that is
            // already blacker than black or whiter than white keeps whatever a
            // grade leaves of it instead of being crushed by this pass.
            lut.y[code] = byte((f - MID_Y) * contrast + MID_Y + brightness);
            lut.u[code] = byte((f - 128.0) * saturation + 128.0 - tint);
            lut.v[code] = byte((f - 128.0) * saturation + 128.0 + tint);
        }
        lut
    }
}

/// A parameter that survived a file or a UI edit; NaN and infinity fall back to
/// the identity value rather than blanking the picture.
fn sane(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// Applies `params` in place to one planar 8-bit frame.
///
/// Per-sample, so the plane layout is free: I420 with `stride == width` (what
/// [`crate::convert`] and the export path use) or any other planar 8-bit YUV,
/// padding included. Interleaved chroma (NV12) is *not* this function's frame.
///
/// Identity params return without touching a byte.
pub fn apply_yuv(params: &ColorParams, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    if params.is_identity() {
        return;
    }
    let lut = Lut::new(params);
    for s in y.iter_mut() {
        *s = lut.y[*s as usize];
    }
    for s in u.iter_mut() {
        *s = lut.u[*s as usize];
    }
    for s in v.iter_mut() {
        *s = lut.v[*s as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient frame with every luma code and a chroma sweep, so a test that
    /// walks it has seen the whole table.
    fn frame(w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..w * h).map(|i| (i % 256) as u8).collect();
        let cw = w.div_ceil(2) * h.div_ceil(2);
        let u = (0..cw).map(|i| (i % 256) as u8).collect();
        let v = (0..cw).map(|i| (255 - i % 256) as u8).collect();
        (y, u, v)
    }

    /// The charter invariant: no colour set means the exact same bytes, not
    /// bytes that round back to the same picture.
    #[test]
    fn identity_params_leave_every_byte_alone() {
        let (y, u, v) = frame(64, 48);
        let (mut gy, mut gu, mut gv) = (y.clone(), u.clone(), v.clone());
        apply_yuv(&ColorParams::default(), &mut gy, &mut gu, &mut gv);
        assert_eq!((gy, gu, gv), (y, u, v));
    }

    #[test]
    fn zero_saturation_is_grey_everywhere() {
        let (mut y, mut u, mut v) = frame(64, 48);
        let before = y.clone();
        apply_yuv(
            &ColorParams {
                saturation: 0.0,
                ..Default::default()
            },
            &mut y,
            &mut u,
            &mut v,
        );
        assert_eq!(y, before, "saturation must not touch luma");
        assert!(u.iter().chain(v.iter()).all(|&c| c == 128), "not grey");
    }

    /// Every extreme, on every code, stays inside `u8` -- a wrap would show as
    /// black specks in a blown highlight.
    #[test]
    fn extremes_clamp_instead_of_wrapping() {
        let extremes = [-1.0f32, 1.0, -100.0, 100.0, f32::NAN, f32::INFINITY];
        for b in extremes {
            for c in [0.0f32, 2.0, 50.0] {
                let lut = Lut::new(&ColorParams {
                    brightness: b,
                    contrast: c,
                    saturation: c,
                    tint: b,
                });
                // A wrap shows up as a curve that falls: every one of these is
                // non-decreasing in the input code, saturated ends included.
                assert!(
                    lut.y.windows(2).all(|w| w[0] <= w[1]),
                    "Y wrapped at {b}/{c}"
                );
                assert!(
                    lut.u.windows(2).all(|w| w[0] <= w[1]),
                    "U wrapped at {b}/{c}"
                );
                assert!(
                    lut.v.windows(2).all(|w| w[0] <= w[1]),
                    "V wrapped at {b}/{c}"
                );
            }
        }
        // The two saturating ends specifically: +219 on code 255 is 474, which
        // a wrapping cast would hand back as 218.
        let lut = Lut::new(&ColorParams {
            brightness: 1.0,
            ..Default::default()
        });
        assert_eq!(lut.y[255], 255, "full brightness must saturate, not wrap");
        assert_eq!(lut.y[0], 219, "black lifted by the whole luma range");
        let lut = Lut::new(&ColorParams {
            brightness: -1.0,
            ..Default::default()
        });
        assert_eq!(lut.y[255], 36, "255 - 219");
    }

    #[test]
    fn brightness_is_monotonic_in_mean_luma() {
        let (y0, u0, v0) = frame(64, 48);
        let mut last = 0.0;
        for step in 0..=8 {
            let (mut y, mut u, mut v) = (y0.clone(), u0.clone(), v0.clone());
            apply_yuv(
                &ColorParams {
                    brightness: -0.5 + step as f32 * 0.125,
                    ..Default::default()
                },
                &mut y,
                &mut u,
                &mut v,
            );
            let mean = y.iter().map(|&s| s as f64).sum::<f64>() / y.len() as f64;
            assert!(mean >= last, "brighter setting darkened the frame");
            last = mean;
        }
    }

    /// The tables are the float maths, sampled: this is what lets the pixel loop
    /// be a byte lookup. Same formula, computed per sample, must agree exactly.
    #[test]
    fn tables_agree_with_the_float_maths() {
        let params = ColorParams {
            brightness: 0.13,
            contrast: 1.4,
            saturation: 0.7,
            tint: -0.35,
        };
        let lut = Lut::new(&params);
        for code in 0..256 {
            let f = code as f32;
            let y = ((f - MID_Y) * 1.4 + MID_Y + 0.13 * LUMA_RANGE)
                .round()
                .clamp(0.0, 255.0) as u8;
            let u = ((f - 128.0) * 0.7 + 128.0 + 0.35 * TINT_RANGE)
                .round()
                .clamp(0.0, 255.0) as u8;
            let v = ((f - 128.0) * 0.7 + 128.0 - 0.35 * TINT_RANGE)
                .round()
                .clamp(0.0, 255.0) as u8;
            assert!(lut.y[code].abs_diff(y) <= 1, "Y {code}");
            assert!(lut.u[code].abs_diff(u) <= 1, "U {code}");
            assert!(lut.v[code].abs_diff(v) <= 1, "V {code}");
        }
    }

    /// Not asserted: a debug build is an order of magnitude off and the number
    /// only means something in release. Run with
    /// `cargo test -p engine --release color:: -- --nocapture`.
    #[test]
    fn perf_1080p() {
        let (mut y, mut u, mut v) = frame(1920, 1080);
        let params = ColorParams {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 1.3,
            tint: 0.2,
        };
        let runs = 30;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            apply_yuv(&params, &mut y, &mut u, &mut v);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("color::apply_yuv 1920x1080: {ms:.3} ms/frame");
    }
}
