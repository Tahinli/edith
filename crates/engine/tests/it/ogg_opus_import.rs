//! Opus in Ogg, read through edith's own audio call site ([`AudioSession`], the
//! path a project's imported audio takes), held to ffmpeg's own decode of the
//! same file: the same length to within one Opus packet, and the same waveform
//! channel by channel.
//!
//! The reference is generated here: ffmpeg encodes a two-second stereo tone pair
//! with libopus into an `.ogg`, and ffmpeg decodes that file to 32-bit float at
//! 48 kHz. Opus output is always 48 kHz, so the rate is part of what is checked.
//!
//! Skipped loudly when ffmpeg is not on the box.
//!
//! ```text
//! cargo test -p engine --release --test ogg_opus_import -- --nocapture
//! ```

use std::path::Path;
use std::process::Command;

use engine::audio::AudioSession;
use engine::scratch::Scratch;

const RATE: u32 = 48_000;
/// One 20 ms Opus packet at 48 kHz: the most two decoders may disagree on the
/// length of a file by, since each trims its own end.
const ONE_PACKET: usize = 960;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn s(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

fn ffmpeg(args: &[&str]) {
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

/// Edith's decode of the file's first audio track: the channel count, the rate
/// it states, and the interleaved samples.
fn decode(path: &Path) -> (u16, u32, Vec<f32>) {
    let (meta, rx) = AudioSession::open(path)
        .expect("opens")
        .expect("has an audio track");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta.channels, meta.sample_rate, samples)
}

/// ffmpeg's decode of the same file, interleaved f32 at 48 kHz, stereo.
fn reference(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn channel(interleaved: &[f32], channels: usize, index: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(index)
        .step_by(channels)
        .copied()
        .collect()
}

/// Normalised correlation over the common length: 1.0 for the same waveform at
/// the same offset.
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
fn stereo_opus_in_ogg_matches_ffmpegs_decode_in_length_and_waveform() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let ogg = Scratch::file("ve_opus_stereo", "ogg");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "aevalsrc=sin(2*PI*440*t)|0.5*sin(2*PI*660*t):s=48000:d=2",
        "-c:a",
        "libopus",
        "-b:a",
        "96k",
        s(&ogg),
    ]);

    let (channels, rate, edith) = decode(&ogg);
    assert_eq!(rate, RATE, "Opus is decoded at the 48 kHz it is coded at");
    assert_eq!(channels, 2, "the stereo the file states");

    let want = reference(&ogg);
    let frames = edith.len() / 2;
    let want_frames = want.len() / 2;
    assert!(
        frames.abs_diff(want_frames) <= ONE_PACKET,
        "edith decodes {frames} frames, ffmpeg {want_frames}"
    );
    // Two seconds, give or take the packet each decoder trims at its end.
    assert!(
        frames.abs_diff(2 * RATE as usize) <= ONE_PACKET,
        "two seconds of audio, not {frames} frames"
    );

    for index in 0..2 {
        let a = channel(&edith, 2, index);
        let b = channel(&want, 2, index);
        let r = correlation(&a, &b);
        assert!(
            r > 0.99,
            "channel {index}: correlation {r} against ffmpeg's decode"
        );
    }
}
