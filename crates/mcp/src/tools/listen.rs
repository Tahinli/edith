//! Listen tools: `transcript` (what is said, in timeline frames), `find_words`
//! (a phrase search over that transcript, with frame spans ready for `cut_regions`
//! or `keep`) and `auto_captions` (the whole transcript as a caption track on S1,
//! one undo step).
//!
//! Speech comes from `edith-stt`, a separate binary built from `tools/edith-stt`.
//! Each audio clip's own source range is decoded, resampled to 16 kHz mono and
//! written to a temp WAV; the binary is given `--offset` = the range start, so
//! its segment times come back in source seconds. Those become timeline frames
//! with the clip's start, in point and speed -- the arithmetic
//! `silence::timeline_regions` uses. Raw segments are cached per source file, model,
//! language and prompt, one entry per range heard: a piece inside a cached range is read
//! from it, so a repeat call, or a cut that only removes material, runs the model again
//! for nothing. `find_words` can cut its matches from the timeline as one undo step.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use engine::export::{ExportSettings, Format};
use engine::project::{Lane, LaneKind, Speed, SpeedRamp};
use engine::speech::{CAPTION_FILLERS, CAPTION_GAP_SECS, CUE_LOW_P, CUE_MIN_SECS, FILLER_PROMPT, MODELS, RATE, RawWord, attach_word_times, caption_cues, capitalise_sentence_starts, check_lang, check_model, home, i16_bytes, norm, parse_raw, pick_model, resample_linear, speaker_changes, stt_bin, wav_bytes};
use engine::{AudioSession, Clip, PlaybackSession, subtitle};
use serde_json::{Value, json};

use crate::parse;
use crate::server::{Entry, Reply, Server, Tool, opt_bool, opt_f64, opt_str, opt_u64};
use crate::tools::edit::{
    caption_drift, close_sliver_holes, has_picture, left_behind, move_titles_for_cuts, shape, side_effects, sliver_note, titles_note,
};
use crate::tools::edit_subs;
use crate::tools::hear::{arg_given, lock_refusal};
use crate::tools::project::expand_home;

const CAP: usize = 6 * 1024;
/// How far a find_words match's span reaches either side when it is checked for sound.
const SILENCE_PAD_SECS: f64 = 0.12;
const CONTEXT_WORDS: usize = 6;
/// Audio read past each requested edge, so a phrase that crosses the edge is heard whole
/// (the model drops a phrase cut off at the window's end). Words are kept only when their
/// middle falls inside the requested span.
const WINDOW_SECS: f64 = 3.0;
/// A segment whose mean token log-prob is below this is marked `?` (low confidence).
const LOW_LOGPROB: f64 = -1.0;
/// Version of the transcript cache key. Bump it when what edith-stt returns for the
/// same audio changes: 2 is the DTW word alignment (`approx:false` segments); 3 is
/// edith-stt's silence filter, so unfiltered results cached under 2 are never reused. 4 is
/// the word-span repair (no zero-length words), segments spanning their first to last word,
/// and the `hint` prompt, which is part of the key. 6 is the decoder's self-attention cache and
/// the loop cut (a looping window ends after its first copy), and the per-range cache layout.
const CACHE_VERSION: u32 = 9;
/// Longest `hint`, in characters.
const HINT_MAX: usize = 400;
/// Words that make a `find_words` query a filler search.
const FILLER_WORDS: &[&str] = &["um", "uh", "er", "ah", "hmm", "like"];
/// Fewer confident words than this in a transcript and auto_captions warns: the audio may hold no
/// speech, and the captions may be the model's guesses over it.
const CAPTION_MIN_CONFIDENT: usize = 3;
/// Phrases Whisper makes up from stock video captions. A segment that contains one (any case) and
/// shows weak audio is flagged for a check; it is kept, so nothing the speaker said is deleted.
const STOCK_PHRASES: &[&str] = &["subscribe", "thanks for watching", "turn on notifications", "subtitles by", "amara.org"];
/// A segment whose span sits this many dB or less above the window's noise floor (edith-stt's
/// `span_db`) is flagged: little or nothing audible under it.
const QUIET_SPAN_DB: f64 = 20.0;
/// edith-stt's `no_speech` at or above this flags a segment the model itself doubted.
const DOUBT_NO_SPEECH: f64 = 0.5;
/// edith-stt's `no_speech` at or above this is weak audio for a stock phrase (see [`Seg::suspect`]).
const STOCK_NO_SPEECH: f64 = 0.3;

const TRANSCRIPT_DOC: &str = "What is said in a clip, a lane, a source (src:N) or the whole mix, as timeline frames. \
One line per segment: `00:12.400–00:15.100 [V1@372] So the first thing we do is…`, the clip it came from in brackets. \
Segment times are reliable. Word times come from alignment (DTW) where edith-stt could align them; a segment whose \
words were only spread evenly across its span is marked approx:true (structuredContent, and the text says so). Cut on \
segments; treat the word spans of an approx segment as a hint. words:true lists the words of each segment. \
Output is capped at ~6 KB: when it is cut, the footer gives `from` for the next page. Each segment carries no_speech and \
avg_logprob when edith-stt gives them; a segment with a `?` in the text is low confidence (avg_logprob below -1.0). \
Silent and hallucinated windows are dropped by edith-stt before they reach this list. \
Whisper usually leaves disfluencies (um, uh, er, hmm, like, you know) out by default, but not always (\"Um, right.\" came through unprompted); fillers:true keeps them, by running with a \
prompt that shows them. That prompt biases the model toward hesitations, so with fillers:true it can mishear content words (\"without a studio\" for \"we're out of studio\"); use the default for captions and reading. hint names words as they should come out (\"Edith, LUFS\"); it helps the transcriber, and a heard word close to a term is respelt to it (the reply names each change). Results are \
cached per source file, model, language and prompt (the hint and any filler prompt); a range an earlier run covered is served from that run, so a repeat, or a call after cuts, is instant. A first run on long audio takes minutes (progress goes to stderr). \
Needs edith-stt built (`cd tools/edith-stt && cargo build --release`, or EDITH_STT_BIN) and the model under \
~/.cache/edith/models/whisper-<size>.";

const FIND_DOC: &str = "Find a phrase (case-insensitive, punctuation ignored) in the transcript of a target. Each match is numbered \
(#0, #1 ...) and gives its timeline span as [\"450f\",\"512f\"] -- ready for cut_regions or keep -- and each word its own span: \
`#0 66f–74f \"we we\" (we 66f–69f, we 70f–74f)`, with six words of context on each side. \
Spans come from word times; a match with approx:true sits in a segment whose words were spread evenly, so pad it or check it with frame before cutting. Runs the same transcript \
as `transcript` (cached). A filler word (um, uh, er, ah, hmm, like, \"you know\") runs with a prompt that makes Whisper keep \
disfluencies, which Whisper usually drops by default (not always), so the search can find them; the reply says so. \
cut:true removes the matches (which: all by default, first, or the #numbers listed) as one undo step, from the lanes: cut:\"lift\" leaves a hole the same length and moves nothing instead, so lanes that are not cut keep their time (a ripple warns when an unlinked sound lane is left out of sync and names this option); by default the target's lane and the lanes of clips linked (grouped) to it, so picture-only B-roll that is not linked to the voice is left alone; a mix or src target takes every lane; lanes:[\"V1\",\"A1\"] names them. Caption lanes ripple with the cut; locked caption lanes stay. A locked lane the cut would move refuses it, dry runs too (lane_flags unlocks it); a mix's default scope skips its locked lanes with a note. A filler query also matches its spellings (uh: ah, eh, er; um: uhm, hum, hmm, mm, erm), and the reply lists which ones matched. keep:\"last\" or \"first\" (with cut, for a query whose words all repeat, such as \"I I\" or \"so so\") cuts every repeat but the last or first one, so one word is kept. A match whose span (padded a little) has no sound on the target is dropped: Whisper writes filler words into silence, and those are not speech, so the reply counts them as dropped silent. Each match \
is padded by pad (default 0.06s, about two frames) on each side, stopped at its neighbouring words so no other speech goes. \
With cut, each edge then moves to the quietest 10 ms within 0.15 s of it (snap:false keeps Whisper's times); the reply gives the average move. \
dry_run:true with cut reports what would be cut (or lifted) and rolls back: nothing changed, no undo step.";

const CAPTIONS_DOC: &str = "Transcribe a target (default lane A1) and write its words as captions: cues are cut at pauses, \
sentence ends, and the max_chars / max_secs limits, saved as an .srt next to the project (<stem>.captions.srt, or a temp \
file when the project is untitled), imported as a subtitle track and placed on a caption lane (default S1) at frame 0. \
One undo step. The default path is <stem>.captions.srt, or <stem>.captions-2.srt, -3 and so on when that file exists or a track on this timeline already holds it, so a second captioning never overwrites the first: a re-run writes a new file too, and the old file stays on disk so undo can bring the old track back; an explicit path that a track already holds is refused. Word times are \
approximate only in segments marked approx, so cue edges there can drift by a few frames. \
hint names words as they should come out (\"Edith, LUFS\"); it helps the transcriber, and a heard word close to a term is respelt to it (the reply names each change). \
A lane that already holds captions takes this run merged in: its cues and the new ones go into one track in time order (the track they came from is removed, one undo step), so each speaker lane can be captioned into one lane (auto_captions {target:\"A2\", lane:\"S1\"} after the A1 run), and burn_captions draws both. \
A run replaces only the captions it hears again on its lane (the same words at overlapping times, so a re-run of the same lane); every other caption that overlaps the run (another speaker's, or a hand edit) is kept and the new cues merge beside it, so a second speaker's run never removes the first's. The reply says how many were replaced and how many overlapping captions differed (kept). replace:true also removes those differing captions and names them. Never refused. Cues over silence on the target are dropped (Whisper writes words into quiet audio, the same check find_words uses), and the reply counts them. A locked caption lane refuses the run. The .srt is a snapshot at caption time; later cue edits live in the project. A mix target splits its cues at speaker changes: each word belongs to the audio lane whose clip plays it (speakers names them; default every audio lane with clips, where clips overlap the one that started last speaks), so no cue spans two speakers. The reply counts the splits.";

// ---- schemas --------------------------------------------------------------

fn hint_prop() -> Value {
    json!({
        "type": "string",
        "description": "Names or jargon as they should come out, e.g. \"Edith, LUFS\". It helps the transcriber, and a heard word (or run of up to three) that is a close spelling or sounds like a term is respelt to it, keeping its timing; the reply names each change. A wrong hint can mislead it, so leave it out when unsure."
    })
}

fn range_prop() -> Value {
    json!({
        "type": "object",
        "description": "Timeline window {from, to}, each a time such as 450f or 15.0s. Only this part of each clip is transcribed.",
        "properties": {"from": {"type": "string"}, "to": {"type": "string"}}
    })
}

fn target_prop() -> Value {
    json!({
        "type": "string",
        "description": "A clip address (V1@372), a lane (A1), src:N, or mix (the whole timeline). Default lane A1."
    })
}

fn model_prop() -> Value {
    json!({
        "type": "string",
        "enum": MODELS,
        "description": "Whisper model size. Default base, or the first installed of base, small, tiny; tiny is fastest and roughest."
    })
}

fn language_prop() -> Value {
    json!({
        "type": "string",
        "description": "Spoken language code such as en or fr, or auto (default) to detect it."
    })
}

fn transcript_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": target_prop(),
            "range": range_prop(),
            "model": model_prop(),
            "language": language_prop(),
            "hint": hint_prop(),
            "words": {"type": "boolean", "description": "List the words of each segment too. Default false."},
            "fillers": {"type": "boolean", "description": "Keep disfluencies (um, uh, er, hmm, like, you know), which Whisper usually drops by default (not always): runs with a prompt that shows them. Default false."},
            "from": {"type": "string", "description": "Page start: the first segment at or after this time (450f, 15.0s). Use the footer's from to get the next page."}
        },
        "additionalProperties": false
    })
}

fn find_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "The phrase to find, e.g. \"brown fox\"."},
            "target": target_prop(),
            "model": model_prop(),
            "language": language_prop(),
            "hint": hint_prop(),
            "cut": {
                "description": "Remove the matches from the target's lane and the lanes of clips linked to it (lanes overrides), as one undo step. true or \"ripple\" closes the time each match leaves, which moves every later clip on the cut lanes. \"lift\" leaves a hole the same length and moves nothing: use it when the cut lanes are not the only ones carrying sound (a host on A1 and a guest on an unlinked A2 would drift apart by the ripple). Each match is padded by pad each side, stopped at the neighbouring words. Default false.",
                "oneOf": [
                    {"type": "boolean"},
                    {"type": "string", "enum": ["ripple", "lift"]}
                ]
            },
            "pad": {"type": "string", "description": "Padding each side of a cut: a time such as 0.06s (default) or 2f; at most 1s."},
            "which": {
                "description": "With cut: all (default), first, or a list of match numbers (#n in the reply), such as [0, 2].",
                "oneOf": [
                    {"type": "string", "enum": ["all", "first"]},
                    {"type": "array", "items": {"type": "integer", "minimum": 0}}
                ]
            },
            "lanes": {
                "type": "array",
                "items": {"type": "string"},
                "description": "With cut: the lanes to cut, e.g. [\"V1\", \"A1\"]. Default the target's lane and the lanes of clips linked to it; a mix or src target defaults to every lane. A locked lane the cut would move refuses it (lane_flags unlocks it); a mix's default scope skips its locked lanes."
            },
            "keep": {
                "type": "string",
                "enum": ["first", "last"],
                "description": "With cut, for a query whose words all repeat (\"I I\", \"so so\"): cut every repeat but the first or the last one, so one word stays. Default: cut every match whole."
            },
            "snap": {"type": "boolean", "description": "With cut: move each cut edge to the quietest 10 ms within 0.15 s of it in the target's sound, so a cut lands in a pause and not on a word's sound. Never closer than 30 ms to the kept word on its side. Default true; false cuts at the word times Whisper gave."},
            "dry_run": {"type": "boolean", "description": "With cut: report what would be cut or lifted, then roll back. Nothing changed and no undo step is added."}
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn captions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": target_prop(),
            "model": model_prop(),
            "language": language_prop(),
            "hint": hint_prop(),
            "lane": {"type": "string", "description": "Caption lane (S1, S2 ...) to place the track on. Default S1. A media lane such as A1 is refused: captions go on a caption lane."},
            "max_chars": {"type": "integer", "minimum": 10, "maximum": 120, "description": "Longest line in characters, two lines per cue. Default 42."},
            "max_secs": {"type": "number", "minimum": 0.5, "maximum": 30, "description": "Longest cue in seconds. Default 6."},
            "speakers": {"type": "array", "items": {"type": "string"}, "description": "Mix target only: the audio lanes that speak, e.g. [\"A1\", \"A2\"], which set where the cues split. Default every audio lane with clips; where clips overlap, the one that started last speaks."},
            "path": {"type": "string", "description": "Where to write the .srt. Default <project stem>.captions.srt beside the project (-2, -3 ... when that name is taken), or a temp file when untitled."},
            "replace": {"type": "boolean", "description": "Default false: a caption on the lane that overlaps this run is replaced only when the run hears the same words again (a re-run); one that overlaps but says something else (another speaker's, or a hand edit) is kept beside the new cues. true also removes those differing captions, and the reply names them. Either way the reply counts what was replaced and what differed; undo brings them back."}
        },
        "additionalProperties": false
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "transcript", description: TRANSCRIPT_DOC, schema: transcript_schema, run: run_transcript },
        Tool { name: "find_words", description: FIND_DOC, schema: find_schema, run: run_find },
        Tool { name: "auto_captions", description: CAPTIONS_DOC, schema: captions_schema, run: run_captions },
    ]
}

// ---- targets --------------------------------------------------------------

/// What a `target` names. `Mix` is the whole timeline as an export renders it.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Clip grew with its effects stack; these are short-lived
enum Want {
    Clip(Lane, Clip),
    Lane(Lane),
    Source(usize),
    Mix,
}

fn session(srv: &Server) -> Result<&PlaybackSession, String> {
    srv.session_ref().map_err(|r| r.text)
}

fn resolve_want(s: &PlaybackSession, raw: &str) -> Result<Want, String> {
    let t = raw.trim();
    if t.eq_ignore_ascii_case("mix") {
        return Ok(Want::Mix);
    }
    if let Some(rest) = t.strip_prefix("src:") {
        let digits: String = rest.trim().chars().take_while(char::is_ascii_digit).collect();
        let n = digits
            .parse::<usize>()
            .map_err(|_| format!("`{t}` is not a source: write src:N"))?;
        if n >= s.sources().len() {
            return Err(format!("no source src:{n}: {}", source_count_note(s.sources().len())));
        }
        return Ok(Want::Source(n));
    }
    if t.contains(['@', '#']) {
        let (lane, idx) = parse::clip(s, t)?;
        if lane.kind == LaneKind::Subtitle {
            return Err(format!("{t} is a caption; transcribe a media clip"));
        }
        let clip = s
            .lane_clips(lane)
            .get(idx)
            .cloned()
            .ok_or_else(|| format!("no media clip at {t}"))?;
        return Ok(Want::Clip(lane, clip));
    }
    let lane = parse::lane(t)?;
    if !s.lanes().contains(&lane) {
        return Err(format!("no lane {t} on this timeline"));
    }
    if lane.kind == LaneKind::Subtitle {
        return Err(format!("{t} is a caption lane; transcribe a media lane"));
    }
    Ok(Want::Lane(lane))
}

/// What `src:N` is missing from: how many sources the project has, and which they are.
fn source_count_note(count: usize) -> String {
    match count {
        0 => "this project has no sources".into(),
        1 => "this project has 1 source (src:0)".into(),
        _ => format!("this project has {count} sources (src:0..src:{})", count - 1),
    }
}

fn want_arg(s: &PlaybackSession, args: &Value) -> Result<Want, String> {
    match args.get("target") {
        None | Some(Value::Null) => {
            if !s.lanes().contains(&Lane::A1) {
                return Err("no lane A1 on this timeline: pass a target (a clip, a lane, src:N or mix)".into());
            }
            Ok(Want::Lane(Lane::A1))
        }
        Some(Value::String(t)) => resolve_want(s, t),
        Some(_) => Err("`target` must be a clip address (V1@372), a lane (A1), src:N or mix".into()),
    }
}

fn want_label(s: &PlaybackSession, want: &Want) -> String {
    match want {
        Want::Clip(l, c) => parse::addr(*l, c.start),
        Want::Lane(l) => parse::lane_name(*l),
        Want::Source(n) => format!(
            "src:{n} {}",
            s.sources()
                .get(*n)
                .and_then(|src| src.path.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        Want::Mix => "mix".into(),
    }
}

/// The clips a target covers, in lane order. A source means its clips on the
/// audio lanes, so a linked video clip is not transcribed twice.
fn clips_of(s: &PlaybackSession, want: Want) -> Vec<(Lane, Clip)> {
    match want {
        Want::Clip(l, c) => vec![(l, c)],
        Want::Lane(l) => s.lane_clips(l).iter().map(|c| (l, c.clone())).collect(),
        Want::Source(n) => s
            .lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Audio)
            .flat_map(|l| {
                s.lane_clips(l)
                    .iter()
                    .filter(move |c| c.source == n)
                    .map(move |c| (l, c.clone()))
                    .collect::<Vec<_>>()
            })
            .collect(),
        Want::Mix => Vec::new(),
    }
}

/// `range {from, to}` in timeline frames, or None when it is left out.
fn range_arg(args: &Value, fps: f64, end: u32) -> Result<Option<(u32, u32)>, String> {
    let Some(r) = args.get("range").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let obj = r
        .as_object()
        .ok_or("`range` must be {from, to}, each a time such as 450f or 15.0s")?;
    // An end of 0 is an empty timeline: every time would clamp to 0 and the window would
    // read as empty. Name the real cause instead.
    if end == 0 {
        return Err("`range` has no timeline to count in: nothing is placed yet. Place a clip first, or transcribe an imported source (src:N), whose range counts in its own file".into());
    }
    let pick = |k: &str, dv: u32| -> Result<u32, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(dv),
            Some(v) => parse::frame(v, fps, end).map(|f| f.min(end)),
        }
    };
    let from = pick("from", 0)?;
    let to = pick("to", end)?;
    if to <= from {
        return Err(format!("range is empty: to ({to}f) must be after from ({from}f)"));
    }
    Ok(Some((from, to)))
}

// ---- time mapping -----------------------------------------------------------

/// Where one piece of audio sits on the timeline.
#[derive(Clone, Copy, Debug)]
enum Map {
    /// A clip: source seconds are read through its in point and speed.
    Clip { start: u32, in_frame: u32, speed: Speed },
    /// A ramped clip ([`Clip::speed_keys`]): source frames are read through the ramp's
    /// integral, which a single [`Speed`] cannot say.
    Ramp { start: u32, in_frame: u32, ramp: SpeedRamp },
    /// The exported mix: its seconds are timeline seconds already.
    Mix,
}

impl Map {
    /// How `clip` plays its source: through its ramp when it has one, else its speed.
    fn of(clip: &Clip) -> Map {
        if clip.speed_keys.is_empty() {
            Map::Clip { start: clip.start, in_frame: clip.in_frame, speed: clip.speed }
        } else {
            Map::Ramp { start: clip.start, in_frame: clip.in_frame, ramp: SpeedRamp::new(&clip.speed_keys, 0) }
        }
    }
}

/// Source seconds (absolute, in the file's own clock) to a timeline frame.
fn timeline_frame(map: Map, src_secs: f64, fps: f64) -> u32 {
    let src = (src_secs * fps).round().max(0.0);
    match map {
        Map::Mix => src as u32,
        Map::Clip { start, in_frame, speed } => {
            let offset = (src - f64::from(in_frame)).max(0.0) as u32;
            start.saturating_add(speed.timeline_at(offset))
        }
        Map::Ramp { start, in_frame, ramp } => {
            let offset = (src - f64::from(in_frame)).max(0.0) as u32;
            start.saturating_add(ramp.timeline_at(offset))
        }
    }
}

/// The source frame range of a clip that falls inside a timeline window, or the
/// whole clip. Returns `None` when the window misses the clip.
fn clip_source_span(clip: &Clip, range: Option<(u32, u32)>) -> Option<(u32, u32)> {
    let Some((r0, r1)) = range else {
        return Some((clip.in_frame, clip.out_frame));
    };
    let tl_end = clip.start.saturating_add(clip.timeline_offset(clip.out_frame - clip.in_frame));
    let a = r0.max(clip.start);
    let b = r1.min(tl_end);
    if a >= b {
        return None;
    }
    let lo = clip.in_frame + clip.source_offset(a - clip.start);
    let hi = (clip.in_frame + clip.source_offset(b - clip.start)).min(clip.out_frame);
    (hi > lo).then_some((lo, hi))
}

// ---- transcript -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Word {
    w: String,
    start_f: u32,
    end_f: u32,
    p: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct Seg {
    start_f: u32,
    end_f: u32,
    text: String,
    /// The clip (or `mix`) the segment was heard in.
    at: String,
    words: Vec<Word>,
    /// True when its word times were spread evenly, not aligned (edith-stt `approx`).
    approx: bool,
    /// edith-stt's `no_speech` probability, when it gave one.
    no_speech: Option<f64>,
    /// edith-stt's mean token log-prob, when it gave one.
    avg_logprob: Option<f64>,
    /// edith-stt's `span_db`: the segment's level over the window's noise floor, when it gave one.
    span_db: Option<f64>,
    /// Foreign-script letters taken out of the text and the words (see [`strip_foreign`]).
    stripped: Option<String>,
}

impl Seg {
    /// Why this segment may be made up, one reason per flag; empty when none applies. Flags
    /// are advice: the segment is kept, and the reply asks for a check against the audio.
    fn suspect(&self) -> Vec<&'static str> {
        let mut why = Vec::new();
        let lower = self.text.to_lowercase();
        // A stock phrase in clear speech is just speech: it is flagged only with a sign that the
        // audio was weak (the model doubted speech, was unsure of its words, or the span is quiet).
        let stock = STOCK_PHRASES.iter().any(|p| lower.contains(p));
        let quiet = self.span_db.is_some_and(|d| d <= QUIET_SPAN_DB);
        let weak = quiet || self.low_confidence() || self.no_speech.is_some_and(|p| p >= STOCK_NO_SPEECH);
        if stock && weak {
            why.push("stock video phrase");
        }
        if !self.text.chars().any(|c| c.is_ascii_alphabetic()) && self.text.chars().any(foreign_letter) {
            why.push("text in a non-Latin script");
        }
        if self.low_confidence() {
            why.push("low confidence");
        }
        if self.no_speech.is_some_and(|p| p >= DOUBT_NO_SPEECH) {
            why.push("the model doubted there was speech");
        }
        if quiet {
            why.push("quiet audio under it");
        }
        why
    }

    /// Low confidence: edith-stt's `avg_logprob` is below [`LOW_LOGPROB`]. Absent
    /// means unknown, which is not marked.
    fn low_confidence(&self) -> bool {
        self.avg_logprob.is_some_and(|lp| lp < LOW_LOGPROB)
    }
}

struct Transcript {
    segs: Vec<Seg>,
    fps: f64,
    /// Stretches of audible sound (frames) that no segment covers: the model heard nothing there.
    gaps: Vec<(u32, u32)>,
    /// Hint respellings made to the words (heard, term, count), see [`correct_hints`].
    corrections: Vec<(String, String, usize)>,
}

/// A stretch of audio this many seconds long, or longer, with no words over it is a gap.
const GAP_MIN_SECS: f64 = 3.0;
/// Blocks louder than this (dBFS, RMS) count as audible.
const GAP_AUDIBLE_DB: f64 = -40.0;

/// The audible stretches of `samples` (16 kHz mono, starting at timeline frame `lo`) that no
/// segment covers, at least `GAP_MIN_SECS` long. Blocks are 0.5 s; a block is covered when a
/// segment overlaps it.
fn coverage_gaps(samples: &[f32], lo: u32, fps: f64, segs: &[Seg]) -> Vec<(u32, u32)> {
    const BLOCK_SECS: f64 = 0.5;
    let block = (BLOCK_SECS * f64::from(RATE)) as usize;
    let mut gaps = Vec::new();
    let mut run: Option<(usize, usize)> = None; // (first block, blocks so far)
    let blocks = samples.len() / block;
    let min_blocks = (GAP_MIN_SECS / BLOCK_SECS).ceil() as usize;
    for i in 0..=blocks {
        let (a, b) = (
            lo + (i as f64 * BLOCK_SECS * fps).round() as u32,
            lo + ((i + 1) as f64 * BLOCK_SECS * fps).round() as u32,
        );
        let quiet_or_end = i == blocks || {
            let s = &samples[i * block..(i + 1) * block];
            let ms = s.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / s.len() as f64;
            10.0 * (ms + 1e-12).log10() <= GAP_AUDIBLE_DB
        };
        let covered = segs.iter().any(|g| g.start_f < b && g.end_f > a);
        if !quiet_or_end && !covered {
            run = Some(run.map_or((i, 1), |(first, n)| (first, n + 1)));
        } else if let Some((first, n)) = run.take()
            && n >= min_blocks
        {
            let start = lo + (first as f64 * BLOCK_SECS * fps).round() as u32;
            let end = lo + ((first + n) as f64 * BLOCK_SECS * fps).round() as u32;
            gaps.push((start, end));
        }
    }
    gaps
}

/// One stretch of audio to hand to edith-stt.
struct Piece {
    label: String,
    path: PathBuf,
    stream: usize,
    /// Source seconds to read, absolute in the file (the requested span plus padding).
    from: f64,
    to: f64,
    /// The requested span in the same seconds: segments whose middle falls outside it
    /// are padding and dropped.
    keep: (f64, f64),
    /// Source seconds this lane does not play from the file (see [`cut_out_source`]). The
    /// padding reads into them, so a word on a cut's seam is heard whole; a word mostly inside
    /// them is not speech on the timeline and is dropped.
    removed: Vec<(f64, f64)>,
    map: Map,
    /// The mix is a temp render, so its cache key is its content.
    mix: bool,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The cache directory name of one source's transcription: file identity (path, mtime, length),
/// the stream, and the model, language and prompt that heard it. `file` is (mtime in ns, length
/// in bytes). The source range is not part of it: each range is an entry inside the directory.
fn cache_key(path: &str, file: (u128, u64), stream: usize, model: &str, lang: &str, hint: &str) -> String {
    let (mtime_ns, len) = file;
    let text = format!("v{CACHE_VERSION}|{path}|{mtime_ns}|{len}|{stream}|{model}|{lang}|{hint}");
    format!("{:016x}", fnv1a(text.as_bytes()))
}

fn source_key(path: &Path, stream: usize, model: &str, lang: &str, hint: &str) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(cache_key(&path.display().to_string(), (mtime, meta.len()), stream, model, lang, hint))
}

fn cache_dir() -> PathBuf {
    home().join(".cache/edith/transcripts")
}

/// A range in whole milliseconds, outward: the start floored, the end ceiled.
fn range_ms(from: f64, to: f64) -> (u64, u64) {
    ((from * 1000.0).floor().max(0.0) as u64, (to * 1000.0).ceil().max(0.0) as u64)
}

/// The file name of one range's entry: `<from_ms>-<to_ms>.jsonl`.
fn range_name(from: f64, to: f64) -> String {
    let (a, b) = range_ms(from, to);
    format!("{a}-{b}.jsonl")
}

/// The raw output cached for exactly `from..to` (seconds) in `dir`. Only an exact range is served:
/// a wider entry would give the piece's segments the window splits of a different decode, so the
/// same request could answer 3 segments one time and 1 the next, depending on what ran before.
fn read_exact(dir: &Path, from: f64, to: f64) -> Option<String> {
    let want = range_name(from, to);
    let found = fs::read_dir(dir).ok()?.filter_map(|e| e.ok()?.file_name().into_string().ok()).any(|name| name == want);
    if !found {
        return None;
    }
    fs::read_to_string(dir.join(want)).ok()
}

/// Stores one range's raw output in `dir`, through a temp file so a half-written entry is
/// never read. A cache that cannot be written only costs the next call its model run.
fn write_entry(dir: &Path, from: f64, to: f64, raw: &str) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let name = range_name(from, to);
    let file = dir.join(&name);
    let part = dir.join(format!("{name}.part"));
    if fs::write(&part, raw).is_ok() && fs::rename(&part, &file).is_err() {
        let _ = fs::remove_file(&part);
    }
}

fn model_arg(args: &Value) -> Result<String, String> {
    match opt_str(args, "model") {
        Some(m) => {
            let m = m.trim().to_ascii_lowercase();
            check_model(&m)?;
            Ok(m)
        }
        None => pick_model(|m| check_model(m).is_ok())
            .map(str::to_string)
            .ok_or_else(|| "no whisper model is installed: install base (or small or tiny) under ~/.cache/edith/models".into()),
    }
}

/// `hint`: names and jargon as they should come out, handed to edith-stt as its prompt.
fn hint_arg(args: &Value) -> Result<String, String> {
    let h = opt_str(args, "hint").unwrap_or("").trim().to_string();
    if h.chars().count() > HINT_MAX {
        return Err(format!("`hint` is longer than {HINT_MAX} characters"));
    }
    Ok(h)
}

/// The prompt edith-stt gets: `hint` alone, or with the filler prompt in front when
/// disfluencies are wanted. The prompt is part of the cache key, so the two never mix.
fn with_fillers(hint: &str, fillers: bool) -> String {
    match (fillers, hint.is_empty()) {
        (false, _) => hint.to_string(),
        (true, true) => FILLER_PROMPT.to_string(),
        (true, false) => format!("{FILLER_PROMPT} {hint}"),
    }
}

/// Spellings Whisper gives one filler sound. A query word matches any word of its group, so
/// "uh" finds "Ah," and "um" finds "Hmm".
const FILLER_GROUPS: &[&[&str]] = &[
    &["uh", "ah", "eh", "er", "uhh", "err"],
    &["um", "uhm", "umm", "hum", "hmm", "hm", "mm", "erm"],
];

/// True when a normalised query asks for a filler: one of [`FILLER_WORDS`], a word of a
/// [`FILLER_GROUPS`] group, or "you know".
fn is_filler_query(words: &[String]) -> bool {
    words.iter().any(|w| FILLER_WORDS.contains(&w.as_str()) || FILLER_GROUPS.iter().any(|g| g.contains(&w.as_str())))
        || words.windows(2).any(|p| p[0] == "you" && p[1] == "know")
}

/// The filler prompt as normalised words: the sequence Whisper echoes back over speech it did not hear.
fn prompt_words() -> Vec<String> {
    FILLER_PROMPT.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect()
}

/// Whether match `i` (of `k` words) is the filler prompt echoed back: its phrase and the words
/// around it, at least four in all, run in the prompt in the same order. Real speech rarely repeats
/// four prompt words in a row, so such a match quotes words that are not in the audio.
fn prompt_echo(norms: &[String], i: usize, k: usize, prompt: &[String]) -> bool {
    let lo = i.saturating_sub(CONTEXT_WORDS);
    let hi = (i + k + CONTEXT_WORDS).min(norms.len());
    (lo..=i).any(|a| ((i + k)..=hi).any(|b| b - a >= 4 && prompt.windows(b - a).any(|w| w == &norms[a..b])))
}

/// Query words that are fillers only sometimes and real words the rest of the time. Only these get
/// the "looks like a real word" note: "um" or "uh" with no pause around them is still a filler.
const REAL_WORD_FILLERS: &[&str] = &["like", "so"];

/// True when a normalised query names a word that may be a real word rather than a filler.
fn may_be_real_word(words: &[String]) -> bool {
    words.iter().any(|w| REAL_WORD_FILLERS.contains(&w.as_str()))
}

/// Whether a match has no pause either side: the gaps (frames) before and after it are both under
/// 0.1 s. A missing neighbour (the start or end of the transcript) is passed as `u32::MAX`, a pause.
fn mid_phrase(gap_before: u32, gap_after: u32, fps: f64) -> bool {
    let limit = 0.1 * fps;
    f64::from(gap_before) < limit && f64::from(gap_after) < limit
}

/// The reply line for a filler query whose match is mid-phrase: the match with the words after it.
fn real_word_note(n: usize, quote: &str) -> String {
    format!("match #{n} '{quote}' looks like a real word (no pause around it); use which to skip it")
}

/// Whether a normalised transcript word is what a normalised query word asks for: the same
/// word, or a spelling of the same filler sound.
fn same_word(query: &str, word: &str) -> bool {
    query == word || FILLER_GROUPS.iter().any(|g| g.contains(&query) && g.contains(&word))
}

/// Whisper's stock phrases over silence. A segment that is one of them, and that edith-stt
/// heard as no speech (or low confidence), is a hallucination and is dropped.
const SILENCE_HALLUCINATIONS: &[&str] = &["thank you", "thank you for watching", "thanks for watching", "you"];

/// True for a segment that is a known silence hallucination. `no_speech` is edith-stt's
/// probability there is no speech; `avg_logprob` its mean token log-prob. Without either the
/// segment is kept: nothing says it was silent.
fn silence_hallucination(text: &str, no_speech: Option<f64>, avg_logprob: Option<f64>) -> bool {
    let norm_text = text.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    SILENCE_HALLUCINATIONS.contains(&norm_text.as_str())
        && (no_speech.is_some_and(|p| p >= 0.5) || avg_logprob.is_some_and(|lp| lp < LOW_LOGPROB))
}

/// Lanes named by a `lanes` argument: each must be on the timeline. `None` when absent.
fn named_lanes(s: &PlaybackSession, args: &Value) -> Result<Option<Vec<Lane>>, String> {
    match args.get("lanes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|x| {
                let name = x
                    .as_str()
                    .ok_or_else(|| "`lanes` lists lane names such as [\"V1\", \"A1\"]".to_string())?;
                let lane = parse::lane(name)?;
                if !s.lanes().contains(&lane) {
                    return Err(format!("no lane {name} on this timeline"));
                }
                Ok(lane)
            })
            .collect::<Result<Vec<Lane>, String>>()
            .map(Some),
        Some(_) => Err("`lanes` must be a non-empty list of lane names such as [\"V1\", \"A1\"]".into()),
    }
}

/// The lanes a cut on `lane` takes by default: `lane` itself, and every other lane holding a
/// clip linked (grouped) to one of `clips`. Picture-only B-roll that is not linked to the
/// voice stays where it is. Also used by cut_silence.
pub(crate) fn linked_lanes(s: &PlaybackSession, lane: Lane, clips: &[Clip]) -> Vec<Lane> {
    let links: Vec<u32> = clips.iter().filter_map(|c| c.link).collect();
    s.lanes()
        .into_iter()
        .filter(|&l| {
            l == lane
                || (!links.is_empty() && s.lane_clips(l).iter().any(|c| c.link.is_some_and(|k| links.contains(&k))))
        })
        .collect()
}

/// The lanes a find_words cut takes: `lanes` as named, else the target's linked lanes. A mix or
/// src target has no one lane, so it takes every lane.
fn cut_scope(s: &PlaybackSession, want: &Want, args: &Value) -> Result<Vec<Lane>, String> {
    if let Some(mut v) = named_lanes(s, args)? {
        // The matches sit on the target's lane, so a named scope that leaves it out still cuts it
        // (as cut_silence's ripple scope does); the reply says so (see `target_added`).
        if let Some(t) = target_lane(want).filter(|t| !v.contains(t)) {
            v.push(t);
        }
        return Ok(v);
    }
    Ok(match want {
        Want::Lane(l) => linked_lanes(s, *l, s.lane_clips(*l)),
        Want::Clip(l, c) => linked_lanes(s, *l, std::slice::from_ref(c)),
        Want::Source(_) | Want::Mix => s.lanes(),
    })
}

/// The lane a find_words target sits on, when it has one (a lane or a clip; not a mix or src).
fn target_lane(want: &Want) -> Option<Lane> {
    match want {
        Want::Lane(l) | Want::Clip(l, _) => Some(*l),
        Want::Source(_) | Want::Mix => None,
    }
}

/// The reply note for a named `lanes` list that left the target's lane out, so cut_scope added it.
fn target_added(want: &Want, args: &Value) -> Option<String> {
    let t = target_lane(want)?;
    let named: Vec<Lane> = args.get("lanes")?.as_array()?.iter().filter_map(|x| x.as_str().and_then(|n| parse::lane(n).ok())).collect();
    (!named.is_empty() && !named.contains(&t))
        .then(|| format!("; {} is the target's lane, so it is cut too (added to lanes)", parse::lane_name(t)))
}

/// The locks a find_words cut meets, as cut_silence has them: a locked lane the cut would move
/// refuses it with the lane_flags way out. Only a mix's default scope (every lane) skips its locked
/// lanes instead, and returns them for the reply's note.
fn cut_locks(s: &PlaybackSession, want: &Want, args: &Value, scope: &mut Vec<Lane>) -> Result<Vec<Lane>, String> {
    let mix_default = matches!(want, Want::Mix) && !arg_given(args, "lanes");
    let locked: Vec<Lane> = scope.iter().copied().filter(|&l| s.lane_locked(l)).collect();
    let mut skipped = Vec::new();
    if mix_default {
        scope.retain(|l| !locked.contains(l));
        skipped = locked;
    } else if let Some(&l) = locked.first() {
        return Err(format!("find_words refused: {}", lock_refusal(l)));
    }
    if scope.is_empty() {
        return Err("find_words refused: every lane it would cut is locked; unlock one with lane_flags".into());
    }
    Ok(skipped)
}

/// Which repeat a `keep` cut leaves: the first or the last word of a repeated run.
#[derive(Clone, Copy, PartialEq)]
enum Keep {
    First,
    Last,
}

/// The span a `keep` cut takes for one match of a repeated run, whose first and last words are
/// `first` and `last` (start, end frames). The kept word is left whole: "last" cuts up to the
/// last word's start, "first" cuts from the first word's end. Padding runs as in [`cut_span`],
/// stopped at the neighbouring words.
fn keep_span(
    keep: Keep,
    first: (u32, u32),
    last: (u32, u32),
    pad: u32,
    prev_end: u32,
    next_start: u32,
    timeline_end: u32,
) -> (u32, u32) {
    match keep {
        Keep::Last => {
            let a = first.0.saturating_sub(pad).max(prev_end.min(first.0));
            (a, last.0)
        }
        Keep::First => {
            let b = last.1.saturating_add(pad).min(next_start.max(last.1)).min(timeline_end.max(last.1));
            (first.1, b)
        }
    }
}

/// `cut`: true or false, or a mode. "ripple" (what true means) closes the time each match leaves;
/// "lift" leaves a hole the same length and moves nothing, so other lanes keep their time. Returns
/// (cut, lift).
fn cut_arg(args: &Value) -> Result<(bool, bool), String> {
    match args.get("cut") {
        None | Some(Value::Null) => Ok((false, false)),
        Some(Value::Bool(b)) => Ok((*b, false)),
        Some(Value::String(m)) if m == "ripple" => Ok((true, false)),
        Some(Value::String(m)) if m == "lift" => Ok((true, true)),
        Some(_) => Err("`cut` is true, false, \"ripple\" or \"lift\"".into()),
    }
}

/// `keep`: "first" or "last", or absent.
fn keep_arg(args: &Value) -> Result<Option<Keep>, String> {
    match args.get("keep") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(k)) => match k.trim().to_ascii_lowercase().as_str() {
            "first" => Ok(Some(Keep::First)),
            "last" => Ok(Some(Keep::Last)),
            _ => Err(format!("`keep` is first or last; got `{k}`")),
        },
        Some(_) => Err("`keep` is first or last".into()),
    }
}

fn language_arg(args: &Value) -> Result<String, String> {
    let l = opt_str(args, "language").unwrap_or("auto").trim().to_ascii_lowercase();
    check_lang(&l)?;
    Ok(l)
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Runs edith-stt on one WAV (see [`engine::speech::run_stt`]); this server has no progress line
/// and nobody to cancel it.
fn run_stt(bin: &Path, wav: &Path, model: &str, lang: &str, hint: &str, offset: f64, label: &str) -> Result<String, String> {
    engine::speech::run_stt(bin, wav, model, lang, hint, offset, label, &engine::speech::Job::default())
}

fn temp_path(tag: &str, ext: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("edith-listen-{tag}-{}-{nanos}.{ext}", std::process::id()))
}

/// The error for a failed write to a temp file. It names the file and says where temp files go:
/// the folder comes from TMPDIR, so a full disk there is fixed by freeing it or pointing TMPDIR elsewhere.
fn temp_write_error(path: &Path, doing: &str, e: impl std::fmt::Display) -> String {
    format!(
        "{doing} {}: {e}. Temp files go in the folder TMPDIR picks ({}); free space there or point TMPDIR at a roomier folder",
        path.display(),
        std::env::temp_dir().display()
    )
}

/// The piece's audio as mono 16 kHz samples, or `None` for a file with no audio.
fn decode_16k(path: &Path, stream: usize, from: f64, to: f64) -> Result<Option<Vec<f32>>, String> {
    let sources = [(path.to_path_buf(), stream)];
    let Some((meta, rx)) = AudioSession::open_multi_streams(&sources, &[(Some(0), from, to)])
        .map_err(|e| format!("reading the sound of {}: {e}", path.display()))?
    else {
        return Ok(None);
    };
    let ch = usize::from(meta.channels.max(1));
    let mut mono = Vec::new();
    for chunk in rx {
        for frame in chunk.samples.chunks(ch) {
            mono.push(frame.iter().sum::<f32>() / frame.len() as f32);
        }
    }
    Ok(Some(resample_linear(&mono, f64::from(meta.sample_rate), f64::from(RATE))))
}

/// The whole timeline rendered the way an export writes it, to `wav`.
fn render_mix(s: &PlaybackSession, wav: &Path) -> Result<(), String> {
    let settings = ExportSettings { format: Format::Wav, ..ExportSettings::default() };
    let handle = s.export_to_with(wav, &settings);
    let outcome = loop {
        if let Some(r) = handle.result() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    outcome.map_err(|e| temp_write_error(wav, "rendering the mix to", e))
}

/// Raw segments of one piece, on the timeline. A piece reads padding around its span, and
/// one raw segment can run across a cut (a sentence over the gap between two clips of the
/// same source). A segment therefore belongs to the span it overlaps, not to the span its
/// middle falls in: a midpoint rule dropped such a segment from every clip it crossed. Its
/// words outside the span are dropped and the text is rebuilt from the words that stay.
fn raw_to_segs(raw: &str, p: &Piece, fps: f64) -> Result<Vec<Seg>, String> {
    let tl = |secs: f64| timeline_frame(p.map, secs, fps);
    let (keep_lo, keep_hi) = p.keep;
    let mut segs = Vec::new();
    for r in parse_raw(raw)? {
        if r.text.is_empty() || r.end <= keep_lo || r.start >= keep_hi {
            continue;
        }
        // Whisper's sign-off over silence: not speech, so not a word anyone said.
        if silence_hallucination(&r.text, r.no_speech, r.avg_logprob) {
            continue;
        }
        // A word with no duration means the alignment collapsed; its times are not reliable.
        let collapsed = r.words.iter().any(|w| w.end <= w.start);
        let timed: Vec<&RawWord> = r.words.iter().filter(|w| !w.w.is_empty()).collect();
        // A phrase with no word times is heard whole: one mostly inside cut-out audio goes with it.
        if timed.is_empty() && mostly_removed(&p.removed, r.start, r.end) {
            continue;
        }
        // A word is kept when its middle is in the span and it is not mostly in cut-out audio
        // (a word on a cut's seam can be heard in the audio the cut took out).
        let inside = |w: &RawWord| {
            let mid = (w.start + w.end) / 2.0;
            mid >= keep_lo && mid <= keep_hi && !mostly_removed(&p.removed, w.start, w.end)
        };
        let kept: Vec<&RawWord> = timed.iter().copied().filter(|w| inside(w)).collect();
        if !timed.is_empty() && kept.is_empty() {
            continue;
        }
        let text = if kept.len() < timed.len() {
            kept.iter().map(|w| w.w.as_str()).collect::<Vec<_>>().join(" ")
        } else {
            r.text
        };
        // Foreign-script letters inside a Latin segment are Whisper's garbage: they come out, and
        // the reply says so. A segment with no Latin letters keeps its text (flagged instead).
        let latin = text.chars().any(|c| c.is_ascii_alphabetic());
        let (text, text_run) = strip_foreign(&text, latin);
        let mut runs: Vec<String> = text_run.into_iter().collect();
        // A phrase that crosses the span's edge starts or ends at the words it keeps, not at the
        // edge (a cut at the edge would split a word). Otherwise it keeps the model's own times.
        let (lo_s, hi_s) = kept.iter().fold((f64::MAX, f64::MIN), |(a, b), w| (a.min(w.start), b.max(w.end)));
        let start_s = if r.start < keep_lo && !kept.is_empty() { lo_s } else { r.start.max(keep_lo) };
        let end_s = if r.end > keep_hi && !kept.is_empty() { hi_s } else { r.end.min(keep_hi) };
        let start_f = tl(start_s);
        let end_f = tl(end_s).max(start_f);
        let words = kept
            .iter()
            .filter_map(|w| {
                let ws = tl(w.start);
                let (clean, run) = strip_foreign(&w.w, latin);
                runs.extend(run);
                (!clean.is_empty()).then(|| Word { w: clean, start_f: ws, end_f: tl(w.end).max(ws), p: w.p })
            })
            .collect();
        let mut stripped_runs: Vec<String> = Vec::new();
        for run in runs {
            if !stripped_runs.contains(&run) {
                stripped_runs.push(run);
            }
        }
        segs.push(Seg {
            start_f,
            end_f,
            text,
            at: p.label.clone(),
            words,
            approx: r.approx || collapsed,
            no_speech: r.no_speech,
            avg_logprob: r.avg_logprob,
            span_db: r.span_db,
            stripped: (!stripped_runs.is_empty()).then(|| stripped_runs.join(", ")),
        });
    }
    Ok(segs)
}

/// True when most of `start`..`end` (seconds) lies in one of the `removed` regions. A span with
/// no length counts by its start.
fn mostly_removed(removed: &[(f64, f64)], start: f64, end: f64) -> bool {
    let len = end - start;
    if len <= 0.0 {
        return removed.iter().any(|&(a, b)| a <= start && start < b);
    }
    let inside: f64 = removed.iter().map(|&(a, b)| (end.min(b) - start.max(a)).max(0.0)).sum();
    inside * 2.0 > len
}

/// The seconds of source `source` that `lane` does not play: the audio before its first clip of
/// that source, the stretch between two of them (a cut took it out) and everything after the
/// last. Its clips are what the timeline plays, so a word heard only here is not on the timeline.
fn cut_out_source(s: &PlaybackSession, lane: Lane, source: usize, fps: f64) -> Vec<(f64, f64)> {
    let played: Vec<(u32, u32)> = s.lane_clips(lane).iter().filter(|c| c.source == source).map(|c| (c.in_frame, c.out_frame)).collect();
    cut_out(played, fps)
}

/// [`cut_out_source`] for the source frame ranges `played` (`[in, out)`) of one source.
fn cut_out(mut played: Vec<(u32, u32)>, fps: f64) -> Vec<(f64, f64)> {
    played.sort_unstable();
    let mut out = Vec::new();
    let mut at = 0;
    for (a, b) in played {
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    out.push((at, u32::MAX));
    out.into_iter().map(|(a, b)| (f64::from(a) / fps, f64::from(b) / fps)).collect()
}

/// True for a letter of a script English does not use: Cyrillic, Arabic, Indic and Thai, CJK,
/// kana, Hangul and the CJK compatibility block. Accented Latin letters are not foreign.
fn foreign_letter(c: char) -> bool {
    c.is_alphabetic() && matches!(c as u32, 0x0400..=0x0EFF | 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)
}

/// Takes the foreign-script letters out of `text` when the text is Latin (`latin`): the runs
/// Whisper makes up inside English. Returns the text without them, and the runs removed (joined
/// by ", "), or None when nothing was removed. Punctuation and spaces stay; whitespace is
/// collapsed only when something was removed.
fn strip_foreign(text: &str, latin: bool) -> (String, Option<String>) {
    if !latin || !text.chars().any(foreign_letter) {
        return (text.to_string(), None);
    }
    let mut runs: Vec<String> = Vec::new();
    let mut run = String::new();
    let mut kept = String::new();
    for c in text.chars() {
        if foreign_letter(c) {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
        kept.push(c);
    }
    if !run.is_empty() {
        runs.push(run);
    }
    (kept.split_whitespace().collect::<Vec<_>>().join(" "), Some(runs.join(", ")))
}

/// The reply lines for flagged and cleaned segments, each on its own line; empty when there are
/// none. `segs` are the segments the reply shows.
fn review_text(segs: &[&Seg], fps: f64) -> String {
    let mut flagged = Vec::new();
    let mut cleaned = Vec::new();
    for g in segs {
        let at = format!("{} {:.2}s", g.at, f64::from(g.start_f) / fps);
        let why = g.suspect();
        if !why.is_empty() {
            let said: String = g.text.chars().take(48).collect();
            let more = if g.text.chars().count() > 48 { "…" } else { "" };
            flagged.push(format!("{at} '{said}{more}' ({})", why.join(", ")));
        }
        if let Some(run) = &g.stripped {
            cleaned.push(format!("{at} '{run}'"));
        }
    }
    let mut out = String::new();
    if !flagged.is_empty() {
        out.push_str(&format!(
            "\ncheck {} segment(s) that may be made up (kept, not removed): {}",
            flagged.len(),
            flagged.join("; ")
        ));
    }
    if !cleaned.is_empty() {
        out.push_str(&format!(
            "\nremoved foreign-script text from {} segment(s), the rest kept: {}",
            cleaned.len(),
            cleaned.join("; ")
        ));
    }
    out
}

/// Transcribes one piece, through the cache when it can: a cache entry for exactly this range
/// serves it, and a new run is stored under that range. The output depends only on the piece.
fn run_piece(bin: &Path, p: &Piece, model: &str, lang: &str, hint: &str, fps: f64) -> Result<Vec<Seg>, String> {
    let known = if p.mix { None } else { source_key(&p.path, p.stream, model, lang, hint) };
    // An entry with no segments is never served: it is a silent answer, and audible audio gets
    // heard again (below) instead of trusting it.
    let cached = |dir: &Path| read_exact(dir, p.from, p.to).filter(|raw| has_segments(raw));
    if let Some(raw) = known.as_deref().and_then(|k| cached(&cache_dir().join(k))) {
        return raw_to_segs(&raw, p, fps);
    }
    let Some(samples) = decode_16k(&p.path, p.stream, p.from, p.to)? else {
        return Ok(Vec::new());
    };
    // Digital silence has nothing to hear: no model run, nothing cached.
    if is_silent(&samples) {
        return Ok(Vec::new());
    }
    let key = known.unwrap_or_else(|| {
        let bytes = i16_bytes(&samples);
        cache_key(&format!("content:{:016x}", fnv1a(&bytes)), (0, bytes.len() as u64), 0, model, lang, hint)
    });
    let dir = cache_dir().join(&key);
    if let Some(raw) = cached(&dir) {
        return raw_to_segs(&raw, p, fps);
    }
    eprintln!("transcript {}: {:.1}s to {:.1}s with {model}", p.label, p.from, p.to);
    let wav = TempFile(temp_path("stt", "wav"));
    // `wav` is removed when this function returns, on success and on every error path.
    fs::write(&wav.0, wav_bytes(&samples)).map_err(|e| temp_write_error(&wav.0, "writing", e))?;
    let raw = run_stt(bin, &wav.0, model, lang, hint, p.from, &p.label)?;
    let segs = parse_raw(&raw)?.len();
    if segs > 0 {
        write_entry(&dir, p.from, p.to, &raw);
    }
    // A segment is one phrase the model timed (start and end), padding included; words are counted inside them.
    eprintln!("transcript {}: done, {segs} segment(s) from the model (phrases with their own start and end, padding included)", p.label);
    raw_to_segs(&raw, p, fps)
}

/// True when a cached raw output holds at least one segment.
fn has_segments(raw: &str) -> bool {
    parse_raw(raw).is_ok_and(|v| !v.is_empty())
}

/// True for audio with no sound in it at all (below -80 dBFS).
fn is_silent(samples: &[f32]) -> bool {
    samples.iter().all(|s| s.abs() < 1e-4)
}

/// Drops the copy that a clip's padding or a window edge leaves behind. Segments are in time order;
/// a segment (or its leading words) that starts before the previous kept segment ends and repeats
/// that segment's last words is the same speech heard twice, so those words go.
fn drop_overlap_repeats(segs: Vec<Seg>) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::with_capacity(segs.len());
    for mut seg in segs {
        if let Some(prev) = out.last() {
            let tail: Vec<String> = prev.words.iter().map(|w| norm(&w.w)).collect();
            let lead: Vec<String> = seg.words.iter().map(|w| norm(&w.w)).collect();
            // The longest run of leading words that overlap the previous segment and match its last words.
            let repeat = (1..=lead.len().min(tail.len()))
                .filter(|&n| seg.words[n - 1].start_f < prev.end_f && lead[..n] == tail[tail.len() - n..])
                .max()
                .unwrap_or(0);
            if repeat > 0 {
                seg.words.drain(..repeat);
                if seg.words.is_empty() {
                    continue;
                }
                seg.start_f = seg.words[0].start_f;
                seg.text = seg.words.iter().map(|w| w.w.as_str()).collect::<Vec<_>>().join(" ");
            }
        }
        out.push(seg);
    }
    out
}

/// The transcript of a target over an optional timeline window.
/// The refusal for an offline source the target reads. The mix is rendered from every source, so an
/// offline one refuses it by name; a lane, clip or source reads its own files, so an offline one is
/// named the same way. Checked before the model is resolved (`model_arg`), so a file that is gone is
/// named even on a machine with no whisper model installed.
fn check_sources_online(s: &PlaybackSession, want: &Want) -> Result<(), String> {
    if let Want::Mix = want
        && let Some(why) = crate::tools::hear::offline_refusal(s)
    {
        return Err(why);
    }
    let used: Vec<usize> = match want {
        Want::Clip(_, c) => vec![c.source],
        Want::Lane(l) => s.lane_clips(*l).iter().map(|c| c.source).collect(),
        Want::Source(n) => vec![*n],
        Want::Mix => Vec::new(),
    };
    match crate::tools::hear::offline_refusal_for(s, want_label(s, want), &used) {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// The transcript of source `n` in its own frames: what `transcribe` does for a source with no
/// clip, whether or not the timeline plays it.
fn transcribe_source(
    s: &PlaybackSession,
    n: usize,
    range: Option<(u32, u32)>,
    model: &str,
    lang: &str,
    hint: &str,
    fps: f64,
) -> Result<Transcript, String> {
    let bin = stt_bin()?;
    check_model(model)?;
    let mut segs = source_segs(s, n, range, &bin, model, lang, hint, fps)?;
    segs.sort_by_key(|g| (g.start_f, g.end_f));
    let mut segs = drop_overlap_repeats(segs);
    let corrections = correct_hints(&mut segs, hint);
    Ok(Transcript { segs, fps, gaps: Vec::new(), corrections })
}

/// The reply line that says a source's times are its own, and which clips play it, each at its
/// source range. Names no clip when none plays the source.
fn source_clips_note(n: usize, clips: &[(Lane, Clip)], fps: f64) -> String {
    if clips.is_empty() {
        return format!("\nsrc:{n} times are the file's own clock; no clip on the timeline plays it");
    }
    let used: Vec<String> = clips
        .iter()
        .map(|(lane, c)| format!("{} plays {}–{}", parse::addr(*lane, c.start), stamp(c.in_frame, fps), stamp(c.out_frame, fps)))
        .collect();
    format!("\nsrc:{n} times are the file's own clock; used by {}", used.join(", "))
}

fn transcribe(s: &PlaybackSession, want: Want, range: Option<(u32, u32)>, model: &str, lang: &str, hint: &str) -> Result<Transcript, String> {
    let fps = parse::session_fps(s);
    let bin = stt_bin()?;
    check_model(model)?;
    let mut segs = Vec::new();
    let mut gaps = Vec::new();
    let pad = (WINDOW_SECS * fps).round() as u32;
    if let Want::Mix = want {
        let end = parse::session_end(s);
        let (lo, hi) = range.unwrap_or((0, end));
        let wav = TempFile(temp_path("mix", "wav"));
        render_mix(s, &wav.0)?;
        let piece = Piece {
            label: "mix".into(),
            path: wav.0.clone(),
            stream: 0,
            from: f64::from(lo.saturating_sub(pad)) / fps,
            to: f64::from((hi + pad).min(end)) / fps,
            keep: (f64::from(lo) / fps, f64::from(hi) / fps),
            removed: Vec::new(),
            map: Map::Mix,
            mix: true,
        };
        segs.extend(run_piece(&bin, &piece, model, lang, hint, fps)?);
        // Audible sound the model returned no words for: reported, not hidden.
        if let Ok(Some(samples)) = decode_16k(&wav.0, 0, f64::from(lo) / fps, f64::from(hi) / fps) {
            gaps = coverage_gaps(&samples, lo, fps, &segs);
        }
    } else {
        let source = match &want {
            Want::Source(n) => Some(*n),
            _ => None,
        };
        let clips = clips_of(s, want);
        if let (true, Some(n)) = (clips.is_empty(), source) {
            segs.extend(source_segs(s, n, range, &bin, model, lang, hint, fps)?);
        }
        for (lane, clip) in clips {
            let Some(src) = s.sources().get(clip.source) else {
                return Err(format!("a clip uses src:{}, which is not on the timeline", clip.source));
            };
            let Some((lo, hi)) = clip_source_span(&clip, range) else {
                continue;
            };
            // The requested span plus padding. The padding reads past the clip into the audio a cut
            // took out beside it: the model then hears a word on the seam whole, where it is, and
            // the words that fall in that audio are dropped (`removed`), not heard again as new ones.
            let (plo, phi) = (lo.saturating_sub(pad), hi + pad);
            let piece = Piece {
                label: parse::addr(lane, clip.start),
                path: src.path.clone(),
                stream: src.audio_stream,
                from: f64::from(plo) / fps,
                to: f64::from(phi) / fps,
                keep: (f64::from(lo) / fps, f64::from(hi) / fps),
                removed: cut_out_source(s, lane, clip.source, fps),
                map: Map::of(&clip),
                mix: false,
            };
            segs.extend(run_piece(&bin, &piece, model, lang, hint, fps)?);
        }
    }
    segs.sort_by_key(|g| (g.start_f, g.end_f));
    let mut segs = drop_overlap_repeats(segs);
    let corrections = correct_hints(&mut segs, hint);
    Ok(Transcript { segs, fps, gaps, corrections })
}

/// The length of source `n` in frames at the timeline rate, from its own file.
fn source_frames(s: &PlaybackSession, n: usize, fps: f64) -> Result<u32, String> {
    let src = s.sources().get(n).ok_or_else(|| format!("src:{n} is not on this timeline"))?;
    let secs = engine::probe_media(&src.path)
        .duration_secs
        .filter(|d| *d > 0.0)
        .ok_or_else(|| format!("src:{n} has no length to transcribe: {}", src.path.display()))?;
    Ok((secs * fps).ceil() as u32)
}

/// A source no clip uses (imported, not placed): its own file, from the start, in
/// source time at the timeline rate, so `range` counts from the file's first frame.
#[allow(clippy::too_many_arguments)]
fn source_segs(
    s: &PlaybackSession,
    n: usize,
    range: Option<(u32, u32)>,
    bin: &Path,
    model: &str,
    lang: &str,
    hint: &str,
    fps: f64,
) -> Result<Vec<Seg>, String> {
    let src = s.sources().get(n).ok_or_else(|| format!("src:{n} is not on this timeline"))?;
    let end = source_frames(s, n, fps)?;
    let (lo, hi) = range.map_or((0, end), |(a, b)| (a.min(end), b.min(end)));
    if hi <= lo {
        return Ok(Vec::new());
    }
    let pad = (WINDOW_SECS * fps).round() as u32;
    let piece = Piece {
        label: format!("src:{n}"),
        path: src.path.clone(),
        stream: src.audio_stream,
        from: f64::from(lo.saturating_sub(pad)) / fps,
        to: f64::from((hi + pad).min(end)) / fps,
        keep: (f64::from(lo) / fps, f64::from(hi) / fps),
        removed: Vec::new(),
        map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
        mix: false,
    };
    run_piece(bin, &piece, model, lang, hint, fps)
}

/// `mm:ss.mmm` for a timeline frame.
/// What one `find_words` hit line shows.
struct HitText<'a, D: std::fmt::Display> {
    n: usize,
    /// The match's first and last frame.
    span: (u32, u32),
    /// Each word with its frames, as printed.
    words: &'a [String],
    /// The match's start and end as clock times.
    stamps: (String, String),
    /// The segment's place in the transcript.
    at: D,
    before: &'a str,
    phrase: &'a str,
    after: &'a str,
}

/// One hit line: the frames, the words with their own frames, the clock times, then the
/// match in its context. The phrase is printed once, in the brackets.
fn hit_line<D: std::fmt::Display>(h: &HitText<D>) -> String {
    format!(
        "#{} {}f–{}f ({}) ({}–{}) [{}] …{} [{}] {}…\n",
        h.n,
        h.span.0,
        h.span.1,
        h.words.join(", "),
        h.stamps.0,
        h.stamps.1,
        h.at,
        h.before,
        h.phrase,
        h.after
    )
}

fn stamp(frame: u32, fps: f64) -> String {
    let ms = (f64::from(frame) / fps * 1000.0).round() as u64;
    format!("{:02}:{:02}.{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000)
}

fn word_json(w: &Word) -> Value {
    json!({"w": w.w, "start_f": w.start_f, "end_f": w.end_f, "p": (w.p * 1000.0).round() / 1000.0})
}

/// The segments from `from` that fit one page, the text for them, and the frame
/// the next page starts at when some did not fit.
fn page(t: &Transcript, from: u32, words: bool) -> (String, Vec<&Seg>, Option<u32>) {
    let mut out = String::new();
    let mut shown = Vec::new();
    let mut next = None;
    for seg in t.segs.iter().filter(|g| g.start_f >= from) {
        let mark = if seg.low_confidence() { "? " } else { "" };
        let mut block = format!(
            "{}–{} [{}] {mark}{}\n",
            stamp(seg.start_f, t.fps),
            stamp(seg.end_f, t.fps),
            seg.at,
            seg.text
        );
        if words && !seg.words.is_empty() {
            let ws: Vec<String> = seg.words.iter().map(|w| format!("{} {}f-{}f", w.w, w.start_f, w.end_f)).collect();
            block.push_str("  ");
            block.push_str(&ws.join(" · "));
            block.push('\n');
        }
        if !shown.is_empty() && out.len() + block.len() > CAP {
            next = Some(seg.start_f);
            break;
        }
        out.push_str(&block);
        shown.push(seg);
    }
    (out, shown, next)
}

fn run_transcript(srv: &mut Server, args: &Value) -> Reply {
    match transcript_run(srv, args) {
        Ok(r) => r,
        Err(e) => Reply::err(e),
    }
}

fn transcript_run(srv: &Server, args: &Value) -> Result<Reply, String> {
    let s = session(srv)?;
    let fps = parse::session_fps(s);
    let want = want_arg(s, args)?;
    // A source is read in its own file's frames, placed or not: its range counts in its own file.
    let end = match &want {
        Want::Source(n) => source_frames(s, *n, fps)?,
        _ => parse::session_end(s),
    };
    let range = range_arg(args, fps, end)?;
    check_sources_online(s, &want)?;
    let model = model_arg(args)?;
    let lang = language_arg(args)?;
    let hint = with_fillers(&hint_arg(args)?, opt_bool(args, "fillers").unwrap_or(false));
    let from = parse::opt_frame_arg(args, "from", fps, end).map_err(|r| r.text)?.unwrap_or(0);
    let words = opt_bool(args, "words").unwrap_or(false);
    let label = want_label(s, &want);
    // A source's segments are labelled with its own times (src:N), whatever clips play it; the
    // reply then names those clips, so a timeline address is never read as the source's time.
    let source_clips = match &want {
        Want::Source(n) => Some((*n, clips_of(s, want.clone()))),
        _ => None,
    };
    let t = match &want {
        Want::Source(n) => transcribe_source(s, *n, range, &model, &lang, &hint, fps)?,
        _ => transcribe(s, want, range, &model, &lang, &hint)?,
    };
    let (body, shown, next) = page(&t, from, words);
    let mut text = transcript_text(&label, &model, &lang, from, &body, &shown, next);
    if let Some((n, clips)) = &source_clips {
        text.push_str(&source_clips_note(*n, clips, fps));
    }
    text.push_str(&review_text(&shown, fps));
    let fixes = hint_note(&t.corrections);
    if !fixes.is_empty() {
        text.push('\n');
        text.push_str(&fixes);
    }
    for (a, b) in &t.gaps {
        text.push_str(&format!(
            "\nno words in {}–{} though audio is loud there: the model returned none for it. Try target:\"A1\" (the voice lane), or a narrower range, to hear it again.",
            stamp(*a, t.fps),
            stamp(*b, t.fps)
        ));
    }
    let any_approx = shown.iter().any(|g| g.approx);
    let segments: Vec<Value> = shown
        .iter()
        .map(|g| {
            json!({
                "start_f": g.start_f,
                "end_f": g.end_f,
                "text": g.text,
                "clip": g.at,
                "approx": g.approx,
                "no_speech": g.no_speech,
                "avg_logprob": g.avg_logprob.map(|lp| (lp * 1000.0).round() / 1000.0),
                "low_confidence": g.low_confidence(),
                "words": g.words.iter().map(word_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    let gaps: Vec<Value> = t.gaps.iter().map(|(a, b)| json!([format!("{a}f"), format!("{b}f")])).collect();
    Ok(Reply::ok(text).with(json!({"segments": segments, "approx": any_approx, "next_from": next, "gaps": gaps, "hint_corrections": corrections_json(&t.corrections)})))
}

/// The reply text of `transcript`. With no segments it is one no-speech line and
/// no timing claims; with segments it states what the markers mean, once each.
fn transcript_text(
    label: &str,
    model: &str,
    lang: &str,
    from: u32,
    body: &str,
    shown: &[&Seg],
    next: Option<u32>,
) -> String {
    let mut text = if shown.is_empty() {
        format!(
            "transcript {label}: no speech found in this range · model {model} · language {lang}. \
edith-stt filters silent and hallucinated windows, so nothing was heard here."
        )
    } else {
        let mut head = format!(
            "transcript {label}: {} segment(s) shown, from {from}f · model {model} · language {lang}. Segment times are reliable.",
            shown.len()
        );
        if shown.iter().any(|g| g.approx) {
            head.push_str(" Word times approximate in the segments marked approx.");
        }
        if shown.iter().any(|g| g.low_confidence()) {
            head.push_str(" A `?` marks a low-confidence segment (avg_logprob below -1.0): check it against the audio.");
        }
        format!("{head}\n{}", body.trim_end())
    };
    if let Some(n) = next {
        text.push_str(&format!("\nmore: from={n}f"));
    }
    text
}

// ---- find_words -------------------------------------------------------------

/// Where `query` starts in `words` (both already normalised).
fn phrase_hits(words: &[String], query: &[String]) -> Vec<usize> {
    if query.is_empty() || words.len() < query.len() {
        return Vec::new();
    }
    (0..=words.len() - query.len())
        .filter(|&i| query.iter().enumerate().all(|(j, q)| same_word(q, &words[i + j])))
        .collect()
}

/// The span a cut takes for one match, in frames: its words (`start`..`end`) padded `pad` each
/// side, but never into the neighbouring words (`prev_end`, `next_start`) and never past the
/// timeline (`timeline_end`). With no word after it, `next_start` is `u32::MAX`.
fn cut_span(start: u32, end: u32, pad: u32, prev_end: u32, next_start: u32, timeline_end: u32) -> (u32, u32) {
    let a = start.saturating_sub(pad).max(prev_end.min(start));
    let b = end.saturating_add(pad).min(next_start.max(end)).min(timeline_end.max(end));
    (a, b)
}

/// What a match's cut padding came out as: the frames the span takes before and after the words,
/// the words' own length, and each side that stopped short of the padding, with the frame it stopped
/// at and what held it there.
struct PadFact {
    len: u32,
    before: u32,
    after: u32,
    stops: Vec<String>,
}

/// The padding a match's `span` took, given its words (`start`..`end`), the requested `pad` and the
/// next word's start. Call it after `clamp_to_clips`, so the clip edges count. A side the timeline
/// end stops is named as such.
fn pad_fact(start: u32, end: u32, pad: u32, span: (u32, u32), next_start: u32, clips: &[(u32, u32)]) -> PadFact {
    let (a, b) = span;
    let mut stops = Vec::new();
    if a > start.saturating_sub(pad) {
        let clip = clips.iter().any(|&(c0, c1)| c0 <= start && start < c1 && c0 == a);
        let why = if clip { "the clip edge" } else { "the previous word ends there" };
        stops.push(format!("{a}f ({why})"));
    }
    if b < end.saturating_add(pad) {
        let clip = clips.iter().any(|&(c0, c1)| c0 < end && end <= c1 && c1 == b);
        let why = if clip {
            "the clip edge"
        } else if b == next_start.max(end) {
            "the next word starts there"
        } else {
            "the timeline end"
        };
        stops.push(format!("{b}f ({why})"));
    }
    PadFact { len: end - start, before: start.saturating_sub(a), after: b.saturating_sub(end), stops }
}

/// The padding part of a cut reply, such as ` (the match 0.400 s plus 2f padding each side, stopped
/// at 360f (the clip edge))`. Empty when `facts` is empty.
fn pad_note(facts: &[&PadFact], pad: u32, fps: f64) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let len: u32 = facts.iter().map(|f| f.len).sum();
    let noun = if facts.len() == 1 { "the match" } else { "the matches" };
    let padding = match facts {
        [one] if one.before == pad && one.after == pad => format!("plus {pad}f padding each side"),
        [one] => format!("plus {}f before and {}f after", one.before, one.after),
        _ => format!("plus up to {pad}f padding each side"),
    };
    let stops: Vec<String> = facts.iter().flat_map(|f| f.stops.iter().cloned()).collect();
    let stopped = if stops.is_empty() { String::new() } else { format!(", stopped at {}", stops.join("; ")) };
    format!(" ({noun} {:.3} s {padding}{stopped})", parse::secs(len, fps))
}

/// Every clip on `lanes` as `[start, end)` timeline frames.
fn clip_intervals(s: &PlaybackSession, lanes: &[Lane]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &l in lanes {
        for c in s.lane_clips(l) {
            out.push((c.start, c.end()));
        }
    }
    out.sort_unstable();
    out
}

/// `span` held inside the clip that holds a match (`first`..`last` frames): its start is not
/// before that clip's start, its end not past that clip's end. A match on no clip is left as it is.
fn clamp_to_clips(span: (u32, u32), clips: &[(u32, u32)], first: u32, last: u32) -> (u32, u32) {
    let (mut a, mut b) = span;
    if let Some(&(lo, _)) = clips.iter().find(|&&(c0, c1)| c0 <= first && first < c1) {
        a = a.max(lo);
    }
    if let Some(&(_, hi)) = clips.iter().find(|&&(c0, c1)| c0 < last && last <= c1) {
        b = b.min(hi);
    }
    (a, b.max(a))
}

/// Overlapping or touching spans joined and sorted; empty spans dropped.
fn merge_spans(mut spans: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    spans.retain(|(a, b)| b > a);
    spans.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (a, b) in spans {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The text of a search with no match. A filler query says what the silence means.
fn no_match_text(query: &str, label: &str, model: &str, words_heard: usize, fillers: bool) -> String {
    if fillers {
        format!(
            "no '{}' heard by the {model} model in {label} ({words_heard} words heard); whisper may miss short fillers \u{2014} try model:\"small\" or listen with levels/hear.",
            query.trim()
        )
    } else {
        format!(
            "no match for \"{}\" in {label} ({words_heard} words heard; model {model}). Word search runs over what edith-stt heard, so try a shorter phrase or model small.",
            query.trim()
        )
    }
}

/// Which matches a cut takes.
enum Which {
    All,
    First,
    Nums(Vec<usize>),
}

fn which_arg(args: &Value) -> Result<Which, String> {
    match args.get("which") {
        None | Some(Value::Null) => Ok(Which::All),
        Some(Value::String(w)) => match w.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Which::All),
            "first" => Ok(Which::First),
            _ => Err(format!("`which` is all, first, or a list of match numbers; got `{w}`")),
        },
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_u64()
                    .map(|n| n as usize)
                    .ok_or_else(|| "`which` lists match numbers (#n in the reply), such as [0, 2]".to_string())
            })
            .collect::<Result<Vec<usize>, String>>()
            .map(Which::Nums),
        Some(_) => Err("`which` is all, first, or a list of match numbers (#n in the reply)".into()),
    }
}

impl Which {
    /// The match numbers picked out of `n` matches. A number past the last match is an error.
    fn pick(&self, n: usize) -> Result<Vec<usize>, String> {
        match self {
            Which::All => Ok((0..n).collect()),
            Which::First => Ok((0..n.min(1)).collect()),
            Which::Nums(nums) => nums
                .iter()
                .map(|&i| if i < n { Ok(i) } else { Err(format!("match #{i} does not exist: the query has {n} match(es)")) })
                .collect(),
        }
    }
}

/// `pad`: the time each side of a cut. Default 0.06 s, about two frames at 30 fps; at most a second.
fn pad_arg(args: &Value, fps: f64) -> Result<u32, String> {
    let v = match args.get("pad") {
        None | Some(Value::Null) => json!("0.06s"),
        Some(v) => v.clone(),
    };
    let f = parse::frame(&v, fps, u32::MAX).map_err(|e| format!("`pad`: {e}"))?;
    if f as f64 > fps {
        return Err("`pad` must be at most 1s".into());
    }
    Ok(f)
}

/// What a find_words cut did: the timeline either side of it, and its undo step.
struct Cut {
    /// The timeline end either side of the cut.
    before: u32,
    after: u32,
    /// The end of the cut lanes either side: a cut on A1 alone shortens A1 even when the
    /// timeline end is set by a longer lane.
    lanes_before: u32,
    lanes_after: u32,
    undo_step: usize,
    /// The ripple notes an edit op gives too: the lanes the cut left in place, and the
    /// caption lanes that now run out of sync (`caption_drift`). Empty when none.
    ripple: String,
    /// The effects an edit op's reply names for the same cut (`side_effects`), in order.
    effects: Vec<String>,
}

/// The end of the latest clip on `lanes`, in frames.
pub(crate) fn lanes_end(s: &PlaybackSession, lanes: &[Lane]) -> u32 {
    lanes.iter().flat_map(|&l| s.lane_clips(l).iter().map(|c| c.end())).max().unwrap_or(0)
}

/// The start, end and text of every caption cue on `scope`'s caption lanes: what a cut may move,
/// trim or remove, read before it and after it.
pub(crate) fn scope_cues(s: &PlaybackSession, scope: &[Lane]) -> Vec<(i64, i64, String)> {
    scope
        .iter()
        .filter(|l| l.kind == LaneKind::Subtitle)
        .flat_map(|&l| s.sub_lane_cues(l).into_iter().map(|c| (c.start_us, c.end_us, c.text)))
        .collect()
}

/// What a cut did to the captions, from the cues before it and after it: cues are matched by their
/// words, in order. A matched cue whose start moved counts as moved, one whose length changed as
/// trimmed. A cue the cut split keeps its first or last words as a piece, which counts as trimmed;
/// a cue with no match at all is named as removed, at the start it had. Empty when nothing changed.
pub(crate) fn caption_changes(before: &[(i64, i64, String)], after: &[(i64, i64, String)]) -> String {
    let mut left: Vec<usize> = (0..after.len()).collect();
    let mut gone: Vec<(i64, String)> = Vec::new();
    let (mut moved, mut trimmed) = (0usize, 0usize);
    for (start, end, text) in before {
        if let Some(p) = left.iter().position(|&i| after[i].2 == *text) {
            let (a0, a1, _) = &after[left.remove(p)];
            moved += usize::from(a0 != start);
            trimmed += usize::from(a1 - a0 != end - start);
        } else if let Some(p) = left.iter().position(|&i| is_piece_of(&after[i].2, text)) {
            left.remove(p);
            trimmed += 1;
        } else {
            gone.push((*start, text.clone()));
        }
    }
    let mut parts = Vec::new();
    if moved > 0 {
        parts.push(format!("{moved} moved"));
    }
    if trimmed > 0 {
        parts.push(format!("{trimmed} trimmed"));
    }
    let mut out = match parts.is_empty() {
        true => String::new(),
        false => format!("; captions followed the cut: {}", parts.join(", ")),
    };
    out.push_str(&removed_note(&gone));
    out
}

/// Whether `piece` is the first or last words of `whole`, short of all of them: what a cut leaves of
/// a cue it splits.
fn is_piece_of(piece: &str, whole: &str) -> bool {
    let p: Vec<&str> = piece.split_whitespace().collect();
    let w: Vec<&str> = whole.split_whitespace().collect();
    !p.is_empty() && p.len() < w.len() && (w.starts_with(&p) || w.ends_with(&p))
}

/// The caption lanes a cut rides along with: captions follow a cut on their own, as `cut_regions`
/// moves them when they are in its scope, so every caption lane the scope does not name joins it.
/// A locked caption lane stays where it is and is returned, for the reply to name.
pub(crate) fn ride_captions(s: &PlaybackSession, scope: &mut Vec<Lane>) -> Vec<Lane> {
    let mut locked = Vec::new();
    for l in s.subtitle_lanes() {
        if scope.contains(&l) {
            continue;
        }
        if s.lane_locked(l) {
            locked.push(l);
        } else {
            scope.push(l);
        }
    }
    locked
}

/// The end of the picture and sound on `lanes`, in frames: the captions over them are left out, so
/// a cut's loss is the media's and not a caption tail's.
pub(crate) fn media_end(s: &PlaybackSession, lanes: &[Lane]) -> u32 {
    lanes
        .iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .flat_map(|&l| s.lane_clips(l).iter().map(|c| c.end()))
        .max()
        .unwrap_or(0)
}

/// The note for a timeline whose captions run past its picture and sound, which is what makes it
/// longer than the picture: both ends named. Empty when no caption runs past.
pub(crate) fn caption_tail_note(s: &PlaybackSession, fps: f64) -> String {
    let media = media_end(s, &s.lanes());
    let caps = s
        .subtitle_lanes()
        .into_iter()
        .flat_map(|l| s.sub_lane_cues(l))
        .map(|c| (c.end_us as f64 * fps / 1e6).round() as u32)
        .max()
        .unwrap_or(0);
    match media > 0 && caps > media {
        true => format!(
            "; captions run to {:.3} s, past the picture's end at {:.3} s",
            parse::secs(caps, fps),
            parse::secs(media, fps)
        ),
        false => String::new(),
    }
}

/// `; removed cue 'Coming up' at 22.1s` for each cue a cut removed, at its start before the cut.
fn removed_note(gone: &[(i64, String)]) -> String {
    gone.iter()
        .map(|(at, text)| format!("; removed cue '{}' at {:.1}s", text.replace('\n', " / "), *at as f64 / 1e6))
        .collect()
}

/// Lifts `spans` (timeline frames, start and end) from `scope` lanes: each span is split out on the
/// scope lanes and the clips it covers are lifted, so a hole the span's length is left and nothing
/// after it moves. This is `cut:"lift"`, the way to remove a match while the other lanes keep their
/// time. Lanes outside `scope` are not touched. An `Err` names the refusal; the caller rolls back.
fn lift_spans(s: &mut PlaybackSession, spans: &[(u32, u32)], scope: &[Lane], fps: f64) -> Result<(), String> {
    for &(a, b) in spans {
        for f in [a, b] {
            // A split on the scope lanes only; a frame no clip crosses changes nothing.
            s.cut_at_lanes(parse::secs(f, fps), scope);
        }
    }
    for &(a, b) in spans {
        for &lane in scope {
            // Highest index first, so lifting one clip never shifts one still to visit.
            let inside: Vec<usize> =
                s.lane_clips(lane).iter().enumerate().filter(|(_, c)| c.start >= a && c.end() <= b).map(|(i, _)| i).collect();
            for idx in inside.into_iter().rev() {
                if !s.lift_clip(lane, idx) {
                    return Err(format!("lift refused on {}: the lane is locked", parse::lane_name(lane)));
                }
            }
        }
    }
    Ok(())
}

/// The warning a ripple owes when it moves the cut lanes but leaves another sound lane where it
/// was: a sound lane outside `scope` with a clip running past `from` now plays out of step with
/// the lanes that moved. Names the lift option, which leaves a hole and keeps every lane in time.
/// Empty when no such lane.
fn out_of_sync_note(s: &PlaybackSession, scope: &[Lane], from: u32, fps: f64) -> String {
    let stale: Vec<String> = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio && !scope.contains(l))
        .filter(|&l| s.lane_clips(l).iter().any(|c| c.end() > from))
        .map(parse::lane_name)
        .collect();
    if stale.is_empty() {
        return String::new();
    }
    let moved: Vec<String> = scope.iter().map(|&l| parse::lane_name(l)).collect();
    format!(
        "; {} now out of sync with {} from {:.3} s (they were not cut): pass cut:\"lift\" to leave a hole and keep every lane in time",
        stale.join(", "),
        moved.join("+"),
        parse::secs(from, fps)
    )
}

/// Cuts `spans` (timeline frames, start and end) from `scope` lanes as one undo step. A dry run
/// rolls the cut back, so nothing is kept and no step is added. A refused cut changes nothing.
/// The reply's ripple notes are the ones an edit op's cut gives: titles over the moved picture
/// move with it, the lanes left in place, caption lanes that drift, the cues it removed, and the
/// effects it had.
fn cut_spans(srv: &mut Server, spans: &[(u32, u32)], dry: bool, what: &str, scope: &[Lane], args: &Value, lift: bool) -> Result<Cut, String> {
    let start = srv.session().map_err(|r| r.text)?.history_pushes();
    let (before, lanes_before, fps, shape0, cues0) = {
        let s = srv.session().map_err(|r| r.text)?;
        (parse::session_end(s), media_end(s, scope), parse::session_fps(s), shape(s), scope_cues(s, scope))
    };
    // A kept piece under a sliver between two cuts is joined into them, as the engine's cut would
    // leave it unreadable (edit.rs `join_slivers`). A lift leaves its holes as they are.
    let lens: Vec<(u32, u32)> = spans.iter().map(|&(a, b)| (a, b - a)).collect();
    let (regions, _) = super::edit::join_slivers(&lens);
    let cut = if lift {
        lift_spans(srv.session().map_err(|r| r.text)?, spans, scope, fps)
    } else {
        srv.session().map_err(|r| r.text)?.cut_regions(&regions, scope).map_err(|e| e.to_string())
    };
    if let Err(e) = cut {
        let rolled = roll_back_since(srv, start);
        let note = if rolled { "" } else { " (could not roll every step back; check the project)" };
        return Err(format!("cut refused: {e}; nothing changed{note}"));
    }
    let s = srv.session().map_err(|r| r.text)?;
    if lift {
        // Nothing moves: the timeline keeps its length and the lanes not cut keep their place.
        let from = spans.first().map_or(0, |sp| sp.0);
        let after = parse::session_end(s);
        let lanes_after = media_end(s, scope);
        let holes = scope.iter().map(|&l| parse::lane_name(l)).collect::<Vec<_>>().join("+");
        let ripple = format!("; holes left on {holes} at {:.3} s: nothing after them moved", parse::secs(from, fps));
        let steps = s.history_pushes().saturating_sub(start) as usize;
        if dry {
            roll_back_since(srv, start);
        } else if steps > 0 {
            srv.ledger.push(Entry::new(steps, format!("find_words lift {what}")));
            srv.history_lost_on_open = false;
            srv.redo_ledger.clear();
            srv.trim_ledger();
        }
        return Ok(Cut { before, after, lanes_before, lanes_after, undo_step: srv.ledger.len(), ripple, effects: Vec::new() });
    }
    let mut moved = if has_picture(scope) { move_titles_for_cuts(s, &regions, fps)? } else { Vec::new() };
    // A kept piece the cut left under a few frames is a hole now: it closes with the ripple.
    let closed = sliver_note(close_sliver_holes(s, scope, fps, &mut moved));
    let s = srv.session_ref().map_err(|r| r.text)?;
    let after = parse::session_end(s);
    let lanes_after = media_end(s, scope);
    // The spans are sorted and joined, so the first starts the cut: what starts at or
    // after it on the lanes not cut stays where it was, as an edit op says.
    let from = regions.first().map_or(0, |sp| sp.0);
    let effects = side_effects(&shape0, &shape(s), "cut_regions");
    let ripple = format!(
        "{}{closed}{}{}{}{}{}",
        titles_note(&moved),
        left_behind(s, scope, from),
        out_of_sync_note(s, scope, from, fps),
        caption_drift(s, args, scope, from),
        caption_changes(&cues0, &scope_cues(s, scope)),
        caption_tail_note(s, fps)
    );
    let steps = s.history_pushes().saturating_sub(start) as usize;
    if dry {
        roll_back_since(srv, start);
    } else if steps > 0 {
        srv.ledger.push(Entry::new(steps, format!("find_words cut {what}")));
        srv.history_lost_on_open = false;
        srv.redo_ledger.clear();
        srv.trim_ledger();
    }
    Ok(Cut { before, after, lanes_before, lanes_after, undo_step: srv.ledger.len(), ripple, effects })
}

fn run_find(srv: &mut Server, args: &Value) -> Reply {
    match find_run(srv, args) {
        Ok(r) => r,
        Err(e) => Reply::err(e),
    }
}

fn find_run(srv: &mut Server, args: &Value) -> Result<Reply, String> {
    let query = opt_str(args, "query").ok_or("missing `query`: the words to find, e.g. \"brown fox\"")?.to_string();
    let qwords: Vec<String> = query.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
    if qwords.is_empty() {
        return Err("`query` has no words to find".into());
    }
    let (cut, lift) = cut_arg(args)?;
    // Without cut a listing is read-only already, so dry_run is accepted and named as having no effect.
    let dry = opt_bool(args, "dry_run").unwrap_or(false);
    let which = which_arg(args)?;
    let keep = keep_arg(args)?;
    // Each cut edge moves to the quiet point near it (see `snap_spans`) unless `snap` is false.
    let snap_on = opt_bool(args, "snap").unwrap_or(true);
    if keep.is_some() {
        if !cut {
            return Err("`keep` applies to a cut: pass cut true with it".into());
        }
        if qwords.len() < 2 || qwords.iter().any(|w| *w != qwords[0]) {
            return Err("`keep` needs a query whose words all repeat, such as \"I I\" or \"so so\"".into());
        }
    }
    let s = session(srv)?;
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let pad = if cut { pad_arg(args, fps)? } else { 0 };
    let want = want_arg(s, args)?;
    // Where the target's sound is read from, for the silence check on each match: the mix, a
    // source read on its own clock, or the target's clips.
    let mixed = matches!(want, Want::Mix);
    let target_clips = clips_of(s, want.clone());
    let direct = match &want {
        Want::Source(n) if target_clips.is_empty() => s.sources().get(*n).map(|src| (src.path.clone(), src.audio_stream)),
        _ => None,
    };
    let mut scope = cut_scope(s, &want, args)?;
    // A lift moves nothing, so captions (which only ripple) stay where they are: a lift's scope is
    // its sound and picture lanes, and the reply says so when there are caption lanes.
    let caption_note = lift && !s.subtitle_lanes().is_empty();
    if lift {
        scope.retain(|l| l.kind != LaneKind::Subtitle);
    }
    // Taken before any change, so the reply can say the target's lane was added to a named scope.
    let added_target = if cut { target_added(&want, args) } else { None };
    // The locks are checked before anything is heard or changed, so a refused cut costs nothing.
    let skipped_locked = if cut { cut_locks(s, &want, args, &mut scope)? } else { Vec::new() };
    // Captions ride the cut: the caption lanes the scope does not name join it, and a locked one stays put.
    let locked_captions = if cut && !lift { ride_captions(s, &mut scope) } else { Vec::new() };
    // The clips on the cut lanes: a match's padding stops at the clip edge, so a cut never takes the
    // gap beside a clip, which the ripple would close and shift the clip by.
    let clips = clip_intervals(s, &scope);
    check_sources_online(s, &want)?;
    let model = model_arg(args)?;
    let lang = language_arg(args)?;
    let fillers = is_filler_query(&qwords);
    let hint = with_fillers(&hint_arg(args)?, fillers);
    let label = want_label(s, &want);
    let t = transcribe(s, want, None, &model, &lang, &hint)?;
    let mut flat: Vec<(&Seg, &Word)> = t.segs.iter().flat_map(|g| g.words.iter().map(move |w| (g, w))).collect();
    flat.sort_by_key(|(_, w)| w.start_f);
    let norms: Vec<String> = flat.iter().map(|(_, w)| norm(&w.w)).collect();
    let k = qwords.len();
    // A filler search can quote its own prompt back over speech Whisper did not hear: a match that
    // is the prompt's words in order is dropped, and the reply says how many.
    let prompt = prompt_words();
    let all_hits = phrase_hits(&norms, &qwords);
    let hits: Vec<usize> = all_hits.iter().copied().filter(|&i| !fillers || !prompt_echo(&norms, i, k, &prompt)).collect();
    let echoed = all_hits.len() - hits.len();
    // A match whose span (padded a little) has no sound on the target is a word Whisper heard in
    // silence (a filler, mostly): dropped, and counted in the reply.
    let mix_wav = if mixed && !hits.is_empty() {
        let wav = TempFile(temp_path("silence", "wav"));
        render_mix(s, &wav.0)?;
        Some(wav)
    } else {
        None
    };
    let ground = match (&mix_wav, &direct) {
        (Some(wav), _) => Ground::Mix(&wav.0),
        (None, Some((path, stream))) => Ground::Source(path, *stream),
        _ => Ground::Clips(&target_clips),
    };
    let pad_f = (SILENCE_PAD_SECS * fps).round() as u32;
    let mut audible = Vec::new();
    let mut silent_dropped = 0usize;
    for &i in &hits {
        let first = flat[i].1.start_f;
        let last = flat[i + k - 1].1.end_f.max(first);
        let (from, to) = (first.saturating_sub(pad_f), (last + pad_f).max(first + 1));
        if span_silent(s, &ground, from, to, fps)? {
            silent_dropped += 1;
        } else {
            audible.push(i);
        }
    }
    let hits = audible;
    let mut out = String::new();
    let mut truncated = false;
    let mut matches = Vec::new();
    // Each match's cut span, padded and stopped at its neighbours; `which` picks from these.
    let mut spans: Vec<(u32, u32)> = Vec::new();
    // Each match's padding as the span took it, for the padding note a cut reply gives.
    let mut pads: Vec<PadFact> = Vec::new();
    let mut any_approx = false;
    let mut hit_segs: Vec<&Seg> = Vec::new();
    // Each match's number, phrase and start, for the kept list a cut reply gives.
    let mut match_info: Vec<(usize, String, u32)> = Vec::new();
    // Filler spellings heard for a query word: (query word, heard word) -> matches.
    let mut variants: std::collections::BTreeMap<(String, String), usize> = std::collections::BTreeMap::new();
    // A filler word the query names (like, um) that sits mid-phrase, with no pause either side, is
    // most likely the word itself ("like a marathon"), not a filler: named in the reply.
    let filler_word = may_be_real_word(&qwords);
    let mut looks_real: Vec<String> = Vec::new();
    // Each context word's silence check, made once (see context_usable).
    let mut quiet = std::collections::HashMap::new();
    for (n, &i) in hits.iter().enumerate() {
        let (seg, first) = flat[i];
        let last = flat[i + k - 1].1;
        let start_f = first.start_f;
        let end_f = last.end_f.max(start_f);
        let phrase: Vec<&str> = flat[i..i + k].iter().map(|(_, w)| w.w.as_str()).collect();
        let words: Vec<&Word> = flat[i..i + k].iter().map(|(_, w)| *w).collect();
        // The context is only words that were heard and kept: a word over silence, or in a segment
        // that may be made up, is left out, so the reply never quotes text the speaker did not say.
        let before_ix = context_indices(i.saturating_sub(CONTEXT_WORDS)..i, |j| {
            context_usable(s, &ground, &flat, t.fps, pad_f, &mut quiet, j)
        })?;
        let after_ix = context_indices((i + k)..(i + k + CONTEXT_WORDS).min(flat.len()), |j| {
            context_usable(s, &ground, &flat, t.fps, pad_f, &mut quiet, j)
        })?;
        let before: Vec<&str> = before_ix.iter().map(|&j| flat[j].1.w.as_str()).collect();
        let after: Vec<&str> = after_ix.iter().map(|&j| flat[j].1.w.as_str()).collect();
        let prev_end = if i > 0 { flat[i - 1].1.end_f } else { 0 };
        let next_start = flat.get(i + k).map_or(u32::MAX, |(_, w)| w.start_f);
        let span = match keep {
            None => cut_span(start_f, end_f, pad, prev_end, next_start, end),
            Some(k) => keep_span(k, (first.start_f, first.end_f), (last.start_f, last.end_f), pad, prev_end, next_start, end),
        };
        let span = clamp_to_clips(span, &clips, start_f, end_f);
        spans.push(span);
        pads.push(pad_fact(start_f, end_f, pad, span, next_start, &clips));
        if filler_word {
            let gap_before = if i == 0 { u32::MAX } else { start_f.saturating_sub(prev_end) };
            let gap_after = if i + k < flat.len() { next_start.saturating_sub(end_f) } else { u32::MAX };
            if mid_phrase(gap_before, gap_after, t.fps) {
                let mut quote = phrase.join(" ");
                for w in after.iter().take(2) {
                    quote.push(' ');
                    quote.push_str(w);
                }
                looks_real.push(real_word_note(n, &quote));
            }
        }
        for (q, w) in qwords.iter().zip(&words) {
            let heard = norm(&w.w);
            if heard != *q {
                *variants.entry((q.clone(), heard)).or_insert(0) += 1;
            }
        }
        let word_spans: Vec<String> = words.iter().map(|w| format!("{} {}f–{}f", w.w, w.start_f, w.end_f)).collect();
        let line = hit_line(&HitText {
            n,
            span: (start_f, end_f),
            words: &word_spans,
            stamps: (stamp(start_f, t.fps), stamp(end_f, t.fps)),
            at: &seg.at,
            before: &before.join(" "),
            phrase: &phrase.join(" "),
            after: &after.join(" "),
        });
        if out.is_empty() || out.len() + line.len() <= CAP {
            out.push_str(&line);
        } else if !truncated {
            truncated = true;
            out.push_str("(more matches; narrow the query or target)\n");
        }
        any_approx |= seg.approx;
        if !hit_segs.iter().any(|h| std::ptr::eq(*h, seg)) {
            hit_segs.push(seg);
        }
        match_info.push((n, phrase.join(" "), start_f));
        matches.push(json!({
            "n": n,
            "span": [format!("{start_f}f"), format!("{end_f}f")],
            "start_f": start_f,
            "end_f": end_f,
            "approx": seg.approx,
            "at": seg.at,
            "text": phrase.join(" "),
            "before": before.join(" "),
            "after": after.join(" "),
            "words": words
                .iter()
                .map(|w| json!({"w": w.w, "span": [format!("{}f", w.start_f), format!("{}f", w.end_f)], "start_f": w.start_f, "end_f": w.end_f}))
                .collect::<Vec<_>>(),
        }));
    }
    let mut text = if matches.is_empty() {
        no_match_text(&query, &label, &model, flat.len(), fillers)
    } else {
        let note = if any_approx { " Spans from approx segments are approximate." } else { "" };
        let mut kept = if fillers { " Disfluencies were kept (filler prompt).".to_string() } else { String::new() };
        if !variants.is_empty() {
            let heard: Vec<String> = variants.iter().map(|((q, h), c)| format!("\"{h}\" for \"{q}\" ×{c}")).collect();
            kept.push_str(&format!(" Filler spellings matched: {}.", heard.join(", ")));
        }
        for note in &looks_real {
            kept.push(' ');
            kept.push_str(note);
            kept.push('.');
        }
        match keep {
            Some(Keep::Last) => kept.push_str(" Cut every repeat but the last."),
            Some(Keep::First) => kept.push_str(" Cut every repeat but the first."),
            None => {}
        }
        format!("{} match(es) for \"{}\" in {label}.{note}{kept}\n{}", matches.len(), query.trim(), out.trim_end())
    };
    if echoed > 0 {
        text.push_str(&format!(
            "\n{echoed} match(es) dropped: their words are the filler prompt's, echoed by Whisper over speech it did not hear"
        ));
    }
    if silent_dropped > 0 {
        text.push_str(&format!(
            "\n{silent_dropped} match(es) dropped as silent: no sound on {label} at those times (Whisper heard words in silence)"
        ));
    }
    if dry && !cut {
        text.push_str("\n(dry_run has no effect without cut)");
    }
    text.push_str(&review_text(&hit_segs, t.fps));
    let fixes = hint_note(&t.corrections);
    if !fixes.is_empty() {
        text.push('\n');
        text.push_str(&fixes);
    }
    let mut structured = json!({"matches": matches, "approx": any_approx, "dropped_echoes": echoed, "dropped_silent": silent_dropped});
    structured["hint_corrections"] = corrections_json(&t.corrections);
    structured["variants"] = variants
        .iter()
        .map(|((q, h), c)| json!({"query": q, "heard": h, "matches": c}))
        .collect::<Vec<_>>()
        .into();
    if cut {
        let picked = which.pick(hits.len())?;
        let picked_spans: Vec<(u32, u32)> = picked.iter().map(|&n| spans[n]).collect();
        let snapped = if snap_on && !picked_spans.is_empty() {
            let words: Vec<(u32, u32)> = flat.iter().map(|(_, w)| (w.start_f, w.end_f)).collect();
            Some(snap_spans(s, &ground, &words, &picked_spans, fps)?)
        } else {
            None
        };
        let plan = merge_spans(snapped.as_ref().map_or(picked_spans, |sn| sn.spans.clone()));
        if plan.is_empty() {
            text.push_str("\nnothing cut");
        } else {
            let what = format!("\"{}\" ({} match(es))", query.trim(), picked.len());
            let done = cut_spans(srv, &plan, dry, &what, &scope, args, lift)?;
            // A ripple shortens the timeline by what it removes; a lift leaves that time as a hole.
            let removed = done.lanes_before.saturating_sub(done.lanes_after);
            let cleared: u32 = plan.iter().map(|(a, b)| b - a).sum();
            let secs = parse::secs(if lift { cleared } else { removed }, fps);
            let (b, a) = (parse::secs(done.before, fps), parse::secs(done.after, fps));
            // A keep cut takes other spans than the words, so its reply does not state the padding.
            let facts: Vec<&PadFact> = if keep.is_none() { picked.iter().map(|&n| &pads[n]).collect() } else { Vec::new() };
            let pad_part = pad_note(&facts, pad, fps);
            text.push('\n');
            if dry {
                let verb = if lift { "lift" } else { "cut" };
                text.push_str(&format!(
                    "dry run — would {verb} {} match(es), {secs:.3} s{pad_part}; timeline {b:.3} s → {a:.3} s; nothing changed",
                    picked.len()
                ));
            } else if lift {
                text.push_str(&format!(
                    "lifted {} match(es) from {} ({}): {secs:.3} s left as holes{pad_part}; timeline {b:.3} s · undo step {}",
                    picked.len(),
                    scope.len(),
                    scope.iter().map(|&l| parse::lane_name(l)).collect::<Vec<_>>().join("+"),
                    done.undo_step
                ));
                if caption_note {
                    text.push_str("; caption lanes are not lifted, so a cue on a cut word stays");
                }
            } else {
                text.push_str(&format!(
                    "cut {} match(es) from {} ({}): removed {secs:.3} s{pad_part}; timeline {b:.3} s → {a:.3} s ({}f) · undo step {}",
                    picked.len(),
                    scope.len(),
                    scope.iter().map(|&l| parse::lane_name(l)).collect::<Vec<_>>().join("+"),
                    done.after,
                    done.undo_step
                ));
            }
            if let Some(note) = &added_target {
                text.push_str(note);
            }
            if !skipped_locked.is_empty() {
                let names: Vec<String> = skipped_locked.iter().map(|&l| parse::lane_name(l)).collect();
                text.push_str(&format!("; skipped locked {}: it stays put", names.join(", ")));
            }
            if !locked_captions.is_empty() {
                let names: Vec<String> = locked_captions.iter().map(|&l| parse::lane_name(l)).collect();
                text.push_str(&format!("; locked captions {} stay put, so they are not cut (unlock them with lane_flags to ride the cut)", names.join(", ")));
            }
            if let Some(sn) = &snapped {
                text.push('\n');
                text.push_str(&snap_note(sn, fps));
            }
            text.push_str(&kept_note(&match_info, &picked, keep, fps));
            text.push_str(&done.ripple);
            if !done.effects.is_empty() {
                text.push_str(&format!("\n   effects: {}", done.effects.join("; ")));
            }
            structured["cut"] = json!({
                "dry_run": dry,
                "mode": if lift { "lift" } else { "ripple" },
                "which": picked,
                "spans": plan.iter().map(|(x, y)| json!([format!("{x}f"), format!("{y}f")])).collect::<Vec<_>>(),
                "removed_frames": removed,
                "timeline_frames_before": done.before,
                "timeline_frames": done.after,
                "undo_steps": done.undo_step,
            });
            if let Some(sn) = &snapped {
                structured["cut"]["snap"] = json!({"edges": sn.edges, "moved": sn.moved, "frames": sn.frames});
            }
        }
    }
    Ok(Reply::ok(text).with(structured))
}

/// Where a target's sound is read from for the silence check.
enum Ground<'a> {
    /// The rendered mix: its seconds are timeline seconds.
    Mix(&'a Path),
    /// A source no clip uses, read on its own clock: its frames are the file's.
    Source(&'a Path, usize),
    /// The target's clips: a timeline span reads through each clip's source.
    Clips(&'a [(Lane, Clip)]),
}

/// Whether the sound on the target over timeline frames `from..to` is silent: its level at or
/// below `GAP_AUDIBLE_DB`, the same bar the caption gaps use. A span no clip of the target covers
/// has no sound. A source whose sound cannot be read is kept (`false`): nothing is dropped on a guess.
fn span_silent(s: &PlaybackSession, ground: &Ground, from: u32, to: u32, fps: f64) -> Result<bool, String> {
    let samples = match ground {
        Ground::Mix(wav) => decode_16k(wav, 0, f64::from(from) / fps, f64::from(to) / fps)?,
        Ground::Source(path, stream) => decode_16k(path, *stream, f64::from(from) / fps, f64::from(to) / fps)?,
        Ground::Clips(clips) => {
            let hit = clips.iter().find_map(|(_, c)| clip_source_span(c, Some((from, to))).map(|span| (c, span)));
            let Some((clip, (lo, hi))) = hit else {
                return Ok(true);
            };
            let Some(src) = s.sources().get(clip.source) else {
                return Ok(false);
            };
            decode_16k(&src.path, src.audio_stream, f64::from(lo) / fps, f64::from(hi) / fps)?
        }
    };
    let Some(samples) = samples else {
        return Ok(false);
    };
    Ok(is_quiet(&samples))
}

/// The indices of the context words in `range` that `usable` accepts, in order. A word that is
/// not usable (over silence, or in a segment that may be made up) is left out of the context.
fn context_indices(range: std::ops::Range<usize>, mut usable: impl FnMut(usize) -> Result<bool, String>) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for j in range {
        if usable(j)? {
            out.push(j);
        }
    }
    Ok(out)
}

/// Whether the transcript word `j` (of `flat`, the find's words in time order) may stand as
/// context: its segment is not flagged as possibly made up, and its span (padded) is not silent on
/// the target. A silence check is memoised in `quiet`, since neighbouring matches share context.
fn context_usable(
    s: &PlaybackSession,
    ground: &Ground,
    flat: &[(&Seg, &Word)],
    fps: f64,
    pad_f: u32,
    quiet: &mut std::collections::HashMap<usize, bool>,
    j: usize,
) -> Result<bool, String> {
    let (seg, word) = flat[j];
    if !seg.suspect().is_empty() {
        return Ok(false);
    }
    if let Some(&q) = quiet.get(&j) {
        return Ok(!q);
    }
    let from = word.start_f.saturating_sub(pad_f);
    let to = (word.end_f + pad_f).max(from + 1);
    let silent = span_silent(s, ground, from, to, fps)?;
    quiet.insert(j, silent);
    Ok(!silent)
}

/// Whether a segment's span (microseconds) is mostly under one of the `dropped` caption cues,
/// so its words were dropped with them: the cue covers more than half of it.
fn over_dropped(span: (i64, i64), dropped: &[subtitle::Cue]) -> bool {
    let (start, end) = span;
    let len = (end - start).max(1);
    dropped.iter().any(|c| {
        let overlap = end.min(c.end_us) - start.max(c.start_us);
        overlap > 0 && overlap * 2 >= len
    })
}

/// The segments the caption review reads: those whose cues were not dropped over silence. A
/// dropped cue is named in the reply already, so its segment is neither checked nor listed as
/// possibly made up. `to_us` maps a frame to microseconds on the caption clock.
fn review_segs<'a>(segs: &'a [Seg], dropped: &[subtitle::Cue], to_us: impl Fn(u32) -> i64) -> Vec<&'a Seg> {
    segs.iter().filter(|g| !over_dropped((to_us(g.start_f), to_us(g.end_f)), dropped)).collect()
}

/// The caption cues whose span on the target is not silent, and the ones over silence, which
/// Whisper wrote words into. Each cue's frames are read with the same check find_words makes on
/// its matches ([`span_silent`]), on the target's own sound.
fn drop_silent_cues(s: &PlaybackSession, want: &Want, cues: Vec<subtitle::Cue>, fps: f64) -> Result<(Vec<subtitle::Cue>, Vec<subtitle::Cue>), String> {
    if cues.is_empty() {
        return Ok((cues, Vec::new()));
    }
    let target_clips = clips_of(s, want.clone());
    let direct = match want {
        Want::Source(n) if target_clips.is_empty() => s.sources().get(*n).map(|src| (src.path.clone(), src.audio_stream)),
        _ => None,
    };
    let wav = matches!(want, Want::Mix).then(|| TempFile(temp_path("silence", "wav")));
    if let Some(w) = &wav {
        render_mix(s, &w.0)?;
    }
    let ground = match (&wav, &direct) {
        (Some(w), _) => Ground::Mix(&w.0),
        (None, Some((path, stream))) => Ground::Source(path, *stream),
        _ => Ground::Clips(&target_clips),
    };
    split_by_quiet(cues, fps, |from, to| span_silent(s, &ground, from, to, fps))
}

/// Splits cues by whether their frames `from..to` are silent, as `quiet` says: the cues kept, and
/// the ones over silence.
fn split_by_quiet(
    cues: Vec<subtitle::Cue>,
    fps: f64,
    mut quiet: impl FnMut(u32, u32) -> Result<bool, String>,
) -> Result<(Vec<subtitle::Cue>, Vec<subtitle::Cue>), String> {
    let (mut kept, mut silent) = (Vec::new(), Vec::new());
    for c in cues {
        let from = (c.start_us as f64 / 1e6 * fps).round() as u32;
        let to = ((c.end_us as f64 / 1e6 * fps).round() as u32).max(from + 1);
        if quiet(from, to)? {
            silent.push(c);
        } else {
            kept.push(c);
        }
    }
    Ok((kept, silent))
}

/// Whether a stretch of samples is at or below `GAP_AUDIBLE_DB` (its RMS in dB); an empty one is quiet.
fn is_quiet(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return true;
    }
    let ms = samples.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / samples.len() as f64;
    10.0 * (ms + 1e-12).log10() <= GAP_AUDIBLE_DB
}

/// The matches a cut leaves in place, one `kept:` note each: a match a `which` list
/// does not pick is kept whole, and under `keep` each picked one keeps the word `keep`
/// names. Empty when every match is cut whole.
fn kept_note(info: &[(usize, String, u32)], picked: &[usize], keep: Option<Keep>, fps: f64) -> String {
    let mut out = String::new();
    for (n, phrase, start) in info {
        let why = match (picked.contains(n), keep) {
            (true, Some(Keep::First)) => "keep:first",
            (true, Some(Keep::Last)) => "keep:last",
            (true, None) => continue,
            (false, _) => "not cut",
        };
        out.push_str(&format!("; kept: '{phrase}' at {:.1}s ({why})", parse::secs(*start, fps)));
    }
    out
}

// ---- auto_captions ----------------------------------------------------------

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn run_captions(srv: &mut Server, args: &Value) -> Reply {
    match captions_run(srv, args) {
        Ok(r) => r,
        Err(e) => Reply::err(e),
    }
}

/// Takes back the snapshots pushed since `start` (a `history_pushes` reading),
/// clears the redo branch they left, and says whether every step came back.
fn roll_back_since(srv: &mut Server, start: u64) -> bool {
    let Some(s) = srv.session.as_mut() else {
        return true;
    };
    let n = s.history_pushes().saturating_sub(start) as usize;
    if n == 0 {
        return true;
    }
    let rolled = (0..n).all(|_| s.undo());
    // As in edit.rs: the rolled-back states are not a branch to redo.
    s.clear_redo();
    srv.redo_ledger.clear();
    srv.trim_ledger();
    rolled
}

/// The default caption file: `base` when nothing is there and no track on this timeline holds it,
/// else `<stem>-2.srt`, `-3`, ... The first track's file is never overwritten by a second captioning.
fn free_caption_path(s: &PlaybackSession, base: PathBuf) -> PathBuf {
    pick_free(base, |p| p.exists() || s.subtitles().iter().any(|t| same_file(&t.path, p)))
}

/// `base`, or the first `<stem>-N.srt` (N from 2) that `taken` does not hold.
fn pick_free(base: PathBuf, taken: impl Fn(&Path) -> bool) -> PathBuf {
    if !taken(&base) {
        return base;
    }
    let stem = base
        .file_stem()
        .map_or_else(|| "captions".to_string(), |x| x.to_string_lossy().into_owned());
    (2u32..)
        .map(|n| base.with_file_name(format!("{stem}-{n}.srt")))
        .find(|c| !taken(c))
        .unwrap_or(base)
}

/// The words of the transcript the model is confident of: in a segment that is not low-confidence,
/// with a probability of at least [`CUE_LOW_P`], and not a disfluency.
fn confident_words(segs: &[Seg]) -> usize {
    segs.iter()
        .filter(|g| !g.low_confidence())
        .flat_map(|g| g.words.iter())
        .filter(|w| w.p >= CUE_LOW_P && !CAPTION_FILLERS.contains(&norm(&w.w).as_str()))
        .count()
}

/// The `speakers` argument of a mix captioning: audio lanes such as ["A1", "A2"], or None.
fn speakers_arg(s: &PlaybackSession, args: &Value) -> Result<Option<Vec<Lane>>, String> {
    let Some(v) = args.get("speakers").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let arr = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "`speakers` must be a non-empty list of audio lane names such as [\"A1\", \"A2\"]".to_string())?;
    arr.iter()
        .map(|x| {
            let name = x
                .as_str()
                .ok_or_else(|| "`speakers` lists lane names such as [\"A1\", \"A2\"]".to_string())?;
            let lane = parse::lane(name)?;
            if lane.kind != LaneKind::Audio || !s.lanes().contains(&lane) {
                return Err(format!("{name} is not an audio lane on this timeline: speakers are audio lanes such as A1"));
            }
            Ok(lane)
        })
        .collect::<Result<Vec<Lane>, String>>()
        .map(Some)
}

/// The speaker of the clip span covering `mid` that started last (a lower lane on a tie), as its
/// index in `spans`' lanes.
fn covering_speaker(spans: &[(u32, u32, usize)], mid: u32) -> Option<usize> {
    spans
        .iter()
        .filter(|&&(a, b, _)| a <= mid && mid < b)
        .max_by_key(|&&(a, _, i)| (a, std::cmp::Reverse(i)))
        .map(|&(_, _, i)| i)
}

/// Each word's speaker, with a word no speaker covers taking the one before it (the first speaker
/// for leading words), so a gap between clips never splits a cue. Owners are 0 when nothing covers.
fn fill_owners(raw: &[Option<usize>]) -> Vec<u32> {
    let mut last = raw.iter().flatten().next().copied().unwrap_or(0);
    raw.iter()
        .map(|o| {
            if let Some(i) = o {
                last = *i;
            }
            last as u32
        })
        .collect()
}

/// The speaker lanes of a mix captioning and each word's speaker index into them. The lanes are
/// `speakers` as named, else every audio lane with clips. A word belongs to the speaker whose clip
/// covers its middle (see [`covering_speaker`]). `frames` are the words' (start, end) timeline frames.
fn speaker_owners(s: &PlaybackSession, speakers: Option<&[Lane]>, frames: &[(u32, u32)]) -> (Vec<Lane>, Vec<u32>) {
    let lanes: Vec<Lane> = match speakers {
        Some(l) => l.to_vec(),
        None => s
            .lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Audio && !s.lane_clips(*l).is_empty())
            .collect(),
    };
    let mut spans: Vec<(u32, u32, usize)> = Vec::new();
    for (i, &l) in lanes.iter().enumerate() {
        for c in s.lane_clips(l) {
            let end = c.start.saturating_add(c.timeline_offset(c.out_frame.saturating_sub(c.in_frame)));
            spans.push((c.start, end, i));
        }
    }
    let raw: Vec<Option<usize>> = frames
        .iter()
        .map(|&(a, b)| covering_speaker(&spans, ((u64::from(a) + u64::from(b)) / 2) as u32))
        .collect();
    (lanes, fill_owners(&raw))
}

/// The tracks a merge takes, and their cues on the timeline clock.
type MergePlan = (Vec<usize>, Vec<subtitle::Cue>);

/// The caption lane's tracks already on it, as the merge of a new captioning: their track indices
/// and their cues on the timeline clock. `None` when the lane holds no captions. Refused when a
/// track is also placed on another caption lane (removing it would take that placement too) or
/// holds pictures, which an SRT cannot carry.
fn merge_plan(s: &PlaybackSession, lane: Lane) -> Result<Option<MergePlan>, String> {
    let mut tracks: Vec<usize> = s.sub_lane(lane).iter().map(|c| c.track).collect();
    tracks.sort_unstable();
    tracks.dedup();
    if tracks.is_empty() {
        return Ok(None);
    }
    let name = parse::lane_name(lane);
    for &t in &tracks {
        if s.subtitle_lanes().iter().any(|&l| l != lane && s.sub_lane(l).iter().any(|c| c.track == t)) {
            return Err(format!(
                "auto_captions cannot merge into {name}: track {t} is also placed on another caption lane; sub_delete its other placement first"
            ));
        }
    }
    let mut cues = Vec::new();
    for sp in s.sub_lane_cue_spans(lane) {
        let (a, b, t, c) = (sp.start_us, sp.end_us, sp.track, sp.cue);
        let Some(cue) = s.subtitles().get(t).and_then(|tr| tr.cues.get(c)) else {
            continue;
        };
        if cue.image.is_some() {
            return Err(format!("auto_captions cannot merge into {name}: track {t} holds pictures, which an SRT cannot carry"));
        }
        cues.push(subtitle::Cue { start_us: a, end_us: b, text: cue.text.clone(), image: None, words: Vec::new(), karaoke: Vec::new() });
        let last = cues.last_mut().expect("just pushed");
        last.words = subtitle::map_words(&cue.words, sp.window, (a, b));
    }
    Ok(Some((tracks, cues)))
}

/// True when an existing cue is the same speech as one of the new cues: it overlaps them in time
/// and most of its words are heard again there. A re-run of auto_captions on the same target
/// (after a cut, say) writes these again, so the old ones are replaced. Another speaker's cues
/// overlap in time but carry other words, so they stay.
fn heard_again(old: &subtitle::Cue, new: &[subtitle::Cue]) -> bool {
    let old_words: Vec<String> = old.text.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
    if old_words.is_empty() {
        return false;
    }
    let heard: Vec<String> = new
        .iter()
        .filter(|c| c.start_us < old.end_us && old.start_us < c.end_us)
        .flat_map(|c| c.text.split_whitespace().map(norm))
        .collect();
    let found = old_words.iter().filter(|w| heard.contains(w)).count();
    found * 4 >= old_words.len() * 3
}

/// What a run does to the captions already on its lane. `replaced` are the old captions the run
/// hears again (the same speech, replaced); `edited` of those have a word the run does not hear
/// where they were, so their hand edits are lost. `differing` are the overlapping captions that
/// say something else: another speaker's, or hand edits. They are kept beside the new cues, unless
/// the run replaces them, in which case they are in `removed` and named.
#[derive(Debug, Default)]
struct Sweep {
    replaced: usize,
    edited: usize,
    differing: usize,
    kept: Vec<subtitle::Cue>,
    removed: Vec<subtitle::Cue>,
}

/// Splits the old captions of a lane against the cues a run is about to write. The captions the run
/// covers are the ones that overlap its span. One heard again in the run is the same speech and is
/// replaced. One that overlaps without being heard again is kept (another speaker's, or a hand
/// edit), unless `replace` is set, which removes it too. Never refuses. Changes nothing itself.
fn replace_plan(old: Vec<subtitle::Cue>, new: &[subtitle::Cue], replace: bool) -> Sweep {
    let lo = new.iter().map(|c| c.start_us).min().unwrap_or(0);
    let hi = new.iter().map(|c| c.end_us).max().unwrap_or(0);
    let covers = |c: &subtitle::Cue| c.start_us < hi && lo < c.end_us;
    let mut sweep = Sweep::default();
    for c in old {
        if !covers(&c) {
            sweep.kept.push(c);
        } else if heard_again(&c, new) {
            sweep.replaced += 1;
            // A replaced caption with a word the run does not hear where it was differs from it.
            let heard: Vec<String> = new
                .iter()
                .filter(|n| n.start_us < c.end_us && c.start_us < n.end_us)
                .flat_map(|n| n.text.split_whitespace().map(norm))
                .collect();
            if c.text.split_whitespace().map(norm).any(|w| !w.is_empty() && !heard.contains(&w)) {
                sweep.edited += 1;
            }
        } else {
            sweep.differing += 1;
            if replace {
                sweep.removed.push(c);
            } else {
                sweep.kept.push(c);
            }
        }
    }
    sweep
}

/// Names captions for a reply: their text (cut short) and their span in seconds.
fn cue_names(cues: &[subtitle::Cue]) -> String {
    cues.iter()
        .map(|c| {
            let text: String = c.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
            format!("\"{text}\" ({:.3}s–{:.3}s)", c.start_us as f64 / 1e6, c.end_us as f64 / 1e6)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn captions_run(srv: &mut Server, args: &Value) -> Result<Reply, String> {
    let (fps, want, model, lang, hint, label, lane, max_chars, max_secs, out, speakers) = {
        let s = session(srv)?;
        let fps = parse::session_fps(s);
        let want = want_arg(s, args)?;
        check_sources_online(s, &want)?;
        let model = model_arg(args)?;
        let lang = language_arg(args)?;
        let hint = hint_arg(args)?;
        let label = want_label(s, &want);
        let lane_name = opt_str(args, "lane").unwrap_or("S1").trim().to_ascii_uppercase();
        let lane = parse::lane(&lane_name)?;
        if lane.kind != LaneKind::Subtitle {
            return Err(format!("{lane_name} is not a caption lane; captions go on S1.."));
        }
        // Named here, before anything is heard, so a locked lane costs nothing and says why.
        if s.lane_locked(lane) {
            return Err(format!("auto_captions refused: {}", lock_refusal(lane)));
        }
        let speakers = speakers_arg(s, args)?;
        if speakers.is_some() && !matches!(want, Want::Mix) {
            return Err("`speakers` applies to a mix target: one lane's captions have one speaker".into());
        }
        let max_chars = opt_u64(args, "max_chars").unwrap_or(42);
        if !(10..=120).contains(&max_chars) {
            return Err("`max_chars` must be between 10 and 120".into());
        }
        let max_secs = opt_f64(args, "max_secs").unwrap_or(6.0);
        if !(0.5..=30.0).contains(&max_secs) {
            return Err("`max_secs` must be between 0.5 and 30".into());
        }
        let out = match opt_str(args, "path") {
            Some(p) => expand_home(p),
            None => match &srv.path {
                Some(p) => {
                    let stem = p.file_stem().map_or_else(|| "project".to_string(), |x| x.to_string_lossy().into_owned());
                    free_caption_path(s, p.with_file_name(format!("{stem}.captions.srt")))
                }
                None => free_caption_path(s, std::env::temp_dir().join(format!("edith-captions-{}.srt", std::process::id()))),
            },
        };
        (fps, want, model, lang, hint, label, lane_name, max_chars, max_secs, out, speakers)
    };
    let is_mix = matches!(want, Want::Mix);
    let replace = opt_bool(args, "replace").unwrap_or(false);
    let t = transcribe(session(srv)?, want.clone(), None, &model, &lang, &hint)?;
    let s = session(srv)?;
    // Captions carry the words people say: disfluencies are left out, even when the
    // transcript kept them (a hint or the default model can still hear an "um").
    let us = |f: u32| (f64::from(f) / fps * 1e6).round() as i64;
    let mut words: Vec<(i64, i64, String, f64)> = Vec::new();
    let mut frames: Vec<(u32, u32)> = Vec::new();
    for w in t.segs.iter().flat_map(|g| g.words.iter()).filter(|w| !CAPTION_FILLERS.contains(&norm(&w.w).as_str())) {
        words.push((us(w.start_f), us(w.end_f), w.w.clone(), w.p));
        frames.push((w.start_f, w.end_f));
    }
    if words.is_empty() {
        return Err(format!("no speech to caption in {label}: the transcript has no words"));
    }
    // A mix's words belong to the speaker whose clip plays them, and its cues split at each change.
    let (speaker_lanes, owners) = if is_mix {
        speaker_owners(s, speakers.as_deref(), &frames)
    } else {
        (Vec::new(), vec![0; words.len()])
    };
    let changes = speaker_changes(&words, &owners);
    let confident = confident_words(&t.segs);
    let (mut cues, mut skipped, short) = caption_cues(&words, &owners, max_chars as usize, max_secs, CAPTION_GAP_SECS);
    if cues.is_empty() {
        return Err(format!("no cues came out of the {} words in {label}", words.len()));
    }
    // Every heard segment becomes a cue or is named as skipped. The word clock can end a segment's
    // last word before the segment does (an approximate or early word time), so the cue holding
    // that last word runs to the segment's own end, up to the next cue.
    for g in &t.segs {
        let kept: Vec<&Word> = g.words.iter().filter(|w| !CAPTION_FILLERS.contains(&norm(&w.w).as_str())).collect();
        let Some(last) = kept.iter().copied().max_by_key(|w| w.start_f) else {
            if !g.words.is_empty() {
                skipped.push(format!("\"{}\" ({:.3}s–{:.3}s): only disfluencies", g.text.trim(), us(g.start_f) as f64 / 1e6, us(g.end_f) as f64 / 1e6));
            }
            continue;
        };
        let (ws, se) = (us(last.start_f), us(g.end_f.max(last.end_f)));
        if let Some(i) = cues.iter().rposition(|c| c.start_us <= ws) {
            let cap = cues.get(i + 1).map_or(i64::MAX, |n| n.start_us);
            let end = se.min(cap);
            if end > cues[i].end_us {
                cues[i].end_us = end;
            }
        }
    }

    // A cue over silence on the target is not speech (Whisper writes words into quiet audio): it is
    // dropped, the same check find_words makes on its matches, and the reply counts it.
    let (mut cues, silent) = drop_silent_cues(s, &want, cues, fps)?;
    if cues.is_empty() {
        return Err(format!(
            "every caption in {label} falls over silence: {} cue(s) dropped ({}); nothing written",
            silent.len(),
            cue_names(&silent)
        ));
    }

    // A cue that begins a sentence starts with a capital, even where a dropped filler came before it.
    let starts: Vec<i64> = t
        .segs
        .iter()
        .filter_map(|g| {
            g.words
                .iter()
                .filter(|w| !CAPTION_FILLERS.contains(&norm(&w.w).as_str()))
                .min_by_key(|w| w.start_f)
                .map(|w| us(w.start_f))
        })
        .collect();
    capitalise_sentence_starts(&mut cues, &starts);
    attach_word_times(&mut cues, &words);

    // The captions already on this lane are merged in: one track holds them and the new cues, in
    // time order, and the tracks they came from are removed (one undo step with the rest).
    let lane_val = parse::lane(&lane)?;
    let mut merged = merge_plan(s, lane_val)?;
    // The captions this run covers are the ones that overlap its span. An old caption heard again in
    // the run is the same speech and is replaced. One that overlaps but says something else (another
    // speaker's, or a hand edit) is kept beside the new cues; replace removes it too.
    let mut sweep = Sweep::default();
    if let Some((_, old)) = merged.as_mut() {
        sweep = replace_plan(std::mem::take(old), &cues, replace);
        *old = std::mem::take(&mut sweep.kept);
    }
    let start = session(srv)?.history_pushes();
    let placed: Result<usize, String> = (|| {
        let s = srv.session().map_err(|r| r.text)?;
        if s.subtitles().iter().any(|t| same_file(&t.path, &out)) {
            return Err(format!(
                "{} already holds a subtitle track on this timeline; pass another path",
                out.display()
            ));
        }
        if let Some((tracks, _)) = &merged {
            for &t in tracks.iter().rev() {
                let op = json!({"op": "captions_remove", "track": t, "force": true});
                match edit_subs::apply(s, &op, fps) {
                    Some(r) => {
                        r?;
                    }
                    None => return Err("captions_remove is not available".into()),
                }
            }
        }
        let mut all = cues.clone();
        if let Some((_, old)) = &merged {
            all.extend(old.iter().cloned());
            all.sort_by_key(|c| c.start_us);
        }
        subtitle::save_cues(&out, &all).map_err(|e| format!("writing {}: {e}", out.display()))?;
        let added = s.import_subtitles(&out).map_err(|e| format!("importing {}: {e}", out.display()))?;
        if added == 0 {
            return Err(format!("importing {} added no subtitle track", out.display()));
        }
        let track = s
            .subtitles()
            .iter()
            .rposition(|t| same_file(&t.path, &out))
            .ok_or_else(|| format!("{} was imported but no track for it was found", out.display()))?;
        // The transcript's word times ride along: the SRT cannot hold them.
        s.attach_cue_words(track, &all);
        let op = json!({"op": "captions_place", "track": track, "lane": lane, "at": 0});
        match edit_subs::apply(s, &op, fps) {
            Some(r) => r?,
            None => return Err("captions_place is not available".into()),
        };
        Ok(track)
    })();
    let track = match placed {
        Ok(v) => v,
        Err(e) => {
            // Roll back whatever the failed attempt pushed, exactly that many steps.
            let rolled = roll_back_since(srv, start);
            let note = if rolled { "" } else { " (could not roll every step back; check the project)" };
            return Err(format!("{e}; nothing changed{note}"));
        }
    };
    let merged_cues = merged.as_ref().map_or(0, |(_, old)| old.len());
    if let Some(s) = srv.session.as_ref() {
        let n = s.history_pushes().saturating_sub(start) as usize;
        if n > 0 {
            srv.ledger.push(Entry::new(n, format!("auto_captions {lane} ({} cues)", cues.len() + merged_cues)));
            srv.history_lost_on_open = false;
            srv.redo_ledger.clear();
            srv.trim_ledger();
        }
    }
    // The placement shows the track from its first microsecond to its last cue; the captions
    // themselves start and stop at their cue times, so those are the span to report.
    let first_s = cues.iter().map(|c| c.start_us).min().unwrap_or(0) as f64 / 1e6;
    let last_s = cues.iter().map(|c| c.end_us).max().unwrap_or(0) as f64 / 1e6;
    let mut text = format!(
        "auto_captions: {} captions on {lane} from {first_s:.3}s to {last_s:.3}s (track {track} for burn_captions). SRT: {}\n\
         The SRT is a snapshot of the captions at this time: later cue edits (cue_text, cue_delete, cue_replace) are kept in the project and saved with it, not written back to that file.",
        cues.len(),
        out.display()
    );
    if !silent.is_empty() {
        text.push_str(&format!(
            "\ndropped {} caption(s) over silence on {label} (Whisper writes words into quiet audio): {}",
            silent.len(),
            cue_names(&silent)
        ));
    }
    if let Some((tracks, old)) = &merged {
        let was: Vec<String> = tracks.iter().map(|t| t.to_string()).collect();
        let total = cues.len() + old.len();
        let kept = if old.is_empty() {
            String::new()
        } else {
            format!(" with the {} caption(s) already on {lane}", old.len())
        };
        text.push_str(&format!(
            "\nmerged {} new cue(s){kept} (track {} removed): {total} captions on {lane} in all, in time order",
            cues.len(),
            was.join(", ")
        ));
    }
    // Always stated, also when nothing was replaced or differed.
    let edits = if sweep.edited > 0 {
        format!(" ({} with hand edits, which are lost; undo brings them back)", sweep.edited)
    } else {
        String::new()
    };
    text.push_str(&format!(
        "\nreplaced {} caption(s) on {lane} that this run heard again{edits}",
        sweep.replaced
    ));
    let differ = "overlapping caption(s) differ from what this run heard (another speaker's or hand edits)";
    if replace && sweep.differing > 0 {
        text.push_str(&format!(
            "\n{} {differ}; replace:true removed {}: {}",
            sweep.differing,
            sweep.differing,
            cue_names(&sweep.removed)
        ));
    } else if replace {
        text.push_str(&format!("\n0 {differ}; replace:true removed none"));
    } else {
        text.push_str(&format!("\nkept {} {differ}, beside the new cues", sweep.differing));
    }
    if merged.is_none() && !is_mix {
        text.push_str(&format!(
            "\neach speaker lane can be captioned into one lane: auto_captions {{target:\"A2\", lane:\"{lane}\"}} merges that speaker's cues into {lane}, in time order, so burn_captions draws both"
        ));
    }
    if changes > 0 {
        // Only the lanes that speak a word here are named, not every lane offered as a speaker.
        let mut used: Vec<u32> = owners.clone();
        used.sort_unstable();
        used.dedup();
        let names: Vec<String> = used.iter().filter_map(|&i| speaker_lanes.get(i as usize)).map(|&l| parse::lane_name(l)).collect();
        text.push_str(&format!(
            "\nsplit at {changes} speaker change(s) ({}): no cue spans two speakers",
            names.join(", ")
        ));
    }
    if !skipped.is_empty() {
        text.push_str(&format!("\nskipped, not captioned: {}", skipped.join("; ")));
    }
    if !short.is_empty() {
        text.push_str(&format!(
            "\n{} cue(s) under {CUE_MIN_SECS} s, forced by the limits (max_secs {max_secs}, max_chars {max_chars}): {}",
            short.len(),
            short.join("; ")
        ));
    }
    let heard = review_segs(&t.segs, &silent, us);
    text.push_str(&review_text(&heard, fps));
    let fixes = hint_note(&t.corrections);
    if !fixes.is_empty() {
        text.push('\n');
        text.push_str(&fixes);
    }
    if confident < CAPTION_MIN_CONFIDENT {
        text.push_str(&format!(
            "\nwarning: only {confident} confident word(s) of {} heard in {label}: it may have no speech, so these captions may be guesses. Check them against the audio before use.",
            words.len()
        ));
    }
    Ok(Reply::ok(text).with(json!({
        "cues": cues.len(),
        "words": words.len(),
        "confident_words": confident,
        "track": track,
        "lane": lane,
        "path": out.display().to_string(),
        "merged_cues": merged_cues,
        "replaced": sweep.replaced,
        "differing": sweep.differing,
        "silent_dropped": silent.len(),
        "speaker_changes": changes,
        "skipped": skipped,
        "short_cues": short,
    })))
}

// ---- hint names -------------------------------------------------------------

/// Common English words a single-word hint term never takes the place of. A short term sounds
/// like many of them ("Theo" and "the"), and a plain word is far likelier to be what was said.
const COMMON_WORDS: &[&str] = &[
    "a", "about", "above", "across", "after", "again", "against", "all", "also", "always", "am", "an", "and", "another",
    "any", "anything", "are", "around", "as", "at", "back", "be", "because", "been", "before", "being", "below", "between",
    "both", "but", "by", "can", "come", "could", "did", "do", "does", "done", "down", "during", "each", "either", "else",
    "end", "enough", "even", "ever", "every", "everyone", "everything", "far", "few", "for", "from", "get", "gets", "give",
    "go", "goes", "going", "gone", "good", "got", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his",
    "how", "however", "i", "if", "in", "into", "is", "it", "its", "just", "keep", "kind", "know", "last", "let", "like",
    "little", "long", "look", "lose", "lost", "lot", "made", "make", "many", "may", "me", "might", "mine", "more", "most",
    "much", "must", "my", "near", "need", "never", "new", "next", "no", "nobody", "none", "nor", "not", "nothing", "now",
    "of", "off", "often", "oh", "ok", "okay", "on", "once", "one", "only", "or", "other", "our", "out", "over", "own",
    "part", "people", "perhaps", "place", "please", "put", "quite", "rather", "really", "right", "said", "same", "say",
    "see", "seem", "she", "should", "since", "so", "some", "someone", "something", "still", "such", "sure", "take", "than",
    "thank", "thanks", "that", "the", "their", "them", "then", "there", "these", "they", "thing", "think", "this", "those",
    "though", "through", "till", "time", "to", "too", "took", "try", "two", "under", "until", "up", "upon", "us", "use",
    "very", "via", "want", "was", "way", "we", "well", "were", "what", "when", "where", "whether", "which", "while", "who",
    "whom", "whose", "why", "will", "with", "within", "without", "would", "yeah", "yes", "yet", "you", "your", "um", "uh",
    "er", "ah", "hmm", "mm",
];

/// Largest normalised edit distance (over the longer spelling's letters) at which a heard run is
/// a spelling of a hint term.
const HINT_EDIT_MAX: f64 = 0.34;

/// Fewest letters a hint term and a heard run need for a fuzzy or sound-alike match. An exact
/// spelling, ignoring case, needs none.
const HINT_MIN_LETTERS: usize = 4;

/// Most words a hint term spans, and so the longest heard run that is matched to one.
const HINT_MAX_WORDS: usize = 3;

/// One hint term: its words as spelt (punctuation off the ends) and their letters, lower case.
struct HintTerm {
    words: Vec<String>,
    letters: String,
}

/// The letters of `s`, lower case, with everything else left out.
fn letters(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The word `s` with punctuation off its ends.
fn core_word(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_alphanumeric())
}

/// The terms of a hint prompt: its comma-separated names and jargon. The filler prompt that
/// `with_fillers` puts in front is not a term; a term of more than [`HINT_MAX_WORDS`] words is not one either.
fn hint_terms(prompt: &str) -> Vec<HintTerm> {
    let body = prompt.strip_prefix(FILLER_PROMPT).unwrap_or(prompt);
    body.split([',', ';', '\n'])
        .filter_map(|term| {
            let words: Vec<String> = term.split_whitespace().map(core_word).filter(|w| !w.is_empty()).map(str::to_string).collect();
            (!words.is_empty() && words.len() <= HINT_MAX_WORDS).then(|| HintTerm { letters: letters(&words.concat()), words })
        })
        .collect()
}

/// Levenshtein distance between two strings, in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The four-character Soundex key of a word (letters only, lower case): its first letter, then
/// codes for the consonants that follow, with repeats and H or W between equal codes dropped.
fn soundex(word: &str) -> String {
    let mut out = String::new();
    let mut last = 0u8;
    for c in word.chars().filter(char::is_ascii_alphabetic) {
        let up = c.to_ascii_uppercase();
        let code = match up {
            'B' | 'F' | 'P' | 'V' => 1,
            'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => 2,
            'D' | 'T' => 3,
            'L' => 4,
            'M' | 'N' => 5,
            'R' => 6,
            'H' | 'W' => continue,
            _ => 0,
        };
        if out.is_empty() {
            out.push(up);
        } else if code != 0 && code != last {
            out.push(char::from(b'0' + code));
        }
        last = code;
        if out.len() == 4 {
            break;
        }
    }
    if out.is_empty() {
        return out;
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

/// Whether a heard run (its letters, lower case) is a spelling of a hint term (its letters): the
/// same letters, or an edit distance within [`HINT_EDIT_MAX`], or the same first letter and Soundex key.
/// Only exact matches may be short; a fuzzy or sound-alike match needs [`HINT_MIN_LETTERS`] letters on both sides.
fn is_spelling_of(heard: &str, term: &str) -> bool {
    if heard == term {
        return true;
    }
    let (hn, tn) = (heard.chars().count(), term.chars().count());
    if hn.min(tn) < HINT_MIN_LETTERS {
        return false;
    }
    if edit_distance(heard, term) as f64 / hn.max(tn) as f64 <= HINT_EDIT_MAX {
        return true;
    }
    heard.chars().next() == term.chars().next() && soundex(heard) == soundex(term)
}

/// `orig` with its core word replaced by `word`, the punctuation around it kept ("Dough," becomes "Doe,").
fn with_affixes(orig: &str, word: &str) -> String {
    let core = core_word(orig);
    let lead = orig.find(core).unwrap_or(0);
    let tail = &orig[lead + core.len()..];
    format!("{}{word}{tail}", &orig[..lead])
}

/// Respells, in place, each run of `tokens` that is a spelling of a hint term. A run has as many
/// tokens as the term has words, so each word keeps its own token and its own timing. A
/// single-word term never takes a common English word. Returns each change as (heard, term).
fn respell(tokens: &mut [String], terms: &[HintTerm]) -> Vec<(String, String)> {
    let mut changes = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let hit = terms.iter().find(|t| {
            let n = t.words.len();
            if i + n > tokens.len() {
                return false;
            }
            let run = &tokens[i..i + n];
            if run.iter().any(|w| letters(w).is_empty()) {
                return false;
            }
            if n == 1 && COMMON_WORDS.contains(&letters(&run[0]).as_str()) {
                return false;
            }
            is_spelling_of(&letters(&run.concat()), &t.letters)
        });
        let Some(term) = hit else {
            i += 1;
            continue;
        };
        let n = term.words.len();
        let heard: Vec<&str> = tokens[i..i + n].iter().map(|w| core_word(w)).collect();
        // A run already spelt as the term is not a change.
        if heard.iter().copied().ne(term.words.iter().map(String::as_str)) {
            changes.push((heard.join(" "), term.words.join(" ")));
            for (k, word) in term.words.iter().enumerate() {
                tokens[i + k] = with_affixes(&tokens[i + k], word);
            }
        }
        i += n;
    }
    changes
}

/// Respells the names and jargon in each segment's words (and its text) to the hint terms in
/// `prompt` (see [`respell`]). Returns each change once, as (heard, term, count), in the order it
/// was first heard. The words keep their times; only their spelling moves.
fn correct_hints(segs: &mut [Seg], prompt: &str) -> Vec<(String, String, usize)> {
    let terms = hint_terms(prompt);
    let mut counts: Vec<(String, String, usize)> = Vec::new();
    if terms.is_empty() {
        return counts;
    }
    let mut count = |changes: Vec<(String, String)>| {
        for (from, to) in changes {
            match counts.iter_mut().find(|c| c.0 == from && c.1 == to) {
                Some(c) => c.2 += 1,
                None => counts.push((from, to, 1)),
            }
        }
    };
    for seg in segs.iter_mut() {
        let mut heard: Vec<String> = seg.words.iter().map(|w| w.w.clone()).collect();
        let word_changes = respell(&mut heard, &terms);
        let has_words = !seg.words.is_empty();
        for (w, new) in seg.words.iter_mut().zip(heard) {
            w.w = new;
        }
        let mut tokens: Vec<String> = seg.text.split_whitespace().map(str::to_string).collect();
        let text_changes = respell(&mut tokens, &terms);
        if !text_changes.is_empty() {
            seg.text = tokens.join(" ");
        }
        // The words are counted; a segment with no word times is counted by its text.
        count(if has_words { word_changes } else { text_changes });
    }
    counts
}

/// The hint corrections as JSON: each heard spelling, the term it became, and how many times.
fn corrections_json(fixes: &[(String, String, usize)]) -> Value {
    fixes.iter().map(|(heard, to, n)| json!({"heard": heard, "to": to, "n": n})).collect::<Vec<_>>().into()
}

/// The reply line for hint corrections, empty when there were none.
fn hint_note(fixes: &[(String, String, usize)]) -> String {
    if fixes.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = fixes.iter().map(|(from, to, n)| format!("'{from}' → '{to}' ×{n}")).collect();
    format!("hint corrected: {}", parts.join(", "))
}

// ---- cut edges --------------------------------------------------------------

/// How far either side of a cut edge its pause is looked for, in seconds.
const SNAP_SECS: f64 = 0.15;

/// The length of one block of the level curve a cut edge snaps over.
const SNAP_BLOCK_SECS: f64 = 0.01;

/// The least distance a snapped edge keeps from the kept word on its side, in seconds.
const SNAP_MARGIN_SECS: f64 = 0.03;

/// The mean level in dB of each block of `samples` (16 kHz mono, [`SNAP_BLOCK_SECS`] long).
fn block_db(samples: &[f32]) -> Vec<f64> {
    let n = (f64::from(RATE) * SNAP_BLOCK_SECS).round() as usize;
    samples
        .chunks(n)
        .map(|b| {
            let ms = b.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / b.len() as f64;
            10.0 * (ms + 1e-12).log10()
        })
        .collect()
}

/// Each block's level paired with the timeline frame its centre lies at. `dbs` start at `origin`
/// seconds of the audio as read, and `map` says how that audio sits on the timeline.
fn level_frames(dbs: &[f64], origin: f64, fps: f64, map: Map) -> Vec<(u32, f64)> {
    dbs.iter()
        .enumerate()
        .map(|(j, db)| (timeline_frame(map, origin + (j as f64 + 0.5) * SNAP_BLOCK_SECS, fps), *db))
        .collect()
}

/// The frame an edge moves to: the quietest block in silence (at or below `GAP_AUDIBLE_DB`) whose
/// frame lies in `lo..=hi`, the nearest to `edge` among equally quiet ones. Only a pause is taken:
/// a dip inside speech is not a place to cut, so a range with no silence gives `None` and the edge
/// stays where Whisper put it.
fn quietest_frame(levels: &[(u32, f64)], edge: u32, lo: u32, hi: u32) -> Option<u32> {
    levels
        .iter()
        .filter(|(f, db)| (lo..=hi).contains(f) && *db <= GAP_AUDIBLE_DB)
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.abs_diff(edge).cmp(&b.0.abs_diff(edge))))
        .map(|(f, _)| *f)
}

/// The frames an edge may move to: `half` each side of it, and `margin` clear of the neighbouring
/// word on its side (`before` for a start, `after` for an end). No neighbour leaves that side open.
fn snap_range(edge: u32, half: u32, before: Option<u32>, after: Option<u32>, margin: u32) -> (u32, u32) {
    let lo = edge.saturating_sub(half).max(before.map_or(0, |e| e.saturating_add(margin)));
    let hi = edge.saturating_add(half).min(after.map_or(u32::MAX, |s| s.saturating_sub(margin)));
    (lo, hi)
}

/// The levels of the target's sound over timeline frames `from..to`, one per block. Empty when no
/// clip of the target covers them, or the audio cannot be read: then nothing moves.
fn edge_levels(s: &PlaybackSession, ground: &Ground, from: u32, to: u32, fps: f64) -> Result<Vec<(u32, f64)>, String> {
    let (samples, origin, map) = match ground {
        Ground::Mix(wav) => (decode_16k(wav, 0, f64::from(from) / fps, f64::from(to) / fps)?, f64::from(from) / fps, Map::Mix),
        Ground::Source(path, stream) => (decode_16k(path, *stream, f64::from(from) / fps, f64::from(to) / fps)?, f64::from(from) / fps, Map::Mix),
        Ground::Clips(clips) => {
            let hit = clips.iter().find_map(|(_, c)| clip_source_span(c, Some((from, to))).map(|span| (c, span)));
            let Some((clip, (lo, hi))) = hit else {
                return Ok(Vec::new());
            };
            let Some(src) = s.sources().get(clip.source) else {
                return Ok(Vec::new());
            };
            let map = Map::of(clip);
            (decode_16k(&src.path, src.audio_stream, f64::from(lo) / fps, f64::from(hi) / fps)?, f64::from(lo) / fps, map)
        }
    };
    let Some(samples) = samples else {
        return Ok(Vec::new());
    };
    Ok(level_frames(&block_db(&samples), origin, fps, map))
}

/// One edge moved to its pause (see [`quietest_frame`]), or `None` when no sound is read under it.
fn snap_edge(s: &PlaybackSession, ground: &Ground, edge: u32, before: Option<u32>, after: Option<u32>, fps: f64) -> Result<Option<u32>, String> {
    let half = (SNAP_SECS * fps).round() as u32;
    let margin = (SNAP_MARGIN_SECS * fps).round() as u32;
    let levels = edge_levels(s, ground, edge.saturating_sub(half), edge.saturating_add(half), fps)?;
    if levels.is_empty() {
        return Ok(None);
    }
    let (lo, hi) = snap_range(edge, half, before, after, margin);
    Ok(Some(quietest_frame(&levels, edge, lo, hi).unwrap_or(edge)))
}

/// The cut spans after their edges moved to quiet points, and how many edges were read and moved.
#[derive(Debug, Default)]
struct Snapped {
    spans: Vec<(u32, u32)>,
    edges: usize,
    moved: usize,
    frames: u64,
}

/// Each span's start and end moved to a pause near it (see [`snap_edge`]). A start stays
/// clear of the word that ends before it, an end of the word that starts after it, by
/// [`SNAP_MARGIN_SECS`]. A span whose edges would cross keeps its Whisper times. `words` are every
/// word of the target as (start, end) frames.
fn snap_spans(s: &PlaybackSession, ground: &Ground, words: &[(u32, u32)], spans: &[(u32, u32)], fps: f64) -> Result<Snapped, String> {
    let mut out = Snapped::default();
    for &(a, b) in spans {
        let before = words.iter().filter(|w| w.1 <= a).map(|w| w.1).max();
        let after = words.iter().filter(|w| w.0 >= b).map(|w| w.0).min();
        let ra = snap_edge(s, ground, a, before, None, fps)?;
        let rb = snap_edge(s, ground, b, None, after, fps)?;
        let (na, nb) = (ra.unwrap_or(a), rb.unwrap_or(b));
        let (na, nb) = if na < nb { (na, nb) } else { (a, b) };
        for (old, read, new) in [(a, ra.is_some(), na), (b, rb.is_some(), nb)] {
            if read {
                out.edges += 1;
                out.moved += usize::from(old != new);
                out.frames += u64::from(old.abs_diff(new));
            }
        }
        out.spans.push((na, nb));
    }
    Ok(out)
}

/// The reply line for edge snapping: the mean move over the edges read, and how many moved.
fn snap_note(sn: &Snapped, fps: f64) -> String {
    if sn.edges == 0 {
        return "edges not snapped: no sound under them, so Whisper's times stand".into();
    }
    let avg_ms = sn.frames as f64 / sn.edges as f64 / fps * 1000.0;
    format!("edges snapped to quiet points: avg {avg_ms:.0} ms ({} of {} moved; the rest had no pause in reach)", sn.moved, sn.edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn cue(start_s: f64, end_s: f64, text: &str) -> subtitle::Cue {
        subtitle::Cue { start_us: (start_s * 1e6) as i64, end_us: (end_s * 1e6) as i64, text: text.into(), image: None, words: Vec::new(), karaoke: Vec::new() }
    }

    #[test]
    fn a_rerun_replaces_the_caption_it_hears_again_in_one_step() {
        let old = vec![cue(0.0, 2.0, "Hello there"), cue(5.0, 6.0, "later words")];
        let new = vec![cue(0.0, 2.0, "hello there")];
        let sweep = replace_plan(old, &new, false);
        assert_eq!(sweep.replaced, 1, "the caption heard again is replaced");
        assert_eq!(sweep.edited, 0, "the same words are not a hand edit");
        assert_eq!(sweep.differing, 0, "nothing overlapping differs");
        assert_eq!(sweep.kept.len(), 1, "a caption outside the run's span stays");
        assert_eq!(sweep.kept[0].text, "later words");
    }

    #[test]
    fn a_hand_edited_caption_is_kept_by_default_and_removed_by_replace() {
        let old = vec![cue(0.0, 2.0, "Hello there friend, edited")];
        let new = vec![cue(0.0, 2.0, "hello there")];
        let kept = replace_plan(old.clone(), &new, false);
        assert_eq!((kept.replaced, kept.differing, kept.kept.len()), (0, 1, 1), "kept beside the new cues");
        assert!(kept.removed.is_empty());
        let gone = replace_plan(old, &new, true);
        assert_eq!((gone.replaced, gone.differing, gone.kept.len()), (0, 1, 0), "replace:true removes it");
        assert_eq!(gone.removed.len(), 1);
        assert!(cue_names(&gone.removed).contains("Hello there friend, edited"), "the reply names it");
    }

    #[test]
    fn a_second_speakers_run_keeps_the_first_speakers_captions() {
        // Host captions over 0..5 s; the guest's run hears other words over the same stretch.
        let host = vec![cue(0.0, 2.0, "Welcome to the show"), cue(2.5, 4.5, "Today we talk chemistry")];
        let guest = [cue(0.5, 2.0, "Thanks for having me"), cue(3.0, 4.0, "Happy to be here")];
        let sweep = replace_plan(host.clone(), &guest, false);
        assert_eq!((sweep.replaced, sweep.differing), (0, 2), "both host cues differ from the guest's words");
        assert_eq!(sweep.kept.len(), 2, "both host cues stay beside the guest's");
        let sweep = replace_plan(host, &guest, true);
        assert_eq!(sweep.removed.len(), 2, "replace would remove them, and only with replace");
    }

    #[test]
    fn a_rerun_of_the_same_lane_replaces_what_it_hears_again_and_keeps_the_rest() {
        let old = vec![cue(0.0, 2.0, "Hello there"), cue(2.5, 4.0, "Our guest is here"), cue(9.0, 10.0, "Outside")];
        let new = vec![cue(0.0, 2.0, "hello there"), cue(2.5, 4.0, "our guest is here")];
        let sweep = replace_plan(old, &new, false);
        assert_eq!((sweep.replaced, sweep.edited, sweep.differing), (2, 0, 0));
        assert_eq!(sweep.kept.len(), 1, "only the caption outside the run stays");
        assert_eq!(sweep.kept[0].text, "Outside");
    }

    #[test]
    fn a_cue_over_silence_is_dropped_and_the_speech_kept() {
        // Frames 0..250 at 25 fps are 10 s: the first cue is over quiet audio, the second over speech.
        let cues = vec![cue(0.0, 2.0, "Faster Heady Board of Chemical Didnates"), cue(3.0, 4.0, "Hello there")];
        let (kept, silent) = split_by_quiet(cues, 25.0, |from, _| Ok(from < 50)).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "Hello there");
        assert_eq!(silent.len(), 1, "the cue over silence is counted");
        assert!(cue_names(&silent).contains("Faster Heady Board"));
    }

    #[test]
    fn a_cue_span_is_checked_in_frames_and_always_has_a_frame() {
        let mut seen = Vec::new();
        let cues = vec![subtitle::Cue { start_us: 1_000_000, end_us: 1_000_000, text: "x".into(), image: None, words: Vec::new(), karaoke: Vec::new() }];
        split_by_quiet(cues, 25.0, |from, to| {
            seen.push((from, to));
            Ok(false)
        })
        .unwrap();
        assert_eq!(seen, vec![(25, 26)], "an empty cue still spans a frame, at 25 fps one second is frame 25");
    }

    #[test]
    fn quiet_spans_are_silent_and_speech_is_not() {
        assert!(is_quiet(&[]));
        assert!(is_quiet(&vec![0.0; 1600]));
        let tone: Vec<f32> = (0..1600).map(|i| 0.3 * (i as f32 * 0.1).sin()).collect();
        assert!(!is_quiet(&tone), "a 0.3 amplitude tone is well above -40 dB");
    }

    #[test]
    fn a_source_read_names_its_own_clock_and_the_clips_that_play_it() {
        assert_eq!(source_clips_note(0, &[], 25.0), "\nsrc:0 times are the file's own clock; no clip on the timeline plays it");
    }

    #[test]
    fn range_arg_reads_seconds_on_a_placed_timeline() {
        // Two seconds at 30 fps is frame 60, whether written 2s, 2.0s or 0.0s.
        for (from, to) in [("0s", "2s"), ("0.0s", "2.0s"), ("0", "2")] {
            let args = json!({ "range": { "from": from, "to": to } });
            assert_eq!(range_arg(&args, 30.0, 300).unwrap(), Some((0, 60)), "{from}..{to}");
        }
        assert_eq!(range_arg(&json!({}), 30.0, 300).unwrap(), None);
    }

    #[test]
    fn range_arg_names_the_empty_timeline_instead_of_an_empty_range() {
        let args = json!({ "range": { "from": "0s", "to": "2s" } });
        let err = range_arg(&args, 30.0, 0).unwrap_err();
        assert!(err.contains("nothing is placed"), "{err}");
        assert!(!err.contains("range is empty"), "{err}");
    }

    #[test]
    fn source_count_note_says_how_many_sources_there_are() {
        assert_eq!(source_count_note(0), "this project has no sources");
        assert_eq!(source_count_note(1), "this project has 1 source (src:0)");
        assert_eq!(source_count_note(3), "this project has 3 sources (src:0..src:2)");
    }

    #[test]
    fn timeline_frame_maps_through_in_point_and_speed() {
        let normal = Map::Clip { start: 50, in_frame: 100, speed: Speed::NORMAL };
        // 4.0 s at 25 fps is source frame 100, the in point: the clip's first frame.
        assert_eq!(timeline_frame(normal, 4.0, 25.0), 50);
        // 6.0 s is source frame 150, fifty frames in at real time.
        assert_eq!(timeline_frame(normal, 6.0, 25.0), 100);
        // Double speed: fifty source frames take twenty-five timeline frames.
        let fast = Map::Clip { start: 50, in_frame: 100, speed: Speed::from_permille(2000) };
        assert_eq!(timeline_frame(fast, 6.0, 25.0), 75);
        // A mix is already on the timeline clock.
        assert_eq!(timeline_frame(Map::Mix, 6.0, 25.0), 150);
    }

    /// A word in a ramped clip lands where the ramp's integral puts it, not at the mean
    /// rate: 1x to 2x over 50 frames reads source frame `t + t^2/100`, so 25 frames of
    /// source in is timeline frame 21 (frame 20 has only reached 24), where the mean 1.5x
    /// would say 17.
    #[test]
    fn timeline_frame_maps_through_a_ramp() {
        use engine::project::{Clip, Speed, SpeedKey};
        let mut clip = Clip::source_range(0, 100, 250);
        clip.start = 50;
        clip.speed_keys = vec![
            SpeedKey { at: 0, speed: Speed::NORMAL },
            SpeedKey { at: 50, speed: Speed::from_permille(2000) },
        ];
        let map = Map::of(&clip);
        // The in point, then 25 source frames in.
        assert_eq!(timeline_frame(map, 4.0, 25.0), 50);
        assert_eq!(timeline_frame(map, 5.0, 25.0), 50 + 21);
        // The transcript window of the clip's first 30 timeline frames reads the ramp's
        // 30 + 9 = 39 source frames.
        assert_eq!(clip_source_span(&clip, Some((50, 80))), Some((100, 139)));
    }

    #[test]
    fn cache_key_follows_every_input() {
        let base = cache_key("/a.mp4", (7, 100), 0, "small", "auto", "");
        assert_eq!(base, cache_key("/a.mp4", (7, 100), 0, "small", "auto", ""));
        assert_ne!(base, cache_key("/a.mp4", (8, 100), 0, "small", "auto", ""));
        assert_ne!(base, cache_key("/a.mp4", (7, 101), 0, "small", "auto", ""));
        assert_ne!(base, cache_key("/a.mp4", (7, 100), 1, "small", "auto", ""));
        assert_ne!(base, cache_key("/a.mp4", (7, 100), 0, "tiny", "auto", ""));
        assert_ne!(base, cache_key("/a.mp4", (7, 100), 0, "small", "en", ""));
        assert_ne!(base, cache_key("/a.mp4", (7, 100), 0, "small", "auto", "Edith, LUFS"));
        assert_eq!(base.len(), 16);
    }

    #[test]
    fn cache_key_is_versioned_so_older_results_are_not_reused() {
        let k = cache_key("/a.mp4", (7, 100), 0, "small", "auto", "");
        let v5 = format!("{:016x}", fnv1a(b"v5|/a.mp4|7|100|0|small|auto|"));
        assert_ne!(k, v5);
        assert_eq!(CACHE_VERSION, 9);
    }

    #[test]
    fn only_the_exact_cached_range_is_served() {
        // A wider entry must not answer a narrower piece: its window splits differ from a fresh decode.
        let dir = crate::testutil::TestDir::new("listen-exact");
        let key = dir.join("k");
        assert_eq!(range_name(1.0, 9.0), "1000-9000.jsonl");
        write_entry(&key, 1.0, 9.0, "wide");
        write_entry(&key, 2.0, 4.0, "narrow");
        assert_eq!(read_exact(&key, 2.0, 4.0).as_deref(), Some("narrow"));
        assert_eq!(read_exact(&key, 1.0, 9.0).as_deref(), Some("wide"));
        assert_eq!(read_exact(&key, 2.5, 3.5), None, "a piece inside an entry is decoded fresh");
        assert_eq!(read_exact(&key, 5.0, 9.0), None);
        assert_eq!(read_exact(&dir.join("missing"), 1.0, 2.0), None);
    }

    #[test]
    fn a_cut_pads_each_side_but_stops_at_the_neighbouring_words() {
        // Neighbours end at 96 and start at 124: pad 3 each side.
        assert_eq!(cut_span(100, 120, 3, 96, 124, 1000), (97, 123));
        // The left neighbour ends at 99, so the pad stops there.
        assert_eq!(cut_span(100, 120, 3, 99, 121, 1000), (99, 121));
        // At the timeline's ends the pad stops at the timeline.
        assert_eq!(cut_span(2, 10, 5, 0, u32::MAX, 12), (0, 12));
    }

    #[test]
    fn overlapping_cut_spans_join_and_empty_ones_go() {
        assert_eq!(merge_spans(vec![(10, 20), (18, 30), (40, 45), (50, 50)]), vec![(10, 30), (40, 45)]);
    }

    #[test]
    fn a_filler_match_with_no_pause_around_it_is_named_as_a_real_word() {
        assert!(mid_phrase(1, 2, 30.0), "no pause either side, at 30 fps");
        assert!(!mid_phrase(1, 9, 30.0), "a 0.3 s pause after it is a pause");
        assert!(!mid_phrase(9, 1, 30.0), "a pause before it");
        assert!(!mid_phrase(u32::MAX, 1, 30.0), "the start of the transcript is a pause");
        assert!(is_filler_query(&["like".to_string()]));
        assert_eq!(
            real_word_note(0, "like a marathon"),
            "match #0 'like a marathon' looks like a real word (no pause around it); use which to skip it"
        );
    }

    #[test]
    fn a_known_filler_never_gets_the_real_word_note() {
        let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        for filler in ["um", "uh", "erm", "hmm", "ah", "er", "uhm"] {
            assert!(is_filler_query(&words(filler)), "{filler} is a filler query");
            assert!(!may_be_real_word(&words(filler)), "{filler} gets no real-word note");
        }
        assert!(may_be_real_word(&words("like")));
        assert!(may_be_real_word(&words("so")));
        assert!(may_be_real_word(&words("um so")), "a phrase with a real-word query keeps the note");
    }

    #[test]
    fn a_word_mostly_in_cut_out_audio_is_not_speech_on_the_timeline() {
        // The cut took source 7f..13f (0.233 s to 0.433 s at 30 fps); the right piece keeps 13f on.
        let removed = [(7.0 / 30.0, 13.0 / 30.0)];
        // "makes" heard across the seam: 10f..15f, 3 of 5 frames cut out.
        assert!(mostly_removed(&removed, 10.0 / 30.0, 15.0 / 30.0));
        // "editing" starting at 16f: clear of the cut.
        assert!(!mostly_removed(&removed, 16.0 / 30.0, 25.0 / 30.0));
        // A word that only touches the seam stays.
        assert!(!mostly_removed(&removed, 12.0 / 30.0, 17.0 / 30.0));
        // No length counts by its start; no removed audio removes nothing.
        assert!(mostly_removed(&removed, 8.0 / 30.0, 8.0 / 30.0));
        assert!(!mostly_removed(&removed, 13.0 / 30.0, 13.0 / 30.0));
        assert!(!mostly_removed(&[], 0.0, 1.0));
    }

    #[test]
    fn a_lane_plays_the_stretches_between_its_clips_as_cut_out() {
        // Clips at source 0..7f and 13f..316f: 7f..13f was cut, and nothing is cut before 0 or after 316f.
        let out = cut_out(vec![(13, 316), (0, 7)], 30.0);
        assert_eq!(out.len(), 2);
        assert!((out[0].0 - 7.0 / 30.0).abs() < 1e-9 && (out[0].1 - 13.0 / 30.0).abs() < 1e-9);
        assert!((out[1].0 - 316.0 / 30.0).abs() < 1e-9 && out[1].1 > 1e6);
        // The first clip starts at the file's start: nothing before it is cut.
        assert_eq!(cut_out(vec![(0, 100)], 30.0).len(), 1);
    }

    #[test]
    fn a_word_or_phrase_mostly_in_cut_out_audio_is_dropped_from_a_piece() {
        // Keep 0..2 s; 0.3..0.5 s was cut out. "makes" (0.2..0.45 s) is mostly cut out, so it goes;
        // "editing" (0.6..0.9 s) stays; a phrase with no word times at 0.3..0.45 s goes whole.
        let raw = r#"{"start":0.2,"end":0.9,"text":"makes editing","words":[{"w":"makes","start":0.2,"end":0.45,"p":0.9},{"w":"editing","start":0.6,"end":0.9,"p":0.9}],"approx":false}
{"start":0.3,"end":0.45,"text":"fast","words":[],"approx":false}"#;
        let piece = Piece {
            label: "A1@0".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 0.0,
            to: 2.0,
            keep: (0.0, 2.0),
            removed: vec![(0.3, 0.5)],
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        let segs = raw_to_segs(raw, &piece, 30.0).unwrap();
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].text, "editing");
        assert_eq!(segs[0].words.len(), 1);
    }

    #[test]
    fn which_picks_all_first_or_listed_matches() {
        assert_eq!(which_arg(&json!({})).unwrap().pick(3).unwrap(), vec![0, 1, 2]);
        assert_eq!(which_arg(&json!({"which": "first"})).unwrap().pick(3).unwrap(), vec![0]);
        assert_eq!(which_arg(&json!({"which": [2, 0]})).unwrap().pick(3).unwrap(), vec![2, 0]);
        assert!(which_arg(&json!({"which": [3]})).unwrap().pick(3).is_err());
        assert!(which_arg(&json!({"which": "some"})).is_err());
    }

    #[test]
    fn pad_defaults_to_about_two_frames_and_is_bounded() {
        assert_eq!(pad_arg(&json!({}), 30.0).unwrap(), 2);
        assert_eq!(pad_arg(&json!({"pad": "3f"}), 30.0).unwrap(), 3);
        assert!(pad_arg(&json!({"pad": "2s"}), 30.0).is_err());
    }

    #[test]
    fn a_filler_with_no_match_names_the_model_and_suggests_small() {
        let text = no_match_text("like", "A1", "base", 71, true);
        assert!(text.starts_with("no 'like' heard by the base model in A1 (71 words heard)"), "{text}");
        assert!(text.contains("model:\"small\""), "{text}");
        assert!(!text.contains("good if"), "{text}");
        assert!(no_match_text("brown fox", "A1", "base", 120, false).contains("try a shorter phrase"));
    }

    #[test]
    fn a_segment_across_a_cut_keeps_its_words_on_each_side() {
        // One raw segment runs from 0.0 s to 4.8 s; its middle (2.4 s) falls in the gap between
        // two clips of the same source, which used to drop it from both.
        let raw = r#"{"start":0.0,"end":4.8,"text":"Hi, thanks we shipped March.","words":[{"w":"Hi,","start":0.0,"end":0.5,"p":0.9},{"w":"thanks","start":0.5,"end":0.9,"p":0.9},{"w":"we","start":2.2,"end":2.36,"p":0.9},{"w":"shipped","start":3.0,"end":3.3,"p":0.9},{"w":"March.","start":4.4,"end":4.8,"p":0.9}],"approx":false}"#;
        let piece = |keep: (f64, f64), in_frame: u32| Piece {
            label: "A1@0".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 0.0,
            to: 4.84,
            keep,
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame, speed: Speed::NORMAL },
            mix: false,
        };
        let first = raw_to_segs(raw, &piece((0.0, 2.4), 0), 25.0).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].text, "Hi, thanks we");
        // It runs past the span's end, so it ends at its last kept word ("we", 2.36 s), not at 2.4 s.
        assert_eq!((first[0].start_f, first[0].end_f), (0, 59));
        let second = raw_to_segs(raw, &piece((2.96, 4.84), 74), 25.0).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "shipped March.");
        assert_eq!(second[0].words.len(), 2);
        // A span with no words of the segment in it gets nothing.
        assert!(raw_to_segs(raw, &piece((1.0, 1.9), 25), 25.0).unwrap().is_empty());
    }

    #[test]
    fn an_empty_cached_answer_is_not_served() {
        assert!(!has_segments(""));
        assert!(!has_segments("\n"));
        assert!(has_segments(r#"{"start":0.0,"end":1.0,"text":"hi","words":[]}"#));
        assert!(is_silent(&[0.0; 16]));
        assert!(!is_silent(&[0.0, 0.2, 0.0]));
    }

    #[test]
    fn wav_header_describes_16k_mono_16_bit() {
        let b = wav_bytes(&[0.0, 1.0, -1.0]);
        assert_eq!(b.len(), 44 + 6);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), RATE);
        assert_eq!(u16::from_le_bytes(b[34..36].try_into().unwrap()), 16);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 6);
    }

    /// A segment of words given as (text, start frame, end frame).
    fn seg_of(words: &[(&str, u32, u32)]) -> Seg {
        let ws: Vec<Word> = words.iter().map(|&(w, s, e)| Word { w: w.into(), start_f: s, end_f: e, p: 0.9 }).collect();
        Seg {
            start_f: ws[0].start_f,
            end_f: ws[ws.len() - 1].end_f,
            text: ws.iter().map(|w| w.w.as_str()).collect::<Vec<_>>().join(" "),
            at: "A2".into(),
            words: ws,
            approx: false,
            no_speech: None,
            avg_logprob: None,
            span_db: None,
            stripped: None,
        }
    }

    #[test]
    fn a_segment_repeating_the_last_word_of_the_one_before_is_dropped() {
        // The A2 case: "volume." (32.1 s to 32.6 s) repeats the end of the segment that runs to 32.4 s.
        let first = seg_of(&[("Treat", 885, 900), ("the", 900, 920), ("first", 920, 940), ("reflection", 940, 1000), ("points", 1000, 1040), ("volume", 1900, 1940)]);
        let repeat = seg_of(&[("volume.", 1928, 1952)]);
        let next = seg_of(&[("Then", 1960, 1990), ("we", 1990, 2000)]);
        let out = drop_overlap_repeats(vec![first.clone(), repeat, next.clone()]);
        assert_eq!(out.len(), 2, "the repeat is gone");
        assert_eq!(out[0], first);
        assert_eq!(out[1], next, "a segment that starts after the one before ends is kept");
    }

    #[test]
    fn leading_words_that_repeat_the_previous_end_are_dropped_and_the_rest_kept() {
        let first = seg_of(&[("shape", 100, 120), ("the", 120, 130), ("volume", 130, 160)]);
        let second = seg_of(&[("volume", 150, 170), ("evenly", 170, 200)]);
        let out = drop_overlap_repeats(vec![first, second]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "evenly", "the repeated word is cut and the rest kept");
        assert_eq!(out[1].start_f, 170);
    }

    #[test]
    fn a_repeat_that_does_not_overlap_in_time_is_kept() {
        // Same words, but the second starts after the first ends: two real utterances.
        let first = seg_of(&[("no", 100, 110), ("no", 110, 120)]);
        let second = seg_of(&[("no", 130, 140)]);
        let out = drop_overlap_repeats(vec![first, second]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn a_temp_write_error_names_the_file_and_the_temp_folder() {
        let path = PathBuf::from("/nowhere/edith-listen-stt-1-2.wav");
        let msg = temp_write_error(&path, "writing", "Disk quota exceeded (os error 122)");
        assert!(msg.starts_with("writing /nowhere/edith-listen-stt-1-2.wav: Disk quota exceeded"), "{msg}");
        assert!(msg.contains("TMPDIR"), "the message names TMPDIR: {msg}");
        assert!(msg.contains(&std::env::temp_dir().display().to_string()), "the message names the folder: {msg}");
    }

    #[test]
    fn a_temp_file_is_removed_when_it_goes_out_of_scope() {
        let path = temp_path("cleanup-test", "wav");
        {
            let _guard = TempFile(path.clone());
            fs::write(&path, wav_bytes(&[0.0; 16])).expect("write");
            assert!(path.exists());
        }
        assert!(!path.exists(), "the temp wav is gone after its scope ends");
        // A failed write leaves nothing behind either: the guard is made before the write.
        {
            let guard = TempFile(path.clone());
            let _ = fs::write(&guard.0, wav_bytes(&[0.5; 16]));
        }
        assert!(!path.exists());
    }

    #[test]
    fn resample_halves_length_when_rate_halves() {
        let input = vec![0.5f32; 100];
        assert_eq!(resample_linear(&input, 32_000.0, 16_000.0).len(), 50);
        assert_eq!(resample_linear(&input, 16_000.0, 16_000.0).len(), 100);
    }

    #[test]
    fn raw_lines_become_timeline_segments() {
        let raw = r#"{"start":2.0,"end":3.0,"text":"the fox","words":[{"w":"the","start":2.0,"end":2.4,"p":0.9},{"w":"fox","start":2.4,"end":3.0,"p":0.8}],"approx":true}"#;
        let p = Piece {
            label: "A1@0".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 0.0,
            to: 0.0,
            keep: (0.0, 10.0),
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        let segs = raw_to_segs(raw, &p, 25.0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].start_f, segs[0].end_f), (50, 75));
        assert_eq!(segs[0].words[1].w, "fox");
        assert_eq!((segs[0].words[1].start_f, segs[0].words[1].end_f), (60, 75));
        assert_eq!(segs[0].at, "A1@0");
    }

    #[test]
    fn phrase_search_matches_normalised_words() {
        let words: Vec<String> = "The quick, brown fox; the Fox".split_whitespace().map(norm).collect();
        let q: Vec<String> = "brown FOX".split_whitespace().map(norm).collect();
        assert_eq!(phrase_hits(&words, &q), vec![2]);
        let one: Vec<String> = vec!["fox".into()];
        assert_eq!(phrase_hits(&words, &one), vec![3, 5]);
        assert!(phrase_hits(&words, &[]).is_empty());
    }

    #[test]
    fn repeated_phrase_matches_each_word_once() {
        let words: Vec<String> = "we we we".split_whitespace().map(norm).collect();
        let q: Vec<String> = "we we".split_whitespace().map(norm).collect();
        assert_eq!(phrase_hits(&words, &q), vec![0, 1]);
    }

    #[test]
    fn stamp_formats_minutes_and_milliseconds() {
        assert_eq!(stamp(300, 25.0), "00:12.000");
        assert_eq!(stamp(10, 25.0), "00:00.400");
    }

    #[test]
    fn coverage_gaps_flags_audible_audio_without_words() {
        // 6 s of tone at 16 kHz, then 2 s of silence, then 4 s of tone.
        let tone = |n: usize| (0..n).map(|i| 0.1 * ((i as f32) * 0.3).sin()).collect::<Vec<f32>>();
        let mut samples = tone(6 * 16_000);
        samples.extend(vec![0.0; 2 * 16_000]);
        samples.extend(tone(4 * 16_000));
        let seg = |start_f: u32, end_f: u32| Seg {
            start_f,
            end_f,
            text: String::new(),
            at: String::new(),
            words: Vec::new(),
            approx: false,
            no_speech: None,
            avg_logprob: None,
            span_db: None,
            stripped: None,
        };
        // Words over the first 2 s only: 2 to 6 s and 8 to 12 s are audible and uncovered.
        let gaps = coverage_gaps(&samples, 0, 30.0, &[seg(0, 60)]);
        assert_eq!(gaps, vec![(60, 180), (240, 360)]);
        // No words at all: the whole audible stretch is one gap (the silence splits it at 6 s).
        let none = coverage_gaps(&samples, 0, 30.0, &[]);
        assert_eq!(none, vec![(0, 180), (240, 360)]);
    }

    fn call(srv: &mut Server, name: &str, args: Value) -> Reply {
        let tool = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool is registered");
        (tool.run)(srv, &args)
    }

    /// Synthesises speech, transcribes, searches and captions it. Skipped, with the
    /// reason printed, when edith-stt, the tiny model or espeak-ng is missing.
    #[test]
    fn speech_round_trip() {
        if let Err(e) = stt_bin() {
            eprintln!("skipping speech_round_trip: {e}");
            return;
        }
        if check_model("tiny").is_err() {
            eprintln!("skipping speech_round_trip: the tiny model is not installed");
            return;
        }
        if Command::new("espeak-ng").arg("--version").output().is_err() {
            eprintln!("skipping speech_round_trip: espeak-ng is not installed");
            return;
        }
        // Removed when the guard drops, so a failing assert does not leave it behind.
        let dir = crate::testutil::TestDir::new("listen-speech");
        let wav = dir.join("speech.wav");
        let ok = Command::new("espeak-ng")
            .arg("-w")
            .arg(&wav)
            .arg("the quick brown fox jumps over the lazy dog")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "espeak-ng could not write {}", wav.display());

        let mut srv = Server::new();
        let proj = dir.join("speech.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);
        assert!(!call(&mut srv, "media_import", json!({"paths": [wav.display().to_string()]})).is_error);
        let placed = call(&mut srv, "edit", json!({"ops": [{"op": "place", "source": 0, "at": "0f", "lane": "A1"}]}));
        assert!(!placed.is_error, "place failed: {}", placed.text);

        let t = call(&mut srv, "transcript", json!({"target": "A1", "model": "tiny"}));
        assert!(!t.is_error, "transcript failed: {}", t.text);
        assert!(t.text.to_lowercase().contains("fox"), "no `fox` in: {}", t.text);

        let f = call(&mut srv, "find_words", json!({"query": "fox", "model": "tiny"}));
        assert!(!f.is_error, "find_words failed: {}", f.text);
        let hits = f.structured.as_ref().and_then(|v| v["matches"].as_array()).map_or(0, Vec::len);
        assert!(hits >= 1, "find_words found nothing: {}", f.text);

        let srt = dir.join("speech.srt");
        let c = call(&mut srv, "auto_captions", json!({"model": "tiny", "path": srt.display().to_string()}));
        assert!(!c.is_error, "auto_captions failed: {}", c.text);
        let cues = c.structured.as_ref().and_then(|v| v["cues"].as_u64()).unwrap_or(0);
        assert!(cues >= 1, "no caption cues: {}", c.text);
        assert!(srt.is_file());
    }

    #[test]
    fn a_hit_line_names_the_phrase_once() {
        let words = vec!["brown 10f–20f".to_string(), "fox 20f–40f".to_string()];
        let line = hit_line(&HitText {
            n: 0,
            span: (10, 40),
            words: &words,
            stamps: ("00:00.333".into(), "00:01.333".into()),
            at: 2,
            before: "the quick",
            phrase: "brown fox",
            after: "jumps",
        });
        assert_eq!(
            line,
            "#0 10f–40f (brown 10f–20f, fox 20f–40f) (00:00.333–00:01.333) [2] …the quick [brown fox] jumps…\n"
        );
        assert_eq!(line.matches("brown fox").count(), 1, "{line}");
    }

    fn end_of(srv: &Server) -> u32 {
        parse::session_end(session(srv).expect("a project"))
    }

    /// `find_words` with cut: a dry run keeps the timeline, a cut shortens it and undo gives the
    /// time back. Skipped, with the reason printed, when edith-stt, the tiny model or espeak-ng is missing.
    #[test]
    fn find_words_dry_run_without_cut_is_accepted_and_named_as_no_effect() {
        // No project is open, so the search itself fails; the dry_run argument must not be refused.
        let mut srv = Server::new();
        let r = call(&mut srv, "find_words", json!({"query": "um", "target": "A3", "dry_run": true}));
        assert!(!r.text.contains("applies to a cut"), "{}", r.text);
    }

    #[test]
    fn pad_note_states_the_match_and_the_padding_each_side() {
        // 367f..379f at 30 fps is 0.400 s; a 2f pad each side takes 363f..383f, 0.533 s.
        let f = pad_fact(367, 379, 2, (365, 381), 400, &[]);
        assert_eq!((f.before, f.after, f.len), (2, 2, 12));
        assert!(f.stops.is_empty());
        assert_eq!(pad_note(&[&f], 2, 30.0), " (the match 0.400 s plus 2f padding each side)");
    }

    #[test]
    fn pad_note_names_where_a_side_stopped_short() {
        // The next word starts at 380f, so the padding after the match stops there.
        let f = pad_fact(367, 379, 2, (365, 380), 380, &[]);
        assert_eq!(f.after, 1);
        assert_eq!(f.stops, vec!["380f (the next word starts there)".to_string()]);
        assert_eq!(pad_note(&[&f], 2, 30.0), " (the match 0.400 s plus 2f before and 1f after, stopped at 380f (the next word starts there))");
    }

    #[test]
    fn find_cut_removes_the_matched_word_and_undo_restores_it() {
        if stt_bin().is_err() || check_model("tiny").is_err() {
            eprintln!("skipping find_cut_removes_the_matched_word_and_undo_restores_it: edith-stt or the tiny model is missing");
            return;
        }
        if Command::new("espeak-ng").arg("--version").output().is_err() {
            eprintln!("skipping find_cut_removes_the_matched_word_and_undo_restores_it: espeak-ng is not installed");
            return;
        }
        let dir = crate::testutil::TestDir::new("listen-cut");
        let wav = dir.join("speech.wav");
        let ok = Command::new("espeak-ng")
            .arg("-w")
            .arg(&wav)
            .arg("the quick brown fox jumps over the lazy dog")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "espeak-ng could not write {}", wav.display());

        let mut srv = Server::new();
        let proj = dir.join("cut.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);
        assert!(!call(&mut srv, "media_import", json!({"paths": [wav.display().to_string()]})).is_error);
        let placed = call(&mut srv, "edit", json!({"ops": [{"op": "place", "source": 0, "at": "0f", "lane": "A1"}]}));
        assert!(!placed.is_error, "place failed: {}", placed.text);
        let full = end_of(&srv);

        let dry = call(&mut srv, "find_words", json!({"query": "fox", "model": "tiny", "cut": true, "dry_run": true}));
        assert!(!dry.is_error, "dry run failed: {}", dry.text);
        assert!(dry.text.contains("dry run"), "{}", dry.text);
        assert_eq!(end_of(&srv), full, "a dry run must keep the timeline");

        let cut = call(&mut srv, "find_words", json!({"query": "fox", "model": "tiny", "cut": true}));
        assert!(!cut.is_error, "cut failed: {}", cut.text);
        assert!(cut.text.contains("undo step"), "{}", cut.text);
        let after = end_of(&srv);
        assert!(after < full, "the cut did not shorten the timeline: {}", cut.text);

        let undo = call(&mut srv, "undo", json!({}));
        assert!(!undo.is_error, "undo failed: {}", undo.text);
        assert_eq!(end_of(&srv), full, "one undo must restore the cut");
    }

    /// The kept note names each match a cut leaves: a picked one keeps the word `keep` names,
    /// an unpicked one is kept whole.
    #[test]
    fn kept_note_names_each_match_a_cut_leaves() {
        let info = vec![(0, "so so".to_string(), 372), (1, "so so".to_string(), 616), (2, "so so".to_string(), 900)];
        assert_eq!(
            kept_note(&info, &[0, 2], Some(Keep::First), 30.0),
            "; kept: 'so so' at 12.4s (keep:first); kept: 'so so' at 20.5s (not cut); kept: 'so so' at 30.0s (keep:first)"
        );
        assert_eq!(
            kept_note(&info, &[0], None, 30.0),
            "; kept: 'so so' at 20.5s (not cut); kept: 'so so' at 30.0s (not cut)"
        );
        assert_eq!(kept_note(&info, &[0, 1, 2], None, 30.0), "", "a cut of every match keeps none");
    }

    fn av_server() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        Server { session: Some(PlaybackSession::open(p).expect("open test asset")), ..Server::default() }
    }

    /// `cut:"lift"` removes the span's audio from the cut lanes and leaves a hole: the other sound
    /// lanes keep their time and the timeline keeps its length. A ripple over the same span warns
    /// that the unlinked sound lane is now out of sync and names the lift option.
    #[test]
    fn a_lift_leaves_a_hole_and_a_ripple_warns_about_the_unlinked_lane() {
        let mut srv = av_server();
        let placed = call(&mut srv, "edit", json!({"ops": [{"op": "lane_add", "kind": "audio"}, {"op": "place", "source": 0, "at": "0f", "lane": "A2"}]}));
        assert!(!placed.is_error, "place failed: {}", placed.text);
        let a1 = Lane::new(LaneKind::Audio, 0);
        let a2 = Lane::new(LaneKind::Audio, 1);
        // The test clip's A1 is one take with V1, so the cut takes both: A2 stays unlinked.
        let take = [Lane::new(LaneKind::Video, 0), a1];
        let full = end_of(&srv);
        let starts = |srv: &Server, l: Lane| -> Vec<u32> {
            srv.session_ref().map_err(|_| "no project").expect("a project").lane_clips(l).iter().map(|c| c.start).collect()
        };
        let a2_before = starts(&srv, a2);

        let ripple = cut_spans(&mut srv, &[(30, 60)], true, "ripple", &take, &json!({}), false).expect("a dry ripple");
        assert!(ripple.ripple.contains("A2 now out of sync with V1+A1 from"), "{}", ripple.ripple);
        assert!(ripple.ripple.contains("cut:\"lift\""), "{}", ripple.ripple);

        let lift = cut_spans(&mut srv, &[(30, 60)], true, "lift", &take, &json!({}), true).expect("a dry lift");
        assert!(lift.ripple.contains("holes left on V1+A1 at"), "{}", lift.ripple);
        assert!(!lift.ripple.contains("out of sync"), "{}", lift.ripple);
        assert_eq!(end_of(&srv), full, "a dry run keeps the timeline");

        let cut = cut_spans(&mut srv, &[(30, 60)], false, "lift", &take, &json!({}), true).expect("a lift");
        assert_eq!(end_of(&srv), full, "a lift moves nothing: the timeline keeps its length");
        assert_eq!(starts(&srv, a1), vec![0, 60], "A1 keeps the two halves around the hole");
        assert_eq!(starts(&srv, a2), a2_before, "the unlinked lane is not touched");
        assert!(cut.undo_step > 0, "the lift is an undo step");
    }

    /// The `cut` argument: a bool, or the mode names. Anything else is refused by name.
    #[test]
    fn cut_arg_reads_bool_and_the_two_modes() {
        assert_eq!(cut_arg(&json!({})).expect("absent"), (false, false));
        assert_eq!(cut_arg(&json!({"cut": true})).expect("true"), (true, false));
        assert_eq!(cut_arg(&json!({"cut": "ripple"})).expect("ripple"), (true, false));
        assert_eq!(cut_arg(&json!({"cut": "lift"})).expect("lift"), (true, true));
        assert!(cut_arg(&json!({"cut": "slide"})).is_err());
    }

    /// A filler match whose words are the prompt's own, in order, is the prompt echoed back. Real
    /// speech with a filler in it, or a short run of prompt words, is not.
    #[test]
    fn a_match_quoting_the_filler_prompt_is_an_echo() {
        let prompt = prompt_words();
        let words = |t: &str| -> Vec<String> { t.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect() };
        // The QA case: the prompt's words around "uh" (index 3), over "Yes, that is the whole job really."
        let echo = words("mean er hmm uh so like you know I mean");
        assert!(prompt_echo(&echo, 3, 1, &prompt), "the prompt's own run is an echo");
        let real = words("yes that is the whole job really uh");
        assert!(!prompt_echo(&real, 6, 1, &prompt), "a filler in real speech is kept");
        // Three prompt words in a row ("uh so like") are not enough on their own.
        let short = words("well uh so like there");
        assert!(!prompt_echo(&short, 1, 1, &prompt), "a three-word run is not an echo");
    }

    /// A find_words cut gives the ripple notes an edit op gives: the lanes it leaves in
    /// place, and, with `lanes` naming the cut lanes, the caption lanes it puts out of sync.
    /// The dry run says the same and keeps nothing.
    #[test]
    fn a_find_cut_reports_what_it_left_in_place_and_drifting_captions() {
        let mut srv = av_server();
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "subtitles_import", "path": srt.display().to_string()},
                {"op": "captions_place", "track": 0}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        // The picture and its sound are one take, so the cut takes both lanes; the captions stay.
        let scope = [Lane::new(LaneKind::Video, 0), Lane::new(LaneKind::Audio, 0)];
        let full = end_of(&srv);
        let args = json!({"lanes": ["V1", "A1"]});

        let dry = cut_spans(&mut srv, &[(0, 30)], true, "dry", &scope, &args, false).expect("a dry cut");
        assert!(dry.ripple.contains("; left in place (not linked; pass lanes to move them): S1@"), "{}", dry.ripple);
        assert!(dry.ripple.contains("; S1 not in lanes: 3 cues now out of sync with V1"), "{}", dry.ripple);
        assert_eq!(end_of(&srv), full, "a dry run keeps the timeline");

        let cut = cut_spans(&mut srv, &[(0, 30)], false, "test", &scope, &args, false).expect("a cut");
        assert_eq!(cut.ripple, dry.ripple, "the cut says what its dry run said");
        assert!(cut.undo_step > 0, "the cut is an undo step");

        // Without `lanes` there is no caption warning, as an edit op gives none then.
        let quiet = cut_spans(&mut srv, &[(0, 30)], true, "quiet", &scope, &json!({}), false).expect("a dry cut");
        assert!(!quiet.ripple.contains("out of sync"), "{}", quiet.ripple);
    }

    /// A find_words cut closes a hole it leaves under the sliver on the lanes it ripples, and says so.
    #[test]
    fn a_find_cut_closes_a_sliver_hole_on_its_lanes() {
        let mut srv = av_server();
        let fix = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "lane_add", "kind": "video"},
                {"op": "place", "source": 0, "at": "0f", "in": "0s", "out": "2s", "lane": "V2", "audio": false},
                {"op": "place", "source": 0, "at": "61f", "in": "0s", "out": "2s", "lane": "V2", "audio": false}
            ]}),
        );
        assert!(!fix.is_error, "{}", fix.text);
        let scope = [Lane::new(LaneKind::Video, 1)];
        assert_eq!(srv.session.as_ref().unwrap().gap_count(scope[0]), 1, "the fixture leaves a one-frame hole");
        let cut = cut_spans(&mut srv, &[(100, 2)], false, "sliver", &scope, &json!({}), false).expect("a cut");
        assert!(cut.ripple.contains("closed 1 hole(s) under 4f on V2"), "{}", cut.ripple);
        assert_eq!(srv.session.as_ref().unwrap().gap_count(scope[0]), 0, "{}", cut.ripple);
    }

    /// The find_words reply for a cut with `which` names the matches it kept, and what the cut
    /// left in place. Skipped, with the reason printed, when edith-stt, the tiny model or espeak-ng is missing.
    #[test]
    fn find_cut_with_which_names_the_kept_match_and_the_lane_left_in_place() {
        if stt_bin().is_err() || check_model("tiny").is_err() {
            eprintln!("skipping find_cut_with_which_names_the_kept_match: edith-stt or the tiny model is missing");
            return;
        }
        if Command::new("espeak-ng").arg("--version").output().is_err() {
            eprintln!("skipping find_cut_with_which_names_the_kept_match: espeak-ng is not installed");
            return;
        }
        let dir = crate::testutil::TestDir::new("listen-kept");
        let wav = dir.join("speech.wav");
        let ok = Command::new("espeak-ng")
            .arg("-w")
            .arg(&wav)
            .arg("the quick brown fox jumps over the lazy dog")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "espeak-ng could not write {}", wav.display());

        let mut srv = Server::new();
        let proj = dir.join("kept.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);
        assert!(!call(&mut srv, "media_import", json!({"paths": [wav.display().to_string()]})).is_error);
        // A second copy of the speech on A2, starting after the first match: a lane the cut
        // does not take, so the reply must name it as left in place.
        let placed = call(
            &mut srv,
            "edit",
            json!({"ops": [
                {"op": "place", "source": 0, "at": "0f", "lane": "A1"},
                {"op": "lane_add", "kind": "audio"},
                {"op": "place", "source": 0, "at": "5s", "lane": "A2"}
            ]}),
        );
        assert!(!placed.is_error, "place failed: {}", placed.text);

        let cut = call(&mut srv, "find_words", json!({"query": "the", "model": "tiny", "cut": true, "which": [0]}));
        assert!(!cut.is_error, "cut failed: {}", cut.text);
        assert!(cut.text.contains("kept: 'the' at "), "the first match is kept whole: {}", cut.text);
        assert!(cut.text.contains("(not cut)"), "{}", cut.text);
        assert!(cut.text.contains("left in place (not linked; pass lanes to move them): A2@"), "{}", cut.text);
    }

    /// A cut lists the caption cues it wholly removes, by words and start, and moves the titles
    /// over the picture it moves. Both are dry runs, so the timeline is kept.
    #[test]
    fn a_find_cut_lists_removed_cues_and_moves_titles() {
        let mut srv = av_server();
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "subtitles_import", "path": srt.display().to_string()},
                {"op": "captions_place", "track": 0},
                {"op": "title_add", "at": "3s", "duration": "1s", "text": "Chapter"}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let scope = [Lane::new(LaneKind::Video, 0), Lane::new(LaneKind::Audio, 0), Lane::new(LaneKind::Subtitle, 0)];
        let title_before = srv.session().map_err(|_| "no project").expect("a project").titles()[0].start;

        // The cue 2.000 s to 3.250 s lies wholly inside frames 60..98: it is removed, and listed.
        let gone = cut_spans(&mut srv, &[(60, 98)], true, "dry", &scope, &json!({}), false).expect("a dry cut");
        assert!(gone.ripple.contains("; removed cue 'second line / with a break' at 2.0s"), "{}", gone.ripple);

        // Cutting the first second moves the title at 3 s back by that second.
        let moved = cut_spans(&mut srv, &[(0, 30)], true, "dry", &scope, &json!({}), false).expect("a dry cut");
        assert!(moved.ripple.contains("T1"), "the title's move is named: {}", moved.ripple);
        assert_eq!(srv.session().map_err(|_| "no project").expect("a project").titles()[0].start, title_before, "a dry run keeps the title");
    }

    /// An imported source that no clip uses is heard as its own file, not reported as
    /// silence. Skipped, with the reason printed, when edith-stt, the tiny model or espeak-ng is missing.
    #[test]
    fn unplaced_source_is_heard_as_its_own_file() {
        if stt_bin().is_err() || check_model("tiny").is_err() {
            eprintln!("skipping unplaced_source_is_heard_as_its_own_file: edith-stt or the tiny model is missing");
            return;
        }
        if Command::new("espeak-ng").arg("--version").output().is_err() {
            eprintln!("skipping unplaced_source_is_heard_as_its_own_file: espeak-ng is not installed");
            return;
        }
        let dir = crate::testutil::TestDir::new("listen-unplaced");
        let wav = dir.join("speech.wav");
        let ok = Command::new("espeak-ng")
            .arg("-w")
            .arg(&wav)
            .arg("the quick brown fox jumps over the lazy dog")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "espeak-ng could not write {}", wav.display());

        let mut srv = Server::new();
        let proj = dir.join("unplaced.edith");
        assert!(!call(&mut srv, "project_new", json!({"path": proj.display().to_string()})).is_error);
        assert!(!call(&mut srv, "media_import", json!({"paths": [wav.display().to_string()]})).is_error);
        let t = call(&mut srv, "transcript", json!({"target": "src:0", "model": "tiny"}));
        assert!(!t.is_error, "transcript of an unplaced source failed: {}", t.text);
        assert!(t.text.to_lowercase().contains("fox"), "an unplaced source was not heard: {}", t.text);
    }

    #[test]
    fn approx_is_read_per_segment_and_missing_means_approximate() {
        let raw = concat!(
            r#"{"start":0.0,"end":1.0,"text":"a","words":[],"approx":false}"#,
            "\n",
            r#"{"start":1.0,"end":2.0,"text":"b","words":[]}"#
        );
        let segs = parse_raw(raw).expect("parses");
        assert!(!segs[0].approx);
        assert!(segs[1].approx);
    }

    fn mix_piece() -> Piece {
        Piece {
            label: "mix".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 0.0,
            to: 20.0,
            keep: (0.0, 20.0),
            removed: Vec::new(),
            map: Map::Mix,
            mix: true,
        }
    }

    #[test]
    fn a_segment_with_a_collapsed_word_is_approximate() {
        // Real edith-stt output before the span repair: "You" had no duration.
        let raw = concat!(
            r#"{"start":5.24,"end":11.44,"text":"You describe the story","words":[{"w":"You","start":5.24,"end":5.24,"p":0.99},{"w":"describe","start":5.24,"end":5.62,"p":0.9}],"approx":false}"#,
            "\n",
            r#"{"start":0.0,"end":2.0,"text":"Edith turns","words":[{"w":"Edith","start":0.0,"end":0.28,"p":0.9},{"w":"turns","start":0.28,"end":0.58,"p":0.9}],"approx":false}"#,
        );
        let segs = raw_to_segs(raw, &mix_piece(), 30.0).unwrap();
        assert!(segs[0].approx, "a collapsed word must not be reported as reliable");
        assert!(!segs[1].approx, "clean alignment stays reliable");
    }

    #[test]
    fn hint_is_trimmed_and_bounded() {
        assert_eq!(hint_arg(&json!({"hint": "  Edith, LUFS "})).unwrap(), "Edith, LUFS");
        assert_eq!(hint_arg(&json!({})).unwrap(), "");
        let long = "x".repeat(HINT_MAX + 1);
        assert!(hint_arg(&json!({ "hint": long })).is_err());
    }

    #[test]
    fn confidence_fields_are_optional_and_read_when_present() {
        let raw = concat!(
            r#"{"start":0.0,"end":1.0,"text":"a","words":[],"no_speech":0.12,"avg_logprob":-0.4}"#,
            "\n",
            r#"{"start":1.0,"end":2.0,"text":"b","words":[]}"#
        );
        let segs = parse_raw(raw).expect("parses");
        assert_eq!(segs[0].no_speech, Some(0.12));
        assert_eq!(segs[0].avg_logprob, Some(-0.4));
        assert_eq!(segs[1].no_speech, None);
        assert_eq!(segs[1].avg_logprob, None);
    }

    #[test]
    fn confident_words_leave_out_low_segments_low_words_and_disfluencies() {
        let w = |s: &str, p: f64| Word { w: s.into(), start_f: 0, end_f: 1, p };
        let seg = |lp: Option<f64>, words: Vec<Word>| Seg {
            start_f: 0,
            end_f: 1,
            text: String::new(),
            at: "A1".into(),
            words,
            approx: false,
            no_speech: None,
            avg_logprob: lp,
            span_db: None,
            stripped: None,
        };
        // A confident segment: two confident words, one under the bar, one disfluency.
        let good = seg(Some(-0.2), vec![w("hello", 0.9), w("there", 0.8), w("maybe", 0.3), w("um", 0.95)]);
        assert_eq!(confident_words(&[good]), 2);
        // A low-confidence segment counts for nothing, however sure its words sound.
        let shaky = seg(Some(-1.4), vec![w("for", 0.9), w("seeks", 0.9)]);
        assert_eq!(confident_words(&[shaky]), 0);
        assert_eq!(confident_words(&[]), 0);
    }

    #[test]
    fn low_confidence_is_below_minus_one_and_unknown_is_not_marked() {
        let seg = |lp: Option<f64>| Seg {
            start_f: 0,
            end_f: 1,
            text: "x".into(),
            at: "A1@0".into(),
            words: Vec::new(),
            approx: false,
            no_speech: None,
            avg_logprob: lp,
            span_db: None,
            stripped: None,
        };
        assert!(seg(Some(-1.2)).low_confidence());
        assert!(!seg(Some(-1.0)).low_confidence());
        assert!(!seg(Some(-0.3)).low_confidence());
        assert!(!seg(None).low_confidence());
    }

    #[test]
    fn no_speech_reply_has_no_reliability_claim() {
        let text = transcript_text("A1", "base", "auto", 0, "", &[], None);
        assert!(text.contains("no speech found in this range"));
        assert!(text.contains("filters silent and hallucinated"));
        assert!(!text.contains("reliable"));
        assert!(!text.contains("approx"));
    }

    #[test]
    fn low_confidence_marker_is_explained_once_in_the_header() {
        let seg = Seg {
            start_f: 0,
            end_f: 25,
            text: "maybe".into(),
            at: "A1@0".into(),
            words: Vec::new(),
            approx: false,
            no_speech: None,
            avg_logprob: Some(-1.5),
            span_db: None,
            stripped: None,
        };
        let t = Transcript { segs: vec![seg], fps: 25.0, gaps: Vec::new(), corrections: Vec::new() };
        let (body, shown, next) = page(&t, 0, false);
        let text = transcript_text("A1", "base", "auto", 0, &body, &shown, next);
        assert!(body.contains("] ? maybe"));
        assert_eq!(text.matches("low-confidence").count(), 1);
        assert!(text.contains("Segment times are reliable."));
    }

    #[test]
    fn segments_outside_the_requested_span_are_padding() {
        // Padding is read at 1.0 s..3.0 s; the request is 1.5 s..2.5 s.
        let raw = concat!(
            r#"{"start":1.0,"end":1.4,"text":"early pad","words":[]}"#,
            "\n",
            r#"{"start":1.6,"end":2.2,"text":"inside","words":[]}"#,
            "\n",
            r#"{"start":2.6,"end":2.9,"text":"late pad","words":[]}"#
        );
        let p = Piece {
            label: "A1@0".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 1.0,
            to: 3.0,
            keep: (1.5, 2.5),
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        let segs = raw_to_segs(raw, &p, 25.0).unwrap();
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["inside"]);
    }

    #[test]
    fn a_phrase_crossing_a_range_edge_keeps_the_words_whose_middle_is_inside() {
        // "Edith makes editing fast." ends at 1.54 s; "We cut" starts at 2.3 s. The model heard
        // the phrase whole in a window that reads past the range.
        let raw = concat!(
            r#"{"start":0.0,"end":4.4,"text":"Edith makes editing fast. We cut the silence.","words":["#,
            r#"{"w":"Edith","start":0.0,"end":0.26,"p":0.9},{"w":"makes","start":0.26,"end":0.44,"p":0.9},"#,
            r#"{"w":"editing","start":0.44,"end":0.84,"p":0.9},{"w":"fast.","start":0.84,"end":1.54,"p":0.9},"#,
            r#"{"w":"We","start":2.3,"end":2.38,"p":0.9},{"w":"cut","start":2.38,"end":2.54,"p":0.9}]}"#
        );
        let piece = |keep: (f64, f64)| Piece {
            label: "src:0".into(),
            path: PathBuf::new(),
            stream: 0,
            from: 0.0,
            to: 5.0,
            keep,
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        // Range 0 s to 2 s: the phrase's first four words stay, the text is rebuilt from them,
        // and the segment ends at its last kept word, not at the range edge.
        let segs = raw_to_segs(raw, &piece((0.0, 2.0)), 30.0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Edith makes editing fast.");
        assert_eq!((segs[0].start_f, segs[0].end_f), (0, 46));
        assert_eq!(segs[0].words.len(), 4);
        // Range 1 s to 6 s: "fast." (middle 1.19 s) is inside, so the segment starts at its
        // first word instead of cutting it at 1.0 s.
        let segs = raw_to_segs(raw, &piece((1.0, 6.0)), 30.0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "fast. We cut");
        assert_eq!(segs[0].start_f, 25);
    }

    #[test]
    fn default_model_is_base_then_small_then_tiny() {
        assert_eq!(pick_model(|_| true), Some("base"));
        assert_eq!(pick_model(|m| m != "base"), Some("small"));
        assert_eq!(pick_model(|m| m == "tiny"), Some("tiny"));
        assert_eq!(pick_model(|_| false), None);
    }

    #[test]
    fn filler_prompt_goes_in_front_of_the_hint() {
        assert_eq!(with_fillers("Edith, LUFS", false), "Edith, LUFS");
        assert_eq!(with_fillers("", true), FILLER_PROMPT);
        assert_eq!(with_fillers("Edith", true), format!("{FILLER_PROMPT} Edith"));
        let words = |w: &str| -> Vec<String> { w.split_whitespace().map(norm).collect() };
        assert!(is_filler_query(&words("uh")));
        assert!(is_filler_query(&words("Um")));
        assert!(is_filler_query(&words("you know")));
        assert!(is_filler_query(&words("so like")));
        assert!(!is_filler_query(&words("brown fox")));
        assert!(!is_filler_query(&words("you")));
    }

    fn word(start: f64, end: f64, text: &str, p: f64) -> (i64, i64, String, f64) {
        ((start * 1e6) as i64, (end * 1e6) as i64, text.to_string(), p)
    }

    #[test]
    fn captions_carry_the_transcripts_word_times() {
        let words = vec![
            word(0.0, 0.2, "we", 0.9),
            word(0.2, 1.5, "extraordinarily", 0.9),
            word(1.5, 1.8, "win.", 0.9),
            word(5.0, 5.4, "next", 0.9),
            word(5.4, 5.9, "one", 0.9),
        ];
        let (mut cues, _, _) = caption_cues(&words, 42, 6.0, 0.7);
        assert_eq!(cues.len(), 2);
        attach_word_times(&mut cues, &words);
        let first = cues[0].real_words().expect("the first cue's words read back as its text");
        assert_eq!((first[1].0, first[1].1), (200_000, 1_500_000), "the long word keeps its real span");
        assert_eq!(cues[1].real_words().map(|w| w.len()), Some(2));
    }

    #[test]
    fn small_limits_still_hold_the_minimum_by_moving_words() {
        // Twelve 7-letter words of 0.2 s, no pause: at max_chars 15 and max_secs 1.0 the old cut
        // made 0.6 s cues of three. Four words fit two lines of 15 and last 0.8 s, so every cue
        // holds the minimum and nothing is reported.
        let words: Vec<(i64, i64, String, f64)> = (0..12)
            .map(|k| {
                let text = if k == 11 { "abcdefg.".to_string() } else { "abcdefg".to_string() };
                (k * 200_000, (k + 1) * 200_000, text, 0.9)
            })
            .collect();
        let (cues, _, short) = caption_cues(&words, 15, 1.0, 0.7);
        assert!(short.is_empty(), "{short:?}");
        assert_eq!(cues.len(), 3, "{cues:?}");
        for c in &cues {
            assert!(c.end_us - c.start_us >= 800_000, "short cue: {cues:?}");
            assert!(c.text.lines().all(|l| l.chars().count() <= 15), "{cues:?}");
        }
    }

    #[test]
    fn a_short_cue_the_limits_cannot_hold_is_named() {
        // Each word is longer than two lines of 5 can hold, so every cue is one 0.3 s word. The
        // first two cannot reach 0.8 s (the next cue starts too soon) and are named as forced by the
        // limits; the last one runs on to 0.8 s, as nothing follows it.
        let long = ["abcdefghij", "klmnopqrst", "uvwxyzabcd"];
        let words: Vec<(i64, i64, String, f64)> = long
            .iter()
            .enumerate()
            .map(|(k, w)| (k as i64 * 300_000, (k as i64 + 1) * 300_000, w.to_string(), 0.9))
            .collect();
        let (cues, _, short) = caption_cues(&words, 5, 1.0, 0.7);
        assert_eq!(cues.len(), 3, "{cues:?}");
        assert_eq!(short.len(), 2, "{short:?}");
        assert!(short[0].contains("abcdefghij"), "{short:?}");
    }

    #[test]
    fn caption_cues_never_overlap_and_no_short_cue_survives() {
        // "you" starts before "interview." ends: an overlap the engine's cut leaves in place.
        let words = vec![
            word(10.0, 10.4, "We", 0.9),
            word(10.4, 11.0, "are", 0.9),
            word(11.0, 12.0, "cutting", 0.9),
            word(12.0, 13.0, "the", 0.9),
            word(13.0, 14.16, "interview.", 0.9),
            word(14.08, 14.12, "you", 0.9),
            word(14.12, 15.0, "know", 0.9),
            word(15.0, 16.0, "it", 0.9),
            word(16.0, 17.0, "works.", 0.9),
        ];
        let (cues, _, _) = caption_cues(&words, 42, 6.0, 0.7);
        assert!(!cues.is_empty());
        for pair in cues.windows(2) {
            assert!(pair[0].end_us <= pair[1].start_us, "cues overlap: {cues:?}");
        }
        for c in &cues {
            assert!(c.end_us >= c.start_us, "cue ends before it starts: {cues:?}");
            assert!(c.end_us - c.start_us >= 300_000 || cues.len() == 1, "short cue survived: {cues:?}");
        }
        assert!(cues.iter().any(|c| c.text.contains("you know")), "no word was lost: {cues:?}");
    }

    #[test]
    fn a_one_word_cue_folds_into_its_neighbour_when_the_text_fits() {
        let words = vec![
            word(0.0, 0.4, "We", 0.9),
            word(0.4, 0.8, "cut", 0.9),
            word(0.8, 1.2, "the", 0.9),
            word(1.2, 1.6, "show", 0.9),
            word(1.6, 2.0, "today.", 0.9),
            word(2.8, 3.2, "Done.", 0.9),
        ];
        let (cues, _, _) = caption_cues(&words, 24, 6.0, 0.7);
        assert_eq!(cues.len(), 1, "the one-word cue joined its neighbour: {cues:?}");
        assert!(cues[0].text.contains("Done."), "{cues:?}");
        assert!(cues[0].text.lines().all(|l| l.chars().count() <= 24), "{cues:?}");
    }

    #[test]
    fn a_one_word_cue_that_cannot_join_stays_up_to_the_minimum() {
        // The two cues together run 2.6 s, past max_secs, so "Done." stays its own cue and is
        // held on screen to CUE_MIN_SECS instead of flashing by.
        let words = vec![
            word(0.0, 0.28, "We", 0.9),
            word(0.28, 0.56, "cut", 0.9),
            word(0.56, 0.84, "the", 0.9),
            word(0.84, 1.12, "show", 0.9),
            word(1.12, 1.4, "today.", 0.9),
            word(2.2, 2.6, "Done.", 0.9),
        ];
        let (cues, _, _) = caption_cues(&words, 24, 1.5, 0.7);
        assert_eq!(cues.len(), 2, "{cues:?}");
        let done = &cues[1];
        assert_eq!(done.text, "Done.");
        assert!(done.end_us - done.start_us >= 800_000, "held to 0.8 s: {cues:?}");
    }

    /// The cues of words said by one speaker (the whole track of a lane).
    fn caption_cues(words: &[(i64, i64, String, f64)], max_chars: usize, max_secs: f64, gap_secs: f64) -> (Vec<subtitle::Cue>, Vec<String>, Vec<String>) {
        super::caption_cues(words, &vec![0; words.len()], max_chars, max_secs, gap_secs)
    }

    #[test]
    fn a_cue_never_spans_two_speakers_even_with_no_pause() {
        // Two turns with no gap between them: one cue would hold both, unless the speakers split it.
        let words = vec![
            word(40.0, 41.0, "We", 0.9),
            word(41.0, 42.0, "cut", 0.9),
            word(42.0, 43.0, "here", 0.9),
            word(43.0, 44.0, "now", 0.9),
            word(44.0, 45.0, "and", 0.9),
            word(45.0, 46.0, "then", 0.9),
        ];
        let owners = [0, 0, 0, 1, 1, 1];
        let (cues, _, _) = super::caption_cues(&words, &owners, 42, 6.0, 0.7);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["We cut here", "now and then"], "{texts:?}");
        // One speaker throughout is the same cue as before the split existed.
        let (one, _, _) = super::caption_cues(&words, &[0; 6], 42, 6.0, 0.7);
        assert_eq!(one.len(), 1, "{one:?}");
        assert_eq!(speaker_changes(&words, &owners), 1);
        assert_eq!(speaker_changes(&words, &[0; 6]), 0);
    }

    #[test]
    fn a_trailing_line_whose_words_are_scattered_in_the_cue_before_is_kept() {
        // "Let us check the loudness next." is not a repeat of a cue that happens to hold each of
        // its words in another order; only the same words in a row are.
        let words = vec![
            word(40.0, 41.0, "Next", 0.9),
            word(41.0, 41.5, "we", 0.9),
            word(41.5, 42.0, "check", 0.9),
            word(42.0, 42.5, "the", 0.9),
            word(42.5, 43.0, "loudness,", 0.9),
            word(43.0, 43.5, "let", 0.9),
            word(43.5, 44.0, "us", 0.9),
            word(44.0, 44.5, "go.", 0.9),
            word(50.0, 51.0, "Let", 0.9),
            word(51.0, 52.0, "us", 0.9),
            word(52.0, 53.0, "check", 0.9),
            word(53.0, 54.0, "the", 0.9),
            word(54.0, 55.0, "loudness", 0.9),
            word(55.0, 56.0, "next.", 0.9),
        ];
        let (cues, skipped, _) = caption_cues(&words, 42, 6.0, 0.7);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(cues.last().map(|c| c.text.as_str()), Some("Let us check the loudness next."), "{cues:?}");
        // The same words in a row, said again, are the repeat the drop is for.
        let again = vec![
            word(40.0, 40.5, "Let", 0.9),
            word(40.5, 41.0, "us", 0.9),
            word(41.0, 41.5, "check", 0.9),
            word(41.5, 42.0, "the", 0.9),
            word(42.0, 42.5, "loudness", 0.9),
            word(42.5, 43.0, "next.", 0.9),
            word(50.0, 51.0, "Let", 0.9),
            word(51.0, 52.0, "us", 0.9),
            word(52.0, 53.0, "check", 0.9),
            word(53.0, 54.0, "the", 0.9),
            word(54.0, 55.0, "loudness", 0.9),
            word(55.0, 56.0, "next.", 0.9),
        ];
        let (cues, skipped, _) = caption_cues(&again, 42, 6.0, 0.7);
        assert_eq!(cues.len(), 1, "{cues:?}");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
    }

    #[test]
    fn speakers_are_the_clip_that_started_last_and_gaps_keep_the_speaker_before() {
        // A bed (owner 2) starts at 0 with the first voice (owner 0); the second voice starts at 100.
        let spans = [(0, 100, 0), (0, 300, 2), (100, 200, 1)];
        assert_eq!(covering_speaker(&spans, 50), Some(0), "the tie goes to the lower lane");
        assert_eq!(covering_speaker(&spans, 150), Some(1), "the voice that started last");
        assert_eq!(covering_speaker(&spans, 250), Some(2), "the bed alone");
        assert_eq!(covering_speaker(&spans, 400), None);
        let raw = [Some(0), None, Some(1), None, None];
        assert_eq!(fill_owners(&raw), vec![0, 0, 1, 1, 1]);
        assert_eq!(fill_owners(&[None, Some(1)]), vec![1, 1]);
        assert_eq!(fill_owners(&[None, None]), vec![0, 0]);
    }

    #[test]
    fn a_second_speakers_captions_merge_into_the_lane_holding_the_first() {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut srv = Server { session: Some(PlaybackSession::open(p).expect("open test asset")), ..Server::default() };
        let s = srv.session.as_mut().expect("session");
        // Nothing on S1 yet: nothing to merge.
        assert!(merge_plan(s, Lane::new(LaneKind::Subtitle, 0)).unwrap().is_none());
        let dir = crate::testutil::TestDir::new("merge-captions");
        let first = dir.path().join("one.srt");
        subtitle::save_cues(&first, &[subtitle::Cue { start_us: 0, end_us: 1_000_000, text: "First".into(), image: None, words: Vec::new(), karaoke: Vec::new() }]).unwrap();
        let added = s.import_subtitles(&first).unwrap();
        assert_eq!(added, 1);
        let op = json!({"op": "captions_place", "track": 0, "lane": "S1", "at": 0});
        edit_subs::apply(s, &op, 30.0).expect("known op").expect("placed");
        let (tracks, cues) = merge_plan(s, Lane::new(LaneKind::Subtitle, 0)).unwrap().expect("the lane holds captions");
        assert_eq!(tracks, vec![0]);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "First");
    }

    #[test]
    fn trailing_sign_off_after_a_sign_off_is_dropped() {
        // Whisper's stock ending over silence: the second "Thank you." repeats the sign-off.
        let words = vec![
            word(40.0, 41.0, "We", 0.9),
            word(41.0, 42.0, "cut.", 0.9),
            word(50.0, 51.5, "Thanks", 0.8),
            word(51.5, 52.0, "for", 0.8),
            word(52.0, 53.0, "watching.", 0.8),
            word(55.0, 55.5, "Thank", 0.4),
            word(55.5, 56.0, "you.", 0.4),
        ];
        let (cues, skipped, _) = caption_cues(&words, 42, 6.0, 0.7);
        assert_eq!(skipped.len(), 1, "the dropped sign-off is named: {skipped:?}");
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts.last(), Some(&"Thanks for watching."), "{texts:?}");
    }

    #[test]
    fn trailing_low_confidence_cue_is_dropped_and_a_confident_one_kept() {
        let low = vec![word(40.0, 42.0, "We cut.", 0.9), word(50.0, 51.0, "Mumble", 0.2), word(51.0, 52.0, "here", 0.2)];
        let (cues, skipped, _) = caption_cues(&low, 42, 6.0, 0.7);
        assert_eq!(cues.len(), 1, "{cues:?}");
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        let sure = vec![word(40.0, 42.0, "We cut.", 0.9), word(50.0, 51.0, "Done", 0.9), word(51.0, 52.0, "here.", 0.9)];
        assert_eq!(caption_cues(&sure, 42, 6.0, 0.7).0.len(), 2);
    }

    /// A timeline with V1+A1 as one linked take (test_av.mp4) and picture-only B-roll on V2.
    fn linked_av_with_broll() -> Server {
        engine::ao::set_silent_output();
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let sess = PlaybackSession::open(p).expect("open test asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "lane_add", "kind": "video"}, {"op": "place", "source": 0, "at": "0f", "lane": "V2", "audio": false}]}),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    #[test]
    fn cut_defaults_to_the_target_lane_and_its_linked_partners() {
        let srv = linked_av_with_broll();
        let s = srv.session.as_ref().unwrap();
        // V1 and A1 are one take, so a cut on the voice takes V1 too; the B-roll on V2 is not linked.
        let voice = cut_scope(s, &Want::Lane(Lane::A1), &json!({})).unwrap();
        assert!(voice.contains(&Lane::A1) && voice.contains(&Lane::V1), "voice cut lanes {voice:?}");
        assert!(!voice.contains(&parse::lane("V2").unwrap()), "voice cut lanes {voice:?}");
        // The B-roll alone cuts only its own lane.
        assert_eq!(cut_scope(s, &Want::Lane(parse::lane("V2").unwrap()), &json!({})).unwrap(), vec![parse::lane("V2").unwrap()]);
        // A clip target takes its own lane and its partners.
        let clip = s.lane_clips(Lane::V1)[0].clone();
        let of_clip = cut_scope(s, &Want::Clip(Lane::V1, clip), &json!({})).unwrap();
        assert!(of_clip.contains(&Lane::A1) && !of_clip.contains(&parse::lane("V2").unwrap()));
        // A mix or source target has no one lane, so every lane.
        assert_eq!(cut_scope(s, &Want::Mix, &json!({})).unwrap(), s.lanes());
        // `lanes` names the lanes as given, and must be on the timeline.
        // A named scope that leaves the target's lane out still cuts it, and the reply says so.
        assert_eq!(
            cut_scope(s, &Want::Lane(Lane::A1), &json!({"lanes": ["V2"]})).unwrap(),
            vec![parse::lane("V2").unwrap(), Lane::A1]
        );
        assert!(target_added(&Want::Lane(Lane::A1), &json!({"lanes": ["V2"]})).is_some());
        assert!(target_added(&Want::Lane(Lane::A1), &json!({"lanes": ["A1", "V2"]})).is_none());
        assert!(target_added(&Want::Mix, &json!({"lanes": ["V2"]})).is_none());
        assert!(cut_scope(s, &Want::Lane(Lane::A1), &json!({"lanes": ["A9"]})).is_err());
    }

    /// Locks the lane `name` (or unlocks it) through edit, as a person would with lane_flags.
    fn set_locked(srv: &mut Server, name: &str, locked: bool) {
        let made = crate::tools::edit::run_edit(srv, &json!({"ops": [{"op": "lane_flags", "lane": name, "locked": locked}]}));
        assert!(!made.is_error, "{}", made.text);
    }

    #[test]
    fn find_cut_refuses_a_locked_lane_it_would_move_with_the_way_out() {
        let mut srv = linked_av_with_broll();
        let before = parse::session_end(srv.session.as_ref().unwrap());
        // A locked target refuses, a dry run too, and nothing changes.
        set_locked(&mut srv, "A1", true);
        for dry in [false, true] {
            let r = call(&mut srv, "find_words", json!({"query": "fox", "target": "A1", "cut": true, "dry_run": dry}));
            assert!(r.is_error, "a locked target cut: {}", r.text);
            assert!(r.text.contains("lane A1 is locked") && r.text.contains("lane_flags {lane:\"A1\", locked:false}"), "{}", r.text);
        }
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before, "a refused cut changed the timeline");
        // A locked linked partner (V1 rides with A1) refuses the default scope the same way.
        set_locked(&mut srv, "A1", false);
        set_locked(&mut srv, "V1", true);
        let r = call(&mut srv, "find_words", json!({"query": "fox", "target": "A1", "cut": true, "dry_run": true}));
        assert!(r.is_error && r.text.contains("lane V1 is locked"), "{}", r.text);
        // A locked lane named in lanes refuses too, and a cut without cut:true is never refused.
        set_locked(&mut srv, "V1", false);
        set_locked(&mut srv, "V2", true);
        let r = call(&mut srv, "find_words", json!({"query": "fox", "target": "A1", "cut": true, "lanes": ["V2"]}));
        assert!(r.is_error && r.text.contains("lane V2 is locked") && r.text.contains("lane_flags"), "{}", r.text);
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before, "a refused cut changed the timeline");
    }

    #[test]
    fn find_cut_locks_skip_a_mix_default_scope_and_pass_when_unlocked() {
        let mut srv = linked_av_with_broll();
        set_locked(&mut srv, "V2", true);
        let s = srv.session.as_ref().unwrap();
        let v2 = parse::lane("V2").unwrap();
        // A mix's default scope skips the locked lane and says so.
        let mut scope = s.lanes();
        let skipped = cut_locks(s, &Want::Mix, &json!({}), &mut scope).unwrap();
        assert_eq!(skipped, vec![v2]);
        assert!(!scope.contains(&v2) && scope.contains(&Lane::A1), "scope {scope:?}");
        // Named lanes refuse instead, even for a mix.
        let mut named = s.lanes();
        assert!(cut_locks(s, &Want::Mix, &json!({"lanes": ["V2"]}), &mut named).is_err());
        // Unlocked, the same scope passes with nothing skipped.
        set_locked(&mut srv, "V2", false);
        let s = srv.session.as_ref().unwrap();
        let mut scope = s.lanes();
        assert!(cut_locks(s, &Want::Mix, &json!({}), &mut scope).unwrap().is_empty());
        assert_eq!(scope, s.lanes());
    }

    #[test]
    fn filler_queries_match_their_spellings() {
        let words = |w: &str| -> Vec<String> { w.split_whitespace().map(norm).collect() };
        // "uh" finds "Ah," (the case that was missed), "er" and "eh".
        assert_eq!(phrase_hits(&words("we ah go"), &words("uh")), vec![1]);
        assert_eq!(phrase_hits(&words("so eh and er"), &words("uh")), vec![1, 3]);
        // "um" finds its group: uhm, hum, hmm, mm, erm.
        assert_eq!(phrase_hits(&words("hmm mm uhm hum erm"), &words("um")), vec![0, 1, 2, 3, 4]);
        // Across groups and outside them there is no match.
        assert!(phrase_hits(&words("um"), &words("uh")).is_empty());
        assert!(phrase_hits(&words("ah"), &words("um")).is_empty());
        assert!(phrase_hits(&words("the cat"), &words("the dog")).is_empty());
        // Spellings make a filler query, so the filler prompt goes in.
        assert!(is_filler_query(&words("ah")));
        assert!(is_filler_query(&words("hum")));
        assert!(is_filler_query(&words("erm")));
    }

    #[test]
    fn keep_last_leaves_the_last_repeat_whole() {
        // "I I" at 100–110 and 120–130, padded 2 frames, neighbours far off.
        let span = keep_span(Keep::Last, (100, 110), (120, 130), 2, 0, 200, 1000);
        assert_eq!(span, (98, 120), "cuts the first I and the gap, keeps the last I");
    }

    #[test]
    fn keep_first_leaves_the_first_repeat_whole() {
        let span = keep_span(Keep::First, (100, 110), (120, 130), 2, 0, 200, 1000);
        assert_eq!(span, (110, 132), "keeps the first I, cuts the second with its pad");
        // The pad stops at the word after the run.
        let tight = keep_span(Keep::First, (100, 110), (120, 130), 5, 0, 131, 1000);
        assert_eq!(tight, (110, 131));
    }

    #[test]
    fn keep_needs_a_repeated_query() {
        let mut srv = linked_av_with_broll();
        let r = call(&mut srv, "find_words", json!({"query": "I think", "cut": true, "keep": "last"}));
        assert!(r.is_error && r.text.contains("repeat"), "{}", r.text);
        let r = call(&mut srv, "find_words", json!({"query": "I I", "keep": "last"}));
        assert!(r.is_error && r.text.contains("cut true"), "{}", r.text);
    }

    #[test]
    fn whisper_sign_off_over_silence_is_dropped() {
        // The stock sign-off, heard as no speech: a hallucination.
        assert!(silence_hallucination("Thank you.", Some(0.9), None));
        assert!(silence_hallucination("Thanks for watching!", None, Some(-1.6)));
        assert!(silence_hallucination("you", Some(0.8), None));
        // Confident speech with the same words is kept, and so is a sign-off with no silence evidence.
        assert!(!silence_hallucination("Thank you.", Some(0.1), Some(-0.2)));
        assert!(!silence_hallucination("Thank you.", None, None));
        assert!(!silence_hallucination("you", Some(0.1), None));
        // Only the whole segment is a sign-off, not a phrase that contains one.
        assert!(!silence_hallucination("we thank you for coming", Some(0.9), None));
    }

    #[test]
    fn a_rerun_replaces_the_cues_it_hears_again_and_keeps_another_speaker() {
        let cue = |start: f64, end: f64, text: &str| subtitle::Cue {
            start_us: (start * 1e6) as i64,
            end_us: (end * 1e6) as i64,
            text: text.to_string(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        };
        // The re-run wrote these: the same words, one of them cut shorter.
        let new = [cue(0.0, 1.5, "Edith makes editing fast."), cue(2.0, 3.5, "We cut the silence.")];
        // The same speech from the first run: replaced.
        assert!(heard_again(&cue(0.0, 1.6, "Edith makes editing fast."), &new));
        assert!(heard_again(&cue(2.0, 3.6, "We cut the silence, then"), &new));
        // Another speaker over the same stretch with other words: kept.
        assert!(!heard_again(&cue(0.0, 1.5, "Good morning everyone."), &new));
        // The same words where the re-run has no cue: kept.
        assert!(!heard_again(&cue(6.0, 7.0, "Edith makes editing fast."), &new));
        // An empty cue has nothing to match and is kept.
        assert!(!heard_again(&cue(0.0, 1.5, "  "), &new));
    }

    /// A mix with an offline source is refused by name, with the fix, before any render.
    #[test]
    fn a_transcript_of_the_mix_with_an_offline_source_is_refused_by_name() {
        let td = crate::testutil::TestDir::new("listen-offline-mix");
        let gone = td.path().join("gone.mp4");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4"), &gone).expect("copy");
        let project = td.path().join("offline.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [gone]})).is_error);
        assert!(!call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]})).is_error);
        assert!(!call(&mut server, "project_save", json!({})).is_error);
        std::fs::remove_file(&gone).expect("remove");

        let mut opened = Server::new();
        assert!(!call(&mut opened, "project_open", json!({"path": project})).is_error);
        for name in ["transcript", "auto_captions"] {
            let r = call(&mut opened, name, json!({"target": "mix"}));
            assert!(r.is_error, "{name}: {}", r.text);
            assert!(r.text.starts_with("rendering the mix refused: 1 source(s) offline"), "{name}: {}", r.text);
            assert!(r.text.contains("src:0") && r.text.contains("media_relink"), "{name}: {}", r.text);
        }
    }

    #[test]
    fn a_second_default_caption_file_gets_the_next_free_name() {
        let base = PathBuf::from("/x/talk2.captions.srt");
        assert_eq!(pick_free(base.clone(), |_| false), base);
        let taken: Vec<PathBuf> = vec![base.clone(), PathBuf::from("/x/talk2.captions-2.srt")];
        assert_eq!(
            pick_free(base.clone(), |p| taken.iter().any(|t| t == p)),
            PathBuf::from("/x/talk2.captions-3.srt")
        );
    }

    #[test]
    fn a_rerun_that_replaces_every_cue_writes_a_new_file_and_leaves_the_old_one() {
        // The old track's file is held by a track, so a re-run takes the next free name and never writes over it.
        let base = PathBuf::from("/x/talk4.captions.srt");
        let old = PathBuf::from("/x/talk4.captions.srt");
        let taken = [old.clone()];
        let rerun = pick_free(base.clone(), |p| taken.iter().any(|t| t == p));
        assert_eq!(rerun, PathBuf::from("/x/talk4.captions-2.srt"));
        assert_ne!(rerun, old);
    }

    #[test]
    fn a_match_padding_stops_at_its_clip_edges() {
        // Clip 450..600 on the lanes: a match at its head with 2 frames of pad would reach 448,
        // into the gap before the clip, and the ripple would move the clip. It stops at 450.
        let clips = [(100, 300), (450, 600)];
        assert_eq!(clamp_to_clips((448, 470), &clips, 452, 470), (450, 470));
        // The end pad past the clip's end stops at the end.
        assert_eq!(clamp_to_clips((590, 603), &clips, 592, 600), (590, 600));
        // A match inside a clip with room either side keeps its span.
        assert_eq!(clamp_to_clips((120, 140), &clips, 125, 135), (120, 140));
        // A match on no clip is left as it was.
        assert_eq!(clamp_to_clips((310, 320), &clips, 312, 318), (310, 320));
    }

    #[test]
    fn foreign_run_inside_a_latin_text_is_taken_out_and_named() {
        let (text, run) = strip_foreign("SUBSCRIBE for more videos and turn on notifications.锈旗困念拜讶 The town has changed", true);
        assert_eq!(text, "SUBSCRIBE for more videos and turn on notifications. The town has changed");
        assert_eq!(run.as_deref(), Some("锈旗困念拜讶"));
        // Accented Latin, punctuation and text with no Latin letters are left as they are.
        assert_eq!(strip_foreign("café at 5 p.m. — ok", true), ("café at 5 p.m. — ok".to_string(), None));
        assert_eq!(strip_foreign("你好世界", false), ("你好世界".to_string(), None));
    }

    #[test]
    fn stock_phrases_and_doubtful_segments_are_flagged_but_kept() {
        let mk = |text: &str, no_speech: Option<f64>, avg: Option<f64>, span: Option<f64>| Seg {
            start_f: 0,
            end_f: 50,
            text: text.to_string(),
            at: "A3".into(),
            words: Vec::new(),
            approx: false,
            no_speech,
            avg_logprob: avg,
            span_db: span,
            stripped: None,
        };
        // A stock phrase flags only with weak audio under it.
        assert_eq!(mk("SUBSCRIBE for more videos.", Some(0.4), Some(-0.1), Some(80.0)).suspect(), vec!["stock video phrase"]);
        assert!(mk("SUBSCRIBE for more videos.", Some(0.0), Some(-0.1), Some(80.0)).suspect().is_empty());
        assert_eq!(mk("All right.", Some(0.1), Some(-0.2), Some(3.0)).suspect(), vec!["quiet audio under it"]);
        assert!(mk("The harbour was busy.", Some(0.01), Some(-0.1), Some(60.0)).suspect().is_empty());
        assert_eq!(mk("Hm.", Some(0.7), Some(-0.2), None).suspect(), vec!["the model doubted there was speech"]);
        assert_eq!(mk("你好世界", None, None, None).suspect(), vec!["text in a non-Latin script"]);
        // Unknown levels and confidence flag nothing.
        assert!(mk("All right.", None, None, None).suspect().is_empty());
    }

    #[test]
    fn stock_phrase_in_clear_speech_is_not_flagged() {
        let mk = |text: &str, no_speech: Option<f64>, avg: Option<f64>, span: Option<f64>| Seg {
            start_f: 0,
            end_f: 50,
            text: text.to_string(),
            at: "V1".into(),
            words: Vec::new(),
            approx: false,
            no_speech,
            avg_logprob: avg,
            span_db: span,
            stripped: None,
        };
        // Narration ending in a stock phrase, heard well and loud: speech, not a hallucination.
        let said = "That is all. Thanks for watching.";
        assert!(mk(said, Some(0.01), Some(-0.2), Some(45.0)).suspect().is_empty());
        assert!(mk(said, None, None, None).suspect().is_empty());
        // The same phrase over a quiet span, or doubted as speech, is flagged.
        assert_eq!(mk(said, Some(0.01), Some(-0.2), Some(12.0)).suspect(), vec!["stock video phrase", "quiet audio under it"]);
        assert_eq!(mk("Thanks for watching.", Some(0.6), Some(-0.2), Some(45.0)).suspect(), vec!["stock video phrase", "the model doubted there was speech"]);
        assert_eq!(mk(said, Some(0.35), Some(-0.2), Some(45.0)).suspect(), vec!["stock video phrase"]);
        // Low confidence is weak audio too.
        assert_eq!(mk(said, Some(0.01), Some(-1.4), Some(45.0)).suspect(), vec!["stock video phrase", "low confidence"]);
    }

    #[test]
    fn flagged_segment_keeps_its_text_and_the_reply_names_it_and_the_removed_run() {
        let raw = r#"{"start":0.0,"end":4.0,"text":"Turn on notifications.锈旗困念拜讶 The town has changed.","words":[{"w":"notifications.锈旗困念拜讶","start":1.0,"end":2.0,"p":0.9},{"w":"The","start":2.1,"end":2.3,"p":0.9},{"w":"town","start":2.3,"end":2.6,"p":0.9},{"w":"has","start":2.6,"end":2.8,"p":0.9},{"w":"changed.","start":2.8,"end":3.6,"p":0.9}],"approx":false,"no_speech":0.4,"avg_logprob":-0.3,"span_db":55.0}"#;
        let p = Piece {
            label: "A3".into(),
            path: PathBuf::from("narration.wav"),
            stream: 0,
            from: 0.0,
            to: 4.0,
            keep: (0.0, 4.0),
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        let segs = raw_to_segs(raw, &p, 25.0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Turn on notifications. The town has changed.");
        assert_eq!(segs[0].words[0].w, "notifications.");
        assert_eq!(segs[0].stripped.as_deref(), Some("锈旗困念拜讶"));
        assert_eq!(segs[0].span_db, Some(55.0));
        let note = review_text(&segs.iter().collect::<Vec<_>>(), 25.0);
        assert!(
            note.contains("check 1 segment(s) that may be made up (kept, not removed): A3 0.00s 'Turn on notifications. The town has changed.' (stock video phrase)"),
            "{note}"
        );
        assert!(note.contains("removed foreign-script text from 1 segment(s), the rest kept: A3 0.00s '锈旗困念拜讶'"), "{note}");
        // A clean, audible segment gets no note at all.
        let clean = Seg { stripped: None, span_db: Some(60.0), ..segs[0].clone() };
        let clean = Seg { text: "The town has changed.".into(), ..clean };
        assert!(review_text(&[&clean], 25.0).is_empty());
    }

    /// The reply names what a cut did to the captions: a cue it moved and one it trimmed are counted,
    /// a cue whose words are gone is named at the start it had, and nothing is said when none changed.
    #[test]
    fn caption_changes_counts_moved_and_trimmed_cues_and_names_removed_ones() {
        let before = vec![
            (0, 1_000_000, "one".to_string()),
            (2_000_000, 3_000_000, "two".to_string()),
            (4_000_000, 5_000_000, "gone".to_string()),
            (6_000_000, 8_000_000, "keep this words here".to_string()),
        ];
        // "two" moved and shortened; the last cue was split, and its first words are what is left.
        let after = vec![
            (0, 1_000_000, "one".to_string()),
            (1_000_000, 1_500_000, "two".to_string()),
            (6_000_000, 7_000_000, "keep this".to_string()),
        ];
        assert_eq!(caption_changes(&before, &after), "; captions followed the cut: 1 moved, 2 trimmed; removed cue 'gone' at 4.0s");
        assert_eq!(caption_changes(&before, &before), "");
    }

    /// A cue that begins a sentence starts with a capital: the first cue, one after a cue ending in
    /// . ! or ?, and one where a transcript segment starts. A cue that continues a sentence keeps its case.
    #[test]
    fn auto_captions_capitalises_the_cues_that_begin_a_sentence() {
        let cue = |start_us: i64, text: &str| subtitle::Cue { start_us, end_us: start_us + 1_000_000, text: text.to_string(), image: None, words: Vec::new(), karaoke: Vec::new() };
        let mut cues = vec![
            cue(0, "hello there."),
            cue(2_000_000, "how are you? fine"),
            cue(4_000_000, "ok"),
            cue(5_000_000, "and more"),
            cue(6_000_000, "élan"),
        ];
        capitalise_sentence_starts(&mut cues, &[4_000_000, 6_000_000]);
        let got: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, ["Hello there.", "How are you? fine", "Ok", "and more", "Élan"]);
    }

    /// Captions ride a cut: a scope that does not name a caption lane gets it, and a locked one stays
    /// out and is returned for the reply to name.
    #[test]
    fn captions_ride_a_cut_unless_their_lane_is_locked() {
        let mut srv = av_server();
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "subtitles_import", "path": srt.display().to_string()},
                {"op": "captions_place", "track": 0}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let s1 = parse::lane("S1").unwrap();
        let picture = vec![Lane::new(LaneKind::Video, 0), Lane::new(LaneKind::Audio, 0)];
        let mut scope = picture.clone();
        assert!(ride_captions(srv.session.as_ref().unwrap(), &mut scope).is_empty());
        assert_eq!(scope.iter().filter(|&&l| l == s1).count(), 1, "{scope:?}");

        let locked = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "lane_flags", "lane": "S1", "locked": true}]}));
        assert!(!locked.is_error, "{}", locked.text);
        let mut scope = picture.clone();
        assert_eq!(ride_captions(srv.session.as_ref().unwrap(), &mut scope), vec![s1]);
        assert_eq!(scope, picture, "a locked caption lane is not in the scope");
    }

    /// A transcript segment at frames `start..end` (25 fps) with `avg_logprob`, for review tests.
    fn seg_at(start_f: u32, end_f: u32, text: &str, avg_logprob: f64) -> Seg {
        Seg {
            start_f,
            end_f,
            text: text.into(),
            at: "A1".into(),
            words: Vec::new(),
            approx: false,
            no_speech: None,
            avg_logprob: Some(avg_logprob),
            span_db: None,
            stripped: None,
        }
    }

    #[test]
    fn a_segment_whose_cues_were_dropped_over_silence_is_not_reviewed() {
        // "out with hatred." (5 s to 7 s) is over silence: its cue is dropped and the reply names it.
        // "kept words" (0 s to 2 s) keeps its cue, and is still checked.
        let dropped = vec![cue(5.0, 7.0, "out with hatred.")];
        let segs = vec![seg_at(125, 175, "out with hatred.", -5.0), seg_at(0, 50, "kept words", -5.0)];
        assert!(!review_text(&[&segs[0]], 25.0).is_empty(), "the segment is flagged on its own");
        let to_us = |f: u32| (f64::from(f) / 25.0 * 1e6).round() as i64;
        let reviewed = review_segs(&segs, &dropped, to_us);
        assert_eq!(reviewed.len(), 1, "the dropped segment is left out");
        assert_eq!(reviewed[0].text, "kept words");
        let note = review_text(&reviewed, 25.0);
        assert!(note.contains("kept words") && !note.contains("out with hatred"), "{note}");
    }

    #[test]
    fn a_segment_mostly_under_a_silent_cue_is_over_dropped() {
        let silent = vec![cue(5.0, 6.0, "x")];
        let us = |s: f64| (s * 1e6) as i64;
        // More than half of the span is under the cue.
        assert!(over_dropped((us(4.5), us(5.8)), &silent));
        // A sliver under the cue, or none at all, is not.
        assert!(!over_dropped((us(4.0), us(5.2)), &silent));
        assert!(!over_dropped((us(0.0), us(4.5)), &silent));
        assert!(!over_dropped((us(6.0), us(7.0)), &silent), "touching is not under");
        assert!(!over_dropped((us(0.0), us(1.0)), &[]), "no cues dropped");
    }

    #[test]
    fn find_context_leaves_out_words_over_silence_or_in_a_segment_that_may_be_made_up() {
        // Words 2 (over silence) and 5 (in a flagged segment) stay out of the context, in order.
        let kept = context_indices(0..7, |j| Ok(j != 2 && j != 5)).unwrap();
        assert_eq!(kept, vec![0, 1, 3, 4, 6]);
        // Nothing usable: an empty context, not the neighbours.
        assert!(context_indices(0..3, |_| Ok(false)).unwrap().is_empty());
        // A failed silence check is an error for the whole find, not a guessed context.
        assert!(context_indices(0..3, |j| if j == 1 { Err("no sound".into()) } else { Ok(true) }).is_err());
    }

    #[test]
    fn a_disfluency_whisper_writes_is_kept_in_the_transcript_words() {
        // Whisper's "Um," at 4.58 s is a word of the guest's first segment: it is not dropped
        // by the silence or stock-phrase checks, so a search for "um" can find it.
        let raw = r#"{"start":4.58,"end":9.52,"text":"Um, yes, because when we talk.","words":[{"w":"Um,","start":4.58,"end":5.1,"p":0.9},{"w":"yes,","start":5.14,"end":5.6,"p":0.9},{"w":"because","start":5.8,"end":6.04,"p":0.9},{"w":"when","start":6.04,"end":6.36,"p":0.9},{"w":"we","start":6.36,"end":6.54,"p":0.9},{"w":"talk.","start":6.54,"end":6.9,"p":0.9}],"approx":false,"no_speech":0.0,"avg_logprob":-0.2,"span_db":90.0}"#;
        let p = Piece {
            label: "A2".into(),
            path: PathBuf::from("guest.wav"),
            stream: 0,
            from: 0.0,
            to: 10.0,
            keep: (0.0, 10.0),
            removed: Vec::new(),
            map: Map::Clip { start: 0, in_frame: 0, speed: Speed::NORMAL },
            mix: false,
        };
        let segs = raw_to_segs(raw, &p, 25.0).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].words[0].w, "Um,");
        assert_eq!(norm(&segs[0].words[0].w), "um");
        assert_eq!(segs[0].start_f, 115, "the segment keeps its own start (4.58 s at 25 fps, rounded)");
    }

    fn texts(segs: &[Seg]) -> Vec<String> {
        segs.iter().flat_map(|g| g.words.iter().map(|w| w.w.clone())).collect()
    }

    #[test]
    fn a_heard_name_is_respelt_to_its_hint_term_and_keeps_its_timing() {
        let mut segs = vec![seg_of(&[("Jane", 10, 14), ("Dough", 15, 20), ("said", 21, 25), ("Jane", 30, 34), ("Dough,", 35, 40)])];
        let fixes = correct_hints(&mut segs, "Jane Doe, LUFS");
        assert_eq!(fixes, vec![("Jane Dough".to_string(), "Jane Doe".to_string(), 2)]);
        assert_eq!(texts(&segs), vec!["Jane", "Doe", "said", "Jane", "Doe,"]);
        assert_eq!(segs[0].text, "Jane Doe said Jane Doe,", "the text follows the words");
        let times: Vec<(u32, u32)> = segs[0].words.iter().map(|w| (w.start_f, w.end_f)).collect();
        assert_eq!(times, vec![(10, 14), (15, 20), (21, 25), (30, 34), (35, 40)]);
        assert_eq!(hint_note(&fixes), "hint corrected: 'Jane Dough' → 'Jane Doe' ×2");
    }

    #[test]
    fn a_word_that_sounds_like_a_term_is_respelt() {
        // "loofs" and "LUFS": the same first letter and Soundex key L120, four letters or more each side.
        let mut segs = vec![seg_of(&[("the", 0, 2), ("LOOFS", 3, 9)])];
        let fixes = correct_hints(&mut segs, "LUFS");
        assert_eq!(texts(&segs), vec!["the", "LUFS"]);
        assert_eq!(fixes, vec![("LOOFS".to_string(), "LUFS".to_string(), 1)]);
    }

    #[test]
    fn lose_is_not_lufs_and_the_is_not_theo() {
        let mut segs = vec![seg_of(&[("we", 0, 2), ("lose", 3, 6), ("the", 7, 9), ("track", 10, 15)])];
        let fixes = correct_hints(&mut segs, "LUFS, Theo");
        assert!(fixes.is_empty(), "no change for common words: {fixes:?}");
        assert_eq!(texts(&segs), vec!["we", "lose", "the", "track"]);
    }

    #[test]
    fn a_common_word_never_takes_a_single_word_term_but_a_multi_word_run_can() {
        let mut segs = vec![seg_of(&[("will", 0, 4)])];
        assert!(correct_hints(&mut segs, "Will").is_empty());
        assert_eq!(texts(&segs), vec!["will"]);
        let mut segs = vec![seg_of(&[("wil", 0, 4), ("smith", 5, 9)])];
        let fixes = correct_hints(&mut segs, "Will Smith");
        assert_eq!(texts(&segs), vec!["Will", "Smith"]);
        assert_eq!(fixes, vec![("wil smith".to_string(), "Will Smith".to_string(), 1)]);
    }

    #[test]
    fn short_terms_match_only_exactly_and_a_case_fix_is_a_change() {
        let mut segs = vec![seg_of(&[("ay", 0, 2)]), seg_of(&[("lufs", 3, 6)])];
        let fixes = correct_hints(&mut segs, "AI, LUFS");
        assert_eq!(texts(&segs), vec!["ay", "LUFS"], "'ay' is two letters: not a fuzzy match of 'AI'");
        assert_eq!(fixes, vec![("lufs".to_string(), "LUFS".to_string(), 1)]);
    }

    #[test]
    fn punctuation_around_a_respelt_word_is_kept() {
        assert_eq!(with_affixes("Dough,", "Doe"), "Doe,");
        assert_eq!(with_affixes("\"LOOFS\"", "LUFS"), "\"LUFS\"");
    }

    #[test]
    fn hint_terms_are_the_comma_separated_names_without_the_filler_prompt() {
        let words: Vec<Vec<String>> = hint_terms(&with_fillers("Jane Doe, LUFS.", true)).into_iter().map(|t| t.words).collect();
        assert_eq!(words, vec![vec!["Jane".to_string(), "Doe".to_string()], vec!["LUFS".to_string()]]);
        assert!(hint_terms(&with_fillers("", true)).is_empty());
        assert!(hint_terms("").is_empty());
    }

    #[test]
    fn soundex_keys_match_the_standard_codes() {
        assert_eq!(soundex("robert"), "R163");
        assert_eq!(soundex("rupert"), "R163");
        assert_eq!(soundex("ashcraft"), "A261", "H between two S-like codes does not split the key");
        assert_eq!(soundex("tymczak"), "T522");
        assert_eq!(soundex("lufs"), "L120");
        assert_eq!(soundex("lose"), "L200");
    }

    #[test]
    fn edit_distance_counts_each_change_once() {
        assert_eq!(edit_distance("janedough", "janedoe"), 3);
        assert_eq!(edit_distance("lose", "lufs"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
    }

    #[test]
    fn the_quietest_frame_in_range_is_the_edge_snaps_to() {
        // Speech at -6 dB, a pause at frame 12.
        let levels: Vec<(u32, f64)> = (0..30u32).map(|f| (f, if f == 12 { -60.0 } else { -6.0 })).collect();
        assert_eq!(quietest_frame(&levels, 10, 5, 20), Some(12));
    }

    #[test]
    fn a_pause_outside_the_range_is_not_taken_and_speech_is_never_a_snap_point() {
        let levels = [(3, -60.0), (10, -6.0), (25, -20.0)];
        assert_eq!(quietest_frame(&levels, 10, 0, 20), Some(3), "a pause in range is taken");
        assert_eq!(quietest_frame(&levels, 10, 5, 20), None, "the pause at 3 is out of range, and speech at 10 is not a pause");
        assert_eq!(quietest_frame(&levels, 10, 26, 30), None);
    }

    #[test]
    fn equally_quiet_frames_take_the_nearest_to_the_edge() {
        let levels = [(4, -50.0), (9, -50.0), (14, -50.0)];
        assert_eq!(quietest_frame(&levels, 10, 0, 20), Some(9));
    }

    #[test]
    fn a_snapped_edge_keeps_its_margin_from_the_word_on_its_side() {
        assert_eq!(snap_range(105, 15, Some(100), None, 3), (103, 120), "a start stays 3 frames past the word before");
        assert_eq!(snap_range(195, 15, None, Some(200), 3), (180, 197), "an end stays 3 frames before the word after");
        assert_eq!(snap_range(50, 15, None, None, 3), (35, 65), "no neighbour: only the half window bounds it");
        // The deepest pause is at 101, inside the word before: it is not taken.
        let levels = [(101, -90.0), (104, -60.0), (110, -10.0)];
        let (lo, hi) = snap_range(105, 15, Some(100), None, 3);
        assert_eq!(quietest_frame(&levels, 105, lo, hi), Some(104));
    }

    #[test]
    fn a_synthetic_level_curve_puts_an_edge_in_its_pause() {
        // 0.2 s of tone, 0.05 s of silence, 0.2 s of tone at 16 kHz: blocks 20..25 are silent.
        let rate = RATE as usize;
        let mut samples = vec![0.5f32; rate / 5];
        samples.extend(vec![0.0f32; rate / 20]);
        samples.extend(vec![0.5f32; rate / 5]);
        let dbs = block_db(&samples);
        assert_eq!(dbs.len(), 45);
        assert!(dbs[0] > -10.0 && dbs[22] < -100.0, "tone at {:.1} dB, silence at {:.1} dB", dbs[0], dbs[22]);
        // At 100 fps each block is one frame: the silent blocks are frames 21..=25.
        let levels = level_frames(&dbs, 0.0, 100.0, Map::Mix);
        let (lo, hi) = snap_range(16, 20, None, None, 3);
        assert_eq!(quietest_frame(&levels, 16, lo, hi), Some(21));
        // A window of tone alone has no pause: the edge stays where Whisper put it.
        assert_eq!(quietest_frame(&levels, 10, 5, 15), None);
    }

    #[test]
    fn a_snap_note_states_the_mean_move_over_the_edges_read() {
        let sn = Snapped { spans: Vec::new(), edges: 4, moved: 2, frames: 8 };
        assert_eq!(snap_note(&sn, 25.0), "edges snapped to quiet points: avg 80 ms (2 of 4 moved; the rest had no pause in reach)");
        assert!(snap_note(&Snapped::default(), 25.0).starts_with("edges not snapped"));
    }

    #[test]
    fn find_takes_snap_as_a_boolean_option() {
        assert_eq!(find_schema()["properties"]["snap"]["type"], "boolean");
    }
}
