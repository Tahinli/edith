//! HEVC on the software seat: `ec-h265` through the engine's own span, with the
//! VA-API plugin forced off (`VE_SW=1`), the way VP9 and VP8 already run there.
//!
//! The fixtures are encoded by ffmpeg's libx265 (`scripts/gen_fixtures.sh`); the
//! plane-for-plane check against ffmpeg's own decode lives beside the seat, in
//! `decode/hevc.rs`. Here the seat as a whole: what a file opens as, what a span
//! delivers, and whether a seek lands on the very bytes a linear decode gave.
//!
//! `VE_SW` is process-wide, so every test that pins it holds [`SW`] for as long
//! as it is set: the span reads the pin when it opens on its worker.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ec_h265::Decoder;
use engine::decode::{Backend, DecodeSession, probe};
use engine::demux::{Codec, Demuxer, VideoMeta};

/// 352x288@30, 1 s -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 30;
/// 1920x1080@30, 2 s.
const FRAMES_1080: u32 = 60;

static SW: Mutex<()> = Mutex::new(());

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Runs `f` with the plugin forced off, holding [`SW`] so no other test here sees
/// the pin under it.
fn software<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SW.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    engine::envcfg::set("VE_SW", "1");
    let out = f();
    engine::envcfg::unset("VE_SW");
    out
}

/// Opens a clip on the software seat and drains it.
fn software_frames(path: &Path) -> (VideoMeta, Vec<engine::decode::Frame>) {
    software(|| {
        let (meta, rx) = DecodeSession::open(path).expect("software HEVC open");
        (meta, rx.into_iter().collect())
    })
}

/// The container half: an HEVC track is found in both containers, at its size,
/// its length and its bit depth, which is what picks the 10-bit path.
#[test]
fn the_demuxer_reports_hevc_in_mp4_and_matroska() {
    for (name, depth) in [
        ("test_hevcsw_8.mp4", 8),
        ("test_hevcsw_8.mkv", 8),
        ("test_hevcsw_10.mp4", 10),
        ("test_hevcsw_10.mkv", 10),
    ] {
        let (meta, demuxer) = Demuxer::open(&asset(name)).expect(name);
        assert_eq!(meta.codec, Codec::Hevc, "{name}");
        assert_eq!((meta.width, meta.height), (352, 288), "{name}");
        assert_eq!(meta.frame_count, FRAMES, "{name}: container count");
        assert_eq!(demuxer.bit_depth(), depth, "{name}: bit depth off the hvcC");
    }
}

/// With the plugin off, an HEVC file probes as the software seat, not as a
/// refusal naming the plugin. This is the answer a box without VA-API gets.
#[test]
fn the_software_seat_takes_hevc_with_the_plugin_off() {
    let (codec, backend) = software(|| probe(&asset("test_hevcsw_8.mp4")).expect("probe"));
    assert_eq!(codec, Some(Codec::Hevc));
    assert_eq!(backend, Backend::Software);
}

/// The seat end to end, for 8-bit and Main 10, both containers, an odd size and a
/// GOP that starts on an open CRA: every frame comes out in display order at the
/// clip's size with a picture in it, and a seek lands on the bytes a linear decode
/// delivered.
#[test]
fn the_software_seat_decodes_and_seeks_hevc() {
    let cases = [
        ("test_hevcsw_8.mp4", 352, 288),
        ("test_hevcsw_8.mkv", 352, 288),
        ("test_hevcsw_10.mp4", 352, 288),
        ("test_hevcsw_10.mkv", 352, 288),
        ("test_hevcsw_odd.mp4", 350, 286),
        ("test_hevcsw_cra.mp4", 352, 288),
    ];
    for (name, w, h) in cases {
        let clip = asset(name);
        let (meta, frames) = software_frames(&clip);
        assert_eq!(meta.codec, Codec::Hevc, "{name}");
        assert_eq!(
            frames.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..FRAMES).collect::<Vec<_>>(),
            "{name}: frames must arrive 0..n in display order"
        );
        assert!(
            frames.iter().all(|f| (f.width, f.height) == (w, h)),
            "{name}: every frame at the clip's size"
        );
        let first = &frames[0].bgra;
        assert!(
            first
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[..] != first[..4]),
            "{name}: frame 0 is a single colour -- no picture was decoded"
        );

        // The sync points (the IDR at 0, and the IRAP at 15 that the open-GOP
        // fixture starts its leading pictures on), the frames either side, and the last.
        for target in [0, 1, 14, 15, 16, FRAMES - 1] {
            let want = &frames[target as usize].bgra;
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
                rx.into_iter()
                    .next()
                    .unwrap_or_else(|| panic!("{name}: no frame after a seek to {target}"))
            });
            assert_eq!(
                seeked.index, target,
                "{name}: first frame after seek to {target}"
            );
            assert!(
                seeked.bgra == *want,
                "{name}: seek to {target} differs from a linear decode"
            );
        }
    }
}

/// A clip of `pix_fmt` encoded by ffmpeg's libx265 into the temp dir: 30 frames
/// of 352x288. `assets/` is gitignored, so the clips these tests need are made here.
fn gen_clip(name: &str, pix_fmt: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{name}-{}.mp4", std::process::id()));
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=352x288:rate=30:duration=1")
        .args(["-frames:v", "30", "-c:v", "libx265", "-pix_fmt", pix_fmt])
        .args(["-x265-params", "log-level=error"])
        .arg(&path)
        .output()
        .expect("run ffmpeg: the clips are encoded with it");
    assert!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

/// A 4:2:2 HEVC file opens on the software seat at 8 and 10 bits and every frame
/// comes out: the chroma is averaged to 4:2:0 on the way in, and the plane check
/// against ffmpeg lives beside the seat, in `decode/hevc.rs`.
#[test]
fn a_4_2_2_hevc_file_opens_and_decodes_every_frame_on_the_software_seat() {
    for (name, pix_fmt) in [
        ("hevc422-it-8", "yuv422p"),
        ("hevc422-it-10", "yuv422p10le"),
    ] {
        let clip = gen_clip(name, pix_fmt);
        let (meta, frames) = software(|| {
            let (meta, rx) = DecodeSession::open(&clip)
                .unwrap_or_else(|e| panic!("{name}: the software seat must open 4:2:2: {e}"));
            (meta, rx.into_iter().collect::<Vec<_>>())
        });
        let _ = std::fs::remove_file(&clip);
        assert_eq!(meta.codec, Codec::Hevc, "{name}");
        assert_eq!(frames.len(), 30, "{name}: every frame comes out");
    }
}

/// 4:4:4 opens on the software seat at 8 and 10 bits, and every frame comes out.
#[test]
fn a_4_4_4_hevc_file_opens_and_decodes_every_frame_on_the_software_seat() {
    for (name, pix_fmt) in [("hevc444-it-8", "yuv444p"), ("hevc444-it-10", "yuv444p10le")] {
        let clip = gen_clip(name, pix_fmt);
        Demuxer::open(&clip).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (_, frames) = software_frames(&clip);
        let _ = std::fs::remove_file(&clip);
        assert_eq!(frames.len(), 30, "{name}: every frame comes out");
    }
}

/// The per-frame cost at 1080p, 8-bit: `ec-h265` alone (decode, no conversion)
/// and the whole software seat (decode, crop, narrow, BGRA). Printed for the
/// lane report; every frame must come out.
#[test]
fn the_software_seat_times_1080p_hevc() {
    let clip = asset("test_hevcsw_1080.mp4");

    let (_, mut demuxer) = Demuxer::open(&clip).expect("open the 1080p clip");
    let mut decoder = Decoder::new();
    let (mut decoded, mut spent) = (0u32, Duration::ZERO);
    while let Some(au) = demuxer.next_access_unit().expect("read") {
        let start = Instant::now();
        decoder.decode_annex_b(&au).expect("decode");
        while decoder.next_frame().is_some() {
            decoded += 1;
        }
        spent += start.elapsed();
    }
    let start = Instant::now();
    decoder.flush().expect("flush");
    while decoder.next_frame().is_some() {
        decoded += 1;
    }
    spent += start.elapsed();
    assert_eq!(decoded, FRAMES_1080, "ec-h265 alone releases every frame");
    let per_frame = spent.as_secs_f64() * 1e3 / f64::from(decoded);
    println!("ec-h265 alone, 1080p 8-bit: {per_frame:.2} ms/frame over {decoded} frames");

    let (seat_frames, seat_spent) = software(|| {
        let start = Instant::now();
        let (_, rx) = DecodeSession::open(&clip).expect("software 1080p open");
        let n = rx.into_iter().count();
        (n, start.elapsed())
    });
    assert_eq!(
        seat_frames, FRAMES_1080 as usize,
        "the seat delivers every frame"
    );
    let per_frame = seat_spent.as_secs_f64() * 1e3 / seat_frames as f64;
    println!("software seat, 1080p 8-bit: {per_frame:.2} ms/frame over {seat_frames} frames");
}
