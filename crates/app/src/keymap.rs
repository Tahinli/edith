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

/// The action list, written once: the enum and the display-order array come out
/// of the same names, so an action added here is in [`ActionId::ALL`] by
/// construction. It used to be two lists, and a variant added to one of them
/// compiled, passed every test, and was simply missing from the keys card --
/// the pointer-unreachable action this editor opened with.
macro_rules! actions {
    ($($(#[$note:meta])* $variant:ident),+ $(,)?) => {
        /// Everything a stroke can ask for. Not the mouse's actions: only what a
        /// key can reach is bindable.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum ActionId {
            $($(#[$note])* $variant,)+
        }

        impl ActionId {
            /// Display order everywhere -- the editor lists them in it and
            /// [`Keymap::defaults`] binds them in it. Derived from the list
            /// above, so there is no second place to forget.
            pub const ALL: [ActionId; [$(stringify!($variant)),+].len()] =
                [$(ActionId::$variant),+];
        }
    };
}

actions! {
    Play,
    StepBack,
    StepForward,
    JumpBack,
    JumpForward,
    GoStart,
    GoEnd,
    /// The two that put the playhead on a *source* sync point -- the frames a
    /// cut may be placed on for an export to copy the film's own coded pictures
    /// instead of decoding and coding every one of them again. Playback keys by
    /// what they do (they move the playhead and nothing else) and editing keys
    /// by what they are for.
    PrevSyncPoint,
    NextSyncPoint,
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
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Undo,
    AddVideoLane,
    AddAudioLane,
    RemoveVideoLane,
    RemoveAudioLane,
    AddSubtitleTrack,
    RemoveSubtitleTrack,
    ToggleMute,
    VolumeUp,
    VolumeDown,
    Equalizer,
    Speed,
    Silence,
    Mix,
    ToggleSnap,
    ToggleSubtitles,
    /// Cut on the stand-ins or on the films themselves
    /// ([`engine::proxy`]): what the picture is decoded from, and nothing
    /// else -- the sound and every export stay the film's whichever way it
    /// is set.
    ToggleProxies,
    /// The window's own colours, which belong to the person looking at them and
    /// not to the project: a list of the palettes, opened here and from the
    /// toolbar's Theme button. It was a build feature once, which made it a
    /// choice only whoever compiled the binary could make.
    Theme,
    CancelExport,
    /// Opens the actions card -- the list every other action is on. A door of
    /// its own, because a card reachable only by a button in the panel is one a
    /// hand on the keyboard has to leave the keyboard for.
    ShowActions,
}

impl ActionId {
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
            ActionId::PrevSyncPoint => "Previous sync point (a cut here is copied, not re-encoded)",
            ActionId::NextSyncPoint => "Next sync point (a cut here is copied, not re-encoded)",
            ActionId::Export => "Export",
            ActionId::Save => "Save",
            ActionId::Copy => "Copy",
            ActionId::Paste => "Paste",
            ActionId::Cut => "Cut",
            ActionId::Regroup => "Regroup",
            // Neither half's action: it takes the *pair* apart, and the menu
            // that offers it hangs on whichever half was right-clicked -- worded
            // from the picture's side it read as a video item on an audio clip.
            ActionId::Detach => "Ungroup the picture and the sound",
            ActionId::Group => "Group with the clip on another track",
            ActionId::Select => "Select the clip under the playhead (again for the next lane)",
            ActionId::SelectNext => "Select the next clip in the lane",
            ActionId::SelectPrev => "Select the previous clip in the lane",
            ActionId::Delete => "Delete",
            ActionId::Lift => "Lift (leave a gap)",
            ActionId::Color => "Colour…",
            ActionId::Fit => "Fit policy: fit → fill → stretch → centre",
            ActionId::Resolution => "Project resolution: source → 2160p → 1080p → 720p → 480p",
            ActionId::ZoomIn => "Zoom in on the timeline (around the playhead)",
            ActionId::ZoomOut => "Zoom out of the timeline",
            ActionId::ZoomFit => "Fit the whole timeline on screen",
            ActionId::Undo => "Undo",
            ActionId::AddVideoLane => "Add a video track",
            ActionId::AddAudioLane => "Add an audio track",
            ActionId::RemoveVideoLane => "Remove the last video track (it must be empty)",
            ActionId::RemoveAudioLane => "Remove the last audio track (it must be empty)",
            ActionId::AddSubtitleTrack => "Add subtitles from a file…",
            ActionId::RemoveSubtitleTrack => "Remove the picked subtitle track",
            ActionId::ToggleMute => "Mute / Unmute",
            ActionId::VolumeUp => "Volume up",
            ActionId::VolumeDown => "Volume down",
            ActionId::Equalizer => "Equalizer",
            ActionId::Speed => "Speed (tape)…",
            ActionId::Silence => "Silences: cut or speed up…",
            ActionId::Mix => "Mix: track volumes and the limiter…",
            ActionId::ToggleSnap => "Snap on / off (edges, the playhead, the start)",
            ActionId::ToggleSubtitles => "Subtitles on / off over the picture",
            ActionId::ToggleProxies => "Proxies on / off for the picture",
            ActionId::Theme => "Theme: the window's colours…",
            ActionId::CancelExport => "Cancel export",
            ActionId::ShowActions => "All actions and their keys…",
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
            ActionId::PrevSyncPoint => "prev-sync-point",
            ActionId::NextSyncPoint => "next-sync-point",
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
            ActionId::ZoomIn => "zoom-in",
            ActionId::ZoomOut => "zoom-out",
            ActionId::ZoomFit => "zoom-fit",
            ActionId::Undo => "undo",
            ActionId::AddVideoLane => "add-video-lane",
            ActionId::AddAudioLane => "add-audio-lane",
            ActionId::RemoveVideoLane => "remove-video-lane",
            ActionId::RemoveAudioLane => "remove-audio-lane",
            ActionId::AddSubtitleTrack => "add-subtitle-track",
            ActionId::RemoveSubtitleTrack => "remove-subtitle-track",
            ActionId::ToggleMute => "toggle-mute",
            ActionId::VolumeUp => "volume-up",
            ActionId::VolumeDown => "volume-down",
            ActionId::Equalizer => "equalizer",
            ActionId::Speed => "speed",
            ActionId::Silence => "silence",
            ActionId::Mix => "mix",
            ActionId::ToggleSnap => "toggle-snap",
            ActionId::ToggleSubtitles => "toggle-subtitles",
            ActionId::ToggleProxies => "toggle-proxies",
            ActionId::Theme => "theme",
            ActionId::CancelExport => "cancel-export",
            ActionId::ShowActions => "show-actions",
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
            | ActionId::GoEnd
            | ActionId::PrevSyncPoint
            | ActionId::NextSyncPoint => Category::Playback,
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
            // Neither is how much of the timeline the panel shows: a zoom edits
            // nothing, it magnifies.
            // ...and whether the cues are drawn over that picture: a subtitle
            // edits nothing either, it is part of what is being watched.
            ActionId::Resolution
            | ActionId::ZoomIn
            | ActionId::ZoomOut
            | ActionId::ZoomFit
            | ActionId::ToggleSubtitles
            // ...and which file the picture is decoded from, which changes
            // nothing that is saved and nothing that is exported: a stand-in
            // is what one watches while cutting, not what one delivers.
            | ActionId::ToggleProxies
            // ...and what the whole window is painted in: it edits nothing at
            // all, it is what one is looking *with*.
            | ActionId::Theme => Category::View,
            ActionId::Cut
            | ActionId::Regroup
            | ActionId::Detach
            | ActionId::Group
            | ActionId::Undo
            | ActionId::AddVideoLane
            | ActionId::AddAudioLane
            | ActionId::RemoveVideoLane
            // Not a view setting despite being a pointer aid: it decides where
            // a drag actually lands, which is an edit and not a magnification.
            | ActionId::ToggleSnap
            // The subtitle pair is a track pair like the lanes above it: what
            // they add is on the timeline and goes into the file an export
            // writes, where the toggle two headings down only decides whether
            // this window draws it.
            | ActionId::AddSubtitleTrack
            | ActionId::RemoveSubtitleTrack
            | ActionId::RemoveAudioLane => Category::Editing,
            ActionId::ToggleMute
            | ActionId::VolumeUp
            | ActionId::VolumeDown
            | ActionId::Equalizer
            // The mix is the project's sound -- what every track plays at and
            // what the whole of it is held under -- where the three above it
            // are this machine's monitoring. Both are audio, and the card says
            // which is which.
            | ActionId::Mix => Category::Audio,
            // Under the heading the panel button sits in, beside the save and
            // the export it stands next to on screen.
            ActionId::Save
            | ActionId::Export
            | ActionId::CancelExport
            | ActionId::ShowActions => Category::File,
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
    /// spelling and for a family of keys is the family (`0–9`). A `String`
    /// because one of them is generated: the codec row's keys are the export
    /// card's own table, not a line typed a second time here.
    pub chord: String,
    pub label: &'static str,
    pub category: Category,
    /// What a hand with only a mouse does instead. Every stroke here is
    /// card-local, and a card whose rows only a keyboard can reach is half this
    /// editor's users locked out of it, so each row names the thing on its card
    /// -- `every_action_is_reachable_without_the_keyboard` looks each id up in
    /// the render's own source.
    pub reach: Reach,
}

/// The pointer's answer to a fixed stroke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// The id of the element on the card that does the same thing by click or
    /// by drag.
    Click(&'static str),
    /// Not an action a pointer takes: the way *out* of a card, and the hold
    /// that only repeats what a drag already does continuously. A variant
    /// rather than an omission, so a row with nothing to click is a decision
    /// somebody wrote down instead of a line missing from a list.
    Gesture,
}

/// The codec row's keys, from the export card's own table. Typed out here once,
/// it named six of the seven codecs the same row's label advertised -- OGG had
/// a key (`o`) that this card never said.
fn codec_chord() -> String {
    crate::FORMATS
        .iter()
        .map(|(_, stroke, ..)| *stroke)
        .filter(|stroke| !stroke.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

pub static FIXED: std::sync::LazyLock<[Fixed; 29]> = std::sync::LazyLock::new(|| {
    [
        // Not a chord at all but a way of pressing one, and the only place the
        // editor can say so: holding a key that moves a *value* runs it, and
        // holding anything else still does what one press did.
        Fixed {
            chord: "hold ← → ↑ ↓".into(),
            label: "Run a card's slider, or the volume and zoom keys, while held",
            category: Category::View,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "esc".into(),
            label: "Close this card or menu, or cancel a capture",
            category: Category::View,
            reach: Reach::Gesture,
        },
        // An open choice list is driven by the keyboard as well as clicked: the
        // list is the door to a setting, and a door only a pointer opens is one
        // half of this editor's users cannot use.
        Fixed {
            chord: "↑ ↓ enter".into(),
            label: "Move through an open choice list, and take the row",
            category: Category::View,
            reach: Reach::Click("picker-row"),
        },
        Fixed {
            chord: "n".into(),
            label: "Open the custom export bitrate field — ↑↓ step it",
            category: Category::File,
            reach: Reach::Click("quality"),
        },
        Fixed {
            chord: "0–9".into(),
            label: "Type into that field — nothing outside it",
            category: Category::File,
            reach: Reach::Click("mbps-up"),
        },
        Fixed {
            chord: codec_chord(),
            label: "Pick the export codec: H.264, AV1, HEVC, WAV, FLAC, MP3 or OGG",
            category: Category::File,
            reach: Reach::Click("format"),
        },
        // The rest of the export card's own rows, each on the letter its row is
        // named after. They shadow the clip keys of the same letter while the card
        // is up, exactly as its digits shadow nothing and its arrows would: a modal
        // card owns the keyboard, and cutting a clip under it is not a thing that
        // can happen anyway.
        Fixed {
            chord: "c".into(),
            label: "Switch the export container: Matroska or MP4",
            category: Category::File,
            reach: Reach::Click("container"),
        },
        Fixed {
            chord: "q".into(),
            label: "Step through the export quality rows",
            category: Category::File,
            reach: Reach::Click("quality"),
        },
        Fixed {
            chord: "b".into(),
            label: "Step through the export sound bitrates",
            category: Category::File,
            reach: Reach::Click("sound"),
        },
        Fixed {
            chord: "d".into(),
            label: "Choose where the export is written",
            category: Category::File,
            reach: Reach::Click("destination"),
        },
        Fixed {
            chord: "g".into(),
            label: "Export card: sections or one flat list",
            category: Category::File,
            reach: Reach::Click("export-layout"),
        },
        Fixed {
            chord: "r".into(),
            label: "Export card: the formats with no encoder as rows or as one line",
            category: Category::File,
            reach: Reach::Click("export-refusals"),
        },
        Fixed {
            chord: "enter".into(),
            label: "Start the export — or commit the bitrate field",
            category: Category::File,
            reach: Reach::Click("export-confirm"),
        },
        Fixed {
            chord: "backspace".into(),
            label: "Erase a digit in the bitrate field",
            category: Category::File,
            reach: Reach::Click("mbps-down"),
        },
        // The equalizer card's own input, for the same reason the export card has
        // its own: a band nothing but a drag can reach is a band half the users of
        // this editor cannot move at all. Card-local, so none of them is bindable.
        Fixed {
            chord: "1–9, 0".into(),
            label: "Pick an equalizer band",
            category: Category::Audio,
            reach: Reach::Click("eq-graph"),
        },
        Fixed {
            chord: "up".into(),
            label: "Raise the picked band 1 dB",
            category: Category::Audio,
            reach: Reach::Click("eq-gain-up"),
        },
        Fixed {
            chord: "down".into(),
            label: "Lower the picked band 1 dB",
            category: Category::Audio,
            reach: Reach::Click("eq-gain-down"),
        },
        Fixed {
            chord: "← / →".into(),
            label: "Move the picked band down or up in frequency",
            category: Category::Audio,
            reach: Reach::Click("eq-freq-up"),
        },
        Fixed {
            chord: "shift ← / →".into(),
            label: "Widen or narrow the picked band (its Q)",
            category: Category::Audio,
            reach: Reach::Click("eq-q-wider"),
        },
        Fixed {
            chord: "a".into(),
            label: "Add an equalizer band beside the picked one",
            category: Category::Audio,
            reach: Reach::Click("eq-add"),
        },
        Fixed {
            chord: "x".into(),
            label: "Remove the picked equalizer band",
            category: Category::Audio,
            reach: Reach::Click("eq-remove"),
        },
        Fixed {
            chord: "f".into(),
            label: "Flatten the picked band (double-click does the same)",
            category: Category::Audio,
            reach: Reach::Click("eq-flat-band"),
        },
        Fixed {
            chord: "r".into(),
            label: "Flatten every band",
            category: Category::Audio,
            reach: Reach::Click("eq-reset"),
        },
        Fixed {
            chord: "s".into(),
            label: "Show or hide the spectrum behind the curve",
            category: Category::Audio,
            reach: Reach::Click("eq-spectrum"),
        },
        // The colour card's own three, which mean nothing outside it -- the same
        // card-local input the export card's digits are.
        Fixed {
            chord: "↑ / ↓".into(),
            label: "Pick a colour slider",
            category: Category::Clips,
            reach: Reach::Click("color-row"),
        },
        Fixed {
            chord: "← / →".into(),
            label: "Move the picked colour slider",
            category: Category::Clips,
            reach: Reach::Click("color-bar"),
        },
        Fixed {
            chord: "r".into(),
            label: "Take the colour grade off the clip",
            category: Category::Clips,
            reach: Reach::Click("color-reset"),
        },
        // The silence card's two apply keys. Card-local like every stroke above --
        // they mean nothing while it is closed -- but the card is the one place in
        // this editor where a key rewrites forty places at once, so both of them
        // are listed rather than hidden in the card's own hint line.
        Fixed {
            chord: "enter".into(),
            label: "Cut every silence the card found",
            category: Category::Clips,
            reach: Reach::Click("silence-apply"),
        },
        Fixed {
            chord: "f".into(),
            label: "Speed the silences up instead of cutting them",
            category: Category::Clips,
            reach: Reach::Click("silence-apply"),
        },
    ]
});

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
                // The sync-point pair, on the brackets that already mean "the
                // one before / the one after" here (they select the clip either
                // side) -- with ctrl, which was free on both, because these
                // step through the same timeline by the source's own grid.
                b(ActionId::PrevSyncPoint, "[", true),
                b(ActionId::NextSyncPoint, "]", true),
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
                // The zoom pair, on the chords every editor and every browser
                // magnifies with. The bare "=" and "-" are the volume keys, so
                // these take the ctrl versions of them -- and ctrl+0 is the
                // "back to 100%" of the same family, which here is the whole
                // timeline across the bed.
                b(ActionId::ZoomIn, "=", true),
                b(ActionId::ZoomOut, "-", true),
                b(ActionId::ZoomFit, "0", true),
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
                // The subtitle pair reads the same way: the unshifted initial
                // adds one -- "s" is free, the save is the *ctrl* chord -- and a
                // ctrl chord takes it back. Ctrl+s is that save, so the removal
                // takes the other letter subtitles already answer to here: "t"
                // shows and hides them, and ctrl+t takes one off the timeline.
                b(ActionId::AddSubtitleTrack, "s", false),
                b(ActionId::RemoveSubtitleTrack, "t", true),
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
                // The mix card takes "f", for the faders on it: "m" would be
                // the word but it is the monitoring mute, which is this
                // machine's volume and not the project's -- two things one key
                // must not mean.
                b(ActionId::Mix, "f", false),
                // The snap takes "n", which is what every other editor toggles
                // it with: free here, and nowhere near the two keys that delete.
                b(ActionId::ToggleSnap, "n", false),
                // The subtitles take "t", for the word: free, one press like
                // the rest of the things one *looks* at, and nowhere near the
                // two keys that delete.
                b(ActionId::ToggleSubtitles, "t", false),
                // The stand-ins take ctrl+p, the p of the word: the unshifted
                // one is the fit policy, and a ctrl chord is right for a switch
                // thrown once for a whole session rather than one wanted under
                // a hand -- the theme takes ctrl+h for that reason.
                b(ActionId::ToggleProxies, "p", true),
                // The theme takes ctrl+h -- the h of the word, since "t" is the
                // subtitles and ctrl+t takes one off -- and a ctrl chord because
                // it is a preference set once, not a stroke wanted under a hand
                // that is editing.
                b(ActionId::Theme, "h", true),
                b(ActionId::CancelExport, "escape", false),
                // The help key, where every program's list of what it can do
                // has always been. Free -- gpui names it "f1"
                // (platform.rs:880, the keysym's own name lowercased) -- and
                // not a letter, so it takes nothing away from the clip keys.
                b(ActionId::ShowActions, "f1", false),
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
    use engine::scratch::Scratch;

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
        assert_eq!(k.entries().len(), 50);
        assert_eq!(k.lookup("p", true), Some(ActionId::ToggleProxies));
        assert_eq!(k.lookup("f1", false), Some(ActionId::ShowActions));
        assert_eq!(k.lookup("h", true), Some(ActionId::Theme));
        assert_eq!(k.lookup("space", false), Some(ActionId::Play));
        // The seek keys: bare arrows a frame, ctrl arrows a second, and the two
        // ends of the timeline.
        assert_eq!(k.lookup("left", false), Some(ActionId::StepBack));
        assert_eq!(k.lookup("right", false), Some(ActionId::StepForward));
        assert_eq!(k.lookup("left", true), Some(ActionId::JumpBack));
        assert_eq!(k.lookup("right", true), Some(ActionId::JumpForward));
        assert_eq!(k.lookup("home", false), Some(ActionId::GoStart));
        assert_eq!(k.lookup("end", false), Some(ActionId::GoEnd));
        // ...and the source's own grid, which is neither a frame nor a second:
        // the brackets bare select the clip either side, with ctrl they step
        // the sync points a cut has to land on to be copied.
        assert_eq!(k.lookup("[", true), Some(ActionId::PrevSyncPoint));
        assert_eq!(k.lookup("]", true), Some(ActionId::NextSyncPoint));
        assert_eq!(k.lookup("[", false), Some(ActionId::SelectPrev));
        assert_eq!(k.lookup("]", false), Some(ActionId::SelectNext));
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
        // The subtitle pair, the third kind of track: the initial adds and the
        // subtitle letter's chord takes it back.
        assert_eq!(k.lookup("s", false), Some(ActionId::AddSubtitleTrack));
        assert_eq!(k.lookup("t", true), Some(ActionId::RemoveSubtitleTrack));
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
        let dir = Scratch::dir("edith-keymap-drop");
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
        // Any free chord will do here; ctrl+= is the zoom's.
        k.rebind_action(ActionId::Play, chord("w", true)).unwrap();
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
        // And the refusal is a name, not a shrug: it is the holder's own label
        // the card prints back at the waiting row (`Player::capture`, "ALREADY
        // BOUND — space is Play / Pause").
        assert_eq!(ActionId::Play.label(), "Play / Pause");
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
        assert_eq!(k.display(ActionId::ToggleSnap), "n");
        assert_eq!(k.display(ActionId::ShowActions), "f1");
        assert_eq!(k.display(ActionId::ToggleSubtitles), "t");
        assert_eq!(k.display(ActionId::ZoomIn), "ctrl+=");
    }

    /// The one row of [`FIXED`] that speaks for a whole table elsewhere: it
    /// named six keys for seven codecs, and OGG's `o` was a stroke the card
    /// never mentioned. The chord is generated from `FORMATS` now, and this
    /// holds the *label* to the same table -- a codec added there with no name
    /// in this row fails here rather than in a user's hands.
    #[test]
    fn the_codec_row_says_every_codec_the_export_card_offers() {
        let row = super::FIXED
            .iter()
            .find(|f| f.label.starts_with("Pick the export codec"))
            .expect("the codec row");
        let keys: Vec<&str> = row.chord.split(" / ").collect();
        for (boxes, stroke, label, _) in crate::FORMATS {
            if stroke.is_empty() {
                // A codec this program cannot write at all has no key and no
                // business in this row.
                assert!(boxes.is_empty(), "{label} has a box but no stroke");
                assert!(!keys.contains(&stroke));
                continue;
            }
            assert!(
                keys.contains(&stroke),
                "{label}'s key {stroke:?} is not in {:?}",
                row.chord
            );
            assert!(
                row.label.contains(label),
                "{label} is not named in {:?}",
                row.label
            );
        }
        assert_eq!(
            keys.len(),
            crate::FORMATS
                .iter()
                .filter(|(_, s, ..)| !s.is_empty())
                .count()
        );
    }

    #[test]
    fn a_saved_keymap_comes_back_off_the_disk() {
        // Its own directory, never the real config one: this test writes.
        let dir = Scratch::dir("edith-keymap");
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
