//! VP9 import: the first source codec that is not H.264.
//!
//! The container and refusal checks need nothing installed. The decode and
//! export twins need a built `libengine_hw.so` and a VA-API driver with a VP9
//! decode entrypoint (`vainfo | grep VP9`), so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test vp9 -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide, hence `--test-threads=1`: the refusal test sets it
//! and puts it back so the hardware twins below really are hardware.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::export::ExportSettings;
use engine::{AudioSession, DecodeSession, PlaybackSession, Project};

/// 1280x720@30, 2 s -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The container half: a vp09 track is found, described and named as VP9, and
/// its AAC track is read exactly as any other file's -- the audio path knows
/// nothing about the video codec and this is what says so.
#[test]
fn the_demuxer_reports_a_vp9_track() {
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

/// There is no software VP9 decoder, so the software path must refuse by name
/// rather than feed VP9 bytes to `rusty_h264` -- and it must refuse where a
/// caller can still show it, i.e. out of `open`, not from inside the worker.
#[test]
fn the_software_path_refuses_vp9_by_name() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused =
        DecodeSession::open(asset("test_vp9.mp4")).expect_err("software must not accept VP9");
    let refused = refused.to_string();
    // Restored immediately: the hardware tests in this binary share the process.
    unsafe { std::env::remove_var("VE_SW") };

    assert!(refused.contains("VP9"), "{refused}");
    assert!(refused.contains("plugin"), "{refused}");
    // H.264 is untouched by all of this.
    unsafe { std::env::set_var("VE_SW", "1") };
    let h264 = DecodeSession::open(asset("test_baseline.mp4")).is_ok();
    unsafe { std::env::remove_var("VE_SW") };
    assert!(h264, "software H.264 decode still opens");
}

/// A timeline is *not* refused a source coded differently -- every clip opens
/// its own decoder. What survives is the reason the codec gate existed: on a
/// machine that cannot decode VP9 the file is refused at the door, by the
/// decoder's own name, rather than becoming a clip of black frames.
#[test]
fn a_timeline_takes_the_other_codec_or_names_the_missing_decoder() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    // The no-plugin machine, forced.
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused = session
        .import(&asset("test_vp9.mp4"))
        .expect_err("no VP9 decoder means no VP9 clip")
        .to_string();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(refused, Codec::Vp9.needs_plugin());
    assert!(
        !refused.contains("H.264"),
        "the timeline's own codec is no longer a reason: {refused}"
    );
    assert_eq!(session.sources().len(), 1, "a refusal left no row");

    // ...and where a decoder exists, it simply joins.
    match session.import(&asset("test_vp9.mp4")) {
        Ok(_) => assert_eq!(session.sources().len(), 2, "VP9 beside H.264"),
        Err(e) => assert_eq!(e.to_string(), Codec::Vp9.needs_plugin()),
    }
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with VP9 decode"]
fn the_plugin_decodes_every_vp9_frame() {
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
        first.chunks_exact(4).any(|px| px != &first[..4]),
        "frame 0 is a single colour -- no picture was decoded"
    );
}

/// Export re-encodes to H.264 whatever the source was coded with: the packet
/// copy path is audio-only, so no VP9 byte can reach an `avc1` track.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with VP9 decode"]
fn a_vp9_source_exports_as_h264() {
    let session = PlaybackSession::open(asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    let meta = *session.meta();
    let project = Project::single(asset("test_vp9.mp4"), meta.frame_count);
    let out = std::env::temp_dir().join(format!("ve_export_vp9_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
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
