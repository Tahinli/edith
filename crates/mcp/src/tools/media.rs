//! Media tools: probe, import, list.
//!
//! `media_probe` reads headers only and never touches the open project.
//! `media_import` adds sources to the open project; it places nothing.
use std::path::{Path, PathBuf};

use engine::{probe_bitrate, probe_media, MediaFacts};
use serde_json::{json, Value};

use crate::parse;
use crate::server::{opt_str, tri, Entry, Reply, Server, Tool};
use crate::tools::project::expand_home;

/// Most files one probe call reads.
const CAP: usize = 200;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "media_relink",
            description: "Point an offline source (its file was moved or deleted) at a new file. The new file must be the same kind (picture, sound or still) and long enough for the clips on it; a length that differs from the old file by more than a frame is a warning. Clips are unchanged. The project is unsaved until project_save.",
            schema: || {
                schema(
                    json!({
                        "source": {"type": "string", "description": "The source to relink, as media_list names it, e.g. src:3."},
                        "path": {"type": "string", "description": "The file the source now is."}
                    }),
                    &["source", "path"],
                )
            },
            run: relink,
        },
        Tool {
            name: "media_probe",
            description: "Read a media file's header without importing it: container, codec, size, frame rate, duration and bitrate, then each audio and subtitle track with its language, channel layout (mono, stereo, 5.1, 7.1) and sample rate. A track of more than two channels is noted as mixed to stereo on export. Give paths, a glob, or both. Up to 200 files per call.",
            schema: || {
                schema(
                    json!({
                        "paths": {
                            "type": ["array", "string"],
                            "items": {"type": "string"},
                            "description": "File paths to probe: a list, or one path as a string. A path whose last component has * ? or [ is expanded as a glob."
                        },
                        "glob": {
                            "type": "string",
                            "description": "A glob such as ~/clips/*.mp4. Wildcards * and ? apply to the last path component only."
                        }
                    }),
                    &[],
                )
            },
            run: probe,
        },
        Tool {
            name: "media_import",
            description: "Add media files to the open project as sources (src:<n>). Places nothing on the timeline. Importing a file twice registers it once.",
            schema: || {
                schema(
                    json!({
                        "paths": {
                            "type": ["array", "string"],
                            "items": {"type": "string"},
                            "description": "Media file paths to import into the open project: a list, or one path as a string. A path whose last component has * ? or [ is expanded as a glob."
                        }
                    }),
                    &["paths"],
                )
            },
            run: import,
        },
        Tool {
            name: "media_remove",
            description: "Drop an unused source (src:<n>) from the open project. Refused, naming the clips, while any clip plays from it. The undo history is kept when the source is the last one; any other source renumbers the sources after it, which clears the undo history, so that is refused unless force is true. The project is left unsaved until project_save.",
            schema: || {
                schema(
                    json!({
                        "source": {
                            "type": "string",
                            "description": "The source to drop, as media_list names it: src:<n>."
                        },
                        "force": {
                            "type": "boolean",
                            "description": "Remove it even though that clears the undo history (any source but the last renumbers the ones after it)."
                        }
                    }),
                    &["source"],
                )
            },
            run: remove,
        },
        Tool {
            name: "media_bin",
            description: "Organise the media library into bins (named folders, one level of nesting) and star favourites. action \"create\" {name, parent?} makes a bin; \"rename\" {bin, name}; \"remove\" {bin} deletes a bin and keeps what it held (its sources go up one level, bins inside it come out to the top); \"move\" {sources, bin?} files sources in a bin, and no bin takes them out of every bin; \"favourite\" {sources, on?=true} stars or unstars sources. A bin is written bin:<n> (as media_list shows it) or by its name; Parent/Child names a bin inside another. Sources are src:<n>, a list of them, or \"all\". Each call is one undo step; the project is unsaved until project_save.",
            schema: || {
                schema(
                    json!({
                        "action": {"type": "string", "enum": ["create", "rename", "remove", "move", "favourite"], "description": "What to do."},
                        "name": {"type": "string", "description": "create: the new bin's name. rename: its new name."},
                        "parent": {"type": ["string", "number"], "description": "create: the bin to make it inside (bin:<n> or a name). Omit for the top level."},
                        "bin": {"type": ["string", "number"], "description": "rename, remove, move: the bin, as bin:<n> or by name. move without it takes the sources out of their bin."},
                        "sources": {"type": ["array", "string", "number"], "items": {"type": ["string", "number"]}, "description": "move, favourite: src:<n>, a list of them, or \"all\"."},
                        "on": {"type": "boolean", "description": "favourite: true stars (default), false unstars."}
                    }),
                    &["action"],
                )
            },
            run: bin,
        },
        Tool {
            name: "media_list",
            description: "List the open project's sources: src:<n>, name, duration, and video size, codec and frame rate, then the bin each is filed in and a star if it is a favourite. Bins are listed first as bin:<n>. kind keeps only one kind of source: video (a picture with motion), audio (sound only) or image (a still); offline sources are kept only when no kind is asked for.",
            schema: || schema(json!({"kind": {"type": "string", "enum": ["video", "audio", "image"], "description": "Only sources of this kind: video, audio or image (a still). Omitted: every source."}}), &[]),
            run: list,
        },
    ]
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

// ---- gathering paths -------------------------------------------------------

/// Expands one argument: a glob if its last component has a wildcard, else the path itself.
fn expand_glob(raw: &str) -> Result<Vec<PathBuf>, String> {
    let path = expand_home(raw);
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.contains(['*', '?', '[']) => n.to_string(),
        _ => return Ok(vec![path]),
    };
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut hits: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| (!n.starts_with('.') || name.starts_with('.')) && wild(&name, n))
        })
        .collect();
    hits.sort();
    Ok(hits)
}

/// Matches `name` against `pat`, where `*` is any run, `?` one character, and
/// `[abc]`, `[a-z]` or `[!abc]` one character from (or outside) a set. A `[` with no
/// closing `]` is an ordinary character.
fn wild(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<usize> = None;
    let mut mark = 0;
    while ni < n.len() {
        match one(&p, pi, n[ni]) {
            Some(next) => {
                pi = next;
                ni += 1;
            }
            None if pi < p.len() && p[pi] == '*' => {
                star = Some(pi);
                mark = ni;
                pi += 1;
            }
            None => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ni = mark;
                }
                None => return false,
            },
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The pattern index just past the one-character item at `pi`, when that item matches `c`.
fn one(p: &[char], pi: usize, c: char) -> Option<usize> {
    match *p.get(pi)? {
        '?' => Some(pi + 1),
        '[' => match p[pi + 1..].iter().position(|&x| x == ']') {
            Some(off) => set_matches(&p[pi + 1..pi + 1 + off], c).then_some(pi + off + 2),
            None => (c == '[').then_some(pi + 1),
        },
        item if item == c => Some(pi + 1),
        _ => None,
    }
}

/// Whether `c` is in a bracket set's body (the text between `[` and `]`): listed
/// characters, `a-z` ranges, and a leading `!` for "none of these".
fn set_matches(body: &[char], c: char) -> bool {
    let (negate, body) = match body.first() {
        Some('!') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == '-' {
            hit |= body[i] <= c && c <= body[i + 2];
            i += 3;
        } else {
            hit |= body[i] == c;
            i += 1;
        }
    }
    hit != negate
}

/// The files named by `paths` and `glob` arguments, in order, without repeats.
/// `glob_arg` says whether the tool takes a `glob` argument of its own, which only media_probe does;
/// the wording of its refusals names it, and media_import's names the pattern inside `paths`.
fn gather(args: &Value, glob_arg: bool) -> Result<Vec<PathBuf>, Reply> {
    gather_listed(args, glob_arg).map(|(files, _)| files)
}

/// The files `paths` and `glob` name, each once, and the repeats: the later entries that
/// named a file already in the list (so an import can say so).
fn gather_listed(args: &Value, glob_arg: bool) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Reply> {
    let mut raw: Vec<String> = Vec::new();
    // Set when `paths` was given but names nothing: an empty list, or a blank string.
    let mut empty = false;
    match args.get("paths") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s.trim().is_empty() => empty = true,
        Some(Value::String(s)) => raw.push(s.clone()),
        Some(Value::Array(items)) => {
            empty = items.is_empty();
            for item in items {
                match item.as_str() {
                    Some(s) => raw.push(s.to_string()),
                    None => return Err(Reply::err("every entry of `paths` must be a string")),
                }
            }
        }
        Some(_) => return Err(Reply::err("argument `paths` must be a file path, or a list of file paths")),
    }
    if let Some(g) = opt_str(args, "glob") {
        raw.push(g.to_string());
    }
    if raw.is_empty() {
        let pattern = "~/clips/*.mp4";
        if empty {
            return Err(Reply::err(match glob_arg {
                true => format!("the list `paths` is empty: give at least one file path, or a `glob` such as {pattern}"),
                false => format!("the list `paths` is empty: give at least one file path, or a pattern such as {pattern} inside it"),
            }));
        }
        return Err(Reply::err(match glob_arg {
            true => format!("give `paths` or `glob`: a list of file paths, or a pattern such as {pattern}"),
            false => format!("give `paths`: a list of file paths, or a pattern such as {pattern}"),
        }));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let mut repeats: Vec<PathBuf> = Vec::new();
    for r in &raw {
        let found = expand_glob(r).map_err(Reply::err)?;
        for p in found {
            if out.contains(&p) {
                repeats.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok((out, repeats))
}

// ---- formatting ------------------------------------------------------------

/// `29.97`, `30`, `23.976`: a frame rate with no trailing zeros.
fn fmt_fps(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `00:15.000` from seconds. Milliseconds are the frame count at a rate of 1000.
fn clock_secs(secs: f64) -> String {
    parse::clock((secs * 1000.0).round() as u32, 1000.0)
}

/// A file's playing time for a row: `00:05.000` when its header states one, `still`
/// for a picture (which has no playing time to state), else `duration unknown`.
///
/// Read from the header and never from frame counts: a source's frames are counted at
/// the timeline's rate, so dividing them by either rate is how a 5 s clip came out 4.167 s.
fn duration_label(path: &Path, secs: Option<f64>) -> String {
    match secs {
        Some(s) => clock_secs(s),
        None if engine::is_image(path) => "still".to_string(),
        None => "duration unknown".to_string(),
    }
}

/// The audio stream's own length, when it is not the picture's: a 2 s clip with a
/// 60 s commentary plays 2 s of picture and 60 s of sound. Only a picture file
/// can differ, so a song or a still never asks.
fn audio_only_secs(path: &Path, facts: &MediaFacts) -> Option<f64> {
    facts.video?;
    let picture = facts.duration_secs?;
    let audio = engine::AudioSession::duration_secs(path).ok().flatten()?;
    ((audio - picture).abs() > 0.01).then_some(audio)
}

/// A row's playing time, in the clock form every row uses: the picture's length as the
/// timeline counts it, and the audio's own length after a semicolon when it runs on past it (or stops short),
/// in seconds so the two lengths never nest in brackets: `2.000 s; audio 2.027 s`.
fn duration_text(path: &Path, facts: &MediaFacts) -> String {
    match (facts.duration_secs, audio_only_secs(path, facts)) {
        (Some(picture), Some(audio)) => format!("{picture:.3} s; audio {audio:.3} s"),
        _ => duration_label(path, facts.duration_secs),
    }
}

fn bitrate_text(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.2} Mb/s", bps as f64 / 1e6)
    } else {
        format!("{:.0} kb/s", bps as f64 / 1e3)
    }
}

/// The message for a path that is not there, the same in every tool that takes paths.
pub fn not_found(path: &Path) -> String {
    format!("not found: {}", path.display())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Why `path` cannot be read as a file, or `None` when it can: a folder is named as
/// one, and so is anything else that is not a regular file (a device, a pipe, a socket),
/// which is never opened.
pub fn not_a_file(path: &Path) -> Option<String> {
    if path.is_file() {
        None
    } else if path.is_dir() {
        Some(format!("is a directory, not a file: {}", path.display()))
    } else {
        Some(format!("is not a regular file: {}", path.display()))
    }
}

// ---- media_probe -----------------------------------------------------------

/// One probed file. `error` is set when the file cannot be read as media.
struct Row {
    path: PathBuf,
    facts: Option<MediaFacts>,
    bitrate_bps: Option<u64>,
    /// The sound and subtitle streams beside the picture, in container order.
    streams: Vec<Stream>,
    /// Why the file is refused. A refused picture keeps its facts and streams beside it.
    error: Option<String>,
    /// What the file's sound will do that the person should know: a codec nothing decodes.
    warnings: Vec<String>,
    /// The picture as stored, when the picture was read: its coded size and pixel aspect.
    coded: Option<Coded>,
    /// A still's size as shown (turn applied), when the file is a still that reads.
    still: Option<(u32, u32)>,
}

/// A picture as its header stores it, before the display stretch: the coded size and the
/// sample aspect ratio (`sar`, `(1, 1)` for square pixels), and the turn the header's display
/// matrix asks for. Also what the picture's samples mean: the colour the import path resolves
/// and the chroma layout, where the engine states one.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Coded {
    w: u32,
    h: u32,
    sar: (u32, u32),
    turn: engine::demux::Rotation,
    colour: engine::colorspace::ColorDescription,
    /// The chroma layout the bitstream states (gray, 4:2:0, 4:2:2, 4:4:4); `None` where the
    /// engine reads none.
    chroma: Option<engine::colorspace::Chroma>,
}

impl Coded {
    /// `(stored 1280x640, turned 90° counter-clockwise to stand upright; ffprobe rotation 90)` for a
    /// picture the header turns, `None` for an upright one. The turn is what edith applies to show the
    /// picture upright; ffprobe's `rotation` is the same turn with the opposite sign, as its tag reads.
    fn turn_text(&self) -> Option<String> {
        let (turn, ffprobe) = match self.turn {
            engine::demux::Rotation::None => return None,
            engine::demux::Rotation::Cw90 => ("90° clockwise", "-90"),
            engine::demux::Rotation::Cw180 => ("180°", "180"),
            engine::demux::Rotation::Cw270 => ("90° counter-clockwise", "90"),
        };
        Some(format!("(stored {}x{}, turned {turn} to stand upright; ffprobe rotation {ffprobe})", self.w, self.h))
    }

    /// `1440x1080 (shown 1920x1080, pixel aspect 4:3)` for a picture whose pixels are not square,
    /// `None` for square pixels, which show the size alone.
    fn text(&self, shown: (u32, u32)) -> Option<String> {
        let (h, v) = self.sar;
        if v == 0 || h == v {
            return None;
        }
        let g = gcd(h, v).max(1);
        Some(format!(
            "{}x{} (shown {}x{}, pixel aspect {}:{})",
            self.w,
            self.h,
            shown.0,
            shown.1,
            h / g,
            v / g
        ))
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The picture's coded size and pixel aspect, read from its header (`Demuxer::open` reads the
/// header only). `None` when the file will not open as a picture.
fn coded_of(path: &Path) -> Option<Coded> {
    let (meta, demuxer) = engine::demux::Demuxer::open(path).ok()?;
    let chroma = demuxer.chroma();
    Some(Coded {
        w: meta.coded_width,
        h: meta.coded_height,
        sar: (meta.pixel_aspect.h, meta.pixel_aspect.v),
        turn: meta.rotation,
        colour: meta.color,
        chroma,
    })
}

/// The colour a picture's samples are on, in the words a person knows it by: the matrix, then
/// the curve where it is not SDR (`BT.2020 PQ`), then the range where it is full. An HDR curve
/// says what export does with it, since the file written is SDR. A matrix the file does not tag
/// (`assumed`, the size rule's guess) says so: `BT.709 (assumed, untagged)`.
fn colour_text(c: engine::colorspace::ColorDescription, assumed: bool) -> String {
    use engine::colorspace::{Matrix, Transfer};
    let matrix = match c.matrix {
        Matrix::Bt601 => "BT.601",
        Matrix::Bt709 => "BT.709",
        Matrix::Bt2020Ncl => "BT.2020",
    };
    let matrix = if assumed { format!("{matrix} (assumed, untagged)") } else { matrix.to_string() };
    let mut text = match c.transfer {
        Transfer::Sdr => matrix,
        Transfer::Pq => format!("{matrix} PQ"),
        Transfer::Hlg => format!("{matrix} HLG"),
    };
    if c.full_range {
        text.push_str(" full range");
    }
    if c.transfer != Transfer::Sdr {
        text.push_str(" (HDR: exports tone-mapped to 8-bit SDR)");
    }
    text
}

/// The picture's bit depth, chroma layout and colour, each only where the engine states it,
/// joined as one line: `10-bit 4:2:0 · BT.2020 PQ (HDR: ...)`. `None` when none is known.
fn picture_info(bit_depth: Option<u8>, matrix_assumed: bool, coded: Option<&Coded>) -> Option<String> {
    // The depth and the chroma are one part (`10-bit 4:2:0`); the colour is the other.
    let mut layout = Vec::new();
    if let Some(d) = bit_depth {
        layout.push(format!("{d}-bit"));
    }
    layout.extend(coded.and_then(|c| c.chroma).map(|c| c.to_string()));
    let mut parts = Vec::new();
    if !layout.is_empty() {
        parts.push(layout.join(" "));
    }
    if let Some(c) = coded {
        parts.push(colour_text(c.colour, matrix_assumed));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The refusal for a `.edith` project given where media goes: it is the editor's own file, not
/// a container, so the probe and the import both say how to open it instead of calling it
/// truncated. The probe says it as a plain line, not a refusal.
const EDITH_PROJECT: &str = "an edith project (open it with project_open)";

fn is_edith_project(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("edith"))
}

/// A subtitle file (`.srt`, `.vtt`, `.ass`, `.ssa`): text, not media, and imported with subtitles_import.
fn is_subtitle_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| ["srt", "vtt", "ass", "ssa"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The sentence a subtitle file gets from the probe and the import: not a refusal, and not truncated media.
fn subtitle_file_text(path: &Path) -> String {
    format!("{} is a subtitle file: import it with subtitles_import", file_name(path))
}

/// The plain words for a file the engine cannot read as media. An empty file says so. A file whose
/// boxes do not add up (a truncated copy, or a text file given an .mp4 name) may be truncated or not
/// an MP4 at all: the engine's detail is kept, and the next step is named. `None` for any other
/// refusal, which keeps its own words.
fn plain_refusal(path: &Path, engine_msg: &str) -> Option<String> {
    if is_subtitle_file(path) {
        return Some(subtitle_file_text(path));
    }
    let name = file_name(path);
    if std::fs::metadata(path).is_ok_and(|m| m.len() == 0) {
        return Some(format!("{name} is empty (0 bytes)"));
    }
    if engine_msg.contains("larger size") || engine_msg.contains("ftyp not found") {
        return Some(format!(
            "{name} looks truncated or is not a valid MP4 ({engine_msg}); re-copy or re-export the file from its source"
        ));
    }
    None
}

/// Whether two paths name the same file: by their canonical form where both resolve, else as written.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// One stream of a file that is not its picture, as media_probe lists it.
#[derive(Debug, Clone, PartialEq)]
struct Stream {
    /// `audio` or `subtitle`.
    kind: &'static str,
    /// Position among all the file's streams, video included, as ffprobe numbers them (0-based).
    index: usize,
    /// The container's track number (Matroska `TrackNumber`, mp4 track id); `None` when the
    /// engine's reader does not give one.
    track: Option<u64>,
    /// The codec in plain words (`aac`, `opus`, `srt`, `mov_text`); `None` when the file says none.
    codec: Option<String>,
    /// The codec as the file spells it (a Matroska CodecID or an mp4 sample-entry fourcc).
    codec_id: Option<String>,
    /// Audio AAC: the profile the engine reads, spelled as ffprobe does (`LC`, `HE-AAC`, `HE-AACv2`);
    /// `None` for other codecs and where the engine states none.
    profile: Option<String>,
    /// Audio lossless (FLAC, ALAC, PCM): the sample depth the header states, in bits; `None` otherwise.
    bit_depth: Option<u32>,
    /// ISO 639-2, `und` when the file states none.
    language: String,
    /// Audio: the channel count the header states; `None` where it states none, and for subtitles.
    channels: Option<u16>,
    /// Audio: the sample rate in Hz the header states; `None` where it states none.
    sample_rate: Option<u32>,
    /// The track's title (`Signs`), empty when it has none.
    name: String,
}

/// The codec in plain words, from a Matroska CodecID or an mp4 sample-entry fourcc. A spelling
/// not listed here is returned as the file gives it.
fn plain_codec(raw: &str) -> String {
    let name = match raw {
        "A_OPUS" | "Opus" => "opus",
        "A_VORBIS" => "vorbis",
        "A_FLAC" | "fLaC" => "flac",
        "mp4a" => "aac",
        "A_ALAC" | "alac" => "alac",
        "A_AC3" | "ac-3" => "ac3",
        "A_EAC3" | "ec-3" => "eac3",
        "A_DTS" | "dtsc" | "dtsh" | "dtse" => "dts",
        "A_TRUEHD" | "mlpa" => "truehd",
        "A_MPEG/L3" | ".mp3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "lpcm" | "sowt" | "twos" | "in24" | "in32" | "fl32" | "fl64" => "pcm",
        "V_VP8" | "vp08" => "vp8",
        "V_VP9" | "vp09" => "vp9",
        "V_AV1" | "av01" => "av1",
        "V_MPEG4/ISO/AVC" | "avc1" | "avc3" => "h264",
        "V_MPEGH/ISO/HEVC" | "hvc1" | "hev1" => "hevc",
        "apco" | "apcs" | "apcn" | "apch" | "ap4h" | "ap4x" => "prores",
        "AVdn" => "dnxhd",
        "AVdh" => "dnxhr",
        "V_MJPEG" | "jpeg" | "mjpa" => "mjpeg",
        "dvc " | "dvcp" | "dvpp" | "dvcs" | "dvis" | "dv5n" | "dv5p" | "dv5q" | "dvhq" | "dvh1" | "dvh2"
        | "dvh3" | "dvh4" | "dvh5" | "dvh6" | "dvhp" | "dvhs" | "dvhn" | "dvhr" => "dv",
        "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/AP" | "mp4v" => "mpeg4",
        "s263" | "h263" => "h263",
        "S_TEXT/UTF8" => "srt",
        "S_TEXT/ASS" | "S_ASS" => "ass",
        "S_TEXT/SSA" | "S_SSA" => "ssa",
        "S_TEXT/WEBVTT" | "wvtt" => "webvtt",
        "tx3g" => "mov_text",
        "stpp" => "ttml",
        "S_VOBSUB" => "vobsub",
        "S_HDMV/PGS" | "hdmv_pgs_subtitle" => "pgs",
        other if other.starts_with("A_AAC") => "aac",
        other if other.starts_with("A_PCM") => "pcm",
        other => return other.to_string(),
    };
    name.to_string()
}

/// A stream's language as a person reads it: the tag the header states, else the `fallback`, and
/// `und` when neither says anything. A blank tag is no language, and neither is a tag of
/// unprintable bytes: test_qt.mov's mdhd packs its unset language as 0x7fff, which the engine
/// hands on as three DEL bytes. Both read `und` like a missing one.
fn language_of(stated: Option<&str>, fallback: &str) -> String {
    [stated, Some(fallback)]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_graphic()))
        .unwrap_or("und")
        .to_string()
}

/// The audio and subtitle streams of a file, in ffprobe's index order. The engine's stream list
/// is in file order for mp4 (its position is the index), and it names each stream's codec by
/// fourcc; the subtitle walk gives the mp4 track number, language and title, in the same order.
/// Matroska keeps its own walks, which name the language and title; its track numbers are
/// taken as the index (`TrackNumber` minus one, the usual order).
fn other_streams(path: &Path, facts: &MediaFacts) -> Vec<Stream> {
    // The engine's stream list is the source of every stream's codec and language, and of the audio
    // numbering: an audio stream's index is the audio track `audio_track` takes (0-based). A subtitle
    // track's number, title and, where the engine states none (Matroska subtitles), its language come
    // from the container walk, which numbers them.
    let mkv = engine::demux::is_matroska(path);
    let walk: Vec<(u64, String, String)> = if mkv {
        engine::demux::matroska_subtitles(path)
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.number, t.language, t.name))
            .collect()
    } else if engine::demux::is_mpeg_system(path) {
        // A transport stream numbers its PGS tracks by the engine's stream list alone.
        Vec::new()
    } else {
        engine::demux::mp4_subtitles(path)
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.number, t.language, t.name))
            .collect()
    };
    let mut subs = walk.into_iter();
    let mut out = Vec::new();
    for (index, st) in facts.streams.iter().enumerate() {
        match st.kind {
            engine::demux::StreamKind::Video => continue,
            engine::demux::StreamKind::Audio => out.push(Stream {
                kind: "audio",
                index: st.index,
                track: None,
                codec: st.codec_id.as_deref().map(plain_codec),
                codec_id: st.codec_id.clone(),
                profile: st.profile.clone(),
                bit_depth: st.bit_depth,
                language: language_of(st.language.as_deref(), ""),
                channels: st.channels,
                sample_rate: st.sample_rate,
                name: String::new(),
            }),
            engine::demux::StreamKind::Subtitle => {
                let (track, walk_language, name) = match subs.next() {
                    Some((number, language, name)) => (Some(number), language, name),
                    None => (None, "und".to_string(), String::new()),
                };
                let index = match (mkv, track) {
                    (true, Some(number)) => (number as usize).saturating_sub(1),
                    _ => index,
                };
                out.push(Stream {
                    kind: "subtitle",
                    index,
                    track,
                    codec: st.codec_id.as_deref().map(plain_codec),
                    codec_id: st.codec_id.clone(),
                    profile: None,
                    bit_depth: None,
                    language: language_of(st.language.as_deref(), &walk_language),
                    channels: None,
                    sample_rate: None,
                    name,
                });
            }
        }
    }
    // Sound the engine could not list by kind (a file with no header streams) is still named.
    if !out.iter().any(|s| s.kind == "audio") && engine::AudioSession::duration_secs(path).ok().flatten().is_some() {
        out.insert(0, Stream { kind: "audio", index: 0, track: None, codec: None, codec_id: None, profile: None, bit_depth: None, language: "und".into(), channels: None, sample_rate: None, name: String::new() });
    }
    // Closed captions ride inside the picture: listed (from the first seconds of it) beside the
    // container's own subtitle tracks, and imported with them by subtitles_import.
    if facts.streams.iter().any(|s| s.kind == engine::demux::StreamKind::Video) {
        let base = facts.streams.len();
        for (k, c) in engine::demux::captions::present(path).iter().enumerate() {
            let cc708 = c.number == engine::demux::captions::TRACK_BASE + 5;
            out.push(Stream {
                kind: "subtitle",
                index: base + k,
                track: Some(c.number),
                codec: Some(if cc708 { "eia_708" } else { "eia_608" }.to_string()),
                codec_id: Some(if cc708 { "cea708" } else { "cea608" }.to_string()),
                profile: None,
                bit_depth: None,
                language: "und".into(),
                channels: None,
                sample_rate: None,
                name: c.name.clone(),
            });
        }
    }
    out
}

/// A channel count in plain words: `mono`, `stereo`, `5.1`, `7.1`, and `N ch` for any other.
fn channels_text(n: u16) -> String {
    match n {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{n} ch"),
    }
}

/// A sample rate as `48 kHz` or `44.1 kHz`.
fn rate_text(hz: u32) -> String {
    format!("{} kHz", fmt_fps(f64::from(hz) / 1000.0))
}

/// The picture codec with its profile: `ProRes 422 HQ`, `HEVC Main 10`. Avid's profile names
/// already begin with the family (`DNxHD 145`, `DNxHR HQX`), so they stand alone.
fn codec_phrase(codec: engine::demux::Codec, profile: Option<&str>) -> String {
    match profile {
        Some(p) if codec == engine::demux::Codec::Dnxhd && p.starts_with("DNx") => p.to_string(),
        // H.263 is MPEG-4 Part 2's baseline decoder; its profile is its name.
        Some("H.263") if codec == engine::demux::Codec::Mpeg4 => "H.263".to_string(),
        _ => format!("{}{}", codec.name(), profile_text(profile)),
    }
}

/// A profile after its codec name, with the leading space; empty where the stream states none.
fn profile_text(profile: Option<&str>) -> String {
    profile.map(|p| format!(" {p}")).unwrap_or_default()
}

/// A stream as one short phrase: `audio track 1 opus (fra) 5.1, 48 kHz "Commentary"`, or `subtitle #3 srt (eng)`.
/// An audio track's number is the one `audio_track` takes (0-based); a subtitle's is the index ffprobe gives it.
/// The channel layout and rate follow the language where the header states them. A lossless track's
/// depth follows the codec: `audio track 0 flac 24-bit (und) stereo, 48 kHz`.
fn stream_text(st: &Stream) -> String {
    let codec = st.codec.as_deref().unwrap_or("codec unknown");
    let profile = profile_text(st.profile.as_deref());
    let depth = st.bit_depth.map(|d| format!(" {d}-bit")).unwrap_or_default();
    let name = if st.name.is_empty() { String::new() } else { format!(" \"{}\"", st.name) };
    let number = match st.kind {
        "audio" => format!("track {}", st.index),
        _ => format!("#{}", st.index),
    };
    let spec: Vec<String> = [st.channels.map(channels_text), st.sample_rate.map(rate_text)].into_iter().flatten().collect();
    let spec = if spec.is_empty() { String::new() } else { format!(" {}", spec.join(", ")) };
    format!("{} {number} {codec}{profile}{depth} ({}){spec}{name}", st.kind, st.language)
}

/// The note for each audio track of more than two channels: the export mixes all sound to stereo,
/// so the surround layout does not reach the file. Said where the layout is shown.
fn surround_notes(streams: &[Stream]) -> Vec<String> {
    streams
        .iter()
        .filter(|s| s.kind == "audio")
        .filter_map(|s| {
            s.channels
                .filter(|c| *c > 2)
                .map(|c| format!("note: audio track {} is {}; mixed to stereo on export", s.index, channels_text(c)))
        })
        .collect()
}

/// Why a file with no picture and no playing time is refused. The words are
/// the ones `media_import` gives for the same file (`PlaybackSession::import`
/// reads the same demuxer, then the audio header), so a probe and an import
/// never disagree about a broken file.
fn unrecognised_reason(path: &Path) -> String {
    match engine::AudioSession::duration_secs(path) {
        Err(e) => e.to_string(),
        Ok(_) => "not a recognised media file (no picture track and no playing time)".to_string(),
    }
}

/// Warnings about a file's sound that the picture does not show: a sound codec nothing here
/// decodes plays silent, and the person is told so at probe and import.
fn audio_warnings(path: &Path) -> Vec<String> {
    // The engine's own rule: a codec is named only for a file whose sound came up silent. A
    // file that opens with sound is never warned, whatever its header says.
    if matches!(engine::AudioSession::open(path), Ok(Some(_))) {
        return Vec::new();
    }
    match engine::AudioSession::unsupported(path) {
        Ok(Some(why)) => vec![match codec_named_in(&why) {
            Some(codec) => format!("warning: audio: {codec} is not supported; this file's sound will be silent"),
            None => format!("warning: audio: {why}; this file's sound will be silent"),
        }],
        _ => Vec::new(),
    }
}

/// The codec a refusal names, in the words a person knows it by. The engine's reason names it
/// as `the WavPack track of …` (already friendly, see `codec_display_name`), and a reason that
/// still carries a Matroska id names it as `A_DTS` (says `DTS`). `None` when the reason names
/// no codec at all.
fn codec_named_in(why: &str) -> Option<String> {
    let friendly = why
        .find(" track of")
        .and_then(|end| why[..end].rsplit_once("the ").map(|(_, name)| name.trim()))
        .filter(|name| !name.is_empty() && !name.starts_with("A_") && !name.contains(' '));
    if let Some(name) = friendly {
        return Some(name.to_string());
    }
    why.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '/' || c == '-' || c == '.'))
        .find_map(|t| t.strip_prefix("A_"))
        .map(|t| t.split('/').next().unwrap_or(t).to_ascii_uppercase())
        .filter(|t| !t.is_empty())
}

fn probe_one(path: &Path) -> Row {
    let fail = |msg: String| Row {
        path: path.to_path_buf(),
        facts: None,
        bitrate_bps: None,
        streams: Vec::new(),
        error: Some(msg),
        warnings: Vec::new(),
        coded: None,
        still: None,
    };
    if !path.exists() {
        return fail(not_found(path));
    }
    if let Some(why) = not_a_file(path) {
        return fail(why);
    }
    if is_edith_project(path) {
        return fail(EDITH_PROJECT.to_string());
    }
    let facts = probe_media(path);
    let streams = other_streams(path, &facts);
    let mut warnings = audio_warnings(path);
    warnings.extend(surround_notes(&streams));
    // The engine's word for a picture it refuses (`picture_refusal`). The container comes from the
    // extension, so a file that only looks like media shows up as having no picture and no playing
    // time; stills are the exception. A refused picture keeps its streams: the reason and the
    // streams are shown together.
    let refusal = engine::demux::picture_refusal(path).or_else(|| picture_refusal_at_import(path, &facts));
    if refusal.is_some() || (facts.video.is_none() && facts.duration_secs.is_none() && !engine::is_image(path)) {
        return Row {
            path: path.to_path_buf(),
            facts: Some(facts),
            bitrate_bps: None,
            streams,
            error: Some(refusal.unwrap_or_else(|| unrecognised_reason(path))),
            warnings,
            coded: None,
            still: None,
        };
    }
    let bitrate_bps = probe_bitrate(path).total;
    let coded = facts.video.as_ref().and_then(|_| coded_of(path));
    // A still's size comes from its header (`decode::image_size`, the turn applied), not a decode.
    let still = engine::is_image(path).then(|| engine::decode::image_size(path).ok()).flatten();
    Row {
        path: path.to_path_buf(),
        facts: Some(facts),
        bitrate_bps,
        streams,
        error: None,
        warnings,
        coded,
        still,
    }
}

/// The refusal import gives a picture that opens but that the decoder will not take (an HEVC
/// range-extensions stream, say), in the engine's own words: `decode::probe` runs the very check
/// import runs. Asked only of the codecs whose check it makes (HEVC, VP9, VP8), since for H.264
/// and AV1 it would start a decoder for every file probed.
fn picture_refusal_at_import(path: &Path, facts: &MediaFacts) -> Option<String> {
    let codec = facts.video.as_ref()?.codec;
    if !matches!(codec, engine::demux::Codec::Hevc | engine::demux::Codec::Vp9 | engine::demux::Codec::Vp8) {
        return None;
    }
    engine::decode::probe(path).err().map(|e| e.to_string())
}

/// A file with subtitle tracks and no picture or sound: not a refusal, since its tracks import as captions.
fn subtitles_only(row: &Row) -> bool {
    row.facts.as_ref().is_some_and(|f| f.video.is_none())
        && row.streams.iter().any(|st| st.kind == "subtitle")
        && !row.streams.iter().any(|st| st.kind == "audio")
}

fn row_line(row: &Row) -> String {
    let name = file_name(&row.path);
    let mut warn = String::new();
    for w in &row.warnings {
        warn.push_str(&format!("\n  {w}"));
    }
    if let Some(e) = &row.error {
        if is_edith_project(&row.path) {
            return format!("{name} · {EDITH_PROJECT}");
        }
        if is_subtitle_file(&row.path) {
            return subtitle_file_text(&row.path);
        }
        // A refused picture: the reason, then the streams the header still names.
        let Some(f) = row.facts.as_ref() else {
            return format!("{name} · error: {e}{warn}");
        };
        let head = plain_refusal(&row.path, e).unwrap_or_else(|| {
            if subtitles_only(row) {
                format!("{name} · subtitles only: import with subtitles_import")
            } else {
                format!("{name} · refused: {e}")
            }
        });
        let mut list: Vec<String> = f
            .streams
            .iter()
            .filter(|st| st.kind == engine::demux::StreamKind::Video)
            .map(|st| format!("video #{} {}", st.index, st.codec_id.as_deref().unwrap_or("codec unknown")))
            .collect();
        list.extend(row.streams.iter().map(stream_text));
        if list.is_empty() {
            return format!("{head}{warn}");
        }
        return format!("{head}\n  streams: {}{warn}", list.join(" · "));
    }
    let Some(f) = &row.facts else {
        return format!("{name} · error: no facts");
    };
    let container = f.container.as_deref().unwrap_or("unknown container");
    let picture = match &f.video {
        Some(v) => {
            let rate = v.frame_rate.map_or("@?".to_string(), |r| format!("@{}", fmt_fps(r)));
            // The size the picture is stored at, and the shown size when the pixels are not square.
            let size = row
                .coded
                .as_ref()
                .and_then(|c| c.text((v.width, v.height)))
                .unwrap_or_else(|| format!("{}x{}", v.width, v.height));
            // A turned picture shows its displayed size; the stored one is said beside it.
            let size = match row.coded.as_ref().and_then(Coded::turn_text) {
                Some(turn) => format!("{size} {turn}"),
                None => size,
            };
            let text = format!("{} {size} {rate}", codec_phrase(v.codec, v.profile));
            // Cover art beside a picture track is said, since it is not one of the video streams.
            if f.attached_pic {
                format!("{text} · cover art attached")
            } else {
                text
            }
        }
        None if engine::is_image(&row.path) => match row.still {
            Some((w, h)) => format!("still image {w}x{h}"),
            None => "still image".to_string(),
        },
        // A song with a cover: its picture is not a track to play or export.
        None if f.attached_pic => "cover art only (not a picture track)".to_string(),
        None => "no picture".to_string(),
    };
    let duration = duration_text(&row.path, f);
    let size = f
        .bytes
        .map_or("size unknown".to_string(), |b| format!("{:.1} MB", b as f64 / 1e6));
    let rate = row.bitrate_bps.map_or("bitrate unknown".to_string(), bitrate_text);
    let mut line = format!("{name} · {container} · {picture} · {duration} · {size} · {rate}");
    if let Some(info) = picture_info(f.video.and_then(|v| v.bit_depth), f.video.is_some_and(|v| v.matrix_assumed), row.coded.as_ref()) {
        line.push_str(&format!("\n  picture: {info}"));
    }
    if !row.streams.is_empty() {
        let list: Vec<String> = row.streams.iter().map(stream_text).collect();
        line.push_str(&format!("\n  streams: {}", list.join(" · ")));
    }
    line.push_str(&warn);
    line
}

fn row_json(row: &Row) -> Value {
    let path = row.path.display().to_string();
    // An edith project is not a failed probe: its line is plain, and it has no error.
    if let Some(e) = row.error.as_ref().filter(|_| !is_edith_project(&row.path)) {
        return json!({
            "path": path,
            "name": file_name(&row.path),
            "error": plain_refusal(&row.path, e).unwrap_or_else(|| e.clone()),
            "streams": row.streams.iter().map(stream_json).collect::<Vec<_>>(),
            "warnings": row.warnings,
        });
    }
    let f = row.facts.as_ref();
    let video = f.and_then(|f| f.video);
    json!({
        "still_width": row.still.map(|s| s.0),
        "still_height": row.still.map(|s| s.1),
        "colour": row.coded.map(|c| colour_text(c.colour, video.is_some_and(|v| v.matrix_assumed))),
        "matrix_assumed": video.map(|v| v.matrix_assumed),
        "chroma": row.coded.and_then(|c| c.chroma).map(|c| c.to_string()),
        "path": path,
        "name": file_name(&row.path),
        "container": f.and_then(|f| f.container.clone()),
        "codec": video.map(|v| v.codec.name()),
        "width": video.map(|v| v.width),
        "height": video.map(|v| v.height),
        "coded_width": row.coded.map(|c| c.w),
        "coded_height": row.coded.map(|c| c.h),
        "frame_rate": video.and_then(|v| v.frame_rate),
        "bit_depth": video.and_then(|v| v.bit_depth),
        "profile": video.and_then(|v| v.profile),
        "interlaced": video.and_then(|v| v.interlaced),
        "duration_secs": f.and_then(|f| f.duration_secs),
        "audio_duration_secs": f.and_then(|f| audio_only_secs(&row.path, f)),
        "bytes": f.and_then(|f| f.bytes),
        "bitrate_bps": row.bitrate_bps,
        "streams": row.streams.iter().map(stream_json).collect::<Vec<_>>(),
        "warnings": row.warnings,
        "error": null,
    })
}

fn stream_json(st: &Stream) -> Value {
    json!({
        "kind": st.kind,
        "index": st.index,
        "track": st.track,
        "codec": st.codec,
        "codec_id": st.codec_id,
        "profile": st.profile,
        "bit_depth": st.bit_depth,
        "language": st.language,
        "channels": st.channels,
        "sample_rate": st.sample_rate,
        "name": st.name,
    })
}

fn probe(_server: &mut Server, args: &Value) -> Reply {
    let mut files = tri!(gather(args, true));
    let total = files.len();
    let clipped = total > CAP;
    files.truncate(CAP);
    if files.is_empty() {
        return Reply::err("no files matched");
    }

    let rows: Vec<Row> = files.iter().map(|p| probe_one(p)).collect();
    let failed = rows.iter().filter(|r| r.error.is_some() && !subtitles_only(r) && !is_edith_project(&r.path)).count();
    let mut text: Vec<String> = rows.iter().map(row_line).collect();
    if clipped {
        text.push(format!(
            "showing the first {CAP} of {total} files; narrow the glob to see the rest"
        ));
    }
    let structured = json!({
        "files": rows.iter().map(row_json).collect::<Vec<_>>(),
        "total_matched": total,
        "shown": rows.len(),
        "failed": failed,
    });
    let reply = if failed == rows.len() {
        Reply::err(text.join("\n"))
    } else {
        Reply::ok(text.join("\n"))
    };
    reply.with(structured)
}

// ---- media_import ----------------------------------------------------------

/// Plain successes an import reply shows before it counts the rest: a call with thousands
/// of paths must not send thousands of lines back. Refusals and warnings are never counted.
const IMPORT_SHOWN: usize = 20;

/// One line of an import reply: a plain success (the only kind that may be folded into a
/// count) or a refusal, a not-found, or a warning (always shown in full).
enum ImportLine {
    Imported(String),
    Note(String),
}

/// The reply lines: every note in full and in order, the first `keep` successes in order,
/// and one summary line counting the successes left out.
fn cap_successes(lines: Vec<ImportLine>, keep: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut shown = 0usize;
    let mut hidden = 0usize;
    for line in lines {
        match line {
            ImportLine::Imported(s) if shown < keep => {
                shown += 1;
                out.push(s);
            }
            ImportLine::Imported(_) => hidden += 1,
            ImportLine::Note(s) => out.push(s),
        }
    }
    if hidden > 0 {
        out.push(format!("…and {hidden} more imported"));
    }
    out
}

fn import(server: &mut Server, args: &Value) -> Reply {
    let (files, repeats) = tri!(gather_listed(args, false));
    let start = tri!(server.session_ref()).history_pushes();
    let mut lines: Vec<ImportLine> = Vec::new();
    let mut imported: Vec<Value> = Vec::new();
    let mut failed = 0usize;
    {
        let s = tri!(server.session());
        for path in &files {
            let name = file_name(path);
            if !path.exists() {
                failed += 1;
                lines.push(ImportLine::Note(not_found(path)));
                continue;
            }
            if let Some(why) = not_a_file(path) {
                failed += 1;
                lines.push(ImportLine::Note(why));
                continue;
            }
            // A file already on the timeline as a whole-file source is not added twice.
            if let Some(n) = s.sources().iter().position(|src| src.audio_stream == 0 && same_file(&src.path, path)) {
                failed += 1;
                lines.push(ImportLine::Note(format!("{name} is already imported as src:{n}; nothing was added")));
                continue;
            }
            if is_edith_project(path) {
                failed += 1;
                lines.push(ImportLine::Note(format!("{name} is an edith project, not media: open it with project_open")));
                continue;
            }
            match s.import(path) {
                Ok(n) => {
                    // A numbered run is one source named for the whole run: said as a sequence, with its frames.
                    let run = engine::sequence::Run::parse(&s.sources()[n].path);
                    let (shown, dur) = match &run {
                        Some(r) => (file_name(&r.spelled()), format!("image sequence, {} frames", r.count)),
                        None => (name.clone(), duration_text(path, &probe_media(path))),
                    };
                    lines.push(ImportLine::Imported(format!("src:{n} {shown} ({dur})")));
                    lines.extend(audio_warnings(path).into_iter().map(|w| ImportLine::Note(format!("  {name}: {w}"))));
                    imported.push(json!({
                        "src": n, "name": shown, "path": path.display().to_string(), "duration": dur,
                        "sequence": run.as_ref().map(|r| r.count),
                    }));
                }
                Err(e) => {
                    failed += 1;
                    // A subtitles-only file has no audio track for the engine to open: say where it goes.
                    let msg = if subtitles_only(&probe_one(path)) {
                        format!("{name} · subtitles only: import with subtitles_import")
                    } else {
                        plain_refusal(path, &e.to_string()).unwrap_or_else(|| format!("{name}: refused: {e}"))
                    };
                    lines.push(ImportLine::Note(msg));
                }
            }
        }
    }
    // A file named twice in one call is imported once; the repeat is said, not taken for a second import.
    for path in &repeats {
        lines.push(ImportLine::Note(format!(
            "{} is listed twice: the second entry is a duplicate and nothing was added",
            file_name(path)
        )));
    }
    remember_sources(server);
    // Counted by the engine's push count, so an import at the undo cap still lands
    // on the ledger; one ledger entry for the whole call.
    if let Some(s) = server.session.as_ref() {
        let n = s.history_pushes().saturating_sub(start) as usize;
        if n > 0 {
            let label = format!("media_import {} file(s)", imported.len());
            server.ledger.push(Entry::new(n, label));
            server.history_lost_on_open = false;
            server.redo_ledger.clear();
            server.trim_ledger();
        }
    }

    let mut lines = cap_successes(lines, IMPORT_SHOWN);
    let reply = if imported.is_empty() {
        // Said outright, so a refused file is not taken for a file that went in.
        lines.push("nothing was imported".to_string());
        Reply::err(lines.join("\n"))
    } else {
        Reply::ok(lines.join("\n"))
    };
    reply.with(json!({"imported": imported, "failed": failed}))
}

// ---- media_remove ---------------------------------------------------------

/// Drops one source nothing plays. The engine refuses a source a clip uses; this
/// names the clips first, in the terms `project_status` uses, so the refusal says
/// what to remove or move. A removal is not an undo step and leaves the project
/// unsaved; it keeps the undo history when it can (the last source), and clears
/// it, with the ledger, when the sources after it have to renumber.
/// `media_relink {source, path}`: points an offline source at a file the person
/// found. The engine checks the kind and the length against the clips on it.
pub(crate) fn relink(server: &mut Server, args: &Value) -> Reply {
    let raw = match args.get("source") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Reply::err("argument `source` is required: a source as media_list names it, e.g. src:3"),
    };
    let digits = raw.strip_prefix("src:").unwrap_or(&raw);
    let Ok(idx) = digits.parse::<usize>() else {
        return Reply::err(format!("`{raw}` is not a source: write src:<n>, e.g. src:3"));
    };
    let Some(path) = opt_str(args, "path").map(|p| expand_home(p.trim())) else {
        return Reply::err("argument `path` is required: the file the source now is");
    };
    // The noted shapes are read before the session is borrowed mutably below.
    let known = server.source_shape.clone();
    let s = tri!(server.session());
    let Some(old) = s.sources().get(idx).map(|src| src.path.clone()) else {
        return Reply::err(format!("there is no src:{idx}; media_list shows the sources"));
    };
    // Read before the relink: what the file it replaces was, against the one it becomes. An
    // offline file cannot be read, so its shape is the one noted when the project last saw it.
    let old_shape = if old.is_file() { Some(shape_of(&probe_media(&old))) } else { known.get(&old).copied() };
    let new_shape = shape_of(&probe_media(&path));
    let shift = relink_shift_warnings(s, idx, old_shape, new_shape);
    let before = s.history_pushes();
    let relinked = s.relink_source(idx, &path);
    let steps = s.history_pushes().saturating_sub(before);
    match relinked {
        Ok(warnings) => {
            // A relink is an unsaved change: the saved source count stops matching, which is
            // what makes the project read as dirty.
            server.saved_sources = None;
            server.source_shape.insert(path.clone(), new_shape);
            // One ledger entry, as an import is: undo puts the old file back and redo points it at
            // the new one. A relink to the file the source already plays pushes nothing.
            if steps > 0 {
                let label = format!("media_relink src:{idx} to {}", file_name(&path));
                crate::tools::project::note_pushes_as(server, before, &label);
            }
            let mut lines = vec![format!("src:{idx} relinked: {} -> {}", old.display(), path.display())];
            lines.extend(shift.into_iter().map(|w| format!("warning: {w}")));
            lines.extend(warnings.into_iter().map(|w| format!("warning: {w}")));
            if steps > 0 {
                lines.push("undo takes it back".to_string());
            }
            lines.push("unsaved until project_save".to_string());
            Reply::ok(lines.join("\n"))
        }
        Err(e) => {
            let why = plain_refusal(&path, &e.to_string()).unwrap_or_else(|| e.to_string());
            Reply::err(format!("src:{idx} not relinked: {why}"))
        }
    }
}

/// A source file's frame rate and playing length: the two figures a relink compares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub fps: Option<f64>,
    pub secs: Option<f64>,
}

fn shape_of(facts: &MediaFacts) -> Shape {
    Shape {
        fps: facts.video.as_ref().and_then(|v| v.frame_rate),
        secs: facts.duration_secs,
    }
}

/// Notes the shape of every source whose file is on disk and not yet noted. Called wherever the
/// open project's sources change (import, open, relink), so an offline source still has the shape
/// its file had when the project last saw it.
pub(crate) fn remember_sources(server: &mut Server) {
    let Some(s) = server.session.as_ref() else {
        return;
    };
    let fresh: Vec<PathBuf> = s
        .sources()
        .iter()
        .map(|src| src.path.clone())
        .filter(|p| p.is_file() && !server.source_shape.contains_key(p))
        .collect();
    for p in fresh {
        let shape = shape_of(&probe_media(&p));
        server.source_shape.insert(p, shape);
    }
}

/// What relinking source `idx` from a file of `old` shape to one of `new` does to the clips on it.
/// The engine checks only the frame count, which a 25 fps clip and a 23.976 fps file of the same
/// length pass, so the rate and the length are compared here and the clips named. `old` is `None`
/// when the file it replaces was never read: then nothing is compared, and the reply says so.
fn relink_shift_warnings(s: &engine::PlaybackSession, idx: usize, old: Option<Shape>, new: Shape) -> Vec<String> {
    let Some(old) = old else {
        return vec![format!(
            "the file src:{idx} replaces is offline and its frame rate and length were never read, so the new file was not compared with it"
        )];
    };
    let fps = parse::session_fps(s);
    let mut users: Vec<String> = Vec::new();
    for lane in s.lanes() {
        for c in s.lane_clips(lane).iter().filter(|c| c.source == idx) {
            users.push(parse::addr(lane, c.start));
        }
    }
    let names = match users.len() {
        0 => "no clip is on it".to_string(),
        n if n > 6 => format!("{} and {} more", users[..6].join(", "), n - 6),
        _ => users.join(", "),
    };
    shape_warnings(idx, &names, old, new, fps)
}

/// The rate and length warnings of a relink, given the clips already named (`names`). Pure, so the
/// comparison is tested without a session. Longer and shorter are both a length difference.
fn shape_warnings(idx: usize, names: &str, old: Shape, new: Shape, fps: f64) -> Vec<String> {
    let mut out = Vec::new();
    if let (Some(was), Some(now)) = (old.fps, new.fps)
        && (was - now).abs() > 0.001
    {
        out.push(format!(
            "{} fps against the {} fps file it replaces: src:{idx} plays at another rate, so its clips ({names}) may shift",
            fmt_fps(now),
            fmt_fps(was)
        ));
    }
    if let (Some(was), Some(now)) = (old.secs, new.secs)
        && (was - now).abs() > 1.0 / fps
    {
        out.push(format!(
            "{} long against the {} it replaces (more than a frame): the clips on src:{idx} ({names}) may shift or run past its end",
            clock_secs(now),
            clock_secs(was)
        ));
    }
    out
}

fn remove(server: &mut Server, args: &Value) -> Reply {
    let raw = match args.get("source") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Reply::err("argument `source` is required: a source as media_list names it, e.g. src:3"),
    };
    let digits = raw.strip_prefix("src:").unwrap_or(&raw);
    let Ok(idx) = digits.parse::<usize>() else {
        return Reply::err(format!("`{raw}` is not a source: write src:<n>, e.g. src:3"));
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let s = tri!(server.session());
    let Some(src) = s.sources().get(idx).cloned() else {
        return Reply::err(format!("there is no src:{idx}; media_list shows the sources"));
    };
    let mut users: Vec<String> = Vec::new();
    for lane in s.lanes() {
        for c in s.lane_clips(lane).iter().filter(|c| c.source == idx) {
            users.push(parse::addr(lane, c.start));
        }
    }
    if !users.is_empty() {
        return Reply::err(format!(
            "src:{idx} {} still plays in {} clip(s): {}. Remove or move those clips first.",
            file_name(&src.path),
            users.len(),
            users.join(", ")
        ));
    }
    let name = file_name(&src.path);
    let last = s.sources().len() - 1;
    // The sources after it, named before the removal, so the reply can say which numbers moved.
    let shifted: Vec<(usize, String)> =
        s.sources().iter().enumerate().skip(idx + 1).map(|(j, o)| (j, file_name(&o.path))).collect();
    let renumber = renumber_note(&shifted);
    // The last source, with no clip and no undo or redo step on it, goes with its history kept
    // (`remove_unused_source`, which changes nothing when it refuses). Any other source renumbers
    // the ones after it, which only `remove_source` does, and that retires the history; so does the
    // last one when an undo or redo step still names it. That clears undo, so it is refused unless
    // `force` is set, and the reply says why.
    let kept = s.remove_unused_source(&src.path, src.audio_stream).is_ok();
    // A removal that renumbers with no undo or redo step to lose keeps nothing it needs to, so it
    // goes ahead without `force` and the reply says there was nothing to clear.
    let cleared = !kept && (s.undo_depth() > 0 || s.redo_depth() > 0);
    if !kept {
        if cleared && !force {
            let why = if idx == last {
                format!("src:{idx} {name} is named by an undo or redo step, so removing it clears the undo history")
            } else {
                format!(
                    "removing src:{idx} renumbers src:{}.. , so the undo history is cleared; leave src:{idx} in place to keep undo",
                    idx + 1
                )
            };
            return Reply::err(format!("src:{idx} {name} not removed: {why}. Pass force:true to remove it and clear the undo history."));
        }
        if let Err(e) = s.remove_source(&src.path, src.audio_stream) {
            return Reply::err(format!("could not remove src:{idx} {name}: {e}"));
        }
        // The engine retired its undo history with the source; the ledger goes with it.
        server.history_lost_on_open = false;
        server.ledger.clear();
        server.redo_ledger.clear();
    }
    // Not an undo step, so the depth may not move: force the unsaved state.
    server.saved_depth = None;
    let text = match (kept, cleared, idx == last) {
        (true, _, _) => format!("removed src:{idx} {name}; {renumber}; undo history is kept. project_save to keep the change."),
        (false, true, true) => format!("removed src:{idx} {name}; its undo and redo steps were cleared with it. project_save to keep the change."),
        (false, true, false) => format!(
            "removed src:{idx} {name}; {renumber}. Undo history is cleared because the renumbered sources cannot keep their steps; project_save to keep the change."
        ),
        (false, false, _) => format!(
            "removed src:{idx} {name}; {renumber}. There was no undo history to clear; project_save to keep the change."
        ),
    };
    Reply::ok(text).with(json!({"removed": {"src": idx, "name": name, "path": src.path.display().to_string(), "undo_kept": kept}}))
}

/// What a removal says about the sources after it: each one with its old and new number, the
/// first three shown and a count for the rest. With none after it, nothing was renumbered.
fn renumber_note(shifted: &[(usize, String)]) -> String {
    if shifted.is_empty() {
        return "no other source was renumbered".to_string();
    }
    let shown: Vec<String> = shifted
        .iter()
        .take(3)
        .map(|(j, name)| format!("src:{j} {name} is now src:{}", j - 1))
        .collect();
    let more = shifted.len().saturating_sub(3);
    let rest = if more > 0 { format!(", and {more} more") } else { String::new() };
    format!("{}{rest} (media_list shows the numbers)", shown.join(", "))
}

// ---- media_list ------------------------------------------------------------

/// ` · audio 1 (fra)` for a source placed on a sound track other than the first, so two sources of one
/// file read apart. Empty for track 0, which a source is unless it was placed with `audio_track`.
fn audio_choice_text(facts: &MediaFacts, audio_stream: usize) -> String {
    if audio_stream == 0 {
        return String::new();
    }
    let language = facts
        .streams
        .iter()
        .find(|st| st.kind == engine::demux::StreamKind::Audio && st.index == audio_stream)
        .map(|st| language_of(st.language.as_deref(), ""))
        .unwrap_or_else(|| "und".to_string());
    format!(" · audio {audio_stream} ({language})")
}

/// The bin `id` as a path: `Parent/Child` for a nested one.
fn bin_path(bins: &[engine::Bin], id: u32) -> String {
    let Some(b) = bins.iter().find(|b| b.id == id) else {
        return String::new();
    };
    match b.parent {
        0 => b.name.clone(),
        p => format!("{}/{}", bin_path(bins, p), b.name),
    }
}

/// A bin as the person wrote it: `bin:<n>`, a bare number, a name, or `Parent/Child`.
fn resolve_bin(bins: &[engine::Bin], raw: &Value) -> Result<u32, String> {
    let text = match raw {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return Err("a bin is bin:<n> or its name".into()),
    };
    let digits = text.strip_prefix("bin:").unwrap_or(&text);
    if let Ok(id) = digits.parse::<u32>() {
        return match bins.iter().any(|b| b.id == id) {
            true => Ok(id),
            false => Err(format!("there is no bin:{id}; media_list shows the bins")),
        };
    }
    let hits: Vec<u32> = bins.iter().filter(|b| bin_path(bins, b.id).eq_ignore_ascii_case(&text) || (b.parent == 0 || !text.contains('/')) && b.name.eq_ignore_ascii_case(&text)).map(|b| b.id).collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("there is no bin named {text}; media_list shows the bins")),
        many => Err(format!("{} bins are named {text}: use bin:{}", many.len(), many.iter().map(u32::to_string).collect::<Vec<_>>().join(" or bin:"))),
    }
}

/// Sources as a list of indexes: `src:<n>`, a number, a list of them, or "all".
fn resolve_sources(count: usize, raw: Option<&Value>) -> Result<Vec<usize>, String> {
    let one = |v: &Value| -> Result<usize, String> {
        let text = match v {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            _ => return Err("a source is src:<n>".into()),
        };
        let idx = text.strip_prefix("src:").unwrap_or(&text).parse::<usize>().map_err(|_| format!("`{text}` is not a source: write src:<n>, e.g. src:3"))?;
        match idx < count {
            true => Ok(idx),
            false => Err(format!("there is no src:{idx}; media_list shows the sources")),
        }
    };
    match raw {
        None | Some(Value::Null) => Err("argument `sources` is required: src:<n>, a list of them, or \"all\"".into()),
        Some(Value::String(s)) if s.trim() == "all" => Ok((0..count).collect()),
        Some(Value::Array(a)) if !a.is_empty() => a.iter().map(one).collect(),
        Some(Value::Array(_)) => Err("`sources` is empty".into()),
        Some(v) => one(v).map(|i| vec![i]),
    }
}

/// `media_bin`: the library's bins and favourites.
fn bin(server: &mut Server, args: &Value) -> Reply {
    let action: String = opt_str(args, "action").unwrap_or_default().to_owned();
    let before = tri!(server.session_ref()).history_pushes();
    let (text, label) = {
        let s = server.session.as_mut().expect("checked above");
        let bins = s.bins().to_vec();
        let result: Result<(String, String), String> = (|| match action.as_str() {
            "create" => {
                let name = opt_str(args, "name").ok_or("create needs `name`")?;
                let parent = match args.get("parent") {
                    None | Some(Value::Null) => 0,
                    Some(p) => resolve_bin(&bins, p)?,
                };
                let id = s.create_bin(name, parent)?;
                let shown = bin_path(s.bins(), id);
                Ok((format!("bin:{id} {shown} created"), format!("media_bin create {shown}")))
            }
            "rename" => {
                let id = resolve_bin(&bins, args.get("bin").ok_or("rename needs `bin`")?)?;
                let name = opt_str(args, "name").ok_or("rename needs `name`")?;
                let was = bin_path(&bins, id);
                let changed = s.rename_bin(id, name)?;
                let shown = bin_path(s.bins(), id);
                Ok((if changed { format!("bin:{id} {was} renamed to {shown}") } else { format!("bin:{id} is already named {shown}; nothing changed") }, format!("media_bin rename {shown}")))
            }
            "remove" => {
                let id = resolve_bin(&bins, args.get("bin").ok_or("remove needs `bin`")?)?;
                let was = bin_path(&bins, id);
                let moved = s.remove_bin(id)?;
                Ok((format!("bin:{id} {was} removed; {moved} source{} moved up", if moved == 1 { "" } else { "s" }), format!("media_bin remove {was}")))
            }
            "move" => {
                let srcs = resolve_sources(s.sources().len(), args.get("sources"))?;
                let target = match args.get("bin") {
                    None | Some(Value::Null) => 0,
                    Some(b) => resolve_bin(&bins, b)?,
                };
                let moved = s.move_to_bin(&srcs, target)?;
                let where_ = if target == 0 { "out of every bin".to_string() } else { format!("into bin:{target} {}", bin_path(&bins, target)) };
                Ok((if moved == 0 { format!("already {where_}; nothing changed") } else { format!("{moved} source{} moved {where_}", if moved == 1 { "" } else { "s" }) }, format!("media_bin move {where_}")))
            }
            "favourite" => {
                let srcs = resolve_sources(s.sources().len(), args.get("sources"))?;
                let on = args.get("on").and_then(Value::as_bool).unwrap_or(true);
                let changed = s.set_favourite(&srcs, on)?;
                let verb = if on { "starred" } else { "unstarred" };
                Ok((if changed == 0 { format!("already {verb}; nothing changed") } else { format!("{changed} source{} {verb}", if changed == 1 { "" } else { "s" }) }, format!("media_bin {verb}")))
            }
            "" => Err("argument `action` is required: create, rename, remove, move or favourite".into()),
            other => Err(format!("unknown action `{other}`: create, rename, remove, move or favourite")),
        })();
        match result {
            Ok(v) => v,
            Err(e) => return Reply::err(e),
        }
    };
    if tri!(server.session_ref()).history_pushes() > before {
        crate::tools::project::note_pushes_as(server, before, &label);
        return Reply::ok(format!("{text}\nundo takes it back; unsaved until project_save"));
    }
    Reply::ok(text)
}

fn list(server: &mut Server, args: &Value) -> Reply {
    let s = tri!(server.session_ref());
    let sources = s.sources();
    let want = opt_str(args, "kind");
    if let Some(k) = want.filter(|k| !["video", "audio", "image"].contains(k)) {
        return Reply::err(format!("kind `{k}` is not video, audio or image"));
    }
    if sources.is_empty() {
        return Reply::ok("no sources; media_import adds them").with(json!({"sources": [], "bins": []}));
    }
    let mut lines: Vec<String> = Vec::new();
    let mut items: Vec<Value> = Vec::new();
    // Each source's kind, parallel to `lines` and `items`, so `kind` can keep some of them.
    let mut kinds: Vec<&str> = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        let path = &src.path;
        let name = file_name(path);
        // A source whose file is gone says so; probing a missing path would call it audio only.
        if !engine::source_present(path) {
            lines.push(format!("src:{i} {name} · OFFLINE (moved or deleted: {})", path.display()));
            items.push(json!({"src": i, "name": name, "path": path.display().to_string(), "offline": true}));
            kinds.push(if engine::is_image(path) { "image" } else { "offline" });
            continue;
        }
        // A generated clip's source has no file to probe: it is a picture made from numbers.
        if let Some(g) = engine::generate::Gen::parse(path) {
            lines.push(format!("src:{i} {} · generated, no file · {}x{}", super::generated::describe(&g), g.width, g.height));
            items.push(json!({"src": i, "name": name, "generator": super::generated::describe(&g), "width": g.width, "height": g.height, "kind": "video"}));
            kinds.push("video");
            continue;
        }
        let facts = probe_media(path);
        let duration_secs = facts.duration_secs;
        let dur = duration_text(path, &facts);
        let picture = match &facts.video {
            Some(v) => {
                let rate = v.frame_rate.map_or("@?".to_string(), |r| format!("@{}", fmt_fps(r)));
                format!("{}x{} {} {rate}", v.width, v.height, codec_phrase(v.codec, v.profile))
            }
            None if engine::is_image(path) => "still image".to_string(),
            None => "audio only".to_string(),
        };
        let audio = audio_choice_text(&facts, src.audio_stream);
        lines.push(format!("src:{i} {name} · {dur} · {picture}{audio}"));
        let kind = if engine::is_image(path) {
            "image"
        } else if facts.video.is_some() {
            "video"
        } else {
            "audio"
        };
        kinds.push(kind);
        items.push(json!({
            "src": i,
            "name": name,
            "kind": kind,
            "path": path.display().to_string(),
            "audio_stream": src.audio_stream,
            "duration_secs": duration_secs,
            "audio_duration_secs": audio_only_secs(path, &facts),
            "width": facts.video.map(|v| v.width),
            "height": facts.video.map(|v| v.height),
            "codec": facts.video.map(|v| v.codec.name()),
            "profile": facts.video.and_then(|v| v.profile),
            "interlaced": facts.video.and_then(|v| v.interlaced),
            "frame_rate": facts.video.and_then(|v| v.frame_rate),
        }));
    }
    // The library's shelves: each source's bin and star, then the bins themselves up front.
    let bins = s.bins();
    for ((src, line), item) in sources.iter().zip(lines.iter_mut()).zip(items.iter_mut()) {
        let in_bin = bin_path(bins, src.bin);
        if src.bin != 0 {
            line.push_str(&format!(" · in bin:{} {in_bin}", src.bin));
        }
        if src.fav {
            line.push_str(" · favourite");
        }
        item["bin"] = if src.bin == 0 { Value::Null } else { json!(src.bin) };
        item["bin_path"] = if src.bin == 0 { Value::Null } else { json!(in_bin) };
        item["favourite"] = json!(src.fav);
    }
    // `kind` keeps the sources of that kind; the bins are listed whole, as before.
    if let Some(k) = want {
        let keep: Vec<bool> = kinds.iter().map(|x| *x == k).collect();
        let mut keep_it = keep.iter();
        items.retain(|_| *keep_it.next().unwrap_or(&false));
        let mut keep_it = keep.iter();
        lines.retain(|_| *keep_it.next().unwrap_or(&false));
        if lines.is_empty() {
            return Reply::ok(format!("no {k} sources")).with(json!({"sources": [], "bins": []}));
        }
    }
    let bin_items: Vec<Value> = bins
        .iter()
        .map(|b| json!({"bin": b.id, "name": b.name, "parent": (b.parent != 0).then_some(b.parent), "path": bin_path(bins, b.id), "sources": sources.iter().filter(|x| x.bin == b.id).count()}))
        .collect();
    let mut head: Vec<String> = bins.iter().map(|b| format!("bin:{} {} · {} source{}", b.id, bin_path(bins, b.id), sources.iter().filter(|x| x.bin == b.id).count(), if sources.iter().filter(|x| x.bin == b.id).count() == 1 { "" } else { "s" })).collect();
    head.extend(lines);
    Reply::ok(head.join("\n")).with(json!({"sources": items, "bins": bin_items}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use engine::PlaybackSession;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn quiet() {
        engine::ao::set_silent_output();
    }

    /// An empty scratch directory unique to this test, removed when the guard drops.
    fn scratch_dir(test: &str) -> TestDir {
        TestDir::new(&format!("media-{test}"))
    }

    #[test]
    fn wildcard_matcher() {
        assert!(wild("*.mp4", "a.mp4"));
        assert!(wild("clip?.mp4", "clip1.mp4"));
        assert!(!wild("clip?.mp4", "clip12.mp4"));
        assert!(wild("c*p*4", "clip1.mp4"));
        assert!(!wild("*.mp4", "a.mov"));
        assert!(wild("*", "anything"));
    }

    #[test]
    fn glob_and_paths_expand_in_order() {
        quiet();
        let dir = scratch_dir("glob");
        for name in ["clip1.mp4", "clip2.mp4", "notes.txt"] {
            std::fs::copy(asset("test_av.mp4"), dir.join(name)).unwrap();
        }
        let pattern = format!("{}/clip*.mp4", dir.display());
        let found = expand_glob(&pattern).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(file_name(&found[0]), "clip1.mp4");
        assert_eq!(expand_glob(&format!("{}/clip?.mp4", dir.display())).unwrap().len(), 2);
        assert_eq!(expand_glob(&format!("{}/*.txt", dir.display())).unwrap().len(), 1);
        assert!(expand_glob(&format!("{}/*.mkv", dir.display())).unwrap().is_empty());

        let args = json!({"glob": format!("{}/*.mp4", dir.display()), "paths": [format!("{}/clip1.mp4", dir.display())]});
        assert_eq!(gather(&args, true).ok().map(|v| v.len()), Some(2), "repeats are dropped");
    }

    #[test]
    fn probe_glob_reports_each_file() {
        quiet();
        let dir = scratch_dir("probe");
        for name in ["clip1.mp4", "clip2.mp4"] {
            std::fs::copy(asset("test_av.mp4"), dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("broken.mp4"), b"not media at all").unwrap();
        let mut server = Server::new();
        let args = json!({"glob": format!("{}/*.mp4", dir.display())});
        let r = probe(&mut server, &args);
        assert!(!r.is_error, "{}", r.text);
        let files = r.structured.as_ref().unwrap()["files"].as_array().unwrap().clone();
        assert_eq!(files.len(), 3);
        let good = files.iter().find(|f| f["name"] == "clip1.mp4").unwrap();
        assert!(good["width"].as_u64().unwrap() > 0);
        assert!(good["error"].is_null());
        let bad = files.iter().find(|f| f["name"] == "broken.mp4").unwrap();
        assert!(bad["error"].is_string());
        assert!(r.text.contains("clip1.mp4"), "{}", r.text);
        assert!(r.text.contains("broken.mp4 looks truncated or is not a valid MP4"), "{}", r.text);
    }

    #[test]
    fn probe_missing_file_is_inline_error() {
        let mut server = Server::new();
        let r = probe(&mut server, &json!({"paths": ["/definitely/not/here.mp4"]}));
        assert!(r.is_error);
        assert!(r.text.contains("not found"), "{}", r.text);
        assert!(probe(&mut server, &json!({})).is_error);
    }

    #[test]
    fn import_then_list_needs_a_project() {
        quiet();
        let mut none = Server::new();
        assert!(import(&mut none, &json!({"paths": [asset("test_av.mp4")]})).is_error);
        assert!(list(&mut none, &json!({})).is_error);

        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = import(&mut server, &json!({"paths": [asset("test_av.mp4")]}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src:0 test_av.mp4"), "{}", r.text);
        assert!(server.ledger.is_empty(), "import is not an undo step");

        let again = import(&mut server, &json!({"paths": [asset("test_av.mp4")]}));
        assert!(again.text.contains("src:0"), "importing twice registers once: {}", again.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1);

        let l = list(&mut server, &json!({}));
        assert!(!l.is_error);
        assert!(l.text.contains("src:0 test_av.mp4"), "{}", l.text);
        assert_eq!(l.structured.as_ref().unwrap()["sources"][0]["src"], json!(0));
    }

    #[test]
    fn import_refuses_missing_file_and_keeps_going() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = import(
            &mut server,
            &json!({"paths": ["/definitely/not/here.mp4", asset("test_av.mp4").display().to_string()]}),
        );
        assert!(!r.is_error, "one good file makes the call a success: {}", r.text);
        assert!(r.text.contains("not found: /definitely/not/here.mp4"), "{}", r.text);
        assert!(r.text.contains("src:0"), "{}", r.text);
    }

    /// A matrix the file does not tag says so; a tagged one reads as before.
    #[test]
    fn an_untagged_matrix_says_it_is_assumed() {
        use engine::colorspace::{ColorDescription, Matrix, Transfer};
        let bt709 = ColorDescription { matrix: Matrix::Bt709, ..ColorDescription::default() };
        assert_eq!(colour_text(bt709, true), "BT.709 (assumed, untagged)");
        assert_eq!(colour_text(bt709, false), "BT.709");
        let pq = ColorDescription { transfer: Transfer::Pq, ..bt709 };
        assert!(colour_text(pq, true).starts_with("BT.709 (assumed, untagged) PQ"), "{}", colour_text(pq, true));
        assert_eq!(colour_text(pq, false).split(" (HDR").next(), Some("BT.709 PQ"));
    }

    /// ProRes, DNxHD/DNxHR and MJPEG are named in plain words, with the profile after the codec
    /// (Avid's profile names carry the family themselves).
    #[test]
    fn the_professional_codecs_are_named_with_their_profile() {
        use engine::demux::Codec;
        assert_eq!(codec_phrase(Codec::ProRes, Some("422 HQ")), "ProRes 422 HQ");
        assert_eq!(codec_phrase(Codec::Dnxhd, Some("DNxHR HQX")), "DNxHR HQX");
        assert_eq!(codec_phrase(Codec::Dnxhd, Some("DNxHD 145")), "DNxHD 145");
        assert_eq!(codec_phrase(Codec::Dnxhd, None), "DNxHD");
        assert_eq!(codec_phrase(Codec::Mjpeg, None), "MJPEG");
        assert_eq!(plain_codec("apch"), "prores");
        assert_eq!(plain_codec("AVdn"), "dnxhd");
        assert_eq!(plain_codec("AVdh"), "dnxhr");
        assert_eq!(plain_codec("V_MJPEG"), "mjpeg");
        assert_eq!(plain_codec("mjpa"), "mjpeg");
        assert_eq!(plain_codec("dvc "), "dv");
        assert_eq!(plain_codec("dv5p"), "dv");
        assert_eq!(plain_codec("mp4v"), "mpeg4");
        assert_eq!(plain_codec("V_MPEG4/ISO/ASP"), "mpeg4");
        assert_eq!(plain_codec("s263"), "h263");
        assert_eq!(codec_phrase(Codec::Dv, Some("DVCPRO50")), "DV DVCPRO50");
        assert_eq!(codec_phrase(Codec::Mpeg4, Some("Advanced Simple")), "MPEG-4 Advanced Simple");
        assert_eq!(codec_phrase(Codec::Mpeg4, Some("H.263")), "H.263");
        assert_eq!(codec_phrase(Codec::MsMpeg4, Some("v3")), "MS-MPEG4 v3");
        assert_eq!(codec_phrase(Codec::Raw, Some("BGR24")), "Raw BGR24");
    }

    /// The video profile follows the codec name in the picture phrase, in the probe line and the list,
    /// and the JSON row carries it with the matrix flag.
    #[test]
    fn the_video_profile_follows_the_codec_name() {
        let row = probe_one(&asset("test_hevc10.mkv"));
        let line = row_line(&row);
        assert!(line.starts_with("test_hevc10.mkv · mkv · HEVC Main 10 "), "{line}");
        assert_eq!(row_json(&row)["profile"], json!("Main 10"));
        assert_eq!(row_json(&row)["matrix_assumed"], json!(true));
        let gray = probe_one(&asset("test_av1_gray.mkv"));
        assert_eq!(row_json(&gray)["profile"], json!("Main"));
        assert_eq!(row_json(&gray)["matrix_assumed"], json!(true));

        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [asset("test_hevc10.mkv").display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        let listed = list(&mut server, &json!({}));
        assert!(listed.text.contains("1280x720 HEVC Main 10 @30"), "{}", listed.text);
        assert_eq!(listed.structured.as_ref().unwrap()["sources"][0]["profile"], json!("Main 10"));
    }

    /// A lossless track says its depth after the codec: FLAC and PCM from the header, and a lossy
    /// track says none. The JSON row carries the depth as a number.
    #[test]
    fn a_lossless_track_says_its_bit_depth() {
        let text = |name: &str| -> Vec<String> {
            let path = asset(name);
            other_streams(&path, &probe_media(&path)).iter().map(stream_text).collect()
        };
        assert_eq!(text("test_tone.flac"), vec!["audio track 0 flac 24-bit (und) stereo, 44.1 kHz"]);
        assert_eq!(text("test_tone.wav"), vec!["audio track 0 pcm_s16le 16-bit (und) stereo, 44.1 kHz"]);
        assert_eq!(text("test_tone.mp3"), vec!["audio track 0 mp3 (und) stereo, 44.1 kHz"]);
        let flac = asset("test_tone.flac");
        let streams = other_streams(&flac, &probe_media(&flac));
        assert_eq!(stream_json(&streams[0])["bit_depth"], json!(24));
        let mp3 = asset("test_tone.mp3");
        assert!(stream_json(&other_streams(&mp3, &probe_media(&mp3))[0])["bit_depth"].is_null());
    }

    #[test]
    fn durations_come_from_the_header_and_stills_say_still() {
        quiet();
        let dir = scratch_dir("durations");
        std::fs::copy(asset("test_still.png"), dir.join("still.png")).unwrap();
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": [asset("test_av.mp4").display().to_string(), dir.join("still.png").display().to_string()]}));
        assert!(!probed.is_error, "{}", probed.text);
        assert!(probed.text.contains("00:05.000"), "5 s clip read as 5 s: {}", probed.text);
        assert!(probed.text.contains("· still ·"), "a still has no playing time: {}", probed.text);
        assert!(!probed.text.contains("10:00"), "{}", probed.text);

        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [asset("test_av.mp4").display().to_string(), dir.join("still.png").display().to_string()]}));
        assert!(imported.text.contains("test_av.mp4 (00:05.000)"), "{}", imported.text);
        assert!(imported.text.contains("still.png (still)"), "{}", imported.text);
        let listed = list(&mut server, &json!({}));
        assert!(listed.text.contains("00:05.000"), "{}", listed.text);
        assert!(listed.text.contains("still"), "{}", listed.text);
        let still = &listed.structured.as_ref().unwrap()["sources"][1];
        assert!(still["duration_secs"].is_null(), "{still}");
    }

    #[test]
    fn missing_file_says_not_found_with_its_path() {
        quiet();
        let missing = "/definitely/not/here.mp4";
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": [missing]}));
        assert!(probed.text.contains(&format!("not found: {missing}")), "{}", probed.text);
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [missing]}));
        assert!(imported.is_error);
        assert_eq!(imported.text, format!("not found: {missing}\nnothing was imported"));
    }

    /// A bulk import shows every refusal and warning in full, however many paths there are;
    /// only the plain successes are folded into a count.
    #[test]
    fn bulk_import_lists_every_refusal_and_folds_only_successes() {
        quiet();
        let mut names: Vec<PathBuf> = std::fs::read_dir(asset("")).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "mp4" || e == "wav" || e == "mkv")).collect();
        names.sort();
        assert!(names.len() >= 30, "enough assets for the bulk test: {}", names.len());
        let mut paths: Vec<String> = names.iter().take(30).map(|p| p.display().to_string()).collect();
        let missing = "/definitely/not/here-41.mp4".to_string();
        paths.insert(3, missing.clone());
        let folder = asset("fonts").display().to_string();
        paths.insert(20, folder.clone());
        assert_eq!(paths.len(), 32);
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = import(&mut server, &json!({"paths": paths}));
        let imported = r.structured.as_ref().unwrap()["imported"].as_array().unwrap().len();
        assert!(imported > 20, "more than the shown count imported: {imported}");
        assert!(r.text.contains(&format!("not found: {missing}")), "the not-found line is in full: {}", r.text);
        assert!(r.text.contains(&folder), "the refused folder is in full: {}", r.text);
        let folded = imported - IMPORT_SHOWN;
        assert!(r.text.contains(&format!("…and {folded} more imported")), "the count is exact: {}", r.text);
        assert_eq!(r.text.lines().filter(|l| l.starts_with("src:")).count(), IMPORT_SHOWN, "{}", r.text);
        assert!(!r.is_error);
    }

    /// A picture the decoder refuses shows its reason and the streams the header names, together.
    /// (12-bit AV1 opens now; the display matrix is a refusal that remains.)
    #[test]
    fn a_refused_picture_shows_its_reason_and_its_streams() {
        quiet();
        let row = probe_one(&asset("test_av1_12bit.mkv"));
        assert!(row.error.is_none(), "12-bit AV1 opens now: {:?}", row.error);
        let text = row_line(&row);
        assert!(!text.contains("refused:"), "{text}");
        assert!(text.contains("picture: 12-bit 4:2:0"), "the picture is named: {text}");
        assert!(row_json(&row)["error"].is_null());
        let rot = probe_one(&asset("test_rotation45.mkv"));
        assert!(rot.error.is_some(), "the display matrix refusal is kept");
        assert!(row_line(&rot).contains("streams: video #0"), "{}", row_line(&rot));
    }

    /// A sound codec nothing decodes is named in probe, with the silence it causes, and an import
    /// of such a file comes in silent with the same warning: the length the header states holds
    /// the silence. DTS is decoded now, so the file is a WavPack one written here.
    #[test]
    fn an_unsupported_sound_codec_is_warned_in_probe_and_import() {
        quiet();
        let td = crate::testutil::TestDir::new("unsupported-sound-codec");
        let wv = td.path().join("wavpack.mkv");
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=duration=1", "-ar", "48000", "-ac", "2"])
            .args(["-c:a", "wavpack"])
            .arg(&wv)
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
        let row = probe_one(&wv);
        let want = "warning: audio: WavPack is not supported; this file's sound will be silent";
        assert_eq!(row.warnings.len(), 1, "{:?}", row.warnings);
        assert_eq!(row.warnings[0], want, "{:?}", row.warnings);
        assert!(row_line(&row).contains(&row.warnings[0]), "{}", row_line(&row));
        assert_eq!(row_json(&row)["warnings"][0], row.warnings[0].as_str());
        // Import takes the file: its sound is silent for the length the header states, and the
        // same warning is said beside the file's name.
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [wv.display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        assert!(!imported.text.contains("has no audio track"), "{}", imported.text);
        assert_eq!(imported.structured.as_ref().unwrap()["imported"].as_array().unwrap().len(), 1, "{}", imported.text);
        assert!(imported.text.contains(want), "{}", imported.text);
        // A file with decodable sound has no warning.
        assert!(probe_one(&asset("test_h264.mkv")).warnings.is_empty());
    }

    #[test]
    fn codec_names_come_from_the_engine_reason() {
        // The engine's friendly names, as `codec_display_name` gives them.
        let wavpack = "the WavPack track of this Matroska file cannot be decoded — AAC, AC-3/E-AC-3, … are; \
                       this file's sound will be silent";
        assert_eq!(codec_named_in(wavpack).as_deref(), Some("WavPack"));
        assert_eq!(codec_named_in("the DTS track of this Matroska file cannot be decoded: no XLL").as_deref(), Some("DTS"));
        assert_eq!(codec_named_in("the Musepack track of this Matroska file cannot be decoded").as_deref(), Some("Musepack"));
        assert_eq!(codec_named_in("the MP2 track of this Matroska file cannot be decoded").as_deref(), Some("MP2"));
        // A Matroska id still named in a reason falls back to its id.
        assert_eq!(codec_named_in("the A_DTS track of this file cannot be decoded").as_deref(), Some("DTS"));
        assert_eq!(codec_named_in("the A_AC3/ELD track").as_deref(), Some("AC3"));
        // Nothing named at all.
        assert_eq!(codec_named_in("the audio track cannot be decoded"), None);
        assert_eq!(codec_named_in("some other reason"), None);
    }

    #[test]
    fn import_cap_folds_successes_and_never_refusals() {
        let mut lines = vec![ImportLine::Imported("a".into())];
        lines.extend((0..5).map(|i| ImportLine::Imported(format!("s{i}"))));
        lines.insert(2, ImportLine::Note("bad: refused".into()));
        lines.push(ImportLine::Note("warn: x".into()));
        let out = cap_successes(lines, 3);
        assert_eq!(out, vec!["a", "s0", "bad: refused", "s1", "warn: x", "…and 3 more imported"]);
    }

    #[test]
    fn a_short_picture_with_a_long_sound_reports_both_streams() {
        quiet();
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": [asset("test_short_video_long_audio.mp4").display().to_string()]}));
        assert!(!probed.is_error, "{}", probed.text);
        assert!(probed.text.contains("2.000 s; audio "), "both lengths named: {}", probed.text);
        assert!(!probed.text.contains("(audio "), "no nested brackets: {}", probed.text);
        let facts = probed.structured.as_ref().unwrap();
        let row = &facts["files"][0];
        assert!((row["duration_secs"].as_f64().unwrap() - 2.0).abs() < 0.01, "{row}");
        assert!(row["audio_duration_secs"].as_f64().unwrap() > 59.0, "{row}");
    }

    #[test]
    fn media_remove_refuses_a_used_source_and_drops_an_unused_one() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let used = asset("test_av.mp4");
        let spare = asset("test_av2.mp4");
        let session = server.session.as_mut().unwrap();
        session.import(&used).expect("import the placed file");
        session.import(&spare).expect("import the spare");
        assert!(session.place_stream_at(0.0, &used, 0, None).expect("place"));
        // src:0 is the placed file; src:1 is the spare, which nothing plays.
        let refused = remove(&mut server, &json!({"source": "src:0"}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("still plays in"), "{}", refused.text);
        assert!(refused.text.contains("V1@"), "names the clip: {}", refused.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 2, "refusal changed nothing");

        let dropped = remove(&mut server, &json!({"source": "src:1"}));
        assert!(!dropped.is_error, "{}", dropped.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1);
        assert_eq!(server.saved_depth, None, "a removal leaves the project unsaved");
        assert!(server.ledger.is_empty());
        let bad = remove(&mut server, &json!({"source": "src:9"}));
        assert!(bad.is_error && bad.text.contains("there is no src:9"), "{}", bad.text);
    }

    #[test]
    fn a_still_is_a_picture_in_the_probe_and_the_list() {
        quiet();
        let dir = scratch_dir("still-label");
        let still = dir.join("still.png");
        std::fs::copy(asset("test_still.png"), &still).unwrap();
        let probed = probe(&mut Server::new(), &json!({"paths": [still.display().to_string()]}));
        assert!(probed.text.contains("still image"), "{}", probed.text);
        assert!(!probed.text.contains("no picture"), "{}", probed.text);
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        assert!(!import(&mut server, &json!({"paths": [still.display().to_string()]})).is_error);
        let listed = list(&mut server, &json!({}));
        assert!(listed.text.contains("still image"), "{}", listed.text);
        assert!(!listed.text.contains("audio only"), "{}", listed.text);
    }

    #[test]
    fn a_device_is_not_a_regular_file_and_a_refused_import_says_nothing_came_in() {
        quiet();
        let dir = scratch_dir("refused-import");
        let junk = dir.join("junk.mp4");
        std::fs::write(&junk, b"not a movie at all").unwrap();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let dev = import(&mut server, &json!({"paths": ["/dev/zero", "/dev/null"]}));
        assert!(dev.is_error, "{}", dev.text);
        assert!(dev.text.contains("is not a regular file: /dev/zero"), "{}", dev.text);
        assert!(!dev.text.contains("is a directory"), "{}", dev.text);
        assert!(dev.text.contains("nothing was imported"), "{}", dev.text);
        let probed = probe(&mut server, &json!({"paths": ["/dev/null"]}));
        assert!(probed.text.contains("is not a regular file: /dev/null"), "{}", probed.text);

        let refused = import(&mut server, &json!({"paths": [junk.display().to_string()]}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("nothing was imported"), "{}", refused.text);
        let listed = list(&mut server, &json!({}));
        assert!(listed.text.contains("no sources"), "{}", listed.text);
        assert_eq!(listed.structured.as_ref().unwrap()["sources"], json!([]));
    }

    #[test]
    fn wildcard_sets_match_one_character() {
        assert!(wild("clip[0-9].mp4", "clip7.mp4"));
        assert!(!wild("clip[0-9].mp4", "clipa.mp4"));
        assert!(wild("clip[!0-9].mp4", "clipa.mp4"));
        assert!(!wild("clip[!0-9].mp4", "clip7.mp4"));
        assert!(wild("[ab]*", "beta"));
        assert!(!wild("[ab]*", "gamma"));
        assert!(wild("odd[", "odd["), "an unclosed [ is a plain character");
    }

    #[test]
    fn paths_accepts_one_string_and_names_an_empty_list() {
        quiet();
        let dir = scratch_dir("string-paths");
        for name in ["clip1.mp4", "clip2.mp4"] {
            std::fs::copy(asset("test_av.mp4"), dir.join(name)).unwrap();
        }
        let single = asset("test_av.mp4").display().to_string();
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": single}));
        assert!(!probed.is_error, "{}", probed.text);
        assert!(probed.text.contains("test_av.mp4"), "{}", probed.text);

        let glob = format!("{}/clip[12].mp4", dir.display());
        assert_eq!(gather(&json!({"paths": glob}), true).ok().map(|v| v.len()), Some(2), "a string with a glob expands");
        assert_eq!(gather(&json!({"paths": [glob]}), true).ok().map(|v| v.len()), Some(2));

        let empty = probe(&mut server, &json!({"paths": []}));
        assert!(empty.is_error);
        assert!(empty.text.contains("is empty"), "{}", empty.text);
        let blank = import(&mut server, &json!({"paths": "  "}));
        assert!(blank.is_error && blank.text.contains("is empty"), "{}", blank.text);
        assert!(probe(&mut server, &json!({})).text.contains("give `paths` or `glob`"));
    }

    /// A blank language tag reads `und`, as a missing one does: test_qt.mov's sound has an empty tag.
    #[test]
    fn blank_language_reads_und() {
        assert_eq!(language_of(Some(""), ""), "und");
        assert_eq!(language_of(Some("   "), ""), "und");
        assert_eq!(language_of(None, ""), "und");
        assert_eq!(language_of(Some("\u{7f}\u{7f}\u{7f}"), ""), "und", "an unset mdhd language");
        assert_eq!(language_of(Some("\u{7f}\u{7f}\u{7f}"), "eng"), "eng");
        assert_eq!(language_of(Some(""), "eng"), "eng", "a blank tag falls back to the walk's language");
        assert_eq!(language_of(Some(" fra "), "eng"), "fra");

        let qt = asset("test_qt.mov");
        let streams = other_streams(&qt, &engine::probe_media(&qt));
        let text: Vec<String> = streams.iter().map(stream_text).collect();
        assert_eq!(text, vec!["audio track 0 aac (und) stereo, 44.1 kHz"], "{text:?}");
    }

    /// A track's layout and rate are named where the header states them, and the surround
    /// note is given once per track of more than two channels, in media_probe only.
    #[test]
    fn channels_and_rate_are_listed_and_surround_is_noted_as_mixed() {
        let surround = asset("test_ac3_51.mp4");
        let streams = other_streams(&surround, &engine::probe_media(&surround));
        let text: Vec<String> = streams.iter().filter(|s| s.kind == "audio").map(stream_text).collect();
        assert_eq!(text, vec!["audio track 0 ac3 (und) 5.1, 48 kHz"], "{text:?}");
        assert_eq!(
            surround_notes(&streams),
            vec!["note: audio track 0 is 5.1; mixed to stereo on export"]
        );
        let row = probe_one(&surround);
        assert!(row_line(&row).contains("\n  note: audio track 0 is 5.1; mixed to stereo on export"), "{}", row_line(&row));

        let stereo = asset("test_multilang.mp4");
        let streams = other_streams(&stereo, &engine::probe_media(&stereo));
        assert!(surround_notes(&streams).is_empty(), "stereo gets no note");
        assert!(!row_line(&probe_one(&stereo)).contains("mixed to stereo"));
    }

    /// Two sources of one file read apart: a source on track 1 says which track and language.
    #[test]
    fn media_list_names_a_non_default_audio_track() {
        let path = asset("test_multilang.mp4");
        let facts = engine::probe_media(&path);
        assert_eq!(audio_choice_text(&facts, 0), "", "track 0 is the default and says nothing");
        assert_eq!(audio_choice_text(&facts, 1), " · audio 1 (fra)");
    }

    /// media_list kind keeps one kind of source, says so when there is none, and refuses any other word.
    #[test]
    fn media_list_kind_keeps_one_kind_and_refuses_the_rest() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let video = list(&mut server, &json!({"kind": "video"}));
        assert!(!video.is_error && video.text.contains("test_av"), "{}", video.text);
        let image = list(&mut server, &json!({"kind": "image"}));
        assert!(!image.is_error && image.text.contains("no image sources"), "{}", image.text);
        let movie = list(&mut server, &json!({"kind": "movie"}));
        assert!(movie.is_error && movie.text.contains("video, audio or image"), "{}", movie.text);
    }

    /// Bins and favourites: made, filed, starred, listed, refused by name, and one undo step each.
    #[test]
    fn media_bin_files_and_stars_sources_and_media_list_shows_it() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let s = server.session.as_mut().unwrap();
        s.import(&asset("test_av.mp4")).expect("import");
        s.import(&asset("test_av2.mp4")).expect("import");
        let made = bin(&mut server, &json!({"action": "create", "name": "Interviews"}));
        assert!(!made.is_error, "{}", made.text);
        assert!(made.text.contains("bin:1 Interviews created") && made.text.contains("undo takes it back"), "{}", made.text);
        let inner = bin(&mut server, &json!({"action": "create", "name": "Day 1", "parent": "interviews"}));
        assert!(inner.text.contains("Interviews/Day 1"), "{}", inner.text);
        let twin = bin(&mut server, &json!({"action": "create", "name": "INTERVIEWS"}));
        assert!(twin.is_error && twin.text.contains("already a bin"), "{}", twin.text);
        let moved = bin(&mut server, &json!({"action": "move", "sources": ["src:1"], "bin": "Interviews/Day 1"}));
        assert!(moved.text.contains("1 source moved into bin:2"), "{}", moved.text);
        let starred = bin(&mut server, &json!({"action": "favourite", "sources": "all"}));
        assert!(starred.text.contains("2 sources starred"), "{}", starred.text);
        let again = bin(&mut server, &json!({"action": "favourite", "sources": "src:0"}));
        assert!(again.text.contains("nothing changed"), "{}", again.text);
        let listed = list(&mut server, &json!({}));
        assert!(listed.text.contains("bin:2 Interviews/Day 1 · 1 source"), "{}", listed.text);
        assert!(listed.text.contains("in bin:2 Interviews/Day 1 · favourite"), "{}", listed.text);
        let bad = bin(&mut server, &json!({"action": "move", "sources": "src:9", "bin": 1}));
        assert!(bad.is_error && bad.text.contains("no src:9"), "{}", bad.text);
        let gone = bin(&mut server, &json!({"action": "remove", "bin": "bin:1"}));
        assert!(gone.text.contains("removed; 0 sources moved up"), "{}", gone.text);
        let s = server.session.as_ref().unwrap();
        assert_eq!(s.bins().len(), 1);
        assert_eq!(s.bins()[0].parent, 0);
        let s = server.session.as_mut().unwrap();
        assert!(s.undo(), "the removal undoes");
        assert_eq!(s.bins().len(), 2);
    }

    /// Removing the last unused source keeps the undo history; a source before others clears it.
    #[test]
    fn media_remove_keeps_undo_for_the_last_unused_source() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let used = asset("test_av.mp4");
        let spare = asset("test_av2.mp4");
        let session = server.session.as_mut().unwrap();
        session.import(&used).expect("import the placed file");
        session.import(&spare).expect("import the spare");
        assert!(session.place_stream_at(0.0, &used, 0, None).expect("place"));
        let depth = session.undo_depth();
        assert!(depth > 0, "placing pushed an undo step");

        let dropped = remove(&mut server, &json!({"source": "src:1"}));
        assert!(!dropped.is_error, "{}", dropped.text);
        assert!(dropped.text.contains("undo history is kept"), "{}", dropped.text);
        let session = server.session.as_mut().unwrap();
        assert_eq!(session.sources().len(), 1);
        assert_eq!(session.undo_depth(), depth, "the undo steps are still there");
        assert!(session.undo(), "undo still works after the removal");
    }

    #[test]
    fn media_remove_of_a_source_before_others_says_history_is_cleared() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let session = server.session.as_mut().unwrap();
        // src:0 plays nothing; src:1 is placed, which is an undo step.
        session.import(&asset("test_av2.mp4")).expect("import the spare");
        session.import(&asset("test_av.mp4")).expect("import the placed file");
        assert!(session.place_stream_at(0.0, &asset("test_av.mp4"), 0, None).expect("place"));
        let depth = session.undo_depth();
        assert!(depth > 0, "placing pushed an undo step");
        // Refused without force: the reply says why and changes nothing.
        let refused = remove(&mut server, &json!({"source": "src:0"}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("renumbers src:1"), "{}", refused.text);
        assert!(refused.text.contains("undo history is cleared"), "{}", refused.text);
        assert!(refused.text.contains("force:true"), "{}", refused.text);
        let session = server.session.as_ref().unwrap();
        assert_eq!(session.sources().len(), 2);
        assert_eq!(session.undo_depth(), depth, "the undo steps are still there");
        // With force the removal goes ahead and says the history was cleared.
        let dropped = remove(&mut server, &json!({"source": "src:0", "force": true}));
        assert!(!dropped.is_error, "{}", dropped.text);
        assert!(dropped.text.contains("Undo history is cleared"), "{}", dropped.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1);
    }

    /// The empty-list refusal names only arguments media_import has.
    #[test]
    fn import_refusal_names_no_glob_argument() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let empty = import(&mut server, &json!({"paths": []}));
        assert!(empty.is_error);
        assert!(!empty.text.contains("`glob`"), "{}", empty.text);
        assert!(empty.text.contains("pattern such as ~/clips/*.mp4 inside it"), "{}", empty.text);
        let none = import(&mut server, &json!({}));
        assert!(!none.text.contains("`glob`"), "{}", none.text);
        assert!(none.text.contains("give `paths`:"), "{}", none.text);
    }
}

#[cfg(test)]
mod agreement_tests {
    use super::*;
    use crate::testutil::TestDir;
    use engine::PlaybackSession;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn quiet() {
        engine::ao::set_silent_output();
    }

    /// A truncated file is refused by `media_probe` in the same words `media_import` refuses it
    /// Probe and import give the same plain sentence for a truncated file: the name, that it may be
    /// truncated or not an MP4, the engine's detail, and the next step.
    #[test]
    fn a_truncated_file_gets_the_same_refusal_from_probe_and_import() {
        quiet();
        let td = TestDir::new("probe-import-agree");
        let full = std::fs::read(asset("test_av.mp4")).expect("read the fixture");
        let cut = td.path().join("cut.mp4");
        std::fs::write(&cut, &full[..full.len().min(4000)]).expect("write the truncated copy");
        let probe = row_line(&probe_one(&cut));
        let want = "cut.mp4 looks truncated or is not a valid MP4 (";
        assert!(probe.contains(want) && probe.contains("re-copy or re-export"), "{probe}");
        assert!(!probe.contains("refused: file contains"), "{probe}");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [cut.display().to_string()]}));
        assert!(imported.is_error, "{}", imported.text);
        assert!(imported.text.contains(want) && imported.text.contains("re-copy or re-export"), "{}", imported.text);
    }

    /// A text file renamed .mp4 and a zero-byte .mp4 each get their plain sentence, not the engine's.
    /// A subtitle file is named as one, with the tool that imports it, not as a truncated MP4, by
    /// both the probe and the import.
    #[test]
    fn a_subtitle_file_is_named_and_not_called_truncated() {
        quiet();
        let td = TestDir::new("subtitle-file");
        let srt = td.path().join("cues.srt");
        std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").expect("write the srt");
        let line = row_line(&probe_one(&srt));
        assert!(line.contains("cues.srt is a subtitle file: import it with subtitles_import"), "{line}");
        assert!(!line.contains("truncated"), "{line}");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = import(&mut server, &json!({"paths": [srt.display().to_string()]}));
        assert!(r.text.contains("cues.srt is a subtitle file: import it with subtitles_import"), "{}", r.text);
        assert!(!r.text.contains("truncated"), "{}", r.text);
    }

    /// One frame of a numbered PNG run imports the whole run as one clip, reported as an image
    /// sequence of its frames, not as a file of its own.
    #[test]
    fn a_numbered_png_run_imports_as_one_image_sequence() {
        quiet();
        let td = TestDir::new("png-run-import");
        let still = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../engine/tests/fixtures/fuzz/still.png")).expect("read the still");
        for k in 1..=10 {
            std::fs::write(td.path().join(format!("frame_{k:04}.png")), &still).expect("write a frame");
        }
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let one = td.path().join("frame_0004.png");
        let r = import(&mut server, &json!({"paths": [one.display().to_string()]}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("image sequence, 10 frames"), "{}", r.text);
        assert!(r.text.contains("frame_[0001-0010].png"), "{}", r.text);
    }

    #[test]
    fn a_text_file_named_mp4_looks_truncated_and_an_empty_file_is_empty() {
        quiet();
        let td = TestDir::new("corrupt-plain");
        let text = td.path().join("notes.mp4");
        std::fs::write(&text, "this is plain text, not a video\n").expect("write the text file");
        let empty = td.path().join("blank.mp4");
        std::fs::write(&empty, b"").expect("write the empty file");
        let text_line = row_line(&probe_one(&text));
        assert!(text_line.contains("notes.mp4 looks truncated or is not a valid MP4 ("), "{text_line}");
        let empty_line = row_line(&probe_one(&empty));
        assert!(empty_line.contains("blank.mp4 is empty (0 bytes)"), "{empty_line}");
        assert!(!empty_line.contains("ftyp"), "{empty_line}");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let r = import(&mut server, &json!({"paths": [empty.display().to_string()]}));
        assert!(r.text.contains("blank.mp4 is empty (0 bytes)"), "{}", r.text);
        assert!(server.session.as_ref().unwrap().sources().is_empty());
    }

    /// Range-extensions HEVC (4:4:4, 4:0:0, 12-bit) is carried by the software seat, so the probe
    /// refuses nothing and states the real chroma format and depth, as it does for other codecs.
    #[test]
    fn range_extensions_hevc_probes_clean_with_its_real_chroma_and_depth() {
        quiet();
        let td = TestDir::new("rext-hevc-probe");
        for (pix, chroma, depth) in [
            ("yuv444p", "4:4:4", 8),
            ("yuv444p10le", "4:4:4", 10),
            ("yuv422p12le", "4:2:2", 12),
            ("gray", "gray", 8),
        ] {
            let clip = td.path().join(format!("rext_{pix}.mp4"));
            let out = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=30:duration=0.1"])
                .args(["-frames:v", "3", "-c:v", "libx265", "-pix_fmt", pix, "-x265-params", "log-level=error"])
                .arg(&clip)
                .output()
                .expect("ffmpeg runs");
            assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
            let row = probe_one(&clip);
            assert_eq!(row.error, None, "{pix}");
            let json = row_json(&row);
            assert_eq!(json["chroma"], json!(chroma), "{pix}");
            assert_eq!(json["bit_depth"], json!(depth), "{pix}: {json}");
        }
    }

    #[test]
    fn the_picture_line_names_depth_chroma_and_colour_only_where_known() {
        let hdr = row_line(&probe_one(&asset("test_pq.mp4")));
        assert!(hdr.contains("\n  picture: 4:2:0 · BT.2020 PQ (HDR: exports tone-mapped to 8-bit SDR)"), "{hdr}");
        let ten = row_line(&probe_one(&asset("test_hevc10.mkv")));
        assert!(ten.contains("\n  picture: 10-bit 4:2:0 · BT.709 (assumed, untagged)"), "{ten}");
        assert_eq!(row_json(&probe_one(&asset("test_hevc10.mkv")))["chroma"], json!("4:2:0"));
        // Every codec names its chroma now, monochrome included: AV1 gray has no chroma plane.
        let gray = row_line(&probe_one(&asset("test_av1_gray.mkv")));
        assert!(gray.contains("\n  picture: 8-bit gray · BT.601 (assumed, untagged) full range"), "{gray}");
        assert_eq!(row_json(&probe_one(&asset("test_av1_gray.mkv")))["chroma"], json!("gray"));
        let vp9 = row_line(&probe_one(&asset("test_vp9_10.webm")));
        assert!(vp9.contains("\n  picture: 10-bit 4:2:0 "), "{vp9}");
    }

    #[test]
    fn a_song_with_cover_art_says_so_and_has_no_picture_track() {
        quiet();
        // An mp3 whose ID3 APIC frame carries the picture: the probe names it, and no picture track.
        let probed = probe_one(&asset("test_tone_cover.mpeg"));
        let line = row_line(&probed);
        assert!(line.contains("cover art only (not a picture track)"), "{line}");
        assert!(!line.contains("no picture") && !line.contains("cover art attached"), "{line}");
    }

    #[test]
    fn cover_art_beside_a_picture_track_is_said_after_the_picture() {
        quiet();
        // A picture track with a cover attached as a Matroska image attachment.
        let td = crate::testutil::TestDir::new("cover-and-picture");
        let mkv = td.path().join("cover.mkv");
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-t", "1", "-i"])
            .arg(asset("test_25fps.mp4"))
            .arg("-attach")
            .arg(asset("test_tone_cover.jpg"))
            .args(["-metadata:s:t", "mimetype=image/jpeg", "-c", "copy"])
            .arg(&mkv)
            .output()
            .expect("ffmpeg runs");
        assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
        let line = row_line(&probe_one(&mkv));
        assert!(line.contains("H.264") && line.contains("· cover art attached ·"), "{line}");
        assert!(!line.contains("cover art only") && !line.contains("no picture"), "{line}");
    }

    #[test]
    fn a_still_says_its_size_and_a_cover_jpeg_is_no_different() {
        assert!(row_line(&probe_one(&asset("test_still.png"))).contains("still image 640x360 · still"));
        assert!(row_line(&probe_one(&asset("test_tone_cover.jpg"))).contains("still image 64x64 · still"));
        assert_eq!(row_json(&probe_one(&asset("test_still.png")))["still_width"], json!(640));
    }

    #[test]
    fn an_edith_project_is_named_as_one_in_probe_and_import() {
        quiet();
        let row = probe_one(&asset("test_h264.edith"));
        let line = row_line(&row);
        assert_eq!(line, "test_h264.edith · an edith project (open it with project_open)");
        assert!(!line.contains("error"), "{line}");
        assert!(!line.contains("truncated"), "{line}");
        // Not a failed probe: the reply is not an error, and the row carries no error.
        let probed = probe(&mut Server::new(), &json!({"paths": [asset("test_h264.edith").display().to_string()]}));
        assert!(!probed.is_error, "{}", probed.text);
        assert_eq!(probed.text, line);
        assert_eq!(row_json(&row)["error"], json!(null));
        let td = TestDir::new("edith-import");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let imported = import(&mut server, &json!({"paths": [asset("test_h264.edith").display().to_string()]}));
        assert!(imported.is_error, "{}", imported.text);
        assert!(
            imported.text.contains("test_h264.edith is an edith project, not media: open it with project_open"),
            "{}",
            imported.text
        );
        drop(td);
    }

    #[test]
    fn an_anamorphic_picture_names_its_coded_size_and_pixel_aspect() {
        let line = row_line(&probe_one(&asset("test_anamorphic.mp4")));
        assert!(line.contains("1440x1080 (shown 1920x1080, pixel aspect 4:3)"), "{line}");
        let row = probe_one(&asset("test_anamorphic.mkv"));
        assert!(row_line(&row).contains("1440x1080 (shown 1920x1080, pixel aspect 4:3)"));
        assert_eq!(row_json(&row)["coded_width"], json!(1440));
    }

    #[test]
    fn square_pixels_show_the_size_alone() {
        let square = Coded { w: 1920, h: 1080, sar: (1, 1), turn: engine::demux::Rotation::None, colour: engine::colorspace::ColorDescription::default(), chroma: None };
        assert_eq!(square.text((1920, 1080)), None);
        let unstated = Coded { w: 1920, h: 1080, sar: (0, 0), turn: engine::demux::Rotation::None, colour: engine::colorspace::ColorDescription::default(), chroma: None };
        assert_eq!(unstated.text((1920, 1080)), None);
        let ratio = Coded { w: 1440, h: 1080, sar: (16, 12), turn: engine::demux::Rotation::None, colour: engine::colorspace::ColorDescription::default(), chroma: None };
        assert_eq!(ratio.text((1920, 1080)).as_deref(), Some("1440x1080 (shown 1920x1080, pixel aspect 4:3)"));
    }

    /// A turned picture shows its displayed size and says what is stored and how far it turns.
    #[test]
    fn a_rotated_picture_says_how_it_is_stored() {
        quiet();
        let tall = row_line(&probe_one(&asset("test_rotation90tall.mp4")));
        assert!(tall.contains("640x1280 (stored 1280x640, turned 90° counter-clockwise to stand upright; ffprobe rotation 90)"), "{tall}");
        let upright = row_line(&probe_one(&asset("test_av.mp4")));
        assert!(!upright.contains("turned"), "{upright}");
    }

    /// Each turn says the way edith turns the picture and ffprobe's `rotation` for the same file.
    #[test]
    fn turn_text_names_the_turn_and_ffprobes_tag() {
        let turned = |turn| Coded { w: 320, h: 180, sar: (1, 1), turn, colour: engine::colorspace::ColorDescription::default(), chroma: None };
        use engine::demux::Rotation;
        assert_eq!(turned(Rotation::None).turn_text(), None);
        assert_eq!(turned(Rotation::Cw90).turn_text().as_deref(), Some("(stored 320x180, turned 90° clockwise to stand upright; ffprobe rotation -90)"));
        assert_eq!(turned(Rotation::Cw180).turn_text().as_deref(), Some("(stored 320x180, turned 180° to stand upright; ffprobe rotation 180)"));
        assert_eq!(turned(Rotation::Cw270).turn_text().as_deref(), Some("(stored 320x180, turned 90° counter-clockwise to stand upright; ffprobe rotation 90)"));
    }

    /// The AAC profile follows the codec in the stream phrase, and the JSON row carries it beside the codec.
    #[test]
    fn the_aac_profile_follows_the_codec() {
        let mut st = Stream { kind: "audio", index: 0, track: None, codec: Some("aac".into()), codec_id: Some("mp4a".into()), profile: Some("HE-AACv2".into()), bit_depth: None, language: "und".into(), channels: Some(2), sample_rate: Some(22050), name: String::new() };
        assert_eq!(stream_text(&st), "audio track 0 aac HE-AACv2 (und) stereo, 22.05 kHz");
        assert_eq!(stream_json(&st)["profile"], "HE-AACv2");
        st.profile = None;
        assert_eq!(stream_text(&st), "audio track 0 aac (und) stereo, 22.05 kHz");
        assert!(stream_json(&st)["profile"].is_null());
    }

    /// The fixtures the engine reads an AAC profile from: LC and HE-AAC v1 made here, HE-AACv2 from
    /// edith_codecs. Each reads as its profile; the edith_codecs file is skipped where the checkout is not.
    #[test]
    fn an_aac_file_reports_its_profile() {
        let v2 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../edith_codecs/crates/ec-aac/tests/data/he/he_v2_stereo.m4a");
        if v2.exists() {
            let text: Vec<String> = other_streams(&v2, &engine::probe_media(&v2)).iter().map(stream_text).collect();
            assert_eq!(text, vec!["audio track 0 aac HE-AACv2 (und) stereo, 22.05 kHz"], "{text:?}");
        } else {
            eprintln!("skipped: no edith_codecs checkout beside this repo");
        }
    }

    /// A file already on the timeline is not added again: the reply names the source it is.
    #[test]
    fn importing_a_file_twice_says_it_is_already_imported() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let file = asset("test_av.mp4").display().to_string();
        let first = import(&mut server, &json!({"paths": [file.clone()]}));
        assert!(!first.is_error, "{}", first.text);
        let again = import(&mut server, &json!({"paths": [file]}));
        assert!(again.is_error, "{}", again.text);
        assert!(again.text.contains("test_av.mp4 is already imported as src:0"), "{}", again.text);
        assert!(again.text.contains("nothing was imported"), "{}", again.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1, "no second source");
    }

    /// A file named twice in one call is imported once, and the reply says the repeat was a duplicate.
    #[test]
    fn a_file_listed_twice_in_one_call_is_imported_once_and_said() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let file = asset("test_av.mp4").display().to_string();
        let r = import(&mut server, &json!({"paths": [file.clone(), file]}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1, "one source");
        assert!(
            r.text.contains("test_av.mp4 is listed twice: the second entry is a duplicate and nothing was added"),
            "{}",
            r.text
        );
        assert_eq!(r.structured.as_ref().unwrap()["failed"], 0);
    }

    /// A non-last source goes without force when no undo step can be lost, and the reply says so.
    #[test]
    fn removing_a_source_with_no_history_renumbers_without_force() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let session = server.session.as_mut().unwrap();
        session.import(&asset("test_av2.mp4")).expect("import");
        session.import(&asset("test_av.mp4")).expect("import");
        assert_eq!(session.undo_depth(), 0, "a direct import leaves no undo step");
        let dropped = remove(&mut server, &json!({"source": "src:0"}));
        assert!(!dropped.is_error, "{}", dropped.text);
        assert!(dropped.text.contains("There was no undo history to clear"), "{}", dropped.text);
        assert!(dropped.text.contains("src:1 test_av.mp4 is now src:0"), "{}", dropped.text);
        assert_eq!(server.session.as_ref().unwrap().sources().len(), 1);
    }

    /// The reply names each source whose number moved, and says so when the removed one was last.
    #[test]
    fn media_remove_names_the_sources_it_renumbers() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let session = server.session.as_mut().unwrap();
        session.import(&asset("test_av2.mp4")).expect("import");
        session.import(&asset("test_av.mp4")).expect("import");
        session.import(&asset("test_still.png")).expect("import the still");
        // Removing the first of three renumbers the other two; the reply names both.
        let first = remove(&mut server, &json!({"source": "src:0"}));
        assert!(!first.is_error, "{}", first.text);
        assert!(first.text.contains("src:1 test_av.mp4 is now src:0"), "{}", first.text);
        assert!(first.text.contains("src:2 test_still.png is now src:1"), "{}", first.text);
        assert!(!first.text.contains("no other source was renumbered"), "{}", first.text);
        // Removing the last one renumbers nothing.
        let last = remove(&mut server, &json!({"source": "src:1"}));
        assert!(!last.is_error, "{}", last.text);
        assert!(last.text.contains("no other source was renumbered"), "{}", last.text);
        assert!(!last.text.contains("is now src:"), "{}", last.text);
    }
}

#[cfg(test)]
mod relink_shift_tests {
    use super::*;
    use crate::testutil::TestDir;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn call(server: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
        (t.run)(server, &args)
    }

    /// A 25 fps source relinked to a 23.976 fps file of the same length: the rate is named and
    /// the clip on it. The length is under a frame apart, so no length warning.
    #[test]
    fn relink_to_another_frame_rate_names_the_clips_that_may_shift() {
        let td = TestDir::new("relink-shift");
        let project = td.path().join("shift.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [asset("test_25fps.mp4")]})).is_error);
        let placed = call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]}));
        assert!(!placed.is_error, "{}", placed.text);
        let r = call(&mut server, "media_relink", json!({"source": "src:0", "path": asset("test_23976fps.mp4")}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("23.976 fps against the 25 fps file it replaces"), "{}", r.text);
        assert!(r.text.contains("V1@0"), "{}", r.text);
        assert!(!r.text.contains("long against"), "a length under a frame is not warned: {}", r.text);
    }

    /// A relink is one undo step: the reply says undo takes it back, the ledger holds one entry for
    /// it, undo points the source at the old file again and redo points it at the new one.
    #[test]
    fn a_relink_is_one_undo_step_that_undo_and_redo_walk() {
        let td = TestDir::new("relink-undo");
        let project = td.path().join("undo.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [asset("test_av.mp4")]})).is_error);
        let old = server.session.as_ref().unwrap().sources()[0].path.clone();
        let ledger = server.ledger.len();
        let copy = td.path().join("moved.mp4");
        std::fs::copy(asset("test_av.mp4"), &copy).expect("a copy of the asset");
        let new = std::fs::canonicalize(&copy).expect("the copy");
        let r = call(&mut server, "media_relink", json!({"source": "src:0", "path": copy}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("undo takes it back"), "{}", r.text);
        assert_eq!(server.ledger.len(), ledger + 1, "one entry for the relink");
        let entry = server.ledger.last().expect("the entry");
        assert_eq!(entry.steps, 1, "one engine step");
        assert!(entry.label.starts_with("media_relink src:0"), "{}", entry.label);
        let u = call(&mut server, "undo", json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(server.session.as_ref().unwrap().sources()[0].path, old, "undo points back at the old file");
        let d = call(&mut server, "redo", json!({}));
        assert!(!d.is_error, "{}", d.text);
        assert_eq!(server.session.as_ref().unwrap().sources()[0].path, new, "redo re-points it");
    }

    /// project_collect copies the used files beside the project, relinks them in one undo step,
    /// and says what it skipped; a second run copies nothing.
    #[test]
    fn project_collect_copies_relinks_and_undoes_in_one_step() {
        let td = TestDir::new("collect");
        let project = td.path().join("film.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [asset("test_av.mp4")]})).is_error);
        let old = server.session.as_ref().unwrap().sources()[0].path.clone();
        let ledger = server.ledger.len();
        let r = call(&mut server, "project_collect", json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1 file(s) copied") && r.text.contains("undo takes the relink back"), "{}", r.text);
        let now = server.session.as_ref().unwrap().sources()[0].path.clone();
        assert_eq!(now.parent().unwrap().file_name().unwrap(), "film_media");
        assert!(now.is_file());
        assert_eq!(server.ledger.len(), ledger + 1);
        let again = call(&mut server, "project_collect", json!({}));
        assert!(again.text.contains("0 file(s) copied") && !again.text.contains("undo takes"), "{}", again.text);
        assert!(!call(&mut server, "undo", json!({})).is_error);
        assert_eq!(server.session.as_ref().unwrap().sources()[0].path, old);
        // Checkpoints use the functions the window uses.
        let c = call(&mut server, "checkpoint", json!({"name": "first-cut"}));
        assert!(!c.is_error, "{}", c.text);
        assert!(td.path().join("checkpoints/film/first-cut.edith").is_file());
    }

    /// A relink to the file the source already plays is no change, so it is no undo step and the
    /// reply does not offer one.
    #[test]
    fn a_relink_to_the_same_file_is_no_step() {
        let td = TestDir::new("relink-same");
        let project = td.path().join("same.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [asset("test_av.mp4")]})).is_error);
        let r = call(&mut server, "media_relink", json!({"source": "src:0", "path": asset("test_av.mp4")}));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("undo takes it back"), "{}", r.text);
        assert!(server.ledger.is_empty(), "a relink to the same file records no undo step");
    }

    /// The length warning is the same for a file that is longer and one that is shorter, and a
    /// rate change is named apart from it: a 30 s 24 fps file for a 5 s 30 fps one, and back.
    #[test]
    fn a_longer_or_shorter_file_is_warned_both_ways() {
        let five = Shape { fps: Some(30.0), secs: Some(5.0) };
        let longer = shape_warnings(0, "V1@0", five, Shape { fps: Some(24.0), secs: Some(30.0) }, 30.0);
        assert_eq!(longer.len(), 2, "{longer:?}");
        assert!(longer[0].contains("24 fps against the 30 fps file it replaces"), "{longer:?}");
        assert!(longer[1].contains("00:30.000 long against the 00:05.000"), "{longer:?}");
        assert!(longer.iter().all(|w| w.contains("V1@0")), "{longer:?}");
        let shorter = shape_warnings(0, "V1@0", Shape { fps: Some(24.0), secs: Some(30.0) }, five, 30.0);
        assert_eq!(shorter.len(), 2, "{shorter:?}");
        assert!(shorter[0].contains("30 fps against the 24 fps file it replaces"), "{shorter:?}");
        assert!(shorter[1].contains("00:05.000 long against the 00:30.000"), "{shorter:?}");
        // The same rate and a length within a frame: nothing to say.
        let quiet = shape_warnings(0, "x", five, Shape { fps: Some(30.0), secs: Some(5.01) }, 30.0);
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    /// An offline source relinked to a longer file of another rate is warned against the length
    /// its file had when the project was opened: the old file is gone, so its shape is remembered.
    #[test]
    fn an_offline_source_is_compared_with_the_shape_its_file_had() {
        let td = TestDir::new("relink-offline-shape");
        std::fs::copy(asset("test_25fps.mp4"), td.path().join("short.mp4")).expect("copy");
        let project = td.path().join("offline-shape.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [td.path().join("short.mp4")]})).is_error);
        let placed = call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]}));
        assert!(!placed.is_error, "{}", placed.text);
        std::fs::remove_file(td.path().join("short.mp4")).expect("remove");
        let r = call(&mut server, "media_relink", json!({"source": "src:0", "path": asset("test_av.mp4")}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("30 fps against the 25 fps file it replaces"), "{}", r.text);
        assert!(r.text.contains("00:05.000 long against the 00:02.000"), "{}", r.text);
    }

    /// A source offline when its project was opened has no shape to compare with: the reply says
    /// so, and does not invent a comparison.
    #[test]
    fn an_offline_source_never_read_says_it_was_not_compared() {
        let td = TestDir::new("relink-never-read");
        std::fs::copy(asset("test_25fps.mp4"), td.path().join("short.mp4")).expect("copy");
        let project = td.path().join("never-read.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [td.path().join("short.mp4")]})).is_error);
        let placed = call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]}));
        assert!(!placed.is_error, "{}", placed.text);
        assert!(!call(&mut server, "project_save", json!({})).is_error);
        std::fs::remove_file(td.path().join("short.mp4")).expect("remove");
        let mut fresh = Server::new();
        assert!(!call(&mut fresh, "project_open", json!({"path": project})).is_error);
        let r = call(&mut fresh, "media_relink", json!({"source": "src:0", "path": asset("test_av.mp4")}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("were never read, so the new file was not compared"), "{}", r.text);
    }

    /// A 5 s source of which a clip reads only its first 2 s relinks to a 2 s file: the clips
    /// read 60 source frames, so the file needs no more.
    #[test]
    fn a_clip_reading_part_of_a_source_relinks_to_a_file_of_that_part() {
        let td = TestDir::new("relink-part");
        let project = td.path().join("part.edith");
        let mut server = Server::new();
        assert!(!call(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        assert!(!call(&mut server, "media_import", json!({"paths": [asset("test_av.mp4")]})).is_error);
        let placed = call(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s", "length": "2s"}]}));
        assert!(!placed.is_error, "{}", placed.text);
        let r = call(&mut server, "media_relink", json!({"source": "src:0", "path": asset("test_hevc.mp4")}));
        assert!(!r.is_error, "{}", r.text);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// A subtitle-only Matroska is named as subtitles, with the way in, not as a refusal.
    #[test]
    fn a_subtitle_only_file_says_import_with_subtitles_import() {
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": [asset("test_subs.mks").display().to_string()]}));
        assert!(probed.text.contains("test_subs.mks · subtitles only: import with subtitles_import"), "{}", probed.text);
        assert!(!probed.is_error, "{}", probed.text);
        assert!(!probed.text.contains("refused"), "{}", probed.text);
        assert!(probed.text.contains("streams: subtitle #"), "{}", probed.text);
    }

    /// media_import of a subtitles-only file names subtitles_import, not an audio-track refusal.
    #[test]
    fn importing_a_subtitle_only_file_points_at_subtitles_import() {
        let td = crate::testutil::TestDir::new("media-subs-import");
        let mut server = Server::new();
        let run = |server: &mut Server, name: &str, args: Value| {
            let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
            (t.run)(server, &args)
        };
        let path = td.path().join("proj.edith");
        assert!(!run(&mut server, "project_new", json!({"path": path.display().to_string()})).is_error);
        let r = run(&mut server, "media_import", json!({"paths": [asset("test_subs.mks").display().to_string()]}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("test_subs.mks · subtitles only: import with subtitles_import"), "{}", r.text);
        assert!(!r.text.contains("has no audio track"), "{}", r.text);
    }

    /// A Matroska file's subtitle tracks are listed with their kind, codec, language and title,
    /// in container order, beside the picture's line.
    #[test]
    fn subtitle_tracks_are_listed_with_codec_and_language() {
        let path = asset("test_subs.mkv");
        let facts = engine::probe_media(&path);
        let streams = other_streams(&path, &facts);
        let subs: Vec<&Stream> = streams.iter().filter(|s| s.kind == "subtitle").collect();
        assert_eq!(subs.len(), 2, "{streams:?}");
        assert_eq!(subs[0].codec.as_deref(), Some("srt"));
        assert_eq!(subs[0].codec_id.as_deref(), Some("S_TEXT/UTF8"));
        assert_eq!(subs[0].language, "eng");
        assert_eq!(subs[1].language, "fra");
        assert_eq!(subs[1].name, "Signs");
        let mut server = Server::new();
        let probed = probe(&mut server, &json!({"paths": [path.display().to_string()]}));
        assert!(probed.text.contains("streams: subtitle #") && probed.text.contains("ass (fra) \"Signs\""), "{}", probed.text);
        let row = &probed.structured.as_ref().unwrap()["files"][0];
        assert_eq!(row["streams"][0]["kind"], json!("subtitle"));
        assert_eq!(row["streams"][1]["language"], json!("fra"));
    }

    /// A still has no sound or subtitle tracks, so it lists nothing extra.
    #[test]
    fn a_still_lists_no_streams() {
        let still = asset("test_still.png");
        let streams = other_streams(&still, &engine::probe_media(&still));
        assert!(streams.is_empty(), "{streams:?}");
    }

    /// An audio track without a codec or a title reads as `audio codec unknown (und)`.
    #[test]
    fn a_stream_without_codec_or_title_says_so() {
        let st = Stream { kind: "audio", index: 1, track: None, codec: None, codec_id: None, profile: None, bit_depth: None, language: "und".into(), channels: None, sample_rate: None, name: String::new() };
        assert_eq!(stream_text(&st), "audio track 1 codec unknown (und)");
        let named = Stream {
            kind: "subtitle",
            index: 3,
            track: Some(3),
            codec: Some("srt".into()),
            codec_id: Some("S_TEXT/UTF8".into()),
            profile: None,
            bit_depth: None,
            language: "eng".into(),
            channels: None,
            sample_rate: None,
            name: "Signs".into(),
        };
        assert_eq!(stream_text(&named), "subtitle #3 srt (eng) \"Signs\"");
    }

    /// The codec spellings a file gives are named plainly; any other passes through as written.
    #[test]
    fn codec_ids_are_named_in_plain_words() {
        for (raw, plain) in [
            ("A_OPUS", "opus"),
            ("Opus", "opus"),
            ("A_VORBIS", "vorbis"),
            ("A_AAC/MPEG4/LC", "aac"),
            ("mp4a", "aac"),
            ("fLaC", "flac"),
            ("A_PCM/INT/LIT", "pcm"),
            ("V_VP8", "vp8"),
            ("V_VP9", "vp9"),
            ("V_AV1", "av1"),
            ("V_MPEG4/ISO/AVC", "h264"),
            ("avc1", "h264"),
            ("V_MPEGH/ISO/HEVC", "hevc"),
            ("hvc1", "hevc"),
            ("S_TEXT/UTF8", "srt"),
            ("S_TEXT/ASS", "ass"),
            ("S_TEXT/WEBVTT", "webvtt"),
            ("tx3g", "mov_text"),
            ("S_HDMV/PGS", "pgs"),
            ("X_UNKNOWN", "X_UNKNOWN"),
        ] {
            assert_eq!(plain_codec(raw), plain, "{raw}");
        }
    }

    /// Audio tracks are numbered from 0 in file order, the numbers `audio_track` takes, and each
    /// carries the language the engine's stream list states for it.
    #[test]
    fn audio_tracks_are_numbered_from_zero_with_their_language() {
        let mp4 = asset("test_multiaudio.mp4");
        let tracks: Vec<(usize, String)> = other_streams(&mp4, &engine::probe_media(&mp4))
            .iter()
            .filter(|s| s.kind == "audio")
            .map(|s| (s.index, s.language.clone()))
            .collect();
        assert_eq!(tracks.len(), 3, "{tracks:?}");
        assert_eq!(tracks.iter().map(|t| t.0).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(tracks[1].1, "fra", "track 1 is the French one (ffprobe's stream 2): {tracks:?}");
        let mkv = asset("test_multiaudio.mkv");
        let text: Vec<String> = other_streams(&mkv, &engine::probe_media(&mkv)).iter().filter(|s| s.kind == "audio").map(stream_text).collect();
        assert_eq!(text, vec!["audio track 0 aac LC (eng) stereo, 44.1 kHz", "audio track 1 aac LC (fra) stereo, 44.1 kHz"], "{text:?}");
    }

    /// An mp4's streams are numbered in file order, video included, and its audio is named by
    /// its sample entry. The picture is not listed.
    #[test]
    fn mp4_streams_use_ffprobe_order_and_fourcc_names() {
        let path = asset("test_av.mp4");
        let facts = engine::probe_media(&path);
        let streams = other_streams(&path, &facts);
        assert!(streams.iter().all(|s| s.kind != "video"));
        let audio: Vec<&Stream> = streams.iter().filter(|s| s.kind == "audio").collect();
        assert_eq!(audio.len(), 1, "{streams:?}");
        assert_eq!(audio[0].codec_id.as_deref(), Some("mp4a"), "{streams:?}");
        assert_eq!(audio[0].codec.as_deref(), Some("aac"));
        assert_eq!(audio[0].index, 0, "the first audio track is track 0");
        assert!(!stream_text(audio[0]).contains("codec unknown"), "{}", stream_text(audio[0]));
    }
}
