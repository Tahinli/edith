//! Auto reframe: the `reframe` edit op. The subject search and the keys are `engine::reframe`; this
//! is the argument handling and the reply.

use std::sync::atomic::{AtomicBool, AtomicU32};

use engine::PlaybackSession;
use engine::project::LaneKind;
use engine::reframe;
use serde_json::Value;

use super::edit_fx::{Applied, transform_keys_text};

pub const REFRAME_OP_DOC: &str = "reframe {clip, aspect?, smoothness?} — auto reframe: follows the subject of a video clip and writes transform keys so the clip fills the project canvas, e.g. a 16:9 shot in a 9:16 project. \
     The subject is found from the footage itself (what moves, contrast, a pull to the middle; no models), the path is smoothed with a dead zone so the crop holds still while the subject stays put, and keys are written only where the path turns. \
     The scale makes the clip cover the canvas, so no frame ever shows an edge. It sets the clip's fit to fit (the placement the keys are made for) and replaces the clip's transform keys; it is one undo step, and the keys are ordinary: refine them with transform_keys, take them off with transform_keys {clip, clear:true}. \
     aspect: \"fit\" (default) is the project canvas; or a shape such as \"9:16\", which must be the project's own shape (set the size first with project_settings) — it guards against reframing for the wrong canvas. \
     smoothness 0 to 1 (default 0.5): how slowly the crop follows, about 0.25 + 1.75 × smoothness seconds. \
     It analyses the clip before it answers (a minute of 1080p takes a few seconds). A clip's display rotation is not accounted for. \
     e.g. project_settings {resolution:\"1080x1920\"}, then edit {ops:[{op:\"reframe\", clip:\"V1@0\", smoothness:0.6}]}";

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

/// `9:16`, `9/16` or `0.5625` as a width over a height.
fn aspect_of(text: &str) -> Option<f64> {
    let t = text.trim();
    let v = match t.split_once([':', '/', 'x']) {
        Some((w, h)) => w.trim().parse::<f64>().ok()? / h.trim().parse::<f64>().ok()?,
        None => t.parse::<f64>().ok()?,
    };
    (v.is_finite() && v > 0.0).then_some(v)
}

/// The `reframe` edit op.
pub fn op(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let raw = match op.get("clip") {
        Some(Value::String(c)) => c.as_str(),
        None | Some(Value::Null) => return Err("missing argument `clip`".into()),
        Some(v) => return Err(format!("`clip` must be a string, got {}", kind_of(v))),
    };
    let (lane, idx) = super::edit::clip_addr(s, raw)?;
    super::edit::check_unlocked(s, &[lane])?;
    let who = format!("reframe {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: reframing is for video clips, and {raw} is not one"));
    }
    let smoothness = match op.get("smoothness") {
        None | Some(Value::Null) => reframe::DEFAULT_SMOOTHNESS,
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(f64::NAN);
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{who}: smoothness {v} out of range 0–1"));
            }
            v as f32
        }
        Some(v) => return Err(format!("{who}: `smoothness` must be a number from 0 to 1, got {}", kind_of(v))),
    };
    let (cw, ch) = (s.meta().width, s.meta().height);
    match op.get("aspect") {
        None | Some(Value::Null) => {}
        Some(Value::String(a)) if a.trim().eq_ignore_ascii_case("fit") => {}
        Some(Value::String(a)) => {
            let want = aspect_of(a).ok_or_else(|| format!("{who}: aspect `{a}` is not fit or a shape such as 9:16"))?;
            let have = f64::from(cw) / f64::from(ch);
            if (want / have - 1.0).abs() > 0.02 {
                return Err(format!(
                    "{who}: the project canvas is {cw}x{ch}, not {a}; set it first (project_settings {{resolution:\"…\"}}) and reframe again, or use aspect:\"fit\""
                ));
            }
        }
        Some(v) => return Err(format!("{who}: `aspect` must be \"fit\" or a shape such as \"9:16\", got {}", kind_of(v))),
    }
    let start = s.lane_clips(lane).get(idx).map_or(0, |c| c.start);
    let job = s.reframe_job(lane, idx).ok_or_else(|| format!("{who}: a still or a song has no subject to follow"))?;
    let (progress, cancel) = (AtomicU32::new(0), AtomicBool::new(false));
    let subject = reframe::analyse(&job.path, job.lo, job.hi, &progress, &cancel).map_err(|e| format!("{who}: {e}"))?;
    let keys = reframe::keys(&subject, &job.target, smoothness);
    let n = keys.len();
    let scale = keys[0].params.scale;
    if !s.apply_reframe(lane, idx, keys) {
        return Err(format!("{who}: the engine refused the keys"));
    }
    let clip = s.lane_clips(lane)[idx].clone();
    let shown = transform_keys_text(&clip, s.meta().frame_rate, s.transform_keys_of(lane, idx));
    Ok(Applied {
        text: format!("{who} → {n} key(s), scale {scale:.2} to cover {cw}x{ch}, smoothness {smoothness}; {shown}"),
        touched: vec![(lane, start)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    #[test]
    fn the_op_doc_names_every_argument() {
        for arg in ["aspect", "smoothness", "transform_keys", "undo step"] {
            assert!(REFRAME_OP_DOC.contains(arg), "{arg}");
        }
    }

    #[test]
    fn aspects_parse() {
        assert_eq!(aspect_of("9:16"), Some(0.5625));
        assert_eq!(aspect_of("16/9"), Some(16.0 / 9.0));
        assert_eq!(aspect_of("0:1"), None);
        assert_eq!(aspect_of("wide"), None);
    }

    #[test]
    fn reframing_into_the_wrong_shape_is_refused_and_a_vertical_one_writes_keys_in_one_undo() {
        let mut s = session();
        let lane = engine::project::Lane::V1;
        let err = op(&mut s, &json!({"op": "reframe", "clip": "V1#0", "aspect": "9:16"})).expect_err("16:9 canvas");
        assert!(err.contains("project_settings"), "{err}");
        let err = op(&mut s, &json!({"op": "reframe", "clip": "V1#0", "smoothness": 2})).expect_err("range");
        assert!(err.contains("0–1"), "{err}");
        assert!(op(&mut s, &json!({"op": "reframe", "clip": "V1#0", "aspect": "banana"})).is_err());

        assert!(s.set_resolution(1080, 1920), "vertical canvas");
        let applied = op(&mut s, &json!({"op": "reframe", "clip": "V1#0", "aspect": "9:16"})).expect("reframed");
        assert!(applied.text.contains("key(s)"), "{}", applied.text);
        let keys = s.transform_keys_of(lane, 0).to_vec();
        assert!(!keys.is_empty() && keys[0].params.scale > 3.0, "{keys:?}");
        assert_eq!(s.fit_of(lane, 0), engine::scale::FitPolicy::Fit);
        assert!(s.undo(), "one undo");
        assert!(s.transform_keys_of(lane, 0).is_empty(), "the keys are gone in that one step");
    }
}
