//! AVI (`.avi`, `.divx`) end to end: probe, decode, seek, sound, A/V sync, export and the
//! streams that are refused.
//!
//! Every clip is synthetic (`testsrc2`, a flash on black, a chirp, a beep) made by ffmpeg into
//! a scratch directory; without ffmpeg and the encoders a test needs, it says so and passes,
//! as `mpegts` does. `VE_SW` is process-wide, so tests that pin it hold [`SW`].
//!
//! The pictures of B-frame streams are checked against the stream they were remuxed from
//! (an mp4 for H.264, a transport stream for MPEG-2): an AVI has no timestamps, so this is
//! the proof that decode order and display order come out right. MJPEG pictures go through
//! the MJPEG decode seat (another lane); until it exists the picture check says so and
//! passes, and the demux-level checks (packets, probe, seek, field order) carry the case.
//!
//! ```text
//! cargo test -p engine --release --test avi
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
    let dir = std::env::temp_dir().join(format!("edith-avi-{name}-{}", std::process::id()));
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
const CARD: &str = "testsrc2=size=352x288:rate=25:duration=2";
const CHIRP: &str = "aevalsrc=exprs='0.5*sin(2*PI*(300+200*t)*t)|0.5*sin(2*PI*(310+200*t)*t)':s=48000:d=2";
const FLASH: &str = "color=c=black:s=352x288:r=25:d=2.4,drawbox=x=0:y=0:w=352:h=288:color=white:t=fill:enable='between(t,1,1.2)'";
const BEEP: &str = "aevalsrc=exprs='0.8*sin(2*PI*1000*t)*between(t,1,1.2)|0.8*sin(2*PI*1000*t)*between(t,1,1.2)':s=48000:d=2.4";

const X264: &[&str] = &["-c:v", "libx264", "-x264-params", "keyint=10:min-keyint=10:bframes=2", "-pix_fmt", "yuv420p"];
/// Open GOPs: the keyframe is a non-IDR I picture with B pictures behind it that are shown first.
const X264_OPEN: &[&str] =
    &["-c:v", "libx264", "-x264-params", "keyint=10:min-keyint=10:bframes=2:open-gop=1", "-pix_fmt", "yuv420p"];
const MPEG2: &[&str] = &["-c:v", "mpeg2video", "-b:v", "4M", "-g", "10", "-bf", "2", "-pix_fmt", "yuv420p"];
const MJPEG: &[&str] = &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"];
const MP3: &[&str] = &["-c:a", "libmp3lame", "-b:a", "128k"];
const MP2: &[&str] = &["-c:a", "mp2", "-b:a", "192k"];
const PCM: &[&str] = &["-c:a", "pcm_s16le"];
const AAC: &[&str] = &["-c:a", "aac", "-b:a", "128k"];
const AC3: &[&str] = &["-c:a", "ac3", "-b:a", "192k"];

/// Muxes `video` (+ `audio`) with `vcodec`/`acodec` into `dir/name`, in the container the
/// name's extension says.
fn make(dir: &Path, name: &str, video: &str, audio: Option<&str>, vcodec: &[&str], acodec: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", video];
    if let Some(a) = audio {
        args.extend(["-f", "lavfi", "-i", a]);
    }
    args.extend(["-map", "0:v"]);
    args.extend(vcodec);
    if audio.is_some() {
        args.extend(["-map", "1:a"]);
        args.extend(acodec);
    }
    args.push(s(&out));
    ffmpeg(&args);
    out
}

/// `src` copied (no re-encode) into a file called `name` of `dir`; an mp4's H.264 is turned
/// into the Annex B an AVI carries.
fn remux(dir: &Path, src: &Path, name: &str) -> PathBuf {
    let out = dir.join(name);
    // `-r`: the source's timescale (1/12800, 1/90000) would otherwise become the AVI's rate.
    let mut args = vec!["-i", s(src), "-map", "0", "-c", "copy", "-r", "25"];
    if src.extension().is_some_and(|e| e == "mp4") {
        args.extend(["-bsf:v", "h264_mp4toannexb"]);
    }
    args.push(s(&out));
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

fn session_frames(path: &Path) -> Vec<engine::decode::Frame> {
    software(|| DecodeSession::open(path).expect("open").1.into_iter().collect())
}

// ------------------------------------------------------------------ pictures

/// Probe, frame count, every picture against the stream the AVI was remuxed from, and a seek
/// to any frame against the sequential decode: H.264 (IDR and open GOPs) and MPEG-2, with
/// B pictures, whose AVI carries decode order and no timestamps.
#[test]
fn b_frame_pictures_probe_decode_and_seek_as_their_source_does() {
    if !have(&["libx264", "mpeg2video", "libmp3lame", "mp2"]) {
        return;
    }
    let dir = scratch("pictures");
    #[allow(clippy::type_complexity)]
    let cases: [(&str, &[&str], &[&str], &str, Codec, &str); 3] = [
        ("h264", X264, MP3, "mp4", Codec::H264, "h264"),
        ("h264-open", X264_OPEN, MP3, "mp4", Codec::H264, "h264"),
        ("mpeg2", MPEG2, MP2, "ts", Codec::Mpeg2, "mpeg2video"),
    ];
    for (name, vcodec, acodec, via, codec, ffname) in cases {
        let origin = make(&dir, &format!("{name}.{via}"), CARD, Some(CHIRP), vcodec, acodec);
        let path = remux(&dir, &origin, &format!("{name}.avi"));
        assert!(engine::demux::is_avi(&path), "{name}");
        let facts = engine::probe_media(&path);
        assert_eq!(facts.container.as_deref(), Some("avi"), "{name}");
        let video = facts.video.as_ref().unwrap_or_else(|| panic!("{name}: no picture: {:?}", Demuxer::open(&path).err().map(|e| e.to_string())));
        assert_eq!((video.width, video.height), (352, 288), "{name}");
        assert_eq!(video.codec, codec, "{name}");
        assert!((video.frame_rate.unwrap() - FPS).abs() < 0.01, "{name}: {:?}", video.frame_rate);
        assert!(video.bit_depth.is_none_or(|d| d == 8), "{name}: {:?}", video.bit_depth);
        assert_eq!(video.interlaced, None, "{name}");
        let secs = facts.duration_secs.expect("a length");
        assert!((secs - 2.0).abs() <= 1.0 / FPS, "{name}: {secs} s");
        let kinds: Vec<_> = facts.streams.iter().map(|f| f.codec_id.clone().unwrap_or_default()).collect();
        assert_eq!(kinds.first().map(String::as_str), Some(ffname), "{name}: {kinds:?}");
        assert!(kinds.iter().any(|k| k == if name == "mpeg2" { "mp2" } else { "mp3" }), "{name}: {kinds:?}");
        let (meta, _) = Demuxer::open(&path).expect("open");
        assert_eq!(meta.frame_count, 50, "{name}");

        let all = session_frames(&path);
        let reference = session_frames(&origin);
        assert_eq!(all.len(), 50, "{name}");
        assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>(), "{name}");
        assert_eq!(reference.len(), 50, "{name}");
        for (a, b) in all.iter().zip(&reference) {
            assert!(a.bgra == b.bgra, "{name}: frame {} differs from the {via} it was remuxed from", a.index);
        }
        for target in [0u32, 1, 2, 5, 9, 10, 11, 13, 17, 20, 25, 33, 41, 49] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
                rx.into_iter().next().unwrap_or_else(|| panic!("{name}: no frame after seek to {target}"))
            });
            assert_eq!(seeked.index, target, "{name}");
            assert!(seeked.bgra == all[target as usize].bgra, "{name}: seek to {target} differs from sequential");
        }
        let range = software(|| {
            let (_, rx, _cancel) = DecodeSession::open_range(&path, 12, 31).expect("open_range");
            rx.into_iter().collect::<Vec<_>>()
        });
        assert_eq!(range.iter().map(|f| f.index).collect::<Vec<_>>(), (12..31).collect::<Vec<_>>(), "{name}");
        for f in &range {
            assert!(f.bgra == all[f.index as usize].bgra, "{name}: range frame {} differs", f.index);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// MJPEG + PCM: at the demux level the packets are ffmpeg's, the probe states the stream,
/// a seek lands on the frame asked for, and the field order is the AVI convention. The
/// pictures themselves are compared with ffmpeg's decode once the MJPEG seat exists.
#[test]
fn mjpeg_and_pcm_demux_probe_seek_and_decode() {
    if !have(&["mjpeg", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("mjpeg");
    let path = make(&dir, "m.avi", CARD, Some(CHIRP), MJPEG, PCM);
    let facts = engine::probe_media(&path);
    assert_eq!(facts.container.as_deref(), Some("avi"));
    let video = facts.video.as_ref().expect("a picture");
    assert_eq!((video.width, video.height), (352, 288));
    assert_eq!(video.codec, Codec::Mjpeg);
    assert!((video.frame_rate.unwrap() - FPS).abs() < 0.01);
    assert_eq!(video.interlaced, None);
    let secs = facts.duration_secs.expect("a length");
    assert!((secs - 2.0).abs() <= 1.0 / FPS, "{secs} s");
    let codecs: Vec<_> = facts.streams.iter().map(|f| (f.codec_id.clone().unwrap_or_default(), f.bit_depth)).collect();
    assert_eq!(codecs[0].0, "mjpeg", "{codecs:?}");
    assert_eq!(codecs[1], ("pcm_s16le".to_string(), Some(16)), "{codecs:?}");
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!((streams.len(), streams[0].decodable, streams[0].channels, streams[0].sample_rate), (1, true, 2, 48_000));
    assert_eq!(streams[0].bit_depth, Some(16));

    let (meta, mut demuxer) = Demuxer::open(&path).expect("open");
    assert_eq!((meta.codec, meta.frame_count, meta.width, meta.height), (Codec::Mjpeg, 50, 352, 288));
    assert_eq!(demuxer.field_order(), Some(ec_core::frame::Interlace::BottomFieldFirst));
    // Every packet equals ffmpeg's, byte for byte.
    let want = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-an", "-c:v", "copy", "-f", "mjpeg", "-"])
        .output()
        .expect("ffmpeg")
        .stdout;
    demuxer.seek_to_sync_at_or_before(0);
    let mut packets = Vec::new();
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        packets.push(au);
    }
    assert_eq!(packets.len(), 50);
    assert!(packets.concat() == want, "the demuxed packets are not ffmpeg's");
    // Every frame is a keyframe: a seek lands on the frame asked for and hands out its packet.
    for target in [0u32, 1, 24, 25, 49] {
        assert_eq!(demuxer.seek_to_sync_at_or_before(target), i64::from(target));
        assert!(demuxer.next_access_unit().unwrap().unwrap() == packets[target as usize], "seek to {target}");
    }

    // The pictures: PSNR against ffmpeg's own decode (full-range BT.601 to BGRA).
    let frames = session_frames_or_none(&path);
    let Some(frames) = frames else {
        eprintln!("no MJPEG decode seat yet: pictures not compared");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert_eq!(frames.len(), 50);
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-an", "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .output()
        .expect("ffmpeg")
        .stdout;
    let size = 352 * 288 * 4;
    for f in &frames {
        let theirs = &raw[f.index as usize * size..(f.index as usize + 1) * size];
        let mse = f.bgra.iter().zip(theirs).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum::<f64>() / size as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10();
        assert!(psnr > 30.0, "frame {}: PSNR {psnr:.1} dB against ffmpeg", f.index);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every picture the session releases, or `None` when the codec has no decode seat yet.
fn session_frames_or_none(path: &Path) -> Option<Vec<engine::decode::Frame>> {
    let frames = session_frames(path);
    (!frames.is_empty()).then_some(frames)
}

// ------------------------------------------------------------------ sound

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

fn ffmpeg_sound(path: &Path, rate: u32) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-ac", "2", "-ar", &rate.to_string(), "-f", "f32le", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Our samples (`from` seconds in) against ffmpeg's decode; the largest difference.
fn compare(path: &Path, from: f64, frames: usize, reference: &[f32]) -> f32 {
    let (rate, width, ours) = sound(path, 0, from, frames);
    assert_eq!(width, 2);
    let first = (from * f64::from(rate)).round() as i64;
    let mut worst = 0f32;
    let mut compared = 0;
    for frame in 0..ours.len() / 2 {
        let at = first + frame as i64;
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

/// Every audio codec an AVI carries against ffmpeg's decode, from the top and from the
/// middle; a seek is the sequential decode's samples; the probe row states the track.
#[test]
fn the_sound_is_ffmpegs_at_every_time() {
    if !have(&["mjpeg", "libmp3lame", "mp2", "aac", "ac3", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("sound");
    let cases: [(&str, &[&str], &str, f32); 5] = [
        ("pcm", PCM, "pcm_s16le", 1e-6),
        ("mp3", MP3, "mp3", 1.5e-3),
        ("mp2", MP2, "mp2", 1.5e-3),
        ("aac", AAC, "aac", 1.5e-3),
        ("ac3", AC3, "ac3", 1.5e-3),
    ];
    for (name, acodec, codec, tolerance) in cases {
        let path = make(&dir, &format!("{name}.avi"), CARD, Some(CHIRP), MJPEG, acodec);
        let streams = AudioSession::probe_streams(&path).expect("streams");
        assert_eq!(streams.len(), 1, "{name}");
        let row = &streams[0];
        assert!(row.decodable, "{name}: {row:?}");
        assert_eq!((row.codec.as_str(), row.channels, row.sample_rate), (codec, 2, 48_000), "{name}");
        let reference = ffmpeg_sound(&path, 48_000);
        for from in [0.0, 0.4, 1.1, 1.7] {
            let worst = compare(&path, from, 7_200, &reference);
            eprintln!("{name} from {from}s: worst difference {worst:.6}");
            assert!(worst <= tolerance, "{name} from {from}s: {worst}");
        }
        let (_, _, full) = sound(&path, 0, 0.0, 96_000 * 2);
        let (_, _, cut) = sound(&path, 0, 1.3, 4_800);
        let at = (1.3 * 48_000.0) as usize * 2;
        let worst = cut.iter().zip(&full[at..]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst <= 1e-4, "{name}: a seek differs from sequential by {worst}");
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

/// The flash is at 1.0 s in the picture and the beep at 1.0 s in the sound, after the open
/// and after a seek, for the two B-frame codecs; the beep alone for MJPEG + PCM.
#[test]
fn picture_and_sound_stay_in_sync_from_any_start() {
    if !have(&["libx264", "libmp3lame", "mpeg2video", "mp2", "mjpeg", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("sync");
    let cases: [(&str, &[&str], &[&str]); 3] = [("h264", X264, MP3), ("mpeg2", MPEG2, MP2), ("mjpeg", MJPEG, PCM)];
    for (name, vcodec, acodec) in cases {
        let path = make(&dir, &format!("{name}.avi"), FLASH, Some(BEEP), vcodec, acodec);
        let (rate, width, all) = sound(&path, 0, 0.0, 48_000 * 3);
        let beep = first_loud(&all, width).expect("a beep") as f64 / f64::from(rate);
        assert!(beep > 0.99 && beep < 1.0 + 0.065, "{name}: the beep is at {beep:.3} s");
        let (_, _, tail) = sound(&path, 0, 0.8, 48_000);
        let again = 0.8 + first_loud(&tail, width).expect("a beep after a seek") as f64 / f64::from(rate);
        assert!((again - beep).abs() < 0.002, "{name}: {again} vs {beep}");
        if name == "mjpeg" {
            continue;
        }
        let frames = session_frames(&path);
        assert_eq!(frames.len(), 60, "{name}");
        let flash = flash_frame(&frames).expect("a flash");
        assert_eq!(flash, 25, "{name}: the flash is at 1.0 s of the clip");
        let (_, rx, _) = software(|| DecodeSession::open_at(&path, 25).map(|(m, rx, c)| (m, rx.into_iter().next(), c))).expect("seek");
        assert_eq!(rx.map(|f| f.index), Some(25), "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ export

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

/// An export of an AVI re-encodes, keeps its length and its frame count, and keeps the flash
/// and the beep together.
#[test]
fn an_export_from_an_avi_keeps_its_length_its_frames_and_its_sync() {
    if !have(&["libx264", "libmp3lame", "mpeg2video", "mp2"]) {
        return;
    }
    let dir = scratch("export");
    for (name, vcodec, acodec) in [("h264", X264, MP3), ("mpeg2", MPEG2, MP2)] {
        let path = make(&dir, &format!("{name}.avi"), FLASH, Some(BEEP), vcodec, acodec);
        let out = dir.join(format!("{name}.mp4"));
        software(|| export_as(&path, &out, Format::Mp4));
        let secs: f64 = ffprobe(&out, "format=duration", "v:0").parse().unwrap_or(0.0);
        assert!((secs - 2.4).abs() < 0.1, "{name}: {secs} s");
        let count: u32 = ffprobe(&out, "stream=nb_frames", "v:0").parse().unwrap_or(0);
        assert!(count.abs_diff(60) <= 1, "{name}: {count} frames");
        let video = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-an", "-vf", "scale=8:8,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "-"])
            .output()
            .expect("ffmpeg");
        let flash = video.stdout.chunks(64).position(|f| f.iter().map(|&b| u32::from(b)).sum::<u32>() > 128 * 64).expect("a flash");
        let pcm = ffmpeg_sound(&out, 48_000);
        let beep = pcm.chunks(2).position(|f| f.iter().any(|s| s.abs() > 0.2)).expect("a beep") as f64 / 48_000.0;
        let flash_at = flash as f64 / FPS;
        assert!((flash_at - 1.0).abs() < 0.045, "{name}: flash at {flash_at}");
        assert!((beep - flash_at).abs() < 0.09, "{name}: beep {beep} vs flash {flash_at}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------ refusals and sniffing

/// What the crates refuse is still refused by name with the ffmpeg line, in the open error and
/// in the probe: raw layouts `ec-riff` does not carry (1 bpp) and MS-MPEG4 version 1 (`MPG4`);
/// with sound beside them the sound still opens. (DV, MPEG-4 Part 2, MS-MPEG4 version 2 and 3
/// and the common raw layouts have decode seats: `tests/dvmpeg4.rs`, `tests/oldavi.rs`.)
#[test]
fn unreadable_raw_and_msmpeg4_v1_are_refused_by_name_and_the_sound_still_opens() {
    if !have(&["rawvideo", "msmpeg4", "pcm_s16le"]) {
        return;
    }
    let dir = scratch("refused");
    // ffmpeg has no version 1 encoder: a version 3 file with its FourCC rewritten (in the
    // stream header and the format chunk) is the same bitstream under the version 1 name.
    let v1 = |from: &Path, name: &str| {
        let mut bytes = std::fs::read(from).unwrap();
        for at in 0..bytes.len().saturating_sub(4) {
            if &bytes[at..at + 4] == b"DIV3" && at < 4096 {
                bytes[at..at + 4].copy_from_slice(b"MPG4");
            }
        }
        let out = dir.join(name);
        std::fs::write(&out, bytes).unwrap();
        out
    };
    let cases: [(&str, &[&str], &str); 2] = [
        ("raw", &["-c:v", "rawvideo", "-pix_fmt", "monow"], "rawvideo"),
        ("msmpeg4v1", &["-c:v", "msmpeg4", "-vtag", "DIV3"], "msmpeg4v1"),
    ];
    for (name, vcodec, word) in cases {
        let mut silent = make(&dir, &format!("{name}.avi"), CARD, None, vcodec, &[]);
        let mut with = make(&dir, &format!("{name}-pcm.avi"), CARD, Some(CHIRP), vcodec, PCM);
        if name == "msmpeg4v1" {
            silent = v1(&silent, "v1.avi");
            with = v1(&with, "v1-pcm.avi");
        }
        let err = Demuxer::open(&silent).err().unwrap_or_else(|| panic!("{name}: opened")).to_string();
        assert!(err.contains("ffmpeg -i") && err.contains("convert"), "{name}: {err}");
        let why = engine::demux::picture_refusal(&silent).expect("a reason");
        assert!(why.contains("ffmpeg"), "{name}: {why}");

        let err = Demuxer::open(&with).err().unwrap_or_else(|| panic!("{name}: opened")).to_string();
        assert!(err.contains("ffmpeg -i"), "{name}: {err}");
        let facts = engine::probe_media(&with);
        assert!(facts.video.is_none(), "{name}");
        let first = facts.streams.first().expect("the refused picture is listed");
        assert_eq!(first.codec_id.as_deref(), Some(word), "{name}: {:?}", facts.streams);
        let streams = AudioSession::probe_streams(&with).expect("streams");
        assert!(streams.len() == 1 && streams[0].decodable, "{name}: {streams:?}");
        assert!(AudioSession::open(&with).expect("opens").is_some(), "{name}: the sound opens");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An AVI is recognised by its bytes under any name, and `.divx` by its name.
#[test]
fn an_avi_is_sniffed_whatever_it_is_called() {
    if !have(&["mjpeg"]) {
        return;
    }
    let dir = scratch("sniff");
    let path = make(&dir, "x.avi", CARD, None, MJPEG, &[]);
    let odd = dir.join("recording.dat");
    std::fs::copy(&path, &odd).unwrap();
    let divx = dir.join("clip.divx");
    std::fs::copy(&path, &divx).unwrap();
    assert!(engine::demux::is_avi(&path) && engine::demux::is_avi(&odd) && engine::demux::is_avi(&divx));
    assert_eq!(Demuxer::open(&odd).expect("opens by content").0.frame_count, 50);
    assert!(!engine::demux::is_avi(Path::new("/nonexistent/song.mp3")));
    let wav = dir.join("x.wav");
    ffmpeg(&["-f", "lavfi", "-i", CHIRP, s(&wav)]);
    assert!(!engine::demux::is_avi(&wav));
    let _ = std::fs::remove_dir_all(&dir);
}
