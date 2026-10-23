//! The channel count and sample rate a file's header states, per audio stream.
//! Checked against what ffprobe reports for the same assets.

use std::path::PathBuf;

use engine::{probe_media, StreamFacts, StreamKind};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn audio(path: &str) -> Vec<StreamFacts> {
    probe_media(&asset(path))
        .streams
        .into_iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .collect()
}

/// `test_multiaudio.mp4`: stereo 44.1 kHz, mono 22.05 kHz, stereo 44.1 kHz AC-3.
#[test]
fn mp4_sample_entries_state_channels_and_rate() {
    let tracks = audio("test_multiaudio.mp4");
    assert_eq!(tracks.len(), 3, "{tracks:?}");
    assert_eq!((tracks[0].channels, tracks[0].sample_rate), (Some(2), Some(44_100)));
    assert_eq!((tracks[1].channels, tracks[1].sample_rate), (Some(1), Some(22_050)));
    assert_eq!(tracks[1].language.as_deref(), Some("fra"), "the index and language agree");
}

/// An mp4 AC-3 entry's own channel count is 2 in most files; the `dac3` box says what it carries.
#[test]
fn mp4_ac3_channels_come_from_the_dac3_box() {
    let surround = audio("test_ac3_51.mp4");
    assert_eq!(surround.len(), 1, "{surround:?}");
    assert_eq!(surround[0].codec_id.as_deref(), Some("ac-3"));
    assert_eq!(surround[0].channels, Some(6), "5.1 is six channels, not the entry's count");
    assert_eq!(surround[0].sample_rate, Some(48_000));

    let mono = audio("test_ac3.mp4");
    assert_eq!(mono[0].channels, Some(1), "{mono:?}");
    assert_eq!(mono[0].sample_rate, Some(44_100));
}

/// A Matroska `Audio` element states `Channels` and `SamplingFrequency`.
#[test]
fn matroska_audio_elements_state_channels_and_rate() {
    let eac3 = audio("test_eac3.mkv");
    assert_eq!(eac3.len(), 1, "{eac3:?}");
    assert_eq!(eac3[0].channels, Some(6));
    assert_eq!(eac3[0].sample_rate, Some(48_000));

    let stereo = audio("test_multiaudio.mkv");
    assert_eq!(stereo.len(), 2, "{stereo:?}");
    assert_eq!((stereo[1].channels, stereo[1].sample_rate), (Some(2), Some(44_100)));
}

/// Picture tracks carry no audio layout.
#[test]
fn video_streams_state_no_audio_layout() {
    let facts = probe_media(&asset("test_multiaudio.mp4"));
    for s in facts.streams.iter().filter(|s| s.kind != StreamKind::Audio) {
        assert_eq!((s.channels, s.sample_rate), (None, None), "{s:?}");
    }
}
