//! H.264 High 10 (4:2:0 progressive, 10-bit) end to end: what the container
//! probe reports, that `ec-h264`'s I010 pictures are ffmpeg's `yuv420p10le`
//! byte for byte, that a software seek lands on the bytes a linear decode gave,
//! and that a 10-bit export of such a source stays 10-bit. (4:4:4 and
//! monochrome live in `h264_hi444.rs`.)
//!
//! The clips are synthetic (`testsrc2`) and encoded by ffmpeg's libx264 into the
//! temp dir; without ffmpeg and libx264 each test says so and passes.
//!
//! `VE_SW` is process-wide, so every test that pins it holds [`SW`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase};
use ec_h264::H264Decoder;
use engine::PlaybackSession;
use engine::decode::{Backend, DecodeSession, probe};
use engine::demux::{Codec, Demuxer};
use engine::export::{ExportSettings, Format};

const W: usize = 352;
const H: usize = 288;
const FRAMES: u32 = 30;

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

/// A 30-frame libx264 clip of `pix_fmt` at `profile`, with B pictures and a sync
/// point every 10 pictures, so a seek lands mid-GOP.
fn make(dir: &Path, name: &str, source: &str, profile: &str, pix_fmt: &str) -> PathBuf {
    let path = dir.join(name);
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("{source}=size={W}x{H}:rate=30"))
        .args(["-frames:v", "30", "-c:v", "libx264", "-profile:v", profile])
        .args(["-pix_fmt", pix_fmt, "-x264-params", "keyint=10:min-keyint=10:bframes=2"])
        .arg(&path)
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    path
}

fn ffprobe_field(path: &Path, field: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(format!("stream={field}"))
        .args(["-of", "csv=p=0"])
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

fn clips(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    vec![
        (make(dir, "t.mp4", "testsrc2", "high10", "yuv420p10le"), "testsrc2 mp4"),
        (make(dir, "t.mkv", "testsrc2", "high10", "yuv420p10le"), "testsrc2 mkv"),
        (make(dir, "m.mp4", "mandelbrot", "high10", "yuv420p10le"), "mandelbrot mp4"),
        (make(dir, "m.mkv", "mandelbrot", "high10", "yuv420p10le"), "mandelbrot mkv"),
    ]
}

#[test]
fn high_10_opens_and_probes_at_ten_bits() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("h264hi10-probe");
    for (clip, what) in clips(&dir) {
        let (meta, demuxer) = Demuxer::open(&clip).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(meta.codec, Codec::H264, "{what}");
        assert_eq!((meta.width as usize, meta.height as usize), (W, H), "{what}");
        assert_eq!(meta.frame_count, FRAMES, "{what}");
        assert_eq!(demuxer.bit_depth(), 10, "{what}: bit depth off the SPS");
        assert_eq!(demuxer.profile(), Some("High 10"), "{what}");
        // The GPU has no High 10 profile: the probe names the software seat even
        // with the plugin free to take it.
        let (codec, backend) = probe(&clip).expect("probe");
        assert_eq!((codec, backend), (Some(Codec::H264), Backend::Software), "{what}");
        let facts = engine::demux::probe_media(&clip).video.map(|f| f.bit_depth);
        assert_eq!(facts, Some(Some(10)), "{what}: the media facts say 10 bits");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ec_h264_pictures_are_ffmpegs_yuv420p10le_exactly() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("h264hi10-exact");
    for (clip, what) in clips(&dir) {
        let want = ffmpeg_raw(&clip, "yuv420p10le");
        let (_, mut demuxer) = Demuxer::open(&clip).expect("open");
        demuxer.seek_to_sync_at_or_before(0);
        let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder");
        let mut got = Vec::new();
        let take = |decoder: &mut H264Decoder, got: &mut Vec<u8>| {
            while let Ok(frame) = decoder.receive_frame() {
                let ec_core::frame::Frame::Video(pic) = frame else { continue };
                assert_eq!(pic.format, ec_core::frame::PixelFormat::I010, "{what}");
                for plane in &pic.planes {
                    got.extend_from_slice(&plane.data);
                }
            }
        };
        while let Some(au) = demuxer.next_access_unit().expect("demux") {
            decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).expect("decode");
            take(&mut decoder, &mut got);
        }
        decoder.flush().expect("flush");
        take(&mut decoder, &mut got);
        assert_eq!(got.len(), want.len(), "{what}: every picture");
        assert!(got == want, "{what}: pictures differ from ffmpeg's yuv420p10le");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn software_seeks_land_on_the_bytes_of_a_sequential_decode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("h264hi10-seek");
    for (clip, what) in clips(&dir) {
        let (meta, all) = software(|| {
            let (meta, rx) = DecodeSession::open(&clip).expect("open");
            (meta, rx.into_iter().collect::<Vec<_>>())
        });
        assert_eq!(meta.codec, Codec::H264, "{what}");
        assert_eq!(
            all.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..FRAMES).collect::<Vec<_>>(),
            "{what}: display order"
        );
        let first = &all[0].bgra;
        assert!(
            first.as_chunks::<4>().0.iter().any(|px| px[..] != first[..4]),
            "{what}: frame 0 is a single colour"
        );
        for target in [1, 5, 9, 10, 13, 17, 25, FRAMES - 1] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{what}: no frame after seek to {target}"))
            });
            assert_eq!(seeked.index, target, "{what}");
            assert!(seeked.bgra == all[target as usize].bgra, "{what}: seek to {target} differs");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
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

fn export_as(source: &Path, out: &Path, format: Format, depth: Option<u8>) {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let settings = ExportSettings { format, video_bit_depth: depth, ..Default::default() };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

#[test]
fn a_high_10_source_exports_ten_bit() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("h264hi10-export");
    let frame_bytes = W * H * 3;
    for (src, source_ext) in [("t.mp4", "mp4"), ("t.mkv", "mkv")] {
        let source = make(&dir, src, "testsrc2", "high10", "yuv420p10le");
        let reference = ffmpeg_raw(&source, "yuv420p10le");
        for (format, ext) in [(Format::Hevc, "mkv"), (Format::HevcMp4, "mp4")] {
            let ten = dir.join(format!("ten-{source_ext}.{ext}"));
            export_as(&source, &ten, format, Some(10));
            assert_eq!(ffprobe_field(&ten, "pix_fmt"), "yuv420p10le", "{source_ext}->{ext}: 10-bit");
            let ten_decoded = ffmpeg_raw(&ten, "yuv420p10le");
            assert_eq!(ten_decoded.len(), reference.len(), "{source_ext}->{ext}: every picture");
            assert_eq!(ten_decoded.len() / frame_bytes, FRAMES as usize);
            let psnr_ten = psnr_10(&reference, &ten_decoded);

            let eight = dir.join(format!("eight-{source_ext}.{ext}"));
            export_as(&source, &eight, format, Some(8));
            assert_eq!(ffprobe_field(&eight, "pix_fmt"), "yuv420p", "{source_ext}->{ext}: 8-bit");
            let psnr_eight = psnr_10(&reference, &ffmpeg_raw(&eight, "yuv420p10le"));
            eprintln!("h264 high10 {source_ext}->{ext}: 10-bit {psnr_ten:.2} dB, 8-bit {psnr_eight:.2} dB");
            assert!(psnr_ten > 30.0, "{source_ext}->{ext}: {psnr_ten:.2} dB against the source");
            assert!(psnr_ten > psnr_eight, "{source_ext}->{ext}: 10-bit {psnr_ten:.2} <= 8-bit {psnr_eight:.2}");

            // Unset: the source's own depth decides.
            let auto = dir.join(format!("auto-{source_ext}.{ext}"));
            export_as(&source, &auto, format, None);
            assert_eq!(ffprobe_field(&auto, "pix_fmt"), "yuv420p10le", "{source_ext}->{ext}: default follows the source");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
