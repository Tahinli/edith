//! The `effect` tool: a video clip's effects stack (blur, sharpen, vignette, glow, shake,
//! chroma key, blend, masks) -- add, set, remove, move and list. The pixels are
//! `engine::effects`; this is the argument handling and the replies. Every mutating op is one
//! `set_effects` call, so one undo step.

use engine::PlaybackSession;
use engine::effects::{BlendMode, Effect, EffectKind, MAX_EFFECTS};
use engine::project::{Lane, LaneKind};
use serde_json::{Map, Value, json};

use super::hear::record_pushes;
use crate::parse;
use crate::server::{Reply, Server, Tool, opt_str};

const NO_PROJECT: &str = "no project open — call project_open or project_new first";

const EFFECT_DOC: &str = r##"A video clip's effects stack: ops add, set, remove, move, list. The stack applies in order on the clip's placed picture (after its colour, fit and transform), in preview and in every export; a masked or keyed-out part shows the lanes below. Every mutating op is one undo step. Video lanes only; at most 16 effects per clip.
- add {clip, kind, at?, params?, size?, color?, mode?}: a new effect at stack position `at` (default the end), at its defaults unless params say otherwise. size is pixelate's block size, the same as params {size}.
- set {clip, index, params?, on?, color?, mode?}: changes the effect at stack position `index`; what is not named keeps its value. on:false switches it off without losing its settings.
- remove {clip, index} · move {clip, from, to} (stack positions; `to` is where it lands).
- list {clip}: the stack with each effect's position, kind, on/off and named values. list with no clip: the kinds with their parameters and ranges.
params is {name: number}; a number outside a range is clamped and the reply says so. color:"#rrggbb" sets a chroma_key's red, green and blue; mode:"screen" sets a blend's mode (normal, multiply, screen, overlay, add, darken, lighten). Masks take invert as 1/0 or true/false.
Effects are static: one value for the whole clip, except a rect or ellipse mask's position, size and soft edge, which take keys through edit's mask_keys (the stack position is the effect index), or follow a picture feature through the clip with track {clip, effect_index}. Distances (blur radius, shake amount, pixelate block size) are pixels at 1080 lines and scale with the frame height, so preview and export agree at any size. Ken Burns stays in transform_keys. Pixelate (size 2..200) makes mosaic blocks; a mask placed right after it limits the mosaic to that mask's shape and does not cut the clip, so an ellipse after a pixelate censors a face. Without a mask the whole picture is mosaiced. A stack that is empty or all switched off changes nothing. chroma_key and the masks reveal the lanes below; blend picks how the clip lies over them."##;

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "effect", description: EFFECT_DOC, schema: effect_schema, run: run_effect }]
}

fn effect_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "op": { "type": "string", "enum": ["add", "set", "remove", "move", "list"], "description": "What to do." },
            "clip": { "type": "string", "description": "A video clip address: LANE@START (V1@450) or LANE#INDEX (V1#0). Omit with op list to list the kinds." },
            "kind": { "type": "string", "enum": EffectKind::ALL.map(EffectKind::name), "description": "add: which effect." },
            "at": { "type": "integer", "description": "add: stack position (0 first). Default the end." },
            "index": { "type": "integer", "description": "set, remove: the effect's stack position (0 first)." },
            "from": { "type": "integer", "description": "move: stack position to take the effect from." },
            "to": { "type": "integer", "description": "move: stack position it lands on." },
            "params": { "type": "object", "description": "add, set: {name: number}, such as {\"radius\": 12}. op list with no clip shows the names and ranges." },
            "size": { "type": "number", "description": "add: pixelate's block size in pixels at 1080 lines, 2..200." },
            "on": { "type": "boolean", "description": "set: switch the effect on or off." },
            "color": { "type": "string", "description": "chroma_key: the key colour as #rrggbb.", "examples": ["#00ff00"] },
            "mode": { "type": "string", "enum": BlendMode::ALL.map(BlendMode::name), "description": "blend: how the clip lies over the lanes below." }
        },
        "required": ["op"]
    })
}

fn kind_names() -> String {
    EffectKind::ALL.map(EffectKind::name).join(", ")
}

/// A stack position argument.
fn position(args: &Value, key: &str) -> Result<usize, String> {
    match args.get(key) {
        None | Some(Value::Null) => Err(format!("missing `{key}`: a stack position, 0 for the first effect")),
        Some(v) => v.as_u64().map(|n| n as usize).ok_or_else(|| format!("{key} must be a whole number, 0 for the first effect")),
    }
}

fn parse_color(s: &str) -> Result<[f32; 3], String> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return Err(format!("color `{s}` is not #rrggbb"));
    }
    let ch = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(f32::from).map_err(|_| format!("color `{s}` is not #rrggbb"));
    Ok([ch(0)?, ch(2)?, ch(4)?])
}

/// Applies `params`, `color` and `mode` of the call to `e`; returns notes for values clamped.
fn apply_params(e: &mut Effect, args: &Value) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let mut put = |e: &mut Effect, name: &str, x: f64| -> Result<(), String> {
        e.set(name, x as f32)?;
        let got = f64::from(e.get(name).unwrap_or(0.0));
        if (got - x).abs() > 1e-6 {
            notes.push(format!("{name} {x} clamped to {got}"));
        }
        Ok(())
    };
    match args.get("params") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                let x = match v {
                    Value::Bool(b) => f64::from(u8::from(*b)),
                    v => v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| format!("{k} must be a number"))?,
                };
                put(e, k, x)?;
            }
        }
        Some(_) => return Err("params must be an object of numbers, such as {\"radius\": 12}".to_owned()),
    }
    if let Some(c) = args.get("color").filter(|v| !v.is_null()) {
        if e.kind != EffectKind::ChromaKey {
            return Err(format!("color is for chroma_key; {} has no colour", e.kind.name()));
        }
        let rgb = parse_color(c.as_str().ok_or("color must be a string like \"#00ff00\"")?)?;
        for (name, x) in ["red", "green", "blue"].into_iter().zip(rgb) {
            put(e, name, f64::from(x))?;
        }
    }
    if let Some(m) = args.get("mode").filter(|v| !v.is_null()) {
        if e.kind != EffectKind::Blend {
            return Err(format!("mode is for blend; {} has no mode", e.kind.name()));
        }
        let name = m.as_str().ok_or("mode must be a name such as \"screen\"")?;
        let mode = BlendMode::from_name(name).ok_or_else(|| {
            format!("unknown blend mode `{name}`; the modes are {}", BlendMode::ALL.map(BlendMode::name).join(", "))
        })?;
        let at = BlendMode::ALL.iter().position(|b| *b == mode).unwrap_or(0);
        put(e, "mode", at as f64)?;
    }
    Ok(notes)
}

fn effect_json(i: usize, e: &Effect) -> Value {
    let mut params = Map::new();
    for p in e.kind.params() {
        params.insert(p.name.to_owned(), json!(e.get(p.name)));
    }
    let mut v = json!({ "index": i, "kind": e.kind.name(), "label": e.kind.label(), "on": e.on, "params": params });
    if e.kind == EffectKind::Blend {
        v["mode"] = json!(BlendMode::from_value(e.values[0]).name());
    }
    v
}

fn effect_line(i: usize, e: &Effect) -> String {
    let vals: Vec<String> = e
        .kind
        .params()
        .iter()
        .map(|p| match (e.kind, p.name) {
            (EffectKind::Blend, "mode") => format!("mode={}", BlendMode::from_value(e.values[0]).name()),
            _ => format!("{}={}", p.name, e.get(p.name).unwrap_or(0.0)),
        })
        .collect();
    let keys = match e.keys.len() {
        0 => String::new(),
        n => format!(" keys={n}"),
    };
    format!("{i}: {} {} {}{keys}", e.kind.name(), if e.on { "on" } else { "off" }, vals.join(" "))
}

fn stack_text(stack: &[Effect]) -> String {
    match stack.is_empty() {
        true => "no effects".to_owned(),
        false => stack.iter().enumerate().map(|(i, e)| effect_line(i, e)).collect::<Vec<_>>().join("\n"),
    }
}

fn stack_json(addr: &str, stack: &[Effect]) -> Value {
    json!({ "clip": addr, "effects": stack.iter().enumerate().map(|(i, e)| effect_json(i, e)).collect::<Vec<_>>() })
}

fn kinds_reply() -> Reply {
    let mut lines = Vec::new();
    let mut kinds = Vec::new();
    for k in EffectKind::ALL {
        let ps: Vec<String> =
            k.params().iter().map(|p| format!("{} {}..{} (default {})", p.name, p.min, p.max, p.default)).collect();
        lines.push(format!("{} — {}: {}", k.name(), k.label(), ps.join(", ")));
        let params: Vec<Value> = k
            .params()
            .iter()
            .map(|p| json!({ "name": p.name, "label": p.label, "min": p.min, "max": p.max, "default": p.default }))
            .collect();
        kinds.push(json!({ "kind": k.name(), "label": k.label(), "params": params }));
    }
    lines.push(format!("blend mode: {}", BlendMode::ALL.map(BlendMode::name).join(", ")));
    Reply::ok(lines.join("\n")).with(json!({ "kinds": kinds }))
}

/// The clip's lane and index, refused unless it is a picture clip on an unlocked lane when
/// `mutate`.
fn resolve(s: &PlaybackSession, args: &Value, mutate: bool) -> Result<(Lane, usize, String), String> {
    let addr = opt_str(args, "clip")
        .ok_or("missing argument `clip`: give a clip address such as V1@450 (lane and start) or V1#0 (lane and index)")?;
    let (lane, idx) = parse::clip(s, addr)?;
    if lane.kind != LaneKind::Video {
        return Err(format!("{addr}: effects are for picture clips on video lanes, and {} is not one", parse::lane_name(lane)));
    }
    if mutate {
        super::edit::check_unlocked(s, &[lane])?;
    }
    Ok((lane, idx, parse::addr(lane, s.lane_clips(lane)[idx].start)))
}

fn out_of_range(what: &str, i: usize, len: usize) -> String {
    match len {
        0 => format!("{what} {i} is out of range: the clip has no effects; add one first"),
        n => format!("{what} {i} is out of range: the stack has {n} effect{}, positions 0..{}", if n == 1 { "" } else { "s" }, n - 1),
    }
}

/// The new stack for a mutating op, or the refusal.
fn edit_stack(op: &str, args: &Value, mut stack: Vec<Effect>) -> Result<(Vec<Effect>, String, Vec<String>), String> {
    let mut notes = Vec::new();
    let what = match op {
        "add" => {
            let name = opt_str(args, "kind").ok_or_else(|| format!("add needs `kind`: one of {}", kind_names()))?;
            let kind = EffectKind::from_name(name)
                .ok_or_else(|| format!("unknown effect kind `{name}`; the kinds are {}", kind_names()))?;
            if stack.len() >= MAX_EFFECTS {
                return Err(format!("a clip holds at most {MAX_EFFECTS} effects; remove one first"));
            }
            let at = match args.get("at").filter(|v| !v.is_null()) {
                None => stack.len(),
                Some(_) => position(args, "at")?,
            };
            if at > stack.len() {
                return Err(format!("at {at} is past the end: the stack has {} effects, so at is 0..{}", stack.len(), stack.len()));
            }
            let mut e = Effect::new(kind);
            notes = apply_params(&mut e, args)?;
            if let Some(size) = args.get("size").filter(|v| !v.is_null()) {
                if kind != EffectKind::Pixelate {
                    return Err(format!("size is for pixelate; {} has no size", kind.name()));
                }
                let x = size.as_f64().filter(|x| x.is_finite()).ok_or("size must be a number of pixels, 2..200")?;
                e.set("size", x as f32)?;
            }
            stack.insert(at, e);
            format!("added {} at {at}", kind.name())
        }
        "set" => {
            let i = position(args, "index")?;
            let len = stack.len();
            let e = stack.get_mut(i).ok_or_else(|| out_of_range("index", i, len))?;
            notes = apply_params(e, args)?;
            match args.get("on").filter(|v| !v.is_null()) {
                None => {}
                Some(Value::Bool(b)) => e.on = *b,
                Some(_) => return Err("on must be true or false".to_owned()),
            }
            if args.get("params").is_none() && args.get("on").is_none() && args.get("color").is_none() && args.get("mode").is_none() {
                return Err("set needs something to change: params, on, color or mode".to_owned());
            }
            format!("set {} {i}", e.kind.name())
        }
        "remove" => {
            let i = position(args, "index")?;
            if i >= stack.len() {
                return Err(out_of_range("index", i, stack.len()));
            }
            format!("removed {} {i}", stack.remove(i).kind.name())
        }
        _ => {
            let (from, to) = (position(args, "from")?, position(args, "to")?);
            if from >= stack.len() {
                return Err(out_of_range("from", from, stack.len()));
            }
            if to >= stack.len() {
                return Err(out_of_range("to", to, stack.len()));
            }
            let e = stack.remove(from);
            let name = e.kind.name();
            stack.insert(to, e);
            format!("moved {name} {from} → {to}")
        }
    };
    Ok((stack, what, notes))
}

fn run_effect(srv: &mut Server, args: &Value) -> Reply {
    let op = match opt_str(args, "op") {
        Some(o @ ("add" | "set" | "remove" | "move" | "list")) => o,
        Some(o) => return Reply::err(format!("unknown op `{o}`; the ops are add, set, remove, move, list")),
        None => return Reply::err("missing argument `op`: add, set, remove, move or list"),
    };
    if op == "list" && args.get("clip").is_none_or(Value::is_null) {
        return kinds_reply();
    }
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let (lane, idx, addr) = match resolve(s, args, op != "list") {
        Ok(v) => v,
        Err(e) => return Reply::err(format!("effect {op}: {e}")),
    };
    let before = s.effects_of(lane, idx).to_vec();
    if op == "list" {
        return Reply::ok(format!("{addr}: {}", stack_text(&before))).with(stack_json(&addr, &before));
    }
    let (after, what, notes) = match edit_stack(op, args, before.clone()) {
        Ok(v) => v,
        Err(e) => return Reply::err(format!("effect {op} {addr}: {e}")),
    };
    let note = |notes: &[String]| notes.iter().map(|n| format!(" ({n})")).collect::<String>();
    if after == before {
        return Reply::ok(format!("{addr}: already set · {what}{}\n{}", note(&notes), stack_text(&after)))
            .with(stack_json(&addr, &after));
    }
    let start = s.history_pushes();
    if !s.set_effects(lane, idx, after) {
        return Reply::err(format!("effect {op} {addr}: the engine refused it"));
    }
    let stack = s.effects_of(lane, idx).to_vec();
    record_pushes(srv, start, format!("effect {op} {addr}"));
    let undo = srv.ledger.len();
    let mut j = stack_json(&addr, &stack);
    j["undo_steps"] = json!(undo);
    Reply::ok(format!("{addr}: {what}{} · undo step {undo}\n{}", note(&notes), stack_text(&stack))).with(j)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        engine::ao::set_silent_output();
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(p).expect("open test asset"));
        srv
    }

    fn stack(srv: &Server) -> Vec<Effect> {
        srv.session.as_ref().unwrap().effects_of(Lane::new(LaneKind::Video, 0), 0).to_vec()
    }

    #[test]
    fn add_set_move_remove_are_one_undo_step_each() {
        let mut srv = server();
        let r = run_effect(&mut srv, &json!({"op": "add", "clip": "V1#0", "kind": "blur", "params": {"radius": 12}}));
        assert!(!r.is_error && r.text.contains("added blur at 0") && r.text.contains("undo step 1"), "{}", r.text);
        let r = run_effect(&mut srv, &json!({"op": "add", "clip": "V1#0", "kind": "vignette", "at": 0}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(stack(&srv)[0].kind, EffectKind::Vignette);
        assert_eq!(stack(&srv)[1].get("radius"), Some(12.0));
        let r = run_effect(&mut srv, &json!({"op": "set", "clip": "V1#0", "index": 1, "params": {"radius": 500}, "on": false}));
        assert!(!r.is_error && r.text.contains("radius 500 clamped to 100"), "{}", r.text);
        assert!(!stack(&srv)[1].on && stack(&srv)[1].get("radius") == Some(100.0));
        let r = run_effect(&mut srv, &json!({"op": "move", "clip": "V1#0", "from": 0, "to": 1}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(stack(&srv)[1].kind, EffectKind::Vignette);
        let r = run_effect(&mut srv, &json!({"op": "remove", "clip": "V1#0", "index": 0}));
        assert!(!r.is_error && r.text.contains("removed blur") && r.text.contains("undo step 5"), "{}", r.text);
        assert_eq!(stack(&srv).len(), 1);
        let s = srv.session.as_mut().unwrap();
        assert!(s.undo(), "remove undone alone");
        assert_eq!(stack(&srv).len(), 2);
        let s = srv.session.as_mut().unwrap();
        for _ in 0..4 {
            assert!(s.undo());
        }
        assert!(stack(&srv).is_empty());
    }

    #[test]
    fn color_and_mode_conveniences_and_list() {
        let mut srv = server();
        let r = run_effect(&mut srv, &json!({"op": "add", "clip": "V1#0", "kind": "chroma_key", "color": "#00ff80", "params": {"tolerance": 0.4}}));
        assert!(!r.is_error, "{}", r.text);
        let e = stack(&srv)[0].clone();
        assert_eq!((e.get("red"), e.get("green"), e.get("blue"), e.get("tolerance")), (Some(0.0), Some(255.0), Some(128.0), Some(0.4)));
        let r = run_effect(&mut srv, &json!({"op": "add", "clip": "V1#0", "kind": "blend", "mode": "screen"}));
        assert!(!r.is_error && r.text.contains("mode=screen"), "{}", r.text);
        let r = run_effect(&mut srv, &json!({"op": "list", "clip": "V1#0"}));
        let j = r.structured.as_deref().unwrap();
        assert_eq!(j["effects"].as_array().unwrap().len(), 2);
        assert_eq!(j["effects"][1]["mode"], "screen");
        assert_eq!(j["effects"][0]["params"]["green"], 255.0);
        let r = run_effect(&mut srv, &json!({"op": "list"}));
        assert!(r.text.contains("ellipse_mask") && r.text.contains("radius 0..100"), "{}", r.text);
        // Setting what is already there is not an undo step.
        let r = run_effect(&mut srv, &json!({"op": "set", "clip": "V1#0", "index": 1, "mode": "screen"}));
        assert!(r.text.contains("already set"), "{}", r.text);
        assert_eq!(srv.ledger.len(), 2);
    }

    #[test]
    fn refusals_are_specific_and_change_nothing() {
        let mut srv = server();
        let err = |srv: &mut Server, v: Value| {
            let r = run_effect(srv, &v);
            assert!(r.is_error, "{}", r.text);
            r.text
        };
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "sparkle"}));
        assert!(t.contains("unknown effect kind `sparkle`") && t.contains("chroma_key"), "{t}");
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "blur", "params": {"size": 3}}));
        assert!(t.contains("no parameter \"size\"") && t.contains("radius"), "{t}");
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "blur", "color": "#fff000"}));
        assert!(t.contains("color is for chroma_key"), "{t}");
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "blend", "mode": "burn"}));
        assert!(t.contains("unknown blend mode") && t.contains("multiply"), "{t}");
        let t = err(&mut srv, json!({"op": "remove", "clip": "V1#0", "index": 0}));
        assert!(t.contains("no effects"), "{t}");
        let t = err(&mut srv, json!({"op": "add", "clip": "A1#0", "kind": "blur"}));
        assert!(t.contains("not one"), "{t}");
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "blur", "at": 3}));
        assert!(t.contains("past the end"), "{t}");
        for _ in 0..MAX_EFFECTS {
            assert!(!run_effect(&mut srv, &json!({"op": "add", "clip": "V1#0", "kind": "sharpen"})).is_error);
        }
        let t = err(&mut srv, json!({"op": "add", "clip": "V1#0", "kind": "blur"}));
        assert!(t.contains("at most 16"), "{t}");
        let t = err(&mut srv, json!({"op": "move", "clip": "V1#0", "from": 0, "to": 16}));
        assert!(t.contains("to 16 is out of range") && t.contains("0..15"), "{t}");
        let t = err(&mut srv, json!({"op": "set", "clip": "V1#0", "index": 0}));
        assert!(t.contains("needs something to change"), "{t}");
        srv.session.as_mut().unwrap().set_lane_locked(Lane::new(LaneKind::Video, 0), true);
        let t = err(&mut srv, json!({"op": "set", "clip": "V1#0", "index": 0, "on": false}));
        assert!(t.contains("locked"), "{t}");
        assert_eq!(stack(&srv).len(), MAX_EFFECTS);
        assert_eq!(srv.ledger.len(), MAX_EFFECTS, "refusals pushed no undo step");
        srv.session = None;
        assert!(err(&mut srv, json!({"op": "list", "clip": "V1#0"})).contains("no project"));
    }
}
