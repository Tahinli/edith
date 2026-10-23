//! Motion tracking: a region of the picture followed from frame to frame, its centre
//! written out as a path that a mask (or anything else with a position) can follow.
//!
//! The pipeline, per clip, over the source frames from the start frame to the end of
//! the range:
//!
//! 1. **Reduce.** Each decoded frame is box-averaged to luma no wider than
//!    [`TRACK_W`], so the search and the template are sized in these small pixels.
//! 2. **Match.** The template is the region's own luma at the start frame (its
//!    centre, at most [`MAX_TEMPLATE`] pixels a side). Each frame, the template is
//!    searched for by normalised cross-correlation in a window of [`SEARCH`] pixels
//!    around its last place, and the peak is refined to a sub-pixel position by a
//!    parabola through the three scores on each axis.
//! 3. **Learn.** After a match the template takes in [`LEARN`] of the matched
//!    picture, so it follows slow change in the feature, yet keeps its identity.
//! 4. **Stop.** When the best score falls below [`MIN_CONFIDENCE`] the feature is
//!    lost; the track ends there and says where.
//!
//! The matcher ([`Tracker`]) is a pure function over pictures, so it tests without a
//! decode. [`start`] runs `analyse` on a thread of its own, polled by [`status`] and
//! [`take`] and stopped by [`cancel`], the same way stabilisation runs. A backward
//! track decodes its range first, since it walks the frames in reverse.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::scale::{FitPolicy, canvas_to_picture, picture_to_canvas};
use crate::transform::{TransformKey, TransformParams, evaluate};

/// The width pictures are reduced to before they are matched.
pub const TRACK_W: usize = 320;
/// Below this normalised cross-correlation the feature is lost.
pub const MIN_CONFIDENCE: f32 = 0.6;
/// How far, in tracked pixels, the template may move between two frames.
const SEARCH: i64 = 24;
/// The largest template edge in tracked pixels; a bigger region is followed by its centre.
const MAX_TEMPLATE: usize = 48;
/// The smallest template edge in tracked pixels: a small box has too little texture to lock onto.
const MIN_TEMPLATE: usize = 24;
/// Below this standard deviation of luma, a template is flat or nearly so and has nothing to lock onto.
const MIN_TEXTURE: f32 = 8.0;
/// Matches within this many tracked pixels of the best one are the same match, not a rival.
const RIVAL_RADIUS: i64 = 3;
/// A best match this close to the best rival outside [`RIVAL_RADIUS`] is ambiguous (a
/// line, a repeat pattern): the feature cannot be told from its surroundings, so it is lost.
const MAX_RIVAL_RATIO: f32 = 0.9;
/// The share of each matched picture that is learned into the template.
const LEARN: f32 = 0.05;
/// Most pictures a backward track holds in memory (it decodes its range first).
pub const MAX_BACKWARD: usize = 2400;
/// The most a dropped key may stray, in canvas fractions, from the path it stands for.
pub const KEY_TOLERANCE: f32 = 0.002;

/// A picture reduced to luma for tracking: `px` is `w * h` values, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// A region of the picture in canvas fractions, stated the way a rect mask states
/// itself: its centre and its size, `0..=1` each.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The luma of a full picture box-averaged down to [`TRACK_W`] wide (a narrower one
/// is kept as it is).
pub fn reduce(y: &[u8], w: usize, h: usize) -> Plane {
    let f = (w / TRACK_W).max(1);
    let (nw, nh) = (w / f, h / f);
    let mut px = vec![0u8; nw * nh];
    for r in 0..nh {
        for c in 0..nw {
            let mut sum = 0u32;
            for dy in 0..f {
                let row = (r * f + dy) * w + c * f;
                sum += y[row..row + f].iter().map(|&v| u32::from(v)).sum::<u32>();
            }
            px[r * nw + c] = (sum / (f * f) as u32) as u8;
        }
    }
    Plane { w: nw, h: nh, px }
}

/// Follows one template through successive pictures.
pub struct Tracker {
    tw: usize,
    th: usize,
    /// The learned template, raw luma.
    tpl: Vec<f32>,
    /// The template, zero-mean, and its norm: what the correlation is taken against.
    zero_mean: Vec<f32>,
    norm: f32,
    /// The template's top-left corner in tracked pixels, sub-pixel.
    top: (f32, f32),
    /// How far the last step moved the template: the search is centred on where it goes next.
    vel: (f32, f32),
}

impl Tracker {
    /// Takes the template at `region` in `first`, or `None` when that patch is too flat
    /// (see [`MIN_TEXTURE`]) to follow.
    pub fn new(first: &Plane, region: Region) -> Option<Tracker> {
        let (pw, ph) = (first.w as f32, first.h as f32);
        let tw = ((region.w * pw).round() as usize).clamp(MIN_TEMPLATE, MAX_TEMPLATE).min(first.w);
        let th = ((region.h * ph).round() as usize).clamp(MIN_TEMPLATE, MAX_TEMPLATE).min(first.h);
        let x = (region.x * pw - tw as f32 / 2.0).clamp(0.0, (first.w - tw) as f32);
        let y = (region.y * ph - th as f32 / 2.0).clamp(0.0, (first.h - th) as f32);
        let mut t = Tracker { tw, th, tpl: Vec::new(), zero_mean: Vec::new(), norm: 0.0, top: (x, y), vel: (0.0, 0.0) };
        let patch = patch(first, t.top.0.round() as usize, t.top.1.round() as usize, tw, th)?;
        t.tpl = patch;
        t.prepare();
        let std = t.norm / (t.tpl.len() as f32).sqrt();
        (std >= MIN_TEXTURE).then_some(t)
    }

    /// The template's centre in tracked pixels.
    pub fn centre(&self) -> (f32, f32) {
        (self.top.0 + self.tw as f32 / 2.0, self.top.1 + self.th as f32 / 2.0)
    }

    /// Finds the template in `plane`, learns the match, and returns the new centre; `None`
    /// when the best match is below [`MIN_CONFIDENCE`].
    pub fn step(&mut self, plane: &Plane) -> Option<(f32, f32)> {
        let (bx, by) = ((self.top.0 + self.vel.0).round() as i64, (self.top.1 + self.vel.1).round() as i64);
        let mut scores = Vec::with_capacity(((2 * SEARCH + 1) * (2 * SEARCH + 1)) as usize);
        for dy in -SEARCH..=SEARCH {
            for dx in -SEARCH..=SEARCH {
                let (x, y) = (bx + dx, by + dy);
                scores.push((self.score(plane, x, y), x, y));
            }
        }
        let best = scores.iter().copied().fold((f32::MIN, bx, by), |a, b| if b.0 > a.0 { b } else { a });
        let (score, x, y) = best;
        if score < MIN_CONFIDENCE {
            return None;
        }
        // The best rival outside the peak's own neighbourhood: a match that is nearly as
        // good elsewhere means the picture cannot tell the feature from its surroundings.
        let rival = scores
            .iter()
            .filter(|&&(_, rx, ry)| (rx - x).abs().max((ry - y).abs()) > RIVAL_RADIUS)
            .map(|&(s, _, _)| s)
            .fold(f32::MIN, f32::max);
        if rival > MAX_RIVAL_RATIO * score {
            return None;
        }
        let sx = peak(self.score(plane, x - 1, y), score, self.score(plane, x + 1, y));
        let sy = peak(self.score(plane, x, y - 1), score, self.score(plane, x, y + 1));
        let before = self.top;
        self.top = (x as f32 + sx, y as f32 + sy);
        self.vel = (self.top.0 - before.0, self.top.1 - before.1);
        // Learn the matched picture, at the whole-pixel place it was found.
        if let Some(matched) = patch(plane, x.max(0) as usize, y.max(0) as usize, self.tw, self.th) {
            for (t, m) in self.tpl.iter_mut().zip(matched) {
                *t += LEARN * (m - *t);
            }
            self.prepare();
        }
        Some(self.centre())
    }

    /// The zero-mean template and its norm, from the raw template.
    fn prepare(&mut self) {
        let n = self.tpl.len() as f32;
        let mean = self.tpl.iter().sum::<f32>() / n;
        self.zero_mean = self.tpl.iter().map(|v| v - mean).collect();
        self.norm = self.zero_mean.iter().map(|v| v * v).sum::<f32>().sqrt();
    }

    /// The normalised cross-correlation of the template with the patch whose top-left
    /// is `(x, y)`; `-1` when the patch leaves the picture, `0` when it is flat.
    fn score(&self, plane: &Plane, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x as usize + self.tw > plane.w || y as usize + self.th > plane.h {
            return -1.0;
        }
        let (x, y) = (x as usize, y as usize);
        let (mut sum, mut sq, mut dot) = (0.0f32, 0.0f32, 0.0f32);
        for r in 0..self.th {
            let row = (y + r) * plane.w + x;
            for c in 0..self.tw {
                let p = f32::from(plane.px[row + c]);
                sum += p;
                sq += p * p;
                dot += self.zero_mean[r * self.tw + c] * p;
            }
        }
        let var = sq - sum * sum / (self.tw * self.th) as f32;
        if var <= 1e-6 {
            return 0.0;
        }
        dot / (self.norm * var.sqrt())
    }
}

/// The `w` by `h` luma patch at `(x, y)`, or `None` when it leaves the picture.
fn patch(plane: &Plane, x: usize, y: usize, w: usize, h: usize) -> Option<Vec<f32>> {
    if x + w > plane.w || y + h > plane.h {
        return None;
    }
    Some((0..h).flat_map(|r| plane.px[(y + r) * plane.w + x..][..w].iter().map(|&v| f32::from(v))).collect())
}

/// The sub-pixel offset of a peak from its neighbours' scores, within half a pixel.
fn peak(left: f32, mid: f32, right: f32) -> f32 {
    let d = left - 2.0 * mid + right;
    if left < -1.0 || right < -1.0 || d.abs() < 1e-9 {
        return 0.0;
    }
    (0.5 * (left - right) / d).clamp(-0.5, 0.5)
}

/// The clip's placement as the renderer puts it on the canvas: the base transform and its
/// keys (at source frames), the canvas size and the fit policy. A track matches the source
/// picture and reports its path on the canvas, so each point goes through this frame's
/// placement.
#[derive(Clone)]
pub struct Pose {
    pub base: TransformParams,
    pub keys: Vec<TransformKey>,
    pub canvas: (u32, u32),
    pub fit: FitPolicy,
}

impl Pose {
    /// The placement at source frame `frame`.
    fn at(&self, frame: u32) -> TransformParams {
        evaluate(&self.base, &self.keys, f64::from(frame))
    }
}

/// The box `region` (canvas fractions) covers, in picture fractions of a `src` picture
/// placed under `t`: the bounding box of its four corners mapped back.
fn picture_region(region: Region, src: (u32, u32), t: &TransformParams, canvas: (u32, u32), fit: FitPolicy) -> Region {
    let (hw, hh) = (region.w / 2.0, region.h / 2.0);
    let corners = [(-hw, -hh), (hw, -hh), (-hw, hh), (hw, hh)]
        .map(|(dx, dy)| canvas_to_picture(src, t, canvas, fit, (region.x + dx, region.y + dy)));
    let (lo_x, hi_x) = corners.iter().fold((f32::MAX, f32::MIN), |(l, h), c| (l.min(c.0), h.max(c.0)));
    let (lo_y, hi_y) = corners.iter().fold((f32::MAX, f32::MIN), |(l, h), c| (l.min(c.1), h.max(c.1)));
    Region { x: (lo_x + hi_x) / 2.0, y: (lo_y + hi_y) / 2.0, w: hi_x - lo_x, h: hi_y - lo_y }
}

/// The path of one track: `(frame, x, y)` in canvas fractions, in the order it was
/// followed (ascending for a forward track, descending for a backward one), and the
/// frame it was lost at when it stopped early.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub points: Vec<(u32, f32, f32)>,
    pub lost_at: Option<u32>,
}

/// Steps a [`Tracker`] through pictures, numbering the points it makes.
struct Walk {
    /// The box to follow, in canvas fractions, as it is at the start frame.
    region: Region,
    pose: Pose,
    tracker: Option<Tracker>,
    points: Vec<(u32, f32, f32)>,
    lost_at: Option<u32>,
}

impl Walk {
    fn new(region: Region, pose: Pose) -> Walk {
        Walk { region, pose, tracker: None, points: Vec::new(), lost_at: None }
    }

    /// Takes picture `plane` (of a source picture `src` pixels big) as frame `index`;
    /// `false` when the feature is lost there. The first frame sets the template from
    /// the box, and every point is recorded on the canvas.
    fn push(&mut self, index: u32, plane: &Plane, src: (u32, u32)) -> bool {
        let t = self.pose.at(index);
        let centre = match self.tracker.as_mut() {
            None => {
                let box_ = picture_region(self.region, src, &t, self.pose.canvas, self.pose.fit);
                self.tracker = Tracker::new(plane, box_);
                self.tracker.as_ref().map(Tracker::centre)
            }
            Some(tr) => tr.step(plane),
        };
        match centre {
            Some((x, y)) => {
                let picture = (x / plane.w as f32, y / plane.h as f32);
                let (cx, cy) = picture_to_canvas(src, &t, self.pose.canvas, self.pose.fit, picture);
                self.points.push((index, cx, cy));
                true
            }
            None => {
                self.lost_at = Some(index);
                false
            }
        }
    }

    fn finish(self) -> Track {
        Track { points: self.points, lost_at: self.lost_at }
    }
}

/// Tracks `region` from frame `start` through the frames `lo..hi` of `path`, forward
/// (`start` to `hi`) or backward (`lo` to `start`). `progress` is set in thousandths of
/// the range done; a set `cancel` stops it with an error.
fn analyse(
    path: &Path,
    (lo, hi): (u32, u32),
    start: u32,
    forward: bool,
    region: Region,
    pose: Pose,
    job: &Job,
) -> crate::Result<Track> {
    let (progress, cancel) = (&job.progress, &job.cancel);
    if hi <= lo || start < lo || start >= hi {
        return Err("nothing to track: the range is empty".into());
    }
    let span = u64::from(hi - lo);
    let mut walk = Walk::new(region, pose);
    let mut stopped = false;
    if forward {
        crate::export::each_layer_luma(path, start, hi, |index, y, w, h| {
            if cancel.load(Ordering::Relaxed) {
                stopped = true;
                return false;
            }
            let more = walk.push(index, &reduce(y, w as usize, h as usize), (w, h));
            progress.store(((index + 1 - lo) as u64 * 1000 / span) as u32, Ordering::Relaxed);
            more
        })?;
    } else {
        let mut held: Vec<(u32, Plane, (u32, u32))> = Vec::new();
        let mut too_long = false;
        crate::export::each_layer_luma(path, lo, start + 1, |index, y, w, h| {
            if cancel.load(Ordering::Relaxed) {
                stopped = true;
                return false;
            }
            if held.len() >= MAX_BACKWARD {
                too_long = true;
                return false;
            }
            held.push((index, reduce(y, w as usize, h as usize), (w, h)));
            progress.store(((index + 1 - lo) as u64 * 1000 / span) as u32, Ordering::Relaxed);
            true
        })?;
        if too_long {
            return Err(format!("a backward track holds at most {MAX_BACKWARD} frames: track the part nearer the start").into());
        }
        for (index, plane, src) in held.iter().rev() {
            if cancel.load(Ordering::Relaxed) {
                stopped = true;
                break;
            }
            if !walk.push(*index, plane, *src) {
                break;
            }
        }
    }
    if stopped {
        return Err("tracking cancelled".into());
    }
    if walk.points.is_empty() {
        return Err("the source gave no pictures to track".into());
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(walk.finish())
}

/// Reduces a track to keys: the points kept by Douglas-Peucker simplification, so that
/// every dropped point lies within `tolerance` of the straight line between the kept
/// ones. `points` must be in ascending frame order.
pub fn simplify(points: &[(u32, f32, f32)], tolerance: f32) -> Vec<usize> {
    let n = points.len();
    if n <= 2 {
        return (0..n).collect();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0, n - 1)];
    while let Some((a, b)) = stack.pop() {
        let (fa, xa, ya) = points[a];
        let (fb, xb, yb) = points[b];
        let span = (fb - fa).max(1) as f32;
        let mut worst = (0.0f32, a);
        for (i, &(f, x, y)) in points.iter().enumerate().take(b).skip(a + 1) {
            let t = (f - fa) as f32 / span;
            let err = (x - (xa + (xb - xa) * t)).abs().max((y - (ya + (yb - ya) * t)).abs());
            if err > worst.0 {
                worst = (err, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    (0..n).filter(|&i| keep[i]).collect()
}

// ---- background jobs --------------------------------------------------------

/// A running track, by its id.
pub type JobId = u64;

/// Where a track stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Still running, this fraction done.
    Running(f32),
    /// Finished; [`take`] returns the outcome.
    Finished,
    /// Not known: taken already, or never started.
    Gone,
}

struct Job {
    progress: AtomicU32,
    cancel: AtomicBool,
    outcome: Mutex<Option<Result<Track, String>>>,
}

fn jobs() -> &'static Mutex<HashMap<JobId, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<JobId, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

/// Starts tracking `region` from frame `start` in the background; see `analyse`.
pub fn start(path: &Path, range: (u32, u32), start: u32, forward: bool, region: Region, pose: Pose) -> JobId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let job = Arc::new(Job { progress: AtomicU32::new(0), cancel: AtomicBool::new(false), outcome: Mutex::new(None) });
    if let Ok(mut j) = jobs().lock() {
        j.insert(id, job.clone());
    }
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let outcome = analyse(&path, range, start, forward, region, pose, &job);
        let mut o = job.outcome.lock().unwrap_or_else(|e| e.into_inner());
        *o = Some(outcome.map_err(|e| e.to_string()));
    });
    id
}

/// Where the track `id` stands.
pub fn status(id: JobId) -> Status {
    let Some(job) = jobs().lock().ok().and_then(|j| j.get(&id).cloned()) else {
        return Status::Gone;
    };
    let done = job.outcome.lock().is_ok_and(|o| o.is_some());
    if done {
        Status::Finished
    } else {
        Status::Running(job.progress.load(Ordering::Relaxed) as f32 / 1000.0)
    }
}

/// The outcome of the finished track `id`, which is then forgotten; `None` while it
/// runs or when it is gone.
pub fn take(id: JobId) -> Option<Result<Track, String>> {
    let job = jobs().lock().ok().and_then(|j| j.get(&id).cloned())?;
    let outcome = job.outcome.lock().ok()?.take()?;
    if let Ok(mut j) = jobs().lock() {
        j.remove(&id);
    }
    Some(outcome)
}

/// Stops the running track `id`. `false` when it is not running.
pub fn cancel(id: JobId) -> bool {
    let Some(job) = jobs().lock().ok().and_then(|j| j.get(&id).cloned()) else { return false };
    let running = job.outcome.lock().is_ok_and(|o| o.is_none());
    job.cancel.store(true, Ordering::Relaxed);
    running
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bright square drifting over a dark picture follows to within 1 % of the canvas.
    #[test]
    fn follows_a_moving_square_within_one_percent() {
        let (w, h, side) = (160usize, 90usize, 24usize);
        let frame = |f: usize| {
            let (x0, y0) = (30 + 2 * f, 20 + f);
            let mut px = vec![20u8; w * h];
            for r in y0..y0 + side {
                for c in x0..x0 + side {
                    px[r * w + c] = 230;
                }
            }
            Plane { w, h, px }
        };
        let centre = |f: usize| ((30 + 2 * f + side / 2) as f32 / w as f32, (20 + f + side / 2) as f32 / h as f32);
        let (cx, cy) = centre(0);
        // A region a little wider than the square, so the template has edges to lock onto.
        let grow = 12;
        let region = Region { x: cx, y: cy, w: (side + grow) as f32 / w as f32, h: (side + grow) as f32 / h as f32 };
        let pose = Pose { base: TransformParams::default(), keys: Vec::new(), canvas: (w as u32, h as u32), fit: FitPolicy::Fit };
        let mut walk = Walk::new(region, pose.clone());
        for f in 0..30 {
            assert!(walk.push(f as u32, &frame(f), (w as u32, h as u32)), "lost at frame {f}");
        }
        let track = walk.finish();
        assert_eq!(track.lost_at, None);
        for &(f, x, y) in &track.points {
            let (tx, ty) = centre(f as usize);
            assert!((x - tx).abs() <= 0.01 && (y - ty).abs() <= 0.01, "frame {f}: ({x}, {y}) vs ({tx}, {ty})");
        }
        // A flat patch has nothing to follow: it is refused at once, not tracked to nowhere.
        let flat = Plane { w, h, px: vec![90u8; w * h] };
        assert!(!Walk::new(region, pose).push(0, &flat, (w as u32, h as u32)), "a flat region was accepted");
    }
}
