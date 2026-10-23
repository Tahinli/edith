//! `probe_media`: what a file is beyond its streams -- the container it is
//! wrapped in, its size on disk, and what its picture is coded as. Every field
//! a file does not state is absent, never a made-up value.

use std::path::{Path, PathBuf};

use engine::{Codec, probe_media};

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

#[test]
fn the_size_is_the_file_on_disk() {
    for name in ["test_av.mp4", "test_av1_10.mkv", "test_tone_48k.wav"] {
        let path = asset(name);
        let facts = probe_media(&path);
        let on_disk = std::fs::metadata(&path).expect("fixture exists").len();
        assert_eq!(facts.bytes, Some(on_disk), "{name}");
    }
}

#[test]
fn the_container_is_named_by_its_header_where_the_header_says() {
    assert_eq!(
        probe_media(&asset("test_av.mp4")).container.as_deref(),
        Some("mp4")
    );
    // A QuickTime file is an mp4-family file whose brand says so; the
    // extension alone would have called it mp4.
    assert_eq!(
        probe_media(&asset("test_qt.mov")).container.as_deref(),
        Some("mov")
    );
    assert_eq!(
        probe_media(&asset("test_av1_10.mkv")).container.as_deref(),
        Some("mkv")
    );
    assert_eq!(
        probe_media(&asset("test_opus_51.mka")).container.as_deref(),
        Some("mka")
    );
}

#[test]
fn a_picture_track_is_described_by_its_coded_shape() {
    let facts = probe_media(&asset("test_av.mp4"));
    let video = facts.video.expect("test_av.mp4 has a picture");
    assert_eq!(video.codec, Codec::H264);
    assert!(video.width > 0 && video.height > 0, "{video:?}");
    assert!(video.frame_rate.is_some_and(|fps| fps > 0.0), "{video:?}");
    // H.264's depth is not read anywhere, so it is not answered at all.
    assert_eq!(video.bit_depth, None);
}

#[test]
fn a_stated_bit_depth_is_reported_for_the_codecs_that_state_one() {
    let av1 = probe_media(&asset("test_av1_10.mkv"))
        .video
        .expect("a picture");
    assert_eq!(av1.codec, Codec::Av1);
    assert_eq!(av1.bit_depth, Some(10));
    let hevc = probe_media(&asset("test_hevc10.mkv"))
        .video
        .expect("a picture");
    assert_eq!(hevc.codec, Codec::Hevc);
    assert_eq!(hevc.bit_depth, Some(10));
}

#[test]
fn a_file_with_no_picture_says_so() {
    assert!(probe_media(&asset("test_tone_48k.wav")).video.is_none());
    assert!(probe_media(&asset("test_audio_only.mp4")).video.is_none());
    assert_eq!(
        probe_media(&asset("test_tone_48k.wav"))
            .container
            .as_deref(),
        Some("wav")
    );
}

#[test]
fn a_file_that_will_not_open_has_no_picture_and_no_guess() {
    let facts = probe_media(Path::new("/nonexistent/nothing.mp4"));
    assert_eq!(facts.bytes, None);
    assert!(facts.video.is_none());
}

#[test]
fn a_stated_duration_is_the_header_length_for_video_and_sound() {
    // Both the mp4 and the Matroska fixtures state their length in the header;
    // the wav's length is the audio header's. Checked against ffprobe: 5.000 s,
    // 2.023 s, 3.000 s.
    let mp4 = probe_media(&asset("test_av.mp4")).duration_secs.expect("mvhd");
    assert!((mp4 - 5.0).abs() < 0.05, "{mp4}");
    let mkv = probe_media(&asset("test_h264.mkv"))
        .duration_secs
        .expect("Info.Duration");
    assert!((mkv - 2.023).abs() < 0.05, "{mkv}");
    let wav = probe_media(&asset("test_tone.wav"))
        .duration_secs
        .expect("audio header");
    assert!((wav - 3.0).abs() < 0.05, "{wav}");
    // An image has no playing time of its own.
    assert_eq!(probe_media(&asset("test_still.png")).duration_secs, None);
}
