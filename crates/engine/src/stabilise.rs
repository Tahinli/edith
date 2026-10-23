//! Video stabilisation: the camera's shake measured from the picture, smoothed, and
//! taken back out of each frame before the clip's own placement is applied.
//!
//! The pipeline, per clip, over the source frames it plays:
//!
//! 1. **Motion.** Each decoded frame is reduced to luma at no more than
//!    [`ANALYSIS_W`] wide (a box average, so the reduction is also the noise
//!    filter), and a three-level pyramid is built. Up to 20x12 Shi-Tomasi corners
//!    (the strongest in each grid cell) are tracked from the previous frame by
//!    pyramidal Lucas-Kanade, and a similarity transform (shift, turn, scale) is
//!    fitted to the tracks by RANSAC and refitted by least squares on the inliers,
//!    so a walker crossing the shot does not move the estimate.
//! 2. **Path.** The frame-to-frame transforms are chained into the camera's path,
//!    and each of its four parameters (angle, log scale, x, y) is smoothed by a
//!    Gaussian whose width the strength sets. The ends are padded by point
//!    reflection, so a steady pan -- a straight line -- comes out of the smoother
//!    exactly as it went in and is kept, while the shake around it is removed.
//! 3. **Correction.** Each frame is moved from where the camera was to where the
//!    smoothed camera is. In [`Crop::Zoom`] the picture is enlarged about its
//!    centre by the one factor that keeps every corrected frame of the clip
//!    covering the whole picture (no black edge anywhere, capped at [`MAX_ZOOM`]);
//!    [`Crop::Full`] keeps the frame's size and lets the edges show black.
//!
//! The measured motion is cached per source and per analysed range of its frames
//! (see [`cache_path`]): the key is the source's path, length and modification
//! time, so a changed file is a new analysis and re-opening a project is not. Only
//! the raw frame-to-frame motion is stored; the strength and crop are applied on
//! load, so changing them never re-analyses. The correction is indexed by the
//! file's own frame numbers, so it is the same picture whatever speed, ramp,
//! direction or trim the clip plays that footage at.
//!
//! Analysis runs on a thread of its own ([`start`]), polled by [`status`] and
//! stopped by [`cancel`]; the renderer only ever reads a finished analysis
//! ([`correction`]) and plays the clip unstabilised until there is one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;


/// The widest picture the motion is measured on; wider sources are box-averaged
/// down by a whole factor first.
pub const ANALYSIS_W: u32 = 640;
/// The most [`Crop::Zoom`] enlarges a picture to hide the corrected edges.
pub const MAX_ZOOM: f64 = 1.5;
/// The strength a clip is stabilised at when nothing else is asked for.
pub const DEFAULT_STRENGTH: f32 = 0.5;
/// Bumped when the analysis changes what it stores, so an old cache is a miss.
const ALGO: u32 = 1;
const MAGIC: &[u8; 8] = b"EDSTAB1\n";

/// How the corrected frame meets the edges it moved away from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Crop {
    /// Enlarged just enough that no frame shows an edge.
    #[default]
    Zoom,
    /// Kept at its size; the edges show black where the frame moved off them.
    Full,
}

impl Crop {
    pub fn name(self) -> &'static str {
        match self {
            Crop::Zoom => "zoom",
            Crop::Full => "full",
        }
    }

    pub fn from_name(s: &str) -> Option<Crop> {
        match s {
            "zoom" => Some(Crop::Zoom),
            "full" => Some(Crop::Full),
            _ => None,
        }
    }
}

/// One clip's stabilisation settings ([`crate::project::Clip::stabilise`]): a strength
/// in whole percent and a [`Crop`], packed into one non-zero byte so `Option<Stabilise>`
/// costs the clip a single byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stabilise(std::num::NonZeroU8);

impl Default for Stabilise {
    fn default() -> Self {
        Self::new(DEFAULT_STRENGTH, Crop::Zoom)
    }
}

impl Stabilise {
    /// A setting at `strength` `0.0..=1.0` (clamped, to the nearest percent). How smooth
    /// the camera path is made: the Gaussian's width is `strength` x 2 seconds of frames
    /// (at least half a frame).
    pub fn new(strength: f32, crop: Crop) -> Self {
        let s = if strength.is_finite() { strength.clamp(0.0, 1.0) } else { DEFAULT_STRENGTH };
        Self::with_percent((s * 100.0).round() as u8, crop)
    }

    /// A setting at `percent` (clamped to 100).
    pub fn with_percent(percent: u8, crop: Crop) -> Self {
        let code = 1 + percent.min(100) + if crop == Crop::Full { 101 } else { 0 };
        Self(std::num::NonZeroU8::new(code).expect("at least 1"))
    }

    /// The strength in whole percent, `0..=100`.
    pub fn strength(self) -> u8 {
        (self.0.get() - 1) % 101
    }

    /// The strength as a fraction, `0.0..=1.0`.
    pub fn amount(self) -> f32 {
        f32::from(self.strength()) / 100.0
    }

    pub fn crop(self) -> Crop {
        if self.0.get() > 101 { Crop::Full } else { Crop::Zoom }
    }
}

/// A similarity transform on centred pixel coordinates: `q = [a -b; b a] p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sim {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Sim {
    pub const IDENTITY: Sim = Sim { a: 1.0, b: 0.0, tx: 0.0, ty: 0.0 };

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x - self.b * y + self.tx, self.b * x + self.a * y + self.ty)
    }

    /// `self` first, then `next`.
    pub fn then(self, next: Sim) -> Sim {
        let (tx, ty) = next.apply(self.tx, self.ty);
        Sim { a: next.a * self.a - next.b * self.b, b: next.a * self.b + next.b * self.a, tx, ty }
    }

    pub fn inverse(self) -> Sim {
        let d = self.a * self.a + self.b * self.b;
        let (a, b) = (self.a / d, -self.b / d);
        let r = Sim { a, b, tx: 0.0, ty: 0.0 };
        let (tx, ty) = r.apply(-self.tx, -self.ty);
        Sim { a, b, tx, ty }
    }

    /// Radians, positive turning +x toward +y (clockwise on a y-down picture).
    pub fn angle(&self) -> f64 {
        self.b.atan2(self.a)
    }

    pub fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }

    fn params(&self) -> [f64; 4] {
        [self.angle(), self.scale().ln(), self.tx, self.ty]
    }

    fn from_params(p: [f64; 4]) -> Sim {
        let s = p[1].exp();
        Sim { a: s * p[0].cos(), b: s * p[0].sin(), tx: p[2], ty: p[3] }
    }

    /// The same transform for a picture `k` times the size.
    fn scaled(self, k: f64) -> Sim {
        Sim { tx: self.tx * k, ty: self.ty * k, ..self }
    }
}

// ---- motion ---------------------------------------------------------------

struct Level {
    w: usize,
    h: usize,
    px: Vec<f32>,
    gx: Vec<f32>,
    gy: Vec<f32>,
}

impl Level {
    fn new(w: usize, h: usize, px: Vec<f32>) -> Level {
        let (mut gx, mut gy) = (vec![0.0; w * h], vec![0.0; w * h]);
        for y in 1..h.saturating_sub(1) {
            for x in 1..w.saturating_sub(1) {
                let i = y * w + x;
                gx[i] = 0.5 * (px[i + 1] - px[i - 1]);
                gy[i] = 0.5 * (px[i + w] - px[i - w]);
            }
        }
        Level { w, h, px, gx, gy }
    }

    fn half(&self) -> Level {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut px = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = 2 * y * self.w + 2 * x;
                px[y * w + x] = 0.25 * (self.px[i] + self.px[i + 1] + self.px[i + self.w] + self.px[i + self.w + 1]);
            }
        }
        Level::new(w, h, px)
    }

    /// Bilinear sample of `plane` at `(x, y)`, which must be inside `0..w-1`.
    fn at(&self, plane: &[f32], x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let i = y0 as usize * self.w + x0 as usize;
        let top = plane[i] + (plane[i + 1] - plane[i]) * fx;
        let bot = plane[i + self.w] + (plane[i + self.w + 1] - plane[i + self.w]) * fx;
        top + (bot - top) * fy
    }
}

const LEVELS: usize = 4;
/// How far, in top-of-pyramid pixels, [`coarse_shift`] looks.
const COARSE: usize = 6;
const HALF_WIN: i32 = 7;
const GRID: (usize, usize) = (20, 12);

/// One frame, reduced, ready to be tracked from and to.
struct Pyramid {
    levels: Vec<Level>,
    /// Source pixels per analysis pixel.
    factor: f64,
}

impl Pyramid {
    fn of(luma: &[u8], w: usize, h: usize) -> Option<Pyramid> {
        let f = (w as u32).div_ceil(ANALYSIS_W).max(1) as usize;
        let (aw, ah) = (w / f, h / f);
        if aw < 64 || ah < 36 {
            return None;
        }
        let mut px = vec![0.0f32; aw * ah];
        let norm = 1.0 / (f * f) as f32;
        for y in 0..ah {
            for x in 0..aw {
                let mut s = 0u32;
                for dy in 0..f {
                    let row = &luma[(y * f + dy) * w + x * f..][..f];
                    s += row.iter().map(|&v| u32::from(v)).sum::<u32>();
                }
                px[y * aw + x] = s as f32 * norm;
            }
        }
        let mut levels = vec![Level::new(aw, ah, px)];
        for _ in 1..LEVELS {
            let next = levels.last().expect("one level").half();
            levels.push(next);
        }
        Some(Pyramid { levels, factor: f as f64 })
    }

    /// The strongest corner of each grid cell, as level-0 coordinates.
    fn corners(&self) -> Vec<(f32, f32)> {
        let l = &self.levels[0];
        let m = (HALF_WIN as usize + 2) << (LEVELS - 1);
        if l.w <= 2 * m || l.h <= 2 * m {
            return Vec::new();
        }
        let (cw, ch) = ((l.w - 2 * m) / GRID.0, (l.h - 2 * m) / GRID.1);
        let mut best: Vec<(f32, f32, f32)> = Vec::new();
        for gy in 0..GRID.1 {
            for gx in 0..GRID.0 {
                let mut cell = (0.0f32, 0.0, 0.0);
                for y in (m + gy * ch..m + (gy + 1) * ch).step_by(2) {
                    for x in (m + gx * cw..m + (gx + 1) * cw).step_by(2) {
                        let (mut xx, mut xy, mut yy) = (0.0f32, 0.0, 0.0);
                        for dy in -2i32..=2 {
                            for dx in -2i32..=2 {
                                let i = (y as i32 + dy) as usize * l.w + (x as i32 + dx) as usize;
                                xx += l.gx[i] * l.gx[i];
                                xy += l.gx[i] * l.gy[i];
                                yy += l.gy[i] * l.gy[i];
                            }
                        }
                        let tr = 0.5 * (xx + yy);
                        let lam = tr - (0.25 * (xx - yy) * (xx - yy) + xy * xy).sqrt();
                        if lam > cell.0 {
                            cell = (lam, x as f32, y as f32);
                        }
                    }
                }
                best.push(cell);
            }
        }
        let top = best.iter().map(|c| c.0).fold(0.0, f32::max);
        best.into_iter().filter(|c| c.0 > 0.02 * top && c.0 > 1.0).map(|c| (c.1, c.2)).collect()
    }
}

/// Pyramidal Lucas-Kanade: where each of `pts` in `a` is in `b`, or `None` for a
/// point that left the picture or would not converge.
fn track(a: &Pyramid, b: &Pyramid, pts: &[(f32, f32)], guess: (f32, f32)) -> Vec<Option<(f32, f32)>> {
    let n = (2 * HALF_WIN + 1) as usize;
    let mut pw = vec![0.0f32; n * n];
    let mut gxw = vec![0.0f32; n * n];
    let mut gyw = vec![0.0f32; n * n];
    pts.iter()
        .map(|&(px, py)| {
            let mut g = (guess.0 / (1 << (LEVELS - 1)) as f32, guess.1 / (1 << (LEVELS - 1)) as f32);
            for lvl in (0..LEVELS).rev() {
                let (la, lb) = (&a.levels[lvl], &b.levels[lvl]);
                let k = 1.0 / (1 << lvl) as f32;
                let (ux, uy) = (px * k, py * k);
                let hw = HALF_WIN as f32;
                let inside = |l: &Level, x: f32, y: f32| x >= hw && y >= hw && x < l.w as f32 - hw - 1.0 && y < l.h as f32 - hw - 1.0;
                // A coarse level that cannot place the point (too near the edge, or
                // too flat once halved) hands its start down unrefined.
                let skip = |g: (f32, f32)| if lvl == 0 { None } else { Some((2.0 * g.0, 2.0 * g.1)) };
                if !inside(la, ux, uy) {
                    g = skip(g)?;
                    continue;
                }
                let (mut sxx, mut sxy, mut syy) = (0.0f32, 0.0, 0.0);
                for (j, dy) in (-HALF_WIN..=HALF_WIN).enumerate() {
                    for (i, dx) in (-HALF_WIN..=HALF_WIN).enumerate() {
                        let (x, y) = (ux + dx as f32, uy + dy as f32);
                        let o = j * n + i;
                        pw[o] = la.at(&la.px, x, y);
                        gxw[o] = la.at(&la.gx, x, y);
                        gyw[o] = la.at(&la.gy, x, y);
                        sxx += gxw[o] * gxw[o];
                        sxy += gxw[o] * gyw[o];
                        syy += gyw[o] * gyw[o];
                    }
                }
                let det = sxx * syy - sxy * sxy;
                if det < 1e-6 || det < 1e-4 * (sxx + syy) * (sxx + syy) {
                    g = skip(g)?;
                    continue;
                }
                let mut d = g;
                let mut lost = false;
                for _ in 0..30 {
                    let (cx, cy) = (ux + d.0, uy + d.1);
                    if !inside(lb, cx, cy) {
                        lost = true;
                        break;
                    }
                    let (mut ex, mut ey) = (0.0f32, 0.0);
                    for (j, dy) in (-HALF_WIN..=HALF_WIN).enumerate() {
                        for (i, dx) in (-HALF_WIN..=HALF_WIN).enumerate() {
                            let o = j * n + i;
                            let e = pw[o] - lb.at(&lb.px, cx + dx as f32, cy + dy as f32);
                            ex += e * gxw[o];
                            ey += e * gyw[o];
                        }
                    }
                    let (sx, sy) = ((syy * ex - sxy * ey) / det, (sxx * ey - sxy * ex) / det);
                    d = (d.0 + sx, d.1 + sy);
                    if sx * sx + sy * sy < 1e-6 {
                        break;
                    }
                }
                if lost {
                    g = skip(g)?;
                    continue;
                }
                g = if lvl == 0 { d } else { (2.0 * d.0, 2.0 * d.1) };
            }
            Some((px + g.0, py + g.1))
        })
        .collect()
}

/// The least-squares similarity taking `p` to `q`.
fn fit(p: &[(f64, f64)], q: &[(f64, f64)]) -> Option<Sim> {
    let n = p.len() as f64;
    if p.len() < 2 {
        return None;
    }
    let (mut px, mut py, mut qx, mut qy) = (0.0, 0.0, 0.0, 0.0);
    for (a, b) in p.iter().zip(q) {
        px += a.0;
        py += a.1;
        qx += b.0;
        qy += b.1;
    }
    let (px, py, qx, qy) = (px / n, py / n, qx / n, qy / n);
    let (mut sa, mut sb, mut ss) = (0.0, 0.0, 0.0);
    for (a, b) in p.iter().zip(q) {
        let (ax, ay, bx, by) = (a.0 - px, a.1 - py, b.0 - qx, b.1 - qy);
        sa += ax * bx + ay * by;
        sb += ax * by - ay * bx;
        ss += ax * ax + ay * ay;
    }
    if ss < 1e-9 {
        return None;
    }
    let r = Sim { a: sa / ss, b: sb / ss, tx: 0.0, ty: 0.0 };
    let (rx, ry) = r.apply(px, py);
    Some(Sim { tx: qx - rx, ty: qy - ry, ..r })
}

fn residual(m: &Sim, p: (f64, f64), q: (f64, f64)) -> f64 {
    let (x, y) = m.apply(p.0, p.1);
    (x - q.0).hypot(y - q.1)
}

/// RANSAC over two-point similarities, then least squares on the inliers, twice.
fn robust(p: &[(f64, f64)], q: &[(f64, f64)]) -> Option<Sim> {
    if p.len() < 6 {
        return None;
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let mut best: (usize, Sim) = (0, Sim::IDENTITY);
    for _ in 0..200 {
        let (i, j) = (rnd(p.len()), rnd(p.len()));
        if i == j {
            continue;
        }
        let Some(m) = fit(&[p[i], p[j]], &[q[i], q[j]]) else { continue };
        let count = p.iter().zip(q).filter(|(a, b)| residual(&m, **a, **b) < 1.0).count();
        if count > best.0 {
            best = (count, m);
        }
    }
    if best.0 < 6 {
        return None;
    }
    let mut m = best.1;
    for thr in [1.0, 0.5] {
        let (ip, iq): (Vec<_>, Vec<_>) = p.iter().zip(q).filter(|(a, b)| residual(&m, **a, **b) < thr).map(|(a, b)| (*a, *b)).unzip();
        if ip.len() < 6 {
            break;
        }
        m = fit(&ip, &iq)?;
    }
    Some(m)
}

/// The motion from frame `a` to frame `b`, as the similarity taking a point of
/// `a` to where it is in `b`, in centred **source** pixels.
fn motion(a: &Pyramid, b: &Pyramid) -> Option<Sim> {
    let l = &a.levels[0];
    let (cx, cy) = ((l.w as f64 - 1.0) / 2.0, (l.h as f64 - 1.0) / 2.0);
    let pts = a.corners();
    let guess = coarse_shift(a, b);
    // The points are independent: tracked on a few threads at once.
    let per = pts.len().div_ceil(4).max(1);
    let tracked: Vec<Option<(f32, f32)>> = std::thread::scope(|s| {
        let parts: Vec<_> = pts.chunks(per).map(|c| s.spawn(move || track(a, b, c, guess))).collect();
        parts.into_iter().flat_map(|h| h.join().expect("a tracker thread")).collect()
    });
    let (mut p, mut q) = (Vec::new(), Vec::new());
    for (s, t) in pts.iter().zip(tracked) {
        if let Some(t) = t {
            p.push((f64::from(s.0) - cx, f64::from(s.1) - cy));
            q.push((f64::from(t.0) - cx, f64::from(t.1) - cy));
        }
    }
    robust(&p, &q).map(|m| m.scaled(a.factor))
}

/// The whole-picture shift from `a` to `b` found by trying every whole-pixel
/// shift up to [`COARSE`] one level below the top of the pyramid: the start the tracker
/// refines from, so a jolt bigger than its window is still found.
fn coarse_shift(a: &Pyramid, b: &Pyramid) -> (f32, f32) {
    let top = LEVELS - 2;
    let (la, lb) = (&a.levels[top], &b.levels[top]);
    let r = COARSE;
    if la.w <= 4 * r || la.h <= 4 * r {
        return (0.0, 0.0);
    }
    let mut best = (f32::MAX, 0i64, 0i64);
    for dy in -(r as i64)..=r as i64 {
        for dx in -(r as i64)..=r as i64 {
            let mut sad = 0.0f32;
            for y in 2 * r..la.h - 2 * r {
                let ra = &la.px[y * la.w + 2 * r..(y + 1) * la.w - 2 * r];
                let yb = (y as i64 + dy) as usize;
                let xb = (2 * r) as i64 + dx;
                let rb = &lb.px[yb * lb.w + xb as usize..][..ra.len()];
                sad += ra.iter().zip(rb).map(|(p, q)| (p - q).abs()).sum::<f32>();
            }
            if sad < best.0 {
                best = (sad, dx, dy);
            }
        }
    }
    let k = (1 << top) as f32;
    (best.1 as f32 * k, best.2 as f32 * k)
}

/// BT.601 luma of a BGRA picture, one byte a pixel.
pub fn luma_from_bgra(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .take(w * h)
        .map(|p| ((29 * u32::from(p[0]) + 150 * u32::from(p[1]) + 77 * u32::from(p[2])) >> 8) as u8)
        .collect()
}

/// The motion between two `w` x `h` luma pictures: the similarity taking a point
/// of `a` to where it is in `b`, on pixel coordinates centred on the picture.
/// `None` where too little of the picture could be tracked.
pub fn estimate(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<Sim> {
    motion(&Pyramid::of(a, w, h)?, &Pyramid::of(b, w, h)?)
}

// ---- the stored analysis --------------------------------------------------

/// One source range's measured motion: `steps[i]` takes frame `lo + i - 1` to
/// frame `lo + i` (the first is the identity), in centred pixels of a picture
/// `w` x `h`.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub w: u32,
    pub h: u32,
    pub lo: u32,
    pub steps: Vec<Sim>,
}

impl Track {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        for v in [self.w, self.h, self.lo, self.steps.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for s in &self.steps {
            for v in [s.a, s.b, s.tx, s.ty] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    fn from_bytes(b: &[u8]) -> Option<Track> {
        let rest = b.strip_prefix(MAGIC)?;
        let u = |i: usize| rest.get(i * 4..i * 4 + 4).map(|s| u32::from_le_bytes(s.try_into().expect("4")));
        let (w, h, lo, n) = (u(0)?, u(1)?, u(2)?, u(3)? as usize);
        let body = &rest[16..];
        if body.len() != n * 32 {
            return None;
        }
        let f = |i: usize| f64::from_le_bytes(body[i * 8..i * 8 + 8].try_into().expect("8"));
        let steps = (0..n).map(|k| Sim { a: f(4 * k), b: f(4 * k + 1), tx: f(4 * k + 2), ty: f(4 * k + 3) }).collect();
        Some(Track { w, h, lo, steps })
    }
}

/// Measures the motion of source frames `lo..hi` of `path`. `progress` is set in
/// thousandths of the range done; a set `cancel` stops it with an error.
///
/// The frames measured are the very planes a layer's decoder hands the warp
/// ([`crate::export::each_layer_luma`]): the file's own, before its display turn
/// and pixel stretch, so the maps and the pictures they move share their axes.
pub fn analyse(path: &Path, lo: u32, hi: u32, progress: &AtomicU32, cancel: &AtomicBool) -> crate::Result<Track> {
    if hi <= lo {
        return Err("nothing to analyse: the range is empty".into());
    }
    let count = crate::demux::Demuxer::open(path)?.0.frame_count;
    let hi = hi.min(count.max(lo + 1));
    let mut steps: Vec<Sim> = Vec::with_capacity((hi - lo) as usize);
    let mut prev: Option<Pyramid> = None;
    let mut size = (0, 0);
    let mut stopped = false;
    crate::export::each_layer_luma(path, lo, hi, |index, y, width, height| {
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            return false;
        }
        let (w, h) = (width as usize, height as usize);
        let pyr = Pyramid::of(&y[..w * h], w, h);
        let step = match (&prev, &pyr) {
            (Some(a), Some(b)) if size == (width, height) => motion(a, b).unwrap_or(Sim::IDENTITY),
            _ => Sim::IDENTITY,
        };
        // The first frame is where the path starts: no step into it.
        steps.push(if steps.is_empty() { Sim::IDENTITY } else { step });
        size = (width, height);
        prev = pyr;
        progress.store(((index + 1 - lo) as u64 * 1000 / u64::from(hi - lo)) as u32, Ordering::Relaxed);
        true
    })?;
    if stopped {
        return Err("stabilise analysis cancelled".into());
    }
    if steps.is_empty() {
        return Err("the source gave no pictures to analyse".into());
    }
    // A file shorter than its header says holds its last frame still.
    while steps.len() < (hi - lo) as usize {
        steps.push(Sim::IDENTITY);
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(Track { w: size.0, h: size.1, lo, steps })
}

// ---- smoothing and the correction ------------------------------------------

/// Gaussian smoothing of `xs` with point-reflected ends, which keeps a straight
/// line straight.
pub(crate) fn smooth(xs: &[f64], sigma: f64) -> Vec<f64> {
    let n = xs.len();
    if n < 2 || sigma <= 0.0 {
        return xs.to_vec();
    }
    let r = (3.0 * sigma).ceil() as i64;
    let kernel: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let total: f64 = kernel.iter().sum();
    let last = n as i64 - 1;
    let at = |i: i64| -> f64 {
        // Point reflection about the end samples, repeated for a kernel wider
        // than the clip: x[-i] = 2 x[0] - x[i], x[n-1+i] = 2 x[n-1] - x[n-1-i].
        let (mut i, mut off, mut sign) = (i, 0.0, 1.0);
        loop {
            if i < 0 {
                off += sign * 2.0 * xs[0];
                sign = -sign;
                i = -i;
            } else if i > last {
                off += sign * 2.0 * xs[last as usize];
                sign = -sign;
                i = 2 * last - i;
            } else {
                return off + sign * xs[i as usize];
            }
        }
    };
    (0..n as i64)
        .map(|k| kernel.iter().zip(-r..=r).map(|(w, i)| w * at(k + i)).sum::<f64>() / total)
        .collect()
}

/// The per-frame correction of one analysed range at one setting: for each file
/// frame, the map from a centred pixel of the stabilised picture to the centred
/// pixel of the decoded one it shows, zoom included.
#[derive(Clone, Debug)]
pub struct Correction {
    lo: u32,
    w: u32,
    h: u32,
    maps: Vec<Sim>,
    /// The enlargement [`Crop::Zoom`] chose, `1.0` for [`Crop::Full`].
    pub zoom: f64,
}

impl Correction {
    /// The map for file frame `frame` on a `w`-wide picture (the analysed size or
    /// any other, a stand-in's), or `None` before the analysed range. A frame past
    /// its end -- a file longer than its header said -- keeps the last map.
    pub fn at(&self, frame: u32, w: u32) -> Option<Sim> {
        let i = (frame.checked_sub(self.lo)? as usize).min(self.maps.len().checked_sub(1)?);
        let m = self.maps[i];
        Some(m.scaled(f64::from(w) / f64::from(self.w.max(1))))
    }

    /// The analysed picture's size.
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
}

/// The correction `track` gives at `s`, for a source at `fps` frames a second.
pub fn correct(track: &Track, s: Stabilise, fps: f64) -> Correction {
    let mut path = Vec::with_capacity(track.steps.len());
    let mut c = Sim::IDENTITY;
    for (i, step) in track.steps.iter().enumerate() {
        if i > 0 {
            c = c.then(*step);
        }
        path.push(c);
    }
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
    let sigma = (f64::from(s.amount()) * 2.0 * fps).max(0.5);
    let mut params: [Vec<f64>; 4] = Default::default();
    let mut unwrap = 0.0f64;
    let mut prev_angle = 0.0;
    for m in &path {
        let mut p = m.params();
        // Keep the angle continuous past a half turn.
        let mut a = p[0] + unwrap;
        while a - prev_angle > std::f64::consts::PI {
            a -= 2.0 * std::f64::consts::PI;
            unwrap -= 2.0 * std::f64::consts::PI;
        }
        while a - prev_angle < -std::f64::consts::PI {
            a += 2.0 * std::f64::consts::PI;
            unwrap += 2.0 * std::f64::consts::PI;
        }
        prev_angle = a;
        p[0] = a;
        for (dst, v) in params.iter_mut().zip(p) {
            dst.push(v);
        }
    }
    let smoothed: Vec<Vec<f64>> = params.iter().map(|xs| smooth(xs, sigma)).collect();
    let maps: Vec<Sim> = path
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let sm = Sim::from_params([smoothed[0][k], smoothed[1][k], smoothed[2][k], smoothed[3][k]]);
            sm.inverse().then(*c)
        })
        .collect();
    let zoom = match s.crop() {
        Crop::Full => 1.0,
        Crop::Zoom => maps.iter().map(|m| zoom_for(m, track.w, track.h)).fold(1.0, f64::max),
    };
    let maps = maps.into_iter().map(|m| Sim { a: m.a / zoom, b: m.b / zoom, ..m }).collect();
    Correction { lo: track.lo, w: track.w, h: track.h, maps, zoom }
}

/// The least enlargement `z` for which `m(p / z)` stays inside a `w` x `h`
/// picture for every `p` of it, capped at [`MAX_ZOOM`].
fn zoom_for(m: &Sim, w: u32, h: u32) -> f64 {
    let (hw, hh) = ((f64::from(w) - 1.0) / 2.0, (f64::from(h) - 1.0) / 2.0);
    // The corners map to u * (R c) + t with u = 1 / z; each picture edge bounds u.
    let mut u: f64 = 1.0;
    for (cx, cy) in [(-hw, -hh), (hw, -hh), (-hw, hh), (hw, hh)] {
        let r = Sim { tx: 0.0, ty: 0.0, ..*m }.apply(cx, cy);
        for (a, t, lim) in [(r.0, m.tx, hw), (r.1, m.ty, hh)] {
            if a > 1e-9 {
                u = u.min((lim - t) / a);
            } else if a < -1e-9 {
                u = u.min((-lim - t) / a);
            }
        }
    }
    if u <= 1.0 / MAX_ZOOM { MAX_ZOOM } else { (1.0 / u).max(1.0) }
}

// ---- applying it ----------------------------------------------------------

/// Draws the stabilised picture: each pixel of the `w` x `h` I420 output is the
/// bilinear sample of the input at `m` of its centred position (chroma at half
/// the shift). Outside the input is black.
#[allow(clippy::too_many_arguments)]
pub fn warp_i420<T: crate::scale::Sample>(y: &[T], u: &[T], v: &[T], w: u32, h: u32, m: Sim, out: &mut (Vec<T>, Vec<T>, Vec<T>)) {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    out.0.resize(w * h, T::BLACK);
    out.1.resize(cw * ch, T::NEUTRAL);
    out.2.resize(cw * ch, T::NEUTRAL);
    let half = Sim { tx: m.tx / 2.0, ty: m.ty / 2.0, ..m };
    std::thread::scope(|s| {
        s.spawn(|| warp_plane(y, w, h, m, T::BLACK, &mut out.0));
        s.spawn(|| warp_plane(u, cw, ch, half, T::NEUTRAL, &mut out.1));
        warp_plane(v, cw, ch, half, T::NEUTRAL, &mut out.2);
    });
}

fn warp_plane<T: crate::scale::Sample>(src: &[T], w: usize, h: usize, m: Sim, black: T, dst: &mut [T]) {
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8);
    let rows = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (chunk, part) in dst.chunks_mut(rows * w).enumerate() {
            s.spawn(move || {
                for (r, line) in part.chunks_mut(w).enumerate() {
                    let oy = (chunk * rows + r) as f64 - cy;
                    let (mut sx, mut sy) = m.apply(-cx, oy);
                    (sx, sy) = (sx + cx, sy + cy);
                    let (dx, dy) = (m.a, m.b);
                    for px in line.iter_mut() {
                        *px = sample(src, w, h, sx, sy).unwrap_or(black);
                        sx += dx;
                        sy += dy;
                    }
                }
            });
        }
    });
}

#[inline]
fn sample<T: crate::scale::Sample>(src: &[T], w: usize, h: usize, x: f64, y: f64) -> Option<T> {
    if x < -0.5 || y < -0.5 || x > w as f64 - 0.5 || y > h as f64 - 0.5 {
        return None;
    }
    let x = x.clamp(0.0, (w - 1) as f64);
    let y = y.clamp(0.0, (h - 1) as f64);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let p = |xx: usize, yy: usize| src[yy * w + xx].code() as f32;
    let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
    let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
    Some(T::from_code((top + (bot - top) * fy + 0.5) as u32))
}

// ---- the cache ------------------------------------------------------------

/// The source's identity: its canonical path, length and modification time, as
/// a proxy's name is made ([`crate::proxy::path_for`]).
fn source_key(source: &Path) -> Option<u64> {
    let meta = std::fs::metadata(source).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).map_or((0, 0), |d| (d.as_secs(), d.subsec_nanos()));
    let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut bytes = canonical.as_os_str().as_encoded_bytes().to_vec();
    bytes.extend_from_slice(&meta.len().to_le_bytes());
    bytes.extend_from_slice(&mtime.0.to_le_bytes());
    bytes.extend_from_slice(&mtime.1.to_le_bytes());
    bytes.extend_from_slice(&ALGO.to_le_bytes());
    Some(crate::demux::fnv1a(&bytes))
}

fn cache_dir() -> Option<PathBuf> {
    crate::demux::cache_dir(crate::envcfg::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"), "stabilise")
}

/// Where the analysis of source frames `lo..hi` of `source` is kept.
pub fn cache_path(source: &Path, lo: u32, hi: u32) -> Option<PathBuf> {
    Some(cache_dir()?.join(format!("{:016x}-{lo}-{hi}.stab", source_key(source)?)))
}

/// A finished analysis of `source` that covers `lo..hi`: the exact range, or the
/// smallest wider one (a clip trimmed after it was analysed).
fn find(source: &Path, lo: u32, hi: u32) -> Option<PathBuf> {
    let exact = cache_path(source, lo, hi)?;
    if exact.is_file() {
        return Some(exact);
    }
    let prefix = format!("{:016x}-", source_key(source)?);
    let mut best: Option<(u32, PathBuf)> = None;
    for e in std::fs::read_dir(cache_dir()?).ok()?.flatten() {
        let name = e.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(&prefix)).and_then(|n| n.strip_suffix(".stab")) else {
            continue;
        };
        let Some((a, b)) = rest.split_once('-') else { continue };
        let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) else { continue };
        if a <= lo && hi <= b && best.as_ref().is_none_or(|(span, _)| b - a < *span) {
            best = Some((b - a, e.path()));
        }
    }
    best.map(|(_, p)| p)
}

fn write_track(path: &Path, t: &Track) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = path.with_extension("part");
    std::fs::write(&part, t.to_bytes())?;
    std::fs::rename(&part, path)
}

/// The analysis covering `lo..hi` of `source`, read from the cache.
pub fn cached_track(source: &Path, lo: u32, hi: u32) -> Option<Track> {
    Track::from_bytes(&std::fs::read(find(source, lo, hi)?).ok()?)
}

/// The correction of source frames `lo..hi` of `source` at `s`, from a finished
/// analysis, or `None` while there is none. Remembered per analysis file and
/// setting, so a renderer reopening a span pays for it once.
pub fn correction(source: &Path, lo: u32, hi: u32, s: Stabilise, fps: f64) -> Option<Arc<Correction>> {
    type Memo = Mutex<HashMap<(PathBuf, u32, Crop, u64), Arc<Correction>>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let file = find(source, lo, hi)?;
    let key = (file.clone(), u32::from(s.0.get()), s.crop(), fps.to_bits());
    let memo = MEMO.get_or_init(Default::default);
    if let Some(c) = memo.lock().ok()?.get(&key) {
        return Some(c.clone());
    }
    let track = Track::from_bytes(&std::fs::read(&file).ok()?)?;
    let c = Arc::new(correct(&track, s, fps));
    let mut m = memo.lock().ok()?;
    if m.len() > 64 {
        m.clear();
    }
    m.insert(key, c.clone());
    Some(c)
}

/// [`correction`] without waiting, for a picture that must come at once (the
/// viewer, a still): `None` while none is finished, and the analysis is started
/// in the background when none was asked for yet -- not again after a failure,
/// which the editor says and retries on request.
pub fn correction_or_start(source: &Path, lo: u32, hi: u32, s: Stabilise, fps: f64) -> Option<Arc<Correction>> {
    let c = correction(source, lo, hi, s, fps);
    if c.is_none() && status(source, lo, hi) == Status::Missing {
        start(source, lo, hi);
    }
    c
}

/// [`correction`], analysing first when no finished analysis covers `lo..hi`:
/// what an export asks, which must never write a stabilised clip unstabilised.
/// An analysis already running for the range (the one the editor started) is
/// waited on rather than run twice; `cancel` stops the waiting, not that job.
/// `progress` is told the fraction measured, `0.0..=1.0`, while it waits; it is
/// never called when a finished analysis was there. Every way it can fail is an
/// error that names the file.
pub fn ensure(
    source: &Path,
    lo: u32,
    hi: u32,
    s: Stabilise,
    fps: f64,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> crate::Result<Arc<Correction>> {
    let named = |why: String| -> crate::Error { format!("cannot stabilise {}: {why}", source.display()).into() };
    if let Some(c) = correction(source, lo, hi, s, fps) {
        return Ok(c);
    }
    let mut now = start(source, lo, hi);
    while let Status::Running(done) = now {
        if cancel.load(Ordering::Relaxed) {
            return Err(named("cancelled while its motion was being measured".into()));
        }
        progress(done);
        std::thread::sleep(std::time::Duration::from_millis(20));
        now = status(source, lo, hi);
    }
    match now {
        Status::Ready => correction(source, lo, hi, s, fps)
            .ok_or_else(|| named("its finished analysis cannot be read back from the cache".into())),
        Status::Failed(e) => Err(named(e)),
        Status::Missing | Status::Running(_) => Err(named("its analysis did not start".into())),
    }
}

// ---- background jobs --------------------------------------------------------

/// Where one source range's analysis stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Finished and cached.
    Ready,
    /// Running, this fraction done.
    Running(f32),
    /// Stopped by an error or a cancel; [`start`] tries again.
    Failed(String),
    /// Never asked for.
    Missing,
}

struct Job {
    progress: AtomicU32,
    cancel: AtomicBool,
    outcome: Mutex<Option<Result<(), String>>>,
}

fn jobs() -> &'static Mutex<HashMap<PathBuf, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<PathBuf, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

/// Where the analysis of `lo..hi` of `source` stands.
pub fn status(source: &Path, lo: u32, hi: u32) -> Status {
    let Some(key) = cache_path(source, lo, hi) else {
        return Status::Failed("the source cannot be read, or there is no cache directory".into());
    };
    let job = jobs().lock().ok().and_then(|j| j.get(&key).cloned());
    // A job holds its outcome lock while it writes the file, so the file and the
    // outcome are seen together: never a file that is there before the job says it
    // is done (which would read as running once the file is cleared).
    let outcome = job.as_ref().map(|j| j.outcome.lock().ok().and_then(|o| o.clone()));
    if find(source, lo, hi).is_some() {
        return Status::Ready;
    }
    match (job, outcome) {
        (Some(job), Some(None)) => Status::Running(job.progress.load(Ordering::Relaxed) as f32 / 1000.0),
        // Finished, but its file is gone (a cleared cache): measured again on start.
        (Some(_), Some(Some(Ok(())))) | (None, _) | (Some(_), None) => Status::Missing,
        (Some(_), Some(Some(Err(e)))) => Status::Failed(e),
    }
}

/// Starts analysing `lo..hi` of `source` in the background unless it is cached
/// or already running, and says where it stands.
pub fn start(source: &Path, lo: u32, hi: u32) -> Status {
    match status(source, lo, hi) {
        s @ (Status::Ready | Status::Running(_)) => return s,
        Status::Failed(_) | Status::Missing => {}
    }
    let Some(key) = cache_path(source, lo, hi) else {
        return status(source, lo, hi);
    };
    let job = Arc::new(Job { progress: AtomicU32::new(0), cancel: AtomicBool::new(false), outcome: Mutex::new(None) });
    // The check and the insert share one lock, so two starts cannot both see the range as missing.
    let mut map = jobs().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(running) = map.get(&key).filter(|j| j.outcome.lock().unwrap_or_else(|e| e.into_inner()).is_none()) {
        return Status::Running(running.progress.load(Ordering::Relaxed) as f32 / 1000.0);
    }
    map.insert(key.clone(), job.clone());
    drop(map);
    let source = source.to_path_buf();
    std::thread::spawn(move || {
        let measured = analyse(&source, lo, hi, &job.progress, &job.cancel);
        // Written and recorded under one lock, so [`status`] sees both or neither.
        let mut o = job.outcome.lock().unwrap_or_else(|e| e.into_inner());
        *o = Some(measured.and_then(|t| write_track(&key, &t).map_err(Into::into)).map_err(|e| e.to_string()));
    });
    Status::Running(0.0)
}

/// Stops a running analysis of `lo..hi` of `source`. `false` when none runs.
pub fn cancel(source: &Path, lo: u32, hi: u32) -> bool {
    let Some(key) = cache_path(source, lo, hi) else { return false };
    let Some(job) = jobs().lock().ok().and_then(|j| j.get(&key).cloned()) else { return false };
    let running = job.outcome.lock().is_ok_and(|o| o.is_none());
    job.cancel.store(true, Ordering::Relaxed);
    running
}

/// Waits for the analysis of `lo..hi` of `source` to finish, starting it if it
/// is not running, and says how it ended.
pub fn wait(source: &Path, lo: u32, hi: u32) -> Status {
    let mut s = start(source, lo, hi);
    while let Status::Running(_) = s {
        std::thread::sleep(std::time::Duration::from_millis(20));
        s = status(source, lo, hi);
    }
    s
}

/// The source frames `lo..hi` a clip plays, in the file's own numbering, for a
/// source at `rate` against the timeline ([`crate::project::Rate`]).
pub fn file_range(clip: &crate::project::Clip, rate: crate::project::Rate) -> (u32, u32) {
    let lo = rate.source_at(clip.in_frame);
    let hi = rate.source_at(clip.out_frame.saturating_sub(1)) + 1;
    (lo, hi.max(lo + 1))
}

/// [`file_range`] for a clip of the source at `source`, on a timeline at `fps`:
/// the file's own rate read from its header (remembered per path).
pub fn clip_range(source: &Path, clip: &crate::project::Clip, fps: f64) -> (u32, u32) {
    static RATES: OnceLock<Mutex<HashMap<PathBuf, f64>>> = OnceLock::new();
    let rates = RATES.get_or_init(Default::default);
    let known = rates.lock().ok().and_then(|r| r.get(source).copied());
    let file_fps = known.or_else(|| {
        let f = crate::demux::Demuxer::open(source).ok()?.0.frame_rate;
        if let Ok(mut r) = rates.lock() {
            r.insert(source.to_path_buf(), f);
        }
        Some(f)
    });
    let rate = file_fps
        .and_then(|f| crate::project::Rate::from_fps(f, fps).ok())
        .unwrap_or(crate::project::Rate::REAL_TIME);
    file_range(clip, rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_packs_into_one_byte_and_reads_back() {
        assert_eq!(std::mem::size_of::<Option<Stabilise>>(), 1);
        for (p, c) in [(0, Crop::Zoom), (100, Crop::Zoom), (0, Crop::Full), (37, Crop::Full), (100, Crop::Full)] {
            let s = Stabilise::with_percent(p, c);
            assert_eq!((s.strength(), s.crop()), (p, c));
        }
        assert_eq!(Stabilise::new(0.756, Crop::Zoom).strength(), 76);
        assert_eq!(Stabilise::with_percent(250, Crop::Full).strength(), 100);
    }

    #[test]
    fn smoothing_keeps_a_straight_line() {
        let xs: Vec<f64> = (0..50).map(|i| 3.0 + 2.0 * f64::from(i)).collect();
        for (a, b) in smooth(&xs, 20.0).iter().zip(&xs) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }

    #[test]
    fn sim_inverse_and_compose() {
        let m = Sim::from_params([0.1, 0.02, 5.0, -3.0]);
        let id = m.then(m.inverse());
        assert!((id.a - 1.0).abs() < 1e-12 && id.b.abs() < 1e-12 && id.tx.abs() < 1e-9 && id.ty.abs() < 1e-9);
    }

    #[test]
    fn track_round_trips_its_bytes() {
        let t = Track { w: 640, h: 360, lo: 7, steps: vec![Sim::IDENTITY, Sim::from_params([0.01, 0.0, 1.5, -2.0])] };
        assert_eq!(Track::from_bytes(&t.to_bytes()), Some(t));
    }

    #[test]
    fn zoom_hides_a_shift() {
        // A 10 px shift on a 101 px wide picture: the corner at 50 lands at 50 / z + 10,
        // inside the 50 half-width for z = 50 / 40 = 1.25.
        let z = zoom_for(&Sim { tx: 10.0, ..Sim::IDENTITY }, 101, 101);
        assert!((z - 1.25).abs() < 1e-9, "{z}");
        assert_eq!(zoom_for(&Sim::IDENTITY, 100, 100), 1.0);
    }

    #[test]
    fn warp_by_identity_is_a_copy_and_a_shift_moves_the_picture() {
        let (w, h) = (16u32, 8u32);
        let y: Vec<u8> = (0..w * h).map(|i| (i % w * 10) as u8).collect();
        let c = vec![128u8; 8 * 4];
        let mut out = Default::default();
        warp_i420(&y, &c, &c, w, h, Sim::IDENTITY, &mut out);
        assert_eq!(out.0, y);
        warp_i420(&y, &c, &c, w, h, Sim { tx: 1.0, ..Sim::IDENTITY }, &mut out);
        assert_eq!(out.0[0], y[1], "each pixel shows the one to its right");
        assert_eq!(out.0[w as usize - 1], 16, "the right edge ran off the picture");
    }

    #[test]
    fn warp_keeps_ten_bit_codes() {
        let (w, h) = (16u32, 8u32);
        let y: Vec<u16> = (0..w * h).map(|i| 64 + (i % w * 50) as u16).collect();
        let c = vec![512u16; 8 * 4];
        let mut out = Default::default();
        warp_i420(&y, &c, &c, w, h, Sim::IDENTITY, &mut out);
        assert_eq!(out.0, y);
        warp_i420(&y, &c, &c, w, h, Sim { tx: 0.5, ..Sim::IDENTITY }, &mut out);
        assert_eq!(out.0[0], 89, "half way between 64 and 114, kept at 10 bits");
    }
}
