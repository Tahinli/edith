//! HEVC import: the second source codec that is not H.264, and the first whose
//! parameter sets have to be dug out of the container by hand (`mp4 0.14` reads
//! one byte of `hvcC` and skips the rest).
//!
//! The container and refusal checks need nothing installed. The decode and
//! export twins need a built `libengine_hw.so` and a VA-API driver with an HEVC
//! decode entrypoint (`vainfo | grep HEVC`), so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test hevc -- --include-ignored --nocapture --test-threads=1
//! ```
//!
//! `VE_SW` is process-wide, hence `--test-threads=1`: the refusal test sets it
//! and puts it back so the hardware twins below really are hardware.

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

/// The container half: a hev1 track is found, described and named as HEVC, and
/// its AAC track is read exactly as any other file's.
#[test]
fn the_demuxer_reports_an_hevc_track() {
    let (meta, _) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(meta.codec, Codec::Hevc);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!((meta.frame_rate - 30.0).abs() < 0.01, "{}", meta.frame_rate);
    assert_eq!(meta.frame_count, FRAMES);

    let probe = AudioSession::probe(asset("test_hevc.mp4"), 0)
        .expect("probe")
        .expect("the fixture has an AAC track");
    assert_eq!(
        (probe.sample_rate, probe.channels),
        (44_100, 2),
        "audio is read the same whatever the picture is coded with"
    );

    // The invariant, stated as an assert: the older codecs still say themselves.
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
    let (vp9, _) = Demuxer::open(&asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(vp9.codec, Codec::Vp9);
}

/// The part `mp4 0.14` cannot do: every access unit is Annex-B framed, and the
/// sync ones carry the VPS, SPS and PPS read out of `hvcC` by hand. Without
/// those three a hardware decoder has nothing to configure itself from, so this
/// is the check that says the box was really parsed and not merely found.
#[test]
fn every_sync_sample_carries_annex_b_vps_sps_and_pps() {
    let (_, mut demuxer) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    let first = demuxer
        .next_access_unit()
        .expect("read")
        .expect("a first access unit");

    // NAL types out of the two-byte HEVC NAL header: bits 6..1 of the first byte.
    let types: Vec<u8> = first
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == [0, 0, 0, 1])
        .map(|(i, _)| (first[i + 4] >> 1) & 0x3f)
        .collect();
    assert!(types.starts_with(&[32, 33, 34]), "{types:?}");
    assert!(
        types.iter().any(|&t| (16..=21).contains(&t)),
        "the first sample must also carry an IRAP slice: {types:?}"
    );

    let mut count = 1;
    while let Some(au) = demuxer.next_access_unit().expect("read") {
        assert_eq!(&au[..4], [0, 0, 0, 1], "sample {count} is not Annex-B");
        count += 1;
    }
    assert_eq!(count, FRAMES, "every sample came back out");
}

/// There is no software HEVC decoder, so the software path must refuse by name
/// rather than feed HEVC bytes to `rusty_h264` -- and it must refuse where a
/// caller can still show it, i.e. out of `open`, not from inside the worker.
#[test]
fn the_software_path_refuses_hevc_by_name() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused =
        DecodeSession::open(asset("test_hevc.mp4")).expect_err("software must not accept HEVC");
    let refused = refused.to_string();
    // Restored immediately: the hardware tests in this binary share the process.
    unsafe { std::env::remove_var("VE_SW") };

    assert!(refused.contains("HEVC"), "{refused}");
    assert!(refused.contains("plugin"), "{refused}");
    // H.264 is untouched by all of this.
    unsafe { std::env::set_var("VE_SW", "1") };
    let h264 = DecodeSession::open(asset("test_baseline.mp4")).is_ok();
    unsafe { std::env::remove_var("VE_SW") };
    assert!(h264, "software H.264 decode still opens");
}

/// A timeline is *not* refused a source coded differently -- every clip opens
/// its own decoder, so H.264 and HEVC share one timeline. What survives is the
/// reason the codec gate existed: on a machine that cannot decode HEVC the file
/// is refused at the door, by the decoder's own name, rather than becoming a
/// clip of black frames with the complaint on stderr.
#[test]
fn a_timeline_takes_the_other_codec_or_names_the_missing_decoder() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    // The no-plugin machine, forced: the refusal must still arrive, and it must
    // be the decoder's words and not the timeline's.
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused = session
        .import(&asset("test_hevc.mp4"))
        .expect_err("no HEVC decoder means no HEVC clip")
        .to_string();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(refused, Codec::Hevc.needs_plugin());
    assert!(
        !refused.contains("H.264"),
        "the timeline's own codec is no longer a reason: {refused}"
    );
    assert_eq!(session.sources().len(), 1, "a refusal left no row");

    // ...and where a decoder exists, it simply joins.
    match session.import(&asset("test_hevc.mp4")) {
        Ok(_) => assert_eq!(session.sources().len(), 2, "HEVC beside H.264"),
        Err(e) => assert_eq!(e.to_string(), Codec::Hevc.needs_plugin()),
    }
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with HEVC decode"]
fn the_plugin_decodes_every_hevc_frame() {
    let start = Instant::now();
    let (meta, frames) = DecodeSession::open(asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(meta.codec, Codec::Hevc);
    let frames: Vec<_> = frames.into_iter().collect();
    eprintln!(
        "test_hevc.mp4: {} frames in {:?}",
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

/// The mixed-codec timeline, end to end: an H.264 take joins an HEVC one, both
/// spans really decode (a decoder each, opened at the span), and the export --
/// which re-encodes -- writes every frame of both. The refusal that used to
/// stand here would have made this whole timeline impossible.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with HEVC decode"]
fn an_h264_take_plays_and_exports_on_an_hevc_timeline() {
    let hevc = asset("test_hevc.mp4");
    let h264 = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&hevc).expect("open test_hevc.mp4");
    session.set_gain(0.0);
    session.import(&h264).expect("H.264 joins an HEVC timeline");
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &h264, 0, None)
            .expect("a file just imported is on this timeline"),
        "drag it onto the end"
    );
    assert_eq!(session.clip_spans().len(), 2, "two takes, two codecs");

    // One picture out of each span, which is one decoder each: the HEVC clip
    // through the plugin, the H.264 one through either backend.
    for (at, what) in [(1.0, "the HEVC take"), (end + 1.0, "the H.264 take")] {
        session.seek(at);
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = loop {
            if let Some(frame) = session.try_frame() {
                break frame;
            }
            assert!(Instant::now() < deadline, "no frame from {what}");
            std::thread::sleep(Duration::from_millis(4));
        };
        assert_eq!(
            (frame.width, frame.height),
            session.resolution(),
            "{what} came out at the project size"
        );
    }

    // ...and the export -- the one a front-end starts, off the session itself --
    // re-encodes both to one H.264 track.
    let frames =
        (session.timeline_duration() * f64::from(session.meta().frame_rate as f32)).round() as u32;
    let out = Scratch::file("ve_export_mixed", "mp4");
    let handle = session.export_to_with(&out, &ExportSettings::default());
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(240), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("the mixed export");
    let (written, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(written.codec, Codec::H264, "exports are always H.264");
    assert_eq!(written.frame_count, frames, "both takes were written");
    let _ = std::fs::remove_file(&out);
}

/// Export re-encodes to H.264 whatever the source was coded with: the packet
/// copy path is audio-only, so no HEVC byte can reach an `avc1` track.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with HEVC decode"]
fn an_hevc_source_exports_as_h264() {
    let session = PlaybackSession::open(asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    let meta = *session.meta();
    let project = Project::single(asset("test_hevc.mp4"), meta.frame_count);
    let out = Scratch::file("ve_export_hevc", "mp4");

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
