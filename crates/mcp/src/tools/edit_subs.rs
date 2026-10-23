//! Caption ops for the batched `edit` verb, and the `captions` read tool.
//!
//! A caption lane (S1..) holds placements of subtitle-track windows. A whole
//! track goes down as one placement covering all its cues, the same rule the GUI
//! uses when a palette row is dropped on a lane ([`captions_place`]). The ops here
//! follow the contract of `edit_fx`: [`apply`] answers its own op names, or `None`
//! so the caller can offer the op to the next group.

use engine::project::{Edge, Lane, LaneKind, SubClip};
use engine::PlaybackSession;
use serde_json::{json, Value};

use super::edit_fx::Applied;
use crate::parse;
use crate::server::{opt_str, opt_u64, Reply, Server, Tool};

const NO_PROJECT: &str = "no project open — call project_open or project_new first";

/// The caption ops this module answers.
pub const OPS: &[&str] = &["captions_place", "sub_move", "sub_trim", "sub_delete", "cue_text", "cue_add", "cue_replace", "cue_delete", "cue_time", "cue_merge", "cue_split", "captions_remove", "subtitles_lang"];

/// Largest `captions` reply, in bytes of text.
const CAPTIONS_CAP: usize = 4096;

pub const CAPTIONS_DOC: &str = "List the cues on caption lanes (S1..) in timeline time, one line per cue: \
\"<n>. track T cue C [S1@F]  MM:SS.mmm–MM:SS.mmm  text\". Use the track and cue numbers with the cue_text op (empty text removes the cue) or the cue_delete op. \
[S1@F] is the placed caption that shows the cue: its address for sub_move, sub_trim and sub_delete (a cue split across placements is listed once per piece, each with its own address). \
Only cues that a placed caption shows are listed; the footer lists every subtitle track, placed or not. \
lane limits it to one caption lane; range {from, to} (times, as in the edit tool) limits it to a stretch of the timeline: from is inclusive, to is exclusive, so a cue that starts at to is not listed (and a cue that ends at from is not either). Seconds such as 1s are exact, not rounded to a frame. \
Lists 40 cues a page: a long listing says which page it is and the next one to ask for (page). Each page is capped at about 4 KB; narrow with lane or range when it says more cues were cut.";

/// The help for the `subtitles_lang` op, for the edit tool's op list (help.rs).
pub const SUBTITLES_LANG_DOC: &str = "subtitles_lang {track, lang} — names the language of a subtitle track: lang is an ISO 639-2 code (fra, deu, eng) or a two-letter code or an English name (fr, French). It is what the track's MKV or mp4 export says the track is in; a track with none exports as und. One undo step. subtitles_import {path, lang?} sets it at import: when lang is left out it is guessed from the file name (talk.fr.srt, fra.srt, talk_en.srt, eng.srt), and otherwise left unset.";

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "captions", description: CAPTIONS_DOC, schema: captions_schema, run: run_captions }]
}

fn captions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "lane": { "type": "string", "description": "A caption lane, e.g. S1. Default: every caption lane." },
            "page": { "type": "integer", "minimum": 1, "description": "Which page of 40 cues to list (default 1). A long listing says `next: captions {page:N}`." },
            "range": {
                "type": "object",
                "description": "Only cues overlapping this stretch: from inclusive, to exclusive, e.g. {\"from\": \"0\", \"to\": \"end\"}.",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                }
            }
        }
    })
}

// ---- ops ------------------------------------------------------------------

/// Applies one caption op if it is one of this module's. `None` for any other op name.
pub fn apply(s: &mut PlaybackSession, op: &Value, fps: f64) -> Option<Result<Applied, String>> {
    let name = op.get("op")?.as_str()?;
    let end = parse::session_end(s);
    let r = match name {
        "captions_place" => op_captions_place(s, op, fps, end),
        "sub_move" => op_sub_move(s, op, fps, end),
        "sub_trim" => op_sub_trim(s, op, fps, end),
        "sub_delete" => op_sub_delete(s, op, fps),
        "cue_text" => op_cue_text(s, op),
        "subtitles_lang" => op_subtitles_lang(s, op),
        "cue_add" => op_cue_add(s, op, fps, end),
        "cue_replace" => op_cue_replace(s, op),
        "cue_delete" => op_cue_delete(s, op, fps, end),
        "cue_time" => op_cue_time(s, op, fps, end),
        "cue_merge" => op_cue_merge(s, op),
        "cue_split" => op_cue_split(s, op, fps, end),
        "captions_remove" => op_captions_remove(s, op),
        _ => return None,
    };
    Some(r)
}

/// `captions_place {track, lane?, at?}`: the whole track as one placement from
/// its first microsecond to its last cue, on `lane` (default S1, made if missing),
/// at timeline frame `at` (default 0).
fn op_captions_place(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let track = track_arg(s, op)?;
    let t = &s.subtitles()[track];
    if let Some(why) = &t.refused {
        return Err(format!("captions_place: subtitle track {track} could not be read: {why}"));
    }
    let cues = t.cues.len();
    let end_us = t.cues.iter().map(|c| c.end_us).max().unwrap_or(0);
    if cues == 0 || end_us <= 0 {
        return Err(format!("captions_place: subtitle track {track} has no cues to place"));
    }
    let lane = match op.get("lane") {
        None | Some(Value::Null) => Lane::new(LaneKind::Subtitle, 0),
        Some(Value::String(t)) => parse::lane(t)?,
        Some(_) => return Err("`lane` must be a caption lane name such as S1".into()),
    };
    if lane.kind != LaneKind::Subtitle {
        return Err(format!(
            "captions_place: {} is not a caption lane; captions go on S1.. (pictures and sound go on V/A lanes)",
            parse::lane_name(lane)
        ));
    }
    refuse_if_locked(s, "captions_place", &[lane])?;
    let made = ensure_lane(s, lane)?;
    let target = parse::lane_name(lane);
    let below: Vec<&String> = made.iter().filter(|n| **n != target).collect();
    let stray = match below.as_slice() {
        [] => String::new(),
        [one] => format!("; {one} was created empty, because {target} sits above it"),
        many => format!(
            "; {} were created empty, because {target} sits above them",
            many.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
        ),
    };
    let at = opt_frame(op, "at", fps, end)?.unwrap_or(0);
    // The window is whole frames: a track that ends between two frames would
    // otherwise be stretched over the frames it occupies, and every cue would
    // be shown late by the same fraction (a 4.750 s track on 143 frames showed
    // its 0.500 s cue at 0.502 s). `whole_track` makes the window the frames' own length.
    let sub = SubClip::whole_track(track, end_us, fps);
    let frames = sub.frames;
    s.place_sub(lane, at, sub).map_err(|e| {
        let e = e.to_string();
        let mut msg = format!("captions_place track {track}: {e}");
        if e.contains("already covers") {
            // The clash names the caption in the way by its start frame: say how to clear it.
            let blocker = e
                .split_once("at frame ")
                .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
                .unwrap_or("0");
            let name = parse::lane_name(lane);
            msg.push_str(&format!(
                "; to make room: sub_delete {{clip:\"{name}@{blocker}\"}} removes that caption, \
                 sub_move {{clip:\"{name}@{blocker}\", to_lane:\"S2\"}} moves it to another caption lane, \
                 or captions_place {{track:{track}, lane:\"S2\"}} puts this track on another caption lane"
            ));
        }
        msg
    })?;
    let cue_word = cue_count(cues);
    Ok(Applied {
        text: format!(
            "captions_place track {track} ({cue_word}, {frames}f) → {} @{at}f{stray}",
            parse::lane_name(lane)
        ),
        touched: vec![(lane, at)],
    })
}

/// `sub_move {clip, to_lane?, at}`: moves a caption to another caption lane and/or frame.
fn op_sub_move(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let (lane, idx) = sub_arg(s, op)?;
    let addr = sub_addr(s, lane, idx);
    let before = s.sub_lane(lane)[idx];
    let to = match op.get("to_lane") {
        None | Some(Value::Null) => lane,
        Some(Value::String(t)) => parse::lane(t)?,
        Some(_) => return Err("`to_lane` must be a caption lane name such as S2".into()),
    };
    if to.kind != LaneKind::Subtitle || !s.subtitle_lanes().contains(&to) {
        return Err(format!(
            "sub_move {addr}: {} is not a caption lane here; the caption lanes are {}",
            parse::lane_name(to),
            lane_names(s)
        ));
    }
    let mut locks = sub_group_lanes(s, lane, idx);
    if !locks.contains(&to) {
        locks.push(to);
    }
    refuse_if_locked(s, "sub_move", &locks)?;
    let at = opt_frame(op, "at", fps, end)?.ok_or("missing `at`: give the frame to move the caption to")?;
    s.move_sub(lane, idx, to, at)
        .map_err(|e| format!("sub_move {addr} to {}@{at}f refused: {e}", parse::lane_name(to)))?;
    // Read back by the window it shows: a move changes where it starts, not what it shows.
    let touched: Vec<(Lane, u32)> = s
        .sub_lane(to)
        .iter()
        .find(|c| c.track == before.track && c.in_us == before.in_us && c.out_us == before.out_us)
        .map(|c| vec![(to, c.start)])
        .unwrap_or_default();
    let new = touched
        .first()
        .map(|&(l, st)| parse::addr(l, st))
        .unwrap_or_else(|| format!("{}@{at}", parse::lane_name(to)));
    Ok(Applied { text: format!("sub_move {addr} → {new}"), touched })
}

/// `sub_trim {clip, edge: in|out, to}`: moves one edge of a caption.
fn op_sub_trim(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let (lane, idx) = sub_arg(s, op)?;
    let addr = sub_addr(s, lane, idx);
    let (edge, side) = match op.get("edge").and_then(Value::as_str) {
        Some("in") => (Edge::Start, "in"),
        Some("out") => (Edge::End, "out"),
        _ => return Err("`edge` must be \"in\" or \"out\"".into()),
    };
    let to = opt_frame(op, "to", fps, end)?.ok_or("missing `to`: give the frame the edge moves to")?;
    refuse_if_locked(s, "sub_trim", &sub_group_lanes(s, lane, idx))?;
    if let Err(e) = s.trim_sub(lane, idx, edge, to) {
        let room = match s.trim_sub_room(lane, idx, edge) {
            Some((lo, hi)) => format!(" (it can travel {lo}f–{hi}f)"),
            None => String::new(),
        };
        return Err(format!("sub_trim {addr} {side} to {to}f refused: {e}{room}"));
    }
    let touched = s.sub_lane(lane).get(idx).map(|c| vec![(lane, c.start)]).unwrap_or_default();
    Ok(Applied { text: format!("sub_trim {addr} {side} → {}", parse::tc(to, fps)), touched })
}

/// `sub_delete {clip}`: the Delete key on a caption. A lone caption is lifted
/// (a gap is left); one in a group takes its group with it. A caption address
/// names a whole placement, so a single cue inside one takes `cue_delete`.
fn op_sub_delete(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx) = match sub_arg(s, op) {
        Ok(found) => found,
        Err(e) if e.contains("is not a caption") => return Err(e),
        Err(e) => return Err(format!("{e}{}", single_cue_hint(s, op, fps))),
    };
    let addr = sub_addr(s, lane, idx);
    refuse_if_locked(s, "sub_delete", &sub_group_lanes(s, lane, idx))?;
    if !s.delete_sub(lane, idx) {
        return Err(format!("sub_delete {addr} refused: no such caption"));
    }
    Ok(Applied { text: format!("sub_delete {addr}"), touched: Vec::new() })
}

/// `captions_remove {track, force?}`: removes a caption track from the project, one undo step with
/// the batch. A track with no placement on the timeline goes as it is. A track with placements is
/// refused, naming them, unless `force:true`, which deletes those placements first (a placement in a
/// group is refused: deleting it would delete its group's clips). The engine reindexes the other tracks.
fn op_captions_remove(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let track = track_arg(s, op)?;
    let force = match op.get("force") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("captions_remove: `force` must be true or false".into()),
    };
    let name = s.subtitles()[track].name.clone();
    let placed: Vec<(Lane, usize)> = s
        .subtitle_lanes()
        .into_iter()
        .flat_map(|l| {
            s.sub_lane(l)
                .iter()
                .enumerate()
                .filter(|(_, c)| c.track == track)
                .map(move |(i, _)| (l, i))
                .collect::<Vec<_>>()
        })
        .collect();
    // Removing the track renumbers every track after it, so a locked lane holding one of those
    // (or this track's own placements) would change what it shows.
    let renumbered: Vec<Lane> = s
        .subtitle_lanes()
        .into_iter()
        .filter(|&l| s.sub_lane(l).iter().any(|c| c.track >= track))
        .collect();
    refuse_if_locked(s, "captions_remove", &renumbered)?;
    if !placed.is_empty() && !force {
        let shown: Vec<String> = placed.iter().take(3).map(|&(l, i)| sub_addr(s, l, i)).collect();
        return Err(format!(
            "captions_remove refused: track {track} ({name}) has {} placement(s) on the timeline ({}{}); pass force:true to delete them with it, or sub_delete them first",
            placed.len(),
            shown.join(", "),
            if placed.len() > shown.len() { ", …" } else { "" }
        ));
    }
    if let Some(&(l, i)) = placed.iter().find(|&&(l, i)| s.sub_lane(l)[i].link.is_some()) {
        return Err(format!(
            "captions_remove refused: {} is grouped with clips; ungroup it first, since deleting it would delete its group",
            sub_addr(s, l, i)
        ));
    }
    // Each placement is deleted from the last down, so the indices still to go are unmoved.
    let mut removed = 0usize;
    for &(l, i) in placed.iter().rev() {
        let addr = sub_addr(s, l, i);
        if !s.delete_sub(l, i) {
            return Err(format!("captions_remove refused: {addr} could not be deleted (its lane is locked?)"));
        }
        removed += 1;
    }
    s.remove_subtitles(track)
        .map_err(|e| format!("captions_remove refused: {e}"))?;
    let text = if removed == 0 {
        format!("captions_remove track {track} ({name}) — removed")
    } else {
        format!("captions_remove track {track} ({name}) — removed with its {removed} placement(s)")
    };
    Ok(Applied { text, touched: Vec::new() })
}

/// The track and cue number an op names: `track` and `index`, the numbers captions lists.
fn cue_arg(s: &PlaybackSession, op: &Value, name: &str) -> Result<(usize, usize), String> {
    let track = track_arg(s, op)?;
    let index = op
        .get("index")
        .and_then(Value::as_u64)
        .ok_or("missing `index`: the cue number as captions lists it")? as usize;
    let cues = s.subtitles()[track].cues.len();
    if index >= cues {
        return Err(format!("{name}: track {track} has {}; index {index} is not one of them", cue_count(cues)));
    }
    Ok((track, index))
}

/// The caption lane that shows cue `index` of `track`, to read timeline times through.
fn cue_lane(s: &PlaybackSession, track: usize, index: usize) -> Option<Lane> {
    s.subtitle_lanes()
        .into_iter()
        .find(|&l| s.sub_lane_cue_refs(l).iter().any(|&(_, _, t, c)| t == track && c == index))
}

/// `cue_time {track, index, at?, to?}`: moves one cue's start (`at`) and/or end (`to`), as timeline
/// times (seconds exact). Give both to move or resize, one to trim that edge. The cue stays inside
/// its neighbours and at least 0.1 s long. One undo step.
fn op_cue_time(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let (track, index) = cue_arg(s, op, "cue_time")?;
    refuse_if_tracks_locked(s, "cue_time", &[track])?;
    let at = range_end_us(op, "at", fps, end)?;
    let to = range_end_us(op, "to", fps, end)?;
    if at.is_none() && to.is_none() {
        return Err("cue_time needs `at` (the new start), `to` (the new end), or both, as timeline times".into());
    }
    let lane = cue_lane(s, track, index)
        .ok_or_else(|| format!("cue_time: no caption lane shows track {track} cue {index}; place the track first"))?;
    let cue = &s.subtitles()[track].cues[index];
    let (old_a, old_b) = (cue.start_us, cue.end_us);
    let conv = |tl: i64| s.cue_track_us_at(lane, track, index, tl);
    let a = at.and_then(conv).unwrap_or(old_a);
    let b = to.and_then(conv).unwrap_or(old_b);
    match s.set_cue_times(track, index, a, b) {
        Ok(true) => Ok(Applied {
            text: format!("cue_time track {track} cue {index}  {} → {}", span_text(old_a, old_b), span_text(a, b)),
            touched: Vec::new(),
        }),
        Ok(false) => Err(format!("cue_time: track {track} cue {index} is already there: nothing to change")),
        Err(e) => Err(format!("cue_time: {e}")),
    }
}

/// `cue_merge {track, index}`: joins a cue with the one after it into one cue. One undo step.
fn op_cue_merge(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (track, index) = cue_arg(s, op, "cue_merge")?;
    refuse_if_tracks_locked(s, "cue_merge", &[track])?;
    s.merge_cue_next(track, index).map_err(|e| format!("cue_merge: {e}"))?;
    let words = echo_words(&s.subtitles()[track].cues[index].text);
    Ok(Applied { text: format!("cue_merge track {track} cue {index} + {} → \"{words}\"", index + 1), touched: Vec::new() })
}

/// `cue_split {track, index, at}`: divides one cue in two at timeline time `at`, the words shared
/// at the word nearest the point. One undo step.
fn op_cue_split(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let (track, index) = cue_arg(s, op, "cue_split")?;
    refuse_if_tracks_locked(s, "cue_split", &[track])?;
    let at = range_end_us(op, "at", fps, end)?.ok_or("cue_split needs `at`: where to cut the cue, e.g. \"12s\"")?;
    let lane = cue_lane(s, track, index)
        .ok_or_else(|| format!("cue_split: no caption lane shows track {track} cue {index}; place the track first"))?;
    let t = s.cue_track_us_at(lane, track, index, at).ok_or("cue_split: that cue is not shown")?;
    let cue = &s.subtitles()[track].cues[index];
    if !(cue.start_us < t && t < cue.end_us) {
        return Err(format!(
            "cue_split: {} is not inside the cue ({})",
            clock_us(at),
            span_text(cue.start_us, cue.end_us)
        ));
    }
    if !s.split_cues_at(track, t) {
        return Err("cue_split: the cue has no word to put on each side".into());
    }
    Ok(Applied { text: format!("cue_split track {track} cue {index} at {}", clock_us(at)), touched: Vec::new() })
}

/// `cue_text {track, index, text}`: retypes one cue of a subtitle track. Empty
/// text removes the cue instead ([`remove_cue`]), since a blank cue is no cue.
fn op_cue_text(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let track = track_arg(s, op)?;
    let index = op
        .get("index")
        .and_then(Value::as_u64)
        .ok_or("missing `index`: the cue number as captions lists it")? as usize;
    let raw = op.get("text").and_then(Value::as_str).ok_or("missing `text`: the new words for the cue")?;
    // A literal backslash-n (two characters) is what a model types when it means a line break:
    // read it as one, and say so in the reply, so the cue never keeps a stray `\n`.
    let converted = raw.contains("\\n");
    let text_owned = raw.replace("\\n", "\n");
    let text = text_owned.as_str();
    let cues = &s.subtitles()[track].cues;
    let Some(cue) = cues.get(index) else {
        return Err(format!(
            "cue_text: track {track} has {}; index {index} is not one of them",
            cue_count(cues.len())
        ));
    };
    refuse_if_tracks_locked(s, "cue_text", &[track])?;
    if text.is_empty() {
        return remove_cue(s, track, index);
    }
    if cue.text == text {
        return Err(format!("cue_text: track {track} cue {index} already reads that: nothing to change"));
    }
    if !s.set_cue_text(track, index, text) {
        return Err(format!("cue_text: track {track} cue {index} refused the new text"));
    }
    let note = if converted { "; literal \\n read as a line break" } else { "" };
    Ok(Applied {
        text: format!("cue_text track {track} cue {index} → \"{}\"{note}", echo_words(text)),
        touched: Vec::new(),
    })
}

/// `subtitles_lang {track, lang}`: names a subtitle track's language, as an ISO 639-2 code. One
/// undo step. Refused for a track that is not there, a language that is not one, or the language
/// the track already has.
fn op_subtitles_lang(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let track = track_arg(s, op)?;
    let raw = op
        .get("lang")
        .and_then(Value::as_str)
        .ok_or("missing `lang`: the language as a code (fra, fr) or an English name (French)")?;
    let code = engine::subtitle::language_code(raw).ok_or_else(|| {
        format!("subtitles_lang: `{raw}` is not a language: give its code (fra or fr) or its English name (French)")
    })?;
    if s.subtitles()[track].language == code {
        return Err(format!("subtitles_lang: track {track} is already {code}: nothing to change"));
    }
    if !s.set_subtitle_language(track, &code) {
        return Err(format!("subtitles_lang: track {track} refused the language {code}"));
    }
    let name = engine::subtitle::language_name(&code).map(|n| format!(" ({n})")).unwrap_or_default();
    Ok(Applied { text: format!("subtitles_lang track {track} → {code}{name}"), touched: Vec::new() })
}

/// `cue_add {track, at, to, text, overwrite?, track_clock?, extend?}`: adds a cue to a subtitle
/// track. `at` and `to` are timeline times, as the edit tool takes them (seconds exact), mapped onto
/// the track's own clock through the placement that shows it; `track_clock: true` takes them on the
/// track's clock instead (the times its file gives its cues). The reply gives both clocks. The
/// cue is kept in the project and saved with it. A cue that overlaps one already in the track is
/// refused, unless `overwrite` is true, which replaces every cue it overlaps, as one undo step.
/// A cue no placement shows is kept, and the reply says where it falls outside the captions;
/// `extend: true` then lengthens the caption that ends nearest it so that it shows the cue.
fn op_cue_add(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    let track = track_arg(s, op)?;
    refuse_if_tracks_locked(s, "cue_add", &[track])?;
    // Every placement of the track: (lane, index on the lane, start frame, frames, window in, out).
    let mut placements: Vec<(Lane, usize, u32, u32, i64, i64)> = Vec::new();
    for l in s.subtitle_lanes() {
        for (idx, c) in s.sub_lane(l).iter().enumerate() {
            if c.track == track {
                placements.push((l, idx, c.start, c.frames, c.in_us, c.out_us));
            }
        }
    }
    // A track no caption lane shows has no place in the timeline: its cues are kept on the track's
    // own clock, the timeline reads the same, and the reply says nothing shows until it is placed.
    let track_clock = match op.get("track_clock") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("cue_add: `track_clock` must be true or false")?,
    };
    let extend = match op.get("extend") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("cue_add: `extend` must be true or false")?,
    };
    let at_raw = range_end_us(op, "at", fps, end)?.ok_or("cue_add needs `at`: the cue's start, e.g. \"31s\"")?;
    let to_raw = range_end_us(op, "to", fps, end)?.ok_or("cue_add needs `to`: the cue's end, e.g. \"33s\"")?;
    // The placement whose clock the times are read on: the last one that starts at or before `at`
    // on that clock (a track may have several placements, each with its own window), else the first.
    let clock = |p: &(Lane, usize, u32, u32, i64, i64)| if track_clock { p.4 } else { us_of_frame(p.2, fps) };
    let chosen = placements
        .iter()
        .copied()
        .filter(|p| clock(p) <= at_raw)
        .max_by_key(|p| clock(p))
        .or(placements.first().copied());
    // That placement's map between the clocks: timeline = start + (track - in) * rate, the rate
    // being its proportion (1.0 at unity), as the engine lays cues out. A track no placement shows
    // has no map: its clock is the timeline's.
    let (lane, idx, p0, p_in, rate) = match chosen {
        Some((lane, idx, p_start, p_frames, p_in, p_out)) => {
            let window = (p_out - p_in).max(1);
            (Some(lane), idx, us_of_frame(p_start, fps), p_in, f64::from(p_frames) / fps * 1e6 / window as f64)
        }
        None => (None, 0, 0, 0, 1.0),
    };
    let to_timeline = |t: i64| p0 + ((t - p_in) as f64 * rate).round() as i64;
    let to_track = |tl: i64| p_in + ((tl - p0) as f64 / rate).round() as i64;
    let (at_tr, to_tr, at_tl, to_tl) = if track_clock {
        (at_raw, to_raw, to_timeline(at_raw), to_timeline(to_raw))
    } else {
        (to_track(at_raw), to_track(to_raw), at_raw, to_raw)
    };
    if to_tr <= at_tr {
        return Err(format!(
            "cue_add: the cue's end {}s is not after its start {}s",
            secs_of(to_raw),
            secs_of(at_raw)
        ));
    }
    let raw = op.get("text").and_then(Value::as_str).ok_or("missing `text`: the words for the new cue")?;
    // A literal backslash-n (two characters) is what a model types when it means a line break:
    // read it as one, and say so in the reply, so the cue never keeps a stray `\n`.
    let converted = raw.contains("\\n");
    let text_owned = raw.replace("\\n", "\n");
    if text_owned.is_empty() {
        return Err("cue_add needs `text`: the words for the new cue (empty adds nothing)".into());
    }
    let overwrite = match op.get("overwrite") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or("cue_add: `overwrite` must be true or false")?,
    };
    let before = s.subtitles()[track].cues.len();
    let prior: Vec<(i64, i64, String)> = s.subtitles()[track].cues.iter().map(|c| (c.start_us, c.end_us, c.text.clone())).collect();
    let index = s
        .add_cue(track, at_tr, to_tr, &text_owned, overwrite)
        .map_err(|e| format!("cue_add: {e}"))?;
    let replaced = (before + 1).saturating_sub(s.subtitles()[track].cues.len());
    let note = if converted { "; literal \\n read as a line break" } else { "" };
    // Name the cues an overwrite removed: those of the prior list no longer present.
    let now: Vec<(i64, i64, String)> = s.subtitles()[track].cues.iter().map(|c| (c.start_us, c.end_us, c.text.clone())).collect();
    let gone: Vec<String> = prior
        .iter()
        .filter(|p| !now.contains(p))
        .map(|(a, b, t)| format!("\"{}\" ({:.3}s–{:.3}s)", echo_words(t), secs_of(*a), secs_of(*b)))
        .collect();
    let cue_word = cues_word(replaced);
    let swap = if replaced > 0 {
        if gone.is_empty() {
            format!("; replaced {replaced} overlapping {cue_word}")
        } else {
            format!("; replaced {replaced} overlapping {cue_word}: {}", gone.join(", "))
        }
    } else {
        String::new()
    };
    // Shown by some placement of the track when it overlaps that placement's window.
    let mut shown = placements.iter().any(|&(_, _, _, _, i, o)| at_tr < o && i < to_tr);
    let lane_end = placements.iter().map(|&(_, _, st, fr, _, _)| us_of_frame(st + fr, fps)).max().unwrap_or(0);
    let lane_start = placements.iter().map(|&(_, _, st, _, _, _)| us_of_frame(st, fps)).min().unwrap_or(0);
    let mut ext = String::new();
    let mut extend_failed = None;
    // Lengthening the caption's end cannot bring in a cue that starts before the caption.
    if let (false, true, Some(lane), true) = (shown, extend, lane, at_tl >= lane_start) {
        // The caption that carries the cue is the one this clock reads: its out edge moves to the
        // cue's end (the engine refuses one that would overlap the next caption of its lane).
        let to_frame = (to_tl as f64 * fps / 1e6).ceil().max(0.0) as u32;
        match s.trim_sub(lane, idx, Edge::End, to_frame) {
            Ok(()) => {
                let c = &s.sub_lane(lane)[idx];
                shown = at_tr < c.out_us && c.in_us < to_tr;
                ext = format!("; extended {}'s caption to {:.3}s", parse::lane_name(lane), secs_of(to_tl));
            }
            Err(e) => extend_failed = Some(e.to_string()),
        }
    }
    let warn = if shown {
        String::new()
    } else if placements.is_empty() {
        format!("; cue {index} is on track {track}, which no caption lane shows yet, so nothing shows until captions_place {{track:{track}}} puts it on a caption lane")
    } else {
        let lane_name = lane.map(parse::lane_name).unwrap_or_default();
        if at_tl >= lane_end {
            let fix = match extend_failed {
                Some(e) => format!("extend refused: {e}"),
                None => "extend:true lengthens the caption to cover it, or sub_trim/place".to_string(),
            };
            format!("; cue {index} is past {lane_name}'s end {:.1}s, so it won't show — {fix}", secs_of(lane_end))
        } else {
            format!(
                "; cue {index} lies before {lane_name}'s start {:.1}s, so it won't show — move the cue to a time the caption covers, or sub_move the caption earlier",
                secs_of(lane_start)
            )
        }
    };
    // The two clocks name the same span when they agree, so the second form is left out then.
    let tl_text = span_text(at_tl, to_tl);
    let tr_text = span_text(at_tr, to_tr);
    let clocks = if tl_text == tr_text {
        tl_text
    } else if track_clock {
        format!("{tr_text} track = {tl_text} timeline")
    } else {
        format!("{tl_text} timeline = {tr_text} track")
    };
    Ok(Applied {
        text: format!(
            "cue_add track {track} cue {index}: {clocks} → \"{}\"{swap}{note}{ext}{warn}",
            echo_words(&text_owned)
        ),
        touched: Vec::new(),
    })
}

/// Microseconds as seconds, for the replies.
fn secs_of(us: i64) -> f64 {
    us as f64 / 1e6
}

/// A span in seconds for the replies, `1.000s–2.000s`. A span that runs before zero (a track
/// clock earlier than its start) is joined with "to", so no dash runs into a minus sign.
fn span_text(from: i64, to: i64) -> String {
    if from < 0 || to < 0 {
        format!("{:.3}s to {:.3}s", secs_of(from), secs_of(to))
    } else {
        format!("{:.3}s–{:.3}s", secs_of(from), secs_of(to))
    }
}

/// `cue_delete {track, index}` or `cue_delete {at, lane?}`: removes one caption cue from its
/// subtitle track, as captions lists it. The track is what every placement of it shows, so
/// the cue leaves all of them and nothing moves. `at` names the cue by the time it is shown
/// at, on `lane` or on the first caption lane that shows one there.
fn op_cue_delete(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Applied, String> {
    if op.get("at").is_some_and(|v| !v.is_null()) {
        let at = opt_frame(op, "at", fps, end)?.unwrap_or(0);
        let lanes = match op.get("lane") {
            None | Some(Value::Null) => s.subtitle_lanes(),
            Some(Value::String(t)) => {
                let lane = parse::lane(t)?;
                if lane.kind != LaneKind::Subtitle || !s.subtitle_lanes().contains(&lane) {
                    return Err(format!(
                        "cue_delete: {t} is not a caption lane here; the caption lanes are {}",
                        lane_names(s)
                    ));
                }
                vec![lane]
            }
            Some(_) => return Err("`lane` must be a caption lane name such as S1".into()),
        };
        let Some((track, index, _, _)) = cue_shown_at(s, &lanes, us_of_frame(at, fps)) else {
            return Err(format!("cue_delete: no caption cue is shown at {at}f; captions lists the cues and their times"));
        };
        refuse_if_tracks_locked(s, "cue_delete", &[track])?;
        return remove_cue(s, track, index);
    }
    let track = track_arg(s, op)?;
    let index = op
        .get("index")
        .and_then(Value::as_u64)
        .ok_or("missing `index`: the cue number as captions lists it (or `at`, the time the cue is shown at)")?
        as usize;
    let cues = s.subtitles()[track].cues.len();
    if index >= cues {
        return Err(format!("cue_delete: track {track} has {}; index {index} is not one of them", cue_count(cues)));
    }
    refuse_if_tracks_locked(s, "cue_delete", &[track])?;
    remove_cue(s, track, index)
}

/// Removes cue `index` of subtitle track `track` (both checked by the caller), and says what went.
fn remove_cue(s: &mut PlaybackSession, track: usize, index: usize) -> Result<Applied, String> {
    let cue = &s.subtitles()[track].cues[index];
    let (start, stop, words) = (cue.start_us, cue.end_us, echo_words(&cue.text));
    // The times are the timeline's, as captions lists them; the track's own clock follows in
    // parentheses only when it reads differently.
    let track_text = format!("{}–{}", clock_us(start), clock_us(stop));
    let when = match cue_timeline_span(s, track, index) {
        Some((a, b)) if format!("{}–{}", clock_us(a), clock_us(b)) != track_text => {
            format!("{}–{}  (track {track_text})", clock_us(a), clock_us(b))
        }
        Some(_) | None => track_text,
    };
    let placed: usize = s
        .subtitle_lanes()
        .into_iter()
        .map(|l| s.sub_lane(l).iter().filter(|c| c.track == track).count())
        .sum();
    if !s.delete_cue(track, index) {
        return Err(format!("cue_delete: track {track} cue {index} could not be removed"));
    }
    let gone = match placed {
        0 => String::new(),
        n => format!("; the track's {n} placement(s) no longer show it"),
    };
    Ok(Applied {
        text: format!("cue_delete track {track} cue {index}  {when}  \"{words}\"{gone}"),
        touched: Vec::new(),
    })
}

/// The timeline span of cue `cue` of `track`, from first to last microsecond any placement of the
/// track shows it at (as `sub_lane_cue_refs` clips it). `None` when no placement shows it.
fn cue_timeline_span(s: &PlaybackSession, track: usize, cue: usize) -> Option<(i64, i64)> {
    s.subtitle_lanes()
        .into_iter()
        .flat_map(|l| s.sub_lane_cue_refs(l))
        .filter(|&(_, _, t, c)| t == track && c == cue)
        .map(|(a, b, _, _)| (a, b))
        .reduce(|(a0, b0), (a1, b1)| (a0.min(a1), b0.max(b1)))
}

/// The caption cue shown at `us` on the first of `lanes` that shows one there, as
/// `(track, cue, start_us, end_us)`.
fn cue_shown_at(s: &PlaybackSession, lanes: &[Lane], us: i64) -> Option<(usize, usize, i64, i64)> {
    lanes
        .iter()
        .flat_map(|&lane| s.sub_lane_cue_refs(lane))
        .filter(|&(a, b, _, _)| a <= us && us < b)
        .min_by_key(|r| r.0)
        .map(|(a, b, track, cue)| (track, cue, a, b))
}

/// What a `sub_delete` that names no placed caption adds to its error: the cue shown at
/// the address's frame when there is one, with the op that removes just that cue.
fn single_cue_hint(s: &PlaybackSession, op: &Value, fps: f64) -> String {
    let generic = "; to remove a single caption cue use cue_delete {track, index} (captions lists each cue's track and index)";
    let frame = op
        .get("clip")
        .and_then(Value::as_str)
        .and_then(|a| a.split_once('@'))
        .and_then(|(_, f)| f.trim().parse::<u32>().ok());
    let Some(frame) = frame else {
        return generic.to_string();
    };
    match cue_shown_at(s, &s.subtitle_lanes(), us_of_frame(frame, fps)) {
        Some((track, cue, _, _)) => format!(
            "; a caption cue is shown at {frame}f: cue_delete {{\"track\": {track}, \"index\": {cue}}} removes just that cue"
        ),
        None => generic.to_string(),
    }
}

/// `cue_replace {track?, find, replace, case_sensitive?}`: replaces every occurrence of `find`
/// in the cues of `track`, or of every subtitle track when `track` is omitted. Matching ignores
/// case unless `case_sensitive` is true. The edit batch is one undo step.
fn op_cue_replace(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let find = op
        .get("find")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .ok_or("missing `find`: the text to look for in the cues")?;
    let replace = op
        .get("replace")
        .and_then(Value::as_str)
        .ok_or("missing `replace`: the text to put in its place (an empty string deletes it)")?;
    let case_sensitive = op.get("case_sensitive").and_then(Value::as_bool).unwrap_or(false);
    let tracks: Vec<usize> = match op.get("track") {
        None | Some(Value::Null) => (0..s.subtitles().len()).collect(),
        Some(_) => vec![track_arg(s, op)?],
    };
    // The new texts are worked out first, while the cues are only read.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut occurrences = 0usize;
    for &t in &tracks {
        for (i, cue) in s.subtitles()[t].cues.iter().enumerate() {
            if let Some((new, n)) = replace_in(&cue.text, find, replace, case_sensitive) {
                occurrences += n;
                edits.push((t, i, new));
            }
        }
    }
    if edits.is_empty() {
        return Err(format!("cue_replace: no cue reads \"{find}\"{}", if case_sensitive { " (case-sensitive)" } else { "" }));
    }
    let edited: Vec<usize> = edits.iter().map(|(t, _, _)| *t).collect();
    refuse_if_tracks_locked(s, "cue_replace", &edited)?;
    let changed = edits.iter().filter(|(t, i, new)| s.set_cue_text(*t, *i, new)).count();
    if changed == 0 {
        return Err(format!("cue_replace: none of the {} matching {} accepted the new text", edits.len(), cues_word(edits.len())));
    }
    Ok(Applied {
        text: format!("cue_replace \"{find}\" → \"{replace}\": {occurrences} occurrence(s) in {changed} {}", cues_word(changed)),
        touched: Vec::new(),
    })
}

/// `text` with every match of `find` replaced, and how many there were; None when there is no match.
fn replace_in(text: &str, find: &str, with: &str, case_sensitive: bool) -> Option<(String, usize)> {
    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = find.chars().collect();
    let same = |a: char, b: char| {
        a == b || (!case_sensitive && a.to_lowercase().eq(b.to_lowercase()))
    };
    let mut out = String::with_capacity(text.len());
    let mut n = 0usize;
    let mut i = 0usize;
    while i < hay.len() {
        if i + needle.len() <= hay.len() && needle.iter().enumerate().all(|(k, &c)| same(hay[i + k], c)) {
            out.push_str(with);
            i += needle.len();
            n += 1;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    (n > 0).then_some((out, n))
}

// ---- captions (read) ------------------------------------------------------

/// Cues per `captions` page: a page is what one reply lists, and `page` takes the next.
const CAPTIONS_PAGE: usize = 40;

/// One cue a `captions` listing can show: its lane, its track and cue, the stretch it covers
/// (microseconds), the placement that shows it, and which part of a cut cue it is.
struct Listed {
    lane: Lane,
    track: usize,
    cue: usize,
    a: i64,
    b: i64,
    at: String,
    part: Option<(usize, usize)>,
}

fn run_captions(srv: &mut Server, args: &Value) -> Reply {
    let Some(s) = srv.session.as_ref() else {
        return Reply::err(NO_PROJECT);
    };
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let lanes = match captions_lanes(s, args) {
        Ok(l) => l,
        Err(e) => return Reply::err(e),
    };
    let range = match range_us(args, fps, end) {
        Ok(r) => r,
        Err(e) => return Reply::err(e),
    };

    if s.subtitle_lanes().is_empty() {
        return Reply::ok(format!(
            "no caption lanes on this timeline. subtitles_import {{path}} loads a track; captions_place {{track}} puts it on S1.{}",
            track_footer(s)
        ));
    }

    // Every listed cue in lane order, before paging, so the page count and part numbers are the
    // same whichever page is asked for.
    // Headline counts are distinct cues, not pieces: a cue a cut split across placements is one cue.
    // The cues outside every placement are counted over every track, as the footer counts them.
    let unplaced = unplaced_cue_count(s);
    let range_label = range.map(|(from, to)| {
        let open_end = match args.get("range").and_then(|r| r.get("to")) {
            None | Some(Value::Null) => true,
            Some(Value::String(t)) => t.trim() == "end",
            Some(_) => false,
        };
        format!("{}–{}", clock_us(from), if open_end { "end".to_string() } else { clock_us(to) })
    });
    let mut heads: Vec<String> = Vec::new();
    let mut all: Vec<Listed> = Vec::new();
    for lane in lanes {
        let refs = s.sub_lane_cue_refs(lane);
        let total = distinct_cues(refs.iter().map(|&(_, _, t, c)| (t, c)));
        let inside: Vec<_> = refs
            .into_iter()
            .filter(|&(a, b, _, _)| range.is_none_or(|(from, to)| a < to && b > from))
            .collect();
        heads.push(match &range_label {
            Some(label) => {
                let in_range = distinct_cues(inside.iter().map(|&(_, _, t, c)| (t, c)));
                let more = if unplaced > 0 { format!(", {unplaced} more outside every placement") } else { String::new() };
                format!(
                    "{}: {in_range} {} in {label} ({total} cues placed{more})",
                    parse::lane_name(lane),
                    cues_word(in_range)
                )
            }
            None => format!("{} {}", parse::lane_name(lane), cue_count(total)),
        });
        for &(a, b, track, cue) in &inside {
            // The placement that shows this piece of the cue: the address sub_move and friends take.
            let mid = (a + b) / 2;
            let at = s
                .sub_lane(lane)
                .iter()
                .find(|c| c.track == track && us_of_frame(c.start, fps) <= mid && mid < us_of_frame(c.end(), fps))
                .map(|c| parse::addr(lane, c.start))
                .unwrap_or_default();
            all.push(Listed { lane, track, cue, a, b, at, part: None });
        }
    }
    // A cue a cut split across placements is listed once per piece: each row says which part
    // it is, so no two rows read as the same cue.
    let mut counts: Vec<((usize, usize), usize)> = Vec::new();
    for l in &all {
        match counts.iter_mut().find(|(k, _)| *k == (l.track, l.cue)) {
            Some(c) => c.1 += 1,
            None => counts.push(((l.track, l.cue), 1)),
        }
    }
    let mut seen: Vec<((usize, usize), usize)> = Vec::new();
    for l in all.iter_mut() {
        let of = counts.iter().find(|(k, _)| *k == (l.track, l.cue)).map_or(1, |c| c.1);
        if of > 1 {
            let k = match seen.iter_mut().find(|(key, _)| *key == (l.track, l.cue)) {
                Some(c) => {
                    c.1 += 1;
                    c.1
                }
                None => {
                    seen.push(((l.track, l.cue), 1));
                    1
                }
            };
            l.part = Some((k, of));
        }
    }
    let total = all.len();
    let pages = total.div_ceil(CAPTIONS_PAGE).max(1);
    let page = match opt_u64(args, "page") {
        None => 1,
        Some(p) if p >= 1 && (p as usize) <= pages => p as usize,
        Some(p) => return Reply::err(format!("captions: page {p} is past the last page ({pages}); pages run 1 to {pages}")),
    };
    let first = (page - 1) * CAPTIONS_PAGE;
    let last = (first + CAPTIONS_PAGE).min(total);

    let mut text = format!("{}\n", heads.join(", "));
    if pages > 1 {
        let next = if page < pages { format!("; next: captions {{page:{}}}", page + 1) } else { String::new() };
        text.push_str(&format!(
            "page {page} of {pages} (cues {}–{last} of {total}){next}\n",
            first + 1
        ));
    }
    let mut rows = Vec::new();
    let mut cut = 0usize;
    for (n, l) in all[first..last].iter().enumerate() {
        let body = s
            .subtitles()
            .get(l.track)
            .and_then(|t| t.cues.get(l.cue))
            .map(|c| c.text.replace('\n', " / "))
            .unwrap_or_default();
        let part = l.part.map_or_else(String::new, |(k, of)| format!(" (part {k}/{of})"));
        let line = format!(
            " {}. track {} cue {}{part} [{}]  {}–{}  {body}\n",
            first + n + 1,
            l.track,
            l.cue,
            l.at,
            clock_us(l.a),
            clock_us(l.b)
        );
        if text.len() + line.len() > CAPTIONS_CAP {
            cut = last - first - n;
            break;
        }
        text.push_str(&line);
        rows.push(json!({
            "lane": parse::lane_name(l.lane),
            "track": l.track,
            "cue": l.cue,
            "address": l.at,
            "start_us": l.a,
            "end_us": l.b,
            "text": body,
        }));
    }
    if cut > 0 {
        text.push_str(&format!(
            "… {cut} more {} on this page cut at 4 KB; narrow with lane or range\n",
            cues_word(cut)
        ));
    }
    text.push_str(&track_footer(s));
    let next = (page < pages).then_some(page + 1);
    Reply::ok(text.trim_end().to_string()).with(json!({ "cues": rows, "page": page, "pages": pages, "next": next }))
}

fn captions_lanes(s: &PlaybackSession, args: &Value) -> Result<Vec<Lane>, String> {
    match opt_str(args, "lane") {
        None => Ok(s.subtitle_lanes()),
        Some(t) => {
            let lane = parse::lane(t)?;
            if lane.kind != LaneKind::Subtitle || !s.subtitle_lanes().contains(&lane) {
                return Err(format!(
                    "{t} is not a caption lane here; the caption lanes are {}",
                    lane_names(s)
                ));
            }
            Ok(vec![lane])
        }
    }
}

/// The `range` argument as microseconds `[from, to)` of timeline time, or `None`.
fn range_us(args: &Value, fps: f64, end: u32) -> Result<Option<(i64, i64)>, String> {
    let r = match args.get("range") {
        None | Some(Value::Null) => return Ok(None),
        Some(r) => r,
    };
    if !r.is_object() {
        return Err("argument `range` must be an object such as {\"from\": \"0\", \"to\": \"end\"}".into());
    }
    let from = range_end_us(r, "from", fps, end)?.unwrap_or(0);
    let to = range_end_us(r, "to", fps, end)?.unwrap_or_else(|| us_of_frame(end, fps));
    if to < from {
        return Err(format!(
            "argument `range`: it ends at {}, before it starts at {}",
            clock_us(to),
            clock_us(from)
        ));
    }
    Ok(Some((from, to)))
}

/// One end of a `range` as microseconds. Seconds (`"1s"`, `1.0`) are taken exactly, so
/// a cue at 1.000 s is not moved by a frame rounding; other forms go through a frame.
fn range_end_us(r: &Value, key: &str, fps: f64, end: u32) -> Result<Option<i64>, String> {
    let secs = match r.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.trim().ends_with('s') && !s.contains(':') => {
            s.trim().trim_end_matches('s').trim().parse::<f64>().ok()
        }
        _ => None,
    };
    if let Some(secs) = secs.filter(|v| v.is_finite()) {
        return Ok(Some((secs * 1e6).round() as i64));
    }
    Ok(opt_frame(r, key, fps, end)?.map(|f| us_of_frame(f, fps)))
}

/// One line per subtitle track, so a cue not yet on a lane can still be targeted.
/// The cues some placement shows, per track: a cue a ripple or a trim pushed out of every
/// placement is still in the track, and the footer says so rather than counting it as placed.
fn shown_cues(s: &PlaybackSession) -> Vec<std::collections::BTreeSet<usize>> {
    let mut shown: Vec<std::collections::BTreeSet<usize>> = vec![Default::default(); s.subtitles().len()];
    for l in s.subtitle_lanes() {
        for (_, _, track, cue) in s.sub_lane_cue_refs(l) {
            if let Some(set) = shown.get_mut(track) {
                set.insert(cue);
            }
        }
    }
    shown
}

/// Cues in the tracks that no placement shows, over every track.
fn unplaced_cue_count(s: &PlaybackSession) -> usize {
    let shown = shown_cues(s);
    s.subtitles()
        .iter()
        .enumerate()
        .map(|(i, t)| (0..t.cues.len()).filter(|c| !shown[i].contains(c)).count())
        .sum()
}

/// How many different cues the `(track, cue)` pairs name.
fn distinct_cues(cues: impl Iterator<Item = (usize, usize)>) -> usize {
    cues.collect::<std::collections::BTreeSet<_>>().len()
}

fn track_footer(s: &PlaybackSession) -> String {
    let tracks = s.subtitles();
    if tracks.is_empty() {
        return String::new();
    }
    let shown = shown_cues(s);
    let mut out = String::from("\ntracks:");
    for (i, t) in tracks.iter().enumerate() {
        let placed = shown[i].len();
        let unplaced: Vec<usize> = (0..t.cues.len()).filter(|c| !shown[i].contains(c)).collect();
        let state = match &t.refused {
            Some(why) => format!("refused: {why}"),
            None if t.cues.is_empty() => "0 cues".to_string(),
            None if unplaced.is_empty() => format!("{}, all placed", cue_count(t.cues.len())),
            None => format!("{}, {placed} placed ({})", cue_count(t.cues.len()), unplaced_why(&unplaced)),
        };
        let label = if t.label.is_empty() { String::new() } else { format!(" {}", t.label) };
        out.push_str(&format!("\n track {i}{label} · {state}"));
    }
    out
}

// ---- helpers --------------------------------------------------------------

/// `cue` or `cues`, by the count.
fn cues_word(n: usize) -> &'static str {
    if n == 1 { "cue" } else { "cues" }
}

/// A count of cues, `1 cue` or `9 cues`.
fn cue_count(n: usize) -> String {
    format!("{n} {}", cues_word(n))
}

/// Why some cues are not placed, named by their index as captions lists them: three at most.
fn unplaced_why(cues: &[usize]) -> String {
    let named: Vec<String> = cues.iter().take(3).map(usize::to_string).collect();
    let more = if cues.len() > 3 { ", …" } else { "" };
    let verb = if cues.len() == 1 { "is" } else { "are" };
    format!("{} {}{more} {verb} outside every placement", cues_word(cues.len()), named.join(", "))
}

/// Cue words as a reply echoes them: line breaks shown as " / ", cut to 60 characters.
fn echo_words(text: &str) -> String {
    const MAX: usize = 60;
    let flat = text.replace('\n', " / ");
    match flat.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// The subtitle track named by `track`, checked to exist.
fn track_arg(s: &PlaybackSession, op: &Value) -> Result<usize, String> {
    let track = op
        .get("track")
        .and_then(Value::as_u64)
        .ok_or("missing `track`: the subtitle track number, as captions and subtitles_import list it")?
        as usize;
    let count = s.subtitles().len();
    if track >= count {
        return Err(if count == 0 {
            "there is no subtitle track on this timeline; subtitles_import {path} adds one".to_string()
        } else {
            format!("subtitle track {track} is not on this timeline: the tracks are 0 to {}", count - 1)
        });
    }
    Ok(track)
}

/// A caption address (`S1@0` or `S1#0`) resolved to its lane and index. Refuses media clips.
fn sub_arg(s: &PlaybackSession, op: &Value) -> Result<(Lane, usize), String> {
    let addr = op
        .get("clip")
        .and_then(Value::as_str)
        .ok_or("missing `clip`: give a caption address such as S1@0")?;
    let (lane, idx) = match super::edit::clip_addr(s, addr) {
        Ok(found) => found,
        Err(e) => return Err(caption_addr_hint(s, addr, e)),
    };
    if lane.kind != LaneKind::Subtitle {
        return Err(format!("{addr} is not a caption: caption ops take S-lane addresses such as S1@0"));
    }
    Ok((lane, idx))
}

/// What a caption address that starts no placement adds to its error. A cue's own start frame is
/// the usual wrong guess (captions lists cues, not placements): when a placement covers that frame,
/// the error names it, so the next call can use it. Other addresses keep the generic error.
fn caption_addr_hint(s: &PlaybackSession, addr: &str, generic: String) -> String {
    let Some((lane_name, frame)) = addr.split_once('@') else {
        return generic;
    };
    let (Ok(lane), Ok(frame)) = (parse::lane(lane_name.trim()), frame.trim().parse::<u32>()) else {
        return generic;
    };
    if lane.kind != LaneKind::Subtitle {
        return generic;
    }
    let covering = s.sub_lane(lane).iter().find(|c| c.start <= frame && frame < c.end());
    match covering {
        Some(c) => format!(
            "no caption starts at {addr}; the caption placed at {frame}f is {} (spans {}–{}). Address it by that: sub_move {{\"clip\": \"{}\", ...}}. captions lists each cue with its address in [brackets]",
            parse::addr(lane, c.start),
            c.start,
            c.end(),
            parse::addr(lane, c.start)
        ),
        None => format!("{generic}; captions lists each cue with its address in [brackets], which is the address sub_move takes"),
    }
}

fn sub_addr(s: &PlaybackSession, lane: Lane, idx: usize) -> String {
    match s.sub_lane(lane).get(idx) {
        Some(c) => parse::addr(lane, c.start),
        None => format!("{}#{idx}", parse::lane_name(lane)),
    }
}

/// Makes caption lane `lane` exist, adding caption lanes up to it. Each add is one undo step.
/// Returns the names of the lanes it added, `lane` last. The engine adds lanes in order, so
/// S2 cannot be made without S1 under it: those lanes come back here for the reply to name.
fn ensure_lane(s: &mut PlaybackSession, lane: Lane) -> Result<Vec<String>, String> {
    let mut added: Vec<String> = Vec::new();
    while !s.subtitle_lanes().contains(&lane) {
        if added.len() > lane.ord {
            return Err(format!(
                "{} could not be made; the caption lanes are {}",
                parse::lane_name(lane),
                lane_names(s)
            ));
        }
        let made = s.add_lane(LaneKind::Subtitle);
        added.push(parse::lane_name(made));
    }
    Ok(added)
}

/// Refuses a caption op that would change any of `lanes` while one is locked, naming the lock:
/// `<op> refused: lane S1 is locked: ...`. Nothing is changed when it refuses.
fn refuse_if_locked(s: &PlaybackSession, op: &str, lanes: &[Lane]) -> Result<(), String> {
    super::edit::check_unlocked(s, lanes).map_err(|e| format!("{op} refused: {e}"))
}

/// Refuses a cue op on any of `tracks` while a locked caption lane shows one of them: a cue
/// change reaches every placement of its track, so each such lane counts.
fn refuse_if_tracks_locked(s: &PlaybackSession, op: &str, tracks: &[usize]) -> Result<(), String> {
    let showing: Vec<Lane> = s
        .subtitle_lanes()
        .into_iter()
        .filter(|&l| s.sub_lane(l).iter().any(|c| tracks.contains(&c.track)))
        .collect();
    refuse_if_locked(s, op, &showing)
}

/// The lanes a caption op reaches besides its own: the lanes of its group, which move, trim or
/// go with it. `[lane]` alone when the caption is not grouped.
fn sub_group_lanes(s: &PlaybackSession, lane: Lane, idx: usize) -> Vec<Lane> {
    let mut out = vec![lane];
    let Some(link) = s.sub_lane(lane).get(idx).and_then(|c| c.link) else {
        return out;
    };
    for l in s.lanes().into_iter().chain(s.subtitle_lanes()) {
        let linked = if l.kind == LaneKind::Subtitle {
            s.sub_lane(l).iter().any(|c| c.link == Some(link))
        } else {
            s.lane_clips(l).iter().any(|c| c.link == Some(link))
        };
        if linked && !out.contains(&l) {
            out.push(l);
        }
    }
    out
}

fn lane_names(s: &PlaybackSession) -> String {
    let names: Vec<String> = s.subtitle_lanes().into_iter().map(parse::lane_name).collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

/// A frame argument: a time in any form the edit tool takes, or absent.
fn opt_frame(op: &Value, key: &str, fps: f64, end: u32) -> Result<Option<u32>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => parse::frame(v, fps, end).map(Some).map_err(|e| format!("`{key}`: {e}")),
    }
}

fn us_of_frame(frame: u32, fps: f64) -> i64 {
    (f64::from(frame) / fps * 1e6).round() as i64
}

/// `MM:SS.mmm`, or `H:MM:SS.mmm` from an hour on.
fn clock_us(us: i64) -> String {
    let ms = (us.max(0) as f64 / 1000.0).round() as u64;
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    let milli = ms % 1000;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{milli:03}")
    } else {
        format!("{m:02}:{s:02}.{milli:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn open() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        Server { session: Some(PlaybackSession::open(p).expect("open test asset")), ..Server::default() }
    }

    fn srt() -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/data/test_subs.srt")
            .to_string_lossy()
            .into_owned()
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        crate::tools::edit::run_edit(srv, &json!({ "ops": ops }))
    }

    fn list(srv: &mut Server, args: Value) -> Reply {
        run_captions(srv, &args)
    }

    /// Imports the test track and places it on S1 at 0; returns the placement's frames.
    fn placed(srv: &mut Server) -> u32 {
        let r = ed(srv, json!([{ "op": "subtitles_import", "path": srt() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(srv, json!([{ "op": "captions_place", "track": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0))[0].frames
    }

    /// `subtitles_lang` names a track's language as a code or a name, refuses a
    /// language it cannot read and the one a track already has, and is one undo step.
    #[test]
    fn subtitles_lang_names_a_track_and_one_undo_takes_it_back() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": srt() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "subtitles_lang", "track": 0, "lang": "fr" }]));
        assert!(!r.is_error && r.text.contains("subtitles_lang track 0 → fra (French)"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles()[0].language, "fra");
        let r = ed(&mut srv, json!([{ "op": "subtitles_lang", "track": 0, "lang": "French" }]));
        assert!(r.is_error && r.text.contains("already fra"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "subtitles_lang", "track": 0, "lang": "klingon" }]));
        assert!(r.is_error && r.text.contains("not a language"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "subtitles_lang", "track": 4, "lang": "deu" }]));
        assert!(r.is_error, "{}", r.text);
        let r = run_tool(&mut srv, "undo", json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles()[0].language, "", "one undo takes it back");
    }

    #[test]
    fn captions_place_lists_and_cue_text_retypes() {
        let mut srv = open();
        placed(&mut srv);
        let r = list(&mut srv, json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("S1 3 cues"), "{}", r.text);
        assert!(r.text.contains("track 0 cue 0"), "{}", r.text);
        assert!(r.text.contains("first line"), "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": "Hello there" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({ "lane": "S1" }));
        assert!(r.text.contains("Hello there"), "{}", r.text);
        assert!(!r.text.contains("first line"), "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": "Hello there" }]));
        assert!(r.is_error);
        assert!(r.text.contains("already reads that"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 9, "text": "x" }]));
        assert!(r.is_error && r.text.contains("index 9 is not one of them"), "{}", r.text);
    }

    /// `cue_add` puts a cue in the gap between two cues, replies with its index and times, and is
    /// one undo step; an overlap is refused unless `overwrite`, which replaces what it covers.
    #[test]
    fn cue_add_places_a_cue_in_a_gap_and_refuses_an_overlap() {
        // A track no caption lane shows takes the cue too, and the reply says nothing shows yet.
        let mut unplaced = open();
        let r = ed(&mut unplaced, json!([{ "op": "subtitles_import", "path": srt() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut unplaced, json!([{ "op": "cue_add", "track": 0, "at": "1.6s", "to": "1.9s", "text": "x" }]));
        assert!(!r.is_error && r.text.contains("nothing shows until captions_place {track:0}"), "{}", r.text);
        assert_eq!(unplaced.session.as_ref().unwrap().subtitles()[0].cues.len(), 4, "the cue is kept on the track");

        let mut srv = open();
        placed(&mut srv);
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.6s", "to": "1.9s", "track_clock": true, "text": "Added\\nline" }]));
        assert!(!r.is_error, "{}", r.text);
        // Placed at 0, the track's clock and the timeline agree: one span, no second form.
        assert!(r.text.contains("cue_add track 0 cue 1: 1.600s–1.900s → \"Added / line\""), "{}", r.text);
        assert!(!r.text.contains("track ="), "{}", r.text);
        assert!(r.text.contains("Added / line"), "{}", r.text);
        assert_eq!(srv.ledger.len(), steps + 1, "one batch is one undo step");
        let cues = srv.session.as_ref().unwrap().subtitles()[0].cues.clone();
        assert_eq!(cues.len(), 4, "three file cues and the added one");
        assert_eq!((cues[1].start_us, cues[1].end_us, cues[1].text.as_str()), (1_600_000, 1_900_000, "Added\nline"));

        // Over the first cue (0.5 s to 1.5 s): refused, and nothing changes.
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.0s", "to": "1.7s", "text": "Clash" }]));
        assert!(r.is_error && r.text.contains("overlaps"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles()[0].cues.len(), 4, "a refusal changes nothing");

        // With overwrite it replaces the two cues it covers, in one step.
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.0s", "to": "1.7s", "text": "Over", "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("replaced 2 overlapping cues"), "{}", r.text);
        assert_eq!(srv.ledger.len(), steps + 1, "the replacing add is one undo step");
        let texts: Vec<String> = srv.session.as_ref().unwrap().subtitles()[0].cues.iter().map(|c| c.text.clone()).collect();
        assert_eq!(texts, ["Over", "second line\nwith a break", "third line"], "{texts:?}");

        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 9, "at": "1s", "to": "2s", "text": "x" }]));
        assert!(r.is_error && r.text.contains("track 9"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "2s", "to": "1s", "text": "x" }]));
        assert!(r.is_error && r.text.contains("not after"), "{}", r.text);
    }

    /// A batch that imports a track and then fails is rolled back with the import.
    #[test]
    fn a_failed_batch_takes_its_subtitle_import_back() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": srt() },
                { "op": "captions_place", "track": 5 }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(srv.session.as_ref().unwrap().subtitles().is_empty(), "the import was rolled back: {}", r.text);
    }

    /// A timeline time maps onto the track's clock through the placement, the track-clock flag
    /// takes the track's own times, and a cue no placement shows says where it falls.
    #[test]
    fn cue_add_maps_timeline_times_and_warns_outside_the_caption() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "31s", "to": "32s", "text": "Late" }]));
        assert!(!r.is_error && r.text.contains("won't show"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.6s", "to": "1.9s", "track_clock": true, "text": "Track" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1.600s–1.900s → \"Track\""), "{}", r.text);
        assert!(!r.text.contains("won't show"), "{}", r.text);
    }

    /// Imports the test track and places it on S1 at `at`, as `captions_place` takes it.
    fn placed_at(srv: &mut Server, at: &str) {
        let r = ed(srv, json!([{ "op": "subtitles_import", "path": srt() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(srv, json!([{ "op": "captions_place", "track": 0, "at": at }]));
        assert!(!r.is_error, "{}", r.text);
    }

    /// On a placement that starts at 4 s, a cue_delete reply names the timeline span, with the
    /// track's own clock in parentheses because it reads differently; placed at 0 it names one span.
    #[test]
    fn cue_delete_names_the_timeline_span_and_the_track_clock_only_when_it_differs() {
        let mut srv = open();
        placed_at(&mut srv, "4s");
        let r = ed(&mut srv, json!([{ "op": "cue_delete", "track": 0, "index": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_delete track 0 cue 0  00:04.500–00:05.500  (track 00:00.500–00:01.500)"), "{}", r.text);

        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_delete", "track": 0, "index": 1 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_delete track 0 cue 1  00:02.000–00:03.250  \"second line"), "{}", r.text);
        assert!(!r.text.contains("(track"), "{}", r.text);
    }

    /// A cue before an offset placement's start is accepted with a warning, its reply has no
    /// doubled minus, and both spans read with "to" where the track clock is below zero.
    #[test]
    fn cue_add_before_an_offset_placement_warns_and_never_prints_a_double_minus() {
        let mut srv = open();
        placed_at(&mut srv, "4s");
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "0s", "to": "1s", "text": "Early" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("0.000s–1.000s timeline = -4.000s to -3.000s track"), "{}", r.text);
        assert!(r.text.contains("lies before S1's start 4.0s, so it won't show"), "{}", r.text);
        assert!(!r.text.contains("--") && !r.text.contains("–-"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles()[0].cues.len(), 4, "the cue is kept");
    }

    /// A cue whose two clocks read the same span names that span once.
    #[test]
    fn cue_add_names_one_span_when_the_clocks_agree() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "8s", "to": "9s", "text": "Nine" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_add track 0 cue 3: 8.000s–9.000s → \"Nine\""), "{}", r.text);
        assert!(!r.text.contains("timeline ="), "{}", r.text);
    }

    /// A caption whose trim left its out edge at 4 s shows the cue that crosses it as 3.000–4.000,
    /// the same span the export writes, and the cue after the trim is not shown.
    #[test]
    fn a_cue_crossing_a_trimmed_out_edge_is_listed_clipped() {
        let dir = crate::testutil::TestDir::new("sub-trim-out");
        let path = dir.path().join("cues.srt");
        std::fs::write(&path, "1\n00:00:00,500 --> 00:00:02,000\nAlpha line\n\n2\n00:00:03,000 --> 00:00:04,500\nBravo line\n\n3\n00:00:05,000 --> 00:00:07,000\nCharlie line\n").expect("write");
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": path.to_string_lossy() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "sub_trim", "clip": "S1@0", "edge": "out", "to": "4s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("track 0 cue 1 [S1@0]  00:03.000–00:04.000  Bravo line"), "{}", r.text);
        assert!(!r.text.contains("00:04.500"), "{}", r.text);
        assert!(r.text.contains("cue 2 is outside every placement"), "{}", r.text);
    }

    /// A cue past the caption's end says where the caption ends (its own lane's end, not the first
    /// placement's), and `extend` lengthens the caption so the cue shows.
    #[test]
    fn cue_add_past_the_end_names_the_lane_end_and_extend_shows_it() {
        let mut srv = open();
        let fps = 30.0;
        placed(&mut srv);
        let end = {
            let s = srv.session.as_ref().unwrap();
            let c = &s.sub_lane(Lane::new(LaneKind::Subtitle, 0))[0];
            (c.start + c.frames) as f64 / fps
        };
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "20s", "to": "21s", "text": "Late", "track_clock": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("past S1's end {end:.1}s, so it won't show")), "{}", r.text);
        assert!(r.text.contains("extend:true"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "22s", "to": "23s", "text": "Later", "track_clock": true, "extend": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("extended S1's caption to") && !r.text.contains("won't show"), "{}", r.text);
        let c = &srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0))[0];
        assert!(c.start + c.frames > (end * fps) as u32, "the caption now runs past its old end: {}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("Later"), "{}", r.text);
    }

    /// The captions footer counts the cues of a track and how many a placement shows: a cue no
    /// placement shows is not counted as placed.
    #[test]
    fn captions_footer_says_how_many_cues_are_placed() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "31s", "to": "32s", "text": "Late" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("track 0 explainer.captions.srt") || r.text.contains("· 4 cues, 3 placed"), "{}", r.text);
        assert!(r.text.contains("· 4 cues, 3 placed"), "{}", r.text);
        // The cue no placement shows is named by its index, as captions lists it.
        assert!(r.text.contains("4 cues, 3 placed (cue 3 is outside every placement)"), "{}", r.text);
    }

    /// A range headline names the cues in the range, then the cues placed and the cues outside every
    /// placement, so the counts add up: a range past the placements says 0 in range, not "0 of 17".
    #[test]
    fn captions_range_headline_says_cues_in_range_placed_and_outside() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "31s", "to": "32s", "text": "Late" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = run_tool(&mut srv, "captions", json!({"range": {"from": "30s", "to": "40s"}}));
        assert!(
            r.text.starts_with("S1: 0 cues in 00:30.000–00:40.000 (3 cues placed, 1 more outside every placement)"),
            "{}",
            r.text
        );
        let r = run_tool(&mut srv, "captions", json!({"range": {"from": "0s", "to": "40s"}}));
        assert!(
            r.text.starts_with("S1: 3 cues in 00:00.000–00:40.000 (3 cues placed, 1 more outside every placement)"),
            "{}",
            r.text
        );
        // A range that runs to the end says so, rather than the clock time of the end.
        let r = run_tool(&mut srv, "captions", json!({"range": {"from": "0s", "to": "end"}}));
        assert!(r.text.starts_with("S1: ") && r.text.contains("–end (3 cues placed"), "{}", r.text);
        // Without a range the heading is the lane's placed count, as before.
        let r = run_tool(&mut srv, "captions", json!({}));
        assert!(r.text.starts_with("S1 3 cues"), "{}", r.text);
    }

    /// A track that ends between two frames is placed onto whole frames, and each cue is still shown
    /// at its own time: a 4.750 s track on 143 frames used to show its 0.500 s cue at 0.502 s.
    #[test]
    fn cues_show_at_their_own_times_when_the_track_ends_between_frames() {
        let dir = std::env::temp_dir().join(format!("edith_f17_subs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("off_frame.srt");
        std::fs::write(
            &path,
            "1\n00:00:00,500 --> 00:00:01,500\nfirst\n\n\
             2\n00:00:02,000 --> 00:00:03,250\nsecond\n\n\
             3\n00:00:04,000 --> 00:00:04,750\nthird\n",
        )
        .expect("write");
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": path.to_string_lossy() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        for shown in ["00:00.500–00:01.500", "00:02.000–00:03.250", "00:04.000–00:04.750"] {
            assert!(r.text.contains(shown), "{shown} missing:\n{}", r.text);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An added cue is on its track, so a placed caption shows it, and a save and reopen keep it.
    #[test]
    fn an_added_cue_shows_on_the_caption_lane_and_survives_a_reopen() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.6s", "to": "1.9s", "text": "Added" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("S1 4 cues"), "{}", r.text);
        assert!(r.text.contains("Added"), "{}", r.text);

        let dir = crate::testutil::TestDir::new("cue-add-reopen");
        let path = dir.path().join("proj.edith");
        srv.path = Some(path.clone());
        let r = run_tool(&mut srv, "project_save", json!({}));
        assert!(!r.is_error, "{}", r.text);
        let mut fresh = Server::default();
        let r = run_tool(&mut fresh, "project_open", json!({ "path": path.display().to_string() }));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut fresh, json!({}));
        assert!(r.text.contains("S1 4 cues"), "{}", r.text);
        assert!(r.text.contains("Added"), "{}", r.text);
    }

    #[test]
    fn cue_replace_rewrites_every_match_in_one_undo_step() {
        let mut srv = open();
        placed(&mut srv);
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "cue_replace", "find": "LINE", "replace": "row" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("occurrence(s) in"), "{}", r.text);
        assert_eq!(srv.ledger.len(), steps + 1, "one batch is one undo step");
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("first row"), "{}", r.text);
        assert!(!r.text.to_lowercase().contains("first line"), "{}", r.text);

        // Case-sensitive matching leaves the lower-case word alone.
        let r = ed(&mut srv, json!([{ "op": "cue_replace", "track": 0, "find": "LINE", "replace": "x", "case_sensitive": true }]));
        assert!(r.is_error && r.text.contains("no cue reads"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_replace", "track": 0, "find": "zzz-absent", "replace": "x" }]));
        assert!(r.is_error && r.text.contains("no cue reads"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_replace", "track": 9, "find": "a", "replace": "b" }]));
        assert!(r.is_error, "{}", r.text);
    }

    /// Runs a registered tool by name, as the server would.
    fn run_tool(srv: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("registered tool");
        (t.run)(srv, &args)
    }

    /// A long track is listed 40 cues a page, each page saying where the next one is; a page past
    /// the last is refused by name.
    #[test]
    fn captions_pages_forty_cues_and_says_where_the_next_page_is() {
        let mut srv = open();
        let dir = crate::testutil::TestDir::new("captions-pages");
        let path = dir.path().join("many.srt");
        let mut body = String::new();
        for i in 0..90u32 {
            let (a, b) = (i * 100, i * 100 + 90);
            body.push_str(&format!("{}\n{:02}:{:02}:{:02},{:03} --> {:02}:{:02}:{:02},{:03}\ncue {i}\n\n", i + 1, a / 3_600_000, a / 60_000 % 60, a / 1000 % 60, a % 1000, b / 3_600_000, b / 60_000 % 60, b / 1000 % 60, b % 1000));
        }
        std::fs::write(&path, body).unwrap();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": path.display().to_string() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0 }]));
        assert!(!r.is_error, "{}", r.text);

        let r = list(&mut srv, json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("page 1 of 3 (cues 1–40 of 90); next: captions {page:2}"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 40, "{}", r.text);
        assert!(r.text.contains(" 1. track 0 cue 0 "), "{}", r.text);
        let r2 = list(&mut srv, json!({"page": 2}));
        assert!(r2.text.contains("page 2 of 3 (cues 41–80 of 90); next: captions {page:3}"), "{}", r2.text);
        assert!(r2.text.contains(" 41. track 0 cue 40 "), "{}", r2.text);
        let r3 = list(&mut srv, json!({"page": 3}));
        assert!(r3.text.contains("page 3 of 3 (cues 81–90 of 90)") && !r3.text.contains("next:"), "{}", r3.text);
        assert_eq!(cue_rows(&r3.text), 10, "{}", r3.text);
        assert_eq!(r3.structured.as_ref().unwrap()["next"], serde_json::Value::Null);
        let past = list(&mut srv, json!({"page": 4}));
        assert!(past.is_error && past.text.contains("past the last page (3)"), "{}", past.text);
        assert!(list(&mut srv, json!({"page": 0})).is_error);
    }

    /// The cue rows of a captions listing (" n. track T cue C ..."), not its header or footer.
    /// Each listed cue names the placement sub_move takes, and a cue's own start frame, which
    /// starts no placement, is refused by naming the placement that covers it.
    #[test]
    fn captions_list_the_address_sub_move_takes_for_each_cue() {
        let mut srv = open();
        placed(&mut srv);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let r = list(&mut srv, json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.text.matches("[S1@0]").count(), cue_rows(&r.text), "{}", r.text);
        let rows = r.structured.as_ref().and_then(|v| v["cues"].as_array()).expect("cues");
        assert!(rows.iter().all(|c| c["address"] == "S1@0"), "{rows:?}");
        // The second cue starts at 2 s, inside the placement at 0: that frame is no placement start.
        let frame = (2.0 * fps).round() as u32;
        let r = ed(&mut srv, json!([{ "op": "sub_move", "clip": format!("S1@{frame}"), "at": "30s" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("is S1@0"), "{}", r.text);
        assert!(r.text.contains("in [brackets]"), "{}", r.text);
        // The address the listing gives moves the placement.
        let r = ed(&mut srv, json!([{ "op": "sub_move", "clip": "S1@0", "at": "30s" }]));
        assert!(!r.is_error, "{}", r.text);
    }

    fn cue_rows(text: &str) -> usize {
        text.lines()
            .filter(|l| l.starts_with(' ') && l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
            .count()
    }

    #[test]
    fn cue_text_turns_a_literal_backslash_n_into_a_line_break_and_says_so() {
        let mut srv = open();
        placed(&mut srv);
        // Two characters, backslash then n, as a model types a line break.
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 1, "text": "Line one\\nline two" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("literal \\n read as a line break"), "{}", r.text);
        assert!(r.text.contains("\"Line one / line two\""), "{}", r.text);
        let kept = srv.session.as_ref().unwrap().subtitles()[0].cues[1].text.clone();
        assert_eq!(kept, "Line one\nline two", "a real newline is stored, not two characters");
        // A real newline needs no note.
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": "Plain" }]));
        assert!(!r.text.contains("literal"), "{}", r.text);
    }

    #[test]
    fn captions_place_clash_says_how_to_clear_the_caption_in_the_way() {
        let mut srv = open();
        placed(&mut srv);
        // The track already sits on S1 from frame 0: placing it there again clashes.
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0 }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("already covers"), "{}", r.text);
        assert!(r.text.contains("sub_delete {clip:\"S1@0\"}"), "{}", r.text);
        assert!(r.text.contains("sub_move {clip:\"S1@0\", to_lane:\"S2\"}"), "{}", r.text);
    }

    #[test]
    fn cue_text_reply_echoes_the_new_words() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 1, "text": "Line one\nline two" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_text track 0 cue 1 → \"Line one / line two\""), "{}", r.text);

        let long = "x".repeat(100);
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": long }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("\"{}…\"", "x".repeat(60))), "{}", r.text);
        assert!(!r.text.contains(&"x".repeat(61)), "the echo stops at 60 characters: {}", r.text);
    }

    /// A cut over a whole caption removes it from the listing, the header count
    /// agrees with the rows, and the reopened project lists the same. A ripple
    /// delete (remove_time) over the same span is the same cut.
    #[test]
    fn a_cut_over_a_whole_caption_leaves_no_copy_and_the_count_agrees() {
        let mut srv = open();
        placed(&mut srv);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // Cue 2 ("second line") runs 2.000 s to 3.250 s: cut the frames it covers.
        let from = (2.0 * fps).round() as u32;
        let to = (3.25 * fps).ceil() as u32;
        let r = ed(&mut srv, json!([{ "op": "cut_regions", "regions": [[format!("{from}f"), format!("{to}f")]] }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "header and rows agree: {}", r.text);
        assert!(!r.text.contains("second line"), "{}", r.text);

        // The same cut as a ripple delete of that span.
        let mut rip = open();
        placed(&mut rip);
        let r = ed(&mut rip, json!([{ "op": "remove_time", "at": format!("{from}f"), "duration": format!("{}f", to - from) }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut rip, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);

        // Saved and reopened: the same two cues, and no copy of the cut one.
        let dir = crate::testutil::TestDir::new("cut-whole-caption");
        let path = dir.path().join("proj.edith");
        srv.path = Some(path.clone());
        let r = run_tool(&mut srv, "project_save", json!({}));
        assert!(!r.is_error, "{}", r.text);
        let mut fresh = Server::default();
        let r = run_tool(&mut fresh, "project_open", json!({ "path": path.display().to_string() }));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut fresh, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);
        assert!(!r.text.contains("second line"), "{}", r.text);
    }

    /// `cue_delete {track, index}` removes one cue from the listing, names it in
    /// the reply, and the removal (with a retype after it) survives a save and reopen.
    #[test]
    fn cue_delete_removes_one_cue_and_survives_a_reopen() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_delete", "track": 0, "index": 1 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_delete track 0 cue 1"), "{}", r.text);
        assert!(r.text.contains("second line"), "the reply names what went: {}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);
        assert!(!r.text.contains("second line"), "{}", r.text);
        assert!(r.text.contains("track 0 cue 1"), "the cue after it moved up one: {}", r.text);

        // A retype of the cue that moved up is kept under its file index.
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 1, "text": "Third one" }]));
        assert!(!r.is_error, "{}", r.text);

        let dir = crate::testutil::TestDir::new("cue-delete");
        let path = dir.path().join("proj.edith");
        srv.path = Some(path.clone());
        let r = run_tool(&mut srv, "project_save", json!({}));
        assert!(!r.is_error, "{}", r.text);
        let mut fresh = Server::default();
        let r = run_tool(&mut fresh, "project_open", json!({ "path": path.display().to_string() }));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut fresh, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);
        assert!(!r.text.contains("second line"), "the removed cue stays gone: {}", r.text);
        assert!(r.text.contains("Third one"), "the retype came back: {}", r.text);
        assert!(!r.text.contains("third line"), "{}", r.text);
    }

    /// Empty `cue_text` removes the cue rather than leaving a blank one.
    #[test]
    fn cue_text_with_no_words_removes_the_cue() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": "" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_delete track 0 cue 0"), "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert!(r.text.contains("S1 2 cues"), "{}", r.text);
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);
        assert!(!r.text.contains("first line"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_delete", "track": 0, "index": 5 }]));
        assert!(r.is_error && r.text.contains("index 5 is not one of them"), "{}", r.text);
    }

    /// `cue_delete {at}` takes the cue shown at that time; `sub_delete` on a time
    /// inside a placed cue says so and names the cue_delete that removes just it.
    #[test]
    fn cue_delete_by_time_and_the_sub_delete_hint() {
        let mut srv = open();
        placed(&mut srv);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // "second line" runs 2.000 s to 3.250 s; 2.5 s is inside it.
        let frame = (2.5 * fps).round() as u32;

        let r = ed(&mut srv, json!([{ "op": "sub_delete", "clip": format!("S1@{frame}") }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("cue_delete") && r.text.contains("\"index\": 1"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0)).len(), 1);

        let r = ed(&mut srv, json!([{ "op": "cue_delete", "at": format!("{frame}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("second line"), "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert_eq!(cue_rows(&r.text), 2, "{}", r.text);
        assert!(!r.text.contains("second line"), "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "cue_delete", "at": "2.5s", "lane": "S2" }]));
        assert!(r.is_error && r.text.contains("is not a caption lane here"), "{}", r.text);
    }

    /// A cut that leaves a sliver of a cue under four frames at its edge shows no
    /// sliver: the cue's tail is still a cue, and the cue is listed once.
    #[test]
    fn a_cut_edge_sliver_is_not_a_cue() {
        let mut srv = open();
        placed(&mut srv);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // "second line" runs 2.000 s to 3.250 s: cutting 2.1 s to 2.9 s leaves 0.1 s at its head.
        let from = (2.1 * fps).round() as u32;
        let to = (2.9 * fps).round() as u32;
        let r = ed(&mut srv, json!([{ "op": "cut_regions", "regions": [[format!("{from}f"), format!("{to}f")]] }]));
        assert!(!r.is_error, "{}", r.text);
        let r = list(&mut srv, json!({}));
        assert_eq!(cue_rows(&r.text), 3, "the head sliver is no cue: {}", r.text);
        // Cues split at the cut: the words inside it ("with") and the 0.1 s head ("second line") are gone; "a break" survives.
        assert!(!r.text.contains("second line"), "the head sliver's words are gone: {}", r.text);
        assert!(!r.text.contains("with"), "the words inside the cut are gone: {}", r.text);
        assert!(r.text.contains("a break"), "the tail after the cut stays: {}", r.text);
    }

    #[test]
    fn replace_in_counts_and_respects_case() {
        assert_eq!(replace_in("a Cat, a cat", "cat", "dog", false), Some(("a dog, a dog".into(), 2)));
        assert_eq!(replace_in("a Cat, a cat", "cat", "dog", true), Some(("a Cat, a dog".into(), 1)));
        assert_eq!(replace_in("aaaa", "aa", "b", true), Some(("bb".into(), 2)));
        assert_eq!(replace_in("nothing", "zz", "b", false), None);
        assert_eq!(replace_in("Édith LUFS", "édith", "Edith", false), Some(("Edith LUFS".into(), 1)));
    }

    #[test]
    fn captions_place_refuses_overlap_and_makes_missing_lanes() {
        let mut srv = open();
        placed(&mut srv);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "at": "0f" }]));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth, "a refused placement changes nothing");

        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S3", "at": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let lanes = srv.session.as_ref().unwrap().subtitle_lanes();
        assert_eq!(lanes.len(), 3, "S2 and S3 were made alongside S1");
    }

    #[test]
    fn sub_move_trim_delete_on_a_placed_track() {
        let mut srv = open();
        let frames = placed(&mut srv);

        let r = ed(&mut srv, json!([{ "op": "sub_move", "clip": "S1@0", "at": "10s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let start = s.sub_lane(Lane::new(LaneKind::Subtitle, 0))[0].start;
        assert!(start > 0, "moved away from 0");
        assert!(r.text.contains(&format!("S1@{start}")), "{}", r.text);

        let to = format!("S1@{start}");
        let half = frames / 2;
        let r = ed(&mut srv, json!([{ "op": "sub_trim", "clip": to, "edge": "out", "to": format!("{}f", start + half) }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0))[0].frames, half);

        let r = ed(&mut srv, json!([{ "op": "sub_delete", "clip": to }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0)).is_empty());
    }

    #[test]
    fn caption_ops_refuse_media_clips_and_missing_lanes() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "sub_delete", "clip": "V1@0" }]));
        assert!(r.is_error && r.text.contains("is not a caption"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "sub_move", "clip": "S1@0", "to_lane": "S9", "at": "2s" }]));
        assert!(r.is_error && r.text.contains("S9 is not a caption lane"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 5 }]));
        assert!(r.is_error && r.text.contains("is not on this timeline"), "{}", r.text);
    }

    #[test]
    fn captions_lists_nothing_helpfully_without_lanes() {
        let mut srv = open();
        let r = list(&mut srv, json!({}));
        assert!(!r.is_error);
        assert!(r.text.contains("no caption lanes"), "{}", r.text);
    }

    /// Timeline frames a track's window covers, at least one. The GUI's rule for a dropped row.
    /// Test-only: nothing in the tool calls it any more.
    fn frames_of_us(us: i64, fps: f64) -> u32 {
        match fps.is_finite() && fps > 0.0 {
            true => ((us as f64) * fps / 1e6).round().clamp(1.0, f64::from(u32::MAX)) as u32,
            false => 1,
        }
    }

    #[test]
    fn frames_of_us_rounds_and_never_zero() {
        assert_eq!(frames_of_us(1_000_000, 25.0), 25);
        assert_eq!(frames_of_us(1, 25.0), 1);
        assert_eq!(frames_of_us(5, 0.0), 1);
    }

    #[test]
    fn clock_is_minutes_and_millis() {
        assert_eq!(clock_us(1_200_000), "00:01.200");
        assert_eq!(clock_us(3_723_500_000), "1:02:03.500");
    }

    /// An overwrite names the cue it replaces, and a remove_time over captions names the cue it trims.
    #[test]
    fn overwrite_and_a_cut_over_captions_name_the_cues() {
        let mut srv = open();
        placed(&mut srv);
        // Over the first file cue (0.5 s to 1.5 s): the overwrite says which cue it replaced.
        let r = ed(&mut srv, json!([{ "op": "cue_add", "track": 0, "at": "1.0s", "to": "1.7s", "text": "Over", "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("replaced") && r.text.contains("\"first line") && r.text.contains("0.500s–1.500s"), "{}", r.text);
        // Cutting 1.0 s to 1.5 s trims the "Over" cue (1.0 s to 1.7 s) at its edge, and the reply names it.
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1.0s", "duration": "0.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains("caption cue(s) trimmed: S1 cue 1 'Over' → 'Over' (was 1.000s–1.700s, now 1.000s–1.200s)"),
            "{}",
            r.text
        );
    }

    #[test]
    fn captions_remove_takes_an_unplaced_track_and_refuses_a_placed_one_until_forced() {
        let mut srv = open();
        placed(&mut srv);
        // A second, unplaced track, from a file of its own.
        let dir = std::env::temp_dir().join(format!("edith_r14_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let second = dir.join("second.srt");
        std::fs::write(&second, "1\n00:00:00,500 --> 00:00:01,000\nother track\n").unwrap();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": second.to_string_lossy() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_remove", "track": 1 }]));
        assert!(!r.is_error && r.text.contains("removed"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles().len(), 1, "track 1 is gone");
        // The placed track 0 is refused, naming its placement, and nothing changes.
        let before = srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0)).len();
        let r = ed(&mut srv, json!([{ "op": "captions_remove", "track": 0 }]));
        assert!(r.is_error && r.text.contains("1 placement(s) on the timeline (S1@0)") && r.text.contains("force:true"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0)).len(), before);
        // With force:true the placement goes with the track, as one step that undo gives back.
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "captions_remove", "track": 0, "force": true }]));
        assert!(!r.is_error && r.text.contains("with its 1 placement(s)"), "{}", r.text);
        assert!(srv.session.as_ref().unwrap().subtitles().is_empty());
        assert!(srv.session.as_ref().unwrap().undo_depth() > depth);
        let r = run_tool(&mut srv, "undo", json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitles().len(), 1, "undo brings the track back");
        assert_eq!(srv.session.as_ref().unwrap().sub_lane(Lane::new(LaneKind::Subtitle, 0)).len(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A locked caption lane is never changed: every caption op that would change it refuses,
    /// names the lock, and leaves the history and the cues as they were. Unlocking lets them work.
    #[test]
    fn a_locked_caption_lane_refuses_every_caption_op() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "S1", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let cues_before: Vec<String> = srv.session.as_ref().unwrap().subtitles()[0].cues.iter().map(|c| c.text.clone()).collect();
        let cases = [
            json!({ "op": "sub_move", "clip": "S1@0", "at": "3s" }),
            json!({ "op": "sub_trim", "clip": "S1@0", "edge": "out", "to": "2s" }),
            json!({ "op": "sub_delete", "clip": "S1@0" }),
            json!({ "op": "captions_place", "track": 0, "lane": "S1", "at": "5s" }),
            json!({ "op": "cue_text", "track": 0, "index": 0, "text": "changed" }),
            json!({ "op": "cue_text", "track": 0, "index": 0, "text": "" }),
            json!({ "op": "cue_delete", "track": 0, "index": 0 }),
            json!({ "op": "cue_delete", "at": "1s" }),
            json!({ "op": "cue_replace", "find": "a", "replace": "b" }),
            json!({ "op": "cue_add", "track": 0, "at": "10s", "to": "11s", "text": "new" }),
            json!({ "op": "captions_remove", "track": 0 }),
        ];
        for case in cases {
            let name = case["op"].as_str().unwrap().to_string();
            let r = ed(&mut srv, json!([case]));
            assert!(r.is_error && r.text.contains("lane S1 is locked"), "{name} must refuse on the lock: {}", r.text);
        }
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.undo_depth(), depth, "refused ops leave no history");
        assert_eq!(s.subtitles()[0].cues.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), cues_before, "refused ops change no cue");
        assert_eq!(s.sub_lane(Lane::new(LaneKind::Subtitle, 0)).len(), 1, "the placement is where it was");

        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "S1", "locked": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cue_text", "track": 0, "index": 0, "text": "changed" }]));
        assert!(!r.is_error, "unlocked, the cue edit goes through: {}", r.text);
    }

    /// A move onto a locked caption lane is refused naming that lane, and the caption stays put.
    #[test]
    fn a_move_onto_a_locked_caption_lane_is_refused() {
        let mut srv = open();
        placed(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S2", "at": "0f" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "S2", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "sub_move", "clip": "S1@0", "to_lane": "S2", "at": "1s" }]));
        assert!(r.is_error && r.text.contains("lane S2 is locked"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    /// captions_place on S2 with no S1 makes S1 first (the engine adds lanes in order), and the
    /// reply says that S1 was made empty rather than leaving it unexplained.
    #[test]
    fn captions_place_on_s2_says_the_empty_s1_it_made() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "subtitles_import", "path": srt() }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S2" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("S1 was created empty, because S2 sits above it"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().subtitle_lanes().len(), 2);
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S1" }]));
        assert!(!r.is_error && !r.text.contains("created empty"), "{}", r.text);
    }
}
