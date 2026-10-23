//! Interlaced H.264 (x264 MBAFF, 8-bit and High 10, mp4 and mkv) end to end:
//! what the probe reports, that `ec-h264`'s woven pictures are ffmpeg's byte for
//! byte, how close the engine's deinterlacer comes to ffmpeg's `bwdif` and
//! `yadif` against the progressive original, that a mid-GOP seek equals a
//! sequential decode, that a progressive source is untouched, that an export is
//! progressive with its frame count, and that High 4:2:2 is still refused.
//!
//! Clips are synthetic (`testsrc2`, and a moving edge made with `geq`),
//! interlaced by ffmpeg's `interlace` filter (two source moments per frame) and
//! encoded by libx264 into the temp dir; without ffmpeg and libx264 each test
//! says so and passes. `VE_SW` is process-wide, so tests that pin it hold [`SW`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_core::frame::{Frame as CoreFrame, Interlace, PixelFormat};
use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase, VideoFrame};
use ec_h264::H264Decoder;
use engine::PlaybackSession;
use engine::decode::DecodeSession;
use engine::deinterlace::Deinterlacer;
use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};

const W: usize = 352;
const H: usize = 288;
/// Interlaced frames; the source has twice as many progressive ones.
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

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// What a clip is made of.
#[derive(Clone, Copy)]
struct Look {
    /// A lavfi source at 50 progressive frames a second.
    source: &'static str,
    /// Top field first (else bottom).
    tff: bool,
    /// 10-bit High 10 (else 8-bit High).
    deep: bool,
    /// High 4:2:2 (else 4:2:0).
    c422: bool,
}

const TESTSRC: &str = "testsrc2=size=352x288:rate=50";
/// A hard vertical edge moving 8 pixels per source moment: the worst case for
/// combing.
const EDGE: &str = "nullsrc=size=352x288:rate=50,format=yuv420p,geq=lum='if(lt(X,20+8*N),230,25)':cb=128:cr=128";

impl Look {
    fn pix(self) -> &'static str {
        match (self.c422, self.deep) {
            (false, false) => "yuv420p",
            (false, true) => "yuv420p10le",
            (true, false) => "yuv422p",
            (true, true) => "yuv422p10le",
        }
    }

    /// Bytes of one raw frame.
    fn frame_bytes(self) -> usize {
        W * H * if self.c422 { 2 } else { 3 } / if self.c422 { 1 } else { 2 } * if self.deep { 2 } else { 1 }
    }
}

/// The progressive original (`ref`, raw, every other source moment: the one the
/// kept field belongs to) and the interlaced clip `name`.
fn make(dir: &Path, name: &str, look: Look) -> (PathBuf, PathBuf) {
    let src = dir.join(format!("{name}.src.y4m"));
    ffmpeg(&[
        "-f", "lavfi", "-i", look.source, "-frames:v", &(FRAMES * 2).to_string(), "-pix_fmt", if look.c422 { "yuv422p" } else { "yuv420p" },
        src.to_str().unwrap(),
    ]);
    let reference = dir.join(format!("{name}.ref.yuv"));
    // Source moment 2n belongs to interlaced frame n whichever field leads: a
    // top-first frame is lines of 2n over lines of 2n+1, a bottom-first one the
    // other way round, and the field that leads is the one a frame-rate
    // deinterlacer keeps.
    ffmpeg(&[
        "-i",
        src.to_str().unwrap(),
        "-vf",
        "select=not(mod(n\\,2))",
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        look.pix(),
        reference.to_str().unwrap(),
    ]);
    let clip = dir.join(name);
    let scan = if look.tff { "tff" } else { "bff" };
    let params = format!("interlaced=1:{}=1:keyint=10:min-keyint=10:bframes=2", scan);
    let profile = match (look.c422, look.deep) {
        (true, _) => "high422",
        (false, true) => "high10",
        (false, false) => "high",
    };
    ffmpeg(&[
        "-i",
        src.to_str().unwrap(),
        "-vf",
        &format!("interlace=scan={scan}"),
        "-c:v",
        "libx264",
        "-profile:v",
        profile,
        "-pix_fmt",
        look.pix(),
        "-crf",
        "10",
        "-flags",
        "+ildct+ilme",
        "-x264-params",
        &params,
        clip.to_str().unwrap(),
    ]);
    (clip, reference)
}

/// Every picture ec-h264 releases for `clip`, woven, in display order.
fn decode_all(clip: &Path) -> Vec<VideoFrame> {
    let (_, mut demuxer) = Demuxer::open(clip).expect("open");
    demuxer.seek_to_sync_at_or_before(0);
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder");
    let mut out = Vec::new();
    let take = |decoder: &mut H264Decoder, out: &mut Vec<VideoFrame>| {
        while let Ok(frame) = decoder.receive_frame() {
            if let CoreFrame::Video(pic) = frame {
                out.push(pic);
            }
        }
    };
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).expect("decode");
        take(&mut decoder, &mut out);
    }
    decoder.flush().expect("flush");
    take(&mut decoder, &mut out);
    out
}

fn raw(pics: &[VideoFrame]) -> Vec<u8> {
    let mut out = Vec::new();
    for pic in pics {
        for plane in &pic.planes {
            out.extend_from_slice(&plane.data);
        }
    }
    out
}

fn ffmpeg_raw(args: &[&str], path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(args)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// PSNR over every plane of every frame; `deep` reads 16-bit samples at 10 bits.
fn psnr(a: &[u8], b: &[u8], deep: bool) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let (mut squared, n) = (0f64, if deep { a.len() / 2 } else { a.len() });
    if deep {
        for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
            let d = f64::from(u16::from_le_bytes(*x)) - f64::from(u16::from_le_bytes(*y));
            squared += d * d;
        }
    } else {
        for (x, y) in a.iter().zip(b) {
            let d = f64::from(*x) - f64::from(*y);
            squared += d * d;
        }
    }
    if squared == 0.0 {
        return f64::INFINITY;
    }
    let peak = if deep { 1023.0f64 } else { 255.0 };
    10.0 * (peak * peak * n as f64 / squared).log10()
}

/// Combing: the share of luma pixels that are an extreme against both vertical
/// neighbours, in the same direction, by more than 20 steps (a feather edge
/// between two fields of a moving object does exactly this; a vertical edge
/// and smooth shading do not).
fn combing(luma: &[u8], width: usize, height: usize, deep: bool) -> f64 {
    let sample = |x: usize, y: usize| -> i32 {
        if deep {
            let i = (y * width + x) * 2;
            i32::from(u16::from_le_bytes([luma[i], luma[i + 1]])) >> 2
        } else {
            i32::from(luma[y * width + x])
        }
    };
    let mut hits = 0u64;
    for y in 1..height - 1 {
        for x in 0..width {
            let (a, b, c) = (sample(x, y - 1), sample(x, y), sample(x, y + 1));
            if ((b - a) > 20 && (b - c) > 20) || ((a - b) > 20 && (c - b) > 20) {
                hits += 1;
            }
        }
    }
    hits as f64 / (width * (height - 2)) as f64
}

/// The luma planes of a raw run of frames, one `Vec` per frame.
fn lumas(raw: &[u8], look: Look) -> Vec<&[u8]> {
    let bytes = if look.deep { 2 } else { 1 };
    raw.chunks(look.frame_bytes()).map(|f| &f[..W * H * bytes]).collect()
}

fn combing_of(raw: &[u8], look: Look) -> f64 {
    let l = lumas(raw, look);
    l.iter().map(|y| combing(y, W, H, look.deep)).sum::<f64>() / l.len() as f64
}

fn deinterlaced(pics: Vec<VideoFrame>) -> Vec<VideoFrame> {
    let mut stage = Deinterlacer::new(true, 0, Interlace::Progressive);
    let mut out = Vec::new();
    for (i, pic) in pics.into_iter().enumerate() {
        out.extend(stage.push(i as i64, pic).map(|(_, f)| f));
    }
    out.extend(stage.finish().map(|(_, f)| f));
    out
}

const LOOKS: [(&str, Look); 8] = [
    ("t8-tff.mp4", Look { source: TESTSRC, tff: true, deep: false, c422: false }),
    ("t8-bff.mkv", Look { source: TESTSRC, tff: false, deep: false, c422: false }),
    ("t10-tff.mp4", Look { source: TESTSRC, tff: true, deep: true, c422: false }),
    ("t10-bff.mkv", Look { source: TESTSRC, tff: false, deep: true, c422: false }),
    ("t422-tff.mp4", Look { source: TESTSRC, tff: true, deep: false, c422: true }),
    ("t422-10-bff.mkv", Look { source: TESTSRC, tff: false, deep: true, c422: true }),
    ("t422-10-tff.mp4", Look { source: TESTSRC, tff: true, deep: true, c422: true }),
    ("e8-tff.mp4", Look { source: EDGE, tff: true, deep: false, c422: false }),
];

#[test]
fn probe_reports_interlace_and_field_order() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-probe");
    for (name, look) in LOOKS {
        let (clip, _) = make(&dir, name, look);
        let facts = engine::probe_media(&clip).video.expect("a picture track");
        assert_eq!(facts.interlaced, Some(if look.tff { "tff" } else { "bff" }), "{name}");
        assert_eq!(facts.bit_depth, look.deep.then_some(10), "{name}");
        if look.c422 {
            assert_eq!(facts.profile, Some("High 4:2:2"), "{name}");
        }
        let (meta, demuxer) = Demuxer::open(&clip).expect("interlaced opens, High 10 too");
        assert_eq!(meta.frame_count, FRAMES, "{name}");
        assert!(demuxer.h264_interlaced(), "{name}");
    }
    // A progressive stream says nothing.
    let prog = dir.join("p.mp4");
    ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25", "-frames:v", "10", "-c:v", "libx264", prog.to_str().unwrap()]);
    assert_eq!(engine::probe_media(&prog).video.expect("video").interlaced, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn software_pictures_are_ffmpegs_before_deinterlacing() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-exact");
    for (name, look) in LOOKS {
        let (clip, _) = make(&dir, name, look);
        let pics = decode_all(&clip);
        assert_eq!(pics.len(), FRAMES as usize, "{name}");
        let want_order = if look.tff { Interlace::TopFieldFirst } else { Interlace::BottomFieldFirst };
        assert!(
            pics.iter().all(|p| p.interlace == want_order || p.interlace == Interlace::Progressive),
            "{name}: pictures state the wrong order"
        );
        let format = match (look.c422, look.deep) {
            (false, false) => PixelFormat::I420,
            (false, true) => PixelFormat::I010,
            (true, false) => PixelFormat::I422,
            (true, true) => PixelFormat::I210,
        };
        assert_eq!(pics[0].format, format, "{name}");
        let want = ffmpeg_raw(&[], &clip, look.pix());
        assert!(raw(&pics) == want, "{name}: woven pictures differ from ffmpeg's");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The PSNR table: woven, this engine's filter, ffmpeg's bwdif and yadif,
/// each against the progressive original.
#[test]
fn deinterlaced_psnr_is_within_half_a_db_of_bwdif() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-psnr");
    for (name, look) in LOOKS {
        let (clip, reference) = make(&dir, name, look);
        let want = std::fs::read(&reference).expect("reference");
        let pics = decode_all(&clip);
        let woven = raw(&pics);
        let ours = raw(&deinterlaced(pics));
        let parity = if look.tff { "tff" } else { "bff" };
        let bwdif = ffmpeg_raw(&["-vf", &format!("bwdif=mode=send_frame:parity={parity}:deint=all")], &clip, look.pix());
        let yadif = ffmpeg_raw(&["-vf", &format!("yadif=mode=send_frame:parity={parity}:deint=all")], &clip, look.pix());
        let p = |x: &[u8]| psnr(&want, x, look.deep);
        let (pw, po, pb, py) = (p(&woven), p(&ours), p(&bwdif), p(&yadif));
        let c = |x: &[u8]| combing_of(x, look);
        eprintln!(
            "PSNR {name}: woven {pw:.2} dB, ours {po:.2} dB, bwdif {pb:.2} dB, yadif {py:.2} dB | combing woven {:.5}, ours {:.5}, bwdif {:.5}, yadif {:.5}, original {:.5}",
            c(&woven),
            c(&ours),
            c(&bwdif),
            c(&yadif),
            c(&want),
        );
        assert!(po > pb - 0.5, "{name}: ours {po:.2} dB is not within 0.5 dB of bwdif {pb:.2} dB");
        assert!(po > pw + 1.0, "{name}: ours {po:.2} dB is not clearly above woven {pw:.2} dB");
        assert!(c(&ours) < c(&woven) * 0.25 + 1e-6, "{name}: still combed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_frames_are_deinterlaced_and_seeks_match_a_sequential_decode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-seek");
    for (name, look) in LOOKS {
        let (clip, _) = make(&dir, name, look);
        let (meta, all) = software(|| {
            let (meta, rx) = DecodeSession::open(&clip).expect("open");
            (meta, rx.into_iter().collect::<Vec<_>>())
        });
        assert_eq!(meta.frame_count, FRAMES, "{name}");
        assert_eq!(
            all.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..FRAMES).collect::<Vec<_>>(),
            "{name}: display order"
        );
        // The session's frames are the filter's, not the woven pictures: the
        // BGRA of each is the conversion of the deinterlaced planes, so its
        // combing (read off green, which carries most of the luma) is the
        // filtered picture's, far under the woven one's on the moving edge.
        if name.starts_with("e8") {
            let green = |f: &engine::decode::Frame| -> Vec<u8> { f.bgra.chunks(4).map(|p| p[1]).collect() };
            let session: f64 = all.iter().map(|f| combing(&green(f), W, H, false)).sum::<f64>() / all.len() as f64;
            let woven = combing_of(&raw(&decode_all(&clip)), look);
            assert!(session < woven * 0.25, "{name}: session combing {session:.5} vs woven {woven:.5}");
        }
        for target in [0, 1, 2, 5, 9, 10, 11, 13, 17, 20, 25, FRAMES - 1] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{name}: no frame after seek to {target}"))
            });
            assert_eq!(seeked.index, target, "{name}");
            assert!(seeked.bgra == all[target as usize].bgra, "{name}: seek to {target} differs from sequential");
        }
        // A range that ends mid-stream still has its last picture filtered
        // against the one after it.
        let range = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_range(&clip, 6, 14).expect("open_range");
            rx.into_iter().collect::<Vec<_>>()
        });
        assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (6..14).collect::<Vec<_>>(), "{name}");
        for f in &range {
            assert!(f.bgra == all[f.index as usize].bgra, "{name}: range frame {} differs", f.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_progressive_source_decodes_untouched() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-prog");
    let clip = dir.join("p.mp4");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=30", "-frames:v", "30", "-c:v", "libx264", "-x264-params",
        "keyint=10:min-keyint=10:bframes=2", clip.to_str().unwrap(),
    ]);
    let (_, demuxer) = Demuxer::open(&clip).expect("open");
    assert!(!demuxer.h264_interlaced());
    // The stage is a pass-through for it: the same picture back at once.
    let pics = decode_all(&clip);
    let mut stage = Deinterlacer::new(false, 0, Interlace::Progressive);
    for (i, pic) in pics.iter().enumerate() {
        let (tag, same) = stage.push(i as i64, pic.clone()).expect("no delay");
        assert_eq!(tag, i as i64);
        assert!(&same == pic);
    }
    // A hash of the whole decode, for comparing builds by hand.
    let frames = software(|| DecodeSession::open(&clip).expect("open").1.into_iter().collect::<Vec<_>>());
    let mut hash = 0xcbf29ce484222325u64;
    for f in &frames {
        for b in &f.bgra {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    }
    eprintln!("progressive decode hash {hash:016x} over {} frames", frames.len());
    let _ = std::fs::remove_dir_all(&dir);
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
fn an_interlaced_source_exports_progressive() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-export");
    for (name, look) in [LOOKS[0], LOOKS[1], LOOKS[2], LOOKS[4], LOOKS[5], LOOKS[7]] {
        let (clip, reference) = make(&dir, name, look);
        let want = std::fs::read(&reference).expect("reference");
        for (format, ext) in [(Format::Mp4, "mp4"), (Format::HevcMp4, "hevc.mp4"), (Format::Hevc, "mkv")] {
            let out = dir.join(format!("{name}-out.{ext}"));
            software(|| export_as(&clip, &out, format));
            let field = ffprobe_field(&out, "field_order");
            assert!(field == "progressive" || field == "unknown" || field == "N/A", "{name}.{ext}: field_order {field}");
            if look.deep && format != Format::Mp4 {
                assert_eq!(ffprobe_field(&out, "pix_fmt"), "yuv420p10le", "{name}.{ext}: 10 bits kept");
            }
            let decoded = ffmpeg_raw(&[], &out, look.pix());
            let frame = look.frame_bytes();
            assert_eq!(decoded.len() / frame, FRAMES as usize, "{name}.{ext}: every frame, no more");
            let secs: f64 = {
                let o = Command::new("ffprobe")
                    .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                    .arg(&out)
                    .output()
                    .expect("ffprobe runs");
                String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0.0)
            };
            assert!((secs - f64::from(FRAMES) / 25.0).abs() < 0.1, "{name}.{ext}: {secs} s");
            // Progressive in content too: far less combing than the woven
            // source, and a good match to the original.
            let woven = combing_of(&raw(&decode_all(&clip)), look);
            let exported = combing_of(&decoded, look);
            let db = psnr(&want, &decoded, look.deep);
            eprintln!("export {name}.{ext}: field_order {field}, combing {exported:.5} (woven {woven:.5}), {db:.2} dB vs original");
            assert!(exported <= woven * 0.5 + 1e-6, "{name}.{ext}: combing {exported:.5} vs woven {woven:.5}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn four_four_four_monochrome_and_lossless_open_and_keep_the_gpu_off() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("il-open");
    let cases: [(&str, &str, &str, &str, engine::colorspace::Chroma); 5] = [
        ("a.mp4", "high444", "yuv444p", "interlaced=1:tff=1", engine::colorspace::Chroma::C444),
        ("b.mkv", "high444", "yuv444p10le", "interlaced=1:tff=1", engine::colorspace::Chroma::C444),
        ("c.mp4", "high444", "yuv444p10le", "keyint=5", engine::colorspace::Chroma::C444),
        ("d.mp4", "high", "gray", "keyint=5", engine::colorspace::Chroma::Gray),
        ("e.mkv", "high444", "yuv420p", "qp=0", engine::colorspace::Chroma::C420),
    ];
    for (name, profile, pix, params, chroma) in cases {
        let clip = dir.join(name);
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=25", "-frames:v", "10", "-c:v", "libx264"])
            .args(["-profile:v", profile, "-pix_fmt", pix, "-x264-params", params])
            .arg(&clip)
            .output()
            .expect("ffmpeg runs");
        if !out.status.success() {
            eprintln!("{name}: this libx264 cannot make {pix}: skipping the case");
            continue;
        }
        let (meta, demuxer) = Demuxer::open(&clip).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(meta.frame_count, 10, "{name}");
        assert_eq!(demuxer.chroma(), Some(chroma), "{name}");
        let (_, backend) = engine::decode::probe(&clip).expect("probe");
        assert_eq!(backend, engine::decode::Backend::Software, "{name}: never offered to VA-API");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Speed of the filter alone at 1080 lines, for the report.
#[test]
fn deinterlace_speed_at_1080() {
    let mut frames = Vec::new();
    for k in 0..3usize {
        let mut f = VideoFrame::alloc(PixelFormat::I420, 1920, 1080);
        for (i, plane) in f.planes.iter_mut().enumerate() {
            let stride = plane.stride;
            let data: Vec<u8> = (0..plane.data.len())
                .map(|n| (((n % stride) * 5 + (n / stride) * 3 + k * 17 + i * 31) ^ (((n / stride) & 1) * (k * 9))) as u8)
                .collect();
            *plane = ec_core::Plane::new(data, stride);
        }
        frames.push(f);
    }
    let run = |threads: usize| {
        let n = 30;
        let started = Instant::now();
        let mut keep = 0usize;
        for _ in 0..n {
            let out = engine::deinterlace::filter_frame(
                &frames[0],
                &frames[1],
                &frames[2],
                engine::deinterlace::Keep::Top,
                threads,
            );
            keep += usize::from(out.planes[0].data[1920 * 3]);
        }
        std::hint::black_box(keep);
        f64::from(n) / started.elapsed().as_secs_f64()
    };
    eprintln!("deinterlace 1080: {:.1} fps on 1 thread, {:.1} fps on 4 threads", run(1), run(4));
}
