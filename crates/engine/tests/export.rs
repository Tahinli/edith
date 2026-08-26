//! Export round trips: an edited timeline out to mp4 and straight back in
//! through our own demuxer and decoder.
//!
//! The software test is the default one and needs nothing installed. The
//! hardware twins need a built `libengine_hw.so` plus a VA-API driver with an
//! H.264 encode entrypoint, so they are `#[ignore]`d:
//!
//! ```text
//! cargo build -p engine-hw --release
//! LD_LIBRARY_PATH=target/release \
//!   cargo test -p engine --release --test export -- --ignored --nocapture --test-threads=1
//! ```
//!
//! That build is its own step and stays its own step: `--tests` (or building
//! `-p engine` alongside) does **not** rebuild the cdylib, so the plugin beside
//! the test binary is whatever was there last time -- a stale one, silently, and
//! every hardware claim measured against it is about code nobody edited.
//!
//! Both env pins are process-wide, hence `--test-threads=1` for the ignored run.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

/// The two proxy tests below share one film, and a proxy job is one per film:
/// run together, whichever asks second is refused ("already being made"), and
/// a cancel in one deletes what the other is asserting about. One at a time.
static PROXY_FILM: Mutex<()> = Mutex::new(());

use engine::export::ExportSettings;
use engine::hw::HwEncoder;
use engine::mux::{AudioParams, Mp4Muxer, VideoParams, parameter_sets};
use engine::project::{Lane, Source, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::{DecodeSession, ExportHandle, PlaybackSession, Project};

const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Its own directory, gone with the value: two suite runs at once (a release
/// sweep beside a debug one, or two agents) would otherwise write and delete
/// each other's export, and nothing may outlive the test that wrote it.
fn out_path(name: &str) -> Scratch {
    Scratch::file(&format!("ve_export_{name}"), "mp4")
}

/// What the worker actually writes to until it succeeds; the engine appends the
/// suffix rather than replacing the extension.
fn part_path(out: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", out.display()))
}

/// Which seat an export takes is decided by *process-wide* environment
/// variables, so it is a shared resource and every test in this binary borrows
/// it rather than assuming it.
///
/// Readers share: the whole software suite wants the same pin and may run at
/// once, exactly as it always did. A test that has to *change* the pin --
/// there is one family, the encoder-death ones at the end of this file -- takes
/// it exclusively, so no export is ever in flight while the seat is being
/// cycled underneath it. A borrow held only for the seat's opening would not do:
/// [`engine::export::start`] falls back and re-opens mid-run, so the pin has to
/// hold for the whole export.
static SEAT: std::sync::RwLock<()> = std::sync::RwLock::new(());

type Shared = std::sync::RwLockReadGuard<'static, ()>;

/// Pins both paths to software for the whole test binary, so the default suite
/// proves the fallback that every machine has. Bind the guard: it is the borrow
/// above, and dropping it early puts the export back in the race this exists to
/// end.
#[must_use = "the seat pin holds only while its guard is alive"]
fn pin_software() -> Shared {
    let borrowed = SEAT.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::set_var("VE_SW_ENC", "1");
    });
    borrowed
}

/// Removes both pins, for the hardware twins -- which run in their own
/// `--ignored --test-threads=1` pass, so they never share this binary with the
/// software ones.
#[must_use = "the seat pin holds only while its guard is alive"]
fn pin_hardware() -> Shared {
    let borrowed = SEAT.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    static PIN: Once = Once::new();
    PIN.call_once(|| unsafe {
        std::env::remove_var("VE_SW");
        std::env::remove_var("VE_SW_ENC");
    });
    borrowed
}

/// `[0,30) + [60,150)` of the source: two cuts and a delete, so the export has
/// to seek, and the join is a real discontinuity rather than a formality.
fn edited(source: &Path) -> PlaybackSession {
    let mut session = PlaybackSession::open(source).expect("open source");
    assert!(session.cut_at(30.0 / FPS), "cut at frame 30");
    assert!(session.cut_at(60.0 / FPS), "cut at frame 60");
    assert!(session.delete_clip(Lane::V1, 1), "drop the middle clip");
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

/// What an outside decoder complains about while reading every packet of
/// `path`: empty when it read the file clean. `None` where ffmpeg is not
/// installed, which is a skip rather than a failure -- the same posture the
/// colour tests take.
fn ffmpeg_complaints(path: &Path) -> Option<String> {
    let said = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&said.stderr).trim().to_string())
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
    let _seat = pin_software();
    round_trip("sw", Duration::from_secs(600));
}

/// Same trip through the plugin, and the only place the end-to-end hardware
/// cost (decode + encode + mux) is measured.
#[test]
#[ignore]
fn exports_an_edited_timeline_in_hardware() {
    let _seat = pin_hardware();
    round_trip("hw", Duration::from_secs(60));
}

/// An export is written at the **project's** resolution, whatever sizes the
/// media on the timeline are -- and with the same geometry that was watched.
///
/// 1280x720 and 640x360 clips on a 960x720 (4:3) project: every frame comes out
/// 960x720, and both clips are letterboxed with 90 rows of bar at each edge,
/// which is exactly what `scale::fit_rect` places and what playback shows
/// (`playback.rs: a_fitted_clip_is_letterboxed_and_a_filled_one_is_not`).
#[test]
fn exports_at_the_project_resolution_with_the_watched_geometry() {
    let _seat = pin_software();
    let clip = |source: usize, start: u32| engine::Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start,
        in_frame: 0,
        out_frame: 10,
        source,
        link: Some(source as u32),
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let clips = vec![clip(0, 0), clip(1, 10)];
    let project = Project::from_parts(
        vec![
            Source::new(asset("test_baseline.mp4"), 0),
            Source::new(asset("test_mismatch.mp4"), 0),
        ],
        vec![
            (engine::project::LaneKind::Video, clips.clone()),
            (engine::project::LaneKind::Audio, clips),
        ],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("a two-source project");
    // The project's own resolution, which is neither source's: this is the
    // value `PlaybackSession` keeps in its meta and hands the exporter.
    let meta = engine::VideoMeta {
        width: 960,
        height: 720,
        frame_rate: FPS,
        frame_count: 20,
        codec: engine::Codec::H264,
        color: Default::default(),
    };
    let out = out_path("mixed");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
    wait(&handle, Duration::from_secs(600)).expect("export");

    let (written, _) = engine::demux::Demuxer::open(&out).expect("reopen export");
    assert_eq!(
        (written.width, written.height),
        (960, 720),
        "an export is the project's size, not any source's"
    );
    assert_eq!(written.frame_count, 20);

    let frames = decode_all(&out);
    assert_eq!(frames.len(), 20);
    // Lossy: a bar is black plus whatever the encoder left, so the bars are
    // asserted as *flat and dark* rather than as byte 0, and the picture rows
    // as neither.
    let mean = |frame: &[u8], y: usize| {
        frame[y * 960 * 4..][..960 * 4]
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .map(f64::from)
            .sum::<f64>()
            / (960.0 * 3.0)
    };
    for (at, what) in [(5usize, "the 1280x720 clip"), (15, "the 640x360 clip")] {
        let frame = &frames[at];
        assert!(mean(frame, 2) < 8.0, "{what}: top bar is not black");
        assert!(mean(frame, 717) < 8.0, "{what}: bottom bar is not black");
        assert!(
            mean(frame, 360) > 20.0,
            "{what}: the picture area is black, so nothing was composed"
        );
    }
    std::fs::remove_file(&out).unwrap();
}

/// The trap this slice exists to close: a timeline playing a file's *second*
/// audio stream must export that stream's packets. Playing one stream and
/// exporting another is a file that sounds nothing like what was edited, and
/// nothing in it would say so -- so the check is the copied bytes, not just the
/// header. Software only: the picture is beside the point here.
#[test]
fn exports_the_audio_stream_the_timeline_plays() {
    let _seat = pin_software();
    let multi = asset("test_multiaudio.mp4");
    let (meta, _) = engine::demux::Demuxer::open(&multi).expect("open the fixture");
    let whole = engine::Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start: 0,
        in_frame: 0,
        out_frame: meta.frame_count,
        source: 0,
        link: Some(0),
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
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
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("a one-source project on stream 1");
    let out = out_path("stream1");
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
    wait(&handle, Duration::from_secs(600)).expect("export");

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
    let handle = engine::export::start(project.clone(), meta, &out, &ExportSettings::default(), None);
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
    let _seat = pin_software();
    multi_source_round_trip("multi_sw", Duration::from_secs(900));
}

#[test]
#[ignore]
fn exports_two_sources_as_one_timeline_in_hardware() {
    let _seat = pin_hardware();
    multi_source_round_trip("multi_hw", Duration::from_secs(60));
}

/// The bitrate setting is the one knob a user turns for quality, so it has to
/// reach the encoder: the same 120-frame timeline at 1, 4 and 8 Mbps has to come
/// back as three strictly growing files. `force_sw` pins the software encoder,
/// whose rate control is the one every machine has.
#[test]
fn a_higher_bitrate_writes_a_bigger_file() {
    let _seat = pin_software();
    let mut sizes = Vec::new();
    for bitrate in [1_000_000u64, 4_000_000, 8_000_000] {
        let mut session = edited(&asset("test_baseline.mp4"));
        session.pause();
        let out = out_path(&format!("bitrate_{bitrate}"));
        let settings = ExportSettings {
            bitrate: Some(bitrate),
            seat: engine::export::EncoderSeat::Software,
            ..Default::default()
        };
        let handle = session.export_to_with(&out, &settings);
        wait(&handle, Duration::from_secs(900)).expect("export");
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
    let _seat = pin_software();
    let baseline = asset("test_baseline.mp4");
    let doomed = Scratch::file("ve_export_vanished_source", "mp4");
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
    let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
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
    let _seat = pin_software();
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
    let _seat = pin_software();
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
                started.elapsed() < Duration::from_secs(600),
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
    let _seat = pin_hardware();
    let source = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    // One cut: the timeline is unchanged but the audio copy now runs as two
    // segments, so the packet-boundary rounding at the join is exercised.
    assert!(session.cut_at(2.0));
    session.pause();
    let timeline = session.timeline_duration();
    let out = out_path("av");

    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(600)).expect("export");

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

/// One throwaway IDR slice NAL -- not a real picture (nothing here decodes
/// it), just something `Mp4Muxer::write_video_au` accepts and ffprobe can
/// see as one packet on the video track.
fn fake_au(n: u8) -> Vec<u8> {
    vec![0, 0, 1, 0x65, n, n.wrapping_add(1), 0xA5]
}

/// `Mp4Muxer` holds coded pictures back until the sound can be interleaved
/// under them ([`Mp4Muxer::hold`], mux.rs), so that a streaming reader sees
/// both tracks advancing together rather than the whole picture and only
/// then the whole track. This pins the boundary the muxer's own doc comment
/// on `hold` states in words: how tightly the two interleave depends on
/// *when* the mix lands relative to the picture, which on a real export is a
/// race against a mixer thread this test does not want to run. So it forces
/// the landing itself -- writing straight to `Mp4Muxer`, exactly the shape
/// `export::run`'s `feed_late_audio` uses it in (a burst of video, then the
/// whole mix handed over in one tight loop, then the rest of the picture) --
/// rather than the real threaded export.
#[test]
fn mp4_interleaves_a_mix_that_lands_early_under_the_rest_of_the_picture() {
    let out = out_path("interleave");
    let (width, height, fps) = (64u32, 64u32, 30.0);
    let (sps, pps) = ([0x67, 0x42, 0x00, 0x1E], [0x68, 0xCE, 0x3C, 0x80]);
    let mut muxer = Mp4Muxer::create(
        &out,
        &VideoParams {
            width,
            height,
            frame_rate: fps,
            sps: &sps,
            pps: &pps,
        },
        None,
    )
    .expect("create");

    // 15 frames (0.5 s) of a head start for the picture -- the file always
    // wrote this stretch video-only, mix or no mix, and this test is about
    // the file *after* it.
    for i in 0..15u8 {
        muxer.write_video_au(&fake_au(i)).expect("write video");
    }
    muxer
        .add_audio_track(&AudioParams {
            freq_index: 3, // mp4::SampleFreqIndex::Freq48000
            chan_conf: 2,  // ChannelConfig::Stereo
            sample_rate: 48_000,
            opus_pre_skip: None,
        })
        .expect("declare audio");
    // The whole mix, handed over in one tight loop exactly as
    // `export::feed_late_audio` does: 200 packets of 1024/48000 s each is
    // ~4.27 s, well past the picture's 0.5 s head start, so releasing it
    // correctly needs the *rest* of the picture below to pace it out.
    let silence = [0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c];
    for _ in 0..200u32 {
        muxer.write_audio_packet(&silence).expect("write audio");
    }
    // The rest of the picture: 135 more frames, 4.5 s, what the mix above is
    // paced against.
    for i in 15..150u8 {
        muxer.write_video_au(&fake_au(i)).expect("write video");
    }
    muxer.write_subtitles(&[]).expect("no subtitles");
    muxer.finish().expect("finish");

    assert_no_single_stream_span_over(&out, 2.0);
    std::fs::remove_file(&out).unwrap();
}

/// Reads `path`'s packets back with ffprobe (pts + byte position) and fails
/// if any run of file-order-consecutive same-stream packets spans more media
/// time than `seconds` -- the numeric shape of "no byte-span longer than 2 s
/// of media contains packets from only one stream".
fn assert_no_single_stream_span_over(path: &Path, seconds: f64) {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts_time,pos",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let mut packets: Vec<(i64, u32, f64)> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split(',');
            let stream: u32 = fields.next().unwrap().parse().unwrap();
            let pts: f64 = fields.next().unwrap().parse().unwrap();
            let pos: i64 = fields.next().unwrap().parse().unwrap();
            (pos, stream, pts)
        })
        .collect();
    assert!(packets.len() > 10, "ffprobe saw only {} packets", packets.len());
    packets.sort_by_key(|&(pos, ..)| pos);

    let streams: std::collections::HashSet<u32> = packets.iter().map(|&(_, s, _)| s).collect();
    assert_eq!(
        streams.len(),
        2,
        "expected one video and one audio stream, ffprobe saw {streams:?}"
    );

    let (mut run_stream, mut run_start) = (packets[0].1, packets[0].2);
    for &(_, stream, pts) in &packets[1..] {
        if stream != run_stream {
            run_stream = stream;
            run_start = pts;
            continue;
        }
        let span = pts - run_start;
        assert!(
            span <= seconds,
            "a run of stream {run_stream} packets alone spans {span:.2} s (from {run_start:.2} s) -- over the {seconds} s bound"
        );
    }
}

/// A timeline whose *first* second of sound was lifted. A reader drops the
/// first packet of an AAC track as encoder priming, so a track that opens on a
/// hole has to carry one extra packet of silence for it to drop -- without it
/// the drop comes out of the hole and every sound after the hole plays 23 ms
/// early, which is a lip sync error for the whole export.
#[test]
fn a_leading_audio_gap_keeps_the_sound_after_it_in_place() {
    let _seat = pin_software();
    let source = asset("test_av.mp4");
    let mut session = PlaybackSession::open(&source).expect("open source");
    assert!(session.cut_at(1.0), "cut at 1 s");
    assert!(session.lift_clip(Lane::A1, 0), "lift the first second");
    session.pause();
    let out = out_path("leading_gap");
    let handle = session.export_to(&out);
    wait(&handle, Duration::from_secs(900)).expect("export");

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
    let _seat = pin_software();
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
    wait(&handle, Duration::from_secs(900)).expect("export");

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

/// The stand-in a proxy really is, in one pass over the fixture: a picture-only
/// file whose *every* frame is a decoder's starting point, playable through the
/// ordinary session, and found in the cache the second time it is asked for.
///
/// Software seat, like every default test here ([`pin_software`]) -- what the
/// hardware one buys is speed, and that is the ignored test below.
#[test]
fn a_proxy_is_picture_only_every_frame_a_starting_point_and_cached() {
    let _film = PROXY_FILM.lock().unwrap_or_else(|e| e.into_inner());
    let _seat = pin_software();
    let source = asset("test_av.mp4");
    let out = engine::proxy::path_for(&source).expect("a cache directory");
    // A proxy left by an earlier run would make this test assert about a file
    // it did not write -- and the cache-hit half below would be the only half
    // that ran.
    let _ = std::fs::remove_file(&out);

    let job = engine::proxy::generate(&source).expect("start the proxy");
    assert_eq!(job.path(), out, "the job writes where the key says");
    let started = Instant::now();
    while !job.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(900),
            "the proxy did not finish: {:.0}% in",
            job.progress() * 100.0
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let made = job
        .outcome()
        .expect("a finished job has an outcome")
        .expect("the proxy");
    assert_eq!(made, out);
    assert_eq!(job.progress(), 1.0, "finished at full progress");

    let (source_meta, _) = engine::demux::Demuxer::open(&source).expect("open the fixture");
    let (meta, _) = engine::demux::Demuxer::open(&made).expect("open the proxy");
    assert_eq!(
        (meta.width, meta.height),
        engine::proxy::size_for(&source_meta),
        "the proxy is coded at the size the rule picked"
    );
    assert_eq!(meta.codec, engine::Codec::H264, "proxies are H.264");
    assert_eq!(
        meta.frame_count, source_meta.frame_count,
        "a stand-in as long as the film, or every cut lands elsewhere"
    );

    // The whole point: every frame its own random-access point, so a seek never
    // has to start earlier than it was asked to.
    let syncs = engine::demux::sync_points(&made);
    assert_eq!(
        syncs.len() as u32,
        meta.frame_count,
        "{} of {} frames are starting points -- the proxy is not intra-only",
        syncs.len(),
        meta.frame_count
    );

    // ...and no sound in it at all: the mix is the original's, always.
    assert!(
        engine::AudioSession::open(&made)
            .expect("probe the proxy for sound")
            .is_none(),
        "a proxy carries no audio track"
    );

    // It plays through the ordinary door, which is what substitution needs.
    let session = PlaybackSession::open(&made).expect("play the proxy");
    assert_eq!(session.meta().frame_count, source_meta.frame_count);

    // ...and a session on the *film* plays it instead once the switch is on,
    // while the film is still the only thing the project names -- which is what
    // an export reads and why an export can never come off a stand-in.
    let mut session = PlaybackSession::open(&source).expect("open the film");
    // The film as the project names it: canonical ([`Source::new`]), which the
    // proxy key resolves to as well -- the same file under two names is one
    // stand-in.
    let film = session.sources()[0].path.clone();
    assert_eq!(session.picture_path(0), film, "off: the film itself");
    session.set_proxies(true);
    assert!(session.proxies());
    assert_eq!(session.picture_path(0), made, "on: the stand-in");
    assert_eq!(
        session.sources()[0].path,
        film,
        "the project still names the film, and that is what an export reads"
    );
    // A frame really arrives from it: the switch reseeks, so the picture after
    // it is the proxy's.
    let started = Instant::now();
    while session.try_frame().is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no picture after switching to the stand-in"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    session.set_proxies(false);
    assert_eq!(session.picture_path(0), film, "and back again");

    // The switch survives a save and a load, and a project written before there
    // were stand-ins opens on its films.
    let project = Scratch::file("ve_proxy_switch", "edith");
    session.set_proxies(true);
    session.save_project(&project).expect("save");
    let text = std::fs::read_to_string(&project).expect("read the project back");
    assert!(text.starts_with("edith 20\n"), "{text}");
    assert!(text.contains("\nproxy on\n"), "{text}");
    let reopened = PlaybackSession::open_project(&project).expect("load");
    assert!(reopened.proxies(), "the switch came back on");
    assert_eq!(reopened.picture_path(0), made, "and it is substituting");

    // Asked again, it is the file already there: no second encode.
    let again = engine::proxy::generate(&source).expect("ask again");
    assert!(again.is_finished(), "a cached proxy is done on the spot");
    assert_eq!(
        again.outcome().expect("cached outcome").expect("cached path"),
        out
    );
    assert_eq!(engine::proxy::cached(&source), Some(out.clone()));

    std::fs::remove_file(&out).expect("clean the cache entry up");
    assert_eq!(engine::proxy::cached(&source), None);
}

/// The way out of a stand-in nobody wants to wait for -- a whole film through a
/// decoder and an encoder, started without being asked for, and until now with
/// nothing that could stop it.
///
/// What a stop has to be worth trusting, all of it here: the worker really
/// gives up (it settles, so its encoder is closed and its thread is gone), the
/// half-written `.part` goes with it, and **nothing is left under the name a
/// finished stand-in would have** -- which is the invariant that keeps a cut
/// from being made on a truncated picture, since the cache is looked up by
/// existence alone ([`engine::proxy::cached`]).
///
/// ...and the two edges: asking twice is asking once, and a stop that arrives
/// on a stand-in *already written* must not take that file away -- it is what
/// the switch is at that moment playing.
///
/// Software seat, like every default test here ([`pin_software`]).
#[test]
fn a_stopped_proxy_leaves_neither_a_stand_in_nor_half_of_one() {
    let _film = PROXY_FILM.lock().unwrap_or_else(|e| e.into_inner());
    let _seat = pin_software();
    let source = asset("test_av.mp4");
    let out = engine::proxy::path_for(&source).expect("a cache directory");
    let part = part_path(&out);
    // A stand-in left by an earlier run would make this a cache hit, and a
    // cache hit is the one job with nothing to stop.
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&part);

    let job = engine::proxy::generate(&source).expect("start the proxy");
    // Stopped with the encoder really running: a flag set before the worker has
    // opened anything tests the flag and not the stop. Opening costs under a
    // second alone, but the sibling exports of this binary can starve it for
    // minutes -- the budget is a hang guard, not a speed claim.
    let opening = Instant::now();
    while job.encoder().is_none() && !job.is_finished() {
        assert!(
            opening.elapsed() < Duration::from_secs(600),
            "the encoder never opened"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !job.is_finished(),
        "the fixture finished before it could be stopped: this test needs a film \
         the software seat takes a moment over"
    );
    // Twice, because a second click on the × is a thing that happens.
    job.cancel();
    job.cancel();
    let stopping = Instant::now();
    while !job.is_finished() {
        assert!(
            stopping.elapsed() < Duration::from_secs(120),
            "the worker kept encoding after the stop: {:.0}% in",
            job.progress() * 100.
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let refusal = job
        .outcome()
        .expect("a settled job has an outcome")
        .err()
        .expect("a stopped proxy is not a proxy");
    assert!(
        refusal.to_string().contains("cancelled"),
        "not the cancellation: {refusal}"
    );
    assert!(
        !out.exists(),
        "a stopped proxy left {} behind, and the cache is looked up by existence",
        out.display()
    );
    assert!(
        !part.exists(),
        "a stopped proxy left the half-written {} behind",
        part.display()
    );
    assert_eq!(
        engine::proxy::cached(&source),
        None,
        "the cache offers a stand-in that was never finished"
    );
    // Past the end it is still idempotent, and the outcome is handed out once.
    job.cancel();
    assert!(job.outcome().is_none(), "the outcome came out twice");
    assert!(!out.exists());

    // The other edge, at no cost: a job for a film whose stand-in is already
    // written is the cache-hit job, which is exactly what a stop racing the
    // last instant of an encode settles into -- and cancelling it must leave
    // the file alone, because that file is what the picture is coming off.
    std::fs::write(&out, b"a stand-in that is already written").expect("seed the cache");
    let hit = engine::proxy::generate(&source).expect("ask again");
    assert!(hit.is_finished(), "a cached stand-in is done on the spot");
    hit.cancel();
    assert!(
        out.is_file(),
        "a stop deleted a finished stand-in the switch was playing"
    );
    assert_eq!(engine::proxy::cached(&source), Some(out.clone()));
    assert_eq!(
        hit.outcome().expect("cached outcome").expect("cached path"),
        out,
        "the stop turned a finished stand-in into a failure"
    );

    std::fs::remove_file(&out).expect("clean the cache entry up");
}

/// The same file on the **hardware** seat, asking the one question the software
/// twin above cannot: is what the GPU wrote really every-frame-a-starting-point?
///
/// It was not, and that is why this test exists. The plugin's ordinary H.264
/// seat, asked for a key frame on every picture, codes I slices that are not
/// IDRs and carry no parameter sets: a 60-picture proxy came back with **one**
/// sync point in it, and a proxy whose seeks are no better than the film's is
/// no proxy at all. The seat that answers this is `vh_enc_open_intra`.
///
/// ```text
/// cargo build -p engine-hw --release   # its own step: `--tests` leaves it stale
/// LD_LIBRARY_PATH=target/release cargo test -p engine --release --test export \
///   -- --ignored --nocapture --test-threads=1 hardware_proxy
/// ```
#[test]
#[ignore = "needs the VA-API plugin"]
fn a_hardware_proxy_is_every_frame_a_starting_point_too() {
    let _seat = pin_hardware();
    let source = asset("test_av.mp4");
    let out = engine::proxy::path_for(&source).expect("a cache directory");
    let _ = std::fs::remove_file(&out);
    let job = engine::proxy::generate(&source).expect("start the proxy");
    let started = Instant::now();
    while !job.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(900),
            "the proxy did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let made = job
        .outcome()
        .expect("a finished job has an outcome")
        .expect("the proxy");
    let seat = job.encoder().unwrap_or_default();
    println!("hardware proxy seat: {seat}");
    // The silent-software trap: without the plugin on the path this measures
    // the fallback and calls it hardware.
    assert!(
        seat.contains("HW encode"),
        "not the hardware seat ({seat}): LD_LIBRARY_PATH must name the plugin"
    );
    let (meta, _) = engine::demux::Demuxer::open(&made).expect("open the proxy");
    let syncs = engine::demux::sync_points(&made);
    println!("{} of {} frames are starting points", syncs.len(), meta.frame_count);
    assert_eq!(
        syncs.len() as u32,
        meta.frame_count,
        "the hardware seat wrote {} starting points in {} frames",
        syncs.len(),
        meta.frame_count
    );
    // Counting starting points is not reading the file: a proxy whose every
    // access unit is garbage counts perfectly. It shipped that way -- the SPS
    // carried a wrapped `log2_max_frame_num_minus4` of 252 and every decoder
    // refused the stream, while this test stayed green. So decode it, all of
    // it, and let an outside decoder say the same.
    let frames = decode_all(&made);
    println!("decoded back {} of {} pictures", frames.len(), meta.frame_count);
    assert_eq!(
        frames.len() as u32,
        meta.frame_count,
        "the proxy demuxes but does not decode"
    );
    assert!(
        frames.iter().all(|f| f.iter().any(|&b| b != 0)),
        "a decoded picture came back empty"
    );
    match ffmpeg_complaints(&made) {
        Some(said) => assert!(said.is_empty(), "ffmpeg read the proxy and said:\n{said}"),
        None => eprintln!("no ffmpeg: skipping the outside decoder's word on the proxy"),
    }
    std::fs::remove_file(&out).expect("clean the cache entry up");
}

/// His own 4K HEVC film, on the hardware seat: a proxy of it is made **faster
/// than the film plays**, which is the whole promise -- and a cancel stops it
/// dead and leaves nothing behind.
///
/// Measured over a window rather than to the end: the film is two hours and the
/// number wanted is a rate, not a wall time.
///
/// ```text
/// cargo build -p engine-hw --release   # its own step: `--tests` leaves it stale
/// LD_LIBRARY_PATH=target/release cargo test -p engine --release --test export \
///   -- --ignored --nocapture --test-threads=1 proxy_of_his_4k
/// ```
#[test]
#[ignore = "needs the VA-API plugin and his own library"]
fn a_proxy_of_his_4k_film_is_made_faster_than_it_plays() {
    let _seat = pin_hardware();
    let Some(film) = engine::real_library::film("hevc_4k_hdr") else {
        return;
    };
    let out = engine::proxy::path_for(&film).expect("a cache directory");
    let _ = std::fs::remove_file(&out);
    let (meta, _) = engine::demux::Demuxer::open(&film).expect("open the film");
    let duration = f64::from(meta.frame_count) / meta.frame_rate;
    println!(
        "film: {}x{} {:.3} fps, {duration:.0}s",
        meta.width, meta.height, meta.frame_rate
    );

    let job = engine::proxy::generate(&film).expect("start the proxy");
    let window = Duration::from_secs(60);
    let started = Instant::now();
    while started.elapsed() < window && !job.is_finished() {
        std::thread::sleep(Duration::from_millis(200));
    }
    let elapsed = started.elapsed().as_secs_f64();
    let made = f64::from(job.progress()) * duration;
    let seat = job.encoder().unwrap_or_default();
    println!(
        "proxy: {made:.0}s of film in {elapsed:.0}s = {:.2}x realtime, seat {seat}",
        made / elapsed
    );
    // The silent-software trap: a run without the plugin on the path measures
    // the fallback and calls it hardware.
    assert!(
        seat.contains("HW encode"),
        "not the hardware seat ({seat}): LD_LIBRARY_PATH must name the plugin"
    );
    // Twice realtime, not once: the rubric's floor, and the number this seat
    // actually measured (2.35x warm, 3.54x with the page cache hot). A `>= 1.0`
    // here would let a run at 1.2x -- half the speed this shipped at -- pass as
    // green. Cold, the disk is the ceiling instead (~1x on a first pass over an
    // uncached remux), so this wants a film the cache has already seen, which
    // is what a second run of it is.
    assert!(
        made / elapsed >= 2.0,
        "{:.2}x realtime is under the 2x floor this shipped at",
        made / elapsed
    );

    // Cancel stops the reading, and a proxy that was not finished is not left
    // lying around under the name a finished one would have.
    job.cancel();
    let stopped = Instant::now();
    while !job.is_finished() {
        assert!(
            stopped.elapsed() < Duration::from_secs(30),
            "the proxy did not stop when cancelled"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(job.outcome().expect("an outcome").is_err(), "cancelled");
    assert!(
        !out.exists(),
        "a cancelled proxy left {} behind",
        out.display()
    );
    assert!(!part_path(&out).exists(), "a cancelled proxy left its .part");
    assert_eq!(engine::proxy::cached(&film), None);
}

// ---------------------------------------------------------------------------
// The hardware encoder's process dying under the driver.
//
// Mesa's radeonsi calls libc `abort()` from its own C worker thread when the
// video ring resets under it, which no `catch_unwind` in the process can see:
// on 2026-08-13 that killed the editor mid-export and took the session with it.
// The encoder now lives in a child (`engine::hwproc`), and these are the proof
// that its death is an inconvenience rather than a crash.
//
// None of them needs a GPU, and that is deliberate: the machine where this
// matters is the one whose ring is already wedged, and a test that had to hang a
// real ring to run could never be the oracle for the fix. `VE_HW_TEST_FAKE`
// opens a stand-in seat in the child that touches no driver;
// `VE_HW_TEST_ABORT` / `VE_HW_TEST_HANG` say where it dies or stops.

/// The one place in this binary that *changes* the seat rather than reading it,
/// so it takes [`SEAT`] exclusively: while it holds, no other export in this
/// process is in flight, and every variable it set is gone again before it lets
/// go. Which is the whole of the bug this shape exists to make impossible --
/// a stand-in encoder seat cycled underneath a software test that had already
/// opened its own is a hang, not a failure, and it does not reproduce.
///
/// It also lifts the software *encode* pin for its own exports, because the
/// question here is what an export does when its hardware seat dies. Decode
/// stays pinned to software: it is not what is under test and the fixture must
/// decode the same way it does everywhere else in this file.
fn injected<T>(abort: Option<&str>, hang: Option<&str>, body: impl FnOnce() -> T) -> T {
    // Short, because one of these is waiting out a child that has stopped
    // answering and the wait is the thing under test.
    injected_with(abort, hang, Some("1500"), body)
}

/// The same, with the wait left to the caller: `None` is what a person's own
/// editor runs with, which is the one setting none of the tests above ever
/// exercised.
fn injected_with<T>(
    abort: Option<&str>,
    hang: Option<&str>,
    timeout_ms: Option<&str>,
    body: impl FnOnce() -> T,
) -> T {
    let _exclusive = SEAT.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    let restore = std::env::var_os("VE_SW_ENC");
    unsafe {
        std::env::set_var("VE_SW", "1");
        std::env::remove_var("VE_SW_ENC");
        std::env::set_var("VE_HW_TEST_FAKE", "1");
        std::env::set_var("VE_HW_CHILD_BIN", env!("CARGO_BIN_EXE_hw-encode-child"));
        match timeout_ms {
            Some(ms) => std::env::set_var("VE_HW_TIMEOUT_MS", ms),
            None => std::env::remove_var("VE_HW_TIMEOUT_MS"),
        }
        match abort {
            Some(at) => std::env::set_var("VE_HW_TEST_ABORT", at),
            None => std::env::remove_var("VE_HW_TEST_ABORT"),
        }
        match hang {
            Some(at) => std::env::set_var("VE_HW_TEST_HANG", at),
            None => std::env::remove_var("VE_HW_TEST_HANG"),
        }
    }
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    unsafe {
        for var in [
            "VE_HW_TEST_FAKE",
            "VE_HW_CHILD_BIN",
            "VE_HW_TIMEOUT_MS",
            "VE_HW_TEST_ABORT",
            "VE_HW_TEST_HANG",
        ] {
            std::env::remove_var(var);
        }
        // Put the software suite's pin back exactly as it was found -- it is
        // set once for the whole binary and this is the only thing that ever
        // takes it away.
        if let Some(pinned) = restore {
            std::env::set_var("VE_SW_ENC", pinned);
        }
    }
    match out {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// The timeline these all export: short, and cut, so the export really runs a
/// span loop rather than copying one clip.
fn short_edit(source: &Path, frames: u32) -> (Project, engine::VideoMeta) {
    let (meta, _) = engine::demux::Demuxer::open(source).expect("open source");
    let mut project = Project::single(source, meta.frame_count);
    assert!(project.split(frames));
    assert!(project.delete(1));
    assert_eq!(project.timeline_frames(), frames);
    (project, meta)
}

/// A whole export whose hardware encoder is killed three frames in: the editor
/// is still here, and so is the file -- written again, from the first frame, by
/// the software encoder for the same codec, and clean enough that an outside
/// decoder reads every packet of it without a word.
///
/// Invariant 1 at its entry surface: `export::start` is what the export card
/// presses, and this is that same call with a driver dying under it.
#[test]
fn a_hardware_encoder_killed_mid_export_costs_the_file_nothing() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 20);
    let out = out_path("hw_child_abort");

    injected(Some("3"), None, || {
        // `Auto` is what an untouched export card sends: the seat that *may*
        // fall back, which is the whole question here.
        let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
        wait(&handle, Duration::from_secs(180)).expect("the export survived its encoder");
    });

    let (written, _) = engine::demux::Demuxer::open(&out).expect("the export is a file");
    assert_eq!(written.frame_count, 20, "every timeline frame was written");
    assert_eq!(decode_all(&out).len(), 20, "and every one of them decodes");
    if let Some(said) = ffmpeg_complaints(&out) {
        assert!(
            said.is_empty(),
            "ffmpeg read the fallback's file badly: {said}"
        );
    }
    assert!(!part_path(&out).exists(), "the .part outlived the export");
    std::fs::remove_file(&out).unwrap();
}

/// ...and the bar over that same death only ever goes forwards. The rerun is a
/// whole second pass over the timeline, so the one thing a person must not see
/// is the progress they watched fill drop back to zero: it holds at the mark the
/// dead seat reached and moves on from there, ending exactly full.
#[test]
fn a_hardware_encoder_killed_mid_export_never_pulls_the_bar_backwards() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 20);
    let out = out_path("hw_child_abort_progress");

    let seen = injected(Some("3"), None, || {
        let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
        // Sampled far faster than the export writes, so the retry cannot slip
        // through between two reads.
        let started = Instant::now();
        let mut seen = vec![handle.progress()];
        while !handle.is_finished() {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "export did not finish"
            );
            let now = handle.progress();
            if now != *seen.last().unwrap() {
                seen.push(now);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        handle
            .result()
            .expect("a finished export has an outcome")
            .expect("the export survived its encoder");
        seen.push(handle.progress());
        seen
    });

    assert!(
        seen.windows(2).all(|pair| pair[1] >= pair[0]),
        "the bar went backwards across the fallback: {seen:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some(1.0),
        "a finished export leaves a full bar: {seen:?}"
    );
    std::fs::remove_file(&out).unwrap();
}

/// Threads of this very process that belong to an audio pass. Every worker the
/// pass starts inherits its name from the thread that spawned it (Linux `comm`
/// is inherited), so this is the pass *and its own workers* -- which is what
/// makes it a measure of how many passes are running: two at once is twice the
/// crowd of one.
fn audio_pass_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|thread| {
            std::fs::read_to_string(thread.path().join("comm"))
                .is_ok_and(|name| name.trim() == "export-audio")
        })
        .count()
}

/// The busiest that crowd ever gets while `export` writes this project.
fn busiest_audio_pass(project: Project, meta: engine::VideoMeta, out: &Path) -> usize {
    let handle = engine::export::start(project, meta, out, &ExportSettings::default(), None);
    let started = Instant::now();
    let mut most = 0;
    while !handle.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "export did not finish"
        );
        most = most.max(audio_pass_threads());
        std::thread::sleep(Duration::from_millis(1));
    }
    handle
        .result()
        .expect("a finished export has an outcome")
        .expect("the export was written");
    most
}

/// One audio pass at a time, across that same death. An mp4 runs the sound
/// beside the picture, and the picture's failure used to leave that thread
/// *detached*: the re-run started a second pass, and a machine that had just
/// lost its GPU mixed and encoded the same film's sound twice at once. Measured
/// against the same export with nothing dying under it -- the fallback may not
/// be a busier place than an ordinary export is.
#[test]
fn a_hardware_encoder_killed_mid_export_leaves_no_audio_pass_behind() {
    // AC-3: an mp4 cannot carry it, so the pass is a real decode and encode
    // rather than the packet copy an AAC source is over in a millisecond.
    let source = asset("test_ac3.mp4");
    let out = out_path("hw_child_abort_audio");

    let (project, meta) = short_edit(&source, 40);
    let alone = {
        let _seat = pin_software();
        busiest_audio_pass(project, meta, &out)
    };
    std::fs::remove_file(&out).unwrap();

    let (project, meta) = short_edit(&source, 40);
    let across = injected(Some("3"), None, || busiest_audio_pass(project, meta, &out));

    println!("audio pass threads: {alone} alone, {across} across the fallback");
    assert!(
        across <= alone,
        "the fallback ran {across} audio-pass threads where one pass runs {alone}"
    );
    let (written, _) = engine::demux::Demuxer::open(&out).expect("the export is a file");
    assert_eq!(written.frame_count, 40, "every timeline frame was written");
    std::fs::remove_file(&out).unwrap();
}

/// What a *shipping* editor waits when its child stops answering. Every test
/// here pins `VE_HW_TIMEOUT_MS` at 1.5 s, so none of them ever ran the 15 s
/// default -- fifteen seconds an export spends on a child that will never
/// answer again, before the software encoder it is owed can even begin.
///
/// The ceiling stays where it is, because an encoder opening on a 4K frame is
/// not a dead one. What changes is that a child which has answered its first
/// frames has *said* what its frames cost, and from then on the wait is that
/// measure and not the ceiling.
#[test]
fn a_wedged_child_is_waited_out_by_its_own_measure_not_the_ceiling() {
    let (width, height) = (320u32, 240u32);
    // No `VE_HW_TIMEOUT_MS` at all: the wait is the one a person's export gets.
    injected_with(None, Some("10"), None, || {
        let mut encoder =
            HwEncoder::open(width, height, 30, 1, 1_000_000).expect("the stand-in seat opened");
        let (y, u, v) = synthetic(0, width as usize, height as usize);
        for frame in 0..10 {
            assert!(
                encoder.encode(&y, &u, &v, width, height, false).is_ok(),
                "picture {frame} is before the wedge"
            );
        }
        let started = Instant::now();
        let wedged = encoder
            .encode(&y, &u, &v, width, height, false)
            .expect_err("a child that stops answering is not an encode");
        let waited = started.elapsed();
        println!("wedge at the shipping default: contained in {waited:.2?}");
        assert!(engine::hwproc::is_lost(&wedged), "{wedged}");
        assert!(
            waited < Duration::from_secs(6),
            "waited {waited:.2?} on a wedged child at the shipping default"
        );
        // ...and never so little that a slow picture could be mistaken for a
        // dead one: the floor holds even where every measured frame was
        // instant, which the stand-in encoder's are.
        assert!(waited >= Duration::from_secs(2), "waited only {waited:.2?}");
    });
}

/// The same death at the *open*: the child never gets far enough to say yes, so
/// there is no hardware seat at all and the software one takes the export from
/// the first frame. A refusal, not a crash, and the file is whole.
#[test]
fn a_hardware_encoder_killed_at_its_open_is_simply_no_seat() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 12);
    let out = out_path("hw_child_abort_init");

    injected(Some("init"), None, || {
        let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
        wait(&handle, Duration::from_secs(180)).expect("the export survived the open");
    });

    let (written, _) = engine::demux::Demuxer::open(&out).expect("the export is a file");
    assert_eq!(written.frame_count, 12);
    std::fs::remove_file(&out).unwrap();
}

/// ...and at the finalize, which is the third place a ring resets: every
/// picture is fed and the encoder is being flushed when it dies.
#[test]
fn a_hardware_encoder_killed_at_the_drain_costs_the_file_nothing() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 12);
    let out = out_path("hw_child_abort_drain");

    injected(Some("drain"), None, || {
        let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
        wait(&handle, Duration::from_secs(180)).expect("the export survived the drain");
    });

    let (written, _) = engine::demux::Demuxer::open(&out).expect("the export is a file");
    assert_eq!(written.frame_count, 12);
    assert_eq!(decode_all(&out).len(), 12);
    std::fs::remove_file(&out).unwrap();
}

/// Invariant 4: a ring can wedge *without* anything dying -- the child stays
/// alive and simply stops answering. The parent gives it `VE_HW_TIMEOUT_MS` and
/// then kills it, which lands on the same fallback a death does. "Contained"
/// here means bounded, so the wait is what is really being measured: an export
/// that sat on a stuck child forever would be the hang this refuses.
#[test]
fn a_hardware_encoder_that_stops_answering_is_killed_and_replaced() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 12);
    let out = out_path("hw_child_hang");

    let started = Instant::now();
    injected(None, Some("2"), || {
        let handle = engine::export::start(project, meta, &out, &ExportSettings::default(), None);
        wait(&handle, Duration::from_secs(180)).expect("the export outlived the wedge");
    });
    println!(
        "hung child: contained in {:.2} s",
        started.elapsed().as_secs_f64()
    );

    let (written, _) = engine::demux::Demuxer::open(&out).expect("the export is a file");
    assert_eq!(written.frame_count, 12);
    assert_eq!(decode_all(&out).len(), 12);
    std::fs::remove_file(&out).unwrap();
}

/// ...and the export that does **not** get written again: a person who picked
/// the GPU on the export card asked for it by name, so a dead hardware seat is
/// refused in words rather than answered with an hour in an encoder nobody
/// chose. The same posture [`engine::export::start`] already takes for a machine
/// with no hardware seat at all -- and the guard that separates the two is the
/// one every test above depends on being narrow.
///
/// The process is alive either way, which is the invariant; what differs is who
/// is told.
#[test]
fn an_export_that_asked_for_the_gpu_by_name_refuses_instead_of_falling_back() {
    let source = asset("test_baseline.mp4");
    let (project, meta) = short_edit(&source, 12);
    let out = out_path("hw_child_named_seat");

    let error = injected(Some("2"), None, || {
        let settings = ExportSettings {
            seat: engine::export::EncoderSeat::Hardware,
            ..Default::default()
        };
        let handle = engine::export::start(project, meta, &out, &settings, None);
        wait(&handle, Duration::from_secs(180))
            .expect_err("a named hardware seat that died is not written again in software")
    });

    println!("named hardware seat: {error}");
    assert!(
        engine::hwproc::is_lost(&error),
        "the refusal has to name the hardware failure: {error}"
    );
    assert!(
        !out.exists(),
        "a refused export still wrote {}",
        out.display()
    );
    assert!(!part_path(&out).exists(), "the .part outlived the refusal");
}

/// The seat's own contract, under the same injection and with no export around
/// it: a killed child comes back as an error carrying
/// [`engine::hwproc::HW_LOST`], every later call gives the same answer instead
/// of blocking on a socket nobody holds, and the process doing the asking is
/// still running to be asked.
#[test]
fn a_dead_encoder_answers_every_later_call_the_same_way() {
    let (width, height) = (320u32, 240u32);
    injected(Some("1"), None, || {
        let mut encoder =
            HwEncoder::open(width, height, 30, 1, 1_000_000).expect("the stand-in seat opened");
        let (y, u, v) = synthetic(0, width as usize, height as usize);
        assert!(
            encoder.encode(&y, &u, &v, width, height, false).is_ok(),
            "the first picture is before the injection point"
        );
        let died = encoder
            .encode(&y, &u, &v, width, height, false)
            .expect_err("the second picture kills the child");
        assert!(
            engine::hwproc::is_lost(&died),
            "an encoder death must say so: {died}"
        );
        // The second ask is the one that would hang if the child were not
        // reaped, and it must not spend the timeout answering either.
        let again = Instant::now();
        let repeat = encoder.drain().expect_err("the seat stays dead");
        assert!(engine::hwproc::is_lost(&repeat), "{repeat}");
        assert!(
            again.elapsed() < Duration::from_millis(500),
            "a dead seat answered only after waiting"
        );
    });
}
