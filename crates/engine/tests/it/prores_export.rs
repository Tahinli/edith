//! ProRes export (`Format::ProRes`): every profile writes a `.mov` that ffprobe
//! reads as ProRes in the right profile and pixel format, with a 24-bit PCM
//! sound track, the source's duration and frame count; ffmpeg decodes it back
//! within a PSNR floor of the source; the sound is in sync; a cancel leaves no
//! file.
//!
//! The source is synthetic (ffmpeg `testsrc2` and `sine`, H.264 + AAC in an
//! mp4). Without ffmpeg and ffprobe the tests say so and pass.
//!
//! ```text
//! cargo test -p engine --release --test prores_export -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::export::{ExportSettings, Format, ProResProfile};

use crate::seat;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 25;
const SECONDS: u32 = 2;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

fn tools() -> bool {
    have("ffmpeg") && have("ffprobe")
}

/// `seconds` of testsrc2 with a 440 Hz sine beside it, H.264 + AAC in an mp4.
fn make_source(out: &Path, width: u32, height: u32, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={width}x{height}:rate={FPS}:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:sample_rate=48000:duration={seconds}"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "10", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k", "-shortest"])
        .arg(out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg makes the source");
}

/// One ffprobe field of the first stream of `kind` (`v:0`, `a:0`).
fn probe(path: &Path, stream: &str, field: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries"])
        .arg(format!("stream={field}"))
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn frame_count(path: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().parse().expect("a frame count")
}

fn format_duration(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().parse().expect("a duration")
}

fn decoded_video(path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decodes {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn decoded_audio(path: &Path) -> Vec<i16> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "48000", "-f", "s16le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decodes the sound of {}", path.display());
    out.stdout.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

fn psnr_10(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let mut squared = 0f64;
    for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
        let d = f64::from(u16::from_le_bytes(*x)) - f64::from(u16::from_le_bytes(*y));
        squared += d * d;
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (1023.0f64 * 1023.0 * (a.len() / 2) as f64 / squared).log10()
}

fn export(source: &Path, out: &Path, settings: &ExportSettings) {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let handle = session.export_to_with(out, settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

#[test]
fn every_profile_exports_a_prores_mov_that_decodes_back() {
    if !tools() {
        eprintln!("no ffmpeg/ffprobe: skipping the ProRes export test");
        return;
    }
    let _seat = seat::software();
    let dir = scratch("prores");
    let source = dir.join("source.mp4");
    make_source(&source, WIDTH, HEIGHT, SECONDS);
    let reference_y = decoded_video(&source, "gray10le");
    let reference_420 = decoded_video(&source, "yuv420p10le");
    let source_audio = decoded_audio(&source);
    let frames = (FPS * SECONDS) as usize;

    assert_eq!(Format::ProRes.ext(), "mov");
    // (profile, ffprobe profile name, pix_fmt, luma PSNR floor, all-plane floor)
    let table = [
        (ProResProfile::Proxy, "Proxy", "yuv422p10le", 32.0, 30.0),
        (ProResProfile::Lt, "LT", "yuv422p10le", 36.0, 33.0),
        (ProResProfile::Standard, "Standard", "yuv422p10le", 38.0, 35.0),
        (ProResProfile::Hq, "HQ", "yuv422p10le", 40.0, 36.0),
        // ffmpeg's decoder reports 4444 at 12 bits (the codec's 12-bit path); the
        // encoder is fed 10-bit 4:4:4 and the stream is 4:4:4 either way.
        (ProResProfile::P4444, "4444", "yuv444p12le", 40.0, 33.0),
        (ProResProfile::P4444Xq, "XQ", "yuv444p12le", 40.0, 33.0),
    ];
    for (profile, name, pix_fmt, luma_floor, all_floor) in table {
        let out = dir.join(format!("{}.mov", name.replace(' ', "_")));
        let settings = ExportSettings { format: Format::ProRes, prores_profile: profile, ..Default::default() };
        export(&source, &out, &settings);

        assert_eq!(probe(&out, "v:0", "codec_name"), "prores", "{name}");
        assert_eq!(probe(&out, "v:0", "profile"), name, "{name}: the profile");
        assert_eq!(probe(&out, "v:0", "pix_fmt"), pix_fmt, "{name}: the pixel format");
        assert_eq!(probe(&out, "a:0", "codec_name"), "pcm_s24be", "{name}: the sound is 24-bit PCM");
        assert_eq!(probe(&out, "a:0", "channel_layout"), "mono", "{name}: the layout");
        assert_eq!(probe(&out, "a:0", "sample_rate"), "48000", "{name}");
        assert_eq!(frame_count(&out), frames, "{name}: every picture is there");
        let duration = format_duration(&out);
        assert!((duration - f64::from(SECONDS)).abs() < 0.05, "{name}: duration {duration}");

        // Luma against the source: ProRes' own quantiser is the only loss, since
        // luma is not resampled on the way.
        let luma = psnr_10(&reference_y, &decoded_video(&out, "gray10le"));
        let all = psnr_10(&reference_420, &decoded_video(&out, "yuv420p10le"));
        eprintln!("{name}: luma {luma:.2} dB, all planes (4:2:0 view) {all:.2} dB");
        assert!(luma >= luma_floor, "{name}: luma PSNR {luma:.2} dB < {luma_floor}");
        assert!(all >= all_floor, "{name}: PSNR {all:.2} dB < {all_floor}");

        // The sound is the source's, in sync: the best lag between the two over a
        // window is zero (within 1 ms) and the correlation there is high.
        let audio = decoded_audio(&out);
        assert!(
            (audio.len() as i64 - source_audio.len() as i64).abs() <= 2048,
            "{name}: sound length {} vs {}",
            audio.len(),
            source_audio.len()
        );
        let (lag, corr) = best_lag(&source_audio, &audio);
        assert!(lag.abs() <= 48, "{name}: A/V lag of {lag} samples");
        assert!(corr > 0.99, "{name}: sound correlation {corr:.4}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lag (in samples, of `b` against `a`) of the best normalised correlation
/// over a 0.25 s window starting 0.5 s in, within +-100 ms, and its value.
fn best_lag(a: &[i16], b: &[i16]) -> (i64, f64) {
    let start = 24_000usize;
    let len = 12_000usize;
    let mut best = (0i64, f64::MIN);
    for lag in -96i64..=96 {
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for i in start..start + len {
            let x = f64::from(a[i]);
            let y = f64::from(b[(i as i64 + lag) as usize]);
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
        let c = ab / (aa.sqrt() * bb.sqrt()).max(1.0);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

#[test]
fn cancelling_a_prores_export_leaves_no_file() {
    if !tools() {
        eprintln!("no ffmpeg/ffprobe: skipping the ProRes cancel test");
        return;
    }
    let _seat = seat::software();
    let dir = scratch("prores-cancel");
    let source = dir.join("source.mp4");
    make_source(&source, 1920, 1080, 12);
    let out = dir.join("cancel.mov");
    let session = PlaybackSession::open(&source).expect("open");
    let settings = ExportSettings { format: Format::ProRes, prores_profile: ProResProfile::Hq, ..Default::default() };
    let handle = session.export_to_with(&out, &settings);
    let started = Instant::now();
    while handle.progress() < 0.1 {
        assert!(started.elapsed() < Duration::from_secs(120), "export never got going");
        assert!(!handle.is_finished(), "export finished before it was cancelled");
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.cancel();
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(60), "the cancel never landed");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.result().expect("an outcome").is_err(), "a cancelled export is an error");
    assert!(!out.exists(), "no partial file survives a cancel");
    let mut part = out.clone().into_os_string();
    part.push(".part");
    assert!(!Path::new(&part).exists(), "the .part is cleaned up too");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_other_format_keeps_its_extension() {
    assert_eq!(Format::Mp4.ext(), "mp4");
    assert_eq!(Format::Hevc.ext(), "mkv");
    assert_eq!(Format::HevcMp4.ext(), "mp4");
    assert_eq!(ExportSettings::default().prores_profile, ProResProfile::Hq);
}

/// The sound of `path` as 32-bit floats (exact for 24-bit PCM), all channels interleaved.
fn decoded_f32(path: &Path, channels: usize) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", &channels.to_string(), "-ar", "48000", "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decodes the sound of {}", path.display());
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// A source with `channels` of 440 Hz sine (mono or stereo by `layout`).
fn make_source_layout(out: &Path, layout: &str) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size=320x240:rate={FPS}:duration=1"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:sample_rate=48000:duration=1,volume=0.5,aformat=channel_layouts={layout}"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "256k", "-shortest"])
        .arg(out)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg makes the {layout} source");
}

#[test]
fn prores_sound_is_a_real_24_bit_mix_with_its_channel_layout() {
    if !tools() {
        eprintln!("no ffmpeg/ffprobe: skipping the ProRes 24-bit sound test");
        return;
    }
    let _seat = seat::software();
    let dir = scratch("prores-24");
    // The mixer folds anything wider than stereo down, so 5.1 never reaches the file.
    for (layout, name, channels) in [("mono", "mono", 1usize), ("stereo", "stereo", 2)] {
        let source = dir.join(format!("source-{name}.mp4"));
        make_source_layout(&source, layout);
        let out = dir.join(format!("{name}.mov"));
        let settings = ExportSettings { format: Format::ProRes, prores_profile: ProResProfile::Proxy, ..Default::default() };
        export(&source, &out, &settings);
        assert_eq!(probe(&out, "a:0", "codec_name"), "pcm_s24be", "{name}");
        assert_eq!(probe(&out, "a:0", "bits_per_sample"), "24", "{name}");
        assert_eq!(probe(&out, "a:0", "channels"), channels.to_string(), "{name}");
        assert_eq!(probe(&out, "a:0", "channel_layout"), layout, "{name}: the channel layout");

        // A 24-bit reference mix: the source decoded to float, scaled to 24 bits and
        // rounded, then compared at the lag the files differ by (found, not assumed).
        let reference = decoded_f32(&source, channels);
        let got = decoded_f32(&out, channels);
        let scale = 8_388_608.0f64;
        let n = reference.len().min(got.len());
        assert!(n >= 40_000 * channels, "{name}: {n} samples");
        let mut worst = 0f64;
        let mut distinct = std::collections::HashSet::new();
        for i in 4_800 * channels..n - 4_800 * channels {
            let r = (f64::from(reference[i]) * 8_388_607.0).round();
            let g = f64::from(got[i]) * scale;
            worst = worst.max((r - g).abs());
            distinct.insert((g as i64) & 0xFF);
        }
        eprintln!("{name}: worst difference {worst} LSB, {} distinct low bytes", distinct.len());
        assert!(worst <= 1.0, "{name}: {worst} LSB from the 24-bit reference");
        assert!(distinct.len() > 128, "{name}: the low byte is used, so this is not a widened 16-bit mix");
    }
    // Asking for 16 bits still gets 16.
    let source = dir.join("source-stereo.mp4");
    let out = dir.join("sixteen.mov");
    let settings = ExportSettings {
        format: Format::ProRes,
        prores_profile: ProResProfile::Proxy,
        audio_bit_depth: Some(16),
        ..Default::default()
    };
    export(&source, &out, &settings);
    assert_eq!(probe(&out, "a:0", "codec_name"), "pcm_s16le");
    let _ = std::fs::remove_dir_all(&dir);
}
