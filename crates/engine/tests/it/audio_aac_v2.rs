//! HE-AAC v2 (AAC with Parametric Stereo) is decoded through `ec-aac`, not refused.
//!
//! The stream is encoded here by ffmpeg's `libfdk_aac` from a stereo source, so
//! the core is mono and the second channel comes from PS. Three wrappings are
//! covered: an mp4 with the explicit v2 config ffmpeg writes on encode, a bare
//! ADTS stream (implicit signalling), and an mp4 remuxed from that ADTS. Each is
//! held to ffmpeg's own decode, per channel. A machine whose ffmpeg has no
//! `libfdk_aac` that can encode v2 skips the test loudly, as the other reference
//! tests do. Some builds of `libfdk_aac` have SBR and PS compiled out, so the
//! encoder is probed with a real v2 encode, not just looked up by name.

use std::path::Path;
use std::process::Command;

use engine::audio::AudioSession;
use engine::scratch::Scratch;

/// The ffmpeg arguments that encode the test source as HE-AAC v2 into `out`.
fn he_v2_args(out: &str, seconds: &str) -> Vec<String> {
    [
        "-f",
        "lavfi",
        "-i",
        &format!("sine=f=440:d={seconds}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=f=880:d={seconds}"),
        "-filter_complex",
        "[0][1]amerge=inputs=2",
        "-ac",
        "2",
        "-ar",
        "44100",
        "-c:a",
        "libfdk_aac",
        "-profile:a",
        "aac_he_v2",
        "-b:a",
        "24k",
        out,
    ]
    .map(String::from)
    .to_vec()
}

/// Whether this machine's ffmpeg can encode HE-AAC v2: a short real encode, so a
/// `libfdk_aac` without PS support counts as absent.
fn can_encode_he_v2() -> bool {
    let probe = Scratch::file("ve_aac_v2_probe", "m4a");
    Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(he_v2_args(probe.to_str().expect("utf-8 path"), "0.2"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Runs `ffmpeg` with `args`, panicking with its stderr on failure.
fn ffmpeg(args: &[String]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// Normalised correlation of two runs over their common length.
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

/// ffmpeg's own decode of `path`'s first audio track, stereo at 44.1 kHz.
fn reference(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", "44100", "-",
        ])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg decode: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// Encodes a 2 s stereo source (440 Hz left, 880 Hz right) as HE-AAC v2 at
/// 24 kbps into `out`, in the container `out` names.
fn encode_he_v2(out: &Path) {
    ffmpeg(&he_v2_args(out.to_str().expect("utf-8 path"), "2"));
}

/// Checks `ours` against ffmpeg's decode of `path`, channel by channel.
fn assert_matches_ffmpeg(path: &Path, ours: &[f32]) {
    let theirs = reference(path);
    for index in 0..2 {
        let r = correlation(&channel(ours, 2, index), &channel(&theirs, 2, index));
        assert!(
            r >= 0.999,
            "channel {index} correlates {r:.6} with ffmpeg, want >= 0.999"
        );
    }
}

fn assert_stereo_at_44_1(path: &Path) -> Vec<f32> {
    let (meta, samples) = decode(path);
    // The output is stereo at the SBR extension rate, whatever the core's width.
    assert_eq!(
        (meta.sample_rate, meta.channels),
        (44_100, 2),
        "{}",
        path.display()
    );
    let frames = samples.len() / 2;
    assert!(
        frames > 40_000,
        "{frames} frames per channel from {}",
        path.display()
    );
    samples
}

#[test]
fn he_aac_v2_in_mp4_decodes_to_stereo_and_matches_ffmpeg() {
    if !can_encode_he_v2() {
        eprintln!("skipped: no ffmpeg here encodes HE-AAC v2 (libfdk_aac with PS needed)");
        return;
    }
    let m4a = Scratch::file("ve_aac_v2_mp4", "m4a");
    encode_he_v2(&m4a);
    let samples = assert_stereo_at_44_1(&m4a);
    assert_matches_ffmpeg(&m4a, &samples);
}

#[test]
fn he_aac_v2_in_adts_decodes_to_stereo_and_matches_ffmpeg() {
    if !can_encode_he_v2() {
        eprintln!("skipped: no ffmpeg here encodes HE-AAC v2 (libfdk_aac with PS needed)");
        return;
    }
    let aac = Scratch::file("ve_aac_v2_adts", "aac");
    encode_he_v2(&aac);
    let samples = assert_stereo_at_44_1(&aac);
    assert_matches_ffmpeg(&aac, &samples);
}

#[test]
fn he_aac_v2_adts_remuxed_to_mp4_decodes_to_stereo_and_matches_ffmpeg() {
    if !can_encode_he_v2() {
        eprintln!("skipped: no ffmpeg here encodes HE-AAC v2 (libfdk_aac with PS needed)");
        return;
    }
    let aac = Scratch::file("ve_aac_v2_src", "aac");
    encode_he_v2(&aac);
    let m4a = Scratch::file("ve_aac_v2_remux", "m4a");
    ffmpeg(&[
        "-i".into(),
        aac.to_str().expect("utf-8 path").into(),
        "-c".into(),
        "copy".into(),
        m4a.to_str().expect("utf-8 path").into(),
    ]);
    let samples = assert_stereo_at_44_1(&m4a);
    assert_matches_ffmpeg(&m4a, &samples);
}
