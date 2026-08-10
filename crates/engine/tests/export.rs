//! Export round trips: an edited timeline out to mp4 and straight back in
//! through our own demuxer and decoder.
//!
//! The software test is the default one and needs nothing installed. The
//! hardware twins need a built `libengine_hw.so` plus a VA-API driver with an
//! H.264 encode entrypoint, so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test export -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Both env pins are process-wide, hence `--test-threads=1` for the ignored run.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant};

use engine::export::ExportSettings;
use engine::hw::HwEncoder;
use engine::mux::{Mp4Muxer, VideoParams, parameter_sets};
use engine::project::Lane;
use engine::project::Source;
use engine::{DecodeSession, ExportHandle, PlaybackSession, Project};

const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Per-run unique: two suite runs at once (a release sweep beside a debug one,
/// or two agents) would otherwise write and delete each other's export.
fn out_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ve_export_{name}_{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(part_path(&path));
    path
}

/// What the worker actually writes to until it succeeds; the engine appends the
/// suffix rather than replacing the extension.
fn part_path(out: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", out.display()))
}

/// Pins both paths to software for the whole test binary, so the default suite
/// proves the fallback that every machine has.
fn pin_software() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
}

/// Removes both pins, for the hardware twins.
fn pin_hardware() {
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::remove_var("VE_SW");
        std::env::remove_var("VE_SW_ENC");
    });
}

/// `[0,30) + [60,150)` of the source: two cuts and a delete, so the export has
/// to seek, and the join is a real discontinuity rather than a formality.
fn edited(source: &Path) -> PlaybackSession {
    let mut session = PlaybackSession::open(source).expect("open source");
    assert!(session.cut_at(30.0 / FPS), "cut at frame 30");
    assert!(session.cut_at(60.0 / FPS), "cut at frame 60");
    assert!(session.delete_clip(1), "drop the middle clip");
    assert_eq!(session.clip_spans().len(), 2);
    session
}

fn wait(handle: &ExportHandle, limit: Duration) -> engine::Result<()> {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(
            started.elapsed() < limit,
            "export did not finish in {limit:?}"
        );
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

/// The whole point of the slice: an edited timeline survives the trip out to
/// disk and back, frame for frame and picture for picture.
fn round_trip(name: &str, limit: Duration) {
    let source = asset("test_baseline.mp4");
    let mut session = edited(&source);
    session.pause();
    let out = out_path(name);

    let started = Instant::now();
    let handle = session.export_to(&out);
    wait(&handle, limit).expect("export");
    let spent = started.elapsed();
    println!(
        "{name}: 120 frames in {:.2} s = {:.2} ms/frame",
        spent.as_secs_f64(),
        spent.as_secs_f64() * 1000.0 / 120.0
    );
    assert_eq!(handle.progress(), 1.0, "finished at full progress");

    let (meta, _) = engine::demux::Demuxer::open(&out).expect("reopen export");
    assert_eq!(meta.frame_count, 120, "timeline frames written");
    assert_eq!((meta.width, meta.height), (1280, 720));
    assert_eq!(meta.frame_rate.round(), FPS);

    let file = File::open(&out).unwrap();
    let size = file.metadata().unwrap().len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let duration = reader.duration().as_secs_f64();
    assert!(
        (duration - 120.0 / FPS).abs() <= 1.0 / FPS,
        "duration {duration} s is more than a frame off 4.0 s"
    );

    let frames = decode_all(&out);
    assert_eq!(frames.len(), 120, "every written frame decodes back");

    // Timeline -> source: [0,30) maps to itself, [30,120) to [60,150).
    for (timeline, source_frame) in [(0u32, 0u32), (59, 89), (119, 149)] {
        let diff = mean_abs_diff(&frames[timeline as usize], &frame_at(&source, source_frame));
        println!("timeline {timeline} vs source {source_frame}: mean abs diff {diff:.2}");
        assert!(
            diff < 6.0,
            "timeline frame {timeline} drifted by {diff:.2} from source frame {source_frame}"
        );
    }
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn exports_an_edited_timeline_in_software() {
    pin_software();
    round_trip("sw", Duration::from_secs(120));
}

/// Same trip through the plugin, and the only place the end-to-end hardware
/// cost (decode + encode + mux) is measured.
#[test]
#[ignore]
fn exports_an_edited_timeline_in_hardware() {
    pin_hardware();
    round_trip("hw", Duration::from_secs(60));
}

/// The trap this slice exists to close: a timeline playing a file's *second*
/// audio stream must export that stream's packets. Playing one stream and
/// exporting another is a file that sounds nothing like what was edited, and
/// nothing in it would say so -- so the check is the copied bytes, not just the
/// header. Software only: the picture is beside the point here.
#[test]
fn exports_the_audio_stream_the_timeline_plays() {
    pin_software();
    let multi = asset("test_multiaudio.mp4");
    let (meta, _) = engine::demux::Demuxer::open(&multi).expect("open the fixture");
    let whole = engine::Clip {
        start: 0,
        in_frame: 0,
        out_frame: meta.frame_count,
        source: 0,
        link: Some(0),
    };
    // Stream 1 is the 22.05 kHz mono French track; stream 0 is 44.1 kHz
    // stereo. A project may only hold streams that agree, so this timeline is
    // stream 1 alone -- which is exactly what an export must carry.
    let project = Project::from_parts(
        vec![Source {
            path: multi.clone(),
            audio_stream: 1,
        }],
        vec![
            (engine::project::LaneKind::Video, vec![whole]),
            (engine::project::LaneKind::Audio, vec![whole]),
        ],
    )
    .expect("a one-source project on stream 1");
    let out = out_path("stream1");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    wait(&handle, Duration::from_secs(120)).expect("export");

    let (audio, _) = engine::AudioSession::open(&out)
        .expect("reopen export audio")
        .expect("the export has audio");
    assert_eq!(
        (audio.sample_rate, audio.channels),
        (22_050, 1),
        "the export carries stream 0's parameters, not the stream that played"
    );

    // Byte for byte: the exported track is the copy of stream 1, and it is not
    // the copy of stream 0 -- the second assert is what makes the first one
    // mean something.
    let segs = [(Some(0), 0.0, f64::from(meta.frame_count) / FPS)];
    let copy = |stream| {
        engine::AudioSession::copy_multi_streams(&[(multi.clone(), stream)], &segs)
            .expect("copy")
            .expect("has audio")
            .1
            .into_iter()
            .map(|p| p.bytes)
            .collect::<Vec<_>>()
    };
    let (one, zero) = (copy(1), copy(0));
    assert_ne!(one, zero, "the two streams are not the same bytes");

    let file = File::open(&out).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let track = *reader
        .tracks()
        .keys()
        .find(|id| reader.sample_count(**id).unwrap_or(0) as usize == one.len())
        .unwrap_or(&2);
    let written: Vec<Vec<u8>> = (1..=reader.sample_count(track).expect("audio track"))
        .map(|id| {
            reader
                .read_sample(track, id)
                .expect("sample")
                .expect("present")
                .bytes
                .to_vec()
        })
        .collect();
    assert_eq!(written, one, "the exported packets are stream 1's");
    std::fs::remove_file(&out).unwrap();
}

/// `test_av[0,120)` then the whole of `test_av2`: the last second of the first
/// source is deleted, so timeline frame 120 is both a cut and a file change --
/// the join the multi-source export exists for.
fn two_sources() -> Project {
    let (av, _) = engine::demux::Demuxer::open(&asset("test_av.mp4")).expect("open test_av");
    let (av2, _) = engine::demux::Demuxer::open(&asset("test_av2.mp4")).expect("open test_av2");
    assert_eq!((av.width, av.height), (av2.width, av2.height), "policy a");

    let mut project = Project::single(asset("test_av.mp4"), av.frame_count);
    let second = project.import(asset("test_av2.mp4"), 0);
    assert_eq!(second, 1, "a second file is a second source");
    assert!(project.append_clip(second, av2.frame_count));
    assert!(project.split(120), "cut a second before the end of test_av");
    assert!(project.delete(1), "drop test_av's tail");
    assert_eq!(project.timeline_frames(), 120 + av2.frame_count);
    assert_eq!(
        project.clips().iter().map(|c| c.source).collect::<Vec<_>>(),
        vec![0, 1]
    );
    project
}

/// A timeline spanning two files exports as one: every clip is decoded from its
/// own source, the audio is copied across the join, and the result reopens as a
/// single stream whose frame at the join is the second file's first frame.
fn multi_source_round_trip(name: &str, limit: Duration) {
    let project = two_sources();
    let total = project.timeline_frames();
    let (meta, _) = engine::demux::Demuxer::open(&asset("test_av.mp4")).unwrap();
    let out = out_path(name);

    let started = Instant::now();
    let handle = engine::export::start(project.clone(), meta, &out, &ExportSettings::default());
    wait(&handle, limit).expect("export");
    println!(
        "{name}: {total} frames of two sources in {:.2} s",
        started.elapsed().as_secs_f64()
    );

    let (written, _) = engine::demux::Demuxer::open(&out).expect("reopen export");
    assert_eq!(written.frame_count, total, "timeline frames written");
    assert_eq!((written.width, written.height), (1280, 720));

    let frames = decode_all(&out);
    assert_eq!(
        frames.len() as u32,
        total,
        "every written frame decodes back"
    );
    // The join: timeline 120 is test_av2's frame 0, timeline 119 still test_av's.
    for (timeline, source, source_frame) in [
        (0u32, "test_av.mp4", 0u32),
        (119, "test_av.mp4", 119),
        (120, "test_av2.mp4", 0),
        (total - 1, "test_av2.mp4", total - 121),
    ] {
        let diff = mean_abs_diff(
            &frames[timeline as usize],
            &frame_at(&asset(source), source_frame),
        );
        println!("timeline {timeline} vs {source} {source_frame}: mean abs diff {diff:.2}");
        assert!(
            diff < 6.0,
            "timeline frame {timeline} drifted by {diff:.2} from {source} frame {source_frame}"
        );
    }

    // Audio across the join: 1 + 172 + 173. Both segments ask for 4.0 s =
    // 172.27 packets, and the head is test_av's own priming packet (the first
    // segment's alone -- the join must not add a second one). The first segment
    // copies 172 and owes 272 samples, and it is that *carried* debt that makes
    // the second copy 173: rounding each source on its own would give 345, so
    // this number is the proof the accumulator survives a source change.
    let file = File::open(&out).unwrap();
    let size = file.metadata().unwrap().len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let packets = reader.sample_count(2).expect("audio track");
    assert_eq!(packets, 346, "packet total across the source join");
    let wanted = f64::from(total) / FPS * 44_100.0;
    assert!(
        (f64::from(packets * 1024) - wanted).abs() < 3.0 * 1024.0,
        "audio length {} samples drifted from {wanted}",
        packets * 1024
    );
    let (audio, _) = engine::AudioSession::open(&out)
        .expect("reopen export audio")
        .expect("export has an audio track");
    assert_eq!((audio.sample_rate, audio.channels), (44_100, 2));
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn exports_two_sources_as_one_timeline_in_software() {
    pin_software();
    multi_source_round_trip("multi_sw", Duration::from_secs(180));
}

#[test]
#[ignore]
fn exports_two_sources_as_one_timeline_in_hardware() {
    pin_hardware();
    multi_source_round_trip("multi_hw", Duration::from_secs(60));
}

/// The bitrate setting is the one knob a user turns for quality, so it has to
/// reach the encoder: the same 120-frame timeline at 1, 4 and 8 Mbps has to come
/// back as three strictly growing files. `force_sw` pins the software encoder,
/// whose rate control is the one every machine has.
#[test]
fn a_higher_bitrate_writes_a_bigger_file() {
    pin_software();
    let mut sizes = Vec::new();
    for bitrate in [1_000_000u64, 4_000_000, 8_000_000] {
        let mut session = edited(&asset("test_baseline.mp4"));
        session.pause();
        let out = out_path(&format!("bitrate_{bitrate}"));
        let settings = ExportSettings {
            bitrate: Some(bitrate),
            force_sw: true,
        };
        let handle = session.export_to_with(&out, &settings);
        wait(&handle, Duration::from_secs(180)).expect("export");
        let size = std::fs::metadata(&out).expect("export exists").len();
        println!("{} Mbps: {size} bytes", bitrate / 1_000_000);
        sizes.push(size);
        std::fs::remove_file(&out).unwrap();
    }
    assert!(
        sizes[0] < sizes[1] && sizes[1] < sizes[2],
        "file sizes {sizes:?} are not strictly growing with the bitrate"
    );
}

/// A source that disappears between the edit and the export fails the export
/// rather than silently writing a shorter file -- and leaves neither an output
/// nor a `.part`. Both sources are silent, so the failure is the per-clip open
/// (the audio pass never touches the second file).
#[test]
fn a_vanished_source_fails_the_export() {
    pin_software();
    let baseline = asset("test_baseline.mp4");
    let doomed = std::env::temp_dir().join("ve_export_vanished_source.mp4");
    std::fs::copy(&baseline, &doomed).expect("copy a second source");
    let (meta, _) = engine::demux::Demuxer::open(&baseline).unwrap();

    // Five frames of each: the export has to get going and then fail.
    let mut project = Project::single(&baseline, meta.frame_count);
    assert!(project.split(5));
    assert!(project.delete(1));
    let second = project.import(&doomed, 0);
    assert!(project.append_clip(second, meta.frame_count));
    assert!(project.split(10));
    assert!(project.delete(2));
    assert_eq!(project.timeline_frames(), 10);

    std::fs::remove_file(&doomed).expect("unlink the second source");
    let out = out_path("vanished");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default());
    let result = wait(&handle, Duration::from_secs(60));
    let error = result.expect_err("an export of a missing file cannot succeed");
    println!("vanished source: {error}");
    assert!(
        !out.exists(),
        "a failed export still wrote {}",
        out.display()
    );
    assert!(!part_path(&out).exists(), "the .part outlived the failure");
}

/// A cancelled export leaves nothing behind -- the half-written file is the
/// thing a user would otherwise try to play.
#[test]
fn cancelling_leaves_no_file() {
    pin_software();
    let mut session = edited(&asset("test_baseline.mp4"));
    session.pause();
    let out = out_path("cancel");

    let handle = session.export_to(&out);
    let started = Instant::now();
    while handle.progress() < 0.1 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "export never got going"
        );
        assert!(
            !handle.is_finished(),
            "export finished before it was cancelled"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.cancel();
    let result = wait(&handle, Duration::from_secs(5));
    assert!(result.is_err(), "a cancelled export is an error");
    assert!(!out.exists(), "no partial file survives a cancel");
    assert!(!part_path(&out).exists(), "the .part is cleaned up too");
}

/// The late window: cancel after the last frame, while the export is draining,
/// writing audio and closing the file. Progress already reads 100% there, and
/// before the cancel checkpoints outside the frame loop the export completed
/// anyway and left a file the user never asked for.
///
/// Landing in that window is a race, so the attempt is repeated; whichever side
/// of it an attempt lands on, the invariant is the same -- a finished file or no
/// file, never a `.part`.
#[test]
fn cancelling_after_the_last_frame_leaves_no_file() {
    pin_software();
    let out = out_path("late");
    let part = part_path(&out);
    for attempt in 1..=5 {
        let mut session = edited(&asset("test_baseline.mp4"));
        session.pause();
        let handle = session.export_to(&out);
        // Sleep while there is time to sleep, then spin: the window is the few
        // milliseconds between the last frame and `finish`.
        let started = Instant::now();
        while handle.progress() < 0.9 && !handle.is_finished() {
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "export stalled"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        while handle.progress() < 1.0 && !handle.is_finished() {
            std::hint::spin_loop();
        }
        handle.cancel();

        let result = wait(&handle, Duration::from_secs(30));
        assert!(!part.exists(), "attempt {attempt} left a .part behind");
        if result.is_err() {
            assert!(!out.exists(), "a late cancel still wrote {}", out.display());
            return;
        }
        // The export beat the cancel; then it must be a whole file, not a stump.
        assert_eq!(
            engine::demux::Demuxer::open(&out).unwrap().0.frame_count,
            120
        );
        std::fs::remove_file(&out).unwrap();
    }
    panic!("cancel never landed in the drain/audio/finish window");
}

/// Audio: the copied AAC packets have to land in the export as a track a reader
/// can open again, at a length the video agrees with.
#[test]
#[ignore]
fn exports_audio_alongside_the_video() {
    pin_hardware();
    let source = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    // One cut: the timeline is unchanged but the audio copy now runs as two
    // segments, so the packet-boundary rounding at the join is exercised.
    assert!(session.cut_at(2.0));
    session.pause();
    let timeline = session.timeline_duration();
    let out = out_path("av");

    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(120)).expect("export");

    let file = File::open(&out).unwrap();
    let size = file.metadata().unwrap().len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let packets = reader.sample_count(2).expect("audio track");
    let expected = engine::AudioSession::copy_segments(&source, &[(0.0, 2.0), (2.0, 5.0)])
        .unwrap()
        .expect("source has audio")
        .1
        .len() as u32;
    assert_eq!(packets, expected, "every copied packet reached the file");
    // Independently of the copier: the rounding stays inside one packet per join
    // plus the priming packet it starts with.
    let samples = f64::from(packets * 1024);
    let wanted = timeline * 44_100.0;
    println!("audio: {packets} packets = {samples} samples for {wanted} of timeline");
    assert!(
        (samples - wanted).abs() < 3.0 * 1024.0,
        "audio length {samples} drifted from {wanted}"
    );

    let (meta, chunks) = engine::AudioSession::open(&out)
        .expect("reopen export audio")
        .expect("export has an audio track");
    assert_eq!(meta.sample_rate, 44_100);
    assert_eq!(meta.channels, 2);
    let decoded: usize = chunks.into_iter().map(|c| c.samples.len()).sum();
    assert!(
        decoded as f64 / 2.0 > wanted - 3.0 * 1024.0,
        "only {decoded} samples decoded back out"
    );

    let (video, _) = engine::demux::Demuxer::open(&out).unwrap();
    assert_eq!(video.frame_count, 150, "video track unaffected by the cut");
    std::fs::remove_file(&out).unwrap();
}

/// Hardware-encoded access units had never been through the muxer before this
/// slice: the Mesa slice-header fixup and the parameter-set placement were only
/// ever proven against a raw decoder.
#[test]
#[ignore]
fn hardware_access_units_repack_into_mp4() {
    let (w, h) = (640u32, 480u32);
    let mut encoder =
        HwEncoder::open(w, h, 30, 1, 4_000_000).expect("no hardware encode plugin/driver");
    let out = out_path("hwmux");
    let mut muxer: Option<Mp4Muxer> = None;
    let mut written = 0;
    for index in 0..10u32 {
        let (y, u, v) = synthetic(index, w as usize, h as usize);
        let Some(au) = encoder.encode(&y, &u, &v, w, h, false).expect("encode") else {
            continue;
        };
        if muxer.is_none() {
            let (sps, pps) = parameter_sets(au).expect("first access unit carries SPS+PPS");
            muxer = Some(
                Mp4Muxer::create(
                    &out,
                    &VideoParams {
                        width: w,
                        height: h,
                        frame_rate: 30.0,
                        sps,
                        pps,
                    },
                    None,
                )
                .expect("create"),
            );
        }
        muxer.as_mut().unwrap().write_video_au(au).expect("write");
        written += 1;
    }
    while let Some(au) = encoder.drain().expect("drain") {
        muxer.as_mut().unwrap().write_video_au(au).expect("write");
        written += 1;
    }
    muxer.unwrap().finish().expect("finish");
    assert_eq!(written, 10);

    let (meta, mut demuxer) = engine::demux::Demuxer::open(&out).unwrap();
    assert_eq!(meta.frame_count, 10);
    let mut decoder = rusty_h264::Decoder::new();
    let mut decoded = 0;
    while let Some(au) = demuxer.next_access_unit().unwrap() {
        if decoder
            .decode(&au)
            .expect("software decode of the remuxed stream")
            .is_some()
        {
            decoded += 1;
        }
    }
    assert_eq!(decoded, 10, "every remuxed picture decodes again");
    std::fs::remove_file(&out).unwrap();
}

/// A moving diagonal gradient: something that costs bits, so the encoder cannot
/// pass the check by emitting skip frames.
fn synthetic(index: u32, width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut y = vec![0u8; width * height];
    for row in 0..height {
        for col in 0..width {
            y[row * width + col] = (row + col + index as usize * 5) as u8;
        }
    }
    (
        y,
        vec![64u8.wrapping_add(index as u8); cw * ch],
        vec![192u8.wrapping_sub(index as u8); cw * ch],
    )
}

/// A timeline whose *first* second of sound was lifted. A reader drops the
/// first packet of an AAC track as encoder priming, so a track that opens on a
/// hole has to carry one extra packet of silence for it to drop -- without it
/// the drop comes out of the hole and every sound after the hole plays 23 ms
/// early, which is a lip sync error for the whole export.
#[test]
fn a_leading_audio_gap_keeps_the_sound_after_it_in_place() {
    pin_software();
    let source = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    assert!(session.cut_at(1.0), "cut at 1 s");
    assert!(session.lift_clip(Lane::A1, 0), "lift the first second");
    session.pause();
    let out = out_path("leading_gap");
    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(180)).expect("export");

    let (_, chunks) = engine::AudioSession::open(&out)
        .expect("reopen export audio")
        .expect("export has an audio track");
    let mut pcm: Vec<f32> = Vec::new();
    while let Ok(chunk) = chunks.recv_timeout(Duration::from_secs(10)) {
        pcm.resize(chunk.start_sample as usize * 2, 0.0);
        pcm.extend_from_slice(&chunk.samples);
    }
    // Where the sound starts, measured in whole packets so the MDCT leak either
    // side of the splice cannot be mistaken for the onset: the first block
    // carrying a quarter of the export's own peak.
    let block = 1024 * 2;
    let peak = |b: &[f32]| b.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let loud = peak(&pcm) / 4.0;
    let onset = pcm
        .chunks(block)
        .position(|b| peak(b) > loud)
        .expect("the export has no sound at all") as f64
        * 1024.0;
    println!("leading gap: sound starts at sample {onset} of an expected 44100");
    assert!(
        (onset - 44_100.0).abs() <= 1024.0,
        "the sound after the gap is at {onset}, not at the 1 s hole's end"
    );
    // ...and the hole really is a hole, not the first second played quietly.
    let quiet = peak(&pcm[..(0.9 * 44_100.0) as usize * 2]);
    assert!(quiet < 1e-3, "the lifted second is not silent: {quiet}");

    std::fs::remove_file(&out).unwrap();
}

/// A timeline with a hole in each lane, exported. The video gap is encoded as
/// black rather than skipped -- the file is as long as the timeline, frame for
/// frame -- and the audio gap is copied as real silent packets, so the audio
/// track covers the whole timeline inside the bresenham bound instead of ending
/// a second early.
#[test]
fn exports_a_gapped_timeline_as_black_and_silence() {
    pin_software();
    let source = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    assert!(session.cut_at(1.0), "split at 1 s");
    assert!(session.cut_at(2.0), "split at 2 s");
    // Lift the middle out of *both* lanes: one second of black and silence in
    // the middle of a five-second timeline, with the length unchanged.
    assert!(session.lift_clip(Lane::V1, 1));
    assert!(session.lift_clip(Lane::A1, 1));
    session.pause();
    let timeline = session.timeline_duration();
    let frames = (timeline * FPS).round() as u32;
    let out = out_path("gapped");

    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(180)).expect("export");

    // Every frame is there, and the ones over the hole are black.
    let (video, _) = engine::demux::Demuxer::open(&out).unwrap();
    assert_eq!(
        video.frame_count, frames,
        "a gap is encoded, not skipped: {} frames for a {timeline:.3}s timeline",
        video.frame_count
    );
    let pictures = decode_all(&out);
    assert_eq!(pictures.len() as u32, frames);
    let (hole, hole_end) = ((1.0 * FPS) as usize, (2.0 * FPS) as usize);
    // Encoded black, so not bit-exact: a mean absolute distance from true black
    // well under one code value per channel is what "black" can mean here.
    let black = [0u8, 0, 0, 255].repeat(pictures[0].len() / 4);
    for (i, picture) in pictures.iter().enumerate() {
        let diff = mean_abs_diff(picture, &black);
        if (hole..hole_end).contains(&i) {
            assert!(
                diff < 4.0,
                "frame {i} of the gap is not black (MAE {diff:.2})"
            );
        }
    }
    let lit = mean_abs_diff(&pictures[0], &black);
    assert!(lit > 4.0, "the clips around the gap came out black too");

    // The audio track spans the whole timeline in packets: the hole is real
    // silent access units, so the bresenham bound the copier promises -- half a
    // packet per join, carried through the gap -- covers the gap too.
    let file = File::open(&out).unwrap();
    let size = file.metadata().unwrap().len();
    let reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
    let count = reader.sample_count(2).expect("audio track");
    let samples = f64::from(count * 1024);
    let wanted = timeline * 44_100.0;
    println!("gapped export: {count} packets = {samples} samples for {wanted}");
    assert!(
        (samples - wanted).abs() < 3.0 * 1024.0,
        "audio length {samples} drifted from {wanted} across the gap"
    );

    // ...and the silence is really silent, where the lift was.
    let (_, chunks) = engine::AudioSession::open(&out)
        .expect("reopen export audio")
        .expect("export has an audio track");
    let mut pcm: Vec<f32> = Vec::new();
    while let Ok(chunk) = chunks.recv_timeout(Duration::from_secs(10)) {
        pcm.resize(chunk.start_sample as usize * 2, 0.0);
        pcm.extend_from_slice(&chunk.samples);
    }
    let window = |from: f64, to: f64| {
        let (a, b) = ((from * 44_100.0) as usize * 2, (to * 44_100.0) as usize * 2);
        pcm[a.min(pcm.len())..b.min(pcm.len())]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    // Inside the hole, off its own edges: AAC is lapped, so the packets either
    // side of a splice carry a little of their neighbour.
    let quiet = window(1.2, 1.8);
    assert!(quiet < 1e-3, "the gap is not silent: {quiet}");
    assert!(window(0.1, 0.9) > 1e-2, "the sound before it went missing");
    assert!(window(2.2, 4.9) > 1e-2, "the sound after it went missing");

    std::fs::remove_file(&out).unwrap();
}
