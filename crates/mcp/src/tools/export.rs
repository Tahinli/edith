//! Export tools: start, poll, and cancel exports.
use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::demux::Demuxer;
use engine::export::{caption_title_clash, chroma_reduction_words, fit_picture_clips, planned_seats, planned_tone, EncoderSeat, ExportSettings, FitOutcome, Format, HdrOutput, HdrTransfer, GifSettings, HevcSpeed, ProResProfile, AUDIO_KBPS};
use engine::project::LaneKind;
use engine::scale::FitPolicy;
use engine::captionanim::{CaptionAnim, CaptionStyle, MAX_WORDS_PER_LINE};
use engine::text::{Outline, Place, TextStyle};
use engine::PlaybackSession;
use serde_json::{json, Map, Value};

use crate::parse::{frame, session_end, session_fps, tc};
use crate::server::{arg_str, opt_bool, opt_str, opt_u64, tri, FileLevels, Job, Pending, Reply, Server, Tool};

const FORMAT_NAMES: &str = "mp4, av1, av1-mp4, hevc, hevc-mp4, webm, wav, flac, mp3, ogg, m4a, opus, prores, png_sequence, still, gif";
/// How many finished exports an unasked status poll lists (the newest ones).
const FINISHED_LISTED: usize = 5;

/// The argument key of burned-in captions, shared with `frame` and `contact_sheet`.
pub(crate) const BURN_KEY: &str = "burn_captions";
const BURN_KEYS: [&str; 11] = ["track", "size", "outline", "boxed", "place", "family", "animation", "highlight", "box", "uppercase", "words_per_line"];
const BURN_DOC: &str = "Draw one subtitle track's text into the picture, so it shows in every \
player and in WebM. Text cues only: a bitmap track (PGS) is refused. Compare `subtitles`, which \
adds soft tracks a player can switch off; WebM carries no soft subtitles, so burn there. \
One track only: two speakers' captions burn together when they share a track, which auto_captions makes by merging a second speaker into the same caption lane (auto_captions {target:\"A2\", lane:\"S1\"}).";

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "export_start",
            description: "Start exporting the open project to a file. Returns job:<n> at once; poll it with export_status. Refuses an existing file unless overwrite is true. Two ways to carry captions: `subtitles` adds soft tracks a player can switch off (WebM carries none); `burn_captions` draws one track's text into the picture, so every player and WebM shows it. With `burn_captions` and no `subtitles`, the file has no soft subtitle track at all, so the captions are not shown twice; name `subtitles` as well to add soft tracks beside the burned ones. The export keeps running while you do other things; if the client only closes this server's input it finishes before the server exits, but if the server is stopped by a signal (as most clients do when they quit) it is cancelled and its partial file removed — poll export_status until it finishes.",
            schema: start_schema,
            run: export_start,
        },
        Tool {
            name: "export_presets",
            description: "List the export presets export_start { preset: \"<id>\" } takes: id, label and the settings each sets (format, bitrate_kbps, audio_kbps, size, bit_depth, loudness). Explicit export_start arguments override the preset.",
            schema: presets_schema,
            run: export_presets,
        },
        Tool {
            name: "export_captions",
            description: "Write the timeline's captions (every cue on the subtitle lanes, in start order) to a text file: SRT or WebVTT. Picture cues and empty ones are left out. Writes nothing and says so when there are no captions. Refuses an existing file unless overwrite is true.",
            schema: captions_schema,
            run: export_captions,
        },
        Tool {
            name: "export_status",
            description: "Progress, elapsed time, ETA, encoder and outcome of one export (job), or with no job of every unfinished export and the last five finished ones (an older finished export is named by job:<n>). A stage before the encode (Measuring shake, while a stabilised clip is measured) is named with its own percent. Reports the file's size when done, its average bitrate for an HEVC or AV1 file, or the error when it failed.",
            schema: status_schema,
            run: export_status,
        },
        Tool {
            name: "export_cancel",
            description: "Cancel a running export. The worker stops at its next checkpoint and deletes the partial file.",
            schema: cancel_schema,
            run: export_cancel,
        },
    ]
}

fn presets_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The JSON of one preset row, the settings in `export_start`'s own argument names.
fn preset_json(p: &engine::export_presets::Preset) -> Value {
    json!({
        "id": p.id,
        "label": p.label,
        "description": p.description,
        "category": format!("{:?}", p.target.category).to_lowercase(),
        "output": if p.target.output == engine::export_presets::Output::Video { "video" } else { "audio" },
        "format": format_name(p.format),
        "bitrate_kbps": p.bitrate.map(|b| b / 1000),
        "audio_kbps": p.audio_kbps,
        "size": p.target.size.map(|(w, h)| format!("{w}x{h}")),
        "bit_depth": p.video_bit_depth,
        "prores_profile": p.prores_profile.map(prores_name),
        "hdr": hdr_name(p.hdr),
        "loudness": p.loudness.map(|l| json!({ "lufs": l.lufs, "true_peak_dbtp": l.true_peak_dbtp })),
    })
}

fn export_presets(_srv: &mut Server, _args: &Value) -> Reply {
    let rows: Vec<Value> = engine::export_presets::presets().iter().map(preset_json).collect();
    let mut text = String::from("export presets (export_start { preset: id }; explicit arguments override):");
    for p in engine::export_presets::presets() {
        text.push_str(&format!("\n  {} -- {}", p.id, p.description));
    }
    Reply::ok(text).with(json!({ "presets": rows }))
}

fn captions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "format": {"type": "string", "enum": ["srt", "vtt"], "description": "srt = SubRip (00:00:01,000); vtt = WebVTT with its WEBVTT header (00:00:01.000)."},
            "path": {"type": "string", "description": "Path of the file to write. A leading ~/ is expanded; its folder must exist and its extension must match the format. Omitted: <first source's name> edit.<format> in the Videos folder (Untitled when there are no sources)."},
            "overwrite": {"type": "boolean", "description": "Replace the file when it already exists. Default false."}
        },
        "required": ["format"],
        "additionalProperties": false
    })
}

/// File > Export captions: the timeline's caption cues as SRT or WebVTT, written at `path`.
fn export_captions(srv: &mut Server, args: &Value) -> Reply {
    let format = match opt_str(args, "format") {
        Some(f @ ("srt" | "vtt")) => f,
        _ => return Reply::err("format is srt or vtt".to_string()),
    };
    let s = tri!(srv.session_ref());
    let cues = s.caption_cues();
    if cues.is_empty() {
        return Reply::err("no captions on the timeline: add a caption on a subtitle lane first".to_string());
    }
    let path = match opt_str(args, "path") {
        Some(raw) => crate::tools::project::expand_home(raw),
        None => {
            let stem = s.sources().first().and_then(|src| src.path.file_stem()).map_or_else(|| "Untitled".to_string(), |n| format!("{} edit", n.to_string_lossy()));
            engine::export::default_export_folder(Path::new("")).join(format!("{stem}.{format}"))
        }
    };
    if path.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case(format)) {
        return Reply::err(format!("path {} must end in .{format} for {format}", path.display()));
    }
    if !path.parent().is_some_and(|d| d.as_os_str().is_empty() || d.is_dir()) {
        return Reply::err(format!("the folder of {} does not exist", path.display()));
    }
    if path.exists() && !opt_bool(args, "overwrite").unwrap_or(false) {
        return Reply::err(format!("{} already exists: pass overwrite true to replace it, or choose another path", path.display()));
    }
    let text = if format == "srt" { engine::subtitle::write_srt(&cues) } else { engine::subtitle::write_vtt(&cues) };
    if let Err(e) = std::fs::write(&path, text) {
        return Reply::err(format!("could not write {}: {e}", path.display()));
    }
    Reply::ok(format!("wrote {} caption{} to {} ({format})", cues.len(), if cues.len() == 1 { "" } else { "s" }, path.display()))
        .with(json!({"path": path.display().to_string(), "format": format, "cues": cues.len()}))
}

fn start_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sequence": super::sequence::prop(),
            "preset": {
                "type": "string",
                "enum": engine::export_presets::presets().iter().map(|p| p.id).collect::<Vec<_>>(),
                "description": "An export preset id (list them with export_presets): fills format, bitrate_kbps, audio_kbps, size, bit_depth and, for sound, a loudness target. Anything else you pass overrides the preset's value; `out`'s extension must match the preset's format."
            },
            "loudness": {
                "type": "number",
                "minimum": -30,
                "maximum": -5,
                "description": "Integrated loudness target in LUFS, any number from -30 to -5 (-14 is the usual streaming level, -16 the podcast preset). The whole mix takes one gain so it lands on this, and the true peak is held at the preset's ceiling or -1 dBTP; the gain is cut back, never a limiter. Omitted: the preset's target, or none."
            },
            "out": {
                "type": "string",
                "description": "Path of the file to write. A leading ~/ is expanded. Its folder must exist. Its extension names the format unless `format` is given: .mp4 is mp4, .mkv is av1, .webm, .wav, .flac, .mp3, .ogg, .m4a, .opus."
            },
            "format": {
                "type": "string",
                "enum": ["mp4", "av1", "av1-mp4", "hevc", "hevc-mp4", "webm", "wav", "flac", "mp3", "ogg", "m4a", "opus", "prores", "png_sequence", "still", "gif"],
                "description": "png_sequence = a folder of PNG pictures, one per frame (name_00001.png ...), at the project size, no sound: `out` is the folder; still = one PNG, the frame at the start of `range` (the poster frame); gif = animated GIF of the range (see gif_width, gif_fps, gif_loop; at most 30 s and 720 px wide, no sound). mp4 = H.264 in mp4; av1 = AV1 in Matroska (.mkv); av1-mp4 = AV1 in mp4; hevc = HEVC in Matroska, I+P GOPs (.mkv); hevc-mp4 = HEVC in mp4; webm; wav/flac = audio only, lossless; mp3/ogg = audio only; m4a = AAC in an mp4 box, audio only; opus = Opus in Ogg, audio only, mixed at 48 kHz; prores = Apple ProRes in a QuickTime .mov with PCM sound (see prores_profile, audio_bit_depth). An mp4 of a timeline with no video clips is the sound alone (AAC in an mp4), so it is not refused. Default: inferred from the out extension."
            },
            "range": {
                "type": "object",
                "description": "Part of the timeline to write, from `from` up to but not including `to`. Times take the usual forms: 450f, 15.0s, 15 (seconds), 00:00:15:00, end. Omitted: the whole timeline.",
                "properties": {
                    "from": { "description": "Start of the range." },
                    "to": { "description": "End of the range (exclusive); must come after `from` and not pass the timeline end." }
                },
                "required": ["from", "to"]
            },
            "gif_width": {
                "type": "integer",
                "minimum": 1,
                "description": "GIF only: width in pixels; the height follows the project's aspect. Default 480."
            },
            "gif_fps": {
                "type": "integer",
                "minimum": 1,
                "maximum": 60,
                "description": "GIF only: frames a second, 1 to 60. Default 15."
            },
            "gif_loop": {
                "type": "boolean",
                "description": "GIF only: loop forever (true, the default) or play once (false)."
            },
            "size": {
                "type": "string",
                "description": "Picture size of this export only, e.g. 1280x720, or a height with p: 720p, 1080p, 2160p (16:9 at that height). Both sides even, at most 7680. Omitted: the project's resolution. The project is not changed: the export draws at this size and its clips refit by their fit mode, as they do when the project's resolution changes."
            },
            "fit": {
                "type": "string",
                "enum": ["keep", "fit", "fill"],
                "description": "How every picture clip meets the export's canvas, for this export only (the project's clips keep their own fit). fill crops each picture to the whole canvas, so 16:9 footage fills a vertical 1080x1920; fit keeps the whole picture with bars; keep (the default) uses each clip's own fit. Captions and titles keep their place on the canvas."
            },
            "bitrate_kbps": {
                "type": "integer",
                "minimum": 1,
                "description": "Video bitrate in kbps. Omitted: chosen from the picture size and frame rate. Ignored by audio-only formats."
            },
            "audio_kbps": {
                "type": "integer",
                "enum": [128, 192, 256, 320],
                "description": "Sound bitrate in kbps. Refused where the sound's rate is not a choice (wav, flac, a timeline with no sound, and similar)."
            },
            "seat": {
                "type": "string",
                "enum": ["auto", "hardware", "software"],
                "description": "Which encoder writes the picture. auto: hardware where this machine has one for the codec, software elsewhere. hardware: the GPU or nothing, no silent fallback. software: always the CPU."
            },
            "intra_only": {
                "type": "boolean",
                "description": "hevc and hevc-mp4: true codes every picture as an IDR (intra only, large files, every frame a cut point). false, the default, codes I+P GOPs (an IDR a second, P pictures between). h264 mp4 uses it the same way."
            },
            "subtitles": {
                "anyOf": [
                    { "type": "boolean", "description": "true: every subtitle track as a soft track. false: none." },
                    { "type": "array", "items": { "type": "integer", "minimum": 0 }, "description": "Indices of the project's subtitle tracks to include as soft tracks, in the order given." }
                ],
                "description": "Soft subtitle tracks, opt-in: omitted, false or [] writes none, true writes every track, a list writes those indices. For captions every player shows, use burn_captions instead."
            },
            "burn_captions": burn_schema(),
            "chapters": {
                "type": ["boolean", "string"],
                "description": "Write the project's chapter markers into the file. Default true. Written to mp4, av1-mp4, hevc-mp4, m4a, av1 (.mkv), hevc (.mkv), webm and opus (as CHAPTERxxx comments); the other formats say they are not written. false leaves them out. \"markers\" writes the comment markers (marker_add) as the chapter list instead of the chapters."
            },
            "bit_depth": {
                "type": "integer",
                "enum": [8, 10],
                "description": "Sample depth of the picture: 10 writes a 10-bit AV1 or HEVC (Main 10) file (format av1, av1-mp4, hevc or hevc-mp4 only; H.264 and the rest refuse it). Omitted: 10-bit when the source is 10-bit, 8-bit otherwise. 8 forces 8-bit. Compositions (grades, titles, captions, transitions) are still composed at 8 bits and shifted up."
            },
            "prores_profile": {
                "type": "string",
                "enum": ["proxy", "lt", "standard", "hq", "4444", "4444xq"],
                "description": "format prores only: the ProRes profile, which is the quality (the bitrate is the profile's own). proxy, lt, standard (422) and hq (the default, 422 HQ) are 4:2:2; 4444 and 4444xq are 4:4:4. ProRes is always 10-bit."
            },
            "hevc_speed": {
                "type": "string",
                "enum": ["default", "fast", "faster"],
                "description": "hevc and hevc-mp4 only: the software encoder's speed preset. default (the default) is the full search; fast is about 1.2-1.6x quicker for 0-3% more bits at the same quality; faster is about 1.5-2x quicker for up to 27% more."
            },
            "hdr": {
                "type": "string",
                "enum": ["off", "pq", "hlg"],
                "description": "hevc and hevc-mp4 only: tag the file HDR. pq is HDR10 (SMPTE ST 2084), hlg is ARIB STD-B67; both BT.2020, 10-bit (bit_depth 8 is refused), and HDR sources are not tone-mapped. off (the default) is the SDR file it always was."
            },
            "audio_bit_depth": {
                "type": "integer",
                "enum": [16, 24],
                "description": "format prores only: bits per sample of the PCM sound. 24 is the default."
            },
            "overwrite": {
                "type": "boolean",
                "description": "Replace the file at `out` if it already exists. Default false."
            },
            "queue": {
                "type": "boolean",
                "description": "true: line this export up behind the others. It keeps its own copy of the timeline as it is now (later edits do not change it) and starts when every earlier export has finished, one encode at a time; export_status shows it as waiting. Default false: the export starts at once, beside any others running."
            }
        },
        "required": ["out"]
    })
}

fn status_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job": {
                "type": "integer",
                "minimum": 1,
                "description": "The job number from export_start (job:<n>). Omitted: every export so far."
            },
            "out": {
                "type": "string",
                "description": "The file an export writes (the out path given to export_start). Use it for an export this session did not start: the reply says whether the file is there, its size and when it was written. A job this session started for that file is reported as its job."
            }
        }
    })
}

fn cancel_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "job": {
                "type": "integer",
                "minimum": 1,
                "description": "The job number from export_start (job:<n>). A waiting (queued) job is removed from the queue and never starts; a running one stops and its partial file is deleted."
            }
        },
        "required": ["job"]
    })
}

// ---- formats and paths ----------------------------------------------------

fn format_from_name(s: &str) -> Option<Format> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "mp4" => Format::Mp4,
        "av1" => Format::Av1,
        "av1-mp4" => Format::Av1Mp4,
        "hevc" => Format::Hevc,
        "hevc-mp4" => Format::HevcMp4,
        "webm" => Format::Webm,
        "wav" => Format::Wav,
        "flac" => Format::Flac,
        "mp3" => Format::Mp3,
        "ogg" => Format::Ogg,
        "opus" => Format::Opus,
        "m4a" => Format::M4a,
        "prores" => Format::ProRes,
        "png_sequence" => Format::PngSequence,
        "still" => Format::Still,
        "gif" => Format::Gif,
        _ => return None,
    })
}

pub(crate) fn parse_prores_profile(args: &Value, format: Format) -> Result<Option<ProResProfile>, String> {
    let Some(name) = opt_str(args, "prores_profile") else { return Ok(None) };
    let profile = match name.trim().to_ascii_lowercase().as_str() {
        "proxy" => ProResProfile::Proxy,
        "lt" => ProResProfile::Lt,
        "standard" => ProResProfile::Standard,
        "hq" => ProResProfile::Hq,
        "4444" => ProResProfile::P4444,
        "4444xq" => ProResProfile::P4444Xq,
        other => return Err(format!("prores_profile `{other}` is not proxy, lt, standard, hq, 4444 or 4444xq")),
    };
    match format.is_prores() {
        true => Ok(Some(profile)),
        false => Err(format!("prores_profile is for format prores, not {}", format_name(format))),
    }
}

pub(crate) fn parse_hevc_speed(args: &Value, format: Format) -> Result<Option<HevcSpeed>, String> {
    let Some(name) = opt_str(args, "hevc_speed") else { return Ok(None) };
    let speed = match name.trim().to_ascii_lowercase().as_str() {
        "default" => HevcSpeed::Default,
        "fast" => HevcSpeed::Fast,
        "faster" => HevcSpeed::Faster,
        other => return Err(format!("hevc_speed `{other}` is not default, fast or faster")),
    };
    match format.is_hevc() {
        true => Ok(Some(speed)),
        false => Err(format!("hevc_speed is for format hevc or hevc-mp4, not {}", format_name(format))),
    }
}

pub(crate) fn parse_hdr(args: &Value, format: Format) -> Result<Option<HdrTransfer>, String> {
    let Some(name) = opt_str(args, "hdr") else { return Ok(None) };
    let transfer = match name.trim().to_ascii_lowercase().as_str() {
        "off" => HdrTransfer::Sdr,
        "pq" => HdrTransfer::Pq,
        "hlg" => HdrTransfer::Hlg,
        other => return Err(format!("hdr `{other}` is not off, pq or hlg")),
    };
    match transfer == HdrTransfer::Sdr || format.is_hevc() {
        true => Ok(Some(transfer)),
        false => Err(format!("hdr is for format hevc or hevc-mp4, not {}", format_name(format))),
    }
}

pub(crate) fn parse_sound_bits(args: &Value, format: Format) -> Result<Option<u8>, String> {
    let bits = match opt_u64(args, "audio_bit_depth") {
        None => return Ok(None),
        Some(b @ (16 | 24)) => b as u8,
        Some(b) => return Err(format!("audio_bit_depth {b} is not 16 or 24")),
    };
    match format.is_prores() {
        true => Ok(Some(bits)),
        false => Err(format!("audio_bit_depth is for format prores, not {}", format_name(format))),
    }
}

fn prores_name(p: ProResProfile) -> &'static str {
    match p {
        ProResProfile::Proxy => "proxy",
        ProResProfile::Lt => "lt",
        ProResProfile::Standard => "standard",
        ProResProfile::Hq => "hq",
        ProResProfile::P4444 => "4444",
        ProResProfile::P4444Xq => "4444xq",
    }
}

fn hdr_name(t: HdrTransfer) -> &'static str {
    match t {
        HdrTransfer::Sdr => "off",
        HdrTransfer::Pq => "pq",
        HdrTransfer::Hlg => "hlg",
    }
}

fn format_name(f: Format) -> &'static str {
    match f {
        Format::Mp4 => "mp4",
        Format::Av1 => "av1",
        Format::Av1Mp4 => "av1-mp4",
        Format::Hevc => "hevc",
        Format::HevcMp4 => "hevc-mp4",
        Format::Webm => "webm",
        Format::Wav => "wav",
        Format::Flac => "flac",
        Format::Mp3 => "mp3",
        Format::Ogg => "ogg",
        Format::Opus => "opus",
        Format::M4a => "m4a",
        Format::ProRes => "prores",
        Format::PngSequence => "png_sequence",
        Format::Still => "still",
        Format::Gif => "gif",
    }
}

/// The format a bare extension implies when no format was named. `.mkv` is
/// AV1, the default video format; HEVC needs its name.
fn infer_format(ext: &str) -> Option<Format> {
    Some(match ext {
        "mp4" => Format::Mp4,
        "mkv" => Format::Av1,
        "webm" => Format::Webm,
        "wav" => Format::Wav,
        "flac" => Format::Flac,
        "mp3" => Format::Mp3,
        "ogg" => Format::Ogg,
        "opus" => Format::Opus,
        "m4a" => Format::M4a,
        "mov" => Format::ProRes,
        "png" => Format::Still,
        "gif" => Format::Gif,
        _ => return None,
    })
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(raw)
}

/// The path to write and the format it is written in. Refuses a named format
/// whose extension the path does not carry.
fn resolve_out(raw: &str, format: Option<&str>) -> Result<(PathBuf, Format), String> {
    let path = expand_home(raw);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let named = match format {
        Some(name) => Some(format_from_name(name).ok_or_else(|| {
            format!("unknown format `{name}`: the formats are {FORMAT_NAMES}")
        })?),
        None => None,
    };
    match (named, ext) {
        // A numbered run is written into a folder: the name is the folder's, with no extension added.
        (Some(Format::PngSequence), _) => Ok((path, Format::PngSequence)),
        (Some(f), None) => Ok((path.with_extension(f.ext()), f)),
        (Some(f), Some(e)) if e == f.ext() => Ok((path, f)),
        (Some(f), Some(e)) => Err(format!(
            "`{}` ends in .{e}, but {} is written as .{}: name the file .{} or pick a format that writes .{e}",
            path.display(),
            format_name(f),
            f.ext(),
            f.ext()
        )),
        (None, Some(e)) => match infer_format(&e) {
            Some(f) => Ok((path, f)),
            None => Err(format!(
                "can't tell the format from `.{e}`: name the file .mp4, .mkv, .webm, .wav, .flac, .mp3, .ogg, .m4a, .opus or .mov, or pass `format` ({FORMAT_NAMES})"
            )),
        },
        (None, None) => Err(format!(
            "`{}` has no extension, so the format is unknown: name the file with one (.mp4, .mkv, .webm, .wav, .flac, .mp3, .ogg, .m4a, .opus, .mov) or pass `format`",
            path.display()
        )),
    }
}

/// Whether `path` may be written: not an existing file unless `overwrite`,
/// not a folder, and in a folder that exists and takes a new file.
fn check_out(path: &Path, overwrite: bool) -> Result<(), String> {
    if path.is_dir() {
        return Err(format!("`{}` is a folder: give the file a name inside it", path.display()));
    }
    if path.exists() && !overwrite {
        return Err(format!(
            "`{}` already exists: pass overwrite: true to replace it, or choose another name",
            path.display()
        ));
    }
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() && !p.is_dir() => Err(format!(
            "the folder `{}` does not exist: create it first or choose an existing folder",
            p.display()
        )),
        Some(p) if !p.as_os_str().is_empty() => writable(p),
        _ => writable(Path::new(".")),
    }
}

/// Whether a picture run may be written into `folder`: not a file, and not a folder that already
/// holds files unless `overwrite`, in a parent folder that exists. The run creates the folder.
fn check_folder(folder: &Path, overwrite: bool) -> Result<(), String> {
    if folder.exists() && !folder.is_dir() {
        return Err(format!("`{}` is a file: name a folder for the picture run", folder.display()));
    }
    let occupied = folder.is_dir() && folder.read_dir().is_ok_and(|mut d| d.next().is_some());
    if occupied && !overwrite {
        return Err(format!(
            "`{}` already holds files: pass overwrite: true to write the run into it, or choose another name",
            folder.display()
        ));
    }
    match folder.parent() {
        Some(p) if !p.as_os_str().is_empty() && !p.is_dir() => Err(format!(
            "the folder `{}` does not exist: create it first or choose an existing folder",
            p.display()
        )),
        Some(p) if !p.as_os_str().is_empty() => writable(p),
        _ => writable(Path::new(".")),
    }
}

/// Proves a folder takes a new file by making one there and removing it: the mode
/// bits lie about a read-only mount, a full disk or a `/proc` folder, so the answer
/// is the attempt itself. Refused up front rather than after the job has run.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".edith-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "cannot create files in `{}`: {e}. choose a file in a folder you can write to",
            dir.display()
        )),
    }
}

/// The engine's rule for the sound a job reads, checked before the job starts so a
/// mismatch is refused in these words rather than failing inside it. The timeline
/// takes its width from its first audio source; a mono source joins a wider timeline
/// (it is widened), a stereo pair folds down onto a mono timeline (it is averaged), and
/// a silent one joins any. Anything else is refused here.
fn channel_mismatch(s: &PlaybackSession, fps: f64) -> Option<String> {
    let sources = s.sources();
    let probe = |i: usize| -> Option<engine::AudioProbe> {
        let src = sources.get(i)?;
        if engine::is_image(&src.path) {
            return None;
        }
        engine::AudioSession::probe(&src.path, src.audio_stream).ok().flatten()
    };
    let (first_at, timeline) = (0..sources.len()).find_map(|i| Some((i, probe(i)?)))?;
    let width = timeline.channels;
    let name = |i: usize| {
        sources[i].path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    for lane in s.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio) {
        for clip in s.lane_clips(lane) {
            let Some(p) = probe(clip.source) else {
                continue; // a silent source is silence, whatever the timeline is
            };
            // The engine's own gates ([`engine::audio`]'s export check): a mono
            // source joins a wider timeline, a stereo pair folds onto a mono one.
            if p.channels == width || (p.channels == 1 && width > 1) || (p.channels == 2 && width == 1) {
                continue;
            }
            return Some(format!(
                "{} at {} plays `{}` ({} ch), but the timeline is {width} ch, set by its first audio source `{}`: \
an export would fail on it. Remove that clip, or remove `{}` with media_remove to let another source set the width",
                crate::parse::lane_name(lane),
                crate::parse::clock(clip.start, fps),
                name(clip.source),
                p.channels,
                name(first_at),
                name(first_at),
            ));
        }
    }
    None
}

/// The open project's folder, the one a relative `out` is resolved against.
fn project_dir(srv: &Server) -> Option<PathBuf> {
    let file = std::path::absolute(srv.path.as_ref()?).ok()?;
    file.parent().map(Path::to_path_buf)
}

/// Makes `raw` absolute. A relative path is taken from the project's folder, never the
/// server's working directory, and is refused when no project is open. Returns the path
/// and, when it was resolved against the project, a note saying so.
fn anchor_out(srv: &Server, raw: &str) -> Result<(String, Option<String>), String> {
    let p = expand_home(raw);
    if p.is_absolute() {
        return Ok((p.display().to_string(), None));
    }
    let Some(dir) = project_dir(srv) else {
        return Err(format!(
            "out `{raw}` is a relative path and no project is open, so there is no folder to resolve it against: give an absolute path (such as ~/Videos/x.mp4), or open a project first"
        ));
    };
    let note = format!(
        "relative out `{raw}` resolved against the project folder {}",
        dir.display()
    );
    Ok((dir.join(&p).display().to_string(), Some(note)))
}

/// The `.part` an export to `out` writes through (`out` with `.part` appended), when a
/// killed process left one behind: it is deleted, and the note says so. `None` when there is none.
fn stale_part_note(out: &Path) -> Option<String> {
    let mut name = out.as_os_str().to_owned();
    name.push(".part");
    let part = PathBuf::from(name);
    if !part.is_file() {
        return None;
    }
    Some(match std::fs::remove_file(&part) {
        Ok(()) => format!(
            "an earlier export to {} did not finish: {} left behind — deleted",
            out.display(),
            part.display()
        ),
        Err(e) => format!(
            "an earlier export to {} did not finish: {} left behind — could not delete it: {e}",
            out.display(),
            part.display()
        ),
    })
}

/// A warning when `out` lands inside a git working tree and not beside the project file:
/// an export there shows up in `git status`. Does not refuse.
fn repo_warning(out: &Path, project: Option<&Path>) -> Option<String> {
    let parent = out.parent()?;
    if project == Some(parent) {
        return None;
    }
    let repo = parent.ancestors().find(|a| a.join(".git").exists())?;
    Some(format!(
        "warning: {} is inside the git working tree at {}, so the export will show up in git status; write exports outside the repo, or next to the project file",
        out.display(),
        repo.display()
    ))
}

// ---- burned captions ------------------------------------------------------

/// The schema of `burn_captions`, shared with `frame` and `contact_sheet`.
pub(crate) fn burn_schema() -> Value {
    json!({
        "type": ["object", "boolean"],
        "description": format!("{BURN_DOC} Pass true for the defaults, or false for no burn."),
        "properties": {
            "track": {
                "type": "integer",
                "minimum": 0,
                "description": "Which subtitle track to burn, as in `subtitles` (its index). Default: the track placed on the first subtitle lane that has clips, else track 0."
            },
            "size": {
                "type": "integer",
                "minimum": 10,
                "maximum": 200,
                "description": "Text height as permille of the frame height: 50 (the default) is about 5% of the picture."
            },
            "outline": {
                "type": "string",
                "enum": ["none", "thin", "thick"],
                "description": "Dark outline around each letter. Default thin."
            },
            "boxed": {
                "type": "boolean",
                "description": "Draw a translucent black box behind the caption. Default true."
            },
            "place": {
                "type": "string",
                "enum": ["bottom", "top", "centre"],
                "description": "Where the caption sits in the picture. Default bottom."
            },
            "family": {
                "type": "string",
                "description": "The font family the caption is set in, by the name the GUI's font list shows (for example \"Noto Serif\"). Default: the bundled Archivo. A name the system does not have draws in the bundled face."
            },
            "animation": {
                "type": "string",
                "enum": ["none", "karaoke", "pop", "bounce", "word"],
                "description": "Caption animation against the words' times. none: the static cue (default). karaoke: the whole chunk shown, the word being spoken in the highlight colour. pop: words appear as they are spoken, each popping from 125% to 100% in 120 ms. bounce: the word being spoken is raised and enlarged, easing down. word: one big word at a time. Default: the project's caption style (project_settings captions), else none."
            },
            "highlight": {
                "type": "string",
                "description": "Highlight colour as #rrggbb (or #rgb). Default #ffd600 yellow."
            },
            "box": {
                "type": "boolean",
                "description": "The highlight is a rounded box of the colour behind white text, not coloured text. Default false. (`boxed` is the black box behind the whole caption.)"
            },
            "uppercase": {
                "type": "boolean",
                "description": "Draw the text in capitals. Default false."
            },
            "words_per_line": {
                "type": "integer",
                "minimum": 0,
                "maximum": 12,
                "description": "At most this many words on screen at once; a longer cue is re-chunked by its word times (2 to 4 is the shorts style). 0 keeps each cue whole (default). The word animation is always one."
            }
        },
        "additionalProperties": false
    })
}

/// The note a burn of `track` owes when a cue has a character no bundled face draws:
/// `no glyph for: U+XXXX`, each named once. `None` when every character draws.
fn missing_glyph_note(s: &PlaybackSession, track: usize) -> Option<String> {
    let (project, meta) = s.export_snapshot();
    let cues = engine::export::burn_cues(&project, track, meta.frame_rate).ok()?;
    let lines: Vec<String> = cues.iter().filter(|c| c.image.is_none()).map(|c| c.text.clone()).collect();
    let missing = engine::text::missing_glyphs(&lines);
    if missing.is_empty() {
        return None;
    }
    let codes: Vec<String> = missing.iter().map(|c| format!("U+{:04X}", *c as u32)).collect();
    Some(format!("no glyph for: {} (drawn as an empty box)", codes.join(" ")))
}

/// The `burn_captions` argument checked against the open session: `None` when it is
/// absent, else the subtitle track to draw (an index into `Project::subtitles`) and its
/// style. Refuses a track that does not exist, a bitmap track, and a track with no text.
/// Shared by `export_start`, `frame` and `contact_sheet`.
pub(crate) fn parse_burn(args: &Value, s: &PlaybackSession) -> Result<Option<(usize, TextStyle)>, String> {
    // `true` is the defaults, `{}`; `false` is off, as an absent key is.
    let defaults = json!({});
    let raw = match args.get(BURN_KEY) {
        None | Some(Value::Null | Value::Bool(false)) => return Ok(None),
        Some(Value::Bool(true)) => &defaults,
        Some(v) => v,
    };
    let obj = raw
        .as_object()
        .ok_or("burn_captions must be an object or a boolean, e.g. {\"track\": 0, \"size\": 50} or true")?;
    if let Some(key) = obj.keys().find(|k| !BURN_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "burn_captions: unknown key `{key}`: the keys are track, size, outline, boxed, place, family, animation, highlight, box, uppercase, words_per_line"
        ));
    }
    let subs = s.subtitles();
    let track = match obj.get("track").filter(|v| !v.is_null()) {
        Some(v) => {
            let i = v
                .as_u64()
                .ok_or("burn_captions.track must be a whole-number track index, e.g. 0")?
                as usize;
            if i >= subs.len() {
                return Err(format!(
                    "burn_captions: track {i} does not exist: this project has {} subtitle track(s)",
                    subs.len()
                ));
            }
            i
        }
        None if subs.is_empty() => {
            return Err(
                "burn_captions: this project has no subtitle track to burn: import one first (subtitles_import)"
                    .to_string(),
            )
        }
        None => placed_track(s),
    };
    let t = &subs[track];
    if t.is_bitmap() {
        return Err(format!(
            "burn_captions: track {track} is pictures (PGS), not text, and only text cues can be burned: pick a text track"
        ));
    }
    if let Some(why) = &t.refused {
        return Err(format!("burn_captions: track {track} has no text to burn: {why}"));
    }
    if t.cues.is_empty() {
        return Err(format!("burn_captions: track {track} has no cues to burn"));
    }
    let (project, meta) = s.export_snapshot();
    let drawn = engine::export::burn_cues(&project, track, meta.frame_rate)
        .map_err(|e| format!("burn_captions: {e}"))?;
    if drawn.is_empty() {
        return Err(format!(
            "burn_captions: track {track} has no cue on the timeline to draw: its cues are cut away, past the picture, or on a hidden lane"
        ));
    }

    let mut style = TextStyle {
        outline: Outline::Thin,
        caption: caption_from(obj, s.caption_style())?,
        ..TextStyle::default()
    };
    if let Some(v) = obj.get("size").filter(|v| !v.is_null()) {
        let n = v
            .as_u64()
            .filter(|n| (10..=200).contains(n))
            .ok_or("burn_captions.size must be a whole number from 10 to 200 (permille of the frame height; 50 is about 5%)")?;
        style.size = n as u32;
    }
    if let Some(v) = obj.get("outline").filter(|v| !v.is_null()) {
        style.outline = match v.as_str() {
            Some("none") => Outline::None,
            Some("thin") => Outline::Thin,
            Some("thick") => Outline::Thick,
            _ => return Err("burn_captions.outline must be none, thin or thick".to_string()),
        };
    }
    if let Some(v) = obj.get("boxed").filter(|v| !v.is_null()) {
        style.boxed = v.as_bool().ok_or("burn_captions.boxed must be true or false")?;
    }
    if let Some(v) = obj.get("place").filter(|v| !v.is_null()) {
        style.place = match v.as_str() {
            Some("bottom") => Place::Bottom,
            Some("top") => Place::Top,
            Some("centre") => Place::Centre,
            _ => return Err("burn_captions.place must be bottom, top or centre".to_string()),
        };
    }
    if let Some(v) = obj.get("family").filter(|v| !v.is_null()) {
        let name = v.as_str().ok_or("burn_captions.family must be a font family name, e.g. \"Noto Serif\"")?;
        style.family = Some(name.to_string());
    }
    Ok(Some((track, style)))
}

/// `base` with the caption keys of `obj` (`animation`, `highlight`, `box`, `uppercase`,
/// `words_per_line`) laid over it. Shared by `burn_captions` and `project_settings captions`.
pub(crate) fn caption_from(obj: &Map<String, Value>, base: CaptionStyle) -> Result<CaptionStyle, String> {
    let mut cs = base;
    let set = |key: &str| obj.get(key).filter(|v| !v.is_null());
    if let Some(v) = set("animation") {
        cs.anim = v
            .as_str()
            .and_then(CaptionAnim::from_name)
            .ok_or("animation must be none, karaoke, pop, bounce or word")?;
    }
    if let Some(v) = set("highlight") {
        let hex = v.as_str().map(|t| t.trim_start_matches('#')).unwrap_or("");
        let digits: Option<Vec<u8>> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect();
        cs.highlight = match digits.as_deref() {
            Some([r, g, b]) => [r * 17, g * 17, b * 17],
            Some([r1, r2, g1, g2, b1, b2]) => [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2],
            _ => return Err("highlight must be a colour like #ffd600 or #fd0".to_string()),
        };
    }
    if let Some(v) = set("box") {
        cs.hl_box = v.as_bool().ok_or("box must be true or false")?;
    }
    if let Some(v) = set("uppercase") {
        cs.uppercase = v.as_bool().ok_or("uppercase must be true or false")?;
    }
    if let Some(v) = set("words_per_line") {
        cs.words_per_line = v
            .as_u64()
            .filter(|&n| n <= u64::from(MAX_WORDS_PER_LINE))
            .ok_or_else(|| format!("words_per_line must be a whole number from 0 to {MAX_WORDS_PER_LINE}"))? as u8;
    }
    Ok(cs)
}

/// The track on the first subtitle lane that has clips, or track 0 when no lane does.
/// A clip naming a track that is gone is skipped rather than trusted.
fn placed_track(s: &PlaybackSession) -> usize {
    let n = s.subtitles().len();
    s.subtitle_lanes()
        .into_iter()
        .find_map(|l| s.sub_lane(l).first().map(|c| c.track))
        .filter(|&t| t < n)
        .unwrap_or(0)
}

// ---- argument helpers -----------------------------------------------------

/// `range: {from, to}` as a half-open span of timeline frames, or `None`.
pub(crate) fn parse_range(args: &Value, fps: f64, end: u32, media: u32) -> Result<Option<(u32, u32)>, String> {
    let Some(r) = args.get("range") else { return Ok(None) };
    if r.is_null() {
        return Ok(None);
    }
    let from = r
        .get("from")
        .ok_or("`range` needs both `from` and `to`, e.g. {\"from\": \"0s\", \"to\": \"15s\"}")?;
    let to = r
        .get("to")
        .ok_or("`range` needs both `from` and `to`, e.g. {\"from\": \"0s\", \"to\": \"15s\"}")?;
    let from = frame(from, fps, end).map_err(|e| format!("range.from: {e}"))?;
    let to = frame(to, fps, end).map_err(|e| format!("range.to: {e}"))?;
    if from >= to {
        return Err(format!("range.from ({}) must come before range.to ({})", tc(from, fps), tc(to, fps)));
    }
    // A `to` past the picture and sound but inside the timeline is clipped to them by the
    // caller, and said; past the timeline itself it is refused, naming both ends.
    if to > end {
        return Err(format!(
            "range.to ({}) is past the end of the timeline ({}): the picture and sound end at {}",
            tc(to, fps),
            tc(end, fps),
            tc(media, fps)
        ));
    }
    Ok(Some((from, to)))
}

fn parse_subtitles(args: &Value, count: usize) -> Result<Vec<usize>, String> {
    let arr = match args.get("subtitles") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => return Ok(Vec::new()),
        Some(Value::Bool(true)) => return Ok((0..count).collect()),
        Some(Value::Array(arr)) => arr,
        Some(_) => return Err("subtitles must be true (every track) or a list of track indices, e.g. [0, 2]".to_string()),
    };
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        let i = x
            .as_u64()
            .ok_or("subtitles must be whole-number track indices, e.g. [0, 2]")? as usize;
        if i >= count {
            return Err(format!(
                "subtitles: track {i} does not exist: this project has {count} subtitle track(s)"
            ));
        }
        out.push(i);
    }
    Ok(out)
}

/// Why a hardware AV1 encode is refused: the VA-API AV1 encoder crashed the GPU on this
/// machine. WebM is seated as AV1 by the engine, so it is refused the same way.
const HW_AV1_REFUSAL: &str = "hardware AV1 encoding is disabled here (it has crashed the GPU on this machine); exporting with software AV1 (rav1e) — or pick h264/hevc for hardware";

/// True when this export would put AV1 (or WebM) on the GPU. `auto` never does: the engine
/// seats AV1 in software unless the seat is asked for by name. Only `EDITH_ALLOW_HW_AV1=1`
/// lifts the refusal.
fn hw_av1_refused(format: Format, seat: EncoderSeat) -> bool {
    seat == EncoderSeat::Hardware
        && matches!(format, Format::Av1 | Format::Av1Mp4 | Format::Webm)
        && std::env::var("EDITH_ALLOW_HW_AV1").map_or(true, |v| v != "1")
}

/// The note a request for soft subtitles owes when the file will have no picture: the sound
/// alone has no track to carry them, and the card would otherwise say only `none`, which
/// reads as if the export had been asked nothing. Said once, in the reply, before the job runs.
fn sound_only_subs_note(format: Format) -> String {
    if format.ext() == "mp4" {
        "subtitles not written: an mp4 with no picture is written as sound only; add a picture (a still image or a video clip) to carry them".to_string()
    } else {
        format!("subtitles not written: {} is the sound alone; add a picture to carry them", format_name(format))
    }
}

/// The picture encoder a finished or running job used, from the engine's encoder line
/// (`seat · sound`); the planned seat when the engine has not published one yet.
/// The sound a job measured, as a reply's ` · audio: …` part; empty until the engine says.
fn sound_note(job: &Job) -> String {
    sound_words(job.picture.is_empty(), job.encoders().as_deref())
        .map_or_else(String::new, |s| format!(" · audio: {s}"))
}

/// The pure half of [`sound_note`]. An audio-only job's engine line is the sound alone
/// (`AAC · SW encode (ec-aac)`, `AAC copy`, `PCM · SW (hound)`), with no picture part in front,
/// so its codec is the first part; a job with a picture skips that part.
fn sound_words(audio_only: bool, encoders: Option<&str>) -> Option<String> {
    if audio_only {
        encoders.map(plain_audio)
    } else {
        measured_sound(encoders)
    }
}

fn picture_note(job: &Job) -> String {
    picture_label(job.encoders(), &job.picture, job.format)
}

/// The pure half of [`picture_note`]: the seat named by the engine's line when it has one,
/// else the planned seat, in plain words. Empty when the export has no picture (`planned` is empty).
fn picture_label(encoders: Option<String>, planned: &str, format: Format) -> String {
    if planned.is_empty() {
        return String::new();
    }
    let seat = encoders
        .as_deref()
        .and_then(|e| e.split(" · ").next())
        .unwrap_or(planned);
    format!(" · video: {}", plain_video(format, seat))
}

/// The sound a finished export says it wrote, in plain words, when the engine has measured it:
/// the part of the engine's line after the picture (`AAC · SW encode (ec-aac)` says `AAC`).
fn measured_sound(encoders: Option<&str>) -> Option<String> {
    let rest = encoders?.split(" · ").skip(1).collect::<Vec<_>>().join(" · ");
    (!rest.is_empty()).then(|| plain_audio(&rest))
}

/// The picture's codec and seat in plain words, from the format and the engine's seat label:
/// `AV1 (software)`, `H.264 (hardware)`, `H.264 (copy)`.
fn plain_video(format: Format, seat: &str) -> String {
    let codec = match format {
        Format::Mp4 => "H.264",
        Format::Av1 | Format::Av1Mp4 | Format::Webm => "AV1",
        Format::Hevc | Format::HevcMp4 => "HEVC",
        _ => "video",
    };
    let how = if seat.starts_with("HW") {
        "hardware"
    } else if seat.starts_with("copy") {
        "copy"
    } else {
        "software"
    };
    format!("{codec} ({how})")
}

/// The sound's codec in plain words: `AAC`, `Opus`, `AAC (copy)`, `none`.
fn plain_audio(label: &str) -> String {
    if label.starts_with("no sound") {
        return "none".to_string();
    }
    let name = label.split(" · ").next().unwrap_or(label).trim();
    match name.strip_suffix(" copy") {
        Some(codec) => format!("{codec} (copy)"),
        None => name.to_string(),
    }
}

/// The container in plain words: `MP4`, `WebM`, `Matroska`, `WAV`.
fn plain_container(format: Format) -> &'static str {
    match format {
        Format::Mp4 | Format::Av1Mp4 | Format::HevcMp4 => "MP4",
        Format::Av1 | Format::Hevc => "Matroska",
        Format::Webm => "WebM",
        Format::Wav => "WAV",
        Format::Flac => "FLAC",
        Format::Mp3 => "MP3",
        Format::Ogg => "Ogg",
        Format::Opus => "Ogg Opus",
        Format::M4a => "M4A",
        Format::ProRes => "QuickTime",
        Format::PngSequence => "PNG folder",
        Format::Still => "PNG",
        Format::Gif => "GIF",
    }
}

/// What an export writes, in the words a start reply uses:
/// `video: AV1 (software), audio: Opus, container: WebM`, or `audio: PCM, container: WAV`.
/// `picture` is the planned seat, empty for an audio-only export.
fn av_words(format: Format, picture: &str, sound: &str) -> String {
    // A picture set writes pictures and no sound, so it names no audio codec at all.
    if matches!(format, Format::PngSequence | Format::Still | Format::Gif) {
        return format!("no sound, container: {}", plain_container(format));
    }
    let mut parts = Vec::new();
    if !picture.is_empty() {
        parts.push(format!("video: {}", plain_video(format, picture)));
    }
    // The sound is the codec the engine predicts from the sources' headers (a Matroska mix is
    // Opus at 48 kHz mono or stereo, AAC otherwise); the status line names the one measured.
    parts.push(format!("audio: {}", plain_audio(sound)));
    parts.push(format!("container: {}", plain_container(format)));
    parts.join(", ")
}

/// The `size` of export_start: `WIDTHxHEIGHT` (`1280x720`), or a height with a `p` (`720p`),
/// which is 16:9 at that height. Both sides must be even and a picture size the engine takes,
/// so the picture has whole chroma samples. `None` when no size was asked for.
pub(crate) fn parse_size(args: &Value) -> Result<Option<(u32, u32)>, String> {
    let Some(raw) = args.get("size").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = raw
        .as_str()
        .ok_or("size must be a string: WIDTHxHEIGHT such as 1280x720, or a height such as 720p")?
        .trim()
        .to_ascii_lowercase();
    let number = |part: &str| -> Result<u64, String> {
        part.trim()
            .parse::<u64>()
            .map_err(|_| format!("size `{text}`: write WIDTHxHEIGHT such as 1280x720, or a height such as 720p"))
    };
    let (w, h) = match text.strip_suffix('p') {
        Some(height) => {
            let h = number(height)?;
            // 16:9 at that height, the width rounded to the nearest even number.
            let w = (h * 16 / 9 + 1) & !1;
            (w, h)
        }
        None => match text.split_once('x') {
            Some((w, h)) => (number(w)?, number(h)?),
            None => return Err(format!("size `{text}`: write WIDTHxHEIGHT such as 1280x720, or a height such as 720p")),
        },
    };
    let max = u64::from(engine::MAX_RESOLUTION);
    if w == 0 || h == 0 || w > max || h > max {
        return Err(format!("size `{text}`: each side must be from 2 to {max} pixels"));
    }
    if w % 2 != 0 || h % 2 != 0 {
        return Err(format!("size `{text}`: both sides must be even (the picture is 4:2:0 coded)"));
    }
    Ok(Some((w as u32, h as u32)))
}

/// The `fit` of export_start: `None` for `keep` (or no fit), which leaves every clip on its own
/// policy; `fit` or `fill` puts every picture clip on that policy in this export only.
pub(crate) fn parse_fit(args: &Value) -> Result<Option<FitPolicy>, String> {
    match opt_str(args, "fit").map(|s| s.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(s) => match s.as_str() {
            "keep" => Ok(None),
            "fit" => Ok(Some(FitPolicy::Fit)),
            "fill" => Ok(Some(FitPolicy::Fill)),
            other => Err(format!("unknown fit `{other}`: write keep, fit or fill")),
        },
    }
}

fn parse_seat(args: &Value) -> Result<EncoderSeat, String> {
    match opt_str(args, "seat").map(|s| s.trim().to_ascii_lowercase()) {
        None => Ok(EncoderSeat::Auto),
        Some(s) => match s.as_str() {
            "auto" => Ok(EncoderSeat::Auto),
            "hardware" => Ok(EncoderSeat::Hardware),
            "software" => Ok(EncoderSeat::Software),
            other => Err(format!("unknown seat `{other}`: write auto, hardware or software")),
        },
    }
}

/// Index of `job:<n>` (1-based in text) into `srv.jobs`.
fn job_index(srv: &Server, n: u64) -> Result<usize, String> {
    let len = srv.jobs.len();
    if n >= 1 && n <= len as u64 {
        Ok(n as usize - 1)
    } else if len == 0 {
        Err(format!("no job:{n} — no export has been started yet (export_start makes one)"))
    } else {
        Err(format!("no job:{n} — the exports so far are job:1 to job:{len}"))
    }
}

/// How long a finished job took: one decimal under ten seconds (`0.7 s`), else the clock form.
fn took_words(s: f64) -> String {
    if s < 10.0 {
        format!("{:.1} s", s.max(0.0))
    } else {
        clock_secs(s)
    }
}

fn clock_secs(s: f64) -> String {
    let t = s.round().max(0.0) as u64;
    if t >= 3600 {
        format!("{}h{:02}m{:02}s", t / 3600, t % 3600 / 60, t % 60)
    } else if t >= 60 {
        format!("{}m{:02}s", t / 60, t % 60)
    } else {
        format!("{t}s")
    }
}

/// The true-peak ceiling the mix is held to (dBTP), the default `true_peak` of the loudness tools.
const PEAK_CEILING_DBTP: f64 = -1.0;

/// The smallest true peak that clips for text: the lowest value one decimal prints above 0 (+0.1).
const CLIP_DBTP: f64 = 0.05;

/// A true peak above the ceiling for text: two decimals within 0.05 dB of it, since one decimal
/// would print the ceiling itself ("-1.0 dBTP is above the -1 dBTP ceiling").
fn ceiling_figure(p: f64) -> String {
    if (p - PEAK_CEILING_DBTP).abs() < 0.05 {
        format!("{p:+.2}")
    } else {
        format!("{p:+.1}")
    }
}

/// The word a caption or title's place reads as in a reply.
pub(crate) fn place_word(place: Place) -> &'static str {
    match place {
        Place::Bottom => "bottom",
        Place::Top => "top",
        Place::Centre => "centre",
    }
}

/// The note a burned caption and a title on screen together in the same place earn: when they
/// clash, and the place to move the captions to (or the title). `secs` is the moment they first
/// show together.
pub(crate) fn clash_note(secs: f64, title: &str, place: Place) -> String {
    let other = if place == Place::Bottom { "top" } else { "bottom" };
    format!(
        "at {secs:.1}s the caption overlaps title '{}' (both at the {}): burn_captions {{place:\"{other}\"}} or move the title",
        title.replace('\n', " / "),
        place_word(place)
    )
}

/// The integrated loudness and true peak of an exported file's first sound track, decoded from
/// the file with the same measurement the `loudness` tool uses. A silent stretch measures -inf
/// on both (no block above the gate), which is kept as such. `None` when the file has no sound
/// or cannot be measured.
fn file_levels(path: &std::path::Path) -> Option<FileLevels> {
    engine::loudness::measure_file(path, 0, None)
        .ok()
        .flatten()
        .map(|l| FileLevels { lufs: l.integrated_lufs, true_peak_dbtp: l.true_peak_dbtp })
}

/// The shortest sound integrated loudness is gated over: one 400 ms block.
const LOUDNESS_MIN_SECS: f64 = 0.4;

/// `-14.0 LUFS`. A file with no block above the gate says why: `too short to measure loudness`
/// when its sound is under one 400 ms block, else `silent`.
fn lufs_text(lufs: f64, too_short: bool) -> String {
    if lufs.is_finite() {
        format!("{lufs:.1} LUFS")
    } else if too_short {
        format!("too short to measure loudness (under {LOUDNESS_MIN_SECS} s)")
    } else {
        "silent".to_string()
    }
}

/// Whether the first sound track of the written file is shorter than one loudness block.
fn sound_too_short(path: &std::path::Path) -> bool {
    engine::AudioSession::stream_duration_secs(path, 0)
        .ok()
        .flatten()
        .is_some_and(|secs| secs < LOUDNESS_MIN_SECS)
}

/// `-1.0 dBTP` or `+0.3 dBTP` (signed, as hear reads its figures), or `silent` for a file with no sound to peak.
fn peak_text(dbtp: f64) -> String {
    if dbtp.is_finite() { format!("{dbtp:+.1} dBTP") } else { "silent".to_string() }
}

/// A size in decimal units (KB = 1000 bytes, MB = 10^6 bytes), the unit `media_probe` uses,
/// so one file reads the same in every reply.
fn human_bytes(n: u64) -> String {
    const MB: f64 = 1_000_000.0;
    if n < 1000 {
        format!("{n} B")
    } else if (n as f64) < MB {
        format!("{:.1} KB", n as f64 / 1000.0)
    } else {
        format!("{:.1} MB", n as f64 / MB)
    }
}

// ---- tools ----------------------------------------------------------------

fn export_start(srv: &mut Server, args: &Value) -> Reply {
    let out_raw = tri!(arg_str(args, "out"));
    let overwrite = opt_bool(args, "overwrite").unwrap_or(false);
    // An offline source has no picture or sound to put in the file: refused by name, before
    // anything is written.
    if let Ok(s) = srv.session_ref() {
        let offline = s.offline_sources();
        if !offline.is_empty() {
            let names: Vec<String> = offline.iter().map(|(i, p)| format!("src:{i} {}", p.display())).collect();
            return Reply::err(format!(
                "export refused: {} source(s) offline (moved or deleted): {}; media_relink each one, or project_open with search",
                offline.len(),
                names.join(", ")
            ));
        }
    }
    let (anchored, anchor_note) = match anchor_out(srv, out_raw) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let preset = match opt_str(args, "preset") {
        None => None,
        Some(id) => match engine::export_presets::by_id(id) {
            Some(p) => Some(p),
            None => {
                let ids: Vec<&str> = engine::export_presets::presets().iter().map(|p| p.id).collect();
                return Reply::err(format!("unknown preset `{id}`: the presets are {} (export_presets lists them)", ids.join(", ")));
            }
        },
    };
    let (out, format) = match resolve_out(&anchored, opt_str(args, "format").or(preset.map(|p| format_name(p.format)))) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let checked = match format {
        Format::PngSequence => check_folder(&out, overwrite),
        _ => check_out(&out, overwrite),
    };
    if let Err(e) = checked {
        return Reply::err(e);
    }
    let warning = repo_warning(&out, project_dir(srv).as_deref());
    // Two jobs on one file would write the same `.part` and race for the name, so a
    // running job on `out` refuses this one even when `overwrite` is set.
    if let Some((n, _)) = srv
        .jobs
        .iter()
        .enumerate()
        .find(|(_, j)| job_running(j) && j.out == out)
    {
        return Reply::err(format!(
            "job:{} is still writing {}: wait for it to finish or cancel it, or choose another out path",
            n + 1,
            out.display()
        ));
    }

    let s = tri!(srv.session_ref());
    // A stabilised clip with no finished analysis is measured by the export itself,
    // which export_status shows as its "Measuring shake" stage.
    let fps = session_fps(s);
    let end = session_end(s);
    if end == 0 {
        return Reply::err("timeline is empty — nothing to export: place a clip first (edit place/append)");
    }
    if let Some(why) = channel_mismatch(s, fps) {
        return Reply::err(why);
    }
    // The engine refuses a picture format with no picture, but only once the job runs;
    // refuse it here, in the same words, so the agent learns it at the start.
    let has_picture = s
        .lanes()
        .into_iter()
        .any(|l| l.kind == LaneKind::Video && !s.lane_clips(l).is_empty());
    // An mp4 of a picture-less timeline is written as the sound alone (AAC in an mp4), as
    // the engine does; every other picture format would be black.
    let sound_only = !format.has_video() || (!has_picture && format.ext() == "mp4");
    // Named first in the reply: an mp4 of a picture-less timeline is the sound alone, so any caption
    // track is not written, and the words say what to use instead.
    let lead = (!has_picture && format.ext() == "mp4" && !s.subtitles().is_empty()).then(|| {
        "sound only: an mp4 with no picture is written as the sound alone, so the captions are not written. For sound alone use m4a; for a video with captions add a picture to the timeline (a still image is enough)".to_string()
    });
    if format.has_video() && !has_picture && format.ext() != "mp4" {
        return Reply::err(format!(
            "the timeline has no video clips, so {} would be black: export mp4 (the sound alone), m4a, wav, flac or mp3 instead",
            format_name(format)
        ));
    }
    // A whole-timeline export stops where the picture and sound stop (the engine's
    // `media_frames`): a caption hanging past that is not written, and neither is a range's
    // part past it. The reply reports the length the file really is, not the timeline's
    // (which counts the captions), and says why.
    let media = media_end(s);
    let (range, clipped) = match parse_range(args, fps, end, media) {
        Ok(Some((from, _))) if from >= media => {
            return Reply::err(format!(
                "range.from ({}) is at or past the picture and sound end ({}): captions alone are not written, so there is nothing to export there",
                tc(from, fps),
                tc(media, fps)
            ))
        }
        Ok(Some((from, to))) => (Some((from, to.min(media))), to > media),
        Ok(None) => (None, false),
        Err(e) => return Reply::err(e),
    };
    let ranged = range.is_some();
    let captions_note = (range.is_none() && media < end).then(|| {
        format!(
            "captions run to {}s, past the picture and sound end at {}s: they are not exported (a range stops there too)",
            secs(end, fps),
            secs(media, fps)
        )
    });
    let range_note = clipped.then(|| {
        format!(
            "the range runs past the picture and sound end at {}s: the file stops there, and captions past it are not exported",
            secs(media, fps)
        )
    });
    if range.is_none() && media == 0 {
        return Reply::err("the picture and sound end at 0s, and only captions run past them: nothing to export");
    }

    let subtitles = match parse_subtitles(args, s.subtitles().len()) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let seat = match parse_seat(args) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    if hw_av1_refused(format, seat) {
        return Reply::err(HW_AV1_REFUSAL);
    }
    let burn = match parse_burn(args, s) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    if burn.is_some() && sound_only {
        return Reply::err(format!(
            "burn_captions draws into the picture, and {} is audio only: drop burn_captions, or export a video format",
            format_name(format)
        ));
    }
    let size = match parse_size(args) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let fit = match parse_fit(args) {
        Ok(v) => v,
        Err(e) => return Reply::err(e),
    };
    let burned = burn.as_ref().map(|(track, _)| *track);
    let glyph_note = burned.and_then(|t| missing_glyph_note(s, t));
    let bitrate = match opt_u64(args, "bitrate_kbps") {
        Some(0) => return Reply::err("bitrate_kbps must be above 0"),
        Some(k) => Some(k.saturating_mul(1000)),
        None => None,
    };
    let audio_kbps = match opt_u64(args, "audio_kbps") {
        None => None,
        Some(k) => {
            let Some(k32) = u32::try_from(k).ok().filter(|v| AUDIO_KBPS.contains(v)) else {
                return Reply::err(format!(
                    "audio_kbps {k} is not one of {}",
                    AUDIO_KBPS.map(|v| v.to_string()).join(", ")
                ));
            };
            if let Some(why) = s.audio_rate_refusal(format, ranged) {
                return Reply::err(format!(
                    "audio_kbps can't be chosen for {}: {why}",
                    format_name(format)
                ));
            }
            Some(k32)
        }
    };

    let video_bit_depth = match opt_u64(args, "bit_depth") {
        None => None,
        Some(8) => Some(8u8),
        Some(10) => Some(10u8),
        Some(b) => return Reply::err(format!("bit_depth {b} is not 8 or 10")),
    };
    // The preset fills what the arguments left out; an explicit argument wins.
    let preset_settings = preset.map(|p| engine::export_presets::apply(p, ExportSettings::default()));
    let bitrate = bitrate.or(preset_settings.as_ref().and_then(|a| a.bitrate));
    let audio_kbps = audio_kbps.or(preset_settings.as_ref().and_then(|a| a.audio_kbps));
    let size = size.or(preset_settings.as_ref().and_then(|a| a.size));
    let video_bit_depth = video_bit_depth.or(preset_settings.as_ref().and_then(|a| a.video_bit_depth));
    let hevc_speed = tri!(parse_hevc_speed(args, format).map_err(Reply::err));
    let prores_profile = tri!(parse_prores_profile(args, format).map_err(Reply::err))
        .or(preset_settings.as_ref().filter(|_| format.is_prores()).map(|a| a.prores_profile))
        .unwrap_or_default();
    let hdr_transfer = tri!(parse_hdr(args, format).map_err(Reply::err))
        .or(preset_settings.as_ref().filter(|_| format.is_hevc()).map(|a| a.hdr.transfer))
        .unwrap_or_default();
    let audio_bit_depth = tri!(parse_sound_bits(args, format).map_err(Reply::err));
    if hdr_transfer != HdrTransfer::Sdr && video_bit_depth == Some(8) {
        return Reply::err("an HDR export is 10-bit: bit_depth 8 is refused (PQ and HLG are not carried in 8 bits)".to_string());
    }
    // A loudness argument sets the target from -30 to -5 LUFS; its true-peak ceiling stays the preset's.
    let loudness = match args.get("loudness") {
        None | Some(Value::Null) => preset_settings.as_ref().and_then(|a| a.loudness),
        Some(v) => match v.as_f64() {
            Some(lufs) if (-30.0..=-5.0).contains(&lufs) => Some(engine::export::LoudnessTarget {
                lufs,
                true_peak_dbtp: preset_settings
                    .as_ref()
                    .and_then(|a| a.loudness)
                    .map_or(PEAK_CEILING_DBTP, |l| l.true_peak_dbtp),
            }),
            _ => return Reply::err("loudness is a number of LUFS from -30 to -5 (-14 is the usual streaming level)".to_string()),
        },
    };
    let loudness_note = match loudness {
        Some(l) if args.get("loudness").is_some_and(|v| !v.is_null()) => format!("; loudness {} LUFS", l.lufs),
        _ => String::new(),
    };
    if video_bit_depth == Some(10) && !(format.is_av1() || format.is_hevc()) {
        return Reply::err(format!(
            "10-bit export is AV1 and HEVC only: format av1 (.mkv), av1-mp4, hevc (.mkv) or hevc-mp4, not {}",
            format_name(format)
        ));
    }
    // Chapters: the project's markers, measured from the range's start (the engine's own
    // clock, [`PlaybackSession::chapter_list_ns`]). Formats that cannot carry them get none.
    // `chapters: "markers"` writes the comment markers as the chapter list in place of the chapters.
    let from_markers = match args.get("chapters") {
        Some(Value::String(t)) if t.eq_ignore_ascii_case("markers") => true,
        Some(Value::String(t)) => {
            return Reply::err(format!("argument `chapters`: `{t}` is not true, false or \"markers\""));
        }
        _ => false,
    };
    let chapters_asked = from_markers || opt_bool(args, "chapters").unwrap_or(true);
    let chapter_format = matches!(
        format,
        Format::Mp4 | Format::Av1 | Format::Av1Mp4 | Format::Hevc | Format::HevcMp4 | Format::Webm | Format::M4a | Format::Opus
    );
    let chapters = match (chapters_asked && chapter_format, from_markers) {
        (false, _) => Vec::new(),
        (true, true) => s.marker_list_ns(range),
        (true, false) => s.chapter_list_ns(range),
    };
    let chapter_note = if (from_markers && s.markers().is_empty()) || (!from_markers && s.chapters().is_empty()) {
        None
    } else if !chapters_asked {
        Some("chapters: left out (chapters: false)".to_string())
    } else if !chapter_format {
        Some(format!(
            "chapters are not written: {} carries no chapter marks (mp4, m4a, mkv, webm and opus do)",
            format_name(format)
        ))
    } else {
        let from = if from_markers { " (from the markers)" } else { "" };
        Some(match chapters.len() {
            1 => format!("1 chapter{from}"),
            n => format!("{n} chapters{from}"),
        })
    };

    // Soft subtitle tracks are opt-in: without `subtitles` the file carries none, even
    // when the timeline has captions placed on it (the engine writes those whenever it
    // is not told otherwise).
    let no_soft = subtitles.is_empty();
    let asked_subs = !subtitles.is_empty() || args.get("subtitles") == Some(&json!(true));
    let sub_note = if asked_subs && s.subtitles().is_empty() {
        "note: no subtitle tracks, so no soft track was written".to_string()
    } else if asked_subs && sound_only {
        sound_only_subs_note(format)
    } else if !subtitles.is_empty() {
        // The card is asked of the file as it will be written: the range drops a track with
        // no cue inside it, and a standalone ASS script's override tags are named as dropped.
        let (snap, snap_meta) = s.export_snapshot();
        let mut note = engine::export::planned_subtitles_in(
            &snap,
            format,
            subtitles.iter().copied(),
            range,
            snap_meta.frame_rate,
        );
        let styled = engine::export::stripped_styling(&snap, &subtitles);
        if !styled.is_empty() {
            note.push_str(&format!(
                "\nstyling not written: the ASS tags ({{\\i1}}, {{\\an8}}) in {} are dropped; the soft track carries the words only",
                styled.join(", ")
            ));
        }
        note
    } else if burn.is_some() {
        "subtitles: burned into the picture (no soft track)".to_string()
    } else if s.subtitles().is_empty() {
        String::new()
    } else if sound_only {
        format!("subtitles do not apply: {} is the sound alone", format_name(format))
    } else {
        "subtitles: none (no soft track; pass subtitles to add one)".to_string()
    };
    // The depth an AV1 or HEVC file is written at, said in the reply: a named depth as asked, else the
    // source's own (10 where any picture source is 10-bit, as the engine decides it).
    let depth = (format.is_av1() || format.is_hevc()).then(|| match video_bit_depth {
        _ if hdr_transfer != HdrTransfer::Sdr => (true, format!("output 10-bit, {}", hdr_name(hdr_transfer))),
        Some(b) => (b == 10, format!("output {b}-bit, as asked")),
        None if source_ten_bit(s) => (true, "output 10-bit, from the source".to_string()),
        None => (false, "output 8-bit".to_string()),
    });
    let depth_note = depth.as_ref().map(|(_, note)| note.clone());
    let chapters_written = chapters.len();
    let settings = ExportSettings {
        bitrate,
        seat,
        format,
        intra_only: opt_bool(args, "intra_only").unwrap_or(false),
        audio_kbps,
        subtitles,
        no_soft_subtitles: no_soft,
        range,
        burn_subtitles: burn,
        size,
        video_bit_depth,
        chapters,
        loudness,
        prores_profile,
        hdr: HdrOutput { transfer: hdr_transfer, ..HdrOutput::default() },
        audio_bit_depth,
        hevc_speed,
        gif: GifSettings {
            width: opt_u64(args, "gif_width").map_or(480, |w| w as u32),
            fps: opt_u64(args, "gif_fps").map_or(15, |f| f as u32),
            loop_forever: opt_bool(args, "gif_loop").unwrap_or(true),
        },
        ..Default::default()
    };
    // The picture's seat and the sound's codec as the engine will decide them for this
    // timeline (its own copy gates, so a Matroska source is never a sound copy). Asked
    // only for video: an audio-only file has no picture to name.
    let (picture, sound, tone, project_size, hevc_mode, av1_mode) = {
        let (project, meta) = s.export_snapshot();
        let (video, sound) = planned_seats(&project, &meta, &settings);
        let picture = if sound_only { String::new() } else { video.unwrap_or("none").to_string() };
        let tone = planned_tone(&project, &meta, &settings);
        let hevc_mode = engine::export::hevc_mode_words(&meta, &settings);
        let av1_mode = engine::export::av1_mode_words(&meta, &settings);
        (picture, sound, tone, (meta.width, meta.height), hevc_mode, av1_mode)
    };
    // A `.part` no running job of this server owns was left by a process that was killed
    // mid-export. This export is about to write the same name, so it is cleared here, and said.
    let stale = stale_part_note(&out);
    // The export's own copy of the timeline: a `fit` puts every picture clip on that policy in
    // this copy only, and the project keeps its clips.
    let (mut project, meta) = s.export_snapshot();
    let fitted: Option<(FitPolicy, FitOutcome)> = fit.map(|f| (f, fit_picture_clips(&mut project, f)));
    // A caption burned where a title sits in the same place, on screen together: named before
    // the file is written, since the overlap is in the picture.
    let clash = settings.burn_subtitles.as_ref().and_then(|(track, style)| {
        let cues = engine::export::burn_cues(&project, *track, meta.frame_rate).ok()?;
        let (secs, i) = caption_title_clash(&cues, project.titles(), style.place, meta.frame_rate)?;
        Some(clash_note(secs, &project.titles()[i].text, style.place))
    });
    // An mp4 writes two overlapping cues as one stretch with both lines in it, so the cue
    // times in the file are not the timeline's; an mkv keeps each cue. Named here, once.
    let overlap = (format.ext() == "mp4")
        .then(|| engine::export::overlapping_subtitles(&project, &meta, &settings))
        .filter(|names| !names.is_empty())
        .map(|names| {
            format!(
                "overlapping cues are merged in mp4 ({}): where two overlap they show as one cue with both lines; mkv keeps them",
                names.join(", ")
            )
        });
    let chroma = chroma_notes(&project, &settings, range);
    let sample_rate = s.sample_rate();
    // `queue`: behind any job still running or waiting, this one holds its own copy of the
    // timeline and starts when they are done (`advance`). With nothing ahead it starts now.
    let queued = opt_bool(args, "queue").unwrap_or(false);
    let ahead = srv.jobs.iter().position(job_running).map(|i| i + 1);
    let (handle, pending) = if queued && ahead.is_some() {
        let frames = settings.range.map_or(project.media_frames(), |(a, b)| b.min(project.media_frames()).saturating_sub(a));
        let length_secs = f64::from(frames) / meta.frame_rate;
        (None, Some(Box::new(Pending { project, meta, settings: settings.clone(), sample_rate, length_secs })))
    } else {
        (Some(engine::export::start(project, meta, &out, &settings, sample_rate)), None)
    };
    let waiting = pending.is_some();

    let when = match range {
        Some((a, b)) => format!("range {} to {}", tc(a, fps), tc(b, fps)),
        None if media < end => format!("whole timeline, to {}", tc(media, fps)),
        None => "whole timeline".to_string(),
    };
    let job = srv.jobs.len() + 1;
    srv.jobs.push(Job {
        out: out.clone(),
        picture: picture.clone(),
        format,
        handle,
        pending,
        started: Instant::now(),
        outcome: None,
        measured: None,
    });

    let captions = burned.map_or_else(String::new, |t| format!("; captions burned (track {t})"));
    let tone_note = tone.filter(|_| !sound_only).map(|t| format!(", picture: {t}")).unwrap_or_default();
    // The size named only when it differs from the project's: the reply says what the file is.
    let size_note = size.map_or_else(String::new, |(w, h)| {
        format!("; size {w}x{h} (project {}x{} kept)", project_size.0, project_size.1)
    });
    // The fit named with the lanes it went to: this export's copy only, the project's clips keep
    // theirs. Overlays keep their own fit and aspect, and the reply says so.
    let fit_note = fitted.as_ref().map_or_else(String::new, |(f, outcome)| fit_words(*f, outcome));
    let words = av_words(format, &picture, sound);
    let preset_note = preset.map_or_else(String::new, |p| format!("; preset {}", p.id));
    let verb = if waiting { "queued" } else { "started" };
    let mut text = format!("job:{job} {verb} → {} ({when}); {words}{captions}{tone_note}{size_note}{fit_note}{preset_note}{loudness_note}", out.display());
    if waiting {
        text.push_str(&format!(
            "\nWaiting: it starts when job:{} and every export queued before it have finished, from the timeline as it is now (later edits do not change it). export_status shows it as waiting; export_cancel with its job removes it.",
            ahead.unwrap_or(1)
        ));
    }
    // An HEVC or AV1 export says in one line which encoder codes it and at what rate or quality.
    let seat = if picture.starts_with("HW") { "hardware" } else { "software" };
    if let Some(mode) = &hevc_mode {
        text.push_str(&format!("\nHEVC: {seat} encoder, {mode}"));
    }
    if let Some(mode) = &av1_mode {
        text.push_str(&format!("\nAV1: {seat} encoder, {mode}"));
    }
    if !waiting { text.push_str("\nThe export keeps running while you do other things; if the client only closes this server's input it finishes before the server exits, but if the server is stopped by a signal (as most clients do when they quit) it is cancelled and its partial file removed — poll export_status until it finishes."); }
    if !sub_note.is_empty() {
        text.push('\n');
        text.push_str(&sub_note);
    }
    // A picture-less mp4 is the sound alone and drops the captions: said first, before the job.
    if let Some(lead) = &lead {
        text = format!("{lead}\n{text}");
    }
    for note in anchor_note
        .iter()
        .chain(stale.iter())
        .chain(warning.iter())
        .chain(captions_note.iter())
        .chain(range_note.iter())
        .chain(glyph_note.iter())
        .chain(clash.iter())
        .chain(overlap.iter())
        .chain(chroma.iter())
        .chain(chapter_note.iter())
        .chain(depth_note.iter())
    {
        text.push('\n');
        text.push_str(note);
    }
    Reply::ok(text).with(json!({
        "job": job,
        "queued": waiting,
        "out": out.display().to_string(),
        "size": size.map(|(w, h)| format!("{w}x{h}")),
        "preset": preset.map(|p| p.id),
        "fit": fitted.as_ref().map(|(f, _)| if *f == FitPolicy::Fill { "fill" } else { "fit" }),
        "captions_burned": burned,
        "warning": warning,
        "chapters": chapters_written,
        "bit_depth": depth.map(|(ten, _)| if ten { 10 } else { 8 }),
    }))
}

/// Whether any picture source the timeline reads is 10-bit: the same test the engine's AV1
/// seat makes ([`engine::export`]'s `av1_ten_bit`). Still images have no bit depth to read.
fn source_ten_bit(s: &PlaybackSession) -> bool {
    s.sources()
        .iter()
        .filter(|src| !engine::is_image(&src.path))
        .any(|src| Demuxer::open(&src.path).is_ok_and(|(_, d)| d.bit_depth() == 10))
}

/// The reply's words for an export `fit`: which lanes' full-frame clips it put on the policy, and
/// that the overlays kept their own aspect. `fill` on V1 (1 clip); overlays keep their own aspect.
fn fit_words(fit: FitPolicy, outcome: &FitOutcome) -> String {
    let word = if fit == FitPolicy::Fill { "fill" } else { "fit" };
    let on = if outcome.lanes.is_empty() {
        "no full-frame picture clip".to_string()
    } else {
        let lanes: Vec<String> = outcome
            .lanes
            .iter()
            .map(|(lane, n)| format!("{} ({n} clip{})", lane.label(), if *n == 1 { "" } else { "s" }))
            .collect();
        format!("on {}", lanes.join(", "))
    };
    let overlays = if outcome.overlays > 0 { "; overlays keep their own aspect" } else { "" };
    format!("; fit {word} {on}{overlays}")
}

/// The frame the picture and the sound end at, titles included: where a whole-timeline
/// export stops. The timeline's own end also counts captions, which can run past it.
fn media_end(s: &PlaybackSession) -> u32 {
    let clips = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .filter_map(|l| s.lane_clips(l).iter().map(|c| c.end()).max())
        .max()
        .unwrap_or(0);
    let titles = s.titles().iter().map(|t| t.end()).max().unwrap_or(0);
    clips.max(titles)
}

/// One line per distinct video source the export reads (a clip on a video lane that
/// overlaps `range`, or anywhere on the timeline when there is none), naming the chroma
/// the engine reduces on the way out, e.g. `src:2 clip444.webm: VP9 4:4:4 source: chroma
/// reduced to 4:2:0 for export`. Each source is opened once; one that will not open, a
/// still and a sound file are skipped without a word.
fn chroma_notes(project: &engine::project::Project, settings: &ExportSettings, range: Option<(u32, u32)>) -> Vec<String> {
    let (lo, hi) = range.unwrap_or((0, u32::MAX));
    let used: std::collections::BTreeSet<usize> = project
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Video)
        .flat_map(|lane| project.lane(lane).iter().filter(|c| c.start < hi && c.end() > lo).map(|c| c.source).collect::<Vec<_>>())
        .collect();
    used.into_iter()
        .filter_map(|i| {
            let source = project.sources().get(i)?;
            if engine::is_image(&source.path) || engine::is_audio(&source.path) {
                return None;
            }
            let (meta, demux) = Demuxer::open(&source.path).ok()?;
            let words = chroma_reduction_words(meta.codec, demux.chroma()?, settings)?;
            let name = source.path.file_name().map_or_else(|| source.path.display().to_string(), |n| n.to_string_lossy().into_owned());
            Some(format!("src:{i} {name}: {words}"))
        })
        .collect()
}

/// A frame count as seconds to the millisecond, the figure a reply's `(n.nnns)` shows, e.g. `18.000`.
fn secs(frames: u32, fps: f64) -> String {
    format!("{:.3}", f64::from(frames) / fps)
}

/// Whether a job is still writing: no outcome recorded and the encoder not yet finished.
pub fn job_running(job: &Job) -> bool {
    job.outcome.is_none() && !job.is_finished()
}

/// Moves the queue on: a waiting job whose cancel arrived is settled by `Job::cancel`; when no
/// worker is running, the first waiting job starts from its own copy of the timeline. One encode
/// at a time. Called on every server tick, so a queue drains with no client polling.
pub fn advance(srv: &mut Server) {
    for job in srv.jobs.iter_mut() {
        settle(job);
    }
    if srv.jobs.iter().any(|j| j.handle.is_some() && job_running(j)) {
        return;
    }
    if let Some(job) = srv.jobs.iter_mut().find(|j| j.outcome.is_none() && j.waiting())
        && let Some(p) = job.pending.take()
    {
        job.handle = Some(engine::export::start(p.project, p.meta, &job.out, &p.settings, p.sample_rate));
        job.started = Instant::now();
    }
}

/// Records a finished job's outcome, once. Does nothing while it is still running.
pub fn settle(job: &mut Job) {
    if job.outcome.is_none() && job.is_finished() {
        job.outcome = job.result().map(|r| r.map_err(|e| e.to_string()));
    }
}

fn export_status(srv: &mut Server, args: &Value) -> Reply {
    // `out` names an export by its file. A job this session started for it is reported as
    // its job; any other file is one an earlier session may have written, said as such.
    if let Some(raw) = opt_str(args, "out") {
        let anchored = match anchor_out(srv, raw) {
            Ok((p, _)) => PathBuf::from(p),
            Err(e) => return Reply::err(e),
        };
        return match srv.jobs.iter().position(|j| j.out == anchored) {
            Some(i) => export_status(srv, &json!({ "job": i + 1 })),
            None => Reply::ok(unknown_out_text(&anchored)).with(json!({"jobs": [], "finished": 0})),
        };
    }
    if srv.jobs.is_empty() && args.get("job").is_none() {
        return Reply::ok(
            "no exports in this session — export_start makes one. An export from an earlier session is not tracked here: pass out (its file path) to see whether its file is there",
        )
        .with(json!({"jobs": [], "finished": 0}));
    }
    // The jobs this poll measured: their true-peak warning is said on this poll, and only here.
    let mut measured_now = Vec::new();
    // The master limiter as it stands, so the advice never repeats the ceiling the project already holds.
    let limiter = srv.session.as_ref().map(|s| s.limiter()).filter(|l| l.on).map(|l| l.ceiling_db);
    for (i, job) in srv.jobs.iter_mut().enumerate() {
        settle(job);
        // Measured once, from the written file: the mix's own levels are not the file's.
        if matches!(job.outcome, Some(Ok(()))) && job.measured.is_none() {
            job.measured = Some(file_levels(&job.out));
            measured_now.push(i);
        }
    }
    let asked = args.get("job").is_some();
    let finished_total = srv.jobs.iter().filter(|j| j.outcome.is_some()).count();
    // One job when asked for; otherwise every unfinished job and the last few finished
    // ones. A finished job stays in `jobs`, so its result is reported on every poll.
    let (ids, hidden): (Vec<usize>, usize) = match opt_u64(args, "job") {
        Some(n) => match job_index(srv, n) {
            Ok(i) => (vec![i], 0),
            Err(e) => {
                return Reply::err(format!(
                    "{e}; an export from an earlier session is not tracked here: pass out (its file path) to see whether its file is there"
                ))
            }
        },
        None => {
            let unfinished = (0..srv.jobs.len()).filter(|&i| srv.jobs[i].outcome.is_none());
            let finished: Vec<usize> = (0..srv.jobs.len())
                .filter(|&i| srv.jobs[i].outcome.is_some())
                .collect();
            let hidden = finished.len().saturating_sub(FINISHED_LISTED);
            let mut ids: Vec<usize> = unfinished.chain(finished.into_iter().skip(hidden)).collect();
            ids.sort_unstable();
            (ids, hidden)
        }
    };

    let mut lines = Vec::new();
    let mut rows = Vec::new();
    if args.get("job").is_none() {
        let unfinished = srv.jobs.len() - finished_total;
        lines.push(format!("exports: {unfinished} unfinished, {finished_total} finished"));
    }
    for i in ids {
        let job = &srv.jobs[i];
        let n = i + 1;
        let out = job.out.display().to_string();
        let elapsed = if job.waiting() { 0.0 } else { job.started.elapsed().as_secs_f64() };
        let p = f64::from(job.progress()).clamp(0.0, 1.0);

        let (state, line, bytes, eta, error, took) = match &job.outcome {
            Some(Ok(())) => {
                let bytes = std::fs::metadata(&job.out).map(|m| m.len()).ok();
                let size = bytes.map_or_else(|| "size unknown".to_string(), human_bytes);
                let took = took_secs(job);
                let took_text = took.map_or_else(String::new, |t| format!(" · took {}", took_words(t)));
                // An HEVC or AV1 file's average bitrate, measured from the written file.
                let rate_text = (job.format.is_hevc() || job.format.is_av1())
                    .then(|| engine::demux::probe_bitrate(&job.out).total)
                    .flatten()
                    .map_or_else(String::new, |bps| format!(" · average bitrate {}", bitrate_text(bps)));
                let levels_text = job.measured.flatten().map_or_else(String::new, |m| {
                    let short = !m.lufs.is_finite() && sound_too_short(&job.out);
                    format!(" · integrated {} · true peak {}", lufs_text(m.lufs, short), peak_text(m.true_peak_dbtp))
                });
                (
                    "done",
                    format!("job:{n} done — {size} → {out}{took_text}{rate_text}{}{sound_text}{levels_text}", picture_note(job), sound_text = sound_note(job)),
                    bytes,
                    None,
                    None,
                    took,
                )
            }
            Some(Err(e)) => {
                let state = if e.to_ascii_lowercase().contains("cancel") { "cancelled" } else { "failed" };
                (state, format!("job:{n} {state} — {e} → {out}"), None, None, Some(e.clone()), None)
            }
            None => {
                let eta = if p > 0.01 { Some(elapsed * (1.0 - p) / p) } else { None };
                let enc = match job.encoders() {
                    Some(_) => format!("{}{}", picture_note(job), sound_note(job)),
                    None if !job.picture.is_empty() => format!(" · video: {} (planned)", plain_video(job.format, &job.picture)),
                    None => String::new(),
                };
                let state = if job.waiting() {
                    "waiting"
                } else if job.is_cancelling() {
                    "cancelling"
                } else if job.is_finished() {
                    "finishing"
                } else {
                    "running"
                };
                let eta_text = eta.map_or_else(|| "unknown".to_string(), clock_secs);
                let ahead = srv.jobs[..i].iter().filter(|j| j.outcome.is_none()).count();
                let line = match job.stage() {
                    _ if job.waiting() => format!("job:{n} waiting · {ahead} export{} ahead → {out}", if ahead == 1 { "" } else { "s" }),
                    // A stage before the encode has its own percent; the encode's ETA
                    // means nothing yet.
                    Some((stage, f)) => format!(
                        "job:{n} {state} · {stage} {:.0}% · elapsed {} · encoding {:.0}%{enc} → {out}",
                        f * 100.0,
                        clock_secs(elapsed),
                        p * 100.0
                    ),
                    None => format!(
                        "job:{n} {state} {:.0}% · elapsed {} · ETA {eta_text}{enc} → {out}",
                        p * 100.0,
                        clock_secs(elapsed)
                    ),
                };
                (
                    state,
                    line,
                    None,
                    eta,
                    None,
                    None,
                )
            }
        };
        lines.push(line);
        let levels = job.measured.flatten();
        let peak = levels.map(|m| m.true_peak_dbtp);
        // Said once per job: on the poll that measured its file, or whenever it is asked for by number.
        let lossy = !matches!(job.format, engine::export::Format::Wav | engine::export::Format::Flac);
        if (asked || measured_now.contains(&i)) && let Some(w) = peak_warning(n, peak, lossy, limiter) {
            lines.push(w);
        }
        rows.push(json!({
            "job": n,
            "out": out,
            "state": state,
            "progress": p,
            "stage": job.stage().map(|(name, _)| name),
            "stage_progress": job.stage().map(|(_, f)| f),
            "elapsed_s": elapsed,
            "eta_s": eta,
            "duration_s": job.length_secs(),
            "took_s": took,
            "bytes": bytes,
            "error": error,
            "integrated_lufs": levels.map(|m| m.lufs),
            "true_peak_dbtp": peak,
        }));
    }
    if hidden > 0 {
        lines.push(format!(
            "({hidden} earlier finished export{} not shown; ask with job:<n>)",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    if lines.is_empty() {
        lines.push("no exports to show".to_string());
    }
    Reply::ok(lines.join("\n")).with(json!({ "jobs": rows, "finished": finished_total }))
}

/// The true-peak ceiling the advice names: -2 dBTP for a lossy file, whose encoder can add about a
/// dB of overshoot above the mix, and -1 dBTP for a lossless one. When the master limiter is already
/// on at that ceiling, the call the project has is the one advised, so the advice steps down a dB.
fn advised_ceiling(lossy: bool, limiter: Option<f32>) -> f64 {
    let base = if lossy { -2.0 } else { -1.0 };
    match limiter {
        Some(c) if (f64::from(c) - base).abs() < 0.05 => base - 1.0,
        _ => base,
    }
}

/// The warning a finished file's true peak earns: over full scale (0 dBTP) the samples clip,
/// and the reply names the limiter setting and the gain that stop it; over the ceiling the
/// mix is held to, the normalize that holds it. `None` for a file that is under both, or silent.
fn peak_warning(n: usize, peak: Option<f64>, lossy: bool, limiter: Option<f32>) -> Option<String> {
    let p = peak?;
    let ceiling = advised_ceiling(lossy, limiter);
    // "The mix clips" only when the peak reads above 0 at the one decimal the reply prints:
    // a peak under +0.05 dBTP prints as +0.0 and is the ceiling case, not a clip.
    if p >= CLIP_DBTP {
        Some(format!(
            "warning: job:{n} true peak {p:+.1} dBTP: the mix clips, so the file has samples past full scale. turn the master limiter on with a {ceiling} dBTP ceiling: project_settings {{limiter:{{on:true, ceiling_db:{ceiling}}}}} (the same as the edit op limiter {{on:true, ceiling_db:{ceiling}}}), then export again; or lower the level with lane_gain {{lane, db}}"
        ))
    } else if p > PEAK_CEILING_DBTP {
        Some(format!(
            "warning: job:{n} true peak {} dBTP is above the -1 dBTP ceiling (clips on some players) the mix is held to; the file itself (decoded, with the encoder's overshoot) is what was measured. normalize {{target:\"mix\", allow_limiter:true, true_peak:{ceiling}}} holds the mix to the ceiling{}; then export again",
            ceiling_figure(p),
            if lossy { " (a lossy encode adds up to about 1 dB, so the ceiling sits under -1)" } else { "" }
        ))
    } else {
        None
    }
}

/// `2026-10-17 05:57:03` in local time: when a file was written.
fn file_when(t: std::time::SystemTime) -> String {
    if t.duration_since(std::time::UNIX_EPOCH).is_err() {
        return "before 1970".to_string();
    }
    let when = chrono::DateTime::<chrono::Local>::from(t);
    when.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The reply for an `out` that no job of this session writes: whether its file is there,
/// its size and when it was written, and that an earlier session's job (whose progress this
/// session cannot see) may have written it, or may still be writing its `.part`.
fn unknown_out_text(out: &Path) -> String {
    let head = format!(
        "no export to {} in this session, so its progress is not known here: a job for it ran in an earlier session, or never ran",
        out.display()
    );
    match std::fs::metadata(out) {
        Ok(m) => {
            let size = human_bytes(m.len());
            let when = m.modified().map_or_else(|_| "at an unknown time".to_string(), file_when);
            format!("{head}. The file is there: {size}, written {when}")
        }
        Err(_) => {
            let mut part = out.as_os_str().to_owned();
            part.push(".part");
            if Path::new(&part).exists() {
                format!("{head}. No file yet, but a partial {} is there: that export is still writing, or was killed", Path::new(&part).display())
            } else {
                format!("{head}. No file is there at that path")
            }
        }
    }
}

/// How long a finished export took, from its job's start to the write of its file.
/// The file is written last, so its modification time is the finish. `None` when the
/// file is gone or the clock went backwards.
/// A file's average bitrate in bits a second, in the words a reply uses: `4.00 Mb/s`, or `850 kb/s` under a megabit.
fn bitrate_text(bps: u64) -> String {
    let kbps = bps / 1000;
    if kbps >= 1000 {
        format!("{:.2} Mb/s", kbps as f64 / 1000.0)
    } else {
        format!("{kbps} kb/s")
    }
}

fn took_secs(job: &Job) -> Option<f64> {
    let finished = std::fs::metadata(&job.out).ok()?.modified().ok()?;
    let started = std::time::SystemTime::now().checked_sub(job.started.elapsed())?;
    finished.duration_since(started).ok().map(|d| d.as_secs_f64())
}

fn export_cancel(srv: &mut Server, args: &Value) -> Reply {
    let Some(n) = opt_u64(args, "job") else {
        return Reply::err("export_cancel needs `job`: the number from export_start (job:<n>)");
    };
    let i = match job_index(srv, n) {
        Ok(i) => i,
        Err(e) => return Reply::err(e),
    };
    let job = &mut srv.jobs[i];
    if job.outcome.is_some() || job.is_finished() {
        return Reply::ok(format!("job:{n} already finished — nothing to cancel"));
    }
    if job.waiting() {
        job.cancel();
        return Reply::ok(format!("job:{n} removed from the queue — it was waiting and never started"));
    }
    job.cancel();
    Reply::ok(format!(
        "job:{n} cancel requested — the worker stops at its next checkpoint and deletes the partial file"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::time::{Duration, Instant};

    #[test]
    fn hardware_av1_is_refused_but_auto_and_software_are_not() {
        // The GPU crash: hardware AV1 (and WebM, which the engine seats as AV1) is refused.
        assert!(hw_av1_refused(Format::Av1, EncoderSeat::Hardware));
        assert!(hw_av1_refused(Format::Av1Mp4, EncoderSeat::Hardware));
        assert!(hw_av1_refused(Format::Webm, EncoderSeat::Hardware));
        // `auto` never reaches the GPU for AV1 (the engine seats it in software), and the
        // software seat is always allowed. Hardware H.264 is not this rule's business.
        assert!(!hw_av1_refused(Format::Av1, EncoderSeat::Auto));
        assert!(!hw_av1_refused(Format::Av1Mp4, EncoderSeat::Auto));
        assert!(!hw_av1_refused(Format::Av1, EncoderSeat::Software));
        assert!(!hw_av1_refused(Format::Mp4, EncoderSeat::Hardware));
    }

    #[test]
    fn picture_label_names_the_engine_seat_then_the_plan_and_skips_audio_only() {
        let measured = Some("HW encode (VA-API) · AAC · SW encode (ec-aac)".to_string());
        assert_eq!(picture_label(measured, "SW encode (rav1e)", Format::Mp4), " · video: H.264 (hardware)");
        assert_eq!(picture_label(None, "SW encode (ec-h264)", Format::Mp4), " · video: H.264 (software)");
        assert_eq!(picture_label(Some("copy · AAC".into()), "SW encode (ec-h264)", Format::Mp4), " · video: H.264 (copy)");
        assert_eq!(picture_label(None, "", Format::Mp4), "");
        assert_eq!(picture_label(None, "SW encode (rav1e)", Format::Webm), " · video: AV1 (software)");
    }

    /// The codec and container words a start reply and a status line use, with no engine labels left.
    #[test]
    fn export_words_are_plain() {
        assert_eq!(
            av_words(Format::Webm, "SW encode (rav1e)", "Opus · SW encode (ec-opus)"),
            "video: AV1 (software), audio: Opus, container: WebM"
        );
        assert_eq!(
            av_words(Format::Mp4, "SW encode (ec-h264)", "AAC · SW encode (ec-aac)"),
            "video: H.264 (software), audio: AAC, container: MP4"
        );
        assert_eq!(av_words(Format::Mp4, "copy (source packets)", "AAC copy"), "video: H.264 (copy), audio: AAC (copy), container: MP4");
        assert_eq!(av_words(Format::Wav, "", "PCM · SW (hound)"), "audio: PCM, container: WAV");
        assert_eq!(av_words(Format::Gif, "", ""), "no sound, container: GIF");
        assert_eq!(av_words(Format::PngSequence, "", ""), "no sound, container: PNG folder");
        assert_eq!(measured_sound(Some("HW encode (VA-API) · AAC · SW encode (ec-aac)")).as_deref(), Some("AAC"));
        assert_eq!(measured_sound(Some("SW encode (ec-h264)")), None);
        assert_eq!(measured_sound(None), None);
        assert_eq!(plain_audio("no sound to write"), "none");
    }

    /// An audio-only job (an m4a, or an mp4 with no picture) reports its sound from the line's
    /// first part, which is the codec: `AAC · SW encode (ec-aac)` is the AAC the engine wrote
    /// by re-encoding, and `AAC copy` the source's own packets. A job with a picture still skips
    /// its picture seat to reach the sound.
    #[test]
    fn an_audio_only_job_reports_its_sound_from_the_codec_part() {
        assert_eq!(sound_words(true, Some("AAC · SW encode (ec-aac)")).as_deref(), Some("AAC"));
        assert_eq!(sound_words(true, Some("AAC copy")).as_deref(), Some("AAC (copy)"));
        assert_eq!(sound_words(true, Some("PCM · SW (hound)")).as_deref(), Some("PCM"));
        assert_eq!(sound_words(true, Some("no sound to write")).as_deref(), Some("none"));
        assert_eq!(sound_words(true, None), None);
        assert_eq!(sound_words(false, Some("SW encode (ec-h264) · AAC · SW encode (ec-aac)")).as_deref(), Some("AAC"));
        assert_eq!(sound_words(false, Some("SW encode (ec-h264)")), None);
    }

    /// An Auto HEVC export says in its start reply which encoder codes it and in which GOP
    /// mode: I+P is software at its rate (or constant quality), intra-only says so.
    #[test]
    fn auto_hevc_start_line_names_the_seat_and_the_gop_mode() {
        engine::ao::set_silent_output();
        let root = env!("CARGO_MANIFEST_DIR");
        let src = format!("{root}/../../assets/test_av.mp4");
        let dir = scratch("hevc-start-line");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open the source"));
        let range = json!({"from": 0, "to": 1});

        let at_rate = export_start(&mut srv, &json!({
            "out": dir.join("rate.mp4").display().to_string(), "format": "hevc-mp4", "range": range, "bitrate_kbps": 4000
        }));
        assert!(!at_rate.is_error, "{}", at_rate.text);
        assert!(
            at_rate.text.contains("\nHEVC: software encoder, I+P GOP (1 s keyframes), 4 Mb/s"),
            "the I+P line names the seat, the GOP and the rate: {}",
            at_rate.text
        );

        let quality = export_start(&mut srv, &json!({
            "out": dir.join("quality.mp4").display().to_string(), "format": "hevc-mp4", "range": range
        }));
        assert!(!quality.is_error, "{}", quality.text);
        assert!(
            quality.text.contains("\nHEVC: software encoder, I+P GOP (1 s keyframes), constant quality"),
            "the automatic row says constant quality: {}",
            quality.text
        );

        // Auto takes the GPU for intra-only where there is one, so either seat is right here.
        let intra = export_start(&mut srv, &json!({
            "out": dir.join("intra.mp4").display().to_string(), "format": "hevc-mp4", "range": range, "intra_only": true
        }));
        assert!(!intra.is_error, "{}", intra.text);
        assert!(
            intra.text.contains("\nHEVC: software encoder, intra-only")
                || intra.text.contains("\nHEVC: hardware encoder, intra-only"),
            "intra-only is named with its seat: {}",
            intra.text
        );
        // Let the three jobs finish before the scratch directory goes.
        for _ in 0..600 {
            if srv.jobs.iter().all(|j| j.is_finished()) { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// A start reply echoes an absolute output path as given, and names the codecs and the
    /// container in plain words; the finished job says the same.
    #[test]
    fn start_reply_echoes_an_absolute_out_in_plain_words() {
        engine::ao::set_silent_output();
        let root = env!("CARGO_MANIFEST_DIR");
        let src = format!("{root}/../../assets/test_seek_chirp.mkv");
        let dir = scratch("words-start");
        let out = dir.join("highlight.wav");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open the source"));
        let started = export_start(&mut srv, &json!({"out": out.display().to_string()}));
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains(&format!("started → {}", out.display())), "absolute out is echoed: {}", started.text);
        assert!(started.text.contains("container: WAV"), "{}", started.text);
        assert!(!started.text.contains("SW encode"), "no engine labels in the reply: {}", started.text);
        assert!(!started.text.contains("picture:"), "{}", started.text);
        let deadline = Instant::now() + Duration::from_secs(120);
        while srv.jobs[0].outcome.is_none() {
            assert!(Instant::now() < deadline, "export did not finish in 120s");
            export_status(&mut srv, &json!({ "job": 1 }));
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// A PQ (HDR) picture's export names the tone map it will apply, on the start line; a
    /// sound-only export names none.
    #[test]
    fn export_start_names_the_tone_map_of_an_hdr_picture() {
        let dir = scratch("tone");
        let src = format!("{}/../../assets/test_pq.mp4", env!("CARGO_MANIFEST_DIR"));
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open assets/test_pq.mp4"));
        let out = dir.join("pq.mp4");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string()}));
        assert!(!started.is_error, "{}", started.text);
        // test_pq.mp4 is 320x240: standard definition, so the file is tagged BT.601.
        assert!(started.text.contains("picture: HDR (PQ) source tone-mapped to SDR BT.601"), "{}", started.text);
        let _ = export_cancel(&mut srv, &json!({ "job": 1 }));
        let wav = dir.join("pq.wav");
        let sound = export_start(&mut srv, &json!({"out": wav.display().to_string()}));
        assert!(!sound.text.contains("picture:"), "{}", sound.text);
    }

    /// The chroma note: a 4:4:4 VP9 source is named once in the start reply, with the
    /// reduction the engine makes; a 4:2:0 source names nothing.
    #[test]
    fn export_start_names_the_chroma_reduction_of_a_444_source_once() {
        let dir = scratch("chroma");
        let src = dir.join("clip444.webm");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x240:d=2"])
            .args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuv444p", "-t", "2"])
            .arg(&src)
            .output()
            .expect("ffmpeg runs");
        assert!(made.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&made.stderr));
        let range = json!({"from": 0, "to": 1});
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open the 4:4:4 clip"));
        let started = export_start(&mut srv, &json!({"out": dir.join("c444.mp4").display().to_string(), "range": range}));
        assert!(!started.is_error, "{}", started.text);
        let note = "src:0 clip444.webm: VP9 4:4:4 source: chroma reduced to 4:2:0 for export";
        assert_eq!(started.text.matches(note).count(), 1, "{}", started.text);
        let _ = export_cancel(&mut srv, &json!({ "job": 1 }));

        let mut plain = Server::new();
        plain.session = Some(engine::PlaybackSession::open(Path::new(&format!("{}/../../assets/test_speed_sync.mp4", env!("CARGO_MANIFEST_DIR")))).expect("open a 4:2:0 clip"));
        let quiet = export_start(&mut plain, &json!({"out": dir.join("c420.mp4").display().to_string(), "range": range}));
        assert!(!quiet.is_error, "{}", quiet.text);
        assert!(!quiet.text.contains("chroma reduced"), "{}", quiet.text);
        let _ = export_cancel(&mut plain, &json!({ "job": 1 }));
    }

    /// The size words: WIDTHxHEIGHT and NNNp (16:9 at that height), even sides, within the
    /// engine's ceiling. Anything else is refused by name.
    #[test]
    fn size_reads_widths_by_height_and_refuses_what_no_picture_can_be() {
        let size = |v: Value| parse_size(&json!({ "size": v }));
        assert_eq!(parse_size(&json!({})), Ok(None));
        assert_eq!(size(json!("1280x720")), Ok(Some((1280, 720))));
        assert_eq!(size(json!(" 854X480 ")), Ok(Some((854, 480))));
        assert_eq!(size(json!("720p")), Ok(Some((1280, 720))));
        assert_eq!(size(json!("1080p")), Ok(Some((1920, 1080))));
        assert_eq!(size(json!("2160p")), Ok(Some((3840, 2160))));
        assert_eq!(size(json!("480p")), Ok(Some((854, 480))));
        for bad in ["1281x720", "1280x721", "0x720", "8192x4320", "720", "wide", "x720", "-2x720"] {
            let e = size(json!(bad)).expect_err(bad);
            assert!(e.contains("size"), "{bad}: {e}");
        }
        assert!(size(json!(7680)).is_err(), "a number is not a size");
        assert!(size(json!("7682p")).is_err(), "past the engine's ceiling");
    }

    /// A `size` draws the file at that size and leaves the project alone: the reply names both,
    /// the project's resolution stays where it was, and the export's own settings carry it.
    #[test]
    fn export_size_is_the_files_canvas_and_the_project_keeps_its_own() {
        let dir = scratch("size");
        let src = format!("{}/../../assets/test_pq.mp4", env!("CARGO_MANIFEST_DIR"));
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open assets/test_pq.mp4"));
        let before = srv.session.as_ref().expect("a session").export_snapshot().1;
        let out = dir.join("small.mp4");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string(), "size": "160x120"}));
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("size 160x120 (project 320x240 kept)"), "{}", started.text);
        assert_eq!(started.structured.as_ref().and_then(|v| v.get("size")), Some(&json!("160x120")));
        let after = srv.session.as_ref().expect("a session").export_snapshot().1;
        assert_eq!((before.width, before.height), (after.width, after.height), "the project is not resized");
        let refused = export_start(&mut srv, &json!({"out": dir.join("odd.mp4").display().to_string(), "size": "161x120"}));
        assert!(refused.is_error && refused.text.contains("even"), "{}", refused.text);
        let _ = export_cancel(&mut srv, &json!({ "job": 1 }));
    }

    /// `export_presets` lists every row of the engine's table, and `export_start { preset }` fills
    /// the settings from one while an explicit argument overrides it.
    #[test]
    fn export_presets_list_and_fill_export_start() {
        let mut srv = Server::new();
        let listed = export_presets(&mut srv, &json!({}));
        assert!(!listed.is_error, "{}", listed.text);
        let rows = listed.structured.as_ref().expect("structured")["presets"].as_array().expect("rows").clone();
        assert_eq!(rows.len(), engine::export_presets::presets().len());
        let podcast = rows.iter().find(|r| r["id"] == "podcast").expect("podcast row");
        assert_eq!(podcast["format"], "m4a");
        assert_eq!(podcast["loudness"]["lufs"], -16.0);
        assert_eq!(podcast["loudness"]["true_peak_dbtp"], -1.0);
        let archive = rows.iter().find(|r| r["id"] == "archive").expect("archive row");
        assert_eq!(archive["bit_depth"], 10);
        assert_eq!(archive["format"], "av1");

        let dir = scratch("preset");
        let src = format!("{}/../../assets/test_pq.mp4", env!("CARGO_MANIFEST_DIR"));
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open assets/test_pq.mp4"));
        let unknown = export_start(&mut srv, &json!({"out": dir.join("x.mp4").display().to_string(), "preset": "nope"}));
        assert!(unknown.is_error && unknown.text.contains("youtube_1080p"), "{}", unknown.text);

        let out = dir.join("short.mp4");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string(), "preset": "shorts_reels"}));
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("size 1080x1920"), "{}", started.text);
        assert!(started.text.contains("preset shorts_reels"), "{}", started.text);
        let _ = export_cancel(&mut srv, &json!({ "job": 1 }));

        let out = dir.join("short2.mp4");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string(), "preset": "shorts_reels", "size": "160x120"}));
        assert!(!started.is_error, "{}", started.text);
        assert_eq!(started.structured.as_ref().and_then(|v| v.get("size")), Some(&json!("160x120")));
        let _ = export_cancel(&mut srv, &json!({ "job": 2 }));

        let out = dir.join("pod.m4a");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string(), "preset": "podcast"}));
        assert!(!started.is_error, "{}", started.text);
        assert_eq!(started.structured.as_ref().and_then(|v| v.get("preset")), Some(&json!("podcast")));
        let _ = export_cancel(&mut srv, &json!({ "job": 3 }));
    }

    /// A Matroska export names the sound it will get: a 44.1 kHz AAC source is mixed to AAC, the
    /// codec its done line reports, rather than the Opus-or-AAC the start line used to say.
    #[test]
    fn a_matroska_start_line_names_aac_for_a_44k_mix() {
        let dir = scratch("mkv-start");
        let src = format!("{}/../../assets/test_av1.mkv", env!("CARGO_MANIFEST_DIR"));
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open assets/test_av1.mkv"));
        let started = export_start(&mut srv, &json!({"out": dir.join("av1.mkv").display().to_string()}));
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("audio: AAC, container: Matroska"), "{}", started.text);
        assert!(!started.text.contains("as the mix decides"), "{}", started.text);
        let _ = export_cancel(&mut srv, &json!({ "job": 1 }));
    }

    /// duration_s is the length of the file the export writes (its range, here one second), known
    /// from the start and the same on every poll. It is not how long the encode took, which is
    /// `took_s`: a 45 s timeline that encoded in 20 s reported 19.8 before this.
    #[test]
    fn export_status_duration_is_the_files_length_not_the_encode_time() {
        engine::ao::set_silent_output();
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(small_clip()).expect("open assets/test_speed_sync.mp4"));
        let dir = scratch("duration");
        let out = dir.join("one.mp4");
        let args = json!({ "out": out.display().to_string(), "format": "mp4", "range": { "from": 0, "to": 1 } });
        assert!(!export_start(&mut srv, &args).is_error);
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = export_status(&mut srv, &json!({ "job": 1 }));
            if srv.jobs[0].outcome.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "export did not finish in 120s: {}", st.text);
            std::thread::sleep(Duration::from_millis(200));
        }
        let st = export_status(&mut srv, &json!({ "job": 1 }));
        let row = &st.structured.as_ref().unwrap()["jobs"][0];
        let duration = row["duration_s"].as_f64().expect("duration_s is a number");
        assert!((duration - 1.0).abs() < 0.05, "a one-second range: {duration}");
        assert!(row["took_s"].as_f64().is_some(), "the encode time is still reported: {row}");
    }

    /// The three picture formats write their files through `export_start`: a numbered PNG run into
    /// the folder named, a PNG still of the frame at the range's start, and a GIF whose width and
    /// rate are the arguments given; a GIF past 720 px is refused by name.
    #[test]
    fn the_picture_formats_write_their_files() {
        let dir = scratch("pictures");
        let src = format!("{}/../../assets/test_25fps.mp4", env!("CARGO_MANIFEST_DIR"));
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(Path::new(&src)).expect("open assets/test_25fps.mp4"));

        let folder = dir.join("run");
        let started = export_start(&mut srv, &json!({"out": folder.display().to_string(), "format": "png_sequence", "range": { "from": "0f", "to": "25f" }}));
        assert!(!started.is_error, "{}", started.text);
        finish_job(&mut srv, 1);
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .expect("the folder the run is written into")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 25, "{names:?}");
        assert_eq!(names[0], "run_00001.png");

        let still = dir.join("poster.png");
        let started = export_start(&mut srv, &json!({"out": still.display().to_string(), "range": { "from": "10f", "to": "11f" }}));
        assert!(!started.is_error, "{}", started.text);
        finish_job(&mut srv, 2);
        assert!(still.exists(), "the still was written");

        let gif = dir.join("clip.gif");
        let started = export_start(&mut srv, &json!({"out": gif.display().to_string(), "range": { "from": "0f", "to": "25f" }, "gif_width": 320, "gif_fps": 10, "gif_loop": false}));
        assert!(!started.is_error, "{}", started.text);
        finish_job(&mut srv, 3);
        let bytes = std::fs::read(&gif).expect("the GIF was written");
        assert!(bytes.starts_with(b"GIF89a"), "a GIF file");
        // The logical screen's width is the header's little-endian u16 at byte 6.
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 320, "the width asked for");

        // Refused by the engine when the export runs, so the job starts and then fails by name.
        let wide = export_start(&mut srv, &json!({"out": dir.join("wide.gif").display().to_string(), "gif_width": 1280}));
        assert!(!wide.is_error, "{}", wide.text);
        let deadline = Instant::now() + Duration::from_secs(120);
        while srv.jobs[3].outcome.is_none() {
            assert!(Instant::now() < deadline, "the refused GIF did not finish");
            export_status(&mut srv, &json!({ "job": 4 }));
            std::thread::sleep(Duration::from_millis(50));
        }
        let outcome = format!("{:?}", srv.jobs[3].outcome);
        assert!(outcome.contains("pixels wide"), "{outcome}");
        assert!(!dir.join("wide.gif").exists(), "a refused GIF leaves no file");
    }

    /// A fresh folder for one test, removed when the test ends.
    fn scratch(name: &str) -> TestDir {
        TestDir::new(&format!("export-{name}"))
    }

    /// The clip the job tests export from: 320x180 rather than the 1280x720
    /// `test_av.mp4`. These tests check the job lifecycle, not the picture, and a
    /// debug build encodes 16x fewer pixels in this one.
    fn small_clip() -> String {
        format!("{}/../../assets/test_speed_sync.mp4", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn format_inference() {
        assert_eq!(resolve_out("/x/a.mp4", None).unwrap().1, Format::Mp4);
        assert_eq!(resolve_out("/x/a.mkv", None).unwrap().1, Format::Av1);
        assert_eq!(resolve_out("/x/a.webm", None).unwrap().1, Format::Webm);
        assert_eq!(resolve_out("/x/a.wav", None).unwrap().1, Format::Wav);
        assert_eq!(resolve_out("/x/a.flac", None).unwrap().1, Format::Flac);
        assert_eq!(resolve_out("/x/a.mp3", None).unwrap().1, Format::Mp3);
        assert_eq!(resolve_out("/x/a.ogg", None).unwrap().1, Format::Ogg);
        assert_eq!(resolve_out("/x/a.m4a", None).unwrap().1, Format::M4a);
        assert_eq!(resolve_out("/x/a", Some("m4a")).unwrap().0, PathBuf::from("/x/a.m4a"));
        assert_eq!(format_from_name("M4A"), Some(Format::M4a));
        assert_eq!(format_name(Format::M4a), "m4a");
        assert_eq!(resolve_out("/x/a.mkv", Some("av1")).unwrap().1, Format::Av1);
        assert_eq!(resolve_out("/x/a.mp4", Some("hevc-mp4")).unwrap().1, Format::HevcMp4);
        assert_eq!(
            resolve_out("/x/a", Some("webm")).unwrap().0,
            PathBuf::from("/x/a.webm")
        );
        assert!(resolve_out("/x/a.mkv", Some("mp4")).is_err());
        assert!(resolve_out("/x/a.mp4", Some("av1-mp4")).is_ok());
        assert!(resolve_out("/x/a.xyz", None).is_err());
        assert!(resolve_out("/x/a", None).is_err());
        // A GIF is written now: the name follows the format, and a mismatched extension still refuses.
        assert_eq!(resolve_out("/x/a", Some("gif")).unwrap(), (PathBuf::from("/x/a.gif"), Format::Gif));
        assert!(resolve_out("/x/a.mp4", Some("gif")).is_err());
    }

    #[test]
    fn prores_hdr_and_sound_depth_arguments_parse_and_name_what_they_are_for() {
        let prores = Format::ProRes;
        for (name, want) in [
            ("proxy", ProResProfile::Proxy),
            ("lt", ProResProfile::Lt),
            ("standard", ProResProfile::Standard),
            ("hq", ProResProfile::Hq),
            ("4444", ProResProfile::P4444),
            ("4444xq", ProResProfile::P4444Xq),
        ] {
            assert_eq!(parse_prores_profile(&json!({ "prores_profile": name }), prores), Ok(Some(want)), "{name}");
            assert_eq!(prores_name(want), name);
        }
        assert_eq!(parse_prores_profile(&json!({}), prores), Ok(None));
        assert!(parse_prores_profile(&json!({ "prores_profile": "max" }), prores).unwrap_err().contains("4444xq"));
        assert!(parse_prores_profile(&json!({ "prores_profile": "hq" }), Format::Mp4).unwrap_err().contains("format prores"));
        assert_eq!(parse_hevc_speed(&json!({ "hevc_speed": "fast" }), Format::Hevc), Ok(Some(HevcSpeed::Fast)));
        assert_eq!(parse_hevc_speed(&json!({}), Format::Hevc), Ok(None));
        assert!(parse_hevc_speed(&json!({ "hevc_speed": "fast" }), Format::Mp4).unwrap_err().contains("hevc"));
        assert!(parse_hevc_speed(&json!({ "hevc_speed": "max" }), Format::Hevc).unwrap_err().contains("faster"));

        assert_eq!(parse_hdr(&json!({ "hdr": "pq" }), Format::HevcMp4), Ok(Some(HdrTransfer::Pq)));
        assert_eq!(parse_hdr(&json!({ "hdr": "HLG" }), Format::Hevc), Ok(Some(HdrTransfer::Hlg)));
        assert_eq!(parse_hdr(&json!({ "hdr": "off" }), Format::Mp4), Ok(Some(HdrTransfer::Sdr)));
        assert!(parse_hdr(&json!({ "hdr": "pq" }), Format::Mp4).unwrap_err().contains("hevc"));
        assert!(parse_hdr(&json!({ "hdr": "dolby" }), Format::Hevc).is_err());

        assert_eq!(parse_sound_bits(&json!({ "audio_bit_depth": 16 }), prores), Ok(Some(16)));
        assert_eq!(parse_sound_bits(&json!({ "audio_bit_depth": 24 }), prores), Ok(Some(24)));
        assert_eq!(parse_sound_bits(&json!({}), Format::Wav), Ok(None));
        assert!(parse_sound_bits(&json!({ "audio_bit_depth": 20 }), prores).is_err());
        assert!(parse_sound_bits(&json!({ "audio_bit_depth": 16 }), Format::Wav).is_err());

        assert_eq!(format_from_name("prores"), Some(Format::ProRes));
        assert_eq!(infer_format("mov"), Some(Format::ProRes));
        assert_eq!(resolve_out("/x/a.mov", None).map(|(_, f)| f), Ok(Format::ProRes));
        assert!(resolve_out("/x/a.mp4", Some("prores")).is_err(), "prores is written as .mov");
    }

    #[test]
    fn the_prores_and_hdr_presets_are_listed_with_their_settings() {
        let rows: Vec<Value> = engine::export_presets::presets().iter().map(preset_json).collect();
        let master = rows.iter().find(|r| r["id"] == "archive_prores").expect("prores row");
        assert_eq!((master["format"].as_str(), master["prores_profile"].as_str()), (Some("prores"), Some("hq")));
        let hdr = rows.iter().find(|r| r["id"] == "hdr10_hevc").expect("hdr row");
        assert_eq!((hdr["hdr"].as_str(), hdr["bit_depth"].as_u64()), (Some("pq"), Some(10)));
        assert_eq!(rows[0]["hdr"], "off");
    }

    /// A ProRes export through the tool writes a .mov of ProRes video and PCM sound.
    #[test]
    fn a_prores_export_writes_a_mov() {
        engine::ao::set_silent_output();
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(small_clip()).expect("open assets/test_speed_sync.mp4"));
        let dir = scratch("prores");
        let out = dir.join("master.mov");
        std::fs::remove_file(&out).ok();
        let args = json!({
            "out": out.display().to_string(),
            "prores_profile": "lt",
            "audio_bit_depth": 16,
            "seat": "software",
            "range": { "from": 0, "to": 1 }
        });
        let started = export_start(&mut srv, &args);
        assert!(!started.is_error, "{}", started.text);
        let deadline = Instant::now() + Duration::from_secs(120);
        while srv.jobs[0].outcome.is_none() {
            let st = export_status(&mut srv, &json!({ "job": 1 }));
            assert!(!st.is_error, "{}", st.text);
            assert!(Instant::now() < deadline, "export did not finish in 120s: {}", st.text);
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(matches!(srv.jobs[0].outcome, Some(Ok(()))), "{:?}", srv.jobs[0].outcome);
        assert!(std::fs::metadata(&out).expect("output exists").len() > 0);
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
            .arg(&out)
            .output();
        if let Some(probe) = probe.ok().filter(|p| p.status.success()) {
            let names = String::from_utf8_lossy(&probe.stdout).to_string();
            assert!(names.contains("prores"), "{names}");
        }
    }

    #[test]
    fn overwrite_refused() {
        let dir = scratch("overwrite");
        let f = dir.join("exists.mp4");
        std::fs::write(&f, b"x").unwrap();
        assert!(check_out(&f, false).unwrap_err().contains("overwrite"));
        assert!(check_out(&f, true).is_ok());
        assert!(check_out(&dir.join("missing").join("a.mp4"), false)
            .unwrap_err()
            .contains("does not exist"));
    }

    #[test]
    fn short_real_export() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let session = engine::PlaybackSession::open(&src).expect("open assets/test_speed_sync.mp4");
        let mut srv = Server::new();
        srv.session = Some(session);

        let dir = scratch("real");
        let out = dir.join("short.mp4");
        std::fs::remove_file(&out).ok();
        let args = json!({
            "out": out.display().to_string(),
            "format": "mp4",
            "range": { "from": 0, "to": 1 }
        });
        let started = export_start(&mut srv, &args);
        assert!(!started.is_error, "{}", started.text);

        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = export_status(&mut srv, &json!({ "job": 1 }));
            assert!(!st.is_error, "{}", st.text);
            if srv.jobs[0].outcome.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "export did not finish in 120s: {}", st.text);
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(matches!(srv.jobs[0].outcome, Some(Ok(()))), "{:?}", srv.jobs[0].outcome);
        let size = std::fs::metadata(&out).expect("output exists").len();
        assert!(size > 0);
        // The done line names the file's integrated loudness and true peak, and the structured
        // reply carries both: the same measurement `loudness` reports for the file.
        let done = export_status(&mut srv, &json!({ "job": 1 }));
        assert!(done.text.contains("· integrated ") && done.text.contains("· true peak "), "{}", done.text);
        let direct = engine::loudness::measure_file(&out, 0, None).unwrap().unwrap();
        let row = &done.structured.as_ref().unwrap()["jobs"][0];
        if direct.integrated_lufs.is_finite() {
            let lufs = row["integrated_lufs"].as_f64().expect("integrated_lufs is a number");
            assert!((direct.integrated_lufs - lufs).abs() < 1e-9, "same measurement as loudness");
        } else {
            assert!(done.text.contains("integrated silent"), "{}", done.text);
        }
    }

    /// A file whose sound is under one 400 ms block says it is too short to measure, not silent: a
    /// 0.1 s tone is real sound, and only the block length keeps it from a loudness figure.
    #[test]
    fn a_tenth_of_a_second_of_sound_is_too_short_to_measure_not_silent() {
        let dir = scratch("short-sound");
        let path = dir.join("short.wav");
        // 0.1 s of a 440 Hz tone at 48 kHz, mono 16-bit PCM.
        let frames = 4800u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&96000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            let x = 0.5 * (2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / 48000.0).sin();
            wav.extend_from_slice(&((x * 32767.0) as i16).to_le_bytes());
        }
        std::fs::write(&path, &wav).expect("write wav");
        assert!(sound_too_short(&path), "0.1 s is under one block");
        let measured = engine::loudness::measure_file(&path, 0, None).unwrap().unwrap();
        assert!(!measured.integrated_lufs.is_finite(), "no block above the gate in 0.1 s");
        assert!(measured.true_peak_dbtp.is_finite(), "the sound is real");
        assert!(lufs_text(measured.integrated_lufs, sound_too_short(&path)).starts_with("too short to measure loudness"));
    }

    /// A file with sound measures to finite LUFS and dBTP figures, and the text names them.
    #[test]
    fn file_levels_are_the_loudness_of_the_written_file() {
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        let levels = file_levels(Path::new(&src)).expect("a clip with sound measures");
        assert!(levels.lufs.is_finite() && levels.lufs < 0.0, "{levels:?}");
        assert!(levels.true_peak_dbtp.is_finite(), "{levels:?}");
        assert_eq!(lufs_text(-14.04, false), "-14.0 LUFS");
        // The figure rounds to one decimal, never truncates: -16.06 is -16.1, and -15.96 is -16.0.
        assert_eq!(lufs_text(-16.06, false), "-16.1 LUFS");
        assert_eq!(lufs_text(-15.96, false), "-16.0 LUFS");
        assert_eq!(lufs_text(f64::NEG_INFINITY, false), "silent");
        assert_eq!(lufs_text(f64::NEG_INFINITY, true), "too short to measure loudness (under 0.4 s)");
        assert_eq!(peak_text(-1.0), "-1.0 dBTP");
        assert_eq!(peak_text(0.3), "+0.3 dBTP");
        assert_eq!(peak_text(-3.4), "-3.4 dBTP");
        assert_eq!(peak_text(f64::NEG_INFINITY), "silent");
    }

    /// A finished export keeps its result: every poll after it finished reports it, for
    /// one job or for all, and the text is never empty -- not even with no exports.
    #[test]
    fn a_finished_export_is_reported_on_every_poll() {
        engine::ao::set_silent_output();
        let mut srv = Server::new();
        let none = export_status(&mut srv, &json!({}));
        assert!(!none.is_error && !none.text.is_empty(), "{}", none.text);

        let src = small_clip();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open assets/test_speed_sync.mp4"));
        let dir = scratch("status-twice");
        let out = dir.join("twice.mp4");
        std::fs::remove_file(&out).ok();
        let args = json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}});
        let started = export_start(&mut srv, &args);
        assert!(!started.is_error, "{}", started.text);

        let deadline = Instant::now() + Duration::from_secs(120);
        while job_running(&srv.jobs[0]) {
            assert!(Instant::now() < deadline, "export did not finish in 120s");
            std::thread::sleep(Duration::from_millis(100));
        }
        let out_text = out.display().to_string();
        for args in [json!({}), json!({"job": 1}), json!({})] {
            let r = export_status(&mut srv, &args);
            assert!(!r.is_error && !r.text.is_empty(), "{}", r.text);
            assert!(r.text.contains("job:1 done"), "{}", r.text);
            assert!(r.text.contains(&out_text), "{}", r.text);
        }
    }

    /// A start line says what the engine will decide, not what it might: an AV1 or HEVC
    /// Matroska sound is Opus or AAC by the mix, so it says both; WebM is always Opus.
    #[test]
    fn start_line_names_the_sound_codec_the_engine_predicts() {
        assert_eq!(
            av_words(Format::Av1, "copy (source packets)", "Opus · SW encode (ec-opus)"),
            "video: AV1 (copy), audio: Opus, container: Matroska"
        );
        assert_eq!(
            av_words(Format::Av1, "SW encode (rav1e)", "AAC · SW encode (ec-aac)"),
            "video: AV1 (software), audio: AAC, container: Matroska"
        );
        assert_eq!(
            av_words(Format::Hevc, "SW encode (ec-h265 GOP)", "AAC copy"),
            "video: HEVC (software), audio: AAC (copy), container: Matroska"
        );
        assert_eq!(
            av_words(Format::Webm, "SW encode (rav1e)", "Opus · SW encode (ec-opus)"),
            "video: AV1 (software), audio: Opus, container: WebM"
        );
    }

    /// Over 0 dBTP the file clips and the warning names the limiter setting; above the -1 dBTP
    /// ceiling (exactly -1 is within spec) it is the normalize warning; otherwise, or silent, none.
    #[test]
    fn took_reads_one_decimal_under_ten_seconds() {
        assert_eq!(took_words(0.696), "0.7 s");
        assert_eq!(took_words(0.0), "0.0 s");
        assert_eq!(took_words(9.96), "10.0 s", "under ten, one decimal, rounded");
        assert_eq!(took_words(42.4), "42s");
        assert_eq!(took_words(125.0), "2m05s");
    }

    #[test]
    fn fit_is_keep_fit_or_fill_and_refuses_the_rest() {
        assert_eq!(parse_fit(&json!({})), Ok(None));
        assert_eq!(parse_fit(&json!({"fit": "keep"})), Ok(None));
        assert_eq!(parse_fit(&json!({"fit": "Fill"})), Ok(Some(FitPolicy::Fill)));
        assert_eq!(parse_fit(&json!({"fit": "fit"})), Ok(Some(FitPolicy::Fit)));
        let e = parse_fit(&json!({"fit": "stretch"})).unwrap_err();
        assert!(e.contains("keep, fit or fill"), "{e}");
    }

    #[test]
    fn clash_note_names_the_moment_the_title_and_the_place_to_move_to() {
        let note = clash_note(86.5, "EVENING, MIRADOURO", Place::Bottom);
        assert_eq!(
            note,
            "at 86.5s the caption overlaps title 'EVENING, MIRADOURO' (both at the bottom): burn_captions {place:\"top\"} or move the title"
        );
        assert!(clash_note(3.0, "A\nB", Place::Top).contains("title 'A / B' (both at the top): burn_captions {place:\"bottom\"}"));
    }

    /// The clipping advice names the ceiling in plain words: "with a -2 dBTP ceiling" for a lossy
    /// file, -1 for a lossless one, never "under 0".
    #[test]
    fn the_clipping_advice_states_the_ceiling_in_plain_words() {
        let lossy = peak_warning(1, Some(0.43), true, None).expect("0.43 dBTP clips");
        assert!(lossy.contains("turn the master limiter on with a -2 dBTP ceiling: project_settings {limiter:{on:true, ceiling_db:-2}}"), "{lossy}");
        assert!(!lossy.contains("under 0"), "{lossy}");
        let lossless = peak_warning(1, Some(0.43), false, None).expect("0.43 dBTP clips");
        assert!(lossless.contains("with a -1 dBTP ceiling: project_settings {limiter:{on:true, ceiling_db:-1}}"), "{lossless}");
    }

    /// A `to` past the timeline is refused naming both ends; inside the timeline it is accepted
    /// and the caller clips it to the picture.
    #[test]
    fn a_range_past_the_timeline_names_both_ends() {
        let r = |to: &str| json!({"range": {"from": "4s", "to": to}});
        let err = parse_range(&r("12s"), 25.0, 225, 150).expect_err("12 s is past the 9 s timeline");
        assert_eq!(
            err,
            "range.to (300f (12.000s)) is past the end of the timeline (225f (9.000s)): the picture and sound end at 150f (6.000s)"
        );
        assert_eq!(parse_range(&r("8s"), 25.0, 225, 150).unwrap(), Some((100, 200)), "inside the timeline is kept");
        assert_eq!(parse_range(&r("end"), 25.0, 225, 150).unwrap(), Some((100, 225)), "end is the timeline end");
    }

    #[test]
    fn peak_warning_names_the_limiter_when_the_file_clips() {
        let clip = peak_warning(1, Some(0.43), false, None).expect("0.43 dBTP clips");
        assert!(clip.contains("the mix clips") && clip.contains("project_settings {limiter:{on:true, ceiling_db:-1}}"), "{clip}");
        assert!(clip.contains("lane_gain"), "{clip}");
        let ceiling = peak_warning(1, Some(-0.5), false, None).expect("-0.5 dBTP is above the ceiling");
        assert!(ceiling.contains("ceiling (clips on some players) the mix is held to") && !ceiling.contains("the mix clips"), "{ceiling}");
        assert!(ceiling.contains("true peak -0.5 dBTP is above the -1 dBTP ceiling"), "{ceiling}");
        assert!(clip.contains("true peak +0.4 dBTP"), "{clip}");
        // A value that one decimal would print as the ceiling itself shows two.
        let near = peak_warning(1, Some(-0.96), false, None).expect("-0.96 is above the ceiling");
        assert!(near.contains("true peak -0.96 dBTP is above the -1 dBTP ceiling"), "{near}");
        assert_eq!(peak_warning(1, Some(-1.0), false, None), None, "exactly -1 dBTP is within spec");
        assert!(peak_warning(1, Some(-0.99), false, None).is_some(), "just above -1 dBTP warns");
        assert_eq!(peak_warning(1, Some(f64::NEG_INFINITY), false, None), None);
        assert_eq!(peak_warning(1, None, false, None), None);
    }

    /// On an mp4 (lossy) the peak reads as the printed decimal: only +0.1 and up is "the mix clips".
    /// A peak that prints as +0.0 (0.0 itself, or +0.04) is over the -1 ceiling and says so.
    #[test]
    fn an_mp4_peak_clips_only_from_plus_a_tenth_and_else_reads_as_the_ceiling() {
        assert_eq!(peak_warning(1, Some(-2.5), true, None), None, "-2.5 dBTP is within spec");
        assert_eq!(peak_warning(1, Some(-1.0), true, None), None, "exactly -1 dBTP is within spec");
        let zero = peak_warning(1, Some(0.0), true, None).expect("0.0 is over the ceiling");
        assert!(zero.contains("true peak +0.0 dBTP is above the -1 dBTP ceiling") && !zero.contains("the mix clips"), "{zero}");
        let tiny = peak_warning(1, Some(0.04), true, None).expect("+0.04 is over the ceiling");
        assert!(tiny.contains("true peak +0.0 dBTP is above the -1 dBTP ceiling") && !tiny.contains("the mix clips"), "{tiny}");
        let clip = peak_warning(1, Some(0.3), true, None).expect("+0.3 clips");
        assert!(clip.contains("the mix clips") && clip.contains("true peak +0.3 dBTP"), "{clip}");
        let edge = peak_warning(1, Some(0.05), true, None).expect("+0.05 prints as +0.1 and clips");
        assert!(edge.contains("the mix clips"), "{edge}");
    }

    #[test]
    fn peak_advice_aims_lossy_files_at_minus_two_and_steps_past_the_ceiling_in_use() {
        let lossy = peak_warning(1, Some(-0.5), true, None).expect("-0.5 dBTP is above the ceiling");
        assert!(lossy.contains("true_peak:-2}") && lossy.contains("a lossy encode adds up to about 1 dB"), "{lossy}");
        let lossless = peak_warning(1, Some(-0.5), false, None).expect("-0.5 dBTP is above the ceiling");
        assert!(lossless.contains("true_peak:-1}") && !lossless.contains("lossy"), "{lossless}");
        // The master limiter already on at the advised -2 for a lossy file: the advice is the next ceiling down.
        let held = peak_warning(1, Some(-0.5), true, Some(-2.0)).expect("-0.5 dBTP is above the ceiling");
        assert!(held.contains("true_peak:-3}") && !held.contains("true_peak:-2}"), "{held}");
        // A limiter on at some other ceiling is not the call to avoid.
        let other = peak_warning(1, Some(-0.5), true, Some(-1.5)).expect("-0.5 dBTP is above the ceiling");
        assert!(other.contains("true_peak:-2}"), "{other}");
        // A clip on a lossless file whose limiter is on at -1 is told -2, not the -1 already in use.
        let clip = peak_warning(1, Some(0.43), false, Some(-1.0)).expect("0.43 dBTP clips");
        assert!(clip.contains("ceiling_db:-2}") && !clip.contains("ceiling_db:-1}"), "{clip}");
    }

    /// An `out` no job of this session wrote says whether its file is there, with size and
    /// time, and that an earlier session may have run it; a missing file says so too.
    #[test]
    fn unknown_out_says_whether_the_file_is_there() {
        let dir = scratch("unknown-out");
        let there = dir.join("earlier.mp4");
        std::fs::write(&there, vec![0u8; 12_139]).unwrap();
        let text = unknown_out_text(&there);
        assert!(text.contains("earlier session") && text.contains("12.1 KB") && text.contains("written 20"), "{text}");
        let gone = dir.join("never.mp4");
        let text = unknown_out_text(&gone);
        assert!(text.contains("No file is there at that path") && !text.contains("partial"), "{text}");
        std::fs::write(dir.join("writing.mp4.part"), b"x").unwrap();
        let part = unknown_out_text(&dir.join("writing.mp4"));
        assert!(part.contains("a partial") && part.contains("still writing"), "{part}");
    }

    /// Asking for a job or a file this session never started is answered, not a bare
    /// "no exports yet": the reply names the `out` route to the earlier export's file.
    #[test]
    fn status_for_an_unknown_job_or_an_empty_session_points_at_out() {
        let mut srv = Server::new();
        let empty = export_status(&mut srv, &json!({}));
        assert!(!empty.is_error && empty.text.contains("pass out"), "{}", empty.text);
        let job = export_status(&mut srv, &json!({"job": 3}));
        assert!(job.is_error && job.text.contains("pass out"), "{}", job.text);
        let dir = scratch("status-out-gone");
        let gone = dir.join("nothing.mp4");
        let r = export_status(&mut srv, &json!({"out": gone.display().to_string()}));
        assert!(!r.is_error && r.text.contains("No file is there at that path"), "{}", r.text);
    }

    /// A relative `out` is resolved against the project's folder and refused without a
    /// project; a file inside a git tree outside the project folder gets a warning.
    #[test]
    fn relative_out_is_anchored_to_the_project_and_repo_is_warned() {
        let mut srv = Server::new();
        let r = export_start(&mut srv, &json!({"out": "clip.mp4"}));
        assert!(r.is_error && r.text.contains("no project is open"), "{}", r.text);

        let dir = scratch("anchor");
        srv.path = Some(dir.join("proj.edith"));
        let (p, note) = anchor_out(&srv, "clip.mp4").expect("anchored");
        assert_eq!(PathBuf::from(&p), dir.join("clip.mp4"));
        assert!(note.is_some_and(|n| n.contains("resolved against the project folder")));
        let (p, note) = anchor_out(&srv, "/abs/x.mp4").expect("absolute kept");
        assert_eq!(PathBuf::from(&p), PathBuf::from("/abs/x.mp4"));
        assert!(note.is_none());

        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(repo_warning(&repo.join("sub/x.mp4"), Some(dir.path())).is_some());
        assert!(repo_warning(&repo.join("x.mp4"), Some(&repo)).is_none(), "beside the project");
        assert!(repo_warning(&dir.join("x.mp4"), Some(dir.path())).is_none(), "outside any repo");
    }

    #[test]
    fn empty_timeline_and_pictureless_video_are_refused_up_front() {
        engine::ao::set_silent_output();
        let dir = scratch("refuse");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = export_start(&mut srv, &json!({"out": dir.join("a.mp4").display().to_string()}));
        assert!(r.is_error);
        assert_eq!(r.text, "timeline is empty — nothing to export: place a clip first (edit place/append)");

        let tone = format!("{}/../../assets/test_tone.wav", env!("CARGO_MANIFEST_DIR"));
        let session = engine::PlaybackSession::open(&tone).expect("open assets/test_tone.wav");
        srv.session = Some(session);
        // A Matroska file of it would be black: refused. An mp4 is the sound alone (below).
        let r = export_start(&mut srv, &json!({"out": dir.join("b.mkv").display().to_string()}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("no video clips"), "{}", r.text);
        assert!(srv.jobs.is_empty(), "a refused export starts no job");
    }

    /// Runs `ffprobe` on a finished file and returns its `codec_name:codec_type` per stream.
    fn stream_codecs(path: &Path) -> Vec<String> {
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.replace(',', ":"))
            .collect()
    }

    /// Waits for job `n` (1-based) to finish, and checks that it wrote its file.
    fn finish_job(srv: &mut Server, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(120);
        while srv.jobs[n - 1].outcome.is_none() {
            assert!(Instant::now() < deadline, "export did not finish in 120s");
            export_status(srv, &json!({ "job": n }));
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(matches!(srv.jobs[n - 1].outcome, Some(Ok(()))), "{:?}", srv.jobs[n - 1].outcome);
    }

    /// `m4a` writes the timeline's sound as AAC in an mp4 box, and an mp4 of the same
    /// audio-only timeline is the sound alone too: neither is refused, and neither has a picture.
    #[test]
    fn m4a_and_mp4_of_a_tone_timeline_are_the_sound_alone() {
        engine::ao::set_silent_output();
        let tone = format!("{}/../../assets/test_tone.wav", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("m4a");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&tone).expect("open assets/test_tone.wav"));

        let m4a = dir.join("tone.m4a");
        let started = export_start(&mut srv, &json!({"out": m4a.display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("container: M4A"), "{}", started.text);
        finish_job(&mut srv, 1);
        assert_eq!(stream_codecs(&m4a), ["aac:audio"], "one AAC stream and no picture");

        let mp4 = dir.join("tone-sound.mp4");
        let started = export_start(&mut srv, &json!({"out": mp4.display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!started.is_error, "{}", started.text);
        assert!(!started.text.contains("video:"), "no picture is named: {}", started.text);
        finish_job(&mut srv, 2);
        assert_eq!(stream_codecs(&mp4), ["aac:audio"], "an mp4 of a picture-less timeline is audio only");
    }

    /// Subtitles asked for on a file with no picture say so in the reply, for `true` and for a
    /// list alike, and the export still runs: the sound is written, the captions are not.
    #[test]
    fn subtitles_asked_for_a_picture_less_mp4_are_named_as_not_written() {
        engine::ao::set_silent_output();
        let tone = format!("{}/../../assets/test_tone.wav", env!("CARGO_MANIFEST_DIR"));
        let srt = format!("{}/../engine/tests/data/test_subs.srt", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("sound-only-subs");
        let mut srv = Server::new();
        let mut s = engine::PlaybackSession::open(&tone).expect("open assets/test_tone.wav");
        s.import_subtitles(Path::new(&srt)).expect("import test_subs.srt");
        srv.session = Some(s);
        let out = dir.join("sound.mp4").display().to_string();
        for subtitles in [json!(true), json!([0])] {
            let r = export_start(&mut srv, &json!({"out": out, "range": {"from": 0, "to": 1}, "subtitles": subtitles, "overwrite": true}));
            assert!(!r.is_error, "the export is not refused: {}", r.text);
            assert!(
                r.text.contains("subtitles not written: an mp4 with no picture is written as sound only; add a picture"),
                "{}",
                r.text
            );
            assert!(!r.text.contains("none — a timeline"), "the bare card is replaced: {}", r.text);
            let n = srv.jobs.len();
            finish_job(&mut srv, n);
        }
        assert_eq!(stream_codecs(Path::new(&out)), ["aac:audio"], "the sound is written, no subtitle track");
    }

    #[test]
    fn a_running_job_on_the_same_out_path_refuses_even_with_overwrite() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open assets/test_speed_sync.mp4"));
        let dir = scratch("samepath");
        let out = dir.join("same.mp4");
        let args = json!({"out": out.display().to_string(), "overwrite": true});
        let first = export_start(&mut srv, &args);
        assert!(!first.is_error, "{}", first.text);
        let second = export_start(&mut srv, &args);
        if job_running(&srv.jobs[0]) {
            assert!(second.is_error, "{}", second.text);
            assert!(second.text.contains("still writing"), "{}", second.text);
            assert_eq!(srv.jobs.len(), 1, "a refused export starts no job");
        }
        // Cancelled, the first job stops running and the same path is allowed again.
        // Not waited for to finish: the encoder is slow on a loaded machine.
        srv.jobs[0].cancel();
        let deadline = Instant::now() + Duration::from_secs(120);
        while job_running(&srv.jobs[0]) {
            assert!(Instant::now() < deadline, "cancelled export did not stop");
            std::thread::sleep(Duration::from_millis(100));
        }
        let again = export_start(&mut srv, &args);
        assert!(!again.is_error, "{}", again.text);
        srv.jobs[1].cancel();
    }

    /// `queue: true` lines exports up: the second waits with its own copy of the timeline until the
    /// first has finished, nothing is encoded twice at once, and both files land and probe.
    #[test]
    fn queued_exports_run_one_after_another_and_both_files_probe() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open the clip"));
        let dir = scratch("queue-two");
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        for out in [&a, &b] {
            let args = json!({"out": out.display().to_string(), "seat": "software", "queue": true, "range": {"from": 0, "to": 1}});
            let r = export_start(&mut srv, &args);
            assert!(!r.is_error, "{}", r.text);
        }
        assert_eq!(srv.jobs.len(), 2);
        if job_running(&srv.jobs[0]) {
            assert!(srv.jobs[1].waiting(), "the second export waits behind the first");
            let st = export_status(&mut srv, &json!({}));
            assert!(st.text.contains("job:2 waiting"), "{}", st.text);
        }
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            advance(&mut srv);
            // One encode at a time: a waiting job has no worker.
            assert!(srv.jobs.iter().filter(|j| j.handle.is_some() && job_running(j)).count() <= 1);
            if srv.jobs.iter().all(|j| j.outcome.is_some()) {
                break;
            }
            assert!(Instant::now() < deadline, "queue did not finish in 240s");
            std::thread::sleep(Duration::from_millis(100));
        }
        for (i, out) in [&a, &b].into_iter().enumerate() {
            assert!(matches!(srv.jobs[i].outcome, Some(Ok(()))), "{:?}", srv.jobs[i].outcome);
            assert!(std::fs::metadata(out).is_ok_and(|m| m.len() > 0), "{} written", out.display());
            assert!(stream_codecs(out).iter().any(|c| c.starts_with("h264")), "{:?}", stream_codecs(out));
        }
    }

    /// A waiting job cancelled by number never starts and writes nothing; the queue moves past it.
    #[test]
    fn a_waiting_job_can_be_cancelled_and_never_starts() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open the clip"));
        let dir = scratch("queue-cancel");
        let a = dir.join("a.mp4");
        let b = dir.join("b.mp4");
        for out in [&a, &b] {
            let args = json!({"out": out.display().to_string(), "seat": "software", "queue": true});
            assert!(!export_start(&mut srv, &args).is_error);
        }
        if !srv.jobs[1].waiting() {
            // The first finished before the second was queued: nothing to cancel, nothing to check.
            srv.jobs[0].cancel();
            srv.jobs[1].cancel();
            return;
        }
        let r = export_cancel(&mut srv, &json!({"job": 2}));
        assert!(r.text.contains("removed from the queue"), "{}", r.text);
        assert!(matches!(&srv.jobs[1].outcome, Some(Err(e)) if e.contains("cancel")));
        srv.jobs[0].cancel();
        let deadline = Instant::now() + Duration::from_secs(120);
        while job_running(&srv.jobs[0]) {
            advance(&mut srv);
            assert!(Instant::now() < deadline, "cancelled export did not stop");
            std::thread::sleep(Duration::from_millis(100));
        }
        advance(&mut srv);
        assert!(srv.jobs[1].handle.is_none(), "a cancelled waiting job never got a worker");
        assert!(!b.exists(), "nothing written for the removed job");
    }

    /// A caption past the picture end is not written by a whole-timeline export, and the
    /// reply says so and reports the picture's end, not the caption's.
    #[test]
    fn whole_timeline_export_stops_at_the_picture_and_says_so() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let dir = scratch("picture-end");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open the clip"));
        let picture = srv.session.as_ref().expect("session").timeline_duration();
        let past = picture + 2.5;
        let ms = (past * 1000.0) as u64;
        let srt = dir.join("late.srt");
        std::fs::write(
            &srt,
            format!(
                "1\n00:00:00,000 --> 00:00:01,000\nearly\n\n2\n00:{:02}:{:02},{:03} --> 00:{:02}:{:02},{:03}\nlate\n",
                ms / 60000,
                ms / 1000 % 60,
                ms % 1000,
                (ms + 1000) / 60000,
                (ms + 1000) / 1000 % 60,
                (ms + 1000) % 1000
            ),
        )
        .expect("write the srt");
        let s = srv.session.as_mut().expect("session");
        s.import_subtitles(&srt).expect("import the srt");
        // Placed on S1 so the captions count toward the timeline's end.
        let frames = (past * session_fps(s)).round() as u32;
        let sub = engine::project::SubClip { start: 0, frames, track: 0, in_us: 0, out_us: (past * 1e6) as i64, link: None };
        s.add_lane(LaneKind::Subtitle);
        s.place_sub(engine::project::Lane::new(LaneKind::Subtitle, 0), 0, sub).expect("place the captions");
        assert!(media_end(srv.session.as_ref().expect("session")) < session_end(srv.session.as_ref().expect("session")));

        let out = dir.join("picture-end.mp4");
        let r = export_start(&mut srv, &json!({"out": out.display().to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions run to"), "{}", r.text);
        assert!(r.text.contains("past the picture and sound end at 3.000s: they are not exported"), "{}", r.text);
        assert!(r.text.contains("whole timeline, to "), "{}", r.text);
        let deadline = Instant::now() + Duration::from_secs(240);
        for job in srv.jobs.iter_mut() {
            while job_running(job) {
                assert!(Instant::now() < deadline, "the export did not finish");
                std::thread::sleep(Duration::from_millis(200));
            }
            settle(job);
            assert!(matches!(job.outcome, Some(Ok(()))), "{:?}", job.outcome);
        }
        let s = srv.session.as_ref().expect("session");
        assert!(media_end(s) as f64 / session_fps(s) <= picture + 0.05);
    }

    fn burn_session() -> Server {
        engine::ao::set_silent_output();
        let src = small_clip();
        let srt = format!("{}/../engine/tests/data/test_subs.srt", env!("CARGO_MANIFEST_DIR"));
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&src).expect("open assets/test_speed_sync.mp4"));
        srv.session
            .as_mut()
            .expect("session")
            .import_subtitles(Path::new(&srt))
            .expect("import test_subs.srt");
        srv
    }

    #[test]
    fn a_burn_names_the_characters_no_face_draws_once_each() {
        let dir = scratch("burn_glyph");
        let srt = dir.join("odd.srt");
        // U+10FFFD is a private-use noncharacter no face has; the emoji is covered by the fallback.
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:03,000\nHello \u{10FFFD} and \u{10FFFD} \u{1F3AC}\n\n")
            .expect("write the srt");
        let mut srv = burn_session();
        srv.session
            .as_mut()
            .expect("session")
            .import_subtitles(srt.as_path())
            .expect("import odd.srt");
        let s = srv.session.as_ref().expect("session");
        // burn_session imports test_subs.srt as track 0, so the odd file is track 1.
        assert_eq!(
            missing_glyph_note(s, 1).as_deref(),
            Some("no glyph for: U+10FFFD (drawn as an empty box)")
        );
        assert_eq!(missing_glyph_note(s, 0), None, "test_subs.srt draws every character");
    }

    #[test]
    fn burn_captions_refuses_bad_settings_and_accepts_a_good_track() {
        let mut srv = burn_session();
        let dir = scratch("burn");
        let out = dir.join("burn.mp4").display().to_string();

        let bad_track = export_start(&mut srv, &json!({"out": out, "burn_captions": {"track": 3}}));
        assert!(bad_track.is_error && bad_track.text.contains("does not exist"), "{}", bad_track.text);
        let bad_size = export_start(&mut srv, &json!({"out": out, "burn_captions": {"size": 500}}));
        assert!(bad_size.is_error && bad_size.text.contains("size"), "{}", bad_size.text);
        let bad_key = export_start(&mut srv, &json!({"out": out, "burn_captions": {"colour": "red"}}));
        assert!(bad_key.is_error && bad_key.text.contains("unknown key"), "{}", bad_key.text);
        let audio = export_start(
            &mut srv,
            &json!({"out": dir.join("a.wav").display().to_string(), "burn_captions": {}}),
        );
        assert!(audio.is_error && audio.text.contains("audio only"), "{}", audio.text);
        assert!(srv.jobs.is_empty(), "refused exports start no job");

        let s = srv.session.as_ref().expect("session");
        let (track, style) = parse_burn(
            &json!({"burn_captions": {"size": 80, "outline": "thick", "place": "top", "boxed": false}}),
            s,
        )
        .expect("valid")
        .expect("present");
        assert_eq!(track, 0, "the default track is track 0 when no lane places one");
        assert_eq!(style.size, 80);
        assert_eq!(style.outline, Outline::Thick);
        assert_eq!(style.place, Place::Top);
        assert!(!style.boxed);
        let (_, style) = parse_burn(&json!({"burn_captions": {"family": "Noto Serif"}}), s)
            .expect("valid")
            .expect("present");
        assert_eq!(style.family.as_deref(), Some("Noto Serif"), "the family is the one named");
        assert!(parse_burn(&json!({"burn_captions": {"family": 3}}), s).is_err(), "a family is a name");
        assert!(parse_burn(&json!({}), s).expect("valid").is_none());
        assert!(parse_burn(&json!({"burn_captions": false}), s).expect("valid").is_none());
        let (track, style) = parse_burn(&json!({"burn_captions": true}), s)
            .expect("true is valid")
            .expect("true is on");
        assert_eq!((track, style.size, style.outline, style.place), (0, 50, Outline::Thin, Place::Bottom));
        assert!(parse_burn(&json!({"burn_captions": "yes"}), s).unwrap_err().contains("object or a boolean"));

        // Under a second of picture, across the first cue (0.5 s to 1.5 s), burned in.
        let started = export_start(
            &mut srv,
            &json!({
                "out": out,
                "range": {"from": "0.4s", "to": "1.0s"},
                "burn_captions": {"track": 0}
            }),
        );
        assert!(!started.is_error, "{}", started.text);
        assert!(started.text.contains("captions burned (track 0)"), "{}", started.text);
        // A second of 320x180 picture: a few seconds in a debug build. The deadline is
        // generous for a loaded machine.
        let deadline = Instant::now() + Duration::from_secs(120);
        while job_running(&srv.jobs[0]) {
            assert!(Instant::now() < deadline, "burned export did not finish in 120s");
            std::thread::sleep(Duration::from_millis(200));
        }
        settle(&mut srv.jobs[0]);
        assert!(matches!(srv.jobs[0].outcome, Some(Ok(()))), "{:?}", srv.jobs[0].outcome);
    }

    #[test]
    fn burn_captions_takes_the_animation_keys_and_defaults_them_from_the_project() {
        use engine::captionanim::CaptionAnim;
        let mut srv = burn_session();
        let burn = |srv: &Server, b: Value| parse_burn(&json!({"burn_captions": b}), srv.session.as_ref().unwrap());
        let (_, style) = burn(&srv, json!({
            "animation": "karaoke", "highlight": "#12ab9f", "box": true, "uppercase": true, "words_per_line": 3
        }))
        .expect("valid")
        .expect("present");
        let cs = style.caption;
        assert_eq!(cs.anim, CaptionAnim::Karaoke);
        assert_eq!(cs.highlight, [0x12, 0xab, 0x9f]);
        assert!(cs.hl_box && cs.uppercase);
        assert_eq!(cs.words_per_line, 3);
        let (_, style) = burn(&srv, json!({"highlight": "#f80"})).unwrap().unwrap();
        assert_eq!(style.caption.highlight, [0xff, 0x88, 0x00], "#rgb");
        assert!(burn(&srv, json!({})).unwrap().unwrap().1.caption.is_plain(), "static by default");
        for (bad, word) in [
            (json!({"animation": "spin"}), "animation"),
            (json!({"highlight": "yellow"}), "highlight"),
            (json!({"box": "yes"}), "box"),
            (json!({"words_per_line": 13}), "words_per_line"),
            (json!({"uppercase": 1}), "uppercase"),
        ] {
            let e = burn(&srv, bad).unwrap_err();
            assert!(e.contains(word), "{e}");
        }
        // The project's caption style is the default; a key overrides it; undo takes it back.
        let r = crate::tools::project::project_settings(
            &mut srv,
            &json!({"captions": {"animation": "pop", "words_per_line": 2}}),
        );
        assert!(!r.is_error, "{}", r.text);
        let (_, style) = burn(&srv, json!({"uppercase": true})).unwrap().unwrap();
        assert_eq!((style.caption.anim, style.caption.words_per_line, style.caption.uppercase), (CaptionAnim::Pop, 2, true));
        let r = crate::tools::project::project_settings(&mut srv, &json!({"captions": {"animation": "pop", "words_per_line": 2}}));
        assert!(r.text.contains("already"), "{}", r.text);
        let r = crate::tools::project::project_settings(&mut srv, &json!({"captions": {"animation": "wiggle"}}));
        assert!(r.is_error && r.text.contains("refused"), "{}", r.text);
        assert!(srv.session.as_mut().unwrap().undo());
        assert!(srv.session.as_ref().unwrap().caption_style().is_plain());
    }

    #[test]
    fn burn_only_export_carries_no_soft_subtitle_track() {
        let mut srv = burn_session();
        let dir = scratch("burn-only");
        let burned = dir.join("burn-only.mp4");
        let both = dir.join("burn-and-soft.mp4");
        let common = json!({"range": {"from": "0.4s", "to": "1.0s"}, "burn_captions": {"track": 0}});
        let mut with_soft = common.clone();
        with_soft["subtitles"] = json!([0]);
        with_soft["out"] = json!(both.display().to_string());
        let mut only = common.clone();
        only["out"] = json!(burned.display().to_string());
        assert!(!export_start(&mut srv, &only).is_error);
        assert!(!export_start(&mut srv, &with_soft).is_error);
        let deadline = Instant::now() + Duration::from_secs(240);
        for job in srv.jobs.iter_mut() {
            while job_running(job) {
                assert!(Instant::now() < deadline, "burned exports did not finish");
                std::thread::sleep(Duration::from_millis(200));
            }
            settle(job);
            assert!(matches!(job.outcome, Some(Ok(()))), "{:?}", job.outcome);
        }
        let tracks = engine::subtitle::of_mp4(&burned).expect("read the burned export back");
        assert!(tracks.is_empty(), "a burn-only export carries soft tracks: {tracks:?}");
        let tracks = engine::subtitle::of_mp4(&both).expect("read the export back");
        assert_eq!(tracks.len(), 1, "an explicit subtitles pick still adds its track");
    }

    /// Soft subtitle tracks are opt-in: a ranged export that names none carries none, even
    /// with captions placed on the timeline; `subtitles: true` carries every track.
    #[test]
    fn soft_subtitles_are_opt_in_and_true_means_every_track() {
        let mut srv = burn_session();
        let dir = scratch("soft-opt-in");
        let plain = dir.join("plain.mp4");
        let all = dir.join("all.mp4");
        let range = json!({"range": {"from": "0.4s", "to": "1.0s"}});
        let mut plain_args = range.clone();
        plain_args["out"] = json!(plain.display().to_string());
        let mut all_args = range.clone();
        all_args["subtitles"] = json!(true);
        all_args["out"] = json!(all.display().to_string());
        let mut bad = range.clone();
        bad["subtitles"] = json!("yes");
        bad["out"] = json!(dir.join("bad.mp4").display().to_string());
        assert!(export_start(&mut srv, &bad).is_error, "a string is not a subtitles value");
        assert!(!export_start(&mut srv, &plain_args).is_error);
        assert!(!export_start(&mut srv, &all_args).is_error);
        let deadline = Instant::now() + Duration::from_secs(240);
        for job in srv.jobs.iter_mut() {
            while job_running(job) {
                assert!(Instant::now() < deadline, "exports did not finish");
                std::thread::sleep(Duration::from_millis(200));
            }
            settle(job);
            assert!(matches!(job.outcome, Some(Ok(()))), "{:?}", job.outcome);
        }
        let tracks = engine::subtitle::of_mp4(&plain).expect("read the plain export back");
        assert!(tracks.is_empty(), "an export without `subtitles` carries soft tracks: {tracks:?}");
        let tracks = engine::subtitle::of_mp4(&all).expect("read the export back");
        assert_eq!(tracks.len(), 1, "`subtitles: true` carries every track");
    }

    /// A picked track no caption lane places is written whole, beside the lanes, and the
    /// reply names it: the card and the file agree. A placed track keeps its lane and is
    /// never written twice. `subtitles: true` is every track.
    #[test]
    fn a_picked_track_no_lane_places_is_written_whole_and_named() {
        engine::ao::set_silent_output();
        let mkv = format!("{}/../../assets/test_subs.mkv", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("unplaced-pick");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(small_clip()).expect("open the clip"));
        let s = srv.session.as_mut().expect("session");
        s.import_subtitles(Path::new(&mkv)).expect("import test_subs.mkv");
        // Track 0 is the English subrip, track 1 the French ass ("Signs"). Nothing is placed.
        let (fps, end) = (session_fps(s), session_end(s));
        let fra = dir.join("fra.mp4");
        let r = export_start(&mut srv, &json!({"out": fra.display().to_string(), "subtitles": [1], "overwrite": true}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("fra — Signs → embedded"), "{}", r.text);
        finish_job(&mut srv, 1);
        let tracks = engine::subtitle::of_mp4(&fra).expect("read the mp4 back");
        assert_eq!(tracks.len(), 1, "the picked track is written: {tracks:?}");
        assert_eq!(tracks[0].language, "fra");

        // Now track 0 is placed on S1: its lane is written at its placement, and the
        // unplaced French track beside it, once each.
        let s = srv.session.as_mut().expect("session");
        s.add_lane(LaneKind::Subtitle);
        let sub = engine::project::SubClip {
            start: 0,
            frames: end,
            track: 0,
            in_us: 0,
            out_us: (f64::from(end) / fps * 1e6) as i64,
            link: None,
        };
        s.place_sub(engine::project::Lane::new(LaneKind::Subtitle, 0), 0, sub).expect("place the English track");
        let all = dir.join("all.mkv");
        let r = export_start(&mut srv, &json!({"out": all.display().to_string(), "subtitles": true, "overwrite": true}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 tracks → embedded (S1 eng, fra — Signs)"), "{}", r.text);
        finish_job(&mut srv, 2);
        let tracks = engine::subtitle::of_matroska(&all).expect("read the mkv back");
        assert_eq!(tracks.len(), 2, "one lane and one whole track, each once: {tracks:?}");

        // A burn takes a placed track and an unplaced one alike: the same cues either way.
        for track in [0, 1] {
            let burn = dir.join(format!("burn{track}.mp4"));
            let r = export_start(&mut srv, &json!({"out": burn.display().to_string(), "burn_captions": {"track": track}, "overwrite": true}));
            assert!(!r.is_error, "burn of track {track} is refused: {}", r.text);
            finish_job(&mut srv, 3 + track);
        }
    }

    /// An mp4 writes overlapping cues as one stretch with both lines, so the reply names the
    /// track; an mkv keeps every cue and is not named.
    #[test]
    fn overlapping_cues_are_named_as_merged_in_an_mp4_only() {
        engine::ao::set_silent_output();
        let dir = scratch("overlap-note");
        let srt = dir.join("overlap.srt");
        std::fs::write(&srt, "1\n00:00:00,100 --> 00:00:00,600\nfirst\n\n2\n00:00:00,300 --> 00:00:00,900\nsecond\n")
            .expect("write the srt");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(small_clip()).expect("open the clip"));
        srv.session.as_mut().expect("session").import_subtitles(&srt).expect("import the srt");
        let mp4 = export_start(&mut srv, &json!({"out": dir.join("o.mp4").display().to_string(), "subtitles": [0], "overwrite": true}));
        assert!(!mp4.is_error, "{}", mp4.text);
        assert!(mp4.text.contains("overlapping cues are merged in mp4 ("), "{}", mp4.text);
        assert!(mp4.text.contains("mkv keeps them"), "{}", mp4.text);
        finish_job(&mut srv, 1);
        let mkv = export_start(&mut srv, &json!({"out": dir.join("o.mkv").display().to_string(), "subtitles": [0], "overwrite": true}));
        assert!(!mkv.is_error, "{}", mkv.text);
        assert!(!mkv.text.contains("merged in mp4"), "{}", mkv.text);
        finish_job(&mut srv, 2);
    }

    /// A range that runs past the picture is clipped to it, and the reply says so: the
    /// length it names is the file's, and captions past the picture are not exported.
    #[test]
    fn a_range_past_the_picture_end_is_clipped_and_said() {
        engine::ao::set_silent_output();
        let dir = scratch("range-clip");
        let srt = dir.join("long.srt");
        std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:30,000\nstays on screen\n").expect("write the srt");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(small_clip()).expect("open the clip"));
        let s = srv.session.as_mut().expect("session");
        s.import_subtitles(&srt).expect("import the srt");
        let (fps, media) = (session_fps(s), media_end(s));
        // Placed to 30 s, so the timeline runs far past the picture and the range can reach it.
        s.add_lane(LaneKind::Subtitle);
        let sub = engine::project::SubClip { start: 0, frames: 30 * fps as u32, track: 0, in_us: 0, out_us: 30_000_000, link: None };
        s.place_sub(engine::project::Lane::new(LaneKind::Subtitle, 0), 0, sub).expect("place the captions");
        assert!(media < session_end(s), "the captions run past the picture");
        let want = format!("range 0f (0.000s) to {}", tc(media, fps));
        let r = export_start(&mut srv, &json!({"out": dir.join("r.mp4").display().to_string(), "range": {"from": "0s", "to": "30s"}, "overwrite": true}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&want), "the reply names the length the file is: {}", r.text);
        assert!(r.text.contains("the range runs past the picture and sound end at"), "{}", r.text);
        finish_job(&mut srv, 1);
    }

    /// An audio-only export does not take subtitles: the reply says so, and does not offer
    /// `subtitles` as the way to add one.
    #[test]
    fn an_audio_only_export_says_subtitles_do_not_apply() {
        engine::ao::set_silent_output();
        let tone = format!("{}/../../assets/test_tone.wav", env!("CARGO_MANIFEST_DIR"));
        let srt = format!("{}/../engine/tests/data/test_subs.srt", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("audio-only-subs");
        let mut srv = Server::new();
        let mut s = PlaybackSession::open(&tone).expect("open assets/test_tone.wav");
        s.import_subtitles(Path::new(&srt)).expect("import test_subs.srt");
        srv.session = Some(s);
        let r = export_start(&mut srv, &json!({"out": dir.join("tone.m4a").display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("subtitles do not apply:"), "{}", r.text);
        assert!(!r.text.contains("pass subtitles to add one"), "{}", r.text);
        finish_job(&mut srv, 1);
    }

    /// The fit reply names the lanes whose full-frame clips it refit and says the overlays kept
    /// their own aspect; a file's average bitrate is said in Mb/s or kb/s.
    #[test]
    fn fit_reply_names_the_lanes_and_the_overlays_keep_their_aspect() {
        use engine::export::FitOutcome;
        use engine::project::Lane;
        let outcome = FitOutcome { lanes: vec![(Lane::V1, 1)], overlays: 1 };
        assert_eq!(fit_words(FitPolicy::Fill, &outcome), "; fit fill on V1 (1 clip); overlays keep their own aspect");
        let two = FitOutcome { lanes: vec![(Lane::V1, 2), (Lane::new(engine::project::LaneKind::Video, 1), 1)], overlays: 0 };
        assert_eq!(fit_words(FitPolicy::Fit, &two), "; fit fit on V1 (2 clips), V2 (1 clip)");
        let none = FitOutcome { lanes: Vec::new(), overlays: 2 };
        assert_eq!(fit_words(FitPolicy::Fill, &none), "; fit fill no full-frame picture clip; overlays keep their own aspect");
        assert_eq!(bitrate_text(4_000_000), "4.00 Mb/s");
        assert_eq!(bitrate_text(850_000), "850 kb/s");
    }

    /// A podcast with no picture exported to mp4 is the sound alone and drops its captions: the
    /// reply says so first, before the job line, and names m4a and a still picture as the way out.
    #[test]
    fn a_picture_less_mp4_with_captions_says_so_first() {
        engine::ao::set_silent_output();
        let tone = format!("{}/../../assets/test_tone.wav", env!("CARGO_MANIFEST_DIR"));
        let srt = format!("{}/../engine/tests/data/test_subs.srt", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("podcast-lead");
        let mut srv = Server::new();
        let mut s = PlaybackSession::open(&tone).expect("open assets/test_tone.wav");
        s.import_subtitles(Path::new(&srt)).expect("import test_subs.srt");
        srv.session = Some(s);
        let r = export_start(&mut srv, &json!({"out": dir.join("podcast.mp4").display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("sound only: an mp4 with no picture"), "{}", r.text);
        assert!(r.text.contains("For sound alone use m4a") && r.text.contains("a still image"), "{}", r.text);
        assert!(r.text.find("job:1 started").unwrap() > 0, "the job line follows: {}", r.text);
        finish_job(&mut srv, 1);
    }

    /// An AV1 export says its seat and its average bitrate target in the start reply, like HEVC.
    #[test]
    fn auto_av1_start_line_names_the_seat_and_the_bitrate() {
        engine::ao::set_silent_output();
        let src = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        let dir = scratch("av1-start-line");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(Path::new(&src)).expect("open the source"));
        let r = export_start(&mut srv, &json!({
            "out": dir.join("av1.mp4").display().to_string(), "format": "av1-mp4", "range": {"from": 0, "to": 1}
        }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("\nAV1: software encoder, average bitrate "), "{}", r.text);
        assert!(r.text.contains("pass bitrate_kbps for a fixed rate"), "{}", r.text);
        finish_job(&mut srv, 1);
    }

    /// A cancelled export says `cancelling` for as long as its worker is still running, and
    /// `cancelled` once it has stopped.
    #[test]
    fn a_cancelled_export_says_cancelling_until_its_worker_stops() {
        engine::ao::set_silent_output();
        let dir = scratch("cancelling");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(small_clip()).expect("open the clip"));
        let out = dir.join("c.mp4");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string(), "overwrite": true}));
        assert!(!started.is_error, "{}", started.text);
        let c = export_cancel(&mut srv, &json!({"job": 1}));
        assert!(!c.is_error, "{}", c.text);
        let r = export_status(&mut srv, &json!({"job": 1}));
        assert!(r.text.contains("job:1 cancelling"), "{}", r.text);
        let deadline = Instant::now() + Duration::from_secs(120);
        while job_running(&srv.jobs[0]) {
            assert!(Instant::now() < deadline, "the cancelled export did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        let r = export_status(&mut srv, &json!({"job": 1}));
        assert!(r.text.contains("job:1 cancelled"), "{}", r.text);
    }

    #[test]
    fn burn_draws_the_edited_lanes_not_the_original_track() {
        use crate::tools::edit::run_edit;
        let mut srv = burn_session();
        let r = run_edit(&mut srv, &json!({"ops": [{"op": "captions_place", "track": 0}]}));
        assert!(!r.is_error, "{}", r.text);
        let r = run_edit(
            &mut srv,
            &json!({"ops": [{"op": "cue_text", "track": 0, "index": 0, "text": "Hello there"}]}),
        );
        assert!(!r.is_error, "{}", r.text);

        // Cut the second cue's time out of the captions, as cut_regions does.
        let s = srv.session.as_mut().expect("session");
        let fps = session_fps(s);
        let lanes = s.subtitle_lanes();
        let (a, b, _, _) = s.sub_lane_cue_refs(lanes[0])[1];
        let cut_text = s.subtitles()[0].cues[1].text.clone();
        let from = (a as f64 * fps / 1e6).round() as u32;
        let len = ((b - a) as f64 * fps / 1e6).round().max(1.0) as u32;
        s.cut_regions(&[(from, len)], &lanes).expect("cut the second cue");

        let s = srv.session.as_ref().expect("session");
        let (project, meta) = s.export_snapshot();
        let drawn = engine::export::burn_cues(&project, 0, meta.frame_rate).expect("track 0");
        let texts: Vec<&str> = drawn.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"Hello there"), "the retyped cue is not burned: {texts:?}");
        assert!(!texts.contains(&cut_text.as_str()), "the cut cue still burns: {texts:?}");
        assert!(drawn.windows(2).all(|w| w[0].start_us <= w[1].start_us), "cues out of order");
    }

    /// A mono clip under a stereo clip: the mix takes the widest source's channel count, so
    /// a stereo clip makes the timeline stereo. The output is stereo and audible in both
    /// clips' spans (the mono clip first, the stereo clip one second in).
    #[test]
    fn a_stereo_clip_makes_the_timeline_stereo() {
        engine::ao::set_silent_output();
        let root = env!("CARGO_MANIFEST_DIR");
        let mono = format!("{root}/../../assets/test_seek_chirp.mkv");
        let stereo = format!("{root}/../../assets/test_av1_grain.mkv");
        let dir = scratch("channels");
        let out = dir.join("c.wav");
        let mut srv = Server::new();
        srv.session = Some(engine::PlaybackSession::open(&mono).expect("open the mono source"));
        let s = srv.session.as_mut().expect("session");
        s.import(Path::new(&stereo)).expect("import the stereo source");
        s.place_stream_at(0.0, Path::new(&stereo), 0, Some(engine::project::Lane::A1))
            .expect("place the stereo source on A1");
        let started = export_start(&mut srv, &json!({"out": out.display().to_string()}));
        assert!(!started.is_error, "{}", started.text);

        let deadline = Instant::now() + Duration::from_secs(120);
        while srv.jobs[0].outcome.is_none() {
            assert!(Instant::now() < deadline, "export did not finish in 120s");
            export_status(&mut srv, &json!({ "job": 1 }));
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(matches!(srv.jobs[0].outcome, Some(Ok(()))), "{:?}", srv.jobs[0].outcome);
        let (audio, chunks) = engine::AudioSession::open(&out)
            .expect("reopen the export")
            .expect("the export has audio");
        assert_eq!(audio.channels, 2, "a stereo clip makes the timeline stereo");
        let samples: Vec<f32> = chunks.into_iter().flat_map(|c| c.samples).collect();
        let rate = f64::from(audio.sample_rate) * f64::from(audio.channels);
        let rms = |from: f64, to: f64| {
            // Interleaved samples: `rate` counts every channel, so one second is `rate` samples.
            let at = |secs: f64| (secs * rate) as usize;
            let window = &samples[at(from).min(samples.len())..at(to).min(samples.len())];
            assert!(!window.is_empty(), "no samples in [{from}, {to})");
            (window.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / window.len() as f64)
                .sqrt()
        };
        let (mono_clip, stereo_clip) = (rms(0.05, 0.95), rms(1.05, 1.95));
        assert!(mono_clip > 0.01, "the mono clip is audible: {mono_clip}");
        assert!(stereo_clip > 0.01, "the stereo clip is audible: {stereo_clip}");
    }

    #[test]
    fn an_out_folder_that_cannot_take_a_file_is_refused_up_front() {
        let mut srv = burn_session();
        let r = export_start(&mut srv, &json!({"out": "/proc/nope.mp4"}));
        assert!(r.is_error && r.text.contains("cannot create files in"), "{}", r.text);
        assert!(srv.jobs.is_empty(), "a refused export starts no job");
    }

    /// The first stream's value of one ffprobe field, for a finished file.
    fn probe_field(path: &Path, section: &str, entry: &str) -> String {
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", &format!("{section}={entry}"), "-of", "csv=p=0"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn chapters_are_written_counted_and_left_out_on_request() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let dir = scratch("chapters");
        let mut srv = Server::new();
        let mut session = PlaybackSession::open(Path::new(&src)).expect("open the source");
        for (frame, title) in [(0, "Intro"), (10, "Middle"), (20, "End")] {
            session.add_chapter(frame, title).expect("add a chapter");
        }
        srv.session = Some(session);
        let out = dir.join("c.mp4");
        let r = export_start(&mut srv, &json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("\n3 chapters"), "{}", r.text);
        assert_eq!(r.structured.as_deref().unwrap()["chapters"], json!(3), "{}", r.text);
        finish_job(&mut srv, 1);

        let off = dir.join("off.mp4");
        let r = export_start(&mut srv, &json!({"out": off.display().to_string(), "range": {"from": 0, "to": 1}, "chapters": false}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("chapters: left out"), "{}", r.text);
        assert_eq!(r.structured.as_deref().unwrap()["chapters"], json!(0), "{}", r.text);
        finish_job(&mut srv, 2);

        let wav = dir.join("tone.wav");
        let r = export_start(&mut srv, &json!({"out": wav.display().to_string(), "range": {"from": 0, "to": 1}}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("chapters are not written"), "{}", r.text);
    }

    #[test]
    fn chapters_markers_writes_the_comment_markers_as_the_chapter_list() {
        use engine::project::{Marker, MarkerColor};
        engine::ao::set_silent_output();
        let src = small_clip();
        let dir = scratch("marker-chapters");
        let mut srv = Server::new();
        let mut session = PlaybackSession::open(Path::new(&src)).expect("open the source");
        session.add_chapter(0, "Only chapter").expect("add a chapter");
        for (frame, name) in [(0, "Intro"), (0, "Same frame"), (10, "Middle")] {
            let m = Marker { frame, length: 0, name: name.into(), note: String::new(), color: MarkerColor::Red };
            session.add_marker(m).expect("add a marker");
        }
        // Two markers on one frame are one chapter: the first of them.
        assert_eq!(session.marker_list_ns(None).iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["Intro", "Middle"]);
        srv.session = Some(session);
        let out = dir.join("m.mp4");
        let r = export_start(&mut srv, &json!({"out": out.display().to_string(), "range": {"from": 0, "to": 1}, "chapters": "markers"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 chapters (from the markers)"), "{}", r.text);
        finish_job(&mut srv, 1);
        let bad = export_start(&mut srv, &json!({"out": dir.join("x.mp4").display().to_string(), "chapters": "all"}));
        assert!(bad.is_error && bad.text.contains("not true, false or"), "{}", bad.text);
    }

    #[test]
    fn bit_depth_ten_is_refused_outside_av1_and_hevc_and_named_in_words() {
        engine::ao::set_silent_output();
        let src = small_clip();
        let dir = scratch("bit-depth");
        let mut srv = Server::new();
        srv.session = Some(PlaybackSession::open(Path::new(&src)).expect("open the source"));
        let r = export_start(&mut srv, &json!({"out": dir.join("h.mp4").display().to_string(), "bit_depth": 10}));
        assert!(r.is_error, "{}", r.text);
        assert!(
            r.text.contains("10-bit export is AV1 and HEVC only: format av1 (.mkv), av1-mp4, hevc (.mkv) or hevc-mp4, not mp4"),
            "{}",
            r.text
        );
        assert!(srv.jobs.is_empty(), "a refused export starts no job");

        let r = export_start(&mut srv, &json!({"out": dir.join("b.mp4").display().to_string(), "bit_depth": 9}));
        assert!(r.is_error && r.text.contains("bit_depth 9 is not 8 or 10"), "{}", r.text);

        let out = dir.join("ten.mp4");
        let r = export_start(&mut srv, &json!({
            "out": out.display().to_string(), "format": "av1-mp4", "bit_depth": 10, "range": {"from": 0, "to": 1}
        }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("output 10-bit, as asked"), "{}", r.text);
        assert_eq!(r.structured.as_deref().unwrap()["bit_depth"], json!(10), "{}", r.text);
        finish_job(&mut srv, 1);
        assert_eq!(probe_field(&out, "stream", "pix_fmt"), "yuv420p10le");

        let hevc = dir.join("ten-hevc.mp4");
        let r = export_start(&mut srv, &json!({
            "out": hevc.display().to_string(), "format": "hevc-mp4", "bit_depth": 10, "seat": "software", "range": {"from": 0, "to": 1}
        }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("output 10-bit, as asked"), "{}", r.text);
        assert_eq!(r.structured.as_deref().unwrap()["bit_depth"], json!(10), "{}", r.text);
        finish_job(&mut srv, 2);
        assert_eq!(probe_field(&hevc, "stream", "pix_fmt"), "yuv420p10le");
        assert_eq!(probe_field(&hevc, "stream", "profile"), "Main 10");

        let r = export_start(&mut srv, &json!({"out": dir.join("h264.mp4").display().to_string(), "format": "mp4", "bit_depth": 10}));
        assert!(r.is_error && r.text.contains("AV1 and HEVC only"), "{}", r.text);
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    /// Sizes are decimal (MB = 10^6 bytes), the unit media_probe prints, so one file reads the same
    /// in both replies: 12,139,083 bytes is 12.1 MB, not 11.6 MiB.
    #[test]
    fn export_sizes_use_decimal_units() {
        assert_eq!(human_bytes(12_139_083), "12.1 MB");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(1_000_000), "1.0 MB");
    }

}
