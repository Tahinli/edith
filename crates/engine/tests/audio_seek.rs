//! `AudioSession::open_at` must produce exactly the tail of a full run: same
//! samples, same absolute `start_sample` numbering, no gaps at the splice.

use std::path::PathBuf;

use engine::audio::{AudioChunk, AudioSession};

const RATE: u32 = 44100;
const CHANNELS: usize = 2;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Drains a session into one flat interleaved buffer, checking as it goes that
/// chunks are contiguous and that the first one lands on `want_start`.
fn drain(start_secs: f64, want_start: u64) -> Vec<f32> {
    let (meta, rx) = AudioSession::open_at(asset("test_av.mp4"), start_secs)
        .expect("open")
        .expect("test_av.mp4 has an audio track");
    assert_eq!((meta.sample_rate, meta.channels as usize), (RATE, CHANNELS));

    let mut samples = Vec::new();
    let mut next = want_start;
    for (
        i,
        AudioChunk {
            start_sample,
            samples: s,
        },
    ) in rx.into_iter().enumerate()
    {
        assert_eq!(start_sample, next, "chunk {i} start at {start_secs}s");
        assert!(!s.is_empty(), "empty chunk {i} at {start_secs}s");
        next += (s.len() / CHANNELS) as u64;
        samples.extend(s);
    }
    samples
}

/// Largest per-sample difference, and where it is.
fn max_diff(a: &[f32], b: &[f32]) -> (f32, usize) {
    a.iter()
        .zip(b)
        .enumerate()
        .map(|(i, (x, y))| ((x - y).abs(), i))
        .fold((0.0, 0), |m, d| if d.0 > m.0 { d } else { m })
}

#[test]
fn open_at_zero_matches_open() {
    let (mut full, mut at_zero) = (Vec::new(), Vec::new());
    for chunk in AudioSession::open(asset("test_av.mp4")).unwrap().unwrap().1 {
        full.extend(chunk.samples);
    }
    for chunk in AudioSession::open_at(asset("test_av.mp4"), 0.0)
        .unwrap()
        .unwrap()
        .1
    {
        at_zero.extend(chunk.samples);
    }
    assert_eq!(full.len(), at_zero.len());
    assert_eq!(full, at_zero, "open() must be open_at(0.0)");
}

#[test]
fn open_at_two_seconds_is_the_tail_of_a_full_run() {
    let full = drain(0.0, 0);
    let target = 2 * RATE as u64;
    let tail = drain(2.0, target);

    let want = &full[target as usize * CHANNELS..];
    assert_eq!(tail.len(), want.len(), "seeked run is a different length");
    if tail == want {
        eprintln!("bit-exact: {} samples", tail.len());
        return;
    }
    // Not bit-exact, and it never will be: perceptual noise substitution draws
    // from an LCG seeded once per decoder (symphonia-codec-aac cpe.rs:32,44), so
    // a run that skipped 86 packets draws different noise in PNS bands forever.
    // The MDCT overlap the pre-roll exists for *is* exact — the first packets
    // after the splice match sample for sample.
    let (diff, at) = max_diff(&tail, want);
    eprintln!("max abs diff {diff:e} at sample {at} of {}", tail.len());
    assert!(diff < 1e-3, "max abs diff {diff} at sample {at}");
}

#[test]
fn open_at_mid_first_packet_is_exact() {
    // 500 samples in: inside packet 1, before any pre-roll exists to walk back
    // to, and it forces the drain-inside-a-chunk trim path.
    let full = drain(0.0, 0);
    let start = 500u64;
    let tail = drain(start as f64 / RATE as f64, start);
    let want = &full[start as usize * CHANNELS..];
    assert_eq!(tail.len(), want.len());
    assert_eq!(tail, want, "no decoder state to diverge from at packet 1");
}

#[test]
fn open_at_past_duration_ends_clean() {
    let tail = drain(60.0, 60 * RATE as u64);
    assert!(
        tail.is_empty(),
        "{} samples past the end of a 5 s file",
        tail.len()
    );
}

/// The seek lands on the second it was asked for, in *content*, ten and twenty
/// seconds into a file that has a cue table.
///
/// That is not what one symphonia seek gives on a Matroska file: it goes
/// through the cues and lands on the first frame at or past the *cluster* the
/// cue names, which on this fixture is up to 4.99 s later than the request
/// (asking 25.0 s landed the reader at 29.987 s) and on his 5.1 Opus film was
/// 1.47 s at 600 s and 3.39 s at 610 s. Nothing downstream can recover samples
/// the reader is already past, so the film simply played from there while the
/// timeline said otherwise -- sound against picture, after every scrub.
///
/// The chirp is what makes that measurable without a reference decode: 200 Hz
/// rising 160 Hz a second, so the frequency of a quarter second of audio names
/// the second of the file it was cut from.
#[test]
fn a_seek_into_a_cued_matroska_lands_on_its_own_second() {
    for start in [5.0, 12.5, 25.0] {
        let (meta, rx) = AudioSession::open_at(asset("test_seek_chirp.mkv"), start)
            .expect("open")
            .expect("test_seek_chirp.mkv has an audio track");
        let at = chirp_second(&meta, rx.into_iter().flat_map(|c| c.samples));
        assert!(
            (at - start).abs() < 0.1,
            "asked {start}s of the chirp, heard {at:.3}s"
        );
    }
}

/// ...and the *second* segment of one session lands too. Two clips of one file
/// is two seeks on one reader, which is where symphonia's mkv reader gives up
/// altogether: it hands out the frames left over from the first landing and
/// then reads on from wherever its iterator ended up -- measured on his film as
/// the second seek playing the *first cluster* of it, 0.094 s in, whatever it
/// was asked for.
#[test]
fn the_second_segment_of_a_session_lands_too() {
    let (meta, rx) =
        AudioSession::open_segments(asset("test_seek_chirp.mkv"), &[(5.0, 6.0), (22.5, 23.5)])
            .expect("open")
            .expect("test_seek_chirp.mkv has an audio track");
    let channels = usize::from(meta.channels);
    let samples: Vec<f32> = rx.into_iter().flat_map(|c| c.samples).collect();
    let second = meta.sample_rate as usize * channels;
    assert_eq!(samples.len(), 2 * second, "two one-second segments");
    for (want, cut) in [(5.0, &samples[..second]), (22.5, &samples[second..])] {
        let at = chirp_second(&meta, cut.iter().copied());
        assert!(
            (at - want).abs() < 0.1,
            "segment from {want}s of the chirp came out as {at:.3}s"
        );
    }
}

/// Where in `test_seek_chirp.mkv` a stream of samples was cut from, by the
/// frequency of its first quarter second: the chirp starts at 200 Hz and rises
/// 160 Hz a second, and zero crossings date it without a reference decode.
fn chirp_second(meta: &engine::audio::AudioMeta, samples: impl Iterator<Item = f32>) -> f64 {
    const WINDOW: f64 = 0.25;
    let channels = usize::from(meta.channels);
    let want = (WINDOW * f64::from(meta.sample_rate)) as usize * channels;
    let first: Vec<f32> = samples.take(want).step_by(channels).collect();
    assert_eq!(first.len(), want / channels, "short of a window to date");
    let crossings = first
        .windows(2)
        .filter(|p| (p[0] < 0.0) != (p[1] < 0.0))
        .count();
    // The window's *midpoint* is what its mean frequency dates.
    (crossings as f64 / 2.0 / WINDOW - 200.0) / 160.0 - WINDOW / 2.0
}

/// His own film, which is where the desync was measured and where the fixture
/// cannot follow: a 5 GB AV1 remux with a 5.1 Opus track and a cue table, seeked
/// to three seconds spread across two hours and correlated against ffmpeg's
/// decode of the same window.
///
/// The chirp fixture above proves the rule on 30 seconds of one cluster layout.
/// This proves it where it broke: `fix(audio): a seek into a Matroska lands on
/// the second it asked for` measured +1.473 s at 600 s and +3.394 s at 610 s on
/// this file and left no runnable check behind it, so a regression would come
/// back the way the original did -- through his ears, after a scrub.
///
/// Skipped, not failed, without the film or without ffmpeg: a claim about his
/// library cannot be made by a machine that does not have it.
#[test]
fn a_seek_into_his_film_lands_on_the_second_it_asked_for() {
    let Some(film) = engine::real_library::film("av1_opus_51") else {
        return;
    };
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipped: no ffmpeg to decode the reference window with");
        return;
    }
    // Five seconds either side of the request: the landings this exists to catch
    // were +1.5 and +3.4 s, and a narrow search is a search that cannot pick the
    // wrong repeat of a phrase.
    const SEARCH: f64 = 5.0;
    for want in [600.0, 610.0, 3600.0] {
        let (meta, rx) = AudioSession::open_at(&film, want)
            .expect("open")
            .expect("the film has audio");
        let rate = f64::from(meta.sample_rate);
        let channels = usize::from(meta.channels);
        // One second of what came out, summed to mono: what is being located is
        // the content, and a fold's channel balance is not part of that. Taken a
        // chunk at a time and then dropped -- this window is open-ended, and
        // draining it is decoding the rest of a two-hour film.
        let mut probe: Vec<f32> = Vec::with_capacity(meta.sample_rate as usize);
        for chunk in &rx {
            probe.extend(
                chunk
                    .samples
                    .chunks_exact(channels)
                    .map(|f| f.iter().sum::<f32>() / channels as f32),
            );
            if probe.len() >= meta.sample_rate as usize {
                break;
            }
        }
        drop(rx);
        probe.truncate(meta.sample_rate as usize);
        assert_eq!(probe.len(), meta.sample_rate as usize, "a second to locate");

        let from = want - SEARCH;
        let reference = ffmpeg_mono(&film, from, 2.0 * SEARCH + 2.0, meta.sample_rate);
        let (lag, score) = best_lag(&probe, &reference);
        let at = from + lag as f64 / rate;
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

/// `dur` seconds of `path`'s first audio track from `start`, decoded by ffmpeg,
/// summed to mono at `rate`. The reference this engine's own landing is measured
/// against, because a decoder cannot be its own witness about where it landed.
fn ffmpeg_mono(path: &std::path::Path, start: f64, dur: f64, rate: u32) -> Vec<f32> {
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
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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
fn best_lag(probe: &[f32], reference: &[f32]) -> (usize, f64) {
    const DECIMATE: usize = 32;
    let down = |xs: &[f32]| -> Vec<f64> {
        xs.chunks_exact(DECIMATE)
            .map(|c| c.iter().map(|s| f64::from(*s)).sum::<f64>() / DECIMATE as f64)
            .collect()
    };
    let (coarse, at) = scan(&down(probe), &down(reference), 1);
    let from = (at * DECIMATE).saturating_sub(DECIMATE);
    let span = 3 * DECIMATE + probe.len();
    // The score reported is the coarse one, over the whole search window: the
    // fine pass only sharpens the offset inside a block it has already chosen,
    // and a score over a window that short says nothing about the match.
    let (_, offset) = scan(
        &probe.iter().map(|s| f64::from(*s)).collect::<Vec<f64>>(),
        &reference[from.min(reference.len())..(from + span).min(reference.len())]
            .iter()
            .map(|s| f64::from(*s))
            .collect::<Vec<f64>>(),
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
