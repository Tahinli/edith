//! Project templates: a named starting point for a new project. A template keeps the settings a
//! project is started with -- frame rate, canvas size, the lanes and their names, the caption
//! style and the export preset -- and, when asked, the clips too.
//!
//! A template file is `<name>.edith` in the user's data folder (`$XDG_DATA_HOME/edith/templates`,
//! else `~/.local/share/edith/templates`). Its header lines hold the settings, so a template
//! without clips needs no project body: a project file must name a source, and a template has
//! none. A template with clips carries the project file after the header:
//!
//! ```text
//! edith-template 1
//! export youtube_1080p        (or `export -` for none)
//! clips no                    (or `clips yes`, and the project file follows the header)
//! settings 1920 1080 30
//! lane video
//! lane audio Dialogue
//! capstyle none ffffff 0 0 3
//! ```
//!
//! The built-in templates ([`BUILTINS`]) are made in code and nothing is on disk for them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::captionanim::{CaptionAnim, CaptionStyle};
use crate::project::{Lane, LaneKind};
use crate::PlaybackSession;

/// The first line of a template file.
const MAGIC: &str = "edith-template 1";
/// The longest template name, in characters.
pub const NAME_MAX: usize = 40;
/// The export preset a template stores for "match the project" (the export sheet's Match project).
pub const MATCH_PROJECT: &str = "match_project";

/// A template made in code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Builtin {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// The export preset it starts on ([`crate::export_presets::by_id`]), `None` for the project's own.
    pub export: Option<&'static str>,
}

/// The templates that ship with edith, in menu order.
pub const BUILTINS: [Builtin; 3] = [
    Builtin { name: "YouTube 1080p 30", width: 1920, height: 1080, fps: 30.0, export: Some("youtube_1080p") },
    Builtin { name: "Vertical 1080\u{d7}1920 30", width: 1080, height: 1920, fps: 30.0, export: Some("shorts_reels") },
    Builtin { name: "Square 1080 30", width: 1080, height: 1080, fps: 30.0, export: None },
];

/// One row of the template list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub name: String,
    /// Shipped with edith rather than saved by a person.
    pub builtin: bool,
    /// Whether the template carries clips as well as its settings.
    pub clips: bool,
}

/// What a template opens as: the project and the export preset it names, if any.
pub struct Opened {
    pub session: PlaybackSession,
    pub export: Option<String>,
}

/// The folder templates are kept in: `$XDG_DATA_HOME/edith/templates`, else under `~/.local/share`.
pub fn dir() -> Option<PathBuf> {
    Some(dir_in(&data_home()?))
}

/// The templates folder under the data folder `data` (what [`dir`] is for the user's own).
pub fn dir_in(data: &Path) -> PathBuf {
    data.join("edith").join("templates")
}

/// The user's data folder: `$XDG_DATA_HOME` when it is an absolute path, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty() && Path::new(v).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local").join("share")))
}

/// Whether `name` can name a template: 1 to 40 characters once trimmed, no path separators or
/// control characters, not starting with a dot, and not the name of a built-in.
pub fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.chars().count() <= NAME_MAX
        && !n.starts_with('.')
        && !n.chars().any(|c| matches!(c, '/' | '\\' | '\0') || c.is_control())
        && !BUILTINS.iter().any(|b| b.name.eq_ignore_ascii_case(n))
}

/// The file a template named `name` is kept in.
pub fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.edith", name.trim()))
}

/// Every template: the built-ins first, then the saved ones by name. A folder that is missing is
/// no saved templates, and a file that does not read as one is left out.
pub fn list(dir: &Path) -> Vec<Info> {
    let mut out: Vec<Info> = BUILTINS.iter().map(|b| Info { name: b.name.to_string(), builtin: true, clips: false }).collect();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut saved: Vec<Info> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "edith"))
        .filter_map(|p| {
            let name = p.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).ok()?;
            let (head, _) = parse(&text).ok()?;
            Some(Info { name, builtin: false, clips: head.clips })
        })
        .collect();
    saved.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    out.extend(saved);
    out
}

/// Saves the open project as template `name` in `dir`, replacing one of that name. The settings
/// are always kept. `keep_clips` also keeps the clips and their sources, and is ignored for a
/// project with no sources (it has no clips to keep). `export` is the preset id the template starts
/// on, `None` for none. Returns whether a template of that name was replaced.
pub fn save(dir: &Path, s: &PlaybackSession, name: &str, keep_clips: bool, export: Option<&str>) -> crate::Result<bool> {
    if !valid_name(name) {
        return Err(format!("template name `{}` is not usable: 1 to 40 characters, no slashes, not a built-in's name", name.trim()).into());
    }
    if let Some(id) = export
        && id != MATCH_PROJECT
        && crate::export_presets::by_id(id).is_none()
    {
        return Err(format!("export preset `{id}` is not one edith has").into());
    }
    let clips = keep_clips && !s.sources().is_empty();
    let (w, h) = s.resolution();
    let lanes = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .map(|l| (l.kind, s.lane_name(l).unwrap_or("").to_string()))
        .collect();
    let head = Head {
        export: export.map(str::to_string),
        clips,
        width: w,
        height: h,
        fps: s.meta().frame_rate,
        lanes,
        caption: s.caption_style(),
    };
    let mut text = render(&head).into_bytes();
    if clips {
        text.extend_from_slice(&project_bytes(s, dir)?);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let path = file(dir, name);
    let replaced = path.exists();
    crate::edith::write_atomic(&path, &text)?;
    Ok(replaced)
}

/// The project a new one starts as: the built-in `name` made in code, or the saved template
/// `name` in `dir`. Nothing is written. A template without clips is a project with no source, so
/// it is not a project file yet.
pub fn open(dir: &Path, name: &str) -> crate::Result<Opened> {
    let (head, body) = load(dir, name)?;
    let session = match body {
        Some(bytes) => {
            let work = scratch_dir(dir);
            std::fs::create_dir_all(&work).map_err(|e| format!("cannot make {}: {e}", work.display()))?;
            let scratch = work.join("body.edith");
            let opened = std::fs::write(&scratch, &bytes)
                .map_err(|e| format!("cannot read template {}: {e}", name.trim()))
                .and_then(|()| PlaybackSession::open_project(&scratch).map_err(|e| e.to_string()));
            let _ = std::fs::remove_dir_all(&work);
            opened.map_err(|e| format!("template {} does not open: {e}", name.trim()))?
        }
        None => {
            let mut s = PlaybackSession::empty(head.width, head.height, head.fps)?;
            apply_lanes(&mut s, &head);
            s.set_caption_style(head.caption);
            s
        }
    };
    Ok(Opened { session, export: head.export })
}

/// Puts the lanes, lane names and caption style of template `name` onto `s`, the way a project
/// that has footage takes them when its first media arrives. Its canvas and rate are the caller's.
/// Returns the export preset the template names. A template with clips is refused: its clips
/// come with [`write_project`] only.
pub fn apply(dir: &Path, name: &str, s: &mut PlaybackSession) -> crate::Result<Option<String>> {
    let (head, body) = load(dir, name)?;
    if body.is_some() {
        return Err(format!("template {} has clips, so it starts a project of its own", name.trim()).into());
    }
    apply_lanes(s, &head);
    s.set_caption_style(head.caption);
    Ok(head.export)
}

/// Writes the project file template `name` starts as to `target`, for a template that carries
/// clips (its sources come with it). Returns the export preset the template names. A template
/// without clips is refused: its file appears with the first footage ([`apply`]).
pub fn write_project(dir: &Path, name: &str, target: &Path) -> crate::Result<Option<String>> {
    let (head, body) = load(dir, name)?;
    let Some(body) = body else {
        return Err(format!("template {} has no clips: its project is made with the first footage", name.trim()).into());
    };
    crate::edith::write_atomic(target, &body)?;
    Ok(head.export)
}

/// The settings a template keeps, read off its header.
struct Head {
    export: Option<String>,
    clips: bool,
    width: u32,
    height: u32,
    fps: f64,
    lanes: Vec<(LaneKind, String)>,
    caption: CaptionStyle,
}

fn render(h: &Head) -> String {
    let mut out = format!(
        "{MAGIC}\nexport {}\nclips {}\nsettings {} {} {}\n",
        h.export.as_deref().unwrap_or("-"),
        if h.clips { "yes" } else { "no" },
        h.width,
        h.height,
        h.fps
    );
    for (kind, name) in &h.lanes {
        // A name is one line: a control character in it would end the lane's line.
        let name: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let line = if name.is_empty() { format!("lane {}\n", kind_word(*kind)) } else { format!("lane {} {name}\n", kind_word(*kind)) };
        out.push_str(&line);
    }
    let [r, g, b] = h.caption.highlight;
    out.push_str(&format!(
        "capstyle {} {r:02x}{g:02x}{b:02x} {} {} {}\n",
        h.caption.anim.name(),
        u8::from(h.caption.hl_box),
        u8::from(h.caption.uppercase),
        h.caption.words_per_line
    ));
    out
}

fn kind_word(kind: LaneKind) -> &'static str {
    match kind {
        LaneKind::Video => "video",
        LaneKind::Audio => "audio",
        LaneKind::Subtitle => "subtitle",
    }
}

/// The header of a template, and the project file bytes after it when it has clips.
fn parse(text: &str) -> Result<(Head, Option<usize>), String> {
    let mut lines = Vec::new();
    let mut body_at = None;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        if line.starts_with("edith ") {
            body_at = Some(at);
            break;
        }
        lines.push(line.trim_end_matches(['\n', '\r']));
        at += line.len();
    }
    let mut it = lines.into_iter();
    if it.next() != Some(MAGIC) {
        return Err("not an edith template".into());
    }
    let export = it.next().and_then(|l| l.strip_prefix("export ")).ok_or("no export line")?;
    let clips = match it.next().and_then(|l| l.strip_prefix("clips ")) {
        Some("yes") => true,
        Some("no") => false,
        _ => return Err("no clips line".into()),
    };
    if clips != body_at.is_some() {
        return Err("its clips line and its project do not agree".into());
    }
    let mut head = Head {
        export: Some(export).filter(|e| *e != "-").map(str::to_string),
        clips,
        width: 0,
        height: 0,
        fps: 0.0,
        lanes: Vec::new(),
        caption: CaptionStyle::default(),
    };
    let mut have_settings = false;
    for line in it {
        if let Some(rest) = line.strip_prefix("settings ") {
            let f: Vec<&str> = rest.split(' ').collect();
            let (Some(w), Some(h), Some(fps)) = (f.first(), f.get(1), f.get(2)) else {
                return Err("a settings line needs a width, height and rate".into());
            };
            head.width = w.parse().map_err(|_| "bad width")?;
            head.height = h.parse().map_err(|_| "bad height")?;
            head.fps = fps.parse().map_err(|_| "bad rate")?;
            have_settings = true;
        } else if let Some(rest) = line.strip_prefix("lane ") {
            let (kind, name) = rest.split_once(' ').unwrap_or((rest, ""));
            let kind = match kind {
                "video" => LaneKind::Video,
                "audio" => LaneKind::Audio,
                other => return Err(format!("a lane of kind {other} is not one a template holds")),
            };
            head.lanes.push((kind, name.to_string()));
        } else if let Some(rest) = line.strip_prefix("capstyle ") {
            let f: Vec<&str> = rest.split(' ').collect();
            let (Some(anim), Some(hex), Some(bx), Some(up), Some(wpl)) = (f.first(), f.get(1), f.get(2), f.get(3), f.get(4)) else {
                return Err("a capstyle line needs five fields".into());
            };
            let channel = |i: usize| hex.get(i..i + 2).and_then(|c| u8::from_str_radix(c, 16).ok());
            let (Some(r), Some(g), Some(b)) = (channel(0), channel(2), channel(4)) else {
                return Err("a capstyle colour is six hex digits".into());
            };
            head.caption = CaptionStyle {
                anim: CaptionAnim::from_name(anim).ok_or("unknown caption animation")?,
                highlight: [r, g, b],
                hl_box: *bx == "1",
                uppercase: *up == "1",
                words_per_line: wpl.parse().map_err(|_| "bad words per line")?,
            };
        }
    }
    if !have_settings {
        return Err("no settings line".into());
    }
    Ok((head, body_at))
}

/// The header and, for a template with clips, the project file bytes after it.
fn load(dir: &Path, name: &str) -> crate::Result<(Head, Option<Vec<u8>>)> {
    if let Some(b) = BUILTINS.iter().find(|b| b.name == name) {
        let head = Head {
            export: b.export.map(str::to_string),
            clips: false,
            width: b.width,
            height: b.height,
            fps: b.fps,
            lanes: vec![(LaneKind::Video, String::new()), (LaneKind::Audio, String::new())],
            caption: CaptionStyle::default(),
        };
        return Ok((head, None));
    }
    let path = file(dir, name);
    let bytes = std::fs::read(&path).map_err(|e| format!("no template named {}: {e}", name.trim()))?;
    let text = String::from_utf8_lossy(&bytes);
    let (head, body_at) = parse(&text).map_err(|e| format!("template {}: {e}", name.trim()))?;
    Ok((head, body_at.map(|at| bytes[at..].to_vec())))
}

/// Adds the lanes of `h` that `s` lacks, in order, and names each lane. Lanes already there keep
/// their clips; only the names are set.
fn apply_lanes(s: &mut PlaybackSession, h: &Head) {
    for (i, (kind, name)) in h.lanes.iter().enumerate() {
        let ord = h.lanes[..i].iter().filter(|(k, _)| k == kind).count();
        let lane = Lane::new(*kind, ord);
        if !s.lanes().contains(&lane) {
            let _ = s.add_lane(*kind);
        }
        if !name.is_empty() {
            let _ = s.set_lane_name(lane, name);
        }
    }
}

/// The project file bytes of `s`, written through a scratch file.
/// The project a clips template's body is written as, through a scratch folder of its own
/// inside `dir`. Sources are written relative to the folder their project file is in when they sit
/// inside it, so a folder that holds no media keeps every source path absolute: the body then
/// reads the same from wherever it is copied to.
fn project_bytes(s: &PlaybackSession, dir: &Path) -> crate::Result<Vec<u8>> {
    let work = scratch_dir(dir);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot make {}: {e}", work.display()))?;
    let path = work.join("body.edith");
    let written = s.save_project(&path);
    let bytes = written.and_then(|()| std::fs::read(&path).map_err(Into::into));
    let _ = std::fs::remove_dir_all(&work);
    bytes
}

/// A scratch folder name no other call uses, inside the templates folder `dir`. Hidden, so a
/// listing of the folder skips it.
fn scratch_dir(dir: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(".work-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    fn media() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4")
    }

    /// One test for the whole feature: built-ins, settings-only templates (with lanes, names and
    /// captions applied to footage), clips templates with absolute sources, and refusals.
    #[test]
    fn a_template_keeps_its_settings_and_clips_and_refuses_bad_input() {
        let dir = Scratch::dir("tpl_round");
        let vertical = open(&dir, "Vertical 1080×1920 30").expect("built-in opens");
        assert_eq!((vertical.session.resolution(), vertical.session.meta().frame_rate), ((1080, 1920), 30.0));
        assert!(open(&dir, "Nope").is_err());

        let mut s = PlaybackSession::open(media()).expect("open test asset");
        s.set_lane_name(Lane::A1, "Voice").expect("names a lane");
        let _ = s.add_lane(LaneKind::Audio);
        let styled = CaptionStyle { anim: CaptionAnim::Pop, uppercase: true, ..CaptionStyle::default() };
        assert!(s.set_caption_style(styled));
        let clips = crate::versions::clip_count(&s);
        assert!(!save(&dir, &s, "Voices", false, Some("podcast")).expect("saves"));
        let o = open(&dir, "Voices").expect("opens");
        assert!(o.session.is_empty() && o.export.as_deref() == Some("podcast"));
        assert_eq!(o.session.caption_style(), styled);
        let mut fresh = PlaybackSession::open(media()).expect("open test asset");
        apply(&dir, "Voices", &mut fresh).expect("applies");
        assert_eq!(fresh.lane_name(Lane::A1), Some("Voice"));
        assert_eq!(crate::versions::clip_count(&fresh), clips);

        save(&dir, &s, "Full", true, None).expect("saves with clips");
        let text = std::fs::read_to_string(file(&dir, "Full")).expect("reads");
        let media_path = media().canonicalize().expect("media");
        assert!(text.contains(&media_path.display().to_string()), "{text}");
        let target = dir.join("Full-copy.edith");
        write_project(&dir, "Full", &target).expect("writes");
        assert_eq!(crate::versions::clip_count(&PlaybackSession::open_project(&target).expect("opens")), clips);

        assert!(save(&dir, &s, "a/b", false, None).is_err());
        assert!(save(&dir, &s, "Ok", false, Some("no_such_preset")).is_err());
        std::fs::write(file(&dir, "junk"), "edith 51\n").unwrap();
        assert!(!list(&dir).iter().any(|i| i.name == "junk"));
    }
}
