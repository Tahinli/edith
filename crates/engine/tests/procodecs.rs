//! ProRes, DNxHD/DNxHR and Motion JPEG (QuickTime/MP4 and Matroska) end to end: probe, every
//! picture against ffmpeg's decode, mid-file seeks, picture/sound sync after open and seek, and
//! an export. All media is synthetic, made by ffmpeg into a scratch directory at test time;
//! without ffmpeg (or the encoder a case needs) a case says so and passes, as `mpegts` does.
//! `VE_SW` is process-wide, so tests pin it through [`software`].
//!
//! Pictures are compared in BGRA against `ffmpeg -pix_fmt bgra` by PSNR: the engine averages
//! 4:2:2 / 4:4:4 chroma down to 4:2:0 and drops 10/12-bit to 8 before converting, which ffmpeg's
//! scaler does differently, so exactness is not the claim (the decoders are byte-exact against
//! ffmpeg in their own crates); a wrong matrix, range, plane or off-by-one frame lands far below
//! [`MIN_PSNR`]. A seek, and the same stream in the other container, are compared exactly.
//!
//! ```text
//! cargo test -p engine --release --test procodecs
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
    let dir = std::env::temp_dir().join(format!("edith-procodecs-{name}-{}", std::process::id()));
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

const SINE: &str = "sine=frequency=440:sample_rate=48000:duration=2.4";
/// Black with a white flash from 1.0 s to 1.2 s (frames 25..30), 2.4 s long.
const FLASH: &str = "color=c=black:s=352x288:r=25:d=2.4,drawbox=x=0:y=0:w=352:h=288:color=white:t=fill:enable='between(t,1,1.2)'";
/// A 1 kHz beep over the same 1.0 s to 1.2 s.
const BEEP: &str = "aevalsrc=exprs='0.8*sin(2*PI*1000*t)*between(t,1,1.2)|0.8*sin(2*PI*1000*t)*between(t,1,1.2)':s=48000:d=2.4";

/// One clip to make and check.
struct Case {
    name: &'static str,
    /// lavfi video source, then the encoder arguments.
    video: &'static str,
    vargs: &'static [&'static str],
    /// `.mov` or `.mkv`.
    ext: &'static str,
    codec: Codec,
    profile: Option<&'static str>,
    depth: u8,
    width: u32,
    frames: u32,
    /// `Some("tff")` for an interlaced clip, which is deinterlaced and so not compared to ffmpeg.
    interlaced: Option<&'static str>,
}

const CARD: &str = "testsrc2=size=352x288:rate=25:duration=2";
const CARD_TFF: &str = "testsrc2=size=352x288:rate=50:duration=2,interlace=scan=tff";
const HD: &str = "testsrc2=size=1920x1080:rate=25:duration=1";
const HD_TFF: &str = "testsrc2=size=1920x1080:rate=50:duration=1,interlace=scan=tff";

const fn prores(name: &'static str, profile: &'static str, args: &'static [&'static str], depth: u8) -> Case {
    Case { name, video: CARD, vargs: args, ext: "mov", codec: Codec::ProRes, profile: Some(profile), depth, width: 352, frames: 50, interlaced: None }
}

fn prores_cases() -> Vec<Case> {
    vec![
        prores("proxy", "422 Proxy", &["-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le"], 10),
        prores("lt", "422 LT", &["-c:v", "prores_ks", "-profile:v", "1", "-pix_fmt", "yuv422p10le"], 10),
        prores("standard", "422", &["-c:v", "prores_ks", "-profile:v", "2", "-pix_fmt", "yuv422p10le"], 10),
        prores("hq", "422 HQ", &["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"], 10),
        prores("4444", "4444", &["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"], 12),
        prores("4444xq", "4444 XQ", &["-c:v", "prores_ks", "-profile:v", "5", "-pix_fmt", "yuv444p10le"], 12),
        // The alpha plane is dropped (the engine has no alpha path for a video clip); the colour
        // planes must come out as the opaque 4444 does.
        prores("4444-alpha", "4444", &["-vf", "format=yuva444p10le", "-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuva444p10le"], 12),
        Case {
            name: "hq-interlaced",
            video: CARD_TFF,
            vargs: &["-c:v", "prores_ks", "-profile:v", "3", "-flags", "+ildct", "-top", "1", "-pix_fmt", "yuv422p10le"],
            ext: "mov",
            codec: Codec::ProRes,
            profile: Some("422 HQ"),
            depth: 10,
            width: 352,
            frames: 50,
            interlaced: Some("tff"),
        },
    ]
}

fn dnxhd_cases() -> Vec<Case> {
    let hd = |name, rate: &'static [&'static str], profile, depth| Case {
        name,
        video: HD,
        vargs: rate,
        ext: "mov",
        codec: Codec::Dnxhd,
        profile: Some(profile),
        depth,
        width: 1920,
        frames: 25,
        interlaced: None,
    };
    let hr = |name, args: &'static [&'static str], profile, depth, ext| Case {
        name,
        video: CARD,
        vargs: args,
        ext,
        codec: Codec::Dnxhd,
        profile: Some(profile),
        depth,
        width: 352,
        frames: 50,
        interlaced: None,
    };
    vec![
        hd("dnxhd-120", &["-c:v", "dnxhd", "-b:v", "120M", "-pix_fmt", "yuv422p"], "DNxHD 120", 8),
        hd("dnxhd-185x", &["-c:v", "dnxhd", "-b:v", "185M", "-pix_fmt", "yuv422p10le"], "DNxHD 185x", 10),
        hd("dnxhd-36", &["-c:v", "dnxhd", "-b:v", "36M", "-pix_fmt", "yuv422p"], "DNxHD 36", 8),
        hr("dnxhr-lb", &["-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-pix_fmt", "yuv422p"], "DNxHR LB", 8, "mov"),
        hr("dnxhr-sq", &["-c:v", "dnxhd", "-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "DNxHR SQ", 8, "mov"),
        hr("dnxhr-hq", &["-c:v", "dnxhd", "-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"], "DNxHR HQ", 8, "mov"),
        hr("dnxhr-hqx", &["-c:v", "dnxhd", "-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "DNxHR HQX", 10, "mov"),
        hr("dnxhr-444", &["-c:v", "dnxhd", "-profile:v", "dnxhr_444", "-pix_fmt", "yuv444p10le"], "DNxHR 444", 10, "mov"),
        hr("dnxhr-sq-mkv", &["-c:v", "dnxhd", "-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"], "DNxHR SQ", 8, "mkv"),
        hr("dnxhr-hqx-mkv", &["-c:v", "dnxhd", "-profile:v", "dnxhr_hqx", "-pix_fmt", "yuv422p10le"], "DNxHR HQX", 10, "mkv"),
        Case {
            name: "dnxhd-interlaced",
            video: HD_TFF,
            vargs: &["-c:v", "dnxhd", "-b:v", "120M", "-flags", "+ildct", "-top", "1", "-pix_fmt", "yuv422p"],
            ext: "mov",
            codec: Codec::Dnxhd,
            profile: Some("DNxHD 120"),
            depth: 8,
            width: 1920,
            frames: 25,
            interlaced: Some("tff"),
        },
    ]
}

fn mjpeg_cases() -> Vec<Case> {
    let c = |name, pix: &'static [&'static str], ext| Case {
        name,
        video: CARD,
        vargs: pix,
        ext,
        codec: Codec::Mjpeg,
        profile: None,
        depth: 8,
        width: 352,
        frames: 50,
        interlaced: None,
    };
    vec![
        c("mjpeg-420-mov", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"], "mov"),
        c("mjpeg-422-mov", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj422p"], "mov"),
        c("mjpeg-420-mkv", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"], "mkv"),
        c("mjpeg-422-mkv", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj422p"], "mkv"),
    ]
}

/// Makes `case` with a sine track.
fn make(dir: &Path, case: &Case) -> PathBuf {
    let out = dir.join(format!("{}.{}", case.name, case.ext));
    let mut args = vec!["-f", "lavfi", "-i", case.video, "-f", "lavfi", "-i", SINE, "-map", "0:v", "-map", "1:a"];
    args.extend(case.vargs);
    args.extend(["-c:a", "aac", "-b:a", "128k", "-shortest", s(&out)]);
    ffmpeg(&args);
    out
}

fn session_frames(path: &Path) -> Vec<engine::decode::Frame> {
    software(|| DecodeSession::open(path).expect("open").1.into_iter().collect())
}

/// ffmpeg's decode of the picture, as BGRA at the file's size.
fn ffmpeg_bgra(path: &Path, width: u32) -> Vec<u8> {
    // The engine reads an untagged picture as BT.601 up to 576 lines and BT.709 above.
    // A stream that states its matrix is converted by ffmpeg as stated, as the engine does; an
    // untagged one is BT.601 to ffmpeg's scaler, and the size rule's BT.709 above 576 lines here.
    let vf = if width > 854 { "scale=in_color_matrix=bt709:out_color_matrix=bt709" } else { "null" };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-vf", vf, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
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

/// Probe, decode, seek and export one case.
fn check(dir: &Path, case: &Case) {
    let name = case.name;
    let path = make(dir, case);
    let fps = 25.0;

    // ---- probe
    let facts = engine::probe_media(&path);
    let video = facts.video.as_ref().unwrap_or_else(|| panic!("{name}: no picture"));
    assert_eq!(video.codec, case.codec, "{name}");
    assert_eq!(video.width, case.width, "{name}");
    assert!((video.frame_rate.unwrap() - fps).abs() < 0.01, "{name}: {:?}", video.frame_rate);
    assert_eq!(video.bit_depth, Some(case.depth), "{name}");
    assert_eq!(video.profile, case.profile, "{name}");
    assert_eq!(video.interlaced, case.interlaced, "{name}");
    let want_secs = f64::from(case.frames) / fps;
    let secs = facts.duration_secs.expect("a length");
    assert!((secs - want_secs).abs() <= 1.0 / fps + 1e-6, "{name}: {secs} s");
    let (meta, demuxer) = Demuxer::open(&path).expect("open");
    assert_eq!(meta.frame_count, case.frames, "{name}");
    assert_eq!(demuxer.bit_depth(), case.depth, "{name}");
    // Every frame is a sync sample.
    assert_eq!(engine::demux::sync_points(&path).len() as u32, case.frames, "{name}");
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1, "{name}");

    // ---- pictures
    let all = session_frames(&path);
    assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..case.frames).collect::<Vec<_>>(), "{name}");
    if case.interlaced.is_none() {
        let want = ffmpeg_bgra(&path, case.width);
        let size = all[0].bgra.len();
        assert_eq!(want.len(), size * all.len(), "{name}: frame count or size differs from ffmpeg's");
        let mut worst = 99.0f64;
        for (i, f) in all.iter().enumerate() {
            worst = worst.min(psnr(&f.bgra, &want[i * size..(i + 1) * size]));
        }
        eprintln!("{name}: worst PSNR against ffmpeg {worst:.1} dB");
        assert!(worst >= MIN_PSNR, "{name}: PSNR {worst:.1} dB");
    } else {
        // Woven by the encoder, deinterlaced by us: less row-to-row energy than ffmpeg's woven decode.
        let want = ffmpeg_bgra(&path, case.width);
        let size = all[0].bgra.len();
        let w = all[0].width as usize;
        let (mut woven, mut ours) = (0.0, 0.0);
        for n in 5..(case.frames as usize - 5) {
            woven += comb(&want[n * size..(n + 1) * size], w);
            ours += comb(&all[n].bgra, w);
        }
        eprintln!("{name}: comb woven {woven:.1}, deinterlaced {ours:.1}");
        assert!(ours < woven, "{name}: the interlaced picture is not deinterlaced");
    }

    // ---- seeks
    let n = case.frames;
    for target in [0, 1, 2, n / 3, n / 2, n - 2, n - 1] {
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
    let secs: f64 = ffprobe(&out, "format=duration", "v:0").parse().unwrap_or(0.0);
    assert!((secs - want_secs).abs() < 0.1, "{name}: exported {secs} s");
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), case.frames, "{name}");
}

#[test]
fn prores_every_profile_opens_decodes_seeks_and_exports() {
    if !have(&["prores_ks", "aac"]) {
        return;
    }
    let dir = scratch("prores");
    for case in prores_cases() {
        check(&dir, &case);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dnxhd_and_dnxhr_open_decode_seek_and_export() {
    if !have(&["dnxhd", "aac"]) {
        return;
    }
    let dir = scratch("dnxhd");
    for case in dnxhd_cases() {
        check(&dir, &case);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mjpeg_full_range_opens_decodes_seeks_and_exports_from_mov_and_mkv() {
    if !have(&["mjpeg", "aac"]) {
        return;
    }
    let dir = scratch("mjpeg");
    for case in mjpeg_cases() {
        check(&dir, &case);
    }
    // The same JPEGs in the two containers are the same pictures.
    let mov = session_frames(&dir.join("mjpeg-420-mov.mov"));
    let mkv = session_frames(&dir.join("mjpeg-420-mkv.mkv"));
    assert_eq!(mov.len(), mkv.len());
    assert!(psnr(&mov[10].bgra, &mkv[10].bgra) > 40.0, "mov and mkv differ (same source, same encoder)");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ interlaced MJPEG

fn le(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut inner = kind.to_vec();
    inner.extend(body);
    chunk(b"LIST", &inner)
}

/// A minimal AVI holding `packets` as MJPG frames at 25 fps: the carrier the two-field packets are
/// stream-copied out of (ffmpeg's mjpeg encoder cannot make them itself).
fn write_avi(path: &Path, w: u32, h: u32, packets: &[Vec<u8>]) {
    let avih = le(&[40_000, 0, 0, 0x10, packets.len() as u32, 0, 1, 1 << 20, w, h, 0, 0, 0, 0]);
    let mut strh = b"vidsMJPG".to_vec();
    strh.extend(le(&[0, 0, 0, 1, 25, 0, packets.len() as u32, 1 << 20, 0xffff_ffff, 0]));
    for v in [0u16, 0, w as u16, h as u16] {
        strh.extend(v.to_le_bytes());
    }
    let mut strf = le(&[40, w, h]);
    strf.extend(1u16.to_le_bytes());
    strf.extend(24u16.to_le_bytes());
    strf.extend(b"MJPG");
    strf.extend(le(&[w * h * 3, 0, 0, 0, 0]));
    let mut strl = chunk(b"strh", &strh);
    strl.extend(chunk(b"strf", &strf));
    let mut hdrl = chunk(b"avih", &avih);
    hdrl.extend(list(b"strl", &strl));
    let mut movi = b"movi".to_vec();
    let mut idx = Vec::new();
    for p in packets {
        idx.extend(b"00dc");
        idx.extend(le(&[0x10, movi.len() as u32 - 4, p.len() as u32]));
        movi.extend(chunk(b"00dc", p));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(chunk(b"LIST", &movi));
    body.extend(chunk(b"idx1", &idx));
    let mut file = b"RIFF".to_vec();
    file.extend((body.len() as u32).to_le_bytes());
    file.extend(body);
    std::fs::write(path, file).unwrap();
}

/// Two-field MJPEG (one packet = two JPEGs, one per field) in `.mov` (`mjpa`) and `.mkv`: probed
/// as interlaced, deinterlaced, frame-exact, seekable, and the sound stays with the picture.
#[test]
fn interlaced_mjpeg_two_field_packets_open_deinterlace_and_seek() {
    if !have(&["mjpeg", "aac"]) {
        return;
    }
    let dir = scratch("mjpeg-fields");
    let fields = dir.join("fields.mjpg");
    // 25 half-height JPEGs per second from a 50 fields/s card; 60 fields = 30 frames.
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25", "-frames:v", "60", "-vf", "separatefields",
        "-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p", "-f", "mjpeg", s(&fields),
    ]);
    let raw = std::fs::read(&fields).unwrap();
    let starts: Vec<usize> = (0..raw.len() - 2).filter(|&i| raw[i..i + 3] == [0xff, 0xd8, 0xff]).collect();
    assert_eq!(starts.len(), 60);
    let jpegs: Vec<Vec<u8>> = starts
        .iter()
        .enumerate()
        .map(|(n, &a)| raw[a..starts.get(n + 1).copied().unwrap_or(raw.len())].to_vec())
        .collect();
    let packets: Vec<Vec<u8>> = jpegs.chunks(2).map(|c| [c[0].clone(), c[1].clone()].concat()).collect();
    let avi = dir.join("fields.avi");
    write_avi(&avi, 352, 288, &packets);
    for (ext, tag) in [("mov", Some("mjpa")), ("mkv", None)] {
        let path = dir.join(format!("fields.{ext}"));
        let mut a = vec!["-i", s(&avi), "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1.2", "-c:v", "copy", "-c:a", "aac"];
        if let Some(t) = tag {
            a.extend(["-tag:v", t]);
        }
        a.extend(["-shortest", s(&path)]);
        ffmpeg(&a);
        let facts = engine::probe_media(&path);
        let video = facts.video.as_ref().expect("a picture");
        assert_eq!(video.codec, Codec::Mjpeg, "{ext}");
        assert_eq!((video.width, video.height), (352, 288), "{ext}");
        assert_eq!(video.interlaced, Some("tff"), "{ext}");
        assert_eq!(video.bit_depth, Some(8), "{ext}");
        let all = session_frames(&path);
        assert_eq!(all.len(), 30, "{ext}");
        assert_eq!((all[0].width, all[0].height), (352, 288), "{ext}");
        // Deinterlaced: less row-to-row energy than ffmpeg's woven decode.
        let want = ffmpeg_bgra(&path, 352);
        let size = all[0].bgra.len();
        let (mut woven, mut ours) = (0.0, 0.0);
        for n in 5..25 {
            woven += comb(&want[n * size..(n + 1) * size], 352);
            ours += comb(&all[n].bgra, 352);
        }
        assert!(ours < woven, "{ext}: not deinterlaced ({ours:.1} vs {woven:.1})");
        for target in [0u32, 1, 7, 15, 29] {
            let seeked = software(|| {
                let (_, rx, _c) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().expect("a frame")
            });
            assert_eq!(seeked.index, target, "{ext}");
            assert!(seeked.bgra == all[target as usize].bgra, "{ext}: seek to {target} differs");
        }
        let out = dir.join(format!("fields-{ext}.mp4"));
        software(|| export_mp4(&path, &out));
        assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(0), 30, "{ext}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ sync

fn sound(path: &Path, from: f64, frames: usize) -> (u32, usize, Vec<f32>) {
    let (meta, rx) = AudioSession::open_multi_streams(&[(path.to_path_buf(), 0)], &[(Some(0), from, f64::INFINITY)])
        .expect("open")
        .expect("the file has sound");
    let width = usize::from(meta.channels);
    let mut out = Vec::new();
    for chunk in rx {
        out.extend_from_slice(&chunk.samples);
        if out.len() >= frames * width {
            break;
        }
    }
    out.truncate(frames * width);
    (meta.sample_rate, width, out)
}

fn first_loud(samples: &[f32], width: usize) -> Option<usize> {
    samples.chunks(width).position(|f| f.iter().any(|s| s.abs() > 0.2))
}

fn flash_frame(frames: &[engine::decode::Frame]) -> Option<u32> {
    frames
        .iter()
        .find(|f| f.bgra.chunks(4).map(|p| u64::from(p[1])).sum::<u64>() > 128 * (f.bgra.len() as u64 / 4))
        .map(|f| f.index)
}

/// The flash is at 1.0 s in the picture and the beep at 1.0 s in the sound, from open, after a
/// seek in either, and in the export.
#[test]
fn picture_and_sound_stay_in_sync_after_open_and_seek() {
    if !have(&["prores_ks", "dnxhd", "mjpeg", "aac"]) {
        return;
    }
    let dir = scratch("sync");
    let clips: [(&str, &str, &[&str]); 7] = [
        ("prores.mov", "mov", &["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"]),
        ("prores-4444.mov", "mov", &["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"]),
        ("dnxhr.mov", "mov", &["-c:v", "dnxhd", "-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"]),
        ("dnxhr.mkv", "mkv", &["-c:v", "dnxhd", "-profile:v", "dnxhr_sq", "-pix_fmt", "yuv422p"]),
        ("mjpeg.mov", "mov", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"]),
        ("mjpeg.mkv", "mkv", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"]),
        ("mjpeg-422.mov", "mov", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj422p"]),
    ];
    for (name, _, vargs) in clips {
        let path = dir.join(name);
        let mut args = vec!["-f", "lavfi", "-i", FLASH, "-f", "lavfi", "-i", BEEP, "-map", "0:v", "-map", "1:a"];
        args.extend(vargs);
        // PCM in Matroska, so a seek lands on the sample and not on an AAC frame boundary (21 ms).
        let audio: &[&str] = if name.ends_with("mkv") { &["-c:a", "pcm_s16le"] } else { &["-c:a", "aac", "-b:a", "128k"] };
        args.extend(audio);
        args.push(s(&path));
        ffmpeg(&args);
        let frames = session_frames(&path);
        assert_eq!(frames.len(), 60, "{name}");
        assert_eq!(flash_frame(&frames), Some(25), "{name}: the flash is at 1.0 s");
        let (rate, width, all) = sound(&path, 0.0, 48_000 * 3);
        let beep = first_loud(&all, width).expect("a beep") as f64 / f64::from(rate);
        assert!(beep > 0.99 && beep < 1.065, "{name}: the beep is at {beep:.3} s");
        let (_, _, tail) = sound(&path, 0.8, 48_000);
        let again = 0.8 + first_loud(&tail, width).expect("a beep after a seek") as f64 / f64::from(rate);
        assert!((again - beep).abs() < 0.002, "{name}: {again} vs {beep}");
        let at = software(|| {
            let (_, rx, _c) = DecodeSession::open_at(&path, 25).expect("seek");
            rx.into_iter().next().map(|f| f.index)
        });
        assert_eq!(at, Some(25), "{name}");
        // The export keeps them together.
        let out = dir.join(format!("{name}.mp4"));
        software(|| export_mp4(&path, &out));
        let video = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-an", "-vf", "scale=8:8,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "-"])
            .output()
            .expect("ffmpeg");
        let flash = video.stdout.chunks(64).position(|f| f.iter().map(|&b| u32::from(b)).sum::<u32>() > 128 * 64).expect("a flash");
        let pcm = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
            .output()
            .expect("ffmpeg");
        let samples: Vec<f32> = pcm.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let beep_out = first_loud(&samples, 2).expect("a beep") as f64 / 48_000.0;
        let flash_at = flash as f64 / 25.0;
        assert!((flash_at - 1.0).abs() < 0.045, "{name}: exported flash at {flash_at}");
        assert!((beep_out - flash_at).abs() < 0.09, "{name}: exported beep {beep_out} vs flash {flash_at}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// ProRes cannot ride in Matroska here (ec-matroska maps no ProRes id): the gap is a named
/// refusal, not a mis-decode. DNxHD and MJPEG do open from both containers (above).
#[test]
fn prores_in_matroska_is_a_named_gap() {
    if !have(&["prores_ks"]) {
        return;
    }
    let dir = scratch("prores-mkv");
    let path = dir.join("p.mkv");
    ffmpeg(&["-f", "lavfi", "-i", CARD, "-c:v", "prores_ks", "-profile:v", "3", s(&path)]);
    let err = Demuxer::open(&path).err().map(|e| e.to_string()).unwrap_or_default();
    eprintln!("prores in mkv: {err}");
    assert!(!err.is_empty(), "ProRes in Matroska opened, update the capability matrix");
    let _ = std::fs::remove_dir_all(&dir);
}
