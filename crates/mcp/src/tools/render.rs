//! Render preview: pre-render a stretch of the timeline so playback over it reads
//! frames instead of compositing them (`engine::render_cache`). One render at a time.
use engine::render_cache;
use serde_json::{json, Value};

use crate::parse::{session_end, session_fps};
use crate::server::{opt_bool, tri, Reply, Server, Tool};
use crate::tools::export::parse_range;

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "render_preview",
        description: "Pre-render part of the timeline into the render cache, so playback over it reads finished frames instead of compositing them: use it over heavy sections (stacked effects, titles over video, colour, speed, multicam) that drop frames live. `range` is \"all\" (the default, the whole timeline) or {from, to} as export takes it. The render runs in the background; project_status shows its progress and the cached ranges. Any edit inside a cached range drops that render, and playback composes live again there. `cancel: true` stops the render running now and removes its partial file. One render runs at a time. The sound is not rendered: the live mix always plays.",
        schema,
        run: render_preview,
    }]
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "range": {
                "type": ["string", "object"],
                "description": "\"all\" for the whole timeline (the default), or {\"from\": \"<time>\", \"to\": \"<time>\"}.",
                "examples": [{"from": "2s", "to": "5s"}]
            },
            "cancel": { "type": "boolean", "description": "true cancels the render running now." }
        }
    })
}

fn render_preview(srv: &mut Server, args: &Value) -> Reply {
    if opt_bool(args, "cancel").unwrap_or(false) {
        return match srv.render.as_ref().filter(|(job, _)| !job.is_finished()) {
            Some((job, _)) => {
                job.cancel();
                Reply::ok("render cancelled; its partial file is removed")
            }
            None => Reply::ok("no render is running"),
        };
    }
    if srv.render.as_ref().is_some_and(|(job, _)| !job.is_finished()) {
        return Reply::err("a render preview is already running: poll project_status, or stop it with render_preview {cancel: true}");
    }
    // A render that finished since the last ask: say how it went, once.
    let mut note = String::new();
    if let Some(Err(e)) = srv.render.take().and_then(|(job, _)| job.outcome()) {
        note = format!("the last render failed: {e}\n");
    }
    let s = tri!(srv.session_ref());
    let fps = session_fps(s);
    let end = session_end(s);
    let (from, to) = match args.get("range") {
        None | Some(Value::Null) => (0, end),
        Some(Value::String(word)) if word == "all" => (0, end),
        Some(_) => match parse_range(args, fps, end, end) {
            Ok(Some(range)) => range,
            Ok(None) => (0, end),
            Err(e) => return Reply::err(e),
        },
    };
    let (project, meta) = s.export_snapshot();
    let size = engine::proxy::size_for(&meta);
    match render_cache::start(project, meta, from, to) {
        Ok(job) => {
            let done = job.is_finished();
            srv.render = Some((job, (from, to)));
            let secs = |f: u32| format!("{:.1}s", f64::from(f) / fps);
            let what = match done {
                true => "is already cached".to_string(),
                false => format!("is rendering at {}x{}", size.0, size.1),
            };
            Reply::ok(format!("{note}frames {}-{} {what}; project_status shows it", secs(from), secs(to)))
        }
        Err(e) => Reply::err(format!("{note}{e}")),
    }
}

/// The render line `project_status` shows: what is rendering and what is cached.
pub fn status_line(server: &Server) -> Option<String> {
    let s = server.session_ref().ok()?;
    let fps = session_fps(s);
    let secs = |f: u32| format!("{:.1}s", f64::from(f) / fps);
    let mut parts = Vec::new();
    if let Some((job, (from, to))) = server.render.as_ref().filter(|(job, _)| !job.is_finished()) {
        parts.push(format!("rendering {}-{} {:.0}%", secs(*from), secs(*to), job.progress() * 100.0));
    }
    let (project, meta) = s.export_snapshot();
    let cached = render_cache::segments(&project, &meta);
    if cached.is_empty() {
        parts.push("nothing cached".to_string());
    } else {
        let ranges: Vec<String> = cached.iter().map(|c| format!("{}-{}", secs(c.from), secs(c.to))).collect();
        parts.push(format!("cached {}", ranges.join(", ")));
    }
    Some(format!("render preview: {}", parts.join(" · ")))
}
