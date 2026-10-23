//! Hear tools: `levels` (how loud the sound is over time), `silence` (where it is
//! quiet, in timeline frames) and `cut_silence` (cut those stretches out of the
//! timeline as one undo step).
//!
//! Silence is found per clip, the way the GUI's silence card does it
//! (`player/cards.rs::scan_silences`): each clip's own in..out range is read with
//! `silence::levels`, the quiet runs become timeline frames through
//! `silence::timeline_regions` (which applies the clip's in point and speed), and
//! the cut goes to `cut_regions` on every lane, or the lanes named, as one edit.
//! A lane, and the mix, are read along the timeline instead: gaps between clips
//! are silence there, so they are found and cut too.

use std::time::Instant;

pub(crate) use engine::normalize::{offline_refusal, offline_refusal_for};
use engine::limiter::Limiter;
use engine::loudness::{self, Loudness, gain_to_target};
use engine::normalize::{Plan, duck_offsets, measure_lane, measure_mix, plan_of, render_mix, set_plans, shifted};
use engine::project::{
    GainKey, Lane, LaneKind, MAX_CLIP_GAIN_DB, MAX_GAIN_DB, MIN_GAIN_DB, RedoCheckpoint, gain_keys_db_at,
};
use engine::silence::{self, SILENT_DB, Settings, WINDOWS_PER_SEC};
use engine::{Clip, PlaybackSession};
use serde_json::{Value, json};

use crate::parse;
use crate::server::{Entry, Reply, Server, Tool, opt_bool, opt_f64, opt_str, tri};

const NO_PROJECT: &str = "no project open — call project_open or project_new first";

/// Reply text stays under this, per MCP.md 3.3.
const CAP: usize = 4096;
/// Silence lines listed before the rest are counted.
const MAX_LINES: usize = 60;
/// Values kept in the structured `levels` output.
const MAX_VALUES: usize = 6000;

const LEVELS_DOC: &str = r#"Loudness over time: RMS dBFS per 20 ms window, grouped into per_sec values (power mean of each group), as text rows plus a summary.
target: a clip (A1@0) reads its own sound over its in..out span; a lane (A1) reads its clips in timeline time, with gaps as -120; src:N reads the whole source file, and range is then in source time; mix reads the timeline as exported (every unmuted lane at its gain), rendered to a temporary WAV first.
Clip gain, its volume envelope and lane gain are added to the levels of an audio clip or lane (so they read as played), and a lane under a duck includes the duck's dips, as the mix plays it; fades and the equalizer are not, use loudness for those.
range {from,to}: narrows the span. Timeline frames for a clip, lane or mix, source time for src:N. Defaults to the whole span.
per_sec: a number, the buckets per second (how many values each second of sound gets), e.g. 10; default 2, max 50. The text is capped near 4 KB; a coarser per_sec shows the rest.
target is required. Read only; changes nothing."#;

const SILENCE_DOC: &str = r#"Find the quiet stretches (silence) in the sound, per clip, and list them as timeline frames. Read only.
target: a clip (A1@0), a lane (A1, the default), src:N, or mix. A lane is judged inside its clips only: the timeline gaps between clips are structure, not silence, so they are never listed (the reply names how many were left alone). src:N scans the source file's own sound from its start, so its times are source time (frames at the timeline fps), not timeline time. mix scans the timeline as exported, every unmuted lane together, in timeline time (rendered to a temporary WAV first).
Clip gain, its volume envelope and lane gain are applied to the sound before it is judged, so a quiet clip reads quiet as it plays, and a lane under a duck is judged with the duck (fades and the equalizer are not).
threshold_db: at or below this is quiet. Default: measured from the target, 30 dB under its median speech level (windows above -60 dBFS), kept between -90 and -10; the reply names the threshold used. min_silence: a quiet run shorter than this is ignored, judged before the padding is taken off (default 0.5 s). padding: kept at both ends of each quiet run, so a cut never touches speech (default 0.15 s). min_keep: a kept sliver shorter than this joins the silences either side of it (default 0: no joins).
Structured regions are [from, to] pairs of frame strings such as "450f", exactly what edit's cut_regions op takes (a JSON number there would be seconds, so these are strings on purpose)."#;

const CUT_SILENCE_DOC: &str = r#"Tighten the silences that silence finds on the timeline, as one undo step. The regions are exactly those silence reports for the same target and settings (the target defaults to lane A1). Each silence is cut back so the pause left in it is keep seconds, a natural pause surviving as a short beat instead of being removed; a silence at or under keep is left alone. Only the sound inside clips is judged: the timeline gaps between a lane's clips (a lead-in, the space between takes) are structure, never cut, and the reply names how many were left alone. src:N is refused: a source is not on the timeline, so silence it for its times and cut the lane or clip that plays it. Caption lanes ripple with the cut; locked caption lanes stay.
keep: the TOTAL pause left in each silence, in seconds, padding included (0.05 to 10, default 0.4; to delete pauses entirely use cut_regions). The padding at each end of a silence is never cut and counts toward keep: a 0.4 s keep with 0.15 s padding leaves 0.15 s at each end and 0.10 s of quiet between them, 0.40 s in all. When keep is under twice the padding, the pause left is 2 × padding, the least a cut can leave. A silence that runs to the end of its clip keeps keep in all after the last speech (its end padding is part of that), so a tail never shrinks under keep. The reply measures it.
force: a cut that would remove more than half the target's sound is refused, with its spans named, so a threshold that is too loose cannot wipe a take. force:true cuts anyway.
lanes: the lanes to cut. With a target, lanes is the RIPPLE SCOPE: the silence is judged on the target alone, and the same spans are cut on every listed lane (the target's own lane is added when it is not listed). Default with no lanes: the target's lane and the lanes of clips linked (grouped) to it, so linked video follows the sound while picture-only B-roll stays put; a mix target cuts every lane. A take must stay whole, or the engine refuses and says why; a lane not named never moves, and the reply names it as left in place (a music bed: name it in lanes to move it too).
JOINT CUT: lanes:["A1","A2"] (two speakers in a conversation, say) with no target. The silence is where EVERY listed audio lane is quiet at once: each lane is judged on its own clips at its own threshold (or threshold_db for all when given), and a timeline gap on a lane counts as quiet for it, so the joint silence is a stretch where the other listed lanes are quiet too, and a stretch where no listed lane has a clip is never cut. The cut is made on the listed lanes together, so their turns stay in step. A video lane in a joint list is cut with the spans but not judged, and a joint cut needs two or more audio lanes in its list. A target with lanes is never a joint cut: there lanes is the ripple scope, judged on the target alone. Locks follow the ripple rule: a locked target, or a locked lane named in lanes, refuses the cut with the lane_flags way out; a mix's default scope (every lane) refuses the same way, naming each locked lane, and changes nothing.
dry_run: report what would be cut, then roll back. Nothing changed and no undo step is added.
The reply names the pause each cut left as "pauses now X s", measured back from the sound after the cut: the lane's for a lane or clip, the mix as exported for a mix, the joint quiet for a joint cut (frames are 1/fps, so a keep that is not a whole frame reads a frame over at most)."#;

/// The pause cut_silence leaves at each cut when neither `keep` nor `padding` is given.
const DEFAULT_KEEP: f64 = 0.4;

const LOUDNESS_DOC: &str = r#"EBU R128 loudness of the sound (integrated LUFS, loudness range, true peak, max momentary and short-term). Read only.
target: a clip (A1@0), a lane (A1), src:N (a whole source file), or "mix" (the timeline as exported, every unmuted lane at its gain).
A clip or lane reads as it plays: clip speed, clip gain and envelope, clip fades, equalizer, lane gain and the lane's duck all included (a lane reads its clips with their gains; a clip reads its own span). A video clip reads its source's sound through its in point and speed, with no gain of its own.
range {from,to}: narrows the span. Timeline frames for a clip, lane or mix; source frames for src:N.
target is required: a clip (A1@0), a lane (A1), src:N, or mix.
"mix" renders the whole timeline to a temporary WAV first, so it takes about as long as an export of the sound. Figures show "silent" where there is no energy."#;

const NORMALIZE_DOC: &str = r#"Set the volume so the sound measures a target loudness. Changes lane gain; a boost past the lane's +12 dB cap goes on that lane's clips as clip gain (each up to +24 dB, added to what the clip has), in the same undo step. One undo step. dry_run:true measures and reports the planned gain per lane (predicted from the measurement) and changes nothing.
target: a lane (A1: its gain moves), or "mix" (every unmuted audio lane with clips moves by the same dB, so the mix as a whole lands on the target).
lufs: integrated target, default -14. true_peak: ceiling in dBTP, default -1; the gain is cut back if the peak would pass it.
allow_limiter (target mix only): when the true peak is what holds the gain back, or the mix already sits over the true_peak ceiling (a clipped mix), switch the master limiter on with its true-peak ceiling at the true_peak value and re-gain with it in circuit, all in the same undo step. When the master limiter is already on, a mix normalize counts on it without the flag, at the lower of the limiter's ceiling and true_peak. The limiter's detector is the same 8x true-peak meter the reply reads, so it holds the mix at or under true_peak; a measured overshoot still lowers its ceiling. The limiter is a mix-wide setting, so it is left on afterwards, at the true-peak ceiling reported in the reply.
The figures in the reply are measured after the change, not predicted; when the level misses the target by more than 0.1 LU or the peaks pass the ceiling, the gain is corrected up to eight times; with the limiter on, a true-peak overshoot lowers the limiter's ceiling (down to -12 dBFS) instead of the gain.
Structured output: target, lufs (asked), achieved_lufs, reached (within 0.1 LU and under the ceiling), short_by_lu, capped_by (true_peak, limiter, clip_gain_limit, lane_gain_limit or null), clip_gain (the clips that took a boost: clip, change_db), limiter (the setting it switched on, or null), measured_true_peak_dbtp, refinements.
Common targets: YouTube and Spotify -14 LUFS, Apple Music -16, EBU R128 broadcast -23.
Lane gain stays within -60..+12 dB: the rest of a boost goes on the lane's clips, each within +24 dB; the reply names the split (lane gain, then clip gain per clip). A lane held at both limits is reported with what it wanted and what it reached."#;

/// What a `target` names: one clip, a whole lane, every clip of one source, or the
/// whole timeline as exported.
#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // Clip grew with its effects stack; these are short-lived
enum Target {
    Clip(Lane, Clip),
    Lane(Lane),
    Source(usize),
    Mix,
}

// ---- schemas --------------------------------------------------------------

fn settings_props() -> serde_json::Map<String, Value> {
    match json!({
        "threshold_db": {
            "type": "number", "minimum": -120, "maximum": 0,
            "description": "At or below this level (dBFS) a window is quiet. Default: 30 dB under the target's median speech level, between -90 and -10; the reply names the value used."
        },
        "min_silence": {
            "type": "number", "minimum": 0, "maximum": 60,
            "description": "A quiet run shorter than this many seconds is ignored (judged before the padding). Default 0.5."
        },
        "padding": {
            "type": "number", "minimum": 0, "maximum": 10,
            "description": "Seconds kept at each end of a silence so a cut never touches speech. Default 0.15."
        },
        "min_keep": {
            "type": "number", "minimum": 0, "maximum": 10,
            "description": "Kept sound shorter than this many seconds joins the silences around it. Default 0 (no joins)."
        }
    }) {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

fn target_prop(default: &str) -> Value {
    json!({
        "type": "string",
        "description": format!("A clip address (A1@0), a lane (A1), src:N for a whole source file, or \"mix\" for the timeline as exported. Default {default}.")
    })
}

fn levels_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": "A clip address (A1@0), a lane (A1), src:N for a whole source file, or \"mix\" for the timeline as exported."
            },
            "range": {
                "type": "object",
                "description": "Optional span. Timeline times for a clip, lane or mix, source times for src:N.",
                "properties": {
                    "from": { "description": "Start, e.g. 450f, 15.0s or 15." },
                    "to": { "description": "End, e.g. 900f, 30.0s or end." }
                }
            },
            "per_sec": {
                "type": "number", "exclusiveMinimum": 0, "maximum": 50, "examples": [10],
                "description": "Buckets per second: a number, how many values each second of sound gets (e.g. 10 for ten per second). Default 2, max 50. Higher is finer and longer."
            }
        },
        "required": ["target"]
    })
}

fn silence_schema() -> Value {
    let mut props = settings_props();
    props.insert("target".into(), target_prop("A1"));
    json!({ "type": "object", "properties": props })
}

fn cut_silence_schema() -> Value {
    let mut props = settings_props();
    props.insert("target".into(), target_prop("A1"));
    props.insert(
        "keep".into(),
        json!({
            "type": "number", "minimum": 0.05, "maximum": 10,
            "description": "The total pause left in each silence, in seconds, padding included (cut back to this, not removed; 0.05 to 10). Default 0.4. The padding at each end is never cut and counts toward it."
        }),
    );
    props.insert(
        "force".into(),
        json!({ "type": "boolean", "description": "Cut even when it would remove more than half the target's sound (refused otherwise)." }),
    );
    props.insert(
        "lanes".into(),
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Lanes to cut, e.g. [\"V1\", \"A1\"]. With a target, these are the ripple scope: the silence is judged on the target alone and the same spans are cut on each listed lane (the target's lane is added if not listed). With no target, two or more audio lanes make a JOINT cut: the silence is where every listed audio lane is quiet at once (a gap on a lane counts as quiet; a stretch with no listed clip is never cut), cut from the listed lanes together, so their turns stay in step; a video lane in that list is cut with the spans but not judged. Default with no lanes: the target's lane and the lanes of clips linked to it; a mix target defaults to every lane. A lane not listed never moves and is reported. A locked lane named here refuses the cut (unlock it with lane_flags); a locked lane the default scope reaches through a link refuses too, and a mix's default scope (every lane) refuses the same way."
        }),
    );
    props.insert(
        "dry_run".into(),
        json!({ "type": "boolean", "description": "Report what would be cut, then roll back." }),
    );
    json!({ "type": "object", "properties": props })
}

fn loudness_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": "Required. A clip address (A1@0), a lane (A1), src:N for a whole source file, or \"mix\" for the timeline as exported."
            },
            "range": {
                "type": "object",
                "description": "Optional span. Timeline times for a clip, lane or mix; source times for src:N.",
                "properties": {
                    "from": { "description": "Start, e.g. 450f, 15.0s or 15." },
                    "to": { "description": "End, e.g. 900f, 30.0s or end." }
                }
            }
        },
        "required": ["target"]
    })
}

fn normalize_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dry_run": {
                "type": "boolean",
                "description": "Measure and report the gain each lane would move by, then change nothing (no undo step)."
            },
            "target": {
                "type": "string",
                "description": "A lane (A1) whose gain moves, or \"mix\" for every unmuted audio lane together."
            },
            "allow_limiter": {
                "type": "boolean",
                "description": "Target mix only. If the true peak holds the gain back, turn the master limiter on with its true-peak ceiling at the true_peak value and re-gain, in one undo step. Default false. Not needed when the master limiter is already on: a mix normalize counts on it."
            },
            "lufs": {
                "type": "number", "minimum": -70, "maximum": 0,
                "description": "Integrated loudness to reach, LUFS. Default -14 (YouTube, Spotify). Apple Music -16, EBU R128 broadcast -23."
            },
            "true_peak": {
                "type": "number", "minimum": -20, "maximum": 0,
                "description": "True-peak ceiling in dBTP. Default -1."
            },
            "each": {
                "type": "boolean",
                "description": "With lanes: set each listed lane on its own, so each one alone measures lufs over its clips (its own gain moves, one undo step). Needs lanes; not with target or allow_limiter."
            },
            "lanes": {
                "type": "array", "items": { "type": "string" },
                "description": "With each:true: the audio lanes to set, such as [\"A1\",\"A2\"]."
            }
        }
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "levels",
            description: LEVELS_DOC,
            schema: levels_schema,
            run: run_levels,
        },
        Tool {
            name: "silence",
            description: SILENCE_DOC,
            schema: silence_schema,
            run: run_silence,
        },
        Tool {
            name: "cut_silence",
            description: CUT_SILENCE_DOC,
            schema: cut_silence_schema,
            run: run_cut_silence,
        },
        Tool {
            name: "loudness",
            description: LOUDNESS_DOC,
            schema: loudness_schema,
            run: run_loudness,
        },
        Tool {
            name: "normalize",
            description: NORMALIZE_DOC,
            schema: normalize_schema,
            run: run_normalize,
        },
    ]
}

// ---- targets --------------------------------------------------------------

fn fail(e: String) -> Reply {
    Reply::err(e)
}

fn source_name(s: &PlaybackSession, n: usize) -> String {
    s.sources()
        .get(n)
        .and_then(|src| src.path.file_name())
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What `src:N` is missing from: how many sources the project has, and which they are.
fn source_hint(s: &PlaybackSession) -> String {
    source_count_note(s.sources().len())
}

fn source_count_note(count: usize) -> String {
    match count {
        0 => "this project has no sources".into(),
        1 => "this project has 1 source (src:0)".into(),
        _ => format!("this project has {count} sources (src:0..src:{})", count - 1),
    }
}

/// The refusal for a `src:N` past the project's sources, worded like listen's.
fn no_source(s: &PlaybackSession, n: usize) -> String {
    format!("no source src:{n}: {}", source_hint(s))
}

fn lane_ok(s: &PlaybackSession, lane: Lane, name: &str) -> Result<(), String> {
    if !s.lanes().contains(&lane) {
        return Err(format!("no lane {name} on this timeline"));
    }
    if lane.kind == LaneKind::Subtitle {
        return Err(format!("{name} is a caption lane; scan a media lane"));
    }
    Ok(())
}

/// The one parser of `target` for every hear tool: `mix`, `src:N`, a clip address
/// (A1@0 or A1#0), or a lane (A1).
fn resolve(s: &PlaybackSession, raw: &str) -> Result<Target, String> {
    let t = raw.trim();
    if is_mix(t) {
        return Ok(Target::Mix);
    }
    if let Some(rest) = t.strip_prefix("src:") {
        let digits: String = rest
            .trim()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let n = digits
            .parse::<usize>()
            .map_err(|_| format!("`{t}` is not a source: write src:N"))?;
        if n >= s.sources().len() {
            return Err(no_source(s, n));
        }
        return Ok(Target::Source(n));
    }
    if t.contains(['@', '#']) {
        let (lane, idx) = parse::clip(s, t)?;
        if lane.kind == LaneKind::Subtitle {
            return Err(format!("{t} is a caption; scan a media clip"));
        }
        let clip = s
            .lane_clips(lane)
            .get(idx)
            .cloned()
            .ok_or_else(|| format!("no media clip at {t}"))?;
        return Ok(Target::Clip(lane, clip));
    }
    let lane = parse::lane(t)?;
    lane_ok(s, lane, t)?;
    Ok(Target::Lane(lane))
}

/// The `target` argument, or A1 when it is left out (the silence default).
fn target_arg(s: &PlaybackSession, args: &Value) -> Result<Target, String> {
    match args.get("target") {
        None | Some(Value::Null) => {
            lane_ok(s, Lane::A1, "A1")?;
            Ok(Target::Lane(Lane::A1))
        }
        Some(Value::String(t)) => resolve(s, t),
        Some(_) => Err("`target` must be a clip address (A1@0), a lane (A1), src:N or mix".into()),
    }
}

fn label(s: &PlaybackSession, t: &Target) -> String {
    match t {
        Target::Clip(l, c) => parse::addr(*l, c.start),
        Target::Lane(l) => parse::lane_name(*l),
        Target::Source(n) => format!("src:{n} {}", source_name(s, *n)),
        Target::Mix => "mix".into(),
    }
}

/// The clips a clip or lane target covers. A source or the mix is read as a file,
/// not clip by clip, so it has none here.
fn clips_of(s: &PlaybackSession, t: Target) -> Vec<(Lane, Clip)> {
    match t {
        Target::Clip(l, c) => vec![(l, c)],
        Target::Lane(l) => s.lane_clips(l).iter().map(|c| (l, c.clone())).collect(),
        Target::Source(_) | Target::Mix => Vec::new(),
    }
}

/// The dB a clip's sound moves by on its way out: its clip gain plus its lane
/// gain, on an audio lane. Video has neither, so it is 0. Fades and the
/// equalizer are not in it; `loudness` reads those.
fn played_offset(s: &PlaybackSession, lane: Lane, clip: &Clip) -> f32 {
    if lane.kind != LaneKind::Audio {
        return 0.0;
    }
    let idx = s
        .lane_clips(lane)
        .iter()
        .position(|c| c.start == clip.start);
    let clip_db = idx.and_then(|i| s.clip_gain_of(lane, i)).unwrap_or(0.0);
    clip_db + s.lane_gain_db(lane)
}

/// The loudness of one clip's own source range, one value per 20 ms window from
/// its in point, with the clip's gain and its lane's gain applied (see
/// `played_offset`). `None` for a file with no audio track.
fn read(
    s: &PlaybackSession,
    lane: Lane,
    clip: &Clip,
    fps: f64,
) -> Result<Option<Vec<f32>>, String> {
    if let Some(why) = offline_refusal_for(s, parse::addr(lane, clip.start), &[clip.source]) {
        return Err(why);
    }
    let src = s.sources().get(clip.source).ok_or_else(|| {
        format!(
            "a clip uses src:{}, which is not on the timeline",
            clip.source
        )
    })?;
    let range = (
        f64::from(clip.in_frame) / fps,
        f64::from(clip.out_frame) / fps,
    );
    let levels = silence::levels(&src.path, src.audio_stream, range).map_err(|e| {
        format!(
            "reading the sound of src:{} {}: {e}",
            clip.source,
            source_name(s, clip.source)
        )
    })?;
    let off = played_offset(s, lane, clip);
    // The volume envelope varies over the clip, so each window reads it at its own
    // source time: window i starts i windows past the in point.
    let keys = clip_keys(s, lane, clip);
    let window = 1.0 / f64::from(WINDOWS_PER_SEC);
    Ok(levels.map(|v| {
        if off == 0.0 && keys.is_empty() {
            return v;
        }
        v.into_iter()
            .enumerate()
            .map(|(i, x)| {
                if x > SILENT_DB {
                    let env = if keys.is_empty() {
                        0.0
                    } else {
                        gain_keys_db_at(keys, f64::from(clip.in_frame) + i as f64 * window * fps)
                    };
                    (x + off + env).max(SILENT_DB)
                } else {
                    x
                }
            })
            .collect()
    }))
}

/// The clip's own volume envelope, or none for a video clip or one with no points.
fn clip_keys<'a>(s: &'a PlaybackSession, lane: Lane, clip: &Clip) -> &'a [GainKey] {
    if lane.kind != LaneKind::Audio {
        return &[];
    }
    s.lane_clips(lane)
        .iter()
        .position(|c| c.start == clip.start)
        .map_or(&[][..], |i| s.gain_keys_of(lane, i))
}

// ---- silence --------------------------------------------------------------

fn settings_arg(args: &Value) -> Result<Settings, String> {
    let d = Settings::default();
    let get = |k: &str, dv: f64, lo: f64, hi: f64| -> Result<f64, String> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(dv),
            Some(v) => {
                let x = v
                    .as_f64()
                    .ok_or_else(|| format!("`{k}` must be a number"))?;
                if !x.is_finite() || x < lo || x > hi {
                    Err(format!("`{k}` {x} is out of range; it must be {lo} to {hi}"))
                } else {
                    Ok(x)
                }
            }
        }
    };
    Ok(Settings {
        threshold_db: get("threshold_db", f64::from(d.threshold_db), -120.0, 0.0)? as f32,
        min_silence: get("min_silence", d.min_silence, 0.0, 60.0)?,
        padding: get("padding", d.padding, 0.0, 10.0)?,
        min_keep: get("min_keep", 0.0, 0.0, 10.0)?,
    })
}

/// The silent stretches silence reports. `min_silence` is judged on the quiet run itself, before
/// the padding is taken off its ends (the engine does that): a 0.4 s pause is a pause at a 0.3 s
/// minimum even though 0.1 s of it is padding. Counting the padding first dropped every short
/// narration pause.
fn regions_of(levels: &[f32], cfg: Settings) -> Vec<(f64, f64)> {
    silence::regions(levels, cfg)
}

/// Sorts and joins overlapping or touching `(from, to)` frame spans.
fn merge(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

struct Found {
    /// Timeline `(from, to)` frames, merged and in order.
    regions: Vec<(u32, u32)>,
    /// Clips whose sound was read.
    scanned: usize,
    /// Clips skipped because their file has no audio track.
    no_audio: Vec<String>,
}

/// The silence of the given clips, as timeline frames. A lane's clips that touch (one ends where
/// the next begins) are one stretch of sound: a quiet run that reaches the seam continues into the
/// next clip, so a pause a cut has split across two clips is judged as the one pause it is. The
/// gap between two clips that do not touch is never sound to judge, so a run never crosses it.
/// The settings then apply to each joined run as `silence::regions` applies them to a clip's run.
fn find(s: &PlaybackSession, clips: &[(Lane, Clip)], cfg: Settings) -> Result<Found, String> {
    let fps = parse::session_fps(s);
    let quiet_cfg = Settings {
        min_silence: 0.0,
        padding: 0.0,
        min_keep: 0.0,
        ..cfg
    };
    let mut regions: Vec<(u32, u32)> = Vec::new();
    let mut scanned = 0;
    let mut no_audio = Vec::new();
    let mut lanes: Vec<Lane> = Vec::new();
    for (lane, _) in clips {
        if !lanes.contains(lane) {
            lanes.push(*lane);
        }
    }
    for lane in lanes {
        let mut lane_clips: Vec<&Clip> = clips
            .iter()
            .filter(|(l, _)| *l == lane)
            .map(|(_, c)| c)
            .collect();
        lane_clips.sort_by_key(|c| c.start);
        // The duck of an audio lane, as windows of timeline time.
        let duck = if lane.kind == LaneKind::Audio {
            let to = lane_clips.iter().map(|c| c.end()).max().unwrap_or(0);
            Some(duck_offsets(s, lane, to, fps)?)
        } else {
            None
        };
        // Unpadded quiet runs in timeline frames, joined across touching clips.
        let mut quiet: Vec<(u32, u32)> = Vec::new();
        // Whether the last run read reaches the end of the clip before this one, and where that clip ends.
        let mut open_at_end = false;
        let mut prev_end: Option<u32> = None;
        for clip in lane_clips {
            let mut levels = match read(s, lane, clip, fps)? {
                None => {
                    no_audio.push(format!(
                        "{} (src:{} {} has no audio track)",
                        parse::addr(lane, clip.start),
                        clip.source,
                        source_name(s, clip.source)
                    ));
                    open_at_end = false;
                    prev_end = None;
                    continue;
                }
                Some(levels) => levels,
            };
            scanned += 1;
            if let Some(offsets) = &duck {
                apply_duck(&mut levels, clip, fps, offsets);
            }
            let runs = silence::timeline_regions(clip, fps, &regions_of(&levels, quiet_cfg));
            let touching = open_at_end && prev_end == Some(clip.start);
            for (i, &(a, len)) in runs.iter().enumerate() {
                match quiet.last_mut() {
                    Some(last) if i == 0 && touching && a <= clip.start + 1 => last.1 = a + len,
                    _ => quiet.push((a, a + len)),
                }
            }
            open_at_end = runs.last().is_some_and(|&(a, len)| a + len + 1 >= clip.end());
            prev_end = Some(clip.end());
        }
        regions.extend(padded_runs(&quiet, cfg, fps));
    }
    Ok(Found {
        regions: merge(regions),
        scanned,
        no_audio,
    })
}

/// The silence `find` settles from unpadded quiet runs: a run must be long enough (judged before
/// the padding), its ends are padded, and a sliver of speech shorter than min_keep between two runs
/// is swallowed. `silence::regions` applies the same rules to a clip's run.
fn padded_runs(quiet: &[(u32, u32)], cfg: Settings, fps: f64) -> Vec<(u32, u32)> {
    let pad = (cfg.padding * fps).ceil() as u32;
    let mut padded: Vec<(u32, u32)> = Vec::new();
    for &(a, b) in quiet {
        if f64::from(b - a) / fps < cfg.min_silence {
            continue;
        }
        let (from, to) = (a + pad, b.saturating_sub(pad));
        if to <= from {
            continue;
        }
        match padded.last_mut() {
            Some(last) if f64::from(from.saturating_sub(last.1)) / fps < cfg.min_keep => last.1 = to,
            _ => padded.push((from, to)),
        }
    }
    padded
}

/// The frames both lists of sorted, disjoint `(from, to)` spans share.
fn intersect(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if lo < hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The silence of several lanes judged together: a stretch is silent where every judged lane is
/// quiet at once. A lane is quiet inside its clips at its own threshold (as `find` judges it), and
/// in its timeline gaps and past its last clip (a lane with no clip at a time is silent there). A
/// stretch where no judged lane has a clip is structure, not silence, so it is never listed. The
/// settings then apply to the joint runs as they do to one lane's.
fn find_joint(s: &PlaybackSession, judged: &[(Lane, f32)], cfg: Settings) -> Result<Found, String> {
    let fps = parse::session_fps(s);
    let lane_quiet = Settings {
        min_silence: 0.0,
        padding: 0.0,
        min_keep: 0.0,
        ..cfg
    };
    let mut quiet: Option<Vec<(u32, u32)>> = None;
    let mut sound: Vec<(u32, u32)> = Vec::new();
    let mut scanned = 0;
    let mut no_audio = Vec::new();
    for &(lane, threshold_db) in judged {
        let clips = lane_clips(s, lane);
        let own = if clips.is_empty() {
            vec![(0, u32::MAX)]
        } else {
            let f = find(s, &clips, Settings { threshold_db, ..lane_quiet })?;
            scanned += f.scanned;
            no_audio.extend(f.no_audio);
            let mut v = f.regions;
            v.extend(lane_gaps(s, lane));
            // Past the lane's last clip there is no sound on it either, so it is quiet there too.
            let lane_end = clips.iter().map(|(_, c)| c.end()).max().unwrap_or(0);
            v.push((lane_end, u32::MAX));
            merge(v)
        };
        quiet = Some(match quiet {
            None => own,
            Some(q) => intersect(&q, &own),
        });
        sound.extend(clips.iter().map(|(_, c)| (c.start, c.end())));
    }
    let Some(quiet) = quiet else {
        return Ok(Found {
            regions: Vec::new(),
            scanned,
            no_audio,
        });
    };
    let joint = intersect(&quiet, &merge(sound));
    Ok(Found {
        regions: merge(padded_runs(&joint, cfg, fps)),
        scanned,
        no_audio,
    })
}

/// A cut that would remove more than this share of the target's sound is refused unless forced.
const CUT_SHARE_LIMIT: f64 = 0.5;

/// The first few spans as `start–end s` pairs, the rest counted. Spans are `(start, len)` frames
/// (what `cut_spans_for` returns), so the end is start plus len.
fn span_list(spans: &[(u32, u32)], fps: f64) -> String {
    const SHOWN: usize = 6;
    let mut parts: Vec<String> = spans
        .iter()
        .take(SHOWN)
        .map(|&(at, len)| format!("{:.3}–{:.3}s", parse::secs(at, fps), parse::secs(at + len, fps)))
        .collect();
    if spans.len() > SHOWN {
        parts.push(format!("… {} more", spans.len() - SHOWN));
    }
    parts.join(", ")
}

/// The numbered region lines of the text reply, capped at `MAX_LINES`.
fn region_lines(regions: &[(u32, u32)], fps: f64) -> Vec<String> {
    let mut lines: Vec<String> = regions
        .iter()
        .take(MAX_LINES)
        .enumerate()
        .map(|(i, &(a, b))| {
            format!(
                "{}. {:.3}–{:.3}s ({}f)",
                i + 1,
                parse::secs(a, fps),
                parse::secs(b, fps),
                b - a
            )
        })
        .collect();
    if regions.len() > MAX_LINES {
        lines.push(format!("… {} more", regions.len() - MAX_LINES));
    }
    lines
}

fn region_json(regions: &[(u32, u32)]) -> Value {
    Value::Array(
        regions
            .iter()
            .map(|&(a, b)| json!([format!("{a}f"), format!("{b}f")]))
            .collect(),
    )
}

/// The silence of one target, found where its sound is: a source's own file (in
/// source time), the timeline as exported (the mix), a lane along its timeline, or
/// the clips a clip covers (in timeline time).
fn scan(s: &PlaybackSession, t: Target, cfg: Settings) -> Result<Found, String> {
    match t {
        Target::Source(n) => find_source(s, n, cfg),
        Target::Mix => find_mix(s, cfg),
        Target::Lane(lane) => scan_lane(s, lane, cfg),
        Target::Clip(lane, clip) => find(s, &[(lane, clip)], cfg),
    }
}

/// Seconds regions as frames at `fps`, rounded inward like `timeline_regions` does.
fn frame_regions(regions: &[(f64, f64)], fps: f64) -> Vec<(u32, u32)> {
    regions
        .iter()
        .filter_map(|&(from, to)| {
            let a = (from * fps).ceil().max(0.0) as u32;
            let b = (to * fps).floor().max(0.0) as u32;
            (b > a).then_some((a, b))
        })
        .collect()
}

/// Silence of a whole source file, read from its start. Its regions are source
/// frames at the timeline fps, rounded inward like `timeline_regions` does.
fn find_source(s: &PlaybackSession, n: usize, cfg: Settings) -> Result<Found, String> {
    let fps = parse::session_fps(s);
    let src = s
        .sources()
        .get(n)
        .ok_or_else(|| no_source(s, n))?;
    let name = format!("src:{n} {}", source_name(s, n));
    if let Some(why) = offline_refusal_for(s, &name, &[n]) {
        return Err(why);
    }
    let Some(levels) = silence::levels(&src.path, src.audio_stream, (0.0, f64::INFINITY))
        .map_err(|e| format!("reading the sound of {name}: {e}"))?
    else {
        return Ok(Found {
            regions: Vec::new(),
            scanned: 0,
            no_audio: vec![format!("{name} has no audio track")],
        });
    };
    Ok(Found {
        regions: merge(frame_regions(&regions_of(&levels, cfg), fps)),
        scanned: 1,
        no_audio: Vec::new(),
    })
}

/// Silence of the timeline as exported: the mix rendered to a temporary WAV, which
/// starts at timeline frame 0, so its regions are timeline frames.
fn find_mix(s: &PlaybackSession, cfg: Settings) -> Result<Found, String> {
    let fps = parse::session_fps(s);
    let wav = render_mix(s)?;
    let Some(levels) = silence::levels(&wav.0, 0, (0.0, f64::INFINITY))
        .map_err(|e| format!("reading the mix: {e}"))?
    else {
        return Ok(Found {
            regions: Vec::new(),
            scanned: 0,
            no_audio: vec!["the mix has no sound track".into()],
        });
    };
    Ok(Found {
        regions: merge(frame_regions(&regions_of(&levels, cfg), fps)),
        scanned: 1,
        no_audio: Vec::new(),
    })
}

/// Silence of a lane: the quiet inside its clips, read where each clip sits, with the lane's duck.
/// Only the sound INSIDE clips is judged. The timeline gaps between clips (a title's lead-in, the
/// space between two takes) are structure, not silence, so they are never listed or cut; see
/// [`lane_gaps`] for the count the reply names.
fn scan_lane(s: &PlaybackSession, lane: Lane, cfg: Settings) -> Result<Found, String> {
    if s.lane_clips(lane).is_empty() {
        return Err(format!("{} has no clips to scan", parse::lane_name(lane)));
    }
    let clips: Vec<(Lane, Clip)> = s.lane_clips(lane).iter().map(|c| (lane, c.clone())).collect();
    find(s, &clips, cfg)
}

/// The timeline spans of a lane that no clip covers, as `(from, to)` frames, in order: the lead-in
/// before the first clip and every gap between two clips. Structure, not silence, so they are
/// reported and left alone.
pub(crate) fn lane_gaps(s: &PlaybackSession, lane: Lane) -> Vec<(u32, u32)> {
    let mut clips: Vec<&Clip> = s.lane_clips(lane).iter().collect();
    clips.sort_by_key(|c| c.start);
    let mut gaps = Vec::new();
    let mut cursor = 0u32;
    for c in clips {
        if c.start > cursor {
            gaps.push((cursor, c.start));
        }
        cursor = cursor.max(c.end());
    }
    gaps
}

/// The reply's note on the gaps a lane-level scan or cut leaves alone, or empty when there are none.
fn gap_note(s: &PlaybackSession, lanes: &[Lane], fps: f64) -> String {
    let mut parts = Vec::new();
    for &l in lanes {
        let gaps = lane_gaps(s, l);
        if gaps.is_empty() {
            continue;
        }
        let total: u32 = gaps.iter().map(|&(a, b)| b - a).sum();
        parts.push(format!(
            "{} timeline gap{} on {} left alone ({:.3} s, not silence)",
            gaps.len(),
            if gaps.len() == 1 { "" } else { "s" },
            parse::lane_name(l),
            parse::secs(total, fps)
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join("; "))
    }
}

/// A lane's sound along its timeline, one value per window from timeline 0 to the lane's end:
/// the clips read where they sit, the gaps silent, the lane's duck applied. Also the clips read
/// and the clips with no audio track, by name.
fn lane_windows(s: &PlaybackSession, lane: Lane) -> Result<(Vec<f32>, usize, Vec<String>), String> {
    if s.lane_clips(lane).is_empty() {
        return Err(format!("{} has no clips to scan", parse::lane_name(lane)));
    }
    let fps = parse::session_fps(s);
    let end = lane_end(s, lane);
    let mut reads = Vec::new();
    let mut no_audio = Vec::new();
    for clip in s.lane_clips(lane) {
        match read(s, lane, clip, fps)? {
            None => no_audio.push(format!(
                "{} (src:{} {} has no audio track)",
                parse::addr(lane, clip.start),
                clip.source,
                source_name(s, clip.source)
            )),
            Some(levels) => reads.push((clip.clone(), Some(levels))),
        }
    }
    let scanned = reads.len();
    // Windows from timeline 0 to the lane's end; a gap has no clip, so it reads silent.
    let (_, mut windows) = span_windows(&reads, fps, (0, end));
    let offsets = duck_offsets(s, lane, end, fps)?;
    apply_offsets_to_windows(&mut windows, 0, &offsets);
    Ok((windows, scanned, no_audio))
}

/// The pause each cut joint now sits in, measured back from the sound after the cut: the quiet
/// run around the joint, in seconds, one per span in order. `quiet` is the silence of that sound
/// judged at the same threshold with no padding and no forgiveness, in seconds. Read on the
/// result, so it is what is there, not the plan.
fn measured_pauses(quiet: &[(f64, f64)], spans: &[(u32, u32)], fps: f64) -> Vec<f64> {
    let mut removed = 0u32;
    let mut out = Vec::with_capacity(spans.len());
    for &(at, len) in spans {
        // Each span's joint, on the timeline as it stands after the spans before it are gone.
        let joint = f64::from(at.saturating_sub(removed)) / fps;
        removed = removed.saturating_add(len);
        let pause = quiet
            .iter()
            .find(|&&(a, b)| a - 1e-6 <= joint && joint <= b + 1e-6)
            .map_or(0.0, |&(a, b)| b - a);
        out.push(pause);
    }
    out
}

/// The pauses after a cut, judged the way `silence` judges them: only the sound INSIDE clips, so
/// the timeline gaps between clips are never part of a pause. Quiet runs that touch across a cut
/// seam are one pause, as the sound is. A lane or clip target is read on its own clips (its duck
/// included), a mix as exported, which has no one lane to read and may be cut on lanes that are
/// empty.
fn measured_cut_pauses(
    s: &PlaybackSession,
    target: &Target,
    spans: &[(u32, u32)],
    cfg: Settings,
    fps: f64,
) -> Result<Vec<f64>, String> {
    let quiet_cfg = Settings {
        min_silence: 0.0,
        padding: 0.0,
        min_keep: 0.0,
        ..cfg
    };
    let quiet: Vec<(f64, f64)> = match target {
        Target::Lane(lane) => find(s, &lane_clips(s, *lane), quiet_cfg)?
            .regions
            .iter()
            .map(|&(a, b)| (f64::from(a) / fps, f64::from(b) / fps))
            .collect(),
        Target::Clip(lane, clip) => find(s, &[(*lane, clip.clone())], quiet_cfg)?
            .regions
            .iter()
            .map(|&(a, b)| (f64::from(a) / fps, f64::from(b) / fps))
            .collect(),
        Target::Mix | Target::Source(_) => {
            let wav = render_mix(s)?;
            let levels = silence::levels(&wav.0, 0, (0.0, f64::INFINITY))
                .map_err(|e| format!("reading the mix: {e}"))?
                .unwrap_or_default();
            regions_of(&levels, quiet_cfg)
        }
    };
    Ok(measured_pauses(&quiet, spans, fps))
}

/// The clips of a lane, paired with their lane, as `find` takes them.
fn lane_clips(s: &PlaybackSession, lane: Lane) -> Vec<(Lane, Clip)> {
    s.lane_clips(lane).iter().map(|c| (lane, c.clone())).collect()
}

/// Why cut_silence will not cut a bare source, and what to run instead.
fn source_cut_refusal(s: &PlaybackSession, n: usize) -> String {
    let name = format!("src:{n} {}", source_name(s, n));
    let playing = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio)
        .find_map(|l| {
            s.lane_clips(l)
                .iter()
                .find(|c| c.source == n)
                .map(|c| (l, parse::addr(l, c.start)))
        });
    match playing {
        Some((lane, addr)) => format!(
            "cut_silence needs the timeline: {name} is a source file, not a clip or lane. \
Run silence on src:{n} for its quiet stretches in source time, then cut_silence on the lane that plays it ({}), or on the clip {addr}.",
            parse::lane_name(lane)
        ),
        None => format!(
            "cut_silence needs the timeline: {name} is a source file, and no audio lane plays it. Put it on an audio lane, then cut_silence on that lane or clip."
        ),
    }
}

/// Median speech level (the windows above this) is the reference for an automatic threshold.
const AUTO_ACTIVE_DB: f32 = -60.0;
/// An automatic threshold sits this far under the median speech level.
const AUTO_OFFSET_DB: f32 = 30.0;

/// Whether the caller gave `threshold_db`. When not, the threshold is measured from the target.
fn threshold_given(args: &Value) -> bool {
    matches!(args.get("threshold_db"), Some(v) if !v.is_null())
}

/// The levels of a target as one track, for measuring its speech: a lane's windows (its duck
/// included), a clip's read, a source file, or the mix as exported.
fn speech_levels(s: &PlaybackSession, target: &Target) -> Result<Option<Vec<f32>>, String> {
    let fps = parse::session_fps(s);
    match target {
        Target::Lane(l) => Ok(Some(lane_windows(s, *l)?.0)),
        Target::Clip(l, c) => read(s, *l, c, fps),
        Target::Source(n) => {
            let src = s
                .sources()
                .get(*n)
                .ok_or_else(|| no_source(s, *n))?;
            silence::levels(&src.path, src.audio_stream, (0.0, f64::INFINITY))
                .map_err(|e| format!("reading the sound of src:{n}: {e}"))
        }
        Target::Mix => {
            let wav = render_mix(s)?;
            silence::levels(&wav.0, 0, (0.0, f64::INFINITY))
                .map_err(|e| format!("reading the mix: {e}"))
        }
    }
}

/// The threshold to use when none is given: 30 dB under the median level of the target's speech
/// (its windows above -60 dBFS), kept between -90 and -10 dBFS. Returns it with a note saying how it
/// was measured, so the reply can name the threshold it used. With no speech to measure, the
/// default stands and the note says so.
fn auto_threshold(s: &PlaybackSession, target: &Target) -> Result<(f32, String), String> {
    let default = Settings::default().threshold_db;
    let mut active: Vec<f32> = speech_levels(s, target)?
        .unwrap_or_default()
        .into_iter()
        .filter(|&x| x > AUTO_ACTIVE_DB)
        .collect();
    if active.is_empty() {
        return Ok((
            default,
            format!("no speech above {AUTO_ACTIVE_DB} dBFS to measure, so the default {default} dBFS"),
        ));
    }
    active.sort_by(f32::total_cmp);
    let median = active[active.len() / 2];
    let thr = (median - AUTO_OFFSET_DB).clamp(-90.0, -10.0);
    Ok((
        thr,
        format!(
            "threshold {thr:.1} dBFS, {AUTO_OFFSET_DB} dB under the median speech level {median:.1} dBFS (set threshold_db to override)"
        ),
    ))
}

fn run_silence(srv: &mut Server, args: &Value) -> Reply {
    let cfg = match settings_arg(args) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let s = tri!(srv.session_ref());
    let target = match target_arg(s, args) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let (cfg, thr_note) = if threshold_given(args) {
        (cfg, None)
    } else {
        match auto_threshold(s, &target) {
            Ok((t, note)) => (Settings { threshold_db: t, ..cfg }, Some(note)),
            Err(e) => return fail(e),
        }
    };
    let name = label(s, &target);
    let found = match scan(s, target.clone(), cfg) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    if found.scanned == 0 {
        return fail(format!(
            "nothing to scan on {name}: {}",
            found.no_audio.join("; ")
        ));
    }
    let fps = parse::session_fps(s);
    let total: u32 = found.regions.iter().map(|&(a, b)| b - a).sum();
    let scope = match target {
        Target::Source(_) => "the source file, in source time".to_string(),
        Target::Mix => "the mix as exported, in timeline time".to_string(),
        Target::Lane(_) => format!("{} clip(s) on the lane, judged inside the clips", found.scanned),
        Target::Clip(..) => format!("scanned {} clip(s)", found.scanned),
    };
    let mut text = format!(
        "silence on {name}: {} region(s), {:.3} s ({total}f) at or below {:.1} dBFS for {:.2} s or more · {scope}",
        found.regions.len(),
        parse::secs(total, fps),
        cfg.threshold_db,
        cfg.min_silence,
    );
    if let Some(note) = &thr_note {
        text.push_str(&format!("\nthreshold: {note}"));
    }
    if let Target::Lane(l) = &target {
        text.push_str(&gap_note(s, &[*l], fps));
    }
    for line in region_lines(&found.regions, fps) {
        text.push('\n');
        text.push_str(&line);
    }
    if !found.no_audio.is_empty() {
        text.push_str(&format!(
            "\nskipped, no audio track: {}",
            found.no_audio.join("; ")
        ));
    }
    let structured = json!({
        "regions": region_json(&found.regions),
        "count": found.regions.len(),
        "total_frames": total,
        "scanned": found.scanned,
        "threshold_db": cfg.threshold_db,
        "threshold_auto": thr_note.is_some(),
    });
    Reply::ok(text).with(structured)
}

// ---- cut_silence ----------------------------------------------------------

/// The `lanes` argument: `None` for every lane, or the named ones.
fn lanes_arg(s: &PlaybackSession, args: &Value) -> Result<Option<Vec<Lane>>, String> {
    match args.get("lanes") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .map(|x| {
                let name = x
                    .as_str()
                    .ok_or_else(|| "`lanes` lists lane names such as V1".to_string())?;
                let lane = parse::lane(name)?;
                // A caption lane is a valid scope: captions follow a cut on their own.
                if lane.kind == LaneKind::Subtitle {
                    if !s.lanes().contains(&lane) {
                        return Err(format!("no lane {name} on this timeline"));
                    }
                } else {
                    lane_ok(s, lane, name)?;
                }
                Ok(lane)
            })
            .collect::<Result<Vec<_>, String>>()
            .map(Some),
        Some(_) => {
            Err("`lanes` must be a non-empty list of lane names such as [\"V1\", \"A1\"]".into())
        }
    }
}

/// The lanes of a joint cut: `lanes` naming two or more distinct lanes with no target. None
/// otherwise: with a target, `lanes` is the target's ripple scope (the silence is judged on the
/// target alone), and one lane is the target's own cut. A joint cut has no target, since the
/// silence is judged across the lanes it names.
fn joint_lanes(s: &PlaybackSession, args: &Value) -> Result<Option<Vec<Lane>>, String> {
    let Some(lanes) = lanes_arg(s, args)? else {
        return Ok(None);
    };
    let mut uniq: Vec<Lane> = Vec::new();
    for l in lanes {
        if !uniq.contains(&l) {
            uniq.push(l);
        }
    }
    if uniq.len() < 2 || arg_given(args, "target") {
        return Ok(None);
    }
    Ok(Some(uniq))
}

/// The lane a target sits on, if it is one lane's (a lane or a clip). A mix or a source has none.
fn target_lane(t: &Target) -> Option<Lane> {
    match t {
        Target::Lane(l) | Target::Clip(l, _) => Some(*l),
        Target::Source(_) | Target::Mix => None,
    }
}

/// The note a cut owes when it took the default target (A1, the doc's default) and other audio
/// lanes with clips hold sound it did not scan: they are named, with the target to cut them.
/// Empty when the target was given, for a joint cut, or when no other audio lane has clips.
fn default_target_note(s: &PlaybackSession, args: &Value, target: Option<&Target>) -> String {
    let Some(target) = target else {
        return String::new();
    };
    if arg_given(args, "target") {
        return String::new();
    }
    let scanned = target_lane(target);
    let mut others: Vec<String> = Vec::new();
    for l in s.lanes() {
        if l.kind == LaneKind::Audio && Some(l) != scanned && !s.lane_clips(l).is_empty() {
            others.push(parse::lane_name(l));
        }
    }
    let Some(first) = others.first() else {
        return String::new();
    };
    let scanned_name = label(s, target);
    if others.len() == 1 {
        format!(" · scanned {scanned_name} only, the default target; {first} also holds sound: target {first} to cut its pauses")
    } else {
        format!(
            " · scanned {scanned_name} only, the default target; {} also hold sound: target {first} (or another of them) to cut their pauses",
            others.join(", ")
        )
    }
}

/// The reply of a cut that finds no pause to cut. It names the rule that kept each quiet stretch out
/// and the longest quiet there was, so the setting to lower is the one named. `thr` is the threshold
/// as the reply words it; `longest` is in seconds, unpadded.
fn no_pause_note(name: &str, thr: &str, min_silence: f64, padding: f64, longest: f64) -> String {
    if longest <= 0.0 {
        format!("no quiet under {thr} on {name}; nothing cut")
    } else if longest < min_silence {
        format!(
            "no pause of {min_silence:.2} s or longer (min_silence) under {thr} on {name}; the longest quiet was {longest:.2} s — lower min_silence to cut shorter pauses; nothing cut"
        )
    } else {
        format!(
            "the longest quiet under {thr} on {name} was {longest:.2} s, not longer than the {padding:.2} s padding kept at each end; lower padding to cut it; nothing cut"
        )
    }
}

/// Whether an optional argument other than null was given.
pub(crate) fn arg_given(args: &Value, key: &str) -> bool {
    matches!(args.get(key), Some(v) if !v.is_null())
}

/// The refusal for a cut on a locked lane, with the way out (the same words edit's lock check gives).
pub(crate) fn lock_refusal(l: Lane) -> String {
    let n = parse::lane_name(l);
    format!("lane {n} is locked: unlock it first with lane_flags {{lane:\"{n}\", locked:false}}")
}

/// The name of a joint cut in the reply: its lanes joined, marked as joint.
fn joint_label(lanes: &[Lane]) -> String {
    let names: Vec<String> = lanes.iter().map(|&l| parse::lane_name(l)).collect();
    format!("{} (joint)", names.join("+"))
}

/// The lanes' judged gains and the note, if any, that goes with them.
type Judgement = (Vec<(Lane, f32)>, Option<String>);

/// Each judged lane with the threshold it is judged at, and the note that says how they were
/// set. A threshold given applies to every lane; otherwise each lane measures its own (the same
/// rule as a single lane's). A lane with no clips has no speech to measure, so it takes the
/// default and is silent throughout.
fn joint_thresholds(
    s: &PlaybackSession,
    lanes: &[Lane],
    cfg: Settings,
    given: bool,
) -> Result<Judgement, String> {
    let mut judged = Vec::new();
    let mut notes = Vec::new();
    for &l in lanes {
        if given {
            judged.push((l, cfg.threshold_db));
        } else if s.lane_clips(l).is_empty() {
            judged.push((l, Settings::default().threshold_db));
            notes.push(format!("{} has no clips, so it is quiet throughout", parse::lane_name(l)));
        } else {
            let (thr, note) = auto_threshold(s, &Target::Lane(l))?;
            judged.push((l, thr));
            notes.push(format!("{}: {note}", parse::lane_name(l)));
        }
    }
    let note = (!given).then(|| notes.join("; "));
    Ok((judged, note))
}

/// The pauses a joint cut leaves, measured on the joint silence after the cut, as
/// `measured_cut_pauses` measures a single lane's.
fn measured_joint_pauses(
    s: &PlaybackSession,
    judged: &[(Lane, f32)],
    spans: &[(u32, u32)],
    fps: f64,
) -> Result<Vec<f64>, String> {
    let quiet_cfg = Settings {
        min_silence: 0.0,
        padding: 0.0,
        min_keep: 0.0,
        ..Settings::default()
    };
    let quiet: Vec<(f64, f64)> = find_joint(s, judged, quiet_cfg)?
        .regions
        .iter()
        .map(|&(a, b)| (f64::from(a) / fps, f64::from(b) / fps))
        .collect();
    Ok(measured_pauses(&quiet, spans, fps))
}

/// Snapshots the engine pushed since `start` (a `history_pushes` reading). Exact
/// at the undo cap, where the undo depth stops growing.
fn pushed_since(s: &PlaybackSession, start: u64) -> usize {
    s.history_pushes().saturating_sub(start) as usize
}

/// Takes `n` engine undo steps back. False if the engine would not take one.
fn undo_n(s: &mut PlaybackSession, n: usize) -> bool {
    (0..n).all(|_| s.undo())
}

/// Records the snapshots pushed since `start` as one ledger entry named `label`.
pub(super) fn record_pushes(srv: &mut Server, start: u64, label: String) {
    let Some(s) = srv.session.as_ref() else {
        return;
    };
    let n = pushed_since(s, start);
    if n > 0 {
        srv.ledger.push(Entry::new(n, label));
        srv.history_lost_on_open = false;
        srv.redo_ledger.clear();
        srv.trim_ledger();
    }
}

/// The shortest pause a cut may leave: below it the pause is not a pause a person can hear.
const MIN_KEEP_SECS: f64 = 0.05;

/// `keep` in seconds, 0.05 to 10, or absent.
fn keep_secs(args: &Value) -> Result<Option<f64>, String> {
    match args.get("keep") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && (MIN_KEEP_SECS..=10.0).contains(&x) => Ok(Some(x)),
            Some(x) if x.is_finite() && (0.0..MIN_KEEP_SECS).contains(&x) => Err(format!(
                "`keep` {x} would leave less than {MIN_KEEP_SECS} s of pause; use keep ≥ {MIN_KEEP_SECS}. To delete pauses entirely use cut_regions"
            )),
            _ => Err("`keep` is a time in seconds, 0.05 to 10".into()),
        },
    }
}

/// The settings silence and cut_silence both scan with. They are the same, so a cut is made on
/// exactly the regions silence reports for the same args. min_keep is 0 unless given: a kept
/// sliver of speech joined into two silences would be cut with them.
fn cut_settings(args: &Value) -> Result<Settings, String> {
    settings_arg(args)
}

/// The lanes a cut takes when `lanes` is absent: the target's lane and the lanes of clips linked
/// to it. A mix has no one lane, so it takes every lane.
fn default_cut_scope(s: &PlaybackSession, t: &Target) -> Vec<Lane> {
    match t {
        Target::Lane(l) => super::listen::linked_lanes(s, *l, s.lane_clips(*l)),
        Target::Clip(l, c) => super::listen::linked_lanes(s, *l, std::slice::from_ref(c)),
        Target::Source(_) | Target::Mix => s.lanes(),
    }
}

/// The frames of sound the cut lanes hold: the lengths of their audio clips, gaps and
/// picture-only lanes left out. A scope with no audio lane counts every clip on it.
fn sound_frames(s: &PlaybackSession, scope: &[Lane]) -> u32 {
    let audio: Vec<Lane> = scope.iter().copied().filter(|l| l.kind == LaneKind::Audio).collect();
    let pick = if audio.is_empty() { scope.to_vec() } else { audio };
    let mut total = 0u32;
    for l in pick {
        for c in s.lane_clips(l) {
            total = total.saturating_add(c.end().saturating_sub(c.start));
        }
    }
    total
}

/// The spans a cut takes out of each silence region, so the pause left in it is `keep` seconds
/// in all, padding included. `regions` are the silence as `find` reports it: the padding at each
/// end is already outside them, and it is what stays there, so the frames of quiet left are the
/// two paddings plus the part of the region kept, which is keep less the two paddings. The
/// padding is rounded up to whole frames as `find` rounds it, and keep to the nearest frame. A
/// silence whose quiet is at or under keep is left alone; a keep under twice the padding leaves
/// 2 × padding (the region is cut whole). A silence that runs to its clip's end keeps keep after
/// the speech before it, its end padding included, so the tail is never shorter than keep.
/// Returns `(start, len)` timeline frames.
fn cut_spans_for(
    regions: &[(u32, u32)],
    keep: f64,
    padding: f64,
    fps: f64,
    clip_ends: &[u32],
) -> Vec<(u32, u32)> {
    let pad = (padding * fps).ceil() as u32;
    let total = (keep * fps).round() as u32;
    // The frames of the region left in place.
    let inner = total.saturating_sub(2 * pad);
    regions
        .iter()
        .filter_map(|&(a, b)| {
            let f = b.saturating_sub(a);
            if f == 0 {
                return None;
            }
            // The region ends `pad` before the clip's end (give or take the frame rounding) when the
            // silence runs to it: the padding at the clip's end stays, and the kept part sits after
            // the speech before it.
            let at_clip_end = clip_ends.iter().any(|&e| e >= b && e - b <= pad + 3);
            if at_clip_end {
                let start = a.saturating_add(inner);
                return (b > start).then_some((start, b - start));
            }
            // The kept part is split half at each end; the cut takes the rest of the region.
            let len = f.checked_sub(inner).filter(|&n| n > 0)?;
            Some((a + inner / 2, len))
        })
        .collect()
}

/// The audio clips on lanes a cut does not name that the cut runs through. They keep their place
/// while the cut lanes shorten, so they fall out of step with them (a music bed under two speakers,
/// say). Their addresses, so the reply can name them.
fn unlisted_spanning(s: &PlaybackSession, scope: &[Lane], spans: &[(u32, u32)]) -> Vec<String> {
    let mut out = Vec::new();
    for l in s.lanes() {
        if scope.contains(&l) || l.kind != LaneKind::Audio {
            continue;
        }
        for c in s.lane_clips(l) {
            if spans.iter().any(|&(a, len)| c.start < a + len && c.end() > a) {
                out.push(parse::addr(l, c.start));
            }
        }
    }
    out
}

/// The note a cut owes when a lane it did not cut now runs past the end of the cut lanes: a music bed
/// under two speakers that the cut shortened, say. Each such lane is named, with how far past it runs
/// and the trim that brings it back to the new end. Empty when no lane outside the cut runs past.
fn overrun_note(s: &PlaybackSession, scope: &[Lane], fps: f64) -> String {
    let cut_end = super::listen::lanes_end(s, scope);
    if cut_end == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    for l in s.lanes() {
        let end = super::listen::lanes_end(s, &[l]);
        if scope.contains(&l) || end <= cut_end {
            continue;
        }
        let name = parse::lane_name(l);
        // The clip that crosses the new end is trimmed; when the lane has none there, the clips past it go.
        let fix = match s.lane_clips(l).iter().find(|c| c.start < cut_end && c.end() > cut_end) {
            Some(c) => format!(
                "trim {{clip:\"{}\", edge:\"out\", to:\"{:.3}s\"}}",
                parse::addr(l, c.start),
                parse::secs(cut_end, fps)
            ),
            None => "delete the clips past the cut lanes' end".to_string(),
        };
        parts.push(format!(
            "{name} runs {:.3} s past the cut lanes (they end at {:.3} s, {name} at {:.3} s): trim it with the edit op {fix}",
            parse::secs(end - cut_end, fps),
            parse::secs(cut_end, fps),
            parse::secs(end, fps),
        ));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" · {}", parts.join("; "))
}

/// A cut rolled back from a clean project leaves it clean: its save marks move past the rolled-back
/// pushes, as `edit`'s dry run and batch rollback do. A project that was dirty keeps its marks.
fn keep_clean_marks(srv: &mut Server, was_clean: bool) {
    if !was_clean {
        return;
    }
    if let Some(s) = srv.session.as_ref() {
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
    }
}

fn run_cut_silence(srv: &mut Server, args: &Value) -> Reply {
    let cfg = match cut_settings(args) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let keep = match keep_secs(args) {
        Ok(k) => k.unwrap_or(DEFAULT_KEEP),
        Err(e) => return fail(e),
    };
    let dry = opt_bool(args, "dry_run").unwrap_or(false);
    // A cut that is rolled back (a dry run, or a refused cut) leaves the project as it was.
    let was_clean = !super::project::dirty(srv);
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    // A joint cut names two or more lanes and has no target; any other cut has one.
    let joint = match joint_lanes(s, args) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    let target = match &joint {
        Some(_) => None,
        None => match target_arg(s, args) {
            Ok(t) => Some(t),
            Err(e) => return fail(e),
        },
    };
    if let Some(Target::Source(n)) = target {
        return fail(source_cut_refusal(s, n));
    }
    // The name, the scope a cut takes by default, the thresholds used and the regions found. A
    // joint cut judges each listed lane at its own threshold; a single cut as it always has.
    let (name, linked, judged, thr_note, found, cfg) = match (&joint, &target) {
        (Some(lanes), _) => {
            // Only audio lanes are judged; a video lane in the list is cut with the spans, not judged.
            let judged_lanes: Vec<Lane> = lanes.iter().copied().filter(|l| l.kind == LaneKind::Audio).collect();
            if judged_lanes.len() < 2 {
                return fail(format!(
                    "a joint cut judges two or more audio lanes, and lanes has {}: give target with the one audio lane, and lanes for the lanes to cut with it",
                    judged_lanes.len()
                ));
            }
            let (judged, thr_note) = match joint_thresholds(s, &judged_lanes, cfg, threshold_given(args)) {
                Ok(j) => j,
                Err(e) => return fail(e),
            };
            // The same regions the joint silence reports for these args: the cut is made on those.
            let found = match find_joint(s, &judged, cfg) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            (joint_label(lanes), Vec::new(), judged, thr_note, found, cfg)
        }
        (None, Some(target)) => {
            let name = label(s, target);
            let linked = default_cut_scope(s, target);
            let (cfg, thr_note) = if threshold_given(args) {
                (cfg, None)
            } else {
                match auto_threshold(s, target) {
                    Ok((t, note)) => (Settings { threshold_db: t, ..cfg }, Some(note)),
                    Err(e) => return fail(e),
                }
            };
            // The same regions `silence` reports for these args: the cut is made on exactly those.
            let found = match scan(s, target.clone(), cfg) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            (name, linked, Vec::new(), thr_note, found, cfg)
        }
        _ => return fail("cut_silence needs a target, or two or more lanes to cut together".into()),
    };
    if found.scanned == 0 {
        return fail(format!(
            "nothing to scan on {name}: {}",
            found.no_audio.join("; ")
        ));
    }
    // With a target, `lanes` is the ripple scope: the same spans are cut on each. The target's own
    // lane joins them when it is not listed, so the silence is always cut on the lane it was found on.
    let mut added_target = None;
    let mut scope = match (&joint, lanes_arg(s, args)) {
        (Some(lanes), _) => lanes.clone(),
        (None, Ok(Some(mut v))) => {
            if let Some(l) = target.as_ref().and_then(target_lane).filter(|l| !v.contains(l)) {
                v.insert(0, l);
                added_target = Some(l);
            }
            v
        }
        (None, Ok(None)) => linked,
        (None, Err(e)) => return fail(e),
    };
    // Locks, as the ripple rule has them. A locked lane the cut would move refuses, with the way
    // out: the target's own lane, a lane named in `lanes`, a lane a take reaches through its link.
    // A mix's default scope (every lane) refuses too, naming each locked lane, and changes nothing.
    // Captions ride the cut: every caption lane the scope does not name joins it, and a locked one stays put.
    let locked_captions = super::listen::ride_captions(s, &mut scope);
    let locked: Vec<Lane> = scope.iter().copied().filter(|&l| s.lane_locked(l)).collect();
    if let Some((&first, rest)) = locked.split_first() {
        let mut why = lock_refusal(first);
        if !rest.is_empty() {
            let names: Vec<String> = rest.iter().map(|&l| parse::lane_name(l)).collect();
            why.push_str(&format!("; lanes {} are locked too", names.join(", ")));
        }
        return fail(format!("cut_silence refused: {why}; nothing changed"));
    }
    if scope.is_empty() {
        return fail(format!("cut_silence refused: every lane on {name} is locked; unlock one with lane_flags"));
    }
    let scope_names: Vec<String> = scope.iter().map(|&l| parse::lane_name(l)).collect();
    // Both the dry run and the real run name the lanes the cut takes, the same way.
    let lanes_named = format!("{} lane(s) ({})", scope_names.len(), scope_names.join("+"));
    let fps = parse::session_fps(s);
    let other_lanes = default_target_note(s, args, target.as_ref());
    if found.regions.is_empty() {
        // Judged without the min_silence and padding, so the reply can name the longest quiet.
        let quiet_cfg = Settings { min_silence: 0.0, padding: 0.0, min_keep: 0.0, ..cfg };
        let quiet = match (&target, &joint) {
            (Some(t), _) => scan(s, t.clone(), quiet_cfg).map(|f| f.regions),
            (None, Some(_)) => find_joint(s, &judged, quiet_cfg).map(|f| f.regions),
            (None, None) => Ok(Vec::new()),
        };
        let quiet = match quiet {
            Ok(q) => q,
            Err(e) => return fail(e),
        };
        let longest = quiet.iter().map(|&(a, b)| f64::from(b - a)).fold(0.0, f64::max) / fps;
        let thr = if joint.is_some() {
            "each lane's threshold".to_string()
        } else {
            format!("{:.1} dBFS", cfg.threshold_db)
        };
        let text = no_pause_note(&name, &thr, cfg.min_silence, cfg.padding, longest) + &other_lanes;
        return Reply::ok(text).with(json!({
            "regions": [], "count": 0, "removed_frames": 0, "dry_run": dry,
            "longest_quiet_s": (longest * 1000.0).round() / 1000.0,
        }));
    }
    // Where the cut lanes' clips end: a silence running to one of those ends is the tail.
    let clip_ends: Vec<u32> = scope
        .iter()
        .flat_map(|&l| s.lane_clips(l).iter().map(|c| c.end()))
        .collect();
    let spans = cut_spans_for(&found.regions, keep, cfg.padding, fps, &clip_ends);
    let gaps = match (&joint, &target) {
        (Some(lanes), _) => gap_note(s, lanes, fps),
        (None, Some(Target::Lane(l))) => gap_note(s, &[*l], fps),
        _ => String::new(),
    };
    if spans.is_empty() {
        return Reply::ok(format!(
            "{} silence(s) found on {name}, all ≤ keep ({keep} s); nothing cut{other_lanes}",
            found.regions.len()
        ))
        .with(json!({
            "regions": region_json(&found.regions),
            "count": found.regions.len(),
            "removed_frames": 0,
            "dry_run": dry,
        }));
    }

    // Read before the cut: the audio on unlisted lanes that the cut runs through.
    let spanning = unlisted_spanning(s, &scope, &spans);
    let start = s.history_pushes();
    // A dry run is a trial: its redo history and redo ledger are put back once it is undone.
    let trial = start_trial(s, dry).map(|redo| Trial { redo, ledger: std::mem::take(&mut srv.redo_ledger) });
    let end_before = parse::session_end(s);
    let lanes_before = super::listen::media_end(s, &scope);
    let sound_before = sound_frames(s, &scope);
    let cues_before = super::listen::scope_cues(s, &scope);
    // Each span is cut as found: its edges are inside the silence, so no kept sliver of speech
    // lies between two spans for the edit's sliver join to delete.
    let shape0 = super::edit::shape(s);
    if let Err(e) = s.cut_regions(&spans, &scope) {
        let rolled = roll_back_cut(srv, start, trial);
        srv.trim_ledger();
        keep_clean_marks(srv, was_clean);
        let note = if rolled {
            ""
        } else {
            " (could not roll every step back; check the project)"
        };
        return fail(format!("cut refused: {e}; nothing changed{note}"));
    }
    // Titles move with the cut as they do for cut_regions, and the reply names what the cut left behind.
    let moved = if super::edit::has_picture(&scope) {
        match super::edit::move_titles_for_cuts(s, &spans, fps) {
            Ok(m) => m,
            Err(e) => {
                roll_back_cut(srv, start, trial);
                keep_clean_marks(srv, was_clean);
                return fail(format!("cut refused: {e}; nothing changed"));
            }
        }
    } else {
        Vec::new()
    };
    let from = spans.first().map_or(0, |sp| sp.0);
    let effects = super::edit::side_effects(&shape0, &super::edit::shape(s), "cut_regions");
    let ripple = format!(
        "{}{}{}{}{}",
        super::edit::titles_note(&moved),
        super::edit::left_behind(s, &scope, from),
        super::edit::caption_drift(s, args, &scope, from),
        super::listen::caption_changes(&cues_before, &super::listen::scope_cues(s, &scope)),
        super::listen::caption_tail_note(s, fps)
    );
    let end_after = parse::session_end(s);
    // Read now, while the cut is in place: a dry run rolls it back below.
    let overrun = overrun_note(s, &scope, fps);
    let lanes_after = super::listen::media_end(s, &scope);
    let sound_after = sound_frames(s, &scope);
    // The pause actually left, read back from the sound after the cut and before any undo below.
    let pauses = match &target {
        Some(t) => measured_cut_pauses(s, t, &spans, cfg, fps),
        None => measured_joint_pauses(s, &judged, &spans, fps),
    };
    // Measured on the cut lanes: a cut on A1 alone shortens A1 even when a longer lane sets the timeline end.
    let removed = lanes_before.saturating_sub(lanes_after);
    let removed_secs = parse::secs(removed, fps);
    // The share is of the sound: the clips' own frames, so a lead-in gap is not counted as sound.
    let removed_sound = sound_before.saturating_sub(sound_after);

    // The same decision for a dry run and a real cut: a cut over the share limit is undone here.
    let force = opt_bool(args, "force").unwrap_or(false);
    let share = if sound_before > 0 {
        f64::from(removed_sound) / f64::from(sound_before)
    } else {
        0.0
    };
    if !force && share > CUT_SHARE_LIMIT {
        let rolled = roll_back_cut(srv, start, trial);
        srv.trim_ledger();
        keep_clean_marks(srv, was_clean);
        let note = if rolled {
            ""
        } else {
            " (could not roll every step back; check the project)"
        };
        return fail(format!(
            "cut refused: it would remove {:.3} s of {:.3} s of sound on {name} ({:.0}%), more than half. Cut spans: {}. Nothing changed{note}. Raise keep or lower threshold_db, or pass force:true to cut anyway.",
            parse::secs(removed_sound, fps),
            parse::secs(sound_before, fps),
            share * 100.0,
            span_list(&spans, fps),
        ));
    }

    let mut note = String::new();
    if dry {
        if !roll_back_cut(srv, start, trial) {
            note = " (could not roll every step back; check the project)".into();
        }
        keep_clean_marks(srv, was_clean);
    }
    // `s` is not used past this point, so the ledger can be written.
    let head = if dry {
        format!(
            "dry run — would cut {} of {} silence(s) from {lanes_named}, and would remove {removed_secs:.3} s (the timeline's loss, not the silence found); timeline {:.3} s → {:.3} s; nothing changed{note}",
            spans.len(),
            found.regions.len(),
            parse::secs(end_before, fps),
            parse::secs(end_after, fps),
        )
    } else {
        record_pushes(
            srv,
            start,
            format!("cut_silence {name} ({} regions)", spans.len()),
        );
        format!(
            "cut {} of {} silence(s) from {lanes_named}, {removed_secs:.3} s removed; timeline {:.3} s → {:.3} s ({end_after}f) · undo step {}",
            spans.len(),
            found.regions.len(),
            parse::secs(end_before, fps),
            parse::secs(end_after, fps),
            srv.ledger.len(),
        )
    };
    let mut text = head;
    text.push_str(&overrun);
    text.push_str(&ripple);
    if let Some(l) = added_target {
        text.push_str(&format!(" · {} is the target, so it is cut with the listed lanes", parse::lane_name(l)));
    }
    if !locked_captions.is_empty() {
        let names: Vec<String> = locked_captions.iter().map(|&l| parse::lane_name(l)).collect();
        text.push_str(&format!("; locked captions {} stay put, so they are not cut (unlock them with lane_flags to ride the cut)", names.join(", ")));
    }
    if !spanning.is_empty() {
        text.push_str(&format!(
            "; out of step, not cut: {} (audio left in place across the cut; pass its lane in lanes to cut it too)",
            spanning.join(", ")
        ));
    }
    let pause_note = match &pauses {
        Ok(p) if !p.is_empty() => {
            let lo = p.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = p.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if hi - lo < 0.005 {
                format!("{lo:.2} s")
            } else {
                format!("{lo:.2}–{hi:.2} s")
            }
        }
        Ok(_) => "none (no cut joints)".to_string(),
        Err(e) => format!("not measured ({e})"),
    };
    text.push_str(&format!(" · pauses now {pause_note} (measured after the cut on {name})"));
    if let Some(note) = &thr_note {
        text.push_str(&format!("\nthreshold: {note}"));
    }
    text.push_str(&gaps);
    if !effects.is_empty() {
        text.push_str(&format!("\neffects: {}", effects.join("; ")));
    }
    // The lines are the spans cut, as the timeline loses them: the silence's frames less what is kept.
    let cut_regions: Vec<(u32, u32)> = spans.iter().map(|&(a, len)| (a, a + len)).collect();
    for line in region_lines(&cut_regions, fps) {
        text.push('\n');
        text.push_str(&line);
    }
    if !found.no_audio.is_empty() {
        text.push_str(&format!(
            "\nskipped, no audio track: {}",
            found.no_audio.join("; ")
        ));
    }
    let structured = json!({
        "regions": region_json(&cut_regions),
        "count": found.regions.len(),
        "cut_count": spans.len(),
        "lanes": scope_names,
        "joint": joint.is_some(),
        "out_of_step": spanning,
        "removed_frames": removed,
        "removed_sound_frames": removed_sound,
        "timeline_frames_before": end_before,
        "timeline_frames": if dry { end_before } else { end_after },
        "pauses_measured_s": pauses.as_ref().ok().map(|p| p.iter().map(|x| (x * 1000.0).round() / 1000.0).collect::<Vec<f64>>()),
        "dry_run": dry,
        "undo_steps": srv.ledger.len(),
    });
    Reply::ok(text).with(structured)
}

// ---- levels ---------------------------------------------------------------

/// `[from, to)` from `range`, clamped to `[lo, hi)`. Frames for a timeline span,
/// source frames for a source. A time past the end of the span is refused by name,
/// in seconds, rather than clamped into an empty range.
fn range_arg(args: &Value, fps: f64, lo: u32, hi: u32, name: &str) -> Result<(u32, u32), String> {
    let Some(r) = args.get("range").filter(|v| !v.is_null()) else {
        return Ok((lo, hi));
    };
    let obj = r
        .as_object()
        .ok_or("`range` must be {from, to}, each a time such as 450f or 15.0s")?;
    let pick = |k: &str, dv: u32| -> Result<u32, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(dv),
            Some(v) => parse::frame(v, fps, hi),
        }
    };
    let (raw_from, raw_to) = (pick("from", lo)?, pick("to", hi)?);
    // The timeline shows times to the millisecond, and a `to` one frame past the end is the end.
    let secs = |f: u32| format!("{:.3}s", parse::secs(f, fps));
    let raw_to = if hi < u32::MAX && raw_to > hi && raw_to - hi <= 1 { hi } else { raw_to };
    if hi < u32::MAX && (raw_from > hi || raw_to > hi) {
        return Err(format!(
            "range {}–{} is past the end of {name} ({})",
            secs(raw_from),
            secs(raw_to),
            secs(hi)
        ));
    }
    let (from, to) = (raw_from.clamp(lo, hi), raw_to.clamp(lo, hi));
    if to <= from {
        return Err(format!(
            "range is empty: it runs from {} to {} in {name}",
            secs(from),
            secs(to)
        ));
    }
    Ok((from, to))
}

/// One value per window across a timeline span, with gaps and silent files read as
/// `SILENT_DB`. Returns the time of the first window in seconds and the values.
fn span_windows(reads: &[(Clip, Option<Vec<f32>>)], fps: f64, span: (u32, u32)) -> (f64, Vec<f32>) {
    let w = f64::from(WINDOWS_PER_SEC);
    let first = (f64::from(span.0) / fps * w).floor() as usize;
    let last = (f64::from(span.1) / fps * w).ceil() as usize;
    let values = (first..last)
        .map(|i| {
            let t = i as f64 / w;
            reads
                .iter()
                .find(|(c, _)| t >= f64::from(c.start) / fps && t < f64::from(c.end()) / fps)
                .and_then(|(c, levels)| {
                    let levels = levels.as_ref()?;
                    let src_secs = c.source_offset_f((t - f64::from(c.start) / fps) * fps) / fps;
                    let j = (src_secs * w).floor() as usize;
                    levels.get(j).copied()
                })
                .unwrap_or(SILENT_DB)
        })
        .collect();
    (first as f64 / w, values)
}

/// Groups windows into `per_sec` values each: the power mean, in dB, floored at
/// `SILENT_DB`, so a loud window and a quiet one average as sound does.
fn bucket(windows: &[f32], per_sec: f64) -> Vec<f32> {
    if windows.is_empty() {
        return Vec::new();
    }
    let w = f64::from(WINDOWS_PER_SEC);
    let n = ((windows.len() as f64) * per_sec / w).ceil().max(1.0) as usize;
    let mut sum = vec![0f64; n];
    let mut count = vec![0u32; n];
    for (k, &v) in windows.iter().enumerate() {
        let b = (((k as f64) * per_sec / w).floor() as usize).min(n - 1);
        sum[b] += 10f64.powf(f64::from(v) / 10.0);
        count[b] += 1;
    }
    (0..n)
        .map(|b| {
            if count[b] == 0 {
                SILENT_DB
            } else {
                let db = 10.0 * (sum[b] / f64::from(count[b])).log10();
                db.max(f64::from(SILENT_DB)) as f32
            }
        })
        .collect()
}

fn lane_end(s: &PlaybackSession, l: Lane) -> u32 {
    s.lane_clips(l).last().map_or(0, |c| c.end())
}

fn run_levels(srv: &mut Server, args: &Value) -> Reply {
    let s = tri!(srv.session_ref());
    let Some(raw) = opt_str(args, "target") else {
        return fail(
            "missing `target`: give a clip address such as A1@0, a lane such as A1, src:N, or mix"
                .into(),
        );
    };
    let per_sec = match opt_f64(args, "per_sec") {
        None => 2.0,
        Some(p) if p.is_finite() && p > 0.0 && p <= 50.0 => p,
        Some(p) => {
            return fail(format!(
                "per_sec {p} is out of range: per_sec is buckets per second, a number above 0 up to 50, e.g. 10"
            ));
        }
    };
    let target = match resolve(s, raw) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let fps = parse::session_fps(s);
    let (lo, hi) = match &target {
        Target::Source(_) => (0, u32::MAX),
        Target::Clip(_, c) => (c.start, c.end()),
        Target::Lane(l) => (0, lane_end(s, *l)),
        Target::Mix => (0, parse::session_end(s)),
    };
    if hi == 0 {
        return fail(format!("{} has no clips to measure", label(s, &target)));
    }
    let (from, to) = match range_arg(args, fps, lo, hi, &label(s, &target)) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let name = label(s, &target);

    let mut notes: Vec<String> = Vec::new();
    let lane_of_target = audio_lane_of(&target);
    let (base, mut windows) = match target {
        Target::Source(n) => {
            if let Some(why) = offline_refusal_for(s, &name, &[n]) {
                return fail(why);
            }
            let src = &s.sources()[n];
            let from_s = f64::from(from) / fps;
            let to_s = if to == u32::MAX {
                f64::INFINITY
            } else {
                f64::from(to) / fps
            };
            match silence::levels(&src.path, src.audio_stream, (from_s, to_s)) {
                Ok(Some(v)) => (from_s, v),
                Ok(None) => {
                    return fail(format!("{name} has no audio track: nothing to measure"));
                }
                Err(e) => return fail(format!("reading the sound of {name}: {e}")),
            }
        }
        Target::Mix => {
            let wav = match render_mix(s) {
                Ok(w) => w,
                Err(e) => return fail(e),
            };
            let from_s = f64::from(from) / fps;
            let to_s = if to >= hi {
                f64::INFINITY
            } else {
                f64::from(to) / fps
            };
            match silence::levels(&wav.0, 0, (from_s, to_s)) {
                Ok(Some(v)) => (from_s, v),
                Ok(None) => {
                    return fail(format!("{name} has no sound track: nothing to measure"));
                }
                Err(e) => return fail(format!("reading the mix: {e}")),
            }
        }
        _ => {
            let mut reads = Vec::new();
            let mut silent = Vec::new();
            for (lane, clip) in clips_of(s, target) {
                match read(s, lane, &clip, fps) {
                    Ok(levels) => {
                        if levels.is_none() {
                            silent.push(format!(
                                "src:{} {}",
                                clip.source,
                                source_name(s, clip.source)
                            ));
                        }
                        reads.push((clip, levels));
                    }
                    Err(e) => return fail(e),
                }
            }
            if !silent.is_empty() {
                notes.push(format!(
                    "no audio track, read as silence: {}",
                    silent.join(", ")
                ));
            }
            span_windows(&reads, fps, (from, to))
        }
    };
    if windows.is_empty() {
        return fail(format!("{name}: the span is empty"));
    }
    // A lane under a duck reads as it plays: the duck comes off its windows too.
    if let Some(lane) = lane_of_target {
        let offsets = match duck_offsets(s, lane, to, fps) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        if let Some(note) = duck_note(s, lane) {
            let first = (f64::from(from) / fps * f64::from(WINDOWS_PER_SEC)).floor() as usize;
            apply_offsets_to_windows(&mut windows, first, &offsets);
            notes.push(note);
        }
        // A muted lane is left out of the mix and export, so its own sound is all this reads.
        if s.lane_muted(lane) {
            notes.push(muted_note(lane));
        }
    }

    // Summary over the 20 ms windows themselves, not over the grouped values.
    let w = f64::from(WINDOWS_PER_SEC);
    let (mut loud, mut loud_at, mut quiet, mut quiet_at) = (f32::MIN, 0usize, f32::MAX, 0usize);
    let (mut power, mut below) = (0f64, 0usize);
    for (i, &v) in windows.iter().enumerate() {
        if v > loud {
            loud = v;
            loud_at = i;
        }
        if v < quiet {
            quiet = v;
            quiet_at = i;
        }
        // The mean is taken over power, not over the dB values, as bucket does.
        power += 10f64.powf(f64::from(v) / 10.0);
        if v <= -40.0 {
            below += 1;
        }
    }
    let n = windows.len() as f64;
    let mean = (10.0 * (power / n).log10()).max(f64::from(SILENT_DB));
    let at = |i: usize| base + i as f64 / w;
    // Every figure says the window it measures: the extremes are single 20 ms windows, the
    // rows below are per-second groups of them.
    let summary = format!(
        "loudest 20 ms window {:.0} dBFS at {:.2}s · quietest 20 ms window {:.0} dBFS at {:.2}s · mean {mean:.1} dBFS (power mean of the 20 ms windows) · {:.0}% of windows at or below -40 dBFS",
        loud,
        at(loud_at),
        quiet,
        at(quiet_at),
        below as f64 / n * 100.0,
    );

    let values = bucket(&windows, per_sec);
    let header = format!(
        "{name} levels dBFS, {per_sec}/s (power mean of each group of 20 ms windows), {:.1}–{:.1}s",
        base,
        base + n / w
    );
    let rows: Vec<String> = values
        .chunks(10)
        .enumerate()
        .map(|(r, chunk)| {
            let t = base + (r * 10) as f64 / per_sec;
            let mut line = format!("{t:>6.1}s ");
            for v in chunk {
                // Five wide: "-120" is four characters, and a column must keep a gap after it.
                line.push_str(&format!("{:>5}", v.round() as i32));
            }
            line
        })
        .collect();

    // Fixed parts first, then as many rows as the cap leaves room for.
    let fixed = header.len() + summary.len() + notes.iter().map(|x| x.len() + 1).sum::<usize>();
    let budget = CAP.saturating_sub(fixed + 120);
    let mut used = 0usize;
    let mut shown = 0usize;
    for row in &rows {
        if used + row.len() + 1 > budget {
            break;
        }
        used += row.len() + 1;
        shown += 1;
    }
    let mut text = String::new();
    text.push_str(&header);
    text.push('\n');
    text.push_str(&summary);
    for note in &notes {
        text.push('\n');
        text.push_str(note);
    }
    for row in rows.iter().take(shown) {
        text.push('\n');
        text.push_str(row);
    }
    if shown < rows.len() {
        let suggest = ((per_sec * shown as f64 / rows.len() as f64) * 10.0).floor() / 10.0;
        text.push_str(&format!(
            "\n… {} more row(s) cut to stay under 4 KB; per_sec {:.1} or a narrower range shows the rest",
            rows.len() - shown,
            suggest.max(0.1),
        ));
    }

    let kept: Vec<f64> = values
        .iter()
        .take(MAX_VALUES)
        .map(|&v| (f64::from(v) * 10.0).round() / 10.0)
        .collect();
    // Levels are RMS; the peak that clips is the true peak, so a target whose true peak is above
    // the -1 dBTP ceiling is warned here too, as loudness warns.
    if let Ok(r) = measure_target(s, args, raw)
        && let Some(w) = peak_warning(r.loud.true_peak_dbtp, r.loud.integrated_lufs, &r.span, r.name == "mix")
    {
        text.push('\n');
        text.push_str(&w);
    }
    let structured = json!({
        "per_sec": per_sec,
        "start_secs": base,
        "values": kept,
        "truncated": values.len() > MAX_VALUES,
    });
    Reply::ok(text).with(structured)
}

/// The true-peak ceiling the loudness tools hold the mix to, in dBTP.
const PEAK_CEILING_DBTP: f64 = -1.0;

/// The warning for a true peak above the -1 dBTP ceiling, judged on the value as it is shown (one
/// decimal): a peak that reads -1.0 is within spec, so -0.96 does not warn. Headroom gone, so a
/// gain up is held or clips.
fn peak_warning(true_peak_dbtp: f64, integrated_lufs: f64, span: &str, mix: bool) -> Option<String> {
    if !true_peak_dbtp.is_finite() || shown_peak(true_peak_dbtp) <= PEAK_CEILING_DBTP {
        return None;
    }
    let base = format!(
        "WARNING: true peak {} dBTP over {span} is above the -1 dBTP ceiling (clips on some players)",
        signed_figure(true_peak_dbtp)
    );
    if !integrated_lufs.is_finite() {
        return Some(base);
    }
    // The headroom only matters when the target needs a gain up: a louder mix comes down under a
    // normalize, and its peaks come down with it.
    let to_normalize = NORMALIZE_TARGET_LUFS - integrated_lufs;
    // The cut the peak needs to come down to the ceiling. A normalize makes the larger of the two
    // cuts, so when the peak needs more, the level it lands on is the one the peak allows.
    let peak_cut = (true_peak_dbtp - PEAK_CEILING_DBTP).max(0.0);
    Some(if to_normalize > 0.05 {
        format!("{base}; no headroom, so a normalize to {NORMALIZE_TARGET_LUFS:.0} or a gain up is held by the ceiling or clips")
    } else if to_normalize < -0.05 && peak_cut > -to_normalize + 0.05 {
        let held = integrated_lufs - peak_cut;
        let way = if mix {
            "allow_limiter:true".to_string()
        } else {
            "a normalize of the mix with allow_limiter:true".to_string()
        };
        format!(
            "{base}; a normalize to {NORMALIZE_TARGET_LUFS:.0} would turn it down {:.1} dB, but the {PEAK_CEILING_DBTP:.0} dBTP ceiling holds it at {} LUFS unless {way}",
            -to_normalize,
            figure(held),
        )
    } else if to_normalize < -0.05 {
        format!(
            "{base}; a normalize to {NORMALIZE_TARGET_LUFS:.0} would turn it down {:.1} dB",
            -to_normalize
        )
    } else {
        base
    })
}

/// The integrated target `normalize` aims for by default: YouTube and Spotify.
const NORMALIZE_TARGET_LUFS: f64 = -14.0;

// ---- loudness -------------------------------------------------------------

/// Whether `target` names the whole mix rather than an address.
fn is_mix(raw: &str) -> bool {
    raw.trim().eq_ignore_ascii_case("mix")
}

/// The lane an audio target plays on; `None` for a source or a video lane.
fn audio_lane_of(t: &Target) -> Option<Lane> {
    match t {
        Target::Lane(l) | Target::Clip(l, _) if l.kind == LaneKind::Audio => Some(*l),
        _ => None,
    }
}

/// Takes the duck's dB off the windows from timeline window `first` on. Silent
/// windows stay silent.
fn apply_offsets_to_windows(windows: &mut [f32], first: usize, offsets: &[f32]) {
    if offsets.is_empty() {
        return;
    }
    for (i, v) in windows.iter_mut().enumerate() {
        if *v > SILENT_DB {
            let off = offsets.get(first + i).copied().unwrap_or(0.0);
            *v = (*v + off).max(SILENT_DB);
        }
    }
}

/// Takes the duck's dB off one clip's windows. Window j of a clip is at timeline
/// time `start + j / (speed * WINDOWS_PER_SEC)`, which is where the duck's windows
/// are read.
fn apply_duck(levels: &mut [f32], clip: &Clip, fps: f64, offsets: &[f32]) {
    if offsets.is_empty() {
        return;
    }
    let w = f64::from(WINDOWS_PER_SEC);
    for (j, v) in levels.iter_mut().enumerate() {
        if *v > SILENT_DB {
            let t = f64::from(clip.start) / fps + clip.timeline_offset_f(j as f64 / w * fps) / fps;
            let off = offsets
                .get((t * w).floor() as usize)
                .copied()
                .unwrap_or(0.0);
            *v = (*v + off).max(SILENT_DB);
        }
    }
}

/// Measures a source file's own sound, `range` in seconds.
fn source_loudness(
    s: &PlaybackSession,
    n: usize,
    range: Option<(f64, f64)>,
) -> Result<Loudness, String> {
    let src = &s.sources()[n];
    loudness::measure_file(&src.path, src.audio_stream, range)
        .map_err(|e| format!("reading the sound of src:{n} {}: {e}", source_name(s, n)))?
        .ok_or_else(|| format!("src:{n} {} has no audio track", source_name(s, n)))
}


/// One measurement, as the engine plays it.
struct Read {
    name: String,
    /// The span measured, in seconds and the target's name: "0.000–11.633s of A2".
    span: String,
    /// The lane gain (dB) the figures include, for an audio lane or clip.
    lane_gain: Option<f64>,
    /// "includes the duck under A3 (...)" (or "the ducks under A2 and A3") when the measured lane is under a duck.
    duck: Option<String>,
    loud: Loudness,
    seconds: f64,
}

/// The note that a lane's figures include its ducks, the same words `levels` uses.
fn duck_note(s: &PlaybackSession, lane: Lane) -> Option<String> {
    let ducks = s.lane_duck_list(lane);
    if ducks.is_empty() {
        return None;
    }
    let each: Vec<String> = ducks
        .iter()
        .map(|d| {
            format!(
                "{} ({:+.1} dB at most)",
                parse::lane_name(Lane::new(LaneKind::Audio, d.under)),
                f64::from(d.depth_db)
            )
        })
        .collect();
    let word = if each.len() == 1 { "duck" } else { "ducks" };
    Some(format!("includes the {word} under {}", join_and(&each)))
}

/// The note on a muted lane's levels: the figures are its own sound, which the mix does not play.
fn muted_note(lane: Lane) -> String {
    format!(
        "{} is muted: it is silent in the mix and export; the level shown is the lane's own sound",
        parse::lane_name(lane)
    )
}

/// `a`, `a and b`, `a, b and c`.
fn join_and(items: &[String]) -> String {
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// The span a measurement covers, named in seconds with the target.
fn span_label(from: u32, to: u32, fps: f64, name: &str) -> String {
    if to == u32::MAX {
        format!("{:.3}s to the end of {name}", parse::secs(from, fps))
    } else {
        format!("{:.3}–{:.3}s of {name}", parse::secs(from, fps), parse::secs(to, fps))
    }
}

fn measure_target(s: &PlaybackSession, args: &Value, raw: &str) -> Result<Read, String> {
    let started = Instant::now();
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let target = resolve(s, raw)?;
    let name = label(s, &target);
    // Each arm names the span it measured.
    let span: String;
    let (loud, lane_gain, duck) = match target {
        Target::Mix => {
            let (from, to) = range_arg(args, fps, 0, end, &name)?;
            span = span_label(from, to, fps, &name);
            (measure_mix(s, from, to, fps, end)?, None, None)
        }
        Target::Lane(lane) => {
            if lane.kind != LaneKind::Audio {
                return Err(format!(
                    "{name} is a video lane with no sound of its own; measure an audio lane (A1) or mix"
                ));
            }
            let lane_end_frame = lane_end(s, lane);
            if lane_end_frame == 0 {
                return Err(format!("{name} has no clips to measure"));
            }
            let (from, to) = range_arg(args, fps, 0, lane_end_frame, &name)?;
            span = span_label(from, to, fps, &name);
            let g = f64::from(s.lane_gain_db(lane));
            let l = measure_lane(s, lane, from, to, fps)?
                .ok_or_else(|| format!("{name} has no sound to measure"))?;
            (shifted(l, g), Some(g), duck_note(s, lane))
        }
        Target::Clip(lane, clip) if lane.kind == LaneKind::Audio => {
            let (from, to) = range_arg(args, fps, clip.start, clip.end(), &name)?;
            span = span_label(from, to, fps, &name);
            let g = f64::from(s.lane_gain_db(lane));
            let l = measure_lane(s, lane, from, to, fps)?
                .ok_or_else(|| format!("{name} has no sound to measure"))?;
            (shifted(l, g), Some(g), duck_note(s, lane))
        }
        Target::Clip(_, clip) => {
            // A video clip's sound is its source's, read through the clip's in
            // point and speed.
            let (from, to) = range_arg(args, fps, clip.start, clip.end(), &name)?;
            span = span_label(from, to, fps, &name);
            let src_at = |t: u32| f64::from(clip.in_frame) + clip.source_offset_f(f64::from(t - clip.start));
            let range = (src_at(from) / fps, src_at(to) / fps);
            (source_loudness(s, clip.source, Some(range))?, None, None)
        }
        Target::Source(n) => {
            if let Some(why) = offline_refusal_for(s, &name, &[n]) {
                return Err(why);
            }
            // The file's own length bounds the range, so a range past its end is refused by name.
            let src = s.sources().get(n).ok_or_else(|| no_source(s, n))?;
            let secs = engine::probe_media(&src.path).duration_secs.filter(|d| *d > 0.0);
            let end = secs.map_or(u32::MAX, |d| (d * fps).ceil() as u32);
            let (from, to) = range_arg(args, fps, 0, end, &name)?;
            span = span_label(from, to, fps, &name);
            let range = (from != 0 || to != end).then(|| {
                let end_s = if end == u32::MAX { f64::INFINITY } else { f64::from(to) / fps };
                (f64::from(from) / fps, end_s)
            });
            (source_loudness(s, n, range)?, None, None)
        }
    };
    Ok(Read {
        name,
        span,
        lane_gain,
        duck,
        loud,
        seconds: started.elapsed().as_secs_f64(),
    })
}

/// A figure for text: one decimal, or `silent` for no energy.
fn figure(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.1}")
    } else {
        "silent".into()
    }
}

/// A true peak as the text shows it: one decimal, rounded half away from zero.
fn shown_peak(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// A true peak for text: one decimal with its sign, so a level over zero reads `+0.3`.
fn signed_figure(x: f64) -> String {
    if x.is_finite() {
        format!("{x:+.1}")
    } else {
        "silent".into()
    }
}

fn loudness_text(r: &Read) -> String {
    let l = &r.loud;
    let gain = match r.lane_gain {
        Some(g) => format!(", as played at lane gain {g:+.1} dB"),
        None => String::new(),
    };
    let duck = r.duck.as_ref().map_or_else(String::new, |d| format!(" · {d}"));
    let lra = if l.lra_samples >= 2 {
        format!("LRA {:.1} LU", l.range_lu)
    } else {
        "LRA n/a (needs 3.5 s or more of audio: two 3 s short-term values)".into()
    };
    let short = if l.analysed_secs >= SHORT_TERM_SECS {
        figure(l.short_term_max_lufs)
    } else {
        format!("n/a (under {SHORT_TERM_SECS} s of audio)")
    };
    format!(
        "{} loudness (EBU R128{gain}): integrated {} LUFS · {lra} · true peak {} dBTP · max momentary {} · max short-term {short}{duck} · analysed {:.2} s of audio in {:.2} s{warn}",
        r.name,
        figure(l.integrated_lufs),
        signed_figure(l.true_peak_dbtp),
        figure(l.momentary_max_lufs),
        l.analysed_secs,
        r.seconds,
        warn = peak_warning(l.true_peak_dbtp, l.integrated_lufs, &r.span, r.name == "mix").map_or_else(String::new, |w| format!(" · {w}")),
    )
}

/// The EBU R128 short-term window: no short-term value exists for less audio than this.
const SHORT_TERM_SECS: f64 = 3.0;

fn run_loudness(srv: &mut Server, args: &Value) -> Reply {
    let Some(raw) = opt_str(args, "target") else {
        return fail(
            "missing `target`: give a clip address such as A1@0, a lane such as A1, src:N, or mix"
                .into(),
        );
    };
    let s = tri!(srv.session_ref());
    let r = match measure_target(s, args, raw) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let text = loudness_text(&r);
    let l = r.loud;
    let structured = json!({
        "target": r.name,
        "integrated_lufs": l.integrated_lufs,
        // Null where the figure is undefined, not a 0.0 that reads as a measurement.
        "range_lu": if l.lra_samples >= 2 { json!(l.range_lu) } else { Value::Null },
        "true_peak_dbtp": l.true_peak_dbtp,
        "sample_peak_dbfs": l.sample_peak_dbfs,
        "momentary_max_lufs": l.momentary_max_lufs,
        "short_term_max_lufs": if l.analysed_secs >= SHORT_TERM_SECS { json!(l.short_term_max_lufs) } else { Value::Null },
        "analysed_secs": l.analysed_secs,
        "lane_gain_db": r.lane_gain,
        "seconds": r.seconds,
    });
    Reply::ok(text).with(structured)
}

/// The clip gain a boost past the lane cap put on a lane's clips, read after the change: each
/// clip that moved, by its address (`A1@450`) and the dB it gained, with the lane's gain.
struct Spill {
    lane: String,
    lane_db: f64,
    clips: Vec<(String, f64)>,
}

/// How many clip addresses a split names before it says "and N more".
const SPLIT_CLIPS_NAMED: usize = 8;

impl Spill {
    /// "lane A1 +12.0 dB, clips A1@0 A1@450 +6.0 dB clip gain", or the clips' own figures when they differ.
    fn text(&self) -> String {
        let lane = format!("lane {} {:+.1} dB", self.lane, self.lane_db);
        let same = self.clips.windows(2).all(|w| (w[0].1 - w[1].1).abs() <= 0.05);
        if same {
            let named: Vec<&str> = self.clips.iter().take(SPLIT_CLIPS_NAMED).map(|(a, _)| a.as_str()).collect();
            let more = self.clips.len().saturating_sub(SPLIT_CLIPS_NAMED);
            let and = if more > 0 { format!(" and {more} more") } else { String::new() };
            format!("{lane}, clips {}{and} {:+.1} dB clip gain", named.join(" "), self.clips[0].1)
        } else {
            let parts: Vec<String> = self.clips.iter().map(|(a, d)| format!("{a} {d:+.1} dB")).collect();
            format!("{lane}, clip gain {}", parts.join(", "))
        }
    }
}

/// The clip gain a plan's lane holds now beyond its old clip gains, or `None` when no clip moved.
fn spill_of(s: &PlaybackSession, p: &Plan) -> Option<Spill> {
    let clips: Vec<(String, f64)> = p
        .clips
        .iter()
        .filter_map(|&(i, old)| {
            let moved = f64::from(s.clip_gain_of(p.lane, i)?) - old;
            let start = s.lane_clips(p.lane).get(i)?.start;
            (moved.abs() > 0.05).then(|| (format!("{}@{start}", parse::lane_name(p.lane)), moved))
        })
        .collect();
    (!clips.is_empty()).then(|| Spill {
        lane: parse::lane_name(p.lane),
        lane_db: f64::from(s.lane_gain_db(p.lane)),
        clips,
    })
}

/// The gain a plan's lane really moved by in total: its lane gain, and the mean clip gain on top.
fn total_moved(s: &PlaybackSession, p: &Plan) -> f64 {
    let lane = f64::from(s.lane_gain_db(p.lane)) - p.old;
    if p.clips.is_empty() {
        return lane;
    }
    let clip: f64 = p
        .clips
        .iter()
        .map(|&(i, old)| f64::from(s.clip_gain_of(p.lane, i).unwrap_or(0.0)) - old)
        .sum::<f64>()
        / p.clips.len() as f64;
    lane + clip
}

/// The clips a boost put clip gain on, as the reply's structured output lists them: address and dB.
fn clip_gain_json<'a>(spills: impl Iterator<Item = &'a Spill>) -> Vec<Value> {
    spills
        .flat_map(|sp| sp.clips.iter().map(|(clip, db)| json!({ "clip": clip, "change_db": db })))
        .collect()
}

/// The cap a held normalize names: the clip gain when a lane's boost ran past the lane cap into its clips,
/// otherwise the lane gain (below the range).
fn cap_kind(clamped: &[(String, f64, f64)]) -> &'static str {
    if clamped.iter().any(|(_, wanted, _)| *wanted > f64::from(MAX_GAIN_DB)) {
        "clip_gain_limit"
    } else {
        "lane_gain_limit"
    }
}

/// How close a normalize has to land, in LU, before it stops refining.
const NORMALIZE_LUFS_TOLERANCE: f64 = 0.1;
/// Float slack on the tolerance, so a level exactly 0.1 LU off (after the arithmetic) still counts.
const LUFS_EPSILON: f64 = 1e-6;

/// Whether a measured integrated level lands on the target: within the tolerance, on the unrounded
/// values. The reply shows one decimal, so a level that reads -14.1 can still be within 0.1 of -14.0.
fn lufs_reached(lufs: f64, achieved: f64) -> bool {
    achieved.is_finite() && (lufs - achieved).abs() <= NORMALIZE_LUFS_TOLERANCE + LUFS_EPSILON
}
/// How far over the true-peak ceiling the measured sound may sit, in dB.
const NORMALIZE_PEAK_TOLERANCE: f64 = 0.05;
/// Corrections made after the first change, at most. With the master limiter in circuit each one
/// may also lower the limiter's ceiling by the true-peak overshoot the limiter lets through,
/// which takes several.
const NORMALIZE_PASSES: usize = 8;
/// Corrections allowed with the master limiter in circuit. Each one walks the limiter's ceiling down
/// by the overshoot, and the inter-sample peaks take several walks to clear, so this is more than the
/// plain cap: at 8 a walk could run out one step short of the ceiling.
const LIMITED_PASSES: usize = 24;

/// The corrections a normalize may make: `LIMITED_PASSES` with the limiter, `NORMALIZE_PASSES` without.
fn pass_cap(limited: bool) -> usize {
    if limited {
        LIMITED_PASSES
    } else {
        NORMALIZE_PASSES
    }
}

/// How far over the true-peak ceiling a measured result may sit and still count as held. With the
/// limiter in circuit the peak must be at or under the ceiling: the walk steps past it by a margin,
/// so a result that reads -0.96 against a -1.0 ceiling is not held.
fn peak_tolerance(limited: bool) -> f64 {
    if limited {
        0.0
    } else {
        NORMALIZE_PEAK_TOLERANCE
    }
}
/// How close under the limiter's ceiling a measured peak must sit to count as held at it, in dB.
const LIMITER_AT_CEILING_DB: f64 = 0.25;
/// How far (dB) the limiter's ceiling may be walked down from the target's, before the peaks are
/// called held. Measured from the target, not a fixed level: a target of -15 dBTP must walk too.
const LIMITER_WALK_DB: f64 = 11.0;

/// The lowest ceiling the limiter may be walked down to for a true-peak target `tp` (dBTP): the
/// walk allowance below it, never past the engine's own limit.
fn limiter_floor(tp: f64) -> f64 {
    (tp - LIMITER_WALK_DB).max(f64::from(Limiter::MIN_DB))
}

/// How far the measured true peak sits over the ceiling, in dB (negative when
/// under); 0 for silence.
fn peak_over(l: &Loudness, ceiling: f64) -> f64 {
    if l.true_peak_dbtp.is_finite() {
        l.true_peak_dbtp - ceiling
    } else {
        0.0
    }
}

/// Where a normalize's gain settles: the ceiling the limiter ends at, the
/// lanes the lane gain limit held back, the measured result, and the corrections made.
struct Settled {
    ceiling: f64,
    clamped: Vec<(String, f64, f64)>,
    after: Loudness,
    passes: usize,
    stuck: bool,
    /// The peaks were cut to the ceiling and the level was left short of it: the ceiling holds it.
    cut: bool,
}

/// Sets `lanes` to their old gains plus `gain` and refines until the level lands on `lufs` and the
/// peaks sit under `tp`, measuring after each change. Shared by the mix, one lane and `each`.
#[allow(clippy::too_many_arguments)]
fn settle(
    s: &mut PlaybackSession,
    raw: &str,
    plans: &[Plan],
    lufs: f64,
    tp: f64,
    limited: bool,
    mut ceiling: f64,
    mut gain: f64,
) -> Result<Settled, String> {
    let mut clamped = set_plans(s, plans, gain);
    // The result is measured, not predicted: a limiter, a lane gain limit or the
    // true peak moves the sound by less than the gain asks. Corrections follow
    // while the level misses or the peaks pass the ceiling.
    let mut after = measure_target(s, &json!({}), raw)?.loud;
    let max_passes = pass_cap(limited);
    let tol = peak_tolerance(limited);
    let mut passes = 0;
    // Set once the peaks have been cut to the ceiling: a louder step after that
    // would only breach it again, so the level is left where the ceiling holds it.
    let mut cut = false;
    // Set when a cut left the measured true peak where it was: the peaks are held by
    // something gain does not move (the limiter's inter-sample overshoot), so more cuts are wasted.
    let mut stuck = false;
    let mut prev_over = f64::INFINITY;
    // The level a gain step moves: learned from the last two measurements, since a limiter in
    // circuit moves the level by less than the gain it is given.
    let mut slope = 1.0_f64;
    let mut last: Option<(f64, f64)> = None;
    while passes < max_passes && after.integrated_lufs.is_finite() {
        let miss = lufs - after.integrated_lufs;
        let over = peak_over(&after, tp);
        // Every lane held at its gain limit ends the search, unless the limiter still has its peak to
        // walk down: a clamp moves no more level, but the ceiling can still come down under the peak.
        if (clamped.len() == plans.len() && (!limited || over <= tol))
            || (lufs_reached(lufs, after.integrated_lufs) && over <= tol)
        {
            break;
        }
        if let Some((g0, l0)) = last {
            let (dg, dl) = (gain - g0, after.integrated_lufs - l0);
            if dg.abs() > 0.05 && (dl / dg).is_finite() {
                slope = (dl / dg).clamp(0.2, 2.0);
            }
        }
        last = Some((gain, after.integrated_lufs));
        let level_step = miss / slope;
        if limited && over > tol {
            // The limiter holds its true-peak ceiling, so a measured overshoot is rare. When one
            // comes, the ceiling comes down by the overshoot, and the level is taken again with it.
            let next = ceiling - over - NORMALIZE_PEAK_TOLERANCE;
            if next < limiter_floor(tp) {
                stuck = true;
                break;
            }
            ceiling = next;
            if !s.set_limiter(Limiter { on: true, ceiling_db: ceiling as f32 }) {
                stuck = true;
                break;
            }
            gain += level_step;
        } else {
            if cut && over > tol && over >= prev_over - 0.02 {
                stuck = true;
                break;
            }
            prev_over = over;
            let step = if over > tol {
                cut = true;
                level_step.min(-over)
            } else if cut && miss > 0.0 {
                break;
            } else if !limited && over < NORMALIZE_PEAK_TOLERANCE && level_step > -over {
                // The peaks have less headroom under the ceiling than the level step needs. Gain
                // moves the peaks about as far as it moves the level, so the step stops at the
                // ceiling; a step past it would be found only after a lane clamp had ended the search.
                cut = true;
                -over
            } else {
                level_step
            };
            if step.abs() < 0.01 {
                break;
            }
            gain += step;
        }
        clamped = set_plans(s, plans, gain);
        passes += 1;
        after = measure_target(s, &json!({}), raw)?.loud;
    }

    Ok(Settled { ceiling, clamped, after, passes, stuck, cut })
}

/// The loudness `normalize` aims for (`lufs`, default -14) and the true-peak ceiling (`true_peak`,
/// default -1), checked.
fn normalize_levels(args: &Value) -> Result<(f64, f64), String> {
    let lufs = match opt_f64(args, "lufs") {
        None => -14.0,
        Some(x) if x.is_finite() && (-70.0..=0.0).contains(&x) => x,
        Some(x) => return Err(format!("lufs {x} is out of range; it must be -70 to 0")),
    };
    let tp = match opt_f64(args, "true_peak") {
        None => -1.0,
        Some(x) if x.is_finite() && (-20.0..=0.0).contains(&x) => x,
        Some(x) => return Err(format!("true_peak {x} is out of range; it must be -20 to 0")),
    };
    Ok((lufs, tp))
}

/// One lane of `normalize` with `each`, as measured before any change.
struct EachLane {
    name: String,
    played: Loudness,
    delta: f64,
    tp_binds: bool,
    plan: Plan,
}

/// What a lossy export can add to the true peak: an AAC or Opus encode may land above the timeline's
/// peaks, so a ceiling right at 0 dBTP leaves nothing for it.
const LOSSY_HEADROOM_NOTE: &str = "AAC/Opus encoding can add up to about 1 dB of true peak; for a lossy export aim the ceiling at -2 dBTP";

/// The words a normalize reply leads with: whether the target was reached, before anything else.
fn verdict(reached: bool) -> &'static str {
    if reached {
        "target reached: "
    } else {
        "short of the target: "
    }
}

/// How far a measured level sits from the target, as a reply words it: "0.90 LU short" or "0.20 LU over".
fn short_text(lufs: f64, achieved: f64) -> String {
    if achieved <= lufs {
        format!("{:.2} LU short", lufs - achieved)
    } else {
        format!("{:.2} LU over", achieved - lufs)
    }
}

/// The note a rollback that could not take every step back adds; empty when it did.
fn rollback_note(rolled: bool) -> &'static str {
    if rolled {
        ""
    } else {
        " (could not roll every step back; check the project)"
    }
}

/// Opens a trial for a dry run: the change is made as a real run makes it, then `finish_trial` takes
/// it back. The redo history is set aside, since the trial's own steps would clear it, and the undo
/// cap holds, so a rolled-back trial leaves the history as it found it (as a rolled-back batch does).
fn start_trial(s: &mut PlaybackSession, dry: bool) -> Option<RedoCheckpoint> {
    dry.then(|| {
        s.hold_history(true);
        s.take_redo()
    })
}

/// Takes a trial's steps back, since `start`, and puts its redo history back. True if every step came back.
fn finish_trial(s: &mut PlaybackSession, start: u64, redo: RedoCheckpoint) -> bool {
    let rolled = undo_n(s, pushed_since(s, start));
    s.restore_redo(redo);
    s.hold_history(false);
    rolled
}

/// A dry run's set-aside history: its redo stack, and the ledger's redo entries.
struct Trial {
    redo: RedoCheckpoint,
    ledger: Vec<Entry>,
}

/// Undoes a cut since `start`. A dry run (`trial`) puts back the redo history it set aside. A cut
/// rolled back for real leaves no branch to redo, as edit.rs's batch rollback did before it kept
/// redo. True if every step came back.
fn roll_back_cut(srv: &mut Server, start: u64, trial: Option<Trial>) -> bool {
    let Some(s) = srv.session.as_mut() else {
        return false;
    };
    match trial {
        Some(t) => {
            srv.redo_ledger = t.ledger;
            finish_trial(s, start, t.redo)
        }
        None => {
            let n = pushed_since(s, start);
            let rolled = undo_n(s, n);
            if n > 0 {
                s.clear_redo();
                srv.redo_ledger.clear();
            }
            rolled
        }
    }
}

/// Whether `allow_limiter` puts the master limiter in the way of a normalize. The linear prediction
/// (`tp_binds`: gain would carry the peaks over `tp`) is one reason; the other is a mix that already
/// sits over `tp`, because a clipped mix does not fall linearly with gain (the clip moves with it).
fn limiter_in_the_way(allow_limiter: bool, tp_binds: bool, played_tp: f64, tp: f64) -> bool {
    allow_limiter && (tp_binds || played_tp > tp)
}

/// The change a normalize makes, measured as it goes. With `limited` the master limiter goes on at
/// the ceiling and the gain is taken again from the mix as it then measures; the limiter holds the
/// peaks down so the rest of the gain can land. A real run and a dry run both make this change.
#[allow(clippy::too_many_arguments)]
fn normalize_trial(
    s: &mut PlaybackSession,
    raw: &str,
    plans: &[Plan],
    lufs: f64,
    hold: f64,
    limited: bool,
    delta: f64,
) -> Result<Settled, String> {
    let mut gain = delta;
    // The limiter's detector is the meter's own true-peak method, so its ceiling is the target: the
    // walk aims at the true_peak value (or the lower limiter ceiling) with or without the limiter.
    if limited {
        // A limiter already on at this ceiling is no change to make, not a refusal.
        let want = Limiter { on: true, ceiling_db: hold as f32 };
        if s.limiter() != want && !s.set_limiter(want) {
            return Err("the engine refused the limiter change".into());
        }
        let r = measure_target(s, &json!({}), raw)?;
        if r.loud.integrated_lufs.is_finite() {
            gain = lufs - r.loud.integrated_lufs;
        }
    }
    settle(s, raw, plans, lufs, hold, limited, hold, gain)
}

/// The past tense a reply uses for a change that was made, or the conditional for a dry run, which
/// makes the change only to measure it and says so.
fn mood(dry: bool, done: &'static str, would: &'static str) -> &'static str {
    if dry {
        would
    } else {
        done
    }
}

/// `normalize` with `each`: every audio lane in `lanes` is set on its own, so that each lane alone
/// measures `lufs` over its clips and its peaks stay under `true_peak`. One undo step for all of
/// them. Nothing changes if any listed lane cannot be measured.
fn run_normalize_each(srv: &mut Server, args: &Value) -> Reply {
    let (lufs, tp) = match normalize_levels(args) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    if arg_given(args, "target") {
        return fail("each takes `lanes`, not `target`: list the audio lanes to set, such as [\"A1\",\"A2\"]".into());
    }
    if opt_bool(args, "allow_limiter").unwrap_or(false) {
        return fail("allow_limiter goes with target mix: the master limiter is mix-wide, so it cannot be set for each lane".into());
    }
    let Some(list) = args.get("lanes").and_then(Value::as_array) else {
        return fail("each needs `lanes`: the audio lanes to set, such as [\"A1\",\"A2\"]".into());
    };
    let s = tri!(srv.session_ref());
    // A master limiter already on is mix-wide and already in the mix: the flag would change nothing.
    let limiter_on = s.limiter().on;
    let mut lanes: Vec<(Lane, String)> = Vec::new();
    for raw in list {
        let Some(raw) = raw.as_str() else {
            return fail("lanes takes lane names, such as [\"A1\",\"A2\"]".into());
        };
        match resolve(s, raw) {
            Ok(Target::Lane(l)) if l.kind == LaneKind::Audio => {
                if lanes.iter().any(|(k, _)| *k == l) {
                    return fail(format!("{raw} is listed twice in lanes"));
                }
                lanes.push((l, raw.to_string()));
            }
            Ok(_) => return fail(format!("normalize takes audio lanes (A1) in lanes; `{raw}` is not one")),
            Err(e) => return fail(e),
        }
    }
    if lanes.is_empty() {
        return fail("lanes is empty: list the audio lanes to set, such as [\"A1\",\"A2\"]".into());
    }
    let picked: Vec<Lane> = lanes.iter().map(|(l, _)| *l).collect();
    if let Err(e) = super::edit::check_unlocked(s, &picked) {
        return fail(e);
    }

    // Every lane is measured alone before anything moves, so a silent lane refuses the whole call.
    let mut each: Vec<EachLane> = Vec::new();
    for (lane, raw) in &lanes {
        let before = match measure_target(s, &json!({}), raw) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let played = before.loud;
        if !played.integrated_lufs.is_finite() {
            return fail(format!("{} is silent: nothing to normalize", before.name));
        }
        let delta = gain_to_target(&played, lufs, tp);
        let tp_binds = played.true_peak_dbtp.is_finite()
            && tp - played.true_peak_dbtp < lufs - played.integrated_lufs;
        each.push(EachLane {
            name: before.name,
            played,
            delta,
            tp_binds,
            plan: plan_of(s, *lane),
        });
    }

    let dry = opt_bool(args, "dry_run").unwrap_or(false);
    let was_clean = !super::project::dirty(srv);
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let start = s.history_pushes();
    let redo = start_trial(s, dry);
    // Each lane is set and settled in turn. The gain each lane really moved by is read before a dry
    // run takes the change back, so the dry run reports what the real run does.
    let settled: Result<Vec<(Settled, f64, Option<Spill>)>, String> = each
        .iter()
        .map(|e| -> Result<(Settled, f64, Option<Spill>), String> {
            let st = settle(s, &e.name, std::slice::from_ref(&e.plan), lufs, tp, false, tp, e.delta)?;
            let applied = total_moved(s, &e.plan);
            Ok((st, applied, spill_of(s, &e.plan)))
        })
        .collect();
    let changed = s.history_pushes() > start;
    let rolled = redo.is_none_or(|r| finish_trial(s, start, r));
    if dry {
        keep_clean_marks(srv, was_clean);
    }
    let rows = match settled {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let changed_kept = changed && !dry;
    if changed_kept {
        let names: Vec<&str> = each.iter().map(|e| e.name.as_str()).collect();
        record_pushes(srv, start, format!("normalize each {} {lufs:.1} LUFS", names.join(" ")));
    }

    let mut parts = Vec::new();
    let mut lane_json = Vec::new();
    let mut all_reached = true;
    let mut held_by_peak = false;
    // The headline's clauses, one per lane: what the lane moved by and the level it reached.
    let mut clauses: Vec<String> = Vec::new();
    // Each lane the true peak held back, with the peak it measured before the change.
    let mut peak_names: Vec<String> = Vec::new();
    for (e, (st, applied, spill)) in each.iter().zip(&rows) {
        let achieved = st.after.integrated_lufs;
        let over = peak_over(&st.after, tp);
        let level_ok = lufs_reached(lufs, achieved);
        let reached = level_ok && over <= NORMALIZE_PEAK_TOLERANCE;
        all_reached &= reached;
        let capped_by: Option<&str> = if reached {
            None
        } else if !st.clamped.is_empty() {
            Some(cap_kind(&st.clamped))
        } else if e.tp_binds || over > NORMALIZE_PEAK_TOLERANCE {
            Some("true_peak")
        } else {
            None
        };
        held_by_peak |= capped_by == Some("true_peak");
        let moved = if *applied > 0.05 {
            format!("{} {} {:.1} dB to {achieved:.1} LUFS", e.name, mood(dry, "raised", "would raise"), applied)
        } else if *applied < -0.05 {
            format!("{} {} {:.1} dB to {achieved:.1} LUFS", e.name, mood(dry, "lowered", "would lower"), -applied)
        } else {
            format!("{} {} {achieved:.1} LUFS", e.name, mood(dry, "left at", "would be left at"))
        };
        let clause_status = match capped_by {
            _ if reached => String::new(),
            Some("clip_gain_limit") => ", held by the clip gain limit".to_string(),
            Some("lane_gain_limit") => ", held by the lane gain limit".to_string(),
            Some("true_peak") => String::new(),
            _ => format!(", {} {lufs:.1}", mood(dry, "did not reach", "would not reach")),
        };
        clauses.push(format!("{moved}{clause_status}"));
        if !reached && capped_by == Some("true_peak") {
            peak_names.push(format!("{} peaked at {} dBTP before", e.name, signed_figure(e.played.true_peak_dbtp)));
        }
        let status = match capped_by {
            _ if reached => String::new(),
            Some("clip_gain_limit") => format!(
                " — held by the clip gain limit: lane {:+.0} dB and each clip {:+.0} dB",
                f64::from(MAX_GAIN_DB),
                f64::from(MAX_CLIP_GAIN_DB)
            ),
            Some("lane_gain_limit") => format!(
                " — held by the lane gain limit {:+.0}..{:+.0} dB",
                f64::from(MIN_GAIN_DB),
                f64::from(MAX_GAIN_DB)
            ),
            Some(_) if !level_ok => format!(
                " — {}, held by the true-peak ceiling {tp:.1} dBTP",
                short_text(lufs, achieved)
            ),
            Some(_) => format!(" — held by the true-peak ceiling {tp:.1} dBTP"),
            None => format!(" — {} {lufs:.1}: got {achieved:.2} LUFS", mood(dry, "did not reach", "would not reach")),
        };
        let stuck = if st.stuck {
            " · stopped refining: the last cut did not move the true peak"
        } else {
            ""
        };
        let split = spill.as_ref().map_or(String::new(), |sp| format!(" · {}", sp.text()));
        parts.push(format!(
            "{} {} → {} LUFS (gain {applied:+.2} dB, true peak {} → {} dBTP){status}{stuck}{split}",
            e.name,
            figure(e.played.integrated_lufs),
            figure(achieved),
            signed_figure(e.played.true_peak_dbtp),
            signed_figure(st.after.true_peak_dbtp),
        ));
        lane_json.push(json!({
            "lane": e.name,
            "before_integrated_lufs": e.played.integrated_lufs,
            "after_integrated_lufs": achieved,
            "applied_db": applied,
            "reached": reached,
            "capped_by": capped_by,
            "measured_true_peak_dbtp": st.after.true_peak_dbtp,
            "refinements": st.passes,
            "clamped": st.clamped.iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>(),
            "clip_gain": clip_gain_json(spill.iter()),
        }));
    }
    // The headline says what the lanes reached, not only what was asked: a lane held by the peak
    // ceiling does not reach the target, and the parts below give each lane's level.
    // A lane that missed the target names what it applied and reached; lanes held by the true-peak
    // ceiling say so once, with the way to reach the target on the mix.
    let lossy = if held_by_peak || each.iter().any(|e| e.tp_binds) {
        format!(" · {LOSSY_HEADROOM_NOTE}")
    } else {
        String::new()
    };
    let (lead, undo) = if dry {
        ("dry run — ", format!(" · nothing changed: measured, then taken back{}", rollback_note(rolled)))
    } else if changed_kept {
        ("", format!(" · undo step {}", srv.ledger.len()))
    } else {
        ("", String::from(" · no change"))
    };
    let text = if all_reached {
        format!("{}{lead}{} each lane to {lufs:.1} LUFS: {}{undo}", verdict(true), mood(dry, "normalized", "would normalize"), parts.join("; "))
    } else {
        let mut head = clauses.join("; ");
        if !peak_names.is_empty() {
            let who = match peak_names.len() {
                1 => "held".to_string(),
                2 => "both held".to_string(),
                n => format!("all {n} held"),
            };
            head.push_str(&format!(
                " — {who} by the {tp:.1} dBTP ceiling ({}); no lane alone reaches {lufs:.1} past it{}",
                peak_names.join(", "),
                if limiter_on {
                    "; the master limiter is already on, so normalize target mix can reach it on the mix"
                } else {
                    "; normalize target mix with allow_limiter:true to reach it on the mix"
                }
            ));
        }
        format!("{}{lead}{head}{lossy}{undo} · {}", verdict(false), parts.join("; "))
    };
    Reply::ok(text).with(json!({
        "each": true,
        "lufs": lufs,
        "true_peak_dbtp": tp,
        "reached": all_reached,
        "lanes": lane_json,
        "dry_run": dry,
        "changed": changed_kept,
        "undo_steps": srv.ledger.len(),
    }))
}


fn run_normalize(srv: &mut Server, args: &Value) -> Reply {
    if opt_bool(args, "each").unwrap_or(false) {
        return run_normalize_each(srv, args);
    }
    if arg_given(args, "lanes") {
        return fail("lanes goes with each:true: each lane is set on its own. Add each:true, or use target for one lane".into());
    }
    let Some(raw) = opt_str(args, "target") else {
        return fail("missing `target`: give a lane such as A1, or mix".into());
    };
    let (lufs, tp) = match normalize_levels(args) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let allow_limiter = opt_bool(args, "allow_limiter").unwrap_or(false);
    let mix = is_mix(raw);
    if allow_limiter && !mix {
        return fail(format!(
            "allow_limiter goes with target mix, not with {raw}: the master limiter is mix-wide, it acts on every lane at once and cannot be set for one lane. To do both, normalize {raw} without it (that moves its lane gain only), then run normalize with target mix and allow_limiter:true to limit the mix"
        ));
    }

    let s = tri!(srv.session_ref());
    if !mix {
        match resolve(s, raw) {
            Ok(Target::Lane(l)) if l.kind == LaneKind::Audio => {}
            Ok(_) => {
                return fail(format!(
                    "normalize takes an audio lane (A1) or mix; `{raw}` is not one"
                ));
            }
            Err(e) => return fail(e),
        }
    }
    let before = match measure_target(s, &json!({}), raw) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let played = before.loud;
    if !played.integrated_lufs.is_finite() {
        return fail(format!("{} is silent: nothing to normalize", before.name));
    }
    // The limiter as it stood before the trial: a dry run reads its wording from this. A master
    // limiter already on below true_peak is the ceiling the peaks are held to, so the gain is
    // predicted against it, not against true_peak.
    let was_limiter = s.limiter();
    let counted_on = mix && was_limiter.on;
    let hold = if counted_on { tp.min(f64::from(was_limiter.ceiling_db)) } else { tp };
    let delta = gain_to_target(&played, lufs, hold);
    // The true peak holds the gain back when it is the tighter of the two limits.
    let tp_binds = played.true_peak_dbtp.is_finite()
        && hold - played.true_peak_dbtp < lufs - played.integrated_lufs;

    // Each lane that moves, with its gain now.
    let lanes: Vec<Lane> = if mix {
        s.lanes()
            .into_iter()
            .filter(|l| {
                l.kind == LaneKind::Audio && !s.lane_clips(*l).is_empty() && !s.lane_muted(*l)
            })
            .collect()
    } else {
        match resolve(s, raw) {
            Ok(Target::Lane(l)) => vec![l],
            _ => Vec::new(),
        }
    };
    if lanes.is_empty() {
        return fail("the mix has no unmuted audio lane with clips to move".into());
    }
    // A locked lane is one a normalize would move, so the whole call refuses before anything changes.
    if let Err(e) = super::edit::check_unlocked(s, &lanes) {
        return fail(e);
    }
    // Each lane as it stands: its lane gain, and its clips' gains for a boost past the lane cap.
    let plans: Vec<Plan> = lanes.iter().map(|&l| plan_of(s, l)).collect();
    let olds: Vec<f64> = plans.iter().map(|p| p.old).collect();

    let dry = opt_bool(args, "dry_run").unwrap_or(false);
    let was_clean = !super::project::dirty(srv);
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let start = s.history_pushes();
    let was_on = was_limiter.on;
    // A mix with the master limiter already on counts on it, as allow_limiter would: the flag then
    // changes nothing, so it is never advised for this case.
    let limiter_asked = allow_limiter || counted_on;
    let redo = start_trial(s, dry);
    // allow_limiter with the true peak in the way: the master limiter goes on at the ceiling, and
    // the gain is taken again from the mix as it then measures. A dry run makes the same change and
    // takes it back, so its numbers are the real run's. The peak is in the way when the linear
    // prediction says so (`tp_binds`), or when the mix already sits over the ceiling: a clipped mix
    // does not fall with gain the way the prediction assumes (the clip is what the peak is made of),
    // so a linear walk down chases it, while the limiter, ahead of the clip, holds it linearly.
    let limited = limiter_in_the_way(limiter_asked, tp_binds, played.true_peak_dbtp, hold);
    let trial = normalize_trial(s, raw, &plans, lufs, hold, limited, delta);
    let changed = s.history_pushes() > start;
    let lim = s.limiter();
    // The lane gain each lane really holds after the change: the clamp can hold a lane back.
    let news: Vec<f64> = lanes.iter().map(|&l| f64::from(s.lane_gain_db(l))).collect();
    // The gain each lane really moved by in total (lane gain and clip gain), and the clip gain a boost
    // past the lane cap put on its clips: read here, before a dry run takes the change back.
    let totals: Vec<f64> = plans.iter().map(|p| total_moved(s, p)).collect();
    let spills: Vec<Option<Spill>> = plans.iter().map(|p| spill_of(s, p)).collect();
    let rolled = redo.is_none_or(|r| finish_trial(s, start, r));
    if dry {
        keep_clean_marks(srv, was_clean);
    }
    let st = match trial {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    let changed_kept = changed && !dry;
    if changed_kept {
        let what = if mix { "mix".to_string() } else { raw.to_string() };
        let with = if limited { " with the limiter" } else { "" };
        record_pushes(srv, start, format!("normalize {what} {lufs:.1} LUFS{with}"));
    }

    let Settled { ceiling, clamped, after, passes, stuck, cut, .. } = st;
    let max_passes = pass_cap(limited);
    let tol = peak_tolerance(limited);
    // The lane gain each lane really moved by, and the mean of those (the one change for a lane).
    let moved: Vec<f64> = news.iter().zip(&olds).map(|(n, o)| n - o).collect();
    let applied = totals.iter().sum::<f64>() / totals.len() as f64;
    // Each lane's change: the split where a boost went on its clips, else its lane gain.
    let lane_moves: Vec<String> = lanes
        .iter()
        .zip(&moved)
        .zip(&spills)
        .map(|((&l, m), sp)| match sp {
            Some(sp) => sp.text(),
            None => format!("lane gain {} {m:+.2} dB", parse::lane_name(l)),
        })
        .collect();
    // The split of every lane the boost reached into its clips, for a reply that is not reached.
    let split_note: Vec<String> = spills.iter().flatten().map(Spill::text).collect();

    let achieved = after.integrated_lufs;
    let over = peak_over(&after, hold);
    let level_ok = lufs_reached(lufs, achieved);
    let reached = level_ok && over <= tol;
    let short = if achieved.is_finite() {
        (lufs - achieved).max(0.0)
    } else {
        0.0
    };
    let limiter_holds = over > tol && (limited || lim.on);
    // A limiter on below the true_peak target holds the peaks at its own ceiling: a level short of
    // the target there is the limiter's doing, whatever the lane gains say, since the gain it takes back is
    // what the walk asked for.
    let limiter_at_ceiling = lim.on
        && f64::from(lim.ceiling_db) < tp
        && over <= tol
        && after.true_peak_dbtp.is_finite()
        && after.true_peak_dbtp >= f64::from(lim.ceiling_db) - LIMITER_AT_CEILING_DB;
    let capped_by: Option<&str> = if reached {
        None
    } else if limiter_at_ceiling {
        Some("limiter")
    } else if !clamped.is_empty() {
        Some(cap_kind(&clamped))
    } else if limiter_holds {
        Some("limiter")
    } else if tp_binds || over > tol || cut {
        Some("true_peak")
    } else {
        None
    };

    let head = if reached {
        if mix && limited {
            format!(
                "{} the mix to {lufs:.1} LUFS with the master limiter on at {ceiling:.1} dBTP: {} {} audio lane(s): {}",
                mood(dry, "normalized", "would normalize"),
                mood(dry, "moved", "would move"),
                lanes.len(),
                lane_moves.join("; ")
            )
        } else if mix {
            format!(
                "{} the mix to {lufs:.1} LUFS: {} {} audio lane(s): {}",
                mood(dry, "normalized", "would normalize"),
                mood(dry, "moved", "would move"),
                lanes.len(),
                lane_moves.join("; ")
            )
        } else {
            match &spills[0] {
                Some(sp) => format!("{} {} to {lufs:.1} LUFS: {}", mood(dry, "normalized", "would normalize"), before.name, sp.text()),
                None => format!(
                    "{} {} to {lufs:.1} LUFS: lane gain {:+.1} → {:+.1} dB",
                    mood(dry, "normalized", "would normalize"), before.name, olds[0], news[0]
                ),
            }
        }
    } else {
        let what = if mix { "mix" } else { before.name.as_str() };
        // The outcome first, then what held it. A level within the tolerance reads as reached, and
        // the peak that kept it from the target is named: "mix reached -14.1 LUFS (true peak held
        // at ceiling)". Otherwise "did not reach" says what was asked, what came out and how far off.
        let peak_held = matches!(capped_by, Some("true_peak") | Some("limiter"));
        let gap = if level_ok {
            let held = if peak_held { " (true peak held at ceiling)" } else { "" };
            format!("{what} {} {} LUFS{held}", mood(dry, "reached", "would reach"), figure(achieved))
        } else if changed_kept && applied.abs() >= 0.05 && achieved.is_finite() {
            // The gain went on, so the reply leads with what it did; "did not reach" alone reads as if
            // nothing moved. The reason that held it follows, as below.
            let verb = if applied > 0.0 { mood(dry, "raised", "would raise") } else { mood(dry, "lowered", "would lower") };
            format!(
                "{what} {verb} {:.1} dB to {} LUFS, {}",
                applied.abs(),
                figure(achieved),
                short_text(lufs, achieved)
            )
        } else {
            // Two decimals here: a level that reads -14.1 may still be 0.14 off, and that is why it missed.
            let how_far = if achieved > lufs {
                " (above it)".to_string()
            } else {
                format!(" ({})", short_text(lufs, achieved))
            };
            format!("{what} {} {lufs:.1}: got {achieved:.2} LUFS{how_far}", mood(dry, "did not reach", "would not reach"))
        };
        match capped_by {
            // The lane is at its +12 dB cap and every clip at its +24 dB: nothing more can be added.
            Some("clip_gain_limit") => {
                let needs: Vec<String> = clamped
                    .iter()
                    .map(|(name, wanted, got)| format!("{name} needs {wanted:+.1} dB, reaches {got:+.1}"))
                    .collect();
                format!(
                    "{gap}: {} — lane gain +{:.0} dB and clip gain +{:.0} dB on each clip is the most; aim lower or lower the target",
                    needs.join("; "),
                    f64::from(MAX_GAIN_DB),
                    f64::from(MAX_CLIP_GAIN_DB)
                )
            }
            Some("lane_gain_limit") => {
                let needs: Vec<String> = clamped
                    .iter()
                    .map(|(name, wanted, _)| {
                        format!("{name} needs {wanted:+.1} dB but lane gain is held at {:+.0}..{:+.0} dB", f64::from(MIN_GAIN_DB), f64::from(MAX_GAIN_DB))
                    })
                    .collect();
                format!("{gap}: {}", needs.join("; "))
            }
            Some("limiter") if f64::from(lim.ceiling_db) < tp && over <= tol => format!(
                "{gap}, held by the master limiter at {:.1} dBTP (the mix peaks at {} dBTP); project_settings limiter {{\"on\": true, \"ceiling_db\": {tp:.1}}} raises that ceiling to true_peak",
                lim.ceiling_db,
                signed_figure(after.true_peak_dbtp),
            ),
            Some("limiter") => format!(
                "{gap}: the master limiter (on at {:.1} dBTP) is already at its floor {:.0} dBTP and the true peak {} dBTP is still {over:.1} dB over {tp:.1}; lower the target or true_peak",
                lim.ceiling_db,
                limiter_floor(tp),
                signed_figure(after.true_peak_dbtp),
            ),
            // The level is on the target and the peak alone holds it back: the fix is the same
            // advice without the "did not reach" that the level no longer has.
            // The master limiter already on below true_peak is the ceiling: the fix is the limiter's own.
            Some("true_peak") if counted_on && hold < tp => format!(
                "{gap}, held by the master limiter at {hold:.1} dBTP (already on; the mix peaks at {} dBTP); project_settings limiter {{\"on\": true, \"ceiling_db\": {tp:.1}}} raises that ceiling to true_peak",
                signed_figure(after.true_peak_dbtp),
            ),
            Some("true_peak") if level_ok && mix && limiter_asked => format!("{gap}; lower the target or true_peak"),
            Some("true_peak") if level_ok && mix => format!("{gap}; allow_limiter:true (or lower the target)"),
            // A lane under a limiter already on: the flag is not the fix, and the limiter is mix-wide.
            Some("true_peak") if level_ok && was_on => format!("{gap}; lower the target or true_peak"),
            Some("true_peak") if level_ok => format!(
                "{gap}; the master limiter is mix-wide: normalize target mix with allow_limiter:true (or lower the target)"
            ),
            // With the limiter already allowed, the fix it could offer is the one just tried.
            Some("true_peak") if mix && limiter_asked => format!(
                "{gap}, held by the true-peak ceiling {hold:.1} dBTP; lower the target or true_peak"
            ),
            Some("true_peak") if was_on => format!(
                "{gap}, held by the true-peak ceiling {hold:.1} dBTP; lower the target or true_peak"
            ),
            Some("true_peak") if mix => format!(
                "{gap}, held by the true-peak ceiling {hold:.1} dBTP — allow_limiter:true (or lower the target)"
            ),
            Some("true_peak") => format!(
                "{gap}, held by the true-peak ceiling {hold:.1} dBTP — the master limiter is mix-wide: normalize target mix with allow_limiter:true (or lower the target)"
            ),
            _ => gap,
        }
    };
    let clamp_note = if clamped.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = clamped
            .iter()
            .map(|(name, wanted, got)| format!("{name} wanted {wanted:+.1} dB, held at {got:+.1}"))
            .collect();
        if cap_kind(&clamped) == "clip_gain_limit" {
            format!(
                " · held at lane {:+.0} dB and each clip {:+.0} dB: {}",
                f64::from(MAX_GAIN_DB),
                f64::from(MAX_CLIP_GAIN_DB),
                list.join("; ")
            )
        } else {
            format!(
                " · held at the limit {:+.0}..{:+.0} dB: {}",
                f64::from(MIN_GAIN_DB),
                f64::from(MAX_GAIN_DB),
                list.join("; ")
            )
        }
    };
    let refined = if passes > 0 {
        format!(" · {passes} refinement pass(es)")
    } else {
        String::new()
    };
    let mut tail = String::new();
    // A reached reply names the split in its head; one that fell short names it here.
    if !reached && !split_note.is_empty() {
        tail.push_str(&format!(" · {}", split_note.join("; ")));
    }
    if !reached && capped_by.is_none() && passes >= max_passes {
        tail.push_str(&format!(" · stopped after {max_passes} corrections, the most tried"));
    }
    if stuck {
        tail.push_str(" · stopped refining: the last cut did not move the true peak");
    }
    if limited && achieved.is_finite() && played.integrated_lufs.is_finite() {
        // The lane gain is what the mix's loudness gets; the limiter takes back the rest of it.
        let took = applied - (achieved - played.integrated_lufs);
        if took.abs() >= 0.05 {
            tail.push_str(&format!(" · the limiter took back {took:.2} dB of that lane gain"));
        }
    }
    if allow_limiter && lim.on && dry {
        // Nothing is kept: the reply says what the real run would do to the limiter.
        let verb = if was_on { "leave" } else { "turn" };
        tail.push_str(&format!(" · would {verb} the limiter on at {:.1} dBTP", lim.ceiling_db));
    } else if allow_limiter && lim.on {
        tail.push_str(&format!(
            " · limiter left on at {:.1} dBTP (limiter off to remove: project_settings limiter {{\"on\": false}})",
            lim.ceiling_db
        ));
    }
    if counted_on && limited {
        tail.push_str(&format!(
            " · the master limiter was already on at {:.1} dBTP and is counted on in the gain",
            was_limiter.ceiling_db
        ));
    }
    let ceiling_involved = limited || lim.on || tp_binds || capped_by.is_some();
    let lossy = if ceiling_involved {
        format!(" · {LOSSY_HEADROOM_NOTE}")
    } else {
        String::new()
    };
    let (lead, undo) = if dry {
        ("dry run — ", format!(" · nothing changed: measured, then taken back{}", rollback_note(rolled)))
    } else if changed_kept {
        // The entry recorded above is the last one, so its number is the ledger's length.
        ("", format!(" · undo step {}", srv.ledger.len()))
    } else {
        ("", String::from(" · no change"))
    };
    let text = format!(
        "{}{lead}{head}; integrated {} → {} LUFS, true peak {} → {} dBTP (measured){refined}{clamp_note}{tail}{lossy}{undo}",
        verdict(reached),
        figure(played.integrated_lufs),
        figure(achieved),
        signed_figure(played.true_peak_dbtp),
        signed_figure(after.true_peak_dbtp),
    );
    let limiter = if limited {
        json!({ "on": true, "ceiling_db": ceiling })
    } else {
        Value::Null
    };
    let lane_moves_json: Vec<Value> = lanes
        .iter()
        .zip(olds.iter().zip(&news))
        .map(|(&l, (&from, &to))| {
            json!({ "lane": parse::lane_name(l), "from_db": from, "to_db": to, "change_db": to - from })
        })
        .collect();
    Reply::ok(text).with(json!({
        "target": before.name,
        "lufs": lufs,
        "achieved_lufs": achieved,
        "reached": reached,
        "short_by_lu": short,
        "capped_by": capped_by,
        "true_peak_dbtp": tp,
        "measured_true_peak_dbtp": after.true_peak_dbtp,
        "allow_limiter": allow_limiter,
        "limiter_counted_on": counted_on,
        "limiter": limiter,
        "applied_db": applied,
        "lane_moves": lane_moves_json,
        "before_integrated_lufs": played.integrated_lufs,
        "after_integrated_lufs": achieved,
        "after_measured": true,
        "refinements": passes,
        "clamped": clamped.iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>(),
        "clip_gain": clip_gain_json(spills.iter().flatten()),
        "dry_run": dry,
        "changed": changed_kept,
        "undo_steps": srv.ledger.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::path::Path;

    /// A 48 kHz mono WAV: 1 s of 440 Hz tone, 1 s of digital silence, 1 s of tone.
    fn write_wav(path: &Path) {
        write_wav_with(path, 3, |t| {
            if (1.0..2.0).contains(&t) {
                0.0
            } else {
                0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            }
        });
    }

    /// A 48 kHz mono WAV of `secs` seconds whose sample at time `t` is `f(t)`.
    fn write_wav_with(path: &Path, secs: u32, f: impl Fn(f64) -> f64) {
        let rate: u32 = 48_000;
        let mut data: Vec<u8> = Vec::new();
        for i in 0..(secs * rate) {
            let v = f(f64::from(i) / f64::from(rate));
            data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).expect("write test wav");
    }

    fn server_on(path: &Path) -> Server {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(path).expect("open test media");
        Server {
            session: Some(sess),
            ..Server::default()
        }
    }

    /// The generated WAV, and its directory removed when the test ends.
    struct TestWav {
        dir: PathBuf,
        path: PathBuf,
    }

    impl Drop for TestWav {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn tone_wav() -> TestWav {
        // One directory per call: the tests run in parallel and each removes its own.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("tone_gap.wav");
        write_wav(&path);
        TestWav { dir, path }
    }

    fn server_for(wav: &TestWav) -> Server {
        server_on(&wav.path)
    }

    fn fps_of(srv: &Server) -> f64 {
        parse::session_fps(srv.session.as_ref().unwrap())
    }

    fn frames_of(v: &Value) -> (u32, u32) {
        let f = |x: &Value| {
            x.as_str()
                .unwrap()
                .trim_end_matches('f')
                .parse::<u32>()
                .unwrap()
        };
        let pair = &v.as_array().unwrap()[0];
        (f(&pair[0]), f(&pair[1]))
    }

    #[test]
    fn silence_finds_the_gap() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let r = run_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["count"], 1, "{}", r.text);
        let (a, b) = frames_of(&st["regions"]);
        let (a, b) = (parse::secs(a, fps), parse::secs(b, fps));
        assert!((1.0..=1.3).contains(&a), "from {a}s");
        assert!((1.7..=2.0).contains(&b), "to {b}s");
    }

    #[test]
    fn silence_on_a_source_scans_the_file_in_source_time() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let r = run_silence(&mut srv, &json!({ "target": "src:0" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("source time"), "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["count"], 1, "{}", r.text);
        let (a, b) = frames_of(&st["regions"]);
        let (a, b) = (parse::secs(a, fps), parse::secs(b, fps));
        assert!((1.0..=1.3).contains(&a), "from {a}s");
        assert!((1.7..=2.0).contains(&b), "to {b}s");
    }

    #[test]
    fn cut_silence_refuses_a_bare_source_and_names_the_lane() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let before = srv.session.as_ref().unwrap().undo_depth();
        let r = run_cut_silence(&mut srv, &json!({ "target": "src:0" }));
        assert!(r.is_error, "{}", r.text);
        assert!(
            r.text.contains("cut_silence needs the timeline"),
            "{}",
            r.text
        );
        assert!(r.text.contains("A1"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), before);
    }

    #[test]
    fn cut_silence_shortens_the_timeline() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let after = parse::session_end(srv.session.as_ref().unwrap());
        let removed = parse::secs(before - after, fps);
        // The 1 s of silence keeps the default 0.4 s pause, the padding 0.15 s each side is never
        // cut, so about 0.53 s goes (the kept piece inside the silence rounds in to whole frames).
        assert!(
            (0.5..=0.6).contains(&removed),
            "removed {removed}s: {}",
            r.text
        );
        assert_eq!(srv.ledger.len(), 1);
    }

    #[test]
    fn cut_silence_dry_run_changes_nothing() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        // A dry run says it would cut and that nothing changed; it never says the cut was kept.
        assert!(r.text.starts_with("dry run — would cut ") && r.text.contains("; nothing changed"), "{}", r.text);
        assert!(!r.text.contains("nothing kept"), "{}", r.text);
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
        assert!(srv.ledger.is_empty());
    }

    /// At the undo cap the depth stops growing, so a depth delta would count
    /// nothing: the cut must still land one ledger step, and undo must restore
    /// the state from before it.
    #[test]
    fn cut_silence_at_the_cap_is_counted_and_undo_restores_it() {
        use crate::tools::edit::run_undo;
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        // Each gain flip moves the lane gain, so each pushes one snapshot and one ledger step.
        for i in 0..105 {
            let db = if i % 2 == 0 { -3 } else { -6 };
            let ops = json!([{ "op": "lane_gain", "lane": "A1", "db": db }]);
            let r = crate::tools::edit::run_edit(&mut srv, &json!({ "ops": ops }));
            assert!(!r.is_error, "lane_gain {i}: {}", r.text);
        }
        let lane = parse::lane("A1").unwrap();
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.undo_depth(), 100, "the cap holds the depth at 100");
        assert_eq!(srv.ledger.len(), 100);
        let end_before = parse::session_end(s);
        let gain_before = s.lane_gain_db(lane);
        let pushes_before = s.history_pushes();

        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let pushed = (s.history_pushes() - pushes_before) as usize;
        assert!(pushed >= 1, "the cut pushed a snapshot");
        let last = srv.ledger.last().unwrap();
        assert_eq!(
            last.steps, pushed,
            "the ledger counts the pushes, not the depth"
        );
        assert!(last.label.starts_with("cut_silence A1"), "{}", last.label);
        assert_ne!(
            parse::session_end(s),
            end_before,
            "the cut shortened the timeline"
        );

        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(
            parse::session_end(s),
            end_before,
            "undo restores the timeline"
        );
        assert_eq!(
            s.lane_gain_db(lane),
            gain_before,
            "undo restores the lane gain"
        );
        assert_eq!(
            srv.ledger.iter().map(|e| e.steps).sum::<usize>(),
            s.undo_depth()
        );
    }

    #[test]
    fn levels_rows_keep_every_value_apart() {
        // The tone has a second of digital silence, so its rows carry -120: a value four
        // characters wide must still stand apart from the next one.
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("-120"), "no -120 value to test with: {}", r.text);
        let mut rows = 0;
        for line in r.text.lines() {
            // A row is "{time:>6.1}s " then 5-wide values; the header and summary lines do not parse.
            let Some((time, rest)) = line.split_once("s ") else { continue };
            if time.trim().parse::<f64>().is_err() {
                continue;
            }
            rows += 1;
            let chars: Vec<char> = rest.chars().collect();
            assert_eq!(chars.len() % 5, 0, "row not in 5-wide columns: {line:?}");
            for cell in chars.chunks(5) {
                let cell: String = cell.iter().collect();
                assert!(cell.starts_with(' ') || cell.starts_with('-'), "{line:?}");
                assert!(cell.trim().parse::<i32>().is_ok(), "bad cell {cell:?} in {line:?}");
            }
        }
        assert!(rows > 0, "no rows read: {}", r.text);
    }

    #[test]
    fn levels_reads_loud_and_silent() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("levels dBFS, 2/s"), "{}", r.text);
        let vals: Vec<f64> = r.structured.unwrap()["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        assert!(vals.iter().any(|&v| v > -20.0), "no loud bucket: {vals:?}");
        assert!(
            vals.iter().any(|&v| v <= -100.0),
            "no silent bucket: {vals:?}"
        );
    }

    /// The loudest bucket of a `levels` reply.
    fn loudest(r: &Reply) -> f64 {
        r.structured.as_ref().unwrap()["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .fold(f64::MIN, f64::max)
    }

    #[test]
    fn levels_summary_names_its_window_and_takes_the_power_mean() {
        // Half a second of digital silence, then a 0.3 sine (about -13.5 dBFS RMS). The extremes are
        // single 20 ms windows and say so; the mean is the power mean, near -14.7 dBFS, not the
        // average of the dB values (about -43).
        let wav = wav_of(2, |t| if t < 0.5 { 0.0 } else { 0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() });
        let mut srv = server_for(&wav);
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("quietest 20 ms window -120 dBFS at 0.00s"), "{}", r.text);
        assert!(r.text.contains("loudest 20 ms window -13 dBFS") || r.text.contains("loudest 20 ms window -14 dBFS"), "{}", r.text);
        assert!(r.text.contains("(power mean of the 20 ms windows)"), "{}", r.text);
        assert!(r.text.contains("(power mean of each group of 20 ms windows)"), "{}", r.text);
        let mean: f64 = r
            .text
            .split(" · mean ")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|t| t.parse().ok())
            .unwrap_or_else(|| panic!("no mean figure: {}", r.text));
        assert!((-16.0..-13.0).contains(&mean), "power mean of the windows, got {mean}: {}", r.text);
    }

    #[test]
    fn levels_read_the_volume_envelope() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let before = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!before.is_error, "{}", before.text);
        let set = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "gain_keys", "clip": "A1#0", "keys": [["0s", -20]]}]}),
        );
        assert!(!set.is_error, "{}", set.text);
        // A constant -20 dB envelope lowers every window of the tone by 20 dB.
        let after = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!after.is_error, "{}", after.text);
        let drop = loudest(&before) - loudest(&after);
        assert!((drop - 20.0).abs() < 0.5, "drop {drop} dB, expected 20");
    }

    #[test]
    fn levels_refuses_a_bad_target() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_levels(&mut srv, &json!({ "target": "V9" }));
        assert!(r.is_error);
        let r = run_levels(&mut srv, &json!({}));
        assert!(r.is_error);
    }

    #[test]
    fn levels_says_a_muted_lane_is_silent_in_the_mix() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let set = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "lane_flags", "lane": "A1", "muted": true}]}),
        );
        assert!(!set.is_error, "{}", set.text);
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains(
                "A1 is muted: it is silent in the mix and export; the level shown is the lane's own sound"
            ),
            "{}",
            r.text
        );
        assert!(loudest(&r) > -20.0, "the lane's own sound is still read");
        // Unmuted, the same lane says nothing of the kind.
        let set = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "lane_flags", "lane": "A1", "muted": false}]}),
        );
        assert!(!set.is_error, "{}", set.text);
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 2 }));
        assert!(!r.text.contains("is muted"), "{}", r.text);
    }

    /// The test WAV on A1 at 0 s (the session's own clip), and again at 5 s: a 2 s
    /// gap between the two clips.
    fn gapped_lane(wav: &TestWav) -> Server {
        let mut srv = server_for(wav);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "at": "5s"}]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        srv
    }

    #[test]
    fn silence_on_a_lane_leaves_the_gap_between_clips_alone() {
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        let fps = fps_of(&srv);
        let r = run_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        // The quiet second inside the first clip and the quiet inside the second. The 2 s gap
        // (3 s to 5 s) is structure, not silence: it is listed nowhere and named in the reply.
        assert_eq!(st["count"], 2, "{}", r.text);
        for region in st["regions"].as_array().unwrap() {
            let (a, b) = frames_of(&json!([region.clone()]));
            let (a, b) = (parse::secs(a, fps), parse::secs(b, fps));
            assert!(b <= 3.0 || a >= 5.0, "region {a}–{b}s lies in the gap: {}", r.text);
        }
        assert!(r.text.contains("1 timeline gap on A1 left alone"), "{}", r.text);
    }

    #[test]
    fn a_lead_in_before_the_first_clip_is_never_silence_to_cut() {
        // A 3 s lead-in (a title's time) before the only clip on A1: silence reports the clip's
        // quiet and no lead-in, and cut_silence leaves the lead-in where it is.
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "delete", "clip": "A1#0"},
                {"op": "place", "source": 0, "at": "3s"},
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        let fps = fps_of(&srv);
        let lane = parse::lane("A1").unwrap();
        let r = run_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["count"], 1, "{}", r.text);
        let (a, _) = frames_of(&st["regions"]);
        assert!(parse::secs(a, fps) >= 4.0, "region starts in the lead-in: {}", r.text);
        assert!(r.text.contains("1 timeline gap on A1 left alone"), "{}", r.text);
        let cut = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!cut.is_error, "{}", cut.text);
        let first = srv.session.as_ref().unwrap().lane_clips(lane)[0].start;
        assert_eq!(first, (3.0 * fps).round() as u32, "the lead-in moved: {}", cut.text);
    }

    #[test]
    fn cut_silence_tightens_the_quiet_in_clips_and_leaves_the_gap() {
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        let fps = fps_of(&srv);
        let r = run_cut_silence(
            &mut srv,
            &json!({ "target": "A1", "min_silence": 0.5, "padding": 0.2 }),
        );
        assert!(!r.is_error, "{}", r.text);
        // Each 1 s quiet inside a clip keeps 0.4 s and loses 0.6 s, 1.2 s in all. The 2 s gap is
        // not silence, so it is not cut.
        let removed = r.structured.as_ref().unwrap()["removed_frames"]
            .as_u64()
            .unwrap() as u32;
        let removed = parse::secs(removed, fps);
        assert!(
            (removed - 1.2).abs() < 0.05,
            "removed {removed} s: {}",
            r.text
        );
        assert!(r.text.contains("1 timeline gap on A1 left alone"), "{}", r.text);
    }

    /// 440 Hz bursts: 1 s of tone, then 1.2 s of digital silence, repeated, 13 s in all. The
    /// last silence is 1 s, and ends the file.
    fn bursts_wav() -> TestWav {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-bursts-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bursts.wav");
        write_wav_with(&path, 13, |t| {
            if t % 2.2 < 1.0 {
                0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            } else {
                0.0
            }
        });
        TestWav { dir, path }
    }

    /// The bursts on A2 and A3, A1 emptied: the mix is silent only where both lanes are.
    fn bursts_on_two_lanes(wav: &TestWav) -> Server {
        let mut srv = server_for(wav);
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "delete", "clip": "A1#0"},
                {"op": "lane_add", "kind": "audio"},
                {"op": "lane_add", "kind": "audio"},
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "place", "source": 0, "lane": "A2", "at": "0s"},
                {"op": "place", "source": 0, "lane": "A3", "at": "0s"},
            ]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        srv
    }

    #[test]
    fn cut_silence_on_the_mix_keeps_each_pause_and_measures_it() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        let fps = fps_of(&srv);
        let end_before = parse::session_end(srv.session.as_ref().unwrap());
        let dry = run_cut_silence(
            &mut srv,
            &json!({ "target": "mix", "keep": 0.25, "dry_run": true }),
        );
        assert!(!dry.is_error, "{}", dry.text);
        assert_eq!(
            parse::session_end(srv.session.as_ref().unwrap()),
            end_before,
            "a dry run keeps nothing"
        );
        let r = run_cut_silence(&mut srv, &json!({ "target": "mix", "keep": 0.25 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("not measured"), "{}", r.text);
        let st = r.structured.clone().expect("structured");
        let pauses: Vec<f64> = st["pauses_measured_s"]
            .as_array()
            .expect("measured pauses")
            .iter()
            .map(|p| p.as_f64().unwrap())
            .collect();
        assert_eq!(pauses.len(), 6, "{}", r.text);
        for p in &pauses {
            assert!(*p >= 0.25 - 1e-9, "pause {p} s is under keep: {}", r.text);
        }
        // The timeline loses exactly what each silence gave up: 5 gaps of 1.2 s and a last one of 1 s.
        let removed = parse::secs(st["removed_frames"].as_u64().unwrap() as u32, fps);
        let gaps = 5.0 * 1.2 + 1.0;
        let expected: f64 = gaps - pauses.iter().sum::<f64>();
        assert!(
            (removed - expected).abs() < 0.25,
            "removed {removed} s, expected {expected} s from pauses {pauses:?}: {}",
            r.text
        );
        // The dry run reported the same amount the real cut removed.
        let d = dry.structured.as_ref().unwrap();
        assert_eq!(d["removed_frames"], st["removed_frames"], "{}", dry.text);
        let removed_dry = parse::secs(d["removed_frames"].as_u64().unwrap() as u32, fps);
        assert!(
            dry.text.contains(&format!("would remove {removed_dry:.3} s")),
            "{}",
            dry.text
        );
    }

    #[test]
    fn cut_silence_leaves_the_pause_it_names_measured_from_the_result() {
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        let r = run_cut_silence(
            &mut srv,
            &json!({ "target": "A1", "min_silence": 0.5, "keep": 0.4 }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("measured after the cut"), "{}", r.text);
        // Each measured joint sits in a quiet run of keep seconds, to the window's 20 ms.
        let pauses = r.structured.as_ref().unwrap()["pauses_measured_s"]
            .as_array()
            .expect("measured pauses")
            .clone();
        assert!(!pauses.is_empty(), "{}", r.text);
        for p in pauses {
            let p = p.as_f64().unwrap();
            // keep 0.4 with the rounding inward of two frames and the 20 ms windows that read it.
            assert!((p - 0.4).abs() < 0.1, "pause {p} s, want 0.4: {}", r.text);
        }
    }

    #[test]
    fn joint_cut_counts_a_lane_past_its_last_clip_as_quiet() {
        // A3 ends at 3 s, so after that A3 has no clip: its time there is silent, as a gap is. Under
        // A2's bursts (sound 0-1, 2.2-3.2, 4.4-5.4 s ...) the pauses after 3 s are joint silence too.
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        let trimmed = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "trim", "clip": "A3#0", "edge": "out", "to": "3s"}]}));
        assert!(!trimmed.is_error, "{}", trimmed.text);
        let r = run_cut_silence(&mut srv, &json!({"lanes": ["A2", "A3"], "dry_run": true}));
        assert!(!r.is_error, "{}", r.text);
        let count = r.structured.as_ref().unwrap()["count"].as_u64().unwrap();
        assert!(count >= 3, "pauses past A3's last clip are silent on both lanes: {} ({count})", r.text);
    }

    /// 1 s of tone, then 2 s of digital silence to the end: the clip's quiet runs to its edge.
    fn tail_wav() -> TestWav {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-tail-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("tail.wav");
        write_wav_with(&path, 3, |t| if t < 1.0 { 0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() } else { 0.0 });
        TestWav { dir, path }
    }

    #[test]
    fn cut_silence_measures_the_kept_pause_inside_its_clip_not_the_gap_after_it() {
        // The tail on A1 at 0, and a 1 s tone copy at 6 s: the 3 s gap between the clips is not
        // silence, so the pause the cut leaves is the 0.4 s kept inside the first clip.
        // The session opens with the source on A1 at 0; the second copy goes at 6 s.
        let wav = tail_wav();
        let mut srv = server_for(&wav);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "at": "6s", "in": "0s", "out": "1s"}]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let pauses = r.structured.as_ref().unwrap()["pauses_measured_s"]
            .as_array()
            .expect("measured pauses")
            .clone();
        assert_eq!(pauses.len(), 1, "{}", r.text);
        let p = pauses[0].as_f64().unwrap();
        // The keep (0.4 s, padding included) is the whole pause left after the speech, the 0.15 s
        // padding at the clip's end among it. The 2 s gap after the clip is not in it.
        assert!((p - 0.4).abs() < 0.05, "pause {p} s, want 0.40 s: {}", r.text);
        assert!(p < 1.0, "the gap after the clip is counted: {}", r.text);
    }

    #[test]
    fn cut_silence_dry_run_leaves_a_saved_project_saved() {
        let wav = tail_wav();
        let mut srv = server_for(&wav);
        let s = srv.session.as_ref().unwrap();
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
        assert!(!crate::tools::project::dirty(&srv));
        // The tail is 2 s of the 3 s clip, over half the sound, so both the dry run and the real cut are forced.
        let dry = run_cut_silence(&mut srv, &json!({ "target": "A1", "dry_run": true, "force": true }));
        assert!(!dry.is_error, "{}", dry.text);
        assert!(dry.text.starts_with("dry run"), "{}", dry.text);
        assert!(!crate::tools::project::dirty(&srv), "a dry run leaves the saved project saved");
        assert!(srv.ledger.is_empty(), "and keeps no undo step");
        // A real cut is a change, so the project is unsaved after it.
        let cut = run_cut_silence(&mut srv, &json!({ "target": "A1", "force": true }));
        assert!(!cut.is_error, "{}", cut.text);
        assert!(crate::tools::project::dirty(&srv), "a real cut is unsaved");
    }

    #[test]
    fn silence_judges_a_pause_across_touching_clips_as_one() {
        // The tail cut to keep 0.4 s, which leaves its pause split across two clips that touch.
        let wav = tail_wav();
        let mut srv = server_for(&wav);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "at": "6s", "in": "0s", "out": "1s"}]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        let fps = fps_of(&srv);
        let cut = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!cut.is_error, "{}", cut.text);
        // The silence now reads the pause as one run: the seam is no gap.
        // The pause left is keep, 0.4 s, under the default 0.5 s minimum, so it is listed at 0.3 s.
        let r = run_silence(&mut srv, &json!({ "target": "A1", "min_silence": 0.3 }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["count"], 1, "the seam split the pause: {}", r.text);
        let (a, b) = frames_of(&st["regions"]);
        assert!(parse::secs(a, fps) >= 1.0 && parse::secs(b, fps) <= 1.7, "region {a}–{b}f: {}", r.text);
        // And the cut leaves that pause where it is: a second cut finds nothing to take.
        let again = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!again.is_error, "{}", again.text);
        assert_eq!(again.structured.as_ref().unwrap()["removed_frames"], 0, "{}", again.text);
    }

    #[test]
    fn silence_never_joins_quiet_across_a_gap_between_clips() {
        // Two copies of the tail, 1 s apart: each clip's own quiet ends at its edge, and the gap
        // between them is not sound, so the two pauses stay two.
        let wav = tail_wav();
        let mut srv = server_for(&wav);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "at": "4s"}]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        let r = run_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["count"], 2, "{}", r.text);
        assert!(r.text.contains("1 timeline gap on A1 left alone"), "{}", r.text);
    }

    #[test]
    fn levels_and_silence_accept_the_mix() {
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        let r = run_levels(&mut srv, &json!({ "target": "mix", "per_sec": 2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("mix levels dBFS"), "{}", r.text);
        let r = run_silence(&mut srv, &json!({ "target": "MIX" }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["count"], 3, "{}", r.text);
    }

    fn tone_asset() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_tone.wav")
    }

    fn integrated_of(r: &Reply) -> f64 {
        r.structured.as_ref().expect("structured")["integrated_lufs"]
            .as_f64()
            .unwrap_or(f64::NAN)
    }

    #[test]
    fn loudness_reads_the_tone_on_a_lane() {
        let mut srv = server_on(&tone_asset());
        let r = run_loudness(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(integrated_of(&r).is_finite(), "{}", r.text);
        assert!(r.text.contains("integrated"), "{}", r.text);
    }

    #[test]
    fn loudness_says_when_a_lane_includes_its_duck() {
        let tone = tone_asset();
        let mut srv = server_on(&tone);
        let s = srv.session.as_mut().expect("session");
        let a2 = s.add_lane(LaneKind::Audio);
        s.place_stream_at(0.0, Path::new(&tone), 0, Some(a2)).expect("place the tone on A2");
        let plain = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!(!plain.is_error && !plain.text.contains("duck"), "{}", plain.text);
        let s = srv.session.as_mut().expect("session");
        let duck = engine::duck::Duck {
            under: 0,
            depth_db: -12.0,
            threshold_db: -40.0,
            attack_ms: 80.0,
            release_ms: 400.0,
        };
        assert!(s.set_lane_duck(a2, Some(duck)), "the duck is set");
        let ducked = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!(
            !ducked.is_error && ducked.text.contains("includes the duck under A1 (-12.0 dB at most)"),
            "{}",
            ducked.text
        );
    }

    #[test]
    fn a_lane_gain_moves_the_mix_by_its_power_share() {
        // Two mono lanes at about equal loudness: their powers add, so the mix sits 3 LU above
        // either lane alone. At -40 dB A2 keeps a ten-thousandth of its power, so the mix falls to
        // A1's share: about 3 LU lower than at 0 dB.
        let tau = std::f64::consts::PI * 2.0;
        let mut wav = tone_wav();
        wav.path = wav.dir.join("low.wav");
        write_wav_with(&wav.path, 3, |t| 0.5 * (tau * 440.0 * t).sin());
        let high = wav.dir.join("high.wav");
        write_wav_with(&high, 3, |t| 0.5 * (tau * 1000.0 * t).sin());
        let mut srv = server_on(&wav.path);
        let import = crate::server::tools().into_iter().find(|t| t.name == "media_import").expect("tool");
        let made = (import.run)(&mut srv, &json!({ "paths": [high.display().to_string()] }));
        assert!(!made.is_error, "{}", made.text);
        let placed = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "lane_add", "kind": "audio"},
                {"op": "place", "source": 1, "lane": "A2", "at": "0s"},
            ]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        let a2 = Lane::new(LaneKind::Audio, 1);
        let lane_db = |srv: &mut Server, lane: &str| {
            integrated_of(&run_loudness(srv, &json!({ "target": lane })))
        };
        let (one, other) = (lane_db(&mut srv, "A1"), lane_db(&mut srv, "A2"));
        // Both lanes are mono, so the mix is mono too: it is the two powers added.
        let power_sum = |a: f64, b: f64| 10.0 * (10f64.powf(a / 10.0) + 10f64.powf(b / 10.0)).log10();
        let both = integrated_of(&run_loudness(&mut srv, &json!({ "target": "mix" })));
        assert!((both - power_sum(one, other)).abs() < 0.2, "mix {both}, lanes {one} and {other}");
        let s = srv.session.as_mut().expect("session");
        assert!(s.set_lane_gain_db(a2, -40.0), "the gain is set");
        let quiet = integrated_of(&run_loudness(&mut srv, &json!({ "target": "mix" })));
        let expected = power_sum(one, other - 40.0);
        assert!((quiet - expected).abs() < 0.2, "mix {quiet} at -40 dB, expected {expected}");
        // The 440 Hz and 1 kHz tones are near enough in loudness that the drop is about 3 LU.
        let drop = both - quiet;
        assert!((drop - 3.0).abs() < 0.5, "drop {drop} LU");
    }

    #[test]
    fn cut_silence_dry_run_keeps_the_undo_and_redo_history() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let cut = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!cut.is_error, "{}", cut.text);
        let undone = crate::tools::edit::run_undo(&mut srv, &json!({}));
        assert!(!undone.is_error, "{}", undone.text);
        let s = srv.session.as_ref().unwrap();
        let (undo, redo) = (s.undo_depth(), s.redo_depth());
        assert!(redo > 0, "the undone cut is on the redo stack");
        let ledger = srv.redo_ledger.len();
        assert_eq!(ledger, 1);
        let dirty = crate::tools::project::dirty(&srv);

        let dry = run_cut_silence(&mut srv, &json!({ "target": "A1", "dry_run": true }));
        assert!(!dry.is_error, "{}", dry.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!((s.undo_depth(), s.redo_depth()), (undo, redo), "{}", dry.text);
        assert_eq!(srv.redo_ledger.len(), ledger, "{}", dry.text);
        assert_eq!(crate::tools::project::dirty(&srv), dirty, "{}", dry.text);
    }

    /// The limiter is in the way with allow_limiter when the linear prediction says the peaks bind, or
    /// when the mix already sits over the ceiling (the clipped case the prediction gets wrong). Without
    /// allow_limiter it is never in the way, and a quiet mix the peaks do not touch leaves it out.
    #[test]
    fn allow_limiter_engages_for_a_clipped_mix_the_prediction_misses() {
        // Clipped: +1.6 dBTP against a -1 ceiling, the linear gain says the peaks are fine.
        assert!(limiter_in_the_way(true, false, 1.6, -1.0));
        // The linear prediction binds.
        assert!(limiter_in_the_way(true, true, -6.1, -1.0));
        // Quiet, and the peaks do not bind: no limiter.
        assert!(!limiter_in_the_way(true, false, -6.1, -1.0));
        // Not allowed: never.
        assert!(!limiter_in_the_way(false, true, 1.6, -1.0));
    }

    #[test]
    fn normalize_mix_lands_on_the_target() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -20 }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "one undo step: {}", r.text);
        assert!(r.text.ends_with("undo step 1"), "{}", r.text);
        let m = run_loudness(&mut srv, &json!({ "target": "mix" }));
        assert!(!m.is_error, "{}", m.text);
        assert!(
            (integrated_of(&m) - -20.0).abs() <= 1.0,
            "mix at {} LUFS: {}",
            integrated_of(&m),
            m.text
        );
    }

    #[test]
    fn normalize_lane_lands_on_the_target() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -20 }));
        assert!(!r.is_error, "{}", r.text);
        let m = run_loudness(&mut srv, &json!({ "target": "A1" }));
        assert!(!m.is_error, "{}", m.text);
        assert!(
            (integrated_of(&m) - -20.0).abs() <= 1.0,
            "A1 at {} LUFS: {}",
            integrated_of(&m),
            m.text
        );
    }

    /// The tone on A1 and on A2 (a lane of its own at -6 dB), two speakers at different levels.
    fn two_lanes_apart() -> Server {
        let tone = tone_asset();
        let mut srv = server_on(&tone);
        let s = srv.session.as_mut().expect("session");
        let a2 = s.add_lane(LaneKind::Audio);
        s.place_stream_at(0.0, Path::new(&tone), 0, Some(a2)).expect("place the tone on A2");
        assert!(s.set_lane_gain_db(a2, -6.0));
        srv
    }

    #[test]
    fn normalize_each_sets_every_lane_alone_to_the_target() {
        let mut srv = two_lanes_apart();
        let r = run_normalize(&mut srv, &json!({ "each": true, "lanes": ["A1", "A2"], "lufs": -20 }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "one undo step: {}", r.text);
        assert!(r.text.ends_with("undo step 1"), "{}", r.text);
        assert!(r.text.contains("A1 ") && r.text.contains("A2 "), "{}", r.text);
        let a1 = lufs_at(&mut srv, "A1");
        let a2 = lufs_at(&mut srv, "A2");
        assert!((a1 - -20.0).abs() <= 1.0, "A1 at {a1} LUFS: {}", r.text);
        assert!((a2 - -20.0).abs() <= 1.0, "A2 at {a2} LUFS: {}", r.text);
        // The same tone on both lanes: A2 started 6 dB under A1 (-6 against 0), but each lane is set
        // by its own measurement, so both land on the same gain.
        let s = srv.session.as_ref().expect("session");
        let g1 = f64::from(s.lane_gain_db(parse::lane("A1").unwrap()));
        let g2 = f64::from(s.lane_gain_db(parse::lane("A2").unwrap()));
        assert!((g2 - g1).abs() <= 0.5, "gains {g1} and {g2}: {}", r.text);
        let st = r.structured.as_ref().expect("structured");
        assert_eq!(st["lanes"].as_array().expect("lanes").len(), 2);
        assert_eq!(st["reached"], true, "{}", r.text);
    }

    #[test]
    fn normalize_each_undo_restores_both_lanes_in_one_step() {
        let mut srv = two_lanes_apart();
        let s = srv.session.as_ref().expect("session");
        let (before1, before2) = (
            s.lane_gain_db(parse::lane("A1").unwrap()),
            s.lane_gain_db(parse::lane("A2").unwrap()),
        );
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = run_normalize(&mut srv, &json!({ "each": true, "lanes": ["A1", "A2"], "lufs": -20 }));
        assert!(!r.is_error, "{}", r.text);
        // One normalize records several engine snapshots; the one ledger entry covers all of them.
        assert_eq!(srv.ledger.len(), 1, "{}", r.text);
        let s = srv.session.as_mut().expect("session");
        while s.undo_depth() > depth && s.undo() {}
        let s = srv.session.as_ref().expect("session");
        assert_eq!(s.lane_gain_db(parse::lane("A1").unwrap()), before1);
        assert_eq!(s.lane_gain_db(parse::lane("A2").unwrap()), before2);
    }

    #[test]
    fn normalize_each_dry_run_predicts_each_lane_and_changes_nothing() {
        let mut srv = two_lanes_apart();
        let g2 = srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A2").unwrap());
        let r = run_normalize(&mut srv, &json!({ "each": true, "lanes": ["A1", "A2"], "lufs": -20, "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("target reached: dry run — would "), "{}", r.text);
        assert!(r.text.contains("· nothing changed"), "{}", r.text);
        let st = r.structured.as_ref().expect("structured");
        assert_eq!(st["changed"], false);
        assert_eq!(st["lanes"].as_array().expect("lanes").len(), 2);
        assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A2").unwrap()), g2);
        assert_eq!(srv.ledger.len(), 0);
    }

    #[test]
    fn normalize_each_refuses_bad_arguments_before_changing_anything() {
        let mut srv = two_lanes_apart();
        let g1 = srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A1").unwrap());
        for (args, want) in [
            (json!({ "each": true, "lufs": -20 }), "each needs `lanes`"),
            (json!({ "each": true, "lanes": ["A1"], "target": "A2" }), "each takes `lanes`, not `target`"),
            (json!({ "each": true, "lanes": ["A1", "A1"] }), "listed twice"),
            (json!({ "each": true, "lanes": ["V1"] }), "audio lanes"),
            (json!({ "each": true, "lanes": ["A1"], "allow_limiter": true }), "allow_limiter goes with target mix"),
            (json!({ "lanes": ["A1", "A2"] }), "lanes goes with each:true"),
        ] {
            let r = run_normalize(&mut srv, &args);
            assert!(r.is_error, "{args}: {}", r.text);
            assert!(r.text.contains(want), "wanted {want:?} in {}", r.text);
        }
        assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A1").unwrap()), g1);
        assert_eq!(srv.ledger.len(), 0);
    }

    fn lufs_at(srv: &mut Server, target: &str) -> f64 {
        let r = run_loudness(srv, &json!({ "target": target }));
        assert!(!r.is_error, "{}", r.text);
        integrated_of(&r)
    }

    #[test]
    fn clip_gain_lowers_the_clip_loudness_by_the_same_amount() {
        let mut srv = server_on(&tone_asset());
        let before = lufs_at(&mut srv, "A1@0");
        assert!(before.is_finite(), "tone clip is silent");
        let set = srv
            .session
            .as_mut()
            .unwrap()
            .set_clip_gain(Lane::A1, 0, -6.0);
        assert!(set, "the clip gain was not set");
        let after = lufs_at(&mut srv, "A1@0");
        assert!(
            (before - after - 6.0).abs() <= 0.3,
            "clip at -6 dB read {before} -> {after}"
        );
        // The lane reading moves with it, since the lane is that clip.
        let lane_after = lufs_at(&mut srv, "A1");
        assert!(
            (lane_after - after).abs() <= 0.3,
            "lane {lane_after} vs clip {after}"
        );
    }

    #[test]
    fn clip_gain_reaches_levels_and_silence_on_a_clip() {
        let mut srv = server_on(&tone_asset());
        let peak = |srv: &mut Server| {
            let r = run_levels(srv, &json!({ "target": "A1@0", "per_sec": 2 }));
            assert!(!r.is_error, "{}", r.text);
            r.structured.expect("structured")["values"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_f64())
                .fold(f64::MIN, f64::max)
        };
        let before = peak(&mut srv);
        srv.session
            .as_mut()
            .unwrap()
            .set_clip_gain(Lane::A1, 0, -6.0);
        let after = peak(&mut srv);
        assert!(
            (before - after - 6.0).abs() <= 0.5,
            "levels peak {before} -> {after}"
        );
        // Silence at a threshold the tone sits below only once the gain is on.
        let r = run_silence(&mut srv, &json!({ "target": "A1@0", "threshold_db": -20 }));
        assert!(!r.is_error, "{}", r.text);
    }

    #[test]
    fn normalize_names_the_true_peak_cap_and_the_limiter_fixes_it() {
        let mut srv = server_on(&tone_asset());
        // The test tone peaks at -18 dBTP, so a -15 ceiling is the one that holds.
        let args = json!({ "target": "mix", "lufs": -14, "true_peak": -15 });
        let r = run_normalize(&mut srv, &args);
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["capped_by"], "true_peak", "{}", r.text);
        assert!(r.text.starts_with("short of the target: mix raised "), "{}", r.text);
        assert!(r.text.contains("held by the true-peak ceiling -15.0 dBTP — allow_limiter:true"), "{}", r.text);
        let plain = st["achieved_lufs"].as_f64().unwrap();
        assert!(plain < -14.0, "the cap should hold it short: {plain}");
        assert!(st["limiter"].is_null());

        let mut srv = server_on(&tone_asset());
        let r = run_normalize(
            &mut srv,
            &json!({ "target": "mix", "lufs": -14, "true_peak": -15, "allow_limiter": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "one undo step: {}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["limiter"]["on"], true, "{}", r.text);
        assert!(r.text.contains("dBTP"), "the limiter is named by its true-peak ceiling: {}", r.text);
        let reached = st["achieved_lufs"].as_f64().unwrap();
        // The lane gain cap binds here, so the limiter adds no level; its walk to the ceiling costs
        // a few hundredths of a LU at most.
        assert!(reached > plain - 0.1, "limiter lost level {plain} -> {reached}");
        assert_eq!(st["after_measured"], true, "{}", r.text);
        assert!(srv.session.as_ref().unwrap().limiter().on);
    }

    /// The mix is held at or under the true_peak target with the limiter in circuit. The limiter's
    /// detector is the meter's own true-peak method, but on this tone at this gain the limiter can
    /// read a little over its ceiling (gain moves within the filter's support), so the walk may
    /// lower the ceiling below the target; the held mix is what is checked.
    #[test]
    fn the_mix_with_the_limiter_is_held_at_the_true_peak_target() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(
            &mut srv,
            &json!({ "target": "mix", "lufs": -14, "true_peak": -15, "allow_limiter": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["limiter"]["on"], true, "{}", r.text);
        let measured = st["measured_true_peak_dbtp"].as_f64().unwrap();
        assert!(measured <= -15.0, "the mix reads {measured} dBTP past the -15 ceiling: {}", r.text);
    }

    /// Three sources for three audio lanes, as the report's project had them: quiet speech-like pulses
    /// on A1, a quiet tone on A2 and clicks on A3. The mix is peaky, so its true peak, not its level, is what
    /// a gain runs into first.
    fn three_lanes_of_sound() -> (TestWav, Server) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-three-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pulses = dir.join("pulses.wav");
        write_wav_with(&pulses, 13, |t| {
            if t % 2.2 < 1.0 {
                0.05 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()
            } else {
                0.0
            }
        });
        let tone = dir.join("tone.wav");
        write_wav_with(&tone, 13, |t| 0.003 * (2.0 * std::f64::consts::PI * 300.0 * t).sin());
        let clicks = dir.join("clicks.wav");
        write_wav_with(&clicks, 13, |t| {
            if t % 1.0 < 0.02 {
                0.7 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()
            } else {
                0.0
            }
        });
        let wav = TestWav { dir, path: pulses.clone() };
        let mut srv = server_on(&pulses);
        let import = crate::server::tools().into_iter().find(|t| t.name == "media_import").expect("tool");
        let paths = json!({ "paths": [tone.to_string_lossy(), clicks.to_string_lossy()] });
        let imported = (import.run)(&mut srv, &paths);
        assert!(!imported.is_error, "{}", imported.text);
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "lane_add", "kind": "audio"},
                {"op": "lane_add", "kind": "audio"},
                {"op": "place", "source": 1, "lane": "A2", "at": "0s"},
                {"op": "place", "source": 2, "lane": "A3", "at": "0s"},
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        (wav, srv)
    }

    /// A master limiter already on at -6 dBTP, below the -1 true_peak target, is the ceiling the peaks
    /// are held to: a normalize counts on it. The gain used to be predicted against -1 dBTP and the walk
    /// stopped at the limiter's ceiling short of the target, with the reply naming no cause.
    #[test]
    fn normalize_counts_on_a_limiter_already_on_below_the_true_peak_target() {
        let (_wav, mut srv) = three_lanes_of_sound();
        let set = srv.session.as_mut().unwrap().set_limiter(Limiter { on: true, ceiling_db: -6.0 });
        assert!(set, "the limiter was not set");
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -19 }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["reached"], true, "{}", r.text);
        assert_eq!(st["capped_by"], Value::Null, "{}", r.text);
        let achieved = st["achieved_lufs"].as_f64().unwrap();
        assert!((achieved + 19.0).abs() <= 0.1, "reached {achieved}: {}", r.text);
        let measured = st["measured_true_peak_dbtp"].as_f64().unwrap();
        assert!(measured <= -6.0 + 0.05, "the limiter holds the peaks at -6: {measured}: {}", r.text);
    }

    /// A level short of the target that the master limiter's ceiling holds names that limiter.
    #[test]
    fn a_short_normalize_held_by_the_limiter_names_the_limiter() {
        let (_wav, mut srv) = three_lanes_of_sound();
        srv.session.as_mut().unwrap().set_limiter(Limiter { on: true, ceiling_db: -6.0 });
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -9, "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["reached"], false, "{}", r.text);
        assert_eq!(st["capped_by"], "limiter", "{}", r.text);
        assert!(r.text.contains("held by the master limiter at -6.0 dBTP"), "{}", r.text);
    }

    #[test]
    fn range_past_the_end_of_a_lane_is_named_in_seconds() {
        // A2 of a 3 s timeline: 5 s to 6 s is past its end, and says so.
        let err = range_arg(&json!({ "range": { "from": "5.0s", "to": "6.0s" } }), 30.0, 0, 90, "A2")
            .unwrap_err();
        assert!(err.contains("5.000s–6.000s is past the end of A2 (3.000s)"), "{err}");
        let err = range_arg(&json!({ "range": { "from": "1.0s", "to": "1.0s" } }), 30.0, 0, 90, "A2")
            .unwrap_err();
        assert!(err.starts_with("range is empty"), "{err}");
        assert_eq!(range_arg(&json!({ "range": { "from": "1.0s" } }), 30.0, 0, 90, "A2").unwrap(), (30, 90));
    }

    #[test]
    fn missing_source_names_how_many_the_project_has() {
        assert_eq!(source_count_note(0), "this project has no sources");
        assert_eq!(source_count_note(1), "this project has 1 source (src:0)");
        assert_eq!(source_count_note(3), "this project has 3 sources (src:0..src:2)");
    }

    #[test]
    fn loudness_of_a_missing_source_names_the_project_sources() {
        let mut srv = server_on(&tone_asset());
        let r = run_loudness(&mut srv, &json!({ "target": "src:1" }));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(r.text, "no source src:1: this project has 1 source (src:0)");
    }

    #[test]
    fn loudness_of_a_three_second_file_says_lra_needs_more_audio() {
        let mut srv = server_on(&tone_asset());
        let r = run_loudness(&mut srv, &json!({ "target": "src:0" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("LRA n/a"), "{}", r.text);
        assert!(r.text.contains("analysed 3.00 s of audio in "), "{}", r.text);
        let st = r.structured.expect("structured");
        assert!(st["range_lu"].is_null(), "{}", r.text);
    }

    /// A 440 Hz tone at 0.0153 for 3 s, about -40 LUFS: a recording too quiet for the lane's +12 dB cap.
    fn quiet_tone() -> TestWav {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-quiet-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("quiet_tone.wav");
        write_wav_with(&path, 3, |t| 0.0153 * (2.0 * std::f64::consts::PI * 440.0 * t).sin());
        TestWav { dir, path }
    }

    fn clip_gain_of(srv: &Server, idx: usize) -> f32 {
        srv.session.as_ref().unwrap().clip_gain_of(Lane::A1, idx).expect("clip")
    }

    #[test]
    fn normalize_takes_a_quiet_tone_past_the_lane_cap_onto_its_clip() {
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        let before = lufs_at(&mut srv, "A1");
        assert!((before - -40.0).abs() <= 1.0, "quiet tone at {before} LUFS");
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -16 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("target reached: "), "{}", r.text);
        assert!(r.text.contains("lane A1 +12.0 dB, clips A1@0 +12.0 dB clip gain"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(Lane::A1), 12.0, "{}", r.text);
        assert!((clip_gain_of(&srv, 0) - 12.0).abs() <= 0.05, "clip gain {}: {}", clip_gain_of(&srv, 0), r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["reached"], true, "{}", r.text);
        assert!(st["capped_by"].is_null(), "{}", r.text);
        assert_eq!(st["clip_gain"][0]["clip"], "A1@0", "{}", r.text);
        let after = lufs_at(&mut srv, "A1");
        assert!((after - -16.0).abs() <= 0.2, "A1 at {after} LUFS: {}", r.text);
    }

    #[test]
    fn clip_gain_boost_adds_to_the_gain_a_clip_already_has() {
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        assert!(srv.session.as_mut().unwrap().set_clip_gain(Lane::A1, 0, 3.0));
        // At +3 dB the tone is about -37 LUFS: the lane takes +12, the clip the other +9 on its +3.
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -16 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("lane A1 +12.0 dB, clips A1@0 +9.0 dB clip gain"), "{}", r.text);
        assert!((clip_gain_of(&srv, 0) - 12.0).abs() <= 0.05, "clip gain {}: {}", clip_gain_of(&srv, 0), r.text);
    }

    #[test]
    fn clip_gain_boost_undo_restores_the_lane_and_clip_gains_exactly() {
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        assert!(srv.session.as_mut().unwrap().set_clip_gain(Lane::A1, 0, 3.0));
        let (lane0, clip0) = (srv.session.as_ref().unwrap().lane_gain_db(Lane::A1), clip_gain_of(&srv, 0));
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -16 }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "one undo step: {}", r.text);
        assert!(clip_gain_of(&srv, 0) > clip0, "{}", r.text);
        let s = srv.session.as_mut().expect("session");
        while s.undo_depth() > depth && s.undo() {}
        let s = srv.session.as_ref().expect("session");
        assert_eq!(s.lane_gain_db(Lane::A1), lane0);
        assert_eq!(s.clip_gain_of(Lane::A1, 0), Some(clip0));
    }

    #[test]
    fn clip_gain_boost_dry_run_predicts_the_split_and_changes_nothing() {
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -16, "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.starts_with("target reached: dry run — would normalize A1 to -16.0 LUFS: lane A1 +12.0 dB, clips A1@0 +12.0 dB clip gain"),
            "{}",
            r.text
        );
        assert_eq!(r.structured.as_ref().unwrap()["changed"], false);
        assert_eq!(srv.ledger.len(), 0);
        assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(Lane::A1), 0.0);
        assert_eq!(clip_gain_of(&srv, 0), 0.0, "{}", r.text);
    }

    #[test]
    fn normalize_each_puts_the_boost_past_the_lane_cap_on_the_clips() {
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        let r = run_normalize(&mut srv, &json!({ "each": true, "lanes": ["A1"], "lufs": -16 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("target reached: normalized each lane"), "{}", r.text);
        assert!(r.text.contains("lane A1 +12.0 dB, clips A1@0 +12.0 dB clip gain"), "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "{}", r.text);
    }

    #[test]
    fn a_boost_past_both_caps_says_so_and_names_the_shortfall() {
        // -40 LUFS plus +36 dB (lane +12, clip +24) reaches -4; -3 is one LU past that.
        let wav = quiet_tone();
        let mut srv = server_for(&wav);
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -3, "true_peak": 0 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("short of the target: mix raised "), "{}", r.text);
        assert!(r.text.contains("lane gain +12 dB and clip gain +24 dB on each clip is the most"), "{}", r.text);
        assert!(r.text.contains("LU short"), "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["capped_by"], "clip_gain_limit", "{}", r.text);
        assert_eq!(st["reached"], false, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(Lane::A1), 12.0);
        assert!((clip_gain_of(&srv, 0) - 24.0).abs() <= 0.05, "clip gain {}", clip_gain_of(&srv, 0));
    }

    #[test]
    fn normalize_leads_with_the_outcome_and_names_the_lane_gain_cap() {
        // A loud target the lanes cannot reach: the reply says where it landed and why.
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -2 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("short of the target: mix raised "), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["capped_by"], "true_peak", "{}", r.text);
    }

    fn levels_max(r: &Reply) -> f64 {
        r.structured.as_ref().expect("structured")["values"]
            .as_array()
            .expect("values")
            .iter()
            .filter_map(Value::as_f64)
            .fold(f64::MIN, f64::max)
    }

    #[test]
    fn normalize_mix_reports_the_measured_peak() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(
            &mut srv,
            &json!({ "target": "mix", "lufs": -14, "true_peak": -15 }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("expected"), "{}", r.text);
        assert!(r.text.contains("(measured)"), "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["after_measured"], true, "{}", r.text);
        // The figure the reply gives is the one a fresh reading of the mix finds.
        let m = run_loudness(&mut srv, &json!({ "target": "mix" }));
        let peak = m.structured.expect("structured")["true_peak_dbtp"]
            .as_f64()
            .unwrap();
        let said = st["measured_true_peak_dbtp"].as_f64().unwrap();
        assert!((peak - said).abs() < 0.1, "reply {said} vs reading {peak}");
        assert!(peak <= -15.0 + 0.1, "the ceiling holds: {peak}");
    }

    #[test]
    fn normalize_mix_keeps_the_true_peak_under_the_default_ceiling() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(&mut srv, &json!({ "target": "mix", "lufs": -8 }));
        assert!(!r.is_error, "{}", r.text);
        let m = run_loudness(&mut srv, &json!({ "target": "mix" }));
        let peak = m.structured.expect("structured")["true_peak_dbtp"]
            .as_f64()
            .unwrap();
        assert!(
            peak <= -1.0 + 0.1,
            "-1 dBTP ceiling, mix reads {peak}: {}",
            r.text
        );
    }

    fn run_edit_flags(srv: &mut Server, lane: &str, locked: bool) -> Reply {
        crate::tools::edit::run_edit(srv, &json!({"ops": [{"op": "lane_flags", "lane": lane, "locked": locked}]}))
    }

    #[test]
    fn normalize_refuses_when_a_lane_it_would_change_is_locked() {
        // Single lane, each, and the mix: a locked lane among those a normalize would move refuses
        // the whole call, and nothing changes.
        for args in [
            json!({ "target": "A2", "lufs": -20 }),
            json!({ "each": true, "lanes": ["A1", "A2"], "lufs": -20 }),
            json!({ "target": "mix", "lufs": -20 }),
        ] {
            let mut srv = two_lanes_apart();
            let r = run_edit_flags(&mut srv, "A2", true);
            assert!(!r.is_error, "{}", r.text);
            let gain_before = srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A2").unwrap());
            let ledger = srv.ledger.len();
            let r = run_normalize(&mut srv, &args);
            assert!(r.is_error, "{args}: {}", r.text);
            assert!(r.text.contains("A2 is locked"), "{args}: {}", r.text);
            assert_eq!(srv.ledger.len(), ledger, "{args}: nothing changed");
            assert_eq!(srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A2").unwrap()), gain_before, "{args}");
        }
    }

    #[test]
    fn normalize_limiter_path_ends_under_the_true_peak_ceiling() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(
            &mut srv,
            &json!({ "target": "mix", "lufs": -14, "true_peak": -15, "allow_limiter": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.expect("structured");
        // The walk aims under the target: the measured peak is at or under the ceiling, not 0.1 over it.
        let peak = st["measured_true_peak_dbtp"].as_f64().unwrap();
        assert!(peak <= -15.0, "true peak {peak} over the -15 ceiling: {}", r.text);
        assert!(r.text.contains("limiter left on at") && r.text.contains("dBTP"), "{}", r.text);
    }

    #[test]
    fn normalize_counts_on_a_limiter_already_on_and_never_advises_the_flag() {
        // The true peak holds the linear gain at -15 dBTP (the tone reaches -14 LUFS only past it), so
        // without a limiter the mix stops short. With the limiter already on at the same ceiling, the
        // flag would change nothing: the mix is normalized with the limiter in circuit, and says so.
        let args = json!({ "target": "mix", "lufs": -14, "true_peak": -15, "dry_run": true });
        let mut plain = server_on(&tone_asset());
        let plain_r = run_normalize(&mut plain, &args);
        let plain_lufs = plain_r.structured.expect("structured")["achieved_lufs"].as_f64().unwrap();

        let mut srv = server_on(&tone_asset());
        srv.session
            .as_mut()
            .unwrap()
            .set_limiter(engine::limiter::Limiter { on: true, ceiling_db: -15.0 });
        let r = run_normalize(&mut srv, &args);
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("allow_limiter"), "the flag changes nothing here: {}", r.text);
        assert!(r.text.contains("counted on"), "{}", r.text);
        let st = r.structured.expect("structured");
        assert_eq!(st["limiter_counted_on"], true, "{}", r.text);
        let achieved = st["achieved_lufs"].as_f64().unwrap();
        assert!(achieved > plain_lufs + 0.5, "the limiter lets the gain on: {achieved} vs {plain_lufs}: {}", r.text);
        // A dry run takes its change back: the limiter is still on at the ceiling it was at.
        assert!(srv.session.as_ref().unwrap().limiter().on);
        assert!((srv.session.as_ref().unwrap().limiter().ceiling_db + 15.0).abs() < 1e-3);
    }

    #[test]
    fn levels_per_sec_error_says_what_the_number_is() {
        let mut srv = server_on(&tone_asset());
        let r = run_levels(&mut srv, &json!({ "target": "A1", "per_sec": 60 }));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("per_sec is buckets per second"), "{}", r.text);
        assert!(r.text.contains("e.g. 10"), "{}", r.text);
    }

    #[test]
    fn hear_schemas_show_per_sec_type_and_loudness_target_required() {
        let levels = levels_schema();
        assert_eq!(levels["properties"]["per_sec"]["type"], "number");
        assert!(levels["properties"]["per_sec"]["description"].as_str().unwrap().contains("Buckets per second"));
        let loudness = loudness_schema();
        assert_eq!(loudness["required"], json!(["target"]));
        assert!(loudness["properties"]["target"]["description"].as_str().unwrap().starts_with("Required."));
        assert!(LOUDNESS_DOC.contains("target is required"));
    }

    #[test]
    fn allow_limiter_on_a_lane_says_why_and_what_to_run() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(
            &mut srv,
            &json!({ "target": "A1", "lufs": -14, "allow_limiter": true }),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("mix-wide"), "{}", r.text);
        assert!(r.text.contains("normalize A1 without it"), "{}", r.text);
        assert!(r.text.contains("allow_limiter:true"), "{}", r.text);
        assert_eq!(srv.ledger.len(), 0);
    }

    #[test]
    fn a_ducked_lane_reads_with_its_duck() {
        let mut srv = server_on(&tone_asset());
        let a2 = {
            let s = srv.session.as_mut().expect("session");
            let a2 = s.add_lane(LaneKind::Audio);
            let clip = s.lane_clips(Lane::A1)[0].clone();
            assert!(s.place_at(a2, 0.0, clip), "place a copy of the tone on A2");
            a2
        };
        let plain = run_loudness(&mut srv, &json!({ "target": "A2" }));
        let plain_levels = run_levels(&mut srv, &json!({ "target": "A2", "per_sec": 10 }));
        assert!(!plain.is_error && !plain_levels.is_error, "{}", plain.text);

        // A1 is the key: the tone is well above -60 dBFS, so A2 drops by 20 dB.
        let duck = engine::duck::Duck {
            threshold_db: -60.0,
            ..engine::duck::Duck::new(0, -20.0)
        };
        assert!(srv.session.as_mut().unwrap().set_lane_duck(a2, Some(duck)));
        let ducked = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!(!ducked.is_error, "{}", ducked.text);
        assert!(
            integrated_of(&ducked) <= integrated_of(&plain) - 10.0,
            "ducked {} vs plain {}",
            integrated_of(&ducked),
            integrated_of(&plain)
        );
        let ducked_levels = run_levels(&mut srv, &json!({ "target": "A2", "per_sec": 10 }));
        assert!(!ducked_levels.is_error, "{}", ducked_levels.text);
        assert!(
            levels_max(&ducked_levels) <= levels_max(&plain_levels) - 10.0,
            "levels {} vs {}",
            levels_max(&ducked_levels),
            levels_max(&plain_levels)
        );
        assert!(
            ducked_levels.text.contains("duck under A1"),
            "{}",
            ducked_levels.text
        );

        // Taken off again, the lane reads as it did.
        assert!(srv.session.as_mut().unwrap().set_lane_duck(a2, None));
        let off = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!((integrated_of(&off) - integrated_of(&plain)).abs() < 0.1);
    }

    #[test]
    fn a_lane_ducked_under_two_keys_reads_with_both_ducks() {
        let mut srv = server_on(&tone_asset());
        let a2 = {
            let s = srv.session.as_mut().expect("session");
            let a2 = s.add_lane(LaneKind::Audio);
            let a3 = s.add_lane(LaneKind::Audio);
            let clip = s.lane_clips(Lane::A1)[0].clone();
            assert!(s.place_at(a2, 0.0, clip.clone()), "the ducked tone on A2");
            assert!(s.place_at(a3, 0.0, clip), "a second key on A3");
            a2
        };
        let plain_levels = run_levels(&mut srv, &json!({ "target": "A2", "per_sec": 10 }));
        assert!(!plain_levels.is_error, "{}", plain_levels.text);
        let plain_levels_lufs = run_loudness(&mut srv, &json!({ "target": "A2" }));
        let s = srv.session.as_mut().unwrap();
        // A1 and A3 are both loud tones: A2 drops by 20 dB under A1 and 6 dB under A3.
        let under_a1 = engine::duck::Duck {
            threshold_db: -60.0,
            ..engine::duck::Duck::new(0, -20.0)
        };
        let under_a3 = engine::duck::Duck {
            threshold_db: -60.0,
            ..engine::duck::Duck::new(2, -6.0)
        };
        assert!(s.set_lane_duck(a2, Some(under_a1)));
        assert!(s.set_lane_duck(a2, Some(under_a3)));
        assert_eq!(s.lane_duck_list(a2).len(), 2, "both ducks stand");

        let both = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!(!both.is_error, "{}", both.text);
        assert!(
            both.text.contains("ducks under A1 (-20.0 dB at most) and A3 (-6.0 dB at most)"),
            "{}",
            both.text
        );
        assert!(
            integrated_of(&both) <= integrated_of(&plain_levels_lufs) - 20.0,
            "both ducks: {} vs plain {}",
            integrated_of(&both),
            integrated_of(&plain_levels_lufs)
        );

        // Off for A3 alone leaves A1's duck, and the reading comes back up to it.
        assert!(srv.session.as_mut().unwrap().clear_lane_duck(a2, 2));
        let one = run_loudness(&mut srv, &json!({ "target": "A2" }));
        assert!(!one.is_error, "{}", one.text);
        assert!(one.text.contains("duck under A1 (-20.0 dB at most)"), "{}", one.text);
        let gap = integrated_of(&one) - integrated_of(&plain_levels_lufs);
        assert!((-22.0..=-18.0).contains(&gap), "only A1's duck: {gap} dB");
    }

    /// The 1 s silence between two tones, cut with the given settings; returns the seconds removed.
    fn removed_by_cut(args: Value) -> f64 {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let r = run_cut_silence(&mut srv, &args);
        assert!(!r.is_error, "{}", r.text);
        let after = parse::session_end(srv.session.as_ref().unwrap());
        parse::secs(before - after, fps)
    }

    #[test]
    fn cut_silence_keeps_a_beat_of_each_pause_by_default() {
        // A 1 s pause keeps 0.4 s by default, so about 0.53 s goes.
        let default = removed_by_cut(json!({ "target": "A1" }));
        assert!((0.5..=0.6).contains(&default), "default removed {default}s");
        let kept = removed_by_cut(json!({ "target": "A1", "keep": 0.7 }));
        assert!((0.25..=0.35).contains(&kept), "keep 0.7 removed {kept}s");
    }

    #[test]
    fn cut_silence_refuses_keep_under_a_twentieth_of_a_second() {
        let mut srv = server_for(&tone_wav());
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0 }));
        assert!(r.is_error && r.text.contains("cut_regions"), "{}", r.text);
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before);
    }

    /// The refusal names each span as start to end, never end before start: spans are start and
    /// length frames, so the end is their sum.
    #[test]
    fn cut_spans_are_named_start_to_end() {
        assert_eq!(super::span_list(&[(10, 5), (40, 12)], 25.0), "0.400–0.600s, 1.600–2.080s");
    }

    /// Every span a refused cut names runs forward: its start comes before its end.
    #[test]
    fn a_refused_cut_names_each_span_start_to_end() {
        let wav = beeps_wav();
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05 }));
        assert!(r.is_error, "{}", r.text);
        let list = r.text.split("Cut spans: ").nth(1).and_then(|t| t.split(". Nothing changed").next()).unwrap();
        let spans: Vec<&str> = list.split(", ").collect();
        assert!(spans.len() >= 2, "{}", r.text);
        for span in spans {
            let (a, b) = span.trim_end_matches('s').split_once('–').unwrap();
            let (a, b): (f64, f64) = (a.parse().unwrap(), b.parse().unwrap());
            assert!(a < b, "span {span} runs backwards: {}", r.text);
        }
    }

    /// Four 50 ms tone bursts, one per second: the quiet between them is 0.95 s.
    fn beeps_wav() -> TestWav {
        let wav = tone_wav();
        write_wav_with(&wav.path, 4, |t| {
            if t % 1.0 < 0.05 {
                0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            } else {
                0.0
            }
        });
        wav
    }

    #[test]
    fn cut_silence_over_half_the_sound_is_refused_and_changes_nothing() {
        let wav = beeps_wav();
        let mut srv = server_for(&wav);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        // Keep 0.05 cuts 4 x 0.6 s of a 4 s take: 60 %, over the half a cut may take.
        for dry in [true, false] {
            let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05, "dry_run": dry }));
            assert!(r.is_error && r.text.contains("force:true"), "dry {dry}: {}", r.text);
            assert!(r.text.contains("Cut spans"), "{}", r.text);
            assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before, "dry {dry}");
        }
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05, "force": true }));
        assert!(!r.is_error, "{}", r.text);
    }

    #[test]
    fn cut_silence_keeps_every_burst_of_speech_at_a_short_keep() {
        let wav = beeps_wav();
        let mut srv = server_for(&wav);
        let bursts = loud_windows(&srv);
        // min_keep would join each 50 ms burst to the silences either side and cut it too; a cut
        // keeps every burst whole.
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05, "force": true }));
        assert!(!r.is_error, "{}", r.text);
        let loud_after = loud_windows(&srv);
        assert!(loud_after.abs_diff(bursts) <= 3, "burst windows {bursts} before, {loud_after} after: {}", r.text);
    }

    #[test]
    fn cut_silence_takes_keep_and_padding_together_and_refuses_keep_over_ten() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.4, "padding": 0.2 }));
        assert!(!r.is_error, "{}", r.text);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 11 }));
        assert!(r.is_error, "{}", r.text);
    }

    /// The loud (above -40 dBFS) 20 ms windows of A1, as the silence scan reads them.
    fn loud_windows(srv: &Server) -> usize {
        let s = srv.session.as_ref().unwrap();
        let (w, _, _) = lane_windows(s, parse::lane("A1").unwrap()).expect("lane windows");
        w.iter().filter(|&&v| v > -40.0).count()
    }

    #[test]
    fn cut_silence_cuts_exactly_the_regions_silence_reports_and_no_tone_sample() {
        // Tone, 1 s of silence, tone: the silence is 1.0 to 2.0 s, and its padding is kept.
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let loud_before = loud_windows(&srv);
        assert!(loud_before >= 99, "tone windows before: {loud_before}");
        let sil = run_silence(&mut srv, &json!({ "target": "A1", "min_silence": 0.05 }));
        assert!(!sil.is_error, "{}", sil.text);
        let cut = run_cut_silence(&mut srv, &json!({ "target": "A1", "min_silence": 0.05, "keep": 0.1 }));
        assert!(!cut.is_error, "{}", cut.text);
        // Same regions, to the frame, for the same settings.
        assert_eq!(
            sil.structured.as_ref().unwrap()["regions"],
            cut.structured.as_ref().unwrap()["regions"],
            "silence {} / cut {}",
            sil.text,
            cut.text
        );
        // The cut removed interior silence only: every tone window is still there (a window
        // either side of the joint may read differently, by a window or two).
        let loud_after = loud_windows(&srv);
        assert!(loud_after.abs_diff(loud_before) <= 3, "tone windows {loud_before} before, {loud_after} after: {}", cut.text);
        // keep 0.1 is under the 0.3 s of padding either side, so the whole 0.67 s between paddings goes,
        // and the 0.15 s of padding each side is what is left: a 0.36 s pause, as measured.
        let removed = parse::secs(cut.structured.as_ref().unwrap()["removed_frames"].as_u64().unwrap() as u32, fps);
        assert!((0.6..=0.7).contains(&removed), "removed {removed} s: {}", cut.text);
    }

    #[test]
    fn silence_default_min_lists_only_regions_as_long_as_the_minimum() {
        // The 1 s quiet run is judged before its padding: the default 0.5 s minimum keeps it, and
        // a 1.2 s minimum, which it does not reach, lists nothing.
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_silence(&mut srv, &json!({ "target": "A1" }));
        assert_eq!(r.structured.as_ref().unwrap()["count"], 1, "{}", r.text);
        let r = run_silence(&mut srv, &json!({ "target": "A1", "min_silence": 1.2 }));
        assert_eq!(r.structured.as_ref().unwrap()["count"], 0, "{}", r.text);
    }

    /// A WAV of `secs` seconds whose sample at `t` is `f(t)`, removed when the test ends.
    fn wav_of(secs: u32, f: impl Fn(f64) -> f64) -> TestWav {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("edith-hear-wav-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("wav.wav");
        write_wav_with(&path, secs, f);
        TestWav { dir, path }
    }

    #[test]
    fn a_pause_of_0_4_s_is_found_at_a_0_3_s_minimum() {
        // Tone, a 0.4 s pause of digital silence, tone. The pause is judged before the 0.15 s of
        // padding comes off its ends, so it is a region at a 0.3 s minimum (0.1 s after padding).
        let wav = wav_of(3, |t| {
            if (1.0..1.4).contains(&t) {
                0.0
            } else {
                0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            }
        });
        let mut srv = server_for(&wav);
        let r = run_silence(&mut srv, &json!({ "target": "A1", "threshold_db": -40, "min_silence": 0.3 }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["count"], 1, "{}", r.text);
    }

    #[test]
    fn the_threshold_is_measured_from_quiet_speech_when_none_is_given() {
        // Speech at about -48 dBFS, pauses of about -80 dBFS. The default -40 dBFS calls the speech
        // quiet too, so the pauses are lost in it; the measured threshold (30 dB under the speech) finds
        // the pauses, and the reply names the threshold it used.
        let speech = |t: f64| 0.00563 * (2.0 * std::f64::consts::PI * 220.0 * t).sin();
        let wav = wav_of(4, |t| {
            if (1.0..1.4).contains(&t) || (2.4..2.8).contains(&t) {
                1.4e-4 * (2.0 * std::f64::consts::PI * 300.0 * t).sin()
            } else {
                speech(t)
            }
        });
        let mut srv = server_for(&wav);
        let fixed = run_silence(&mut srv, &json!({ "target": "A1", "threshold_db": -40, "min_silence": 0.3 }));
        // At -40 the speech is quiet too, so the pauses are not separate silences.
        assert_ne!(fixed.structured.as_ref().unwrap()["count"], 2, "{}", fixed.text);
        let auto = run_silence(&mut srv, &json!({ "target": "A1", "min_silence": 0.3 }));
        assert!(!auto.is_error, "{}", auto.text);
        assert_eq!(auto.structured.as_ref().unwrap()["count"], 2, "{}", auto.text);
        assert_eq!(auto.structured.as_ref().unwrap()["threshold_auto"], true);
        assert!(auto.text.contains("threshold") && auto.text.contains("median speech level"), "{}", auto.text);
    }

    #[test]
    fn a_silence_running_to_the_clip_end_keeps_keep_after_the_speech() {
        // 1 s of tone, then 1 s of digital silence that ends the clip. The tail's pause is kept at
        // 0.4 s after the last speech (its 0.15 s end padding stays too), not cut to ~0.2 s.
        let wav = wav_of(2, |t| if t < 1.0 { 0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() } else { 0.0 });
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        let measured = st["pauses_measured_s"][0].as_f64().expect("a measured pause");
        assert!(measured >= 0.4 - 0.03, "tail pause {measured} s: {}", r.text);
        // The silence was 1 s: 0.4 s is the whole pause left after the speech, its 0.15 s end
        // padding among it, and 0.6 s goes.
        let removed = parse::secs(st["removed_frames"].as_u64().unwrap() as u32, fps);
        assert!((removed - 0.6).abs() < 0.05, "removed {removed} s: {}", r.text);
    }

    #[test]
    fn cut_silence_says_every_silence_is_at_or_under_keep_and_cuts_nothing() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        // The 1 s gap is 0.7 s after padding, so a 1.5 s keep is more than every silence.
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 1.5 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1 silence(s) found") && r.text.contains("all ≤ keep") && r.text.contains("nothing cut"), "{}", r.text);
        assert!(!r.text.contains("no silence found"), "{}", r.text);
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before);
        assert_eq!(srv.ledger.len(), 0);
    }

    #[test]
    fn cut_silence_guard_counts_the_clips_sound_not_the_lead_in_gap() {
        // Two 3 s clips with a 2 s gap between them: the gap is not sound, so the 2 x 0.57 s of quiet
        // inside the clips is about a sixth of the 6 s of sound and is not refused.
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        let fps = fps_of(&srv);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("more than half"), "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        let sound = parse::secs(st["removed_sound_frames"].as_u64().unwrap() as u32, fps);
        assert!((0.9..=1.3).contains(&sound), "sound removed {sound} s: {}", r.text);
    }

    #[test]
    fn cut_silence_over_half_the_sound_counts_the_sound_it_removes() {
        // Over half of the sound is refused with the sound's own figures, not the timeline's.
        let wav = beeps_wav();
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05, "dry_run": true }));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("of 4.000 s of sound"), "{}", r.text);
    }

    #[test]
    fn levels_clamps_a_to_one_frame_past_the_end_and_refuses_more() {
        // A lane of 90 frames at 30 fps: 91 is one frame past its end, which is the end.
        let r = range_arg(&json!({ "range": { "from": "0f", "to": "91f" } }), 30.0, 0, 90, "A2").unwrap();
        assert_eq!(r, (0, 90));
        let err = range_arg(&json!({ "range": { "from": "0f", "to": "92f" } }), 30.0, 0, 90, "A2").unwrap_err();
        assert!(err.contains("3.067s") && err.contains("3.000s"), "{err}");
    }

    #[test]
    fn levels_warns_about_the_range_it_measured_and_says_which() {
        // One second of full-scale tone, then silence: the peak is only in the first second.
        let wav = tone_wav();
        write_wav_with(&wav.path, 3, |t| if t < 1.0 { 0.99 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() } else { 0.0 });
        let mut srv = server_for(&wav);
        let quiet = run_levels(&mut srv, &json!({ "target": "A1", "range": { "from": "1.5s", "to": "2.5s" } }));
        assert!(!quiet.is_error, "{}", quiet.text);
        assert!(!quiet.text.contains("WARNING"), "{}", quiet.text);
        let loud = run_levels(&mut srv, &json!({ "target": "A1", "range": { "from": "0s", "to": "1s" } }));
        assert!(loud.text.contains("WARNING: true peak") && loud.text.contains("over 0.000–1.000s of A1"), "{}", loud.text);
    }

    #[test]
    fn peak_warning_names_the_span_it_measured() {
        assert!(peak_warning(-0.3, -20.0, "0.000–1.000s of A1", false).unwrap().contains("over 0.000–1.000s of A1"));
        assert!(peak_warning(-1.5, -20.0, "the whole A1", false).is_none());
    }

    #[test]
    fn peak_warning_is_strictly_above_the_ceiling() {
        // Exactly -1 dBTP is within spec: no warning. Just over it warns, with the ceiling named.
        assert!(peak_warning(-1.0, -20.0, "the mix", true).is_none());
        let over = peak_warning(-0.94, -20.0, "the mix", true).expect("-0.94 is above the ceiling");
        assert!(over.contains("true peak -0.9 dBTP over the mix is above the -1 dBTP ceiling (clips on some players)"), "{over}");
        assert!(!over.contains("at or above"), "{over}");
    }

    #[test]
    fn peak_warning_is_judged_on_the_shown_value() {
        // -0.96 and -0.95 read -1.0 at one decimal, the ceiling itself: no warning, and no
        // "-1.0 dBTP ... above the -1 dBTP ceiling" line. -0.94 reads -0.9 and warns.
        assert!(peak_warning(-0.96, -20.0, "the mix", true).is_none());
        assert!(peak_warning(-0.951, -20.0, "the mix", true).is_none());
        assert!(peak_warning(-0.95, -20.0, "the mix", true).is_none());
        assert!(peak_warning(-0.949, -20.0, "the mix", true).unwrap().contains("true peak -0.9 dBTP"));
    }

    #[test]
    fn headroom_is_named_only_when_the_target_needs_a_gain_up() {
        // Louder than -14: the normalize turns the mix down, so there is no headroom claim.
        let loud = peak_warning(-0.3, -12.6, "the mix", true).unwrap();
        assert!(loud.contains("a normalize to -14 would turn it down 1.4 dB"), "{loud}");
        assert!(!loud.contains("held by the ceiling"), "{loud}");
        // Quieter than -14: the gain up is held by the peaks, and that is said.
        let quiet = peak_warning(-0.3, -16.0, "the mix", true).unwrap();
        assert!(quiet.contains("no headroom") && quiet.contains("gain up is held"), "{quiet}");
    }

    /// A mix a normalize would turn down 0.6 dB, but whose peak needs 2.2 dB more: the ceiling holds
    /// the level at -15.6 LUFS, and the note says so. A lane names the limiter by its mix instead.
    #[test]
    fn loudness_prediction_counts_the_true_peak_ceiling() {
        let mix = peak_warning(1.2, -13.4, "the mix", true).unwrap();
        assert!(
            mix.contains("a normalize to -14 would turn it down 0.6 dB, but the -1 dBTP ceiling holds it at -15.6 LUFS unless allow_limiter:true"),
            "{mix}"
        );
        let lane = peak_warning(1.2, -13.4, "0.000–4.000s of A1", false).unwrap();
        assert!(lane.contains("holds it at -15.6 LUFS unless a normalize of the mix with allow_limiter:true"), "{lane}");
        // A peak that needs less than the level's own turn-down leaves the prediction as it was.
        let small = peak_warning(-0.4, -13.4, "the mix", true).unwrap();
        assert!(small.contains("would turn it down 0.6 dB") && !small.contains("holds it"), "{small}");
    }

    #[test]
    fn normalize_dry_run_reports_the_planned_gain_and_changes_nothing() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let gain_before = srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A1").unwrap());
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = run_normalize(&mut srv, &json!({"target": "A1", "lufs": -14, "dry_run": true}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("target reached: dry run — would "), "{}", r.text);
        assert!(r.text.contains("· nothing changed"), "{}", r.text);
        assert!(r.text.contains("A1 ") && r.text.contains("dB"), "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        assert_eq!(st["changed"], false);
        assert_eq!(st["lane_moves"].as_array().unwrap().len(), 1);
        let gain_after = srv.session.as_ref().unwrap().lane_gain_db(parse::lane("A1").unwrap());
        assert_eq!(gain_before, gain_after);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
        assert_eq!(srv.ledger.len(), 0);
    }

    #[test]
    fn normalize_dry_run_with_the_limiter_predicts_the_real_run() {
        // The tone peaks at -18 dBTP, so the -15 ceiling binds and allow_limiter gives the gain back
        // through the limiter. The dry run makes that same change, then takes it back.
        let args = json!({ "target": "mix", "lufs": -19, "true_peak": -15, "allow_limiter": true });
        let mut dry_srv = server_on(&tone_asset());
        let depth = dry_srv.session.as_ref().unwrap().undo_depth();
        let dry = run_normalize(&mut dry_srv, &json!({ "target": "mix", "lufs": -19, "true_peak": -15, "allow_limiter": true, "dry_run": true }));
        assert!(!dry.is_error, "{}", dry.text);
        assert!(dry.text.starts_with("target reached: dry run — would normalize "), "{}", dry.text);
        assert!(dry.text.contains("· nothing changed"), "{}", dry.text);
        assert_eq!(dry_srv.ledger.len(), 0);
        // A dry run changes nothing, so it says what the real run would do to the limiter.
        assert!(dry.text.contains("would turn the limiter on at -15."), "{}", dry.text);
        assert!(!dry.text.contains("left on"), "{}", dry.text);
        let s = dry_srv.session.as_ref().unwrap();
        assert!(!s.limiter().on, "the trial's limiter is taken back");
        assert_eq!(s.undo_depth(), depth);

        let mut real_srv = server_on(&tone_asset());
        let real = run_normalize(&mut real_srv, &args);
        assert!(!real.is_error, "{}", real.text);
        assert!(real.text.contains("limiter left on at -15."), "{}", real.text);
        let d = dry.structured.expect("structured");
        let r = real.structured.expect("structured");
        let (dl, rl) = (d["achieved_lufs"].as_f64().unwrap(), r["achieved_lufs"].as_f64().unwrap());
        assert!((dl - rl).abs() < 1e-9, "dry run {dl} vs real {rl}: {} / {}", dry.text, real.text);
        assert_eq!(d["lane_moves"], r["lane_moves"], "{} / {}", dry.text, real.text);
        assert_eq!(d["measured_true_peak_dbtp"], r["measured_true_peak_dbtp"]);
        assert_eq!(d["capped_by"], r["capped_by"]);
        // A boost, not a cut: the lanes go up, and the limiter takes back part of that gain.
        let applied = r["applied_db"].as_f64().unwrap();
        assert!(applied > 0.0, "{}", real.text);
        assert!(real.text.contains("the limiter took back"), "{}", real.text);
        assert!(real.text.contains("lane gain A"), "{}", real.text);
        assert!(real.text.contains("AAC/Opus encoding can add up to about 1 dB of true peak"), "{}", real.text);
    }

    #[test]
    fn normalize_each_headline_says_what_the_lanes_reached() {
        let mut srv = two_lanes_apart();
        // -8 LUFS is past what a -15 dBTP ceiling lets the tone reach on either lane.
        let r = run_normalize(&mut srv, &json!({ "each": true, "lanes": ["A1", "A2"], "lufs": -8, "true_peak": -15 }));
        assert!(!r.is_error, "{}", r.text);
        // Each lane names what it moved by and reached; the peak ceiling holds them both, said once.
        assert!(r.text.starts_with("short of the target: A1 "), "{}", r.text);
        assert!(r.text.contains(" LUFS; A2 "), "{}", r.text);
        assert!(r.text.contains(" — both held by the -15.0 dBTP ceiling (A1 peaked at -18.1 dBTP before, A2 peaked at -24.1 dBTP before); no lane alone reaches -8.0 past it; normalize target mix with allow_limiter:true to reach it on the mix"), "{}", r.text);
        assert!(!r.text.contains("speech"), "{}", r.text);
        assert!(!r.text.contains("normalized each lane to -8.0"), "{}", r.text);
        assert!(!r.text.contains("did not all reach"), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["reached"], false);
    }

    #[test]
    fn normalize_lane_names_the_level_it_reached_and_how_far_short() {
        let mut srv = server_on(&tone_asset());
        let r = run_normalize(&mut srv, &json!({ "target": "A1", "lufs": -8, "true_peak": -15 }));
        assert!(!r.is_error, "{}", r.text);
        // The gain went on, so the headline says what it did, not "did not reach" as if nothing moved.
        assert!(r.text.starts_with("short of the target: A1 raised "), "{}", r.text);
        assert!(!r.text.contains("did not reach"), "{}", r.text);
        assert!(r.text.contains(" dB to -") && r.text.contains(" LUFS, "), "{}", r.text);
        assert!(r.text.contains(" LU short, held by the true-peak ceiling -15.0 dBTP"), "{}", r.text);
        assert_eq!(srv.ledger.len(), 1, "a lane that is held still changes: {}", r.text);
        assert!(r.text.contains("· undo step 1"), "{}", r.text);

        // A dry run changes nothing, so it keeps the "did not reach" headline.
        let mut dry_srv = server_on(&tone_asset());
        let dry = run_normalize(&mut dry_srv, &json!({ "target": "A1", "lufs": -8, "true_peak": -15, "dry_run": true }));
        assert!(dry.text.starts_with("short of the target: dry run — A1 would not reach -8.0: got "), "{}", dry.text);
        assert!(dry.text.contains("· nothing changed"), "{}", dry.text);
        assert_eq!(dry_srv.ledger.len(), 0, "{}", dry.text);
    }

    #[test]
    fn cut_spans_keep_the_total_pause_at_keep_or_twice_the_padding() {
        // A quiet run of 31 frames at 30 fps with 0.15 s (5 frames) of padding each side: the
        // region is 21 frames inside. Keep 0.4 s (12 frames) is the whole pause, padding included,
        // so 2 frames of the region stay and 19 go: 5 + 1 + 1 + 5 = 12 frames of quiet left.
        let spans = cut_spans_for(&[(100, 121)], 0.4, 0.15, 30.0, &[]);
        assert_eq!(spans, vec![(101, 19)], "{spans:?}");
        // Keep under twice the padding: the paddings are all that can stay, so the region goes whole.
        assert_eq!(cut_spans_for(&[(100, 121)], 0.1, 0.15, 30.0, &[]), vec![(100, 21)]);
        // A silence whose quiet is at or under keep is left alone (the quiet is 31 frames).
        assert!(cut_spans_for(&[(100, 121)], 1.1, 0.15, 30.0, &[]).is_empty());
        assert!(cut_spans_for(&[(100, 121)], 1.5, 0.15, 30.0, &[]).is_empty());
    }

    #[test]
    fn cut_spans_keep_the_tail_at_keep_in_all() {
        // The region runs to the clip's end at 126: the 5 frames of padding after it stay, and the
        // kept part follows the speech before it. 5 + 7 + 5 = 12 frames, which is keep 0.4 s.
        let spans = cut_spans_for(&[(100, 121)], 0.4, 0.15, 30.0, &[126]);
        assert_eq!(spans, vec![(102, 19)], "{spans:?}");
        let left = 5 + (102 - 100) + (126 - 121);
        assert_eq!(left, 12);
    }

    #[test]
    fn a_cut_never_reaches_speech_on_either_side() {
        // Speech is loud at both sides of each silence: the padding never cut means no frame of it is.
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let loud_before = loud_windows(&srv);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.05, "force": true }));
        assert!(!r.is_error, "{}", r.text);
        let loud_after = loud_windows(&srv);
        assert!(loud_after.abs_diff(loud_before) <= 3, "tone windows {loud_before} before, {loud_after} after: {}", r.text);
    }

    /// A timeline with V1+A1 as one linked take (test_av.mp4) and picture-only B-roll on V2.
    fn linked_av_with_broll() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let sess = PlaybackSession::open(p).expect("open test asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let tool = crate::server::tools().into_iter().find(|t| t.name == "edit").expect("edit");
        let r = (tool.run)(
            &mut srv,
            &json!({"ops": [{"op": "lane_add", "kind": "video"}, {"op": "place", "source": 0, "at": "0f", "lane": "V2", "audio": false}]}),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    #[test]
    fn cut_silence_defaults_to_the_voice_and_its_linked_picture() {
        let srv = linked_av_with_broll();
        let s = srv.session.as_ref().unwrap();
        let voice = default_cut_scope(s, &Target::Lane(Lane::A1));
        assert!(voice.contains(&Lane::V1) && voice.contains(&Lane::A1), "{voice:?}");
        assert!(!voice.contains(&parse::lane("V2").unwrap()), "B-roll is not linked to the voice: {voice:?}");
        assert_eq!(default_cut_scope(s, &Target::Lane(parse::lane("V2").unwrap())), vec![parse::lane("V2").unwrap()]);
        assert_eq!(default_cut_scope(s, &Target::Mix), s.lanes());
    }

    #[test]
    fn cut_silence_on_the_default_lane_names_the_other_sound_lanes() {
        // A1 (the default target) holds a steady tone with no pause; A2 holds bursts with pauses.
        // The reply says what it scanned, and points at A2 where the pauses are.
        let steady = wav_of(3, |t| 0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin());
        let bursts = bursts_wav();
        let mut srv = server_for(&steady);
        {
            let s = srv.session.as_mut().expect("session");
            let a2 = s.add_lane(LaneKind::Audio);
            s.import(&bursts.path).expect("import the bursts");
            s.place_stream_at(0.0, &bursts.path, 0, Some(a2)).expect("place the bursts on A2");
        }
        let r = run_cut_silence(&mut srv, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("scanned A1 only, the default target; A2 also holds sound: target A2 to cut its pauses"), "{}", r.text);
        // Given a target, the reply does not second-guess the choice.
        let explicit = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!explicit.text.contains("also holds sound"), "{}", explicit.text);
    }

    /// A mix with an offline source is refused by name, with the fix, before any render.
    #[test]
    fn a_mix_with_an_offline_source_is_refused_by_name() {
        let td = crate::testutil::TestDir::new("hear-offline-mix");
        let gone = td.path().join("gone.mp4");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4"), &gone).expect("copy");
        let project = td.path().join("offline.edith");
        let call = |server: &mut Server, name: &str, args: Value| {
            let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
            (t.run)(server, &args)
        };
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [gone]})).is_error);
        assert!(!call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]})).is_error);
        assert!(!call(&mut server, "project_save", json!({})).is_error);
        std::fs::remove_file(&gone).expect("remove");

        let mut opened = Server::new();
        assert!(!call(&mut opened, "project_open", json!({"path": project})).is_error);
        let r = call(&mut opened, "levels", json!({"target": "mix"}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.starts_with("rendering the mix refused: 1 source(s) offline"), "{}", r.text);
        assert!(r.text.contains("src:0") && r.text.contains("media_relink"), "{}", r.text);
        assert!(!r.text.contains("No such file"), "{}", r.text);
    }

    /// The bursts on A1 and A2, each the first 3 s only, and the whole bursts on A3, which the
    /// joint cuts do not list. A1 and A2 end at 3 s; A3 runs to 13 s.
    fn short_joint_server(wav: &TestWav) -> Server {
        let mut srv = server_for(wav);
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "delete", "clip": "A1#0"},
                {"op": "lane_add", "kind": "audio"},
                {"op": "lane_add", "kind": "audio"},
                {"op": "place", "source": 0, "lane": "A1", "at": "0s", "in": "0s", "out": "3s"},
                {"op": "place", "source": 0, "lane": "A2", "at": "0s", "in": "0s", "out": "3s"},
                {"op": "place", "source": 0, "lane": "A3", "at": "0s"},
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        srv
    }

    #[test]
    fn a_joint_silence_is_where_every_listed_lane_is_quiet_and_no_stretch_without_a_clip() {
        let wav = bursts_wav();
        let srv = short_joint_server(&wav);
        let s = srv.session.as_ref().unwrap();
        let fps = parse::session_fps(s);
        let (a1, a2) = (parse::lane("A1").unwrap(), parse::lane("A2").unwrap());
        let cfg = Settings { threshold_db: -40.0, min_silence: 0.0, padding: 0.0, min_keep: 0.0 };
        let found = find_joint(s, &[(a1, -40.0), (a2, -40.0)], cfg).expect("joint silence");
        // Both lanes play the tone 0..1 s and go quiet 1..2.2 s, so the joint silence is that
        // stretch. Past 3 s no listed lane has a clip, so nothing there is silence to cut.
        assert_eq!(found.regions.len(), 1, "{:?}", found.regions);
        let (a, b) = found.regions[0];
        assert!((parse::secs(a, fps) - 1.0).abs() < 0.05, "starts at {:.3} s", parse::secs(a, fps));
        assert!((parse::secs(b, fps) - 2.2).abs() < 0.05, "ends at {:.3} s", parse::secs(b, fps));
    }

    #[test]
    fn a_joint_cut_moves_the_listed_lanes_together_and_leaves_the_unlisted_lane() {
        let wav = bursts_wav();
        let mut srv = short_joint_server(&wav);
        let (a1, a2, a3) = (
            parse::lane("A1").unwrap(),
            parse::lane("A2").unwrap(),
            parse::lane("A3").unwrap(),
        );
        let before_a3 = lane_end(srv.session.as_ref().unwrap(), a3);
        let before_a1 = lane_end(srv.session.as_ref().unwrap(), a1);
        let r = run_cut_silence(&mut srv, &json!({ "lanes": ["A1", "A2"], "keep": 0.4 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(joint)"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        // The cut is the same on both listed lanes, so their ends agree and both shrink.
        assert_eq!(lane_end(s, a1), lane_end(s, a2), "the listed lanes stay in step: {}", r.text);
        assert!(lane_end(s, a1) < before_a1, "the joint silence is cut: {}", r.text);
        // A3 is not listed, so it never moves: its clip is whole and it keeps its end.
        assert_eq!(lane_end(s, a3), before_a3, "an unlisted lane moved: {}", r.text);
        // Its clip runs through the cut, so the reply names it as out of step.
        assert!(r.text.contains("out of step, not cut: A3@0"), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["out_of_step"], json!(["A3@0"]));
        let pauses = r.structured.as_ref().unwrap()["pauses_measured_s"].as_array().expect("pauses").clone();
        assert_eq!(pauses.len(), 1, "{}", r.text);
        let p = pauses[0].as_f64().unwrap();
        assert!((p - 0.4).abs() < 0.05, "joint pause {p} s, want 0.40 s: {}", r.text);
    }

    /// A cut that leaves the timeline as long as it was, or shorter, says which lane holds its end:
    /// the bed on A3, which the cut does not list, still runs to the end. Dry run and real cut say
    /// the same.
    #[test]
    fn a_cut_that_leaves_the_timeline_long_names_the_lane_holding_it() {
        let wav = bursts_wav();
        for dry in [true, false] {
            let mut srv = short_joint_server(&wav);
            let r = run_cut_silence(&mut srv, &json!({ "lanes": ["A1", "A2"], "keep": 0.4, "dry_run": dry }));
            assert!(!r.is_error, "{}", r.text);
            assert!(
                r.text.contains("A3 runs ") && r.text.contains(" past the cut lanes (they end at "),
                "dry {dry}: {}",
                r.text
            );
        }
    }

    /// A cut that shortens the timeline with no uncut lane holding its end gets no such note.
    #[test]
    fn a_cut_that_shortens_the_timeline_has_no_timeline_stays_note() {
        let wav = tone_wav();
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("timeline stays"), "{}", r.text);
    }

    #[test]
    fn a_joint_cut_with_a_video_lane_judges_only_the_audio_lanes() {
        let mut srv = linked_av_with_broll();
        // One audio lane and a video lane is no joint cut: the reply says what it needs.
        for lanes in [json!(["A1", "V1"]), json!(["V1", "V2"])] {
            let r = run_cut_silence(&mut srv, &json!({ "lanes": lanes, "dry_run": true }));
            assert!(r.is_error && r.text.contains("two or more audio lanes"), "{}", r.text);
        }
    }

    #[test]
    fn a_target_with_lanes_is_the_ripple_scope_and_judges_the_target_alone() {
        let wav = bursts_wav();
        for dry in [true, false] {
            let mut srv = short_joint_server(&wav);
            let a1 = parse::lane("A1").unwrap();
            let a2 = parse::lane("A2").unwrap();
            let before = lane_end(srv.session.as_ref().unwrap(), a1);
            let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "lanes": ["A1", "A2"], "keep": 0.4, "dry_run": dry }));
            assert!(!r.is_error, "dry {dry}: {}", r.text);
            assert!(!r.text.contains("(joint)"), "{}", r.text);
            assert_eq!(r.structured.as_ref().unwrap()["lanes"], json!(["A1", "A2"]), "{}", r.text);
            let s = srv.session.as_ref().unwrap();
            if dry {
                assert_eq!(lane_end(s, a1), before, "a dry run cut");
            } else {
                // The spans found on A1 are cut on A2 too, so both lanes shrink and stay in step.
                assert!(lane_end(s, a1) < before, "{}", r.text);
                assert_eq!(lane_end(s, a1), lane_end(s, a2), "the lanes stay in step: {}", r.text);
            }
        }
    }

    #[test]
    fn a_target_with_lanes_adds_the_target_lane_when_it_is_not_listed() {
        let wav = bursts_wav();
        let mut srv = short_joint_server(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "lanes": ["A2"], "keep": 0.4, "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["lanes"], json!(["A1", "A2"]), "{}", r.text);
        assert!(r.text.contains("A1 is the target, so it is cut with the listed lanes"), "{}", r.text);
    }

    #[test]
    fn a_target_with_lanes_cuts_the_video_lanes_at_the_same_spans() {
        // test_av has 0.16 to 0.19 s pauses on A1, which a 0.1 s minimum finds. V1 is the linked
        // picture, V2 the B-roll: both shrink by exactly what A1 loses, so they stay in step.
        let mut srv = linked_av_with_broll();
        let (a1, v1, v2) = (parse::lane("A1").unwrap(), parse::lane("V1").unwrap(), parse::lane("V2").unwrap());
        let before = [a1, v1, v2].map(|l| lane_end(srv.session.as_ref().unwrap(), l));
        let r = run_cut_silence(
            &mut srv,
            &json!({ "target": "A1", "lanes": ["A1", "V1", "V2"], "threshold_db": -40, "min_silence": 0.1, "padding": 0.02, "keep": 0.1 }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.structured.as_ref().unwrap()["cut_count"].as_u64().unwrap_or(0) >= 1, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let after = [a1, v1, v2].map(|l| lane_end(s, l));
        let cut = before[0] - after[0];
        assert!(cut > 0, "A1 was not cut: {}", r.text);
        assert_eq!(before[1] - after[1], cut, "V1 cut at other spans: {}", r.text);
        assert_eq!(before[2] - after[2], cut, "V2 cut at other spans: {}", r.text);
        assert_eq!(after[0], after[1], "V1 and A1 stay in step: {}", r.text);
    }

    #[test]
    fn a_pause_shorter_than_min_silence_says_so_and_names_the_longest_quiet() {
        // Tone, a 0.4 s pause of digital silence, tone: the default min_silence (0.5 s) keeps it out.
        let wav = wav_of(3, |t| {
            if (1.0..1.4).contains(&t) {
                0.0
            } else {
                0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            }
        });
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("no pause of 0.50 s or longer (min_silence) under "), "{}", r.text);
        assert!(r.text.contains(" dBFS on A1; the longest quiet was 0.4"), "{}", r.text);
        assert!(r.text.contains("lower min_silence to cut shorter pauses"), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["count"], 0, "{}", r.text);
    }

    #[test]
    fn no_pause_note_names_the_rule_that_kept_the_quiet_out() {
        let short = no_pause_note("A1", "-40.0 dBFS", 0.5, 0.15, 0.42);
        assert_eq!(
            short,
            "no pause of 0.50 s or longer (min_silence) under -40.0 dBFS on A1; the longest quiet was 0.42 s — lower min_silence to cut shorter pauses; nothing cut"
        );
        let padded = no_pause_note("A1", "-40.0 dBFS", 0.5, 0.3, 0.6);
        assert!(padded.contains("the longest quiet under -40.0 dBFS on A1 was 0.60 s") && padded.contains("lower padding"), "{padded}");
        assert_eq!(no_pause_note("A1", "-40.0 dBFS", 0.5, 0.15, 0.0), "no quiet under -40.0 dBFS on A1; nothing cut");
    }

    #[test]
    fn cut_silence_keep_is_the_total_pause_and_the_reply_says_what_is_left() {
        let wav = bursts_wav();
        let mut srv = server_for(&wav);
        let fps = fps_of(&srv);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.4 }));
        assert!(!r.is_error, "{}", r.text);
        // Padding is inside keep, so the pauses measure keep, not keep plus the padding. The
        // measure is read in 20 ms windows, so each may run a frame over keep at most.
        assert!(r.text.contains("pauses now 0.4"), "{}", r.text);
        let pauses = r.structured.as_ref().unwrap()["pauses_measured_s"].as_array().expect("pauses").clone();
        assert!(pauses.len() >= 4, "{}", r.text);
        for p in pauses {
            let p = p.as_f64().unwrap();
            assert!((0.4 - 0.01..=0.4 + 1.0 / fps + 0.01).contains(&p), "pause {p} s, want 0.40 s: {}", r.text);
        }
    }

    #[test]
    fn a_level_within_a_tenth_of_a_lu_is_reached_on_the_unrounded_value() {
        // -14.1 reads one decimal; the check is on the value, so 0.1 LU off is reached.
        assert!(lufs_reached(-14.0, -14.1));
        assert!(lufs_reached(-14.0, -13.9));
        assert!(!lufs_reached(-14.0, -14.11));
        assert!(!lufs_reached(-14.0, f64::NAN));
        assert!(!lufs_reached(-14.0, f64::NEG_INFINITY));
    }

    /// Locks the lane `name` (or unlocks it) through edit, as a person would with lane_flags.
    fn set_locked(srv: &mut Server, name: &str, locked: bool) {
        let made = crate::tools::edit::run_edit(
            srv,
            &json!({"ops": [{"op": "lane_flags", "lane": name, "locked": locked}]}),
        );
        assert!(!made.is_error, "{}", made.text);
    }

    #[test]
    fn cut_silence_refuses_a_locked_target_with_the_way_out() {
        let wav = tone_wav();
        let mut srv = gapped_lane(&wav);
        set_locked(&mut srv, "A1", true);
        let before = parse::session_end(srv.session.as_ref().unwrap());
        for dry in [false, true] {
            let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "dry_run": dry }));
            assert!(r.is_error, "a locked target cut: {}", r.text);
            assert!(r.text.contains("lane A1 is locked") && r.text.contains("lane_flags"), "{}", r.text);
        }
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), before, "a refused cut changes nothing");
        // Unlocked, the same cut goes through.
        set_locked(&mut srv, "A1", false);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
    }

    #[test]
    fn cut_silence_refuses_a_locked_lane_named_in_lanes() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        set_locked(&mut srv, "A3", true);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A2", "lanes": ["A2", "A3"], "dry_run": true }));
        assert!(r.is_error, "a named locked lane was cut: {}", r.text);
        assert!(r.text.contains("lane A3 is locked") && r.text.contains("lane_flags"), "{}", r.text);
        // A joint cut names its lanes too, so a locked one refuses it the same way.
        let j = run_cut_silence(&mut srv, &json!({ "lanes": ["A2", "A3"], "keep": 0.25 }));
        assert!(j.is_error && j.text.contains("lane A3 is locked"), "{}", j.text);
    }

    #[test]
    fn cut_silence_mix_refuses_over_a_locked_lane_and_changes_nothing() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        set_locked(&mut srv, "A3", true);
        let a2 = parse::lane("A2").unwrap();
        let a3 = parse::lane("A3").unwrap();
        let a2_before = srv.session.as_ref().unwrap().lane_clips(a2).to_vec();
        let a3_before = srv.session.as_ref().unwrap().lane_clips(a3).to_vec();
        let end_before = parse::session_end(srv.session.as_ref().unwrap());
        let ledger_before = srv.ledger.len();
        let r = run_cut_silence(&mut srv, &json!({ "target": "mix", "keep": 0.25 }));
        assert!(r.is_error, "a mix cut over a locked lane ran: {}", r.text);
        assert!(r.text.contains("lane A3 is locked") && r.text.contains("lane_flags"), "{}", r.text);
        assert!(r.text.contains("nothing changed"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(a2), a2_before.as_slice(), "the unlocked lane moved");
        assert_eq!(s.lane_clips(a3), a3_before.as_slice(), "the locked lane moved");
        assert_eq!(parse::session_end(s), end_before, "the timeline changed");
        assert_eq!(srv.ledger.len(), ledger_before, "a refused cut left an undo step");
    }

    /// The dry run and the real run name the same lanes, in the same words.
    #[test]
    fn cut_silence_dry_run_and_real_run_name_the_same_lanes() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        let dry = run_cut_silence(&mut srv, &json!({ "target": "mix", "keep": 0.25, "dry_run": true }));
        assert!(!dry.is_error, "{}", dry.text);
        let names: Vec<String> = dry.structured.as_ref().unwrap()["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let named = format!("({})", names.join("+"));
        assert!(dry.text.contains(&named), "the dry run hides its lanes: {}", dry.text);
        let real = run_cut_silence(&mut srv, &json!({ "target": "mix", "keep": 0.25 }));
        assert!(!real.is_error, "{}", real.text);
        assert!(real.text.contains(&named), "the real run hides its lanes: {}", real.text);
    }

    /// The listed spans are the ones removed: their lengths sum to what the timeline lost.
    #[test]
    fn cut_silence_lists_the_spans_it_removes() {
        let wav = bursts_wav();
        let mut srv = server_for(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1", "keep": 0.25 }));
        assert!(!r.is_error, "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        let listed: u32 = st["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let (a, b) = frames_of(&json!([p]));
                b - a
            })
            .sum();
        assert!(listed > 0, "nothing was cut: {}", r.text);
        assert_eq!(listed, st["removed_frames"].as_u64().unwrap() as u32, "the listed spans differ from the removal: {}", r.text);
    }

    /// A cut on A2 alone leaves A3, which runs past the new end of A2: the reply names A3, how far
    /// past it runs, and the trim that brings it to the end.
    #[test]
    fn cut_silence_names_an_unlisted_lane_that_runs_past_the_cut_lanes() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A2", "lanes": ["A2"], "keep": 0.25 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A3 runs ") && r.text.contains(" s past the cut lanes"), "{}", r.text);
        assert!(r.text.contains("trim {clip:\"A3@") && r.text.contains("edge:\"out\""), "{}", r.text);
    }

    /// A cut that leaves no lane outside it past its end says nothing about the lanes it skipped.
    #[test]
    fn cut_silence_with_no_lane_past_the_cut_names_none() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        let r = run_cut_silence(&mut srv, &json!({ "target": "mix", "keep": 0.25 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("past the cut lanes"), "{}", r.text);
    }

    #[test]
    fn cut_silence_joint_cut_runs_once_the_locked_lane_is_unlocked() {
        let wav = bursts_wav();
        let mut srv = bursts_on_two_lanes(&wav);
        set_locked(&mut srv, "A3", true);
        set_locked(&mut srv, "A3", false);
        let r = run_cut_silence(&mut srv, &json!({ "lanes": ["A2", "A3"], "keep": 0.25 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("skipped locked"), "{}", r.text);
    }

    /// The tone with a silence from 1 s to 2 s, and captions placed on S1 over it: "first line"
    /// 0.5 s to 1.5 s, "second line" 2 s to 3.25 s, "third line" 4 s to 4.75 s.
    fn captioned_tone(wav: &TestWav) -> Server {
        let mut srv = server_for(wav);
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "subtitles_import", "path": srt.display().to_string()},
                {"op": "captions_place", "track": 0}
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        srv
    }

    /// The caption cues on S1 as (text, start in microseconds).
    fn cue_starts(srv: &Server) -> Vec<(String, i64)> {
        let s = srv.session.as_ref().unwrap();
        s.sub_lane_cues(parse::lane("S1").unwrap()).into_iter().map(|c| (c.text, c.start_us)).collect()
    }

    fn start_of(cues: &[(String, i64)], text: &str) -> i64 {
        cues.iter().find(|c| c.0 == text).unwrap_or_else(|| panic!("no cue {text:?} in {cues:?}")).1
    }

    /// Captions ride a silence cut as cut_regions moves them: the cue after the cut moves back by
    /// the time the cut removed, and the reply names the moved cues. The picture is cut too.
    #[test]
    fn cut_silence_moves_the_captions_with_the_cut() {
        let wav = tone_wav();
        let mut srv = captioned_tone(&wav);
        let a1 = parse::lane("A1").unwrap();
        let fps = fps_of(&srv);
        let media = |srv: &Server| super::super::listen::media_end(srv.session.as_ref().unwrap(), &[a1]);
        let (m0, c0) = (media(&srv), cue_starts(&srv));
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let removed = m0 - media(&srv);
        assert!(removed > 0, "the silence is cut: {}", r.text);
        let c1 = cue_starts(&srv);
        let shift = f64::from(removed) / fps * 1e6;
        let frame_us = 1e6 / fps;
        let moved = (start_of(&c0, "third line") - start_of(&c1, "third line")) as f64;
        assert!((moved - shift).abs() <= frame_us, "third line moved {moved} us, the cut removed {shift} us: {}", r.text);
        assert!(r.text.contains("captions followed the cut"), "{}", r.text);
        // The timeline is as long as its captions here: the picture is shorter than "third line" runs.
        assert!(r.text.contains("captions run to"), "{}", r.text);
        // A dry run predicts the same captions and keeps them.
        let mut dry = captioned_tone(&wav);
        let d = run_cut_silence(&mut dry, &json!({ "target": "A1", "dry_run": true }));
        assert!(d.text.contains("captions followed the cut"), "{}", d.text);
        assert_eq!(cue_starts(&dry), c0, "a dry run keeps the captions");
    }

    /// A caption lane named in `lanes` is a valid ripple scope: the cut is made with it, and its
    /// cues move the same as they do on their own.
    #[test]
    fn cut_silence_accepts_a_caption_lane_in_lanes() {
        let wav = tone_wav();
        let mut named = captioned_tone(&wav);
        let mut own = captioned_tone(&wav);
        let r = run_cut_silence(&mut named, &json!({ "target": "A1", "lanes": ["A1", "S1"] }));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("caption lane"), "{}", r.text);
        let o = run_cut_silence(&mut own, &json!({ "target": "A1" }));
        assert!(!o.is_error, "{}", o.text);
        assert_eq!(cue_starts(&named), cue_starts(&own));
    }

    /// A locked caption lane is left alone and named; the cut still runs on the rest.
    #[test]
    fn cut_silence_leaves_a_locked_caption_lane_and_names_it() {
        let wav = tone_wav();
        let mut srv = captioned_tone(&wav);
        let before = cue_starts(&srv);
        set_locked(&mut srv, "S1", true);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("locked captions S1 stay put"), "{}", r.text);
        assert_eq!(cue_starts(&srv), before, "the locked captions do not move");
        // Named in lanes, a lock refuses the cut with the way out, as any locked lane does.
        let named = run_cut_silence(&mut srv, &json!({ "target": "A1", "lanes": ["A1", "S1"] }));
        assert!(named.is_error && named.text.contains("lane_flags"), "{}", named.text);
    }

    /// A caption wholly inside the silence is removed with it, and the reply names it.
    #[test]
    fn cut_silence_drops_a_caption_wholly_inside_the_cut() {
        let wav = tone_wav();
        let td = crate::testutil::TestDir::new("hear-caption-inside");
        let srt = td.path().join("inside.srt");
        std::fs::write(&srt, "1\n00:00:01.250,000 --> 00:00:01.350,000\ngone words\n\n2\n00:00:02.500,000 --> 00:00:02.900,000\nafter words\n").expect("write srt");
        let mut srv = server_for(&wav);
        let made = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "subtitles_import", "path": srt.display().to_string()},
                {"op": "captions_place", "track": 0}
            ]}),
        );
        assert!(!made.is_error, "{}", made.text);
        let r = run_cut_silence(&mut srv, &json!({ "target": "A1" }));
        assert!(!r.is_error, "{}", r.text);
        let texts: Vec<String> = cue_starts(&srv).into_iter().map(|c| c.0).collect();
        assert!(!texts.iter().any(|t| t == "gone words"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "after words"), "{texts:?}");
        assert!(r.text.contains("removed cue 'gone words'"), "{}", r.text);
    }
}
