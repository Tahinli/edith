//! Voice-over recording: `record` starts a take from the audio input, and on stop
//! imports it as a source and places it on an audio lane at the start point, as one
//! undo step. The take is a 16-bit WAV in `<project stem>_media/` beside the project,
//! the folder collect uses. `voiceover` is different: it speaks text with Piper.

use serde_json::{Value, json};

use engine::project::LaneKind;
use engine::recorder::Recorder;

use super::project::dirty;
use super::speak::{check_place, imported_words};
use crate::server::{Reply, Server, Tool, opt_str};

const RECORD_DOC: &str = "Record a voice-over from the audio input (the default input, or source: a source name such as alsa_input.usb-mic, \
as status lists them). start begins a take at, by default, the playhead: at is a time such as 5s or 150f. lane is the audio lane it will land on; \
without lane, stop adds a new audio lane for it. stop ends the take, imports the WAV as a source and places it at the start point, as one undo step. \
status reports the take's clock and input level in dBFS while recording, and the input list when idle. \
The take is a 16-bit stereo WAV at 48 kHz in <project stem>_media/voiceover-<n>.wav beside the project (the folder project_collect uses), so it needs a project path (project_new or project_open). \
This is not voiceover, which speaks text with Piper. Only one take runs at a time.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["start", "stop", "status"], "description": "start a take, stop it and place it, or report the take in progress (and the inputs when idle)."},
            "lane": {"type": "string", "examples": ["A2"], "description": "An existing audio lane to place the take on, such as A2. Default: a new audio lane. Used by start."},
            "at": {"type": "string", "examples": ["5s"], "description": "Where the take starts on the timeline, such as 5s or 150f. Default: the playhead. Used by start."},
            "source": {"type": "string", "examples": ["edtest.monitor"], "description": "The input to record from, by name (see status). Default: the audio server's default input. Used by start."}
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// A take in progress: the recorder and where its clip goes.
pub struct Active {
    pub recorder: Recorder,
    pub lane: Option<String>,
    pub at: String,
}

pub fn tools() -> Vec<Tool> {
    vec![Tool { name: "record", description: RECORD_DOC, schema, run: run_record }]
}

fn run_record(server: &mut Server, args: &Value) -> Reply {
    match opt_str(args, "action") {
        Some("start") => run_start(server, args),
        Some("stop") => run_stop(server),
        Some("status") | None => run_status(server),
        Some(other) => Reply::err(format!("unknown action `{other}`: use start, stop or status")),
    }
}

fn run_start(server: &mut Server, args: &Value) -> Reply {
    if server.recording.is_some() {
        return Reply::err("already recording: record {action:\"stop\"} ends this take first");
    }
    let Some(project) = server.path.clone() else {
        return Reply::err("record needs a project path to keep the take beside: open or start one with project_open or project_new {path:\"…\"} first");
    };
    let at = {
        let s = match server.session_ref() {
            Ok(s) => s,
            Err(r) => return r,
        };
        match opt_str(args, "at") {
            Some(at) => at.to_string(),
            None => format!("{:.3}s", s.now()),
        }
    };
    let lane = opt_str(args, "lane").map(str::to_string);
    if let Some(lane) = &lane
        && let Err(r) = check_place(server, lane, &at)
    {
        return r;
    }
    let folder = engine::collect::default_folder(&project);
    let recorder = match Recorder::start(&folder, opt_str(args, "source")) {
        Ok(r) => r,
        Err(e) => return Reply::err(e),
    };
    let path = recorder.status().path;
    server.recording = Some(Active { recorder, lane, at: at.clone() });
    Reply::ok(format!("recording to {} from {at}; record {{action:\"stop\"}} places it", path.display()))
}

fn run_status(server: &mut Server) -> Reply {
    let Some(active) = &server.recording else {
        let inputs = match engine::pulse::list_sources() {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|s| format!("{} ({}{})", s.name, s.description, if s.monitor { ", monitor" } else { "" }))
                .collect::<Vec<_>>()
                .join("; "),
            _ => "none (no audio server answers)".to_string(),
        };
        return Reply::ok(format!("not recording. inputs: {inputs}"));
    };
    let status = active.recorder.status();
    Reply::ok(format!(
        "recording {:.2}s to {} at {}, level {:.1} dBFS",
        status.elapsed_secs,
        status.path.display(),
        active.at,
        status.level_db
    ))
}

fn run_stop(server: &mut Server) -> Reply {
    let Some(active) = server.recording.take() else {
        return Reply::err("not recording: record {action:\"start\"} begins a take");
    };
    let take = match active.recorder.stop() {
        Ok(t) => t,
        Err(e) => return Reply::err(format!("the take failed: {e}")),
    };
    if take.frames == 0 {
        return Reply::err(format!("nothing was captured: {} is empty, so nothing was placed", take.path.display()));
    }

    let start = match server.session_ref() {
        Ok(s) => s.history_pushes(),
        Err(r) => return r,
    };
    let source = {
        let s = match server.session() {
            Ok(s) => s,
            Err(r) => return r,
        };
        match s.import(&take.path) {
            Ok(n) => n,
            Err(e) => return Reply::err(format!("{}: refused: {e}", take.path.display())),
        }
    };
    let import_steps = match server.session_ref() {
        Ok(s) => s.history_pushes().saturating_sub(start) as usize,
        Err(r) => return r,
    };

    // A new audio lane is the next A-number after the audio lanes there are.
    let (lane, add_lane) = match active.lane {
        Some(lane) => (lane, false),
        None => {
            let audio = match server.session_ref() {
                Ok(s) => s.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio).count(),
                Err(r) => return r,
            };
            (format!("A{}", audio + 1), true)
        }
    };
    let mut ops = Vec::new();
    if add_lane {
        ops.push(json!({"op": "lane_add", "kind": "audio"}));
    }
    ops.push(json!({"op": "place", "source": source, "lane": lane, "at": active.at}));
    let file_name = take.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let label = format!("record voice-over {file_name}");
    let placed = super::edit::run_edit(server, &json!({"ops": ops, "label": label}));
    if placed.is_error {
        return Reply::err(format!("{} was imported as src:{source} but not placed: {}", take.path.display(), placed.text));
    }
    // The import and the place are one undo step: the place's entry takes the import's too.
    if let Some(entry) = server.ledger.last_mut() {
        entry.steps += import_steps;
    }
    server.redo_ledger.clear();
    server.history_lost_on_open = false;
    server.trim_ledger();

    let unsaved = dirty(server);
    Reply::ok(format!(
        "{file_name} ({:.2}s) {}, placed on {lane} at {}, one undo step\n{}",
        take.duration_secs,
        imported_words(source, unsaved),
        active.at,
        placed.text
    ))
    .with(json!({
        "source": source,
        "path": take.path.display().to_string(),
        "duration": take.duration_secs,
        "lane": lane,
    }))
}
