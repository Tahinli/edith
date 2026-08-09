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

#[cfg(test)]
mod tests {
    use super::*;

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
