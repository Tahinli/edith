//! MS-MPEG4 version 2 and 3 (DivX 3), uncompressed video (BGR24, YUY2, I420, ...) and DV type-1
//! (`iavs`) in AVI, end to end: probe, every picture against ffmpeg's decode, mid-file seeks
//! against a sequential decode, sound, an export. All media is synthetic, made by ffmpeg into a
//! scratch directory at test time (the type-1 wrapper is built here from a `.dv` file, as
//! `ec-riff`'s tests do); without ffmpeg (or the encoder a case needs) a case says so and passes.
//! `VE_SW` is process-wide, so tests pin it through [`software`].
//!
//! Pictures are compared in BGRA against `ffmpeg -pix_fmt bgra` by PSNR (raw RGB goes through the
//! engine's YCbCr and 4:2:0 chroma, which costs a few dB against ffmpeg's untouched bytes). A seek
//! is compared exactly.
//!
//! ```text
//! cargo test -p engine --release --test oldavi
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::audio::AudioSession;
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
    let dir = std::env::temp_dir().join(format!("edith-dvmpeg4-{name}-{}", std::process::id()));
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

/// Row-to-row luma-ish difference of a BGRA frame: high for a combed (woven) picture.
fn comb(frame: &[u8], width: usize) -> f64 {
    let row = width * 4;
    let rows: Vec<&[u8]> = frame.chunks(row).collect();
    let total: u64 = rows.windows(2).map(|w| w[0].iter().zip(w[1]).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>()).sum();
    total as f64 / frame.len() as f64
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

/// Raw RGB takes the engine's YCbCr round trip and 4:2:0 chroma: lower than a YUV source.
const RAW_PSNR: f64 = 40.0;

const SINE: &str = "sine=frequency=440:sample_rate=48000:duration=2.4";
const CIF: &str = "testsrc2=size=352x288:rate=25:duration=2";
const PCM: &[&str] = &["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2"];

struct Case {
    name: &'static str,
    vargs: &'static [&'static str],
    codec: Codec,
    profile: &'static str,
    min_psnr: f64,
}

fn make(dir: &Path, case: &Case) -> PathBuf {
    let out = dir.join(format!("{}.avi", case.name));
    let mut args = vec!["-f", "lavfi", "-i", CIF, "-f", "lavfi", "-i", SINE, "-map", "0:v", "-map", "1:a"];
    args.extend(case.vargs);
    args.extend(PCM);
    args.extend(["-shortest", s(&out)]);
    ffmpeg(&args);
    out
}

const FRAMES: u32 = 50;

fn cases() -> Vec<Case> {
    let c = |name, vargs, codec, profile, min_psnr| Case { name, vargs, codec, profile, min_psnr };
    vec![
        c("msmpeg4v3", &["-c:v", "msmpeg4", "-vtag", "DIV3", "-qscale:v", "3", "-g", "12"], Codec::MsMpeg4, "v3", MIN_PSNR),
        c("msmpeg4v3-mp43", &["-c:v", "msmpeg4", "-vtag", "MP43", "-b:v", "400k", "-g", "25"], Codec::MsMpeg4, "v3", MIN_PSNR),
        c("msmpeg4v2", &["-c:v", "msmpeg4v2", "-qscale:v", "4", "-g", "12"], Codec::MsMpeg4, "v2", MIN_PSNR),
        c("raw-bgr24", &["-c:v", "rawvideo", "-pix_fmt", "bgr24"], Codec::Raw, "BGR24", RAW_PSNR),
        c("raw-bgra", &["-c:v", "rawvideo", "-pix_fmt", "bgra"], Codec::Raw, "BGRA", RAW_PSNR),
        c("raw-yuyv", &["-c:v", "rawvideo", "-pix_fmt", "yuyv422"], Codec::Raw, "YUY2", MIN_PSNR),
        c("raw-uyvy", &["-c:v", "rawvideo", "-pix_fmt", "uyvy422"], Codec::Raw, "UYVY", MIN_PSNR),
        c("raw-i420", &["-c:v", "rawvideo", "-pix_fmt", "yuv420p"], Codec::Raw, "I420", MIN_PSNR),
        c("raw-yv12", &["-c:v", "rawvideo", "-pix_fmt", "yuv420p", "-vtag", "YV12"], Codec::Raw, "YV12", MIN_PSNR),
    ]
}

/// Probe, decode and seek one case.
fn check(dir: &Path, case: &Case) {
    let name = case.name;
    let path = make(dir, case);

    // ---- probe
    let facts = engine::probe_media(&path);
    let video = facts.video.as_ref().unwrap_or_else(|| panic!("{name}: no picture"));
    eprintln!("{name}: {video:?}");
    assert_eq!(video.codec, case.codec, "{name}");
    assert!((video.frame_rate.unwrap() - 25.0).abs() < 0.01, "{name}: {:?}", video.frame_rate);
    assert_eq!(video.bit_depth, Some(8), "{name}");
    assert_eq!(video.profile, Some(case.profile), "{name}");
    let (meta, _) = Demuxer::open(&path).expect("open");
    assert_eq!(meta.frame_count, FRAMES, "{name}");
    assert_eq!(AudioSession::probe_streams(&path).expect("streams").len(), 1, "{name}");

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

    // ---- export
    let out = dir.join(format!("{name}.export.mp4"));
    software(|| export_mp4(&path, &out));
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), FRAMES, "{name}");
}

fn run(filter: impl Fn(&Case) -> bool, encoders: &[&str], tag: &str) {
    if !have(encoders) {
        return;
    }
    let dir = scratch(tag);
    for case in cases().iter().filter(|c| filter(c)) {
        check(&dir, case);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ms_mpeg4_v2_and_v3_open_decode_seek_and_export_from_avi() {
    run(|c| c.codec == Codec::MsMpeg4, &["msmpeg4", "msmpeg4v2", "pcm_s16le"], "msmpeg4");
}

#[test]
fn raw_video_opens_decodes_seeks_and_exports_from_avi() {
    run(|c| c.codec == Codec::Raw, &["rawvideo", "pcm_s16le"], "raw");
}

// ------------------------------------------------------------------ DV type-1

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() & 1 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut inner = kind.to_vec();
    inner.extend(body);
    let mut v = b"LIST".to_vec();
    v.extend((inner.len() as u32).to_le_bytes());
    v.extend(inner);
    v
}

/// Raw DV frames as a type-1 AVI (`iavs`, handler `dvsd`, one `00dc` chunk per frame).
fn type1_avi(dv: &[u8], frame_size: usize, scale: u32, rate: u32) -> Vec<u8> {
    let frames: Vec<&[u8]> = dv.chunks_exact(frame_size).collect();
    let mut strh = b"iavsdvsd".to_vec();
    strh.extend([0u8; 12]);
    strh.extend(scale.to_le_bytes());
    strh.extend(rate.to_le_bytes());
    strh.extend(0u32.to_le_bytes());
    strh.extend((frames.len() as u32).to_le_bytes());
    strh.extend([0u8; 20]);
    let strl = list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &[0u8; 32])].concat());
    let mut avih = vec![0u8; 56];
    avih[0..4].copy_from_slice(&(1_000_000u32 * scale / rate).to_le_bytes());
    avih[16..20].copy_from_slice(&(frames.len() as u32).to_le_bytes());
    avih[24..28].copy_from_slice(&1u32.to_le_bytes());
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), strl].concat());
    let (mut movi_body, mut idx1) = (Vec::new(), Vec::new());
    for f in &frames {
        idx1.extend(b"00dc");
        idx1.extend(0x10u32.to_le_bytes());
        idx1.extend((4 + movi_body.len() as u32).to_le_bytes());
        idx1.extend((f.len() as u32).to_le_bytes());
        movi_body.extend(chunk(b"00dc", f));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(hdrl);
    body.extend(list(b"movi", &movi_body));
    body.extend(chunk(b"idx1", &idx1));
    let mut out = b"RIFF".to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

fn sound(path: &Path) -> Vec<f32> {
    let (meta, rx) = AudioSession::open_multi_streams(&[(path.to_path_buf(), 0)], &[(Some(0), 0.0, f64::INFINITY)])
        .expect("open")
        .expect("sound");
    assert_eq!(meta.sample_rate, 48_000);
    let mut got = Vec::new();
    for chunk in rx {
        got.extend_from_slice(&chunk.samples);
    }
    got
}

/// DV type-1: the picture and the sound `ec-riff` splits out of each frame, against the same frames
/// as a raw `.dv` (picture exact, sound exact) and, for the weave, ffmpeg's decode.
fn type1_case(dir: &Path, tag: &str, size: &str, rate: &str, scale_rate: (u32, u32), frame_size: usize, pix: &str) {
    let dv = dir.join(format!("{tag}.dv"));
    let video = format!("testsrc2=size={size}:rate={rate}:duration=1.2");
    ffmpeg(&[
        "-f", "lavfi", "-i", &video, "-f", "lavfi", "-i", SINE, "-map", "0:v", "-map", "1:a", "-c:v", "dvvideo", "-pix_fmt", pix,
        "-ac", "2", "-c:a", "pcm_s16le", "-shortest", "-f", "dv", s(&dv),
    ]);
    let avi = dir.join(format!("{tag}.avi"));
    let bytes = std::fs::read(&dv).unwrap();
    let frames = (bytes.len() / frame_size) as u32;
    assert!(frames >= 28, "{tag}: {frames} frames");
    std::fs::write(&avi, type1_avi(&bytes, frame_size, scale_rate.0, scale_rate.1)).unwrap();

    let facts = engine::probe_media(&avi);
    let v = facts.video.as_ref().unwrap_or_else(|| panic!("{tag}: no picture"));
    assert_eq!(v.codec, Codec::Dv, "{tag}");
    assert_eq!(v.profile, Some("DV25"), "{tag}");
    let (meta, _) = Demuxer::open(&avi).expect("open");
    assert_eq!(meta.frame_count, frames, "{tag}");
    assert_eq!(AudioSession::probe_streams(&avi).expect("streams").len(), 1, "{tag}");

    let all = session_frames(&avi);
    assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..frames).collect::<Vec<_>>(), "{tag}");
    let raw = session_frames(&dv);
    for (a, b) in all.iter().zip(&raw) {
        assert!(a.bgra == b.bgra, "{tag}: frame {} differs from the same frames as a raw .dv", a.index);
    }
    let want = ffmpeg_bgra(&avi, all[0].width, all[0].height);
    let size = all[0].bgra.len();
    assert_eq!(want.len(), size * all.len(), "{tag}: frame count or size differs from ffmpeg's");
    // Woven by the camera, deinterlaced by us: less row-to-row energy than ffmpeg's woven decode.
    let w = all[0].width as usize;
    let (mut woven, mut ours) = (0.0, 0.0);
    for n in 3..(frames as usize - 3) {
        woven += comb(&want[n * size..(n + 1) * size], w);
        ours += comb(&all[n].bgra, w);
    }
    assert!(ours <= woven, "{tag}: the interlaced picture is not deinterlaced ({ours:.1} vs {woven:.1})");

    // The sound is the DIF audio blocks, the same samples a raw .dv gives.
    let (ours, theirs) = (sound(&avi), sound(&dv));
    let peak = ours.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(ours.len() / 2 >= 48_000 * 9 / 10, "{tag}: {} frames of sound", ours.len() / 2);
    assert!(peak > 0.05, "{tag}: silent");
    let n = ours.len().min(theirs.len());
    assert!(ours[..n] == theirs[..n], "{tag}: the sound differs from the raw .dv's");

    for target in [0, 1, frames / 2, frames - 1] {
        let seeked = software(|| {
            let (_, rx, _c) = DecodeSession::open_at(&avi, target).expect("open_at");
            rx.into_iter().next().unwrap_or_else(|| panic!("{tag}: no frame after seek to {target}"))
        });
        assert_eq!(seeked.index, target, "{tag}");
        assert!(seeked.bgra == all[target as usize].bgra, "{tag}: seek to {target} differs from sequential");
    }
    let out = dir.join(format!("{tag}.export.mp4"));
    software(|| export_mp4(&avi, &out));
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), frames, "{tag}");
}

#[test]
fn dv_type1_avi_opens_plays_seeks_and_exports_with_its_sound() {
    if !have(&["dvvideo", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("iavs");
    type1_case(&dir, "pal", "720x576", "25", (1, 25), 144_000, "yuv420p");
    type1_case(&dir, "ntsc", "720x480", "30000/1001", (1001, 30000), 120_000, "yuv411p");
    let _ = std::fs::remove_dir_all(&dir);
}
