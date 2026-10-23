//! The keymap: which stroke means which action, and the file it survives in.
//!
//! ```text
//! edith-keys 2
//! play space
//! delete x
//! delete delete
//! save ctrl+s
//! ```
//!
//! One line per binding, so an action bound to two strokes is two lines and the
//! format needs no list syntax. A chord is `key` with any of `ctrl+`, `shift+` and
//! `alt+` in front of it, in any order, where `key` is
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
const MAGIC: &str = "edith-keys 2";

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
    /// The angle a number key switches to (1 to 9).
    pub fn angle_number(self) -> Option<usize> {
        match self {
            ActionId::Angle1 => Some(1),
            ActionId::Angle2 => Some(2),
            ActionId::Angle3 => Some(3),
            ActionId::Angle4 => Some(4),
            ActionId::Angle5 => Some(5),
            ActionId::Angle6 => Some(6),
            ActionId::Angle7 => Some(7),
            ActionId::Angle8 => Some(8),
            ActionId::Angle9 => Some(9),
            _ => None,
        }
    }
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
    /// Whether the active session -- the timeline or a library preview --
    /// restarts from the top on reaching its end instead of stopping there.
    /// Live exactly where Play is: a transport with nothing under it has
    /// nothing to loop either.
    Loop,
    /// Plays from a short lead-in before the playhead to a short run-on after
    /// it, then puts the playhead back where it started (Premiere's Play Around).
    PlayAround,
    StepBack,
    StepForward,
    JumpBack,
    JumpForward,
    GoStart,
    GoEnd,
    /// Types a time (`1:23`, `83.5`, `00:01:23:12`, `+2s`, `-10f`) and the playhead jumps there.
    GoToTime,
    /// The two that put the playhead on a *source* sync point -- the frames a
    /// cut may be placed on for an export to copy the film's own coded pictures
    /// instead of decoding and coding every one of them again. Playback keys by
    /// what they do (they move the playhead and nothing else) and editing keys
    /// by what they are for.
    PrevSyncPoint,
    NextSyncPoint,
    /// Walks to the next cut (DESIGN.md §6): the odometer the cut readout
    /// counts against.
    WalkCutNext,
    /// The pair's other half, one cut earlier.
    WalkCutPrev,
    /// [`ActionId::WalkCutNext`]'s stride-ten (DESIGN.md §6's "shift"): its
    /// own action, not a modifier read off `WalkCutNext`, because gpui 0.2.2
    /// never delivers a shift modifier alongside `.`/`,` (see [`Chord`]'s
    /// doc) -- the *only* channel a stroke has back to [`Player::act`] is
    /// which [`ActionId`] the keymap resolved it to, so the stride the
    /// platform can't spell as a modifier is spelled as its own bound key
    /// (`>`, what shift+. actually types) instead.
    WalkCutNext10,
    /// The pair's other half (`<`, what shift+, actually types).
    WalkCutPrev10,
    /// Drops a comment marker at the playhead (`engine::project::Marker`): a name, a
    /// note and a colour, listed on the markers card. Bare `m` is the mute, so it
    /// takes the alt rung of the same letter.
    AddMarker,
    /// Drops a comment marker at the frame the ruler menu was opened on.
    MarkerHere,
    /// Renames the marker the ruler menu was opened on: the markers card opens with its name
    /// being typed ([`Player::begin_marker_edit`]).
    RenameMarker,
    /// Takes the marker the ruler menu was opened on off the list.
    DeleteMarker,
    /// Moves the playhead to the frame the ruler (or empty-track) menu was opened on.
    PlayheadHere,
    /// Walks the playhead to the next comment marker, after the playhead.
    NextMarker,
    /// The pair's other half, the marker before the playhead.
    PrevMarker,
    /// Opens the markers card: every marker with its time, name, note and colour.
    Markers,
    /// Marks the playhead as an export's in point. The out point wraps around
    /// it if the mark lands past the current out ([`ordered_range`]).
    SetIn,
    /// The out point's pair. Ordered the same way against whatever in point
    /// stands already.
    SetOut,
    /// Drops the mark: an export goes back to the whole timeline.
    ClearRange,
    /// Pre-renders the In-Out range into the render cache, which plays back as finished frames.
    RenderInOut,
    /// Pre-renders the whole timeline into the render cache.
    RenderAll,
    /// Stops the render preview running now and removes its partial file.
    CancelRender,
    /// Deletes every render-cache file.
    DeleteRenderFiles,
    /// The J/K/L shuttle (DESIGN.md §10.2). J plays backward, and again steps
    /// the rate through 2x and 4x.
    ShuttleBack,
    /// K: pauses whatever is playing.
    ShuttleStop,
    /// L: plays forward, and again steps the rate through 2x and 4x.
    ShuttleForward,
    Export,
    Save,
    /// Reloads the open project from its file on disk, as the window follows it
    /// when another writer saves it ([`crate::Player::reload_request`]). Menu and
    /// palette only: no stroke, since a key that throws away edits is one that
    /// is pressed by accident.
    ReloadProject,
    /// File > New > Project…: a small dialog (name, folder, size, rate) that starts a project
    /// the first footage then fills. Menu, palette and the welcome screen's button.
    NewProject,
    /// File > Save version: a named full copy of the project beside it, kept to go back to
    /// ([`engine::versions`]). Menu and palette only.
    SaveVersion,
    /// File > Save as template…: the settings, and optionally the clips, as a reusable template.
    SaveTemplate,
    /// File > Export captions: the timeline's captions written as an SRT or WebVTT file beside
    /// the export folder's default name ([`engine::subtitle::write_srt`]). Menu and palette only.
    ExportCaptions,
    /// File > Versions: the saved versions, and Restore for one (a single undo step).
    Versions,
    /// File > Collect media: copies every used source into one folder and relinks the project
    /// to the copies ([`engine::collect`]).
    CollectMedia,
    /// File > Project notes: a free-text note kept in the project, several lines allowed.
    ProjectNotes,
    /// File > Export timeline as > OpenTimelineIO, Final Cut Pro XML or EDL: writes the active
    /// sequence to the default export folder, named after the project ([`engine::interchange`]).
    ExportTimelineOtio,
    ExportTimelineFcpxml,
    ExportTimelineEdl,
    /// File > Import timeline (OTIO): reads an OpenTimelineIO file as a new sequence of the project.
    ImportTimelineOtio,
    Copy,
    Paste,
    /// Three-point insert (DESIGN.md §10.2): the library item being previewed --
    /// its marked in..out, or the whole of it -- goes in at the playhead, cutting
    /// the clip there and rippling the later ones on the lanes it lands on. With
    /// timeline marks set, their length is the duration. F9, as in Resolve:
    /// `,` and `.` already walk the cuts.
    InsertSource,
    /// Three-point overwrite: the same source range laid over what is at the
    /// playhead for its length, trimming or splitting it. Nothing moves. F10.
    OverwriteSource,
    Cut,
    Regroup,
    Detach,
    Group,
    Select,
    SelectNext,
    SelectPrev,
    /// Every placement on every lane in one selection -- the companion of the
    /// ctrl-click that builds a selection by hand, for the times everything
    /// wants selecting at once.
    SelectAll,
    /// Bare escape with nothing else claiming it: empties a multi-clip
    /// selection without touching picks, cards or fullscreen.
    Deselect,
    Delete,
    Lift,
    Color,
    /// Where a clip sits, how big and which way up
    /// ([`crate::transform_ui::TRANSFORM_BANDS`]) -- the geometry twin of
    /// [`ActionId::Color`], filed right beside it.
    Transform,
    /// Opens the effects card: the clip's stack of blurs, keys and masks.
    Effects,
    /// The stopwatch on the transform card's placement: a key on the playhead
    /// holding what the clip shows there, or the one that is there taken off.
    KeyPlacement,
    /// [`ActionId::KeyPlacement`] for the opacity.
    KeyOpacity,
    /// [`ActionId::KeyPlacement`] for the grade, on the colour card.
    KeyColor,
    /// Steps the easing of the key on the playhead: linear, ease in, ease out,
    /// ease in-out, hold.
    KeyEasing,
    Fit,
    Resolution,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    /// Fits the timeline view to the selected clips' span, with a small margin.
    ZoomToSelection,
    /// Every track to Small height (the Timeline menu's "All tracks" rows; a lane's own
    /// header menu sets one track, see `oracle::lane_items`).
    TrackSmall,
    /// Every track to Medium height, the height a track starts at.
    TrackMedium,
    /// Every track to Large height, for thumbnails and waveforms in full.
    TrackLarge,
    /// Every track one size shorter, from its own size.
    TracksSmaller,
    /// Every track one size taller, from its own size.
    TracksLarger,
    /// The viewer's zoom list: Fit, 25, 50, 100, 200%. View state, saved with the project's view.
    ViewZoom,
    /// The viewer's playback quality list: Full, Half, Quarter.
    PlaybackQuality,
    /// Safe-area rectangles and centre cross over the picture.
    GuideStyle,
    /// The aspect mask list: none, 9:16, 1:1, 4:5.
    GuideMask,
    Undo,
    Redo,
    /// Edit > History: the card that lists the undo steps and jumps to any of them.
    History,
    AddVideoLane,
    AddAudioLane,
    RemoveVideoLane,
    RemoveAudioLane,
    AddSubtitleLane,
    RemoveSubtitleLane,
    /// The palette's own door: a file's subtitle tracks read into the list the
    /// panel shows, which is where a lane's words are dragged *from*. Adding a
    /// track to that list places nothing -- the pair above it is what makes the
    /// timeline row a caption can land on.
    ImportSubtitles,
    /// The dock's own `Import media` verb (MOCK-SPEC "Dock" IMPORT section):
    /// the file chooser, same door [`crate::Player::pick_and_import`] and a
    /// drop on the window already use.
    AddFiles,
    /// The dock's `Paste path` verb: imports whatever file the OS clipboard
    /// names ([`crate::Player::paste_file_path`]).
    PasteFilePath,
    /// File > Open project…: asks for a `.edith` project and opens it, with the
    /// same unsaved-changes question a recent project asks ([`crate::Player::pick_open_project`]).
    /// The welcome screen's Open project button is the same door.
    OpenProject,
    ToggleMute,
    VolumeUp,
    VolumeDown,
    Equalizer,
    Speed,
    /// The speed card's ramp shapes, laid across the clip the card would open on
    /// without opening it ([`crate::audio_cards::RampPreset`]): real time easing
    /// up to double...
    SpeedRampUp,
    /// ...double easing down to real time...
    SpeedRampDown,
    /// ...slow-fast-slow, the classic speed ramp...
    SpeedRampSlowFastSlow,
    /// ...and fast-slow-fast, the hero shot.
    SpeedRampFastSlowFast,
    /// Puts a key on the clip's speed ramp at the playhead, at the speed the ramp
    /// runs there.
    SpeedRampKey,
    /// Takes the clip's speed ramp off, back to its constant speed.
    SpeedRampClear,
    Silence,
    /// The Tools card on Normalize: the mix (or each track) to a loudness target, the true peak
    /// held under a ceiling ([`engine::normalize`]).
    Normalize,
    /// The Tools card on Captions: listen to the sound, put the words on a caption track.
    Captions,
    /// The Tools card on Fillers: the ums and uhs found, checked, cut out as one step.
    Fillers,
    /// Cuts the words picked in the transcript out of every lane, closing the hole: one undo
    /// step. The transcript's own Cut button and the Delete key there do the same.
    CutSelectedWords,
    /// The Tools card on Voiceover: text spoken by piper and placed at the playhead.
    Voiceover,
    /// Timeline > Record voice-over: records from the audio input onto an audio lane at the
    /// playhead, as one undo step. Menu and palette only.
    RecordVoiceover,
    /// Paints a sliding waveform of this audio clip onto a video lane, so the
    /// sound is a picture on the same timeline — preview and export both.
    Visualizer,
    /// Listens to a sound clip for its beat and marks the beats as comment markers.
    MarkBeats,
    Reframe,
    TrackMask,
    /// Folds the selected clips (and their linked partners) into a new sequence and leaves
    /// one compound clip where they were ([`engine::Project::nest`]).
    NestClips,
    /// Puts a compound clip's contents back on the timeline ([`engine::Project::break_apart`]).
    BreakApart,
    /// Opens the selected compound clip's sequence for editing.
    OpenCompound,
    /// Back to the sequence the compound clip was opened from.
    CloseCompound,
    /// A new empty sequence, opened.
    NewSequence,
    /// A copy of the sequence being edited, opened.
    DuplicateSequence,
    /// Retypes the open sequence's name on its tab.
    RenameSequence,
    /// Deletes the open sequence (never the last, never one a compound clip plays).
    DeleteSequence,
    /// Makes a multicam clip of the media selected in the library, synced by their sound
    /// ([`engine::PlaybackSession::make_multicam_at`]).
    CreateMulticam,
    /// Replaces the multicam clip with the plain camera clips it plays.
    FlattenMulticam,
    /// Shows or hides the angle monitor beside the picture: a grid of the multicam clip's angles
    /// at the playhead, each one a button that switches the picture to it.
    ToggleAngleMonitor,
    ToggleOutputWindow,
    OutputDisplay,
    /// Switches the multicam clip under the playhead to angle 1..9 from the playhead on (a razor
    /// cut and the new angle on the right half, one undo step). Live while the angle monitor is.
    Angle1,
    Angle2,
    Angle3,
    Angle4,
    Angle5,
    Angle6,
    Angle7,
    Angle8,
    Angle9,
    /// Retypes a track's name on its head (the track's right-click menu; double-click too).
    RenameLane,
    /// Crossfades the selected audio clip into its neighbour ([`Project::crossfade`]):
    /// two adjacent clips picked on one lane, or a single pick faded into the
    /// clip right after it.
    Crossfade,
    /// No-aim trim of the subject cut's in point, frame detents, at any zoom
    /// (DESIGN.md §6), on its designed bare `[`. [`ActionId::SelectPrev`]
    /// moved to `{` (what shift+[ actually types) to free it -- see
    /// keymap.rs's own defaults.
    TrimIn,
    /// The pair's other half, the subject cut's out point, on its designed
    /// bare `]` -- [`ActionId::SelectNext`] moved to `}` (what shift+]
    /// actually types) for the same reason.
    TrimOut,
    /// Loop-trim: loops around the subject cut while trimming it (DESIGN.md
    /// §6, "the modernized Avid trim mode").
    LoopTrim,
    /// The keyboard's own version of the pointer's drag-the-edge-to-a-spot
    /// trim (debt #42: trim used to be pointer-only): snaps the subject
    /// cut's in point straight to the playhead, one press, one undo step --
    /// [`ActionId::TrimIn`]'s frame-detent nudge covers the other case. Bare
    /// `[`/`{`/ctrl+`[` are all spoken for (nudge, select-prev, sync point),
    /// so this takes the next rung of the same bracket family: ctrl held on
    /// the shifted bracket, `ctrl+{` (what ctrl+shift+[ actually types).
    TrimInToPlayhead,
    /// The pair's other half, the subject cut's out point, on `ctrl+}`.
    TrimOutToPlayhead,
    /// The ripple version of [`ActionId::TrimInToPlayhead`], on `q`: the same edge moves to the
    /// playhead, and the hole it leaves closes so the clips after it move up (Premiere's ripple trim).
    RippleTrimInToPlayhead,
    /// The ripple version of [`ActionId::TrimOutToPlayhead`], on `w`: the out point moves to the
    /// playhead and what follows moves with it.
    RippleTrimOutToPlayhead,
    /// **Roll**: moves the cut after the subject clip one frame earlier,
    /// the clip's out point and its neighbour's in point together, so the
    /// timeline keeps its length ([`Project::roll`]). The pointer's version
    /// is shift-drag on a clip edge.
    RollBack,
    /// Roll's other direction: the cut one frame later.
    RollForward,
    /// **Slip**: the subject clip shows one frame earlier of its source,
    /// keeping its place and length ([`Project::slip`]). The pointer's
    /// version is shift-drag on the clip.
    SlipBack,
    /// Slip's other direction: one frame later of the source.
    SlipForward,
    /// **Slide**: the subject clip moves one frame earlier between its
    /// neighbours, which give and take the frame ([`Project::slide`]). The
    /// pointer's version is shift+alt-drag on the clip.
    SlideBack,
    /// Slide's other direction: one frame later.
    SlideForward,
    /// Dissolves the selected video clip into its neighbour
    /// ([`Project::set_transition_out`]): two adjacent clips picked on one
    /// lane, or a single pick dissolved into the clip right after it. A
    /// second press on a clip that already dissolves removes it instead.
    Dissolve,
    /// The `Transition*` family sets how the selected video clip's join with
    /// its neighbour is drawn ([`engine::transitions::TransitionKind`]), making
    /// the join (one second) if the clip has none: the command palette's door
    /// to the picker the Clip tab shows. Unbound by default.
    TransitionDissolve,
    TransitionDipBlack,
    TransitionDipWhite,
    TransitionWipeLeft,
    TransitionWipeRight,
    TransitionWipeUp,
    TransitionWipeDown,
    TransitionSlideLeft,
    TransitionSlideRight,
    TransitionSlideUp,
    TransitionSlideDown,
    TransitionPushLeft,
    TransitionPushRight,
    TransitionPushUp,
    TransitionPushDown,
    TransitionZoom,
    /// Plays the selected clip (and its group) backward, or forward again
    /// ([`engine::PlaybackSession::set_reverse`]): picture and sound, composing
    /// with the clip's rate. Menu, palette and `ctrl+alt+r`.
    Reverse,
    /// Switches the selected clip on or off ([`engine::PlaybackSession::set_enabled`]): a
    /// switched-off clip stays on the timeline, drawn dimmed, and is skipped by picture and sound.
    ClipEnabled,
    /// Opens the colour label list for the selected clip ([`engine::project::LABELS`]).
    ClipLabel,
    /// Puts a solid colour, a gradient, a countdown, colour bars or an adjustment layer on the
    /// timeline at the playhead ([`engine::generate`]): a clip with no file behind it.
    NewSolid,
    NewGradient,
    NewCountdown,
    NewBars,
    /// A 1 kHz test tone on an audio lane (File > New > Tone).
    NewTone,
    /// Colour bars on V and a tone on A, linked (File > New > Bars and tone).
    NewBarsAndTone,
    NewAdjustment,
    NewRectangle,
    NewEllipse,
    NewLine,
    NewArrow,
    /// A title card at the playhead, opened for typing ([`Player::new_title`]).
    NewTitle,
    /// Makes a bin in the media library, named in place ([`Player::new_bin`]); the picked media go in it.
    NewBin,
    /// Makes a bin inside the bin picked in the media library (one level of nesting).
    NewBinInside,
    /// Retypes the name of the bin picked in the media library.
    RenameBin,
    /// Removes the bin picked in the media library; what it held stays, one level up.
    RemoveBin,
    /// Stars or unstars the picked media ([`Player::toggle_favourite`]).
    ToggleFavourite,
    /// Opens the list of bins to file the picked media in.
    MoveToBin,
    /// The media list shows only the starred sources.
    ShowFavourites,
    /// The media list shows every source.
    ShowAllMedia,
    Mix,
    /// One-click voice cleanup of the audio track the selection (or the mix
    /// card) names: gate, noise reduction, compressor and de-esser at medium
    /// strength, one undo step ([`engine::voice`]).
    CleanVoice,
    /// The mix card with the picked track's dynamics section unfolded.
    VoiceDynamics,
    ToggleSnap,
    ToggleSubtitles,
    /// Draws the audio clips' waveforms, or leaves them as flat bars with their names.
    ToggleWaveforms,
    ToggleScrubAudio,
    CompareOff,
    CompareSplit,
    CompareSide,
    AutoColour,
    SaveLook,
    /// Cut on the stand-ins or on the films themselves
    /// ([`engine::proxy`]): what the picture is decoded from, and nothing
    /// else -- the sound and every export stay the film's whichever way it
    /// is set.
    ToggleProxies,
    /// Whether a film that wants a stand-in gets one made as it is imported
    /// ([`engine::proxy::wanted`]): what is *encoded*, where the switch above
    /// is what is decoded. With it off the switch above is also the ask -- it
    /// starts the ones the project is missing.
    ToggleAutoProxies,
    /// The window's own colours, which belong to the person looking at them and
    /// not to the project: a list of the palettes, opened here and from the
    /// toolbar's Theme button. It was a build feature once, which made it a
    /// choice only whoever compiled the binary could make.
    Theme,
    /// The accent colour the window is painted with, as a list beside the Theme one: kept
    /// for every project like the theme, so it belongs to the person and not to the timeline.
    AccentColour,
    /// The whole window, borderless over the screen: a player's own control,
    /// on the platform's own toggle ([`gpui::Window::toggle_fullscreen`]).
    Fullscreen,
    CancelExport,
    /// Opens the actions card -- the list every other action is on. A door of
    /// its own, because a card reachable only by a button in the panel is one a
    /// hand on the keyboard has to leave the keyboard for.
    ShowActions,
    /// The shortcuts sheet (DESIGN.md §10.2's `?` and F1): swings the dock to
    /// its KEYS tab, and a second press swings it back. Its own action, not
    /// [`ActionId::ShowActions`], which is the command search's door.
    ShowKeys,
    /// The About dialog: name, version and what edith is. Opens from Help.
    About,
    /// Saves the frame on screen to a PNG: whatever is showing, the timeline's
    /// own picture or a library preview's, since both write through the one
    /// `self.image` the atlas already holds.
    Screenshot,
    /// The cue plate's font and size -- a person's own preference, kept
    /// beside the theme and not the project, and opened the same way.
    SubtitleStyle,
    /// The settings page: PROJECT rows (resolution, fps, sample-rate, HDR
    /// tonemap, the mix) beside EDITOR rows (proxies, auto-proxies, subtitle
    /// font) -- one door, two clearly-headed sections, per the split rule
    /// that "anything that changes the rendered output or lives in the
    /// project file is PROJECT; anything about this machine, this person,
    /// this window is EDITOR." Always answerable, same as `Theme`: it is
    /// about the editor and the project's own numbers, not about a clip.
    Settings,
    /// Moves the keyboard ring onto the dock (`ui::stance::Surface::Dock`,
    /// the cycle's first stop): the door into keyboard-only navigation
    /// (`ui::stance`'s Tab/Shift-Tab ring), which nothing else in the app
    /// opens. Bare Tab stays [`ActionId::Select`] at the root the whole time
    /// -- this only starts the ring, it never steals the clip-selection key.
    /// Closes the gap under the playhead (or the picked one): everything after it on the track, and on its linked take, moves up. One undo step.
    CloseGap,
    /// Holds the frame under the playhead as a still of the default length, pushing the rest later.
    Freeze,
    /// Pastes the copied clip's colour, effects, transform, speed and sound onto the selected clips.
    PasteAttributes,
    /// The copied clip's grade onto the selected clips.
    PasteColor,
    /// The copied clip's effect stack onto the selected clips.
    PasteEffects,
    /// The copied clip's placement, size and fit onto the selected clips.
    PasteTransform,
    /// The copied clip's rate, ramp and direction onto the selected clips.
    PasteSpeed,
    /// The copied clip's gain and equalizer onto the selected clips.
    PasteAudio,
    /// Opens the source of the clip under the playhead in the library preview, parked on the frame the playhead shows.
    MatchFrame,
    /// Replaces the clip under the playhead with the selected library item, keeping its place and duration.
    ReplaceEdit,
    /// Selects every clip from the playhead onward on the selected clip's track.
    SelectForward,
    /// Selects every clip from the playhead onward on every track.
    SelectForwardAll,
    /// Split edit: moves the audio edge of the take to the playhead, the picture's staying put (an L-cut after the cut, a J-cut before).
    ExtendAudio,
    /// Split edit: moves the picture edge of the take to the playhead, the sound's staying put.
    ExtendVideo,
    /// Moves the selected clips one frame earlier.
    NudgeBack,
    /// Moves the selected clips one frame later.
    NudgeForward,
    /// Moves the selected clips ten frames earlier.
    NudgeBack10,
    /// Moves the selected clips ten frames later.
    NudgeForward10,
    FocusPanels,
}

impl ActionId {
    /// The transition kind a `Transition*` action sets.
    pub fn transition_kind(self) -> Option<engine::transitions::TransitionKind> {
        use engine::transitions::{Dir, TransitionKind};
        Some(match self {
            ActionId::TransitionDissolve => TransitionKind::Dissolve,
            ActionId::TransitionDipBlack => TransitionKind::DipBlack,
            ActionId::TransitionDipWhite => TransitionKind::DipWhite,
            ActionId::TransitionWipeLeft => TransitionKind::Wipe(Dir::Left),
            ActionId::TransitionWipeRight => TransitionKind::Wipe(Dir::Right),
            ActionId::TransitionWipeUp => TransitionKind::Wipe(Dir::Up),
            ActionId::TransitionWipeDown => TransitionKind::Wipe(Dir::Down),
            ActionId::TransitionSlideLeft => TransitionKind::Slide(Dir::Left),
            ActionId::TransitionSlideRight => TransitionKind::Slide(Dir::Right),
            ActionId::TransitionSlideUp => TransitionKind::Slide(Dir::Up),
            ActionId::TransitionSlideDown => TransitionKind::Slide(Dir::Down),
            ActionId::TransitionPushLeft => TransitionKind::Push(Dir::Left),
            ActionId::TransitionPushRight => TransitionKind::Push(Dir::Right),
            ActionId::TransitionPushUp => TransitionKind::Push(Dir::Up),
            ActionId::TransitionPushDown => TransitionKind::Push(Dir::Down),
            ActionId::TransitionZoom => TransitionKind::Zoom,
            _ => return None,
        })
    }

    /// What a person calls it.
    pub fn label(self) -> &'static str {
        match self {
            ActionId::Play => "Play / Pause",
            ActionId::Loop => "Loop playback",
            ActionId::PlayAround => "Play around playhead",
            ActionId::StepBack => "One frame back",
            ActionId::StepForward => "One frame forward",
            ActionId::JumpBack => "One second back",
            ActionId::JumpForward => "One second forward",
            ActionId::GoStart => "Go to start",
            ActionId::GoEnd => "Go to end",
            ActionId::GoToTime => "Go to time\u{2026}",
            ActionId::PrevSyncPoint => "Previous sync point",
            ActionId::NextSyncPoint => "Next sync point",
            ActionId::WalkCutNext => "Next cut",
            ActionId::WalkCutPrev => "Previous cut",
            ActionId::WalkCutNext10 => "Jump 10 cuts forward",
            ActionId::WalkCutPrev10 => "Jump 10 cuts back",
            ActionId::AddMarker => "Add marker",
            ActionId::MarkerHere => "Add marker here",
            ActionId::RenameMarker => "Rename marker\u{2026}",
            ActionId::DeleteMarker => "Delete marker",
            ActionId::PlayheadHere => "Move playhead here",
            ActionId::NextMarker => "Next marker",
            ActionId::PrevMarker => "Previous marker",
            ActionId::Markers => "Markers…",
            ActionId::SetIn => "Mark in",
            ActionId::SetOut => "Mark out",
            ActionId::ClearRange => "Clear range",
            ActionId::RenderInOut => "Render in to out",
            ActionId::RenderAll => "Render whole timeline",
            ActionId::CancelRender => "Cancel render",
            ActionId::DeleteRenderFiles => "Delete render files",
            ActionId::ShuttleBack => "Shuttle back",
            ActionId::ShuttleStop => "Pause shuttle",
            ActionId::ShuttleForward => "Shuttle forward",
            ActionId::Export => "Export",
            ActionId::Save => "Save",
            ActionId::ReloadProject => "Reload project from disk",
            ActionId::NewProject => "New project…",
            ActionId::SaveVersion => "Save version…",
            ActionId::SaveTemplate => "Save as template…",
            ActionId::ExportCaptions => "Export captions…",
            ActionId::Versions => "Versions…",
            ActionId::CollectMedia => "Collect media…",
            ActionId::ProjectNotes => "Project notes…",
            ActionId::ExportTimelineOtio => "OpenTimelineIO…",
            ActionId::ExportTimelineFcpxml => "Final Cut Pro XML…",
            ActionId::ExportTimelineEdl => "EDL…",
            ActionId::ImportTimelineOtio => "Import OpenTimelineIO…",
            ActionId::Copy => "Copy",
            ActionId::Paste => "Paste",
            ActionId::InsertSource => "Insert source at playhead",
            ActionId::OverwriteSource => "Overwrite source at playhead",
            ActionId::Cut => "Split",
            ActionId::Regroup => "Join clips back",
            // Neither half's action: it takes the *pair* apart, and the menu
            // that offers it hangs on whichever half was right-clicked -- worded
            // from the picture's side it read as a video item on an audio clip.
            ActionId::Detach => "Unlink audio and video",
            ActionId::Group => "Group",
            ActionId::Select => "Select clip at playhead",
            ActionId::SelectNext => "Select next clip",
            ActionId::SelectPrev => "Select previous clip",
            ActionId::SelectAll => "Select all",
            ActionId::Deselect => "Deselect",
            ActionId::Delete => "Delete",
            ActionId::Lift => "Lift",
            ActionId::Color => "Colour…",
            ActionId::Transform => "Transform…",
            ActionId::Effects => "Effects…",
            ActionId::KeyPlacement => "Add position key here",
            ActionId::KeyOpacity => "Add opacity key here",
            ActionId::KeyColor => "Add colour key here",
            ActionId::KeyEasing => "Key easing…",
            ActionId::Fit => "Fit to frame…",
            ActionId::Resolution => "Project resolution…",
            ActionId::ZoomIn => "Zoom in",
            ActionId::ZoomOut => "Zoom out",
            ActionId::ZoomFit => "Fit timeline",
            ActionId::ZoomToSelection => "Zoom to selection",
            ActionId::TrackSmall => "All tracks: Small",
            ActionId::TrackMedium => "All tracks: Medium",
            ActionId::TrackLarge => "All tracks: Large",
            ActionId::TracksSmaller => "Make tracks smaller",
            ActionId::TracksLarger => "Make tracks larger",
            ActionId::ViewZoom => "Viewer zoom…",
            ActionId::PlaybackQuality => "Playback quality…",
            ActionId::GuideStyle => "Guides…",
            ActionId::GuideMask => "Frame mask…",
            ActionId::Undo => "Undo",
            ActionId::Redo => "Redo",
            ActionId::History => "History…",
            ActionId::AddVideoLane => "Add video track",
            ActionId::AddAudioLane => "Add audio track",
            ActionId::RemoveVideoLane => "Remove video track",
            ActionId::RemoveAudioLane => "Remove audio track",
            ActionId::AddSubtitleLane => "Add subtitle track",
            ActionId::RemoveSubtitleLane => "Remove subtitle track",
            ActionId::ImportSubtitles => "Add subtitles…",
            ActionId::AddFiles => "Import media…",
            ActionId::OpenProject => "Open project…",
            ActionId::PasteFilePath => "Paste path",
            ActionId::ToggleMute => "Mute / Unmute",
            ActionId::VolumeUp => "Volume up",
            ActionId::VolumeDown => "Volume down",
            ActionId::Equalizer => "Equalizer…",
            ActionId::Speed => "Speed…",
            ActionId::SpeedRampUp => "Speed ramp up",
            ActionId::SpeedRampDown => "Speed ramp down",
            ActionId::SpeedRampSlowFastSlow => "Speed ramp slow-fast-slow",
            ActionId::SpeedRampFastSlowFast => "Speed ramp fast-slow-fast",
            ActionId::SpeedRampKey => "Add speed key here",
            ActionId::SpeedRampClear => "Clear speed ramp",
            ActionId::Silence => "Silences…",
            ActionId::MarkBeats => "Mark the beats…",
            ActionId::Normalize => "Normalize loudness…",
            ActionId::Captions => "Generate captions",
            ActionId::Fillers => "Remove fillers…",
            ActionId::CutSelectedWords => "Cut selected words",
            ActionId::Voiceover => "Voiceover from text…",
            ActionId::RecordVoiceover => "Record voice-over…",
            ActionId::Visualizer => "Create visualizer",
            ActionId::Reframe => "Auto reframe",
            ActionId::TrackMask => "Track mask",
            ActionId::NestClips => "Nest into compound clip",
            ActionId::BreakApart => "Break apart compound clip",
            ActionId::OpenCompound => "Open compound clip",
            ActionId::CloseCompound => "Back to parent sequence",
            ActionId::NewBin => "New bin",
            ActionId::NewBinInside => "New bin inside",
            ActionId::RenameBin => "Rename bin",
            ActionId::RemoveBin => "Remove bin",
            ActionId::ToggleFavourite => "Favourite media",
            ActionId::MoveToBin => "Move media to bin…",
            ActionId::ShowFavourites => "Show favourite media",
            ActionId::ShowAllMedia => "Show all media",
            ActionId::NewSequence => "New sequence",
            ActionId::DuplicateSequence => "Duplicate sequence",
            ActionId::RenameSequence => "Rename sequence",
            ActionId::DeleteSequence => "Delete sequence",
            ActionId::CreateMulticam => "Create multicam from media…",
            ActionId::FlattenMulticam => "Flatten multicam clip",
            ActionId::ToggleAngleMonitor => "Angle monitor",
            ActionId::ToggleOutputWindow => "Video output window",
            ActionId::OutputDisplay => "Output display…",
            ActionId::Angle1 => "Switch to angle 1–9",
            ActionId::Angle2 => "Switch to angle 2",
            ActionId::Angle3 => "Switch to angle 3",
            ActionId::Angle4 => "Switch to angle 4",
            ActionId::Angle5 => "Switch to angle 5",
            ActionId::Angle6 => "Switch to angle 6",
            ActionId::Angle7 => "Switch to angle 7",
            ActionId::Angle8 => "Switch to angle 8",
            ActionId::Angle9 => "Switch to angle 9",
            ActionId::RenameLane => "Rename track",
            ActionId::Crossfade => "Crossfade audio",
            ActionId::TrimIn => "Trim in",
            ActionId::TrimOut => "Trim out",
            ActionId::LoopTrim => "Loop while trimming",
            ActionId::TrimInToPlayhead => "Trim start to playhead",
            ActionId::TrimOutToPlayhead => "Trim end to playhead",
            ActionId::RippleTrimInToPlayhead => "Ripple start to playhead",
            ActionId::RippleTrimOutToPlayhead => "Ripple end to playhead",
            ActionId::RollBack => "Move cut earlier",
            ActionId::RollForward => "Move cut later",
            ActionId::SlipBack => "Slip back",
            ActionId::SlipForward => "Slip forward",
            ActionId::SlideBack => "Slide back",
            ActionId::SlideForward => "Slide forward",
            ActionId::Dissolve => "Dissolve",
            ActionId::TransitionDissolve => "Dissolve at a cut",
            ActionId::TransitionDipBlack => "Dip to black",
            ActionId::TransitionDipWhite => "Dip to white",
            ActionId::TransitionWipeLeft => "Wipe left",
            ActionId::TransitionWipeRight => "Wipe right",
            ActionId::TransitionWipeUp => "Wipe up",
            ActionId::TransitionWipeDown => "Wipe down",
            ActionId::TransitionSlideLeft => "Slide left",
            ActionId::TransitionSlideRight => "Slide right",
            ActionId::TransitionSlideUp => "Slide up",
            ActionId::TransitionSlideDown => "Slide down",
            ActionId::TransitionPushLeft => "Push left",
            ActionId::TransitionPushRight => "Push right",
            ActionId::TransitionPushUp => "Push up",
            ActionId::TransitionPushDown => "Push down",
            ActionId::TransitionZoom => "Zoom",
            ActionId::Reverse => "Reverse clip",
            ActionId::ClipEnabled => "Enabled",
            ActionId::ClipLabel => "Label",
            ActionId::NewSolid => "Solid colour",
            ActionId::NewGradient => "Gradient",
            ActionId::NewCountdown => "Countdown",
            ActionId::NewBars => "Colour bars",
            ActionId::NewTone => "Tone",
            ActionId::NewBarsAndTone => "Bars and tone",
            ActionId::NewAdjustment => "Adjustment layer",
            ActionId::NewRectangle => "Rectangle shape",
            ActionId::NewEllipse => "Ellipse shape",
            ActionId::NewLine => "Line shape",
            ActionId::NewArrow => "Arrow shape",
            ActionId::NewTitle => "Add title",
            ActionId::Mix => "Mix…",
            ActionId::CleanVoice => "Clean voice",
            ActionId::VoiceDynamics => "Voice dynamics…",
            ActionId::ToggleSnap => "Snap",
            ActionId::ToggleSubtitles => "Subtitles",
            ActionId::ToggleWaveforms => "Audio waveforms",
            ActionId::ToggleScrubAudio => "Audio scrubbing",
            ActionId::CompareOff => "Compare: off",
            ActionId::CompareSplit => "Compare: split",
            ActionId::CompareSide => "Compare: side by side",
            ActionId::AutoColour => "Auto colour",
            ActionId::SaveLook => "Save colour as look\u{2026}",
            ActionId::ToggleProxies => "Use editing copies",
            ActionId::ToggleAutoProxies => "Editing copies on import",
            ActionId::Theme => "Theme…",
            ActionId::AccentColour => "Accent colour…",
            ActionId::Fullscreen => "Fullscreen",
            ActionId::CancelExport => "Cancel export",
            ActionId::ShowActions => "Search commands…",
            ActionId::ShowKeys => "Keyboard shortcuts",
            ActionId::About => "About edith…",
            ActionId::Screenshot => "Save frame as PNG",
            ActionId::SubtitleStyle => "Subtitle style…",
            ActionId::Settings => "Settings…",
            ActionId::CloseGap => "Close gap",
            ActionId::Freeze => "Freeze frame",
            ActionId::PasteAttributes => "Paste all attributes",
            ActionId::PasteColor => "Paste colour",
            ActionId::PasteEffects => "Paste effects",
            ActionId::PasteTransform => "Paste transform",
            ActionId::PasteSpeed => "Paste speed",
            ActionId::PasteAudio => "Paste audio",
            ActionId::MatchFrame => "Match frame",
            ActionId::ReplaceEdit => "Replace edit",
            ActionId::SelectForward => "Select track forward",
            ActionId::SelectForwardAll => "Select all tracks forward",
            ActionId::ExtendAudio => "Extend audio to playhead",
            ActionId::ExtendVideo => "Extend video to playhead",
            ActionId::NudgeBack => "Nudge back",
            ActionId::NudgeForward => "Nudge forward",
            ActionId::NudgeBack10 => "Nudge back ten frames",
            ActionId::NudgeForward10 => "Nudge forward ten frames",
            ActionId::FocusPanels => "Next panel",
        }
    }

    /// What the file calls it: ASCII, no spaces, one spelling per variant and
    /// never the label, which is free to be reworded.
    fn name(self) -> &'static str {
        match self {
            ActionId::Play => "play",
            ActionId::Loop => "loop",
            ActionId::PlayAround => "play-around",
            ActionId::StepBack => "step-back",
            ActionId::StepForward => "step-forward",
            ActionId::JumpBack => "jump-back",
            ActionId::JumpForward => "jump-forward",
            ActionId::GoStart => "go-start",
            ActionId::GoEnd => "go-end",
            ActionId::GoToTime => "go-to-time",
            ActionId::PrevSyncPoint => "prev-sync-point",
            ActionId::NextSyncPoint => "next-sync-point",
            ActionId::AddMarker => "add-marker",
            ActionId::MarkerHere => "marker-here",
            ActionId::RenameMarker => "rename-marker",
            ActionId::DeleteMarker => "delete-marker",
            ActionId::PlayheadHere => "playhead-here",
            ActionId::NextMarker => "next-marker",
            ActionId::PrevMarker => "prev-marker",
            ActionId::Markers => "markers",
            ActionId::WalkCutNext => "walk-cut-next",
            ActionId::WalkCutPrev => "walk-cut-prev",
            ActionId::WalkCutNext10 => "walk-cut-next-10",
            ActionId::WalkCutPrev10 => "walk-cut-prev-10",
            ActionId::SetIn => "set-in",
            ActionId::SetOut => "set-out",
            ActionId::ClearRange => "clear-range",
            ActionId::RenderInOut => "render-in-out",
            ActionId::RenderAll => "render-all",
            ActionId::CancelRender => "cancel-render",
            ActionId::DeleteRenderFiles => "delete-render-files",
            ActionId::ShuttleBack => "shuttle-back",
            ActionId::ShuttleStop => "shuttle-stop",
            ActionId::ShuttleForward => "shuttle-forward",
            ActionId::Export => "export",
            ActionId::Save => "save",
            ActionId::ReloadProject => "reload-project",
            ActionId::NewProject => "new-project",
            ActionId::SaveVersion => "save-version",
            ActionId::SaveTemplate => "save-template",
            ActionId::ExportCaptions => "export-captions",
            ActionId::Versions => "versions",
            ActionId::CollectMedia => "collect-media",
            ActionId::ProjectNotes => "project-notes",
            ActionId::ExportTimelineOtio => "export-timeline-otio",
            ActionId::ExportTimelineFcpxml => "export-timeline-fcpxml",
            ActionId::ExportTimelineEdl => "export-timeline-edl",
            ActionId::ImportTimelineOtio => "import-timeline-otio",
            ActionId::Copy => "copy",
            ActionId::Paste => "paste",
            ActionId::InsertSource => "insert-source",
            ActionId::OverwriteSource => "overwrite-source",
            ActionId::Cut => "cut",
            ActionId::Regroup => "regroup",
            ActionId::Detach => "detach",
            ActionId::Group => "group",
            ActionId::Select => "select",
            ActionId::SelectNext => "select-next",
            ActionId::SelectPrev => "select-prev",
            ActionId::SelectAll => "select-all",
            ActionId::Deselect => "deselect",
            ActionId::Delete => "delete",
            ActionId::Lift => "lift",
            ActionId::Color => "color",
            ActionId::Transform => "transform",
            ActionId::Effects => "effects",
            ActionId::KeyPlacement => "key-placement",
            ActionId::KeyOpacity => "key-opacity",
            ActionId::KeyColor => "key-color",
            ActionId::KeyEasing => "key-easing",
            ActionId::Fit => "fit",
            ActionId::Resolution => "resolution",
            ActionId::ZoomIn => "zoom-in",
            ActionId::ZoomOut => "zoom-out",
            ActionId::ZoomFit => "zoom-fit",
            ActionId::ZoomToSelection => "zoom-to-selection",
            ActionId::TrackSmall => "track-small",
            ActionId::TrackMedium => "track-medium",
            ActionId::TrackLarge => "track-large",
            ActionId::TracksSmaller => "tracks-smaller",
            ActionId::TracksLarger => "tracks-larger",
            ActionId::ViewZoom => "view-zoom",
            ActionId::PlaybackQuality => "playback-quality",
            ActionId::GuideStyle => "guide-style",
            ActionId::GuideMask => "guide-mask",
            ActionId::Undo => "undo",
            ActionId::Redo => "redo",
            ActionId::History => "history",
            ActionId::AddVideoLane => "add-video-lane",
            ActionId::AddAudioLane => "add-audio-lane",
            ActionId::RemoveVideoLane => "remove-video-lane",
            ActionId::RemoveAudioLane => "remove-audio-lane",
            ActionId::AddSubtitleLane => "add-subtitle-lane",
            ActionId::RemoveSubtitleLane => "remove-subtitle-lane",
            ActionId::ImportSubtitles => "import-subtitles",
            ActionId::AddFiles => "add-files",
            ActionId::OpenProject => "open-project",
            ActionId::PasteFilePath => "paste-file-path",
            ActionId::ToggleMute => "toggle-mute",
            ActionId::VolumeUp => "volume-up",
            ActionId::VolumeDown => "volume-down",
            ActionId::Equalizer => "equalizer",
            ActionId::Speed => "speed",
            ActionId::SpeedRampUp => "speed-ramp-up",
            ActionId::SpeedRampDown => "speed-ramp-down",
            ActionId::SpeedRampSlowFastSlow => "speed-ramp-slow-fast-slow",
            ActionId::SpeedRampFastSlowFast => "speed-ramp-fast-slow-fast",
            ActionId::SpeedRampKey => "speed-ramp-key",
            ActionId::SpeedRampClear => "speed-ramp-clear",
            ActionId::Silence => "silence",
            ActionId::Normalize => "normalize",
            ActionId::Captions => "generate-captions",
            ActionId::Fillers => "remove-fillers",
            ActionId::CutSelectedWords => "cut-selected-words",
            ActionId::Voiceover => "voiceover",
            ActionId::RecordVoiceover => "record-voiceover",
            ActionId::Visualizer => "visualizer",
            ActionId::MarkBeats => "mark-beats",
            ActionId::Reframe => "reframe",
            ActionId::TrackMask => "track-mask",
            ActionId::NestClips => "nest-clips",
            ActionId::BreakApart => "break-apart",
            ActionId::OpenCompound => "open-compound",
            ActionId::CloseCompound => "close-compound",
            ActionId::NewBin => "new-bin",
            ActionId::NewBinInside => "new-bin-inside",
            ActionId::RenameBin => "rename-bin",
            ActionId::RemoveBin => "remove-bin",
            ActionId::ToggleFavourite => "toggle-favourite",
            ActionId::MoveToBin => "move-to-bin",
            ActionId::ShowFavourites => "show-favourites",
            ActionId::ShowAllMedia => "show-all-media",
            ActionId::NewSequence => "new-sequence",
            ActionId::DuplicateSequence => "duplicate-sequence",
            ActionId::RenameSequence => "rename-sequence",
            ActionId::DeleteSequence => "delete-sequence",
            ActionId::CreateMulticam => "create-multicam",
            ActionId::FlattenMulticam => "flatten-multicam",
            ActionId::ToggleAngleMonitor => "toggle-angle-monitor",
            ActionId::ToggleOutputWindow => "toggle-output-window",
            ActionId::OutputDisplay => "output-display",
            ActionId::Angle1 => "angle-1",
            ActionId::Angle2 => "angle-2",
            ActionId::Angle3 => "angle-3",
            ActionId::Angle4 => "angle-4",
            ActionId::Angle5 => "angle-5",
            ActionId::Angle6 => "angle-6",
            ActionId::Angle7 => "angle-7",
            ActionId::Angle8 => "angle-8",
            ActionId::Angle9 => "angle-9",
            ActionId::RenameLane => "rename-lane",
            ActionId::Crossfade => "crossfade",
            ActionId::TrimIn => "trim-in",
            ActionId::TrimOut => "trim-out",
            ActionId::LoopTrim => "loop-trim",
            ActionId::TrimInToPlayhead => "trim-in-to-playhead",
            ActionId::TrimOutToPlayhead => "trim-out-to-playhead",
            ActionId::RippleTrimInToPlayhead => "ripple-trim-in-to-playhead",
            ActionId::RippleTrimOutToPlayhead => "ripple-trim-out-to-playhead",
            ActionId::RollBack => "roll-back",
            ActionId::RollForward => "roll-forward",
            ActionId::SlipBack => "slip-back",
            ActionId::SlipForward => "slip-forward",
            ActionId::SlideBack => "slide-back",
            ActionId::SlideForward => "slide-forward",
            ActionId::Dissolve => "dissolve",
            ActionId::TransitionDissolve => "transition-dissolve",
            ActionId::TransitionDipBlack => "transition-dip-black",
            ActionId::TransitionDipWhite => "transition-dip-white",
            ActionId::TransitionWipeLeft => "transition-wipe-left",
            ActionId::TransitionWipeRight => "transition-wipe-right",
            ActionId::TransitionWipeUp => "transition-wipe-up",
            ActionId::TransitionWipeDown => "transition-wipe-down",
            ActionId::TransitionSlideLeft => "transition-slide-left",
            ActionId::TransitionSlideRight => "transition-slide-right",
            ActionId::TransitionSlideUp => "transition-slide-up",
            ActionId::TransitionSlideDown => "transition-slide-down",
            ActionId::TransitionPushLeft => "transition-push-left",
            ActionId::TransitionPushRight => "transition-push-right",
            ActionId::TransitionPushUp => "transition-push-up",
            ActionId::TransitionPushDown => "transition-push-down",
            ActionId::TransitionZoom => "transition-zoom",
            ActionId::Reverse => "reverse",
            ActionId::ClipEnabled => "clip-enabled",
            ActionId::ClipLabel => "clip-label",
            ActionId::NewSolid => "new-solid",
            ActionId::NewGradient => "new-gradient",
            ActionId::NewCountdown => "new-countdown",
            ActionId::NewBars => "new-bars",
            ActionId::NewTone => "new-tone",
            ActionId::NewBarsAndTone => "new-bars-and-tone",
            ActionId::NewAdjustment => "new-adjustment",
            ActionId::NewRectangle => "new-rectangle",
            ActionId::NewEllipse => "new-ellipse",
            ActionId::NewLine => "new-line",
            ActionId::NewArrow => "new-arrow",
            ActionId::NewTitle => "new-title",
            ActionId::Mix => "mix",
            ActionId::CleanVoice => "clean-voice",
            ActionId::VoiceDynamics => "voice-dynamics",
            ActionId::ToggleSnap => "toggle-snap",
            ActionId::ToggleSubtitles => "toggle-subtitles",
            ActionId::ToggleWaveforms => "toggle-waveforms",
            ActionId::ToggleScrubAudio => "toggle-scrub-audio",
            ActionId::CompareOff => "compare-off",
            ActionId::CompareSplit => "compare-split",
            ActionId::CompareSide => "compare-side",
            ActionId::AutoColour => "auto-colour",
            ActionId::SaveLook => "save-look",
            ActionId::ToggleProxies => "toggle-proxies",
            ActionId::ToggleAutoProxies => "toggle-auto-proxies",
            ActionId::Theme => "theme",
            ActionId::AccentColour => "accent-colour",
            ActionId::Fullscreen => "fullscreen",
            ActionId::CancelExport => "cancel-export",
            ActionId::ShowActions => "show-actions",
            ActionId::ShowKeys => "show-keys",
            ActionId::About => "about",
            ActionId::Screenshot => "screenshot",
            ActionId::SubtitleStyle => "subtitle-style",
            ActionId::Settings => "settings",
            ActionId::CloseGap => "close-gap",
            ActionId::Freeze => "freeze",
            ActionId::PasteAttributes => "paste-attributes",
            ActionId::PasteColor => "paste-color",
            ActionId::PasteEffects => "paste-effects",
            ActionId::PasteTransform => "paste-transform",
            ActionId::PasteSpeed => "paste-speed",
            ActionId::PasteAudio => "paste-audio",
            ActionId::MatchFrame => "match-frame",
            ActionId::ReplaceEdit => "replace-edit",
            ActionId::SelectForward => "select-forward",
            ActionId::SelectForwardAll => "select-forward-all",
            ActionId::ExtendAudio => "extend-audio",
            ActionId::ExtendVideo => "extend-video",
            ActionId::NudgeBack => "nudge-back",
            ActionId::NudgeForward => "nudge-forward",
            ActionId::NudgeBack10 => "nudge-back-10",
            ActionId::NudgeForward10 => "nudge-forward-10",
            ActionId::FocusPanels => "focus-panels",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        // "cycle-key-ease" is what a keys file written before the cycle became a choice calls it.
        let name = if name == "cycle-key-ease" { "key-easing" } else { name };
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Which heading the keys menu files it under. Every action has one, so a
    /// new action is listed the moment it exists -- there is no second list to
    /// forget.
    pub fn category(self) -> Category {
        match self {
            ActionId::Play
            | ActionId::Loop
            | ActionId::PlayAround
            | ActionId::StepBack
            | ActionId::StepForward
            | ActionId::JumpBack
            | ActionId::JumpForward
            | ActionId::GoStart
            | ActionId::GoEnd
            | ActionId::GoToTime
            | ActionId::PrevSyncPoint
            | ActionId::NextSyncPoint
            | ActionId::WalkCutNext
            | ActionId::WalkCutPrev
            | ActionId::WalkCutNext10
            | ActionId::WalkCutPrev10
            | ActionId::ShuttleBack
            | ActionId::ShuttleStop
            | ActionId::PlayheadHere
            | ActionId::ShuttleForward => Category::Playback,
            // Marks and markers: the one place a range or a saved point is set and
            // listed, so the palette and the menus find them together.
            ActionId::SetIn
            | ActionId::SetOut
            | ActionId::ClearRange
            | ActionId::RenderInOut
            | ActionId::RenderAll
            | ActionId::CancelRender
            | ActionId::DeleteRenderFiles
            | ActionId::AddMarker
            | ActionId::MarkerHere
            | ActionId::RenameMarker
            | ActionId::DeleteMarker
            | ActionId::Markers
            | ActionId::NextMarker
            | ActionId::PrevMarker => Category::Marking,
            ActionId::Copy
            | ActionId::Paste
            | ActionId::Select
            | ActionId::SelectNext
            | ActionId::SelectPrev
            | ActionId::SelectAll
            | ActionId::Deselect
            | ActionId::Delete
            | ActionId::Lift
            | ActionId::Color
            | ActionId::AutoColour
            | ActionId::SaveLook
            | ActionId::Transform
            | ActionId::Effects
            | ActionId::KeyPlacement
            | ActionId::KeyOpacity
            | ActionId::KeyColor
            | ActionId::KeyEasing
            | ActionId::Fit
            // A rate is the clip's, not the sound's: it re-times the picture
            // and the sound together, and the card opens on whichever half was
            // clicked.
            | ActionId::Speed
            | ActionId::SpeedRampUp
            | ActionId::SpeedRampDown
            | ActionId::SpeedRampSlowFastSlow
            | ActionId::SpeedRampFastSlowFast
            | ActionId::SpeedRampKey
            | ActionId::SpeedRampClear
            // The scan reads a clip's sound, but what it does is edit the
            // timeline the clip is on -- it is a clip card like the three
            // above it, opened on whichever half was picked.
            | ActionId::Reframe
            | ActionId::TrackMask
            | ActionId::NestClips
            | ActionId::BreakApart
            | ActionId::CloseCompound
            | ActionId::OpenCompound
            | ActionId::CreateMulticam
            | ActionId::FlattenMulticam
            | ActionId::Angle1
            | ActionId::Angle2
            | ActionId::Angle3
            | ActionId::Angle4
            | ActionId::Angle5
            | ActionId::Angle6
            | ActionId::Angle7
            | ActionId::Angle8
            | ActionId::Angle9
            | ActionId::Crossfade
            | ActionId::TrimIn
            | ActionId::TrimOut
            | ActionId::LoopTrim
            | ActionId::TrimInToPlayhead
            | ActionId::TrimOutToPlayhead
            | ActionId::RippleTrimInToPlayhead
            | ActionId::RippleTrimOutToPlayhead
            | ActionId::RollBack
            | ActionId::RollForward
            | ActionId::SlipBack
            | ActionId::SlipForward
            | ActionId::SlideBack
            | ActionId::SlideForward
            | ActionId::Dissolve
            | ActionId::TransitionDissolve
            | ActionId::TransitionDipBlack
            | ActionId::TransitionDipWhite
            | ActionId::TransitionWipeLeft
            | ActionId::TransitionWipeRight
            | ActionId::TransitionWipeUp
            | ActionId::TransitionWipeDown
            | ActionId::TransitionSlideLeft
            | ActionId::TransitionSlideRight
            | ActionId::TransitionSlideUp
            | ActionId::TransitionSlideDown
            | ActionId::TransitionPushLeft
            | ActionId::TransitionPushRight
            | ActionId::TransitionPushUp
            | ActionId::TransitionPushDown
            | ActionId::TransitionZoom
            | ActionId::PasteAttributes
            | ActionId::PasteColor
            | ActionId::PasteEffects
            | ActionId::PasteTransform
            | ActionId::PasteSpeed
            | ActionId::PasteAudio
            | ActionId::SelectForward
            | ActionId::SelectForwardAll
            | ActionId::ExtendAudio
            | ActionId::ExtendVideo
            | ActionId::NudgeBack
            | ActionId::NudgeForward
            | ActionId::NudgeBack10
            | ActionId::NudgeForward10
            | ActionId::Reverse
            | ActionId::ClipEnabled
            | ActionId::ClipLabel
            | ActionId::NewSolid
            | ActionId::NewGradient
            | ActionId::NewCountdown
            | ActionId::NewBars
            | ActionId::NewTone
            | ActionId::NewBarsAndTone
            | ActionId::NewAdjustment
            | ActionId::NewRectangle
            | ActionId::NewEllipse
            | ActionId::NewLine
            | ActionId::NewArrow
            | ActionId::NewTitle => Category::Clips,
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
            | ActionId::ZoomToSelection
            // How tall the tracks are drawn: a view of the timeline, not an edit.
            | ActionId::TrackSmall
            | ActionId::TrackMedium
            | ActionId::TrackLarge
            | ActionId::TracksSmaller
            | ActionId::TracksLarger
            // The viewer's own look: how big, how sharp while playing, which guides.
            | ActionId::ViewZoom
            | ActionId::PlaybackQuality
            | ActionId::GuideStyle
            | ActionId::GuideMask
            | ActionId::ToggleSubtitles
            | ActionId::ToggleWaveforms
            | ActionId::ToggleScrubAudio
            | ActionId::CompareOff
            | ActionId::CompareSplit
            | ActionId::CompareSide
            // ...and the angle monitor: a second way of looking at the footage.
            | ActionId::ToggleAngleMonitor
            // ...and the video output window and its display: a second window onto the same
            // picture, which edits nothing and saves nothing.
            | ActionId::ToggleOutputWindow
            | ActionId::OutputDisplay
            // ...and which file the picture is decoded from, which changes
            // nothing that is saved and nothing that is exported: a stand-in
            // is what one watches while cutting, not what one delivers.
            | ActionId::ToggleProxies
            // ...and whether those stand-ins are made without being asked: the
            // other half of the same switch, filed beside it so a card reader
            // meets the pair the panel shows as a pair.
            | ActionId::ToggleAutoProxies
            // ...and what the whole window is painted in: it edits nothing at
            // all, it is what one is looking *with*.
            | ActionId::Theme
            | ActionId::AccentColour
            // ...and what the cue plate is drawn in: a person's own reading
            // preference, same as the theme beside it.
            | ActionId::SubtitleStyle
            // ...and whether that window fills the screen: a way of looking,
            // same as the zoom and the theme beside it.
            | ActionId::Fullscreen
            // The settings page: it changes no clip and no lane, only the
            // project's own numbers and this window's preferences.
            | ActionId::Settings
            // Moves the ring, not a clip or a lane: the same "how one is
            // looking/reaching, not what is being edited" shape as the rest
            // of this heading.
            | ActionId::FocusPanels
            | ActionId::About
            | ActionId::ShowKeys => Category::View,
            ActionId::InsertSource
            | ActionId::OverwriteSource
            | ActionId::CloseGap
            | ActionId::Freeze
            | ActionId::CutSelectedWords
            | ActionId::MatchFrame
            | ActionId::ReplaceEdit
            | ActionId::Cut
            | ActionId::Regroup
            | ActionId::Detach
            | ActionId::Group
            | ActionId::Undo
            | ActionId::Redo
            | ActionId::History
            | ActionId::AddVideoLane
            | ActionId::AddAudioLane
            | ActionId::RemoveVideoLane
            // Not a view setting despite being a pointer aid: it decides where
            // a drag actually lands, which is an edit and not a magnification.
            | ActionId::ToggleSnap
            // A marker is saved with the project and travels with the picture
            // through every cut, the way a title does: an edit, and the card
            // that lists them is the project's own list.
            // The subtitle pair is a track pair like the lanes above it: what
            // they add is on the timeline and goes into the file an export
            // writes, where the toggle two headings down only decides whether
            // this window draws it.
            | ActionId::AddSubtitleLane
            | ActionId::RemoveSubtitleLane
            // ...and the door that fills the palette those lanes are dragged
            // from: it edits no lane, but what it brings in is the project's
            // and is saved with it, which is not a view setting.
            | ActionId::ImportSubtitles
            | ActionId::NewSequence
            | ActionId::DuplicateSequence
            | ActionId::RenameSequence
            | ActionId::DeleteSequence
            | ActionId::RenameLane
            | ActionId::RemoveAudioLane => Category::Editing,
            ActionId::ToggleMute
            | ActionId::VolumeUp
            | ActionId::VolumeDown
            | ActionId::Equalizer
            // The mix is the project's sound -- what every track plays at and
            // what the whole of it is held under -- where the three above it
            // are this machine's monitoring. Both are audio, and the card says
            // which is which.
            | ActionId::Mix
            | ActionId::CleanVoice
            | ActionId::VoiceDynamics
            // The creator tools: they listen to or write the project's sound (and the
            // words said in it), and open one card each.
            | ActionId::Normalize
            | ActionId::Captions
            | ActionId::Fillers
            | ActionId::Silence
            | ActionId::MarkBeats
            | ActionId::Visualizer
            | ActionId::Voiceover
            | ActionId::RecordVoiceover => Category::Audio,
            // Under the heading the panel button sits in, beside the save and
            // the export it stands next to on screen.
            ActionId::Save
            | ActionId::ReloadProject
            | ActionId::NewProject
            | ActionId::SaveVersion
            | ActionId::SaveTemplate
            | ActionId::ExportCaptions
            | ActionId::Versions
            | ActionId::CollectMedia
            | ActionId::ProjectNotes
            | ActionId::ExportTimelineOtio
            | ActionId::ExportTimelineFcpxml
            | ActionId::ExportTimelineEdl
            | ActionId::Export
            | ActionId::CancelExport
            | ActionId::ShowActions
            // It writes a file like the three above it, and touches no edit
            // list -- what an export's own allow-list already means it stays
            // out of, not what puts it in this heading.
            | ActionId::Screenshot
            // The dock's import pair brings a file *in*, the same heading a
            // file going *out* (Export, Save) already sits under.
            | ActionId::AddFiles
            | ActionId::ImportTimelineOtio
            | ActionId::PasteFilePath
            | ActionId::OpenProject
            // ...and the library's own housekeeping sits beside them: how the media that came
            // in is filed and found again.
            | ActionId::NewBin
            | ActionId::NewBinInside
            | ActionId::RenameBin
            | ActionId::RemoveBin
            | ActionId::ToggleFavourite
            | ActionId::MoveToBin
            | ActionId::ShowFavourites
            | ActionId::ShowAllMedia => Category::File,
        }
    }
}

/// A heading in the keys menu, and the order the headings come in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Playback,
    Marking,
    Editing,
    Clips,
    Audio,
    File,
    View,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Playback,
        Category::Marking,
        Category::Editing,
        Category::Clips,
        Category::Audio,
        Category::File,
        Category::View,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Playback => "Playback",
            Category::Marking => "Marking",
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

/// The words a person types for a shown key, besides the glyph itself, so the
/// keys menu's search finds `↵` by "enter" and `↑` by "up". Each token of a
/// shown chord (`↑ ↓ ↵`, `← / →`) is looked up; a token with no alias adds
/// nothing.
pub(crate) fn search_aliases(shown: &str) -> String {
    const ALIASES: [(&str, &str); 6] = [
        ("↵", "enter return"),
        ("↑", "up arrow"),
        ("↓", "down arrow"),
        ("←", "left arrow"),
        ("→", "right arrow"),
        ("Esc", "escape"),
    ];
    shown
        .split(|c: char| c.is_whitespace() || c == '+' || c == '/')
        .filter_map(|token| {
            ALIASES
                .iter()
                .find(|(glyph, _)| *glyph == token)
                .map(|(_, words)| *words)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub static FIXED:std::sync::LazyLock<[Fixed; 36]> = std::sync::LazyLock::new(|| {
    [
        // Not a chord at all but a way of pressing one, and the only place the
        // editor can say so: holding a key that moves a *value* runs it, and
        // holding anything else still does what one press did.
        Fixed {
            chord: "hold ← → ↑ ↓".into(),
            label: "Hold a key to adjust",
            category: Category::View,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "Esc".into(),
            label: "Close a menu or card, or cancel",
            category: Category::View,
            reach: Reach::Gesture,
        },
        // An open choice list is driven by the keyboard as well as clicked: the
        // list is the door to a setting, and a door only a pointer opens is one
        // half of this editor's users cannot use.
        Fixed {
            chord: "↑ ↓ ↵".into(),
            label: "Move through an open choice list, and take the row",
            category: Category::View,
            reach: Reach::Click("picker-row"),
        },
        Fixed {
            chord: "N".into(),
            label: "Say the export budget: 850k, 6.5M, 6.5 or 1.2G of file",
            category: Category::File,
            reach: Reach::Click("export-c-bitrate"),
        },
        Fixed {
            chord: "− / + / ← →".into(),
            label: "Move the export budget — shift steps whole megabits",
            category: Category::File,
            reach: Reach::Click("export-c-bitrate"),
        },
        Fixed {
            chord: "0–9".into(),
            label: "Type into an open field — the budget, or a clip's duration",
            category: Category::File,
            reach: Reach::Click("export-c-bitrate"),
        },
        Fixed {
            chord: "Backspace".into(),
            label: "Erase a character in the budget or duration field",
            category: Category::File,
            reach: Reach::Click("export-c-bitrate"),
        },
        Fixed {
            chord: "D".into(),
            label: "Choose where the export is written",
            category: Category::File,
            reach: Reach::Click("export-destination"),
        },
        Fixed {
            chord: "↵".into(),
            label: "Start the export — or take the budget being typed",
            category: Category::File,
            reach: Reach::Click("export-confirm"),
        },
        // The equalizer card's own input, for the same reason the export card has
        // its own: a band nothing but a drag can reach is a band half the users of
        // this editor cannot move at all. Card-local, so none of them is bindable.
        Fixed {
            chord: "1–9, 0".into(),
            label: "Select an equalizer band",
            category: Category::Audio,
            reach: Reach::Click("eq-graph"),
        },
        Fixed {
            chord: "↑".into(),
            label: "Raise the selected band 1 dB",
            category: Category::Audio,
            reach: Reach::Click("eq-gain-field"),
        },
        Fixed {
            chord: "↓".into(),
            label: "Lower the selected band 1 dB",
            category: Category::Audio,
            reach: Reach::Click("eq-gain-field"),
        },
        Fixed {
            chord: "← / →".into(),
            label: "Move the selected band down or up in frequency",
            category: Category::Audio,
            reach: Reach::Click("eq-freq-field"),
        },
        Fixed {
            chord: "Shift+← / →".into(),
            label: "Widen or narrow the selected band (its Q)",
            category: Category::Audio,
            reach: Reach::Click("eq-q-field"),
        },
        Fixed {
            chord: "A".into(),
            label: "Add an equalizer band beside the selected one",
            category: Category::Audio,
            reach: Reach::Click("eq-add"),
        },
        Fixed {
            chord: "X".into(),
            label: "Remove the selected equalizer band",
            category: Category::Audio,
            reach: Reach::Click("eq-remove"),
        },
        Fixed {
            chord: "F".into(),
            label: "Flatten the selected band (double-click does the same)",
            category: Category::Audio,
            reach: Reach::Click("eq-flat-band"),
        },
        Fixed {
            chord: "R".into(),
            label: "Flatten every band",
            category: Category::Audio,
            reach: Reach::Click("eq-reset"),
        },
        Fixed {
            chord: "S".into(),
            label: "Show or hide the spectrum behind the curve",
            category: Category::Audio,
            reach: Reach::Click("eq-spectrum"),
        },
        // The colour card's own three, which mean nothing outside it -- the same
        // card-local input the export card's digits are.
        Fixed {
            chord: "↑ / ↓".into(),
            label: "Select a colour slider",
            category: Category::Clips,
            reach: Reach::Click("color-row"),
        },
        Fixed {
            chord: "← / →".into(),
            label: "Move the selected colour slider",
            category: Category::Clips,
            reach: Reach::Click("color-bar"),
        },
        Fixed {
            chord: "R".into(),
            label: "Take the colour grade off the clip",
            category: Category::Clips,
            reach: Reach::Click("color-reset"),
        },
        // The transform card's own three, the same shape as the colour card's:
        // up/down pick a band (position, scale, rotate, crop), left/right move
        // it, and `r` resets to identity.
        Fixed {
            chord: "↑ / ↓".into(),
            label: "Select a transform band",
            category: Category::Clips,
            reach: Reach::Click("transform-row"),
        },
        Fixed {
            chord: "← / →".into(),
            label: "Move the selected transform band",
            category: Category::Clips,
            reach: Reach::Click("transform-bar"),
        },
        Fixed {
            chord: "R".into(),
            label: "Take the transform off the clip",
            category: Category::Clips,
            reach: Reach::Click("transform-reset"),
        },
        // The silence card's two apply keys. Card-local like every stroke above --
        // they mean nothing while it is closed -- but the card is the one place in
        // this editor where a key rewrites forty places at once, so both of them
        // are listed rather than hidden in the card's own hint line.
        Fixed {
            chord: "↵".into(),
            label: "Cut every silence the card found",
            category: Category::Clips,
            reach: Reach::Click("silence-apply"),
        },
        Fixed {
            chord: "F".into(),
            label: "Speed the silences up instead of cutting them",
            category: Category::Clips,
            reach: Reach::Click("silence-apply"),
        },
        Fixed {
            chord: "K".into(),
            label: "Keep only the silences the card found",
            category: Category::Clips,
            reach: Reach::Click("silence-keep-only"),
        },
        // The markers card's own three: up/down select a marker, `↵` renames it
        // and shift+`↵` edits its note (`↵` then saves what was typed, esc gives it up).
        Fixed {
            chord: "↑ / ↓".into(),
            label: "Select marker",
            category: Category::Editing,
            reach: Reach::Click("markers-row"),
        },
        Fixed {
            chord: "↵".into(),
            label: "Rename marker",
            category: Category::Editing,
            reach: Reach::Click("markers-name"),
        },
        Fixed {
            chord: "Shift+↵".into(),
            label: "Edit marker note",
            category: Category::Editing,
            reach: Reach::Click("markers-note"),
        },
        // The pointer's trims on the timeline, in plain words: nothing on
        // screen says that shift turns an edge drag into a roll or a body drag
        // into a slip, so the sheet does. The status bar says the same while
        // the pointer rests on a clip (`ui/bench_stance/clip.rs`).
        Fixed {
            chord: "drag edge".into(),
            label: "Trim: lengthen or shorten the clip from that edge",
            category: Category::Editing,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "Shift+drag edge".into(),
            label: "Roll the cut: both clips change, the timeline keeps its length",
            category: Category::Editing,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "Shift+drag clip".into(),
            label: "Slip: show other footage of the file in the same place",
            category: Category::Editing,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "Shift+Alt+drag clip".into(),
            label: "Slide: move the clip between its neighbours, which give and take",
            category: Category::Editing,
            reach: Reach::Gesture,
        },
        Fixed {
            chord: "Alt+drag".into(),
            label: "Move or trim with snapping off, for that drag only",
            category: Category::Editing,
            reach: Reach::Gesture,
        },
    ]
});

/// A stroke as the key handler sees it: gpui's key name plus the three
/// modifiers this editor binds, `ctrl`, `shift` and `alt`.
///
/// Shift is kept on letters: gpui 0.2.2 reports `ctrl+shift+i` as the key `i`
/// with both held, so it is a different stroke from `ctrl+i`. It is not kept on
/// symbols: gpui clears shift for any single-char key whose upper and lower
/// case are the same (linux/platform.rs:909-917), so a shifted symbol arrives
/// as the glyph it types -- shift+/ is `?`, shift+. is `>`. Those are bound on
/// the literal key, with no shift, which is why `?` needs no modifier at all.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// The file's spelling, and the only one [`parse`] accepts back: the
    /// modifiers in one fixed order, then the key.
    fn text(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("ctrl+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        out.push_str(&self.key);
        out
    }

    /// The spelling a person reads, as DESIGN.md §10.2 writes it: `Ctrl+Shift+I`,
    /// `Alt+1`, `Delete`, `F11`, `←`. Modifiers are capitalised, a letter is a
    /// capital, and named keys are title case ([`Chord::shown_key`]). Display
    /// only: the file keeps gpui's lowercase names. Public because a keybindings
    /// row shows one chord, where [`Keymap::display`] shows every chord an action
    /// answers to.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(&Self::shown_key(&self.key));
        out
    }

    /// One key as a person reads it. Named keys are title case (`Delete`,
    /// `PageUp`, `Esc`), `enter` is `↵`, the arrows are glyphs, function keys
    /// are `F1`..`F12`, and a single letter is its capital. Symbols and anything
    /// else are left as gpui names them.
    fn shown_key(key: &str) -> String {
        if let Some(n) = key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=12).contains(n))
        {
            return format!("F{n}");
        }
        match key {
            "escape" => "Esc".to_string(),
            "enter" => "↵".to_string(),
            "left" => "←".to_string(),
            "right" => "→".to_string(),
            "up" => "↑".to_string(),
            "down" => "↓".to_string(),
            "backspace" => "Backspace".to_string(),
            "delete" => "Delete".to_string(),
            "space" => "Space".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "pageup" => "PageUp".to_string(),
            "pagedown" => "PageDown".to_string(),
            "tab" => "Tab".to_string(),
            key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
                key.to_ascii_uppercase()
            }
            key => key.to_string(),
        }
    }

    /// The spelling a badge reads. It is the same spelling as
    /// [`Chord::pretty`], so a hint in the status bar, the transport and the
    /// toolbar all say `Ctrl+K`, `Space`, `Delete`. The old short forms (`^K`,
    /// `Spc`, `Del`) were dropped so one key never has two spellings on screen.
    pub fn compact(&self) -> String {
        self.pretty()
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

    /// Strict: each of `ctrl+`, `shift+` and `alt+` at most once, in any order,
    /// then a key gpui could have reported. A spelling this cannot emit back
    /// unchanged is refused rather than normalised, because the round-trip is
    /// what makes the file editable by hand at all -- `shift+ctrl+i` is read as
    /// `ctrl+shift+i` and written back that way, but `Ctrl+i` is refused.
    fn parse(text: &str, n: usize) -> Result<Self, String> {
        let (mut ctrl, mut shift, mut alt) = (false, false, false);
        let mut rest = text;
        loop {
            if !ctrl && let Some(r) = rest.strip_prefix("ctrl+") {
                ctrl = true;
                rest = r;
            } else if !shift && let Some(r) = rest.strip_prefix("shift+") {
                shift = true;
                rest = r;
            } else if !alt && let Some(r) = rest.strip_prefix("alt+") {
                alt = true;
                rest = r;
            } else {
                break;
            }
        }
        if rest.is_empty()
            || rest.contains(' ')
            || rest.contains('+')
            || rest.chars().any(|c| c.is_ascii_uppercase())
        {
            return Err(format!("line {n}: {text:?} is not a chord"));
        }
        Ok(Chord {
            key: rest.to_string(),
            ctrl,
            shift,
            alt,
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
/// the invariant [`parse`] defends, and it is what lets [`Keymap::lookup`]
/// answer with the first match.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Keymap {
    /// The bindings the editor ships with, in the order the editor lists them:
    /// DESIGN.md §10.2's table, row for row. `defaults_are_the_proposed_keys`
    /// pins every row, so a change here is a change to that table first.
    pub fn defaults() -> Self {
        let b = |action, text: &str| Binding {
            action,
            chord: Chord::parse(text, 0).expect("a default chord is a chord"),
        };
        Keymap {
            bindings: vec![
                // File
                b(ActionId::Export, "e"),
                b(ActionId::Save, "ctrl+s"),
                b(ActionId::AddFiles, "ctrl+i"),
                b(ActionId::OpenProject, "ctrl+o"),
                // PasteFilePath: menu only, no chord.
                b(ActionId::CancelExport, "ctrl+escape"),
                b(ActionId::ShowActions, "ctrl+k"),
                // View
                b(ActionId::ShowKeys, "?"),
                b(ActionId::ShowKeys, "f1"),
                // File
                b(ActionId::Screenshot, "f12"),
                // Playback
                b(ActionId::Play, "space"),
                b(ActionId::Loop, "r"),
                b(ActionId::PlayAround, "shift+k"),
                b(ActionId::StepBack, "left"),
                b(ActionId::StepForward, "right"),
                b(ActionId::JumpBack, "ctrl+left"),
                b(ActionId::JumpForward, "ctrl+right"),
                b(ActionId::GoStart, "home"),
                b(ActionId::GoEnd, "end"),
                // Free: Ctrl+G is Group.
                b(ActionId::GoToTime, "ctrl+j"),
                b(ActionId::PrevSyncPoint, "ctrl+["),
                b(ActionId::NextSyncPoint, "ctrl+]"),
                b(ActionId::WalkCutNext, "down"),
                b(ActionId::WalkCutNext, "."),
                b(ActionId::WalkCutPrev, "up"),
                b(ActionId::WalkCutPrev, ","),
                b(ActionId::WalkCutNext10, ">"),
                b(ActionId::WalkCutPrev10, "<"),
                b(ActionId::NextMarker, "shift+m"),
                b(ActionId::PrevMarker, "ctrl+shift+m"),
                b(ActionId::SetIn, "i"),
                b(ActionId::SetOut, "o"),
                b(ActionId::ClearRange, "ctrl+u"),
                b(ActionId::RenderInOut, "enter"),
                b(ActionId::ShuttleBack, "j"),
                b(ActionId::ShuttleStop, "k"),
                b(ActionId::ShuttleForward, "l"),
                // Editing
                b(ActionId::InsertSource, "f9"),
                b(ActionId::OverwriteSource, "f10"),
                b(ActionId::Cut, "s"),
                b(ActionId::Regroup, "g"),
                b(ActionId::Detach, "d"),
                b(ActionId::Group, "ctrl+g"),
                b(ActionId::Undo, "ctrl+z"),
                b(ActionId::Redo, "ctrl+y"),
                b(ActionId::Redo, "ctrl+shift+z"),
                // AddVideoLane: menu only, no chord.
                // AddAudioLane: menu only, no chord.
                // RemoveVideoLane: menu only, no chord.
                // RemoveAudioLane: menu only, no chord.
                // AddSubtitleLane: menu only, no chord.
                // RemoveSubtitleLane: menu only, no chord.
                b(ActionId::ImportSubtitles, "ctrl+shift+i"),
                b(ActionId::ToggleSnap, "n"),
                b(ActionId::AddMarker, "alt+m"),
                b(ActionId::Markers, "alt+8"),
                // Clips
                b(ActionId::Copy, "ctrl+c"),
                b(ActionId::Paste, "ctrl+v"),
                b(ActionId::Select, "tab"),
                b(ActionId::SelectNext, "}"),
                b(ActionId::SelectPrev, "{"),
                b(ActionId::SelectAll, "ctrl+a"),
                b(ActionId::Deselect, "escape"),
                b(ActionId::Delete, "delete"),
                b(ActionId::Delete, "backspace"),
                b(ActionId::Lift, ";"),
                b(ActionId::Color, "alt+2"),
                b(ActionId::Transform, "alt+3"),
                b(ActionId::Fit, "alt+7"),
                b(ActionId::Speed, "alt+1"),
                b(ActionId::Silence, "alt+5"),
                b(ActionId::MarkBeats, "alt+b"),
                b(ActionId::Visualizer, "alt+6"),
                b(ActionId::Angle1, "1"),
                b(ActionId::Angle2, "2"),
                b(ActionId::Angle3, "3"),
                b(ActionId::Angle4, "4"),
                b(ActionId::Angle5, "5"),
                b(ActionId::Angle6, "6"),
                b(ActionId::Angle7, "7"),
                b(ActionId::Angle8, "8"),
                b(ActionId::Angle9, "9"),
                b(ActionId::Crossfade, "ctrl+f"),
                b(ActionId::TrimIn, "["),
                b(ActionId::TrimOut, "]"),
                b(ActionId::LoopTrim, "/"),
                b(ActionId::RippleTrimInToPlayhead, "q"),
                b(ActionId::RippleTrimOutToPlayhead, "w"),
                b(ActionId::TrimInToPlayhead, "alt+q"),
                b(ActionId::TrimOutToPlayhead, "alt+w"),
                b(ActionId::RollBack, "alt+["),
                b(ActionId::RollForward, "alt+]"),
                b(ActionId::SlipBack, "alt+,"),
                b(ActionId::SlipForward, "alt+."),
                b(ActionId::SlideBack, "alt+left"),
                b(ActionId::SlideForward, "alt+right"),
                b(ActionId::Dissolve, "ctrl+d"),
                b(ActionId::Reverse, "ctrl+alt+r"),
                b(ActionId::ClipEnabled, "shift+e"),
                b(ActionId::NewTitle, "alt+t"),
                // ClipLabel, NewSolid, NewGradient, NewCountdown, NewBars, NewTone, NewBarsAndTone, NewAdjustment: menu only, no chord.
                // Audio
                b(ActionId::ToggleMute, "m"),
                b(ActionId::VolumeUp, "pageup"),
                b(ActionId::VolumeDown, "pagedown"),
                b(ActionId::Equalizer, "alt+4"),
                b(ActionId::Mix, "f"),
                b(ActionId::Mix, "alt+x"),
                b(ActionId::Effects, "alt+e"),
                b(ActionId::CleanVoice, "alt+9"),
                b(ActionId::VoiceDynamics, "alt+0"),
                // View
                b(ActionId::Resolution, "ctrl+r"),
                b(ActionId::ZoomIn, "="),
                b(ActionId::ZoomIn, "ctrl+="),
                b(ActionId::ZoomOut, "-"),
                b(ActionId::ZoomOut, "ctrl+-"),
                b(ActionId::ZoomFit, "\\"),
                b(ActionId::ZoomFit, "ctrl+0"),
                b(ActionId::TracksLarger, "alt+="),
                b(ActionId::TracksSmaller, "alt+-"),
                // Free: the shifted Z, next to the fit keys.
                b(ActionId::ZoomToSelection, "shift+z"),
                b(ActionId::ToggleSubtitles, "t"),
                b(ActionId::ToggleAngleMonitor, "alt+a"),
                b(ActionId::ToggleProxies, "ctrl+p"),
                // ToggleAutoProxies: menu only, no chord.
                b(ActionId::Normalize, "alt+n"),
                b(ActionId::Captions, "alt+c"),
                b(ActionId::Fillers, "alt+u"),
                b(ActionId::Voiceover, "alt+v"),
                // Theme: menu only, no chord.
                b(ActionId::Fullscreen, "f11"),
                b(ActionId::SubtitleStyle, "y"),
                b(ActionId::Settings, "ctrl+,"),
                b(ActionId::CloseGap, "shift+delete"),
                b(ActionId::Freeze, "shift+r"),
                b(ActionId::PasteAttributes, "ctrl+alt+v"),
                b(ActionId::MatchFrame, "shift+f"),
                b(ActionId::ReplaceEdit, "f8"),
                b(ActionId::SelectForward, "alt+y"),
                b(ActionId::SelectForwardAll, "shift+a"),
                b(ActionId::ExtendAudio, "shift+alt+l"),
                b(ActionId::ExtendVideo, "shift+alt+j"),
                b(ActionId::NudgeBack, "ctrl+alt+left"),
                b(ActionId::NudgeForward, "ctrl+alt+right"),
                b(ActionId::NudgeBack10, "ctrl+shift+alt+left"),
                b(ActionId::NudgeForward10, "ctrl+shift+alt+right"),
                b(ActionId::FocusPanels, "f6"),
            ],
        }
    }

    /// What a stroke means, if anything. `key` is gpui's key name and the three
    /// modifiers are the ones the key handler reads. A stroke matches only a
    /// binding with the same modifiers, so `i` and `ctrl+shift+i` are two.
    pub fn lookup(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<ActionId> {
        self.bindings
            .iter()
            .find(|b| {
                b.chord.key == key
                    && b.chord.ctrl == ctrl
                    && b.chord.shift == shift
                    && b.chord.alt == alt
            })
            .map(|b| b.action)
    }

    /// Every stroke that reaches an action, each in display form, in binding order. Empty when
    /// nothing reaches it; a reference sheet shows that as an empty key cell.
    pub fn strokes(&self, action: ActionId) -> Vec<String> {
        self.bindings.iter().filter(|b| b.action == action).map(|b| b.chord.pretty()).collect()
    }

    /// Every stroke that reaches an action, as one readable phrase: `X or
    /// Delete`. An action nobody can reach says so rather than reading blank.
    pub fn display(&self, action: ActionId) -> String {
        let strokes = self.strokes(action);
        if strokes.is_empty() {
            "unbound".to_string()
        } else {
            strokes.join(" or ")
        }
    }

    /// The primary chord for `action`, compact ([`Chord::compact`]) -- a
    /// badge that has room for one token, not [`Keymap::display`]'s sentence.
    /// The first bound stroke wins (`defaults`' own insertion order, and
    /// [`parse`]'s "no chord appears twice" leaves at most one anyway); an
    /// action with no stroke reads `--` rather than the unbound sentence,
    /// since a badge is not a warning.
    pub fn chord(&self, action: ActionId) -> String {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.chord.compact())
            .unwrap_or_else(|| "--".to_string())
    }

    /// The first stroke for `action` in display form (`Delete`, `Ctrl+Z`), for
    /// a control with room for one key only. [`Keymap::display`] lists every
    /// stroke; a toolbar shows the one a person would press.
    pub fn primary(&self, action: ActionId) -> String {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.chord.pretty())
            .unwrap_or_else(|| "unbound".to_string())
    }

    /// Every binding, in display order.
    pub fn entries(&self) -> &[Binding] {
        &self.bindings
    }

    /// The other binding that already holds `chord`, if any. The Keys page asks
    /// this before it moves a stroke, so the conflict is named rather than
    /// silently stolen.
    pub(crate) fn holder(&self, chord: &Chord, except: usize) -> Option<usize> {
        self.bindings
            .iter()
            .enumerate()
            .find(|(i, b)| *i != except && b.chord == *chord)
            .map(|(i, _)| i)
    }

    /// Puts `chord` on the binding at `index`. A stroke another binding holds
    /// goes to that binding in exchange for the one `index` gives up, so no
    /// chord ever appears twice -- the invariant [`Keymap::lookup`] answers by.
    pub(crate) fn swap_in(&mut self, index: usize, chord: Chord) {
        let old = self.bindings[index].chord.clone();
        if let Some(other) = self.holder(&chord, index) {
            self.bindings[other].chord = old;
        }
        self.bindings[index].chord = chord;
    }

    /// Whether `action` carries exactly its shipped strokes. `shipped` is
    /// [`Keymap::defaults`], passed in so a page asking once per row parses the
    /// defaults once per frame rather than once per row.
    pub(crate) fn is_default(&self, action: ActionId, shipped: &Keymap) -> bool {
        let now: Vec<&Chord> = self
            .bindings
            .iter()
            .filter(|b| b.action == action)
            .map(|b| &b.chord)
            .collect();
        let want: Vec<&Chord> = shipped
            .bindings
            .iter()
            .filter(|b| b.action == action)
            .map(|b| &b.chord)
            .collect();
        now.len() == want.len() && want.iter().all(|c| now.contains(c))
    }

    /// Restores `action` to its shipped strokes, in their shipped place in the
    /// list. A shipped stroke another action now holds is taken back from it;
    /// the actions that lost one are returned so the page can say so.
    pub(crate) fn reset_action(&mut self, action: ActionId) -> Vec<ActionId> {
        let shipped: Vec<Chord> = Keymap::defaults()
            .bindings
            .into_iter()
            .filter(|b| b.action == action)
            .map(|b| b.chord)
            .collect();
        let mut displaced = Vec::new();
        self.bindings.retain(|b| {
            let taken = b.action != action && shipped.contains(&b.chord);
            if taken && !displaced.contains(&b.action) {
                displaced.push(b.action);
            }
            !taken
        });
        // Taken back first, so the position read next is where the action's
        // own strokes sit in the list after that.
        let at = self
            .bindings
            .iter()
            .position(|b| b.action == action)
            .unwrap_or(self.bindings.len());
        self.bindings.retain(|b| b.action != action);
        for (k, chord) in shipped.into_iter().enumerate() {
            self.bindings.insert(at + k, Binding { action, chord });
        }
        displaced
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
                        "Keybindings ignored — {}: {e} — the defaults are in force",
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
                    "Keybindings part-read — {}, {} — those strokes are unbound",
                    path.display(),
                    dropped.join("; ")
                )),
            ),
            Err(e) => (
                Self::defaults(),
                Some(format!(
                    "Keybindings ignored — {}, {e} — the defaults are in force",
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
        config_path_in(xdg_config_home(), std::env::var_os("HOME"))
    }
}

#[cfg(test)]
thread_local! {
    /// The test-only stand-in for `$XDG_CONFIG_HOME`, scoped to the test's own
    /// thread so no other test sees it. Set through [`set_test_config_home`].
    static TEST_CONFIG_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Points every config file at `dir` for the calling test thread, or back at the
/// real environment with `None`. Stands in for setting `XDG_CONFIG_HOME`, which
/// Rust 2024 no longer lets safe code change it.
#[cfg(test)]
pub(crate) fn set_test_config_home(dir: Option<PathBuf>) {
    TEST_CONFIG_HOME.with(|c| *c.borrow_mut() = dir);
}

/// The XDG config directory as the environment (or a test) names it, if any.
pub(crate) fn xdg_config_home() -> Option<OsString> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_HOME.with(|c| c.borrow().clone()) {
        return Some(dir.into_os_string());
    }
    std::env::var_os("XDG_CONFIG_HOME")
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

/// First line of the files the build before shift and alt wrote. Still read;
/// see [`V1_DEFAULTS`].
const MAGIC_V1: &str = "edith-keys 1";

/// The chords the build before shift and alt defaulted each action to, for
/// every action whose stroke has since moved. A `edith-keys 1` file that holds
/// exactly these for an action is one nobody rebound, and that action takes
/// today's defaults. Anything else in such a file is somebody's own choice and
/// is kept. An action that did not exist then (the shuttle, the shortcuts
/// sheet) has no entries here, so its absence is what gets the default.
const V1_DEFAULTS: &[(ActionId, &[&str])] = &[
    (ActionId::AddFiles, &["ctrl+o"]),
    (ActionId::PasteFilePath, &["ctrl+l"]),
    (ActionId::ShowActions, &["?"]),
    (ActionId::ShowKeys, &[]),
    (ActionId::Screenshot, &["w"]),
    (ActionId::WalkCutNext, &["."]),
    (ActionId::WalkCutPrev, &[","]),
    (ActionId::ShuttleBack, &[]),
    (ActionId::ShuttleStop, &[]),
    (ActionId::ShuttleForward, &[]),
    (ActionId::Undo, &["z", "ctrl+z"]),
    (ActionId::AddVideoLane, &["v"]),
    (ActionId::AddAudioLane, &["a"]),
    (ActionId::RemoveVideoLane, &["ctrl+b"]),
    (ActionId::RemoveAudioLane, &["ctrl+q"]),
    (ActionId::AddSubtitleLane, &["c"]),
    (ActionId::RemoveSubtitleLane, &["ctrl+t"]),
    (ActionId::ImportSubtitles, &["ctrl+i"]),
    (ActionId::Delete, &["x", "delete"]),
    (ActionId::Lift, &["l"]),
    (ActionId::Color, &["k"]),
    (ActionId::Transform, &["ctrl+k"]),
    (ActionId::Fit, &["p"]),
    (ActionId::Speed, &["j"]),
    (ActionId::Silence, &["u"]),
    (ActionId::Visualizer, &["h"]),
    (ActionId::Reframe, &[]),
    (ActionId::TrackMask, &[]),
    (ActionId::TrimInToPlayhead, &["ctrl+{"]),
    (ActionId::TrimOutToPlayhead, &["ctrl+}"]),
    (ActionId::Dissolve, &["ctrl+x"]),
    (ActionId::VolumeUp, &["="]),
    (ActionId::VolumeDown, &["-"]),
    (ActionId::Equalizer, &["q"]),
    (ActionId::ZoomIn, &["ctrl+="]),
    (ActionId::ZoomOut, &["ctrl+-"]),
    (ActionId::ZoomFit, &["ctrl+0"]),
    (ActionId::ToggleAutoProxies, &["ctrl+m"]),
    (ActionId::Theme, &["ctrl+h"]),
];

/// Moves a `edith-keys 1` file onto today's defaults, action by action (see
/// [`V1_DEFAULTS`]). The lines it keeps go first, so a stroke a kept line holds
/// is never taken away from it by a default; a default whose stroke is held
/// stays unbound instead.
fn migrate_v1(file: Vec<Binding>) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    let mut replaced: Vec<ActionId> = Vec::new();
    for action in ActionId::ALL {
        let held: Vec<Binding> = file
            .iter()
            .filter(|b| b.action == action)
            .cloned()
            .collect();
        let mut have: Vec<String> = held.iter().map(|b| b.chord.text()).collect();
        have.sort();
        let untouched = V1_DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .is_some_and(|(_, old)| {
                let mut want: Vec<String> = old.iter().map(|s| s.to_string()).collect();
                want.sort();
                have == want
            });
        if untouched {
            replaced.push(action);
        } else {
            out.extend(held);
        }
    }
    for default in Keymap::defaults().bindings {
        if replaced.contains(&default.action) && !out.iter().any(|b| b.chord == default.chord) {
            out.push(default);
        }
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
    let migrating = match first {
        MAGIC => false,
        MAGIC_V1 => true,
        _ => {
            return Err(match first.strip_prefix("edith-keys ") {
                Some(v) => format!("line 1: unsupported version {v}"),
                None => "line 1: not a keybindings file".to_string(),
            });
        }
    };

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
    let bindings = if migrating {
        migrate_v1(bindings)
    } else {
        bindings
    };
    Ok((Keymap { bindings }, dropped))
}

#[cfg(test)]
mod tests {
    use engine::scratch::Scratch;

    use super::{ActionId, Chord, Keymap, V1_DEFAULTS, config_path_in, emit, parse};

    fn chord(key: &str, ctrl: bool) -> Chord {
        Chord {
            key: key.to_string(),
            ctrl,
            shift: false,
            alt: false,
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
        // Each stroke as the file spells it, read the way the key handler reads it.
        let at = |text: &str| {
            let c = Chord::parse(text, 0).expect("a stroke");
            k.lookup(&c.key, c.ctrl, c.shift, c.alt)
        };
        // The cut odometer: bare walks one, `<`/`>` (what shift+,/shift+. actually
        // type) strides ten, and the arrows walk too.
        assert_eq!(at("."), Some(ActionId::WalkCutNext));
        assert_eq!(at("down"), Some(ActionId::WalkCutNext));
        assert_eq!(at(","), Some(ActionId::WalkCutPrev));
        assert_eq!(at("up"), Some(ActionId::WalkCutPrev));
        assert_eq!(at(">"), Some(ActionId::WalkCutNext10));
        assert_eq!(at("<"), Some(ActionId::WalkCutPrev10));
        assert_eq!(at("/"), Some(ActionId::LoopTrim));
        // The trim pair on its bare brackets, and the playhead trims on Q and W.
        assert_eq!(at("["), Some(ActionId::TrimIn));
        assert_eq!(at("]"), Some(ActionId::TrimOut));
        assert_eq!(at("q"), Some(ActionId::RippleTrimInToPlayhead));
        assert_eq!(at("w"), Some(ActionId::RippleTrimOutToPlayhead));
        assert_eq!(at("alt+q"), Some(ActionId::TrimInToPlayhead));
        // The shuttle: J back, K pauses, L plays.
        assert_eq!(at("j"), Some(ActionId::ShuttleBack));
        assert_eq!(at("k"), Some(ActionId::ShuttleStop));
        assert_eq!(at("l"), Some(ActionId::ShuttleForward));
        // The shortcuts sheet is ? and F1; command search is Ctrl+K alone.
        assert_eq!(at("?"), Some(ActionId::ShowKeys));
        assert_eq!(at("f1"), Some(ActionId::ShowKeys));
        assert_eq!(at("ctrl+k"), Some(ActionId::ShowActions));
        assert_eq!(at("f11"), Some(ActionId::Fullscreen));
        assert_eq!(at("f12"), Some(ActionId::Screenshot));
        assert_eq!(at("y"), Some(ActionId::SubtitleStyle));
        assert_eq!(at("ctrl+p"), Some(ActionId::ToggleProxies));
        // Import media is Ctrl+I; Import subtitles is Ctrl+Shift+I. The shift is
        // part of the stroke, so the two never answer for each other.
        assert_eq!(at("ctrl+i"), Some(ActionId::AddFiles));
        assert_eq!(at("ctrl+shift+i"), Some(ActionId::ImportSubtitles));
        assert_eq!(at("ctrl+o"), Some(ActionId::OpenProject));
        assert_eq!(at("ctrl+l"), None);
        assert_eq!(at("ctrl+h"), None);
        assert_eq!(at("space"), Some(ActionId::Play));
        // The seek keys: arrows a frame, ctrl arrows a second, the ends of the timeline.
        assert_eq!(at("left"), Some(ActionId::StepBack));
        assert_eq!(at("right"), Some(ActionId::StepForward));
        assert_eq!(at("ctrl+left"), Some(ActionId::JumpBack));
        assert_eq!(at("ctrl+right"), Some(ActionId::JumpForward));
        assert_eq!(at("home"), Some(ActionId::GoStart));
        assert_eq!(at("end"), Some(ActionId::GoEnd));
        // The source's own grid is on ctrl+brackets.
        assert_eq!(at("ctrl+["), Some(ActionId::PrevSyncPoint));
        assert_eq!(at("ctrl+]"), Some(ActionId::NextSyncPoint));
        assert_eq!(at("i"), Some(ActionId::SetIn));
        assert_eq!(at("o"), Some(ActionId::SetOut));
        assert_eq!(at("ctrl+u"), Some(ActionId::ClearRange));
        assert_eq!(at("e"), Some(ActionId::Export));
        assert_eq!(at("ctrl+s"), Some(ActionId::Save));
        assert_eq!(at("ctrl+escape"), Some(ActionId::CancelExport));
        assert_eq!(at("escape"), Some(ActionId::Deselect));
        // Editing and clips.
        assert_eq!(at("ctrl+c"), Some(ActionId::Copy));
        assert_eq!(at("ctrl+v"), Some(ActionId::Paste));
        assert_eq!(at("s"), Some(ActionId::Cut));
        assert_eq!(at("g"), Some(ActionId::Regroup));
        assert_eq!(at("d"), Some(ActionId::Detach));
        assert_eq!(at("ctrl+g"), Some(ActionId::Group));
        assert_eq!(at("tab"), Some(ActionId::Select));
        assert_eq!(at("}"), Some(ActionId::SelectNext));
        assert_eq!(at("{"), Some(ActionId::SelectPrev));
        assert_eq!(at("ctrl+a"), Some(ActionId::SelectAll));
        assert_eq!(at("delete"), Some(ActionId::Delete));
        assert_eq!(at("backspace"), Some(ActionId::Delete));
        assert_eq!(at(";"), Some(ActionId::Lift));
        assert_eq!(at("alt+1"), Some(ActionId::Speed));
        assert_eq!(at("alt+2"), Some(ActionId::Color));
        assert_eq!(at("alt+3"), Some(ActionId::Transform));
        assert_eq!(at("alt+4"), Some(ActionId::Equalizer));
        assert_eq!(at("alt+5"), Some(ActionId::Silence));
        assert_eq!(at("alt+6"), Some(ActionId::Visualizer));
        assert_eq!(at("alt+7"), Some(ActionId::Fit));
        assert_eq!(at("ctrl+alt+r"), Some(ActionId::Reverse));
        assert_eq!(at("alt+9"), Some(ActionId::CleanVoice));
        assert_eq!(at("alt+0"), Some(ActionId::VoiceDynamics));
        assert_eq!(at("ctrl+f"), Some(ActionId::Crossfade));
        assert_eq!(at("ctrl+d"), Some(ActionId::Dissolve));
        assert_eq!(at("ctrl+z"), Some(ActionId::Undo));
        assert_eq!(at("ctrl+y"), Some(ActionId::Redo));
        assert_eq!(at("ctrl+shift+z"), Some(ActionId::Redo));
        // Audio and view.
        assert_eq!(at("m"), Some(ActionId::ToggleMute));
        assert_eq!(at("pageup"), Some(ActionId::VolumeUp));
        assert_eq!(at("pagedown"), Some(ActionId::VolumeDown));
        assert_eq!(at("f"), Some(ActionId::Mix));
        assert_eq!(at("ctrl+r"), Some(ActionId::Resolution));
        assert_eq!(at("="), Some(ActionId::ZoomIn));
        assert_eq!(at("ctrl+="), Some(ActionId::ZoomIn));
        assert_eq!(at("-"), Some(ActionId::ZoomOut));
        assert_eq!(at("ctrl+-"), Some(ActionId::ZoomOut));
        assert_eq!(at("\\"), Some(ActionId::ZoomFit));
        assert_eq!(at("ctrl+0"), Some(ActionId::ZoomFit));
        assert_eq!(at("t"), Some(ActionId::ToggleSubtitles));
        assert_eq!(at("n"), Some(ActionId::ToggleSnap));
        assert_eq!(at("ctrl+,"), Some(ActionId::Settings));
        // What the old defaults answered to and the new ones give up: a bare
        // letter that destroys, the old volume keys, and the moved clip keys.
        // A bare `+` is the grammar's own separator, so it is not a stroke at all.
        assert!(Chord::parse("+", 0).is_err());
        for gone in ["z", "x", "a", "v", "c", "h", "shift+l", "alt+l", "ctrl+e"] {
            assert_eq!(at(gone), None, "{gone} should be unbound");
        }
        // Nothing is bound twice, or lookup's first match would be a coin toss.
        assert_eq!(k.entries().len(), 136);
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
        assert!(text.starts_with("edith-keys 2\n"));
        assert!(text.contains("play space\n"));
        assert!(text.contains("save ctrl+s\n"));
        assert!(text.contains("cancel-export ctrl+escape\n"));
        // Both directions: the file this writes is the file it reads back, and
        // a file read in comes out byte for byte as it went.
        assert_eq!(whole(&text), Keymap::defaults());
        assert_eq!(emit(&whole(&text)), text);
        let handwritten = "edith-keys 2\nundo ctrl+z\nplay space\n";
        assert_eq!(emit(&whole(handwritten)), handwritten);
        // A keymap that binds nothing is legal -- and unusable, which is the
        // user's business, not the parser's.
        assert_eq!(whole("edith-keys 2\n").entries(), &[]);
    }

    #[test]
    fn every_refusal_names_its_line() {
        // Only the first line refuses the whole file.
        let err = |text: &str| parse(text).unwrap_err();
        assert_eq!(err(""), "line 1: not a keybindings file");
        assert_eq!(err("nonsense\n"), "line 1: not a keybindings file");
        assert_eq!(err("edith-keys 3\n"), "line 1: unsupported version 3");

        // Every other kind of bad line costs that line and nothing else.
        let dropped = |text: &str| parse(text).unwrap().1;
        assert_eq!(
            dropped("edith-keys 2\nfly space\n"),
            ["line 2: unknown action \"fly\""]
        );
        assert_eq!(
            dropped("edith-keys 2\nplay space\nplayspace\n"),
            ["line 3: \"playspace\" is not `<action> <chord>`"]
        );
        // An empty line is not optional whitespace, it is a line.
        assert_eq!(
            dropped("edith-keys 2\n\n"),
            ["line 2: \"\" is not `<action> <chord>`"]
        );
        assert_eq!(
            dropped("edith-keys 2\nplay ctrl+shift+shift+x\n"),
            ["line 2: \"ctrl+shift+shift+x\" is not a chord"]
        );
        assert_eq!(
            dropped("edith-keys 2\nplay ctrl+\n"),
            ["line 2: \"ctrl+\" is not a chord"]
        );
        // gpui reports lowercase key names, so an uppercase one would simply
        // never match -- dropped rather than quietly dead.
        assert_eq!(
            dropped("edith-keys 2\nplay X\n"),
            ["line 2: \"X\" is not a chord"]
        );
        // The same stroke twice: whichever line lost would depend on order, so
        // the second one is the one that goes.
        assert_eq!(
            dropped("edith-keys 2\nplay space\ncut space\n"),
            ["line 3: space is already play's"]
        );
    }

    /// The rebind data-loss path: one line the parser cannot use must not cost
    /// the rebinds around it (a `capture` of shift+= wrote `play +`, and the
    /// next load threw the whole file away and went back to the defaults).
    #[test]
    fn one_bad_line_costs_only_that_line() {
        let text = "edith-keys 2\nplay +\nexport ctrl+w\ncut j\n";
        let (keymap, dropped) = parse(text).expect("the magic line is still good");
        assert_eq!(dropped, ["line 2: \"+\" is not a chord"]);
        // The lines around it are in force, which is the whole point.
        assert_eq!(
            keymap.lookup("w", true, false, false),
            Some(ActionId::Export)
        );
        assert_eq!(keymap.lookup("j", false, false, false), Some(ActionId::Cut));
        // ...and the one that went is unbound, not silently something else.
        assert_eq!(keymap.display(ActionId::Play), "unbound");
        // What survives still round-trips: the kept lines emit back as read.
        assert_eq!(emit(&keymap), "edith-keys 2\nexport ctrl+w\ncut j\n");

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
        // A bindable chord is one the file's own grammar can spell: what it
        // writes is what the next load reads, for every stroke that passes
        // here.
        let k = whole("edith-keys 2\nplay ctrl+w\n");
        assert_eq!(whole(&emit(&k)), k);
    }

    #[test]
    fn display_reads_as_a_person_would_say_it() {
        let k = Keymap::defaults();
        assert_eq!(k.display(ActionId::Play), "Space");
        assert_eq!(k.display(ActionId::Save), "Ctrl+S");
        assert_eq!(k.display(ActionId::Delete), "Delete or Backspace");
        assert_eq!(k.display(ActionId::Undo), "Ctrl+Z");
        // A toolbar shows one key: the first stroke, not the phrase.
        assert_eq!(k.primary(ActionId::Delete), "Delete");
        assert_eq!(k.primary(ActionId::Undo), "Ctrl+Z");
        // The one key whose common name is not gpui's.
        assert_eq!(k.display(ActionId::CancelExport), "Ctrl+Esc");
        assert_eq!(whole("edith-keys 2\n").display(ActionId::Cut), "unbound");
        // The label is the editor's column, and never the file's word for it.
        assert_eq!(ActionId::CancelExport.label(), "Cancel export");
        assert_eq!(k.display(ActionId::ToggleSnap), "N");
        assert_eq!(k.display(ActionId::ShowActions), "Ctrl+K");
        assert_eq!(k.display(ActionId::ShowKeys), "? or F1");
        assert_eq!(k.display(ActionId::ToggleSubtitles), "T");
        assert_eq!(k.display(ActionId::ZoomIn), "= or Ctrl+=");
        assert_eq!(k.display(ActionId::GoStart), "Home");
        assert_eq!(k.display(ActionId::StepBack), "←");
        assert_eq!(k.display(ActionId::VolumeUp), "PageUp");
        assert_eq!(k.display(ActionId::Fullscreen), "F11");
        assert_eq!(k.display(ActionId::WalkCutNext), "↓ or .");
        assert_eq!(k.display(ActionId::SetIn), "I");
        assert_eq!(k.display(ActionId::ImportSubtitles), "Ctrl+Shift+I");
        assert_eq!(k.display(ActionId::Speed), "Alt+1");
        assert_eq!(k.display(ActionId::AddVideoLane), "unbound");
    }

    /// DESIGN §8: a label names the verb. Prose truncates -- the KEYS list
    /// ellipsises it, a menu row's fixed plate cuts it mid-word, a hover plate
    /// the same -- so four words is the whole budget, and what a parenthesis
    /// used to carry (why an action refuses) lives in the refusal string the
    /// oracle and the engine already word.
    #[test]
    fn a_label_names_the_verb_in_four_words() {
        let mut seen: Vec<&'static str> = Vec::new();
        for action in ActionId::ALL {
            let label = action.label();
            let words = label.split_whitespace().count();
            assert!(words <= 4, "{action:?}: {words} words -- {label:?}");
            // No parenthesis: "(picture)" and "(sound)" were the jargon a menu
            // reader could not place; the names say the kind on their own.
            assert!(!label.contains('('), "{action:?}: a parenthesis -- {label:?}");
            assert!(
                !label.to_lowercase().starts_with("the "),
                "{action:?}: leads with an article -- {label:?}"
            );
            assert!(!seen.contains(&label), "two actions both named {label:?}");
            seen.push(label);
        }
    }

    /// [`Keymap::chord`] is the badge's one token, not [`Keymap::display`]'s
    /// sentence: the primary (first-bound) stroke only, spelled as
    /// [`Chord::pretty`] spells it.
    #[test]
    fn chord_is_the_primary_stroke_compact() {
        let k = Keymap::defaults();
        assert_eq!(k.chord(ActionId::Delete), "Delete");
        assert_eq!(k.chord(ActionId::Undo), "Ctrl+Z");
        assert_eq!(k.chord(ActionId::AddFiles), "Ctrl+I");
        assert_eq!(k.chord(ActionId::ImportSubtitles), "Ctrl+Shift+I");
        assert_eq!(k.chord(ActionId::CancelExport), "Ctrl+Esc");
        assert_eq!(k.chord(ActionId::Play), "Space");
        assert_eq!(k.chord(ActionId::Speed), "Alt+1");
        assert_eq!(k.chord(ActionId::Settings), "Ctrl+,");
        assert_eq!(k.chord(ActionId::ShowActions), "Ctrl+K");
        // Menu-only: no stroke, so the badge reads `--`.
        assert_eq!(k.chord(ActionId::PasteFilePath), "--");
        assert_eq!(whole("edith-keys 2\n").chord(ActionId::Cut), "--");
    }

    /// The keys search matches a glyph by the word a person says for it, and
    /// `Esc` by its full name.
    #[test]
    fn search_aliases_name_the_glyphs_a_person_would_type() {
        assert_eq!(super::search_aliases("↵"), "enter return");
        assert_eq!(super::search_aliases("Esc"), "escape");
        assert_eq!(
            super::search_aliases("↑ ↓ ↵"),
            "up arrow down arrow enter return"
        );
        assert_eq!(super::search_aliases("← / →"), "left arrow right arrow");
        assert_eq!(super::search_aliases("N"), "");
    }

    #[test]
    fn a_saved_keymap_comes_back_off_the_disk() {
        // Its own directory, never the real config one: this test writes.
        let dir = Scratch::dir("edith-keymap");
        let path = dir.join("edith").join("keybindings");
        // A defaults file with Export's own line swapped for a free stroke --
        // parsed rather than rebound, since nothing left in the API mutates
        // a `Keymap` in place.
        let text: String = emit(&Keymap::defaults())
            .lines()
            .filter(|l| !l.starts_with("export "))
            .chain(std::iter::once("export ctrl+w"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let written = whole(&text);
        // The config directory does not exist yet -- the first save makes it.
        written.save_to(&path).unwrap();
        let (read, notice) = Keymap::load_from(&path);
        assert_eq!(notice, None);
        assert_eq!(read, written);
        assert_eq!(read.lookup("w", true, false, false), Some(ActionId::Export));
        assert_eq!(read.lookup("e", false, false, false), None);
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
        std::fs::write(
            &path,
            "edith-keys 2\nplay ctrl+shift+shift+x\nexport ctrl+w\n",
        )
        .unwrap();
        let (kept, notice) = Keymap::load_from(&path);
        assert_eq!(kept.lookup("w", true, false, false), Some(ActionId::Export));
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
    fn shift_and_alt_round_trip_in_any_order() {
        let c = Chord::parse("shift+ctrl+i", 1).unwrap();
        assert_eq!(
            c,
            Chord {
                key: "i".into(),
                ctrl: true,
                shift: true,
                alt: false,
            }
        );
        assert_eq!(c.text(), "ctrl+shift+i");
        assert_eq!(c.pretty(), "Ctrl+Shift+I");
        assert_eq!(c.compact(), "Ctrl+Shift+I");
        let a = Chord::parse("alt+1", 1).unwrap();
        assert_eq!(a.pretty(), "Alt+1");
        assert_eq!(a.compact(), "Alt+1");
        // Each modifier once, a lowercase key last, and nothing else.
        for bad in [
            "alt+ctrl+alt+1",
            "ctrl+ctrl+x",
            "shift+",
            "ctrl+shift+",
            "Ctrl+i",
            "shift+I",
        ] {
            assert!(Chord::parse(bad, 1).is_err(), "{bad} should be refused");
        }
        // Read from a file and written back in the one spelling; the same key
        // with and without shift is two strokes.
        let k = whole("edith-keys 2\nimport-subtitles shift+ctrl+i\nadd-files ctrl+i\n");
        assert_eq!(
            k.lookup("i", true, true, false),
            Some(ActionId::ImportSubtitles)
        );
        assert_eq!(k.lookup("i", true, false, false), Some(ActionId::AddFiles));
        assert_eq!(k.lookup("i", false, false, false), None);
        assert_eq!(
            emit(&k),
            "edith-keys 2\nimport-subtitles ctrl+shift+i\nadd-files ctrl+i\n"
        );
        assert!(c.bindable());
        assert!(
            !Chord {
                key: "+".into(),
                ctrl: false,
                shift: true,
                alt: false,
            }
            .bindable()
        );
    }

    /// DESIGN.md §10.2's table, row for row: every action's strokes, in order,
    /// as keymap.json's "proposed" column has them, with Delete's Backspace and
    /// ShowActions alone on Ctrl+K as the brief set them.
    #[test]
    fn defaults_are_the_proposed_keys() {
        const PROPOSED: &[(&str, &[&str])] = &[
            ("Export", &["e"]),
            ("Save", &["ctrl+s"]),
            ("ReloadProject", &[]),
            ("NewProject", &[]),
            ("SaveVersion", &[]),
            ("SaveTemplate", &[]),
            ("ExportCaptions", &[]),
            ("Versions", &[]),
            ("CollectMedia", &[]),
            ("ProjectNotes", &[]),
            ("ExportTimelineOtio", &[]),
            ("ExportTimelineFcpxml", &[]),
            ("ExportTimelineEdl", &[]),
            ("ImportTimelineOtio", &[]),
            ("Normalize", &["alt+n"]),
            ("Captions", &["alt+c"]),
            ("Fillers", &["alt+u"]),
            ("CutSelectedWords", &[]),
            ("Voiceover", &["alt+v"]),
            ("RecordVoiceover", &[]),
            ("AddFiles", &["ctrl+i"]),
            ("OpenProject", &["ctrl+o"]),
            ("PasteFilePath", &[]),
            ("CancelExport", &["ctrl+escape"]),
            ("ShowActions", &["ctrl+k"]),
            ("ShowKeys", &["?", "f1"]),
            ("Screenshot", &["f12"]),
            ("Play", &["space"]),
            ("Loop", &["r"]),
            ("PlayAround", &["shift+k"]),
            ("StepBack", &["left"]),
            ("StepForward", &["right"]),
            ("JumpBack", &["ctrl+left"]),
            ("JumpForward", &["ctrl+right"]),
            ("GoStart", &["home"]),
            ("GoEnd", &["end"]),
            ("GoToTime", &["ctrl+j"]),
            ("PrevSyncPoint", &["ctrl+["]),
            ("NextSyncPoint", &["ctrl+]"]),
            ("WalkCutNext", &["down", "."]),
            ("WalkCutPrev", &["up", ","]),
            ("WalkCutNext10", &[">"]),
            ("WalkCutPrev10", &["<"]),
            ("NextMarker", &["shift+m"]),
            ("PrevMarker", &["ctrl+shift+m"]),
            ("SetIn", &["i"]),
            ("SetOut", &["o"]),
            ("ClearRange", &["ctrl+u"]),
            ("ShuttleBack", &["j"]),
            ("ShuttleStop", &["k"]),
            ("ShuttleForward", &["l"]),
            ("InsertSource", &["f9"]),
            ("OverwriteSource", &["f10"]),
            ("Cut", &["s"]),
            ("Regroup", &["g"]),
            ("Detach", &["d"]),
            ("Group", &["ctrl+g"]),
            ("Undo", &["ctrl+z"]),
            ("Redo", &["ctrl+y", "ctrl+shift+z"]),
            ("History", &[]),
            ("AddVideoLane", &[]),
            ("AddAudioLane", &[]),
            ("RemoveVideoLane", &[]),
            ("RemoveAudioLane", &[]),
            ("AddSubtitleLane", &[]),
            ("RemoveSubtitleLane", &[]),
            ("ImportSubtitles", &["ctrl+shift+i"]),
            ("ToggleSnap", &["n"]),
            ("AddMarker", &["alt+m"]),
            ("Markers", &["alt+8"]),
            ("Copy", &["ctrl+c"]),
            ("Paste", &["ctrl+v"]),
            ("Select", &["tab"]),
            ("SelectNext", &["}"]),
            ("SelectPrev", &["{"]),
            ("SelectAll", &["ctrl+a"]),
            ("Deselect", &["escape"]),
            ("Delete", &["delete", "backspace"]),
            ("Lift", &[";"]),
            ("Color", &["alt+2"]),
            ("Transform", &["alt+3"]),
            ("Effects", &["alt+e"]),
            ("KeyPlacement", &[]),
            ("KeyOpacity", &[]),
            ("KeyColor", &[]),
            ("KeyEasing", &[]),
            ("Fit", &["alt+7"]),
            ("Reverse", &["ctrl+alt+r"]),
            ("ClipEnabled", &["shift+e"]),
            ("ClipLabel", &[]),
            ("NewSolid", &[]),
            ("NewGradient", &[]),
            ("NewCountdown", &[]),
            ("NewBars", &[]),
            ("NewTone", &[]),
            ("NewBarsAndTone", &[]),
            ("NewAdjustment", &[]),
            ("NewRectangle", &[]),
            ("NewEllipse", &[]),
            ("NewLine", &[]),
            ("NewArrow", &[]),
            ("NewTitle", &["alt+t"]),
            ("NewBin", &[]),
            ("NewBinInside", &[]),
            ("RenameBin", &[]),
            ("RemoveBin", &[]),
            ("ToggleFavourite", &[]),
            ("MoveToBin", &[]),
            ("ShowFavourites", &[]),
            ("ShowAllMedia", &[]),
            ("Speed", &["alt+1"]),
            ("SpeedRampUp", &[]),
            ("SpeedRampDown", &[]),
            ("SpeedRampSlowFastSlow", &[]),
            ("SpeedRampFastSlowFast", &[]),
            ("SpeedRampKey", &[]),
            ("SpeedRampClear", &[]),
            ("Silence", &["alt+5"]),
            ("MarkBeats", &["alt+b"]),
            ("Visualizer", &["alt+6"]),
            ("Reframe", &[]),
            ("TrackMask", &[]),
            ("NestClips", &[]),
            ("BreakApart", &[]),
            ("OpenCompound", &[]),
            ("CloseCompound", &[]),
            ("NewSequence", &[]),
            ("DuplicateSequence", &[]),
            ("RenameSequence", &[]),
            ("DeleteSequence", &[]),
            ("CreateMulticam", &[]),
            ("FlattenMulticam", &[]),
            ("ToggleAngleMonitor", &["alt+a"]),
            ("ToggleOutputWindow", &[]),
            ("OutputDisplay", &[]),
            ("Angle1", &["1"]),
            ("Angle2", &["2"]),
            ("Angle3", &["3"]),
            ("Angle4", &["4"]),
            ("Angle5", &["5"]),
            ("Angle6", &["6"]),
            ("Angle7", &["7"]),
            ("Angle8", &["8"]),
            ("Angle9", &["9"]),
            ("RenameLane", &[]),
            ("Crossfade", &["ctrl+f"]),
            ("TrimIn", &["["]),
            ("TrimOut", &["]"]),
            ("LoopTrim", &["/"]),
            ("RippleTrimInToPlayhead", &["q"]),
            ("RippleTrimOutToPlayhead", &["w"]),
            ("TrimInToPlayhead", &["alt+q"]),
            ("TrimOutToPlayhead", &["alt+w"]),
            ("RollBack", &["alt+["]),
            ("RollForward", &["alt+]"]),
            ("SlipBack", &["alt+,"]),
            ("SlipForward", &["alt+."]),
            ("SlideBack", &["alt+left"]),
            ("SlideForward", &["alt+right"]),
            ("Dissolve", &["ctrl+d"]),
            ("TransitionDissolve", &[]),
            ("TransitionDipBlack", &[]),
            ("TransitionDipWhite", &[]),
            ("TransitionWipeLeft", &[]),
            ("TransitionWipeRight", &[]),
            ("TransitionWipeUp", &[]),
            ("TransitionWipeDown", &[]),
            ("TransitionSlideLeft", &[]),
            ("TransitionSlideRight", &[]),
            ("TransitionSlideUp", &[]),
            ("TransitionSlideDown", &[]),
            ("TransitionPushLeft", &[]),
            ("TransitionPushRight", &[]),
            ("TransitionPushUp", &[]),
            ("TransitionPushDown", &[]),
            ("TransitionZoom", &[]),
            ("ToggleMute", &["m"]),
            ("VolumeUp", &["pageup"]),
            ("VolumeDown", &["pagedown"]),
            ("Equalizer", &["alt+4"]),
            ("Mix", &["f", "alt+x"]),
            ("CleanVoice", &["alt+9"]),
            ("VoiceDynamics", &["alt+0"]),
            ("Resolution", &["ctrl+r"]),
            ("ZoomIn", &["=", "ctrl+="]),
            ("ZoomOut", &["-", "ctrl+-"]),
            ("ZoomFit", &["\\", "ctrl+0"]),
            ("ZoomToSelection", &["shift+z"]),
            ("TrackSmall", &[]),
            ("TrackMedium", &[]),
            ("TrackLarge", &[]),
            ("TracksSmaller", &["alt+-"]),
            ("TracksLarger", &["alt+="]),
            ("ViewZoom", &[]),
            ("PlaybackQuality", &[]),
            ("GuideStyle", &[]),
            ("GuideMask", &[]),
            ("ToggleSubtitles", &["t"]),
            ("ToggleWaveforms", &[]),
            ("ToggleScrubAudio", &[]),
            ("CompareOff", &[]),
            ("CompareSplit", &[]),
            ("CompareSide", &[]),
            ("AutoColour", &[]),
            ("SaveLook", &[]),
            ("ToggleProxies", &["ctrl+p"]),
            ("ToggleAutoProxies", &[]),
            ("Theme", &[]),
            ("AccentColour", &[]),
            ("Fullscreen", &["f11"]),
            ("SubtitleStyle", &["y"]),
            ("Settings", &["ctrl+,"]),
            ("CloseGap", &["shift+delete"]),
            ("Freeze", &["shift+r"]),
            ("PasteAttributes", &["ctrl+alt+v"]),
            ("PasteColor", &[]),
            ("PasteEffects", &[]),
            ("PasteTransform", &[]),
            ("PasteSpeed", &[]),
            ("PasteAudio", &[]),
            ("MatchFrame", &["shift+f"]),
            ("ReplaceEdit", &["f8"]),
            ("SelectForward", &["alt+y"]),
            ("SelectForwardAll", &["shift+a"]),
            ("ExtendAudio", &["shift+alt+l"]),
            ("ExtendVideo", &["shift+alt+j"]),
            ("NudgeBack", &["ctrl+alt+left"]),
            ("NudgeForward", &["ctrl+alt+right"]),
            ("NudgeBack10", &["ctrl+shift+alt+left"]),
            ("NudgeForward10", &["ctrl+shift+alt+right"]),
            ("FocusPanels", &["f6"]),
            ("About", &[]),
            ("MarkerHere", &[]),
            ("RenameMarker", &[]),
            ("DeleteMarker", &[]),
            ("PlayheadHere", &[]),
            ("RenderInOut", &["enter"]),
            ("RenderAll", &[]),
            ("CancelRender", &[]),
            ("DeleteRenderFiles", &[]),
        ];
        let k = Keymap::defaults();
        assert_eq!(PROPOSED.len(), ActionId::ALL.len());
        for (name, want) in PROPOSED {
            let action = ActionId::ALL
                .into_iter()
                .find(|a| format!("{a:?}") == *name)
                .unwrap_or_else(|| panic!("{name} is not an action"));
            let have: Vec<String> = k
                .entries()
                .iter()
                .filter(|b| b.action == action)
                .map(|b| b.chord.text())
                .collect();
            let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            assert_eq!(have, want, "{name}");
        }
    }

    /// A file the build before shift and alt saved, with that build's defaults
    /// in it, comes back as today's defaults for every action that moved.
    #[test]
    fn a_v1_file_with_the_old_defaults_migrates() {
        let mut text = String::from("edith-keys 1\n");
        for (action, old) in V1_DEFAULTS {
            for chord in old.iter() {
                text.push_str(&format!("{} {chord}\n", action.name()));
            }
        }
        let migrated = whole(&text);
        let defaults = Keymap::defaults();
        for (action, _) in V1_DEFAULTS {
            let of = |k: &Keymap| {
                k.entries()
                    .iter()
                    .filter(|b| b.action == *action)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(of(&migrated), of(&defaults), "{action:?}");
        }
        assert_eq!(migrated.display(ActionId::Speed), "Alt+1");
        assert_eq!(migrated.display(ActionId::Delete), "Delete or Backspace");
        assert_eq!(migrated.display(ActionId::Undo), "Ctrl+Z");
        assert_eq!(migrated.display(ActionId::ShuttleBack), "J");
        // Nothing the table does not name was in the file, so nothing else is bound.
        assert_eq!(migrated.display(ActionId::Cut), "unbound");
    }

    /// A rebind in the old format is the person's: it is kept, and a default
    /// whose stroke it takes is left unbound rather than taking the stroke back.
    #[test]
    fn a_v1_rebind_is_kept_and_a_default_it_holds_is_not_taken() {
        let (k, dropped) = parse("edith-keys 1\nspeed x\nlift j\n").unwrap();
        assert!(dropped.is_empty());
        assert_eq!(k.display(ActionId::Speed), "X");
        assert_eq!(k.display(ActionId::Lift), "J");
        // The shuttle's default is `j`, which the lift line holds.
        assert_eq!(k.display(ActionId::ShuttleBack), "unbound");
        // A default the file does not touch comes from the defaults.
        assert_eq!(k.display(ActionId::ShuttleStop), "K");
    }

    /// Today's files are never migrated, so an action unbound on purpose stays
    /// unbound after a save and a load.
    #[test]
    fn a_v2_file_keeps_what_its_owner_unbound() {
        let text: String = emit(&Keymap::defaults())
            .lines()
            .filter(|l| !l.starts_with("shuttle-back "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let k = whole(&text);
        assert_eq!(k.display(ActionId::ShuttleBack), "unbound");
        assert_eq!(k.display(ActionId::ShuttleStop), "K");
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

    fn index_of(keymap: &Keymap, action: ActionId) -> usize {
        keymap
            .entries()
            .iter()
            .position(|b| b.action == action)
            .expect("a shipped action has a binding")
    }

    #[test]
    fn a_swap_hands_the_holders_stroke_over_and_never_doubles_a_chord() {
        let mut keymap = Keymap::defaults();
        let save = index_of(&keymap, ActionId::Save);
        // Save is ctrl+s and AddFiles is ctrl+i: taking ctrl+i for Save must
        // leave AddFiles with the stroke Save gave up.
        assert_eq!(
            keymap.holder(&chord("i", true), save),
            Some(index_of(&keymap, ActionId::AddFiles))
        );
        keymap.swap_in(save, chord("i", true));
        assert_eq!(keymap.lookup("i", true, false, false), Some(ActionId::Save));
        assert_eq!(
            keymap.lookup("s", true, false, false),
            Some(ActionId::AddFiles)
        );
        let chords: Vec<&Chord> = keymap.entries().iter().map(|b| &b.chord).collect();
        for (at, c) in chords.iter().enumerate() {
            assert!(!chords[at + 1..].contains(c), "{c:?} is on two rows");
        }
    }

    #[test]
    fn a_reset_restores_the_shipped_strokes_and_names_whoever_lost_one() {
        let shipped = Keymap::defaults();
        let mut keymap = Keymap::defaults();
        assert!(keymap.is_default(ActionId::Save, &shipped));
        let save = index_of(&keymap, ActionId::Save);
        keymap.swap_in(save, chord("i", true));
        assert!(!keymap.is_default(ActionId::Save, &shipped));
        assert!(!keymap.is_default(ActionId::AddFiles, &shipped));
        let displaced = keymap.reset_action(ActionId::Save);
        assert!(keymap.is_default(ActionId::Save, &shipped));
        // AddFiles held Save's shipped ctrl+s after the swap, so the reset takes
        // it back and AddFiles is left with nothing -- and says so.
        assert_eq!(displaced, vec![ActionId::AddFiles]);
        assert_eq!(keymap.lookup("i", true, false, false), None);
        assert_eq!(keymap.lookup("s", true, false, false), Some(ActionId::Save));
    }

    #[test]
    fn a_reset_keeps_the_action_in_its_shipped_place() {
        let mut keymap = Keymap::defaults();
        let before: Vec<ActionId> = keymap.entries().iter().map(|b| b.action).collect();
        let save = index_of(&keymap, ActionId::Save);
        keymap.swap_in(save, chord("q", true));
        keymap.reset_action(ActionId::Save);
        let after: Vec<ActionId> = keymap.entries().iter().map(|b| b.action).collect();
        assert_eq!(before, after);
    }
}

#[cfg(test)]
mod nocycle_names {
    use super::*;

    /// A keys file written while the action still stepped calls it `cycle-key-ease`.
    #[test]
    fn an_old_keys_file_still_finds_key_easing() {
        assert_eq!(ActionId::from_name("cycle-key-ease"), Some(ActionId::KeyEasing));
        assert_eq!(ActionId::from_name("key-easing"), Some(ActionId::KeyEasing));
    }
}
