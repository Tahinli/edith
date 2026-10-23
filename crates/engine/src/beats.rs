//! Where a sound's beat falls, so a cut can land on the music.
//!
//! Three steps, the same shape as [`crate::silence`]: [`analyse`] decodes a
//! stretch of a file once and turns it into an onset-strength envelope (spectral
//! flux over 1024-sample Hann frames, 512 hop, at the file's own rate, streamed
//! so a long take never sits in memory whole); [`find`] reads that envelope for
//! a tempo and a grid of beats along it; [`timeline_beats`] puts those beats on
//! the timeline through one clip's own range and rate, the way
//! [`crate::silence::timeline_regions`] does for silences.
//!
//! The tempo is the autocorrelation peak between 60 and 200 BPM, with a mild
//! preference for the octave near 120 so a click at 120 is not read as 60 or
//! 240 by a hair. The beats are a fixed-period grid, and the phase of that grid
//! is the one whose grid points sit on the most onset energy. It is not a
//! tracker for music whose tempo drifts: a grid is what a cut needs, and a drift
//! shows as beats that slide off the hits after a while.

use std::path::Path;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::AudioSession;
use crate::project::{Clip, Marker, MarkerColor};

/// Samples per analysis frame.
pub const FRAME: usize = 1024;
/// Samples between the starts of two frames: 512, so each frame overlaps the last by half.
pub const HOP: usize = 512;
/// The slowest tempo looked for, in beats per minute.
pub const MIN_BPM: f64 = 60.;
/// The fastest tempo looked for, in beats per minute.
pub const MAX_BPM: f64 = 200.;

/// The envelope's peak, below which the sound is silence to the detector.
const SILENT_PEAK: f32 = 1e-3;
/// The best periodicity a sound may show and still have a beat, as a fraction of its own energy.
const MIN_PULSE: f64 = 0.1;

/// What a beat search came back with. The beats are seconds **from where the read
/// began**, in order, one per beat of the grid; the caller maps them.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// The file has no audio track, so there is no music to listen to.
    NoAudio,
    /// The sound is silent, so no beat can be heard in it.
    Silent,
    /// The sound has no steady pulse one could keep time to.
    NoPulse,
    /// A steady pulse: its tempo and every beat along it.
    Beats { bpm: f64, times: Vec<f64> },
}

/// The onset-strength envelope of a stretch of `stream` of `path` over the
/// half-open `range` of source seconds, and the rate its frames come at. Frame
/// `i` covers samples `i*HOP .. i*HOP + FRAME`, and its centre is the beat time
/// the grid is built on.
pub fn analyse(
    path: impl AsRef<Path>,
    stream: usize,
    range: (f64, f64),
) -> crate::Result<Option<(Vec<f32>, f64)>> {
    let from = range.0.max(0.);
    let sources = [(path.as_ref().to_path_buf(), stream)];
    let Some((meta, rx)) = AudioSession::open_multi_streams(&sources, &[(Some(0), from, range.1)])?
    else {
        return Ok(None);
    };
    let rate = f64::from(meta.sample_rate).max(1.);
    let channels = (meta.channels as usize).max(1);
    let mut onsets = Onsets::new();
    let mut mono: Vec<f32> = Vec::new();
    for chunk in rx {
        mono.clear();
        // Folded to one channel: a beat is a beat in either ear.
        mono.extend(
            chunk
                .samples
                .chunks(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32),
        );
        onsets.push(&mono);
    }
    Ok(Some((onsets.finish(), rate)))
}

/// Streams mono samples into the onset envelope: Hann-windowed frames, log-compressed
/// magnitudes, and the positive change in every bin since the frame before.
struct Onsets {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Samples not yet consumed by a whole frame.
    pending: Vec<f32>,
    /// Log magnitudes of the previous frame, one per bin up to Nyquist.
    prev: Vec<f32>,
    buf: Vec<Complex<f32>>,
    env: Vec<f32>,
}

impl Onsets {
    fn new() -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let window = (0..FRAME)
            .map(|i| {
                let x = std::f32::consts::PI * 2. * i as f32 / FRAME as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            fft,
            window,
            pending: Vec::new(),
            prev: vec![0.; FRAME / 2 + 1],
            buf: vec![Complex::new(0., 0.); FRAME],
            env: Vec::new(),
        }
    }

    fn push(&mut self, mono: &[f32]) {
        self.pending.extend_from_slice(mono);
        let mut at = 0;
        while self.pending.len() - at >= FRAME {
            for (i, slot) in self.buf.iter_mut().enumerate() {
                *slot = Complex::new(self.pending[at + i] * self.window[i], 0.);
            }
            self.fft.process(&mut self.buf);
            let mut flux = 0f32;
            for k in 0..=FRAME / 2 {
                let mag = self.buf[k].norm() / FRAME as f32;
                let db = (1. + 100. * mag).ln();
                flux += (db - self.prev[k]).max(0.);
                self.prev[k] = db;
            }
            self.env.push(flux);
            at += HOP;
        }
        self.pending.drain(..at);
    }

    fn finish(self) -> Vec<f32> {
        self.env
    }
}

/// The beats in an envelope read at `frame_rate` frames a second (the rate
/// [`analyse`] gives back over [`HOP`]).
pub fn find(env: &[f32], frame_rate: f64) -> Found {
    let peak = env.iter().copied().fold(0f32, f32::max);
    if peak <= SILENT_PEAK {
        return Found::Silent;
    }
    let n = env.len();
    if n < 4 || frame_rate <= 0. {
        return Found::NoPulse;
    }
    let mean = env.iter().map(|&v| f64::from(v)).sum::<f64>() / n as f64;
    let x: Vec<f64> = env.iter().map(|&v| f64::from(v) - mean).collect();
    // Autocorrelation at every whole lag up to the slowest beat, each divided by its
    // overlap so a long lag is not penalised for being short of data.
    let lag_max = (frame_rate * 60. / MIN_BPM).ceil() as usize + 1;
    let lag_max = lag_max.min(n - 1);
    let ac: Vec<f64> = (0..=lag_max)
        .map(|lag| {
            let s: f64 = x[..n - lag].iter().zip(&x[lag..]).map(|(a, b)| a * b).sum();
            s / (n - lag) as f64
        })
        .collect();
    let energy = ac[0];
    if energy <= 0. {
        return Found::NoPulse;
    }
    // Best tempo on a 0.1 BPM grid, the autocorrelation read between whole lags. The
    // log-octave prior is mild (one octave is a factor of e^-0.5) and only breaks a tie
    // between a tempo and its double or half.
    let mut best = (f64::NEG_INFINITY, MIN_BPM);
    let mut bpm = MIN_BPM;
    while bpm <= MAX_BPM + 1e-9 {
        let lag = frame_rate * 60. / bpm;
        let (lo, frac) = (lag.floor() as usize, lag - lag.floor());
        if lo < lag_max {
            let at = ac[lo] * (1. - frac) + ac[lo + 1] * frac;
            let prior = (-0.5 * ((bpm / 120.).log2()).powi(2)).exp();
            let score = at * prior;
            if score > best.0 {
                best = (score, bpm);
            }
        }
        bpm += 0.1;
    }
    let (_, bpm) = best;
    let period = frame_rate * 60. / bpm;
    // The pulse has to show in the sound itself, not only in the prior's favourite tempo.
    if ac_at(&ac, period) / energy < MIN_PULSE {
        return Found::NoPulse;
    }
    // Phase: the grid point offset whose beats sit on the most onset energy. Searched in
    // quarter frames, over one period.
    let mut best_phase = (f64::NEG_INFINITY, 0.);
    let mut phase = 0.;
    while phase < period {
        let mut sum = 0f64;
        let mut p = phase;
        while p < n as f64 {
            sum += f64::from(x_at(&x, p));
            p += period;
        }
        if sum > best_phase.0 {
            best_phase = (sum, phase);
        }
        phase += 0.25;
    }
    let mut times = Vec::new();
    let mut p = best_phase.1;
    while p < n as f64 {
        // Snapped to the strongest frame within two of the grid point, so a beat that sits a
        // hair off the grid lands on its hit.
        let centre = p.round() as usize;
        let lo = centre.saturating_sub(2);
        let hi = (centre + 2).min(n - 1);
        let mut at = centre.min(n - 1);
        for i in lo..=hi {
            if env[i] > env[at] {
                at = i;
            }
        }
        times.push((at * HOP + FRAME / 2) as f64 / (frame_rate * HOP as f64));
        p += period;
    }
    Found::Beats { bpm, times }
}

fn ac_at(ac: &[f64], lag: f64) -> f64 {
    let lo = lag.floor() as usize;
    let frac = lag - lag.floor();
    if lo + 1 >= ac.len() {
        return 0.;
    }
    ac[lo] * (1. - frac) + ac[lo + 1] * frac
}

fn x_at(x: &[f64], at: f64) -> f32 {
    let i = (at.round() as usize).min(x.len() - 1);
    x[i] as f32
}

/// [`analyse`] and [`find`] in one, for a stretch of a file: what a card or a tool asks.
/// `Found::NoAudio` for a file with no audio track.
pub fn detect(path: impl AsRef<Path>, stream: usize, range: (f64, f64)) -> crate::Result<Found> {
    let Some((env, rate)) = analyse(path, stream, range)? else {
        return Ok(Found::NoAudio);
    };
    Ok(find(&env, rate / HOP as f64))
}

/// Every `every`th beat, starting with the first: `every` 1 is every beat, 4 is one per bar of
/// four. A value of 0 is treated as 1.
pub fn every_nth(times: &[f64], every: usize) -> Vec<f64> {
    times.iter().copied().step_by(every.max(1)).collect()
}

/// The note a beat marker carries, and the one that tells a beat marker from any other.
pub const BEAT_NOTE: &str = "Beat";
/// The colour beat markers are drawn in: apart from the blue a marker gets by default.
pub const BEAT_COLOR: MarkerColor = MarkerColor::Yellow;

/// A beat marker on `frame`: a point, named and noted "Beat", in [`BEAT_COLOR`].
pub fn beat_marker(frame: u32) -> Marker {
    Marker {
        frame,
        length: 0,
        name: BEAT_NOTE.to_string(),
        note: BEAT_NOTE.to_string(),
        color: BEAT_COLOR,
    }
}

/// The indices, in order, of the beat markers that start inside `from..to` frames: the ones a
/// new set of beats for that clip replaces. Remove them highest index first.
pub fn beat_marker_indices(markers: &[Marker], from: u32, to: u32) -> Vec<usize> {
    markers
        .iter()
        .enumerate()
        .filter(|(_, m)| m.note == BEAT_NOTE && m.frame >= from && m.frame < to)
        .map(|(i, _)| i)
        .collect()
}

/// Where those beat times sit on the timeline, as frames of the lane `clip` is on: `times` are
/// seconds from the clip's in point, which is where [`analyse`] began reading, so they land at
/// `in_frame` plus that. Positions go through the clip's own rate, and a beat outside the part
/// of the source the clip plays is dropped. Frames come back in order with no repeats.
pub fn timeline_beats(clip: &Clip, fps: f64, times: &[f64]) -> Vec<u32> {
    if !(fps.is_finite() && fps > 0.) {
        return Vec::new();
    }
    let mut out: Vec<u32> = Vec::with_capacity(times.len());
    for &t in times {
        let src = (t * fps).round();
        if src.is_nan() || src < 0. {
            continue;
        }
        let src = clip.in_frame.saturating_add(src.min(f64::from(u32::MAX)) as u32);
        if src < clip.in_frame || src >= clip.out_frame {
            continue;
        }
        let at = clip.start + clip.timeline_offset(src - clip.in_frame);
        if out.last() != Some(&at) {
            out.push(at);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{Found, HOP, detect, find, timeline_beats};
    use crate::project::{Clip, Speed};
    use crate::scale::FitPolicy;

    /// An onset envelope of pulses every `period` frames, with a little noise of
    /// the sort a real mix leaves, so the test is not a pure comb.
    fn comb(n: usize, period: f64, phase: f64) -> Vec<f32> {
        let mut env = vec![0.02f32; n];
        let mut p = phase;
        while p < n as f64 {
            env[p as usize] = 1.;
            env[(p as usize + 1).min(n - 1)] = 0.4;
            p += period;
        }
        for (i, v) in env.iter_mut().enumerate() {
            *v += 0.01 * ((i * 7919) % 13) as f32 / 13.;
        }
        env
    }

    #[test]
    fn a_steady_comb_gives_its_tempo_and_its_beats() {
        // 86.13 frames a second at 44.1 kHz and HOP 512; 120 BPM is a beat every 43.07 frames.
        let rate = 44_100. / HOP as f64;
        let period = rate * 60. / 120.;
        let env = comb(4000, period, 10.);
        let Found::Beats { bpm, times } = find(&env, rate) else {
            panic!("a steady pulse is a beat");
        };
        assert!((bpm - 120.).abs() <= 2., "bpm {bpm}");
        assert!(times.len() > 80, "{} beats", times.len());
    }

    #[test]
    fn silence_is_silent_and_noise_has_no_pulse() {
        let rate = 86.13;
        assert_eq!(find(&vec![0.; 500], rate), Found::Silent);
        // Noise has energy but no periodicity at any tempo in range.
        let mut seed = 12_345u64;
        let noise: Vec<f32> = (0..2000)
            .map(|_| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                (seed >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect();
        assert_eq!(find(&noise, rate), Found::NoPulse);
    }

    /// A scratch directory for one test's ffmpeg output, emptied and made again.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("edith-beats-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Renders `expr` (an aevalsrc expression of `t`) as ten seconds of 44.1 kHz mono WAV.
    fn render(dir: &Path, name: &str, expr: &str) -> PathBuf {
        let out = dir.join(name);
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc='{expr}':d=10:s=44100:c=mono"))
            .args(["-c:a", "pcm_s16le"])
            .arg(&out)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg made {name}");
        out
    }

    /// Two beats per second is 120 BPM: a 20 ms 1 kHz burst on every half second.
    #[test]
    fn a_click_track_at_120_bpm_gives_120_and_every_half_second() {
        let dir = scratch("click");
        let wav = render(&dir, "click120.wav", "0.8*sin(2*PI*1000*t)*lt(mod(t,0.5),0.02)");
        let found = detect(&wav, 0, (0., f64::INFINITY)).expect("analyse");
        let _ = std::fs::remove_dir_all(&dir);
        let Found::Beats { bpm, times } = found else {
            panic!("a click track has beats: {found:?}");
        };
        assert!((bpm - 120.).abs() <= 2., "bpm {bpm}");
        // Within one frame at 25 fps of a multiple of 0.5 s, every beat in the ten seconds.
        assert!((18..=21).contains(&times.len()), "{} beats", times.len());
        for &t in &times {
            let off = (t - (t / 0.5).round() * 0.5).abs();
            assert!(off < 1. / 25., "beat at {t} is {off} s off the grid");
        }
    }

    #[test]
    fn a_silent_clip_has_no_beats() {
        let dir = scratch("silent");
        let wav = render(&dir, "silent.wav", "0");
        let found = detect(&wav, 0, (0., f64::INFINITY)).expect("analyse");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Found::Silent);
    }

    #[test]
    fn beats_land_on_the_timeline_through_the_clip() {
        // 25 fps; a clip at frame 100 that plays source frames 50..300 at normal speed.
        let clip = Clip {
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start: 100,
            in_frame: 50,
            out_frame: 300,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::default(),
            pitch: 0,
        };
        // 0.5 s is source frame 62.5, rounded to 63 -> timeline 113; 1.0 s is 75 -> 125.
        // 20 s is past the clip's end and is dropped.
        assert_eq!(timeline_beats(&clip, 25., &[0.5, 1.0, 20.]), vec![113, 125]);
        assert!(timeline_beats(&clip, 0., &[0.5]).is_empty());
    }
}
