//! The comment markers on the window's side: dropping one at the playhead, walking the
//! playhead from marker to marker, and the markers card's rows (pick, retype, recolour,
//! remove). The list itself is the project's ([`engine::project::Marker`]); every change
//! is one undo step through the session, and a move of the playhead edits nothing.

use crate::ui::widgets::MarkerField;
use crate::*;
use engine::project::{MAX_MARKER_NAME_CHARS, Marker};

/// The name a marker dropped with the key gets: the lowest `Marker N` nobody in the list
/// holds, so a run of them reads apart on the card, a renamed or removed one frees its
/// number again, and a name never doubles up.
pub(crate) fn default_marker_name<'a>(taken: impl IntoIterator<Item = &'a str>) -> String {
    let taken: std::collections::HashSet<&str> = taken.into_iter().collect();
    (1..)
        .map(|n| format!("Marker {n}"))
        .find(|name| !taken.contains(name.as_str()))
        .unwrap_or_default()
}

/// The frame the next (`forward`) or previous marker sits on, strictly after or before
/// `now`; `None` when there is none that way.
pub(crate) fn marker_after(markers: &[Marker], now: u32, forward: bool) -> Option<u32> {
    if forward {
        markers.iter().map(|m| m.frame).find(|&f| f > now)
    } else {
        markers.iter().map(|m| m.frame).rev().find(|&f| f < now)
    }
}

/// Why the engine refused a marker, in a plain sentence: the engine words its refusals for a log,
/// and a toast is read by the person who pressed the key.
pub(crate) fn marker_refusal(reason: &str) -> &'static str {
    if reason.contains("needs a name") {
        "The name is empty."
    } else if reason.contains("name holds at most") {
        "The name is too long."
    } else if reason.contains("note holds at most") {
        "The note is too long."
    } else if reason.contains("runs past") {
        "It would run past the end of the timeline."
    } else {
        "The timeline would not take it."
    }
}

impl Player {
    /// Drops a marker on the frame the playhead shows, named by its place in the list,
    /// blue, and picked on the card. One undo step.
    pub(crate) fn add_marker_here(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let frame = crate::subs::frame_at(session.now(), self.fps);
        self.add_marker_at(frame, cx);
    }

    /// "Add marker here" from a ruler or empty-track menu: at the frame that menu was opened on,
    /// the playhead's frame when it was not.
    pub(crate) fn add_marker_at_menu(&mut self, cx: &mut Context<Self>) {
        match self.menu_frame {
            Some(frame) => self.add_marker_at(frame, cx),
            None => self.add_marker_here(cx),
        }
    }

    /// Drops a marker at `frame` (see [`Self::add_marker_here`]).
    pub(crate) fn add_marker_at(&mut self, frame: u32, cx: &mut Context<Self>) {
        let fps = self.fps;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let marker = Marker {
            frame,
            length: 0,
            name: default_marker_name(session.markers().iter().map(|m| m.name.as_str())),
            note: String::new(),
            color: Default::default(),
        };
        match session.add_marker(marker) {
            Ok(idx) => {
                self.marker_sel = idx;
                self.mark_structural();
                self.notify_user(format!("Marker added at {}", frames_timecode(frame, fps)).into());
            }
            Err(e) => self.notify_user(format!("No marker added. {}", marker_refusal(&e.to_string())).into()),
        }
        cx.notify();
    }

    /// Walks the playhead to the next (or previous) marker.
    pub(crate) fn jump_marker(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        match marker_after(session.markers(), crate::subs::frame_at(session.now(), self.fps), forward) {
            Some(frame) => {
                let at = f64::from(frame) / self.fps;
                self.seek(at, cx);
            }
            None => {
                let way = if forward { "after" } else { "before" };
                self.notify_user(format!("No marker {way} the playhead").into());
                cx.notify();
            }
        }
    }

    /// Opens the markers card. Nothing here is a clip's, so nothing is refused for want
    /// of a selection.
    pub(crate) fn open_markers(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        // One card at a time -- `close_card`'s own rule.
        self.close_other_cards();
        self.markers_open = true;
        let count = self.session.as_ref().map_or(0, |s| s.markers().len());
        self.marker_sel = self.marker_sel.min(count.saturating_sub(1));
        self.context_menu = None;
        // Cards live in the Clip tab; every opening door routes the dock there.
        self.dock_src_active = false;
        crate::ui::dock_stance::save(false);
        cx.notify();
    }

    /// Moves the card's pick by `by` rows, wrapping.
    pub(crate) fn step_marker_pick(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.session.as_ref().map_or(0, |s| s.markers().len());
        if count > 0 {
            self.marker_sel = (self.marker_sel as isize + by).rem_euclid(count as isize) as usize;
        }
        cx.notify();
    }

    /// Puts the playhead on marker `idx`.
    pub(crate) fn seek_marker(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(frame) = self.session.as_ref().and_then(|s| s.markers().get(idx).map(|m| m.frame)) else {
            return;
        };
        self.marker_sel = idx;
        let at = f64::from(frame) / self.fps;
        self.seek(at, cx);
    }

    /// Sets marker `idx` to `color`, the one the colour list named. One undo step.
    pub(crate) fn set_marker_color(&mut self, idx: usize, color: engine::project::MarkerColor, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(mut marker) = session.markers().get(idx).cloned() else {
            return;
        };
        marker.color = color;
        if session.set_marker(idx, marker).is_ok() {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// Takes marker `idx` off. One undo step.
    pub(crate) fn remove_marker_at(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.remove_marker(idx) {
            let count = session.markers().len();
            self.marker_sel = self.marker_sel.min(count.saturating_sub(1));
            self.marker_edit = None;
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The ruler's menu on marker `idx`: the marker is selected first, so its verbs act on it.
    pub(crate) fn open_marker_menu(&mut self, idx: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.menu_allowed() || self.session.is_none() {
            return;
        }
        self.marker_sel = idx;
        self.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Marker,
            at,
            details: false,
            sub: Sub::Closed,
        });
        cx.notify();
    }

    /// Renames marker `idx` from the ruler (its double-click, or the menu's "Rename marker"):
    /// the markers card opens with the name being typed on that row, where the name is edited.
    pub(crate) fn rename_marker_at(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.begin_marker_edit(idx, MarkerField::Name, cx);
    }

    /// The menu's "Rename marker": the marker the menu was opened on.
    pub(crate) fn rename_marker_at_pick(&mut self, cx: &mut Context<Self>) {
        self.rename_marker_at(self.marker_sel, cx);
    }

    /// The menu's "Delete marker": the marker the menu was opened on. One undo step.
    pub(crate) fn delete_marker_at_pick(&mut self, cx: &mut Context<Self>) {
        self.remove_marker_at(self.marker_sel, cx);
    }

    /// Starts retyping marker `idx`'s name or note, from the words it has.
    pub(crate) fn begin_marker_edit(&mut self, idx: usize, field: MarkerField, cx: &mut Context<Self>) {
        let Some(marker) = self.session.as_ref().and_then(|s| s.markers().get(idx)) else {
            return;
        };
        let seed = match field {
            MarkerField::Name => marker.name.clone(),
            MarkerField::Note => marker.note.clone(),
        };
        self.marker_sel = idx;
        self.marker_edit = Some(MarkerEdit::new(idx, field, &seed));
        cx.notify();
    }

    /// Writes what was typed into the marker, as one undo step; a name left empty is
    /// refused out loud and the old one stays. Nothing typed that differs is no edit.
    pub(crate) fn commit_marker_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.marker_edit.take() else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(mut marker) = session.markers().get(edit.idx).cloned() else {
            return;
        };
        let text = edit.text.trim().to_string();
        match edit.field {
            MarkerField::Name if text.is_empty() => {
                self.notify_user(format!("A marker needs a name (up to {MAX_MARKER_NAME_CHARS} characters)").into());
                cx.notify();
                return;
            }
            MarkerField::Name => marker.name = text,
            MarkerField::Note => marker.note = text,
        }
        if session.set_marker(edit.idx, marker).is_ok() {
            self.mark_dirty();
        }
        cx.notify();
    }

    /// The keyboard while a marker's words are being retyped (ahead of the palette and
    /// every chord, as the cue editor is): enter saves, escape gives up, and the rest is
    /// text. `key_char` is the character the layout made.
    pub(crate) fn marker_edit_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match key {
            "escape" => self.marker_edit = None,
            "enter" => self.commit_marker_edit(cx),
            "backspace" => {
                if let Some(edit) = self.marker_edit.as_mut() {
                    edit.backspace();
                }
            }
            "a" if ctrl && !alt => {
                if let Some(edit) = self.marker_edit.as_mut() {
                    edit.all = true;
                }
            }
            _ => {
                if !ctrl
                    && !alt
                    && let Some(edit) = self.marker_edit.as_mut()
                    && let Some(chars) = key_char
                {
                    for c in chars.chars() {
                        edit.typed(c);
                    }
                }
            }
        }
        cx.notify();
    }

    /// The markers card's own keys while it is up and nothing is being typed: up/down pick
    /// a row, enter retypes the name and shift+enter the note. `true` once the card is up:
    /// it owns the strokes it does not answer, like every card.
    pub(crate) fn markers_card_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        if !self.markers_open {
            return false;
        }
        match key {
            "escape" => self.markers_open = false,
            "down" => self.step_marker_pick(1, cx),
            "up" => self.step_marker_pick(-1, cx),
            "enter" => {
                let field = if shift { MarkerField::Note } else { MarkerField::Name };
                self.begin_marker_edit(self.marker_sel, field, cx);
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::project::MarkerColor;

    fn at(frame: u32) -> Marker {
        Marker { frame, length: 0, name: "m".into(), note: String::new(), color: MarkerColor::Blue }
    }

    #[test]
    fn the_jump_goes_strictly_past_the_playhead_either_way() {
        let list = [at(10), at(40), at(40), at(90)];
        assert_eq!(marker_after(&list, 0, true), Some(10));
        assert_eq!(marker_after(&list, 10, true), Some(40), "a marker on the playhead is passed");
        assert_eq!(marker_after(&list, 40, true), Some(90));
        assert_eq!(marker_after(&list, 90, true), None);
        assert_eq!(marker_after(&list, 90, false), Some(40));
        assert_eq!(marker_after(&list, 40, false), Some(10));
        assert_eq!(marker_after(&list, 10, false), None);
        assert_eq!(marker_after(&[], 5, true), None);
    }

    #[test]
    fn a_dropped_marker_takes_the_lowest_free_number() {
        assert_eq!(default_marker_name([]), "Marker 1", "the first one is Marker 1");
        assert_eq!(default_marker_name(["Marker 1", "check colour"]), "Marker 2");
        assert_eq!(default_marker_name(["check colour", "Marker 2"]), "Marker 1", "a free number is reused");
        assert_eq!(default_marker_name(["Marker 1", "Marker 2", "Marker 3"]), "Marker 4");
    }
}
