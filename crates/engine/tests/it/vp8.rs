//! VP8 import: the container half, and the decode half. The software seat is
//! `ec-vp8` in this process (`decode::vp8`), and it runs unignored: its clips
//! are encoded by ffmpeg's `libvpx` at test time, and its reference is ffmpeg's
//! libvpx decode, so those tests skip where ffmpeg has no libvpx. The plugin's
//! own VP8 arm (`engine-hw`'s `vpx.rs`, the same `ec-vp8`) is the hardware-first
//! twin and needs a built `libengine_hw.so`, but **no VA-API VP8 profile**: no
//! such profile exists on this GPU, and none is asked for.
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test vp8 -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide: every test that pins a seat takes it through
//! `support/seat.rs`, so the software and hardware tests here never see each
//! other's pin, with or without `--test-threads=1`.
use crate::seat;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ec_vp8::decode::{Decoder, Picture};
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

/// The container half: a vp08 track is found and named as VP8 out of the mp4,
/// its `V_VP8` twin out of the `.webm`, and the AAC track is read exactly as
/// any other file's -- the audio path knows nothing about the video codec.
#[test]
fn the_demuxer_reports_a_vp8_track() {
    let (meta, _) = Demuxer::open(&asset("test_vp8.mp4")).expect("open test_vp8.mp4");
    assert_eq!(meta.codec, Codec::Vp8);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!((meta.frame_rate - 30.0).abs() < 0.01, "{}", meta.frame_rate);
    assert_eq!(meta.frame_count, FRAMES);

    let (webm, _) = Demuxer::open(&asset("test_vp8.webm")).expect("open test_vp8.webm");
    assert_eq!(webm.codec, Codec::Vp8);

    let probe = AudioSession::probe(asset("test_vp8.mp4"), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!(
        (probe.sample_rate, probe.channels),
        (44_100, 2),
        "audio is read the same whatever the picture is coded with"
    );
}

/// The software seat takes VP8 now: with the plugin forced off, the stream
/// opens on `ec-vp8` in this process, the same way H.264 and VP9 do.
#[test]
fn the_software_path_opens_vp8() {
    let _seat = seat::software();
    let opened = DecodeSession::open(asset("test_vp8.mp4")).is_ok();
    assert!(opened, "software VP8 decode opens");
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin's VP8 arm. The `.webm` is the file this decodes,
/// the container half being the Matroska demuxer's `V_VP8` row.
#[test]
#[ignore = "needs a built libengine_hw.so -- no VA-API profile involved"]
fn the_plugin_decodes_every_vp8_frame() {
    let _seat = seat::hardware();
    let start = Instant::now();
    let (meta, frames) = DecodeSession::open(asset("test_vp8.webm")).expect("open test_vp8.webm");
    assert_eq!(meta.codec, Codec::Vp8);
    let frames: Vec<_> = frames.into_iter().collect();
    eprintln!(
        "test_vp8.webm: {} frames in {:?} ({:.1} fps)",
        frames.len(),
        start.elapsed(),
        frames.len() as f64 / start.elapsed().as_secs_f64().max(1e-9)
    );

    assert_eq!(frames.len() as u32, FRAMES, "every sample decoded");
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!((frame.width, frame.height), (1280, 720), "frame {i} dims");
        assert_eq!(frame.index, i as u32, "frames arrive in display order");
        assert_eq!(frame.bgra.len(), 1280 * 720 * 4, "frame {i} size");
    }
    // A picture, not a flat surface: a decoder handing back an untouched
    // buffer would satisfy every count above.
    let first = &frames[0].bgra;
    assert!(
        first.as_chunks::<4>().0.iter().any(|px| px != &first[..4]),
        "frame 0 is a single colour -- no picture was decoded"
    );
    // ...and a moving one, not one still: the fixture is a rendered clip, so
    // frame 30 disagrees with frame 0 however the two are sampled.
    let thumb = |f: &engine::Frame| -> Vec<u8> {
        (0..f.bgra.len()).step_by(6173).map(|i| f.bgra[i]).collect()
    };
    assert_ne!(
        thumb(&frames[0]),
        thumb(&frames[FRAMES as usize / 2]),
        "every frame identical -- nothing moved"
    );
}

/// Export re-encodes to H.264 whatever the source was coded with: the packet
/// copy path is audio-only, so no VP8 byte can reach an `avc1` track.
#[test]
#[ignore = "needs a built libengine_hw.so -- no VA-API profile involved"]
fn a_vp8_source_exports_as_h264() {
    let _seat = seat::hardware();
    let session = PlaybackSession::open(asset("test_vp8.webm")).expect("open test_vp8.webm");
    let meta = *session.meta();
    let project = Project::single(asset("test_vp8.webm"), meta.frame_count);
    let out = Scratch::file("ve_export_vp8", "mp4");

    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(120), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("export");

    let (written, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(written.codec, Codec::H264, "exports are always H.264");
    // The fixture's own length and rate, back again: 60 pictures at 30 fps,
    // which is the 2 s the source played.
    assert_eq!(written.frame_count, FRAMES, "every timeline frame written");
    assert!(
        (written.frame_rate - 30.0).abs() < 0.01,
        "{}",
        written.frame_rate
    );
    assert_eq!((written.width, written.height), (1280, 720));
    let _ = std::fs::remove_file(&out);
}

// --- the software seat -----------------------------------------------------
//
// `ec-vp8` against ffmpeg's `libvpx` decode, picture for picture. The clips are
// encoded by the same libvpx at test time. Three shapes: a plain stream with a
// key frame every 8, a stream with hidden alt-ref frames (lag-in-frames and
// auto-alt-ref, so the decoder's reference handling beyond the key frame is
// exercised), and an odd-sized 99x67 stream (chroma planes of 50x34).

const SW_FRAMES: u32 = 24;
const SW_KEYFRAME: u32 = 8;

/// One clip shape: name, width, height, and whether it carries alt-ref frames.
struct Clip {
    name: &'static str,
    width: u32,
    height: u32,
    altref: bool,
}

const CLIPS: [Clip; 3] = [
    Clip {
        name: "ve_vp8_plain",
        width: 352,
        height: 288,
        altref: false,
    },
    Clip {
        name: "ve_vp8_altref",
        width: 352,
        height: 288,
        altref: true,
    },
    Clip {
        name: "ve_vp8_odd",
        width: 99,
        height: 67,
        altref: false,
    },
];

/// A WebM encoded by ffmpeg's libvpx at test time. `None` where ffmpeg with
/// `libvpx` is not installed -- the skip the other ffmpeg witnesses take.
///
/// The alt-ref clip is two-pass: libvpx's one-pass encoder through ffmpeg never
/// emits a hidden alt-ref frame, and the stream would not test what its name says.
fn ffmpeg_vp8(clip: &Clip) -> Option<Scratch> {
    if clip.width % 2 == 1 {
        return odd_clip(clip);
    }
    let out = Scratch::file(clip.name, "webm");
    let size = format!("testsrc2=size={}x{}:rate=30", clip.width, clip.height);
    let pass_no = |n: u8| n.to_string();
    let encode = |pass: Option<(u8, &Path)>, out: &Path| -> Option<()> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(&size)
            .args(["-frames:v", "24", "-c:v", "libvpx", "-g", "8"])
            .args(["-b:v", "400k", "-deadline", "good", "-cpu-used", "4"]);
        if clip.altref {
            cmd.args(["-auto-alt-ref", "1", "-lag-in-frames", "16", "-g", "100"]);
        }
        if let Some((n, log)) = pass {
            cmd.args(["-pass", &pass_no(n), "-passlogfile"]).arg(log);
            if n == 1 {
                cmd.args(["-f", "null", "-"]);
                return cmd.status().ok()?.success().then_some(());
            }
        }
        cmd.arg(out).status().ok()?.success().then_some(())
    };
    if clip.altref {
        let log = Scratch::file(&format!("{}_pass", clip.name), "log");
        encode(Some((1, &log)), &out)?;
        encode(Some((2, &log)), &out)?;
    } else {
        encode(None, &out)?;
    }
    Some(out)
}

/// An odd-sized clip. ffmpeg's libvpx encoder only takes even sizes (asked for
/// 99x67 it writes 98x66), so the 98x66 stream is re-labelled at the bitstream
/// level: 98 and 66 round up to the same 7x5 macroblock grid as 99 and 67, so
/// every key frame's dimension field is rewritten and the macroblock data is
/// untouched. The result is a real 99x67 stream, which libvpx then decodes as
/// its own. The IVF header is rewritten too, so the WebM ffmpeg muxes from it
/// carries 99x67.
fn odd_clip(clip: &Clip) -> Option<Scratch> {
    let src = ffmpeg_vp8(&Clip {
        name: "ve_vp8_odd_src",
        width: 98,
        height: 66,
        altref: false,
    })?;
    let raw = Scratch::file(&format!("{}_raw", clip.name), "ivf");
    let out = Scratch::file(clip.name, "webm");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-c", "copy", "-f", "ivf"])
        .arg(&raw)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut bytes = std::fs::read(&raw).ok()?;
    let (w, h) = (clip.width as u16, clip.height as u16);
    bytes[12..14].copy_from_slice(&w.to_le_bytes());
    bytes[14..16].copy_from_slice(&h.to_le_bytes());
    let mut at = 32;
    while at + 12 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let frame = at + 12;
        if frame + size > bytes.len() {
            break;
        }
        // A key frame (bit 0 of the tag clear) carries the dimensions at 6..10,
        // after the 3-byte start code 9d 01 2a.
        if size >= 10 && bytes[frame] & 1 == 0 {
            bytes[frame + 6..frame + 8].copy_from_slice(&w.to_le_bytes());
            bytes[frame + 8..frame + 10].copy_from_slice(&h.to_le_bytes());
        }
        at = frame + size;
    }
    std::fs::write(&raw, &bytes).ok()?;
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&raw)
        .args(["-c", "copy"])
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

/// The display indices of a file's shown frames, and how many access units it
/// has. A hidden frame is one whose `show_frame` bit (bit 4 of the first byte,
/// RFC 6386 9.1) is clear: it takes a container slot and shows nothing.
fn shown_samples(path: &Path) -> (Vec<u32>, usize) {
    let (_, mut demuxer) = Demuxer::open(path).expect("open the VP8 clip");
    let mut shown = Vec::new();
    let mut units = 0;
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        if au[0] & 0x10 != 0 {
            shown.push(units as u32);
        }
        units += 1;
    }
    (shown, units)
}

/// Every picture ffmpeg's libvpx decoder releases, as 8-bit I420 planes widened
/// to 16 bits, so one comparison serves every shape. Chroma is `div_ceil(2)`,
/// which is what makes the odd-sized clip's planes come out right.
fn ffmpeg_libvpx_planes(path: &Path, width: usize, height: usize) -> Vec<[Vec<u16>; 3]> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "libvpx", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg (libvpx) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let frame = width * height + 2 * cw * ch;
    assert_eq!(
        out.stdout.len() % frame,
        0,
        "ffmpeg handed back a partial frame"
    );
    let widen = |bytes: &[u8]| -> Vec<u16> { bytes.iter().map(|&b| u16::from(b)).collect() };
    out.stdout
        .chunks_exact(frame)
        .map(|data| {
            let (luma, rest) = data.split_at(width * height);
            let (cb, cr) = rest.split_at(cw * ch);
            [widen(luma), widen(cb), widen(cr)]
        })
        .collect()
}

/// Every shown picture `ec-vp8` produces for the file's own access units, plus
/// how many access units it took to get them: the difference is the hidden
/// frames, which the altref clip must have.
fn ec_vp8_planes(path: &Path) -> (Vec<[Vec<u16>; 3]>, usize) {
    let (_, mut demuxer) = Demuxer::open(path).expect("open the VP8 clip");
    let mut decoder = Decoder::new();
    let mut shown = Vec::new();
    let mut units = 0;
    while let Some(au) = demuxer.next_access_unit().expect("demux") {
        units += 1;
        if let Some(pic) = decoder.decode(&au).expect("ec-vp8 decode") {
            let Picture { y, u, v, .. } = pic;
            shown.push([y, u, v].map(|p| p.into_iter().map(u16::from).collect()));
        }
    }
    (shown, units)
}

/// The decoder, picture for picture against libvpx: every shown frame of every
/// clip shape, plane by plane. The altref clip must also carry hidden frames,
/// or it would not be testing what it is named for.
#[test]
fn ec_vp8_matches_libvpx_picture_for_picture() {
    for clip in CLIPS {
        let Some(path) = ffmpeg_vp8(&clip) else {
            eprintln!("no ffmpeg with libvpx: skipping {}", clip.name);
            continue;
        };
        let (w, h) = (clip.width as usize, clip.height as usize);
        let want = ffmpeg_libvpx_planes(&path, w, h);
        let (got, units) = ec_vp8_planes(&path);
        assert_eq!(
            want.len(),
            SW_FRAMES as usize,
            "{}: libvpx released every frame",
            clip.name
        );
        assert_eq!(got.len(), want.len(), "{}: picture count", clip.name);
        if clip.altref {
            assert!(
                units > got.len(),
                "{}: no hidden frames in the stream",
                clip.name
            );
        }
        for (i, (a, b)) in got.iter().zip(&want).enumerate() {
            for (plane, (x, z)) in ["y", "u", "v"].iter().zip(a.iter().zip(b)) {
                assert!(
                    x == z,
                    "{} picture {i} {plane} differs from libvpx",
                    clip.name
                );
            }
        }
        eprintln!(
            "{}: {} pictures ({} access units) agree with libvpx sample for sample",
            clip.name,
            got.len(),
            units
        );
    }
}

/// Opens a clip and drains it, returning the pictures with their display
/// indices. The caller holds the software seat: the pin is read when the span
/// opens on its worker, so it must stay in place through the drain.
fn software_frames(path: &Path) -> (engine::demux::VideoMeta, Vec<engine::decode::Frame>) {
    let (meta, rx) = DecodeSession::open(path).expect("software VP8 open");
    let frames: Vec<_> = rx.into_iter().collect();
    (meta, frames)
}

/// The edith seat, end to end: every frame of every clip shape comes out
/// through the span, in display order, at the clip's size, with pictures in
/// them -- and a seek lands on the very bytes a linear decode delivered, at the
/// key frames, between them, and past the hidden frames.
#[test]
fn the_software_seat_decodes_and_seeks_vp8() {
    let _seat = seat::software();
    for clip in CLIPS {
        let Some(path) = ffmpeg_vp8(&clip) else {
            eprintln!("no ffmpeg with libvpx: skipping the seat on {}", clip.name);
            continue;
        };
        let (shown, units) = shown_samples(&path);
        let (meta, frames) = software_frames(&path);
        assert_eq!(meta.codec, Codec::Vp8);
        assert_eq!(
            meta.frame_count as usize, units,
            "{}: container count",
            clip.name
        );
        assert_eq!(
            shown.len(),
            SW_FRAMES as usize,
            "{}: shown frames",
            clip.name
        );
        // Shown frames arrive at their container slots, in order: a hidden slot
        // is skipped, so the altref clip's indices have a hole where it sits.
        assert_eq!(
            frames.iter().map(|f| f.index).collect::<Vec<_>>(),
            shown,
            "{}: frames must arrive in display order at their slots",
            clip.name
        );
        assert!(
            frames
                .iter()
                .all(|f| (f.width, f.height) == (clip.width, clip.height)),
            "{}: every frame at the clip's size, got {:?}",
            clip.name,
            frames.first().map(|f| (f.width, f.height))
        );
        let first = &frames[0].bgra;
        assert!(
            first
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[..] != first[..4]),
            "{}: frame 0 is a single colour -- no picture was decoded",
            clip.name
        );

        // Seeks: a key frame, the frame after it, a frame just before the next
        // key frame, the key frame itself, the last frame, and the shown frame
        // straight after each hidden slot (a seek must not stop on the hole).
        let last = units as u32 - 1;
        let hidden_after: Vec<u32> = (0..units as u32)
            .filter(|i| !shown.contains(i))
            .map(|i| i + 1)
            .collect();
        let mut targets = vec![0, 1, SW_KEYFRAME - 1, SW_KEYFRAME, SW_KEYFRAME + 1, last];
        targets.extend(hidden_after);
        targets.retain(|t| shown.contains(t));
        for target in targets {
            // Looked up by display index: the linear decode's frames skip a
            // hidden slot, so position and index part ways after it.
            let want = &frames
                .iter()
                .find(|f| f.index == target)
                .unwrap_or_else(|| panic!("{}: no linear frame {target}", clip.name))
                .bgra;
            let (_, rx, _cancel) = DecodeSession::open_at(&path, target).expect("open_at");
            let seeked = rx
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("{}: no frame after seek to {target}", clip.name));
            assert_eq!(
                seeked.index, target,
                "{}: first frame after seek",
                clip.name
            );
            let first = seeked.bgra.iter().zip(want).position(|(a, b)| a != b);
            assert!(
                seeked.bgra == *want,
                "{}: seek to {target} differs from a linear decode (first byte at {first:?} of {})",
                clip.name,
                want.len()
            );
        }
    }
}
