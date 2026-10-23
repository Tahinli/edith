//! Sequences and compound clips: the `sequence` tool (list, new, rename, duplicate, delete,
//! switch), the `nest`, `break_apart` and `place_sequence` edit ops, and the optional `sequence`
//! argument of `timeline`, `edit`, `export_start` and `clip_info`. The model is `engine::project`'s
//! sequences; this is the argument handling and the replies.

use engine::PlaybackSession;
use engine::project::Lane;
use serde_json::{Value, json};

use super::edit_fx::Applied;
use crate::parse;
use crate::server::{Reply, Server, Tool};

pub const NEST_OP_DOC: &str = "nest {clips:[addr,...], name?} — folds the picked clips (and their linked partners) into a new sequence and puts one compound clip where they were, on the lowest picked video lane and the lowest picked sound lane, linked when both exist; the picture and sound are unchanged. The clips keep their lanes (compacted) and their timing relative to the first. Refuses when a clip that is not picked sits between picked ones on a lane. One undo step. The new sequence is named `name` or Compound N; open it with sequence {action:\"switch\"}.";
pub const BREAK_APART_OP_DOC: &str = "break_apart {clips:[addr,...]} — the opposite of nest: each picked compound clip (and its linked partner) is replaced by the clips of its sequence, trimmed to the stretch the compound played, on the same lanes (extra lanes are made as needed). The sequence itself stays. Refuses when other clips are in the way. One undo step.";
pub const PLACE_SEQUENCE_OP_DOC: &str = "place_sequence {sequence, at, lane?=V1} — puts a sequence on a video lane at `at` as a compound clip (its sound goes on the first sound lane, linked). A sequence cannot hold itself or one that holds it (cycle: refused). A compound clip has no speed, ramp or reverse (refused); trims, splits, moves, fades and gain work as for any clip.";

const DOC: &str = "Sequences: a project holds several named sequences (timelines) that share its size and frame rate; one is active and every other tool works on it. action: list (default) · new {name?} · rename {sequence, new_name} · duplicate {sequence, name?} · delete {sequence} · switch {sequence}. `sequence` is a name or the number list shows. new and duplicate do not switch; delete refuses the last sequence and one a compound clip plays; switching is not an undo step but new, rename, duplicate and delete are. A sequence can play inside another as a compound clip (edit ops nest, break_apart, place_sequence). timeline, edit, export_start and clip_info take an optional `sequence` to act on another one (edit leaves it active). The frame rate cannot change once there is more than one sequence.";

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "sequence", description: DOC, schema, run }]
}

/// The `sequence` property the other tools share.
pub fn prop() -> Value {
    json!({ "type": ["string", "number"], "description": "A sequence by name or number (sequence {action:\"list\"}). Default: the active one." })
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["list", "new", "rename", "duplicate", "delete", "switch"], "description": "Default list." },
            "sequence": { "type": ["string", "number"], "description": "The sequence to act on, by name or number." },
            "name": { "type": "string", "maxLength": 100, "description": "new and duplicate: the new sequence's name." },
            "new_name": { "type": "string", "maxLength": 100, "description": "rename: the new name." }
        }
    })
}

/// A sequence named by `v`: a number, a digit string, or a name.
pub fn resolve(s: &PlaybackSession, v: &Value) -> Result<u32, String> {
    let all = s.sequences();
    let list = || all.iter().map(|q| format!("{} {}", q.id, q.name)).collect::<Vec<_>>().join(", ");
    let by_id = |n: u64| u32::try_from(n).ok().filter(|n| all.iter().any(|q| q.id == *n));
    let found = match v {
        Value::Number(n) => n.as_u64().and_then(by_id),
        Value::String(t) => s.sequence_named(t).or_else(|| t.trim().parse::<u64>().ok().and_then(by_id)),
        _ => return Err(format!("`sequence` must be a name or a number, got {}", parse::kind_of(v))),
    };
    found.ok_or_else(|| format!("no sequence {v}; the sequences are: {}", list()))
}

fn line(q: &engine::project::SequenceInfo, fps: f64) -> String {
    let used = if q.used > 0 { format!(", played by {} compound clip(s)", q.used) } else { String::new() };
    format!("{}{} {} \"{}\" {}{used}", if q.active { "*" } else { " " }, "seq", q.id, q.name, parse::clock(q.frames, fps))
}

fn listing(s: &PlaybackSession) -> String {
    let fps = parse::session_fps(s);
    let rows: Vec<String> = s.sequences().iter().map(|q| line(q, fps)).collect();
    format!("{} sequence(s), * is active:\n{}", rows.len(), rows.join("\n"))
}

fn run(srv: &mut Server, args: &Value) -> Reply {
    let before = match srv.session_ref() {
        Ok(s) => s.history_pushes(),
        Err(r) => return r,
    };
    let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
    let text = |k: &str| args.get(k).and_then(Value::as_str);
    let s = match srv.session() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let which = |s: &PlaybackSession| match args.get("sequence").filter(|v| !v.is_null()) {
        Some(v) => resolve(s, v),
        None => Err("missing argument `sequence`: a name or a number from sequence {action:\"list\"}".into()),
    };
    let done: Result<String, String> = match action {
        "list" => return Reply::ok(listing(s)),
        "new" => s.new_sequence(text("name")).map_err(|e| e.to_string()).map(|id| {
            format!("new sequence {id} \"{}\" (not switched to; sequence {{action:\"switch\", sequence:{id}}} opens it)", s.sequence_name(id).unwrap_or_default())
        }),
        "rename" => which(s).and_then(|id| {
            let name = text("new_name").ok_or("missing argument `new_name`")?;
            s.rename_sequence(id, name).map_err(|e| e.to_string())?;
            Ok(format!("sequence {id} is now \"{}\"", s.sequence_name(id).unwrap_or_default()))
        }),
        "duplicate" => which(s).and_then(|id| {
            let copy = s.duplicate_sequence(id).map_err(|e| e.to_string())?;
            if let Some(n) = text("name") {
                s.rename_sequence(copy, n).map_err(|e| e.to_string())?;
            }
            Ok(format!("sequence {id} copied to {copy} \"{}\"", s.sequence_name(copy).unwrap_or_default()))
        }),
        "delete" => which(s).and_then(|id| {
            let now = s.delete_sequence(id).map_err(|e| e.to_string())?;
            Ok(format!("sequence {id} deleted; sequence {now} is active"))
        }),
        "switch" => which(s).and_then(|id| {
            s.switch_sequence(id).map_err(|e| e.to_string())?;
            Ok(format!("sequence {id} \"{}\" is active", s.sequence_name(id).unwrap_or_default()))
        }),
        other => Err(format!("unknown action `{other}`; list, new, rename, duplicate, delete or switch")),
    };
    match done {
        Ok(t) => {
            super::project::note_pushes_as(srv, before, &format!("sequence {action}"));
            Reply::ok(t)
        }
        Err(e) => Reply::err(format!("sequence {action}: {e}")),
    }
}

/// Makes `args.sequence` the active sequence for one call. Returns the one to go back to when the
/// call is a read (`keep`), else `None`. A refusal is the reply to give.
pub fn enter(srv: &mut Server, args: &Value, keep: bool) -> Result<Option<u32>, Reply> {
    let Some(v) = args.get("sequence").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let s = srv.session()?;
    let id = resolve(s, v).map_err(Reply::err)?;
    let was = s.active_sequence();
    if id == was {
        return Ok(None);
    }
    s.switch_sequence(id).map_err(|e| Reply::err(e.to_string()))?;
    Ok(keep.then_some(was))
}

/// Goes back to the sequence `enter` left.
pub fn leave(srv: &mut Server, back: Option<u32>) {
    if let (Some(id), Some(s)) = (back, srv.session.as_mut()) {
        let _ = s.switch_sequence(id);
    }
}

fn clips_arg(s: &PlaybackSession, op: &Value, who: &str) -> Result<Vec<(Lane, usize)>, String> {
    let list = match op.get("clips") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        None | Some(Value::Null) => return Err("missing argument `clips`".into()),
        Some(_) => return Err(format!("{who}: `clips` must be a non-empty list of addresses such as [\"V1@0\", \"V1@90\"]")),
    };
    let mut out = Vec::new();
    for v in list {
        let raw = v.as_str().ok_or_else(|| format!("{who}: each clip is an address string, got {}", parse::kind_of(v)))?;
        let (lane, idx) = super::edit::clip_addr(s, raw)?;
        super::edit::check_unlocked(s, &[lane])?;
        out.push((lane, idx));
    }
    Ok(out)
}

fn touched(s: &PlaybackSession, clips: &[(Lane, usize)]) -> Vec<(Lane, u32)> {
    clips.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start))).collect()
}

fn names(s: &PlaybackSession, t: &[(Lane, u32)]) -> String {
    let _ = s;
    t.iter().map(|&(l, st)| parse::addr(l, st)).collect::<Vec<_>>().join(", ")
}

/// The `nest` edit op.
pub fn nest(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let picks = clips_arg(s, op, "nest")?;
    let name = op.get("name").and_then(Value::as_str);
    let made = s.nest(&picks, name).map_err(|e| format!("nest: {e}"))?;
    let t = touched(s, &made.clips);
    let title = s.sequence_name(made.sequence).unwrap_or_default();
    Ok(Applied { text: format!("nest → sequence {} \"{title}\" as {}", made.sequence, names(s, &t)), touched: t })
}

/// The `break_apart` edit op.
pub fn break_apart(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let picks = clips_arg(s, op, "break_apart")?;
    let out = s.break_apart(&picks).map_err(|e| format!("break_apart: {e}"))?;
    let t = touched(s, &out);
    Ok(Applied { text: format!("break_apart → {} clip(s): {}", t.len(), names(s, &t)), touched: t })
}

/// The `place_sequence` edit op.
pub fn place_sequence(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let seq = resolve(s, op.get("sequence").ok_or("missing argument `sequence`")?)?;
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let at = super::edit::opt_time(op, "at", fps, end)?.ok_or("missing argument `at`")?;
    let lane = match op.get("lane") {
        None | Some(Value::Null) => Lane::V1,
        Some(Value::String(t)) => parse::lane(t)?,
        Some(v) => return Err(format!("`lane` must be a lane name such as \"V1\", got {}", parse::kind_of(v))),
    };
    super::edit::check_unlocked(s, &[lane])?;
    s.place_sequence_at(seq, lane, parse::secs(at, fps)).map_err(|e| format!("place_sequence: {e}"))?;
    Ok(Applied {
        text: format!("place_sequence → sequence {seq} at {}", parse::addr(lane, at)),
        touched: vec![(lane, at)],
    })
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

    #[test]
    fn docs_name_their_arguments() {
        for (doc, words) in [
            (NEST_OP_DOC, &["clips", "name", "undo step"][..]),
            (BREAK_APART_OP_DOC, &["clips", "undo step"][..]),
            (PLACE_SEQUENCE_OP_DOC, &["sequence", "at", "cycle"][..]),
            (DOC, &["list", "new", "rename", "duplicate", "delete", "switch"][..]),
        ] {
            for w in words {
                assert!(doc.contains(w), "{w}");
            }
        }
    }

    #[test]
    fn the_tool_manages_sequences_with_undo() {
        let mut srv = open();
        let first = run(&mut srv, &json!({}));
        assert!(first.text.contains("1 sequence(s)"), "{}", first.text);
        assert!(!run(&mut srv, &json!({"action": "new", "name": "Scratch"})).is_error);
        assert!(run(&mut srv, &json!({"action": "list"})).text.contains("\"Scratch\""));
        assert!(!run(&mut srv, &json!({"action": "rename", "sequence": "Scratch", "new_name": "B"})).is_error);
        assert!(!run(&mut srv, &json!({"action": "duplicate", "sequence": "B", "name": "C"})).is_error);
        assert!(!run(&mut srv, &json!({"action": "switch", "sequence": "C"})).is_error);
        assert!(run(&mut srv, &json!({"action": "list"})).text.contains("*seq"));
        let gone = run(&mut srv, &json!({"action": "delete", "sequence": "nope"}));
        assert!(gone.is_error && gone.text.contains("the sequences are"), "{}", gone.text);
        assert!(!run(&mut srv, &json!({"action": "delete", "sequence": "C"})).is_error);
        assert!(!run(&mut srv, &json!({"action": "delete", "sequence": 1})).is_error);
        let last = run(&mut srv, &json!({"action": "delete", "sequence": 0}));
        assert!(last.is_error, "{}", last.text);
        assert_eq!(srv.session.as_ref().unwrap().sequences().len(), 1);
    }

    #[test]
    fn nest_and_break_apart_are_edit_ops_and_one_undo_each() {
        let mut srv = open();
        let r = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "nest", "clips": ["V1#0"], "name": "Inner"}]}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("Inner"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sequences().len(), 2);
        let r = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "break_apart", "clips": ["V1#0"]}]}));
        assert!(!r.is_error, "{}", r.text);
        let r = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "nest", "clips": []}]}));
        assert!(r.is_error);
        let cyc = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "place_sequence", "sequence": 0, "at": 0}]}));
        assert!(cyc.is_error, "{}", cyc.text);
    }

    #[test]
    fn the_sequence_argument_reads_another_and_comes_back() {
        let mut srv = open();
        run(&mut srv, &json!({"action": "new", "name": "B"}));
        let back = enter(&mut srv, &json!({"sequence": "B"}), true).ok().expect("enter");
        assert_eq!(srv.session.as_ref().unwrap().sequence_name(srv.session.as_ref().unwrap().active_sequence()).as_deref(), Some("B"));
        leave(&mut srv, back);
        assert_eq!(srv.session.as_ref().unwrap().active_sequence(), 0);
        assert!(enter(&mut srv, &json!({"sequence": "zzz"}), true).is_err());
        let _ = run(&mut srv, &json!({"action": "duplicate", "sequence": 0}));
        let r = crate::tools::project::project_settings(&mut srv, &json!({"fps": 5})).text;
        assert!(r.contains("several sequences"), "{r}");
    }

    #[test]
    fn the_timeline_names_a_compound_clip_and_reads_another_sequence() {
        let mut srv = open();
        let r = crate::tools::edit::run_edit(&mut srv, &json!({"ops": [{"op": "nest", "clips": ["V1#0"], "name": "Inner"}]}));
        assert!(!r.is_error, "{}", r.text);
        let call = |srv: &mut Server, args: Value| {
            let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "timeline", "arguments": args}});
            crate::rpc::handle(srv, msg).expect("a reply")["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
        };
        let tl = call(&mut srv, json!({}));
        assert!(tl.contains("compound \"Inner\""), "{tl}");
        let inner = call(&mut srv, json!({"sequence": "Inner"}));
        assert!(inner.contains(".mp4"), "{inner}");
        assert_eq!(srv.session.as_ref().unwrap().active_sequence(), 0, "a read leaves the active sequence");
    }
}
