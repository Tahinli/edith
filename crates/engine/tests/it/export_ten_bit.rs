//! The software AV1 and HEVC exports at 10 bits: a 10-bit source exported to AV1
//! or to HEVC (Main 10) comes out 10-bit, in Matroska and in mp4, and an 8-bit
//! source (or an explicit 8-bit setting) still comes out 8-bit (Main).
//!
//! The source is generated with ffmpeg (a 1-second 10-bit HEVC Main 10 clip, or
//! a 10-bit libaom AV1 clip where ffmpeg has no libx265). Each export is read back
//! with ffmpeg and held against the source at 10-bit precision: PSNR above 35 dB,
//! and above the same clip exported at 8 bits, which is what keeps the 10-bit path
//! honest about banding. Without ffmpeg the test says so and passes.
//!
//! ```text
//! cargo test -p engine --release --test export_ten_bit -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::color::ColorParams;
use engine::export::{ExportSettings, Format};
use engine::project::Lane;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 25;

/// A scratch directory for this run's sources and exports, emptied first.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The ffmpeg encoder to make the sources with: libx265 where it is built in
/// (it codes 10-bit input as Main 10), else libaom-av1. `None` when neither is.
fn source_encoder() -> Option<&'static str> {
    let out = Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output().ok()?;
    let list = String::from_utf8_lossy(&out.stdout);
    if list.contains("libx265") {
        Some("libx265")
    } else if list.contains("libaom-av1") {
        Some("libaom-av1")
    } else {
        None
    }
}

/// The codec arguments for `encoder`, at the fastest speed the source needs.
fn codec_args(encoder: &str) -> Vec<&str> {
    match encoder {
        "libx265" => vec!["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error"],
        _ => vec!["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "30", "-b:v", "0"],
    }
}

/// A 1-second test pattern at `pix_fmt`, encoded with `codec` into `out`.
fn make_source(out: &Path, pix_fmt: &str, codec: &[&str]) -> bool {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={WIDTH}x{HEIGHT}:rate={FPS}:duration=1"))
        .args(["-pix_fmt", pix_fmt])
        .args(codec)
        .arg(out)
        .status();
    matches!(status, Ok(s) if s.success())
}

/// The first video stream's profile, as ffprobe names it ("Main", "Main 10").
fn profile(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=profile"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The first video stream's pixel format, as ffprobe names it.
fn pix_fmt(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Every frame of `path` as ffmpeg decodes it, as raw `pix_fmt` planes.
fn decoded(path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg decodes {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// PSNR over every sample of two streams of little-endian 16-bit planes, at the
/// 10-bit peak (1023). Both streams must hold the same number of samples.
fn psnr_10(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let mut squared = 0f64;
    let mut count = 0usize;
    for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
        let x = f64::from(u16::from_le_bytes(*x));
        let y = f64::from(u16::from_le_bytes(*y));
        squared += (x - y) * (x - y);
        count += 1;
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (1023.0f64 * 1023.0 * count as f64 / squared).log10()
}

/// Exports `source` to `out` in `format`, at `video_bit_depth`, failing on the
/// deadline rather than hanging.
fn export_as(source: &Path, out: &Path, format: Format, video_bit_depth: Option<u8>) {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let settings = ExportSettings {
        format,
        video_bit_depth,
        ..Default::default()
    };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

#[test]
fn a_ten_bit_source_exports_to_ten_bit_av1() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the 10-bit export test");
        return;
    };
    let dir = scratch("ten-bit");
    let source = dir.join("source10.mkv");
    assert!(make_source(&source, "yuv420p10le", &codec_args(encoder)), "ffmpeg makes the 10-bit source");
    assert_eq!(pix_fmt(&source), "yuv420p10le", "the source is 10-bit");
    let reference = decoded(&source, "yuv420p10le");
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2 * 2) as usize;
    let frames = reference.len() / frame_bytes;
    assert!(frames >= FPS as usize - 1, "the source has about a second of pictures");

    for (format, ext) in [(Format::Av1, "mkv"), (Format::Av1Mp4, "mp4")] {
        // The default follows the source: a 10-bit file.
        let ten = dir.join(format!("ten.{ext}"));
        export_as(&source, &ten, format, None);
        assert_eq!(pix_fmt(&ten), "yuv420p10le", "{ext}: a 10-bit source exports 10-bit AV1");
        let ten_decoded = decoded(&ten, "yuv420p10le");
        assert_eq!(ten_decoded.len() / frame_bytes, frames, "{ext}: every picture reads back");
        let psnr_ten = psnr_10(&reference, &ten_decoded);

        // The same clip at 8 bits, read back at 10 bits.
        let eight = dir.join(format!("eight.{ext}"));
        export_as(&source, &eight, format, Some(8));
        assert_eq!(pix_fmt(&eight), "yuv420p", "{ext}: an explicit 8-bit setting wins");
        let eight_decoded = decoded(&eight, "yuv420p10le");
        assert_eq!(eight_decoded.len() / frame_bytes, frames, "{ext}: every picture reads back");
        let psnr_eight = psnr_10(&reference, &eight_decoded);

        eprintln!("{ext}: 10-bit export {psnr_ten:.2} dB, 8-bit export {psnr_eight:.2} dB at 10-bit");
        assert!(psnr_ten > 35.0, "{ext}: the 10-bit export is {psnr_ten:.2} dB against the source");
        assert!(
            psnr_ten > psnr_eight,
            "{ext}: the 10-bit export ({psnr_ten:.2} dB) is no closer than the 8-bit one ({psnr_eight:.2} dB)"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_ten_bit_source_exports_to_main_10_hevc() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the HEVC 10-bit export test");
        return;
    };
    let dir = scratch("ten-bit-hevc");
    let source = dir.join("source10.mkv");
    assert!(make_source(&source, "yuv420p10le", &codec_args(encoder)), "ffmpeg makes the 10-bit source");
    let reference = decoded(&source, "yuv420p10le");
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2 * 2) as usize;
    let frames = reference.len() / frame_bytes;
    assert!(frames >= FPS as usize - 1, "the source has about a second of pictures");

    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        // An explicit 10-bit setting, so the file is coded here: the default on a
        // 10-bit HEVC source in Matroska is a copy of its bitstream.
        let ten = dir.join(format!("ten.{ext}"));
        export_as(&source, &ten, format, Some(10));
        assert_eq!(pix_fmt(&ten), "yuv420p10le", "{ext}: a 10-bit export is 10-bit HEVC");
        assert_eq!(profile(&ten), "Main 10", "{ext}: the HEVC export is Main 10");
        let ten_decoded = decoded(&ten, "yuv420p10le");
        assert_eq!(ten_decoded.len() / frame_bytes, frames, "{ext}: every picture reads back");
        let psnr_ten = psnr_10(&reference, &ten_decoded);

        // The same clip at 8 bits, read back at 10 bits.
        let eight = dir.join(format!("eight.{ext}"));
        export_as(&source, &eight, format, Some(8));
        assert_eq!(pix_fmt(&eight), "yuv420p", "{ext}: an explicit 8-bit setting wins");
        assert_eq!(profile(&eight), "Main", "{ext}: the 8-bit HEVC export is Main");
        let eight_decoded = decoded(&eight, "yuv420p10le");
        assert_eq!(eight_decoded.len() / frame_bytes, frames, "{ext}: every picture reads back");
        let psnr_eight = psnr_10(&reference, &eight_decoded);

        eprintln!("hevc {ext}: 10-bit export {psnr_ten:.2} dB, 8-bit export {psnr_eight:.2} dB at 10-bit");
        assert!(psnr_ten > 30.0, "{ext}: the 10-bit HEVC export is {psnr_ten:.2} dB against the source");
        assert!(
            psnr_ten > psnr_eight,
            "{ext}: the 10-bit HEVC export ({psnr_ten:.2} dB) is no closer than the 8-bit one ({psnr_eight:.2} dB)"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_eight_bit_source_still_exports_eight_bit() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the 8-bit export test");
        return;
    };
    let dir = scratch("eight-bit");
    let source = dir.join("source8.mkv");
    assert!(make_source(&source, "yuv420p", &codec_args(encoder)), "ffmpeg makes the 8-bit source");
    assert_eq!(pix_fmt(&source), "yuv420p", "the source is 8-bit");

    for (format, ext) in [(Format::Av1, "mkv"), (Format::Av1Mp4, "mp4")] {
        let out = dir.join(format!("out.{ext}"));
        export_as(&source, &out, format, None);
        assert_eq!(pix_fmt(&out), "yuv420p", "{ext}: an 8-bit source exports 8-bit AV1");
    }
    for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
        let out = dir.join(format!("out-hevc.{ext}"));
        export_as(&source, &out, format, None);
        assert_eq!(pix_fmt(&out), "yuv420p", "{ext}: an 8-bit source exports 8-bit HEVC");
        assert_eq!(profile(&out), "Main", "{ext}: an 8-bit source exports Main HEVC");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 1-second 10-bit diagonal ramp: luma runs over the whole limited span (64 to
/// 940) along x + y, so a picture has about 560 distinct luma codes, and chroma is
/// neutral. Written raw, then encoded with `encoder` and the ffmpeg `tags` into `out`.
fn make_ramp_source(out: &Path, encoder: &str, tags: &[&str]) -> bool {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut raw = Vec::new();
    for _ in 0..FPS {
        for y in 0..h {
            for x in 0..w {
                let code = 64 + (x + y) * 876 / (w + h - 2);
                raw.extend_from_slice(&(code as u16).to_le_bytes());
            }
        }
        for _ in 0..2 * (w / 2) * (h / 2) {
            raw.extend_from_slice(&512u16.to_le_bytes());
        }
    }
    let input = out.with_extension("yuv");
    if std::fs::write(&input, &raw).is_err() {
        return false;
    }
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le"])
        .args(["-s", &format!("{WIDTH}x{HEIGHT}"), "-r", &FPS.to_string()])
        .args(tags)
        .arg("-i")
        .arg(&input)
        .args(["-pix_fmt", "yuv420p10le"])
        .args(codec_args(encoder))
        .arg(out)
        .status();
    let _ = std::fs::remove_file(&input);
    matches!(status, Ok(s) if s.success())
}

/// One colour field of the first video stream, as ffprobe names it.
fn probe(path: &Path, entry: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(format!("stream={entry}"))
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Exports `session` to `out` at the settings, failing on the deadline.
fn export_session(session: &PlaybackSession, out: &Path, settings: &ExportSettings) {
    let handle = session.export_to_with(out, settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

#[test]
fn a_graded_and_faded_gradient_keeps_more_than_256_luma_levels_at_ten_bits() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the composed 10-bit test");
        return;
    };
    let dir = scratch("ten-bit-compose");
    let source = dir.join("ramp.mkv");
    assert!(make_ramp_source(&source, encoder, &[]), "ffmpeg makes the 10-bit ramp");
    assert_eq!(pix_fmt(&source), "yuv420p10le", "the source is 10-bit");

    // A grade and a one-second fade-in: both are composed, so this export takes
    // the 10-bit composition path, not the untouched one.
    let mut session = PlaybackSession::open(&source).expect("open the ramp");
    let grade = ColorParams {
        brightness: 0.05,
        contrast: 1.1,
        ..Default::default()
    };
    assert!(session.set_color(Lane::V1, 0, Some(grade)), "the grade went on the clip");
    assert!(session.set_fade_in(Lane::V1, 0, FPS), "the fade-in went on the clip");

    let out = dir.join("graded.mkv");
    export_session(
        &session,
        &out,
        &ExportSettings {
            format: Format::Av1,
            ..Default::default()
        },
    );
    assert_eq!(pix_fmt(&out), "yuv420p10le", "the composed export is 10-bit");

    let frame_luma = (WIDTH * HEIGHT * 2) as usize;
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2 * 2) as usize;
    let decoded = decoded(&out, "yuv420p10le");
    let mut seen = [false; 1024];
    for frame in decoded.chunks_exact(frame_bytes) {
        for sample in frame[..frame_luma].as_chunks::<2>().0 {
            seen[usize::from(u16::from_le_bytes(*sample)).min(1023)] = true;
        }
    }
    let levels = seen.iter().filter(|&&s| s).count();
    eprintln!("graded, faded export: {levels} distinct luma levels at 10 bits");
    assert!(
        levels > 256,
        "the composed export has {levels} luma levels: it was narrowed to 8 bits somewhere"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clip whose only animation is colour keys stays on the 10-bit composition
/// path, graded per picture by the keys. Its first and last pictures sit on
/// the keys, so each must be the very picture a constant grade of that key's
/// value exports at 10 bits: sample for sample, which an 8-bit pass (about a
/// code of error everywhere on a gradient) cannot match.
#[test]
fn a_colour_keyed_ten_bit_clip_is_graded_at_ten_bits() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the colour-keyed 10-bit test");
        return;
    };
    let dir = scratch("ten-bit-color-keys");
    let source = dir.join("ramp.mkv");
    assert!(make_ramp_source(&source, encoder, &[]), "ffmpeg makes the 10-bit ramp");
    let bright = |brightness| ColorParams { brightness, ..Default::default() };
    let settings = ExportSettings {
        format: Format::Av1,
        video_bit_depth: Some(10),
        // High enough that the encoder's own noise does not hide the grid.
        bitrate: Some(100_000_000),
        ..Default::default()
    };
    let frame_luma = (WIDTH * HEIGHT * 2) as usize;
    let frame_bytes = (WIDTH * HEIGHT * 3 / 2 * 2) as usize;
    let luma_of = |decoded: &[u8], frame: usize| -> Vec<u16> {
        decoded[frame * frame_bytes..][..frame_luma]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| u16::from_le_bytes(*s))
            .collect()
    };
    let export = |name: &str, setup: &dyn Fn(&mut PlaybackSession)| {
        let mut session = PlaybackSession::open(&source).expect("open the ramp");
        setup(&mut session);
        let out = dir.join(name);
        export_session(&session, &out, &settings);
        assert_eq!(pix_fmt(&out), "yuv420p10le", "{name} is 10-bit");
        decoded(&out, "yuv420p10le")
    };
    let key = |at, brightness| engine::color::ColorKey {
        at,
        params: bright(brightness),
        ease: engine::transform::KeyEase::Linear,
    };
    let keyed = export("keyed.mkv", &|s| {
        assert!(s.set_color_keys(Lane::V1, 0, vec![key(0, -0.05), key(FPS - 1, 0.05)]));
    });
    let low = export("low.mkv", &|s| assert!(s.set_color(Lane::V1, 0, Some(bright(-0.05)))));
    let high = export("high.mkv", &|s| assert!(s.set_color(Lane::V1, 0, Some(bright(0.05)))));
    let last = (FPS - 1) as usize;
    let diff = |a: Vec<u16>, b: Vec<u16>| -> u64 {
        a.iter().zip(&b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum()
    };
    let (d0, d1) = (
        diff(luma_of(&keyed, 0), luma_of(&low, 0)),
        diff(luma_of(&keyed, last), luma_of(&high, last)),
    );
    let mean_first = luma_of(&keyed, 0).iter().map(|&v| f64::from(v)).sum::<f64>();
    let mean_last = luma_of(&keyed, last).iter().map(|&v| f64::from(v)).sum::<f64>();
    eprintln!("keyed vs constant: first {d0}, last {d1}; sums {mean_first} {mean_last}");
    assert!(mean_last > mean_first, "the grade moved");
    let samples = (WIDTH * HEIGHT) as u64;
    // An 8-bit pass is off by about a code a sample (1.1 here); the 10-bit grade by a quarter.
    assert!(d0 < samples / 2 && d1 < samples / 2, "off the 10-bit grade: {d0}, {d1}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bt2020_sdr_ten_bit_source_exports_with_its_own_colour() {
    let (Some(encoder), true, true) = (source_encoder(), have("ffmpeg"), have("ffprobe")) else {
        eprintln!("no ffmpeg with a source encoder: skipping the colour test");
        return;
    };
    let dir = scratch("ten-bit-colour");
    let source = dir.join("bt2020.mkv");
    let tags = ["-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "bt709"];
    assert!(make_ramp_source(&source, encoder, &tags), "ffmpeg makes the BT.2020 source");
    assert_eq!(probe(&source, "color_primaries"), "bt2020", "the source is BT.2020");
    assert_eq!(probe(&source, "color_space"), "bt2020nc", "the source matrix is BT.2020");

    for (format, ext) in [
        (Format::Av1, "mkv"),
        (Format::Av1Mp4, "mp4"),
        (Format::Hevc, "mkv"),
        (Format::HevcMp4, "mp4"),
    ] {
        let ext = format!("{format:?}.{ext}");
        let ten = dir.join(format!("ten.{ext}"));
        export_as(&source, &ten, format, None);
        assert_eq!(pix_fmt(&ten), "yuv420p10le", "{ext}: the export is 10-bit");
        assert_eq!(probe(&ten, "color_primaries"), "bt2020", "{ext}: primaries follow the source");
        assert_eq!(probe(&ten, "color_space"), "bt2020nc", "{ext}: matrix follows the source");

        // An explicit 8-bit export of the same source keeps the output rule.
        let eight = dir.join(format!("eight.{ext}"));
        export_as(&source, &eight, format, Some(8));
        assert_ne!(probe(&eight, "color_primaries"), "bt2020", "{ext}: 8-bit keeps the output rule");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
