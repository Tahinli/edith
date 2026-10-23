//! Frame-accurate seeking on `test_av1_grain.mkv` (AV1 in Matroska, film grain,
//! keyframes every 30 frames). The source carries its own timecode burned into
//! the top-left corner, so a composed frame names the source frame it shows by
//! that text: `compose_at(f)` must show source frame `f`.
//!
//! Was failing: a hardware AV1 open at a mid-stream frame of this file showed a
//! later source frame (`compose_at(30)` showed source 60). The cause was in the
//! VA-API plugin, not the seek: AV1 is decoded one OBU per call, and the plugin
//! dropped the rest of each temporal unit on the Annex-B start-code rule, so no
//! keyframe was ever decoded and every seek drifted by a GOP. Fixed in
//! `crates/engine-hw`; the sequential and software paths are unchanged.
//!
//! ```text
//! cargo test -p engine --release --test grain_seek -- --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

use engine::PlaybackSession;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

const W: u32 = 1280;
const H: u32 = 720;
/// The timecode box, top-left, in full-size pixels.
const CW: u32 = 200;
const CH: u32 = 28;
const CROP: usize = (CW * CH * 4) as usize;

/// The top-left timecode crop of a full-size BGRA frame, row by row.
fn crop(full: &[u8], width: u32) -> Vec<f64> {
    let mut out = Vec::with_capacity(CROP);
    for y in 0..CH {
        let row = (y * width * 4) as usize;
        out.extend(full[row..row + (CW * 4) as usize].iter().map(|b| f64::from(*b)));
    }
    out
}

/// `1 - r` of two crops, `r` their Pearson correlation. Lower is a better match.
fn distance(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    1.0 - sab / (saa * sbb).sqrt().max(1e-9)
}

/// The source frame each timeline frame shows, by its timecode crop against an
/// ffmpeg decode of the same file.
fn shown_source_frames(session: &PlaybackSession, file: &PathBuf, timeline: &[u32]) -> Vec<(u32, usize, f64)> {
    let refs_path = std::env::temp_dir().join(format!("grain_ref_{}.bgra", std::process::id()));
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(file)
        .args(["-vf", &format!("crop={CW}:{CH}:0:0"), "-pix_fmt", "bgra", "-f", "rawvideo"])
        .arg(&refs_path)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success(), "ffmpeg reference decode failed");
    let raw = std::fs::read(&refs_path).expect("read the reference");
    let _ = std::fs::remove_file(&refs_path);
    let refs: Vec<Vec<f64>> = raw
        .as_chunks::<CROP>().0.iter()
        .map(|c| c.iter().map(|b| f64::from(*b)).collect())
        .collect();

    timeline
        .iter()
        .map(|&f| {
            let full = session.compose_at(f, W, H).expect("compose");
            let got = crop(&full.bgra, full.width);
            let (best, dist) = refs
                .iter()
                .enumerate()
                .map(|(r, c)| (r, distance(&got, c)))
                .fold((0, f64::MAX), |acc, x| if x.1 < acc.1 { x } else { acc });
            (f, best, dist)
        })
        .collect()
}

#[test]
fn composed_frames_show_the_source_frame_they_name() {
    engine::ao::set_silent_output();
    let file = asset("test_av1_grain.mkv");
    let session = PlaybackSession::open(&file).expect("open the grain fixture");
    let len = (session.timeline_duration() * 30.0).round() as u32;
    let frames: Vec<u32> = [0u32, 1, 15, 29, 30, 31, 45, 59, 60, 61, 75]
        .into_iter()
        .filter(|f| *f < len)
        .collect();
    let mut report = String::new();
    let mut misses = Vec::new();
    for (f, best, dist) in shown_source_frames(&session, &file, &frames) {
        report.push_str(&format!("frame {f:>3}: shows source {best:>3} (1-r {dist:.4})\n"));
        if best != f as usize {
            misses.push(f);
        }
    }
    println!("{report}");
    assert!(misses.is_empty(), "composed frames show the wrong source frame:\n{report}");
}
