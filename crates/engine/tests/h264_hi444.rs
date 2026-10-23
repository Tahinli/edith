//! H.264 High 4:4:4 Predictive (8 and 10 bit, also lossless) and monochrome
//! (8 and 10 bit) end to end, in mp4 and Matroska: what the container probe
//! reports, that the pictures edith shows are ffmpeg's decode brought to the
//! engine's 4:2:0 working format, that a software seek lands on the bytes of a
//! sequential decode, that VA-API is never offered these streams, and that an
//! export keeps the duration and every frame.
//!
//! Chroma path: 4:4:4 chroma is averaged 2x2 down to 4:2:0 (`(a + b + 1) >> 1`
//! per axis, rows first), 10-bit then drops its two low bits, exactly as
//! `Deep::load` does; monochrome arrives as 4:2:0 with neutral chroma. The test
//! rebuilds that planar picture from ffmpeg's native decode (`yuv444p`,
//! `yuv444p10le`, `gray`, `gray10le`), lets ffmpeg convert it to BGRA, and holds
//! the engine's frames to it by PSNR: the two share the 4:2:0 samples and differ
//! only in the YUV->RGB rounding and the chroma upsampling filter, so the
//! tolerance is [`MIN_PSNR`] (the measured figures are printed with `--nocapture`).
//!
//! The clips are synthetic (`testsrc2`) and encoded by ffmpeg's libx264 into the
//! temp dir; without ffmpeg and libx264 each test says so and passes.
//!
//! `VE_SW` is process-wide, so every test that pins it holds [`SW`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::colorspace::Chroma;
use engine::decode::{Backend, DecodeSession, probe};
use engine::demux::{Codec, Demuxer};
use engine::export::{ExportSettings, Format};

const W: usize = 352;
const H: usize = 288;
const FRAMES: u32 = 30;
/// Against ffmpeg's own conversion of the same 4:2:0 samples. Rounding and the
/// chroma upsampling filter are the only differences (measured: 48.2 dB at worst
/// for 4:4:4, bit exact for monochrome).
const MIN_PSNR: f64 = 45.0;

static SW: Mutex<()> = Mutex::new(());

fn software<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    engine::envcfg::set("VE_SW", "1");
    let out = f();
    engine::envcfg::unset("VE_SW");
    out
}

fn have_x264() -> bool {
    let tool = |t: &str, a: &str| Command::new(t).arg(a).output().is_ok_and(|o| o.status.success());
    if !tool("ffmpeg", "-version") || !tool("ffprobe", "-version") {
        return false;
    }
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libx264"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// One shape under test.
struct Shape {
    label: &'static str,
    pix_fmt: &'static str,
    x264_args: &'static [&'static str],
    chroma: Chroma,
    depth: u8,
    profile: &'static str,
    /// The ffmpeg decode format that carries every sample.
    native: &'static str,
}

const SHAPES: [Shape; 6] = [
    Shape { label: "444-8", pix_fmt: "yuv444p", x264_args: &[], chroma: Chroma::C444, depth: 8, profile: "High 4:4:4 Predictive", native: "yuv444p" },
    Shape { label: "444-10", pix_fmt: "yuv444p10le", x264_args: &[], chroma: Chroma::C444, depth: 10, profile: "High 4:4:4 Predictive", native: "yuv444p10le" },
    Shape { label: "444-8-lossless", pix_fmt: "yuv444p", x264_args: &["-qp", "0"], chroma: Chroma::C444, depth: 8, profile: "High 4:4:4 Predictive", native: "yuv444p" },
    Shape { label: "444-10-lossless", pix_fmt: "yuv444p10le", x264_args: &["-qp", "0"], chroma: Chroma::C444, depth: 10, profile: "High 4:4:4 Predictive", native: "yuv444p10le" },
    Shape { label: "gray-8", pix_fmt: "gray", x264_args: &[], chroma: Chroma::Gray, depth: 8, profile: "High", native: "gray" },
    Shape { label: "gray-10", pix_fmt: "gray10le", x264_args: &[], chroma: Chroma::Gray, depth: 10, profile: "High 10", native: "gray10le" },
];

/// A 30-frame libx264 clip with B pictures and a sync point every 10 pictures,
/// so a seek lands mid-GOP.
fn make(dir: &Path, ext: &str, shape: &Shape) -> PathBuf {
    let path = dir.join(format!("{}.{ext}", shape.label));
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={W}x{H}:rate=30"))
        .args(["-frames:v", "30", "-c:v", "libx264"])
        .args(if shape.chroma == Chroma::Gray { &[][..] } else { &["-profile:v", "high444"][..] })
        .args(["-pix_fmt", shape.pix_fmt])
        .args(["-x264-params", "keyint=10:min-keyint=10:bframes=2"])
        .args(shape.x264_args)
        .arg(&path)
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    path
}

fn ffprobe_field(path: &Path, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn ffmpeg_raw(path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// ffmpeg's `yuv444p`/`yuv444p10le`/`gray`/`gray10le` picture as the engine's
/// 8-bit 4:2:0 planes: chroma averaged `(a + b + 1) >> 1` down rows, then down
/// columns, and 10-bit samples `>> 2`; gray gets neutral chroma.
fn working_420(native: &[u8], shape: &Shape) -> Vec<u8> {
    let bytes = if shape.depth > 8 { 2 } else { 1 };
    let shift = if shape.depth > 8 { 2 } else { 0 };
    let plane_len = W * H * bytes;
    let sample = |plane: &[u8], i: usize| -> u32 {
        if bytes == 2 { u32::from(u16::from_le_bytes([plane[2 * i], plane[2 * i + 1]])) } else { u32::from(plane[i]) }
    };
    let (cw, ch) = (W / 2, H / 2);
    let frame_len = if shape.chroma == Chroma::Gray { plane_len } else { 3 * plane_len };
    let mut out = Vec::new();
    for frame in native.chunks(frame_len) {
        out.extend((0..W * H).map(|i| (sample(frame, i) >> shift) as u8));
        for p in 1..3 {
            if shape.chroma == Chroma::Gray {
                out.extend(std::iter::repeat_n(128u8, cw * ch));
                continue;
            }
            let plane = &frame[p * plane_len..(p + 1) * plane_len];
            for r in 0..ch {
                for x in 0..cw {
                    let at = |row: usize| (sample(plane, row * W + 2 * x) + sample(plane, row * W + 2 * x + 1) + 1) >> 1;
                    out.push((((at(2 * r) + at(2 * r + 1) + 1) >> 1) >> shift) as u8);
                }
            }
        }
    }
    out
}

/// ffmpeg's BGRA rendering of 8-bit planar 4:2:0 `yuv`, BT.601, in the `range`
/// the stream states (`tv` unless its VUI says `pc`: ffmpeg tags a gray encode
/// full range, and the engine honours the flag).
fn ffmpeg_bgra(dir: &Path, yuv: &[u8], range: &str) -> Vec<u8> {
    let raw = dir.join("ref.yuv");
    std::fs::write(&raw, yuv).expect("write reference");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s"])
        .arg(format!("{W}x{H}"))
        .arg("-i")
        .arg(&raw)
        .args(["-vf", &format!("scale=in_color_matrix=bt601:in_range={range}"), "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn psnr_bgra(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let (mut squared, mut n) = (0f64, 0f64);
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for c in 0..3 {
            let d = f64::from(x[c]) - f64::from(y[c]);
            squared += d * d;
            n += 1.0;
        }
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 * n / squared).log10()
}

fn clips(dir: &Path) -> Vec<(PathBuf, &'static Shape, String)> {
    let mut v = Vec::new();
    for shape in &SHAPES {
        for ext in ["mp4", "mkv"] {
            v.push((make(dir, ext, shape), shape, format!("{} {ext}", shape.label)));
        }
    }
    v
}

#[test]
fn hi444_and_monochrome_open_and_probe() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("hi444-probe");
    for (clip, shape, what) in clips(&dir) {
        let (meta, demuxer) = Demuxer::open(&clip).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(meta.codec, Codec::H264, "{what}");
        assert_eq!((meta.width as usize, meta.height as usize), (W, H), "{what}");
        assert_eq!(meta.frame_count, FRAMES, "{what}");
        assert_eq!(demuxer.bit_depth(), shape.depth, "{what}: depth off the SPS");
        assert_eq!(demuxer.chroma(), Some(shape.chroma), "{what}: chroma format");
        assert_eq!(demuxer.profile(), Some(shape.profile), "{what}");
        let facts = engine::demux::probe_media(&clip).video.expect("video facts");
        assert_eq!(facts.profile, Some(shape.profile), "{what}: media facts profile");
        // Whatever the plugin could do, these are never offered to VA-API.
        let (codec, backend) = probe(&clip).expect("probe");
        assert_eq!((codec, backend), (Some(Codec::H264), Backend::Software), "{what}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pictures_are_ffmpegs_decode_averaged_to_the_working_format() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("hi444-pixels");
    for (clip, shape, what) in clips(&dir) {
        let range = if ffprobe_field(&clip, "stream=color_range") == "pc" { "pc" } else { "tv" };
        let reference = ffmpeg_bgra(&dir, &working_420(&ffmpeg_raw(&clip, shape.native), shape), range);
        let (_, rx) = software(|| DecodeSession::open(&clip).expect("open"));
        let frames: Vec<_> = rx.into_iter().collect();
        assert_eq!(
            frames.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..FRAMES).collect::<Vec<_>>(),
            "{what}: display order"
        );
        let frame_bytes = W * H * 4;
        assert_eq!(reference.len(), frame_bytes * FRAMES as usize, "{what}");
        let mut worst = f64::INFINITY;
        for (frame, want) in frames.iter().zip(reference.chunks(frame_bytes)) {
            assert_eq!(frame.bgra.len(), frame_bytes, "{what}");
            worst = worst.min(psnr_bgra(&frame.bgra, want));
        }
        eprintln!("h264 {what}: worst frame {worst:.2} dB against ffmpeg");
        assert!(worst >= MIN_PSNR, "{what}: worst frame {worst:.2} dB < {MIN_PSNR}");
        let first = &frames[0].bgra;
        assert!(first.as_chunks::<4>().0.iter().any(|px| px[..] != first[..4]), "{what}: frame 0 is a single colour");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn software_seeks_land_on_the_bytes_of_a_sequential_decode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("hi444-seek");
    for (clip, _, what) in clips(&dir) {
        let all = software(|| DecodeSession::open(&clip).expect("open").1.into_iter().collect::<Vec<_>>());
        for target in [1, 5, 9, 10, 13, 17, 25, FRAMES - 1] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{what}: no frame after seek to {target}"))
            });
            assert_eq!(seeked.index, target, "{what}");
            assert!(seeked.bgra == all[target as usize].bgra, "{what}: seek to {target} differs");
        }
        // Without VE_SW pinned the choice is the engine's own, and it must reach
        // the same pictures (the GPU is never asked, so a host with one agrees).
        let free = DecodeSession::open_at(&clip, 13).expect("open_at").1.into_iter().next().expect("a frame");
        assert!(free.bgra == all[13].bgra, "{what}: default route differs from software");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn export_as(source: &Path, out: &Path, format: Format) {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let settings = ExportSettings { format, ..Default::default() };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

#[test]
fn exports_keep_every_frame_and_the_duration() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("hi444-export");
    for shape in &SHAPES {
        for ext in ["mp4", "mkv"] {
            let source = make(&dir, ext, shape);
            for (format, out_ext) in [(Format::HevcMp4, "mp4"), (Format::Hevc, "mkv")] {
                let what = format!("{} {ext}->{out_ext}", shape.label);
                let out = dir.join(format!("out-{}-{ext}.{out_ext}", shape.label));
                export_as(&source, &out, format);
                let raw = ffmpeg_raw(&out, "yuv420p");
                assert_eq!(raw.len(), W * H * 3 / 2 * FRAMES as usize, "{what}: every frame");
                let secs: f64 = ffprobe_field(&out, "format=duration").parse().expect("a duration");
                assert!((secs - 1.0).abs() < 0.1, "{what}: {secs} s, want 1 s");
                let _ = std::fs::remove_file(&out);
            }
            let _ = std::fs::remove_file(&source);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
