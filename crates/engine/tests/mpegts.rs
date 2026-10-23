//! MPEG transport streams (`.ts`, `.mts`, `.m2ts`) end to end: probe, decode, seek, sound,
//! a non-zero start, a timestamp wrap, the tracks that are refused, and an export.
//!
//! Every clip is synthetic (`testsrc2`, a flash on black, a chirp, a beep) muxed by ffmpeg
//! into a scratch directory; without ffmpeg and the encoders a test needs, it says so and
//! passes, as `interlaced_h264` does. `VE_SW` is process-wide, so tests that pin it hold [`SW`].
//!
//! ```text
//! cargo test -p engine --release --test mpegts
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

/// What to mux.
struct Clip<'a> {
    name: &'a str,
    video: &'a str,
    audio: &'a [(&'a str, &'a [&'a str])],
    vcodec: &'a [&'a str],
    /// `-mpegts_m2ts_mode 1`: the 192-byte packets of AVCHD and Blu-ray.
    m2ts: bool,
    /// `-output_ts_offset`, seconds.
    offset: f64,
}

const X264: &[&str] = &["-c:v", "libx264", "-x264-params", "keyint=10:min-keyint=10:bframes=2", "-pix_fmt", "yuv420p"];
const AAC: &[&str] = &["-c:a", "aac", "-b:a", "128k"];
const AC3: &[&str] = &["-c:a", "ac3", "-b:a", "192k"];

fn make(dir: &Path, clip: &Clip) -> PathBuf {
    let out = dir.join(clip.name);
    let mut args: Vec<String> = vec!["-f".into(), "lavfi".into(), "-i".into(), clip.video.into()];
    for (src, _) in clip.audio {
        args.extend(["-f".into(), "lavfi".into(), "-i".into(), (*src).into()]);
    }
    args.extend(["-map".into(), "0:v".into()]);
    args.extend(clip.vcodec.iter().map(|a| (*a).to_string()));
    for (i, (_, codec)) in clip.audio.iter().enumerate() {
        args.extend(["-map".into(), format!("{}:a", i + 1)]);
        let n = i.to_string();
        // Codec options are per stream: `-c:a` becomes `-c:a:N`.
        for pair in codec.chunks(2) {
            args.push(format!("{}:{n}", pair[0]));
            args.push(pair[1].to_string());
        }
    }
    args.extend(["-f".into(), "mpegts".into()]);
    if clip.m2ts {
        args.extend(["-mpegts_m2ts_mode".into(), "1".into()]);
    }
    if clip.offset != 0.0 {
        args.extend(["-output_ts_offset".into(), clip.offset.to_string()]);
    }
    args.push(out.to_str().unwrap().into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ffmpeg(&args);
    out
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

fn start_time(path: &Path, select: &str) -> f64 {
    ffprobe(path, "stream=start_time", select).lines().next().unwrap().trim_end_matches(',').parse().expect("a start time")
}

// ------------------------------------------------------------------ video

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

/// The same file remuxed to mp4 without touching a bit of the picture.
fn to_mp4(dir: &Path, clip: &Path) -> PathBuf {
    let out = dir.join(format!("{}.remux.mp4", clip.file_name().unwrap().to_string_lossy()));
    ffmpeg(&["-i", s(clip), "-an", "-c:v", "copy", s(&out)]);
    out
}

/// Probe, frame count, every picture against ffmpeg's, and the same pictures as the mp4
/// remux of the stream gives, for each of the container's spellings.
#[test]
fn the_picture_opens_probes_and_decodes_as_ffmpeg_does() {
    if !have(&["libx264", "aac", "ac3"]) {
        return;
    }
    let dir = scratch("video");
    let cases = [
        Clip { name: "a.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: false, offset: 0.0 },
        Clip { name: "b.m2ts", video: CARD, audio: &[(CHIRP, AC3)], vcodec: X264, m2ts: true, offset: 0.0 },
        Clip { name: "c.mts", video: CARD, audio: &[(CHIRP, AC3)], vcodec: X264, m2ts: true, offset: 0.0 },
        Clip { name: "d.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: false, offset: 10.0 },
        Clip { name: "e.ts", video: CARD, audio: &[], vcodec: X264, m2ts: false, offset: 0.0 },
    ];
    for clip in &cases {
        let path = make(&dir, clip);
        let facts = engine::probe_media(&path);
        let video = facts.video.as_ref().unwrap_or_else(|| panic!("{}: no picture", clip.name));
        assert_eq!((video.width, video.height), (352, 288), "{}", clip.name);
        assert_eq!(video.codec, engine::demux::Codec::H264, "{}", clip.name);
        assert!((video.frame_rate.unwrap() - FPS).abs() < 0.01, "{}: {:?}", clip.name, video.frame_rate);
        assert_eq!(video.interlaced, None, "{}", clip.name);
        let secs = facts.duration_secs.expect("a length");
        assert!((secs - 2.0).abs() <= 1.0 / FPS, "{}: {secs} s", clip.name);
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.frame_count, 50, "{}", clip.name);
        let want = ffmpeg_raw(&path);
        let got = h264_planes(&path);
        assert!(got == want, "{}: pictures differ from ffmpeg's ({} vs {} bytes)", clip.name, got.len(), want.len());
        assert_eq!(session_frames(&path).len(), 50, "{}", clip.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seek into any frame, GOP boundary or not, is the sequential decode's frame.
#[test]
fn mid_gop_seeks_equal_a_sequential_decode() {
    if !have(&["libx264", "libx265", "aac"]) {
        return;
    }
    let dir = scratch("seek");
    let x265: &[&str] = &["-c:v", "libx265", "-x265-params", "keyint=10:min-keyint=10:bframes=2:log-level=error", "-pix_fmt", "yuv420p"];
    let cases = [
        Clip { name: "s.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: false, offset: 0.0 },
        Clip { name: "s.m2ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: true, offset: 3.0 },
        Clip { name: "h.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: x265, m2ts: false, offset: 0.0 },
        Clip { name: "h-off.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: x265, m2ts: false, offset: 10.0 },
    ];
    for clip in &cases {
        let path = make(&dir, clip);
        let all = session_frames(&path);
        assert_eq!(all.len(), 50, "{}", clip.name);
        assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>(), "{}", clip.name);
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.codec, if clip.name.starts_with('h') { engine::demux::Codec::Hevc } else { engine::demux::Codec::H264 });
        for target in [0u32, 1, 2, 5, 9, 10, 11, 13, 17, 20, 25, 33, 41, 49] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{}: no frame after seek to {target}", clip.name))
            });
            assert_eq!(seeked.index, target, "{}", clip.name);
            assert!(seeked.bgra == all[target as usize].bgra, "{}: seek to {target} differs from sequential", clip.name);
        }
        let range = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_range(&path, 12, 31).expect("open_range");
            rx.into_iter().collect::<Vec<_>>()
        });
        assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (12..31).collect::<Vec<_>>(), "{}", clip.name);
        for f in &range {
            assert!(f.bgra == all[f.index as usize].bgra, "{}: range frame {} differs", clip.name, f.index);
        }
        // The same pictures as the stream remuxed into an mp4.
        let mp4 = to_mp4(&dir, &path);
        let reference = session_frames(&mp4);
        assert_eq!(reference.len(), all.len(), "{}", clip.name);
        for (a, b) in all.iter().zip(&reference) {
            assert!(a.bgra == b.bgra, "{}: frame {} differs from the mp4 remux", clip.name, a.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ sound

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
/// picture-relative start of the sound kept.
#[test]
fn the_sound_is_ffmpegs_at_every_time_and_keeps_its_offset_from_the_picture() {
    if !have(&["libx264", "aac", "ac3", "libmp3lame", "pcm_bluray", "mp2", "mpeg2video"]) {
        return;
    }
    let dir = scratch("sound");
    let mp3: &[&str] = &["-c:a", "libmp3lame", "-b:a", "128k"];
    let lpcm: &[&str] = &["-c:a", "pcm_bluray"];
    let cases = [
        (Clip { name: "aac.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: false, offset: 0.0 }, 1.5e-3f32),
        (Clip { name: "aac-off.ts", video: CARD, audio: &[(CHIRP, AAC)], vcodec: X264, m2ts: false, offset: 10.0 }, 1.5e-3),
        (Clip { name: "ac3.m2ts", video: CARD, audio: &[(CHIRP, AC3)], vcodec: X264, m2ts: true, offset: 0.0 }, 1.5e-3),
        (Clip { name: "mp3.ts", video: CARD, audio: &[(CHIRP, mp3)], vcodec: X264, m2ts: false, offset: 0.0 }, 1.5e-3),
        (Clip { name: "mp2.ts", video: CARD, audio: &[(CHIRP, MP2)], vcodec: X264, m2ts: false, offset: 0.0 }, 1.5e-3),
        (Clip { name: "dvb.ts", video: CARD, audio: &[(CHIRP, MP2)], vcodec: MPEG2, m2ts: false, offset: 10.0 }, 1.5e-3),
        (Clip { name: "lpcm.m2ts", video: CARD, audio: &[(CHIRP, lpcm)], vcodec: X264, m2ts: true, offset: 0.0 }, 1e-6),
        (Clip { name: "two.mts", video: CARD, audio: &[(CHIRP, AC3), (CHIRP, lpcm)], vcodec: X264, m2ts: true, offset: 0.0 }, 1.5e-3),
    ];
    for (clip, tolerance) in &cases {
        let path = make(&dir, clip);
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), clip.audio.len(), "{}", clip.name);
        for (index, row) in streams.iter().enumerate() {
            assert!(row.decodable, "{}: {row:?}", clip.name);
            assert_eq!(row.channels, 2, "{}", clip.name);
            assert_eq!(row.sample_rate, 48_000, "{}", clip.name);
            let reference = ffmpeg_sound(&path, index, 48_000);
            let offset = sound_offset(&path, index, 48_000);
            for from in [0.0, 0.4, 1.1, 1.7] {
                let worst = compare(&path, index, from, 7_200, &reference, offset);
                eprintln!("{} stream {index} ({}) from {from}s: worst difference {worst:.6}", clip.name, row.codec);
                assert!(worst <= *tolerance, "{} stream {index} from {from}s: {worst}", clip.name);
            }
            // A seek is the sequential decode's samples.
            let (_, _, full) = sound(&path, index, 0.0, 96_000 * 2);
            let (_, _, cut) = sound(&path, index, 1.3, 4_800);
            let at = (1.3 * 48_000.0) as usize * 2;
            let worst = cut.iter().zip(&full[at..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(worst <= 1e-4, "{} stream {index}: a seek differs from sequential by {worst}", clip.name);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
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

/// The flash is at 1.0 s in the picture and the beep at 1.0 s in the sound, whatever the
/// stream's own clock says: a start at 10 s, one a few seconds from the 33-bit wrap, a
/// wrap in the middle of the clip.
#[test]
fn picture_and_sound_stay_in_sync_from_any_start() {
    if !have(&["libx264", "aac", "ac3", "mp2", "mpeg2video"]) {
        return;
    }
    let dir = scratch("sync");
    let wrap = (1u64 << 33) as f64 / 90_000.0;
    let cases = [
        Clip { name: "zero.ts", video: FLASH, audio: &[(BEEP, AAC)], vcodec: X264, m2ts: false, offset: 0.0 },
        Clip { name: "ten.ts", video: FLASH, audio: &[(BEEP, AAC)], vcodec: X264, m2ts: false, offset: 10.0 },
        Clip { name: "ten.m2ts", video: FLASH, audio: &[(BEEP, AC3)], vcodec: X264, m2ts: true, offset: 10.0 },
        Clip { name: "near-wrap.ts", video: FLASH, audio: &[(BEEP, AAC)], vcodec: X264, m2ts: false, offset: wrap - 2.0 },
        Clip { name: "wrap.mts", video: FLASH, audio: &[(BEEP, AC3)], vcodec: X264, m2ts: true, offset: wrap - 1.1 },
        Clip { name: "dvb.ts", video: FLASH, audio: &[(BEEP, MP2)], vcodec: MPEG2, m2ts: false, offset: 0.0 },
        Clip { name: "dvb-ten.ts", video: FLASH, audio: &[(BEEP, MP2)], vcodec: MPEG2, m2ts: false, offset: 10.0 },
        Clip { name: "dvb-wrap.ts", video: FLASH, audio: &[(BEEP, MP2)], vcodec: MPEG2, m2ts: false, offset: wrap - 1.1 },
    ];
    for clip in &cases {
        let path = make(&dir, clip);
        let frames = session_frames(&path);
        assert_eq!(frames.len(), 60, "{}", clip.name);
        let flash = flash_frame(&frames).expect("a flash");
        assert_eq!(flash, 25, "{}: the flash is at 1.0 s of the clip", clip.name);
        let (rate, width, all) = sound(&path, 0, 0.0, 48_000 * 3);
        let beep = first_loud(&all, width).expect("a beep") as f64 / f64::from(rate);
        eprintln!("{}: flash at {:.3} s, beep at {beep:.3} s", clip.name, f64::from(flash) / FPS);
        assert!(beep > 0.99 && beep < 1.0 + 0.065, "{}: the beep is at {beep:.3} s", clip.name);
        // After a seek the beep is still where it was.
        let (_, _, tail) = sound(&path, 0, 0.8, 48_000);
        let again = 0.8 + first_loud(&tail, width).expect("a beep after a seek") as f64 / f64::from(rate);
        assert!((again - beep).abs() < 0.002, "{}: {again} vs {beep}", clip.name);
        let (_, rx, _) = software(|| DecodeSession::open_at(&path, 25).map(|(m, rx, c)| (m, rx.into_iter().next(), c))).expect("seek");
        assert_eq!(rx.map(|f| f.index), Some(25), "{}", clip.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ AVCHD

/// A camcorder's file: 1920x1080 interlaced top field first, AC-3 and Blu-ray LPCM, 192-byte
/// packets, `.mts`.
#[test]
fn a_fake_avchd_file_opens_with_both_sound_tracks_and_exports() {
    if !have(&["libx264", "ac3", "pcm_bluray"]) {
        return;
    }
    let dir = scratch("avchd");
    let path = dir.join("00001.mts");
    ffmpeg(&[
        "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=50:duration=0.8",
        "-f", "lavfi", "-i", FLASH_AUDIO,
        "-f", "lavfi", "-i", FLASH_AUDIO,
        "-map", "0:v", "-vf", "interlace=scan=tff", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-flags", "+ildct+ilme",
        "-x264-params", "interlaced=1:tff=1:keyint=12:min-keyint=12:bframes=2", "-crf", "20", "-r", "25",
        "-map", "1:a", "-c:a:0", "ac3", "-b:a:0", "192k",
        "-map", "2:a", "-c:a:1", "pcm_bluray",
        "-f", "mpegts", "-mpegts_m2ts_mode", "1", s(&path),
    ]);
    let facts = engine::probe_media(&path);
    assert_eq!(facts.container.as_deref(), Some("mts"));
    let video = facts.video.as_ref().expect("a picture");
    assert_eq!((video.width, video.height), (1920, 1080));
    assert_eq!(video.interlaced, Some("tff"));
    assert!((video.frame_rate.unwrap() - 25.0).abs() < 0.01, "{:?}", video.frame_rate);
    let audio: Vec<_> = facts.streams.iter().filter(|s| s.kind == engine::demux::StreamKind::Audio).collect();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0].codec_id.as_deref(), Some("ac3"));
    assert_eq!(audio[1].codec_id.as_deref(), Some("pcm_bluray"));
    let (meta, demuxer) = Demuxer::open(&path).expect("open");
    assert!(demuxer.h264_interlaced());
    assert_eq!(meta.frame_count, 20);
    let want = ffmpeg_raw_1080(&path);
    let got = h264_planes(&path);
    assert!(got == want, "pictures differ from ffmpeg's ({} vs {} bytes)", got.len(), want.len());
    // Deinterlaced frames out of the session, one per interlaced frame.
    assert_eq!(session_frames(&path).len(), 20);
    for stream in 0..2 {
        let (rate, width, pcm) = sound(&path, stream, 0.0, 48_000);
        assert_eq!((rate, width), (48_000, 2));
        assert!(pcm.iter().any(|v| v.abs() > 0.1), "stream {stream} is silent");
    }
    // Export: a progressive mp4 of the right length with its sound.
    let out = dir.join("out.mp4");
    software(|| export_as(&path, &out, Format::Mp4));
    let secs: f64 = ffprobe(&out, "format=duration", "v:0").parse().unwrap_or(0.0);
    assert!((secs - 0.8).abs() < 0.12, "{secs} s");
    assert_eq!(ffprobe(&out, "stream=nb_frames", "v:0").parse::<u32>().unwrap_or(20), 20);
    assert!(!ffprobe(&out, "stream=codec_name", "a:0").is_empty(), "the export has sound");
    let _ = std::fs::remove_dir_all(&dir);
}

const FLASH_AUDIO: &str = "aevalsrc=exprs='0.5*sin(2*PI*(300+200*t)*t)|0.5*sin(2*PI*(310+200*t)*t)':s=48000:d=0.8";

fn ffmpeg_raw_1080(path: &Path) -> Vec<u8> {
    ffmpeg_raw(path)
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

/// An export of a transport stream re-encodes (there is no stream copy), keeps its length,
/// and keeps the flash and the beep together.
#[test]
fn an_export_from_a_transport_stream_keeps_its_length_and_its_sync() {
    if !have(&["libx264", "aac", "ac3", "mp2", "mpeg2video"]) {
        return;
    }
    let dir = scratch("export");
    for (name, audio, vcodec, m2ts, offset) in [
        ("e.mts", AC3, X264, true, 10.0),
        ("e.ts", AAC, X264, false, 0.0),
        // A DVB recording: MPEG-2 video and MP2 sound.
        ("dvb.ts", MP2, MPEG2, false, 0.0),
        ("dvb-off.ts", MP2, MPEG2, false, 10.0),
    ] {
        let clip = Clip { name, video: FLASH, audio: &[(BEEP, audio)], vcodec, m2ts, offset };
        let path = make(&dir, &clip);
        let out = dir.join(format!("{name}.mp4"));
        software(|| export_as(&path, &out, Format::Mp4));
        let secs: f64 = ffprobe(&out, "format=duration", "v:0").parse().unwrap_or(0.0);
        assert!((secs - 2.4).abs() < 0.1, "{name}: {secs} s");
        // The flash and the beep in the exported file, decoded by ffmpeg.
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

// ------------------------------------------------------------------ MPEG-2 and MPEG audio

/// MPEG-2 video, I/P/B, a GOP of ten.
const MPEG2: &[&str] = &["-c:v", "mpeg2video", "-b:v", "4M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p"];
/// MPEG-2 interlaced, top field first (the source is woven by the `interlace` filter).
const MPEG2_TFF: &[&str] = &["-c:v", "mpeg2video", "-b:v", "4M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p", "-flags", "+ildct+ilme", "-top", "1"];
const MPEG2_BFF: &[&str] = &["-c:v", "mpeg2video", "-b:v", "4M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p", "-flags", "+ildct+ilme", "-top", "0"];
/// MPEG-2 4:2:2 profile.
const MPEG2_422: &[&str] = &["-c:v", "mpeg2video", "-b:v", "8M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv422p"];
const MP2: &[&str] = &["-c:a", "mp2", "-b:a", "192k"];
const CARD_TFF: &str = "testsrc2=size=352x288:rate=50:duration=2,interlace=scan=tff";
const CARD_BFF: &str = "testsrc2=size=352x288:rate=50:duration=2,interlace=scan=bff";

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

/// MPEG-2 video opens, probes and decodes bit for bit as ffmpeg does, for the shapes a recorder
/// writes: progressive, interlaced either way, 4:2:2, in a `.ts`, an `.m2ts` and with a start offset.
#[test]
fn mpeg2_video_opens_probes_and_decodes_as_ffmpeg_does() {
    if !have(&["mpeg2video", "mp2", "ac3"]) {
        return;
    }
    let dir = scratch("mpeg2v");
    let cases = [
        (Clip { name: "p.ts", video: CARD, audio: &[(CHIRP, MP2)], vcodec: MPEG2, m2ts: false, offset: 0.0 }, None, "yuv420p", "Main"),
        (Clip { name: "p.m2ts", video: CARD, audio: &[(CHIRP, AC3)], vcodec: MPEG2, m2ts: true, offset: 10.0 }, None, "yuv420p", "Main"),
        (Clip { name: "tff.ts", video: CARD_TFF, audio: &[(CHIRP, MP2)], vcodec: MPEG2_TFF, m2ts: false, offset: 0.0 }, Some("tff"), "yuv420p", "Main"),
        (Clip { name: "bff.ts", video: CARD_BFF, audio: &[], vcodec: MPEG2_BFF, m2ts: false, offset: 0.0 }, Some("bff"), "yuv420p", "Main"),
        (Clip { name: "422.ts", video: CARD, audio: &[], vcodec: MPEG2_422, m2ts: false, offset: 0.0 }, None, "yuv422p", "4:2:2"),
    ];
    for (clip, interlaced, pix_fmt, profile) in &cases {
        let path = make(&dir, clip);
        let facts = engine::probe_media(&path);
        let video = facts.video.as_ref().unwrap_or_else(|| panic!("{}: no picture", clip.name));
        assert_eq!((video.width, video.height), (352, 288), "{}", clip.name);
        assert_eq!(video.codec, engine::demux::Codec::Mpeg2, "{}", clip.name);
        assert!((video.frame_rate.unwrap() - FPS).abs() < 0.01, "{}: {:?}", clip.name, video.frame_rate);
        assert_eq!(video.interlaced, *interlaced, "{}", clip.name);
        assert_eq!(video.profile, Some(*profile), "{}", clip.name);
        let secs = facts.duration_secs.expect("a length");
        assert!((secs - 2.0).abs() <= 1.0 / FPS, "{}: {secs} s", clip.name);
        assert!(
            facts.streams.iter().any(|s| s.codec_id.as_deref() == Some("mpeg2video")),
            "{}: {:?}",
            clip.name,
            facts.streams
        );
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.frame_count, 50, "{}", clip.name);
        let want = ffmpeg_raw_as(&path, pix_fmt);
        let got = mpeg2_planes(&path);
        assert!(got == want, "{}: pictures differ from ffmpeg's ({} vs {} bytes)", clip.name, got.len(), want.len());
        let frames = session_frames(&path);
        assert_eq!(frames.iter().map(|f| f.index).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>(), "{}", clip.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seek into any frame of MPEG-2 video, GOP boundary or not, interlaced (and so
/// deinterlaced against its neighbours) or not, is the sequential decode's frame.
#[test]
fn mpeg2_seeks_equal_a_sequential_decode() {
    if !have(&["mpeg2video"]) {
        return;
    }
    let dir = scratch("mpeg2seek");
    let cases = [
        Clip { name: "p.ts", video: CARD, audio: &[], vcodec: MPEG2, m2ts: false, offset: 0.0 },
        Clip { name: "p-off.m2ts", video: CARD, audio: &[], vcodec: MPEG2, m2ts: true, offset: 3.0 },
        Clip { name: "tff.ts", video: CARD_TFF, audio: &[], vcodec: MPEG2_TFF, m2ts: false, offset: 0.0 },
        Clip { name: "bff.ts", video: CARD_BFF, audio: &[], vcodec: MPEG2_BFF, m2ts: false, offset: 0.0 },
        Clip { name: "422.ts", video: CARD, audio: &[], vcodec: MPEG2_422, m2ts: false, offset: 0.0 },
    ];
    for clip in &cases {
        let path = make(&dir, clip);
        let all = session_frames(&path);
        assert_eq!(all.len(), 50, "{}", clip.name);
        for target in [0u32, 1, 2, 5, 9, 10, 11, 13, 17, 20, 25, 33, 41, 49] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{}: no frame after seek to {target}", clip.name))
            });
            assert_eq!(seeked.index, target, "{}", clip.name);
            assert!(seeked.bgra == all[target as usize].bgra, "{}: seek to {target} differs from sequential", clip.name);
        }
        let range = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_range(&path, 12, 31).expect("open_range");
            rx.into_iter().collect::<Vec<_>>()
        });
        assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (12..31).collect::<Vec<_>>(), "{}", clip.name);
        for f in &range {
            assert!(f.bgra == all[f.index as usize].bgra, "{}: range frame {} differs", clip.name, f.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An interlaced MPEG-2 picture comes out of the session deinterlaced: not the woven frame
/// ffmpeg gives (rows of both fields), and the progressive one is left exactly as it was.
#[test]
fn interlaced_mpeg2_is_deinterlaced_and_progressive_mpeg2_is_not() {
    if !have(&["mpeg2video"]) {
        return;
    }
    let dir = scratch("mpeg2deint");
    let tff = make(&dir, &Clip { name: "tff.ts", video: CARD_TFF, audio: &[], vcodec: MPEG2_TFF, m2ts: false, offset: 0.0 });
    let prog = make(&dir, &Clip { name: "p.ts", video: CARD, audio: &[], vcodec: MPEG2, m2ts: false, offset: 0.0 });
    // A moving card woven from two moments combs; the filter removes it, so a deinterlaced frame
    // has less row-to-row energy at the edges than the woven one.
    let comb = |frame: &engine::decode::Frame| -> u64 {
        let row = frame.width as usize * 4;
        frame
            .bgra
            .chunks(row)
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| w[0].iter().zip(w[1]).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>())
            .sum()
    };
    let woven = ffmpeg_raw_as(&tff, "yuv420p");
    let frames = session_frames(&tff);
    let size = 352 * 288;
    let luma_comb = |y: &[u8]| -> u64 { y.chunks(352).collect::<Vec<_>>().windows(2).map(|w| w[0].iter().zip(w[1]).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>()).sum() };
    let (mut woven_total, mut ours_total) = (0u64, 0u64);
    for n in 5..45 {
        woven_total += luma_comb(&woven[n * size * 3 / 2..n * size * 3 / 2 + size]);
        ours_total += comb(&frames[n]);
    }
    assert!(ours_total > 0 && woven_total > 0);
    // Different units (BGRA against luma), so compare per-sample.
    let (woven_avg, ours_avg) = (woven_total as f64 / (size as f64), ours_total as f64 / (size as f64 * 4.0));
    eprintln!("row-to-row difference: woven {woven_avg:.1}, deinterlaced {ours_avg:.1}");
    assert!(ours_avg < woven_avg, "the interlaced picture is not deinterlaced");
    // Progressive: untouched, i.e. what ec-mpeg2's own pictures convert to.
    let (meta, _) = Demuxer::open(&prog).expect("open");
    assert_eq!(meta.frame_count, 50);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An MPEG-2 file renamed is still opened by its bytes.
#[test]
fn an_mpeg2_transport_stream_is_sniffed_whatever_it_is_called() {
    if !have(&["mpeg2video"]) {
        return;
    }
    let dir = scratch("mpeg2sniff");
    let path = make(&dir, &Clip { name: "x.ts", video: CARD, audio: &[], vcodec: MPEG2, m2ts: false, offset: 0.0 });
    let odd = dir.join("recording.dat");
    std::fs::copy(&path, &odd).unwrap();
    assert!(engine::demux::is_mpegts(&odd));
    assert_eq!(Demuxer::open(&odd).expect("opens by content").0.frame_count, 50);
    let _ = std::fs::remove_dir_all(&dir);
}

/// DTS and TrueHD in a transport stream both decode through the engine, at the clip's
/// channel count and length, and the picture still opens.
#[test]
fn dts_and_truehd_in_a_transport_stream_both_decode_and_the_picture_still_opens() {
    if !have(&["libx264"]) {
        return;
    }
    let dir = scratch("ts_dts_truehd");
    for (codec, extra, name) in [
        ("dca", vec!["-strict", "-2", "-ac", "2"], "dts"),
        ("truehd", vec!["-strict", "-2", "-ac", "2"], "truehd"),
    ] {
        let path = dir.join(format!("{name}.ts"));
        let mut args: Vec<&str> = vec![
            "-v", "error", "-y", "-f", "lavfi", "-i", CARD, "-f", "lavfi", "-i", CHIRP, "-map", "0:v", "-c:v", "libx264", "-pix_fmt",
            "yuv420p", "-map", "1:a", "-c:a", codec,
        ];
        args.extend(extra.iter().copied());
        args.extend(["-f", "mpegts", s(&path)]);
        let made = Command::new("ffmpeg").args(&args).output().expect("ffmpeg runs");
        if !made.status.success() || !path.exists() {
            eprintln!("ffmpeg cannot make a {name} transport stream here: skipping");
            continue;
        }
        let (meta, _) = Demuxer::open(&path).expect("the picture opens");
        assert_eq!(meta.frame_count, 50, "{name}: picture frames");
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), 1, "{name}: {streams:?}");
        assert!(streams[0].decodable, "{name}: {:?}", streams[0]);
        let (audio, rx) = AudioSession::open(&path).expect("opens").expect("has sound");
        let mut samples = Vec::new();
        for chunk in rx {
            samples.extend_from_slice(&chunk.samples);
        }
        assert_eq!(usize::from(audio.channels), 2, "{name}: timeline channels");
        assert!(samples.iter().any(|s| s.abs() > 0.05), "{name}: no sound, or silence");
        let secs = samples.len() as f64 / 2.0 / f64::from(audio.sample_rate);
        assert!((secs - 2.0).abs() < 0.1, "{name}: {secs} s of sound for a 2 s clip");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A transport stream is recognised by its bytes under any name.
#[test]
fn a_transport_stream_is_sniffed_whatever_it_is_called() {
    if !have(&["libx264"]) {
        return;
    }
    let dir = scratch("sniff");
    let path = make(&dir, &Clip { name: "x.ts", video: CARD, audio: &[], vcodec: X264, m2ts: false, offset: 0.0 });
    let odd = dir.join("recording.dat");
    std::fs::copy(&path, &odd).unwrap();
    assert!(engine::demux::is_mpegts(&path) && engine::demux::is_mpegts(&odd));
    assert_eq!(Demuxer::open(&odd).expect("opens by content").0.frame_count, 50);
    assert!(!engine::demux::is_mpegts(Path::new("/nonexistent/song.mp3")));
    let _ = std::fs::remove_dir_all(&dir);
}
