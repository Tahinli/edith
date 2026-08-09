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

use engine::hw::HwEncoder;
use engine::mux::{Mp4Muxer, VideoParams, parameter_sets};
use engine::{DecodeSession, ExportHandle, PlaybackSession};

const FPS: f64 = 30.0;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn out_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ve_export_{name}.mp4"));
    let _ = std::fs::remove_file(&path);
    path
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
