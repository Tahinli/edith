//! Motion tracking: the `track` tool follows a rect or ellipse mask's region through a clip in
//! the background, and `track_status` reports where each track stands and writes the finished
//! ones onto their masks as keys. The matching is `engine::track`; the writing is
//! `PlaybackSession::track_mask_apply`. A track is not an edit batch, so it is recorded in the
//! undo ledger when it lands.

use engine::playback::TrackJob;
use engine::project::LaneKind;
use engine::track::{self, Status};
use serde_json::{Value, json};

use super::edit_fx::src_at;
use crate::parse;
use crate::server::{Reply, Server, Tool};

const TRACK_DOC: &str = "track {clip, effect_index, from?, to?, direction?, apply?, cancel?} — follows a rect or ellipse mask's box through a clip and moves the mask with the picture. \
     The box is the mask's own at from; the picture under it is matched frame to frame (sub-pixel, template learned slowly), and the mask's centre takes the same path, its size and soft edge kept. \
     from and to are clip-relative times (default: the clip's start and end; direction \"backward\" defaults to starting at the end). \
     The track runs in the background: track_status shows its percent and writes the keys when it has finished (they show as diamonds on the mask). \
     It stops where the feature is lost (a flat or line-only patch, or a second place that matches nearly as well) and says at which frame. apply: \"mask\" (the default); a title cannot be tracked, since titles have no keyable position. \
     The path follows the clip's placement (position, scale, rotation, crop and transform keys), so the mask stays on the feature on a moved or scaled clip. Not yet: a source stored turned (a phone's portrait video) or with non-square pixels is tracked as stored. \
     cancel:true with a clip stops that clip's tracks. \
     e.g. track {clip:\"V1#0\", effect_index:1, direction:\"forward\"}";

const STATUS_DOC: &str = "Where each running track stands (percent). A finished track's keys are written onto its mask, and the reply says how far it got. wait:true blocks until every running track has finished.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "track", description: TRACK_DOC, schema: track_schema, run: run_track },
        Tool { name: "track_status", description: STATUS_DOC, schema: status_schema, run: run_status },
    ]
}

fn track_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "clip": { "type": "string", "description": "The clip, such as V1#0 or V1@0." },
            "effect_index": { "type": "integer", "description": "The mask's stack position on the clip, 0 for the first effect." },
            "from": { "type": "string", "examples": ["0s"], "description": "Where the track starts, clip-relative (\"0s\", \"1.5s\", \"end\"). Default: the clip's start (its end for backward)." },
            "to": { "type": "string", "examples": ["2s"], "description": "Where it stops, clip-relative. Default: the clip's end (its start for backward)." },
            "direction": { "type": "string", "enum": ["forward", "backward"] },
            "apply": { "type": "string", "default": "mask", "description": "What the track writes to: the mask. Titles are not trackable." },
            "cancel": { "type": "boolean", "description": "Stop this clip's running tracks instead of starting one." }
        },
        "required": ["clip"]
    })
}

fn status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "wait": { "type": "boolean", "description": "Block until every running track has finished. Default false." }
        }
    })
}

fn opt_bool(args: &Value, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

/// Writes every finished track onto its mask and keeps the running ones; returns one line per
/// track that ended.
pub(super) fn settle(srv: &mut Server) -> Vec<String> {
    let mut jobs = std::mem::take(&mut srv.tracks);
    let Some(s) = srv.session.as_mut() else {
        return Vec::new();
    };
    let start = s.history_pushes();
    let fps = parse::session_fps(s);
    let done = s.track_settle(&mut jobs);
    let notes = done
        .into_iter()
        .map(|d| {
            let name = format!("{}#{} mask {}", parse::lane_name(d.lane), d.idx, d.slot);
            match d.result {
                Ok(o) => {
                    let lost = o.lost_at.map_or(String::new(), |f| format!(", lost the feature at frame {f} ({:.2}s)", parse::secs(f, fps)));
                    format!("{name}: tracked frames {}–{}{lost}; {} keys written", o.from, o.to, o.keys)
                }
                Err(e) => format!("{name}: track stopped: {e}"),
            }
        })
        .collect();
    srv.tracks = jobs;
    super::hear::record_pushes(srv, start, "track mask".into());
    notes
}

/// The `track` tool: starts a background track of a mask, or cancels a clip's running ones.
fn run_track(srv: &mut Server, args: &Value) -> Reply {
    let cancel = match opt_bool(args, "cancel") {
        Ok(c) => c,
        Err(e) => return Reply::err(e),
    };
    let Some(raw) = args.get("clip").and_then(Value::as_str) else {
        return Reply::err("missing argument `clip`: the clip to track, such as V1#0");
    };
    let mut notes = settle(srv);
    let s = match srv.session_ref() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (lane, idx) = match super::edit::clip_addr(s, raw) {
        Ok(a) => a,
        Err(e) => return Reply::err(e),
    };
    let name = format!("{}#{idx}", parse::lane_name(lane));
    if cancel {
        let mut stopped = 0;
        for job in srv.tracks.iter().filter(|j| j.lane == lane && j.idx == idx) {
            if track::cancel(job.id) {
                stopped += 1;
            }
        }
        let text = if stopped == 0 { format!("{name}: no track was running") } else { format!("{name}: {stopped} track(s) cancelled") };
        notes.push(text);
        return Reply::ok(notes.join("\n"));
    }
    if lane.kind != LaneKind::Video {
        return Reply::err(format!("{raw} is not a picture clip; only a mask on a video clip can be tracked"));
    }
    let Some(c) = s.lane_clips(lane).get(idx).cloned() else {
        return Reply::err(format!("{raw}: there is no clip there"));
    };
    let slot = match args.get("effect_index").or_else(|| args.get("effect")).and_then(Value::as_u64) {
        Some(n) => n as usize,
        None => return Reply::err("missing `effect_index`: the mask's stack position, 0 for the first effect"),
    };
    match args.get("apply").and_then(Value::as_str) {
        None | Some("mask") => {}
        Some(a) if a.starts_with("title") => {
            return Reply::err("a title cannot be tracked: titles have no keyable position. Track a mask (apply:\"mask\")");
        }
        Some(a) => return Reply::err(format!("apply `{a}` is not \"mask\"")),
    }
    let backward = match args.get("direction").and_then(Value::as_str) {
        None | Some("forward") => false,
        Some("backward") => true,
        Some(d) => return Reply::err(format!("direction `{d}` is not forward or backward")),
    };
    let fps = parse::session_fps(s);
    let len = c.frames();
    let clip_frame = |key: &str, default: u32| -> Result<u32, String> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => parse::frame(v, fps, len),
        }
    };
    let (from_default, to_default) = if backward { (len, 0) } else { (0, len) };
    let (from_off, to_off) = match (clip_frame("from", from_default), clip_frame("to", to_default)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Reply::err(e),
    };
    let from = src_at(&c, c.start + from_off);
    let to = src_at(&c, c.start + to_off);
    if from == to {
        return Reply::err(format!("{raw}: from and to are the same frame; a track needs two"));
    }
    if (to > from) == backward {
        return Reply::err(format!("{raw}: direction {} runs the other way from from to to", if backward { "backward" } else { "forward" }));
    }
    let job: TrackJob = match s.track_mask_start(lane, idx, slot, from, to) {
        Ok(j) => j,
        Err(e) => return Reply::err(format!("track {raw} effect {slot}: {e}")),
    };
    let dir = if backward { "backward" } else { "forward" };
    srv.tracks.push(job);
    notes.push(format!(
        "track {raw} effect {slot}: {dir} from {:.2}s to {:.2}s, analysing in the background; track_status shows its progress and writes the keys",
        parse::secs(c.start + from_off, fps),
        parse::secs(c.start + to_off, fps),
    ));
    Reply::ok(notes.join("\n"))
}

/// The `track_status` tool: running tracks' percent, and the finished ones written.
fn run_status(srv: &mut Server, args: &Value) -> Reply {
    let wait = match opt_bool(args, "wait") {
        Ok(w) => w,
        Err(e) => return Reply::err(e),
    };
    if wait {
        while srv.tracks.iter().any(|j| matches!(track::status(j.id), Status::Running(_))) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let mut lines = settle(srv);
    for job in &srv.tracks {
        if let Status::Running(f) = track::status(job.id) {
            lines.push(format!("{}#{} mask {}: analysing, {:.0}% done", parse::lane_name(job.lane), job.idx, job.slot, f * 100.0));
        }
    }
    if lines.is_empty() {
        return Reply::ok("no track is running or waiting to be written");
    }
    Reply::ok(lines.join("\n"))
}
