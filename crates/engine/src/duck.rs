//! Auto-ducking: one audio lane drops in level while another lane (the key,
//! usually dialogue) is loud, so a music bed sits under speech.
//!
//! The shared DSP. [`Ducker`] is one target lane's gain reduction and is run by
//! the mixer ([`crate::audio`]) once per block, on the post-gain signal of the
//! key lane. Playback and export both sum through that mixer, so the two cannot
//! disagree about where a duck lands.
//!
//! The detector is the key's RMS over a 10 ms window (the channels are averaged
//! into one power per frame first). The reduction is a *linear ramp in dB*:
//! while the key is at or above the threshold the gain falls towards
//! `depth_db` at `|depth_db| / attack_ms` per millisecond, so it has reached
//! the full depth exactly `attack_ms` after the key arrives; once the key drops
//! away it climbs back to 0 dB over `release_ms` the same way. A linear-in-dB
//! ramp keeps those two times literal, which a one-pole would not.

/// The deepest a duck may cut, in dB.
pub const MIN_DEPTH_DB: f32 = -40.0;
/// The shallowest: 0 dB, which is a duck that does nothing.
pub const MAX_DEPTH_DB: f32 = 0.0;
/// The quietest key level a threshold may name, in dBFS.
pub const MIN_THRESHOLD_DB: f32 = -90.0;
/// The loudest: a key at full scale is always above it, so the duck never fires.
pub const MAX_THRESHOLD_DB: f32 = 0.0;
/// The slowest attack, in ms. The fastest is [`MIN_TIME_MS`].
pub const MAX_ATTACK_MS: f32 = 2000.0;
/// The slowest release, in ms.
pub const MAX_RELEASE_MS: f32 = 10000.0;
/// The shortest attack or release a ramp is made of, in ms: a zero would be a
/// step, which divides by nothing and clicks.
pub const MIN_TIME_MS: f32 = 1.0;
/// The window the key's level is measured over, in ms.
pub const DETECT_MS: f32 = 10.0;

/// One audio lane's duck, as the project holds it ([`crate::Project::set_lane_duck`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Duck {
    /// The key lane: the 0-based position of an audio lane in the order
    /// [`crate::Project::audio_lanes`] lists them, which is the order the mix
    /// sums them in. Never the duck's own lane.
    pub under: usize,
    /// How far the lane drops while the key is loud, in dB (`-40..=0`).
    pub depth_db: f32,
    /// The key level, in dBFS, that triggers the duck (`-90..=0`).
    pub threshold_db: f32,
    /// How long the drop takes to reach full depth, in ms.
    pub attack_ms: f32,
    /// How long the recovery to 0 dB takes, in ms.
    pub release_ms: f32,
}

impl Duck {
    /// The threshold a duck starts at, in dBFS.
    pub const DEFAULT_THRESHOLD_DB: f32 = -40.0;
    /// The attack a duck starts at, in ms.
    pub const DEFAULT_ATTACK_MS: f32 = 80.0;
    /// The release a duck starts at, in ms.
    pub const DEFAULT_RELEASE_MS: f32 = 400.0;

    /// A duck under `under` at `depth_db`, with the default threshold and times.
    pub fn new(under: usize, depth_db: f32) -> Self {
        Self {
            under,
            depth_db,
            threshold_db: Self::DEFAULT_THRESHOLD_DB,
            attack_ms: Self::DEFAULT_ATTACK_MS,
            release_ms: Self::DEFAULT_RELEASE_MS,
        }
    }

    /// The same duck with every number clamped into its range. `None` when a
    /// number is not finite: there is no clamp for a NaN that means something.
    pub fn clamped(self) -> Option<Self> {
        let finite = [
            self.depth_db,
            self.threshold_db,
            self.attack_ms,
            self.release_ms,
        ]
        .iter()
        .all(|v| v.is_finite());
        finite.then_some(Self {
            under: self.under,
            depth_db: self.depth_db.clamp(MIN_DEPTH_DB, MAX_DEPTH_DB),
            threshold_db: self.threshold_db.clamp(MIN_THRESHOLD_DB, MAX_THRESHOLD_DB),
            attack_ms: self.attack_ms.clamp(MIN_TIME_MS, MAX_ATTACK_MS),
            release_ms: self.release_ms.clamp(MIN_TIME_MS, MAX_RELEASE_MS),
        })
    }
}

/// One target lane's duck while the mix runs: the key's detector and the
/// reduction it has reached. State carries from block to block, so a block
/// boundary is not a step in the gain.
#[derive(Clone, Debug)]
pub struct Ducker {
    duck: Duck,
    /// The key's per-frame power (channel mean of the squares) over the last
    /// [`DETECT_MS`], kept as a ring so the window is exact.
    window: Vec<f64>,
    pos: usize,
    /// The sum of `window`, in f64 so that a long run does not drift.
    sum: f64,
    /// The reduction now, in dB: `0.0` at rest, down to `depth_db` at most.
    gain_db: f32,
    /// How far the reduction may move per frame, in dB.
    step_attack: f32,
    step_release: f32,
}

impl Ducker {
    /// A ducker at rest (0 dB) for `duck`, at `rate` Hz.
    pub fn new(duck: &Duck, rate: u32) -> Self {
        let frames = ((DETECT_MS * 0.001 * rate as f32).round() as usize).max(1);
        let mut ducker = Self {
            duck: *duck,
            window: vec![0.0; frames],
            pos: 0,
            sum: 0.0,
            gain_db: 0.0,
            step_attack: 0.0,
            step_release: 0.0,
        };
        ducker.retune(duck, rate);
        ducker
    }

    /// Takes a new duck's numbers, keeping the detector and the reduction where
    /// they are: a fader moved under the mix is a change of slope, not a click.
    pub fn retune(&mut self, duck: &Duck, rate: u32) {
        self.duck = *duck;
        let rate = rate.max(1) as f32;
        let depth = self.duck.depth_db.abs();
        self.step_attack = depth / (self.duck.attack_ms.max(MIN_TIME_MS) * 0.001 * rate);
        self.step_release = depth / (self.duck.release_ms.max(MIN_TIME_MS) * 0.001 * rate);
    }

    /// The reduction now, in dB (`0.0` or negative).
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Ducks `target` by the key's level. `key` and `target` are interleaved
    /// with `channels` channels and are read frame by frame over the shorter of
    /// the two. The key is only read: the caller passes the key's signal as it
    /// was before any duck touched it.
    pub fn process(&mut self, key: &[f32], target: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let frames = key.len().min(target.len()) / channels;
        let threshold = self.duck.threshold_db;
        let depth = self.duck.depth_db;
        for f in 0..frames {
            let base = f * channels;
            let frame = &key[base..base + channels];
            let power = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>()
                / channels as f64;
            // The window's oldest frame leaves and this one takes its place.
            self.sum += power - self.window[self.pos];
            self.window[self.pos] = power;
            self.pos = (self.pos + 1) % self.window.len();
            let mean = (self.sum.max(0.0)) / self.window.len() as f64;
            let level_db = (10.0 * (mean + 1e-12).log10()) as f32;

            let want = if level_db >= threshold { depth } else { 0.0 };
            if want < self.gain_db {
                self.gain_db = (self.gain_db - self.step_attack).max(want);
            } else if want > self.gain_db {
                self.gain_db = (self.gain_db + self.step_release).min(want);
            }
            // At rest the samples are left exactly as they were.
            if self.gain_db != 0.0 {
                let g = 10f32.powf(self.gain_db / 20.0);
                for s in &mut target[base..base + channels] {
                    *s *= g;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// `ms` of a stereo tone at `amp`, interleaved.
    fn tone(amp: f32, ms: f32) -> Vec<f32> {
        let frames = (ms * 0.001 * RATE as f32) as usize;
        (0..frames)
            .flat_map(|i| {
                let s = amp * (i as f32 * 0.05).sin();
                [s, s]
            })
            .collect()
    }

    fn silence(ms: f32) -> Vec<f32> {
        vec![0.0; (ms * 0.001 * RATE as f32) as usize * 2]
    }

    fn db(lin: f32) -> f32 {
        20.0 * lin.log10()
    }

    /// The gain the target's first frame of `[at_ms..]` carries, in dB, read off
    /// a constant-amplitude target so the ratio is the gain.
    fn gain_at(ducker: &mut Ducker, key: &[f32], ms: f32) -> f32 {
        let ones = vec![1.0f32; key.len()];
        let mut out = ones.clone();
        ducker.process(key, &mut out, 2);
        let frame = ((ms * 0.001 * RATE as f32) as usize).min(out.len() / 2 - 1) * 2;
        db(out[frame])
    }

    #[test]
    fn silence_under_the_key_never_reduces() {
        let duck = Duck::new(0, -12.0);
        let mut ducker = Ducker::new(&duck, RATE);
        let key = silence(500.0);
        let mut target = tone(0.5, 500.0);
        let before = target.clone();
        ducker.process(&key, &mut target, 2);
        assert_eq!(target, before, "a silent key leaves the lane bit-exact");
        assert_eq!(ducker.gain_db(), 0.0);
    }

    #[test]
    fn a_key_below_threshold_never_reduces() {
        let duck = Duck::new(0, -12.0);
        let mut ducker = Ducker::new(&duck, RATE);
        // -60 dBFS, under the -40 dBFS default threshold.
        let key = tone(0.001, 300.0);
        let mut target = tone(0.5, 300.0);
        let before = target.clone();
        ducker.process(&key, &mut target, 2);
        assert_eq!(target, before);
    }

    #[test]
    fn a_loud_key_reaches_the_depth_within_the_attack_time() {
        let duck = Duck::new(0, -12.0); // attack 80 ms by default
        let mut ducker = Ducker::new(&duck, RATE);
        let key = tone(0.5, 300.0); // -6 dBFS, well over the threshold at once
        // Halfway through the attack the reduction is half the depth...
        let mid = gain_at(&mut ducker, &key, 40.0);
        assert!((mid + 6.0).abs() < 0.3, "half way is half the depth, got {mid}");
        // ...and at the end of it, the full depth, which it stays at.
        let mut ducker = Ducker::new(&duck, RATE);
        let end = gain_at(&mut ducker, &key, 80.0);
        assert!((end + 12.0).abs() < 0.3, "the depth within attack_ms, got {end}");
        assert!((ducker.gain_db() + 12.0).abs() < 1e-3, "and it holds there");
    }

    #[test]
    fn the_reduction_releases_back_to_unity() {
        let duck = Duck::new(0, -12.0); // release 400 ms
        let mut ducker = Ducker::new(&duck, RATE);
        let loud = tone(0.5, 300.0);
        let mut target = vec![1.0f32; loud.len()];
        ducker.process(&loud, &mut target, 2);
        assert!((ducker.gain_db() + 12.0).abs() < 1e-3, "down first");
        // Silence for the detector's 10 ms to let go, then half the release.
        let quiet = silence(10.0 + 200.0);
        let mut target = vec![1.0f32; quiet.len()];
        ducker.process(&quiet, &mut target, 2);
        assert!(
            (ducker.gain_db() + 6.0).abs() < 0.3,
            "half way back after half the release, got {}",
            ducker.gain_db()
        );
        let rest = silence(220.0);
        let mut target = vec![1.0f32; rest.len()];
        ducker.process(&rest, &mut target, 2);
        assert_eq!(ducker.gain_db(), 0.0, "back at unity after the release");
        assert_eq!(target.last().copied(), Some(1.0), "and the samples are untouched again");
    }

    #[test]
    fn retune_keeps_the_reduction_it_has_reached() {
        let duck = Duck::new(0, -12.0);
        let mut ducker = Ducker::new(&duck, RATE);
        let loud = tone(0.5, 200.0);
        let mut target = vec![1.0f32; loud.len()];
        ducker.process(&loud, &mut target, 2);
        let reached = ducker.gain_db();
        ducker.retune(&Duck::new(0, -6.0), RATE);
        assert_eq!(ducker.gain_db(), reached, "a change of depth is not a click");
    }

    #[test]
    fn clamped_bounds_every_number_and_refuses_nan() {
        let wild = Duck {
            under: 1,
            depth_db: -90.0,
            threshold_db: 20.0,
            attack_ms: 0.0,
            release_ms: 1e9,
        };
        let tame = wild.clamped().expect("finite");
        assert_eq!(tame.depth_db, MIN_DEPTH_DB);
        assert_eq!(tame.threshold_db, MAX_THRESHOLD_DB);
        assert_eq!(tame.attack_ms, MIN_TIME_MS);
        assert_eq!(tame.release_ms, MAX_RELEASE_MS);
        assert!(Duck { depth_db: f32::NAN, ..wild }.clamped().is_none());
    }
}
