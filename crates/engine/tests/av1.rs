//! AV1 import: the third source codec that is not H.264, and the first that
//! arrives in a container of its own -- `mp4 0.14` knows no `av01` sample entry
//! at all, so AV1 comes in as Matroska and the demuxer walks the EBML itself.
//!
//! The container and refusal checks need nothing installed. The decode and
//! export twins need a built `libengine_hw.so` and a VA-API driver with an AV1
//! decode entrypoint (`vainfo | grep AV1`), so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test av1 -- --include-ignored --nocapture --test-threads=1
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

/// 1280x720@30, 2 s, keyframes 30 apart -- see `scripts/gen_fixtures.sh`.
const FRAMES: u32 = 60;
const KEYFRAME: u32 = 30;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// The container half. Matroska indexes neither frames nor rate, so all three
/// numbers here come out of the walk: the count is the blocks of the track, the
/// rate is `DefaultDuration` in nanoseconds (33333333, i.e. 30 fps to seven
/// digits -- the millisecond timestamps beside it would say 30.30).
#[test]
fn the_demuxer_reports_an_av1_track_in_matroska() {
    let (meta, _) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!(
        (meta.frame_rate - 30.0).abs() < 1e-6,
        "DefaultDuration must not truncate: {}",
        meta.frame_rate
    );
    assert_eq!(meta.frame_count, FRAMES);

    // The invariant, stated as an assert: the mp4 codecs still say themselves.
    let (h264, _) = Demuxer::open(&asset("test_baseline.mp4")).expect("open test_baseline.mp4");
    assert_eq!(h264.codec, Codec::H264);
    let (hevc, _) = Demuxer::open(&asset("test_hevc.mp4")).expect("open test_hevc.mp4");
    assert_eq!(hevc.codec, Codec::Hevc);
    let (vp9, _) = Demuxer::open(&asset("test_vp9.mp4")).expect("open test_vp9.mp4");
    assert_eq!(vp9.codec, Codec::Vp9);
}

/// The depth, which is what picks the surface pool the plugin decodes into: an
/// `av1C` with `high_bitdepth` set reads as 10 and takes the P010 pool HEVC Main
/// 10 already goes through, and the 8-bit fixture beside it still reads as 8.
/// It used to be an outright refusal at open ("10-bit AV1 is not supported").
#[test]
fn a_ten_bit_av1_track_is_read_as_ten_bit() {
    let (meta, ten) = Demuxer::open(&asset("test_av1_10.mkv")).expect("open test_av1_10.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert_eq!(meta.frame_count, FRAMES);
    assert_eq!(ten.bit_depth(), 10, "what picks the P010 surface pool");

    // The invariant: the 8-bit file is untouched by any of it.
    let (_, eight) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(eight.bit_depth(), 8);
}

/// The delivered audio scope, as an assert: a Matroska file's AAC track is read
/// like any other file's (`tests/hevc_mkv.rs` is where that lives), and its
/// *picture* being AV1 changes nothing about it.
#[test]
fn matroska_audio_is_read_whatever_the_picture_is() {
    let path = asset("test_av1.mkv");
    let probe = AudioSession::probe(&path, 0)
        .expect("probe must not fail")
        .expect("the fixture has an AAC track");
    assert_eq!((probe.sample_rate, probe.channels), (44_100, 2));
    let streams = AudioSession::probe_streams(&path).expect("streams");
    assert_eq!(streams.len(), 1, "one readable audio stream: {streams:?}");
    assert!(streams[0].decodable);
}

/// Every block comes back, keyframes carry the sequence header, and the sync
/// index is what a seek lands on. An AV1 decoder cannot start without a
/// sequence header, and `mkv` carries it in `CodecPrivate` -- so this is the
/// check that says the `av1C` record was really parsed and re-injected.
#[test]
fn every_block_comes_back_and_keyframes_carry_the_sequence_header() {
    let (_, mut demuxer) = Demuxer::open(&asset("test_av1.mkv")).expect("open test_av1.mkv");
    let first = demuxer
        .next_access_unit()
        .expect("read")
        .expect("a first access unit");
    // OBU header: bits 6..3 are the type, and 1 is a sequence header.
    assert_eq!(
        (first[0] >> 3) & 0xF,
        1,
        "the keyframe leads with the sequence header"
    );
    assert_eq!(first[0] & 0x2, 2, "obu_has_size_field: low-overhead format");

    let mut count = 1;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(count, FRAMES, "every block came back out");

    // The fixture is keyed every 30 frames, and a seek may only land on one:
    // anywhere in the second GOP rewinds to frame 30, anywhere in the first to
    // frame 0.
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(KEYFRAME + 15),
        i64::from(KEYFRAME)
    );
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(KEYFRAME),
        i64::from(KEYFRAME)
    );
    assert_eq!(demuxer.seek_to_sync_at_or_before(KEYFRAME - 1), 0);
    assert_eq!(demuxer.seek_to_sync_at_or_before(0), 0);
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(FRAMES * 10),
        i64::from(KEYFRAME),
        "past the end clamps to the last sync point"
    );
    // And the access unit after a seek is the keyframe's, sequence header and
    // all -- a decoder restarted mid-file needs it exactly as much.
    let after = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!((after[0] >> 3) & 0xF, 1, "{}", after[0]);
}

/// There is no software AV1 decoder, so the software path must refuse by name
/// rather than feed AV1 bytes to `rusty_h264` -- and it must refuse where a
/// caller can still show it, i.e. out of `open`, not from inside the worker.
#[test]
fn the_software_path_refuses_av1_by_name() {
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused =
        DecodeSession::open(asset("test_av1.mkv")).expect_err("software must not accept AV1");
    let refused = refused.to_string();
    // Restored immediately: the hardware tests in this binary share the process.
    unsafe { std::env::remove_var("VE_SW") };

    assert!(refused.contains("AV1"), "{refused}");
    assert!(refused.contains("plugin"), "{refused}");
}

/// A timeline is *not* refused a source coded differently -- every clip opens
/// its own decoder. What survives is the reason the codec gate existed: on a
/// machine that cannot decode AV1 the file is refused at the door, by the
/// decoder's own name, rather than becoming a clip of black frames.
#[test]
fn a_timeline_takes_the_other_codec_or_names_the_missing_decoder() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av.mp4");
    // The no-plugin machine, forced.
    // SAFETY: the suite is documented to run with --test-threads=1.
    unsafe { std::env::set_var("VE_SW", "1") };
    let refused = session
        .import(&asset("test_av1.mkv"))
        .expect_err("no AV1 decoder means no AV1 clip")
        .to_string();
    unsafe { std::env::remove_var("VE_SW") };
    assert_eq!(refused, Codec::Av1.needs_plugin());
    assert!(
        !refused.contains("H.264"),
        "the timeline's own codec is no longer a reason: {refused}"
    );
    assert_eq!(session.sources().len(), 1, "a refusal left no row");
}

/// The end-to-end user path: opening the file yields pictures, all of them,
/// through the plugin.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn the_plugin_decodes_every_av1_frame() {
    let start = Instant::now();
    let (meta, frames) = DecodeSession::open(asset("test_av1.mkv")).expect("open test_av1.mkv");
    assert_eq!(meta.codec, Codec::Av1);
    let frames: Vec<_> = frames.into_iter().collect();
    eprintln!(
        "test_av1.mkv: {} frames in {:?}",
        frames.len(),
        start.elapsed()
    );

    assert_eq!(frames.len() as u32, FRAMES, "every block decoded");
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

/// ...and the 10-bit file through the very call the window's open door makes: a
/// user opens it and the timeline shows a picture, read back off the P010 pool
/// to the same 8-bit BGRA every other frame arrives in. The whole point of the
/// slice, and the half a container test cannot say anything about.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with 10-bit AV1 decode"]
fn opening_a_ten_bit_av1_file_shows_frames() {
    let mut session =
        PlaybackSession::open(asset("test_av1_10.mkv")).expect("open the 10-bit file");
    assert_eq!(session.meta().codec, Codec::Av1);
    assert_eq!(session.meta().frame_count, FRAMES);

    // The decoder is a thread behind the door, so a frame is waited for exactly
    // as the window's own pump waits for one.
    let deadline = Instant::now() + Duration::from_secs(60);
    let frame = loop {
        if let Some(frame) = session.try_frame() {
            break frame;
        }
        assert!(Instant::now() < deadline, "no frame in 60 s");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!((frame.width, frame.height), (1280, 720));
    // A picture, not a flat surface: a P010 surface read back through the NV12
    // path would come out as noise or as nothing at all.
    assert!(
        frame.bgra.chunks_exact(4).any(|px| px != &frame.bgra[..4]),
        "the first frame is a single colour -- no picture was decoded"
    );
}

/// A seek into the second GOP hands back that very frame, not the keyframe it
/// had to restart from: the demuxer's sync index and the plugin's skip count
/// have to agree, and Matroska is where both are this crate's own doing.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn a_seek_lands_on_the_frame_it_asked_for() {
    let (_, frames, _cancel) =
        DecodeSession::open_range(asset("test_av1.mkv"), KEYFRAME + 7, FRAMES)
            .expect("open at a frame inside the second GOP");
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(frames.len() as u32, FRAMES - KEYFRAME - 7, "to the end");
    assert_eq!(frames[0].index, KEYFRAME + 7, "the frame asked for");
}

/// An mp4 export of a Matroska source carries its sound.
///
/// Three lives, this one. It used to *succeed* silently and write picture with
/// no sound, because this engine could not read a Matroska file's audio at all.
/// Then the sound became readable and this became a refusal by name, because
/// the mp4 path could only *copy* AAC out of an mp4's own sample table and a
/// Matroska file has none. Now the copy that cannot be made is a decode and a
/// re-encode (`export::copy_audio` -> `encode_audio`), so the export carries the
/// film's sound instead of naming what it cannot do with it. The picture half of
/// the same export is [`an_av1_export_reopens_through_our_own_demuxer`].
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn an_mp4_export_of_a_matroska_source_carries_its_sound() {
    let session = PlaybackSession::open(asset("test_av1.mkv")).expect("open test_av1.mkv");
    let meta = *session.meta();
    // A short one: the picture is re-encoded frame by frame and what is under
    // test here is the sound reaching the file, not H.264 throughput.
    let project = Project::single(asset("test_av1.mkv"), 15);
    let out = Scratch::file("ve_export_av1", "mp4");

    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(300), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle
        .result()
        .expect("outcome")
        .expect("an mkv's AAC is decoded and encoded again into the mp4");
    let (audio, chunks) = AudioSession::open(&out)
        .expect("reopen the export")
        .expect("the export has an audio track");
    let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    println!("mkv -> mp4: {} Hz, rms {rms:.4}", audio.sample_rate);
    assert!(rms > 0.001, "the film's sound is in the mp4 (rms {rms})");
    std::fs::remove_file(&out).unwrap();
}

/// A short timeline out of the H.264 fixture, which is what the AV1 export tests
/// below write: `rav1e` is built here without its assembly, so the length of the
/// timeline is the length of the test.
fn short_timeline(frames: f64) -> PlaybackSession {
    let mut session = PlaybackSession::open(asset("test_baseline.mp4")).expect("open the fixture");
    assert!(session.cut_at(frames / 30.0), "cut at frame {frames}");
    assert!(
        session.delete_clip(engine::project::Lane::V1, 1),
        "drop everything after it"
    );
    session
}

fn av1_settings() -> ExportSettings {
    ExportSettings {
        format: engine::export::Format::Av1,
        // The software encoder, on every machine: the hardware twin below is
        // where the plugin's AV1 seat is exercised.
        force_sw: true,
        ..Default::default()
    }
}

fn exported(
    session: &PlaybackSession,
    name: &str,
    settings: &ExportSettings,
    limit: Duration,
) -> Scratch {
    let out = Scratch::file(&format!("ve_av1_{name}"), "mkv");
    let started = Instant::now();
    let handle = session.export_to_with(&out, settings);
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("export");
    assert_eq!(handle.progress(), 1.0, "finished at full progress");
    let spent = started.elapsed().as_secs_f64();
    let frames = (session.timeline_duration() * 30.0).round().max(1.0);
    eprintln!(
        "{name}: {frames} frames in {spent:.2} s = {:.2} ms/frame",
        spent * 1000.0 / frames
    );
    out
}

/// The export half of this slice: AV1 out of the software encoder, into a
/// Matroska file this project's own demuxer walks back. Nothing installed is
/// needed -- the file is *read* as a container here, which is where the
/// hand-written EBML either agrees with the hand-written EBML reader or does not.
#[test]
fn an_av1_export_reopens_through_our_own_demuxer() {
    let session = short_timeline(30.0);
    let out = exported(&session, "sw", &av1_settings(), Duration::from_secs(300));

    let (meta, mut demuxer) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(meta.codec, Codec::Av1, "an AV1 export is AV1");
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert!(
        (meta.frame_rate - 30.0).abs() < 1e-6,
        "DefaultDuration must state the rate exactly: {}",
        meta.frame_rate
    );
    assert_eq!(meta.frame_count, 30, "every timeline frame is a block");

    // The first block is a keyframe and leads with the sequence header -- the
    // demuxer prepends `CodecPrivate` to every keyframe, so this is also the
    // check that the `av1C` record was written and parsed back.
    let first = demuxer.next_access_unit().expect("read").expect("a unit");
    assert_eq!(
        (first[0] >> 3) & 0xF,
        1,
        "the keyframe leads with the sequence header"
    );
    assert_eq!(first[0] & 0x2, 2, "obu_has_size_field: low-overhead format");
    let mut count = 1;
    while demuxer.next_access_unit().expect("read").is_some() {
        count += 1;
    }
    assert_eq!(count, 30, "every block comes back out");
    assert_eq!(
        demuxer.seek_to_sync_at_or_before(29),
        0,
        "one GOP, one sync point"
    );
    std::fs::remove_file(&out).unwrap();
}

/// A timeline with sound, exported as AV1 into **both** containers: the file
/// carries the picture *and* the timeline's audio, and this project's own
/// readers are what say so -- the demuxer for the picture, the audio session for
/// the sound. An AV1 export used to be picture only, which is a file whose sound
/// went missing with nobody told.
///
/// Two frames of 720p: `rav1e` is built here without its assembly, so the length
/// of the timeline is the length of the test, and what is under test is the
/// wiring from the format to the muxer, not the encoder.
#[test]
fn an_av1_export_carries_the_timelines_sound_in_either_container() {
    for (format, ext) in [
        (engine::export::Format::Av1, "mkv"),
        (engine::export::Format::Av1Mp4, "mp4"),
    ] {
        let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open test_av");
        assert!(session.cut_at(2.0 / 30.0), "two frames of it");
        assert!(session.delete_clip(engine::project::Lane::V1, 1));
        let out = Scratch::file("ve_av1_sound", ext);
        let settings = ExportSettings {
            format,
            force_sw: true,
            ..Default::default()
        };
        let handle = session.export_to_with(&out, &settings);
        let started = Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(300), "export hung");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.result().expect("outcome").expect("an AV1 export");

        // The picture, through the demuxer an import comes in by -- an `av01`
        // sample entry in the mp4 and a `V_AV1` track in the Matroska file, both
        // of them written by hand and read back by hand.
        let (meta, _) = Demuxer::open(&out).expect("reopen the export");
        assert_eq!(meta.codec, Codec::Av1, "{ext}: an AV1 export is AV1");
        assert_eq!((meta.width, meta.height), (1280, 720), "{ext}");
        assert_eq!(meta.frame_count, 2, "{ext}: every timeline frame is there");

        // ...and the sound beside it, which is the half that used to be absent.
        let (audio, chunks) = AudioSession::open(&out)
            .expect("reopen for its sound")
            .expect("an AV1 export has an audio track");
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        println!(
            "{ext}: {} Hz x{}, rms {rms:.4}",
            audio.sample_rate, audio.channels
        );
        assert!(rms > 0.001, "{ext}: the sound is in the file (rms {rms})");
        std::fs::remove_file(&out).unwrap();
    }
}

/// An audio-only timeline is refused an AV1 export by name, exactly as it is
/// refused an mp4: every frame of it is a gap, so the file would be black.
#[test]
fn an_av1_export_of_an_audio_only_timeline_is_refused_by_name() {
    let session = PlaybackSession::open(asset("test_tone.wav")).expect("open the tone");
    let out = Scratch::file("ve_av1_refused", "mkv");
    let handle = session.export_to_with(&out, &av1_settings());
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let refused = handle
        .result()
        .expect("outcome")
        .expect_err("an audio-only timeline has no picture to code")
        .to_string();
    assert!(refused.contains("no picture"), "{refused}");
    assert!(refused.contains("AV1"), "{refused}");
    assert!(!out.exists(), "nothing is written for a refusal");
}

/// The whole loop, and the only place it is closed: the file the export wrote is
/// decoded back into pictures, and those pictures are the source's.
#[test]
#[ignore = "needs libengine_hw.so and a VA-API driver with AV1 decode"]
fn an_av1_export_decodes_back_into_the_pictures_that_went_in() {
    let session = short_timeline(30.0);
    let out = exported(
        &session,
        "roundtrip",
        &av1_settings(),
        Duration::from_secs(300),
    );

    let (_, frames) = DecodeSession::open(&out).expect("decode the export");
    let frames: Vec<_> = frames.into_iter().collect();
    assert_eq!(frames.len(), 30, "every written frame decodes back");
    let (_, source, _) = DecodeSession::open_range(asset("test_baseline.mp4"), 0, 30)
        .expect("open the source again");
    let source: Vec<_> = source.into_iter().collect();
    for (i, (written, original)) in frames.iter().zip(&source).enumerate() {
        let diff: f64 = written
            .bgra
            .iter()
            .zip(&original.bgra)
            .map(|(a, b)| f64::from(a.abs_diff(*b)))
            .sum::<f64>()
            / written.bgra.len() as f64;
        assert!(
            diff < 12.0,
            "frame {i} drifted by {diff:.2} from the source"
        );
    }
    std::fs::remove_file(&out).unwrap();
}

/// The hardware seat of the same pair, which is opt-in: `VE_HW_AV1=1` is what
/// enters it, because the vendored encoder reset the GPU of the box this was
/// written on (see `export::Enc::open_av1`). Without that variable this measures
/// the software encoder again, which is the honest outcome rather than a
/// failure, so what it asserts is the file.
#[test]
#[ignore = "needs libengine_hw.so, VE_HW_AV1=1 and a driver whose AV1 encoder survives it"]
fn an_av1_export_runs_on_the_plugin_where_the_gpu_has_one() {
    let session = short_timeline(60.0);
    let settings = ExportSettings {
        format: engine::export::Format::Av1,
        ..Default::default()
    };
    let out = exported(&session, "hw", &settings, Duration::from_secs(600));
    let (meta, _) = Demuxer::open(&out).expect("reopen the export");
    assert_eq!(meta.codec, Codec::Av1);
    assert_eq!(meta.frame_count, 60);
    std::fs::remove_file(&out).unwrap();
}
