//! The cover art a probe reports (`MediaFacts::attached_pic`): an ID3v2 `APIC`,
//! an mp4 `covr` atom, a FLAC `PICTURE` block and a Matroska `image/*`
//! attachment. A cover is never a playable stream, so the audio file that
//! carries one stays audio-only. Fixtures are made with ffmpeg at test time.

use std::path::{Path, PathBuf};
use std::process::Command;

use engine::{StreamKind, probe_media};

/// A fresh directory for one test's fixtures, under the system temp dir.
fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("edith-cover-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fixture dir");
    dir
}

/// Runs ffmpeg with `args`, failing the test with its stderr when it does not succeed.
fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A tiny generated png, the cover every fixture embeds.
fn cover_png(dir: &Path) -> PathBuf {
    let png = dir.join("cover.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64x64",
        "-frames:v",
        "1",
        png.to_str().expect("utf-8 path"),
    ]);
    png
}

/// A one-second sine tone, the audio every fixture carries.
const TONE: [&str; 4] = ["-f", "lavfi", "-i", "sine=frequency=440:duration=1"];

#[test]
fn an_mp3_with_an_id3_apic_frame_has_a_cover_and_stays_audio_only() {
    let dir = fixture_dir("mp3-apic");
    let png = cover_png(&dir);
    let mp3 = dir.join("apic.mp3");
    ffmpeg(&[
        TONE[0],
        TONE[1],
        TONE[2],
        TONE[3],
        "-i",
        png.to_str().unwrap(),
        "-map",
        "0",
        "-map",
        "1",
        "-c:a",
        "libmp3lame",
        "-c:v",
        "mjpeg",
        "-disposition:v",
        "attached_pic",
        "-id3v2_version",
        "3",
        mp3.to_str().unwrap(),
    ]);
    let facts = probe_media(&mp3);
    assert!(facts.attached_pic, "an APIC frame is cover art");
    assert!(facts.video.is_none(), "a cover is not the picture track");
    assert!(
        facts.streams.iter().all(|s| s.kind != StreamKind::Video),
        "a cover is not listed as a video stream: {:?}",
        facts.streams
    );
}

#[test]
fn an_m4a_with_a_covr_atom_has_a_cover() {
    let dir = fixture_dir("m4a-covr");
    let png = cover_png(&dir);
    let m4a = dir.join("covr.m4a");
    ffmpeg(&[
        TONE[0],
        TONE[1],
        TONE[2],
        TONE[3],
        "-i",
        png.to_str().unwrap(),
        "-map",
        "0:a",
        "-map",
        "1",
        "-c:a",
        "aac",
        "-c:v",
        "mjpeg",
        "-disposition:v",
        "attached_pic",
        m4a.to_str().unwrap(),
    ]);
    let facts = probe_media(&m4a);
    assert!(facts.attached_pic, "a covr atom is cover art");
    assert!(facts.video.is_none());
    assert!(facts.streams.iter().all(|s| s.kind != StreamKind::Video));
}

#[test]
fn a_flac_with_a_picture_block_has_a_cover() {
    let dir = fixture_dir("flac-picture");
    let png = cover_png(&dir);
    let flac = dir.join("picture.flac");
    ffmpeg(&[
        TONE[0],
        TONE[1],
        TONE[2],
        TONE[3],
        "-i",
        png.to_str().unwrap(),
        "-map",
        "0:a",
        "-map",
        "1",
        "-c:a",
        "flac",
        "-c:v",
        "png",
        "-disposition:v",
        "attached_pic",
        flac.to_str().unwrap(),
    ]);
    let facts = probe_media(&flac);
    assert!(facts.attached_pic, "a METADATA_BLOCK_PICTURE is cover art");
    assert!(facts.video.is_none());
    assert!(facts.streams.iter().all(|s| s.kind != StreamKind::Video));
}

#[test]
fn a_matroska_image_attachment_is_a_cover() {
    let dir = fixture_dir("mkv-attach");
    let png = cover_png(&dir);
    let mkv = dir.join("attach.mkv");
    ffmpeg(&[
        TONE[0],
        TONE[1],
        TONE[2],
        TONE[3],
        "-map",
        "0:a",
        "-c:a",
        "libopus",
        "-attach",
        png.to_str().unwrap(),
        "-metadata:s:t",
        "mimetype=image/png",
        mkv.to_str().unwrap(),
    ]);
    let facts = probe_media(&mkv);
    assert!(facts.attached_pic, "an image/* attachment is cover art");
    assert!(facts.streams.iter().all(|s| s.kind != StreamKind::Video));
}

#[test]
fn a_plain_mp3_has_no_cover() {
    let dir = fixture_dir("mp3-plain");
    let mp3 = dir.join("plain.mp3");
    ffmpeg(&[
        TONE[0],
        TONE[1],
        TONE[2],
        TONE[3],
        "-c:a",
        "libmp3lame",
        mp3.to_str().unwrap(),
    ]);
    let facts = probe_media(&mp3);
    assert!(!facts.attached_pic);
    assert!(facts.video.is_none());
}
