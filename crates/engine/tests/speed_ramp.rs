//! A speed ramp through the export: a clip whose speed climbs from 1x to 2x over its
//! first 60 timeline frames is written with the length the ramp gives, and the picture
//! at its midpoint is the source frame the ramp's integral names -- not the one a
//! constant speed would show there.
//!
//! `test_baseline.mp4` is 150 frames of 1280x720 at 30 fps. Its ramp covers 90 source
//! frames in its first 60 timeline frames, then runs the last 60 source frames at 2x in
//! 30 more, so the export is 90 frames.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::project::{Lane, Speed, SpeedKey, ramp_frames, ramp_source_at};
use engine::scratch::Scratch;
use engine::{DecodeSession, ExportHandle, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < limit, "export did not finish in {limit:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("a finished export has an outcome")
}

/// Every frame of `path`, BGRA, in timeline order.
fn decode_all(path: &Path) -> Vec<Vec<u8>> {
    let (_, frames) = DecodeSession::open(path).expect("open export");
    frames.into_iter().map(|frame| frame.bgra).collect()
}

/// One frame of `path` by absolute index.
fn frame_at(path: &Path, index: u32) -> Vec<u8> {
    let (_, frames, _) = DecodeSession::open_at(path, index).expect("open source");
    frames.recv().expect("frame present").bgra
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "frame sizes differ");
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len() as f64
}

#[test]
fn a_ramped_clip_exports_the_ramps_length_and_its_midpoint_shows_the_ramps_source_frame() {
    engine::envcfg::set("VE_SW", "1");
    engine::envcfg::set("VE_SW_ENC", "1");
    let source = asset("test_baseline.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    session.pause();
    let len = session.lane_clips(Lane::V1)[0].len();
    assert_eq!(len, 150, "the baseline is 150 frames");

    let keys = [
        SpeedKey { at: 0, speed: Speed::NORMAL },
        SpeedKey { at: 60, speed: Speed::from_permille(2000) },
    ];
    session.set_speed_ramp(Lane::V1, 0, &keys).expect("the ramp sets");
    let clip = session.lane_clips(Lane::V1)[0].clone();
    assert_eq!(clip.frames(), 90, "150 source frames at a ramp that ends at 2x");
    assert_eq!(clip.frames(), ramp_frames(&keys, len), "the footprint is the ramp's");

    let out = Scratch::file("ve_speed_ramp", "mp4");
    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(600)).expect("export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");

    let (meta, _) = engine::demux::Demuxer::open(&out).expect("reopen export");
    assert_eq!(meta.frame_count, 90, "the timeline frames written are the ramp's length");

    let frames = decode_all(&out);
    assert_eq!(frames.len(), 90, "every written frame decodes back");

    // Timeline frame 30 shows source frame floor(S(30)) = 37; a constant 1x would show 30.
    let midpoint = ramp_source_at(&keys, 30.0).floor() as u32;
    assert_eq!(midpoint, 37);
    let want = frame_at(&source, midpoint);
    let unramped = frame_at(&source, 30);
    let got = &frames[30];
    let near = mean_abs_diff(got, &want);
    let constant = mean_abs_diff(got, &unramped);
    println!("midpoint: vs source {midpoint} {near:.2}, vs source 30 {constant:.2}");
    assert!(near < 6.0, "midpoint drifted by {near:.2} from source frame {midpoint}");
    assert!(near < constant, "the midpoint is the ramp's frame, not the constant speed's");

    std::fs::remove_file(&out).ok();
}
