//! Export of the sources that only the software seats decode: HEVC at 8 and 10
//! bits, VP9 and VP8. Before `ec-h265`, `ec-vp9` and `ec-vp8` joined export's
//! software path, each of these was refused by name on a machine with no VA-API
//! plugin; now they are decoded by the same streams playback reads.
//!
//! Each source is trimmed to a span that starts off any sync sample and is
//! exported as H.264 in mp4, so the picture goes through the decoder and the
//! encoder both. The output is then read back, and held against ffmpeg's own
//! decode of the source from the same frame on:
//!
//! - the frame count and the duration are the span's own;
//! - the first and last frames are at least 30 dB PSNR from the source.
//!
//! ```text
//! cargo test -p engine --release --test export_software -- --test-threads=1
//! ```
//!
//! The PSNR half needs the `ffmpeg` CLI; without it the test says so and passes,
//! as `tests/hevc_export.rs` does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::demux::{Codec, Demuxer};
use engine::export::{ExportSettings, Format};
use engine::project::Lane;
use engine::scratch::Scratch;

/// The first and last span frames must come out at least this close to ffmpeg's
/// decode of the source.
const MIN_PSNR_DB: f64 = 30.0;

/// The span, in source frames: it starts off the sync grid of every fixture
/// (their keyframes are every 30 or so, and frame 5 is on none of them).
const SPAN: (u32, u32) = (5, 25);

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Runs an export to completion, failing on the deadline rather than hanging.
fn export(session: &PlaybackSession, out: &Path) {
    let settings = ExportSettings {
        format: Format::Mp4,
        ..Default::default()
    };
    let handle = session.export_to_with(out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "the export did not finish in 600 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").expect("the export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
}

/// The source with everything outside `[from, to)` taken off the timeline. Both
/// cuts are made on the timeline, so the export is what is left of it.
fn trimmed(source: &Path, (from, to): (u32, u32)) -> PlaybackSession {
    let mut session = PlaybackSession::open(source).expect("open the source");
    let fps = session.meta().frame_rate;
    assert!(session.cut_at(f64::from(to) / fps), "cut the tail");
    assert!(session.delete_clip(Lane::V1, 1), "drop the tail");
    assert!(session.cut_at(f64::from(from) / fps), "cut the head");
    assert!(session.delete_clip(Lane::V1, 0), "drop the head");
    session
}

/// Every frame of `path` as ffmpeg decodes it, planar 8-bit 4:2:0 at `width` x
/// `height`, one `Vec` per frame in display order. `None` where ffmpeg is not
/// installed or fails.
fn decoded_frames(path: &Path, (width, height): (u32, u32)) -> Option<Vec<Vec<u8>>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-an",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let frame = (width * height * 3 / 2) as usize;
    Some(out.stdout.chunks_exact(frame).map(<[u8]>::to_vec).collect())
}

/// PSNR between two frames of planar 8-bit I420, over every sample.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let squares: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = i64::from(*x) - i64::from(*y);
            (d * d) as u64
        })
        .sum();
    if squares == 0 {
        return f64::INFINITY;
    }
    let mse = squares as f64 / a.len() as f64;
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// Exports `[SPAN)` of the fixture at `name` and checks the file that comes out.
/// `codec` is what the fixture must be, so the test is about the decoder it
/// names and not a fixture that quietly became another codec.
fn reencodes_the_span(name: &str, codec: Codec) {
    let source = asset(name);
    let (meta, _) = Demuxer::open(&source).expect("open the fixture");
    assert_eq!(meta.codec, codec, "{name} is {:?}", meta.codec);
    let (from, to) = SPAN;
    assert!(meta.frame_count >= to, "{name} is shorter than the span");

    let session = trimmed(&source, SPAN);
    let out = Scratch::file(&format!("ve_sw_{}", name.replace('.', "_")), "mp4");
    export(&session, &out);

    // The container: the span's own count, at the source's size, and the
    // duration that count makes at the source's rate.
    let (written, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(
        written.frame_count,
        to - from,
        "{name}: the span's frames are the export's frames"
    );
    assert_eq!(
        (written.width, written.height),
        (meta.width, meta.height),
        "{name}: the export is at the source's size"
    );
    let duration = f64::from(written.frame_count) / written.frame_rate;
    let want = f64::from(to - from) / meta.frame_rate;
    assert!(
        (duration - want).abs() <= 1.0 / meta.frame_rate,
        "{name}: a {duration:.4} s export of a {want:.4} s span"
    );

    // The pictures: the first and the last frame against ffmpeg's decode of the
    // source, the same frames in the same order.
    let size = (meta.width, meta.height);
    let (Some(source_frames), Some(out_frames)) =
        (decoded_frames(&source, size), decoded_frames(&out, size))
    else {
        eprintln!("no ffmpeg: skipping the PSNR half of {name}");
        return;
    };
    let want = &source_frames[from as usize..to as usize];
    assert_eq!(
        out_frames.len(),
        want.len(),
        "{name}: ffmpeg reads back every frame of the export"
    );
    let first = psnr(&out_frames[0], &want[0]);
    let last = psnr(&out_frames[out_frames.len() - 1], &want[want.len() - 1]);
    eprintln!(
        "{name}: {} frames, PSNR first {first:.2} dB, last {last:.2} dB",
        want.len()
    );
    assert!(first >= MIN_PSNR_DB, "{name}: first frame at {first:.2} dB");
    assert!(last >= MIN_PSNR_DB, "{name}: last frame at {last:.2} dB");
}

#[test]
fn an_hevc_8_bit_source_is_exported_through_the_software_decoder() {
    reencodes_the_span("test_hevcsw_8.mkv", Codec::Hevc);
}

#[test]
fn an_hevc_10_bit_source_is_exported_through_the_software_decoder() {
    reencodes_the_span("test_hevcsw_10.mkv", Codec::Hevc);
}

#[test]
fn a_vp9_source_is_exported_through_the_software_decoder() {
    reencodes_the_span("test_vp9.webm", Codec::Vp9);
}

#[test]
fn a_vp8_source_is_exported_through_the_software_decoder() {
    reencodes_the_span("test_vp8.webm", Codec::Vp8);
}
