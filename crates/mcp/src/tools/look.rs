//! Look tools: timeline text and one clip's full description.
//!
//! `timeline` is the agent's main "screen" (MCP.md section 4.3). `clip_info`
//! describes one clip in full. Both answer in text first and in
//! `structuredContent` second.

use serde_json::{json, Value};

use engine::decode::{viz_style, VIZ_FAST};
use engine::duck::Duck;
use engine::eq::BandKind;
use engine::project::{Edge, Lane, LaneKind, Title, TrimWalls};
use engine::scale::FitPolicy;
use engine::color::ColorKey;
use engine::transform::{OpacityKey, TransformKey};
use engine::{Clip, PlaybackSession};

use super::edit::audio_tracks;
use super::edit_fx::{place_word, timeline_of, title_name};
use crate::parse;
use crate::server::{opt_str, tri, Reply, Server, Tool};

/// Brief-mode text cap, in bytes (MCP.md section 3.3).
const BRIEF_CAP: usize = 4096;
/// Bytes kept free in a truncated lane for its "… N more clips" line.
const MORE_RESERVE: usize = 48;
/// Clips a lane lists in brief mode, and clips on one `page` of a lane.
const CLIPS_PER_PAGE: usize = 40;
const VIZ_STYLES: [&str; 4] = ["fill", "ribbon", "ring", "wave"];

const TIME_DOC: &str = "A time: 450f (frames), 15.0s or 15 (seconds), 01:02.500 (MM:SS.fff), \
00:00:15:00 (timecode at the project frame rate), or end.";

const TIMELINE_DOC: &str = "The timeline as text: one block per lane, one line per clip with its \
address (LANE@START), timeline span, source file and in/out frames, and any effect flags. \
Also lists lane gain and mute/hide/lock state, and the gap count per media lane. \
Use range to read a stretch, and clip_info for everything about one clip.";

const CLIP_INFO_DOC: &str = "Everything about one clip: its lane and address, timeline span, source \
file and in/out frames, speed, fades, dissolve, fit, visualizer, EQ bands, colour, transform, \
link-group members, and how far each edge can be trimmed. With `at` (a timeline time inside the clip) it also answers match frame: the source file, its src:N number and the source time shown on that frame.";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "timeline",
            description: TIMELINE_DOC,
            schema: timeline_schema,
            run: timeline,
        },
        Tool {
            name: "clip_info",
            description: CLIP_INFO_DOC,
            schema: clip_info_schema,
            run: clip_info,
        },
    ]
}

// ---- schemas --------------------------------------------------------------

fn timeline_schema() -> Value {
    json!({
        "type": "object",
        "description": TIMELINE_DOC,
        "properties": {
            "sequence": super::sequence::prop(),
            "range": {
                "type": "object",
                "description": "Only clips that overlap this stretch are listed. Omit for the whole timeline.",
                "properties": {
                    "from": {
                        "type": ["string", "number"],
                        "description": format!("Start of the stretch. Defaults to 0. {TIME_DOC}")
                    },
                    "to": {
                        "type": ["string", "number"],
                        "description": format!("End of the stretch, exclusive. Defaults to end. {TIME_DOC}")
                    }
                }
            },
            "lanes": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Lanes to show, such as [\"V1\", \"A2\", \"S1\"]. Omit for every lane. V1.. are video, A1.. audio, S1.. subtitles."
            },
            "detail": {
                "type": "string",
                "enum": ["brief", "full"],
                "description": "brief (default) caps the text at about 4 KB, shared out equally among the lanes that have clips: each lane lists as many clips as its share holds, never more than 40 (a lane of few clips lists them all), and says how many clips were cut from it (use range, or lanes [one lane] with page, to see them). full lists every clip. structuredContent follows it: brief gives each clip's address, span, link and track; full gives every field (source, in/out, effects) as clip_info does."
            },
            "page": {
                "type": "integer",
                "minimum": 1,
                "description": "Lists one page of 40 clips of the one lane named in lanes, such as lanes [\"V1\"] with page 2 for clips 41 to 80. Lists every clip on the page, whatever the size cap. Needs exactly one lane."
            }
        }
    })
}

fn clip_info_schema() -> Value {
    json!({
        "type": "object",
        "description": CLIP_INFO_DOC,
        "properties": {
            "sequence": super::sequence::prop(),
            "clip": {
                "type": "string",
                "description": "A clip address. LANE@START names the clip starting at that time, such as V1@450, A2@15.0s or S1@00:00:30:00. LANE#INDEX names the clip by its position on the lane, such as V1#0 for the first clip. Lanes are V1.., A1.., S1.."
            },
            "at": {
                "type": "string",
                "description": "Match frame: a timeline time (450f, 15.0s, 00:00:15:00) inside the clip. The reply adds the source (src:N and file) and the source time and frame shown at that moment. Default: the clip's first frame."
            }
        },
        "required": ["clip"]
    })
}

// ---- timeline -------------------------------------------------------------

/// One clip or caption as the timeline shows it.
struct Row {
    /// Address column, `@450`. The lane name is printed by the lane header.
    addr: String,
    /// Everything after the address column.
    body: String,
    json: Value,
}

struct LaneView {
    lane: Lane,
    /// The name the user gave the lane, if any.
    name: Option<String>,
    gain_db: f32,
    hidden: bool,
    muted: bool,
    locked: bool,
    /// Every duck on the lane (a lane can hold several), in the order the engine keeps them.
    ducks: Vec<Duck>,
    /// The lane's voice processors ([`engine::voice`]); all off for a lane nobody has cleaned.
    voice: engine::voice::VoiceFx,
    rows: Vec<Row>,
    /// Set when `page` lists one page of the lane's clips.
    page: Option<LanePage>,
    /// Clips on the lane (in the range) that carry a colour grade, out of `rows` before a page is cut.
    coloured: usize,
    /// Clips on the lane in the range, before a page is cut.
    clips: usize,
}

/// Where one page of a lane sits among the lane's clips (in the range asked for).
#[derive(Clone, Copy)]
struct LanePage {
    /// Index of the page's first clip.
    first: usize,
    /// Clips on the lane in the range.
    total: usize,
    /// This page's number (from 1) and how many pages there are.
    number: usize,
    pages: usize,
}

impl LanePage {
    fn of(number: usize, total: usize) -> Self {
        let pages = total.div_ceil(CLIPS_PER_PAGE).max(1);
        Self { first: (number - 1) * CLIPS_PER_PAGE, total, number, pages }
    }

    fn json(&self, shown: usize) -> Value {
        json!({
            "page": self.number,
            "pages": self.pages,
            "first_clip": self.first + 1,
            "last_clip": self.first + shown,
            "clips": self.total,
        })
    }
}

impl LaneView {
    /// The header extras: only non-default state is shown.
    fn flags(&self) -> Vec<String> {
        let mut f = Vec::new();
        if let Some(n) = &self.name {
            f.push(format!("\"{n}\""));
        }
        if self.gain_db != 0.0 {
            f.push(format!("gain {}dB", num(f64::from(self.gain_db))));
        }
        for d in &self.ducks {
            f.push(duck_text(d));
        }
        if self.voice.is_active() {
            f.push(format!("voice: {}", self.voice.summary()));
        }
        // A batch of grades can sit past the rows shown, so the header counts them all.
        if self.coloured > 0 {
            f.push(format!("color on {} of {} clips", self.coloured, self.clips));
        }
        if self.hidden {
            f.push("hidden".into());
        }
        if self.muted {
            f.push("muted".into());
        }
        if self.locked {
            f.push("locked".into());
        }
        f
    }
}

/// A clip's brief structuredContent: its address, span, link and (on a subtitle lane)
/// track. The source, edits and effects are left to `detail: full` and `clip_info`.
fn compact_clip(clip: &Value) -> Value {
    const KEEP: [&str; 5] = ["addr", "start", "end", "link", "track"];
    Value::Object(
        KEEP.iter()
            .filter_map(|k| clip.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect(),
    )
}

fn timeline(server: &mut Server, args: &Value) -> Reply {
    let s = tri!(server.session_ref());
    let fps = parse::session_fps(s);
    let end = parse::session_end(s);
    let range = tri!(range_arg(args, fps, end));
    let full = match opt_str(args, "detail") {
        None | Some("brief") => false,
        Some("full") => true,
        Some(other) => {
            return Reply::err(format!("argument `detail`: `{other}` is not brief or full"))
        }
    };
    let page = tri!(page_arg(args));
    let known = all_lanes(s);
    let wanted = tri!(lanes_arg(args, &known));
    if page.is_some() && wanted.as_ref().is_none_or(|w| w.len() != 1) {
        return Reply::err(
            "argument `page` pages one lane: give it alone in `lanes`, such as lanes [\"V1\"] with page 2",
        );
    }
    let shown: Vec<Lane> = match wanted {
        None => known.clone(),
        Some(w) => known.iter().copied().filter(|l| w.contains(l)).collect(),
    };

    let mut views: Vec<LaneView> = shown.iter().map(|&l| lane_view(s, l, fps, range)).collect();
    if let (Some(number), [view]) = (page, views.as_mut_slice()) {
        let lp = LanePage::of(number, view.rows.len());
        if number > lp.pages {
            return Reply::err(format!(
                "page {number} is past the end: {} has {} page(s) of {CLIPS_PER_PAGE} clips",
                parse::lane_name(view.lane),
                lp.pages
            ));
        }
        let rows = std::mem::take(&mut view.rows);
        view.rows = rows.into_iter().skip(lp.first).take(CLIPS_PER_PAGE).collect();
        view.page = Some(lp);
    }
    let (w, h) = s.resolution();
    // The agent-visible steps (the ledger), the same count project_status reports.
    let undo = server.ledger.len();
    let redo = server.redo_ledger.len();
    let name = server
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string());
    let mut head = format!(
        "{name} · {w}x{h} @{} · {} ({end}f) · undo {undo} · redo {redo}",
        fps_text(fps),
        parse::clock(end, fps)
    );
    if let Some((from, to)) = range {
        head.push_str(&format!(" · range {}–{}", fmt_secs(from, fps), fmt_secs(to, fps)));
    }

    let gaps: Vec<String> = shown
        .iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .map(|&l| {
            // The count includes a lead-in gap before the lane's first clip; say so.
            let lead = s.lane_clips(l).first().map_or(0, |c| c.start);
            let note = lead_in_note(lead, fps);
            format!("{} {}{note}", parse::lane_name(l), s.gap_count(l))
        })
        .collect();
    let gaps_line = if gaps.is_empty() {
        String::new()
    } else {
        format!("gaps: {}", gaps.join(" · "))
    };

    let budget = BRIEF_CAP
        .saturating_sub(head.len() + gaps_line.len() + 32 * views.len() + 64);
    let mut lines = vec![head];
    // Each offline source is named once, with its fix; its clips are marked OFFLINE in their rows.
    for (i, p) in s.offline_sources() {
        lines.push(format!("OFFLINE: src:{i} {} (relink with media_relink)", p.display()));
    }
    if views.is_empty() {
        lines.push("no lanes in this project yet".into());
    }
    lines.extend(lane_lines(&views, full, budget, range));
    let room = BRIEF_CAP.saturating_sub(lines.iter().map(|l| l.len() + 1).sum::<usize>() + gaps_line.len());
    let (title_text, titles_json) = title_rows(s, fps, range, full, room);
    lines.extend(title_text);
    let chapters = chapter_block(s, fps, end);
    if let Some(line) = &chapters.line {
        lines.push(line.clone());
    }
    for w in &chapters.warnings {
        lines.push(format!("chapters: {w}"));
    }
    let (marker_line, markers_json) = marker_block(s, fps);
    if let Some(line) = marker_line {
        lines.push(line);
    }
    if !gaps_line.is_empty() {
        lines.push(gaps_line);
    }
    let text = lines.join("\n");

    let lanes_json: Vec<Value> = views
        .iter()
        .map(|v| {
            json!({
                "lane": parse::lane_name(v.lane),
                "name": v.name,
                "gain_db": v.gain_db,
                "hidden": v.hidden,
                "muted": v.muted,
                "locked": v.locked,
                "duck": duck_json(v.ducks.first()),
                "ducks": ducks_json(&v.ducks),
                "voice": voice_json_of(&v.voice),
                "page": v.page.map_or(Value::Null, |p| p.json(v.rows.len())),
                "coloured_clips": v.coloured,
                "clips": v
                    .rows
                    .iter()
                    .map(|r| if full { r.json.clone() } else { compact_clip(&r.json) })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let structured = json!({
        "fps": fps,
        "width": w,
        "height": h,
        "duration_frames": end,
        "lanes": lanes_json,
        "titles": titles_json,
        "chapters": chapters.json,
        "chapters_text": chapters.listing,
        "chapter_warnings": chapters.warnings,
        "markers": markers_json,
    });
    Reply::ok(text).with(structured)
}

/// The comment markers of the whole timeline as the `markers:` line and the structured list.
fn marker_block(s: &PlaybackSession, fps: f64) -> (Option<String>, Vec<Value>) {
    let markers = s.markers();
    if markers.is_empty() {
        return (None, Vec::new());
    }
    let json: Vec<Value> = markers
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({
                "addr": format!("M@{}", m.frame),
                "index": i + 1,
                "frame": m.frame,
                "time": parse::secs(m.frame, fps),
                "length": m.length,
                "name": m.name,
                "note": m.note,
                "color": m.color.name(),
            })
        })
        .collect();
    let names: Vec<String> = markers
        .iter()
        .map(|m| {
            let range = if m.length > 0 { format!("–{}f", m.end()) } else { String::new() };
            let note = if m.note.is_empty() { String::new() } else { format!(" — {}", m.note.replace('\n', " ")) };
            format!("M@{}{range} {} [{}]{note}", m.frame, m.name, m.color.name())
        })
        .collect();
    (Some(format!("markers: {}", names.join(" · "))), json)
}

/// The chapters of the whole timeline (never cut to `range`: a description lists them all) as the
/// timeline's `chapters:` line, the structured list, the plain text a video description takes, and
/// the warnings YouTube's rules call for.
struct ChapterBlock {
    line: Option<String>,
    json: Vec<Value>,
    listing: String,
    warnings: Vec<String>,
}

fn chapter_block(s: &PlaybackSession, fps: f64, end: u32) -> ChapterBlock {
    let chapters: Vec<(u32, String)> = s.chapters().iter().map(|c| (c.frame, c.title.clone())).collect();
    let (listing, warnings) = chapter_listing(&chapters, fps, end);
    if chapters.is_empty() {
        return ChapterBlock { line: None, json: Vec::new(), listing, warnings };
    }
    let json: Vec<Value> = chapters
        .iter()
        .enumerate()
        .map(|(i, (frame, title))| {
            json!({
                "addr": format!("C@{frame}"),
                "index": i + 1,
                "start_f": frame,
                "time": youtube_time(*frame, fps),
                "title": title,
            })
        })
        .collect();
    let names: Vec<String> = chapters.iter().map(|(f, t)| format!("C@{f} {t} ({})", youtube_time(*f, fps))).collect();
    let line = format!("chapters: {}", names.join(" · "));
    ChapterBlock { line: Some(line), json, listing, warnings }
}

/// The YouTube chapter listing lives in the engine ([`engine::playback::chapter_listing`]), so the
/// GUI's Copy chapters button and this tool write the same text.
pub(crate) use engine::playback::{chapter_listing, youtube_time};

/// The title lines of the timeline text, after the lanes, and the titles as JSON.
/// Only titles that overlap `range` are shown. Brief output stops the lines at
/// `room` bytes and says how many were left out; the JSON is always complete.
fn title_rows(s: &PlaybackSession, fps: f64, range: Option<(u32, u32)>, full: bool, room: usize) -> (Vec<String>, Vec<Value>) {
    let shown: Vec<(usize, &Title)> = s
        .titles()
        .iter()
        .enumerate()
        .filter(|(_, t)| overlaps(t.start, t.end(), range))
        .collect();
    let json: Vec<Value> = shown
        .iter()
        .map(|(i, t)| {
            json!({
                "name": title_name(*i),
                "start_f": t.start,
                "end_f": t.end(),
                "text": t.text,
                "place": place_word(t.style.place),
                "size": t.style.size,
                "outline": super::edit_fx::outline_word(t.style.outline),
                "boxed": t.style.boxed,
                "fade_in": t.fade_in,
                "fade_out": t.fade_out,
                "in_anim": t.in_anim.name(),
                "out_anim": t.out_anim.name(),
            })
        })
        .collect();
    let mut lines = Vec::new();
    let mut used = 0;
    for (k, (i, t)) in shown.iter().enumerate() {
        let line = format!(
            "T  {} {:.3}–{:.3} \"{}\" {}",
            title_name(*i),
            parse::secs(t.start, fps),
            parse::secs(t.end(), fps),
            short_text(&t.text),
            place_word(t.style.place)
        );
        if !full && k > 0 && used + line.len() + 1 > room {
            lines.push(format!("T  … {} more titles (use range)", shown.len() - k));
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    (lines, json)
}

/// A title's text for a timeline line: newlines as ` / `, cut to 40 characters.
fn short_text(text: &str) -> String {
    let flat = text.replace('\n', " / ");
    if flat.chars().count() <= 40 {
        return flat;
    }
    let mut cut: String = flat.chars().take(39).collect();
    cut.push('…');
    cut
}

/// The `range` argument as `[from, to)` frames, or `None` for the whole timeline.
fn range_arg(args: &Value, fps: f64, end: u32) -> Result<Option<(u32, u32)>, Reply> {
    let r = match args.get("range") {
        None | Some(Value::Null) => return Ok(None),
        Some(r) => r,
    };
    if !r.is_object() {
        return Err(Reply::err(
            "argument `range` must be an object such as {\"from\": \"0\", \"to\": \"end\"}",
        ));
    }
    let from = parse::opt_frame_arg(r, "from", fps, end)?.unwrap_or(0);
    let to = parse::opt_frame_arg(r, "to", fps, end)?.unwrap_or(end);
    if to < from {
        return Err(Reply::err(format!(
            "argument `range`: it ends at {to}f, before it starts at {from}f"
        )));
    }
    Ok(Some((from, to)))
}

/// The `page` argument: a page number from 1, or `None` when it is not given.
fn page_arg(args: &Value) -> Result<Option<usize>, Reply> {
    match args.get("page") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(p) if p >= 1 => Ok(Some(p as usize)),
            _ => Err(Reply::err("argument `page` must be a page number from 1 up, such as 2")),
        },
    }
}

/// The `lanes` argument, each name checked against the project's lanes.
fn lanes_arg(args: &Value, known: &[Lane]) -> Result<Option<Vec<Lane>>, Reply> {
    let items = match args.get("lanes") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(Reply::err(
                "argument `lanes` must be a list of lane names, such as [\"V1\", \"A2\"]",
            ))
        }
    };
    let mut wanted = Vec::new();
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| Reply::err("argument `lanes`: each entry must be a lane name such as \"V1\""))?;
        // Titles are drawn on the picture, not on a lane, so they cannot be filtered.
        if matches!(text.trim().to_ascii_uppercase().as_str(), "T" | "TITLE" | "TITLES") {
            return Err(Reply::err(
                "argument `lanes`: titles are not a lane; they always show in the timeline, so drop T from lanes",
            ));
        }
        let lane = parse::lane(text).map_err(|e| Reply::err(format!("argument `lanes`: {e}")))?;
        if !known.contains(&lane) {
            return Err(Reply::err(format!(
                "argument `lanes`: {} is not a lane in this project; the lanes are {}",
                parse::lane_name(lane),
                lane_names(known)
            )));
        }
        wanted.push(lane);
    }
    Ok(Some(wanted))
}

fn lane_view(s: &PlaybackSession, lane: Lane, fps: f64, range: Option<(u32, u32)>) -> LaneView {
    let rows = match lane.kind {
        LaneKind::Subtitle => sub_rows(s, lane, fps, range),
        _ => media_rows(s, lane, fps, range),
    };
    LaneView {
        lane,
        name: s.lane_name(lane).map(str::to_string),
        gain_db: s.lane_gain_db(lane),
        hidden: s.lane_hidden(lane),
        muted: s.lane_muted(lane),
        locked: s.lane_locked(lane),
        ducks: s.lane_duck_list(lane),
        voice: s.lane_voice(lane),
        coloured: rows.iter().filter(|r| r.json["color"].as_bool() == Some(true)).count(),
        clips: rows.len(),
        rows,
        page: None,
    }
}

/// `duck under A1 -12dB`: the key lane's name and the depth. `under` is the key's lane
/// ordinal, which is how `set_lane_duck` reads it.
fn duck_text(d: &Duck) -> String {
    format!("duck under {} {}dB", key_name(d), num(f64::from(d.depth_db)))
}

fn key_name(d: &Duck) -> String {
    parse::lane_name(Lane::new(LaneKind::Audio, d.under))
}

/// A lane's voice processors for the structured reply: `null` when none is on.
fn voice_json_of(fx: &engine::voice::VoiceFx) -> Value {
    if fx.is_active() { super::voice::voice_json(fx) } else { Value::Null }
}

/// Every duck on a lane, as the structured reply lists them.
fn ducks_json(ds: &[Duck]) -> Value {
    Value::Array(ds.iter().map(|d| duck_json(Some(d))).collect())
}

fn duck_json(d: Option<&Duck>) -> Value {
    d.map_or(Value::Null, |d| {
        json!({
            "under": key_name(d),
            "depth_db": d.depth_db,
            "threshold_db": d.threshold_db,
            "attack_ms": d.attack_ms,
            "release_ms": d.release_ms,
        })
    })
}

fn media_rows(s: &PlaybackSession, lane: Lane, fps: f64, range: Option<(u32, u32)>) -> Vec<Row> {
    let mut rows = Vec::new();
    // Sources whose file is not on disk: a clip on one is marked, since its picture or sound is missing.
    let offline: Vec<usize> = s.offline_sources().iter().map(|(i, _)| *i).collect();
    for (idx, c) in s.lane_clips(lane).iter().enumerate() {
        let end = c.end();
        if !overlaps(c.start, end, range) {
            continue;
        }
        let is_offline = offline.contains(&c.source);
        // in and out are stored at the timeline's rate; they are shown as the source frames they
        // play, in the source's own time (as edit replies print them). The JSON keeps stored frames.
        let src_fps = source_fps(s, c.source, fps);
        let (src_in, src_out) = src_range_secs(c.in_frame, c.out_frame, fps, src_fps, file_secs(s, c.source));
        let mut body = format!(
            "{}–{}   {} {}  src {}–{}",
            fmt_secs(c.start, fps),
            fmt_secs(end, fps),
            source_tag(c.source),
            source_name(s, c.source),
            src_secs_text(src_in),
            src_secs_text(src_out),
        );
        if let Some((_, names)) = s.multicam_at(lane, idx) {
            let shown = usize::from(c.angle.of(lane.kind));
            body.push_str(&format!("  angle {} \"{}\"", shown + 1, names.get(shown).map_or("", String::as_str)));
            if lane.kind == LaneKind::Video
                && let Some(a) = c.angle.audio
            {
                body.push_str(&format!(" (sound: angle {})", usize::from(a) + 1));
            }
        }
        if let Some(id) = c.link {
            body.push_str(&link_tag(s, id, (lane, c.start)));
        }
        let opacity = s.transform_of(lane, idx).map(|t| t.opacity);
        // A dissolve overlaps the next clip on its lane: the row names where the overlap runs into.
        let into = s.lane_clips(lane).get(idx + 1).map(|n| parse::addr(lane, n.start));
        for flag in media_flags(c, opacity, into.as_deref()) {
            body.push_str("  ");
            body.push_str(&flag);
        }
        let keys = s.transform_keys_of(lane, idx).len();
        if keys > 0 {
            body.push_str(&format!("  keys {keys}"));
        }
        let (okeys, ckeys) = (s.opacity_keys_of(lane, idx).len(), s.color_keys_of(lane, idx).len());
        if okeys > 0 {
            body.push_str(&format!("  opacity keys {okeys}"));
        }
        if ckeys > 0 {
            body.push_str(&format!("  color keys {ckeys}"));
        }
        if is_offline {
            body.push_str("  OFFLINE");
        }
        let mark = c.mark;
        if let Some(n) = engine::project::label_name(mark.label) {
            body.push_str(&format!("  label {n}"));
        }
        if mark.off {
            body.push_str("  disabled");
        }
        let json = json!({
            "label": engine::project::label_name(mark.label),
            "disabled": mark.off,
            "generator": s.generator_of(lane, idx).map(|g| super::generated::describe(&g)),
            "addr": parse::addr(lane, c.start),
            "start": c.start,
            "end": end,
            "source": c.source,
            "offline": is_offline,
            "in": c.in_frame,
            "out": c.out_frame,
            "link": c.link,
            "speed": c.speed.as_f64(),
            "reversed": c.reversed,
            "fade_in": c.fade_in,
            "fade_out": c.fade_out,
            "transition_out": c.transition_out,
            "transition_kind": c.transition_kind.name(),
            "fit": fit_name(c.fit).unwrap_or("fit"),
            "eq": c.eq.is_some(),
            "color": c.color.is_some(),
            "transform": c.transform.is_some(),
            "transform_keys": keys,
            "opacity_keys": okeys,
            "color_keys": ckeys,
            "env": !c.gain_keys.is_empty(),
            "visualizer": c.visualizer != 0,
        });
        rows.push(Row {
            addr: format!("@{}", c.start),
            body,
            json,
        });
    }
    rows
}

fn sub_rows(s: &PlaybackSession, lane: Lane, fps: f64, range: Option<(u32, u32)>) -> Vec<Row> {
    let mut rows = Vec::new();
    for c in s.sub_lane(lane) {
        let end = c.end();
        if !overlaps(c.start, end, range) {
            continue;
        }
        let label = s
            .subtitles()
            .get(c.track)
            .map(|t| t.label.clone())
            .unwrap_or_default();
        let mut body = format!(
            "{}–{}   track {}",
            fmt_secs(c.start, fps),
            fmt_secs(end, fps),
            c.track
        );
        if !label.is_empty() {
            body.push(' ');
            body.push_str(&label);
        }
        if let Some(id) = c.link {
            body.push_str(&link_tag(s, id, (lane, c.start)));
        }
        let json = json!({
            "addr": parse::addr(lane, c.start),
            "start": c.start,
            "end": end,
            "source": null,
            "in": null,
            "out": null,
            "link": c.link,
            "speed": null,
            "fade_in": null,
            "fade_out": null,
            "transition_out": null,
            "track": c.track,
            "label": label,
        });
        rows.push(Row {
            addr: format!("@{}", c.start),
            body,
            json,
        });
    }
    rows
}

/// Text lines for every lane. In brief mode each lane with clips gets an equal
/// share of `budget` bytes; a lane that cannot fit all its clips says how many
/// it cut. Full mode never cuts.
fn lane_lines(views: &[LaneView], full: bool, budget: usize, range: Option<(u32, u32)>) -> Vec<String> {
    let with_rows = views.iter().filter(|v| !v.rows.is_empty()).count().max(1);
    let per_lane = budget / with_rows;
    let mut out = Vec::new();
    for v in views {
        let name = parse::lane_name(v.lane);
        let flags = v.flags();
        // A paged lane always has its page line, so its first clip starts below it.
        let inline = flags.is_empty() && v.page.is_none();
        if !flags.is_empty() {
            out.push(format!("{name:<4}{}", flags.join(" ")));
        }
        if v.rows.is_empty() {
            if flags.is_empty() {
                let none = if range.is_some() { "no clips in range" } else { "no clips" };
                out.push(format!("{name:<4}{none}"));
            }
            continue;
        }
        if let Some(p) = v.page {
            let (a, b) = (p.first + 1, p.first + v.rows.len());
            out.push(format!("{name:<4}clips {a}–{b} of {} · page {} of {}", p.total, p.number, p.pages));
        }
        // With no header extras the first clip shares the lane's line.
        let first_prefix = if inline { format!("{name:<4}") } else { "    ".to_string() };
        let lines: Vec<String> = v
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let prefix = if i == 0 { first_prefix.as_str() } else { "    " };
                format!("{prefix}{:<8}{}", r.addr, r.body)
            })
            .collect();
        let total: usize = lines.iter().map(|l| l.len() + 1).sum();
        // A page is listed whole: it is at most CLIPS_PER_PAGE clips.
        if full || v.page.is_some() || (total <= per_lane && lines.len() <= CLIPS_PER_PAGE) {
            out.extend(lines);
            continue;
        }
        let mut used = 0;
        let mut keep = 0;
        for l in &lines {
            if keep > 0 && (keep == CLIPS_PER_PAGE || used + l.len() + 1 + MORE_RESERVE > per_lane) {
                break;
            }
            used += l.len() + 1;
            keep += 1;
        }
        let cut = lines.len() - keep;
        out.extend(lines.into_iter().take(keep));
        out.push(format!("    … {cut} more clips (use range, or lanes [\"{name}\"] with page)"));
    }
    out
}

impl LaneView {
    #[cfg(test)]
    fn for_test(lane: Lane, rows: Vec<Row>) -> Self {
        Self {
            lane,
            name: None,
            gain_db: 0.0,
            hidden: false,
            muted: false,
            locked: false,
            ducks: Vec::new(),
            voice: engine::voice::VoiceFx::default(),
            coloured: rows.iter().filter(|r| r.json["color"].as_bool() == Some(true)).count(),
            clips: rows.len(),
            rows,
            page: None,
        }
    }
}

// ---- clip_info ------------------------------------------------------------

/// What a clip's source says about its sound: the audio stream it plays from with that track's
/// language, e.g. `audio stream 1 (fra)`, or `audio: none` when the file has no sound stream at
/// all (a picture alone).
fn source_sound(path: Option<&std::path::Path>, audio_stream: usize) -> String {
    let Some(path) = path else {
        return format!("audio stream {audio_stream}");
    };
    let tracks = audio_tracks(path);
    if tracks.is_empty() {
        return "audio: none".to_string();
    }
    match tracks.iter().find(|(t, _)| *t == audio_stream) {
        Some((_, lang)) => format!("audio stream {audio_stream} ({lang})"),
        None => format!("audio stream {audio_stream}"),
    }
}

/// A clip's transform keys, one line each, with the time both ways: the clip time is what the
/// transform_keys op's `at` takes, the timeline time is where the key sits on the timeline:
/// `at clip 1.000s (timeline 7.000s) x 0.1 y 0 scale 1.20 rotate 0 opacity 1 ease linear`.
fn transform_key_lines(c: &Clip, fps: f64, keys: &[TransformKey]) -> Vec<String> {
    keys.iter()
        .map(|k| {
            let p = &k.params;
            let ease = k.ease.name();
            let at = timeline_of(c, k.at);
            format!(
                "  at clip {:.3}s (timeline {:.3}s) x {} y {} scale {} rotate {} opacity {} ease {ease}",
                (at - f64::from(c.start)) / fps,
                at / fps,
                num(f64::from(p.pos_x)),
                num(f64::from(p.pos_y)),
                num(f64::from(p.scale)),
                num(f64::from(p.rotate)),
                num(f64::from(p.opacity)),
            )
        })
        .collect()
}

/// A clip's opacity keys, one line each, in the shape of [`transform_key_lines`].
fn opacity_key_lines(c: &Clip, fps: f64, keys: &[OpacityKey]) -> Vec<String> {
    keys.iter()
        .map(|k| {
            let at = timeline_of(c, k.at);
            format!(
                "  at clip {:.3}s (timeline {:.3}s) opacity {} ease {}",
                (at - f64::from(c.start)) / fps,
                at / fps,
                num(f64::from(k.opacity)),
                k.ease.name()
            )
        })
        .collect()
}

/// The look part of a grade (wheels, curves, LUT) as one line, or `None` when it is neutral.
fn look_line(l: &engine::color::Look) -> Option<String> {
    let mut parts = Vec::new();
    for (name, w) in [("lift", l.wheels.lift), ("gamma", l.wheels.gamma), ("gain", l.wheels.gain)] {
        if w.iter().any(|&v| v != 0.0) {
            parts.push(format!("{name} [{}]", w.iter().map(|&v| num(f64::from(v))).collect::<Vec<_>>().join(", ")));
        }
    }
    for (name, c) in engine::color::CHANNELS.iter().zip(&l.curves) {
        if !c.points().is_empty() {
            let pts: Vec<String> = c.points().iter().map(|p| format!("{}:{}", num(f64::from(p[0])), num(f64::from(p[1])))).collect();
            parts.push(format!("{name} curve {}", pts.join(",")));
        }
    }
    if let Some(r) = l.lut {
        let state = match r.problem() {
            Some(e) => format!(" — NOT APPLIED: {e}; relink with lut {{relink, path}}"),
            None => String::new(),
        };
        parts.push(format!("lut {} mix {}{state}", r.path().display(), num(f64::from(l.lut_mix))));
    }
    (!parts.is_empty()).then(|| format!("look: {}", parts.join("; ")))
}

/// A clip's colour keys, one line each, in the shape of [`transform_key_lines`].
fn color_key_lines(c: &Clip, fps: f64, keys: &[ColorKey]) -> Vec<String> {
    keys.iter()
        .map(|k| {
            let p = &k.params;
            let at = timeline_of(c, k.at);
            format!(
                "  at clip {:.3}s (timeline {:.3}s) brightness {} contrast {} saturation {} tint {} ease {}",
                (at - f64::from(c.start)) / fps,
                at / fps,
                num(f64::from(p.brightness)),
                num(f64::from(p.contrast)),
                num(f64::from(p.saturation)),
                num(f64::from(p.tint)),
                k.ease.name()
            )
        })
        .collect()
}

fn opacity_keys_json(c: &Clip, fps: f64, keys: &[OpacityKey]) -> Value {
    Value::Array(
        keys.iter()
            .map(|k| {
                json!({
                    "time": timeline_of(c, k.at) / fps,
                    "source_frame": k.at,
                    "opacity": k.opacity,
                    "ease": k.ease.name(),
                })
            })
            .collect(),
    )
}

fn color_keys_json(c: &Clip, fps: f64, keys: &[ColorKey]) -> Value {
    Value::Array(
        keys.iter()
            .map(|k| {
                json!({
                    "time": timeline_of(c, k.at) / fps,
                    "source_frame": k.at,
                    "brightness": k.params.brightness,
                    "contrast": k.params.contrast,
                    "saturation": k.params.saturation,
                    "tint": k.params.tint,
                    "ease": k.ease.name(),
                })
            })
            .collect(),
    )
}

/// The same keys as JSON: the timeline time in seconds, the source frame they sit on, and the placement.
fn transform_keys_json(c: &Clip, fps: f64, keys: &[TransformKey]) -> Value {
    Value::Array(
        keys.iter()
            .map(|k| {
                json!({
                    "time": timeline_of(c, k.at) / fps,
                    "source_frame": k.at,
                    "x": k.params.pos_x,
                    "y": k.params.pos_y,
                    "scale": k.params.scale,
                    "rotate": k.params.rotate,
                    "opacity": k.params.opacity,
                    "ease": k.ease.name(),
                })
            })
            .collect(),
    )
}

fn clip_info(server: &mut Server, args: &Value) -> Reply {
    let Some(addr) = opt_str(args, "clip") else {
        return Reply::err(
            "missing argument `clip`: give a clip address such as V1@450 (lane and start) or V1#0 (lane and index)",
        );
    };
    let s = tri!(server.session_ref());
    let fps = parse::session_fps(s);
    let (lane, idx) = match parse::clip(s, addr) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    match lane.kind {
        LaneKind::Subtitle => sub_info(s, lane, idx, fps),
        _ => {
            let mut reply = media_info(s, lane, idx, fps);
            if let Some(v) = args.get("at").filter(|v| !v.is_null()) {
                match match_frame_line(s, lane, idx, v, fps) {
                    Ok((line, json)) => {
                        reply.text.push('\n');
                        reply.text.push_str(&line);
                        if let Some(Value::Object(o)) = reply.structured.as_deref_mut() {
                            o.insert("match_frame".into(), json);
                        }
                    }
                    Err(e) => return Reply::err(e),
                }
            }
            reply
        }
    }
}

/// Match frame: the source and source time the clip shows at timeline time `at`.
fn match_frame_line(s: &PlaybackSession, lane: Lane, idx: usize, at: &Value, fps: f64) -> Result<(String, Value), String> {
    let c = &s.lane_clips(lane)[idx];
    let frame = parse::frame(at, fps, c.end()).map_err(|e| format!("`at`: {e}"))?;
    if frame < c.start || frame >= c.end() {
        return Err(format!(
            "`at` {} is outside {} (it covers {}f–{}f)",
            parse::tc(frame, fps), parse::addr(lane, c.start), c.start, c.end()
        ));
    }
    let Some((path, stream, secs)) = engine::editkeys::matched_source(s, (lane, idx), frame, fps) else {
        return Err("match frame: a compound clip plays a sequence, not a file".into());
    };
    let n = c.source;
    let line = format!(
        "match frame at {}f: src:{n} {} (audio stream {stream}), source time {secs:.3}s",
        frame,
        path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
    );
    let json = json!({ "frame": frame, "source": n, "path": path.display().to_string(), "audio_stream": stream, "source_seconds": secs });
    Ok((line, json))
}

fn media_info(s: &PlaybackSession, lane: Lane, idx: usize, fps: f64) -> Reply {
    let c: &Clip = &s.lane_clips(lane)[idx];
    let start = c.start;
    let end = c.end();
    let length = end - start;
    let addr = parse::addr(lane, start);
    let lane_nm = parse::lane_name(lane);
    let kind = match lane.kind {
        LaneKind::Audio => "audio",
        _ => "video",
    };

    let src = s.sources().get(c.source);
    let src_name = source_name(s, c.source);
    let generated = s.generator_of(lane, idx);
    let src_path = match (&generated, src) {
        (Some(_), _) => "generated, no file".to_string(),
        (None, Some(x)) => x.path.display().to_string(),
        (None, None) => "(missing)".to_string(),
    };
    let audio_stream = src.map(|x| x.audio_stream).unwrap_or(0);
    // The source's own rate, for its in/out times; the timeline's rate if unknown.
    let src_fps = src
        .map(|x| s.file_fps(&x.path))
        .filter(|f| *f > 0.0)
        .unwrap_or(fps);

    let members = link_members(s, c.link, (lane, start));
    let link_line = match c.link {
        None => "link: none".to_string(),
        Some(_) if members.is_empty() => "detached (ungrouped)".to_string(),
        Some(id) => format!("link {id}: {}", members.join(", ")),
    };

    let eq = s.eq_of(lane, idx);
    let eq_line = match eq {
        None => "eq: none (plays flat)".to_string(),
        Some(e) => {
            let mut t = format!("eq: {} bands", e.bands.len());
            for b in &e.bands {
                t.push_str(&format!(
                    "\n  {} {} Hz gain {} dB q {}",
                    band_kind(b.kind),
                    num(f64::from(b.freq_hz)),
                    num(f64::from(b.gain_db)),
                    num(f64::from(b.q))
                ));
            }
            t
        }
    };
    let eq_json = match eq {
        None => Value::Null,
        Some(e) => Value::Array(
            e.bands
                .iter()
                .map(|b| {
                    json!({
                        "freq_hz": b.freq_hz,
                        "gain_db": b.gain_db,
                        "q": b.q,
                        "kind": band_kind(b.kind),
                    })
                })
                .collect(),
        ),
    };

    let color = s.color_of(lane, idx);
    let color_line = match color {
        None => "color: none".to_string(),
        Some(k) => format!(
            "color: brightness {} contrast {} saturation {} tint {}",
            num(f64::from(k.brightness)),
            num(f64::from(k.contrast)),
            num(f64::from(k.saturation)),
            num(f64::from(k.tint))
        ),
    };
    let color_line = match color.map(|k| look_line(&k.look)) {
        Some(Some(look)) => format!("{color_line}\n{look}"),
        _ => color_line,
    };
    let color_json = color.map_or(Value::Null, |k| {
        let mut j = json!({
            "brightness": k.brightness,
            "contrast": k.contrast,
            "saturation": k.saturation,
            "tint": k.tint,
        });
        let l = &k.look;
        if *l != engine::color::Look::default() {
            j["wheels"] = json!({"lift": l.wheels.lift, "gamma": l.wheels.gamma, "gain": l.wheels.gain});
            j["curves"] = json!(engine::color::CHANNELS
                .iter()
                .zip(&l.curves)
                .map(|(n, c)| (n.to_string(), json!(c.points())))
                .collect::<serde_json::Map<String, Value>>());
            j["lut"] = l.lut.map_or(Value::Null, |r| {
                json!({"path": r.path().display().to_string(), "mix": l.lut_mix, "problem": r.problem()})
            });
        }
        j
    });

    let xf = s.transform_of(lane, idx);
    let keys = s.transform_keys_of(lane, idx);
    let transform_line = match xf {
        None => "transform: none".to_string(),
        Some(t) => format!(
            "transform: pos {} {} scale {} rotate {} crop left {} right {} top {} bottom {}{}",
            num(f64::from(t.pos_x)),
            num(f64::from(t.pos_y)),
            num(f64::from(t.scale)),
            num(f64::from(t.rotate)),
            num(f64::from(t.crop_l)),
            num(f64::from(t.crop_r)),
            num(f64::from(t.crop_t)),
            num(f64::from(t.crop_b)),
            opacity_suffix(t.opacity)
        ),
    };
    let transform_json = xf.map_or(Value::Null, |t| {
        let mut v = json!({
            "pos_x": t.pos_x,
            "pos_y": t.pos_y,
            "scale": t.scale,
            "rotate": t.rotate,
            "crop_l": t.crop_l,
            "crop_r": t.crop_r,
            "crop_t": t.crop_t,
            "crop_b": t.crop_b,
        });
        // Opacity is listed only when the clip is not fully opaque, as in the text.
        if t.opacity != 1.0 {
            v["opacity"] = json!(t.opacity);
        }
        v
    });

    let viz_part = match shown_by(s, c, lane) {
        Some(addr) => format!("visualizer: shown by {addr}"),
        None => format!("visualizer {}", viz_text(c.visualizer)),
    };
    let walls_start = s.trim_walls(lane, idx, Edge::Start);
    let walls_end = s.trim_walls(lane, idx, Edge::End);

    let mut lines = vec![
        format!("{addr} · lane {lane_nm} · {kind}"),
        format!(
            "timeline {} to {} · {} to {} · length {}",
            parse::tc(start, fps),
            parse::tc(end, fps),
            parse::clock(start, fps),
            parse::clock(end, fps),
            parse::tc(length, fps)
        ),
        format!(
            "{} {src_name} · {} · {src_path}",
            source_tag(c.source),
            source_sound(src.map(|x| x.path.as_path()), audio_stream)
        ),
        source_span_line(c.in_frame, c.out_frame, src.map(|x| x.path.as_path()), fps, src_fps),
        format!(
            "speed {}{} · fade_in {}f · fade_out {}f · {} {}f · fit {} · {}",
            speed_text(c),
            if c.reversed { " · reversed" } else { "" },
            c.fade_in,
            c.fade_out,
            c.transition_kind.name(),
            c.transition_out,
            fit_name(c.fit).unwrap_or("fit"),
            viz_part
        ),
        link_line,
        eq_line,
        color_line,
    ];
    // A keyed clip's placement is its keys, so its keys take the place of the single transform line.
    if keys.is_empty() {
        lines.push(transform_line);
    } else {
        lines.push(format!("transform keys: {}", keys.len()));
        lines.extend(transform_key_lines(c, fps, keys));
    }
    let (okeys, ckeys) = (s.opacity_keys_of(lane, idx), s.color_keys_of(lane, idx));
    if !okeys.is_empty() {
        lines.push(format!("opacity keys: {}", okeys.len()));
        lines.extend(opacity_key_lines(c, fps, okeys));
    }
    if !ckeys.is_empty() {
        lines.push(format!("color keys: {}", ckeys.len()));
        lines.extend(color_key_lines(c, fps, ckeys));
    }
    if let Some(n) = engine::project::label_name(c.mark.label) {
        lines.push(format!("label {n}"));
    }
    if c.mark.off {
        lines.push("disabled (switched off: skipped by playback, frames and export)".to_string());
    }
    lines.push(format!("trim start: {}", walls_text(walls_start, fps)));
    lines.push(format!("trim end: {}", walls_text(walls_end, fps)));
    // A clip's own gain, shown only when it is off unity.
    if c.gain_db != 0.0 {
        lines.push(format!("gain {}dB", num(f64::from(c.gain_db))));
    }
    // The volume envelope, in timeline times; only an audio clip can have one.
    if !c.gain_keys.is_empty() {
        lines.push(format!(
            "gain keys: {}",
            super::edit_fx::points_text(c, fps, &c.gain_keys)
        ));
    }
    // A duck belongs to the audio lane, so only an audio clip shows it; so do the voice processors.
    let voice = s.lane_voice(lane);
    if lane.kind == LaneKind::Audio {
        lines.push(match voice.summary() {
            t if t.is_empty() => "voice: none".to_string(),
            t => format!("voice: {t}"),
        });
    }
    let ducks = s.lane_duck_list(lane);
    if lane.kind == LaneKind::Audio {
        if ducks.is_empty() {
            lines.push("duck: none".to_string());
        }
        for d in &ducks {
            lines.push(format!(
                "{} · threshold {}dB · attack {}ms · release {}ms",
                duck_text(d),
                num(f64::from(d.threshold_db)),
                num(f64::from(d.attack_ms)),
                num(f64::from(d.release_ms))
            ));
        }
    }

    let structured = json!({
        "addr": addr,
        "lane": lane_nm,
        "kind": kind,
        "start": start,
        "end": end,
        "length": length,
        "clock_start": parse::clock(start, fps),
        "clock_end": parse::clock(end, fps),
        "source": src.map_or(Value::Null, |x| json!({
            "index": c.source,
            "name": src_name,
            "path": x.path.display().to_string(),
            "audio_stream": x.audio_stream,
        })),
        "generator": generated.map(|g| super::generated::describe(&g)),
        "label": engine::project::label_name(c.mark.label),
        "disabled": c.mark.off,
        "in": c.in_frame,
        "out": c.out_frame,
        "link": c.link,
        "link_members": members,
        "speed": c.speed.as_f64(),
        "reversed": c.reversed,
        "fade_in": c.fade_in,
        "fade_out": c.fade_out,
        "transition_out": c.transition_out,
        "transition_kind": c.transition_kind.name(),
        "fit": fit_name(c.fit).unwrap_or("fit"),
        "visualizer": viz_json(c.visualizer),
        "eq": eq_json,
        "color": color_json,
        "transform": transform_json,
        "transform_keys": transform_keys_json(c, fps, keys),
        "opacity_keys": opacity_keys_json(c, fps, okeys),
        "color_keys": color_keys_json(c, fps, ckeys),
        "trim_walls": {
            "start": walls_json(walls_start),
            "end": walls_json(walls_end),
        },
        "duck": duck_json(ducks.first()),
        "ducks": ducks_json(&ducks),
        "voice": voice_json_of(&voice),
    });
    Reply::ok(lines.join("\n")).with(structured)
}

fn sub_info(s: &PlaybackSession, lane: Lane, idx: usize, fps: f64) -> Reply {
    let c = s.sub_lane(lane)[idx];
    let start = c.start;
    let end = c.end();
    let addr = parse::addr(lane, start);
    let lane_nm = parse::lane_name(lane);
    let track = s.subtitles().get(c.track);
    let label = track.map(|t| t.label.clone()).unwrap_or_default();
    let path = track
        .map(|t| t.path.display().to_string())
        .unwrap_or_else(|| "(missing)".to_string());
    let members = link_members(s, c.link, (lane, start));
    let link_line = match c.link {
        None => "link: none".to_string(),
        Some(_) if members.is_empty() => "detached (ungrouped)".to_string(),
        Some(id) => format!("link {id}: {}", members.join(", ")),
    };
    let room = s.trim_sub_room(lane, idx, Edge::Start);
    let room_end = s.trim_sub_room(lane, idx, Edge::End);

    let lines = [
        format!("{addr} · lane {lane_nm} · subtitle"),
        format!(
            "timeline {} to {} · {} to {} · length {}",
            parse::tc(start, fps),
            parse::tc(end, fps),
            parse::clock(start, fps),
            parse::clock(end, fps),
            parse::tc(c.frames, fps)
        ),
        format!("track {} {label} · {path}", c.track),
        format!(
            "window {:.3}s to {:.3}s of the track",
            c.in_us as f64 / 1e6,
            c.out_us as f64 / 1e6
        ),
        link_line,
        format!("trim start: {}", room_text(room, fps)),
        format!("trim end: {}", room_text(room_end, fps)),
    ];

    let structured = json!({
        "addr": addr,
        "lane": lane_nm,
        "kind": "subtitle",
        "start": start,
        "end": end,
        "length": c.frames,
        "clock_start": parse::clock(start, fps),
        "clock_end": parse::clock(end, fps),
        "track": c.track,
        "label": label,
        "path": path,
        "in_us": c.in_us,
        "out_us": c.out_us,
        "link": c.link,
        "link_members": members,
        "trim_walls": {
            "start": room.map(|(lo, hi)| json!({"lo": lo, "hi": hi})),
            "end": room_end.map(|(lo, hi)| json!({"lo": lo, "hi": hi})),
        },
    });
    Reply::ok(lines.join("\n")).with(structured)
}

/// Addresses of the other clips and captions sharing `id`, across every lane.
/// The timeline's mark for a placement's link id: `link N` while another placement
/// shares it, and `detached (ungrouped)` when none does -- the same
/// words `clip_info` uses, so the two reads agree.
/// A linked clip's row tag: the partners' addresses, so the row says where its other half is.
fn link_tag(s: &PlaybackSession, id: u32, me: (Lane, u32)) -> String {
    let members = link_members(s, Some(id), me);
    if members.is_empty() {
        "  detached (ungrouped)".to_string()
    } else {
        format!("  linked to {}", members.join(", "))
    }
}

fn link_members(s: &PlaybackSession, id: Option<u32>, me: (Lane, u32)) -> Vec<String> {
    let Some(id) = id else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for lane in all_lanes(s) {
        let starts: Vec<(u32, Option<u32>)> = match lane.kind {
            LaneKind::Subtitle => s.sub_lane(lane).iter().map(|c| (c.start, c.link)).collect(),
            _ => s.lane_clips(lane).iter().map(|c| (c.start, c.link)).collect(),
        };
        for (start, link) in starts {
            if link == Some(id) && (lane, start) != me {
                out.push(parse::addr(lane, start));
            }
        }
    }
    out
}

fn walls_text(w: Option<TrimWalls>, fps: f64) -> String {
    match w {
        None => "none (this edge cannot move)".to_string(),
        Some(w) => {
            let limit = match (w.lo_media, w.hi_media) {
                (true, true) => " · both walls are the source's limits",
                (true, false) => " · lower wall is the source's limit",
                (false, true) => " · upper wall is the source's limit",
                (false, false) => " · walls are neighbouring clips or the timeline",
            };
            format!("{} to {}{limit}", parse::tc(w.lo, fps), parse::tc(w.hi, fps))
        }
    }
}

fn walls_json(w: Option<TrimWalls>) -> Value {
    w.map_or(Value::Null, |w| {
        json!({
            "lo": w.lo,
            "hi": w.hi,
            "lo_media": w.lo_media,
            "hi_media": w.hi_media,
        })
    })
}

fn room_text(room: Option<(u32, u32)>, fps: f64) -> String {
    match room {
        None => "none (this edge cannot move)".to_string(),
        Some((lo, hi)) => format!("{} to {}", parse::tc(lo, fps), parse::tc(hi, fps)),
    }
}

/// The address of the grouped video clip whose visualizer draws a sound clip's waveform, when the
/// sound clip has none of its own (the picture is placed on a video lane grouped with it).
fn shown_by(s: &PlaybackSession, c: &Clip, lane: Lane) -> Option<String> {
    let id = c.link?;
    if lane.kind != LaneKind::Audio || c.visualizer != 0 {
        return None;
    }
    all_lanes(s)
        .into_iter()
        .filter(|l| l.kind == LaneKind::Video)
        .find_map(|l| s.lane_clips(l).iter().find(|p| p.link == Some(id) && p.visualizer != 0).map(|p| parse::addr(l, p.start)))
}

fn viz_text(flags: u8) -> String {
    if flags == 0 {
        return "off".to_string();
    }
    let mut t = format!("on (style {}", VIZ_STYLES[usize::from(viz_style(flags)) & 3]);
    if flags & VIZ_FAST != 0 {
        t.push_str(", fast");
    }
    t.push(')');
    t
}

fn viz_json(flags: u8) -> Value {
    json!({
        "on": flags != 0,
        "style": VIZ_STYLES[usize::from(viz_style(flags)) & 3],
        "fast": flags & VIZ_FAST != 0,
        "flags": flags,
    })
}

// ---- helpers --------------------------------------------------------------

/// The opacity phrase a transform line ends with: nothing at 1, otherwise
/// ` opacity 0.5`.
fn opacity_suffix(opacity: f32) -> String {
    if opacity == 1.0 {
        String::new()
    } else {
        format!(" opacity {}", num(f64::from(opacity)))
    }
}

/// The timeline's flags for a clip; `opacity` is its transform's, when it has one, and `into` the
/// address of the clip a dissolve overlaps.
fn media_flags(c: &Clip, opacity: Option<f32>, into: Option<&str>) -> Vec<String> {
    let mut f = Vec::new();
    if c.speed_keys.is_empty() {
        if !c.speed.is_normal() {
            f.push(format!("speed {}x", num(c.speed.as_f64())));
        }
    } else {
        f.push(format!("speed ramp {}", ramp_text(c)));
    }
    if c.reversed {
        f.push("reversed".into());
    }
    if c.fade_in > 0 {
        f.push(format!("fade_in {}f", c.fade_in));
    }
    if c.fade_out > 0 {
        f.push(format!("fade_out {}f", c.fade_out));
    }
    if c.transition_out > 0 {
        match into {
            Some(next) => f.push(format!("{} {}f into {next}", c.transition_kind.name(), c.transition_out)),
            None => f.push(format!("{} {}f", c.transition_kind.name(), c.transition_out)),
        }
    }
    if c.eq.is_some() {
        f.push("eq".into());
    }
    if c.color.is_some() {
        f.push("color".into());
    }
    if c.transform.is_some() {
        f.push("transform".into());
    }
    if let Some(o) = opacity.filter(|o| *o != 1.0) {
        f.push(format!("opacity {}", num(f64::from(o))));
    }
    if c.gain_db != 0.0 {
        f.push(format!("gain {} dB", num(f64::from(c.gain_db))));
    }
    if !c.gain_keys.is_empty() {
        f.push("env".into());
    }
    if let Some(name) = fit_name(c.fit) {
        f.push(format!("fit {name}"));
    }
    if c.visualizer != 0 {
        f.push("viz".into());
    }
    f
}

/// The fit name, or `None` for the default fit (which is not shown).
fn fit_name(fit: FitPolicy) -> Option<&'static str> {
    match fit {
        FitPolicy::Fit => None,
        FitPolicy::Fill => Some("fill"),
        FitPolicy::Stretch => Some("stretch"),
        FitPolicy::Center => Some("center"),
    }
}

fn band_kind(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "low shelf",
        BandKind::Peak => "peak",
        BandKind::HighShelf => "high shelf",
        BandKind::LowCut => "low cut",
        BandKind::HighCut => "high cut",
    }
}

/// Every lane in display order: media lanes first, then subtitle lanes.
fn all_lanes(s: &PlaybackSession) -> Vec<Lane> {
    let mut v = s.lanes();
    for l in s.subtitle_lanes() {
        if !v.contains(&l) {
            v.push(l);
        }
    }
    v
}

fn lane_names(lanes: &[Lane]) -> String {
    if lanes.is_empty() {
        return "none".to_string();
    }
    lanes
        .iter()
        .map(|&l| parse::lane_name(l))
        .collect::<Vec<_>>()
        .join(", ")
}

fn overlaps(start: u32, end: u32, range: Option<(u32, u32)>) -> bool {
    match range {
        None => true,
        Some((from, to)) => start < to && end > from,
    }
}

/// `src:3` for a file, `seq:1` for a compound clip's sequence.
fn source_tag(source: usize) -> String {
    match engine::project::sequence_id_of(source) {
        Some(id) => format!("seq:{id}"),
        None => format!("src:{source}"),
    }
}

fn source_name(s: &PlaybackSession, idx: usize) -> String {
    if let Some(id) = engine::project::sequence_id_of(idx) {
        let kind = if s.angle_names(id).is_some() { "multicam" } else { "compound" };
        return format!("{kind} \"{}\"", s.sequence_name(id).unwrap_or_default());
    }
    if let Some(g) = s.sources().get(idx).and_then(|src| engine::generate::Gen::parse(&src.path)) {
        return super::generated::describe(&g);
    }
    s.sources()
        .get(idx)
        .and_then(|src| src.path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".to_string())
}

/// The source frame a timeline-rate frame shows, in the file's own frames: frame `c` shows
/// `floor(c * file / session)`. The same mapping as edit.rs `Base::native` and see.rs `clip_desc`,
/// with fps in micro-frames so 23.976 stays exact.
fn native_frame(timeline: u32, file_fps: f64, session_fps: f64) -> u32 {
    let micro = |fps: f64| (fps * 1e6).round().max(1.0) as u128;
    (u128::from(timeline) * micro(file_fps) / micro(session_fps)) as u32
}

/// A source's own rate, or the timeline's when the file's rate is not known.
fn source_fps(s: &PlaybackSession, source: usize, fps: f64) -> f64 {
    s.sources()
        .get(source)
        .map(|x| s.file_fps(&x.path))
        .filter(|f| *f > 0.0)
        .unwrap_or(fps)
}

/// The source range a clip plays, in source time. Clip in and out are stored at the
/// timeline's rate and shown as the source frames they play (`native_frame`), in the file's
/// own clock at `src_fps`. A still has no frames to count.
fn source_span_line(in_frame: u32, out_frame: u32, path: Option<&std::path::Path>, fps: f64, src_fps: f64) -> String {
    let in_src = native_frame(in_frame, src_fps, fps);
    // The out is exclusive: the clip plays its length at source speed from its in, so a 2 s clip
    // ends at 2.0 s of source, the same end the replies give. Both are capped at the file's real end.
    let dur = path.and_then(|p| engine::probe_media(p).duration_secs).filter(|d| *d > 0.0);
    let (_, out_secs) = src_range_secs(in_frame, out_frame, fps, src_fps, dur);
    let span = format!(
        "source in {} to out {}",
        parse::clock(in_src, src_fps),
        parse::clock((out_secs * 1000.0).round() as u32, 1000.0)
    );
    if path.is_some_and(engine::is_image) {
        return format!("{span} · still");
    }
    // The count is the source frames from the in to the out, so it stops at the file's own frame count.
    let src_frames = ((out_secs * src_fps).round() as u64).saturating_sub(u64::from(in_src));
    format!("{span} · {src_frames} source frames at {}", fps_text(src_fps))
}

fn fmt_secs(frame: u32, fps: f64) -> String {
    format!("{:.3}", parse::secs(frame, fps))
}

/// The lead-in a lane's gap count includes, named with the clip times' precision: ` (incl. 7.967s lead-in)`.
fn lead_in_note(lead: u32, fps: f64) -> String {
    if lead > 0 {
        format!(" (incl. {:.3}s lead-in)", parse::secs(lead, fps))
    } else {
        String::new()
    }
}

/// A source's real duration in seconds, when it is known.
fn file_secs(s: &PlaybackSession, source: usize) -> Option<f64> {
    s.sources().get(source).and_then(|x| engine::probe_media(&x.path).duration_secs).filter(|d| *d > 0.0)
}

/// A clip's source range in source seconds. Both ends are the source frames they show (in and out
/// are stored at the timeline's rate), counted at the file's own rate. The out is exclusive: it is
/// the frame after the last source frame the clip's last timeline frame shows, so the range has the
/// frame count the edit replies give (a 2 s clip of a 23.976 fps file is 48 frames, ending at
/// 2.002s; a 601-frame clip ends at 480 frames, 20.020s). Both ends are capped at the file's real
/// duration.
fn src_range_secs(in_frame: u32, out_frame: u32, fps: f64, src_fps: f64, dur: Option<f64>) -> (f64, f64) {
    let start = parse::secs(native_frame(in_frame, src_fps, fps), src_fps);
    let end = parse::secs(native_frame(out_frame.saturating_sub(1), src_fps, fps) + 1, src_fps);
    dur.map_or((start, end), |d| (start.min(d), end.min(d)))
}

/// A source time in the timeline's compact text: `4.0s`, `2.033s`, `0.5s`.
fn src_secs_text(secs: f64) -> String {
    let s = format!("{secs:.3}");
    let t = s.trim_end_matches('0');
    let t = if t.ends_with('.') { format!("{t}0") } else { t.to_string() };
    format!("{t}s")
}

fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-6 {
        format!("{}", fps.round() as i64)
    } else {
        num(fps)
    }
}

/// A clip's speed as the look shows it: `1.50x`, or the ramp's `1.00x→2.00x (3 keys)` when it has one.
fn speed_text(c: &Clip) -> String {
    if c.speed_keys.is_empty() {
        format!("{}x", num(c.speed.as_f64()))
    } else {
        ramp_text(c)
    }
}

/// A speed ramp's first and last speeds and its key count: `1.00x→2.00x (3 keys)`.
fn ramp_text(c: &Clip) -> String {
    let (first, last) = (c.speed_keys.first(), c.speed_keys.last());
    let speed = |k: Option<&engine::project::SpeedKey>| k.map_or(0.0, |k| k.speed.as_f64());
    format!("{:.2}x→{:.2}x ({} keys)", speed(first), speed(last), c.speed_keys.len())
}

/// A number with trailing zeros dropped: `-14`, `1.5`, `0`.
fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let t = s.trim_end_matches('0').trim_end_matches('.');
    match t {
        "" | "-0" => "0".to_string(),
        _ => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn server() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(p).expect("open test asset"));
        srv
    }

    fn sess(srv: &Server) -> &PlaybackSession {
        match srv.session_ref() {
            Ok(s) => s,
            Err(e) => panic!("{}", e.text),
        }
    }

    fn call(srv: &mut Server, name: &str, args: Value) -> Reply {
        let tool = tools().into_iter().find(|t| t.name == name).expect("tool exists");
        (tool.run)(srv, &args)
    }

    fn row(i: u32, body: &str) -> Row {
        Row {
            addr: format!("@{i}"),
            body: body.to_string(),
            json: Value::Null,
        }
    }

    #[test]
    fn schemas_describe_every_property() {
        for tool in tools() {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert!(schema["description"].as_str().is_some(), "{} top description", tool.name);
            let props = schema["properties"].as_object().expect("properties");
            for (key, p) in props {
                assert!(p["description"].as_str().is_some(), "{}.{key} has no description", tool.name);
            }
        }
        let t = (timeline_schema)();
        assert!(t["properties"]["range"]["properties"]["from"]["description"].as_str().unwrap().contains("450f"));
        assert_eq!((clip_info_schema)()["required"][0], "clip");
    }

    #[test]
    fn num_drops_trailing_zeros() {
        assert_eq!(num(-14.0), "-14");
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(100.0), "100");
        assert_eq!(num(-0.0001), "0");
    }

    #[test]
    fn brief_cuts_a_long_lane_and_says_so() {
        let body = "0.000–1.000   src:0 interview_long_name.mp4  in 0 out 30  link 1";
        let mk = || LaneView::for_test(Lane::V1, (0..400).map(|i| row(i, body)).collect());
        let brief = lane_lines(&[mk()], false, 3000, None).join("\n");
        assert!(brief.contains("more clips (use range, or lanes [\"V1\"] with page)"), "{brief}");
        assert!(brief.len() < 3200, "brief text is {} bytes", brief.len());
        let full = lane_lines(&[mk()], true, 3000, None).join("\n");
        assert!(!full.contains("more clips"));
        assert_eq!(full.lines().count(), 400);
    }

    #[test]
    fn brief_lists_at_most_forty_clips_a_lane_even_when_bytes_allow_more() {
        let body = "0.000–1.000   src:0 a.mp4  in 0 out 30";
        let v = LaneView::for_test(Lane::V1, (0..68).map(|i| row(i, body)).collect());
        let brief = lane_lines(&[v], false, 100_000, None);
        assert_eq!(brief.iter().filter(|l| l.contains("@")).count(), CLIPS_PER_PAGE);
        assert!(brief.last().unwrap().contains("… 28 more clips"), "{brief:?}");
    }

    #[test]
    fn a_page_lists_its_clips_with_a_page_line() {
        let body = "0.000–1.000   src:0 a.mp4  in 0 out 30";
        let mut v = LaneView::for_test(Lane::V1, (40..68).map(|i| row(i, body)).collect());
        v.page = Some(LanePage::of(2, 68));
        let lines = lane_lines(&[v], false, 100, None);
        assert_eq!(lines[0], "V1  clips 41–68 of 68 · page 2 of 2");
        assert_eq!(lines.len(), 29, "the page is listed whole, with no cut line");
        assert!(!lines.iter().any(|l| l.contains("more clips")));
    }

    #[test]
    fn a_lane_header_counts_its_graded_clips_past_the_cut() {
        let body = "0.000–1.000   src:0 a.mp4  in 0 out 30  color";
        let graded = |i: u32| Row { addr: format!("@{i}"), body: body.into(), json: json!({"color": true}) };
        let plain = |i: u32| Row { addr: format!("@{i}"), body: body.into(), json: json!({"color": false}) };
        let mut rows: Vec<Row> = (0..50).map(graded).collect();
        rows.extend((50..68).map(plain));
        let v = LaneView::for_test(Lane::V1, rows);
        assert_eq!(v.coloured, 50);
        let lines = lane_lines(&[v], false, 100_000, None);
        assert_eq!(lines[0], "V1  color on 50 of 68 clips");
        assert_eq!(lines.iter().filter(|l| l.contains("@")).count(), CLIPS_PER_PAGE);
    }

    #[test]
    fn page_arg_wants_a_number_from_one() {
        assert_eq!(page_arg(&json!({})).ok(), Some(None));
        assert_eq!(page_arg(&json!({"page": 2})).ok(), Some(Some(2)));
        assert!(page_arg(&json!({"page": 0})).is_err());
        assert!(page_arg(&json!({"page": "two"})).is_err());
    }

    #[test]
    fn timeline_pages_one_lane_and_refuses_a_page_without_one() {
        let mut srv = server();
        let n = sess(&srv).lane_clips(Lane::V1).len();
        let pages = n.div_ceil(CLIPS_PER_PAGE).max(1);
        let r = call(&mut srv, "timeline", json!({"lanes": ["V1"], "page": 1}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("of {n} · page 1 of {pages}")), "{}", r.text);
        let past = call(&mut srv, "timeline", json!({"lanes": ["V1"], "page": pages + 1}));
        assert!(past.is_error);
        assert!(
            past.text.contains(&format!("page {} is past the end: V1 has {pages} page(s)", pages + 1)),
            "{}",
            past.text
        );
        let none = call(&mut srv, "timeline", json!({"page": 1}));
        assert!(none.is_error);
        assert!(none.text.contains("pages one lane"), "{}", none.text);
    }

    #[test]
    fn timeline_shows_a_lane_name_on_the_header() {
        let mut srv = server();
        let plain = call(&mut srv, "timeline", json!({}));
        assert!(plain.structured.as_ref().unwrap()["lanes"][0]["name"].is_null());
        assert_eq!(srv.session.as_mut().unwrap().set_lane_name(Lane::V1, "Cam A"), Ok(true));
        let r = call(&mut srv, "timeline", json!({}));
        assert!(r.text.contains("V1  \"Cam A\""), "{}", r.text);
        let v1 = &r.structured.as_ref().unwrap()["lanes"][0];
        assert_eq!((v1["lane"].as_str(), v1["name"].as_str()), (Some("V1"), Some("Cam A")));
    }

    #[test]
    fn timeline_lists_the_test_asset() {
        let mut srv = server();
        let first = sess(&srv).lane_clips(Lane::V1)[0].start;
        let r = call(&mut srv, "timeline", json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("untitled · "), "{}", r.text);
        assert!(r.text.contains(" undo "), "{}", r.text);
        assert!(r.text.contains(&format!("V1  @{first} ")), "{}", r.text);
        assert!(r.text.contains("src:0 test_av.mp4"), "{}", r.text);
        assert!(r.text.contains("gaps: V1 "), "{}", r.text);
        let v1 = &r.structured.as_ref().expect("structured")["lanes"][0];
        assert_eq!(v1["lane"], "V1");
        assert_eq!(v1["clips"][0]["addr"], format!("V1@{first}"));
        // Brief structuredContent is compact: no source, edits or effect flags per clip.
        let brief = v1["clips"][0].as_object().expect("clip object");
        assert!(brief.contains_key("start") && brief.contains_key("end"), "{brief:?}");
        assert!(!brief.contains_key("source") && !brief.contains_key("fade_in"), "{brief:?}");
        let full = call(&mut srv, "timeline", json!({"detail": "full"}));
        let v1 = &full.structured.as_ref().expect("structured")["lanes"][0];
        assert!(v1["clips"][0].get("source").is_some() && v1["clips"][0].get("fade_in").is_some());
    }

    #[test]
    fn timeline_refuses_an_unknown_lane() {
        let mut srv = server();
        let r = call(&mut srv, "timeline", json!({"lanes": ["V9"]}));
        assert!(r.is_error);
        assert!(r.text.contains("is not a lane in this project"), "{}", r.text);
        let bad = call(&mut srv, "timeline", json!({"detail": "loud"}));
        assert!(bad.is_error && bad.text.contains("brief or full"), "{}", bad.text);
    }

    #[test]
    fn timeline_range_keeps_only_overlapping_clips() {
        let mut srv = server();
        let r = call(&mut srv, "timeline", json!({"range": {"from": "0f", "to": "0f"}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V1  no clips in range"), "{}", r.text);
        assert!(!r.text.contains("src:"), "{}", r.text);
        let back = call(&mut srv, "timeline", json!({"range": {"from": 5, "to": 1}}));
        assert!(back.is_error);
    }

    /// A source with no sound says `audio: none` where a sound source names its stream.
    #[test]
    fn clip_info_at_answers_match_frame() {
        let mut srv = server();
        let r = call(&mut srv, "clip_info", json!({"clip": "V1@0", "at": "1s"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("match frame at 30f: src:0 test_av.mp4") && r.text.contains("source time 1.000s"), "{}", r.text);
        assert_eq!(r.structured.as_ref().unwrap()["match_frame"]["source"], 0);
        let r = call(&mut srv, "clip_info", json!({"clip": "V1@0", "at": "100s"}));
        assert!(r.is_error && r.text.contains("outside V1@0"), "{}", r.text);
    }

    #[test]
    fn clip_info_says_when_a_source_has_no_sound() {
        let root = env!("CARGO_MANIFEST_DIR");
        let silent = std::path::PathBuf::from(format!("{root}/../../assets/test_rotation90.mp4"));
        let with_sound = std::path::PathBuf::from(format!("{root}/../../assets/test_h264.mkv"));
        assert_eq!(source_sound(Some(&silent), 0), "audio: none");
        // The stream names its language, as the probe reads it (`und` where the file names none).
        let lang = audio_tracks(&with_sound)[0].1.clone();
        assert_eq!(source_sound(Some(&with_sound), 0), format!("audio stream 0 ({lang})"));
        assert_eq!(source_sound(None, 2), "audio stream 2");
    }

    /// A lone link id is a detach by the user, not a removed partner: the words say `ungrouped`.
    #[test]
    fn an_ungrouped_clip_reads_as_ungrouped_not_as_a_removed_partner() {
        let mut srv = server();
        {
            let s = srv.session.as_mut().expect("session");
            // The test asset's first take (V1 and A1) is linked already; ungroup leaves the
            // partner with an id of its own, which nothing else shares.
            assert!(s.ungroup(Lane::V1, 0));
        }
        let r = call(&mut srv, "clip_info", json!({"clip": "A1#0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("detached (ungrouped)"), "{}", r.text);
        assert!(!r.text.contains("partner was removed"), "{}", r.text);
        let t = call(&mut srv, "timeline", json!({}));
        assert!(!t.text.contains("partner was removed"), "{}", t.text);
    }

    /// A sound clip whose waveform picture sits on a grouped video lane says so, not "visualizer off".
    #[test]
    fn sound_clip_names_the_grouped_visualizer_that_shows_it() {
        let mut srv = server();
        let r = super::super::edit::run_edit(&mut srv, &json!({"ops": [{"op": "visualizer", "clip": "A1#0", "on": true}]}));
        assert!(!r.is_error, "{}", r.text);
        // The picture lands on a video lane grouped with the sound clip (V2 here, V1 is busy at 0).
        let info = call(&mut srv, "clip_info", json!({"clip": "A1#0"}));
        assert!(info.text.contains("visualizer: shown by V"), "{}", info.text);
        assert!(!info.text.contains("visualizer off"), "{}", info.text);
    }

    /// The timeline row of a dissolving clip names the clip its overlap runs into.
    #[test]
    fn timeline_marks_the_dissolve_overlap_into_the_next_clip() {
        let mut srv = server();
        let r = super::super::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true}]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let len = sess(&srv).lane_clips(Lane::V1)[0].frames();
        let r = super::super::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "place", "source": 0, "lane": "V1", "at": format!("{len}f"), "audio": false, "overwrite": true},
                {"op": "dissolve", "clip": "V1@0", "frames": 12}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains(&format!("dissolve 12f into V1@{len}")), "{}", tl.text);
    }

    /// A transition's kind shows in the timeline row and in clip_info, text and JSON.
    #[test]
    fn timeline_and_clip_info_name_the_transition_kind() {
        let mut srv = server();
        let r = super::super::edit::run_edit(
            &mut srv,
            &json!({"ops": [{"op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true}]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let len = sess(&srv).lane_clips(Lane::V1)[0].frames();
        let r = super::super::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "place", "source": 0, "lane": "V1", "at": format!("{len}f"), "audio": false, "overwrite": true},
                {"op": "transition", "clip": "V1@0", "kind": "wipe", "direction": "down", "frames": 12}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains(&format!("wipe-down 12f into V1@{len}")), "{}", tl.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1@0"}));
        assert!(info.text.contains("wipe-down 12f"), "{}", info.text);
        let st = info.structured.as_ref().expect("structured");
        assert_eq!(st["transition_kind"], "wipe-down", "{st:?}");
    }

    /// A keyed clip shows its keys in text, JSON and the timeline row, and no single transform line.
    #[test]
    fn clip_info_and_timeline_show_transform_keys() {
        let mut srv = server();
        let s = srv.session.as_mut().expect("session");
        let r = super::super::edit_fx::apply(
            s,
            &json!({"op": "transform_keys", "clip": "V1#0", "keys": [
                {"at": "0s", "scale": 1.0},
                {"at": "end", "scale": 1.2, "x": 0.1, "ease": "ease"}
            ]}),
            30.0,
        )
        .expect("transform_keys is an edit_fx op");
        assert!(r.is_ok(), "{:?}", r.err());

        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(!info.is_error, "{}", info.text);
        assert!(info.text.contains("transform keys: 2"), "{}", info.text);
        assert!(!info.text.contains("transform: none"), "{}", info.text);
        assert!(info.text.contains("scale 1.2 "), "{}", info.text);
        // The first key sits at the clip's start: its clip time and its timeline time both read 0.
        assert!(info.text.contains("at clip 0.000s (timeline 0.000s) x 0 y 0"), "{}", info.text);
        assert!(info.text.contains(" ease ease_in_out"), "{}", info.text);
        let st = info.structured.expect("structured");
        let keys = st["transform_keys"].as_array().expect("keys array");
        assert_eq!(keys.len(), 2, "{st}");
        assert!((keys[1]["scale"].as_f64().expect("scale") - 1.2).abs() < 1e-5, "{st}");
        assert_eq!(keys[1]["ease"].as_str(), Some("ease_in_out"), "{st}");
        assert!(keys[0]["time"].as_f64().is_some(), "{st}");

        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("keys 2"), "{}", tl.text);
    }

    /// A clip with no keys says `transform: none` and an empty key list.
    #[test]
    fn clip_info_without_keys_has_no_key_lines() {
        let mut srv = server();
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(info.text.contains("transform: none"), "{}", info.text);
        assert!(!info.text.contains("transform keys"), "{}", info.text);
        assert_eq!(info.structured.expect("structured")["transform_keys"], json!([]));
    }

    #[test]
    fn clip_info_describes_the_first_clip() {
        let mut srv = server();
        let first = sess(&srv).lane_clips(Lane::V1)[0].start;
        for addr in [format!("V1@{first}"), "V1#0".to_string()] {
            let r = call(&mut srv, "clip_info", json!({"clip": addr}));
            assert!(!r.is_error, "{}", r.text);
            assert!(r.text.starts_with(&format!("V1@{first} · lane V1 · video")), "{}", r.text);
            assert!(r.text.contains("test_av.mp4"), "{}", r.text);
            assert!(r.text.contains("trim start:"), "{}", r.text);
            assert!(r.text.contains("eq: none"), "{}", r.text);
            let st = r.structured.expect("structured");
            assert_eq!(st["lane"], "V1");
            assert_eq!(st["kind"], "video");
            assert_eq!(st["start"], first);
        }
    }

    #[test]
    fn clip_info_and_timeline_show_the_volume_envelope() {
        let mut srv = server();
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "move", "clip": "A1#0", "at": "2s"},
                {"op": "trim", "clip": "A1#0", "edge": "in", "to": "3s"},
                {"op": "gain_keys", "clip": "A1#0", "keys": [["3.5s", -12], ["4s", -3]]}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        // The points read back as timeline times, not the source frames they are stored on.
        let info = call(&mut srv, "clip_info", json!({"clip": "A1#0"}));
        assert!(!info.is_error, "{}", info.text);
        assert!(info.text.contains("gain keys: 3.500s -12.0 dB · 4.000s -3.0 dB"), "{}", info.text);
        // The timeline marks the clip with an envelope.
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(!tl.is_error, "{}", tl.text);
        assert!(tl.text.contains(" env"), "{}", tl.text);
    }

    #[test]
    fn clip_info_and_timeline_show_the_speed_ramp() {
        let mut srv = server();
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "speed_ramp", "clip": "V1#0", "keys": [
                    {"at": "0s", "speed": 1.0}, {"at": "1s", "speed": 1.5}, {"at": "2s", "speed": 2.0}
                ]}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        // The first and last speeds and the key count replace the constant speed.
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(!info.is_error, "{}", info.text);
        assert!(info.text.contains("speed 1.00x→2.00x (3 keys) ·"), "{}", info.text);
        assert!(!info.text.contains("speed 1x"), "{}", info.text);
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(!tl.is_error, "{}", tl.text);
        assert!(tl.text.contains("speed ramp 1.00x→2.00x (3 keys)"), "{}", tl.text);
    }

    #[test]
    fn reverse_toggles_with_on_and_shows_in_clip_info_and_timeline() {
        let mut srv = server();
        let edit = |srv: &mut Server, op: Value| crate::tools::edit::run_edit(srv, &json!({"ops": [op]}));
        // Forward clips say nothing.
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(!info.text.contains("reversed"), "{}", info.text);
        // No `on` toggles.
        let r = edit(&mut srv, json!({"op": "reverse", "clip": "V1#0"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("plays backward"), "{}", r.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(info.text.contains("· reversed ·"), "{}", info.text);
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("reversed"), "{}", tl.text);
        // The take goes with it.
        let sound = call(&mut srv, "clip_info", json!({"clip": "A1#0"}));
        assert!(sound.text.contains("reversed"), "{}", sound.text);
        // An explicit `on` that already holds changes nothing and says so.
        let r = edit(&mut srv, json!({"op": "reverse", "clip": "V1#0", "on": true}));
        assert!(!r.is_error && r.text.contains("already reversed"), "{}", r.text);
        // on:false puts it back; a bad `on` refuses; undo takes a step back.
        let r = edit(&mut srv, json!({"op": "reverse", "clip": "V1#0", "on": false}));
        assert!(!r.is_error && r.text.contains("forward again"), "{}", r.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(!info.text.contains("reversed"), "{}", info.text);
        let r = edit(&mut srv, json!({"op": "reverse", "clip": "V1#0", "on": "yes"}));
        assert!(r.is_error && r.text.contains("`on` must be true or false"), "{}", r.text);
        let u = crate::tools::edit::run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(info.text.contains("reversed"), "{}", info.text);
    }

    #[test]
    fn reverse_composes_with_speed_in_clip_info() {
        let mut srv = server();
        let r = crate::tools::edit::run_edit(
            &mut srv,
            &json!({"ops": [
                {"op": "reverse", "clip": "V1#0"},
                {"op": "speed", "clip": "V1#0", "x": 2.0}
            ]}),
        );
        assert!(!r.is_error, "{}", r.text);
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(info.text.contains("speed 2x · reversed"), "{}", info.text);
    }

    #[test]
    fn clip_info_and_timeline_show_opacity_only_when_it_is_partial() {
        let mut srv = server();
        let set = |srv: &mut Server, op: Value| {
            let s = srv.session.as_mut().expect("session");
            let r = super::super::edit_fx::apply(s, &op, 30.0).expect("transform is an edit_fx op");
            assert!(r.is_ok(), "{:?}", r.err());
        };
        // A placement at full opacity says nothing about opacity.
        set(&mut srv, json!({"op": "transform", "clip": "V1#0", "x": 0.1}));
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(!info.is_error, "{}", info.text);
        assert!(info.text.contains("transform: pos 0.1 0 "), "{}", info.text);
        assert!(!info.text.contains("opacity"), "{}", info.text);
        assert!(info.structured.expect("structured")["transform"].get("opacity").is_none());
        // Half opacity shows in the text, the JSON and the timeline row.
        set(&mut srv, json!({"op": "transform", "clip": "V1#0", "opacity": 0.5}));
        let info = call(&mut srv, "clip_info", json!({"clip": "V1#0"}));
        assert!(info.text.contains("bottom 0 opacity 0.5"), "{}", info.text);
        let st = info.structured.expect("structured");
        assert_eq!(st["transform"]["opacity"].as_f64(), Some(0.5), "{st}");
        let tl = call(&mut srv, "timeline", json!({}));
        assert!(tl.text.contains("opacity 0.5"), "{}", tl.text);
    }

    #[test]
    fn clip_info_refusals_say_what_to_do() {
        let mut srv = server();
        let no_clip = call(&mut srv, "clip_info", json!({"clip": "V1@999999"}));
        assert!(no_clip.is_error && no_clip.text.contains("no clip at"), "{}", no_clip.text);
        let not_addr = call(&mut srv, "clip_info", json!({"clip": "V1"}));
        assert!(not_addr.is_error && not_addr.text.contains("not a clip address"), "{}", not_addr.text);
        let missing = call(&mut srv, "clip_info", json!({}));
        assert!(missing.is_error && missing.text.contains("missing argument `clip`"), "{}", missing.text);
        let caption = call(&mut srv, "clip_info", json!({"clip": "S1@0"}));
        assert!(caption.is_error && caption.text.contains("no clips"), "{}", caption.text);
    }

    #[test]
    fn chapter_listing_is_youtube_text_with_its_warnings() {
        // Three chapters, each over 10 s, the first at 00:00: no warnings, MM:SS (and H:MM:SS past an hour).
        let good = vec![(0, "Intro".to_string()), (4500, "Setup".to_string()), (9000, "Done".to_string())];
        let (text, warnings) = chapter_listing(&good, 30.0, 9000 + 3000);
        assert_eq!(text, "00:00 Intro\n02:30 Setup\n05:00 Done");
        assert!(warnings.is_empty(), "{warnings:?}");
        let (hours, _) = chapter_listing(&[(0, "A".into()), (3600 * 30, "B".into()), (3600 * 30 + 900 * 30, "C".into())], 30.0, 3600 * 30 + 1800 * 30);
        assert!(hours.contains("1:00:00 B"), "{hours}");
        // Two chapters, the first not at 0, both 1 s long: four warnings, one per rule broken.
        let bad = vec![(30, "A".to_string()), (60, "B".to_string())];
        let (_, warnings) = chapter_listing(&bad, 30.0, 90);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings[0].contains("not 00:00"), "{warnings:?}");
        assert!(warnings[1].contains("\"A\"") && warnings[2].contains("\"B\""), "{warnings:?}");
        assert!(warnings[3].contains("at least 3 chapters; this timeline has 2"), "{warnings:?}");
        assert_eq!(chapter_listing(&[], 30.0, 90), (String::new(), Vec::new()));
    }

    #[test]
    fn timeline_lists_chapters_on_a_chapters_line() {
        let mut server = Server::new();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).unwrap();
        sess.add_chapter(0, "Intro").unwrap();
        sess.add_chapter(900, "Setup").unwrap();
        server.session = Some(sess);
        let r = timeline(&mut server, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("chapters: C@0 Intro (00:00) · C@900 Setup (00:30)"), "{}", r.text);
        assert!(r.text.contains("chapters: first chapter") || r.text.contains("chapters: YouTube needs at least 3"), "{}", r.text);
        let structured = r.structured.as_ref().expect("structured output");
        assert_eq!(structured["chapters"][1]["addr"], "C@900");
        assert_eq!(structured["chapters_text"], "00:00 Intro\n00:30 Setup");
    }

    #[test]
    fn timeline_lists_markers_on_a_markers_line() {
        use engine::project::{Marker, MarkerColor};
        let mut server = Server::new();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).unwrap();
        let mk = |frame, length, name: &str, note: &str, color| Marker { frame, length, name: name.into(), note: note.into(), color };
        sess.add_marker(mk(900, 0, "Fix", "tighten", MarkerColor::Red)).unwrap();
        sess.add_marker(mk(30, 60, "Range", "", MarkerColor::Green)).unwrap();
        server.session = Some(sess);
        let r = timeline(&mut server, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("markers: M@30–90f Range [green] · M@900 Fix [red] — tighten"), "{}", r.text);
        let structured = r.structured.as_ref().expect("structured output");
        assert_eq!(structured["markers"][1]["addr"], "M@900");
        assert_eq!(structured["markers"][0]["length"], 60);
        assert_eq!(structured["markers"][1]["note"], "tighten");
    }

    #[test]
    fn undo_header_counts_ledger_steps() {
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        server.ledger = vec![crate::server::Entry::new(1, "a"), crate::server::Entry::new(1, "b"), crate::server::Entry::new(1, "c")];
        server.redo_ledger = vec![crate::server::Entry::new(1, "d")];
        let r = timeline(&mut server, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("undo 3 · redo 1"), "{}", r.text);
    }

    #[test]
    fn source_span_is_source_time_at_the_source_rate() {
        // A clip at timeline frames 125..275 at 30 fps: source time 4.167 s to 9.167 s.
        let same = source_span_line(125, 275, Some(Path::new("/x/a.mp4")), 30.0, 30.0);
        assert_eq!(same, "source in 00:04.167 to out 00:09.167 · 150 source frames at 30");
        let slower = source_span_line(125, 275, Some(Path::new("/x/a.mp4")), 30.0, 23.976);
        assert!(slower.ends_with("· 120 source frames at 23.976"), "{slower}");
        let still = source_span_line(125, 275, Some(Path::new("/x/a.png")), 30.0, 30.0);
        assert!(still.ends_with("· still"), "{still}");
    }

    /// A lead-in is named with three decimals, as clip times are: 7.967 s is not 8.0 s.
    #[test]
    fn a_lead_in_is_named_to_the_clip_time_precision() {
        assert_eq!(lead_in_note(0, 30.0), "");
        assert_eq!(lead_in_note(239, 30.0), " (incl. 7.967s lead-in)");
    }

    /// The source end is capped at the file's real duration, and a whole second reads as itself.
    #[test]
    fn a_source_time_past_the_real_duration_reads_the_duration() {
        assert_eq!(src_secs_text(6.0), "6.0s");
        assert_eq!(src_secs_text(5.968), "5.968s");
        let (_, end) = src_range_secs(0, 150, 25.0, 25.0, Some(5.968));
        assert_eq!(src_secs_text(end), "5.968s");
        let (start, end) = src_range_secs(0, 125, 25.0, 25.0, Some(5.968));
        assert_eq!((src_secs_text(start), src_secs_text(end)), ("0.0s".to_string(), "5.0s".to_string()));
    }

    /// A 2 s clip at 30 fps on a 23.976 fps source reads its source range in the source's own
    /// frames: its last timeline frame shows source frame 47, so the range is 48 frames and ends at
    /// 2.002s (48 / 23.976), as the edit reply says, and not at the clip's 2.0 s length.
    #[test]
    fn a_two_second_clip_reads_its_source_frames_in_source_time() {
        let (start, end) = src_range_secs(0, 60, 30.0, 23.976, None);
        assert_eq!((src_secs_text(start), src_secs_text(end)), ("0.0s".to_string(), "2.002s".to_string()));
    }

    /// The timeline's label for a 601-frame clip at 30 fps on a 23.976 fps source is the source's
    /// own 480 frames: `0.0s–20.02s` (480 / 23.976), not the clip's 20.033 s length at 30 fps.
    #[test]
    fn a_clip_of_480_source_frames_ends_at_their_own_time() {
        let (start, end) = src_range_secs(0, 601, 30.0, 23.976, None);
        assert_eq!((src_secs_text(start), src_secs_text(end)), ("0.0s".to_string(), "20.02s".to_string()));
    }

    /// A whole 23.976 fps file (48 frames) placed at 30 fps: its out rounds up to timeline frame 61,
    /// which is past the file's last frame, so the count is the file's 48, not 49.
    #[test]
    fn a_whole_file_counts_its_own_frames() {
        let asset = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_23976fps.mp4");
        let whole = source_span_line(0, 61, Some(&asset), 30.0, 23.976);
        assert!(whole.ends_with("· 48 source frames at 23.976"), "{whole}");
    }

    /// A 25 fps source in a 30 fps project: `place` reports the source time it plays, and the
    /// timeline row, clip_info and the source-span line must agree with it.
    #[test]
    fn a_25_fps_source_reads_the_same_in_place_timeline_and_clip_info() {
        engine::ao::set_silent_output();
        // in 0.52s and in 0.5s both round to source frame 13 (0.5s is 12.5, ties up), stored as
        // timeline frame 16, which shows floor(16 * 25 / 30) = 13 at 0.520s. A one second clip
        // (30 timeline frames) then ends on frame 46, which shows 1.520s.
        let cases = [("0.52s", "0.520s", "1.520s", "00:00.520", "00:01.520"), ("0.5s", "0.520s", "1.520s", "00:00.520", "00:01.520")];
        for (input, src_in, src_out, clock_in, clock_out) in cases {
            let mut sess = engine::PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
            sess.import(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_25fps.mp4"))
                .expect("import 25 fps asset");
            let mut srv = Server { session: Some(sess), ..Server::default() };
            let ed = crate::help::catalogue().into_iter().find(|t| t.name == "edit").expect("edit tool");
            let args = json!({"ops": [{"op": "place", "source": 0, "lane": "V1", "at": "0s", "in": input, "length": "1s"}]});
            let placed = (ed.run)(&mut srv, &args);
            assert!(!placed.is_error, "{}", placed.text);
            assert!(placed.text.contains(&format!("src {src_in}")), "place reply: {}", placed.text);

            let t = call(&mut srv, "timeline", json!({"lanes": ["V1"]}));
            assert!(!t.is_error, "timeline: {}", t.text);
            // The timeline prints the compact form of the same time: 0.520s is `0.52s`.
            let compact = |t: &str| format!("{}s", t.trim_end_matches('s').trim_end_matches('0'));
            let want = format!("src {}–{}", compact(src_in), compact(src_out));
            assert!(t.text.contains(&want), "timeline: {}", t.text);

            let info = call(&mut srv, "clip_info", json!({"clip": "V1@0"}));
            assert!(!info.is_error, "clip_info: {}", info.text);
            assert!(info.text.contains(&format!("source in {clock_in} to out {clock_out}")), "clip_info: {}", info.text);
        }
    }

    /// A lane holding two ducks (one per key) shows both in its header, in timeline and in the
    /// project status, with the structured lists carrying each and `duck` the first.
    #[test]
    fn every_duck_on_a_lane_is_shown() {
        use engine::duck::Duck;
        use engine::project::LaneKind;
        let mut srv = server();
        let s = srv.session.as_mut().unwrap();
        s.add_lane(LaneKind::Audio);
        s.add_lane(LaneKind::Audio);
        let duck = |under| Duck { under, depth_db: -12.0, threshold_db: -40.0, attack_ms: 80.0, release_ms: 400.0 };
        assert!(s.set_lane_duck(Lane::A1, Some(duck(1))), "duck under A2 set");
        assert!(s.set_lane_duck(Lane::A1, Some(duck(2))), "duck under A3 set");
        assert_eq!(s.lane_duck_list(Lane::A1).len(), 2);

        let t = call(&mut srv, "timeline", json!({}));
        assert!(t.text.contains("duck under A2 -12dB") && t.text.contains("duck under A3 -12dB"), "{}", t.text);
        let lanes = t.structured.as_ref().unwrap()["lanes"].as_array().unwrap().clone();
        let a1 = lanes.iter().find(|l| l["lane"] == "A1").expect("A1 listed");
        assert_eq!(a1["duck"]["under"], "A2", "duck is the first one");
        let ducks = a1["ducks"].as_array().expect("ducks list");
        assert_eq!(ducks.len(), 2);
        assert_eq!(ducks[1]["under"], "A3");

        let status = crate::server::tools().into_iter().find(|t| t.name == "project_status").expect("project_status is registered");
        let st = (status.run)(&mut srv, &json!({}));
        assert!(st.text.contains("ducks: A1 under A2 -12dB · A1 under A3 -12dB"), "{}", st.text);
        let listed = st.structured.as_ref().unwrap()["ducks"].as_array().unwrap().clone();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1]["under"], "A3");
    }

    #[test]
    fn duck_shows_in_the_lane_header_and_on_the_clip() {
        use engine::duck::Duck;
        use engine::project::LaneKind;
        let mut srv = server();
        let key = srv.session.as_mut().unwrap().add_lane(LaneKind::Audio);
        assert_eq!(key, Lane::new(LaneKind::Audio, 1));
        let duck = Duck {
            under: 1,
            depth_db: -12.0,
            threshold_db: -40.0,
            attack_ms: 80.0,
            release_ms: 400.0,
        };
        assert!(srv.session.as_mut().unwrap().set_lane_duck(Lane::A1, Some(duck)), "duck set");

        let t = call(&mut srv, "timeline", json!({}));
        assert!(!t.is_error, "{}", t.text);
        assert!(t.text.contains("duck under A2 -12dB"), "{}", t.text);
        let lanes = t.structured.as_ref().unwrap()["lanes"].as_array().unwrap().clone();
        let a1 = lanes.iter().find(|l| l["lane"] == "A1").expect("A1 listed");
        assert_eq!(a1["duck"]["under"], "A2");
        assert_eq!(a1["duck"]["depth_db"], -12.0);
        assert_eq!(a1["duck"]["attack_ms"], 80.0);
        let v1 = lanes.iter().find(|l| l["lane"] == "V1").expect("V1 listed");
        assert!(v1["duck"].is_null(), "only the ducked lane carries a duck");

        let first = sess(&srv).lane_clips(Lane::A1)[0].start;
        let info = call(&mut srv, "clip_info", json!({"clip": format!("A1@{first}")}));
        assert!(!info.is_error, "{}", info.text);
        assert!(info.text.contains("duck under A2 -12dB"), "{}", info.text);
        assert_eq!(info.structured.as_ref().unwrap()["duck"]["under"], "A2");
    }

    #[test]
    fn timeline_lists_titles_after_the_lanes_and_in_json() {
        use engine::project::Title;
        use engine::text::{Outline, Place, TextStyle};
        let mut srv = server();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let style = TextStyle { size: 80, outline: Outline::Thin, boxed: false, place: Place::Centre, ..TextStyle::default() };
        let sess = srv.session.as_mut().unwrap();
        let long = "x".repeat(60);
        sess.add_title(Title { start: 0, frames: 90, text: "Episode 17".into(), style: style.clone(), fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 })
            .unwrap();
        sess.add_title(Title { start: 150, frames: 30, text: long.clone(), style, fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 }).unwrap();

        let r = call(&mut srv, "timeline", json!({}));
        let t1 = format!("T  T1 0.000–{:.3} \"Episode 17\" centre", 90.0 / fps);
        assert!(r.text.contains(&t1), "{}", r.text);
        assert!(r.text.contains(&format!("\"{}…\"", "x".repeat(39))), "{}", r.text);
        let lane_at = r.text.find("V1").expect("a lane line");
        assert!(r.text.find("T  T1").unwrap() > lane_at, "titles come after the lanes");
        let titles = r.structured.as_ref().unwrap()["titles"].as_array().unwrap().clone();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[1]["text"], long.as_str());
        assert_eq!(titles[0]["place"], "centre");

        // the range filter applies to titles too: only T1 overlaps the first second
        let r = call(&mut srv, "timeline", json!({"range": {"from": "0", "to": "1.0s"}}));
        assert_eq!(r.structured.as_ref().unwrap()["titles"].as_array().unwrap().len(), 1);
        assert!(!r.text.contains("T  T2"), "{}", r.text);
    }
}
