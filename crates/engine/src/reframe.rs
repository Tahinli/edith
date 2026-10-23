//! Auto reframe: follow the subject of a wide shot and write the clip's placement
//! keys ([`crate::transform::TransformKey`]) so it fills a canvas of another shape.
//!
//! 1. **Subject.** A clip's frames are walked at a few samples a second, each
//!    reduced to a 64-cell-wide grey grid (a box average). Per cell a cheap
//!    saliency is scored from what moved since the last sample, the local contrast
//!    and a prior for the middle of the picture; the subject is the score-weighted
//!    centre of the cells near the peak. When nothing moves it stays where it was
//!    (a flat opening picture falls back to its contrast, and then the middle).
//!    No model, no colour: the luma planes are all the stabiliser's decoder gives.
//! 2. **Path.** The subject's x and y are smoothed by a Gaussian (the stabiliser's,
//!    see [`crate::stabilise`]), pass through a dead zone -- the crop holds still
//!    until the subject nears the edge of the zone -- and are smoothed once more,
//!    so the crop never wanders on a still subject and never jerks.
//! 3. **Keys.** The window is moved to the path inside the picture's bounds, scaled
//!    so the clip covers the whole canvas (the clip must be placed with
//!    [`crate::scale::FitPolicy::Fit`]); a position past what that scale can hide is
//!    clamped, so no key shows an edge, and neither does anything between two keys.
//!    Keys are kept only where the path turns (Ramer-Douglas-Peucker) and a path that
//!    never leaves its dead zone is one key.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::stabilise::smooth;
use crate::transform::{KeyEase, MAX_TRANSFORM_KEYS, TransformKey, TransformParams};

/// Samples a second the analysis takes, at most.
pub const SAMPLE_HZ: f64 = 8.0;
/// Default smoothness, `0.0..=1.0`.
pub const DEFAULT_SMOOTHNESS: f32 = 0.5;

const GRID_W: usize = 64;
/// A key is dropped when the path stays this close (canvas fractions) to the line through its neighbours.
const TOLERANCE: f64 = 0.003;
/// Share of the visible window the subject may roam without the crop moving.
const DEAD_ZONE: f64 = 0.12;

/// The subject found in each sampled frame: positions as fractions of the picture,
/// `0..=1` from the left and top.
#[derive(Clone, Debug, PartialEq)]
pub struct Subject {
    /// Samples a second.
    pub hz: f64,
    /// File frame, x, y per sample, rising in frame.
    pub samples: Vec<(u32, f64, f64)>,
}

/// Finds the subject frame by frame from grey pictures.
#[derive(Default)]
pub struct Tracker {
    prev: Option<Vec<f32>>,
    last: Option<(f64, f64)>,
}

impl Tracker {
    /// The subject of the next sampled `w` x `h` luma picture, as fractions of it.
    pub fn next(&mut self, y: &[u8], w: usize, h: usize) -> (f64, f64) {
        let (gw, gh) = (GRID_W.min(w).max(1), (GRID_W * h / w.max(1)).clamp(1, h.max(1)));
        let grid = reduce(y, w, h, gw, gh);
        let mut score = vec![0.0f32; gw * gh];
        let mut motion_peak = 0.0f32;
        let mut motion = vec![0.0f32; gw * gh];
        if let Some(prev) = self.prev.as_ref().filter(|p| p.len() == grid.len()) {
            for (m, (a, b)) in motion.iter_mut().zip(grid.iter().zip(prev)) {
                *m = ((a - b).abs() - 4.0).clamp(0.0, 64.0) / 64.0;
                motion_peak = motion_peak.max(*m);
            }
        }
        let at = |x: usize, yy: usize| grid[yy.min(gh - 1) * gw + x.min(gw - 1)];
        for yy in 0..gh {
            for x in 0..gw {
                let grad = (at(x + 1, yy) - at(x, yy)).abs() + (at(x, yy + 1) - at(x, yy)).abs();
                let contrast = (grad / 64.0).min(1.0);
                let (dx, dy) = ((x as f32 + 0.5) / gw as f32 - 0.5, (yy as f32 + 0.5) / gh as f32 - 0.5);
                let centre = 0.6 + 0.4 * (-(dx * dx + dy * dy) / (2.0 * 0.35 * 0.35)).exp();
                // Motion is the subject; contrast only breaks the tie when nothing moves.
                score[yy * gw + x] = (motion[yy * gw + x] + 0.5 * contrast) * centre;
            }
        }
        let moving = motion_peak >= 0.05;
        self.prev = Some(grid);
        if !moving && let Some(last) = self.last {
            return last;
        }
        let peak = score.iter().copied().fold(0.0f32, f32::max);
        let found = if peak > 0.02 {
            let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
            for yy in 0..gh {
                for x in 0..gw {
                    let s = score[yy * gw + x];
                    if s >= 0.5 * peak {
                        let s = f64::from(s);
                        sx += s * (x as f64 + 0.5) / gw as f64;
                        sy += s * (yy as f64 + 0.5) / gh as f64;
                        sw += s;
                    }
                }
            }
            (sx / sw, sy / sw)
        } else {
            (0.5, 0.5)
        };
        if moving {
            self.last = Some(found);
        }
        found
    }
}

/// A box average of a `w` x `h` plane down to `gw` x `gh`.
fn reduce(y: &[u8], w: usize, h: usize, gw: usize, gh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; gw * gh];
    for gy in 0..gh {
        let (y0, y1) = (gy * h / gh, ((gy + 1) * h / gh).max(gy * h / gh + 1).min(h));
        for gx in 0..gw {
            let (x0, x1) = (gx * w / gw, ((gx + 1) * w / gw).max(gx * w / gw + 1).min(w));
            let mut sum = 0u32;
            for row in y0..y1 {
                sum += y[row * w + x0..row * w + x1].iter().map(|&v| u32::from(v)).sum::<u32>();
            }
            out[gy * gw + gx] = sum as f32 / ((y1 - y0) * (x1 - x0)) as f32;
        }
    }
    out
}

/// Finds the subject of file frames `lo..hi` of `path`. `progress` is set in
/// thousandths of the range done; a set `cancel` stops it with an error.
pub fn analyse(path: &Path, lo: u32, hi: u32, progress: &AtomicU32, cancel: &AtomicBool) -> crate::Result<Subject> {
    if hi <= lo {
        return Err("nothing to analyse: the range is empty".into());
    }
    let (count, rate) = {
        let meta = crate::demux::Demuxer::open(path)?.0;
        (meta.frame_count, meta.frame_rate)
    };
    let hi = hi.min(count.max(lo + 1));
    let step = ((rate / SAMPLE_HZ).round() as u32).max(1);
    let mut tracker = Tracker::default();
    let mut samples = Vec::new();
    let mut stopped = false;
    crate::export::each_layer_luma(path, lo, hi, |index, y, width, height| {
        if cancel.load(Ordering::Relaxed) {
            stopped = true;
            return false;
        }
        if (index - lo).is_multiple_of(step) {
            let (x, yy) = tracker.next(&y[..width as usize * height as usize], width as usize, height as usize);
            samples.push((index, x, yy));
        }
        progress.store(((index + 1 - lo) as u64 * 1000 / u64::from(hi - lo)) as u32, Ordering::Relaxed);
        true
    })?;
    if stopped {
        return Err("reframe analysis cancelled".into());
    }
    let Some(&(_, x, y)) = samples.last() else {
        return Err("the source gave no pictures to analyse".into());
    };
    // The clip's last frame holds where the subject ended.
    if samples.last().is_some_and(|s| s.0 < hi - 1) {
        samples.push((hi - 1, x, y));
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(Subject { hz: rate / f64::from(step), samples })
}

/// What the keys are computed for: the picture as the clip displays it, the
/// project canvas, the clip's placement before the reframe and the clip's own
/// key range (`in_frame..out_frame`, as [`crate::project::Clip`] numbers it).
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub src_w: u32,
    pub src_h: u32,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub base: TransformParams,
    pub in_frame: u32,
    pub out_frame: u32,
}

/// The inputs of an auto reframe ([`crate::PlaybackSession::reframe_job`]): [`analyse`]
/// `lo..hi` of `path`, then [`keys`] for `target`.
#[derive(Clone, Debug)]
pub struct ReframeJob {
    pub path: std::path::PathBuf,
    pub lo: u32,
    pub hi: u32,
    pub target: Target,
}

impl Target {
    /// The scale that makes the fitted picture cover the canvas, and the size of
    /// the scaled picture in canvas fractions of the canvas's own width and height.
    fn cover(&self) -> (f64, f64, f64) {
        let (sw, sh) = (f64::from(self.src_w.max(1)), f64::from(self.src_h.max(1)));
        let (cw, ch) = (f64::from(self.canvas_w.max(1)), f64::from(self.canvas_h.max(1)));
        let k = (cw / sw).min(ch / sh);
        let (dw, dh) = (sw * k, sh * k);
        let mut s = (cw / dw).max(ch / dh);
        // The fit rect is rounded to the chroma grid, so a hair of margin keeps its edge off the canvas.
        s = if s <= 1.0005 { 1.0 } else { s * 1.004 };
        (s, dw * s / cw, dh * s / ch)
    }
}

/// The placement keys that follow `subject` on `target`, with the crop held still
/// while the subject stays near it. `smoothness` is `0.0..=1.0`: how slowly the
/// crop follows (about `0.25 + 1.75 * smoothness` seconds of smoothing). The keys
/// keep the clip's own rotation, crop and opacity and cover the canvas at every
/// frame; there is always at least one.
pub fn keys(subject: &Subject, target: &Target, smoothness: f32) -> Vec<TransformKey> {
    let (scale, bw, bh) = target.cover();
    // Window size as a fraction of the scaled picture, per axis, and the pan allowed to each side.
    let (fx, fy) = ((1.0 / bw).min(1.0), (1.0 / bh).min(1.0));
    let s = &subject.samples;
    let sigma = (0.25 + 1.75 * f64::from(smoothness.clamp(0.0, 1.0))) * subject.hz;
    let path = |sel: fn(&(u32, f64, f64)) -> f64, frac: f64| -> Vec<f64> {
        let raw: Vec<f64> = s.iter().map(sel).collect();
        let smoothed = smooth(&raw, sigma);
        let mut c = *smoothed.first().unwrap_or(&0.5);
        let dz = DEAD_ZONE * frac;
        let held: Vec<f64> = smoothed
            .iter()
            .map(|&x| {
                if x - c > dz {
                    c = x - dz;
                } else if c - x > dz {
                    c = x + dz;
                }
                c
            })
            .collect();
        smooth(&held, sigma / 2.0)
    };
    let (px, py) = (path(|t| t.1, fx), path(|t| t.2, fy));
    let lim = |bs: f64| (bs - 1.0).max(0.0) / 2.0;
    let place = |c: f64, bs: f64| ((0.5 - c) * bs).clamp(-lim(bs), lim(bs));
    let pts: Vec<(f64, f64)> = px.iter().zip(&py).map(|(&x, &y)| (place(x, bw), place(y, bh))).collect();
    // Source-position of each sample inside the clip's key range.
    let (lo, hi) = (s.first().map_or(0, |t| t.0), s.last().map_or(1, |t| t.0));
    let span = f64::from(target.out_frame.saturating_sub(1).saturating_sub(target.in_frame));
    let at = |frame: u32| -> u32 {
        let t = if hi > lo { f64::from(frame - lo) / f64::from(hi - lo) } else { 0.0 };
        target.in_frame + (t * span).round() as u32
    };
    let mut keep = vec![false; pts.len()];
    let mut tol = TOLERANCE;
    loop {
        keep.iter_mut().for_each(|k| *k = false);
        simplify(&pts, tol, &mut keep);
        if keep.iter().filter(|&&k| k).count() <= MAX_TRANSFORM_KEYS {
            break;
        }
        tol *= 2.0;
    }
    let mut out: Vec<TransformKey> = Vec::new();
    for (i, _) in keep.iter().enumerate().filter(|(_, k)| **k) {
        let frame = at(s[i].0);
        if out.last().is_some_and(|k| k.at == frame) {
            continue;
        }
        let params = TransformParams { pos_x: pts[i].0 as f32, pos_y: pts[i].1 as f32, scale: scale as f32, ..target.base };
        out.push(TransformKey { at: frame, params, ease: KeyEase::Linear });
    }
    if out.is_empty() {
        let params = TransformParams { scale: scale as f32, ..target.base };
        out.push(TransformKey { at: target.in_frame, params, ease: KeyEase::Linear });
    }
    out
}

/// Marks the points that must stay for a polyline to keep within `tol` of `pts`
/// (Ramer-Douglas-Peucker, by index as time). A path that never leaves `tol` of
/// its first point keeps that point alone.
fn simplify(pts: &[(f64, f64)], tol: f64, keep: &mut [bool]) {
    let Some(&first) = pts.first() else { return };
    if pts.iter().all(|p| (p.0 - first.0).abs() <= tol && (p.1 - first.1).abs() <= tol) {
        keep[0] = true;
        return;
    }
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let mut worst = (0.0f64, a);
        for i in a + 1..b {
            let t = (i - a) as f64 / (b - a) as f64;
            let (ex, ey) = (pts[a].0 + t * (pts[b].0 - pts[a].0), pts[a].1 + t * (pts[b].1 - pts[a].1));
            let d = (pts[i].0 - ex).abs().max((pts[i].1 - ey).abs());
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tol {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 160x90 grey picture with a bright `n`-pixel square at (`x`, `y`).
    fn picture(x: usize, y: usize, n: usize) -> Vec<u8> {
        let mut p = vec![30u8; 160 * 90];
        for row in y..y + n {
            for col in x..x + n {
                p[row * 160 + col] = 235;
            }
        }
        p
    }

    fn target() -> Target {
        Target { src_w: 1920, src_h: 1080, canvas_w: 1080, canvas_h: 1920, base: TransformParams::default(), in_frame: 100, out_frame: 300 }
    }

    fn subject_of(frames: impl Iterator<Item = Vec<u8>>) -> Subject {
        let mut t = Tracker::default();
        let samples = frames.enumerate().map(|(i, f)| {
            let (x, y) = t.next(&f, 160, 90);
            (i as u32 * 3, x, y)
        });
        Subject { hz: 8.0, samples: samples.collect() }
    }

    #[test]
    fn a_moving_bright_square_is_followed() {
        let s = subject_of((0..40).map(|i| picture(4 + i * 3, 37, 16)));
        let (first, last) = (s.samples[2], *s.samples.last().unwrap());
        assert!(first.1 < 0.25 && last.1 > 0.7, "{first:?} {last:?}");
        let k = keys(&s, &target(), 0.2);
        assert!(k.len() >= 2, "{k:?}");
        // The window starts on the left (the picture shifted right, positive) and ends on the right.
        assert!(k[0].params.pos_x > 0.5 && k.last().unwrap().params.pos_x < -0.5, "{k:?}");
        assert!(k.windows(2).all(|w| w[0].at < w[1].at));
        assert_eq!((k[0].at, k.last().unwrap().at), (100, 299));
    }

    #[test]
    fn a_static_scene_is_one_key() {
        let s = subject_of((0..30).map(|_| picture(70, 37, 16)));
        let k = keys(&s, &target(), 0.5);
        assert_eq!(k.len(), 1, "{k:?}");
        assert_eq!(k[0].at, 100);
        // The square is a little left of centre: the crop sits on it, not at zero.
        assert!(k[0].params.scale > 3.0);
    }

    #[test]
    fn a_subject_that_stops_stays_found() {
        let frames = (0..30).map(|i| picture(4 + i.min(15) * 6, 37, 16));
        let s = subject_of(frames);
        let (mid, end) = (s.samples[16], *s.samples.last().unwrap());
        assert!((mid.1 - end.1).abs() < 0.02, "{mid:?} {end:?}");
    }

    #[test]
    fn keys_never_expose_the_frame_edge() {
        // A subject at the very edge of the picture, then the other.
        let s = Subject {
            hz: 8.0,
            samples: (0..60).map(|i| (i, if i < 30 { 0.0 } else { 1.0 }, if i < 30 { 1.0 } else { 0.0 })).collect(),
        };
        for t in [target(), Target { src_w: 1080, src_h: 1920, canvas_w: 1920, canvas_h: 1080, ..target() }] {
            let (scale, bw, bh) = t.cover();
            let k = keys(&s, &t, 0.0);
            for key in &k {
                let p = key.params;
                assert!((f64::from(p.pos_x)).abs() <= ((bw - 1.0) / 2.0).max(0.0) + 1e-6, "{p:?}");
                assert!((f64::from(p.pos_y)).abs() <= ((bh - 1.0) / 2.0).max(0.0) + 1e-6, "{p:?}");
                assert!(f64::from(p.scale) >= scale - 1e-6);
            }
            // The scaled picture covers the canvas on both axes.
            assert!(bw >= 1.0 && bh >= 1.0, "{bw} {bh}");
        }
    }

    #[test]
    fn same_shape_needs_no_reframe() {
        let t = Target { src_w: 1920, src_h: 1080, canvas_w: 1280, canvas_h: 720, ..target() };
        let s = subject_of((0..20).map(|i| picture(4 + i * 6, 37, 16)));
        let k = keys(&s, &t, 0.5);
        assert!(k.iter().all(|k| k.params.scale == 1.0 && k.params.pos_x == 0.0 && k.params.pos_y == 0.0), "{k:?}");
        assert_eq!(k.len(), 1);
    }

    #[test]
    fn long_wandering_paths_stay_within_the_key_cap() {
        let s = Subject {
            hz: 8.0,
            samples: (0..4000).map(|i| (i, 0.5 + 0.45 * (f64::from(i) * 0.37).sin(), 0.5)).collect(),
        };
        let t = Target { out_frame: 4001, ..target() };
        assert!(keys(&s, &t, 0.0).len() <= MAX_TRANSFORM_KEYS);
    }
}
