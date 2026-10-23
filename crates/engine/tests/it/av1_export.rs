//! Exporting a timeline whose picture is an AV1 *source*, on a machine with no
//! VA-API plugin: the software seat (`ec-av1` on a thread of its own, behind
//! the export's `Pictures`) has to start mid-GOP at any in point, deliver every
//! picture in order and exactly, and stop on a cancel without leaving a file.
//!
//! The sources are made here with `ffmpeg` (libaom or libsvtav1, whichever the
//! host has; the test says so and passes vacuously when neither is there, as
//! the other generated-fixture tests do). Synthetic only.

use crate::seat;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::export::{EncoderSeat, ExportSettings, Format};
use engine::project::{Clip, Lane};
use engine::scratch::Scratch;
use engine::{DecodeSession, PlaybackSession};

const W: u32 = 256;
const H: u32 = 144;
const FPS: u32 = 30;
const FRAMES: u32 = 60;
/// Keyframes every 20 pictures: 27 is mid-GOP, 7 frames after the keyframe at 20.
const GOP: u32 = 20;
const IN_POINT: u32 = 27;
const KEPT: u32 = 24;

fn ffmpeg_ok(args: &[String]) -> bool {
    Command::new("ffmpeg")
        .args(["-y", "-v", "error"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

fn encoder() -> Option<&'static str> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    ["libaom-av1", "libsvtav1"]
        .into_iter()
        .find(|e| text.contains(e))
}

/// A synthetic AV1 clip with a sine tone, `ext` being `mp4` or `mkv`, 8 or 10 bit.
fn make_av1(dir: &Path, name: &str, ext: &str, ten_bit: bool, hz: u32) -> Option<PathBuf> {
    let enc = encoder()?;
    let out = dir.join(format!("{name}.{ext}"));
    let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
    let mut args: Vec<String> = [
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size={W}x{H}:rate={FPS}:duration={}", FRAMES / FPS),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency={hz}:sample_rate=48000:duration={}", FRAMES / FPS),
        "-c:v",
        enc,
        "-pix_fmt",
        pix,
        "-g",
        &GOP.to_string(),
        "-keyint_min",
        &GOP.to_string(),
        "-c:a",
        "aac",
        "-shortest",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match enc {
        "libaom-av1" => args.extend(
            ["-cpu-used", "8", "-crf", "20", "-b:v", "0", "-lag-in-frames", "0"]
                .map(String::from),
        ),
        _ => args.extend(["-preset", "10", "-crf", "20"].map(String::from)),
    }
    args.push(out.display().to_string());
    ffmpeg_ok(&args).then_some(out)
}

/// Keeps `len` frames from source frame `from` at the head of the timeline.
fn keep_from(session: &mut PlaybackSession, from: u32, len: u32) {
    let fps = f64::from(FPS);
    assert!(session.cut_at(f64::from(from) / fps), "cut at the in point");
    assert!(session.delete_clip(Lane::V1, 0), "drop the head");
    assert!(session.cut_at(f64::from(len) / fps), "cut at the out point");
    assert!(session.delete_clip(Lane::V1, 1), "drop the tail");
    assert_eq!(session.lane_clips(Lane::V1).len(), 1);
}

fn settings() -> ExportSettings {
    ExportSettings {
        format: Format::Mp4,
        seat: EncoderSeat::Software,
        bitrate: Some(20_000_000),
        ..Default::default()
    }
}

fn wait(handle: &engine::ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < limit, "export did not finish in {limit:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sq = 0f64;
    for (p, q) in a.chunks(4).zip(b.chunks(4)) {
        for c in 0..3 {
            let d = f64::from(p[c]) - f64::from(q[c]);
            sq += d * d;
        }
    }
    let mse = sq / (a.len() / 4 * 3) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn source_frame(path: &Path, n: u32) -> Vec<u8> {
    let (_, rx, _c) = DecodeSession::open_range(path, n, n + 1).expect("open_range");
    let f = rx.into_iter().next().expect("a source frame");
    assert_eq!(f.index, n);
    f.bgra
}

/// Frame `n` of `path` out of ffmpeg itself, as BGRA: the independent reference.
fn ffmpeg_frame(path: &Path, n: u32) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("select=eq(n\\,{n})"),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "-",
        ])
        .output()
        .ok()?;
    (out.stdout.len() == (W * H * 4) as usize).then_some(out.stdout)
}

fn probe_duration(path: &Path, stream: &str) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries", "stream=duration"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn export_ok(session: &PlaybackSession, name: &str) -> Scratch {
    let out = Scratch::file(&format!("ve_av1x_{name}"), "mp4");
    let handle = session.export_to_with(&out, &settings());
    wait(&handle, Duration::from_secs(600)).expect("the AV1 source exports with no plugin");
    assert!(out.exists(), "the export wrote its file");
    out
}

/// One clip, mid-GOP in point: frame count, first picture, and sound sync.
fn one_clip(ext: &str, ten_bit: bool) {
    let _seat = seat::software();
    let dir = Scratch::dir("ve_av1x_src");
    let name = format!("src_{ext}_{}", if ten_bit { 10 } else { 8 });
    let Some(src) = make_av1(&dir, &name, ext, ten_bit, 440) else {
        eprintln!("no AV1 encoder in ffmpeg here: skipped");
        return;
    };
    assert_eq!(Demuxer::open(&src).expect("demux").0.codec, Codec::Av1);
    let mut session = PlaybackSession::open(&src).expect("open the AV1 source");
    keep_from(&mut session, IN_POINT, KEPT);
    let clip = session.lane_clips(Lane::V1)[0].clone();
    assert_eq!((clip.start, clip.in_frame), (0, IN_POINT), "the in point is mid-GOP");

    let out = export_ok(&session, &name);
    let (meta, _) = Demuxer::open(&out).expect("reopen");
    assert_eq!(meta.frame_count, KEPT, "{name}: frame count");
    let (_, rx) = DecodeSession::open(&out).expect("decode export");
    let got: Vec<_> = rx.into_iter().collect();
    assert_eq!(got.len() as u32, KEPT, "{name}: frames decode back");

    // The export re-encodes (H.264 at the seat's own QP), which floors the PSNR at
    // ~39 dB whatever the source; the neighbour margin below is what says "exact".
    let want = source_frame(&src, IN_POINT);
    let p = psnr(&got[0].bgra, &want);
    assert!(p >= 38.0, "{name}: first picture vs source frame {IN_POINT}: {p:.1} dB");
    // Exact: nearer the in point than either neighbour.
    for off in [IN_POINT - 1, IN_POINT + 1] {
        let q = psnr(&got[0].bgra, &source_frame(&src, off));
        assert!(p > q + 3.0, "{name}: frame {IN_POINT} ({p:.1}) is not clearly nearer than {off} ({q:.1})");
    }
    // Frame-exact all the way: each exported picture is its own source frame.
    for k in [1u32, KEPT / 2, KEPT - 1] {
        let p = psnr(&got[k as usize].bgra, &source_frame(&src, IN_POINT + k));
        assert!(p >= 38.0, "{name}: frame +{k} drifted: {p:.1} dB");
    }
    // The independent reference: ffmpeg's own decode of the same frame.
    if let Some(reference) = ffmpeg_frame(&src, IN_POINT) {
        let p = psnr(&source_frame(&src, IN_POINT), &reference);
        assert!(p >= 30.0, "{name}: engine decode vs ffmpeg decode of frame {IN_POINT}: {p:.1} dB");
    }
    // A/V sync: the sound and the picture are the same length, from the same in point.
    let (v, a) = (probe_duration(&out, "v:0"), probe_duration(&out, "a:0"));
    if let (Some(v), Some(a)) = (v, a) {
        let want = f64::from(KEPT) / f64::from(FPS);
        assert!((v - want).abs() < 0.05, "{name}: picture {v:.3}s, want {want:.3}s");
        assert!((a - v).abs() < 0.06, "{name}: sound {a:.3}s vs picture {v:.3}s");
    }
    std::fs::remove_file(&out).ok();
}

#[test]
fn av1_mp4_8bit_exports_from_a_mid_gop_in_point_with_no_plugin() {
    one_clip("mp4", false);
}

#[test]
fn av1_mp4_10bit_exports_from_a_mid_gop_in_point_with_no_plugin() {
    one_clip("mp4", true);
}

#[test]
fn av1_mkv_8bit_exports_from_a_mid_gop_in_point_with_no_plugin() {
    one_clip("mkv", false);
}

#[test]
fn av1_mkv_10bit_exports_from_a_mid_gop_in_point_with_no_plugin() {
    one_clip("mkv", true);
}

/// Two AV1 clips back to back: each opens its own seat, in order.
#[test]
fn two_av1_clips_back_to_back_export_in_order() {
    let _seat = seat::software();
    let dir = Scratch::dir("ve_av1x_two");
    let (Some(a), Some(b)) = (
        make_av1(&dir, "a", "mkv", false, 440),
        make_av1(&dir, "b", "mp4", true, 880),
    ) else {
        eprintln!("no AV1 encoder in ffmpeg here: skipped");
        return;
    };
    let mut session = PlaybackSession::open(&a).expect("open a");
    keep_from(&mut session, IN_POINT, KEPT);
    session.import(&b).expect("import b");
    let second = Clip::source_range(1, 10, 10 + KEPT);
    assert!(session.place_at(Lane::V1, f64::from(KEPT) / f64::from(FPS), second), "place b after a");
    let clips = session.lane_clips(Lane::V1).to_vec();
    assert_eq!(clips.len(), 2, "both clips on the lane");
    let total: u32 = clips.iter().map(|c| c.out_frame - c.in_frame).sum();

    let out = export_ok(&session, "two");
    let (meta, _) = Demuxer::open(&out).expect("reopen");
    assert_eq!(meta.frame_count, total, "frame count is the sum of the clips");
    let (_, rx) = DecodeSession::open(&out).expect("decode export");
    let got: Vec<_> = rx.into_iter().collect();
    assert_eq!(got.len() as u32, total);
    let first = clips[0].out_frame - clips[0].in_frame;
    let p0 = psnr(&got[0].bgra, &source_frame(&a, IN_POINT));
    let p1 = psnr(&got[first as usize].bgra, &source_frame(&b, clips[1].in_frame));
    assert!(p0 >= 38.0, "first clip's first picture: {p0:.1} dB");
    assert!(p1 >= 38.0, "second clip's first picture: {p1:.1} dB");
    std::fs::remove_file(&out).ok();
}

/// A cancel with the AV1 seat decoding leaves neither the file nor its `.part`.
#[test]
fn cancelling_an_av1_source_export_leaves_no_file() {
    let _seat = seat::software();
    let dir = Scratch::dir("ve_av1x_cancel");
    let Some(src) = make_av1(&dir, "c", "mkv", false, 440) else {
        eprintln!("no AV1 encoder in ffmpeg here: skipped");
        return;
    };
    let session = PlaybackSession::open(&src).expect("open");
    let out = Scratch::file("ve_av1x_cancel_out", "mp4");
    let handle = session.export_to_with(&out, &settings());
    let started = Instant::now();
    while handle.progress() < 0.05 {
        assert!(started.elapsed() < Duration::from_secs(120), "export never got going");
        assert!(!handle.is_finished(), "export finished before it was cancelled");
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.cancel();
    let result = wait(&handle, Duration::from_secs(60));
    assert!(result.is_err(), "a cancelled export is an error");
    assert!(!out.exists(), "no partial file survives a cancel");
    assert!(
        !PathBuf::from(format!("{}.part", out.display())).exists(),
        "the .part is cleaned up too"
    );
}
