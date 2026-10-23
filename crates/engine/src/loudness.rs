//! EBU R128 / ITU-R BS.1770-4 loudness and true peak, streamed.
//!
//! [`Meter`] takes interleaved `f32` samples as they come off the decoder and
//! keeps only what the measurement needs: the K-weighted power of every 100 ms
//! chunk, and the peak trackers. [`Meter::finish`] turns those into a
//! [`Loudness`]. Nothing here is hard-coded to one sample rate: the K-weighting
//! biquads are recomputed from the BS.1770 analog prototypes for whatever rate
//! the meter is built with.
//!
//! Conventions, all of them the standard's unless noted:
//!
//! * Momentary is 400 ms, short-term is 3 s, both measured on 100 ms chunks, so
//!   a 400 ms block is four chunks and blocks overlap by 75%.
//! * Gating: absolute at -70 LUFS, then relative at -10 LU below the mean of
//!   what passed the absolute gate. Integrated loudness is the mean of what
//!   passes both.
//! * Channel weights: mono and the first three channels of anything are 1.0;
//!   for six or more channels, in SMPTE order (L R C LFE Ls Rs ...), the LFE is
//!   excluded and the surrounds weigh 1.41. Anything with fewer than six
//!   channels that is not mono or stereo is weighted as if it were the first
//!   channels of that layout.
//! * LRA follows EBU Tech 3342: short-term loudness sampled at 2 Hz, a -20 LU
//!   relative gate, and the spread between the 10th and 95th percentiles. The
//!   percentiles are nearest-rank, not interpolated.
//! * True peak is the BS.1770-4 Annex 2 method, 8x oversampled rather than the
//!   standard's 4x: a 256-tap windowed-sinc FIR (32 taps per phase,
//!   [`TP_COEF`]), and the largest magnitude over every oversampled sample. It is the same detector the
//!   master limiter runs ([`crate::limiter`]), so a ceiling it holds reads at
//!   or under that ceiling here.
//!
//! Silence is reported as [`f64::NEG_INFINITY`] for every loudness and peak
//! that has no energy to measure. A short-term figure that cannot be computed
//! yet (under three seconds of audio) is also `-inf`. LRA needs at least two
//! 2 Hz short-term samples to be a spread at all: with fewer, `range_lu` is
//! `0.0` and `lra_samples` says why (see its docs).

use std::f64::consts::PI;
use std::path::Path;

use crate::AudioSession;

/// Absolute gate, LUFS: a block quieter than this does not count at all.
const ABS_GATE_LUFS: f64 = -70.0;
/// Relative gate for integrated loudness, LU below the absolute-gated mean.
const REL_GATE_LU: f64 = -10.0;
/// Relative gate for LRA, LU below the absolute-gated short-term mean.
const LRA_REL_GATE_LU: f64 = -20.0;
/// Chunks per second: 100 ms, the hop of every block in the standard.
const CHUNKS_PER_SEC: usize = 10;
/// Chunks in a 400 ms momentary block.
const MOMENTARY_CHUNKS: usize = 4;
/// Chunks in a 3 s short-term block.
const SHORT_TERM_CHUNKS: usize = 30;
/// Chunks between two LRA samples: 5 x 100 ms = 500 ms, i.e. 2 Hz.
const LRA_HOP_CHUNKS: usize = 5;
/// Taps of the true-peak interpolator per phase.
pub(crate) const TP_TAPS: usize = 32;
/// Oversampling factor of the true-peak detector.
const TP_PHASES: usize = 8;

/// The 8x oversampling filter, polyphase: a 256-tap Kaiser-windowed sinc (beta
/// 8), split as `TP_COEF[p][j] = h[8j + p]` (phase `p` is the output between
/// input samples, tap `j` multiplies the input `j` steps back), each phase
/// normalised to unit DC gain.
///
/// Why 8x and 32 taps, and not the 4x 48-tap table this file used to carry:
/// the 4x table read up to 1.6 dB under the true peak of dense high-frequency
/// material (white noise), and a limiter that holds that reading lets the
/// export overshoot the ceiling by the same amount. Measured against ffmpeg's
/// `ebur128=peak=true` on the test material (white noise, pink noise, a
/// two-tone mix, a +12 dB test tone, a speech-and-music file): this table
/// reads within 0.1 dB of ffmpeg on all of them and never under it by more than
/// 0.05 dB, where the 4x table was 1.6 dB under on the noise. Tones within
/// about a tenth of the sample rate of Nyquist are the one place the two
/// filters, and ffmpeg's resampler, disagree by a few tenths of a dB in either
/// direction (a 15 kHz tone at 48 kHz reads 0.4 dB under ffmpeg here; a 20 kHz
/// one reads 0.4 dB over the waveform's true crest); nothing else in the test
/// set does that.
pub(crate) const TP_COEF: [[f64; TP_TAPS]; TP_PHASES] = [
    [-9.113131439975678e-06, 4.0076035670222974e-05, -0.00010621819573270595, 0.0002284040759670764, -0.0004338708206289006, 0.0007569269598663376, -0.001239982208738999, 0.0019355921973831004, -0.0029109007764240528, 0.004257373975001418, -0.006112418887724569, 0.00870982957472023, -0.012509950835799514, 0.018598441767977404, -0.030321702331554387, 0.06538702643260477, 0.9935370754684095, -0.057482007155269126, 0.02827349906597848, -0.017640066565440395, 0.011942377142413332, -0.008332275252542716, 0.005846125446777624, -0.0040647034070906305, 0.002770883608971783, -0.0018349008372697962, 0.0011691692091189859, -0.0007087508284246482, 0.00040253288581122037, -0.00020919986159042688, 9.537201341962132e-05, -3.46447644203612e-05],
    [-3.311694388264495e-05, 0.00013113986569634532, -0.0003358453978417591, 0.0007088167627000853, -0.0013300206219824767, 0.0022998489712809, -0.0037422781404128427, 0.005811272430550032, -0.008704737233512524, 0.012695107173726997, -0.018197543923624927, 0.02593058478547953, -0.037337762954957596, 0.05591630108406594, -0.09294865839644116, 0.21555037786454243, 0.9426947295402057, -0.145858673869235, 0.07531945801178211, -0.047702981628914426, 0.032480746980827084, -0.022701723557568016, 0.015920900371157783, -0.011048146610773036, 0.007507937684653195, -0.004950613758014254, 0.0031369765747049742, -0.0018880054283359473, 0.0010620801545960844, -0.0005445803041891528, 0.0002430517421209236, -8.464122840432894e-05],
    [-6.169787761900706e-05, 0.00022418908603070837, -0.000556455526893554, 0.0011539493624898778, -0.0021399892134944926, 0.003668916980414196, -0.005931212321661181, 0.0091640417587589, -0.013674291087551773, 0.019889092089511885, -0.02846897809283543, 0.04057719426108761, -0.05859942936943879, 0.08848148300534162, -0.1503840991546257, 0.3823053610670679, 0.8457322859905341, -0.1965949653427006, 0.10574314941958163, -0.06786563501345873, 0.04644402518288106, -0.03250685839030642, 0.02278221067211325, -0.015776461984025044, 0.0106861423046505, -0.007015178439754547, 0.004419776898118025, -0.002640349206419353, 0.0014706075069126929, -0.0007434479010369788, 0.00032434279095271655, -0.00010771945462504619],
    [-8.888904218233107e-05, 0.00030047873971013594, -0.0007247222673508771, 0.0014781413284720366, -0.0027105491893057556, 0.004608923212425362, -0.00740395560020146, 0.011383759722552785, -0.016923835860553745, 0.02455287281769743, -0.03510086512343416, 0.05005558258074104, -0.07253382289668456, 0.11053946484280222, -0.1927372791631574, 0.552445740092612, 0.7115968219479728, -0.210662107454357, 0.11729533434259606, -0.07616916079430923, 0.052359451330621726, -0.036687218160952884, 0.0256897361931744, -0.0177499783487386, 0.011981956177112868, -0.007829952650949394, 0.004903989286442165, -0.0029071517994024823, 0.0016025392331753556, -0.0007981591599705676, 0.00033979186197556166, -0.00010693619853382159],
    [-0.00010693619853382162, 0.0003397918619755617, -0.0007981591599705678, 0.0016025392331753558, -0.002907151799402483, 0.004903989286442166, -0.007829952650949396, 0.011981956177112871, -0.017749978348738604, 0.025689736193174405, -0.03668721816095289, 0.05235945133062174, -0.07616916079430926, 0.11729533434259608, -0.21066210745435704, 0.711596821947973, 0.5524457400926122, -0.19273727916315744, 0.11053946484280223, -0.07253382289668457, 0.05005558258074105, -0.035100865123434175, 0.024552872817697434, -0.016923835860553748, 0.011383759722552787, -0.007403955600201462, 0.004608923212425363, -0.002710549189305756, 0.001478141328472037, -0.0007247222673508772, 0.00030047873971013604, -8.88890421823311e-05],
    [-0.00010771945462504618, 0.0003243427909527165, -0.0007434479010369787, 0.0014706075069126927, -0.0026403492064193527, 0.004419776898118025, -0.007015178439754547, 0.010686142304650498, -0.01577646198402504, 0.02278221067211325, -0.03250685839030642, 0.04644402518288106, -0.06786563501345873, 0.10574314941958161, -0.19659496534270057, 0.845732285990534, 0.38230536106706786, -0.15038409915462567, 0.0884814830053416, -0.05859942936943878, 0.0405771942610876, -0.028468978092835426, 0.01988909208951188, -0.013674291087551773, 0.009164041758758899, -0.00593121232166118, 0.0036689169804141955, -0.002139989213494492, 0.0011539493624898776, -0.0005564555268935539, 0.00022418908603070835, -6.169787761900705e-05],
    [-8.464122840432894e-05, 0.0002430517421209236, -0.0005445803041891528, 0.0010620801545960844, -0.0018880054283359473, 0.0031369765747049742, -0.004950613758014254, 0.007507937684653195, -0.011048146610773036, 0.015920900371157783, -0.022701723557568016, 0.032480746980827084, -0.047702981628914426, 0.07531945801178211, -0.145858673869235, 0.9426947295402057, 0.21555037786454243, -0.09294865839644116, 0.05591630108406594, -0.037337762954957596, 0.02593058478547953, -0.018197543923624927, 0.012695107173726997, -0.008704737233512524, 0.005811272430550032, -0.0037422781404128427, 0.0022998489712809, -0.0013300206219824767, 0.0007088167627000853, -0.0003358453978417591, 0.00013113986569634532, -3.311694388264495e-05],
    [-3.464476442036121e-05, 9.537201341962134e-05, -0.00020919986159042688, 0.0004025328858112204, -0.0007087508284246483, 0.0011691692091189859, -0.0018349008372697964, 0.0027708836089717836, -0.004064703407090631, 0.005846125446777624, -0.008332275252542718, 0.011942377142413333, -0.0176400665654404, 0.028273499065978484, -0.05748200715526913, 0.9935370754684096, 0.06538702643260477, -0.03032170233155439, 0.018598441767977408, -0.012509950835799515, 0.00870982957472023, -0.00611241888772457, 0.004257373975001418, -0.0029109007764240528, 0.0019355921973831006, -0.001239982208738999, 0.0007569269598663377, -0.00043387082062890065, 0.00022840407596707644, -0.00010621819573270597, 4.007603567022298e-05, -9.113131439975678e-06],
];

/// The measured loudness and peaks of one stream. Loudness in LUFS / LU, peaks
/// in dBTP and dBFS. See the module docs for what `-inf` means here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Gated programme loudness, LUFS.
    pub integrated_lufs: f64,
    /// Loudness range, LU (EBU Tech 3342).
    pub range_lu: f64,
    /// Highest true peak across all channels, dBTP.
    pub true_peak_dbtp: f64,
    /// Highest sample magnitude across all channels, dBFS.
    pub sample_peak_dbfs: f64,
    /// Loudest ungated 400 ms momentary block, LUFS.
    pub momentary_max_lufs: f64,
    /// Loudest ungated 3 s short-term block, LUFS.
    pub short_term_max_lufs: f64,
    /// Seconds of audio analysed (whole 100 ms chunks).
    pub analysed_secs: f64,
    /// Gated 2 Hz short-term samples the loudness range was taken from. The
    /// range is a spread only when this is at least two; with fewer, `range_lu`
    /// is `0.0` because there is no spread to measure.
    pub lra_samples: usize,
}

/// One second-order IIR section, direct form I, coefficients already divided by
/// `a0`.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self {
            b,
            a,
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// The two K-weighting stages of BS.1770 for `rate`: the pre-filter high shelf
/// (stage 1) and the RLB high-pass (stage 2). The corner frequencies, gain and
/// Q are the standard's; they are pre-warped through the bilinear transform at
/// the actual rate, the way libebur128 does it.
fn k_weighting(rate: f64) -> (Biquad, Biquad) {
    let f0 = 1_681.974_450_955_533;
    let g = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_4);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad::new(
        [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );

    // The RLB high-pass has a double zero at DC, so its numerator is [1, -2, 1]
    // with no gain: BS.1770's table at 48 kHz is exactly that numerator. Dividing
    // it by a0 as the other stages do cuts about 0.04 dB at every rate, which is
    // what made edith read under ffmpeg's ebur128.
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let hipass = Biquad::new(
        [1.0, -2.0, 1.0],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    (shelf, hipass)
}

/// BS.1770 channel weight for channel `index` of a `channels`-wide stream.
fn channel_weight(index: usize, channels: usize) -> f64 {
    if channels < 6 {
        return 1.0;
    }
    match index {
        3 => 0.0,      // LFE
        4 | 5 => 1.41, // Ls, Rs
        _ => 1.0,
    }
}

/// One channel's true-peak detector: the last `TP_TAPS` samples, newest first,
/// kept twice over so the window is always one contiguous slice.
#[derive(Debug, Clone)]
pub(crate) struct TruePeak {
    hist: [f64; 2 * TP_TAPS],
    pos: usize,
}

impl TruePeak {
    pub(crate) fn new() -> Self {
        Self {
            hist: [0.0; 2 * TP_TAPS],
            pos: 0,
        }
    }

    /// Feed one sample; returns the largest magnitude of the `TP_PHASES`
    /// oversampled points around the sample `TP_TAPS / 2` steps back, which is
    /// the newest point whose window is complete (the interpolator's group
    /// delay is that many samples). Nothing is flushed at the end: the last
    /// samples have no complete window, and a made-up tail would read the
    /// stream's cut as a step.
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.pos = (self.pos + TP_TAPS - 1) % TP_TAPS;
        self.hist[self.pos] = x;
        self.hist[self.pos + TP_TAPS] = x;
        let w = &self.hist[self.pos..self.pos + TP_TAPS];
        let mut peak = 0.0f64;
        for row in &TP_COEF {
            let y: f64 = w.iter().zip(row.iter()).map(|(s, c)| s * c).sum();
            peak = peak.max(y.abs());
        }
        peak
    }
}

/// Streaming BS.1770 meter. Build it with the stream's own rate and channel
/// count, feed it interleaved samples, and ask for the result at any point.
#[derive(Debug, Clone)]
pub struct Meter {
    channels: usize,
    weights: Vec<f64>,
    shelf: Vec<Biquad>,
    hipass: Vec<Biquad>,
    chunk_frames: usize,
    frames_in_chunk: usize,
    chunk_acc: f64,
    /// Weighted mean-square power of each completed 100 ms chunk.
    chunks: Vec<f64>,
    sample_peak: f64,
    tp: Vec<TruePeak>,
    tp_peak: f64,
}

impl Meter {
    /// A meter for `channels` interleaved channels at `sample_rate` Hz.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels).max(1);
        let (shelf, hipass) = k_weighting(f64::from(sample_rate.max(1)));
        Self {
            channels,
            weights: (0..channels).map(|c| channel_weight(c, channels)).collect(),
            shelf: vec![shelf; channels],
            hipass: vec![hipass; channels],
            chunk_frames: (sample_rate as usize / CHUNKS_PER_SEC).max(1),
            frames_in_chunk: 0,
            chunk_acc: 0.0,
            chunks: Vec::new(),
            sample_peak: 0.0,
            tp: vec![TruePeak::new(); channels],
            tp_peak: 0.0,
        }
    }

    /// Feed interleaved samples, `channels` values per frame. A trailing
    /// partial frame is ignored.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            let mut acc = 0.0;
            for (c, &s) in frame.iter().enumerate() {
                let x = f64::from(s);
                self.sample_peak = self.sample_peak.max(x.abs());
                let y = self.hipass[c].run(self.shelf[c].run(x));
                acc += self.weights[c] * y * y;
                let tp = self.tp[c].push(x);
                self.tp_peak = self.tp_peak.max(tp);
            }
            self.chunk_acc += acc;
            self.frames_in_chunk += 1;
            if self.frames_in_chunk == self.chunk_frames {
                self.chunks.push(self.chunk_acc / self.chunk_frames as f64);
                self.chunk_acc = 0.0;
                self.frames_in_chunk = 0;
            }
        }
    }

    /// The measurement so far. Takes `&self` so a caller can read a running
    /// figure and keep pushing. A partly filled 100 ms chunk at the end is not
    /// counted, and the true peak covers the samples whose interpolation is complete.
    pub fn finish(&self) -> Loudness {
        let chunks = &self.chunks;

        // No tail: see `TruePeak::push`.
        let tp_peak = self.tp_peak;

        let momentary: Vec<f64> = (MOMENTARY_CHUNKS - 1..chunks.len())
            .map(|i| window_mean(chunks, i, MOMENTARY_CHUNKS))
            .collect();
        let short_term: Vec<f64> = (SHORT_TERM_CHUNKS - 1..chunks.len())
            .map(|i| window_mean(chunks, i, SHORT_TERM_CHUNKS))
            .collect();

        let integrated = gated_blocks(&momentary, REL_GATE_LU);
        let integrated_lufs = if integrated.is_empty() {
            f64::NEG_INFINITY
        } else {
            lufs(mean(&integrated))
        };

        // LRA: 2 Hz samples of the short-term loudness, gated, then the spread
        // between the 10th and 95th percentile.
        let lra_blocks: Vec<f64> = short_term.iter().step_by(LRA_HOP_CHUNKS).copied().collect();
        let mut lra_values: Vec<f64> = gated_blocks(&lra_blocks, LRA_REL_GATE_LU)
            .into_iter()
            .map(lufs)
            .collect();
        lra_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let lra_samples = lra_values.len();
        let range_lu = if lra_samples < 2 {
            0.0
        } else {
            percentile(&lra_values, 0.95) - percentile(&lra_values, 0.10)
        };

        Loudness {
            integrated_lufs,
            range_lu,
            true_peak_dbtp: db(tp_peak),
            sample_peak_dbfs: db(self.sample_peak),
            momentary_max_lufs: momentary
                .iter()
                .copied()
                .map(lufs)
                .fold(f64::NEG_INFINITY, f64::max),
            short_term_max_lufs: short_term
                .iter()
                .copied()
                .map(lufs)
                .fold(f64::NEG_INFINITY, f64::max),
            analysed_secs: chunks.len() as f64 / CHUNKS_PER_SEC as f64,
            lra_samples,
        }
    }
}

/// Mean power of the `n` chunks ending at index `end`.
fn window_mean(chunks: &[f64], end: usize, n: usize) -> f64 {
    chunks[end + 1 - n..=end].iter().sum::<f64>() / n as f64
}

fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Block powers that survive the absolute gate and then the relative gate
/// `rel_lu` below the absolute-gated mean.
fn gated_blocks(blocks: &[f64], rel_lu: f64) -> Vec<f64> {
    let above_abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&p| lufs(p) > ABS_GATE_LUFS)
        .collect();
    if above_abs.is_empty() {
        return Vec::new();
    }
    let relative = lufs(mean(&above_abs)) + rel_lu;
    above_abs
        .into_iter()
        .filter(|&p| lufs(p) > relative)
        .collect()
}

/// Nearest-rank percentile of an ascending, non-empty slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Mean power -> LUFS (BS.1770 `-0.691 + 10 log10`), `-inf` for no power.
fn lufs(power: f64) -> f64 {
    if power > 0.0 {
        -0.691 + 10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Linear amplitude -> dB, `-inf` for zero.
fn db(amplitude: f64) -> f64 {
    if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Measure the first `stream` of the audio file at `path`, over `range`
/// seconds (`None` for the whole file). Decodes through the same
/// [`AudioSession`] path [`crate::silence::levels`] uses. `Ok(None)` when the
/// file has no such audio stream.
pub fn measure_file(
    path: &Path,
    stream: usize,
    range: Option<(f64, f64)>,
) -> crate::Result<Option<Loudness>> {
    let (from, to) = range.unwrap_or((0.0, f64::INFINITY));
    let sources = [(path.to_path_buf(), stream)];
    let Some((meta, rx)) =
        AudioSession::open_multi_streams(&sources, &[(Some(0), from.max(0.0), to)])?
    else {
        return Ok(None);
    };
    let mut meter = Meter::new(meta.sample_rate, meta.channels);
    for chunk in rx {
        meter.push(&chunk.samples);
    }
    Ok(Some(meter.finish()))
}

/// The dB gain that brings `measured` to `target_lufs`, cut back if needed so
/// the true peak stays at or under `tp_ceiling` dBTP. Silence has no loudness to
/// move, so it returns `0.0`, and so does a measurement whose integrated
/// loudness is `-inf` for any other reason.
pub fn gain_to_target(measured: &Loudness, target_lufs: f64, tp_ceiling: f64) -> f64 {
    if !measured.integrated_lufs.is_finite() {
        return 0.0;
    }
    let mut gain = target_lufs - measured.integrated_lufs;
    if measured.true_peak_dbtp.is_finite() {
        gain = gain.min(tp_ceiling - measured.true_peak_dbtp);
    }
    gain
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Loudness, Meter, TP_COEF, gain_to_target, measure_file};

    /// Interleaved sine: `amp` at `freq` Hz in the channels listed in `live`,
    /// digital zero in the rest.
    fn sine(
        rate: u32,
        secs: f64,
        amp: f64,
        freq: f64,
        phase: f64,
        channels: usize,
        live: &[usize],
    ) -> Vec<f32> {
        let frames = (f64::from(rate) * secs) as usize;
        let mut out = Vec::with_capacity(frames * channels);
        for n in 0..frames {
            let v = amp
                * (2.0 * std::f64::consts::PI * freq * n as f64 / f64::from(rate) + phase).sin();
            for c in 0..channels {
                out.push(if live.contains(&c) { v as f32 } else { 0.0 });
            }
        }
        out
    }

    fn measure(samples: &[f32], rate: u32, channels: u16) -> Loudness {
        let mut m = Meter::new(rate, channels);
        // Push in uneven pieces so chunk boundaries are not what the test
        // happens to be aligned to.
        for piece in samples.chunks(4097 * usize::from(channels)) {
            m.push(piece);
        }
        m.finish()
    }

    /// -20 dBFS 1 kHz sine in the left channel only: BS.1770 gives -3.01 LUFS
    /// for 0 dBFS in one channel, so -23.01 here.
    #[test]
    fn one_channel_tone_at_minus_20_dbfs_reads_minus_23() {
        let amp = 0.1;
        let s = sine(48_000, 10.0, amp, 1000.0, 0.0, 2, &[0]);
        let l = measure(&s, 48_000, 2);
        assert!((l.integrated_lufs - -23.0).abs() < 0.02, "{l:?}");
    }

    /// The same tone in both channels adds 3.01 dB: -20.0 LUFS. (The spec's
    /// "-20 dBFS stereo reads -23" only holds for one channel; both channels
    /// carry the same power twice.)
    #[test]
    fn two_channel_tone_at_minus_20_dbfs_reads_minus_20() {
        let s = sine(48_000, 10.0, 0.1, 1000.0, 0.0, 2, &[0, 1]);
        let l = measure(&s, 48_000, 2);
        assert!((l.integrated_lufs - -20.0).abs() < 0.1, "{l:?}");
    }

    #[test]
    fn same_reading_at_44_1_khz() {
        let s = sine(44_100, 10.0, 0.1, 1000.0, 0.0, 2, &[0]);
        let l = measure(&s, 44_100, 2);
        assert!((l.integrated_lufs - -23.0).abs() < 0.02, "{l:?}");
    }

    #[test]
    fn lra_needs_two_two_hz_samples_and_says_how_many_it_had() {
        // Short-term values start at 3 s and come every 0.5 s: 3.0 s gives one sample, 3.5 s two.
        let three = measure(&sine(48_000, 3.0, 0.1, 440.0, 0.0, 1, &[0]), 48_000, 1);
        assert_eq!(three.lra_samples, 1);
        assert_eq!(three.range_lu, 0.0);
        assert!((three.analysed_secs - 3.0).abs() < 1e-9, "{}", three.analysed_secs);
        let four = measure(&sine(48_000, 4.0, 0.1, 440.0, 0.0, 1, &[0]), 48_000, 1);
        assert!(four.lra_samples >= 2, "{}", four.lra_samples);
    }

    #[test]
    fn silence_is_negative_infinity_and_has_no_range() {
        let s = vec![0.0f32; 48_000 * 2 * 5];
        let l = measure(&s, 48_000, 2);
        assert_eq!(l.integrated_lufs, f64::NEG_INFINITY);
        assert_eq!(l.true_peak_dbtp, f64::NEG_INFINITY);
        assert_eq!(l.sample_peak_dbfs, f64::NEG_INFINITY);
        assert_eq!(l.momentary_max_lufs, f64::NEG_INFINITY);
        assert_eq!(l.range_lu, 0.0);
    }

    /// Ten seconds of silence after the tone is below the absolute gate, so it
    /// must not drag the integrated figure down. The one 400 ms block that
    /// straddles the cut keeps a fraction of the tone and moves the mean by
    /// under 0.1 dB.
    #[test]
    fn gating_ignores_trailing_silence() {
        let tone = sine(48_000, 10.0, 0.1, 1000.0, 0.0, 2, &[0]);
        let alone = measure(&tone, 48_000, 2);
        let mut padded = tone.clone();
        padded.extend(std::iter::repeat_n(0.0f32, 48_000 * 2 * 10));
        let gated = measure(&padded, 48_000, 2);
        assert!(
            (gated.integrated_lufs - alone.integrated_lufs).abs() < 0.1,
            "alone {alone:?} padded {gated:?}"
        );
    }

    #[test]
    fn full_scale_sine_true_peak_is_about_zero_dbtp() {
        let s = sine(48_000, 1.0, 1.0, 1000.0, 0.0, 1, &[0]);
        let l = measure(&s, 48_000, 1);
        assert!(l.true_peak_dbtp.abs() < 0.1, "{l:?}");
        assert!(l.sample_peak_dbfs.abs() < 0.1, "{l:?}");
    }

    /// fs/4 sine at 45 degrees: every sample is +-0.707 (-3.01 dBFS), but the
    /// waveform between samples reaches 1.0. The true peak has to see that.
    #[test]
    fn intersample_peak_is_found_above_sample_peak() {
        let s = sine(
            48_000,
            1.0,
            1.0,
            12_000.0,
            std::f64::consts::FRAC_PI_4,
            1,
            &[0],
        );
        let l = measure(&s, 48_000, 1);
        assert!((l.sample_peak_dbfs - -3.01).abs() < 0.05, "{l:?}");
        assert!(l.true_peak_dbtp.abs() < 0.2, "{l:?}");
        assert!(l.true_peak_dbtp > l.sample_peak_dbfs + 2.5, "{l:?}");
    }

    /// fs/4 at 45 degrees with the crest at amplitude sqrt(2): the samples are
    /// +-1.0 (0 dBFS) and the waveform between them reaches +3.01 dBTP. The 4x
    /// grid sits 1/8 sample from the crest, which costs about 0.17 dB, so the
    /// BS.1770 reading is close to, not exactly, the continuous crest.
    #[test]
    fn fs_over_4_sine_at_45_degrees_reads_about_3_dbtp() {
        let s = sine(
            48_000,
            1.0,
            std::f64::consts::SQRT_2,
            12_000.0,
            std::f64::consts::FRAC_PI_4,
            1,
            &[0],
        );
        let l = measure(&s, 48_000, 1);
        assert!(l.sample_peak_dbfs.abs() < 0.05, "{l:?}");
        assert!((l.true_peak_dbtp - 3.0).abs() < 0.2, "{l:?}");
    }

    /// Each phase of the interpolator has unit DC gain, so a flat signal reads
    /// its own level at every oversampled point: the table is normalised as the
    /// comment on [`TP_COEF`] says it is.
    #[test]
    fn every_interpolator_phase_has_unit_dc_gain() {
        for (p, row) in TP_COEF.iter().enumerate() {
            let sum: f64 = row.iter().sum();
            assert!((sum - 1.0).abs() < 1e-9, "phase {p} sums to {sum}");
        }
    }

    /// A 997 Hz sine at -6 dBFS is well inside the passband, so its true peak
    /// reads its amplitude, -6 dBTP.
    #[test]
    fn a_997_hz_sine_at_minus_6_dbfs_reads_minus_6_dbtp() {
        let amp = 10f64.powf(-6.0 / 20.0);
        let s = sine(48_000, 1.0, amp, 997.0, 0.3, 1, &[0]);
        let l = measure(&s, 48_000, 1);
        assert!((l.true_peak_dbtp - -6.0).abs() < 0.1, "{l:?}");
    }

    #[test]
    fn measure_file_reads_the_test_tone() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_tone.wav");
        let l = measure_file(&path, 0, None)
            .expect("decode")
            .expect("has audio");
        assert!(l.integrated_lufs.is_finite(), "{l:?}");
        assert!((-60.0..0.0).contains(&l.integrated_lufs), "{l:?}");
        assert!(
            l.true_peak_dbtp.is_finite() && l.true_peak_dbtp <= 0.5,
            "{l:?}"
        );
        assert!(l.sample_peak_dbfs <= l.true_peak_dbtp + 0.5, "{l:?}");
    }

    /// At 48 kHz the two K-weighting stages are BS.1770-4's own table, to the
    /// digits the standard prints.
    #[test]
    fn k_weighting_at_48_khz_is_the_bs1770_table() {
        let (shelf, hipass) = super::k_weighting(48_000.0);
        let close = |got: &[f64], want: &[f64]| {
            got.iter()
                .zip(want)
                .all(|(g, w)| (g - w).abs() < 1e-10)
        };
        assert!(close(
            &shelf.b,
            &[1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85]
        ));
        assert!(close(&shelf.a, &[-1.690_659_293_182_41, 0.732_480_774_215_85]));
        assert!(close(&hipass.b, &[1.0, -2.0, 1.0]));
        assert!(close(&hipass.a, &[-1.990_047_454_833_98, 0.990_072_250_366_21]));
    }

    /// Readings checked against ffmpeg's `ebur128` on the same files (see the
    /// notes on each test for the values). ffmpeg is the reference, not the
    /// standard's own text, because the standard only prints 48 kHz tables.
    #[test]
    fn one_channel_997_hz_at_minus_20_dbfs_matches_ffmpeg() {
        // ffmpeg: -23.016. 0.1 amplitude is -20 dBFS; one channel reads 3 dB under -20.
        let s = sine(48_000, 10.0, 0.1, 997.0, 0.0, 1, &[0]);
        let l = measure(&s, 48_000, 1);
        assert!((l.integrated_lufs - -23.016).abs() < 0.01, "{l:?}");
    }

    #[test]
    fn stereo_997_hz_at_minus_23_dbfs_matches_ffmpeg() {
        // ffmpeg: -23.006 for -23 dBFS in both channels.
        let amp = 10f64.powf(-23.0 / 20.0);
        let s = sine(48_000, 10.0, amp, 997.0, 0.0, 2, &[0, 1]);
        let l = measure(&s, 48_000, 2);
        assert!((l.integrated_lufs - -23.006).abs() < 0.01, "{l:?}");
    }

    #[test]
    fn gain_is_limited_by_true_peak_ceiling() {
        let m = Loudness {
            integrated_lufs: -23.0,
            range_lu: 0.0,
            true_peak_dbtp: -3.0,
            sample_peak_dbfs: -4.0,
            momentary_max_lufs: -20.0,
            short_term_max_lufs: -21.0,
            analysed_secs: 10.0,
            lra_samples: 0,
        };
        // Loudness alone would allow +7 dB; the peak allows only +2 dB.
        assert!((gain_to_target(&m, -16.0, -1.0) - 2.0).abs() < 1e-9);
        // Quiet material with headroom takes the full loudness gain.
        assert!((gain_to_target(&m, -26.0, -1.0) - -3.0).abs() < 1e-9);
        let silent = Loudness {
            integrated_lufs: f64::NEG_INFINITY,
            true_peak_dbtp: f64::NEG_INFINITY,
            ..m
        };
        assert_eq!(gain_to_target(&silent, -16.0, -1.0), 0.0);
    }
}
