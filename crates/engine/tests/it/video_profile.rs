//! The video profile a probe states (`VideoFacts::profile`), checked against
//! ffprobe's `stream=profile` for every video in `assets/` whose codec is H.264,
//! HEVC, AV1, VP9 or VP8, and for a set of small streams this test encodes itself
//! with ffmpeg: each one in Matroska and, where ffmpeg can remux it, in mp4.
//!
//! A file the engine refuses to open has no `VideoFacts` and is skipped. The
//! generated files live under `CARGO_TARGET_TMPDIR`, never in the source tree.
//!
//! ```text
//! cargo test --release -p engine --test video_profile
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use engine::probe_media;

/// The codecs whose profile this checks.
const CODECS: [&str; 5] = ["h264", "hevc", "av1", "vp9", "vp8"];

/// `(name, ffmpeg encoder, extra arguments)`: one half-second stream per case.
const ENCODED: &[(&str, &str, &[&str])] = &[
    (
        "h264_baseline",
        "libx264",
        &["-profile:v", "baseline", "-pix_fmt", "yuv420p"],
    ),
    (
        "h264_main",
        "libx264",
        &["-profile:v", "main", "-pix_fmt", "yuv420p"],
    ),
    (
        "h264_high",
        "libx264",
        &["-profile:v", "high", "-pix_fmt", "yuv420p"],
    ),
    (
        "h264_high10",
        "libx264",
        &["-profile:v", "high10", "-pix_fmt", "yuv420p10le"],
    ),
    (
        "hevc_main",
        "libx265",
        &["-pix_fmt", "yuv420p", "-x265-params", "log-level=error"],
    ),
    (
        "hevc_main10",
        "libx265",
        &["-pix_fmt", "yuv420p10le", "-x265-params", "log-level=error"],
    ),
    (
        "av1_profile0",
        "libaom-av1",
        &[
            "-profile:v",
            "main",
            "-pix_fmt",
            "yuv420p",
            "-cpu-used",
            "8",
        ],
    ),
    (
        "vp9_profile0",
        "libvpx-vp9",
        &[
            "-pix_fmt",
            "yuv420p",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
        ],
    ),
    (
        "vp9_profile2",
        "libvpx-vp9",
        &[
            "-pix_fmt",
            "yuv420p10le",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
        ],
    ),
];

/// One ffprobe `stream=<field>` of the first video stream. `None` where ffprobe
/// cannot read the file; an empty string where the stream states no such field.
fn ffprobe_field(path: &Path, field: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg(format!("stream={field}"))
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Runs ffmpeg quietly and says whether it succeeded.
fn ffmpeg(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .expect("run ffmpeg")
        .success()
}

/// Checks `path`: the profile the probe states is the one ffprobe states, with
/// an empty ffprobe answer meaning `None`.
fn check(path: &Path) {
    let facts = probe_media(path)
        .video
        .unwrap_or_else(|| panic!("{}: no video facts", path.display()));
    let expect =
        ffprobe_field(path, "profile").unwrap_or_else(|| panic!("ffprobe: {}", path.display()));
    let expect = (!expect.is_empty()).then_some(expect);
    assert_eq!(
        facts.profile.map(str::to_owned),
        expect,
        "{}",
        path.display()
    );
}

/// Every video in `assets/` of one of [`CODECS`] that the engine opens.
#[test]
fn asset_videos_state_the_profile_ffprobe_states() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("read assets entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    let mut checked = 0;
    for path in &paths {
        let Some(codec) = ffprobe_field(path, "codec_name") else {
            continue;
        };
        if !CODECS.contains(&codec.as_str()) {
            continue;
        }
        if probe_media(path).video.is_none() {
            eprintln!("skipped (engine refuses): {}", path.display());
            continue;
        }
        check(path);
        checked += 1;
    }
    eprintln!("checked {checked} asset videos");
}

/// Each generated stream, in Matroska and in mp4 (remuxed by ffmpeg from the
/// Matroska one, skipped where ffmpeg cannot mux it).
#[test]
fn generated_streams_state_the_profile_ffprobe_states() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("video_profile-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create the output directory");
    for (name, encoder, args) in ENCODED {
        let mkv = dir.join(format!("{name}.mkv"));
        let mkv_s = mkv.to_str().expect("utf-8 path");
        let mut encode = vec![
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30",
            "-t",
            "0.5",
            "-c:v",
            encoder,
        ];
        encode.extend_from_slice(args);
        encode.push(mkv_s);
        assert!(ffmpeg(&encode), "{name}: ffmpeg could not encode");
        check(&mkv);

        let mp4 = dir.join(format!("{name}.mp4"));
        let mp4_s = mp4.to_str().expect("utf-8 path");
        if ffmpeg(&["-i", mkv_s, "-c", "copy", mp4_s]) {
            check(&mp4);
        } else {
            eprintln!("{name}: ffmpeg cannot mux it into mp4, mkv only");
        }
    }
}
