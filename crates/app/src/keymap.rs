//! The keymap: which stroke means which action, and the file it survives in.
//!
//! ```text
//! edith-keys 1
//! play space
//! delete x
//! delete delete
//! save ctrl+s
//! ```
//!
//! One line per binding, so an action bound to two strokes is two lines and the
//! format needs no list syntax. A chord is `key` or `ctrl+key`, where `key` is
//! exactly what gpui reports for the stroke (`escape`, not `esc`) -- the lookup
//! compares against that string and nothing translates in between. What a
//! *person* reads is [`Keymap::display`]'s business, and only there.
//!
//! The parser is strict and everything it cannot use names its 1-based line,
//! like the project file. Only the first line refuses the whole file: a binding
//! line the parser cannot use is *dropped*, named in the notice, and every other
//! line still stands -- one unusable line is not a reason to throw away the
//! rebinds around it. A missing file is not corrupt either -- it means nobody
//! has changed a key yet, and the defaults stand.
//!
//! Writing mirrors `engine::edith::save`: `.part`, fsync, rename, directory
//! fsync, so an interrupted save cannot cost the bindings that were already
//! there.

// The UI consumes this module in a later step; until then nothing calls it and
// every item here reads as dead.
#![allow(dead_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

/// First line, exactly. The version is part of it, as in the project file.
const MAGIC: &str = "edith-keys 1";

/// Everything a stroke can ask for. Not the mouse's actions: only what a key
/// can reach is bindable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionId {
    Play,
    StepBack,
    StepForward,
    JumpBack,
    JumpForward,
    GoStart,
    GoEnd,
    Export,
    Save,
    Copy,
    Paste,
    Cut,
    Regroup,
    Detach,
    Group,
    Select,
    SelectNext,
    SelectPrev,
    Delete,
    Lift,
    Color,
    Fit,
    Resolution,
    Undo,
    AddVideoLane,
    AddAudioLane,
    RemoveVideoLane,
    RemoveAudioLane,
    ToggleMute,
    VolumeUp,
    VolumeDown,
    Equalizer,
    Speed,
    Silence,
    CancelExport,
}

impl ActionId {
    /// Display order everywhere -- the editor lists them in it and
    /// [`Keymap::defaults`] binds them in it.
    pub const ALL: [ActionId; 35] = [
        ActionId::Play,
        ActionId::StepBack,
        ActionId::StepForward,
        ActionId::JumpBack,
        ActionId::JumpForward,
        ActionId::GoStart,
        ActionId::GoEnd,
        ActionId::Export,
        ActionId::Save,
        ActionId::Copy,
        ActionId::Paste,
        ActionId::Cut,
        ActionId::Regroup,
        ActionId::Detach,
        ActionId::Group,
        ActionId::Select,
        ActionId::SelectNext,
        ActionId::SelectPrev,
        ActionId::Delete,
        ActionId::Lift,
        ActionId::Color,
        ActionId::Fit,
        ActionId::Resolution,
        ActionId::Undo,
        ActionId::AddVideoLane,
        ActionId::AddAudioLane,
        ActionId::RemoveVideoLane,
        ActionId::RemoveAudioLane,
        ActionId::ToggleMute,
        ActionId::VolumeUp,
        ActionId::VolumeDown,
        ActionId::Equalizer,
        ActionId::Speed,
        ActionId::Silence,
        ActionId::CancelExport,
    ];

    /// What a person calls it.
    pub fn label(self) -> &'static str {
        match self {
            ActionId::Play => "Play / Pause",
            ActionId::StepBack => "One frame back",
            ActionId::StepForward => "One frame forward",
            ActionId::JumpBack => "One second back",
            ActionId::JumpForward => "One second forward",
            ActionId::GoStart => "Go to the start",
            ActionId::GoEnd => "Go to the last frame",
            ActionId::Export => "Export",
            ActionId::Save => "Save",
            ActionId::Copy => "Copy",
            ActionId::Paste => "Paste",
            ActionId::Cut => "Cut",
            ActionId::Regroup => "Regroup",
            ActionId::Detach => "Detach the sound from the picture",
            ActionId::Group => "Group with the clip on another track",
            ActionId::Select => "Select the clip under the playhead (again for the next lane)",
            ActionId::SelectNext => "Select the next clip in the lane",
            ActionId::SelectPrev => "Select the previous clip in the lane",
            ActionId::Delete => "Delete",
            ActionId::Lift => "Lift (leave a gap)",
            ActionId::Color => "Colour…",
            ActionId::Fit => "Fit policy: fit → fill → stretch → centre",
            ActionId::Resolution => "Project resolution: source → 2160p → 1080p → 720p → 480p",
            ActionId::Undo => "Undo",
            ActionId::AddVideoLane => "Add a video track",
            ActionId::AddAudioLane => "Add an audio track",
            ActionId::RemoveVideoLane => "Remove the last video track (it must be empty)",
            ActionId::RemoveAudioLane => "Remove the last audio track (it must be empty)",
            ActionId::ToggleMute => "Mute / Unmute",
            ActionId::VolumeUp => "Volume up",
            ActionId::VolumeDown => "Volume down",
            ActionId::Equalizer => "Equalizer",
            ActionId::Speed => "Speed (tape)…",
            ActionId::Silence => "Silences: cut or speed up…",
            ActionId::CancelExport => "Cancel export",
        }
    }

    /// What the file calls it: ASCII, no spaces, one spelling per variant and
    /// never the label, which is free to be reworded.
    fn name(self) -> &'static str {
        match self {
            ActionId::Play => "play",
            ActionId::StepBack => "step-back",
            ActionId::StepForward => "step-forward",
            ActionId::JumpBack => "jump-back",
            ActionId::JumpForward => "jump-forward",
            ActionId::GoStart => "go-start",
            ActionId::GoEnd => "go-end",
            ActionId::Export => "export",
            ActionId::Save => "save",
            ActionId::Copy => "copy",
            ActionId::Paste => "paste",
            ActionId::Cut => "cut",
            ActionId::Regroup => "regroup",
            ActionId::Detach => "detach",
            ActionId::Group => "group",
            ActionId::Select => "select",
            ActionId::SelectNext => "select-next",
            ActionId::SelectPrev => "select-prev",
            ActionId::Delete => "delete",
            ActionId::Lift => "lift",
            ActionId::Color => "color",
            ActionId::Fit => "fit",
            ActionId::Resolution => "resolution",
            ActionId::Undo => "undo",
            ActionId::AddVideoLane => "add-video-lane",
            ActionId::AddAudioLane => "add-audio-lane",
            ActionId::RemoveVideoLane => "remove-video-lane",
            ActionId::RemoveAudioLane => "remove-audio-lane",
            ActionId::ToggleMute => "toggle-mute",
            ActionId::VolumeUp => "volume-up",
            ActionId::VolumeDown => "volume-down",
            ActionId::Equalizer => "equalizer",
            ActionId::Speed => "speed",
            ActionId::Silence => "silence",
            ActionId::CancelExport => "cancel-export",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Which heading the keys menu files it under. Every action has one, so a
    /// new action is listed the moment it exists -- there is no second list to
    /// forget.
    pub fn category(self) -> Category {
        match self {
            ActionId::Play
            | ActionId::StepBack
            | ActionId::StepForward
            | ActionId::JumpBack
            | ActionId::JumpForward
            | ActionId::GoStart
            | ActionId::GoEnd => Category::Playback,
            ActionId::Copy
            | ActionId::Paste
            | ActionId::Select
            | ActionId::SelectNext
            | ActionId::SelectPrev
            | ActionId::Delete
            | ActionId::Lift
            | ActionId::Color
            | ActionId::Fit
            // A rate is the clip's, not the sound's: it re-times the picture
            // and the sound together, and the card opens on whichever half was
            // clicked.
            | ActionId::Speed
            // The scan reads a clip's sound, but what it does is edit the
            // timeline the clip is on -- it is a clip card like the three
            // above it, opened on whichever half was picked.
            | ActionId::Silence => Category::Clips,
            // The project's own picture size is not a clip's business, and not
            // a file operation either: it is what the viewer is looking at.
            ActionId::Resolution => Category::View,
            ActionId::Cut
            | ActionId::Regroup
            | ActionId::Detach
            | ActionId::Group
            | ActionId::Undo
            | ActionId::AddVideoLane
            | ActionId::AddAudioLane
            | ActionId::RemoveVideoLane
            | ActionId::RemoveAudioLane => Category::Editing,
            ActionId::ToggleMute
            | ActionId::VolumeUp
            | ActionId::VolumeDown
            | ActionId::Equalizer => Category::Audio,
            ActionId::Save | ActionId::Export | ActionId::CancelExport => Category::File,
        }
    }
}

/// A heading in the keys menu, and the order the headings come in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Playback,
    Editing,
    Clips,
    Audio,
    File,
    View,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Playback,
        Category::Editing,
        Category::Clips,
        Category::Audio,
        Category::File,
        Category::View,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Playback => "Playback",
            Category::Editing => "Editing",
            Category::Clips => "Clips",
            Category::Audio => "Audio",
            Category::File => "File",
            Category::View => "View",
        }
    }
}

/// A stroke that works but nobody may rebind: the modal cards read the keyboard
/// themselves, and what closes a card must not be a thing a user can take away.
/// Listed here anyway, because the keys menu shows every stroke that works --
/// `no_stroke_is_missing_from_the_keys_menu` reads the key handler's own source
/// and fails on any key that reaches neither this table nor a binding.
pub struct Fixed {
    /// As a person reads it, which for a single key is [`Chord::pretty`]'s
    /// spelling and for a family of keys is the family (`0–9`).
    pub chord: &'static str,
    pub label: &'static str,
    pub category: Category,
}

pub const FIXED: [Fixed; 15] = [
    // Not a chord at all but a way of pressing one, and the only place the
    // editor can say so: holding a key that moves a *value* runs it, and
    // holding anything else still does what one press did.
    Fixed {
        chord: "hold ← → ↑ ↓",
        label: "Run a card's slider, or the volume keys, while held",
        category: Category::View,
    },
    Fixed {
        chord: "esc",
        label: "Close this card or menu, or cancel a capture",
        category: Category::View,
    },
    Fixed {
        chord: "0–9",
        label: "Type a custom export bitrate",
        category: Category::File,
    },
    Fixed {
        chord: "m / a / w / f",
        label: "Pick the export format: MP4, AV1, WAV or FLAC",
        category: Category::File,
    },
    Fixed {
        chord: "enter",
        label: "Start the export the card is set to",
        category: Category::File,
    },
    Fixed {
        chord: "backspace",
        label: "Erase a bitrate digit",
        category: Category::File,
    },
    // The equalizer card's own input, for the same reason the export card has
    // its own: a band nothing but a drag can reach is a band half the users of
    // this editor cannot move at all. Card-local, so none of them is bindable.
    Fixed {
        chord: "1–5",
        label: "Pick an equalizer band",
        category: Category::Audio,
    },
    Fixed {
        chord: "up",
        label: "Raise the picked band 1 dB",
        category: Category::Audio,
    },
    Fixed {
        chord: "down",
        label: "Lower the picked band 1 dB",
        category: Category::Audio,
    },
    Fixed {
        chord: "r",
        label: "Flatten every band",
        category: Category::Audio,
    },
    // The colour card's own three, which mean nothing outside it -- the same
    // card-local input the export card's digits are.
    Fixed {
        chord: "↑ / ↓",
        label: "Pick a colour slider",
        category: Category::Clips,
    },
    Fixed {
        chord: "← / →",
        label: "Move the picked colour slider",
        category: Category::Clips,
    },
    Fixed {
        chord: "r",
        label: "Take the colour grade off the clip",
        category: Category::Clips,
    },
    // The silence card's two apply keys. Card-local like every stroke above --
    // they mean nothing while it is closed -- but the card is the one place in
    // this editor where a key rewrites forty places at once, so both of them
    // are listed rather than hidden in the card's own hint line.
    Fixed {
        chord: "enter",
        label: "Cut every silence the card found",
        category: Category::Clips,
    },
    Fixed {
        chord: "f",
        label: "Speed the silences up instead of cutting them",
        category: Category::Clips,
    },
];

/// A stroke as the key handler sees it: gpui's key name plus the one modifier
/// this editor binds. Nothing else is a chord here -- shift and alt are part of
/// the key name gpui reports, and a second modifier would need the file format
/// to grow before it could be spelled.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: String,
    pub ctrl: bool,
}

impl Chord {
    /// The file's spelling, and the only one [`parse`] accepts back.
    fn text(&self) -> String {
        if self.ctrl {
            format!("ctrl+{}", self.key)
        } else {
            self.key.clone()
        }
    }

    /// The spelling a person reads. Identical to [`Chord::text`] but for the one
    /// key whose usual name is shorter than gpui's. Public because a
    /// keybindings row shows one chord, where [`Keymap::display`] shows every
    /// chord an action answers to.
    pub fn pretty(&self) -> String {
        let key = if self.key == "escape" {
            "esc"
        } else {
            &self.key
        };
        if self.ctrl {
            format!("ctrl+{key}")
        } else {
            key.to_string()
        }
    }

    /// Whether the file can carry this stroke and read it back as itself. gpui
    /// reports strokes this format cannot spell -- shift+= arrives as the key
    /// `"+"`, which is the chord grammar's own separator -- and binding one
    /// would write a line the next load has to drop. Asked before a capture is
    /// taken, so a stroke that cannot be saved is refused while the user is
    /// still looking at the row.
    pub fn bindable(&self) -> bool {
        Chord::parse(&self.text(), 0).is_ok_and(|round_trip| round_trip == *self)
    }

    /// Strict: exactly one optional `ctrl+` and then a key gpui could have
    /// reported. A spelling this cannot emit back unchanged is refused rather
    /// than normalised, because the round-trip is what makes the file editable
    /// by hand at all.
    fn parse(text: &str, n: usize) -> Result<Self, String> {
        let (ctrl, key) = match text.strip_prefix("ctrl+") {
            Some(key) => (true, key),
            None => (false, text),
        };
        if key.is_empty()
            || key.contains(' ')
            || key.contains('+')
            || key.chars().any(|c| c.is_ascii_uppercase())
        {
            return Err(format!("line {n}: {text:?} is not a chord"));
        }
        Ok(Chord {
            key: key.to_string(),
            ctrl,
        })
    }
}

/// One stroke, one action. Two strokes for the same action are two bindings.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binding {
    pub action: ActionId,
    pub chord: Chord,
}

/// Every binding in force, in display order. No chord appears twice -- that is
/// the invariant [`Keymap::rebind_action`] and [`parse`] both defend, and it is what
/// lets [`Keymap::lookup`] answer with the first match.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    /// The bindings the editor has always had, in the order the editor lists
    /// them. Delete and undo get two strokes each because both have always
    /// answered to two.
    pub fn defaults() -> Self {
        let b = |action, key: &str, ctrl| Binding {
            action,
            chord: Chord {
                key: key.to_string(),
                ctrl,
            },
        };
        Keymap {
            bindings: vec![
                b(ActionId::Play, "space", false),
                // Seeking without a pointer, on the keys every player already
                // seeks with (gpui names them "left"/"right"/"home"/"end",
                // platform.rs:872-877). The colour card reads the same arrows
                // for its sliders, but a card answers the stroke and returns
                // before the keymap is consulted at all -- so these move the
                // playhead only while no card is open.
                b(ActionId::StepBack, "left", false),
                b(ActionId::StepForward, "right", false),
                b(ActionId::JumpBack, "left", true),
                b(ActionId::JumpForward, "right", true),
                b(ActionId::GoStart, "home", false),
                b(ActionId::GoEnd, "end", false),
                b(ActionId::Export, "e", false),
                b(ActionId::Save, "s", true),
                b(ActionId::Copy, "c", true),
                b(ActionId::Paste, "v", true),
                b(ActionId::Cut, "c", false),
                // The two lane keys, on the letters they are named after: both
                // were free, and neither is next to a key that edits.
                b(ActionId::Regroup, "g", false),
                // The pair that takes a take apart and puts it back: "d" for
                // detach was free and is one press like the rest of the clip
                // keys, and grouping two clips is the "g" that rejoins a cut one
                // lane over -- the chord beside it, so the two read as one idea.
                b(ActionId::Detach, "d", false),
                b(ActionId::Group, "g", true),
                // Selection without a pointer. Tab is what a keyboard already
                // means by "move on to the next thing", and the two brackets
                // are the pair either side of it in the lane -- shift+tab is
                // the obvious partner and is unspellable here, since a chord
                // carries only ctrl and `parse` refuses an uppercase key.
                b(ActionId::Select, "tab", false),
                b(ActionId::SelectNext, "]", false),
                b(ActionId::SelectPrev, "[", false),
                b(ActionId::Delete, "x", false),
                b(ActionId::Delete, "delete", false),
                b(ActionId::Lift, "l", false),
                // The c of colour is Cut and ctrl+c is Copy, so the grade takes
                // k: free, next to nothing that edits, and one press like the
                // rest of the clip keys.
                b(ActionId::Color, "k", false),
                // The fit policy is a clip key like the grade beside it: "p" for
                // policy, free, and one press like the rest of them. The project
                // resolution is not a clip key and takes a ctrl chord, next to
                // nothing that edits and out of the way of a stray press --
                // it changes what every clip is composed onto.
                b(ActionId::Fit, "p", false),
                b(ActionId::Resolution, "r", true),
                b(ActionId::Undo, "z", false),
                b(ActionId::Undo, "z", true),
                // The unshifted initials of what they add. Both were free --
                // the copy and paste chords are the *ctrl* ones -- and a track
                // is added often enough to deserve a key that is one press.
                b(ActionId::AddVideoLane, "v", false),
                b(ActionId::AddAudioLane, "a", false),
                // ...and the pair that takes one back, on ctrl chords: removing
                // a track is not a stroke to hit by accident (the resolution key
                // is out of the way for the same reason). Ctrl+v would be the
                // matching initial and is the paste, so the video one takes the
                // key beside it.
                b(ActionId::RemoveVideoLane, "b", true),
                b(ActionId::RemoveAudioLane, "a", true),
                b(ActionId::ToggleMute, "m", false),
                // The unshifted pair, which is what gpui reports for those two
                // keys ("=" and "-", platform.rs:862): the volume keys every
                // player has, without asking for shift to make the "+".
                b(ActionId::VolumeUp, "=", false),
                b(ActionId::VolumeDown, "-", false),
                // The one free letter that says the word: "e" is already the
                // export and an equalizer is the q in nobody else's way.
                b(ActionId::Equalizer, "q", false),
                // "s" is the save's ctrl chord, so the speed card takes the
                // unshifted letter beside it: "j" was free, sits under the
                // right hand with the other clip keys (k grades, l lifts), and
                // is not next to anything that deletes.
                b(ActionId::Speed, "j", false),
                // The silence card takes "u": free, sits with the other clip-card
                // keys under the right hand, and is nowhere near the two that
                // delete (x and delete) -- what it opens is a card that can cut
                // forty places at once.
                b(ActionId::Silence, "u", false),
                b(ActionId::CancelExport, "escape", false),
            ],
        }
    }

    /// What a stroke means, if anything. `key` is gpui's key name and `ctrl` its
    /// control modifier -- the pair the key handler already matches on.
    pub fn lookup(&self, key: &str, ctrl: bool) -> Option<ActionId> {
        self.bindings
            .iter()
            .find(|b| b.chord.ctrl == ctrl && b.chord.key == key)
            .map(|b| b.action)
    }

    /// Every stroke that reaches an action, as one readable phrase: `x or
    /// delete`. An action nobody can reach says so rather than reading blank.
    pub fn display(&self, action: ActionId) -> String {
        let strokes: Vec<String> = self
            .bindings
            .iter()
            .filter(|b| b.action == action)
            .map(|b| b.chord.pretty())
            .collect();
        if strokes.is_empty() {
            "unbound".to_string()
        } else {
            strokes.join(" or ")
        }
    }

    /// Every binding, in display order.
    pub fn entries(&self) -> &[Binding] {
        &self.bindings
    }

    /// Makes `chord` the whole of what reaches `action`: its other chords go, so
    /// what the editor shows for an action is what a rebind replaced. Two
    /// strokes for one action stay expressible, but only in the file -- the
    /// parser takes as many lines per action as it is given.
    ///
    /// `Err` names the action that already holds the chord and nothing changes:
    /// two actions on one stroke would make the second unreachable, and silently
    /// stealing it is worse than refusing. A chord the action already holds is
    /// not a conflict with itself -- it collapses the action onto that one.
    pub fn rebind_action(&mut self, action: ActionId, chord: Chord) -> Result<(), ActionId> {
        if let Some(held) = self
            .bindings
            .iter()
            .find(|b| b.action != action && b.chord == chord)
        {
            return Err(held.action);
        }
        self.bindings.retain(|b| b.action != action);
        // Back into its own place in the display order rather than onto the end,
        // so the file and the editor keep reading in the order of `ALL`.
        let at = self
            .bindings
            .iter()
            .position(|b| rank(b.action) > rank(action))
            .unwrap_or(self.bindings.len());
        self.bindings.insert(at, Binding { action, chord });
        Ok(())
    }

    /// The keymap in force, and what to tell the user if the file could not be
    /// used. A file that is merely absent is the normal case and says nothing;
    /// anything else -- unreadable, or a line this parser refuses -- falls back
    /// to the defaults *and* is reported, because a keymap silently not applied
    /// is a keyboard that has quietly changed under the user.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(&Self::config_path())
    }

    /// All of [`Keymap::load`] but the path, which the config directory decides
    /// and a test must not be allowed to.
    fn load_from(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::defaults(), None),
            Err(e) => {
                return (
                    Self::defaults(),
                    Some(format!(
                        "KEYBINDINGS IGNORED — {}: {e} — the defaults are in force",
                        path.display()
                    )),
                );
            }
        };
        match parse(&text) {
            Ok((keymap, dropped)) if dropped.is_empty() => (keymap, None),
            // The lines it could use are in force; the ones it could not are
            // named, and only those are lost.
            Ok((keymap, dropped)) => (
                keymap,
                Some(format!(
                    "KEYBINDINGS PART-READ — {}, {} — those strokes are unbound",
                    path.display(),
                    dropped.join("; ")
                )),
            ),
            Err(e) => (
                Self::defaults(),
                Some(format!(
                    "KEYBINDINGS IGNORED — {}, {e} — the defaults are in force",
                    path.display()
                )),
            ),
        }
    }

    /// Writes the keymap, atomically, creating the config directory if this is
    /// the first save. Until the rename the previous bindings are still the
    /// whole truth on disk.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::config_path())
    }

    /// All of [`Keymap::save`] but the path, for the same reason as
    /// [`Keymap::load_from`].
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let mut part = path.to_path_buf().into_os_string();
        part.push(".part");
        let part = PathBuf::from(part);
        let result = std::fs::File::create(&part)
            .and_then(|mut f| {
                f.write_all(emit(self).as_bytes())?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&part, path))
            .and_then(|()| std::fs::File::open(&dir)?.sync_all());
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }

    /// Where the bindings live: the XDG config directory, or `~/.config` when
    /// the desktop has not named one.
    pub fn config_path() -> PathBuf {
        config_path_in(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

/// Where an action sits in the display order, which is the order of
/// [`ActionId::ALL`] and the order the file is written in.
fn rank(action: ActionId) -> usize {
    ActionId::ALL
        .iter()
        .position(|a| *a == action)
        .unwrap_or(ActionId::ALL.len())
}

/// The path rule, with the environment handed in so it can be checked. An
/// `XDG_CONFIG_HOME` that is empty is one the spec says to ignore; with no
/// `HOME` either there is nowhere but here, and a relative path is still better
/// than dropping the file in `/`.
fn config_path_in(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    let dir = xdg
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    dir.join("edith").join("keybindings")
}

fn emit(keymap: &Keymap) -> String {
    let mut out = String::from(MAGIC);
    out.push('\n');
    for b in &keymap.bindings {
        out.push_str(b.action.name());
        out.push(' ');
        out.push_str(&b.chord.text());
        out.push('\n');
    }
    out
}

/// The magic line is the whole of what makes this a keybindings file, so it is
/// the only thing refused outright. A *binding* line this parser cannot use is
/// dropped instead, and named in the returned warnings: one unusable line used
/// to cost every other rebind in the file, which is the user's data. What
/// survives is still exactly what emits back, so the by-hand round-trip holds
/// for every line that is kept.
fn parse(text: &str) -> Result<(Keymap, Vec<String>), String> {
    // One trailing newline terminates the last line rather than starting an
    // empty one; a second is an empty line and is refused below.
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines = body.split('\n').enumerate();
    let (_, first) = lines.next().unwrap_or((0, ""));
    if first != MAGIC {
        return Err(match first.strip_prefix("edith-keys ") {
            Some(v) => format!("line 1: unsupported version {v}"),
            None => "line 1: not a keybindings file".to_string(),
        });
    }

    let mut bindings: Vec<Binding> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (i, line) in lines {
        let n = i + 1;
        let Some((name, chord)) = line.split_once(' ') else {
            dropped.push(format!("line {n}: {line:?} is not `<action> <chord>`"));
            continue;
        };
        let Some(action) = ActionId::from_name(name) else {
            dropped.push(format!("line {n}: unknown action {name:?}"));
            continue;
        };
        let chord = match Chord::parse(chord, n) {
            Ok(chord) => chord,
            Err(e) => {
                dropped.push(e);
                continue;
            }
        };
        // One stroke, one meaning: the second line to claim it would be the
        // dead one, and which of them died would depend on line order. The
        // first line keeps the stroke, which is the order the file reads in.
        if let Some(held) = bindings.iter().find(|b| b.chord == chord) {
            dropped.push(format!(
                "line {n}: {} is already {}'s",
                chord.text(),
                held.action.name()
            ));
            continue;
        }
        bindings.push(Binding { action, chord });
    }
    Ok((Keymap { bindings }, dropped))
}

#[cfg(test)]
mod tests {
    use super::{ActionId, Chord, Keymap, config_path_in, emit, parse};

    fn chord(key: &str, ctrl: bool) -> Chord {
        Chord {
            key: key.to_string(),
            ctrl,
        }
    }

    /// A file every line of which the parser could use -- what most assertions
    /// are about. A dropped line here is a failure, not a warning.
    fn whole(text: &str) -> Keymap {
        let (keymap, dropped) = parse(text).expect("a keybindings file");
        assert!(dropped.is_empty(), "unexpected dropped lines: {dropped:?}");
        keymap
    }

    #[test]
    fn every_default_stroke_reaches_its_action() {
        let k = Keymap::defaults();
        assert_eq!(k.entries().len(), 37);
        assert_eq!(k.lookup("space", false), Some(ActionId::Play));
        // The seek keys: bare arrows a frame, ctrl arrows a second, and the two
        // ends of the timeline.
        assert_eq!(k.lookup("left", false), Some(ActionId::StepBack));
        assert_eq!(k.lookup("right", false), Some(ActionId::StepForward));
        assert_eq!(k.lookup("left", true), Some(ActionId::JumpBack));
        assert_eq!(k.lookup("right", true), Some(ActionId::JumpForward));
        assert_eq!(k.lookup("home", false), Some(ActionId::GoStart));
        assert_eq!(k.lookup("end", false), Some(ActionId::GoEnd));
        assert_eq!(k.lookup("e", false), Some(ActionId::Export));
        assert_eq!(k.lookup("s", true), Some(ActionId::Save));
        assert_eq!(k.lookup("c", true), Some(ActionId::Copy));
        assert_eq!(k.lookup("v", true), Some(ActionId::Paste));
        assert_eq!(k.lookup("c", false), Some(ActionId::Cut));
        assert_eq!(k.lookup("g", false), Some(ActionId::Regroup));
        // The take apart / put back pair beside it.
        assert_eq!(k.lookup("d", false), Some(ActionId::Detach));
        assert_eq!(k.lookup("g", true), Some(ActionId::Group));
        // The keyboard's way onto a clip, and the pair that walks the lane.
        assert_eq!(k.lookup("tab", false), Some(ActionId::Select));
        assert_eq!(k.lookup("]", false), Some(ActionId::SelectNext));
        assert_eq!(k.lookup("[", false), Some(ActionId::SelectPrev));
        assert_eq!(k.lookup("x", false), Some(ActionId::Delete));
        assert_eq!(k.lookup("delete", false), Some(ActionId::Delete));
        assert_eq!(k.lookup("l", false), Some(ActionId::Lift));
        assert_eq!(k.lookup("k", false), Some(ActionId::Color));
        assert_eq!(k.lookup("p", false), Some(ActionId::Fit));
        assert_eq!(k.lookup("r", true), Some(ActionId::Resolution));
        assert_eq!(k.lookup("z", false), Some(ActionId::Undo));
        assert_eq!(k.lookup("z", true), Some(ActionId::Undo));
        // The track keys are the bare letters; the ctrl ones stay copy/paste.
        assert_eq!(k.lookup("v", false), Some(ActionId::AddVideoLane));
        assert_eq!(k.lookup("a", false), Some(ActionId::AddAudioLane));
        // ...and the ctrl ones take a track away, out of the way of a stray
        // press. Ctrl+v is the paste, so the video one sits beside it.
        assert_eq!(k.lookup("b", true), Some(ActionId::RemoveVideoLane));
        assert_eq!(k.lookup("a", true), Some(ActionId::RemoveAudioLane));
        assert_eq!(k.lookup("m", false), Some(ActionId::ToggleMute));
        assert_eq!(k.lookup("q", false), Some(ActionId::Equalizer));
        // The volume pair is the unshifted one, so neither needs a modifier
        // and neither is the "+" gpui reports for shift+=.
        assert_eq!(k.lookup("=", false), Some(ActionId::VolumeUp));
        assert_eq!(k.lookup("-", false), Some(ActionId::VolumeDown));
        assert_eq!(k.lookup("+", false), None);
        assert_eq!(k.lookup("escape", false), Some(ActionId::CancelExport));
        // The modifier is half the chord: ctrl+e is not e.
        assert_eq!(k.lookup("e", true), None);
        assert_eq!(k.lookup("space", true), None);
        assert_eq!(k.lookup("j", false), Some(ActionId::Speed));
        assert_eq!(k.lookup("u", false), Some(ActionId::Silence));
        assert_eq!(k.lookup("j", true), None);
        // Nothing is bound twice, or lookup's first match would be a coin toss.
        for (i, a) in k.entries().iter().enumerate() {
            assert!(
                !k.entries()[i + 1..].iter().any(|b| b.chord == a.chord),
                "{:?} bound twice",
                a.chord
            );
        }
    }

    #[test]
    fn a_file_survives_the_round_trip_exactly() {
        let text = emit(&Keymap::defaults());
        assert!(text.starts_with("edith-keys 1\nplay space\n"));
        assert!(text.contains("save ctrl+s\n"));
        assert!(text.contains("cancel-export escape\n"));
        // Both directions: the file this writes is the file it reads back, and
        // a file read in comes out byte for byte as it went.
        assert_eq!(whole(&text), Keymap::defaults());
        assert_eq!(emit(&whole(&text)), text);
        let handwritten = "edith-keys 1\nundo ctrl+z\nplay space\n";
        assert_eq!(emit(&whole(handwritten)), handwritten);
        // A keymap that binds nothing is legal -- and unusable, which is the
        // user's business, not the parser's.
        assert_eq!(whole("edith-keys 1\n").entries(), &[]);
    }

    #[test]
    fn every_refusal_names_its_line() {
        // Only the first line refuses the whole file.
        let err = |text: &str| parse(text).unwrap_err();
        assert_eq!(err(""), "line 1: not a keybindings file");
        assert_eq!(err("nonsense\n"), "line 1: not a keybindings file");
        assert_eq!(err("edith-keys 2\n"), "line 1: unsupported version 2");

        // Every other kind of bad line costs that line and nothing else.
        let dropped = |text: &str| parse(text).unwrap().1;
        assert_eq!(
            dropped("edith-keys 1\nfly space\n"),
            ["line 2: unknown action \"fly\""]
        );
        assert_eq!(
            dropped("edith-keys 1\nplay space\nplayspace\n"),
            ["line 3: \"playspace\" is not `<action> <chord>`"]
        );
        // An empty line is not optional whitespace, it is a line.
        assert_eq!(
            dropped("edith-keys 1\n\n"),
            ["line 2: \"\" is not `<action> <chord>`"]
        );
        assert_eq!(
            dropped("edith-keys 1\nplay ctrl+alt+x\n"),
            ["line 2: \"ctrl+alt+x\" is not a chord"]
        );
        assert_eq!(
            dropped("edith-keys 1\nplay ctrl+\n"),
            ["line 2: \"ctrl+\" is not a chord"]
        );
        // gpui reports lowercase key names, so an uppercase one would simply
        // never match -- dropped rather than quietly dead.
        assert_eq!(
            dropped("edith-keys 1\nplay X\n"),
            ["line 2: \"X\" is not a chord"]
        );
        // The same stroke twice: whichever line lost would depend on order, so
        // the second one is the one that goes.
        assert_eq!(
            dropped("edith-keys 1\nplay space\ncut space\n"),
            ["line 3: space is already play's"]
        );
    }

    /// The rebind data-loss path: one line the parser cannot use must not cost
    /// the rebinds around it (a `capture` of shift+= wrote `play +`, and the
    /// next load threw the whole file away and went back to the defaults).
    #[test]
    fn one_bad_line_costs_only_that_line() {
        let text = "edith-keys 1\nplay +\nexport ctrl+w\ncut j\n";
        let (keymap, dropped) = parse(text).expect("the magic line is still good");
        assert_eq!(dropped, ["line 2: \"+\" is not a chord"]);
        // The lines around it are in force, which is the whole point.
        assert_eq!(keymap.lookup("w", true), Some(ActionId::Export));
        assert_eq!(keymap.lookup("j", false), Some(ActionId::Cut));
        // ...and the one that went is unbound, not silently something else.
        assert_eq!(keymap.display(ActionId::Play), "unbound");
        // What survives still round-trips: the kept lines emit back as read.
        assert_eq!(emit(&keymap), "edith-keys 1\nexport ctrl+w\ncut j\n");

        // The notice names the line and the file, and the defaults do *not*
        // take over -- that was the data loss.
        let dir = std::env::temp_dir().join(format!("edith-keymap-drop-{}", std::process::id()));
        let path = dir.join("edith").join("keybindings");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let (read, notice) = Keymap::load_from(&path);
        assert_eq!(read, keymap);
        let notice = notice.expect("a part-read file must say so");
        assert!(notice.contains("line 2:"), "{notice}");
        assert!(notice.contains("keybindings"), "{notice}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The other half of the same bug: a stroke the file cannot spell is refused
    /// at the capture, before it can ever be written.
    #[test]
    fn only_a_stroke_the_file_can_spell_is_bindable() {
        for (key, ctrl) in [
            ("space", false),
            ("=", false),
            ("s", true),
            ("escape", false),
        ] {
            assert!(chord(key, ctrl).bindable(), "{key}");
        }
        // gpui's name for shift+=, and the grammar's own separator.
        assert!(!chord("+", false).bindable());
        assert!(!chord("+", true).bindable());
        // The rest of what `Chord::parse` will not take back.
        assert!(!chord("", false).bindable());
        assert!(!chord("X", false).bindable());
        assert!(!chord("page up", false).bindable());
        // A bindable chord is one `rebind_action` may keep: what it writes is
        // what the next load reads, for every stroke that passes here.
        let mut k = Keymap::defaults();
        k.rebind_action(ActionId::Play, chord("=", true)).unwrap();
        assert_eq!(whole(&emit(&k)), k);
    }

    #[test]
    fn rebinding_refuses_to_steal_another_actions_stroke() {
        let mut k = Keymap::defaults();
        assert_eq!(
            k.rebind_action(ActionId::Export, chord("space", false)),
            Err(ActionId::Play)
        );
        // Refused means unchanged, on both sides of the conflict.
        assert_eq!(k.lookup("e", false), Some(ActionId::Export));
        assert_eq!(k.lookup("space", false), Some(ActionId::Play));
        // A free stroke takes, and the old one stops meaning anything.
        assert_eq!(k.rebind_action(ActionId::Export, chord("w", true)), Ok(()));
        assert_eq!(k.lookup("w", true), Some(ActionId::Export));
        assert_eq!(k.lookup("e", false), None);
        assert_eq!(k.display(ActionId::Export), "ctrl+w");
    }

    #[test]
    fn a_rebound_action_keeps_only_the_new_stroke() {
        let mut k = Keymap::defaults();
        assert_eq!(k.display(ActionId::Delete), "x or delete");
        // One stroke replaces the whole set: what the row showed is what went.
        assert_eq!(k.rebind_action(ActionId::Delete, chord("d", true)), Ok(()));
        assert_eq!(k.display(ActionId::Delete), "ctrl+d");
        assert_eq!(k.lookup("x", false), None);
        assert_eq!(k.lookup("delete", false), None);
        assert_eq!(k.lookup("d", true), Some(ActionId::Delete));
        // One of its own chords is not a conflict with itself -- it collapses
        // the action onto that one.
        assert_eq!(k.rebind_action(ActionId::Undo, chord("z", true)), Ok(()));
        assert_eq!(k.display(ActionId::Undo), "ctrl+z");
        assert_eq!(k.lookup("z", false), None);
        // The action keeps its place in the display order, and the file keeps
        // reading in that order too.
        let order: Vec<_> = k.entries().iter().map(|b| b.action).collect();
        assert_eq!(order, ActionId::ALL.to_vec());
        assert_eq!(emit(&k), emit(&whole(&emit(&k))));
        // Every action is now single-bound, so the file is one line each.
        assert_eq!(k.entries().len(), ActionId::ALL.len());
    }

    #[test]
    fn display_reads_as_a_person_would_say_it() {
        let k = Keymap::defaults();
        assert_eq!(k.display(ActionId::Play), "space");
        assert_eq!(k.display(ActionId::Save), "ctrl+s");
        assert_eq!(k.display(ActionId::Delete), "x or delete");
        assert_eq!(k.display(ActionId::Undo), "z or ctrl+z");
        // The one key whose common name is not gpui's.
        assert_eq!(k.display(ActionId::CancelExport), "esc");
        assert_eq!(whole("edith-keys 1\n").display(ActionId::Cut), "unbound");
        // The label is the editor's column, and never the file's word for it.
        assert_eq!(ActionId::CancelExport.label(), "Cancel export");
        assert_eq!(ActionId::ALL.len(), 35);
    }

    #[test]
    fn a_saved_keymap_comes_back_off_the_disk() {
        // Its own directory, never the real config one: this test writes.
        let dir = std::env::temp_dir().join(format!("edith-keymap-{}", std::process::id()));
        let path = dir.join("edith").join("keybindings");
        let mut written = Keymap::defaults();
        written
            .rebind_action(ActionId::Export, chord("w", true))
            .unwrap();
        // The config directory does not exist yet -- the first save makes it.
        written.save_to(&path).unwrap();
        let (read, notice) = Keymap::load_from(&path);
        assert_eq!(notice, None);
        assert_eq!(read, written);
        assert_eq!(read.lookup("w", true), Some(ActionId::Export));
        assert_eq!(read.lookup("e", false), None);
        // The rename left nothing half-written behind.
        assert!(!dir.join("edith").join("keybindings.part").exists());
        // A file that is merely absent is not an error and says nothing.
        let (fresh, notice) = Keymap::load_from(&dir.join("nothing-here"));
        assert_eq!(fresh, Keymap::defaults());
        assert_eq!(notice, None);
        // A file that is not one at all falls back whole, and names its line.
        std::fs::write(&path, "edith-keys 9\nplay space\n").unwrap();
        let (fallback, notice) = Keymap::load_from(&path);
        assert_eq!(fallback, Keymap::defaults());
        let notice = notice.expect("a refused file must say so");
        assert!(notice.contains("line 1:"), "{notice}");
        assert!(notice.contains("keybindings"), "{notice}");
        // One unusable line inside a good file costs that line only -- the
        // defaults do not come back over the rebinds that read fine.
        std::fs::write(&path, "edith-keys 1\nplay ctrl+alt+x\nexport ctrl+w\n").unwrap();
        let (kept, notice) = Keymap::load_from(&path);
        assert_eq!(kept.lookup("w", true), Some(ActionId::Export));
        assert_eq!(kept.display(ActionId::Play), "unbound");
        let notice = notice.expect("a part-read file must say so");
        assert!(notice.contains("line 2:"), "{notice}");
        assert!(notice.contains("keybindings"), "{notice}");
        // Saving over it is a whole replacement, not an edit.
        Keymap::defaults().save_to(&path).unwrap();
        assert_eq!(Keymap::load_from(&path), (Keymap::defaults(), None));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_config_path_follows_xdg_then_home() {
        let p = |xdg: Option<&str>, home: Option<&str>| {
            config_path_in(xdg.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            p(Some("/x/config"), Some("/home/u")),
            std::path::Path::new("/x/config/edith/keybindings")
        );
        assert_eq!(
            p(None, Some("/home/u")),
            std::path::Path::new("/home/u/.config/edith/keybindings")
        );
        // An empty XDG_CONFIG_HOME is one the spec says to ignore.
        assert_eq!(
            p(Some(""), Some("/home/u")),
            std::path::Path::new("/home/u/.config/edith/keybindings")
        );
        // Nowhere to put it but here -- still never `/edith/keybindings`.
        assert_eq!(
            p(None, None),
            std::path::Path::new(".config/edith/keybindings")
        );
    }
}
