//! Shot-change detection on a source file: where the picture cuts, and where it
//! fades through black. Headless; no UI, no export, no project.
//!
//! The pipeline is one decode pass over the file (or the range asked for):
//!
//! 1. Each analysed frame is reduced to a 64x36 luma thumbnail (point samples at
//!    cell centres, so the cost per frame is a few thousand reads, not a full
//!    pass over the picture) and a coarse chroma histogram (U and V each
//!    quantised to 8 levels, 64 bins, normalised to sum to 1).
//! 2. Consecutive analysed frames are scored `0.5 * mad + 0.5 * hist`, where
//!    `mad` is the mean absolute luma difference over 255 and `hist` is the
//!    intersection distance `1 - sum(min(a, b))` between the two chroma
//!    histograms (equal to half their L1 distance). Both terms are in `0..=1`, so
//!    the score is too.
//! 3. A pair is a hard cut when its score is at least the user threshold **and**
//!    at least [`ADAPT`] times the median of the previous [`WINDOW`] scores. The
//!    second test is the adaptive part: in a busy shot a large but ordinary
//!    change (a pan, a flash, a fast cut-in-motion) sits near its neighbours'
//!    median and is not a cut; a spike that stands out from the local activity
//!    is. The median is over raw scores, so one earlier cut does not raise the bar
//!    for the next one.
//! 4. A run of at least [`BLACK_RUN`] consecutive analysed frames whose mean luma
//!    is below [`BLACK_LUMA`] (of 255) is a fade to and from black. It is reported
//!    once, at the first picture frame after the dark run, as a cut with score `1.0` and kind
//!    [`CutKind::FadeBlack`]. Hard cuts at its two edges are the same boundary
//!    seen again and are dropped. A dissolve that never gets dark is not a fade
//!    here; it is scored like any other change.
//! 5. Candidates closer together than the minimum shot length collapse into the
//!    stronger one (a fade beats a hard cut at equal distance).
//!
//! `sample_fps` decimates the analysis, not the decode: a compressed stream has
//! to be decoded frame by frame regardless, so the saving is in the reduction and
//! scoring. With a cached proxy ([`crate::proxy::cached`]) the decode is the
//! cheaper all-intra half-size copy, and the proxy is used when its frame count
//! matches the source's, so its frame numbers are the source's.
//!
//! Positions are the first frame of the new shot. With `sample_fps: None` every
//! frame is analysed, so that is exact. With a decimated analysis each candidate
//! is refined by decoding the frames between its two samples and taking the one
//! whose change is largest (or, for a fade, the first picture frame), so it is
//! exact too, at the cost of a short extra decode per cut.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::decode::{DecodeSession, Frame};
use crate::demux::{Demuxer, VideoMeta};

/// Thumbnail grid the frames are reduced to.
pub const THUMB_W: usize = 64;
pub const THUMB_H: usize = 36;
/// Coarse chroma histogram: 8 levels of U by 8 levels of V.
const BINS: usize = 64;
/// Rolling window of raw scores the adaptive threshold takes its median from.
const WINDOW: usize = 8;
/// A hard cut must stand this many times above the window's median.
const ADAPT: f32 = 2.0;
/// Mean luma (of 255) under which a frame counts as black.
pub const BLACK_LUMA: f32 = 16.0;
/// Consecutive black analysed frames that make a fade.
pub const BLACK_RUN: usize = 3;
/// Files longer than this get [`LONG_FILE_SAMPLE_FPS`] from [`SceneSettings::for_duration`].
pub const LONG_FILE_SECS: f64 = 60.0;
pub const LONG_FILE_SAMPLE_FPS: f64 = 10.0;
pub const DEFAULT_THRESHOLD: f32 = 0.35;
pub const DEFAULT_MIN_SHOT_SECS: f64 = 0.6;
/// A dissolve step is a pair of frames whose luma barely moves (mean absolute difference
/// at most this, over 255) while their colours do (chroma histogram distance at least
/// [`DISSOLVE_HIST_MIN`]). A cut moves the luma a lot; a pan keeps the colours.
const DISSOLVE_MAD_MAX: f32 = 0.05;
const DISSOLVE_HIST_MIN: f32 = 0.03;
/// Fewest consecutive dissolve steps that make a dissolve.
const DISSOLVE_MIN_STEPS: usize = 4;

/// How the detector is tuned. Every field is validated by [`detect`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSettings {
    /// Score a change must reach to be a cut, `0..=1`. Default 0.35.
    pub threshold: f32,
    /// Shortest shot the result may contain, in seconds. Default 0.6.
    pub min_shot_secs: f64,
    /// Analysed frames per second; `None` analyses every frame.
    pub sample_fps: Option<f64>,
}

impl Default for SceneSettings {
    fn default() -> Self {
        Self { threshold: DEFAULT_THRESHOLD, min_shot_secs: DEFAULT_MIN_SHOT_SECS, sample_fps: None }
    }
}

impl SceneSettings {
    /// The defaults for a file of `secs` seconds: every frame for a short file,
    /// [`LONG_FILE_SAMPLE_FPS`] for a long one.
    pub fn for_duration(secs: f64) -> Self {
        let sample_fps = (secs > LONG_FILE_SECS).then_some(LONG_FILE_SAMPLE_FPS);
        Self { sample_fps, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutKind {
    /// A change between two analysed frames.
    Hard,
    /// The first picture frame after a run of near-black frames.
    FadeBlack,
    /// A change spread over a run of frames: a cross-dissolve. The boundary runs from
    /// the last frame of the old shot (`Cut::frame`) to the first of the new one (`end`).
    Dissolve { end: u32 },
}

/// A shot boundary. `frame` is the first source frame of the new shot (for a dissolve,
/// the last frame of the old one; see [`CutKind::Dissolve`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub frame: u32,
    pub score: f32,
    pub kind: CutKind,
}

/// Detects the cuts in `path`, optionally only in `range` (source frames,
/// half-open `[from, to)`; `None` is the whole file).
///
/// A range is analysed from a window of samples before its start, so the first
/// changes inside it are judged against the same context a whole-file run has, and
/// the sample grid is the whole file's. Cuts before `from` are not reported.
///
/// `progress`, when given, is set to the percentage of the range analysed
/// (`0..=100`). `cancel`, when given and set, stops the walk and returns an
/// error. Returns the cuts in frame order.
pub fn detect(
    path: &Path,
    range: Option<(u32, u32)>,
    s: &SceneSettings,
    progress: Option<&AtomicU32>,
    cancel: Option<&AtomicBool>,
) -> crate::Result<Vec<Cut>> {
    validate(s)?;
    let (from, to) = range.unwrap_or((0, u32::MAX));
    let (head, _) = Demuxer::open(path)?;
    let fps = if head.frame_rate.is_finite() && head.frame_rate > 0.0 { head.frame_rate } else { 25.0 };
    let step = s.sample_fps.map_or(1.0, |sf| (fps / sf).max(1.0));
    let pre = (WINDOW as f64 * step).ceil() as u32 + 1;
    let start = from.saturating_sub(pre);
    let (meta, rx, decoder) = open_analysis(path, start, to)?;
    let to = to.min(meta.frame_count);
    if from >= to {
        decoder.store(true, Ordering::Relaxed);
        return Ok(Vec::new());
    }
    let span = f64::from(to - from);
    let min_shot = (s.min_shot_secs * fps).ceil().max(0.0) as u32;

    // The first due sample is the first grid point at or after `start`.
    let mut due = (f64::from(start) / step).ceil() * step;
    let mut prev: Option<(u32, Thumb)> = None;
    let mut window: VecDeque<f32> = VecDeque::with_capacity(WINDOW);
    // (frame, mean luma) of every analysed frame, for the black-run pass.
    let mut means: Vec<(u32, f32)> = Vec::new();
    // Each hard candidate with the analysed frame before it, for refinement.
    let mut hard: Vec<(Cut, u32)> = Vec::new();
    // The dissolve walked so far, and the dissolves found.
    let mut run: Option<DissolveRun> = None;
    let mut dissolves: Vec<Cut> = Vec::new();

    for frame in rx.iter() {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            decoder.store(true, Ordering::Relaxed);
            return Err("scene detection cancelled".into());
        }
        let idx = frame.index;
        if idx >= to {
            break;
        }
        if f64::from(idx) < due {
            continue;
        }
        due += step;
        while due <= f64::from(idx) {
            due += step;
        }
        if let Some(p) = progress {
            let done = (f64::from(idx.saturating_sub(from)) / span * 100.0).clamp(0.0, 100.0);
            p.store(done as u32, Ordering::Relaxed);
        }
        let Some(thumb) = Thumb::of(&frame) else { continue };
        means.push((idx, thumb.mean));
        if let Some((prev_idx, before)) = &prev {
            let raw = score(before, &thumb);
            let median = median(&window);
            if raw >= s.threshold && raw >= ADAPT * median && idx >= from {
                hard.push((Cut { frame: idx, score: raw, kind: CutKind::Hard }, *prev_idx));
            }
            if window.len() == WINDOW {
                window.pop_front();
            }
            window.push_back(raw);
            let (mad, hist) = parts(before, &thumb);
            if is_dissolve_step(mad, hist, before.mean.min(thumb.mean) < BLACK_LUMA) {
                let r = run.get_or_insert_with(|| DissolveRun::start(*prev_idx, before));
                r.push(idx, &thumb);
            } else if let Some(r) = run.take() {
                dissolves.extend(r.close(s.threshold));
            }
        }
        prev = Some((idx, thumb));
    }
    decoder.store(true, Ordering::Relaxed);
    if let Some(p) = progress {
        p.store(100, Ordering::Relaxed);
    }
    if let Some(r) = run.take() {
        dissolves.extend(r.close(s.threshold));
    }

    let fades = black_runs(&means);
    // A hard cut on the edge of a black run is the fade itself, seen again.
    hard.retain(|(c, _)| !fades.iter().any(|f| f.lo <= c.frame && c.frame <= f.hi));
    // A dissolve inside a fade is the fade's own steps: the fade is the boundary.
    let fade_win = fps.round().max(1.0) as u32;
    dissolves.retain(|d| !fades.iter().any(|f| dissolve_in_fade(d, f, fade_win)));
    // The steps of a dissolve are not cuts; the dissolve is the one boundary.
    hard.retain(|(c, _)| !dissolves.iter().any(|d| inside_dissolve(d, c.frame)));
    let mut all: Vec<Cut> = Vec::new();
    for (c, before) in hard {
        // A decimated candidate is the first analysed frame of the new shot; the
        // exact frame is the one whose change from its predecessor is largest.
        let frame = if step > 1.0 { refine_change(path, before, c.frame).unwrap_or(c.frame) } else { c.frame };
        all.push(Cut { frame, ..c });
    }
    for f in &fades {
        // A fade is placed at the first picture frame after its dark run.
        let frame = if step > 1.0 { refine_fade(path, f.lo, f.cut.frame).unwrap_or(f.cut.frame) } else { f.cut.frame };
        all.push(Cut { frame, ..f.cut });
    }
    all.extend(dissolves);
    all.retain(|c| c.frame >= from && c.frame < to);
    all.sort_by_key(|c| c.frame);
    Ok(collapse(all, min_shot))
}

/// The decoded thumbnails of frames `lo..=hi` of the source, in order.
fn thumbs(path: &Path, lo: u32, hi: u32) -> Option<Vec<(u32, Thumb)>> {
    let (_, rx, cancel) = DecodeSession::open_range(path, lo, hi.saturating_add(1)).ok()?;
    let mut out = Vec::new();
    for frame in rx.iter() {
        if frame.index > hi {
            break;
        }
        if let Some(t) = Thumb::of(&frame) {
            out.push((frame.index, t));
        }
    }
    cancel.store(true, Ordering::Relaxed);
    Some(out)
}

/// The frame in `(lo, hi]` where the picture changes most from the frame before it.
fn refine_change(path: &Path, lo: u32, hi: u32) -> Option<u32> {
    let frames = thumbs(path, lo, hi)?;
    let mut best: Option<(f32, u32)> = None;
    for pair in frames.windows(2) {
        let sc = score(&pair[0].1, &pair[1].1);
        if best.is_none_or(|(b, _)| sc > b) {
            best = Some((sc, pair[1].0));
        }
    }
    best.map(|(_, f)| f)
}

/// The first frame in `(lo, hi]` that is out of the dark run.
fn refine_fade(path: &Path, lo: u32, hi: u32) -> Option<u32> {
    thumbs(path, lo, hi)?
        .into_iter()
        .find(|(f, t)| *f > lo && t.mean >= BLACK_LUMA)
        .map(|(f, _)| f)
}

fn validate(s: &SceneSettings) -> crate::Result<()> {
    if !(s.threshold.is_finite() && (0.0..=1.0).contains(&s.threshold)) {
        return Err(format!("threshold must be between 0 and 1, not {}", s.threshold).into());
    }
    if !(s.min_shot_secs.is_finite() && s.min_shot_secs >= 0.0) {
        return Err(format!("min_shot_secs must be 0 or more, not {}", s.min_shot_secs).into());
    }
    if let Some(sf) = s.sample_fps && !(sf.is_finite() && sf > 0.0) {
        return Err(format!("sample_fps must be above 0, not {sf}").into());
    }
    Ok(())
}

/// Opens the frames to analyse: the proxy when one is cached and has the source's
/// frame count, the source otherwise. Returns the **source's** metadata either
/// way, and the flag that stops whichever worker is not used.
fn open_analysis(
    path: &Path,
    from: u32,
    to: u32,
) -> crate::Result<(VideoMeta, Receiver<Frame>, Arc<AtomicBool>)> {
    let (meta, rx, cancel) = DecodeSession::open_range(path, from, to)?;
    if let Some(proxy) = crate::proxy::cached(path)
        && let Ok((pmeta, prx, pcancel)) = DecodeSession::open_range(&proxy, from, to)
    {
        if pmeta.frame_count == meta.frame_count {
            cancel.store(true, Ordering::Relaxed);
            return Ok((meta, prx, pcancel));
        }
        pcancel.store(true, Ordering::Relaxed);
    }
    Ok((meta, rx, cancel))
}

/// One analysed frame, reduced.
#[derive(Clone)]
struct Thumb {
    /// `THUMB_W * THUMB_H` luma samples, row-major.
    luma: Vec<u8>,
    /// Normalised U/V histogram.
    hist: [f32; BINS],
    /// Mean of `luma` as a 0..=255 value.
    mean: f32,
}

impl Thumb {
    fn of(f: &Frame) -> Option<Thumb> {
        let (w, h) = (f.width as usize, f.height as usize);
        if w == 0 || h == 0 || f.bgra.len() < w * h * 4 {
            return None;
        }
        let mut luma = Vec::with_capacity(THUMB_W * THUMB_H);
        let mut counts = [0u32; BINS];
        let mut sum = 0u64;
        for ty in 0..THUMB_H {
            let sy = (ty * 2 + 1) * h / (2 * THUMB_H);
            for tx in 0..THUMB_W {
                let sx = (tx * 2 + 1) * w / (2 * THUMB_W);
                let p = (sy * w + sx) * 4;
                // BGRA in memory.
                let b = i32::from(f.bgra[p]);
                let g = i32::from(f.bgra[p + 1]);
                let r = i32::from(f.bgra[p + 2]);
                let y = ((77 * r + 150 * g + 29 * b) >> 8).clamp(0, 255);
                let u = (((-43 * r - 85 * g + 128 * b) >> 8) + 128).clamp(0, 255);
                let v = (((128 * r - 107 * g - 21 * b) >> 8) + 128).clamp(0, 255);
                counts[((u >> 5) * 8 + (v >> 5)) as usize] += 1;
                luma.push(y as u8);
                sum += y as u64;
            }
        }
        let n = (THUMB_W * THUMB_H) as f32;
        let mut hist = [0f32; BINS];
        for (dst, c) in hist.iter_mut().zip(counts) {
            *dst = c as f32 / n;
        }
        Some(Thumb { luma, hist, mean: sum as f32 / n })
    }
}

/// Blend of mean absolute luma difference and chroma histogram intersection
/// distance, in `0..=1`.
fn score(a: &Thumb, b: &Thumb) -> f32 {
    let (mad, hist) = parts(a, b);
    (0.5 * mad + 0.5 * hist).clamp(0.0, 1.0)
}

/// The two terms of [`score`]: mean absolute luma difference over 255, and the chroma
/// histogram distance (both `0..=1`).
fn parts(a: &Thumb, b: &Thumb) -> (f32, f32) {
    let total: u64 = a
        .luma
        .iter()
        .zip(&b.luma)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    let mad = total as f32 / (a.luma.len() as f32 * 255.0);
    let hist: f32 = 1.0
        - a.hist
            .iter()
            .zip(&b.hist)
            .map(|(x, y)| x.min(*y))
            .sum::<f32>();
    (mad, hist.max(0.0))
}

/// Whether a pair of frames is one step of a dissolve: the luma barely moves while the
/// colours do. A hard cut moves the luma a lot; a pan keeps the colours; a fade through black
/// is dark, and is left to the black-run pass.
fn is_dissolve_step(mad: f32, hist: f32, dark: bool) -> bool {
    !dark && mad > 0.0 && mad <= DISSOLVE_MAD_MAX && hist >= DISSOLVE_HIST_MIN
}

/// A run of consecutive dissolve steps being walked: the frame and thumbnail before its
/// first step, and the frame and thumbnail after its last one.
struct DissolveRun {
    anchor: u32,
    anchor_thumb: Thumb,
    last: u32,
    last_thumb: Thumb,
    steps: usize,
}

impl DissolveRun {
    fn start(anchor: u32, before: &Thumb) -> Self {
        Self { anchor, anchor_thumb: before.clone(), last: anchor, last_thumb: before.clone(), steps: 0 }
    }

    fn push(&mut self, idx: u32, thumb: &Thumb) {
        self.last = idx;
        self.last_thumb = thumb.clone();
        self.steps += 1;
    }

    /// The dissolve this run makes, if it has enough steps and its two ends differ by at least
    /// the threshold: one boundary from the anchor to the last frame.
    fn close(self, threshold: f32) -> Option<Cut> {
        if self.steps < DISSOLVE_MIN_STEPS {
            return None;
        }
        let raw = score(&self.anchor_thumb, &self.last_thumb);
        (raw >= threshold).then_some(Cut { frame: self.anchor, score: raw, kind: CutKind::Dissolve { end: self.last } })
    }
}

/// Whether a dissolve boundary reaches into a fade's window: from its dark run to `win` frames
/// after its first picture frame.
fn dissolve_in_fade(d: &Cut, f: &Fade, win: u32) -> bool {
    d.frame <= f.cut.frame + win && dissolve_end(d) >= f.lo
}

/// Whether `frame` is one of the steps inside a dissolve (after its first frame, up to its end).
fn inside_dissolve(d: &Cut, frame: u32) -> bool {
    frame > d.frame && frame <= dissolve_end(d)
}

/// The last frame of a dissolve, or its frame for any other kind of cut.
fn dissolve_end(c: &Cut) -> u32 {
    match c.kind {
        CutKind::Dissolve { end } => end,
        _ => c.frame,
    }
}

/// Median of the window, or 0 when it is empty (the first cut of a file is
/// judged on the threshold alone).
fn median(window: &VecDeque<f32>) -> f32 {
    if window.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = window.iter().copied().collect();
    v.sort_by(|a, b| a.total_cmp(b));
    let mid = v.len() / 2;
    if v.len().is_multiple_of(2) { (v[mid - 1] + v[mid]) / 2.0 } else { v[mid] }
}

/// A fade to black, with the analysed frames on either side of it.
struct Fade {
    cut: Cut,
    /// Frame of the analysed sample before the dark run (or the first dark one).
    lo: u32,
    /// Frame of the analysed sample after the dark run (or the last dark one).
    hi: u32,
}

/// Runs of at least [`BLACK_RUN`] consecutive dark analysed frames.
fn black_runs(means: &[(u32, f32)]) -> Vec<Fade> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < means.len() {
        if means[i].1 >= BLACK_LUMA {
            i += 1;
            continue;
        }
        let start = i;
        while i < means.len() && means[i].1 < BLACK_LUMA {
            i += 1;
        }
        let end = i; // exclusive
        if end - start >= BLACK_RUN {
            let lo = means[start.saturating_sub(1)].0;
            let hi = means[end.min(means.len() - 1)].0;
            out.push(Fade {
                cut: Cut { frame: hi, score: 1.0, kind: CutKind::FadeBlack },
                lo,
                hi,
            });
        }
    }
    out
}

/// Keeps the stronger of any two cuts closer than `min_shot` frames.
fn collapse(sorted: Vec<Cut>, min_shot: u32) -> Vec<Cut> {
    let mut out: Vec<Cut> = Vec::with_capacity(sorted.len());
    for c in sorted {
        if let Some(last) = out.last_mut()
            && c.frame.saturating_sub(last.frame) < min_shot
        {
                // A fade is the boundary itself; it beats a hard cut beside it.
                let fade_wins = c.kind == CutKind::FadeBlack && last.kind == CutKind::Hard;
                if c.score > last.score || fade_wins {
                    *last = c;
                }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::scratch::Scratch;
    use std::process::Command;
    use std::time::Instant;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// Three different generated sources back to back, re-encoded as one file.
    /// Returns the file (which owns its directory, removed on drop) and the
    /// frame numbers of the two joins.
    fn three_shots(name: &str, seconds: u32) -> (Scratch, [u32; 2]) {
        let joined = Scratch::file(name, "mp4");
        let dir = joined.parent().unwrap().to_path_buf();
        let parts = [
            ("testsrc2", dir.join("a.mp4")),
            ("mandelbrot", dir.join("b.mp4")),
            ("smptebars", dir.join("c.mp4")),
        ];
        for (src, out) in &parts {
            let lavfi = format!("{src}=size=640x360:rate=25");
            let ok = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", &lavfi, "-t", &seconds.to_string()])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
                .arg(out)
                .status()
                .expect("ffmpeg runs")
                .success();
            assert!(ok, "ffmpeg made {src}");
        }
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-i"]).arg(&parts[0].1)
            .args(["-i"]).arg(&parts[1].1)
            .args(["-i"]).arg(&parts[2].1)
            .args(["-filter_complex", "[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]", "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&*joined)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok, "ffmpeg joined the shots");
        let per = seconds * 25;
        (joined, [per, 2 * per])
    }

    fn near(cuts: &[Cut], want: u32, slack: u32) -> bool {
        cuts.iter().any(|c| c.frame.abs_diff(want) <= slack && c.kind == CutKind::Hard)
    }

    #[test]
    fn finds_the_joins_of_three_sources() {
        let (path, joins) = three_shots("joins", 2);
        let cuts = detect(&path, None, &SceneSettings::default(), None, None).unwrap();
        for j in joins {
            assert!(near(&cuts, j, 2), "no cut within 2 frames of {j}: {cuts:?}");
        }
        assert_eq!(cuts.len(), 2, "exactly the two joins: {cuts:?}");
    }

    #[test]
    fn a_range_only_sees_the_cuts_inside_it() {
        let (path, joins) = three_shots("range", 2);
        let cuts = detect(&path, Some((joins[0] - 10, joins[0] + 30)), &SceneSettings::default(), None, None).unwrap();
        assert!(near(&cuts, joins[0], 2), "{cuts:?}");
        assert!(cuts.iter().all(|c| c.frame < joins[1]), "{cuts:?}");
    }

    /// A range starting just after a cut sees the same cut as the whole file, at the
    /// same frame, and nothing the whole file does not see: its first changes are
    /// judged against the window before the range, not an empty one.
    #[test]
    fn a_range_agrees_with_the_whole_file() {
        let (path, joins) = three_shots("agree", 2);
        let whole = detect(&path, None, &SceneSettings::default(), None, None).unwrap();
        for (from, to) in [(joins[0] - 3, joins[0] + 20), (joins[0] + 1, joins[1] + 5), (5, 3 * 50)] {
            let part = detect(&path, Some((from, to)), &SceneSettings::default(), None, None).unwrap();
            let want: Vec<u32> = whole.iter().map(|c| c.frame).filter(|&f| f >= from && f < to).collect();
            let got: Vec<u32> = part.iter().map(|c| c.frame).collect();
            assert_eq!(got, want, "range {from}..{to}");
        }
    }

    #[test]
    fn a_decimated_run_lands_on_the_first_frame_of_the_new_shot() {
        let (path, joins) = three_shots("decimated", 2);
        let s = SceneSettings { sample_fps: Some(4.0), ..SceneSettings::default() };
        let cuts = detect(&path, None, &s, None, None).unwrap();
        assert_eq!(cuts.iter().map(|c| c.frame).collect::<Vec<_>>(), joins.to_vec(), "{cuts:?}");
    }

    #[test]
    fn a_continuous_shot_has_at_most_one_cut() {
        let cuts = detect(&asset("test_av.mp4"), None, &SceneSettings::default(), None, None).unwrap();
        assert!(cuts.len() <= 1, "a single shot should not cut: {cuts:?}");
    }

    #[test]
    fn a_fade_through_black_is_one_cut_at_the_first_picture_frame() {
        let path = Scratch::file("fade", "mp4");
        // Two seconds of picture, a second of black between them, then picture.
        let filter = "[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]";
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-t", "2", "-i", "testsrc2=size=320x180:rate=25"])
            .args(["-f", "lavfi", "-t", "1", "-i", "color=c=black:size=320x180:rate=25"])
            .args(["-f", "lavfi", "-t", "2", "-i", "mandelbrot=size=320x180:rate=25"])
            .args(["-filter_complex", filter, "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        let cuts = detect(&path, None, &SceneSettings::default(), None, None).unwrap();
        let fades: Vec<_> = cuts.iter().filter(|c| c.kind == CutKind::FadeBlack).collect();
        assert_eq!(fades.len(), 1, "{cuts:?}");
        // The black second is frames 50..75; the picture is back at frame 75.
        assert!(fades[0].frame.abs_diff(75) <= 1, "{cuts:?}");
        assert_eq!(fades[0].score, 1.0);
    }

    /// A flat frame: every luma sample at `level`, and its chroma histogram all in one bin.
    fn flat(level: f32, bin: usize) -> Thumb {
        Thumb { luma: vec![level as u8; THUMB_W * THUMB_H], hist: one_bin(bin, 1.0), mean: level }
    }

    /// A chroma histogram with `w` of its weight in `bin` and the rest in `other`.
    fn one_bin(bin: usize, w: f32) -> [f32; BINS] {
        let mut hist = [0f32; BINS];
        hist[bin] = w;
        hist[(bin + 1) % BINS] += 1.0 - w;
        hist
    }

    /// The dissolve walk of `detect` over a synthetic series of frames: the same steps, run
    /// and close, with the threshold as given.
    fn walk(series: &[Thumb], threshold: f32) -> Vec<Cut> {
        let mut out = Vec::new();
        let mut run: Option<DissolveRun> = None;
        for i in 1..series.len() {
            let (mad, hist) = parts(&series[i - 1], &series[i]);
            let dark = series[i - 1].mean.min(series[i].mean) < BLACK_LUMA;
            if is_dissolve_step(mad, hist, dark) {
                run.get_or_insert_with(|| DissolveRun::start(i as u32 - 1, &series[i - 1])).push(i as u32, &series[i]);
            } else if let Some(r) = run.take() {
                out.extend(r.close(threshold));
            }
        }
        if let Some(r) = run.take() {
            out.extend(r.close(threshold));
        }
        out
    }

    /// Shot A (level 130, chroma bin 5) for 20 frames, then `n` dissolve frames: each one a
    /// level lower and a little more of the chroma in bin 6, then shot B, which has the level and
    /// chroma the last dissolve frame has, so the last step is a colour change with no luma change.
    fn ramp_series(n: usize) -> Vec<Thumb> {
        let mut s: Vec<Thumb> = (0..20).map(|_| flat(130.0, 5)).collect();
        for k in 1..=n {
            let level = 130.0 - k as f32;
            let w = 1.0 - k as f32 / (n + 1) as f32;
            s.push(Thumb { luma: vec![level as u8; THUMB_W * THUMB_H], hist: one_bin(5, w), mean: level });
        }
        let last = 130.0 - n as f32;
        s.extend((0..20).map(|_| Thumb { luma: vec![last as u8; THUMB_W * THUMB_H], hist: one_bin(6, 1.0), mean: last }));
        s
    }

    #[test]
    fn a_dissolve_is_one_boundary_from_its_first_frame_to_its_last() {
        let cuts = walk(&ramp_series(20), 0.35);
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert_eq!(cuts[0].frame, 19, "{cuts:?}");
        assert_eq!(cuts[0].kind, CutKind::Dissolve { end: 39 }, "{cuts:?}");
    }

    /// The same change read at a threshold the ends do not reach is not a dissolve.
    #[test]
    fn a_dissolve_below_the_threshold_is_not_reported() {
        assert!(walk(&ramp_series(20), 0.9).is_empty());
    }

    /// Three steps are too few to be a dissolve; four are the least that are.
    #[test]
    fn a_dissolve_needs_four_steps() {
        assert!(walk(&ramp_series(3), 0.35).is_empty());
        assert_eq!(walk(&ramp_series(4), 0.35).len(), 1);
    }

    /// The luma moves steadily, the colours do not: a pan, not a dissolve.
    #[test]
    fn a_pan_is_not_a_dissolve() {
        let s: Vec<Thumb> = (0..40).map(|i| flat(130.0 - 2.0 * i as f32, 5)).collect();
        assert!(walk(&s, 0.05).is_empty());
    }

    /// A hard cut moves the luma a lot in one step: it is not a run of dissolve steps.
    #[test]
    fn a_hard_cut_is_not_a_dissolve() {
        let mut s: Vec<Thumb> = (0..20).map(|_| flat(130.0, 5)).collect();
        s.extend((0..20).map(|_| flat(40.0, 9)));
        assert!(walk(&s, 0.05).is_empty());
    }

    #[test]
    fn dissolve_steps_are_the_moving_pairs_that_are_not_dark() {
        // mad 0.0118 (luma 60 over 20 steps), hist 0.05: a step. Dark on either side: not.
        assert!(is_dissolve_step(0.0118, 0.05, false));
        assert!(!is_dissolve_step(0.0118, 0.05, true));
        // A cut (mad 0.4, hist 0.75), a pan (no colour change), and a still frame.
        assert!(!is_dissolve_step(0.4, 0.75, false));
        assert!(!is_dissolve_step(0.04, 0.0, false));
        assert!(!is_dissolve_step(0.0, 0.0, false));
    }

    /// A dissolve is one boundary: its own steps are not cuts, the frame after it is.
    #[test]
    fn the_steps_of_a_dissolve_are_inside_it() {
        let d = Cut { frame: 19, score: 0.6, kind: CutKind::Dissolve { end: 40 } };
        assert!(!inside_dissolve(&d, 19));
        assert!(inside_dissolve(&d, 20));
        assert!(inside_dissolve(&d, 40));
        assert!(!inside_dissolve(&d, 41));
    }

    /// A real two-second xfade dissolve (frames 25 to 50 at 25 fps) is one dissolve boundary at
    /// the default threshold and at 0.1, where its steps used to read as four cuts.
    #[test]
    fn a_real_dissolve_is_one_boundary_at_any_threshold() {
        let path = Scratch::file("xfade", "mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-t", "2", "-i", "testsrc2=size=320x180:rate=25"])
            .args(["-f", "lavfi", "-t", "2", "-i", "mandelbrot=size=320x180:rate=25"])
            .args(["-filter_complex", "[0:v][1:v]xfade=transition=fade:duration=1:offset=1[v]", "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        for threshold in [DEFAULT_THRESHOLD, 0.1] {
            let s = SceneSettings { threshold, ..SceneSettings::default() };
            let cuts = detect(&path, None, &s, None, None).unwrap();
            let dissolves: Vec<&Cut> = cuts.iter().filter(|c| matches!(c.kind, CutKind::Dissolve { .. })).collect();
            assert_eq!(dissolves.len(), 1, "threshold {threshold}: {cuts:?}");
            let d = dissolves[0];
            let CutKind::Dissolve { end } = d.kind else { unreachable!() };
            assert!(d.frame.abs_diff(25) <= 2 && end.abs_diff(50) <= 2, "threshold {threshold}: {cuts:?}");
            assert!(cuts.iter().all(|c| !inside_dissolve(d, c.frame)), "threshold {threshold}: {cuts:?}");
        }
    }

    #[test]
    fn settings_out_of_range_are_refused() {
        let s = SceneSettings { threshold: 1.5, ..SceneSettings::default() };
        assert!(detect(&asset("test_av.mp4"), None, &s, None, None).is_err());
        let s = SceneSettings { sample_fps: Some(0.0), ..SceneSettings::default() };
        assert!(detect(&asset("test_av.mp4"), None, &s, None, None).is_err());
    }

    #[test]
    fn cancel_stops_the_walk() {
        let (path, _) = three_shots("cancel", 2);
        let cancel = AtomicBool::new(true);
        assert!(detect(&path, None, &SceneSettings::default(), None, Some(&cancel)).is_err());
    }

    /// Speed on a 5 s 720p file, in frames per second. Run with
    /// `cargo test --release -p engine --lib scenes::tests::speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn speed() {
        let path = Scratch::file("speed", "mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25:duration=5"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        for sample in [None, Some(LONG_FILE_SAMPLE_FPS)] {
            let s = SceneSettings { sample_fps: sample, ..SceneSettings::default() };
            let t = Instant::now();
            let cuts = detect(&path, None, &s, None, None).unwrap();
            let secs = t.elapsed().as_secs_f64();
            println!("sample {sample:?}: 125 frames in {secs:.3}s = {:.0} frames/s, {} cuts", 125.0 / secs, cuts.len());
        }
    }
}
