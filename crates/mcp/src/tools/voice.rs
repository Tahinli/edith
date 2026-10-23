//! Voice cleanup: the `clean_voice` tool (one click: gate, noise reduction, compressor and
//! de-esser at settings for speech, as one undo step) and the `dynamics` edit op (each
//! processor on its own). The DSP is `engine::voice`; this is the argument handling, the
//! noise learning and the replies.

use engine::PlaybackSession;
use engine::project::{Lane, LaneKind};
use engine::voice::{Compressor, DeEsser, Gate, Learned, Strength, VoiceFx};
use serde_json::{Map, Value, json};

use super::edit_fx::Applied;
use super::hear::record_pushes;
use crate::parse;
use crate::server::{Reply, Server, Tool, opt_str};

const NO_PROJECT: &str = "no project open — call project_open or project_new first";

const CLEAN_VOICE_DOC: &str = r#"One-click voice cleanup for an audio lane, as one undo step: a noise gate, spectral noise reduction, a compressor and a de-esser, set for speech. The noise is learned from the lane itself: by default its quietest half second (a pause between words, found automatically), or the noise_from range you name. Replaces the lane's gate, nr, compressor and deesser settings with the preset; fine-tune each afterwards with edit {ops:[{op:"dynamics", lane, ...}]}, take it all off with edit {ops:[{op:"dynamics", lane, reset:true}]} or undo.
strength: light (gentle: nr 35%, 2:1), medium (default: nr 60%, 3:1) or strong (nr 85%, 4:1, deeper gate). The gate opens a few dB over the learned noise level.
noise_from {from,to}: a stretch of the lane that holds only the noise (room tone, breath-free pause), in the usual times (450f, 15.0s). Give it when the automatic pick lands on something that is not noise.
Order of the processors: the clips' equalizer, gate, noise reduction, compressor, de-esser, then the lane's volume, its duck and the master limiter. They run in playback and in every export, and the sound of an export is the sound you hear."#;

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "clean_voice", description: CLEAN_VOICE_DOC, schema: clean_voice_schema, run: run_clean_voice }]
}

fn clean_voice_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "lane": {
                "type": "string",
                "description": "The audio lane to clean, such as A1. Default A1."
            },
            "strength": {
                "type": "string", "enum": ["light", "medium", "strong"],
                "description": "How hard it works. Default medium."
            },
            "noise_from": {
                "type": "object",
                "description": "A stretch of the lane that holds only noise, from `from` up to but not including `to`. Times take the usual forms: 450f, 15.0s, 00:00:15:00. Default: the quietest half second of the lane, found automatically.",
                "properties": {
                    "from": { "description": "Start of the stretch." },
                    "to": { "description": "End of the stretch (exclusive)." }
                },
                "required": ["from", "to"]
            }
        }
    })
}

/// `{from, to}` as lane seconds.
fn range_secs(v: &Value, what: &str, fps: f64, end: u32) -> Result<(f64, f64), String> {
    let (Some(from), Some(to)) = (v.get("from"), v.get("to")) else {
        return Err(format!("{what} needs both `from` and `to`, e.g. {{\"from\": \"2s\", \"to\": \"2.6s\"}}"));
    };
    let from = parse::frame(from, fps, end).map_err(|e| format!("{what}.from: {e}"))?;
    let to = parse::frame(to, fps, end).map_err(|e| format!("{what}.to: {e}"))?;
    if from >= to {
        return Err(format!("{what}.from must come before {what}.to"));
    }
    Ok((f64::from(from) / fps, f64::from(to) / fps))
}

fn lane_checks(s: &PlaybackSession, lane: Lane, raw: &str, who: &str) -> Result<(), String> {
    if !s.lanes().contains(&lane) {
        let names: Vec<String> = s.lanes().into_iter().map(parse::lane_name).collect();
        return Err(format!("{raw} is not a lane on this timeline; the lanes are {}", names.join(", ")));
    }
    if lane.kind != LaneKind::Audio {
        return Err(format!("{who}: voice cleanup is for audio lanes, and {raw} is not one"));
    }
    if s.lane_clips(lane).is_empty() {
        return Err(format!("{who}: {raw} has no clips to clean"));
    }
    super::edit::check_unlocked(s, &[lane])
}

fn learned_note(l: &Learned, explicit: bool) -> String {
    let at = if explicit {
        format!("{:.2}-{:.2} s", l.from.0, l.from.1)
    } else {
        format!("the quietest stretch, {:.2}-{:.2} s", l.from.0, l.from.1)
    };
    format!("noise learned from {at} ({:.1} dBFS)", l.noise_db)
}

fn run_clean_voice(srv: &mut Server, args: &Value) -> Reply {
    let raw = opt_str(args, "lane").unwrap_or("A1");
    let strength = match opt_str(args, "strength") {
        None => Strength::Medium,
        Some(name) => match Strength::from_name(name) {
            Some(s) => s,
            None => return Reply::err(format!("strength `{name}` is not one of light, medium, strong")),
        },
    };
    let lane = match parse::lane(raw) {
        Ok(l) => l,
        Err(e) => return Reply::err(e),
    };
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let who = format!("clean_voice {raw}");
    if let Err(e) = lane_checks(s, lane, raw, &who) {
        return Reply::err(e);
    }
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let explicit = match args.get("noise_from") {
        None | Some(Value::Null) => None,
        Some(v) => match range_secs(v, "noise_from", fps, end) {
            Ok(r) => Some(r),
            Err(e) => return Reply::err(format!("{who}: {e}")),
        },
    };
    let (learned, nr_note) = match s.learn_noise(lane, explicit) {
        Ok(l) => {
            let note = learned_note(&l, explicit.is_some());
            (Some(l), note)
        }
        // Naming a stretch that cannot be learned from is a mistake to report; the automatic pick
        // finding nothing just means there is no noise to subtract, and the rest still applies.
        Err(e) if explicit.is_some() => return Reply::err(format!("{who}: {e}")),
        Err(e) => (None, format!("noise reduction left off: {e}")),
    };
    let fx = VoiceFx::clean_voice(
        strength,
        learned.as_ref().map(|l| l.profile.clone()).unwrap_or_default(),
        learned.as_ref().map(|l| l.noise_db),
    );
    if s.lane_voice(lane) == fx {
        return Reply::ok(format!("{who} {}: already set · {}", strength.name(), fx.summary()));
    }
    let start = s.history_pushes();
    if !s.set_lane_voice(lane, fx.clone()) {
        return Reply::err(format!("{who}: the engine refused it"));
    }
    record_pushes(srv, start, format!("clean_voice {raw} {}", strength.name()));
    let undo = srv.ledger.len();
    Reply::ok(format!(
        "cleaned {raw} ({}): {} · {nr_note} · undo step {undo}",
        strength.name(),
        fx.summary()
    ))
    .with(json!({
        "lane": raw,
        "strength": strength.name(),
        "voice": voice_json(&fx),
        "noise_db": learned.as_ref().map(|l| l.noise_db),
        "noise_from": learned.as_ref().map(|l| json!([l.from.0, l.from.1])),
        "undo_steps": undo,
    }))
}

/// The processors as JSON, the profile left out (it is 256 numbers nobody reads).
pub fn voice_json(fx: &VoiceFx) -> Value {
    json!({
        "gate": { "on": fx.gate.on, "threshold_db": fx.gate.threshold_db, "attack_ms": fx.gate.attack_ms,
                  "release_ms": fx.gate.release_ms, "hold_ms": fx.gate.hold_ms, "range_db": fx.gate.range_db },
        "nr": { "on": fx.nr.on, "strength": fx.nr.strength, "floor_db": fx.nr.floor_db,
                "profile": !fx.nr.profile.is_empty() },
        "compressor": { "on": fx.comp.on, "threshold_db": fx.comp.threshold_db, "ratio": fx.comp.ratio,
                        "attack_ms": fx.comp.attack_ms, "release_ms": fx.comp.release_ms,
                        "knee_db": fx.comp.knee_db, "makeup_db": fx.comp.makeup(),
                        "auto_makeup": fx.comp.auto_makeup },
        "deesser": { "on": fx.deess.on, "threshold_db": fx.deess.threshold_db, "amount": fx.deess.amount,
                     "lo_hz": fx.deess.lo_hz, "hi_hz": fx.deess.hi_hz },
    })
}

// ---- the dynamics op ---------------------------------------------------------------

/// How one processor was addressed: `false` bypasses it, `true` switches it on as it is, an
/// object sets numbers (and switches it on unless it says `on:false`).
struct Addressed<'a> {
    on: bool,
    nums: Option<&'a Map<String, Value>>,
}

fn addressed<'a>(op: &'a Value, key: &str) -> Result<Option<Addressed<'a>>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(Addressed { on: *b, nums: None })),
        Some(Value::Object(m)) => {
            let on = match m.get("on") {
                None | Some(Value::Null) => true,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(format!("dynamics: {key}.on must be true or false")),
            };
            Ok(Some(Addressed { on, nums: Some(m) }))
        }
        Some(_) => Err(format!("dynamics: {key} must be true, false or an object of settings")),
    }
}

fn check_keys(key: &str, m: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match m.keys().find(|k| *k != "on" && !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("dynamics: {key} has no `{k}`; it takes on, {}", allowed.join(", "))),
        None => Ok(()),
    }
}

fn number(who: &str, m: Option<&Map<String, Value>>, k: &str, cur: f32, lo: f32, hi: f32) -> Result<f32, String> {
    let Some(v) = m.and_then(|m| m.get(k)).filter(|v| !v.is_null()) else {
        return Ok(cur);
    };
    let x = v.as_f64().filter(|x| x.is_finite()).ok_or_else(|| format!("{who}: {k} must be a number"))?;
    if x < f64::from(lo) || x > f64::from(hi) {
        return Err(format!("{who}: {k} {x} out of range {lo}–{hi}"));
    }
    Ok(x as f32)
}

fn boolean(who: &str, m: Option<&Map<String, Value>>, k: &str) -> Result<Option<bool>, String> {
    match m.and_then(|m| m.get(k)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{who}: {k} must be true or false")),
    }
}

/// `dynamics {lane, gate?, nr?, compressor?, deesser?, reset?}`: sets the voice processors of an
/// audio lane one by one; what an op does not name stays as it was. Each processor takes `false`
/// (bypass, keeping its settings), `true` (on as it is) or an object of numbers (on, unless it
/// says `on:false`):
///
/// * `gate {threshold_db -90..0, attack_ms, release_ms, hold_ms, range_db -90..0}`
/// * `nr {strength 0..1, floor_db -60..0, from?: {from,to}, learn?}` -- switching it on learns the
///   noise profile from the quietest half second of the lane, or from `from`; `learn:true` learns
///   it again; a profile already there is kept otherwise
/// * `compressor {threshold_db -60..0, ratio 1..20, attack_ms, release_ms, knee_db 0..30, makeup_db
///   -12..24, auto_makeup}` -- a makeup_db without auto_makeup switches automatic makeup off
/// * `deesser {threshold_db -60..0, amount 0..1, lo_hz, hi_hz}`
///
/// `reset:true` takes every processor off and back to its defaults.
pub fn dynamics(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let raw = op.get("lane").and_then(Value::as_str).ok_or("dynamics: missing `lane`, an audio lane such as \"A1\"")?;
    let lane = parse::lane(raw)?;
    let who = format!("dynamics {raw}");
    lane_checks(s, lane, raw, &who)?;
    let before = s.lane_voice(lane);
    let mut fx = before.clone();
    let reset = matches!(op.get("reset"), Some(Value::Bool(true)));
    if reset {
        fx = VoiceFx::default();
    }
    let mut notes = Vec::new();

    if let Some(a) = addressed(op, "gate")? {
        if let Some(m) = a.nums {
            check_keys("gate", m, &["threshold_db", "attack_ms", "release_ms", "hold_ms", "range_db"])?;
        }
        let who = format!("{who} gate");
        let g = fx.gate;
        let n = a.nums;
        fx.gate = Gate {
            on: a.on,
            threshold_db: number(&who, n, "threshold_db", g.threshold_db, -90.0, 0.0)?,
            attack_ms: number(&who, n, "attack_ms", g.attack_ms, 0.1, 100.0)?,
            release_ms: number(&who, n, "release_ms", g.release_ms, 1.0, 3000.0)?,
            hold_ms: number(&who, n, "hold_ms", g.hold_ms, 0.0, 2000.0)?,
            range_db: number(&who, n, "range_db", g.range_db, -90.0, 0.0)?,
        };
    }
    if let Some(a) = addressed(op, "compressor")? {
        if let Some(m) = a.nums {
            check_keys(
                "compressor",
                m,
                &["threshold_db", "ratio", "attack_ms", "release_ms", "knee_db", "makeup_db", "auto_makeup"],
            )?;
        }
        let who = format!("{who} compressor");
        let c = fx.comp;
        let n = a.nums;
        let makeup_given = n.is_some_and(|m| m.get("makeup_db").is_some_and(|v| !v.is_null()));
        fx.comp = Compressor {
            on: a.on,
            threshold_db: number(&who, n, "threshold_db", c.threshold_db, -60.0, 0.0)?,
            ratio: number(&who, n, "ratio", c.ratio, 1.0, 20.0)?,
            attack_ms: number(&who, n, "attack_ms", c.attack_ms, 0.1, 500.0)?,
            release_ms: number(&who, n, "release_ms", c.release_ms, 5.0, 5000.0)?,
            knee_db: number(&who, n, "knee_db", c.knee_db, 0.0, 30.0)?,
            makeup_db: number(&who, n, "makeup_db", c.makeup_db, -12.0, 24.0)?,
            auto_makeup: boolean(&who, n, "auto_makeup")?.unwrap_or(c.auto_makeup && !makeup_given),
        };
    }
    if let Some(a) = addressed(op, "deesser")? {
        if let Some(m) = a.nums {
            check_keys("deesser", m, &["threshold_db", "amount", "lo_hz", "hi_hz"])?;
        }
        let who = format!("{who} deesser");
        let d = fx.deess;
        let n = a.nums;
        fx.deess = DeEsser {
            on: a.on,
            threshold_db: number(&who, n, "threshold_db", d.threshold_db, -60.0, 0.0)?,
            amount: number(&who, n, "amount", d.amount, 0.0, 1.0)?,
            lo_hz: number(&who, n, "lo_hz", d.lo_hz, 1000.0, 12000.0)?,
            hi_hz: number(&who, n, "hi_hz", d.hi_hz, 2000.0, 16000.0)?,
        };
        if fx.deess.clamped() != Some(fx.deess) {
            return Err(format!("{who}: hi_hz must be above lo_hz (by more than 10%)"));
        }
    }
    if let Some(a) = addressed(op, "nr")? {
        let mut explicit = None;
        let mut relearn = false;
        if let Some(m) = a.nums {
            check_keys("nr", m, &["strength", "floor_db", "from", "learn"])?;
            if let Some(r) = m.get("from").filter(|v| !v.is_null()) {
                explicit = Some(range_secs(r, "nr.from", fps, parse::session_end(s)).map_err(|e| format!("{who}: {e}"))?);
            }
            relearn = boolean(&who, Some(m), "learn")?.unwrap_or(false);
        }
        let who = format!("{who} nr");
        let n = a.nums;
        fx.nr.on = a.on;
        fx.nr.strength = number(&who, n, "strength", fx.nr.strength, 0.0, 1.0)?;
        fx.nr.floor_db = number(&who, n, "floor_db", fx.nr.floor_db, -60.0, 0.0)?;
        if a.on && (fx.nr.profile.is_empty() || explicit.is_some() || relearn) {
            let l = s.learn_noise(lane, explicit).map_err(|e| format!("{who}: {e}"))?;
            notes.push(learned_note(&l, explicit.is_some()));
            fx.nr.profile = l.profile;
        }
    }
    if op.get("gate").is_none()
        && op.get("nr").is_none()
        && op.get("compressor").is_none()
        && op.get("deesser").is_none()
        && !reset
    {
        return Err(format!(
            "{who}: name at least one of gate, nr, compressor, deesser (each true, false or an object of settings), or reset:true"
        ));
    }
    if fx == before {
        return Ok(Applied { text: format!("{who} → already set ({})", summary_or_off(&fx)), touched: Vec::new() });
    }
    if !s.set_lane_voice(lane, fx.clone()) {
        return Err(format!("{who}: the engine refused it (is the lane an audio lane with a profile for the noise reduction?)"));
    }
    let mut text = format!("{who} → {}", summary_or_off(&fx));
    for n in notes {
        text.push_str(" · ");
        text.push_str(&n);
    }
    Ok(Applied { text, touched: Vec::new() })
}

fn summary_or_off(fx: &VoiceFx) -> String {
    match fx.summary() {
        s if s.is_empty() => "no processors on".to_string(),
        s => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_processor_is_addressed_by_bool_or_object() {
        let op = json!({"gate": false, "compressor": true, "deesser": {"amount": 0.5, "on": false}});
        let g = addressed(&op, "gate").unwrap().unwrap();
        assert!(!g.on && g.nums.is_none());
        assert!(addressed(&op, "compressor").unwrap().unwrap().on);
        let d = addressed(&op, "deesser").unwrap().unwrap();
        assert!(!d.on && d.nums.is_some());
        assert!(addressed(&op, "nr").unwrap().is_none());
        assert!(addressed(&json!({"gate": 3}), "gate").is_err());
        assert!(addressed(&json!({"gate": {"on": "yes"}}), "gate").is_err());
        let m = json!({"threshold": 1});
        assert!(check_keys("gate", m.as_object().unwrap(), &["threshold_db"]).unwrap_err().contains("no `threshold`"));
    }

    #[test]
    fn numbers_are_range_checked_by_name() {
        let m = json!({"ratio": 40});
        let e = number("dynamics A1 compressor", m.as_object(), "ratio", 3.0, 1.0, 20.0).unwrap_err();
        assert_eq!(e, "dynamics A1 compressor: ratio 40 out of range 1–20");
        assert_eq!(number("x", m.as_object(), "knee_db", 6.0, 0.0, 30.0), Ok(6.0), "unnamed keeps its value");
        let s = json!({"ratio": "x"});
        assert!(number("x", s.as_object(), "ratio", 3.0, 1.0, 20.0).unwrap_err().contains("must be a number"));
    }

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    #[test]
    fn dynamics_sets_processors_one_by_one_and_undoes_per_op() {
        let mut s = session();
        let a1 = Lane::new(LaneKind::Audio, 0);
        let a = dynamics(&mut s, &json!({"lane": "A1", "compressor": {"ratio": 4, "threshold_db": -24}, "gate": true}), 30.0)
            .unwrap();
        assert!(a.text.contains("comp") && a.text.contains("gate"), "{}", a.text);
        let fx = s.lane_voice(a1);
        assert!(fx.comp.on && fx.gate.on && !fx.nr.on && !fx.deess.on);
        assert_eq!(fx.comp.ratio, 4.0);
        // Bypass keeps the settings.
        dynamics(&mut s, &json!({"lane": "A1", "compressor": false}), 30.0).unwrap();
        let fx = s.lane_voice(a1);
        assert!(!fx.comp.on && fx.comp.ratio == 4.0 && fx.gate.on);
        assert!(s.undo());
        assert!(s.lane_voice(a1).comp.on, "undo brings the compressor back");
        assert!(s.undo());
        assert!(!s.lane_voice(a1).is_active(), "second undo leaves everything off");
        // Reset.
        dynamics(&mut s, &json!({"lane": "A1", "deesser": true}), 30.0).unwrap();
        dynamics(&mut s, &json!({"lane": "A1", "reset": true}), 30.0).unwrap();
        assert!(s.lane_voice(a1).is_default());
    }

    #[test]
    fn dynamics_refuses_bad_input_by_name() {
        let mut s = session();
        let e = dynamics(&mut s, &json!({"lane": "A1", "compressor": {"ratio": 99}}), 30.0).unwrap_err();
        assert!(e.contains("ratio 99 out of range"), "{e}");
        let e = dynamics(&mut s, &json!({"lane": "A1", "gate": {"thresh": 1}}), 30.0).unwrap_err();
        assert!(e.contains("no `thresh`"), "{e}");
        let e = dynamics(&mut s, &json!({"lane": "V1", "gate": true}), 30.0).unwrap_err();
        assert!(e.contains("not one"), "{e}");
        assert!(dynamics(&mut s, &json!({"gate": true}), 30.0).unwrap_err().contains("missing `lane`"));
        s.set_lane_locked(Lane::new(LaneKind::Audio, 0), true);
        let e = dynamics(&mut s, &json!({"lane": "A1", "gate": true}), 30.0).unwrap_err();
        assert!(e.contains("locked"), "{e}");
        assert!(!s.lane_voice(Lane::new(LaneKind::Audio, 0)).is_active());
    }

    #[test]
    fn clean_voice_is_one_undo_step_and_the_lane_view_names_the_processors() {
        let mut srv = crate::server::Server::new();
        srv.session = Some(session());
        let r = run_clean_voice(&mut srv, &json!({"lane": "A1", "strength": "medium"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("cleaned A1 (medium)") && r.text.contains("undo step 1"), "{}", r.text);
        let s = srv.session.as_mut().unwrap();
        let a1 = Lane::new(LaneKind::Audio, 0);
        let fx = s.lane_voice(a1);
        assert!(fx.gate.on && fx.comp.on && fx.deess.on);
        assert!(s.undo(), "one undo step");
        assert!(s.lane_voice(a1).is_default());
        let bad = run_clean_voice(&mut srv, &json!({"lane": "A1", "strength": "loud"}));
        assert!(bad.is_error && bad.text.contains("light, medium, strong"), "{}", bad.text);
        let video = run_clean_voice(&mut srv, &json!({"lane": "V1"}));
        assert!(video.is_error, "{}", video.text);
    }
}
