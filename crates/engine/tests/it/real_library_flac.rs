//! The FLAC writer on the owner's own media: 60 seconds from the middle of
//! each named track, written through `engine::flac_write` and decoded back by
//! ffmpeg. The round trip must be exact, and `STREAMINFO` must describe the
//! same audio (rate, channels, bit depth, length and the MD5 of the PCM).
//!
//! * `wav_pcm16`: the file's own 16-bit PCM.
//! * `av1_opus_51`, `ac3_51_film`: the sound track decoded by ffmpeg at its
//!   native rate and channel count, written at 16 bits (what the export asks
//!   for) and at 24 bits (what the writer takes but the export never asks for).
//!
//! The same checks as `flac_export.rs` on synthetic signals: ffmpeg's decode
//! of the written stream is compared sample for sample with the input, and the
//! 24-bit input is decoded by ffmpeg as its 32-bit left-justified form.
//!
//! Named by `real_library.toml` (gitignored). Skips where a key is absent or
//! ffmpeg / ffprobe are not installed.

use crate::reallib;

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use engine::flac_write;
use engine::scratch::Scratch;
use reallib::{ffprobe_streams, have, md5_hex};

/// Seconds of each track that are encoded, taken from its middle.
const WINDOW_SECONDS: f64 = 60.0;

fn run_ok(cmd: &mut Command, what: &str) -> Vec<u8> {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("cannot run {what}: {e}"));
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// `(sample_rate, channels, duration_seconds)` of the first audio stream.
fn source_shape(path: &Path) -> (u32, usize, f64) {
    let audio = ffprobe_streams(path)
        .into_iter()
        .find(|s| s.kind == "audio")
        .expect("the file has an audio stream");
    let duration = run_ok(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(path),
        "ffprobe duration",
    );
    let duration: f64 = String::from_utf8_lossy(&duration)
        .trim()
        .parse()
        .expect("a duration");
    (
        audio.sample_rate.expect("sample rate"),
        usize::from(audio.channels.expect("channel count")),
        duration,
    )
}

/// The first audio track over the 60 seconds from the middle of the file, as
/// raw little-endian PCM in `fmt` (`s16le` or `s24le`), at the track's own rate
/// and channel count.
fn pcm_window(path: &Path, duration: f64, fmt: &str) -> Vec<u8> {
    let start = (duration / 2.0 - WINDOW_SECONDS / 2.0).max(0.0);
    run_ok(
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-ss"])
            .arg(format!("{start}"))
            .arg("-i")
            .arg(path)
            .args(["-map", "0:a:0", "-vn", "-t"])
            .arg(format!("{WINDOW_SECONDS}"))
            .args(["-f", fmt, "-"]),
        "ffmpeg pcm",
    )
}

/// Little-endian PCM bytes to interleaved samples, `bits` wide, sign-extended.
fn samples_of(bytes: &[u8], bits: u32) -> Vec<i32> {
    match bits {
        16 => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i32::from(i16::from_le_bytes(*b)))
            .collect(),
        24 => bytes
            .as_chunks::<3>()
            .0
            .iter()
            .map(|b| (i32::from(b[0]) | i32::from(b[1]) << 8 | i32::from(b[2]) << 16) << 8 >> 8)
            .collect(),
        _ => unreachable!("only 16 and 24 bits are used here"),
    }
}

/// Little-endian bytes of each sample in `bits / 8` bytes: the PCM the FLAC
/// MD5 is defined over.
fn pcm_bytes(samples: &[i32], bits: u32) -> Vec<u8> {
    let width = (bits / 8) as usize;
    samples
        .iter()
        .flat_map(|s| s.to_le_bytes().into_iter().take(width))
        .collect()
}

/// The `STREAMINFO` block read by hand: `(rate, channels, bits, total, md5)`.
fn streaminfo(bytes: &[u8]) -> (u32, usize, u32, u64, String) {
    assert_eq!(&bytes[0..4], b"fLaC", "a FLAC stream, by its own magic");
    assert_eq!(bytes[4] & 0x7F, 0, "the first metadata block is STREAMINFO");
    let s = &bytes[8..42];
    let rate = (u32::from(s[10]) << 12) | (u32::from(s[11]) << 4) | (u32::from(s[12]) >> 4);
    let channels = usize::from((s[12] >> 1) & 0x07) + 1;
    let bits = (u32::from(s[12] & 0x01) << 4 | u32::from(s[13] >> 4)) + 1;
    let total = (u64::from(s[13] & 0x0F) << 32)
        | u64::from(u32::from_be_bytes([s[14], s[15], s[16], s[17]]));
    let md5: String = s[18..34].iter().map(|b| format!("{b:02x}")).collect();
    (rate, channels, bits, total, md5)
}

/// ffmpeg's decode of `path`, as interleaved `i32`s, left-justified to 32 bits.
fn ffmpeg_decode(path: &Path) -> Vec<i32> {
    let bytes = run_ok(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-f", "s32le", "-"]),
        "ffmpeg decode of the written flac",
    );
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect()
}

/// Writes `input` through `flac_write`, then checks the header, the
/// `STREAMINFO` and ffmpeg's decode against what went in.
fn round_trip(name: &str, input: &[i32], bits: u32, channels: usize, rate: u32) {
    let frames = input.len() / channels;
    let out = Scratch::file(&format!("ve_flac_real_{name}"), "flac");
    let started = Instant::now();
    flac_write::write(&out, input, channels, bits, rate).expect("write the flac");
    let wrote = started.elapsed();

    let bytes = std::fs::read(&out).expect("read the flac back");
    let (got_rate, got_channels, got_bits, total, md5) = streaminfo(&bytes);
    assert_eq!(got_rate, rate, "{name}: STREAMINFO sample rate");
    assert_eq!(got_channels, channels, "{name}: STREAMINFO channel count");
    assert_eq!(got_bits, bits, "{name}: STREAMINFO bit depth");
    assert_eq!(total, frames as u64, "{name}: STREAMINFO total samples");
    assert_eq!(
        md5,
        md5_hex(&pcm_bytes(input, bits)),
        "{name}: STREAMINFO MD5 is the MD5 of the input PCM"
    );

    let decoded = ffmpeg_decode(&out);
    let shift = 32 - bits;
    let expected: Vec<i32> = input.iter().map(|s| s << shift).collect();
    assert_eq!(
        decoded.len(),
        expected.len(),
        "{name}: decoded sample count"
    );
    let first_bad = decoded.iter().zip(&expected).position(|(a, b)| a != b);
    assert_eq!(first_bad, None, "{name}: first mismatching sample index");

    println!(
        "{name}: {frames} frames x {channels} ch at {rate} Hz, {bits}-bit: \
         {} bytes flac, encode {:.2} s, exact round trip",
        bytes.len(),
        wrote.as_secs_f64()
    );
}

fn tools_present() -> bool {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipped: ffmpeg and ffprobe are needed");
        return false;
    }
    true
}

#[test]
fn wav_pcm16_round_trips_through_flac() {
    let Some(path) = engine::real_library::film("wav_pcm16") else {
        return;
    };
    if !tools_present() {
        return;
    }
    let (rate, channels, duration) = source_shape(&path);
    let samples = samples_of(&pcm_window(&path, duration, "s16le"), 16);
    round_trip("wav_pcm16", &samples, 16, channels, rate);
}

#[test]
fn av1_opus_51_round_trips_through_flac() {
    let Some(path) = engine::real_library::film("av1_opus_51") else {
        return;
    };
    if !tools_present() {
        return;
    }
    let (rate, channels, duration) = source_shape(&path);
    let pcm16 = samples_of(&pcm_window(&path, duration, "s16le"), 16);
    round_trip("av1_opus_51_s16", &pcm16, 16, channels, rate);
    let pcm24 = samples_of(&pcm_window(&path, duration, "s24le"), 24);
    round_trip("av1_opus_51_s24", &pcm24, 24, channels, rate);
}

#[test]
fn ac3_51_film_round_trips_through_flac() {
    let Some(path) = engine::real_library::film("ac3_51_film") else {
        return;
    };
    if !tools_present() {
        return;
    }
    let (rate, channels, duration) = source_shape(&path);
    let pcm16 = samples_of(&pcm_window(&path, duration, "s16le"), 16);
    round_trip("ac3_51_film_s16", &pcm16, 16, channels, rate);
    let pcm24 = samples_of(&pcm_window(&path, duration, "s24le"), 24);
    round_trip("ac3_51_film_s24", &pcm24, 24, channels, rate);
}
