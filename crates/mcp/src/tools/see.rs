//! See tools: one composed frame, and a contact sheet of many (MCP.md section 4.4).
//!
//! Both return PNG image blocks (`Reply::image`), a text line that says what is
//! on screen, and a copy of the PNG in the persistent frame cache
//! (`$XDG_CACHE_HOME/edith/frames/<project>/`) so the agent can re-open it later.
//! Everything is headless: nothing here touches a window.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use engine::decode::{to_png_bytes, DecodeSession, Frame};
use engine::export::{fit_picture_clips, output_meta, ExportSettings, FitOutcome};
use engine::project::{Clip, Lane, LaneKind, Title};
use engine::scale::{crop_rect, fit_rect, nearest_90_steps, transformed_dst_rect, FitPolicy, Rect};
use engine::text::overlay;
use engine::{Project, PlaybackSession, VideoMeta};

use crate::parse;
use crate::server::{opt_str, tri, Reply, Server, Tool};
use crate::tools::edit_fx::place_word;
use crate::tools::export::{burn_schema, clash_note, parse_burn, parse_fit, parse_size, BURN_KEY};

const FRAME_DEFAULT_W: u32 = 640;
const FRAME_MAX_W: u32 = 1920;
const SHEET_DEFAULT_COUNT: u32 = 12;
const SHEET_MAX_COUNT: u32 = 48;
const SHEET_DEFAULT_COLS: u32 = 4;
const SHEET_DEFAULT_W: u32 = 1280;
const SHEET_MAX_W: u32 = 2400;
/// Dark gutter between tiles, in pixels.
const GUTTER: u32 = 4;
/// The band above each contact-sheet tile that holds its timecode: the 7-row glyphs at
/// `SCALE` px, the outline, and a margin.
const LABEL_H: u32 = (7 * SCALE) as u32 + 6;
/// Fill colour of the sheet: BGRA, dark grey.
const GUTTER_FILL: [u8; 4] = [24, 24, 24, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
/// The fill and cross of a picture whose file is offline.
const MISSING_FILL: [u8; 4] = [48, 48, 48, 255];
const MISSING_MARK: [u8; 4] = [150, 150, 150, 255];
/// Pixels per glyph pixel in the stamp.
const SCALE: i64 = 2;
/// Cut points listed in a contact sheet before "and N more".
const CUTS_CAP: usize = 40;

const FRAME_DOC: &str = "One frame of the composed timeline as a PNG image. Arguments: at (required: \
a frame, a timecode or seconds; with source, a time in that source); source (src:<n>, takes the \
frame from that source file instead of the timeline); width (pixels, default 640, max 1920); \
save (a path you give: one more copy of the PNG is written there, parent folders are created); \
burn_captions (true, or an object, to draw the timeline's captions onto the frame; not with source); \
size (WIDTHxHEIGHT such as 1080x1920, or a height such as 1080p) and fit (keep, fit or fill) preview \
the timeline as export_start would draw it at that picture size and fit: the reply says it is a \
preview of that export size, and the project is not changed (not with source). \
The PNG is always written to the frame cache, ~/.cache/edith/frames/<project>/ (or \
$XDG_CACHE_HOME/edith/frames/<project>/), named <at>-<width>-<hash>.png; the cache keeps the newest \
200 frames. The reply's `saved:` line gives the save path when you passed save, else the cache path. \
The text says which clip is on each lane at that frame, and lists the layers (a picture over \
another is named; a dissolve in progress shows its two clips and how far it has run). \
Seconds round to the nearest frame, and the reply then shows both: `0.5s → 13f`.";

const SHEET_DOC: &str = "A grid of frames, evenly spaced through a range (default the whole \
timeline), each stamped with its timecode, as one PNG image. Arguments: range ({from, to}, default \
the whole timeline, or the whole source with source); count (frames, default 12, max 48); cols \
(tiles per row, default 4); width (whole sheet in pixels, default 1280, max 2400); source (src:<n>, \
samples that source file; range is then in its time); save (a path you give: one more copy of the \
PNG is written there, parent folders are created); burn_captions (as for frame; not with source). \
The PNG is always written to the frame cache, ~/.cache/edith/frames/<project>/ (or \
$XDG_CACHE_HOME/edith/frames/<project>/), and its path is in the reply's `saved:` line when you \
pass no save. The text lists each tile's time and the clip under it, and the edit joins inside the \
range (the joins between clips on the picture lanes: hard cuts, and dissolves with their length, \
listed apart), so a tile can be related to a join. Picture cuts inside a clip are not joins: the \
line points to scenes for them. With source there are no joins, and the line says so. \
Each tile's timecode is stamped on it when it fits the tile; on narrow tiles (about 240 px wide with \
4 columns) the stamps are left off and the text lists the times.";

const SCOPES_DOC: &str = "The video scopes of one frame of the composed timeline, as numbers: a luma \
waveform, an RGB parade and a vectorscope, read from the graded picture at low resolution (what export \
draws). Arguments: at (required: a frame, a timecode or seconds); bands (waveform bands left to right, \
default 4, max 16). Levels are display percent, 0 black and 100 white (full-range R'G'B' of the Rec. 709 \
picture). The reply gives: luma min / 5th percentile / median / 95th / max and mean, the share of the \
frame crushed (luma at or under 2%) and clipped (at or over 98%); the waveform's p5 / median / p95 per \
band, left to right; each of red, green and blue as the parade shows it, and which channel sits highest; \
the mean saturation and the cast (how far, and toward which hue, the picture's average sits off grey on \
the vectorscope). A LUT a clip names that cannot be used (missing or broken) is listed.";

fn scopes_schema() -> Value {
    json!({
        "type": "object",
        "description": SCOPES_DOC,
        "properties": {
            "at": {
                "type": ["string", "number"],
                "description": "The frame to read: 450f, 15.0s, 00:00:15:00, or a number of seconds."
            },
            "bands": {
                "type": "integer",
                "minimum": 1,
                "maximum": 16,
                "description": "Waveform bands, left to right. Default 4."
            }
        },
        "required": ["at"]
    })
}

/// Width the scopes read the frame at: plenty for a scope, and quick.
const SCOPES_W: u32 = 320;

fn scopes(server: &mut Server, args: &Value) -> Reply {
    let bands = match args.get("bands") {
        None | Some(Value::Null) => 4,
        Some(v) => match v.as_u64().filter(|b| (1..=16).contains(b)) {
            Some(b) => b as usize,
            None => return Reply::err("bands is a whole number 1..16"),
        },
    };
    let s = tri!(server.session_ref());
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let at = tri!(parse::frame_arg(args, "at", fps, end));
    let (project, meta) = s.export_snapshot();
    let media = project.media_frames();
    if at >= media {
        return Reply::err(past_end_message(args, fps, at, media));
    }
    let f = match engine::export::compose_at_with(&project, &meta, at, SCOPES_W, SCOPES_W * 4, None) {
        Ok(f) => f,
        Err(e) => return Reply::err(format!("cannot compose frame {at}: {e}")),
    };
    let sc = engine::scopes::compute(&f.bgra, f.width as usize, f.height as usize, 64);
    let sum = sc.summary(bands);
    let r1 = |v: f32| (v * 10.0).round() / 10.0;
    let sp = |x: &engine::scopes::Spread| {
        format!("{:.0}/{:.0}/{:.0}/{:.0}/{:.0} (mean {:.1})", x.min, x.p5, x.median, x.p95, x.max, x.mean)
    };
    let names = ["red", "green", "blue"];
    let means = [sum.spread[1].mean, sum.spread[2].mean, sum.spread[3].mean];
    let hi = (0..3).max_by(|&a, &b| means[a].total_cmp(&means[b])).unwrap_or(0);
    let lo = (0..3).min_by(|&a, &b| means[a].total_cmp(&means[b])).unwrap_or(0);
    let balance = if means[hi] - means[lo] < 2.0 {
        "channels balanced (means within 2%)".to_string()
    } else {
        format!("{} highest, {} lowest ({:.1}% apart)", names[hi], names[lo], means[hi] - means[lo])
    };
    let cast = if sum.cast < 1.0 {
        "no cast (average within 1% of grey)".to_string()
    } else {
        format!("cast toward {} ({:.1}% off grey, {:.0}°)", engine::scopes::hue_word(sum.cast_angle), sum.cast, sum.cast_angle)
    };
    let bands_text: Vec<String> = sum.bands.iter().map(|b| format!("{:.0}/{:.0}/{:.0}", b[0], b[1], b[2])).collect();
    let mut text = format!(
        "scopes · {} ({}) · read at {}x{}\n\
         waveform (luma %, min/p5/median/p95/max): {}; crushed {:.1}%, clipped {:.1}%\n\
         waveform bands left→right (p5/median/p95): {}\n\
         parade: red {}; green {}; blue {}; {balance}\n\
         vectorscope: mean saturation {:.1}%; {cast}",
        parse::clock(at, fps),
        rounded_frame(args, fps, at),
        f.width,
        f.height,
        sp(&sum.spread[0]),
        sum.crushed,
        sum.clipped,
        bands_text.join(" | "),
        sp(&sum.spread[1]),
        sp(&sum.spread[2]),
        sp(&sum.spread[3]),
        sum.saturation,
    );
    let problems = s.lut_problems();
    for (path, why) in &problems {
        text.push_str(&format!("\nLUT not applied: {} ({why})", path.display()));
    }
    let spread_json = |x: &engine::scopes::Spread| {
        json!({"min": r1(x.min), "p5": r1(x.p5), "median": r1(x.median), "p95": r1(x.p95), "max": r1(x.max), "mean": r1(x.mean)})
    };
    let structured = json!({
        "frame": at,
        "seconds": parse::secs(at, fps),
        "luma": spread_json(&sum.spread[0]),
        "crushed_pct": r1(sum.crushed),
        "clipped_pct": r1(sum.clipped),
        "waveform_bands": sum.bands.iter().map(|b| json!([r1(b[0]), r1(b[1]), r1(b[2])])).collect::<Vec<Value>>(),
        "parade": {"red": spread_json(&sum.spread[1]), "green": spread_json(&sum.spread[2]), "blue": spread_json(&sum.spread[3])},
        "saturation_pct": r1(sum.saturation),
        "cast": {"pct": r1(sum.cast), "angle": r1(sum.cast_angle), "hue": engine::scopes::hue_word(sum.cast_angle)},
        "lut_problems": problems.iter().map(|(p, e)| json!({"path": p.display().to_string(), "problem": e})).collect::<Vec<Value>>(),
    });
    Reply::ok(text).with(structured)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "frame", description: FRAME_DOC, schema: frame_schema, run: frame },
        Tool { name: "contact_sheet", description: SHEET_DOC, schema: sheet_schema, run: contact_sheet },
        Tool { name: "scopes", description: SCOPES_DOC, schema: scopes_schema, run: scopes },
    ]
}

// ---- schemas --------------------------------------------------------------

fn frame_schema() -> Value {
    json!({
        "type": "object",
        "description": FRAME_DOC,
        "properties": {
            "at": {
                "type": ["string", "number"],
                "description": "The frame to show: 450f, 15.0s, 00:00:15:00, 01:02.500, or a number of seconds. Seconds round to the nearest frame (0.5s at 25 fps is frame 13). On the timeline unless source is given; with source, a time in that source file."
            },
            "source": {
                "type": "string",
                "description": "A source to take the frame from instead of the timeline, such as src:2. Then `at` is a time in that source."
            },
            "width": {
                "type": "integer",
                "minimum": 1,
                "maximum": FRAME_MAX_W,
                "description": "Width of the image in pixels; the height follows the aspect. Default 640, max 1920."
            },
            "save": {
                "type": "string",
                "description": "Also write one more copy of the PNG to this path (parent folders are created). The frame cache copy is always written too; the reply's saved line gives this path."
            },
            "size": {
                "type": "string",
                "description": "Preview the timeline at this export picture size, as export_start takes it: WIDTHxHEIGHT such as 1080x1920, or a height with a p such as 1080p (16:9). Both sides even. The image is the export's frame scaled to width; the reply says it is a preview of that size."
            },
            "fit": {
                "type": "string",
                "enum": ["keep", "fit", "fill"],
                "description": "Preview the export's fit of full-frame picture clips, as export_start takes it: keep (each clip's own fit, the default), fit, or fill. Overlays keep their own fit."
            },
            "burn_captions": burn_schema()
        },
        "required": ["at"]
    })
}

fn sheet_schema() -> Value {
    json!({
        "type": "object",
        "description": SHEET_DOC,
        "properties": {
            "range": {
                "type": "object",
                "description": "The stretch to sample. Omit for the whole timeline (or, with source, the whole source).",
                "properties": {
                    "from": {
                        "type": ["string", "number"],
                        "description": "Start of the stretch, inclusive. Defaults to 0. Times as for `at` in frame."
                    },
                    "to": {
                        "type": ["string", "number"],
                        "description": "End of the stretch, exclusive. Defaults to the end."
                    }
                }
            },
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": SHEET_MAX_COUNT,
                "description": "How many frames to sample, evenly spaced, each at the centre of its interval. Default 12, max 48."
            },
            "cols": {
                "type": "integer",
                "minimum": 1,
                "maximum": SHEET_MAX_COUNT,
                "description": "Tiles per row. Default 4; at most count."
            },
            "width": {
                "type": "integer",
                "minimum": 1,
                "maximum": SHEET_MAX_W,
                "description": "Total width of the sheet in pixels. Default 1280, max 2400."
            },
            "source": {
                "type": "string",
                "description": "Sample a source file instead of the timeline, such as src:2. Then range is in that source's time."
            },
            "save": {
                "type": "string",
                "description": "Also write one more copy of the PNG to this path (parent folders are created). The frame cache copy is always written too; the reply's saved line gives this path."
            },
            "burn_captions": burn_schema()
        }
    })
}

// ---- frame ----------------------------------------------------------------

fn frame(server: &mut Server, args: &Value) -> Reply {
    let width = match width_arg(args, FRAME_DEFAULT_W, FRAME_MAX_W) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let save = opt_str(args, "save").map(PathBuf::from);
    match opt_str(args, "source") {
        None => timeline_frame(server, args, width, save),
        Some(_) if burn_asked(args) => {
            Reply::err("burn_captions draws the timeline's captions: it cannot be used with source")
        }
        Some(_) if export_asked(args) => {
            Reply::err("size and fit preview the timeline's export: they cannot be used with source")
        }
        Some(src) => source_frame(server, args, src, width, save),
    }
}

/// Whether `size` or `fit` is given: the frame then previews the timeline as an export draws it.
fn export_asked(args: &Value) -> bool {
    [args.get("size"), args.get("fit")].into_iter().flatten().any(|v| !v.is_null())
}

/// The line a size or fit preview says: which export it stands for and what its fit reached.
fn preview_words(meta: &VideoMeta, fit: Option<FitPolicy>, outcome: Option<&FitOutcome>) -> String {
    // parse_fit only gives keep (no policy), fit or fill.
    let word = match fit {
        Some(FitPolicy::Fill) => "fill",
        Some(FitPolicy::Fit) => "fit",
        _ => "keep",
    };
    let on_fit = match outcome {
        Some(o) => {
            let clips: usize = o.lanes.iter().map(|(_, n)| n).sum();
            format!(", {clips} full-frame clip(s) on it, {} overlay(s) keep their own", o.overlays)
        }
        None => String::new(),
    };
    format!("preview of the export at {}x{} (fit {word}{on_fit}); the project is unchanged", meta.width, meta.height)
}

/// The frame as the reply names it: `13f` when the `at` argument was exact, and
/// `0.5s → 13f` when it was seconds that rounded to a frame (MCP.md 3.1).
fn rounded_frame(args: &Value, fps: f64, at: u32) -> String {
    let secs = match args.get("at") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if !s.trim_end().ends_with('f') && !s.contains(':') => {
            s.trim().trim_end_matches('s').parse::<f64>().ok()
        }
        _ => None,
    };
    match secs {
        Some(secs) if (secs * fps - at as f64).abs() > 1e-6 => format!("{secs}s → {at}f"),
        _ => format!("{at}f"),
    }
}

/// The refusal for a frame at or past the end of the timeline. It names the time
/// the caller gave in seconds, the timeline length, and for an empty timeline
/// the way out (media_import adds sources without placing them).
fn past_end_message(args: &Value, fps: f64, at: u32, media: u32) -> String {
    let asked = match args.get("at") {
        Some(Value::Number(n)) => n.as_f64().map(|secs| format!("{secs:.1}s")),
        Some(Value::String(s)) if !s.trim_end().ends_with('f') && !s.contains(':') => {
            s.trim().trim_end_matches('s').parse::<f64>().ok().map(|secs| format!("{secs:.1}s"))
        }
        _ => None,
    };
    let asked = asked.unwrap_or_else(|| format!("frame {at}"));
    if media == 0 {
        return format!(
            "{asked} is past the end: the timeline is empty. media_import adds sources without placing them; \
             place or append them first, or use frame {{source:\"src:N\", at}} to look at a source"
        );
    }
    let len = f64::from(media) / fps;
    format!("{asked} is past the end (timeline {len:.1}s); pick a time before the end")
}

/// Whether the `at` argument is the word `end`.
fn asks_end(args: &Value) -> bool {
    matches!(args.get("at"), Some(Value::String(s)) if s.trim().eq_ignore_ascii_case("end"))
}

/// The warning for two titles on the same place that overlap in time, as a reply line that
/// starts with a newline (empty when they do not). The window named is the part where both
/// are on screen: `two titles overlap at top 1.0–3.0s`. Used by the frame and by title adds.
pub(crate) fn overlap_line(a: &Title, b: &Title, fps: f64) -> String {
    if a.style.place != b.style.place {
        return String::new();
    }
    let from = a.start.max(b.start);
    let to = a.end().min(b.end());
    if from >= to {
        return String::new();
    }
    let from = secs_text(parse::secs(from, fps));
    let window = format!("{}–{}", from.trim_end_matches('s'), secs_text(parse::secs(to, fps)));
    format!("\n   warning: two titles overlap at {} {window}", place_word(a.style.place))
}

/// A frame's size as the reply gives it. A preview narrower than the thing it stands for says
/// so and names that size: `preview 640x1137 (the export is 1080x1920)`.
fn size_text(w: u32, h: u32, full: (u32, u32), whole: &str) -> String {
    if (w, h) == full {
        format!("{w}x{h}")
    } else {
        format!("preview {w}x{h} ({whole} is {}x{})", full.0, full.1)
    }
}

/// A path as the reply names it: absolute, so the person can open it from anywhere.
fn absolute_path(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

/// The warnings for a title on screen that covers part of a picture on an upper lane: every
/// visible picture lane but the bottom one. The title's box is what `overlay` draws for it at the
/// export size, and the picture's box is where the composite places its clip.
fn title_cover_lines(s: &PlaybackSession, project: &Project, meta: &VideoMeta, at: u32) -> Vec<String> {
    let visible: Vec<Lane> = s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video && !s.lane_hidden(*l)).collect();
    let upper: Vec<(Lane, usize)> = visible
        .iter()
        .skip(1)
        .filter_map(|&l| s.lane_clips(l).iter().position(|c| c.start <= at && at < c.end()).map(|i| (l, i)))
        .collect();
    let mut out = Vec::new();
    if upper.is_empty() {
        return out;
    }
    let canvas = (meta.width, meta.height);
    for (ti, t) in s.titles().iter().enumerate() {
        if t.alpha_at(at) == 0 {
            continue;
        }
        let Some(ink) = title_box(t, canvas) else {
            continue;
        };
        for &(lane, idx) in &upper {
            let clip = &s.lane_clips(lane)[idx];
            let Some(src) = s.sources().get(clip.source) else {
                continue;
            };
            let Some((w, h)) = picture_size(&src.path) else {
                continue;
            };
            if !boxes_meet(ink, picture_box(project, lane, idx, at, (w, h), canvas)) {
                continue;
            }
            let name = src.path.file_stem().map(|n| format!(" ({})", n.to_string_lossy())).unwrap_or_default();
            out.push(format!(
                "\n   warning: title T{} covers part of {}{name}",
                ti + 1,
                parse::addr(lane, clip.start)
            ));
        }
    }
    out
}

/// The box a title draws on a `size` canvas, as `[x0, y0, x1, y1)`. It is found by blending the
/// overlay over a white canvas and over a black one: a plate darkens the white, and the glyphs
/// light the black, so the pixels that change are the ones the title covers.
fn title_box(t: &Title, (w, h): (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let lines: Vec<String> = t.text.split('\n').map(str::to_string).collect();
    let ov = overlay(&t.style, w, h, &lines);
    let mut hit: Option<(u32, u32, u32, u32)> = None;
    for fill in [255u8, 0] {
        let mut canvas = vec![fill; w as usize * h as usize * 4];
        ov.blend_bgra(&mut canvas);
        for (i, px) in canvas.as_chunks::<4>().0.iter().enumerate() {
            if px[..3].iter().all(|&c| c == fill) {
                continue;
            }
            let (x, y) = (i as u32 % w, i as u32 / w);
            hit = Some(match hit {
                None => (x, y, x + 1, y + 1),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
            });
        }
    }
    hit
}

/// The displayed size of a source's picture: a video's, or a still's. `None` for sound alone.
fn picture_size(path: &Path) -> Option<(u32, u32)> {
    match engine::probe_media(path).video {
        Some(v) => Some((v.width, v.height)),
        None if engine::is_image(path) => engine::decode::image_size(path).ok(),
        None => None,
    }
}

/// The box a clip covers on the `canvas` at `at`, with its fit policy and transform applied as the
/// composite applies them (the crop first, then the quarter turn, then the placement).
fn picture_box(project: &Project, lane: Lane, idx: usize, at: u32, (w, h): (u32, u32), canvas: (u32, u32)) -> Rect {
    let t = project.transform_at(lane, idx, at).unwrap_or_default();
    let crop = crop_rect(w, h, &t);
    let (rw, rh) = if nearest_90_steps(t.rotate) % 2 == 1 { (crop.h, crop.w) } else { (crop.w, crop.h) };
    let (dst, placed) = fit_rect(rw, rh, canvas.0, canvas.1, project.fit_of(lane, idx));
    transformed_dst_rect(dst, placed, rw, rh, &t, canvas.0, canvas.1).0
}

/// Whether a title's box `[x0, y0, x1, y1)` shares a pixel with a picture's rect.
fn boxes_meet((x0, y0, x1, y1): (u32, u32, u32, u32), r: Rect) -> bool {
    !r.is_empty() && x0 < r.x + r.w && r.x < x1 && y0 < r.y + r.h && r.y < y1
}

/// The sources the picture lanes read at `at` whose files are not on disk, as (index, path),
/// one entry per source. A hidden lane is not drawn, so a clip on one does not count.
fn offline_under(s: &PlaybackSession, at: u32) -> Vec<(usize, PathBuf)> {
    let mut out: Vec<(usize, PathBuf)> = Vec::new();
    for lane in s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video && !s.lane_hidden(*l)) {
        let Some(c) = s.lane_clips(lane).iter().find(|c| c.start <= at && at < c.end()) else {
            continue;
        };
        let Some(src) = s.sources().get(c.source) else {
            continue;
        };
        if !engine::source_present(&src.path) && !out.iter().any(|(i, _)| *i == c.source) {
            out.push((c.source, src.path.clone()));
        }
    }
    out
}

/// The sentence that names an offline source and the fix, for a reply or a refusal.
fn offline_line(idx: usize, path: &Path) -> String {
    format!("src:{idx} {} is offline; its picture is missing here — relink with media_relink", path.display())
}

/// The picture drawn where a clip's file is offline: a grey tile with a cross, as tall as
/// the composed frame would be at `width` for a picture of `src_w` by `src_h`.
fn missing_tile(width: u32, src_w: u32, src_h: u32) -> Frame {
    let height = (u64::from(width) * u64::from(src_h) / u64::from(src_w.max(1))).max(1) as u32;
    let mut c = Canvas::new(width, height, MISSING_FILL);
    let (w, h) = (i64::from(width), i64::from(height));
    for x in 0..w {
        let y = x * h / w;
        for d in -1..=1 {
            c.put(x, y + d, MISSING_MARK);
            c.put(w - 1 - x, y + d, MISSING_MARK);
        }
    }
    c.into_frame()
}

/// Whether `burn_captions` is switched on: `true` or an object. `false`, `null`
/// and an absent key are off.
fn burn_asked(args: &Value) -> bool {
    !matches!(args.get(BURN_KEY), None | Some(Value::Null | Value::Bool(false)))
}

/// The `burn_captions` argument as [`parse_burn`] reads it. `true` means the
/// defaults, `{}`; `false` means off, as an absent key does.
fn burn_of(
    args: &Value,
    s: &PlaybackSession,
) -> Result<Option<(usize, engine::text::TextStyle)>, String> {
    match args.get(BURN_KEY) {
        Some(Value::Bool(true)) => parse_burn(&json!({ BURN_KEY: {} }), s),
        Some(Value::Bool(false)) => Ok(None),
        _ => parse_burn(args, s),
    }
}

fn timeline_frame(server: &mut Server, args: &Value, width: u32, save: Option<PathBuf>) -> Reply {
    let s = tri!(server.session_ref());
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let at = tri!(parse::frame_arg(args, "at", fps, end));
    let burn = match burn_of(args, s) {
        Ok(b) => b,
        Err(e) => return Reply::err(e),
    };
    let (mut project, mut meta) = s.export_snapshot();
    let media = project.media_frames();
    // `end` is the timeline's last frame: the frame after it is the one that is past the end.
    let at = if at >= media && asks_end(args) { media.saturating_sub(1) } else { at };
    if at >= media {
        return Reply::err(past_end_message(args, fps, at, media));
    }
    // size and fit preview the export as export_start draws it: the same per-export picture size
    // and fit, on this copy of the timeline only, so the project keeps its own.
    let (size, fit) = match (parse_size(args), parse_fit(args)) {
        (Ok(sz), Ok(f)) => (sz, f),
        (Err(e), _) | (_, Err(e)) => return Reply::err(e),
    };
    let fitted = fit.map(|f| (f, fit_picture_clips(&mut project, f)));
    if let Some((w, h)) = size {
        meta = output_meta(&meta, &ExportSettings { size: Some((w, h)), ..ExportSettings::default() });
    }
    let preview = (size.is_some() || fit.is_some())
        .then(|| preview_words(&meta, fit, fitted.as_ref().map(|(_, o)| o)));
    let captions = burn.as_ref().map(|(track, _)| *track);
    let burn_place = burn.as_ref().map(|(_, style)| style.place);
    // A box this tall never limits the width, so the picture comes back at `width`. A clip
    // reading an offline file cannot compose: its frame is drawn missing, and named below.
    let (f, missing) = match engine::export::compose_at_with(&project, &meta, at, width, width * 4, burn) {
        Ok(f) => (f, Vec::new()),
        Err(e) => {
            let missing = offline_under(s, at);
            if missing.is_empty() {
                return Reply::err(format!("cannot compose frame {at}: {e}"));
            }
            (missing_tile(width, meta.width, meta.height), missing)
        }
    };
    let png = match to_png_bytes(&f) {
        Ok(p) => p,
        Err(e) => return Reply::err(format!("cannot encode frame {at} as PNG: {e}")),
    };
    let dir = frames_dir(server.path.as_deref());
    let cached = tri!(save_png(&png, &dir, &format!("{at}-{}.png", f.width), save.as_deref()));
    let saved = save.map(absolute_path).unwrap_or_else(|| cached.clone());
    let size = size_text(f.width, f.height, (meta.width, meta.height), "the export");
    let under = describe_timeline(s, at);
    let head = match (under.top_addr.as_deref(), hidden_only_text(&under.hidden_here)) {
        (None, Some(hidden)) => format!(
            "hidden lane only · black ({hidden}) · {} ({}) · {size}",
            parse::clock(at, fps),
            rounded_frame(args, fps, at),
        ),
        (top, _) => format!(
            "{} · {} ({}) · {} · {size}",
            top.unwrap_or("gap"),
            parse::clock(at, fps),
            rounded_frame(args, fps, at),
            under.top_desc.as_deref().unwrap_or("black"),
        ),
    };
    let mut text = head;
    if let Some(p) = &preview {
        text.push('\n');
        text.push_str(p);
    }
    for line in &under.lines {
        text.push_str("\n  ");
        text.push_str(line);
    }
    let (layer_lines, layer_json) = layer_notes(s, at);
    for line in &layer_lines {
        text.push_str("\n  ");
        text.push_str(line);
    }
    for (i, p) in &missing {
        text.push('\n');
        text.push_str(&offline_line(*i, p));
    }
    if let Some(line) = super::stabilise::pending_in(s, at, at) {
        text.push('\n');
        text.push_str(&line);
    }
    let titles: Vec<String> = s
        .titles()
        .iter()
        .filter(|t| t.alpha_at(at) > 0)
        .map(|t| t.text.replace('\n', " / "))
        .collect();
    for t in &titles {
        text.push_str(&format!("\ntitle on screen: \"{t}\""));
    }
    // Two titles of one place on screen here draw over each other: each pair is named.
    let on: Vec<&Title> = s.titles().iter().filter(|t| t.alpha_at(at) > 0).collect();
    for (i, a) in on.iter().enumerate() {
        for b in &on[i + 1..] {
            text.push_str(&overlap_line(a, b, fps));
        }
    }
    // A title on screen over a picture on an upper lane hides part of that picture: each is named.
    for line in title_cover_lines(s, &project, &meta, at) {
        text.push_str(&line);
    }
    // What the burn drew here, read from the same cues the picture was drawn from.
    // The burn stacks every cue active at this frame, so each one is listed.
    let drawn: Option<Vec<String>> = captions.map(|track| {
        let cues = engine::export::burn_cues(&project, track, meta.frame_rate).unwrap_or_default();
        let on_screen = engine::export::cues_on_screen(&cues, at, meta.frame_rate);
        let shown: Vec<String> = on_screen.iter().map(|&i| cues[i].text.clone()).collect();
        if shown.is_empty() {
            text.push_str(&format!("\ncaptions burned (track {track}): no caption on screen at this frame"));
        }
        for words in &shown {
            text.push_str(&format!("\ncaptions burned (track {track}): \"{}\"", words.replace('\n', " / ")));
        }
        // Mirrors export's no-glyph note, for the cues on screen here only: a character the
        // caption face cannot draw in what this frame shows.
        let lines: Vec<String> = on_screen
            .iter()
            .filter(|&&i| cues[i].image.is_none())
            .map(|&i| cues[i].text.clone())
            .collect();
        let missing = engine::text::missing_glyphs(&lines);
        if !missing.is_empty() {
            let codes: Vec<String> = missing.iter().map(|c| format!("U+{:04X}", *c as u32)).collect();
            text.push_str(&format!("\nno glyph for: {} (drawn as an empty box)", codes.join(" ")));
        }
        shown
    });
    // A caption drawn here and a title on screen here in the same place clash in the picture.
    if let (Some(place), Some(shown)) = (burn_place, drawn.as_ref())
        && !shown.is_empty()
    {
        for t in s.titles().iter().filter(|t| t.alpha_at(at) > 0 && t.style.place == place) {
            text.push('\n');
            text.push_str(&clash_note(parse::secs(at, fps), &t.text, place));
        }
    }
    let picture = has_picture(s);
    if !picture {
        text.push_str("\nno picture here: the timeline is audio only, so the image is black");
    }
    text.push_str(&format!("\nsaved: {}", saved.display()));
    let structured = json!({
        "frame": at,
        "seconds": parse::secs(at, fps),
        "width": f.width,
        "height": f.height,
        "path": saved.display().to_string(),
        "cache_path": cached.display().to_string(),
        "lanes": under.structured,
        "offline": missing.iter().map(|(i, p)| json!({"source": format!("src:{i}"), "path": p.display().to_string()})).collect::<Vec<Value>>(),
        "layers": layer_json["layers"],
        "dissolve": layer_json["dissolve"],
        "titles": titles,
        "picture": picture,
        "captions_burned": captions,
        "caption_drawn": drawn.as_ref().and_then(|d| d.first().cloned()),
        "captions_drawn": drawn.unwrap_or_default(),
    });
    Reply::ok(text).with(structured).image("image/png", base64(&png))
}

fn source_frame(
    server: &mut Server,
    args: &Value,
    src: &str,
    width: u32,
    save: Option<PathBuf>,
) -> Reply {
    let s = tri!(server.session_ref());
    let idx = tri!(source_index(s, src));
    let path = s.sources()[idx].path.clone();
    let name = file_name(&path);
    if !path.exists() {
        return Reply::err(offline_line(idx, &path));
    }
    let fps = probe_fps(&path).unwrap_or_else(|| parse::session_fps(s));
    let at = tri!(parse::frame_arg(args, "at", fps, u32::MAX));
    // A still is a picture. Sound alone has none, so it is not decoded at all.
    let facts = engine::probe_media(&path);
    let picture = facts.video.is_some() || engine::is_image(&path);
    if !picture && engine::is_audio(&path) {
        return Reply::err(format!("no picture: {name} is audio only, so there is no frame to show"));
    }
    // A frame past the file's own length does not exist: say so with the length and the last time there is.
    if let Some(secs) = probe_secs(&path) {
        let len = (secs * fps).ceil() as u32;
        if at >= len {
            return Reply::err(past_length_text(&format!("src:{idx}"), secs, len, at, fps));
        }
    }
    let f = match decode_source(&path, at, width) {
        Ok(f) => f,
        Err(e) => return Reply::err(e),
    };
    let png = match to_png_bytes(&f) {
        Ok(p) => p,
        Err(e) => return Reply::err(format!("cannot encode frame {at} as PNG: {e}")),
    };
    let dir = frames_dir(server.path.as_deref());
    let cached = tri!(save_png(&png, &dir, &format!("src{idx}-{at}-{}.png", f.width), save.as_deref()));
    let saved = save.map(absolute_path).unwrap_or(cached);
    let size = match facts.video.as_ref() {
        Some(v) => size_text(f.width, f.height, (v.width, v.height), "the source"),
        None => format!("{}x{}", f.width, f.height),
    };
    let silent = if picture { "" } else { "\nno picture here: this source is audio only, so the image is black" };
    let text = format!(
        "src:{idx} {name} · {} ({}) · {size}{silent}\nsaved: {}",
        parse::clock(at, fps),
        rounded_frame(args, fps, at),
        saved.display()
    );
    let structured = json!({
        "source": format!("src:{idx}"),
        "frame": at,
        "seconds": parse::secs(at, fps),
        "width": f.width,
        "height": f.height,
        "path": saved.display().to_string(),
    });
    Reply::ok(text).with(structured).image("image/png", base64(&png))
}

/// Seconds in plain words: at least one decimal, at most two (`0.53s`, `1.0s`, `0.5s`).
fn secs_text(x: f64) -> String {
    let s = format!("{x:.2}");
    let t = s.trim_end_matches('0');
    if t.ends_with('.') { format!("{t}0s") } else { format!("{t}s") }
}

/// The refusal for a frame past a source's known length of `len` frames (`secs` long). The last
/// frame there is, `len - 1`, is the most a person can ask for.
fn past_length_text(label: &str, secs: f64, len: u32, at: u32, fps: f64) -> String {
    let last = len.saturating_sub(1);
    format!(
        "{label} is {} ({len}f) long; frame {at} ({}) is past its end — ask for at most {}",
        secs_text(secs),
        secs_text(f64::from(at) / fps),
        secs_text(f64::from(last) / fps),
    )
}

/// One source frame, scaled to `width` wide at its own aspect.
fn decode_source(path: &Path, at: u32, width: u32) -> Result<Frame, String> {
    let (_, rx, cancel) = DecodeSession::open_range(path, at, at.saturating_add(1))
        .map_err(|e| format!("cannot open {} at frame {at}: {e}", file_name(path)))?;
    let frame = rx
        .recv()
        .map_err(|_| {
            format!(
                "no picture at source frame {at} of {}: past its end, or the decoder gave none",
                file_name(path)
            )
        });
    cancel.store(true, Ordering::Relaxed);
    let frame = frame?;
    let h = fit_height(&frame, width);
    Ok(resize(&frame, width, h))
}

// ---- contact sheet --------------------------------------------------------

fn contact_sheet(server: &mut Server, args: &Value) -> Reply {
    let count = match bounded(args, "count", SHEET_DEFAULT_COUNT, 1, SHEET_MAX_COUNT) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let cols = match bounded(args, "cols", SHEET_DEFAULT_COLS.min(count), 1, count) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let width = match bounded(args, "width", SHEET_DEFAULT_W, 1, SHEET_MAX_W) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tile_w = (width.saturating_sub((cols - 1) * GUTTER)) / cols;
    let dir = frames_dir(server.path.as_deref());
    let s = tri!(server.session_ref());
    // A still sheets as one tile the whole width, so its columns never narrow it.
    let still = opt_str(args, "source")
        .and_then(|src| source_index(s, src).ok())
        .is_some_and(|i| engine::is_image(&s.sources()[i].path));
    if !still && tile_w < 16 {
        return Reply::err(format!(
            "width {width} is too narrow for {cols} columns; use a wider width or fewer cols"
        ));
    }
    match opt_str(args, "source") {
        None => sheet_of_timeline(s, args, &dir, count, cols, tile_w),
        Some(_) if burn_asked(args) => {
            Reply::err("burn_captions draws the timeline's captions: it cannot be used with source")
        }
        Some(src) => sheet_of_source(s, args, src, &dir, count, cols, tile_w, width),
    }
}

fn sheet_of_timeline(
    s: &PlaybackSession,
    args: &Value,
    dir: &Path,
    count: u32,
    cols: u32,
    tile_w: u32,
) -> Reply {
    let fps = parse::session_fps(s);
    let burn = match burn_of(args, s) {
        Ok(b) => b,
        Err(e) => return Reply::err(e),
    };
    let captions = burn.as_ref().map(|(track, _)| *track);
    let (project, meta) = s.export_snapshot();
    let total = project.media_frames();
    if total == 0 {
        return Reply::err(
            "the timeline is empty, so there are no frames to sheet; media_import adds sources without placing them, so place a clip first",
        );
    }
    let (from, to) = tri!(range_of(args, fps, total));
    let samples = sample_frames(from, to, count);
    // Which files are offline at each sample, read before the parallel composes. A tile whose
    // clip is offline is drawn missing and named; the other tiles still compose.
    let missing: Vec<Vec<(usize, PathBuf)>> = samples.iter().map(|&f| offline_under(s, f)).collect();
    let (src_w, src_h) = (meta.width, meta.height);

    let picked = parallel(samples.len(), |i| {
        match engine::export::compose_at_with(&project, &meta, samples[i], tile_w, tile_w * 4, burn.clone()) {
            Ok(f) => Ok((f, false)),
            Err(_) if !missing[i].is_empty() => Ok((missing_tile(tile_w, src_w, src_h), true)),
            Err(e) => Err(format!("cannot compose frame {}: {e}", samples[i])),
        }
    });
    let picked = match picked.into_iter().collect::<Result<Vec<(Frame, bool)>, String>>() {
        Ok(p) => p,
        Err(e) => return Reply::err(e),
    };
    let mut gone: Vec<(usize, PathBuf)> = Vec::new();
    for (i, (_, drawn_missing)) in picked.iter().enumerate() {
        if *drawn_missing {
            for (src, path) in &missing[i] {
                if !gone.iter().any(|(j, _)| j == src) {
                    gone.push((*src, path.clone()));
                }
            }
        }
    }
    let frames: Vec<Frame> = picked.into_iter().map(|(f, _)| f).collect();

    let mut tiles = Vec::new();
    for &f in &samples {
        tiles.push(describe_timeline(s, f));
    }
    let cuts = cut_points(s, from, to);
    let sheet = tile_sheet(&frames, &samples, cols, tile_w, fps);
    let png = match to_png_bytes(&sheet) {
        Ok(p) => p,
        Err(e) => return Reply::err(format!("cannot encode the sheet as PNG: {e}")),
    };
    let name = format!("sheet-{from}-{count}-{}.png", sheet.width);
    let extra = opt_str(args, "save").map(PathBuf::from);
    let saved = match save_png(&png, dir, &name, extra.as_deref()) {
        Ok(p) => extra.unwrap_or(p),
        Err(r) => return r,
    };

    let header = format!(
        "contact sheet · {count} tiles · {cols} cols · {}x{} · {}–{} ({from}f–{to}f)",
        sheet.width,
        sheet.height,
        parse::clock(from, fps),
        parse::clock(to, fps),
    );
    let mut lines = vec![header];
    let mut tile_json = Vec::new();
    for (i, (&f, t)) in samples.iter().zip(&tiles).enumerate() {
        let desc = t.top_addr.clone().map_or_else(
            || "gap".to_string(),
            |a| format!("{a} {}", t.top_desc.clone().unwrap_or_default()),
        );
        lines.push(format!("{:>2}  {} ({f}f)  {desc}", i + 1, parse::clock(f, fps)));
        tile_json.push(json!({"tile": i + 1, "frame": f, "seconds": parse::secs(f, fps), "clip": t.top_addr}));
    }
    if let Some(line) = super::stabilise::pending_in(s, from, to) {
        lines.push(line);
    }
    // Picture joins are listed; audio joins (a music track every few seconds, say) are
    // only counted, so a long range does not bury the picture joins.
    let pictures: Vec<&Cut> = cuts.iter().filter(|c| c.picture).collect();
    let audio_joins = cuts.len() - pictures.len();
    let audio_tail = if audio_joins > 0 {
        let noun = if audio_joins == 1 { "join" } else { "joins" };
        format!(" · ({audio_joins} {noun} on audio lanes not shown)")
    } else {
        String::new()
    };
    // Picture cuts inside a clip are not joins between clips: scenes finds them, so the line points there.
    let scenes_tail = tiles
        .iter()
        .find_map(|t| t.top_addr.clone())
        .map_or_else(String::new, |a| format!(" · picture cuts inside a clip: scenes {a}"));
    // Hard joins and dissolves are listed apart: `edit joins: 3.0s · dissolve 6.50s–7.00s`.
    let hard: Vec<&Cut> = pictures.iter().copied().filter(|c| c.dissolve == 0).collect();
    let dissolves: Vec<&Cut> = pictures.iter().copied().filter(|c| c.dissolve > 0).collect();
    let cap_tail = |n: usize| {
        let more = n.saturating_sub(CUTS_CAP);
        if more > 0 { format!(" · and {more} more") } else { String::new() }
    };
    let joins_line = if pictures.is_empty() {
        if audio_joins > 0 {
            format!("edit joins: none on the picture lanes{audio_tail}")
        } else {
            "edit joins: none in range".to_string()
        }
    } else {
        let cuts_part = if hard.is_empty() {
            "edit joins: none".to_string()
        } else {
            let shown: Vec<String> = hard.iter().take(CUTS_CAP).map(|c| c.text.clone()).collect();
            format!("edit joins: {}{}", shown.join(" · "), cap_tail(hard.len()))
        };
        let dissolve_part = if dissolves.is_empty() {
            String::new()
        } else {
            // The cut is the frame the dissolve ends on, so the window starts a dissolve's length before it.
            let shown: Vec<String> = dissolves
                .iter()
                .take(CUTS_CAP)
                .map(|c| {
                    let from = c.frame.saturating_sub(c.dissolve);
                    format!("dissolve {:.2}s–{:.2}s", parse::secs(from, fps), parse::secs(c.frame, fps))
                })
                .collect();
            format!(" · {}{}", shown.join(", "), cap_tail(dissolves.len()))
        };
        format!("{cuts_part}{dissolve_part}{audio_tail}")
    };
    lines.push(format!("{joins_line}{scenes_tail}"));
    if let Some(track) = captions {
        lines.push(format!("captions burned (track {track})"));
    }
    for (i, p) in &gone {
        lines.push(offline_line(*i, p));
    }
    lines.push(format!("saved: {}", saved.display()));
    let cut_json: Vec<Value> = hard.iter().map(|c| json!({"frame": c.frame, "text": c.text})).collect();
    let dissolve_json: Vec<Value> = dissolves
        .iter()
        .map(|c| {
            json!({
                "frame": c.frame,
                "seconds": parse::secs(c.frame, fps),
                "frames": c.dissolve,
                "length_seconds": parse::secs(c.dissolve, fps),
            })
        })
        .collect();
    let offline_json: Vec<Value> =
        gone.iter().map(|(i, p)| json!({"source": format!("src:{i}"), "path": p.display().to_string()})).collect();
    let structured = json!({
        "width": sheet.width,
        "height": sheet.height,
        "tile": {"w": tile_w, "h": tile_height(&frames)},
        "path": saved.display().to_string(),
        "tiles": tile_json,
        "cuts": cut_json,
        "dissolves": dissolve_json,
        "audio_cuts": audio_joins,
        "captions_burned": captions,
        "offline": offline_json,
    });
    Reply::ok(lines.join("\n"))
        .with(structured)
        .image("image/png", base64(&png))
}

/// Refuses a source with no picture (sound alone) before any frame is looked for, so
/// the reply names the source and the cause rather than a container error.
pub(crate) fn picture_check(path: &Path, idx: usize) -> Result<(), String> {
    let picture = engine::probe_media(path).video.is_some() || engine::is_image(path);
    if picture {
        Ok(())
    } else {
        Err(format!("src:{idx} ({}) has no picture", file_name(path)))
    }
}

#[allow(clippy::too_many_arguments)]
fn sheet_of_source(
    s: &PlaybackSession,
    args: &Value,
    src: &str,
    dir: &Path,
    count: u32,
    cols: u32,
    tile_w: u32,
    width: u32,
) -> Reply {
    let idx = tri!(source_index(s, src));
    let path = s.sources()[idx].path.clone();
    let name = file_name(&path);
    if !path.exists() {
        return Reply::err(offline_line(idx, &path));
    }
    if let Err(e) = picture_check(&path, idx) {
        return Reply::err(e);
    }
    let fps = probe_fps(&path).unwrap_or_else(|| parse::session_fps(s));
    // A still is one picture, one frame long: it sheets as a single tile.
    let still = engine::is_image(&path);
    // Without a probed duration the range has to name its end.
    let total = match still {
        true => 1,
        false => probe_secs(&path).map_or(u32::MAX, |d| (d * fps).round() as u32),
    };
    let (from, to) = tri!(range_of(args, fps, total));
    if to == u32::MAX {
        return Reply::err(format!("{name} has no known duration; give range.to in frames"));
    }
    let save = opt_str(args, "save").map(PathBuf::from);
    // The one tile takes the whole width asked for, not a column's share of it.
    let (count, cols, tile_w) = match still {
        true => (1, 1, width),
        false => (count, cols, tile_w),
    };
    sheet_of_source_range(&path, idx, &name, from, to, fps, dir, count, cols, tile_w, save.as_deref())
}

#[allow(clippy::too_many_arguments)]
fn sheet_of_source_range(
    path: &Path,
    idx: usize,
    name: &str,
    from: u32,
    to: u32,
    fps: f64,
    dir: &Path,
    count: u32,
    cols: u32,
    tile_w: u32,
    save: Option<&Path>,
) -> Reply {
    let samples = sample_frames(from, to, count);
    let frames = parallel(samples.len(), |i| decode_source(path, samples[i], tile_w));
    let frames = match frames.into_iter().collect::<Result<Vec<Frame>, String>>() {
        Ok(f) => f,
        Err(e) => return Reply::err(e),
    };
    let sheet = tile_sheet(&frames, &samples, cols, tile_w, fps);
    let png = match to_png_bytes(&sheet) {
        Ok(p) => p,
        Err(e) => return Reply::err(format!("cannot encode the sheet as PNG: {e}")),
    };
    let file = format!("sheet-src{idx}-{from}-{count}-{}.png", sheet.width);
    let saved = match save_png(&png, dir, &file, save) {
        Ok(p) => save.map_or(p, Path::to_path_buf),
        Err(r) => return r,
    };
    let header = format!(
        "contact sheet · src:{idx} {name} · {count} tiles · {cols} cols · {}x{} · {}–{} ({from}f–{to}f)",
        sheet.width,
        sheet.height,
        parse::clock(from, fps),
        parse::clock(to, fps),
    );
    let mut lines = vec![header];
    let mut tile_json = Vec::new();
    for (i, &f) in samples.iter().enumerate() {
        lines.push(format!("{:>2}  {} ({f}f)  src:{idx} {name}", i + 1, parse::clock(f, fps)));
        tile_json.push(json!({"tile": i + 1, "frame": f, "seconds": parse::secs(f, fps)}));
    }
    // Edit joins sit between clips on the timeline, so a source has none; scenes finds its picture cuts.
    lines.push(format!("edit joins: none (a source has no edit joins) · scenes src:{idx} finds its picture cuts"));
    lines.push(format!("saved: {}", saved.display()));
    let structured = json!({
        "width": sheet.width,
        "height": sheet.height,
        "tile": {"w": tile_w, "h": tile_height(&frames)},
        "path": saved.display().to_string(),
        "tiles": tile_json,
    });
    Reply::ok(lines.join("\n"))
        .with(structured)
        .image("image/png", base64(&png))
}

/// `count` frame numbers in `[from, to)`, each at the centre of its interval.
fn sample_frames(from: u32, to: u32, count: u32) -> Vec<u32> {
    let len = u64::from(to - from);
    let n = u64::from(count);
    (0..n)
        .map(|i| {
            let off = (2 * i + 1) * len / (2 * n);
            (u64::from(from) + off).min(u64::from(to) - 1) as u32
        })
        .collect()
}

/// Lays the frames out as a grid with a dark gutter. Each tile has its timecode in a
/// band above it, not drawn on the picture: a stamp over the picture hid the
/// lower-third titles on narrow tiles (MCP.md 4.4).
pub(crate) fn tile_sheet(frames: &[Frame], samples: &[u32], cols: u32, tile_w: u32, fps: f64) -> Frame {
    let th = tile_height(frames);
    let rows = (frames.len() as u32).div_ceil(cols);
    let cell_h = LABEL_H + th;
    let cw = cols * tile_w + (cols - 1) * GUTTER;
    let ch = rows * cell_h + (rows - 1) * GUTTER;
    let mut canvas = Canvas::new(cw, ch, GUTTER_FILL);
    for (i, (tile, &f)) in frames.iter().zip(samples).enumerate() {
        let i = i as u32;
        let x0 = (i % cols) * (tile_w + GUTTER);
        let y0 = (i / cols) * (cell_h + GUTTER);
        canvas.blit(tile, x0, y0 + LABEL_H);
        let label = parse::clock(f, fps);
        // A stamp that does not fit its tile at 2x is drawn at 1x, and dropped when
        // even that would run into the next tile; the text lists the times anyway.
        if let Some(scale) = stamp_scale(&label, tile_w) {
            draw_text_at(&mut canvas, i64::from(x0) + 4, i64::from(y0) + 3, &label, scale);
        }
    }
    canvas.into_frame()
}

/// The largest stamp scale at which `label` fits inside a tile `tile_w` wide, with
/// a 4 px margin each side, or None when not even 1x fits.
fn stamp_scale(label: &str, tile_w: u32) -> Option<i64> {
    let chars = label.chars().count() as i64;
    [SCALE, 1].into_iter().find(|&k| chars * 6 * k + 8 <= i64::from(tile_w))
}

fn tile_height(frames: &[Frame]) -> u32 {
    frames.iter().map(|f| f.height).max().unwrap_or(1)
}

struct Cut {
    frame: u32,
    text: String,
    /// On a picture (V) lane; audio cuts are counted, not listed.
    picture: bool,
    /// Frames of the dissolve that joins the clips at `frame`; 0 for a hard cut.
    dissolve: u32,
}

/// Clip boundaries on the media lanes that fall strictly inside `(from, to)`. A picture
/// boundary joined by a dissolve is one entry with its length, not a clip ending and
/// another starting, so a dissolve can be told from a hard cut.
fn cut_points(s: &PlaybackSession, from: u32, to: u32) -> Vec<Cut> {
    let mut cuts = Vec::new();
    for lane in s.lanes() {
        let picture = lane.kind == LaneKind::Video;
        let clips = s.lane_clips(lane);
        for (idx, c) in clips.iter().enumerate() {
            let end = c.end();
            let dissolve = if picture { s.transition_out_of(lane, idx) } else { 0 };
            let joined = clips.get(idx + 1).is_some_and(|b| b.start == end);
            if joined && dissolve > 0 {
                if from < end && end < to {
                    let addr = parse::addr(lane, c.start);
                    let text = format!("{end}f {addr} dissolves ({dissolve}f)");
                    cuts.push(Cut { frame: end, text, picture, dissolve });
                }
                continue;
            }
            // A clip that a neighbour joins at its start was listed with that join: a dissolve
            // with its length, a hard cut as one `ends→starts` entry (see below).
            let joined_in = idx > 0 && clips[idx - 1].end() == c.start;
            if from < c.start && c.start < to && !joined_in {
                let addr = parse::addr(lane, c.start);
                cuts.push(Cut { frame: c.start, text: format!("{}f {addr} starts", c.start), picture, dissolve: 0 });
            }
            if from < end && end < to {
                let addr = parse::addr(lane, c.start);
                let text = match clips.get(idx + 1).filter(|b| b.start == end) {
                    // A hard cut is one entry naming both sides, not an end and a start at one frame.
                    Some(b) => format!("{end}f {addr}→{}", parse::addr(lane, b.start)),
                    None => format!("{end}f {addr} ends"),
                };
                cuts.push(Cut { frame: end, text, picture, dissolve: 0 });
            }
        }
    }
    cuts.sort_by_key(|c| c.frame);
    cuts.dedup_by(|a, b| a.frame == b.frame && a.text == b.text);
    cuts
}

// ---- describing what is on screen ----------------------------------------

/// What is on each lane at one timeline frame.
struct Under {
    /// Address of the top (first) video clip at the frame, if any.
    top_addr: Option<String>,
    /// That clip's source description, `src:0 interview.mp4 @ 15.000s`.
    top_desc: Option<String>,
    /// One line per media lane: `A1  A1@0 src:0 ... `, or `V2  gap`.
    lines: Vec<String>,
    /// Names of the hidden picture lanes that have a clip at the frame: their clips are there
    /// but not drawn, so the frame is not a gap.
    hidden_here: Vec<String>,
    structured: Vec<Value>,
}

fn describe_timeline(s: &PlaybackSession, at: u32) -> Under {
    let mut under = Under { top_addr: None, top_desc: None, lines: Vec::new(), hidden_here: Vec::new(), structured: Vec::new() };
    // Caption lanes are not picture or sound: what they draw is reported as its own line.
    for lane in s.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle) {
        let name = parse::lane_name(lane);
        let clips = s.lane_clips(lane);
        // A hidden picture lane is not drawn, so it is marked and cannot be the picture on top.
        let hidden = lane.kind == LaneKind::Video && s.lane_hidden(lane);
        match clips.iter().find(|c| c.start <= at && at < c.end()) {
            Some(c) => {
                let addr = parse::addr(lane, c.start);
                let desc = clip_desc(s, c, at);
                if lane.kind == LaneKind::Video && !hidden && under.top_addr.is_none() {
                    under.top_addr = Some(addr.clone());
                    under.top_desc = Some(desc.clone());
                }
                let note = if hidden { " (hidden: not in the picture)" } else { "" };
                if hidden {
                    under.hidden_here.push(name.clone());
                }
                under.lines.push(format!("{name:<3} {addr} {desc}{note}"));
                under.structured.push(json!({"lane": name, "clip": addr, "desc": desc, "hidden": hidden}));
            }
            None => {
                under.lines.push(format!("{name:<3} gap"));
                under.structured.push(json!({"lane": name, "clip": null}));
            }
        }
    }
    under
}

/// What stacks at `at` on the picture lanes, read the way the composite reads it: the
/// visible video clips covering the frame, topmost first, and a dissolve in progress. A
/// dissolve blends only where one clip covers the frame (a layered picture is not blended),
/// and its window is the tail of the clip, as the export computes it: the frames from the
/// clip's end back, bounded by the transition, both clips' lengths and the span the frame
/// sits in, which starts where a higher lane last took over.
fn layer_notes(s: &PlaybackSession, at: u32) -> (Vec<String>, Value) {
    let visible: Vec<Lane> = s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video && !s.lane_hidden(*l)).collect();
    let covering: Vec<(usize, usize)> = visible
        .iter()
        .enumerate()
        .filter_map(|(pos, &l)| s.lane_clips(l).iter().position(|c| c.start <= at && at < c.end()).map(|i| (pos, i)))
        .collect();
    let addr = |pos: usize, idx: usize| parse::addr(visible[pos], s.lane_clips(visible[pos])[idx].start);
    let mut lines = Vec::new();
    let layers: Vec<String> = covering.iter().rev().map(|&(pos, idx)| addr(pos, idx)).collect();
    let mut dissolve = Value::Null;
    if covering.len() >= 2 {
        lines.push(format!(
            "layered: {} (the topmost shows; the rest sit beneath it)",
            layers.join(" over ")
        ));
    } else if let Some(&(pos, idx)) = covering.first() {
        let lane = visible[pos];
        let clips = s.lane_clips(lane);
        let a = &clips[idx];
        let tout = s.transition_out_of(lane, idx);
        if let (true, Some(b)) = (tout > 0, clips.get(idx + 1).filter(|b| b.start == a.end())) {
            // The span the frame sits in starts where a higher lane last took over before it,
            // and ends where one takes over after it; a takeover before A's end means no tail.
            let higher = &visible[pos + 1..];
            let span_start = higher
                .iter()
                .flat_map(|&h| s.lane_clips(h).iter())
                .map(|c| c.end())
                .filter(|&e| a.start < e && e <= at)
                .fold(a.start, u32::max);
            let takes_over = higher
                .iter()
                .flat_map(|&h| s.lane_clips(h).iter())
                .any(|c| at < c.start && c.start < a.end());
            let window = tout.min(a.frames()).min(b.frames()).min(a.end() - span_start);
            let tail = a.end().saturating_sub(window);
            if !takes_over && window > 0 && at >= tail {
                let weight = f64::from(at - tail + 1) / f64::from(window + 1) * 100.0;
                let name = parse::lane_name(lane);
                let (from, to) = (addr(pos, idx), parse::addr(lane, b.start));
                let kind = a.transition_kind;
                let what = if kind == engine::transitions::TransitionKind::Dissolve {
                    "dissolving".to_string()
                } else {
                    format!("{} transition", kind.name())
                };
                lines.push(format!("{name} {what} {from}→{to}, {weight:.0}% (B's share of the blend)"));
                dissolve = json!({"from": from, "to": to, "window": window, "b_percent": weight.round(), "kind": kind.name()});
            }
        }
    }
    (lines, json!({"layers": layers, "dissolve": dissolve}))
}

/// The note for a frame whose only clips are on hidden picture lanes: `V1 is hidden: not in
/// the picture`, or `V1 and V2 are hidden: not in the picture`. None when no hidden lane has one.
fn hidden_only_text(names: &[String]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(format!("{one} is hidden: not in the picture")),
        [init @ .., last] => Some(format!("{} and {last} are hidden: not in the picture", init.join(", "))),
    }
}

/// Whether any video clip is placed on the timeline: without one the picture is black.
fn has_picture(s: &PlaybackSession) -> bool {
    s.lanes().into_iter().any(|l| l.kind == LaneKind::Video && !s.lane_clips(l).is_empty())
}

/// `src:0 interview.mp4 @ 15.000s`: where in its source a clip shows `at`. A clip's
/// `in_frame` and the frame `at` lands on both count timeline frames of its source;
/// the decoder maps that to the file's own frame (`floor(frame * file / timeline)`,
/// as playback does), and the time is that source frame over the file's own rate.
fn clip_desc(s: &PlaybackSession, c: &Clip, at: u32) -> String {
    let src = s.sources().get(c.source);
    let name = src.map(|src| file_name(&src.path)).unwrap_or_else(|| "?".to_string());
    let timeline_frame = c.in_frame + c.source_offset(at - c.start);
    let secs = match src {
        Some(src) => {
            // Micro-frames per second, as the edit tools convert between the two rates.
            let micro = |fps: f64| (fps * 1e6).round().max(1.0) as u128;
            let file = micro(s.file_fps(&src.path));
            let tl = micro(parse::session_fps(s));
            let frame = (u128::from(timeline_frame) * file / tl) as u32;
            f64::from(frame) / (file as f64 / 1e6)
        }
        None => parse::secs(timeline_frame, parse::session_fps(s)),
    };
    format!("src:{} {name} @ {secs:.3}s", c.source)
}

// ---- helpers --------------------------------------------------------------

/// The timeline frame range from `range.from`/`range.to`, defaulting to the whole.
fn range_of(args: &Value, fps: f64, total: u32) -> Result<(u32, u32), Reply> {
    let range = args.get("range").filter(|v| !v.is_null());
    let from = match range.and_then(|r| r.get("from")).filter(|v| !v.is_null()) {
        Some(v) => parse::frame(v, fps, total).map_err(Reply::err)?,
        None => 0,
    };
    let to = match range.and_then(|r| r.get("to")).filter(|v| !v.is_null()) {
        Some(v) => parse::frame(v, fps, total).map_err(Reply::err)?,
        None => total,
    };
    if to <= from {
        return Err(Reply::err(format!("range {from}f–{to}f is empty; `to` must be after `from`")));
    }
    if to > total {
        return Err(Reply::err(format!("range ends at {to}f, past the end ({total} frames)")));
    }
    Ok((from, to))
}

fn width_arg(args: &Value, default: u32, max: u32) -> Result<u32, Reply> {
    bounded(args, "width", default, 1, max)
}

/// A whole-number argument in `min..=max`, or `default` when it is absent. A value
/// that is not a whole number, or is outside the range, is refused by name: never
/// taken as the default.
fn bounded(args: &Value, name: &str, default: u32, min: u32, max: u32) -> Result<u32, Reply> {
    let Some(raw) = args.get(name).filter(|v| !v.is_null()) else {
        return Ok(default);
    };
    let Some((v, shown)) = whole_number(raw) else {
        return Err(Reply::err(format!("argument `{name}` must be a whole number from {min} to {max}, not {raw}")));
    };
    if v < f64::from(min) {
        Err(Reply::err(format!("argument `{name}` {shown} is too small: {min} to {max}")))
    } else if v > f64::from(max) {
        Err(Reply::err(format!("argument `{name}` {shown} is too large: {min} to {max}")))
    } else {
        Ok(v as u32)
    }
}

/// A whole-number JSON value as a float (for the range test) and as plain text. An
/// integral float such as `1000000000.0` counts, and prints without an exponent.
fn whole_number(raw: &Value) -> Option<(f64, String)> {
    if let Some(v) = raw.as_i64() {
        return Some((v as f64, v.to_string()));
    }
    if let Some(u) = raw.as_u64() {
        return Some((u as f64, u.to_string()));
    }
    let f = raw.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0)?;
    Some((f, format!("{f:.0}")))
}

/// `src:2` (or `src:2 interview.mp4`) to an index into the session's sources.
fn source_index(s: &PlaybackSession, raw: &str) -> Result<usize, Reply> {
    let head = raw.split_whitespace().next().unwrap_or("");
    let n = head
        .strip_prefix("src:")
        .and_then(|d| d.parse::<usize>().ok())
        .ok_or_else(|| Reply::err(format!("argument `source`: `{raw}` is not src:<n>, such as src:0")))?;
    if n >= s.sources().len() {
        return Err(Reply::err(format!(
            "no source src:{n}; the project has {} source(s)",
            s.sources().len()
        )));
    }
    Ok(n)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
}

fn probe_fps(path: &Path) -> Option<f64> {
    engine::probe_media(path).video.and_then(|v| v.frame_rate).filter(|f| *f > 0.0)
}

fn probe_secs(path: &Path) -> Option<f64> {
    engine::probe_media(path).duration_secs.filter(|d| *d > 0.0)
}

/// Most frame PNGs kept in the cache, across every project; the oldest are removed first.
const FRAME_CACHE_KEEP: usize = 200;
/// Most bytes of frame PNGs kept in the cache, across every project; the oldest go first.
const FRAME_CACHE_BYTES: u64 = 200 * 1024 * 1024;
const SCRATCH_PREFIX: &str = "edith_mcp-";

/// `$XDG_CACHE_HOME/edith/frames`, or `~/.cache/edith/frames`. Saved frames live here
/// and outlive the process, so the path an agent is told stays valid.
fn frame_cache_root() -> PathBuf {
    frame_cache_base().join("edith").join("frames")
}

#[cfg(not(test))]
fn frame_cache_base() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| std::env::temp_dir().join("edith-cache"))
}

/// Tests never touch the real cache. Each test thread gets its own folder under the temp
/// dir, removed when the thread ends, so a panicking test leaves nothing behind either.
#[cfg(test)]
fn frame_cache_base() -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static BASE: crate::testutil::TestDir = crate::testutil::TestDir::new(&format!(
            "frame-cache-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    }
    BASE.with(|b| b.path().to_path_buf())
}

/// `<cache>/<project stem or 'untitled'>`: the folder one project's frames go in.
pub(crate) fn frames_dir(project: Option<&Path>) -> PathBuf {
    let stem = project
        .and_then(|p| p.file_stem())
        .map(|s| safe_component(&s.to_string_lossy()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "untitled".to_string());
    frame_cache_root().join(stem)
}

/// A name as one path component: anything but letters, digits, `-`, `_` and `.` becomes
/// `_`, and leading dots are dropped so it cannot climb out of the cache.
fn safe_component(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' })
        .collect();
    mapped.trim_start_matches('.').to_string()
}

/// `<temp>/edith_mcp-<pid>`: this process's temp root for the scratch files that are not
/// frames (the scene sheets). Frames live in the persistent cache instead.
pub(crate) fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(format!("{SCRATCH_PREFIX}{}", std::process::id()))
}

/// The exit hook `main` calls. Removes only this process's temp root, which holds no
/// frames, so a frame path handed to the agent is never deleted here.
pub fn remove_scratch() {
    let _ = std::fs::remove_dir_all(scratch_root());
}

/// Removes the scratch folders of processes that are no longer alive, so a crash does
/// not leave PNGs behind. Called once at startup. Does nothing without `/proc`, where
/// liveness cannot be told.
pub fn sweep_stale_scratch() {
    sweep_in(&std::env::temp_dir(), std::process::id());
}

fn sweep_in(temp: &Path, own_pid: u32) {
    if !Path::new("/proc/self").exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(SCRATCH_PREFIX))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && pid != own_pid && !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Keeps the cache within its caps: removes the oldest PNGs under the project folders of
/// `root` until at most `keep` files and `max_bytes` bytes remain.
fn prune_cache(root: &Path, keep: usize, max_bytes: u64) {
    let Ok(projects) = std::fs::read_dir(root) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for project in projects.flatten() {
        let Ok(inner) = std::fs::read_dir(project.path()) else { continue };
        for f in inner.flatten() {
            if let Ok(m) = f.metadata()
                && m.is_file()
            {
                files.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), f.path()));
            }
        }
    }
    // Newest first: keep while under both caps, remove everything older than the first miss.
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut total: u64 = 0;
    for (i, (_, len, path)) in files.iter().enumerate() {
        total = total.saturating_add(*len);
        if i >= keep || total > max_bytes {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A short tag of the PNG's bytes (FNV-1a, 12 hex digits). It goes in every cache name,
/// so the same moment of a different project state gets a new file instead of
/// overwriting the one an agent was already given.
fn content_tag(png: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in png {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// Writes the PNG to `dir` (and to `extra` if given); returns the path written under `dir`.
/// The name gets the content tag before `.png`, so `12-960.png` is written as
/// `12-960-3fa9c0d2e1b7.png`.
pub(crate) fn save_png(png: &[u8], dir: &Path, name: &str, extra: Option<&Path>) -> Result<PathBuf, Reply> {
    std::fs::create_dir_all(dir)
        .map_err(|e| Reply::err(format!("cannot create {}: {e}", dir.display())))?;
    let stem = name.strip_suffix(".png").unwrap_or(name);
    let path = dir.join(format!("{stem}-{}.png", content_tag(png)));
    std::fs::write(&path, png)
        .map_err(|e| Reply::err(format!("cannot write {}: {e}", path.display())))?;
    prune_cache(&frame_cache_root(), FRAME_CACHE_KEEP, FRAME_CACHE_BYTES);
    if let Some(extra) = extra {
        if let Some(parent) = extra.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| Reply::err(format!("cannot create {}: {e}", parent.display())))?;
        }
        std::fs::write(extra, png)
            .map_err(|e| Reply::err(format!("cannot write {}: {e}", extra.display())))?;
    }
    Ok(path)
}

/// Runs `f(0..n)` on up to the machine's parallelism, returning results in index order.
fn parallel<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let workers = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .clamp(1, n.max(1));
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, T)> = std::thread::scope(|sc| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                sc.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        mine.push((i, f(i)));
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("contact sheet worker panicked"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, t)| t).collect()
}

pub(crate) fn fit_height(f: &Frame, width: u32) -> u32 {
    let h = (u64::from(f.height) * u64::from(width) + u64::from(f.width) / 2) / u64::from(f.width.max(1));
    (h as u32).max(1)
}

/// Box-filter resize of a BGRA frame: each output pixel averages its source block.
pub(crate) fn resize(src: &Frame, w: u32, h: u32) -> Frame {
    let (sw, sh) = (u64::from(src.width), u64::from(src.height));
    let (dw, dh) = (u64::from(w), u64::from(h));
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..dh {
        let y0 = y * sh / dh;
        let y1 = ((y + 1) * sh / dh).max(y0 + 1).min(sh);
        for x in 0..dw {
            let x0 = x * sw / dw;
            let x1 = ((x + 1) * sw / dw).max(x0 + 1).min(sw);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = ((sy * sw + sx) * 4) as usize;
                    for (acc, &b) in sum.iter_mut().zip(&src.bgra[p..p + 4]) {
                        *acc += u64::from(b);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (sum[c] / n) as u8;
            }
        }
    }
    Frame { index: src.index, width: w, height: h, bgra: out }
}

/// A BGRA canvas that tiles are blitted onto and text is stamped on.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: [u8; 4]) -> Self {
        let px = fill.iter().copied().cycle().take((w * h * 4) as usize).collect();
        Self { w, h, px }
    }

    fn put(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) {
            return;
        }
        let o = ((y as usize) * self.w as usize + x as usize) * 4;
        self.px[o..o + 4].copy_from_slice(&c);
    }

    /// Copies `f` with its top-left corner at (x0, y0), clipped to the canvas.
    fn blit(&mut self, f: &Frame, x0: u32, y0: u32) {
        let w = f.width.min(self.w.saturating_sub(x0));
        let h = f.height.min(self.h.saturating_sub(y0));
        for y in 0..h {
            let s = (y * f.width * 4) as usize;
            let d = (((y0 + y) * self.w + x0) * 4) as usize;
            let n = (w * 4) as usize;
            self.px[d..d + n].copy_from_slice(&f.bgra[s..s + n]);
        }
    }

    fn into_frame(self) -> Frame {
        Frame { index: 0, width: self.w, height: self.h, bgra: self.px }
    }
}

/// 5x7 glyphs for the timecode alphabet; `#` is a lit pixel.
const GLYPHS: [(char, [&str; 7]); 13] = [
    ('0', [".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."]),
    ('1', ["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###."]),
    ('2', [".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####"]),
    ('3', ["#####", "...#.", "..#..", "...#.", "....#", "#...#", ".###."]),
    ('4', ["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."]),
    ('5', ["#####", "#....", "####.", "....#", "....#", "#...#", ".###."]),
    ('6', ["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."]),
    ('7', ["#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#..."]),
    ('8', [".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."]),
    ('9', [".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."]),
    (':', [".....", "..#..", "..#..", ".....", "..#..", "..#..", "....."]),
    ('.', [".....", ".....", ".....", ".....", ".....", ".##..", ".##.."]),
    ('f', ["..##.", ".#..#", ".#...", "###..", ".#...", ".#...", ".#..."]),
];

/// Draws `text` at (x, y) in white with a 1-pixel black outline, at `scale` pixels per
/// glyph pixel. Characters outside the alphabet, spaces included, advance the pen blank.
fn draw_text_at(c: &mut Canvas, x: i64, y: i64, text: &str, scale: i64) {
    let mut lit: Vec<(i64, i64)> = Vec::new();
    let mut pen = x;
    for ch in text.chars() {
        if let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == ch) {
            for (ry, row) in rows.iter().enumerate() {
                for (rx, b) in row.bytes().enumerate() {
                    if b != b'#' {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            lit.push((pen + rx as i64 * scale + sx, y + ry as i64 * scale + sy));
                        }
                    }
                }
            }
        }
        pen += 6 * scale;
    }
    for &(px, py) in &lit {
        for dy in -1..=1 {
            for dx in -1..=1 {
                c.put(px + dx, py + dy, BLACK);
            }
        }
    }
    for &(px, py) in &lit {
        c.put(px, py, WHITE);
    }
}

/// Standard base64 (RFC 4648, padded). No dependency.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use engine::project::Lane;
    use std::time::Instant;

    fn server() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(p).expect("open test asset"));
        srv
    }

    /// The source time a frame of a ramped clip names is the ramp's integral, not a single
    /// rate: 1x to 2x over 60 frames has played 90 source frames at frame 60.
    #[test]
    fn clip_desc_names_the_source_time_through_a_ramp() {
        let mut srv = server();
        let s = srv.session.as_mut().unwrap();
        let keys = [
            engine::project::SpeedKey { at: 0, speed: engine::project::Speed::NORMAL },
            engine::project::SpeedKey { at: 60, speed: engine::project::Speed::from_permille(2000) },
        ];
        s.set_speed_ramp(Lane::V1, 0, &keys).expect("the ramp sets");
        let c = s.lane_clips(Lane::V1)[0].clone();
        let fps = parse::session_fps(s);
        let want = format!("@ {:.3}s", f64::from(c.in_frame + 90) / fps);
        let got = clip_desc(s, &c, c.start + 60);
        assert!(got.ends_with(&want), "{got} should end with {want}");
    }

    /// Calls any registered tool, so a test can set up a project with `edit`.
    fn call(srv: &mut Server, name: &str, args: Value) -> Reply {
        let tool = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool exists");
        (tool.run)(srv, &args)
    }

    /// Decodes standard base64 (test only).
    fn unbase64(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0;
        for ch in s.bytes().filter(|&b| b != b'=') {
            let v = match ch {
                b'A'..=b'Z' => ch - b'A',
                b'a'..=b'z' => ch - b'a' + 26,
                b'0'..=b'9' => ch - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("bad base64 byte {ch}"),
            };
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    /// (width, height) from a PNG's IHDR chunk.
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
        (w, h)
    }

    /// scopes reads the graded frame: a warm LUT puts red above blue in the parade
    /// and turns the cast toward red; a missing LUT is named and not applied.
    #[test]
    fn scopes_read_the_graded_frame_and_name_a_missing_lut() {
        let mut srv = server();
        let before = call(&mut srv, "scopes", json!({"at": "0.5s"}));
        assert!(!before.is_error, "{}", before.text);
        assert!(before.text.contains("waveform (luma %") && before.text.contains("parade: red") && before.text.contains("vectorscope:"), "{}", before.text);
        let b = before.structured.clone().expect("structured");
        let dir = std::env::temp_dir().join(format!("edith-scopes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cube = dir.join("warm.cube");
        // Red lifted hard, blue pulled down.
        std::fs::write(&cube, "LUT_3D_SIZE 2\n0.3 0 0\n1 0 0\n0.3 1 0\n1 1 0\n0.3 0 0.6\n1 0 0.6\n0.3 1 0.6\n1 1 0.6\n").unwrap();
        let r = call(&mut srv, "edit", json!({"ops": [{"op": "lut", "clip": "V1#0", "path": cube.display().to_string()}]}));
        assert!(!r.is_error, "{}", r.text);
        let after = call(&mut srv, "scopes", json!({"at": "0.5s", "bands": 2}));
        let a = after.structured.clone().expect("structured");
        let mean = |v: &Value, c: &str| v["parade"][c]["mean"].as_f64().unwrap();
        assert!(mean(&a, "red") - mean(&a, "blue") > mean(&b, "red") - mean(&b, "blue") + 10.0, "{}\n{}", before.text, after.text);
        assert_eq!(a["waveform_bands"].as_array().unwrap().len(), 2);
        std::fs::remove_file(&cube).unwrap();
        // Re-read from disk on the next open of the path: a missing file is named.
        engine::cube::LutRef::reload(&cube);
        let gone = call(&mut srv, "scopes", json!({"at": "0.5s"}));
        assert!(gone.text.contains("LUT not applied"), "{}", gone.text);
        let status = call(&mut srv, "project_status", json!({}));
        assert!(status.text.contains("LUT not applied") && status.text.contains("relink"), "{}", status.text);
        assert!(call(&mut srv, "scopes", json!({"at": "0.5s", "bands": 0})).is_error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
        assert_eq!(unbase64(&base64(&[0, 1, 2, 254, 255])), vec![0, 1, 2, 254, 255]);
    }

    #[test]
    fn glyphs_draw_pixels() {
        let mut c = Canvas::new(120, 24, BLACK);
        draw_text_at(&mut c, 2, 2, "01:02.500", SCALE);
        let lit = c.px.chunks(4).filter(|p| p[0] == 255).count();
        assert!(lit > 100, "only {lit} white pixels");
        let mut one = Canvas::new(40, 24, BLACK);
        draw_text_at(&mut one, 2, 2, "0123456789:.f", SCALE);
        assert!(one.px.chunks(4).any(|p| p[0] == 255));
    }

    /// A folder of a dead pid goes; this process's own, and a live one, stay.
    #[cfg(target_os = "linux")]
    #[test]
    fn sweep_removes_only_dead_processes_scratch() {
        let temp = TestDir::new("sweep");
        let dead = temp.join("edith_mcp-4294967290");
        let own = temp.join(format!("edith_mcp-{}", std::process::id()));
        let other = temp.join("edith_mcp-main-1");
        for d in [&dead, &own, &other] {
            std::fs::create_dir_all(d.join("frames")).unwrap();
        }
        sweep_in(&temp, std::process::id());
        assert!(!dead.exists(), "a dead process's scratch is removed");
        assert!(own.exists(), "our own scratch is kept");
        assert!(other.exists(), "names that are not a pid are left alone");
    }

    #[test]
    fn same_moment_of_two_states_keeps_two_files() {
        // Two frames at one time with different pictures must not share a cache name.
        let dir = TestDir::new("same-moment");
        let a = save_png(b"state one", &dir, "150-640.png", None).ok().expect("saved");
        let b = save_png(b"state two", &dir, "150-640.png", None).ok().expect("saved");
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"state one");
        assert_eq!(std::fs::read(&b).unwrap(), b"state two");
        let again = save_png(b"state one", &dir, "150-640.png", None).ok().expect("saved");
        assert_eq!(again, a, "the same bytes reuse the same file");
    }

    #[test]
    fn prune_keeps_the_newest_files() {
        let root = TestDir::new("prune");
        let frames = root.join("frames");
        std::fs::create_dir_all(&frames).unwrap();
        for i in 0..12 {
            let p = frames.join(format!("{i}.png"));
            std::fs::write(&p, b"png").unwrap();
            // Older for smaller i, so the sort by modification time is the name order.
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(when).unwrap();
        }
        prune_cache(&root, 5, u64::MAX);
        let mut left: Vec<String> = std::fs::read_dir(&frames)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["10.png", "11.png", "7.png", "8.png", "9.png"]);
    }

    #[test]
    fn prune_stops_at_the_byte_cap() {
        let root = TestDir::new("prune-bytes");
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        for i in 0..4u64 {
            let p = proj.join(format!("{i}.png"));
            std::fs::write(&p, vec![0u8; 100]).unwrap();
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(when).unwrap();
        }
        // Room for two 100-byte files: the two newest stay.
        prune_cache(&root, 500, 250);
        let mut left: Vec<String> = std::fs::read_dir(&proj)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["2.png", "3.png"]);
    }

    #[test]
    fn frames_dir_is_one_safe_folder_per_project() {
        let name = |p: Option<&Path>| frames_dir(p).file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(name(Some(Path::new("/x/My Edit (2).edith"))), "My_Edit__2_");
        assert_eq!(name(None), "untitled");
        assert_eq!(name(Some(Path::new("/x/..."))), "untitled");
        assert_eq!(safe_component("../../etc"), "_.._etc");
    }

    #[test]
    fn a_frame_reply_names_the_frame_seconds_rounded_to() {
        // 0.5 s at 25 fps is 12.5 frames: the frame shown is 13, and the reply says so.
        assert_eq!(rounded_frame(&json!({"at": "0.5s"}), 25.0, 13), "0.5s → 13f");
        assert_eq!(rounded_frame(&json!({"at": 0.5}), 25.0, 13), "0.5s → 13f");
        assert_eq!(rounded_frame(&json!({"at": "1s"}), 25.0, 25), "25f");
        assert_eq!(rounded_frame(&json!({"at": "13f"}), 25.0, 13), "13f");
        assert_eq!(rounded_frame(&json!({"at": "00:00:00:13"}), 25.0, 13), "13f");
    }

    /// A size preview draws the frame at the export's picture size, scaled to the width asked, and
    /// the reply says it is a preview of that export; the project keeps its own size.
    #[test]
    fn a_size_preview_is_the_export_size_and_says_so() {
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "size": "1080x1920", "width": 270}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(png_size(&unbase64(&r.images[0].1)), (270, 480), "{}", r.text);
        assert!(r.text.contains("preview of the export at 1080x1920 (fit keep)"), "{}", r.text);
        assert!(r.text.contains("the project is unchanged"), "{}", r.text);
        assert!(r.text.contains("preview 270x480 (the export is 1080x1920)"), "{}", r.text);
        let plain = call(&mut srv, "frame", json!({"at": "1.0s", "width": 270}));
        assert!(!plain.is_error, "{}", plain.text);
        assert!(!plain.text.contains("preview of the export"), "{}", plain.text);
        assert_eq!(png_size(&unbase64(&plain.images[0].1)).0, 270);
    }

    /// `fit` on a size preview puts the full-frame clips on that policy and says how many; without
    /// a size the export is the project's own picture size.
    #[test]
    fn a_fit_preview_names_the_fit_and_the_clips_it_reached() {
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "size": "720x1280", "fit": "fill", "width": 180}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(fit fill, 1 full-frame clip(s) on it"), "{}", r.text);
        assert_eq!(png_size(&unbase64(&r.images[0].1)), (180, 320), "{}", r.text);
        let fit_only = call(&mut srv, "frame", json!({"at": "1.0s", "fit": "fit", "width": 320}));
        assert!(!fit_only.is_error, "{}", fit_only.text);
        assert!(fit_only.text.contains("preview of the export at 1280x720 (fit fit"), "{}", fit_only.text);
    }

    #[test]
    fn a_size_or_fit_refuses_a_source_and_a_bad_value() {
        let mut srv = server();
        let on_source = call(&mut srv, "frame", json!({"at": "0.5s", "source": "src:0", "size": "720p"}));
        assert!(on_source.is_error, "{}", on_source.text);
        assert!(on_source.text.contains("cannot be used with source"), "{}", on_source.text);
        let bad_fit = call(&mut srv, "frame", json!({"at": "0.5s", "fit": "stretch"}));
        assert!(bad_fit.is_error, "{}", bad_fit.text);
        let odd = call(&mut srv, "frame", json!({"at": "0.5s", "size": "161x120"}));
        assert!(odd.is_error, "{}", odd.text);
    }

    #[test]
    fn frame_returns_one_png_block_at_the_width_asked() {
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s"}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].0, "image/png");
        let png = unbase64(&r.images[0].1);
        assert_eq!(png_size(&png).0, 640);
        assert!(r.text.contains("V1@"), "{}", r.text);
        assert!(r.text.contains("src:0 test_av.mp4"), "{}", r.text);
        assert!(r.text.contains("640x"), "{}", r.text);
        let path = r.structured.as_ref().unwrap()["path"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read(&path).unwrap(), png);
    }

    #[test]
    fn frame_from_a_source_is_scaled_to_width() {
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "source": "src:0", "width": 320}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(png_size(&unbase64(&r.images[0].1)).0, 320);
    }

    #[test]
    fn a_frame_past_a_sources_length_says_the_length_and_the_last_time() {
        engine::ao::set_silent_output();
        // A 2 s picture (60 frames at 30 fps) with 60 s of sound: the picture ends first.
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_short_video_long_audio.mp4");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(p).expect("open test asset"));
        let r = call(&mut srv, "frame", json!({"at": "2.5s", "source": "src:0"}));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(
            r.text,
            "src:0 is 2.0s (60f) long; frame 75 (2.5s) is past its end — ask for at most 1.97s"
        );
        assert!(!r.text.contains("decoder"), "{}", r.text);
        // Inside the length the frame still comes back.
        let inside = call(&mut srv, "frame", json!({"at": "1.0s", "source": "src:0"}));
        assert!(!inside.is_error, "{}", inside.text);
    }

    #[test]
    fn seconds_in_plain_words_keep_one_or_two_decimals() {
        assert_eq!(secs_text(1.0), "1.0s");
        assert_eq!(secs_text(0.5), "0.5s");
        assert_eq!(secs_text(0.533), "0.53s");
        assert_eq!(secs_text(2.0), "2.0s");
    }

    #[test]
    fn frame_names_audio_only_sources_and_shows_stills() {
        let mut srv = server();
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let s = srv.session.as_mut().unwrap();
        for name in ["test_tone.aac", "test_tone.opus", "test_tone_cover.mpeg"] {
            s.import(&assets.join(name)).expect("import the sound");
        }
        s.import(&assets.join("test_still.png")).expect("import the still");
        // src:0 is test_av.mp4; src:1 to src:3 are sound, src:4 the still.
        for n in 1..=4 {
            let r = call(&mut srv, "frame", json!({"at": "0s", "source": format!("src:{n}")}));
            if n < 4 {
                assert!(r.is_error, "{}", r.text);
                assert!(r.text.starts_with("no picture: test_tone"), "{}", r.text);
                assert!(r.text.contains("audio only"), "{}", r.text);
                assert!(!r.text.contains("box"), "no decode is attempted: {}", r.text);
            } else {
                assert!(!r.is_error, "{}", r.text);
                assert!(!r.text.contains("no picture"), "a still is a picture: {}", r.text);
                assert_eq!(r.images.len(), 1);
            }
        }
    }

    /// A frame or contact sheet over an offline clip draws that picture missing and names the
    /// source with the fix, with no OS error; the other tiles still compose. A source frame of
    /// the offline file is refused by name.
    #[test]
    fn an_offline_clip_is_drawn_missing_and_named() {
        let td = TestDir::new("see-offline");
        let gone = td.path().join("gone.mp4");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4"), &gone)
            .expect("copy");
        let project = td.path().join("offline.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [gone]})).is_error);
        assert!(!call(
            &mut server,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]})
        )
        .is_error);
        assert!(!call(&mut server, "project_save", json!({})).is_error);
        std::fs::remove_file(&gone).expect("remove");

        let mut opened = Server::new();
        assert!(!call(&mut opened, "project_open", json!({"path": project})).is_error);
        let note = "is offline; its picture is missing here — relink with media_relink";

        let r = call(&mut opened, "frame", json!({"at": "1.0s", "width": 320}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src:0 ") && r.text.contains(note), "{}", r.text);
        assert!(!r.text.contains("No such file"), "{}", r.text);
        assert_eq!(r.images.len(), 1);
        assert_eq!(png_size(&unbase64(&r.images[0].1)).0, 320, "the missing picture is drawn at the width asked");

        let sheet = call(&mut opened, "contact_sheet", json!({"count": 3, "cols": 3, "width": 480}));
        assert!(!sheet.is_error, "{}", sheet.text);
        assert!(sheet.text.contains(note), "{}", sheet.text);
        assert!(!sheet.text.contains("No such file"), "{}", sheet.text);

        let src = call(&mut opened, "frame", json!({"source": "src:0", "at": 0}));
        assert!(src.is_error && src.text.starts_with("src:0 ") && src.text.contains(note), "{}", src.text);
    }

    #[test]
    fn frame_refuses_bad_width_and_unknown_source() {
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": 10, "width": 4000}));
        assert!(r.is_error && r.text.contains("width"), "{}", r.text);
        let r = call(&mut srv, "frame", json!({"at": 10, "source": "src:9"}));
        assert!(r.is_error && r.text.contains("src:9"), "{}", r.text);
    }

    /// A session on one asset, as `server` does, with the project at `fps`.
    fn server_at(asset: &str, fps: f64) -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(asset);
        let mut srv = Server::new();
        let mut s = PlaybackSession::open(p).expect("open test asset");
        assert!(s.set_frame_rate(fps), "project set to {fps} fps");
        srv.session = Some(s);
        srv
    }

    #[test]
    fn frame_names_the_source_time_in_seconds_across_a_rate_change() {
        // A 30 fps source in a 25 fps project: 4.96 s is timeline frame 124, and the
        // source plays it at 4.96 s, not at 124 frames of its own 30 fps (4.133 s).
        let mut srv = server_at("test_av.mp4", 25.0);
        let r = call(&mut srv, "frame", json!({"at": "4.96s"}));
        assert!(!r.is_error, "{}", r.text);
        // Timeline frame 124 shows source frame floor(124 * 30 / 25) = 148, at 4.933 s.
        assert!(r.text.contains("V1@0 src:0 test_av.mp4 @ 4.933s"), "{}", r.text);

        // A 25 fps source in a 30 fps project: 1.96 s is timeline frame 59, which the
        // source shows at its own frame floor(59 * 25 / 30) = 49, at 1.960 s (not 59 of
        // its own 25 fps, 2.360 s, and not 59 / 30 = 1.967 s).
        let mut srv = server_at("test_25fps.mp4", 30.0);
        let r = call(&mut srv, "frame", json!({"at": "1.96s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V1@0 src:0 test_25fps.mp4 @ 1.960s"), "{}", r.text);
    }

    #[test]
    fn source_frame_rounds_seconds_with_the_source_fps() {
        // 0.5 s at 25 fps is 12.5 frames: it rounds up to 13, which is 0.520 s, and the
        // reply says both. The project fps (25 here) plays no part in a source time.
        let mut srv = server_at("test_av.mp4", 25.0);
        let r = call(&mut srv, "frame", json!({"at": "0.5s", "source": "src:0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("· 00:00.500 (15f)"), "{}", r.text);

        let mut srv = server_at("test_25fps.mp4", 30.0);
        let r = call(&mut srv, "frame", json!({"at": "0.5s", "source": "src:0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("· 00:00.520 (0.5s → 13f)"), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["frame"], json!(13));

        // An exact frame count names no rounding.
        let r = call(&mut srv, "frame", json!({"at": "13f", "source": "src:0"}));
        assert!(r.text.contains("· 00:00.520 (13f)"), "{}", r.text);
    }

    #[test]
    fn timeline_frame_follows_the_project_fps_not_the_source() {
        // In a 30 fps project 0.5 s is timeline frame 15, whatever the 25 fps source is.
        let mut srv = server_at("test_25fps.mp4", 30.0);
        let placed = call(&mut srv, "edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "2s"]]}]}));
        assert!(!placed.is_error, "{}", placed.text);
        let r = call(&mut srv, "frame", json!({"at": "0.5s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(15f)"), "{}", r.text);
        assert!(r.text.contains("src:0 test_25fps.mp4 @ 0.480s"), "{}", r.text);
    }

    #[test]
    fn frame_with_save_names_the_path_it_wrote() {
        let dir = TestDir::new("frame-save");
        let want = dir.join("picked.png");
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "save": want.display().to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("saved: {}", want.display())), "{}", r.text);
        let structured = r.structured.as_ref().unwrap();
        assert_eq!(structured["path"].as_str().unwrap(), want.display().to_string());
        assert_ne!(structured["cache_path"].as_str().unwrap(), want.display().to_string());
        assert_eq!(std::fs::read(&want).unwrap(), unbase64(&r.images[0].1));

        let src_want = dir.join("source.png");
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "source": "src:0", "save": src_want.display().to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("saved: {}", src_want.display())), "{}", r.text);
        assert!(src_want.exists());
    }

    #[test]
    fn contact_sheet_save_names_the_path_it_wrote() {
        let dir = TestDir::new("sheet-save");
        let want = dir.join("sheet.png");
        let mut srv = server();
        let r = call(&mut srv, "contact_sheet", json!({"count": 4, "cols": 2, "save": want.display().to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("saved: {}", want.display())), "{}", r.text);
        assert_eq!(std::fs::read(&want).unwrap(), unbase64(&r.images[0].1));
    }

    #[test]
    fn contact_sheet_is_one_image_of_the_expected_size() {
        let mut srv = server();
        let r = call(&mut srv, "contact_sheet", json!({"count": 6, "cols": 3}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.images.len(), 1);
        let (w, h) = png_size(&unbase64(&r.images[0].1));
        let st = r.structured.as_ref().unwrap();
        let th = st["tile"]["h"].as_u64().unwrap() as u32;
        assert_eq!(w, 1280);
        assert_eq!(h, 2 * (th + LABEL_H) + GUTTER, "rows of tiles, each with its label band, one gutter between");
        assert!(r.text.contains("edit joins:"), "{}", r.text);
        assert_eq!(st["tiles"].as_array().unwrap().len(), 6);
    }

    /// `server`'s test_av on V1 at 0f, then test_25fps and test_av again on V1: a 12f
    /// dissolve (picture only) joins the first two, a hard cut the last two. Returns the
    /// frame where the dissolve's boundary falls.
    fn dissolve_then_cut() -> (Server, u32) {
        let mut srv = server();
        let second = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4");
        assert!(!call(&mut srv, "media_import", json!({"paths": [second.display().to_string()]})).is_error);
        let at = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].end();
        let placed = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:1", "lane": "V1", "at": format!("{at}f"), "audio": false}]}),
        );
        assert!(!placed.is_error, "{}", placed.text);
        let dissolved = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "dissolve", "clip": "V1@0", "frames": 12, "linked": false}]}),
        );
        assert!(!dissolved.is_error, "{}", dissolved.text);
        let end = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].end();
        let cut = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": format!("{end}f"), "audio": false}]}),
        );
        assert!(!cut.is_error, "{}", cut.text);
        (srv, at)
    }

    #[test]
    fn contact_sheet_lists_a_dissolve_apart_from_the_hard_cut() {
        let (mut srv, at) = dissolve_then_cut();
        let s = srv.session.as_ref().unwrap();
        let fps = parse::session_fps(s);
        let hard_at = s.lane_clips(Lane::V1)[2].start;
        let r = call(&mut srv, "contact_sheet", json!({"count": 6, "cols": 3}));
        assert!(!r.is_error, "{}", r.text);
        let want = format!("dissolve {:.2}s–{:.2}s", parse::secs(at - 12, fps), parse::secs(at, fps));
        assert!(r.text.contains(&want), "{}", r.text);
        let cut_line = r.text.lines().find(|l| l.starts_with("edit joins:")).expect("cuts line");
        assert!(cut_line.contains(&format!("{hard_at}f V1@")), "{cut_line}");
        assert!(!cut_line.contains(&format!(" {at}f ")), "the dissolve is not also a cut: {cut_line}");
        let st = r.structured.as_ref().unwrap();
        assert_eq!(st["cuts"].as_array().unwrap().len(), 1, "the hard cut is one entry naming both sides");
        assert!(st["cuts"][0]["text"].as_str().unwrap().contains("→V1@"), "{}", st["cuts"][0]);
        let d = &st["dissolves"].as_array().unwrap()[0];
        assert_eq!(d["frame"].as_u64().unwrap(), u64::from(at));
        assert_eq!(d["frames"].as_u64().unwrap(), 12);
    }

    #[test]
    fn contact_sheet_with_only_a_dissolve_in_range_says_no_cuts() {
        let (mut srv, at) = dissolve_then_cut();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let range = json!({"from": format!("{}f", at - 10), "to": format!("{}f", at + 30)});
        let r = call(&mut srv, "contact_sheet", json!({"count": 4, "cols": 2, "range": range}));
        assert!(!r.is_error, "{}", r.text);
        let want = format!("edit joins: none · dissolve {:.2}s–{:.2}s", parse::secs(at - 12, fps), parse::secs(at, fps));
        assert!(r.text.contains(&want), "{}", r.text);
    }

    /// A hard cut between two clips on one lane is one `ends→starts` entry, and a gap's end and
    /// start stay two entries.
    #[test]
    fn contact_sheet_merges_a_hard_cut_and_keeps_gaps_apart() {
        let mut srv = server();
        let end = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].end();
        let cut = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": format!("{end}f"), "audio": false}]}),
        );
        assert!(!cut.is_error, "{}", cut.text);
        let second = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start;
        assert_eq!(second, end, "the second clip starts where the first ends");
        // A one-second gap after it, so the next clip's end and start are not joined.
        let gap_at = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].end() + 30;
        let gapped = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": format!("{gap_at}f"), "audio": false}]}),
        );
        assert!(!gapped.is_error, "{}", gapped.text);
        let r = call(&mut srv, "contact_sheet", json!({"count": 4, "cols": 2}));
        assert!(!r.is_error, "{}", r.text);
        let cut_line = r.text.lines().find(|l| l.starts_with("edit joins:")).expect("cuts line");
        let merged = format!("{end}f V1@0→V1@{end}");
        assert!(cut_line.contains(&merged), "{cut_line}");
        assert!(!cut_line.contains(&format!("{end}f V1@0 ends")), "no separate end at the cut: {cut_line}");
        assert!(!cut_line.contains(&format!("{end}f V1@{end} starts")), "no separate start at the cut: {cut_line}");
        let gap_end = gap_at - 30;
        assert!(cut_line.contains(&format!("{gap_end}f V1@{end} ends")), "a gap's end stays: {cut_line}");
        assert!(cut_line.contains(&format!("{gap_at}f V1@{gap_at} starts")), "a gap's start stays: {cut_line}");
    }

    /// A clip with no joins in range: the line says so and points to scenes for the picture
    /// cuts inside the clip, which are not joins.
    #[test]
    fn contact_sheet_on_one_clip_points_to_scenes_for_its_picture_cuts() {
        let mut srv = server();
        let r = call(&mut srv, "contact_sheet", json!({"count": 4, "cols": 2}));
        assert!(!r.is_error, "{}", r.text);
        let join_line = r.text.lines().find(|l| l.starts_with("edit joins:")).expect("joins line");
        assert_eq!(join_line, "edit joins: none in range · picture cuts inside a clip: scenes V1@0", "{}", r.text);
    }

    /// A join on an audio lane is counted, not listed, and the count says it is an audio join.
    #[test]
    fn contact_sheet_counts_an_audio_join_as_a_join_on_audio_lanes() {
        let mut srv = server();
        let end = srv.session.as_ref().unwrap().lane_clips(Lane::A1)[0].end();
        let joined = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "place", "source": "src:0", "lane": "A1", "at": format!("{end}f")}]}),
        );
        assert!(!joined.is_error, "{}", joined.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::A1)[1].start, end, "the audio clip joins at {end}f");
        let r = call(&mut srv, "contact_sheet", json!({"count": 4, "cols": 2}));
        assert!(!r.is_error, "{}", r.text);
        let join_line = r.text.lines().find(|l| l.starts_with("edit joins:")).expect("joins line");
        assert!(join_line.contains(" · (1 join on audio lanes not shown) · "), "{join_line}");
        assert!(!r.text.contains("audio cuts"), "{}", r.text);
    }

    /// A source has no edit joins: the line says so and names scenes for its picture cuts.
    #[test]
    fn contact_sheet_of_a_source_says_it_has_no_edit_joins() {
        let mut srv = server();
        let r = call(&mut srv, "contact_sheet", json!({"source": "src:0", "count": 4, "cols": 2}));
        assert!(!r.is_error, "{}", r.text);
        let want = "edit joins: none (a source has no edit joins) · scenes src:0 finds its picture cuts";
        assert!(r.text.lines().any(|l| l == want), "{}", r.text);
    }

    /// An empty timeline says it is empty, not that the range runs past zero frames.
    #[test]
    fn contact_sheet_on_an_empty_timeline_says_the_timeline_is_empty() {
        let td = TestDir::new("r30-empty-sheet");
        let mut srv = Server::new();
        let made = call(&mut srv, "project_new", json!({"path": td.path().join("proj.edith"), "resolution": "1280x720"}));
        assert!(!made.is_error, "{}", made.text);
        let r = call(&mut srv, "contact_sheet", json!({"range": {"from": "12s", "to": "15s"}}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("the timeline is empty"), "{}", r.text);
        assert!(!r.text.contains("past the end"), "{}", r.text);
    }

    #[test]
    fn contact_sheet_refuses_a_bad_count() {
        let mut srv = server();
        let r = call(&mut srv, "contact_sheet", json!({"count": 49}));
        assert!(r.is_error && r.text.contains("count"), "{}", r.text);
    }

    #[test]
    fn gap_frame_is_black() {
        let mut srv = server();
        // Move the first clip (and its link group) to start at 500f, leaving a gap before it.
        let moved = call(
            &mut srv,
            "edit",
            json!({"ops": [{"op": "move", "clip": "V1@0", "at": "500f"}]}),
        );
        assert!(!moved.is_error, "{}", moved.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.gap_at(Lane::V1, 100).is_some(), "expected a gap at 100f");
        let r = call(&mut srv, "frame", json!({"at": "100f"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("gap ·"), "{}", r.text);
        let (project, meta) = srv.session.as_ref().unwrap().export_snapshot();
        let f = engine::export::compose_at(&project, &meta, 100, 64, 36).unwrap();
        let sum: u64 = f.bgra.chunks(4).map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum();
        let mean = sum / (f.bgra.len() as u64 / 4 * 3);
        assert!(mean < 16, "gap frame mean channel value {mean}");
    }

    #[test]
    fn a_frame_whose_only_picture_is_hidden_is_not_called_a_gap() {
        let mut srv = server();
        // V1 holds the clip at 0; hiding the lane leaves the frame with a clip there but no picture.
        assert!(srv.session.as_mut().unwrap().set_lane_hidden(Lane::V1, true));
        let r = call(&mut srv, "frame", json!({"at": "100f"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.starts_with("hidden lane only · black (V1 is hidden: not in the picture) · "),
            "{}",
            r.text
        );
        assert!(!r.text.contains("gap"), "{}", r.text);
        // The lane line still says the clip is there and not drawn.
        assert!(r.text.contains("V1  V1@0 src:0"), "{}", r.text);
        assert!(r.text.contains("(hidden: not in the picture)"), "{}", r.text);
        // A visible picture on another lane makes the frame a normal one again.
        assert!(srv.session.as_mut().unwrap().set_lane_hidden(Lane::V1, false));
        let shown = call(&mut srv, "frame", json!({"at": "100f"}));
        assert!(shown.text.starts_with("V1@0 "), "{}", shown.text);
    }

    #[test]
    fn hidden_only_text_names_one_lane_or_several() {
        assert_eq!(hidden_only_text(&[]), None);
        assert_eq!(hidden_only_text(&["V1".to_string()]).unwrap(), "V1 is hidden: not in the picture");
        let two = ["V1".to_string(), "V2".to_string()];
        assert_eq!(hidden_only_text(&two).unwrap(), "V1 and V2 are hidden: not in the picture");
        let three = ["V1".to_string(), "V2".to_string(), "V3".to_string()];
        assert_eq!(hidden_only_text(&three).unwrap(), "V1, V2 and V3 are hidden: not in the picture");
    }

    #[test]
    fn burn_captions_true_is_the_defaults_and_false_is_off() {
        let mut srv = server_with_subs();
        let plain = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640}));
        let defaults = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640, "burn_captions": {}}));
        let on = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640, "burn_captions": true}));
        let off = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640, "burn_captions": false}));
        assert!(!on.is_error, "{}", on.text);
        assert!(on.text.contains("captions burned (track 0)"), "{}", on.text);
        assert_eq!(unbase64(&on.images[0].1), unbase64(&defaults.images[0].1), "true is {{}}");
        assert!(!off.is_error, "{}", off.text);
        assert!(!off.text.contains("captions burned"), "{}", off.text);
        assert_eq!(unbase64(&off.images[0].1), unbase64(&plain.images[0].1), "false is no burn");
    }

    #[test]
    fn contact_sheet_of_a_still_is_one_tile() {
        let mut srv = server();
        let still = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_still.png");
        srv.session.as_mut().expect("session").import(&still).expect("import the still");
        let r = call(&mut srv, "contact_sheet", json!({"source": "src:1", "count": 4}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.structured.as_ref().unwrap()["tiles"].as_array().unwrap().len(), 1, "{}", r.text);
        // The one tile takes the width asked for, even though a grid's columns would split it.
        let w = call(&mut srv, "contact_sheet", json!({"source": "src:1", "width": 640}));
        assert!(!w.is_error, "{}", w.text);
        assert_eq!(w.structured.as_ref().unwrap()["tile"]["w"], 640, "{}", w.text);
        assert_eq!(w.structured.as_ref().unwrap()["width"], 640, "{}", w.text);
    }

    fn server_with_subs() -> Server {
        let mut srv = server();
        let srt = Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt");
        srv.session
            .as_mut()
            .expect("session")
            .import_subtitles(&srt)
            .expect("import test_subs.srt");
        srv
    }

    #[test]
    fn burned_frame_differs_from_plain_inside_a_cue() {
        use engine::text::TextStyle;
        let mut srv = server_with_subs();
        let plain = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640}));
        let burned = call(&mut srv, "frame", json!({"at": "1.0s", "width": 640, "burn_captions": {}}));
        assert!(!plain.is_error, "{}", plain.text);
        assert!(!burned.is_error, "{}", burned.text);
        assert!(burned.text.contains("captions burned (track 0)"), "{}", burned.text);
        assert_ne!(unbase64(&plain.images[0].1), unbase64(&burned.images[0].1), "burning must change the PNG");

        // The same frame, straight from the compositor: the captions sit in the bottom
        // quarter and leave the top quarter alone.
        let s = srv.session.as_ref().expect("session");
        let fps = parse::session_fps(s);
        let at = parse::frame_arg(&json!({"at": "1.0s"}), "at", fps, parse::session_end(s))
            .unwrap_or_else(|r| panic!("{}", r.text));
        let (project, meta) = s.export_snapshot();
        let a = engine::export::compose_at_with(&project, &meta, at, 640, 2560, None).expect("plain");
        let b = engine::export::compose_at_with(&project, &meta, at, 640, 2560, Some((0, TextStyle::default())))
            .expect("burned");
        assert_eq!(a.bgra.len(), b.bgra.len());
        let row = (a.width * 4) as usize;
        let h = a.height as usize;
        let differs = |from: usize, to: usize| {
            (from..to).any(|y| a.bgra[y * row..(y + 1) * row] != b.bgra[y * row..(y + 1) * row])
        };
        assert!(differs(h * 3 / 4, h), "no caption pixels in the bottom quarter");
        assert!(!differs(0, h / 4), "captions changed the top quarter");
    }

    #[test]
    fn burn_captions_refuses_source_and_bad_track() {
        let mut srv = server_with_subs();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "source": "src:0", "burn_captions": {}}));
        assert!(r.is_error && r.text.contains("source"), "{}", r.text);
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "burn_captions": {"track": 4}}));
        assert!(r.is_error && r.text.contains("does not exist"), "{}", r.text);
        let r = call(&mut srv, "contact_sheet", json!({"count": 2, "burn_captions": {"track": 4}}));
        assert!(r.is_error && r.text.contains("does not exist"), "{}", r.text);
        let r = call(&mut srv, "contact_sheet", json!({"count": 2, "burn_captions": {"size": 60}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions burned (track 0)"), "{}", r.text);
    }

    #[test]
    fn frame_refuses_a_width_that_is_not_a_size_before_anything_else() {
        let mut srv = server();
        for w in [json!(-3), json!(0), json!(5000), json!("wide"), json!(2.5)] {
            let r = call(&mut srv, "frame", json!({"at": "1.0s", "width": w}));
            assert!(r.is_error && r.text.contains("width"), "{w}: {}", r.text);
        }
    }

    #[test]
    fn burned_frame_reports_the_caption_it_drew_after_an_edit_and_a_cut() {
        use crate::tools::edit::run_edit;
        let mut srv = server_with_subs();
        let ops = json!({"ops": [{"op": "captions_place", "track": 0}]});
        assert!(!run_edit(&mut srv, &ops).is_error);
        let ops = json!({"ops": [{"op": "cue_text", "track": 0, "index": 0, "text": "Hello there"}]});
        assert!(!run_edit(&mut srv, &ops).is_error);

        let r = call(&mut srv, "frame", json!({"at": "1.0s", "burn_captions": {}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions burned (track 0): \"Hello there\""), "{}", r.text);

        // Cut the second cue's time out of the captions, then burn at the spot it was.
        let s = srv.session.as_mut().expect("session");
        let fps = parse::session_fps(s);
        let lanes = s.subtitle_lanes();
        let (a, b, _, _) = s.sub_lane_cue_refs(lanes[0])[1];
        let cut_text = s.subtitles()[0].cues[1].text.clone();
        let from = (a as f64 * fps / 1e6).round() as u32;
        let len = ((b - a) as f64 * fps / 1e6).round().max(1.0) as u32;
        s.cut_regions(&[(from, len)], &lanes).expect("cut the second cue");
        let r = call(&mut srv, "frame", json!({"at": format!("{from}f"), "burn_captions": {}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains(&cut_text), "the cut caption is still reported: {}", r.text);
    }

    #[test]
    fn burned_frame_lists_every_cue_on_screen_and_the_missing_glyph() {
        use crate::tools::edit::run_edit;
        let td = crate::testutil::TestDir::new("see_stacked_cues");
        let srt = td.path().join("stack.srt");
        // The two cues overlap at 2.5 s, and U+10FFFD is a noncharacter no face draws.
        std::fs::write(
            &srt,
            "1\n00:00:01,000 --> 00:00:04,000\nFirst \u{10FFFD}\n\n2\n00:00:02,000 --> 00:00:05,000\nSecond\n\n",
        )
        .expect("write the srt");
        let mut srv = server();
        srv.session.as_mut().expect("session").import_subtitles(&srt).expect("import");
        let ops = json!({"ops": [{"op": "captions_place", "track": 0}]});
        assert!(!run_edit(&mut srv, &ops).is_error);

        let r = call(&mut srv, "frame", json!({"at": "2.5s", "burn_captions": {}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions burned (track 0): \"First"), "{}", r.text);
        assert!(r.text.contains("captions burned (track 0): \"Second\""), "{}", r.text);
        assert!(r.text.contains("no glyph for: U+10FFFD (drawn as an empty box)"), "{}", r.text);
    }

    #[test]
    fn frame_text_lists_the_titles_on_screen() {
        use crate::tools::edit::run_edit;
        let mut srv = server();
        let ops = json!({"ops": [{"op": "title_add", "at": "0.5s", "duration": "1s", "text": "Opening"}]});
        let r = run_edit(&mut srv, &ops);
        assert!(!r.is_error, "{}", r.text);
        let r = call(&mut srv, "frame", json!({"at": "0.7s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("title on screen: \"Opening\""), "{}", r.text);
        let r = call(&mut srv, "frame", json!({"at": "3.0s"}));
        assert!(!r.text.contains("title on screen"), "{}", r.text);
    }

    #[test]
    #[ignore = "timing: run with --release --ignored -- --nocapture"]
    fn contact_sheet_12_timing() {
        let mut srv = server();
        let t = Instant::now();
        let r = call(&mut srv, "contact_sheet", json!({"count": 12}));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(!r.is_error, "{}", r.text);
        eprintln!("contact_sheet count 12: {ms:.1} ms");
    }

    #[test]
    fn schemas_describe_every_property() {
        for tool in tools() {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            for (key, p) in schema["properties"].as_object().expect("properties") {
                assert!(p["description"].as_str().is_some(), "{}.{key} has no description", tool.name);
            }
        }
    }

    /// The `burn_captions` argument of frame and contact_sheet is the export's own schema: the
    /// boolean reading is said once, and its type takes a boolean as well as the object.
    #[test]
    fn burn_captions_schema_says_its_boolean_reading_once() {
        // `scopes` reads the picture's levels, never captions.
        for tool in tools().into_iter().filter(|t| t.name != "scopes") {
            let schema = (tool.schema)();
            let burn = &schema["properties"]["burn_captions"];
            assert_eq!(burn["type"], json!(["object", "boolean"]), "{}", tool.name);
            let doc = burn["description"].as_str().unwrap_or_default();
            assert_eq!(doc.matches("Pass true for the defaults").count(), 1, "{}: {doc}", tool.name);
        }
    }

    #[test]
    fn integral_float_out_of_range_is_named_in_plain_words() {
        let err = bounded(&json!({"width": 1000000000.0}), "width", 640, 1, 1920).expect_err("refused");
        assert_eq!(err.text, "argument `width` 1000000000 is too large: 1 to 1920");
        let err = bounded(&json!({"width": 1e30}), "width", 640, 1, 1920).expect_err("refused");
        assert_eq!(err.text, "argument `width` 1000000000000000019884624838656 is too large: 1 to 1920");
        let err = bounded(&json!({"width": -5.0}), "width", 640, 1, 1920).expect_err("refused");
        assert_eq!(err.text, "argument `width` -5 is too small: 1 to 1920");
        let err = bounded(&json!({"width": 1000000000}), "width", 640, 1, 1920).expect_err("refused");
        assert_eq!(err.text, "argument `width` 1000000000 is too large: 1 to 1920");
    }

    #[test]
    fn integral_float_in_range_is_accepted() {
        assert_eq!(bounded(&json!({"width": 1280.0}), "width", 640, 1, 1920).ok(), Some(1280));
        assert_eq!(bounded(&json!({}), "width", 640, 1, 1920).ok(), Some(640));
    }

    #[test]
    fn fractional_width_still_says_whole_number() {
        let err = bounded(&json!({"width": 1.5}), "width", 640, 1, 1920).expect_err("refused");
        assert_eq!(err.text, "argument `width` must be a whole number from 1 to 1920, not 1.5");
    }

    #[test]
    fn frame_at_end_shows_the_last_frame_not_refused() {
        let mut srv = server();
        let s = srv.session.as_ref().expect("session");
        let media = s.export_snapshot().0.media_frames();
        let r = call(&mut srv, "frame", json!({"at": "end"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("{}f", media - 1)), "{}", r.text);
    }

    #[test]
    fn two_titles_on_one_place_that_overlap_warn_on_the_frame() {
        use crate::tools::edit::run_edit;
        let mut srv = server();
        let ops = json!({"ops": [
            {"op": "title_add", "at": "0s", "duration": "3s", "text": "Top one", "place": "top"},
            {"op": "title_add", "at": "1s", "duration": "2s", "text": "Top two", "place": "top"},
        ]});
        assert!(!run_edit(&mut srv, &ops).is_error);
        let r = call(&mut srv, "frame", json!({"at": "1.5s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("warning: two titles overlap at top 1.0–3.0s"), "{}", r.text);
        let apart = call(&mut srv, "frame", json!({"at": "0.5s"}));
        assert!(!apart.text.contains("two titles overlap"), "{}", apart.text);
    }

    #[test]
    fn caption_glyph_note_names_only_the_cue_on_screen() {
        use crate::tools::edit::run_edit;
        let td = crate::testutil::TestDir::new("see_glyph_on_screen");
        let srt = td.path().join("glyph.srt");
        // A Latin cue, then one with a character no face draws; the frame must only name the cue it shows.
        std::fs::write(&srt, "1\n00:00:00,200 --> 00:00:01,000\nHello\n\n2\n00:00:01,500 --> 00:00:02,200\n\u{10FFFD}\n\n")
            .expect("write the srt");
        let mut srv = server();
        srv.session.as_mut().expect("session").import_subtitles(&srt).expect("import");
        assert!(!run_edit(&mut srv, &json!({"ops": [{"op": "captions_place", "track": 0}]})).is_error);

        let latin = call(&mut srv, "frame", json!({"at": "0.5s", "burn_captions": {}}));
        assert!(!latin.is_error, "{}", latin.text);
        assert!(!latin.text.contains("no glyph for"), "{}", latin.text);
        let cyrillic = call(&mut srv, "frame", json!({"at": "2.0s", "burn_captions": {}}));
        assert!(!cyrillic.is_error, "{}", cyrillic.text);
        assert!(cyrillic.text.contains("no glyph for: U+10FFFD"), "{}", cyrillic.text);
    }

    #[test]
    fn a_preview_names_the_export_size_it_stands_for() {
        assert_eq!(size_text(640, 1137, (1080, 1920), "the export"), "preview 640x1137 (the export is 1080x1920)");
        assert_eq!(size_text(1080, 1920, (1080, 1920), "the export"), "1080x1920");
        let mut srv = server();
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "width": 320}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("preview 320x"), "{}", r.text);
        assert!(r.text.contains("(the export is "), "{}", r.text);
    }

    #[test]
    fn a_relative_save_path_is_named_in_full() {
        let want = std::env::current_dir().unwrap().join("shots").join("one.png");
        assert_eq!(absolute_path(PathBuf::from("shots/one.png")), want);
        let mut srv = server();
        let td = TestDir::new("frame-save-rel");
        let abs = td.join("abs.png");
        let r = call(&mut srv, "frame", json!({"at": "1.0s", "save": abs.display().to_string()}));
        assert!(r.text.contains(&format!("saved: {}", abs.display())), "{}", r.text);
    }

    #[test]
    fn a_title_over_an_upper_lane_picture_is_named_on_the_frame() {
        use crate::tools::edit::run_edit;
        let mut srv = server();
        let ops = json!({"ops": [
            {"op": "lane_add", "kind": "video"},
            {"op": "place", "source": 0, "at": "0f", "lane": "V2", "audio": false},
            {"op": "title_add", "at": "0.5s", "duration": "1s", "text": "Opening", "place": "top"},
        ]});
        assert!(!run_edit(&mut srv, &ops).is_error);
        let r = call(&mut srv, "frame", json!({"at": "0.7s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("warning: title T1 covers part of V2@0 (test_av)"), "{}", r.text);
        let before = call(&mut srv, "frame", json!({"at": "0.1s"}));
        assert!(!before.text.contains("covers part of"), "{}", before.text);
    }

    #[test]
    fn a_title_on_the_base_lane_alone_covers_no_upper_picture() {
        use crate::tools::edit::run_edit;
        let mut srv = server();
        let ops = json!({"ops": [{"op": "title_add", "at": "0.5s", "duration": "1s", "text": "Base", "place": "top"}]});
        assert!(!run_edit(&mut srv, &ops).is_error);
        let r = call(&mut srv, "frame", json!({"at": "0.7s"}));
        assert!(!r.text.contains("covers part of"), "{}", r.text);
    }

    #[test]
    fn a_burned_caption_and_a_bottom_title_on_screen_together_are_named() {
        use crate::tools::edit::run_edit;
        let td = crate::testutil::TestDir::new("see_clash_on_frame");
        let srt = td.path().join("clash.srt");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:04,000\nA caption\n\n").expect("write the srt");
        let mut srv = server();
        srv.session.as_mut().expect("session").import_subtitles(&srt).expect("import");
        let ops = json!({"ops": [
            {"op": "captions_place", "track": 0},
            {"op": "title_add", "at": "1s", "duration": "3s", "text": "Lower", "place": "bottom"},
        ]});
        assert!(!run_edit(&mut srv, &ops).is_error);
        let both = call(&mut srv, "frame", json!({"at": "2.5s", "burn_captions": {}}));
        assert!(!both.is_error, "{}", both.text);
        assert!(both.text.contains("caption overlaps title 'Lower' (both at the bottom)"), "{}", both.text);
        let caption_only = call(&mut srv, "frame", json!({"at": "0.5s", "burn_captions": {}}));
        assert!(!caption_only.text.contains("overlaps title"), "{}", caption_only.text);
    }
}
