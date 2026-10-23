//! Video scopes of one picture: a luma waveform, an RGB parade and a
//! vectorscope, counted off a BGRA frame at low resolution, plus a compact
//! numeric summary of them for a reader that cannot look at a picture.
//!
//! Counted from a strided grid of at most [`MAX_SAMPLES`] pixels: a scope's
//! shape is the same from sixteen thousand samples as from two million, and at
//! that count the whole pass is well under a millisecond, cheap enough to run
//! on every preview frame on the thread that received it.

/// Pixels read per frame, at most.
pub const MAX_SAMPLES: usize = 16_384;
/// Rows of the waveform and parade: display levels 0..100 % in this many steps.
pub const LEVELS: usize = 64;
/// Cells per side of the vectorscope grid.
pub const VECTOR: usize = 64;

/// One frame's scopes. Every grid is row-major counts; level 0 is black.
#[derive(Debug, Clone, PartialEq)]
pub struct Scopes {
    /// Columns of the waveform and of each parade channel.
    pub cols: usize,
    /// `cols * LEVELS`: luma counts, `[level * cols + col]`.
    pub wave: Vec<u32>,
    /// Red, green, blue, each `cols * LEVELS` as `wave`.
    pub parade: [Vec<u32>; 3],
    /// `VECTOR * VECTOR` counts of (Cb, Cr): `[row * VECTOR + col]`, Cb along
    /// the columns (blue right), Cr up the rows (red at the top, row 0).
    pub vector: Vec<u32>,
    /// 256-bin histograms of luma, red, green, blue (display codes).
    hist: [[u32; 256]; 4],
    /// Sum of Cb and Cr, and of chroma magnitude, over the samples.
    chroma_sum: [f64; 3],
    /// Samples counted.
    pub samples: u32,
}

/// Rec. 709 luma of display-code R, G, B.
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Counts the scopes of a `width` x `height` BGRA frame into `cols` columns.
pub fn compute(bgra: &[u8], width: usize, height: usize, cols: usize) -> Scopes {
    let cols = cols.max(1);
    let mut s = Scopes {
        cols,
        wave: vec![0; cols * LEVELS],
        parade: [vec![0; cols * LEVELS], vec![0; cols * LEVELS], vec![0; cols * LEVELS]],
        vector: vec![0; VECTOR * VECTOR],
        hist: [[0; 256]; 4],
        chroma_sum: [0.0; 3],
        samples: 0,
    };
    if width == 0 || height == 0 || bgra.len() < width * height * 4 {
        return s;
    }
    // A square-ish grid: the same step across and down.
    let step = ((width * height) as f64 / MAX_SAMPLES as f64).sqrt().ceil().max(1.0) as usize;
    let level = |v: f32| ((v / 255.0 * LEVELS as f32) as usize).min(LEVELS - 1);
    for y in (0..height).step_by(step) {
        for x in (0..width).step_by(step) {
            let p = &bgra[(y * width + x) * 4..];
            let (b, g, r) = (f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
            let l = luma(r, g, b);
            let col = x * cols / width;
            s.wave[level(l) * cols + col] += 1;
            for (c, v) in [r, g, b].into_iter().enumerate() {
                s.parade[c][level(v) * cols + col] += 1;
                s.hist[c + 1][v as usize] += 1;
            }
            s.hist[0][(l.round() as usize).min(255)] += 1;
            let cb = (b - l) / 1.8556 / 255.0;
            let cr = (r - l) / 1.5748 / 255.0;
            let cell = |v: f32| (((v + 0.5) * VECTOR as f32) as usize).min(VECTOR - 1);
            s.vector[(VECTOR - 1 - cell(cr)) * VECTOR + cell(cb)] += 1;
            s.chroma_sum[0] += f64::from(cb);
            s.chroma_sum[1] += f64::from(cr);
            s.chroma_sum[2] += f64::from((cb * cb + cr * cr).sqrt());
            s.samples += 1;
        }
    }
    s
}

/// Five points of one channel's spread, in display percent (0 black, 100 white).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    pub min: f32,
    pub p5: f32,
    pub median: f32,
    pub p95: f32,
    pub max: f32,
    pub mean: f32,
}

/// What the scopes say, in numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Luma, then red, green, blue.
    pub spread: [Spread; 4],
    /// Share of samples at or under 2 % luma, and at or over 98 %, in percent.
    pub crushed: f32,
    pub clipped: f32,
    /// The waveform left to right in `bands` bands: luma p5, median, p95 (percent).
    pub bands: Vec<[f32; 3]>,
    /// Mean chroma magnitude (percent of the 709 chroma range, 0 for grey).
    pub saturation: f32,
    /// The direction of the mean chroma, degrees on the vectorscope (0 is +Cb,
    /// blue; 90 is +Cr, red), and how far it sits off centre (percent): a cast.
    pub cast_angle: f32,
    pub cast: f32,
}

fn pct(i: usize) -> f32 {
    i as f32 / 255.0 * 100.0
}

fn spread(h: &[u32; 256], n: u32) -> Spread {
    let at = |q: f64| {
        let want = (q * f64::from(n)).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (i, &c) in h.iter().enumerate() {
            acc += u64::from(c);
            if acc >= want {
                return pct(i);
            }
        }
        100.0
    };
    let mean = h.iter().enumerate().map(|(i, &c)| i as f64 * f64::from(c)).sum::<f64>() / f64::from(n.max(1));
    Spread {
        min: h.iter().position(|&c| c > 0).map_or(0.0, pct),
        p5: at(0.05),
        median: at(0.5),
        p95: at(0.95),
        max: h.iter().rposition(|&c| c > 0).map_or(0.0, pct),
        mean: (mean / 255.0 * 100.0) as f32,
    }
}

impl Scopes {
    /// The numbers, with the waveform split into `bands` bands left to right.
    pub fn summary(&self, bands: usize) -> Summary {
        let n = self.samples.max(1);
        let spread = [0, 1, 2, 3].map(|c| spread(&self.hist[c], self.samples));
        let share = |r: std::ops::RangeInclusive<usize>| {
            self.hist[0][r].iter().map(|&c| f64::from(c)).sum::<f64>() as f32 / n as f32 * 100.0
        };
        let bands = bands.clamp(1, self.cols);
        let band_rows: Vec<[f32; 3]> = (0..bands)
            .map(|b| {
                let (c0, c1) = (b * self.cols / bands, (b + 1) * self.cols / bands);
                let mut h = vec![0u32; LEVELS];
                for (lvl, slot) in h.iter_mut().enumerate() {
                    *slot = (c0..c1).map(|c| self.wave[lvl * self.cols + c]).sum();
                }
                let total: u32 = h.iter().sum();
                let at = |q: f32| {
                    let want = (q * total as f32).ceil().max(1.0) as u32;
                    let mut acc = 0;
                    for (lvl, &c) in h.iter().enumerate() {
                        acc += c;
                        if acc >= want {
                            return (lvl as f32 + 0.5) / LEVELS as f32 * 100.0;
                        }
                    }
                    100.0
                };
                [at(0.05), at(0.5), at(0.95)]
            })
            .collect();
        let (cb, cr) = (self.chroma_sum[0] / f64::from(n), self.chroma_sum[1] / f64::from(n));
        Summary {
            spread,
            crushed: share(0..=5),
            clipped: share(250..=255),
            bands: band_rows,
            saturation: (self.chroma_sum[2] / f64::from(n) * 200.0) as f32,
            cast_angle: (cr.atan2(cb).to_degrees() as f32).rem_euclid(360.0),
            cast: ((cb * cb + cr * cr).sqrt() * 200.0) as f32,
        }
    }
}

/// The colour word for a vectorscope angle ([`Summary::cast_angle`]).
pub fn hue_word(angle: f32) -> &'static str {
    // The 709 primaries and secondaries on the scope, nearest wins.
    const HUES: [(f32, &str); 6] = [
        (103.0, "red"),
        (175.0, "yellow"),
        (230.0, "green"),
        (283.0, "cyan"),
        (355.0, "blue"),
        (50.0, "magenta"),
    ];
    let dist = |a: f32| {
        let d = (a - angle).rem_euclid(360.0);
        d.min(360.0 - d)
    };
    HUES.iter().min_by(|a, b| dist(a.0).total_cmp(&dist(b.0))).map_or("grey", |h| h.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, bgr: [u8; 3]) -> Vec<u8> {
        (0..w * h).flat_map(|_| [bgr[0], bgr[1], bgr[2], 255]).collect()
    }

    #[test]
    fn grey_frame_sits_on_one_level_with_no_chroma() {
        let s = compute(&solid(320, 180, [128, 128, 128]), 320, 180, 32);
        assert!(s.samples > 1000 && s.samples as usize <= MAX_SAMPLES);
        let sum = s.summary(4);
        for sp in sum.spread {
            assert!((sp.median - 50.2).abs() < 0.5 && sp.min == sp.max, "{sp:?}");
        }
        assert!(sum.saturation < 0.01 && sum.cast < 0.01);
        assert_eq!(s.vector.iter().filter(|&&c| c > 0).count(), 1, "grey is one dot at the centre");
        assert_eq!(sum.bands.len(), 4);
    }

    #[test]
    fn a_warm_frame_reads_as_a_red_ish_cast_and_red_above_blue() {
        let s = compute(&solid(64, 64, [60, 120, 200]), 64, 64, 16);
        let sum = s.summary(2);
        assert!(sum.spread[1].mean > sum.spread[3].mean + 30.0);
        assert!(sum.cast > 10.0);
        let w = hue_word(sum.cast_angle);
        assert!(w == "red" || w == "yellow", "{w} at {}", sum.cast_angle);
    }

    #[test]
    fn black_and_white_halves_report_crush_and_clip() {
        let (w, h) = (100, 10);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| if i % w < 50 { [0, 0, 0, 255] } else { [255; 4] }).collect();
        let sum = compute(&bgra, w, h, 10).summary(2);
        assert!((sum.crushed - 50.0).abs() < 2.0 && (sum.clipped - 50.0).abs() < 2.0, "{sum:?}");
        assert!(sum.bands[0][2] < 5.0 && sum.bands[1][0] > 95.0, "left band dark, right band bright");
        assert!(compute(&[], 0, 0, 8).summary(3).bands.len() == 3);
    }
}
