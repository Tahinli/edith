//! The software export seat for AV1: the still-frame path (`compose_at`, which
//! the export's decoder feeds) and a short software export, for sources the
//! export used to refuse outright -- `ec-av1` is the decoder, the same one
//! playback runs in software.
//!
//! ```text
//! cargo test -p engine --release --test av1_software_export -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::Clip;
use engine::PlaybackSession;
use engine::decode::Frame;
use engine::demux::{Codec, Demuxer};
use engine::export::{EncoderSeat, ExportSettings};
use engine::project::{Lane, Speed};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// Opens a session silently, the way the server opens one: no seat is pinned,
/// so a machine with the plugin takes the hardware seat first and falls back
/// to `ec-av1` exactly as a server session does.
fn open(name: &str) -> PlaybackSession {
    engine::ao::set_silent_output();
    PlaybackSession::open(asset(name)).unwrap_or_else(|e| panic!("open {name}: {e}"))
}

/// Mean and standard deviation of BT.601 luma over a BGRA frame.
fn luma_stats(bgra: &[u8]) -> (f64, f64) {
    let ys: Vec<f64> = bgra
        .as_chunks::<4>().0.iter()
        .map(|p| 0.114 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.299 * f64::from(p[2]))
        .collect();
    let n = ys.len().max(1) as f64;
    let mean = ys.iter().sum::<f64>() / n;
    let var = ys.iter().map(|y| (y - mean) * (y - mean)).sum::<f64>() / n;
    (mean, var.sqrt())
}

/// The first `count` pictures of an AV1 file as `ec-av1` decodes them, luma
/// only, 8-bit (a 10-bit file's samples shifted down by two, as the seats do).
/// Decoded straight from the demuxer's access units -- no export code between
/// the reference and the picture under test.
fn reference_luma(name: &str, count: usize) -> Vec<(usize, usize, Vec<u8>)> {
    let (meta, mut demuxer) = Demuxer::open(&asset(name)).expect("demux");
    assert_eq!(meta.codec, Codec::Av1, "{name} is AV1");
    let shift = demuxer.bit_depth() - 8;
    let mut stream = Vec::new();
    while let Some(au) = demuxer.next_access_unit().expect("read") {
        stream.extend_from_slice(&au);
    }
    let mut out = Vec::new();
    let _ = ec_av1::stream::decode_stream_with(&stream, |picture, _, shown| {
        if shown {
            let y = picture.y.iter().map(|&s| (s >> shift) as u8).collect();
            out.push((picture.width, picture.height, y));
        }
        if out.len() >= count {
            return Err(ec_core::Error::Eof);
        }
        Ok(())
    });
    assert!(
        out.len() >= count,
        "{name}: {} pictures, wanted {count}",
        out.len()
    );
    out.truncate(count);
    out
}

/// Pearson correlation of two equal-length series.
fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        cov += (x - ma) * (y - mb);
        va += (x - ma) * (x - ma);
        vb += (y - mb) * (y - mb);
    }
    cov / (va.sqrt() * vb.sqrt()).max(f64::MIN_POSITIVE)
}

/// The composed picture at `frame` must sit on the reference picture: same
/// luma within what the BGRA round trip costs, and not a flat black or grey
/// plane. Returns the composed frame for the caller's own checks.
fn assert_composes_like_reference(
    session: &PlaybackSession,
    name: &str,
    frame: u32,
    source_frame: u32,
) -> Frame {
    let reference = reference_luma(name, source_frame as usize + 1);
    let (rw, rh, ref_y) = &reference[source_frame as usize];
    let f = session
        .compose_at(frame, *rw as u32, *rh as u32)
        .unwrap_or_else(|e| panic!("compose {name} frame {frame}: {e}"));
    assert_eq!((f.width as usize, f.height as usize), (*rw, *rh), "{name}");
    let (mean, sd) = luma_stats(&f.bgra);
    let ref_mean = ref_y.iter().map(|&y| f64::from(y)).sum::<f64>() / ref_y.len() as f64;
    eprintln!(
        "{name} frame {frame}: composed luma mean {mean:.1} sd {sd:.1}; reference mean {ref_mean:.1}"
    );
    assert!(
        mean > 16.0 && sd > 1.0,
        "{name}: black or flat ({mean:.1}, {sd:.1})"
    );
    // Pixel by pixel: the composed luma of each pixel against the reference's.
    // The BGRA round trip re-maps range and matrix, so the test asks for the
    // same picture (correlation), not the same code values.
    let ours: Vec<f64> = f
        .bgra
        .as_chunks::<4>().0.iter()
        .map(|p| 0.114 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.299 * f64::from(p[2]))
        .collect();
    let theirs: Vec<f64> = ref_y.iter().map(|&y| f64::from(y)).collect();
    let corr = correlation(&ours, &theirs);
    eprintln!("{name} frame {frame}: luma correlation with the reference {corr:.4}");
    assert!(
        corr > 0.95,
        "{name}: composed picture is not the reference picture (r {corr:.4})"
    );
    f
}

#[test]
fn compose_at_takes_an_8_bit_av1_source() {
    let session = open("test_av1.mkv");
    let a = assert_composes_like_reference(&session, "test_av1.mkv", 0, 0);
    // A later frame, past the first GOP's keyframe and into its inter frames.
    assert_composes_like_reference(&session, "test_av1.mkv", 15, 15);
    // Deterministic: the same frame again is the same picture.
    let b = session
        .compose_at(0, a.width, a.height)
        .expect("frame 0 again");
    assert!(a.bgra == b.bgra, "two composes of frame 0 differ");
}

#[test]
fn compose_at_takes_a_ten_bit_av1_source() {
    let session = open("test_av1_10.mkv");
    assert_composes_like_reference(&session, "test_av1_10.mkv", 0, 0);
    assert_composes_like_reference(&session, "test_av1_10.mkv", 10, 10);
}

#[test]
fn compose_at_takes_an_mp4_av1_source_with_vui() {
    let session = open("test_vui_av1.mp4");
    assert_composes_like_reference(&session, "test_vui_av1.mp4", 0, 0);
    assert_composes_like_reference(&session, "test_vui_av1.mp4", 15, 15);
}

/// The regression: an AV1 clip placed mid-timeline, with a non-zero in-point,
/// composed at a frame inside it -- the path the server's `frame {at}` takes.
/// Before the hardware-fallback fix this reported "ends before frame". The
/// clip is source 15..45 placed at timeline frame 60, so timeline 60 is
/// source 15 and timeline 75 is source 30.
#[test]
fn a_mid_clip_av1_frame_composes_with_a_start_and_an_in_point() {
    let mut session = open("test_av1.mkv");
    assert!(session.delete_clip(Lane::V1, 0), "drop the whole-file clip");
    let clip = Clip {
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        start: 60,
        in_frame: 15,
        out_frame: 45,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
        transform: None,
    };
    assert!(session.place_at(Lane::V1, 2.0, clip), "place it at 2 s");
    assert_composes_like_reference(&session, "test_av1.mkv", 60, 15);
    assert_composes_like_reference(&session, "test_av1.mkv", 75, 30);
    // The last frame of the clip: timeline 89, source 44.
    session
        .compose_at(89, 1280, 720)
        .expect("the clip's last frame composes");
}

/// The source-frame path the server's `frame {source}` takes, through the same
/// decoder the playback worker opens: a frame past the first GOP, of each
/// source the robustness pass named.
#[test]
fn a_source_frame_decodes_at_a_mid_stream_index() {
    for name in ["test_av1.mkv", "test_av1_10.mkv", "test_vui_av1.mp4"] {
        let path = asset(name);
        let (_, rx, cancel) = engine::DecodeSession::open_range(&path, 15, 16)
            .unwrap_or_else(|e| panic!("open {name}: {e}"));
        let frame = rx
            .recv()
            .unwrap_or_else(|_| panic!("{name}: source frame 15 is past the end"));
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            frame.bgra.iter().any(|&b| b != 0),
            "{name}: frame 15 is black"
        );
    }
}

/// A three-frame, 64x64 source: the stream is shorter than a GOP and the
/// hardware seat gives it nothing. Compose and a full export must still work.
#[test]
fn a_tiny_av1_source_composes_and_exports() {
    let session = open("test_av1_420.mkv");
    assert_composes_like_reference(&session, "test_av1_420.mkv", 0, 0);
    let session = open("test_av1_420.mkv");
    let out = Scratch::file("ve_av1_tiny_export", "mp4");
    let settings = ExportSettings {
        seat: EncoderSeat::Software,
        ..Default::default()
    };
    let handle = session.export_to_with(&out, &settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(120), "export hung");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("outcome").expect("the tiny export");
    let (meta, _) = Demuxer::open(&out).expect("reopen the tiny export");
    assert_eq!(meta.frame_count, 3, "every frame of the tiny source");
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn a_short_software_export_of_an_av1_source_succeeds() {
    for name in ["test_av1.mkv", "test_av1_10.mkv"] {
        let mut session = open(name);
        assert!(session.cut_at(3.0 / 30.0), "three frames of it");
        assert!(
            session.delete_clip(Lane::V1, 1),
            "drop everything after them"
        );
        let out = Scratch::file(&format!("ve_av1_sw_export_{name}"), "mp4");
        let settings = ExportSettings {
            seat: EncoderSeat::Software,
            ..Default::default()
        };
        let handle = session.export_to_with(&out, &settings);
        let started = Instant::now();
        while !handle.is_finished() {
            assert!(
                started.elapsed() < Duration::from_secs(600),
                "{name}: export hung"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        handle
            .result()
            .expect("outcome")
            .unwrap_or_else(|e| panic!("{name}: software export refused: {e}"));
        let (meta, _) = Demuxer::open(&out).expect("reopen the export");
        assert_eq!(meta.frame_count, 3, "{name}: every timeline frame is there");
        std::fs::remove_file(&out).unwrap();
    }
}

/// AV1 4:2:2, 4:4:4 and 12-bit open in both containers, at the depth they state.
#[test]
fn av1_422_444_and_12_bit_open_in_both_containers() {
    for (name, depth) in [("test_av1_422", 8), ("test_av1_444", 8), ("test_av1_12bit", 12)] {
        for ext in ["mkv", "mp4"] {
            let file = asset(&format!("{name}.{ext}"));
            let (_, demuxer) = Demuxer::open(&file).unwrap_or_else(|e| panic!("{name}.{ext}: {e}"));
            assert!(demuxer.bit_depth() >= depth, "{name}.{ext}: {}", demuxer.bit_depth());
        }
    }
}

/// A 4:2:2 AV1 source (libaom, generated here) and the 12-bit fixture export end
/// to end on the software seat: every frame comes out, in a file that reopens.
#[test]
fn a_422_and_a_12_bit_av1_source_export_end_to_end() {
    let clip = Scratch::file("ve_av1_422_src", "mkv");
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=128x96:rate=30:duration=1"])
        .args(["-frames:v", "6", "-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "20", "-b:v", "0"])
        .args(["-pix_fmt", "yuv422p"])
        .arg(&*clip)
        .output()
        .expect("ffmpeg");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    for (label, path) in [("422", PathBuf::from(&*clip)), ("12bit", asset("test_av1_12bit.mkv"))] {
        engine::ao::set_silent_output();
        let session = PlaybackSession::open(&path).unwrap_or_else(|e| panic!("{label}: {e}"));
        let (source, _) = Demuxer::open(&path).unwrap();
        let out = Scratch::file(&format!("ve_av1_{label}_export"), "mp4");
        let settings = ExportSettings {
            seat: EncoderSeat::Software,
            ..Default::default()
        };
        let handle = session.export_to_with(&out, &settings);
        let started = Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed() < Duration::from_secs(600), "{label}: export hung");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.result().expect("outcome").unwrap_or_else(|e| panic!("{label}: {e}"));
        let (meta, _) = Demuxer::open(&out).expect("reopen the export");
        assert_eq!(meta.frame_count, source.frame_count, "{label}: every frame");
    }
}
