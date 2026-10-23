//! Auto colour: one correction read off a picture, as the colour card's Auto
//! button and the `auto_color` edit op set it.
//!
//! Two readings, both on the clip as shot (the ungraded frame), so the values
//! it writes are absolute and a second press lands where the first did:
//!
//! * grey-world white balance: each channel's mean is moved to the average of
//!   the three means, by the wheels' per-channel gain (`1 + gain`, so the
//!   correction is a multiplier in R'G'B'),
//! * a luma stretch: after that balance, the 0.5 % and 99.5 % luma percentiles
//!   are carried to 0.02 and 0.98 by the contrast and brightness sliders. The
//!   sliders pivot on mid grey and move in luma-range units, so a linear stretch
//!   `v' = a v + b` is exactly `contrast = a` with `brightness = b + (a - 1) / 2`.
//!
//! Nothing else is touched: saturation, tint, lift, gamma, curves and the LUT
//! keep what the clip has. A flat frame, one with no luma spread to stretch,
//! gets no stretch at all.

use crate::color::ColorParams;

/// Luma the low percentile lands on.
pub const LOW_TARGET: f32 = 0.02;
/// Luma the high percentile lands on.
pub const HIGH_TARGET: f32 = 0.98;
/// Percentiles read off the balanced frame.
const LOW_PCT: f32 = 0.005;
const HIGH_PCT: f32 = 0.995;
/// Pixels read per frame, at most: a strided grid, as the scopes read theirs.
const MAX_SAMPLES: usize = 65_536;
/// Bins of the luma histogram the percentiles are read from.
const BINS: usize = 1024;

/// The correction one frame calls for. Plain numbers, so the card and the MCP
/// op can both say what they set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Correction {
    /// Red, green and blue wheel gains, -1..1: each channel's own move to meet the grey average.
    pub gain: [f32; 3],
    /// The brightness slider, -1..1.
    pub brightness: f32,
    /// The contrast slider, 0..2.
    pub contrast: f32,
    /// Luma at the low percentile before the stretch, 0..1.
    pub low: f32,
    /// Luma at the high percentile before the stretch, 0..1.
    pub high: f32,
}

/// Reads the correction off one packed RGB8 frame (`width * height` pixels,
/// three bytes each). `None` when the buffer does not hold that many pixels or
/// there are none.
pub fn analyse(rgb: &[u8], width: usize, height: usize) -> Option<Correction> {
    let pixels = width.checked_mul(height).filter(|&n| n > 0)?;
    if rgb.len() != pixels.checked_mul(3)? {
        return None;
    }
    let step = (pixels / MAX_SAMPLES).max(1);
    let samples: Vec<[f32; 3]> = (0..pixels)
        .step_by(step)
        .map(|i| [rgb[3 * i], rgb[3 * i + 1], rgb[3 * i + 2]].map(|c| f32::from(c) / 255.0))
        .collect();

    // Grey world: every channel's mean goes to the average of the three.
    let n = samples.len() as f32;
    let mut mean = [0f32; 3];
    for s in &samples {
        for c in 0..3 {
            mean[c] += s[c] / n;
        }
    }
    let target = (mean[0] + mean[1] + mean[2]) / 3.0;
    let mut gain = [0f32; 3];
    for c in 0..3 {
        if mean[c] > 1e-3 && target > 0.0 {
            gain[c] = (target / mean[c] - 1.0).clamp(-1.0, 1.0);
        }
    }

    // Luma of the balanced frame, counted into bins over 0..1.
    let mut bins = [0u32; BINS];
    for s in &samples {
        let rgb = [0, 1, 2].map(|c| (s[c] * (1.0 + gain[c])).clamp(0.0, 1.0));
        let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        bins[((luma * (BINS - 1) as f32).round() as usize).min(BINS - 1)] += 1;
    }
    let low = percentile(&bins, LOW_PCT);
    let high = percentile(&bins, HIGH_PCT);

    let (brightness, contrast) = if high - low < 1.0 / 256.0 {
        (0.0, 1.0)
    } else {
        let a = ((HIGH_TARGET - LOW_TARGET) / (high - low)).clamp(0.0, 2.0);
        let b = LOW_TARGET - a * low;
        ((b + (a - 1.0) / 2.0).clamp(-1.0, 1.0), a)
    };
    Some(Correction { gain, brightness, contrast, low, high })
}

/// The luma at `pct` of the histogram, as 0..1.
fn percentile(bins: &[u32; BINS], pct: f32) -> f32 {
    let total: u32 = bins.iter().sum();
    let want = (pct * total as f32).ceil().max(1.0) as u32;
    let mut seen = 0;
    for (i, &count) in bins.iter().enumerate() {
        seen += count;
        if seen >= want {
            return i as f32 / (BINS - 1) as f32;
        }
    }
    1.0
}

impl Correction {
    /// `base` with this correction written onto it: the three channel gains
    /// (the master gain stays where it is, so the channels still meet the
    /// average once it is added), brightness and contrast. Everything else in
    /// `base` is kept as it was.
    pub fn apply(&self, base: ColorParams) -> ColorParams {
        let mut look = base.look;
        for c in 0..3 {
            look.wheels.gain[c + 1] = (self.gain[c] - look.wheels.gain[0]).clamp(-1.0, 1.0);
        }
        ColorParams { brightness: self.brightness, contrast: self.contrast, look, ..base }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::look_rgb;

    /// A `w` x `h` frame whose pixel (x, y) is `f(x, y)`.
    fn frame(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect()
    }

    /// The wheels' part of the correction on one R'G'B' value (the decoder's own
    /// formulas; the sliders move luma and leave the channels' balance alone).
    fn balanced(c: &Correction, p: [f32; 3]) -> [f32; 3] {
        look_rgb(&c.apply(ColorParams::default()).look, p)
    }

    #[test]
    fn a_blue_cast_comes_back_neutral() {
        // A grey ramp with blue lifted by 40 %: a clear cast.
        let rgb = frame(16, 16, |x, _| {
            let g = 40 + (x * 12) as u8;
            [g, g, (f32::from(g) * 1.4).min(255.0) as u8]
        });
        let c = analyse(&rgb, 16, 16).expect("a frame");
        assert!(c.gain[2] < -0.1, "blue must be pulled down: {c:?}");
        assert!(c.gain[0] > 0.0, "red must come up: {c:?}");
        let mut sums = [0f32; 3];
        for px in rgb.chunks(3) {
            let p = [px[0], px[1], px[2]].map(|v| f32::from(v) / 255.0);
            let out = balanced(&c, p);
            for k in 0..3 {
                sums[k] += out[k] / 256.0;
            }
        }
        let avg = (sums[0] + sums[1] + sums[2]) / 3.0;
        for k in 0..3 {
            assert!((sums[k] - avg).abs() < 0.02, "channel {k} off the average: {sums:?}");
        }
        // Only the balance and the two slider fields are written; the rest of the grade is kept.
        let base = ColorParams { saturation: 0.7, tint: 0.2, ..Default::default() };
        let p = c.apply(base);
        assert_eq!((p.saturation, p.tint), (0.7, 0.2));
        assert_eq!((p.brightness, p.contrast), (c.brightness, c.contrast));
    }

}
