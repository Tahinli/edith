//! Per-segment audio equalizer: a cascade of RBJ biquads applied engine-side by
//! sample math. The effect has to live in our own samples, never in a device
//! property — a daemon-side gain would be invisible to an export and to anyone
//! reading the graph, and the RT callback is allocation-free either way.
//!
//! Coefficients follow the Audio EQ Cookbook (Robert Bristow-Johnson,
//! <https://www.w3.org/TR/audio-eq-cookbook/>), peaking and shelving sections,
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
/// a peak is local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    LowShelf,
    Peak,
    HighShelf,
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
    pub fn is_identity(&self) -> bool {
        self.bands.iter().all(|b| b.gain_db.abs() < 1e-4)
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

impl Coeffs {
    /// Cookbook peaking/shelving section for `band` at `sample_rate`.
    fn new(band: &Band, sample_rate: u32) -> Self {
        // A frequency at or past Nyquist has no cookbook answer (sin(w0) folds),
        // and Q of zero divides by zero: clamp rather than reject, the caller's
        // value came out of a text file.
        let fs = f64::from(sample_rate.max(1));
        let f0 = f64::from(band.freq_hz).clamp(1.0, fs * 0.49);
        let q = f64::from(band.q).clamp(0.05, 40.0);
        let a = 10f64.powf(f64::from(band.gain_db) / 40.0);
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
        for (frame, pair) in buf.chunks_exact(2).enumerate() {
            assert_eq!(
                pair[1], 0.0,
                "frame {frame}: right channel picked up {pair:?}"
            );
        }
    }
}
