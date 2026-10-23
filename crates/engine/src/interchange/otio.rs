//! OpenTimelineIO (`.otio`), the JSON timeline format: [`write`] and [`read`].
//! Tracks become `Video` and `Audio` tracks, clips reference their file by an
//! absolute `file://` path, speed is a `LinearTimeWarp`, a transition is a
//! `Transition` (`SMPTE_Dissolve` for a dissolve), and markers sit on the stack.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{ClipItem, Item, Timeline, Track};
use crate::project::{LaneKind, Marker, MarkerColor, Speed};
use crate::transitions::TransitionKind;

const DISSOLVE: &str = "SMPTE_Dissolve";

fn rational(frames: f64, fps: f64) -> Value {
    json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": frames })
}

fn range(start: u32, duration: u32, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational(f64::from(start), fps),
        "duration": rational(f64::from(duration), fps),
    })
}

fn color_name(color: MarkerColor) -> &'static str {
    match color {
        MarkerColor::Red => "RED",
        MarkerColor::Orange => "ORANGE",
        MarkerColor::Yellow => "YELLOW",
        MarkerColor::Green => "GREEN",
        MarkerColor::Blue => "BLUE",
        MarkerColor::Purple => "PURPLE",
    }
}

/// The nearest of edith's six marker colours to an OpenTimelineIO colour name.
fn color_of(name: &str) -> MarkerColor {
    match name {
        "RED" | "PINK" => MarkerColor::Red,
        "ORANGE" => MarkerColor::Orange,
        "YELLOW" => MarkerColor::Yellow,
        "GREEN" => MarkerColor::Green,
        "PURPLE" | "MAGENTA" => MarkerColor::Purple,
        _ => MarkerColor::Blue,
    }
}

/// `file:///abs/path` with everything but unreserved characters and `/` escaped.
fn file_url(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(char::from(b)),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The OpenTimelineIO document for `t`, as pretty JSON.
pub fn write(t: &Timeline) -> String {
    let fps = t.fps;
    let mut tracks = Vec::new();
    for track in &t.tracks {
        let (kind, letter) = match track.kind {
            LaneKind::Audio => ("Audio", 'A'),
            _ => ("Video", 'V'),
        };
        let mut children = Vec::new();
        for item in &track.items {
            match item {
                Item::Gap(n) => children.push(json!({
                    "OTIO_SCHEMA": "Gap.1",
                    "name": "",
                    "source_range": range(0, *n, fps),
                    "effects": [],
                    "markers": [],
                    "metadata": {},
                })),
                Item::Clip(c) => {
                    children.push(clip(c, fps));
                    if let Some((kind, frames)) = c.transition {
                        children.push(transition(kind, frames, fps));
                    }
                }
            }
        }
        tracks.push(json!({
            "OTIO_SCHEMA": "Track.1",
            "name": format!("{letter}{}", track.ord + 1),
            "kind": kind,
            "source_range": null,
            "effects": [],
            "markers": [],
            "metadata": {},
            "children": children,
        }));
    }
    let markers: Vec<Value> = t.markers.iter().map(|m| marker(m, fps)).collect();
    let doc = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": t.name,
        "global_start_time": null,
        "metadata": {
            "edith": { "width": t.resolution.0, "height": t.resolution.1, "fps": fps, "notes": t.notes },
        },
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "source_range": null,
            "effects": [],
            "markers": markers,
            "metadata": {},
            "children": tracks,
        },
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn clip(c: &ClipItem, fps: f64) -> Value {
    let mut effects = Vec::new();
    if !c.speed.is_normal() {
        effects.push(json!({
            "OTIO_SCHEMA": "LinearTimeWarp.1",
            "name": "",
            "effect_name": "Speed",
            "metadata": {},
            "time_scalar": f64::from(c.speed.permille()) / 1000.0,
        }));
    }
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": c.file_name(),
        "source_range": range(c.in_frame, c.out_frame - c.in_frame, fps),
        "media_references": {
            "DEFAULT_MEDIA": {
                "OTIO_SCHEMA": "ExternalReference.1",
                "name": c.file_name(),
                "target_url": file_url(&c.path),
                "available_range": null,
                "metadata": {},
            },
        },
        "active_media_reference_key": "DEFAULT_MEDIA",
        "effects": effects,
        "markers": [],
        "metadata": {},
    })
}

fn transition(kind: TransitionKind, frames: u32, fps: f64) -> Value {
    let name = match kind {
        TransitionKind::Dissolve => DISSOLVE.to_string(),
        other => other.name(),
    };
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "name": "",
        "transition_type": name,
        "in_offset": rational(f64::from(frames / 2), fps),
        "out_offset": rational(f64::from(frames - frames / 2), fps),
        "metadata": {},
    })
}

fn marker(m: &Marker, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.2",
        "name": m.name,
        "color": color_name(m.color),
        "marked_range": range(m.frame, m.length, fps),
        "metadata": { "edith": { "note": m.note } },
    })
}

/// The timeline in an OpenTimelineIO document. Times are converted from the
/// document's own rate to `fps`, the project's, so a file cut at another rate
/// still lands on the same moments.
pub fn read(text: &str, fps: f64) -> crate::Result<Timeline> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not an OpenTimelineIO file: {e}"))?;
    if !doc["OTIO_SCHEMA"].as_str().is_some_and(|s| s.starts_with("Timeline")) {
        return Err("not an OpenTimelineIO timeline".into());
    }
    let mut notes = Vec::new();
    let edith = &doc["metadata"]["edith"];
    let resolution = match (edith["width"].as_u64(), edith["height"].as_u64()) {
        (Some(w), Some(h)) => (w as u32, h as u32),
        _ => (1920, 1080),
    };
    let stack = &doc["tracks"];
    let mut tracks = Vec::new();
    let mut ord = [0usize; 2];
    for child in stack["children"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let kind = match child["kind"].as_str() {
            Some("Audio") => LaneKind::Audio,
            Some("Video") => LaneKind::Video,
            _ => continue,
        };
        let slot = match kind {
            LaneKind::Audio => 1,
            _ => 0,
        };
        let items = read_items(child, fps, &mut notes)?;
        tracks.push(Track { kind, ord: ord[slot], items });
        ord[slot] += 1;
    }
    let mut markers = Vec::new();
    for m in stack["markers"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let (start, length) = span(&m["marked_range"], fps);
        markers.push(Marker {
            frame: start,
            length,
            name: m["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or("Marker").to_string(),
            note: m["metadata"]["edith"]["note"].as_str().unwrap_or("").to_string(),
            color: color_of(m["color"].as_str().unwrap_or("")),
        });
    }
    if let Some(list) = edith["notes"].as_array() {
        notes.extend(list.iter().filter_map(|n| n.as_str().map(str::to_string)));
    }
    Ok(Timeline {
        name: doc["name"].as_str().unwrap_or("Timeline").to_string(),
        fps,
        resolution,
        tracks,
        markers,
        notes,
    })
}

fn read_items(track: &Value, fps: f64, notes: &mut Vec<String>) -> crate::Result<Vec<Item>> {
    let mut items: Vec<Item> = Vec::new();
    let mut cursor = 0u32;
    for it in track["children"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let schema = it["OTIO_SCHEMA"].as_str().unwrap_or("");
        if schema.starts_with("Clip") {
            let media = match it["media_references"].as_object() {
                Some(refs) => refs.get(it["active_media_reference_key"].as_str().unwrap_or("DEFAULT_MEDIA")).cloned().unwrap_or(Value::Null),
                None => it["media_reference"].clone(),
            };
            let path = path_of(&media)
                .ok_or_else(|| format!("the clip {} names no file", it["name"].as_str().unwrap_or("")))?;
            let (in_frame, len) = span(&it["source_range"], fps);
            let speed = warp(&it["effects"]);
            let frames = speed.frames(len);
            items.push(Item::Clip(ClipItem {
                path,
                start: cursor,
                in_frame,
                out_frame: in_frame + len,
                speed,
                frames,
                transition: None,
            }));
            cursor += frames;
        } else if schema.starts_with("Gap") {
            let (_, len) = span(&it["source_range"], fps);
            items.push(Item::Gap(len));
            cursor += len;
        } else if schema.starts_with("Transition") {
            let frames = frames_of(&it["in_offset"], fps) + frames_of(&it["out_offset"], fps);
            let name = it["transition_type"].as_str().unwrap_or(DISSOLVE);
            let kind = match name {
                DISSOLVE => TransitionKind::Dissolve,
                other => TransitionKind::parse(other).unwrap_or_else(|| {
                    notes.push(format!("a {other} transition became a dissolve"));
                    TransitionKind::Dissolve
                }),
            };
            if let Some(Item::Clip(prev)) = items.last_mut() {
                prev.transition = Some((kind, frames));
            }
        }
    }
    Ok(items)
}

/// The file a media reference names: `file://` URLs are decoded, a plain path is taken as it is.
fn path_of(media: &Value) -> Option<PathBuf> {
    let url = media["target_url"].as_str()?;
    let path = match url.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => url.to_string(),
    };
    Some(PathBuf::from(path))
}

/// The speed a `LinearTimeWarp` effect asks for, normal when there is none.
fn warp(effects: &Value) -> Speed {
    effects
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["OTIO_SCHEMA"].as_str().is_some_and(|s| s.starts_with("LinearTimeWarp")))
        .and_then(|e| e["time_scalar"].as_f64())
        .map_or(Speed::NORMAL, |s| Speed::from_permille((s * 1000.0).round().clamp(0.0, 65535.0) as u16))
}

/// A RationalTime as frames at `fps`.
fn frames_of(time: &Value, fps: f64) -> u32 {
    let value = time["value"].as_f64().unwrap_or(0.0);
    let rate = time["rate"].as_f64().filter(|r| *r > 0.0).unwrap_or(fps);
    (value * fps / rate).round().max(0.0) as u32
}

/// A TimeRange as (start, duration) in frames at `fps`; zeros for a null range.
fn span(range: &Value, fps: f64) -> (u32, u32) {
    (frames_of(&range["start_time"], fps), frames_of(&range["duration"], fps))
}
