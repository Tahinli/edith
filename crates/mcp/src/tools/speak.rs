//! Speech tools: `voices` lists the local Piper voices, `voiceover` renders text to
//! a WAV beside the project, imports it as a source and, when asked, places it on a
//! lane as one undo step.
//!
//! Piper is an external binary (`EDITH_PIPER`, else `~/.local/share/piper-venv/bin/piper`)
//! fed the text on stdin. A voice is a `<name>.onnx` file with a `<name>.onnx.json`
//! beside it, read from `EDITH_PIPER_VOICES` or `~/.local/share/piper-voices`. Renders
//! are cached by a hash of (text, voice, speed, pause, model) in `voiceover/.renders/`:
//! a repeat reuses the render, and the named WAV beside the project is a copy of it.
//! Piper's phoneme-duration noise is pinned to 0, so one render has one length.

use std::path::Path;

use serde_json::{Value, json};

use engine::project::LaneKind;
pub use engine::tts::{PAUSE_MAX, SPEED_MAX, SPEED_MIN, TEXT_LIMIT, Voice, default_voice, list_voices, voices_dir};

use super::project::dirty;
use crate::parse;
use crate::server::{Entry, Reply, Server, Tool, opt_f64, opt_str};

const VOICES_DOC: &str = "List the local Piper voices: name, language, quality, sample rate and speaker count. \
The default voice is marked *: en_US-ryan-high when installed, else the first en_US voice, else the first one. \
language filters by language code (en, en_US). Voices come from EDITH_PIPER_VOICES, or ~/.local/share/piper-voices.";

const VOICEOVER_DOC: &str = "Speak text with a local Piper voice into <project dir>/voiceover/<name or slug of text>-<hash8>.wav, \
import it as a source (src:N) and report its duration. Needs a project path (project_new or project_open): the WAV goes beside the .edith file even before project_save, and the reply then says the source is unsaved until project_save. \
Renders are cached by (text, voice, speed, pause, model) in voiceover/.renders: the same call reuses the render, and a different name only copies it. Lengths are fixed (piper's duration noise is off); the waveform keeps piper's usual variation. \
place {lane, at} places the voiceover on a lane at a time, as one undo step (the same place op as edit). \
The lane must be an existing audio lane (A1..), and at a time in the project: both are checked before anything is rendered, and a refusal lists the audio lanes or the lane_add that makes one. Text is limited to 5000 characters; piper's stderr is reported on failure. \
speed 0.5..2 sets piper's length_scale to 1/speed, but piper's length does not follow it exactly: measured on en_US-ryan-high, \
speed 0.5 is about 1.7 times as long as speed 1, 0.85 about 1.10 times (not 1.18), 1.5 about 0.78 times and 2 about 0.62 times (not 0.5). \
Judge the pace by ear, not by arithmetic. pause is seconds of silence after each sentence. \
Requires piper (EDITH_PIPER, or ~/.local/share/piper-venv/bin/piper).";

// ---- schemas --------------------------------------------------------------

fn voices_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "language": {"type": "string", "description": "Only voices whose language code starts with this, such as en or en_GB."}
        },
        "additionalProperties": false
    })
}

fn voiceover_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {"type": "string", "maxLength": TEXT_LIMIT, "description": "What to say, up to 5000 characters."},
            "voice": {"type": "string", "description": "A voice name from `voices`. Default en_US-ryan-high, else the first en_US voice."},
            "speed": {"type": "number", "minimum": SPEED_MIN, "maximum": SPEED_MAX, "description": "Pace: 1 is normal, higher is faster (2 is about 1.6 times as fast as 1, not twice). Default 1."},
            "pause": {"type": "number", "minimum": 0, "maximum": PAUSE_MAX, "description": "Seconds of silence after each sentence. Default 0."},
            "place": {
                "type": "object",
                "description": "Place the voiceover on a lane, as one undo step.",
                "properties": {
                    "lane": {"type": "string", "description": "An existing audio lane to place it on, such as A2. lane_add {kind: audio} makes one."},
                    "at": {"type": "string", "description": "Timeline time, such as 5s or 150f."}
                },
                "required": ["lane", "at"],
                "additionalProperties": false
            },
            "name": {"type": "string", "description": "File name stem for the WAV. Default: a slug of the text."}
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "voices", description: VOICES_DOC, schema: voices_schema, run: run_voices },
        Tool { name: "voiceover", description: VOICEOVER_DOC, schema: voiceover_schema, run: run_voiceover },
    ]
}

// ---- voices ---------------------------------------------------------------

fn norm_lang(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

fn language_matches(v: &Voice, want: &str) -> bool {
    v.language.as_deref().is_some_and(|l| norm_lang(l).starts_with(&norm_lang(want)))
}

fn run_voices(_server: &mut Server, args: &Value) -> Reply {
    let dir = voices_dir();
    let voices = match list_voices(&dir) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let filter = opt_str(args, "language");
    let default = default_voice(&voices).map(|v| v.name.clone());
    let shown: Vec<&Voice> = voices
        .iter()
        .filter(|v| filter.is_none_or(|f| language_matches(v, f)))
        .collect();
    if shown.is_empty() {
        return Reply::err(match filter {
            Some(f) => format!("no voice with language `{f}` in {}", dir.display()),
            None => format!("no voices in {}; install a Piper voice (<name>.onnx with its .onnx.json)", dir.display()),
        });
    }
    let lines: Vec<String> = shown
        .iter()
        .map(|v| {
            let mark = if default.as_deref() == Some(v.name.as_str()) { "*" } else { " " };
            format!(
                "{mark} {}  {}  {}  {} Hz  {} speaker(s)  {}",
                v.name,
                v.language.as_deref().unwrap_or("?"),
                v.quality.as_deref().unwrap_or("?"),
                v.sample_rate.map_or_else(|| "?".to_string(), |r| r.to_string()),
                v.speakers,
                v.gender
            )
        })
        .collect();
    let structured: Vec<Value> = shown
        .iter()
        .map(|v| json!({"name": v.name, "language": v.language, "quality": v.quality, "sample_rate": v.sample_rate, "speakers": v.speakers, "gender": v.gender}))
        .collect();
    Reply::ok(format!("{}\n\n* marks the default voice", lines.join("\n")))
        .with(json!({"dir": dir.display().to_string(), "default": default, "voices": structured}))
}

// ---- voiceover ------------------------------------------------------------

/// The voiceover's first clause: the WAV is a source now, and the project file names it
/// only after a save, so a project with unsaved changes says so here.
pub(super) fn imported_words(source: impl std::fmt::Display, unsaved: bool) -> String {
    let mut words = format!("imported as a source (src:{source})");
    if unsaved {
        words.push_str(" (unsaved until project_save)");
    }
    words
}

/// `place {lane, at}`: both are needed, and both are passed to the edit op as written.
fn place_arg(args: &Value) -> Result<Option<(String, String)>, String> {
    let Some(p) = args.get("place").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let lane = p.get("lane").and_then(Value::as_str).ok_or("`place` needs a `lane`, such as A2")?;
    let at = p.get("at").and_then(Value::as_str).ok_or("`place` needs an `at`, such as 5s")?;
    Ok(Some((lane.to_string(), at.to_string())))
}

/// Checks a `place` target before anything is rendered: the lane must be an audio lane
/// the timeline has, and `at` a time in this project. A refusal names the audio lanes
/// that exist, or the `lane_add` that makes the missing one.
pub(super) fn check_place(server: &Server, lane: &str, at: &str) -> Result<(), Reply> {
    let s = server.session_ref()?;
    let l = parse::lane(lane).map_err(Reply::err)?;
    let audio: Vec<String> = s
        .lanes()
        .into_iter()
        .filter(|x| x.kind == LaneKind::Audio)
        .map(parse::lane_name)
        .collect();
    let have = if audio.is_empty() {
        "the timeline has no audio lane".to_string()
    } else {
        format!("the audio lanes are {}", audio.join(", "))
    };
    if l.kind != LaneKind::Audio {
        return Err(Reply::err(format!(
            "voiceover goes on an audio lane (A1..), and {} is not one; {have}",
            parse::lane_name(l)
        )));
    }
    if !s.lanes().contains(&l) {
        return Err(Reply::err(format!(
            "lane {} is not on this timeline; {have}. Add one first with {{\"op\":\"lane_add\",\"kind\":\"audio\"}}",
            parse::lane_name(l)
        )));
    }
    parse::frame(&Value::String(at.to_string()), parse::session_fps(s), parse::session_end(s))
        .map_err(|e| Reply::err(format!("`at`: {e}")))?;
    Ok(())
}

/// The text cut to 40 characters for a ledger label, newlines shown as ` / `.
fn short_text(text: &str) -> String {
    let flat = text.replace('\n', " / ");
    if flat.chars().count() <= 40 {
        return flat;
    }
    let mut cut: String = flat.chars().take(39).collect();
    cut.push('…');
    cut
}

fn run_voiceover(server: &mut Server, args: &Value) -> Reply {
    let text = match opt_str(args, "text") {
        Some(t) if !t.trim().is_empty() => t,
        _ => return Reply::err("missing argument `text`: give the words to say"),
    };
    let chars = text.chars().count();
    if chars > TEXT_LIMIT {
        return Reply::err(format!("text is {chars} characters; the limit is {TEXT_LIMIT}: split it into several voiceovers"));
    }
    let speed = match opt_f64(args, "speed") {
        None => 1.0,
        Some(v) if (SPEED_MIN..=SPEED_MAX).contains(&v) => v,
        Some(v) => return Reply::err(format!("speed {v} is outside {SPEED_MIN}..{SPEED_MAX}")),
    };
    let pause = match opt_f64(args, "pause") {
        None => 0.0,
        Some(v) if (0.0..=PAUSE_MAX).contains(&v) => v,
        Some(v) => return Reply::err(format!("pause {v} is outside 0..{PAUSE_MAX} seconds")),
    };
    let place = match place_arg(args) {
        Ok(p) => p,
        Err(e) => return Reply::err(e),
    };
    let name = opt_str(args, "name");

    let Some(project) = server.path.clone() else {
        return Reply::err("voiceover needs a project path to store its audio next to: open or start one with project_open or project_new {path:\"…\"} first");
    };
    if let Err(r) = server.session_ref() {
        return r;
    }
    if let Some((lane, at)) = &place
        && let Err(r) = check_place(server, lane, at)
    {
        return r;
    }

    let dir = project.parent().unwrap_or(Path::new(".")).join("voiceover");
    let rendered = match engine::tts::render(&dir, text, name, opt_str(args, "voice"), speed, pause) {
        Ok(r) => r,
        Err(e) => return Reply::err(e),
    };
    let (file, cached, voice) = (rendered.file, rendered.cached, rendered.voice);

    let start = match server.session_ref() {
        Ok(s) => s.history_pushes(),
        Err(r) => return r,
    };
    let source = {
        let s = match server.session() {
            Ok(s) => s,
            Err(r) => return r,
        };
        match s.import(&file) {
            Ok(n) => n,
            Err(e) => return Reply::err(format!("{}: refused: {e}", file.display())),
        }
    };
    // Counted by the engine's push count, so an import at the undo cap still lands
    // on the ledger, as media_import does.
    if let Some(s) = server.session.as_ref() {
        let n = s.history_pushes().saturating_sub(start) as usize;
        if n > 0 {
            let label = format!("voiceover \"{}\" → src:{source}", short_text(text));
            server.ledger.push(Entry::new(n, label));
            server.history_lost_on_open = false;
            server.redo_ledger.clear();
            server.trim_ledger();
        }
    }

    let file_name = file.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let secs = engine::probe_media(&file).duration_secs.unwrap_or(0.0);
    let unsaved = dirty(server);
    let mut text_out = format!(
        "{file_name} ({secs:.2}s) {}, voice {}, {}",
        imported_words(source, unsaved),
        voice,
        if cached { "reused" } else { "synthesized" }
    );

    let mut placed = false;
    if let Some((lane, at)) = place {
        let ops = json!({
            "ops": [{"op": "place", "source": source, "lane": lane, "at": at}],
            "label": format!("voiceover {file_name}")
        });
        let r = super::edit::run_edit(server, &ops);
        if r.is_error {
            return Reply::err(format!("{text_out}\n{}", r.text));
        }
        placed = true;
        text_out.push('\n');
        text_out.push_str(&r.text);
    }

    Reply::ok(text_out).with(json!({
        "source": source,
        "path": file.display().to_string(),
        "duration": secs,
        "voice": voice,
        "cached": cached,
        "placed": placed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::tts::*;
    use std::fs;
    use crate::testutil::TestDir;

    /// An empty scratch directory for this test, removed when the guard drops.
    fn tmp(tag: &str) -> TestDir {
        TestDir::new(&format!("speak-{tag}"))
    }

    fn call(srv: &mut Server, name: &str, args: Value) -> Reply {
        let tool = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool is registered");
        (tool.run)(srv, &args)
    }

    fn fake_voice(dir: &Path, name: &str, lang: &str, rate: u64) {
        fs::write(dir.join(format!("{name}.onnx")), b"not a model").unwrap();
        let cfg = json!({
            "audio": {"sample_rate": rate, "quality": "high"},
            "espeak": {"voice": "en-us"},
            "language": {"code": lang},
            "num_speakers": 1
        });
        fs::write(dir.join(format!("{name}.onnx.json")), cfg.to_string()).unwrap();
    }

    #[test]
    fn lists_voices_from_a_dir_with_fake_configs() {
        let dir = tmp("voices");
        fake_voice(&dir, "en_US-ryan-high", "en_US", 22050);
        fake_voice(&dir, "en_GB-alba-medium", "en_GB", 22050);
        fs::write(dir.join("readme.txt"), "not a voice").unwrap();
        fs::write(dir.join("en_US-broken.onnx"), "no json beside it").unwrap();

        let voices = list_voices(&dir).unwrap();
        let names: Vec<&str> = voices.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["en_GB-alba-medium", "en_US-broken", "en_US-ryan-high"]);
        let ryan = voices.iter().find(|v| v.name == "en_US-ryan-high").unwrap();
        assert_eq!(ryan.language.as_deref(), Some("en_US"));
        assert_eq!(ryan.sample_rate, Some(22050));
        assert_eq!(ryan.quality.as_deref(), Some("high"));
        assert_eq!(ryan.speakers, 1);

        assert_eq!(default_voice(&voices).unwrap().name, "en_US-ryan-high");
        assert!(language_matches(ryan, "en"));
        assert!(language_matches(ryan, "en_us"));
        assert!(!language_matches(ryan, "en_GB"));
        assert!(list_voices(&dir.join("missing")).is_err());
    }

    #[test]
    fn a_config_without_language_or_quality_falls_back_to_the_name() {
        // Shaped like tr_TR-eren-medium.onnx.json: no language.code, no audio.quality, no dataset.
        let eren = json!({
            "audio": {"sample_rate": 22050},
            "espeak": {"voice": "tr"},
            "num_speakers": 1,
            "phoneme_type": "espeak"
        });
        let v = parse_voice("tr_TR-eren-medium", Some(&eren));
        assert_eq!(v.language.as_deref(), Some("tr_TR"));
        assert_eq!(v.quality.as_deref(), Some("medium"));
        assert_eq!(v.sample_rate, Some(22050));
        assert_eq!(v.speakers, 1);

        // Fields the config gives win over the name.
        let full = json!({"language": {"code": "en_GB"}, "audio": {"quality": "high", "sample_rate": 22050}});
        let v = parse_voice("en_US-ryan-medium", Some(&full));
        assert_eq!(v.language.as_deref(), Some("en_GB"));
        assert_eq!(v.quality.as_deref(), Some("high"));

        // Empty strings count as missing; a name with more dashes keeps its quality.
        let blank = json!({"language": {"code": "  "}, "audio": {"quality": ""}});
        let v = parse_voice("en_US-libritts_r-medium", Some(&blank));
        assert_eq!(v.language.as_deref(), Some("en_US"));
        assert_eq!(v.quality.as_deref(), Some("medium"));

        // No config at all: the name is the only source, and the speaker count is 1.
        let bare = parse_voice("en_GB-alba-medium", None);
        assert_eq!(bare.language.as_deref(), Some("en_GB"));
        assert_eq!(bare.quality.as_deref(), Some("medium"));
        assert_eq!(bare.sample_rate, None);
        assert_eq!(bare.speakers, 1);

        // A name with no language prefix falls back to espeak's voice.
        let espeak_only = json!({"espeak": {"voice": "tr"}});
        assert_eq!(parse_voice("voice", Some(&espeak_only)).language.as_deref(), Some("tr"));
    }

    #[test]
    fn gender_comes_from_whole_name_tokens() {
        assert_eq!(gender_of("en_US-amy-female-medium"), "female");
        assert_eq!(gender_of("en_US-ryan-male-high"), "male");
        // "female" contains "male": only the whole token counts.
        assert_eq!(gender_of("en_US-female_only-medium"), "female");
        assert_eq!(gender_of("en_US-ryan-high"), "unknown");
        assert_eq!(gender_of("en_US-maleficent-high"), "unknown");
        let v = parse_voice("en_GB-alba-female-medium", None);
        assert_eq!(v.gender, "female");
    }

    #[test]
    fn voiceover_says_when_its_source_is_unsaved() {
        assert_eq!(imported_words(3, false), "imported as a source (src:3)");
        assert_eq!(imported_words(3, true), "imported as a source (src:3) (unsaved until project_save)");
    }

    #[test]
    fn default_voice_falls_back_to_first_en_us_then_first() {
        let dir = tmp("fallback");
        fake_voice(&dir, "en_GB-alba-medium", "en_GB", 22050);
        fake_voice(&dir, "en_US-joe-medium", "en_US", 22050);
        fake_voice(&dir, "en_US-amy-medium", "en_US", 22050);
        let voices = list_voices(&dir).unwrap();
        assert_eq!(default_voice(&voices).unwrap().name, "en_US-amy-medium");

        let only_gb: Vec<Voice> = voices.into_iter().filter(|v| v.name.starts_with("en_GB")).collect();
        assert_eq!(default_voice(&only_gb).unwrap().name, "en_GB-alba-medium");
    }

    #[test]
    fn speed_maps_to_length_scale() {
        assert!((length_scale(1.0) - 1.0).abs() < 1e-9);
        assert!((length_scale(2.0) - 0.5).abs() < 1e-9);
        assert!((length_scale(0.5) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn speed_docs_say_piper_length_is_not_linear_in_speed() {
        // Measured: speed 2 renders about 0.62 times the length of speed 1, not 0.5, so no doc may promise 1/speed.
        assert!(VOICEOVER_DOC.contains("not 0.5"), "the voiceover doc names the measured gap");
        assert!(!VOICEOVER_DOC.contains("twice as fast"), "the voiceover doc does not promise a linear pace");
        assert!(!voiceover_schema().to_string().contains("twice as fast"), "the speed parameter does not promise it either");
    }

    #[test]
    fn names_are_slugs_with_a_hash_of_the_inputs() {
        assert_eq!(slug("Hello, from edith!"), "hello-from-edith");
        assert_eq!(slug("   "), "voiceover");
        assert_eq!(slug("Intro Line!"), "intro-line");
        assert!(slug(&"word ".repeat(30)).len() <= 40);

        let model = Path::new("/voices/en_US-ryan-high.onnx");
        let h = |s: f64, p: f64| cache_hash("Hello from edith.", "en_US-ryan-high", s, p, model);
        let a = wav_name(None, "Hello from edith.", h(1.0, 0.0));
        assert!(a.starts_with("hello-from-edith-"), "{a}");
        assert!(a.ends_with(".wav"));
        let hash = a.trim_end_matches(".wav").rsplit('-').next().unwrap();
        assert_eq!(hash.len(), 8, "{a}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        assert_eq!(a, wav_name(None, "Hello from edith.", h(1.0, 0.0)));
        assert_ne!(a, wav_name(None, "Hello from edith.", h(1.5, 0.0)));
        assert_ne!(a, wav_name(None, "Hello from edith.", h(1.0, 0.5)));
        assert_ne!(a, wav_name(None, "Hello from edith.", cache_hash("Hello from edith.", "en_GB-alba-medium", 1.0, 0.0, Path::new("/voices/en_GB-alba-medium.onnx"))));
        // The name changes the file name only, never the render's key.
        assert_eq!(h(1.0, 0.0), cache_hash("Hello from edith.", "en_US-ryan-high", 1.0, 0.0, model));
        assert!(wav_name(Some("Intro Line!"), "x", h(1.0, 0.0)).starts_with("intro-line-"));
    }

    #[test]
    fn refuses_without_a_project_path() {
        let mut srv = Server::new();
        let r = call(&mut srv, "voiceover", json!({"text": "Hello from edith."}));
        assert!(r.is_error);
        assert!(r.text.contains("project_new"), "{}", r.text);
    }

    #[test]
    fn refuses_long_text_and_bad_speed() {
        let mut srv = Server::new();
        let long = "a".repeat(TEXT_LIMIT + 1);
        let r = call(&mut srv, "voiceover", json!({"text": long}));
        assert!(r.is_error && r.text.contains("5000"), "{}", r.text);
        let r = call(&mut srv, "voiceover", json!({"text": "hi", "speed": 3.0}));
        assert!(r.is_error && r.text.contains("speed"), "{}", r.text);
    }

    fn piper_ready() -> bool {
        if let Err(e) = piper_bin() {
            eprintln!("skipping speak integration: {e}");
            return false;
        }
        match list_voices(&voices_dir()).ok().and_then(|v| default_voice(&v).cloned()) {
            Some(_) => true,
            None => {
                eprintln!("skipping speak integration: no Piper voice in {}", voices_dir().display());
                false
            }
        }
    }

    #[test]
    fn synthesizes_imports_and_caches_a_voiceover() {
        if !piper_ready() {
            return;
        }
        let dir = tmp("synth");
        let mut srv = Server::new();
        let proj = dir.join("hello.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);

        let r = call(&mut srv, "voiceover", json!({"text": "Hello from edith."}));
        assert!(!r.is_error, "voiceover failed: {}", r.text);
        let s = r.structured.as_ref().expect("structured reply");
        assert_eq!(s["source"], json!(0), "{}", r.text);
        let secs = s["duration"].as_f64().unwrap_or(0.0);
        assert!(secs > 0.5, "duration {secs} in {}", r.text);
        assert!(Path::new(s["path"].as_str().unwrap()).is_file());
        assert_eq!(s["cached"], json!(false));
        assert_eq!(s["placed"], json!(false));
        assert!(srv.ledger.is_empty(), "import is not an undo step");

        let again = call(&mut srv, "voiceover", json!({"text": "Hello from edith."}));
        assert!(!again.is_error, "{}", again.text);
        assert_eq!(again.structured.as_ref().unwrap()["cached"], json!(true));
    }

    /// A bad `place` is refused before anything is rendered: no WAV, no source, no ledger step.
    #[test]
    fn place_is_checked_before_anything_is_synthesized() {
        let dir = tmp("place-check");
        let mut srv = Server::new();
        let proj = dir.join("check.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);

        let missing = call(&mut srv, "voiceover", json!({"text": "Hello from edith.", "place": {"lane": "A3", "at": "0s"}}));
        assert!(missing.is_error);
        assert!(missing.text.contains("A3") && missing.text.contains("lane_add"), "{}", missing.text);
        assert!(missing.text.contains("the audio lanes are A1"), "a new project has A1: {}", missing.text);

        let video = call(&mut srv, "voiceover", json!({"text": "Hello from edith.", "place": {"lane": "V1", "at": "0s"}}));
        assert!(video.is_error && video.text.contains("audio lane"), "{}", video.text);

        srv.session.as_mut().unwrap().add_lane(LaneKind::Audio);
        let listed = call(&mut srv, "voiceover", json!({"text": "Hello from edith.", "place": {"lane": "a3", "at": "0s"}}));
        assert!(listed.is_error);
        assert!(listed.text.contains("the audio lanes are A1, A2"), "{}", listed.text);

        let bad_at = call(&mut srv, "voiceover", json!({"text": "Hello from edith.", "place": {"lane": "A1", "at": "soon"}}));
        assert!(bad_at.is_error && bad_at.text.contains("`at`"), "{}", bad_at.text);

        assert!(!dir.join("voiceover").exists(), "no WAV was rendered for a refused place");
        assert!(srv.ledger.is_empty());
        assert!(srv.session.as_ref().unwrap().sources().is_empty(), "nothing was imported");
    }

    #[test]
    fn place_on_a1_is_one_undo_step() {
        if !piper_ready() {
            return;
        }
        let dir = tmp("place");
        let mut srv = Server::new();
        let proj = dir.join("placed.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);

        let r = call(&mut srv, "voiceover", json!({"text": "Hello from edith.", "place": {"lane": "A1", "at": "0s"}}));
        assert!(!r.is_error, "voiceover with place failed: {}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["placed"], json!(true));
        assert_eq!(srv.ledger.len(), 1, "one edit, one ledger entry");
    }
}
