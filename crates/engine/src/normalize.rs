//! Loudness measurement and normalization of the timeline: the core `normalize`
//! (MCP) and the mix card's Normalize (GUI) share.
//!
//! A lane is measured the way the engine plays it (clip speeds, equalizers,
//! fades, gaps and its ducks included, lane gain not applied -- the caller adds
//! it), the mix by rendering it the way an export writes it. [`normalize`] moves
//! lane gain, with the part of a boost past the lane cap going on the lane's
//! clips as clip gain, and measures after each change until the level lands.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::duck::Ducker;
use crate::export::{ExportSettings, Format};
use crate::loudness::{self, Loudness, Meter};
use crate::project::{Lane, LaneKind, MAX_CLIP_GAIN_DB, MAX_GAIN_DB, MIN_GAIN_DB};
use crate::silence::WINDOWS_PER_SEC;
use crate::{AudioChunk, AudioMeta, AudioSession, Clip, PlaybackSession};

/// The refusal for reading `what` (a target's name) while a source it plays is offline: named,
/// with its path and the fix, the way the mix refuses. `None` when every source it reads is on disk.
pub fn offline_refusal_for(s: &PlaybackSession, what: impl std::fmt::Display, sources: &[usize]) -> Option<String> {
    let offline: Vec<String> = s
        .offline_sources()
        .into_iter()
        .filter(|(i, _)| sources.contains(i))
        .map(|(i, p)| format!("src:{i} {}", p.display()))
        .collect();
    if offline.is_empty() {
        return None;
    }
    Some(format!(
        "reading {what} refused: {} source(s) offline (moved or deleted): {}; media_relink each one, or project_open with search",
        offline.len(),
        offline.join(", ")
    ))
}

/// The reason a mix cannot be rendered while sources are offline: named, with the fix, the way
/// `export` refuses. `None` when every source is on disk.
pub fn offline_refusal(s: &PlaybackSession) -> Option<String> {
    let offline = s.offline_sources();
    if offline.is_empty() {
        return None;
    }
    let names: Vec<String> = offline.iter().map(|(i, p)| format!("src:{i} {}", p.display())).collect();
    Some(format!(
        "rendering the mix refused: {} source(s) offline (moved or deleted): {}; media_relink each one, or project_open with search",
        offline.len(),
        names.join(", ")
    ))
}

/// `l` moved by `db`, as the engine plays it with a lane gain. Every finite
/// level shifts by the same amount; the loudness range is a difference and stays.
pub fn shifted(l: Loudness, db: f64) -> Loudness {
    let up = |x: f64| if x.is_finite() { x + db } else { x };
    Loudness {
        integrated_lufs: up(l.integrated_lufs),
        range_lu: l.range_lu,
        true_peak_dbtp: up(l.true_peak_dbtp),
        sample_peak_dbfs: up(l.sample_peak_dbfs),
        momentary_max_lufs: up(l.momentary_max_lufs),
        short_term_max_lufs: up(l.short_term_max_lufs),
        analysed_secs: l.analysed_secs,
        lra_samples: l.lra_samples,
    }
}

/// The sound a lane makes from timeline frame `from`, as the engine plays it:
/// clip speeds, equalizers, fades and gaps included. Lane gain is not applied.
/// `None` when the file set has no audio.
pub fn open_lane(
    s: &PlaybackSession,
    lane: Lane,
    from: u32,
    fps: f64,
) -> Result<Option<(AudioMeta, Receiver<AudioChunk>)>, String> {
    let used: Vec<usize> = s.lane_clips(lane).iter().map(|c| c.source).collect();
    if let Some(why) = offline_refusal_for(s, lane.label(), &used) {
        return Err(why);
    }
    let (project, _) = s.export_snapshot();
    let segs = project.lane_segments_from(lane, from, fps);
    let speeds = project.lane_speeds_from(lane, from, fps);
    let eqs = project.lane_eqs_from(lane, from, fps);
    // The fade envelope carries each clip's own gain as well as its fades, the
    // same envelope playback and export are handed.
    let fades = project.lane_fades_from(lane, from, fps);
    let sources: Vec<(PathBuf, usize)> = project
        .sources()
        .iter()
        .map(|src| (src.path.clone(), src.audio_stream))
        .collect();
    AudioSession::open_multi_streams_speed_at_fade(
        &sources, &segs, &eqs, &speeds, &fades, fps, None,
    )
    .map_err(|e| format!("reading {}: {e}", lane.label()))
}


/// The lane under a duck, read the way the mixer plays it: the key lane's sound
/// after its gain keys the duck, and the duck reduces this lane's own sound.
/// Read from timeline frame 0, so the detector has the same history it has in
/// the mix. Gain is not applied to the lane, only to the keys. A lane can have
/// several ducks; each one is applied in turn, as the mixer applies them.
pub struct Ducked {
    pub rate: u32,
    pub channels: usize,
    /// Frames still to read, up to the end of the span.
    left: u64,
    /// The first frame of the next block.
    at: u64,
    /// One per duck on the lane, in the order the lane holds them.
    keys: Vec<KeyDuck>,
    /// The lane's own sound; `None` when only the duck's gain is wanted, in
    /// which case each block is that gain (1.0 at rest) and the lane is unread.
    lane: Option<Feed>,
}

/// One duck on a lane: its key's sound, the key's lane gain and the detector.
struct KeyDuck {
    key: Feed,
    /// The key's lane gain as a factor, not dB.
    key_gain: f32,
    ducker: Ducker,
}

/// One stream's chunks, drained a block at a time.
struct Feed {
    rx: Receiver<AudioChunk>,
    pending: Vec<f32>,
    live: bool,
}

impl Feed {
    /// Waits for a chunk unless one is pending already or the stream has ended.
    fn fill(&mut self) {
        while self.live && self.pending.is_empty() {
            match self.rx.recv() {
                Ok(chunk) => self.pending = chunk.samples,
                Err(_) => self.live = false,
            }
        }
    }
}

impl Ducked {
    /// The ducks on `lane`, read up to timeline frame `to`. `None` when there is
    /// nothing to duck: no duck, no duck whose key lane has clips, or no sound
    /// to read.
    pub fn open(
        s: &PlaybackSession,
        lane: Lane,
        to: u32,
        fps: f64,
        gain_only: bool,
    ) -> Result<Option<Self>, String> {
        let mut keys = Vec::new();
        let mut first: Option<(u32, usize)> = None;
        for duck in s.lane_duck_list(lane) {
            let key_lane = Lane::new(LaneKind::Audio, duck.under);
            if s.lane_clips(key_lane).is_empty() {
                continue;
            }
            let Some((meta, key_rx)) = open_lane(s, key_lane, 0, fps)? else {
                continue;
            };
            first.get_or_insert((meta.sample_rate, usize::from(meta.channels).max(1)));
            keys.push(KeyDuck {
                key: Feed {
                    rx: key_rx,
                    pending: Vec::new(),
                    live: true,
                },
                key_gain: 10f32.powf(s.lane_gain_db(key_lane) / 20.0),
                ducker: Ducker::new(&duck, meta.sample_rate),
            });
        }
        let Some((rate, channels)) = first else {
            return Ok(None);
        };
        let lane_feed = if gain_only {
            None
        } else {
            match open_lane(s, lane, 0, fps)? {
                Some((_, rx)) => Some(Feed {
                    rx,
                    pending: Vec::new(),
                    live: true,
                }),
                None => return Ok(None),
            }
        };
        Ok(Some(Self {
            rate,
            channels,
            left: (f64::from(to) / fps * f64::from(rate)).round() as u64,
            at: 0,
            keys,
            lane: lane_feed,
        }))
    }

    /// The next block of ducked samples (interleaved) and the frame it starts
    /// at, or `None` once the span or the lane's sound is done. Blocks follow
    /// the mixer's: the frames every live stream has pending, no more.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<(u64, Vec<f32>)> {
        let ch = self.channels;
        for k in self.keys.iter_mut() {
            k.key.fill();
        }
        if let Some(lane) = self.lane.as_mut() {
            lane.fill();
            if !lane.live && lane.pending.is_empty() {
                return None;
            }
        }
        let frames = self
            .keys
            .iter()
            .map(|k| &k.key)
            .chain(self.lane.as_ref())
            .filter(|f| !f.pending.is_empty())
            .map(|f| f.pending.len() / ch)
            .min()?;
        let frames = frames.min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if frames == 0 {
            return None;
        }
        self.left -= frames as u64;
        let n = frames * ch;
        let mut block: Vec<f32> = match self.lane.as_mut() {
            Some(feed) if feed.pending.is_empty() => vec![0.0; n],
            Some(feed) => feed.pending.drain(..n).collect(),
            None => vec![1.0; n],
        };
        for k in self.keys.iter_mut() {
            let gain = k.key_gain;
            let key: Vec<f32> = if k.key.pending.is_empty() {
                vec![0.0; n]
            } else {
                k.key.pending.drain(..n).map(|x| x * gain).collect()
            };
            k.ducker.process(&key, &mut block, ch);
        }
        let at = self.at;
        self.at += frames as u64;
        Some((at, block))
    }
}

/// The sound a lane makes over timeline frames `from..to`, as the engine plays
/// it: clip speeds, equalizers and gaps included, and its duck if it has one.
/// Lane gain is not applied; the caller adds it. `None` when there is no sound.
pub fn measure_lane(
    s: &PlaybackSession,
    lane: Lane,
    from: u32,
    to: u32,
    fps: f64,
) -> Result<Option<Loudness>, String> {
    if let Some(mut ducked) = Ducked::open(s, lane, to, fps, false)? {
        let from_at = (f64::from(from) / fps * f64::from(ducked.rate)).round() as u64;
        let ch = ducked.channels;
        let mut meter = Meter::new(ducked.rate, ducked.channels as u16);
        while let Some((at, block)) = ducked.next() {
            let skip = from_at.saturating_sub(at).min((block.len() / ch) as u64);
            meter.push(&block[skip as usize * ch..]);
        }
        return Ok(Some(meter.finish()));
    }
    let Some((meta, rx)) = open_lane(s, lane, from, fps)? else {
        return Ok(None);
    };
    let channels = usize::from(meta.channels).max(1);
    // The span in interleaved samples; the engine plays past it, so stop there.
    let mut left = (to > from).then(|| {
        let frames = f64::from(to - from) / fps * f64::from(meta.sample_rate);
        frames.round() as usize * channels
    });
    let mut meter = Meter::new(meta.sample_rate, meta.channels);
    for chunk in rx {
        let mut part: &[f32] = &chunk.samples;
        if let Some(n) = left.as_mut() {
            let take = (*n).min(part.len());
            part = &part[..take];
            *n -= take;
        }
        meter.push(part);
        if left == Some(0) {
            break;
        }
    }
    Ok(Some(meter.finish()))
}

/// The dB a lane's duck takes off each 20 ms window of timeline time, from time
/// 0 to frame `to`, indexed by window: the power mean of the duck's gain over
/// the window. Empty when the lane has no duck that does anything.
pub fn duck_offsets(s: &PlaybackSession, lane: Lane, to: u32, fps: f64) -> Result<Vec<f32>, String> {
    let Some(mut ducked) = Ducked::open(s, lane, to, fps, true)? else {
        return Ok(Vec::new());
    };
    let w = u64::from(WINDOWS_PER_SEC);
    let rate = u64::from(ducked.rate);
    let windows = (f64::from(to) / fps * f64::from(WINDOWS_PER_SEC)).ceil() as usize + 1;
    // Power sum and count per window, over channel 0 (the gain is the same in
    // every channel).
    let mut power = vec![(0f64, 0u64); windows];
    while let Some((at, gains)) = ducked.next() {
        for (i, g) in gains.iter().step_by(ducked.channels).enumerate() {
            let k = ((at + i as u64) * w / rate) as usize;
            if let Some(slot) = power.get_mut(k) {
                slot.0 += f64::from(*g) * f64::from(*g);
                slot.1 += 1;
            }
        }
    }
    Ok(power
        .iter()
        .map(|&(sum, n)| {
            if n == 0 {
                0.0
            } else {
                (10.0 * (sum / n as f64).max(1e-12).log10()) as f32
            }
        })
        .collect())
}

/// Deletes its file when dropped, so a failed render leaves nothing behind.
pub struct TempWav(pub PathBuf);

impl Drop for TempWav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The mix (see `render_mix`) measured over timeline frames `from..to`. `end` is the
/// timeline length, so a span that covers all of it is measured without a cut.
pub fn measure_mix(
    s: &PlaybackSession,
    from: u32,
    to: u32,
    fps: f64,
    end: u32,
) -> Result<Loudness, String> {
    let wav = render_mix(s)?;
    let range = (from > 0 || to < end).then(|| {
        let end_s = if to >= end {
            f64::INFINITY
        } else {
            f64::from(to) / fps
        };
        (f64::from(from) / fps, end_s)
    });
    loudness::measure_file(&wav.0, 0, range)
        .map_err(|e| format!("measuring the mix: {e}"))?
        .ok_or_else(|| "the mix has no sound track".to_string())
}

pub fn clamp_gain(db: f64) -> f64 {
    db.clamp(f64::from(MIN_GAIN_DB), f64::from(MAX_GAIN_DB))
}

/// A lane `normalize` sets, with the gains it held before: its lane gain, and each clip's
/// (index, clip gain) for the part of a boost the lane cap leaves, which goes on the clips.
pub struct Plan {
    pub lane: Lane,
    pub old: f64,
    pub clips: Vec<(usize, f64)>,
}

/// A lane as it stands before a normalize changes anything.
pub fn plan_of(s: &PlaybackSession, lane: Lane) -> Plan {
    let clips = (0..s.lane_clips(lane).len())
        .map(|i| (i, f64::from(s.clip_gain_of(lane, i).unwrap_or(0.0))))
        .collect();
    Plan { lane, old: f64::from(s.lane_gain_db(lane)), clips }
}

/// Sets each plan's lane to its old gain plus `gain`, clamped to the lane range. The part of a boost
/// past the lane's +12 dB cap goes on the lane's clips instead: each clip's gain is its old one plus
/// that rest, clamped to the clip range, so a clip that already has gain keeps it. Returns the lanes
/// that could not take all of it, as (name, wanted, got): `wanted` is the lane gain asked for and
/// `got` the lane gain reached with the clips' best take added.
pub fn set_plans(s: &mut PlaybackSession, plans: &[Plan], gain: f64) -> Vec<(String, f64, f64)> {
    let mut clamped = Vec::new();
    for p in plans {
        let wanted = p.old + gain;
        let lane_db = clamp_gain(wanted);
        let rest = (wanted - f64::from(MAX_GAIN_DB)).max(0.0);
        s.set_lane_gain_db(p.lane, lane_db as f32);
        // The gain each clip took on top of its old one.
        let mut took = Vec::with_capacity(p.clips.len());
        for &(i, old) in &p.clips {
            let to = (old + rest).min(f64::from(MAX_CLIP_GAIN_DB));
            s.set_clip_gain(p.lane, i, to as f32);
            took.push(to - old);
        }
        if wanted > f64::from(MAX_GAIN_DB) {
            // The lane is held only when no clip can take the whole rest.
            let best = took.iter().copied().fold(0.0_f64, f64::max);
            if rest - best > 0.05 {
                clamped.push((p.lane.label(), wanted, f64::from(MAX_GAIN_DB) + best));
            }
        } else if lane_db != wanted {
            clamped.push((p.lane.label(), wanted, lane_db));
        }
    }
    clamped
}

/// The whole timeline rendered the way an export writes it (every unmuted lane at its gain) to a
/// temporary WAV, removed when the returned guard drops.
pub fn render_mix(s: &PlaybackSession) -> Result<TempWav, String> {
    if let Some(why) = offline_refusal(s) {
        return Err(why);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let wav =
        TempWav(std::env::temp_dir().join(format!("edith-mix-{}-{nanos}.wav", std::process::id())));
    let settings = ExportSettings {
        format: Format::Wav,
        ..ExportSettings::default()
    };
    let handle = s.export_to_with(&wav.0, &settings);
    let outcome = loop {
        if let Some(r) = handle.result() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    outcome.map_err(|e| format!("rendering the mix: {e}"))?;
    Ok(wav)
}

/// The integrated target online video aims for (YouTube, Spotify), in LUFS.
pub const TARGET_ONLINE_LUFS: f64 = -14.0;
/// The true-peak ceiling a normalize holds the sound to, in dBTP.
pub const TARGET_TRUE_PEAK: f64 = -1.0;
/// The targets a picker offers: online video, streaming speech and broadcast (EBU R128).
pub const PRESETS: [(&str, f64); 3] = [("Online video", -14.0), ("Podcast", -16.0), ("Broadcast", -23.0)];
/// How close the level has to land, in LU, before a normalize stops refining.
const TOLERANCE_LU: f64 = 0.1;
/// How far a true peak may sit over the ceiling, in dB, before the gain is cut back.
const PEAK_TOLERANCE_DB: f64 = 0.05;
/// Measure-and-correct rounds after the first gain.
const PASSES: usize = 8;

/// What a normalize works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every unmuted audio lane with clips, moved by one common gain so the mix lands on target.
    Mix,
    /// Each lane on its own: every one set so that it alone measures the target.
    Each(Vec<Lane>),
}

/// One lane's change.
#[derive(Clone, Debug)]
pub struct LaneChange {
    pub lane: Lane,
    /// The lane alone, before and after (lane gain included).
    pub before: Loudness,
    pub after: Loudness,
    /// The gain the lane moved by in total, lane gain and clip gain, in dB.
    pub moved_db: f64,
}

/// What a normalize did, measured.
#[derive(Clone, Debug)]
pub struct Report {
    pub lufs: f64,
    pub true_peak: f64,
    /// The mix (for `Scope::Mix`) or the lane (for `Each` with one lane) before and after; for
    /// `Each` with several lanes, the mix is not measured and these are the first lane's.
    pub before: Loudness,
    pub after: Loudness,
    pub lanes: Vec<LaneChange>,
    /// Whether the level landed on the target (within 0.1 LU).
    pub reached: bool,
    /// Whether the true-peak ceiling, rather than the target, set the final level.
    pub peak_held: bool,
}

/// What `normalize` measures at each step.
enum Probe {
    Mix,
    Lane(Lane),
}

fn measure_probe(s: &PlaybackSession, probe: &Probe, fps: f64, end: u32) -> Result<Loudness, String> {
    match probe {
        Probe::Mix => measure_mix(s, 0, end, fps, end),
        Probe::Lane(lane) => {
            let g = f64::from(s.lane_gain_db(*lane));
            let l = measure_lane(s, *lane, 0, lane_end(s, *lane), fps)?
                .ok_or_else(|| format!("{} has no sound to measure", lane.label()))?;
            Ok(shifted(l, g))
        }
    }
}

/// The frame a lane's last clip ends at.
pub fn lane_end(s: &PlaybackSession, lane: Lane) -> u32 {
    s.lane_clips(lane).iter().map(Clip::end).max().unwrap_or(0)
}

/// The audio lanes a mix normalize moves: unmuted, with clips.
pub fn mix_lanes(s: &PlaybackSession) -> Vec<Lane> {
    s.lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio && !s.lane_clips(*l).is_empty() && !s.lane_muted(*l))
        .collect()
}

/// Moves `lanes` by one common gain until `probe` measures `lufs` with the peaks under `tp`:
/// the first gain is predicted from `first`, then measured and corrected. Returns the final
/// measurement and whether the ceiling cut the level short.
#[allow(clippy::too_many_arguments)]
fn settle(
    s: &mut PlaybackSession,
    lanes: &[Lane],
    first: &Loudness,
    lufs: f64,
    tp: f64,
    mix: bool,
    fps: f64,
    end: u32,
) -> Result<(Loudness, bool), String> {
    let plans: Vec<Plan> = lanes.iter().map(|&l| plan_of(s, l)).collect();
    let mut gain = loudness::gain_to_target(first, lufs, tp);
    set_plans(s, &plans, gain);
    let probe_of = |s: &PlaybackSession| -> Result<Loudness, String> {
        match (mix, lanes.first()) {
            (false, Some(&l)) => measure_probe(s, &Probe::Lane(l), fps, end),
            _ => measure_probe(s, &Probe::Mix, fps, end),
        }
    };
    let mut after = probe_of(s)?;
    let mut cut = false;
    for _ in 0..PASSES {
        if !after.integrated_lufs.is_finite() {
            break;
        }
        let miss = lufs - after.integrated_lufs;
        let over = if after.true_peak_dbtp.is_finite() { after.true_peak_dbtp - tp } else { 0.0 };
        if miss.abs() <= TOLERANCE_LU && over <= PEAK_TOLERANCE_DB {
            break;
        }
        // Gain moves the level and the peaks about equally: a step up stops at the ceiling.
        let mut step = miss;
        if over > PEAK_TOLERANCE_DB {
            step = step.min(-over);
            cut = true;
        } else if step > -over {
            step = -over;
            cut = true;
        }
        if step.abs() < 0.01 {
            break;
        }
        gain += step;
        set_plans(s, &plans, gain);
        after = probe_of(s)?;
    }
    Ok((after, cut))
}

/// Loudness-normalizes the timeline to `lufs` (integrated) with the true peak held under `tp`
/// dBTP. One undo step. Nothing changes when a lane cannot be measured or is locked.
pub fn normalize(s: &mut PlaybackSession, scope: &Scope, lufs: f64, tp: f64) -> Result<Report, String> {
    if !(-70.0..=0.0).contains(&lufs) {
        return Err(format!("lufs {lufs} is out of range; it must be -70 to 0"));
    }
    if !(-20.0..=0.0).contains(&tp) {
        return Err(format!("true_peak {tp} is out of range; it must be -20 to 0"));
    }
    let fps = s.meta().frame_rate;
    let end = (s.timeline_duration() * fps).round() as u32;
    let lanes = match scope {
        Scope::Mix => mix_lanes(s),
        Scope::Each(l) => l.clone(),
    };
    if lanes.is_empty() {
        return Err("the mix has no unmuted audio lane with clips to move".into());
    }
    for l in &lanes {
        if l.kind != LaneKind::Audio {
            return Err(format!("{} is not an audio lane", l.label()));
        }
        if s.lane_clips(*l).is_empty() {
            return Err(format!("{} has no clips to normalize", l.label()));
        }
        if s.lane_locked(*l) {
            return Err(format!("lane {} is locked: unlock it first", l.label()));
        }
    }
    // Everything is measured before anything changes, so a lane that cannot be read changes nothing.
    let mut befores = Vec::new();
    for &l in &lanes {
        befores.push(measure_probe(s, &Probe::Lane(l), fps, end)?);
    }
    let mix_before = match scope {
        Scope::Mix => Some(measure_probe(s, &Probe::Mix, fps, end)?),
        Scope::Each(_) => None,
    };
    if mix_before.as_ref().is_some_and(|m| !m.integrated_lufs.is_finite()) {
        return Err("the mix is silent: there is no loudness to normalize".into());
    }
    let start_plans: Vec<Plan> = lanes.iter().map(|&l| plan_of(s, l)).collect();
    let pushed = s.history_pushes();
    s.begin_batch();
    let run = (|| -> Result<(Vec<Loudness>, Option<Loudness>, bool, bool), String> {
        match scope {
            Scope::Mix => {
                let before = mix_before.as_ref().expect("a mix is measured");
                let (after, cut) = settle(s, &lanes, before, lufs, tp, true, fps, end)?;
                let afters = lanes
                    .iter()
                    .map(|&l| measure_probe(s, &Probe::Lane(l), fps, end))
                    .collect::<Result<Vec<_>, _>>()?;
                let reached = (lufs - after.integrated_lufs).abs() <= TOLERANCE_LU + 1e-6;
                Ok((afters, Some(after), reached, cut))
            }
            Scope::Each(_) => {
                let mut afters = Vec::new();
                let (mut reached, mut held) = (true, false);
                for (i, &l) in lanes.iter().enumerate() {
                    if !befores[i].integrated_lufs.is_finite() {
                        afters.push(befores[i]);
                        continue;
                    }
                    let (after, cut) = settle(s, &[l], &befores[i], lufs, tp, false, fps, end)?;
                    reached &= (lufs - after.integrated_lufs).abs() <= TOLERANCE_LU + 1e-6;
                    held |= cut;
                    afters.push(after);
                }
                Ok((afters, None, reached, held))
            }
        }
    })();
    s.end_batch();
    let (afters, mix_after, reached, peak_held) = match run {
        Ok(v) => v,
        Err(e) => {
            // Take the half-done change back; the undo history keeps no trace of a failure.
            if s.history_pushes() > pushed {
                s.undo();
                s.clear_redo();
            }
            return Err(e);
        }
    };
    let changes: Vec<LaneChange> = lanes
        .iter()
        .zip(start_plans.iter())
        .zip(befores.iter().zip(afters.iter()))
        .map(|((&lane, plan), (b, a))| LaneChange {
            lane,
            before: *b,
            after: *a,
            moved_db: total_moved(s, plan),
        })
        .collect();
    let (before, after) = match (mix_before, mix_after) {
        (Some(b), Some(a)) => (b, a),
        _ => (changes[0].before, changes[0].after),
    };
    Ok(Report { lufs, true_peak: tp, before, after, lanes: changes, reached, peak_held })
}

/// The gain a plan's lane really moved by in total: its lane gain, and the mean clip gain on top.
fn total_moved(s: &PlaybackSession, p: &Plan) -> f64 {
    let lane = f64::from(s.lane_gain_db(p.lane)) - p.old;
    if p.clips.is_empty() {
        return lane;
    }
    let clip: f64 = p
        .clips
        .iter()
        .map(|&(i, old)| f64::from(s.clip_gain_of(p.lane, i).unwrap_or(0.0)) - old)
        .sum::<f64>()
        / p.clips.len() as f64;
    lane + clip
}
