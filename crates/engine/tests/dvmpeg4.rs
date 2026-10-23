//! DV (DV25 NTSC 4:1:1 and PAL 4:2:0, DVCPRO50, DVCPRO HD) and MPEG-4 Part 2 / H.263 end to end:
//! probe, every picture against ffmpeg's decode, mid-file seeks, sound, an export. Containers:
//! QuickTime/MP4/3GP, Matroska, AVI (DV type-2, XviD / DivX FourCCs) and raw `.dv`. All media is
//! synthetic, made by ffmpeg into a scratch directory at test time; without ffmpeg (or the
//! encoder a case needs) a case says so and passes, as `procodecs` does. `VE_SW` is
//! process-wide, so tests pin it through [`software`].
//!
//! Pictures are compared in BGRA against `ffmpeg -pix_fmt bgra` by PSNR (the engine averages
//! 4:2:2 chroma down to 4:2:0 before converting, which ffmpeg's scaler does differently; the
//! decoders are byte-exact against ffmpeg in their own crates). A seek is compared exactly.
//!
//! ```text
//! cargo test -p engine --release --test dvmpeg4
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

const SINE: &str = "sine=frequency=440:sample_rate=48000:duration=2.4";

struct Case {
    name: &'static str,
    video: &'static str,
    vargs: &'static [&'static str],
    /// Audio arguments; `None` for a silent clip.
    aargs: Option<&'static [&'static str]>,
    ext: &'static str,
    codec: Codec,
    profile: Option<&'static str>,
    frames: u32,
    /// Frames per second the probe must read.
    fps: f64,
    /// Interlace the probe states.
    interlaced: Option<&'static str>,
}

const NTSC: &str = "testsrc2=size=720x480:rate=30000/1001:duration=1.001";
const PAL: &str = "testsrc2=size=720x576:rate=25:duration=1.2";
const HD: &str = "testsrc2=size=1440x1080:rate=25:duration=1.2";
const CIF: &str = "testsrc2=size=352x288:rate=25:duration=2";
const PCM: &[&str] = &["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2"];
const AAC: &[&str] = &["-c:a", "aac", "-b:a", "128k"];
const SP: &[&str] = &["-c:v", "mpeg4", "-qscale:v", "3", "-g", "12", "-pix_fmt", "yuv420p"];
const ASP: &[&str] = &["-c:v", "mpeg4", "-qscale:v", "3", "-g", "12", "-bf", "2", "-flags", "+qpel+mv4", "-pix_fmt", "yuv420p"];

fn make(dir: &Path, case: &Case) -> PathBuf {
    let out = dir.join(format!("{}.{}", case.name, case.ext));
    let mut args = vec!["-f", "lavfi", "-i", case.video];
    if case.aargs.is_some() {
        args.extend(["-f", "lavfi", "-i", SINE]);
    }
    args.extend(["-map", "0:v"]);
    if case.aargs.is_some() {
        args.extend(["-map", "1:a"]);
    }
    args.extend(case.vargs);
    if let Some(a) = case.aargs {
        args.extend(a);
    }
    args.extend(["-shortest", s(&out)]);
    ffmpeg(&args);
    out
}

fn cases() -> Vec<Case> {
    let c = |name, video, vargs, aargs, ext, codec, profile, frames, fps, interlaced| Case {
        name, video, vargs, aargs, ext, codec, profile, frames, fps, interlaced,
    };
    let dv25 = Some("DV25");
    let ntsc = 30000.0 / 1001.0;
    vec![
        c("dv25-ntsc-mov", NTSC, &["-c:v", "dvvideo", "-pix_fmt", "yuv411p"], Some(PCM), "mov", Codec::Dv, dv25, 30, ntsc, Some("bff")),
        c("dv25-ntsc-avi", NTSC, &["-c:v", "dvvideo", "-pix_fmt", "yuv411p"], Some(PCM), "avi", Codec::Dv, dv25, 30, ntsc, Some("bff")),
        c("dv25-ntsc-raw", NTSC, &["-c:v", "dvvideo", "-pix_fmt", "yuv411p", "-f", "dv"], Some(PCM), "dv", Codec::Dv, dv25, 29, ntsc, Some("bff")),
        c("dv25-pal-mov", PAL, &["-c:v", "dvvideo", "-pix_fmt", "yuv420p"], Some(PCM), "mov", Codec::Dv, dv25, 30, 25.0, Some("bff")),
        c("dvcpro50-mov", PAL, &["-c:v", "dvvideo", "-pix_fmt", "yuv422p", "-b:v", "50M"], Some(PCM), "mov", Codec::Dv, Some("DVCPRO50"), 30, 25.0, Some("bff")),
        c("dvcprohd-mov", HD, &["-c:v", "dvvideo", "-pix_fmt", "yuv422p", "-b:v", "100M", "-s", "1440x1080"], None, "mov", Codec::Dv, Some("DVCPRO HD"), 30, 25.0, Some("bff")),
        c("sp-mp4", CIF, SP, Some(AAC), "mp4", Codec::Mpeg4, Some("Simple"), 50, 25.0, None),
        c("asp-mp4", CIF, ASP, Some(AAC), "mp4", Codec::Mpeg4, Some("Advanced Simple"), 50, 25.0, None),
        c("asp-mov", CIF, ASP, Some(AAC), "mov", Codec::Mpeg4, Some("Advanced Simple"), 50, 25.0, None),
        c("asp-mkv", CIF, ASP, Some(PCM), "mkv", Codec::Mpeg4, Some("Advanced Simple"), 50, 25.0, None),
        c("sp-avi-xvid", CIF, &["-c:v", "mpeg4", "-qscale:v", "3", "-g", "12", "-vtag", "xvid", "-pix_fmt", "yuv420p"], Some(PCM), "avi", Codec::Mpeg4, None, 50, 25.0, None),
        c("asp-avi-divx", CIF, &["-c:v", "mpeg4", "-qscale:v", "3", "-g", "12", "-bf", "2", "-vtag", "DIVX", "-pix_fmt", "yuv420p"], Some(PCM), "avi", Codec::Mpeg4, None, 50, 25.0, None),
        c("h263-3gp", CIF, &["-c:v", "h263", "-s", "352x288", "-g", "12"], Some(&["-c:a", "aac", "-b:a", "64k", "-ar", "16000", "-ac", "1"]), "3gp", Codec::Mpeg4, Some("H.263"), 50, 25.0, None),
        c("h263-3g2", CIF, &["-c:v", "h263", "-s", "352x288", "-g", "12"], None, "3g2", Codec::Mpeg4, Some("H.263"), 50, 25.0, None),
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
    assert!((video.frame_rate.unwrap() - case.fps).abs() < 0.01, "{name}: {:?}", video.frame_rate);
    assert_eq!(video.bit_depth, Some(8), "{name}");
    assert_eq!(video.profile, case.profile, "{name}");
    assert_eq!(video.interlaced, case.interlaced, "{name}");
    let (meta, _) = Demuxer::open(&path).expect("open");
    assert_eq!(meta.frame_count, case.frames, "{name}");
    if case.aargs.is_some() {
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), 1, "{name}");
    }

    // ---- pictures
    let all = session_frames(&path);
    assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..case.frames).collect::<Vec<_>>(), "{name}");
    let want = ffmpeg_bgra(&path, all[0].width, all[0].height);
    let size = all[0].bgra.len();
    assert_eq!(want.len(), size * all.len(), "{name}: frame count or size differs from ffmpeg's");
    if case.interlaced.is_none() {
        let mut worst = 99.0f64;
        for (i, f) in all.iter().enumerate() {
            worst = worst.min(psnr(&f.bgra, &want[i * size..(i + 1) * size]));
        }
        eprintln!("{name}: worst PSNR against ffmpeg {worst:.1} dB");
        assert!(worst >= MIN_PSNR, "{name}: PSNR {worst:.1} dB");
    } else {
        // Woven by the encoder, deinterlaced by us: less row-to-row energy than ffmpeg's woven decode.
        let w = all[0].width as usize;
        let (mut woven, mut ours) = (0.0, 0.0);
        for n in 3..(case.frames as usize - 3) {
            woven += comb(&want[n * size..(n + 1) * size], w);
            ours += comb(&all[n].bgra, w);
        }
        eprintln!("{name}: comb woven {woven:.1}, deinterlaced {ours:.1}");
        assert!(ours <= woven, "{name}: the interlaced picture is not deinterlaced");
    }

    // ---- seeks: a frame after a seek is the frame a sequential decode gives
    let n = case.frames;
    for target in [0, 1, 13, n / 2, n - 2, n - 1] {
        let seeked = software(|| {
            let (_, rx, _c) = DecodeSession::open_at(&path, target).expect("open_at");
            rx.into_iter().next().unwrap_or_else(|| panic!("{name}: no frame after seek to {target}"))
        });
        assert_eq!(seeked.index, target, "{name}");
        assert!(seeked.bgra == all[target as usize].bgra, "{name}: seek to {target} differs from sequential");
    }
    let (a, b) = (n / 4, n / 4 + 9);
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
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), case.frames, "{name}");
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
fn dv_opens_decodes_seeks_and_exports_from_mov_avi_and_raw() {
    run(|c| c.codec == Codec::Dv, &["dvvideo", "pcm_s16le"], "dv");
}

#[test]
fn mpeg4_part2_opens_decodes_seeks_and_exports_from_mp4_mov_mkv_and_avi() {
    run(|c| c.codec == Codec::Mpeg4 && !c.name.starts_with("h263"), &["mpeg4", "aac", "pcm_s16le"], "mpeg4");
}

#[test]
fn h263_opens_decodes_seeks_and_exports_from_3gp_and_3g2() {
    run(|c| c.name.starts_with("h263"), &["h263", "aac"], "h263");
}

#[test]
fn raw_dv_carries_its_sound() {
    if !have(&["dvvideo", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("dvaudio");
    let case = &cases()[2];
    let path = make(&dir, case);
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1);
    let (meta, rx) = AudioSession::open_multi_streams(&[(path.clone(), 0)], &[(Some(0), 0.0, f64::INFINITY)])
        .expect("open")
        .expect("sound");
    assert_eq!(meta.sample_rate, 48_000);
    let width = usize::from(meta.channels);
    let mut got = Vec::new();
    for chunk in rx {
        got.extend_from_slice(&chunk.samples);
    }
    let peak = got.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(got.len() / width >= 48_000 * 9 / 10, "{} frames of sound", got.len() / width);
    assert!(peak > 0.05, "silent: {peak}");
    let _ = std::fs::remove_dir_all(&dir);
}
