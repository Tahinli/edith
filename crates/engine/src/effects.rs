//! A clip's effects stack ([`crate::project::Clip::effects`]): an ordered list of
//! picture effects, drawn on the clip's picture as it lands on the frame, before
//! that picture is laid over the lanes beneath it.
//!
//! Two sorts of effect share the one list:
//!
//! * the *filters* -- blur, sharpen, vignette, glow and camera shake -- rewrite
//!   the clip's own samples, in list order;
//! * the *compositing* ones -- the colour key and the two masks -- each multiply
//!   into the clip's coverage (where the clip shows over what is beneath), and a
//!   blend effect picks how it is laid there ([`BlendMode`]). The last enabled
//!   blend in the list wins.
//!
//! Everything works on the I420 planes every path already holds, at 8 bits or at
//! 10 bits in `u16` ([`Sample`]), so playback and the two export depths draw the
//! same picture from the same numbers. Distances (a blur's radius, a shake's
//! reach) are pixels *of a 1080-line frame* and scale with the frame's height, so
//! a preview at a smaller size shows the effect at the size the export will.
//!
//! The parameters are static (one value for the whole clip), except the two masks'
//! position, size and soft edge, which take keys ([`MaskKey`]) at source frames and
//! are read at the frame shown ([`Effect::posed`]). A pixelate is a filter; a mask
//! placed right after it limits it to that mask's shape, and that mask then does
//! not cut the clip's coverage. An empty list, and a list whose every effect is
//! switched off, is no effect at all -- not a sample is touched and no path changes
//! ([`active`]).

use std::borrow::Cow;

use crate::colorspace::Matrix;
use crate::scale::{Sample, chroma_dims};
use crate::transform::{KeyEase, Keyed, sample};

/// The most effects one clip's stack may hold.
pub const MAX_EFFECTS: usize = 16;
/// The most parameters any one effect has.
pub const MAX_PARAMS: usize = 6;

/// Which effect an [`Effect`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Blur,
    Sharpen,
    Vignette,
    Glow,
    Shake,
    ChromaKey,
    Blend,
    RectMask,
    EllipseMask,
    Pixelate,
}

impl EffectKind {
    pub const ALL: [EffectKind; 10] = [
        EffectKind::Blur,
        EffectKind::Sharpen,
        EffectKind::Vignette,
        EffectKind::Glow,
        EffectKind::Shake,
        EffectKind::ChromaKey,
        EffectKind::Blend,
        EffectKind::RectMask,
        EffectKind::EllipseMask,
        EffectKind::Pixelate,
    ];

    /// The name the project file and the MCP ops spell it with.
    pub fn name(self) -> &'static str {
        match self {
            EffectKind::Blur => "blur",
            EffectKind::Sharpen => "sharpen",
            EffectKind::Vignette => "vignette",
            EffectKind::Glow => "glow",
            EffectKind::Shake => "shake",
            EffectKind::ChromaKey => "chroma_key",
            EffectKind::Blend => "blend",
            EffectKind::RectMask => "rect_mask",
            EffectKind::EllipseMask => "ellipse_mask",
            EffectKind::Pixelate => "pixelate",
        }
    }

    pub fn from_name(name: &str) -> Option<EffectKind> {
        EffectKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The plain words a person sees for it.
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Blur => "Blur",
            EffectKind::Sharpen => "Sharpen",
            EffectKind::Vignette => "Vignette",
            EffectKind::Glow => "Glow",
            EffectKind::Shake => "Camera shake",
            EffectKind::ChromaKey => "Green screen",
            EffectKind::Blend => "Blend with lanes below",
            EffectKind::RectMask => "Rectangle mask",
            EffectKind::EllipseMask => "Ellipse mask",
            EffectKind::Pixelate => "Pixelate",
        }
    }

    /// Its parameters, in the order [`Effect::values`] holds them.
    pub fn params(self) -> &'static [ParamSpec] {
        const fn p(name: &'static str, label: &'static str, min: f32, max: f32, default: f32, step: f32) -> ParamSpec {
            ParamSpec { name, label, min, max, default, step }
        }
        const BLUR: [ParamSpec; 1] = [p("radius", "Radius", 0.0, 100.0, 10.0, 0.5)];
        const SHARPEN: [ParamSpec; 1] = [p("amount", "Amount", 0.0, 3.0, 1.0, 0.05)];
        const VIGNETTE: [ParamSpec; 2] = [
            p("amount", "Amount", 0.0, 1.0, 0.5, 0.01),
            p("softness", "Softness", 0.0, 1.0, 0.5, 0.01),
        ];
        const GLOW: [ParamSpec; 2] = [
            p("amount", "Amount", 0.0, 2.0, 0.6, 0.01),
            p("threshold", "Brightness it starts at", 0.0, 1.0, 0.7, 0.01),
        ];
        const SHAKE: [ParamSpec; 3] = [
            p("amount", "Amount", 0.0, 100.0, 12.0, 0.5),
            p("speed", "Speed", 0.1, 20.0, 4.0, 0.1),
            p("seed", "Pattern", 0.0, 9999.0, 1.0, 1.0),
        ];
        const KEY: [ParamSpec; 6] = [
            p("red", "Key red", 0.0, 255.0, 0.0, 1.0),
            p("green", "Key green", 0.0, 255.0, 255.0, 1.0),
            p("blue", "Key blue", 0.0, 255.0, 0.0, 1.0),
            p("tolerance", "Tolerance", 0.0, 1.0, 0.3, 0.01),
            p("softness", "Edge softness", 0.0, 1.0, 0.1, 0.01),
            p("spill", "Remove colour spill", 0.0, 1.0, 0.5, 0.01),
        ];
        const BLEND: [ParamSpec; 1] = [p("mode", "Mode", 0.0, 6.0, 0.0, 1.0)];
        const PIXELATE: [ParamSpec; 1] = [p("size", "Block size", 2.0, 200.0, 16.0, 1.0)];
        const MASK: [ParamSpec; 6] = [
            p("x", "Centre across", 0.0, 1.0, 0.5, 0.01),
            p("y", "Centre down", 0.0, 1.0, 0.5, 0.01),
            p("width", "Width", 0.0, 1.0, 0.5, 0.01),
            p("height", "Height", 0.0, 1.0, 0.5, 0.01),
            p("feather", "Soft edge", 0.0, 0.5, 0.05, 0.005),
            p("invert", "Invert", 0.0, 1.0, 0.0, 1.0),
        ];
        match self {
            EffectKind::Blur => &BLUR,
            EffectKind::Sharpen => &SHARPEN,
            EffectKind::Vignette => &VIGNETTE,
            EffectKind::Glow => &GLOW,
            EffectKind::Shake => &SHAKE,
            EffectKind::ChromaKey => &KEY,
            EffectKind::Blend => &BLEND,
            EffectKind::Pixelate => &PIXELATE,
            EffectKind::RectMask | EffectKind::EllipseMask => &MASK,
        }
    }
}

/// One parameter of an effect: its name in the file and the MCP ops, its plain
/// label, its range and default, and the step a slider moves it by. A `step` of 1
/// over a 0..1 range is a switch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f32,
}

/// How a clip is laid over the lanes beneath it. Defined on RGB, as every
/// editor defines them, and worked per pixel through the output's matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Add,
    Darken,
    Lighten,
}

impl BlendMode {
    pub const ALL: [BlendMode; 7] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Add,
        BlendMode::Darken,
        BlendMode::Lighten,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Add => "add",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
        }
    }

    pub fn from_name(name: &str) -> Option<BlendMode> {
        BlendMode::ALL.into_iter().find(|m| m.name() == name)
    }

    /// The mode a blend effect's `mode` value names (its index in [`BlendMode::ALL`]).
    pub fn from_value(v: f32) -> BlendMode {
        BlendMode::ALL[(v.round().clamp(0.0, 6.0)) as usize]
    }

    fn mix(self, b: f32, t: f32) -> f32 {
        match self {
            BlendMode::Normal => t,
            BlendMode::Multiply => b * t,
            BlendMode::Screen => 1.0 - (1.0 - b) * (1.0 - t),
            BlendMode::Overlay => match b < 0.5 {
                true => 2.0 * b * t,
                false => 1.0 - 2.0 * (1.0 - b) * (1.0 - t),
            },
            BlendMode::Add => (b + t).min(1.0),
            BlendMode::Darken => b.min(t),
            BlendMode::Lighten => b.max(t),
        }
    }
}

/// One point of a mask's animation ([`Effect::keys`]): the shape's centre, size and
/// soft edge at source frame `at`, with the ease of the segment that leaves it. The
/// fields are the mask's [`EffectKind::params`] in order, so [`Effect::posed`] reads
/// them like the static values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskKey {
    /// Source frame of the key.
    pub at: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub feather: f32,
    /// The easing of the segment that leaves this key.
    pub ease: KeyEase,
}

impl Keyed for MaskKey {
    fn at(&self) -> u32 {
        self.at
    }
    fn ease(&self) -> KeyEase {
        self.ease
    }
    fn placed(self, at: u32, ease: KeyEase) -> Self {
        Self { at, ease, ..self }
    }
    fn mix(a: &Self, b: &Self, t: f32) -> Self {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Self {
            x: l(a.x, b.x),
            y: l(a.y, b.y),
            width: l(a.width, b.width),
            height: l(a.height, b.height),
            feather: l(a.feather, b.feather),
            ..*a
        }
    }
}

/// One effect of a stack: what it is, whether it is switched on, and its
/// parameters in [`EffectKind::params`] order (the unused tail is zero). A mask
/// may carry [`MaskKey`]s, which replace its position, size and soft edge.
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub kind: EffectKind,
    pub on: bool,
    pub values: [f32; MAX_PARAMS],
    /// The mask's animation, sorted by [`MaskKey::at`] with no two alike. Empty is
    /// the static values. Only a rect or ellipse mask has keys.
    pub keys: Vec<MaskKey>,
}

impl Effect {
    /// A switched-on effect of `kind` at its defaults.
    pub fn new(kind: EffectKind) -> Effect {
        let mut values = [0.0; MAX_PARAMS];
        for (v, spec) in values.iter_mut().zip(kind.params()) {
            *v = spec.default;
        }
        Effect { kind, on: true, values, keys: Vec::new() }
    }

    /// The effect as it draws at source frame `src`: a keyed mask with its
    /// position, size and soft edge interpolated from its keys; anything else as it
    /// is, borrowed.
    pub fn posed(&self, src: f64) -> Cow<'_, Effect> {
        match sample(&self.keys, src) {
            None => Cow::Borrowed(self),
            Some(k) => {
                let mut e = self.clone();
                e.values[..5].copy_from_slice(&[k.x, k.y, k.width, k.height, k.feather]);
                Cow::Owned(e)
            }
        }
    }

    /// The value of the parameter called `name`, if the effect has one.
    pub fn get(&self, name: &str) -> Option<f32> {
        let at = self.kind.params().iter().position(|p| p.name == name)?;
        Some(self.values[at])
    }

    /// Sets the parameter called `name`, clamped into its range. An error names
    /// the parameters the effect does have.
    pub fn set(&mut self, name: &str, value: f32) -> Result<(), String> {
        let specs = self.kind.params();
        let Some(at) = specs.iter().position(|p| p.name == name) else {
            let names: Vec<&str> = specs.iter().map(|p| p.name).collect();
            return Err(format!(
                "{} has no parameter {name:?}; it has {}",
                self.kind.name(),
                names.join(", ")
            ));
        };
        if !value.is_finite() {
            return Err(format!("{name} must be a finite number"));
        }
        self.values[at] = value.clamp(specs[at].min, specs[at].max);
        Ok(())
    }

    /// Every value clamped into its range and the unused tail zeroed: what a
    /// setter stores, so a stack read back from anywhere draws in range.
    pub fn clamped(mut self) -> Effect {
        let specs = self.kind.params();
        for (i, v) in self.values.iter_mut().enumerate() {
            *v = match specs.get(i) {
                Some(s) if v.is_finite() => v.clamp(s.min, s.max),
                Some(s) => s.default,
                None => 0.0,
            };
        }
        self
    }

    fn v(&self, i: usize) -> f32 {
        self.values[i]
    }
}

/// Whether the stack does anything: one effect at least is switched on. A clip
/// whose stack is not active is drawn exactly as a clip with none.
pub fn active(effects: &[Effect]) -> bool {
    effects.iter().any(|e| e.on)
}

/// Whether the stack changes from frame to frame on the same picture (a camera
/// shake): such a clip is drawn again every frame even when its source holds.
pub fn animated(effects: &[Effect]) -> bool {
    effects.iter().any(|e| e.on && (e.kind == EffectKind::Shake || !e.keys.is_empty()))
}

/// Reusable sample buffers for [`apply`], kept by the caller across frames so a
/// frame allocates nothing per pixel and, after the first, nothing at all for the
/// filters.
#[derive(Default)]
pub struct Scratch<T> {
    a: Vec<T>,
    b: Vec<T>,
}

/// What a stack asks of the compositor, once its filters are drawn: the clip's
/// coverage from its keys and masks (`None` where it has neither, which is full
/// coverage), and the mode it is laid down with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Composite {
    pub alpha: Option<Vec<u8>>,
    pub mode: BlendMode,
}

impl Composite {
    /// Whether the clip is laid down any other way than an opaque normal one.
    pub fn composites(&self) -> bool {
        self.alpha.is_some() || self.mode != BlendMode::Normal
    }
}

/// Draws the stack on a `w` x `h` I420 picture in place, in list order, and
/// returns how the result is to be laid over what is beneath it. `secs` is the
/// timeline time of the frame (what a shake follows), `src` the source frame it
/// plays (what the mask keys follow) and `matrix` the one the samples are coded
/// against (what a key colour and a blend are converted through).
#[allow(clippy::too_many_arguments)]
pub fn apply<T: Sample>(
    effects: &[Effect],
    planes: (&mut [T], &mut [T], &mut [T]),
    w: usize,
    h: usize,
    secs: f64,
    src: f64,
    matrix: Matrix,
    scratch: &mut Scratch<T>,
) -> Composite {
    let (y, u, v) = planes;
    let mut out = Composite::default();
    let px = h as f32 / 1080.0;
    let (cw, ch) = chroma_dims(w, h);
    // A pixelate is limited by the first mask after it, which then only shapes it
    // and does not cut the clip: `limit[i]` is that mask's index for pixelate `i`,
    // `shaping[j]` marks the mask.
    let mut limit = vec![None; effects.len()];
    let mut shaping = vec![false; effects.len()];
    let mut pending = None;
    for (i, e) in effects.iter().enumerate().filter(|(_, e)| e.on) {
        match e.kind {
            EffectKind::Pixelate => pending = Some(i),
            EffectKind::RectMask | EffectKind::EllipseMask => {
                if let Some(p) = pending.take() {
                    limit[p] = Some(i);
                    shaping[i] = true;
                }
            }
            _ => {}
        }
    }
    for (i, e) in effects.iter().enumerate().filter(|(_, e)| e.on) {
        match e.kind {
            EffectKind::Pixelate => {
                let block = (e.v(0) * px).round().max(1.0) as usize;
                let cover = limit[i].map(|j| {
                    let mut m = vec![255u8; w * h];
                    mask(&effects[j].posed(src), w, h, &mut m);
                    m
                });
                pixelate(y, w, h, block, 1, w, cover.as_deref());
                pixelate(u, cw, ch, (block / 2).max(1), 2, w, cover.as_deref());
                pixelate(v, cw, ch, (block / 2).max(1), 2, w, cover.as_deref());
            }
            EffectKind::Blur => {
                let r = e.v(0) * px;
                gauss(y, w, h, r, &mut scratch.a);
                gauss(u, cw, ch, r / 2.0, &mut scratch.a);
                gauss(v, cw, ch, r / 2.0, &mut scratch.a);
            }
            EffectKind::Sharpen => sharpen(y, w, h, e.v(0), (2.0 * px).max(1.0), scratch),
            EffectKind::Vignette => vignette(y, u, v, w, h, e.v(0), e.v(1)),
            EffectKind::Glow => glow(y, w, h, e.v(0), e.v(1), 20.0 * px, scratch),
            EffectKind::Shake => {
                let (dx, dy) = shake_offset(e.v(0) * px, e.v(1), e.v(2) as u32, secs);
                shift(y, w, h, dx, dy, &mut scratch.a);
                shift(u, cw, ch, dx / 2, dy / 2, &mut scratch.a);
                shift(v, cw, ch, dx / 2, dy / 2, &mut scratch.a);
            }
            EffectKind::ChromaKey => {
                let alpha = out.alpha.get_or_insert_with(|| vec![255; w * h]);
                chroma_key(e, u, v, w, h, matrix, alpha);
            }
            EffectKind::Blend => out.mode = BlendMode::from_value(e.v(0)),
            EffectKind::RectMask | EffectKind::EllipseMask => {
                if !shaping[i] {
                    let alpha = out.alpha.get_or_insert_with(|| vec![255; w * h]);
                    mask(&e.posed(src), w, h, alpha);
                }
            }
        }
    }
    out
}

/// The 8-bit-scale value of a sample, and back: the 10-bit codes are four times
/// the 8-bit ones, so the per-pixel arithmetic is written once on the 8-bit scale.
fn depth<T: Sample>() -> f32 {
    (T::MAX + 1) as f32 / 256.0
}

/// The integer Gaussian kernel for a blur of `radius` pixels: taps `-r..=r`, the
/// radius three standard deviations, weights summing to exactly `1 << SHIFT`.
const SHIFT: u32 = 14;
fn kernel(radius: f32) -> Vec<u32> {
    let r = radius.ceil() as usize;
    let sigma = (radius / 3.0).max(0.3);
    let raw: Vec<f32> = (0..=2 * r)
        .map(|i| {
            let d = i as f32 - r as f32;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let total: f32 = raw.iter().sum();
    let mut k: Vec<u32> = raw
        .iter()
        .map(|w| (w / total * (1u32 << SHIFT) as f32).round() as u32)
        .collect();
    let sum: u32 = k.iter().sum();
    // The rounding's leftover goes to the centre tap, so a flat field stays flat.
    k[r] = (k[r] + (1 << SHIFT)).saturating_sub(sum);
    k
}

/// Runs `f` over the rows of `out` (each `w` long), split across the machine's
/// cores. `f` is handed the row's index and a per-thread accumulator row.
fn par_rows<T: Send>(out: &mut [T], w: usize, f: impl Fn(usize, &mut [T], &mut Vec<u32>) + Sync) {
    let h = out.len() / w.max(1);
    if h == 0 {
        return;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = cores.min(h.div_ceil(32)).max(1);
    let per = h.div_ceil(threads);
    if threads == 1 {
        let mut acc = Vec::new();
        for (row, line) in out.chunks_mut(w).enumerate() {
            f(row, line, &mut acc);
        }
        return;
    }
    let f = &f;
    std::thread::scope(|s| {
        for (chunk, lines) in out.chunks_mut(per * w).enumerate() {
            s.spawn(move || {
                let mut acc = Vec::new();
                for (i, line) in lines.chunks_mut(w).enumerate() {
                    f(chunk * per + i, line, &mut acc);
                }
            });
        }
    });
}

/// A separable Gaussian blur of one plane in place: a horizontal pass into
/// `tmp`, a vertical one back. Edges repeat their last sample.
fn gauss<T: Sample>(plane: &mut [T], w: usize, h: usize, radius: f32, tmp: &mut Vec<T>) {
    if radius < 0.5 || w == 0 || h == 0 {
        return;
    }
    let k = kernel(radius);
    let r = k.len() / 2;
    let half = 1u32 << (SHIFT - 1);
    tmp.clear();
    tmp.resize(w * h, T::default());
    {
        let src = &*plane;
        let k = &k;
        par_rows(tmp, w, |row, line, _| {
            let s = &src[row * w..row * w + w];
            for (x, out) in line.iter_mut().enumerate() {
                let mut acc = 0u32;
                if x >= r && x + r < w {
                    for (j, &kw) in k.iter().enumerate() {
                        acc += kw * s[x - r + j].code();
                    }
                } else {
                    for (j, &kw) in k.iter().enumerate() {
                        acc += kw * s[(x + j).saturating_sub(r).min(w - 1)].code();
                    }
                }
                *out = T::from_code((acc + half) >> SHIFT);
            }
        });
    }
    let src = &*tmp;
    let k = &k;
    par_rows(plane, w, |row, line, acc| {
        acc.clear();
        acc.resize(w, 0);
        for (j, &kw) in k.iter().enumerate() {
            let sy = (row + j).saturating_sub(r).min(h - 1);
            for (a, s) in acc.iter_mut().zip(&src[sy * w..sy * w + w]) {
                *a += kw * s.code();
            }
        }
        for (out, a) in line.iter_mut().zip(acc.iter()) {
            *out = T::from_code((a + half) >> SHIFT);
        }
    });
}

/// An unsharp mask on luma: each sample pushed away from its blurred
/// neighbourhood by `amount`.
fn sharpen<T: Sample>(y: &mut [T], w: usize, h: usize, amount: f32, radius: f32, s: &mut Scratch<T>) {
    if amount <= 0.0 {
        return;
    }
    s.b.clear();
    s.b.extend_from_slice(y);
    gauss(&mut s.b, w, h, radius, &mut s.a);
    for (p, &b) in y.iter_mut().zip(&s.b) {
        let (o, b) = (p.code() as f32, b.code() as f32);
        *p = T::from_f32(o + amount * (o - b));
    }
}

/// Darkens toward the corners: 1 at the centre, `1 - amount` at a corner, the
/// fall-off starting further in the softer it is. Chroma fades toward neutral
/// with its luma, so a dark corner is black rather than a dark tint.
fn vignette<T: Sample>(y: &mut [T], u: &mut [T], v: &mut [T], w: usize, h: usize, amount: f32, softness: f32) {
    if amount <= 0.0 {
        return;
    }
    let lo = (1.0 - softness.clamp(0.0, 1.0)) * 0.85;
    let factor = |fx: f32, fy: f32| {
        let nx = fx / w as f32 * 2.0 - 1.0;
        let ny = fy / h as f32 * 2.0 - 1.0;
        let d = ((nx * nx + ny * ny) * 0.5).sqrt();
        1.0 - amount * smoothstep(lo, 1.0, d)
    };
    let black = T::BLACK.code() as f32;
    let neutral = T::NEUTRAL.code() as f32;
    for row in 0..h {
        for col in 0..w {
            let f = factor(col as f32 + 0.5, row as f32 + 0.5);
            let p = &mut y[row * w + col];
            *p = T::from_f32(black + (p.code() as f32 - black) * f);
        }
    }
    let (cw, ch) = chroma_dims(w, h);
    for row in 0..ch {
        for col in 0..cw {
            let f = factor(col as f32 * 2.0 + 1.0, row as f32 * 2.0 + 1.0);
            let i = row * cw + col;
            u[i] = T::from_f32(neutral + (u[i].code() as f32 - neutral) * f);
            v[i] = T::from_f32(neutral + (v[i].code() as f32 - neutral) * f);
        }
    }
}

/// A bloom on luma: what is brighter than `threshold` (0 black, 1 white) is
/// blurred wide and added back at `amount`.
fn glow<T: Sample>(y: &mut [T], w: usize, h: usize, amount: f32, threshold: f32, radius: f32, s: &mut Scratch<T>) {
    if amount <= 0.0 {
        return;
    }
    let d = depth::<T>();
    let thr = ((16.0 + threshold.clamp(0.0, 1.0) * 219.0) * d) as u32;
    s.b.clear();
    s.b.extend(y.iter().map(|p| T::from_code(p.code().saturating_sub(thr))));
    gauss(&mut s.b, w, h, radius, &mut s.a);
    for (p, &b) in y.iter_mut().zip(&s.b) {
        *p = T::from_f32(p.code() as f32 + amount * b.code() as f32);
    }
}

/// The camera shake's offset at `secs`, in even whole pixels (so the chroma
/// planes move exactly half as far): smooth value noise, two octaves, one
/// independent wobble per axis, the same for the same seed every time.
pub fn shake_offset(reach: f32, speed: f32, seed: u32, secs: f64) -> (i32, i32) {
    let t = secs * f64::from(speed.max(0.0));
    let n = |salt: u32| {
        let a = noise(seed.wrapping_mul(2).wrapping_add(salt), t);
        let b = noise(seed.wrapping_mul(2).wrapping_add(salt) ^ 0x5bd1_e995, t * 2.0 + 17.0);
        (a + 0.5 * b) / 1.5
    };
    let even = |v: f32| (v * reach / 2.0).round() as i32 * 2;
    (even(n(0)), even(n(1)))
}

fn noise(seed: u32, t: f64) -> f32 {
    let hash = |i: i64| {
        let mut x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(seed).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 31;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 29;
        (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    };
    let i = t.floor();
    let f = (t - i) as f32;
    let s = f * f * (3.0 - 2.0 * f);
    let (a, b) = (hash(i as i64), hash(i as i64 + 1));
    a + (b - a) * s
}

/// The plane moved by `(dx, dy)` pixels, its edges repeated into what is
/// uncovered.
fn shift<T: Sample>(plane: &mut [T], w: usize, h: usize, dx: i32, dy: i32, tmp: &mut Vec<T>) {
    if (dx == 0 && dy == 0) || w == 0 || h == 0 {
        return;
    }
    tmp.clear();
    tmp.extend_from_slice(plane);
    let clamp = |v: i64, n: usize| v.clamp(0, n as i64 - 1) as usize;
    for row in 0..h {
        let sy = clamp(row as i64 - i64::from(dy), h);
        let src = &tmp[sy * w..sy * w + w];
        for (col, p) in plane[row * w..row * w + w].iter_mut().enumerate() {
            *p = src[clamp(col as i64 - i64::from(dx), w)];
        }
    }
}

/// Luma and chroma weights of a matrix: `(kr, kb)`.
fn weights(matrix: Matrix) -> (f32, f32) {
    match matrix {
        Matrix::Bt601 => (0.299, 0.114),
        Matrix::Bt709 => (0.2126, 0.0722),
        Matrix::Bt2020Ncl => (0.2627, 0.0593),
    }
}

/// Limited-range 8-bit-scale YUV to RGB in 0..=1.
fn to_rgb(y: f32, u: f32, v: f32, (kr, kb): (f32, f32)) -> [f32; 3] {
    let yn = (y - 16.0) / 219.0;
    let (cb, cr) = ((u - 128.0) / 224.0, (v - 128.0) / 224.0);
    let r = yn + 2.0 * (1.0 - kr) * cr;
    let b = yn + 2.0 * (1.0 - kb) * cb;
    let g = (yn - kr * r - kb * b) / (1.0 - kr - kb);
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

/// RGB in 0..=1 to limited-range 8-bit-scale YUV.
fn to_yuv([r, g, b]: [f32; 3], (kr, kb): (f32, f32)) -> (f32, f32, f32) {
    let yn = kr * r + (1.0 - kr - kb) * g + kb * b;
    let cb = (b - yn) / (2.0 * (1.0 - kb));
    let cr = (r - yn) / (2.0 * (1.0 - kr));
    (16.0 + 219.0 * yn, 128.0 + 224.0 * cb, 128.0 + 224.0 * cr)
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    if hi <= lo {
        return if x < lo { 0.0 } else { 1.0 };
    }
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A colour key: coverage falls to 0 where a pixel's colour is within
/// `tolerance` of the key's (measured on chroma, so a shadow on the screen keys
/// as the lit part does), softening over `softness`; `spill` takes the key's own
/// hue out of what is left, so a green fringe goes grey.
fn chroma_key<T: Sample>(e: &Effect, u: &mut [T], v: &mut [T], w: usize, h: usize, matrix: Matrix, alpha: &mut [u8]) {
    let rgb = [e.v(0) / 255.0, e.v(1) / 255.0, e.v(2) / 255.0];
    let (_, ku, kv) = to_yuv(rgb, weights(matrix));
    let (dirx, diry) = (ku - 128.0, kv - 128.0);
    let len = (dirx * dirx + diry * diry).sqrt();
    let (tol, soft, spill) = (e.v(3), e.v(4), e.v(5));
    let d = depth::<T>();
    let (cw, ch) = chroma_dims(w, h);
    let mut block = vec![0u8; cw];
    for row in 0..ch {
        for (col, a) in block.iter_mut().enumerate() {
            let i = row * cw + col;
            let (pu, pv) = (u[i].code() as f32 / d, v[i].code() as f32 / d);
            let dist = ((pu - ku).powi(2) + (pv - kv).powi(2)).sqrt() / 128.0;
            *a = (smoothstep(tol, tol + soft, dist) * 255.0).round() as u8;
            if spill > 0.0 && len > 1.0 {
                let p = ((pu - 128.0) * dirx + (pv - 128.0) * diry) / len;
                if p > 0.0 {
                    let k = spill * p / len;
                    u[i] = T::from_f32((pu - k * dirx) * d);
                    v[i] = T::from_f32((pv - k * diry) * d);
                }
            }
        }
        for ly in [2 * row, 2 * row + 1].into_iter().filter(|&ly| ly < h) {
            for (x, a) in alpha[ly * w..ly * w + w].iter_mut().enumerate() {
                *a = mul255(*a, block[x / 2]);
            }
        }
    }
}

fn mul255(a: u8, b: u8) -> u8 {
    ((u32::from(a) * u32::from(b) + 127) / 255) as u8
}

/// Mosaic on one plane of `w` x `h` samples in place: each `block` x `block` cell
/// takes its average. With `cover` (luma-sized, `lw` wide, read at `sub` times the
/// plane's position) a cell is mixed in only where the coverage says, so the mosaic
/// lands inside a mask and the rest of the picture is left as it was.
fn pixelate<T: Sample>(plane: &mut [T], w: usize, h: usize, block: usize, sub: usize, lw: usize, cover: Option<&[u8]>) {
    for by in (0..h).step_by(block) {
        for bx in (0..w).step_by(block) {
            let (ye, xe) = ((by + block).min(h), (bx + block).min(w));
            let mut sum = 0u32;
            for row in by..ye {
                sum += plane[row * w + bx..row * w + xe].iter().map(|s| s.code()).sum::<u32>();
            }
            let n = ((ye - by) * (xe - bx)) as u32;
            let avg = T::from_code((sum + n / 2) / n);
            for row in by..ye {
                for col in bx..xe {
                    let a = cover.map_or(255, |c| u32::from(c[row * sub * lw + col * sub]));
                    let s = &mut plane[row * w + col];
                    *s = T::from_code((s.code() * (255 - a) + avg.code() * a + 127) / 255);
                }
            }
        }
    }
}

/// A rectangle or ellipse mask: coverage 1 inside the shape and 0 outside, the
/// edge softened over `feather` of the frame's height (centred on the edge), and
/// the whole turned inside out by `invert`. The shape's centre and size are
/// fractions of the frame.
fn mask(e: &Effect, w: usize, h: usize, alpha: &mut [u8]) {
    let (cx, cy) = (e.v(0) * w as f32, e.v(1) * h as f32);
    let (hw, hh) = ((e.v(2) * w as f32 / 2.0).max(0.0), (e.v(3) * h as f32 / 2.0).max(0.0));
    let feather = e.v(4) * h as f32;
    let invert = e.v(5) >= 0.5;
    let ellipse = e.kind == EffectKind::EllipseMask;
    for row in 0..h {
        let py = row as f32 + 0.5 - cy;
        for (col, a) in alpha[row * w..row * w + w].iter_mut().enumerate() {
            let px = col as f32 + 0.5 - cx;
            // Signed distance to the edge in pixels, negative inside.
            let sd = if ellipse {
                if hw <= 0.0 || hh <= 0.0 {
                    f32::INFINITY
                } else {
                    let n = ((px / hw).powi(2) + (py / hh).powi(2)).sqrt();
                    (n - 1.0) * hw.min(hh)
                }
            } else {
                let (qx, qy) = (px.abs() - hw, py.abs() - hh);
                (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0)
            };
            let mut m = match feather > 0.0 {
                true => (0.5 - sd / feather).clamp(0.0, 1.0),
                false => f32::from(u8::from(sd <= 0.0)),
            };
            if invert {
                m = 1.0 - m;
            }
            *a = mul255(*a, (m * 255.0).round() as u8);
        }
    }
}

/// `top` laid over `base` in place, where `cover` (luma-sized, 0..=255) says the
/// clip shows, at strength `k`, in `mode`. Normal mode is a straight mix of the
/// samples; the others are worked on RGB through `matrix`. Chroma takes the
/// coverage and the blend of its block's top-left luma sample.
#[expect(clippy::too_many_arguments, reason = "two pictures, their cover and how they meet")]
pub fn blend_over<T: Sample>(
    base: (&mut [T], &mut [T], &mut [T]),
    top: (&[T], &[T], &[T]),
    cover: &[u8],
    k: f32,
    mode: BlendMode,
    matrix: Matrix,
    w: usize,
    h: usize,
) {
    let (by, bu, bv) = base;
    let (ty, tu, tv) = top;
    let (cw, ch) = chroma_dims(w, h);
    let mix = |b: T, t: f32, a: f32| T::from_f32(b.code() as f32 + (t - b.code() as f32) * a);
    if mode == BlendMode::Normal {
        for (i, (b, t)) in by.iter_mut().zip(ty).enumerate() {
            let a = f32::from(cover[i]) / 255.0 * k;
            if a > 0.0 {
                *b = mix(*b, t.code() as f32, a);
            }
        }
        for i in 0..cw * ch {
            let (cx, cy) = (i % cw, i / cw);
            let a = f32::from(cover[2 * cy * w + 2 * cx]) / 255.0 * k;
            if a > 0.0 {
                bu[i] = mix(bu[i], tu[i].code() as f32, a);
                bv[i] = mix(bv[i], tv[i].code() as f32, a);
            }
        }
        return;
    }
    let wts = weights(matrix);
    let d = depth::<T>();
    for cy in 0..ch {
        for cx in 0..cw {
            let ci = cy * cw + cx;
            let (bu8, bv8) = (bu[ci].code() as f32 / d, bv[ci].code() as f32 / d);
            let (tu8, tv8) = (tu[ci].code() as f32 / d, tv[ci].code() as f32 / d);
            let mut chroma = None;
            for (oy, ox) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (ly, lx) = (2 * cy + oy, 2 * cx + ox);
                if ly >= h || lx >= w {
                    continue;
                }
                let i = ly * w + lx;
                let a = f32::from(cover[i]) / 255.0 * k;
                if a <= 0.0 {
                    continue;
                }
                let b = to_rgb(by[i].code() as f32 / d, bu8, bv8, wts);
                let t = to_rgb(ty[i].code() as f32 / d, tu8, tv8, wts);
                let out = [0, 1, 2].map(|c| b[c] + (mode.mix(b[c], t[c]) - b[c]) * a);
                let (yy, uu, vv) = to_yuv(out, wts);
                by[i] = T::from_f32(yy * d);
                if (oy, ox) == (0, 0) {
                    chroma = Some((uu, vv));
                }
            }
            if let Some((uu, vv)) = chroma {
                bu[ci] = T::from_f32(uu * d);
                bv[ci] = T::from_f32(vv * d);
            }
        }
    }
}

/// Lays a picture whose stack composites ([`Composite::composites`]) over black,
/// in place: what a clip with nothing beneath it shows -- a keyed or masked
/// clip's cut-outs go black, and a blend meets the black a gap is.
pub fn over_black<T: Sample>(planes: (&mut [T], &mut [T], &mut [T]), out: &Composite, matrix: Matrix, w: usize, h: usize) {
    if !out.composites() {
        return;
    }
    let (y, u, v) = planes;
    let top = (y.to_vec(), u.to_vec(), v.to_vec());
    y.fill(T::BLACK);
    u.fill(T::NEUTRAL);
    v.fill(T::NEUTRAL);
    let full;
    let cover = match out.alpha.as_deref() {
        Some(a) => a,
        None => {
            full = vec![255u8; w * h];
            &full
        }
    };
    blend_over((y, u, v), (&top.0, &top.1, &top.2), cover, 1.0, out.mode, matrix, w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat<T: Sample>(w: usize, h: usize, y: u32, u: u32, v: u32) -> (Vec<T>, Vec<T>, Vec<T>) {
        let (cw, ch) = chroma_dims(w, h);
        (
            vec![T::from_code(y); w * h],
            vec![T::from_code(u); cw * ch],
            vec![T::from_code(v); cw * ch],
        )
    }

    fn run<T: Sample>(fx: &[Effect], p: &mut (Vec<T>, Vec<T>, Vec<T>), w: usize, h: usize, secs: f64) -> Composite {
        let mut s = Scratch::default();
        apply(fx, (&mut p.0, &mut p.1, &mut p.2), w, h, secs, secs, Matrix::Bt709, &mut s)
    }

    #[test]
    fn an_empty_or_switched_off_stack_touches_nothing() {
        let (w, h) = (64, 36);
        let mut p = flat::<u8>(w, h, 0, 0, 0);
        for (i, s) in p.0.iter_mut().enumerate() {
            *s = (i * 7 % 251) as u8;
        }
        let before = p.clone();
        assert_eq!(run(&[], &mut p, w, h, 0.0), Composite::default());
        let off: Vec<Effect> = EffectKind::ALL
            .into_iter()
            .map(|k| Effect { on: false, ..Effect::new(k) })
            .collect();
        assert!(!active(&off));
        assert_eq!(run(&off, &mut p, w, h, 0.0), Composite::default());
        assert_eq!(p, before, "not a sample moved");
    }

    #[test]
    fn the_kernel_sums_to_one_and_a_flat_field_stays_flat() {
        for r in [0.6, 1.0, 2.5, 10.0, 47.0] {
            assert_eq!(kernel(r).iter().sum::<u32>(), 1 << SHIFT, "r={r}");
        }
        let (w, h) = (96, 54);
        let mut p = flat::<u16>(w, h, 700, 300, 800);
        run(&[Effect::new(EffectKind::Blur)], &mut p, w, h, 0.0);
        assert!(p.0.iter().all(|&s| s == 700) && p.1.iter().all(|&s| s == 300));
    }

    #[test]
    fn blur_spreads_an_edge_and_matches_at_both_depths() {
        let (w, h) = (200, 108);
        let mut a = flat::<u8>(w, h, 16, 128, 128);
        let mut b = flat::<u16>(w, h, 64, 512, 512);
        for row in 0..h {
            for col in w / 2..w {
                a.0[row * w + col] = 235;
                b.0[row * w + col] = 940;
            }
        }
        let mut fx = Effect::new(EffectKind::Blur);
        fx.set("radius", 50.0).unwrap();
        run(std::slice::from_ref(&fx), &mut a, w, h, 0.0);
        run(std::slice::from_ref(&fx), &mut b, w, h, 0.0);
        let mid = h / 2 * w;
        assert!(a.0[mid + w / 2 - 2] > 30 && a.0[mid + w / 2 + 1] < 220, "the edge is soft");
        assert_eq!(a.0[mid], 16, "far from the edge stays put");
        for col in 0..w {
            let (x, y) = (f32::from(a.0[mid + col]), f32::from(b.0[mid + col]) / 4.0);
            assert!((x - y).abs() <= 1.0, "col {col}: {x} vs {y}");
        }
    }

    #[test]
    fn vignette_darkens_the_corners_and_not_the_centre() {
        let (w, h) = (160, 90);
        let mut p = flat::<u8>(w, h, 200, 160, 100);
        run(&[Effect::new(EffectKind::Vignette)], &mut p, w, h, 0.0);
        assert_eq!(p.0[h / 2 * w + w / 2], 200);
        assert!(p.0[0] < 140, "corner {}", p.0[0]);
        assert!(p.1[0] < 160 && p.1[0] > 128, "the corner's tint fades toward grey");
    }

    #[test]
    fn sharpen_and_glow_lift_what_they_should() {
        let (w, h) = (64, 64);
        let mut p = flat::<u8>(w, h, 100, 128, 128);
        p.0[32 * w + 32] = 150;
        run(&[Effect::new(EffectKind::Sharpen)], &mut p, w, h, 0.0);
        assert!(p.0[32 * w + 32] > 150, "a peak is pushed further");
        let (w, h) = (64, 540);
        let mut g = flat::<u8>(w, h, 30, 128, 128);
        for row in 268..272 {
            for i in 30..34 {
                g.0[row * w + i] = 235;
            }
        }
        let mut fx = Effect::new(EffectKind::Glow);
        fx.set("amount", 2.0).unwrap();
        run(std::slice::from_ref(&fx), &mut g, w, h, 0.0);
        assert!(g.0[270 * w + 27] > 30, "light spills round a bright spot");
        assert_eq!(g.0[0], 30, "dark stays dark");
    }

    #[test]
    fn shake_is_seeded_smooth_and_deterministic() {
        let a: Vec<_> = (0..50).map(|f| shake_offset(20.0, 4.0, 7, f as f64 / 25.0)).collect();
        let b: Vec<_> = (0..50).map(|f| shake_offset(20.0, 4.0, 7, f as f64 / 25.0)).collect();
        let c: Vec<_> = (0..50).map(|f| shake_offset(20.0, 4.0, 8, f as f64 / 25.0)).collect();
        assert_eq!(a, b);
        assert_ne!(a, c, "another pattern moves otherwise");
        assert!(a.iter().any(|&(x, y)| x != 0 || y != 0));
        assert!(a.iter().all(|&(x, y)| x.abs() <= 20 && y.abs() <= 20 && x % 2 == 0 && y % 2 == 0));
        assert!(a.windows(2).all(|p| (p[0].0 - p[1].0).abs() <= 12), "smooth, not jumping");
        let (w, h) = (64, 36);
        let mut p = flat::<u8>(w, h, 50, 128, 128);
        for (i, s) in p.0.iter_mut().enumerate() {
            *s = (i % w) as u8;
        }
        let mut fx = Effect::new(EffectKind::Shake);
        fx.set("amount", 100.0).unwrap();
        let mut q = p.clone();
        run(std::slice::from_ref(&fx), &mut q, w, h, 0.3);
        let (dx, _) = shake_offset(100.0 * h as f32 / 1080.0, 4.0, 1, 0.3);
        assert_eq!(q.0[w + 10], (10 - dx).clamp(0, w as i32 - 1) as u8);
    }

    #[test]
    fn a_green_key_cuts_out_green_and_keeps_the_rest() {
        let (w, h) = (16, 8);
        let (_, gu, gv) = to_yuv([0.0, 1.0, 0.0], weights(Matrix::Bt709));
        let (_, ru, rv) = to_yuv([1.0, 0.2, 0.2], weights(Matrix::Bt709));
        let mut p = flat::<u8>(w, h, 150, gu.round() as u32, gv.round() as u32);
        let cw = w / 2;
        for row in 0..h / 2 {
            for col in cw / 2..cw {
                p.1[row * cw + col] = ru.round() as u8;
                p.2[row * cw + col] = rv.round() as u8;
            }
        }
        let out = run(&[Effect::new(EffectKind::ChromaKey)], &mut p, w, h, 0.0);
        let a = out.alpha.expect("a key makes a cover");
        assert_eq!(a[0], 0, "green is gone");
        assert_eq!(a[w - 1], 255, "red stays");
    }

    #[test]
    fn masks_cover_their_shape_feathered_and_inverted() {
        let (w, h) = (100, 100);
        let mut p = flat::<u8>(w, h, 100, 128, 128);
        let out = run(&[Effect::new(EffectKind::RectMask)], &mut p, w, h, 0.0);
        let a = out.alpha.unwrap();
        assert_eq!(a[50 * w + 50], 255);
        assert_eq!(a[5 * w + 5], 0);
        assert!(a[50 * w + 25] > 60 && a[50 * w + 25] < 200, "the edge is soft: {}", a[50 * w + 25]);
        let mut fx = Effect::new(EffectKind::EllipseMask);
        fx.set("invert", 1.0).unwrap();
        fx.set("feather", 0.0).unwrap();
        let a = run(std::slice::from_ref(&fx), &mut p, w, h, 0.0).alpha.unwrap();
        assert_eq!((a[50 * w + 50], a[0], a[26 * w + 26]), (0, 255, 255), "a corner of the box is outside the ellipse");
    }

    #[test]
    fn pixelate_makes_each_block_one_value_and_a_mask_limits_it() {
        let (w, h) = (1920, 1080);
        let mut p = flat::<u8>(w, h, 0, 128, 128);
        for (i, s) in p.0.iter_mut().enumerate() {
            *s = (i % 251) as u8;
        }
        let mut fx = Effect::new(EffectKind::Pixelate);
        fx.set("size", 16.0).unwrap();
        run(std::slice::from_ref(&fx), &mut p, w, h, 0.0);
        for (bx, by) in [(0, 0), (16, 16)] {
            let first = p.0[by * w + bx];
            for row in by..by + 16 {
                assert!(p.0[row * w + bx..row * w + bx + 16].iter().all(|&s| s == first), "block at {bx},{by}");
            }
        }
        // Limited to an ellipse that covers only the middle of the frame: the corner keeps its samples.
        let mut q = flat::<u8>(w, h, 0, 128, 128);
        for (i, s) in q.0.iter_mut().enumerate() {
            *s = (i % 251) as u8;
        }
        let orig = q.0.clone();
        let mut e = Effect::new(EffectKind::EllipseMask);
        e.set("width", 0.2).unwrap();
        e.set("height", 0.2).unwrap();
        run(&[fx, e], &mut q, w, h, 0.0);
        assert_eq!(q.0[0], orig[0], "outside the mask the picture is as it was");
        assert_ne!(q.0[540 * w + 960], orig[540 * w + 960], "inside it is mosaiced");
    }

    #[test]
    fn blend_modes_meet_grey_as_their_formulas_say() {
        let (w, h) = (4, 4);
        let grey = |y| flat::<u8>(w, h, y, 128, 128);
        let mid = 16 + 219 / 2;
        let cover = vec![255u8; w * h];
        let at = |mode, b: u32, t: u32| {
            let mut base = grey(b);
            let top = grey(t);
            blend_over((&mut base.0, &mut base.1, &mut base.2), (&top.0, &top.1, &top.2), &cover, 1.0, mode, Matrix::Bt709, w, h);
            base.0[0]
        };
        assert_eq!(at(BlendMode::Normal, 16, 200), 200);
        assert_eq!(at(BlendMode::Multiply, 235, mid), mid as u8);
        assert_eq!(at(BlendMode::Multiply, 16, 235), 16);
        assert_eq!(at(BlendMode::Screen, 16, mid), mid as u8);
        assert_eq!(at(BlendMode::Screen, 235, 16), 235);
        assert_eq!(at(BlendMode::Add, mid, mid), 234);
        assert_eq!(at(BlendMode::Darken, 100, 200), 100);
        assert_eq!(at(BlendMode::Lighten, 100, 200), 200);
        assert!(at(BlendMode::Overlay, 60, 200) < at(BlendMode::Screen, 60, 200));
    }

    #[test]
    fn over_black_shows_a_cut_out_as_black() {
        let (w, h) = (100, 100);
        let mut p = flat::<u16>(w, h, 800, 600, 400);
        let out = run(&[Effect::new(EffectKind::EllipseMask)], &mut p, w, h, 0.0);
        over_black((&mut p.0, &mut p.1, &mut p.2), &out, Matrix::Bt709, w, h);
        assert_eq!((p.0[0], p.1[0]), (64, 512));
        assert_eq!(p.0[50 * w + 50], 800);
    }

    #[test]
    fn a_mask_key_track_is_interpolated_at_the_frame_shown() {
        use crate::transform::KeyEase;
        let key = |at, x| MaskKey { at, x, y: 0.5, width: 0.2, height: 0.2, feather: 0.0, ease: KeyEase::Linear };
        let mut fx = Effect::new(EffectKind::RectMask);
        fx.keys = vec![key(0, 0.2), key(100, 0.8)];
        assert!((fx.posed(50.0).get("x").unwrap() - 0.5).abs() < 1e-6, "halfway between the keys");
        assert_eq!(fx.posed(-5.0).get("x"), Some(0.2), "held before the first key");
        assert_eq!(fx.posed(300.0).get("x"), Some(0.8), "held after the last key");
        assert!(animated(std::slice::from_ref(&fx)), "a keyed mask redraws as the frames change");
    }

    #[test]
    fn set_clamps_and_names_what_there_is() {
        let mut e = Effect::new(EffectKind::Vignette);
        e.set("amount", 9.0).unwrap();
        assert_eq!(e.get("amount"), Some(1.0));
        let err = e.set("radius", 1.0).unwrap_err();
        assert!(err.contains("amount, softness"), "{err}");
        assert!(e.set("amount", f32::NAN).is_err());
    }

    /// The perf note's number: one 1080p frame through a radius-10 blur.
    #[test]
    #[ignore = "a timing, run by hand: cargo test --release -p engine blur_1080p_timing -- --ignored --nocapture"]
    fn blur_1080p_timing() {
        let (w, h) = (1920, 1080);
        let mut p = flat::<u8>(w, h, 100, 128, 128);
        for (i, s) in p.0.iter_mut().enumerate() {
            *s = (i * 13 % 219 + 16) as u8;
        }
        let mut fx = Effect::new(EffectKind::Blur);
        fx.set("radius", 10.0).unwrap();
        let mut s = Scratch::default();
        let mut s16 = Scratch::default();
        let mut q = flat::<u16>(w, h, 400, 512, 512);
        for _ in 0..3 {
            apply(std::slice::from_ref(&fx), (&mut p.0, &mut p.1, &mut p.2), w, h, 0.0, 0.0, Matrix::Bt709, &mut s);
        }
        let n = 20;
        let t = std::time::Instant::now();
        for _ in 0..n {
            apply(std::slice::from_ref(&fx), (&mut p.0, &mut p.1, &mut p.2), w, h, 0.0, 0.0, Matrix::Bt709, &mut s);
        }
        let ms8 = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        let t = std::time::Instant::now();
        for _ in 0..n {
            apply(std::slice::from_ref(&fx), (&mut q.0, &mut q.1, &mut q.2), w, h, 0.0, 0.0, Matrix::Bt709, &mut s16);
        }
        let ms16 = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("blur r=10 1080p: {ms8:.2} ms/frame 8-bit, {ms16:.2} ms/frame 10-bit");
    }
}
