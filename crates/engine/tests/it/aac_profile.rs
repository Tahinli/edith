//! The AAC profile a probe states (`StreamFacts::profile`), checked against
//! ffprobe's `profile` field for every case: AAC-LC, HE-AAC v1 and HE-AACv2, each
//! with explicit signalling (the config names SBR or PS) and implicit (a plain
//! AAC-LC config whose bitstream carries them), in mp4 and in Matroska. The
//! Matroska files are `-c copy` twins of the mp4 ones.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::{StreamKind, probe_media};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("run {program}: {e}"));
    assert!(status.success(), "{program} failed");
}

/// The first audio stream's `profile` as ffprobe reports it.
fn ffprobe_profile(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=profile",
        ])
        .args(["-of", "csv=p=0"])
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

/// The first audio stream's profile as the engine's probe states it.
fn probed_profile(path: &Path) -> Option<String> {
    probe_media(path)
        .streams
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio)
        .and_then(|s| s.profile)
}

/// Checks `file` (an mp4) and its mkv twin: the engine and ffprobe both say `expect`.
fn check(file: &Path, expect: &str) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("aac_profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("make the test dir");
    let stem = file.file_stem().expect("input has a file stem");
    let mkv = dir.join(format!("{}.mkv", stem.to_str().unwrap()));
    run(
        "ffmpeg",
        &[
            "-i",
            file.to_str().unwrap(),
            "-c",
            "copy",
            mkv.to_str().unwrap(),
        ],
    );
    for path in [file, mkv.as_path()] {
        assert_eq!(
            ffprobe_profile(path),
            expect,
            "ffprobe on {}",
            path.display()
        );
        assert_eq!(
            probed_profile(path).as_deref(),
            Some(expect),
            "probe_media on {}",
            path.display()
        );
    }
}

#[test]
fn aac_lc_matches_ffprobe_in_mp4_and_mkv() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("aac_profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("make the test dir");
    let lc = dir.join("lc.m4a");
    run(
        "ffmpeg",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac",
            "-profile:a",
            "aac_low",
            "-b:a",
            "64k",
            lc.to_str().unwrap(),
        ],
    );
    check(&lc, "LC");
}

#[test]
fn explicit_he_aac_v1_matches_ffprobe_in_mp4_and_mkv() {
    check(&data("aac_explicit_he_v1.m4a"), "HE-AAC");
}

#[test]
fn explicit_he_aac_v2_matches_ffprobe_in_mp4_and_mkv() {
    check(&data("aac_explicit_he_v2.m4a"), "HE-AACv2");
}

#[test]
fn implicit_he_aac_v1_matches_ffprobe_in_mp4_and_mkv() {
    check(&data("aac_implicit_he_v1.m4a"), "HE-AAC");
}

#[test]
fn implicit_he_aac_v2_matches_ffprobe_in_mp4_and_mkv() {
    check(&data("aac_he_v2_implicit_stereo.m4a"), "HE-AACv2");
}
