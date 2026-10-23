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
//! * **True peak.** The ceiling is a true-peak ceiling (dBTP), so the peak
//!   detector is the one the meter measures with ([`crate::loudness::TruePeak`],
//!   the same filter), 8x oversampled rather than the standard's 4x: a 4x grid
//!   read up to 1.6 dB under the waveform on dense high-frequency material, and
//!   the export then overshot the ceiling by that much. A sample peak limiter
//!   lets a waveform overshoot between its samples; this one holds the
//!   reconstructed waveform instead.
//! * **Lookahead.** The mix is written into a delay line [`LOOKAHEAD_MS`] long
//!   and read out of it that much later, so the gain a peak needs is already in
//!   force by the time the peak itself is emitted. No attack overshoot -- the
//!   first sample over the ceiling is the first sample held under it, which is
//!   what makes the peak of an exported file a *measurement* and not a hope.
//!   The detector sees its peak `TP_TAPS / 2` frames before the samples it
//!   describes, so the lookahead is what lets it act in time.
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
//! aligned with what went in and the sound stays locked to the picture. The
//! last `lookahead` frames are still in the line when the mix runs out; [`drain`]
//! plays them out, so the tail is kept and the cut at the end of the stream is
//! limited like everything before it.

use crate::loudness::{TP_TAPS, TruePeak};

/// How far ahead the gain looks. 2 ms is ~96 frames at 48 kHz: long enough to
/// ride a transient in smoothly, short enough that the tail it eats is nothing.
/// Never less than the detector's window ([`TP_TAPS`] frames), whatever the rate.
const LOOKAHEAD_MS: f32 = 2.0;
/// How wide the knee is, centred on the ceiling.
const KNEE_DB: f32 = 6.0;
/// How fast the gain comes back once the loud part is over.
const RELEASE_MS: f32 = 120.0;
/// How far under its ceiling the limiter holds the sound, in dB. The 8x detector
/// agrees with ffmpeg's `ebur128=peak=true` to a few hundredths of a dB on the
/// test material (white noise, pink noise, speech and music, a test tone), but
/// ffmpeg's own resampler reads a little higher on some of it: a speech-and-music
/// file held exactly at -1 dBTP read -0.974 there. The margin is that difference,
/// taken from the ceiling the limiter holds to and not from the setting a user
/// names, so the reply reads the measured level and the setting stays what was asked.
const SAFETY_DB: f32 = 0.1;

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
    /// The true peak of the loudest channel at each frame as the detector saw
    /// it, same indexing. The detector runs ahead of the line by six frames
    /// frames, so a slot's value describes the samples around the one six
    /// slots behind it; the window below covers those samples either way.
    peaks: Vec<f32>,
    /// One true-peak detector per channel, fed the mix as it arrives.
    detect: Vec<TruePeak>,
    /// Where the next frame is written; the frame read out is the one after it.
    at: usize,
    /// Frames still to be swallowed before anything is emitted -- the empty
    /// delay line, which is how the latency is paid back (module docs).
    warm: usize,
    gain: f32,
    /// Switched out while the mix runs ([`LimiterState::retune`]): the line
    /// still delays and still emits a frame per frame, only nothing is held
    /// down. Dropping the state instead would drop the frames still inside the
    /// line, and those frames are what the master clock counts.
    bypass: bool,
    ceiling: f32,
    /// Where the knee starts, as an amplitude: under this the fast path runs
    /// and no logarithm is taken.
    knee_floor: f32,
    release: f32,
}

impl LimiterState {
    pub fn new(params: &Limiter, sample_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let frames = ((LOOKAHEAD_MS / 1000.0 * sample_rate as f32) as usize).max(TP_TAPS) + 1;
        let ceiling_db = params.with_ceiling(params.ceiling_db).ceiling_db;
        Self {
            channels,
            line: vec![0.0; frames * channels],
            peaks: vec![0.0; frames],
            detect: vec![TruePeak::new(); channels],
            at: 0,
            warm: frames - 1,
            gain: 1.0,
            bypass: !params.on,
            ceiling: db_to_linear(ceiling_db - SAFETY_DB),
            knee_floor: db_to_linear(ceiling_db - SAFETY_DB - KNEE_DB / 2.0),
            // One time constant per sample: the gain covers 63% of the way back
            // in `RELEASE_MS`.
            release: 1.0 - (-1000.0 / (RELEASE_MS * sample_rate as f32)).exp(),
        }
    }

    /// One switched in **while the mix is already running**: the same limiter,
    /// except that it swallows nothing to fill its delay line -- the line is
    /// zeroed and emitting from the first frame, where [`new`] would have eaten
    /// that much of the mix at the front instead.
    ///
    /// Which matters because the count *is* the clock: a mixer that emitted
    /// fewer samples than the timeline asked for would slide the picture off
    /// the sound by the lookahead, once per switch-on.
    ///
    /// The price, stated as it measures rather than as it was hoped: not a
    /// momentary hair of silence the stream grows back from, but a **permanent**
    /// shift of [`LOOKAHEAD_MS`] for the rest of the stream -- the output is
    /// the input delayed by the whole line, every block, because nothing ever
    /// re-aligns it. That is at most 2 ms, which is under the perceptual
    /// threshold, and it is paid once per stream rather than growing; it is
    /// pinned by
    /// `tests::an_engaged_limiter_shifts_the_stream_by_the_lookahead_for_good`.
    /// Changing it is a design call about the export's A/V alignment, not a
    /// bug fix.
    pub fn engaged(params: &Limiter, sample_rate: u32, channels: usize) -> Self {
        Self {
            warm: 0,
            ..Self::new(params, sample_rate, channels)
        }
    }

    /// Points a running limiter at another setting -- a ceiling dragged, the
    /// switch flipped -- without touching the delay line, the peak window or
    /// the gain it is holding. That is what makes the ceiling a live control:
    /// rebuilding the state would restart the window (a pump) and lose the
    /// frames inside the line (a slip against the picture).
    pub fn retune(&mut self, params: &Limiter) {
        let ceiling_db = params.with_ceiling(params.ceiling_db).ceiling_db;
        self.bypass = !params.on;
        self.ceiling = db_to_linear(ceiling_db - SAFETY_DB);
        self.knee_floor = db_to_linear(ceiling_db - SAFETY_DB - KNEE_DB / 2.0);
    }

    /// Rewrites `buf` -- interleaved frames -- as the limited mix. Shorter than
    /// what came in by the delay line's length, once, at the first call.
    pub fn process(&mut self, buf: &mut Vec<f32>) {
        let channels = self.channels;
        let frames = buf.len() / channels;
        let mut out: Vec<f32> = Vec::with_capacity(buf.len());
        for f in 0..frames {
            let frame = &buf[f * channels..(f + 1) * channels];
            // The raw sample counts too: the interpolated peak can read a hair under a
            // sample that sits exactly on the ceiling, and the samples are what is held.
            let mut peak = 0.0f64;
            for (c, &s) in frame.iter().enumerate() {
                let x = f64::from(s);
                peak = peak.max(self.detect[c].push(x)).max(x.abs());
            }
            self.line[self.at * channels..(self.at + 1) * channels].copy_from_slice(frame);
            self.peaks[self.at] = peak as f32;
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
            // Bypassed, the line is a delay and nothing else: the frames come
            // out as they went in, one for one.
            let gain = match self.bypass {
                true => 1.0,
                false => self.gain,
            };
            for c in 0..channels {
                out.push(self.line[self.at * channels + c] * gain);
            }
        }
        *buf = out;
    }

    /// The frames still in the line once the mix has run out, limited: silence is
    /// fed through and what it pushes out is returned, which is the last
    /// `lookahead` frames of the stream. Those frames are the tail the mix would
    /// otherwise drop, and the silence behind them is what the detector needs --
    /// a waveform that stops mid-cycle is a step to zero, and a meter reads that
    /// step as an overshoot, so the gain that holds it is taken here, while the
    /// samples are still in the line. Call it once, at the end.
    pub fn drain(&mut self) -> Vec<f32> {
        let mut silence = vec![0.0f32; (self.peaks.len() - 1) * self.channels];
        self.process(&mut silence);
        silence
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
    use crate::loudness::Meter;
    use std::f64::consts::TAU;

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
        // ...and it is not simply silence: the loud half is right at the wall,
        // which is the ceiling less the safety margin.
        assert!(
            peak > db_to_linear(-3.0 - SAFETY_DB) * 0.99,
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

    /// A ceiling dragged while the mix runs: the new one is honoured from the
    /// next block, the delay line keeps running -- so the block is as long as
    /// it would have been, which is what the master clock counts -- and
    /// switching the limiter out leaves the samples alone rather than dropping
    /// the ones still inside the line.
    #[test]
    fn a_retune_moves_the_ceiling_and_costs_no_samples() {
        let rate = 48_000;
        let mut state = LimiterState::engaged(
            &Limiter {
                ceiling_db: -3.0,
                on: true,
            },
            rate,
            2,
        );
        let block: Vec<f32> = (0..2_000).map(|i| 2.0 * ((i % 7) as f32 - 3.0)).collect();

        // Engaged mid-stream: nothing is swallowed, so the count is the count.
        let mut buf = block.clone();
        state.process(&mut buf);
        assert_eq!(buf.len(), block.len(), "an engaged limiter ate samples");
        let hot = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(hot <= db_to_linear(-3.0) + 1e-6, "peak {hot} passed -3 dB");

        // Down to -12: the same length again, and quieter.
        state.retune(&Limiter {
            ceiling_db: -12.0,
            on: true,
        });
        let mut buf = block.clone();
        state.process(&mut buf);
        assert_eq!(buf.len(), block.len(), "a retune ate samples");
        let quiet = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            quiet <= db_to_linear(-12.0) + 1e-6,
            "peak {quiet} passed -12 dB"
        );
        assert!(quiet < hot, "the ceiling did not move: {hot} -> {quiet}");

        // Switched out: the line still delays, so the count still holds, and
        // what comes out is the mix as it was written.
        state.retune(&Limiter {
            ceiling_db: -12.0,
            on: false,
        });
        let mut buf = block.clone();
        state.process(&mut buf);
        assert_eq!(buf.len(), block.len(), "a bypass ate samples");
        let raw = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(raw > 1.0, "bypassed, the mix is untouched: peak {raw}");
    }

    /// What engaging a limiter mid-stream actually costs, measured rather than
    /// described: not a momentary hair of silence that the stream then grows
    /// back from, but a **permanent** shift. `engaged` starts its delay line
    /// empty and swallows nothing, so from the first block onward the output is
    /// the input delayed by the whole lookahead -- for the rest of the stream.
    /// `new` is head-aligned instead: it swallows the lookahead once, at the
    /// front, and is a block shorter for it.
    ///
    /// The two are measured against each other because the difference is the
    /// whole claim: same tone, same settings, one index offset and one block
    /// length apart. A permanent offset of at most [`LOOKAHEAD_MS`] is below
    /// the perceptual threshold, so this pins the *measured* behaviour the doc
    /// comment on [`LimiterState::engaged`] describes rather than ruling on
    /// whether it should be changed -- that is a design call, not a bug fix.
    #[test]
    fn an_engaged_limiter_shifts_the_stream_by_the_lookahead_for_good() {
        let rate = 48_000;
        let params = Limiter {
            ceiling_db: -3.0,
            on: true,
        };
        let lookahead = ((LOOKAHEAD_MS / 1000.0 * rate as f32) as usize).max(1);
        // Under the knee throughout, so what comes out is the input and the
        // only thing being measured is *when* each frame appears.
        let tone: Vec<f32> = (0..4_000)
            .flat_map(|i| {
                let s = 0.1 * (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin();
                [s, s]
            })
            .collect();

        // `new`: head-aligned, and a block shorter by exactly the lookahead.
        let mut fresh = LimiterState::new(&params, rate, 2);
        let mut head = tone.clone();
        fresh.process(&mut head);
        assert_eq!(
            head.len(),
            tone.len() - lookahead * 2,
            "a fresh limiter is not head-aligned by exactly the lookahead"
        );

        // `engaged`: the same count, the same samples -- one lookahead later.
        let mut live = LimiterState::engaged(&params, rate, 2);
        let mut shifted = tone.clone();
        live.process(&mut shifted);
        assert_eq!(
            shifted.len(),
            tone.len(),
            "an engaged limiter ate samples, which is the thing it exists not to do"
        );
        assert_eq!(
            &shifted[lookahead * 2..],
            &head[..],
            "an engaged limiter is not the fresh one delayed by exactly the \
             lookahead, so what it costs mid-stream is not what the doc claims"
        );
        // And the shift is the *stream's*, not one block's. A second block of a
        // marker the first did not contain comes out *still* delayed by the
        // lookahead: the head of it is still the tail of the tone, and the
        // marker itself only appears from the lookahead on. Nothing re-aligns
        // the line, which is what makes the cost permanent rather than a one-off
        // at the moment of engaging.
        // Well under the knee: a marker at the knee would be ringing-limited
        // by its own step, which is the detector working, not the shift.
        let marker = vec![0.25; tone.len()];
        let mut next = marker.clone();
        live.process(&mut next);
        assert!(
            next[..lookahead * 2].iter().any(|s| *s != 0.25),
            "the second block is aligned where the first was not, so the cost is \
             a one-off rather than a permanent shift"
        );
        assert!(
            next[lookahead * 2..]
                .iter()
                .all(|s| (*s - 0.25).abs() < 1e-6),
            "the marker is not behind by exactly the lookahead: the stream is not \
             delayed by a fixed amount from here on"
        );
    }

    /// The ceiling is a true-peak ceiling: the samples of a waveform can sit
    /// under it while the waveform between them does not, and the export is
    /// measured on the waveform. Held at -1 dBTP, the meter that measures the
    /// export reads it at or under -1 dBTP, for a crest the samples hide (fs/4
    /// at 45 degrees, amplitude 1.3, so the samples are +-0.92) and for a dense
    /// high-frequency mix.
    #[test]
    fn a_true_peak_ceiling_holds_on_the_waveform_the_meter_reads() {
        let rate = 48_000;
        let params = Limiter {
            ceiling_db: -1.0,
            on: true,
        };
        let n = rate as usize;
        let crest: Vec<f32> = (0..n)
            .flat_map(|i| {
                let ph = i as f64 * std::f64::consts::FRAC_PI_2 + std::f64::consts::FRAC_PI_4;
                let s = (1.3 * ph.sin()) as f32;
                [s, s]
            })
            .collect();
        let dense: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f64 / f64::from(rate);
                let s = 0.5 * (TAU * 13_000.0 * t + 1.3).sin()
                    + 0.4 * (TAU * 17_500.0 * t + 0.7).sin()
                    + 0.3 * (TAU * 9_000.0 * t + 2.1).sin();
                [s as f32, (0.5 * (TAU * 13_100.0 * t + 0.2).sin() + 0.4 * (TAU * 17_400.0 * t + 2.9).sin()) as f32]
            })
            .collect();
        // 20 ms of digital silence after each: the meter flushes with zeros, so
        // a signal cut off mid-wave would read the cut itself as an overshoot.
        let tail = vec![0.0f32; rate as usize / 50 * 2];
        for mix in [crest, dense] {
            let mut state = LimiterState::new(&params, rate, 2);
            let mut buf = [mix.as_slice(), tail.as_slice()].concat();
            state.process(&mut buf);
            let tp = Meter::new(rate, 2);
            let mut meter = tp.clone();
            meter.push(&buf);
            let read = meter.finish().true_peak_dbtp;
            // The hold is to a thousandth of a dB: the gain moves within the
            // filter's support, which costs a few ten-thousandths at most.
            assert!(read <= -0.999, "the meter reads {read} dBTP past the -1 ceiling");
        }
    }

    /// The end of the stream. `process` alone keeps the last `lookahead` frames
    /// in the line, and the export pads what it does not have with silence: a
    /// step at the cut, which the meter reads as an overshoot. `drain` plays the
    /// line out, so the whole stream comes out and the stop is held as well.
    #[test]
    fn the_tail_drains_and_the_stop_at_the_end_is_held() {
        let rate = 44_100;
        let params = Limiter {
            ceiling_db: -1.0,
            on: true,
        };
        let n = rate as usize / 2;
        // 900 Hz at 4x full scale, stopped at a sample near its crest.
        let input: Vec<f32> = (0..n)
            .flat_map(|i| {
                let s = 4.0 * (i as f32 * std::f32::consts::TAU * 900.0 / rate as f32).sin();
                [s, s]
            })
            .collect();
        let mut state = LimiterState::new(&params, rate, 2);
        let mut out = input.clone();
        state.process(&mut out);
        let mut tail = state.drain();
        out.append(&mut tail);
        assert_eq!(out.len(), input.len(), "the drained stream is not the whole stream");
        // The meter, fed the stream and then the silence that follows it.
        let mut meter = Meter::new(rate, 2);
        meter.push(&out);
        meter.push(&vec![0.0f32; rate as usize / 50 * 2]);
        let read = meter.finish().true_peak_dbtp;
        assert!(read <= -0.999, "the stop reads {read} dBTP past the -1 ceiling");
    }

    /// The held level sits [`SAFETY_DB`] under the ceiling named, not at it: a
    /// hot tone comes out at the named ceiling less the margin, on the sample
    /// peak, and the setting itself is not moved.
    #[test]
    fn the_limiter_holds_a_safety_margin_under_the_ceiling_named() {
        let rate = 48_000;
        let named = Limiter {
            ceiling_db: -1.0,
            on: true,
        };
        let mut state = LimiterState::new(&named, rate, 1);
        let mut buf: Vec<f32> = (0..rate)
            .map(|i| 3.0 * (i as f32 * std::f32::consts::TAU * 1_000.0 / rate as f32).sin())
            .collect();
        state.process(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let held = db_to_linear(-1.0 - SAFETY_DB);
        assert!(peak <= held + 1e-6, "peak {peak} passed the margin {held}");
        assert!(peak > held * 0.99, "peak {peak} is nowhere near the margin {held}");
        assert_eq!(named.ceiling_db, -1.0, "the setting itself moved");
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

