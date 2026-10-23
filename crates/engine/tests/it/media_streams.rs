//! `probe_media`'s per-stream facts: every video, audio and subtitle stream the
//! header names, with the codec identifier the file spells (an mp4 sample entry
//! fourcc or a Matroska CodecID), and each stream's own header length. The old
//! single-stream fields are unchanged; these are additions beside them.

use std::path::{Path, PathBuf};

use engine::{StreamKind, probe_media};

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

#[test]
fn an_mp4_names_each_stream_by_its_sample_entry_fourcc() {
    let f = probe_media(&asset("test_av.mp4"));
    let video: Vec<_> = f
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .collect();
    assert_eq!(video.len(), 1);
    assert_eq!(video[0].codec_id.as_deref(), Some("avc1"));
    assert_eq!(video[0].index, 0);

    // An hvc1-tagged HEVC track keeps its own fourcc, not the hev1 the crate knows.
    let hvc1 = probe_media(&asset("test_hevc_hvc1.mp4"));
    assert_eq!(hvc1.streams[0].codec_id.as_deref(), Some("hvc1"));
}

#[test]
fn a_matroska_names_each_stream_by_its_codec_id() {
    let f = probe_media(&asset("test_subs.mkv"));
    let ids: Vec<(StreamKind, Option<&str>)> = f
        .streams
        .iter()
        .map(|s| (s.kind, s.codec_id.as_deref()))
        .collect();
    assert!(ids.contains(&(StreamKind::Video, Some("V_AV1"))), "{ids:?}");
    assert!(
        ids.contains(&(StreamKind::Subtitle, Some("S_TEXT/UTF8"))),
        "{ids:?}"
    );
    assert!(
        ids.contains(&(StreamKind::Subtitle, Some("S_TEXT/ASS"))),
        "{ids:?}"
    );
}

#[test]
fn every_audio_track_is_listed_not_just_the_first() {
    let mp4 = probe_media(&asset("test_multiaudio.mp4"));
    let audio: Vec<_> = mp4
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .map(|s| (s.index, s.codec_id.clone().unwrap_or_default()))
        .collect();
    assert_eq!(
        audio,
        vec![
            (0, "mp4a".to_string()),
            (1, "mp4a".to_string()),
            (2, "ac-3".to_string())
        ]
    );

    let mkv = probe_media(&asset("test_multiaudio.mkv"));
    let n = mkv
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .count();
    assert_eq!(n, 2);
}

#[test]
fn each_audio_stream_states_its_own_header_length() {
    // Track 2 of the mp4 is a little longer than track 1: each keeps its own
    // mdhd duration, not the file's.
    let f = probe_media(&asset("test_multiaudio.mp4"));
    let secs: Vec<f64> = f
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .map(|s| s.duration_secs.expect("mp4 states a track length"))
        .collect();
    assert_eq!(secs.len(), 3);
    assert!((secs[0] - 2.0232).abs() < 0.01, "{secs:?}");
    assert!((secs[1] - 2.0464).abs() < 0.01, "{secs:?}");

    // A long commentary track is reported as its own, beside a short picture.
    let long = probe_media(&asset("test_short_video_long_audio.mp4"));
    let audio = long
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .expect("an audio stream");
    assert!(audio.duration_secs.unwrap() > 59.0);
    assert!(
        long.duration_secs.unwrap() < 3.0,
        "picture length unchanged"
    );
}

#[test]
fn matroska_gives_every_track_the_files_length() {
    // Matroska has no per-track length, so each track reports the file's
    // `Info.Duration` times its `TimestampScale`: the same value for every one.
    let f = probe_media(&asset("test_subs.mkv"));
    assert_eq!(f.streams.len(), 3);
    for s in &f.streams {
        let secs = s.duration_secs.expect("the Info length");
        assert!((secs - 5.0).abs() < 0.01, "{secs} for {:?}", s.codec_id);
    }

    let multi = probe_media(&asset("test_multiaudio.mkv"));
    let secs: Vec<Option<f64>> = multi.streams.iter().map(|s| s.duration_secs).collect();
    assert!(secs.len() >= 2, "{secs:?}");
    assert!(secs[0].is_some(), "{secs:?}");
    assert!(secs.iter().all(|s| *s == secs[0]), "{secs:?}");
}

#[test]
fn a_picture_the_decoder_refuses_still_lists_its_streams() {
    // Playback refuses a display matrix that is not a quarter turn by name,
    // and the stream list must not go with the picture.
    let rot = probe_media(&asset("test_rotation45.mkv"));
    assert_eq!(rot.streams.len(), 1, "{:?}", rot.streams);
    assert_eq!(rot.streams[0].kind, StreamKind::Video);
    assert_eq!(rot.streams[0].codec_id.as_deref(), Some("V_MPEG4/ISO/AVC"));
}

#[test]
fn a_wav_has_one_audio_stream_named_by_its_codec() {
    let f = probe_media(&asset("test_tone_48k.wav"));
    assert_eq!(f.streams.len(), 1);
    assert_eq!(f.streams[0].kind, StreamKind::Audio);
    assert_eq!(f.streams[0].codec_id.as_deref(), Some("pcm_s16le"));
    assert!(f.streams[0].duration_secs.is_some());
}

#[test]
fn the_existing_fields_are_unchanged() {
    let f = probe_media(&asset("test_av.mp4"));
    assert_eq!(f.container.as_deref(), Some("mp4"));
    assert!(f.video.is_some());
    assert!(f.duration_secs.is_some());
}
