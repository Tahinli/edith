//! Time, lane and clip-address parsing and formatting for the MCP tools.
//!
//! Frames are the truth; seconds are for the reader (MCP.md section 3).
#![allow(dead_code)]

use engine::project::{Lane, LaneKind};
use engine::PlaybackSession;
use serde_json::Value;

use crate::server::Reply;

const TIME_FORMS: &str =
    "450f (frames), 15.0s or 15 (seconds), MM:SS.fff, HH:MM:SS.fff, HH:MM:SS:FF (timecode at the project fps), or end";

/// Parses one time argument into a timeline frame.
///
/// A JSON number is seconds. A string is one of `450f`, `15.0s`, `15`, `1.5`,
/// `MM:SS(.fff)`, `HH:MM:SS(.fff)`, `HH:MM:SS:FF` or `end`. Seconds round to the
/// nearest frame. Negative times are refused.
pub fn frame(v: &Value, fps: f64, end: u32) -> Result<u32, String> {
    if !(fps > 0.0 && fps.is_finite()) {
        return Err(format!("the project has no usable frame rate ({fps})"));
    }
    match v {
        Value::Number(n) => {
            let shown = n.to_string();
            from_secs(n.as_f64().unwrap_or(f64::NAN), fps, &shown)
        }
        Value::String(s) => parse_str(s, fps, end),
        Value::Null => Err("a time is required, not null".to_string()),
        Value::Bool(_) => Err("a time must be a number of seconds or a string such as 450f, 15.0s or end; got a boolean".to_string()),
        Value::Array(_) => Err("a time must be a number of seconds or a string such as 450f, 15.0s or end; got a list".to_string()),
        Value::Object(_) => Err("a time must be a number of seconds or a string such as 450f, 15.0s or end; got an object".to_string()),
    }
}

/// A signed time distance, for `by` arguments: the same forms as [`frame`], with a leading `-` or
/// `+` on a string (`-15f`, `+0.5s`, `-00:00:01:00`) or a negative JSON number (seconds).
/// `end` is not a distance. Returns the signed frame count.
pub fn delta(v: &Value, fps: f64) -> Result<i64, String> {
    let (neg, rest) = match v {
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            (x < 0.0, Value::from(x.abs()))
        }
        Value::String(s) => {
            let t = s.trim();
            match t.strip_prefix('-') {
                Some(r) => (true, Value::from(r.trim())),
                None => (false, Value::from(t.strip_prefix('+').unwrap_or(t).trim())),
            }
        }
        other => (false, other.clone()),
    };
    if rest.as_str().is_some_and(|t| t.eq_ignore_ascii_case("end")) {
        return Err("`end` is a place on the timeline, not a distance: give a time such as 15f or -0.5s".to_string());
    }
    let n = i64::from(frame(&rest, fps, 0)?);
    Ok(if neg { -n } else { n })
}

/// A required time argument. Missing or null is an error naming the argument.
pub fn frame_arg(args: &Value, name: &str, fps: f64, end: u32) -> Result<u32, Reply> {
    match opt_frame_arg(args, name, fps, end)? {
        Some(f) => Ok(f),
        None => Err(Reply::err(format!(
            "missing argument `{name}`: give a time such as 450f, 15.0s, 00:00:15:00 or end"
        ))),
    }
}

/// An optional time argument. Missing or null is `None`.
pub fn opt_frame_arg(args: &Value, name: &str, fps: f64, end: u32) -> Result<Option<u32>, Reply> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => frame(v, fps, end)
            .map(Some)
            .map_err(|e| Reply::err(format!("argument `{name}`: {e}"))),
    }
}

/// `V1`, `v1`, `A2`, `S1` to a lane. Numbering starts at 1, so `V1` is ord 0.
pub fn lane(s: &str) -> Result<Lane, String> {
    let t = s.trim();
    let mut chars = t.chars();
    let kind = match chars.next().map(|c| c.to_ascii_uppercase()) {
        Some('V') => LaneKind::Video,
        Some('A') => LaneKind::Audio,
        Some('S') => LaneKind::Subtitle,
        _ => return Err(not_lane(s)),
    };
    let n: usize = chars.as_str().parse().map_err(|_| not_lane(s))?;
    if n == 0 {
        return Err(format!("`{s}` is not a lane: lanes are numbered from 1, so V1 is the first video lane"));
    }
    Ok(Lane::new(kind, n - 1))
}

/// The inverse of [`lane`]: `Lane{Audio, 1}` to `"A2"`.
pub fn lane_name(l: Lane) -> String {
    let p = match l.kind {
        LaneKind::Video => 'V',
        LaneKind::Audio => 'A',
        LaneKind::Subtitle => 'S',
    };
    format!("{p}{}", l.ord + 1)
}

/// Resolves a clip address to its lane and index into that lane.
///
/// `V1@450` names the clip starting at frame 450 (a bare number is frames).
/// `V1@15.0s` and the other time forms also work. `V1#0` is the first clip's index (0-based),
/// bounds checked. Subtitle lanes resolve through their own placements.
pub fn clip(s: &PlaybackSession, addr: &str) -> Result<(Lane, usize), String> {
    let a = addr.trim();
    let split = a
        .find(['@', '#'])
        .ok_or_else(|| format!("`{addr}` is not a clip address: write LANE@START (e.g. V1@450) or LANE#INDEX (e.g. V1#0)"))?;
    let ln = lane(&a[..split])?;
    let rest = a[split + 1..].trim();
    let all = spans(s, ln);

    if a.as_bytes()[split] == b'#' {
        let idx: usize = rest.parse().map_err(|_| {
            format!("`{rest}` is not a clip index: use {}#0 for the first clip", lane_name(ln))
        })?;
        if idx >= all.len() {
            return Err(format!(
                "{}#{idx} is out of range: {}",
                lane_name(ln),
                starts_hint(ln, &all)
            ));
        }
        return Ok((ln, idx));
    }

    let bare = rest.parse::<u32>();
    let start = match bare {
        Ok(f) => f,
        Err(_) => frame(&Value::String(rest.to_string()), session_fps(s), session_end(s))
            .map_err(|e| format!("{a}: {e}"))?,
    };
    if let Some(i) = all.iter().position(|&(st, _)| st == start) {
        return Ok((ln, i));
    }
    let frame_note = if bare.is_ok() { frame_hint(s, ln, start, &all) } else { String::new() };
    if let Some(&(st, en)) = all.iter().find(|&&(st, en)| st <= start && start < en) {
        return Err(format!(
            "no clip starts at {}; {}@{st} spans {st}–{en} — use that address{frame_note}",
            addr_of(ln, start),
            lane_name(ln),
        ));
    }
    Err(format!("no clip at {}; {}{frame_note}", addr_of(ln, start), nearest_starts(ln, &all, start)))
}

/// `V1@450`: the address of a clip starting at `start` on `lane`.
pub fn addr(lane: Lane, start: u32) -> String {
    addr_of(lane, start)
}

/// `450f (15.000s)`: a frame in both forms, as responses give it.
pub fn tc(frame: u32, fps: f64) -> String {
    format!("{frame}f ({:.3}s)", secs(frame, fps))
}

/// Seconds of a frame at `fps`.
pub fn secs(frame: u32, fps: f64) -> f64 {
    f64::from(frame) / fps
}

/// `00:15.000`, or `1:02:03.500` once it reaches an hour.
pub fn clock(frame: u32, fps: f64) -> String {
    let ms = (secs(frame, fps) * 1000.0).round() as u64;
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

/// The project frame rate.
pub fn session_fps(s: &PlaybackSession) -> f64 {
    s.meta().frame_rate
}

/// The timeline end in frames.
pub fn session_end(s: &PlaybackSession) -> u32 {
    (s.timeline_duration() * session_fps(s)).round() as u32
}

// ---- helpers --------------------------------------------------------------

fn addr_of(lane: Lane, start: u32) -> String {
    format!("{}@{start}", lane_name(lane))
}

fn not_lane(s: &str) -> String {
    format!("`{s}` is not a lane: write V1.. for video, A1.. for audio, S1.. for subtitles")
}

fn bad(raw: &str) -> String {
    format!("`{raw}` is not a time: accepted forms are {TIME_FORMS}")
}

fn spans(s: &PlaybackSession, lane: Lane) -> Vec<(u32, u32)> {
    match lane.kind {
        LaneKind::Subtitle => s
            .sub_lane(lane)
            .iter()
            .map(|c| (c.start, c.start.saturating_add(c.frames)))
            .collect(),
        _ => s.lane_clips(lane).iter().map(|c| (c.start, c.end())).collect(),
    }
}

/// The three clip starts on a lane nearest a frame that has no clip, in time order.
fn nearest_starts(lane: Lane, all: &[(u32, u32)], start: u32) -> String {
    if all.is_empty() {
        return format!("{} has no clips", lane_name(lane));
    }
    let mut near: Vec<u32> = all.iter().map(|&(st, _)| st).collect();
    near.sort_by_key(|&st| (st.abs_diff(start), st));
    near.truncate(3);
    near.sort_unstable();
    let shown: Vec<String> = near.iter().map(|&st| addr_of(lane, st)).collect();
    format!("{} has {} clip(s), nearest starts {}", lane_name(lane), all.len(), shown.join(", "))
}

/// Why a bare `@N` with no clip is probably a mix-up: addresses count frames, so a clip that
/// starts at N seconds is named by its frame. Names that clip when one starts there.
fn frame_hint(s: &PlaybackSession, lane: Lane, n: u32, all: &[(u32, u32)]) -> String {
    let fps = session_fps(s);
    let as_secs = (f64::from(n) * fps).round() as u32;
    if all.iter().any(|&(st, _)| st == as_secs) {
        return format!(" (addresses are frames: {n} s is {} at {fps} fps)", addr_of(lane, as_secs));
    }
    format!(" (addresses are frames: {} is {:.3} s in)", addr_of(lane, n), secs(n, fps))
}

/// What a lane holds, for an error to list: at most eight addresses.
fn starts_hint(lane: Lane, all: &[(u32, u32)]) -> String {
    const MAX: usize = 8;
    if all.is_empty() {
        return format!("{} has no clips", lane_name(lane));
    }
    let shown: Vec<String> = all.iter().take(MAX).map(|&(st, _)| addr_of(lane, st)).collect();
    let more = if all.len() > MAX {
        format!(", … {} more", all.len() - MAX)
    } else {
        String::new()
    };
    format!(
        "{} has {} clip(s), starting at {}{more}",
        lane_name(lane),
        all.len(),
        shown.join(", ")
    )
}

fn strip_suffix_ci(s: &str, suffix: char) -> Option<&str> {
    let last = s.chars().last()?;
    if last.eq_ignore_ascii_case(&suffix) {
        Some(&s[..s.len() - last.len_utf8()])
    } else {
        None
    }
}

fn parse_str(raw: &str, fps: f64, end: u32) -> Result<u32, String> {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("end") {
        return Ok(end);
    }
    if s.is_empty() {
        return Err(bad(raw));
    }
    if let Some(num) = strip_suffix_ci(s, 'f') {
        return frames_form(num, raw);
    }
    if let Some(num) = strip_suffix_ci(s, 's') {
        return seconds_form(num, fps, raw);
    }
    if s.contains(':') {
        return clock_form(s, fps, raw);
    }
    seconds_form(s, fps, raw)
}

fn frames_form(num: &str, raw: &str) -> Result<u32, String> {
    let t = num.trim();
    if t.starts_with('-') {
        return Err(format!("`{raw}` is negative; a time cannot be before the start of the timeline"));
    }
    t.parse::<u32>().map_err(|_| {
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            format!("`{raw}` is too large: frames go up to {}", u32::MAX)
        } else {
            format!("`{raw}` is not a frame count: write whole frames as 450f")
        }
    })
}

fn seconds_form(num: &str, fps: f64, raw: &str) -> Result<u32, String> {
    let t = num.trim();
    if t.starts_with('-') {
        return Err(format!("`{raw}` is negative; a time cannot be before the start of the timeline"));
    }
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => from_secs(v, fps, raw),
        _ => Err(bad(raw)),
    }
}

/// `MM:SS(.fff)`, `HH:MM:SS(.fff)` or the timecode `HH:MM:SS:FF`.
fn clock_form(s: &str, fps: f64, raw: &str) -> Result<u32, String> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        4 => timecode_form(&parts, fps, raw),
        2 | 3 => {
            let (h, m, sec) = if parts.len() == 3 {
                (whole(parts[0], raw)?, whole(parts[1], raw)?, parts[2])
            } else {
                (0, whole(parts[0], raw)?, parts[1])
            };
            if m >= 60 {
                return Err(format!("`{raw}`: minutes must be below 60 (use HH:MM:SS for longer times)"));
            }
            let sec_v = fraction_secs(sec, raw)?;
            if sec_v >= 60.0 {
                return Err(format!("`{raw}`: seconds must be below 60"));
            }
            from_secs(h as f64 * 3600.0 + m as f64 * 60.0 + sec_v, fps, raw)
        }
        _ => Err(bad(raw)),
    }
}

/// `HH:MM:SS:FF`, FF counted in whole frames at the project fps.
fn timecode_form(parts: &[&str], fps: f64, raw: &str) -> Result<u32, String> {
    let h = whole(parts[0], raw)?;
    let m = whole(parts[1], raw)?;
    let s = whole(parts[2], raw)?;
    let f = whole(parts[3], raw)?;
    if m >= 60 || s >= 60 {
        return Err(format!("`{raw}`: minutes and seconds must be below 60"));
    }
    let per_second = fps.ceil() as u64;
    if f >= per_second {
        return Err(format!(
            "`{raw}`: the frame field FF must be below {per_second} at this project's frame rate"
        ));
    }
    let base = ((h * 3600 + m * 60 + s) as f64 * fps).round();
    from_secs(base + f as f64, 1.0, raw)
}

fn whole(p: &str, raw: &str) -> Result<u64, String> {
    let t = p.trim();
    if t.starts_with('-') {
        return Err(format!("`{raw}` is negative; a time cannot be before the start of the timeline"));
    }
    t.parse::<u64>()
        .map_err(|_| format!("`{raw}`: `{p}` should be a whole number"))
}

fn fraction_secs(p: &str, raw: &str) -> Result<f64, String> {
    let t = p.trim();
    if t.starts_with('-') {
        return Err(format!("`{raw}` is negative; a time cannot be before the start of the timeline"));
    }
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(format!("`{raw}`: `{p}` should be seconds, such as 03 or 03.500")),
    }
}

/// Seconds to a frame, rounded to the nearest. `shown` names the input in errors.
fn from_secs(secs: f64, fps: f64, shown: &str) -> Result<u32, String> {
    if !secs.is_finite() {
        return Err(format!("`{shown}` is not a finite time"));
    }
    if secs < 0.0 {
        return Err(format!("`{shown}` is negative; a time cannot be before the start of the timeline"));
    }
    let f = (secs * fps).round();
    if f > f64::from(u32::MAX) {
        // Plain digits, never an exponent: the seconds to the nearest whole second. Past 1e15 s
        // the digits would run on, so the reply says only that it is far past the end.
        let days = f64::from(u32::MAX) / fps / 86_400.0;
        if secs < 1e15 {
            return Err(format!(
                "{secs:.0} s is past the longest timeline edith supports (about {days:.0} days at {fps} fps)"
            ));
        }
        return Err(format!(
            "that is far past the longest timeline edith supports (about {days:.0} days at {fps} fps)"
        ));
    }
    Ok(f as u32)
}

/// The JSON kind of an argument, for the `got …` in a refusal.
pub fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The address argument `key` as text. A missing key names the example form; a value
/// that is not a string says what it was: `` `clip` must be an address like "V1@0", got number ``.
pub fn addr_str<'a>(op: &'a Value, key: &str, example: &str) -> Result<&'a str, String> {
    match op.get(key) {
        None | Some(Value::Null) => Err(format!("missing `{key}`: give an address such as {example}")),
        Some(Value::String(t)) => Ok(t.trim()),
        Some(v) => Err(format!("`{key}` must be an address like \"{}\", got {}", example.trim_matches('"'), kind_of(v))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    const FPS: f64 = 30.0;
    const END: u32 = 9000;

    #[test]
    fn a_signed_distance_takes_every_time_form() {
        let d = |v: Value| delta(&v, FPS);
        assert_eq!(d(json!("15f")), Ok(15));
        assert_eq!(d(json!("-15f")), Ok(-15));
        assert_eq!(d(json!("+0.5s")), Ok(15));
        assert_eq!(d(json!("-0.5s")), Ok(-15));
        assert_eq!(d(json!(-1.0)), Ok(-30));
        assert_eq!(d(json!(2)), Ok(60));
        assert_eq!(d(json!("-00:00:01:00")), Ok(-30));
        assert_eq!(d(json!("- 3f")), Ok(-3));
        assert!(d(json!("end")).unwrap_err().contains("not a distance"));
        assert!(d(json!("-end")).unwrap_err().contains("not a distance"));
        assert!(d(json!("abc")).unwrap_err().contains("not a time"));
        assert!(d(json!(null)).is_err());
    }

    fn t(v: Value, fps: f64) -> Result<u32, String> {
        frame(&v, fps, END)
    }

    fn s(text: &str, fps: f64) -> Result<u32, String> {
        t(Value::String(text.to_string()), fps)
    }

    #[test]
    fn frames_form() {
        assert_eq!(s("450f", FPS), Ok(450));
        assert_eq!(s("0f", FPS), Ok(0));
        assert_eq!(s("450F", FPS), Ok(450));
        assert_eq!(s(" 12f ", FPS), Ok(12));
        assert!(s("1.5f", FPS).unwrap_err().contains("whole frames"));
        assert!(s("-2f", FPS).unwrap_err().contains("negative"));
        assert!(s("f", FPS).is_err());
        // Whole but past the frame range: too large, not "not a frame count".
        let big = s("99999999999f", FPS).unwrap_err();
        assert!(big.contains("too large: frames go up to 4294967295"), "{big}");
        assert!(!big.contains("whole frames"), "{big}");
    }

    #[test]
    fn huge_seconds_say_how_far_past_the_timeline() {
        let far = s("1e9s", FPS).unwrap_err();
        assert_eq!(
            far,
            "1000000000 s is past the longest timeline edith supports (about 1657 days at 30 fps)"
        );
        // Never an exponent, in a string or a JSON number; past 1e15 s the digits are not run on.
        let near = s("9.9999999999e10", FPS).unwrap_err();
        assert!(near.starts_with("99999999999 s is past the longest timeline"), "{near}");
        let json_far = t(json!(1e300), FPS).unwrap_err();
        assert!(!json_far.contains("1e300") && json_far.len() < 160, "{json_far}");
        assert!(json_far.starts_with("that is far past the longest timeline"), "{json_far}");
    }

    #[test]
    fn seconds_forms() {
        assert_eq!(s("15.0s", FPS), Ok(450));
        assert_eq!(s("15s", FPS), Ok(450));
        assert_eq!(s("15", FPS), Ok(450));
        assert_eq!(s("1.5", FPS), Ok(45));
        assert_eq!(s("15.0s", 24.0), Ok(360));
        // Rounds to the nearest frame: 0.01s is 0.3 frames at 30 fps, 0.02s is 0.6.
        assert_eq!(s("0.01", FPS), Ok(0));
        assert_eq!(s("0.02", FPS), Ok(1));
    }

    #[test]
    fn json_number_is_seconds() {
        assert_eq!(t(json!(15), FPS), Ok(450));
        assert_eq!(t(json!(1.5), FPS), Ok(45));
        assert_eq!(t(json!(0), FPS), Ok(0));
        assert!(t(json!(-1.0), FPS).unwrap_err().contains("negative"));
    }

    #[test]
    fn end_form() {
        assert_eq!(s("end", FPS), Ok(END));
        assert_eq!(s("END", FPS), Ok(END));
        assert_eq!(t(json!("end"), 25.0), Ok(END));
    }

    #[test]
    fn minutes_seconds_forms() {
        assert_eq!(s("01:02", FPS), Ok(62 * 30));
        assert_eq!(s("01:02.500", 25.0), Ok(1563));
        assert_eq!(s("00:15.5", FPS), Ok(465));
        assert!(s("99:00", FPS).unwrap_err().contains("minutes must be below 60"));
        assert!(s("01:75", FPS).unwrap_err().contains("seconds must be below 60"));
        assert!(s("01:", FPS).is_err());
    }

    #[test]
    fn hours_minutes_seconds_forms() {
        assert_eq!(s("1:00:00", FPS), Ok(108_000));
        assert_eq!(s("00:00:15.5", FPS), Ok(465));
        assert!(s("00:61:00", FPS).unwrap_err().contains("minutes must be below 60"));
        assert!(s("-1:00:00", FPS).unwrap_err().contains("negative"));
    }

    #[test]
    fn timecode_form() {
        assert_eq!(s("00:00:15:00", FPS), Ok(450));
        assert_eq!(s("00:01:00:12", 25.0), Ok(1512));
        assert_eq!(s("00:00:00:24", 25.0), Ok(24));
        assert!(s("00:00:00:25", 25.0).unwrap_err().contains("FF must be below 25"));
        assert!(s("00:00:00:30", FPS).is_err());
        assert!(s("00:60:00:00", FPS).is_err());
    }

    #[test]
    fn refusals() {
        assert!(s("", FPS).unwrap_err().contains("not a time"));
        assert!(s("abc", FPS).unwrap_err().contains("accepted forms"));
        assert!(s("-1", FPS).unwrap_err().contains("negative"));
        assert!(s("-1s", FPS).unwrap_err().contains("negative"));
        assert!(s("nan", FPS).is_err());
        assert!(s("inf", FPS).is_err());
        assert!(t(json!(true), FPS).unwrap_err().contains("boolean"));
        assert!(t(json!([1]), FPS).unwrap_err().contains("list"));
        assert!(t(Value::Null, FPS).is_err());
        assert!(frame(&json!(1), 0.0, END).is_err());
    }

    #[test]
    fn frame_arg_missing_and_present() {
        let args = json!({ "at": "450f", "nothing": null });
        assert_eq!(frame_arg(&args, "at", FPS, END).ok(), Some(450));
        let missing = frame_arg(&args, "gone", FPS, END).unwrap_err();
        assert!(missing.is_error);
        assert!(missing.text.contains("missing argument `gone`"), "{}", missing.text);
        assert!(frame_arg(&args, "nothing", FPS, END).is_err());
        let bad = frame_arg(&json!({ "at": "x" }), "at", FPS, END).unwrap_err();
        assert!(bad.text.contains("argument `at`"), "{}", bad.text);
    }

    #[test]
    fn opt_frame_arg_missing_is_none() {
        let args = json!({ "at": "end", "nothing": null });
        assert_eq!(opt_frame_arg(&args, "at", FPS, END).ok(), Some(Some(END)));
        assert_eq!(opt_frame_arg(&args, "gone", FPS, END).ok(), Some(None));
        assert_eq!(opt_frame_arg(&args, "nothing", FPS, END).ok(), Some(None));
        assert!(opt_frame_arg(&json!({ "at": [] }), "at", FPS, END).is_err());
    }

    #[test]
    fn lanes_round_trip() {
        assert_eq!(lane("V1"), Ok(Lane::V1));
        assert_eq!(lane("v1"), Ok(Lane::V1));
        assert_eq!(lane("A1"), Ok(Lane::A1));
        assert_eq!(lane("S1"), Ok(Lane::S1));
        assert_eq!(lane("A2"), Ok(Lane::new(LaneKind::Audio, 1)));
        assert_eq!(lane("v12"), Ok(Lane::new(LaneKind::Video, 11)));
        for name in ["V1", "V3", "A2", "S1", "S9", "V12"] {
            assert_eq!(lane_name(lane(name).unwrap()), name);
        }
        assert_eq!(lane_name(lane("a2").unwrap()), "A2");
        assert_eq!(lane_name(Lane::new(LaneKind::Subtitle, 0)), "S1");
    }

    #[test]
    fn lane_errors() {
        assert!(lane("V0").unwrap_err().contains("numbered from 1"));
        assert!(lane("X1").unwrap_err().contains("not a lane"));
        assert!(lane("V").unwrap_err().contains("not a lane"));
        assert!(lane("").unwrap_err().contains("not a lane"));
        assert!(lane("V-1").unwrap_err().contains("not a lane"));
        assert!(lane("1V").unwrap_err().contains("not a lane"));
    }

    #[test]
    fn addr_and_formatting() {
        assert_eq!(addr(Lane::V1, 450), "V1@450");
        assert_eq!(addr(Lane::new(LaneKind::Audio, 1), 0), "A2@0");
        assert_eq!(addr(Lane::new(LaneKind::Subtitle, 0), 900), "S1@900");
        assert_eq!(tc(450, FPS), "450f (15.000s)");
        assert_eq!(secs(45, FPS), 1.5);
        assert_eq!(clock(450, FPS), "00:15.000");
        assert_eq!(clock(0, FPS), "00:00.000");
        // 1h 2m 3.5s at 30 fps.
        assert_eq!(clock(111_705, FPS), "1:02:03.500");
    }

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    #[test]
    fn no_clip_hint_names_frames_and_nearest_starts() {
        let sess = session();
        let fps = session_fps(&sess);
        let lane = Lane::V1;
        // Clips start at 0, 240, 900, 1200 and 5000; 8 s is frame 240 at 30 fps.
        let all = [(0, 100), (240, 500), (900, 1000), (1200, 1300), (5000, 5100)];
        let hit = frame_hint(&sess, lane, 8, &all);
        assert!(hit.contains(&format!("addresses are frames: 8 s is V1@240 at {fps} fps")), "{hit}");
        let miss = frame_hint(&sess, lane, 7, &all);
        assert!(miss.contains("addresses are frames: V1@7 is"), "{miss}");
        // The three starts nearest frame 1000, in time order.
        assert_eq!(
            nearest_starts(lane, &all, 1000),
            "V1 has 5 clip(s), nearest starts V1@240, V1@900, V1@1200"
        );
        assert_eq!(nearest_starts(Lane::new(LaneKind::Audio, 5), &[], 0), "A6 has no clips");
    }

    #[test]
    fn clip_addresses() {
        let sess = session();
        assert!(session_fps(&sess) > 0.0);
        assert!(session_end(&sess) > 0);
        let first = sess.lane_clips(Lane::V1).first().expect("V1 has a clip");

        assert_eq!(clip(&sess, "V1#0"), Ok((Lane::V1, 0)));
        assert_eq!(clip(&sess, &format!("V1@{}", first.start)), Ok((Lane::V1, 0)));
        assert_eq!(clip(&sess, &format!("v1@{}", first.start)), Ok((Lane::V1, 0)));
        let sec_form = format!("V1@{}s", secs(first.start, session_fps(&sess)));
        assert_eq!(clip(&sess, &sec_form), Ok((Lane::V1, 0)));

        let out = clip(&sess, "V1#999").unwrap_err();
        assert!(out.contains("out of range"), "{out}");
        assert!(out.contains("V1@"), "{out}");

        assert!(clip(&sess, "V1").unwrap_err().contains("not a clip address"));
        assert!(clip(&sess, "V1#x").unwrap_err().contains("not a clip index"));
        assert!(clip(&sess, "Q1@0").unwrap_err().contains("not a lane"));
        assert!(clip(&sess, "A9@0").unwrap_err().contains("no clips"));

        // A frame inside the first clip, past its start, names the clip that spans it.
        if first.end() > first.start + 1 {
            let inside = first.start + 1;
            let msg = clip(&sess, &format!("V1@{inside}")).unwrap_err();
            assert!(msg.contains("no clip starts at"), "{msg}");
            assert!(msg.contains(&format!("V1@{}", first.start)), "{msg}");
            assert!(msg.contains("use that address"), "{msg}");
            // A bare frame inside a clip is named as a frame too.
            assert!(msg.contains("addresses are frames"), "{msg}");
        }

        let beyond = clip(&sess, &format!("V1@{}", session_end(&sess) + 1000)).unwrap_err();
        assert!(beyond.contains("no clip at"), "{beyond}");
    }
}
