//! The AC-3 films: the decode seat exercised on the kind of tracks it exists
//! for -- a BluRay remux's 48 kHz AC-3 5.1 and a web remux's 5.1 E-AC-3
//! (Dolby Digital Plus) -- decoded, folded to the pair the timeline carries,
//! seeked across each film, and correlated against ffmpeg's own decode of the
//! same windows. A wrong channel order, a wrong rate, or a fold that lost a
//! side all move the correlation; a seat that refuses the track fails the
//! open.
//!
//! Mirrors `audio_seek`'s film witnesses, which make the same claim of the
//! Opus and multichannel AAC seats. Skipped, not failed, where the film or
//! ffmpeg is not -- these are named by the local `real_library.toml`, not
//! fixtures in this repository.

use engine::audio::AudioSession;

/// The AC-3 5.1 film: seeked to three seconds spread across the film and
/// correlated against ffmpeg's decode of the same window. The seat's §7.8.2
/// fold and ffmpeg's `-ac 2` disagree by roughly 7.7 dB of level, which is
/// why the comparison is level-normalised (`best_lag`).
#[test]
fn a_seek_into_his_ac3_film_lands_on_the_second_it_asked_for() {
    let Some(film) = engine::real_library::film("ac3_51_film") else {
        return;
    };
    if no_ffmpeg() {
        return;
    }
    const SEARCH: f64 = 5.0;
    for want in [300.0, 600.0, 900.0] {
        let (meta, rx) = AudioSession::open_at(&film, want)
            .expect("open")
            .expect("the film has audio");
        assert_eq!(meta.sample_rate, 48_000);
        assert_eq!(meta.channels, 2, "5.1 reaches the timeline as a pair");
        let probe = one_second(&meta, rx);
        let from = want - SEARCH;
        let reference = ffmpeg_mono(&film, from, 2.0 * SEARCH + 2.0, meta.sample_rate);
        let (lag, score) = best_lag(&probe, &reference);
        let at = from + lag as f64 / f64::from(meta.sample_rate);
        eprintln!("asked {want}s, content at {at:.3}s (correlation {score:.3})");
        assert!(
            score > 0.5,
            "asked {want}s: nothing in the reference window correlates ({score:.3}) -- \
             the measurement, not the seek, is what failed"
        );
        assert!(
            (at - want).abs() <= 0.05,
            "asked {want}s, heard {at:.3}s -- {:+.3}s of sound against picture",
            at - want
        );
    }
}

/// The E-AC-3 film: the Dolby Digital Plus class a web remux carries, whose
/// blocks are fixed-laced in Matroska and whose frames are not the fixed
/// 1536-sample AC-3 shape. Same claim, same measurement.
#[test]
fn a_seek_into_his_eac3_film_lands_on_the_second_it_asked_for() {
    let Some(film) = engine::real_library::film("hevc_4k_hdr") else {
        return;
    };
    if no_ffmpeg() {
        return;
    }
    const SEARCH: f64 = 5.0;
    for want in [600.0, 1800.0, 3600.0] {
        let (meta, rx) = AudioSession::open_at(&film, want)
            .expect("open")
            .expect("the film has audio");
        assert_eq!(meta.sample_rate, 48_000);
        assert_eq!(meta.channels, 2, "5.1 reaches the timeline as a pair");
        let probe = one_second(&meta, rx);
        let from = want - SEARCH;
        let reference = ffmpeg_mono(&film, from, 2.0 * SEARCH + 2.0, meta.sample_rate);
        let (lag, score) = best_lag(&probe, &reference);
        let at = from + lag as f64 / f64::from(meta.sample_rate);
        eprintln!("asked {want}s, content at {at:.3}s (correlation {score:.3})");
        assert!(
            score > 0.5,
            "asked {want}s: nothing in the reference window correlates ({score:.3}) -- \
             the measurement, not the seek, is what failed"
        );
        assert!(
            (at - want).abs() <= 0.05,
            "asked {want}s, heard {at:.3}s -- {:+.3}s of sound against picture",
            at - want
        );
    }
}

/// True, with the skip said, when ffmpeg is not there to decode a reference.
fn no_ffmpeg() -> bool {
    let missing = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true);
    if missing {
        eprintln!("skipped: no ffmpeg to decode the reference window with");
    }
    missing
}

/// One second of the opened session, summed to mono: the probe `best_lag`
/// locates inside ffmpeg's window.
fn one_second(
    meta: &engine::audio::AudioMeta,
    rx: std::sync::mpsc::Receiver<engine::audio::AudioChunk>,
) -> Vec<f64> {
    let channels = usize::from(meta.channels);
    let mut probe: Vec<f64> = Vec::with_capacity(meta.sample_rate as usize);
    for chunk in rx {
        probe.extend(
            chunk
                .samples
                .chunks_exact(channels)
                .map(|f| f.iter().map(|s| f64::from(*s)).sum::<f64>() / channels as f64),
        );
        if probe.len() >= meta.sample_rate as usize {
            break;
        }
    }
    probe.truncate(meta.sample_rate as usize);
    assert_eq!(probe.len(), meta.sample_rate as usize, "a second to locate");
    probe
}

/// `dur` seconds of `path`'s first audio track from `start`, decoded by ffmpeg,
/// summed to mono at `rate`. The reference this engine's own landing is measured
/// against, because a decoder cannot be its own witness about where it landed.
fn ffmpeg_mono(path: &std::path::Path, start: f64, dur: f64, rate: u32) -> Vec<f64> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss"])
        .arg(format!("{start}"))
        .arg("-t")
        .arg(format!("{dur}"))
        .arg("-i")
        .arg(path)
        .args(["-map", "0:a:0", "-ac", "1", "-ar"])
        .arg(rate.to_string())
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|b| f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        .collect()
}

/// Where in `reference` the `probe` sits, and how well: the offset in samples of
/// the best normalised cross-correlation, which is 1.0 for the same content and
/// falls away fast for anything else.
///
/// Coarse then fine, because the naive product over a ten-second window at
/// 48 kHz is 3e10 multiplies: both signals are averaged down by [`DECIMATE`]
/// first -- a crude low-pass, and the peak of a correlation is broad -- then the
/// winner is refined at full rate over the one block around it. The normalisation
/// is what makes the two comparable at all, since this fold is 7.7 dB quieter
/// than ffmpeg's.
fn best_lag(probe: &[f64], reference: &[f64]) -> (usize, f64) {
    const DECIMATE: usize = 32;
    let down = |xs: &[f64]| -> Vec<f64> {
        xs.chunks_exact(DECIMATE)
            .map(|c| c.iter().sum::<f64>() / DECIMATE as f64)
            .collect()
    };
    let (coarse, at) = scan(&down(probe), &down(reference), 1);
    let from = (at * DECIMATE).saturating_sub(DECIMATE);
    let span = 3 * DECIMATE + probe.len();
    // The score reported is the coarse one, over the whole search window: the
    // fine pass only sharpens the offset inside a block it has already chosen,
    // and a score over a window that short says nothing about the match.
    let (_, offset) = scan(
        probe,
        &reference[from.min(reference.len())..(from + span).min(reference.len())],
        1,
    );
    (from + offset, coarse)
}

/// The best normalised cross-correlation of `probe` over `reference`, at `step`
/// samples: `(score, offset)`. Both are mean-removed; the denominator is each
/// window's own energy, so a level difference between the two decoders cannot
/// move the peak.
fn scan(probe: &[f64], reference: &[f64], step: usize) -> (f64, usize) {
    if reference.len() <= probe.len() || probe.is_empty() {
        return (0.0, 0);
    }
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    let p: Vec<f64> = {
        let m = mean(probe);
        probe.iter().map(|s| s - m).collect()
    };
    let pe = p.iter().map(|s| s * s).sum::<f64>().sqrt();
    if pe <= 0.0 {
        return (0.0, 0);
    }
    let (mut best, mut at) = (0.0, 0);
    for offset in (0..reference.len() - p.len()).step_by(step) {
        let window = &reference[offset..offset + p.len()];
        let m = mean(window);
        let (mut dot, mut energy) = (0.0, 0.0);
        for (a, b) in p.iter().zip(window) {
            let b = b - m;
            dot += a * b;
            energy += b * b;
        }
        let score = match energy > 0.0 {
            true => dot / (pe * energy.sqrt()),
            false => 0.0,
        };
        if score > best {
            best = score;
            at = offset;
        }
    }
    (best, at)
}
