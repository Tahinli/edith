//! Project tools: new, open, save, status, settings.
//!
//! The undo ledger: `server.ledger` holds one entry per agent-visible undo step,
//! the number of engine snapshots that step pushed. Any tool that may change the
//! engine's push count (`history_pushes`, which keeps rising at the undo cap)
//! measures it before and after and calls [`note_pushes`].
//! The project is dirty when the undo depth differs from `saved_depth`, the ledger's top
//! entry is not the one it was at the save (`saved_top`), or a source came or went.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use engine::limiter::Limiter;
use engine::project::{Lane, LaneKind, Rate, STILL_MAX_SECS, STILL_MIN_SECS};
use engine::tonemap::Preset;
use engine::export::EncoderSeat;
use engine::PlaybackSession;
use serde_json::{json, Map, Value};

use crate::parse;
use crate::server::{arg_str, opt_bool, opt_f64, opt_str, tri, Reply, Server, Tool};
use crate::tools::media::{not_a_file, not_found};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "project_new",
            description: "Start an empty project at a new path. Nothing is written until project_save. Refused, before anything is created, when an earlier session's unsaved edits sit in the path's autosave: the reply names them and offers project_open {recover:true} to continue from them, or overwrite:true to start fresh (the autosave is set aside as a dated copy; the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted).",
            schema: || {
                schema(
                    json!({
                        "path": {"type": "string", "description": "Where the .edith file will be saved. Must not exist yet."},
                        "resolution": {"type": "string", "description": "Canvas size as WIDTHxHEIGHT, e.g. 1920x1080. Default 1920x1080."},
                        "fps": {"type": "number", "description": "Frame rate of the timeline. Default 30."},
                        "template": {"type": "string", "examples": ["Vertical 1080×1920 30"], "description": "Start from a template by name: a built-in (YouTube 1080p 30, Vertical 1080×1920 30, Square 1080 30) or one saved with File > Save as template (see templates). Sets the canvas size, rate, lanes, lane names and caption style; resolution and fps are then refused."},
                        "discard": {"type": "boolean", "description": "Replace the open project even if it has unsaved changes."},
                        "overwrite": {"type": "boolean", "description": "Let path name an existing file, or a path with an earlier session's unsaved autosave. The autosave is set aside as a dated copy at once. The file is replaced by project_save (which needs its own force:true if the file changed on disk after this call). Without overwrite, an existing path, or one with such an autosave, is refused."}
                    }),
                    &["path"],
                )
            },
            run: project_new,
        },
        Tool {
            name: "templates",
            description: "The project templates: the built-ins and the ones saved with File > Save as template, each with whether it keeps clips. Pass a name as project_new's template.",
            schema: || schema(json!({}), &[]),
            run: templates_list,
        },
        Tool {
            name: "project_open",
            description: "Open a .edith project, or a media file as a new project (its project is <stem>.edith next to it, unsaved). When an autosave beside the project holds unsaved edits from an earlier session, the open is refused before anything is loaded or written, and says what those edits are. Choose up front: recover: true continues from them: they open on the saved file as one undo step, so undo takes them back to the saved timeline (the project stays unsaved until project_save; a never-saved project has nothing for undo to return to), discard_autosave: true opens the saved file and sets the autosave aside as a dated copy (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). Older dated autosaves are listed in the reply; recover: \"<file name or time stamp>\" loads one of those. Returns the status.",
            schema: || {
                schema(
                    json!({
                        "path": {"type": "string", "description": "A .edith file, or a video/audio file to start a project from."},
                        "discard": {"type": "boolean", "description": "Replace the open project even if it has unsaved changes."},
                        "recover": {"type": ["boolean", "string"], "description": "true: continue from the autosave (<name>.autosave.edith) beside the project: the saved file opens and the autosave's edits go on top as one undo step; required when that autosave holds an earlier session's unsaved edits. A string: the dated autosave with that file name or time stamp (as listed in the reply, local time). The project stays unsaved until project_save. When this session has unsaved edits, recover is refused unless discard: true, which drops those edits (or project_save {discard_autosave:true} keeps them first)."},
                        "discard_autosave": {"type": "boolean", "description": "Open the saved file and set the autosave beside it aside without loading it: it is renamed to a dated copy (<stem>.autosave-<stamp>.edith, listed in the reply), so its edits are not loaded and the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). Required, with recover, to open past an earlier session's autosave."},
                        "search": {"type": "array", "items": {"type": "string"}, "description": "Folders searched (recursively) for the files of offline sources, matched by file name. A name found exactly once is relinked; the reply says which."}
                    }),
                    &["path"],
                )
            },
            run: project_open,
        },
        Tool {
            name: "project_save",
            description: "Save the project. With a different path it is save-as. Refuses to overwrite a file this session did not open or save, or one that changed on disk since, unless force: true. Refuses while an autosave beside the project that this session did not load holds edits, unless discard_autosave: true: that saves this session's edits and sets the autosave aside as a dated copy (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). project_open settles an earlier session's autosave before it opens the project, so this refusal is for one that appeared while this session ran.",
            schema: || {
                schema(
                    json!({
                        "path": {"type": "string", "description": "Target .edith path. Default: the current project path."},
                        "force": {"type": "boolean", "description": "Replace the target even if it is not this project's file or changed on disk since it was last opened or saved here."},
                        "discard_autosave": {"type": "boolean", "description": "Save this session's edits anyway: the autosave that appeared beside the project holds edits this save does not have, so it is set aside as a dated copy (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). Use it when project_save refuses over that autosave."}
                    }),
                    &[],
                )
            },
            run: project_save,
        },
        Tool {
            name: "project_collect",
            description: "Copy every file the project plays into one folder and point the project at the copies, so the folder and the project can be moved or archived together. Default folder: <project stem>_media next to the project file. Generated sources (solids, bars, adjustment layers) are skipped, a source whose file is missing is skipped and named, one already in the folder stays, and a name already taken by another file gets -2, -3. The same bytes already in the folder are reused. The relink is one undo step and the project is unsaved until project_save. Nothing changes if a copy fails.",
            schema: || {
                schema(
                    json!({
                        "folder": {"type": "string", "description": "Where the copies go (made if missing). Default: <project stem>_media beside the project file."}
                    }),
                    &[],
                )
            },
            run: project_collect,
        },
        Tool {
            name: "project_export_timeline",
            description: "Write the active sequence for another editor. format \"otio\" writes OpenTimelineIO (.otio): tracks, clips with their file paths, gaps, transitions and markers. \"fcpxml\" writes Final Cut Pro XML 1.10, which Final Cut Pro, DaVinci Resolve and Premiere read. \"edl\" writes a CMX3600 EDL: V1, and audio on A1 and A2. Subtitles are left out, and the reply says what else was left out. Default path: beside the project file, named after it with the format's extension. Only the file is written; the project does not change.",
            schema: || {
                schema(
                    json!({
                        "format": {"type": "string", "enum": ["otio", "fcpxml", "edl"], "description": "Which format to write."},
                        "path": {"type": "string", "description": "Where the file goes. Default: beside the project file, named after it."}
                    }),
                    &["format"],
                )
            },
            run: project_export_timeline,
        },
        Tool {
            name: "project_import_timeline",
            description: "Read an OpenTimelineIO (.otio) file as a new sequence of this project and switch to it. Each clip is relinked by its file path; a file that is missing becomes an offline clip. One undo step; unsaved until project_save.",
            schema: || {
                schema(
                    json!({
                        "path": {"type": "string", "description": "The .otio file to read."}
                    }),
                    &["path"],
                )
            },
            run: project_import_timeline,
        },
        Tool {
            name: "project_status",
            description: "Project path, saved or dirty, canvas size, frame rate, duration, lanes with clip counts, sources, undo and redo steps, running exports, and settings.",
            schema: || schema(json!({}), &[]),
            run: project_status,
        },
        Tool {
            name: "project_settings",
            description: "Change project-wide settings: canvas resolution, frame rate, tone mapping, proxies, encoder seat, limiter, caption style, notes, still length. Reports each change and each refusal.",
            schema: || {
                schema(
                    json!({
                        "resolution": {"type": "string", "description": "Canvas size as WIDTHxHEIGHT, e.g. 1280x720."},
                        "fps": {"type": "number", "description": "Timeline frame rate, e.g. 25 or 29.97. Refused while a locked lane has clips the retime would move, unless force is true."},
                        "force": {"type": "boolean", "description": "With fps: move the clips of locked lanes too."},
                        "tonemap": {"type": "string", "enum": ["reference", "standard", "vivid"], "description": "HDR to SDR rendition."},
                        "notes": {"type": "string", "examples": ["Cut the intro to 30 s"], "description": "The project's notes: free text, several lines allowed (a newline is \\n in JSON). An empty string clears them. Saved with the project; one undo step."},
                        "still_secs": {"type": "number", "description": "How long a still image is placed for, in seconds: 0.5 to 600 (default 5). Stills placed after this change take it; clips already placed keep theirs. Saved with the project; one undo step."},
                        "proxies": {"type": "boolean", "description": "Play and edit through proxy files."},
                        "auto_proxies": {"type": "boolean", "description": "Make proxies automatically for heavy sources."},
                        "encoder": {"type": "string", "enum": ["auto", "hardware", "software"], "description": "Which encoder export uses."},
                        "captions": {
                            "type": "object",
                            "description": "The project's caption style, the default for burn_captions (which can override each key): animation (none, karaoke, pop, bounce, word), highlight (#rrggbb), box (highlight as a rounded box), uppercase, words_per_line (0 to 12). Saved with the project; one undo step.",
                            "properties": {
                                "animation": {"type": "string", "enum": ["none", "karaoke", "pop", "bounce", "word"]},
                                "highlight": {"type": "string", "description": "#rrggbb"},
                                "box": {"type": "boolean"},
                                "uppercase": {"type": "boolean"},
                                "words_per_line": {"type": "integer", "minimum": 0, "maximum": 12}
                            },
                            "additionalProperties": false
                        },
                        "limiter": {
                            "type": "object",
                            "description": "Master limiter.",
                            "properties": {
                                "on": {"type": "boolean", "description": "Turn the limiter on or off."},
                                "ceiling_db": {"type": "number", "description": "Ceiling in dBTP, -24 to 0: the highest true peak the output may reach, between samples included."}
                            }
                        }
                    }),
                    &[],
                )
            },
            run: project_settings,
        },
    ]
}

/// `project_collect {folder?}`: [`engine::collect`] over the open project's sources.
fn project_collect(server: &mut Server, args: &Value) -> Reply {
    let folder = match opt_str(args, "folder") {
        Some(f) => expand_home(f.trim()),
        None => match server.path.as_deref() {
            Some(p) => engine::collect::default_folder(p),
            None => return Reply::err("the project has no path yet: project_save it first, or give `folder`"),
        },
    };
    let s = tri!(server.session());
    let plan = engine::collect::plan(s.sources(), &folder);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let done = match engine::collect::copy_all(&plan, &cancel, &mut |_, _| {}) {
        Ok(done) => done,
        Err(e) => return Reply::err(format!("project_collect failed, nothing changed: {e:?}")),
    };
    let before = s.history_pushes();
    let moved = s.relink_all(&done.pairs);
    let steps = s.history_pushes().saturating_sub(before);
    let mut lines = vec![format!(
        "collected into {}: {} file(s) copied, {}; {moved} source(s) relinked",
        folder.display(),
        done.files,
        engine::collect::size_text(done.bytes)
    )];
    let reused = plan.items.iter().filter(|i| i.present).count();
    if reused > 0 {
        lines.push(format!("{reused} file(s) were already in the folder with the same bytes and were reused"));
    }
    if plan.already > 0 {
        lines.push(format!("{} source(s) were already in the folder", plan.already));
    }
    if plan.generated > 0 {
        lines.push(format!("{} generated source(s) skipped (they are not files)", plan.generated));
    }
    for p in &plan.offline {
        lines.push(format!("warning: skipped, file missing: {}", p.display()));
    }
    if steps > 0 {
        server.saved_sources = None;
        crate::tools::project::note_pushes_as(server, before, "project_collect");
        crate::tools::media::remember_sources(server);
        lines.push("undo takes the relink back".to_string());
    }
    lines.push("unsaved until project_save".to_string());
    Reply::ok(lines.join("\n")).with(json!({
        "folder": folder.display().to_string(),
        "copied": done.files,
        "bytes": done.bytes,
        "relinked": moved,
        "generated_skipped": plan.generated,
        "missing": plan.offline.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
    }))
}

/// `project_export_timeline {format, path?}`: [`engine::interchange`] of the active sequence, written to a file.
fn project_export_timeline(server: &mut Server, args: &Value) -> Reply {
    let format = tri!(arg_str(args, "format"));
    let ext = match format {
        "otio" | "fcpxml" | "edl" => format,
        other => return Reply::err(format!("format `{other}` is not one of otio, fcpxml, edl")),
    };
    let path = match opt_str(args, "path") {
        Some(raw) => expand_home(raw.trim()),
        None => match server.path.as_deref() {
            Some(p) => p.with_extension(ext),
            None => return Reply::err("the project has no path yet: give `path`, or project_save first"),
        },
    };
    let s = tri!(server.session_ref());
    let t = s.timeline_snapshot();
    let text = match format {
        "otio" => engine::interchange::otio::write(&t),
        "fcpxml" => engine::interchange::fcpxml::write(&t),
        _ => engine::interchange::edl::write(&t),
    };
    if let Err(e) = std::fs::write(&path, text) {
        return Reply::err(format!("could not write {}: {e}", path.display()));
    }
    let mut lines = vec![format!("wrote {} ({} track(s), {} marker(s))", path.display(), t.tracks.len(), t.markers.len())];
    lines.extend(t.notes.iter().cloned());
    Reply::ok(lines.join("\n")).with(json!({
        "path": path.display().to_string(),
        "format": format,
        "tracks": t.tracks.len(),
        "markers": t.markers.len(),
        "notes": t.notes,
    }))
}

/// `project_import_timeline {path}`: an OpenTimelineIO file becomes a new sequence, one undo step.
fn project_import_timeline(server: &mut Server, args: &Value) -> Reply {
    let path = tri!(path_arg(args, "path"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => return Reply::err(format!("could not read {}: {e}", path.display())),
    };
    let s = tri!(server.session());
    let fps = s.timeline_frame_rate();
    let t = match engine::interchange::otio::read(&text, fps) {
        Ok(t) => t,
        Err(e) => return Reply::err(format!("project_import_timeline failed, nothing changed: {e}")),
    };
    let before = s.history_pushes();
    let id = match s.import_timeline(&t) {
        Ok(id) => id,
        Err(e) => return Reply::err(format!("project_import_timeline failed, nothing changed: {e}")),
    };
    let steps = s.history_pushes().saturating_sub(before);
    let clips: usize = t
        .tracks
        .iter()
        .map(|tr| tr.items.iter().filter(|i| matches!(i, engine::interchange::Item::Clip(_))).count())
        .sum();
    let mut lines = vec![format!(
        "imported {} as sequence {id}: {} track(s), {clips} clip(s), {} marker(s); it is the active sequence",
        t.name,
        t.tracks.len(),
        t.markers.len()
    )];
    lines.extend(t.notes.iter().cloned());
    if steps > 0 {
        server.saved_sources = None;
        crate::tools::project::note_pushes_as(server, before, "project_import_timeline");
        crate::tools::media::remember_sources(server);
        lines.push("undo takes the import back".to_string());
    }
    lines.push("unsaved until project_save".to_string());
    Reply::ok(lines.join("\n")).with(json!({"sequence": id, "name": t.name, "tracks": t.tracks.len(), "clips": clips, "markers": t.markers.len()}))
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

// ---- shared helpers (also used by the media tools) -------------------------

/// Expands a leading `~` or `~/` to the home directory.
pub fn expand_home(raw: &str) -> PathBuf {
    if (raw == "~" || raw.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut p = PathBuf::from(home);
        if let Some(rest) = raw.strip_prefix("~/") {
            p.push(rest);
        }
        return p;
    }
    PathBuf::from(raw)
}

/// Whether the open project has changes since it was last saved or opened.
/// A project never saved (`saved_depth` is `None`) is dirty. Undo and redo move the ledger's
/// top entry, so back at the saved position the project is clean again; the engine's push
/// count is not used, since undo leaves it raised.
pub fn dirty(server: &Server) -> bool {
    match &server.session {
        None => false,
        Some(s) => {
            Some(s.undo_depth()) != server.saved_depth
                || server.ledger_top() != server.saved_top
                || Some(s.sources().len()) != server.saved_sources
                || !server.load_fixes.is_empty()
        }
    }
}

/// Whether this session changed the project since it opened it, saved it, or (for a media file
/// started as a project) since that start: an engine snapshot pushed, a source added or removed,
/// or the undo depth moved. Loads that only fixed or relinked what was on disk do not count, and
/// neither does a recovery, whose edits are already in an autosave. A session that made no edit
/// never writes an autosave.
fn edited_this_session(server: &Server, session: &PlaybackSession) -> bool {
    if let Some((depth, pushes, sources)) = server.media_open {
        return (session.undo_depth(), session.history_pushes(), session.sources().len()) != (depth, pushes, sources);
    }
    match (server.saved_pushes, server.saved_sources) {
        (Some(pushes), Some(sources)) => {
            session.history_pushes() != pushes
                || session.sources().len() != sources
                || server.saved_depth.is_some_and(|d| session.undo_depth() != d)
        }
        // A project never saved: whatever it holds was made in this session.
        _ => true,
    }
}

/// Records the snapshots an engine call pushed since `before` (an engine
/// `history_pushes` reading taken before the change), as one ledger entry.
/// Exact at the undo cap, where the depth no longer grows.
pub fn note_pushes(server: &mut Server, before: u64) {
    note_pushes_as(server, before, "edit");
}

/// `note_pushes` with the ledger label `label`, which undo and redo report.
pub fn note_pushes_as(server: &mut Server, before: u64, label: &str) {
    let after = match &server.session {
        Some(s) => s.history_pushes(),
        None => return,
    };
    if after > before {
        record(server, (after - before) as usize, label);
    }
}

fn record(server: &mut Server, steps: usize, label: &str) {
    server.ledger.push(crate::server::Entry::new(steps, label));
    server.history_lost_on_open = false;
    server.redo_ledger.clear();
    server.trim_ledger();
}

fn path_arg(args: &Value, name: &str) -> Result<PathBuf, Reply> {
    Ok(absolute(&expand_home(arg_str(args, name)?)))
}

/// `path` as an absolute path: a relative one is resolved against the server's working
/// directory, so every reply can name the file it used. Nothing is canonicalized, since the
/// file may not exist yet.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
}

/// How many dated autosave copies (`<stem>.autosave-<stamp>.edith`) a project keeps. Older
/// ones are deleted when a newer one is set aside.
const DATED_KEEP: usize = 3;

/// A dated copy written this recently is never deleted, however many newer copies there are: its
/// edits may be the only copy of work a person has not yet looked at again.
const DATED_RECENT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Records that this session opened or saved `path`, with the mtime it has now.
fn note_own(server: &mut Server, path: &Path) {
    let c = canon(path);
    let mt = mtime(path);
    server.own_files.retain(|(p, _)| *p != c);
    server.own_files.push((c, mt));
}

/// The mtime this session last left `path` with, when the session opened or saved it.
fn own_mtime(server: &Server, path: &Path) -> Option<Option<SystemTime>> {
    let c = canon(path);
    server.own_files.iter().find(|(p, _)| *p == c).map(|(_, mt)| *mt)
}

fn guard_dirty(server: &Server, discard: bool, what: &str) -> Result<Option<String>, Reply> {
    if !dirty(server) {
        return Ok(None);
    }
    if !discard {
        let at = server
            .path
            .as_ref()
            .map_or_else(|| "the open project".to_string(), |p| p.display().to_string());
        return Err(Reply::err(format!(
            "{at} has unsaved changes; project_save it first, or pass discard: true to {what} anyway"
        )));
    }
    Ok(Some(discarded_note(server)))
}

/// What a discard says: which unsaved edits it drops, and the autosave they were written to
/// first, so they can still be recovered.
fn discarded_note(server: &Server) -> String {
    let old = server
        .path
        .as_ref()
        .map_or_else(|| "the open project".to_string(), |p| p.display().to_string());
    match write_autosave(server) {
        Ok(Some(auto)) => format!("discarded unsaved edits in {old} (still in its autosave {})", auto.display()),
        // Nothing written: the autosave beside the project already holds these edits, or they match the file.
        Ok(None) => match server.path.as_deref().map(autosave_path).filter(|a| a.is_file()) {
            Some(auto) => format!("discarded unsaved edits in {old} (its autosave {} is the last written copy)", auto.display()),
            None => format!("discarded unsaved edits in {old} (nothing was written beside it)"),
        },
        Err(why) => {
            eprintln!("autosave FAILED: {why}; the discarded edits are NOT on disk");
            format!("discarded unsaved edits in {old}: autosave FAILED ({why}), so those edits are gone")
        }
    }
}

/// The reply line for a path the person typed relative: the absolute path the call used.
fn relative_note(args: &Value, name: &str, used: &Path) -> Option<String> {
    let raw = arg_str(args, name).ok()?;
    (!expand_home(raw).is_absolute()).then(|| format!("{raw} is a relative path; the file used is {}", used.display()))
}

/// Installs a session as the open project, with a fresh ledger.
fn install(server: &mut Server, session: PlaybackSession, path: PathBuf, saved: Option<(Option<SystemTime>, usize)>) {
    let (mt, depth) = match saved {
        Some((mt, depth)) => (mt, Some(depth)),
        None => (None, None),
    };
    server.saved_pushes = depth.map(|_| session.history_pushes());
    server.saved_sources = depth.map(|_| session.sources().len());
    server.session = Some(session);
    server.saved_body = if depth.is_some() { server.session.as_ref().and_then(session_body) } else { None };
    note_own(server, &path);
    server.path = Some(path);
    server.saved_mtime = mt;
    server.saved_depth = depth;
    server.history_lost_on_open = !server.ledger.is_empty();
    server.ledger.clear();
    server.redo_ledger.clear();
    server.saved_top = server.ledger_top();
    server.history_capped = false;
    server.unrecovered_autosave = false;
    server.recovered_autosave = None;
    server.media_open = None;
    // What the open had to fix to load: unsaved until a save writes it (see `dirty`).
    server.load_fixes = session_fixes(server.session.as_ref());
    crate::tools::media::remember_sources(server);
}

/// The open's load fixes as the engine reports them, empty for no session.
fn session_fixes(session: Option<&PlaybackSession>) -> Vec<String> {
    session.map_or_else(Vec::new, |s| s.load_warnings().to_vec())
}

fn parse_resolution(raw: &str) -> Result<(u32, u32), Reply> {
    let bad = || Reply::err(format!("`{raw}` is not a resolution: write WIDTHxHEIGHT, e.g. 1920x1080"));
    let lower = raw.trim().to_ascii_lowercase();
    let (w, h) = lower.split_once('x').ok_or_else(bad)?;
    let w: u32 = w.trim().parse().map_err(|_| bad())?;
    let h: u32 = h.trim().parse().map_err(|_| bad())?;
    if !(16..=8192).contains(&w) || !(16..=8192).contains(&h) {
        return Err(Reply::err(format!("{w}x{h}: each side must be 16 to 8192 pixels")));
    }
    if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(Reply::err(format!("{w}x{h}: width and height must both be even")));
    }
    if !engine::is_resolution(w, h) {
        return Err(Reply::err(format!("{w}x{h} is not a resolution the engine accepts")));
    }
    Ok((w, h))
}

fn fps_arg(args: &Value) -> Result<Option<f64>, Reply> {
    match opt_f64(args, "fps") {
        None => match args.get("fps") {
            None | Some(Value::Null) => Ok(None),
            Some(_) => Err(Reply::err("argument `fps` must be a number, e.g. 30")),
        },
        Some(f) if f.is_finite() && (1.0..=240.0).contains(&f) => Ok(Some(f)),
        Some(f) => Err(Reply::err(format!("fps {f} is out of range: use a frame rate from 1 to 240"))),
    }
}

fn fmt_fps(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Every clip's start and end frame on every lane, in lane order. A rate change retimes each one.
fn clip_edges(s: &PlaybackSession) -> Vec<u32> {
    let mut edges = Vec::new();
    for lane in s.lanes() {
        for clip in s.lane_clips(lane) {
            edges.push(clip.start);
            edges.push(clip.end());
        }
    }
    edges
}

/// How many edges sit at a different time after a change from `old_fps` to `new_fps`: each
/// frame number is compared as the seconds it names before and after the change.
fn moved_edges(before: &[u32], after: &[u32], old_fps: f64, new_fps: f64) -> usize {
    before
        .iter()
        .zip(after)
        .filter(|(b, a)| (f64::from(**b) / old_fps - f64::from(**a) / new_fps).abs() > 1e-9)
        .count()
}

/// The locked lanes whose clip edges an fps change from `old` to `new` would move, with how many
/// edges move on each. The engine's own rate map says where each edge lands, the one the retime uses.
fn locked_lanes_moved(s: &PlaybackSession, old: f64, new: f64) -> Vec<(String, usize)> {
    let Ok(k) = Rate::from_fps(old, new) else {
        return Vec::new();
    };
    s.lanes()
        .into_iter()
        .filter(|&l| s.lane_locked(l))
        .filter_map(|l| {
            let edges: Vec<u32> = s.lane_clips(l).iter().flat_map(|c| [c.start, c.end()]).collect();
            let after: Vec<u32> = edges.iter().map(|&e| k.timeline_nearest(e)).collect();
            let moved = moved_edges(&edges, &after, old, new);
            (moved > 0).then(|| (parse::lane_name(l), moved))
        })
        .collect()
}

/// The start frame of every clip on a locked lane, in lane order. An fps change keeps such a clip's
/// seconds and rescales its frame number, so its address (`V1@100`) moves with the rate.
fn locked_starts(s: &PlaybackSession) -> Vec<(Lane, u32)> {
    let mut out = Vec::new();
    for l in s.lanes() {
        if s.lane_locked(l) {
            for c in s.lane_clips(l) {
                out.push((l, c.start));
            }
        }
    }
    out
}

/// How many moved addresses an fps note names before it says `… (N more)`.
const ADDRESSES_SHOWN: usize = 8;

/// The note an fps change owes when it keeps a locked lane's times but moves its frame addresses:
/// `locked lane V1 keeps its times; its frame addresses change: V1@100→V1@120`. `before` and `after`
/// are `locked_starts` read either side of the change. `moved` names the lanes whose times moved
/// (the force warning covers those). None when no other locked clip's address changes.
fn locked_address_note(before: &[(Lane, u32)], after: &[(Lane, u32)], moved: &[String]) -> Option<String> {
    let mut lanes: Vec<String> = Vec::new();
    let mut pairs: Vec<String> = Vec::new();
    for (&(lane, b), &(_, a)) in before.iter().zip(after) {
        let name = parse::lane_name(lane);
        if b == a || moved.contains(&name) {
            continue;
        }
        if !lanes.contains(&name) {
            lanes.push(name);
        }
        pairs.push(format!("{}→{}", parse::addr(lane, b), parse::addr(lane, a)));
    }
    if pairs.is_empty() {
        return None;
    }
    let (one, shown) = (lanes.len() == 1, pairs.len().min(ADDRESSES_SHOWN));
    let more = if pairs.len() > shown { format!(", … ({} more)", pairs.len() - shown) } else { String::new() };
    let (who, keep, pronoun) = if one { ("lane", "keeps its", "its") } else { ("lanes", "keep their", "their") };
    Some(format!(
        "locked {who} {} {keep} times; {pronoun} frame addresses change: {}{more}",
        lanes.join(", "),
        pairs[..shown].join(", ")
    ))
}

// ---- tools -----------------------------------------------------------------

/// The refusal `project_new` gives for a path with an earlier session's unsaved edits in its
/// autosave, unless `overwrite` is set. It names the autosave and what it holds, and the two ways
/// on: continue from it, or start fresh (overwrite sets the autosave aside as a dated copy).
fn new_over_autosave_refusal(path: &Path, overwrite: bool) -> Option<String> {
    if overwrite {
        return None;
    }
    let auto = unloaded_autosave(path)?;
    let when = mtime(&auto).map_or_else(|| "an unknown time".to_string(), local_text);
    // Compared with the saved project beside the path when there is one, else with an empty
    // project. The reply names which, so the difference is read against the right thing.
    let p = path.display();
    let (summary, against) = if path.is_file() {
        let disk = std::fs::read_to_string(path).unwrap_or_default();
        let summary = match PlaybackSession::open_project(path) {
            Ok(saved) => describe_autosave(&saved, &disk, &auto),
            Err(e) => format!("the saved project could not be read: {e}"),
        };
        (summary, "the saved project".to_string())
    } else {
        let summary = match PlaybackSession::empty(1920, 1080, 30.0) {
            // No file to read settings from: the autosave's own settings then count as unchanged.
            Ok(empty) => {
                let auto_text = std::fs::read_to_string(&auto).unwrap_or_default();
                describe_autosave(&empty, &auto_text, &auto)
            }
            Err(e) => format!("could not be read: {e}"),
        };
        (summary, format!("an empty project, since {p} is not saved yet"))
    };
    Some(format!(
        "the autosave at {} from {when} differs from {against} by {summary}; it holds an earlier session's unsaved edits. Continue from it with project_open {{\"path\": \"{p}\", \"recover\": true}}, or start fresh with project_new {{\"path\": \"{p}\", \"overwrite\": true}}, which sets the autosave aside as a dated copy. Nothing was created.",
        auto.display()
    ))
}

/// `templates`: every template, the built-ins first.
fn templates_list(server: &mut Server, _args: &Value) -> Reply {
    let Some(dir) = server.templates_dir() else {
        return Reply::err("there is no data folder to keep templates in");
    };
    let rows = engine::templates::list(&dir);
    let text = rows
        .iter()
        .map(|r| {
            let kind = if r.builtin { "built-in" } else { "saved" };
            let clips = if r.clips { ", keeps clips" } else { "" };
            format!("{} ({kind}{clips})", r.name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let items: Vec<Value> = rows.iter().map(|r| json!({"name": r.name, "builtin": r.builtin, "clips": r.clips})).collect();
    Reply::ok(text).with(json!({"templates": items}))
}

fn project_new(server: &mut Server, args: &Value) -> Reply {
    let path = tri!(path_arg(args, "path"));
    let discard = opt_bool(args, "discard").unwrap_or(false);
    let overwrite = opt_bool(args, "overwrite").unwrap_or(false);
    // An earlier session's unsaved edits beside this path: the person chooses before anything is
    // created, whether or not the file exists.
    if let Some(why) = new_over_autosave_refusal(&path, overwrite) {
        return Reply::err(why);
    }
    let existed = path.exists();
    if existed && !overwrite {
        return Reply::err(format!(
            "{} already exists; use project_open {{\"path\": \"{}\"}} to continue it, or overwrite:true to replace it on project_save",
            path.display(),
            path.display()
        ));
    }
    let discarded = tri!(guard_dirty(server, discard, "start a new project"));
    // A template sets the canvas, the rate, the lanes and the caption style; a size or rate typed
    // as well would be one of two answers, so it is refused.
    let (session, from) = match opt_str(args, "template") {
        Some(name) => {
            if opt_str(args, "resolution").is_some() || args.get("fps").is_some() {
                return Reply::err("a template sets the canvas size and frame rate: leave out resolution and fps, or leave out template");
            }
            let Some(dir) = server.templates_dir() else {
                return Reply::err("there is no data folder to read templates from");
            };
            match engine::templates::open(&dir, name) {
                Ok(o) => (o.session, Some((name.to_string(), o.export))),
                Err(e) => return Reply::err(e.to_string()),
            }
        }
        None => {
            let (w, h) = match opt_str(args, "resolution") {
                Some(r) => tri!(parse_resolution(r)),
                None => (1920, 1080),
            };
            let fps = tri!(fps_arg(args)).unwrap_or(30.0);
            match PlaybackSession::empty(w, h, fps) {
                Ok(s) => (s, None),
                Err(e) => return Reply::err(format!("could not start an empty project: {e}")),
            }
        }
    };
    let (w, h) = session.resolution();
    let fps = session.meta().frame_rate;
    // The folder the project will be saved into is made now, so project_save does not later fail
    // on a folder that is not there. Made only once the project has started, so a refusal leaves nothing.
    let folder = path.parent().filter(|d| !d.as_os_str().is_empty() && !d.is_dir());
    if let Some(dir) = folder
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return Reply::err(format!("could not create the folder {} for the project: {e}", dir.display()));
    }
    install(server, session, path.clone(), None);
    // overwrite:true: the file that exists is the one project_save may replace (its mtime is
    // the one this project was started against), so the save is not refused as a change on disk.
    if existed {
        server.saved_mtime = mtime(&path);
    }
    let mut text = format!(
        "new project {} ({w}x{h} @{}); the project file is written only by project_save, which needs at least one source (media_import first). Unsaved work is autosaved beside it when this session ends",
        path.display(),
        fmt_fps(fps)
    );
    if let Some((name, export)) = &from {
        text.push_str(&format!("\nstarted from template {name}"));
        if let Some(id) = export {
            text.push_str(&format!("; its export preset is {id}"));
        }
    }
    if let Some(note) = relative_note(args, "path", &path) {
        text.push_str(&format!("\n{note}"));
    }
    if let Some(dir) = folder {
        text.push_str(&format!("\ncreated the folder {} for the project (with any folders missing above it)", dir.display()));
    }
    if existed {
        text.push_str(&format!("\n{} exists and project_save will replace it", path.display()));
    }
    // An autosave an earlier session left beside this path is never overwritten by this one.
    // overwrite:true replaces that project, so its autosave is set aside (dated, kept) instead,
    // and the next save is not refused over it.
    let auto = autosave_path(&path);
    if auto.is_file() {
        let aside = if overwrite { set_aside_dated(&auto, &path).ok() } else { None };
        match aside {
            Some((kept, pruned)) => {
                text.push_str(&format!(
                    "\nset the earlier autosave {} aside as {} (overwrite: its unsaved edits are not loaded; the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted))",
                    auto.display(),
                    kept.display()
                ));
                if let Some(note) = pruned_note(&pruned) {
                    text.push_str(&format!("\n{note}"));
                }
            }
            None => {
                server.unrecovered_autosave = true;
                text.push_str(&format!("\nan autosave from an earlier session is at {}; this session's autosave is kept apart from it", auto.display()));
            }
        }
    }
    if let Some(note) = discarded {
        text = format!("{note}\n{text}");
    }
    Reply::ok(text)
}

fn project_open(server: &mut Server, args: &Value) -> Reply {
    let path = tri!(path_arg(args, "path"));
    let discard = opt_bool(args, "discard").unwrap_or(false);
    let discard_autosave = opt_bool(args, "discard_autosave").unwrap_or(false);
    // A media file's project is <stem>.edith beside it: recovering it must install that project,
    // never the media file, or a save would write project data over the media.
    match args.get("recover") {
        Some(Value::Bool(true)) => return recover_open(server, &project_file(&path), discard, None),
        Some(Value::String(name)) => return recover_open(server, &project_file(&path), discard, Some(name.trim())),
        _ => {}
    }
    if !path.exists() {
        // project_new names a path that is written only by project_save, so the open
        // project can have a path no file has yet.
        if server.session.is_some() && server.path.as_deref() == Some(path.as_path()) {
            let mut text = format!(
                "{} is the open project, not yet saved — it's already open; call project_save to write it",
                path.display()
            );
            // An earlier session's autosave makes that save refuse: say how to get past it.
            let auto = autosave_path(&path);
            if auto.is_file() && server.recovered_autosave.as_deref() != Some(auto.as_path()) {
                text.push_str(&format!("\n{}", unrecovered_choices(&path)));
            }
            return Reply::err(text);
        }
        return Reply::err(match unsaved_autosave_note(&path) {
            Some(note) => format!("{}\n{note}", not_found(&path)),
            None => not_found(&path),
        });
    }
    // Checked before the unsaved guard: a refusal names this file, not the open project.
    if let Some(why) = not_a_file(&path) {
        return Reply::err(why);
    }
    let is_edith = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("edith"));
    if let Err(e) = std::fs::File::open(&path) {
        return Reply::err(format!("cannot read {}: {e}", path.display()));
    }
    if !is_edith && !is_media(&path) {
        // The reason a named refusal gives for a media file the engine will not open (a display matrix
        // that is not a quarter turn), so the refusal says why; anything else is not media.
        return Reply::err(match named_media_refusal(&path) {
            Some(why) => format!("{} is media edith cannot open: {why}", path.display()),
            None => format!("{} is not an edith project or a media file", path.display()),
        });
    }
    // Read the project first, so the autosave decision is made before anything is written. A
    // media file's project is <stem>.edith beside it.
    let project = project_file(&path);
    let (session, relinks, notes) = if is_edith {
        let (relinks, notes) = relink_by_name(&path, &search_dirs(args));
        match PlaybackSession::open_project_relinked(&path, &relinks) {
            Ok(s) => (s, relinks, notes),
            Err(e) => return Reply::err(open_failure(&path, &e.to_string())),
        }
    } else {
        match PlaybackSession::open(&path) {
            Ok(s) => (s, Vec::new(), Vec::new()),
            Err(e) => return Reply::err(format!("could not open {} as a project: {e}", path.display())),
        }
    };
    // An earlier session's unsaved edits beside the project: the person chooses how to open, and
    // nothing is loaded, written or set aside until they do.
    if let Some(why) = unloaded_autosave_refusal(&project, &session, discard_autosave) {
        return Reply::err(why);
    }
    let discarded = tri!(guard_dirty(server, discard, "open another file"));
    // The lines the reply starts with, in order: unloaded edits first, then what a discard dropped.
    let mut lead: Vec<String> = Vec::new();
    let settled = match settle_autosave_on_open(&project, discard_autosave) {
        Ok(note) => note,
        Err(why) => return Reply::err(why),
    };
    if is_edith {
        let depth = session.undo_depth();
        let mt = mtime(&path);
        let offline = session.offline_sources();
        let fixes = load_fix_lines(&session);
        let older = older_autosave_lines(&path, &session);
        install(server, session, path, Some((mt, depth)));
        // Relinked paths are written by the next save, so the project is unsaved until then.
        if !relinks.is_empty() {
            server.load_fixes.push(format!("relinked {} source(s) by name", relinks.len()));
            lead.push(format!("relinked {} source(s); save to keep the new paths", relinks.len()));
        }
        lead.extend(fixes);
        lead.extend(settled);
        lead.extend(notes);
        lead.extend(offline_lines(&offline));
        lead.extend(older);
    } else {
        let base = (session.undo_depth(), session.history_pushes(), session.sources().len());
        install(server, session, project, None);
        server.media_open = Some(base);
        lead.extend(settled);
    }
    lead.extend(discarded);
    let mut reply = status_reply(server);
    if !lead.is_empty() {
        reply.text = format!("{}\n{}", lead.join("\n"), reply.text);
    }
    reply.text.push_str(UNDO_OPEN_NOTE);
    reply
}

/// The project file `project_open` loads for `path`: the path itself for a .edith file, else the
/// <stem>.edith beside a media file.
fn project_file(path: &Path) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("edith")) {
        path.to_path_buf()
    } else {
        path.with_extension("edith")
    }
}

/// The folders `project_open {search}` names, expanded.
fn search_dirs(args: &Value) -> Vec<PathBuf> {
    args.get("search")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| expand_home(s.trim())).collect())
        .unwrap_or_default()
}

/// Finds each offline source of the project at `path` by file name under `dirs`. A name found
/// exactly once is relinked; several matches are listed and left offline. Returns the pairs to
/// open with and the lines that say what was done.
fn relink_by_name(path: &Path, dirs: &[PathBuf]) -> (Vec<(PathBuf, PathBuf)>, Vec<String>) {
    let (mut relinks, mut notes) = (Vec::new(), Vec::new());
    if dirs.is_empty() {
        return (relinks, notes);
    }
    let Ok(doc) = engine::edith::load(path) else {
        return (relinks, notes);
    };
    for (i, source) in doc.sources.iter().enumerate() {
        if engine::source_present(&source.path) {
            continue;
        }
        let Some(name) = source.path.file_name() else {
            continue;
        };
        let mut found = Vec::new();
        for dir in dirs {
            find_named(dir, name, 6, &mut found);
        }
        match found.len() {
            0 => notes.push(format!("src:{i} {} is not under the search folders", source.path.display())),
            1 => {
                let new = found.remove(0);
                notes.push(format!("src:{i} relinked: {} -> {}", source.path.display(), new.display()));
                relinks.push((source.path.clone(), new));
            }
            n => {
                let all: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
                notes.push(format!(
                    "src:{i} {}: {n} files named {} under the search folders ({}); not relinked, pick one with media_relink",
                    source.path.display(),
                    name.to_string_lossy(),
                    all.join(", ")
                ));
            }
        }
    }
    (relinks, notes)
}

/// Every file named `name` under `dir`, down `depth` folder levels.
fn find_named(dir: &Path, name: &std::ffi::OsStr, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if depth > 0 {
                find_named(&p, name, depth - 1, out);
            }
        } else if p.file_name() == Some(name) {
            out.push(p);
        }
    }
}

/// The LUT report `project_open` gives: each `.cube` file clips name that cannot be used.
pub(crate) fn lut_lines(problems: &[(PathBuf, String)]) -> Vec<String> {
    problems
        .iter()
        .map(|(p, why)| {
            format!(
                "LUT not applied: {} ({why}); its clips play without it until you relink it: edit {{ops:[{{op:\"lut\", relink:\"{}\", path:\"<new path>\"}}]}}",
                p.display(),
                p.display()
            )
        })
        .collect()
}

/// The offline report `project_open` gives: which sources are missing and how to fix them.
fn offline_lines(offline: &[(usize, PathBuf)]) -> Vec<String> {
    if offline.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "{} source(s) offline (moved or deleted): their clips stay, and frames or exports that need them are refused",
        offline.len()
    )];
    for (i, p) in offline {
        lines.push(format!("  src:{i} {}", p.display()));
    }
    lines.push(
        "fix: media_relink {\"source\": \"src:N\", \"path\": \"<file>\"} for each, or project_open {\"path\": \"<project>\", \"search\": [\"<folder>\"]} to find them by name".to_string(),
    );
    lines
}

/// What `project_open` adds: the undo ledger is in-memory, so a reopened project starts
/// with no steps to undo. Checkpoints are the points that survive.
const UNDO_OPEN_NOTE: &str =
    "\nundo history starts empty in this session; use checkpoint/restore for points that survive";

/// Makes the project's own autosave `own` hold the state of the dated autosave `chosen` that a
/// recovery loads. The autosave it replaces is moved to a dated copy first, so none of its edits
/// are lost, and the copies beside `path` are pruned as a dated copy always is. Returns the copy
/// (when one was made) and the older copies deleted to make room. Nothing is changed when `own`
/// already says what `chosen` says.
fn rewrite_autosave_from(chosen: &Path, own: &Path, path: &Path) -> Result<(Option<PathBuf>, Vec<PathBuf>), String> {
    if own.is_file() && same_project(chosen, own) {
        return Ok((None, Vec::new()));
    }
    // Copied beside the autosave, then moved into place, so a failed copy leaves `own` as it was.
    let staged = own.with_extension("recover.edith");
    std::fs::copy(chosen, &staged).map_err(|e| format!("could not write the autosave {} ({e})", own.display()))?;
    let aside = if own.is_file() {
        match set_aside_dated(own, path) {
            Ok(done) => Some(done),
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return Err(format!("could not move the autosave {} aside ({e}), so nothing was opened", own.display()));
            }
        }
    } else {
        None
    };
    if let Err(e) = std::fs::rename(&staged, own) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("could not move the autosave into place at {} ({e})", own.display()));
    }
    Ok(match aside {
        Some((kept, pruned)) => (Some(kept), pruned),
        None => (None, Vec::new()),
    })
}

/// How many lines of the project file the autosave changes: the larger of the two sides of the line
/// difference, so a clip that moved counts once. Read from the files, since an autosave keeps no count.
fn recovered_edits(saved: &Path, auto: &Path) -> usize {
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let (saved, auto) = (read(saved), read(auto));
    let (sl, al) = (body_lines(&saved), body_lines(&auto));
    minus(&al, &sl).len().max(minus(&sl, &al).len())
}

/// `project_open {recover: true}`: the saved project is opened as it is and the autosave's edits
/// go on top of it as one undo step, so undo takes them back to the saved timeline. At the
/// project's own path, so a later save writes the recovered work there, and left unsaved until
/// project_save. With no readable saved file the autosave is the project itself, and undo has
/// nothing earlier to return to.
fn recover_open(server: &mut Server, path: &Path, discard: bool, chosen: Option<&str>) -> Reply {
    let own = autosave_path(path);
    // `chosen` names a dated autosave (its file name or its stamp); none means the project's own.
    let auto = match chosen {
        None => own.clone(),
        Some(name) => match dated_autosaves(path).into_iter().find(|(_, p)| autosave_matches(p, name)) {
            Some((_, p)) => p,
            None => {
                let listed: Vec<String> = dated_autosaves(path)
                    .iter()
                    .map(|(_, p)| p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()))
                    .collect();
                let have = if listed.is_empty() { "there are no dated autosaves beside it".to_string() } else { format!("dated autosaves: {}", listed.join(", ")) };
                return Reply::err(format!("no dated autosave named {name} beside {}; {have}", path.display()));
            }
        },
    };
    if !auto.is_file() {
        return Reply::err(format!("nothing to recover: no autosave at {}", auto.display()));
    }
    // Unsaved edits in this session would be replaced by the recovered state. Refused unless
    // `discard` says they are dropped. Not written to an autosave first: that would overwrite the
    // very autosave being recovered.
    let discarded = if dirty(server) {
        if !discard {
            return Reply::err(format!(
                "{} has unsaved edits in this session, and recovering would replace them. Keep them: project_save {{discard_autosave:true}} saves them and sets the autosave aside as a dated copy. Or drop them: project_open {{\"path\": \"{}\", \"recover\": true, \"discard\": true}} loads the autosave and the unsaved edits of this session are lost.",
                path.display(),
                path.display()
            ));
        }
        let old = server.path.as_ref().map_or_else(|| "the open project".to_string(), |p| p.display().to_string());
        Some(format!("dropped the unsaved edits in {old} (they were not saved or autosaved)"))
    } else {
        None
    };
    let recovered = match PlaybackSession::open_project(&auto) {
        Ok(s) => s,
        Err(e) => return Reply::err(format!("could not open the autosave {}: {e}", auto.display())),
    };
    // The saved project, opened as it is, when there is one. The recovered edits go on top of it
    // as one undo step, so undo takes them back to the saved file's timeline. Checked before
    // anything is written: a refusal changes nothing. Without a readable saved file the autosave
    // is the project, and undo has nothing earlier to return to.
    let mut layer = if path.is_file() { PlaybackSession::open_project(path).ok() } else { None };
    let unreadable = path.is_file() && layer.is_none();
    let steps = match layer.as_mut() {
        Some(base) => {
            let before = base.history_pushes();
            if !base.adopt_as_step(&recovered) {
                return Reply::err(format!(
                    "could not recover {}: the autosave's effect settings do not fit beside the saved project's; nothing was changed",
                    path.display()
                ));
            }
            Some(base.history_pushes().saturating_sub(before) as usize)
        }
        None => None,
    };
    // A dated recovery makes the project's own autosave hold the recovered state, so the autosave
    // a later recover or save reads is this session's state. The one it replaces is kept dated.
    let rewritten = if auto == own {
        Ok((None, Vec::new()))
    } else {
        rewrite_autosave_from(&auto, &own, path)
    };
    let (replaced, pruned) = match rewritten {
        Ok(done) => done,
        Err(why) => return Reply::err(why),
    };
    let session = layer.unwrap_or(recovered);
    let fixes = load_fix_lines(&session);
    let older = older_autosave_lines(path, &session);
    install(server, session, path.to_path_buf(), Some((mtime(path), 0)));
    if let Some(n) = steps {
        let when = mtime(&auto).map_or_else(|| "an unknown time".to_string(), local_hhmm);
        let edits = recovered_edits(path, &auto);
        record(server, n, &format!("recover autosave ({edits} edits from {when})"));
    }
    // The project's own autosave now holds the recovered state, so it is this session's to replace.
    server.recovered_autosave = Some(own.clone());
    // Dirty by definition: the work lives in the autosave, not in the project file.
    server.saved_depth = None;
    let mut reply = status_reply(server);
    let after = if auto == own {
        "and removes the autosave"
    } else {
        "and removes the autosave (the dated copy stays beside it)"
    };
    reply.text.push_str(&format!(
        "\nrecovered the unsaved edits from {}; project_save writes them to {} {after}",
        auto.display(),
        path.display()
    ));
    if auto != own {
        reply.text.push_str(&format!("\nthe autosave {} now holds the recovered state, so a later recover or save reads the same edits", own.display()));
    }
    if let Some(kept) = replaced {
        reply.text.push_str(&format!(
            "\nthe earlier autosave {} was set aside as {} (its unsaved edits are not loaded; the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted))",
            own.display(),
            kept.display()
        ));
        if let Some(note) = pruned_note(&pruned) {
            reply.text.push_str(&format!("\n{note}"));
        }
    }
    if steps.is_some() {
        reply.text.push_str("\nundo takes the recovered edits back as one step, to the saved file's timeline");
    } else {
        let why = if unreadable { "the saved file could not be opened" } else { "the project was never saved" };
        reply.text.push_str(&format!(
            "\nundo has nothing earlier to return to: {why}, so undo starts empty here and reaches only edits made after this open. To drop the recovered edits, project_open {{discard_autosave:true}} sets the autosave aside as a dated copy"
        ));
    }
    for line in older {
        reply.text.push_str(&format!("\n{line}"));
    }
    if let Some(note) = discarded {
        reply.text = format!("{note}\n{}", reply.text);
    }
    if !fixes.is_empty() {
        reply.text = format!("{}\n{}", fixes.join("\n"), reply.text);
    }
    reply
}

/// One line per change a load made to open the file (a clip that ran past its source is
/// clamped, not refused). They come first in a reply, since the file on disk still has the
/// problem until it is saved again.
fn load_fix_lines(session: &PlaybackSession) -> Vec<String> {
    session
        .load_warnings()
        .iter()
        .map(|w| format!("warning: the file needed fixing to open: {w} — save to keep the fix"))
        .collect()
}

fn project_save(server: &mut Server, args: &Value) -> Reply {
    let force = opt_bool(args, "force").unwrap_or(false);
    // Checked first: with nothing open there is no project to save, whatever path was given.
    if server.session.is_none() {
        return Reply::err(
            "no project is open, so there is nothing to save. Start one with project_new {path:\"/abs/new.edith\"} (then media_import), or open an existing file with project_open {path:\"/abs/file.edith\"}",
        );
    }
    let target = match opt_str(args, "path") {
        Some(p) => absolute(&expand_home(p)),
        None => match &server.path {
            Some(p) => p.clone(),
            None => return Reply::err("no project path to save to: project_save {path:\"/abs/file.edith\"} first"),
        },
    };
    let session = tri!(server.session_ref());
    // The engine refuses a source-less project; say why here, before any folder is blamed.
    if session.sources().is_empty() {
        return Reply::err(format!(
            "nothing to save yet: {} would name no sources, and a project file names its sources. Import media first (media_import), then project_save",
            target.display()
        ));
    }
    if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty())
        && !dir.is_dir()
    {
        return Reply::err(format!("folder does not exist: {}", dir.display()));
    }
    // A file this session opened or saved (at any point, so a save-as and a save back are both
    // this project's) is overwritten without force, unless it changed on disk since.
    let known = own_mtime(server, &target);
    if !force && target.exists() {
        let Some(left) = known else {
            return Reply::err(format!(
                "{} already exists and is not this project's file; pass force: true to overwrite it",
                target.display()
            ));
        };
        if mtime(&target) != left {
            return Reply::err(format!(
                "{} changed on disk since it was last opened or saved (the GUI may have written it); pass force: true to overwrite",
                target.display()
            ));
        }
    }
    // An autosave beside the open project, newer than the file, that this session did not load
    // holds edits this save would leave out: refused unless the person says to set it aside (the
    // save keeps it dated). project_open settles an earlier session's autosave first, so this is
    // one that appeared while this session ran.
    let discard_autosave = opt_bool(args, "discard_autosave").unwrap_or(false);
    let open_path = server.path.clone().unwrap_or_else(|| target.clone());
    let open_auto = autosave_path(&open_path);
    let loaded_here = server.recovered_autosave.as_deref() == Some(open_auto.as_path());
    if !loaded_here && !discard_autosave && unloaded_autosave(&open_path).is_some() {
        let when = mtime(&open_auto).map_or_else(|| "an unknown time".to_string(), local_text);
        let disk = std::fs::read_to_string(&open_path).unwrap_or_default();
        let summary = describe_autosave(session, &disk, &open_auto);
        return Reply::err(format!(
            "an autosave from {when}, not loaded by this session, holds unsaved edits ({summary}) that are not in this project. {}",
            unrecovered_choices(&open_path)
        ));
    }
    let unchanged = !dirty(server) && server.path.as_deref() == Some(target.as_path()) && target.is_file();
    // Judged against the file as it is now: the save below makes the file newer than any autosave.
    let stale_newer = unloaded_autosave(&target).is_some();
    if let Err(e) = session.save_project(&target) {
        return Reply::err(write_failure(&target, &e.to_string()));
    }
    let depth = session.undo_depth();
    let pushes = session.history_pushes();
    let sources = session.sources().len();
    let body = session_body(session);
    let mt = mtime(&target);
    server.saved_body = body;
    note_own(server, &target);
    server.path = Some(target.clone());
    server.saved_mtime = mt;
    server.saved_depth = Some(depth);
    server.saved_pushes = Some(pushes);
    server.saved_top = server.ledger_top();
    server.saved_sources = Some(sources);
    // The fixes the open made to load are in this save from now on.
    let fixed = std::mem::take(&mut server.load_fixes);
    let mut text = if unchanged {
        format!("saved {}: nothing changed since the last save (saved anyway)", target.display())
    } else {
        format!("saved {}", target.display())
    };
    if let Some(note) = relative_note(args, "path", &target) {
        text.push_str(&format!("\n{note}"));
    }
    if !fixed.is_empty() {
        text.push_str(&format!("\nwritten with the {} fix(es) made to open it: {}", fixed.len(), fixed.join("; ")));
    }
    // This save wrote the session's whole state, so the autosave this session recovered is
    // superseded and removed, and so is one no newer than the file (it holds nothing the file
    // lacks). Any other autosave beside the target, newer than the file, holds edits this save
    // does not have: it is kept under a dated name.
    let stale = autosave_path(&target);
    if stale.is_file() {
        if server.recovered_autosave.as_deref() == Some(stale.as_path()) || !stale_newer {
            match std::fs::remove_file(&stale) {
                Ok(()) => text.push_str(&format!("\nremoved the autosave {}: this save replaces it", stale.display())),
                Err(e) => text.push_str(&format!("\ncould not remove the autosave {}: {e}", stale.display())),
            }
        } else {
            match set_aside_dated(&stale, &target) {
                Ok((kept, pruned)) => {
                    text = format!("kept the unrecovered autosave as {} (its edits are not in this save)\n{text}", kept.display());
                    if let Some(note) = pruned_note(&pruned) {
                        text.push_str(&format!("\n{note}"));
                    }
                }
                Err(e) => text.push_str(&format!("\ncould not keep the unrecovered autosave {}: {e}", stale.display())),
            }
        }
    }
    server.unrecovered_autosave = false;
    server.media_open = None;
    Reply::ok(text)
}

/// Where an unrecovered autosave is kept: `<stem>.autosave-<YYYYmmdd-HHMMSS>.edith` beside the
/// project, in local time. A name already taken gets `-2`, `-3`, ... so nothing is overwritten.
fn kept_autosave_path(target: &Path, now: SystemTime) -> PathBuf {
    let stem = target.file_stem().map_or_else(|| "untitled".to_string(), |s| s.to_string_lossy().into_owned());
    let stamp = local_stamp(now);
    let mut p = target.with_file_name(format!("{stem}.autosave-{stamp}.edith"));
    let mut n = 2;
    while p.exists() {
        p = target.with_file_name(format!("{stem}.autosave-{stamp}-{n}.edith"));
        n += 1;
    }
    p
}

/// Moves `from` to a new dated copy beside `target`, then keeps only the
/// newest `DATED_KEEP` dated copies (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). Returns the copy and the older copies deleted to make room.
fn set_aside_dated(from: &Path, target: &Path) -> std::io::Result<(PathBuf, Vec<PathBuf>)> {
    let kept = kept_autosave_path(target, SystemTime::now());
    std::fs::rename(from, &kept)?;
    let pruned = prune_dated(target, &kept);
    Ok((kept, pruned))
}

/// Deletes the dated copies beside `target` beyond the newest `DATED_KEEP`, except those written
/// within `DATED_RECENT`. `keep` (the copy just made) is never deleted and takes one of those places.
/// Returns the paths deleted. What is deleted is named in the reply, and its edits are gone.
fn prune_dated(target: &Path, keep: &Path) -> Vec<PathBuf> {
    let others: Vec<(SystemTime, PathBuf)> = dated_autosaves(target).into_iter().filter(|(_, p)| p != keep).collect();
    let now = SystemTime::now();
    let mut deleted = Vec::new();
    // Newest first: the first `DATED_KEEP - 1` sit beside `keep`. A copy in the future counts as recent.
    for (i, (written, p)) in others.into_iter().enumerate() {
        let recent = match now.duration_since(written) {
            Ok(age) => age < DATED_RECENT,
            Err(_) => true,
        };
        if i < DATED_KEEP - 1 || recent {
            continue;
        }
        if std::fs::remove_file(&p).is_ok() {
            deleted.push(p);
        }
    }
    deleted
}

/// The reply line for dated copies `prune_dated` deleted, if any.
fn pruned_note(pruned: &[PathBuf]) -> Option<String> {
    if pruned.is_empty() {
        return None;
    }
    let names: Vec<String> = pruned.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
    Some(format!(
        "deleted {} older dated autosave(s) beyond the newest {DATED_KEEP} kept beside the project and older than 7 days: {}. Their unsaved edits are gone: they are in neither the project file nor any autosave now",
        pruned.len(),
        names.join(", ")
    ))
}

/// The local time of `t`, or `None` before the epoch.
fn local_time(t: SystemTime) -> Option<chrono::DateTime<chrono::Local>> {
    t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(chrono::DateTime::<chrono::Local>::from(t))
}

/// `YYYYmmdd-HHMMSS` in local time, the stamp dated autosave names carry.
fn local_stamp(t: SystemTime) -> String {
    match local_time(t) {
        Some(tm) => tm.format("%Y%m%d-%H%M%S").to_string(),
        None => "00000000-000000".to_string(),
    }
}

/// `HH:MM` in local time, for a ledger label.
fn local_hhmm(t: SystemTime) -> String {
    match local_time(t) {
        Some(tm) => tm.format("%H:%M").to_string(),
        None => "an unknown time".to_string(),
    }
}

/// `YYYY-mm-dd HH:MM:SS` in local time, for replies.
fn local_text(t: SystemTime) -> String {
    match local_time(t) {
        Some(tm) => tm.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => "an unknown time".to_string(),
    }
}

/// The dated autosaves (`<stem>.autosave-<stamp>.edith`) beside `path`, newest first. Nothing is
/// deleted: a dated autosave may hold unrecovered edits, so the person decides. The project's own
/// `<stem>.autosave.edith` is not among them: `settle_autosave_on_open` owns it.
fn dated_autosaves(path: &Path) -> Vec<(SystemTime, PathBuf)> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{stem}.autosave-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".edith"))
                && p.is_file()
        })
        .map(|p| (mtime(&p).unwrap_or(SystemTime::UNIX_EPOCH), p))
        .collect();
    found.sort_by_key(|b| std::cmp::Reverse(b.0));
    found
}

/// Whether a dated autosave is named `name`: its file name, or its stamp (`20261016-223636`,
/// local time, with any `-2` suffix a name collision added).
fn autosave_matches(auto: &Path, name: &str) -> bool {
    let Some(file) = auto.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if file == name {
        return true;
    }
    file.split_once(".autosave-")
        .and_then(|(_, rest)| rest.strip_suffix(".edith"))
        .is_some_and(|stamp| stamp == name)
}

/// The lines `project_open` lists for the dated autosaves beside `path`: each one's file name,
/// local time and what it differs by from the open project. Empty when there are none.
fn older_autosave_lines(path: &Path, open: &PlaybackSession) -> Vec<String> {
    let found = dated_autosaves(path);
    if found.is_empty() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines = vec![format!(
        "older autosaves ({}; recover one with project_open {{path:\"{}\", recover:\"<name or stamp>\"}}):",
        found.len(),
        path.display()
    )];
    for (t, p) in &found {
        let name = p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let diff = describe_autosave(open, &text, p);
        // The name carries the set-aside stamp (what recover accepts); the time is when the edits were made.
        lines.push(format!("  {name} (edits from {}: {diff})", local_text(*t)));
    }
    lines
}

/// What an autosave's edits change against `against` (a session, with the project file's text
/// for the settings), in the words the warnings use.
fn describe_autosave(against: &PlaybackSession, against_text: &str, auto: &Path) -> String {
    let auto_text = std::fs::read_to_string(auto).unwrap_or_default();
    let settings = settings_differences(against_text, &auto_text);
    match PlaybackSession::open_project(auto) {
        Ok(saved) => autosave_summary(against, &saved, &settings),
        Err(e) => format!("could not be read: {e}"),
    }
}

/// Where the autosave of the project at `path` goes: the same folder, named
/// `<name>.autosave.edith`. Never the project's own file, so an autosave cannot
/// overwrite the work it is protecting.
pub fn autosave_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map_or_else(|| "untitled".to_string(), |s| s.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}.autosave.edith"))
}

/// Where an autosave of a project that was never given a path goes: the editor's
/// own cache, one file per process so two runs do not share one.
fn untitled_autosave_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".cache").join("edith").join("autosave");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Some(dir.join(format!("untitled-{}-{stamp}.autosave.edith", std::process::id())))
}

/// The session's whole project as the engine writes it, the `playhead` line left out. It is
/// written through a scratch file in the cache folder, so nothing lands beside the project.
fn session_body(session: &PlaybackSession) -> Option<Vec<u8>> {
    // One scratch file per call: calls in one process (parallel test threads, or a
    // session checked while another is written) must not write and read the same file.
    static NEXT_BODY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = scratch_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let n = NEXT_BODY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("body-{}-{n}.edith", std::process::id()));
    let bytes = session.save_project(&tmp).ok().and_then(|()| std::fs::read(&tmp).ok());
    let _ = std::fs::remove_file(&tmp);
    bytes.map(|b| b.split(|&c| c == b'\n').filter(|l| !l.starts_with(b"playhead ")).collect::<Vec<_>>().join(&b'\n'))
}

/// The folder for the editor's own scratch files, under the user's cache.
fn scratch_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache").join("edith").join("autosave"))
}

/// The lines of a project file the playhead line does not make.
fn body_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.starts_with("playhead ")).collect()
}

/// `a` without the lines of `b`, counting each line as many times as it appears.
fn minus<'a>(a: &[&'a str], b: &[&str]) -> Vec<&'a str> {
    let mut left: HashMap<&str, usize> = HashMap::new();
    for l in b {
        *left.entry(l).or_default() += 1;
    }
    a.iter()
        .filter(|l| match left.get_mut(**l) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .copied()
        .collect()
}

/// A clip line's fields that the settings diff reads: `video <lane> <start> ... eq color ... transform`.
struct ClipLine<'a> {
    lane: String,
    start: &'a str,
    eq: &'a str,
    color: &'a str,
    transform: &'a str,
    line: &'a str,
}

fn clip_lines<'a>(lines: &[&'a str]) -> Vec<ClipLine<'a>> {
    lines
        .iter()
        .filter_map(|&l| {
            let t: Vec<&str> = l.split(' ').collect();
            (matches!(t.first(), Some(&"video" | &"audio")) && t.len() >= 15).then(|| ClipLine {
                lane: format!("{}{}", if t[0] == "video" { "V" } else { "A" }, t[1]),
                start: t[2],
                eq: t[7],
                color: t[8],
                transform: t[14],
                line: l,
            })
        })
        .collect()
}

/// What differs between the file as opened and the autosave beside it, read from the two
/// texts: lane gains and flags, the colour, eq and transform of named clips, and the
/// project's own settings. Clip counts and sources are the caller's business.
fn settings_differences(file: &str, auto: &str) -> Vec<String> {
    let (f, a) = (body_lines(file), body_lines(auto));
    let (only_auto, only_file) = (minus(&a, &f), minus(&f, &a));
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    let (fc, ac) = (clip_lines(&f), clip_lines(&a));
    // A clip whose line changed: which of its colour, eq or transform moved, else the clip itself.
    for c in ac.iter().filter(|c| only_auto.contains(&c.line)) {
        let Some(old) = fc.iter().find(|o| o.lane == c.lane && o.start == c.start) else {
            continue;
        };
        let mut named = false;
        for (kind, now, was) in [("eq", c.eq, old.eq), ("color", c.color, old.color), ("transform", c.transform, old.transform)] {
            if now != was {
                add(format!("{kind} on {}@{}", c.lane, c.start));
                named = true;
            }
        }
        if !named {
            add(format!("clip {}@{} changed", c.lane, c.start));
        }
    }
    // The colour, eq and transform tables: a changed entry is the clips that point at it.
    for (kind, field) in [("eq", 7), ("color", 8), ("transform", 14)] {
        let entries: Vec<(usize, &str)> = a.iter().filter(|l| l.split(' ').next() == Some(kind)).copied().enumerate().collect();
        for (i, entry) in entries {
            if !only_auto.contains(&entry) {
                continue;
            }
            let users: Vec<String> = ac
                .iter()
                .filter(|c| {
                    let v = match field {
                        7 => c.eq,
                        8 => c.color,
                        _ => c.transform,
                    };
                    v == i.to_string()
                })
                .map(|c| format!("{}@{}", c.lane, c.start))
                .collect();
            if users.is_empty() {
                add(format!("{kind} table entry changed"));
            } else {
                for u in users.iter().take(3) {
                    add(format!("{kind} on {u}"));
                }
                if users.len() > 3 {
                    add(format!("{kind} on {} more clips", users.len() - 3));
                }
            }
        }
    }
    for l in &only_auto {
        let t: Vec<&str> = l.split(' ').collect();
        let described = match t[0] {
            "gain" if t.len() >= 4 => format!("lane {}{} gain {} dB", lane_letter(t[1]), t[2], db_text(t[3])),
            "flags" if t.len() >= 4 => format!("lane {}{} {}", lane_letter(t[1]), t[2], t[3..].join(" ")),
            "tonemap" | "proxy" | "autoproxy" | "encoder" => format!("{} {}", t[0], t.get(1).unwrap_or(&"")),
            "resolution" if t.len() >= 3 => format!("resolution {}x{}", t[1], t[2]),
            "fps" | "samplerate" => format!("{} {}", t[0], t.get(1).unwrap_or(&"")),
            "limiter" => format!("limiter {}", t[1..].join(" ")),
            "sub" | "subtitle" | "cue" | "source" | "video" | "audio" | "eq" | "color" | "transform" => continue,
            other => format!("{other} changed"),
        };
        add(described);
    }
    // A gain or flag line the file had and the autosave drops is a setting put back.
    for l in &only_file {
        let t: Vec<&str> = l.split(' ').collect();
        let lane = |t: &[&str]| format!("{}{}", lane_letter(t[1]), t[2]);
        if t[0] == "gain" && t.len() >= 4 && !only_auto.iter().any(|x| x.starts_with("gain ") && x.split(' ').nth(2) == Some(t[2]) && x.split(' ').nth(1) == Some(t[1])) {
            add(format!("lane {} gain reset", lane(&t)));
        } else if t[0] == "flags" && t.len() >= 4 && !only_auto.iter().any(|x| x.starts_with("flags ") && x.split(' ').nth(2) == Some(t[2]) && x.split(' ').nth(1) == Some(t[1])) {
            add(format!("lane {} flags cleared", lane(&t)));
        }
    }
    out
}

/// A dB figure as a person writes it: `-6` for `-6.0`, `-1.5` as it is.
fn db_text(raw: &str) -> String {
    match raw.parse::<f64>() {
        Ok(v) if v.fract() == 0.0 => format!("{v:.0}"),
        _ => raw.to_string(),
    }
}

fn lane_letter(kind: &str) -> &'static str {
    match kind {
        "video" => "V",
        "audio" => "A",
        _ => "S",
    }
}

/// Whether two project files say the same thing. The `playhead` line is where the view
/// rests, not an edit, so it is left out of the comparison.
fn same_project(a: &Path, b: &Path) -> bool {
    let (Ok(x), Ok(y)) = (std::fs::read(a), std::fs::read(b)) else {
        return false;
    };
    let body = |d: &[u8]| d.split(|&c| c == b'\n').filter(|l| !l.starts_with(b"playhead ")).map(<[u8]>::to_vec).collect::<Vec<_>>();
    body(&x) == body(&y)
}

/// At stdin EOF: a dirty project is written beside its file (or to the cache, if it
/// has none), so the work a client walked away from survives the server. Nothing is
/// written when the session says what the autosave or the file already says, so a
/// read-only run leaves no trace. Logged to stderr. Returns the path written, if any.
/// A write that fails is logged as FAILED with its reason: the edits it held are then
/// not on disk anywhere.
pub fn autosave_at_eof(server: &Server) -> Option<PathBuf> {
    match write_autosave(server) {
        Ok(path) => path,
        Err(why) => {
            eprintln!("autosave FAILED: {why}; this session's unsaved edits are NOT on disk");
            None
        }
    }
}

/// The write behind `autosave_at_eof`; also used when a discard keeps the edits it drops.
/// `Err` carries the reason the write failed.
fn write_autosave(server: &Server) -> Result<Option<PathBuf>, String> {
    if !dirty(server) {
        return Ok(None);
    }
    let Some(session) = server.session.as_ref() else {
        return Ok(None);
    };
    if session.sources().is_empty() {
        return Ok(None);
    }
    // A session that made no edit writes nothing: whatever it opened (a load fix, a relink, a
    // recovered autosave) is still on disk where it was, and the reply already said so.
    if !edited_this_session(server, session) {
        return Ok(None);
    }
    // The whole project, not its clips: a session that says what the file as opened (or saved)
    // says has nothing unsaved. A library row no clip plays from is not in the file's words,
    // so the source count is checked as well.
    if let Some(base) = &server.saved_body
        && server.saved_sources == Some(session.sources().len())
        && session_body(session).is_some_and(|b| b == *base)
    {
        return Ok(None);
    }
    // A media file opened as a project has no project file of its own: its autosave never goes
    // beside the file it came from, and nothing is written until an edit moves it off the open.
    let beside = server.path.as_deref().filter(|_| server.media_open.is_none());
    if let Some(base) = server.media_open
        && (session.undo_depth(), session.history_pushes(), session.sources().len()) == base
    {
        return Ok(None);
    }
    let auto = match beside {
        Some(p) => autosave_path(p),
        None => untitled_autosave_path().ok_or("no home folder for an autosave")?,
    };
    let dir = auto.parent().ok_or_else(|| format!("no folder for {}", auto.display()))?;
    if std::fs::create_dir_all(dir).is_err() {
        return Err(format!("cannot create the folder {}", dir.display()));
    }
    // Written beside the autosave first, then compared: the bytes say whether anything changed.
    let check = auto.with_extension("check.edith");
    // The autosave write never refuses a project the engine would refuse to save (a clip past
    // its source, say); the only refusal left is a project that names no file at all.
    if let Err(e) = session.save_autosave(&check) {
        let _ = std::fs::remove_file(&check);
        return Err(format!("the engine would not write {} ({e})", auto.display()));
    }
    let same_as_autosave = auto.is_file() && same_project(&check, &auto);
    let same_as_file = server.path.as_deref().is_some_and(|p| p.is_file() && same_project(&check, p));
    if same_as_autosave || same_as_file {
        let _ = std::fs::remove_file(&check);
        return Ok(None);
    }
    // An autosave this session did not recover is never overwritten: whatever it holds was
    // written by an earlier run (or another client) and was not loaded here, so it is moved to
    // a dated name first. A recovered autosave is this session's own and is replaced in place.
    // One dated copy per collision, not a fresh copy of the same unrecovered file on every run.
    let mut moved_aside = None;
    if let Some(p) = beside.filter(|_| auto.is_file() && server.recovered_autosave.as_deref() != Some(auto.as_path())) {
        let (kept, pruned) = match set_aside_dated(&auto, p) {
            Ok(done) => done,
            Err(e) => {
                let _ = std::fs::remove_file(&check);
                return Err(format!("could not move the older autosave {} aside ({e})", auto.display()));
            }
        };
        eprintln!("autosave: an earlier session's autosave was kept as {}", kept.display());
        if let Some(note) = pruned_note(&pruned) {
            eprintln!("autosave: {note}");
        }
        moved_aside = Some(kept);
    }
    match std::fs::rename(&check, &auto) {
        Ok(()) => {
            // This session's own write: an earlier session's autosave is named as moved, not as written.
            match moved_aside {
                Some(kept) => eprintln!("autosave: this session's unsaved changes written to {} (the earlier autosave is {})", auto.display(), kept.display()),
                None => eprintln!("autosave: unsaved changes written to {}", auto.display()),
            }
            Ok(Some(auto))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&check);
            Err(format!("could not move the autosave into place at {} ({e})", auto.display()))
        }
    }
}

/// The note for a project path with no file of its own that has an autosave beside it:
/// the project was never saved, and the autosave is the only copy of its work.
fn unsaved_autosave_note(path: &Path) -> Option<String> {
    let auto = autosave_path(path);
    auto.is_file().then(|| {
        let when = mtime(&auto).map_or_else(|| "at an unknown time".to_string(), ago);
        format!(
            "the project was never saved; unsaved changes from the last session are in its autosave {} (written {when}): project_open {{\"path\": \"{}\", \"recover\": true}} to load them",
            auto.display(),
            path.display()
        )
    })
}

/// The two ways out of an unrecovered autosave while this session has the project open: keep
/// this session's edits (the autosave is set aside as a dated copy), or load the autosave and
/// drop this session's edits. Both are named, so neither refusal points back at the other alone.
fn unrecovered_choices(path: &Path) -> String {
    let p = path.display();
    format!(
        "project_save {{discard_autosave:true}} keeps THIS session's edits and sets the earlier autosave aside as a dated copy (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted). project_open {{\"path\": \"{p}\", \"recover\": true, \"discard\": true}} loads the earlier autosave and drops THIS session's unsaved edits."
    )
}

/// The reply when a project file will not open. An unsupported version names the versions this
/// build reads; the way out is the autosave or the dated copies beside the file, or else its
/// checkpoints, each named with the call that restores it.
fn open_failure(path: &Path, engine_error: &str) -> String {
    let mut why = engine_error.to_string();
    if why.contains("unsupported version") {
        why.push_str(&format!("; this edith reads project versions up to {}", engine::edith::NEWEST_VERSION));
    }
    let p = path.display();
    let mut outs = Vec::new();
    if autosave_path(path).is_file() {
        outs.push(format!("the autosave {} (project_open {{\"path\": \"{p}\", \"recover\": true}})", autosave_path(path).display()));
    }
    let dated: Vec<String> = dated_autosaves(path)
        .iter()
        .filter_map(|(_, d)| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    if !dated.is_empty() {
        outs.push(format!("the dated autosaves {} (project_open {{\"path\": \"{p}\", \"recover\": \"<name>\"}})", dated.join(", ")));
    }
    if outs.is_empty() {
        let cps = checkpoints_beside(path);
        if !cps.is_empty() {
            outs.push(format!("the checkpoints {} (restore {{\"name\": \"<name>\"}} after opening a project that opens)", cps.join(", ")));
        }
    }
    let way_out = if outs.is_empty() {
        "no autosave, dated copy or checkpoint was found beside it".to_string()
    } else {
        format!("the way out is {}", outs.join("; or "))
    };
    format!("could not open {p}: {why}; {way_out}")
}

/// The checkpoint names (`<name>.edith`) saved for the project at `path`, in
/// `<project dir>/checkpoints/<stem>/`, sorted. Empty when there are none.
fn checkpoints_beside(path: &Path) -> Vec<String> {
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let dir = path.parent().unwrap_or(Path::new(".")).join("checkpoints").join(stem);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".edith")).map(str::to_string))
        .collect();
    names.sort();
    names
}

/// The reply when a save cannot write its file. A permission refusal says which folder to pick.
fn write_failure(target: &Path, engine_error: &str) -> String {
    if engine_error.contains("ermission denied") {
        return format!("cannot write {}: permission denied; choose a folder you can write to", target.display());
    }
    format!("save failed: could not write {} ({engine_error})", target.display())
}

/// The reason a named refusal (an unsupported rotation or display size) gives for a recognised media
/// file. A file the demuxer cannot parse at all, such as plain text, has no such reason: None.
fn named_media_refusal(path: &Path) -> Option<String> {
    if engine::is_audio(path) || engine::is_image(path) {
        return None;
    }
    let e = engine::demux::Demuxer::open(path).err()?;
    let named = engine::demux::UnsupportedRotation::is_it(&e) || engine::demux::UnsupportedDisplaySize::is_it(&e);
    named.then(|| e.to_string())
}

/// Whether project_open can start a project from `path`: a picture, a sound, or a
/// file whose header gives a picture or a playing time. Anything else is refused by name.
fn is_media(path: &Path) -> bool {
    if engine::is_audio(path) || engine::is_image(path) {
        return true;
    }
    let facts = engine::probe_media(path);
    facts.video.is_some() || facts.duration_secs.is_some()
}

/// How long a span is, in the largest whole unit: `12 s`, `5 min`, `3 h`, `2 d`.
fn span(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        7200..=172_799 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86_400),
    }
}

/// `5 min ago` for a time in the past.
fn ago(t: SystemTime) -> String {
    match SystemTime::now().duration_since(t) {
        Ok(d) => format!("{} ago", span(d)),
        Err(_) => "in the future".to_string(),
    }
}

/// The refusal `project_open` gives for an autosave beside the project that holds an earlier
/// session's unsaved edits, unless the person chose: `recover` continues from it, and
/// `discard_autosave` opens the saved file and sets the autosave aside. None when there is
/// nothing to choose about (no autosave, or a stale one that is removed without a word).
fn unloaded_autosave_refusal(project: &Path, file: &PlaybackSession, discard_autosave: bool) -> Option<String> {
    let auto = unloaded_autosave(project)?;
    if discard_autosave {
        return None;
    }
    let when = mtime(&auto).map_or_else(|| "an unknown time".to_string(), local_text);
    let disk = std::fs::read_to_string(project).unwrap_or_default();
    let summary = describe_autosave(file, &disk, &auto);
    Some(format!(
        "{} has an autosave from {when} (an earlier session's unsaved edits: {summary}). Open with recover:true to continue from it, or discard_autosave:true to open the saved file and set the autosave aside (dated copy). Nothing was loaded.",
        project.display()
    ))
}

/// The autosave beside `project` that holds edits the project file does not: newer than it, and
/// not the same project. A project file that is missing counts as older than any autosave.
fn unloaded_autosave(project: &Path) -> Option<PathBuf> {
    let auto = autosave_path(project);
    if !auto.is_file() {
        return None;
    }
    let newer = match (mtime(&auto), mtime(project)) {
        (Some(a), Some(f)) => a > f,
        (Some(_), None) => true,
        _ => false,
    };
    (newer && !same_project(&auto, project)).then_some(auto)
}

/// Settles the autosave beside `project` once an open is going ahead. An unloaded one is set
/// aside as a dated copy when the person said `discard_autosave` (its edits are not loaded, and
/// they stay on disk until that copy is deleted); without it the open was refused before this.
/// A stale one (same as the file, or older) is removed. `Err` when the set-aside fails.
fn settle_autosave_on_open(project: &Path, discard_autosave: bool) -> Result<Option<String>, String> {
    let auto = autosave_path(project);
    if !auto.is_file() {
        return Ok(None);
    }
    if unloaded_autosave(project).is_none() {
        let _ = std::fs::remove_file(&auto);
        return Ok(None);
    }
    if !discard_autosave {
        return Ok(None);
    }
    match set_aside_dated(&auto, project) {
        Ok((kept, pruned)) => {
            let mut note = format!(
                "set the autosave aside as {} (its unsaved edits are not loaded; the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted))",
                kept.display()
            );
            if let Some(extra) = pruned_note(&pruned) {
                note.push_str(&format!("\n{extra}"));
            }
            Ok(Some(note))
        }
        Err(e) => Err(format!("could not set the autosave {} aside ({e}), so nothing was opened", auto.display())),
    }
}

/// Clip, cue and lane counts by lane name.
fn lane_counts(s: &PlaybackSession) -> BTreeMap<String, usize> {
    s.lanes()
        .into_iter()
        .map(|l| {
            let n = match l.kind {
                LaneKind::Subtitle => s.sub_lane_cues(l).len(),
                _ => s.lane_clips(l).len(),
            };
            (parse::lane_name(l), n)
        })
        .collect()
}

/// What the autosave's edits change against the file, in a few words: `+3 clips on A2`,
/// `new lane A3 with 2 clips`, `+1 source(s)`, `length 00:05.000 -> 00:09.000`.
fn autosave_summary(file: &PlaybackSession, auto: &PlaybackSession, settings: &[String]) -> String {
    let (before, after) = (lane_counts(file), lane_counts(auto));
    let mut parts: Vec<String> = Vec::new();
    for (name, &n) in &after {
        let unit = if name.starts_with('S') { "cue(s)" } else { "clip(s)" };
        match before.get(name) {
            None => parts.push(format!("new lane {name} with {n} {unit}")),
            Some(&b) if b != n => parts.push(format!("{:+} {unit} on {name}", n as i64 - b as i64)),
            Some(_) => {}
        }
    }
    for name in before.keys().filter(|k| !after.contains_key(*k)) {
        parts.push(format!("lane {name} removed"));
    }
    // Sources: a changed path is named by its index and both file names; an added or removed
    // source is named by its file. Kept apart from the content parts, so an import-only change
    // can say that no clip, lane or setting changed.
    let name = |p: &Path| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
    let (fsrc, asrc) = (file.sources(), auto.sources());
    let mut src_parts: Vec<String> = Vec::new();
    for (i, (f, a)) in fsrc.iter().zip(asrc.iter()).enumerate() {
        if f.path != a.path {
            src_parts.push(format!("src:{i} {} -> {}", name(&f.path), name(&a.path)));
        }
    }
    let (sb, sa) = (fsrc.len(), asrc.len());
    if sa > sb {
        let added: Vec<String> = asrc[sb..].iter().enumerate().map(|(k, s)| format!("src:{} {}", sb + k, name(&s.path))).collect();
        src_parts.push(format!("+{} source(s): {}", sa - sb, added.join(", ")));
    } else if sb > sa {
        let gone: Vec<String> = fsrc[sa..].iter().enumerate().map(|(k, s)| format!("src:{} {}", sa + k, name(&s.path))).collect();
        src_parts.push(format!("{} source(s) removed: {}", sb - sa, gone.join(", ")));
    }
    let (eb, ea) = (parse::session_end(file), parse::session_end(auto));
    if eb != ea {
        let fps = parse::session_fps(auto);
        parts.push(format!("length {} -> {}", parse::clock(eb, fps), parse::clock(ea, fps)));
    }
    parts.extend(settings.iter().cloned());
    if parts.is_empty() && src_parts.is_empty() {
        "no clip, lane, setting or source differs".to_string()
    } else if parts.is_empty() {
        format!("{} (no clip, lane or setting changed)", src_parts.join(", "))
    } else {
        parts.extend(src_parts);
        parts.join(", ")
    }
}

/// The caption style as `project_status` reports it.
fn caption_json(cs: engine::captionanim::CaptionStyle) -> Value {
    json!({
        "animation": cs.anim.name(),
        "highlight": format!("#{:02x}{:02x}{:02x}", cs.highlight[0], cs.highlight[1], cs.highlight[2]),
        "box": cs.hl_box,
        "uppercase": cs.uppercase,
        "words_per_line": cs.words_per_line,
    })
}

fn project_status(server: &mut Server, _args: &Value) -> Reply {
    status_reply(server)
}

fn status_reply(server: &Server) -> Reply {
    let s = tri!(server.session_ref());
    let fps = parse::session_fps(s);
    let (w, h) = s.resolution();
    let end = parse::session_end(s);
    // Not "saved" while a newer autosave of unrecovered edits sits beside the file: what is
    // open is the file on disk, which is older than those edits.
    let state = if dirty(server) {
        "dirty"
    } else if server.unrecovered_autosave {
        "on disk, older than the autosave"
    } else {
        "saved"
    };
    let path = server
        .path
        .as_ref()
        .map_or_else(|| "(unsaved)".to_string(), |p| p.display().to_string());
    let undo = server.ledger.len();
    let redo = server.redo_ledger.len();
    // The newest five undo steps, newest first, so a status read says where the edit history stands.
    let recent_steps: Vec<String> = server.ledger.iter().rev().take(5).map(|e| e.label.clone()).collect();
    let recent_text = match recent_steps.is_empty() {
        true => String::new(),
        false => format!("last steps: {}\n", recent_steps.join(" · ")),
    };
    let running = server.jobs.iter().filter(|j| j.outcome.is_none()).count();

    let lanes: Vec<(String, usize)> = s
        .lanes()
        .into_iter()
        .map(|l| {
            let n = match l.kind {
                LaneKind::Subtitle => s.sub_lane_cues(l).len(),
                _ => s.lane_clips(l).len(),
            };
            (parse::lane_name(l), n)
        })
        .collect();
    let lane_text = if lanes.is_empty() {
        "no lanes".to_string()
    } else {
        lanes
            .iter()
            .map(|(name, n)| {
                if name.starts_with('S') {
                    format!("{name} {n} cue(s)")
                } else {
                    format!("{name} {n} clip(s)")
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };

    // Every duck on an audio lane: the lane it dips, the key lane it listens to, and its depth.
    let ducks: Vec<(String, engine::duck::Duck)> = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio)
        .flat_map(|l| s.lane_duck_list(l).into_iter().map(move |d| (parse::lane_name(l), d)))
        .collect();
    let duck_key = |d: &engine::duck::Duck| parse::lane_name(Lane::new(LaneKind::Audio, d.under));
    let ducks_text = if ducks.is_empty() {
        String::new()
    } else {
        format!(
            "\nducks: {}",
            ducks
                .iter()
                .map(|(lane, d)| format!("{lane} under {} {}dB", duck_key(d), d.depth_db))
                .collect::<Vec<_>>()
                .join(" · ")
        )
    };

    // Every audio lane with voice processors on, and which.
    let voiced: Vec<String> = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio)
        .filter_map(|l| {
            let v = s.lane_voice(l);
            v.is_active().then(|| format!("{} {}", parse::lane_name(l), v.summary()))
        })
        .collect();
    let voice_text = if voiced.is_empty() { String::new() } else { format!("\nvoice: {}", voiced.join(" · ")) };

    let sources = s.sources();
    let source_names: Vec<String> = sources
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let name = src
                .path
                .file_name()
                .map_or_else(|| src.path.display().to_string(), |n| n.to_string_lossy().into_owned());
            format!("src:{i} {name}")
        })
        .collect();

    let limiter = s.limiter();
    let sample_rate = match s.sample_rate() {
        Some(r) => format!("{r} Hz"),
        None => "derived from the sources: the highest rate among them, 48 kHz when all are below 44.1 kHz".to_string(),
    };
    let limiter_text = if limiter.on {
        format!("on at {:.1} dBTP true peak", limiter.ceiling_db)
    } else {
        "off".to_string()
    };
    let tone = s.tone().name();
    let encoder = s.encoder_seat().name();

    // Each offline source is named on its own line, with the fix; a clip on it is still on the timeline.
    let offline = s.offline_sources();
    let offline_text: String = offline
        .iter()
        .map(|(i, p)| format!("\nOFFLINE: src:{i} {} (relink with media_relink)", p.display()))
        .collect();
    // A LUT a clip names that cannot be read: the clip plays without it until it is relinked.
    let luts = s.lut_problems();
    let offline_text: String = offline_text + &lut_lines(&luts).iter().map(|l| format!("\n{l}")).collect::<String>();
    // What the open clamped to load: the file on disk still has it, so the project reads unsaved.
    let fix_text = if server.load_fixes.is_empty() {
        String::new()
    } else {
        format!(
            "\nunsaved: the open fixed {} thing(s) to load ({}); project_save writes the fix",
            server.load_fixes.len(),
            server.load_fixes.join("; ")
        )
    };
    // The notes, in full, after the settings, when there are any.
    let notes_text = match s.notes().is_empty() {
        true => String::new(),
        false => format!("\nnotes: {}", s.notes()),
    };
    let render_text = crate::tools::render::status_line(server).map_or_else(String::new, |l| format!("\n{l}"));
    let text = format!(
        "{path} · {state} · {w}x{h} @{} · {} ({end}f) · undo {undo} · redo {redo} · {running} export job(s) running\n\
         lanes: {lane_text}{ducks_text}{voice_text}\n\
         {recent_text}sources: {} {}\n\
         settings: tonemap {tone} · proxies {} · auto proxies {} · encoder {encoder} · limiter {limiter_text} · sample rate {sample_rate} · still {:.1} s{offline_text}{fix_text}{notes_text}{render_text}",
        fmt_fps(fps),
        parse::clock(end, fps),
        sources.len(),
        if source_names.is_empty() { String::new() } else { format!("({})", source_names.join(", ")) },
        on_off(s.proxies()),
        on_off(s.auto_proxies()),
        s.still_secs(),
    );

    let unrecovered = server
        .path
        .as_deref()
        .filter(|_| server.unrecovered_autosave)
        .map(|p| autosave_path(p).display().to_string());
    let structured = json!({
        "path": server.path.as_ref().map(|p| p.display().to_string()),
        "saved": !dirty(server),
        "dirty": dirty(server),
        "resolution": {"width": w, "height": h},
        "fps": fps,
        "duration_frames": end,
        "duration_secs": parse::secs(end, fps),
        "lanes": lanes.iter().map(|(name, n)| json!({"lane": name, "count": n})).collect::<Vec<_>>(),
        "ducks": ducks.iter().map(|(lane, d)| json!({
            "lane": lane,
            "under": duck_key(d),
            "depth_db": d.depth_db,
            "threshold_db": d.threshold_db,
        })).collect::<Vec<_>>(),
        "sources": sources.len(),
        "offline": offline.iter().map(|(i, p)| json!({"src": i, "path": p.display().to_string()})).collect::<Vec<_>>(),
        "lut_problems": luts.iter().map(|(p, e)| json!({"path": p.display().to_string(), "problem": e})).collect::<Vec<_>>(),
        "load_fixes": server.load_fixes,
        "undo_steps": undo,
        "redo_steps": redo,
        "recent_steps": recent_steps,
        "jobs_running": running,
        "unrecovered_autosave": unrecovered,
        "settings": {
            "captions": caption_json(s.caption_style()),
            "tonemap": tone,
            "proxies": s.proxies(),
            "auto_proxies": s.auto_proxies(),
            "encoder": encoder,
            "limiter": {"on": limiter.on, "ceiling_db": limiter.ceiling_db},
            "sample_rate": s.sample_rate(),
            "still_secs": s.still_secs(),
            "notes": s.notes(),
        }
    });
    Reply::ok(text).with(structured)
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub(crate) fn project_settings(server: &mut Server, args: &Value) -> Reply {
    let before = tri!(server.session_ref()).history_pushes();
    let force = opt_bool(args, "force").unwrap_or(false);
    let mut lines: Vec<String> = Vec::new();
    let mut changed = 0usize;
    let mut refused = 0usize;
    let mut asked = 0usize;
    // What each change was, for the ledger label: `project_settings fps 30→5`.
    let mut named: Vec<String> = Vec::new();
    {
        let s = tri!(server.session());

        if let Some(raw) = opt_str(args, "resolution") {
            asked += 1;
            match parse_resolution(raw) {
                Err(r) => {
                    refused += 1;
                    lines.push(format!("resolution: refused ({})", r.text));
                }
                Ok((w, h)) => {
                    let (cw, ch) = s.resolution();
                    if (cw, ch) == (w, h) {
                        lines.push(format!("resolution already {w}x{h}"));
                    } else if s.set_resolution(w, h) {
                        changed += 1;
                        named.push(format!("resolution {cw}x{ch}→{w}x{h}"));
                        lines.push(format!(
                            "resolution {cw}x{ch} -> {w}x{h}: clips keep their fit mode and refit to the new canvas; transforms are canvas fractions, so their positions scale with it; titles keep their placement (top, bottom or centre) and their size is a share of the frame height"
                        ));
                    } else {
                        refused += 1;
                        lines.push(format!("resolution {w}x{h}: refused by the engine"));
                    }
                }
            }
        }

        match fps_arg(args) {
            Err(r) => {
                asked += 1;
                refused += 1;
                lines.push(format!("fps: refused ({})", r.text));
            }
            Ok(None) => {}
            Ok(Some(f)) => {
                asked += 1;
                let cur = s.meta().frame_rate;
                let edges_before = clip_edges(s);
                let locked_before = locked_starts(s);
                let locked = locked_lanes_moved(s, cur, f);
                let locked_list = locked.iter().map(|(name, n)| format!("{name} ({n} {})", if *n == 1 { "clip edge" } else { "clip edges" })).collect::<Vec<_>>().join(", ");
                if (cur - f).abs() < 1e-9 {
                    lines.push(format!("fps already {}", fmt_fps(f)));
                } else if !locked.is_empty() && !force {
                    refused += 1;
                    lines.push(format!(
                        "fps {} -> {}: refused: locked lane {locked_list} would have clips moved by the retime. Unlock the lane first (edit lane_flags {{lane, locked:false}}), or pass force:true to move them anyway",
                        fmt_fps(cur),
                        fmt_fps(f)
                    ));
                } else if s.set_frame_rate(f) {
                    changed += 1;
                    named.push(format!("fps {}→{}", fmt_fps(cur), fmt_fps(f)));
                    lines.push(format!("fps {} -> {}", fmt_fps(cur), fmt_fps(f)));
                    if !locked.is_empty() {
                        lines.push(format!("warning: force:true moved the clips of locked lane {locked_list}"));
                    }
                    // A locked lane whose times stay put still takes new frame numbers: say which.
                    let moved_names: Vec<String> = locked.iter().map(|(name, _)| name.clone()).collect();
                    if let Some(note) = locked_address_note(&locked_before, &locked_starts(s), &moved_names) {
                        lines.push(note);
                    }
                    // A rate that is not a whole multiple of the old one puts some edges between
                    // frames; the retime rounds each to the nearest frame, and says how many moved.
                    let moved = moved_edges(&edges_before, &clip_edges(s), cur, f);
                    if moved > 0 {
                        lines.push(format!(
                            "warning: {moved} clip {} moved to the nearest frame at {} fps; undo restores them. An fps change is not reversible by changing back: setting {} again does not restore the exact frames, only undo does",
                            if moved == 1 { "edge" } else { "edges" },
                            fmt_fps(f),
                            fmt_fps(cur)
                        ));
                    }
                } else {
                    refused += 1;
                    lines.push(if s.rate_locked() {
                        format!("fps {}: refused: the project has several sequences or a compound clip, which are made at one frame rate; keep {} fps", fmt_fps(f), fmt_fps(cur))
                    } else {
                        format!("fps {}: refused by the engine", fmt_fps(f))
                    });
                }
            }
        }

        if let Some(raw) = opt_str(args, "tonemap") {
            asked += 1;
            match Preset::from_name(raw.as_bytes()) {
                None => {
                    refused += 1;
                    lines.push(format!("tonemap `{raw}`: refused; use reference, standard or vivid"));
                }
                Some(p) => {
                    let cur = s.tone();
                    if cur == p {
                        lines.push(format!("tonemap already {}", p.name()));
                    } else if s.set_tone(p) {
                        changed += 1;
                        named.push(format!("tonemap {}→{}", cur.name(), p.name()));
                        lines.push(format!("tonemap {} -> {}", cur.name(), p.name()));
                    } else {
                        refused += 1;
                        lines.push(format!("tonemap {}: refused by the engine", p.name()));
                    }
                }
            }
        }

        if let Some(raw) = opt_str(args, "notes") {
            asked += 1;
            if s.set_notes(raw) {
                changed += 1;
                named.push("notes".to_string());
                lines.push(match raw.is_empty() {
                    true => "notes cleared".to_string(),
                    false => format!("notes set ({} characters)", raw.chars().count()),
                });
            } else {
                lines.push("notes already set to that".to_string());
            }
        }

        if let Some(v) = args.get("still_secs") {
            asked += 1;
            match v.as_f64() {
                Some(secs) if (STILL_MIN_SECS..=STILL_MAX_SECS).contains(&secs) => {
                    let cur = s.still_secs();
                    if s.set_still_secs(secs) {
                        changed += 1;
                        named.push(format!("still {cur:.1}s→{secs:.1}s"));
                        lines.push(format!("still length {cur:.1} s -> {secs:.1} s"));
                    } else {
                        lines.push(format!("still length already {cur:.1} s"));
                    }
                }
                _ => {
                    refused += 1;
                    lines.push(format!("still_secs {v}: refused; use a length from {STILL_MIN_SECS} to {STILL_MAX_SECS} seconds"));
                }
            }
        }

        for (key, get, set) in [
            ("proxies", s.proxies(), true),
            ("auto_proxies", s.auto_proxies(), false),
        ] {
            if let Some(want) = opt_bool(args, key) {
                asked += 1;
                if want == get {
                    lines.push(format!("{key} already {}", on_off(want)));
                } else {
                    if set {
                        s.set_proxies(want);
                    } else {
                        s.set_auto_proxies(want);
                    }
                    changed += 1;
                    named.push(format!("{key} {}→{}", on_off(get), on_off(want)));
                    lines.push(format!("{key} {} -> {}", on_off(get), on_off(want)));
                }
            }
        }

        if let Some(raw) = opt_str(args, "encoder") {
            asked += 1;
            match EncoderSeat::from_name(raw.as_bytes()) {
                None => {
                    refused += 1;
                    lines.push(format!("encoder `{raw}`: refused; use auto, hardware or software"));
                }
                Some(seat) => {
                    let cur = s.encoder_seat();
                    if cur == seat {
                        lines.push(format!("encoder already {}", seat.name()));
                    } else {
                        s.set_encoder_seat(seat);
                        changed += 1;
                        named.push(format!("encoder {}→{}", cur.name(), seat.name()));
                        lines.push(format!("encoder {} -> {}", cur.name(), seat.name()));
                    }
                }
            }
        }

        if let Some(caps) = args.get("captions") {
            asked += 1;
            let cur = s.caption_style();
            let want = match caps.as_object() {
                None => Err("give an object such as {\"animation\": \"karaoke\", \"words_per_line\": 3}".to_string()),
                Some(obj) => match obj.keys().find(|k| !["animation", "highlight", "box", "uppercase", "words_per_line"].contains(&k.as_str())) {
                    Some(k) => Err(format!("unknown key `{k}`")),
                    None => super::export::caption_from(obj, cur),
                },
            };
            match want {
                Err(msg) => {
                    refused += 1;
                    lines.push(format!("captions: refused ({msg})"));
                }
                Ok(want) if want.clamped() == cur => lines.push("captions already set so".into()),
                Ok(want) => {
                    s.set_caption_style(want);
                    changed += 1;
                    named.push(format!("captions {}→{}", cur.anim.name(), want.anim.name()));
                    lines.push(format!(
                        "captions {} -> {} (highlight #{:02x}{:02x}{:02x}{}{}, {} words per line)",
                        cur.anim.name(),
                        want.anim.name(),
                        want.highlight[0],
                        want.highlight[1],
                        want.highlight[2],
                        if want.hl_box { ", box" } else { "" },
                        if want.uppercase { ", uppercase" } else { "" },
                        want.words_per_line
                    ));
                }
            }
        }

        if let Some(lim) = args.get("limiter") {
            asked += 1;
            match lim.as_object() {
                None => {
                    refused += 1;
                    lines.push("limiter: refused; give an object such as {\"on\": true, \"ceiling_db\": -1.0}".into());
                }
                Some(obj) => match limiter_from(obj, s.limiter()) {
                    Err(msg) => {
                        refused += 1;
                        lines.push(format!("limiter: refused ({msg})"));
                    }
                    Ok(want) => {
                        let cur = s.limiter();
                        if cur.on == want.on && cur.ceiling_db == want.ceiling_db {
                            lines.push("limiter already set so".into());
                        } else if s.set_limiter(want) {
                            changed += 1;
                            named.push(format!("limiter {}→{}", on_off(cur.on), on_off(want.on)));
                            lines.push(format!(
                                "limiter {} at {:.1} -> {} at {:.1} dBTP",
                                on_off(cur.on),
                                cur.ceiling_db,
                                on_off(want.on),
                                want.ceiling_db
                            ));
                        } else {
                            refused += 1;
                            lines.push("limiter: refused by the engine".into());
                        }
                    }
                },
            }
        }
    }
    if named.is_empty() {
        note_pushes(server, before);
    } else {
        note_pushes_as(server, before, &format!("project_settings {}", named.join(", ")));
    }

    if asked == 0 {
        return Reply::err(
            "no setting given; pass one of resolution, fps, tonemap, notes, still_secs, proxies, auto_proxies, encoder, limiter, captions",
        );
    }
    let mut out = Map::new();
    out.insert("changed".into(), json!(changed));
    out.insert("refused".into(), json!(refused));
    let text = lines.join("\n");
    let reply = if refused > 0 && changed == 0 {
        Reply::err(text)
    } else {
        Reply::ok(text)
    };
    reply.with(Value::Object(out))
}

/// Applies the `on` and `ceiling_db` keys over the current limiter.
fn limiter_from(obj: &Map<String, Value>, cur: Limiter) -> Result<Limiter, String> {
    let on = match obj.get("on") {
        None | Some(Value::Null) => cur.on,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("`on` must be true or false".into()),
    };
    let ceiling_db = match obj.get("ceiling_db") {
        None | Some(Value::Null) => cur.ceiling_db,
        Some(v) => match v.as_f64() {
            Some(c) if c.is_finite() && (f64::from(Limiter::MIN_DB)..=f64::from(Limiter::MAX_DB)).contains(&c) => c as f32,
            _ => return Err("`ceiling_db` must be a number from -24 to 0 dBTP (a true-peak ceiling)".into()),
        },
    };
    Ok(Limiter { on, ceiling_db })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::fs::File;
    use std::time::Duration;

    #[test]
    fn fps_and_resolution_are_range_checked() {
        assert!(fps_arg(&json!({"fps": 1000})).is_err());
        assert!(fps_arg(&json!({"fps": 0.5})).is_err());
        assert!(fps_arg(&json!({"fps": 240})).is_ok_and(|f| f == Some(240.0)));
        assert!(parse_resolution("1921x1080").is_err(), "odd width");
        assert!(parse_resolution("8x8").is_err(), "below 16");
        assert!(parse_resolution("16000x9000").is_err(), "above 8192");
        assert!(parse_resolution("1920x1080").is_ok());
    }

    #[test]
    fn dated_autosaves_are_found_newest_first_and_none_is_deleted() {
        let td = TestDir::new("dated-autosaves");
        let project = td.path().join("p1.edith");
        std::fs::write(&project, b"x").unwrap();
        for (i, day) in [3u64, 1, 2].iter().enumerate() {
            let p = td.path().join(format!("p1.autosave-2026010{i}-120000.edith"));
            std::fs::write(&p, b"x").unwrap();
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600 + day * 86_400);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        std::fs::write(td.path().join("p1.autosave.edith"), b"x").unwrap();
        let found: Vec<String> = dated_autosaves(&project)
            .iter()
            .map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            vec!["p1.autosave-20260100-120000.edith", "p1.autosave-20260102-120000.edith", "p1.autosave-20260101-120000.edith"],
            "newest first by modification time"
        );
        assert!(td.path().join("p1.autosave.edith").is_file(), "the project's own autosave is not listed or touched");
        assert_eq!(std::fs::read_dir(td.path()).unwrap().count(), 5, "nothing is deleted");
        assert!(dated_autosaves(&td.path().join("nothere.edith")).is_empty());
    }

    #[test]
    fn a_media_file_the_probe_refuses_says_why_it_cannot_open() {
        // A 45-degree display matrix: the probe refuses it, so project_open gives the probe's reason.
        let mut server = Server::new();
        let path = asset("test_rotation45.mp4");
        let r = project_open(&mut server, &json!({"path": path, "discard": true}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("test_rotation45.mp4 is media edith cannot open: "), "{}", r.text);
        assert!(r.text.contains("quarter turn"), "{}", r.text);
        assert!(!r.text.contains("is not an edith project or a media file"), "{}", r.text);
        assert!(server.session.is_none(), "a refused file opens nothing");
        // A text file is not media, so it keeps the plain refusal, not the demuxer's parse error.
        let dir = TestDir::new("project-plain-text-refusal");
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, b"plain text, not media").unwrap();
        assert!(named_media_refusal(&notes).is_none());
    }

    #[test]
    fn autosave_is_matched_by_file_name_or_stamp() {
        let p = Path::new("/x/p1.autosave-20261016-223636-2.edith");
        assert!(autosave_matches(p, "p1.autosave-20261016-223636-2.edith"));
        assert!(autosave_matches(p, "20261016-223636-2"));
        assert!(!autosave_matches(p, "20261016-223636"), "a collision suffix is part of the stamp");
        assert!(!autosave_matches(p, "p1.autosave.edith"));
    }

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn quiet() {
        engine::ao::set_silent_output();
    }

    /// A project path that does not exist yet, unique per test. The guard owns the folder,
    /// so it is removed when the test ends, panic or not.
    fn scratch(test: &str) -> (TestDir, PathBuf) {
        let dir = TestDir::new(&format!("project-{test}"));
        let path = dir.join("proj.edith");
        (dir, path)
    }

    fn call(f: fn(&mut Server, &Value) -> Reply, server: &mut Server, args: Value) -> Reply {
        f(server, &args)
    }

    fn import_av(server: &mut Server) {
        let before = server.session.as_ref().unwrap().history_pushes();
        server
            .session
            .as_mut()
            .unwrap()
            .import(&asset("test_av.mp4"))
            .expect("import test asset");
        note_pushes(server, before);
    }

    #[test]
    fn new_import_save_open_roundtrip() {
        quiet();
        let (_dir, path) = scratch("roundtrip");
        let mut server = Server::new();
        let r = call(project_new, &mut server, json!({"path": path, "resolution": "1280x720"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(!path.exists(), "new must not write the file");
        assert!(dirty(&server));

        import_av(&mut server);
        let r = call(project_save, &mut server, json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(path.exists());
        assert!(!dirty(&server));

        let mut fresh = Server::new();
        let r = call(project_open, &mut fresh, json!({"path": path}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1280x720"), "{}", r.text);
        assert_eq!(fresh.session.as_ref().unwrap().sources().len(), 1);
        assert!(!dirty(&fresh));
        assert!(fresh.ledger.is_empty());
    }

    #[test]
    fn open_media_file_names_project_beside_it() {
        quiet();
        let mut server = Server::new();
        let r = call(project_open, &mut server, json!({"path": asset("test_av.mp4")}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(server.path.as_ref().unwrap().file_name().unwrap(), "test_av.edith");
        assert!(!server.path.as_ref().unwrap().exists());
        assert!(dirty(&server), "a media-started project is unsaved");
    }

    #[test]
    fn dirty_project_refuses_open_unless_discard() {
        quiet();
        let (_dir, path) = scratch("dirty");
        let mut server = Server::new();
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        import_av(&mut server);
        assert!(!call(project_save, &mut server, json!({})).is_error);

        // A settings change is an undo step, so the project is now dirty.
        let r = call(project_settings, &mut server, json!({"tonemap": "vivid"}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(server.ledger.len(), 1);
        assert!(dirty(&server));

        let other = asset("test_av.mp4");
        let refused = call(project_open, &mut server, json!({"path": other}));
        assert!(refused.is_error);
        assert!(refused.text.contains("unsaved changes"), "{}", refused.text);
        assert_eq!(server.ledger.len(), 1, "a refusal changes nothing");

        let ok = call(project_open, &mut server, json!({"path": other, "discard": true}));
        assert!(!ok.is_error, "{}", ok.text);
        assert!(server.ledger.is_empty());
    }

    /// Undo back to the saved content is clean, redo away from it is dirty, and a new edit
    /// made after undoing past the save is dirty even though undo then reaches its depth.
    #[test]
    fn undo_back_to_the_saved_content_is_clean() {
        quiet();
        let (_dir, path) = scratch("clean-undo");
        let mut server = Server::new();
        let run = |server: &mut Server, name: &str| {
            let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
            (t.run)(server, &json!({}))
        };
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        import_av(&mut server);
        assert!(!call(project_settings, &mut server, json!({"tonemap": "vivid"})).is_error);
        assert!(!call(project_save, &mut server, json!({})).is_error);
        assert!(!dirty(&server));

        assert!(!call(project_settings, &mut server, json!({"tonemap": "reference"})).is_error);
        assert!(dirty(&server));
        assert!(!run(&mut server, "undo").is_error);
        assert!(!dirty(&server), "undo back to the save is clean: {}", status_reply(&server).text);
        assert!(!status_reply(&server).text.contains("dirty"), "{}", status_reply(&server).text);

        assert!(!run(&mut server, "redo").is_error);
        assert!(dirty(&server), "redo away from the save is dirty");
        assert!(!run(&mut server, "undo").is_error);
        assert!(!dirty(&server), "redo then undo is clean again");

        // Past the save, the saved entry is still reachable by undo; a new edit there drops it.
        assert!(!run(&mut server, "undo").is_error);
        assert!(dirty(&server), "undo past the save is dirty");
        assert!(!call(project_settings, &mut server, json!({"tonemap": "standard"})).is_error);
        assert!(dirty(&server));
        assert!(!run(&mut server, "undo").is_error);
        assert!(dirty(&server), "the saved content left the ledger with the new edit");
    }

    /// `undo` blames the reopen only when the open dropped steps, and a new edit clears the blame.
    #[test]
    fn undo_names_the_reopen_only_when_the_open_dropped_steps() {
        quiet();
        let (_dir, path) = scratch("undo-reopen");
        let mut server = Server::new();
        let run = |server: &mut Server, name: &str| {
            let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
            (t.run)(server, &json!({}))
        };
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        import_av(&mut server);
        assert!(!call(project_settings, &mut server, json!({"tonemap": "reference"})).is_error);
        assert!(!call(project_save, &mut server, json!({})).is_error);
        // An edit after the save is a step the reopen will drop.
        assert!(!call(project_settings, &mut server, json!({"tonemap": "vivid"})).is_error);
        assert!(!call(project_open, &mut server, json!({"path": path, "discard": true})).is_error);
        assert!(server.ledger.is_empty());
        let r = run(&mut server, "undo");
        assert!(r.is_error && r.text.starts_with("nothing to undo yet (history does not survive project_open); "), "{}", r.text);

        // A new edit lands on the reopened project, so the next undo of it names no reopen.
        assert!(!call(project_settings, &mut server, json!({"tonemap": "standard"})).is_error);
        assert!(!server.history_lost_on_open, "a new edit clears the reopen note");
        assert!(!run(&mut server, "undo").is_error);
        let r = run(&mut server, "undo");
        assert!(r.is_error && !r.text.contains("project_open"), "{}", r.text);
    }

    #[test]
    fn save_with_no_project_open_says_so_and_how_to_start() {
        quiet();
        let mut server = Server::new();
        let r = call(project_save, &mut server, json!({}));
        assert!(r.is_error);
        assert!(r.text.contains("no project is open"), "{}", r.text);
        assert!(r.text.contains("project_new") && r.text.contains("project_open"), "{}", r.text);
        assert!(!r.text.contains("no project path"), "{}", r.text);
    }

    /// The notes and the still length through `project_settings`: each is one undo step, `project_status`
    /// shows the notes when there are any and the still length always, and a length out of range is refused.
    #[test]
    fn notes_and_still_length_go_through_project_settings() {
        quiet();
        let (_dir, path) = scratch("notes_settings");
        let mut server = Server::new();
        assert!(!call(project_new, &mut server, json!({"path": path, "resolution": "1280x720"})).is_error);
        import_av(&mut server);

        let r = call(project_settings, &mut server, json!({"notes": "Line one\nLine two"}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("notes set (17 characters)"), "{}", r.text);
        let status = call(project_status, &mut server, json!({}));
        assert!(status.text.contains("\nnotes: Line one\nLine two"), "{}", status.text);
        assert!(status.text.contains("still 5.0 s"), "{}", status.text);
        assert_eq!(status.structured.as_ref().unwrap()["settings"]["notes"], "Line one\nLine two");

        let r = call(project_settings, &mut server, json!({"still_secs": 2.5}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("still length 5.0 s -> 2.5 s"), "{}", r.text);
        assert!(call(project_status, &mut server, json!({})).text.contains("still 2.5 s"));

        // Out of range is refused, and nothing changes.
        let r = call(project_settings, &mut server, json!({"still_secs": 0.1}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("refused"), "{}", r.text);
        assert_eq!(server.session.as_ref().unwrap().still_secs(), 2.5);

        // Each change is an undo step.
        let session = server.session.as_mut().unwrap();
        assert!(session.undo());
        assert_eq!(session.still_secs(), 5.0);
        assert!(session.undo());
        assert_eq!(session.notes(), "");

        // Clearing the notes says so, and project_status stops showing them.
        assert!(!call(project_settings, &mut server, json!({"notes": "x"})).is_error);
        let r = call(project_settings, &mut server, json!({"notes": ""}));
        assert!(r.text.contains("notes cleared"), "{}", r.text);
        assert!(!call(project_status, &mut server, json!({})).text.contains("notes:"));
    }

    #[test]
    fn save_refuses_when_file_changed_on_disk() {
        quiet();
        let (_dir, path) = scratch("conflict");
        let mut server = Server::new();
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        import_av(&mut server);
        assert!(!call(project_save, &mut server, json!({})).is_error);

        // Pretend the GUI wrote the file after we loaded it.
        let later = SystemTime::now() + Duration::from_secs(3600);
        File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();

        // Change the project so there is something to save.
        assert!(!call(project_settings, &mut server, json!({"fps": 25})).is_error);
        let refused = call(project_save, &mut server, json!({}));
        assert!(refused.is_error);
        assert!(refused.text.contains("changed on disk"), "{}", refused.text);

        let forced = call(project_save, &mut server, json!({"force": true}));
        assert!(!forced.is_error, "{}", forced.text);
        assert!(!dirty(&server));
    }

    #[test]
    fn new_refuses_existing_path_and_status_reports_lanes() {
        quiet();
        let (_dir, path) = scratch("exists");
        std::fs::write(&path, b"x").unwrap();
        let mut server = Server::new();
        let r = call(project_new, &mut server, json!({"path": path}));
        assert!(r.is_error);
        assert!(r.text.contains("project_open"), "{}", r.text);

        let (_fresh_dir, fresh) = scratch("status");
        let mut s = Server::new();
        assert!(!call(project_new, &mut s, json!({"path": fresh})).is_error);
        import_av(&mut s);
        let st = call(project_status, &mut s, json!({}));
        assert!(!st.is_error);
        assert!(st.text.contains("V1"), "{}", st.text);
        assert_eq!(st.structured.as_ref().unwrap()["sources"], json!(1));
    }

    #[test]
    fn settings_report_refusals_and_bad_input() {
        quiet();
        let mut server = Server::new();
        let (_dir, settings) = scratch("settings");
        assert!(!call(project_new, &mut server, json!({"path": settings})).is_error);
        let bad_res = call(project_settings, &mut server, json!({"resolution": "wide"}));
        assert!(bad_res.is_error);
        let bad_tone = call(project_settings, &mut server, json!({"tonemap": "neon"}));
        assert!(bad_tone.is_error);
        assert!(bad_tone.text.contains("refused"), "{}", bad_tone.text);
        let none = call(project_settings, &mut server, json!({}));
        assert!(none.is_error);
        assert!(server.ledger.is_empty(), "refused settings push no undo step");
        let lim = call(project_settings, &mut server, json!({"limiter": {"on": true, "ceiling_db": -1.5}}));
        assert!(!lim.is_error, "{}", lim.text);
        assert_eq!(server.ledger.len(), 1);
    }

    /// An fps change that puts a clip edge between frames says how many moved and that undo restores
    /// them, and its ledger step names the setting. An edge on a frame the new rate keeps does not count.
    #[test]
    fn an_fps_change_that_rounds_clip_edges_warns_and_names_the_step() {
        quiet();
        let by_name = |server: &mut Server, name: &str, args: Value| {
            let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
            (t.run)(server, &args)
        };
        let mut server = Server::new();
        let (_dir, path) = scratch("fps-edges");
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        let imported = by_name(&mut server, "media_import", json!({"paths": [asset("test_av.mp4").display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        let cut = by_name(&mut server, "edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "1.1s"]], "lane": "V1"}]}));
        assert!(!cut.is_error, "{}", cut.text);

        let fps = by_name(&mut server, "project_settings", json!({"fps": 5}));
        assert!(!fps.is_error, "{}", fps.text);
        assert!(fps.text.contains("clip edge") && fps.text.contains("moved to the nearest frame at 5 fps; undo restores them"), "{}", fps.text);
        assert!(fps.text.contains("not reversible by changing back"), "{}", fps.text);
        assert_eq!(server.ledger.last().map(|e| e.label.as_str()), Some("project_settings fps 30→5"));
    }

    /// Two frame numbers compare as the seconds they name at each rate: a frame kept by the new rate is
    /// not a move, and one landing between frames is.
    #[test]
    fn moved_edges_counts_the_edges_whose_time_changes() {
        // 30 -> 5: frame 60 is 2.0 s both before and after; frame 33 (1.1 s) becomes frame 6 (1.2 s).
        assert_eq!(moved_edges(&[60, 33], &[10, 6], 30.0, 5.0), 1);
        // 30 -> 29.97 moves every edge after the first frame.
        assert_eq!(moved_edges(&[0, 1200], &[0, 1200], 30.0, 29.97), 1);
        assert_eq!(moved_edges(&[0, 30], &[0, 30], 30.0, 30.0), 0);
    }

    #[test]
    fn status_formats_the_duration_once_and_counts_ledger_steps() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        server.ledger = vec![crate::server::Entry::new(1, "a"), crate::server::Entry::new(2, "b")];
        let r = status_reply(&server);
        assert!(!r.text.contains("(("), "{}", r.text);
        assert!(r.text.contains("00:00.000 (0f)"), "{}", r.text);
        assert!(r.text.contains("undo 2 · redo 0"), "{}", r.text);
    }

    #[test]
    fn save_names_a_missing_folder() {
        quiet();
        let (dir, _) = scratch("nofolder");
        let missing = dir.join("no-such-folder-xyz").join("p.edith");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        import_av(&mut server);
        let r = project_save(&mut server, &json!({"path": missing.display().to_string()}));
        assert!(r.is_error);
        assert!(
            r.text.contains(&format!("folder does not exist: {}", missing.parent().unwrap().display())),
            "{}",
            r.text
        );
    }

    #[test]
    fn save_keeps_imported_sources_that_are_not_on_the_timeline() {
        quiet();
        let (_dir, file) = scratch("unplaced");
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        server.session.as_mut().unwrap().import(&asset("test_tone.wav")).expect("import wav");
        let r = project_save(&mut server, &json!({"path": file.display().to_string()}));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("not saved"), "every imported source is saved now: {}", r.text);
        let text = std::fs::read_to_string(&file).expect("read the save");
        assert!(text.contains("test_tone.wav"), "the unplaced source reached the file:\n{text}");
    }

    #[test]
    fn open_names_a_missing_file_as_not_found() {
        let mut server = Server::new();
        let r = project_open(&mut server, &json!({"path": "/definitely/not/here.edith"}));
        assert_eq!(r.text, "not found: /definitely/not/here.edith");
    }

    #[test]
    fn open_names_the_unsaved_open_project() {
        quiet();
        let (_dir, path) = scratch("unsaved-open");
        let mut server = Server::new();
        assert!(!call(project_new, &mut server, json!({"path": path})).is_error);
        let r = call(project_open, &mut server, json!({"path": path}));
        assert!(r.is_error);
        assert!(r.text.contains("not yet saved") && r.text.contains("already open"), "{}", r.text);
        assert!(r.text.contains("call project_save to write it"), "{}", r.text);
    }

    /// A project saved past its source (hand-written here: a clip that runs to frame 10000 of
    /// a 150-frame file) opens, and the reply starts with the fix it needed, naming the clamp.
    #[test]
    fn open_reports_a_clamped_clip_first_and_says_to_save() {
        quiet();
        let (_dir, path) = scratch("open-clamped");
        let asset = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4").canonicalize().unwrap();
        std::fs::write(
            &path,
            format!("edith 1\nplayhead 0\nsource {}\nclip 0 30 0\nclip 30 10000 0\n", asset.display()),
        )
        .unwrap();
        let mut server = Server::new();
        let r = call(project_open, &mut server, json!({"path": path}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("warning: the file needed fixing to open: clip "), "{}", r.text);
        assert!(r.text.contains("clamped to") && r.text.contains("save to keep the fix"), "{}", r.text);
    }

    #[test]
    fn open_refuses_a_device_or_a_non_media_file_before_the_unsaved_guard() {
        quiet();
        let (dir, _path) = scratch("open-validate");
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, b"plain text, not media").unwrap();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        assert!(dirty(&server), "the open project is unsaved");
        let dev = call(project_open, &mut server, json!({"path": "/dev/zero"}));
        assert!(dev.is_error);
        assert!(dev.text.contains("is not a regular file: /dev/zero"), "{}", dev.text);
        let text = call(project_open, &mut server, json!({"path": notes}));
        assert!(text.is_error);
        assert!(text.text.contains("is not an edith project or a media file"), "{}", text.text);
        assert!(!text.text.contains("unsaved"), "{}", text.text);
    }

    #[test]
    fn limiter_ceiling_outside_the_engine_range_is_refused_by_name() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        let low = call(project_settings, &mut server, json!({"limiter": {"on": true, "ceiling_db": -30.0}}));
        assert!(low.text.contains("from -24 to 0 dBTP"), "{}", low.text);
        assert!(call(project_status, &mut server, json!({})).text.contains("limiter off"));
        let ok = call(project_settings, &mut server, json!({"limiter": {"on": true, "ceiling_db": -1.5}}));
        assert!(!ok.is_error, "{}", ok.text);
        assert!(call(project_status, &mut server, json!({})).text.contains("limiter on at -1.5 dBTP true peak"));
    }

    #[test]
    fn save_names_an_unwritable_folder() {
        quiet();
        let mut server = Server::new();
        server.session = Some(PlaybackSession::empty(1280, 720, 30.0).unwrap());
        import_av(&mut server);
        let r = call(project_save, &mut server, json!({"path": "/proc/forbidden.edith"}));
        assert!(r.is_error);
        assert!(r.text.contains("save failed: could not write /proc/forbidden.edith"), "{}", r.text);
    }
}

#[cfg(test)]
mod autosave_tests {
    use super::*;
    use std::fs::File;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// A project that was never saved leaves only its autosave: `project_open` of its
    /// path names that autosave, and opening the autosave recovers the work.
    #[test]
    fn open_of_a_never_saved_project_names_its_autosave() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-never-saved");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        let r = project_new(&mut server, &json!({"path": path, "resolution": "1280x720"}));
        assert!(!r.is_error, "{}", r.text);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        assert!(!path.exists(), "the project was never saved");

        let mut fresh = Server::new();
        let missing = project_open(&mut fresh, &json!({"path": path}));
        assert!(missing.is_error, "{}", missing.text);
        assert!(missing.text.starts_with("not found:"), "{}", missing.text);
        assert!(missing.text.contains("never saved"), "{}", missing.text);
        assert!(missing.text.contains(&auto.display().to_string()), "{}", missing.text);
        assert!(missing.text.contains("\"recover\": true"), "{}", missing.text);

        let recovered = project_open(&mut fresh, &json!({"path": path, "recover": true}));
        assert!(!recovered.is_error, "{}", recovered.text);
        assert!(recovered.text.contains("src:0 test_av.mp4") || recovered.text.contains("sources: 1"), "{}", recovered.text);
        assert!(recovered.text.contains("recovered the unsaved edits"), "{}", recovered.text);
        // A never-saved project has no saved timeline under the recovered state: undo says so.
        assert!(recovered.text.contains("undo has nothing earlier to return to: the project was never saved"), "{}", recovered.text);
        assert!(recovered.text.contains("project_open {discard_autosave:true}"), "{}", recovered.text);
        assert!(!recovered.text.contains("use checkpoint/restore"), "{}", recovered.text);
        assert!(dirty(&fresh), "a recovered project is unsaved");
        // Saving the recovered work writes the project file and retires the autosave.
        let saved = project_save(&mut fresh, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(path.is_file(), "the recovered project is now a file");
        assert!(!auto.exists(), "the save replaced the autosave");
    }

    /// `recover` with no autosave to load is refused by name, and writes nothing.
    #[test]
    fn recover_without_an_autosave_is_refused() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-no-recover");
        let path = td.path().join("none.edith");
        let mut server = Server::new();
        let r = project_open(&mut server, &json!({"path": path, "recover": true}));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("nothing to recover"), "{}", r.text);
        assert!(server.session.is_none());
    }

    /// The timeline as `(start, in, out)` per clip on every lane: what a recovery's undo must give back.
    fn timeline(s: &PlaybackSession) -> Vec<Vec<(u32, u32, u32)>> {
        s.lanes()
            .into_iter()
            .map(|l| s.lane_clips(l).iter().map(|c| (c.start, c.in_frame, c.out_frame)).collect())
            .collect()
    }

    fn tool(server: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
        (t.run)(server, &args)
    }

    /// A recovery on a saved project is one undo step: undo gives the saved file's timeline back,
    /// redo the recovered one, and the ledger holds the entry the reply names.
    #[test]
    fn recover_is_one_undo_step_that_undo_and_redo_walk() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-undo-recover");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let placed = tool(&mut server, "edit", json!({"ops": [{"op": "place", "source": "src:0", "lane": "V1", "at": "0s"}]}));
        assert!(!placed.is_error, "{}", placed.text);
        let cut = tool(&mut server, "edit", json!({"ops": [{"op": "split", "at": "1s"}]}));
        assert!(!cut.is_error, "{}", cut.text);
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        let saved = timeline(server.session.as_ref().unwrap());

        // Unsaved edits after the save, then the client walks away.
        let more = tool(&mut server, "edit", json!({"ops": [{"op": "split", "at": "2s"}]}));
        assert!(!more.is_error, "{}", more.text);
        let recovered = timeline(server.session.as_ref().unwrap());
        assert_ne!(recovered, saved, "the edit changed the timeline");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        drop(server);

        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path, "recover": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert_eq!(timeline(fresh.session.as_ref().unwrap()), recovered, "the recovered timeline");
        assert_eq!(fresh.ledger.len(), 1, "one entry for the recovery");
        let label = fresh.ledger[0].label.clone();
        assert!(label.starts_with("recover autosave (") && label.contains(" edits from "), "{label}");
        assert!(dirty(&fresh), "a recovered project is unsaved");

        let undone = tool(&mut fresh, "undo", json!({}));
        assert!(!undone.is_error, "{}", undone.text);
        assert_eq!(timeline(fresh.session.as_ref().unwrap()), saved, "undo gives the saved file's timeline");
        let redone = tool(&mut fresh, "redo", json!({}));
        assert!(!redone.is_error, "{}", redone.text);
        assert_eq!(timeline(fresh.session.as_ref().unwrap()), recovered, "redo gives the recovered timeline");
        assert!(auto.is_file(), "the autosave stays until a save retires it");
    }

    /// A save of a project with no sources says so, rather than blaming the folder; once a
    /// source is imported the same save works.
    #[test]
    fn save_of_a_project_with_no_sources_names_the_missing_source() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("save-empty");
        let path = td.path().join("empty.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        let early = project_save(&mut server, &json!({}));
        assert!(early.is_error, "{}", early.text);
        assert!(early.text.contains("nothing to save yet"), "{}", early.text);
        assert!(!early.text.contains("folder"), "{}", early.text);
        assert!(!path.exists());

        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let saved = project_save(&mut server, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(path.is_file());
    }

    /// Unsaved work at EOF goes beside the project, the project file is left as it was,
    /// `project_open` says a newer autosave exists, and `project_save` clears it.
    #[test]
    fn unsaved_work_is_autosaved_beside_the_file_and_offered_on_open() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-eof");
        let d = td.path().to_path_buf();
        let path = d.join("proj.edith");
        let mut server = Server::new();
        let r = project_new(&mut server, &json!({"path": path, "resolution": "1280x720"}));
        assert!(!r.is_error, "{}", r.text);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        let before = std::fs::read(&path).expect("read the saved project");

        // A change after the save, then the client walks away.
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        assert!(dirty(&server));
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        assert_eq!(auto, autosave_path(&path));
        assert!(auto.is_file(), "the autosave exists");
        assert_eq!(std::fs::read(&path).expect("reread"), before, "the real file is never overwritten");

        // The open is refused before anything loads, and names both ways on.
        let mut fresh = Server::new();
        let refused = project_open(&mut fresh, &json!({"path": path}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.starts_with(&format!("{} has an autosave from ", path.display())), "{}", refused.text);
        assert!(refused.text.contains("recover:true") && refused.text.contains("discard_autosave:true"), "{}", refused.text);
        assert!(refused.text.ends_with("Nothing was loaded."), "{}", refused.text);
        assert!(fresh.session.is_none(), "nothing is loaded by a refused open");
        assert!(auto.is_file(), "nothing is set aside by a refused open");

        // recover:true continues from the autosave; the save then retires it.
        let opened = project_open(&mut fresh, &json!({"path": path, "recover": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("recovered the unsaved edits from "), "{}", opened.text);
        assert!(opened.text.contains("undo takes the recovered edits back as one step"), "{}", opened.text);

        // A save writes the session's state and retires the autosave, and says so once.
        let saved = project_save(&mut fresh, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("removed the autosave"), "{}", saved.text);
        assert!(!saved.text.contains("kept"), "{}", saved.text);
        assert!(!auto.exists(), "the save replaced the autosave");
    }

    /// A project saved once, then given more work and left to autosave: returns the test dir,
    /// the project path and its autosave path.
    fn unrecovered_project(name: &str) -> (crate::testutil::TestDir, PathBuf, PathBuf) {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new(name);
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        (td, path, auto)
    }

    /// An open over an unloaded autosave is refused, and the refusal loads and writes nothing.
    /// `discard_autosave` opens the saved file and sets the autosave aside; the save then succeeds.
    #[test]
    fn unloaded_autosave_refuses_the_open_and_discard_then_saves() {
        let (td, path, auto) = unrecovered_project("autosave-refuse-open");
        let mut fresh = Server::new();
        let refused = project_open(&mut fresh, &json!({"path": path}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("(an earlier session's unsaved edits: +1 source(s)"), "{}", refused.text);
        assert!(fresh.session.is_none(), "nothing is loaded");
        assert!(auto.is_file(), "nothing is written or moved");

        // discard_autosave opens the saved file (one source) and sets the autosave aside.
        let opened = project_open(&mut fresh, &json!({"path": path, "discard_autosave": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.starts_with("set the autosave aside as "), "{}", opened.text);
        assert!(opened.text.contains("(its unsaved edits are not loaded; the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted))"), "{}", opened.text);
        assert!(!opened.text.contains('⚠'), "{}", opened.text);
        assert_eq!(fresh.session.as_ref().unwrap().sources().len(), 1, "the autosave is not loaded");
        assert!(opened.structured.as_ref().unwrap()["unrecovered_autosave"].is_null());
        assert!(!auto.exists(), "the autosave moved aside");
        let kept: Vec<_> = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.starts_with("proj.autosave-"))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");

        // The save after a discard open succeeds: nothing unloaded is left beside the project.
        let saved = project_save(&mut fresh, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.starts_with("saved "), "{}", saved.text);
        assert!(!saved.text.contains("kept"), "{}", saved.text);
        let status = project_status(&mut fresh, &json!({}));
        assert!(status.structured.as_ref().unwrap()["unrecovered_autosave"].is_null());
    }

    /// `discard_autosave` sets the unloaded edits aside as a dated copy (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted), and
    /// the reply says so first with no warning.
    #[test]
    fn discard_autosave_keeps_a_dated_copy_of_the_unloaded_edits() {
        let (_td, path, auto) = unrecovered_project("autosave-discard");
        let bytes = std::fs::read(&auto).expect("the autosave to set aside");
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path, "discard_autosave": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.starts_with("set the autosave aside as "), "{}", opened.text);
        assert!(opened.text.contains("the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted)"), "{}", opened.text);
        assert!(!opened.text.contains('⚠'), "{}", opened.text);
        assert!(!auto.exists(), "the autosave itself is moved");
        let dated = dated_autosaves(&path);
        assert_eq!(dated.len(), 1, "one dated copy: {:?}", dated);
        assert!(opened.text.contains(&dated[0].1.display().to_string()), "the reply names the copy: {}", opened.text);
        assert_eq!(std::fs::read(&dated[0].1).expect("read the copy"), bytes, "the copy holds the same edits");
        assert!(opened.structured.as_ref().unwrap()["unrecovered_autosave"].is_null());
        // Nothing was edited since: the session writes no autosave at exit.
        assert!(autosave_at_eof(&fresh).is_none());
        assert!(!auto.exists());
    }

    /// Over an unloaded autosave, the choice still needs the unsaved-edits guard to pass: with
    /// another project dirty, `discard_autosave` alone is refused and nothing is set aside; with
    /// `discard` as well, both happen and the reply names each.
    #[test]
    fn discard_autosave_still_respects_the_unsaved_guard_of_another_project() {
        let (td, path, auto) = unrecovered_project("autosave-other-dirty");
        let mut server = Server::new();
        let other = td.path().join("other.edith");
        assert!(!project_new(&mut server, &json!({"path": other, "resolution": "1280x720"})).is_error);
        let refused = project_open(&mut server, &json!({"path": path, "discard_autosave": true}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("has unsaved changes"), "{}", refused.text);
        assert!(auto.is_file(), "a refused open sets nothing aside");
        assert_eq!(server.path.as_deref(), Some(other.as_path()), "the other project is still open");

        let opened = project_open(&mut server, &json!({"path": path, "discard_autosave": true, "discard": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("discarded unsaved edits in "), "{}", opened.text);
        assert!(opened.text.contains("set the autosave aside as "), "{}", opened.text);
        assert!(!auto.exists());
    }

    /// A media file started as a project: its project is <stem>.edith beside it. Over that
    /// project's unloaded autosave the open is refused and names the .edith, and recover installs
    /// the .edith as the project, so a save writes the project and never the media file.
    #[test]
    fn media_start_recovers_its_own_project_and_never_the_media_file() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-media");
        let clip = td.path().join("clip.mp4");
        std::fs::copy(asset("test_av.mp4"), &clip).expect("copy the clip");
        let media_bytes = std::fs::read(&clip).expect("read the clip");
        let project = td.path().join("clip.edith");
        let mut first = Server::new();
        assert!(!project_new(&mut first, &json!({"path": project, "resolution": "1280x720"})).is_error);
        first.session.as_mut().unwrap().import(&clip).expect("import");
        let auto = autosave_at_eof(&first).expect("a dirty project autosaves");
        assert_eq!(auto, td.path().join("clip.autosave.edith"));

        let mut fresh = Server::new();
        let refused = project_open(&mut fresh, &json!({"path": clip}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.starts_with(&format!("{} has an autosave from ", project.display())), "{}", refused.text);
        let rec = project_open(&mut fresh, &json!({"path": clip, "recover": true}));
        assert!(!rec.is_error, "{}", rec.text);
        assert_eq!(fresh.path.as_deref(), Some(project.as_path()), "the project file is the one recovered");
        let saved = project_save(&mut fresh, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert_eq!(std::fs::read(&clip).expect("reread the clip"), media_bytes, "the media file is never written");
    }

    /// A session that opens a project and makes no edit writes no autosave at exit, whatever
    /// it found beside the project: an unrecovered autosave, a recovered one, a file that needed
    /// a load fix. An edit made in such a session is still autosaved.
    #[test]
    fn a_session_that_made_no_edit_writes_no_autosave_at_exit() {
        let (td, path, auto) = unrecovered_project("autosave-no-edit");
        let before = std::fs::read(&auto).expect("the unrecovered autosave");
        let listing = |d: &Path| {
            let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        let files_before = listing(td.path());

        // Refused over the unloaded autosave: nothing opens, so nothing is written at exit.
        let mut fresh = Server::new();
        assert!(project_open(&mut fresh, &json!({"path": path})).is_error);
        assert!(autosave_at_eof(&fresh).is_none(), "a refused open leaves the autosave alone at exit");
        assert_eq!(std::fs::read(&auto).expect("still there"), before);
        assert_eq!(listing(td.path()), files_before, "no file was written, moved or copied");

        let mut rec = Server::new();
        assert!(!project_open(&mut rec, &json!({"path": path, "recover": true})).is_error);
        assert!(autosave_at_eof(&rec).is_none(), "a recovered session with no edit writes nothing");
        assert_eq!(listing(td.path()), files_before);

        // An edit in the recovered session is still autosaved, in place of the recovered one.
        rec.session.as_mut().unwrap().import(&asset("test_tone.wav")).expect("import");
        assert!(autosave_at_eof(&rec).is_some(), "an edit is autosaved at exit");
    }

    /// A file that needed a fix to open (a clip past its source, clamped) is not an edit: the
    /// session that only opens it writes no autosave, and the reply still says what was fixed.
    #[test]
    fn a_load_fix_alone_is_not_an_edit() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-load-fix");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        let s = server.session.as_mut().unwrap();
        s.import(&asset("test_av.mp4")).expect("import");
        s.place_stream_at(0.0, &asset("test_av.mp4"), 0, Some(Lane::V1)).expect("place the picture");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        // Make a clip run past its source, as a file a GUI or an older engine wrote can.
        let text = std::fs::read_to_string(&path).expect("read the project");
        let mut out = String::new();
        let mut changed = false;
        for line in text.lines() {
            if !changed && line.starts_with("video ") {
                let mut t: Vec<&str> = line.split(' ').collect();
                if t.len() > 4 {
                    t[4] = "9999";
                    changed = true;
                }
                out.push_str(&t.join(" "));
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        assert!(changed, "the file has a video clip to stretch");
        std::fs::write(&path, out).expect("write the stretched project");
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("needed fixing to open"), "{}", opened.text);
        assert!(dirty(&fresh), "the fix is unsaved");
        assert!(autosave_at_eof(&fresh).is_none(), "a fix alone writes no autosave");
        assert!(!autosave_path(&path).exists());
        let _ = td;
    }

    /// Recover, then save: the save removes the autosave, as the recover reply promised.
    #[test]
    fn recover_then_save_removes_the_autosave() {
        let (_td, path, auto) = unrecovered_project("autosave-recover-save");
        let mut fresh = Server::new();
        let first = project_open(&mut fresh, &json!({"path": path}));
        assert!(first.is_error && first.text.contains("has an autosave from"), "{}", first.text);
        let rec = project_open(&mut fresh, &json!({"path": path, "recover": true}));
        assert!(!rec.is_error, "{}", rec.text);
        assert!(!rec.text.contains('⚠'), "{}", rec.text);
        assert!(rec.text.contains("removes the autosave"), "{}", rec.text);
        assert!(rec.structured.as_ref().unwrap()["unrecovered_autosave"].is_null());
        let saved = project_save(&mut fresh, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.starts_with("saved "), "{}", saved.text);
        assert!(saved.text.contains("removed the autosave"), "{}", saved.text);
        assert!(!saved.text.contains("kept"), "{}", saved.text);
        assert!(!auto.exists());
    }

    /// A discard names the unsaved edits it drops and the autosave they were written to.
    #[test]
    fn discarding_unsaved_edits_names_where_they_are() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("discard-note");
        let path = td.path().join("proj.edith");
        let other = td.path().join("other.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let r = project_new(&mut server, &json!({"path": other, "resolution": "1280x720", "discard": true}));
        assert!(!r.is_error, "{}", r.text);
        let auto = autosave_path(&path);
        let want = format!("discarded unsaved edits in {} (still in its autosave {})", path.display(), auto.display());
        assert!(r.text.starts_with(&want), "{}", r.text);
        assert!(auto.is_file(), "the discarded edits are in the autosave");
    }

    /// An autosave no newer than the file the session last saved is stale, and the save removes it.
    #[test]
    fn save_removes_an_autosave_no_newer_than_the_file() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-stale");
        let path = td.path().join("stale.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        File::options()
            .write(true)
            .open(&auto)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();
        let saved = project_save(&mut server, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("removed the autosave"), "{}", saved.text);
        assert!(!auto.exists());
    }

    /// A project opened over a newer autosave the session did not recover: a save keeps the
    /// autosave under a dated name, byte for byte, and never deletes it.
    #[test]
    fn save_after_an_unrecovered_open_keeps_the_autosave_dated() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-keep");
        let path = td.path().join("keep.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        let auto_bytes = std::fs::read(&auto).expect("read the autosave");

        // The open happens with no autosave beside the project; another session's autosave then
        // appears while this one runs.
        std::fs::remove_file(&auto).expect("take the autosave away before the open");
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path}));
        assert!(!opened.is_error, "{}", opened.text);
        std::fs::write(&auto, &auto_bytes).expect("the autosave appears after the open");

        let refused = project_save(&mut fresh, &json!({}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("not loaded by this session"), "{}", refused.text);
        assert!(auto.is_file(), "a refused save moves nothing");

        let saved = project_save(&mut fresh, &json!({"discard_autosave": true}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("kept the unrecovered autosave as "), "{}", saved.text);
        assert!(!saved.text.contains("removed"), "{}", saved.text);
        assert!(!auto.exists(), "the autosave moved out of the way");

        let kept: Vec<PathBuf> = std::fs::read_dir(td.path())
            .expect("list the folder")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with("keep.autosave-") && n.ends_with(".edith")
            }))
            .collect();
        assert_eq!(kept.len(), 1, "exactly one dated autosave: {kept:?}");
        let name = kept[0].file_name().unwrap().to_string_lossy().into_owned();
        let stamp = &name["keep.autosave-".len()..name.len() - ".edith".len()];
        assert_eq!(stamp.len(), 15, "YYYYmmdd-HHMMSS: {stamp}");
        assert_eq!(stamp.as_bytes()[8], b'-', "{stamp}");
        assert!(saved.text.contains(&kept[0].display().to_string()), "{}", saved.text);
        assert_eq!(std::fs::read(&kept[0]).expect("read the kept autosave"), auto_bytes, "kept byte for byte");
    }

    /// The stamp is the UTC calendar time, with no clock crate: checked at known instants.
    #[test]
    fn local_stamp_and_text_are_the_same_local_time() {
        // The stamp in a dated autosave name and the reply's time are both local: they agree.
        for secs in [0u64, 1_700_000_000, 951_782_400] {
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
            let (stamp, text) = (local_stamp(t), local_text(t));
            assert_eq!(stamp.len(), 15, "{stamp}");
            assert_eq!(text.len(), 19, "{text}");
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            assert_eq!(stamp.replace('-', ""), digits, "{stamp} vs {text}");
        }
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let tm = local_time(t).expect("local time");
        assert_eq!(chrono::Datelike::year(&tm), 2023);
        assert_eq!(local_stamp(t).get(..8), Some(tm.format("%Y%m%d").to_string().as_str()));
    }

    /// A clean project writes nothing at EOF.
    #[test]
    fn a_clean_project_is_not_autosaved() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-clean");
        let d = td.path().to_path_buf();
        let path = d.join("clean.edith");
        let mut server = Server::new();
        project_new(&mut server, &json!({"path": path, "resolution": "1280x720"}));
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        project_save(&mut server, &json!({}));
        assert!(autosave_at_eof(&server).is_none());
        assert!(!autosave_path(&path).exists());
    }

    /// Sources imported but not placed on the timeline are in the autosave, so a recovered
    /// session still has every `src:<n>` the last one had.
    #[test]
    fn recovered_autosave_keeps_every_imported_source() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-sources");
        let path = td.path().join("keep.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        let s = server.session.as_mut().unwrap();
        s.import(&asset("test_av.mp4")).expect("import");
        s.import(&asset("test_tone.wav")).expect("import");
        s.import(&asset("test_hevc.mkv")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");

        let mut fresh = Server::new();
        let r = project_open(&mut fresh, &json!({"path": path, "recover": true}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(fresh.session.as_ref().unwrap().sources().len(), 3, "every import is recovered");
        assert!(auto.is_file());
    }

    /// A read-only run on a saved project, and a run that recovers an autosave and changes
    /// nothing, write no autosave and print nothing.
    #[test]
    fn read_only_runs_write_no_autosave() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-read-only");
        let path = td.path().join("ro.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));

        let mut clean = Server::new();
        assert!(!project_open(&mut clean, &json!({"path": path})).is_error);
        assert!(autosave_at_eof(&clean).is_none(), "a saved project that is unchanged writes nothing");
        assert!(!autosave_path(&path).exists());

        // An unsaved change makes one autosave; a later run that only reads it writes nothing.
        clean.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        assert!(autosave_at_eof(&clean).is_some());
        let mut recovered = Server::new();
        assert!(!project_open(&mut recovered, &json!({"path": path, "recover": true})).is_error);
        assert!(autosave_at_eof(&recovered).is_none(), "the recovered work is already in the autosave");
    }

    /// An autosave that matches the file, or is older than it, is stale: `project_open`
    /// removes it without offering recovery. Only a newer one that differs is offered.
    #[test]
    fn open_removes_a_stale_autosave_silently() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-open-stale");
        let path = td.path().join("stale2.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        let auto = autosave_path(&path);

        // Identical content, newer than the file: stale.
        std::fs::copy(&path, &auto).expect("copy");
        std::fs::File::options().write(true).open(&auto).expect("open").set_modified(SystemTime::now() + Duration::from_secs(60)).expect("mtime");
        let mut a = Server::new();
        let r = project_open(&mut a, &json!({"path": path}));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("unsaved edits"), "{}", r.text);
        assert!(!auto.exists(), "the identical autosave is removed");

        // Different content, but older than the file: stale too.
        let mut b = Server::new();
        assert!(!project_open(&mut b, &json!({"path": path, "discard": true})).is_error);
        b.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        assert!(autosave_at_eof(&b).is_some());
        std::fs::File::options().write(true).open(&path).expect("open").set_modified(SystemTime::now() + Duration::from_secs(120)).expect("mtime");
        let mut c = Server::new();
        let r = project_open(&mut c, &json!({"path": path}));
        assert!(!r.text.contains("unsaved edits"), "{}", r.text);
        assert!(!auto.exists(), "an older autosave is removed");
    }

    /// Parallel checks of one session's body all read back the whole project: none of them
    /// reads a scratch file another thread wrote or removed (the flake this guards against).
    #[test]
    fn session_body_is_stable_under_parallel_calls() {
        engine::ao::set_silent_output();
        let base_session = PlaybackSession::open(asset("test_av.mp4")).expect("open test asset");
        let base = session_body(&base_session).expect("a body");
        let path = asset("test_av.mp4");
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let path = path.clone();
                    s.spawn(move || session_body(&PlaybackSession::open(path).expect("open")))
                })
                .collect();
            for h in handles {
                assert_eq!(h.join().expect("thread").as_deref(), Some(base.as_slice()));
            }
        });
    }

    fn call(server: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
        (t.run)(server, &args)
    }

    /// A project opened and left unchanged writes no autosave at exit, and a setting edit
    /// that is undone again leaves the session saying what the file says.
    #[test]
    fn unchanged_project_writes_no_autosave_at_exit() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-unchanged");
        let path = td.path().join("unchanged.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));

        let mut opened = Server::new();
        assert!(!project_open(&mut opened, &json!({"path": path})).is_error);
        assert!(autosave_at_eof(&opened).is_none(), "an unchanged project is not autosaved");

        let r = call(&mut opened, "edit", json!({"ops": [{"op": "lane_gain", "lane": "A1", "db": -6}]}));
        assert!(!r.is_error, "{}", r.text);
        assert!(autosave_at_eof(&opened).is_some(), "a setting edit is autosaved");
        let r = call(&mut opened, "undo", json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(autosave_at_eof(&opened).is_none(), "undone back to the file: nothing to autosave");
    }

    /// The open warning reads settings from the two files: a gain, and the colour of the
    /// clip that points at a changed colour entry. Identical texts differ in nothing.
    #[test]
    fn settings_differences_name_gains_and_clip_colours() {
        let file = "edith 29\nplayhead 0\nresolution 1280 720\nfps 25\nsource 0 a.mp4\nvideo 1 0 0 50 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 50 0 - - - fit 1000 0 0 0 - 0 0\n";
        let auto = "edith 29\nplayhead 40\nresolution 1280 720\nfps 25\nsource 0 a.mp4\nvideo 1 0 0 50 0 - - 0 fit 1000 0 0 0 - 0 0\naudio 1 0 0 50 0 - - - fit 1000 0 0 0 - 0 0\ncolor 0.1:1.2:0.9:-0.3\ngain audio 1 -6.0\n";
        assert!(settings_differences(file, file).is_empty());
        let got = settings_differences(file, auto);
        assert!(got.contains(&"color on V1@0".to_string()), "{got:?}");
        assert!(got.contains(&"lane A1 gain -6 dB".to_string()), "{got:?}");
        assert!(!got.iter().any(|g| g.starts_with("clip")), "{got:?}");
    }
    /// Gives the clip on A1 at 0 a gain of `db`, as one edit step.
    fn set_gain(server: &mut Server, db: f64) {
        let r = crate::tools::edit::run_edit(server, &json!({"ops": [{"op": "gain", "clip": "A1@0", "db": db}]}));
        assert!(!r.is_error, "{}", r.text);
    }

    /// The gain (dB) the autosave at `path` holds for its A1 clip.
    fn autosave_gain(path: &Path) -> String {
        let text = std::fs::read_to_string(path).expect("read the autosave");
        text.lines().find(|l| l.starts_with("clipgain audio")).unwrap_or("no clipgain").to_string()
    }

    /// The two-session sequence from the dogfood: run 1 leaves the -7 edit in the autosave;
    /// run 2 opens without recovering it, edits, and exits. The -7 autosave must survive under a
    /// dated name, and run 2's own autosave holds run 2's edit.
    #[test]
    fn unrecovered_autosave_survives_a_second_session_that_edits() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-two-sessions");
        let path = td.path().join("au.edith");
        let mut first = Server::new();
        assert!(!project_new(&mut first, &json!({"path": path, "resolution": "1280x720"})).is_error);
        first.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let placed = crate::tools::edit::run_edit(&mut first, &json!({"ops": [{"op": "place", "source": "src:0", "at": 0}]}));
        assert!(!placed.is_error, "{}", placed.text);
        assert!(project_save(&mut first, &json!({})).text.starts_with("saved"));
        // Run 1: gain -7, exit.
        let mut run1 = Server::new();
        assert!(!project_open(&mut run1, &json!({"path": path})).is_error);
        set_gain(&mut run1, -7.0);
        let auto = autosave_at_eof(&run1).expect("run 1 autosaves its -7");
        assert_eq!(autosave_gain(&auto), "clipgain audio 1 0 -7");

        // Run 2: a plain open is refused over run 1's autosave, so it opens with discard_autosave
        // (run 1's edits are set aside), edits, exits.
        let mut run2 = Server::new();
        assert!(project_open(&mut run2, &json!({"path": path})).is_error);
        let opened = project_open(&mut run2, &json!({"path": path, "discard_autosave": true}));
        assert!(opened.text.contains("set the autosave aside as"), "{}", opened.text);
        set_gain(&mut run2, -2.0);
        assert_eq!(autosave_at_eof(&run2).as_deref(), Some(auto.as_path()));

        // The -7 edits are kept, byte for byte, under a dated name; run 2's edit is the autosave.
        let kept: Vec<PathBuf> = std::fs::read_dir(td.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("au.autosave-"))
            .collect();
        assert_eq!(kept.len(), 1, "exactly one dated copy: {kept:?}");
        assert_eq!(autosave_gain(&kept[0]), "clipgain audio 1 0 -7", "the -7 edit survives");
        assert_eq!(autosave_gain(&auto), "clipgain audio 1 0 -2");

        // Run 3 recovers run 2's autosave and exits with no change: nothing is moved or added.
        let mut run3 = Server::new();
        assert!(!project_open(&mut run3, &json!({"path": path, "recover": true})).is_error);
        assert_eq!(autosave_at_eof(&run3), None, "a recovered session that changes nothing writes nothing");
        assert_eq!(autosave_gain(&auto), "clipgain audio 1 0 -2");
        let dated = std::fs::read_dir(td.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("au.autosave-")).count();
        assert_eq!(dated, 1);
    }

    /// A recovered autosave that the session edits is replaced in place, with nothing moved aside.
    #[test]
    fn recovered_autosave_is_replaced_in_place() {
        engine::ao::set_silent_output();
        let (td, path, auto) = unrecovered_project("autosave-recover-in-place");
        let mut run = Server::new();
        assert!(!project_open(&mut run, &json!({"path": path, "recover": true})).is_error);
        run.session.as_mut().unwrap().import(&asset("test_ac3.mp4")).expect("a different edit");
        assert_eq!(autosave_at_eof(&run).as_deref(), Some(auto.as_path()));
        let dated = std::fs::read_dir(td.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("proj.autosave-")).count();
        assert_eq!(dated, 0, "no dated copy when the session recovered the autosave");
    }

    /// A save-as to another file, then a save back to the original, needs no force: both are this
    /// project's files. A third file that exists and was never this project's is still refused.
    #[test]
    fn save_back_to_the_original_after_save_as_needs_no_force() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("save-back-original");
        let orig = td.path().join("orig.edith");
        let copy = td.path().join("copy.undo.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": orig, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        let as_copy = project_save(&mut server, &json!({"path": copy}));
        assert!(as_copy.text.starts_with("saved"), "{}", as_copy.text);
        let back = project_save(&mut server, &json!({"path": orig}));
        assert!(back.text.starts_with("saved"), "{}", back.text);
        // Opened in this session, so saving it needs no force either.
        let mut other = Server::new();
        assert!(!project_open(&mut other, &json!({"path": orig})).is_error);
        let again = project_save(&mut other, &json!({}));
        assert!(again.text.starts_with("saved"), "{}", again.text);
        // The copy exists and this session never opened or saved it: refused.
        let foreign = project_save(&mut other, &json!({"path": copy}));
        assert!(foreign.is_error && foreign.text.contains("not this project's file"), "{}", foreign.text);
        // A file this session never touched is still refused.
        let stranger = td.path().join("stranger.edith");
        std::fs::write(&stranger, b"not ours").unwrap();
        let refused = project_save(&mut server, &json!({"path": stranger}));
        assert!(refused.is_error && refused.text.contains("not this project's file"), "{}", refused.text);
    }


    /// An autosave that cannot be written is reported by name, not silently dropped: the write
    /// returns the reason, and a discard that would have kept the edits says they are gone.
    #[test]
    fn a_failed_autosave_names_its_reason() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("autosave-fails");
        // A file where the folder of the project should be: no autosave can be written beside it.
        std::fs::write(td.path().join("blocker"), b"x").unwrap();
        let path = td.path().join("blocker").join("p.edith");
        let mut server = Server::new();
        server.session = Some(engine::PlaybackSession::empty(1280, 720, 30.0).expect("empty session"));
        server.path = Some(path.clone());
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let why = write_autosave(&server).expect_err("the write must fail");
        assert!(why.contains("cannot create the folder"), "{why}");
        assert!(autosave_at_eof(&server).is_none());
        let note = discarded_note(&server);
        assert!(note.contains("autosave FAILED") && note.contains("gone"), "{note}");
    }
}

#[cfg(test)]
mod relink_tests {
    use super::*;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// A saved project whose one source is a copy of a fixture, then the copy moved away.
    /// Returns the folder, the project path, the source's saved path and where it went.
    fn moved_source(name: &str) -> (crate::testutil::TestDir, PathBuf, PathBuf, PathBuf) {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new(name);
        let d = td.path().to_path_buf();
        let media = d.join("film.mp4");
        std::fs::copy(asset("test_av.mp4"), &media).expect("copy the fixture");
        let path = d.join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&media).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        let moved_dir = d.join("moved");
        std::fs::create_dir(&moved_dir).expect("make the new folder");
        let moved = moved_dir.join("film.mp4");
        std::fs::rename(&media, &moved).expect("move the media");
        (td, path, media, moved_dir)
    }

    #[test]
    fn a_moved_source_opens_offline_and_names_its_fix() {
        let (_td, path, _media, _moved_dir) = moved_source("relink-offline");
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("1 source(s) offline"), "{}", opened.text);
        assert!(opened.text.contains("src:0"), "{}", opened.text);
        assert!(opened.text.contains("media_relink") && opened.text.contains("\"search\""), "{}", opened.text);
        assert_eq!(fresh.session.as_ref().unwrap().offline_sources().len(), 1);
    }

    #[test]
    fn search_relinks_a_moved_source_by_file_name() {
        let (_td, path, _media, moved_dir) = moved_source("relink-search");
        let mut fresh = Server::new();
        let found = project_open(
            &mut fresh,
            &json!({"path": path, "search": [moved_dir.display().to_string()]}),
        );
        assert!(!found.is_error, "{}", found.text);
        assert!(found.text.contains("src:0 relinked:"), "{}", found.text);
        assert!(!found.text.contains("offline"), "{}", found.text);
        assert!(fresh.session.as_ref().unwrap().offline_sources().is_empty());
    }

    #[test]
    fn two_files_with_one_name_are_listed_not_guessed() {
        let (td, path, _media, moved_dir) = moved_source("relink-ambiguous");
        let other = td.path().join("elsewhere");
        std::fs::create_dir(&other).expect("make the other folder");
        std::fs::copy(asset("test_av.mp4"), other.join("film.mp4")).expect("a second film.mp4");
        let mut fresh = Server::new();
        let found = project_open(
            &mut fresh,
            &json!({"path": path, "search": [moved_dir.display().to_string(), other.display().to_string()]}),
        );
        assert!(found.text.contains("2 files named film.mp4"), "{}", found.text);
        assert!(found.text.contains("not relinked"), "{}", found.text);
        assert_eq!(fresh.session.as_ref().unwrap().offline_sources().len(), 1);
    }

    #[test]
    fn media_relink_points_an_offline_source_at_the_file_and_makes_it_dirty() {
        let (_td, path, _media, moved_dir) = moved_source("relink-tool");
        let mut fresh = Server::new();
        assert!(!project_open(&mut fresh, &json!({"path": path})).is_error);
        let moved = moved_dir.join("film.mp4");
        let bad = crate::tools::media::relink(&mut fresh, &json!({"source": "src:9", "path": moved.display().to_string()}));
        assert!(bad.is_error && bad.text.contains("no src:9"), "{}", bad.text);
        let done = crate::tools::media::relink(&mut fresh, &json!({"source": "src:0", "path": moved.display().to_string()}));
        assert!(!done.is_error, "{}", done.text);
        assert!(done.text.contains("src:0 relinked"), "{}", done.text);
        assert!(fresh.session.as_ref().unwrap().offline_sources().is_empty());
        assert!(dirty(&fresh), "a relink is an unsaved change");
    }

    /// A dirty session that was never saved leaves only the autosave beside a project already on
    /// disk: an open that does not recover it says so, and the status does not claim "saved".
    #[test]
    fn an_unrecovered_autosave_is_not_reported_as_saved() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("relink-status");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        autosave_at_eof(&server).expect("a dirty project autosaves");
        // Refused: the autosave is not loaded, so the open reply can not call the project saved.
        let refused = project_open(&mut Server::new(), &json!({"path": path}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(!refused.text.contains("· saved ·"), "{}", refused.text);
    }

    /// Each run that closes dirty used to add a dated copy of the same unrecovered autosave. Now
    /// one collision keeps one dated copy and the session's own autosave takes its place; a run
    /// that closes clean writes nothing.
    #[test]
    fn repeated_runs_do_not_pile_up_dated_autosaves() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("relink-clutter");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!project_new(&mut server, &json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(project_save(&mut server, &json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");

        let dated = || {
            std::fs::read_dir(td.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("proj.autosave-"))
                .count()
        };
        // A run that edits without recovering: the older autosave is kept dated, once.
        let mut run = Server::new();
        assert!(!project_open(&mut run, &json!({"path": path, "discard_autosave": true})).is_error);
        run.session.as_mut().unwrap().import(&asset("test_ac3.mp4")).expect("a different edit");
        assert_eq!(autosave_at_eof(&run).as_deref(), Some(auto.as_path()));
        assert_eq!(dated(), 1, "the unrecovered autosave is kept under a dated name");

        // A run that only looks: the unsaved edits of the run before are refused, not loaded, and
        // the run closes with nothing written.
        let mut clean = Server::new();
        assert!(project_open(&mut clean, &json!({"path": path})).is_error);
        assert!(autosave_at_eof(&clean).is_none());
        assert_eq!(dated(), 1);
    }
}

#[cfg(test)]
mod notice_tests {
    use super::*;
    use crate::testutil::TestDir;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn run(server: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
        (t.run)(server, &args)
    }

    /// A project of the given media, saved, so its file says what those sources are.
    fn saved_project(dir: &Path, name: &str, media: &[PathBuf]) -> PathBuf {
        let project = dir.join(format!("{name}.edith"));
        let mut server = Server::new();
        assert!(!run(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        let paths: Vec<String> = media.iter().map(|p| p.display().to_string()).collect();
        assert!(!run(&mut server, "media_import", json!({"paths": paths})).is_error);
        let saved = run(&mut server, "project_save", json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        project
    }

    /// The autosave notice names a source whose file changed, by its index and both file names.
    #[test]
    fn autosave_notice_names_a_changed_source_path() {
        let td = TestDir::new("notice-src");
        let a = saved_project(&td, "a", &[asset("test_hevc.mp4")]);
        let b = saved_project(&td, "b", &[asset("test_av.mp4")]);
        let fa = PlaybackSession::open_project(&a).expect("open a");
        let fb = PlaybackSession::open_project(&b).expect("open b");
        let text = autosave_summary(&fa, &fb, &[]);
        assert!(text.contains("src:0 test_hevc.mp4 -> test_av.mp4"), "{text}");
        assert!(!text.contains("no clip, lane, setting or source differs"), "{text}");
    }

    /// An import-only change is worded as one: the source added, and that no clip, lane or
    /// setting changed.
    #[test]
    fn autosave_notice_says_an_import_changed_no_clip() {
        let td = TestDir::new("notice-import");
        let a = saved_project(&td, "a", &[asset("test_hevc.mp4")]);
        let c = saved_project(&td, "c", &[asset("test_hevc.mp4"), asset("test_av.mp4")]);
        let fa = PlaybackSession::open_project(&a).expect("open a");
        let fc = PlaybackSession::open_project(&c).expect("open c");
        let text = autosave_summary(&fa, &fc, &[]);
        assert!(text.contains("+1 source(s): src:1 test_av.mp4"), "{text}");
        assert!(text.contains("(no clip, lane or setting changed)"), "{text}");
    }

    /// project_new says the file waits for project_save, and that unsaved work is autosaved
    /// when the session ends: the behaviour, not "nothing is written".
    #[test]
    fn project_new_says_when_the_autosave_is_written() {
        let td = TestDir::new("notice-new");
        let mut server = Server::new();
        let r = run(&mut server, "project_new", json!({"path": td.path().join("n.edith")}));
        assert!(r.text.contains("written only by project_save"), "{}", r.text);
        assert!(r.text.contains("autosaved beside it when this session ends"), "{}", r.text);
        assert!(!r.text.contains("nothing is written until"), "{}", r.text);
    }

    /// A load fix makes the project unsaved, the status says so, and a save writes the fix and says it did.
    #[test]
    fn a_load_fix_makes_the_project_unsaved_until_saved() {
        let td = TestDir::new("notice-fix");
        let project = saved_project(&td, "fix", &[asset("test_av.mp4")]);
        let mut server = Server::new();
        assert!(!run(&mut server, "project_open", json!({"path": project})).is_error);
        assert!(!dirty(&server));
        server.load_fixes = vec!["clip V1@0 ran past its source; cut to the source's end".to_string()];
        assert!(dirty(&server));
        let status = status_reply(&server);
        assert!(status.text.contains("unsaved: the open fixed 1 thing(s) to load"), "{}", status.text);
        assert!(status.text.contains("· dirty ·"), "{}", status.text);
        let saved = run(&mut server, "project_save", json!({}));
        assert!(saved.text.contains("written with the 1 fix(es) made to open it: clip V1@0"), "{}", saved.text);
        assert!(server.load_fixes.is_empty());
        assert!(!dirty(&server));
    }

    /// A source whose file is gone is named in the status with its fix, not only in media_list.
    #[test]
    fn status_names_an_offline_source_with_its_fix() {
        let td = TestDir::new("notice-offline");
        std::fs::copy(asset("test_av.mp4"), td.path().join("moved.mp4")).expect("copy");
        let project = saved_project(&td, "off", &[td.path().join("moved.mp4")]);
        std::fs::remove_file(td.path().join("moved.mp4")).expect("remove");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_open", json!({"path": project})).is_error);
        let status = status_reply(&server);
        assert!(status.text.contains("OFFLINE: src:0 "), "{}", status.text);
        assert!(status.text.contains("(relink with media_relink)"), "{}", status.text);
        let structured = status.structured.expect("structured status");
        assert_eq!(structured["offline"][0]["src"], json!(0));
    }
}

/// Dogfood round 12: the save refusal, dated autosave listing and recovery, and relinks by search.
#[cfg(test)]
mod round12_tests {
    use super::*;
    use crate::testutil::TestDir;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn run(server: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool");
        (t.run)(server, &args)
    }

    /// A project saved with one source, then a second source imported and left to autosave.
    /// Returns the test dir, the project path and the autosave path.
    fn unrecovered(name: &str) -> (TestDir, PathBuf, PathBuf) {
        engine::ao::set_silent_output();
        let td = TestDir::new(name);
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_new", json!({"path": path, "resolution": "1280x720"})).is_error);
        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        assert!(run(&mut server, "project_save", json!({})).text.starts_with("saved"));
        server.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");
        let auto = autosave_at_eof(&server).expect("a dirty project autosaves");
        (td, path, auto)
    }

    /// Item 1: a save over an unrecovered autosave is refused by name, writes nothing, and
    /// `discard_autosave` sets the autosave aside as a dated copy.
    #[test]
    fn save_refuses_while_an_unrecovered_autosave_holds_edits() {
        let (_td, path, auto) = unrecovered("r12-refuse");
        let before = std::fs::read(&path).expect("read the project");
        let auto_bytes = std::fs::read(&auto).expect("read the autosave");
        // The open is clean (no autosave beside the project yet); another session's autosave then
        // appears while this one runs.
        std::fs::remove_file(&auto).expect("take the autosave away before the open");
        let mut fresh = Server::new();
        assert!(!run(&mut fresh, "project_open", json!({"path": path})).is_error);
        std::fs::write(&auto, &auto_bytes).expect("the autosave appears after the open");

        let refused = run(&mut fresh, "project_save", json!({}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.starts_with("an autosave from 20"), "{}", refused.text);
        assert!(refused.text.contains(" holds unsaved edits (+1 source(s)"), "{}", refused.text);
        assert!(refused.text.contains("not loaded by this session, holds unsaved edits"), "{}", refused.text);
        assert!(refused.text.contains("that are not in this project. project_save {discard_autosave:true} keeps THIS session's edits and sets the earlier autosave aside"), "{}", refused.text);
        assert!(refused.text.contains("project_open {\"path\": ") && refused.text.contains("\"recover\": true, \"discard\": true}"), "{}", refused.text);
        assert_eq!(std::fs::read(&path).expect("reread"), before, "the refused save writes nothing");
        assert!(auto.is_file(), "the autosave is still there");

        let saved = run(&mut fresh, "project_save", json!({"discard_autosave": true}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.starts_with("kept the unrecovered autosave as "), "{}", saved.text);
        assert!(!auto.exists(), "the autosave moved aside");
    }

    /// The stuck case: a project never saved, with an earlier session's autosave beside it. Its
    /// save and its reopen each named the other; now each names `project_save {discard_autosave}`
    /// (keeps this session's edits) and `project_open {recover, discard}` (drops them).
    #[test]
    fn already_open_unsaved_project_names_both_ways_out_of_a_stale_autosave() {
        let td = crate::testutil::TestDir::new("r15-stuck");
        let path = td.path().join("p.edith");
        let media = asset("test_av.mp4");
        // The earlier session: a never-saved project whose edits were autosaved at exit.
        let mut earlier = Server::new();
        assert!(!run(&mut earlier, "project_new", json!({"path": path, "resolution": "1280x720"})).is_error);
        earlier.session.as_mut().unwrap().import(&media).expect("import");
        let auto = autosave_at_eof(&earlier).expect("the earlier session autosaves");
        let auto_bytes = std::fs::read(&auto).expect("read the earlier autosave");
        // This session: a new project at the path, started with no autosave beside it (project_new
        // refuses one). Another session's autosave appears while this one runs.
        std::fs::remove_file(&auto).expect("take the autosave away");
        let mut now = Server::new();
        assert!(!run(&mut now, "project_new", json!({"path": path, "resolution": "1280x720"})).is_error);
        std::fs::write(&auto, &auto_bytes).expect("the autosave appears");
        now.session.as_mut().unwrap().import(&asset("test_av2.mp4")).expect("import");

        let refused = run(&mut now, "project_save", json!({}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("project_save {discard_autosave:true}"), "{}", refused.text);
        let reopened = run(&mut now, "project_open", json!({"path": path}));
        assert!(reopened.is_error, "{}", reopened.text);
        assert!(reopened.text.contains("not yet saved"), "{}", reopened.text);
        assert!(reopened.text.contains("project_save {discard_autosave:true}"), "{}", reopened.text);
        assert!(reopened.text.contains("\"discard\": true"), "{}", reopened.text);

        // The way out the refusals name: this session's edits are kept, the earlier autosave is kept too.
        let saved = run(&mut now, "project_save", json!({"discard_autosave": true}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("kept the unrecovered autosave as "), "{}", saved.text);
        assert!(!auto.exists(), "the earlier autosave moved aside");
        assert!(path.is_file(), "this session's edits are saved");
        assert_eq!(std::fs::read_dir(td.path()).unwrap().filter(|e| e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().contains(".autosave-"))).count(), 1);
    }

    /// `project_open {recover, discard}` over unsaved edits drops them and loads the autosave;
    /// without `discard` the recover is refused with both ways out, and nothing is dropped.
    #[test]
    fn recover_over_unsaved_edits_refuses_then_drops_them_only_on_discard() {
        let (_td, path, auto) = unrecovered("r15-recover-dirty");
        let mut fresh = Server::new();
        assert!(run(&mut fresh, "project_open", json!({"path": path})).is_error, "refused over the unloaded autosave");
        assert!(!run(&mut fresh, "project_open", json!({"path": path, "recover": true})).is_error);
        // A settings change makes this session's edits unsaved.
        assert!(!run(&mut fresh, "project_settings", json!({"tonemap": "vivid"})).is_error);
        assert!(dirty(&fresh));

        let refused = run(&mut fresh, "project_open", json!({"path": path, "recover": true}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("project_save {discard_autosave:true}"), "{}", refused.text);
        assert!(refused.text.contains("\"discard\": true"), "{}", refused.text);
        assert!(dirty(&fresh), "a refusal drops nothing");
        assert!(auto.is_file(), "the autosave is untouched by the refusal");

        let ok = run(&mut fresh, "project_open", json!({"path": path, "recover": true, "discard": true}));
        assert!(!ok.is_error, "{}", ok.text);
        assert!(ok.text.starts_with("dropped the unsaved edits in "), "{}", ok.text);
        assert!(ok.text.contains("recovered the unsaved edits from "), "{}", ok.text);
        assert_eq!(fresh.session.as_ref().unwrap().sources().len(), 2, "the autosave's two sources are loaded");
        assert!(auto.is_file(), "the autosave stays until a save removes it");
    }

    /// Item 1: a recovered autosave saves without any flag, and the newest autosave is what
    /// `recover: true` loads.
    #[test]
    fn recovered_autosave_saves_without_a_flag() {
        let (_td, path, _auto) = unrecovered("r12-recover-saves");
        let mut fresh = Server::new();
        assert!(run(&mut fresh, "project_open", json!({"path": path})).is_error, "refused over the unloaded autosave");
        assert!(!run(&mut fresh, "project_open", json!({"path": path, "recover": true})).is_error);
        let saved = run(&mut fresh, "project_save", json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert_eq!(fresh.session.as_ref().unwrap().sources().len(), 2);
    }


    /// Two sessions over one folder. Session 1 leaves a 2-source autosave; session 2 recovers an
    /// older dated copy (1 source) and exits with no edits. The project's autosave must then hold
    /// what session 2 recovered, so session 3's plain recover loads that state and not the replaced
    /// one, and the replaced autosave is kept as a dated copy.
    #[test]
    fn a_dated_recovery_leaves_the_project_autosave_holding_the_recovered_state() {
        let (td, path, auto) = unrecovered("r29-recover-dated-state");
        let auto_bytes = std::fs::read(&auto).expect("read the autosave");
        let dated = td.path().join("proj.autosave-20260101-120000.edith");
        std::fs::copy(&path, &dated).expect("a dated copy of the saved project");

        // Session 2: recover the dated copy, make no edit, and exit.
        let mut second = Server::new();
        let rec = run(&mut second, "project_open", json!({"path": path, "recover": "20260101-120000"}));
        assert!(!rec.is_error, "{}", rec.text);
        assert!(rec.text.contains("set aside as"), "the replaced autosave is named as a dated copy: {}", rec.text);
        assert_eq!(second.session.as_ref().unwrap().sources().len(), 1);
        assert!(autosave_at_eof(&second).is_none(), "an unedited recovery writes nothing new");
        assert!(same_project(&auto, &dated), "the project's autosave now holds the recovered state");

        // The replaced 2-source autosave is kept, not lost.
        let kept: Vec<PathBuf> = dated_autosaves(&path).into_iter().map(|(_, p)| p).filter(|p| p != &dated).collect();
        assert_eq!(kept.len(), 1, "one dated copy holds the replaced autosave");
        assert_eq!(std::fs::read(&kept[0]).expect("read the copy"), auto_bytes, "the copy holds the replaced edits");

        // Session 3: a plain recover loads the same state session 2 recovered, not session 1's.
        let mut third = Server::new();
        let rec = run(&mut third, "project_open", json!({"path": path, "recover": true}));
        assert!(!rec.is_error, "{}", rec.text);
        assert_eq!(third.session.as_ref().unwrap().sources().len(), 1, "the recovered state, not the replaced one");
        // And a save writes that state over the project.
        let saved = run(&mut third, "project_save", json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert_eq!(third.session.as_ref().unwrap().sources().len(), 1);
    }

    /// A recovery of the project's own autosave changes nothing beside it: no dated copy is made.
    #[test]
    fn recovering_the_own_autosave_makes_no_dated_copy() {
        let (_td, path, _auto) = unrecovered("r29-recover-own");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_open", json!({"path": path, "recover": true})).is_error);
        assert!(dated_autosaves(&path).is_empty());
    }

    /// Locks a lane, then changes the frame rate: a lane whose clip edges would move is refused
    /// unless force is set, and the refusal names the lane. A rate that moves no locked edge passes.
    #[test]
    fn an_fps_change_refuses_to_move_a_locked_lane_unless_forced() {
        engine::ao::set_silent_output();
        let by_name = |server: &mut Server, name: &str, args: Value| run(server, name, args);
        let td = crate::testutil::TestDir::new("r29-fps-locked");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_new", json!({"path": path})).is_error);
        let imported = by_name(&mut server, "media_import", json!({"paths": [asset("test_av.mp4").display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        let cut = by_name(&mut server, "edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "1.1s"]], "lane": "V1"}]}));
        assert!(!cut.is_error, "{}", cut.text);
        let locked = by_name(&mut server, "edit", json!({"ops": [{"op": "lane_flags", "lane": "V1", "locked": true}]}));
        assert!(!locked.is_error, "{}", locked.text);
        let steps = server.ledger.len();

        let refused = by_name(&mut server, "project_settings", json!({"fps": 5}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("locked lane V1") && refused.text.contains("force:true"), "{}", refused.text);
        assert_eq!(server.session.as_ref().unwrap().meta().frame_rate, 30.0, "nothing moved");
        assert_eq!(server.ledger.len(), steps, "a refusal pushes no undo step");

        // A whole multiple of the rate keeps every edge on its time: allowed on a locked lane.
        let doubled = by_name(&mut server, "project_settings", json!({"fps": 60}));
        assert!(!doubled.is_error, "{}", doubled.text);
        assert!(!doubled.text.contains("locked"), "{}", doubled.text);
        assert!(by_name(&mut server, "project_settings", json!({"fps": 30})).text.contains("fps 60 -> 30"));

        let forced = by_name(&mut server, "project_settings", json!({"fps": 5, "force": true}));
        assert!(!forced.is_error, "{}", forced.text);
        assert!(forced.text.contains("force:true moved the clips of locked lane V1"), "{}", forced.text);
    }

    /// A project_new folder that does not exist is made at project_new, so project_save later
    /// finds it.
    #[test]
    fn project_new_creates_its_folder_so_a_later_save_works() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("r29-new-folder");
        let path = td.path().join("made").join("later").join("proj.edith");
        let mut server = Server::new();
        let made = run(&mut server, "project_new", json!({"path": path, "resolution": "1280x720"}));
        assert!(!made.is_error, "{}", made.text);
        assert!(made.text.contains("created the folder"), "{}", made.text);
        assert!(path.parent().is_some_and(Path::is_dir), "the folder exists");
        let imported = run(&mut server, "media_import", json!({"paths": [asset("test_av.mp4").display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        let saved = run(&mut server, "project_save", json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(path.is_file());
    }

    /// Item 2: dated autosaves are listed in the open reply with local time and what they
    /// change; `recover` with a file name or a stamp loads the chosen one; a bad name is refused
    /// with the names that exist.
    #[test]
    fn dated_autosaves_are_listed_and_one_is_recoverable_by_name_or_stamp() {
        let (td, path, _auto) = unrecovered("r12-dated");
        let dated = td.path().join("proj.autosave-20260101-120000.edith");
        std::fs::copy(&path, &dated).expect("a dated copy of the saved project");

        // Before any dated recovery: recover: true still loads the project's own (newest) autosave.
        let mut newest = Server::new();
        let rec = run(&mut newest, "project_open", json!({"path": path, "recover": true}));
        assert!(!rec.is_error, "{}", rec.text);
        assert_eq!(newest.session.as_ref().unwrap().sources().len(), 2, "recover: true stays the newest");

        // A plain open is refused over the unloaded newest autosave. The dated copies are listed by
        // a recover, which also names the one it loads (the newest stays unloaded beside it).
        let mut by_stamp = Server::new();
        assert!(run(&mut by_stamp, "project_open", json!({"path": path})).is_error);
        let rec = run(&mut by_stamp, "project_open", json!({"path": path, "recover": "20260101-120000"}));
        assert!(!rec.is_error, "{}", rec.text);
        // The replaced autosave is set aside as a dated copy too, so it is listed beside the chosen one.
        assert!(rec.text.contains("older autosaves (2;"), "{}", rec.text);
        let when = local_text(mtime(&dated).expect("mtime"));
        let entry = format!("proj.autosave-20260101-120000.edith (edits from {when}: no clip, lane, setting or source differs)");
        assert!(rec.text.contains(&entry), "{}", rec.text);
        assert!(rec.text.contains("recovered the unsaved edits from"), "{}", rec.text);
        assert!(rec.text.contains("the dated copy stays beside it"), "{}", rec.text);
        assert_eq!(by_stamp.session.as_ref().unwrap().sources().len(), 1, "the chosen copy is loaded");
        // The project's own autosave now holds the recovered copy, so nothing is left unloaded.
        assert!(!by_stamp.unrecovered_autosave, "the autosave is this session's recovered state");
        assert!(dirty(&by_stamp));

        let mut by_name = Server::new();
        let rec = run(&mut by_name, "project_open", json!({"path": path, "recover": "proj.autosave-20260101-120000.edith"}));
        assert!(!rec.is_error, "{}", rec.text);
        // The recovered state now lives in the project's own autosave, which is what a save replaces.
        assert_eq!(by_name.recovered_autosave.as_deref(), Some(autosave_path(&path).as_path()));

        let mut bad = Server::new();
        let refused = run(&mut bad, "project_open", json!({"path": path, "recover": "nope"}));
        assert!(refused.is_error, "{}", refused.text);
        assert!(refused.text.contains("no dated autosave named nope"), "{}", refused.text);
        assert!(refused.text.contains("proj.autosave-20260101-120000.edith"), "{}", refused.text);
    }

    /// Item 3: a relink by search is unsaved until a save writes the new paths.
    #[test]
    fn relink_by_search_marks_the_project_unsaved() {
        let td = TestDir::new("r12-relink");
        std::fs::create_dir_all(td.path().join("media")).expect("media dir");
        std::fs::create_dir_all(td.path().join("moved")).expect("moved dir");
        std::fs::copy(asset("test_av.mp4"), td.path().join("media/test_av.mp4")).expect("copy");
        let project = td.path().join("rel.edith");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_new", json!({"path": project, "resolution": "1280x720"})).is_error);
        let media = td.path().join("media/test_av.mp4").display().to_string();
        assert!(!run(&mut server, "media_import", json!({"paths": [media]})).is_error);
        assert!(run(&mut server, "project_save", json!({})).text.starts_with("saved"));
        std::fs::rename(td.path().join("media/test_av.mp4"), td.path().join("moved/test_av.mp4")).expect("move");

        let mut fresh = Server::new();
        let search = td.path().join("moved").display().to_string();
        let opened = run(&mut fresh, "project_open", json!({"path": project, "search": [search]}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("relinked 1 source(s); save to keep the new paths"), "{}", opened.text);
        assert!(dirty(&fresh), "a relink is unsaved");
        assert!(status_reply(&fresh).text.contains("dirty"), "{}", status_reply(&fresh).text);

        let saved = run(&mut fresh, "project_save", json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.contains("written with the 1 fix(es) made to open it: relinked 1 source(s) by name"), "{}", saved.text);
        assert!(std::fs::read_to_string(&project).expect("reread").contains("moved"), "the saved file has the new path");
        assert!(!dirty(&fresh));
    }

    /// `project_new {overwrite:true}` replaces the project, so the earlier session's autosave beside
    /// it is set aside as a dated copy: the next save is not refused over it.
    #[test]
    fn project_new_overwrite_sets_the_earlier_autosave_aside() {
        let (_td, path, auto) = unrecovered("p16-overwrite-auto");
        let bytes = std::fs::read(&auto).expect("the autosave");
        let mut server = Server::new();
        let made = project_new(&mut server, &json!({"path": path, "overwrite": true, "resolution": "1280x720"}));
        assert!(!made.is_error, "{}", made.text);
        assert!(made.text.contains("set the earlier autosave"), "{}", made.text);
        assert!(made.text.contains("the copy is kept (the newest 3 dated copies and any from the last 7 days are kept; older ones are deleted)"), "{}", made.text);
        assert!(!auto.exists(), "the autosave moved aside");
        let dated = dated_autosaves(&path);
        assert_eq!(dated.len(), 1, "{:?}", dated);
        assert!(made.text.contains(&dated[0].1.display().to_string()), "the reply names the copy: {}", made.text);
        assert_eq!(std::fs::read(&dated[0].1).expect("read the copy"), bytes, "the copy holds the same edits");

        server.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let saved = project_save(&mut server, &json!({}));
        assert!(!saved.is_error, "{}", saved.text);
        assert!(saved.text.starts_with("saved "), "{}", saved.text);
    }

    /// Without overwrite the earlier autosave is not touched.
    #[test]
    fn project_new_without_overwrite_leaves_the_earlier_autosave_in_place() {
        let (_td, path, auto) = unrecovered("p16-no-overwrite");
        let mut server = Server::new();
        // The project file exists, so project_new refuses without overwrite; the autosave is untouched.
        let made = project_new(&mut server, &json!({"path": path, "resolution": "1280x720"}));
        assert!(made.is_error, "{}", made.text);
        let auto_path = autosave_path(&path).display().to_string();
        assert!(made.text.starts_with(&format!("the autosave at {auto_path} from ")), "{}", made.text);
        assert!(made.text.contains("differs from the saved project by "), "{}", made.text);
        assert!(!made.text.contains("set the earlier autosave"), "{}", made.text);
        assert!(auto.is_file(), "the autosave is still there");
        assert!(dated_autosaves(&path).is_empty());
        assert!(server.session.is_none(), "nothing was created");
    }

    /// `project_new` over an autosave with no project file: refused before anything is created,
    /// naming the autosave and its summary, with both ways on. `overwrite` then sets the autosave
    /// aside as a dated copy (kept), and the recover choice would have loaded it instead.
    #[test]
    fn project_new_refuses_an_autosave_without_a_file_and_overwrite_sets_it_aside() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("new-over-autosave");
        let path = td.path().join("n.edith");
        let mut earlier = Server::new();
        assert!(!run(&mut earlier, "project_new", json!({"path": path, "resolution": "1280x720"})).is_error);
        earlier.session.as_mut().unwrap().import(&asset("test_av.mp4")).expect("import");
        let auto = autosave_at_eof(&earlier).expect("the earlier session autosaves");
        assert!(!path.exists(), "no project file, only the autosave");

        let mut server = Server::new();
        let refused = run(&mut server, "project_new", json!({"path": path, "resolution": "1280x720"}));
        assert!(refused.is_error, "{}", refused.text);
        // The comparison is named: with no project file, the autosave is read against an empty project.
        assert!(refused.text.starts_with(&format!("the autosave at {} from ", auto.display())), "{}", refused.text);
        assert!(refused.text.contains(&format!("differs from an empty project, since {} is not saved yet by +1 source(s)", path.display())), "{}", refused.text);
        assert!(refused.text.contains("project_open {\"path\": ") && refused.text.contains("\"recover\": true}"), "{}", refused.text);
        assert!(refused.text.contains("project_new {\"path\": ") && refused.text.contains("\"overwrite\": true}"), "{}", refused.text);
        assert!(server.session.is_none(), "nothing was created");
        assert!(!path.exists() && auto.is_file(), "nothing was written or moved");

        let fresh = run(&mut server, "project_new", json!({"path": path, "resolution": "1280x720", "overwrite": true}));
        assert!(!fresh.is_error, "{}", fresh.text);
        assert!(fresh.text.contains("set the earlier autosave"), "{}", fresh.text);
        assert!(!auto.exists(), "the autosave moved aside");
        assert_eq!(dated_autosaves(&path).len(), 1, "one dated copy, kept");
    }

    /// Dated copies are capped at the newest DATED_KEEP: a set-aside deletes the older ones and the
    /// reply says which were deleted.
    #[test]
    fn dated_autosave_copies_are_capped_and_the_deletions_are_named() {
        let (_td, path, auto) = unrecovered("p16-cap");
        for day in 1u64..=4 {
            let old = path.with_file_name(format!("proj.autosave-2026010{day}-120000.edith"));
            std::fs::write(&old, b"x").unwrap();
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600 + day * 86_400);
            std::fs::File::options().write(true).open(&old).unwrap().set_modified(t).unwrap();
        }
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path, "discard_autosave": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("deleted 2 older dated autosave(s) beyond the newest 3"), "{}", opened.text);
        assert!(opened.text.contains("proj.autosave-20260101-120000.edith") && opened.text.contains("proj.autosave-20260102-120000.edith"), "{}", opened.text);
        let left: Vec<String> = dated_autosaves(&path).iter().map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), DATED_KEEP, "{:?}", left);
        assert!(left.iter().any(|n| n.starts_with("proj.autosave-2026")) && !left.contains(&"proj.autosave-20260101-120000.edith".to_string()));
        assert!(!auto.exists());
        assert!(opened.text.contains(&kept_name(&left)), "the new copy is among those kept: {}", opened.text);
    }

    /// A dated copy written within the last 7 days is kept even past the cap; an older one beyond it
    /// is deleted, and the reply says its edits are gone.
    #[test]
    fn a_recent_dated_copy_past_the_cap_is_kept_and_an_old_one_is_said_to_be_gone() {
        let (_td, path, auto) = unrecovered("p17-recent");
        let now = SystemTime::now();
        for (name, days) in [("recent-a", 1u64), ("recent-b", 2), ("recent-c", 3), ("old-d", 30)] {
            let copy = path.with_file_name(format!("proj.autosave-{name}.edith"));
            std::fs::write(&copy, b"x").unwrap();
            let t = now - Duration::from_secs(days * 86_400);
            std::fs::File::options().write(true).open(&copy).unwrap().set_modified(t).unwrap();
        }
        let mut fresh = Server::new();
        let opened = project_open(&mut fresh, &json!({"path": path, "discard_autosave": true}));
        assert!(!opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("deleted 1 older dated autosave(s)"), "{}", opened.text);
        assert!(opened.text.contains("proj.autosave-old-d.edith"), "{}", opened.text);
        assert!(opened.text.contains("Their unsaved edits are gone"), "{}", opened.text);
        let left: Vec<String> = dated_autosaves(&path).iter().map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(left.len(), 4, "the new copy and the three recent ones: {:?}", left);
        assert!(!left.iter().any(|n| n.contains("old-d")), "{:?}", left);
        assert!(!auto.exists());
    }

    /// The name of the newest copy among `left`: the one just set aside has the latest stamp.
    fn kept_name(left: &[String]) -> String {
        left.iter().max().cloned().unwrap_or_default()
    }

    /// Item 4: an unsupported version names the versions this build reads.
    #[test]
    fn unsupported_version_names_the_versions_this_build_reads() {
        let td = TestDir::new("p16-version");
        let path = td.path().join("old.edith");
        std::fs::write(&path, b"edith 99\n").unwrap();
        let mut server = Server::new();
        let opened = project_open(&mut server, &json!({"path": path}));
        assert!(opened.is_error, "{}", opened.text);
        assert!(opened.text.contains(&format!("unsupported version 99; this edith reads project versions up to {}", engine::edith::NEWEST_VERSION)), "{}", opened.text);
        assert!(opened.text.contains(&path.display().to_string()), "the reply names the file: {}", opened.text);
    }

    /// Item 4: a garbage file gets the corrected grammar and the dated copies beside it as the way out.
    #[test]
    fn garbage_project_names_the_dated_copies_as_the_way_out() {
        let td = TestDir::new("p16-garbage");
        let path = td.path().join("bad.edith");
        std::fs::write(&path, b"not a project at all\n").unwrap();
        let dated = td.path().join("bad.autosave-20260101-120000.edith");
        std::fs::write(&dated, b"x").unwrap();
        let mut server = Server::new();
        let opened = project_open(&mut server, &json!({"path": path}));
        assert!(opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("not an edith file"), "{}", opened.text);
        assert!(!opened.text.contains("not a edith"), "{}", opened.text);
        assert!(opened.text.contains("the way out is the dated autosaves bad.autosave-20260101-120000.edith"), "{}", opened.text);
    }

    /// Item 4: with no autosave or dated copy beside it, the way out is the checkpoints.
    #[test]
    fn empty_project_names_its_checkpoints_as_the_way_out() {
        let td = TestDir::new("p16-empty");
        let path = td.path().join("empty.edith");
        std::fs::write(&path, b"").unwrap();
        let cps = td.path().join("checkpoints").join("empty");
        std::fs::create_dir_all(&cps).unwrap();
        std::fs::write(cps.join("before-cut.edith"), b"x").unwrap();
        let mut server = Server::new();
        let opened = project_open(&mut server, &json!({"path": path}));
        assert!(opened.is_error, "{}", opened.text);
        assert!(opened.text.contains("the way out is the checkpoints before-cut"), "{}", opened.text);
    }

    /// Item 5: a refused write names the folder to choose instead of a bare "Permission denied".
    #[test]
    fn a_permission_refusal_on_save_names_the_folder_to_choose() {
        let got = write_failure(Path::new("/ro/x.edith"), "Permission denied (os error 13)");
        assert_eq!(got, "cannot write /ro/x.edith: permission denied; choose a folder you can write to");
        assert!(write_failure(Path::new("/ro/x.edith"), "disk full").starts_with("save failed: could not write /ro/x.edith"));
    }

    /// Item 6: a relative path is resolved against the working directory and the reply says so.
    #[test]
    fn a_relative_path_is_shown_as_the_absolute_file_used() {
        let mut server = Server::new();
        let made = project_new(&mut server, &json!({"path": "p16-rel-x.edith", "resolution": "1280x720"}));
        assert!(!made.is_error, "{}", made.text);
        let abs = std::env::current_dir().unwrap().join("p16-rel-x.edith");
        assert!(made.text.contains(&format!("new project {}", abs.display())), "{}", made.text);
        assert!(made.text.contains("p16-rel-x.edith is a relative path; the file used is "), "{}", made.text);
        assert_eq!(server.path.as_deref(), Some(abs.as_path()));
    }

    /// The locked-lane note names each kept lane's moved addresses, and leaves out lanes whose times
    /// moved (the force warning covers those) and addresses that did not change.
    #[test]
    fn locked_address_note_names_the_moved_frame_addresses() {
        let v1 = Lane::V1;
        let a1 = Lane::A1;
        let before = vec![(v1, 100), (v1, 0), (a1, 50)];
        let after = vec![(v1, 120), (v1, 0), (a1, 60)];
        assert_eq!(
            locked_address_note(&before, &after, &[]).as_deref(),
            Some("locked lanes V1, A1 keep their times; their frame addresses change: V1@100→V1@120, A1@50→A1@60")
        );
        // A lane whose times moved is named by the force warning, not here.
        assert_eq!(
            locked_address_note(&before, &after, &["A1".to_string()]).as_deref(),
            Some("locked lane V1 keeps its times; its frame addresses change: V1@100→V1@120")
        );
        // Nothing changes address: no note.
        assert_eq!(locked_address_note(&before, &before, &[]), None);
    }

    /// Past eight moved addresses the note says how many it leaves out.
    #[test]
    fn locked_address_note_counts_the_addresses_it_leaves_out() {
        let before: Vec<(Lane, u32)> = (0..10).map(|i| (Lane::V1, i * 10)).collect();
        let after: Vec<(Lane, u32)> = (0..10).map(|i| (Lane::V1, i * 12)).collect();
        let note = locked_address_note(&before, &after, &[]).expect("a note");
        assert!(note.ends_with("V1@80→V1@96, … (1 more)"), "{note}");
        assert_eq!(note.matches('→').count(), ADDRESSES_SHOWN, "{note}");
    }

    /// An exact fps change on a locked lane is allowed, says which addresses moved, and keeps the times.
    #[test]
    fn an_exact_fps_change_on_a_locked_lane_notes_its_new_addresses() {
        engine::ao::set_silent_output();
        let td = crate::testutil::TestDir::new("r30-fps-note");
        let path = td.path().join("proj.edith");
        let mut server = Server::new();
        assert!(!run(&mut server, "project_new", json!({"path": path})).is_error);
        let imported = run(&mut server, "media_import", json!({"paths": [asset("test_av.mp4").display().to_string()]}));
        assert!(!imported.is_error, "{}", imported.text);
        // One clip at 0 s and one at 2 s (frame 60 at 30 fps); the second moves to frame 120 at 60 fps.
        let first = run(&mut server, "edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "1s"]], "lane": "V1"}]}));
        assert!(!first.is_error, "{}", first.text);
        let second = run(&mut server, "edit", json!({"ops": [{"op": "keep", "source": "src:0", "ranges": [["0s", "1s"]], "lane": "V1", "at": "2s"}]}));
        assert!(!second.is_error, "{}", second.text);
        let locked = run(&mut server, "edit", json!({"ops": [{"op": "lane_flags", "lane": "V1", "locked": true}]}));
        assert!(!locked.is_error, "{}", locked.text);

        let doubled = run(&mut server, "project_settings", json!({"fps": 60}));
        assert!(!doubled.is_error, "{}", doubled.text);
        assert!(
            doubled.text.contains("locked lane V1 keeps its times; its frame addresses change: V1@60→V1@120"),
            "{}",
            doubled.text
        );
        assert!(!doubled.text.contains("warning"), "{}", doubled.text);
        let kept: Vec<(Lane, u32)> = locked_starts(server.session.as_ref().unwrap());
        assert_eq!(kept, vec![(Lane::V1, 0), (Lane::V1, 120)], "the clips moved to the new frame numbers");
    }

    /// A recovery names a dated autosave by its file name or by its stamp, collision suffix included.
    #[test]
    fn autosave_matches_takes_the_listed_file_name_or_its_stamp() {
        let auto = Path::new("/x/E.autosave-20261018-200851.edith");
        assert!(autosave_matches(auto, "E.autosave-20261018-200851.edith"));
        assert!(autosave_matches(auto, "20261018-200851"));
        assert!(!autosave_matches(auto, "20261018-200844"), "the content time is not the stamp");
        let second = Path::new("/x/E.autosave-20261018-200851-2.edith");
        assert!(autosave_matches(second, "20261018-200851-2"));
        assert!(autosave_matches(second, "E.autosave-20261018-200851-2.edith"));
    }
}
