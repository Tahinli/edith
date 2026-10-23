//! The export preset table ([`engine::export_presets`]): every preset applies onto a settings
//! value without clearing the chapters, and a short generated clip exports with `youtube_1080p`
//! and `podcast`, read back with ffprobe (codec, size, sample rate) and measured with ffmpeg's
//! `ebur128` (integrated loudness within 1 LU of -16 LUFS, true peak under -1 dBTP + codec slack).
//! Without ffmpeg the export tests say so and pass.
//!
//! ```text
//! cargo test -p engine --release --test export_presets
//! ```

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use engine::export::{EncoderSeat, ExportSettings, Format};
use engine::export_presets::{apply, by_id, presets};
use engine::scratch::Scratch;
use engine::PlaybackSession;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
        && Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A 2-second 320x180 H.264 clip with a quiet (-25 dBFS-ish) 48 kHz stereo AAC sine.
fn make_clip(out: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=2"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2"])
        .args(["-af", "volume=0.05,aformat=channel_layouts=stereo"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ar", "48000", "-shortest"])
        .arg(out)
        .status()
        .is_ok_and(|s| s.success())
}

fn probe(path: &Path, stream: &str, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Integrated loudness (LUFS) and true peak (dBTP) of the file's sound, from ffmpeg's ebur128 summary.
fn measure(path: &Path) -> (f64, f64) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-vn", "-af", "ebur128=peak=true", "-f", "null", "-"])
        .output()
        .expect("ffmpeg runs");
    let text = String::from_utf8_lossy(&out.stderr);
    let summary = text.rsplit("Summary:").next().expect("a summary");
    let num = |key: &str| -> f64 {
        let line = summary.lines().find(|l| l.trim_start().starts_with(key)).unwrap_or_else(|| panic!("{key} in {summary}"));
        line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or_else(|| panic!("{key} figure in {line}"))
    };
    (num("I:"), num("Peak:"))
}

fn export(src: &Path, name: &str, id: &str) -> Scratch {
    let preset = by_id(id).expect("the preset exists");
    let settings = apply(
        preset,
        ExportSettings { seat: EncoderSeat::Software, ..ExportSettings::default() },
    );
    let session = PlaybackSession::open(src).expect("open the clip");
    let out = Scratch::file(&format!("ve_preset_{name}"), settings.format.ext());
    let handle = session.export_to_with(&out, &settings);
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if let Some(result) = handle.result() {
            result.expect("the export finishes");
            break;
        }
        assert!(Instant::now() < deadline, "export never settled");
        std::thread::sleep(Duration::from_millis(20));
    }
    out
}

#[test]
fn every_preset_applies_and_keeps_the_chapters() {
    assert!(!presets().is_empty());
    let mut seen = std::collections::HashSet::new();
    for p in presets() {
        assert!(seen.insert(p.id), "duplicate id {}", p.id);
        let base = ExportSettings {
            chapters: vec![(0, "one".into()), (1_000_000_000, "two".into())],
            video_bit_depth: Some(8),
            subtitles: vec![0],
            ..ExportSettings::default()
        };
        let got = apply(p, base.clone());
        assert_eq!(got.format, p.format, "{}", p.id);
        assert_eq!(got.chapters, base.chapters, "{} cleared the chapters", p.id);
        assert_eq!(got.subtitles, base.subtitles, "{}", p.id);
        assert_eq!(got.bitrate, p.bitrate, "{}", p.id);
        assert_eq!(got.size, p.target.size, "{}", p.id);
        assert_eq!(got.loudness, p.loudness, "{}", p.id);
        assert_eq!(got.video_bit_depth, p.video_bit_depth.or(Some(8)), "{}", p.id);
    }
    let shorts = apply(by_id("shorts_reels").unwrap(), ExportSettings::default());
    assert_eq!(shorts.size, Some((1080, 1920)));
    let archive = apply(by_id("archive").unwrap(), ExportSettings::default());
    assert_eq!((archive.format, archive.video_bit_depth), (Format::Av1, Some(10)));
    let yt = apply(by_id("youtube_1080p").unwrap(), ExportSettings::default());
    assert_eq!((yt.bitrate, yt.audio_kbps), (Some(8_000_000), Some(384)));
    let podcast = apply(by_id("podcast").unwrap(), ExportSettings::default());
    let l = podcast.loudness.expect("a loudness target");
    assert_eq!((l.lufs, l.true_peak_dbtp), (-16.0, -1.0));
    assert!(by_id("nope").is_none());
}

#[test]
fn youtube_1080p_exports_h264_1080p_with_48k_aac() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_preset_src_yt", "mp4");
    assert!(make_clip(&src), "ffmpeg makes the clip");
    let out = export(&src, "yt", "youtube_1080p");
    assert_eq!(probe(&out, "v:0", "stream=codec_name,width,height"), "h264,1920,1080");
    assert_eq!(probe(&out, "a:0", "stream=codec_name,sample_rate"), "aac,48000");
}

#[test]
fn podcast_lands_on_minus_16_lufs_under_the_peak_ceiling() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let src = Scratch::file("ve_preset_src_pod", "mp4");
    assert!(make_clip(&src), "ffmpeg makes the clip");
    let (before, _) = measure(&src);
    assert!(before < -20.0, "the source is quiet: {before} LUFS");
    for (id, codec) in [("podcast", "aac"), ("podcast_ogg", "vorbis"), ("podcast_opus", "opus")] {
        let out = export(&src, id, id);
        assert_eq!(probe(&out, "a:0", "stream=codec_name,sample_rate"), format!("{codec},48000"), "{id}");
        assert!(probe(&out, "v:0", "stream=codec_name").is_empty(), "{id} has no picture");
        let (lufs, peak) = measure(&out);
        assert!((lufs + 16.0).abs() <= 1.0, "{id}: {lufs} LUFS is not within 1 LU of -16");
        assert!(peak <= -0.5, "{id}: true peak {peak} dBTP");
        println!("{id}: {lufs} LUFS, peak {peak} dBTP");
    }
}
