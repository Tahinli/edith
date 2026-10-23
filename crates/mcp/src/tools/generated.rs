//! Generated clips and clip marks for the batched `edit` verb: `generator_add` (also a test tone on an audio lane) and
//! `generator_set` (a solid, gradient, countdown or colour bars as a clip), `adjustment_add`
//! (an adjustment layer), and `label` / `enable` (a clip's colour label and on/off switch).
//!
//! A generator is an ordinary clip on a video lane, so trim, move, grade and effects reach it
//! through the existing ops. [`apply`] returns `None` for an op that is not one of these.

use engine::generate::{Form, Gen, Kind};
use engine::project::{label_by_name, label_name, Lane, LaneKind, LABELS};
use engine::PlaybackSession;
use serde_json::Value;

use super::edit::{check_unlocked, clip_addr, length};
use super::edit_fx::Applied;
use crate::parse;


pub fn apply(s: &mut PlaybackSession, op: &Value, fps: f64, tones: Option<&std::path::Path>) -> Option<Result<Applied, String>> {
    let name = op.get("op")?.as_str()?;
    Some(match name {
        "generator_add" => generator_add(s, op, fps, tones),
        "generator_set" => generator_set(s, op),
        "adjustment_add" => adjustment_add(s, op, fps),
        "label" => label(s, op),
        "enable" => enable(s, op),
        _ => return None,
    })
}

fn str_arg<'a>(op: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.as_str())),
        Some(v) => Err(format!("`{key}` must be a string, got {}", parse::kind_of(v))),
    }
}

/// `#ff3366` or `ff3366` as 0xRRGGBB.
fn colour(who: &str, key: &str, text: &str) -> Result<u32, String> {
    let t = text.trim().trim_start_matches('#');
    if t.len() == 6
        && let Ok(v) = u32::from_str_radix(t, 16)
    {
        return Ok(v);
    }
    Err(format!("{who}: `{key}` {text:?} is not a colour; give six hex digits such as \"#ff3366\""))
}

fn hex(v: u32) -> String {
    format!("#{v:06x}")
}

/// Plain words for a generator, as the listings show it.
pub fn describe(g: &Gen) -> String {
    match g.kind {
        Kind::Solid { rgb } => format!("generator solid {}", hex(rgb)),
        Kind::Gradient { a, b, angle } => format!("generator gradient {} to {} at {angle} degrees", hex(a), hex(b)),
        Kind::Countdown { from, .. } => format!("generator countdown from {from} s"),
        Kind::Bars => "generator bars".to_string(),
        Kind::Adjust => "adjustment layer".to_string(),
        Kind::Shape(sh) => {
            let paint = |c: Option<u32>| c.map_or("none".to_string(), hex);
            match sh.form {
                Form::Rectangle => format!("shape rectangle fill {} stroke {} width {} radius {}", paint(sh.fill), paint(sh.stroke), sh.width, sh.radius),
                Form::Ellipse => format!("shape ellipse fill {} stroke {} width {}", paint(sh.fill), paint(sh.stroke), sh.width),
                Form::Line | Form::Arrow => format!("shape {} stroke {} width {}", sh.form.name(), paint(sh.stroke), sh.width),
            }
        }
    }
}

/// Which of the params a kind takes; any other given param is refused by name.
fn takes(kind: &str) -> &'static [&'static str] {
    match kind {
        "solid" => &["color"],
        "gradient" => &["color", "color2", "angle"],
        "countdown" => &["from"],
        "rectangle" => &["fill", "stroke", "width", "radius"],
        "ellipse" => &["fill", "stroke", "width"],
        "line" | "arrow" => &["stroke", "width"],
        _ => &[],
    }
}

/// Merges the params in `op` onto `g`, refusing a param the kind does not take.
fn with_params(who: &str, op: &Value, mut g: Gen) -> Result<(Gen, usize), String> {
    let kind = g.kind.name();
    let mut n = 0;
    for key in ["color", "color2", "angle", "from", "fill", "stroke", "width", "radius"] {
        if matches!(op.get(key), None | Some(Value::Null)) {
            continue;
        }
        if !takes(kind).contains(&key) {
            let ok = takes(kind);
            let hint = if ok.is_empty() { format!("{kind} takes no params") } else { format!("{kind} takes {}", ok.join(", ")) };
            return Err(format!("{who}: `{key}` does not apply to {kind}; {hint}"));
        }
        n += 1;
    }
    match &mut g.kind {
        Kind::Solid { rgb } => {
            if let Some(c) = str_arg(op, "color")? {
                *rgb = colour(who, "color", c)?;
            }
        }
        Kind::Gradient { a, b, angle } => {
            if let Some(c) = str_arg(op, "color")? {
                *a = colour(who, "color", c)?;
            }
            if let Some(c) = str_arg(op, "color2")? {
                *b = colour(who, "color2", c)?;
            }
            if let Some(v) = op.get("angle").filter(|v| !v.is_null()) {
                let d = v.as_f64().ok_or_else(|| format!("{who}: `angle` must be a number of degrees, got {}", parse::kind_of(v)))?;
                if !(0.0..360.0).contains(&d) {
                    return Err(format!("{who}: `angle` {d} is out of range 0–359 (0 is left to right, 90 top to bottom)"));
                }
                *angle = d.round() as u16 % 360;
            }
        }
        Kind::Countdown { from, .. } => {
            if let Some(v) = op.get("from").filter(|v| !v.is_null()) {
                let f = v.as_f64().ok_or_else(|| format!("{who}: `from` must be a number of seconds, got {}", parse::kind_of(v)))?;
                if f.fract() != 0.0 || !(1.0..=3600.0).contains(&f) {
                    return Err(format!("{who}: `from` {f} must be a whole number of seconds, 1–3600"));
                }
                *from = f as u16;
            }
        }
        Kind::Shape(sh) => {
            let paint = |key: &str| -> Result<Option<Option<u32>>, String> {
                Ok(match str_arg(op, key)? {
                    None => None,
                    Some(t) if t.trim().eq_ignore_ascii_case("none") => Some(None),
                    Some(t) => Some(Some(colour(who, key, t)?)),
                })
            };
            let whole = |key: &str, max: f64| -> Result<Option<u16>, String> {
                let Some(v) = op.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
                let f = v.as_f64().ok_or_else(|| format!("{who}: `{key}` must be a number of pixels, got {}", parse::kind_of(v)))?;
                if f.fract() != 0.0 || !(0.0..=max).contains(&f) {
                    return Err(format!("{who}: `{key}` {f} must be a whole number of pixels, 0–{max}"));
                }
                Ok(Some(f as u16))
            };
            if let Some(p) = paint("fill")? {
                sh.fill = p;
            }
            if let Some(p) = paint("stroke")? {
                sh.stroke = p;
            }
            if let Some(v) = whole("width", 400.0)? {
                sh.width = v;
            }
            if let Some(v) = whole("radius", 2000.0)? {
                sh.radius = v;
            }
            if sh.fill.is_none() && (sh.stroke.is_none() || sh.width == 0 && sh.form.fills()) {
                return Err(format!("{who}: a shape needs something to paint: a fill, or a stroke with a width above 0"));
            }
            if !sh.form.fills() && sh.stroke.is_none() {
                return Err(format!("{who}: a {} is all stroke; `stroke` cannot be none", sh.form.name()));
            }
            if !sh.form.fills() && sh.width == 0 {
                return Err(format!("{who}: a {} needs a `width` above 0", sh.form.name()));
            }
        }
        Kind::Bars | Kind::Adjust => {}
    }
    Ok((g, n))
}

/// The video lane an op names; `None` when it does not name one.
fn named_video_lane(s: &PlaybackSession, who: &str, op: &Value) -> Result<Option<Lane>, String> {
    let Some(raw) = str_arg(op, "lane")? else { return Ok(None) };
    let lane = parse::lane(raw)?;
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: {raw} is not a video lane; a generated clip goes on a video lane (V1..)"));
    }
    if !s.lanes().contains(&lane) {
        let names: Vec<String> = s.lanes().into_iter().map(parse::lane_name).collect();
        return Err(format!("{who}: {raw} is not a lane on this timeline; the lanes are {}; add one with lane_add {{kind:\"video\"}}", names.join(", ")));
    }
    check_unlocked(s, &[lane])?;
    Ok(Some(lane))
}

/// Places `g` on `lane` and returns the reply line and the new clip.
fn place(s: &mut PlaybackSession, who: &str, op: &Value, fps: f64, lane: Lane, g: Gen, note: &str) -> Result<Applied, String> {
    let end = parse::session_end(s);
    let at = match super::edit::opt_time(op, "at", fps, end)? {
        Some(a) => a,
        None => s.lane_clips(lane).last().map_or(0, |c| c.end()),
    };
    let frames = match length(op, "duration", fps)? {
        Some(0) => return Err(format!("{who}: `duration` must be more than 0")),
        Some(f) => Some(f),
        None => None,
    };
    let secs = frames.map(|f| parse::secs(f, fps));
    let placed = s.place_generated(g, parse::secs(at, fps), secs, Some(lane)).map_err(|e| format!("{who}: {e}"))?;
    if !placed {
        return Err(format!("{who}: the clip did not place at {} on {}", parse::tc(at, fps), parse::lane_name(lane)));
    }
    let idx = s
        .lane_clips(lane)
        .iter()
        .position(|c| c.start == at)
        .ok_or_else(|| format!("{who}: the new clip is not at {}", parse::tc(at, fps)))?;
    if let Some(scale) = g.default_scale() {
        s.set_transform(lane, idx, Some(engine::transform::TransformParams { scale, ..Default::default() }));
    }
    let c = &s.lane_clips(lane)[idx];
    let (start, len) = (c.start, c.end() - c.start);
    let addr = parse::addr(lane, start);
    Ok(Applied { text: format!("{who} → {addr} ({}) for {}{note}", describe(&g), parse::tc(len, fps)), touched: vec![(lane, start)] })
}

/// `generator_add {kind: solid|gradient|countdown|bars|rectangle|ellipse|line|arrow, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?, lane?=V1, at?, duration?=5s}`.
fn generator_add(s: &mut PlaybackSession, op: &Value, fps: f64, tones: Option<&std::path::Path>) -> Result<Applied, String> {
    let kind = str_arg(op, "kind")?.ok_or("generator_add: missing `kind`: solid, gradient, countdown, bars, tone, rectangle, ellipse, line or arrow")?;
    if kind == "tone" {
        return tone_add(s, op, fps, tones);
    }
    let who = format!("generator_add {kind}");
    if !matches!(kind, "solid" | "gradient" | "countdown" | "bars" | "rectangle" | "ellipse" | "line" | "arrow") {
        return Err(format!("generator_add: `{kind}` is not a kind; use solid, gradient, countdown, bars, tone, rectangle, ellipse, line or arrow (adjustment_add makes an adjustment layer)"));
    }
    let base = s.generator_named(kind).ok_or_else(|| format!("generator_add: `{kind}` is not a kind"))?;
    let (g, _) = with_params(&who, op, base)?;
    let lane = named_video_lane(s, &who, op)?.unwrap_or(Lane::V1);
    place(s, &who, op, fps, lane, g, "")
}

/// `generator_add {kind: "tone", hz?=1000, level?=-20, lane?=A1, at?, duration?=10s}`: a test tone,
/// a sine at `hz` whose RMS is `level` dBFS, on an audio lane. The length is a whole number of seconds, rounded up; the default is ten.
fn tone_add(s: &mut PlaybackSession, op: &Value, fps: f64, tones: Option<&std::path::Path>) -> Result<Applied, String> {
    let who = "generator_add tone";
    for key in ["color", "color2", "angle", "from", "fill", "stroke", "width", "radius"] {
        if !matches!(op.get(key), None | Some(Value::Null)) {
            return Err(format!("{who}: `{key}` does not apply to tone; tone takes hz and level"));
        }
    }
    let number = |key: &str, default: f64| -> Result<f64, String> {
        match op.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v.as_f64().ok_or_else(|| format!("{who}: `{key}` must be a number, got {}", parse::kind_of(v))),
        }
    };
    let hz = number("hz", 1000.0)?;
    let level = number("level", -20.0)?;
    if !(20.0..=20_000.0).contains(&hz) {
        return Err(format!("{who}: `hz` {hz} is out of range 20–20000"));
    }
    if !(-60.0..=-3.0).contains(&level) {
        return Err(format!("{who}: `level` {level} is out of range −60 to −3 dBFS"));
    }
    let lane = match str_arg(op, "lane")? {
        None => Lane::A1,
        Some(raw) => {
            let lane = parse::lane(raw)?;
            if lane.kind != LaneKind::Audio {
                return Err(format!("{who}: {raw} is not an audio lane; a tone goes on an audio lane (A1..)"));
            }
            if !s.lanes().contains(&lane) {
                return Err(format!("{who}: {raw} is not a lane on this timeline"));
            }
            check_unlocked(s, &[lane])?;
            lane
        }
    };
    let end = parse::session_end(s);
    let at = match super::edit::opt_time(op, "at", fps, end)? {
        Some(a) => a,
        None => s.lane_clips(lane).last().map_or(0, |c| c.end()),
    };
    let secs = match length(op, "duration", fps)? {
        Some(0) => return Err(format!("{who}: `duration` must be more than 0")),
        Some(f) => (f as f64 / fps).ceil().clamp(1.0, 600.0) as u32,
        None => engine::generate::TONE_SECS,
    };
    let tone = engine::generate::Tone::new(hz.round() as u32, level.round() as i32);
    let dir = tones.ok_or_else(|| format!("{who}: there is no data folder to keep the tone in"))?;
    let placed = s.place_tone(tone, parse::secs(at, fps), secs, Some(lane), dir).map_err(|e| format!("{who}: {e}"))?;
    if !placed {
        return Err(format!("{who}: the clip did not place at {} on {}", parse::tc(at, fps), parse::lane_name(lane)));
    }
    let idx = s
        .lane_clips(lane)
        .iter()
        .position(|c| c.start == at)
        .ok_or_else(|| format!("{who}: the new clip is not at {}", parse::tc(at, fps)))?;
    let c = &s.lane_clips(lane)[idx];
    let (start, len) = (c.start, c.end() - c.start);
    let addr = parse::addr(lane, start);
    Ok(Applied {
        text: format!("{who} → {addr} (sine {} Hz at {} dBFS RMS) for {}", tone.hz, tone.dbfs, parse::tc(len, fps)),
        touched: vec![(lane, start)],
    })
}

/// `adjustment_add {lane?=V2, at?, duration?=5s}`. With no lane, V2 -- added when the timeline has only V1.
fn adjustment_add(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let who = "adjustment_add";
    let (lane, note) = match named_video_lane(s, who, op)? {
        Some(l) => (l, String::new()),
        None => {
            let v2 = Lane::new(LaneKind::Video, 1);
            if s.lanes().contains(&v2) {
                check_unlocked(s, &[v2])?;
                (v2, String::new())
            } else {
                let l = s.add_lane(LaneKind::Video);
                let note = format!("; added lane {} for it", parse::lane_name(l));
                (l, note)
            }
        }
    };
    let g = s.generator_named("adjust").ok_or("adjustment_add: no adjustment layer")?;
    place(s, who, op, fps, lane, g, &note)
}

/// `generator_set {clip, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?}`: params merged onto the generator clip.
fn generator_set(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let raw = str_arg(op, "clip")?.ok_or("generator_set: missing `clip`: a generator clip address such as V2@0")?;
    let who = format!("generator_set {raw}");
    let (lane, idx) = clip_addr(s, raw)?;
    check_unlocked(s, &[lane])?;
    let Some(cur) = s.generator_of(lane, idx) else {
        return Err(format!("{who}: {raw} is not a generator clip; it plays a file. generator_set changes a clip made by generator_add"));
    };
    let (g, n) = with_params(&who, op, cur)?;
    if n == 0 {
        let ok = takes(cur.kind.name());
        return Err(if ok.is_empty() {
            format!("{who}: {} has no params to change", describe(&cur))
        } else {
            format!("{who}: give at least one of {}", ok.join(", "))
        });
    }
    let start = s.lane_clips(lane)[idx].start;
    if g == cur {
        return Ok(Applied { text: format!("{who} → {} (unchanged)", describe(&g)), touched: vec![(lane, start)] });
    }
    if !s.set_generator(lane, idx, g) {
        return Err(format!("{who}: the engine refused it"));
    }
    Ok(Applied { text: format!("{who} → {}", describe(&g)), touched: vec![(lane, start)] })
}

/// The clip addresses of `clip` or `clips`, resolved to (lane, index, address as written).
fn clips_arg(s: &PlaybackSession, who: &str, op: &Value) -> Result<Vec<(Lane, usize, String)>, String> {
    let raws: Vec<String> = match (op.get("clip"), op.get("clips")) {
        (Some(Value::String(c)), None | Some(Value::Null)) => vec![c.clone()],
        (None | Some(Value::Null), Some(Value::Array(a))) => {
            let mut v = Vec::new();
            for x in a {
                v.push(x.as_str().ok_or_else(|| format!("{who}: `clips` lists addresses such as V1@0"))?.to_string());
            }
            v
        }
        (Some(_), Some(c)) if !c.is_null() => return Err(format!("{who}: give clip or clips, not both")),
        _ => return Err(format!("{who}: missing `clip` (an address such as V1@0) or `clips` (a list of them)")),
    };
    if raws.is_empty() {
        return Err(format!("{who}: `clips` is empty"));
    }
    let mut out = Vec::new();
    for raw in raws {
        let (lane, idx) = clip_addr(s, &raw)?;
        if lane.kind == LaneKind::Subtitle {
            return Err(format!("{who}: {raw} is a caption; labels and the on/off switch are for picture and sound clips"));
        }
        check_unlocked(s, &[lane])?;
        out.push((lane, idx, raw));
    }
    Ok(out)
}

fn label_list() -> String {
    LABELS.iter().map(|l| l.0).collect::<Vec<_>>().join(", ")
}

/// `label {clip | clips, value: <name>|1..8|none}`.
fn label(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let who = "label";
    let text = match op.get("value") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Number(n)) => n.to_string(),
        None | Some(Value::Null) => return Err(format!("label: missing `value`: a label name ({}), a number 1–8, or none", label_list())),
        Some(v) => return Err(format!("label: `value` must be a name or a number, got {}", parse::kind_of(v))),
    };
    let n = label_by_name(&text).ok_or_else(|| format!("label: `{text}` is not a label; use one of {}, a number 1–8, or none", label_list()))?;
    let clips = clips_arg(s, who, op)?;
    let shown = label_name(n).unwrap_or("none");
    let mut touched = Vec::new();
    let mut addrs = Vec::new();
    for (lane, idx, raw) in &clips {
        let start = s.lane_clips(*lane)[*idx].start;
        if s.mark_of(*lane, *idx).label != n && !s.set_label(*lane, *idx, n) {
            return Err(format!("label {raw}: the engine refused it"));
        }
        touched.push((*lane, start));
        addrs.push(raw.clone());
    }
    Ok(Applied { text: format!("label {} → {shown}", addrs.join(", ")), touched })
}

/// `enable {clip | clips, on: true|false}`.
fn enable(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let who = "enable";
    let on = match op.get("on") {
        Some(Value::Bool(b)) => *b,
        None | Some(Value::Null) => return Err("enable: missing `on`: true to play the clip, false to switch it off".into()),
        Some(v) => return Err(format!("enable: `on` must be true or false, got {}", parse::kind_of(v))),
    };
    let clips = clips_arg(s, who, op)?;
    let mut touched = Vec::new();
    let mut addrs = Vec::new();
    for (lane, idx, raw) in &clips {
        let start = s.lane_clips(*lane)[*idx].start;
        if s.mark_of(*lane, *idx).off == on && !s.set_enabled(*lane, *idx, on) {
            return Err(format!("enable {raw}: the engine refused it"));
        }
        touched.push((*lane, start));
        addrs.push(raw.clone());
    }
    Ok(Applied { text: format!("enable {} → {}", addrs.join(", "), if on { "on" } else { "off (disabled)" }), touched })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::server::{Reply, Server};
    use crate::testutil::TestDir;

    fn server() -> Server {
        engine::ao::set_silent_output();
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::empty(320, 180, 30.0).expect("empty timeline"));
        srv
    }

    fn call(srv: &mut Server, name: &str, args: Value) -> Reply {
        let tool = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool exists");
        (tool.run)(srv, &args)
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        call(srv, "edit", json!({ "ops": ops }))
    }

    fn ok(srv: &mut Server, ops: Value) -> String {
        let r = ed(srv, ops);
        assert!(!r.is_error, "{}", r.text);
        r.text
    }

    fn err(srv: &mut Server, ops: Value) -> String {
        let r = ed(srv, ops);
        assert!(r.is_error, "expected a refusal, got {}", r.text);
        r.text
    }

    fn sess(srv: &Server) -> &PlaybackSession {
        srv.session.as_ref().unwrap()
    }

    fn frame(srv: &mut Server, at: &str) -> String {
        let r = call(srv, "frame", json!({"at": at}));
        assert!(!r.is_error, "{}", r.text);
        r.images.first().expect("a picture").1.clone()
    }

    #[test]
    fn generator_add_places_each_kind_and_lists_it() {
        let mut srv = server();
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "solid", "color": "#ff3366"}]));
        assert!(t.contains("V1@0") && t.contains("generator solid #ff3366") && t.contains("150f (5.000s)"), "{t}");
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "gradient", "color": "000000", "color2": "#ffffff", "angle": 45, "duration": "2s"}]));
        assert!(t.contains("V1@150") && t.contains("gradient #000000 to #ffffff at 45 degrees") && t.contains("60f"), "{t}");
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "countdown", "from": 3}]));
        assert!(t.contains("V1@210") && t.contains("countdown from 3 s") && t.contains("90f"), "{t}");
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "bars", "at": "20s", "duration": 1}]));
        assert!(t.contains("V1@600") && t.contains("generator bars"), "{t}");
        assert_eq!(sess(&srv).generator_of(Lane::V1, 0).unwrap().kind, Kind::Solid { rgb: 0xff3366 });

        let tl = call(&mut srv, "timeline", json!({}));
        assert!(!tl.is_error, "{}", tl.text);
        assert!(tl.text.contains("generator solid #ff3366") && tl.text.contains("generator countdown from 3 s"), "{}", tl.text);
        assert!(!tl.text.contains("OFFLINE"), "{}", tl.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1@0"}));
        assert!(info.text.contains("generator solid #ff3366") && !info.text.contains("edith-gen"), "{}", info.text);
        let st = call(&mut srv, "project_status", json!({}));
        assert!(!st.text.contains("OFFLINE") && !st.text.contains("offline"), "{}", st.text);
        let ml = call(&mut srv, "media_list", json!({}));
        assert!(!ml.text.contains("OFFLINE"), "{}", ml.text);
    }

    #[test]
    fn a_tone_goes_on_an_audio_lane_ten_seconds_long() {
        // The tone's file goes under a temporary data folder, never the user's own.
        let data = TestDir::new("tone-data");
        let mut srv = server();
        srv.data_home = Some(data.path().to_path_buf());
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "tone"}]));
        assert!(t.contains("A1@0") && t.contains("1000 Hz") && t.contains("-20 dBFS") && t.contains("300f"), "{t}");
        assert!(sess(&srv).lane_clips(Lane::A1).len() == 1 && sess(&srv).lane_clips(Lane::V1).is_empty());
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "tone", "hz": 440, "level": -12, "at": "12s", "duration": "2s"}]));
        assert!(t.contains("A1@360") && t.contains("440 Hz") && t.contains("-12 dBFS") && t.contains("60f"), "{t}");
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "tone", "lane": "V1"}])).contains("not an audio lane"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "tone", "level": 0}])).contains("out of range"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "tone", "hz": 5}])).contains("out of range"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "tone", "color": "#ffffff"}])).contains("does not apply to tone"));
    }

    #[test]
    fn templates_are_listed_and_project_new_starts_from_one() {
        // Templates are read from a temporary data folder, never the user's own.
        let dir = crate::testutil::TestDir::new("generated-template-new");
        let mut listing = Server::new();
        listing.data_home = Some(dir.path().to_path_buf());
        let listed = call(&mut listing, "templates", json!({}));
        assert!(!listed.is_error, "{}", listed.text);
        assert!(listed.text.contains("YouTube 1080p 30 (built-in)") && listed.text.contains("Square 1080 30 (built-in)"), "{}", listed.text);
        let path = dir.path().join("short.edith");
        let r = call(&mut Server::new(), "project_new", json!({"path": path, "template": "Vertical 1080×1920 30"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1080x1920 @30") && r.text.contains("started from template Vertical 1080×1920 30"), "{}", r.text);
        assert!(r.text.contains("shorts_reels"), "{}", r.text);
        let refused = call(&mut Server::new(), "project_new", json!({"path": dir.path().join("b.edith"), "template": "Square 1080 30", "fps": 25}));
        assert!(refused.is_error && refused.text.contains("leave out resolution and fps"), "{}", refused.text);
        let missing = call(&mut Server::new(), "project_new", json!({"path": dir.path().join("c.edith"), "template": "No such one"}));
        assert!(missing.is_error && missing.text.contains("no template named"), "{}", missing.text);
    }

    #[test]
    fn shapes_are_added_set_and_refused_by_name() {
        let mut srv = server();
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "rectangle", "fill": "#ff0000", "stroke": "#ffffff", "width": 6, "radius": 40, "duration": "2s"}]));
        assert!(t.contains("shape rectangle fill #ff0000 stroke #ffffff width 6 radius 40"), "{t}");
        assert!(sess(&srv).transform_of(Lane::V1, 0).is_some_and(|t| t.scale < 1.0), "a shape lands smaller than the frame");
        let t = ok(&mut srv, json!([{"op": "generator_set", "clip": "V1@0", "fill": "none"}]));
        assert!(t.contains("fill none stroke #ffffff"), "{t}");
        let t = ok(&mut srv, json!([{"op": "generator_add", "kind": "arrow", "stroke": "#ffd600", "width": 20}]));
        assert!(t.contains("shape arrow stroke #ffd600 width 20"), "{t}");
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "line", "fill": "#fff"}])).contains("does not apply to line"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "ellipse", "radius": 5}])).contains("does not apply to ellipse"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "arrow", "width": 0}])).contains("above 0"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "rectangle", "fill": "none"}])).contains("something to paint"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "rectangle", "width": 401}])).contains("0–400"));
    }

    #[test]
    fn generator_add_refusals() {
        let mut srv = server();
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "plasma"}])).contains("not a kind"));
        assert!(err(&mut srv, json!([{"op": "generator_add"}])).contains("missing `kind`"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "solid", "color": "pink"}])).contains("not a colour"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "gradient", "color2": "#12345"}])).contains("color2"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "gradient", "angle": 360}])).contains("out of range"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "solid", "angle": 10}])).contains("does not apply to solid"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "countdown", "from": 0}])).contains("1–3600"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "bars", "duration": 0}])).contains("more than 0"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "bars", "lane": "V7"}])).contains("not a lane"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "bars", "lane": "A1"}])).contains("not a video lane"));
        assert!(err(&mut srv, json!([{"op": "generator_add", "kind": "adjust"}])).contains("not a kind"));
        assert!(sess(&srv).lane_clips(Lane::V1).is_empty(), "refusals change nothing");
    }

    #[test]
    fn generator_set_changes_params_and_refuses_what_is_not_a_generator() {
        let mut srv = server();
        ok(&mut srv, json!([{"op": "generator_add", "kind": "solid", "color": "#112233"}, {"op": "adjustment_add"}]));
        let t = ok(&mut srv, json!([{"op": "generator_set", "clip": "V1@0", "color": "#ff0000"}]));
        assert!(t.contains("generator solid #ff0000"), "{t}");
        assert_eq!(sess(&srv).lane_clips(Lane::V1)[0].end(), 150, "place and length kept");
        let t = ok(&mut srv, json!([{"op": "generator_set", "clip": "V1@0", "color": "#ff0000"}]));
        assert!(t.contains("unchanged"), "{t}");
        assert!(err(&mut srv, json!([{"op": "generator_set", "clip": "V1@0"}])).contains("give at least one of color"));
        assert!(err(&mut srv, json!([{"op": "generator_set", "clip": "V1@0", "from": 4}])).contains("does not apply"));
        assert!(err(&mut srv, json!([{"op": "generator_set", "clip": "V2@0", "color": "#fff000"}])).contains("does not apply"));
        assert!(err(&mut srv, json!([{"op": "generator_set", "clip": "V1@0", "color": "nope"}])).contains("not a colour"));
        // a clip that plays a file is not a generator: audio lanes have none, so A1 is refused as a lane
        let r = call(&mut srv, "edit", json!({"ops": [{"op": "generator_set", "clip": "A1@0", "color": "#fff000"}]}));
        assert!(r.is_error, "{}", r.text);
    }

    #[test]
    fn adjustment_add_makes_v2_and_grades_what_is_beneath() {
        let mut srv = server();
        ok(&mut srv, json!([{"op": "generator_add", "kind": "solid", "color": "#808080"}]));
        let before = frame(&mut srv, "1s");
        let t = ok(&mut srv, json!([{"op": "adjustment_add", "at": "0s", "duration": "3s"}]));
        assert!(t.contains("V2@0") && t.contains("adjustment layer") && t.contains("added lane V2"), "{t}");
        assert!(sess(&srv).generator_of(Lane::new(LaneKind::Video, 1), 0).unwrap().is_adjust());
        let flat = frame(&mut srv, "1s");
        assert_eq!(before, flat, "an ungraded adjustment layer changes nothing");
        ok(&mut srv, json!([{"op": "color", "clip": "V2@0", "brightness": -0.6}]));
        let graded = frame(&mut srv, "1s");
        assert_ne!(before, graded, "the layer grades the clip under it");
        ok(&mut srv, json!([{"op": "enable", "clip": "V2@0", "on": false}]));
        assert_eq!(frame(&mut srv, "1s"), before, "off, the layer adjusts nothing");
        let t = ok(&mut srv, json!([{"op": "adjustment_add", "lane": "V2", "at": "5s", "duration": "1s"}]));
        assert!(t.contains("V2@150") && !t.contains("added lane"), "{t}");
        assert!(err(&mut srv, json!([{"op": "adjustment_add", "lane": "V5"}])).contains("not a lane"));
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("adjustment layer"), "{}", tl.text);
    }

    #[test]
    fn a_generator_clip_renders_and_a_disabled_clip_drops_out() {
        let mut srv = server();
        ok(&mut srv, json!([{"op": "generator_add", "kind": "solid", "color": "#000000"}]));
        let base = frame(&mut srv, "1s");
        ok(&mut srv, json!([{"op": "lane_add", "kind": "video"}, {"op": "generator_add", "kind": "solid", "color": "#ff0000", "lane": "V2", "at": "0s"}]));
        let red = frame(&mut srv, "1s");
        assert_ne!(red, base, "the solid shows");
        ok(&mut srv, json!([{"op": "enable", "clip": "V2@0", "on": false}]));
        assert_eq!(frame(&mut srv, "1s"), base, "a disabled clip is not drawn");
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("disabled"), "{}", tl.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V2@0"}));
        assert!(info.text.contains("disabled"), "{}", info.text);
        ok(&mut srv, json!([{"op": "enable", "clip": "V2@0", "on": true}]));
        assert_eq!(frame(&mut srv, "1s"), red);
    }

    #[test]
    fn label_and_enable_are_one_undo_step_each() {
        let mut srv = server();
        ok(&mut srv, json!([
            {"op": "generator_add", "kind": "solid", "color": "#112233"},
            {"op": "generator_add", "kind": "bars"}
        ]));
        let depth = sess(&srv).undo_depth();
        let t = ok(&mut srv, json!([{"op": "label", "clips": ["V1@0", "V1#1"], "value": "rose"}]));
        assert!(t.contains("V1@0, V1#1 → Rose"), "{t}");
        assert_eq!(sess(&srv).mark_of(Lane::V1, 1).label, 7);
        ok(&mut srv, json!([{"op": "label", "clip": "V1@0", "value": 3}]));
        assert_eq!(sess(&srv).mark_of(Lane::V1, 0).label, 3);
        ok(&mut srv, json!([{"op": "label", "clip": "V1@0", "value": "none"}]));
        assert_eq!(sess(&srv).mark_of(Lane::V1, 0).label, 0);
        assert!(!call(&mut srv, "undo", json!({})).is_error);
        assert_eq!(sess(&srv).mark_of(Lane::V1, 0).label, 3, "undo restores the label");
        assert!(!call(&mut srv, "undo", json!({})).is_error);
        assert!(!call(&mut srv, "undo", json!({})).is_error);
        assert_eq!(sess(&srv).mark_of(Lane::V1, 1).label, 0, "one undo took both labels off");
        assert_eq!(sess(&srv).undo_depth(), depth);
        ok(&mut srv, json!([{"op": "label", "clip": "V1@0", "value": "Caribbean"}]));
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("label Caribbean"), "{}", tl.text);

        let t = ok(&mut srv, json!([{"op": "enable", "clips": ["V1@0", "V1@150"], "on": false}]));
        assert!(t.contains("V1@0, V1@150 → off"), "{t}");
        assert!(sess(&srv).mark_of(Lane::V1, 1).off);
        assert!(!call(&mut srv, "undo", json!({})).is_error);
        assert!(!sess(&srv).mark_of(Lane::V1, 0).off && !sess(&srv).mark_of(Lane::V1, 1).off, "one undo switched both back on");
        assert!(!call(&mut srv, "redo", json!({})).is_error);
        assert!(sess(&srv).mark_of(Lane::V1, 0).off);
    }

    #[test]
    fn label_and_enable_refusals() {
        let mut srv = server();
        ok(&mut srv, json!([{"op": "generator_add", "kind": "bars"}]));
        let t = err(&mut srv, json!([{"op": "label", "clip": "V1@0", "value": "teal"}]));
        assert!(t.contains("not a label") && t.contains("Violet"), "{t}");
        assert!(err(&mut srv, json!([{"op": "label", "clip": "V1@0", "value": 9}])).contains("not a label"));
        assert!(err(&mut srv, json!([{"op": "label", "clip": "V1@0"}])).contains("missing `value`"));
        assert!(err(&mut srv, json!([{"op": "label", "value": 1}])).contains("missing `clip`"));
        assert!(err(&mut srv, json!([{"op": "label", "clip": "V1@99", "value": 1}])).contains("V1"));
        assert!(err(&mut srv, json!([{"op": "enable", "clip": "V1@0"}])).contains("missing `on`"));
        assert!(err(&mut srv, json!([{"op": "enable", "clip": "V1@0", "on": "no"}])).contains("true or false"));
        assert!(err(&mut srv, json!([{"op": "enable", "clip": "V1@0", "clips": ["V1@0"], "on": true}])).contains("not both"));
        assert_eq!(sess(&srv).mark_of(Lane::V1, 0).label, 0);
    }

    #[test]
    fn save_and_reopen_keep_generator_label_and_off() {
        let td = TestDir::new("gens-roundtrip");
        let path = td.path().join("gens.edith");
        let mut srv = Server::new();
        let r = call(&mut srv, "project_new", json!({"path": path, "resolution": "320x180"}));
        assert!(!r.is_error, "{}", r.text);
        ok(&mut srv, json!([
            {"op": "generator_add", "kind": "gradient", "color": "#ff7a18", "color2": "#3a1c71", "angle": 90},
            {"op": "generator_add", "kind": "countdown", "from": 4},
            {"op": "adjustment_add", "at": "0s", "duration": "2s"},
            {"op": "label", "clip": "V1@0", "value": "Rose"},
            {"op": "enable", "clip": "V1@150", "on": false}
        ]));
        let r = call(&mut srv, "project_save", json!({}));
        assert!(!r.is_error, "{}", r.text);

        let mut fresh = Server::new();
        let r = call(&mut fresh, "project_open", json!({"path": path}));
        assert!(!r.is_error, "{}", r.text);
        let s = fresh.session.as_ref().unwrap();
        assert_eq!(s.generator_of(Lane::V1, 0).unwrap().kind, Kind::Gradient { a: 0xff7a18, b: 0x3a1c71, angle: 90 });
        assert!(matches!(s.generator_of(Lane::V1, 1).unwrap().kind, Kind::Countdown { from: 4, .. }));
        assert!(s.generator_of(Lane::new(LaneKind::Video, 1), 0).unwrap().is_adjust());
        assert_eq!(s.mark_of(Lane::V1, 0).label, 7);
        assert!(s.mark_of(Lane::V1, 1).off && !s.mark_of(Lane::V1, 0).off);
        let tl = call(&mut fresh, "timeline", json!({}));
        assert!(tl.text.contains("label Rose") && tl.text.contains("disabled") && !tl.text.contains("OFFLINE"), "{}", tl.text);
    }
}
