//! The one text-field behaviour the in-place renames share (sequence tabs, track heads): a
//! name opens fully selected, so typing replaces it; Ctrl+A selects it all again; backspace
//! on a selection clears it; enter saves and escape gives up are the caller's.

/// A name being retyped: its text and whether all of it is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextField {
    pub(crate) text: String,
    pub(crate) all: bool,
}

/// What a key did to the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKey {
    Edited,
    Commit,
    Cancel,
}

impl TextField {
    /// A field opened on `text` with all of it selected (the convention for a rename).
    pub(crate) fn selected(text: impl Into<String>) -> Self {
        Self { text: text.into(), all: true }
    }

    /// A field a click focused: the text stays as it is and the caret sits
    /// where the click put it (at the end -- a name has no caret to place
    /// mid-text), so the next key edits and nothing is replaced.
    #[allow(dead_code)]
    pub(crate) fn clicked(text: impl Into<String>) -> Self {
        Self { text: text.into(), all: false }
    }

    /// A field Tab focused: all of it selected, as in every desktop form.
    #[allow(dead_code)]
    pub(crate) fn tabbed(text: impl Into<String>) -> Self {
        Self::selected(text)
    }

    /// Applies one key; `max` caps the characters.
    pub(crate) fn key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, max: usize) -> FieldKey {
        match key {
            "escape" => return FieldKey::Cancel,
            "enter" => return FieldKey::Commit,
            "a" if ctrl && !alt => self.all = true,
            "backspace" => {
                if self.all {
                    self.text.clear();
                    self.all = false;
                } else {
                    self.text.pop();
                }
            }
            _ => {
                if !ctrl && !alt && let Some(chars) = key_char {
                    for c in chars.chars().filter(|c| !c.is_control()) {
                        if self.all {
                            self.text.clear();
                            self.all = false;
                        }
                        if self.text.chars().count() < max {
                            self.text.push(c);
                        }
                    }
                }
            }
        }
        FieldKey::Edited
    }
}
