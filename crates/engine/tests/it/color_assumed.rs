//! `VideoFacts::matrix_assumed` against ffprobe's `color_space`. An untagged picture
//! gets its matrix from the size rule, and ffprobe calls that `unknown`; a tagged one
//! names its matrix, and ffprobe says so. The two must agree exactly, in mp4 and in
//! Matroska, for an untagged H.264 at 720p, the same clip tagged BT.709, a tagged
//! 480p BT.470BG clip, a tagged 10-bit HEVC BT.2020 PQ clip, and the untagged HEVC
//! asset `test_hevc10.mkv`.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::probe_media;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn run_ffmpeg(args: &[&str]) {
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed: {args:?}");
}

/// The first video stream's `color_space` as ffprobe reports it.
fn ffprobe_color_space(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_space",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed for {}",
        path.display()
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Checks one file: ffprobe calls its matrix untagged (`unknown` or empty) exactly
/// when the engine says the matrix was assumed.
fn check(path: &Path, expect_assumed: bool) {
    let space = ffprobe_color_space(path);
    let untagged = space.is_empty() || space == "unknown";
    assert_eq!(
        untagged,
        expect_assumed,
        "ffprobe color_space is {space:?} for {}",
        path.display()
    );
    let video = probe_media(path)
        .video
        .unwrap_or_else(|| panic!("no picture track in {}", path.display()));
    assert_eq!(
        video.matrix_assumed,
        expect_assumed,
        "matrix_assumed on {} (ffprobe color_space {space:?})",
        path.display()
    );
}

/// Generates a 0.3 s clip into the test temp dir as both an mp4 and an mkv, with the
/// given encoder, size, and extra arguments, and returns both paths.
fn generate(stem: &str, codec: &str, size: &str, extra: &[&str]) -> [PathBuf; 2] {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("color_assumed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("make the test dir");
    let source = format!("testsrc=size={size}:rate=25:duration=0.3");
    ["mp4", "mkv"].map(|ext| {
        let out = dir.join(format!("{stem}.{ext}"));
        let target = out.to_string_lossy().into_owned();
        let mut args = vec!["-f", "lavfi", "-i", source.as_str(), "-c:v", codec];
        args.extend_from_slice(extra);
        args.push(target.as_str());
        run_ffmpeg(&args);
        out
    })
}

#[test]
fn untagged_h264_720p_is_assumed() {
    for path in generate(
        "h264_untagged",
        "libx264",
        "1280x720",
        &["-pix_fmt", "yuv420p"],
    ) {
        check(&path, true);
    }
}

#[test]
fn tagged_bt709_h264_720p_is_not_assumed() {
    let extra = [
        "-pix_fmt",
        "yuv420p",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
    ];
    for path in generate("h264_bt709", "libx264", "1280x720", &extra) {
        check(&path, false);
    }
}

#[test]
fn tagged_bt470bg_480p_is_not_assumed() {
    let extra = [
        "-pix_fmt",
        "yuv420p",
        "-colorspace",
        "bt470bg",
        "-color_primaries",
        "bt470bg",
    ];
    for path in generate("h264_bt470bg", "libx264", "640x480", &extra) {
        check(&path, false);
    }
}

#[test]
fn tagged_bt2020_pq_hevc_is_not_assumed() {
    let extra = [
        "-pix_fmt",
        "yuv420p10le",
        "-colorspace",
        "bt2020nc",
        "-color_primaries",
        "bt2020",
        "-color_trc",
        "smpte2084",
    ];
    for path in generate("hevc_bt2020_pq", "libx265", "640x480", &extra) {
        check(&path, false);
    }
}

#[test]
fn untagged_hevc_asset_is_assumed() {
    check(&asset("test_hevc10.mkv"), true);
}
