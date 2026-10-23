//! The sample depth a probe states (`StreamFacts::bit_depth`), checked against
//! ffprobe for FLAC and ALAC at 16 and 24 bits in every container that carries
//! them (bare `.flac` or `.m4a`, mp4 and Matroska), for PCM at 16 and 24 bits in
//! WAV and Matroska, and against `None` for AAC, which is lossy.
//!
//! ffprobe's field is `bits_per_raw_sample` for FLAC and ALAC. For PCM ffprobe
//! fills `bits_per_sample` and leaves `bits_per_raw_sample` out, so PCM is
//! compared on `bits_per_sample`. A field that is `N/A` or `0` is `None`.
//! Fixtures are generated into `CARGO_TARGET_TMPDIR`, each about 0.3 s long.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::{StreamKind, probe_media};

/// One fixture: its name, the ffmpeg codec arguments, the ffprobe field that
/// states its depth, and the depth the fixture is meant to carry.
struct Case {
    name: &'static str,
    codec: &'static [&'static str],
    field: &'static str,
    expect: Option<u32>,
}

const FLAC16: &[&str] = &["-c:a", "flac", "-sample_fmt", "s16"];
const FLAC24: &[&str] = &[
    "-c:a",
    "flac",
    "-sample_fmt",
    "s32",
    "-bits_per_raw_sample",
    "24",
];
const ALAC16: &[&str] = &["-c:a", "alac", "-sample_fmt", "s16p"];
const ALAC24: &[&str] = &["-c:a", "alac", "-sample_fmt", "s32p"];
const PCM16: &[&str] = &["-c:a", "pcm_s16le"];
const PCM24: &[&str] = &["-c:a", "pcm_s24le"];
const AAC: &[&str] = &["-c:a", "aac"];

const RAW: &str = "bits_per_raw_sample";
const PCM: &str = "bits_per_sample";

const CASES: &[Case] = &[
    Case {
        name: "flac16.flac",
        codec: FLAC16,
        field: RAW,
        expect: Some(16),
    },
    Case {
        name: "flac24.flac",
        codec: FLAC24,
        field: RAW,
        expect: Some(24),
    },
    Case {
        name: "flac16.mp4",
        codec: FLAC16,
        field: RAW,
        expect: Some(16),
    },
    Case {
        name: "flac24.mp4",
        codec: FLAC24,
        field: RAW,
        expect: Some(24),
    },
    Case {
        name: "flac16.mkv",
        codec: FLAC16,
        field: RAW,
        expect: Some(16),
    },
    Case {
        name: "flac24.mkv",
        codec: FLAC24,
        field: RAW,
        expect: Some(24),
    },
    Case {
        name: "alac16.m4a",
        codec: ALAC16,
        field: RAW,
        expect: Some(16),
    },
    Case {
        name: "alac24.m4a",
        codec: ALAC24,
        field: RAW,
        expect: Some(24),
    },
    Case {
        name: "alac16.mkv",
        codec: ALAC16,
        field: RAW,
        expect: Some(16),
    },
    Case {
        name: "alac24.mkv",
        codec: ALAC24,
        field: RAW,
        expect: Some(24),
    },
    Case {
        name: "pcm16.wav",
        codec: PCM16,
        field: PCM,
        expect: Some(16),
    },
    Case {
        name: "pcm24.wav",
        codec: PCM24,
        field: PCM,
        expect: Some(24),
    },
    Case {
        name: "pcm16.mkv",
        codec: PCM16,
        field: PCM,
        expect: Some(16),
    },
    Case {
        name: "pcm24.mkv",
        codec: PCM24,
        field: PCM,
        expect: Some(24),
    },
    Case {
        name: "aac.m4a",
        codec: AAC,
        field: RAW,
        expect: None,
    },
];

fn make(path: &Path, codec: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=f=440:d=0.3")
        .args(codec)
        .arg(path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed for {}", path.display());
}

/// The first audio stream's `field` as ffprobe reports it; `None` for `N/A` or 0.
fn ffprobe_depth(path: &Path, field: &str) -> Option<u32> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries"])
        .arg(format!("stream={field}"))
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed for {}",
        path.display()
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|d| *d > 0)
}

/// The first audio stream's `bit_depth` as the engine's probe states it.
fn probed_depth(path: &Path) -> Option<u32> {
    probe_media(path)
        .streams
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio)
        .and_then(|s| s.bit_depth)
}

#[test]
fn bit_depth_matches_ffprobe_for_lossless_and_is_none_for_aac() {
    let dir: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("audio_bit_depth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("make the test dir");
    let mut failures = Vec::new();
    for case in CASES {
        let path = dir.join(case.name);
        make(&path, case.codec);
        let want = ffprobe_depth(&path, case.field);
        // The fixture must carry the depth it is named for, or the comparison
        // below proves nothing.
        assert_eq!(
            want, case.expect,
            "ffprobe {} for {}",
            case.field, case.name
        );
        let got = probed_depth(&path);
        if got != want {
            failures.push(format!("{}: engine {got:?}, ffprobe {want:?}", case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "bit_depth mismatches:\n{}",
        failures.join("\n")
    );
}
