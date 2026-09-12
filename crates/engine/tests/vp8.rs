//! VP8 import: the container half, and the decode half -- software, on the
//! libvpx the plugin dlopens (`engine-hw`'s `vpx.rs`), which is why these
//! twins need a built `libengine_hw.so` and a `libvpx.so` but **no VA-API
//! VP8 profile**: no such profile exists on this GPU, and none is asked for.
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test vp8 -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide, hence `--test-threads=1`: the refusal test sets it
//! and puts it back.
use std::path::PathBuf;
use std::time::{Duration, Instant};

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

/// There is no software VP8 decoder *in this binary*, so the software path
/// must refuse by name rather than feed VP8 bytes to `rusty_h264` -- and it
/// must refuse where a caller can still show it, i.e. out of `open`, not from
/// inside the worker. The decoder the refusal names is the plugin's libvpx
/// arm, which is why the sentence names the plugin and not a nonexistent
/// absence.
#[test]
fn the_software_path_refuses_vp8_by_name() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused =
        DecodeSession::open(asset("test_vp8.mp4")).expect_err("software must not accept VP8");
    let refused = refused.to_string();
    // Restored immediately: the hardware tests in this binary share the process.
    unsafe { std::env::remove_var("VE_SW") };

    assert!(refused.contains("VP8"), "{refused}");
    assert!(refused.contains("plugin"), "{refused}");
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin's libvpx arm. The `.webm` is the file this decodes,
/// the container half being the Matroska demuxer's `V_VP8` row.
#[test]
#[ignore = "needs a built libengine_hw.so and libvpx.so.9 -- no VA-API profile involved"]
fn the_plugin_decodes_every_vp8_frame() {
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
        first.chunks_exact(4).any(|px| px != &first[..4]),
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
#[ignore = "needs a built libengine_hw.so and libvpx.so.9 -- no VA-API profile involved"]
fn a_vp8_source_exports_as_h264() {
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
    assert!((written.frame_rate - 30.0).abs() < 0.01, "{}", written.frame_rate);
    assert_eq!((written.width, written.height), (1280, 720));
    let _ = std::fs::remove_file(&out);
}
