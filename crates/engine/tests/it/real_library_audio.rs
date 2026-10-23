//! The audio seats on the owner's own files, against ffmpeg's own decode of the
//! same file at the same rate and channel count, in 32-bit float.
//!
//! * AAC (HE-AAC v1 and AAC-LC, in mp4): per channel, the Pearson correlation of
//!   the decoded track against ffmpeg's must be at least 0.999, at the lag the
//!   two streams line up at. That lag is searched first (+-2048 samples over the
//!   opening two seconds), and must be 0 or the engine's documented default
//!   priming of 1024 samples: a track with no edit list, which the engine trims
//!   as an AAC-LC encoder delay (`audio.rs`, `DEFAULT_PRIMING`) and ffmpeg does
//!   not. Any other lag is a failure.
//! * PCM16 WAV: every sample bit-equal to ffmpeg's (`s16 / 32768` is exact in
//!   float, so there is no rounding to excuse a difference), at lag 0.
//!
//! Named by `real_library.toml` (gitignored): `mp4_heaac`, `mp4_h264_aac`,
//! `mp4_phone_h264`, `wav_pcm16`. Skips where the key is absent or ffmpeg is
//! not installed.

use crate::reallib;

use std::io::Read;
use std::path::Path;
use std::process::Command;

use engine::audio::AudioSession;
use reallib::{ffmpeg_pipe, fill, have};

/// The engine's documented priming for an AAC track with no edit list.
const DEFAULT_PRIMING_LAG: i64 = 1024;

/// Running sums for one channel's Pearson correlation.
#[derive(Default, Clone, Copy)]
struct Pearson {
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl Pearson {
    fn add(&mut self, x: f64, y: f64) {
        self.n += 1.0;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.syy += y * y;
        self.sxy += x * y;
    }

    fn r(&self) -> f64 {
        let cov = self.n * self.sxy - self.sx * self.sy;
        let vx = self.n * self.sxx - self.sx * self.sx;
        let vy = self.n * self.syy - self.sy * self.sy;
        cov / (vx * vy).sqrt()
    }
}

/// What one comparison found.
struct Witness {
    /// Engine frame `i` lines up with ffmpeg frame `i + lag`.
    lag: i64,
    /// Per channel, at that lag.
    r: Vec<f64>,
    /// Frames the engine delivered, and frames ffmpeg decoded.
    engine_frames: usize,
    ffmpeg_frames: usize,
    /// Engine samples not bit-equal to ffmpeg's, at that lag.
    differing: usize,
}

/// The engine's first audio track, from the start, as interleaved f32.
fn engine_head(path: &Path, seconds: f64) -> (u16, u32, Vec<f32>) {
    let (meta, rx) = AudioSession::open_at(path, 0.0)
        .expect("open")
        .expect("the file has audio");
    let want = (seconds * f64::from(meta.sample_rate) * f64::from(meta.channels)) as usize;
    let mut out = Vec::with_capacity(want);
    for chunk in rx {
        out.extend_from_slice(&chunk.samples);
        if out.len() >= want {
            break;
        }
    }
    (meta.channels, meta.sample_rate, out)
}

/// ffmpeg's first audio track over the first `seconds`, as interleaved f32.
fn ffmpeg_head(path: &Path, channels: u16, rate: u32, seconds: f64) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-t",
            &format!("{seconds}"),
            "-f",
            "f32le",
        ])
        .args(["-ar", &rate.to_string(), "-ac", &channels.to_string(), "-"])
        .output()
        .expect("run ffmpeg");
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// The lag in `-2048..=2048` (frames) at which the engine's head and ffmpeg's
/// head correlate best on channel 0, with the correlation found there.
fn best_lag(engine: &[f32], ffmpeg: &[f32], channels: usize) -> (i64, f64) {
    let x: Vec<f64> = engine
        .iter()
        .step_by(channels)
        .map(|v| f64::from(*v))
        .collect();
    let y: Vec<f64> = ffmpeg
        .iter()
        .step_by(channels)
        .map(|v| f64::from(*v))
        .collect();
    let mut best = (0i64, f64::NEG_INFINITY);
    for lag in -2048i64..=2048 {
        let mut acc = Pearson::default();
        for (i, a) in x.iter().enumerate() {
            let j = i as i64 + lag;
            if j >= 0 && (j as usize) < y.len() {
                acc.add(*a, y[j as usize]);
            }
        }
        if acc.n > 0.0 {
            let r = acc.r();
            if r > best.1 {
                best = (lag, r);
            }
        }
    }
    best
}

/// Decodes the whole first audio track through the engine and through ffmpeg,
/// at the lag the two line up at, and compares them frame by frame.
fn compare(key: &str, path: &Path) -> Witness {
    let (channels, rate, head) = engine_head(path, 2.0);
    let reference = ffmpeg_head(path, channels, rate, 4.0);
    let ch = usize::from(channels);
    let (lag, score) = best_lag(&head, &reference, ch);
    eprintln!("{key}: best alignment lag {lag} frames (correlation {score:.9})");

    let (meta, rx) = AudioSession::open_at(path, 0.0)
        .expect("open")
        .expect("the file has audio");
    assert_eq!((meta.channels, meta.sample_rate), (channels, rate));
    // The engine is `lag` frames behind ffmpeg when `lag` is positive, so
    // ffmpeg's first `lag` frames have no engine counterpart; a negative lag
    // is the other way round.
    let skip_engine = (-lag).max(0) as usize;
    let skip_ffmpeg = lag.max(0) as usize;
    let (mut child, mut ff) = ffmpeg_pipe(
        &[
            "-map",
            "0:a:0",
            "-vn",
            "-f",
            "f32le",
            "-ar",
            &rate.to_string(),
            "-ac",
            &channels.to_string(),
            "-",
        ],
        path,
    );
    let mut discard = vec![0u8; 4 * skip_ffmpeg * ch];
    fill(&mut ff, &mut discard);

    let mut acc = vec![Pearson::default(); ch];
    let (mut engine_frames, mut differing) = (0usize, 0usize);
    let mut buf: Vec<u8> = Vec::new();
    let mut expected_start = 0u64;
    let mut to_skip = skip_engine * ch;
    for chunk in rx {
        assert_eq!(
            chunk.start_sample, expected_start,
            "{key}: engine chunks are not contiguous"
        );
        expected_start += (chunk.samples.len() / ch) as u64;
        let samples = &chunk.samples[to_skip.min(chunk.samples.len())..];
        to_skip -= (chunk.samples.len() - samples.len()).min(to_skip);
        if samples.is_empty() {
            continue;
        }
        buf.resize(samples.len() * 4, 0);
        let got = fill(&mut ff, &mut buf);
        let usable = got / 4;
        for (i, s) in samples.iter().take(usable).enumerate() {
            let r = f32::from_le_bytes(buf[4 * i..4 * i + 4].try_into().unwrap());
            if s.to_bits() != r.to_bits() {
                differing += 1;
            }
            acc[i % ch].add(f64::from(*s), f64::from(r));
        }
        engine_frames += usable / ch;
        if usable < samples.len() {
            break;
        }
    }
    let mut rest = Vec::new();
    ff.read_to_end(&mut rest).expect("drain ffmpeg");
    let _ = child.wait();
    let ffmpeg_frames = skip_ffmpeg + engine_frames + rest.len() / 4 / ch;
    Witness {
        lag,
        r: acc.iter().map(Pearson::r).collect(),
        engine_frames: engine_frames + skip_engine,
        ffmpeg_frames,
        differing,
    }
}

/// The AAC claim, for one file: at lag 0, or at the engine's documented default
/// priming, every channel correlates with ffmpeg at 0.999 or better.
fn assert_aac(key: &str, path: &Path) {
    let w = compare(key, path);
    eprintln!(
        "{key}: lag {} frames, per-channel correlation {:?}; {} engine vs {} ffmpeg frames",
        w.lag, w.r, w.engine_frames, w.ffmpeg_frames
    );
    assert!(
        w.lag == 0 || w.lag == DEFAULT_PRIMING_LAG,
        "{key}: lag {} is neither 0 nor the engine's default priming",
        w.lag
    );
    for (c, v) in w.r.iter().enumerate() {
        assert!(
            *v >= 0.999,
            "{key} channel {c}: correlation {v} at lag {}",
            w.lag
        );
    }
}

#[test]
fn real_mp4_heaac_audio_correlates_with_ffmpeg() {
    let Some(path) = engine::real_library::film("mp4_heaac") else {
        return;
    };
    if !have("ffmpeg") {
        eprintln!("skipped: no ffmpeg to decode the reference with");
        return;
    }
    assert_aac("mp4_heaac (HE-AAC v1)", &path);
}

#[test]
fn real_mp4_aac_lc_audio_correlates_with_ffmpeg() {
    for key in ["mp4_h264_aac", "mp4_phone_h264"] {
        let Some(path) = engine::real_library::film(key) else {
            continue;
        };
        if !have("ffmpeg") {
            eprintln!("skipped: no ffmpeg to decode the reference with");
            return;
        }
        assert_aac(&format!("{key} (AAC-LC)"), &path);
    }
}

#[test]
fn real_wav_pcm16_is_sample_equal_to_ffmpeg() {
    let Some(path) = engine::real_library::film("wav_pcm16") else {
        return;
    };
    if !have("ffmpeg") {
        eprintln!("skipped: no ffmpeg to decode the reference with");
        return;
    }
    let w = compare("wav_pcm16", &path);
    eprintln!(
        "wav_pcm16: lag {}, {} engine vs {} ffmpeg frames, {} samples not bit-equal",
        w.lag, w.engine_frames, w.ffmpeg_frames, w.differing
    );
    assert_eq!(w.lag, 0, "wav_pcm16: a WAV has no priming to trim");
    assert_eq!(w.engine_frames, w.ffmpeg_frames, "wav_pcm16: frame count");
    assert_eq!(w.differing, 0, "wav_pcm16: samples that differ from ffmpeg");
}
