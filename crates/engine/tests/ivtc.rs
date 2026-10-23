//! Inverse telecine ("remove pulldown", `Deinterlace::Ivtc` and `IvtcAuto`) on 29.97i
//! film: the 23.976p pictures come back out of 3:2 pulldown, in the right number, at
//! the right times, whichever way a clip is read (a sequential decode, a seek, an
//! export), and material that is truly interlaced is left to the deinterlacer.
//!
//! Every clip is synthetic: `testsrc2` at 24000/1001, telecined by ffmpeg's `telecine`
//! filter (hard pulldown, MPEG-2 in a transport stream and interlaced H.264) or flagged
//! (soft pulldown), and a 59.94-field `testsrc2` for the negative case. Without ffmpeg
//! and the encoders a test needs, it says so and passes.
//!
//! ```text
//! cargo test -p engine --release --test ivtc
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_core::frame::{Frame as CoreFrame, Interlace};
use ec_core::registry::{CodecId, CodecParameters};
use ec_core::{Packet, TimeBase, VideoFrame};
use engine::PlaybackSession;
use engine::deinterlace::{Deinterlacer, effective};
use engine::demux::Demuxer;
use engine::export::{ExportSettings, Format};
use engine::ivtc::{Telecine, cadence_for};
use engine::project::{Deinterlace, Lane};

const W: usize = 352;
const H: usize = 288;
/// Film frames in a clip: 15 blocks of four, so 75 telecined pictures.
const FILM: u32 = 60;
const FILM_FPS: f64 = 24000.0 / 1001.0;

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
    let dir = std::env::temp_dir().join(format!("edith-ivtc-{name}-{}", std::process::id()));
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

fn film_source() -> String {
    format!("testsrc2=size={W}x{H}:rate=24000/1001")
}

/// The 60 film frames, raw 4:2:0, and a lossless intermediate of them.
fn film(dir: &Path) -> (PathBuf, Vec<u8>) {
    let y4m = dir.join("film.y4m");
    ffmpeg(&["-f", "lavfi", "-i", &film_source(), "-frames:v", &FILM.to_string(), "-pix_fmt", "yuv420p", s(&y4m)]);
    let raw = dir.join("film.yuv");
    ffmpeg(&["-i", s(&y4m), "-f", "rawvideo", "-pix_fmt", "yuv420p", s(&raw)]);
    let bytes = std::fs::read(&raw).expect("reference");
    assert_eq!(bytes.len(), FILM as usize * W * H * 3 / 2);
    (y4m, bytes)
}

/// Hard telecine in MPEG-2 (a transport stream), top field first.
fn hard_mpeg2(dir: &Path, y4m: &Path) -> PathBuf {
    let out = dir.join("hard.ts");
    ffmpeg(&[
        "-i", s(y4m), "-vf", "telecine=pattern=23", "-flags", "+ilme+ildct", "-top", "1", "-c:v", "mpeg2video", "-qscale:v", "2",
        "-g", "10", "-bf", "2", "-f", "mpegts", s(&out),
    ]);
    out
}

/// Hard telecine in interlaced (MBAFF) H.264.
fn hard_h264(dir: &Path, y4m: &Path, tff: bool) -> PathBuf {
    let out = dir.join(if tff { "hard-tff.mp4" } else { "hard-bff.mp4" });
    let pattern = if tff { "top" } else { "bottom" };
    let scan = if tff { "tff" } else { "bff" };
    ffmpeg(&[
        "-i", s(y4m), "-vf", &format!("telecine=first_field={pattern}:pattern=23"), "-c:v", "libx264", "-crf", "8", "-flags", "+ildct+ilme",
        "-x264-params", &format!("interlaced=1:{scan}=1:keyint=10:min-keyint=10:bframes=2"), s(&out),
    ]);
    out
}

/// Truly interlaced: 59.94 moments, two to a frame.
fn true_interlaced(dir: &Path, name: &str, codec: &str) -> PathBuf {
    let out = dir.join(name);
    let mut args: Vec<&str> = vec![
        "-f", "lavfi", "-i", "testsrc2=size=352x288:rate=60000/1001", "-frames:v", "150", "-vf", "interlace=scan=tff", "-flags", "+ildct+ilme",
        "-c:v", codec,
    ];
    if codec == "libx264" {
        args.extend(["-crf", "8", "-x264-params", "interlaced=1:tff=1:keyint=10:min-keyint=10:bframes=2"]);
    } else {
        args.extend(["-qscale:v", "2", "-g", "10", "-bf", "2", "-top", "1", "-f", "mpegts"]);
    }
    args.push(s(&out));
    ffmpeg(&args);
    out
}

fn decode_all<D: ec_core::registry::Decoder>(clip: &Path, mut decoder: D) -> (Demuxer, Vec<VideoFrame>) {
    let (_, mut demuxer) = Demuxer::open(clip).expect("open");
    demuxer.seek_to_sync_at_or_before(0);
    let mut out = Vec::new();
    let take = |decoder: &mut D, out: &mut Vec<VideoFrame>| {
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
    demuxer.seek_to_sync_at_or_before(0);
    (demuxer, out)
}

fn woven(clip: &Path) -> (Demuxer, Vec<VideoFrame>) {
    let (meta, _) = Demuxer::open(clip).expect("open");
    match meta.codec {
        engine::demux::Codec::Mpeg2 => {
            decode_all(clip, ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).expect("decoder"))
        }
        _ => decode_all(clip, ec_h264::H264Decoder::new(CodecParameters::new(CodecId::H264)).expect("decoder")),
    }
}

fn raw(pic: &VideoFrame) -> Vec<u8> {
    pic.planes
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            let (w, h) = if i == 0 { (W, H) } else { (W / 2, H / 2) };
            (0..h).flat_map(move |y| p.data[y * p.stride..y * p.stride + w].iter().copied())
        })
        .collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "the same number of samples");
    let sq: f64 = a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2)).sum();
    if sq == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 * a.len() as f64 / sq).log10() }
}

/// What the decode layer's pulldown stage makes of `pics`, tagged by film frame.
fn ivtc_stage(demuxer: &Demuxer, pics: Vec<VideoFrame>, first_wanted: i64) -> Vec<(i64, VideoFrame)> {
    let mut stage =
        Deinterlacer::with_mode(Deinterlace::Ivtc, true, first_wanted, Interlace::Progressive).cadenced(Deinterlace::Ivtc, demuxer);
    let mut out = Vec::new();
    for (i, pic) in pics.into_iter().enumerate() {
        out.extend(stage.push_all(i as i64, pic));
    }
    out.extend(stage.finish_all());
    out
}

/// The film frames come back: right count, right tags, each close to the original.
fn check_film(label: &str, clip: &Path, reference: &[u8], floor: f64) {
    let (demuxer, pics) = woven(clip);
    assert_eq!(pics.len(), 75, "{label}: 60 film frames telecined to 75 pictures");
    let out = ivtc_stage(&demuxer, pics, 0);
    assert_eq!(out.len(), FILM as usize, "{label}: 75 pictures decimate to {FILM}");
    assert_eq!(out.iter().map(|f| f.0).collect::<Vec<_>>(), (0..i64::from(FILM)).collect::<Vec<_>>(), "{label}: tags");
    let frame = W * H * 3 / 2;
    let mut worst = f64::INFINITY;
    for (k, (_, pic)) in out.iter().enumerate() {
        assert_eq!(pic.interlace, Interlace::Progressive, "{label}: frame {k}");
        let p = psnr(&raw(pic), &reference[k * frame..(k + 1) * frame]);
        worst = worst.min(p);
        assert!(p >= floor, "{label}: film frame {k} is {p:.1} dB from the original (floor {floor})");
    }
    eprintln!("ivtc {label}: worst film frame {worst:.1} dB");
}

#[test]
fn hard_pulldown_in_mpeg2_comes_back_as_the_film_frames() {
    if !have(&["mpeg2video"]) {
        return;
    }
    let dir = scratch("hard-m2");
    let (y4m, reference) = film(&dir);
    let clip = hard_mpeg2(&dir, &y4m);
    check_film("mpeg2 tff", &clip, &reference, 33.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hard_pulldown_in_h264_comes_back_as_the_film_frames() {
    if !have(&["libx264"]) {
        return;
    }
    let dir = scratch("hard-264");
    let (y4m, reference) = film(&dir);
    for tff in [true, false] {
        let clip = hard_h264(&dir, &y4m, tff);
        check_film(if tff { "h264 tff" } else { "h264 bff" }, &clip, &reference, 38.0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_locks_on_telecine_and_not_on_true_interlaced() {
    if !have(&["mpeg2video", "libx264"]) {
        return;
    }
    let dir = scratch("auto");
    let (y4m, _) = film(&dir);
    let hard = [hard_mpeg2(&dir, &y4m), hard_h264(&dir, &y4m, true)];
    for clip in &hard {
        assert!(cadence_for(clip, Telecine::Auto).is_some(), "{}: a stable cadence", clip.display());
        assert_eq!(effective(Deinterlace::IvtcAuto, clip), Deinterlace::Ivtc);
        assert_eq!(effective(Deinterlace::Ivtc, clip), Deinterlace::Ivtc);
        // Not asked for: the old settings are what they were.
        assert_eq!(effective(Deinterlace::Frame, clip), Deinterlace::Frame);
    }
    for clip in [true_interlaced(&dir, "true.mp4", "libx264"), true_interlaced(&dir, "true.ts", "mpeg2video")] {
        assert!(cadence_for(&clip, Telecine::Auto).is_none(), "{}: true interlaced has no 3:2 cadence", clip.display());
        assert_eq!(effective(Deinterlace::IvtcAuto, &clip), Deinterlace::Frame, "Auto leaves it to the deinterlacer");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seek to film frame `n` shows the picture a sequential decode shows there.
#[test]
fn a_seek_equals_a_sequential_decode() {
    if !have(&["mpeg2video", "libx264"]) {
        return;
    }
    let dir = scratch("seek");
    let (y4m, _) = film(&dir);
    for clip in [hard_mpeg2(&dir, &y4m), hard_h264(&dir, &y4m, true)] {
        let (demuxer, pics) = woven(&clip);
        let all = ivtc_stage(&demuxer, pics, 0);
        assert_eq!(all.len(), FILM as usize);
        for start in (0..FILM).step_by(3).chain([FILM - 1]) {
            let (mut demuxer, _) = {
                let (_, d) = Demuxer::open(&clip).expect("open");
                (d, ())
            };
            let mut stage = Deinterlacer::with_mode(Deinterlace::Ivtc, true, i64::from(start), Interlace::Progressive)
                .cadenced(Deinterlace::Ivtc, &demuxer);
            let landed = demuxer.seek_to_sync_at_or_before(stage.landing(start));
            let mut index = landed;
            let mut got = Vec::new();
            // Decode from the landing frame on, through the same decoder family.
            let meta = Demuxer::open(&clip).expect("open").0;
            let mut feed = |pic: VideoFrame| {
                got.extend(stage.push_all(index, pic));
                index += 1;
            };
            if meta.codec == engine::demux::Codec::Mpeg2 {
                run(&mut demuxer, ec_mpeg2::Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)).unwrap(), &mut feed);
            } else {
                run(&mut demuxer, ec_h264::H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap(), &mut feed);
            }
            got.extend(stage.finish_all());
            assert!(!got.is_empty(), "{}: nothing from film frame {start}", clip.display());
            assert_eq!(got[0].0, i64::from(start), "{}: first frame after seeking to {start}", clip.display());
            for (tag, pic) in &got {
                let want = &all[*tag as usize].1;
                assert!(raw(pic) == raw(want), "{}: film frame {tag} after seeking to {start} differs from the sequential decode", clip.display());
            }
            assert_eq!(got.len(), (FILM - start) as usize, "{}: seek to {start} runs to the end", clip.display());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn run<D: ec_core::registry::Decoder>(demuxer: &mut Demuxer, mut decoder: D, feed: &mut impl FnMut(VideoFrame)) {
    let take = |decoder: &mut D, feed: &mut dyn FnMut(VideoFrame)| {
        while let Ok(frame) = decoder.receive_frame() {
            if let CoreFrame::Video(pic) = frame {
                feed(pic);
            }
        }
    };
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        decoder.send_packet(&Packet::new(0, TimeBase::new(1, 1), au)).expect("decode");
        take(&mut decoder, feed);
    }
    decoder.flush().expect("flush");
    take(&mut decoder, feed);
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

fn probe_pts(path: &Path) -> Vec<f64> {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts_time", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    let mut v: Vec<f64> = String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().trim_end_matches(',').parse().ok()).collect();
    v.sort_by(f64::total_cmp);
    v
}

/// On a 23.976 timeline the clip is the film: compose_at and an export both give the
/// original frames, one per timeline frame, at 1001/24000 s apart.
#[test]
fn session_on_a_film_timeline_is_frame_accurate() {
    if !have(&["mpeg2video", "libx264"]) {
        return;
    }
    let dir = scratch("session");
    let (y4m, reference) = film(&dir);
    for clip in [hard_mpeg2(&dir, &y4m), hard_h264(&dir, &y4m, true)] {
        let mut session = PlaybackSession::open(&clip).unwrap_or_else(|e| panic!("open {}: {e}", clip.display()));
        session.set_gain(0.0);
        assert!(session.set_frame_rate(FILM_FPS), "film rate");
        assert!(session.set_deinterlace(Lane::V1, 0, Deinterlace::IvtcAuto));
        let secs = session.timeline_duration();
        assert!(
            (secs - f64::from(FILM) / FILM_FPS).abs() < 0.5 / FILM_FPS,
            "{}: the clip is {FILM} film frames long on a 23.976 timeline, not {secs} s",
            clip.display()
        );
        let want = |k: u32| {
            let f = W * H * 3 / 2;
            &reference[k as usize * f..(k as usize + 1) * f]
        };
        let ref_bgra = |k: u32| {
            let out = dir.join(format!("ref{k}.rgb"));
            let yuv = dir.join(format!("ref{k}.yuv"));
            std::fs::write(&yuv, want(k)).unwrap();
            ffmpeg(&["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "352x288", "-i", s(&yuv), "-f", "rawvideo", "-pix_fmt", "bgra", s(&out)]);
            std::fs::read(&out).unwrap()
        };
        for k in [0, 1, 2, 3, 4, 17, 30, 31, 58, 59] {
            let got = software(|| session.compose_at(k, W as u32, H as u32)).expect("compose_at").bgra;
            let p = psnr(&got, &ref_bgra(k));
            assert!(p > 28.0, "{}: compose_at({k}) is {p:.1} dB from film frame {k}", clip.display());
        }
        let out = dir.join("out.mp4");
        software(|| export_session(&session, &out));
        let pts = probe_pts(&out);
        assert_eq!(pts.len(), FILM as usize, "{}: one exported frame per film frame", clip.display());
        for (k, t) in pts.iter().enumerate() {
            assert!((t - k as f64 / FILM_FPS).abs() < 0.001, "{}: frame {k} at {t}", clip.display());
        }
        let exported = dir.join("out.yuv");
        ffmpeg(&["-i", s(&out), "-f", "rawvideo", "-pix_fmt", "yuv420p", s(&exported)]);
        let got = std::fs::read(&exported).unwrap();
        let frame = W * H * 3 / 2;
        assert_eq!(got.len(), FILM as usize * frame);
        for k in 0..FILM as usize {
            let p = psnr(&got[k * frame..(k + 1) * frame], want(k as u32));
            assert!(p > 28.0, "{}: exported frame {k} is {p:.1} dB from film frame {k}", clip.display());
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Soft pulldown: the film frames are coded as they are and the stream asks for
// the repeated fields. Pictures arrive 2 and 3 fields apart.

/// 48 progressive film frames as an MPEG-2 elementary stream, then flagged: a
/// sequence that says 29.97, and `progressive_frame` with `repeat_first_field` and
/// `top_field_first` on the 3:2 pattern, in the picture coding extensions.
fn soft_mpeg2(dir: &Path) -> PathBuf {
    let es = dir.join("soft.m2v");
    ffmpeg(&[
        "-f", "lavfi", "-i", &film_source(), "-frames:v", "48", "-flags", "+ildct+ilme", "-c:v", "mpeg2video", "-qscale:v", "2",
        "-bf", "0", "-g", "12", "-f", "mpeg2video", s(&es),
    ]);
    let mut bytes = std::fs::read(&es).expect("elementary stream");
    let pattern = [(true, true), (false, false), (false, true), (true, false)];
    let (mut n, mut i) = (0usize, 0usize);
    while i + 9 < bytes.len() {
        if bytes[i..i + 3] != [0, 0, 1] {
            i += 1;
            continue;
        }
        match bytes[i + 3] {
            // frame_rate_code 4: 29.97
            0xB3 => bytes[i + 7] = (bytes[i + 7] & 0xF0) | 4,
            0xB5 if bytes[i + 4] >> 4 == 8 => {
                let (tff, rff) = pattern[n % 4];
                n += 1;
                bytes[i + 7] = (bytes[i + 7] & 0x7D) | (u8::from(tff) << 7) | (u8::from(rff) << 1);
                bytes[i + 8] |= 0x80;
            }
            _ => {}
        }
        i += 3;
    }
    let flagged = dir.join("soft-flagged.m2v");
    std::fs::write(&flagged, bytes).unwrap();
    let ts = dir.join("soft-mpeg2.ts");
    // ffmpeg's parser spaces the pictures by their repeat flags.
    ffmpeg(&["-fflags", "+genpts", "-f", "mpegvideo", "-i", s(&flagged), "-c:v", "copy", "-f", "mpegts", s(&ts)]);
    ts
}

/// The same as H.264: `pic_struct` 3, 5, 4, 6 in the picture timing SEI, and the
/// transport stream's times spaced by the fields each picture shows.
fn soft_h264(dir: &Path) -> PathBuf {
    let es = dir.join("soft.264");
    ffmpeg(&[
        "-f", "lavfi", "-i", &film_source(), "-frames:v", "48", "-c:v", "libx264", "-crf", "10", "-x264-params",
        "pic-struct=1:bframes=0:keyint=12:min-keyint=12", "-f", "h264", s(&es),
    ]);
    let mut bytes = std::fs::read(&es).expect("elementary stream");
    let (mut n, mut i) = (0usize, 0usize);
    while i + 7 < bytes.len() {
        if bytes[i..i + 3] == [0, 0, 1] && bytes[i + 3] & 0x1f == 6 && bytes[i + 4] == 1 {
            // The payload byte is pic_struct, one clock-timestamp flag (zero) per
            // field count in Table D-1, and the alignment bit that ends it.
            let (pic_struct, clocks) = [(3u8, 2u8), (5, 3), (4, 2), (6, 3)][n % 4];
            n += 1;
            bytes[i + 6] = (pic_struct << 4) | (1 << (3 - clocks));
            i += 6;
        } else {
            i += 1;
        }
    }
    assert!(n >= 48, "a picture timing SEI per picture, found {n}");
    let flagged = dir.join("soft-flagged.264");
    std::fs::write(&flagged, bytes).unwrap();
    // The stream says 29.97 (60000/1001 fields), and ffmpeg's parser spaces the
    // pictures by their pic_struct: 2, 3, 2, 3 fields.
    let timed = dir.join("soft-timed.264");
    ffmpeg(&["-bsf:v", "h264_metadata=tick_rate=60000/1001", "-f", "h264", "-i", s(&flagged), "-c:v", "copy", "-f", "h264", s(&timed)]);
    let ts = dir.join("soft-h264.ts");
    ffmpeg(&["-fflags", "+genpts", "-f", "h264", "-i", s(&timed), "-c:v", "copy", "-f", "mpegts", s(&ts)]);
    ts
}

fn decoded_range(path: &Path, lo: u32, hi: u32) -> Vec<engine::Frame> {
    let (_, rx, _cancel) = engine::DecodeSession::open_range(path, lo, hi).expect("open_range");
    rx.iter().collect()
}

#[test]
fn soft_pulldown_is_presented_as_the_film_frames() {
    if !have(&["mpeg2video", "libx264"]) {
        return;
    }
    let dir = scratch("soft");
    for (label, clip) in [("mpeg2", soft_mpeg2(&dir)), ("h264", soft_h264(&dir))] {
        let (meta, demuxer) = Demuxer::open(&clip).expect("open");
        assert!(demuxer.soft_pulldown(), "{label}: the flags are seen");
        assert!((meta.frame_rate - FILM_FPS).abs() < 0.01, "{label}: {} fps, not 23.976", meta.frame_rate);
        assert_eq!(meta.frame_count, 48, "{label}: 48 coded pictures, 48 frames");
        // Every frame, sequentially, then each by a seek of its own.
        let all = decoded_range(&clip, 0, 48);
        assert_eq!(all.len(), 48, "{label}");
        assert_eq!(all.iter().map(|f| f.index).collect::<Vec<_>>(), (0..48).collect::<Vec<_>>(), "{label}");
        // They are ffmpeg's decode, picture for picture.
        let reference = dir.join(format!("{label}.bgra"));
        ffmpeg(&["-i", s(&clip), "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "bgra", s(&reference)]);
        let bytes = std::fs::read(&reference).unwrap();
        let size = W * H * 4;
        assert_eq!(bytes.len(), 48 * size, "{label}: ffmpeg decodes 48 pictures");
        for (k, f) in all.iter().enumerate() {
            let p = psnr(&f.bgra, &bytes[k * size..(k + 1) * size]);
            assert!(p > 35.0, "{label}: frame {k} is {p:.1} dB from ffmpeg's");
        }
        for k in [0u32, 1, 5, 12, 13, 25, 47] {
            let one = decoded_range(&clip, k, k + 1);
            assert_eq!(one.len(), 1, "{label}: seek to {k}");
            assert!(one[0].bgra == all[k as usize].bgra, "{label}: the seek to {k} shows another picture than the sequential decode");
        }
        // The same through a session on a film-rate timeline, in every pulldown mode:
        // nothing to remove, so nothing is dropped.
        for mode in [Deinterlace::Frame, Deinterlace::IvtcAuto, Deinterlace::Ivtc] {
            let mut session = PlaybackSession::open(&clip).expect("session");
            session.set_gain(0.0);
            session.set_frame_rate(FILM_FPS);
            let _ = session.set_deinterlace(Lane::V1, 0, mode);
            let secs = session.timeline_duration();
            assert!((secs - 48.0 / FILM_FPS).abs() < 1.0 / FILM_FPS, "{label} {mode:?}: {secs} s");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
