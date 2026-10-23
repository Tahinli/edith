//! The FLAC writer, checked against the tools that did not write it: a stream
//! written through `engine::flac_write` must decode, under ffmpeg, to exactly
//! the samples that went in, and its `STREAMINFO` must describe that same
//! audio (rate, channels, bit depth, length and the MD5 of the PCM).
//!
//! Three shapes are covered: 16-bit stereo (what the export writes), 24-bit
//! stereo and 24-bit 5.1 (what the writer takes but the export never asks for).
//! The signal is synthetic and deterministic, with full-scale extremes in it,
//! so a clipped or wrapped sample would show.
//!
//! Needs the `ffmpeg`, `ffprobe` and `md5sum` command-line tools. `flac -t` is
//! not used: it is not installed on the machines this runs on, and ffmpeg's own
//! decoder does not check the MD5, so the MD5 is compared here directly.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use engine::flac_write;
use engine::scratch::Scratch;

/// Deterministic test signal: a per-channel tone plus LCG noise, clamped to the
/// bit depth, with the two extremes planted at the start.
fn signal(frames: usize, channels: usize, bits: u32) -> Vec<i32> {
    let max = (1i64 << (bits - 1)) - 1;
    let min = -(1i64 << (bits - 1));
    let mut seed: u32 = 0x9E37_79B9;
    let mut out = Vec::with_capacity(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = f64::from(seed >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0;
            let tone = (f as f64 * (110.0 + 37.0 * c as f64) * std::f64::consts::TAU / 48_000.0)
                .sin();
            let v = ((tone * 0.7 + noise * 0.05) * max as f64).round() as i64;
            out.push(v.clamp(min, max) as i32);
        }
    }
    if let Some(first) = out.first_mut() {
        *first = max as i32;
    }
    if let Some(second) = out.get_mut(channels) {
        *second = min as i32;
    }
    out
}

/// Little-endian bytes of each sample in `bits / 8` bytes, which is the PCM the
/// FLAC MD5 is defined over.
fn pcm_bytes(samples: &[i32], bits: u32) -> Vec<u8> {
    let width = (bits / 8) as usize;
    samples
        .iter()
        .flat_map(|s| s.to_le_bytes().into_iter().take(width))
        .collect()
}

fn run(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"))
}

/// The MD5 of `data`, as `md5sum` prints it.
fn md5_hex(data: &[u8]) -> String {
    let mut child = Command::new("md5sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("md5sum");
    child
        .stdin
        .take()
        .expect("md5sum stdin")
        .write_all(data)
        .expect("feed md5sum");
    let out = child.wait_with_output().expect("md5sum output");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("md5sum digest")
        .to_string()
}

/// `(sample_rate, channels, bits_per_raw_sample)` as ffprobe reports them.
fn probe(path: &Path) -> (u32, usize, u32) {
    let out = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,bits_per_raw_sample",
            "-of",
            "default=nw=1",
            path.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "ffprobe on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| -> String {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("ffprobe reports no {key}: {text}"))
            .to_string()
    };
    assert_eq!(field("codec_name"), "flac", "a FLAC stream");
    (
        field("sample_rate").parse().expect("rate"),
        field("channels").parse().expect("channels"),
        field("bits_per_raw_sample").parse().expect("bits"),
    )
}

/// The `STREAMINFO` block, read by hand: the MD5 and the total sample count.
fn streaminfo_md5_and_total(bytes: &[u8]) -> (String, u64) {
    assert_eq!(&bytes[0..4], b"fLaC", "a FLAC stream, by its own magic");
    assert_eq!(bytes[4] & 0x7F, 0, "the first metadata block is STREAMINFO");
    let s = &bytes[8..42];
    let total = (u64::from(s[13] & 0x0F) << 32)
        | u64::from(u32::from_be_bytes([s[14], s[15], s[16], s[17]]));
    let md5: String = s[18..34].iter().map(|b| format!("{b:02x}")).collect();
    (md5, total)
}

/// ffmpeg's decode, as interleaved `i32`s (ffmpeg left-justifies every depth
/// into 32 bits, so a 16-bit sample arrives as `sample << 16`).
fn ffmpeg_decode(path: &Path) -> Vec<i32> {
    let out = run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            path.to_str().expect("utf-8 path"),
            "-f",
            "s32le",
            "-",
        ],
    );
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "ffmpeg decode of {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes(*b))
        .collect()
}

fn check(name: &str, bits: u32, channels: usize, rate: u32, frames: usize) {
    let input = signal(frames, channels, bits);
    let out = Scratch::file(&format!("ve_flac_parity_{name}"), "flac");
    flac_write::write(&out, &input, channels, bits, rate).expect("write the flac");

    // ffprobe: the header says what was asked for.
    let (got_rate, got_channels, got_bits) = probe(&out);
    assert_eq!(got_rate, rate, "{name}: sample rate");
    assert_eq!(got_channels, channels, "{name}: channel count");
    assert_eq!(got_bits, bits, "{name}: bits_per_raw_sample");

    // STREAMINFO: the MD5 is the one of the PCM that went in, and the length
    // is the frame count.
    let bytes = std::fs::read(&out).expect("read the flac back");
    let (md5, total) = streaminfo_md5_and_total(&bytes);
    assert_eq!(total, frames as u64, "{name}: total samples in STREAMINFO");
    assert_eq!(
        md5,
        md5_hex(&pcm_bytes(&input, bits)),
        "{name}: STREAMINFO MD5 is the MD5 of the input PCM"
    );

    // ffmpeg: every sample comes back, bit for bit, at the same position.
    let decoded = ffmpeg_decode(&out);
    let shift = 32 - bits;
    let expected: Vec<i32> = input.iter().map(|s| s << shift).collect();
    assert_eq!(decoded.len(), expected.len(), "{name}: decoded sample count");
    let first_bad = decoded.iter().zip(&expected).position(|(a, b)| a != b);
    assert_eq!(first_bad, None, "{name}: first mismatching sample index");

    println!(
        "{name}: {} bytes for {} bytes of PCM",
        bytes.len(),
        pcm_bytes(&input, bits).len()
    );
}

#[test]
fn flac_16_bit_stereo_decodes_to_the_same_samples() {
    check("s16_stereo", 16, 2, 44_100, 22_050);
}

#[test]
fn flac_24_bit_stereo_decodes_to_the_same_samples() {
    check("s24_stereo", 24, 2, 48_000, 24_000);
}

#[test]
fn flac_24_bit_51_decodes_to_the_same_samples() {
    check("s24_51", 24, 6, 48_000, 24_000);
}
