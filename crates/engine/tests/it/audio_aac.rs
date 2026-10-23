//! Every AAC stream edith decodes goes through `ec-aac`, HE-AAC included.
//!
//! symphonia refused SBR outright ("aac: aac too complex"), so a stereo HE-AAC
//! file never opened here. The first test is that file; the second is an AAC-LC
//! file through the same path, held to ffmpeg's decode of it; the third is the
//! guard that keeps symphonia's AAC decoder out of the engine's dependencies.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::audio::AudioSession;
use engine::scratch::Scratch;

/// A stereo HE-AAC v1 clip, 2.04 s, 44.1 kHz output from a 22.05 kHz AAC-LC core
/// with SBR, a 2 s stream copy of edith_codecs' synthetic sweep fixture (scripts/aac-tables/make-heaac-fixtures.sh, heaac_44100_48k). The
/// container says 44.1 kHz; the core it codes is half that.
const HE_FIXTURE: &str = "aac_he_v1_stereo.m4a";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
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
    interleaved.iter().skip(index).step_by(channels).copied().collect()
}

/// Root-mean-square level of a run of samples.
fn rms(samples: &[f32]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len().max(1) as f64).sqrt()
}

/// Normalised correlation of two runs over their common length: 1.0 when they
/// are the same waveform at the same offset.
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

/// Whether `ffmpeg` runs here. Without it the tests that need it are skipped,
/// loudly, the way the rest of the suite skips its reference decodes.
fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn stereo_he_aac_opens_at_its_sbr_rate_and_decodes() {
    let (meta, samples) = decode(&fixture(HE_FIXTURE));
    // The output rate is the extension rate, not the core's: 44.1 kHz from a
    // 22.05 kHz core. A timeline counted in the core rate would run at half speed.
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));

    let frames = samples.len() / 2;
    let secs = frames as f64 / 44_100.0;
    assert!(
        (1.9..=2.1).contains(&secs),
        "{frames} frames per channel is {secs:.3} s, want about 2.04 s"
    );
    for index in 0..2 {
        let level = rms(&channel(&samples, 2, index));
        assert!(level > 1e-3, "channel {index} is silent (rms {level})");
    }
}

#[test]
fn stereo_he_aac_in_matroska_decodes_at_the_same_rate() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to remux the fixture into Matroska");
        return;
    }
    let mkv = Scratch::file("ve_aac_he", "mkv");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture(HE_FIXTURE))
        .args(["-c", "copy"])
        .arg(&*mkv)
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));

    let (meta, samples) = decode(&mkv);
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));
    assert!(rms(&samples) > 1e-3, "no sound out of the Matroska copy");
}

#[test]
fn stereo_aac_lc_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    // 440 Hz left, 880 Hz right, AAC-LC from ffmpeg's own encoder.
    let m4a = Scratch::file("ve_aac_lc", "m4a");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine=f=440:d=2"])
        .args(["-f", "lavfi", "-i", "sine=f=880:d=2"])
        .args(["-filter_complex", "[0][1]amerge=inputs=2", "-ac", "2", "-ar", "44100"])
        .args(["-c:a", "aac", "-b:a", "128k"])
        .arg(&*m4a)
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));

    let (meta, ours) = decode(&m4a);
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));

    let reference = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&*m4a)
        .args(["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", "44100", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(reference.status.success());
    let theirs: Vec<f32> = reference
        .stdout
        .as_chunks::<4>().0.iter()
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    for index in 0..2 {
        let a = channel(&ours, 2, index);
        let b = channel(&theirs, 2, index);
        let r = correlation(&a, &b);
        assert!(r >= 0.999, "channel {index} correlates {r:.6} with ffmpeg, want >= 0.999");
    }
}

#[test]
fn symphonia_codec_aac_is_not_an_engine_dependency() {
    // symphonia's AAC decoder refuses HE-AAC, and every AAC stream now goes
    // through `ec-aac`. Neither the crate nor symphonia's `aac` feature may
    // come back: that feature is the only thing that pulls the crate in.
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("read engine's Cargo.toml");
    let live = |line: &&str| !line.trim_start().starts_with('#');
    assert!(
        !manifest.lines().filter(live).any(|l| l.contains("symphonia-codec-aac")),
        "symphonia-codec-aac is back in engine's dependencies"
    );
    let features = manifest
        .split("symphonia = {")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("engine depends on symphonia");
    assert!(
        !features.contains("\"aac\""),
        "symphonia's aac feature is back in engine's Cargo.toml"
    );
}

/// A bare ADTS HE-AAC stream, 2 s, stereo, 44.1 kHz: its headers say AAC-LC at
/// 22.05 kHz, and every frame carries an SBR payload (implicit signalling). The
/// rate has to come from decoding the frames, not from the headers.
const ADTS_IMPLICIT_FIXTURE: &str = "heaac_44100_48k_implicit_2s.aac";

#[test]
fn bare_adts_he_aac_opens_at_its_implicit_sbr_rate() {
    let (meta, samples) = decode(&fixture(ADTS_IMPLICIT_FIXTURE));
    // The headers say 22.05 kHz; the decoded SBR output is 44.1 kHz.
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));

    // Whole ADTS frames, not trimmed to 2 s: 47 frames of 2048 samples, 2.14 s,
    // which is the length ffmpeg decodes too.
    let frames = samples.len() / 2;
    let secs = frames as f64 / 44_100.0;
    assert!(
        (1.9..=2.2).contains(&secs),
        "{frames} frames per channel is {secs:.3} s, want about 2 s"
    );
    for index in 0..2 {
        let level = rms(&channel(&samples, 2, index));
        assert!(level > 1e-3, "channel {index} is silent (rms {level})");
    }
}

#[test]
fn bare_adts_he_aac_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to decode the reference with");
        return;
    }
    let (_, ours) = decode(&fixture(ADTS_IMPLICIT_FIXTURE));

    let reference = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture(ADTS_IMPLICIT_FIXTURE))
        .args(["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", "44100", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(reference.status.success());
    let theirs: Vec<f32> = reference
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();

    for index in 0..2 {
        let r = correlation(&channel(&ours, 2, index), &channel(&theirs, 2, index));
        assert!(r >= 0.999, "channel {index} correlates {r:.6} with ffmpeg, want >= 0.999");
    }
}

#[test]
fn mp4_he_aac_with_an_implicit_config_opens_at_its_sbr_rate() {
    // ffmpeg's mp4 muxer writes the ADTS headers as an AudioSpecificConfig: plain
    // AAC-LC at 22.05 kHz, with SBR payloads in the frames. The mp4 track must
    // report the decoded 44.1 kHz rate, as the bare stream does.
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to remux the fixture into mp4");
        return;
    }
    let m4a = Scratch::file("ve_aac_implicit", "m4a");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture(ADTS_IMPLICIT_FIXTURE))
        .args(["-c", "copy"])
        .arg(&*m4a)
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));

    let (meta, samples) = decode(&m4a);
    assert_eq!((meta.sample_rate, meta.channels), (44_100, 2));
    assert!(rms(&samples) > 1e-3, "no sound out of the mp4 copy");
}
