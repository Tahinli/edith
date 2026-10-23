//! Scan tool: shot boundaries of a source, found by `engine::scenes` (MCP.md section 4.4).
//!
//! The result is in the source's own frames, shaped the way `edit` `keep` takes
//! ranges, so a scan can be handed straight back as a rough cut. Results are
//! cached per file, modification time and settings under the user's cache
//! directory, so a second scan of the same file is a file read.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use engine::decode::{to_png_bytes, DecodeSession, Frame};
use engine::demux::Demuxer;
use engine::project::{Clip, Rate};
use engine::scenes::{self, Cut, CutKind, SceneSettings};
use engine::PlaybackSession;

use crate::parse;
use crate::server::{opt_str, tri, Reply, Server, Tool};
use crate::tools::see::{base64, fit_height, frames_dir, picture_check, resize, save_png, tile_sheet};

/// Tiles in a scan sheet, at most. More shots than this and the sheet is refused.
const MAX_SHEET_TILES: usize = 96;
const SHEET_COLS: usize = 4;
const TILE_W: u32 = 320;

const SCENES_DOC: &str = "Find the shots of a source file: where the picture cuts, and where it \
fades through black. Returns the shots in SOURCE frames as {from, to} pairs in the form keep \
takes, so the result can go straight into edit keep. A clip address (such as V1@450) scans that \
clip's own source, limited to the clip's span, and adds each shot's timeline time. threshold \
(0-1, default 0.35) sets how big a change must be; min_shot (seconds, default 0.6) merges closer \
cuts. Each boundary is the first frame of the new shot, refined to the exact frame (in a range too); a fade through black is placed at the first picture frame after the dark run. A source with no picture (sound alone) is refused. sheet:true adds one contact-sheet image with each shot's middle frame, saved under the project's frame cache folder (~/.cache/edith/frames/<project>/) and named in the reply. Results are cached \
per file.";

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "scenes", description: SCENES_DOC, schema: scenes_schema, run: scenes }]
}

fn scenes_schema() -> Value {
    json!({
        "type": "object",
        "description": SCENES_DOC,
        "properties": {
            "source": {
                "type": "string",
                "description": "A source, such as src:2, or a clip address such as V1@450 to scan that clip's source within its span."
            },
            "range": {
                "type": ["array", "object"],
                "minItems": 2,
                "maxItems": 2,
                "items": {"type": ["string", "number"]},
                "properties": {
                    "from": {"type": ["string", "number"]},
                    "to": {"type": ["string", "number"]}
                },
                "description": "Optional [from, to] or {from, to} in the source's time: 450f, 15.0s, or a number of seconds. To is exclusive. Intersected with a clip's span."
            },
            "threshold": {
                "type": "number",
                "minimum": 0,
                "maximum": 1,
                "description": "Cut sensitivity from 0 to 1. Default 0.35; lower finds more cuts."
            },
            "min_shot": {
                "type": "number",
                "minimum": 0,
                "description": "Shortest shot in seconds; closer cuts merge into the stronger one. Default 0.6."
            },
            "sheet": {
                "type": "boolean",
                "description": "Also return one contact-sheet image with each shot's middle frame as a tile."
            }
        },
        "required": ["source"]
    })
}

/// What a scan reads: one source, and the clip it was addressed through, if any.
struct Target {
    src: usize,
    clip: Option<Clip>,
    /// The file's frames against the timeline's: how a clip's frame range, which
    /// counts at the timeline's rate, becomes the file's own frames.
    rate: Rate,
    /// The timeline's own frame rate, for the clock of a timeline time.
    timeline_fps: f64,
}

fn scenes(server: &mut Server, args: &Value) -> Reply {
    let s = tri!(server.session_ref());
    let Some(raw) = opt_str(args, "source") else {
        return Reply::err("missing argument `source`: give src:<n>, such as src:0, or a clip address such as V1@450");
    };
    let mut target = tri!(target_of(s, raw));
    let path = s.sources()[target.src].path.clone();
    let name = file_name(&path);
    if engine::is_image(&path) {
        return Reply::err(format!("src:{} is a still image; scenes needs a moving picture", target.src));
    }
    if let Err(e) = picture_check(&path, target.src) {
        return Reply::err(format!("{e}; scenes needs a moving picture"));
    }
    // The file's own frame count and rate: the source's frames, not the timeline's
    // (`PlaybackSession::file_frames` counts in timeline frames).
    let (meta, _demux) = match Demuxer::open(&path) {
        Ok(v) => v,
        Err(e) => return Reply::err(format!("cannot count the frames of src:{} {name}: {e}", target.src)),
    };
    let total = meta.frame_count;
    if total == 0 {
        return Reply::err(format!("cannot count the frames of src:{} {name}", target.src));
    }
    let fps = Some(meta.frame_rate)
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or_else(|| parse::session_fps(s));
    target.rate = match Rate::from_fps(fps, target.timeline_fps) {
        Ok(r) => r,
        Err(e) => return Reply::err(format!("cannot relate src:{} to the timeline's rate: {e}", target.src)),
    };
    let (from, to) = tri!(span_of(args, &target, fps, total));

    let mut settings = SceneSettings::for_duration(f64::from(total) / fps);
    if let Some(t) = args.get("threshold").filter(|v| !v.is_null()) {
        let Some(t) = t.as_f64() else {
            return Reply::err("argument `threshold` must be a number from 0 to 1");
        };
        settings.threshold = t as f32;
    }
    if let Some(m) = args.get("min_shot").filter(|v| !v.is_null()) {
        let Some(m) = m.as_f64() else {
            return Reply::err("argument `min_shot` must be a number of seconds");
        };
        settings.min_shot_secs = m;
    }
    let want_sheet = args.get("sheet").and_then(Value::as_bool).unwrap_or(false);
    let sheet_dir = frames_dir(server.path.as_deref());

    let Scan { cuts, flashes, cached } = match cuts_for(cache_dir().as_deref(), &path, (from, to), &settings) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let flashes: Vec<(u32, u32)> = flashes.into_iter().filter(|&(a, b)| a < to && b > from).collect();
    let shots = shots_of(from, to, &cuts);
    if want_sheet && shots.len() > MAX_SHEET_TILES {
        return Reply::err(format!(
            "{} shots is more than one sheet holds ({MAX_SHEET_TILES}); raise min_shot or narrow range",
            shots.len()
        ));
    }

    let sheet = if want_sheet {
        match sheet_of(&path, &shots, fps, &sheet_dir) {
            Ok(v) => Some(v),
            Err(r) => return r,
        }
    } else {
        None
    };

    let lines = describe(&target, &name, &shots, &cuts, &flashes, from, to, fps);
    let mut text = lines.join("\n");
    let mut structured = structured_of(&target, &name, &shots, &cuts, &flashes, from, to, fps, cached);
    if let Some((png, saved)) = &sheet {
        text.push_str(&format!("\ncontact sheet: {} tiles, saved: {}", shots.len(), saved.display()));
        structured["sheet"] = json!({"tiles": shots.len(), "path": saved.display().to_string()});
        return Reply::ok(text).with(structured).image("image/png", base64(png));
    }
    Reply::ok(text).with(structured)
}

/// The source a target names, and the clip if it was a clip address.
fn target_of(s: &PlaybackSession, raw: &str) -> Result<Target, Reply> {
    let head = raw.split_whitespace().next().unwrap_or("");
    if let Some(d) = head.strip_prefix("src:") {
        let src = d
            .parse::<usize>()
            .map_err(|_| Reply::err(format!("`{raw}` is not src:<n> or a clip address")))?;
        if src >= s.sources().len() {
            return Err(Reply::err(format!(
                "no source src:{src}; the project has {} source(s)",
                s.sources().len()
            )));
        }
        return Ok(Target { src, clip: None, rate: Rate::REAL_TIME, timeline_fps: parse::session_fps(s) });
    }
    let (lane, idx) = parse::clip(s, raw).map_err(Reply::err)?;
    let clip = s
        .lane_clips(lane)
        .get(idx)
        .cloned()
        .ok_or_else(|| Reply::err(format!("no clip at `{raw}`")))?;
    Ok(Target { src: clip.source, clip: Some(clip), rate: Rate::REAL_TIME, timeline_fps: parse::session_fps(s) })
}

/// The `[from, to)` span to scan, in source frames.
fn span_of(args: &Value, target: &Target, fps: f64, total: u32) -> Result<(u32, u32), Reply> {
    let (mut from, mut to) = (0, total);
    if let Some(r) = args.get("range").filter(|v| !v.is_null()) {
        // `[from, to]`, or `{from, to}` as the other tools take it; a missing end of the object is the source's.
        let (a, b) = match r {
            Value::Array(pair) if pair.len() == 2 => (pair[0].clone(), pair[1].clone()),
            Value::Object(o) => (
                o.get("from").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!("0f")),
                o.get("to").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!(format!("{total}f"))),
            ),
            _ => return Err(Reply::err("argument `range` is [from, to] or {from, to}, two times in the source")),
        };
        from = parse::frame(&a, fps, total).map_err(Reply::err)?;
        to = parse::frame(&b, fps, total).map_err(Reply::err)?;
    }
    if let Some(c) = &target.clip {
        // The clip's range counts at the timeline's rate: its first and last frames
        // as the file's own frames, the last one exclusive.
        from = from.max(target.rate.source_at(c.in_frame));
        to = to.min(target.rate.source_at(c.out_frame.saturating_sub(1)) + 1);
    }
    to = to.min(total);
    if from >= to {
        return Err(Reply::err(format!("the range is empty: {from}f to {to}f")));
    }
    Ok((from, to))
}

/// Shots of `[from, to)`: the spans between cuts, as `(from, to)` pairs.
fn shots_of(from: u32, to: u32, cuts: &[Cut]) -> Vec<(u32, u32)> {
    // A dissolve ends the old shot at its first frame and starts the new one at its last, so the
    // frames of the dissolve itself belong to neither shot.
    let mut shots = Vec::new();
    let mut start = from;
    for c in inner(from, to, cuts) {
        shots.push((start, c.frame.max(start)));
        start = match c.kind {
            CutKind::Dissolve { end } => end.min(to),
            _ => c.frame,
        };
    }
    shots.push((start, to));
    shots
}

/// The name of a cut's kind in the cache file and the JSON answer.
fn kind_name(kind: CutKind) -> &'static str {
    match kind {
        CutKind::Hard => "hard",
        CutKind::FadeBlack => "fade",
        CutKind::Dissolve { .. } => "dissolve",
    }
}

/// The cuts that fall strictly inside `(from, to)`.
fn inner(from: u32, to: u32, cuts: &[Cut]) -> Vec<&Cut> {
    cuts.iter().filter(|c| c.frame > from && c.frame < to).collect()
}

/// What a scan found: the shot boundaries, and the flashes that were dropped from them
/// (each `[from, to)` in source frames, a run of frames that is not a shot of its own).
struct Scan {
    cuts: Vec<Cut>,
    flashes: Vec<(u32, u32)>,
    cached: bool,
}

/// Cuts from the cache when the same file and settings were scanned before,
/// from the detector otherwise. `cached` says whether it was a cache hit.
fn cuts_for(dir: Option<&Path>, path: &Path, range: (u32, u32), settings: &SceneSettings) -> Result<Scan, String> {
    let file = dir.map(|d| d.join(format!("{}.json", cache_key(path, range, settings))));
    if let Some(f) = &file
        && let Some((cuts, flashes)) = read_cache(f)
    {
        return Ok(Scan { cuts, flashes, cached: true });
    }
    let mut cuts = scenes::detect(path, Some(range), settings, None, None)
        .map_err(|e| format!("scene detection failed on {}: {e}", file_name(path)))?;
    verify_fades(path, &mut cuts);
    let flashes = find_flashes(path, &mut cuts);
    if let Some(f) = &file {
        write_cache(f, &cuts, &flashes);
    }
    Ok(Scan { cuts, flashes, cached: false })
}

/// Largest mean luma difference (over 255) a frame may have from the picture before a flash and
/// still be that picture again.
const FLASH_SAME: f64 = 0.02;
/// Smallest mean luma difference (over 255) a flash frame must have from the picture before it.
const FLASH_DIFF: f64 = 0.1;
/// Longest flash, in frames. A 3-frame white flash is the case this is sized for.
const FLASH_MAX: u32 = 4;

/// The flash the boundary at `cut` belongs to, as `[from, to)`: one to [`FLASH_MAX`] frames that
/// differ strongly from the picture before them, followed by that picture again. `cut` is either
/// the first frame after the flash (the flash's end is the boundary) or its first frame. `dist(a, b)`
/// is the mean absolute luma difference of frames `a` and `b`, over 255; NaN when unknown.
fn flash_span(cut: u32, dist: &impl Fn(u32, u32) -> f64) -> Option<(u32, u32)> {
    for n in 1..=FLASH_MAX {
        let Some(before) = cut.checked_sub(n + 1) else { break };
        if dist(before, cut) <= FLASH_SAME && (cut - n..cut).all(|k| dist(before, k) >= FLASH_DIFF) {
            return Some((cut - n, cut));
        }
    }
    let before = cut.checked_sub(1)?;
    for n in 1..=FLASH_MAX {
        let after = cut + n;
        if dist(before, after) <= FLASH_SAME && (cut..after).all(|k| dist(before, k) >= FLASH_DIFF) {
            return Some((cut, after));
        }
    }
    None
}

/// Takes the flashes out of the hard cuts: a boundary that is only a flash (the picture leaves
/// and comes straight back) is not a shot change, so it is dropped and reported as a flash.
fn find_flashes(path: &Path, cuts: &mut Vec<Cut>) -> Vec<(u32, u32)> {
    let total = Demuxer::open(path).map_or(0, |(m, _)| m.frame_count);
    let mut flashes = Vec::new();
    let mut keep = Vec::with_capacity(cuts.len());
    for c in cuts.drain(..) {
        if c.kind != CutKind::Hard {
            keep.push(c);
            continue;
        }
        let lo = c.frame.saturating_sub(FLASH_MAX + 1);
        let hi = c.frame.saturating_add(FLASH_MAX).min(total.saturating_sub(1));
        let window = frames_between(path, lo, hi).unwrap_or_default();
        let dist = |a: u32, b: u32| -> f64 {
            match (window.iter().find(|f| f.0 == a), window.iter().find(|f| f.0 == b)) {
                (Some((_, fa)), Some((_, fb))) => luma_gap(fa, fb),
                _ => f64::NAN,
            }
        };
        match flash_span(c.frame, &dist) {
            Some(span) => flashes.push(span),
            None => keep.push(c),
        }
    }
    *cuts = keep;
    flashes
}

/// Mean absolute luma difference of two frames, over 255. NaN when their sizes differ.
fn luma_gap(a: &Frame, b: &Frame) -> f64 {
    if a.bgra.len() != b.bgra.len() || a.bgra.is_empty() {
        return f64::NAN;
    }
    let luma = |p: &[u8]| ((77 * i32::from(p[2]) + 150 * i32::from(p[1]) + 29 * i32::from(p[0])) >> 8).clamp(0, 255);
    let total: i64 = a
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.bgra.as_chunks::<4>().0)
        .map(|(p, q)| i64::from((luma(p) - luma(q)).abs()))
        .sum();
    total as f64 / (a.bgra.len() / 4) as f64 / 255.0
}

/// Share of the frame that must be near-black (luma 16 or below) for a dark run to be a fade.
/// Grain around a mean of 18 has a dip below 16 now and then, which is a steady dark shot, not
/// a fade: a fade's dark end is black nearly all over.
const FADE_BLACK_SHARE: f64 = 0.8;
/// How much brighter, in luma levels, the picture a second either side of a fade must be than its
/// dark end. A dark shot that stays dark on both sides has no dip, so it is not a fade.
const FADE_DIP: f64 = 32.0;

/// Keeps a fade through black only when it is one: the frame before the cut is near-black on
/// most of the frame, and the picture a second before and after (where the file has them) is
/// clearly brighter. The engine finds a dark run by mean luma alone, which a steady dark shot
/// can satisfy; this looks at the pixels, so it is checked here on the decoded frames.
fn verify_fades(path: &Path, cuts: &mut Vec<Cut>) {
    if !cuts.iter().any(|c| c.kind == CutKind::FadeBlack) {
        return;
    }
    let Ok((meta, _demux)) = Demuxer::open(path) else {
        cuts.retain(|c| c.kind != CutKind::FadeBlack);
        return;
    };
    let fps = Some(meta.frame_rate).filter(|f| f.is_finite() && *f > 0.0).unwrap_or(25.0);
    let win = fps.round().max(1.0) as u32;
    cuts.retain(|c| c.kind != CutKind::FadeBlack || real_fade(path, c.frame, win, meta.frame_count));
    drop_cuts_in_fades(cuts, win);
}

/// Drops the hard cuts that fall inside a fade in: within `win` frames after a fade's first picture
/// frame. A fade that brightens in steps is many small changes, and the detector reads one of them
/// as a cut; the fade is one shot boundary, so none of its steps is.
fn drop_cuts_in_fades(cuts: &mut Vec<Cut>, win: u32) {
    let fades: Vec<u32> = cuts.iter().filter(|c| c.kind == CutKind::FadeBlack).map(|c| c.frame).collect();
    cuts.retain(|c| c.kind == CutKind::FadeBlack || !fades.iter().any(|&f| c.frame > f && c.frame <= f + win));
}

/// Whether the fade at `cut` (the first picture frame after its dark run) is a real one; see
/// [`verify_fades`]. `win` is one second in frames.
///
/// The darkest frame before `cut` is the fade's black end. It must be near-black, and the run of
/// near-black frames around it is the dark run. The picture a second before that run, and a second
/// after it, must each be [`FADE_DIP`] levels brighter than the black, wherever the file has them.
fn real_fade(path: &Path, cut: u32, win: u32, total: u32) -> bool {
    let lo = cut.saturating_sub(4 * win);
    let hi = cut.saturating_add(2 * win).min(total.saturating_sub(1));
    let Some(frames) = frames_between(path, lo, hi) else { return false };
    let stats: Vec<(u32, f64, f64)> = frames
        .iter()
        .map(|(i, f)| {
            let (mean, share) = luma_stats(f);
            (*i, mean, share)
        })
        .collect();
    let at = |i: u32| stats.iter().find(|s| s.0 == i).copied();
    // The first darkest frame before the cut; the cut itself is the first picture frame.
    let Some(&(black, black_mean, black_share)) =
        stats.iter().filter(|s| s.0 < cut).min_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return false;
    };
    if black_share < FADE_BLACK_SHARE {
        return false;
    }
    let mut run_lo = black;
    while run_lo > lo && at(run_lo - 1).is_some_and(|s| s.2 >= FADE_BLACK_SHARE) {
        run_lo -= 1;
    }
    let mut run_hi = black;
    while run_hi < hi && at(run_hi + 1).is_some_and(|s| s.2 >= FADE_BLACK_SHARE) {
        run_hi += 1;
    }
    let before = (run_lo.saturating_sub(win)..run_lo).filter_map(at).map(|s| s.1).reduce(f64::max);
    let after = (run_hi + 1..=run_hi.saturating_add(win)).filter_map(at).map(|s| s.1).reduce(f64::max);
    let sides: Vec<f64> = [before, after].into_iter().flatten().collect();
    !sides.is_empty() && sides.iter().all(|&mean| mean >= black_mean + FADE_DIP)
}

/// Mean luma (of 255) of a frame, and the share of its pixels at luma 16 or below. Luma is
/// the same weighting the engine's scene thumbnails use.
fn luma_stats(f: &Frame) -> (f64, f64) {
    let (mut sum, mut dark, mut n) = (0u64, 0u64, 0u64);
    for p in f.bgra.as_chunks::<4>().0 {
        let (b, g, r) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
        let y = ((77 * r + 150 * g + 29 * b) >> 8).clamp(0, 255) as u64;
        sum += y;
        dark += u64::from(y <= 16);
        n += 1;
    }
    let n = n.max(1) as f64;
    (sum as f64 / n, dark as f64 / n)
}

/// The frames `lo..=hi` of the source, by index. Empty when the decoder gives none.
fn frames_between(path: &Path, lo: u32, hi: u32) -> Option<Vec<(u32, Frame)>> {
    let (_, rx, cancel) = DecodeSession::open_range(path, lo, hi.saturating_add(1)).ok()?;
    let out: Vec<(u32, Frame)> = rx.iter().take_while(|f| f.index <= hi).map(|f| (f.index, f)).collect();
    cancel.store(true, Ordering::Relaxed);
    Some(out)
}

/// The cache directory: `$XDG_CACHE_HOME/edith/scenes`, else `~/.cache/edith/scenes`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("edith").join("scenes"))
}

/// A 64-bit FNV-1a over the file's path, modification time, range and settings,
/// so an edited file or a changed setting misses the cache.
fn cache_key(path: &Path, range: (u32, u32), st: &SceneSettings) -> String {
    let mtime = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let text = format!(
        "v5|{}|{mtime}|{}-{}|{:08x}|{:016x}|{:?}",
        path.display(),
        range.0,
        range.1,
        st.threshold.to_bits(),
        st.min_shot_secs.to_bits(),
        st.sample_fps.map(f64::to_bits),
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The cuts and flashes a cache file holds, as `read_cache` returns them.
type CachedScenes = (Vec<Cut>, Vec<(u32, u32)>);

fn read_cache(file: &Path) -> Option<CachedScenes> {
    let v: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    let cuts = v
        .get("cuts")?
        .as_array()?
        .iter()
        .map(|c| {
            let kind = match c.get("kind")?.as_str()? {
                "hard" => CutKind::Hard,
                "fade" => CutKind::FadeBlack,
                "dissolve" => CutKind::Dissolve { end: u32::try_from(c.get("end")?.as_u64()?).ok()? },
                _ => return None,
            };
            Some(Cut {
                frame: u32::try_from(c.get("frame")?.as_u64()?).ok()?,
                score: c.get("score")?.as_f64()? as f32,
                kind,
            })
        })
        .collect::<Option<Vec<Cut>>>()?;
    let flashes = v
        .get("flashes")?
        .as_array()?
        .iter()
        .map(|f| {
            let at = |k: &str| u32::try_from(f.get(k)?.as_u64()?).ok();
            Some((at("from")?, at("to")?))
        })
        .collect::<Option<Vec<(u32, u32)>>>()?;
    Some((cuts, flashes))
}

/// Writes the cache file atomically (a temporary, then a rename). A cache that
/// cannot be written costs the next scan its time and nothing else.
fn write_cache(file: &Path, cuts: &[Cut], flashes: &[(u32, u32)]) {
    let body = json!({
        "cuts": cuts.iter().map(|c| json!({
            "frame": c.frame,
            "score": c.score,
            "kind": kind_name(c.kind),
            "end": match c.kind { CutKind::Dissolve { end } => Some(end), _ => None },
        })).collect::<Vec<_>>(),
        "flashes": flashes.iter().map(|&(from, to)| json!({"from": from, "to": to})).collect::<Vec<_>>(),
    });
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// One contact-sheet image: a tile per shot, at its middle frame. Returns the
/// PNG bytes and the path it was saved to.
/// The sheet is saved in the frame cache folder of the project (as `frame` and
/// `contact_sheet` do), and its path is returned with the PNG bytes.
fn sheet_of(path: &Path, shots: &[(u32, u32)], fps: f64, dir: &Path) -> Result<(Vec<u8>, PathBuf), Reply> {
    let mids: Vec<u32> = shots.iter().map(|&(a, b)| a + (b - a) / 2).collect();
    let (Some(&lo), Some(&last)) = (mids.first(), mids.last()) else {
        return Err(Reply::err("no shots to draw"));
    };
    let (_, rx, cancel) = DecodeSession::open_range(path, lo, last + 1)
        .map_err(|e| Reply::err(format!("cannot open {} for the sheet: {e}", file_name(path))))?;
    let mut tiles: Vec<Frame> = Vec::with_capacity(mids.len());
    for frame in rx.iter() {
        let Some(&want) = mids.get(tiles.len()) else { break };
        if frame.index < want {
            continue;
        }
        if frame.index > want {
            break;
        }
        let h = fit_height(&frame, TILE_W);
        tiles.push(resize(&frame, TILE_W, h));
    }
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    if tiles.len() != mids.len() {
        return Err(Reply::err(format!(
            "could not decode the middle frame of every shot ({} of {})",
            tiles.len(),
            mids.len()
        )));
    }
    let cols = SHEET_COLS.min(mids.len()) as u32;
    let sheet = tile_sheet(&tiles, &mids, cols, TILE_W, fps);
    let png = to_png_bytes(&sheet).map_err(|e| Reply::err(format!("cannot encode the sheet as PNG: {e}")))?;
    let saved = save_png(&png, dir, &format!("scenes-{}-{lo}-{last}", file_name(path)), None)?;
    Ok((png, saved))
}

/// The text answer: a header, then one line per shot in source time, with the
/// timeline time of each shot when the target was a clip.
#[allow(clippy::too_many_arguments)]
fn describe(
    target: &Target,
    name: &str,
    shots: &[(u32, u32)],
    cuts: &[Cut],
    flashes: &[(u32, u32)],
    from: u32,
    to: u32,
    fps: f64,
) -> Vec<String> {
    let starts = inner(from, to, cuts);
    let mut lines = vec![format!(
        "{} shots · src:{} {name} · {from}f–{to}f ({}–{})",
        shots.len(),
        target.src,
        parse::clock(from, fps),
        parse::clock(to, fps),
    )];
    for (i, &(a, b)) in shots.iter().enumerate() {
        let mut line = format!("{}. {}–{} ({}f)", i + 1, parse::clock(a, fps), parse::clock(b, fps), b - a);
        if let Some(c) = i.checked_sub(1).and_then(|k| starts.get(k)) {
            line.push_str(&match c.kind {
                CutKind::Hard => format!(" · cut {:.2}", c.score),
                CutKind::FadeBlack => " · fade through black".to_string(),
                CutKind::Dissolve { end } => format!(" · dissolve {}–{}", parse::clock(c.frame, fps), parse::clock(end, fps)),
            });
        }
        if let Some(clip) = &target.clip {
            let (tl_a, tl_b) = (timeline_of(target.rate, clip, a), timeline_of(target.rate, clip, b));
            line.push_str(&format!(
                " · timeline {}–{}",
                parse::clock(tl_a, target.timeline_fps),
                parse::clock(tl_b, target.timeline_fps)
            ));
        }
        lines.push(line);
    }
    for &(a, b) in flashes {
        lines.push(format!(
            "note: flash at {}–{} ({}f), not a cut",
            parse::clock(a, fps),
            parse::clock(b, fps),
            b - a
        ));
    }
    lines
}

/// The JSON answer: shots and cuts as `"Nf"` strings in source frames.
#[allow(clippy::too_many_arguments)]
fn structured_of(
    target: &Target,
    name: &str,
    shots: &[(u32, u32)],
    cuts: &[Cut],
    flashes: &[(u32, u32)],
    from: u32,
    to: u32,
    fps: f64,
    cached: bool,
) -> Value {
    let frames = |f: u32| format!("{f}f");
    let mut out = json!({
        "source": format!("src:{}", target.src),
        "file": name,
        "fps": fps,
        "cached": cached,
        "range": {"from": frames(from), "to": frames(to)},
        "shots": shots.iter().map(|&(a, b)| json!({"from": frames(a), "to": frames(b)})).collect::<Vec<_>>(),
        "cuts": inner(from, to, cuts).iter().map(|c| json!({
            "frame": frames(c.frame),
            "score": c.score,
            "kind": kind_name(c.kind),
            "end": match c.kind { CutKind::Dissolve { end } => Some(frames(end)), _ => None },
        })).collect::<Vec<_>>(),
        "flashes": flashes.iter().map(|&(a, b)| json!({"from": frames(a), "to": frames(b)})).collect::<Vec<_>>(),
    });
    if let Some(clip) = &target.clip {
        out["clip"] = json!({"start": clip.start, "lane_frames": clip.frames()});
        out["timeline"] = json!(shots
            .iter()
            .map(|&(a, b)| json!({"from": timeline_of(target.rate, clip, a), "to": timeline_of(target.rate, clip, b)}))
            .collect::<Vec<_>>());
    }
    out
}

/// The timeline frame a source frame of the file plays at, through the clip's
/// speed. The clip's `in_frame` counts at the timeline's rate, so the file's
/// frame is first put on that rate.
fn timeline_of(rate: Rate, clip: &Clip, source: u32) -> u32 {
    clip.start + clip.timeline_offset(rate.timeline_at(source).saturating_sub(clip.in_frame))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard};

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn server_on(path: &Path) -> Server {
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(path).expect("open test asset"));
        srv
    }

    /// Scene sheets are saved under this process's temp root. The calls run one at a time
    /// and clear that root afterwards, so no test leaves a `edith_mcp-<pid>` folder behind.
    static SCRATCH: Mutex<()> = Mutex::new(());

    fn call(srv: &mut Server, args: Value) -> Reply {
        let _lock: MutexGuard<'_, ()> = SCRATCH.lock().unwrap_or_else(|e| e.into_inner());
        let tool = crate::server::tools().into_iter().find(|t| t.name == "scenes").expect("tool exists");
        let r = (tool.run)(srv, &args);
        crate::tools::see::remove_scratch();
        r
    }

    /// Two 1 s shots of different content, joined, in a fresh directory that is removed
    /// when the returned guard drops.
    fn two_shots(name: &str) -> (TestDir, PathBuf) {
        let dir = TestDir::new(&format!("scan-{name}"));
        let out = dir.join("two.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-t", "1", "-i", "testsrc2=size=320x180:rate=25"])
            .args(["-f", "lavfi", "-t", "1", "-i", "mandelbrot=size=320x180:rate=25"])
            .args(["-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[v]", "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&out)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        (dir, out)
    }

    #[test]
    fn scan_lists_the_shots_of_a_two_shot_file() {
        let (_dir, path) = two_shots("lists");
        let mut srv = server_on(&path);
        let r = call(&mut srv, json!({"source": "src:0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1. 00:00.000–"), "{}", r.text);
        assert!(r.text.contains("2. "), "{}", r.text);
        let shots = r.structured.as_ref().unwrap()["shots"].as_array().unwrap().clone();
        assert_eq!(shots.len(), 2, "{}", r.text);
        assert_eq!(shots[0]["from"], "0f");
        assert!(shots[0]["to"].as_str().unwrap().ends_with('f'));
        assert_eq!(shots[1]["to"], "50f");
    }

    #[test]
    fn scan_range_and_cache_round_trip() {
        let (_dir, path) = two_shots("cache");
        let mut srv = server_on(&path);
        let dir = path.parent().unwrap().join("cache-dir");
        let settings = SceneSettings::default();
        let first = cuts_for(Some(&dir), &path, (0, 50), &settings).unwrap();
        assert!(!first.cached);
        let second = cuts_for(Some(&dir), &path, (0, 50), &settings).unwrap();
        assert!(second.cached, "the second scan reads the cache");
        assert_eq!(first.cuts, second.cuts);
        assert_eq!(first.flashes, second.flashes);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let r = call(&mut srv, json!({"source": "src:0", "range": ["10f", "40f"]}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["range"]["from"], "10f");
    }

    #[test]
    fn scan_accepts_a_range_object_as_the_other_tools_do() {
        let (_dir, path) = two_shots("object-range");
        let mut srv = server_on(&path);
        let r = call(&mut srv, json!({"source": "src:0", "range": {"from": "10f", "to": "40f"}}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["range"], json!({"from": "10f", "to": "40f"}));
        let same = call(&mut srv, json!({"source": "src:0", "range": ["10f", "40f"]}));
        assert_eq!(r.structured.as_ref().unwrap()["shots"], same.structured.as_ref().unwrap()["shots"]);
        let bad = call(&mut srv, json!({"source": "src:0", "range": {"from": "30f", "to": "10f"}}));
        assert!(bad.is_error && bad.text.contains("empty"), "{}", bad.text);
    }

    #[test]
    fn scan_refuses_an_empty_or_bad_range() {
        let (_dir, path) = two_shots("bad");
        let mut srv = server_on(&path);
        let r = call(&mut srv, json!({"source": "src:0", "range": ["30f", "10f"]}));
        assert!(r.is_error && r.text.contains("empty"), "{}", r.text);
        let r = call(&mut srv, json!({"source": "src:0", "range": "10f"}));
        assert!(r.is_error, "{}", r.text);
        let r = call(&mut srv, json!({"source": "src:9"}));
        assert!(r.is_error && r.text.contains("no source"), "{}", r.text);
    }

    #[test]
    fn scan_sheet_has_one_tile_per_shot() {
        let (_dir, path) = two_shots("sheet");
        let mut srv = server_on(&path);
        let r = call(&mut srv, json!({"source": "src:0", "sheet": true}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.structured.as_ref().unwrap()["sheet"]["tiles"], 2);
    }

    #[test]
    fn scan_of_a_clip_adds_timeline_times() {
        let mut srv = server_on(&asset("test_av.mp4"));
        let edit = crate::server::tools().into_iter().find(|t| t.name == "edit").expect("edit exists");
        let placed = (edit.run)(&mut srv, &json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "2s"]]}]}));
        assert!(!placed.is_error, "{}", placed.text);
        let r = call(&mut srv, json!({"source": "V1@0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("timeline"), "{}", r.text);
        assert!(r.structured.as_ref().unwrap()["timeline"].is_array());
    }

    /// A 30 fps timeline with the 25 fps test_25fps.mp4 imported as src:1. Its 50 frames
    /// are 2.000 s at its own rate; in timeline frames at 30 fps they would be 60.
    fn open_30_with_25() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_av.mp4")).expect("open 30 fps asset");
        sess.import(&asset("test_25fps.mp4")).expect("import 25 fps asset");
        Server { session: Some(sess), ..Server::default() }
    }

    #[test]
    fn scenes_of_a_clip_on_a_30_fps_timeline_reads_the_source_in_its_own_frames() {
        let mut srv = open_30_with_25();
        let edit = crate::server::tools().into_iter().find(|t| t.name == "edit").expect("edit exists");
        let placed = (edit.run)(&mut srv, &json!({"ops": [{"op": "keep", "source": "src:1", "ranges": [["0s", "2s"]]}]}));
        assert!(!placed.is_error, "{}", placed.text);
        // The clip sits at V1@150 (5 s) and is 60 timeline frames (2 s at 30 fps), which are
        // 50 frames of the 25 fps file.
        let r = call(&mut srv, json!({"source": "V1@150"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("0f–50f (00:00.000–00:02.000)"), "{}", r.text);
        assert!(r.text.contains("timeline 00:05.000–00:07.000"), "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        assert_eq!(st["range"], json!({"from": "0f", "to": "50f"}));
        let timeline = st["timeline"].as_array().unwrap();
        assert_eq!(timeline.first().unwrap()["from"], 150);
        assert_eq!(timeline.last().unwrap()["to"], 210);
    }

    #[test]
    fn scenes_counts_a_25_fps_source_in_its_own_frames_on_a_30_fps_timeline() {
        let mut srv = open_30_with_25();
        let r = call(&mut srv, json!({"source": "src:1"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("0f–50f (00:00.000–00:02.000)"), "{}", r.text);
        let st = r.structured.as_ref().unwrap();
        assert_eq!(st["fps"].as_f64(), Some(25.0));
        assert_eq!(st["range"], json!({"from": "0f", "to": "50f"}));
        let shots = st["shots"].as_array().unwrap();
        assert_eq!(shots.first().unwrap()["from"], "0f");
        assert_eq!(shots.last().unwrap()["to"], "50f");
    }

    /// 14 s at 25 fps (350 frames): 6 s of steady dark grain (mean luma about 18, dipping below
    /// 16 now and then), then 4 s of test pattern fading out to black, then 4 s fading in from
    /// black. The real fade is the dark run around frames 225 to 275.
    fn dark_then_fade(name: &str) -> (TestDir, PathBuf) {
        let dir = TestDir::new(&format!("scan-{name}"));
        let out = dir.join("darkfade.mp4");
        let graph = "[0:v]noise=alls=40:allf=t[d];[1:v]fade=t=out:st=2:d=1[a];[2:v]fade=t=in:st=0:d=1[b];[d][a][b]concat=n=3:v=1:a=0[v]";
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-t", "6", "-i", "color=c=0x121212:s=320x180:r=25"])
            .args(["-f", "lavfi", "-t", "4", "-i", "testsrc2=s=320x180:r=25"])
            .args(["-f", "lavfi", "-t", "4", "-i", "testsrc2=s=320x180:r=25"])
            .args(["-filter_complex", graph, "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&out)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        (dir, out)
    }

    #[test]
    fn steady_dark_grain_is_not_a_fade_and_a_real_fade_is() {
        let (_dir, path) = dark_then_fade("fade");
        let cuts = cuts_for(None, &path, (0, 350), &SceneSettings::default()).unwrap().cuts;
        let fades: Vec<u32> = cuts.iter().filter(|c| c.kind == CutKind::FadeBlack).map(|c| c.frame).collect();
        assert!(!fades.is_empty(), "the real fade through black is not found: {cuts:?}");
        assert!(fades.iter().all(|&f| (200..=290).contains(&f)), "a fade in the steady dark part: {fades:?}");
        // The steady dark shot on its own, and a frame in the middle of it: near-black share and
        // a dip are both judged on the decoded pixels.
        assert!(!real_fade(&path, 100, 25, 350), "steady dark frame judged a fade");
        assert!(real_fade(&path, fades[0], 25, 350), "the kept fade at {} is not confirmed", fades[0]);
    }

    /// Steady dark grain whose mean luma drifts across 16 (a slow flicker), so the engine's
    /// mean-luma dark run fires on it twice. The pixels are not near-black, so neither is a fade.
    #[test]
    fn steady_dark_flicker_is_not_a_fade() {
        let dir = TestDir::new("scan-flicker");
        let path = dir.join("flicker.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-t", "6", "-i", "color=c=0x0a0a0a:s=320x180:r=25"])
            .args(["-vf", "noise=alls=40:allf=t,eq=brightness='0.012*sin(2*PI*t/2)':eval=frame", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "ultrafast"])
            .arg(&path)
            .status()
            .expect("ffmpeg runs")
            .success();
        assert!(ok);
        let cuts = cuts_for(None, &path, (0, 150), &SceneSettings::default()).unwrap().cuts;
        assert!(cuts.iter().all(|c| c.kind != CutKind::FadeBlack), "grain judged a fade: {cuts:?}");
    }

    #[test]
    fn a_fade_needs_near_black_on_most_of_the_frame() {
        let (_dir, path) = dark_then_fade("share");
        // Frame 100 is steady dark grain: a share of its pixels at luma 16 or below is well under
        // four in five, so it is refused whatever its mean does.
        let frames = frames_between(&path, 100, 100).expect("frame 100 decodes");
        let (mean, share) = luma_stats(&frames[0].1);
        assert!(mean < 24.0 && mean > 10.0, "grain frame mean {mean}");
        assert!(share < FADE_BLACK_SHARE, "grain frame has {share} near-black");
    }

    #[test]
    fn luma_stats_averages_each_pixel_and_counts_the_near_black_ones() {
        // Two pixels, BGRA: pure black (luma 0) and pure white (luma 255). Each is four bytes,
        // so the mean is 127.5 and half the pixels are at luma 16 or below.
        let f = Frame { index: 0, width: 2, height: 1, bgra: vec![0, 0, 0, 255, 255, 255, 255, 255] };
        let (mean, share) = luma_stats(&f);
        assert!((mean - 127.5).abs() < 1e-9, "mean {mean}");
        assert!((share - 0.5).abs() < 1e-9, "near-black share {share}");
    }

    /// A dissolve from frame 100 to 130 splits the shots at its ends: the old shot stops at 100 and
    /// the new one starts at 130, so the dissolve's own frames are in no shot.
    #[test]
    fn a_dissolve_leaves_its_frames_out_of_both_shots() {
        let cuts = [
            Cut { frame: 60, score: 0.5, kind: CutKind::Hard },
            Cut { frame: 100, score: 0.6, kind: CutKind::Dissolve { end: 130 } },
        ];
        assert_eq!(shots_of(0, 200, &cuts), vec![(0, 60), (60, 100), (130, 200)]);
    }

    /// A synthetic frame-difference series: `series[i]` is the picture of frame i, as a luma level,
    /// and the distance between two frames is their level gap over 255 (a flat picture per frame).
    fn levels(series: &[f64]) -> impl Fn(u32, u32) -> f64 + '_ {
        move |a, b| match (series.get(a as usize), series.get(b as usize)) {
            (Some(x), Some(y)) => (x - y).abs() / 255.0,
            _ => f64::NAN,
        }
    }

    /// Shot A (level 130) for 10 frames, a 3-frame white flash (255) at frames 10 to 12, then shot A
    /// again: a flash, not a cut, so `flash_span` at 13 (the first frame back) names [10, 13).
    #[test]
    fn a_three_frame_flash_is_found_from_its_end_and_its_start() {
        let mut s = vec![130.0; 20];
        s[10..13].fill(255.0);
        let d = levels(&s);
        assert_eq!(flash_span(13, &d), Some((10, 13)));
        assert_eq!(flash_span(10, &d), Some((10, 13)));
    }

    /// A real hard cut from A to B (B stays): the picture never comes back, so no flash.
    #[test]
    fn a_hard_cut_to_a_new_shot_is_not_a_flash() {
        let mut s = vec![130.0; 20];
        s[10..].fill(60.0);
        let d = levels(&s);
        assert_eq!(flash_span(10, &d), None);
        assert_eq!(flash_span(15, &d), None);
    }

    /// A flash of five frames is longer than the flash limit, so it is kept as a cut.
    #[test]
    fn a_flash_longer_than_the_limit_is_kept_as_a_cut() {
        let mut s = vec![130.0; 20];
        s[10..15].fill(255.0);
        let d = levels(&s);
        assert_eq!(flash_span(15, &d), None);
    }

    /// A flash whose frames are close to the picture before them differs too little to be one.
    #[test]
    fn a_dip_the_size_of_grain_is_not_a_flash() {
        let mut s = vec![130.0; 20];
        s[10..13].fill(136.0);
        let d = levels(&s);
        assert_eq!(flash_span(13, &d), None);
    }

    /// Unknown distances (a frame the decoder did not give) never make a flash.
    #[test]
    fn unknown_frames_never_make_a_flash() {
        let d = |_: u32, _: u32| f64::NAN;
        assert_eq!(flash_span(13, &d), None);
        assert_eq!(flash_span(0, &d), None);
    }

    /// Hard cuts within a second after a fade in are its steps, and go; a hard cut well after the
    /// fade, or before it, stays.
    #[test]
    fn hard_cuts_inside_a_fade_in_are_dropped() {
        let cut = |frame, kind| Cut { frame, score: 0.1, kind };
        let mut cuts = vec![
            cut(100, CutKind::Hard),
            cut(304, CutKind::FadeBlack),
            cut(322, CutKind::Hard),
            cut(334, CutKind::Hard),
            cut(335, CutKind::Hard),
        ];
        drop_cuts_in_fades(&mut cuts, 30);
        let kept: Vec<u32> = cuts.iter().map(|c| c.frame).collect();
        assert_eq!(kept, vec![100, 304, 335]);
    }
}
