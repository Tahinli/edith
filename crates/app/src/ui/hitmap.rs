//! Machine-readable bounds for the Darkroom's pointer controls.
use crate::ActionId;

use gpui::*;
use std::{
    cell::RefCell,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};

const WRITE_INTERVAL: Duration = Duration::from_millis(250);

struct Target {
    id: String,
    label: String,
    bounds: Bounds<Pixels>,
    enabled: bool,
}

#[derive(Default)]
struct State {
    targets: Vec<Target>,
    last_write: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn output_path() -> Option<&'static Path> {
    static PATH: LazyLock<Option<PathBuf>> =
        LazyLock::new(|| std::env::var_os("EDITH_HITMAP").map(PathBuf::from));
    PATH.as_deref()
}

/// Clears the current-frame controls before descendants record their laid-out
/// bounds, then writes the completed frame after all descendants prepaint.
pub(crate) fn frame() -> Option<impl IntoElement> {
    output_path()?;
    Some(
        canvas(
            |_, _, _| STATE.with(|state| state.borrow_mut().targets.clear()),
            |_, _, _, _| flush(),
        )
        .absolute()
        .size_full(),
    )
}

/// An invisible child that records a control's final window-relative bounds.
pub(crate) fn control(
    id: &'static str,
    label: &'static str,
    enabled: bool,
) -> Option<impl IntoElement> {
    dynamic(|| (id.into(), label.into()), enabled)
}

/// Dynamic controls defer identifier allocation until instrumentation is on.
pub(crate) fn dynamic(
    target: impl FnOnce() -> (String, String),
    enabled: bool,
) -> Option<impl IntoElement> {
    output_path()?;
    let (id, label) = target();
    Some(
        canvas(
            move |bounds, _, _| {
                STATE.with(|state| {
                    state.borrow_mut().targets.push(Target {
                        id,
                        label,
                        bounds,
                        enabled,
                    });
                });
            },
            |_, _, _, _| (),
        )
        .absolute()
        .size_full(),
    )
}

fn flush() {
    let Some(path) = output_path() else {
        return;
    };
    let now = Instant::now();
    let body = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state
            .last_write
            .is_some_and(|last| now.duration_since(last) < WRITE_INTERVAL)
        {
            return None;
        }
        state.last_write = Some(now);
        let mut json = String::from("[");
        for (i, target) in state.targets.iter().enumerate() {
            if i != 0 {
                json.push(',');
            }
            json.push_str("{\"id\":");
            json_string(&mut json, &target.id);
            json.push_str(",\"label\":");
            json_string(&mut json, &target.label);
            let bounds = target.bounds;
            let _ = write!(
                json,
                ",\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"enabled\":{}",
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
                target.enabled,
            );
            json.push('}');
        }
        json.push(']');
        Some(json)
    });
    let Some(body) = body else {
        return;
    };
    let temp = path.with_extension(format!("hitmap-{}.tmp", std::process::id()));
    if fs::write(&temp, body).is_ok() {
        let _ = fs::rename(temp, path);
    }
}

fn json_string(json: &mut String, value: &str) {
    json.push('"');
    for c in value.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            c if c <= '\u{1f}' => {
                let _ = write!(json, "\\u{:04x}", c as u32);
            }
            c => json.push(c),
        }
    }
    json.push('"');
}

pub(crate) fn action_id(action: ActionId) -> String {
    format!("action.{action:?}")
}

/// Records an action control under the action name it dispatches.
pub(crate) fn action(action: ActionId, enabled: bool) -> Option<impl IntoElement> {
    dynamic(move || (action_id(action), action.label().into()), enabled)
}
