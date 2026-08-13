//! The equalizer graph: its geometry, its analyser and its curve.

use crate::*;

/// Where a frequency sits across the graph, 0..1. Log, because an octave is an
/// octave whether it is 40 Hz wide or 10 kHz wide -- a linear axis would spend
/// three quarters of the card on the top two octaves and squeeze the bass, the
/// half of the range people actually reach for, into nothing.
pub(crate) fn eq_x(freq_hz: f32) -> f32 {
    let span = (EQ_FREQ_HIGH / EQ_FREQ_LOW).log10();
    ((freq_hz.max(1.) / EQ_FREQ_LOW).log10() / span).clamp(0., 1.)
}

/// The frequency at a fraction across the graph -- [`eq_x`] backwards, which is
/// how a drag and the curve's own sample points read one. Clamped to the axis
/// either way, so a pointer that leaves the box stops at 20 Hz or 20 kHz.
pub(crate) fn eq_freq(along: f32) -> f32 {
    EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along.clamp(0., 1.))
}

/// The band an "add" makes: a flat peak half way -- in octaves, which is what
/// the log axis draws -- between the picked band and whatever sits above it, so
/// a new band lands in the gap on screen rather than on top of its neighbour.
/// Above the topmost band the gap is the rest of the axis.
pub(crate) fn inserted_band(bands: &[Band], after: usize) -> Band {
    let below = bands.get(after).map_or(1000., |b| b.freq_hz);
    let above = bands
        .iter()
        .map(|b| b.freq_hz)
        .filter(|f| *f > below)
        .min_by(f32::total_cmp)
        .unwrap_or(EQ_FREQ_HIGH);
    Band {
        freq_hz: (below * above).sqrt().clamp(EQ_FREQ_LOW, EQ_FREQ_HIGH),
        gain_db: 0.,
        // A shade narrower than the flat-shelf 0.707 the defaults use: a band
        // someone asked for is a band they mean to aim, and a wide one aimed at
        // 300 Hz is really a band at everything.
        q: 1.,
        kind: BandKind::Peak,
    }
}

/// Where a gain sits *down* the graph, 0..1 from the top: flat is the middle,
/// so a cut reads as a dip below the line it is a cut from. The inverse of
/// [`Player::drag_band`]'s reading of the pointer, and clamped like it, so a
/// curve loaded from a file with a gain past the card's limit paints on the
/// edge of the box rather than outside it.
pub(crate) fn eq_y(gain_db: f32) -> f32 {
    0.5 - (gain_db / EQ_GAIN_LIMIT).clamp(-1., 1.) / 2.
}

/// A frequency as the card writes it. Two decimals of a kHz at most, with the
/// zeroes trimmed, so "1 kHz" stays "1 kHz" and a band nudged off it reads as
/// 1.12 kHz rather than as the same "1 kHz" it was before the keystroke -- a
/// number that does not move under an edit is worse than no number.
pub(crate) fn eq_freq_label(freq_hz: f32) -> String {
    if freq_hz < 1000. {
        return format!("{freq_hz:.0} Hz");
    }
    let khz = format!("{:.2}", freq_hz / 1000.);
    let khz = khz.trim_end_matches('0').trim_end_matches('.');
    format!("{khz} kHz")
}

/// What a band row calls itself: the corner or centre frequency, and for a
/// shelf the fact that it tilts everything past it -- which is the difference
/// between "12 kHz" moving the last octave and moving one band inside it.
pub(crate) fn band_label(band: &Band) -> String {
    let freq = eq_freq_label(band.freq_hz);
    match band.kind {
        BandKind::LowShelf => format!("{freq} low shelf"),
        BandKind::HighShelf => format!("{freq} high shelf"),
        BandKind::Peak => freq,
    }
}

/// In-place radix-2 FFT of `re`/`im`, whose length must be a power of two.
///
/// Hand-written, and deliberately: a 1024-point transform once a frame is a
/// few tens of microseconds of plain arithmetic, and a dependency for it would
/// be a build cost this editor pays on every compile for one card's backdrop.
pub(crate) fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // Decimation in time: the input is first put in bit-reversed order, after
    // which the butterflies run over neighbours.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut span = 2;
    while span <= n {
        let step = std::f32::consts::TAU / span as f32;
        for start in (0..n).step_by(span) {
            for k in 0..span / 2 {
                // e^{-i2πk/span}: the negative sign is the forward transform.
                let (sin, cos) = (-step * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + span / 2);
                let (tr, ti) = (cos * re[b] - sin * im[b], cos * im[b] + sin * re[b]);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        span <<= 1;
    }
}

/// The played signal as one height per curve point, 0 (silence) to 1 (the top
/// of the box) -- the analyser the response curve is drawn on top of.
///
/// The newest [`EQ_FFT`] samples of the engine's tap, Hann-windowed so a tone
/// that does not land on a bin centre is one hump rather than a smear across
/// the axis. Each column takes the *loudest* bin between it and its
/// neighbours: the axis is logarithmic, so one column near 20 Hz is a fraction
/// of a bin while one near 20 kHz is hundreds, and averaging those would sink
/// every peak up there into the noise beside it.
///
/// Empty -- nothing to draw -- for a tap too short to transform, which is what
/// a session has just after a seek.
///
/// corner-cut: one transform length for the whole axis, so the bass end is a bin
/// (47 Hz at 48 kHz) wide however many columns are drawn across it -- a 60 Hz
/// hum and an 80 Hz one are the same hump down there. Upgrade path is the
/// analyser every mastering EQ uses: two or three transforms of different
/// lengths, each drawn over the octaves it resolves.
pub(crate) fn eq_spectrum(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    if samples.len() < EQ_FFT {
        return Vec::new();
    }
    let tail = &samples[samples.len() - EQ_FFT..];
    let mut re: Vec<f32> = tail
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let w = 0.5 * (1. - (std::f32::consts::TAU * i as f32 / EQ_FFT as f32).cos());
            s * w
        })
        .collect();
    let mut im = vec![0.; EQ_FFT];
    fft(&mut re, &mut im);
    // Magnitude per bin, scaled so a full-scale sine reads 0 dBFS: half the
    // energy is in the mirrored half of the transform, and the Hann window
    // takes another factor of two off.
    let mags: Vec<f32> = (0..EQ_FFT / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * 4. / EQ_FFT as f32)
        .collect();
    let bin_hz = sample_rate as f32 / EQ_FFT as f32;
    let (floor, ceiling) = EQ_SPECTRUM_DB;
    let at = |along: f32| EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along);
    (0..=EQ_CURVE_STEPS)
        .map(|step| {
            let along = step as f32 / EQ_CURVE_STEPS as f32;
            let half = 0.5 / EQ_CURVE_STEPS as f32;
            // Bin 0 is DC and means nothing here, so the low end starts at 1.
            let low = (at(along - half) / bin_hz).round().max(1.) as usize;
            let high = (at(along + half) / bin_hz).round().max(1.) as usize;
            let peak = (low..=high)
                .filter_map(|k| mags.get(k))
                .fold(0f32, |a, &b| a.max(b));
            let db = 20. * peak.max(1e-9).log10();
            ((db - floor) / (ceiling - floor)).clamp(0., 1.)
        })
        .collect()
}

/// The analyser drawn as one filled shape from the floor of the box.
pub(crate) fn eq_spectrum_curve(levels: Vec<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let last = levels.len().saturating_sub(1).max(1) as f32;
            let mut path = PathBuilder::fill();
            path.move_to(point(o.x, o.y + s.height));
            for (i, level) in levels.iter().enumerate() {
                path.line_to(point(
                    o.x + s.width * (i as f32 / last),
                    o.y + s.height * (1. - level),
                ));
            }
            path.line_to(point(o.x + s.width, o.y + s.height));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, rgba(EQ_SPECTRUM_INK()));
            }
        },
    )
    .absolute()
    .size_full()
}

/// The cascade's frequency response, drawn as one line across the graph, with
/// each band's own response threaded dimly under it.
///
/// Every point comes from `EqParams::response_db`, which reads the very
/// coefficients the samples are filtered through: the curve cannot drift from
/// what is heard, because there is no second copy of the maths. A single band's
/// thread is that same call on a cascade of one, so the two cannot disagree
/// either -- and it is what makes a boost sitting inside a cut visible at all,
/// where the sum alone would draw a flat line and say nothing.
///
/// corner-cut: bands that overlap can sum past the ±`EQ_GAIN_LIMIT` axis and the
/// curve then rides the edge of the box; upgrade = a wider dB axis with the
/// handles and [`Player::drag_band`] rescaled to it.
pub(crate) fn eq_curve(params: EqParams, sample_rate: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let line = |of: &EqParams| -> Vec<_> {
                (0..=EQ_CURVE_STEPS)
                    .map(|step| {
                        let along = step as f32 / EQ_CURVE_STEPS as f32;
                        point(
                            o.x + s.width * along,
                            o.y + s.height
                                * eq_y(of.response_db(eq_freq(along), sample_rate)),
                        )
                    })
                    .collect()
            };
            // One thread per band, first, so the sum is drawn over them.
            for band in &params.bands {
                let mut bell = PathBuilder::stroke(px(1.));
                for (step, at) in line(&EqParams {
                    bands: vec![*band],
                })
                .into_iter()
                .enumerate()
                {
                    match step {
                        0 => bell.move_to(at),
                        _ => bell.line_to(at),
                    }
                }
                if let Ok(bell) = bell.build() {
                    window.paint_path(bell, rgba(EQ_BELL_INK()));
                }
            }
            let points = line(&params);
            // The area between the curve and 0 dB, closed along that line: a
            // boost and a cut wind opposite ways around it, which is exactly
            // what makes both of them fill and the flat parts stay empty.
            let mut area = PathBuilder::fill();
            area.move_to(point(o.x, o.y + s.height / 2.));
            for at in &points {
                area.line_to(*at);
            }
            area.line_to(point(o.x + s.width, o.y + s.height / 2.));
            area.close();
            if let Ok(area) = area.build() {
                window.paint_path(area, rgba(EQ_FILL_INK()));
            }
            let mut path = PathBuilder::stroke(px(2.));
            for (step, at) in points.into_iter().enumerate() {
                match step {
                    0 => path.move_to(at),
                    _ => path.line_to(at),
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(ACCENT_PRIMARY()));
            }
        },
    )
    .absolute()
    .size_full()
}
