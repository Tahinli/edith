//! Seeking into an HE-AAC track lands exactly on the target, with the same
//! samples a full decode has there.
//!
//! The SBR (and PS) header of a libfdk stream comes back every few frames, not
//! every frame, so a decoder that starts between headers reads its frames as
//! headerless. The seek decodes a warm-up of packets ahead of the target and
//! throws their output away; this checks that the warm-up is enough. The
//! reference is ffmpeg's full decode, sliced at the target. Each test skips
//! loudly when this machine's ffmpeg cannot encode the profile it needs.

use std::path::Path;
use std::process::Command;

use engine::audio::{AudioChunk, AudioSession};
use engine::scratch::Scratch;

const RATE: u32 = 44_100;
/// The seek target, in seconds of audible timeline.
const TARGET_SECS: f64 = 2.5;
/// Length of the fixture: enough past the target for a real segment.
const FIXTURE_SECS: &str = "6";

/// ffmpeg arguments that encode a 6 s stereo source (440 Hz left, 880 Hz right)
/// as `profile` at `bitrate` into `out`.
fn encode_args(out: &str, profile: &str, bitrate: &str, seconds: &str) -> Vec<String> {
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
        profile,
        "-b:a",
        bitrate,
        out,
    ]
    .map(String::from)
    .to_vec()
}

/// Runs ffmpeg with `args`, reporting whether it succeeded.
fn ffmpeg_ok(args: &[String]) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether this machine's `libfdk_aac` encodes `profile`: a short real encode,
/// since some builds have SBR and PS compiled out.
fn can_encode(profile: &str, bitrate: &str) -> bool {
    let probe = Scratch::file("ve_aac_seek_probe", "m4a");
    let out = probe.to_str().expect("utf-8 path");
    ffmpeg_ok(&encode_args(out, profile, bitrate, "0.2"))
}

/// Fails the test with stderr when ffmpeg cannot run a step it must run.
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

/// ffmpeg's own full decode of `path`'s audio, stereo at 44.1 kHz.
fn ffmpeg_decode(path: &Path) -> Vec<f32> {
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

/// Interleaved samples from `start_secs` on, with the absolute sample number the
/// first chunk claims for them.
fn seek(path: &Path, start_secs: f64) -> (u64, Vec<f32>) {
    let (meta, rx) = AudioSession::open_at(path, start_secs)
        .expect("opens")
        .expect("has an audio track");
    assert_eq!(
        (meta.sample_rate, meta.channels),
        (RATE, 2),
        "{}",
        path.display()
    );
    let mut first = None;
    let mut samples = Vec::new();
    for AudioChunk {
        start_sample,
        samples: s,
    } in rx
    {
        first.get_or_insert(start_sample);
        samples.extend_from_slice(&s);
    }
    (first.expect("the seek yields audio"), samples)
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

fn channel(interleaved: &[f32], index: usize) -> Vec<f32> {
    interleaved.iter().skip(index).step_by(2).copied().collect()
}

/// Encodes the fixture for `profile` into an mp4, seeks it to `TARGET_SECS`, and
/// holds the seeked segment to ffmpeg's full decode sliced at the same point.
fn seek_matches_full_decode(profile: &str, bitrate: &str, name: &str) {
    if !can_encode(profile, bitrate) {
        eprintln!("skipped: this ffmpeg cannot encode {profile} (libfdk_aac with SBR/PS needed)");
        return;
    }
    let m4a = Scratch::file(name, "m4a");
    let out = m4a.to_str().expect("utf-8 path");
    ffmpeg(&encode_args(out, profile, bitrate, FIXTURE_SECS));

    let target = (TARGET_SECS * f64::from(RATE)).round() as usize;
    let reference = ffmpeg_decode(&m4a);
    let want = &reference[target * 2..];

    let (first, ours) = seek(&m4a, TARGET_SECS);
    // The first sample is the target sample, by the chunk's own numbering.
    assert_eq!(
        first, target as u64,
        "the seek starts at sample {first}, not {target}"
    );
    for index in 0..2 {
        let level = channel(&ours, index).iter().map(|s| s * s).sum::<f32>();
        assert!(level > 1.0, "channel {index} is silent after the seek");
        let r = correlation(&channel(&ours, index), &channel(want, index));
        assert!(
            r >= 0.999,
            "{profile} seek to {TARGET_SECS} s: channel {index} correlates {r:.6} with the full decode, want >= 0.999"
        );
    }
}

#[test]
fn he_aac_v2_seek_to_2_5_s_matches_the_full_decode() {
    seek_matches_full_decode("aac_he_v2", "24k", "ve_aac_v2_seek");
}

#[test]
fn he_aac_v1_seek_to_2_5_s_matches_the_full_decode() {
    seek_matches_full_decode("aac_he", "32k", "ve_aac_v1_seek");
}
