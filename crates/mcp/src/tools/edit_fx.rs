//! Look, sound and lane ops for the batched `edit` verb: grade, placement, fit,
//! equalizer, lane gain and flags, the master limiter, grouping, subtitle import,
//! and title cards.
//!
//! [`apply`] takes one op object and either applies it to the session or refuses it
//! with one sentence. It returns `None` for an op name that is not one of these, so
//! the caller can offer the op to the other groups.

use std::path::Path;

use engine::color::ColorParams;
use engine::effects::{EffectKind, MaskKey};
use engine::duck::{
    Duck, MAX_ATTACK_MS, MAX_DEPTH_DB, MAX_RELEASE_MS, MAX_THRESHOLD_DB, MIN_DEPTH_DB,
    MIN_THRESHOLD_DB, MIN_TIME_MS,
};
use engine::eq::{Band, BandKind, EqParams};
use engine::limiter::Limiter;
use engine::project::{
    check_title, gain_keys_db_at, Clip, GainKey, Lane, LaneKind, Title, TitlePreset, MAX_CLIP_GAIN_DB,
    MAX_GAIN_KEYS, MIN_CLIP_GAIN_DB,
};
use engine::scale::FitPolicy;
use engine::text::{layout_share, missing_glyphs, Align, HPlace, Outline, Place, TextStyle, Weight, SIZE_RANGE};
use engine::color::{ColorKey, MAX_COLOR_KEYS};
use engine::transform::{Corner, KeyEase, OpacityKey, TransformKey, TransformParams, MAX_TRANSFORM_KEYS};
use engine::titleanim::TitleAnim;
use engine::PlaybackSession;
use serde_json::{json, Value};

use super::edit::{check_unlocked, length, need, opt_time};
use crate::parse;

/// What one op did: a line for the reply, and the clips it touched as (lane, start
/// frame), so the caller can re-address them after later ops in the batch.
#[derive(Debug)]
pub struct Applied {
    pub text: String,
    pub touched: Vec<(Lane, u32)>,
}

/// Applies one `{op: "...", ...}` if it is one of this module's ops.
///
/// `None` when `op["op"]` is missing or names an op this module does not own.
/// `fps` is the project frame rate, which the time arguments (title times) need.
pub fn apply(s: &mut PlaybackSession, op: &Value, fps: f64) -> Option<Result<Applied, String>> {
    let name = op.get("op")?.as_str()?;
    let r = match name {
        "color" => color(s, op),
        "transform" => transform(s, op),
        "transform_keys" => transform_keys(s, op, fps),
        "opacity_keys" => opacity_keys(s, op, fps),
        "mask_keys" => mask_keys(s, op, fps),
        "color_keys" => color_keys(s, op, fps),
        "lut" => lut(s, op),
        "wheels" => wheels(s, op),
        "curves" => curves(s, op),
        "look" => look(s, op),
        "auto_color" => auto_color(s, op),
        "fit" => fit(s, op),
        "eq" => eq(s, op),
        "gain" => gain(s, op),
        "gain_keys" => gain_keys(s, op, fps),
        "gain_key" => gain_key(s, op, fps),
        "gain_key_remove" => gain_key_remove(s, op, fps),
        "gain_ramp" => gain_ramp(s, op, fps),
        "gain_dip" => gain_dip(s, op, fps),
        "lane_gain" => lane_gain(s, op),
        "pan" => pan(s, op),
        "pitch" => pitch(s, op),
        "lane_pan" => lane_pan(s, op),
        "duck" => duck(s, op),
        "dynamics" => super::voice::dynamics(s, op, fps),
        "stabilize" => super::stabilise::op(s, op),
        "reframe" => super::reframe::op(s, op),
        "angle" => super::multicam::angle(s, op),
        "nest" => super::sequence::nest(s, op),
        "break_apart" => super::sequence::break_apart(s, op),
        "place_sequence" => super::sequence::place_sequence(s, op),
        "limiter" => limiter(s, op),
        "lane_add" => lane_add(s, op),
        "lane_remove" => lane_remove(s, op),
        "lane_move" => lane_move(s, op),
        "lane_flags" => lane_flags(s, op),
        "lane_name" => lane_name(s, op),
        "deinterlace" => deinterlace(s, op),
        "group" => group(s, op),
        "ungroup" => ungroup(s, op),
        "subtitles_import" => subtitles_import(s, op),
        "title_add" => title_add(s, op, fps),
        "title_set" => title_set(s, op, fps),
        "title_remove" => title_remove(s, op, fps),
        _ => return None,
    };
    Some(r)
}

// ---- ops ------------------------------------------------------------------

/// `color {clip, brightness?, contrast?, saturation?, tint?, reset?}`.
/// Merges onto the clip's current grade; `reset` takes the grade off and wins
/// over the other fields.
fn color(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("color {raw}");
    let keyed = s.color_keys_of(lane, idx).len();
    if keyed > 0 {
        return Err(format!(
            "{who}: {raw} has {keyed} colour key(s), and a keyed clip's grade is its keys; change them with color_keys, or take them off first with color_keys {{clip, clear:true}}"
        ));
    }
    if flag(op, "reset")? {
        if !s.set_color(lane, idx, None) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → reset"), vec![(lane, start)]));
    }
    let mut c: ColorParams = s.color_of(lane, idx).copied().unwrap_or_default();
    let mut shown = Vec::new();
    if let Some(v) = num(op, "brightness")? {
        c.brightness = ranged(&who, "brightness", v, -1.0, 1.0)?;
        shown.push(format!("brightness {:.2}", c.brightness));
    }
    if let Some(v) = num(op, "contrast")? {
        c.contrast = ranged(&who, "contrast", v, 0.0, 2.0)?;
        shown.push(format!("contrast {:.2}", c.contrast));
    }
    if let Some(v) = num(op, "saturation")? {
        c.saturation = ranged(&who, "saturation", v, 0.0, 2.0)?;
        shown.push(format!("saturation {:.2}", c.saturation));
    }
    if let Some(v) = num(op, "tint")? {
        c.tint = ranged(&who, "tint", v, -1.0, 1.0)?;
        shown.push(format!("tint {:.2}", c.tint));
    }
    if shown.is_empty() {
        return Err(format!(
            "{who}: give at least one of brightness, contrast, saturation, tint, or reset:true"
        ));
    }
    if !s.set_color(lane, idx, Some(c)) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {}", shown.join(", ")), vec![(lane, start)]))
}

/// The clip's grade for a look op: its address, the grade as it stands (identity
/// for none), and the start frame. Picture clips only.
fn look_target(s: &PlaybackSession, op: &Value, name: &str) -> Result<(Lane, usize, String, u32, ColorParams), String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    if lane.kind != LaneKind::Video {
        return Err(format!("{name} {raw}: a look grades a picture; {raw} is on a {} lane", kind_word(lane.kind)));
    }
    let c = s.color_of(lane, idx).copied().unwrap_or_default();
    Ok((lane, idx, raw, start, c))
}

/// Writes a look op's grade: no grade at all when everything is back at rest.
fn write_look(s: &mut PlaybackSession, lane: Lane, idx: usize, c: ColorParams, who: &str) -> Result<(), String> {
    let params = (!c.is_identity() || c.look.lut.is_some()).then_some(c);
    if !s.set_color(lane, idx, params) {
        return Err(refused(who));
    }
    Ok(())
}

/// A `.cube` path argument, opened and checked: a missing or malformed file is
/// refused here, with the parser's reason, rather than set and silently skipped.
fn open_lut(who: &str, raw: &str) -> Result<engine::cube::LutRef, String> {
    let path = super::project::expand_home(raw.trim());
    let path = std::path::absolute(&path).unwrap_or(path);
    let r = engine::cube::LutRef::reload(&path);
    match r.problem() {
        Some(e) => Err(format!("{who}: cannot use that LUT: {e}")),
        None => Ok(r),
    }
}

fn lut_words(r: engine::cube::LutRef, mix: f32) -> String {
    let path = r.path();
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let table = r.table().map(|c| format!(", {} {}", if c.three_d { "3D" } else { "1D" }, c.size)).unwrap_or_default();
    format!("LUT {name}{table} at {:.0}%", mix * 100.0)
}

/// `lut {clip, path?, mix?, clear?}` or `lut {relink, path}`: a `.cube` file on the
/// clip's grade (after its wheels and curves, before the four sliders), how much
/// of it shows (mix 0..1, default 1), or off; `relink` points every clip naming
/// one file at another, as media_relink does for a source.
fn lut(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    if let Some(from) = op.get("relink") {
        let from = from.as_str().ok_or("lut: `relink` is the path of the LUT file clips name now")?;
        let who = "lut relink".to_string();
        let to = req_str(op, "path").map_err(|e| format!("{who}: {e} (the file the LUT now is)"))?;
        let fresh = open_lut(&who, to)?;
        let from = super::project::expand_home(from.trim());
        let named: Vec<std::path::PathBuf> = s.luts().into_iter().map(|r| r.path()).collect();
        if !named.contains(&from) {
            let list: Vec<String> = named.iter().map(|p| p.display().to_string()).collect();
            return Err(format!(
                "{who}: no clip names {}; the LUTs in use are: {}",
                from.display(),
                if list.is_empty() { "none".into() } else { list.join(", ") }
            ));
        }
        let n = s.relink_lut(&from, &fresh.path());
        return Ok(done(format!("{who} → {n} clip(s) now use {}", fresh.path().display()), Vec::new()));
    }
    let (lane, idx, raw, start, mut c) = look_target(s, op, "lut")?;
    let who = format!("lut {raw}");
    if flag(op, "clear")? {
        c.look.lut = None;
        c.look.lut_mix = 1.0;
        write_look(s, lane, idx, c, &who)?;
        return Ok(done(format!("{who} → off"), vec![(lane, start)]));
    }
    let path = match op.get("path") {
        None | Some(Value::Null) => None,
        Some(Value::String(p)) => Some(p.clone()),
        Some(v) => return Err(format!("{who}: `path` must be a string, got {}", kind_of(v))),
    };
    let mix = num(op, "mix")?.map(|v| ranged(&who, "mix", v, 0.0, 1.0)).transpose()?;
    if path.is_none() && mix.is_none() {
        return Err(format!("{who}: give path (a .cube file), mix (0..1), or clear:true"));
    }
    if let Some(p) = path {
        c.look.lut = Some(open_lut(&who, &p)?);
        c.look.lut_mix = 1.0;
    }
    let Some(r) = c.look.lut else {
        return Err(format!("{who}: {raw} has no LUT to mix; give path first"));
    };
    if let Some(m) = mix {
        c.look.lut_mix = m;
    }
    write_look(s, lane, idx, c, &who)?;
    Ok(done(format!("{who} → {}", lut_words(r, c.look.lut_mix)), vec![(lane, start)]))
}

/// One wheel argument: `[master, red, green, blue]`, or one number for the master alone.
fn wheel_arg(who: &str, op: &Value, key: &str) -> Result<Option<[f32; 4]>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let v = ranged(who, key, n.as_f64().unwrap_or(f64::NAN), -1.0, 1.0)?;
            Ok(Some([v, 0.0, 0.0, 0.0]))
        }
        Some(Value::Array(a)) if a.len() == 4 => {
            let mut out = [0f32; 4];
            for (o, v) in out.iter_mut().zip(a) {
                let x = v.as_f64().ok_or_else(|| format!("{who}: {key} holds {}, not a number", kind_of(v)))?;
                *o = ranged(who, key, x, -1.0, 1.0)?;
            }
            Ok(Some(out))
        }
        Some(_) => Err(format!("{who}: {key} is [master, red, green, blue] (each -1..1), or one number for the master")),
    }
}

fn wheel_words(name: &str, w: &[f32; 4]) -> Option<String> {
    (w.iter().any(|&v| v != 0.0)).then(|| format!("{name} m{:+.2} r{:+.2} g{:+.2} b{:+.2}", w[0], w[1], w[2], w[3]))
}

/// `wheels {clip, lift?, gamma?, gain?, reset?}`: lift (blacks), gamma (mids) and
/// gain (whites), each `[master, red, green, blue]` in -1..1 with 0 neutral, or one
/// number for the master. A wheel named replaces that wheel; the others stay.
fn wheels(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start, mut c) = look_target(s, op, "wheels")?;
    let who = format!("wheels {raw}");
    if flag(op, "reset")? {
        c.look.wheels = engine::color::Wheels::default();
        write_look(s, lane, idx, c, &who)?;
        return Ok(done(format!("{who} → neutral"), vec![(lane, start)]));
    }
    let (lift, gamma, gain) = (wheel_arg(&who, op, "lift")?, wheel_arg(&who, op, "gamma")?, wheel_arg(&who, op, "gain")?);
    if lift.is_none() && gamma.is_none() && gain.is_none() {
        return Err(format!("{who}: give at least one of lift, gamma, gain ([master, red, green, blue], each -1..1), or reset:true"));
    }
    let w = &mut c.look.wheels;
    if let Some(v) = lift {
        w.lift = v;
    }
    if let Some(v) = gamma {
        w.gamma = v;
    }
    if let Some(v) = gain {
        w.gain = v;
    }
    let shown: Vec<String> = [("lift", w.lift), ("gamma", w.gamma), ("gain", w.gain)]
        .iter()
        .filter_map(|(n, v)| wheel_words(n, v))
        .collect();
    write_look(s, lane, idx, c, &who)?;
    let text = if shown.is_empty() { "neutral".to_string() } else { shown.join(", ") };
    Ok(done(format!("{who} → {text}"), vec![(lane, start)]))
}

/// `look {clip, name}`: the clip's grade becomes the look `name` (a built-in, or a saved
/// one from the colour card's "Save current as look"), replacing its colour, wheels, curves
/// and LUT in one undo step. A name nobody has gets the list of the looks there are.
fn look(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start, _) = look_target(s, op, "look")?;
    let who = format!("look {raw}");
    let name = req_str(op, "name")?;
    let keyed = s.color_keys_of(lane, idx).len();
    if keyed > 0 {
        return Err(format!(
            "{who}: {raw} has {keyed} colour key(s), and a keyed clip's grade is its keys; take them off first with color_keys {{clip, clear:true}}"
        ));
    }
    let params = engine::looks::load(name).map_err(|e| {
        let saved = engine::looks::saved_names();
        let saved = if saved.is_empty() { "none saved".to_string() } else { saved.join(", ") };
        let built: Vec<&str> = engine::looks::builtins().iter().map(|(n, _)| *n).collect();
        format!("{who}: {e}. Looks: {}; saved: {saved}", built.join(", "))
    })?;
    write_look(s, lane, idx, params, &who)?;
    Ok(done(format!("{who} → {name}"), vec![(lane, start)]))
}

/// `auto_color {clip}`: reads a correction off the clip's middle frame as shot (grey-world
/// white balance into the three channel gains, then a luma stretch that puts the 0.5 % and
/// 99.5 % percentiles on 0.02 and 0.98 through brightness and contrast) and writes it onto the
/// grade. Saturation, tint, curves and LUT are kept. The reply says what it set.
fn auto_color(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start, c0) = look_target(s, op, "auto_color")?;
    let who = format!("auto_color {raw}");
    let keyed = s.color_keys_of(lane, idx).len();
    if keyed > 0 {
        return Err(format!(
            "{who}: {raw} has {keyed} colour key(s), and a keyed clip's grade is its keys; take them off first with color_keys {{clip, clear:true}}"
        ));
    }
    let mid = start + s.lane_clips(lane)[idx].len() / 2;
    let (project, meta) = s.export_snapshot();
    // Read at a size that keeps the analysis quick; the statistics do not need more.
    let (w, h) = fit_to(meta.width, meta.height, 640);
    let f = engine::export::compose_ungraded_at(&project, &meta, mid, w, h)
        .map_err(|e| format!("{who}: cannot read frame {mid}: {e}"))?;
    let rgb: Vec<u8> = f.bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
    let fix = engine::auto_color::analyse(&rgb, f.width as usize, f.height as usize)
        .ok_or_else(|| format!("{who}: frame {mid} has no picture to read"))?;
    let next = fix.apply(c0);
    write_look(s, lane, idx, next, &who)?;
    let g = fix.gain;
    let text = format!(
        "{who} → wheels gain red {:+.2}, green {:+.2}, blue {:+.2}, brightness {:+.2}, contrast {:.2} (luma 0.5 %/99.5 % was {:.2}/{:.2})",
        g[0], g[1], g[2], fix.brightness, fix.contrast, fix.low, fix.high
    );
    Ok(done(text, vec![(lane, start)]))
}

/// `(w, h)` scaled down to `max` wide, keeping the aspect, never below 2 px.
fn fit_to(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max || w == 0 {
        return (w.max(2), h.max(2));
    }
    let scaled = (u64::from(h) * u64::from(max) / u64::from(w)).max(2) as u32;
    (max, scaled)
}

/// `curves {clip, master?, red?, green?, blue?, reset?}`: each a list of up to 16
/// `[input, output]` points in 0..1, inputs rising; `[]` straightens that curve.
fn curves(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start, mut c) = look_target(s, op, "curves")?;
    let who = format!("curves {raw}");
    if flag(op, "reset")? {
        c.look.curves = [engine::color::Curve::default(); 4];
        write_look(s, lane, idx, c, &who)?;
        return Ok(done(format!("{who} → straight"), vec![(lane, start)]));
    }
    let mut shown = Vec::new();
    for (ch, name) in engine::color::CHANNELS.iter().enumerate() {
        let Some(v) = op.get(*name).filter(|v| !v.is_null()) else {
            continue;
        };
        let pts = v.as_array().ok_or_else(|| format!("{who}: {name} is a list of [input, output] points"))?;
        let mut out = Vec::with_capacity(pts.len());
        for p in pts {
            let xy = p.as_array().filter(|a| a.len() == 2).and_then(|a| Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32]));
            out.push(xy.ok_or_else(|| format!("{who}: a {name} point is [input, output], both 0..1"))?);
        }
        c.look.curves[ch] = engine::color::Curve::new(&out).map_err(|e| format!("{who}: {name}: {e}"))?;
        shown.push(if out.is_empty() { format!("{name} straight") } else { format!("{name} {} point(s)", out.len()) });
    }
    if shown.is_empty() {
        return Err(format!("{who}: give at least one of master, red, green, blue (a list of [input, output] points), or reset:true"));
    }
    write_look(s, lane, idx, c, &who)?;
    Ok(done(format!("{who} → {}", shown.join(", ")), vec![(lane, start)]))
}

/// `transform {clip, x?, y?, scale?, rotate?, crop?: [l,r,t,b], opacity?, reset?}`.
///
/// Units, as `engine::transform` documents them: `x`/`y` are canvas fractions
/// off centre (positive right / down), `scale` a multiplier on the fit size,
/// `rotate` degrees clockwise (rendered at the nearest 90), each crop side a
/// fraction of the source, and `opacity` 0 (clear) to 1 (opaque). `reset` wins
/// over the other fields.
fn transform(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("transform {raw}");
    let keys = s.transform_keys_of(lane, idx).len();
    if keys > 0 {
        return Err(format!(
            "{who}: {raw} has {keys} transform key(s), and a keyed clip's placement is its keys; change them with transform_keys, or take them off first with transform_keys {{clip, clear:true}}"
        ));
    }
    if flag(op, "reset")? {
        if !s.set_transform(lane, idx, None) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → reset"), vec![(lane, start)]));
    }
    let corner = corner_arg(&who, op)?;
    if corner.is_some() && (num(op, "x")?.is_some() || num(op, "y")?.is_some()) {
        return Err(format!(
            "{who}: corner sets x and y itself, so give corner alone, or x and y alone"
        ));
    }
    if corner.is_none() && op.get("margin").is_some_and(|v| !v.is_null()) {
        return Err(format!("{who}: margin is the gap a corner keeps, so it goes with corner"));
    }
    let mut t: TransformParams = s.transform_of(lane, idx).copied().unwrap_or_default();
    let mut shown = Vec::new();
    if let Some(v) = num(op, "x")? {
        t.pos_x = ranged(&who, "x", v, -1.0, 1.0)?;
        shown.push(format!("x {:.2}", t.pos_x));
    }
    if let Some(v) = num(op, "y")? {
        t.pos_y = ranged(&who, "y", v, -1.0, 1.0)?;
        shown.push(format!("y {:.2}", t.pos_y));
    }
    if let Some(v) = num(op, "scale")? {
        t.scale = ranged(&who, "scale", v, 0.05, 8.0)?;
        shown.push(format!("scale {:.2}", t.scale));
    }
    if let Some(v) = num(op, "rotate")? {
        t.rotate = ranged(&who, "rotate", v, -360.0, 360.0)?;
        shown.push(format!("rotate {:.0}°", t.rotate));
    }
    if let Some(v) = op.get("crop").filter(|v| !v.is_null()) {
        let arr = v
            .as_array()
            .filter(|a| a.len() == 4)
            .ok_or_else(|| format!("{who}: crop takes [left, right, top, bottom], four fractions from 0 to 0.5"))?;
        let mut f = [0.0f32; 4];
        for (i, item) in arr.iter().enumerate() {
            let x = item
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| format!("{who}: crop[{i}] must be a number, got {}", kind_of(item)))?;
            f[i] = ranged(&who, &format!("crop[{i}]"), x, 0.0, 0.5)?;
        }
        t.crop_l = f[0];
        t.crop_r = f[1];
        t.crop_t = f[2];
        t.crop_b = f[3];
        shown.push(format!("crop {:.2}/{:.2}/{:.2}/{:.2}", f[0], f[1], f[2], f[3]));
    }
    if let Some(v) = num(op, "opacity")? {
        let keyed = s.opacity_keys_of(lane, idx).len();
        if keyed > 0 {
            return Err(format!(
                "{who}: {raw} has {keyed} opacity key(s), which give its opacity; change them with opacity_keys, or take them off first with opacity_keys {{clip, clear:true}}"
            ));
        }
        t.opacity = ranged(&who, "opacity", v, 0.0, 1.0)?;
        shown.push(format!("opacity {:.2}", t.opacity));
    }
    if let Some(corner) = corner {
        // The corner is measured on the picture as it shows: its crop, then its
        // quarter turn, at the current scale, with the margin in pixels.
        let src = source_size(s, lane, idx).ok_or_else(|| {
            format!("{who}: {raw} has no picture size to place by (a sound, or a generated clip)")
        })?;
        let (cw, ch) = s.resolution();
        let margin = match num(op, "margin")? {
            Some(m) => ranged(&who, "margin", m, 0.0, 4000.0)?,
            None => engine::transform::CORNER_MARGIN_PX,
        };
        // The picture as it shows (its crop and quarter turn) sets the box, at the current scale.
        let (x, y) = engine::transform::corner_placement(corner, &t, src, (cw, ch), margin);
        t.pos_x = x;
        t.pos_y = y;
        shown.push(format!("corner {}, margin {margin:.0} px", corner_name(corner)));
    }
    if shown.is_empty() {
        return Err(format!(
            "{who}: give at least one of x, y, corner, scale, rotate, crop, opacity, or reset:true"
        ));
    }
    if !s.set_transform(lane, idx, Some(t)) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {}", shown.join(", ")), vec![(lane, start)]))
}

/// The `corner` of a transform op: `centre`, `top-left`, `top-right`, `bottom-left`
/// or `bottom-right`. `None` when the op does not name one.
fn corner_arg(who: &str, op: &Value) -> Result<Option<Corner>, String> {
    let Some(v) = op.get("corner").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = v.as_str().ok_or_else(|| {
        format!("{who}: corner is centre, top-left, top-right, bottom-left or bottom-right, got {}", kind_of(v))
    })?;
    match text {
        "centre" | "center" => Ok(Some(Corner::Centre)),
        "top-left" => Ok(Some(Corner::TopLeft)),
        "top-right" => Ok(Some(Corner::TopRight)),
        "bottom-left" => Ok(Some(Corner::BottomLeft)),
        "bottom-right" => Ok(Some(Corner::BottomRight)),
        other => Err(format!(
            "{who}: corner is centre, top-left, top-right, bottom-left or bottom-right, got \"{other}\""
        )),
    }
}

/// The name a corner reads as in a reply.
fn corner_name(corner: Corner) -> &'static str {
    match corner {
        Corner::Centre => "centre",
        Corner::TopLeft => "top-left",
        Corner::TopRight => "top-right",
        Corner::BottomLeft => "bottom-left",
        Corner::BottomRight => "bottom-right",
    }
}

/// The pixel size of a clip's source picture: a video's, or a still's. `None` for
/// sound, and for a generated clip, which has no file to measure.
fn source_size(s: &PlaybackSession, lane: Lane, idx: usize) -> Option<(u32, u32)> {
    let c = s.lane_clips(lane).get(idx)?;
    let path = &s.sources().get(c.source)?.path;
    match engine::probe_media(path).video {
        Some(v) => Some((v.width, v.height)),
        None if engine::is_image(path) => engine::decode::image_size(path).ok(),
        None => None,
    }
}

/// `transform_keys {clip, keys:[{at, x?, y?, scale?, rotate?, opacity?, ease?}], clear?}`:
/// replaces the clip's placement animation, the slow zoom or a move over time. Video
/// clips only. `at` is clip-relative: a time from the clip's start ("0s", "2.5s", a
/// frame count), "50%" of the clip, or "end". A key leaves each field it does not
/// name held from the previous key, and the first key from the clip's own
/// transform. `ease` is "linear" (the default) or "ease" (slow in and out) and
/// shapes the stretch from this key to the next; the last key has no stretch.
/// `clear:true` takes the animation off, back to the clip's single transform. The
/// reply lists the keys in timeline time. A keyed clip refuses `transform`.
fn transform_keys(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("transform_keys {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!(
            "{who}: {raw} is on an audio lane; placement keys belong to a picture clip"
        ));
    }
    let c = s
        .lane_clips(lane)
        .get(idx)
        .cloned()
        .ok_or_else(|| refused(&who))?;
    let list = match op.get("keys") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a),
        Some(v) => {
            return Err(format!(
                "{who}: keys must be a list of {{at, x?, y?, scale?, rotate?, opacity?, ease?}} objects, got {}",
                kind_of(v)
            ))
        }
    };
    if flag(op, "clear")? {
        if list.is_some() {
            return Err(format!("{who}: give keys or clear:true, not both"));
        }
        if s.transform_keys_of(lane, idx).is_empty() {
            return Ok(done(format!("{who} → already no keys"), Vec::new()));
        }
        if !s.set_transform_keys(lane, idx, Vec::new()) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → cleared (0 keys)"), vec![(lane, start)]));
    }
    let list = list.ok_or_else(|| {
        format!("{who}: missing `keys`: a list of {{at, x?, y?, scale?, rotate?, opacity?, ease?}} objects, or clear:true")
    })?;
    if list.is_empty() {
        return Err(format!("{who}: keys is empty; use clear:true to take the animation off"));
    }
    if list.len() > MAX_TRANSFORM_KEYS {
        return Err(format!("{who}: {} keys, and at most {MAX_TRANSFORM_KEYS} fit", list.len()));
    }
    let end = parse::session_end(s);
    let frames = c.frames();
    let mut timed: Vec<(u32, &Value)> = Vec::with_capacity(list.len());
    for (i, k) in list.iter().enumerate() {
        let n = i + 1;
        let obj = k.as_object().ok_or_else(|| {
            format!("{who}: key {n} must be an object such as {{\"at\": \"0s\", \"scale\": 1.0}}")
        })?;
        if let Some(bad) = obj
            .keys()
            .find(|k| !matches!(k.as_str(), "at" | "x" | "y" | "scale" | "rotate" | "opacity" | "ease"))
        {
            return Err(format!(
                "{who}: key {n} has `{bad}`, which a key does not take: x, y, scale, rotate, opacity, ease, at"
            ));
        }
        let at = obj
            .get("at")
            .ok_or_else(|| format!("{who}: key {n} has no `at`, the time from the clip's start"))?;
        let off = clip_offset(&who, n, at, frames, fps, end)?;
        timed.push((off, k));
    }
    timed.sort_by_key(|&(off, _)| off);
    let base = s.transform_of(lane, idx).copied().unwrap_or_default();
    let mut prev = base;
    let mut keys: Vec<TransformKey> = Vec::with_capacity(timed.len());
    for (i, (off, k)) in timed.iter().enumerate() {
        let n = i + 1;
        let mut p = prev;
        if let Some(v) = num_of(k, "x")? {
            p.pos_x = ranged(&who, "x", v, -1.0, 1.0)?;
        }
        if let Some(v) = num_of(k, "y")? {
            p.pos_y = ranged(&who, "y", v, -1.0, 1.0)?;
        }
        if let Some(v) = num_of(k, "scale")? {
            p.scale = ranged(&who, "scale", v, 0.05, 8.0)?;
        }
        if let Some(v) = num_of(k, "rotate")? {
            p.rotate = ranged(&who, "rotate", v, -360.0, 360.0)?;
        }
        if let Some(v) = num_of(k, "opacity")? {
            p.opacity = ranged(&who, "opacity", v, 0.0, 1.0)?;
        }
        let ease = ease_of(&who, n, k)?;
        keys.push(TransformKey {
            at: src_at(&c, start + off),
            params: p,
            ease,
        });
        prev = p;
    }
    if let Some(i) = keys.windows(2).position(|w| w[0].at == w[1].at) {
        // At 1x a source frame is a timeline frame, so the clash is two keys at
        // one time; at another speed two timeline frames can share a source one.
        return Err(if c.speed == engine::project::Speed::NORMAL && c.speed_keys.is_empty() {
            let t = tsec(f64::from(start + timed[i + 1].0), fps);
            format!("{who}: two keys at the same time ({t}); space them further apart")
        } else {
            format!(
                "{who}: two keys land on the same source frame at this speed; space them further apart"
            )
        });
    }
    if keys == s.transform_keys_of(lane, idx) {
        return Ok(done(format!("{who} → already {}", transform_keys_text(&c, fps, &keys)), Vec::new()));
    }
    let text = format!("{who} → {}", transform_keys_text(&c, fps, &keys));
    if !s.set_transform_keys(lane, idx, keys) {
        return Err(refused(&who));
    }
    Ok(done(text, vec![(lane, start)]))
}

/// The `ease` of key `n` (a JSON object): one of the names [`KeyEase::from_name`]
/// takes, `linear` when absent.
fn ease_of(who: &str, n: usize, key: &Value) -> Result<KeyEase, String> {
    match key.get("ease") {
        None | Some(Value::Null) => Ok(KeyEase::Linear),
        Some(Value::String(name)) => KeyEase::from_name(name).ok_or_else(|| {
            format!("{who}: key {n} ease is {EASES}, got {name:?}")
        }),
        Some(v) => Err(format!("{who}: key {n} ease must be a name ({EASES}), got {}", kind_of(v))),
    }
}

/// The ease names, for the refusals that list them.
const EASES: &str = "\"linear\", \"ease_in\", \"ease_out\", \"ease_in_out\" or \"hold\"";

/// ` ease_in` for a reply, nothing for linear.
fn ease_suffix(ease: KeyEase) -> String {
    match ease {
        KeyEase::Linear => String::new(),
        e => format!(" {}", e.name()),
    }
}

/// The keys of an `opacity_keys` / `color_keys` call as `(clip-relative frame, key object)`
/// sorted by time: `keys` checked to be a non-empty list of objects holding only
/// `allowed` fields, each with an `at`.
fn timed_keys<'a>(
    who: &str,
    list: &'a [Value],
    allowed: &[&str],
    c: &Clip,
    fps: f64,
    end: u32,
    cap: usize,
) -> Result<Vec<(u32, &'a Value)>, String> {
    if list.is_empty() {
        return Err(format!("{who}: keys is empty; use clear:true to take the animation off"));
    }
    if list.len() > cap {
        return Err(format!("{who}: {} keys, and at most {cap} fit", list.len()));
    }
    let frames = c.frames();
    let mut timed = Vec::with_capacity(list.len());
    for (i, k) in list.iter().enumerate() {
        let n = i + 1;
        let obj = k.as_object().ok_or_else(|| {
            format!("{who}: key {n} must be an object such as {{\"at\": \"0s\", \"{}\": 0.5}}", allowed[0])
        })?;
        if let Some(bad) = obj.keys().find(|k| k.as_str() != "at" && k.as_str() != "ease" && !allowed.contains(&k.as_str())) {
            return Err(format!(
                "{who}: key {n} has `{bad}`, which a key does not take: {}, ease, at",
                allowed.join(", ")
            ));
        }
        let at = obj
            .get("at")
            .ok_or_else(|| format!("{who}: key {n} has no `at`, the time from the clip's start"))?;
        timed.push((clip_offset(who, n, at, frames, fps, end)?, k));
    }
    timed.sort_by_key(|&(off, _)| off);
    Ok(timed)
}

/// Refuses two keys on one source frame, in the words the transform keys use.
fn clash_check(who: &str, c: &Clip, at: &[u32], timed: &[(u32, &Value)], start: u32, fps: f64) -> Result<(), String> {
    if let Some(i) = at.windows(2).position(|w| w[0] == w[1]) {
        return Err(if c.speed == engine::project::Speed::NORMAL {
            let t = tsec(f64::from(start + timed[i + 1].0), fps);
            format!("{who}: two keys at the same time ({t}); space them further apart")
        } else {
            format!("{who}: two keys land on the same source frame at this speed; space them further apart")
        });
    }
    Ok(())
}

/// `opacity_keys {clip, keys:[{at, opacity, ease?}], clear?}`: replaces the clip's
/// opacity animation (0 clear .. 1 opaque), the fade-in of an overlay or a pulse.
/// `at` is clip-relative as in `transform_keys`. A key that names no `opacity`
/// holds the previous key's (the first, the clip's own opacity). The keys replace
/// the clip's constant opacity while they exist; the fades multiply on top.
/// `clear:true` takes them off.
fn opacity_keys(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("opacity_keys {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: {raw} is on an audio lane; opacity keys belong to a picture clip"));
    }
    let c = s.lane_clips(lane).get(idx).cloned().ok_or_else(|| refused(&who))?;
    let list = match op.get("keys") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a),
        Some(v) => {
            return Err(format!("{who}: keys must be a list of {{at, opacity, ease?}} objects, got {}", kind_of(v)))
        }
    };
    if flag(op, "clear")? {
        if list.is_some() {
            return Err(format!("{who}: give keys or clear:true, not both"));
        }
        if s.opacity_keys_of(lane, idx).is_empty() {
            return Ok(done(format!("{who} → already no keys"), Vec::new()));
        }
        if !s.set_opacity_keys(lane, idx, Vec::new()) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → cleared (0 keys)"), vec![(lane, start)]));
    }
    let list = list.ok_or_else(|| format!("{who}: missing `keys`: a list of {{at, opacity, ease?}} objects, or clear:true"))?;
    let end = parse::session_end(s);
    let timed = timed_keys(&who, list, &["opacity"], &c, fps, end, MAX_TRANSFORM_KEYS)?;
    let mut prev = s.transform_of(lane, idx).map_or(1.0, |t| t.opacity);
    let mut keys = Vec::with_capacity(timed.len());
    for (i, (off, k)) in timed.iter().enumerate() {
        let n = i + 1;
        if let Some(v) = num_of(k, "opacity")? {
            prev = ranged(&who, "opacity", v, 0.0, 1.0)?;
        }
        keys.push(OpacityKey { at: src_at(&c, start + off), opacity: prev, ease: ease_of(&who, n, k)? });
    }
    let at: Vec<u32> = keys.iter().map(|k| k.at).collect();
    clash_check(&who, &c, &at, &timed, start, fps)?;
    let text = format!("{who} → {}", opacity_keys_text(&c, fps, &keys));
    if keys == s.opacity_keys_of(lane, idx) {
        return Ok(done(format!("{who} → already {}", opacity_keys_text(&c, fps, &keys)), Vec::new()));
    }
    if !s.set_opacity_keys(lane, idx, keys) {
        return Err(refused(&who));
    }
    Ok(done(text, vec![(lane, start)]))
}

/// The opacity keys as timeline times: at most twelve, then `(+N more)`.
pub(super) fn opacity_keys_text(c: &Clip, fps: f64, keys: &[OpacityKey]) -> String {
    const SHOWN: usize = 12;
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN)
        .map(|k| format!("{} opacity {:.2}{}", tsec(timeline_of(c, k.at), fps), k.opacity, ease_suffix(k.ease)))
        .collect();
    if keys.len() > SHOWN {
        parts.push(format!("(+{} more)", keys.len() - SHOWN));
    }
    format!("{} key(s): {}", keys.len(), parts.join(" · "))
}

/// `mask_keys {clip, effect, keys:[{at, x?, y?, w?, h?, feather?, ease?}], clear?}`: a rect or
/// ellipse mask's position, size and soft edge over the clip's own time. `effect` is the mask's
/// stack position. A key leaves each field it does not name at the previous key's value (the
/// first key, at the mask's own static value). `clear:true` takes the keys off, and the mask
/// goes back to its static values. The same sorted, clamped list a tracker writes.
fn mask_keys(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("mask_keys {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: {raw} is on an audio lane; mask keys belong to a picture clip"));
    }
    let c = s.lane_clips(lane).get(idx).cloned().ok_or_else(|| refused(&who))?;
    let slot = match op.get("effect") {
        None | Some(Value::Null) => return Err(format!("{who}: missing `effect`: the mask's stack position, 0 for the first effect")),
        Some(v) => v.as_u64().ok_or_else(|| format!("{who}: effect must be a stack position, 0 for the first"))? as usize,
    };
    let e = c.effects.get(slot).ok_or_else(|| {
        format!("{who}: effect {slot} is out of range: the clip has {} effects", c.effects.len())
    })?;
    if !matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask) {
        return Err(format!("{who}: effect {slot} is {}, and only a rect or ellipse mask takes keys", e.kind.name()));
    }
    if flag(op, "clear")? {
        if s.mask_keys_of(lane, idx, slot).is_empty() {
            return Ok(done(format!("{who} → already no keys"), Vec::new()));
        }
        if !s.set_mask_keys(lane, idx, slot, Vec::new()) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → cleared (0 keys)"), vec![(lane, start)]));
    }
    let list = match op.get("keys") {
        Some(Value::Array(a)) => a,
        None | Some(Value::Null) => {
            return Err(format!("{who}: missing `keys`: a list of {{at, x, y, w, h, feather, ease?}} objects, or clear:true"))
        }
        Some(v) => return Err(format!("{who}: keys must be a list of {{at, x, y, w, h, feather, ease?}} objects, got {}", kind_of(v))),
    };
    let names = ["x", "y", "w", "h", "feather"];
    let r = EffectKind::RectMask.params();
    // The mask's own values, in the order `names` lists them: x, y, width, height, feather.
    let mut prev = [e.values[0], e.values[1], e.values[2], e.values[3], e.values[4]];
    let end = parse::session_end(s);
    let timed = timed_keys(&who, list, &names, &c, fps, end, MAX_TRANSFORM_KEYS)?;
    let mut keys = Vec::with_capacity(timed.len());
    for (i, (off, k)) in timed.iter().enumerate() {
        let n = i + 1;
        for (j, name) in names.iter().enumerate() {
            if let Some(v) = num_of(k, name)? {
                prev[j] = ranged(&who, name, v, f64::from(r[j].min), f64::from(r[j].max))?;
            }
        }
        keys.push(MaskKey {
            at: src_at(&c, start + off),
            x: prev[0],
            y: prev[1],
            width: prev[2],
            height: prev[3],
            feather: prev[4],
            ease: ease_of(&who, n, k)?,
        });
    }
    let at: Vec<u32> = keys.iter().map(|k| k.at).collect();
    clash_check(&who, &c, &at, &timed, start, fps)?;
    let text = format!("{who} → {}", mask_keys_text(&c, fps, &keys));
    if keys == s.mask_keys_of(lane, idx, slot) {
        return Ok(done(format!("{who} → already {}", mask_keys_text(&c, fps, &keys)), Vec::new()));
    }
    if !s.set_mask_keys(lane, idx, slot, keys) {
        return Err(refused(&who));
    }
    Ok(done(text, vec![(lane, start)]))
}

/// The mask keys as timeline times: at most twelve, then `(+N more)`.
fn mask_keys_text(c: &Clip, fps: f64, keys: &[MaskKey]) -> String {
    const SHOWN: usize = 12;
    if keys.is_empty() {
        return "cleared (0 keys)".to_string();
    }
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN)
        .map(|k| {
            format!(
                "{} x {:.2} y {:.2} w {:.2} h {:.2} feather {:.3}{}",
                tsec(timeline_of(c, k.at), fps),
                k.x,
                k.y,
                k.width,
                k.height,
                k.feather,
                ease_suffix(k.ease)
            )
        })
        .collect();
    if keys.len() > SHOWN {
        parts.push(format!("(+{} more)", keys.len() - SHOWN));
    }
    format!("{} key(s): {}", keys.len(), parts.join(" · "))
}

/// `color_keys {clip, keys:[{at, brightness?, contrast?, saturation?, tint?, ease?}], clear?}`:
/// replaces the clip's colour animation. `at` is clip-relative as in `transform_keys`.
/// A key leaves each field it does not name at the previous key's value (the first
/// key, at the clip's own grade). The keys replace the clip's constant grade while
/// they exist (`color` refuses until they are cleared); `clear:true` takes them off.
fn color_keys(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("color_keys {raw}");
    if lane.kind != LaneKind::Video {
        return Err(format!("{who}: {raw} is on an audio lane; colour keys belong to a picture clip"));
    }
    let c = s.lane_clips(lane).get(idx).cloned().ok_or_else(|| refused(&who))?;
    let list = match op.get("keys") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a),
        Some(v) => {
            return Err(format!(
                "{who}: keys must be a list of {{at, brightness?, contrast?, saturation?, tint?, ease?}} objects, got {}",
                kind_of(v)
            ))
        }
    };
    if flag(op, "clear")? {
        if list.is_some() {
            return Err(format!("{who}: give keys or clear:true, not both"));
        }
        if s.color_keys_of(lane, idx).is_empty() {
            return Ok(done(format!("{who} → already no keys"), Vec::new()));
        }
        if !s.set_color_keys(lane, idx, Vec::new()) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → cleared (0 keys)"), vec![(lane, start)]));
    }
    let list = list.ok_or_else(|| {
        format!("{who}: missing `keys`: a list of {{at, brightness?, contrast?, saturation?, tint?, ease?}} objects, or clear:true")
    })?;
    let end = parse::session_end(s);
    let timed = timed_keys(&who, list, &["brightness", "contrast", "saturation", "tint"], &c, fps, end, MAX_COLOR_KEYS)?;
    let mut prev: ColorParams = s.color_of(lane, idx).copied().unwrap_or_default();
    let mut keys = Vec::with_capacity(timed.len());
    for (i, (off, k)) in timed.iter().enumerate() {
        let n = i + 1;
        if let Some(v) = num_of(k, "brightness")? {
            prev.brightness = ranged(&who, "brightness", v, -1.0, 1.0)?;
        }
        if let Some(v) = num_of(k, "contrast")? {
            prev.contrast = ranged(&who, "contrast", v, 0.0, 2.0)?;
        }
        if let Some(v) = num_of(k, "saturation")? {
            prev.saturation = ranged(&who, "saturation", v, 0.0, 2.0)?;
        }
        if let Some(v) = num_of(k, "tint")? {
            prev.tint = ranged(&who, "tint", v, -1.0, 1.0)?;
        }
        keys.push(ColorKey { at: src_at(&c, start + off), params: prev, ease: ease_of(&who, n, k)? });
    }
    let at: Vec<u32> = keys.iter().map(|k| k.at).collect();
    clash_check(&who, &c, &at, &timed, start, fps)?;
    let text = format!("{who} → {}", color_keys_text(&c, fps, &keys));
    if keys == s.color_keys_of(lane, idx) {
        return Ok(done(format!("{who} → already {}", color_keys_text(&c, fps, &keys)), Vec::new()));
    }
    if !s.set_color_keys(lane, idx, keys) {
        return Err(refused(&who));
    }
    Ok(done(text, vec![(lane, start)]))
}

/// The colour keys as timeline times: at most twelve, then `(+N more)`.
pub(super) fn color_keys_text(c: &Clip, fps: f64, keys: &[ColorKey]) -> String {
    const SHOWN: usize = 12;
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN)
        .map(|k| {
            let p = &k.params;
            format!(
                "{} brightness {:.2} contrast {:.2} saturation {:.2} tint {:.2}{}",
                tsec(timeline_of(c, k.at), fps),
                p.brightness,
                p.contrast,
                p.saturation,
                p.tint,
                ease_suffix(k.ease)
            )
        })
        .collect();
    if keys.len() > SHOWN {
        parts.push(format!("(+{} more)", keys.len() - SHOWN));
    }
    format!("{} key(s): {}", keys.len(), parts.join(" · "))
}

/// `n` as an English ordinal, `1st`, `2nd`, `11th`, `242nd`: the position of
/// a key in the list it was given in.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A key's `at` as a clip-relative frame offset: "end" is the clip's last frame
/// boundary, "N%" a share of its frames, anything else a time read as
/// [`parse::frame`] reads it, which must fall inside the clip.
fn clip_offset(who: &str, n: usize, at: &Value, frames: u32, fps: f64, end: u32) -> Result<u32, String> {
    if let Some(text) = at.as_str() {
        let text = text.trim();
        if text.eq_ignore_ascii_case("end") {
            return Ok(frames);
        }
        if let Some(pct) = text.strip_suffix('%') {
            let share: f64 = pct
                .trim()
                .parse()
                .map_err(|_| format!("{who}: key {n} at {text:?} is not a percentage"))?;
            if !(0.0..=100.0).contains(&share) {
                return Err(format!("{who}: key {n} at {text} is outside the clip, 0% to 100%"));
            }
            return Ok((f64::from(frames) * share / 100.0).round() as u32);
        }
    }
    let off = parse::frame(at, fps, end).map_err(|e| format!("{who}: key {n}: {e}"))?;
    if off > frames {
        return Err(format!(
            "{who}: the {} key, at {}, is past the clip, which is {} long",
            ordinal(n),
            tsec(f64::from(off), fps),
            tsec(f64::from(frames), fps)
        ));
    }
    Ok(off)
}

/// The keys as timeline times, `10.000s scale 1.20 …`: at most twelve, then
/// `(+N more)`. Shared by the op's reply and, later, any clip listing.
pub(super) fn transform_keys_text(c: &Clip, fps: f64, keys: &[TransformKey]) -> String {
    const SHOWN: usize = 12;
    if keys.is_empty() {
        return "cleared (0 keys)".to_string();
    }
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN)
        .map(|k| {
            let p = &k.params;
            let ease = ease_suffix(k.ease);
            format!(
                "{} scale {:.2} x {:.2} y {:.2} rotate {:.0}° opacity {:.2}{ease}",
                tsec(timeline_of(c, k.at), fps),
                p.scale,
                p.pos_x,
                p.pos_y,
                p.rotate,
                p.opacity
            )
        })
        .collect();
    if keys.len() > SHOWN {
        parts.push(format!("(+{} more)", keys.len() - SHOWN));
    }
    format!("{} key(s): {}", keys.len(), parts.join(" · "))
}

/// `fit {clip, policy: "fit"|"fill"|"stretch"|"center"}`.
fn fit(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let name = req_str(op, "policy")?;
    let policy = match name {
        "fit" => FitPolicy::Fit,
        "fill" => FitPolicy::Fill,
        "stretch" => FitPolicy::Stretch,
        "center" => FitPolicy::Center,
        other => {
            return Err(format!(
                "fit {raw}: `{other}` is not a fit policy; use fit, fill, stretch or center"
            ))
        }
    };
    if !s.set_fit(lane, idx, policy) {
        return Err(refused(&format!("fit {raw}")));
    }
    Ok(done(format!("fit {raw} → {name}"), vec![(lane, start)]))
}

/// `eq {clip, bands?: [{f, db, q?, kind?}], reset?}`. At most 10 bands; the GUI
/// caps are f 20–20000 Hz, db ±12, q 0.3–12. `q` defaults to 0.707 and `kind`
/// to `peak`. `bands` replaces the clip's equalizer; `reset` takes it off.
fn eq(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("eq {raw}");
    if flag(op, "reset")? {
        if !s.set_eq(lane, idx, None) {
            return Err(refused(&who));
        }
        return Ok(done(format!("{who} → reset"), vec![(lane, start)]));
    }
    let list = op
        .get("bands")
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("{who}: give bands (a list of {{f, db, q, kind}}) or reset:true"))?;
    let arr = list
        .as_array()
        .ok_or_else(|| format!("{who}: bands must be a list of {{f, db, q, kind}}, got {}", kind_of(list)))?;
    if arr.len() > 10 {
        return Err(format!("{who}: at most 10 bands, got {}", arr.len()));
    }
    let mut bands = Vec::with_capacity(arr.len());
    for (i, b) in arr.iter().enumerate() {
        let bw = format!("{who} band {i}");
        if !b.is_object() {
            return Err(format!("{bw}: a band is an object such as {{\"f\": 1000, \"db\": 3}}, got {}", kind_of(b)));
        }
        let f = req_num_in(b, "f", &bw, 20.0, 20_000.0)?;
        let db = req_num_in(b, "db", &bw, -12.0, 12.0)?;
        let q = match num_of(b, "q")? {
            Some(q) => ranged(&bw, "q", q, 0.3, 12.0)?,
            None => 0.707,
        };
        let kind = match num_kind(b)? {
            Some(k) => k,
            None => BandKind::Peak,
        };
        bands.push(Band { freq_hz: f, gain_db: db, q, kind });
    }
    let n = bands.len();
    if !s.set_eq(lane, idx, Some(EqParams { bands })) {
        return Err(refused(&who));
    }
    let text = if n == 0 {
        format!("{who} → flat (0 bands)")
    } else {
        format!("{who} → {n} band(s)")
    };
    Ok(done(text, vec![(lane, start)]))
}

/// `gain {clip, db}` or `gain {clip, add_db}`: the clip's own volume in dB, on
/// top of its lane's. `db` is absolute (-60..+24), `add_db` relative to where the
/// clip is now; the result is clamped to the same range. Audio clips only: a
/// video clip has no volume of its own here, so it is refused by name.
fn gain(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("gain {raw}");
    if lane.kind != LaneKind::Audio {
        return Err(format!(
            "{who}: {raw} is on a video lane, and a clip's gain is set on its audio clip; set it on the audio clip split from the same take"
        ));
    }
    let now = s.clip_gain_of(lane, idx).ok_or_else(|| refused(&who))?;
    let (lo, hi) = (MIN_CLIP_GAIN_DB, MAX_CLIP_GAIN_DB);
    let target = match (num(op, "db")?, num(op, "add_db")?) {
        (Some(_), Some(_)) => return Err(format!("{who}: give db (absolute) or add_db (relative), not both")),
        (Some(v), None) => ranged(&who, "db", v, f64::from(lo), f64::from(hi))?,
        (None, Some(v)) => {
            let step = ranged(&who, "add_db", v, -84.0, 84.0)?;
            (now + step).clamp(lo, hi)
        }
        (None, None) => {
            return Err(format!("{who}: missing argument `db` (absolute, -60 to +24 dB) or `add_db` (relative)"));
        }
    };
    if target == now {
        return Ok(done(format!("{who} → already {target:.1} dB"), Vec::new()));
    }
    if !s.set_clip_gain(lane, idx, target) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {target:.1} dB"), vec![(lane, start)]))
}

/// `gain_keys {clip, keys:[[time, db], ...]}`: replaces the clip's whole volume
/// envelope. Each time is a timeline time inside the clip; `keys: []` clears it.
fn gain_keys(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, c) = env_clip(s, op, "gain_keys")?;
    let who = format!("gain_keys {raw}");
    let list = match op.get("keys") {
        None | Some(Value::Null) => {
            return Err(format!("{who}: missing argument `keys`: a list of [time, dB] points, or [] to clear"))
        }
        Some(Value::Array(a)) => a,
        Some(v) => return Err(format!("{who}: keys must be a list of [time, dB] points, got {}", kind_of(v))),
    };
    let end = parse::session_end(s);
    let (lo, hi) = db_range();
    let mut pts = Vec::with_capacity(list.len());
    for (i, p) in list.iter().enumerate() {
        let n = i + 1;
        let pair = match p.as_array() {
            Some(a) if a.len() == 2 || a.len() == 3 => a,
            _ => return Err(format!("{who}: point {n} must be [time, dB] or [time, dB, ease], such as [\"10s\", -18]")),
        };
        let t = parse::frame(&pair[0], fps, end).map_err(|e| format!("{who}: point {n}: {e}"))?;
        in_clip(&who, &raw, &c, t, fps)?;
        let db = pair[1]
            .as_f64()
            .ok_or_else(|| format!("{who}: point {n}: the dB must be a number"))?;
        let ease = match pair.get(2) {
            None => KeyEase::Linear,
            Some(e) => ease_of(&who, n, &json!({ "ease": e }))?,
        };
        pts.push((t, ranged(&who, "dB", db, lo, hi)?, ease));
    }
    let keys = eased_keys_of(&who, &c, &pts)?;
    if keys == s.gain_keys_of(lane, idx) {
        return Ok(done(format!("{who} → already {}", keys_summary(&c, fps, &keys)), Vec::new()));
    }
    let text = format!("{who} → {}", keys_summary(&c, fps, &keys));
    if !s.set_gain_keys(lane, idx, keys) {
        return Err(refused(&who));
    }
    Ok(done(text, vec![(lane, c.start)]))
}

/// `gain_key {clip, at, db}`: one point of the volume envelope at timeline time
/// `at`. A point already at that frame is replaced.
fn gain_key(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, c) = env_clip(s, op, "gain_key")?;
    let who = format!("gain_key {raw}");
    let t = need(op, "at", fps, parse::session_end(s))?;
    in_clip(&who, &raw, &c, t, fps)?;
    let (lo, hi) = db_range();
    let db = req_num_in(op, "db", &who, lo, hi)?;
    let ease = ease_of(&who, 1, op)?;
    let key = GainKey { ease, ..GainKey::new(src_at(&c, t), db) };
    if s.gain_keys_of(lane, idx).contains(&key) {
        return Ok(done(format!("{who} → already {} dB at {}", fmt_db(db), tsec(f64::from(t), fps)), Vec::new()));
    }
    if !s.add_gain_key(lane, idx, key) {
        return Err(format!(
            "{who}: the engine refused it; an envelope holds at most {MAX_GAIN_KEYS} points, so remove one with gain_key_remove first"
        ));
    }
    let n = s.gain_keys_of(lane, idx).len();
    Ok(done(
        format!("{who} → {} {} dB ({n} points)", tsec(f64::from(t), fps), fmt_db(db)),
        vec![(lane, c.start)],
    ))
}

/// `gain_key_remove {clip, at}`: takes out the point within one frame of timeline
/// time `at`. When none is that close the refusal lists the points there are.
fn gain_key_remove(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, c) = env_clip(s, op, "gain_key_remove")?;
    let who = format!("gain_key_remove {raw}");
    let t = need(op, "at", fps, parse::session_end(s))?;
    in_clip(&who, &raw, &c, t, fps)?;
    let current = s.gain_keys_of(lane, idx).to_vec();
    let near = current
        .iter()
        .map(|k| (k.at, k.db, (timeline_of(&c, k.at) - f64::from(t)).abs()))
        .filter(|&(_, _, d)| d <= 1.0)
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((at, db, _)) = near else {
        let have = if current.is_empty() {
            "this clip has no gain keys".to_string()
        } else {
            format!("its points are {}", points_text(&c, fps, &current))
        };
        return Err(format!("{who}: no gain key within a frame of {}; {have}", tsec(f64::from(t), fps)));
    };
    if !s.remove_gain_key(lane, idx, at) {
        return Err(refused(&who));
    }
    let left = s.gain_keys_of(lane, idx).len();
    Ok(done(
        format!(
            "{who} → removed {} {} dB ({left} points left)",
            tsec(timeline_of(&c, at), fps),
            fmt_db(db)
        ),
        vec![(lane, c.start)],
    ))
}

/// `gain_ramp {clip, from, to, from_db?, to_db}`: a straight dB ramp between two
/// timeline times. `from_db` defaults to the envelope's level at `from`. Points
/// inside `from..=to` are replaced; the ones outside stay.
fn gain_ramp(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, c) = env_clip(s, op, "gain_ramp")?;
    let who = format!("gain_ramp {raw}");
    let end = parse::session_end(s);
    let from = need(op, "from", fps, end)?;
    let to = need(op, "to", fps, end)?;
    span_check(&who, &raw, &c, from, to, fps)?;
    let (lo, hi) = db_range();
    let to_db = req_num_in(op, "to_db", &who, lo, hi)?;
    let from_db = match num(op, "from_db")? {
        Some(v) => ranged(&who, "from_db", v, lo, hi)?,
        None => env_db(s, (lane, idx), &c, from),
    };
    let (n, changed) = replace_span(s, &who, (lane, idx), &c, (from, to), &[(from, from_db), (to, to_db)])?;
    if !changed {
        return Ok(done(format!("{who} → already set"), Vec::new()));
    }
    Ok(done(
        format!(
            "{who} {} → {} dB to {} dB, {n} points",
            span_text(from, to, fps),
            fmt_db(from_db),
            fmt_db(to_db)
        ),
        vec![(lane, c.start)],
    ))
}

/// `gain_dip {clip, from, to, db, ramp?="0.5s"}`: dips the envelope to `db` over
/// `from..to`, with a ramp of `ramp` in before `from` and back out after `to`. The
/// ramps start from the level there is now, and are clamped to the clip; a clamped
/// ramp is named in the reply. Points inside the dip (ramps included) are replaced.
/// For a dip that follows speech by itself, `duck` is the automatic alternative.
fn gain_dip(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let (lane, idx, raw, c) = env_clip(s, op, "gain_dip")?;
    let who = format!("gain_dip {raw}");
    let end = parse::session_end(s);
    let from = need(op, "from", fps, end)?;
    let to = need(op, "to", fps, end)?;
    span_check(&who, &raw, &c, from, to, fps).map_err(|e| format!("{e}{}", covering_hint(s, lane, from, to, fps)))?;
    let (lo, hi) = db_range();
    let db = req_num_in(op, "db", &who, lo, hi)?;
    let ramp = length(op, "ramp", fps)?.unwrap_or_else(|| (0.5 * fps).round() as u32);
    if ramp == 0 {
        return Err(format!("{who}: ramp must be more than 0 (default 0.5s)"));
    }
    let a = from.saturating_sub(ramp).max(c.start);
    let b = to.saturating_add(ramp).min(c.end());
    // A ramp cut short by the clip bounds is kept, but the reply says so.
    let mut notes = Vec::new();
    if from - a < ramp {
        notes.push(if from == a {
            format!(
                "no ramp in: the dip starts at the clip's start, so there is no fade down into it; start it later than {} for a ramp",
                tsec(f64::from(c.start), fps)
            )
        } else {
            format!(
                "ramp in shortened to {}s: the clip starts at {}",
                num_text(f64::from(from - a) / fps),
                tsec(f64::from(c.start), fps)
            )
        });
    }
    if b - to < ramp {
        notes.push(if b == to {
            "no ramp out: the dip ends at the clip's end".to_string()
        } else {
            format!(
                "ramp out shortened to {}s: the clip ends at {}",
                num_text(f64::from(b - to) / fps),
                tsec(f64::from(c.end()), fps)
            )
        });
    }
    let note = if notes.is_empty() { String::new() } else { format!(" (note: {})", notes.join("; ")) };
    // The ramp ends sit on the level now, read before anything changes.
    let mut pts = Vec::with_capacity(4);
    if a < from {
        pts.push((a, env_db(s, (lane, idx), &c, a)));
    }
    pts.push((from, db));
    pts.push((to, db));
    if b > to {
        pts.push((b, env_db(s, (lane, idx), &c, b)));
    }
    let n = pts.len();
    let (_, changed) = replace_span(s, &who, (lane, idx), &c, (a, b), &pts)?;
    if !changed {
        return Ok(done(format!("{who} → already set{note}"), Vec::new()));
    }
    Ok(done(
        format!(
            "{who} {} → {} dB (ramps {}s), {n} points{note}",
            span_text(from, to, fps),
            fmt_db(db),
            num_text(f64::from(ramp) / fps)
        ),
        vec![(lane, c.start)],
    ))
}

/// What a span on a lane is covered by, for a refusal that named a span past one clip: the clips
/// of `lane` that the span reaches, so a dip is aimed at the clip (or clips) that play there.
/// Empty when no clip of the lane covers any of it.
fn covering_hint(s: &PlaybackSession, lane: Lane, from: u32, to: u32, fps: f64) -> String {
    let covers: Vec<String> = s
        .lane_clips(lane)
        .iter()
        .filter(|x| x.start < to && x.end() > from)
        .map(|x| parse::addr(lane, x.start))
        .collect();
    let span = format!("{}s–{}s", num_text(f64::from(from) / fps), num_text(f64::from(to) / fps));
    match covers.as_slice() {
        [] => String::new(),
        [one] => format!("; the span {span} is on {one}: dip that clip, or use lane gain keys"),
        [first, .., last] => format!(
            "; the span {span} covers {first}…{last}: dip each, or use lane gain keys"
        ),
    }
}

/// The audio clip an envelope op names: its lane, index, the address as written, and
/// a copy of the clip. Video is refused by name, because the envelope is audio only.
fn env_clip(s: &PlaybackSession, op: &Value, what: &str) -> Result<(Lane, usize, String, Clip), String> {
    let (lane, idx, raw, _) = clip_arg(s, op)?;
    if lane.kind != LaneKind::Audio {
        return Err(format!(
            "{what} {raw}: {raw} is on a video lane, and a clip's volume envelope is set on its audio clip; set it on the audio clip split from the same take"
        ));
    }
    let c = s
        .lane_clips(lane)
        .get(idx)
        .cloned()
        .ok_or_else(|| refused(&format!("{what} {raw}")))?;
    Ok((lane, idx, raw, c))
}

/// Puts `new` (timeline frame, dB) points into the envelope over the timeline span
/// `span` (both ends inclusive). Old points inside the span go, the ones outside
/// stay, and `new` lands on its source frames. Returns the point count and whether
/// the envelope changed at all.
fn replace_span(
    s: &mut PlaybackSession,
    who: &str,
    env: (Lane, usize),
    c: &Clip,
    span: (u32, u32),
    new: &[(u32, f32)],
) -> Result<(usize, bool), String> {
    let (lane, idx) = env;
    let (lo, hi) = (f64::from(span.0), f64::from(span.1));
    let current = s.gain_keys_of(lane, idx).to_vec();
    let fresh = keys_of(who, c, new)?;
    let mut keys: Vec<GainKey> = current
        .iter()
        .copied()
        .filter(|k| {
            let t = timeline_of(c, k.at);
            (t < lo || t > hi) && !fresh.iter().any(|f| f.at == k.at)
        })
        .collect();
    keys.extend(fresh);
    keys.sort_by_key(|k| k.at);
    if keys.len() > MAX_GAIN_KEYS {
        return Err(format!(
            "{who}: the envelope would hold {} points, and at most {MAX_GAIN_KEYS} fit; remove some first",
            keys.len()
        ));
    }
    if keys == current {
        return Ok((keys.len(), false));
    }
    let n = keys.len();
    if !s.set_gain_keys(lane, idx, keys) {
        return Err(refused(who));
    }
    Ok((n, true))
}

/// Timeline-frame points as envelope points on their source frames, sorted. Refused
/// when there are too many, or when two land on one source frame.
fn keys_of(who: &str, c: &Clip, pts: &[(u32, f32)]) -> Result<Vec<GainKey>, String> {
    let eased: Vec<(u32, f32, KeyEase)> = pts.iter().map(|&(t, db)| (t, db, KeyEase::Linear)).collect();
    eased_keys_of(who, c, &eased)
}

/// [`keys_of`] with an ease on each point.
fn eased_keys_of(who: &str, c: &Clip, pts: &[(u32, f32, KeyEase)]) -> Result<Vec<GainKey>, String> {
    let mut keys: Vec<GainKey> = pts
        .iter()
        .map(|&(t, db, ease)| GainKey { ease, ..GainKey::new(src_at(c, t), db) })
        .collect();
    keys.sort_by_key(|k| k.at);
    if keys.len() > MAX_GAIN_KEYS {
        return Err(format!("{who}: {} points, and at most {MAX_GAIN_KEYS} fit", keys.len()));
    }
    if keys.windows(2).any(|w| w[0].at == w[1].at) {
        return Err(format!(
            "{who}: two points land on the same source frame at this speed; space them further apart"
        ));
    }
    Ok(keys)
}

/// Refuses a timeline frame outside the clip. The clip runs from its start up to and
/// including its end boundary, the displayed end, so `to: "3s"` on a clip that plays
/// 0–3 s is accepted; the end boundary is the clip's out point in the envelope.
fn in_clip(who: &str, raw: &str, c: &Clip, t: u32, fps: f64) -> Result<(), String> {
    if t < c.start || t > c.end() {
        return Err(format!(
            "{who}: {} is outside {raw}, which plays {}–{}",
            tsec(f64::from(t), fps),
            tsec(f64::from(c.start), fps),
            tsec(f64::from(c.end()), fps)
        ));
    }
    Ok(())
}

/// A ramp's span checked: `from` before `to`, and both inside the clip.
fn span_check(who: &str, raw: &str, c: &Clip, from: u32, to: u32, fps: f64) -> Result<(), String> {
    if to <= from {
        return Err(format!(
            "{who}: to ({}) must be after from ({})",
            tsec(f64::from(to), fps),
            tsec(f64::from(from), fps)
        ));
    }
    in_clip(who, raw, c, from, fps)?;
    in_clip(who, raw, c, to, fps)
}

/// The source frame a timeline frame `t` plays in `c`. The envelope is keyed to
/// source frames, so this is how an op's timeline time is stored.
fn src_of(c: &Clip, t: f64) -> f64 {
    f64::from(c.in_frame) + c.source_offset_f(t - f64::from(c.start))
}

/// [`src_of`] rounded to a whole source frame, never below 0.
pub(super) fn src_at(c: &Clip, t: u32) -> u32 {
    src_of(c, f64::from(t)).round().max(0.0) as u32
}

/// The timeline frame (fractional) at which source frame `at` plays in `c`; the
/// inverse of [`src_of`].
pub(super) fn timeline_of(c: &Clip, at: u32) -> f64 {
    f64::from(c.start) + c.timeline_offset_f(f64::from(at) - f64::from(c.in_frame))
}

/// The envelope's own dB at timeline frame `t`, clip gain left out: what a ramp
/// starts from, so the ramp keeps the level it found.
fn env_db(s: &PlaybackSession, env: (Lane, usize), c: &Clip, t: u32) -> f32 {
    gain_keys_db_at(s.gain_keys_of(env.0, env.1), src_of(c, f64::from(t)))
}

/// The envelope's points as timeline times, `10.000s -18.0 dB · …`: at most twelve,
/// then `(+N more)`. Shared by `clip_info` and the envelope ops' replies.
pub(super) fn points_text(c: &Clip, fps: f64, keys: &[GainKey]) -> String {
    const SHOWN: usize = 12;
    let mut parts: Vec<String> = keys
        .iter()
        .take(SHOWN)
        .map(|k| format!("{} {} dB{}", tsec(timeline_of(c, k.at), fps), fmt_db(k.db), ease_suffix(k.ease)))
        .collect();
    if keys.len() > SHOWN {
        parts.push(format!("(+{} more)", keys.len() - SHOWN));
    }
    parts.join(" · ")
}

fn keys_summary(c: &Clip, fps: f64, keys: &[GainKey]) -> String {
    if keys.is_empty() {
        "cleared (0 points)".to_string()
    } else {
        format!("{} point(s): {}", keys.len(), points_text(c, fps, keys))
    }
}

fn db_range() -> (f64, f64) {
    (f64::from(MIN_CLIP_GAIN_DB), f64::from(MAX_CLIP_GAIN_DB))
}

/// A time in seconds at `fps`, three decimals: `10.000s`.
fn tsec(t: f64, fps: f64) -> String {
    format!("{:.3}s", t / fps)
}

/// `10.000–25.000s`, the span two timeline frames make.
fn span_text(from: u32, to: u32, fps: f64) -> String {
    format!("{:.3}–{:.3}s", f64::from(from) / fps, f64::from(to) / fps)
}

/// A dB value with one decimal; `+ 0.0` turns a negative zero into zero.
fn fmt_db(db: f32) -> String {
    format!("{:.1}", db + 0.0)
}

/// A number with trailing zeros dropped: `0.5`, `2`.
fn num_text(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `lane_gain {lane, db}`, -60..+12 dB. Unchanged values are a no-op, not an error.
fn lane_gain(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("lane_gain {raw}");
    check_unlocked(s, &[lane])?;
    let v = num(op, "db")?.ok_or_else(|| format!("{who}: missing argument `db` (-60 to +12 dB)"))?;
    let db = ranged(&who, "db", v, -60.0, 12.0)?;
    if s.lane_gain_db(lane) == db {
        return Ok(done(format!("{who} → already {db:.1} dB"), Vec::new()));
    }
    if !s.set_lane_gain_db(lane, db) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {db:.1} dB"), Vec::new()))
}

/// `pan {clip, pan}`, -1 (left) to 1 (right), 0 the middle. An audio clip only.
fn pan(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("pan {raw}");
    if lane.kind != LaneKind::Audio {
        return Err(format!("{who}: {raw} is on a video lane, and a clip's pan is set on its audio clip; set it on the audio clip split from the same take"));
    }
    let v = num(op, "pan")?.ok_or_else(|| format!("{who}: missing argument `pan` (-1 left to 1 right, 0 the middle)"))?;
    // Whole percent, which is what is stored.
    let pan = (ranged(&who, "pan", v, -1.0, 1.0)? * 100.0).round() as f32 / 100.0;
    if s.clip_pan_of(lane, idx) == Some(pan) {
        return Ok(done(format!("{who} → already {pan:+.2}"), Vec::new()));
    }
    if !s.set_clip_pan(lane, idx, pan) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {pan:+.2}"), vec![(lane, start)]))
}

/// `pitch {clip, semitones}`, -12 (an octave down) to 12 (up), whole semitones. The speed is kept.
fn pitch(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let who = format!("pitch {raw}");
    if lane.kind != LaneKind::Audio {
        return Err(format!("{who}: {raw} is on a video lane, and a clip's pitch is set on its audio clip; set it on the audio clip split from the same take"));
    }
    let v = num(op, "semitones")?.ok_or_else(|| format!("{who}: missing argument `semitones` (-12 an octave down to 12 an octave up, 0 the original)"))?;
    if v.fract() != 0.0 {
        return Err(format!("{who}: semitones {} is not a whole number", fmt(v)));
    }
    let semitones = ranged(&who, "semitones", v, -12.0, 12.0)? as i32;
    if s.clip_pitch_of(lane, idx) == Some(semitones as i8) {
        return Ok(done(format!("{who} → already {semitones:+}"), Vec::new()));
    }
    if !s.set_clip_pitch(lane, idx, semitones) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {semitones:+} semitones"), vec![(lane, start)]))
}

/// `lane_pan {lane, pan}`, -1 (left) to 1 (right). Stacks on each clip's own pan.
fn lane_pan(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("lane_pan {raw}");
    check_unlocked(s, &[lane])?;
    let v = num(op, "pan")?.ok_or_else(|| format!("{who}: missing argument `pan` (-1 left to 1 right, 0 the middle)"))?;
    // Whole percent, which is what is stored.
    let pan = (ranged(&who, "pan", v, -1.0, 1.0)? * 100.0).round() as f32 / 100.0;
    if s.lane_pan(lane) == pan {
        return Ok(done(format!("{who} → already {pan:+.2}"), Vec::new()));
    }
    if !s.set_lane_pan(lane, pan) {
        return Err(refused(&who));
    }
    Ok(done(format!("{who} → {pan:+.2}"), Vec::new()))
}

/// `duck {lane, under?, depth_db?, threshold_db?, attack_ms?, release_ms?, off?}`.
/// The audio lane `lane` drops by `depth_db` while the audio lane `under` (the
/// key) is above `threshold_db`. Defaults -12 dB, -40 dBFS, 80 ms, 400 ms.
/// A lane can duck under several keys: a new `under` adds a duck, the same
/// `under` replaces its duck. `off: true` with `under` removes only that key's
/// duck; `off: true` alone clears every duck on the lane. The reply lists the
/// ducks left on the lane. Ranges are the engine's (`engine::duck`). Like a
/// lane lock, a locked lane refuses the duck.
fn duck(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("duck {raw}");
    if lane.kind != LaneKind::Audio {
        return Err(format!(
            "{who}: only audio lanes duck, and {raw} is a {} lane",
            kind_word(lane.kind)
        ));
    }
    if s.lane_locked(lane) {
        let n = parse::lane_name(lane);
        return Err(format!(
            "lane {n} is locked: unlock it first with lane_flags {{lane:\"{n}\", locked:false}}"
        ));
    }
    let under = match op.get("under") {
        None | Some(Value::Null) => None,
        Some(Value::String(u)) => Some(u.as_str()),
        Some(v) => return Err(format!("{who}: under must be a lane such as \"A1\", got {}", kind_of(v))),
    };
    if flag(op, "off")? {
        return duck_off(s, lane, &who, raw, under);
    }
    let under = under.ok_or_else(|| {
        format!("{who}: give under, the key audio lane such as \"A1\" (or off:true to remove the duck)")
    })?;
    let key = parse::lane(under)?;
    if key.kind != LaneKind::Audio {
        return Err(format!("{who}: the key {under} must be an audio lane (A1..)"));
    }
    if key == lane {
        return Err(format!("{who}: a lane is not its own key; give another audio lane as under"));
    }
    if !s.lanes().contains(&key) {
        return Err(format!("{who}: the key {under} is not a lane on this timeline"));
    }
    let depth_db = or_default(&who, op, "depth_db", -12.0, MIN_DEPTH_DB, MAX_DEPTH_DB)?;
    let threshold_db = or_default(
        &who,
        op,
        "threshold_db",
        Duck::DEFAULT_THRESHOLD_DB,
        MIN_THRESHOLD_DB,
        MAX_THRESHOLD_DB,
    )?;
    let attack_ms = or_default(&who, op, "attack_ms", Duck::DEFAULT_ATTACK_MS, MIN_TIME_MS, MAX_ATTACK_MS)?;
    let release_ms = or_default(
        &who,
        op,
        "release_ms",
        Duck::DEFAULT_RELEASE_MS,
        MIN_TIME_MS,
        MAX_RELEASE_MS,
    )?;
    // A duck under a key the lane already has replaces that one; a new key adds a duck.
    let set = Duck { under: key.ord, depth_db, threshold_db, attack_ms, release_ms };
    if s.lane_duck_list(lane).contains(&set) {
        return Ok(done(format!("{who} → already set"), Vec::new()));
    }
    if !s.set_lane_duck(lane, Some(set)) {
        return Err(refused(&who));
    }
    let key_name = parse::lane_name(key);
    Ok(done(
        format!(
            "duck {} under {key_name}: {depth_db} dB when {key_name} > {threshold_db} dBFS (attack {attack_ms} ms, release {release_ms} ms); {}",
            parse::lane_name(lane),
            ducks_now(s, lane)
        ),
        Vec::new(),
    ))
}

/// `duck {lane, off:true}` removes the duck under `under` and keeps the lane's
/// other ducks; with no `under` it clears every duck on the lane. The reply
/// lists the ducks left on the lane.
fn duck_off(
    s: &mut PlaybackSession,
    lane: Lane,
    who: &str,
    raw: &str,
    under: Option<&str>,
) -> Result<Applied, String> {
    let Some(under) = under else {
        let had = s.lane_duck_list(lane);
        if had.is_empty() {
            return Ok(done(format!("{who} → already off"), Vec::new()));
        }
        if !s.set_lane_duck(lane, None) {
            return Err(refused(who));
        }
        let names: Vec<String> = had.iter().map(|d| parse::lane_name(Lane::new(LaneKind::Audio, d.under))).collect();
        return Ok(done(
            format!("{who} → all {} ducks removed ({})", had.len(), names.join(", ")),
            Vec::new(),
        ));
    };
    let key = parse::lane(under)?;
    if key.kind != LaneKind::Audio {
        return Err(format!("{who}: the key {under} must be an audio lane (A1..)"));
    }
    let key_name = parse::lane_name(key);
    if !s.lane_duck_list(lane).iter().any(|d| d.under == key.ord) {
        return Ok(done(format!("{who} under {key_name} → already off"), Vec::new()));
    }
    if !s.clear_lane_duck(lane, key.ord) {
        return Err(refused(who));
    }
    Ok(done(
        format!("duck {raw} under {key_name} → removed; {}", ducks_now(s, lane)),
        Vec::new(),
    ))
}

/// The ducks on `lane` now, for a reply: each key with its depth and threshold,
/// or that the lane has none.
fn ducks_now(s: &PlaybackSession, lane: Lane) -> String {
    let ducks = s.lane_duck_list(lane);
    if ducks.is_empty() {
        return format!("{} has no ducks", parse::lane_name(lane));
    }
    let each: Vec<String> = ducks
        .iter()
        .map(|d| {
            format!(
                "{} ({} dB when over {} dBFS)",
                parse::lane_name(Lane::new(LaneKind::Audio, d.under)),
                d.depth_db,
                d.threshold_db
            )
        })
        .collect();
    format!("{} now ducked under {}", parse::lane_name(lane), each.join(", "))
}

/// `limiter {on?, ceiling_db?}`, ceiling -24..0 dB. Merges onto the current setting.
fn limiter(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let on = flag_opt(op, "on")?;
    let ceiling = num(op, "ceiling_db")?;
    if on.is_none() && ceiling.is_none() {
        return Err("limiter: give on (true or false) and/or ceiling_db (-24 to 0)".to_string());
    }
    let cur = s.limiter();
    let mut next = cur;
    if let Some(v) = ceiling {
        next.ceiling_db = ranged(
            "limiter",
            "ceiling_db",
            v,
            f64::from(Limiter::MIN_DB),
            f64::from(Limiter::MAX_DB),
        )?;
    }
    if let Some(b) = on {
        next.on = b;
    }
    let text = |l: Limiter| {
        format!(
            "limiter → {}, ceiling {:.1} dB",
            if l.on { "on" } else { "off" },
            l.ceiling_db
        )
    };
    if next == cur {
        return Ok(done(format!("{} (unchanged)", text(cur)), Vec::new()));
    }
    if !s.set_limiter(next) {
        return Err("limiter: the engine refused the change".to_string());
    }
    Ok(done(text(next), Vec::new()))
}

/// `lane_add {kind: "video"|"audio"|"subtitle"}`. Reports the new lane's name.
fn lane_add(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let kind = match req_str(op, "kind")? {
        "video" => LaneKind::Video,
        "audio" => LaneKind::Audio,
        "subtitle" => LaneKind::Subtitle,
        other => {
            return Err(format!(
                "lane_add: `{other}` is not a lane kind; use video, audio or subtitle"
            ))
        }
    };
    let lane = s.add_lane(kind);
    Ok(done(format!("lane_add → {} added", parse::lane_name(lane)), Vec::new()))
}

/// `lane_remove {lane}`. The engine refuses a lane that holds clips, and the
/// last video or audio lane.
fn lane_remove(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    check_unlocked(s, &[lane])?;
    // A lane that holds media clips is refused here, naming each clip by the address the edit ops
    // take (A2@0) and its file by name, so the caller can delete exactly those.
    if lane.kind != LaneKind::Subtitle && !s.lane_clips(lane).is_empty() {
        const MAX: usize = 8;
        let clips = s.lane_clips(lane);
        let shown: Vec<String> = clips
            .iter()
            .take(MAX)
            .map(|c| {
                let file = s
                    .sources()
                    .get(c.source)
                    .and_then(|src| src.path.file_name())
                    .map_or_else(|| format!("src:{}", c.source), |f| f.to_string_lossy().into_owned());
                format!("{} ({file})", parse::addr(lane, c.start))
            })
            .collect();
        let more = if clips.len() > MAX { format!(" and {} more", clips.len() - MAX) } else { String::new() };
        return Err(format!(
            "lane_remove {raw}: {raw} still holds {} clip(s): {}{more}; delete those clips (or drag them to another track) first",
            clips.len(),
            shown.join(", ")
        ));
    }
    s.remove_lane(lane)
        .map_err(|e| format!("lane_remove {raw}: {e}"))?;
    Ok(done(format!("lane_remove → {raw} removed"), Vec::new()))
}

/// `lane_move {lane, to}`: `to` is the 1-based display position among all lanes.
/// Lanes are named by position, so moving a lane renumbers every lane between the old place and
/// the new one: a move of V2 to position 3 renames V2 to V3 and the old V3 to V2. The reply gives
/// the moved lane's new name and position, then lists each renamed lane. A locked lane moves
/// (its lock travels with it). The moved lane's clips are touched under their new name.
fn lane_move(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("lane_move {raw}");
    let v = num(op, "to")?.ok_or_else(|| format!("{who}: missing argument `to` (1-based position)"))?;
    let all = s.lanes();
    let n = all.len();
    if v.fract() != 0.0 || v < 1.0 || v > n as f64 {
        return Err(format!(
            "{who}: to {v} is out of range; the timeline has {n} lane(s), positions 1–{n}"
        ));
    }
    let to = v as usize;
    let current = all.iter().position(|l| *l == lane).map_or(0, |i| i + 1);
    if to == current {
        return Ok(done(format!("{who} → already at position {to}"), Vec::new()));
    }
    let moved = s
        .move_lane(lane, to - 1)
        .ok_or_else(|| format!("{who}: nothing moved"))?;
    let touched = starts_on(s, moved);
    let renamed = renamed_by_position(s, &all, &s.lanes(), current - 1, to - 1);
    Ok(done(
        format!("{who} → {} (position {to}){renamed}", parse::lane_name(moved)),
        touched,
    ))
}

/// The lanes a move renamed. A lane's name is its kind and its place among that kind's lanes, so
/// the lanes between the old place and the new one shift by one and take each other's names. Each
/// lane whose name changed is listed with what it holds (its lock, its first clip's file), so the
/// name a caller used before still reads as the lane it was.
fn renamed_by_position(s: &PlaybackSession, before: &[Lane], after: &[Lane], from: usize, to: usize) -> String {
    let new_pos = |p: usize| -> usize {
        if p == from {
            to
        } else if from < to && p > from && p <= to {
            p - 1
        } else if to < from && p >= to && p < from {
            p + 1
        } else {
            p
        }
    };
    let mut parts = Vec::new();
    for (i, old) in before.iter().enumerate() {
        let Some(&new) = after.get(new_pos(i)) else { continue };
        if parse::lane_name(*old) == parse::lane_name(new) {
            continue;
        }
        let mut what = Vec::new();
        if s.lane_locked(new) {
            what.push("locked".to_string());
        }
        let first = if new.kind == LaneKind::Subtitle {
            None
        } else {
            s.lane_clips(new).first().and_then(|c| s.sources().get(c.source))
        };
        if let Some(name) = first.and_then(|src| src.path.file_name()) {
            what.push(name.to_string_lossy().into_owned());
        }
        let note = if what.is_empty() { String::new() } else { format!(" ({})", what.join(", ")) };
        parts.push(format!("{} → {}{note}", parse::lane_name(*old), parse::lane_name(new)));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "; lanes renamed by position: {} — lanes are named by position, so the lanes between the two places renumber; use the new names from now on",
        parts.join(", ")
    )
}

/// `lane_flags {lane, hidden?, muted?, locked?}`. `hidden` is for video and
/// subtitle lanes, `muted` for audio, `locked` for any. Unchanged flags are fine.
fn lane_flags(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("lane_flags {raw}");
    let hidden = flag_opt(op, "hidden")?;
    let muted = flag_opt(op, "muted")?;
    let locked = flag_opt(op, "locked")?;
    if hidden.is_none() && muted.is_none() && locked.is_none() {
        return Err(format!("{who}: give at least one of hidden, muted, locked (true or false)"));
    }
    // Only the lock itself may change on a locked lane, and unlocking is the way out of it.
    // Each flag counts only on the kinds it applies to, so a wrong-kind flag keeps its own refusal.
    let changes_look = hidden
        .is_some_and(|v| v != s.lane_hidden(lane) && lane.kind != LaneKind::Audio)
        || muted.is_some_and(|v| v != s.lane_muted(lane) && lane.kind == LaneKind::Audio);
    if changes_look && locked != Some(false) {
        check_unlocked(s, &[lane])?;
    }
    let mut shown = Vec::new();
    if let Some(v) = hidden {
        if s.lane_hidden(lane) != v && !s.set_lane_hidden(lane, v) {
            return Err(format!("{who}: hidden does not apply to {} lanes", kind_word(lane.kind)));
        }
        shown.push(format!("hidden {v}"));
    }
    if let Some(v) = muted {
        if s.lane_muted(lane) != v && !s.set_lane_muted(lane, v) {
            return Err(format!("{who}: muted does not apply to {} lanes", kind_word(lane.kind)));
        }
        shown.push(format!("muted {v}"));
    }
    if let Some(v) = locked {
        if s.lane_locked(lane) != v && !s.set_lane_locked(lane, v) {
            return Err(refused(&who));
        }
        shown.push(format!("locked {v}"));
    }
    Ok(done(format!("{who} → {}", shown.join(", ")), Vec::new()))
}

/// `lane_name {lane, name}`: the lane's display label; an empty name clears it. A locked lane
/// refuses, like any change to a lane.
fn lane_name(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, raw) = lane_arg(s, op)?;
    let who = format!("lane_name {raw}");
    let name = op
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{who}: give name (text; \"\" clears it)"))?;
    check_unlocked(s, &[lane])?;
    match s.set_lane_name(lane, name) {
        Err(e) => Err(format!("{who}: {e}")),
        Ok(false) => Ok(done(format!("{who} → unchanged"), Vec::new())),
        Ok(true) => Ok(done(
            match s.lane_name(lane) {
                Some(n) => format!("{who} → \"{n}\""),
                None => format!("{who} → name cleared"),
            },
            Vec::new(),
        )),
    }
}

/// `deinterlace {clip, mode: frame|field|off|ivtc-auto|ivtc}`.
fn deinterlace(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    let name = req_str(op, "mode")?;
    let mode = engine::project::Deinterlace::from_name(name)
        .ok_or_else(|| format!("deinterlace {raw}: `{name}` is not a mode; use frame, field or off (or ivtc-auto or ivtc to remove pulldown)"))?;
    if s.deinterlace_of(lane, idx) == mode {
        return Ok(done(format!("deinterlace {raw} → {name} (unchanged)"), vec![(lane, start)]));
    }
    if !s.set_deinterlace(lane, idx, mode) {
        return Err(refused(&format!("deinterlace {raw}")));
    }
    Ok(done(format!("deinterlace {raw} → {name}"), vec![(lane, start)]))
}

/// `group {clips: [addr, ...]}`: two or more clips, each an address such as `V1@450`.
fn group(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let arr = op
        .get("clips")
        .and_then(Value::as_array)
        .ok_or_else(|| "group: clips must be a list of clip addresses such as [\"V1@0\", \"A1@0\"]".to_string())?;
    if arr.len() < 2 {
        return Err(format!("group: give at least two clips, got {}", arr.len()));
    }
    let mut picks = Vec::with_capacity(arr.len());
    let mut raws: Vec<&str> = Vec::with_capacity(arr.len());
    for v in arr {
        let raw = v
            .as_str()
            .ok_or_else(|| format!("group: each clip is an address string, got {}", kind_of(v)))?;
        picks.push(super::edit::clip_addr(s, raw)?);
        raws.push(raw);
    }
    // Grouping relinks each pick's take, so a locked lane in any of those takes refuses.
    let mut lanes: Vec<Lane> = Vec::new();
    for &(l, i) in &picks {
        for t in take_lanes_of(s, l, i) {
            if !lanes.contains(&t) {
                lanes.push(t);
            }
        }
    }
    check_unlocked(s, &lanes)?;
    s.group_all(&picks).map_err(|e| format!("group: {e}"))?;
    let touched = picks
        .iter()
        .map(|&(l, i)| (l, start_of(s, l, i)))
        .collect();
    // Members of one group are often different lengths (a picture and its sound, say): the group
    // holds them together, but their ends do not meet, so the reply says how long each one is.
    let fps = parse::session_fps(s);
    let lens: Vec<(&str, u32)> = raws
        .iter()
        .zip(&picks)
        .filter_map(|(raw, &(l, i))| s.lane_clips(l).get(i).map(|c| (*raw, c.frames())))
        .collect();
    let mut text = format!("group → {} clips", picks.len());
    if lens.windows(2).any(|w| w[0].1 != w[1].1) {
        let each: Vec<String> = lens.iter().map(|(raw, f)| format!("{raw} {:.3}s", parse::secs(*f, fps))).collect();
        text.push_str(&format!("; the members differ in length ({}), so their ends do not meet", each.join(", ")));
    }
    Ok(done(text, touched))
}

/// `ungroup {clip}`.
fn ungroup(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let (lane, idx, raw, start) = clip_arg(s, op)?;
    // Taking the clip out of its take changes the whole take, so a locked lane in it refuses.
    check_unlocked(s, &take_lanes_of(s, lane, idx))?;
    if !s.ungroup(lane, idx) {
        return Err(format!("ungroup {raw}: nothing to ungroup (the clip is alone or not grouped)"));
    }
    Ok(done(format!("ungroup {raw} → taken out of its group"), vec![(lane, start)]))
}

/// `subtitles_import {path, lang?}`. The engine puts the tracks in the timeline's
/// subtitle list; it does not place them on a lane (the GUI does not either, so
/// there is no lane to report). Captions are placed afterwards with `captions_place`.
/// `lang` names the tracks' language; left out, a standalone file's own name
/// says it (`talk.fr.srt`, `fra.srt`), and when neither does the tracks stay unset.
fn subtitles_import(s: &mut PlaybackSession, op: &Value) -> Result<Applied, String> {
    let raw = req_str(op, "path")?;
    let lang = match op.get("lang") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or("subtitles_import: `lang` must be a language code such as fra")?),
    };
    let (n, named) = s
        .import_subtitles_as(Path::new(raw), lang)
        .map_err(|e| format!("subtitles_import {raw}: {e}"))?;
    let lang_note = match &named {
        Some((code, how)) => format!(" · lang {code}, {how}"),
        None => " · lang unset (und on export)".to_string(),
    };
    let text = if n == 0 {
        format!("subtitles_import {raw}: no new track, already on the timeline")
    } else {
        // The new tracks are the last `n` in the list; their indices are what captions_place takes.
        let tracks = s.subtitles();
        let first = tracks.len().saturating_sub(n);
        let cues: usize = tracks[first..].iter().map(|t| t.cues.len()).sum();
        let noun = if n == 1 { "track" } else { "tracks" };
        let at = if n == 1 {
            format!("#{first}, {cues} cues")
        } else {
            format!("#{first}..#{}, {cues} cues", tracks.len() - 1)
        };
        let place = if n == 1 {
            format!("captions_place {{track:{first}}}")
        } else {
            "captions_place {track:N}, once per track".to_string()
        };
        format!("imported {n} {noun} ({at}){lang_note} — not on the timeline yet; place it with {place}")
    };
    // Cues the parser had to drop are named, so a short import is not a silent one.
    let skipped = engine::subtitle::skipped_cues(Path::new(raw));
    let text = if skipped.is_empty() {
        text
    } else {
        // Each skipped entry is `line N: reason`: the cues are counted as cues, named by their lines.
        let parts: Vec<(Option<&str>, &str)> = skipped
            .iter()
            .map(|k| match k.split_once(": ") {
                Some((head, why)) if head.starts_with("line ") => (Some(&head["line ".len()..]), why),
                _ => (None, k.as_str()),
            })
            .collect();
        let nums: Vec<&str> = parts.iter().filter_map(|(n, _)| *n).collect();
        let noun = if skipped.len() == 1 { "cue" } else { "cues" };
        let shown = match nums.as_slice() {
            [] => String::new(),
            [one] => format!(" (line {one})"),
            [a, b] => format!(" (lines {a}, {b})"),
            [a, b, ..] => format!(" (lines {a}, {b}, …)"),
        };
        let reasons: Vec<&str> = parts.iter().map(|(_, why)| *why).collect();
        format!("{text}\nskipped {} {noun}{shown}, not imported: {}", skipped.len(), reasons.join("; "))
    };
    Ok(done(text, Vec::new()))
}

// ---- helpers --------------------------------------------------------------

// ---- titles ---------------------------------------------------------------

/// The name a title goes by: `T1` for the first in start order.
pub(super) fn title_name(idx: usize) -> String {
    format!("T{}", idx + 1)
}

/// The place a title sits at, as the ops spell it.
pub(super) fn place_word(p: Place) -> &'static str {
    match p {
        Place::Bottom => "bottom",
        Place::Top => "top",
        Place::Centre => "centre",
    }
}

/// The outline a title draws, as the ops spell it.
pub(super) fn outline_word(o: Outline) -> &'static str {
    match o {
        Outline::None => "none",
        Outline::Thin => "thin",
        Outline::Thick => "thick",
    }
}

/// The side a block sits at or lines align to, as the ops spell it.
fn side_word(h: HPlace) -> &'static str {
    match h {
        HPlace::Centre => "centre",
        HPlace::Left => "left",
        HPlace::Right => "right",
    }
}

/// One title, as a reply line: `title T1 "Episode 17" 0.000–3.000 centre size 80 outline thin fade 12f/12f`.
/// A newline in the text shows as `\n`. A look off the default adds ` colour #ffcc00 bold left/right
/// box #0a141e/78%` after the outline: only what differs from white, regular, centred, black 60%.
pub(super) fn title_line(name: &str, t: &Title, fps: f64) -> String {
    let d = TextStyle::default();
    let st = &t.style;
    let mut look = String::new();
    if st.ink != d.ink {
        look.push_str(&format!(" colour #{:02x}{:02x}{:02x}", st.ink[0], st.ink[1], st.ink[2]));
    }
    if st.weight == Weight::Bold {
        look.push_str(" bold");
    }
    if st.hplace != d.hplace {
        look.push_str(&format!(" {}", side_word(st.hplace)));
    }
    if st.align != d.align {
        look.push_str(&format!(" align {}", side_word(match st.align {
            Align::Centre => HPlace::Centre,
            Align::Left => HPlace::Left,
            Align::Right => HPlace::Right,
        })));
    }
    if st.boxed && st.box_color != d.box_color {
        let c = st.box_color;
        look.push_str(&format!(" box #{:02x}{:02x}{:02x}/{}%", c[0], c[1], c[2], (u32::from(c[3]) * 100 + 127) / 255));
    }
    let boxed = if t.style.boxed { " boxed" } else { "" };
    format!(
        "title {name} \"{}\" {:.3}–{:.3} {} size {} outline {}{boxed}{look} fade {}f/{}f{anim}",
        t.text.replace('\n', "\\n"),
        parse::secs(t.start, fps),
        parse::secs(t.end(), fps),
        place_word(t.style.place),
        t.style.size,
        outline_word(t.style.outline),
        t.fade_in,
        t.fade_out,
        anim = if t.in_anim == TitleAnim::None && t.out_anim == TitleAnim::None {
            String::new()
        } else {
            format!(" anim in {} out {} {}f", t.in_anim.name(), t.out_anim.name(), t.anim_frames)
        }
    )
}

/// The `in_anim` or `out_anim` word, when given: one of the engine's title animations.
fn anim_arg(op: &Value, key: &str) -> Result<Option<TitleAnim>, String> {
    match word_opt(op, key)? {
        None => Ok(None),
        Some(w) => TitleAnim::from_name(w).map(Some).ok_or_else(|| {
            format!("title {key} `{w}` is not none, fade, typewriter, slide_up, slide_left, pop, or word_by_word")
        }),
    }
}

/// The animation fields an op may give, merged onto `t`: `in_anim`, `out_anim` and `anim_secs`.
/// A title that gains an animation and no `anim_secs` takes half a second of it.
fn title_anim_fields(op: &Value, t: &mut Title, fps: f64) -> Result<(), String> {
    let (ins, outs) = (anim_arg(op, "in_anim")?, anim_arg(op, "out_anim")?);
    if let Some(a) = ins {
        t.in_anim = a;
    }
    if let Some(a) = outs {
        t.out_anim = a;
    }
    // A bare number is seconds here, unlike the fades, which are frames.
    match op.get("anim_secs") {
        None | Some(Value::Null) => {}
        Some(v) => t.anim_frames = parse::frame(v, fps, u32::MAX).map_err(|e| format!("`anim_secs`: {e}"))?,
    }
    if t.anim_frames == 0 && (ins.is_some() || outs.is_some()) {
        t.anim_frames = (engine::titleanim::DEFAULT_SECS * fps).round().max(1.) as u32;
    }
    Ok(())
}

/// The style fields an op may give, merged onto `st`: `size`, `outline`, `boxed`, `place`.
fn title_style(op: &Value, mut st: TextStyle) -> Result<TextStyle, String> {
    if let Some(v) = num(op, "size")? {
        let lo = f64::from(SIZE_RANGE.0);
        let hi = f64::from(SIZE_RANGE.1);
        st.size = ranged("title", "size", v, lo, hi)?.round() as u32;
    }
    if let Some(w) = word_opt(op, "outline")? {
        st.outline = match w {
            "none" => Outline::None,
            "thin" => Outline::Thin,
            "thick" => Outline::Thick,
            other => return Err(format!("title outline `{other}` is not none, thin or thick")),
        };
    }
    if let Some(w) = word_opt(op, "place")? {
        st.place = match w {
            "bottom" => Place::Bottom,
            "top" => Place::Top,
            "centre" | "center" => Place::Centre,
            other => return Err(format!("title place `{other}` is not bottom, top or centre")),
        };
    }
    if let Some(b) = flag_opt(op, "boxed")? {
        st.boxed = b;
    }
    if let Some(w) = word_opt(op, "color")?.or(word_opt(op, "colour")?) {
        let hex = hex_digits("color", w, 6)?;
        st.ink = [hex[0], hex[1], hex[2]];
    }
    if let Some(w) = word_opt(op, "weight")? {
        st.weight = match w {
            "regular" => Weight::Regular,
            "bold" => Weight::Bold,
            other => return Err(format!("title weight `{other}` is not regular or bold")),
        };
    }
    if let Some(w) = word_opt(op, "hplace")? {
        st.hplace = match w {
            "centre" | "center" => HPlace::Centre,
            "left" => HPlace::Left,
            "right" => HPlace::Right,
            other => return Err(format!("title hplace `{other}` is not left, centre or right")),
        };
    }
    if let Some(w) = word_opt(op, "align")? {
        st.align = match w {
            "centre" | "center" => Align::Centre,
            "left" => Align::Left,
            "right" => Align::Right,
            other => return Err(format!("title align `{other}` is not left, centre or right")),
        };
    }
    if let Some(w) = word_opt(op, "box_color")?.or(word_opt(op, "box_colour")?) {
        // Six digits change the colour and keep the opacity; eight set both.
        let hex = if w.trim().trim_start_matches('#').len() == 8 {
            hex_digits("box_color", w, 8)?
        } else {
            hex_digits("box_color", w, 6)?
        };
        st.box_color = [hex[0], hex[1], hex[2], hex.get(3).copied().unwrap_or(st.box_color[3])];
    }
    if let Some(v) = num(op, "box_opacity")? {
        st.box_color[3] = (ranged("title", "box_opacity", v, 0.0, 1.0)? * 255.0).round() as u8;
    }
    Ok(st)
}

/// `#ff3366` or `ff3366` (or eight digits with an opacity) as bytes.
fn hex_digits(key: &str, text: &str, digits: usize) -> Result<Vec<u8>, String> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != digits || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        let shape = if digits == 6 { "six hex digits such as \"#ffcc00\"" } else { "six or eight hex digits such as \"#000000\" or \"#000000c8\"" };
        return Err(format!("title {key} {text:?} is not a colour; give {shape}"));
    }
    Ok((0..digits / 2).map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect())
}

/// An optional word argument; missing or null is `None`.
fn word_opt<'a>(op: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.as_str())),
        Some(v) => Err(format!("`{key}` must be a word such as thin, got {}", kind_of(v))),
    }
}

/// The `preset` word, when given: the engine's one-click look ([`TitlePreset`]).
fn title_preset_arg(op: &Value) -> Result<Option<TitlePreset>, String> {
    match word_opt(op, "preset")? {
        None => Ok(None),
        Some(w) => TitlePreset::from_key(w)
            .map(Some)
            .ok_or_else(|| format!("title preset `{w}` is not lower_third, centered or caption_bar")),
    }
}

/// `title_add {at, duration, text, preset?, size?=80, outline?="thin", boxed?=false, place?="centre", fade_in?=0, fade_out?=0, in_anim?, out_anim?, anim_secs?=0.5}`.
/// `at` and `duration` are times (a bare number is seconds). Fades are frames as
/// an integer, or a time string. The title lands in start order; the reply gives
/// the `T<n>` it now holds, and says when titles after it renumber.
fn title_add(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let end = parse::session_end(s);
    let start = need(op, "at", fps, end)?;
    let frames = need(op, "duration", fps, u32::MAX)?;
    let text = req_str(op, "text")?.to_string();
    let base = TextStyle { size: 80, outline: Outline::Thin, boxed: false, place: Place::Centre, ..TextStyle::default() };
    // A preset is the look the title starts in; the fields given after it win over the preset.
    let mut t = Title { start, frames, text, style: base, fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 };
    if let Some(p) = title_preset_arg(op)? {
        t.apply_preset(p, fps);
    }
    t.style = title_style(op, t.style.clone())?;
    t.fade_in = length(op, "fade_in", fps)?.unwrap_or(t.fade_in);
    t.fade_out = length(op, "fade_out", fps)?.unwrap_or(t.fade_out);
    title_anim_fields(op, &mut t, fps)?;
    check_title(&t).map_err(|e| format!("title_add: {e}"))?;
    let warnings = title_warnings(s, &t);
    // The new title is not in the list yet, so every title there is one it may overlap.
    let overlaps: String = s.titles().iter().map(|o| crate::tools::see::overlap_line(o, &t, fps)).collect();
    let idx = s.add_title(t.clone()).map_err(|e| format!("title_add: {e}"))?;
    let mut text = title_line(&title_name(idx), &t, fps);
    if idx + 1 < s.titles().len() {
        text.push_str(" · titles after it renumber");
    }
    text.push_str(&warnings);
    text.push_str(&overlaps);
    Ok(Applied { text, touched: Vec::new() })
}

/// The title an op names: `"T1"`, `"t1"`, or a 1-based number. Returns its index.
fn title_arg(s: &PlaybackSession, op: &Value) -> Result<usize, String> {
    let raw = match op.get("title") {
        None | Some(Value::Null) => return Err("missing `title`: give a title name such as T1".into()),
        Some(Value::Number(x)) => x.to_string(),
        Some(Value::String(t)) => t.trim().to_string(),
        Some(v) => return Err(format!("`title` must be a name such as T1, got {}", kind_of(v))),
    };
    let n = s.titles().len();
    if n == 0 {
        return Err("the timeline has no titles yet; add one with title_add".into());
    }
    let digits = raw.strip_prefix(|c| c == 'T' || c == 't').unwrap_or(&raw);
    match digits.parse::<usize>() {
        Ok(k) if (1..=n).contains(&k) => Ok(k - 1),
        _ => Err(format!("title {raw} is not on this timeline; the titles are T1..T{n}")),
    }
}

/// `title_set {title, ...}`: any of the `title_add` fields, merged onto the title's
/// current settings. Times and fades are as in `title_add`.
fn title_set(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let idx = title_arg(s, op)?;
    let old = s.titles()[idx].clone();
    let end = parse::session_end(s);
    let mut t = old.clone();
    if let Some(at) = opt_time(op, "at", fps, end)? {
        t.start = at;
    }
    if let Some(d) = opt_time(op, "duration", fps, u32::MAX)? {
        t.frames = d;
    }
    if !matches!(op.get("text"), None | Some(Value::Null)) {
        t.text = req_str(op, "text")?.to_string();
    }
    // A preset is applied first, so the fields given after it win over its look.
    if let Some(p) = title_preset_arg(op)? {
        t.apply_preset(p, fps);
    }
    t.style = title_style(op, t.style.clone())?;
    if let Some(f) = length(op, "fade_in", fps)? {
        t.fade_in = f;
    }
    if let Some(f) = length(op, "fade_out", fps)? {
        t.fade_out = f;
    }
    title_anim_fields(op, &mut t, fps)?;
    let name = title_name(idx);
    if t == old {
        return Ok(Applied { text: format!("{name} unchanged"), touched: Vec::new() });
    }
    check_title(&t).map_err(|e| format!("title_set {name}: {e}"))?;
    if !s.set_title(idx, t.clone()) {
        return Err(format!("title_set {name}: the title did not change"));
    }
    // A moved title re-sorts: report the name it has now.
    let now = s.titles().iter().position(|x| *x == t).unwrap_or(idx);
    // Every other title is checked; the title's own entry (at `now`) is skipped.
    let overlaps: String = s
        .titles()
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != now)
        .map(|(_, o)| crate::tools::see::overlap_line(o, &t, fps))
        .collect();
    let text = title_line(&title_name(now), &t, fps) + &title_warnings(s, &t) + &overlaps;
    Ok(Applied { text, touched: Vec::new() })
}

/// The warnings a title's text earns, each a reply line that starts with a newline (empty when
/// the title is fine). Characters neither bundled face draws show as empty boxes, so they are
/// named, the first ten distinct ones. A title that wraps to more than six lines, or whose lines
/// cover more than 40% of the frame height, fills the frame and is named as long.
fn title_warnings(s: &PlaybackSession, t: &Title) -> String {
    let lines: Vec<String> = t.text.split('\n').map(str::to_string).collect();
    let mut out = String::new();
    let missing = missing_glyphs(&lines);
    if !missing.is_empty() {
        const SHOWN: usize = 10;
        let shown: Vec<String> = missing.iter().take(SHOWN).map(char::to_string).collect();
        let more = missing.len().saturating_sub(SHOWN);
        let more = if more > 0 { format!(" … and {more} more") } else { String::new() };
        out.push_str(&format!(
            "\n   warning: these characters have no glyph in the title font and draw as boxes: {}{more}",
            shown.join(" ")
        ));
    }
    let (width, height) = s.resolution();
    let (wrapped, share) = layout_share(&t.style, width, height, &lines);
    if wrapped > 6 || share > 0.40 {
        out.push_str(&format!(
            "\n   warning: this title is long: it wraps to {wrapped} lines and covers {:.0}% of the frame height, so it fills the frame; shorten it or split it into several titles",
            share * 100.0
        ));
    }
    out
}

/// `title_remove {title}`: takes the title off the picture.
fn title_remove(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Applied, String> {
    let idx = title_arg(s, op)?;
    let t = s.titles()[idx].clone();
    let name = title_name(idx);
    let count = s.titles().len();
    if !s.remove_title(idx) {
        return Err(format!("title_remove {name}: the title is not on this timeline"));
    }
    // Titles are named by their start order, so every title after the removed one moves down a number.
    let renamed: Vec<String> = (idx..count - 1).map(|j| format!("{} is now {}", title_name(j + 1), title_name(j))).collect();
    let renamed = if renamed.is_empty() { String::new() } else { format!("; {}", renamed.join(", ")) };
    Ok(Applied { text: format!("removed {}{renamed}", title_line(&name, &t, fps)), touched: Vec::new() })
}

fn done(text: String, touched: Vec<(Lane, u32)>) -> Applied {
    Applied { text, touched }
}

fn refused(who: &str) -> String {
    format!("{who}: the engine refused it (is the clip or lane there, and of the right kind?)")
}

fn kind_word(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::Video => "video",
        LaneKind::Audio => "audio",
        LaneKind::Subtitle => "subtitle",
    }
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

/// A required string argument.
fn req_str<'a>(op: &'a Value, key: &str) -> Result<&'a str, String> {
    match op.get(key) {
        None | Some(Value::Null) => Err(format!("missing argument `{key}`")),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(v) => Err(format!("`{key}` must be a string, got {}", kind_of(v))),
    }
}

/// An optional number argument; missing or null is `None`.
fn num(op: &Value, key: &str) -> Result<Option<f64>, String> {
    num_of(op, key)
}

fn num_of(obj: &Value, key: &str) -> Result<Option<f64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite())
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a number, got {}", kind_of(v))),
    }
}

/// An optional number argument, or `default` when it is missing or null; a
/// given value is checked against `lo..=hi`.
fn or_default(who: &str, op: &Value, key: &str, default: f32, lo: f32, hi: f32) -> Result<f32, String> {
    match num(op, key)? {
        Some(v) => ranged(who, key, v, f64::from(lo), f64::from(hi)),
        None => Ok(default),
    }
}

fn req_num_in(obj: &Value, key: &str, who: &str, lo: f64, hi: f64) -> Result<f32, String> {
    match num_of(obj, key)? {
        Some(v) => ranged(who, key, v, lo, hi),
        None => Err(format!("{who}: missing `{key}` ({}–{})", fmt(lo), fmt(hi))),
    }
}

fn num_kind(b: &Value) -> Result<Option<BandKind>, String> {
    let name = match b.get("kind") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.as_str(),
        Some(v) => return Err(format!("band `kind` must be a string, got {}", kind_of(v))),
    };
    Ok(Some(match name {
        "low_shelf" => BandKind::LowShelf,
        "peak" => BandKind::Peak,
        "high_shelf" => BandKind::HighShelf,
        "low_cut" => BandKind::LowCut,
        "high_cut" => BandKind::HighCut,
        other => {
            return Err(format!(
                "band kind `{other}` is not one of low_shelf, peak, high_shelf, low_cut, high_cut"
            ))
        }
    }))
}

/// A boolean flag; missing or null is `false`.
fn flag(op: &Value, key: &str) -> Result<bool, String> {
    Ok(flag_opt(op, key)?.unwrap_or(false))
}

/// An optional boolean; missing or null is `None`.
fn flag_opt(op: &Value, key: &str) -> Result<Option<bool>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(v) => Err(format!("`{key}` must be true or false, got {}", kind_of(v))),
    }
}

/// Checks a value against `lo..=hi`, naming the who, the key and the range on refusal.
fn ranged(who: &str, key: &str, v: f64, lo: f64, hi: f64) -> Result<f32, String> {
    if v < lo || v > hi {
        return Err(format!("{who}: {key} {} out of range {}–{}", fmt(v), fmt(lo), fmt(hi)));
    }
    Ok(v as f32)
}

/// A bound or value as the refusals print it: plain for the everyday range, scientific
/// beyond it so `1e308` is not echoed as three hundred digits.
fn fmt(v: f64) -> String {
    if v == 0.0 || (v.abs() >= 1e-3 && v.abs() < 1e6) { format!("{v}") } else { format!("{v:e}") }
}

/// The clip argument, resolved: lane, index, the address as written, and its start frame.
fn clip_arg(s: &PlaybackSession, op: &Value) -> Result<(Lane, usize, String, u32), String> {
    let raw = req_str(op, "clip")?;
    let (lane, idx) = super::edit::clip_addr(s, raw)?;
    // Every op that takes a clip changes it (grade, placement, fit, eq, volume, keys), so a
    // locked lane refuses here, once, for all of them.
    check_unlocked(s, &[lane])?;
    let start = start_of(s, lane, idx);
    Ok((lane, idx, raw.to_string(), start))
}

/// The lanes of the take `(lane, idx)` belongs to, the clip's own lane first. Just that lane when
/// the clip is not linked. Subtitle lanes hold no take.
fn take_lanes_of(s: &PlaybackSession, lane: Lane, idx: usize) -> Vec<Lane> {
    let mut out = vec![lane];
    let Some(link) = s.lane_clips(lane).get(idx).and_then(|c| c.link) else {
        return out;
    };
    for l in s.lanes() {
        if l.kind != LaneKind::Subtitle
            && !out.contains(&l)
            && s.lane_clips(l).iter().any(|c| c.link == Some(link))
        {
            out.push(l);
        }
    }
    out
}

/// The lane argument, resolved and checked to exist: the lane and the text as written.
fn lane_arg<'a>(s: &PlaybackSession, op: &'a Value) -> Result<(Lane, &'a str), String> {
    let raw = req_str(op, "lane")?;
    let lane = parse::lane(raw)?;
    if !s.lanes().contains(&lane) {
        let names: Vec<String> = s.lanes().into_iter().map(parse::lane_name).collect();
        return Err(format!(
            "{raw} is not a lane on this timeline; the lanes are {}",
            names.join(", ")
        ));
    }
    Ok((lane, raw))
}

fn start_of(s: &PlaybackSession, lane: Lane, idx: usize) -> u32 {
    if lane.kind == LaneKind::Subtitle {
        s.sub_lane(lane).get(idx).map_or(0, |c| c.start)
    } else {
        s.lane_clips(lane).get(idx).map_or(0, |c| c.start)
    }
}

/// Every clip on `lane`, as (lane, start).
fn starts_on(s: &PlaybackSession, lane: Lane) -> Vec<(Lane, u32)> {
    if lane.kind == LaneKind::Subtitle {
        s.sub_lane(lane).iter().map(|c| (lane, c.start)).collect()
    } else {
        s.lane_clips(lane).iter().map(|c| (lane, c.start)).collect()
    }
}

/// The waveform picture for an audio clip, for a voiceover or any sound with no picture of its
/// own: the same span of its file, painted from its sound by the engine, placed on the first free
/// video lane at the clip's start (or on a new one). Returns the video lane and start it landed
/// on. A clip at a speed other than 1x is refused, since the picture would be the wrong length.
// Wired from edit.rs op_visualizer (the owner of the visualizer op) when the clip is on an audio lane.
pub(crate) fn place_waveform_for_audio(
    s: &mut PlaybackSession,
    lane: Lane,
    idx: usize,
) -> Result<(Lane, u32), String> {
    if lane.kind != LaneKind::Audio {
        return Err(format!("{} is not an audio lane", parse::lane_name(lane)));
    }
    let clip = s
        .lane_clips(lane)
        .get(idx)
        .cloned()
        .ok_or_else(|| format!("no clip {idx} on {}", parse::lane_name(lane)))?;
    if clip.speed != engine::project::Speed::NORMAL || !clip.speed_keys.is_empty() {
        return Err(format!(
            "{} plays at a speed other than 1x, so a waveform picture would not match its length; set the speed to 1x first",
            parse::addr(lane, clip.start)
        ));
    }
    let (path, stream) = {
        let src = s
            .sources()
            .get(clip.source)
            .ok_or_else(|| format!("src:{} is not on this timeline", clip.source))?;
        (src.path.clone(), src.audio_stream)
    };
    let start = clip.start;
    let placed = s
        .place_visualizer(start, &path, stream, clip.in_frame, clip.out_frame, None)
        .map_err(|e| format!("waveform picture for {}: {e}", parse::addr(lane, start)))?;
    if !placed {
        return Err(format!("waveform picture for {}: no room on a video lane", parse::addr(lane, start)));
    }
    // Where it landed: the video clip this placement made, read back from the timeline.
    s.lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Video)
        .find_map(|l| {
            s.lane_clips(l)
                .iter()
                .any(|c| c.start == start && c.visualizer != 0 && c.in_frame == clip.in_frame)
                .then_some((l, start))
        })
        .ok_or_else(|| "the waveform picture was placed but not found on a video lane".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn waveform_picture_for_an_audio_clip_lands_on_a_video_lane() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_tone_48k.wav");
        let mut s = PlaybackSession::open(p).expect("open audio-only asset");
        let audio = Lane::A1;
        let clip = s.lane_clips(audio)[0].clone();
        assert!(s.lanes().iter().all(|l| l.kind != LaneKind::Video || s.lane_clips(*l).is_empty()));
        let (lane, start) = place_waveform_for_audio(&mut s, audio, 0).expect("placed");
        assert_eq!(lane.kind, LaneKind::Video);
        assert_eq!(start, clip.start);
        let pic = s.lane_clips(lane)[0].clone();
        assert_ne!(pic.visualizer, 0, "the picture is a visualizer clip");
        assert_eq!(pic.out_frame - pic.in_frame, clip.out_frame - clip.in_frame, "same span of the file");
        // The audio clip itself stays a plain clip.
        assert_eq!(s.lane_clips(audio)[0].visualizer, 0);
    }

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    fn run(s: &mut PlaybackSession, op: Value) -> Result<Applied, String> {
        apply(s, &op, 30.0).expect("the op is one of this module's")
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn color_merges_and_resets() {
        let mut s = session();
        let a = run(&mut s, json!({"op": "color", "clip": "V1#0", "brightness": 0.1})).unwrap();
        assert!(a.text.contains("brightness 0.10"), "{}", a.text);
        assert_eq!(a.touched, vec![(Lane::V1, s.lane_clips(Lane::V1)[0].start)]);
        let a = run(&mut s, json!({"op": "color", "clip": "V1#0", "saturation": 1.2})).unwrap();
        assert_eq!(a.text, "color V1#0 → saturation 1.20");
        let c = s.color_of(Lane::V1, 0).copied().expect("graded");
        assert!(close(c.saturation, 1.2));
        assert!(close(c.brightness, 0.1), "earlier brightness kept");
        run(&mut s, json!({"op": "color", "clip": "V1#0", "reset": true})).unwrap();
        assert!(s.color_of(Lane::V1, 0).is_none());
    }

    #[test]
    fn color_refuses_out_of_range() {
        let mut s = session();
        let e = run(&mut s, json!({"op": "color", "clip": "V1#0", "saturation": 3.5})).unwrap_err();
        assert_eq!(e, "color V1#0: saturation 3.5 out of range 0–2");
    }

    #[test]
    fn transform_sets_and_resets() {
        let mut s = session();
        run(
            &mut s,
            json!({"op": "transform", "clip": "V1#0", "x": 0.25, "scale": 1.5, "crop": [0.1, 0, 0, 0.2]}),
        )
        .unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        assert!(close(t.pos_x, 0.25) && close(t.scale, 1.5));
        assert!(close(t.crop_l, 0.1) && close(t.crop_b, 0.2));
        assert!(run(&mut s, json!({"op": "transform", "clip": "V1#0", "crop": [0.6, 0, 0, 0]}))
            .unwrap_err()
            .contains("out of range 0–0.5"));
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "reset": true})).unwrap();
        assert!(s.transform_of(Lane::V1, 0).is_none());
    }

    #[test]
    fn transform_corner_places_by_the_clips_own_size() {
        let mut s = session();
        let (cw, ch) = s.resolution();
        let (w, h) = engine::probe_media(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4").as_path())
            .video
            .map(|v| (v.width, v.height))
            .expect("video size");
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "scale": 0.3, "corner": "top-right", "margin": 32})).unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        let (x, y) = engine::transform::corner_offset(engine::transform::Corner::TopRight, 0.3, 32.0 / cw as f32, (w, h), (cw, ch));
        assert!(close(t.pos_x, x) && close(t.pos_y, y), "{} {} vs {x} {y}", t.pos_x, t.pos_y);
        assert!(t.pos_x > 0.0 && t.pos_y < 0.0, "top-right is +x, -y");
        assert!(close(t.scale, 0.3), "the corner keeps the scale it was given");
        // Corner alone keeps the scale already set, and the default margin is 32 px.
        let r = run(&mut s, json!({"op": "transform", "clip": "V1#0", "corner": "bottom-left"})).unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        let (x, y) = engine::transform::corner_offset(engine::transform::Corner::BottomLeft, 0.3, 32.0 / cw as f32, (w, h), (cw, ch));
        assert!(close(t.pos_x, x) && close(t.pos_y, y));
        assert!(r.text.contains("corner bottom-left, margin 32 px"), "{}", r.text);
        // Centre is the origin.
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "corner": "centre"})).unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        assert!(close(t.pos_x, 0.0) && close(t.pos_y, 0.0));
    }

    #[test]
    fn transform_corner_measures_a_4_3_source_by_its_own_size() {
        // A 4:3 clip on the project canvas fits by height, so its corner is not the
        // width-fill answer; the op must measure the file, not assume a 16:9 picture.
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_vui_h264.mp4");
        let mut s = PlaybackSession::open(p).expect("open 4:3 asset");
        let (cw, ch) = s.resolution();
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "scale": 0.3, "corner": "bottom-right"})).unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        let (x, y) = engine::transform::corner_offset(engine::transform::Corner::BottomRight, 0.3, 32.0 / cw as f32, (640, 480), (cw, ch));
        assert!(close(t.pos_x, x) && close(t.pos_y, y), "{} {} vs {x} {y}", t.pos_x, t.pos_y);
    }

    #[test]
    fn transform_corner_refuses_x_y_and_bad_names_plainly() {
        let mut s = session();
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "corner": "top-left", "x": 0.1})).unwrap_err();
        assert!(e.contains("corner sets x and y itself"), "{e}");
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "corner": "top-left", "y": 0.1})).unwrap_err();
        assert!(e.contains("corner sets x and y itself"), "{e}");
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "corner": "middle"})).unwrap_err();
        assert!(e.contains("centre, top-left, top-right, bottom-left or bottom-right"), "{e}");
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "margin": 10})).unwrap_err();
        assert!(e.contains("margin"), "{e}");
        // Refused ops leave the clip alone.
        assert!(s.transform_of(Lane::V1, 0).is_none());
    }

    #[test]
    fn transform_opacity_is_0_to_1_and_shown() {
        let mut s = session();
        let reply = run(
            &mut s,
            json!({"op": "transform", "clip": "V1#0", "opacity": 0.5}),
        )
        .unwrap();
        let t = s.transform_of(Lane::V1, 0).copied().expect("placed");
        assert!(close(t.opacity, 0.5), "opacity {}", t.opacity);
        assert!(reply.text.contains("opacity 0.50"), "the reply shows it: {}", reply.text);

        // Both ends are allowed; anything past them is refused by name.
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "opacity": 0})).unwrap();
        assert!(close(s.transform_of(Lane::V1, 0).unwrap().opacity, 0.0));
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "opacity": 1.5}))
            .unwrap_err();
        assert_eq!(e, "transform V1#0: opacity 1.5 out of range 0–1");
        let e = run(&mut s, json!({"op": "transform", "clip": "V1#0", "opacity": -0.1}))
            .unwrap_err();
        assert!(e.contains("opacity -0.1 out of range"), "{e}");
        // A refused value leaves the clip as it was.
        assert!(close(s.transform_of(Lane::V1, 0).unwrap().opacity, 0.0));
    }

    #[test]
    fn fit_sets_policy() {
        let mut s = session();
        let a = run(&mut s, json!({"op": "fit", "clip": "V1#0", "policy": "fill"})).unwrap();
        assert_eq!(a.text, "fit V1#0 → fill");
        assert_eq!(s.fit_of(Lane::V1, 0), FitPolicy::Fill);
        assert!(run(&mut s, json!({"op": "fit", "clip": "V1#0", "policy": "zoom"})).is_err());
    }

    #[test]
    fn eq_sets_and_resets() {
        let mut s = session();
        run(
            &mut s,
            json!({"op": "eq", "clip": "A1#0", "bands": [{"f": 1000, "db": 3, "q": 1, "kind": "peak"}]}),
        )
        .unwrap();
        let eq = s.eq_of(Lane::new(LaneKind::Audio, 0), 0).expect("equalized");
        assert_eq!(eq.bands.len(), 1);
        assert!(close(eq.bands[0].gain_db, 3.0));
        assert_eq!(eq.bands[0].kind, BandKind::Peak);
        let too_many: Vec<Value> = (0..11).map(|_| json!({"f": 100, "db": 0})).collect();
        assert!(run(&mut s, json!({"op": "eq", "clip": "A1#0", "bands": too_many}))
            .unwrap_err()
            .contains("at most 10"));
        run(&mut s, json!({"op": "eq", "clip": "A1#0", "reset": true})).unwrap();
        assert!(s.eq_of(Lane::new(LaneKind::Audio, 0), 0).is_none());
    }

    #[test]
    fn gain_sets_relative_clamps_refuses_and_undoes() {
        let mut s = session();
        let a1 = Lane::new(LaneKind::Audio, 0);
        let r = run(&mut s, json!({"op": "gain", "clip": "A1#0", "db": -6})).unwrap();
        assert_eq!(r.text, "gain A1#0 → -6.0 dB");
        assert!(close(s.clip_gain_of(a1, 0).unwrap(), -6.0));
        let r = run(&mut s, json!({"op": "gain", "clip": "A1#0", "add_db": -3})).unwrap();
        assert_eq!(r.text, "gain A1#0 → -9.0 dB");
        let r = run(&mut s, json!({"op": "gain", "clip": "A1#0", "add_db": 50})).unwrap();
        assert_eq!(r.text, "gain A1#0 → 24.0 dB", "relative steps clamp to the range");
        assert!(run(&mut s, json!({"op": "gain", "clip": "A1#0", "db": 30})).unwrap_err().contains("out of range"));
        assert!(run(&mut s, json!({"op": "gain", "clip": "A1#0"})).unwrap_err().contains("missing argument"));
        assert!(run(&mut s, json!({"op": "gain", "clip": "A1#0", "db": 1, "add_db": 1})).unwrap_err().contains("not both"));
        let r = run(&mut s, json!({"op": "gain", "clip": "A1#0", "db": 24})).unwrap();
        assert!(r.text.contains("already 24.0 dB"), "{}", r.text);
        let err = run(&mut s, json!({"op": "gain", "clip": "V1#0", "db": -3})).unwrap_err();
        assert!(err.contains("video lane"), "{err}");
        // three applied changes, three undo steps: back to unity
        for _ in 0..3 {
            assert!(s.undo());
        }
        assert!(close(s.clip_gain_of(a1, 0).unwrap(), 0.0));
    }

    #[test]
    fn pan_and_lane_pan_set_clamp_refuse_and_undo() {
        let mut s = session();
        let a1 = Lane::new(LaneKind::Audio, 0);
        let r = run(&mut s, json!({"op": "pan", "clip": "A1#0", "pan": -0.5})).unwrap();
        assert!(r.text.contains("-0.50"), "{}", r.text);
        assert_eq!(s.clip_pan_of(a1, 0), Some(-0.5));
        let r = run(&mut s, json!({"op": "pan", "clip": "A1#0", "pan": -0.5})).unwrap();
        assert!(r.text.contains("already"), "{}", r.text);
        assert!(run(&mut s, json!({"op": "pan", "clip": "A1#0", "pan": 1.5})).unwrap_err().contains("out of range"));
        assert!(run(&mut s, json!({"op": "pan", "clip": "A1#0"})).unwrap_err().contains("missing argument `pan`"));
        assert!(run(&mut s, json!({"op": "pan", "clip": "V1#0", "pan": 0.2})).unwrap_err().contains("video lane"));
        run(&mut s, json!({"op": "lane_pan", "lane": "A1", "pan": 1})).unwrap();
        assert_eq!(s.lane_pan(a1), 1.0);
        assert!(run(&mut s, json!({"op": "lane_pan", "lane": "V1", "pan": 1})).is_err());
        assert!(s.undo());
        assert_eq!(s.lane_pan(a1), 0.0);
        assert!(s.undo());
        assert_eq!(s.clip_pan_of(a1, 0), Some(0.0));
    }

    #[test]
    fn lane_name_and_deinterlace_ops() {
        let mut s = session();
        let v1 = Lane::new(LaneKind::Video, 0);
        let r = run(&mut s, json!({"op": "lane_name", "lane": "V1", "name": "Interview"})).unwrap();
        assert_eq!(r.text, "lane_name V1 → \"Interview\"");
        assert_eq!(s.lane_name(v1), Some("Interview"));
        assert!(run(&mut s, json!({"op": "lane_name", "lane": "V1", "name": "x".repeat(41)})).unwrap_err().contains("at most 40"));
        assert!(run(&mut s, json!({"op": "lane_name", "lane": "V1"})).unwrap_err().contains("give name"));
        assert!(run(&mut s, json!({"op": "lane_name", "lane": "V9", "name": "a"})).is_err());
        // ops still address by position, never by the name
        assert!(run(&mut s, json!({"op": "lane_flags", "lane": "Interview", "hidden": true})).is_err());
        let r = run(&mut s, json!({"op": "lane_name", "lane": "V1", "name": ""})).unwrap();
        assert!(r.text.contains("name cleared"), "{}", r.text);
        assert!(s.undo());
        assert_eq!(s.lane_name(v1), Some("Interview"));
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": true})).unwrap();
        assert!(run(&mut s, json!({"op": "lane_name", "lane": "V1", "name": "b"})).unwrap_err().contains("locked"));
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": false})).unwrap();

        use engine::project::Deinterlace;
        let r = run(&mut s, json!({"op": "deinterlace", "clip": "V1#0", "mode": "field"})).unwrap();
        assert_eq!(r.text, "deinterlace V1#0 → field");
        assert_eq!(s.deinterlace_of(v1, 0), Deinterlace::Field);
        let r = run(&mut s, json!({"op": "deinterlace", "clip": "V1#0", "mode": "field"})).unwrap();
        assert!(r.text.contains("unchanged"), "{}", r.text);
        assert!(run(&mut s, json!({"op": "deinterlace", "clip": "V1#0", "mode": "weave"})).unwrap_err().contains("frame, field or off"));
        assert!(run(&mut s, json!({"op": "deinterlace", "clip": "A1#0", "mode": "off"})).is_err());
        assert!(s.undo());
        assert_eq!(s.deinterlace_of(v1, 0), Deinterlace::Frame);
    }

    #[test]
    fn lane_gain_and_limiter() {
        let mut s = session();
        let a1 = Lane::new(LaneKind::Audio, 0);
        run(&mut s, json!({"op": "lane_gain", "lane": "A1", "db": -6})).unwrap();
        assert!(close(s.lane_gain_db(a1), -6.0));
        assert!(run(&mut s, json!({"op": "lane_gain", "lane": "A1", "db": 20}))
            .unwrap_err()
            .contains("out of range -60–12"));
        run(&mut s, json!({"op": "limiter", "on": false, "ceiling_db": -3})).unwrap();
        let l = s.limiter();
        assert!(!l.on && close(l.ceiling_db, -3.0));
        assert!(run(&mut s, json!({"op": "limiter", "ceiling_db": 2}))
            .unwrap_err()
            .contains("out of range -24–0"));
    }

    #[test]
    fn lane_move_to_a_number_moves_that_lane_and_the_reply_says_where() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_add", "kind": "video"})).unwrap();
        let v1 = Lane::new(LaneKind::Video, 0);
        let v2 = Lane::new(LaneKind::Video, 1);
        // Mark V2 so the test can tell which lane ended up at position 1.
        run(&mut s, json!({"op": "lane_flags", "lane": "V2", "hidden": true})).unwrap();
        assert!(s.lane_hidden(v2) && !s.lane_hidden(v1));
        // The number form, and a whole number written as a float: both are `to:1`.
        let r = run(&mut s, json!({"op": "lane_move", "lane": "V2", "to": 1})).unwrap();
        assert!(r.text.contains("(position 1)"), "{}", r.text);
        assert_eq!(s.lanes()[0], v1, "the lane at position 1 is the first video lane again");
        assert!(s.lane_hidden(v1), "the moved lane (the hidden one) is now at position 1");
        assert!(!s.lane_hidden(v2));
        // Already there: a true no-op reply, not a second move.
        let again = run(&mut s, json!({"op": "lane_move", "lane": "V1", "to": 1.0})).unwrap();
        assert!(again.text.contains("already at position 1"), "{}", again.text);
        assert!(run(&mut s, json!({"op": "lane_move", "lane": "V1", "to": 1.5}))
            .unwrap_err()
            .contains("out of range"));
    }

    #[test]
    fn lanes_add_move_remove_and_flags() {
        let mut s = session();
        let before = s.lanes().len();
        let added = run(&mut s, json!({"op": "lane_add", "kind": "video"})).unwrap();
        assert_eq!(s.lanes().len(), before + 1);
        assert!(added.text.contains("V2"), "{}", added.text);

        run(&mut s, json!({"op": "lane_move", "lane": "V2", "to": 1})).unwrap();
        assert_eq!(s.lanes()[0].kind, LaneKind::Video);
        assert_eq!(s.lanes()[0].ord, 0);
        assert!(run(&mut s, json!({"op": "lane_move", "lane": "V1", "to": 99}))
            .unwrap_err()
            .contains("out of range"));

        // The empty video lane is now V1 at position 1; take it off again.
        run(&mut s, json!({"op": "lane_remove", "lane": "V1"})).unwrap();
        assert_eq!(s.lanes().len(), before);
        let a1 = Lane::new(LaneKind::Audio, 0);
        run(&mut s, json!({"op": "lane_flags", "lane": "A1", "muted": true, "locked": true})).unwrap();
        assert!(s.lane_muted(a1));
        assert!(s.lane_locked(a1));
        assert!(run(&mut s, json!({"op": "lane_flags", "lane": "A1", "hidden": true}))
            .unwrap_err()
            .contains("hidden does not apply to audio lanes"));
    }

    #[test]
    fn group_and_ungroup() {
        let mut s = session();
        let a = run(&mut s, json!({"op": "group", "clips": ["V1#0", "A1#0"]})).unwrap();
        assert_eq!(a.touched.len(), 2);
        run(&mut s, json!({"op": "ungroup", "clip": "V1#0"})).unwrap();
    }

    /// Grouping a take's picture with a loose clip on V2 and then ungrouping the
    /// picture leaves the take (V1 with its sound on A1) linked: the loose clip
    /// is the only half that comes apart.
    #[test]
    fn ungroup_after_grouping_a_take_member_keeps_the_take() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_add", "kind": "video"})).unwrap();
        let v2 = Lane::new(LaneKind::Video, 1);
        let mut loose = s.lane_clips(Lane::V1)[0].clone();
        loose.link = None;
        // A later start than the take: a loose copy at the same frames would be a third half of it.
        assert!(s.place_at(v2, 1.0, loose));
        run(&mut s, json!({"op": "group", "clips": ["V1#0", "V2#0"]})).unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[0].link, s.lane_clips(v2)[0].link, "one group");
        run(&mut s, json!({"op": "ungroup", "clip": "V1#0"})).unwrap();
        let v1_link = s.lane_clips(Lane::V1)[0].link;
        assert!(v1_link.is_some());
        assert_eq!(v1_link, s.lane_clips(Lane::A1)[0].link, "the take is still linked");
        assert_ne!(s.lane_clips(v2)[0].link, v1_link, "only the loose clip came apart");
    }

    #[test]
    fn subtitles_import_adds_tracks() {
        let mut s = session();
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        let a = run(&mut s, json!({"op": "subtitles_import", "path": srt.to_str().unwrap()})).unwrap();
        assert!(a.text.starts_with("imported 1 track (#0, "), "{}", a.text);
        assert!(a.text.contains("not on the timeline yet"), "{}", a.text);
        assert!(a.text.contains("captions_place {track:0}"), "{}", a.text);
        assert!(a.touched.is_empty());
    }

    #[test]
    fn subtitles_import_takes_the_language_from_the_file_name_or_lang() {
        let td = crate::testutil::TestDir::new("r30b_fx_lang");
        let body = "1\n00:00:01,000 --> 00:00:02,000\nBonjour\n";
        let fra = td.path().join("fra.srt");
        std::fs::write(&fra, body).unwrap();
        let mut s = session();
        let a = run(&mut s, json!({"op": "subtitles_import", "path": fra.to_str().unwrap()})).unwrap();
        assert!(a.text.contains(" · lang fra, from the file name"), "{}", a.text);
        let mut s = session();
        let a = run(&mut s, json!({"op": "subtitles_import", "path": fra.to_str().unwrap(), "lang": "deu"})).unwrap();
        assert!(a.text.contains(" · lang deu, as given"), "{}", a.text);
        assert!(!a.text.contains("lang fra"), "{}", a.text);
    }

    #[test]
    fn subtitles_import_names_the_cues_it_skipped() {
        let td = crate::testutil::TestDir::new("f17_skipped_cues");
        let srt = td.path().join("bad.srt");
        std::fs::write(
            &srt,
            "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n\
             2\n00:00:08,500 --> 00:00:08,000\nReversed\n\n\
             3\n00:00:06,000 --> 00:00:07,000\nFive\n",
        )
        .unwrap();
        let mut s = session();
        let a = run(&mut s, json!({"op": "subtitles_import", "path": srt.to_str().unwrap()})).unwrap();
        assert!(a.text.contains("imported 1 track"), "{}", a.text);
        assert!(
            a.text.ends_with("skipped 1 cue (line 6), not imported: end before start"),
            "{}",
            a.text
        );
    }

    #[test]
    fn several_skipped_cues_are_counted_as_cues_and_named_by_line() {
        let td = crate::testutil::TestDir::new("r27_skipped_cues");
        let srt = td.path().join("bad2.srt");
        std::fs::write(
            &srt,
            "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n\
             2\n00:00:08,500 --> 00:00:08,000\nReversed\n\n\
             3\n00:00:07,000 --> 00:00:06,000\nAlso reversed\n\n\
             4\n00:00:09,000 --> 00:00:10,000\nFour\n",
        )
        .unwrap();
        let mut s = session();
        let a = run(&mut s, json!({"op": "subtitles_import", "path": srt.to_str().unwrap()})).unwrap();
        assert!(a.text.ends_with("skipped 2 cues (lines 6, 10), not imported: end before start; end before start"), "{}", a.text);
    }

    /// A title over another in the same place is named as overlapping, by the window they share;
    /// a title in another place is not.
    #[test]
    fn a_title_overlapping_another_in_the_same_place_is_named() {
        let mut srv = open_server();
        let r = titles_ed(&mut srv, json!([{ "op": "title_add", "at": "0s", "duration": "3s", "text": "one", "place": "centre" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("overlap"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{ "op": "title_add", "at": "1s", "duration": "2s", "text": "two", "place": "centre" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("warning: two titles overlap at centre"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{ "op": "title_add", "at": "1s", "duration": "2s", "text": "three", "place": "bottom" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("overlap"), "{}", r.text);
        // title_set on the first title skips its own entry, and names the second (overlapping) one.
        let r = titles_ed(&mut srv, json!([{ "op": "title_set", "title": "T1", "duration": "4s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.matches("two titles overlap").count() >= 1, "{}", r.text);
        assert!(!r.text.contains("T1 overlap"), "{}", r.text);
    }

    #[test]
    fn a_second_key_on_a_lane_adds_a_duck() {
        let mut s = session();
        for _ in 0..3 {
            run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        }
        let a4 = Lane::new(LaneKind::Audio, 3);
        let first = run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2"})).unwrap();
        assert!(first.text.contains("now ducked under A2 (-12 dB"), "{}", first.text);
        let second = run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A3", "depth_db": -6})).unwrap();
        assert!(
            second.text.contains("now ducked under A2 (-12 dB when over -40 dBFS), A3 (-6 dB"),
            "{}",
            second.text
        );
        let unders: Vec<usize> = s.lane_duck_list(a4).iter().map(|d| d.under).collect();
        assert_eq!(unders, vec![1, 2], "both ducks stand, in the order set");
        // The same key again replaces its duck; the other stays.
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2", "depth_db": -20})).unwrap();
        let d = s.lane_duck_list(a4);
        assert_eq!(d.len(), 2);
        assert!(close(d[0].depth_db, -20.0) && d[1].under == 2, "replaced in place, the other kept");
    }

    #[test]
    fn duck_off_removes_only_its_key_and_off_alone_clears_all() {
        let mut s = session();
        for _ in 0..3 {
            run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        }
        let a4 = Lane::new(LaneKind::Audio, 3);
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2"})).unwrap();
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A3"})).unwrap();
        let one = run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2", "off": true})).unwrap();
        assert!(one.text.contains("A4 under A2 → removed; A4 now ducked under A3"), "{}", one.text);
        assert_eq!(s.lane_duck_list(a4).iter().map(|d| d.under).collect::<Vec<_>>(), vec![2]);
        assert_eq!(
            run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2", "off": true})).unwrap().text,
            "duck A4 under A2 → already off"
        );
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A1"})).unwrap();
        let all = run(&mut s, json!({"op": "duck", "lane": "A4", "off": true})).unwrap();
        assert!(all.text.contains("all 2 ducks removed (A3, A1)"), "{}", all.text);
        assert!(s.lane_duck_list(a4).is_empty());
        assert_eq!(run(&mut s, json!({"op": "duck", "lane": "A4", "off": true})).unwrap().text, "duck A4 → already off");
    }

    #[test]
    fn two_ducks_undo_as_one_step_each() {
        let mut s = session();
        for _ in 0..3 {
            run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        }
        let a4 = Lane::new(LaneKind::Audio, 3);
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2"})).unwrap();
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A3"})).unwrap();
        run(&mut s, json!({"op": "duck", "lane": "A4", "under": "A2", "off": true})).unwrap();
        assert_eq!(s.lane_duck_list(a4).len(), 1);
        assert!(s.undo());
        assert_eq!(s.lane_duck_list(a4).len(), 2, "undo restores the removed key");
        assert!(s.undo());
        assert_eq!(s.lane_duck_list(a4).len(), 1, "undo takes the second key off");
    }

    #[test]
    fn duck_sets_defaults_and_custom_values() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        let a2 = Lane::new(LaneKind::Audio, 1);
        let a = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1"})).unwrap();
        assert_eq!(
            a.text,
            "duck A2 under A1: -12 dB when A1 > -40 dBFS (attack 80 ms, release 400 ms); A2 now ducked under A1 (-12 dB when over -40 dBFS)"
        );
        let d = s.lane_duck(a2).expect("ducked");
        assert_eq!(d.under, 0);
        assert!(close(d.depth_db, -12.0) && close(d.threshold_db, -40.0));
        assert!(close(d.attack_ms, 80.0) && close(d.release_ms, 400.0));
        assert_eq!(run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1"})).unwrap().text, "duck A2 → already set");
        run(
            &mut s,
            json!({"op": "duck", "lane": "A2", "under": "A1", "depth_db": -6, "threshold_db": -30, "attack_ms": 10, "release_ms": 900}),
        )
        .unwrap();
        let d = s.lane_duck(a2).expect("re-ducked");
        assert!(close(d.depth_db, -6.0) && close(d.threshold_db, -30.0));
        assert!(close(d.attack_ms, 10.0) && close(d.release_ms, 900.0));

        run(&mut s, json!({"op": "duck", "lane": "A2", "off": true})).unwrap();
        assert!(s.lane_duck(a2).is_none(), "off removes the duck");
        assert_eq!(run(&mut s, json!({"op": "duck", "lane": "A2", "off": true})).unwrap().text, "duck A2 → already off");
    }

    #[test]
    fn duck_refusals_name_the_rule() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        let self_key = run(&mut s, json!({"op": "duck", "lane": "A1", "under": "A1"})).unwrap_err();
        assert!(self_key.contains("not its own key"), "{self_key}");
        let video = run(&mut s, json!({"op": "duck", "lane": "V1", "under": "A1"})).unwrap_err();
        assert!(video.contains("only audio lanes duck"), "{video}");
        let no_key = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A9"})).unwrap_err();
        assert!(no_key.contains("A9 is not a lane"), "{no_key}");
        let no_under = run(&mut s, json!({"op": "duck", "lane": "A2"})).unwrap_err();
        assert!(no_under.contains("give under"), "{no_under}");
        let deep = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1", "depth_db": -50})).unwrap_err();
        assert!(deep.contains("depth_db -50 out of range -40–0"), "{deep}");
        let slow = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1", "attack_ms": 5000})).unwrap_err();
        assert!(slow.contains("attack_ms 5000 out of range 1–2000"), "{slow}");
        let loud = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1", "threshold_db": 3})).unwrap_err();
        assert!(loud.contains("threshold_db 3 out of range -90–0"), "{loud}");
        assert!(s.lane_duck(Lane::new(LaneKind::Audio, 1)).is_none(), "no refusal changed the lane");
    }

    #[test]
    fn duck_refuses_a_locked_lane_and_undoes() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_add", "kind": "audio"})).unwrap();
        let a2 = Lane::new(LaneKind::Audio, 1);
        run(&mut s, json!({"op": "lane_flags", "lane": "A2", "locked": true})).unwrap();
        let e = run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1"})).unwrap_err();
        assert!(e.contains("lane A2 is locked"), "{e}");
        run(&mut s, json!({"op": "lane_flags", "lane": "A2", "locked": false})).unwrap();
        run(&mut s, json!({"op": "duck", "lane": "A2", "under": "A1"})).unwrap();
        assert!(s.lane_duck(a2).is_some());
        assert!(s.undo(), "the duck is one undo step");
        assert!(s.lane_duck(a2).is_none(), "undo takes the duck off");
    }

    #[test]
    fn every_op_that_changes_a_locked_lane_refuses_and_names_it() {
        let mut s = session();
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": true})).unwrap();
        let clips_before = s.lane_clips(Lane::V1).len();
        let on_v1 = [
            json!({"op": "color", "clip": "V1#0", "brightness": 0.1}),
            json!({"op": "color", "clip": "V1#0", "reset": true}),
            json!({"op": "transform", "clip": "V1#0", "x": 0.1}),
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s", "scale": 1}]}),
            json!({"op": "fit", "clip": "V1#0", "policy": "fill"}),
            json!({"op": "eq", "clip": "V1#0", "reset": true}),
            json!({"op": "ungroup", "clip": "V1#0"}),
            json!({"op": "group", "clips": ["V1#0", "A1#0"]}),
            json!({"op": "lane_remove", "lane": "V1"}),
            json!({"op": "lane_flags", "lane": "V1", "hidden": true}),
            json!({"op": "lane_gain", "lane": "V1", "db": 3}),
        ];
        for op in on_v1 {
            let e = run(&mut s, op.clone()).unwrap_err();
            assert!(e.contains("lane V1 is locked") && e.contains("lane_flags"), "{op}: {e}");
        }
        assert!(s.color_of(Lane::V1, 0).is_none(), "no refused op changed the clip");
        assert_eq!(s.lane_clips(Lane::V1).len(), clips_before);
        assert!(s.lanes().contains(&Lane::V1));
        // Unlocking is the way out, and a locked lane's own lock may still be changed.
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": false})).unwrap();
        run(&mut s, json!({"op": "color", "clip": "V1#0", "brightness": 0.1})).unwrap();
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": true})).unwrap();
        // Audio: a volume edit on a locked audio lane refuses, and so does ungrouping a take
        // whose audio partner is locked.
        run(&mut s, json!({"op": "lane_flags", "lane": "V1", "locked": false})).unwrap();
        run(&mut s, json!({"op": "lane_flags", "lane": "A1", "locked": true})).unwrap();
        for op in [
            json!({"op": "gain", "clip": "A1#0", "db": -3}),
            json!({"op": "gain_keys", "clip": "A1#0", "keys": [["0s", -3]]}),
            json!({"op": "ungroup", "clip": "V1#0"}),
        ] {
            let e = run(&mut s, op.clone()).unwrap_err();
            assert!(e.contains("lane A1 is locked"), "{op}: {e}");
        }
    }

    #[test]
    fn unknown_op_is_not_ours() {
        let mut s = session();
        assert!(apply(&mut s, &json!({"op": "split", "at": "5s"}), 30.0).is_none());
        assert!(apply(&mut s, &json!({"clip": "V1#0"}), 30.0).is_none());
    }

    fn audio_clip(srv: &crate::server::Server) -> (Lane, Clip) {
        let s = srv.session.as_ref().unwrap();
        let a1 = Lane::new(LaneKind::Audio, 0);
        (a1, s.lane_clips(a1)[0].clone())
    }

    /// The test asset is 5 s long. A1 moved to 2 s and trimmed in to 3 s starts at
    /// 3 s on the timeline with its in point 1 s into the source, so the envelope's
    /// timeline and source frames differ and a conversion mistake shows.
    fn trimmed_audio(srv: &mut crate::server::Server) {
        let r = titles_ed(
            srv,
            json!([
                {"op": "move", "clip": "A1#0", "at": "2s"},
                {"op": "trim", "clip": "A1#0", "edge": "in", "to": "3s"}
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let (_, c) = audio_clip(srv);
        assert!(c.start > 0 && c.in_frame > 0, "start {} in {}", c.start, c.in_frame);
    }

    fn fps_of(srv: &crate::server::Server) -> f64 {
        parse::session_fps(srv.session.as_ref().unwrap())
    }

    fn frame_at(fps: f64, secs: f64) -> u32 {
        (secs * fps).round() as u32
    }

    fn db_at(srv: &crate::server::Server, secs: f64) -> f32 {
        let s = srv.session.as_ref().unwrap();
        let fps = parse::session_fps(s);
        s.gain_db_at(Lane::new(LaneKind::Audio, 0), 0, frame_at(fps, secs)).unwrap()
    }

    fn points(srv: &crate::server::Server) -> Vec<GainKey> {
        srv.session.as_ref().unwrap().gain_keys_of(Lane::new(LaneKind::Audio, 0), 0).to_vec()
    }

    #[test]
    fn a_huge_bound_echoes_in_scientific_notation() {
        let mut s = session();
        let err = run(&mut s, json!({"op": "gain", "clip": "A1#0", "db": 1e308})).unwrap_err();
        assert!(err.contains("1e308") && err.len() < 200, "{err}");
        assert_eq!(fmt(0.0), "0");
        assert_eq!(fmt(0.5), "0.5");
        assert_eq!(fmt(-1e-5), "-1e-5");
    }

    #[test]
    fn gain_keys_takes_timeline_times_converts_and_undoes() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        let (_, c) = audio_clip(&srv);
        let fps = fps_of(&srv);
        let r = titles_ed(&mut srv, json!([{"op": "gain_keys", "clip": "A1#0", "keys": [["3.5s", -12.0], ["4s", -3]]}]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 point(s)") && r.text.contains("3.500s -12.0 dB"), "{}", r.text);
        // The points land on the timeline times asked for, through the clip's in point.
        assert!(close(db_at(&srv, 3.5), -12.0));
        assert!(close(db_at(&srv, 4.0), -3.0));
        // Held beyond the last point, and the envelope is stored on source frames.
        assert!(close(db_at(&srv, 4.9), -3.0));
        let keys = points(&srv);
        assert_eq!(keys[0].at, c.in_frame + (frame_at(fps, 3.5) - c.start));
        // Outside the clip is refused, naming the clip's span.
        let bad = titles_ed(&mut srv, json!([{"op": "gain_keys", "clip": "A1#0", "keys": [["1s", -3]]}]));
        assert!(bad.is_error && bad.text.contains("outside") && bad.text.contains("3.000s"), "{}", bad.text);
        // A video clip refuses by name.
        let v = titles_ed(&mut srv, json!([{"op": "gain_keys", "clip": "V1#0", "keys": []}]));
        assert!(v.is_error && v.text.contains("video lane"), "{}", v.text);
        // An empty list clears; undo brings the two points back.
        let clr = titles_ed(&mut srv, json!([{"op": "gain_keys", "clip": "A1#0", "keys": []}]));
        assert!(!clr.is_error && clr.text.contains("cleared"), "{}", clr.text);
        assert!(points(&srv).is_empty());
        super::super::edit::run_undo(&mut srv, &json!({}));
        assert_eq!(points(&srv).len(), 2);
    }

    #[test]
    fn gain_key_adds_replaces_removes_and_names_misses() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        let r = titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": "4s", "db": -6}]));
        assert!(!r.is_error, "{}", r.text);
        assert!(close(db_at(&srv, 4.0), -6.0));
        // The same frame is replaced, not doubled.
        let r = titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": "4s", "db": -9}]));
        assert!(!r.is_error && points(&srv).len() == 1, "{}", r.text);
        assert!(close(db_at(&srv, 4.0), -9.0));
        // A time a frame off the point still removes it.
        let rm = titles_ed(&mut srv, json!([{"op": "gain_key_remove", "clip": "A1#0", "at": "4.02s"}]));
        assert!(!rm.is_error && rm.text.contains("removed 4.000s -9.0 dB"), "{}", rm.text);
        assert!(points(&srv).is_empty());
        // Nothing near: the refusal says so.
        let none = titles_ed(&mut srv, json!([{"op": "gain_key_remove", "clip": "A1#0", "at": "4s"}]));
        assert!(none.is_error && none.text.contains("no gain keys"), "{}", none.text);
        // With points present, a miss lists them.
        titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": "4s", "db": -6}]));
        let miss = titles_ed(&mut srv, json!([{"op": "gain_key_remove", "clip": "A1#0", "at": "3.5s"}]));
        assert!(miss.is_error && miss.text.contains("4.000s -6.0 dB"), "{}", miss.text);
        // A bad dB is refused by name.
        let bad = titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": "4s", "db": 30}]));
        assert!(bad.is_error && bad.text.contains("out of range"), "{}", bad.text);
    }

    #[test]
    fn gain_ramp_replaces_inside_and_keeps_outside() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        titles_ed(
            &mut srv,
            json!([{"op": "gain_keys", "clip": "A1#0", "keys": [["3.5s", -3], ["4s", -3], ["4.5s", -3]]}]),
        );
        // from_db defaults to the level at 3.8 s (-3 dB here); the 4 s point is inside and goes.
        let r = titles_ed(
            &mut srv,
            json!([{"op": "gain_ramp", "clip": "A1#0", "from": "3.8s", "to": "4.2s", "to_db": -18}]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(close(db_at(&srv, 3.8), -3.0));
        assert!(close(db_at(&srv, 4.2), -18.0));
        assert!(close(db_at(&srv, 4.0), -10.5), "midpoint of the ramp: {}", db_at(&srv, 4.0));
        assert_eq!(points(&srv).len(), 4, "3.5, 3.8, 4.2, 4.5");
        // An explicit from_db wins over the level there.
        titles_ed(&mut srv, json!([{"op": "gain_ramp", "clip": "A1#0", "from": "3.8s", "to": "4.2s", "from_db": -6, "to_db": -18}]));
        assert!(close(db_at(&srv, 3.8), -6.0));
        // from must come before to.
        let back = titles_ed(&mut srv, json!([{"op": "gain_ramp", "clip": "A1#0", "from": "4s", "to": "3.5s", "to_db": -3}]));
        assert!(back.is_error && back.text.contains("must be after"), "{}", back.text);
        let v = titles_ed(&mut srv, json!([{"op": "gain_ramp", "clip": "V1#0", "from": "3.5s", "to": "4s", "to_db": -3}]));
        assert!(v.is_error && v.text.contains("video lane"), "{}", v.text);
    }

    #[test]
    fn gain_dip_shapes_its_ramps_and_clamps_to_the_clip() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        let r = titles_ed(
            &mut srv,
            json!([{"op": "gain_dip", "clip": "A1#0", "from": "3.5s", "to": "4.5s", "db": -18, "ramp": "0.2s"}]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("3.500–4.500s") && r.text.contains("(ramps 0.2s), 4 points"), "{}", r.text);
        // Level now at the ramp start and end (0 dB), the dip itself, and the ramps between.
        let fps = fps_of(&srv);
        let at = |f: u32| {
            let s = srv.session.as_ref().unwrap();
            s.gain_db_at(Lane::new(LaneKind::Audio, 0), 0, f).unwrap()
        };
        let from = frame_at(fps, 3.5);
        let to = frame_at(fps, 4.5);
        let ramp = frame_at(fps, 0.2);
        assert!(close(at(from - ramp), 0.0), "ramp start");
        assert!(close(at(from), -18.0), "dip starts");
        assert!(close(at(from - ramp / 2), -9.0), "halfway down the ramp");
        assert!(close(at(from + (to - from) / 2), -18.0), "mid dip");
        assert!(close(at(to), -18.0), "dip ends");
        assert!(close(at(to + ramp), 0.0), "ramp end");
        assert!(close(at(to + ramp + 10), 0.0), "held after");
        // Ramps reaching past the clip start are clamped to it: the first ramp point is the clip start.
        let mut srv2 = open_server();
        trimmed_audio(&mut srv2);
        let r = titles_ed(
            &mut srv2,
            json!([{"op": "gain_dip", "clip": "A1#0", "from": "3s", "to": "3.5s", "db": -18}]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("3 points"), "the ramp in starts at the clip start, so only three points: {}", r.text);
        // Undo takes the dip back off.
        super::super::edit::run_undo(&mut srv, &json!({}));
        assert!(points(&srv).is_empty());
    }

    #[test]
    fn gain_ops_accept_the_clip_start_and_its_displayed_end() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        let (_, c) = audio_clip(&srv);
        // The end is the time the clip's last frame plays to, so it is a valid time.
        let start = format!("{}f", c.start);
        let end = format!("{}f", c.end());
        let dip = titles_ed(
            &mut srv,
            json!([{"op": "gain_dip", "clip": "A1#0", "from": start, "to": end, "db": -12, "ramp": "0.2s"}]),
        );
        assert!(!dip.is_error && dip.text.contains("2 points"), "{}", dip.text);
        let key = titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": end, "db": -6}]));
        assert!(!key.is_error, "{}", key.text);
        let rm = titles_ed(&mut srv, json!([{"op": "gain_key_remove", "clip": "A1#0", "at": end}]));
        assert!(!rm.is_error && rm.text.contains("removed"), "{}", rm.text);
        let keys = titles_ed(&mut srv, json!([{"op": "gain_keys", "clip": "A1#0", "keys": [[&end, -3], [&start, -1]]}]));
        assert!(!keys.is_error, "{}", keys.text);
        let ramp = titles_ed(
            &mut srv,
            json!([{"op": "gain_ramp", "clip": "A1#0", "from": start, "to": end, "to_db": -18}]),
        );
        assert!(!ramp.is_error, "{}", ramp.text);
        // One frame past the end is still outside the clip, and refused by name.
        let past = format!("{}f", c.end() + 1);
        let bad = titles_ed(&mut srv, json!([{"op": "gain_key", "clip": "A1#0", "at": past, "db": -6}]));
        assert!(bad.is_error && bad.text.contains("outside"), "{}", bad.text);
    }

    #[test]
    fn gain_dip_names_a_ramp_the_clip_bounds_shorten() {
        let mut srv = open_server();
        trimmed_audio(&mut srv);
        let (_, c) = audio_clip(&srv);
        let fps = fps_of(&srv);
        // Both ends sit 0.1 s inside the clip, so the default 0.5 s ramps can only be 0.1 s.
        let from = c.start + frame_at(fps, 0.1);
        let to = c.end() - frame_at(fps, 0.1);
        let r = titles_ed(
            &mut srv,
            json!([{"op": "gain_dip", "clip": "A1#0", "from": format!("{from}f"), "to": format!("{to}f"), "db": -18}]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(note:"), "the cut ramps are named: {}", r.text);
        assert!(r.text.contains("ramp in shortened to 0.1s: the clip starts at"), "{}", r.text);
        assert!(r.text.contains("ramp out shortened to 0.1s: the clip ends at"), "{}", r.text);
        // The shortened ramps are still applied: the dip itself holds at its level.
        assert!(close(db_at(&srv, f64::from(to) / fps), -18.0), "the dip still lands");
    }

    fn open_server() -> crate::server::Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut srv = crate::server::Server::new();
        srv.session = Some(PlaybackSession::open(p).expect("open test asset"));
        srv
    }

    fn titles_ed(srv: &mut crate::server::Server, ops: Value) -> crate::server::Reply {
        super::super::edit::run_edit(srv, &json!({ "ops": ops }))
    }

    fn texts(srv: &crate::server::Server) -> Vec<String> {
        srv.session.as_ref().unwrap().titles().iter().map(|t| t.text.clone()).collect()
    }

    #[test]
    fn a_title_animation_is_set_by_title_add_and_title_set() {
        let mut srv = open_server();
        let fps = srv.session.as_ref().unwrap().meta().frame_rate.round() as u32;
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "1s", "duration": 3, "text": "Hi there", "in_anim": "typewriter"}]));
        assert!(!r.is_error && r.text.contains("anim in typewriter out none"), "{}", r.text);
        // A bare number is seconds, so `anim_secs: 1` is a second of frames, not one frame.
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T1", "out_anim": "slide_up", "anim_secs": 1}]));
        assert!(!r.is_error, "{}", r.text);
        {
            let t = &srv.session.as_ref().unwrap().titles()[0];
            assert_eq!((t.in_anim, t.out_anim, t.anim_frames), (engine::titleanim::TitleAnim::Typewriter, engine::titleanim::TitleAnim::SlideUp, fps));
        }
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T1", "in_anim": "wobble"}]));
        assert!(r.is_error && r.text.contains("wobble"), "{}", r.text);
    }

    #[test]
    fn title_add_set_remove_and_undo_through_edit() {
        let mut srv = open_server();
        let r = titles_ed(
            &mut srv,
            json!([{"op": "title_add", "at": "1s", "duration": 3, "text": "Episode 17", "fade_in": 12, "fade_out": 12}]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("title T1 \"Episode 17\" 1.000–4.000 centre size 80 outline thin fade 12f/12f"), "{}", r.text);

        // set by name, by lower-case name, and by number; fields merge onto the old ones
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T1", "size": 120}]));
        assert!(!r.is_error && r.text.contains("size 120"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": 1, "boxed": true, "place": "bottom"}]));
        assert!(!r.is_error, "{}", r.text);
        {
            let t = &srv.session.as_ref().unwrap().titles()[0];
            assert_eq!(t.style.size, 120);
            assert!(t.style.boxed);
            assert_eq!(t.style.place, engine::text::Place::Bottom);
            assert_eq!(t.text, "Episode 17");
        }

        // a title that lands before T1 takes the name T1; the old one is T2
        let r = titles_ed(
            &mut srv,
            json!([{"op": "title_add", "at": "0", "duration": 1, "text": "Cold open"}]),
        );
        assert!(!r.is_error && r.text.contains("title T1 \"Cold open\""), "{}", r.text);
        assert!(r.text.contains("titles after it renumber"), "{}", r.text);
        assert_eq!(texts(&srv), vec!["Cold open", "Episode 17"]);

        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "t2", "text": "Episode 18"}]));
        assert!(!r.is_error && r.text.contains("\"Episode 18\""), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_remove", "title": "T1"}]));
        assert!(!r.is_error && r.text.contains("removed title T1 \"Cold open\""), "{}", r.text);
        assert_eq!(texts(&srv), vec!["Episode 18"]);

        // six batches, six undo steps: back to no titles at all
        let u = super::super::edit::run_undo(&mut srv, &json!({ "steps": 6 }));
        assert!(!u.is_error, "{}", u.text);
        assert!(srv.session.as_ref().unwrap().titles().is_empty());
    }

    #[test]
    fn title_presets_give_the_app_look_and_the_fields_given_win() {
        use engine::project::TitlePreset;
        let mut srv = open_server();
        let fps = srv.session.as_ref().unwrap().meta().frame_rate;
        // A lower third is the app's look: bold, bottom-left, boxed, 45 size, half-second fades.
        let r = titles_ed(
            &mut srv,
            json!([{"op": "title_add", "at": "1s", "duration": "3s", "text": "Jane Doe", "preset": "lower_third"}]),
        );
        assert!(!r.is_error, "{}", r.text);
        {
            let t = &srv.session.as_ref().unwrap().titles()[0];
            assert_eq!(t.text, "Jane Doe");
            assert_eq!((t.start, t.frames), (fps as u32, (3.0 * fps).round() as u32));
            assert_eq!(t.style.size, 45);
            assert_eq!((t.style.place, t.style.hplace, t.style.align, t.style.weight), (Place::Bottom, HPlace::Left, Align::Left, Weight::Bold));
            assert!(t.style.boxed);
            assert_eq!((t.fade_in, t.fade_out), ((0.5 * fps).round() as u32, (0.5 * fps).round() as u32));
        }
        // a field given with the preset wins over the preset's look
        let r = titles_ed(
            &mut srv,
            json!([{"op": "title_add", "at": "6s", "duration": "2s", "text": "Centre", "preset": "centered", "size": 60, "fade_in": 0}]),
        );
        assert!(!r.is_error, "{}", r.text);
        {
            let t = &srv.session.as_ref().unwrap().titles()[1];
            assert_eq!(t.style.size, 60, "the size given wins");
            assert_eq!((t.style.place, t.style.outline, t.style.boxed), (Place::Centre, Outline::Thin, false));
            assert_eq!((t.fade_in, t.fade_out), (0, (0.5 * fps).round() as u32), "fade_in given wins, fade_out keeps the preset's");
        }
        // title_set applies a preset to an existing title: its words and timing stay
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T1", "preset": "caption_bar"}]));
        assert!(!r.is_error, "{}", r.text);
        {
            let t = &srv.session.as_ref().unwrap().titles()[0];
            assert_eq!((t.text.as_str(), t.start), ("Jane Doe", fps as u32));
            assert_eq!((t.style.size, t.style.boxed, t.style.place, t.fade_in), (40, true, Place::Bottom, 0));
        }
        // the engine's own definition agrees with what MCP stored
        let (look, _) = TitlePreset::CaptionBar.look();
        assert_eq!(srv.session.as_ref().unwrap().titles()[0].style, look);
        // an unknown preset is refused by name and adds nothing
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "9s", "duration": "1s", "text": "x", "preset": "fancy"}]));
        assert!(r.is_error && r.text.contains("title preset `fancy` is not lower_third, centered or caption_bar"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().titles().len(), 2);
    }

    #[test]
    fn title_past_the_end_extends_the_timeline() {
        let mut srv = open_server();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let at = format!("{}f", end + 30);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": at, "duration": "2.0s", "text": "The end"}]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let t = s.titles()[0].clone();
        assert_eq!(t.start, end + 30);
        assert_eq!(parse::session_end(s), t.end(), "the timeline runs to the title's last frame");
    }

    #[test]
    fn a_title_warns_about_glyphs_the_font_cannot_draw_and_about_a_long_text() {
        let mut srv = open_server();
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "\u{10FFFD}"}]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("no glyph in the title font") && r.text.contains("draw as boxes"), "{}", r.text);
        assert!(r.text.contains("\u{10FFFD}"), "{}", r.text);
        // In another place, so it does not overlap the first title and earns no overlap warning.
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "Hello", "place": "bottom"}]));
        assert!(!r.text.contains("warning"), "{}", r.text);
        let long = "word ".repeat(100);
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T2", "text": long}]));
        assert!(!r.is_error && r.text.contains("this title is long"), "{}", r.text);
    }

    #[test]
    fn title_remove_says_which_titles_renumber() {
        let mut srv = open_server();
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "One"}, {"op": "title_add", "at": "3", "duration": 2, "text": "Two"}]));
        assert!(!r.is_error, "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_remove", "title": "T1"}]));
        assert!(!r.is_error && r.text.contains("T2 is now T1"), "{}", r.text);
        assert_eq!(texts(&srv), vec!["Two"]);
    }

    #[test]
    fn title_refusals_name_what_to_do() {
        let mut srv = open_server();
        let r = titles_ed(&mut srv, json!([{"op": "title_remove", "title": "T1"}]));
        assert!(r.is_error && r.text.contains("no titles yet"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "   "}]));
        assert!(r.is_error && r.text.contains("needs some text"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "Hi", "size": 500}]));
        assert!(r.is_error && r.text.contains("size"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "Hi", "outline": "wide"}]));
        assert!(r.is_error && r.text.contains("outline"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "Hi", "fade_in": 90}]));
        assert!(r.is_error && r.text.contains("fades"), "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_add", "at": "0", "duration": 2, "text": "Hi"}]));
        assert!(!r.is_error, "{}", r.text);
        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T9", "size": 60}]));
        assert!(r.is_error && r.text.contains("T1..T1"), "{}", r.text);
        // a batch with a refused title rolls back
        let r = titles_ed(
            &mut srv,
            json!([{"op": "title_add", "at": "1s", "duration": 2, "text": "Two"}, {"op": "title_remove", "title": "T7"}]),
        );
        assert!(r.is_error, "{}", r.text);
        assert_eq!(texts(&srv), vec!["Hi"]);
    }

    /// `T@<frame>` names a title by its start frame, the form `timeline` and the title replies
    /// print. The number after `T@` is frames, so `T@1200` on a 30 fps timeline is the title at
    /// 1200 frames (40 s), not at 1200 seconds. Bare numbers are seconds everywhere else.
    #[test]
    fn title_addresses_by_start_frame_resolve_to_the_title_they_name() {
        let mut srv = open_server();
        let r = titles_ed(
            &mut srv,
            json!([
                {"op": "title_add", "at": "30f", "duration": "60f", "text": "early"},
                {"op": "title_add", "at": "1200f", "duration": "60f", "text": "late"}
            ]),
        );
        assert!(!r.is_error, "{}", r.text);

        let r = titles_ed(&mut srv, json!([{"op": "title_set", "title": "T@1200", "size": 60}]));
        assert!(!r.is_error && r.text.contains("size 60"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().titles()[1].style.size, 60);

        let r = titles_ed(&mut srv, json!([{"op": "title_remove", "title": "T@1200"}]));
        assert!(!r.is_error && r.text.contains("removed title T2 \"late\""), "{}", r.text);
        assert_eq!(texts(&srv), vec!["early"]);

        let r = titles_ed(&mut srv, json!([{"op": "title_remove", "title": "T@30"}]));
        assert!(!r.is_error && r.text.contains("removed title T1 \"early\""), "{}", r.text);
        assert!(texts(&srv).is_empty());
    }
}

#[cfg(test)]
mod transform_keys_tests {
    use super::*;
    use serde_json::json;

    /// Envelope times are stored in source frames, so a ramped clip's timeline-to-source
    /// conversion is the ramp's integral and `timeline_of` its inverse.
    #[test]
    fn source_and_timeline_conversions_follow_a_ramp() {
        let mut c = Clip::source_range(0, 10, 400);
        c.start = 100;
        c.speed_keys = vec![
            engine::project::SpeedKey { at: 0, speed: engine::project::Speed::NORMAL },
            engine::project::SpeedKey { at: 60, speed: engine::project::Speed::from_permille(2000) },
        ];
        // 60 frames in the ramp has read 90 source frames.
        assert!((src_of(&c, 160.0) - 100.0).abs() < 1e-9);
        assert_eq!(src_at(&c, 160), 100);
        assert!((timeline_of(&c, 100) - 160.0).abs() < 1e-6);
    }

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    fn run(s: &mut PlaybackSession, op: Value) -> Result<Applied, String> {
        apply(s, &op, 30.0).expect("the op is one of this module's")
    }

    #[test]
    fn keys_sort_on_clip_time_and_hold_the_fields_they_do_not_name() {
        let mut s = session();
        let applied = run(
            &mut s,
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [
                {"at": "end", "scale": 1.2, "ease": "ease"},
                {"at": "0s", "scale": 1.0, "x": 0.1},
                {"at": "50%", "rotate": 10}
            ]}),
        )
        .expect("keys applied");
        let keys = s.transform_keys_of(Lane::V1, 0).to_vec();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys.iter().map(|k| k.at).collect::<Vec<_>>(), vec![0, 75, 150], "0s, 50%, end");
        assert_eq!(keys[1].params.scale, 1.0, "scale held from the 0s key");
        assert_eq!(keys[1].params.pos_x, 0.1, "x held from the 0s key");
        assert_eq!(keys[1].params.rotate, 10.0);
        assert_eq!(keys[2].params.scale, 1.2);
        assert_eq!(keys[2].params.rotate, 10.0, "rotate held from the 50% key");
        assert_eq!(keys[2].ease, engine::transform::KeyEase::Ease);
        assert_eq!(keys[0].ease, engine::transform::KeyEase::Linear, "default ease");
        assert!(applied.text.contains("3 key(s)"), "{}", applied.text);
        assert!(applied.text.contains("0.000s") && applied.text.contains("5.000s"), "timeline times: {}", applied.text);
    }

    #[test]
    fn a_keyed_clip_refuses_transform_until_its_keys_are_cleared() {
        let mut s = session();
        run(
            &mut s,
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s", "scale": 1.0}, {"at": "end", "scale": 1.2}]}),
        )
        .expect("keys");
        let err = run(&mut s, json!({"op": "transform", "clip": "V1#0", "x": 0.2})).expect_err("refused");
        assert!(err.contains("transform key"), "{err}");
        run(&mut s, json!({"op": "transform_keys", "clip": "V1#0", "clear": true})).expect("clear");
        assert!(s.transform_keys_of(Lane::V1, 0).is_empty());
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "x": 0.2})).expect("plain again");
    }

    #[test]
    fn two_keys_at_one_time_say_so_at_normal_speed() {
        let mut s = session();
        let err = run(
            &mut s,
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "1s"}, {"at": "1s", "scale": 1.2}]}),
        )
        .expect_err("two keys at 1 s");
        assert!(err.contains("two keys at the same time (1.000s)"), "{err}");
        assert!(!err.contains("speed"), "no speed talk at 1x: {err}");
    }

    #[test]
    fn a_key_past_the_clip_names_its_place_in_the_list() {
        let mut s = session();
        let err = run(
            &mut s,
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s"}, {"at": "9s"}]}),
        )
        .expect_err("9 s past a 5 s clip");
        assert!(err.contains("the 2nd key, at 9.000s, is past the clip"), "{err}");
    }

    #[test]
    fn ordinals_read_as_english_for_any_list_position() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(111), "111th");
        assert_eq!(ordinal(242), "242nd");
    }

    #[test]
    fn keys_are_refused_on_audio_past_the_clip_and_with_bad_fields() {
        let mut s = session();
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "A1#0", "keys": [{"at": "0s"}]})).is_err());
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "9s"}]})).is_err(), "past a 5 s clip");
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s", "spin": 1}]})).is_err());
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"scale": 1}]})).is_err(), "no at");
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "V1#0", "keys": []})).is_err(), "empty list");
        assert!(run(&mut s, json!({"op": "transform_keys", "clip": "V1#0"})).is_err(), "no keys at all");
    }

    #[test]
    fn every_ease_name_is_taken_and_a_bad_one_lists_them() {
        use engine::transform::KeyEase;
        let mut s = session();
        for (name, want) in [
            ("linear", KeyEase::Linear),
            ("ease_in", KeyEase::EaseIn),
            ("ease_out", KeyEase::EaseOut),
            ("ease_in_out", KeyEase::Ease),
            ("ease", KeyEase::Ease),
            ("hold", KeyEase::Hold),
        ] {
            run(
                &mut s,
                json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s", "ease": name}, {"at": "end", "scale": 1.2}]}),
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(s.transform_keys_of(Lane::V1, 0)[0].ease, want, "{name}");
        }
        let err = run(
            &mut s,
            json!({"op": "transform_keys", "clip": "V1#0", "keys": [{"at": "0s", "ease": "bounce"}]}),
        )
        .expect_err("bad ease");
        assert!(err.contains("ease_in") && err.contains("hold") && err.contains("\"bounce\""), "{err}");
    }

    #[test]
    fn opacity_keys_set_clear_refuse_and_hold_what_a_key_leaves_out() {
        use engine::transform::KeyEase;
        let mut s = session();
        let reply = run(
            &mut s,
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": [
                {"at": "0s", "opacity": 0, "ease": "ease_in_out"}, {"at": "2s"}, {"at": "end", "opacity": 1}]}),
        )
        .expect("opacity_keys");
        assert!(reply.text.contains("3 key(s)") && reply.text.contains("ease_in_out"), "{}", reply.text);
        let keys = s.opacity_keys_of(Lane::V1, 0).to_vec();
        assert_eq!(keys.len(), 3);
        assert_eq!((keys[0].opacity, keys[1].opacity, keys[2].opacity), (0.0, 0.0, 1.0), "a missing opacity holds");
        assert_eq!(keys[0].ease, KeyEase::Ease);
        let again = run(
            &mut s,
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": [
                {"at": "0s", "opacity": 0, "ease": "ease_in_out"}, {"at": "2s"}, {"at": "end", "opacity": 1}]}),
        )
        .expect("repeat");
        assert!(again.text.contains("already"), "{}", again.text);
        // The constant opacity is refused while keys give it.
        let err = run(&mut s, json!({"op": "transform", "clip": "V1#0", "opacity": 0.5})).expect_err("keyed");
        assert!(err.contains("opacity key"), "{err}");
        run(&mut s, json!({"op": "transform", "clip": "V1#0", "x": 0.1})).expect("other fields are fine");
        // Refusals.
        for bad in [
            json!({"op": "opacity_keys", "clip": "A1#0", "keys": [{"at": "0s", "opacity": 1}]}),
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": [{"at": "0s", "opacity": 1.5}]}),
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": [{"at": "0s", "scale": 1}]}),
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": []}),
            json!({"op": "opacity_keys", "clip": "V1#0"}),
            json!({"op": "opacity_keys", "clip": "V1#0", "keys": [{"at": "1s"}, {"at": "1s"}]}),
        ] {
            assert!(run(&mut s, bad.clone()).is_err(), "{bad}");
        }
        run(&mut s, json!({"op": "opacity_keys", "clip": "V1#0", "clear": true})).expect("clear");
        assert!(s.opacity_keys_of(Lane::V1, 0).is_empty());
        assert!(s.undo(), "the clear is one undo step");
        assert_eq!(s.opacity_keys_of(Lane::V1, 0).len(), 3);
    }

    #[test]
    fn color_keys_start_from_the_clips_grade_and_refuse_the_constant_color() {
        let mut s = session();
        run(&mut s, json!({"op": "color", "clip": "V1#0", "contrast": 1.5})).expect("a grade");
        run(
            &mut s,
            json!({"op": "color_keys", "clip": "V1#0", "keys": [
                {"at": "0s", "brightness": -0.5}, {"at": "end", "brightness": 0.5, "tint": 0.25, "ease": "hold"}]}),
        )
        .expect("color_keys");
        let keys = s.color_keys_of(Lane::V1, 0).to_vec();
        assert_eq!(keys[0].params.contrast, 1.5, "the first key starts from the grade");
        assert_eq!(keys[1].params.contrast, 1.5, "a key holds what it leaves out");
        assert_eq!(keys[1].params.tint, 0.25);
        assert_eq!(keys[0].ease, engine::transform::KeyEase::Linear);
        let err = run(&mut s, json!({"op": "color", "clip": "V1#0", "brightness": 0.1})).expect_err("keyed");
        assert!(err.contains("colour key") && err.contains("color_keys"), "{err}");
        assert!(run(&mut s, json!({"op": "color_keys", "clip": "V1#0", "keys": [{"at": "0s", "contrast": 3}]})).is_err());
        assert!(run(&mut s, json!({"op": "color_keys", "clip": "A1#0", "keys": [{"at": "0s", "tint": 0.1}]})).is_err());
        run(&mut s, json!({"op": "color_keys", "clip": "V1#0", "clear": true})).expect("clear");
        run(&mut s, json!({"op": "color", "clip": "V1#0", "brightness": 0.1})).expect("plain again");
    }

    #[test]
    fn gain_points_take_an_ease() {
        use engine::transform::KeyEase;
        let mut s = session();
        run(
            &mut s,
            json!({"op": "gain_keys", "clip": "A1#0", "keys": [["0s", 0, "ease_out"], ["2s", -12], ["3s", -6, "hold"]]}),
        )
        .expect("gain_keys with eases");
        let g = s.gain_keys_of(Lane::A1, 0).to_vec();
        assert_eq!((g[0].ease, g[1].ease, g[2].ease), (KeyEase::EaseOut, KeyEase::Linear, KeyEase::Hold));
        run(&mut s, json!({"op": "gain_key", "clip": "A1#0", "at": "1s", "db": -3, "ease": "ease_in"})).expect("gain_key");
        assert!(s.gain_keys_of(Lane::A1, 0).iter().any(|k| k.ease == KeyEase::EaseIn));
        assert!(run(&mut s, json!({"op": "gain_keys", "clip": "A1#0", "keys": [["0s", 0, "wobble"]]})).is_err());
    }
}

#[cfg(test)]
mod look_ops_tests {
    use super::*;
    use serde_json::json;

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    fn run(s: &mut PlaybackSession, op: Value) -> Result<Applied, String> {
        apply(s, &op, 30.0).expect("the op is one of this module's")
    }

    fn cube(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("edith-mcp-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let p = dir.join(name);
        std::fs::write(&p, text).expect("write");
        p
    }

    const WARM: &str = "LUT_3D_SIZE 2\n0.05 0 0\n1 0 0\n0.05 1 0\n1 1 0\n0.05 0 0.85\n1 0 0.85\n0.05 1 0.85\n1 1 0.85\n";

    #[test]
    fn lut_sets_mixes_clears_and_refuses_a_broken_file_by_name() {
        let mut s = session();
        let warm = cube("warm.cube", WARM);
        let r = run(&mut s, json!({"op": "lut", "clip": "V1#0", "path": warm.display().to_string(), "mix": 0.8})).expect("set");
        assert!(r.text.contains("LUT warm.cube, 3D 2 at 80%"), "{}", r.text);
        let c = *s.color_of(Lane::V1, 0).expect("graded");
        assert_eq!(c.look.lut.map(|l| l.path()), Some(warm.clone()));
        assert_eq!(c.look.lut_mix, 0.8);
        run(&mut s, json!({"op": "lut", "clip": "V1#0", "mix": 0.5})).expect("mix only");
        assert_eq!(s.color_of(Lane::V1, 0).expect("graded").look.lut_mix, 0.5);
        let bad = cube("bad.cube", "LUT_3D_SIZE 2\n0 0 0\n");
        let e = run(&mut s, json!({"op": "lut", "clip": "V1#0", "path": bad.display().to_string()})).unwrap_err();
        assert!(e.contains("cannot use that LUT") && e.contains("entries"), "{e}");
        let e = run(&mut s, json!({"op": "lut", "clip": "V1#0", "path": "/nonexistent/x.cube"})).unwrap_err();
        assert!(e.contains("cannot read"), "{e}");
        assert!(run(&mut s, json!({"op": "lut", "clip": "V1#0", "mix": 2})).is_err());
        assert!(run(&mut s, json!({"op": "lut", "clip": "A1#0", "path": warm.display().to_string()})).is_err(), "picture only");
        // Relink: every clip naming the file moves to the new one.
        let other = cube("other.cube", WARM);
        let r = run(&mut s, json!({"op": "lut", "relink": warm.display().to_string(), "path": other.display().to_string()})).expect("relink");
        assert!(r.text.contains("1 clip(s)"), "{}", r.text);
        assert_eq!(s.color_of(Lane::V1, 0).and_then(|c| c.look.lut).map(|l| l.path()), Some(other.clone()));
        assert!(run(&mut s, json!({"op": "lut", "relink": "/no/such.cube", "path": other.display().to_string()})).unwrap_err().contains("no clip names"));
        run(&mut s, json!({"op": "lut", "clip": "V1#0", "clear": true})).expect("clear");
        assert!(s.color_of(Lane::V1, 0).is_none(), "nothing left: no grade");
    }

    #[test]
    fn wheels_and_curves_merge_onto_the_grade() {
        let mut s = session();
        run(&mut s, json!({"op": "color", "clip": "V1#0", "contrast": 1.2})).expect("grade");
        let r = run(&mut s, json!({"op": "wheels", "clip": "V1#0", "lift": [0, 0.1, 0, -0.05], "gain": 0.2})).expect("wheels");
        assert!(r.text.contains("lift m+0.00 r+0.10 g+0.00 b-0.05") && r.text.contains("gain m+0.20"), "{}", r.text);
        run(&mut s, json!({"op": "curves", "clip": "V1#0", "master": [[0.25, 0.2], [0.75, 0.8]], "blue": [[0.5, 0.45]]})).expect("curves");
        let c = *s.color_of(Lane::V1, 0).expect("graded");
        assert_eq!(c.contrast, 1.2, "the sliders stay");
        assert_eq!(c.look.wheels.lift, [0.0, 0.1, 0.0, -0.05]);
        assert_eq!(c.look.wheels.gain, [0.2, 0.0, 0.0, 0.0]);
        assert_eq!(c.look.curves[0].points(), &[[0.25, 0.2], [0.75, 0.8]]);
        assert_eq!(c.look.curves[3].points(), &[[0.5, 0.45]]);
        assert!(run(&mut s, json!({"op": "wheels", "clip": "V1#0", "lift": [2, 0, 0, 0]})).is_err());
        assert!(run(&mut s, json!({"op": "wheels", "clip": "V1#0", "lift": [0, 0]})).is_err());
        assert!(run(&mut s, json!({"op": "curves", "clip": "V1#0", "red": [[0.5, 0.2], [0.4, 0.3]]})).unwrap_err().contains("strictly increase"));
        assert!(run(&mut s, json!({"op": "curves", "clip": "V1#0"})).is_err());
        run(&mut s, json!({"op": "curves", "clip": "V1#0", "reset": true})).expect("straight");
        run(&mut s, json!({"op": "wheels", "clip": "V1#0", "reset": true})).expect("neutral");
        assert_eq!(s.color_of(Lane::V1, 0).expect("graded").look, engine::color::Look::default());
    }

    #[test]
    fn look_applies_a_builtin_in_one_step_and_names_the_choices_when_unknown() {
        let mut s = session();
        run(&mut s, json!({"op": "color", "clip": "V1#0", "contrast": 1.2})).expect("grade");
        let r = run(&mut s, json!({"op": "look", "clip": "V1#0", "name": "warm"})).expect("look");
        assert_eq!(r.text, "look V1#0 → warm");
        let c = *s.color_of(Lane::V1, 0).expect("graded");
        assert_eq!(c, engine::looks::builtin("Warm").expect("built-in"), "the look replaces the grade");
        let e = run(&mut s, json!({"op": "look", "clip": "V1#0", "name": "Nope"})).unwrap_err();
        assert!(e.contains("Looks: Warm, Cool") && e.contains("Vintage"), "{e}");
        run(&mut s, json!({"op": "look", "clip": "V1#0", "name": "Black & white"})).expect("bw");
        assert_eq!(s.color_of(Lane::V1, 0).expect("graded").saturation, 0.0);
    }

    #[test]
    fn auto_color_writes_a_correction_and_reports_it() {
        let mut s = session();
        let r = run(&mut s, json!({"op": "auto_color", "clip": "V1#0"})).expect("auto");
        assert!(r.text.starts_with("auto_color V1#0 → wheels gain red"), "{}", r.text);
        let c = *s.color_of(Lane::V1, 0).copied().as_ref().unwrap_or(&engine::color::ColorParams::default());
        assert_eq!(c.saturation, 1.0, "saturation is kept");
        assert!(run(&mut s, json!({"op": "color", "clip": "V1#0", "brightness": 0.1})).is_ok());
        assert!(run(&mut s, json!({"op": "auto_color", "clip": "A1#0"})).is_err(), "a sound clip has no picture");
        assert_eq!(super::fit_to(1920, 1080, 640), (640, 360));
    }
}

#[cfg(test)]
mod pitch_op_tests {
    use super::*;
    use serde_json::json;

    fn session() -> PlaybackSession {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        PlaybackSession::open(p).expect("open test asset")
    }

    fn run(s: &mut PlaybackSession, op: Value) -> Result<Applied, String> {
        apply(s, &op, 30.0).expect("the op is one of this module's")
    }

    /// `pitch` sets whole semitones on an audio clip, keeps its length, undoes in one step,
    /// and refuses a fraction, an out-of-range value, a missing value and a video clip.
    #[test]
    fn pitch_sets_whole_semitones_keeps_length_and_refuses_bad_input() {
        let mut s = session();
        let len = s.lane_clips(Lane::A1)[0].len();
        run(&mut s, json!({"op": "pitch", "clip": "A1#0", "semitones": 12})).expect("pitch up an octave");
        assert_eq!(s.clip_pitch_of(Lane::A1, 0), Some(12));
        assert_eq!(s.lane_clips(Lane::A1)[0].len(), len, "pitch keeps the clip's length");
        assert!(s.undo(), "pitch is one undo step");
        assert_eq!(s.clip_pitch_of(Lane::A1, 0), Some(0));
        assert!(run(&mut s, json!({"op": "pitch", "clip": "A1#0", "semitones": 1.5})).is_err());
        assert!(run(&mut s, json!({"op": "pitch", "clip": "A1#0", "semitones": 13})).is_err());
        assert!(run(&mut s, json!({"op": "pitch", "clip": "A1#0"})).is_err());
        assert!(run(&mut s, json!({"op": "pitch", "clip": "V1#0", "semitones": 2})).is_err());
        assert_eq!(s.clip_pitch_of(Lane::A1, 0), Some(0), "refusals change nothing");
    }
}
