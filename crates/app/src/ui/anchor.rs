//! Where a dropdown control sits, so the list it opens hangs from that control
//! (below it, or above it when the window has no room below) instead of from
//! wherever the pointer happened to land. Every dropdown box records its
//! bounds while it is painted ([`probe`]); the click that opens a list asks
//! which recorded box holds the click ([`control_at`]).

use gpui::{Bounds, IntoElement, Pixels, Point, Styled, canvas};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

thread_local! {
    static ROWS: RefCell<HashMap<(std::path::PathBuf, usize), Bounds<Pixels>>> = RefCell::new(HashMap::new());
    static BOXES: RefCell<HashMap<&'static str, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    /// The window's height as the last painted control saw it.
    static VIEWPORT_H: Cell<f32> = const { Cell::new(0.) };
}

/// An invisible child that records its parent's bounds under `id` each frame.
pub(crate) fn probe(id: &'static str) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            VIEWPORT_H.with(|v| v.set(f32::from(window.viewport_size().height)));
            BOXES.with(|b| {
                b.borrow_mut().insert(id, bounds);
            })
        },
        |_, _, _, _| (),
    )
    .absolute()
    .size_full()
}

/// Whether a list `list_h` tall hung from the control `id` would run off the bottom of the
/// window, so it should open above the control instead. An unknown control does not flip.
pub(crate) fn opens_above(id: &'static str, list_h: f32) -> bool {
    let Some(control) = BOXES.with(|b| b.borrow().get(id).copied()) else {
        return false;
    };
    let bottom = f32::from(control.origin.y + control.size.height) + 2. + list_h;
    bottom > VIEWPORT_H.with(Cell::get) - crate::layout::MENU_EDGE
}

/// The bounds a control recorded under `id` when it was last painted, if any.
pub(crate) fn box_of(id: &'static str) -> Option<Bounds<Pixels>> {
    BOXES.with(|b| b.borrow().get(id).copied())
}

/// The recorded control that holds `click`, if any.
pub(crate) fn control_at(click: Point<Pixels>) -> Option<Bounds<Pixels>> {
    BOXES.with(|b| b.borrow().values().find(|r| r.contains(&click)).copied())
}

/// The list's top-left for a control: its left edge and bottom, and the y it
/// would take instead when it opens above ([`Bounds::origin`]'s y).
pub(crate) fn hang(control: Bounds<Pixels>) -> (Point<Pixels>, f32) {
    (
        Point { x: control.origin.x, y: control.origin.y + control.size.height + gpui::px(2.) },
        f32::from(control.origin.y) - 2.,
    )
}

/// An invisible child that records its parent's bounds as the media row (or tile) of a source.
pub(crate) fn probe_row(path: &std::path::Path, stream: usize) -> impl IntoElement {
    let key = (path.to_path_buf(), stream);
    canvas(
        move |bounds, _, _| ROWS.with(|r| {
                r.borrow_mut().insert(key.clone(), bounds);
            }),
        |_, _, _, _| (),
    )
    .absolute()
    .size_full()
}

/// Where a source's row was last painted.
pub(crate) fn row_of(path: &std::path::Path, stream: usize) -> Option<Bounds<Pixels>> {
    ROWS.with(|r| r.borrow().get(&(path.to_path_buf(), stream)).copied())
}
