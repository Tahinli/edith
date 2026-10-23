//! Per-segment audio equalizer: a cascade of RBJ biquads applied engine-side by
//! sample math. The effect has to live in our own samples, never in a device
//! property — a daemon-side gain would be invisible to an export and to anyone
//! reading the graph, and the RT callback is allocation-free either way.
//!
//! Coefficients follow the Audio EQ Cookbook (Robert Bristow-Johnson,
//! <https://www.w3.org/TR/audio-eq-cookbook/>), peaking, shelving and cut sections,
//! run as transposed direct form II: two state words per band per channel, one
//! multiply-add chain per sample, nothing to allocate.
//!
//! [`EqParams::default_layout`] is a five-band starting point — shelves at the
//! edges (80 Hz, 12 kHz), where a peak would leave the last octave untouched,
//! and peaks at 250 Hz / 1 kHz / 4 kHz for mud, presence and air. It is only a
//! default: band count, frequency and kind are data, so a segment can carry any
//! layout the .edith file cares to write.

use std::f64::consts::PI;

/// Shape of one band's response. Shelves tilt everything past their corner;
/// a peak is local; a cut removes everything past its corner. A cut is a
/// second-order filter at the band's frequency with the band's Q, and ignores
/// the band's gain -- it has no level to set, only a corner to put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    LowShelf,
    Peak,
    HighShelf,
    /// High-pass: removes what is below the corner (rumble, mic handling).
    LowCut,
    /// Low-pass: removes what is above the corner (hiss, air).
    HighCut,
}

/// One band. Plain data with no invariants of its own: out-of-range values are
/// clamped when coefficients are built, so a deserializer can hand this over
/// unchecked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub freq_hz: f32,
    pub gain_db: f32,
    /// Bandwidth for a peak, slope for a shelf. 0.707 is the flat-shelf value.
    pub q: f32,
    pub kind: BandKind,
}

/// A segment's equalizer setting: the bands, in cascade order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EqParams {
    pub bands: Vec<Band>,
}

impl EqParams {
    /// The five-band default described in the module docs, all gains flat.
    pub fn default_layout() -> Self {
        let band = |freq_hz, kind| Band {
            freq_hz,
            gain_db: 0.0,
            q: 0.707,
            kind,
        };
        Self {
            bands: vec![
                band(80.0, BandKind::LowShelf),
                band(250.0, BandKind::Peak),
                band(1000.0, BandKind::Peak),
                band(4000.0, BandKind::Peak),
                band(12000.0, BandKind::HighShelf),
            ],
        }
    }

    /// True when no band moves anything, so processing can be skipped outright.
    /// A cut always moves something, whatever its gain says, so it is never
    /// identity: a cut band the listener put down is not a flat curve.
    pub fn is_identity(&self) -> bool {
        self.bands.iter().all(|b| match b.kind {
            BandKind::LowCut | BandKind::HighCut => false,
            _ => b.gain_db.abs() < 1e-4,
        })
    }

    /// How much the cascade moves a sine at `freq_hz`, in dB — the curve a UI
    /// draws. Read off the *same* [`Coeffs`] [`EqState`] filters with and
    /// evaluated at `z = e^{jw}`, so a drawn curve cannot drift from what is
    /// heard: there is one set of coefficients and one formula for both.
    /// Cascaded sections multiply, so their dB add.
    pub fn response_db(&self, freq_hz: f32, sample_rate: u32) -> f32 {
        self.bands
            .iter()
            .map(|b| Coeffs::new(b, sample_rate).magnitude_db(freq_hz, sample_rate))
            .sum()
    }
}

/// Normalized biquad coefficients (a0 divided out).
#[derive(Debug, Clone, Copy, Default)]
struct Coeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// How far a band's gain may go either side of flat, in dB: the bound
/// [`Coeffs::new`] holds the one field [`Band`]'s doc says is clamped for it.
///
/// Deliberately far past anything a card offers -- the UI's own limit is a
/// card decision, not a project one, and a project is allowed to be more
/// extreme than the control that set it -- but bounded all the same, because
/// the gain is not a magnitude anywhere else in this struct: it enters as
/// `a = 10^(gain_db/40)`, and `a` drives the peak section's pole radius toward
/// the unit circle. A few hundred dB puts the poles *on* it, and a section
/// whose poles are on the unit circle does not decay, it rings -- growing the
/// samples without limit until the device clamp and the export's own [-1, 1]
/// turn the whole clip into a full-scale square. Past 10_000 dB `a` is not even
/// finite any more and the coefficients come out `inf`.
///
/// 40 dB is the ceiling because it is where the arithmetic stays comfortably
/// inside f32 (a linear gain of 100) while being louder than any boost a
/// mixing console offers. Clamped, not refused: the struct's contract is that
/// a deserializer may hand this over unchecked, and a project that says
/// +400 dB gets +40 dB rather than a project that will not open.
pub const MAX_BAND_GAIN_DB: f32 = 40.0;
/// ...and the floor, the same distance the other way. Symmetric because a
/// wide cut and a wide boost are the same filter run at two gains.
pub const MIN_BAND_GAIN_DB: f32 = -40.0;

impl Coeffs {
    /// Cookbook peaking, shelving or cut section for `band` at `sample_rate`.
    fn new(band: &Band, sample_rate: u32) -> Self {
        // A frequency at or past Nyquist has no cookbook answer (sin(w0) folds),
        // Q of zero divides by zero, and a gain of a few hundred dB walks the
        // peak's poles onto the unit circle: clamp rather than reject, the
        // caller's value came out of a text file. The gain is the fourth of
        // these four clamps, and the one the struct's own doc promised.
        let fs = f64::from(sample_rate.max(1));
        let f0 = f64::from(band.freq_hz).clamp(1.0, fs * 0.49);
        let q = f64::from(band.q).clamp(0.05, 40.0);
        let gain =
            f64::from(band.gain_db).clamp(f64::from(MIN_BAND_GAIN_DB), f64::from(MAX_BAND_GAIN_DB));
        let a = 10f64.powf(gain / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            BandKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos_w0,
                1.0 - alpha / a,
            ),
            BandKind::LowShelf => (
                a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            ),
            BandKind::HighShelf => (
                a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            ),
            // RBJ high-pass and low-pass; the gain never enters them.
            BandKind::LowCut => (
                (1.0 + cos_w0) / 2.0,
                -(1.0 + cos_w0),
                (1.0 + cos_w0) / 2.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            BandKind::HighCut => (
                (1.0 - cos_w0) / 2.0,
                1.0 - cos_w0,
                (1.0 - cos_w0) / 2.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
        };
        Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    /// |H(e^{jw})| in dB for this section at `freq_hz`. Past Nyquist there is
    /// no answer to give, so the frequency clamps there like `f0` does.
    fn magnitude_db(&self, freq_hz: f32, sample_rate: u32) -> f32 {
        let fs = f64::from(sample_rate.max(1));
        let w = 2.0 * PI * f64::from(freq_hz).clamp(0.0, fs * 0.5) / fs;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let (b0, b1, b2) = (f64::from(self.b0), f64::from(self.b1), f64::from(self.b2));
        let (a1, a2) = (f64::from(self.a1), f64::from(self.a2));
        let num = ((b0 + b1 * cos1 + b2 * cos2).powi(2) + (b1 * sin1 + b2 * sin2).powi(2)).sqrt();
        // A stable section never puts the denominator at zero, but a hand-
        // written file could; the floor keeps the curve finite rather than
        // painting an infinity.
        let den = ((1.0 + a1 * cos1 + a2 * cos2).powi(2) + (a1 * sin1 + a2 * sin2).powi(2))
            .sqrt()
            .max(1e-12);
        (20.0 * (num / den).log10()) as f32
    }
}

/// Anything smaller than this is inaudible at any gain we allow, and letting it
/// stay would drag the state into denormals — where a decaying tail costs tens
/// of times what a live signal does on x86. Flushed to a hard zero instead.
const DENORMAL_FLOOR: f32 = 1e-20;

/// A running equalizer: the coefficients plus one filter memory per band per
/// channel. Built off the UI thread, driven from the audio path.
pub struct EqState {
    sample_rate: u32,
    channels: usize,
    coeffs: Vec<Coeffs>,
    /// `[s1, s2]` per (band, channel), band-major.
    state: Vec<[f32; 2]>,
    /// All-flat params: [`process`](EqState::process) is then a no-op and the
    /// samples come out bit-identical.
    identity: bool,
}

impl EqState {
    /// Allocates coefficients and filter memory for `params`. `channels` is the
    /// interleave width of the buffers [`process`](EqState::process) will see.
    pub fn new(params: &EqParams, sample_rate: u32, channels: u16) -> Self {
        let mut eq = Self {
            sample_rate,
            channels: (channels as usize).max(1),
            coeffs: Vec::new(),
            state: Vec::new(),
            identity: true,
        };
        eq.set_params(params);
        eq
    }

    /// Recomputes coefficients for edited params, keeping the filter memory so a
    /// live segment does not click. Allocates — a UI edit, never the RT path.
    pub fn set_params(&mut self, params: &EqParams) {
        self.identity = params.is_identity();
        self.coeffs.clear();
        self.coeffs.extend(
            params
                .bands
                .iter()
                .map(|b| Coeffs::new(b, self.sample_rate)),
        );
        self.state
            .resize(self.coeffs.len() * self.channels, [0.0; 2]);
    }

    /// Drops the filter memory: for a seek, where the samples either side of the
    /// cut are unrelated and a carried tail would ring.
    pub fn reset(&mut self) {
        self.state.fill([0.0; 2]);
    }

    /// Filters `interleaved` in place, `channels` values per frame. Allocation-
    /// free and branch-light: safe to call from the feeder.
    ///
    /// A trailing partial frame is left alone rather than filtered into the
    /// wrong channel's state.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if self.identity || self.coeffs.is_empty() {
            return;
        }
        let channels = self.channels;
        for (band, c) in self.coeffs.iter().enumerate() {
            let state = &mut self.state[band * channels..][..channels];
            for frame in interleaved.chunks_exact_mut(channels) {
                for (ch, x) in frame.iter_mut().enumerate() {
                    let s = &mut state[ch];
                    let y = c.b0 * *x + s[0];
                    s[0] = c.b1 * *x - c.a1 * y + s[1];
                    s[1] = c.b2 * *x - c.a2 * y;
                    if s[0].abs() < DENORMAL_FLOOR {
                        s[0] = 0.0;
                    }
                    if s[1].abs() < DENORMAL_FLOOR {
                        s[1] = 0.0;
                    }
                    *x = y;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::{Band, BandKind, EqParams, EqState};

    const FS: u32 = 48_000;

    fn peak(freq_hz: f32, gain_db: f32, q: f32) -> EqParams {
        EqParams {
            bands: vec![Band {
                freq_hz,
                gain_db,
                q,
                kind: BandKind::Peak,
            }],
        }
    }

    fn sine(freq_hz: f32, frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| (TAU * freq_hz * (i / channels) as f32 / FS as f32).sin())
            .collect()
    }

    /// RMS of the second half only: the first half is the filter's transient.
    fn rms(samples: &[f32]) -> f32 {
        let tail = &samples[samples.len() / 2..];
        (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt()
    }

    fn db(measured: f32, reference: f32) -> f32 {
        20.0 * (measured / reference).log10()
    }

    #[test]
    fn flat_params_pass_samples_through_bit_identical() {
        let input = sine(440.0, 4800, 2);
        let mut out = input.clone();
        EqState::new(&EqParams::default_layout(), FS, 2).process(&mut out);
        assert_eq!(out, input, "a flat EQ must not touch the samples at all");

        // ...and an explicitly built cascade of 0 dB bands is flat too, which is
        // the case a serialized "EQ enabled, nothing moved" segment hits.
        let mut params = EqParams::default_layout();
        params.bands[2].q = 4.0;
        assert!(params.is_identity());
    }

    #[test]
    fn a_band_moves_its_own_center_and_leaves_two_octaves_away_alone() {
        for gain in [6.0, -6.0] {
            let center = sine(1000.0, 48_000, 1);
            let mut out = center.clone();
            EqState::new(&peak(1000.0, gain, 1.0), FS, 1).process(&mut out);
            let at_center = db(rms(&out), rms(&center));
            assert!(
                (at_center - gain).abs() < 0.5,
                "1 kHz sine through a {gain:+} dB band at 1 kHz measured {at_center:+.2} dB"
            );

            for off in [250.0, 4000.0] {
                let away = sine(off, 48_000, 1);
                let mut out = away.clone();
                EqState::new(&peak(1000.0, gain, 1.0), FS, 1).process(&mut out);
                let leak = db(rms(&out), rms(&away));
                assert!(
                    leak.abs() < 1.0,
                    "{off} Hz is two octaves off but moved {leak:+.2} dB"
                );
            }
        }
    }

    /// The curve a card draws is the filter a listener hears: a sine measured
    /// through `process` lands where `response_db` said it would, at the band
    /// centres and between them, boosted and cut.
    #[test]
    fn the_drawn_response_is_the_gain_the_samples_actually_get() {
        let mut params = EqParams::default_layout();
        params.bands[0].gain_db = -8.0; // 80 Hz low shelf
        params.bands[2].gain_db = 10.0; // 1 kHz peak
        params.bands[2].q = 1.4;
        params.bands[4].gain_db = 5.0; // 12 kHz high shelf

        // Centres, the slopes between them, and the two ends of the axis the
        // card draws -- a curve is wrong in the gaps or nowhere.
        for freq in [
            40.0, 80.0, 250.0, 600.0, 1000.0, 2000.0, 4000.0, 12000.0, 16000.0,
        ] {
            let input = sine(freq, 48_000, 1);
            let mut out = input.clone();
            EqState::new(&params, FS, 1).process(&mut out);
            let measured = db(rms(&out), rms(&input));
            let drawn = params.response_db(freq, FS);
            assert!(
                (measured - drawn).abs() < 0.5,
                "{freq} Hz: curve says {drawn:+.2} dB, samples measured {measured:+.2} dB"
            );
        }

        // Flat draws flat, and nothing is drawn past Nyquist.
        let flat = EqParams::default_layout();
        assert!(flat.response_db(1000.0, FS).abs() < 1e-4);
        assert!(params.response_db(FS as f32, FS).is_finite());
    }

    #[test]
    fn ten_seconds_of_noise_through_every_band_boosted_stays_bounded() {
        let mut params = EqParams::default_layout();
        for band in &mut params.bands {
            band.gain_db = 12.0;
        }
        let mut eq = EqState::new(&params, FS, 2);

        // xorshift: a fixed white-noise source, no dev-dependency.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 40) as f32 / 8_388_608.0 - 1.0
        };

        let mut peak = 0.0f32;
        // Fed in 1024-frame blocks, as the feeder would: state must survive the
        // block boundary, so a per-call reset would show up as clicks here.
        for _ in 0..(10 * FS as usize / 1024) {
            let mut block: Vec<f32> = (0..1024 * 2).map(|_| noise()).collect();
            eq.process(&mut block);
            for v in block {
                assert!(v.is_finite(), "EQ output went to {v}");
                peak = peak.max(v.abs());
            }
        }
        // +12 dB on five overlapping bands is about 4x on the worst-case sum of
        // a full-scale input; anything past that is instability, not gain.
        assert!(
            (1.0..20.0).contains(&peak),
            "peak {peak} out of the plausible band for +12 dB x5"
        );
    }

    #[test]
    fn a_decaying_tail_leaves_the_state_at_a_hard_zero() {
        let mut params = EqParams::default_layout();
        params.bands[1].gain_db = 9.0;
        let mut eq = EqState::new(&params, FS, 2);

        let mut decay: Vec<f32> = (0..4800 * 2)
            .map(|i| 0.9f32.powi(i / 2) * if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        eq.process(&mut decay);
        let mut silence = vec![0.0f32; 4800 * 2];
        eq.process(&mut silence);

        for (i, s) in eq.state.iter().enumerate() {
            assert!(
                s[0] == 0.0 && s[1] == 0.0,
                "state {i} still holds {s:?} after a decayed tail — denormal territory"
            );
        }
    }

    #[test]
    fn an_impulse_in_the_left_channel_never_reaches_the_right() {
        let mut params = EqParams::default_layout();
        params.bands[3].gain_db = -10.0;
        let mut eq = EqState::new(&params, FS, 2);

        let mut buf = vec![0.0f32; 512 * 2];
        buf[0] = 1.0;
        eq.process(&mut buf);

        assert!(buf[2] != 0.0, "the left channel should be ringing");
        for (frame, pair) in buf.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(
                pair[1], 0.0,
                "frame {frame}: right channel picked up {pair:?}"
            );
        }
    }

    /// A gain a `.edith` line can name, and the pole radius that comes of it.
    ///
    /// The invariant [`Band`] documents is that out-of-range values are clamped
    /// when the coefficients are built, so a deserializer may hand one over
    /// unchecked -- and three of the four fields were clamped at
    /// [`Coeffs::new`] while `gain_db` was not. `a = 10^(gain_db/40)` feeds the
    /// peak's `sqrt(a2/a0)` pole radius straight to 1 as the gain grows, and at
    /// a few hundred dB the f32 rounding puts the poles *exactly* on the unit
    /// circle: a marginally-stable section that rings for seconds, blowing up
    /// the samples until the device clamp turns the whole clip into a
    /// full-scale square (in monitoring and in the export alike). This is an
    /// arithmetic claim about the pole radius, not an audibility claim: nothing
    /// here says what the filter sounds like, only that it is stable.
    ///
    /// The poles are solved off the normalized coefficients, so this is the
    /// filter the sample loop actually runs rather than a restatement of the
    /// formula that built it.
    #[test]
    fn a_gain_a_file_can_name_leaves_the_poles_strictly_inside_the_unit_circle() {
        for gain in [
            12.0f32, 40.0, 100.0, 200.0, 400.0, 10_000.0, -200.0, -10_000.0,
        ] {
            let c = super::Coeffs::new(
                &Band {
                    freq_hz: 1000.0,
                    gain_db: gain,
                    q: 1.0,
                    kind: BandKind::Peak,
                },
                FS,
            );
            // The poles of `z^2 + a1 z + a2`, solved: the number the sample
            // loop's homogeneous solution is actually made of, and the only
            // place a section's stability is decided. `radius` is the larger
            // of the two, so a section is stable exactly when it is `< 1`.
            let (a1, a2) = (c.a1 as f64, c.a2 as f64);
            let disc = (a1 * a1 - 4.0 * a2).max(0.0).sqrt();
            let radius = [(-a1 + disc) / 2.0, (-a1 - disc) / 2.0]
                .into_iter()
                .fold(0.0f64, |m, r| m.max(r.abs()));
            assert!(
                radius < 0.999,
                "gain {gain} dB gives a pole radius of {radius:.9} -- \
                 the section rings instead of decaying"
            );
            assert!(
                c.b0.is_finite() && c.b1.is_finite() && c.b2.is_finite(),
                "gain {gain} dB produced non-finite coefficients {c:?}"
            );
        }
    }

    /// The same claim about the samples rather than the coefficients: a
    /// full-scale burst through a band whose gain a file named, with the gain
    /// far past the card's own 12 dB limit, has to come out at the gain the
    /// clamp ceiling *is*. +40 dB is a linear gain of 100, so a full-scale
    /// burst peaks at ~100 and that is the answer, not a fault; anything past
    /// it is the section ringing rather than a band being loud. Pre-fix, 200 dB
    /// and 400 dB are past it by orders of magnitude.
    #[test]
    fn a_gain_a_file_can_name_does_not_blow_the_samples_up() {
        for gain in [40.0f32, 100.0, 200.0, 400.0, 10_000.0] {
            let mut out = vec![0.0f32; 48_000 * 2];
            // A full-scale 1 kHz burst for a tenth of a second, then silence:
            // whatever the section rings into is what shows up after it.
            for (i, o) in out.iter_mut().enumerate().take(4_800 * 2) {
                *o = (TAU * 1000.0 * (i / 2) as f32 / FS as f32).sin();
            }
            EqState::new(&peak(1000.0, gain, 1.0), FS, 2).process(&mut out);
            let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(
                peak.is_finite() && peak < 200.0,
                "gain {gain} dB sent a full-scale burst to {peak}: the clamp ceiling is \
                 +40 dB, a linear gain of 100, and 200x that is a section ringing, not a band"
            );
        }
    }

    fn cut(kind: BandKind, freq_hz: f32) -> EqParams {
        EqParams {
            bands: vec![Band {
                freq_hz,
                gain_db: 0.0,
                q: 0.707,
                kind,
            }],
        }
    }

    /// A second-order cut attenuates well past its corner and passes well
    /// above it (high-pass) or below it (low-pass): the response curve, not the
    /// samples, so the corner and the slope are checked without transients.
    #[test]
    fn a_cut_attenuates_past_its_corner_and_passes_the_other_side() {
        let low = cut(BandKind::LowCut, 1000.0);
        assert!(
            low.response_db(100.0, FS) < -30.0,
            "high-pass at 1 kHz, 100 Hz"
        );
        assert!(
            low.response_db(10_000.0, FS).abs() < 0.1,
            "high-pass passes 10 kHz"
        );

        let high = cut(BandKind::HighCut, 1000.0);
        assert!(
            high.response_db(10_000.0, FS) < -30.0,
            "low-pass at 1 kHz, 10 kHz"
        );
        assert!(
            high.response_db(100.0, FS).abs() < 0.1,
            "low-pass passes 100 Hz"
        );

        // Butterworth (Q 0.707) is -3 dB at the corner for either side.
        for kind in [BandKind::LowCut, BandKind::HighCut] {
            let at_corner = cut(kind, 1000.0).response_db(1000.0, FS);
            assert!(
                (at_corner + 3.0).abs() < 0.2,
                "{kind:?} at its corner measured {at_corner:+.2} dB, want -3"
            );
        }
    }

    /// The same claim through the samples: a 50 Hz tone through a 1 kHz
    /// high-pass is nearly gone, and a 10 kHz tone through a 1 kHz low-pass is.
    #[test]
    fn a_cut_removes_the_tone_it_names_from_the_samples() {
        for (kind, freq, want_below) in [
            (BandKind::LowCut, 50.0, -30.0),
            (BandKind::HighCut, 10_000.0, -30.0),
        ] {
            let input = sine(freq, 48_000, 1);
            let mut out = input.clone();
            EqState::new(&cut(kind, 1000.0), FS, 1).process(&mut out);
            let measured = db(rms(&out), rms(&input));
            assert!(
                measured < want_below,
                "{kind:?} at 1 kHz left {freq} Hz at {measured:+.2} dB"
            );
        }
    }

    /// The gain is not a parameter of a cut: the same corner at +12 dB and at
    /// -12 dB draws the same curve, and the drawn curve is what the samples get.
    #[test]
    fn a_cut_ignores_its_gain() {
        for kind in [BandKind::LowCut, BandKind::HighCut] {
            let flat = cut(kind, 1000.0);
            let mut loud = flat.clone();
            loud.bands[0].gain_db = 12.0;
            let mut quiet = flat.clone();
            quiet.bands[0].gain_db = -12.0;
            for freq in [50.0, 300.0, 1000.0, 3000.0, 15_000.0] {
                let want = flat.response_db(freq, FS);
                assert_eq!(loud.response_db(freq, FS), want, "{kind:?} at {freq} Hz");
                assert_eq!(quiet.response_db(freq, FS), want, "{kind:?} at {freq} Hz");
            }
        }
    }

    /// A cut is never identity, whatever gain it carries, so processing runs
    /// and the samples do change. A peak at 0 dB still is.
    #[test]
    fn a_cut_band_is_never_identity() {
        for kind in [BandKind::LowCut, BandKind::HighCut] {
            let params = cut(kind, 1000.0);
            assert!(!params.is_identity(), "{kind:?} at 0 dB reads as flat");
            let input = sine(50.0, 4800, 1);
            let mut out = input.clone();
            EqState::new(&params, FS, 1).process(&mut out);
            assert_ne!(out, input, "{kind:?} left the samples untouched");
        }
        assert!(peak(1000.0, 0.0, 1.0).is_identity());
    }
}
