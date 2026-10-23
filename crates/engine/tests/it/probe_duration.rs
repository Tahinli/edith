//! The duration `probe_media` reports for every audio and video file in `assets/`,
//! checked against ffprobe, and the language a probe states for an mp4/mov track.
//!
//! The oracle is `ffprobe`'s `format=duration`, with rules that follow what
//! ffprobe itself measures, or what a player plays:
//!
//! - An Ogg Opus stream's playable length is RFC 7845 section 4's: the last
//!   granule position minus the `OpusHead` pre-skip, over 48 kHz. ffprobe does
//!   not take the pre-skip off, so the oracle subtracts it from ffprobe's figure
//!   (`test_tone.opus`: 3.0065 - 312/48000 = 3.000 s, the tone's length).
//! - A raw ADTS stream (`format_name=aac`) states no length. ffprobe's format
//!   duration there is file size over an estimated bit rate, which is a guess
//!   (3.126 s on `test_tone.aac`). Its own packet timeline is the measured length:
//!   the last packet's start plus its duration, which is 131 frames of 1024 samples
//!   (3.0418 s). The test compares against that for ADTS.
//! - A Matroska or WebM file with a picture is reported by the engine as its
//!   picture length, the frame count over the frame rate (the timeline's contract,
//!   see `MediaFacts::duration_secs`). ffprobe reports the Segment `Info` duration,
//!   which runs past the last frame by part of a frame. The tolerance for those
//!   files is one frame on top of the 2 ms.
//!
//! A file the engine refuses (no duration) is skipped, and so is anything ffprobe
//! finds no audio or video in (subtitles alone, a still). When ffprobe is not on
//! the path the whole test is skipped.
//!
//! ```text
//! cargo test --release -p engine --test probe_duration -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::{StreamKind, probe_media};

/// The allowed disagreement between the engine and ffprobe, in seconds.
const TOLERANCE: f64 = 0.002;

/// Files that disagree for a reason outside the rules above, with the reason.
const ALLOWLIST: &[(&str, &str)] = &[(
    "test_short_video_long_audio.mp4",
    "the engine reports the picture's length (2.0 s) as its file duration, as it does \
     for any mp4 with a picture; ffprobe reports the longest stream (the 60 s audio). \
     The 60 s audio is listed as the audio stream's own duration.",
)];

fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

fn ffprobe_available() -> bool {
    Command::new("ffprobe")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The `OpusHead` pre-skip of an Ogg Opus file (RFC 7845 section 5.1: little-endian
/// u16 at byte 10 of the packet), `None` for a file that is not Ogg Opus.
fn opus_pre_skip(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"OggS") {
        return None;
    }
    let head = bytes.windows(8).position(|w| w == b"OpusHead")?;
    let at = head + 10;
    Some(u64::from(u16::from_le_bytes([
        *bytes.get(at)?,
        *bytes.get(at + 1)?,
    ])))
}

/// `ffprobe` with the given `-show_entries` and `-of csv=p=0`, one line per row.
/// The csv writer ends a row with a comma and quotes a value that holds one
/// (`"matroska,webm"`); both are taken off.
fn ffprobe_rows(path: &Path, entries: &str) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_end_matches(',').trim_matches('"').to_string())
        .collect()
}

/// ffprobe's `format=duration`, `None` where it states none (`N/A`).
fn ffprobe_format_secs(path: &Path) -> Option<f64> {
    ffprobe_rows(path, "format=duration").first()?.parse().ok()
}

/// End of the last packet on the first stream, in seconds: the measured length
/// of a stream with no stated duration.
fn ffprobe_packet_end_secs(path: &Path) -> Option<f64> {
    let last = ffprobe_rows(path, "packet=pts_time,duration_time")
        .into_iter()
        .rfind(|l| !l.is_empty())?;
    let (pts, dur) = last.split_once(',')?;
    Some(pts.parse::<f64>().ok()? + dur.parse::<f64>().ok()?)
}

/// Whether ffprobe finds an audio or video stream in the file.
fn has_av(path: &Path) -> bool {
    ffprobe_rows(path, "stream=codec_type")
        .iter()
        .any(|t| t == "audio" || t == "video")
}

#[test]
fn engine_duration_matches_ffprobe_for_every_asset() {
    if !ffprobe_available() {
        eprintln!("ffprobe not on the path: skipped");
        return;
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(assets_dir())
        .expect("assets directory")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && !engine::is_image(p))
        .collect();
    paths.sort();

    let (mut checked, mut skipped, mut failures) = (0, Vec::new(), Vec::new());
    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !has_av(path) {
            skipped.push(format!("{name} (no audio or video for ffprobe)"));
            continue;
        }
        let facts = probe_media(path);
        let Some(engine_secs) = facts.duration_secs else {
            skipped.push(format!("{name} (the engine refuses it)"));
            continue;
        };
        let format = ffprobe_rows(path, "format=format_name")
            .first()
            .cloned()
            .unwrap_or_default();
        let oracle = if format == "aac" {
            ffprobe_packet_end_secs(path)
        } else if let (Some(secs), Some(pre_skip)) =
            (ffprobe_format_secs(path), opus_pre_skip(path))
        {
            // RFC 7845 section 4: the playable length is the granule minus the pre-skip.
            Some(secs - pre_skip as f64 / 48_000.0)
        } else {
            ffprobe_format_secs(path)
        };
        let Some(oracle) = oracle else {
            skipped.push(format!("{name} (ffprobe states no duration)"));
            continue;
        };
        checked += 1;

        let mut tolerance = TOLERANCE;
        let matroska = format.contains("matroska");
        if matroska && let Some(fps) = facts.video.as_ref().and_then(|v| v.frame_rate) {
            tolerance += 1.0 / fps;
        }
        let diff = (engine_secs - oracle).abs();
        let allowed = ALLOWLIST.iter().any(|(n, _)| *n == name);
        if diff > tolerance && !allowed {
            failures.push(format!(
                "{name}: engine {engine_secs:.6} s, ffprobe {oracle:.6} s, off by {diff:.6} s"
            ));
        }
    }
    eprintln!(
        "probe_duration: {checked} checked, {} skipped",
        skipped.len()
    );
    for s in &skipped {
        eprintln!("  skipped: {s}");
    }
    assert!(
        failures.is_empty(),
        "durations off ffprobe by more than {TOLERANCE} s:\n{}",
        failures.join("\n")
    );
}

#[test]
fn mov_track_language_is_und_when_none_is_stated() {
    let path = assets_dir().join("test_qt.mov");
    let facts = probe_media(&path);
    for kind in [StreamKind::Video, StreamKind::Audio] {
        let stream = facts
            .streams
            .iter()
            .find(|s| s.kind == kind)
            .unwrap_or_else(|| panic!("test_qt.mov has no {kind:?} stream"));
        assert_eq!(
            stream.language.as_deref(),
            Some("und"),
            "{kind:?} language of test_qt.mov, an mdhd code of 0x7fff (no language)"
        );
    }
}
