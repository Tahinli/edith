//! The master limiter: one ceiling the whole mix is held under, applied where
//! the lanes are summed and nowhere else ([`crate::audio::mix`]).
//!
//! A mix is an add, and two lanes at full scale add past full scale: what used
//! to happen there was a hard `clamp`, which is a squared-off waveform and the
//! worst-sounding thing an editor can do to a hot timeline. This holds the sum
//! down *before* it reaches the clamp instead, and the clamp stays behind it as
//! the backstop it always was.
//!
//! Per sample, no FFT, no allocation while it runs:
//!
//! * **Lookahead.** The mix is written into a delay line [`LOOKAHEAD_MS`] long
//!   and read out of it that much later, so the gain a peak needs is already in
//!   force by the time the peak itself is emitted. No attack overshoot -- the
//!   first sample over the ceiling is the first sample held under it, which is
//!   what makes the peak of an exported file a *measurement* and not a hope.
//! * **Linked channels.** One gain for the frame, off the loudest channel in
//!   it: a per-channel gain would move the stereo image every time one side
//!   peaked.
//! * **Soft knee.** [`KNEE_DB`] wide and centred on the ceiling, so the gain
//!   comes in gradually instead of switching on at one sample and off at the
//!   next. The knee's own curve never lets the output past the ceiling (at the
//!   centre it is already `KNEE_DB/8` under it), which is what keeps the
//!   guarantee a hard knee would give.
//! * **Release**, not a per-sample snap back: `RELEASE_MS` of exponential
//!   recovery, so one loud transient does not pump the whole bar around it.
//!
//! The latency is *taken out again*, not paid: the first `lookahead` frames of
//! output -- the empty delay line -- are dropped, so what comes out is sample
//! aligned with what went in and the sound stays locked to the picture. What
//! that costs is the last couple of milliseconds of the timeline's tail, which
//! is the trade every lookahead limiter makes.

/// How far ahead the gain looks. 2 ms is ~96 frames at 48 kHz: long enough to
/// ride a transient in smoothly, short enough that the tail it eats is nothing.
const LOOKAHEAD_MS: f32 = 2.0;
/// How wide the knee is, centred on the ceiling.
const KNEE_DB: f32 = 6.0;
/// How fast the gain comes back once the loud part is over.
const RELEASE_MS: f32 = 120.0;

/// What a project's master limiter is set to: a ceiling in dBFS and whether it
/// is in circuit at all. Off by default -- a timeline nobody has asked to limit
/// mixes exactly as it always did, down to the bit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limiter {
    /// The level nothing may pass, in dBFS (0 dBFS is full scale).
    pub ceiling_db: f32,
    pub on: bool,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            ceiling_db: -1.0,
            on: false,
        }
    }
}

impl Limiter {
    /// The ceilings a front-end may set, and what a file may name: a limiter
    /// above full scale would limit nothing, and one 24 dB down is already a
    /// fader.
    pub const MIN_DB: f32 = -24.0;
    pub const MAX_DB: f32 = 0.0;

    /// The setting with `ceiling_db` put inside the range above, which is what
    /// a nudge and a parsed line both go through. Not finite means the default.
    pub fn with_ceiling(self, ceiling_db: f32) -> Self {
        Self {
            ceiling_db: match ceiling_db.is_finite() {
                true => ceiling_db.clamp(Self::MIN_DB, Self::MAX_DB),
                false => Self::default().ceiling_db,
            },
            ..self
        }
    }

    /// Whether it is doing anything to the sound -- what an export asks before
    /// it decides a packet copy would be a lie.
    pub fn is_active(self) -> bool {
        self.on
    }
}

/// dBFS as an amplitude. `db_to_linear(0.0)` is exactly 1.0, which is what
/// keeps a flat setting a bit-exact passthrough.
pub fn db_to_linear(db: f32) -> f32 {
    match db == 0.0 {
        true => 1.0,
        false => 10f32.powf(db / 20.0),
    }
}

/// One running limiter: the delay line, the peak window and the gain.
pub struct LimiterState {
    channels: usize,
    /// The delayed mix, one frame per slot, `lookahead + 1` frames round.
    line: Vec<f32>,
    /// The loudest channel of each frame in `line`, same indexing.
    peaks: Vec<f32>,
    /// Where the next frame is written; the frame read out is the one after it.
    at: usize,
    /// Frames still to be swallowed before anything is emitted -- the empty
    /// delay line, which is how the latency is paid back (module docs).
    warm: usize,
    gain: f32,
    ceiling: f32,
    /// Where the knee starts, as an amplitude: under this the fast path runs
    /// and no logarithm is taken.
    knee_floor: f32,
    release: f32,
}

impl LimiterState {
    pub fn new(params: &Limiter, sample_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let frames = ((LOOKAHEAD_MS / 1000.0 * sample_rate as f32) as usize).max(1) + 1;
        let ceiling_db = params.with_ceiling(params.ceiling_db).ceiling_db;
        Self {
            channels,
            line: vec![0.0; frames * channels],
            peaks: vec![0.0; frames],
            at: 0,
            warm: frames - 1,
            gain: 1.0,
            ceiling: db_to_linear(ceiling_db),
            knee_floor: db_to_linear(ceiling_db - KNEE_DB / 2.0),
            // One time constant per sample: the gain covers 63% of the way back
            // in `RELEASE_MS`.
            release: 1.0 - (-1000.0 / (RELEASE_MS * sample_rate as f32)).exp(),
        }
    }

    /// Rewrites `buf` -- interleaved frames -- as the limited mix. Shorter than
    /// what came in by the delay line's length, once, at the first call.
    pub fn process(&mut self, buf: &mut Vec<f32>) {
        let channels = self.channels;
        let frames = buf.len() / channels;
        let mut out: Vec<f32> = Vec::with_capacity(buf.len());
        for f in 0..frames {
            let frame = &buf[f * channels..(f + 1) * channels];
            let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            self.line[self.at * channels..(self.at + 1) * channels].copy_from_slice(frame);
            self.peaks[self.at] = peak;
            self.at = (self.at + 1) % self.peaks.len();

            // Every frame still in the line, the one about to leave included:
            // the gain below is therefore never larger than what the *emitted*
            // frame needs, which is the whole no-overshoot guarantee.
            let window = self.peaks.iter().fold(0.0f32, |m, &p| m.max(p));
            let want = match window <= self.knee_floor {
                true => 1.0,
                false => self.reduction(window),
            };
            // Up is the release, down is instant -- and instant is safe here
            // only because the frame it is for has not been emitted yet.
            self.gain += (want - self.gain).max(0.0) * self.release;
            self.gain = self.gain.min(want);

            if self.warm > 0 {
                self.warm -= 1;
                continue;
            }
            for c in 0..channels {
                out.push(self.line[self.at * channels + c] * self.gain);
            }
        }
        *buf = out;
    }

    /// The gain the loudest frame in the window needs: 1.0 under the knee, the
    /// knee's own curve across it, `ceiling/peak` above it.
    fn reduction(&self, peak: f32) -> f32 {
        let level = 20.0 * peak.log10();
        let ceiling = 20.0 * self.ceiling.log10();
        let over = level - ceiling;
        let out = match over >= KNEE_DB / 2.0 {
            true => ceiling,
            // The cookbook's quadratic knee. At `over == 0` this is already
            // `KNEE_DB/8` under the ceiling and it never crosses it, so nothing
            // the knee passes needs a clamp behind it.
            false => level - (over + KNEE_DB / 2.0).powi(2) / (2.0 * KNEE_DB),
        };
        10f32.powf((out - level) / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The promise the whole file exists for: a sum well past full scale comes
    /// out under the ceiling, sample for sample, and the quiet part of the same
    /// stream is left where it was.
    #[test]
    fn a_hot_mix_comes_out_under_the_ceiling() {
        let rate = 48_000;
        let params = Limiter {
            ceiling_db: -3.0,
            on: true,
        };
        let mut state = LimiterState::new(&params, rate, 2);
        // Half a second of quiet, then half a second at 4x full scale: the
        // join is where an attack overshoot would show.
        let mut buf: Vec<f32> = (0..rate)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let amp = match i < rate / 2 {
                    true => 0.05,
                    false => 4.0,
                };
                let s = amp * (t * 440.0 * std::f32::consts::TAU).sin();
                [s, s]
            })
            .collect();
        let quiet_in = buf[..rate as usize / 2].to_vec();
        state.process(&mut buf);
        let ceiling = db_to_linear(-3.0);
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= ceiling + 1e-6,
            "peak {peak} passed the {ceiling} ceiling"
        );
        // ...and it is not simply silence: the loud half is right at the wall.
        assert!(
            peak > ceiling * 0.99,
            "peak {peak} is nowhere near the wall"
        );
        // The quiet half is untouched, bar the frames the lookahead eats and
        // the ones where the gain has already started coming down for the loud
        // part that is about to arrive.
        let head = quiet_in.len() / 2;
        for (a, b) in buf[..head].iter().zip(&quiet_in[..head]) {
            assert!((a - b).abs() < 1e-6, "{a} != {b} under the knee");
        }
        // The delay line is paid back, not carried: the output is shorter by
        // exactly the lookahead and nothing else.
        assert_eq!(buf.len(), rate as usize * 2 - (state.peaks.len() - 1) * 2);
    }

    /// Flat settings are a passthrough, which is what the export invariant
    /// rests on: 0 dB is exactly 1.0, never 0.9999999.
    #[test]
    fn zero_db_is_exactly_unity() {
        assert_eq!(db_to_linear(0.0), 1.0);
        assert!((db_to_linear(-6.0) - 0.5011872).abs() < 1e-6);
        assert!(!Limiter::default().is_active(), "off until asked for");
        assert_eq!(
            Limiter::default().with_ceiling(-99.0).ceiling_db,
            Limiter::MIN_DB
        );
        assert_eq!(
            Limiter::default().with_ceiling(f32::NAN).ceiling_db,
            Limiter::default().ceiling_db
        );
    }
}
