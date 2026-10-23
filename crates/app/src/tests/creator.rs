//! The creator tools in the window: normalize, ducking, captions, fillers and voiceover.
//! The window has no `TestAppContext`, so each is driven through the session by the very
//! functions the card's buttons call ([`crate::creator`]) and the timeline is read back; the
//! doors (menu rows, palette entries, card controls) are source scans. The speech and piper
//! tests skip where their binaries or models are not installed.

use super::*;
use crate::creator::{self, DuckField};
use engine::project::LaneKind;
use engine::speech::{self, Spoken};

fn session() -> PlaybackSession {
    let s = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    s.set_gain(0.0);
    s
}

fn word(text: &str, a: u32, b: u32) -> Spoken {
    Spoken { text: text.into(), start_f: a, end_f: b, p: 0.9 }
}

#[test]
fn normalize_sets_the_gain_and_one_undo_puts_it_back() {
    let mut s = session();
    let before = s.lane_gain_db(Lane::A1);
    let report = creator::normalize(&mut s, false, -23.0, -1.0).expect("normalize");
    let line = creator::normalize_line(&report, false);
    assert!(line.contains("LUFS") && !line.contains('→') && !line.contains("undo"), "{line}");
    assert!((s.lane_gain_db(Lane::A1) - before).abs() > 0.05 || report.reached, "{line}");
    assert!(s.undo(), "one step");
    assert!((s.lane_gain_db(Lane::A1) - before).abs() < 1e-3, "and one step is all it took");
}

#[test]
fn normalize_each_track_names_the_tracks_it_moved() {
    let mut s = session();
    let report = creator::normalize(&mut s, true, -16.0, -1.0).expect("normalize each");
    assert_eq!(report.lanes.len(), creator::normalizable_lanes(&s).len());
    assert!(creator::normalize_line(&report, true).contains("A1"));
}

#[test]
fn a_target_out_of_range_changes_nothing() {
    let mut s = session();
    let pushes = s.history_pushes();
    assert!(creator::normalize(&mut s, false, 4.0, -1.0).is_err());
    assert_eq!(s.history_pushes(), pushes);
}

#[test]
fn a_duck_is_set_stepped_and_taken_off_each_as_one_undo_step() {
    let mut s = session();
    let music = Lane::A1;
    let voice = s.add_lane(LaneKind::Audio);
    creator::duck_under(&mut s, music, voice).expect("duck");
    let duck = s.lane_duck_list(music);
    assert_eq!(duck.len(), 1);
    assert_eq!(duck[0].under, voice.ord);
    let depth = duck[0].depth_db;
    // A stepper press changes one number and leaves the others.
    creator::duck_step(&mut s, music, voice, DuckField::Depth, 2).expect("deeper");
    let stepped = s.lane_duck_list(music)[0];
    assert!((stepped.depth_db - (depth + 2.0)).abs() < 1e-3, "{depth} -> {}", stepped.depth_db);
    assert_eq!(stepped.attack_ms, duck[0].attack_ms);
    creator::duck_step(&mut s, music, voice, DuckField::Attack, 3).expect("slower attack");
    creator::duck_step(&mut s, music, voice, DuckField::Release, -2).expect("quicker release");
    let d = s.lane_duck_list(music)[0];
    assert!(d.attack_ms > duck[0].attack_ms && d.release_ms < duck[0].release_ms);
    // The ends hold: a press past the range is a press that changes nothing, not an error.
    creator::duck_step(&mut s, music, voice, DuckField::Depth, 100).expect("clamped");
    assert!(s.lane_duck_list(music)[0].depth_db <= engine::duck::MAX_DEPTH_DB);
    creator::duck_off(&mut s, music, voice).expect("off");
    assert!(s.lane_duck_list(music).is_empty());
    assert!(s.undo(), "the off is a step of its own");
    assert_eq!(s.lane_duck_list(music).len(), 1);
}

#[test]
fn a_duck_refuses_itself_a_video_lane_and_a_locked_track() {
    let mut s = session();
    let other = s.add_lane(LaneKind::Audio);
    assert!(creator::duck_under(&mut s, Lane::A1, Lane::A1).is_err());
    assert!(creator::duck_under(&mut s, Lane::A1, Lane::V1).is_err());
    assert!(creator::duck_step(&mut s, Lane::A1, other, DuckField::Depth, 1).is_err(), "no duck to step");
    assert!(creator::duck_off(&mut s, Lane::A1, other).is_err(), "no duck to take off");
    s.set_lane_locked(Lane::A1, true);
    assert!(creator::duck_under(&mut s, Lane::A1, other).is_err());
    assert!(s.lane_duck_list(Lane::A1).is_empty());
}

#[test]
fn fillers_ticked_are_cut_from_the_timeline_in_one_undo_step() {
    let mut s = session();
    let fps = s.meta().frame_rate;
    let before = s.timeline_duration();
    let a = (fps * 0.5) as u32;
    let b = (fps * 2.0) as u32;
    let words = [
        word("so", 0, a - 2),
        word("um", a, a + 6),
        word("this", a + 8, a + 20),
        word("uh", b, b + 6),
        word("ends", b + 8, b + 18),
    ];
    let found = creator::fillers_in(&s, &words);
    assert_eq!(found.len(), 2, "{found:?}");
    // Only the first is ticked: the second stays.
    let cut = creator::cut_fillers(&mut s, &found[..1]).expect("cut");
    assert_eq!(cut, 1);
    let shorter = before - s.timeline_duration();
    assert!(shorter > 4.0 / fps && shorter < 12.0 / fps, "{shorter}");
    assert!(s.undo());
    assert!((s.timeline_duration() - before).abs() < 1e-6, "one undo restores the whole cut");
    // Both together are still one step.
    assert_eq!(creator::cut_fillers(&mut s, &found).expect("cut both"), 2);
    assert!(s.undo());
    assert!((s.timeline_duration() - before).abs() < 1e-6);
}

#[test]
fn captions_land_on_a_new_caption_lane_as_one_undo_step() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("edith-creator-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("film.edith");
    let words = [
        word("Hello", 0, 8),
        word("there", 9, 18),
        word("and", 19, 25),
        word("welcome", 26, 40),
    ];
    let lanes_before = s.subtitle_lanes().len();
    let (n, lane, replaced) = creator::place_captions(&mut s, &words, Some(&project)).expect("captions");
    assert!(n >= 1);
    assert!(!replaced);
    assert_eq!(lane.kind, LaneKind::Subtitle);
    assert_eq!(s.subtitle_lanes().len(), lanes_before + 1);
    assert_eq!(s.sub_lane(lane).len(), n);
    assert!(dir.join("film.captions.srt").exists(), "the track is a file, beside the project");
    assert!(s.undo(), "one step takes the whole track back");
    assert!(s.sub_lane(lane).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn captions_again_replace_the_first_run_in_one_step() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("edith-creator-again-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("again.edith");
    let words = [word("Hello", 0, 8), word("there", 9, 18), word("friend", 19, 30)];
    let (n, lane, _) = creator::place_captions(&mut s, &words, Some(&project)).expect("first");
    let tracks = s.subtitles().len();
    let lanes = s.subtitle_lanes().len();
    let (m, again, replaced) = creator::place_captions(&mut s, &words, Some(&project)).expect("second");
    assert!(replaced);
    assert_eq!(again, lane, "the same lane, not a new one");
    assert_eq!(s.subtitle_lanes().len(), lanes);
    assert_eq!(s.subtitles().len(), tracks, "the old track is dropped, not stacked");
    assert_eq!(s.sub_lane(lane).len(), m);
    assert_eq!(m, n);
    assert!(s.undo(), "one step takes the second run back");
    assert_eq!(s.sub_lane(lane).len(), n);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_words_is_no_captions_and_no_change() {
    let mut s = session();
    let pushes = s.history_pushes();
    assert!(creator::place_captions(&mut s, &[], None).is_err());
    assert_eq!(s.history_pushes(), pushes);
}

#[test]
fn a_voiceover_lands_at_the_playhead_on_a_free_audio_track() {
    let mut s = session();
    let tone = asset("test_tone.wav");
    let at = 1.0;
    let lane = creator::place_voiceover(&mut s, &tone, at).expect("place");
    assert_eq!(lane.kind, LaneKind::Audio);
    let placed: Vec<_> = s.lane_clips(lane).iter().filter(|c| c.source != 0 || c.start > 0).collect();
    assert!(!placed.is_empty(), "a clip is on {}", lane.label());
    let fps = s.meta().frame_rate;
    assert!(placed.iter().any(|c| (c.start as f64 / fps - at).abs() < 1.5 / fps), "at the playhead");
    assert!(s.undo(), "one undo step");
    assert!(s.lane_clips(lane).iter().all(|c| (c.start as f64 / fps - at).abs() > 1.5 / fps));
}

#[test]
fn speaking_text_makes_a_voiceover_the_timeline_takes() {
    let Ok(voices) = engine::tts::list_voices(&engine::tts::voices_dir()) else {
        eprintln!("skipped: no piper voices installed");
        return;
    };
    if voices.is_empty() || engine::tts::piper_bin().is_err() {
        eprintln!("skipped: piper or its voices are not installed");
        return;
    }
    let mut s = session();
    let dir = creator::voiceover_dir(None).join("test");
    let rendered = engine::tts::render(&dir, "Testing the voiceover.", None, None, 1.0, 0.0).expect("render");
    let lane = creator::place_voiceover(&mut s, &rendered.file, 0.5).expect("place");
    assert!(!s.lane_clips(lane).is_empty());
}

#[test]
fn listening_for_captions_runs_end_to_end_when_speech_is_installed() {
    if speech::available().is_err() {
        eprintln!("skipped: edith-stt or its model is not installed");
        return;
    }
    let s = session();
    let hears = creator::hear_choices(&s);
    assert!(hears.contains(&speech::Hear::Mix) && hears.contains(&speech::Hear::Lane(Lane::A1)));
    let heard = speech::prepare(&s, speech::Hear::Mix).expect("the fixture has sound");
    let job = speech::Job::default();
    let model = speech::installed_models()[0];
    // A tone is not speech: whatever comes back, it comes back (and cancel stops it early).
    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(speech::listen(heard, model, "en", false, &job).unwrap_err(), speech::CANCELLED);
}

#[test]
fn the_language_ring_starts_on_auto_and_every_code_is_one_the_engine_takes() {
    assert_eq!(creator::LANGUAGES[0].0, "auto");
    for (code, name) in creator::LANGUAGES {
        assert!(speech::check_lang(code).is_ok(), "{code} ({name})");
    }
}

/// The doors: every tool is a menu row and a palette entry, and every control is on the card.
#[test]
fn every_creator_tool_has_a_menu_row_a_palette_entry_and_a_card_control() {
    let menu = src_text("ui/menubar.rs");
    let source = ui_source();
    for action in [ActionId::Normalize, ActionId::Captions, ActionId::Fillers, ActionId::Voiceover] {
        assert!(ActionId::ALL.contains(&action), "{action:?} is in the palette's list");
        assert!(menu.contains(&format!("Act(ActionId::{action:?})")), "{action:?} has no menu row");
        assert!(!action.label().is_empty());
    }
    for id in [
        "tools-normalize",
        "tools-captions",
        "tools-find-fillers",
        "tools-cut-fillers",
        "tools-fillers-all",
        "tools-fillers-none",
        "tools-speak",
        "tools-cancel",
        "tools-target",
        "tools-ceiling",
        "tools-lang",
        "tools-model",
        "tools-tab-a",
        "tools-tab-b",
        "mix-normalize",
        "mix-duck-key",
        "mix-duck-off",
    ] {
        assert!(source.contains(&format!("\"{id}\"")), "{id} is on no card");
    }
    // Silences already had its door (the Clips menu); it must keep it.
    assert!(menu.contains("Act(ActionId::Silence)"));
}

#[test]
fn the_tools_card_closes_cancels_its_job_and_the_actions_open_it() {
    let actions = src_text("player/actions.rs");
    // Captions starts its run in place (see `captions_starts_listening`).
    for (action, tool) in [
        ("Normalize", "Normalize"),
        ("Fillers", "Fillers"),
        ("Voiceover", "Voiceover"),
    ] {
        assert!(
            actions.contains(&format!("ActionId::{action} => self.open_tools(crate::player::tools::Tool::{tool}, cx)")),
            "{action} does not open the card"
        );
    }
    assert!(fn_body("close_card").contains("close_tools()"));
    assert!(src_text("player/tools.rs").contains("cancel.store(true"));
}

#[test]
fn captions_starts_listening_in_place_and_opens_the_card_behind_it() {
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains("ActionId::Captions => {\n                self.open_tools(crate::player::tools::Tool::Captions, cx);\n                self.start_listening(false, cx);"),
        "Generate captions must start the run, not only open the Tools card"
    );
}

// ---- the Transcript tab ---------------------------------------------------------------

#[test]
fn a_word_cut_ripples_every_lane_in_one_undo_step_and_the_words_follow() {
    let mut s = session();
    let fps = s.meta().frame_rate;
    let before = s.timeline_duration();
    let f = |secs: f64| (secs * fps) as u32;
    let words = [
        word("one", f(0.2), f(0.5)),
        word("two", f(0.6), f(1.0)),
        word("three", f(1.1), f(1.5)),
        word("four", f(1.6), f(2.0)),
    ];
    let end = (before * fps).round() as u32;
    let span = speech::word_span(&words, 1, 2, (0.06 * fps).round() as u32, end).expect("span");
    let lanes = s.lanes();
    let regions = speech::cut_spans(&mut s, &[span], &lanes).expect("cut");
    let shorter = before - s.timeline_duration();
    assert!(shorter > 0.8 && shorter < 1.2, "{shorter}");
    let kept = speech::remap_words(&words, &regions);
    assert_eq!(kept.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["one", "four"]);
    assert!(kept[1].start_f < words[3].start_f, "the later word moved up");
    assert!(s.undo(), "one undo");
    assert!((s.timeline_duration() - before).abs() < 1e-6);
}

#[test]
fn the_transcript_breaks_paragraphs_finds_the_playing_word_and_selects_runs() {
    let mut t = crate::player::transcript::Transcript::new();
    let fps = 25.0;
    t.words = Some(vec![word("Hello", 0, 10), word("there.", 12, 20), word("Later", 100, 110), word("um", 112, 118)]);
    assert_eq!(t.paragraphs(fps), vec![(0, 1), (2, 3)], "a 3 s pause starts a paragraph");
    assert_eq!(t.playing_word(5, fps), Some(0));
    assert_eq!(t.playing_word(14, fps), Some(1));
    assert_eq!(t.playing_word(60, fps), None, "in the silence nothing is spoken");
    assert_eq!(t.selection(), None);
    t.anchor = Some(3);
    t.head = 1;
    assert_eq!(t.selection(), Some((1, 3)), "dragging backwards still selects first to last");
    t.search = "LATER".into();
    assert!(t.matches(2) && !t.matches(0));
}

#[test]
fn a_tool_error_or_a_clock_or_a_model_tip_reads_plainly() {
    let raw = "rendering the mix refused: 7 source(s) offline (moved or deleted): src:0 /tmp/a.mp4, src:1 /tmp/b.mp4; media_relink each one";
    let said = creator::plain_error(raw);
    assert_eq!(said.line, "7 files are offline. Relink them in Media.");
    assert!(said.offline && said.detail.as_deref() == Some(raw), "the whole refusal stays in the tooltip");
    let said = creator::plain_error("the sound is silent: there is nothing to transcribe");
    assert_eq!(said.line, "The sound is silent: there is nothing to transcribe");
    assert!(said.detail.is_none() && !said.offline);
    assert_eq!(creator::clock(0.0), "0:00");
    assert_eq!(creator::clock(75.0), "1:15");
    assert_eq!(creator::clock(3725.0), "1:02:05");
    assert!(creator::model_tip("base").contains("a middle ground"));
    assert_eq!(creator::lufs_text(-20.0), "\u{2212}20.0");
}
