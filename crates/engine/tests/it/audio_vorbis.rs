//! Vorbis decode, in Ogg and in Matroska, comes out the way ffmpeg's own decode
//! of the same file does, at the same length to the sample. Edith decodes Vorbis
//! with `ec-vorbis`, and the audio path cuts each stream to its own end: the page
//! granules in Ogg, the last block's `DiscardPadding` in Matroska.
//!
//! The reference is generated here: ffmpeg encodes a stereo tone pair with
//! libvorbis, the file is decoded through [`AudioSession`] (the call site a
//! project's audio takes), and ffmpeg decodes it again to 32-bit float. The two
//! are compared channel by channel. A Matroska copy of the same stream is held to
//! the same reference, because the container is the only thing that differs.
//!
//! A seek into the file is held to the full run's own samples (see
//! [`a_seek_into_ogg_vorbis_is_the_full_runs_samples`]).
//!
//! Skipped loudly, the way the rest of the suite skips its reference decodes,
//! when ffmpeg is not on the box.

use std::path::Path;
use std::process::Command;

use engine::audio::AudioSession;
use engine::scratch::Scratch;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The interleaved samples of `path`'s first audio track, and its channel count.
fn decode(path: &Path) -> (u16, Vec<f32>) {
    let (meta, rx) = AudioSession::open(path)
        .expect("opens")
        .expect("has an audio track");
    assert_eq!(meta.sample_rate, 44_100, "the rate the file states");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta.channels, samples)
}

/// `path` decoded from `start_secs` on, interleaved, and its channel count.
fn decode_from(path: &Path, start_secs: f64) -> (u16, Vec<f32>) {
    let (meta, rx) = AudioSession::open_at(path, start_secs)
        .expect("opens")
        .expect("has an audio track");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta.channels, samples)
}

/// ffmpeg's decode of the same file, as interleaved 32-bit float at `channels`.
fn reference(path: &Path, channels: u16) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-ac"])
        .arg(channels.to_string())
        .args(["-ar", "44100", "-"])
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
fn stereo_vorbis_in_ogg_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let ogg = Scratch::file("ve_vorbis_stereo", "ogg");
    encode_ogg(&ogg, 2);
    check_against_ffmpeg(&ogg);
}

#[test]
fn stereo_vorbis_in_matroska_matches_ffmpeg_per_channel() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let mkv = Scratch::file("ve_vorbis_stereo", "mkv");
    encode_mkv(&mkv, 2);
    check_against_ffmpeg(&mkv);
}

#[test]
fn mono_vorbis_in_ogg_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode and decode the reference with");
        return;
    }
    let ogg = Scratch::file("ve_vorbis_mono", "ogg");
    encode_ogg(&ogg, 1);
    check_against_ffmpeg(&ogg);
}

/// A two-second tone, libvorbis at a middling quality: 440 Hz in the left channel
/// and 880 Hz in the right when there are two.
fn encode_ogg(path: &Scratch, channels: u16) {
    encode(path, channels, "ogg", 2);
}

fn encode_mkv(path: &Scratch, channels: u16) {
    encode(path, channels, "matroska", 2);
}

fn encode(path: &Scratch, channels: u16, format: &str, secs: u32) {
    let mut args: Vec<String> = vec![
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("sine=f=440:d={secs}"),
    ];
    if channels == 2 {
        args.extend(["-f", "lavfi", "-i", &format!("sine=f=880:d={secs}")].map(String::from));
        args.extend(["-filter_complex", "[0][1]amerge=inputs=2"].map(String::from));
    }
    args.extend(
        [
            "-ac",
            &channels.to_string(),
            "-ar",
            "44100",
            "-c:a",
            "libvorbis",
            "-q:a",
            "5",
            "-f",
            format,
        ]
        .map(String::from),
    );
    args.push(path.to_string_lossy().into_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs);
}

fn check_against_ffmpeg(file: &Path) {
    let (channels, ours) = decode(file);
    let theirs = reference(file, channels);
    let n = channels as usize;
    assert!(!ours.is_empty(), "no sound out of {}", file.display());
    // The length is the stream's, to the frame: both the Ogg granules and the
    // Matroska padding state it exactly.
    eprintln!(
        "{}: {} frames per channel from edith, {} from ffmpeg",
        file.display(),
        ours.len() / n,
        theirs.len() / n
    );
    assert_eq!(
        ours.len(),
        theirs.len(),
        "frames per channel: edith {}, ffmpeg {}",
        ours.len() / n,
        theirs.len() / n
    );
    for index in 0..n {
        let c = correlation(&channel(&ours, n, index), &channel(&theirs, n, index));
        eprintln!("channel {index} correlates {c:.6} with ffmpeg");
        assert!(c > 0.999, "channel {index} correlates {c:.6} with ffmpeg");
    }
}

/// A seek into an Ogg Vorbis file lands on the full run's own samples: the
/// tail from `start` is the same frames, bit for bit, at the same timeline
/// position. Exact, not a tolerance: the reader lands on the packet at or before
/// the target, and the decoder's warm-up is dropped (`ec-vorbis` answers nothing
/// for a fresh decoder's first block, and that block's position is not taken).
#[test]
fn a_seek_into_ogg_vorbis_is_the_full_runs_samples() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode the fixture with");
        return;
    }
    let ogg = Scratch::file("ve_vorbis_seek", "ogg");
    encode_ogg(&ogg, 2);
    check_seek_is_full_run(&ogg);
}

/// The same seek on Matroska: its blocks are stamped in whole milliseconds, which
/// is not where the samples are, so the reader has to place each packet by the
/// samples before it rather than by its stamp.
#[test]
fn a_seek_into_matroska_vorbis_is_the_full_runs_samples() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode the fixture with");
        return;
    }
    let mkv = Scratch::file("ve_vorbis_seek", "mkv");
    encode_mkv(&mkv, 2);
    check_seek_is_full_run(&mkv);
}

/// A Matroska file long enough for several clusters: the targets fall in and
/// across them, where the container's own seek used to land a cluster past the
/// target.
#[test]
fn a_seek_across_matroska_clusters_is_the_full_runs_samples() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg to encode the fixture with");
        return;
    }
    let mkv = Scratch::file("ve_vorbis_seek_long", "mkv");
    encode(&mkv, 2, "matroska", 14);
    check_seek_targets_is_full_run(&mkv, &[0.25, 3.5, 6.2, 9.9, 12.5, 13.7]);
}

/// Six seeks across `file`, each held to the full run's own tail from the same
/// timeline position.
fn check_seek_is_full_run(file: &Path) {
    check_seek_targets_is_full_run(file, &[0.25, 0.5, 1.0, 1.25, 1.7, 1.99]);
}

fn check_seek_targets_is_full_run(file: &Path, targets: &[f64]) {
    let (channels, full) = decode_from(file, 0.0);
    let n = usize::from(channels);
    let frames = full.len() / n;
    // Targets across the file, the last near its end, each a whole frame.
    for &secs in targets {
        let at = (secs * 44_100.0_f64).round() as usize;
        let (_, part) = decode_from(file, secs);
        let want = &full[at * n..];
        assert_eq!(
            part.len() / n,
            frames - at,
            "a seek to {secs} s is a different length from the full run's tail"
        );
        if let Some(i) = part.iter().zip(want).position(|(a, b)| a != b) {
            panic!(
                "a seek to {secs} s differs from the full run at frame {} \
                 (sample {i} of the tail): {} vs {}",
                i / n,
                part[i],
                want[i]
            );
        }
    }
}

