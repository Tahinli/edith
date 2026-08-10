//! I420 -> BGRA8 (straight alpha), BT.601 limited range.

/// Converts a planar I420 frame (stride == width, chroma at half resolution)
/// into tightly packed BGRA8. Returns `width * height * 4` bytes.
pub fn i420_to_bgra(y: &[u8], u: &[u8], v: &[u8], width: usize, height: usize) -> Vec<u8> {
    let cw = width.div_ceil(2);
    let mut out = vec![0u8; width * height * 4];

    for row in 0..height {
        let y_row = row * width;
        let c_row = (row / 2) * cw;
        for col in 0..width {
            let c = y[y_row + col] as i32 - 16;
            let ci = c_row + col / 2;
            let d = u[ci] as i32 - 128;
            let e = v[ci] as i32 - 128;

            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;

            let o = (y_row + col) * 4;
            out[o] = b;
            out[o + 1] = g;
            out[o + 2] = r;
            out[o + 3] = 255;
        }
    }
    out
}

/// The same conversion with a segment's colour grade folded in: the grade is
/// three table lookups per pixel inside the loop that was reading those samples
/// anyway, and measured free: 4.73 ms/frame against 4.74 for the ungraded
/// conversion, where grading a scratch copy of the planes first costs a further
/// 0.61 (1920x1080, release, `perf_1080p` below).
///
/// Identity params take the ungraded loop above, byte for byte.
pub fn i420_to_bgra_with(
    params: &crate::color::ColorParams,
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
) -> Vec<u8> {
    if params.is_identity() {
        return i420_to_bgra(y, u, v, width, height);
    }
    let lut = crate::color::Lut::new(params);
    let cw = width.div_ceil(2);
    let mut out = vec![0u8; width * height * 4];

    for row in 0..height {
        let y_row = row * width;
        let c_row = (row / 2) * cw;
        for col in 0..width {
            let c = lut.y[y[y_row + col] as usize] as i32 - 16;
            let ci = c_row + col / 2;
            let d = lut.u[u[ci] as usize] as i32 - 128;
            let e = lut.v[v[ci] as usize] as i32 - 128;

            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;

            let o = (y_row + col) * 4;
            out[o] = b;
            out[o + 1] = g;
            out[o + 2] = r;
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorParams;

    /// The render path's half of the charter invariant: an ungraded segment is
    /// the untouched converter, not a grade that happens to round back.
    #[test]
    fn identity_grade_converts_identically() {
        let y: Vec<u8> = (0..64 * 48).map(|i| (i % 256) as u8).collect();
        let u: Vec<u8> = (0..32 * 24).map(|i| (i % 256) as u8).collect();
        let v: Vec<u8> = (0..32 * 24).map(|i| (255 - i % 256) as u8).collect();
        assert_eq!(
            i420_to_bgra_with(&ColorParams::default(), &y, &u, &v, 64, 48),
            i420_to_bgra(&y, &u, &v, 64, 48)
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
        let fused = i420_to_bgra_with(&params, &y, &u, &v, 64, 48);
        crate::color::apply_yuv(&params, &mut y, &mut u, &mut v);
        assert_eq!(fused, i420_to_bgra(&y, &u, &v, 64, 48));
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
                std::hint::black_box(i420_to_bgra(&y, &u, &v, w, h));
            }),
        );
        bench(
            "i420_to_bgra_with  ",
            Box::new(|| {
                std::hint::black_box(i420_to_bgra_with(&params, &y, &u, &v, w, h));
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

    #[test]
    fn known_colors() {
        // 2x2 blocks so each shares one chroma sample.
        // Black (limited range Y=16), white (Y=235), and a saturated red.
        let cases: [([u8; 3], [u8; 3]); 3] = [
            ([16, 128, 128], [0, 0, 0]),
            ([235, 128, 128], [255, 255, 255]),
            ([82, 90, 240], [255, 0, 0]), // BT.601 red primary -> R=255,G~0,B~0
        ];
        for ([yv, uv, vv], [r, g, b]) in cases {
            let out = i420_to_bgra(&[yv; 4], &[uv], &[vv], 2, 2);
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
