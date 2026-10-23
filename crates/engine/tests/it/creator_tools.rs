//! The creator tools the editor and the MCP server share: loudness normalizing, filler
//! cutting and the voiceover render. Each runs on a real session over `test_av.mp4`
//! (a 440/880 Hz stereo tone); the speech and piper ones skip where their binaries or models
//! are not installed.

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::normalize::{self, Scope};
use engine::project::Lane;
use engine::speech::{self, Spoken};

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

fn word(text: &str, a: u32, b: u32) -> Spoken {
    Spoken { text: text.into(), start_f: a, end_f: b, p: 0.9 }
}

#[test]
fn a_filler_is_padded_but_never_into_its_neighbours() {
    let words = [word("so", 0, 10), word("Um,", 12, 20), word("this", 21, 30), word("is", 30, 40), word("uh", 41, 50)];
    let found = speech::find_fillers(&words, 5, 60);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].text, "Um");
    // Pad 5 would reach 7..25, but the word before ends at 10 and the one after starts at 21.
    assert_eq!(found[0].cut, (10, 21));
    // The last one pads out to 55 (the timeline ends at 60) but starts at 40, where the word before ends.
    assert_eq!(found[1].cut, (40, 55));
}

#[test]
fn cutting_fillers_shortens_every_track_and_is_one_undo_step() {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let fps = s.meta().frame_rate;
    let before = s.timeline_duration();
    let lanes = s.lanes();
    let pushes = s.history_pushes();
    let at = (fps * 0.5) as u32;
    let words = [word("um", at, at + 6), word("hello", at + 8, at + 20)];
    let found = speech::find_fillers(&words, 0, (before * fps) as u32);
    assert_eq!(found.len(), 1);
    speech::cut_fillers(&mut s, &found, &lanes).expect("cut");
    let after = s.timeline_duration();
    assert!((before - after - 6.0 / fps).abs() < 1.5 / fps, "{before} -> {after}");
    // One step: a single undo restores the length.
    assert!(s.undo());
    assert!((s.timeline_duration() - before).abs() < 1e-6);
    assert!(s.history_pushes() > pushes);
}

#[test]
fn nothing_to_cut_is_refused_and_changes_nothing() {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let lanes = s.lanes();
    let pushes = s.history_pushes();
    assert!(speech::cut_fillers(&mut s, &[], &lanes).is_err());
    assert_eq!(s.history_pushes(), pushes);
}

#[test]
fn normalize_moves_the_mix_to_the_target_in_one_undo_step() {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let gain_before = s.lane_gain_db(Lane::A1);
    let report = normalize::normalize(&mut s, &Scope::Mix, -23.0, -1.0).expect("normalize");
    assert!(report.before.integrated_lufs.is_finite());
    assert!(
        (report.after.integrated_lufs - -23.0).abs() < 0.5 || report.peak_held,
        "landed on {} (before {})",
        report.after.integrated_lufs,
        report.before.integrated_lufs
    );
    if (report.before.integrated_lufs - -23.0).abs() > 0.5 {
        assert!((s.lane_gain_db(Lane::A1) - gain_before).abs() > 0.05, "the lane gain did not move");
    }
    // One undo puts the gain back.
    assert!(s.undo());
    assert!((s.lane_gain_db(Lane::A1) - gain_before).abs() < 1e-3);
}

#[test]
fn normalize_refuses_a_target_out_of_range_and_changes_nothing() {
    let mut s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let pushes = s.history_pushes();
    assert!(normalize::normalize(&mut s, &Scope::Mix, 5.0, -1.0).is_err());
    assert!(normalize::normalize(&mut s, &Scope::Mix, -14.0, 3.0).is_err());
    assert_eq!(s.history_pushes(), pushes);
}

#[test]
fn a_voiceover_renders_once_and_is_reused() {
    let Ok(voices) = engine::tts::list_voices(&engine::tts::voices_dir()) else {
        eprintln!("skipped: no piper voices installed");
        return;
    };
    if voices.is_empty() || engine::tts::piper_bin().is_err() {
        eprintln!("skipped: piper or its voices are not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("edith-tts-test-{}", std::process::id()));
    let first = engine::tts::render(&dir, "Hello there.", None, None, 1.0, 0.0).expect("render");
    assert!(!first.cached && first.file.exists());
    let again = engine::tts::render(&dir, "Hello there.", None, None, 1.0, 0.0).expect("render again");
    assert!(again.cached && again.file == first.file);
    assert!(engine::probe_media(&first.file).duration_secs.unwrap_or(0.0) > 0.3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_voiceover_refuses_empty_text_and_out_of_range_speed() {
    let dir = std::env::temp_dir();
    assert!(engine::tts::render(&dir, "   ", None, None, 1.0, 0.0).is_err());
    assert!(engine::tts::render(&dir, "hi", None, None, 9.0, 0.0).is_err());
}

#[test]
fn listening_to_the_timeline_finds_the_words_when_speech_is_installed() {
    if speech::available().is_err() {
        eprintln!("skipped: edith-stt or its model is not installed");
        return;
    }
    // The fixture is a tone, not speech: the pipeline must run end to end and say there are no
    // words (or a silent mix), never crash or hang.
    let s = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let job = speech::Job::default();
    let model = speech::installed_models()[0];
    match speech::transcribe(&s, speech::Hear::Mix, model, "en", false, &job) {
        Ok(words) => assert!(words.len() < 20, "a tone is not a speech: {words:?}"),
        Err(e) => assert!(!e.is_empty()),
    }
}
