//! Voice cleanup: the per-lane processors a creator reaches for first -- a noise
//! gate, spectral noise reduction, a compressor and a de-esser -- and the
//! one-click preset that sets them together ([`VoiceFx::clean_voice`]).
//!
//! They run on one audio lane's samples, after the lane's clips have been
//! decoded, cut, equalized and faded (the clip equalizer lives in the segment
//! workers, so it comes *first*) and before the lane's volume, its duck and the
//! master limiter: gate, then noise reduction, then compressor, then de-esser
//! ([`VoiceChain`]). The whole chain is **streaming**: every stage carries its
//! state from block to block, so the output does not depend on where the mix
//! happens to cut the stream into blocks, and playback and export, which pour
//! the same chunks through the same stage, cannot disagree.
//!
//! * **Gate** ([`Gate`]): a 10 ms RMS detector with 2 dB of hysteresis, hold,
//!   and attack/release time constants on the gain. It closes to `range_db`,
//!   not to silence, so a pause keeps a little of the room.
//! * **Noise reduction** ([`Denoise`]): power spectral subtraction in the STFT
//!   domain -- 2048-point FFT, 75% overlap, Hann analysis and synthesis. The
//!   noise is a *profile* ([`learn_profile`]): the mean power spectrum of a
//!   stretch of the lane that holds only noise, kept in 256 bands of dB so it
//!   is small enough to live in the project file. `strength` scales how much of
//!   it is subtracted and `floor_db` is how far a bin may be pulled down, which
//!   is what keeps speech from going watery. The channels share one gain per
//!   bin, so the stereo image does not move. It has [`LATENCY`] samples of
//!   algorithmic delay, which the stage ([`spawn_stage`]) takes out again by
//!   reading ahead, so the lane stays in step with the picture and the other
//!   lanes.
//! * **Compressor** ([`Compressor`]): feed-forward, a 10 ms RMS detector (dBFS),
//!   a soft-knee static curve and attack/release ballistics on the gain
//!   reduction in dB; manual or automatic makeup.
//! * **De-esser** ([`DeEsser`]): a split-band compressor. A 4th-order band-pass
//!   between `lo_hz` and `hi_hz` measures the sibilance; whatever it is over the
//!   threshold, times `amount`, is taken out of that band by a peaking filter at
//!   its centre (a dynamic EQ), so everything outside the band passes untouched
//!   and with no reduction the filter is not even run.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::audio::AudioChunk;

/// The FFT size of the noise reduction.
pub const FFT_SIZE: usize = 2048;
/// The hop between two frames: 75% overlap.
pub const HOP: usize = FFT_SIZE / 4;
/// The algorithmic delay of the noise reduction, in frames of audio.
pub const LATENCY: usize = FFT_SIZE - HOP;
/// The bands a noise profile is kept in.
pub const PROFILE_BANDS: usize = 256;
const BINS: usize = FFT_SIZE / 2 + 1;
const BINS_PER_BAND: usize = (FFT_SIZE / 2) / PROFILE_BANDS;
/// How long a stretch the automatic noise search looks for, in seconds.
pub const AUTO_NOISE_SECS: f64 = 0.5;
/// A level under this is digital silence -- a gap or a muted take -- and never noise.
const DIGITAL_SILENCE_DB: f32 = -90.0;

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

/// One-pole smoothing coefficient for a time constant in ms: the share of the way
/// to the target one sample covers.
fn coef(ms: f32, rate: f32) -> f32 {
    1.0 - (-1.0 / (ms.max(0.01) * 0.001 * rate)).exp()
}

/// A recursive state that has decayed to nothing is zero, so a long silence never
/// leaves a denormal circulating (they cost 100x on some CPUs).
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

fn all_finite(values: &[f32]) -> bool {
    values.iter().all(|v| v.is_finite())
}

// ---- parameters -------------------------------------------------------------

/// The noise gate of one lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    pub on: bool,
    /// Open at or above this RMS level, in dBFS (`-90..=0`).
    pub threshold_db: f32,
    /// How fast it opens, ms (`0.1..=100`).
    pub attack_ms: f32,
    /// How fast it closes, ms (`1..=3000`).
    pub release_ms: f32,
    /// How long it stays open after the level falls below the threshold, ms (`0..=2000`).
    pub hold_ms: f32,
    /// How far it attenuates when closed, dB (`-90..=0`).
    pub range_db: f32,
}

impl Gate {
    pub const DEFAULT: Gate =
        Gate { on: false, threshold_db: -45.0, attack_ms: 2.0, release_ms: 150.0, hold_ms: 60.0, range_db: -18.0 };

    pub fn clamped(self) -> Option<Self> {
        all_finite(&[self.threshold_db, self.attack_ms, self.release_ms, self.hold_ms, self.range_db]).then(|| Self {
            on: self.on,
            threshold_db: self.threshold_db.clamp(-90.0, 0.0),
            attack_ms: self.attack_ms.clamp(0.1, 100.0),
            release_ms: self.release_ms.clamp(1.0, 3000.0),
            hold_ms: self.hold_ms.clamp(0.0, 2000.0),
            range_db: self.range_db.clamp(-90.0, 0.0),
        })
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The spectral noise reduction of one lane.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseReduction {
    pub on: bool,
    /// How much of the noise is taken out, `0..=1` (0 is nothing, 1 is four times the profile).
    pub strength: f32,
    /// How far a bin may be pulled down, dB (`-60..=0`). The spectral floor: a
    /// shallower floor keeps speech natural at the cost of more left-over noise.
    pub floor_db: f32,
    /// The noise profile, [`PROFILE_BANDS`] bands of dB power, empty until one is learned.
    pub profile: Vec<f32>,
}

impl NoiseReduction {
    pub fn clamped(&self) -> Option<Self> {
        if !all_finite(&[self.strength, self.floor_db]) || !all_finite(&self.profile) {
            return None;
        }
        if !(self.profile.is_empty() || self.profile.len() == PROFILE_BANDS) {
            return None;
        }
        Some(Self {
            on: self.on,
            strength: self.strength.clamp(0.0, 1.0),
            floor_db: self.floor_db.clamp(-60.0, 0.0),
            profile: self.profile.iter().map(|d| d.clamp(-200.0, 200.0)).collect(),
        })
    }
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self { on: false, strength: 0.6, floor_db: -18.0, profile: Vec::new() }
    }
}

/// The compressor of one lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Compressor {
    pub on: bool,
    /// Compression starts above this RMS level, dBFS (`-60..=0`).
    pub threshold_db: f32,
    /// Input dB per output dB above the threshold (`1..=20`).
    pub ratio: f32,
    /// ms (`0.1..=500`).
    pub attack_ms: f32,
    /// ms (`5..=5000`).
    pub release_ms: f32,
    /// Width of the soft knee, dB (`0..=30`).
    pub knee_db: f32,
    /// Makeup gain, dB (`-12..=24`); ignored while `auto_makeup` is on.
    pub makeup_db: f32,
    /// Makeup that brings full scale back to full scale.
    pub auto_makeup: bool,
}

impl Compressor {
    pub const DEFAULT: Compressor = Compressor {
        on: false,
        threshold_db: -24.0,
        ratio: 3.0,
        attack_ms: 10.0,
        release_ms: 100.0,
        knee_db: 6.0,
        makeup_db: 0.0,
        auto_makeup: true,
    };

    pub fn clamped(self) -> Option<Self> {
        all_finite(&[self.threshold_db, self.ratio, self.attack_ms, self.release_ms, self.knee_db, self.makeup_db])
            .then(|| Self {
                on: self.on,
                threshold_db: self.threshold_db.clamp(-60.0, 0.0),
                ratio: self.ratio.clamp(1.0, 20.0),
                attack_ms: self.attack_ms.clamp(0.1, 500.0),
                release_ms: self.release_ms.clamp(5.0, 5000.0),
                knee_db: self.knee_db.clamp(0.0, 30.0),
                makeup_db: self.makeup_db.clamp(-12.0, 24.0),
                auto_makeup: self.auto_makeup,
            })
    }

    /// The makeup actually applied, dB.
    pub fn makeup(&self) -> f32 {
        if self.auto_makeup {
            (-self.threshold_db * (1.0 - 1.0 / self.ratio)).clamp(0.0, 24.0)
        } else {
            self.makeup_db
        }
    }

    /// The static curve: output level for an input level, both dB (makeup not included).
    pub fn curve(&self, x: f32) -> f32 {
        let (t, r, w) = (self.threshold_db, self.ratio, self.knee_db);
        let over = x - t;
        if 2.0 * over < -w {
            x
        } else if w > 0.0 && 2.0 * over.abs() <= w {
            x + (1.0 / r - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
        } else {
            t + over / r
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The de-esser of one lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeEsser {
    pub on: bool,
    /// The band's level above which it is turned down, dBFS of its peak envelope (`-60..=0`).
    pub threshold_db: f32,
    /// How much of the excess is taken out, `0..=1`.
    pub amount: f32,
    /// The band's edges, Hz (`1000..=12000` and `2000..=16000`, the top above the bottom).
    pub lo_hz: f32,
    pub hi_hz: f32,
}

impl DeEsser {
    pub const DEFAULT: DeEsser = DeEsser { on: false, threshold_db: -30.0, amount: 0.55, lo_hz: 4500.0, hi_hz: 9000.0 };

    pub fn clamped(self) -> Option<Self> {
        if !all_finite(&[self.threshold_db, self.amount, self.lo_hz, self.hi_hz]) {
            return None;
        }
        let lo_hz = self.lo_hz.clamp(1000.0, 12000.0);
        let hi_hz = self.hi_hz.clamp(2000.0, 16000.0);
        (hi_hz > lo_hz * 1.1).then_some(Self {
            on: self.on,
            threshold_db: self.threshold_db.clamp(-60.0, 0.0),
            amount: self.amount.clamp(0.0, 1.0),
            lo_hz,
            hi_hz,
        })
    }
}

impl Default for DeEsser {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How hard a one-click cleanup works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strength {
    Light,
    Medium,
    Strong,
}

impl Strength {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "light" => Some(Self::Light),
            "medium" => Some(Self::Medium),
            "strong" => Some(Self::Strong),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }
}

/// Every voice processor of one audio lane, in the order they run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceFx {
    pub gate: Gate,
    pub nr: NoiseReduction,
    pub comp: Compressor,
    pub deess: DeEsser,
}

impl VoiceFx {
    /// Whether the noise reduction can run: it is on and has a profile to subtract.
    pub fn nr_active(&self) -> bool {
        self.nr.on && self.nr.profile.len() == PROFILE_BANDS && self.nr.strength > 0.0
    }

    /// Whether anything is switched on (and so whether the lane needs a stage at all).
    pub fn is_active(&self) -> bool {
        self.gate.on || self.nr_active() || self.comp.on || self.deess.on
    }

    /// Whether the lane carries no setting at all, which is what a save leaves out.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Every number clamped into its range; `None` for one that is not finite.
    pub fn clamped(&self) -> Option<Self> {
        Some(Self {
            gate: self.gate.clamped()?,
            nr: self.nr.clamped()?,
            comp: self.comp.clamped()?,
            deess: self.deess.clamped()?,
        })
    }

    /// The processors that are on, for a one-line listing: `gate -45 dB, nr 60%, comp 3:1, de-ess`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.gate.on {
            parts.push(format!("gate {:.0} dB", self.gate.threshold_db));
        }
        if self.nr.on {
            if self.nr.profile.is_empty() {
                parts.push("nr (no profile)".to_string());
            } else {
                parts.push(format!("nr {:.0}%", self.nr.strength * 100.0));
            }
        }
        if self.comp.on {
            parts.push(format!("comp {:.0} dB {}:1", self.comp.threshold_db, trim_num(self.comp.ratio)));
        }
        if self.deess.on {
            parts.push(format!("de-ess {:.0} dB", self.deess.threshold_db));
        }
        parts.join(", ")
    }

    /// The one-click voice preset: gate, noise reduction, compressor and de-esser at
    /// settings that suit speech. `noise_db` is the learned noise's RMS level in dBFS
    /// when known; it sets where the gate opens (a few dB over the noise).
    pub fn clean_voice(strength: Strength, profile: Vec<f32>, noise_db: Option<f32>) -> Self {
        let (gate_over, range, nr, floor, comp_t, ratio, comp_att, comp_rel, ess_t, ess) = match strength {
            Strength::Light => (6.0, -10.0, 0.35, -12.0, -22.0, 2.0, 15.0, 120.0, -26.0, 0.4),
            Strength::Medium => (9.0, -18.0, 0.6, -18.0, -24.0, 3.0, 10.0, 100.0, -30.0, 0.55),
            Strength::Strong => (12.0, -30.0, 0.85, -24.0, -26.0, 4.0, 8.0, 100.0, -33.0, 0.7),
        };
        let threshold = noise_db.map_or(-45.0, |n| (n + gate_over).clamp(-70.0, -30.0));
        Self {
            gate: Gate { on: true, threshold_db: threshold, range_db: range, ..Gate::DEFAULT },
            nr: NoiseReduction { on: !profile.is_empty(), strength: nr, floor_db: floor, profile },
            comp: Compressor {
                on: true,
                threshold_db: comp_t,
                ratio,
                attack_ms: comp_att,
                release_ms: comp_rel,
                // Half the automatic makeup: the full amount lifts speech peaks past full scale.
                makeup_db: -comp_t * (1.0 - 1.0 / ratio) * 0.5,
                auto_makeup: false,
                ..Compressor::DEFAULT
            },
            deess: DeEsser { on: true, threshold_db: ess_t, amount: ess, ..DeEsser::DEFAULT },
        }
    }
}

fn trim_num(x: f32) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

// ---- gate ---------------------------------------------------------------------

/// The gate's running state.
pub struct GateState {
    channels: usize,
    thr: f32,
    range: f32,
    power: f32,
    det: f32,
    gain: f32,
    open: bool,
    hold_left: u32,
    hold_n: u32,
    a_att: f32,
    a_rel: f32,
}

const GATE_HYSTERESIS_DB: f32 = 2.0;

impl GateState {
    pub fn new(p: &Gate, rate: u32, channels: usize) -> Self {
        let r = rate.max(1) as f32;
        Self {
            channels: channels.max(1),
            thr: p.threshold_db,
            range: db_to_lin(p.range_db),
            power: 0.0,
            det: coef(10.0, r),
            gain: 1.0,
            open: true,
            hold_left: 0,
            hold_n: (p.hold_ms * 0.001 * r) as u32,
            a_att: coef(p.attack_ms, r),
            a_rel: coef(p.release_ms, r),
        }
    }

    pub fn process(&mut self, block: &mut [f32]) {
        let ch = self.channels;
        for frame in block.chunks_exact_mut(ch) {
            let mut sq = 0.0f32;
            for &s in frame.iter() {
                sq = sq.max(s * s);
            }
            self.power = flush(self.power + (sq - self.power) * self.det);
            let db = 10.0 * self.power.max(1e-20).log10();
            if db >= self.thr {
                self.open = true;
                self.hold_left = self.hold_n;
            } else if self.open {
                if db >= self.thr - GATE_HYSTERESIS_DB {
                    self.hold_left = self.hold_n;
                } else if self.hold_left == 0 {
                    self.open = false;
                } else {
                    self.hold_left -= 1;
                }
            }
            let target = if self.open { 1.0 } else { self.range };
            let a = if target > self.gain { self.a_att } else { self.a_rel };
            self.gain += (target - self.gain) * a;
            if self.gain != 1.0 {
                for s in frame.iter_mut() {
                    *s *= self.gain;
                }
            }
        }
    }

    /// The gain now (1 open, the range closed).
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

// ---- compressor ----------------------------------------------------------------

/// The compressor's running state.
pub struct CompressorState {
    channels: usize,
    p: Compressor,
    makeup_db: f32,
    power: f32,
    det: f32,
    gr_db: f32,
    a_att: f32,
    a_rel: f32,
}

impl CompressorState {
    pub fn new(p: &Compressor, rate: u32, channels: usize) -> Self {
        let r = rate.max(1) as f32;
        Self {
            channels: channels.max(1),
            p: *p,
            makeup_db: p.makeup(),
            power: 0.0,
            det: coef(10.0, r),
            gr_db: 0.0,
            a_att: coef(p.attack_ms, r),
            a_rel: coef(p.release_ms, r),
        }
    }

    pub fn process(&mut self, block: &mut [f32]) {
        let ch = self.channels;
        for frame in block.chunks_exact_mut(ch) {
            let mut sq = 0.0f32;
            for &s in frame.iter() {
                sq = sq.max(s * s);
            }
            self.power = flush(self.power + (sq - self.power) * self.det);
            let level = 10.0 * self.power.max(1e-20).log10();
            let target = self.p.curve(level) - level;
            let a = if target < self.gr_db { self.a_att } else { self.a_rel };
            self.gr_db = flush(self.gr_db + (target - self.gr_db) * a);
            let g = db_to_lin(self.gr_db + self.makeup_db);
            for s in frame.iter_mut() {
                *s *= g;
            }
        }
    }

    /// The gain reduction now, dB (zero or negative).
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db
    }
}

// ---- de-esser ------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// RBJ Butterworth (Q = 1/sqrt 2) high- or low-pass.
    fn new(high: bool, hz: f32, rate: f32) -> Self {
        let w = 2.0 * PI * (hz / rate).clamp(1e-4, 0.49);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if high {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: -2.0 * cos / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }

    fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = flush(self.b1 * x - self.a1 * y + self.z2);
        self.z2 = flush(self.b2 * x - self.a2 * y);
        y
    }
}

/// One channel's band split: a 4th-order high-pass into a 4th-order low-pass.
#[derive(Clone, Copy)]
struct BandSplit {
    hp: [Biquad; 2],
    lp: [Biquad; 2],
}

impl BandSplit {
    fn new(lo: f32, hi: f32, rate: f32) -> Self {
        Self { hp: [Biquad::new(true, lo, rate); 2], lp: [Biquad::new(false, hi, rate); 2] }
    }

    fn band(&mut self, x: f32) -> f32 {
        let a = self.hp[0].tick(x);
        let a = self.hp[1].tick(a);
        let a = self.lp[0].tick(a);
        self.lp[1].tick(a)
    }
}

/// The de-esser's running state: a band-pass sidechain that measures the sibilance,
/// and a peaking filter at the band's centre that is pulled down by as much as the
/// sibilance is over the threshold -- a dynamic EQ, which (unlike subtracting a
/// band-passed copy) cuts the band by exactly the reduction, phase shift or not.
pub struct DeEsserState {
    channels: usize,
    p: DeEsser,
    split: Vec<BandSplit>,
    env: f32,
    d_env: f32,
    gr_db: f32,
    a_att: f32,
    a_rel: f32,
    cos: f32,
    alpha: f32,
    /// b0 b1 b2 a1 a2, normalised by a0; refreshed every few samples.
    k: [f32; 5],
    /// Transposed direct form II state, per channel.
    z: Vec<[f32; 2]>,
    tick: u32,
}

/// The deepest cut a de-esser makes, dB: past it a voice starts to lisp.
const DEESS_MAX_CUT_DB: f32 = 18.0;
/// How often the peaking filter's coefficients follow the gain reduction, in samples.
const DEESS_UPDATE: u32 = 4;

impl DeEsserState {
    pub fn new(p: &DeEsser, rate: u32, channels: usize) -> Self {
        let r = rate.max(1) as f32;
        let hi = p.hi_hz.min(r * 0.45);
        let lo = p.lo_hz.min(hi * 0.8);
        let octaves = (hi / lo).log2().max(0.1);
        let q = 2f32.powf(octaves / 2.0) / (2f32.powf(octaves) - 1.0);
        let w0 = 2.0 * PI * ((lo * hi).sqrt() / r).clamp(1e-4, 0.49);
        let (sin, cos) = w0.sin_cos();
        let channels = channels.max(1);
        let mut state = Self {
            channels,
            p: *p,
            split: vec![BandSplit::new(lo, hi, r); channels],
            env: 0.0,
            d_env: (-1.0 / (0.003 * r)).exp(),
            gr_db: 0.0,
            a_att: coef(0.5, r),
            a_rel: coef(20.0, r),
            cos,
            alpha: sin / (2.0 * q),
            k: [1.0, 0.0, 0.0, 0.0, 0.0],
            z: vec![[0.0; 2]; channels],
            tick: 0,
        };
        state.retune();
        state
    }

    /// The peaking filter at the current gain reduction (RBJ cookbook).
    fn retune(&mut self) {
        let a = (self.gr_db * std::f32::consts::LN_10 / 40.0).exp();
        let a0 = 1.0 + self.alpha / a;
        self.k = [
            (1.0 + self.alpha * a) / a0,
            -2.0 * self.cos / a0,
            (1.0 - self.alpha * a) / a0,
            -2.0 * self.cos / a0,
            (1.0 - self.alpha / a) / a0,
        ];
    }

    pub fn process(&mut self, block: &mut [f32]) {
        let ch = self.channels;
        for frame in block.chunks_exact_mut(ch) {
            let mut peak = 0.0f32;
            for (c, &s) in frame.iter().enumerate() {
                peak = peak.max(self.split[c].band(s).abs());
            }
            self.env = flush(if peak > self.env { peak } else { self.env * self.d_env });
            let db = lin_to_db(self.env);
            let over = (db - self.p.threshold_db).max(0.0);
            let target = -(over * self.p.amount).min(DEESS_MAX_CUT_DB);
            let a = if target < self.gr_db { self.a_att } else { self.a_rel };
            self.gr_db = flush(self.gr_db + (target - self.gr_db) * a);
            if self.gr_db == 0.0 && self.z.iter().all(|z| *z == [0.0, 0.0]) {
                continue; // at rest and nothing circulating: the input to the last bit
            }
            if self.tick == 0 {
                self.retune();
            }
            self.tick = (self.tick + 1) % DEESS_UPDATE;
            let [b0, b1, b2, a1, a2] = self.k;
            for (c, s) in frame.iter_mut().enumerate() {
                let z = &mut self.z[c];
                let x = *s;
                let y = b0 * x + z[0];
                z[0] = flush(b1 * x - a1 * y + z[1]);
                z[1] = flush(b2 * x - a2 * y);
                *s = y;
            }
        }
    }

    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_db
    }
}

// ---- noise reduction -------------------------------------------------------------

fn hann() -> Vec<f32> {
    (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_SIZE as f32).cos()).collect()
}

/// The per-bin noise power a stored profile stands for: linear in dB between band centres.
fn noise_power(profile: &[f32]) -> Vec<f32> {
    (0..BINS)
        .map(|k| {
            let pos = (k as f32 + 0.5) / BINS_PER_BAND as f32 - 0.5;
            let lo = pos.floor().clamp(0.0, (PROFILE_BANDS - 1) as f32);
            let hi = (lo + 1.0).min((PROFILE_BANDS - 1) as f32);
            let t = (pos - lo).clamp(0.0, 1.0);
            let db = profile[lo as usize] * (1.0 - t) + profile[hi as usize] * t;
            10f32.powf(db / 10.0)
        })
        .collect()
}

/// Streaming spectral subtraction. See the module docs.
pub struct Denoise {
    channels: usize,
    alpha: f32,
    floor2: f32,
    noise: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    win: Vec<f32>,
    inbuf: Vec<Vec<f32>>,
    filled: usize,
    ola: Vec<Vec<f32>>,
    spec: Vec<Vec<Complex<f32>>>,
    scratch: Vec<Complex<f32>>,
    smooth: Vec<f32>,
    gain: Vec<f32>,
    first: bool,
    skip: usize,
    fed: u64,
    emitted: u64,
}

impl Denoise {
    pub fn new(p: &NoiseReduction, channels: usize) -> Self {
        let channels = channels.max(1);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let ifft = planner.plan_fft_inverse(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len());
        Self {
            channels,
            alpha: 4.0 * p.strength,
            floor2: db_to_lin(p.floor_db).powi(2),
            noise: noise_power(&p.profile),
            fft,
            ifft,
            win: hann(),
            inbuf: vec![vec![0.0; FFT_SIZE]; channels],
            filled: 0,
            ola: vec![vec![0.0; FFT_SIZE]; channels],
            spec: vec![vec![Complex::new(0.0, 0.0); FFT_SIZE]; channels],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            smooth: vec![0.0; BINS],
            gain: vec![1.0; BINS],
            first: true,
            skip: LATENCY,
            fed: 0,
            emitted: 0,
        }
    }

    /// Takes interleaved samples and returns the interleaved samples that are ready:
    /// the same stream, in step with the input, [`LATENCY`] frames behind it. Over a
    /// whole stream, `process` of every block then [`finish`](Self::finish) returns
    /// exactly as many frames as went in.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.fed += (input.len() / self.channels) as u64;
        let mut out = Vec::with_capacity(input.len());
        self.push(input, &mut out);
        out
    }

    /// Flushes the frames still inside, so the stream ends where it began.
    pub fn finish(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        let zeros = vec![0.0; HOP * self.channels];
        while self.emitted < self.fed {
            self.push(&zeros, &mut out);
        }
        let over = (self.emitted - self.fed) as usize * self.channels;
        out.truncate(out.len() - over);
        self.emitted = self.fed;
        out
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        for frame in input.chunks_exact(ch) {
            for (c, &s) in frame.iter().enumerate() {
                self.inbuf[c][LATENCY + self.filled] = s;
            }
            self.filled += 1;
            if self.filled == HOP {
                self.frame(out);
                self.filled = 0;
            }
        }
    }

    fn frame(&mut self, out: &mut Vec<f32>) {
        let ch = self.channels;
        for c in 0..ch {
            for (i, bin) in self.spec[c].iter_mut().enumerate() {
                *bin = Complex::new(self.inbuf[c][i] * self.win[i], 0.0);
            }
            self.fft.process_with_scratch(&mut self.spec[c], &mut self.scratch);
        }
        let inv = 1.0 / ch as f32;
        for k in 0..BINS {
            let mut p = 0.0;
            for spec in &self.spec {
                p += spec[k].norm_sqr();
            }
            p *= inv;
            self.smooth[k] = if self.first { p } else { 0.5 * self.smooth[k] + 0.5 * p };
            let ps = self.smooth[k].max(1e-30);
            let g2 = ((ps - self.alpha * self.noise[k]) / ps).clamp(self.floor2, 1.0);
            self.gain[k] = g2.sqrt();
        }
        self.first = false;
        // 1 / (N * sum of the squared window over a hop) -- 1.5 for a Hann at 75%.
        let norm = 1.0 / (FFT_SIZE as f32 * 1.5);
        for c in 0..ch {
            let spec = &mut self.spec[c];
            for k in 0..BINS {
                spec[k] *= self.gain[k];
                if k != 0 && k != FFT_SIZE / 2 {
                    spec[FFT_SIZE - k] *= self.gain[k];
                }
            }
            self.ifft.process_with_scratch(spec, &mut self.scratch);
            for (i, o) in self.ola[c].iter_mut().enumerate() {
                *o += spec[i].re * self.win[i] * norm;
            }
        }
        for j in 0..HOP {
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            for c in 0..ch {
                out.push(self.ola[c][j]);
            }
            self.emitted += 1;
        }
        for c in 0..ch {
            self.ola[c].copy_within(HOP.., 0);
            let n = self.ola[c].len();
            self.ola[c][n - HOP..].fill(0.0);
            self.inbuf[c].copy_within(HOP.., 0);
        }
    }
}

/// The noise profile of a mono stretch: the mean power spectrum of its Hann frames,
/// in [`PROFILE_BANDS`] bands of dB (0.1 dB resolution), and the stretch's RMS level
/// in dBFS. `None` when it is shorter than one frame or not finite.
pub fn learn_profile(mono: &[f32]) -> Option<(Vec<f32>, f32)> {
    if mono.len() < FFT_SIZE || !all_finite(mono) {
        return None;
    }
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let win = hann();
    let mut buf = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut mean = vec![0.0f64; BINS];
    let mut frames = 0u32;
    let mut at = 0;
    while at + FFT_SIZE <= mono.len() {
        for i in 0..FFT_SIZE {
            buf[i] = Complex::new(mono[at + i] * win[i], 0.0);
        }
        fft.process(&mut buf);
        for k in 0..BINS {
            mean[k] += f64::from(buf[k].norm_sqr());
        }
        frames += 1;
        at += HOP;
    }
    let profile = (0..PROFILE_BANDS)
        .map(|b| {
            let lo = b * BINS_PER_BAND;
            let hi = if b == PROFILE_BANDS - 1 { BINS } else { lo + BINS_PER_BAND };
            let m = mean[lo..hi].iter().sum::<f64>() / ((hi - lo) as f64 * f64::from(frames));
            ((10.0 * m.max(1e-20).log10()) as f32 * 10.0).round() / 10.0
        })
        .collect();
    let rms = (mono.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / mono.len() as f64).sqrt();
    Some((profile, lin_to_db(rms as f32)))
}

/// The quietest [`AUTO_NOISE_SECS`] of a stream, found as it goes past (50 ms blocks,
/// O(1) memory beyond the window itself). A window with any digital silence in it --
/// a gap, a muted take -- is never the answer: there is no noise in nothing.
pub struct QuietestFinder {
    block: usize,
    blocks_per_window: usize,
    window: usize,
    acc: f64,
    acc_n: usize,
    recent: VecDeque<f64>,
    ring: VecDeque<f32>,
    pos: u64,
    best: Option<(f64, Vec<f32>, u64)>,
}

impl QuietestFinder {
    pub fn new(rate: u32) -> Self {
        let block = (rate.max(1) as usize / 20).max(1);
        let blocks_per_window = ((AUTO_NOISE_SECS * 20.0) as usize).max(1);
        Self {
            block,
            blocks_per_window,
            window: block * blocks_per_window,
            acc: 0.0,
            acc_n: 0,
            recent: VecDeque::new(),
            ring: VecDeque::new(),
            pos: 0,
            best: None,
        }
    }

    /// Feeds mono samples.
    pub fn push(&mut self, mono: &[f32]) {
        let silent = 10f64.powf(f64::from(DIGITAL_SILENCE_DB) / 10.0);
        for &s in mono {
            self.pos += 1;
            self.ring.push_back(s);
            if self.ring.len() > self.window {
                self.ring.pop_front();
            }
            self.acc += f64::from(s) * f64::from(s);
            self.acc_n += 1;
            if self.acc_n == self.block {
                self.recent.push_back(self.acc / self.block as f64);
                if self.recent.len() > self.blocks_per_window {
                    self.recent.pop_front();
                }
                self.acc = 0.0;
                self.acc_n = 0;
                if self.recent.len() == self.blocks_per_window
                    && self.ring.len() == self.window
                    && self.recent.iter().all(|&p| p > silent)
                {
                    let mean = self.recent.iter().sum::<f64>() / self.blocks_per_window as f64;
                    if self.best.as_ref().is_none_or(|(b, ..)| mean < *b) {
                        self.best = Some((mean, self.ring.iter().copied().collect(), self.pos));
                    }
                }
            }
        }
    }

    /// The quietest window seen: its samples and the sample count where it ended.
    pub fn best(self) -> Option<(Vec<f32>, u64)> {
        self.best.map(|(_, samples, end)| (samples, end))
    }
}

// ---- the chain ---------------------------------------------------------------------

/// Gate, noise reduction, compressor, de-esser, in that order, with only the ones
/// that are on built.
pub struct VoiceChain {
    gate: Option<GateState>,
    nr: Option<Denoise>,
    comp: Option<CompressorState>,
    deess: Option<DeEsserState>,
}

impl VoiceChain {
    pub fn new(fx: &VoiceFx, rate: u32, channels: usize) -> Self {
        Self {
            gate: fx.gate.on.then(|| GateState::new(&fx.gate, rate, channels)),
            nr: fx.nr_active().then(|| Denoise::new(&fx.nr, channels)),
            comp: fx.comp.on.then(|| CompressorState::new(&fx.comp, rate, channels)),
            deess: fx.deess.on.then(|| DeEsserState::new(&fx.deess, rate, channels)),
        }
    }

    fn tail(&mut self, block: &mut [f32]) {
        if let Some(comp) = self.comp.as_mut() {
            comp.process(block);
        }
        if let Some(deess) = self.deess.as_mut() {
            deess.process(block);
        }
    }

    /// Runs interleaved samples through the chain. With the noise reduction on the
    /// result lags the input by [`LATENCY`] frames until [`finish`](Self::finish).
    pub fn process(&mut self, mut block: Vec<f32>) -> Vec<f32> {
        if let Some(gate) = self.gate.as_mut() {
            gate.process(&mut block);
        }
        let mut block = match self.nr.as_mut() {
            Some(nr) => nr.process(&block),
            None => block,
        };
        self.tail(&mut block);
        block
    }

    /// The frames still inside the noise reduction.
    pub fn finish(&mut self) -> Vec<f32> {
        let Some(nr) = self.nr.as_mut() else {
            return Vec::new();
        };
        let mut block = nr.finish();
        self.tail(&mut block);
        block
    }
}

/// Puts a lane's chunk stream through its voice chain on a thread of its own and hands
/// back the processed stream. The result is the same length as the input and in
/// step with it: the noise reduction's delay is read ahead and flushed at the end.
pub fn spawn_stage(rx: Receiver<AudioChunk>, fx: &VoiceFx, rate: u32, channels: usize) -> Receiver<AudioChunk> {
    let (tx, out) = sync_channel(32);
    let mut chain = VoiceChain::new(fx, rate, channels);
    let ch = channels.max(1);
    let _ = thread::Builder::new().name("voice-fx".into()).spawn(move || {
        let mut next: Option<u64> = None;
        for chunk in rx {
            let start = *next.get_or_insert(chunk.start_sample);
            let samples = chain.process(chunk.samples);
            if samples.is_empty() {
                continue;
            }
            next = Some(start + (samples.len() / ch) as u64);
            if tx.send(AudioChunk { start_sample: start, samples }).is_err() {
                return;
            }
        }
        let tail = chain.finish();
        if !tail.is_empty() {
            let _ = tx.send(AudioChunk { start_sample: next.unwrap_or(0), samples: tail });
        }
    });
    out
}

// ---- learning from a lane ------------------------------------------------------------

/// What [`learn_noise`] found: the profile and the noise's own level.
#[derive(Clone, Debug, PartialEq)]
pub struct Learned {
    pub profile: Vec<f32>,
    /// The RMS level of the stretch the profile came from, dBFS.
    pub noise_db: f32,
    /// Where it came from, in seconds of lane (timeline) time.
    pub from: (f64, f64),
}

/// Learns a noise profile from `lane`: the explicit `range` (lane seconds), or by
/// default the quietest half second of its sound. The lane is read exactly as it plays
/// (cuts, speeds and the clip equalizers applied), at `sample_rate`.
pub fn learn_noise(
    project: &crate::Project,
    lane: crate::project::Lane,
    fps: f64,
    sample_rate: Option<u32>,
    range: Option<(f64, f64)>,
) -> crate::Result<Learned> {
    let sources = project.audio_sources();
    let open = |start_frame: u32| {
        let segs = project.lane_segments_from(lane, start_frame, fps);
        let eqs = project.lane_eqs_from(lane, start_frame, fps);
        let speeds = project.lane_speeds_from(lane, start_frame, fps);
        crate::audio::AudioSession::open_multi_streams_speed_at(&sources, &segs, &eqs, &speeds, sample_rate)
    };
    let no_sound = || "the lane has no sound to learn noise from";
    let mono_of = |samples: &[f32], channels: usize, out: &mut Vec<f32>| {
        out.clear();
        out.extend(samples.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
    };
    let mut mono = Vec::new();
    let (samples, from) = match range {
        None => {
            let lane_frames = project.lane(lane).iter().map(|c| c.end()).max().unwrap_or(0);
            let mut best: Option<(f64, Vec<f32>, f64)> = None;
            for (start, take) in noise_scan_plan(lane_frames, fps) {
                let Some((meta, rx)) = open(start)? else {
                    return Err(no_sound().into());
                };
                let channels = usize::from(meta.channels.max(1));
                let rate = f64::from(meta.sample_rate);
                let mut finder = QuietestFinder::new(meta.sample_rate);
                let mut left = (take * rate) as u64;
                for chunk in rx {
                    mono_of(&chunk.samples, channels, &mut mono);
                    let n = (mono.len() as u64).min(left) as usize;
                    finder.push(&mono[..n]);
                    left -= n as u64;
                    // Dropping the receiver ends the decode: the cost stops at the slice.
                    if left == 0 {
                        break;
                    }
                }
                let t0 = f64::from(start) / fps;
                if let Some((win, end)) = finder.best() {
                    let power = win.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / win.len() as f64;
                    if best.as_ref().is_none_or(|(b, ..)| power < *b) {
                        best = Some((power, win, t0 + end as f64 / rate));
                    }
                }
            }
            let (_, win, to) = best.ok_or(
                "no stretch of this lane is both quiet enough to be noise and long enough to learn from \
                 (half a second with no gap in it)",
            )?;
            (win, (to - AUTO_NOISE_SECS, to))
        }
        Some((a, b)) => {
            let Some((meta, rx)) = open(0)? else {
                return Err(no_sound().into());
            };
            let channels = usize::from(meta.channels.max(1));
            let rate = f64::from(meta.sample_rate);
            let mut picked: Vec<f32> = Vec::new();
            for chunk in rx {
                if chunk.start_sample as f64 / rate >= b {
                    break;
                }
                mono_of(&chunk.samples, channels, &mut mono);
                for (i, &s) in mono.iter().enumerate() {
                    let t = (chunk.start_sample as f64 + i as f64) / rate;
                    if t >= a && t < b {
                        picked.push(s);
                    }
                }
            }
            (picked, (a, b))
        }
    };
    let (profile, noise_db) = learn_profile(&samples)
        .ok_or("that stretch is too short to learn noise from (it needs at least 0.05 s of sound)")?;
    if noise_db < DIGITAL_SILENCE_DB {
        return Err("that stretch is digital silence: there is no noise in it to learn".into());
    }
    Ok(Learned { profile, noise_db, from })
}

/// The most sound the automatic noise search reads, seconds.
pub const SCAN_BUDGET_SECS: f64 = 60.0;
/// How many slices that budget is spread over.
const SCAN_SLICES: u32 = 24;

/// Where the automatic noise search looks: `(start frame, seconds to read)` slices. A lane
/// that fits the budget is read whole; a longer one is read in [`SCAN_SLICES`] slices spread
/// evenly from its start to its end, so the cost is the budget, not the lane's length.
pub fn noise_scan_plan(lane_frames: u32, fps: f64) -> Vec<(u32, f64)> {
    let total = f64::from(lane_frames) / fps;
    if !(fps.is_finite() && fps > 0.0) || total <= SCAN_BUDGET_SECS {
        return vec![(0, total.max(0.0) + 1.0)];
    }
    let take = SCAN_BUDGET_SECS / f64::from(SCAN_SLICES);
    let last = total - take;
    (0..SCAN_SLICES)
        .map(|k| ((last * f64::from(k) / f64::from(SCAN_SLICES - 1) * fps) as u32, take))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Deterministic white noise at `rms` (a plain LCG; no rand dependency).
    fn noise(n: usize, rms: f32, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                let mut sum = 0.0;
                for _ in 0..4 {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    sum += ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5;
                }
                // Sum of four uniforms in -0.5..0.5 has variance 4/12.
                sum * rms / (4.0f32 / 12.0).sqrt()
            })
            .collect()
    }

    fn sine(n: usize, hz: f32, amp: f32, start: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * hz * (i + start) as f32 / RATE as f32).sin()).collect()
    }

    fn rms_db(x: &[f32]) -> f32 {
        lin_to_db((x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt())
    }

    fn run(fx: &VoiceFx, input: &[f32], chunk: usize, ch: usize) -> Vec<f32> {
        let mut chain = VoiceChain::new(fx, RATE, ch);
        let mut out = Vec::new();
        for c in input.chunks(chunk * ch) {
            out.extend(chain.process(c.to_vec()));
        }
        out.extend(chain.finish());
        out
    }

    fn only_nr(strength: f32, profile: Vec<f32>) -> VoiceFx {
        VoiceFx { nr: NoiseReduction { on: true, strength, floor_db: -24.0, profile }, ..Default::default() }
    }

    #[test]
    fn noise_reduction_drops_noise_and_keeps_the_tone() {
        let n = RATE as usize * 3;
        let hiss = noise(n, db_to_lin(-30.0), 7);
        // Learned from a separate stretch of the same noise, as a real profile is.
        let (profile, level) = learn_profile(&noise(RATE as usize, db_to_lin(-30.0), 99)).unwrap();
        assert!((level + 30.0).abs() < 0.5, "learned level {level}");
        // First 1.5 s noise only, then a -12 dBFS tone over it.
        let tone = sine(n, 1000.0, db_to_lin(-12.0), 0);
        let input: Vec<f32> = (0..n).map(|i| hiss[i] + if i >= n / 2 { tone[i] } else { 0.0 }).collect();
        let fx = only_nr(0.8, profile);
        let out = run(&fx, &input, 1000, 1);
        assert_eq!(out.len(), input.len(), "same length");
        let (a, b) = (RATE as usize / 2, n / 2 - RATE as usize / 4);
        let before = rms_db(&input[a..b]);
        let after = rms_db(&out[a..b]);
        assert!(before - after >= 15.0, "noise-only stretch fell {:.1} dB ({before:.1} -> {after:.1})", before - after);
        // The tone's level, by correlation with the reference sine.
        let (c0, c1) = (n / 2 + RATE as usize / 2, n - RATE as usize / 4);
        let level_of = |x: &[f32]| {
            let (mut s, mut c) = (0.0f64, 0.0f64);
            for (i, &v) in x.iter().enumerate().take(c1).skip(c0) {
                let ph = 2.0 * std::f64::consts::PI * 1000.0 * i as f64 / RATE as f64;
                s += f64::from(v) * ph.sin();
                c += f64::from(v) * ph.cos();
            }
            (20.0 * (2.0 * (s * s + c * c).sqrt() / (c1 - c0) as f64).log10()) as f32
        };
        assert!((level_of(&out) - level_of(&input)).abs() < 1.0, "tone {} -> {}", level_of(&input), level_of(&out));
    }

    #[test]
    fn noise_reduction_is_aligned_with_its_input() {
        // An impulse comes out where it went in, not LATENCY frames later.
        let mut input = vec![0.0f32; 20_000];
        input[10_000] = 1.0;
        let out = run(&only_nr(0.5, vec![-100.0; PROFILE_BANDS]), &input, 777, 1);
        assert_eq!(out.len(), input.len());
        let peak = out.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert_eq!(peak, 10_000);
        assert!((out[10_000] - 1.0).abs() < 1e-3, "{}", out[10_000]);
    }

    #[test]
    fn the_gate_closes_on_noise_below_its_threshold() {
        let gate = Gate { on: true, threshold_db: -35.0, range_db: -40.0, release_ms: 30.0, ..Gate::DEFAULT };
        let fx = VoiceFx { gate, ..Default::default() };
        let quiet = noise(RATE as usize, db_to_lin(-50.0), 3);
        let loud = sine(RATE as usize / 2, 440.0, 0.3, 0);
        let mut input = loud.clone();
        input.extend(&quiet);
        input.extend(&loud);
        let out = run(&fx, &input, 512, 1);
        let h = RATE as usize / 2;
        // Open on the tone, unchanged.
        assert!((rms_db(&out[h / 2..h]) - rms_db(&input[h / 2..h])).abs() < 0.2);
        // Closed on the noise once the release has run: 40 dB down.
        let (a, b) = (h + RATE as usize * 2 / 5, h + RATE as usize);
        let drop = rms_db(&input[a..b]) - rms_db(&out[a..b]);
        assert!(drop > 38.0, "closed by {drop:.1} dB");
        // Open again for the second tone.
        let t = 2 * h + RATE as usize;
        assert!((rms_db(&out[t - h / 2..t]) - rms_db(&input[t - h / 2..t])).abs() < 0.5);
    }

    #[test]
    fn the_gate_holds_before_it_closes() {
        let gate = Gate { on: true, threshold_db: -30.0, hold_ms: 200.0, release_ms: 5.0, ..Gate::DEFAULT };
        let fx = VoiceFx { gate, ..Default::default() };
        let mut input = sine(RATE as usize / 4, 440.0, 0.3, 0);
        input.extend(vec![0.0; RATE as usize / 2]);
        let mut chain = VoiceChain::new(&fx, RATE, 1);
        let mut gains = Vec::new();
        for c in input.chunks(480) {
            chain.process(c.to_vec());
            gains.push(chain.gate.as_ref().unwrap().gain());
        }
        // 100 ms after the tone ended it is still open, 400 ms after it is shut.
        let at = |ms: usize| gains[(RATE as usize / 4 + RATE as usize * ms / 1000) / 480];
        assert!(at(100) > 0.99, "{}", at(100));
        assert!(at(400) < 0.2, "{}", at(400));
    }

    #[test]
    fn the_compressor_follows_its_static_curve() {
        let comp = Compressor { on: true, threshold_db: -20.0, ratio: 4.0, knee_db: 0.0, auto_makeup: false, ..Compressor::DEFAULT };
        let fx = VoiceFx { comp, ..Default::default() };
        for in_db in [-40.0f32, -30.0, -20.0, -14.0, -8.0, -3.0] {
            // A sine whose RMS is in_db.
            let amp = db_to_lin(in_db) * std::f32::consts::SQRT_2;
            let input = sine(RATE as usize * 2, 997.0, amp, 0);
            let out = run(&fx, &input, 4096, 1);
            let t = RATE as usize..RATE as usize * 2;
            let got = rms_db(&out[t.clone()]) - rms_db(&input[t]);
            let want = comp.curve(in_db) - in_db;
            assert!((got - want).abs() < 0.5, "{in_db} dB in: reduced {got:.2}, curve says {want:.2}");
        }
        // With a knee, the curve is continuous through the threshold and below the hard one.
        let knee = Compressor { knee_db: 10.0, ..comp };
        assert!(knee.curve(-20.0) < -20.0 && knee.curve(-20.0) > comp.curve(-20.0) - 2.0);
        assert!((knee.curve(-26.0) - -26.0).abs() < 1e-5, "below the knee is untouched");
        assert!(comp.curve(-20.0).is_finite(), "a hard knee exactly at the threshold");
    }

    /// The envelopes and the overlap state ride across chunks: the chain's output does not
    /// depend on how the stream was cut into blocks.
    #[test]
    fn chunk_size_does_not_change_the_output() {
        let n = RATE as usize * 3;
        let mut mono = noise(n, db_to_lin(-40.0), 5);
        for (i, s) in sine(n, 700.0, 0.2, RATE as usize).iter().enumerate() {
            mono[i] += s;
        }
        for (i, s) in sine(n, 6500.0, 0.1, RATE as usize * 2).iter().enumerate() {
            mono[i] += s;
        }
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s * 0.7]).collect();
        let (profile, level) = learn_profile(&noise(RATE as usize, db_to_lin(-40.0), 11)).unwrap();
        let fx = VoiceFx::clean_voice(Strength::Strong, profile, Some(level));
        assert!(fx.gate.on && fx.nr.on && fx.comp.on && fx.deess.on);
        let whole = run(&fx, &stereo, n, 2);
        assert_eq!(whole.len(), stereo.len());
        for chunk in [1, 77, 480, 960, 1023, 4093, 20_001] {
            let odd = run(&fx, &stereo, chunk, 2);
            assert_eq!(odd.len(), whole.len(), "chunk {chunk}");
            let worst = whole.iter().zip(&odd).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "chunk {chunk}: the output moved by {worst}");
        }
    }

    #[test]
    fn the_noise_scan_reads_a_bounded_amount_however_long_the_lane() {
        for hours in [0.5f64, 1.0, 10.0] {
            let frames = (hours * 3600.0 * 30.0) as u32;
            let plan = noise_scan_plan(frames, 30.0);
            let read: f64 = plan.iter().map(|p| p.1).sum();
            assert!(read <= SCAN_BUDGET_SECS + 1e-9, "{hours} h reads {read} s");
            assert_eq!(plan.first().unwrap().0, 0);
            let (last, take) = *plan.last().unwrap();
            assert!(f64::from(last) / 30.0 + take <= hours * 3600.0 + 0.1, "the last slice ends inside the lane");
            assert!(plan.windows(2).all(|w| w[0].0 < w[1].0), "slices run in order");
        }
        let short = noise_scan_plan(300, 30.0);
        assert_eq!(short.len(), 1, "a short lane is read whole");
    }

    #[test]
    fn auto_makeup_brings_full_scale_back() {
        let comp = Compressor { on: true, threshold_db: -20.0, ratio: 4.0, knee_db: 0.0, auto_makeup: true, ..Compressor::DEFAULT };
        assert!((comp.makeup() - 15.0).abs() < 1e-5);
        assert!((comp.curve(0.0) + comp.makeup()).abs() < 1e-5, "0 dB in is 0 dB out");
    }

    #[test]
    fn the_de_esser_cuts_six_khz_and_leaves_one_khz() {
        let deess = DeEsser { on: true, threshold_db: -30.0, amount: 0.8, ..DeEsser::DEFAULT };
        let fx = VoiceFx { deess, ..Default::default() };
        let n = RATE as usize;
        for (hz, expect_cut) in [(6000.0f32, true), (1000.0, false)] {
            let input = sine(n, hz, 0.3, 0);
            let out = run(&fx, &input, 480, 1);
            let t = n / 2..n;
            let cut = rms_db(&input[t.clone()]) - rms_db(&out[t]);
            if expect_cut {
                assert!(cut > 8.0, "{hz} Hz cut {cut:.1} dB");
            } else {
                assert!(cut.abs() < 0.1, "{hz} Hz changed by {cut:.2} dB");
            }
        }
        // A 6 kHz burst inside a 1 kHz tone comes out quieter, the tone around it does not.
        let mut input = sine(n, 1000.0, 0.2, 0);
        for (i, b) in sine(4800, 6000.0, 0.3, 0).iter().enumerate() {
            input[n / 2 + i] += b;
        }
        let out = run(&fx, &input, 480, 1);
        let band_before = rms_db(&input[n / 2 + 2000..n / 2 + 4800]);
        let band_after = rms_db(&out[n / 2 + 2000..n / 2 + 4800]);
        assert!(band_before - band_after > 3.0, "burst {band_before:.1} -> {band_after:.1}");
        assert!((rms_db(&out[n / 4..n / 2 - 100]) - rms_db(&input[n / 4..n / 2 - 100])).abs() < 0.1);
    }

    #[test]
    fn silence_stays_silent_and_finite() {
        let fx = VoiceFx::clean_voice(Strength::Strong, vec![-60.0; PROFILE_BANDS], Some(-50.0));
        // A burst, then ten seconds of nothing: no NaN, no denormal left circulating.
        let mut input = noise(RATE as usize / 4, 0.3, 5);
        input.extend(vec![0.0; RATE as usize * 10]);
        let out = run(&fx, &input, 1024, 1);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|s| s.is_finite()));
        let tail = &out[out.len() - RATE as usize..];
        assert!(tail.iter().all(|s| *s == 0.0 || s.abs() >= f32::MIN_POSITIVE), "no denormals");
        assert!(tail.iter().all(|s| s.abs() < 1e-9), "silence in, silence out");
        let mut chain = VoiceChain::new(&fx, RATE, 2);
        assert!(chain.process(vec![0.0; 9600]).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn the_output_does_not_depend_on_where_the_blocks_are_cut() {
        let profile = learn_profile(&noise(RATE as usize, 0.01, 1)).unwrap().0;
        let fx = VoiceFx::clean_voice(Strength::Medium, profile, Some(-40.0));
        // One continuous 300 Hz sine whose level steps every half second (each step lands on a zero crossing).
        let mut input = Vec::new();
        for i in 0..4 {
            let amp = if i % 2 == 0 { 0.4 } else { 0.01 };
            input.extend(sine(RATE as usize / 2, 300.0, amp, i * RATE as usize / 2));
        }
        let whole = run(&fx, &input, input.len(), 1);
        for chunk in [1usize, 37, 480, 513, 4410] {
            let cut = run(&fx, &input, chunk, 1);
            assert_eq!(cut.len(), whole.len());
            let worst = cut.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-5, "chunk {chunk}: differs by {worst}");
        }
        // And no step bigger than the signal's own anywhere (a 0.4 sine at 300 Hz steps ~0.016).
        let step = whole.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(step < 0.1, "largest step {step}");
    }

    #[test]
    fn stereo_channels_share_one_gain() {
        let (profile, _) = learn_profile(&noise(RATE as usize, 0.02, 4)).unwrap();
        let fx = only_nr(0.9, profile);
        let mono = noise(RATE as usize, 0.05, 11);
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s * 0.5]).collect();
        let out = run(&fx, &stereo, 960, 2);
        assert_eq!(out.len(), stereo.len());
        for f in out.as_chunks::<2>().0.iter().skip(5000).take(20000) {
            assert!((f[1] - f[0] * 0.5).abs() < 1e-5, "the image moved: {} {}", f[0], f[1]);
        }
    }

    #[test]
    fn the_quietest_finder_skips_gaps_and_finds_the_pause() {
        let mut finder = QuietestFinder::new(RATE);
        let r = RATE as usize;
        let mut sound = noise(r, 0.2, 1); // speech-ish
        sound.extend(vec![0.0; r]); // a gap
        sound.extend(noise(r, 0.01, 2)); // the pause
        sound.extend(noise(r, 0.2, 3));
        for c in sound.chunks(777) {
            finder.push(c);
        }
        let (best, end) = finder.best().unwrap();
        assert_eq!(best.len(), RATE as usize / 2);
        assert!(end > 2 * r as u64 && end <= 3 * r as u64, "ended at {end}");
        let level = rms_db(&best);
        assert!((level + 40.0).abs() < 1.5, "found {level:.1} dB");
    }

    #[test]
    fn clean_voice_presets_are_in_range_and_grow_with_strength() {
        let profile = vec![-60.0; PROFILE_BANDS];
        let mut last = 0.0;
        for s in [Strength::Light, Strength::Medium, Strength::Strong] {
            let fx = VoiceFx::clean_voice(s, profile.clone(), Some(-55.0));
            assert_eq!(fx.clamped().as_ref(), Some(&fx), "{s:?} is inside every range");
            assert!(fx.is_active() && fx.nr_active());
            assert!(fx.nr.strength > last);
            last = fx.nr.strength;
        }
        let none = VoiceFx::clean_voice(Strength::Medium, Vec::new(), None);
        assert!(!none.nr.on, "no profile, no reduction");
        assert!(none.gate.on && none.comp.on && none.deess.on);
        assert_eq!(none.summary(), "gate -45 dB, comp -24 dB 3:1, de-ess -30 dB");
    }

    /// `cargo test --release -p engine voice_speed -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn voice_speed() {
        let (profile, _) = learn_profile(&noise(RATE as usize, 0.01, 1)).unwrap();
        let fx = VoiceFx::clean_voice(Strength::Strong, profile, Some(-45.0));
        let secs = 20;
        let input: Vec<f32> = (0..RATE as usize * secs)
            .flat_map(|i| {
                let s = 0.2 * (i as f32 * 0.03).sin() + 0.01 * ((i * 7919 % 1000) as f32 / 500.0 - 1.0);
                [s, s]
            })
            .collect();
        let t = std::time::Instant::now();
        let out = run(&fx, &input, 1024, 2);
        let el = t.elapsed().as_secs_f64();
        println!(
            "voice chain, 1 lane 48 kHz stereo: {secs} s in {el:.3} s = {:.0}x realtime ({} frames)",
            secs as f64 / el,
            out.len() / 2
        );
    }
}
