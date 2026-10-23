//! Generated clips: pictures made from a few numbers instead of a file -- a solid colour, a
//! gradient, a countdown, colour bars -- and the adjustment layer, a clip with no picture whose
//! grade and effects reach everything beneath it.
//!
//! A generator is a *source* like any other, so a clip of one is placed, trimmed, graded,
//! transformed and saved by the machinery every still already goes through. Its "file" is a
//! pseudo-path that spells the generator out (`/edith-gen/gradient-ff0000-0000ff-45-1920x1080.gen`);
//! [`is_generated`] recognises it, `is_image` admits it (a generator has a picture, no sound and
//! no length of its own), and [`crate::decode::Still`] draws it instead of opening a file. Editing
//! a parameter is a new spelling, so the clip is pointed at the source with the new name.
//!
//! The countdown is the one animated kind: its picture depends on the source frame, which
//! [`Gen::animated`] tells the layered compositor so it asks for each frame afresh.

#![allow(clippy::chunks_exact_to_as_chunks)]

use std::path::{Path, PathBuf};

/// The folder every generator pseudo-path lives in. Never a real directory.
const DIR: &str = "/edith-gen";

/// The frames-per-second a countdown is spelled with, in millihertz, so the path stays integral.
const MILLI: f64 = 1000.0;

/// What a generator draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One colour, `0xRRGGBB`.
    Solid { rgb: u32 },
    /// `a` to `b` along `angle` degrees (0 left to right, 90 top to bottom).
    Gradient { a: u32, b: u32, angle: u16 },
    /// Counts down from `from` seconds with a big centred number and a sweep. `milli_fps` is
    /// the timeline's rate in millihertz, which turns a source frame into a second.
    Countdown { from: u16, milli_fps: u32 },
    /// SMPTE-style colour bars.
    Bars,
    /// No picture: the clip's grade, effects and transform act on what is composed beneath it.
    Adjust,
    /// A drawn shape on a transparent canvas ([`Shape`]).
    Shape(Shape),
}

/// What a shape draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A box, with `radius` rounding its corners.
    Rectangle,
    Ellipse,
    /// A stroke across the middle of the canvas, left to right, round at both ends.
    Line,
    /// A line that ends in a filled head at the right.
    Arrow,
}

/// A shape on a transparent canvas: it fills the canvas (the clip's transform sizes and places
/// it), `fill` inside, `stroke` as a band of `width` pixels along the inside of the edge. A
/// line and an arrow are all stroke. `None` is no paint, so the canvas shows through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub form: Form,
    pub fill: Option<u32>,
    pub stroke: Option<u32>,
    /// Stroke width in pixels of the canvas, 0 to 400.
    pub width: u16,
    /// Corner radius of a rectangle in pixels of the canvas, 0 to 2000.
    pub radius: u16,
}

impl Form {
    /// The forms a front-end offers, in menu order.
    pub const ALL: [Form; 4] = [Form::Rectangle, Form::Ellipse, Form::Line, Form::Arrow];

    pub fn name(self) -> &'static str {
        match self {
            Form::Rectangle => "rectangle",
            Form::Ellipse => "ellipse",
            Form::Line => "line",
            Form::Arrow => "arrow",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Form::Rectangle => "Rectangle",
            Form::Ellipse => "Ellipse",
            Form::Line => "Line",
            Form::Arrow => "Arrow",
        }
    }

    /// Whether the form has an inside to fill.
    pub fn fills(self) -> bool {
        matches!(self, Form::Rectangle | Form::Ellipse)
    }

    fn by_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }
}

impl Shape {
    /// The default of a form: a blue fill for the ones that fill, a stroke for the ones that are one.
    pub fn of(form: Form) -> Self {
        match form {
            Form::Rectangle | Form::Ellipse => Self { form, fill: Some(0x2a6df4), stroke: None, width: 0, radius: 0 },
            Form::Line => Self { form, fill: None, stroke: Some(0xffffff), width: 12, radius: 0 },
            Form::Arrow => Self { form, fill: None, stroke: Some(0xff3b30), width: 14, radius: 0 },
        }
    }

    /// The RGBA8 picture, straight alpha, for a `w` x `h` canvas. Transparent pixels carry the
    /// colour of the nearest paint, so a scaled edge does not pick up a fringe.
    fn rgba(&self, w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; w * h * 4];
        let (fw, fh) = (w as f32, h as f32);
        let sw = f32::from(self.width);
        let stroke = self.stroke.filter(|_| self.width > 0).map(split);
        let fill = self.fill.filter(|_| self.form.fills()).map(split);
        // The default paint under transparent pixels.
        let base = stroke.or(fill).unwrap_or([0, 0, 0]);
        let (cx, cy) = (fw / 2.0, fh / 2.0);
        // Line and arrow: the stroke is the shape, centred on the middle row, inset from the edges.
        let pad = sw * 0.5 + 1.0;
        let head_len = (sw * 4.5).max(10.0).min(fw * 0.45);
        let head_half = (sw * 2.4).max(6.0).min(fh * 0.45);
        let tip = (fw - 1.0, cy);
        let neck = (fw - 1.0 - head_len, cy);
        let shaft_end = (neck.0 + head_len * 0.25, cy);
        let tri = [tip, (neck.0, cy - head_half), (neck.0, cy + head_half)];
        let r_max = fw.min(fh) / 2.0;
        let radius = f32::from(self.radius).min(r_max);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Signed distance to the edge: negative inside.
                let d = match self.form {
                    Form::Rectangle => {
                        let (qx, qy) = ((px - cx).abs() - (fw / 2.0 - radius), (py - cy).abs() - (fh / 2.0 - radius));
                        qx.max(qy).min(0.0) + qx.max(0.0).hypot(qy.max(0.0)) - radius
                    }
                    Form::Ellipse => {
                        let (rx, ry) = (fw / 2.0, fh / 2.0);
                        let (ex, ey) = ((px - cx) / rx, (py - cy) / ry);
                        let k0 = ex.hypot(ey);
                        let k1 = ((ex / rx).hypot(ey / ry)).max(1e-9);
                        k0 * (k0 - 1.0) / k1
                    }
                    Form::Line => seg((px, py), (pad, cy), (fw - pad, cy)) - sw / 2.0,
                    Form::Arrow => {
                        let shaft = seg((px, py), (pad, cy), (shaft_end.0, cy)) - sw / 2.0;
                        shaft.min(tri_dist((px, py), tri))
                    }
                };
                let outside = !self.form.fills();
                // Coverage of the whole shape and of its stroke band.
                let outer = (0.5 - d).clamp(0.0, 1.0);
                let (alpha, rgb) = if outside {
                    (outer, stroke.unwrap_or(base))
                } else {
                    let band = (0.5 + sw - (-d)).clamp(0.0, 1.0);
                    match (fill, stroke) {
                        (Some(f), Some(s)) => (outer, mix3(f, s, band)),
                        (Some(f), None) => (outer, f),
                        (None, Some(s)) => (outer * band, s),
                        (None, None) => (0.0, base),
                    }
                };
                let o = &mut out[(y * w + x) * 4..][..4];
                o[..3].copy_from_slice(&rgb);
                o[3] = (alpha * 255.0).round() as u8;
            }
        }
        out
    }
}

fn mix3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// Distance from `p` to the segment `a`-`b`.
fn seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (pa, ba) = ((p.0 - a.0, p.1 - a.1), (b.0 - a.0, b.1 - a.1));
    let t = ((pa.0 * ba.0 + pa.1 * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1).max(1e-9)).clamp(0.0, 1.0);
    (pa.0 - ba.0 * t).hypot(pa.1 - ba.1 * t)
}

/// Signed distance from `p` to the triangle `t` (negative inside).
fn tri_dist(p: (f32, f32), t: [(f32, f32); 3]) -> f32 {
    let d = (0..3).map(|i| seg(p, t[i], t[(i + 1) % 3])).fold(f32::MAX, f32::min);
    let side = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
    let (s0, s1, s2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
    let inside = (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0) || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0);
    if inside { -d } else { d }
}

/// A generator and the canvas it is drawn for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gen {
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
}

/// The kinds a front-end offers by name, in menu order.
pub const KINDS: [&str; 9] = ["solid", "gradient", "countdown", "bars", "adjust", "rectangle", "ellipse", "line", "arrow"];

impl Kind {
    /// The word a path, a file and the MCP use.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Solid { .. } => "solid",
            Kind::Gradient { .. } => "gradient",
            Kind::Countdown { .. } => "countdown",
            Kind::Bars => "bars",
            Kind::Adjust => "adjust",
            Kind::Shape(s) => s.form.name(),
        }
    }

    /// The name a person reads on a clip or a library row.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Solid { .. } => "Solid colour",
            Kind::Gradient { .. } => "Gradient",
            Kind::Countdown { .. } => "Countdown",
            Kind::Bars => "Colour bars",
            Kind::Adjust => "Adjustment layer",
            Kind::Shape(s) => s.form.title(),
        }
    }
}

impl Gen {
    pub fn new(kind: Kind, width: u32, height: u32) -> Self {
        Self { kind, width: width.clamp(16, 7680), height: height.clamp(16, 7680) }
    }

    /// The default of a kind by name at the canvas `width` x `height` and timeline `fps`.
    pub fn named(name: &str, width: u32, height: u32, fps: f64) -> Option<Self> {
        let kind = match name {
            "solid" => Kind::Solid { rgb: 0x2a6df4 },
            "gradient" => Kind::Gradient { a: 0xff7a18, b: 0x3a1c71, angle: 90 },
            "countdown" => Kind::Countdown { from: 5, milli_fps: (fps * MILLI).round() as u32 },
            "bars" => Kind::Bars,
            "adjust" => Kind::Adjust,
            other => Kind::Shape(Shape::of(Form::by_name(other)?)),
        };
        Some(Self::new(kind, width, height))
    }

    /// Whether the picture depends on the source frame.
    pub fn animated(&self) -> bool {
        matches!(self.kind, Kind::Countdown { .. })
    }

    /// Whether the picture has a transparent background ([`Gen::alpha`]).
    pub fn has_alpha(&self) -> bool {
        matches!(self.kind, Kind::Shape(_))
    }

    /// The scale a new clip of this generator is placed at: a shape lands as a shape on the
    /// picture, not a full-frame cover.
    pub fn default_scale(&self) -> Option<f32> {
        self.has_alpha().then_some(0.4)
    }

    /// The coverage of each pixel, 0 to 255, for a generator with a transparent background.
    pub fn alpha(&self) -> Option<Vec<u8>> {
        let Kind::Shape(s) = self.kind else { return None };
        Some(s.rgba(self.width as usize, self.height as usize).chunks_exact(4).map(|p| p[3]).collect())
    }

    /// Whether this is the adjustment layer.
    pub fn is_adjust(&self) -> bool {
        self.kind == Kind::Adjust
    }

    /// The pseudo-path that names this generator as a source.
    pub fn path(&self) -> PathBuf {
        let (w, h) = (self.width, self.height);
        let spec = match self.kind {
            Kind::Solid { rgb } => format!("solid-{rgb:06x}-{w}x{h}"),
            Kind::Gradient { a, b, angle } => format!("gradient-{a:06x}-{b:06x}-{angle}-{w}x{h}"),
            Kind::Countdown { from, milli_fps } => format!("countdown-{from}-{milli_fps}-{w}x{h}"),
            Kind::Bars => format!("bars-{w}x{h}"),
            Kind::Adjust => format!("adjust-{w}x{h}"),
            Kind::Shape(s) => {
                let paint = |c: Option<u32>| c.map_or("none".to_string(), |c| format!("{c:06x}"));
                format!("{}-{}-{}-{}-{}-{w}x{h}", s.form.name(), paint(s.fill), paint(s.stroke), s.width, s.radius)
            }
        };
        PathBuf::from(format!("{DIR}/{spec}.gen"))
    }

    /// The generator a pseudo-path spells, `None` for any real path.
    pub fn parse(path: &Path) -> Option<Self> {
        if path.parent()? != Path::new(DIR) || path.extension()? != "gen" {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        let f: Vec<&str> = stem.split('-').collect();
        let hex = |s: &str| u32::from_str_radix(s, 16).ok().filter(|v| *v <= 0xff_ffff && s.len() == 6);
        let (size, kind) = match f.as_slice() {
            ["solid", c, size] => (size, Kind::Solid { rgb: hex(c)? }),
            ["gradient", a, b, angle, size] => {
                (size, Kind::Gradient { a: hex(a)?, b: hex(b)?, angle: angle.parse::<u16>().ok().filter(|a| *a < 360)? })
            }
            ["countdown", from, fps, size] => (
                size,
                Kind::Countdown { from: from.parse().ok().filter(|f| (1..=3600).contains(f))?, milli_fps: fps.parse().ok().filter(|f| *f > 0)? },
            ),
            ["bars", size] => (size, Kind::Bars),
            ["adjust", size] => (size, Kind::Adjust),
            [form, fill, stroke, width, radius, size] => {
                let paint = |s: &str| if s == "none" { Some(None) } else { hex(s).map(Some) };
                let shape = Shape {
                    form: Form::by_name(form)?,
                    fill: paint(fill)?,
                    stroke: paint(stroke)?,
                    width: width.parse().ok().filter(|v| *v <= 400)?,
                    radius: radius.parse().ok().filter(|v| *v <= 2000)?,
                };
                (size, Kind::Shape(shape))
            }
            _ => return None,
        };
        let (w, h) = size.split_once('x')?;
        let (width, height): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
        ((16..=7680).contains(&width) && (16..=7680).contains(&height)).then_some(Self { kind, width, height })
    }

    /// The packed RGB8 picture at source frame `frame`.
    pub fn rgb(&self, frame: u32) -> Vec<u8> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; w * h * 3];
        match self.kind {
            Kind::Solid { rgb } => {
                let c = split(rgb);
                out.chunks_exact_mut(3).for_each(|p| p.copy_from_slice(&c));
            }
            Kind::Gradient { a, b, angle } => {
                let (ca, cb) = (split(a), split(b));
                let rad = f32::from(angle).to_radians();
                let (dx, dy) = (rad.cos(), rad.sin());
                // The projection of the whole frame on the direction: the corners are t 0 and 1.
                let span = (w as f32 * dx).abs() + (h as f32 * dy).abs();
                let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
                for y in 0..h {
                    for x in 0..w {
                        let p = ((x as f32 + 0.5 - cx) * dx + (y as f32 + 0.5 - cy) * dy) / span.max(1.0);
                        let t = (0.5 + p).clamp(0.0, 1.0);
                        let px = &mut out[(y * w + x) * 3..][..3];
                        for k in 0..3 {
                            px[k] = (f32::from(ca[k]) + (f32::from(cb[k]) - f32::from(ca[k])) * t).round() as u8;
                        }
                    }
                }
            }
            Kind::Bars => {
                // 75% white, yellow, cyan, green, magenta, red, blue.
                const BARS: [[u8; 3]; 7] = [
                    [191, 191, 191],
                    [191, 191, 0],
                    [0, 191, 191],
                    [0, 191, 0],
                    [191, 0, 191],
                    [191, 0, 0],
                    [0, 0, 191],
                ];
                for y in 0..h {
                    for x in 0..w {
                        out[(y * w + x) * 3..][..3].copy_from_slice(&BARS[x * 7 / w]);
                    }
                }
            }
            Kind::Countdown { from, milli_fps } => self.countdown(&mut out, from, milli_fps, frame),
            Kind::Adjust => out.fill(128),
            Kind::Shape(s) => {
                let rgba = s.rgba(w, h);
                for (p, q) in out.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
                    p.copy_from_slice(&q[..3]);
                }
            }
        }
        out
    }

    /// A film-leader countdown: a dark field, a ring, a lighter sweep that fills the ring once a
    /// second, and the whole seconds left as a big white numeral in the middle.
    fn countdown(&self, out: &mut [u8], from: u16, milli_fps: u32, frame: u32) {
        let (w, h) = (self.width as usize, self.height as usize);
        let t = f64::from(frame) / (f64::from(milli_fps) / MILLI);
        let whole = t.floor();
        let left = (f64::from(from) - whole).max(0.0) as u32;
        // Past the end the sweep rests empty rather than looping.
        let sweep = if f64::from(from) - whole <= 0.0 { 0.0 } else { (t - whole) as f32 };
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let radius = h as f32 * 0.38;
        let ring = (h as f32 * 0.008).max(2.0);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let mut v = 32u8;
                if d <= radius {
                    // Clockwise from twelve o'clock, 0 to 1.
                    let a = (dx.atan2(-dy) / std::f32::consts::TAU).rem_euclid(1.0);
                    if a <= sweep {
                        v = 72;
                    }
                }
                if (d - radius).abs() <= ring {
                    v = 235;
                }
                out[(y * w + x) * 3..][..3].fill(v);
            }
        }
        // The numeral goes through the text renderer, on a BGRA copy of the frame.
        let mut bgra = vec![255u8; w * h * 4];
        for (px, rgb) in bgra.chunks_exact_mut(4).zip(out.chunks_exact(3)) {
            px[0] = rgb[2];
            px[1] = rgb[1];
            px[2] = rgb[0];
        }
        let style = crate::text::TextStyle {
            size: crate::text::SIZE_RANGE.1,
            boxed: false,
            place: crate::text::Place::Centre,
            outline: crate::text::Outline::Thin,
            ..Default::default()
        };
        crate::text::draw_text(&mut bgra, self.width, self.height, &style, &[left.to_string()]);
        for (px, rgb) in bgra.chunks_exact(4).zip(out.chunks_exact_mut(3)) {
            rgb[0] = px[2];
            rgb[1] = px[1];
            rgb[2] = px[0];
        }
    }
}

fn split(rgb: u32) -> [u8; 3] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

/// Whether `path` is a generator's pseudo-path rather than a file.
pub fn is_generated(path: &Path) -> bool {
    Gen::parse(path).is_some()
}

/// Whether `path` is the adjustment layer's pseudo-path.
pub fn is_adjust(path: &Path) -> bool {
    Gen::parse(path).is_some_and(|g| g.is_adjust())
}

/// What a source is called where a file name would be shown.
pub fn title(path: &Path) -> Option<&'static str> {
    Gen::parse(path).map(|g| g.kind.title())
}

/// What a tone file is called where a name is shown: "Tone 1 kHz · −20 dB", read back from the
/// file name `Tone::file_name` made. `None` for any other file.
pub fn tone_title(path: &Path) -> Option<String> {
    let stem = path.file_name()?.to_str()?.strip_prefix("tone-")?.strip_suffix(".wav")?;
    let (hz, rest) = stem.split_once("hz-")?;
    let (db, _secs) = rest.split_once("dbfs-")?;
    let hz: u32 = hz.parse().ok()?;
    let db: i32 = db.parse().ok()?;
    let freq = if hz >= 1000 { format!("{} kHz", f64::from(hz) / 1000.) } else { format!("{hz} Hz") };
    let level = if db < 0 { format!("\u{2212}{} dB", -db) } else { format!("{db} dB") };
    Some(format!("Tone {freq} \u{00b7} {level}"))
}

/// A test tone: a sine at `hz` whose RMS level is `dbfs` (broadcast's reading: a sine's RMS is 3 dB
/// under its peak, so -20 dBFS is a peak of 0.14). It is made once as a mono 16-bit WAV in the
/// user's data folder and placed like any sound file, so mixing, gain, ducking and export need
/// nothing new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    /// Hertz, 20 to 20000 and below half the sample rate.
    pub hz: u32,
    /// RMS level in whole dB, -60 to -3 (-3 is a peak of full scale).
    pub dbfs: i32,
}

/// The sample rate tone files are written at.
pub const TONE_RATE: u32 = 48_000;
/// The length a tone is made at when none is asked for, in seconds.
pub const TONE_SECS: u32 = 10;

impl Tone {
    /// 1 kHz at -20 dBFS, the reference level.
    pub const DEFAULT: Tone = Tone { hz: 1000, dbfs: -20 };

    /// The tone as its settings allow: the frequency and level clamped into their ranges.
    pub fn new(hz: u32, dbfs: i32) -> Self {
        Self { hz: hz.clamp(20, 20_000), dbfs: dbfs.clamp(-60, -3) }
    }

    /// The file name the tone of `secs` seconds is kept under.
    pub fn file_name(&self, secs: u32) -> String {
        format!("tone-{}hz-{}dbfs-{secs}s.wav", self.hz, self.dbfs)
    }

    /// The folder tone files of an untitled project are kept in: `$XDG_DATA_HOME/edith/tones`.
    /// A saved project keeps its tones beside its file instead (the caller chooses).
    pub fn dir() -> Option<std::path::PathBuf> {
        Some(Self::dir_in(&crate::templates::data_home()?))
    }

    /// The tones folder under the data folder `data`.
    pub fn dir_in(data: &std::path::Path) -> std::path::PathBuf {
        data.join("edith").join("tones")
    }

    /// The tone's WAV bytes, `secs` long: 16-bit mono PCM at [`TONE_RATE`].
    pub fn wav(&self, secs: u32) -> Vec<u8> {
        let n = u64::from(secs) * u64::from(TONE_RATE);
        let data_len = (n * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&TONE_RATE.to_le_bytes());
        out.extend_from_slice(&(TONE_RATE * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        let peak = 10f64.powf(f64::from(self.dbfs) / 20.0) * std::f64::consts::SQRT_2;
        let step = std::f64::consts::TAU * f64::from(self.hz) / f64::from(TONE_RATE);
        for i in 0..n {
            let v = peak * (step * i as f64).sin();
            out.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes());
        }
        out
    }

    /// Writes the tone of `secs` seconds into `dir` (made when missing) and returns its path. A
    /// file already there is reused, since its name spells out every setting it has.
    pub fn write(&self, dir: &Path, secs: u32) -> crate::Result<PathBuf> {
        let path = dir.join(self.file_name(secs));
        if path.is_file() {
            return Ok(path);
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
        crate::edith::write_atomic(&path, &self.wav(secs))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generator_survives_its_own_spelling() {
        for g in [
            Gen::new(Kind::Solid { rgb: 0x123456 }, 1920, 1080),
            Gen::new(Kind::Gradient { a: 0xff0000, b: 0x0000ff, angle: 45 }, 640, 360),
            Gen::new(Kind::Countdown { from: 10, milli_fps: 29_970 }, 1280, 720),
            Gen::new(Kind::Bars, 320, 240),
            Gen::new(Kind::Adjust, 1920, 1080),
            Gen::new(Kind::Shape(Shape { form: Form::Rectangle, fill: Some(0x112233), stroke: Some(0xffffff), width: 8, radius: 40 }), 1920, 1080),
            Gen::new(Kind::Shape(Shape { form: Form::Ellipse, fill: None, stroke: Some(0xff0000), width: 3, radius: 0 }), 640, 360),
            Gen::new(Kind::Shape(Shape::of(Form::Line)), 640, 360),
            Gen::new(Kind::Shape(Shape::of(Form::Arrow)), 640, 360),
        ] {
            assert_eq!(Gen::parse(&g.path()), Some(g), "{:?}", g.path());
        }
        for bad in ["/edith-gen/solid-12345-1920x1080.gen", "/edith-gen/solid-123456-5x5.gen", "/tmp/solid-123456-64x64.gen", "/edith-gen/x.gen",
            "/edith-gen/rectangle-zzzzzz-none-0-0-64x64.gen", "/edith-gen/rectangle-ffffff-none-401-0-64x64.gen", "/edith-gen/blob-ffffff-none-1-0-64x64.gen", "/edith-gen/line-none-ffffff-1-0-5x5.gen"] {
            assert_eq!(Gen::parse(Path::new(bad)), None, "{bad}");
        }
    }

    #[test]
    fn a_solid_is_one_colour_and_a_gradient_runs_between_its_two() {
        let solid = Gen::new(Kind::Solid { rgb: 0x336699 }, 32, 32).rgb(0);
        assert!(solid.chunks_exact(3).all(|p| p == [0x33, 0x66, 0x99]));
        let g = Gen::new(Kind::Gradient { a: 0xff0000, b: 0x0000ff, angle: 0 }, 100, 20).rgb(0);
        let at = |x: usize| &g[(10 * 100 + x) * 3..][..3];
        assert!(at(0)[0] > 250 && at(0)[2] < 5, "red at the left: {:?}", at(0));
        assert!(at(99)[2] > 250 && at(99)[0] < 5, "blue at the right: {:?}", at(99));
        assert!((i32::from(at(50)[0]) - 128).abs() < 8, "half way in the middle: {:?}", at(50));
        let down = Gen::new(Kind::Gradient { a: 0x000000, b: 0xffffff, angle: 90 }, 20, 100).rgb(0);
        assert!(down[..3].iter().all(|v| *v < 6) && down[(99 * 20) * 3] > 250, "top to bottom");
    }

    #[test]
    fn bars_are_seven_columns_and_a_countdown_counts_down() {
        let b = Gen::new(Kind::Bars, 70, 8).rgb(0);
        assert_eq!(&b[..3], [191, 191, 191]);
        assert_eq!(&b[(69) * 3..][..3], [0, 0, 191]);
        let c = |f| Gen::new(Kind::Countdown { from: 3, milli_fps: 10_000 }, 160, 90).rgb(f);
        // Each second differs (the numeral and the sweep), and a frame repeats exactly.
        assert_ne!(c(0), c(10));
        assert_ne!(c(10), c(20));
        assert_eq!(c(10), c(10));
        assert_ne!(c(0), c(5), "the sweep moves inside a second");
    }

    #[test]
    fn shapes_draw_on_a_transparent_canvas() {
        let shape = |form, fill, stroke, width, radius| Gen::new(Kind::Shape(Shape { form, fill, stroke, width, radius }), 100, 60);
        let at = |a: &[u8], x: usize, y: usize| a[y * 100 + x];
        // A filled rectangle covers all of its canvas; with rounded corners the corner is clear.
        let sq = shape(Form::Rectangle, Some(0xff0000), None, 0, 0);
        assert!(sq.has_alpha() && sq.alpha().unwrap().iter().all(|a| *a == 255));
        assert!(sq.rgb(0).chunks_exact(3).all(|p| p == [255, 0, 0]));
        let round = shape(Form::Rectangle, Some(0xff0000), None, 0, 20).alpha().unwrap();
        assert_eq!(at(&round, 0, 0), 0, "the rounded corner is clear");
        assert_eq!(at(&round, 50, 30), 255);
        // A stroke runs along the inside of the edge in its own colour; the inside keeps the fill.
        let ring = shape(Form::Rectangle, Some(0x0000ff), Some(0x00ff00), 6, 0);
        let rgb = ring.rgb(0);
        let c = |x: usize, y: usize| &rgb[(y * 100 + x) * 3..][..3];
        assert_eq!(c(50, 2), [0, 255, 0]);
        assert_eq!(c(50, 30), [0, 0, 255]);
        // An ellipse leaves the corners clear and the centre painted; a stroke-only one is hollow.
        let e = shape(Form::Ellipse, Some(0xffffff), None, 0, 0).alpha().unwrap();
        assert_eq!((at(&e, 0, 0), at(&e, 50, 30)), (0, 255));
        let hollow = shape(Form::Ellipse, None, Some(0xffffff), 4, 0).alpha().unwrap();
        assert_eq!((at(&hollow, 50, 30), at(&hollow, 50, 1)), (0, 255));
        // A line is a stripe through the middle; an arrow adds a head wider than the stripe.
        let line = shape(Form::Line, None, Some(0xffffff), 6, 0).alpha().unwrap();
        assert_eq!((at(&line, 50, 30), at(&line, 50, 10), at(&line, 50, 50)), (255, 0, 0));
        let arrow = shape(Form::Arrow, None, Some(0xffffff), 4, 0).alpha().unwrap();
        assert!(at(&arrow, 85, 24) > 200, "the head is wide where the shaft is not");
        assert_eq!(at(&arrow, 30, 24), 0);
        // Every other generator is opaque.
        assert!(Gen::new(Kind::Bars, 32, 32).alpha().is_none());
    }

    #[test]
    fn clear_pixels_carry_the_paint_colour_so_scaled_edges_have_no_fringe() {
        let g = Gen::new(Kind::Shape(Shape { form: Form::Ellipse, fill: Some(0x336699), stroke: None, width: 0, radius: 0 }), 64, 64);
        let (rgb, a) = (g.rgb(0), g.alpha().unwrap());
        assert_eq!(a[0], 0);
        assert_eq!(&rgb[..3], [0x33, 0x66, 0x99]);
    }
}

#[cfg(test)]
mod tone_tests {
    use super::*;

    fn samples(wav: &[u8]) -> Vec<f64> {
        wav[44..].chunks_exact(2).map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])) / 32767.0).collect()
    }

    /// One test for the tone: a 1 kHz sine at -20 dBFS RMS by default, settings clamped, and a
    /// file written once and reused.
    #[test]
    fn the_tone_is_a_sine_at_its_level_and_its_file_is_reused() {
        let wav = Tone::DEFAULT.wav(2);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), TONE_RATE);
        let s = samples(&wav);
        assert_eq!(s.len(), 2 * TONE_RATE as usize);
        let db = 20.0 * ((s.iter().map(|v| v * v).sum::<f64>() / s.len() as f64).sqrt()).log10();
        assert!((db + 20.0).abs() < 0.05, "rms {db} dBFS");
        // Zero crossings: 1 kHz is 2000 a second.
        let crossings = s.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert!((crossings as i64 - 4000).abs() <= 2, "{crossings} crossings");

        assert_eq!(Tone::new(0, 0), Tone { hz: 20, dbfs: -3 });
        assert_eq!(Tone::new(99_999, -99), Tone { hz: 20_000, dbfs: -60 });
        assert_eq!(Tone::DEFAULT.file_name(10), "tone-1000hz--20dbfs-10s.wav");
        assert_eq!(tone_title(Path::new("/x/tone-1000hz--20dbfs-10s.wav")).as_deref(), Some("Tone 1 kHz \u{b7} \u{2212}20 dB"));

        let dir = crate::scratch::Scratch::dir("tone_file");
        let t = Tone::new(440, -18);
        let path = t.write(&dir, 1).expect("writes");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 44 + 2 * u64::from(TONE_RATE));
        assert_eq!(t.write(&dir, 1).expect("reuses"), path);
    }
}
