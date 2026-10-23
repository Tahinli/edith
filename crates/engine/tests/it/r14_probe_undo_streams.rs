//! Dogfood round 14 on the engine's file-level answers: what a probe says about
//! a file's streams and refusals, what an import says about sound it cannot
//! play, rolling back a just-added source without losing undo, and placing a
//! clip from a chosen audio track of a multi-audio file.
//!
//! ```text
//! cargo test -p engine --release --test r14_probe_undo_streams -- --nocapture
//! ```

use std::path::PathBuf;

use engine::demux::{StreamKind, picture_refusal, probe_media};
use engine::{AudioSession, PlaybackSession};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

fn quiet(session: &mut PlaybackSession) {
    session.set_gain(0.0);
}

/// Audio languages as ffprobe reads them, in file order.
#[test]
fn mp4_audio_languages_match_the_file() {
    let facts = probe_media(&asset("test_multiaudio.mp4"));
    let audio: Vec<_> = facts
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .map(|s| s.language.clone())
        .collect();
    assert_eq!(
        audio,
        [Some("und".into()), Some("fra".into()), Some("und".into())],
        "the second track is French, read from its mdhd"
    );
    let lang = probe_media(&asset("test_multilang.mp4"));
    let second = lang
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .nth(1)
        .and_then(|s| s.language.clone());
    assert_eq!(second.as_deref(), Some("fra"));
}

/// Codec identifiers for sound-only files, the names ffprobe gives them.
#[test]
fn audio_only_files_name_their_codec() {
    let cases = [
        ("test_tone.flac", "flac"),
        ("test_tone.mp3", "mp3"),
        ("test_tone.opus", "opus"),
        ("test_tone_cover.mpeg", "mp3"),
    ];
    for (name, codec) in cases {
        let facts = probe_media(&asset(name));
        let first = facts.streams.first().unwrap_or_else(|| panic!("{name}: no streams"));
        assert_eq!(first.codec_id.as_deref(), Some(codec), "{name}");
    }
}

/// A refused picture still lists its streams, and says why.
#[test]
fn refused_pictures_keep_their_streams_and_say_why() {
    {
        let name = "test_rotation45.mkv";
        let facts = probe_media(&asset(name));
        assert!(facts.video.is_none(), "{name}: the picture is refused");
        assert!(
            facts.streams.iter().any(|s| s.kind == StreamKind::Video),
            "{name}: the video stream is still listed"
        );
        let why = picture_refusal(&asset(name)).unwrap_or_else(|| panic!("{name}: no reason"));
        assert!(!why.is_empty(), "{name}");
    }
    assert_eq!(picture_refusal(&asset("test_av.mp4")), None, "a playable picture has no refusal");
    assert_eq!(picture_refusal(&asset("test_tone.flac")), None, "sound alone has no picture to refuse");
}

/// Sound the build cannot decode imports with its picture, and the import says
/// so as a returned warning -- not on stderr, and not as a silent success.
#[test]
fn unsupported_sound_on_import_is_a_returned_warning() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the picture file");
    quiet(&mut session);
    let (source, warnings) = session
        .import_with_warnings(&asset("test_dts.mkv"))
        .expect("the picture imports");
    assert!(session.sources().len() >= 2 && source < session.sources().len());
    // DTS decodes now (`ec-dts`), so its track imports with sound and no warning.
    assert!(warnings.is_empty(), "a DTS track that decodes draws no warning: {warnings:?}");

    // A file whose sound plays draws no warning at all.
    let (_, clean) = session
        .import_with_warnings(&asset("test_multiaudio.mkv"))
        .expect("a playable file imports");
    assert!(clean.is_empty(), "no warning for playable sound: {clean:?}");
}

/// The rollback door: a source imported and then dropped while it is unused
/// leaves the undo history as it was, and undo still works afterwards.
#[test]
fn removing_an_unused_source_keeps_undo() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    quiet(&mut session);
    let clips_before = session.lane_clips(session.lanes()[0]).len();
    let depth_before = session.undo_depth();

    // An edit that gives undo something to do: take the first clip off the lane.
    let lane = session.lanes()[0];
    session.delete_clip(lane, 0);
    let depth_after_edit = session.undo_depth();
    assert!(depth_after_edit > depth_before, "the delete is undoable");

    // Import, then roll the import back.
    let other = asset("test_multiaudio.mkv");
    let idx = session.import(&other).expect("import");
    session.remove_unused_source(&other, 0).expect("the unused last source goes");
    assert_eq!(session.sources().len(), idx, "back to the library before the import");
    assert_eq!(
        session.undo_depth(),
        depth_after_edit,
        "the history is not cleared by the rollback"
    );

    // And undo still restores the clip it removed.
    assert!(session.undo());
    assert_eq!(session.lane_clips(lane).len(), clips_before, "undo put the clip back");
}

/// A source a clip still plays, or an undo step still names, is refused, and
/// the library is left alone.
#[test]
fn an_unused_source_refuses_when_something_still_names_it() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    quiet(&mut session);
    let other = asset("test_multiaudio.mkv");
    session.import(&other).expect("import");
    let end = session.timeline_duration();
    assert!(session.place_stream_at(end, &other, 0, None).expect("place"));
    let sources = session.sources().len();
    assert!(
        session.remove_unused_source(&other, 0).is_err(),
        "a clip plays it"
    );
    assert_eq!(session.sources().len(), sources, "nothing was removed");
}

/// A chosen audio track of a multi-audio file is placed by importing the file
/// and then placing the stream: `import` takes stream 0, `place_stream_at` takes
/// any other. The placed source keeps its stream number through a save.
#[test]
fn a_chosen_audio_track_is_placed_and_saved() {
    let dir = engine::scratch::Scratch::dir("ve_r14_stream_place");
    let file = dir.join("test_multiaudio.mkv");
    std::fs::copy(asset("test_multiaudio.mkv"), &file).expect("copy");
    let mut session = PlaybackSession::open(&file).expect("open the multi-audio file");
    quiet(&mut session);
    let end = session.timeline_duration();
    assert!(
        session.place_stream_at(end, &file, 1, None).expect("stream 1 is French"),
        "the French track is placed"
    );
    let streams: Vec<usize> = session.sources().iter().map(|s| s.audio_stream).collect();
    assert_eq!(streams, [0, 1], "stream 1 is its own source");

    let path = dir.join("pick.edith");
    session.save_project(&path).expect("save");
    let loaded = PlaybackSession::open_project(&path).expect("reopen");
    let back: Vec<usize> = loaded.sources().iter().map(|s| s.audio_stream).collect();
    assert_eq!(back, [0, 1], "the chosen track comes back as the source it was");
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// The unsupported-sound answer flags only what no decoder here can take: AAC in
/// an mp4 and DTS in a Matroska file both decode, so neither is flagged.
#[test]
fn unsupported_flags_only_what_cannot_decode() {
    assert_eq!(
        AudioSession::unsupported(asset("test_av.mp4")).expect("header reads"),
        None,
        "AAC in an mp4 plays and is not a refusal"
    );
    assert_eq!(
        AudioSession::unsupported(asset("test_dts.mkv")).expect("header reads"),
        None,
        "DTS in a Matroska file decodes and is not a refusal"
    );
}
