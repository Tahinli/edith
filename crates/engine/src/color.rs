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
    /// The RGB grade that runs before the four sliders: wheels, curves, LUT.
    pub look: Look,
}

impl Default for ColorParams {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            tint: 0.0,
            look: Look::default(),
        }
    }
}

/// The most keys a clip's colour animation may hold ([`crate::project::Clip::color_keys`]).
pub const MAX_COLOR_KEYS: usize = 256;

/// One point of a clip's colour animation ([`crate::project::Clip::color_keys`]):
/// the whole grade at source frame `at`, numbered as `in_frame` is. Between two
/// keys each field moves with the earlier key's ease; the grade is rebuilt per
/// frame from the interpolated numbers, so a frame between two keys is graded
/// exactly as a constant grade of those numbers would be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorKey {
    /// Source frame of the key.
    pub at: u32,
    /// The grade exactly at that frame.
    pub params: ColorParams,
    /// The easing of the segment that leaves this key.
    pub ease: crate::transform::KeyEase,
}

impl crate::transform::Keyed for ColorKey {
    fn at(&self) -> u32 {
        self.at
    }
    fn ease(&self) -> crate::transform::KeyEase {
        self.ease
    }
    fn placed(self, at: u32, ease: crate::transform::KeyEase) -> Self {
        Self { at, ease, ..self }
    }
    fn mix(a: &Self, b: &Self, t: f32) -> Self {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Self {
            params: ColorParams {
                brightness: l(a.params.brightness, b.params.brightness),
                contrast: l(a.params.contrast, b.params.contrast),
                saturation: l(a.params.saturation, b.params.saturation),
                tint: l(a.params.tint, b.params.tint),
                look: a.params.look,
            },
            ..*a
        }
    }
}

/// The grade the keys give at source frame `src`, or `base` with no keys. Keys
/// animate the four sliders; the clip's [`Look`] (wheels, curves, LUT) is
/// `base`'s and holds steady under them.
pub fn evaluate_color(base: ColorParams, keys: &[ColorKey], src: f64) -> ColorParams {
    crate::transform::sample(keys, src).map_or(base, |k| ColorParams { look: base.look, ..k.params })
}

impl ColorParams {
    /// Whether this setting is the do-nothing one. Compared exactly on purpose:
    /// anything that is not the identity value goes through the transform, and
    /// only a `true` here is allowed to skip it.
    pub fn is_identity(&self) -> bool {
        self.sliders_identity() && self.look.is_identity()
    }

    /// Whether the four YUV sliders alone are at rest (the look may not be).
    pub fn sliders_identity(&self) -> bool {
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
    /// `full_range` says the samples span 0..255 instead of 16..235: the
    /// sliders' pivot, span and clamp follow it.
    pub(crate) fn new(params: &ColorParams, full_range: bool) -> Self {
        let (mid_y, luma_range, lo, hi) = if full_range { (127.5, 255.0, 0.0, 255.0) } else { (MID_Y, LUMA_RANGE, f32::from(CODES.0), f32::from(CODES.1)) };
        let brightness = sane(params.brightness, 0.0).clamp(-1.0, 1.0) * luma_range;
        let contrast = sane(params.contrast, 1.0).clamp(0.0, 2.0);
        let saturation = sane(params.saturation, 1.0).clamp(0.0, 2.0);
        let tint = sane(params.tint, 0.0).clamp(-1.0, 1.0) * TINT_RANGE * (luma_range / LUMA_RANGE);

        let mut lut = Self {
            y: [0; 256],
            u: [0; 256],
            v: [0; 256],
        };
        for code in 0..256 {
            let f = code as f32;
            // Clamped to the valid codes 1..254, not to 16..235: content that
            // is already blacker than black or whiter than white keeps whatever
            // a grade leaves of it instead of being crushed by this pass, and
            // nothing lands on 0 or 255, which Rec. 709 reserves for sync.
            lut.y[code] = byte((f - mid_y) * contrast + mid_y + brightness, lo, hi);
            lut.u[code] = byte((f - 128.0) * saturation + 128.0 - tint, lo, hi);
            lut.v[code] = byte((f - 128.0) * saturation + 128.0 + tint, lo, hi);
        }
        lut
    }
}

/// A parameter that survived a file or a UI edit; NaN and infinity fall back to
/// the identity value rather than blanking the picture.
fn sane(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// The lowest and highest valid 8-bit code: 0 and 255 are reserved for sync
/// (Rec. 709 / BT.1361), so no grade writes them. At 10 bits the reserved codes
/// are 0..=3 and 1020..=1023 ([`WIDE_CODES`]).
pub const CODES: (u8, u8) = (1, 254);
/// [`CODES`] at 10 bits.
pub const WIDE_CODES: (u16, u16) = (4, 1019);

fn byte(value: f32, lo: f32, hi: f32) -> u8 {
    value.round().clamp(lo, hi) as u8
}

/// The 10-bit tables of [`apply_yuv_wide`]: [`Lut::new`]'s arithmetic with every
/// code-valued constant at four times its 8-bit value, so one grade reads the same
/// picture at either depth.
fn wide_tables(params: &ColorParams, full_range: bool) -> ([u16; 1024], [u16; 1024], [u16; 1024]) {
    const SCALE: f32 = 4.0;
    // Full range spans 0..=1023 whole; limited is 16..235 at four times 8-bit.
    let (mid, span, lo, hi) = if full_range {
        (511.5, 1023.0, 0.0, 1023.0)
    } else {
        (MID_Y * SCALE, LUMA_RANGE * SCALE, f32::from(WIDE_CODES.0), f32::from(WIDE_CODES.1))
    };
    let brightness = sane(params.brightness, 0.0).clamp(-1.0, 1.0) * span;
    let contrast = sane(params.contrast, 1.0).clamp(0.0, 2.0);
    let saturation = sane(params.saturation, 1.0).clamp(0.0, 2.0);
    let tint = sane(params.tint, 0.0).clamp(-1.0, 1.0) * TINT_RANGE * (span / (LUMA_RANGE * SCALE));
    let wide_byte = |value: f32| value.round().clamp(lo, hi) as u16;
    let (mut y, mut u, mut v) = ([0u16; 1024], [0u16; 1024], [0u16; 1024]);
    for code in 0..1024 {
        let f = code as f32;
        y[code] = wide_byte((f - mid) * contrast + mid + brightness);
        u[code] = wide_byte((f - 512.0) * saturation + 512.0 - tint);
        v[code] = wide_byte((f - 512.0) * saturation + 512.0 + tint);
    }
    (y, u, v)
}

/// [`apply_yuv`] for one planar 10-bit frame (16-bit samples at 10-bit scale). The
/// same four controls as the 8-bit path, so a 10-bit export's grade is the grade
/// the preview shows, at the precision of the file.
pub fn apply_yuv_wide(params: &ColorParams, y: &mut [u16], u: &mut [u16], v: &mut [u16], width: usize, full_range: bool) {
    if params.is_identity() {
        return;
    }
    if let Some(stage) = Stage::new(&params.look) {
        stage.run(y, u, v, width, Codes::new(4.0, 1023, full_range));
    }
    if params.sliders_identity() {
        return;
    }
    let (ty, tu, tv) = wide_tables(params, full_range);
    for s in y.iter_mut() {
        *s = ty[usize::from(*s).min(1023)];
    }
    for s in u.iter_mut() {
        *s = tu[usize::from(*s).min(1023)];
    }
    for s in v.iter_mut() {
        *s = tv[usize::from(*s).min(1023)];
    }
}

/// Applies `params` in place to one planar 8-bit frame.
///
/// Planar 8-bit YUV with `stride == width`: 4:2:0 (what [`crate::convert`] and
/// the export path use), 4:2:2 or 4:4:4, told apart by the chroma planes' size.
/// Interleaved chroma (NV12) is *not* this function's frame.
///
/// Identity params return without touching a byte. `width` is the luma
/// plane's (stride == width): the look's RGB stage pairs each luma sample with
/// its chroma, which the four YUV sliders never need.
pub fn apply_yuv(params: &ColorParams, y: &mut [u8], u: &mut [u8], v: &mut [u8], width: usize, full_range: bool) {
    if params.is_identity() {
        return;
    }
    if let Some(stage) = Stage::new(&params.look) {
        stage.run(y, u, v, width, Codes::new(1.0, 255, full_range));
    }
    if params.sliders_identity() {
        return;
    }
    let lut = Lut::new(params, full_range);
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

// -- the look: lift/gamma/gain wheels, RGB curves and a .cube LUT -------------
//
// These three live in R'G'B' rather than YUV, because each is defined per
// red, green and blue channel. The stage converts a sample to Rec. 709
// non-linear R'G'B' in 0..1 (limited-range codes, the matrix every SDR export
// is tagged with), runs wheels -> curves -> LUT, and converts back. The order is
// the grading suites' own: the wheels balance the picture, the curves shape it,
// the LUT is the look laid over the balanced shot, and the four YUV sliders
// after them trim the result.
//
// The wheels and curves are both per-channel functions of one value, so they
// fold into one 4097-entry table per channel built once per frame; only the
// LUT is evaluated per pixel. A neutral look builds no stage at all, which keeps
// "nothing set" byte-identical.

/// The most points one curve may hold.
pub const MAX_CURVE_POINTS: usize = 16;

/// The wheel and curve channels, in the order [`Wheels`] and [`Look::curves`] hold them.
pub const CHANNELS: [&str; 4] = ["master", "red", "green", "blue"];

/// Lift, gamma and gain: each a master offset and a red, green and blue one, all
/// -1..1 with 0 neutral. Index 0 is master, then red, green, blue
/// ([`CHANNELS`]). A channel's amount is its own plus the master's, clamped.
///
/// * lift raises (or lowers) the blacks and leaves white where it is: 1.0 puts
///   black at half grey,
/// * gamma bends the mid-tones and leaves both ends: 1.0 is an exponent of 0.5
///   (brighter), -1.0 an exponent of 2 (darker),
/// * gain scales the whole channel from black: 1.0 doubles it, -1.0 is black.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Wheels {
    pub lift: [f32; 4],
    pub gamma: [f32; 4],
    pub gain: [f32; 4],
}

impl Wheels {
    pub fn is_identity(&self) -> bool {
        self.lift.iter().chain(&self.gamma).chain(&self.gain).all(|&v| v == 0.0)
    }

    /// Channel `c` (0 red, 1 green, 2 blue) of `x` through the three wheels.
    fn eval(&self, c: usize, x: f32) -> f32 {
        let amt = |w: &[f32; 4]| (sane(w[0], 0.0) + sane(w[c + 1], 0.0)).clamp(-1.0, 1.0);
        let (lift, gamma, gain) = (amt(&self.lift) * 0.5, amt(&self.gamma), amt(&self.gain));
        let v = (x + lift * (1.0 - x)) * (1.0 + gain);
        v.clamp(0.0, 1.0).powf(2f32.powf(-gamma))
    }
}

/// One curve: up to [`MAX_CURVE_POINTS`] points (input, output), both 0..1,
/// inputs strictly increasing, joined by a monotone cubic (Fritsch-Carlson), so
/// a curve through rising points never dips and never overshoots. Ends the
/// points leave open are pinned: no point at input 0 means (0, 0) is one, no
/// point at input 1 means (1, 1) is. No points, or points all on the diagonal,
/// is the neutral curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    len: u8,
    pts: [[f32; 2]; MAX_CURVE_POINTS],
}

impl Default for Curve {
    fn default() -> Self {
        Self { len: 0, pts: [[0.0; 2]; MAX_CURVE_POINTS] }
    }
}

impl Curve {
    /// A curve through `points`, or why they cannot be one.
    pub fn new(points: &[[f32; 2]]) -> Result<Self, String> {
        if points.len() > MAX_CURVE_POINTS {
            return Err(format!("a curve holds at most {MAX_CURVE_POINTS} points, not {}", points.len()));
        }
        let mut c = Self::default();
        for (i, p) in points.iter().enumerate() {
            if !p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                return Err(format!("curve point {i} ({}, {}) is outside 0..1", p[0], p[1]));
            }
            if i > 0 && p[0] <= points[i - 1][0] {
                return Err(format!("curve point {i}: inputs must strictly increase"));
            }
            c.pts[i] = *p;
        }
        c.len = points.len() as u8;
        Ok(c)
    }

    pub fn points(&self) -> &[[f32; 2]] {
        &self.pts[..usize::from(self.len)]
    }

    pub fn is_identity(&self) -> bool {
        self.points().iter().all(|p| p[0] == p[1])
    }

    /// The knots the spline runs through, the pinned ends included.
    fn knots(&self) -> Vec<[f32; 2]> {
        let mut k = self.points().to_vec();
        if k.first().is_none_or(|p| p[0] > 0.0) {
            k.insert(0, [0.0, 0.0]);
        }
        if k.last().is_none_or(|p| p[0] < 1.0) {
            k.push([1.0, 1.0]);
        }
        k
    }

    /// The curve sampled at `n` evenly spaced inputs over 0..1.
    pub fn sampled(&self, n: usize) -> Vec<f32> {
        let k = self.knots();
        let m = tangents(&k);
        let mut seg = 0;
        (0..n)
            .map(|i| {
                let x = i as f32 / (n - 1) as f32;
                while seg + 2 < k.len() && x > k[seg + 1][0] {
                    seg += 1;
                }
                let ([x0, y0], [x1, y1]) = (k[seg], k[seg + 1]);
                let h = x1 - x0;
                let t = ((x - x0) / h).clamp(0.0, 1.0);
                let (t2, t3) = (t * t, t * t * t);
                let y = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                    + (t3 - 2.0 * t2 + t) * h * m[seg]
                    + (-2.0 * t3 + 3.0 * t2) * y1
                    + (t3 - t2) * h * m[seg + 1];
                y.clamp(0.0, 1.0)
            })
            .collect()
    }
}

/// Fritsch-Carlson tangents: the secant average, zeroed at a turn and limited so
/// no segment overshoots its two knots.
fn tangents(k: &[[f32; 2]]) -> Vec<f32> {
    let n = k.len();
    let d: Vec<f32> = k.windows(2).map(|w| (w[1][1] - w[0][1]) / (w[1][0] - w[0][0])).collect();
    let mut m = vec![0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
        let s = a * a + b * b;
        if s > 9.0 {
            let t = 3.0 / s.sqrt();
            m[i] = t * a * d[i];
            m[i + 1] = t * b * d[i];
        }
    }
    m
}

/// The RGB part of a grade: wheels, curves and a LUT, in that order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub wheels: Wheels,
    /// Master, red, green, blue ([`CHANNELS`]). The master runs first, then the
    /// channel's own.
    pub curves: [Curve; 4],
    /// The `.cube` file, if any.
    pub lut: Option<crate::cube::LutRef>,
    /// How much of the LUT shows, 0..1: 1 is the LUT's output, 0 the input.
    pub lut_mix: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            wheels: Wheels::default(),
            curves: [Curve::default(); 4],
            lut: None,
            lut_mix: 1.0,
        }
    }
}

impl Look {
    /// Whether this look leaves the picture alone. A LUT at mix 0 does.
    pub fn is_identity(&self) -> bool {
        self.wheels.is_identity()
            && self.curves.iter().all(Curve::is_identity)
            && (self.lut.is_none() || self.lut_mix == 0.0)
    }
}

/// Entries of a channel table: 4096 steps, a quarter of a 10-bit code apart.
const TABLE_N: usize = 4097;

/// One frame's look, ready to run: a table per channel (`None` when the wheels
/// and curves are neutral) and the LUT with its mix.
struct Stage {
    tables: Option<[Vec<f32>; 3]>,
    cube: Option<(std::sync::Arc<crate::cube::Cube>, f32)>,
}

impl Stage {
    /// `None` for a look that does nothing -- a missing or broken LUT included,
    /// which plays as no LUT (its handle says why, for the warnings).
    fn new(look: &Look) -> Option<Self> {
        let shaped = !look.wheels.is_identity() || !look.curves.iter().all(Curve::is_identity);
        let mix = sane(look.lut_mix, 1.0).clamp(0.0, 1.0);
        let cube = look.lut.filter(|_| mix > 0.0).and_then(|r| r.table().ok()).map(|c| (c, mix));
        if !shaped && cube.is_none() {
            return None;
        }
        let tables = shaped.then(|| {
            let master = look.curves[0].sampled(TABLE_N);
            [0, 1, 2].map(|c| {
                let own = look.curves[c + 1].sampled(TABLE_N);
                (0..TABLE_N)
                    .map(|i| {
                        let x = look.wheels.eval(c, i as f32 / (TABLE_N - 1) as f32);
                        let x = lerp_table(&master, x);
                        lerp_table(&own, x)
                    })
                    .collect()
            })
        });
        Some(Self { tables, cube })
    }

    /// One R'G'B' value (0..1) through the stage.
    ///
    /// The stage grades the value clamped to 0..1 and adds back what the clamp
    /// took off: a sample a little outside the gamut (a 4:2:0 edge, a super-white)
    /// keeps its excess instead of being flattened, so an identity LUT is
    /// transparent and the final code clamp is the only one.
    #[inline]
    pub(crate) fn rgb(&self, p: [f32; 3]) -> [f32; 3] {
        let mut q = p;
        if let Some(t) = &self.tables {
            q = [0, 1, 2].map(|k| {
                let c = q[k].clamp(0.0, 1.0);
                lerp_table(&t[k], c) + (q[k] - c)
            });
        }
        if let Some((cube, mix)) = &self.cube {
            let o = cube.sample(q);
            q = [0, 1, 2].map(|k| {
                let held = q[k].clamp(cube.domain_min[k], cube.domain_max[k]);
                q[k] + (o[k] + (q[k] - held) - q[k]) * mix
            });
        }
        q
    }

    /// Runs the stage over planar YUV with `stride == width`, on the code span `k`
    /// (8 or 10 bits, limited range or full). Each chroma
    /// sample becomes the mean of the graded chroma of the luma samples it
    /// covers. A plane layout this cannot read is left alone.
    fn run<T: Sample>(&self, y: &mut [T], u: &mut [T], v: &mut [T], width: usize, k: Codes) {
        if width == 0 || !y.len().is_multiple_of(width) || u.len() != v.len() {
            return;
        }
        let height = y.len() / width;
        let (hw, hh) = (width.div_ceil(2), height.div_ceil(2));
        let (cw, ch) = if u.len() == hw * hh {
            (hw, hh)
        } else if u.len() == hw * height {
            (hw, height)
        } else if u.len() == width * height {
            (width, height)
        } else {
            return;
        };
        let (sx, sy) = (width.div_ceil(cw), height.div_ceil(ch));
        let lanes = std::thread::available_parallelism().map_or(1, |n| n.get()).min(ch).max(1);
        let rows = ch.div_ceil(lanes);
        std::thread::scope(|scope| {
            let mut ys = &mut y[..];
            let mut us = &mut u[..];
            let mut vs = &mut v[..];
            let mut c0 = 0;
            while c0 < ch {
                let c1 = (c0 + rows).min(ch);
                let ly = ((c1 * sy).min(height) - c0 * sy) * width;
                let (yc, yr) = ys.split_at_mut(ly);
                let (uc, ur) = us.split_at_mut((c1 - c0) * cw);
                let (vc, vr) = vs.split_at_mut((c1 - c0) * cw);
                (ys, us, vs) = (yr, ur, vr);
                let k = &k;
                let rows_here = c1 - c0;
                let luma_rows = ly / width;
                scope.spawn(move || {
                    for cy in 0..rows_here {
                        for cx in 0..cw {
                            let ci = cy * cw + cx;
                            let (cb, cr) = (k.chroma(uc[ci].get()), k.chroma(vc[ci].get()));
                            let (mut su, mut sv, mut n) = (0f32, 0f32, 0f32);
                            for dy in 0..sy {
                                let row = cy * sy + dy;
                                if row >= luma_rows {
                                    break;
                                }
                                for dx in 0..sx {
                                    let col = cx * sx + dx;
                                    if col >= width {
                                        break;
                                    }
                                    let at = row * width + col;
                                    let l = k.luma(yc[at].get());
                                    let p = self.rgb(to_rgb(l, cb, cr));
                                    let (l, b, r) = from_rgb(p);
                                    yc[at] = T::put(k.luma_code(l));
                                    su += b;
                                    sv += r;
                                    n += 1.0;
                                }
                            }
                            uc[ci] = T::put(k.chroma_code(su / n));
                            vc[ci] = T::put(k.chroma_code(sv / n));
                        }
                    }
                });
                c0 = c1;
            }
        });
    }
}

#[inline]
fn lerp_table(t: &[f32], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * (t.len() - 1) as f32;
    let i = (p as usize).min(t.len() - 2);
    let d = p - i as f32;
    t[i] + (t[i + 1] - t[i]) * d
}

const KR: f32 = 0.2126;
const KB: f32 = 0.0722;
const KG: f32 = 1.0 - KR - KB;

#[inline]
fn to_rgb(y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let r = y + 2.0 * (1.0 - KR) * cr;
    let b = y + 2.0 * (1.0 - KB) * cb;
    let g = (y - KR * r - KB * b) / KG;
    [r, g, b]
}

#[inline]
fn from_rgb([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let y = KR * r + KG * g + KB * b;
    (y, (b - y) / (2.0 * (1.0 - KB)), (r - y) / (2.0 * (1.0 - KR)))
}

/// Code arithmetic at one depth, limited range or full.
struct Codes {
    black: f32,
    luma: f32,
    mid: f32,
    chroma: f32,
    min: f32,
    max: f32,
}

impl Codes {
    fn new(scale: f32, max: u16, full_range: bool) -> Self {
        let max = f32::from(max);
        if full_range {
            // Whole span: black is 0, white is `max`, and every code is valid.
            return Self { black: 0.0, luma: max, mid: 128.0 * scale, chroma: max, min: 0.0, max };
        }
        // The valid codes: `scale` up from 0 and down from `max` ([`CODES`]).
        Self { black: 16.0 * scale, luma: 219.0 * scale, mid: 128.0 * scale, chroma: 224.0 * scale, min: scale, max: max - scale }
    }
    #[inline]
    fn luma(&self, c: u16) -> f32 {
        (f32::from(c) - self.black) / self.luma
    }
    #[inline]
    fn chroma(&self, c: u16) -> f32 {
        (f32::from(c) - self.mid) / self.chroma
    }
    #[inline]
    fn luma_code(&self, v: f32) -> u16 {
        (v * self.luma + self.black).round().clamp(self.min, self.max) as u16
    }
    #[inline]
    fn chroma_code(&self, v: f32) -> u16 {
        (v * self.chroma + self.mid).round().clamp(self.min, self.max) as u16
    }
}

/// A sample of either depth.
trait Sample: Copy + Send + Sync {
    fn get(self) -> u16;
    fn put(v: u16) -> Self;
}

impl Sample for u8 {
    fn get(self) -> u16 {
        u16::from(self)
    }
    fn put(v: u16) -> Self {
        v.min(255) as u8
    }
}

impl Sample for u16 {
    fn get(self) -> u16 {
        self.min(1023)
    }
    fn put(v: u16) -> Self {
        v
    }
}

/// [`Look`]'s RGB stage on one R'G'B' value in 0..1, for tests and tools that
/// want the maths without a frame.
pub fn look_rgb(look: &Look, p: [f32; 3]) -> [f32; 3] {
    Stage::new(look).map_or(p, |s| s.rgb(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No grade writes a reserved sync code (0 and 255 at 8 bits, 0..=3 and
    /// 1020..=1023 at 10), however hard it pushes, on a frame holding every code
    /// with chroma at both ends; and a super-white or sub-black sample inside
    /// the valid codes is kept, not clipped to 16..235.
    #[test]
    fn grades_write_only_valid_codes_and_keep_super_white() {
        let (w, h) = (64usize, 64usize);
        let y8: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        let u8_: Vec<u8> = (0..w * h / 4).map(|i| if i % 3 == 0 { 0 } else if i % 3 == 1 { 255 } else { (i * 7 % 256) as u8 }).collect();
        let v8: Vec<u8> = (0..w * h / 4).map(|i| if i % 2 == 0 { 255 } else { (i * 13 % 256) as u8 }).collect();
        let push = Wheels { lift: [1.0, -1.0, 1.0, -1.0], gamma: [-1.0, 1.0, 0.0, 1.0], gain: [1.0, 1.0, -1.0, 0.5] };
        let grades = [
            ColorParams { brightness: 1.0, contrast: 2.0, saturation: 2.0, tint: 1.0, ..Default::default() },
            ColorParams { brightness: -1.0, contrast: 2.0, saturation: 2.0, tint: -1.0, ..Default::default() },
            ColorParams { look: Look { wheels: push, ..Default::default() }, ..Default::default() },
            ColorParams { saturation: 2.0, look: Look { wheels: push, ..Default::default() }, ..Default::default() },
        ];
        for params in &grades {
            let (mut y, mut u, mut v) = (y8.clone(), u8_.clone(), v8.clone());
            apply_yuv(params, &mut y, &mut u, &mut v, w, false);
            for s in y.iter().chain(&u).chain(&v) {
                assert!((CODES.0..=CODES.1).contains(s), "8-bit code {s} from {params:?}");
            }
            let wide = |p: &[u8]| p.iter().map(|&c| u16::from(c) * 4 + c as u16 / 64).collect::<Vec<u16>>();
            let (mut y, mut u, mut v) = (wide(&y8), wide(&u8_), wide(&v8));
            apply_yuv_wide(params, &mut y, &mut u, &mut v, w, false);
            for s in y.iter().chain(&u).chain(&v) {
                assert!((WIDE_CODES.0..=WIDE_CODES.1).contains(s), "10-bit code {s} from {params:?}");
            }
        }
        // A barely-there look on grey chroma: super-white 250 and sub-black 5
        // come out where they went in, inside the valid codes.
        let grey = ColorParams {
            look: Look { wheels: Wheels { gain: [0.001, 0.0, 0.0, 0.0], ..Default::default() }, ..Default::default() },
            ..Default::default()
        };
        let mut y = vec![250u8, 250, 5, 5, 250, 250, 5, 5];
        let (mut u, mut v) = (vec![128u8; 2], vec![128u8; 2]);
        apply_yuv(&grey, &mut y, &mut u, &mut v, 4, false);
        assert!(y.iter().zip([250u8, 250, 5, 5, 250, 250, 5, 5]).all(|(a, b)| a.abs_diff(b) <= 1), "{y:?}");
        let mut y = vec![1000u16, 1000, 20, 20, 1000, 1000, 20, 20];
        let (mut u, mut v) = (vec![512u16; 2], vec![512u16; 2]);
        apply_yuv_wide(&grey, &mut y, &mut u, &mut v, 4, false);
        assert!(y.iter().zip([1000u16, 1000, 20, 20, 1000, 1000, 20, 20]).all(|(a, b)| a.abs_diff(b) <= 2), "{y:?}");
    }

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
        apply_yuv(&ColorParams::default(), &mut gy, &mut gu, &mut gv, 64, false);
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
            64, false,
        );
        // Untouched but for the reserved sync codes 0 and 255, which no grade
        // writes ([`CODES`]).
        let valid: Vec<u8> = before.iter().map(|&c| c.clamp(CODES.0, CODES.1)).collect();
        assert_eq!(y, valid, "saturation must not touch luma");
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
                    ..Default::default()
                }, false);
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
        }, false);
        assert_eq!(lut.y[255], 254, "full brightness must saturate at the last valid code, not wrap");
        assert_eq!(lut.y[0], 219, "black lifted by the whole luma range");
        let lut = Lut::new(&ColorParams {
            brightness: -1.0,
            ..Default::default()
        }, false);
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
                64, false,
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
            ..Default::default()
        };
        let lut = Lut::new(&params, false);
        for code in 0..256 {
            let f = code as f32;
            let y = ((f - MID_Y) * 1.4 + MID_Y + 0.13 * LUMA_RANGE)
                .round()
                .clamp(1.0, 254.0) as u8;
            let u = ((f - 128.0) * 0.7 + 128.0 + 0.35 * TINT_RANGE)
                .round()
                .clamp(1.0, 254.0) as u8;
            let v = ((f - 128.0) * 0.7 + 128.0 - 0.35 * TINT_RANGE)
                .round()
                .clamp(1.0, 254.0) as u8;
            assert!(lut.y[code].abs_diff(y) <= 1, "Y {code}");
            assert!(lut.u[code].abs_diff(u) <= 1, "U {code}");
            assert!(lut.v[code].abs_diff(v) <= 1, "V {code}");
        }
    }

    /// Not asserted: a debug build is an order of magnitude off and the number
    /// only means something in release. Run with
    /// `cargo test -p engine --release color:: -- --nocapture`.
    #[test]
    #[ignore = "perf: run with --ignored"]
    fn perf_1080p() {
        let (mut y, mut u, mut v) = frame(1920, 1080);
        let params = ColorParams {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 1.3,
            tint: 0.2,
            ..Default::default()
        };
        let runs = 30;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            apply_yuv(&params, &mut y, &mut u, &mut v, 1920, false);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
        println!("color::apply_yuv 1920x1080: {ms:.3} ms/frame");
    }

    fn looked(look: Look) -> ColorParams {
        ColorParams { look, ..Default::default() }
    }

    fn warm_cube() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("edith-color-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("warm.cube");
        let mut t = String::from("LUT_3D_SIZE 5\n");
        for b in 0..5 {
            for g in 0..5 {
                for r in 0..5 {
                    let (r, g, b) = (r as f32 / 4.0, g as f32 / 4.0, b as f32 / 4.0);
                    t.push_str(&format!("{} {} {}\n", (r * 1.1 + 0.03).min(1.0), g, b * 0.85));
                }
            }
        }
        std::fs::write(&path, t).expect("write");
        path
    }

    /// Neutral wheels, curves whose points sit on the diagonal, and a LUT at mix
    /// 0 are all "nothing set": the bytes come back as they went in, at both
    /// depths and through the fused conversion.
    #[test]
    fn a_neutral_look_leaves_every_byte_alone() {
        let diagonal = Curve::new(&[[0.25, 0.25], [0.75, 0.75]]).expect("curve");
        let look = Look {
            curves: [diagonal; 4],
            lut: Some(crate::cube::LutRef::open(&warm_cube())),
            lut_mix: 0.0,
            ..Default::default()
        };
        let p = looked(look);
        assert!(p.is_identity());
        let (y, u, v) = frame(64, 48);
        let (mut gy, mut gu, mut gv) = (y.clone(), u.clone(), v.clone());
        apply_yuv(&p, &mut gy, &mut gu, &mut gv, 64, false);
        assert_eq!((gy, gu, gv), (y.clone(), u.clone(), v.clone()));
        let w = |s: &[u8]| s.iter().map(|&c| u16::from(c) * 4 + 1).collect::<Vec<u16>>();
        let (wy, wu, wv) = (w(&y), w(&u), w(&v));
        let (mut gy, mut gu, mut gv) = (wy.clone(), wu.clone(), wv.clone());
        apply_yuv_wide(&p, &mut gy, &mut gu, &mut gv, 64, false);
        assert_eq!((gy, gu, gv), (wy, wu, wv));
    }

    /// A limited-range grey ramp with neutral chroma, graded through a look.
    fn graded_grey(look: Look, wide: bool) -> Vec<f32> {
        let p = looked(look);
        if wide {
            let mut y: Vec<u16> = (0..=219).map(|c| (16 + c) * 4).collect();
            let n = y.len();
            let (mut u, mut v) = (vec![512u16; n.div_ceil(2)], vec![512u16; n.div_ceil(2)]);
            apply_yuv_wide(&p, &mut y, &mut u, &mut v, n, false);
            y.iter().map(|&c| f32::from(c) / 4.0).collect()
        } else {
            let mut y: Vec<u8> = (16..=235).collect();
            let n = y.len();
            let (mut u, mut v) = (vec![128u8; n.div_ceil(2)], vec![128u8; n.div_ceil(2)]);
            apply_yuv(&p, &mut y, &mut u, &mut v, n, false);
            y.iter().map(|&c| f32::from(c)).collect()
        }
    }

    #[test]
    fn wheels_move_the_ends_they_say_at_both_depths() {
        let lift = Look { wheels: Wheels { lift: [0.5, 0.0, 0.0, 0.0], ..Default::default() }, ..Default::default() };
        let gain = Look { wheels: Wheels { gain: [-0.5, 0.0, 0.0, 0.0], ..Default::default() }, ..Default::default() };
        let gamma = Look { wheels: Wheels { gamma: [0.5, 0.0, 0.0, 0.0], ..Default::default() }, ..Default::default() };
        for wide in [false, true] {
            let l = graded_grey(lift, wide);
            assert!((l[0] - (16.0 + 0.25 * 219.0)).abs() <= 1.0, "lift 0.5 puts black at a quarter: {}", l[0]);
            assert!((l[219] - 235.0).abs() <= 0.5, "lift keeps white");
            let g = graded_grey(gain, wide);
            assert!((g[0] - 16.0).abs() <= 0.5 && (g[219] - (16.0 + 0.5 * 219.0)).abs() <= 1.0, "gain halves from black");
            let m = graded_grey(gamma, wide);
            assert!(m[110] > 125.0 + 10.0 && (m[0] - 16.0).abs() <= 0.5 && (m[219] - 235.0).abs() <= 0.5, "gamma lifts mids only");
            assert!(m.windows(2).all(|w| w[1] >= w[0]));
        }
        // 8 and 10 bit agree to within an 8-bit code.
        for look in [lift, gain, gamma] {
            let (a, b) = (graded_grey(look, false), graded_grey(look, true));
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1.0));
        }
    }

    #[test]
    fn a_red_lift_warms_without_touching_luma_much() {
        let look = Look { wheels: Wheels { lift: [0.0, 0.3, 0.0, -0.1], ..Default::default() }, ..Default::default() };
        let out = look_rgb(&look, [0.1, 0.1, 0.1]);
        assert!(out[0] > 0.2 && (out[1] - 0.1).abs() < 1e-5 && out[2] < 0.1, "{out:?}");
    }

    #[test]
    fn curves_are_monotone_and_pass_through_their_points() {
        let pts = [[0.1, 0.02], [0.3, 0.4], [0.5, 0.45], [0.7, 0.9], [0.9, 0.92]];
        let c = Curve::new(&pts).expect("curve");
        let t = c.sampled(1001);
        assert!(t.windows(2).all(|w| w[1] >= w[0] - 1e-6), "monotone through rising points");
        for p in pts {
            assert!((t[(p[0] * 1000.0).round() as usize] - p[1]).abs() < 1e-3, "{p:?}");
        }
        assert!((t[0]).abs() < 1e-6 && (t[1000] - 1.0).abs() < 1e-6, "pinned ends");
        assert!(Curve::new(&[[0.5, 0.5], [0.4, 0.1]]).is_err());
        assert!(Curve::new(&[[1.5, 0.5]]).is_err());
        assert!(Curve::new(&[[f32::NAN, 0.5]]).is_err());
        assert!(Curve::new(&[[0.0, 0.0]; 17]).is_err());
        // A master S-curve darkens the shadows and brightens the highlights.
        let s = Look { curves: [Curve::new(&[[0.25, 0.15], [0.75, 0.85]]).expect("s"), Curve::default(), Curve::default(), Curve::default()], ..Default::default() };
        let g = graded_grey(s, false);
        assert!(g[55] < 16.0 + 55.0 - 10.0 && g[165] > 16.0 + 165.0 + 10.0);
    }

    #[test]
    fn the_lut_is_mixed_by_intensity_and_a_missing_one_plays_without_it() {
        let lut = crate::cube::LutRef::open(&warm_cube());
        let full = look_rgb(&Look { lut: Some(lut), ..Default::default() }, [0.5, 0.5, 0.5]);
        assert!((full[0] - 0.58).abs() < 1e-4 && (full[2] - 0.425).abs() < 1e-4, "{full:?}");
        let half = look_rgb(&Look { lut: Some(lut), lut_mix: 0.5, ..Default::default() }, [0.5, 0.5, 0.5]);
        assert!((half[0] - 0.54).abs() < 1e-4, "{half:?}");
        let gone = crate::cube::LutRef::open(std::path::Path::new("/nonexistent/edith/x.cube"));
        let p = looked(Look { lut: Some(gone), ..Default::default() });
        let (y, u, v) = frame(64, 48);
        let (mut gy, mut gu, mut gv) = (y.clone(), u.clone(), v.clone());
        apply_yuv(&p, &mut gy, &mut gu, &mut gv, 64, false);
        assert_eq!((gy, gu, gv), (y, u, v), "a missing LUT is no LUT, never a crash");
    }

    /// Keys animate the sliders; the look under them is the clip's.
    #[test]
    fn keys_keep_the_base_look() {
        let base = looked(Look { lut_mix: 0.4, wheels: Wheels { gain: [0.2; 4], ..Default::default() }, ..Default::default() });
        let key = |at, b| ColorKey { at, params: ColorParams { brightness: b, ..Default::default() }, ease: crate::transform::KeyEase::Linear };
        let got = evaluate_color(base, &[key(0, 0.0), key(10, 0.5)], 5.0);
        assert!((got.brightness - 0.25).abs() < 1e-6);
        assert_eq!(got.look, base.look);
    }

    /// The look's RGB stage on a 1080p frame, printed. Release:
    /// `cargo test -p engine --release color::tests::perf_look -- --nocapture`.
    #[test]
    #[ignore = "perf: run with --ignored"]
    fn perf_look_1080p() {
        let (mut y, mut u, mut v) = frame(1920, 1080);
        let p = looked(Look {
            wheels: Wheels { lift: [0.05, 0.1, 0.0, -0.05], ..Default::default() },
            lut: Some(crate::cube::LutRef::open(&warm_cube())),
            ..Default::default()
        });
        let t = std::time::Instant::now();
        for _ in 0..10 {
            apply_yuv(&p, &mut y, &mut u, &mut v, 1920, false);
        }
        println!("color look 1920x1080: {:.3} ms/frame", t.elapsed().as_secs_f64() * 100.0);
    }

    /// A full-range source: identity (sliders and a neutral look) is bit-exact at
    /// both depths, and a lift reaches the true ends of 0..255 / 0..1023 where the
    /// limited arithmetic would stop at 1 and 254.
    #[test]
    fn full_range_sources_grade_over_the_whole_span() {
        let ramp: Vec<u8> = (0..=255u8).collect();
        let (cu, cv) = (vec![128u8; 64], vec![128u8; 64]);
        let neutral = ColorParams { look: Look { wheels: Wheels::default(), ..Look::default() }, ..Default::default() };
        let (mut y, mut u, mut v) = (ramp.clone(), cu.clone(), cv.clone());
        apply_yuv(&neutral, &mut y, &mut u, &mut v, 16, true);
        assert_eq!((&y, &u, &v), (&ramp, &cu, &cv));
        let wide: Vec<u16> = (0..1024u16).collect();
        let (mut y, mut u, mut v) = (wide.clone(), vec![512u16; 256], vec![512u16; 256]);
        apply_yuv_wide(&neutral, &mut y, &mut u, &mut v, 32, true);
        assert_eq!(y, wide);
        assert!(u.iter().all(|&c| c == 512));

        // Brightness down then up: the ends of the span are reachable.
        let down = ColorParams { brightness: -1.0, ..Default::default() };
        let up = ColorParams { brightness: 1.0, ..Default::default() };
        let (mut y, mut u, mut v) = (ramp.clone(), cu.clone(), cv.clone());
        apply_yuv(&down, &mut y, &mut u, &mut v, 16, true);
        assert_eq!(y[255], 0);
        let (mut y, mut u, mut v) = (ramp.clone(), cu.clone(), cv.clone());
        apply_yuv(&up, &mut y, &mut u, &mut v, 16, true);
        assert_eq!((y[0], y[255]), (255, 255));
        // A lift wheel on black reaches 0 and 255 through the RGB stage.
        let lift = |amount: f32| ColorParams { look: Look { wheels: Wheels { lift: [amount, 0.0, 0.0, 0.0], ..Default::default() }, ..Look::default() }, ..Default::default() };
        let (mut y, mut u, mut v) = (vec![0u8, 255, 0, 255], vec![128u8], vec![128u8]);
        apply_yuv(&lift(0.0001), &mut y, &mut u, &mut v, 2, true);
        assert_eq!((y[0], y[1]), (0, 255), "a negligible lift keeps the true ends");
        let (mut y, mut u, mut v) = (vec![0u8; 4], vec![128u8], vec![128u8]);
        apply_yuv(&lift(-1.0), &mut y, &mut u, &mut v, 2, true);
        assert_eq!(y[0], 0);
        let (mut y, mut u, mut v) = (vec![255u8; 4], vec![128u8], vec![128u8]);
        apply_yuv(&ColorParams { look: Look { wheels: Wheels { gain: [1.0, 0.0, 0.0, 0.0], ..Default::default() }, ..Look::default() }, ..Default::default() }, &mut y, &mut u, &mut v, 2, true);
        assert_eq!(y[0], 255);
        let (mut y, mut u, mut v) = (vec![0u16; 4], vec![512u16], vec![512u16]);
        apply_yuv_wide(&down, &mut y, &mut u, &mut v, 2, true);
        assert_eq!(y[0], 0);
    }
}
