//! Multicam: the `multicam_create` and `multicam_flatten` tools and the `angle` edit op. The model
//! (angles as lanes of a compound clip's sequence, sync by sound) is `engine::project::multicam`
//! and `engine::multisync`; this is the argument handling and the replies.

use engine::project::{Lane, LaneKind};
use engine::{AngleSync, MulticamSync, PlaybackSession};
use serde_json::{Value, json};

use super::edit_fx::Applied;
use crate::parse;
use crate::server::{Reply, Server, Tool};

pub const ANGLE_OP_DOC: &str = "angle {clip, at?, angle?, audio?} — switches the angle of a multicam clip (made by multicam_create) from `at` on: the clip is cut there (no cut when `at` is its first frame or left out) and the part after the cut shows `angle`, a number from 1 or an angle name; the cut is one undo step. The sound follows the picture unless `audio` says otherwise: audio:\"follow\" makes it follow again, audio:2 (or a name) keeps it on that angle whatever the picture shows. Give `angle`, `audio` or both. `clip` is the picture clip (or its linked sound clip) of the multicam. Refused when the clip is not a multicam clip, the angle does not exist, or nothing would change. e.g. edit {ops:[{op:\"angle\", clip:\"V1@0\", at:\"12s\", angle:2}]}";

const CREATE_DOC: &str = "Makes a multicam clip from several cameras of one event and puts it on the timeline. sources: 2 to 9 imported files (src:N as media_import reports them; the first is the reference). sync: \"audio\" (default) lines every camera up with the first one that has sound by the shape of its loudness over the first four minutes (a clap, a voice or a song both cameras heard; accurate to 20 ms; a camera that cannot be matched, or has no sound, starts with the others and the reply says so) or \"start\" (all cameras start together). The result is a compound clip of a multicam sequence with one lane per camera: it plays angle 1 until you switch angles with edit {op:\"angle\"}. name? names the sequence (default Multicam); at? where the clip goes (default 0); lane? the video lane (default V1; the sound goes on the audio lane of the same number, linked). It overwrites what is on those lanes there, like place. One undo step. multicam_flatten turns the clips back into plain camera clips.";

const FLATTEN_DOC: &str = "Replaces a multicam clip (give the picture or sound clip as `clip`, e.g. \"V1@0\"), every other piece of the same multicam on this timeline and their linked sound with the plain camera clips they play: each segment becomes the trimmed clip of the angle it showed, the sound of the angle it used. The render does not change. The multicam sequence itself stays (delete it with sequence {action:\"delete\"} once nothing plays it). Refused when another clip is in the way. One undo step.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "multicam_create", description: CREATE_DOC, schema: create_schema, run: create },
        Tool { name: "multicam_flatten", description: FLATTEN_DOC, schema: flatten_schema, run: flatten },
    ]
}

fn create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sources": { "type": "array", "minItems": 2, "maxItems": 9, "items": { "type": ["string", "number"] }, "description": "The cameras: src:N or N for each imported file, 2 to 9. The first is the sync reference." },
            "sync": { "type": "string", "enum": ["audio", "start"], "description": "audio (default): line up by the sound the cameras share. start: all start together." },
            "name": { "type": "string", "maxLength": 100, "description": "The multicam sequence's name. Default Multicam." },
            "at": { "type": ["string", "number"], "description": "Where the clip goes on the timeline. Default 0." },
            "lane": { "type": "string", "description": "The video lane, e.g. V1 (default); the sound goes on the audio lane of the same number." }
        },
        "required": ["sources"]
    })
}

fn flatten_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "clip": { "type": "string", "description": "A multicam clip, such as V1@0 (or its sound clip A1@0)." } },
        "required": ["clip"]
    })
}

/// A source index from `src:N` or a number.
fn source_of(s: &PlaybackSession, v: &Value) -> Result<usize, String> {
    let n = match v {
        Value::Number(n) => n.as_u64().map(|n| n as usize).ok_or("a source is a whole number or src:N")?,
        Value::String(t) => {
            let t = t.trim();
            t.strip_prefix("src:").unwrap_or(t).trim().parse::<usize>().map_err(|_| format!("`{t}` is not a source: write src:N"))?
        }
        other => return Err(format!("a source is src:N, got {}", parse::kind_of(other))),
    };
    if n >= s.sources().len() {
        return Err(format!("there is no src:{n} (media_list shows what is imported)"));
    }
    Ok(n)
}

fn create(srv: &mut Server, args: &Value) -> Reply {
    let before = match srv.session_ref() {
        Ok(s) => s.history_pushes(),
        Err(r) => return r,
    };
    let s = match srv.session() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let list = match args.get("sources") {
        Some(Value::Array(a)) if a.len() >= 2 => a,
        Some(Value::Array(_)) => return Reply::err("multicam_create: a multicam clip needs at least 2 sources"),
        None | Some(Value::Null) => return Reply::err("multicam_create: missing argument `sources`: a list such as [\"src:0\", \"src:1\"]"),
        Some(v) => return Reply::err(format!("multicam_create: `sources` must be a list, got {}", parse::kind_of(v))),
    };
    let mut picks = Vec::new();
    for v in list {
        match source_of(s, v) {
            Ok(n) => picks.push(n),
            Err(e) => return Reply::err(format!("multicam_create: {e}")),
        }
    }
    let sync = match args.get("sync").and_then(Value::as_str) {
        None | Some("audio") => MulticamSync::Audio,
        Some("start") => MulticamSync::Start,
        Some(other) => return Reply::err(format!("multicam_create: sync is audio or start, not `{other}`")),
    };
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let at = match parse::opt_frame_arg(args, "at", fps, end) {
        Ok(a) => a.unwrap_or(0),
        Err(r) => return r,
    };
    let lane = match args.get("lane") {
        None | Some(Value::Null) => Lane::V1,
        Some(Value::String(t)) => match parse::lane(t) {
            Ok(l) => l,
            Err(e) => return Reply::err(format!("multicam_create: {e}")),
        },
        Some(v) => return Reply::err(format!("multicam_create: `lane` must be a name such as V1, got {}", parse::kind_of(v))),
    };
    if let Err(e) = super::edit::check_unlocked(s, &[lane, Lane::new(LaneKind::Audio, lane.ord)]) {
        return Reply::err(format!("multicam_create: {e}"));
    }
    let name = args.get("name").and_then(Value::as_str);
    let report = match s.make_multicam_at(&picks, sync, name, lane, parse::secs(at, fps)) {
        Ok(r) => r,
        Err(e) => return Reply::err(format!("multicam_create: {e}")),
    };
    let title = s.sequence_name(report.made.sequence).unwrap_or_default();
    let names = s.angle_names(report.made.sequence).unwrap_or_default();
    let mut lines = vec![format!(
        "multicam \"{title}\" (sequence {}): {} angles, {}",
        report.made.sequence,
        picks.len(),
        parse::clock(s.sequences().iter().find(|q| q.id == report.made.sequence).map_or(0, |q| q.frames), fps)
    )];
    let mut angles = Vec::new();
    for (i, src) in picks.iter().enumerate() {
        let how = match report.how[i] {
            AngleSync::Reference => "sync reference".to_string(),
            AngleSync::Audio { score } => format!("synced by sound, match {score:.2}, {:+.2} s against angle 1", report.offsets_secs[i]),
            AngleSync::Start => "placed at the start".to_string(),
            AngleSync::Weak { score } => format!("sound did not match well enough ({score:.2}): placed at the start"),
            AngleSync::NoSound => "no sound: placed at the start".to_string(),
        };
        lines.push(format!(
            "  angle {} \"{}\" src:{src}: starts at {} — {how}",
            i + 1,
            names.get(i).cloned().unwrap_or_default(),
            parse::clock(report.starts[i], fps)
        ));
        angles.push(json!({
            "angle": i + 1,
            "name": names.get(i).cloned().unwrap_or_default(),
            "source": format!("src:{src}"),
            "start_frames": report.starts[i],
            "sync": match report.how[i] {
                AngleSync::Reference => "reference",
                AngleSync::Audio { .. } => "audio",
                AngleSync::Start => "start",
                AngleSync::Weak { .. } => "weak",
                AngleSync::NoSound => "no_sound",
            },
            "offset_secs": report.offsets_secs[i],
        }));
    }
    let clips: Vec<String> = report
        .made
        .clips
        .iter()
        .filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| parse::addr(l, c.start)))
        .collect();
    lines.push(format!("placed as {}; it shows angle 1 — switch with edit {{op:\"angle\", clip, at, angle}}", clips.join(" + ")));
    let out = Reply::ok(lines.join("\n")).with(json!({
        "sequence": report.made.sequence,
        "name": title,
        "clips": clips,
        "angles": angles,
    }));
    super::project::note_pushes_as(srv, before, "multicam_create");
    out
}

/// The picture clip of the multicam that `clip` (a picture or its linked sound) belongs to.
fn picture_of(s: &PlaybackSession, lane: Lane, idx: usize) -> Result<Lane, String> {
    if lane.kind == LaneKind::Video {
        return Ok(lane);
    }
    let c = s.lane_clips(lane).get(idx).ok_or("there is no such clip")?;
    let link = c.link.ok_or("that sound clip is not linked to a picture clip: give the picture clip")?;
    s.lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Video)
        .find(|&l| s.lane_clips(l).iter().any(|o| o.link == Some(link) && o.source == c.source))
        .ok_or_else(|| "no picture clip is linked to that sound clip: give the picture clip".to_string())
}

/// An angle by 1-based number or name.
fn angle_of(names: &[String], v: &Value, what: &str) -> Result<u8, String> {
    let list = || names.iter().enumerate().map(|(i, n)| format!("{} \"{n}\"", i + 1)).collect::<Vec<_>>().join(", ");
    let found = match v {
        Value::Number(n) => n.as_u64().and_then(|n| n.checked_sub(1)).map(|n| n as usize).filter(|n| *n < names.len()),
        Value::String(t) => {
            let t = t.trim();
            names
                .iter()
                .position(|n| n == t)
                .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(t)))
                .or_else(|| t.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).filter(|n| *n < names.len()))
        }
        other => return Err(format!("`{what}` is an angle number or name, got {}", parse::kind_of(other))),
    };
    found.map(|n| n as u8).ok_or_else(|| format!("no angle {v}; the angles are {}", list()))
}

/// The `angle` edit op.
pub fn angle(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let raw = parse::addr_str(op, "clip", "\"V1@0\"")?;
    let (lane, idx) = super::edit::clip_addr(s, raw)?;
    let vlane = picture_of(s, lane, idx).map_err(|e| format!("angle {raw}: {e}"))?;
    super::edit::check_unlocked(s, &[lane, vlane])?;
    let Some((_, names)) = s.multicam_at(lane, idx) else {
        return Err(format!("angle {raw}: {raw} is not a multicam clip (multicam_create makes one)"));
    };
    let video = match op.get("angle") {
        None | Some(Value::Null) => None,
        Some(v) => Some(angle_of(&names, v, "angle").map_err(|e| format!("angle {raw}: {e}"))?),
    };
    let audio = match op.get("audio") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if t.trim().eq_ignore_ascii_case("follow") => Some(None),
        Some(v) => Some(Some(angle_of(&names, v, "audio").map_err(|e| format!("angle {raw}: {e}"))?)),
    };
    if video.is_none() && audio.is_none() {
        return Err(format!("angle {raw}: give `angle` (the picture), `audio` (follow or an angle), or both"));
    }
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let start = s.lane_clips(lane).get(idx).map_or(0, |c| c.start);
    let at = super::edit::opt_time(op, "at", fps, end)?.unwrap_or(start);
    // The clip addressed must be the one under `at`: its picture lane is where the cut is looked for.
    let touched = s.switch_angle_at(Some(vlane), parse::secs(at, fps), video, audio).map_err(|e| format!("angle {raw}: {e}"))?;
    let shown = |a: Option<u8>| a.map_or(String::new(), |a| format!("{} \"{}\"", a + 1, names[usize::from(a)]));
    let mut what = Vec::new();
    if video.is_some() {
        what.push(format!("picture {}", shown(video)));
    }
    match audio {
        Some(None) => what.push("sound follows the picture".to_string()),
        Some(Some(a)) => what.push(format!("sound {}", shown(Some(a)))),
        None => {}
    }
    let at_text = parse::clock(at, fps);
    let list: Vec<String> = touched.iter().map(|&(l, st)| parse::addr(l, st)).collect();
    Ok(Applied { text: format!("angle from {at_text}: {} → {}", what.join(", "), list.join(", ")), touched })
}

fn flatten(srv: &mut Server, args: &Value) -> Reply {
    let before = match srv.session_ref() {
        Ok(s) => s.history_pushes(),
        Err(r) => return r,
    };
    let s = match srv.session() {
        Ok(s) => s,
        Err(r) => return r,
    };
    let raw = match parse::addr_str(args, "clip", "\"V1@0\"") {
        Ok(r) => r,
        Err(e) => return Reply::err(format!("multicam_flatten: {e}")),
    };
    let (lane, idx) = match super::edit::clip_addr(s, raw) {
        Ok(x) => x,
        Err(e) => return Reply::err(format!("multicam_flatten: {e}")),
    };
    if let Err(e) = super::edit::check_unlocked(s, &s.lanes()) {
        return Reply::err(format!("multicam_flatten: {e}"));
    }
    match s.flatten_multicam(lane, idx) {
        Ok(made) => {
            let names: Vec<String> = made.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| parse::addr(l, c.start))).collect();
            let out = Reply::ok(format!("multicam_flatten → {} plain clip(s): {}", names.len(), names.join(", ")))
                .with(json!({ "clips": names }));
            super::project::note_pushes_as(srv, before, "multicam_flatten");
            out
        }
        Err(e) => Reply::err(format!("multicam_flatten: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A 4 s clip of `pattern` with a 0.5 s tone burst at `burst` seconds.
    fn camera(dir: &Path, name: &str, pattern: &str, burst: f64) -> PathBuf {
        let out = dir.join(name);
        let sound = format!(
            "aevalsrc='0.02*sin(2*PI*220*t)+if(between(t,{burst},{}),0.5*sin(2*PI*1000*t),0)+if(between(t,{},{}),0.4*sin(2*PI*600*t),0)':s=44100:d=4",
            burst + 0.5,
            burst + 1.6,
            burst + 1.9
        );
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("{pattern}=s=160x90:r=25:d=4"), "-f", "lavfi", "-i", &sound])
            .args(["-ac", "2", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&out)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok, "ffmpeg made {name}");
        out
    }

    fn rig() -> (Server, engine::scratch::Scratch) {
        engine::ao::set_silent_output();
        let dir = engine::scratch::Scratch::dir("ve_mcp_multicam");
        let a = camera(&dir, "cam_a.mp4", "testsrc2", 0.8);
        let b = camera(&dir, "cam_b.mp4", "smptebars", 1.7);
        let mut s = PlaybackSession::open(a).expect("open");
        s.import(&b).expect("import");
        (Server { session: Some(s), ..Server::default() }, dir)
    }

    fn edit(srv: &mut Server, ops: Value) -> Reply {
        crate::tools::edit::run_edit(srv, &json!({ "ops": ops }))
    }

    #[test]
    fn docs_name_their_arguments() {
        for w in ["clip", "at", "angle", "audio", "follow", "undo step"] {
            assert!(ANGLE_OP_DOC.contains(w), "{w}");
        }
        for w in ["sources", "sync", "audio", "start", "name", "lane"] {
            assert!(CREATE_DOC.contains(w), "{w}");
        }
        assert!(FLATTEN_DOC.contains("clip"));
    }

    #[test]
    fn create_syncs_by_sound_then_angle_switches_and_flatten_restores() {
        let (mut srv, _dir) = rig();
        let r = create(&mut srv, &json!({"sources": ["src:0", 1], "name": "Show"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("synced by sound") && r.text.contains("+0.90 s"), "{}", r.text);
        let st = r.structured.as_deref().expect("structured");
        assert_eq!(st["angles"][1]["sync"], "audio");
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.sequence_name(st["sequence"].as_u64().unwrap() as u32).as_deref(), Some("Show"));
        let r = edit(&mut srv, json!([{"op": "angle", "clip": "V1@0", "at": "2s", "angle": 2}]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cam_b"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1).len(), 2);
        assert_eq!(s.lane_clips(Lane::A1).len(), 2);
        // The sound clip address works too, a name works, and the sound can be pinned.
        let r = edit(&mut srv, json!([{"op": "angle", "clip": "A1@50", "at": "3s", "angle": "cam_a", "audio": 2}]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::A1).last().map(|c| c.angle.audio), Some(Some(1)));
        // Refusals say why.
        for (op, word) in [
            (json!({"op": "angle", "clip": "V1@0", "angle": 9}), "no angle"),
            (json!({"op": "angle", "clip": "V1@0"}), "give `angle`"),
            (json!({"op": "angle", "clip": "V1@0", "angle": 1}), "already"),
            (json!({"op": "angle", "clip": "V1@900", "angle": 1}), "V1@900"),
        ] {
            let r = edit(&mut srv, json!([op]));
            assert!(r.is_error && r.text.contains(word), "{word}: {}", r.text);
        }
        let tl = crate::rpc::handle(&mut srv, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "timeline", "arguments": {}}}));
        let tl = tl.expect("reply")["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        assert!(tl.contains("multicam \"Show\"") && tl.contains("angle 2"), "{tl}");
        let r = flatten(&mut srv, &json!({"clip": "V1@0"}));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(Lane::V1).iter().all(|c| !engine::project::is_sequence_source(c.source)));
        assert_eq!(s.lane_clips(Lane::V1).len(), 3);
        assert!(flatten(&mut srv, &json!({"clip": "V1@0"})).is_error, "nothing multicam left");
    }

    #[test]
    fn create_refuses_what_it_cannot_do_by_name() {
        let (mut srv, _dir) = rig();
        for (args, word) in [
            (json!({}), "sources"),
            (json!({"sources": ["src:0"]}), "at least 2"),
            (json!({"sources": ["src:0", "src:7"]}), "src:7"),
            (json!({"sources": ["src:0", "src:0"]}), "twice"),
            (json!({"sources": [0, 1], "sync": "clap"}), "audio or start"),
            (json!({"sources": [0, 1], "lane": "A1"}), "video lane"),
        ] {
            let r = create(&mut srv, &args);
            assert!(r.is_error && r.text.contains(word), "{word}: {}", r.text);
        }
        assert_eq!(srv.session.as_ref().unwrap().sequences().len(), 1, "a refusal changes nothing");
        let r = create(&mut srv, &json!({"sources": [0, 1], "sync": "start"}));
        assert!(!r.is_error && r.text.contains("placed at the start"), "{}", r.text);
    }

    #[test]
    fn create_is_one_undo_step() {
        let (mut srv, _dir) = rig();
        let s = srv.session.as_ref().unwrap();
        let before = s.undo_depth();
        let r = create(&mut srv, &json!({"sources": [0, 1]}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), before + 1);
        assert_eq!(srv.ledger.last().map(|e| e.label.as_str()), Some("multicam_create"));
    }
}
