//! I420 -> BGRA8 (straight alpha), in the matrix and range the *stream* says
//! its samples were coded against (see [`crate::colorspace`]).

use crate::colorspace::{ColorDescription, Matrix};

/// One stream's YUV->RGB matrix as 8.8 fixed point, derived once per frame so
/// the loop below stays the multiply-and-shift it always was.
struct Coeffs {
    /// What luma is measured from: 16 for limited range, 0 for full.
    y_off: i32,
    y: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

impl Coeffs {
    /// The inverse of the matrix itself, out of its two luma weights (ITU-R
    /// BT.601 §2.5.1, BT.709 §3, BT.2020 Table 4), with `Kg = 1 - Kr - Kb`:
    ///
    /// ```text
    /// R = Y                    + 2(1 - Kr)          Cr
    /// G = Y - 2Kb(1 - Kb)/Kg Cb - 2Kr(1 - Kr)/Kg    Cr
    /// B = Y + 2(1 - Kb)      Cb
    /// ```
    ///
    /// where `Y` is 0..1 and `Cb`/`Cr` are -0.5..0.5. What turns bytes into
    /// those is the *range* (ITU-T H.273 §5.4): limited-range luma spans codes
    /// 16..235 and chroma 16..240, so a code is worth 255/219 and 255/224 of
    /// the scale respectively; full-range samples span the whole byte and both
    /// scales are 1, which is what a JFIF still or a `-color_range pc` encode
    /// carries.
    ///
    /// BT.601 limited comes out as exactly the 298/409/-100/-208/516 this
    /// engine hardcoded before it read a single colour tag, so SD material is
    /// byte for byte the picture it always was (`bt601_limited_is_the_legacy_matrix`).
    fn new(color: &ColorDescription) -> Self {
        // tonemap lands here: a PQ or HLG stream is converted with its own
        // matrix and then shown as if its curve were SDR -- which is what this
        // engine did with it before, only in the wrong matrix. The curve is a
        // pass over the RGB coming out of the loop, not a coefficient.
        let (kr, kb) = match color.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020Ncl => (0.2627, 0.0593),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = if color.full_range {
            (0, 1.0, 1.0)
        } else {
            (16, 255.0 / 219.0, 255.0 / 224.0)
        };
        let q = |v: f64| (v * 256.0).round() as i32;
        Self {
            y_off,
            y: q(y_scale),
            rv: q(2.0 * (1.0 - kr) * c_scale),
            gu: q(2.0 * kb * (1.0 - kb) / kg * c_scale),
            gv: q(2.0 * kr * (1.0 - kr) / kg * c_scale),
            bu: q(2.0 * (1.0 - kb) * c_scale),
        }
    }
}

/// One row band's worth of the conversion: `rows` are absolute row indices
/// into `y`/`u`/`v` (chroma read at half resolution as ever), written into
/// `out` from its own start (`out[0]` is the first row in `rows`). Pulled out
/// of `i420_to_bgra`/`i420_to_bgra_with` so both can run it across disjoint
/// row ranges on separate threads -- each row is independent (chroma only
/// ever reads `row / 2`, never a neighbouring output row), so splitting the
/// row loop changes nothing about what a pixel comes out as.
fn convert_rows(
    k: &Coeffs,
    lut: Option<&crate::color::Lut>,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    cw: usize,
    rows: std::ops::Range<usize>,
    out: &mut [u8],
) {
    for (i, row) in rows.enumerate() {
        let y_row = row * width;
        let c_row = (row / 2) * cw;
        let o_row = i * width * 4;
        for col in 0..width {
            let ci = c_row + col / 2;
            let (c, d, e) = match lut {
                Some(lut) => (
                    lut.y[y[y_row + col] as usize] as i32 - k.y_off,
                    lut.u[u[ci] as usize] as i32 - 128,
                    lut.v[v[ci] as usize] as i32 - 128,
                ),
                None => (
                    y[y_row + col] as i32 - k.y_off,
                    u[ci] as i32 - 128,
                    v[ci] as i32 - 128,
                ),
            };

            let r = ((k.y * c + k.rv * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((k.y * c - k.gu * d - k.gv * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((k.y * c + k.bu * d + 128) >> 8).clamp(0, 255) as u8;

            let o = o_row + col * 4;
            out[o] = b;
            out[o + 1] = g;
            out[o + 2] = r;
            out[o + 3] = 255;
        }
    }
}

/// `convert_rows`, fanned out over as many threads as the machine has: rows
/// split into one contiguous band per lane (`std::thread::scope`, the same
/// pattern `HevcEnc::code_batch` uses, borrowing the planes rather than
/// copying them), each lane writing straight into its own slice of `out`. A
/// frame too short to be worth the spawn (or a machine that only reports one
/// core) falls back to running the whole thing on the calling thread.
fn convert_parallel(
    k: &Coeffs,
    lut: Option<&crate::color::Lut>,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
) -> Vec<u8> {
    let cw = width.div_ceil(2);
    let mut out = vec![0u8; width * height * 4];
    let lanes = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(height);
    if lanes <= 1 {
        convert_rows(k, lut, y, u, v, width, cw, 0..height, &mut out);
        return out;
    }
    let rows_per_lane = height.div_ceil(lanes);
    std::thread::scope(|scope| {
        let mut rest = &mut out[..];
        let mut start = 0;
        while start < height {
            let end = (start + rows_per_lane).min(height);
            let (chunk, remainder) = rest.split_at_mut((end - start) * width * 4);
            rest = remainder;
            scope.spawn(move || convert_rows(k, lut, y, u, v, width, cw, start..end, chunk));
            start = end;
        }
    });
    out
}

/// Converts a planar I420 frame (stride == width, chroma at half resolution)
/// into tightly packed BGRA8. Returns `width * height * 4` bytes.
pub fn i420_to_bgra(
    color: &ColorDescription,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
) -> Vec<u8> {
    let k = Coeffs::new(color);
    convert_parallel(&k, None, y, u, v, width, height)
}

/// The same conversion with a segment's colour grade folded in: the grade is
/// three table lookups per pixel inside the loop that was reading those samples
/// anyway, and measured free: 4.73 ms/frame against 4.74 for the ungraded
/// conversion, where grading a scratch copy of the planes first costs a further
/// 0.61 (1920x1080, release, `perf_1080p` below).
///
/// Identity params take the ungraded loop above, byte for byte.
pub fn i420_to_bgra_with(
    color: &ColorDescription,
    params: &crate::color::ColorParams,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
) -> Vec<u8> {
    if params.is_identity() {
        return i420_to_bgra(color, y, u, v, width, height);
    }
    let k = Coeffs::new(color);
    let lut = crate::color::Lut::new(params);
    convert_parallel(&k, Some(&lut), y, u, v, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorParams;

    /// BT.601 limited: what the tests that predate the colour tags measured,
    /// and what they have to keep measuring.
    const SD: ColorDescription = ColorDescription {
        matrix: Matrix::Bt601,
        transfer: crate::colorspace::Transfer::Sdr,
        full_range: false,
    };

    /// Limited-range BT.601, the one every stream was converted as before the
    /// colour tags were read: the derivation has to land on the exact integers
    /// that were hardcoded, or SD material shifts under a change that was only
    /// ever meant to leave it alone.
    #[test]
    fn bt601_limited_is_the_legacy_matrix() {
        let k = Coeffs::new(&ColorDescription::default());
        assert_eq!(
            (k.y_off, k.y, k.rv, k.gu, k.gv, k.bu),
            (16, 298, 409, 100, 208, 516)
        );
    }

    /// The render path's half of the charter invariant: an ungraded segment is
    /// the untouched converter, not a grade that happens to round back.
    #[test]
    fn identity_grade_converts_identically() {
        let y: Vec<u8> = (0..64 * 48).map(|i| (i % 256) as u8).collect();
        let u: Vec<u8> = (0..32 * 24).map(|i| (i % 256) as u8).collect();
        let v: Vec<u8> = (0..32 * 24).map(|i| (255 - i % 256) as u8).collect();
        assert_eq!(
            i420_to_bgra_with(&SD, &ColorParams::default(), &y, &u, &v, 64, 48),
            i420_to_bgra(&SD, &y, &u, &v, 64, 48)
        );
    }

    /// Fused must equal grading the planes first and converting after -- that is
    /// what makes the export path (which grades a scratch copy before the
    /// encoder) and the render path show the same picture.
    #[test]
    fn fused_matches_grade_then_convert() {
        let params = ColorParams {
            brightness: 0.1,
            contrast: 1.3,
            saturation: 0.4,
            tint: 0.25,
        };
        let mut y: Vec<u8> = (0..64 * 48).map(|i| (i % 256) as u8).collect();
        let mut u: Vec<u8> = (0..32 * 24).map(|i| (i % 256) as u8).collect();
        let mut v: Vec<u8> = (0..32 * 24).map(|i| (255 - i % 256) as u8).collect();
        let fused = i420_to_bgra_with(&SD, &params, &y, &u, &v, 64, 48);
        crate::color::apply_yuv(&params, &mut y, &mut u, &mut v);
        assert_eq!(fused, i420_to_bgra(&SD, &y, &u, &v, 64, 48));
    }

    /// Not asserted, printed: the numbers in `i420_to_bgra_with`'s docs come
    /// from here. `cargo test -p engine --release convert:: -- --nocapture`.
    #[test]
    fn perf_1080p() {
        let (w, h) = (1920usize, 1080usize);
        let y: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        let u: Vec<u8> = (0..w / 2 * h / 2).map(|i| (i % 256) as u8).collect();
        let v: Vec<u8> = (0..w / 2 * h / 2).map(|i| (255 - i % 256) as u8).collect();
        let params = ColorParams {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 1.3,
            tint: 0.2,
        };
        let runs = 20;
        let bench = |name: &str, mut f: Box<dyn FnMut()>| {
            let t = std::time::Instant::now();
            for _ in 0..runs {
                f();
            }
            println!(
                "{name} 1920x1080: {:.3} ms/frame",
                t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs)
            );
        };
        bench(
            "i420_to_bgra       ",
            Box::new(|| {
                std::hint::black_box(i420_to_bgra(&SD, &y, &u, &v, w, h));
            }),
        );
        bench(
            "i420_to_bgra_with  ",
            Box::new(|| {
                std::hint::black_box(i420_to_bgra_with(&SD, &params, &y, &u, &v, w, h));
            }),
        );
        bench(
            "copy + apply_yuv   ",
            Box::new(|| {
                let (mut y, mut u, mut v) = (y.clone(), u.clone(), v.clone());
                crate::color::apply_yuv(&params, &mut y, &mut u, &mut v);
                std::hint::black_box((y, u, v));
            }),
        );
    }

    fn desc(matrix: Matrix, full_range: bool) -> ColorDescription {
        ColorDescription {
            matrix,
            full_range,
            ..ColorDescription::default()
        }
    }

    /// Each matrix against its own red primary -- the sample the matrices
    /// disagree about most, and the one a wrong matrix turns visibly orange --
    /// plus the two neutrals, which is where a wrong *range* shows instead.
    #[test]
    fn known_colors() {
        // 2x2 blocks so each shares one chroma sample.
        let cases: [(ColorDescription, [u8; 3], [u8; 3]); 9] = [
            // Limited range: black is Y=16, white Y=235.
            (desc(Matrix::Bt601, false), [16, 128, 128], [0, 0, 0]),
            (desc(Matrix::Bt601, false), [235, 128, 128], [255, 255, 255]),
            (desc(Matrix::Bt601, false), [82, 90, 240], [255, 0, 0]),
            (desc(Matrix::Bt709, false), [16, 128, 128], [0, 0, 0]),
            (desc(Matrix::Bt709, false), [235, 128, 128], [255, 255, 255]),
            // BT.709's red primary sits at a *lower* luma than BT.601's (0.2126
            // against 0.299): read with the 601 matrix it comes out dark.
            (desc(Matrix::Bt709, false), [63, 102, 240], [255, 0, 0]),
            (desc(Matrix::Bt2020Ncl, false), [74, 97, 240], [255, 0, 0]),
            // Full range: no headroom, so Y=16 is a dark grey rather than black
            // -- the case a limited-range conversion crushes to 0.
            (desc(Matrix::Bt601, true), [16, 128, 128], [16, 16, 16]),
            (desc(Matrix::Bt601, true), [76, 85, 255], [255, 0, 0]),
        ];
        for (color, [yv, uv, vv], [r, g, b]) in cases {
            let out = i420_to_bgra(&color, &[yv; 4], &[uv], &[vv], 2, 2);
            assert_eq!(out.len(), 2 * 2 * 4);
            for px in out.chunks_exact(4) {
                assert!(px[0].abs_diff(b) <= 2, "B {} vs {}", px[0], b);
                assert!(px[1].abs_diff(g) <= 2, "G {} vs {}", px[1], g);
                assert!(px[2].abs_diff(r) <= 2, "R {} vs {}", px[2], r);
                assert_eq!(px[3], 255);
            }
        }
    }
}
