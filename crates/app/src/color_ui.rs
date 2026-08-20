//! The colour card's numbers and its histogram.

use crate::*;

/// The colour card's four controls, in the order it lists them: what each is
/// called and the range it moves in. The order is `ColorParams`' own, which is
/// what [`color_band`] indexes.
pub(crate) const COLOR_BANDS: [(&str, f32, f32); 4] = [
    ("Brightness", -1., 1.),
    ("Contrast", 0., 2.),
    ("Saturation", 0., 2.),
    ("Tint", -1., 1.),
];

/// A press of a nudge key: a fortieth of a band's range, so a slider crosses it
/// in forty presses and every stop is a number the file can write. A drag lands
/// on the same grid ([`Player::drag_color`]), so the pointer and the keyboard
/// cannot reach two different sets of values.
pub(crate) const COLOR_STEP: f32 = 0.05;

/// The card's width: a slider row is a label, the bar and the value, and the
/// longest label has to fit beside all three without truncating (measured
/// against "Tint (cool–warm)").
pub(crate) const COLOR_W: f32 = 460.;
/// How much of a slider row the bar itself gets -- what a drag is read against,
/// so it takes the width the two nudge buttons used to.
pub(crate) const COLOR_BAR_W: f32 = 240.;

/// The histogram's bins per channel. 64 is four codes of an 8-bit ramp per bin:
/// fine enough to see a grade tilt, coarse enough that a subsampled count is
/// not noise.
pub(crate) const HIST_BINS: usize = 64;

/// How many pixels of a frame the histogram reads. The stride is
/// `pixels / HIST_SAMPLES` (1920x1080 -> every 253rd pixel), which is a
/// thousandth of the frame and walks across columns rather than down one.
pub(crate) const HIST_SAMPLES: usize = 8_192;

/// The histogram box. Shorter than the equalizer's curve because four slider
/// rows stand under it and the card still has to fit a 360 px window.
pub(crate) const HIST_H: f32 = 96.;

/// A slider value on the [`COLOR_STEP`] grid: what a drag rounds to, so the
/// pointer stops where the arrow keys do and "0.35" on screen is the number the
/// file writes rather than a rounding of one.
pub(crate) fn color_snap(value: f32) -> f32 {
    (value / COLOR_STEP).round() * COLOR_STEP
}

/// How the frame on screen is spread across the tone range: `HIST_BINS` counts
/// per channel, read off the BGRA the decoder handed over -- which is the
/// *graded* picture, because the grade is folded into the conversion
/// (`engine::convert::i420_to_bgra_with`). So what this counts is what the eye
/// is looking at, and moving a slider moves it.
///
/// Every [`HIST_SAMPLES`]th-of-a-frame pixel, not every pixel: a shape drawn
/// from eight thousand samples is the same shape, at a thousandth of the reads.
pub(crate) fn histogram(bgra: &[u8]) -> [[u32; HIST_BINS]; 3] {
    let pixels = bgra.len() / 4;
    let stride = (pixels / HIST_SAMPLES).max(1);
    let mut bins = [[0u32; HIST_BINS]; 3];
    for p in (0..pixels).step_by(stride) {
        let px = &bgra[p * 4..];
        // BGRA on the wire, `[r, g, b]` in the bins: the graph names channels
        // the way a person does.
        for (channel, value) in [px[2], px[1], px[0]].into_iter().enumerate() {
            bins[channel][usize::from(value) * HIST_BINS / 256] += 1;
        }
    }
    bins
}

/// The three counts drawn as three lines across the box, tallest bin to the top.
///
/// Square root, not linear: a shot with a big flat area (a night sky, a title
/// card) puts one bin so far above the rest that a linear graph is a single
/// spike beside a flat line, and the tilt a grade puts in the rest is exactly
/// what the card is for.
pub(crate) fn hist_curves(bins: [[u32; HIST_BINS]; 3]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            // Shared across the channels, so their relative weight is readable;
            // never zero, so an unpumped (all-zero) histogram is a flat line
            // rather than a division by nothing.
            let top = bins.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
            for (channel, counts) in bins.iter().enumerate() {
                let mut path = PathBuilder::stroke(px(1.5));
                for (bin, &count) in counts.iter().enumerate() {
                    let at = point(
                        o.x + s.width * (bin as f32 / (HIST_BINS - 1) as f32),
                        o.y + s.height * (1. - (count as f32 / top).sqrt()),
                    );
                    match bin {
                        0 => path.move_to(at),
                        _ => path.line_to(at),
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, rgb(HIST_INK()[channel]));
                }
            }
        },
    )
    .absolute()
    .size_full()
}
