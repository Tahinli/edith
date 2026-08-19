//! `hvc1`-tagged HEVC: the same stream `tests/hevc.rs` reads, in the sample
//! entry Apple and ffmpeg's mov muxer actually write. `mp4 0.14` recognises only
//! `hev1`, so the crate reports no video track at all for these files and the
//! fourcc is read out of `stsd` by hand (`demux::sample_entry`).
//!
//! The container and refusal checks need nothing installed; the decode and
//! export twins need a built `libengine_hw.so` and a VA-API driver with an HEVC
//! decode entrypoint, so they are `#[ignore]`d -- same invocation as the `hevc`
//! suite:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hevc_hvc1 -- --include-ignored --test-threads=1
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::demux::{Codec, Demuxer};
use engine::export::ExportSettings;
use engine::scratch::Scratch;
use engine::{AudioSession, DecodeSession, PlaybackSession, Project};

/// 1280x720@30, 2 s -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;
const HVC1: &str = "test_hevc_hvc1.mp4";

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The gap this suite exists for: an hvc1 file used to read back as "no H.264,
/// HEVC or VP9 video track". It is described exactly as its hev1 twin now,
/// parameter sets included -- those come out of `hvcC`, which hvc1 carries just
/// as hev1 does.
#[test]
fn the_demuxer_reports_an_hvc1_track_as_hevc() {
    let (meta, mut demuxer) = Demuxer::open(&asset(HVC1)).expect("open the hvc1 fixture");
    assert_eq!(meta.codec, Codec::Hevc);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!((meta.frame_rate - 30.0).abs() < 0.01, "{}", meta.frame_rate);
    assert_eq!(meta.frame_count, FRAMES);

    let probe = AudioSession::probe(asset(HVC1), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!((probe.sample_rate, probe.channels), (44_100, 2));

    // The sample tables the mp4 crate parsed anyway are the ones being read: the
    // dropped sample entry costs nothing but the fourcc.
    let first = demuxer
        .next_access_unit()
        .expect("read")
        .expect("a first access unit");
    let types: Vec<u8> = first
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == [0, 0, 0, 1])
        .map(|(i, _)| (first[i + 4] >> 1) & 0x3f)
        .collect();
    assert!(types.starts_with(&[32, 33, 34]), "VPS/SPS/PPS: {types:?}");
    let mut count = 1;
    while let Some(au) = demuxer.next_access_unit().expect("read") {
        assert_eq!(&au[..4], [0, 0, 0, 1], "sample {count} is not Annex-B");
        count += 1;
    }
    assert_eq!(count, FRAMES, "every sample came back out");

    // The invariant: hev1 and the other codecs still say themselves.
    let (hev1, _) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(hev1.codec, Codec::Hevc);
    assert_eq!(hev1.frame_count, FRAMES);
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
    let (vp9, _) = Demuxer::open(&asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(vp9.codec, Codec::Vp9);
}

/// Being found is not being decodable: there is no software HEVC decoder, so the
/// software path must refuse an hvc1 file by name too, out of `open` where a
/// caller can still show it.
///
/// `VE_SW` is process-wide, hence the suite's `--test-threads=1`.
#[test]
fn the_software_path_refuses_an_hvc1_file_by_name() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused = DecodeSession::open(asset(HVC1)).expect_err("software must not accept HEVC");
    unsafe { std::env::remove_var("VE_SW") };
    let refused = refused.to_string();
    assert!(refused.contains("HEVC"), "{refused}");
    assert!(refused.contains("plugin"), "{refused}");
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with HEVC decode"]
fn the_plugin_decodes_every_hvc1_frame() {
    let (meta, frames) = DecodeSession::open(asset(HVC1)).expect("open the hvc1 fixture");
    assert_eq!(meta.codec, Codec::Hevc);
    let frames: Vec<_> = frames.into_iter().collect();
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

/// Export re-encodes to H.264 whatever the source was tagged with.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with HEVC decode"]
fn an_hvc1_source_exports_as_h264() {
    let session = PlaybackSession::open(asset(HVC1)).expect("open the hvc1 fixture");
    let meta = *session.meta();
    let project = Project::single(asset(HVC1), meta.frame_count);
    let out = Scratch::file("ve_export_hvc1", "mp4");

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
