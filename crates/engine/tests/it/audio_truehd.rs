//! Dolby TrueHD in Matroska, decoded through `ec-truehd`.
//!
//! The fixtures are made here, at test time, by ffmpeg's own TrueHD encoder: a
//! stereo and a 5.1 sine in a `.mka`. The stereo track is held to ffmpeg's
//! decode sample for sample, since TrueHD is lossless and both sides land on
//! the same 24-bit values. The 5.1 track reaches the timeline as the stereo
//! pair every multichannel source does, so it is held to ffmpeg's own `-ac 2`
//! fold by correlation instead (the fold's coefficients are edith's, not
//! ffmpeg's, so the samples cannot be equal).
//!
//! Skipped, loudly, where ffmpeg is not installed.

use std::path::Path;
use std::process::Command;

use engine::audio::AudioSession;
use engine::scratch::Scratch;

/// Whether `ffmpeg` runs here. Without it the tests are skipped, loudly.
fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Encodes a two-second sine into a TrueHD Matroska file with `channels` channels.
/// The guard is returned, not its path: dropping it removes the fixture.
fn make_truehd(name: &str, channels: &str) -> Scratch {
    let mka = Scratch::file(name, "mka");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-ac", channels, "-c:a", "truehd", "-strict", "-2"])
        .arg(&*mka)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    mka
}

/// ffmpeg's decode of `path`'s first audio track as f32, `-ac` channels wide
/// (0 keeps the file's own width), in the interleaved order it writes.
fn ffmpeg_f32(path: &Path, channels: Option<&str>) -> Vec<f32> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0"]);
    if let Some(channels) = channels {
        cmd.args(["-ac", channels]);
    }
    let out = cmd
        .args(["-f", "f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>().0.iter()
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// ffmpeg's decode of `path`'s first audio track as its own 32-bit integers, at
/// the width the file carries.
fn ffmpeg_s32(path: &Path) -> Vec<i32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "s32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>().0.iter()
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// The interleaved samples of `path`'s first audio track, with its metadata.
fn decode(path: &Path) -> (engine::audio::AudioMeta, Vec<f32>) {
    let (meta, rx) = AudioSession::open(path)
        .expect("opens")
        .expect("has an audio track");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta, samples)
}

/// One channel out of an interleaved buffer.
fn channel(interleaved: &[f32], channels: usize, index: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(index)
        .step_by(channels)
        .copied()
        .collect()
}

/// Normalised correlation of two runs over their common length: 1.0 when they
/// are the same waveform at the same offset, whatever their level.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let (mut dot, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (&x, &y) in a[..n].iter().zip(&b[..n]) {
        dot += f64::from(x) * f64::from(y);
        aa += f64::from(x) * f64::from(x);
        bb += f64::from(y) * f64::from(y);
    }
    dot / (aa.sqrt() * bb.sqrt()).max(f64::MIN_POSITIVE)
}

#[test]
fn stereo_truehd_in_matroska_opens_at_its_rate_and_channels() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode the TrueHD fixture with");
        return;
    }
    let mka = make_truehd("ve_truehd_stereo", "2");
    let (meta, samples) = decode(&mka);
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));
    assert!(!samples.is_empty(), "no sound out of the TrueHD track");
}

#[test]
fn surround_truehd_in_matroska_reaches_the_timeline_as_a_pair() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode the TrueHD fixture with");
        return;
    }
    let mka = make_truehd("ve_truehd_51", "6");
    let (meta, samples) = decode(&mka);
    // The 5.1 presentation is folded to stereo at the decoder's door, as every
    // multichannel source is; the rate is the stream's own.
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));
    assert!(!samples.is_empty(), "no sound out of the 5.1 TrueHD track");
}

#[test]
fn stereo_truehd_matches_ffmpeg_sample_for_sample() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let mka = make_truehd("ve_truehd_exact", "2");
    let (meta, ours) = decode(&mka);
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));

    let theirs = ffmpeg_s32(&mka);
    assert_eq!(
        ours.len(),
        theirs.len(),
        "{} samples out, {} from ffmpeg",
        ours.len(),
        theirs.len()
    );
    // edith's f32 is the 24-bit sample over 2^31: exact in f32, so the
    // comparison is exact too, not a tolerance.
    let mismatches = ours
        .iter()
        .zip(&theirs)
        .filter(|(o, t)| f64::from(**o) * 2_147_483_648.0 != f64::from(**t))
        .count();
    assert_eq!(
        mismatches,
        0,
        "{mismatches} of {} samples differ from ffmpeg",
        ours.len()
    );
    assert!(
        ours.iter().any(|s| s.abs() > 1e-3),
        "the reference decode is silent; the fixture is wrong"
    );
}

#[test]
fn surround_truehd_folds_to_ffmpegs_stereo_by_correlation() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let mka = make_truehd("ve_truehd_51_fold", "6");
    let (meta, ours) = decode(&mka);
    assert_eq!(meta.channels, 2);

    let theirs = ffmpeg_f32(&mka, Some("2"));
    for index in 0..2 {
        let level = ours
            .iter()
            .skip(index)
            .step_by(2)
            .map(|s| s.abs())
            .fold(0f32, f32::max);
        assert!(level > 1e-3, "timeline channel {index} is silent");
        let score = correlation(&channel(&ours, 2, index), &channel(&theirs, 2, index));
        assert!(
            score > 0.99,
            "timeline channel {index} correlates {score:.4} with ffmpeg's -ac 2 decode"
        );
    }
}
