//! Video stabilisation: the `stabilize` edit op (on with a strength and a crop, or off) and the
//! `stabilize_status` tool (where each stabilised clip's analysis stands). The analysis and the
//! correction are `engine::stabilise`; this is the argument handling and the replies.

use engine::PlaybackSession;
use engine::project::{Lane, LaneKind};
use engine::stabilise::{self, Crop, Stabilise, Status};
use serde_json::{Value, json};

use super::edit_fx::Applied;
use crate::parse;
use crate::server::{Reply, Server, Tool};

pub const STABILIZE_OP_DOC: &str = "stabilize {clip, on?, strength?, crop?, off?, cancel?} — takes the camera shake out of a video clip. \
     The shake is measured from the clip's own footage (corners tracked frame to frame, a shift-turn-zoom fitted to them), \
     the camera path is smoothed, and each frame is moved to the smoothed path; a steady pan or tilt is kept, the jitter around it goes. \
     strength 0 to 1 (default 0.5): how smooth, about strength × 2 seconds of smoothing. \
     crop: \"zoom\" (default) enlarges the picture just enough that no frame shows an edge (at most 1.5×); \"full\" keeps the frame's size and lets black edges show where it moved. \
     off:true (or on:false) takes it off; cancel:true stops a running analysis. \
     The analysis runs in the background the first time (stabilize_status shows its progress) and is cached per source, so re-opening the project does not redo it; until it finishes the clip plays unstabilised. \
     It applies before the clip's transform, and follows the footage through trims, speed, ramps and reverse. An export measures it first if it is missing (export_status shows Measuring shake). \
     e.g. edit {ops:[{op:\"stabilize\", clip:\"V1@0\", strength:0.6}]}";

const STATUS_DOC: &str = "Where each stabilised clip's analysis stands: ready, running with its percent, failed with why, or not started. clip limits it to one clip (V1@0); wait:true blocks until every listed analysis has finished (an export measures a missing one itself, as its Measuring shake stage).";

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "stabilize_status", description: STATUS_DOC, schema: status_schema, run: run_status }]
}

fn status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "clip": { "type": "string", "description": "One clip, such as V1@0 or V1#0. Default: every stabilised clip." },
            "wait": { "type": "boolean", "description": "Block until the analyses have finished. Default false." }
        }
    })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn opt_bool(op: &Value, key: &str) -> Result<Option<bool>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(v) => Err(format!("`{key}` must be true or false, got {}", kind_of(v))),
    }
}

fn status_words(st: &Status) -> String {
    match st {
        Status::Ready => "analysis ready".into(),
        Status::Running(f) => format!("analysing, {:.0}% done", f * 100.0),
        Status::Failed(e) => format!("analysis failed: {e}"),
        Status::Missing => "analysis not started".into(),
    }
}

fn settings_words(st: Stabilise) -> String {
    let crop = match st.crop() {
        Crop::Zoom => "zoom to hide edges",
        Crop::Full => "keep full frame",
    };
    format!("strength {}, crop {} ({crop})", st.amount(), st.crop().name())
}

/// The `stabilize` edit op.
pub fn op(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let raw = match op.get("clip") {
        Some(Value::String(c)) => c.as_str(),
        None | Some(Value::Null) => return Err("missing argument `clip`".into()),
        Some(v) => return Err(format!("`clip` must be a string, got {}", kind_of(v))),
    };
    let (lane, idx) = super::edit::clip_addr(s, raw)?;
    super::edit::check_unlocked(s, &[lane])?;
    let who = format!("stabilize {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: stabilisation is for video clips, and {raw} is not one"));
    }
    let start = s.lane_clips(lane).get(idx).map_or(0, |c| c.start);
    if s.stabilise_range(lane, idx).is_none() {
        return Err(format!("{who}: a still or a song has no camera motion to take out"));
    }
    if opt_bool(op, "cancel")? == Some(true) {
        let stopped = s.stabilise_cancel(lane, idx);
        let what = if stopped { "analysis cancelled" } else { "no analysis was running" };
        return Ok(Applied { text: format!("{who} → {what}"), touched: vec![(lane, start)] });
    }
    let off = opt_bool(op, "off")? == Some(true) || opt_bool(op, "on")? == Some(false);
    if off {
        if s.stabilise_of(lane, idx).is_none() {
            return Ok(Applied { text: format!("{who} → already off (no change)"), touched: vec![(lane, start)] });
        }
        s.set_stabilise(lane, idx, None);
        return Ok(Applied { text: format!("{who} → off"), touched: vec![(lane, start)] });
    }
    let mut st = s.stabilise_of(lane, idx).unwrap_or_default();
    match op.get("strength") {
        None | Some(Value::Null) => {}
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(f64::NAN);
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{who}: strength {v} out of range 0–1"));
            }
            st = Stabilise::new(v as f32, st.crop());
        }
        Some(v) => return Err(format!("{who}: `strength` must be a number from 0 to 1, got {}", kind_of(v))),
    }
    match op.get("crop") {
        None | Some(Value::Null) => {}
        Some(Value::String(c)) => {
            let crop = Crop::from_name(c).ok_or_else(|| format!("{who}: crop `{c}` is not zoom or full"))?;
            st = Stabilise::with_percent(st.strength(), crop);
        }
        Some(v) => return Err(format!("{who}: `crop` must be \"zoom\" or \"full\", got {}", kind_of(v))),
    }
    let changed = s.stabilise_of(lane, idx) != Some(st);
    if changed && !s.set_stabilise(lane, idx, Some(st)) {
        return Err(format!("{who}: the engine refused it (is the clip there?)"));
    }
    let status = s.stabilise_start(lane, idx).unwrap_or(Status::Missing);
    let tail = if changed { "" } else { " (no change)" };
    Ok(Applied {
        text: format!("{who} → on, {}; {}{tail}", settings_words(st), status_words(&status)),
        touched: vec![(lane, start)],
    })
}

/// The line a still or a sheet over timeline frames `from..=to` adds when a
/// stabilised clip there has no finished analysis yet: it is drawn unstabilised,
/// and its analysis runs in the background (the compose started it).
pub fn pending_in(s: &PlaybackSession, from: u32, to: u32) -> Option<String> {
    let names: Vec<String> = stabilised(s)
        .into_iter()
        .filter(|&(lane, idx)| {
            let c = &s.lane_clips(lane)[idx];
            c.start <= to && from < c.end() && !matches!(s.stabilise_status(lane, idx), Some(Status::Ready) | None)
        })
        .map(|(lane, idx)| format!("{}#{idx}", parse::lane_name(lane)))
        .collect();
    (!names.is_empty()).then(|| {
        format!(
            "stabilisation analysis pending for {}: drawn unstabilised here; stabilize_status {{wait:true}} waits for it (an export measures it itself)",
            names.join(", ")
        )
    })
}

/// Every stabilised clip as (lane, index).
fn stabilised(s: &PlaybackSession) -> Vec<(Lane, usize)> {
    s.lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Video)
        .flat_map(|l| {
            s.lane_clips(l)
                .iter()
                .enumerate()
                .filter(|(_, c)| c.stabilise.is_some())
                .map(move |(i, _)| (l, i))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn run_status(srv: &mut Server, args: &Value) -> Reply {
    let s = match srv.session_ref() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let wait = match opt_bool(args, "wait") {
        Ok(w) => w.unwrap_or(false),
        Err(e) => return Reply::err(e),
    };
    let clips = match args.get("clip").and_then(Value::as_str) {
        None => stabilised(s),
        Some(raw) => match super::edit::clip_addr(s, raw) {
            Ok(c) => vec![c],
            Err(e) => return Reply::err(e),
        },
    };
    if clips.is_empty() {
        return Reply::ok("no clip is stabilised: edit {ops:[{op:\"stabilize\", clip:\"V1@0\"}]} turns it on");
    }
    let mut lines = Vec::new();
    for (lane, idx) in clips {
        let name = format!("{}#{idx}", parse::lane_name(lane));
        let Some(st) = s.stabilise_of(lane, idx) else {
            lines.push(format!("{name}: not stabilised"));
            continue;
        };
        let Some((path, lo, hi)) = s.stabilise_range(lane, idx) else {
            lines.push(format!("{name}: not footage"));
            continue;
        };
        let status = if wait { stabilise::wait(&path, lo, hi) } else { stabilise::status(&path, lo, hi) };
        let zoom = match (&status, stabilise::correction(&path, lo, hi, st, s.meta().frame_rate)) {
            (Status::Ready, Some(c)) => format!(", zoom {:.3}×", c.zoom),
            _ => String::new(),
        };
        lines.push(format!("{name}: {}; {}{zoom} (source frames {lo}–{})", settings_words(st), status_words(&status), hi - 1));
    }
    Reply::ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_op_doc_names_every_argument() {
        for arg in ["strength", "crop", "off", "cancel", "zoom", "full"] {
            assert!(STABILIZE_OP_DOC.contains(arg), "{arg}");
        }
    }
}
