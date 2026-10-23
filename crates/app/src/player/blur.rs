//! Leaving a text field: a click on anything that is not text, or a chord that is not a
//! text-editing one, takes the keyboard back to the room instead of letting the field keep it.

use crate::*;

/// Whether a stroke belongs to the app rather than to a field being typed into: any Ctrl or Alt
/// chord except the editing ones (Ctrl+A, C, V, X, Z on their own).
pub(crate) fn is_app_chord(key: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
    if !ctrl && !alt {
        return false;
    }
    !(ctrl && !alt && !shift && matches!(key, "a" | "c" | "v" | "x" | "z"))
}

/// Where a stroke goes while a text field is open: into the field, or out of it to the room's
/// keymap. Every stroke is the field's except an app chord (Ctrl+K, Alt+T, Ctrl+Y ...), and an
/// editing chord (Ctrl+A, C, V, X, Z) is the field's too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Field,
    Room,
}

/// The dispatch rule for an open field: one place, so every field (current and future) takes its
/// keys the same way. A field that is not open never takes a stroke.
pub(crate) fn route(field_open: bool, key: &str, ctrl: bool, shift: bool, alt: bool) -> Route {
    match field_open && !is_app_chord(key, ctrl, shift, alt) {
        true => Route::Field,
        false => Route::Room,
    }
}

/// Escape in a search box: clears the words when there are some (the box stays open for the next
/// words), and leaves the box when it is empty. Never passed on.
pub(crate) fn search_escape_clears(text: &str) -> bool {
    !text.is_empty()
}

impl Player {
    /// Whether any text field (a rename, a path, a filter, a hex, a number) is taking keys.
    pub(crate) fn any_text_field(&self) -> bool {
        (self.export_open && self.export_sheet.field.is_some())
            || self.cue_edit.is_some()
            || self.cue_num.is_some()
            || self.still_num.is_some()
            || self.transition_edit.is_some()
            || self.marker_edit.is_some()
            || self.seq_edit.is_some()
            || self.title_draft.is_some()
            || self.title_hex.is_some()
            || self.gen_hex.is_some()
            || self.accent_hex.is_some()
            || self.lane_edit.is_some()
            || self.time_edit.is_some()
            || self.look_name.is_some()
            || self.bin_edit.is_some()
            || crate::ui::cards::numedit::typing()
            || self.tools.as_ref().is_some_and(|t| t.typing)
            || self.dock_filter_edit
            || self.transcript.search_edit
    }

    /// The one door every open text field takes the keyboard through, ahead of the palette, the
    /// chords, the cards and the keymap. `true` when a field took the stroke; `false` when no
    /// field is open or an app chord left the field (the field is closed and the chord runs).
    pub(crate) fn text_field_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        let (ctrl, shift, alt) = (event.keystroke.modifiers.control, event.keystroke.modifiers.shift, event.keystroke.modifiers.alt);
        let key_char = event.keystroke.key_char.as_deref();
        if route(self.any_text_field(), key, ctrl, shift, alt) == Route::Room {
            if self.any_text_field() {
                self.blur_text_fields(window, cx);
            }
            return false;
        }
        if self.export_open && self.export_sheet.field.is_some() {
            self.export_field_key(key, ctrl, alt, key_char, cx);
        } else if self.cue_edit.is_some() {
            self.cue_edit_key(key, ctrl, shift, alt, key_char, cx);
        } else if self.time_edit.is_some() {
            self.time_edit_key(key, ctrl, alt, key_char, cx);
        } else if self.look_name.is_some() {
            self.look_name_key(key, ctrl, alt, key_char, cx);
        } else if self.transition_edit.is_some() {
            self.transition_key(key, cx);
        } else if self.cue_num.is_some() {
            self.cue_num_key(key, ctrl, alt, key_char, cx);
        } else if self.still_num.is_some() {
            self.still_num_key(key, ctrl, alt, key_char, cx);
        } else if self.marker_edit.is_some() {
            self.marker_edit_key(key, ctrl, alt, key_char, cx);
        } else if self.seq_edit.is_some() {
            self.seq_edit_key(key, ctrl, alt, key_char, cx);
        } else if self.title_draft.is_some() || self.title_hex.is_some() {
            self.title_key(key, ctrl, shift, alt, key_char, cx);
        } else if self.gen_hex.is_some() {
            self.gen_key(key, ctrl, alt, key_char, cx);
        } else if self.accent_hex.is_some() {
            self.accent_key(key, ctrl, alt, key_char, cx);
        } else if self.lane_edit.is_some() {
            self.lane_edit_key(key, ctrl, alt, key_char, cx);
        } else if self.bin_edit.is_some() {
            self.bin_edit_key(key, ctrl, alt, key_char, cx);
        } else if crate::ui::cards::numedit::typing() {
            crate::ui::cards::numedit::key(self, key, ctrl, alt, key_char, cx);
        } else if self.tools.as_ref().is_some_and(|t| t.typing) {
            self.tools_type_key(key, ctrl, alt, key_char, cx);
        } else if self.dock_filter_edit {
            self.dock_filter_key(key, ctrl, alt, key_char, window, cx);
        } else if self.transcript.search_edit {
            self.transcript_key(key, ctrl, shift, alt, true, cx);
        }
        cx.notify();
        true
    }

    /// The Sources search box: letters narrow the list, Escape clears the words and then leaves
    /// the box, and no stroke is passed on.
    fn dock_filter_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        key_char: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key {
            "escape" if search_escape_clears(&self.dock_filter) => self.dock_filter.clear(),
            "escape" | "enter" => {
                self.dock_filter_edit = false;
                window.focus(&self.focus);
            }
            "backspace" => {
                self.dock_filter.pop();
            }
            _ => {
                if let Some(chars) = key_char.filter(|_| !ctrl && !alt) {
                    self.dock_filter.extend(chars.chars().filter(|c| !c.is_control()));
                }
            }
        }
        cx.notify();
    }

    /// Takes every open field's text as it stands and gives the keyboard back to the room. An
    /// export name or folder that is refused stays open with its note: the typed text is never
    /// dropped, and the export is blocked until it is fixed (see `commit_pending_field`).
    pub(crate) fn blur_text_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.export_sheet.field.is_some() {
            self.commit_field();
        }
        if self.cue_edit.is_some() {
            self.commit_cue_edit(cx);
            self.cue_edit = None;
        }
        if self.marker_edit.is_some() {
            self.marker_edit_key("enter", false, false, None, cx);
            self.marker_edit = None;
        }
        if self.seq_edit.is_some() {
            self.seq_edit_key("enter", false, false, None, cx);
            self.seq_edit = None;
        }
        // A click away commits the words, as enter does: they are written as one undo step.
        if self.title_draft.is_some() || self.title_hex.is_some() {
            self.commit_title_draft(cx);
            self.title_hex = None;
        }
        if self.gen_hex.is_some() {
            self.gen_key("enter", false, false, None, cx);
            self.gen_hex = None;
        }
        self.commit_accent_hex(cx);
        if self.lane_edit.is_some() {
            self.lane_edit_key("enter", false, false, None, cx);
            self.lane_edit = None;
        }
        if self.bin_edit.is_some() {
            self.bin_edit_key("enter", false, false, None, cx);
            self.bin_edit = None;
        }
        // Go to time is cancelled by a click elsewhere: the click then does its own job (a
        // timeline click seeks), and nothing is typed into a time that was not entered.
        self.time_edit = None;
        // The look name stays open: its Save and Cancel buttons sit under the card, and a click on
        // Save must still find the name there.
        if let Some(t) = self.tools.as_mut() {
            t.typing = false;
        }
        self.dock_filter_edit = false;
        self.transcript.search_edit = false;
        window.focus(&self.focus);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_field_takes_every_stroke_but_an_app_chord() {
        // Letters, the arrows, Escape, Backspace and the editing chords are the field's.
        for key in ["f", "l", "backspace", "delete", "left", "home", "escape", "enter"] {
            assert_eq!(route(true, key, false, false, false), Route::Field, "{key}");
        }
        for key in ["a", "c", "v", "x", "z"] {
            assert_eq!(route(true, key, true, false, false), Route::Field, "ctrl+{key}");
        }
        // An app chord leaves the field and runs.
        assert_eq!(route(true, "k", true, false, false), Route::Room);
        assert_eq!(route(true, "t", false, false, true), Route::Room);
        assert_eq!(route(true, "y", true, false, false), Route::Room);
        // No open field: nothing is taken, so the room's keymap sees every stroke.
        assert_eq!(route(false, "f", false, false, false), Route::Room);
        assert_eq!(route(false, "a", true, false, false), Route::Room);
    }

    #[test]
    fn escape_in_a_search_box_clears_words_first_and_only_then_leaves() {
        assert!(search_escape_clears("fractal"));
        assert!(!search_escape_clears(""));
    }

    #[test]
    fn only_text_editing_chords_stay_with_a_field() {
        assert!(!is_app_chord("k", false, false, false));
        assert!(is_app_chord("k", true, false, false));
        assert!(is_app_chord("t", false, false, true));
        assert!(is_app_chord("z", true, true, false));
        assert!(is_app_chord("y", true, false, false));
        for k in ["a", "c", "v", "x", "z"] {
            assert!(!is_app_chord(k, true, false, false));
        }
    }
}
