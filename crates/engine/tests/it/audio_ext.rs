//! DTS and the legacy audio codecs (IMA and MS ADPCM, ima4, A-law, mu-law, AMR-NB/WB)
//! imported and decoded through the engine from every container that carries them.
//!
//! The fixtures are made here, at test time, by ffmpeg: a short two-tone signal
//! encoded with the codec under test into a Matroska, mp4 / QuickTime / 3GP, WAVE,
//! AVI or MPEG transport stream file, or a raw `.dts` / `.amr`. Each file is held to
//! ffmpeg's own decode of the same bytes: mono and stereo sample for sample within a
//! tight tolerance (the decoders are integer or bit-exact against ffmpeg's), and the
//! 5.1 DTS, which reaches the timeline as the stereo pair every multichannel source
//! does, by correlation with ffmpeg's `-ac 2` fold (the fold's coefficients are
//! edith's, not ffmpeg's, so the samples cannot be equal).
//!
//! Skipped, loudly, where ffmpeg (or the encoder a case needs, AMR's above all) is
//! not installed.

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

/// Whether this ffmpeg lists `name` among its encoders (or decoders with `-decoders`).
fn ffmpeg_has(kind: &str, name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", kind])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(name))
        })
}

/// What a case is: how it is made, what comes out of the engine.
struct Case {
    name: &'static str,
    /// File extension, which picks ffmpeg's muxer.
    ext: &'static str,
    codec: &'static str,
    /// The encoder ffmpeg must have, else the case is skipped.
    encoder: &'static str,
    /// Extra output arguments (`-strict -2`, a bit rate, `-f dts`).
    extra: &'static [&'static str],
    rate: u32,
    /// Channels of the source; the timeline gets 1 or 2.
    channels: usize,
    /// A picture alongside the sound (`mpeg4`, or `mpeg2video` for a transport stream).
    video: Option<&'static str>,
    /// ffmpeg decoder to read the reference with, when the default is not the one.
    decoder: Option<&'static str>,
}

impl Case {
    const fn new(
        name: &'static str,
        ext: &'static str,
        codec: &'static str,
        rate: u32,
        channels: usize,
    ) -> Self {
        Self {
            name,
            ext,
            codec,
            encoder: codec,
            extra: &[],
            rate,
            channels,
            video: None,
            decoder: None,
        }
    }

    const fn extra(mut self, extra: &'static [&'static str]) -> Self {
        self.extra = extra;
        self
    }

    const fn video(mut self, codec: &'static str) -> Self {
        self.video = Some(codec);
        self
    }

    const fn decoder(mut self, decoder: &'static str) -> Self {
        self.decoder = Some(decoder);
        self
    }

    const fn encoder(mut self, encoder: &'static str) -> Self {
        self.encoder = encoder;
        self
    }

    /// The fixture, or `None` (said aloud) where ffmpeg cannot make it.
    fn make(&self) -> Option<Scratch> {
        if !have_ffmpeg() {
            eprintln!("skipped {}: no ffmpeg to encode the fixture with", self.name);
            return None;
        }
        if !ffmpeg_has("-encoders", self.encoder) {
            eprintln!(
                "skipped {}: this ffmpeg has no `{}` encoder to make the fixture with",
                self.name, self.encoder
            );
            return None;
        }
        if let Some(decoder) = self.decoder
            && !ffmpeg_has("-decoders", decoder)
        {
            eprintln!(
                "skipped {}: this ffmpeg has no `{decoder}` decoder to make the reference with",
                self.name
            );
            return None;
        }
        // A distinct tone per channel, so a swapped or dropped channel is heard.
        let tones = [440.0, 660.0, 880.0, 110.0, 550.0, 330.0];
        let expr = (0..self.channels)
            .map(|c| {
                format!(
                    "{:.2}*sin(2*PI*{}*t)+0.15*sin(2*PI*{}*t)",
                    0.35 - 0.02 * c as f64,
                    tones[c % tones.len()],
                    tones[(c + 2) % tones.len()] * 3.0
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        let file = Scratch::file(self.name, self.ext);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i"])
            .arg(format!("aevalsrc={expr}:s={}:d=2", self.rate));
        if let Some(video) = self.video {
            cmd.args(["-f", "lavfi", "-i", "color=c=gray:s=64x64:r=10:d=2"])
                .args(["-c:v", video, "-pix_fmt", "yuv420p"]);
        }
        let out = cmd
            .args(["-c:a", self.codec])
            .args(self.extra)
            .arg(&*file)
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success(),
            "ffmpeg ({}): {}",
            self.name,
            String::from_utf8_lossy(&out.stderr)
        );
        Some(file)
    }
}

/// ffmpeg's decode of `path`'s first audio track as f32, `channels` wide when given.
fn ffmpeg_f32(path: &Path, decoder: Option<&str>, channels: Option<&str>) -> Vec<f32> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if let Some(decoder) = decoder {
        cmd.args(["-c:a", decoder]);
    }
    cmd.arg("-i").arg(path).args(["-map", "0:a:0"]);
    if let Some(channels) = channels {
        cmd.args(["-ac", channels]);
    }
    let out = cmd.args(["-f", "f32le", "-"]).output().expect("ffmpeg runs");
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

/// The interleaved samples of `path`'s first audio track at its own `rate` (the mix
/// would otherwise take a source under 44.1 kHz up to 48 kHz), with its metadata.
fn decode_at(path: &Path, rate: u32) -> (engine::audio::AudioMeta, Vec<f32>) {
    let (meta, rx) = AudioSession::open_multi_streams_speed_at(
        &[(path.to_path_buf(), 0)],
        &[(Some(0), 0.0, f64::INFINITY)],
        &[],
        &[],
        Some(rate),
    )
    .expect("opens")
    .expect("has an audio track");
    let mut samples = Vec::new();
    for chunk in rx {
        samples.extend_from_slice(&chunk.samples);
    }
    (meta, samples)
}

fn channel(interleaved: &[f32], channels: usize, index: usize) -> Vec<f32> {
    interleaved
        .iter()
        .skip(index)
        .step_by(channels)
        .copied()
        .collect()
}

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

/// Opens the case's fixture through the engine and holds it to ffmpeg's decode.
fn check(case: &Case, tolerance: f32) {
    let Some(file) = case.make() else { return };
    let timeline = if case.channels == 1 { 1 } else { 2 };
    let (meta, ours) = decode_at(&file, case.rate);
    assert_eq!(
        (meta.sample_rate, usize::from(meta.channels)),
        (case.rate, timeline),
        "{}: rate and channels of the import",
        case.name
    );
    assert!(!ours.is_empty(), "{}: no sound out of the track", case.name);
    assert!(
        ours.iter().any(|s| s.abs() > 0.05),
        "{}: the decode is silent",
        case.name
    );

    if case.channels <= 2 {
        let theirs = ffmpeg_f32(&file, case.decoder, None);
        let slack = 4096 * timeline;
        assert!(
            ours.len().abs_diff(theirs.len()) <= slack,
            "{}: {} samples out, {} from ffmpeg",
            case.name,
            ours.len(),
            theirs.len()
        );
        let n = ours.len().min(theirs.len());
        let worst = ours[..n]
            .iter()
            .zip(&theirs[..n])
            .map(|(o, t)| (o - t).abs())
            .fold(0f32, f32::max);
        assert!(
            worst <= tolerance,
            "{}: worst sample differs from ffmpeg by {worst} (limit {tolerance})",
            case.name
        );
    } else {
        let theirs = ffmpeg_f32(&file, case.decoder, Some("2"));
        for index in 0..2 {
            let level = ours.iter().skip(index).step_by(2).map(|s| s.abs()).fold(0f32, f32::max);
            assert!(level > 1e-3, "{}: timeline channel {index} is silent", case.name);
            let score = correlation(&channel(&ours, 2, index), &channel(&theirs, 2, index));
            assert!(
                score > 0.98,
                "{}: timeline channel {index} correlates {score:.4} with ffmpeg's -ac 2 decode",
                case.name
            );
        }
    }
}

const DTS: &[&str] = &["-strict", "-2", "-b:a", "384k"];
/// Float decoding of the integer DTS core: well inside this on every sample.
const DTS_TOL: f32 = 2e-4;
const EXACT: f32 = 1e-4;

#[test]
fn dts_stereo_in_matroska_matches_ffmpeg() {
    check(&Case::new("ve_dts_mka", "mka", "dca", 48_000, 2).extra(DTS), DTS_TOL);
}

#[test]
fn dts_stereo_in_matroska_video_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_mkv", "mkv", "dca", 48_000, 2)
            .extra(DTS)
            .video("mpeg4"),
        DTS_TOL,
    );
}

#[test]
fn dts_51_in_matroska_folds_to_ffmpegs_stereo_by_correlation() {
    check(
        &Case::new("ve_dts_51_mka", "mka", "dca", 48_000, 6).extra(&["-strict", "-2", "-b:a", "1536k"]),
        DTS_TOL,
    );
}

#[test]
fn raw_dts_file_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_raw", "dts", "dca", 48_000, 2).extra(&["-strict", "-2", "-b:a", "384k", "-f", "dts"]),
        DTS_TOL,
    );
}

#[test]
fn dts_in_mp4_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_mp4", "mp4", "dca", 48_000, 2)
            .extra(DTS)
            .video("mpeg4"),
        DTS_TOL,
    );
}

#[test]
fn dts_in_transport_stream_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_ts", "ts", "dca", 48_000, 2)
            .extra(DTS)
            .video("mpeg2video"),
        DTS_TOL,
    );
}

#[test]
fn dts_in_wav_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_wav", "wav", "dca", 48_000, 2).extra(&["-strict", "-2", "-b:a", "384k", "-f", "wav"]),
        DTS_TOL,
    );
}

#[test]
fn dts_in_avi_matches_ffmpeg() {
    check(
        &Case::new("ve_dts_avi", "avi", "dca", 48_000, 2)
            .extra(DTS)
            .video("mpeg4"),
        DTS_TOL,
    );
}

#[test]
fn ima_adpcm_wav_matches_ffmpeg() {
    check(&Case::new("ve_ima_wav", "wav", "adpcm_ima_wav", 44_100, 2), EXACT);
    check(&Case::new("ve_ima_wav_mono", "wav", "adpcm_ima_wav", 22_050, 1), EXACT);
}

#[test]
fn ms_adpcm_wav_matches_ffmpeg() {
    check(&Case::new("ve_msadpcm_wav", "wav", "adpcm_ms", 44_100, 2), EXACT);
    check(&Case::new("ve_msadpcm_wav_mono", "wav", "adpcm_ms", 22_050, 1), EXACT);
}

#[test]
fn alaw_and_mulaw_wav_match_ffmpeg() {
    check(&Case::new("ve_alaw_wav", "wav", "pcm_alaw", 8_000, 1), EXACT);
    check(&Case::new("ve_mulaw_wav", "wav", "pcm_mulaw", 8_000, 1), EXACT);
    check(&Case::new("ve_alaw_wav_st", "wav", "pcm_alaw", 44_100, 2), EXACT);
}

#[test]
fn adpcm_and_g711_in_avi_match_ffmpeg() {
    for (name, codec, rate) in [
        ("ve_ima_avi", "adpcm_ima_wav", 44_100),
        ("ve_msadpcm_avi", "adpcm_ms", 44_100),
        ("ve_alaw_avi", "pcm_alaw", 8_000),
        ("ve_mulaw_avi", "pcm_mulaw", 8_000),
    ] {
        let channels = if rate == 8_000 { 1 } else { 2 };
        check(&Case::new(name, "avi", codec, rate, channels).video("mpeg4"), EXACT);
    }
}

#[test]
fn ima4_and_g711_in_quicktime_match_ffmpeg() {
    check(
        &Case::new("ve_ima4_mov", "mov", "adpcm_ima_qt", 44_100, 2).video("mpeg4"),
        EXACT,
    );
    check(
        &Case::new("ve_alaw_mov", "mov", "pcm_alaw", 8_000, 1).video("mpeg4"),
        EXACT,
    );
    check(
        &Case::new("ve_mulaw_mov", "mov", "pcm_mulaw", 8_000, 1).video("mpeg4"),
        EXACT,
    );
}

#[test]
fn amr_nb_in_3gp_and_raw_matches_ffmpeg() {
    let nb = |name, ext, extra: &'static [&'static str]| {
        Case::new(name, ext, "libopencore_amrnb", 8_000, 1)
            .extra(extra)
            .decoder("libopencore_amrnb")
    };
    check(&nb("ve_amrnb_3gp", "3gp", &["-b:a", "12.2k"]).video("mpeg4"), EXACT);
    check(&nb("ve_amrnb_raw", "amr", &["-b:a", "7.95k"]), EXACT);
    check(&nb("ve_amrnb_mov", "mov", &["-b:a", "10.2k"]).video("mpeg4"), EXACT);
}

#[test]
fn amr_wb_in_3gp_and_raw_matches_ffmpeg() {
    let wb = |name, ext| {
        Case::new(name, ext, "libvo_amrwbenc", 16_000, 1)
            .extra(if ext == "awb" { &["-f", "amr"] } else { &["-f", "3gp"] })
            .encoder("libvo_amrwbenc")
            .decoder("libopencore_amrwb")
    };
    check(&wb("ve_amrwb_3gp", "3gp").video("mpeg4"), EXACT);
    check(&wb("ve_amrwb_raw", "awb"), EXACT);
}

#[test]
fn dts_refuses_lossless_by_name_and_reports_decodable_streams() {
    // The stream listing of a DTS Matroska file says it decodes.
    let Some(file) = Case::new("ve_dts_probe", "mka", "dca", 48_000, 2).extra(DTS).make() else {
        return;
    };
    let streams = AudioSession::probe_streams(&*file).expect("lists");
    assert_eq!(streams.len(), 1);
    assert!(streams[0].decodable, "{:?}", streams[0]);
    assert_eq!((streams[0].sample_rate, streams[0].channels), (48_000, 2));
    assert_eq!(AudioSession::unsupported(&*file).expect("asks"), None);
}

#[test]
fn dts_window_starts_where_asked() {
    let Some(file) = Case::new("ve_dts_window", "mka", "dca", 48_000, 2).extra(DTS).make() else {
        return;
    };
    let (_, whole) = decode_at(&file, 48_000);
    let (meta, rx) = AudioSession::open_at(&*file, 1.0).expect("opens").expect("has sound");
    let mut tail = Vec::new();
    for chunk in rx {
        tail.extend_from_slice(&chunk.samples);
    }
    let from = 48_000 * usize::from(meta.channels);
    let n = tail.len().min(whole.len() - from);
    assert!(n > 40_000, "the window decoded only {n} samples");
    let worst = tail[..n]
        .iter()
        .zip(&whole[from..from + n])
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(worst < 2e-3, "a window at 1 s differs from the whole decode by {worst}");
}

/// MP2 (MPEG-1 Layer II). ffmpeg writes it into mp4 under the MP3 object type (0x6B), and into
/// Matroska as `A_MPEG/L2`; the frames are read by `ec-mp3`, which takes every layer.
const MP2_TOL: f32 = 1e-3;

#[test]
fn mp2_in_mp4_and_matroska_matches_ffmpeg() {
    let mp2 = |name, ext| {
        Case::new(name, ext, "mp2", 44_100, 1).extra(&["-b:a", "128k"]).video("mpeg4")
    };
    check(&mp2("ve_mp2_mp4", "mp4"), MP2_TOL);
    check(&mp2("ve_mp2_mkv", "mkv"), MP2_TOL);
}

#[test]
fn mp2_is_named_mp2_and_decodes_in_mp4_and_matroska() {
    for (name, ext) in [("ve_mp2_named_mp4", "mp4"), ("ve_mp2_named_mkv", "mkv")] {
        let case = Case::new(name, ext, "mp2", 44_100, 1).extra(&["-b:a", "128k"]).video("mpeg4");
        let Some(file) = case.make() else {
            return;
        };
        let streams = AudioSession::probe_streams(&*file).expect("lists");
        assert_eq!(streams.len(), 1, "{name}");
        assert_eq!(streams[0].codec, "mp2", "{name}: named by its layer, not as mp3");
        assert!(streams[0].decodable, "{name}: {:?}", streams[0]);
        assert_eq!(AudioSession::unsupported(&*file).expect("asks"), None, "{name}");
    }
}
