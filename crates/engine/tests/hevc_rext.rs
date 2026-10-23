//! HEVC range extensions on the software seat: 4:0:0, 4:2:2 and 4:4:4 at 8, 10
//! and 12 bits, made by ffmpeg's libx265 into the temp dir (never personal
//! media). Each opens, probes with its real chroma and depth, plays every frame
//! in order, seeks onto the bytes a linear decode gave, and exports.
//!
//! The plane-for-plane PSNR against ffmpeg lives beside the seat, in
//! `decode/hevc.rs`.
//!
//! ```text
//! cargo test -p engine --release --test hevc_rext
//! ```

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::colorspace::Chroma;
use engine::decode::{DecodeSession, probe};
use engine::demux::{Codec, Demuxer};
use engine::export::ExportSettings;
use engine::scratch::Scratch;

const FRAMES: u32 = 30;

/// (pix_fmt, chroma, luma bit depth)
const SHAPES: [(&str, Chroma, u8); 8] = [
    ("yuv444p", Chroma::C444, 8),
    ("yuv444p10le", Chroma::C444, 10),
    ("yuv444p12le", Chroma::C444, 12),
    ("yuv422p12le", Chroma::C422, 12),
    ("yuv422p10le", Chroma::C422, 10),
    ("yuv422p", Chroma::C422, 8),
    ("gray", Chroma::Gray, 8),
    ("gray10le", Chroma::Gray, 10),
];

static SW: Mutex<()> = Mutex::new(());

fn software<T>(f: impl FnOnce() -> T) -> T {
    let _guard = SW.lock().unwrap_or_else(|p| p.into_inner());
    engine::envcfg::set("VE_SW", "1");
    let out = f();
    engine::envcfg::unset("VE_SW");
    out
}

/// 30 frames of 352x288 with a sync point every ten.
fn make_clip(pix_fmt: &str) -> Scratch {
    let clip = Scratch::file(&format!("hevc_rext_{pix_fmt}"), "mp4");
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=352x288:rate=30:duration=1")
        .args(["-frames:v", "30", "-c:v", "libx265", "-pix_fmt", pix_fmt])
        .args(["-x265-params", "log-level=error:keyint=10:min-keyint=10"])
        .arg(&*clip)
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    clip
}

#[test]
fn every_rext_shape_opens_and_probes_with_its_real_chroma_and_depth() {
    for (pix, chroma, depth) in SHAPES {
        let clip = make_clip(pix);
        let (meta, demuxer) = Demuxer::open(&clip).unwrap_or_else(|e| panic!("{pix}: {e}"));
        assert_eq!(meta.codec, Codec::Hevc, "{pix}");
        assert_eq!(demuxer.chroma(), Some(chroma), "{pix}");
        assert_eq!(demuxer.bit_depth(), depth, "{pix}");
        probe(&clip).unwrap_or_else(|e| panic!("{pix}: probe refused: {e}"));
        // Hardware cannot decode these profiles, so the seat is software whatever
        // the plugin is, and the span still delivers.
        let (_, rx) = DecodeSession::open(&clip).unwrap_or_else(|e| panic!("{pix}: {e}"));
        assert_eq!(rx.into_iter().count(), FRAMES as usize, "{pix}: unpinned seat decodes");
    }
}

#[test]
fn rext_streams_play_in_order_and_seeks_match_a_linear_decode() {
    for (pix, _, _) in SHAPES {
        let clip = make_clip(pix);
        let frames: Vec<_> = software(|| {
            let (_, rx) = DecodeSession::open(&clip).expect("open");
            rx.into_iter().collect()
        });
        assert_eq!(
            frames.iter().map(|f| f.index).collect::<Vec<_>>(),
            (0..FRAMES).collect::<Vec<_>>(),
            "{pix}: display order"
        );
        let first = &frames[0].bgra;
        assert!(
            first.as_chunks::<4>().0.iter().any(|px| px[..] != first[..4]),
            "{pix}: frame 0 holds a picture"
        );
        for target in [0, 9, 10, 11, 20, FRAMES - 1] {
            let seeked = software(|| {
                let (_, rx, _cancel) = DecodeSession::open_at(&clip, target).expect("open_at");
                rx.into_iter().next().expect("a frame after the seek")
            });
            assert_eq!(seeked.index, target, "{pix}");
            assert!(
                seeked.bgra == frames[target as usize].bgra,
                "{pix}: seek to {target} differs from a linear decode"
            );
        }
    }
}

#[test]
fn rext_streams_export() {
    for (pix, _, _) in SHAPES {
        let clip = make_clip(pix);
        let mut session = PlaybackSession::open(&*clip).unwrap_or_else(|e| panic!("{pix}: {e}"));
        assert!(session.cut_at(5.0 / 30.0), "{pix}: cut");
        assert!(session.delete_clip(engine::project::Lane::V1, 1));
        let out = Scratch::file(&format!("hevc_rext_out_{pix}"), "mp4");
        let handle = session.export_to_with(&out, &ExportSettings::default());
        let started = Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(300), "{pix}: export hung");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.result().expect("an outcome").unwrap_or_else(|e| panic!("{pix}: export: {e}"));
        let (meta, _) = Demuxer::open(Path::new(&*out)).expect("the export reopens");
        assert_eq!(meta.frame_count, 5, "{pix}");
        assert_eq!((meta.width, meta.height), (352, 288), "{pix}");
    }
}
