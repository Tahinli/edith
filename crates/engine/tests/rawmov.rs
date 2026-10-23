//! Uncompressed video in QuickTime mov (`raw ` RGB24 / ARGB, `2vuy`, `yuvs`, `v210`, `v410`,
//! `b16g`) and the AVI layouts added after the first raw seat (NV12, gray, RGB555, palette,
//! v210), end to end: probe, every picture against ffmpeg's decode, mid-file seeks against a
//! sequential decode, an export (a 10-bit source exports at 10 bits). All media is synthetic,
//! made by ffmpeg into a scratch directory at test time; without ffmpeg (or the encoder a case
//! needs) a case says so and passes.
//! `VE_SW` is process-wide, so tests pin it through [`software`].
//!
//! Pictures are compared in BGRA against `ffmpeg -pix_fmt bgra` by PSNR (raw RGB goes through the
//! engine's YCbCr and 4:2:0 chroma, which costs a few dB against ffmpeg's untouched bytes). A seek
//! is compared exactly.
//!
//! ```text
//! cargo test -p engine --release --test rawmov
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::decode::DecodeSession;
use engine::demux::{Codec, Demuxer};
use engine::export::{ExportSettings, Format};
use engine::PlaybackSession;

/// Floor for the BGRA PSNR against ffmpeg's own conversion (dB). Chroma averaging and the 8-bit
/// truncation cost a few dB on a sharp test card; measured 31 to 55 dB (4:4:4 lowest, MJPEG 4:2:0 highest); a wrong range or matrix lands around 24.
const MIN_PSNR: f64 = 28.0;

static SW: Mutex<()> = Mutex::new(());

fn software<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SW.lock().unwrap_or_else(|p| p.into_inner());
    engine::envcfg::set("VE_SW", "1");
    let out = f();
    engine::envcfg::unset("VE_SW");
    out
}

fn have(encoders: &[&str]) -> bool {
    let tool = |t: &str| Command::new(t).arg("-version").output().is_ok_and(|o| o.status.success());
    if !tool("ffmpeg") || !tool("ffprobe") {
        eprintln!("no ffmpeg: skipping");
        return false;
    }
    let list = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for e in encoders {
        if !list.split_whitespace().any(|w| w == *e) {
            eprintln!("ffmpeg has no {e} encoder: skipping");
            return false;
        }
    }
    true
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-rawmov-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn ffprobe(path: &Path, entries: &str, select: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", select, "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn session_frames(path: &Path) -> Vec<engine::decode::Frame> {
    software(|| DecodeSession::open(path).expect("open").1.into_iter().collect())
}

/// ffmpeg's decode of the picture, as BGRA at the file's size.
fn ffmpeg_bgra(path: &Path, width: u32, height: u32) -> Vec<u8> {
    // The engine reads an untagged picture as BT.601 up to 576 lines and BT.709 above.
    // A stream that states its matrix is converted by ffmpeg as stated, as the engine does; an
    // untagged one is BT.601 to ffmpeg's scaler, and the size rule's BT.709 above 576 lines here.
    // A stated pixel aspect (a raw .dv states 8:9) widens the engine's picture; ffmpeg's decode
    // is at the coded size and is scaled to the engine's.
    let vf = if width > 854 { format!("scale={width}:{height}:in_color_matrix=bt709:out_color_matrix=bt709") } else { format!("scale={width}:{height}") };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-vf", &vf, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// PSNR over the colour bytes of two BGRA frames.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let (mut sq, mut n) = (0f64, 0f64);
    for (p, q) in a.chunks(4).zip(b.chunks(4)) {
        for k in 0..3 {
            let d = f64::from(p[k]) - f64::from(q[k]);
            sq += d * d;
            n += 1.0;
        }
    }
    if sq == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 * n / sq).log10() }
}

fn export_mp4(source: &Path, out: &Path) {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let settings = ExportSettings { format: Format::Mp4, ..Default::default() };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

const FRAMES: u32 = 50;

struct Case {
    name: &'static str,
    ext: &'static str,
    size: (u32, u32),
    vargs: &'static [&'static str],
    profile: &'static str,
    bit_depth: u8,
    min_psnr: f64,
}

/// Raw RGB takes the engine's YCbCr round trip and 4:2:0 chroma: lower than a YUV source.
const RGB_PSNR: f64 = 38.0;
const CIF: (u32, u32) = (352, 288);
const ODD: (u32, u32) = (203, 151);

fn cases() -> Vec<Case> {
    let c = |name, ext, size, vargs, profile, bit_depth, min_psnr| Case { name, ext, size, vargs, profile, bit_depth, min_psnr };
    let raw = |p: &'static str| -> &'static [&'static str] {
        match p {
            "uyvy422" => &["-c:v", "rawvideo", "-pix_fmt", "uyvy422"],
            "yuyv422" => &["-c:v", "rawvideo", "-pix_fmt", "yuyv422"],
            "rgb24" => &["-c:v", "rawvideo", "-pix_fmt", "rgb24"],
            "argb" => &["-c:v", "rawvideo", "-pix_fmt", "argb"],
            "rgb555be" => &["-c:v", "rawvideo", "-pix_fmt", "rgb555be"],
            "gray16be" => &["-c:v", "rawvideo", "-pix_fmt", "gray16be"],
            "nv12" => &["-c:v", "rawvideo", "-pix_fmt", "nv12"],
            "gray" => &["-c:v", "rawvideo", "-pix_fmt", "gray"],
            "rgb555le" => &["-c:v", "rawvideo", "-pix_fmt", "rgb555le"],
            _ => &["-c:v", "rawvideo", "-pix_fmt", "pal8"],
        }
    };
    vec![
        c("mov-uyvy", "mov", CIF, raw("uyvy422"), "UYVY", 8, MIN_PSNR),
        c("mov-uyvy-odd", "mov", ODD, raw("uyvy422"), "UYVY", 8, MIN_PSNR),
        c("mov-yuvs", "mov", CIF, raw("yuyv422"), "YUY2", 8, MIN_PSNR),
        c("mov-rgb24", "mov", CIF, raw("rgb24"), "RGB24", 8, RGB_PSNR),
        c("mov-rgb24-odd", "mov", ODD, raw("rgb24"), "RGB24", 8, RGB_PSNR),
        c("mov-argb", "mov", CIF, raw("argb"), "ARGB", 8, RGB_PSNR),
        c("mov-rgb555", "mov", CIF, raw("rgb555be"), "RGB555", 8, 30.0),
        c("mov-v210", "mov", CIF, &["-c:v", "v210"], "v210", 10, MIN_PSNR),
        c("mov-v210-odd", "mov", ODD, &["-c:v", "v210"], "v210", 10, MIN_PSNR),
        c("mov-v410", "mov", CIF, &["-c:v", "v410"], "v410", 10, 28.0),
        c("mov-b16g", "mov", CIF, raw("gray16be"), "Gray16", 10, MIN_PSNR),
        c("avi-nv12", "avi", CIF, raw("nv12"), "NV12", 8, MIN_PSNR),
        c("avi-nv12-odd", "avi", ODD, raw("nv12"), "NV12", 8, MIN_PSNR),
        c("avi-gray", "avi", CIF, raw("gray"), "Y800", 8, MIN_PSNR),
        c("avi-rgb555", "avi", CIF, raw("rgb555le"), "RGB555", 8, 30.0),
        c("avi-pal8", "avi", CIF, raw("pal8"), "PAL8", 8, 28.0),
        c("avi-v210", "avi", CIF, &["-c:v", "v210"], "v210", 10, MIN_PSNR),
    ]
}

fn make(dir: &Path, case: &Case) -> PathBuf {
    let out = dir.join(format!("{}.{}", case.name, case.ext));
    let src = format!("testsrc2=size={}x{}:rate=25:duration=2", case.size.0, case.size.1);
    let mut args = vec!["-f", "lavfi", "-i", &src];
    args.extend(case.vargs);
    args.extend(["-an", s(&out)]);
    ffmpeg(&args);
    out
}

/// Probe, decode, seek and export one case.
fn check(dir: &Path, case: &Case) {
    let name = case.name;
    let path = make(dir, case);

    // ---- probe
    let facts = engine::probe_media(&path);
    let video = facts.video.as_ref().unwrap_or_else(|| panic!("{name}: no picture: {facts:?}"));
    eprintln!("{name}: {video:?}");
    assert_eq!(video.codec, Codec::Raw, "{name}");
    assert!((video.frame_rate.unwrap() - 25.0).abs() < 0.01, "{name}: {:?}", video.frame_rate);
    assert_eq!(video.bit_depth, Some(case.bit_depth), "{name}");
    assert_eq!(video.profile, Some(case.profile), "{name}");
    // The engine draws even sizes: an odd edge loses its last column or row.
    assert_eq!((video.width, video.height), (case.size.0 & !1, case.size.1 & !1), "{name}");
    let (meta, _) = Demuxer::open(&path).expect("open");
    assert_eq!(meta.frame_count, FRAMES, "{name}");

    // ---- pictures
    let all = session_frames(&path);
    assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..FRAMES).collect::<Vec<_>>(), "{name}");
    let want = ffmpeg_bgra(&path, all[0].width, all[0].height);
    let size = all[0].bgra.len();
    assert_eq!(want.len(), size * all.len(), "{name}: frame count or size differs from ffmpeg's");
    let mut worst = 99.0f64;
    for (i, f) in all.iter().enumerate() {
        worst = worst.min(psnr(&f.bgra, &want[i * size..(i + 1) * size]));
    }
    eprintln!("{name}: worst PSNR against ffmpeg {worst:.1} dB");
    assert!(worst >= case.min_psnr, "{name}: PSNR {worst:.1} dB");

    // ---- seeks: a frame after a seek is the frame a sequential decode gives
    for target in [0, 1, 13, FRAMES / 2, FRAMES - 2, FRAMES - 1] {
        let seeked = software(|| {
            let (_, rx, _c) = DecodeSession::open_at(&path, target).expect("open_at");
            rx.into_iter().next().unwrap_or_else(|| panic!("{name}: no frame after seek to {target}"))
        });
        assert_eq!(seeked.index, target, "{name}");
        assert!(seeked.bgra == all[target as usize].bgra, "{name}: seek to {target} differs from sequential");
    }
    let (a, b) = (FRAMES / 4, FRAMES / 4 + 9);
    let range = software(|| {
        let (_, rx, _c) = DecodeSession::open_range(&path, a, b).expect("open_range");
        rx.into_iter().collect::<Vec<_>>()
    });
    assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (a..b).collect::<Vec<_>>(), "{name}");
    for f in &range {
        assert!(f.bgra == all[f.index as usize].bgra, "{name}: range frame {} differs", f.index);
    }

    // ---- export (8-bit H.264 mp4)
    let out = dir.join(format!("{name}.export.mp4"));
    software(|| export_mp4(&path, &out));
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), FRAMES, "{name}");

    // ---- a 10-bit source exports at 10 bits where the format has the choice, and reads back
    // close to ffmpeg's decode of the source
    if case.bit_depth == 10 {
        let ten = dir.join(format!("{name}.ten.mp4"));
        software(|| export_with(&path, &ten, Format::Av1Mp4, None));
        assert_eq!(ffprobe(&ten, "stream=pix_fmt", "v:0"), "yuv420p10le", "{name}: a 10-bit source exports 10-bit");
        assert_eq!(ffprobe(&ten, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), FRAMES, "{name}");
        let back = ffmpeg_bgra(&ten, all[0].width, all[0].height);
        assert_eq!(back.len(), want.len(), "{name}: export size");
        let mid = (FRAMES / 2) as usize;
        let p = psnr(&back[mid * size..(mid + 1) * size], &want[mid * size..(mid + 1) * size]);
        eprintln!("{name}: 10-bit export against ffmpeg's decode of the source {p:.1} dB");
        assert!(p >= 24.0, "{name}: 10-bit export PSNR {p:.1} dB");
    }
}

fn export_with(source: &Path, out: &Path, format: Format, depth: Option<u8>) {
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

fn run(filter: impl Fn(&Case) -> bool, tag: &str) {
    if !have(&["rawvideo", "v210", "v410"]) {
        return;
    }
    let dir = scratch(tag);
    for case in cases().iter().filter(|c| filter(c)) {
        check(&dir, case);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn raw_video_in_mov_opens_decodes_seeks_and_exports() {
    run(|c| c.ext == "mov", "mov");
}

#[test]
fn newer_raw_layouts_in_avi_open_decode_seek_and_export() {
    run(|c| c.ext == "avi", "avi");
}

/// A layout `ec-riff` refuses (an 8-bit `raw ` palette in a mov, 1 bpp in an AVI) fails the open
/// by name, not as a black picture.
#[test]
fn raw_layouts_the_decoder_refuses_are_refused_by_name() {
    if !have(&["rawvideo"]) {
        return;
    }
    let dir = scratch("refused");
    for (name, ext, pix) in [("mov-pal8", "mov", "pal8"), ("avi-mono", "avi", "monow")] {
        let path = dir.join(format!("{name}.{ext}"));
        ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1", "-c:v", "rawvideo", "-pix_fmt", pix, "-an", s(&path)]);
        let err = Demuxer::open(&path).err().unwrap_or_else(|| panic!("{name}: opened")).to_string();
        eprintln!("{name}: {err}");
        assert!((err.contains("not supported") || err.contains("unsupported")) && err.contains("ffmpeg -i"), "{name}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
