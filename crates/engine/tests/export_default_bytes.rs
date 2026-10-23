//! Exports that ask for none of the ProRes/HDR settings are the files they always
//! were: the bytes of the default H.264 and HEVC exports are pinned (size and
//! md5), captured on main c890955a before `Format::ProRes` and `ExportSettings::hdr`
//! existed. A picture-only source (the interleave of a sound track is not
//! deterministic), the software seats.
//!
//! ```text
//! cargo test -p engine --release --test export_default_bytes -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use engine::PlaybackSession;
use engine::export::{EncoderSeat, ExportSettings, Format};

#[path = "support/seat.rs"]
mod seat;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 25;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Cores the pins were captured with.
const PIN_CORES: usize = 4;

fn have_taskset() -> bool {
    Command::new("taskset").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn run(source: &Path, out: &Path, settings: &ExportSettings) -> Result<(), String> {
    let session = PlaybackSession::open(source).unwrap_or_else(|e| panic!("open {}: {e}", source.display()));
    let handle = session.export_to_with(out, settings);
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(600), "the export did not finish in 600 s");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.result().expect("an outcome").map_err(|e| e.to_string())
}

/// MD5 of the file's bytes.
fn file_md5(path: &Path) -> String {
    let out = Command::new("md5sum").arg(path).output().expect("md5sum runs");
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or("").to_owned()
}

#[test]
fn default_exports_are_byte_identical_to_before() {
    if !have("ffmpeg") {
        eprintln!("no ffmpeg: skipping");
        return;
    }
    // The software H.264 and HEVC encoders cut the picture into one slice band per
    // core (`available_parallelism`), so the bytes depend on the core count: the pins
    // below were captured on four. On any other machine the test runs itself again
    // pinned to four CPUs, which is the same four cores to the encoders.
    if std::thread::available_parallelism().map_or(1, |n| n.get()) != PIN_CORES {
        if std::env::var_os("EDITH_PIN_REEXEC").is_some() {
            eprintln!("could not pin to {PIN_CORES} CPUs: skipping");
            return;
        }
        if !have_taskset() {
            eprintln!("no taskset: skipping");
            return;
        }
        let status = Command::new("taskset")
            .args(["-c", &format!("0-{}", PIN_CORES - 1)])
            .arg(std::env::current_exe().expect("this test binary"))
            .args(["--exact", "default_exports_are_byte_identical_to_before", "--nocapture"])
            .env("EDITH_PIN_REEXEC", "1")
            .status()
            .expect("taskset runs");
        assert!(status.success(), "the export on {PIN_CORES} CPUs changed");
        return;
    }
    let _seat = seat::software();
    let dir = scratch("hevc10-defaults");
    let source = dir.join("plain.mp4");
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={WIDTH}x{HEIGHT}:rate={FPS}:duration=1"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "12", "-pix_fmt", "yuv420p", "-an"])
        .arg(&source)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg makes the source");
    let mut report = String::new();
    for (name, settings, ext) in [
        ("h264", ExportSettings { format: Format::Mp4, seat: EncoderSeat::Software, ..Default::default() }, "mp4"),
        ("hevc-mp4", ExportSettings { format: Format::HevcMp4, seat: EncoderSeat::Software, ..Default::default() }, "mp4"),
        ("hevc-mkv", ExportSettings { format: Format::Hevc, seat: EncoderSeat::Software, ..Default::default() }, "mkv"),
        ("hevc10-mp4", ExportSettings { format: Format::HevcMp4, seat: EncoderSeat::Software, video_bit_depth: Some(10), ..Default::default() }, "mp4"),
        ("hevc-intra-mkv", ExportSettings { format: Format::Hevc, seat: EncoderSeat::Software, intra_only: true, ..Default::default() }, "mkv"),
    ] {
        let out = dir.join(format!("{name}.{ext}"));
        run(&source, &out, &settings).unwrap_or_else(|e| panic!("{name}: {e}"));
        let size = std::fs::metadata(&out).expect("the file").len();
        let again = dir.join(format!("{name}-again.{ext}"));
        run(&source, &again, &settings).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (a, b) = (file_md5(&out), file_md5(&again));
        eprintln!("PIN {name} {size} {a}");
        report.push_str(&format!("{name} {size} {a}\n"));
        assert_eq!(a, b, "{name}: the export is deterministic");
        if let Some((_, pinned)) = PINS.iter().find(|(n, _)| *n == name) {
            assert_eq!(format!("{size} {a}"), *pinned, "{name}: the default export changed");
        }
    }
    eprintln!("{report}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `(name, "size md5")` of the default exports above, captured on main c890955a.
const PINS: &[(&str, &str)] = &[
    ("h264", "28816 99882583a60538757d8c2cfb16f24f95"),
    ("hevc-mp4", "34617 ab1f3164ffc3f79cf41d19ba78a85261"),
    ("hevc-mkv", "34207 7a400530e2d541b203a5d4963e160acc"),
    ("hevc10-mp4", "34446 4659cc08bf7227ec1609e086c7fd9078"),
    ("hevc-intra-mkv", "39154 1099ec487e0c165f41c7302be18ef775"),
];
