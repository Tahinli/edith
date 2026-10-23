//! A clip's deinterlace setting (`Frame`, `Field`, `Off`) on interlaced H.264:
//! `Field` is ffmpeg's `bwdif=mode=send_field` (one picture per field, 50i -> 50p),
//! `Off` leaves the woven pictures as decoded, `Frame` is what it always was, and a
//! progressive source ignores all three. Exports and stills (`compose_at`) go
//! through the public session; the decode layer (seeks, reverse) is covered by
//! `deinterlace::session_tests`.
//!
//! Clips are synthetic (`testsrc2`, interlaced by ffmpeg's `interlace` filter and
//! encoded by libx264 MBAFF into the temp dir); without ffmpeg and libx264 each
//! test says so and passes. `VE_SW` is process-wide, so exports hold [`SW`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_core::frame::{Frame as CoreFrame, Interlace};
use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase, VideoFrame};
use ec_h264::H264Decoder;
use engine::PlaybackSession;
use engine::deinterlace::Deinterlacer;
use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::project::{Deinterlace, Lane, Speed};

const W: usize = 352;
const H: usize = 288;
/// Interlaced frames; the source has twice as many progressive moments.
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

#[derive(Clone, Copy)]
struct Look {
    tff: bool,
    deep: bool,
    c422: bool,
}

impl Look {
    fn pix(self) -> &'static str {
        match (self.c422, self.deep) {
            (false, false) => "yuv420p",
            (false, true) => "yuv420p10le",
            (true, false) => "yuv422p",
            (true, true) => "yuv422p10le",
        }
    }

    fn frame_bytes(self) -> usize {
        W * H * if self.c422 { 2 } else { 3 } / if self.c422 { 1 } else { 2 } * if self.deep { 2 } else { 1 }
    }

    fn parity(self) -> &'static str {
        if self.tff { "tff" } else { "bff" }
    }
}

/// The progressive original, every one of its 60 moments (raw), and the interlaced
/// clip `name`. Field `k` of the clip belongs to moment `k` whichever field leads.
fn make(dir: &Path, name: &str, look: Look) -> (PathBuf, PathBuf) {
    let src = dir.join(format!("{name}.src.y4m"));
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=50", "-frames:v", &(FRAMES * 2).to_string(), "-pix_fmt",
        if look.c422 { "yuv422p" } else { "yuv420p" }, src.to_str().unwrap(),
    ]);
    let reference = dir.join(format!("{name}.ref.yuv"));
    ffmpeg(&["-i", src.to_str().unwrap(), "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", look.pix(), reference.to_str().unwrap()]);
    let clip = dir.join(name);
    let scan = look.parity();
    let params = format!("interlaced=1:{scan}=1:keyint=10:min-keyint=10:bframes=2");
    let profile = match (look.c422, look.deep) {
        (true, _) => "high422",
        (false, true) => "high10",
        (false, false) => "high",
    };
    ffmpeg(&[
        "-i", src.to_str().unwrap(), "-vf", &format!("interlace=scan={scan}"), "-c:v", "libx264", "-profile:v", profile, "-pix_fmt",
        look.pix(), "-crf", "10", "-flags", "+ildct+ilme", "-x264-params", &params, clip.to_str().unwrap(),
    ]);
    (clip, reference)
}

fn progressive(dir: &Path, name: &str, fps: u32) -> PathBuf {
    let clip = dir.join(name);
    ffmpeg(&[
        "-f", "lavfi", "-i", &format!("testsrc2=size=352x288:rate={fps}"), "-frames:v", "30", "-c:v", "libx264", "-x264-params",
        "keyint=10:min-keyint=10:bframes=2", clip.to_str().unwrap(),
    ]);
    clip
}

fn woven(clip: &Path) -> Vec<VideoFrame> {
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
    pics.iter().flat_map(|p| p.planes.iter().flat_map(|pl| pl.data.iter().copied())).collect()
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

/// Share of luma pixels that are an extreme against both vertical neighbours.
fn combing(luma: &[u8]) -> f64 {
    let mut hits = 0u64;
    for y in 1..H - 1 {
        for x in 0..W {
            let (a, b, c) = (i32::from(luma[(y - 1) * W + x]), i32::from(luma[y * W + x]), i32::from(luma[(y + 1) * W + x]));
            if ((b - a) > 20 && (b - c) > 20) || ((a - b) > 20 && (c - b) > 20) {
                hits += 1;
            }
        }
    }
    hits as f64 / (W * (H - 2)) as f64
}

/// Mean combing of an 8-bit 4:2:0 raw run.
fn combing_of(raw: &[u8]) -> f64 {
    let frame = W * H * 3 / 2;
    let n = raw.len() / frame;
    raw.chunks(frame).map(|f| combing(&f[..W * H])).sum::<f64>() / n as f64
}

fn frames_of(raw: &[u8], look: Look) -> Vec<&[u8]> {
    raw.chunks(look.frame_bytes()).collect()
}

const LOOKS: [(&str, Look); 4] = [
    ("t8-tff.mp4", Look { tff: true, deep: false, c422: false }),
    ("t8-bff.mkv", Look { tff: false, deep: false, c422: false }),
    ("t10-tff.mp4", Look { tff: true, deep: true, c422: false }),
    ("t422-bff.mkv", Look { tff: false, deep: false, c422: true }),
];

fn field_stage(pics: Vec<VideoFrame>) -> Vec<(i64, VideoFrame)> {
    let mut stage = Deinterlacer::with_mode(Deinterlace::Field, true, 0, Interlace::Progressive);
    let mut out = Vec::new();
    for (i, pic) in pics.into_iter().enumerate() {
        out.extend(stage.push_all(i as i64, pic));
    }
    out.extend(stage.finish_all());
    out
}

fn frame_stage(pics: Vec<VideoFrame>) -> Vec<VideoFrame> {
    let mut stage = Deinterlacer::new(true, 0, Interlace::Progressive);
    let mut out = Vec::new();
    for (i, pic) in pics.into_iter().enumerate() {
        out.extend(stage.push(i as i64, pic).map(|(_, f)| f));
    }
    out.extend(stage.finish().map(|(_, f)| f));
    out
}

/// The numbers: ours, ffmpeg's send_field, and the progressive moments, per clip.
#[test]
fn field_mode_is_bwdif_send_field() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-bwdif");
    for (name, look) in LOOKS {
        let (clip, reference) = make(&dir, name, look);
        let want = std::fs::read(&reference).expect("reference");
        let pics = woven(&clip);
        let fields = field_stage(pics.clone());
        assert_eq!(fields.len(), 2 * FRAMES as usize, "{name}: one picture per field");
        assert_eq!(fields.iter().map(|f| f.0).collect::<Vec<_>>(), (0..2 * i64::from(FRAMES)).collect::<Vec<_>>(), "{name}");
        let ours = raw(&fields.iter().map(|f| f.1.clone()).collect::<Vec<_>>());
        let bwdif = ffmpeg_raw(&["-vf", &format!("bwdif=mode=send_field:parity={}:deint=all", look.parity())], &clip, look.pix());
        assert_eq!(bwdif.len(), ours.len(), "{name}: ffmpeg's send_field has the same number of pictures");
        let (po, pb, pd) = (psnr(&want, &ours, look.deep), psnr(&want, &bwdif, look.deep), psnr(&bwdif, &ours, look.deep));
        // Per picture, to see whether the second field is as close as the first.
        let (mut first, mut second) = (Vec::new(), Vec::new());
        for (i, (a, b)) in frames_of(&ours, look).iter().zip(frames_of(&bwdif, look)).enumerate() {
            (if i % 2 == 0 { &mut first } else { &mut second }).push(psnr(a, b, look.deep));
        }
        let mean = |v: &[f64]| v.iter().map(|x| x.min(99.0)).sum::<f64>() / v.len() as f64;
        eprintln!(
            "send_field {name}: vs original ours {po:.2} dB, bwdif {pb:.2} dB; ours vs bwdif {pd:.2} dB (first fields {:.2}, second fields {:.2})",
            mean(&first),
            mean(&second)
        );
        assert!(po > pb - 0.5, "{name}: ours {po:.2} dB is not within 0.5 dB of bwdif {pb:.2} dB");
        assert!(pd > 40.0, "{name}: ours is {pd:.2} dB from bwdif send_field");
        // The first field of every frame is exactly the frame-rate picture, so a
        // timeline at the frame rate shows the same thing in either mode.
        let frame_rate = frame_stage(pics);
        for (n, f) in frame_rate.iter().enumerate() {
            assert!(f == &fields[2 * n].1, "{name}: first field of frame {n} differs from Frame mode");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn off_is_the_plain_decode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-off");
    for (name, look) in LOOKS {
        let (clip, _) = make(&dir, name, look);
        let pics = woven(&clip);
        let mut stage = Deinterlacer::with_mode(Deinterlace::Off, true, 0, Interlace::Progressive);
        let mut out = Vec::new();
        for (i, pic) in pics.into_iter().enumerate() {
            let got = stage.push_all(i as i64, pic);
            assert_eq!(got.len(), 1, "{name}: no delay");
            out.extend(got.into_iter().map(|(_, f)| f));
        }
        assert!(stage.finish_all().is_empty());
        let plain = ffmpeg_raw(&[], &clip, look.pix());
        assert!(raw(&out) == plain, "{name}: Off differs from ffmpeg's plain decode");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn session(clip: &Path) -> PlaybackSession {
    let s = PlaybackSession::open(clip).unwrap_or_else(|e| panic!("open {}: {e}", clip.display()));
    s.set_gain(0.0);
    s
}

/// The default (`Frame`) is a no-op the session reports as unchanged.
fn set_mode(s: &mut PlaybackSession, mode: Deinterlace) {
    assert!(s.set_deinterlace(Lane::V1, 0, mode) || mode == Deinterlace::Frame, "{mode:?}");
}

fn export_session(session: &PlaybackSession, out: &Path) {
    let settings = ExportSettings { format: Format::Mp4, ..Default::default() };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
}

fn duration(path: &Path) -> f64 {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0.0)
}

/// Exports `clip` with `mode` on a timeline at `fps` (`None`: the clip's own) and
/// returns the decoded raw frames of the result.
fn exported(clip: &Path, out: &Path, mode: Deinterlace, fps: Option<f64>, reverse: bool, look: Look) -> Vec<u8> {
    let mut s = session(clip);
    set_mode(&mut s, mode);
    if let Some(fps) = fps {
        assert!(s.set_frame_rate(fps), "to {fps}");
    }
    if reverse {
        s.set_reverse(Lane::V1, 0, true).expect("reverse");
    }
    software(|| export_session(&s, out));
    ffmpeg_raw(&[], out, look.pix())
}

#[test]
fn a_50p_timeline_shows_every_field_of_a_25i_field_clip() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-50p");
    for (name, look) in [LOOKS[0], LOOKS[1]] {
        let (clip, reference) = make(&dir, name, look);
        let want = std::fs::read(&reference).expect("reference");
        let out = dir.join(format!("{name}-50p.mp4"));
        let decoded = exported(&clip, &out, Deinterlace::Field, Some(50.0), false, look);
        let frames = frames_of(&decoded, look);
        assert_eq!(frames.len(), 2 * FRAMES as usize, "{name}: twice the frames");
        let secs = duration(&out);
        assert!((secs - f64::from(FRAMES) / 25.0).abs() < 0.1, "{name}: {secs} s, want 1.2 s");
        // Motion continuity: each field's picture differs from the next, and the
        // content moves forward (closest to its own moment).
        for w in frames.windows(2) {
            assert!(w[0] != w[1], "{name}: two equal pictures in a row");
        }
        let bwdif = ffmpeg_raw(&["-vf", &format!("bwdif=mode=send_field:parity={}:deint=all", look.parity())], &clip, look.pix());
        let (pe, pb) = (psnr(&want, &decoded, false), psnr(&want, &bwdif, false));
        eprintln!("50p export {name}: {} frames, {secs:.3} s, {pe:.2} dB vs original (bwdif send_field {pb:.2} dB)", frames.len());
        assert!(pe > pb - 1.5, "{name}: exported {pe:.2} dB vs bwdif {pb:.2} dB");
        let reference_frames = frames_of(&want, look);
        for (k, f) in frames.iter().enumerate() {
            let own = psnr(reference_frames[k], f, false);
            let other = psnr(reference_frames[(k + 1).min(frames.len() - 1)], f, false);
            assert!(own >= other, "{name}: picture {k} is closer to moment {} than to its own", k + 1);
        }
        // Reverse plays the fields last to first.
        let back = dir.join(format!("{name}-50p-rev.mp4"));
        let rev = exported(&clip, &back, Deinterlace::Field, Some(50.0), true, look);
        let rframes = frames_of(&rev, look);
        assert_eq!(rframes.len(), frames.len(), "{name}: reverse keeps the count");
        for (d, f) in rframes.iter().enumerate() {
            let p = psnr(frames[frames.len() - 1 - d], f, false);
            assert!(p > 28.0, "{name}: reversed picture {d} is {p:.2} dB from field {}", frames.len() - 1 - d);
        }
        // Speed 2 on the 50p timeline: every first field, half the frames.
        let mut s = session(&clip);
        set_mode(&mut s, Deinterlace::Field);
        assert!(s.set_frame_rate(50.0));
        s.set_speed(Lane::V1, 0, Speed::from_permille(2000)).expect("speed");
        let fast = dir.join(format!("{name}-50p-x2.mp4"));
        software(|| export_session(&s, &fast));
        let fast_frames = ffmpeg_raw(&[], &fast, look.pix());
        assert_eq!(frames_of(&fast_frames, look).len(), FRAMES as usize, "{name}: speed 2 halves the frames");
        let fast_frames = frames_of(&fast_frames, look);
        for (k, f) in fast_frames.iter().enumerate() {
            let p = psnr(frames[2 * k], f, false);
            assert!(p > 28.0, "{name}: speed-2 picture {k} is {p:.2} dB from field {}", 2 * k);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_25p_timeline_shows_the_first_field_of_each_frame_like_frame_mode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-25p");
    for (name, look) in [LOOKS[0], LOOKS[1]] {
        let (clip, _) = make(&dir, name, look);
        let field = exported(&clip, &dir.join(format!("{name}-field.mp4")), Deinterlace::Field, None, false, look);
        let frame = exported(&clip, &dir.join(format!("{name}-frame.mp4")), Deinterlace::Frame, None, false, look);
        let off = exported(&clip, &dir.join(format!("{name}-off.mp4")), Deinterlace::Off, None, false, look);
        assert_eq!(frames_of(&field, look).len(), FRAMES as usize, "{name}");
        assert!(field == frame, "{name}: Field on a 25p timeline is not Frame's pictures");
        let (cf, co) = (combing_of(&frame), combing_of(&off));
        eprintln!("25p export {name}: combing frame {cf:.5}, off {co:.5}");
        assert!(co > cf * 2.0, "{name}: Off ({co:.5}) should keep the woven combing Frame ({cf:.5}) removes");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stills_follow_the_mode() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-still");
    let (name, look) = LOOKS[0];
    let (clip, _) = make(&dir, name, look);
    let still = |mode: Deinterlace, fps: Option<f64>, frame: u32| {
        let mut s = session(&clip);
        set_mode(&mut s, mode);
        if let Some(fps) = fps {
            assert!(s.set_frame_rate(fps));
        }
        software(|| s.compose_at(frame, W as u32, H as u32)).expect("compose_at").bgra
    };
    for n in [0u32, 1, 7, 10, 11, 29] {
        let frame = still(Deinterlace::Frame, None, n);
        assert!(still(Deinterlace::Field, None, n) == frame, "25p frame {n}: Field differs from Frame");
        let first = still(Deinterlace::Field, Some(50.0), 2 * n);
        let second = still(Deinterlace::Field, Some(50.0), 2 * n + 1);
        assert!(first == frame, "50p frame {}: not Frame mode's picture {n}", 2 * n);
        assert!(second != first, "50p frame {}: the second field repeats the first", 2 * n + 1);
        assert!(still(Deinterlace::Off, None, n) != frame, "25p frame {n}: Off equals Frame");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_progressive_source_ignores_the_setting() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-prog");
    let look = Look { tff: true, deep: false, c422: false };
    let clip = progressive(&dir, "p.mp4", 25);
    for fps in [None, Some(50.0)] {
        let base = exported(&clip, &dir.join("base.mp4"), Deinterlace::Frame, fps, false, look);
        for mode in [Deinterlace::Field, Deinterlace::Off] {
            let other = exported(&clip, &dir.join("other.mp4"), mode, fps, false, look);
            assert!(other == base, "{mode:?} at {fps:?} changed a progressive export");
        }
        let still = |mode: Deinterlace| {
            let mut s = session(&clip);
            set_mode(&mut s, mode);
            if let Some(fps) = fps {
                s.set_frame_rate(fps);
            }
            software(|| s.compose_at(5, W as u32, H as u32)).expect("compose_at").bgra
        };
        assert!(still(Deinterlace::Field) == still(Deinterlace::Frame), "{fps:?}: still");
        assert!(still(Deinterlace::Off) == still(Deinterlace::Frame), "{fps:?}: still");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Playback and scrub follow the mode. Needs the `playback.rs` change in the lane
/// report (the decode layer is field-ready, the session's rate mapping is not), so it
/// is ignored until that lands: `cargo test --test deinterlace_mode -- --ignored`.
#[test]
#[ignore = "needs the peer-owned playback.rs hunk from the lane report"]
fn playback_and_scrub_show_the_field_the_export_writes() {
    if !have_x264() {
        eprintln!("no ffmpeg with libx264: skipping");
        return;
    }
    let dir = scratch("dm-play");
    let (name, look) = LOOKS[0];
    let (clip, _) = make(&dir, name, look);
    let mut s = session(&clip);
    set_mode(&mut s, Deinterlace::Field);
    assert!(s.set_frame_rate(50.0));
    for k in [0u32, 1, 2, 5, 20, 31, 58, 59] {
        // Mid-GOP seeks and back-and-forth scrubs alike.
        s.seek((f64::from(k) + 0.25) / 50.0);
        let deadline = Instant::now() + Duration::from_secs(20);
        let shown = loop {
            if let Some(f) = software(|| s.try_frame()) {
                break f;
            }
            assert!(Instant::now() < deadline, "no frame at {k}");
            std::thread::sleep(Duration::from_millis(5));
        };
        let want = software(|| s.compose_at(k, W as u32, H as u32)).expect("compose_at");
        assert!(shown.bgra == want.bgra, "scrub to {k}: the viewer and the export disagree");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
