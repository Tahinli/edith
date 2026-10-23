//! The colour card's numbers and its histogram.

use crate::*;
use crate::ui::type_scale::{self, Typeset};
use gpui::{Rgba, fill};

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

/// The card's width: the longest label has to fit on its line beside the value
/// without truncating (measured against "Tint (cool–warm)").
pub(crate) const COLOR_W: f32 = 460.;
/// A bar's width for the pointer tests to drive. The card's bars fill the row
/// now, so nothing in the app reads this.
#[cfg(test)]
pub(crate) const COLOR_BAR_W: f32 = 240.;

/// The histogram's bins. 64 is four codes of an 8-bit ramp per bin: fine enough
/// to see a grade tilt, coarse enough that a subsampled count is not noise.
pub(crate) const HIST_BINS: usize = 64;

/// How many pixels of a frame the histogram reads. The stride is
/// `pixels / HIST_SAMPLES` (1920x1080 -> every 253rd pixel), which is a
/// thousandth of the frame and walks across columns rather than down one.
pub(crate) const HIST_SAMPLES: usize = 8_192;

/// The histogram box. Shorter than the equalizer's curve because the slider
/// rows stand under it and the card still has to fit a 360 px window.
pub(crate) const HIST_H: f32 = 96.;

/// A slider value on the [`COLOR_STEP`] grid: what a drag rounds to, so the
/// pointer stops where the arrow keys do and "0.35" on screen is the number the
/// file writes rather than a rounding of one.
pub(crate) fn color_snap(value: f32) -> f32 {
    (value / COLOR_STEP).round() * COLOR_STEP
}

/// How the frame on screen is spread across the tone range: `HIST_BINS` counts
/// of its luma, read off the BGRA the decoder handed over -- which is the
/// *graded* picture, because the grade is folded into the conversion
/// (`engine::convert::i420_to_bgra_with`). So what this counts is what the eye
/// is looking at, and moving a slider moves it.
///
/// Every [`HIST_SAMPLES`]th-of-a-frame pixel, not every pixel: a shape drawn
/// from eight thousand samples is the same shape, at a thousandth of the reads.
pub(crate) fn histogram(bgra: &[u8]) -> [u32; HIST_BINS] {
    let pixels = bgra.len() / 4;
    let stride = (pixels / HIST_SAMPLES).max(1);
    let mut bins = [0u32; HIST_BINS];
    for p in (0..pixels).step_by(stride) {
        let px = &bgra[p * 4..];
        // BGRA on the wire. Rec. 709 weights in 256ths (54 + 183 + 19 is 256),
        // so the luma stays in 0..=255 with no division.
        let luma = (54 * u32::from(px[2]) + 183 * u32::from(px[1]) + 19 * u32::from(px[0])) >> 8;
        bins[luma as usize * HIST_BINS / 256] += 1;
    }
    bins
}

/// The luma counts drawn as one line across the box, tallest bin to the top.
///
/// Square root, not linear: a shot with a big flat area (a night sky, a title
/// card) puts one bin so far above the rest that a linear graph is a single
/// spike beside a flat line, and the tilt a grade puts in the rest is exactly
/// what the card is for.
pub(crate) fn hist_curves(bins: [u32; HIST_BINS]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            // Never zero, so an unpumped (all-zero) histogram is a flat line
            // rather than a division by nothing.
            let top = bins.iter().copied().max().unwrap_or(0).max(1) as f32;
            let mut path = PathBuilder::stroke(px(1.5));
            for (bin, &count) in bins.iter().enumerate() {
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
                window.paint_path(path, rgb(HIST_INK()[1]));
            }
        },
    )
    .absolute()
    .size_full()
}

// -- the look: wheels, curves and a LUT, and the scopes ------------------------

/// The colour card's sections, one shown at a time so the card stays the height
/// it was: the four sliders, the wheels, the curves, the LUT. The order is the
/// order the grade runs in, bar the sliders, which are what a grade starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorSection {
    Basic,
    Wheels,
    Curves,
    Lut,
}

pub(crate) const COLOR_SECTIONS: [(ColorSection, &str); 4] = [
    (ColorSection::Basic, "Basic"),
    (ColorSection::Wheels, "Wheels"),
    (ColorSection::Curves, "Curves"),
    (ColorSection::Lut, "Look file"),
];

/// What the graph over the controls shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeView {
    Histogram,
    Waveform,
    Vectorscope,
    Parade,
}

pub(crate) const SCOPE_VIEWS: [(ScopeView, &str); 4] = [
    (ScopeView::Histogram, "Histogram"),
    (ScopeView::Waveform, "Waveform"),
    (ScopeView::Vectorscope, "Vectorscope"),
    (ScopeView::Parade, "RGB parade"),
];

/// Columns the waveform and parade are counted into.
pub(crate) const SCOPE_COLS: usize = 48;

/// The wheels the Wheels section picks between, in `engine::color::Wheels`' order.
pub(crate) const WHEEL_NAMES: [&str; 3] = ["Lift", "Gamma", "Gain"];
/// What each wheel does, for its tooltip.
pub(crate) const WHEEL_WHY: [&str; 3] = [
    "Raises or lowers the blacks; white stays where it is",
    "Bends the mid-tones; black and white stay",
    "Scales the whole channel from black: the highlights move most",
];
/// The channel rows of a wheel and the curves' channel picker, in
/// `engine::color::CHANNELS`' order.
pub(crate) const CHANNEL_NAMES: [&str; 4] = ["Master", "Red", "Green", "Blue"];
/// The five points of a curve the Curves section edits: their inputs and names.
pub(crate) const CURVE_GRID: [(f32, &str); 5] =
    [(0.0, "Blacks"), (0.25, "Shadows"), (0.5, "Mid-tones"), (0.75, "Highlights"), (1.0, "Whites")];

/// Slider slots: the four basic bands, then a wheel's four channels, the five
/// curve points and the LUT's intensity. One recorded bar box each.
pub(crate) const WHEEL_BAND: usize = 4;
pub(crate) const CURVE_BAND: usize = WHEEL_BAND + 4;
pub(crate) const LUT_BAND: usize = CURVE_BAND + CURVE_GRID.len();
pub(crate) const COLOR_SLOTS: usize = LUT_BAND + 1;

/// The bands a section's arrows walk.
pub(crate) fn section_bands(section: ColorSection) -> std::ops::Range<usize> {
    match section {
        ColorSection::Basic => 0..COLOR_BANDS.len(),
        ColorSection::Wheels => WHEEL_BAND..CURVE_BAND,
        ColorSection::Curves => CURVE_BAND..LUT_BAND,
        ColorSection::Lut => LUT_BAND..COLOR_SLOTS,
    }
}

/// A band's name, range and keyboard step.
pub(crate) fn band_spec(band: usize) -> (&'static str, f32, f32, f32) {
    match band {
        0..WHEEL_BAND => {
            let (name, lo, hi) = COLOR_BANDS[band];
            (name, lo, hi, COLOR_STEP)
        }
        WHEEL_BAND..CURVE_BAND => (CHANNEL_NAMES[band - WHEEL_BAND], -1., 1., 0.01),
        CURVE_BAND..LUT_BAND => (CURVE_GRID[band - CURVE_BAND].1, 0., 1., 0.01),
        _ => ("Intensity", 0., 1., COLOR_STEP),
    }
}

/// The value of `band` in `params`, with `wheel` and `curve` the Wheels and
/// Curves sections' picks. A curve point reads the curve's output at that input,
/// whatever points the curve was made of.
pub(crate) fn band_value(params: &ColorParams, band: usize, wheel: usize, curve: usize) -> f32 {
    let look = &params.look;
    match band {
        0..WHEEL_BAND => *band_mut(&mut params.clone(), band),
        WHEEL_BAND..CURVE_BAND => wheel_of(&look.wheels, wheel)[band - WHEEL_BAND],
        CURVE_BAND..LUT_BAND => curve_at(&look.curves[curve], CURVE_GRID[band - CURVE_BAND].0),
        _ => look.lut_mix,
    }
}

/// [`band_value`]'s writer. Moving a curve point rebuilds that curve on the
/// five-point grid (each point at the output the curve had there), so an
/// all-diagonal grid is the straight curve again.
pub(crate) fn set_band(params: &mut ColorParams, band: usize, wheel: usize, curve: usize, value: f32) {
    let look = &mut params.look;
    match band {
        0..WHEEL_BAND => *band_mut(params, band) = value,
        WHEEL_BAND..CURVE_BAND => {
            let w = &mut look.wheels;
            let slot = match wheel {
                0 => &mut w.lift,
                1 => &mut w.gamma,
                _ => &mut w.gain,
            };
            slot[band - WHEEL_BAND] = value;
        }
        CURVE_BAND..LUT_BAND => {
            let c = &look.curves[curve];
            let mut pts: Vec<[f32; 2]> = CURVE_GRID.iter().map(|&(x, _)| [x, curve_at(c, x)]).collect();
            pts[band - CURVE_BAND][1] = value;
            look.curves[curve] = if pts.iter().all(|p| (p[0] - p[1]).abs() < 1e-6) {
                engine::color::Curve::default()
            } else {
                engine::color::Curve::new(&pts).unwrap_or_default()
            };
        }
        _ => look.lut_mix = value,
    }
}

/// A band's neutral value: what Reset puts back.
pub(crate) fn band_neutral(band: usize) -> f32 {
    match band {
        0..WHEEL_BAND => *band_mut(&mut ColorParams::default(), band),
        WHEEL_BAND..CURVE_BAND => 0.,
        CURVE_BAND..LUT_BAND => CURVE_GRID[band - CURVE_BAND].0,
        _ => 1.,
    }
}

fn wheel_of(w: &engine::color::Wheels, wheel: usize) -> [f32; 4] {
    match wheel {
        0 => w.lift,
        1 => w.gamma,
        _ => w.gain,
    }
}

/// A curve's output at input `x`.
pub(crate) fn curve_at(c: &engine::color::Curve, x: f32) -> f32 {
    if c.points().is_empty() {
        return x;
    }
    let t = c.sampled(257);
    t[(x.clamp(0., 1.) * 256.).round() as usize]
}

/// `color` at `alpha` (0..255) as a gpui colour.
fn ink(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | u32::from(alpha))
}

/// Count to alpha: square root, so a sparse trace still shows beside a dense one
/// (the histogram's own reason).
fn glow(count: u32, top: u32) -> u8 {
    if count == 0 {
        return 0;
    }
    (40. + 215. * (count as f32 / top.max(1) as f32).sqrt()).min(255.) as u8
}

/// The scopes drawn in the graph box: the waveform, the vectorscope or the RGB
/// parade of the frame on screen, counted at low resolution
/// ([`engine::scopes::compute`]). Theme inks: the histogram's three. Each one
/// carries a faint graticule and its labels in the caption ink, so a reading
/// can be taken off it: 0 to 100 percent lines on the two level scopes, and a
/// centre cross, a ring and the six colour targets on the vectorscope.
pub(crate) fn scope_canvas(view: ScopeView, scopes: Option<engine::scopes::Scopes>) -> impl IntoElement {
    let (head, foot) = plot_inset(view);
    let gutter = plot_gutter(view);
    let plot = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, sz) = (bounds.origin, bounds.size);
            let levels = engine::scopes::LEVELS;
            let cell = |x0: f32, y0: f32, w: f32, h: f32| {
                Bounds::new(point(o.x + px(x0), o.y + px(y0)), size(px(w), px(h)))
            };
            let (w, h) = (f32::from(sz.width), f32::from(sz.height));
            let [red, green, blue] = HIST_INK();
            // The graticule goes down first, so the trace is drawn over it.
            let faint = ink(INK3(), GRATICULE_ALPHA);
            match view {
                ScopeView::Histogram => {}
                ScopeView::Waveform | ScopeView::Parade => {
                    for f in GRATICULE_LEVELS {
                        window.paint_quad(fill(cell(0., (h * (1. - f)).clamp(0., h - 1.), w, 1.), faint));
                    }
                    if view == ScopeView::Parade {
                        for k in 1..3 {
                            window.paint_quad(fill(cell(w * k as f32 / 3., 0., 1., h), faint));
                        }
                    }
                }
                ScopeView::Vectorscope => {
                    let (cx, cy) = (w / 2., h / 2.);
                    let rr = w.min(h) / 2. - VECTOR_RING_INSET;
                    window.paint_quad(fill(cell(cx, 0., 1., h), faint));
                    window.paint_quad(fill(cell(0., cy, w, 1.), faint));
                    let mut ring = PathBuilder::stroke(px(1.));
                    for i in 0..=72 {
                        let a = i as f32 / 72. * std::f32::consts::TAU;
                        let at = point(o.x + px(cx + rr * a.cos()), o.y + px(cy + rr * a.sin()));
                        match i {
                            0 => ring.move_to(at),
                            _ => ring.line_to(at),
                        }
                    }
                    if let Ok(ring) = ring.build() {
                        window.paint_path(ring, faint);
                    }
                    for (_, cb, cr) in VECTOR_TARGETS {
                        let (dx, dy) = unit(cb, cr);
                        window.paint_quad(fill(cell(cx + rr * dx - 1.5, cy - rr * dy - 1.5, 3., 3.), ink(INK3(), 0x90)));
                    }
                }
            }
            let Some(s) = &scopes else {
                return;
            };
            match view {
                ScopeView::Histogram => {}
                ScopeView::Waveform => {
                    let top = s.wave.iter().copied().max().unwrap_or(1);
                    let (cw, ch) = (w / s.cols as f32, h / levels as f32);
                    for lvl in 0..levels {
                        for col in 0..s.cols {
                            let a = glow(s.wave[lvl * s.cols + col], top);
                            if a > 0 {
                                let y = h - (lvl + 1) as f32 * ch;
                                window.paint_quad(fill(cell(col as f32 * cw, y, cw, ch), ink(green, a)));
                            }
                        }
                    }
                }
                ScopeView::Parade => {
                    let third = w / 3.;
                    let (cw, ch) = (third / s.cols as f32, h / levels as f32);
                    for (k, color) in [red, green, blue].into_iter().enumerate() {
                        let grid = &s.parade[k];
                        let top = grid.iter().copied().max().unwrap_or(1);
                        for lvl in 0..levels {
                            for col in 0..s.cols {
                                let a = glow(grid[lvl * s.cols + col], top);
                                if a > 0 {
                                    let x = k as f32 * third + col as f32 * cw;
                                    let y = h - (lvl + 1) as f32 * ch;
                                    window.paint_quad(fill(cell(x, y, cw, ch), ink(color, a)));
                                }
                            }
                        }
                    }
                }
                ScopeView::Vectorscope => {
                    let n = engine::scopes::VECTOR;
                    let side = w.min(h);
                    let (x0, y0, c) = ((w - side) / 2., (h - side) / 2., side / n as f32);
                    let top = s.vector.iter().copied().max().unwrap_or(1);
                    for row in 0..n {
                        for col in 0..n {
                            let a = glow(s.vector[row * n + col], top);
                            if a > 0 {
                                window.paint_quad(fill(cell(x0 + col as f32 * c, y0 + row as f32 * c, c, c), ink(green, a)));
                            }
                        }
                    }
                }
            }
        },
    )
    .absolute()
    .left(px(gutter))
    .right_0()
    .top(px(head))
    .bottom(px(foot));
    div()
        .absolute()
        .size_full()
        .child(plot)
        .children(scope_labels(view))
}

/// The strip above and below a parade's traces: the R/G/B names sit in the top one, and the
/// bottom one gives the 0% label room below its line. Other scopes use the whole box.
const PARADE_HEAD: f32 = 16.;
const PARADE_FOOT: f32 = 20.;

/// The (top, bottom) inset of a scope's traces inside the graph box.
fn plot_inset(view: ScopeView) -> (f32, f32) {
    match view {
        ScopeView::Parade => (PARADE_HEAD, PARADE_FOOT),
        _ => (0., 0.),
    }
}

/// The left strip that holds a level scope's percent labels, clear of its traces.
/// Other scopes have no labels down the side, so they keep the whole box.
/// Sized for the widest percent label, "100%", at the caption size with room to spare for the
/// glyphs' real width, so a label is never cut at the box's left edge.
fn axis_gutter() -> f32 {
    crate::ui::popup::text_w("100%", type_scale::CAPTION_PX, false) * 1.2 + 8.
}

fn plot_gutter(view: ScopeView) -> f32 {
    match view {
        ScopeView::Waveform | ScopeView::Parade => axis_gutter(),
        _ => 0.,
    }
}

/// The graticule's level lines, as fractions of the scope's height: 0 to 100 percent.
const GRATICULE_LEVELS: [f32; 5] = [0., 0.25, 0.5, 0.75, 1.];

/// The graticule's line strength: the caption ink at a low alpha, so it reads as a guide.
const GRATICULE_ALPHA: u8 = 0x38;

/// How far the vectorscope's ring sits in from the box's edge, so the target
/// letters have room outside it.
const VECTOR_RING_INSET: f32 = 14.;

/// The six colour targets on a vectorscope, each as the Cb and Cr its full-scale
/// colour lands on in the engine's own units ([`engine::scopes::compute`]).
const VECTOR_TARGETS: [(&str, f32, f32); 6] = [
    ("R", -0.161, 0.445),
    ("Mg", 0.316, 0.373),
    ("B", 0.478, -0.072),
    ("Cy", 0.161, -0.445),
    ("G", -0.316, -0.373),
    ("Yl", -0.478, 0.072),
];

/// A target's direction on the ring, unit length, with Cr up (screen y is down, so
/// the caller flips the sign for the screen).
fn unit(cb: f32, cr: f32) -> (f32, f32) {
    let len = cb.hypot(cr).max(f32::EPSILON);
    (cb / len, cr / len)
}

/// The labels over a scope's graticule, in the caption ink. The canvas paints the
/// lines; the words are plain text laid over it.
fn scope_labels(view: ScopeView) -> Vec<Div> {
    let label = |text: &str| {
        div()
            .absolute()
            .type_style(type_scale::caption())
            .text_color(rgb(INK3()))
            .whitespace_nowrap()
            .child(text.to_string())
    };
    // The percent labels sit in the gutter, right-aligned to the box, centred on their line.
    let level = |text: &str, y: f32| {
        label(text)
            .left_0()
            .w(px(axis_gutter() - 4.))
            .flex()
            .justify_end()
            .top(px(y - 6.))
    };
    match view {
        ScopeView::Histogram => Vec::new(),
        ScopeView::Waveform | ScopeView::Parade => {
            let parade = view == ScopeView::Parade;
            let (head, foot) = plot_inset(view);
            let plot_h = HIST_H - head - foot;
            let mut out: Vec<Div> = GRATICULE_LEVELS
                .iter()
                .map(|&f| level(&format!("{:.0}%", f * 100.), head + plot_h * (1. - f)))
                .collect();
            if parade {
                for (k, name) in ["R", "G", "B"].into_iter().enumerate() {
                    out.push(
                        div()
                            .absolute()
                            .left(relative(k as f32 / 3.))
                            .w(relative(1. / 3.))
                            .top_0()
                            .h(px(PARADE_HEAD))
                            .flex()
                            .items_center()
                            .justify_center()
                            .type_style(type_scale::caption())
                            .text_color(rgb(INK3()))
                            .child(name),
                    );
                }
            }
            out
        }
        ScopeView::Vectorscope => {
            // The ring is `VECTOR_RING_INSET` in from the scope's side, and the labels sit
            // just outside it, centred on the box.
            let lr = HIST_H / 2. - VECTOR_RING_INSET + 9.;
            VECTOR_TARGETS
                .iter()
                .map(|&(name, cb, cr)| {
                    let (dx, dy) = unit(cb, cr);
                    div()
                        .absolute()
                        .left(relative(0.5))
                        .top(relative(0.5))
                        .ml(px(lr * dx - 8.))
                        .mt(px(-lr * dy - 7.))
                        .w(px(16.))
                        .h(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .type_style(type_scale::caption())
                        .text_color(rgb(INK3()))
                        .child(name)
                })
                .collect()
        }
    }
}

/// The picked curve drawn over the graph: the line, and the five grid points as
/// small squares, in the channel's ink (master in the light ink).
pub(crate) fn curve_overlay(curve: engine::color::Curve, channel: usize) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let inks = HIST_INK();
            let color = if channel == 0 { INK1() } else { inks[channel - 1] };
            let t = curve.sampled(65);
            let mut path = PathBuilder::stroke(px(1.5));
            for (i, &y) in t.iter().enumerate() {
                let at = point(o.x + s.width * (i as f32 / 64.), o.y + s.height * (1. - y));
                match i {
                    0 => path.move_to(at),
                    _ => path.line_to(at),
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
            for &(x, _) in &CURVE_GRID {
                let y = curve_at(&curve, x);
                let c = point(o.x + s.width * x - px(3.), o.y + s.height * (1. - y) - px(3.));
                window.paint_quad(fill(Bounds::new(c, size(px(6.), px(6.))), rgb(color)));
            }
        },
    )
    .absolute()
    .size_full()
}

// -- the colour wheels ----------------------------------------------------------

/// How far a puck at the rim of a wheel pushes a channel: the wheel's R, G and B
/// offsets are `WHEEL_REACH` times the puck's position, so the rim is a strong
/// but usable tint and the sliders' -1..1 range stays far beyond it.
pub(crate) const WHEEL_REACH: f32 = 0.5;

/// The R, G, B offsets a puck at `(x, y)` (right and up, the unit disc) stands
/// for: the hue is the angle (red at 3 o'clock, green at 7, blue at 11) and the
/// saturation the distance, so the three offsets always sum to zero and the
/// wheel tints without moving the master.
pub(crate) fn wheel_offsets(x: f32, y: f32) -> [f32; 3] {
    let len = x.hypot(y);
    let k = if len > 1. { 1. / len } else { 1. };
    let (x, y) = (x * k, y * k);
    let h = 3f32.sqrt() / 2.;
    [WHEEL_REACH * x, WHEEL_REACH * (-x / 2. + h * y), WHEEL_REACH * (-x / 2. - h * y)]
}

/// [`wheel_offsets`]' inverse: where the puck sits for an RGB offset (any mean the
/// three share is the master's business, not the pad's, and is dropped).
pub(crate) fn wheel_puck(r: f32, g: f32, b: f32) -> (f32, f32) {
    let x = (2. * r - g - b) / 3. / WHEEL_REACH;
    let y = (g - b) / 3f32.sqrt() / WHEEL_REACH;
    let len = x.hypot(y);
    if len > 1. { (x / len, y / len) } else { (x, y) }
}

/// A wheel's RGB offsets snapped to the hundredth the sliders land on.
pub(crate) fn wheel_snap(v: f32) -> f32 {
    ((v / 0.01).round() * 0.01).clamp(-1., 1.)
}

/// A wheel's pad: the hue/saturation disc, a cross through the centre and the
/// puck at `(x, y)` (the unit disc, up positive).
pub(crate) fn wheel_disc(x: f32, y: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let (cx, cy) = (f32::from(bounds.origin.x) + w / 2., f32::from(bounds.origin.y) + h / 2.);
            let radius = w.min(h) / 2. - 1.;
            let (wedges, rings) = (48, 5);
            let at = |s: f32, a: f32| point(px(cx + radius * s * a.cos()), px(cy - radius * s * a.sin()));
            for ring in 0..rings {
                let (s0, s1) = (ring as f32 / rings as f32, (ring + 1) as f32 / rings as f32);
                for i in 0..wedges {
                    let a0 = i as f32 / wedges as f32 * std::f32::consts::TAU;
                    let a1 = (i + 1) as f32 / wedges as f32 * std::f32::consts::TAU;
                    let (r, g, b) = engine::decode::viz_hsv_rgb(i as f32 / wedges as f32 * 360., (s0 + s1) / 2. * 0.8, 0.55);
                    let mut path = PathBuilder::fill();
                    path.add_polygon(&[at(s0, a0), at(s0, a1), at(s1, a1), at(s1, a0)], false);
                    if let Ok(path) = path.build() {
                        window.paint_path(path, rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)));
                    }
                }
            }
            for (a0, a1) in [(std::f32::consts::PI, 0.), (-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2)] {
                let mut line = PathBuilder::stroke(px(1.));
                line.move_to(at(1., a0));
                line.line_to(at(1., a1));
                if let Ok(line) = line.build() {
                    window.paint_path(line, rgba(DARK_SEAM()));
                }
            }
            let (mx, my) = (cx + radius * x, cy - radius * y);
            for (r, color, wd) in [(6., 0x000000, 3.), (6., INK1(), 1.5)] {
                let mut ring = PathBuilder::stroke(px(wd));
                let pts: Vec<_> = (0..20)
                    .map(|k| {
                        let a = k as f32 / 20. * std::f32::consts::TAU;
                        point(px(mx + r * a.cos()), px(my + r * a.sin()))
                    })
                    .collect();
                ring.add_polygon(&pts, true);
                if let Ok(ring) = ring.build() {
                    window.paint_path(ring, rgb(color));
                }
            }
        },
    )
    .absolute()
    .size_full()
}
