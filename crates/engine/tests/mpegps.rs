//! MPEG program streams (`.mpg`, `.mpeg`, `.vob`, a DVD's, `.m2v`) end to end: probe, decode,
//! seek, sound, a non-zero start, a timestamp wrap, byte-joined VOBs, the tracks that are
//! refused, and an export. The twin of `tests/mpegts.rs`.
//!
//! Every clip is synthetic (`testsrc2`, a flash on black, a chirp, a beep) muxed by ffmpeg
//! into a scratch directory; without ffmpeg and the encoders a test needs, it says so and
//! passes. `VE_SW` is process-wide, so tests that pin it hold [`SW`].
//!
//! ```text
//! cargo test -p engine --release --test mpegps
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_core::frame::Frame as CoreFrame;
use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
use ec_core::{Packet, TimeBase, VideoFrame};
use ec_h264::H264Decoder;
use engine::audio::AudioSession;
use engine::decode::DecodeSession;
use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::PlaybackSession;

static SW: Mutex<()> = Mutex::new(());

fn software<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let dir = std::env::temp_dir().join(format!("edith-mpegts-{name}-{}", std::process::id()));
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

const FPS: f64 = 25.0;
/// A moving test card, 2 s.
const CARD: &str = "testsrc2=size=352x288:rate=25:duration=2";
/// A chirp, which is never periodic, so a shifted sample shows.
const CHIRP: &str = "aevalsrc=exprs='0.5*sin(2*PI*(300+200*t)*t)|0.5*sin(2*PI*(310+200*t)*t)':s=48000:d=2";
/// Black with a white flash from 1.0 s to 1.2 s (frames 25..30), 2.4 s long.
const FLASH: &str = "color=c=black:s=352x288:r=25:d=2.4,drawbox=x=0:y=0:w=352:h=288:color=white:t=fill:enable='between(t,1,1.2)'";
/// A 1 kHz beep over the same 1.0 s to 1.2 s.
const BEEP: &str = "aevalsrc=exprs='0.8*sin(2*PI*1000*t)*between(t,1,1.2)|0.8*sin(2*PI*1000*t)*between(t,1,1.2)':s=48000:d=2.4";

fn ffprobe(path: &Path, entries: &str, select: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", select, "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(out.status.success(), "ffprobe reads {}", path.display());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn start_time(path: &Path, select: &str) -> f64 {
    ffprobe(path, "stream=start_time", select).lines().next().unwrap().trim_end_matches(',').parse().expect("a start time")
}

/// Every frame the session releases, as BGRA, in display order.
fn session_frames(path: &Path) -> Vec<engine::decode::Frame> {
    software(|| DecodeSession::open(path).expect("open").1.into_iter().collect())
}

/// Every picture ec-h264 releases for `clip`, in display order, as raw planes.
fn h264_planes(clip: &Path) -> Vec<u8> {
    let (_, mut demuxer) = Demuxer::open(clip).expect("open");
    demuxer.seek_to_sync_at_or_before(0);
    let mut decoder = H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder");
    let mut out = Vec::new();
    let take = |decoder: &mut H264Decoder, out: &mut Vec<u8>| {
        while let Ok(frame) = decoder.receive_frame() {
            if let CoreFrame::Video(VideoFrame { planes, .. }) = frame {
                for p in planes {
                    out.extend_from_slice(&p.data);
                }
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

fn ffmpeg_raw(path: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// The whole sound of `stream` from `from` seconds, up to `frames` frames, on the clip's clock.
fn sound(path: &Path, stream: usize, from: f64, frames: usize) -> (u32, usize, Vec<f32>) {
    let (meta, rx) = AudioSession::open_multi_streams(&[(path.to_path_buf(), stream)], &[(Some(0), from, f64::INFINITY)])
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

/// ffmpeg's decode of audio stream `index`, stereo f32 at `rate`.
fn ffmpeg_sound(path: &Path, index: usize, rate: u32) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", &format!("0:a:{index}"), "-vn", "-ac", "2", "-ar", &rate.to_string(), "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Our samples (`from` seconds in) against ffmpeg's decode, the offset of the sound from the
/// picture applied: sample `i` of the clip is sample `i - offset` of ffmpeg's. Returns the
/// largest difference.
fn compare(path: &Path, stream: usize, from: f64, frames: usize, reference: &[f32], offset: i64) -> f32 {
    let (rate, width, ours) = sound(path, stream, from, frames);
    assert_eq!(width, 2);
    let first = (from * f64::from(rate)).round() as i64;
    let mut worst = 0f32;
    let mut compared = 0;
    for frame in 0..ours.len() / 2 {
        let at = first + frame as i64 - offset;
        // Not the first stretch of the sound: a decoder's start-up is its own business.
        if at < 2048 {
            continue;
        }
        let Some(want) = reference.get(at as usize * 2..at as usize * 2 + 2) else { break };
        worst = worst.max((ours[frame * 2] - want[0]).abs()).max((ours[frame * 2 + 1] - want[1]).abs());
        compared += 1;
    }
    assert!(compared > frames / 2, "compared {compared} of {frames} frames");
    worst
}

fn sound_offset(path: &Path, index: usize, rate: u32) -> i64 {
    let (v, a) = (start_time(path, "v:0"), start_time(path, &format!("a:{index}")));
    ((a - v) * f64::from(rate)).round() as i64
}

/// Every codec's sound against ffmpeg's, from the top and from the middle, with the
fn first_loud(samples: &[f32], width: usize) -> Option<usize> {
    samples.chunks(width).position(|f| f.iter().any(|s| s.abs() > 0.2))
}

fn flash_frame(frames: &[engine::decode::Frame]) -> Option<u32> {
    frames
        .iter()
        .find(|f| f.bgra.chunks(4).map(|p| u64::from(p[1])).sum::<u64>() > 128 * (f.bgra.len() as u64 / 4))
        .map(|f| f.index)
}

/// The flash is at 1.0 s in the picture and the beep at 1.0 s in the sound, whatever the
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

/// An export of a transport stream re-encodes (there is no stream copy), keeps its length,
/// Every picture ec-mpeg2 releases for `clip`, in display order, as raw planes.
fn mpeg2_planes(clip: &Path) -> Vec<u8> {
    let (_, mut demuxer) = Demuxer::open(clip).expect("open");
    demuxer.seek_to_sync_at_or_before(0);
    let mut decoder = ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).expect("decoder");
    let mut out = Vec::new();
    let take = |decoder: &mut ec_mpeg2::Mpeg2Decoder, out: &mut Vec<u8>| {
        while let Ok(frame) = decoder.receive_frame() {
            if let CoreFrame::Video(VideoFrame { planes, .. }) = frame {
                for p in planes {
                    out.extend_from_slice(&p.data);
                }
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

fn ffmpeg_raw_as(path: &Path, pix_fmt: &str) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// A DVD frame: PAL 720x576.
const DVD_CARD: &str = "testsrc2=size=720x576:rate=25:duration=2";
const DVD_TFF: &str = "testsrc2=size=720x576:rate=50:duration=2,interlace=scan=tff";
const DVD_BFF: &str = "testsrc2=size=720x576:rate=50:duration=2,interlace=scan=bff";
const DVD_FLASH: &str = "color=c=black:s=720x576:r=25:d=2.4,drawbox=x=0:y=0:w=720:h=576:color=white:t=fill:enable='between(t,1,1.2)'";

/// MPEG-1 video, I/P/B, a GOP of ten.
const MPEG1: &[&str] = &["-c:v", "mpeg1video", "-b:v", "2M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p"];
/// MPEG-2 video, I/P/B, a GOP of ten.
const MPEG2: &[&str] = &["-c:v", "mpeg2video", "-b:v", "4M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p"];
/// The DVD's: interlaced, top field first.
const DVD_V_TFF: &[&str] = &["-c:v", "mpeg2video", "-b:v", "6M", "-g", "12", "-bf", "2", "-pix_fmt", "yuv420p", "-flags", "+ildct+ilme", "-top", "1"];
const DVD_V_BFF: &[&str] = &["-c:v", "mpeg2video", "-b:v", "6M", "-g", "12", "-bf", "2", "-pix_fmt", "yuv420p", "-flags", "+ildct+ilme", "-top", "0"];
const DVD_V: &[&str] = &["-c:v", "mpeg2video", "-b:v", "6M", "-g", "12", "-bf", "2", "-pix_fmt", "yuv420p"];
const X264: &[&str] = &["-c:v", "libx264", "-x264-params", "keyint=10:min-keyint=10:bframes=2", "-pix_fmt", "yuv420p", "-strict", "-2"];
const MP2: &[&str] = &["-c:a", "mp2", "-b:a", "192k"];
const MP3: &[&str] = &["-c:a", "libmp3lame", "-b:a", "128k"];
const AC3: &[&str] = &["-c:a", "ac3", "-b:a", "192k"];
const LPCM16: &[&str] = &["-c:a", "pcm_dvd", "-sample_fmt:a", "s16", "-ar:a", "48000"];
const LPCM24: &[&str] = &["-c:a", "pcm_dvd", "-sample_fmt:a", "s32", "-ar:a", "48000"];

/// What to mux.
struct Clip<'a> {
    name: &'a str,
    video: &'a str,
    audio: &'a [(&'a str, &'a [&'a str])],
    vcodec: &'a [&'a str],
    /// ffmpeg's muxer: `mpeg` (MPEG-1 system), `vob`, `dvd` (nav packs), or the raw
    /// `mpeg1video`/`mpeg2video`.
    fmt: &'a str,
    /// `-output_ts_offset`, seconds.
    offset: f64,
}

fn clip<'a>(name: &'a str, video: &'a str, audio: &'a [(&'a str, &'a [&'a str])], vcodec: &'a [&'a str], fmt: &'a str) -> Clip<'a> {
    Clip { name, video, audio, vcodec, fmt, offset: 0.0 }
}

/// Muxes `clip`; `None` when this ffmpeg cannot (a muxer or codec it lacks), which skips the case.
fn try_make(dir: &Path, clip: &Clip) -> Option<PathBuf> {
    let out = dir.join(clip.name);
    let mut args: Vec<String> = vec!["-v".into(), "error".into(), "-y".into(), "-f".into(), "lavfi".into(), "-i".into(), clip.video.into()];
    for (src, _) in clip.audio {
        args.extend(["-f".into(), "lavfi".into(), "-i".into(), (*src).into()]);
    }
    args.extend(["-map".into(), "0:v".into()]);
    args.extend(clip.vcodec.iter().map(|a| (*a).to_string()));
    for (i, (_, codec)) in clip.audio.iter().enumerate() {
        args.extend(["-map".into(), format!("{}:a", i + 1)]);
        let n = i.to_string();
        for pair in codec.chunks(2) {
            args.push(format!("{}:{n}", pair[0]));
            args.push(pair[1].to_string());
        }
    }
    args.extend(["-f".into(), clip.fmt.into()]);
    if clip.offset != 0.0 {
        args.extend(["-output_ts_offset".into(), clip.offset.to_string()]);
    }
    args.push(out.to_str().unwrap().into());
    let made = Command::new("ffmpeg").args(&args).output().expect("run ffmpeg");
    if !made.status.success() || !out.exists() {
        eprintln!("{}: ffmpeg cannot mux this here ({}): skipping", clip.name, String::from_utf8_lossy(&made.stderr).lines().last().unwrap_or(""));
        return None;
    }
    Some(out)
}

fn make(dir: &Path, clip: &Clip) -> PathBuf {
    try_make(dir, clip).unwrap_or_else(|| panic!("ffmpeg could not mux {}", clip.name))
}

// ------------------------------------------------------------------ video

/// The shapes a program stream comes in: (clip, interlaced, width, height).
fn shapes() -> Vec<(Clip<'static>, Option<&'static str>, u32, u32)> {
    let mut v = vec![
        (clip("a.mpg", CARD, &[(CHIRP, MP2)], MPEG1, "mpeg"), None, 352, 288),
        (clip("b.vob", CARD, &[(CHIRP, AC3)], MPEG2, "vob"), None, 352, 288),
        (clip("c.mpeg", CARD, &[(CHIRP, MP2)], MPEG2, "vob"), None, 352, 288),
        (clip("d.m2v", CARD, &[], MPEG2, "mpeg2video"), None, 352, 288),
        (clip("e.m1v", CARD, &[], MPEG1, "mpeg1video"), None, 352, 288),
        (clip("f.vob", DVD_TFF, &[(CHIRP, AC3)], DVD_V_TFF, "dvd"), Some("tff"), 720, 576),
        (clip("g.vob", DVD_BFF, &[(CHIRP, MP2)], DVD_V_BFF, "dvd"), Some("bff"), 720, 576),
        (clip("h.vob", DVD_CARD, &[(CHIRP, LPCM16)], DVD_V, "dvd"), None, 720, 576),
    ];
    let mut off = clip("i.mpg", CARD, &[(CHIRP, MP2)], MPEG1, "mpeg");
    off.offset = 10.0;
    v.push((off, None, 352, 288));
    v
}

/// Probe, frame count, and every picture against ffmpeg's, for each of the container's spellings.
#[test]
fn the_picture_opens_probes_and_decodes_as_ffmpeg_does() {
    if !have(&["mpeg1video", "mpeg2video", "mp2", "ac3", "pcm_dvd"]) {
        return;
    }
    let dir = scratch("video");
    for (spec, interlaced, width, height) in shapes() {
        let Some(path) = try_make(&dir, &spec) else { continue };
        let facts = engine::probe_media(&path);
        let video = facts.video.as_ref().unwrap_or_else(|| panic!("{}: no picture", spec.name));
        assert_eq!((video.width, video.height), (width, height), "{}", spec.name);
        assert_eq!(video.codec, engine::demux::Codec::Mpeg2, "{}", spec.name);
        assert!((video.frame_rate.unwrap() - FPS).abs() < 0.01, "{}: {:?}", spec.name, video.frame_rate);
        assert_eq!(video.interlaced, interlaced, "{}", spec.name);
        let secs = facts.duration_secs.expect("a length");
        assert!((secs - 2.0).abs() <= 1.0 / FPS, "{}: {secs} s", spec.name);
        let name = facts.container.clone().unwrap_or_default();
        assert!(["mpeg", "mpegvideo", "vob"].contains(&name.as_str()), "{}: container {name}", spec.name);
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.frame_count, 50, "{}", spec.name);
        let want = ffmpeg_raw_as(&path, "yuv420p");
        let got = mpeg2_planes(&path);
        assert!(got == want, "{}: pictures differ from ffmpeg's ({} vs {} bytes)", spec.name, got.len(), want.len());
        let frames = session_frames(&path);
        assert_eq!(frames.iter().map(|f| f.index).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>(), "{}", spec.name);
        if spec.audio.is_empty() {
            continue;
        }
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), 1, "{}: {streams:?}", spec.name);
        assert!(streams[0].decodable && streams[0].sample_rate == 48_000, "{}: {streams:?}", spec.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seek into any frame, GOP boundary or not, interlaced (deinterlaced against its
/// neighbours) or not, is the sequential decode's frame.
#[test]
fn mid_gop_seeks_equal_a_sequential_decode() {
    if !have(&["mpeg1video", "mpeg2video", "mp2", "ac3"]) {
        return;
    }
    let dir = scratch("seek");
    for (spec, ..) in shapes() {
        let Some(path) = try_make(&dir, &spec) else { continue };
        let all = session_frames(&path);
        assert_eq!(all.len(), 50, "{}", spec.name);
        for target in [0u32, 1, 2, 5, 9, 10, 11, 13, 17, 20, 25, 33, 41, 49] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{}: no frame after seek to {target}", spec.name))
            });
            assert_eq!(seeked.index, target, "{}", spec.name);
            assert!(seeked.bgra == all[target as usize].bgra, "{}: seek to {target} differs from sequential", spec.name);
        }
        let range = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_range(&path, 12, 31).expect("open_range");
            rx.into_iter().collect::<Vec<_>>()
        });
        assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (12..31).collect::<Vec<_>>(), "{}", spec.name);
        for f in &range {
            assert!(f.bgra == all[f.index as usize].bgra, "{}: range frame {} differs", spec.name, f.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// H.264 in a program stream (ffmpeg writes it for `-f vob`/`-f mpeg` with `-strict -2`): probe, every picture
/// against ffmpeg's, and a seek equal to the sequential decode.
#[test]
fn h264_in_a_program_stream_opens_and_seeks() {
    if !have(&["libx264", "ac3"]) {
        return;
    }
    let dir = scratch("h264");
    for (name, fmt, audio) in [("h.vob", "vob", AC3), ("h.mpg", "mpeg", MP2)] {
        let Some(path) = try_make(&dir, &clip(name, CARD, &[(CHIRP, audio)], X264, fmt)) else { continue };
        let facts = engine::probe_media(&path);
        let Some(video) = facts.video.as_ref() else {
            eprintln!("{name}: ffmpeg wrote H.264 in a form the demuxer does not take as a picture: skipping");
            continue;
        };
        assert_eq!(video.codec, engine::demux::Codec::H264, "{name}");
        assert_eq!((video.width, video.height), (352, 288), "{name}");
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.frame_count, 50, "{name}");
        let want = ffmpeg_raw(&path);
        let got = h264_planes(&path);
        assert!(got == want, "{name}: pictures differ from ffmpeg's ({} vs {} bytes)", got.len(), want.len());
        let all = session_frames(&path);
        assert_eq!(all.len(), 50, "{name}");
        for target in [0u32, 3, 10, 14, 25, 37, 49] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{name}: no frame after seek to {target}"))
            });
            assert_eq!(seeked.index, target, "{name}");
            assert!(seeked.bgra == all[target as usize].bgra, "{name}: seek to {target} differs from sequential");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An interlaced DVD picture is deinterlaced; a progressive one is not.
#[test]
fn an_interlaced_dvd_picture_is_deinterlaced() {
    if !have(&["mpeg2video", "ac3"]) {
        return;
    }
    let dir = scratch("deint");
    let tff = make(&dir, &clip("tff.vob", DVD_TFF, &[], DVD_V_TFF, "dvd"));
    let woven = ffmpeg_raw_as(&tff, "yuv420p");
    let frames = session_frames(&tff);
    let (w, h) = (720usize, 576usize);
    let size = w * h;
    let luma_comb = |y: &[u8]| -> u64 { y.chunks(w).collect::<Vec<_>>().windows(2).map(|p| p[0].iter().zip(p[1]).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>()).sum() };
    let bgra_comb = |f: &engine::decode::Frame| -> u64 {
        f.bgra.chunks(w * 4).collect::<Vec<_>>().windows(2).map(|p| p[0].iter().zip(p[1]).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>()).sum()
    };
    let (mut woven_total, mut ours_total) = (0u64, 0u64);
    for n in 5..45 {
        woven_total += luma_comb(&woven[n * size * 3 / 2..n * size * 3 / 2 + size]);
        ours_total += bgra_comb(&frames[n]);
    }
    let (woven_avg, ours_avg) = (woven_total as f64 / size as f64, ours_total as f64 / (size as f64 * 4.0));
    eprintln!("row-to-row difference: woven {woven_avg:.1}, deinterlaced {ours_avg:.1}");
    assert!(woven_total > 0 && ours_avg < woven_avg, "the interlaced picture is not deinterlaced");
    let _ = h;
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ sound

/// Where the stereo run `cut`, which should sit at frame `at` of `whole`, really sits: the
/// shift in frames (within 1200) with the smallest largest difference, and that difference.
fn best_shift(cut: &[f32], whole: &[f32], at: usize) -> (i64, f32) {
    let mut best = (0i64, f32::MAX);
    for shift in -1200i64..=1200 {
        let start = at as i64 + shift;
        if start < 0 || (start as usize) * 2 + cut.len() > whole.len() {
            continue;
        }
        let start = start as usize * 2;
        let d = cut.iter().zip(&whole[start..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        if d < best.1 {
            best = (shift, d);
        }
    }
    best
}

/// [`best_shift`] of a read from `from` seconds against ffmpeg's decode, the sound's offset from the picture applied.
fn lpcm_shift(path: &Path, stream: usize, from: f64, reference: &[f32], offset: i64) -> (i64, f32) {
    let (rate, _, ours) = sound(path, stream, from, 2_000);
    let at = (from * f64::from(rate)).round() as i64 - offset;
    best_shift(&ours, reference, at.max(0) as usize)
}

/// Every codec's sound against ffmpeg's, from the top and from the middle, with the
/// picture-relative start of the sound kept.
#[test]
fn the_sound_is_ffmpegs_at_every_time_and_keeps_its_offset_from_the_picture() {
    if !have(&["mpeg1video", "mpeg2video", "mp2", "ac3", "libmp3lame", "pcm_dvd"]) {
        return;
    }
    let dir = scratch("sound");
    let mut cases = vec![
        (clip("mp2.mpg", CARD, &[(CHIRP, MP2)], MPEG1, "mpeg"), 1.5e-3f32),
        (clip("mp3.mpg", CARD, &[(CHIRP, MP3)], MPEG1, "mpeg"), 1.5e-3),
        (clip("ac3.vob", CARD, &[(CHIRP, AC3)], MPEG2, "vob"), 1.5e-3),
        (clip("mp2.vob", CARD, &[(CHIRP, MP2)], MPEG2, "vob"), 1.5e-3),
        (clip("dvd-ac3.vob", DVD_CARD, &[(CHIRP, AC3)], DVD_V, "dvd"), 1.5e-3),
        (clip("lpcm16.vob", DVD_CARD, &[(CHIRP, LPCM16)], DVD_V, "dvd"), 1e-6),
        (clip("lpcm24.vob", DVD_CARD, &[(CHIRP, LPCM24)], DVD_V, "dvd"), 1e-5),
        (clip("two.vob", CARD, &[(CHIRP, AC3), (CHIRP, MP2)], MPEG2, "vob"), 1.5e-3),
    ];
    let mut off = clip("off.mpg", CARD, &[(CHIRP, MP2)], MPEG1, "mpeg");
    off.offset = 10.0;
    cases.push((off, 1.5e-3));
    for (spec, tolerance) in &cases {
        let Some(path) = try_make(&dir, spec) else { continue };
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), spec.audio.len(), "{}", spec.name);
        for (index, row) in streams.iter().enumerate() {
            assert!(row.decodable, "{}: {row:?}", spec.name);
            assert_eq!(row.channels, 2, "{}", spec.name);
            assert_eq!(row.sample_rate, 48_000, "{}", spec.name);
            let reference = ffmpeg_sound(&path, index, 48_000);
            let offset = sound_offset(&path, index, 48_000);
            // DVD LPCM carries a time stamp per packet that an authoring tool, ffmpeg's muxer
            // included, sets to within one packet (503 samples) of where the packet's samples
            // truly are; a read from the top counts samples and is exact, a seek trusts the stamp.
            let lpcm = row.codec == "pcm_dvd";
            for from in [0.0, 0.4, 1.1, 1.7] {
                if lpcm && from > 0.0 {
                    let (shift, worst) = lpcm_shift(&path, index, from, &reference, offset);
                    eprintln!("{} stream {index} from {from}s: seek lands {shift} samples off (difference {worst:.6})", spec.name);
                    assert!(shift.abs() <= 520 && worst <= *tolerance, "{} from {from}s: shift {shift}, difference {worst}", spec.name);
                    continue;
                }
                let worst = compare(&path, index, from, 7_200, &reference, offset);
                eprintln!("{} stream {index} ({}) from {from}s: worst difference {worst:.6}", spec.name, row.codec);
                assert!(worst <= *tolerance, "{} stream {index} from {from}s: {worst}", spec.name);
            }
            // A seek is the sequential decode's samples.
            let (_, _, full) = sound(&path, index, 0.0, 96_000 * 2);
            let (_, _, cut) = sound(&path, index, 1.3, 4_800);
            let at = (1.3 * 48_000.0) as usize * 2;
            if lpcm {
                let (shift, worst) = best_shift(&cut, &full, at / 2);
                assert!(shift.abs() <= 520 && worst <= 1e-6, "{} stream {index}: a seek is {shift} samples off ({worst})", spec.name);
                continue;
            }
            let worst = cut.iter().zip(&full[at..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(worst <= 1e-4, "{} stream {index}: a seek differs from sequential by {worst}", spec.name);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ sync

/// The flash is at 1.0 s in the picture and the beep at 1.0 s in the sound, whatever the
/// stream's own clock says: a start at 10 s, one near the 33-bit wrap, a wrap in the middle.
#[test]
fn picture_and_sound_stay_in_sync_from_any_start() {
    if !have(&["mpeg1video", "mpeg2video", "mp2", "ac3", "pcm_dvd"]) {
        return;
    }
    let dir = scratch("sync");
    let wrap = (1u64 << 33) as f64 / 90_000.0;
    let mut cases = vec![
        (clip("zero.mpg", FLASH, &[(BEEP, MP2)], MPEG1, "mpeg"), 0.0),
        (clip("ten.mpg", FLASH, &[(BEEP, MP2)], MPEG1, "mpeg"), 10.0),
        (clip("ten.vob", FLASH, &[(BEEP, AC3)], MPEG2, "vob"), 10.0),
        (clip("near-wrap.vob", FLASH, &[(BEEP, MP2)], MPEG2, "vob"), wrap - 2.0),
        (clip("wrap.vob", FLASH, &[(BEEP, AC3)], MPEG2, "vob"), wrap - 1.1),
        (clip("dvd-lpcm.vob", DVD_FLASH, &[(BEEP, LPCM16)], DVD_V, "dvd"), 0.0),
        (clip("dvd-ac3.vob", DVD_FLASH, &[(BEEP, AC3)], DVD_V, "dvd"), 0.0),
        (clip("dvd-wrap.vob", DVD_FLASH, &[(BEEP, AC3)], DVD_V, "dvd"), wrap - 1.1),
    ];
    for (spec, offset) in &mut cases {
        spec.offset = *offset;
    }
    for (spec, _) in &cases {
        let Some(path) = try_make(&dir, spec) else { continue };
        let frames = session_frames(&path);
        assert_eq!(frames.len(), 60, "{}: {} frames", spec.name, frames.len());
        let flash = flash_frame(&frames).expect("a flash");
        assert_eq!(flash, 25, "{}: the flash is at 1.0 s of the clip", spec.name);
        let (rate, width, all) = sound(&path, 0, 0.0, 48_000 * 3);
        let beep = first_loud(&all, width).expect("a beep") as f64 / f64::from(rate);
        eprintln!("{}: flash at {:.3} s, beep at {beep:.3} s", spec.name, f64::from(flash) / FPS);
        assert!(beep > 0.99 && beep < 1.0 + 0.065, "{}: the beep is at {beep:.3} s", spec.name);
        // After a seek the beep is still where it was.
        let (_, _, tail) = sound(&path, 0, 0.8, 48_000);
        let again = 0.8 + first_loud(&tail, width).expect("a beep after a seek") as f64 / f64::from(rate);
        // DVD LPCM stamps are good to one packet (see the sound test).
        let slack = if spec.name.contains("lpcm") { 0.015 } else { 0.002 };
        assert!((again - beep).abs() < slack, "{}: {again} vs {beep}", spec.name);
        let (_, rx, _) = software(|| DecodeSession::open_at(&path, 25).map(|(m, rx, c)| (m, rx.into_iter().next(), c))).expect("seek");
        assert_eq!(rx.map(|f| f.index), Some(25), "{}", spec.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ joined VOBs

/// Two VOBs joined by bytes (the clock of the second starts over): the file opens, plays as
/// the two clips one after the other, and a seek into the second equals the sequential decode.
#[test]
fn two_vobs_joined_by_bytes_play_one_after_the_other() {
    if !have(&["mpeg2video", "ac3"]) {
        return;
    }
    let dir = scratch("joined");
    let one = make(&dir, &clip("one.vob", FLASH, &[(BEEP, AC3)], MPEG2, "vob"));
    let joined = dir.join("joined.vob");
    let mut bytes = std::fs::read(&one).unwrap();
    bytes.extend(std::fs::read(&one).unwrap());
    std::fs::write(&joined, bytes).unwrap();
    let facts = engine::probe_media(&joined);
    let secs = facts.duration_secs.expect("a length");
    // The timeline is continuous over the join: the length is the sum of the two clips.
    assert!((secs - 4.8).abs() < 0.03, "{secs} s");
    let all = session_frames(&joined);
    assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..all.len() as u32).collect::<Vec<_>>());
    // Sequential play is exact: the two clips one after the other, the flash at 1.0 s of each.
    assert_eq!(all.len(), 120);
    assert_eq!(flash_frame(&all), Some(25));
    let second = flash_frame(&all[60..]).expect("a second flash");
    assert_eq!(second, 85);
    // A seek on either side of the join is the sequential frame.
    for target in [5u32, 33, 59, 60, 66, 70, 80, 119] {
        let seeked = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_at(&joined, target).expect("open_at");
            rx.into_iter().next().expect("a frame")
        });
        assert_eq!(seeked.index, target);
        assert!(seeked.bgra == all[target as usize].bgra, "seek to {target} differs from sequential");
    }
    let (_, _, beeps) = sound(&joined, 0, 0.0, 48_000 * 4);
    let loud = beeps.chunks(2).filter(|f| f[0].abs() > 0.2).count();
    assert!(loud > 2 * 8_000, "both beeps are heard ({loud} loud samples)");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ refusals

/// DTS stays refused, by name and with the conversion, and the picture still opens.
#[test]
fn a_dts_track_is_refused_by_name_and_the_picture_still_opens() {
    if !have(&["mpeg2video"]) {
        return;
    }
    let dir = scratch("refused");
    let dts: &[&str] = &["-c:a", "dca", "-strict", "-2", "-ac", "2"];
    let Some(path) = try_make(&dir, &clip("dts.vob", CARD, &[(CHIRP, dts)], MPEG2, "vob")) else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let (meta, _) = Demuxer::open(&path).expect("the picture opens");
    assert_eq!(meta.frame_count, 50);
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1, "{streams:?}");
    assert!(!streams[0].decodable && streams[0].codec == "dts", "{:?}", streams[0]);
    assert!(AudioSession::open(&path).expect("a silent source, not an error").is_none());
    let why = AudioSession::unsupported(&path).expect("header").expect("a reason");
    assert!(why.to_lowercase().contains("dts") && why.contains("ffmpeg"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program stream with sound only is no picture: the open error says so.
#[test]
fn a_program_stream_without_a_picture_says_so() {
    if !have(&["mp2"]) {
        return;
    }
    let dir = scratch("nopic");
    let path = dir.join("sound.vob");
    ffmpeg(&["-f", "lavfi", "-i", CHIRP, "-c:a", "mp2", "-f", "vob", s(&path)]);
    let err = Demuxer::open(&path).err().expect("no picture").to_string();
    assert!(err.to_lowercase().contains("video"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ names

/// A program stream is recognised by its bytes under any name, and `.mpg` stays an audio file
/// when it is one.
#[test]
fn a_program_stream_is_sniffed_whatever_it_is_called() {
    if !have(&["mpeg1video", "mp2", "mpeg2video"]) {
        return;
    }
    let dir = scratch("sniff");
    let ps = make(&dir, &clip("x.mpg", CARD, &[(CHIRP, MP2)], MPEG1, "mpeg"));
    let m2v = make(&dir, &clip("x.m2v", CARD, &[], MPEG2, "mpeg2video"));
    for (src, name) in [(&ps, "recording.dat"), (&ps, "clip.mod"), (&ps, "clip.TOD"), (&ps, "clip.m2p"), (&ps, "noext"), (&m2v, "video.dat"), (&m2v, "video.mpv")] {
        let odd = dir.join(name);
        std::fs::copy(src, &odd).unwrap();
        assert!(engine::demux::is_mpegps(&odd), "{name}");
        assert!(!engine::demux::is_mpegts(&odd), "{name}");
        assert_eq!(Demuxer::open(&odd).expect("opens by content").0.frame_count, 50, "{name}");
    }
    // A program stream with a picture is no audio file whatever `.mpg` says; the audio stream is.
    assert!(!engine::is_audio(&ps));
    let song = dir.join("song.mpg");
    ffmpeg(&["-f", "lavfi", "-i", CHIRP, "-c:a", "mp2", "-f", "mp2", s(&song)]);
    assert!(engine::is_audio(&song) && !engine::demux::is_mpegps(&song));
    // A tracker module is not a program stream, nor is text, nor a missing file.
    let module = dir.join("tune.mod");
    std::fs::write(&module, b"Extended Module: not an MPEG stream at all\0\0\0\0").unwrap();
    assert!(!engine::demux::is_mpegps(&module));
    assert!(!engine::demux::is_mpegps(Path::new("/nonexistent/song.mp3")));
    assert!(engine::is_audio(Path::new("/nonexistent/k.mpg")));
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ export

/// An export of a program stream re-encodes (there is no stream copy), keeps its length, and
/// keeps the flash and the beep together.
#[test]
fn an_export_from_a_program_stream_keeps_its_length_and_its_sync() {
    if !have(&["mpeg1video", "mpeg2video", "libx264", "aac", "ac3", "mp2", "pcm_dvd"]) {
        return;
    }
    let dir = scratch("export");
    let mut cases = vec![
        clip("e.mpg", FLASH, &[(BEEP, MP2)], MPEG1, "mpeg"),
        clip("e.vob", FLASH, &[(BEEP, AC3)], MPEG2, "vob"),
        clip("e-off.vob", FLASH, &[(BEEP, AC3)], MPEG2, "vob"),
        clip("e-dvd.vob", DVD_FLASH, &[(BEEP, LPCM16)], DVD_V, "dvd"),
    ];
    cases[2].offset = 10.0;
    for spec in &cases {
        let Some(path) = try_make(&dir, spec) else { continue };
        let name = spec.name;
        let out = dir.join(format!("{name}.mp4"));
        software(|| export_as(&path, &out, Format::Mp4));
        let secs: f64 = ffprobe(&out, "format=duration", "v:0").parse().unwrap_or(0.0);
        assert!((secs - 2.4).abs() < 0.1, "{name}: {secs} s");
        let video = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-an", "-vf", "scale=8:8,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "-"])
            .output()
            .expect("ffmpeg");
        let flash = video.stdout.chunks(64).position(|f| f.iter().map(|&b| u32::from(b)).sum::<u32>() > 128 * 64).expect("a flash");
        let pcm = ffmpeg_sound(&out, 0, 48_000);
        let beep = pcm.chunks(2).position(|f| f.iter().any(|s| s.abs() > 0.2)).expect("a beep") as f64 / 48_000.0;
        let flash_at = flash as f64 / FPS;
        eprintln!("{name}: exported flash at {flash_at:.3} s, beep at {beep:.3} s");
        assert!((flash_at - 1.0).abs() < 0.045, "{name}: flash at {flash_at}");
        assert!((beep - flash_at).abs() < 0.09, "{name}: beep {beep} vs flash {flash_at}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
