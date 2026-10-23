//! VP9 import: the first source codec that is not H.264.
//!
//! The container and refusal checks need nothing installed. The software seat
//! (`ec-vp9`, `VE_SW` pinned) runs unignored; its fixtures are encoded by
//! ffmpeg's `libvpx-vp9` at test time, and its reference is ffmpeg's libvpx
//! decode, so those tests skip where ffmpeg has no libvpx. The hardware decode
//! and export twins need a built `libengine_hw.so` and a VA-API driver with a
//! VP9 decode entrypoint (`vainfo | grep VP9`), so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test vp9 -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide: every test that pins a seat takes it through
//! `support/seat.rs`, so the software and hardware tests here never see each
//! other's pin, with or without `--test-threads=1`.

use crate::seat;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ec_vp9::decode::{Decoder, Picture};
use engine::colorspace::Chroma;
use engine::demux::{Codec, Demuxer};
use engine::export::ExportSettings;
use engine::scratch::Scratch;
use engine::{AudioSession, DecodeSession, PlaybackSession, Project};

/// 1280x720@30, 2 s -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// `VE_SW` is process-wide, so every test here that sets it or reads a decode
/// that depends on it takes this pin: parallel tests then see each other's
/// `VE_SW` only when they hold it, and a poisoned lock still serialises.
static ENV_PIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_pin() -> std::sync::MutexGuard<'static, ()> {
    ENV_PIN.lock().unwrap_or_else(|e| e.into_inner())
}

/// The container half: a vp09 track is found, described and named as VP9, and
/// its AAC track is read exactly as any other file's -- the audio path knows
/// nothing about the video codec and this is what says so.
#[test]
fn the_demuxer_reports_a_vp9_track() {
    let _pin = env_pin();
    let (meta, _) = Demuxer::open(&asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(meta.codec, Codec::Vp9);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!((meta.frame_rate - 30.0).abs() < 0.01, "{}", meta.frame_rate);
    assert_eq!(meta.frame_count, FRAMES);

    let probe = AudioSession::probe(asset("test_vp9.mp4"), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!(
        (probe.sample_rate, probe.channels),
        (44_100, 2),
        "audio is read the same whatever the picture is coded with"
    );

    // The invariant, stated as an assert: H.264 files still say H.264.
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
}

/// The software seat takes VP9 now: with the plugin forced off, the stream
/// opens on `ec-vp9` in this process. H.264 is untouched by the same pin.
#[test]
fn the_software_path_opens_vp9() {
    let _seat = seat::software();
    let opened = DecodeSession::open(asset("test_vp9.mp4")).is_ok();
    let h264 = DecodeSession::open(asset("test_baseline.mp4")).is_ok();
    assert!(opened, "software VP9 decode opens");
    assert!(h264, "software H.264 decode still opens");
}

/// A 4:4:4 HEVC clip, which no seat takes, encoded by ffmpeg's libx265 into the
/// temp dir: 3 frames of 64x64. `assets/` is gitignored, so the clip is made here.
fn hevc_444_clip() -> PathBuf {
    let path = std::env::temp_dir().join(format!("vp9-hevc444-{}.mp4", std::process::id()));
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=64x64:rate=30:duration=0.1")
        .args(["-frames:v", "3", "-c:v", "libx265", "-pix_fmt", "yuv444p"])
        .args(["-x265-params", "log-level=error"])
        .arg(&path)
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    // Mark it profile 5 (high throughput), which ec-h265 does not decode.
    let mut bytes = std::fs::read(&path).expect("read the clip");
    let at = bytes.windows(4).position(|w| w == b"hvcC").expect("an hvcC box");
    bytes[at + 5] = (bytes[at + 5] & 0xe0) | 5;
    std::fs::write(&path, bytes).expect("write the patched clip");
    path
}

/// A timeline is *not* refused a source coded differently -- every clip opens
/// its own decoder. With the plugin forced off, VP9 joins the timeline on
/// `ec-vp9` and HEVC on `ec-h265`; 4:4:4 HEVC is no seat's, so it is refused at
/// the door by the decoder's own name.
#[test]
fn a_timeline_takes_the_other_codec_or_names_the_refusal() {
    let _seat = seat::software();
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    let clip = hevc_444_clip();
    let refused = session
        .import(&clip)
        .expect_err("profile 5 HEVC is no seat's to decode")
        .to_string();
    let _ = std::fs::remove_file(&clip);
    assert!(refused.contains("profile 5"), "{refused}");
    assert_eq!(session.sources().len(), 1, "a refusal left no row");
    session
        .import(&asset("test_vp9.mp4"))
        .expect("the software VP9 seat takes the clip beside H.264");
    session
        .import(&asset("test_hevc.mp4"))
        .expect("the software HEVC seat takes the clip beside H.264");
    assert_eq!(session.sources().len(), 3, "VP9 and HEVC beside H.264");
}

/// The refusal *at a span*, which is where it happens now: playback opens its
/// files on the worker (`DecodeSession::open_worker_deferred`), so a clip no
/// decoder here can take is refused there rather than on the caller's thread.
/// What must survive that move is the behaviour a refused span always had --
/// no pictures from that clip, and the session walks on to the end instead of
/// stalling on it.
///
/// The clip is imported as H.264, because the *door* still refuses what it
/// cannot open; the bytes under it become 4:4:4 HEVC afterwards, which is the only
/// way to put a span the worker must refuse on a timeline. (4:4:4 HEVC, not
/// HEVC: `ec-h265` has a software seat now and would decode 4:2:0 HEVC, and 4:2:2.)
#[test]
fn a_refused_codec_at_a_span_advances_instead_of_stalling() {
    let _seat = seat::software();
    let scratch = Scratch::file("video_editor_vp9_span", "mp4");
    std::fs::copy(asset("test_av2.mp4"), &scratch).expect("copy the fixture");
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    session.set_gain(0.0);
    let first = session.meta().frame_count;
    session.import(&scratch).expect("an H.264 file joins this timeline");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &scratch, 0, None)
            .expect("a file just imported is on this timeline")
    );
    let refused_clip = hevc_444_clip();
    std::fs::copy(&refused_clip, &scratch).expect("swap in the 4:4:4 HEVC bytes");
    let _ = std::fs::remove_file(&refused_clip);

    session.seek(0.0);
    session.play();
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut count, mut last) = (0u32, None);
    while !session.is_eos() {
        session.tick();
        while let Some(frame) = session.try_frame() {
            count += 1;
            last = Some(frame.index);
        }
        assert!(Instant::now() < deadline, "the refused span stalled playback");
        std::thread::sleep(Duration::from_millis(4));
    }

    assert_eq!(last, Some(first - 1), "the refused clip made pictures");
    assert_eq!(count, first, "the first source still played whole");
    let _ = std::fs::remove_file(&scratch);
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with VP9 decode"]
fn the_plugin_decodes_every_vp9_frame() {
    let _seat = seat::hardware();
    let start = Instant::now();
    let (meta, frames) = DecodeSession::open(asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(meta.codec, Codec::Vp9);
    let frames: Vec<_> = frames.into_iter().collect();
    eprintln!(
        "test_vp9.mp4: {} frames in {:?}",
        frames.len(),
        start.elapsed()
    );

    assert_eq!(frames.len() as u32, FRAMES, "every sample decoded");
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!((frame.width, frame.height), (1280, 720), "frame {i} dims");
        assert_eq!(frame.index, i as u32, "frames arrive in display order");
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4, "frame {i} size");
    }
    // A picture, not a flat surface: a driver handing back an untouched buffer
    // would satisfy every count above.
    let first = &frames[0].bgra;
    assert!(
        first.as_chunks::<4>().0.iter().any(|px| px != &first[..4]),
        "frame 0 is a single colour -- no picture was decoded"
    );
}

/// Export re-encodes to H.264 whatever the source was coded with: the packet
/// copy path is audio-only, so no VP9 byte can reach an `avc1` track.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with VP9 decode"]
fn a_vp9_source_exports_as_h264() {
    let _seat = seat::hardware();
    let session = PlaybackSession::open(asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    let meta = *session.meta();
    let project = Project::single(asset("test_vp9.mp4"), meta.frame_count);
    let out = Scratch::file("ve_export_vp9", "mp4");

    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(120), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("export");

    let (written, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(written.codec, Codec::H264, "exports are always H.264");
    assert_eq!(written.frame_count, FRAMES, "every timeline frame written");
    assert_eq!((written.width, written.height), (1280, 720));
    let _ = std::fs::remove_file(&out);
}

// --- the software seat -----------------------------------------------------
//
// `ec-vp9` against ffmpeg's `libvpx-vp9` decode, sample for sample. The clips
// are encoded by the same libvpx at test time: 24 frames of 352x288, a keyframe
// every 8, so seeks have several sync points to land on.

const SW_FRAMES: u32 = 24;
const SW_KEYFRAME: u32 = 8;

/// A WebM encoded by ffmpeg's libvpx at test time. `None` where ffmpeg with
/// `libvpx-vp9` is not installed -- the skip the other ffmpeg witnesses take.
fn ffmpeg_vp9(name: &str, profile: u8, pix_fmt: &str) -> Option<Scratch> {
    let out = Scratch::file(name, "webm");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=352x288:rate=30")
        .args(["-frames:v", "24", "-c:v", "libvpx-vp9", "-profile:v"])
        .arg(profile.to_string())
        .args(["-pix_fmt", pix_fmt, "-g", "8", "-b:v", "400k", "-deadline", "good"])
        .args(["-cpu-used", "4", "-auto-alt-ref", "1"])
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

/// Every picture ffmpeg's libvpx decoder releases, as raw planes of 16-bit
/// samples (8-bit output is widened, so one comparison serves both depths).
fn ffmpeg_libvpx_planes(path: &Path, pix_fmt: &str, bytes_per_sample: usize) -> Vec<[Vec<u16>; 3]> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "libvpx-vp9", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg (libvpx-vp9) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (w, h) = (352usize, 288usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let frame = (w * h + 2 * cw * ch) * bytes_per_sample;
    assert_eq!(out.stdout.len() % frame, 0, "ffmpeg handed back a partial frame");
    let widen = |bytes: &[u8]| -> Vec<u16> {
        if bytes_per_sample == 1 {
            bytes.iter().map(|&b| u16::from(b)).collect()
        } else {
            bytes.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect()
        }
    };
    out.stdout
        .chunks_exact(frame)
        .map(|data| {
            let (luma, rest) = data.split_at(w * h * bytes_per_sample);
            let (cb, cr) = rest.split_at(cw * ch * bytes_per_sample);
            [widen(luma), widen(cb), widen(cr)]
        })
        .collect()
}

/// Every shown picture `ec-vp9` produces for the file's own access units,
/// planes as 16-bit samples, the same widening as [`ffmpeg_libvpx_planes`].
fn ec_vp9_planes(path: &Path) -> Vec<[Vec<u16>; 3]> {
    let (_, mut demuxer) = Demuxer::open(path).expect("open the VP9 clip");
    let mut decoder = Decoder::new();
    let mut shown = Vec::new();
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        if let Some(pic) = decoder.decode(&au).expect("ec-vp9 decode") {
            let Picture { y, u, v, .. } = pic;
            shown.push([y, u, v]);
        }
    }
    shown
}

/// The decoder, sample for sample against libvpx: profile 0 (8-bit) and profile
/// 2 (10-bit), every picture, not a sample of them.
#[test]
fn ec_vp9_matches_libvpx_sample_for_sample() {
    let cases = [(0u8, "yuv420p", 1usize), (2, "yuv420p10le", 2)];
    for (profile, pix_fmt, bytes) in cases {
        let Some(clip) = ffmpeg_vp9(&format!("ve_vp9_p{profile}"), profile, pix_fmt) else {
            eprintln!("no ffmpeg with libvpx-vp9: skipping profile {profile}");
            continue;
        };
        let want = ffmpeg_libvpx_planes(&clip, pix_fmt, bytes);
        let got = ec_vp9_planes(&clip);
        assert_eq!(want.len(), SW_FRAMES as usize, "libvpx released every frame");
        assert_eq!(got.len(), want.len(), "profile {profile}: picture count");
        for (i, (a, b)) in got.iter().zip(&want).enumerate() {
            for (plane, (x, z)) in ["y", "u", "v"].iter().zip(a.iter().zip(b)) {
                assert!(x == z, "profile {profile} picture {i} {plane} differs from libvpx");
            }
        }
        eprintln!("profile {profile}: {} pictures agree sample for sample with libvpx", got.len());
    }
}

/// Opens a clip and drains it, returning the pictures with their display
/// indices. The caller holds the software seat: the pin is read when the span
/// opens on its worker, so it must stay in place through the drain.
fn software_frames(path: &Path) -> (engine::demux::VideoMeta, Vec<engine::decode::Frame>) {
    let (meta, rx) = DecodeSession::open(path).expect("software VP9 open");
    let frames: Vec<_> = rx.into_iter().collect();
    (meta, frames)
}

/// The edith seat, end to end: every frame of profile 0 and profile 2 comes out
/// through the span, in display order, at the clip's size, with pictures in
/// them -- and a seek lands on the very bytes a linear decode delivered, at the
/// sync points and between them.
#[test]
fn the_software_seat_decodes_and_seeks_vp9() {
    let _seat = seat::software();
    for (profile, pix_fmt) in [(0u8, "yuv420p"), (2, "yuv420p10le")] {
        let Some(clip) = ffmpeg_vp9(&format!("ve_vp9_seat_p{profile}"), profile, pix_fmt) else {
            eprintln!("no ffmpeg with libvpx-vp9: skipping the seat on profile {profile}");
            continue;
        };
        let (meta, frames) = software_frames(&clip);
        assert_eq!(meta.codec, Codec::Vp9);
        assert_eq!(meta.frame_count, SW_FRAMES, "profile {profile}: container count");
        assert_eq!(
            frames.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..SW_FRAMES).collect::<Vec<_>>(),
            "profile {profile}: frames must arrive 0..n in display order"
        );
        assert!(
            frames.iter().all(|f| (f.width, f.height) == (352, 288)),
            "profile {profile}: every frame at the clip's size"
        );
        let first = &frames[0].bgra;
        assert!(
            first.as_chunks::<4>().0.iter().any(|px| px[..] != first[..4]),
            "profile {profile}: frame 0 is a single colour -- no picture was decoded"
        );

        // Seeks: a keyframe, the frame after it, a frame just before the next
        // keyframe, and the last frame.
        let targets = [0, 1, SW_KEYFRAME - 1, SW_KEYFRAME, SW_KEYFRAME + 1, SW_FRAMES - 1];
        for target in targets {
            let want = &frames[target as usize].bgra;
            let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
            let seeked = rx
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("profile {profile}: no frame after seek to {target}"));
            assert_eq!(seeked.index, target, "profile {profile}: first frame after seek");
            assert!(
                seeked.bgra == *want,
                "profile {profile}: seek to {target} differs from a linear decode"
            );
        }
    }
}

/// Profile 1 is 4:4:4 (or 4:2:2 / 4:4:0), which the GPU does not take. Such a file
/// is not refused: the demuxer reads it, and it opens and decodes on the ec-vp9
/// software seat, which takes all four profiles. Its pictures come out at the
/// clip's size, in display order, with a picture in them.
#[test]
fn a_profile_1_444_vp9_file_opens_and_decodes_on_the_software_seat() {
    let _seat = seat::software();
    let Some(clip) = ffmpeg_vp9("ve_vp9_profile1", 1, "yuv444p") else {
        eprintln!("no ffmpeg with libvpx-vp9: skipping the profile 1 seat");
        return;
    };
    let (meta, demuxer) = Demuxer::open(&clip).expect("the demuxer reads profile 1");
    assert_eq!(meta.codec, Codec::Vp9);
    assert_eq!(demuxer.chroma(), Some(Chroma::C444), "the stream states 4:4:4");

    let (meta, frames) = software_frames(&clip);
    assert_eq!(meta.frame_count, SW_FRAMES, "container count");
    assert_eq!(
        frames.iter().map(|f| f.index).collect::<Vec<_>>(),
        (0..SW_FRAMES).collect::<Vec<_>>(),
        "profile 1: frames must arrive 0..n in display order"
    );
    assert!(
        frames.iter().all(|f| (f.width, f.height) == (352, 288)),
        "profile 1: every frame at the clip's size"
    );
    let first = &frames[0].bgra;
    assert!(
        first.as_chunks::<4>().0.iter().any(|px| px[..] != first[..4]),
        "profile 1: frame 0 is a single colour -- no picture was decoded"
    );
}
